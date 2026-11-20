//! The walk over statements, expressions, calls and matches.

#[allow(clippy::wildcard_imports)]
use crate::lower::affine::*;

impl<'a> Walk<'_, 'a, '_> {
    pub(crate) fn block(&mut self, b: &'a HirBlock, mut st: State, mode: Mode) -> (State, Val) {
        self.scopes.push(Vec::new());
        if b.label.is_some() {
            self.targets.push(Target {
                label: b.label.clone(),
                is_loop: false,
                depth: self.scopes.len() - 1,
                breaks: None,
                conts: None,
                val: None,
            });
        }
        for s in &b.stmts {
            st = self.stmt(s, st);
        }
        let mut val = Val::Plain;
        if let Some(t) = &b.trailing {
            let (s, v) = self.expr(t, st, mode);
            st = s;
            val = v;
        }
        let depth = self.scopes.len() - 1;
        if st.is_some() {
            let at = b.trailing.as_ref().map_or(self.last, |t| t.span);
            self.leave(&st, depth, at);
        }
        if b.label.is_some() {
            let t = self.targets.pop().expect("the block's target");
            if t.breaks.is_some() {
                let bv = t.val.unwrap_or(Val::Plain);
                val = if st.is_some() { combine(val, bv) } else { bv };
            }
            st = join(st, t.breaks);
        }
        let scope = self.scopes.pop().unwrap_or_default();
        for r in scope {
            let root = &self.roots[r];
            for i in 0..root.leaves.len() {
                set(&mut st, root.base + i, 0);
            }
        }
        (st, val)
    }

    pub(crate) fn stmt(&mut self, s: &'a HirStmt, st: State) -> State {
        st.as_ref()?;
        match s {
            HirStmt::Let { local, init } => {
                self.note_hidden(*local, init.span);
                self.let_stmt(*local, init, st)
            }
            HirStmt::Assign { local, value } => {
                if self.flag_locals.contains(local) {
                    self.flag_init(*local, value);
                    return st;
                }
                if self.primed_back(value) || arm_back(value) {
                    return st;
                }
                if self.write_back(*local, value) {
                    return self.expr(value, st, Mode::Use).0;
                }
                if let HirExprKind::WithField {
                    target,
                    field_index,
                    value: v,
                    ..
                } = &value.kind
                {
                    if matches!(target.kind, HirExprKind::Local(l) if l == *local) {
                        return self.field_write(*local, *field_index, v, st);
                    }
                }
                let mut st = st;
                if let Some(&r) = self.root_of.get(local) {
                    let open = self.roots[r].owned
                        && self.roots[r].kind == Kind::Close
                        && (0..self.roots[r].leaves.len())
                            .any(|i| get(&st, self.roots[r].base + i) & LIVE != 0);
                    if open {
                        let who = self.roots[r].name.clone();
                        let ty = self.c.ty_name(&self.roots[r].ty);
                        self.error(
                            Code::UNCLOSED_RESOURCE,
                            value.span,
                            format!("`{who}` still holds an open `{ty}` when this replaces it"),
                            |d| d.with_note(format!("help: close it first: `try {who}.close();`")),
                        );
                        let root = &self.roots[r];
                        for i in 0..root.leaves.len() {
                            set(&mut st, root.base + i, 0);
                        }
                    }
                }
                self.assigning = true;
                let st = self.bind_value(*local, value, st);
                self.assigning = false;
                st
            }
            HirStmt::ArrayAssign { index, value, .. } => {
                let st = self.expr(index, st, Mode::Use).0;
                self.expr(value, st, Mode::Move).0
            }
            HirStmt::StaticAssign { value, .. } => self.expr(value, st, Mode::Move).0,
            HirStmt::Expr(e) => {
                if self.release_if(e) {
                    return st;
                }
                let (st, v) = self.expr(e, st, Mode::Use);
                if v == Val::Owned && e.ty != HirTy::Never {
                    if let Some((Kind::Close, _)) = self.c.shape(&e.ty) {
                        let ty = self.c.ty_name(&e.ty);
                        let aty = article(&ty);
                        self.error(
                            Code::UNCLOSED_RESOURCE,
                            e.span,
                            format!("the `{ty}` made here is never closed"),
                            |d| {
                                d.with_note(format!(
                                    "help: closing {aty} can fail, so it is not closed for \
                                     you: bind it and close it (`let x = ..; try x.close();`), or \
                                     use `using`"
                                ))
                            },
                        );
                    }
                }
                st
            }
            HirStmt::Return(v) => {
                let st = match v {
                    Some(v) => {
                        let (st, val) = self.expr(v, st, Mode::Move);
                        if val == Val::Alias {
                            self.out.alias = true;
                        }
                        st
                    }
                    None => st,
                };
                if let Some(v) = v {
                    if !v.span.is_dummy() {
                        self.last = v.span;
                    }
                }
                // An error leaving releases what is still open: its own
                // error is the one reported.
                if st.is_some() && !v.as_ref().is_some_and(is_throw) {
                    let at = v.as_ref().map_or(self.last, |v| v.span);
                    self.leave(&st, 0, at);
                }
                None
            }
            HirStmt::Break { label, value } => {
                let (mut st, bv) = match value {
                    Some(v) => self.expr(v, st, Mode::Move),
                    None => (st, Val::Plain),
                };
                // A cancelled task's way out is not checked: what must be
                // closed counts as released there. Drop values keep their
                // state, so their release flags stay right.
                if self.quiet {
                    self.cancel_releases(&mut st);
                }
                let i = self.target(label.as_deref(), false);
                if let Some(i) = i {
                    let depth = self.targets[i].depth;
                    let raise = value.is_none()
                        && label.as_deref().is_some_and(|l| l.starts_with("#catch"));
                    if st.is_some() && !raise {
                        self.leave(&st, depth + usize::from(self.targets[i].is_loop), self.last);
                    }
                    let t = &mut self.targets[i];
                    if st.is_some() {
                        t.val = Some(t.val.map_or(bv, |x| combine(x, bv)));
                    }
                    t.breaks = join(t.breaks.take(), st);
                }
                None
            }
            HirStmt::Continue { label } => {
                let i = self.target(label.as_deref(), true);
                if let Some(i) = i {
                    let depth = self.targets[i].depth + 1;
                    if st.is_some() {
                        self.leave(&st, depth, self.last);
                    }
                    let t = &mut self.targets[i];
                    t.conts = join(t.conts.take(), st);
                }
                None
            }
            HirStmt::Panic(e) => {
                let saved = self.dying;
                self.dying = true;
                self.expr(e, st, Mode::Use);
                self.dying = saved;
                None
            }
        }
    }

