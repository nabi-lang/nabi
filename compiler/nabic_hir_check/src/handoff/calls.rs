//! Calls: summaries applied at call sites, aliased arguments, ops.

#[allow(clippy::wildcard_imports)]
use crate::handoff::*;

impl Walk<'_, '_> {
    /// An array literal two of whose elements are one object holds it
    /// twice (`p ++ [ELEM]` in `dup`).
    pub(crate) fn array_dup(&mut self, p: &Place, elements: &[HirExpr]) {
        let fls: Vec<Vec<Place>> = elements
            .iter()
            .map(|x| {
                let mut fl = Vec::new();
                self.flows(x, &mut fl);
                fl.into_iter().map(|(_, q)| q).collect()
            })
            .collect();
        if fls.iter().filter(|f| !f.is_empty()).count() < 2 {
            return;
        }
        for st in self.cur.states() {
            let walks: Vec<(Vec<Place>, Vec<Place>)> = fls
                .iter()
                .map(|f| {
                    let (mut inner, mut stops) = (Vec::new(), Vec::new());
                    for q in f {
                        let (x, y) = reach(st, q, &[]);
                        inner.extend(x);
                        stops.extend(y);
                    }
                    (inner, stops)
                })
                .collect();
            let mut twice = false;
            for (i, (ai, ao)) in walks.iter().enumerate() {
                for (bi, bo) in &walks[i + 1..] {
                    // Two reads of one buffer may be one element.
                    twice |= ai.iter().any(|x| bi.iter().any(|y| overlap(x, y)))
                        || ao.iter().any(|x| bo.iter().any(|y| overlap(x, y)));
                }
            }
            if twice {
                st.dup.insert(extend(p, &[ELEM]));
            }
        }
    }

    /// Handing over `qs`, taken out of a parameter's buffer: the call
    /// hands over that buffer's elements (`Summary::sent_from`).
    pub(crate) fn note_sent_from(&mut self, qs: &[Place]) {
        let mut found = Vec::new();
        for (_, st) in &self.cur.0 {
            let near: Vec<Place> = qs
                .iter()
                .flat_map(|q| {
                    let (mut inner, _) = reach(st, q, &[]);
                    inner.push(q.clone());
                    inner
                })
                .collect();
            for q in &near {
                for e in st.edges.of(q.0) {
                    let Edge::Taken(a, b) = e else { continue };
                    if !overlap(q, a) {
                        continue;
                    }
                    for (sub, x) in same_reach(st, b) {
                        if let (true, Some(i)) = (sub.is_empty(), self.param_index(x.0)) {
                            found.push((i, x.1.clone()));
                        }
                    }
                }
            }
        }
        self.sent_from.extend(found);
    }

    /// How to take an element out of the collection at `p` (named `x`).
    pub(crate) fn take_out(&self, p: &Place, x: &str) -> String {
        let base = match self.place_ty(p) {
            Some(HirTy::Struct(id)) => {
                let structs = self.c.env.structs();
                structs.get(id.0 as usize).map(|s| {
                    let n = s.name.split("__").next().unwrap_or_default();
                    n.split('<')
                        .next()
                        .unwrap_or_default()
                        .rsplit("::")
                        .next()
                        .unwrap_or_default()
                        .to_owned()
                })
            }
            _ => None,
        };
        match base.as_deref() {
            Some("Deque") => format!("`{x}.pop_front()`, `{x}.pop_back()`"),
            Some("Dict") => format!("`{x}.remove(key)`"),
            Some("BinaryHeap") => format!("`{x}.pop()`"),
            _ => format!("`{x}.remove(i)`, `{x}.pop()`"),
        }
    }

    /// `.f.g` for the field steps `rest` below place `p`.
    pub(crate) fn field_path(&self, p: &Place, rest: &[u32]) -> Option<String> {
        let mut t = self.place_ty(p)?;
        let mut out = String::new();
        for &k in rest {
            if k & TUPLE != 0 {
                out.push_str(&format!(".{}", k & !TUPLE));
                let HirTy::Tuple(ts) = t else { return None };
                t = ts.get((k & !TUPLE) as usize)?.clone();
                continue;
            }
            let HirTy::Struct(id) = t else { return None };
            let structs = self.c.env.structs();
            let def = structs.get(id.0 as usize)?;
            // The standard library's fields are private: name what holds them.
            if nabic_core::in_toolchain(&def.name) {
                break;
            }
            let f = def.fields.get(k as usize)?;
            out.push_str(&format!(".{}", f.name));
            t = f.ty.clone();
        }
        Some(out)
    }

