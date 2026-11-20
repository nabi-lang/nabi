//! Function summaries, value flows and what a body gives its callers.

#[allow(clippy::wildcard_imports)]
use crate::handoff::*;

/// A parameter (`RET_ROOT`: the return value) and a path into it.
pub(crate) type Root = (u32, Vec<u32>);

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum SEdge {
    Same(Root, Root),
    /// The first going away takes the second along.
    Into(Root, Root),
    /// The first was taken out of the buffer elements the second is.
    Taken(Root, Root),
}

/// What a function does with its parameters, as its callers see it.
#[derive(Clone, Debug, Default)]
pub(crate) struct Summary {
    /// The parts of parameters the call hands over.
    pub(crate) gives: BTreeMap<Root, Why>,
    pub(crate) edges: BTreeSet<SEdge>,
    /// `(guard, root)`: tied to the guard that parameter part is (a
    /// closure's captured guard is a part of it).
    pub(crate) ties: BTreeSet<(Root, Root)>,
    /// Exit tag (of the return value; `ANY`: not known) -> the parts of
    /// `gives` handed over on those exits. Empty: every exit hands over
    /// all of them.
    pub(crate) exits: BTreeMap<u32, BTreeSet<Root>>,
    /// Buffer elements that may now hold one object twice.
    pub(crate) dups: BTreeSet<Root>,
    /// The call may hand something over (to a channel, a lock).
    pub(crate) sends: bool,
    /// Parameter parts the body uses.
    pub(crate) uses: BTreeSet<Root>,
    /// Buffer elements of parameters whose taken-out elements the call
    /// hands over.
    pub(crate) sent_from: BTreeSet<Root>,
    /// Parts of `ref` parameters handed over and then given a new value on
    /// every path that hands them over: the caller's variable is usable.
    pub(crate) renews: BTreeSet<Root>,
    /// Parameters the call keeps past it: stored into another parameter
    /// (a field, a `Cell`, a capture) or into what other threads reach.
    pub(crate) keeps: BTreeSet<u32>,
}

impl Summary {
    pub(crate) fn is_empty(&self) -> bool {
        self.gives.is_empty()
            && self.edges.is_empty()
            && self.ties.is_empty()
            && self.dups.is_empty()
            && !self.sends
            && self.uses.is_empty()
            && self.renews.is_empty()
            && self.keeps.is_empty()
    }

    /// The same facts (which hand-off is named does not matter).
    pub(crate) fn same_facts(&self, o: &Self) -> bool {
        self.gives.keys().eq(o.gives.keys())
            && self.edges == o.edges
            && self.ties == o.ties
            && self.exits == o.exits
            && self.dups == o.dups
            && self.sends == o.sends
            && self.uses == o.uses
            && self.sent_from == o.sent_from
            && self.renews == o.renews
            && self.keeps == o.keeps
    }

    /// `exits`, with every exit kind spelled out.
    pub(crate) fn exit_kinds(&self) -> BTreeMap<u32, BTreeSet<Root>> {
        if self.exits.is_empty() {
            BTreeMap::from([(ANY, self.gives.keys().cloned().collect())])
        } else {
            self.exits.clone()
        }
    }

    /// Drop `exits` when no exit of a known tag hands over less.
    pub(crate) fn trim(&mut self) {
        let any = self.exits.get(&ANY).cloned().unwrap_or_default();
        let less = self
            .exits
            .iter()
            .any(|(t, r)| *t != ANY && r.union(&any).count() < self.gives.len());
        if !less {
            self.exits.clear();
        }
    }

    pub(crate) fn roots(&self) -> impl Iterator<Item = &Root> {
        self.gives
            .keys()
            .chain(self.edges.iter().flat_map(|e| {
                let (SEdge::Same(a, b) | SEdge::Into(a, b) | SEdge::Taken(a, b)) = e;
                [a, b]
            }))
            .chain(self.ties.iter().map(|(_, r)| r))
            .chain(self.dups.iter())
    }