    pub(crate) fn target(&self, label: Option<&str>, needs_loop: bool) -> Option<usize> {
        self.targets.iter().rposition(|t| match label {
            None => t.is_loop,
            Some(l) => t.label.as_deref() == Some(l) && (t.is_loop || !needs_loop),
        })
    }

    /// A flag's `let` or assignment: unset when its value is not owned.
    pub(crate) fn flag_init(&mut self, flag: LocalId, value: &'a HirExpr) {
        if !self.report {
            return;
        }
        let var = self
            .flags
            .iter()
            .find(|&(_, &f)| f == flag)
            .map(|(v, _)| *v);
        let owned = var
            .and_then(|v| self.bound.get(&v).copied())
            .unwrap_or(false);
        if owned {
            self.edits.unset.remove(&addr(value));
        } else {
            self.edits.unset.insert(addr(value));
        }
    }

    /// `local = <call>.k`: a `ref` argument written back. An `async`
    /// method's `ref self` comes back in a cell its future fills:
    /// `local = <call>.k[0]` (or `<call>[0]` when it returns nothing).
    pub(crate) fn write_back(&self, local: LocalId, value: &HirExpr) -> bool {
        if let HirExprKind::BufGet { buf, .. } = &value.kind {
            let target = match &buf.kind {
                HirExprKind::TupleField { target, .. } => &**target,
                _ => &**buf,
            };
            return self.call_of(target).is_some_and(|(callee, args)| {
                self.c.fns.get(&callee).is_some_and(|f| {
                    f.params
                        .iter()
                        .position(|p| p.by_ref)
                        .and_then(|i| args.get(i))
                        .is_some_and(|a| base_local(a) == Some(local))
                })
            });
        }
        let HirExprKind::TupleField { target, index, .. } = &value.kind else {
            return false;
        };
        if *index == 0 {
            return false;
        }
        let Some((callee, args)) = self.call_of(target) else {
            return false;
        };
        let Some(f) = self.c.fns.get(&callee) else {
            return false;
        };
        let refs: Vec<usize> = f
            .params
            .iter()
            .enumerate()
            .filter(|(_, p)| p.by_ref)
            .map(|(i, _)| i)
            .collect();
        refs.get(*index as usize - 1)
            .and_then(|&i| args.get(i))
            .is_some_and(|a| base_local(a) == Some(local))
    }

