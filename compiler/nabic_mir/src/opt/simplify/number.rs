//! Value numbering over the dominator tree.

#[allow(clippy::wildcard_imports)]
use crate::opt::simplify::*;

/// An aggregate a local is known to hold.
#[derive(Clone, Debug)]
pub(crate) enum Agg {
    /// Built from these fields.
    Fields(Vec<Operand>),
    /// Element 0 of buffer `buf` as loaded (a struct or tuple `ty`), while
    /// memory is unchanged: its field `k` reads the object in place.
    Elem0(Operand, MirTy),
}

#[derive(Clone, Default)]
pub(crate) struct State {
    pub(crate) pw: u32,
    /// Local -> an operand of equal value and type.
    pub(crate) vals: HashMap<LocalId, Operand>,
    /// Available pure expressions and loads `(rvalue, holder, load)`.
    pub(crate) exprs: Vec<(Rc<RValue>, LocalId, bool)>,
    pub(crate) aggs: HashMap<LocalId, Rc<Agg>>,
    pub(crate) facts: Vec<Fact>,
    /// Loads of a place a store wrote, and the value it wrote.
    pub(crate) stored: Vec<(Rc<RValue>, Operand)>,
}

pub(crate) fn reads(o: &Operand, l: LocalId) -> bool {
    matches!(o, Operand::Copy(x) if *x == l)
}

/// Whether `rv` reads a local `hit` picks.
pub(crate) fn rv_reads_any(rv: &RValue, hit: &dyn Fn(&LocalId) -> bool) -> bool {
    match rv {
        RValue::AddrOf(x) => hit(x),
        RValue::ArrayIndex { array, index, .. } => {
            hit(array) || matches!(index, Operand::Copy(x) if hit(x))
        }
        _ => {
            let mut any = false;
            crate::opt::rvalue_ops(rv, &mut |o| any |= matches!(o, Operand::Copy(x) if hit(x)));
            any
        }
    }
}

pub(crate) fn rv_reads(rv: &RValue, l: LocalId) -> bool {
    match rv {
        RValue::AddrOf(x) => *x == l,
        RValue::ArrayIndex { array, index, .. } => *array == l || reads(index, l),
        _ => {
            let mut any = false;
            crate::opt::rvalue_ops(rv, &mut |o| any |= reads(o, l));
            any
        }
    }
}

impl State {
    /// `l` changes (or ends): forget what it held and what used it.
    pub(crate) fn kill(&mut self, l: LocalId, ever: &[Cell<bool>]) {
        // Nothing here names it.
        if !ever[l.0 as usize].get() {
            return;
        }
        self.vals.remove(&l);
        self.vals.retain(|_, v| !reads(v, l));
        self.exprs.retain(|(rv, h, _)| *h != l && !rv_reads(rv, l));
        self.aggs.remove(&l);
        self.aggs.retain(|_, a| match &**a {
            Agg::Fields(fs) => !fs.iter().any(|o| reads(o, l)),
            Agg::Elem0(b, _) => !reads(b, l),
        });
        forget(&mut self.facts, l, self.pw);
        self.stored
            .retain(|(rv, v)| !rv_reads(rv, l) && !reads(v, l));
    }

    /// Effect `e` happened: forget the loads it may change.
    pub(crate) fn clobber(&mut self, e: &Eff, fresh: &HashSet<LocalId>) {
        if *e == Eff::None {
            return;
        }
        let hit = |rv: &RValue| alias::load(rv).is_some_and(|a| alias::clobbers(e, &a, fresh));
        self.exprs.retain(|(rv, _, load)| !load || !hit(rv));
        // What a box was made of is not carried past a call: the box
        // would die early, and a collection there can tell.
        let call = *e == Eff::Any;
        self.stored
            .retain(|(rv, _)| !(hit(rv) || call && matches!(**rv, RValue::HeapLoad { .. })));
        self.aggs.retain(|_, a| match &**a {
            Agg::Fields(_) => true,
            Agg::Elem0(b, ty) => !alias::clobbers(
                e,
                &alias::View {
                    base: b,
                    ty,
                    part: alias::PartRef::Whole,
                },
                fresh,
            ),
        });
    }