    /// The index of the parameter whose local is `l`.
    pub(crate) fn param_index(&self, l: LocalId) -> Option<u32> {
        #[allow(clippy::cast_possible_truncation)]
        self.f
            .params
            .iter()
            .position(|p| p.local == l)
            .map(|i| i as u32)
    }

    /// Callee facts about parameters that are, at this call, parts of this
    /// function's own parameters (`uses`, `sent_from`).
    pub(crate) fn lift_params(&mut self, s: &Summary, args: &[&HirExpr]) {
        let lift = |w: &Self, set: &BTreeSet<Root>| -> Vec<Root> {
            set.iter()
                .filter_map(|(j, sub)| {
                    let q = place(args.get(*j as usize)?)?;
                    let path = [&q.1[..], &sub[..]].concat();
                    // Recursion through a field would grow the path forever.
                    (path.len() <= MAX_DEPTH).then_some(())?;
                    Some((w.param_index(q.0)?, path))
                })
                .collect()
        };
        let (u, f) = (lift(self, &s.uses), lift(self, &s.sent_from));
        self.uses.extend(u);
        self.sent_from.extend(f);
    }

    /// At a return: which parts of `ref` parameters handed over on this
    /// path were given a new value (the caller's variable gets it back).
    pub(crate) fn note_renews(&mut self) {
        let refs: Vec<(u32, LocalId)> = self
            .f
            .params
            .iter()
            .enumerate()
            .filter(|(_, p)| p.by_ref)
            .map(|(i, p)| {
                #[allow(clippy::cast_possible_truncation)]
                (i as u32, p.local)
            })
            .collect();
        for (i, l) in refs {
            let sh = shadow(i as usize);
            for (_, st) in &self.cur.0 {
                for g in st.gone.keys().filter(|g| g.0 == sh) {
                    let mine = (l, g.1.clone());
                    if st.gone.keys().any(|x| overlap(x, &mine)) {
                        self.not_renewed.insert((i, g.1.clone()));
                    } else {
                        self.renewed.insert((i, g.1.clone()));
                    }
                }
            }
        }
    }