    /// Parameter `i` of `self` as parameter `map(i)` (`None`: dropped).
    pub(crate) fn remap(&self, map: &dyn Fn(u32) -> Option<u32>) -> Self {
        let root = |r: &Root| {
            if r.0 == RET_ROOT || r.0 == WORLD_ROOT {
                Some(r.clone())
            } else {
                map(r.0).map(|i| (i, r.1.clone()))
            }
        };
        Self {
            gives: self
                .gives
                .iter()
                .filter_map(|(r, w)| Some((root(r)?, w.clone())))
                .collect(),
            edges: self
                .edges
                .iter()
                .filter_map(|e| match e {
                    SEdge::Same(a, b) => Some(SEdge::Same(root(a)?, root(b)?)),
                    SEdge::Into(a, b) => Some(SEdge::Into(root(a)?, root(b)?)),
                    SEdge::Taken(a, b) => Some(SEdge::Taken(root(a)?, root(b)?)),
                })
                .collect(),
            ties: self
                .ties
                .iter()
                .filter_map(|(g, r)| Some((root(g)?, root(r)?)))
                .collect(),
            exits: self
                .exits
                .iter()
                .map(|(t, rs)| (*t, rs.iter().filter_map(root).collect()))
                .collect(),
            dups: self.dups.iter().filter_map(root).collect(),
            sends: self.sends,
            uses: self.uses.iter().filter_map(root).collect(),
            sent_from: self.sent_from.iter().filter_map(root).collect(),
            renews: self.renews.iter().filter_map(root).collect(),
            keeps: self.keeps.iter().filter_map(|&i| map(i)).collect(),
        }
    }

    pub(crate) fn merge(&mut self, o: &Self) {
        let mut exits = self.exit_kinds();
        for (t, rs) in o.exit_kinds() {
            exits.entry(t).or_default().extend(rs);
        }
        for (r, w) in &o.gives {
            self.gives.entry(r.clone()).or_insert_with(|| w.clone());
        }
        self.edges.extend(o.edges.iter().cloned());
        self.ties.extend(o.ties.iter().cloned());
        self.dups.extend(o.dups.iter().cloned());
        self.sends |= o.sends;
        self.uses.extend(o.uses.iter().cloned());
        self.sent_from.extend(o.sent_from.iter().cloned());
        self.keeps.extend(o.keeps.iter().copied());
        // A part renewed by every candidate.
        self.renews = self.renews.intersection(&o.renews).cloned().collect();
        self.exits = exits;
        self.trim();
    }

    /// One line per fact, fields split by tabs (NABIHIR).
    pub(crate) fn encode(&self) -> String {
        let root = |r: &Root| {
            let head = match r.0 {
                RET_ROOT => "r".to_owned(),
                WORLD_ROOT => "w".to_owned(),
                i => i.to_string(),
            };
            std::iter::once(head)
                .chain(r.1.iter().map(u32::to_string))
                .collect::<Vec<_>>()
                .join(".")
        };
        let mut out = String::new();
        for (r, w) in &self.gives {
            out.push_str(&format!(
                "g\t{}\t{}\t{}\t{}\t{}\t{}\n",
                root(r),
                w.what.tag(),
                w.line,
                w.func,
                w.file,
                u8::from(w.lib)
            ));
        }
        for e in &self.edges {
            let (k, a, b) = match e {
                SEdge::Same(a, b) => ("s", a, b),
                SEdge::Into(a, b) => ("i", a, b),
                SEdge::Taken(a, b) => ("k", a, b),
            };
            out.push_str(&format!("{k}\t{}\t{}\n", root(a), root(b)));
        }
        for (g, r) in &self.ties {
            out.push_str(&format!("t\t{}\t{}\n", root(g), root(r)));
        }
        for r in &self.dups {
            out.push_str(&format!("d\t{}\n", root(r)));
        }
        if self.sends {
            out.push_str("h\n");
        }
        for i in &self.keeps {
            out.push_str(&format!("p\t{i}\n"));
        }
        for (k, set) in [
            ("u", &self.uses),
            ("f", &self.sent_from),
            ("n", &self.renews),
        ] {
            for r in set {
                out.push_str(&format!("{k}\t{}\n", root(r)));
            }
        }
        for (t, rs) in &self.exits {
            let t = if *t == ANY {
                "*".to_owned()
            } else {
                t.to_string()
            };
            if rs.is_empty() {
                out.push_str(&format!("x\t{t}\n"));
            }
            for r in rs {
                out.push_str(&format!("x\t{t}\t{}\n", root(r)));
            }
        }
        out
    }