    /// What of this state, at the end of the immediate dominator of a
    /// block, holds at the block's entry: the blocks between assigned
    /// `kills` and did `effs`.
    pub(crate) fn carried(&self, kills: &[bool], effs: &[Eff], fresh: &HashSet<LocalId>) -> Self {
        let killed = |l: &LocalId| kills[l.0 as usize];
        let hit = |o: &Operand| matches!(o, Operand::Copy(x) if killed(x));
        let rv_hit = |rv: &RValue| rv_reads_any(rv, &killed);
        // Not a comparison: a branch reads it at once, and computing it
        // again beats holding a flag across the blocks between.
        let cheap = |rv: &RValue| matches!(rv, RValue::BinOp { op, .. } if is_cmp(*op));
        let mut st = Self {
            pw: self.pw,
            vals: self
                .vals
                .iter()
                .filter(|(l, v)| !killed(l) && !hit(v))
                .map(|(l, v)| (*l, v.clone()))
                .collect(),
            exprs: self
                .exprs
                .iter()
                .filter(|(rv, h, _)| !cheap(rv) && !killed(h) && !rv_hit(rv))
                .cloned()
                .collect(),
            aggs: self
                .aggs
                .iter()
                .filter(|(l, a)| {
                    !killed(l)
                        && match &***a {
                            Agg::Fields(fs) => !fs.iter().any(hit),
                            Agg::Elem0(b, _) => !hit(b),
                        }
                })
                .map(|(l, a)| (*l, a.clone()))
                .collect(),
            facts: Vec::new(),
            stored: self
                .stored
                .iter()
                .filter(|(rv, v)| !rv_hit(rv) && !hit(v))
                .cloned()
                .collect(),
        };
        for e in effs {
            st.clobber(e, fresh);
        }
        st
    }
}

pub(crate) struct Ctx<'a> {
    pub(crate) pw: u32,
    pub(crate) ro: &'a HashSet<FunctionId>,
    pub(crate) tys: &'a [crate::LocalDecl],
    pub(crate) pinned: &'a HashSet<LocalId>,
    pub(crate) fresh: &'a HashSet<LocalId>,
    /// Locals some state ever named: a kill of any other finds nothing.
    pub(crate) ever: Vec<Cell<bool>>,
}

impl Ctx<'_> {
    pub(crate) fn mark(&self, l: LocalId) {
        self.ever[l.0 as usize].set(true);
    }

    pub(crate) fn mark_op(&self, o: &Operand) {
        if let Operand::Copy(l) = o {
            self.mark(*l);
        }
    }

    pub(crate) fn mark_rv(&self, rv: &RValue) {
        rv_reads_any(rv, &|l| {
            self.mark(*l);
            false
        });
    }

    pub(crate) fn mark_fact(&self, x: &Fact) {
        self.mark_op(&x.a);
        self.mark_op(&x.b);
    }

    pub(crate) const fn ty(&self, l: LocalId) -> &MirTy {
        &self.tys[l.0 as usize].ty
    }

    /// Whether `o` is a `t`.
    pub(crate) fn is(&self, o: &Operand, t: &MirTy) -> bool {
        crate::opt::has_ty(self.tys, o, t)
    }
}

/// A block further than this many blocks from its immediate dominator
/// starts afresh.
pub(crate) const MAX_SPAN: usize = 256;

/// More distinct stores than this between them count as any write.
pub(crate) const MAX_EFFS: usize = 64;

/// What block `b` assigns (parameters included) and may write.
pub(crate) fn summary(b: &crate::BasicBlock, ro: &HashSet<FunctionId>) -> (Vec<LocalId>, Vec<Eff>) {
    let mut kills: Vec<LocalId> = b.params.clone();
    let mut effs = Vec::new();
    for s in &b.stmts {
        match s {
            Statement::Assign { target, .. } => kills.push(*target),
            Statement::StorageDead(l) | Statement::ArrayWrite { array: l, .. } => kills.push(*l),
            _ => {}
        }
        let e = alias::effect(s, ro);
        if e != Eff::None && !effs.contains(&e) {
            effs.push(e);
        }
    }
    (kills, effs)
}

