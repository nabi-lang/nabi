//! Places, alias edges, tags and the per-path flow state.

#[allow(clippy::wildcard_imports)]
use crate::handoff::*;

/// A local and a field path into it.
pub(crate) type Place = (LocalId, Vec<u32>);

/// `a` is `b` or a place inside it.
pub(crate) fn within(a: &Place, b: &Place) -> bool {
    a.0 == b.0 && a.1.starts_with(&b.1)
}

/// One of `a`, `b` is inside the other: they share memory.
pub(crate) fn overlap(a: &Place, b: &Place) -> bool {
    within(a, b) || within(b, a)
}

pub(crate) fn extend(p: &Place, rest: &[u32]) -> Place {
    (p.0, [&p.1[..], rest].concat())
}

/// Where a hand-off inside a callee happened, as its callers report it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Why {
    pub(crate) what: What,
    /// The function that made the hand-off, as written (`Bag::store`).
    pub(crate) func: String,
    pub(crate) file: String,
    pub(crate) line: u32,
    /// Inside the standard library: its names are not the user's to see.
    pub(crate) lib: bool,
}

/// Why a place is gone.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Gone {
    /// Handed over at this call.
    To(Span, What),
    /// Handed over by this call (a `Why` id).
    Via(Span, u32),
    /// Tied to this guard at this span; the guard was dropped.
    Guard(Span, LocalId, Tie),
}

/// How a place is tied to a guard.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Tie {
    /// It holds what the guard gave out (`g.get()`).
    Taken,
    /// The guard holds it (`g.set(v)`).
    Given,
    /// It shares one of those (a copy, a part, what was stored in it).
    Shared,
    /// It is the guard (a copy of `g` stored or captured).
    Itself,
    /// A view of a stream that was then moved (`Walk::moved_at`).
    Moved,
}

impl Gone {
    pub(crate) const fn origin(self) -> Span {
        match self {
            Self::To(s, _) | Self::Via(s, _) | Self::Guard(s, ..) => s,
        }
    }
}

/// An alias: `Same` places are one object; `In(a, b)` holds one inside
/// the other (both go away together); `Into(a, b)`: `b` goes away with
/// `a`, not the reverse; `Taken(a, b)`: `a` was taken out of the buffer
/// elements `b`, so it may be what was stored there, but `b` no longer
/// holds it (unless the buffer may hold an object twice).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Edge {
    Same(Place, Place),
    In(Place, Place),
    Into(Place, Place),
    Taken(Place, Place),
}

impl Edge {
    pub(crate) const fn ends(&self) -> (&Place, &Place) {
        let (Self::Same(a, b) | Self::In(a, b) | Self::Into(a, b) | Self::Taken(a, b)) = self;
        (a, b)
    }

    pub(crate) fn touches(&self, p: &Place) -> bool {
        let (a, b) = self.ends();
        within(a, p) || within(b, p)
    }
}

/// The edges, under the local of each end.
#[derive(Clone, Default, PartialEq)]
pub(crate) struct Edges(pub(crate) BTreeMap<LocalId, BTreeSet<Edge>>);

impl Edges {
    pub(crate) fn of(&self, l: LocalId) -> impl Iterator<Item = &Edge> {
        self.0.get(&l).into_iter().flatten()
    }

    pub(crate) fn insert(&mut self, e: Edge) {
        let (a, b) = (e.ends().0.0, e.ends().1.0);
        if a != b {
            self.0.entry(b).or_default().insert(e.clone());
        }
        self.0.entry(a).or_default().insert(e);
    }

    pub(crate) fn extend(&mut self, o: Self) {
        for (l, es) in o.0 {
            self.0.entry(l).or_default().extend(es);
        }
    }

    /// Remove and return every edge with an end on `l`.
    pub(crate) fn take_of(&mut self, l: LocalId) -> Vec<Edge> {
        let mine: Vec<Edge> = self.0.remove(&l).into_iter().flatten().collect();
        for e in &mine {
            let (a, b) = (e.ends().0.0, e.ends().1.0);
            let other = if a == l { b } else { a };
            if let Some(es) = self.0.get_mut(&other) {
                es.remove(e);
                if es.is_empty() {
                    self.0.remove(&other);
                }
            }
        }
        mine
    }
}