    /// The call `e` is, or the one its temporary was bound to.
    pub(crate) fn call_of<'x>(&self, e: &'x HirExpr) -> Option<(FunctionId, &'x [HirExpr])>
    where
        'a: 'x,
    {
        let call = match &e.kind {
            HirExprKind::Call { .. } => Some(e),
            HirExprKind::Local(t) => self.inits.get(t).copied(),
            _ => None,
        };
        match call {
            Some(HirExpr {
                kind: HirExprKind::Call { callee, args },
                ..
            }) => Some((*callee, args.as_slice())),
            _ => None,
        }
    }

    /// `local` takes `value` (a `let` or an assignment).
    pub(crate) fn bind_value(&mut self, local: LocalId, value: &'a HirExpr, st: State) -> State {
        let ty = self
            .local_ty(local)
            .filter(|t| !matches!(t, HirTy::Unknown | HirTy::Var(_)))
            .unwrap_or(&value.ty)
            .clone();
        let affine = self.c.shape(&ty).is_some();
        // A capture read out of a closure's or coroutine's environment.
        if let (Some(env), HirExprKind::TupleField { target, index, .. }) =
            (self.env_local, &value.kind)
        {
            if matches!(target.kind, HirExprKind::Local(l) if l == env) {
                if affine {
                    let mut st = st;
                    let k = SLOT + index;
                    let owned = self.owned_params.contains(&k);
                    self.bind(local, &ty, owned, Some(k), value.span, &mut st);
                    self.bound.insert(local, owned);
                    return st;
                }
                return st;
            }
        }
        // A temporary the lowering binds to a place (a `for` loop's
        // iterator, an f-string hole), and the parts a `using` pattern
        // binds, are views.
        let view = self.place(value).is_some_and(|(r, _)| {
            let l = self.root_local(r);
            let temp = self
                .f
                .locals
                .iter()
                .find(|x| x.id == local)
                .and_then(|x| x.name.as_deref())
                .is_some_and(|n| n.starts_with("#for") || is_hole_temp(n));
            temp || l
                .and_then(|l| self.f.locals.iter().find(|x| x.id == l))
                .and_then(|x| x.name.as_deref())
                .is_some_and(|n| n.starts_with("#using"))
        });
        let mode = if affine && !view {
            Mode::Move
        } else {
            Mode::Use
        };
        let (mut st, val) = self.expr(value, st, mode);
        if !affine {
            return st;
        }
        // An awaited future: polling it to the end hands on what it holds
        // (its output, or the error it raises).
        let awaited = self
            .local_name_raw(local)
            .is_some_and(|n| n.starts_with(crate::lower::coro::AWAITED));
        let val = if view || awaited { Val::Alias } else { val };
        if val == Val::Owned {
            self.bind(local, &ty, true, None, value.span, &mut st);
        } else {
            self.unbind(local, &mut st);
            if val == Val::Plain {
                // Followed from its `let` on, so a later assignment of a
                // resource leaves with the scope that declared it.
                if !self.root_of.contains_key(&local)
                    && !self.assigning
                    && self.local_name(local).is_some()
                    && self.place(value).is_none()
                {
                    self.bind(local, &ty, true, None, value.span, &mut st);
                }
                if let Some(&r) = self.root_of.get(&local) {
                    let root = &self.roots[r];
                    for i in 0..root.leaves.len() {
                        set(&mut st, root.base + i, EMPTY);
                    }
                }
            }
        }
        self.bound.insert(local, val == Val::Owned);
        st
    }

    /// `local.f = value`.
    pub(crate) fn field_write(
        &mut self,
        local: LocalId,
        field: u32,
        value: &'a HirExpr,
        st: State,
    ) -> State {
        // `ref h.f` written back by a call.
        if self.write_back(local, value) {
            return self.expr(value, st, Mode::Use).0;
        }
        // `h.f.x = v`: `h.f` updated in place.
        if let HirExprKind::WithField { target, .. } = &value.kind {
            if self
                .place(target)
                .is_some_and(|(r, p)| self.root_of.get(&local) == Some(&r) && p == [field])
            {
                return self.expr(value, st, Mode::Use).0;
            }
        }
        let affine = self.c.shape(&value.ty).is_some();
        let mode = if affine { Mode::Move } else { Mode::Use };
        let (mut st, val) = self.expr(value, st, mode);
        let Some(&r) = self.root_of.get(&local) else {
            return st;
        };
        let slots = self.slots_at(r, &[field]);
        for s in slots {
            set(&mut st, s, if val == Val::Owned { LIVE } else { 0 });
            self.why.remove(&s);
        }
        st
    }

    /// The release the lowering adds: `if #own { drop(x) }`, not walked.
    pub(crate) fn release_if(&self, e: &HirExpr) -> bool {
        match &e.kind {
            HirExprKind::If { cond, .. } => {
                matches!(cond.kind, HirExprKind::Local(l) if self.flag_locals.contains(&l))
            }
            HirExprKind::Block(b) => {
                b.trailing.is_none()
                    && b.stmts.len() == 1
                    && matches!(&b.stmts[0], HirStmt::Expr(x) if self.release_if(x))
            }
            _ => false,
        }
    }

    /// Parts moved into a new value: it is owned unless a part is not.
    pub(crate) fn exprs(
        &mut self,
        es: impl IntoIterator<Item = &'a HirExpr>,
        mut st: State,
    ) -> (State, Val) {
        let mut val = Val::Owned;
        let mut any = false;
        for x in es {
            let affine = self.c.shape(&x.ty).is_some();
            let mode = if affine { Mode::Move } else { Mode::Use };
            let (s, v) = self.expr(x, st, mode);
            st = s;
            if affine && v != Val::Plain {
                val = if any { combine(val, v) } else { v };
                any = true;
            }
        }
        (st, val)
    }

    #[allow(clippy::cognitive_complexity)] // one arm per expression kind
    pub(crate) fn expr(&mut self, e: &'a HirExpr, st: State, mode: Mode) -> (State, Val) {
        if st.is_none() {
            return (None, Val::Plain);
        }
        if !e.span.is_dummy() {
            self.last = e.span;
        }
        let affine = self.c.shape(&e.ty).is_some();
        let mode = if affine { mode } else { Mode::Use };
        if let Some((r, path)) = self.place(e) {
            let mut st = st;
            // A place's own reads: `f.fd` reads `f`.
            if mode == Mode::Move {
                let owned = self.roots[r].owned || self.roots[r].param.is_some();
                self.move_at(r, &path, e, &mut st, false);
                return (st, if owned { Val::Owned } else { Val::Alias });
            }
            // Plain data of a closed value (`f.path`) can still be read.
            let moved = self
                .slots_at(r, &path)
                .iter()
                .any(|&x| self.why.get(&x).is_some_and(|w| !w.1) && get(&st, x) & MOVED != 0);
            if affine || moved {
                self.use_at(r, &path, e.span, &st, false);
            }
            return (st, if affine { Val::Alias } else { Val::Plain });
        }
        match &e.kind {
            HirExprKind::Local(l) => {
                // A temporary the lowering made around a value.
                let _ = l;
                (
                    st,
                    if affine {
                        self.origin(e, 0)
                    } else {
                        Val::Plain
                    },
                )
            }
            HirExprKind::Block(b) => self.block(b, st, mode),
            HirExprKind::Call { callee, args } => self.call(*callee, args, e, st),
            // A function value has no summary: it takes what it is passed,
            // as a callee that keeps its parameters does.
            HirExprKind::CallIndirect {
                callee: c, args, ..
            }
            | HirExprKind::CallClosure {
                closure: c, args, ..
            } => {
                let (st, _) = self.expr(c, st, Mode::Use);
                let st = args.iter().fold(st, |s, a| self.expr(a, s, Mode::Move).0);
                (st, if affine { Val::Owned } else { Val::Plain })
            }
            HirExprKind::StructInit { fields, .. } | HirExprKind::Tuple { fields } => {
                let (st, v) = self.exprs(fields, st);
                (st, if affine { v } else { Val::Plain })
            }
            HirExprKind::EnumVariantInit { args, .. } => {
                // A variant with no resource in it holds nothing to follow
                // (`Stdio::Piped`), unless the enum's own `drop` runs.
                let holds = args.iter().any(|a| self.c.shape(&a.ty).is_some());
                let ctx = self.c.ctx;
                let own = ctx.has_trait(&e.ty, ctx.lang("drop_method"));
                let (st, v) = self.exprs(args, st);
                (
                    st,
                    if affine && (holds || own) {
                        v
                    } else {
                        Val::Plain
                    },
                )
            }
            HirExprKind::ArrayInit { elements, .. } => {
                let (st, v) = self.exprs(elements, st);
                (st, if affine { v } else { Val::Plain })
            }
            HirExprKind::ArrayRepeat { value, .. } => {
                let (st, v) = self.exprs(std::iter::once(&**value), st);
                (st, if affine { v } else { Val::Plain })
            }
            HirExprKind::SumWrap { value, .. } => match value {
                Some(v) => {
                    let (st, val) = self.expr(v, st, mode);
                    (st, if affine { val } else { Val::Plain })
                }
                None => (st, Val::Plain),
            },
            HirExprKind::WithField { target, value, .. } => {
                // An update in place: the target keeps its owner.
                let (st, a) = match self.place(target) {
                    Some((r, _)) => {
                        let owned = self.roots[r].owned;
                        let (st, _) = self.expr(target, st, Mode::Use);
                        (st, if owned { Val::Owned } else { Val::Alias })
                    }
                    None => self.expr(target, st, mode),
                };
                let (st, b) = self.exprs(std::iter::once(&**value), st);
                (st, if affine { combine(a, b) } else { Val::Plain })
            }
            HirExprKind::FieldRead { target, .. } | HirExprKind::TupleField { target, .. } => {
                // A part of a value that is not a followed place: a call's
                // result, a `ref` call's parts.
                if let HirExprKind::TupleField { index, .. } = &e.kind {
                    if let Some(v) = self.ref_part(target, *index, st.clone()) {
                        return v;
                    }
                }
                let (st, v) = self.expr(target, st, mode);
                let v = match (affine, v) {
                    (false, _) => Val::Plain,
                    (true, Val::Plain) => self.origin(e, 0),
                    (true, v) => v,
                };
                (st, v)
            }
            HirExprKind::Cast { operand, .. } => {
                let (st, v) = self.expr(operand, st, mode);
                (st, if affine { v } else { Val::Plain })
            }
            HirExprKind::If {
                cond,
                then_block,
                else_branch,
            } => {
                let (st, _) = self.expr(cond, st, Mode::Use);
                // A cancellation point leaves through the cleanups.
                let cancel = self.cancel_check(cond);
                let (a, va) = if cancel {
                    let saved = self.quiet;
                    self.quiet = true;
                    let r = self.block(then_block, st.clone(), mode);
                    self.quiet = saved;
                    (None, r.1)
                } else {
                    self.block(then_block, st.clone(), mode)
                };
                // A coroutine's `if true { '#coro: { body } } else { undef }`:
                // the `else` is never taken.
                let (b, vb) = match else_branch {
                    _ if matches!(cond.kind, HirExprKind::Const(ConstVal::Bool(true))) => {
                        (None, Val::Plain)
                    }
                    Some(x) => self.expr(x, st, mode),
                    None => (st, Val::Plain),
                };
                let v = match (a.is_some(), b.is_some()) {
                    (true, true) => combine(va, vb),
                    (true, false) => va,
                    (false, true) => vb,
                    _ => Val::Plain,
                };
                (join(a, b), v)
            }
            HirExprKind::Match { scrutinee, arms } => self.matched(scrutinee, arms, st, mode),
            HirExprKind::Loop { label, body, .. } => self.looped(label.as_deref(), None, body, st),
            HirExprKind::While { label, cond, body } => {
                self.looped(label.as_deref(), Some(cond), body, st)
            }
            HirExprKind::ClosureValue { fn_id, env, .. } => {
                (self.captures(*fn_id, env, st), Val::Plain)
            }
            HirExprKind::CoroNew { body_fn, args } => {
                let mut st = st;
                for a in args {
                    st = self.captures(*body_fn, a, st);
                }
                (st, Val::Plain)
            }
            HirExprKind::BufSet {
                buf, index, value, ..
            } => {
                let (st, _) = self.expr(buf, st, Mode::Use);
                let (st, _) = self.expr(index, st, Mode::Use);
                let st = self.exprs(std::iter::once(&**value), st).0;
                (st, Val::Plain)
            }
            // Its address taken: raw access from here on, not followed.
            HirExprKind::AddrOf { place } => {
                let mut st = st;
                if let Some((r, path)) = self.place(place) {
                    for x in self.slots_at(r, &path) {
                        set(&mut st, x, 0);
                    }
                    let flag = self.root_local(r).and_then(|l| self.flags.get(&l).copied());
                    if let (true, Some(flag)) = (self.report, flag) {
                        self.edits.clears.entry(addr(e)).or_default().push(flag);
                    }
                    return (st, Val::Plain);
                }
                (self.expr(place, st, Mode::Use).0, Val::Plain)
            }
            HirExprKind::Suspend { value, .. } => {
                let st = self.exprs(std::iter::once(&**value), st).0;
                (st, Val::Plain)
            }
            _ => {
                let mut st = st;
                for c in children(e) {
                    st = self.expr(c, st, Mode::Use).0;
                }
                (
                    st,
                    if affine {
                        self.origin(e, 0)
                    } else {
                        Val::Plain
                    },
                )
            }
        }
    }

    /// Where the value of `e` comes from, without walking it: a value
    /// through the temporaries the lowering makes (a `throws` carrier).
    pub(crate) fn origin(&self, e: &HirExpr, depth: u32) -> Val {
        if depth > 16 {
            return Val::Alias;
        }
        match &e.kind {
            HirExprKind::Local(l) => {
                if self.root_of.contains_key(l) {
                    return Val::Alias;
                }
                match self.inits.get(l) {
                    Some(i) if self.local_name(*l).is_none() => self.origin(i, depth + 1),
                    _ => Val::Alias,
                }
            }
            HirExprKind::Call { callee, .. } => {
                if self.c.summary(*callee).is_some_and(|s| s.alias) {
                    Val::Alias
                } else {
                    Val::Owned
                }
            }
            HirExprKind::CallIndirect { .. }
            | HirExprKind::CallClosure { .. }
            | HirExprKind::StructInit { .. }
            | HirExprKind::Tuple { .. }
            | HirExprKind::EnumVariantInit { .. } => Val::Owned,
            HirExprKind::TupleField { target, index, .. } => {
                let refs = match &target.kind {
                    HirExprKind::Call { callee, .. } => self
                        .c
                        .fns
                        .get(callee)
                        .is_some_and(|f| f.params.iter().any(|p| p.by_ref)),
                    _ => false,
                };
                if refs && *index > 0 {
                    Val::Alias
                } else {
                    self.origin(target, depth + 1)
                }
            }
            HirExprKind::FieldRead { target, .. }
            | HirExprKind::UnionRead { target, .. }
            | HirExprKind::Cast {
                operand: target, ..
            }
            | HirExprKind::SumWrap {
                value: Some(target),
                ..
            } => self.origin(target, depth + 1),
            HirExprKind::Block(b) => b
                .trailing
                .as_ref()
                .map_or(Val::Plain, |t| self.origin(t, depth + 1)),
            _ => Val::Alias,
        }
    }

    /// `e` asks whether the task was cancelled (an `await`'s lowering).
    pub(crate) fn cancel_check(&self, e: &HirExpr) -> bool {
        let mut hit = false;
        crate::each_expr(e, &mut |x| {
            if let HirExprKind::Call { callee, .. } = &x.kind {
                hit |= self.c.fns.get(callee).is_some_and(|f| {
                    matches!(
                        f.name.as_str(),
                        "core::task::cancel_point" | "core::task::cancel_left"
                    )
                });
            }
        });
        hit
    }

    /// Part `index` of a call with `ref` parameters: the result (0) or a
    /// `ref` argument written back.
    pub(crate) fn ref_part(
        &mut self,
        target: &'a HirExpr,
        index: u32,
        st: State,
    ) -> Option<(State, Val)> {
        let HirExprKind::Call { callee, .. } = &target.kind else {
            return None;
        };
        let f = self.c.fns.get(callee)?;
        if !f.params.iter().any(|p| p.by_ref) {
            return None;
        }
        let (st, _) = self.expr(target, st, Mode::Use);
        Some((
            st,
            if index == 0 {
                self.ref_result
            } else {
                Val::Alias
            },
        ))
    }

    /// Captured values: an owned one moves in; a borrowed one is lent
    /// unless the body keeps it.
    pub(crate) fn captures(&mut self, body: FunctionId, env: &'a HirExpr, mut st: State) -> State {
        let HirExprKind::Tuple { fields } = &env.kind else {
            return self.expr(env, st, Mode::Use).0;
        };
        let keeps = self.c.summary(body).map(|s| s.keeps).unwrap_or_default();
        for (k, x) in fields.iter().enumerate() {
            let k = u32::try_from(k).unwrap_or(u32::MAX);
            let affine = self.c.shape(&x.ty).is_some();
            if !affine {
                st = self.expr(x, st, Mode::Use).0;
                continue;
            }
            let owned_place = self.place(x).is_some_and(|(r, _)| self.roots[r].owned);
            let fresh = self.place(x).is_none();
            let mode = if owned_place || keeps.contains(&(SLOT + k)) {
                Mode::Move
            } else {
                Mode::Use
            };
            if mode == Mode::Move
                && !self
                    .c
                    .ctx
                    .src(x.span)
                    .is_some_and(|t| t.trim_start().starts_with('['))
            {
                self.captured.insert(x.span);
            }
            let (s, v) = self.expr(x, st, mode);
            st = s;
            if self.report
                && (owned_place || (fresh && v == Val::Owned) || keeps.contains(&(SLOT + k)))
            {
                self.c.owned_slots.borrow_mut().insert((body, k));
            }
        }
        st
    }

    pub(crate) fn call(
        &mut self,
        callee: FunctionId,
        args: &'a [HirExpr],
        e: &'a HirExpr,
        mut st: State,
    ) -> (State, Val) {
        let f = self.c.fns.get(&callee).copied();
        let sum = self.c.summary(callee);
        let method = f.map_or("", |f| method_name(&f.name));
        let own_method = f.is_some_and(|f| {
            f.params.first().is_some_and(|p| p.name == "self")
                && args.first().is_some_and(|a| a.ty == f.params[0].ty)
        });
        // `marker::share(x)`: a second handle nobody follows.
        if f.is_some_and(|f| SHARES.contains(&f.name.split("__mono__").next().unwrap_or(""))) {
            for a in args {
                st = self.expr(a, st, Mode::Use).0;
            }
            return (st, Val::Alias);
        }
        if let Some(f) =
            f.filter(|f| SWAPS.contains(&f.name.split("__mono__").next().unwrap_or("")))
        {
            let owned = args
                .first()
                .and_then(|a| self.place(a))
                .is_some_and(|(r, _)| self.roots[r].owned);
            let _ = f;
            for (i, a) in args.iter().enumerate() {
                let mode = if i == 0 { Mode::Use } else { Mode::Move };
                st = self.expr(a, st, mode).0;
            }
            self.ref_result = if owned { Val::Owned } else { Val::Alias };
            return (st, Val::Alias);
        }
        // `using`'s release, or the compiler's.
        if method == "drop" && f.is_some_and(|f| f.params.len() == 1) {
            if let Some(a) = args.first() {
                self.dropped(a, &mut st);
            }
            return (st, Val::Plain);
        }
        for (i, a) in args.iter().enumerate() {
            let p = f.and_then(|f| f.params.get(i));
            let by_ref = p.is_some_and(|p| p.by_ref);
            let i = u32::try_from(i).unwrap_or(u32::MAX);
            let closer = i == 0 && own_method && CLOSERS.contains(&method);
            let iter = method == "into_iter";
            let into = i == 0 && own_method && method.starts_with("into_") && !iter;
            let keeps = !iter && sum.as_ref().is_some_and(|s| s.keeps.contains(&i));
            let affine = self.c.shape(&a.ty).is_some();
            if !affine || by_ref || !(closer || into || keeps) {
                let (next, val) = self.expr(a, st, Mode::Use);
                st = next;
                // `await` polls the future it runs to the end.
                if val == Val::Owned && self.place(a).is_none() && method != "poll" {
                    self.temp_left_open(a);
                }
                continue;
            }
            if closer {
                if let Some((r, path)) = self.place(a) {
                    self.move_at(r, &path, a, &mut st, true);
                    continue;
                }
            }
            st = self.expr(a, st, Mode::Move).0;
        }
        let ret = f.map_or(&e.ty, |f| &f.ret_ty);
        let affine = self.c.shape(&e.ty).is_some() || self.c.shape(ret).is_some();
        let v = if !affine {
            Val::Plain
        } else if method == "into_iter" || sum.is_some_and(|s| s.alias) {
            Val::Alias
        } else {
            Val::Owned
        };
        // A call with `ref` parameters returns `(result, ref args..)`: the
        // tuple is not one value; its part 0 is the result.
        if f.is_some_and(|f| f.params.iter().any(|p| p.by_ref)) {
            self.ref_result = v;
            return (st, Val::Alias);
        }
        (st, v)
    }

    /// A value that must be closed, made for a call that only borrows it
    /// (`File::open(p).read_to_str()`): nothing closes it.
    pub(crate) fn temp_left_open(&mut self, a: &HirExpr) {
        if a.ty == HirTy::Never || !matches!(self.c.shape(&a.ty), Some((Kind::Close, _))) {
            return;
        }
        let ty = self.c.ty_name(&a.ty);
        let aty = article(&ty);
        self.error(
            Code::UNCLOSED_RESOURCE,
            a.span,
            format!("the `{ty}` made here is never closed: this call only borrows it"),
            |d| {
                d.with_note(format!(
                    "help: closing {aty} can fail, so it is not closed for you: bind it with \
                     `using` (`using x = .. {{ .. }}`), or bind it and close it (`let x = ..; \
                     .. try x.close();`)"
                ))
            },
        );
    }

    pub(crate) fn matched(
        &mut self,
        scrutinee: &'a HirExpr,
        arms: &'a [crate::HirMatchArm],
        st: State,
        mode: Mode,
    ) -> (State, Val) {
        let mut binds = false;
        for a in arms {
            pattern_bindings(&a.pat, &mut |_, ty| binds |= self.c.shape(ty).is_some());
        }
        // A sum's members are parts of their own: an arm binds the one it
        // narrows to.
        let at = self.place(scrutinee).filter(|(r, p)| {
            matches!(scrutinee.ty, HirTy::Sum(_)) || self.slots_at(*r, p).len() == 1
        });
        let mode_s = if binds && at.is_none() {
            Mode::Move
        } else {
            Mode::Use
        };
        let (st, sv) = self.expr(scrutinee, st, mode_s);
        let mut out: State = None;
        let mut val: Option<Val> = None;
        // Per member of a sum, or variant of an enum: how the resource it
        // holds is released, if it holds one.
        let kind = |t: &HirTy| self.c.shape(t).map(|s| s.0);
        let members: Option<Vec<Option<Kind>>> = match &scrutinee.ty {
            HirTy::Sum(ts) => Some(ts.iter().map(kind).collect()),
            HirTy::Enum(eid) => Some(
                self.c
                    .ctx
                    .enum_def(*eid)
                    .variants
                    .iter()
                    .map(|v| {
                        crate::lower::expr::variant_arg_tys(v)
                            .iter()
                            .filter_map(kind)
                            .max_by_key(|k| *k == Kind::Close)
                    })
                    .collect(),
            ),
            _ => None,
        };
        let mut taken: HashSet<u32> = HashSet::default();
        for a in arms {
            let mut s = st.clone();
            // The members this arm may see; none of them a resource: the
            // place holds nothing to release there.
            if let (Some(ms), Some((r, p))) = (&members, &at) {
                let here: Vec<u32> = match &a.pat {
                    HirPattern::SumVariant { variant_index, .. }
                    | HirPattern::SumMember { variant_index, .. }
                    | HirPattern::EnumVariant { variant_index, .. } => vec![*variant_index],
                    HirPattern::Wildcard | HirPattern::Binding { .. } => (0..ms.len())
                        .filter_map(|i| u32::try_from(i).ok())
                        .filter(|i| !taken.contains(i))
                        .collect(),
                    _ => (0..ms.len())
                        .filter_map(|i| u32::try_from(i).ok())
                        .collect(),
                };
                if a.guard.is_none() {
                    match &a.pat {
                        HirPattern::SumVariant { variant_index, .. }
                        | HirPattern::SumMember { variant_index, .. } => {
                            taken.insert(*variant_index);
                        }
                        HirPattern::EnumVariant {
                            variant_index,
                            args,
                            ..
                        } if args.iter().all(HirPattern::is_irrefutable) => {
                            taken.insert(*variant_index);
                        }
                        _ => {}
                    }
                }
                let sum = matches!(scrutinee.ty, HirTy::Sum(_));
                self.arm_members(*r, p, ms, &here, sum, &mut s);
            }
            self.scopes.push(Vec::new());
            let mut parts = HashMap::default();
            if let Some((r, p)) = &at {
                pattern_places(&a.pat, &mut p.clone(), &mut |l, ty, q| {
                    if self.c.shape(ty).is_some() {
                        merge_place(&mut parts, l, q);
                    }
                });
                for (&l, q) in &parts {
                    self.alias_of.insert(l, (*r, q.clone()));
                }
            }
            let mut locals = Vec::new();
            pattern_bindings(&a.pat, &mut |l, ty| locals.push((l, ty.clone())));
            for (l, ty) in locals {
                if at.is_some() {
                    continue;
                }
                if sv == Val::Owned {
                    self.bind(l, &ty, true, None, scrutinee.span, &mut s);
                } else {
                    self.unbind(l, &mut s);
                }
            }
            if let Some(g) = &a.guard {
                s = self.expr(g, s, Mode::Use).0;
            }
            let (s, v) = self.expr(&a.body, s, mode);
            if let (Some((r, _)), true) = (&at, s.is_some()) {
                self.arm_left_open(*r, &parts, &a.pat, &s, a.body.span);
            }
            let depth = self.scopes.len() - 1;
            if s.is_some() {
                self.leave(&s, depth, a.body.span);
            }
            let scope = self.scopes.pop().unwrap_or_default();
            let mut s = s;
            for r in scope {
                let root = &self.roots[r];
                for i in 0..root.leaves.len() {
                    set(&mut s, root.base + i, 0);
                }
            }
            for l in parts.keys() {
                self.alias_of.remove(l);
            }
            if s.is_some() {
                val = Some(val.map_or(v, |x| combine(x, v)));
            }
            out = join(out, s);
        }
        (out, val.unwrap_or(Val::Plain))
    }

    /// What the place `(r, p)` holds in an arm that sees the members
    /// `here` of its type (`ms`: each member's release kind).
    pub(crate) fn arm_members(
        &self,
        r: usize,
        p: &[u32],
        ms: &[Option<Kind>],
        here: &[u32],
        sum: bool,
        s: &mut State,
    ) {
        let empty = here
            .iter()
            .all(|&i| ms.get(i as usize).copied().unwrap_or(None).is_none());
        // Every member here is released for the program: the arm
        // owes no close, the flag still releases what it holds.
        let auto = here.iter().all(|&i| {
            ms.get(i as usize)
                .copied()
                .unwrap_or(None)
                .is_none_or(|k| k == Kind::Auto)
        });
        let close = self.roots[r].kind == Kind::Close;
        if sum {
            // Per member: one the arm cannot see holds nothing.
            for (member, kind) in ms.iter().enumerate() {
                let Ok(member) = u32::try_from(member) else {
                    continue;
                };
                let seen = here.contains(&member);
                if seen && !(close && *kind == Some(Kind::Auto)) {
                    continue;
                }
                let mut part = p.to_vec();
                part.push(member);
                for slot in self.slots_at(r, &part) {
                    let bits = get(s, slot);
                    if bits & LIVE != 0 {
                        set(s, slot, if seen { bits & !LIVE } else { 0 });
                    }
                }
            }
        } else if empty || (auto && close) {
            for slot in self.slots_at(r, p) {
                let bits = get(s, slot);
                if bits & LIVE != 0 {
                    set(s, slot, if empty { 0 } else { bits & !LIVE });
                }
            }
        }
    }

    /// Note the slots of `(r, p)` an arm binding a must-close member
    /// leaves open, so a missing close names that binding and that arm.
    pub(crate) fn arm_left_open(
        &mut self,
        r: usize,
        parts: &HashMap<LocalId, Vec<u32>>,
        pat: &HirPattern,
        s: &State,
        body: Span,
    ) {
        if self.roots[r].kind != Kind::Close {
            return;
        }
        let mut bound = Vec::new();
        pattern_bindings(pat, &mut |l, ty| {
            if self.c.shape(ty).is_some_and(|x| x.0 == Kind::Close) {
                bound.push((l, ty.clone()));
            }
        });
        // The first binding naming a slot is the one shown.
        for (l, ty) in bound.into_iter().rev() {
            let Some(q) = parts.get(&l) else { continue };
            for x in self.slots_at(r, q) {
                if get(s, x) & LIVE != 0 {
                    self.arm_open.insert(x, (l, ty.clone(), body, q.clone()));
                }
            }
        }
    }

    pub(crate) fn looped(
        &mut self,
        label: Option<&str>,
        cond: Option<&'a HirExpr>,
        body: &'a HirBlock,
        st: State,
    ) -> (State, Val) {
        let mut head = st.clone();
        let entry = st;
        let mut exit: State = None;
        let mut breaks: State = None;
        let mut lv: Option<Val> = None;
        for _ in 0..8 {
            self.targets.push(Target {
                label: label.map(str::to_owned),
                is_loop: true,
                depth: self.scopes.len(),
                breaks: None,
                conts: None,
                val: None,
            });
            let mut s = head.clone();
            if let Some(c) = cond {
                s = self.expr(c, s, Mode::Use).0;
                exit = join(exit.take(), s.clone());
            }
            let (end, _) = self.block(body, s, Mode::Use);
            let t = self.targets.pop().expect("the loop's target");
            breaks = join(breaks.take(), t.breaks);
            if let Some(v) = t.val {
                lv = Some(lv.map_or(v, |x| combine(x, v)));
            }
            let next = join(entry.clone(), join(end, t.conts));
            if next == head {
                break;
            }
            head = next;
        }
        let out = if cond.is_some() {
            join(exit, breaks)
        } else {
            breaks
        };
        (out, lv.unwrap_or(Val::Plain))
    }
}