/// The effects `es` of the blocks between a block and its dominator, as
/// few: `Any` covers every write but one to a fresh object (no call
/// reaches it), so those stay listed beside it. `None` when too many
/// fresh writes are left to list.
pub(crate) fn merge_effs<'a>(
    es: impl Iterator<Item = &'a Eff>,
    fresh: &HashSet<LocalId>,
) -> Option<Vec<Eff>> {
    let to_fresh = |e: &Eff| matches!(e, Eff::Write(w) if matches!(w.base, Operand::Copy(x) if fresh.contains(&x)));
    let mut any = false;
    let mut effs: Vec<Eff> = Vec::new();
    for e in es {
        if *e == Eff::Any {
            any = true;
            continue;
        }
        if (any && !to_fresh(e)) || effs.contains(e) {
            continue;
        }
        if effs.len() >= MAX_EFFS {
            if to_fresh(e) {
                return None;
            }
            any = true;
            continue;
        }
        effs.push(e.clone());
    }
    if any {
        effs.retain(to_fresh);
        effs.push(Eff::Any);
    }
    Some(effs)
}

/// The blocks on some way from `d` to `b` (`d` excluded, `b` itself only
/// in a loop), if few.
pub(crate) fn span(preds: &[Vec<usize>], d: usize, b: usize) -> Option<Vec<usize>> {
    let mut seen: HashSet<usize> = HashSet::default();
    let mut work: Vec<usize> = preds[b].clone();
    while let Some(p) = work.pop() {
        if p == d || !seen.insert(p) {
            continue;
        }
        if seen.len() > MAX_SPAN {
            return None;
        }
        work.extend(preds[p].iter().copied());
    }
    Some(seen.into_iter().collect())
}