#[derive(Clone, Default, PartialEq)]
pub(crate) struct St {
    pub(crate) gone: BTreeMap<Place, Gone>,
    /// Guard local -> the places holding what it protects.
    pub(crate) tied: BTreeMap<LocalId, BTreeMap<Place, (Span, Tie)>>,
    pub(crate) edges: Edges,
    /// Buffers (their `ELEM` places) that may hold one object twice.
    pub(crate) dup: BTreeSet<Place>,
    /// Where values were stored into buffer elements (for messages).
    pub(crate) stored: BTreeMap<Place, BTreeSet<Span>>,
}

/// An edge worth keeping: between two places, of followed depth.
pub(crate) fn kept(e: &Edge) -> bool {
    let (a, b) = e.ends();
    a != b && a.1.len() <= MAX_DEPTH && b.1.len() <= MAX_DEPTH
}

pub(crate) fn join_st(mut a: St, b: St) -> St {
    for (l, g) in b.gone {
        a.gone.entry(l).or_insert(g);
    }
    for (g, ls) in b.tied {
        let t = a.tied.entry(g).or_default();
        for (l, s) in ls {
            t.entry(l).or_insert(s);
        }
    }
    a.edges.extend(b.edges);
    a.dup.extend(b.dup);
    for (p, sp) in b.stored {
        a.stored.entry(p).or_default().extend(sp);
    }
    a
}

/// What a path knows of the value at a place: a `bool`, a sum or enum
/// variant, a carrier's (`ERR`, `OK`, `OK_TAG` + its value's), or a
/// `TryAsyncFuture`'s (that of the carrier its body ends with).
pub(crate) type Tags = BTreeMap<Place, u32>;

/// An exit whose tag is not known (`Summary::exits`).
pub(crate) const ANY: u32 = u32::MAX;

/// Paths kept apart at one point; past it they are one.
pub(crate) const MAX_PATHS: usize = 8;

/// Loop passes with paths kept apart; then they are one.
pub(crate) const MAX_SPLIT_PASSES: usize = 8;

/// The paths reaching a point, apart by the tags they know (empty:
/// unreachable). Paths with the same tags are one, and so are paths
/// with the same places gone and tied (their aliases joined): only
/// those tell paths apart later.
#[derive(Clone, Default, PartialEq)]
pub(crate) struct Cur(pub(crate) Vec<(Tags, St)>);

impl Cur {
    pub(crate) fn start() -> Self {
        Self(vec![(Tags::new(), St::default())])
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(crate) fn states(&mut self) -> impl Iterator<Item = &mut St> {
        self.0.iter_mut().map(|(_, s)| s)
    }

    pub(crate) fn norm(&mut self) {
        loop {
            let mut out: Vec<(Tags, St)> = Vec::with_capacity(self.0.len());
            let mut merged = false;
            for (t, s) in std::mem::take(&mut self.0) {
                match out
                    .iter_mut()
                    .find(|(u, v)| *u == t || v.gone.keys().eq(s.gone.keys()) && v.tied == s.tied)
                {
                    Some(o) => {
                        if o.0 != t {
                            o.0.retain(|k, v| t.get(k) == Some(v));
                        }
                        if o.1 != s {
                            o.1 = join_st(std::mem::take(&mut o.1), s);
                        }
                        merged = true;
                    }
                    None => out.push((t, s)),
                }
            }
            self.0 = out;
            if !merged {
                break;
            }
        }
        if self.0.len() > MAX_PATHS {
            self.flatten();
        }
        self.0.sort_by(|a, b| a.0.cmp(&b.0));
    }

    /// One path: the tags all know, the states joined.
    pub(crate) fn flatten(&mut self) {
        let mut it = std::mem::take(&mut self.0).into_iter();
        if let Some((mut t, mut s)) = it.next() {
            for (u, v) in it {
                t.retain(|k, x| u.get(k) == Some(x));
                s = join_st(s, v);
            }
            self.0.push((t, s));
        }
    }

    pub(crate) fn untag(&mut self, k: &Place) {
        let mut hit = false;
        for (t, _) in &mut self.0 {
            let n = t.len();
            t.retain(|q, _| !overlap(q, k));
            hit |= t.len() != n;
        }
        if hit {
            self.norm();
        }
    }
}

pub(crate) fn join(a: Cur, b: Cur) -> Cur {
    if a.is_empty() {
        return b;
    }
    if b.is_empty() {
        return a;
    }
    let mut c = a;
    c.0.extend(b.0);
    c.norm();
    c
}

/// The place `e` names exactly (`x`, `x.f.0`).
pub(crate) fn tag_place(e: &HirExpr) -> Option<Place> {
    match &e.kind {
        HirExprKind::Local(l) => Some((*l, Vec::new())),
        HirExprKind::FieldRead {
            target,
            field_index,
            ..
        } => tag_place(target).map(|p| extend(&p, &[*field_index])),
        HirExprKind::TupleField { target, index, .. } => {
            tag_place(target).map(|p| extend(&p, &[index | TUPLE]))
        }
        _ => None,
    }
}

/// How a value leaves the body (`escapes_guard`).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Exit {
    Return,
    Yield,
    Throw,
}