    pub(crate) fn decode(text: &str) -> Option<Self> {
        let root = |s: &str| -> Option<Root> {
            let mut it = s.split('.');
            let head = it.next()?;
            let i = match head {
                "r" => RET_ROOT,
                "w" => WORLD_ROOT,
                _ => head.parse().ok()?,
            };
            Some((i, it.map(str::parse).collect::<Result<_, _>>().ok()?))
        };
        let mut s = Self::default();
        for line in text.lines() {
            let f: Vec<&str> = line.split('\t').collect();
            match f[..] {
                ["g", r, what, l, func, file, lib] => {
                    s.gives.insert(
                        root(r)?,
                        Why {
                            what: What::from_tag(what)?,
                            func: func.to_owned(),
                            file: file.to_owned(),
                            line: l.parse().ok()?,
                            lib: lib == "1",
                        },
                    );
                }
                ["s", a, b] => {
                    s.edges.insert(SEdge::Same(root(a)?, root(b)?));
                }
                ["i", a, b] => {
                    s.edges.insert(SEdge::Into(root(a)?, root(b)?));
                }
                ["k", a, b] => {
                    s.edges.insert(SEdge::Taken(root(a)?, root(b)?));
                }
                ["d", r] => {
                    s.dups.insert(root(r)?);
                }
                ["h"] => s.sends = true,
                ["p", i] => {
                    s.keeps.insert(i.parse().ok()?);
                }
                ["u", r] => {
                    s.uses.insert(root(r)?);
                }
                ["f", r] => {
                    s.sent_from.insert(root(r)?);
                }
                ["n", r] => {
                    s.renews.insert(root(r)?);
                }
                ["t", g, r] => {
                    s.ties.insert((root(g)?, root(r)?));
                }
                ["x", t, ref rest @ ..] if rest.len() <= 1 => {
                    let t = if t == "*" { ANY } else { t.parse().ok()? };
                    let rs = s.exits.entry(t).or_default();
                    if let [r] = rest {
                        rs.insert(root(r)?);
                    }
                }
                _ => return None,
            }
        }
        Some(s)
    }
}

/// The name prefix of a NABIHIR entry for the functions used as values
/// with these parameter types (`e`: a closure, its environment first),
/// then `RET_KEY` and the return type when it is one `ret_key` keeps.
pub(crate) const FN_VALUE: &str = "#fn";

pub(crate) const RET_KEY: char = '\u{1e}';

/// A function used as a value: a call through a function value of its
/// parameter and return types may reach it.
pub(crate) struct Cand {
    /// A closure: parameter 0 is its environment.
    pub(crate) env: bool,
    pub(crate) keys: Vec<String>,
    /// `None`: any call of these parameter types may reach it.
    pub(crate) ret: Option<String>,
    pub(crate) from: CandFrom,
}

/// The key a call through a function value matches on its return type.
/// `None` for a type a function's own return type may differ from in
/// form: unknown or generic, a sum, a function or closure type.
pub(crate) fn ret_key(env: &dyn Facts, t: &HirTy) -> Option<String> {
    let loose = matches!(
        t,
        HirTy::Unknown | HirTy::Any | HirTy::Sum(_) | HirTy::Fn(..) | HirTy::Closure { .. }
    ) || env.has_param(t);
    (!loose).then(|| t.stable_key())
}

pub(crate) enum CandFrom {
    Fn(FunctionId),
    Stored(Rc<Summary>),
}

/// How a value flows when stored at a place `P`.
#[derive(Clone)]
pub(crate) enum Fk {
    /// `P ++ sub` is this object.
    Is(Vec<u32>),
    /// Held inside `P`, both going away together.
    Both,
    /// Held inside `P`: it goes away with `P`.
    Holds,
}

pub(crate) type Flow = (Fk, Place);

/// The flows of the part at `path` of a value with flows `fl`.
pub(crate) fn project(fl: Vec<Flow>, path: &[u32]) -> Vec<Flow> {
    let mut fl = fl;
    for &i in path {
        let mut out = Vec::new();
        for (k, q) in fl {
            match k {
                Fk::Is(s) if s.is_empty() => out.push((Fk::Is(s), extend(&q, &[i]))),
                Fk::Is(s) if s[0] == i => out.push((Fk::Is(s[1..].to_vec()), q)),
                Fk::Is(_) => {}
                k => out.push((k, q)),
            }
        }
        fl = out;
    }
    fl
}