/// Dominator-tree value numbering over every block.
pub(crate) fn blocks(
    f: &mut MirFunction,
    sh: &Shape<'_>,
    pw: u32,
    ro: &HashSet<FunctionId>,
) -> bool {
    let Shape {
        idx,
        preds,
        dom,
        pinned,
    } = sh;
    let order = crate::opt::rpo(f, idx);
    let facts = entry_facts(f, idx, pinned, pw);
    let fresh = alias::fresh(f);
    // Only a block of several predecessors reads them.
    let merges = (1..f.blocks.len()).any(|b| preds[b].len() > 1);
    let sums: Vec<(Vec<LocalId>, Vec<Eff>)> = if merges {
        f.blocks.iter().map(|b| summary(b, ro)).collect()
    } else {
        Vec::new()
    };
    // Statements change, locals do not: lend their types out.
    let tys = std::mem::take(&mut f.locals);
    let cx = Ctx {
        pw,
        ro,
        tys: &tys,
        pinned,
        fresh: &fresh,
        ever: (0..tys.len()).map(|_| Cell::new(false)).collect(),
    };
    // The block whose end state each block starts from: its only
    // predecessor, else its immediate dominator.
    let from: Vec<Option<usize>> = (0..f.blocks.len())
        .map(|b| match preds[b][..] {
            [p] if p != b => Some(p),
            _ if b != 0 && dom.reachable(b) => Some(dom.idom(b)),
            _ => None,
        })
        .collect();
    let mut left: HashMap<usize, usize> = HashMap::default();
    for p in from.iter().flatten() {
        *left.entry(*p).or_default() += 1;
    }
    let mut out: HashMap<usize, State> = HashMap::default();
    // Locals assigned between a block and its immediate dominator.
    let mut kills = vec![false; tys.len()];
    let mut changed = false;
    for &b in &order {
        let fresh_st = || State {
            pw,
            ..State::default()
        };
        let mut st = match from.get(b).copied().flatten() {
            Some(p) => {
                let last = left.get(&p) == Some(&1);
                if let Some(n) = left.get_mut(&p) {
                    *n -= 1;
                }
                let st = if preds[b].len() > 1 {
                    match (span(preds, p, b), out.get(&p)) {
                        (Some(between), Some(from_st)) => 'carry: {
                            let mut touched: Vec<LocalId> = Vec::new();
                            for &x in &between {
                                touched.extend(sums[x].0.iter().copied());
                            }
                            let Some(effs) =
                                merge_effs(between.iter().flat_map(|&x| &sums[x].1), &fresh)
                            else {
                                break 'carry fresh_st();
                            };
                            touched.extend(f.blocks[b].params.iter().copied());
                            for l in &touched {
                                kills[l.0 as usize] = true;
                            }
                            let st = from_st.carried(&kills, &effs, &fresh);
                            for l in &touched {
                                kills[l.0 as usize] = false;
                            }
                            st
                        }
                        _ => fresh_st(),
                    }
                } else if last {
                    out.remove(&p).unwrap_or_else(fresh_st)
                } else {
                    out.get(&p).cloned().unwrap_or_else(fresh_st)
                };
                if last {
                    out.remove(&p);
                }
                st
            }
            None => fresh_st(),
        };
        // The facts, in the operands the state reads them as.
        {
            st.facts.clone_from(&facts[b]);
            for x in &mut st.facts {
                for o in [&mut x.a, &mut x.b] {
                    if let Operand::Copy(l) = o {
                        if let Some(v) = st.vals.get(l) {
                            *o = v.clone();
                        }
                    }
                }
                cx.mark_fact(x);
            }
            for p in &f.blocks[b].params.clone() {
                st.kill(*p, &cx.ever);
            }
        };
        changed |= walk(&cx, &mut st, &mut f.blocks[b]);
        if left.contains_key(&b) {
            out.insert(b, st);
        }
    }
    f.locals = tys;
    changed
}