    /// A call that hands something over must not get, through another
    /// argument, an object it hands over and then uses (`f(tx, a, a)`,
    /// `h.go(tx, h.items)` when `go` uses `self.items`). Distinct elements
    /// of one collection are taken to be distinct.
    pub(crate) fn aliased_args(
        &mut self,
        s: &Summary,
        args: &[&HirExpr],
        call: &HirExpr,
        name: Option<&str>,
    ) {
        if self.c.in_unsafe(call.span) {
            return;
        }
        let at = |w: &Self, k: usize, sub: &[u32]| -> Vec<Place> {
            let Some(arg) = args.get(k) else {
                return Vec::new();
            };
            let mut fl = Vec::new();
            w.flows(arg, &mut fl);
            fl.into_iter().map(|(_, q)| extend(&q, sub)).collect()
        };
        let handed: Vec<&Root> = s
            .gives
            .keys()
            .chain(s.sent_from.iter())
            .filter(|r| r.0 != RET_ROOT && r.0 != WORLD_ROOT)
            .collect();
        let mut hit: Option<(usize, usize)> = None;
        for (hi, hsub) in handed {
            let hs = at(self, *hi as usize, hsub);
            for (ui, usub) in s.uses.iter().filter(|(u, _)| u != hi) {
                let us = at(self, *ui as usize, usub);
                // Either side reaches the other; the elements of a
                // collection are reached as its element place.
                let shared = self.cur.0.iter().any(|(_, st)| {
                    hs.iter().any(|h| {
                        let (inner, _) = reach(st, h, &[]);
                        let hs_all = if elem_of(h).is_some() {
                            elem_aliases(st, h)
                        } else {
                            vec![h.clone()]
                        };
                        us.iter().any(|u| {
                            let (u_in, _) = reach(st, u, &[]);
                            inner.iter().any(|x| overlap(x, u))
                                || u_in.iter().any(|x| hs_all.iter().any(|y| overlap(x, y)))
                                || (elem_of(h).is_some() && {
                                    // A taken element: anything stored there.
                                    let mut probe = St {
                                        gone: BTreeMap::new(),
                                        tied: BTreeMap::new(),
                                        edges: st.edges.clone(),
                                        dup: BTreeSet::new(),
                                        stored: BTreeMap::new(),
                                    };
                                    mark_gone(
                                        &mut probe,
                                        u.clone(),
                                        Gone::To(Span::DUMMY, What::Mutex),
                                    );
                                    probe
                                        .gone
                                        .keys()
                                        .any(|x| hs_all.iter().any(|y| overlap(x, y)))
                                })
                        })
                    })
                });
                if shared {
                    hit = Some((*hi as usize, *ui as usize));
                }
            }
        }
        let Some((given, used)) = hit else { return };
        let show = |w: &Self, k: usize| {
            w.c.src(args[k].span)
                .filter(|t| t.len() <= 30 && !t.contains('\n'))
                .map_or_else(|| format!("argument {}", k + 1), |t| format!("`{t}`"))
        };
        let (a, b) = (show(self, given), show(self, used));
        let callee = name.map_or_else(|| "the call".to_owned(), |n| format!("`{n}`"));
        let head = if a == b {
            format!(
                "{a} is passed twice to {callee}, which hands one over and keeps using the other"
            )
        } else {
            format!(
                "{a} and {b} share an object, and {callee} hands over the one and keeps using \
                 the other"
            )
        };
        self.diags.push(
            NabiDiagnostic::error(Code::SHARED_HANDOFF, call.span, head)
                .with_note("help: pass separate values (`.clone()` one of them)".to_owned()),
        );
    }

    /// `par::filter`'s result holds items of argument 0; `par::map`'s
    /// does when the function may return its argument.
    pub(crate) fn par_result(&mut self, callee: FunctionId, args: &[&HirExpr], e: &HirExpr) {
        let Some(f) = self.c.fns.get(&callee) else {
            return;
        };
        if !self.c.tracked(&e.ty) {
            return;
        }
        let filter = is_fn(&f.name, "core::par::filter");
        let map = is_fn(&f.name, "core::par::map");
        if !filter && !map {
            return;
        }
        if map {
            let returns_arg = match args.get(1).map(|a| &a.kind) {
                Some(HirExprKind::ClosureValue { fn_id, .. }) => {
                    self.callee(*fn_id).is_some_and(|s| {
                        s.edges.iter().any(|e| {
                            let (SEdge::Same(a, b) | SEdge::Into(a, b) | SEdge::Taken(a, b)) = e;
                            (a.0 == RET_ROOT && b.0 == 1) || (b.0 == RET_ROOT && a.0 == 1)
                        })
                    })
                }
                Some(HirExprKind::FnRef(id)) => self.callee(*id).is_some_and(|s| {
                    s.edges.iter().any(|e| {
                        let (SEdge::Same(a, b) | SEdge::Into(a, b) | SEdge::Taken(a, b)) = e;
                        (a.0 == RET_ROOT && b.0 == 0) || (b.0 == RET_ROOT && a.0 == 0)
                    })
                }),
                _ => true,
            };
            if !returns_arg {
                return;
            }
        }
        let Some(items) = args.first() else { return };
        let mut fl = Vec::new();
        self.flows(items, &mut fl);
        if fl.is_empty() {
            return;
        }
        let key = addr(e);
        let t = if let Some((_, q)) = self.results.get(&key).and_then(|v| v.first()) {
            q.clone()
        } else {
            let t = (self.temp(key, RET_ROOT, &e.ty), Vec::new());
            self.assign(&t);
            self.results
                .insert(key, vec![(Fk::Is(Vec::new()), t.clone())]);
            t
        };
        let at = extend(&t, &[0, ELEM]);
        for st in self.cur.states() {
            for (_, q) in &fl {
                st.edges
                    .insert(Edge::Same(at.clone(), extend(q, &[0, ELEM])));
            }
        }
    }