/// The local a place expression starts from.
pub(crate) fn base_local(e: &HirExpr) -> Option<LocalId> {
    match &e.kind {
        HirExprKind::Local(l) => Some(*l),
        HirExprKind::FieldRead { target, .. } | HirExprKind::TupleField { target, .. } => {
            base_local(target)
        }
        _ => None,
    }
}

/// `return` of an error carrier: a `throw` or a `try` passing one on.
pub(crate) fn is_throw(v: &HirExpr) -> bool {
    let HirExprKind::Tuple { fields } = &v.kind else {
        return false;
    };
    matches!(v.ty, HirTy::Throws { .. })
        && matches!(
            fields.first().map(|f| &f.kind),
            Some(HirExprKind::Const(ConstVal::Bool(true)))
        )
}

/// The locals a pattern binds, with their types.
pub(crate) fn pattern_bindings(p: &HirPattern, f: &mut impl FnMut(LocalId, &HirTy)) {
    match p {
        HirPattern::Binding { local, ty } => f(*local, ty),
        HirPattern::At { local, ty, sub, .. } => {
            f(*local, ty);
            pattern_bindings(sub, f);
        }
        HirPattern::Or(ps) | HirPattern::Tuple(ps) => {
            for q in ps {
                pattern_bindings(q, f);
            }
        }
        HirPattern::Struct { fields, .. } => {
            for (_, q) in fields {
                pattern_bindings(q, f);
            }
        }
        HirPattern::EnumVariant { args, .. } => {
            for q in args {
                pattern_bindings(q, f);
            }
        }
        HirPattern::SumVariant {
            binder: Some(l),
            payload_ty: Some(ty),
            ..
        } => f(*l, ty),
        HirPattern::SumMember { sub, .. } => pattern_bindings(sub, f),
        HirPattern::Slice(s) => s.items().for_each(|(_, q)| pattern_bindings(q, f)),
        _ => {}
    }
}