/// Value numbering through block `bb`, from state `st`.
pub(crate) fn walk(cx: &Ctx<'_>, st: &mut State, bb: &mut crate::BasicBlock) -> bool {
    let pw = cx.pw;
    let mut changed = false;
    let mut dead_stores: Vec<usize> = Vec::new();
    // Stores to memory nothing read since, by statement.
    let mut pending: Vec<(usize, Acc)> = Vec::new();
    // Last unread assignment of a local in this block, if removable.
    let mut unread: HashMap<LocalId, usize> = HashMap::default();
    let n = bb.stmts.len();
    for si in 0..n {
        let s = &mut bb.stmts[si];
        crate::opt::stmt_ops_mut(s, &mut |o| {
            if let Operand::Copy(x) = o {
                if let Some(v) = st.vals.get(x) {
                    *o = v.clone();
                    changed = true;
                }
            }
        });
        // An element of a copy is the element of what it copies (a
        // struct parameter an inlined body copied).
        if let Statement::Assign {
            rvalue: RValue::ArrayIndex { array, .. },
            ..
        } = s
        {
            if let Some(Operand::Copy(y)) = st.vals.get(array) {
                if !cx.pinned.contains(array) && !cx.pinned.contains(y) {
                    *array = *y;
                    changed = true;
                }
            }
        }
        // Element 0 stored back with one field changed: a store of that
        // field (no barrier for the fields that hold references).
        if let Some(one) = one_field(st, s) {
            *s = one;
            changed = true;
        }
        if !unread.is_empty() {
            crate::stmt_reads(s, &mut |r| {
                unread.remove(&r);
            });
        }
        match s {
            Statement::Assign { target, rvalue } => {
                let t = *target;
                // What a checked step proves, whatever it becomes.
                let made = if cx.pinned.contains(&t) || rv_reads(rvalue, t) {
                    None
                } else {
                    step_of(cx.tys, cx.pinned, pw, t, rvalue)
                };
                if let Some(new) = simplify_rv(cx, st, t, rvalue) {
                    *rvalue = new;
                    changed = true;
                }
                if let Some(a) = alias::load(rvalue) {
                    pending.retain(|(_, w)| !alias::may_alias(&w.view(), &a, cx.fresh));
                } else if !pure(rvalue, pw) {
                    // A call, a trap, an allocation: a store may be seen.
                    pending.clear();
                }
                pending.retain(|(_, w)| !w.reads(t));
                // Dead store: assigned again before any read.
                if let Some(prev) = unread.remove(&t) {
                    dead_stores.push(prev);
                }
                if !cx.pinned.contains(&t) && removable(rvalue, pw) && !rv_reads(rvalue, t) {
                    unread.insert(t, si);
                }
                st.kill(t, &cx.ever);
                st.clobber(&alias::effect(s, cx.ro), cx.fresh);
                let Statement::Assign { rvalue, .. } = &*s else {
                    unreachable!()
                };
                if !cx.pinned.contains(&t) && !rv_reads(rvalue, t) {
                    record(cx, st, t, rvalue);
                }
                if let Some(x) = &made {
                    cx.mark_fact(x);
                }
                st.facts.extend(made);
            }
            Statement::StorageDead(l) => {
                let l = *l;
                st.kill(l, &cx.ever);
                unread.remove(&l);
                pending.retain(|(_, w)| !w.reads(l));
            }
            Statement::ArrayWrite { array, .. } => {
                let a = *array;
                st.kill(a, &cx.ever);
                unread.remove(&a);
                pending.retain(|(_, w)| !w.reads(a));
            }
            Statement::BufStore { value, .. } | Statement::HeapStore { value, .. } => {
                let value = value.clone();
                let e = alias::effect(s, cx.ro);
                st.clobber(&e, cx.fresh);
                let Eff::Write(w) = e else { unreachable!() };
                // Overwritten before any read.
                if let Some(k) = pending.iter().position(|(_, p)| *p == w) {
                    dead_stores.push(pending.swap_remove(k).0);
                }
                if !matches!(&value, Operand::Copy(v) if cx.pinned.contains(v)) {
                    if st.stored.len() >= 16 {
                        st.stored.remove(0);
                    }
                    let pl = place(&w);
                    cx.mark_rv(&pl);
                    cx.mark_op(&value);
                    st.stored.push((Rc::new(pl), value));
                }
                pending.push((si, w));
            }
            Statement::Loc(..) => {}
            s => {
                st.clobber(&alias::effect(s, cx.ro), cx.fresh);
                pending.clear();
            }
        }
    }
    if !dead_stores.is_empty() {
        dead_stores.sort_unstable();
        for i in dead_stores.into_iter().rev() {
            bb.stmts.remove(i);
        }
        changed = true;
    }
    let t = &mut bb.terminator;
    crate::opt::term_ops_mut(t, &mut |o| {
        if let Operand::Copy(x) = o {
            if let Some(v) = st.vals.get(x) {
                *o = v.clone();
                changed = true;
            }
        }
    });
    // A branch the facts decide.
    if let Terminator::If {
        cond: cond @ Operand::Copy(_),
        ..
    } = t
    {
        let Operand::Copy(c) = cond else {
            unreachable!()
        };
        let known = st.exprs.iter().find_map(|(rv, h, _)| match &**rv {
            RValue::BinOp {
                op,
                op_ty: MirTy::Prim(p),
                lhs,
                rhs,
            } if h == c && is_cmp(*op) && fact_ty(&MirTy::Prim(*p)) => decide(
                &st.facts,
                &Fact {
                    op: *op,
                    ty: *p,
                    a: lhs.clone(),
                    b: rhs.clone(),
                },
            ),
            _ => None,
        });
        if let Some(v) = known {
            *cond = Operand::Const(ConstVal::Bool(v));
            changed = true;
        }
    }
    changed
}