    /// `dst.extend(src)`: `dst` holds an object twice if it already
    /// holds one of `src`'s, or `src` is `dst`.
    pub(crate) fn copy_dup(&mut self, args: &[&HirExpr]) {
        let (Some(dst), Some(src)) = (args.first().and_then(|a| place(a)), args.get(1)) else {
            return;
        };
        let mut fl = Vec::new();
        self.flows(src, &mut fl);
        for st in self.cur.states() {
            let mut probe = St {
                gone: BTreeMap::new(),
                tied: BTreeMap::new(),
                edges: st.edges.clone(),
                dup: BTreeSet::new(),
                stored: BTreeMap::new(),
            };
            for (_, q) in &fl {
                mark_gone(&mut probe, q.clone(), Gone::To(Span::DUMMY, What::Mutex));
            }
            let held = probe
                .gone
                .keys()
                .any(|g| within(g, &dst) && g.1[dst.1.len()..].contains(&ELEM));
            if held || fl.iter().any(|(_, q)| overlap(q, &dst)) {
                st.dup.insert(extend(&dst, &[0, ELEM]));
            }
        }
    }

    /// Every element of the buffer at `p` was dropped (`clear`): what
    /// was handed over or stored there is no longer in it.
    pub(crate) fn cleared(&mut self, p: &Place) {
        for st in self.cur.states() {
            let aliases: Vec<Place> = same_reach(st, p)
                .into_iter()
                .filter(|(sub, _)| sub.is_empty())
                .map(|(_, a)| a)
                .collect();
            let inside = |q: &Place| {
                aliases
                    .iter()
                    .any(|a| within(q, a) && q.1[a.1.len()..].contains(&ELEM))
            };
            st.gone.retain(|g, _| !inside(g));
            st.dup.retain(|d| !inside(d));
            let locals: BTreeSet<LocalId> = aliases.iter().map(|a| a.0).collect();
            for l in locals {
                for e in st.edges.take_of(l) {
                    let (a, b) = e.ends();
                    if !inside(a) && !inside(b) {
                        st.edges.insert(e);
                    }
                }
            }
        }
    }

    /// A collection literal built from an array that holds one object
    /// twice holds it twice too.
    pub(crate) fn literal_dup(&mut self, s: &Summary, e: &HirExpr) {
        let key = addr(e);
        let Some(&ret) = self.temps.get(&(key, RET_ROOT)) else {
            return;
        };
        let elems: BTreeSet<Place> = s
            .edges
            .iter()
            .flat_map(|e| {
                let (SEdge::Same(a, b) | SEdge::Into(a, b) | SEdge::Taken(a, b)) = e;
                [a, b]
            })
            .filter(|r| r.0 == RET_ROOT)
            .filter_map(|r| elem_of(&(ret, r.1.clone())))
            .collect();
        let args: Vec<LocalId> = (0..8)
            .filter_map(|i| self.temps.get(&(key, i)).copied())
            .collect();
        for st in self.cur.states() {
            let mut probe = St {
                gone: BTreeMap::new(),
                tied: BTreeMap::new(),
                edges: st.edges.clone(),
                dup: BTreeSet::new(),
                stored: BTreeMap::new(),
            };
            for a in &args {
                mark_gone(
                    &mut probe,
                    (*a, Vec::new()),
                    Gone::To(Span::DUMMY, What::Mutex),
                );
            }
            let twice = probe.gone.keys().any(|g| st.dup.iter().any(|d| d.0 == g.0));
            if twice {
                st.dup.extend(elems.iter().cloned());
            }
        }
    }

    pub(crate) fn callee(&mut self, id: FunctionId) -> Option<Rc<Summary>> {
        self.deps.insert(id);
        self.c.summary(id).filter(|s| !s.is_empty())
    }