/// The locals a pattern on the place `path` binds, with their types and
/// the part of the place each one names.
pub(crate) fn pattern_places(
    p: &HirPattern,
    path: &mut Vec<u32>,
    f: &mut impl FnMut(LocalId, &HirTy, &[u32]),
) {
    let mut sub = |i: u32, q: &HirPattern, path: &mut Vec<u32>| {
        path.push(i);
        pattern_places(q, path, f);
        path.pop();
    };
    match p {
        HirPattern::Binding { local, ty } => f(*local, ty, path),
        HirPattern::At { local, ty, sub, .. } => {
            f(*local, ty, path);
            pattern_places(sub, path, f);
        }
        HirPattern::Or(ps) => {
            for q in ps {
                pattern_places(q, path, f);
            }
        }
        HirPattern::Tuple(ps) => {
            for (i, q) in ps.iter().enumerate() {
                sub(u32::try_from(i).unwrap_or(u32::MAX), q, path);
            }
        }
        HirPattern::Struct { fields, .. } => {
            for (i, q) in fields {
                sub(*i, q, path);
            }
        }
        // An enum is followed whole.
        HirPattern::EnumVariant { args, .. } => {
            for q in args {
                pattern_places(q, path, f);
            }
        }
        HirPattern::SumVariant {
            variant_index,
            binder: Some(l),
            payload_ty: Some(ty),
            ..
        } => {
            path.push(*variant_index);
            f(*l, ty, path);
            path.pop();
        }
        HirPattern::SumMember {
            variant_index,
            sub: q,
            ..
        } => sub(*variant_index, q, path),
        // An element is a collection's, not a part of the place.
        HirPattern::Slice(s) => {
            for (_, q) in s.items() {
                pattern_places(q, &mut Vec::new(), f);
            }
        }
        _ => {}
    }
}