/// A checked `x - c` / `x + c` (`c > 0`) that did not trap is below /
/// above `x`; an unsigned `x % y` below `y`.
pub(crate) fn step_of(
    tys: &[crate::LocalDecl],
    pinned: &HashSet<LocalId>,
    pw: u32,
    t: LocalId,
    rv: &RValue,
) -> Option<Fact> {
    // Unsigned `x % y` (`y` not zero, or it trapped) is below `y`.
    if let RValue::BinOp {
        op: PrimOp::Rem,
        op_ty: MirTy::Prim(p),
        rhs: y @ Operand::Copy(yl),
        ..
    } = rv
    {
        let ok = p.is_unsigned_int()
            && !pinned.contains(yl)
            && *yl != t
            && tys[t.0 as usize].ty == MirTy::Prim(*p)
            && tys[yl.0 as usize].ty == MirTy::Prim(*p);
        return ok.then(|| Fact {
            op: PrimOp::Lt,
            ty: *p,
            a: Operand::Copy(t),
            b: y.clone(),
        });
    }
    let RValue::BinOp {
        op: op @ (PrimOp::Add | PrimOp::Sub),
        op_ty: MirTy::Prim(p),
        lhs: x @ Operand::Copy(xl),
        rhs: Operand::Const(c),
    } = rv
    else {
        return None;
    };
    let ok = (p.is_signed_int() || p.is_unsigned_int())
        && int_val(c, *p, pw).is_some_and(|v| v > 0)
        && !pinned.contains(xl)
        && *xl != t
        && tys[t.0 as usize].ty == MirTy::Prim(*p)
        && tys[xl.0 as usize].ty == MirTy::Prim(*p);
    ok.then(|| Fact {
        op: if *op == PrimOp::Sub {
            PrimOp::Lt
        } else {
            PrimOp::Gt
        },
        ty: *p,
        a: Operand::Copy(t),
        b: x.clone(),
    })
}

/// The load reading what store `w` wrote.
pub(crate) fn place(w: &Acc) -> RValue {
    match &w.part {
        alias::Part::Field(k) => RValue::HeapField {
            ptr: w.base.clone(),
            ty: w.ty.clone(),
            index: *k,
        },
        alias::Part::Elem(i) => RValue::BufLoad {
            buf: w.base.clone(),
            index: i.clone(),
            elem_ty: w.ty.clone(),
        },
    }
}

/// `b[0] = v` where `v` is built of what `b[0]` holds but for field
/// `k`, as the store of field `k` alone: the other fields are loads of
/// the same place memory has not changed since.
fn one_field(st: &State, s: &Statement) -> Option<Statement> {
    let Statement::BufStore {
        buf,
        index: Operand::Const(i),
        value: Operand::Copy(v),
        elem_ty: ty @ (MirTy::Struct(_, tys) | MirTy::Tuple(tys)),
    } = s
    else {
        return None;
    };
    if int_val(i, PrimTy::Usize, st.pw) != Some(0) {
        return None;
    }
    let Agg::Fields(fs) = &**st.aggs.get(v)? else {
        return None;
    };
    if fs.len() != tys.len() {
        return None;
    }
    let held = |j: usize, o: &Operand| {
        let Operand::Copy(h) = o else {
            return false;
        };
        st.exprs.iter().any(|(rv, x, load)| {
            *load
                && x == h
                && matches!(&**rv, RValue::HeapField { ptr, ty: t, index }
                    if ptr == buf && t == ty && *index as usize == j)
        })
    };
    let mut changed = (0..fs.len()).filter(|&j| !held(j, &fs[j]));
    let k = changed.next()?;
    if changed.next().is_some() {
        return None;
    }
    Some(Statement::HeapStore {
        ptr: buf.clone(),
        ty: ty.clone(),
        index: k as u32,
        value: fs[k].clone(),
    })
}