/// Each local `p` binds with its path inside the matched value (`None`:
/// somewhere in it). A sum's member is the sum's place.
pub(crate) fn binding_paths(
    p: &HirPattern,
    at: Option<&[u32]>,
    out: &mut Vec<(LocalId, Option<Vec<u32>>)>,
) {
    let below = |k: u32| at.map(|a| [a, &[k]].concat());
    match p {
        HirPattern::Binding { local, .. }
        | HirPattern::SumVariant {
            binder: Some(local),
            ..
        } => out.push((*local, at.map(<[u32]>::to_vec))),
        HirPattern::At { local, sub, .. } => {
            out.push((*local, at.map(<[u32]>::to_vec)));
            binding_paths(sub, at, out);
        }
        HirPattern::SumMember { sub, .. } => binding_paths(sub, at, out),
        HirPattern::Tuple(ps) => {
            for (i, x) in ps.iter().enumerate() {
                #[allow(clippy::cast_possible_truncation)]
                binding_paths(x, below(i as u32 | TUPLE).as_deref(), out);
            }
        }
        HirPattern::Struct { fields, .. } => {
            for (i, x) in fields {
                binding_paths(x, below(*i).as_deref(), out);
            }
        }
        HirPattern::EnumVariant {
            variant_index,
            args,
            ..
        } => {
            for (i, x) in args.iter().enumerate() {
                #[allow(clippy::cast_possible_truncation)]
                binding_paths(x, below(payload(*variant_index, i as u32)).as_deref(), out);
            }
        }
        HirPattern::Or(alts) => {
            for x in alts {
                binding_paths(x, None, out);
            }
        }
        _ => p.each_binding(&mut |l| out.push((l, None))),
    }
}

/// The place `e` reads as is (`x`, `x.f.0`, `x` wrapped into a sum
/// slot or cast).
pub(crate) fn place(e: &HirExpr) -> Option<Place> {
    match &e.kind {
        HirExprKind::Local(l) => Some((*l, Vec::new())),
        HirExprKind::FieldRead {
            target,
            field_index,
            ..
        } => place(target).map(|p| extend(&p, &[*field_index])),
        HirExprKind::TupleField { target, index, .. } => {
            place(target).map(|p| extend(&p, &[index | TUPLE]))
        }
        HirExprKind::SumWrap { value: Some(x), .. } | HirExprKind::Cast { operand: x, .. } => {
            place(x)
        }
        HirExprKind::Block(b) if b.stmts.is_empty() => place(b.trailing.as_ref()?),
        _ => None,
    }
}

/// The expressions whose values `e` yields: a block's trailing value and
/// the `break` values that leave it, every `if` / `match` arm, a loop's
/// `break` values. Others yield their own.
pub(crate) fn value_exits<'e>(e: &'e HirExpr, out: &mut Vec<&'e HirExpr>) {
    match &e.kind {
        HirExprKind::Block(b) => block_exits(b, out),
        HirExprKind::If {
            then_block,
            else_branch,
            ..
        } => {
            block_exits(then_block, out);
            if let Some(x) = else_branch {
                value_exits(x, out);
            }
        }
        HirExprKind::Match { arms, .. } => arms.iter().for_each(|a| value_exits(&a.body, out)),
        HirExprKind::Loop { label, body, .. } => {
            let mut v = Vec::new();
            breaks_to(body, label.as_deref(), true, false, &mut v);
            v.into_iter().for_each(|x| value_exits(x, out));
        }
        _ => out.push(e),
    }
}

pub(crate) fn block_exits<'e>(b: &'e HirBlock, out: &mut Vec<&'e HirExpr>) {
    if let Some(l) = b.label.as_deref() {
        let mut v = Vec::new();
        breaks_to(b, Some(l), false, false, &mut v);
        v.into_iter().for_each(|x| value_exits(x, out));
    }
    if let Some(t) = &b.trailing {
        value_exits(t, out);
    }
}