/// The error a returned carrier `(true, .., err)` throws.
pub(crate) fn thrown(e: &HirExpr) -> Option<&HirExpr> {
    let HirExprKind::Tuple { fields } = &e.kind else {
        return None;
    };
    match (&e.ty, fields.first().map(|f| &f.kind)) {
        (HirTy::Throws { .. }, Some(HirExprKind::Const(ConstVal::Bool(true)))) => fields.last(),
        _ => None,
    }
}

/// A carrier's tag: an error, a value of unknown tag, or `OK_TAG` plus
/// the value's tag.
pub(crate) const OK: u32 = 0;

pub(crate) const ERR: u32 = 1;

pub(crate) const OK_TAG: u32 = 2;

/// `ty` is a carrier with a value slot (`throws -> T`, `T` not `()`).
pub(crate) const fn ok_slot(ty: &HirTy) -> bool {
    matches!(ty, HirTy::Throws { ok, .. } if !matches!(**ok, HirTy::Unit | HirTy::Never))
}

/// The tag of `e`'s value on a path that knows `t`.
pub(crate) fn tag_of(e: &HirExpr, t: &Tags) -> Option<u32> {
    match &e.kind {
        HirExprKind::Const(ConstVal::Bool(b)) => Some(u32::from(*b)),
        HirExprKind::SumWrap {
            sum_ty,
            variant_index,
            ..
        } if is_sum(sum_ty) && *sum_ty == e.ty => Some(*variant_index),
        HirExprKind::EnumVariantInit { variant_index, .. } => Some(*variant_index),
        HirExprKind::Tuple { fields } if matches!(e.ty, HirTy::Throws { .. }) => {
            match (tag_of(fields.first()?, t)?, ok_slot(&e.ty)) {
                (1, _) => Some(ERR),
                (_, true) => Some(tag_of(&fields[1], t).map_or(OK, |v| v.saturating_add(OK_TAG))),
                (_, false) => Some(OK),
            }
        }
        HirExprKind::PrimUnaryOp {
            op: PrimOp::LogicalNot,
            operand,
        } => tag_of(operand, t).map(|b| b ^ 1),
        // A carrier's `is_err`, and its value.
        HirExprKind::TupleField {
            target, index: 0, ..
        } if matches!(target.ty, HirTy::Throws { .. }) => {
            t.get(&tag_place(target)?).map(|&v| u32::from(v == ERR))
        }
        HirExprKind::TupleField {
            target, index: 1, ..
        } if ok_slot(&target.ty) => t
            .get(&tag_place(target)?)
            .and_then(|&v| v.checked_sub(OK_TAG)),
        HirExprKind::Block(b) if b.stmts.is_empty() => tag_of(b.trailing.as_ref()?, t),
        _ => t.get(&tag_place(e)?).copied(),
    }
}

pub(crate) const fn is_sum(ty: &HirTy) -> bool {
    matches!(ty, HirTy::Sum(_))
}

/// Pattern `p` may match a value of type `ty` with tag `v`.
pub(crate) fn may_match(p: &HirPattern, v: u32, ty: &HirTy) -> bool {
    match p {
        HirPattern::Literal {
            value: ConstVal::Bool(b),
            ..
        } if *ty == HirTy::Prim(PrimTy::Bool) => u32::from(*b) == v,
        HirPattern::SumVariant {
            sum_ty,
            variant_index,
            ..
        }
        | HirPattern::SumMember {
            sum_ty,
            variant_index,
            ..
        } if is_sum(ty) && sum_ty == ty => *variant_index == v,
        HirPattern::EnumVariant {
            enum_id,
            variant_index,
            ..
        } if *ty == HirTy::Enum(*enum_id) => *variant_index == v,
        HirPattern::At { sub, .. } => may_match(sub, v, ty),
        HirPattern::Or(alts) => alts.iter().any(|a| may_match(a, v, ty)),
        _ => true,
    }
}