/// What `t = rv` lets later statements know.
pub(crate) fn record(cx: &Ctx<'_>, st: &mut State, t: LocalId, rv: &RValue) {
    let tt = cx.ty(t);
    match rv {
        RValue::Use(o) => {
            if cx.is(o, tt) && !matches!(o, Operand::Copy(y) if cx.pinned.contains(y)) {
                cx.mark(t);
                cx.mark_op(o);
                if let Some(a) = match o {
                    Operand::Copy(y) => st.aggs.get(y).cloned(),
                    Operand::Const(_) => None,
                } {
                    st.aggs.insert(t, a);
                }
                st.vals.insert(t, o.clone());
            }
            return;
        }
        RValue::StructInit { fields, .. }
        | RValue::ArrayInit {
            elements: fields, ..
        } => {
            cx.mark(t);
            fields.iter().for_each(|o| cx.mark_op(o));
            st.aggs.insert(t, Rc::new(Agg::Fields(fields.clone())));
        }
        // A box holds what it was made of until a store or call says
        // otherwise.
        RValue::HeapBox { value, ty } if !matches!(value, Operand::Copy(v) if cx.pinned.contains(v)) =>
        {
            if st.stored.len() >= 16 {
                st.stored.remove(0);
            }
            let pl = RValue::HeapLoad {
                ptr: Operand::Copy(t),
                ty: ty.clone(),
            };
            cx.mark(t);
            cx.mark_op(value);
            st.stored.push((Rc::new(pl), value.clone()));
        }
        RValue::BufLoad {
            buf,
            index: Operand::Const(i),
            elem_ty: ty @ (MirTy::Struct(..) | MirTy::Tuple(_)),
        } if matches!(int_val(i, PrimTy::Usize, cx.pw), Some(0)) => {
            cx.mark(t);
            cx.mark_op(buf);
            st.aggs
                .insert(t, Rc::new(Agg::Elem0(buf.clone(), ty.clone())));
        }
        _ => {}
    }
    let load = matches!(
        rv,
        RValue::BufLoad { .. } | RValue::HeapField { .. } | RValue::HeapLoad { .. }
    );
    if (load || pure(rv, cx.pw) || arith(rv)) && !matches!(rv, RValue::ArrayInit { .. }) {
        if st.exprs.len() >= 48 {
            st.exprs.remove(0);
        }
        cx.mark(t);
        cx.mark_rv(rv);
        st.exprs.push((Rc::new(rv.clone()), t, load));
    }
}

/// Arithmetic that may trap: where an equal computation already ran, it
/// did not trap and its value is this one's.
pub(crate) const fn arith(rv: &RValue) -> bool {
    matches!(
        rv,
        RValue::BinOp {
            op_ty: MirTy::Prim(_),
            ..
        } | RValue::UnaryOp {
            op_ty: MirTy::Prim(_),
            ..
        }
    )
}

/// A simpler form of `rv` (the value of `t`), if one is known.
pub(crate) fn simplify_rv(cx: &Ctx<'_>, st: &State, t: LocalId, rv: &RValue) -> Option<RValue> {
    let tt = cx.ty(t);
    // An expression some local already holds.
    let cse = matches!(
        rv,
        RValue::BufLoad { .. } | RValue::HeapField { .. } | RValue::HeapLoad { .. }
    ) || arith(rv)
        || (pure(rv, cx.pw) && !matches!(rv, RValue::Use(_) | RValue::ArrayInit { .. }));
    if cse {
        if let Some((_, h, _)) = st.exprs.iter().find(|(e, h, _)| **e == *rv && *h != t) {
            if cx.ty(*h) == tt && !cx.pinned.contains(h) {
                return Some(RValue::Use(Operand::Copy(*h)));
            }
        }
    }
    if let Some(new) = fold(cx, st, tt, rv) {
        return Some(new);
    }
    // What a store there wrote.
    if let Some((_, v)) = st.stored.iter().rev().find(|(e, _)| **e == *rv) {
        if cx.is(v, tt) && !reads(v, t) {
            return Some(RValue::Use(v.clone()));
        }
    }
    None
}

pub(crate) const fn bool_of(o: &Operand) -> Option<bool> {
    match o {
        Operand::Const(ConstVal::Bool(b)) => Some(*b),
        _ => None,
    }
}