/// `l` names `q`: an or-pattern binding it in two places names what
/// holds both.
pub(crate) fn merge_place(parts: &mut HashMap<LocalId, Vec<u32>>, l: LocalId, q: &[u32]) {
    let e = parts.entry(l).or_insert_with(|| q.to_vec());
    let n = e.iter().zip(q).take_while(|(a, b)| a == b).count();
    e.truncate(n);
}

/// The direct subexpressions of `e`.
pub(crate) fn children(e: &HirExpr) -> Vec<&HirExpr> {
    let mut out = Vec::new();
    nabic_hir_check::send::each_child(e, &mut |c| {
        if let nabic_hir_check::send::Child::E(x) = c {
            out.push(x);
        }
    });
    out
}

/// A function's method name: `File.close`, `Struct7__close__by_Close`,
/// `m::Sender__close__mono__..`, `m::__impl1a2b_drop`.
pub(crate) fn method_name(name: &str) -> &str {
    let n = name.split("__mono__").next().unwrap_or(name);
    let n = n.split("__by_").next().unwrap_or(n);
    let n = n.rsplit("::").next().unwrap_or(n);
    if let Some(rest) = n.strip_prefix("__impl") {
        return rest.split_once('_').map_or(rest, |(_, m)| m);
    }
    let n = n.rsplit('.').next().unwrap_or(n);
    n.rsplit("__").next().unwrap_or(n)
}