/// Pattern `p` matches every value of type `ty` with tag `v`.
pub(crate) fn catches(p: &HirPattern, v: u32, ty: &HirTy) -> bool {
    match p {
        HirPattern::Wildcard | HirPattern::Binding { .. } => true,
        HirPattern::Literal {
            value: ConstVal::Bool(b),
            ..
        } => *ty == HirTy::Prim(PrimTy::Bool) && u32::from(*b) == v,
        HirPattern::SumVariant {
            sum_ty,
            variant_index,
            ..
        } => is_sum(ty) && sum_ty == ty && *variant_index == v,
        HirPattern::SumMember {
            sum_ty,
            variant_index,
            sub,
            ..
        } => is_sum(ty) && sum_ty == ty && *variant_index == v && sub.is_irrefutable(),
        HirPattern::EnumVariant {
            enum_id,
            variant_index,
            args,
        } => {
            *ty == HirTy::Enum(*enum_id)
                && *variant_index == v
                && args.iter().all(HirPattern::is_irrefutable)
        }
        HirPattern::At { sub, .. } => catches(sub, v, ty),
        HirPattern::Or(alts) => alts.iter().any(|a| catches(a, v, ty)),
        _ => false,
    }
}

/// `p` and every place aliased to it go away in `s`.
pub(crate) fn mark_gone(s: &mut St, p: Place, why: Gone) {
    let first = p.clone();
    // Each place with the buffer elements it must not reach: what was
    // taken out of a buffer is not in it any more.
    let mut work: Vec<(Place, Rc<Vec<Place>>)> = vec![(p, Rc::new(Vec::new()))];
    while let Some((q, blocked)) = work.pop() {
        // What a guard gave out: its aliases share it.
        let why = match why {
            Gone::Guard(at, g, Tie::Taken) if q != first => Gone::Guard(at, g, Tie::Shared),
            w => w,
        };
        if q.1.len() > MAX_DEPTH || s.gone.keys().any(|g| within(&q, g)) {
            continue;
        }
        let mut next: Vec<(Place, Rc<Vec<Place>>)> = step(s, &q)
            .into_iter()
            .map(|x| (x, blocked.clone()))
            .collect();
        for e in s.edges.of(q.0) {
            let Edge::Taken(a, b) = e else { continue };
            if !overlap(&q, a) {
                continue;
            }
            if is_dup(s, b) {
                next.push((b.clone(), blocked.clone()));
                continue;
            }
            // What was stored into the buffer, not the buffer.
            let elems = elem_aliases(s, b);
            let mut bl = (*blocked).clone();
            bl.extend(elems.iter().cloned());
            let bl = Rc::new(bl);
            for h in &elems {
                for x in step(s, h) {
                    if !elems.iter().any(|h| overlap(&x, h)) {
                        next.push((x, bl.clone()));
                    }
                }
            }
        }
        for (x, bl) in next {
            if !bl.iter().any(|b| overlap(&x, b)) {
                work.push((x, bl));
                continue;
            }
            // A part of the buffer's elements: a value taken out of it
            // holds the same part.
            let Some(h) = bl.iter().find(|b| within(&x, b)) else {
                continue;
            };
            let rest = &x.1[h.1.len()..];
            for es in s.edges.0.values() {
                for e in es {
                    if let Edge::Taken(t, b) = e {
                        if elem_aliases(s, b).contains(h) {
                            work.push((extend(t, rest), bl.clone()));
                        }
                    }
                }
            }
        }
        s.gone.retain(|g, _| !within(g, &q));
        s.gone.insert(q, why);
    }
}