    /// A call through a function value with these arguments, of type
    /// `ret`: every function used as a value with their types.
    pub(crate) fn indirect(&mut self, args: &[HirExpr], ret: &HirTy) -> Option<Rc<Summary>> {
        let c = self.c;
        let cands = c.cands.get(&args.len())?;
        let ret = ret_key(c.env, ret);
        let mut out = Summary::default();
        for k in cands {
            let fits = k.keys.iter().zip(args).all(|(key, a)| {
                matches!(a.ty, HirTy::Unknown | HirTy::Any) || *key == a.ty.stable_key()
            }) && (k.ret.is_none() || ret.is_none() || k.ret == ret);
            if !fits {
                continue;
            }
            let s = match &k.from {
                CandFrom::Fn(id) => {
                    self.deps.insert(*id);
                    match c.summary(*id) {
                        Some(s) => s,
                        None => continue,
                    }
                }
                CandFrom::Stored(s) => s.clone(),
            };
            let env = k.env;
            out.merge(&s.remap(&|i| Some(if env { i } else { i + 1 })));
        }
        (!out.is_empty()).then(|| Rc::new(out))
    }

    /// Run summary `s` at call `e` with these arguments (`ret`: its
    /// value is the function's return value).
    pub(crate) fn apply(
        &mut self,
        s: &Summary,
        args: &[&HirExpr],
        e: &HirExpr,
        ret: bool,
        takes: bool,
    ) {
        self.lift_params(s, args);
        let key = addr(e);
        let used: BTreeSet<u32> = s.roots().map(|r| r.0).collect();
        // Each parameter's object: a local holding the argument.
        let mut slot: HashMap<u32, Place> = HashMap::default();
        for (i, a) in args.iter().enumerate() {
            #[allow(clippy::cast_possible_truncation)]
            let i = i as u32;
            if !used.contains(&i) || !self.c.tracked(&a.ty) {
                continue;
            }
            let t = (self.temp(key, i, &a.ty), Vec::new());
            self.store(&t, a, false);
            slot.insert(i, t);
        }
        if ret && used.contains(&RET_ROOT) && self.c.tracked(&e.ty) {
            let t = (self.temp(key, RET_ROOT, &e.ty), Vec::new());
            self.assign(&t);
            self.results
                .insert(key, vec![(Fk::Is(Vec::new()), t.clone())]);
            slot.insert(RET_ROOT, t);
        }
        if used.contains(&WORLD_ROOT) {
            slot.insert(WORLD_ROOT, (WORLD, Vec::new()));
        }
        let map = |r: &Root| slot.get(&r.0).map(|p| extend(p, &r.1));
        let (add, stores) = summary_edges(s, &map, takes);
        let trusted = TRUSTED.iter().any(|m| self.f.name.starts_with(m));
        for st in self.cur.states() {
            let mut dup = Vec::new();
            let mut al = Aliases::new(st);
            for (at, v) in &stores {
                if st.dup.contains(at) || dup.contains(at) {
                    continue;
                }
                // A copy from a buffer that may hold one object twice.
                let from_dup = elem_of(v).is_some() && al.is_dup(v);
                if from_dup || stores_twice(&mut al, at, v, trusted) {
                    dup.push(at.clone());
                }
            }
            drop(al);
            st.dup.extend(dup);
            for (at, _) in &stores {
                st.stored.entry(at.clone()).or_default().insert(e.span);
            }
            for edge in &add {
                if kept(edge) {
                    st.edges.insert(edge.clone());
                }
            }
            st.dup.extend(s.dups.iter().filter_map(&map));
        }
        for (g, r) in &s.ties {
            let guard = args
                .get(g.0 as usize)
                .and_then(|a| place(a))
                .and_then(|p| self.guard_at(&extend(&p, &g.1)));
            if let (Some(guard), Some(p)) = (guard, map(r)) {
                self.tie(guard, p, e.span, Tie::Taken);
            }
        }
        let given: Vec<(&Root, Place, Gone)> = s
            .gives
            .iter()
            .filter_map(|(r, why)| {
                // A spawned closure goes along only when it may own moved
                // values (as for a direct spawn).
                let arg = args.get(r.0 as usize).filter(|_| r.1.is_empty());
                if matches!(why.what, What::Task | What::Closure)
                    && arg.is_some_and(|a| {
                        matches!(a.ty, HirTy::Fn(..) | HirTy::Closure { .. }) && !self.moves_own(a)
                    })
                {
                    return None;
                }
                let id = self.c.why_id(why.clone());
                Some((r, map(r)?, Gone::Via(e.span, id)))
            })
            .collect();
        // Apart by how the call ends, when its value's tag is followed.
        let dest = self.dest_of(e);
        let (Some(dest), false) = (dest, s.exits.is_empty()) else {
            for (_, p, why) in given {
                self.mark_gone(p, why);
            }
            self.renew(s, args, &slot);
            return;
        };
        let none = BTreeSet::new();
        let any = s.exits.get(&ANY).unwrap_or(&none);
        let mut out = Cur::default();
        for (t, st) in std::mem::take(&mut self.cur.0) {
            for (tag, roots) in &s.exits {
                let (mut t, mut st) = (t.clone(), st.clone());
                if *tag != ANY {
                    t.insert(dest.clone(), *tag);
                }
                for (r, p, why) in &given {
                    if roots.contains(*r) || *tag != ANY && any.contains(*r) {
                        mark_gone(&mut st, p.clone(), *why);
                    }
                }
                out.0.push((t, st));
            }
        }
        out.norm();
        self.cur = out;
        self.renew(s, args, &slot);
    }