/// The values of the `break`s in `b` that leave the loop or block
/// `label` (`is_loop`: an unlabelled one too, outside inner loops).
pub(crate) fn breaks_to<'e>(
    b: &'e HirBlock,
    label: Option<&str>,
    is_loop: bool,
    inner: bool,
    out: &mut Vec<&'e HirExpr>,
) {
    fn ex<'e>(
        e: &'e HirExpr,
        label: Option<&str>,
        is_loop: bool,
        inner: bool,
        out: &mut Vec<&'e HirExpr>,
    ) {
        match &e.kind {
            HirExprKind::Loop { body, .. } => breaks_to(body, label, is_loop, true, out),
            HirExprKind::While { cond, body, .. } => {
                ex(cond, label, is_loop, inner, out);
                breaks_to(body, label, is_loop, true, out);
            }
            HirExprKind::Match { scrutinee, arms } => {
                ex(scrutinee, label, is_loop, inner, out);
                for a in arms {
                    if let Some(g) = &a.guard {
                        ex(g, label, is_loop, inner, out);
                    }
                    ex(&a.body, label, is_loop, inner, out);
                }
            }
            _ => each_child(e, &mut |c| match c {
                Child::E(x) => ex(x, label, is_loop, inner, out),
                Child::B(b) => breaks_to(b, label, is_loop, inner, out),
            }),
        }
    }
    for s in &b.stmts {
        match s {
            HirStmt::Break { label: l, value } => {
                let hit = match l.as_deref() {
                    Some(l) => label == Some(l),
                    None => is_loop && !inner,
                };
                if let Some(v) = value {
                    if hit {
                        out.push(v);
                    } else {
                        ex(v, label, is_loop, inner, out);
                    }
                }
            }
            HirStmt::Let { init: e, .. }
            | HirStmt::Assign { value: e, .. }
            | HirStmt::StaticAssign { value: e, .. }
            | HirStmt::Expr(e)
            | HirStmt::Panic(e)
            | HirStmt::Return(Some(e)) => ex(e, label, is_loop, inner, out),
            HirStmt::ArrayAssign { index, value, .. } => {
                ex(index, label, is_loop, inner, out);
                ex(value, label, is_loop, inner, out);
            }
            HirStmt::Return(None) | HirStmt::Continue { .. } => {}
        }
    }
    if let Some(t) = &b.trailing {
        ex(t, label, is_loop, inner, out);
    }
}

impl Walk<'_, '_> {
    /// What `exit` has handed over of the parameters.
    pub(crate) fn gives(&self, exit: &St) -> BTreeMap<Root, Why> {
        let mut out = BTreeMap::new();
        let root_of = |l: LocalId| -> Option<u32> {
            if l == RET {
                Some(RET_ROOT)
            } else if (SHADOW..TEMP).contains(&l.0) {
                Some(l.0 - SHADOW)
            } else {
                None
            }
        };
        let me = shown_fn(self.c, self.f);
        let lib = in_lib(self.c, self.f);
        for (q, g) in &exit.gone {
            let Some(r) = root_of(q.0).filter(|&r| r != RET_ROOT) else {
                continue;
            };
            // A `Handoff` protocol is the `unsafe` code's: it ends before
            // the function that made it returns.
            if matches!(g, Gone::To(_, What::Handoff)) {
                continue;
            }
            let why = match *g {
                Gone::To(at, what) => Why {
                    what,
                    func: me.clone(),
                    file: self.c.file_name(at.file),
                    line: at.line,
                    lib,
                },
                Gone::Via(_, id) => self.c.whys.borrow().0[id as usize].clone(),
                Gone::Guard(at, guard, _) => Why {
                    what: self.guard_what(guard),
                    func: me.clone(),
                    file: self.c.file_name(at.file),
                    line: at.line,
                    lib,
                },
            };
            out.insert((r, q.1.clone()), why);
        }
        out
    }