/// What holds `p` and what `p` was taken out of (`t = o` with
/// `o = xs.pop()`, `o` forgotten): the holder was taken out of it too, not
/// tied to the buffer, which no longer holds it. Each end: (other end,
/// holds or is `p`, taken: 1 `p` out of it, 2 it out of `p`).
pub(crate) fn taken_through(
    one: bool,
    a: (&Place, bool, u8),
    b: (&Place, bool, u8),
) -> Option<Edge> {
    if !one {
        return None;
    }
    let (x1, both1, tk1) = a;
    let (x2, both2, tk2) = b;
    match (both1, tk1, both2, tk2) {
        (true, _, _, 1) | (_, 2, true, _) => Some(Edge::Taken(x1.clone(), x2.clone())),
        (true, _, _, 2) | (_, 1, true, _) => Some(Edge::Taken(x2.clone(), x1.clone())),
        _ => None,
    }
}

/// `p` and every copy of it or place holding it go away, not what it
/// holds (a dropped guard: not its lock). Another guard variable goes only
/// as a direct copy of `p` (`using h = g`): reached through a slot that may
/// hold either (`if c { (a, b) } else { (b, a) }`) it is its own guard, but
/// what holds it may hold `p` and goes.
pub(crate) fn mark_gone_whole(st: &mut St, place: Place, why: Gone, guards: &HashSet<LocalId>) {
    let first = place.clone();
    let mut seen: HashSet<Place> = HashSet::default();
    // (place, a direct copy of `place`)
    let mut work = vec![(place, true)];
    while let Some((q, direct)) = work.pop() {
        if q.1.len() > MAX_DEPTH || st.gone.keys().any(|g| within(&q, g)) || !seen.insert(q.clone())
        {
            continue;
        }
        for e in st.edges.of(q.0) {
            match e {
                Edge::Same(one, two) => {
                    for (other, end) in [(one, two), (two, one)] {
                        if within(&q, end) {
                            let copy = extend(other, &q.1[end.1.len()..]);
                            let whole = direct && q.1.is_empty() && copy.1.is_empty();
                            work.push((copy, whole));
                        }
                    }
                }
                // A coroutine frame holds its arguments one way (`Into`).
                Edge::In(holder, held) | Edge::Into(holder, held) if within(&q, held) => {
                    work.push((holder.clone(), false));
                }
                _ => {}
            }
        }
        if q != first && !direct && q.1.is_empty() && guards.contains(&q.0) {
            continue;
        }
        st.gone.retain(|g, _| !within(g, &q));
        st.gone.insert(q, why);
    }
}

/// Every `a` of an `Into(a, b)` whose `b` went away since `before` goes
/// too, to a fixpoint.
pub(crate) fn holders_go(st: &mut St, before: &BTreeSet<Place>, why: Gone) {
    loop {
        let mut more = Vec::new();
        for es in st.edges.0.values() {
            for e in es {
                if let Edge::Into(holder, held) = e {
                    let new =
                        |x: &Place| st.gone.keys().any(|g| !before.contains(g) && within(x, g));
                    let gone = |x: &Place| st.gone.keys().any(|g| within(x, g));
                    if new(held) && !gone(holder) {
                        more.push(holder.clone());
                    }
                }
            }
        }
        if more.is_empty() {
            return;
        }
        for h in more {
            mark_gone(st, h, why);
        }
    }
}

/// The places that go away with `q` through one edge (not `Taken`).
pub(crate) fn step(s: &St, q: &Place) -> Vec<Place> {
    let mut out = Vec::new();
    for e in s.edges.of(q.0) {
        match e {
            Edge::Same(a, b) => {
                for (x, y) in [(a, b), (b, a)] {
                    if within(q, y) {
                        out.push(extend(x, &q.1[y.1.len()..]));
                    } else if within(y, q) {
                        out.push(x.clone());
                    }
                }
            }
            Edge::In(a, b) => {
                if overlap(q, b) {
                    out.push(a.clone());
                }
                if overlap(q, a) {
                    out.push(b.clone());
                }
            }
            Edge::Into(a, b) => {
                if overlap(q, a) {
                    out.push(b.clone());
                }
            }
            Edge::Taken(..) => {}
        }
    }
    out
}

