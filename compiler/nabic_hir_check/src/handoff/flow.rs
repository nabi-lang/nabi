//! Assignments, stores, guard ties and value flows.

#[allow(clippy::wildcard_imports)]
use crate::handoff::*;

impl<'h> Walk<'_, 'h> {
    pub(crate) fn mark_gone(&mut self, p: Place, why: Gone) {
        for s in self.cur.states() {
            mark_gone(s, p.clone(), why);
        }
    }

    /// `p` is not gone any more; a gone place it is a field of: its other
    /// fields stay gone.
    pub(crate) fn regain(&self, s: &mut St, p: &Place) {
        let wider: Vec<(Place, Gone)> = s
            .gone
            .iter()
            .filter(|(g, _)| within(p, g) && g.1.len() < p.1.len())
            .map(|(g, w)| (g.clone(), *w))
            .collect();
        s.gone.retain(|g, _| !within(g, p) && !within(p, g));
        for (g, why) in wider {
            match self.split(&g, &p.1[g.1.len()..]) {
                Some(rest) => {
                    for r in rest {
                        s.gone.insert(extend(&g, &r), why);
                    }
                }
                None => {
                    s.gone.insert(g, why);
                }
            }
        }
    }

    /// `p` gets a new value: it is ours again and aliases nothing. Edges
    /// through it are joined past it first: what aliased its old value
    /// still aliases each other.
    pub(crate) fn assign(&mut self, p: &Place) {
        let mut cur = std::mem::take(&mut self.cur);
        for (_, s) in &mut cur.0 {
            *s = self.assign_st(std::mem::take(s), p);
        }
        cur.untag(p);
        self.cur = cur;
    }

    /// The edges of a copy that `p` is a field of, `p` given a new value:
    /// its other fields stay copies, and its copy of `p` is still the old
    /// value (`rep`).
    pub(crate) fn field_copies(
        &self,
        keep: Vec<Edge>,
        p: &Place,
        rep: Option<&Place>,
        edges: &mut HashSet<Edge>,
    ) {
        for e in keep {
            let Edge::Same(a, b) = &e else {
                edges.insert(e);
                continue;
            };
            let (base, other) = if within(p, a) {
                (a, b)
            } else if within(p, b) {
                (b, a)
            } else {
                edges.insert(e);
                continue;
            };
            match self.split(base, &p.1[base.1.len()..]) {
                Some(rest) => {
                    for r in rest {
                        edges.insert(Edge::Same(extend(base, &r), extend(other, &r)));
                    }
                    if let Some(r) = rep {
                        edges.insert(Edge::Same(extend(other, &p.1[base.1.len()..]), r.clone()));
                    }
                }
                None => {
                    edges.insert(e);
                }
            }
        }
    }

    pub(crate) fn assign_st(&self, mut s: St, p: &Place) -> St {
        self.regain(&mut s, p);
        for t in s.tied.values_mut() {
            t.retain(|q, _| !within(q, p));
        }
        let (touch, keep): (Vec<Edge>, Vec<Edge>) =
            s.edges.take_of(p.0).into_iter().partition(|e| e.touches(p));
        // Hashed: only the state's ordered edge map needs an order.
        let mut edges: HashSet<Edge> = HashSet::default();
        // A copy of all of `p`'s old value stands in for it in the rest.
        let rep = touch.iter().find_map(|e| match e {
            Edge::Same(a, b) if a == p && !within(b, p) => Some(b.clone()),
            Edge::Same(a, b) if b == p && !within(a, p) => Some(a.clone()),
            _ => None,
        });
        self.field_copies(keep, p, rep.as_ref(), &mut edges);
        if let Some(r) = rep {
            let swap = |u: &Place| {
                if within(u, p) {
                    extend(&r, &u.1[p.1.len()..])
                } else {
                    u.clone()
                }
            };
            for e in &touch {
                let e = match e {
                    Edge::Same(a, b) => Edge::Same(swap(a), swap(b)),
                    Edge::In(a, b) => Edge::In(swap(a), swap(b)),
                    Edge::Into(a, b) => Edge::Into(swap(a), swap(b)),
                    Edge::Taken(a, b) => Edge::Taken(swap(a), swap(b)),
                };
                edges.insert(e);
            }
            for e in edges.into_iter().filter(kept) {
                s.edges.insert(e);
            }
            return s;
        }
        // Each end in `p`: (end, other end, other => end, end => other, same,
        // taken: 1 the end was taken out of the other, 2 the reverse).
        let mut ends: Vec<(Place, Place, bool, bool, bool, u8)> = Vec::new();
        for e in &touch {
            let (a, b) = e.ends();
            for (u, x, fwd) in [(a, b, true), (b, a, false)] {
                if !within(u, p) || within(x, p) {
                    continue;
                }
                let (to_u, from_u, same) = match e {
                    Edge::Same(..) => (true, true, true),
                    Edge::In(..) => (true, true, false),
                    // `Into(a, b)`: a => b; a taken element as one.
                    Edge::Into(..) | Edge::Taken(..) => (!fwd, fwd, false),
                };
                let taken = match e {
                    Edge::Taken(..) if fwd => 1,
                    Edge::Taken(..) => 2,
                    _ => 0,
                };
                ends.push((u.clone(), x.clone(), to_u, from_u, same, taken));
            }
        }
        // Only pairs whose ends overlap join: index the ends by place, so
        // each end meets the ends inside it (a sorted range) and those it
        // is inside (its prefixes), not every other end. An edge `kept`
        // drops is not made at all.
        let mut by_place: Vec<usize> = (0..ends.len()).collect();
        by_place.sort_by(|&a, &b| ends[a].0.cmp(&ends[b].0).then(a.cmp(&b)));
        let mut pairs: Vec<(usize, usize)> = Vec::new();
        for (r, &i) in by_place.iter().enumerate() {
            let u = &ends[i].0;
            // Same place, or inside `u`: they sort right after it.
            for &j in &by_place[r + 1..] {
                if !within(&ends[j].0, u) {
                    break;
                }
                pairs.push((i.min(j), i.max(j)));
            }
        }
        let mut edges_add = |e: Edge| {
            if kept(&e) {
                edges.insert(e);
            }
        };
        for (i, j) in pairs {
            let (u1, x1, to1, from1, same1, tk1) = &ends[i];
            let (u2, x2, to2, from2, same2, tk2) = &ends[j];
            if x1 == x2 {
                continue;
            }
            let both1 = *to1 && *from1 && *tk1 == 0;
            let both2 = *to2 && *from2 && *tk2 == 0;
            if let Some(e) = taken_through(u1 == u2, (x1, both1, *tk1), (x2, both2, *tk2)) {
                edges_add(e);
                continue;
            }
            if *same1 && *same2 {
                let e = if within(u2, u1) {
                    Edge::Same(extend(x1, &u2.1[u1.1.len()..]), x2.clone())
                } else {
                    Edge::Same(x1.clone(), extend(x2, &u1.1[u2.1.len()..]))
                };
                edges_add(e);
                continue;
            }
            let fwd = *to1 && *from2;
            let back = *to2 && *from1;
            match (fwd, back) {
                (true, true) => edges_add(Edge::In(x1.clone(), x2.clone())),
                (true, false) => edges_add(Edge::Into(x1.clone(), x2.clone())),
                (false, true) => edges_add(Edge::Into(x2.clone(), x1.clone())),
                (false, false) => {}
            };
        }
        for e in edges.into_iter().filter(kept) {
            s.edges.insert(e);
        }
        s
    }

    /// `e` may own a `[move x]` capture that is not `Sync`: a closure that
    /// moved one in (its local keeps the closure type when a `fn` slot took
    /// it), or a `fn` value whose type does not say `Sync`. Such a value
    /// is handed over once.
    pub(crate) fn owns_moved(&self, e: &HirExpr) -> bool {
        let ty = match &e.kind {
            // A closure made here: its captures were moved when it was made.
            HirExprKind::ClosureValue { .. } => return false,
            HirExprKind::Local(l) => self.local_ty(*l).unwrap_or_else(|| e.ty.clone()),
            _ => e.ty.clone(),
        };
        // A `fn` value that is not `Sync` may own one.
        match &ty {
            HirTy::Closure { moved, .. } => *moved != 0 && self.c.tracked(&ty),
            HirTy::Fn(..) => self.c.tracked(&ty),
            _ => false,
        }
    }

    /// `owns_moved`, also for a closure whose type lost what it moved in
    /// (stored in a tuple, a field, a list): it is a closure that did.
    pub(crate) fn moves_own(&self, e: &HirExpr) -> bool {
        if self.owns_moved(e) {
            return true;
        }
        if !matches!(e.ty, HirTy::Fn(..) | HirTy::Closure { .. }) {
            return false;
        }
        self.reaches_moved_closure(e)
    }

    /// `e` is (or shares) a closure that owns moved values.
    pub(crate) fn reaches_moved_closure(&self, e: &HirExpr) -> bool {
        let mut fl = Vec::new();
        self.flows_any(e, &mut fl);
        let owner = |q: &Place| {
            q.1.is_empty()
                && matches!(self.local_ty(q.0), Some(ref t @ HirTy::Closure { moved, .. })
                    if moved != 0 && self.c.tracked(t))
        };
        self.cur.0.iter().any(|(_, st)| {
            let mut seen: HashSet<Place> = HashSet::default();
            let mut work: Vec<Place> = fl.iter().map(|(_, q)| q.clone()).collect();
            while let Some(q) = work.pop() {
                if owner(&q) {
                    return true;
                }
                if seen.len() > 64 || !seen.insert(q.clone()) {
                    continue;
                }
                work.extend(step(st, &q));
            }
            false
        })
    }

    /// A parameter typed `fn(..) + Send` without `Sync` is one the callee
    /// may run on another thread: an argument that may own moved values
    /// goes there, whoever the callee is.
    pub(crate) fn send_fn_args(&mut self, args: &[HirExpr], at: Span) {
        let send = self.c.env.lang("send_trait");
        let sync = self.c.env.lang("sync_trait");
        let mut owners = Vec::new();
        for a in args {
            let HirTy::Fn(_, _, b) = &a.ty else { continue };
            if !b.auto.iter().any(|k| *k == send) || b.auto.iter().any(|k| *k == sync) {
                continue;
            }
            // A `fn` value adapted to the slot (`throws` added) is a closure
            // holding it.
            match &a.kind {
                HirExprKind::ClosureValue { env, .. } => match &env.kind {
                    HirExprKind::Tuple { fields } => owners.extend(fields.iter()),
                    _ => owners.push(env),
                },
                _ => owners.push(a),
            }
        }
        for a in owners {
            if !self.moves_own(a) || self.c.in_unsafe(at) {
                continue;
            }
            self.c.fn_owners.borrow_mut().insert((at.file, at.start));
            // Whatever the value came through (a call returning its
            // argument, an element, an arm), the places it is go along. One
            // an earlier argument of this call took is used twice.
            let mut fl = Vec::new();
            self.flows(a, &mut fl);
            for (_, q) in &fl {
                let shown = place(a).is_some().then_some(a);
                self.use_place(q, &a.ty, shown, a.span);
            }
            let Some(p) = place(a) else {
                if let Some(t) = self
                    .c
                    .src(a.span)
                    .filter(|t| t.len() <= 40 && !t.contains('\n'))
                {
                    self.c
                        .handed
                        .borrow_mut()
                        .insert((at.file, at.start), format!("`{t}`"));
                }
                for (_, q) in fl {
                    self.mark_gone(q, Gone::To(at, What::Task));
                }
                continue;
            };
            if let Some(t) = self
                .c
                .src(a.span)
                .filter(|t| t.len() <= 40 && !t.contains('\n'))
            {
                self.c
                    .handed
                    .borrow_mut()
                    .insert((at.file, at.start), format!("`{t}`"));
            }
            let why = Gone::To(at, What::Task);
            // A `fn` local: only that place (what it owns was followed from
            // the closure that made it). A closure, or a part holding one,
            // goes with its aliases.
            let closure = !p.1.is_empty()
                || matches!(a.ty, HirTy::Closure { .. })
                || matches!(&a.kind, HirExprKind::Local(l)
                    if matches!(self.local_ty(*l), Some(HirTy::Closure { .. })));
            if closure {
                self.mark_gone(p, why);
            } else {
                for s in self.cur.states() {
                    s.gone.retain(|g, _| !within(g, &p));
                    s.gone.insert(p.clone(), why);
                }
            }
        }
    }

    /// An `async` closure's future runs on what the closure moved in: the
    /// two are one owner. Handing either over takes the other along.
    pub(crate) fn closure_future(&mut self, closure: &HirExpr, e: &HirExpr) {
        if !self.is_coroutine(&e.ty) || !self.moves_own(closure) {
            return;
        }
        let mut fl = Vec::new();
        self.flows_any(closure, &mut fl);
        let v = self.results.entry(addr(e)).or_default();
        v.extend(fl.into_iter().map(|(_, q)| (Fk::Both, q)));
    }

    /// What `e`'s value carries, of the places this walk knows.
    pub(crate) fn flows(&self, e: &HirExpr, out: &mut Vec<Flow>) {
        // A literal holding a closure that owns moved values carries it,
        // whatever its own type says.
        let literal = matches!(
            e.kind,
            HirExprKind::Tuple { .. }
                | HirExprKind::StructInit { .. }
                | HirExprKind::ArrayInit { .. }
                | HirExprKind::EnumVariantInit { .. }
        );
        if literal || self.owns_moved(e) || self.c.tracked(&e.ty) || self.moves_own(e) {
            self.flows_any(e, out);
        }
    }

    /// `flows`, also for a `Sync` value whose field is followed (a list's
    /// head holding its buffer).
    pub(crate) fn flows_any(&self, e: &HirExpr, out: &mut Vec<Flow>) {
        if self.shared_read(e) {
            out.push((Fk::Is(Vec::new()), (WORLD, Vec::new())));
            return;
        }
        if let Some(p) = place(e) {
            out.push((Fk::Is(Vec::new()), p));
            return;
        }
        let sub = |s: &Self, i: u32, x: &HirExpr, out: &mut Vec<Flow>| {
            let mut v = Vec::new();
            s.flows(x, &mut v);
            out.extend(v.into_iter().map(|(k, q)| match k {
                Fk::Is(s) => (Fk::Is([&[i][..], &s].concat()), q),
                k => (k, q),
            }));
        };
        let as_kind = |s: &Self, x: &HirExpr, k: &Fk, out: &mut Vec<Flow>| {
            let mut v = Vec::new();
            s.flows(x, &mut v);
            out.extend(v.into_iter().map(|(_, q)| (k.clone(), q)));
        };
        match &e.kind {
            HirExprKind::StructInit { fields, .. } => {
                for (i, x) in fields.iter().enumerate() {
                    #[allow(clippy::cast_possible_truncation)]
                    sub(self, i as u32, x, out);
                }
            }
            HirExprKind::Tuple { fields } => {
                for (i, x) in fields.iter().enumerate() {
                    #[allow(clippy::cast_possible_truncation)]
                    sub(self, i as u32 | TUPLE, x, out);
                }
            }
            HirExprKind::WithField {
                target,
                field_index,
                value,
                ..
            } => {
                self.flows(target, out);
                sub(self, *field_index, value, out);
            }
            HirExprKind::FieldRead {
                target,
                field_index: i,
                ..
            }
            | HirExprKind::TupleField {
                target, index: i, ..
            } => {
                let i = if matches!(e.kind, HirExprKind::TupleField { .. }) {
                    i | TUPLE
                } else {
                    *i
                };
                let mut v = Vec::new();
                self.flows_any(target, &mut v);
                out.extend(project(v, &[i]));
            }
            HirExprKind::SumWrap { value: Some(x), .. } | HirExprKind::Cast { operand: x, .. } => {
                self.flows(x, out);
            }
            // A capture, or an element of a fixed array: held by it.
            // A closure's captures are fields of its value.
            HirExprKind::ClosureValue { env: x, .. } => self.flows(x, out),
            HirExprKind::ArrayIndex { array: x, .. } => {
                as_kind(self, x, &Fk::Both, out);
            }
            HirExprKind::ArrayInit { elements: xs, .. } => {
                xs.iter().for_each(|x| as_kind(self, x, &Fk::Both, out));
            }
            HirExprKind::EnumVariantInit {
                variant_index: v,
                args: xs,
                ..
            } => {
                for (i, x) in xs.iter().enumerate() {
                    #[allow(clippy::cast_possible_truncation)]
                    sub(self, payload(*v, i as u32), x, out);
                }
            }
            HirExprKind::Block(_)
            | HirExprKind::If { .. }
            | HirExprKind::Match { .. }
            | HirExprKind::Loop { .. } => {
                let mut v = Vec::new();
                value_exits(e, &mut v);
                v.into_iter().for_each(|x| self.flows(x, out));
            }
            // An element of the buffer: what was stored into it.
            HirExprKind::BufGet { buf, .. } => {
                let mut hs = Vec::new();
                self.holders(buf, &mut hs);
                out.extend(hs.iter().map(|h| (Fk::Is(Vec::new()), extend(h, &[ELEM]))));
            }
            HirExprKind::Call { .. }
            | HirExprKind::CallIndirect { .. }
            | HirExprKind::CallClosure { .. }
            | HirExprKind::CoroNew { .. } => {
                if let Some(v) = self.results.get(&addr(e)) {
                    out.extend(v.iter().cloned());
                }
            }
            _ => {}
        }
    }

    /// `e` reads a value that is not `Sync` out of a `Sync` object (an
    /// `unsafe impl Sync` type): other threads may reach it too.
    pub(crate) fn shared_read(&self, e: &HirExpr) -> bool {
        // These `Sync` types hand a value over (a channel, a task's
        // result, a `Handoff`, a lock's guard): what comes out is the
        // receiver's alone.
        const OWNERS: &[&str] = &[
            "core::channel::",
            "core::marker::",
            "core::par::",
            "core::sync::",
            "core::task::",
        ];
        if OWNERS.iter().any(|m| self.f.name.starts_with(m)) {
            return false;
        }
        match &e.kind {
            HirExprKind::FieldRead { target: t, .. }
            | HirExprKind::TupleField { target: t, .. }
            | HirExprKind::BufGet { buf: t, .. }
            | HirExprKind::ArrayIndex { array: t, .. } => {
                let shared = !matches!(t.ty, HirTy::Unknown | HirTy::Any)
                    && !self.c.env.has_param(&t.ty)
                    && !self.c.tracked(&t.ty);
                // A buffer is followed whatever its elements; a `Sync` struct
                // holding one is not shared state.
                let buf = matches!(e.ty, HirTy::GcBuf(_));
                (shared && !buf && self.c.tracked(&e.ty)) || self.shared_read(t)
            }
            HirExprKind::SumWrap { value: Some(x), .. } | HirExprKind::Cast { operand: x, .. } => {
                self.shared_read(x)
            }
            HirExprKind::Block(b) if b.stmts.is_empty() => {
                b.trailing.as_ref().is_some_and(|t| self.shared_read(t))
            }
            _ => false,
        }
    }

    /// `p` now holds `e`'s value (`part`: a part of it).
    pub(crate) fn store(&mut self, p: &Place, e: &HirExpr, part: bool) {
        self.assign(p);
        if let HirExprKind::ArrayInit { elements, .. } = &e.kind {
            self.array_dup(p, elements);
        }
        let mut fl = Vec::new();
        self.flows(e, &mut fl);
        self.store_flows(p, &fl, part);
        let mut got = Vec::new();
        self.got_parts(e, &mut Vec::new(), &mut got);
        for (path, g, at) in got {
            self.tie(g, extend(p, &path), at, Tie::Taken);
        }
    }

    /// The parts of `e` (paths into it) that hold a guard's `get()` value:
    /// `e` itself, or fields of the tuple and struct literals it builds.
    pub(crate) fn got_parts(
        &self,
        e: &HirExpr,
        path: &mut Vec<u32>,
        out: &mut Vec<(Vec<u32>, LocalId, Span)>,
    ) {
        if path.len() > 8 {
            return;
        }
        if let Some((g, at)) = self.got_from(e) {
            out.push((path.clone(), g, at));
            return;
        }
        let mut sub = |s: &Self, i: u32, x: &HirExpr, out: &mut Vec<_>| {
            path.push(i);
            s.got_parts(x, path, out);
            path.pop();
        };
        match &e.kind {
            HirExprKind::Tuple { fields } => {
                for (i, x) in fields.iter().enumerate() {
                    #[allow(clippy::cast_possible_truncation)]
                    sub(self, i as u32 | TUPLE, x, out);
                }
            }
            HirExprKind::StructInit { fields, .. } => {
                for (i, x) in fields.iter().enumerate() {
                    #[allow(clippy::cast_possible_truncation)]
                    sub(self, i as u32, x, out);
                }
            }
            HirExprKind::WithField {
                field_index, value, ..
            } => sub(self, *field_index, value, out),
            HirExprKind::SumWrap { value: Some(x), .. } => self.got_parts(x, path, out),
            HirExprKind::Block(b) if b.stmts.is_empty() => {
                if let Some(t) = &b.trailing {
                    self.got_parts(t, path, out);
                }
            }
            _ => {}
        }
    }

    /// `p` now holds a value with flows `fl` (`part`: a part of it).
    pub(crate) fn store_flows(&mut self, p: &Place, fl: &[Flow], part: bool) {
        for s in self.cur.states() {
            // What is gone of the value stays gone in `p` (a call's result
            // that its callee handed over).
            let mut inherit = Vec::new();
            for (k, q) in fl {
                for (g, why) in s.gone.iter().filter(|(g, _)| overlap(g, q)) {
                    match k {
                        Fk::Is(sub) if !part => {
                            let rest = g.1.get(q.1.len()..).unwrap_or_default();
                            inherit.push((extend(p, &[&sub[..], rest].concat()), *why));
                        }
                        Fk::Is(_) | Fk::Both => inherit.push((p.clone(), *why)),
                        Fk::Holds => {}
                    }
                }
            }
            for (k, q) in fl {
                let edge = match k {
                    Fk::Is(sub) if !part => Edge::Same(extend(p, sub), q.clone()),
                    Fk::Is(_) | Fk::Both => Edge::In(p.clone(), q.clone()),
                    Fk::Holds => Edge::Into(p.clone(), q.clone()),
                };
                if kept(&edge) {
                    s.edges.insert(edge);
                }
            }
            for (q, why) in inherit {
                mark_gone(s, q, why);
            }
        }
    }

    /// Place `p` becomes `e`. A field update of `p` itself
    /// (`p.f = v`, lowered to `p = p with f: v`) stores into `p.f`.
    pub(crate) fn assign_into(&mut self, p: Place, e: &'h HirExpr) {
        if let HirExprKind::WithField {
            target,
            field_index,
            value,
            ..
        } = &e.kind
        {
            if place(target).as_ref() == Some(&p) {
                return self.assign_into(extend(&p, &[*field_index]), value);
            }
        }
        let k = self.key(e);
        self.expr_to(e, &k);
        self.note_guard(&p, e);
        let hole = self.local_name(p.0).is_some_and(crate::is_hole_temp);
        if place(e).is_none_or(|q| q.0 != p.0) && !hole {
            self.stream_moved(e, e.span);
        }
        self.store(&p, e, false);
        self.retag(&k, &p);
        // The variable that holds a new lock / `Handoff`: named in hints.
        let mut init = e;
        while let HirExprKind::Block(blk) = &init.kind {
            match (&blk.trailing, blk.stmts.is_empty()) {
                (Some(t), true) => init = t,
                _ => break,
            }
        }
        if let HirExprKind::Call { callee, .. } = &init.kind {
            if matches!(self.c.ops.get(callee), Some(Op::Handoff(_, w)) if *w != What::Channel)
                && p.1.is_empty()
            {
                let owner = (
                    self.name(p.0),
                    self.scopes.last().copied().unwrap_or_default(),
                );
                self.c
                    .owners
                    .borrow_mut()
                    .insert((init.span.file, init.span.start), owner);
            }
        }
    }

    /// The place guard local `g` was copied from, as far as it goes.
    pub(crate) fn guard_source(&self, mut g: LocalId) -> Place {
        let mut path = Vec::new();
        for _ in 0..8 {
            let Some(q) = self.guard_copies.get(&g) else {
                break;
            };
            path = [&q.1[..], &path[..]].concat();
            g = q.0;
        }
        (g, path)
    }

    /// The guard local place `p` is: `p` itself, or a guard a closure
    /// captured.
    pub(crate) fn guard_at(&self, p: &Place) -> Option<LocalId> {
        if p.1.is_empty() {
            return Some(p.0);
        }
        self.guard_parts.get(p).copied()
    }

    /// Note where a guard in `e` lands when `p` becomes `e`.
    pub(crate) fn note_guard(&mut self, p: &Place, e: &HirExpr) {
        if !p.1.is_empty() {
            return;
        }
        // A guard copy, or a closure's capture (a stream it reads).
        let capture = |q: &Place| {
            self.f
                .params
                .iter()
                .any(|x| x.local == q.0 && x.name == "__env")
        };
        if let Some(q) = place(e).filter(|q| self.is_guard(p.0) || capture(q)) {
            self.guard_copies.insert(p.0, q);
            return;
        }
        if let HirExprKind::ClosureValue { env, .. } = &e.kind {
            if let HirExprKind::Tuple { fields } = &env.kind {
                for (i, x) in fields.iter().enumerate() {
                    if let Some((l, path)) = place(x) {
                        if path.is_empty() {
                            #[allow(clippy::cast_possible_truncation)]
                            self.guard_parts.insert(extend(p, &[i as u32 | TUPLE]), l);
                        }
                    }
                }
            }
        }
    }

    pub(crate) fn tie(&mut self, guard: LocalId, p: Place, at: Span, given: Tie) {
        for s in self.cur.states() {
            s.tied
                .entry(guard)
                .or_default()
                .entry(p.clone())
                .or_insert((at, given));
        }
    }

    /// The guard a `g.get()` (or a field of it) reads from, through any
    /// form that yields it (a block, an arm, a loop's `break`), and the
    /// expression that took it out.
    pub(crate) fn got_from(&self, e: &HirExpr) -> Option<(LocalId, Span)> {
        let mut v = Vec::new();
        value_exits(e, &mut v);
        v.into_iter().find_map(|x| match &x.kind {
            HirExprKind::Call { callee, args } if self.c.ops.get(callee) == Some(&Op::Get) => {
                Some((place(args.first()?)?.0, x.span))
            }
            HirExprKind::FieldRead { target: y, .. }
            | HirExprKind::TupleField { target: y, .. }
            | HirExprKind::SumWrap { value: Some(y), .. } => {
                self.got_from(y).map(|(g, _)| (g, x.span))
            }
            _ => None,
        })
    }
}