    /// A `ref` parameter part the callee handed over and then replaced:
    /// the argument gets the new value back, its other aliases stay gone.
    pub(crate) fn renew(&mut self, s: &Summary, args: &[&HirExpr], slot: &HashMap<u32, Place>) {
        for (i, sub) in &s.renews {
            let mut ps: Vec<Place> = slot.get(i).map(|t| extend(t, sub)).into_iter().collect();
            if let Some(q) = args.get(*i as usize).and_then(|a| place(a)) {
                ps.push(extend(&q, sub));
            }
            // The value copied back out to the caller (`ref`): part of the
            // call's value.
            if let Some(ret) = slot.get(&RET_ROOT) {
                for e in &s.edges {
                    let SEdge::Same(x, y) = e else { continue };
                    for (r, q) in [(x, y), (y, x)] {
                        if r.0 == RET_ROOT && q.0 == *i && sub.starts_with(&q.1) {
                            let rest = &sub[q.1.len()..];
                            ps.push(extend(ret, &[&r.1[..], rest].concat()));
                        }
                    }
                }
            }
            let mut cur = std::mem::take(&mut self.cur);
            for (_, st) in &mut cur.0 {
                for q in &ps {
                    self.regain(st, q);
                }
            }
            self.cur = cur;
        }
    }

    /// Where the tag of call `e`'s value goes.
    pub(crate) fn dest_of(&self, e: &HirExpr) -> Option<Place> {
        let a = addr(e);
        self.dests
            .iter()
            .rev()
            .find(|(x, _)| *x == a)
            .map(|(_, p)| p.clone())
    }

    /// `e` polled future `fut` (`poll(ref self, ref ctx)`): its value
    /// is `(step, fut, ctx)`, and the step is no error if `fut`'s body
    /// ends with none.
    pub(crate) fn polled(&mut self, e: &HirExpr, fut: Option<&HirExpr>) {
        let (Some(dest), Some(f)) = (self.dest_of(e), fut.and_then(tag_place)) else {
            return;
        };
        for (t, _) in &mut self.cur.0 {
            if let Some(&v) = t.get(&f) {
                t.insert(extend(&dest, &[1 | TUPLE]), v);
                if v != ERR {
                    t.insert(extend(&dest, &[TUPLE]), OK);
                }
            }
        }
    }

    pub(crate) fn temp(&mut self, key: usize, slot: u32, ty: &HirTy) -> LocalId {
        let n = self.temps.len();
        let l = *self.temps.entry((key, slot)).or_insert_with(|| {
            #[allow(clippy::cast_possible_truncation)]
            LocalId(TEMP + n as u32)
        });
        self.temp_tys.insert(l, ty.clone());
        l
    }