/// The buffer elements `at` now hold the values flowing in `fl`
/// (`check`: a store through a call; inside a collection's own methods
/// elements move between slots of one buffer).
pub(crate) fn hold_st(s: &mut St, at: &Place, fl: &[Flow], check: bool) {
    if fl.is_empty() {
        return;
    }
    if check {
        let mut probe = St {
            gone: BTreeMap::new(),
            tied: BTreeMap::new(),
            edges: s.edges.clone(),
            dup: BTreeSet::new(),
            stored: BTreeMap::new(),
        };
        mark_gone(&mut probe, at.clone(), Gone::To(Span::DUMMY, What::Mutex));
        if fl
            .iter()
            .any(|(_, q)| probe.gone.keys().any(|g| overlap(g, q)))
        {
            s.dup.insert(at.clone());
        }
    }
    for (k, q) in fl {
        if within(q, at) {
            continue;
        }
        let e = match k {
            Fk::Is(sub) => Edge::Same(extend(at, sub), q.clone()),
            Fk::Both | Fk::Holds => Edge::In(at.clone(), q.clone()),
        };
        if kept(&e) {
            s.edges.insert(e);
        }
    }
}

/// The `ELEM` places of every alias of the buffer whose elements `b`
/// is (`b` itself among them).
pub(crate) fn elem_aliases(s: &St, b: &Place) -> Vec<Place> {
    Aliases::new(s).elems(b).to_vec()
}

/// [`elem_aliases`] and [`is_dup`] asked of one state many times (a
/// call storing into many buffers): the `Same` walk and each buffer's
/// aliases found once.
pub(crate) struct Aliases<'s> {
    pub(crate) s: &'s St,
    pub(crate) ends: SameEnds<'s>,
    pub(crate) memo: HashMap<Place, Vec<Place>>,
}

impl<'s> Aliases<'s> {
    pub(crate) fn new(s: &'s St) -> Self {
        Self {
            s,
            ends: SameEnds::new(s),
            memo: HashMap::default(),
        }
    }

    /// [`elem_aliases`] on this state.
    pub(crate) fn elems(&mut self, b: &Place) -> &[Place] {
        if !self.memo.contains_key(b) {
            let out = match elem_of(b) {
                None => vec![b.clone()],
                Some(e) => {
                    let holder = (e.0, e.1[..e.1.len() - 1].to_vec());
                    let mut out: Vec<Place> = self
                        .ends
                        .reach(&holder)
                        .into_iter()
                        .filter(|(sub, _)| sub.is_empty())
                        .map(|(_, h)| extend(&h, &[ELEM]))
                        .collect();
                    if !out.contains(&e) {
                        out.push(e);
                    }
                    out
                }
            };
            self.memo.insert(b.clone(), out);
        }
        &self.memo[b]
    }

    /// [`is_dup`] on this state.
    pub(crate) fn is_dup(&mut self, b: &Place) -> bool {
        let s = self.s;
        !s.dup.is_empty()
            && self
                .elems(b)
                .iter()
                .any(|x| s.dup.iter().any(|d| within(x, d)))
    }
}

/// Summary `s`'s edges at a call (`map`: a root to the caller's place),
/// and its stores into a buffer: (its elements, the stored value).
/// `takes`: the value is an element taken out of argument 0's buffer.
pub(crate) fn summary_edges(
    s: &Summary,
    map: &dyn Fn(&Root) -> Option<Place>,
    takes: bool,
) -> (Vec<Edge>, Vec<(Place, Place)>) {
    let mut add: Vec<Edge> = Vec::new();
    let mut stores: Vec<(Place, Place)> = Vec::new();
    for edge in &s.edges {
        let mapped = match edge {
            SEdge::Same(x, y) if takes && (x.0 == RET_ROOT) != (y.0 == RET_ROOT) => {
                let (ret, from) = if x.0 == RET_ROOT { (x, y) } else { (y, x) };
                match (map(ret), map(from).and_then(|p| elem_of(&p))) {
                    (Some(ret), Some(from)) => Some(Edge::Taken(ret, from)),
                    _ => map(x).zip(map(y)).map(|(x, y)| Edge::Same(x, y)),
                }
            }
            SEdge::Same(x, y) => map(x).zip(map(y)).map(|(x, y)| Edge::Same(x, y)),
            SEdge::Into(x, y) => map(x).zip(map(y)).map(|(x, y)| Edge::Into(x, y)),
            SEdge::Taken(x, y) => map(x).zip(map(y)).map(|(x, y)| Edge::Taken(x, y)),
        };
        let Some(mapped) = mapped.filter(|m| m.ends().0 != m.ends().1) else {
            continue;
        };
        if let (SEdge::Same(rx, ry), Edge::Same(x, y)) = (edge, &mapped) {
            if rx.0 != RET_ROOT && ry.0 != RET_ROOT && rx.0 != ry.0 {
                match (elem_of(x), elem_of(y)) {
                    (Some(at), _) => stores.push((at, y.clone())),
                    (None, Some(at)) => stores.push((at, x.clone())),
                    (None, None) => {}
                }
            }
        }
        add.push(mapped);
    }
    (add, stores)
}

