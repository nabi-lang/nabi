//! The walk over blocks, statements and expressions.

#[allow(clippy::wildcard_imports)]
use crate::handoff::*;

impl<'h> Walk<'_, 'h> {
    pub(crate) fn block(&mut self, b: &'h HirBlock) {
        self.block_to(b, None);
    }

    /// Block `b`; the tag of its value goes to `k`.
    pub(crate) fn block_to(&mut self, b: &'h HirBlock, k: Option<&Place>) {
        self.scopes.push(std::ptr::from_ref(b) as usize);
        if b.label.is_some() {
            self.frames.push(Frame {
                label: b.label.as_deref(),
                is_loop: false,
                brk: Cur::default(),
                cont: Cur::default(),
                dest: k.cloned(),
            });
        }
        for s in &b.stmts {
            self.stmt(s);
        }
        if let Some(t) = &b.trailing {
            self.value(t, k);
        }
        if b.label.is_some() {
            let f = self.frames.pop().expect("block frame");
            self.cur = join(std::mem::take(&mut self.cur), f.brk);
        }
        self.scopes.pop();
    }

    pub(crate) fn value(&mut self, e: &'h HirExpr, k: Option<&Place>) {
        match k {
            Some(k) => self.expr_to(e, k),
            None => self.expr(e),
        }
    }

    /// The place holding the tag of `e`'s value while it is tested or
    /// stored.
    pub(crate) fn key(&mut self, e: &HirExpr) -> Place {
        let n = self.keys.len();
        #[allow(clippy::cast_possible_truncation)]
        let l = *self
            .keys
            .entry(addr(e))
            .or_insert(LocalId(TAG_KEY + n as u32));
        (l, Vec::new())
    }

    /// The tags at `from` move to `to`.
    pub(crate) fn retag(&mut self, from: &Place, to: &Place) {
        let pinned = self.pinned.contains(&to.0);
        for (t, _) in &mut self.cur.0 {
            let moved: Vec<(Place, u32)> = t
                .iter()
                .filter(|(q, _)| within(q, from))
                .map(|(q, v)| (extend(to, &q.1[from.1.len()..]), *v))
                .collect();
            t.retain(|q, _| !within(q, from));
            if !pinned {
                t.extend(moved);
            }
        }
    }

    /// `e`; the tag of its value goes to `k` on the paths that know it.
    pub(crate) fn expr_to(&mut self, e: &'h HirExpr, k: &Place) {
        if self.cur.is_empty() {
            return;
        }
        for (t, _) in &mut self.cur.0 {
            t.retain(|q, _| !within(q, k));
        }
        match &e.kind {
            HirExprKind::Block(b) => {
                let returning = std::mem::take(&mut self.returning);
                self.returning = returning && b.stmts.is_empty();
                self.block_to(b, Some(k));
            }
            HirExprKind::If {
                cond,
                then_block,
                else_branch,
            } => {
                self.returning = false;
                self.branch(cond, then_block, else_branch.as_deref(), Some(k));
            }
            HirExprKind::Match { scrutinee, arms } => {
                self.returning = false;
                self.matched(scrutinee, arms, Some(k));
            }
            HirExprKind::Loop { label, body, .. } => {
                self.returning = false;
                self.looped(label.as_deref(), None, body, Some(k));
            }
            HirExprKind::Call { .. }
            | HirExprKind::CallIndirect { .. }
            | HirExprKind::CallClosure { .. }
            | HirExprKind::CoroNew { .. } => self.expr_into(e, e, k),
            HirExprKind::PrimUnaryOp {
                op: PrimOp::LogicalNot,
                operand,
            } => {
                self.expr_to(operand, k);
                for (t, _) in &mut self.cur.0 {
                    if let Some(v) = t.get_mut(k) {
                        *v ^= 1;
                    }
                }
            }
            // A future's tag is its body's.
            HirExprKind::StructInit { struct_id, fields }
                if self.c.futures.contains(struct_id)
                    && matches!(&fields[..], [x] if matches!(x.kind, HirExprKind::CoroNew { .. })) =>
            {
                self.expr_into(e, &fields[0], k);
            }
            _ => {
                self.expr(e);
                let copied = tag_place(e).filter(|_| {
                    !matches!(&e.kind, HirExprKind::TupleField { target, .. }
                        if matches!(target.ty, HirTy::Throws { .. }))
                });
                for (t, _) in &mut self.cur.0 {
                    if let Some(q) = &copied {
                        let got: Vec<(Place, u32)> = t
                            .iter()
                            .filter(|(p, _)| within(p, q))
                            .map(|(p, v)| (extend(k, &p.1[q.1.len()..]), *v))
                            .collect();
                        t.extend(got);
                    } else if let Some(v) = tag_of(e, t) {
                        t.insert(k.clone(), v);
                    }
                }
            }
        }
    }

    /// `e`, whose call `call` gives the tag of its value to `k`.
    pub(crate) fn expr_into(&mut self, e: &'h HirExpr, call: &HirExpr, k: &Place) {
        self.dests.push((addr(call), k.clone()));
        self.expr(e);
        self.dests.pop();
    }

    /// Walk `cond`: the paths on which it may be true, and false.
    pub(crate) fn test(&mut self, cond: &'h HirExpr) -> (Cur, Cur) {
        let k = self.key(cond);
        self.expr_to(cond, &k);
        let (mut yes, mut no) = (Cur::default(), Cur::default());
        for (mut t, s) in std::mem::take(&mut self.cur.0) {
            match t.remove(&k) {
                Some(1) => yes.0.push((t, s)),
                Some(0) => no.0.push((t, s)),
                _ => {
                    no.0.push((t.clone(), s.clone()));
                    yes.0.push((t, s));
                }
            }
        }
        yes.norm();
        no.norm();
        (yes, no)
    }

    pub(crate) fn branch(
        &mut self,
        cond: &'h HirExpr,
        then_block: &'h HirBlock,
        else_branch: Option<&'h HirExpr>,
        k: Option<&Place>,
    ) {
        let (yes, no) = self.test(cond);
        self.cur = yes;
        self.block_to(then_block, k);
        let then = std::mem::replace(&mut self.cur, no);
        if let Some(x) = else_branch {
            self.value(x, k);
        }
        self.cur = join(then, std::mem::take(&mut self.cur));
    }

    /// Matching a variable reads its tag (in the value itself) and binds
    /// its parts: the uses are the bindings'. An arm sees the paths whose
    /// tag it may match and no arm before it takes.
    pub(crate) fn matched(
        &mut self,
        scrutinee: &'h HirExpr,
        arms: &'h [HirMatchArm],
        k: Option<&Place>,
    ) {
        let key = if place(scrutinee).is_none() {
            let ks = self.key(scrutinee);
            self.expr_to(scrutinee, &ks);
            Some((ks, true))
        } else {
            tag_place(scrutinee).map(|p| (p, false))
        };
        // Every arm's bindings are new each time the `match` runs: what an
        // earlier pass left of them (one handed over) is not theirs.
        for a in arms {
            a.pat.each_binding(&mut |l| self.assign(&(l, Vec::new())));
        }
        let mut base = std::mem::take(&mut self.cur);
        let ty = &scrutinee.ty;
        let mut out = Cur::default();
        for (i, a) in arms.iter().enumerate() {
            let mut cur = Cur::default();
            for (t, s) in &base.0 {
                if let Some(&v) = key.as_ref().and_then(|(p, _)| t.get(p)) {
                    let taken = arms[..i]
                        .iter()
                        .any(|b| b.guard.is_none() && catches(&b.pat, v, ty));
                    if taken || !may_match(&a.pat, v, ty) {
                        continue;
                    }
                }
                cur.0.push((t.clone(), s.clone()));
            }
            if let Some((ks, true)) = &key {
                cur.untag(ks);
            }
            self.cur = cur;
            let mut binds = Vec::new();
            binding_paths(&a.pat, Some(&[]), &mut binds);
            for (l, path) in binds {
                let at = (l, Vec::new());
                match path {
                    Some(path) => {
                        self.assign(&at);
                        let mut fl = Vec::new();
                        self.flows(scrutinee, &mut fl);
                        self.store_flows(&at, &project(fl, &path), false);
                        if let Some((g, span)) = self.got_from(scrutinee) {
                            self.tie(g, at, span, Tie::Taken);
                        }
                    }
                    None => self.store(&at, scrutinee, true),
                }
            }
            if let Some(g) = &a.guard {
                self.expr(g);
            }
            self.value(&a.body, k);
            out = join(out, std::mem::take(&mut self.cur));
        }
        if let Some((ks, true)) = &key {
            base.untag(ks);
        }
        self.cur = if arms.is_empty() { base } else { out };
    }

    /// The paths reach an exit; `k` holds the returned value's tag.
    pub(crate) fn exit_with(&mut self, k: Option<&Place>) {
        for (t, s) in std::mem::take(&mut self.cur.0) {
            let tag = k.and_then(|k| t.get(k)).copied().unwrap_or(ANY);
            let s = match self.exits.remove(&tag) {
                Some(e) => join_st(e, s),
                None => s,
            };
            self.exits.insert(tag, s);
        }
    }

    pub(crate) fn target(&mut self, label: Option<&str>, cont: bool) -> Option<&mut Frame<'h>> {
        self.frames.iter_mut().rev().find(|f| match label {
            Some(l) => f.label == Some(l),
            None => f.is_loop,
        } && (!cont || f.is_loop))
    }

    pub(crate) fn stmt(&mut self, s: &'h HirStmt) {
        match s {
            HirStmt::Let { local: l, init: e } | HirStmt::Assign { local: l, value: e } => {
                self.assign_into((*l, Vec::new()), e);
            }
            HirStmt::ArrayAssign {
                array_local,
                path,
                index,
                value,
                ..
            } => {
                self.expr(index);
                self.expr(value);
                let arr = (*array_local, path.clone());
                if let Some(ty) = self.local_ty(*array_local) {
                    self.use_place(&arr, &ty, None, index.span);
                }
                self.hold(&arr, value);
            }
            HirStmt::StaticAssign { value: e, .. } | HirStmt::Expr(e) => self.expr(e),
            HirStmt::Return(e) => {
                if let Some(e) = e {
                    self.returning = place(e).is_some();
                    self.thrown = thrown(e).and_then(place).map(|p| p.0);
                    self.expr_to(e, &RET_TAG);
                    self.returning = false;
                    self.thrown = None;
                    self.store(&(RET, Vec::new()), e, false);
                    match thrown(e) {
                        Some(err) => self.throw_escapes(err),
                        None => self.escapes_guard(e, Exit::Return),
                    }
                }
                self.note_renews();
                self.exit_with(Some(&RET_TAG));
            }
            HirStmt::Panic(e) => {
                self.expr(e);
                self.cur = Cur::default();
            }
            HirStmt::Break { label, value } => {
                if let Some(e) = value {
                    let k = self
                        .target(label.as_deref(), false)
                        .and_then(|f| f.dest.clone());
                    self.value(e, k.as_ref());
                }
                let st = std::mem::take(&mut self.cur);
                if let Some(f) = self.target(label.as_deref(), false) {
                    f.brk = join(std::mem::take(&mut f.brk), st);
                }
            }
            HirStmt::Continue { label } => {
                let st = std::mem::take(&mut self.cur);
                if let Some(f) = self.target(label.as_deref(), true) {
                    f.cont = join(std::mem::take(&mut f.cont), st);
                }
            }
        }
    }

    /// The places whose objects hold the buffer `e` (`self.h[0].buf`
    /// is held by `self.h`).
    pub(crate) fn holders(&self, e: &HirExpr, out: &mut Vec<Place>) {
        if let Some(p) = place(e) {
            out.push(p);
            return;
        }
        match &e.kind {
            HirExprKind::BufGet { buf: x, .. }
            | HirExprKind::ArrayIndex { array: x, .. }
            | HirExprKind::FieldRead { target: x, .. }
            | HirExprKind::TupleField { target: x, .. }
            | HirExprKind::Cast { operand: x, .. } => self.holders(x, out),
            _ => {
                let mut fl = Vec::new();
                self.flows(e, &mut fl);
                out.extend(fl.into_iter().map(|(_, q)| q));
            }
        }
    }

    /// The buffer at `c` now holds `v` among its elements: they share it
    /// both ways. Storing what it may hold already makes it hold an
    /// object twice.
    pub(crate) fn hold(&mut self, c: &Place, v: &HirExpr) {
        let mut fl = Vec::new();
        self.flows(v, &mut fl);
        let at = extend(c, &[ELEM]);
        for s in self.cur.states() {
            hold_st(s, &at, &fl, false);
        }
    }

    /// `while` (with `cond`) or `loop`: the body runs until the state at
    /// its head stops growing.
    pub(crate) fn looped(
        &mut self,
        label: Option<&'h str>,
        cond: Option<&'h HirExpr>,
        body: &'h HirBlock,
        k: Option<&Place>,
    ) {
        let mut head = std::mem::take(&mut self.cur);
        for pass in 0.. {
            self.frames.push(Frame {
                label,
                is_loop: true,
                brk: Cur::default(),
                cont: Cur::default(),
                dest: k.cloned(),
            });
            self.cur.clone_from(&head);
            let mut exit = Cur::default();
            if let Some(c) = cond {
                let (yes, no) = self.test(c);
                self.cur = yes;
                exit = no;
            }
            self.block(body);
            let f = self.frames.pop().expect("loop frame");
            let cur = std::mem::take(&mut self.cur);
            let mut next = join(head.clone(), join(cur, f.cont));
            if pass >= MAX_SPLIT_PASSES {
                next.flatten();
            }
            if next == head {
                self.cur = join(f.brk, exit);
                return;
            }
            head = next;
        }
    }

    pub(crate) fn expr(&mut self, e: &'h HirExpr) {
        if self.cur.is_empty() {
            return;
        }
        if let Some(p) = place(e).filter(|_| {
            matches!(
                e.kind,
                HirExprKind::Local(_)
                    | HirExprKind::FieldRead { .. }
                    | HirExprKind::TupleField { .. }
            )
        }) {
            return self.use_place(&p, &e.ty, Some(e), e.span);
        }
        // The release the lowering adds for an affine value (`if #own
        // { drop }`) runs only while it still owns the value.
        if let HirExprKind::If { cond, .. } = &e.kind {
            if let HirExprKind::Local(l) = cond.kind {
                if self.local_name(l).is_some_and(|n| n.starts_with("#own")) {
                    return;
                }
            }
        }
        let returning = std::mem::take(&mut self.returning);
        match &e.kind {
            HirExprKind::Block(b) => {
                self.returning = returning && b.stmts.is_empty();
                self.block(b);
            }
            HirExprKind::If {
                cond,
                then_block,
                else_branch,
            } => self.branch(cond, then_block, else_branch.as_deref(), None),
            HirExprKind::While { label, cond, body } => {
                self.looped(label.as_deref(), Some(cond), body, None);
            }
            HirExprKind::Loop { label, body, .. } => {
                self.looped(label.as_deref(), None, body, None);
            }
            HirExprKind::Match { scrutinee, arms } => self.matched(scrutinee, arms, None),
            // A step handed to the resumer; the last one ends the body.
            HirExprKind::Suspend { value, is_final } => {
                self.expr_to(value, &RET_TAG);
                // A step leaves like a return; the resumer may keep it past
                // the lock (`yield g.get()` inside `using`).
                if !*is_final {
                    let kept = self.cur.clone();
                    self.store(&(RET, Vec::new()), value, false);
                    self.escapes_guard(value, Exit::Yield);
                    self.cur = kept;
                }
                if *is_final {
                    self.exit_with(Some(&RET_TAG));
                } else {
                    let mut mixed = false;
                    for (t, _) in &mut self.cur.0 {
                        mixed |= t.remove(&RET_TAG).is_none_or(|v| v == ERR);
                    }
                    self.mixed |= mixed;
                    self.cur.norm();
                }
            }
            _ => {
                let is_call = matches!(
                    e.kind,
                    HirExprKind::Call { .. }
                        | HirExprKind::CallIndirect { .. }
                        | HirExprKind::CallClosure { .. }
                );
                if is_call {
                    self.calls.push(e);
                }
                let mut kids = Vec::new();
                each_child(e, &mut |c| kids.push(c));
                for c in kids {
                    match c {
                        Child::E(x) => self.expr(x),
                        Child::B(b) => self.block(b),
                    }
                }
                if is_call {
                    self.calls.pop();
                }
                self.after(e);
            }
        }
    }

    /// The effect of `e` once its operands ran.
    pub(crate) fn after(&mut self, e: &'h HirExpr) {
        if let HirExprKind::Call { args, .. }
        | HirExprKind::CallIndirect { args, .. }
        | HirExprKind::CallClosure { args, .. } = &e.kind
        {
            self.note_runner(e);
            self.send_fn_args(args, e.span);
        }
        match &e.kind {
            HirExprKind::Call { callee, args } => {
                self.stream_released(*callee, args);
                self.args_moved(*callee, args);
                if let Some(op) = self.c.ops.get(callee).copied() {
                    return self.op(op, args, e.span);
                }
                let args: Vec<&HirExpr> = args.iter().collect();
                if self.c.passes.contains(callee) {
                    let mut fl = Vec::new();
                    if let Some(a) = args.first() {
                        self.flows(a, &mut fl);
                    }
                    self.results.insert(addr(e), fl);
                    return;
                }
                let takes = self.c.takes.contains(callee);
                let literal = self.c.literals.contains(callee);
                // Only a direct call says the callee itself hands something
                // over (a call through a function value may reach any
                // function of its type).
                if let Some(s) = self.c.summary(*callee).filter(|s| s.sends) {
                    self.sends = true;
                    let name = self.c.fns.get(callee).map(|f| shown_fn(self.c, f));
                    self.aliased_args(&s, &args, e, name.as_deref());
                }
                if self.c.copies.contains(callee) {
                    self.copy_dup(&args);
                }
                if let Some(s) = self.callee(*callee) {
                    self.kept_views(&s, &args, e, None);
                    let s = if literal {
                        Rc::new(Summary {
                            dups: BTreeSet::new(),
                            ..(*s).clone()
                        })
                    } else {
                        s
                    };
                    if !s.gives.is_empty() {
                        if let Some(f) = self.c.fns.get(callee) {
                            let n = shown_fn(self.c, f);
                            self.c
                                .callees
                                .borrow_mut()
                                .insert((e.span.file, e.span.start), n);
                        }
                    }
                    self.apply(&s, &args, e, true, takes);
                    if literal {
                        self.literal_dup(&s, e);
                    }
                }
                if self.c.polls.contains(callee) {
                    self.polled(e, args.first().copied());
                }
                if self.c.par.contains(callee) {
                    self.par_result(*callee, &args, e);
                }
                if self.c.clears.contains(callee) {
                    if let Some(p) = args.first().and_then(|a| place(a)) {
                        self.cleared(&p);
                    }
                }
            }
            HirExprKind::CallIndirect { callee, args, .. } => {
                let mut all = vec![&**callee];
                all.extend(args.iter());
                let s = match &callee.kind {
                    HirExprKind::FnRef(id) => {
                        self.callee(*id).map(|s| Rc::new(s.remap(&|i| Some(i + 1))))
                    }
                    _ => self.indirect(args, &e.ty),
                };
                if let Some(s) = s {
                    self.kept_views(&s, &all, e, Some(callee));
                    self.apply(&s, &all, e, true, false);
                }
            }
            HirExprKind::CallClosure { closure, args, .. } => {
                let mut all = vec![&**closure];
                all.extend(args.iter());
                let s = match &closure.kind {
                    HirExprKind::ClosureValue { fn_id, .. } => self.callee(*fn_id),
                    _ => self.indirect(args, &e.ty),
                };
                if let Some(s) = s {
                    self.kept_views(&s, &all, e, Some(closure));
                    self.apply(&s, &all, e, true, false);
                }
                self.closure_future(closure, e);
            }
            HirExprKind::CoroNew { body_fn, args } => {
                let args: Vec<&HirExpr> = args.iter().collect();
                if let Some(s) = self.callee(*body_fn) {
                    self.apply(&s, &args, e, false, false);
                }
                // The frame holds the arguments until it runs.
                let mut v = Vec::new();
                for a in &args {
                    let mut fl = Vec::new();
                    self.flows(a, &mut fl);
                    v.extend(fl.into_iter().map(|(_, q)| (Fk::Holds, q)));
                }
                self.results.insert(addr(e), v);
            }
            HirExprKind::BufSet { buf, value, .. } => {
                let mut cs = Vec::new();
                self.holders(buf, &mut cs);
                for c in cs {
                    self.hold(&c, value);
                }
            }
            // A capture moves into the closure.
            HirExprKind::ClosureValue { env, .. } => match &env.kind {
                HirExprKind::Tuple { fields } => {
                    for x in fields {
                        self.stream_moved(x, e.span);
                    }
                }
                _ => self.stream_moved(env, e.span),
            },
            _ => {}
        }
    }
}