    /// The walk's exit state as its callers see it.
    pub(crate) fn summarize(&self) -> Summary {
        let mut s = Summary {
            sends: self.sends,
            uses: self.uses.clone(),
            sent_from: self.sent_from.clone(),
            renews: self
                .renewed
                .difference(&self.not_renewed)
                .cloned()
                .collect(),
            ..Summary::default()
        };
        let Some(exit) = self.exits.values().cloned().reduce(join_st) else {
            return s;
        };
        let exit = &exit;
        let mut ends = SameEnds::new(exit);
        s.gives = self.gives(exit);
        if !self.mixed && self.exits.keys().any(|&t| t != ANY) {
            s.exits = self
                .exits
                .iter()
                .map(|(t, st)| (*t, self.gives(st).into_keys().collect()))
                .collect();
            s.trim();
        }
        let root_of = |l: LocalId| -> Option<u32> {
            if l == RET {
                Some(RET_ROOT)
            } else if l == WORLD {
                Some(WORLD_ROOT)
            } else if (SHADOW..TEMP).contains(&l.0) {
                Some(l.0 - SHADOW)
            } else {
                None
            }
        };
        // One walk per place: a recursive value's exit has thousands of
        // `Taken` ends over the same places.
        let mut roots_of: HashMap<Place, Rc<Vec<Root>>> = HashMap::default();
        let mut to_roots = |p: &Place| -> Rc<Vec<Root>> {
            roots_of
                .entry(p.clone())
                .or_insert_with(|| {
                    Rc::new(
                        ends.reach(p)
                            .into_iter()
                            .filter(|(sub, _)| sub.is_empty())
                            .filter_map(|(_, x)| Some((root_of(x.0)?, x.1)))
                            .collect(),
                    )
                })
                .clone()
        };
        for e in exit.edges.0.values().flatten() {
            if let Edge::Taken(a, b) = e {
                let (ras, rbs) = (to_roots(a), to_roots(b));
                for ra in ras.iter() {
                    for rb in rbs.iter() {
                        s.edges.insert(SEdge::Taken(ra.clone(), rb.clone()));
                    }
                }
            }
        }
        for d in &exit.dup {
            s.dups.extend(to_roots(d).iter().cloned());
        }
        let mut roots: Vec<(u32, Place)> = self
            .f
            .params
            .iter()
            .enumerate()
            .filter(|(_, p)| self.c.tracked(&p.ty))
            .map(|(i, _)| {
                #[allow(clippy::cast_possible_truncation)]
                (i as u32, (shadow(i), Vec::new()))
            })
            .collect();
        if self.c.tracked(&self.f.ret_ty) {
            roots.push((RET_ROOT, (RET, Vec::new())));
        }
        roots.push((WORLD_ROOT, (WORLD, Vec::new())));
        for (r, start) in &roots {
            let same = ends.reach(start);
            for (sub, q) in &same {
                if let Some(r2) = root_of(q.0) {
                    if (r2, &q.1) != (*r, sub) {
                        let (a, b) = ((*r, sub.clone()), (r2, q.1.clone()));
                        s.edges
                            .insert(SEdge::Same(a.clone().min(b.clone()), a.max(b)));
                    }
                }
            }
            let mut probe = St {
                gone: BTreeMap::new(),
                tied: BTreeMap::new(),
                edges: exit.edges.clone(),
                dup: exit.dup.clone(),
                stored: BTreeMap::new(),
            };
            mark_gone(
                &mut probe,
                start.clone(),
                Gone::To(Span::DUMMY, What::Mutex),
            );
            for q in probe.gone.keys() {
                let Some(r2) = root_of(q.0).filter(|r2| r2 != r) else {
                    continue;
                };
                let implied = same.iter().any(|(_, p)| within(q, p));
                if !implied {
                    s.edges
                        .insert(SEdge::Into((*r, Vec::new()), (r2, q.1.clone())));
                }
            }
        }
        for (r, start) in &roots {
            if *r != RET_ROOT && *r != WORLD_ROOT && keeps_param(&mut ends, start, *r, &root_of) {
                s.keeps.insert(*r);
            }
        }
        for (g, tied) in &exit.tied {
            let (gl, gpath) = self.guard_source(*g);
            let Some(i) = self.f.params.iter().position(|p| p.local == gl) else {
                continue;
            };
            // A stream taken by value is this function's own: an owning
            // adapter holds it beside its view.
            if !self.f.params[i].by_ref && gpath.is_empty() && self.stream_view(*g) {
                continue;
            }
            let mut probe = St {
                gone: BTreeMap::new(),
                tied: BTreeMap::new(),
                edges: exit.edges.clone(),
                dup: exit.dup.clone(),
                stored: BTreeMap::new(),
            };
            for q in tied.keys() {
                mark_gone(&mut probe, q.clone(), Gone::To(Span::DUMMY, What::Mutex));
            }
            for q in probe.gone.keys() {
                if let Some(r) = root_of(q.0) {
                    #[allow(clippy::cast_possible_truncation)]
                    s.ties.insert(((i as u32, gpath.clone()), (r, q.1.clone())));
                }
            }
        }
        s
    }
}