/// The places aliased to `start`, not walking past the elements of a
/// buffer outside `own` (those are returned apart): elements read out
/// of one buffer are taken to be distinct objects unless that buffer
/// may hold one twice.
pub(crate) fn reach(s: &St, start: &Place, own: &[Place]) -> (Vec<Place>, Vec<Place>) {
    let (mut inner, mut stops) = (Vec::new(), Vec::new());
    let mut seen: HashSet<Place> = HashSet::default();
    let mut work = vec![start.clone()];
    while let Some(q) = work.pop() {
        if q.1.len() > MAX_DEPTH || !seen.insert(q.clone()) {
            continue;
        }
        let foreign = q.1.contains(&ELEM) && !own.iter().any(|h| within(&q, h));
        if foreign && q != *start {
            stops.push(q);
            continue;
        }
        for e in s.edges.of(q.0) {
            match e {
                Edge::Same(a, b) => {
                    for (x, y) in [(a, b), (b, a)] {
                        if within(&q, y) {
                            work.push(extend(x, &q.1[y.1.len()..]));
                        } else if within(y, &q) {
                            work.push(x.clone());
                        }
                    }
                }
                Edge::In(a, b) => {
                    if overlap(&q, b) {
                        work.push(a.clone());
                    }
                    if overlap(&q, a) {
                        work.push(b.clone());
                    }
                }
                Edge::Into(a, b) | Edge::Taken(a, b) => {
                    if overlap(&q, a) {
                        work.push(b.clone());
                    }
                }
            }
        }
        inner.push(q);
    }
    (inner, stops)
}

/// Storing `v` into the buffer elements `at` makes the buffer hold one
/// object twice: `v` is already among them, or both come from the
/// elements of one other buffer (elements are not told apart by index).
/// `trusted`: a collection's own method, which moves elements between its
/// buffers and copies each element of another buffer once: only a plain
/// value (a parameter, a callback's result) stored twice counts.
pub(crate) fn stores_twice(al: &mut Aliases<'_>, at: &Place, v: &Place, trusted: bool) -> bool {
    let s = al.s;
    let own = al.elems(at).to_vec();
    let (a_in, a_stop) = reach(s, at, &own);
    let (v_in, v_stop) = reach(s, v, &own);
    // A call's argument slot is reused on each pass of a loop.
    let moved = |p: &Place| p.1.contains(&ELEM) || (TEMP..TAG_KEY).contains(&p.0.0);
    let inner = v_in.iter().any(|x| {
        a_in.iter()
            .any(|y| overlap(x, y) && !(trusted && (moved(x) || moved(y))))
    });
    inner
        || v_stop
            .iter()
            .any(|x| a_stop.iter().any(|y| overlap(x, y)) && (!trusted || al.is_dup(x)))
}

/// The buffer whose elements `b` is may hold one object twice.
pub(crate) fn is_dup(s: &St, b: &Place) -> bool {
    !s.dup.is_empty() && Aliases::new(s).is_dup(b)
}

/// The places that are `start ++ sub` itself, through `Same` edges.
pub(crate) fn same_reach(s: &St, start: &Place) -> Vec<(Vec<u32>, Place)> {
    SameEnds::new(s).reach(start)
}

/// What a `SameEnds` walk does at a place.
pub(crate) enum Visit {
    /// Follow its edges.
    Step,
    /// Do not follow its edges.
    Skip,
    /// End the walk.
    Stop,
}

/// A `Same` edge's end on a local: (its path, order in `Edges::of`,
/// the other end).
pub(crate) type SameEnd<'s> = (&'s [u32], usize, &'s Place);

/// A place's steps through `Same` edges: (added to `sub`, place id).
pub(crate) type Steps = Vec<(Vec<u32>, usize)>;