    /// A lock op or a hand-off.
    pub(crate) fn op(&mut self, op: Op, args: &[HirExpr], at: Span) {
        let carried = |w: &Self, i: usize| {
            let mut fl = Vec::new();
            if let Some(a) = args.get(i) {
                w.flows(a, &mut fl);
            }
            fl.into_iter().map(|(_, q)| q).collect::<Vec<_>>()
        };
        let guard = args.first().and_then(place).map(|p| p.0);
        match op {
            Op::Handoff(i, what) => {
                // A spawned closure shares its plain captures (they are
                // `Sync`); only one that owns a `[move x]` goes along.
                if what == What::Task
                    && args.get(i).is_some_and(|a| {
                        matches!(a.ty, HirTy::Fn(..) | HirTy::Closure { .. }) && !self.moves_own(a)
                    })
                {
                    return;
                }
                // A `fn` value moved into a closure: only that place goes;
                // what it owns was followed from the closure that made it.
                if let Some(a) = args
                    .get(i)
                    .filter(|a| what == What::Closure && matches!(a.ty, HirTy::Fn(..)))
                {
                    if let Some(p) = place(a).filter(|_| self.owns_moved(a)) {
                        for s in self.cur.states() {
                            s.gone.retain(|g, _| !within(g, &p));
                            s.gone.insert(p.clone(), Gone::To(at, what));
                        }
                    }
                    return;
                }
                self.sends |= what != What::Handoff;
                if what == What::Task && args.get(i).is_some_and(|a| self.moves_own(a)) {
                    self.c.fn_owners.borrow_mut().insert((at.file, at.start));
                }
                if let Some(a) = args.get(i).filter(|a| place(a).is_some()) {
                    let shown = self.show(a);
                    self.c
                        .direct
                        .borrow_mut()
                        .insert((at.file, at.start), (shown, a.ty.stable_key()));
                }
                if let Some(t) = args
                    .get(i)
                    .and_then(|a| self.c.src(a.span))
                    .filter(|t| t.len() <= 40 && !t.contains('\n'))
                {
                    self.c
                        .handed
                        .borrow_mut()
                        .insert((at.file, at.start), format!("`{t}`"));
                }
                let qs = carried(self, i);
                self.note_sent_from(&qs);
                if what != What::Handoff && !self.c.in_unsafe(at) {
                    self.shared_handoff(&qs, args.get(i), what);
                    self.guarded_handoff(&qs, args.get(i), what);
                }
                for q in qs {
                    self.mark_gone(q, Gone::To(at, what));
                }
            }
            // The lock now guards the new value, not what `get` returned.
            Op::Set => {
                let Some(g) = guard else { return };
                for s in self.cur.states() {
                    s.tied.remove(&g);
                }
                for q in carried(self, 1) {
                    self.tie(g, q, at, Tie::Given);
                }
            }
            // The closure's captures may receive the guarded value or
            // hand theirs to the lock.
            Op::Update => {
                let Some(g) = guard else { return };
                for q in carried(self, 1) {
                    self.tie(g, q, at, Tie::Shared);
                }
            }
            // What it protects is unprotected now, and so is every copy of
            // the guard itself (pushed, captured, stored by a callee).
            Op::Drop => {
                let Some(g) = guard else { return };
                let guards: HashSet<LocalId> = self
                    .f
                    .locals
                    .iter()
                    .map(|l| l.id)
                    .filter(|&l| self.is_guard(l))
                    .collect();
                for s in self.cur.states() {
                    let tied = s.tied.remove(&g).unwrap_or_default();
                    let at_tie = tied.values().next().map(|(sp, _)| *sp);
                    let before: BTreeSet<Place> = s.gone.keys().cloned().collect();
                    for (q, (span, tie)) in tied {
                        mark_gone(s, q, Gone::Guard(span, g, tie));
                    }
                    // What holds a gone value one way (a coroutine frame
                    // of an `async` / `yield` fn's arguments) goes too.
                    if let Some(span) = at_tie {
                        holders_go(s, &before, Gone::Guard(span, g, Tie::Shared));
                    }
                    mark_gone_whole(s, (g, Vec::new()), Gone::Guard(at, g, Tie::Itself), &guards);
                }
            }
            Op::Get => {}
        }
    }
}