/// The `Same` edges of `St`, walked from many starts: each local's ends
/// sorted by path, and each place's steps found once (a recursive value
/// has thousands of edges on one local).
pub(crate) struct SameEnds<'s> {
    pub(crate) s: &'s St,
    pub(crate) by: HashMap<LocalId, Vec<SameEnd<'s>>>,
    pub(crate) ids: HashMap<Place, usize>,
    pub(crate) places: Vec<Place>,
    /// In `Edges::of` order.
    pub(crate) next: Vec<Option<Steps>>,
    /// The walk that last visited a place.
    pub(crate) seen: Vec<u32>,
    pub(crate) walk: u32,
}

impl<'s> SameEnds<'s> {
    pub(crate) fn new(s: &'s St) -> Self {
        Self {
            s,
            by: HashMap::default(),
            ids: HashMap::default(),
            places: Vec::new(),
            next: Vec::new(),
            seen: Vec::new(),
            walk: 0,
        }
    }

    pub(crate) fn id(&mut self, p: Place) -> usize {
        if let Some(&i) = self.ids.get(&p) {
            return i;
        }
        let i = self.places.len();
        self.ids.insert(p.clone(), i);
        self.places.push(p);
        self.next.push(None);
        self.seen.push(0);
        i
    }

    /// [`same_reach`] on this state.
    pub(crate) fn reach(&mut self, start: &Place) -> Vec<(Vec<u32>, Place)> {
        let mut out = Vec::new();
        self.walk_from(start, |sub, c| {
            out.push((sub.to_vec(), c.clone()));
            Visit::Step
        });
        out
    }

    /// Visit each place that is `start ++ sub`, depth first; the first
    /// visit of a place keeps its `sub`. True if `visit` stopped it.
    pub(crate) fn walk_from(
        &mut self,
        start: &Place,
        mut visit: impl FnMut(&[u32], &Place) -> Visit,
    ) -> bool {
        self.walk += 1;
        let walk = self.walk;
        let first = self.id(start.clone());
        let mut work: Vec<(Vec<u32>, usize)> = vec![(Vec::new(), first)];
        while let Some((sub, c)) = work.pop() {
            if self.places[c].1.len() > MAX_DEPTH || sub.len() > MAX_DEPTH || self.seen[c] == walk {
                continue;
            }
            self.seen[c] = walk;
            match visit(&sub, &self.places[c]) {
                Visit::Step => {}
                Visit::Skip => continue,
                Visit::Stop => return true,
            }
            if self.next[c].is_none() {
                let steps = self.steps(c);
                self.next[c] = Some(steps);
            }
            for (more, x) in self.next[c].iter().flatten() {
                // Skipped when popped anyway.
                if self.places[*x].1.len() > MAX_DEPTH
                    || sub.len() + more.len() > MAX_DEPTH
                    || self.seen[*x] == walk
                {
                    continue;
                }
                work.push(([&sub[..], more].concat(), *x));
            }
        }
        false
    }

    /// Where place `c` steps through one `Same` edge, in `Edges::of`
    /// order: an end inside `c` adds its rest to `sub`, an end around `c`
    /// carries `c`'s rest over to the other end.
    pub(crate) fn steps(&mut self, c: usize) -> Steps {
        let st = self.s;
        let c = self.places[c].clone();
        let ends = self.by.entry(c.0).or_insert_with(|| {
            let mut list = Vec::new();
            for (i, e) in st.edges.of(c.0).enumerate() {
                let Edge::Same(one, two) = e else { continue };
                for (k, (x, y)) in [(one, two), (two, one)].into_iter().enumerate() {
                    if y.0 == c.0 {
                        list.push((y.1.as_slice(), 2 * i + k, x));
                    }
                }
            }
            list.sort_unstable();
            list
        });
        let mut hit: Vec<(usize, Vec<u32>, Place)> = Vec::new();
        let lo = ends.partition_point(|e| e.0 < c.1.as_slice());
        for &(y, i, x) in ends[lo..].iter().take_while(|e| e.0.starts_with(&c.1)) {
            hit.push((i, y[c.1.len()..].to_vec(), x.clone()));
        }
        for n in 0..c.1.len() {
            let pre = &c.1[..n];
            let lo = ends.partition_point(|e| e.0 < pre);
            for &(_, i, x) in ends[lo..].iter().take_while(|e| e.0 == pre) {
                hit.push((i, Vec::new(), extend(x, &c.1[n..])));
            }
        }
        hit.sort_unstable_by_key(|h| h.0);
        hit.into_iter()
            .map(|(_, more, x)| (more, self.id(x)))
            .collect()
    }
}
