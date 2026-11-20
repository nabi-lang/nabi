//! Loop body shapes: lane width, counted bodies, strips, index forms, minima and maxima.

#[allow(clippy::wildcard_imports)]
use crate::lower::vector::*;

/// The lane width of a loop body's values: one for all, or `None`.
/// `skip`: statements of no vector value; `accs`: the reductions, any
/// width (a count's lanes need not be its accumulator's). Elements may
/// be narrower: loaded to be widened, or narrowed to be stored (the
/// builder checks that they are only that).
pub(crate) fn lane_width(
    lw: &FnLower<'_, '_>,
    stmts: &[&Statement],
    skip: &HashSet<LocalId>,
    accs: &HashSet<LocalId>,
) -> Option<u32> {
    let pw = lw.c.t.pointer_width;
    let width = |p: PrimTy| -> Option<u32> {
        (p != PrimTy::Str && (p.is_int() || p.is_float())).then(|| p.bit_width(pw) / 8)
    };
    // Every value of an operation has the lane width; elements and casts
    // between integers may be narrower.
    let mut w: Option<u32> = None;
    let mut most = 0;
    let mut need_byte = false;
    let exact = |p: PrimTy, w: &mut Option<u32>| -> bool {
        if p == PrimTy::Bool {
            return true;
        }
        let Some(x) = width(p) else { return false };
        *w.get_or_insert(x) == x
    };
    for s in stmts {
        let ok = match s {
            Statement::BufStore {
                elem_ty: MirTy::Prim(p),
                ..
            }
            | Statement::Assign {
                rvalue:
                    RValue::BufLoad {
                        elem_ty: MirTy::Prim(p),
                        ..
                    },
                ..
            } => {
                need_byte |= *p == PrimTy::Bool;
                *p == PrimTy::Bool || width(*p).inspect(|x| most = most.max(*x)).is_some()
            }
            Statement::Assign { target, .. } if skip.contains(target) || accs.contains(target) => {
                true
            }
            Statement::Assign { rvalue, .. } => match rvalue {
                RValue::BinOp {
                    op_ty: MirTy::Prim(p),
                    ..
                }
                | RValue::UnaryOp {
                    op_ty: MirTy::Prim(p),
                    ..
                } => exact(*p, &mut w),
                RValue::Cast {
                    from_ty: MirTy::Prim(PrimTy::Bool),
                    ..
                }
                | RValue::Use(_) => true,
                RValue::Cast {
                    from_ty: MirTy::Prim(a),
                    to_ty: MirTy::Prim(b),
                    ..
                } if a.is_int() && b.is_int() => match (width(*a), width(*b)) {
                    (Some(x), Some(y)) => {
                        most = most.max(x.max(y));
                        true
                    }
                    _ => false,
                },
                RValue::Cast {
                    from_ty: MirTy::Prim(a),
                    to_ty: MirTy::Prim(b),
                    ..
                } => exact(*a, &mut w) && exact(*b, &mut w),
                RValue::Call {
                    ret_ty: MirTy::Prim(p),
                    ..
                } if lw.float_extreme(rvalue).is_some() || lw.float_fma(rvalue) => {
                    exact(*p, &mut w)
                }
                _ => false,
            },
            _ => true,
        };
        if !ok {
            return None;
        }
    }
    let w = match w {
        Some(x) if x >= most => x,
        Some(_) => return None,
        None if most > 0 => most,
        None => 1,
    };
    (!need_byte || w == 1).then_some(w)
}

/// Whether a loop body only sums products of byte elements into a
/// 4-byte accumulator: `s += (a[i] as i32) * (b[i] as i32)`, of `i8`
/// (or `u8`, also into a `u32`), nothing else (`skip`: no vector value).
pub(crate) fn dot_shape(
    stmts: &[&Statement],
    skip: &HashSet<LocalId>,
    accs: &HashSet<LocalId>,
) -> bool {
    let byte = |p: &PrimTy| matches!(p, PrimTy::I8 | PrimTy::U8);
    let (mut loads, mut casts, mut muls, mut adds) = (Vec::new(), 0, 0, 0);
    for s in stmts {
        match s {
            Statement::Loc(..) => {}
            Statement::Assign { target, .. } if skip.contains(target) => {}
            Statement::Assign { target, rvalue } => match rvalue {
                RValue::BufLoad {
                    elem_ty: MirTy::Prim(p),
                    ..
                } if byte(p) => loads.push(*p),
                RValue::Cast {
                    from_ty: MirTy::Prim(a),
                    to_ty: MirTy::Prim(b),
                    ..
                } if byte(a) && (*b == PrimTy::I32 || (*b == PrimTy::U32 && *a == PrimTy::U8)) => {
                    casts += 1;
                }
                RValue::BinOp {
                    op: PrimOp::Mul | PrimOp::WrappingMul,
                    op_ty: MirTy::Prim(PrimTy::I32 | PrimTy::U32),
                    ..
                } => muls += 1,
                RValue::BinOp {
                    op: PrimOp::Add | PrimOp::WrappingAdd,
                    op_ty: MirTy::Prim(PrimTy::I32 | PrimTy::U32),
                    ..
                } if accs.contains(target) => adds += 1,
                _ => return false,
            },
            _ => return false,
        }
    }
    (1..=2).contains(&loads.len())
        && loads.iter().all(|p| *p == loads[0])
        && (1..=2).contains(&casts)
        && muls == 1
        && adds == 1
}

/// A counted loop whose blocks below its header make no cycle: the
/// header runs first, the latch last, which steps a `usize` counter by
/// one, tests it against a bound and branches back. Other edges out of
/// the loop are side exits.
pub struct Body {
    /// Block indices in an order every edge but the back edge keeps,
    /// header first, latch last.
    pub(crate) order: Vec<usize>,
    /// Blocks every iteration that stays in the loop runs.
    pub(crate) full: HashSet<usize>,
    pub(crate) iv: LocalId,
    pub(crate) bound: Operand,
    /// The test's local and the value it has on exit.
    pub(crate) cond: (LocalId, bool),
    pub(crate) exit: (BlockId, Vec<Operand>),
    /// The step's position in the latch.
    pub(crate) step: usize,
}

/// Blocks a loop body may have.
pub(crate) const MAX_BODY: usize = 16;

/// Statements a loop body may have at `-Os`.
pub(crate) const SIZE_BODY: usize = 24;

/// A position or a scope end: no code.
pub(crate) const fn marker(s: &Statement) -> bool {
    matches!(s, Statement::Loc(..) | Statement::StorageDead(_))
}

/// The body of the loop of header `h` (an index of `f.blocks`); `at`:
/// block index by id, `preds`: predecessor indices per block index.
pub fn body(
    f: &MirFunction,
    h: usize,
    at: &HashMap<u32, usize>,
    preds: &[Vec<usize>],
) -> Option<Body> {
    let hid = f.blocks[h].id;
    if !f.blocks[h].params.is_empty() {
        return None;
    }
    // The latch: the one block branching back on a counted test.
    let mut latch = None;
    for &l in &preds[h] {
        let Terminator::If {
            cond: Operand::Copy(c),
            then_bb,
            then_args,
            else_bb,
            else_args,
        } = &f.blocks[l].terminator
        else {
            continue;
        };
        let (on_true, exit) = if *then_bb == hid && then_args.is_empty() && *else_bb != hid {
            (true, (*else_bb, else_args.clone()))
        } else if *else_bb == hid && else_args.is_empty() && *then_bb != hid {
            (false, (*then_bb, then_args.clone()))
        } else {
            continue;
        };
        if let Some((iv, bound, step)) = counted(&f.blocks[l], *c, on_true) {
            if latch.is_some() {
                return None;
            }
            latch = Some((l, iv, bound, (*c, !on_true), exit, step));
        }
    }
    let (l, iv, bound, cond, exit, step) = latch?;
    // The blocks that reach the latch without passing the header.
    let mut inside: HashSet<usize> = HashSet::from_iter([h, l]);
    let mut work = vec![l];
    while let Some(b) = work.pop() {
        if b == h {
            continue;
        }
        for &p in &preds[b] {
            if inside.insert(p) {
                if inside.len() > MAX_BODY {
                    return None;
                }
                work.push(p);
            }
        }
    }
    // One way in (the header), one way back (the latch).
    for &b in &inside {
        let ok = if b == h {
            preds[b]
                .iter()
                .filter(|p| inside.contains(p))
                .all(|&p| p == l)
        } else {
            preds[b].iter().all(|p| inside.contains(p))
        };
        if !ok {
            return None;
        }
    }
    let succs = |b: usize| -> Vec<usize> {
        nabic_mir::succs(&f.blocks[b])
            .into_iter()
            .filter_map(|s| at.get(&s).copied())
            .collect()
    };
    // A side exit may come back to the header (an outer loop): that is
    // another entry, through the vector loop again.
    let edges = |b: usize| -> Vec<usize> {
        succs(b)
            .into_iter()
            .filter(|s| inside.contains(s) && !(b == l && *s == h))
            .collect()
    };
    // In order: every block after all it comes from.
    let mut need: HashMap<usize, usize> = HashMap::default();
    for &b in &inside {
        for s in edges(b) {
            *need.entry(s).or_default() += 1;
        }
    }
    let mut order = Vec::new();
    let mut ready = vec![h];
    while let Some(b) = ready.pop() {
        order.push(b);
        let mut next: Vec<usize> = edges(b);
        next.dedup();
        for s in next {
            let n = need.get_mut(&s)?;
            *n -= edges(b).iter().filter(|x| **x == s).count();
            if *n == 0 {
                ready.push(s);
            }
        }
    }
    if order.len() != inside.len() || order.last() != Some(&l) {
        return None;
    }
    // Blocks every staying iteration passes: those after the header
    // on every way to the latch.
    let mut pdom: HashMap<usize, HashSet<usize>> = HashMap::default();
    for &b in order.iter().rev() {
        let ss = edges(b);
        let set = if b == l {
            HashSet::from_iter([l])
        } else if ss.is_empty() {
            inside.clone()
        } else {
            let mut acc: Option<HashSet<usize>> = None;
            for s in ss {
                let p = pdom.get(&s)?;
                acc = Some(match acc {
                    None => p.clone(),
                    Some(a) => a.intersection(p).copied().collect(),
                });
            }
            let mut a = acc?;
            a.insert(b);
            a
        };
        pdom.insert(b, set);
    }
    let full = pdom.get(&h)?.clone();
    Some(Body {
        order,
        full,
        iv,
        bound,
        cond,
        exit,
        step,
    })
}

/// The bound of the strip loop a loop's exit `exit` tests (`strip`'s
/// `S: c = ctr < bound; if c then E else ..`), if it is one.
pub(crate) fn strip_bound(
    f: &MirFunction,
    exit: &(BlockId, Vec<Operand>),
    iv: LocalId,
    c: LocalId,
) -> Option<Operand> {
    let b = f.blocks.iter().find(|b| b.id == exit.0)?;
    if !b.params.is_empty() || !exit.1.is_empty() {
        return None;
    }
    let real: Vec<&Statement> = b
        .stmts
        .iter()
        .filter(|s| !matches!(s, Statement::Loc(..)))
        .collect();
    let [
        Statement::Assign {
            target,
            rvalue:
                RValue::BinOp {
                    op: PrimOp::Lt,
                    op_ty: MirTy::Prim(PrimTy::Usize),
                    lhs: Operand::Copy(ctr),
                    rhs,
                },
        },
    ] = real.as_slice()
    else {
        return None;
    };
    let Terminator::If {
        cond: Operand::Copy(t),
        ..
    } = &b.terminator
    else {
        return None;
    };
    (*target == c && *t == c && *ctr == iv && *rhs != Operand::Copy(iv)).then(|| rhs.clone())
}

/// The counted test ending block `b` (`c`, `on_true`: the branch back is
/// taken when `c` is): `(counter, bound, position of the step)`.
pub(crate) fn counted(
    b: &BasicBlock,
    c: LocalId,
    on_true: bool,
) -> Option<(LocalId, Operand, usize)> {
    let real: Vec<usize> = (0..b.stmts.len())
        .filter(|&i| !matches!(b.stmts[i], Statement::Loc(..)))
        .collect();
    let (&ci, rest) = real.split_last()?;
    let &si = rest.last()?;
    let Statement::Assign {
        target,
        rvalue:
            RValue::BinOp {
                op,
                op_ty: MirTy::Prim(PrimTy::Usize),
                lhs,
                rhs,
            },
    } = &b.stmts[ci]
    else {
        return None;
    };
    if *target != c {
        return None;
    }
    let Statement::Assign {
        target: iv,
        rvalue:
            RValue::BinOp {
                op: PrimOp::Add | PrimOp::WrappingAdd,
                op_ty: MirTy::Prim(PrimTy::Usize),
                lhs: sl,
                rhs: sr,
            },
    } = &b.stmts[si]
    else {
        return None;
    };
    let one = |o: &Operand| {
        matches!(
            o,
            Operand::Const(ConstVal::UInt { value: 1, .. } | ConstVal::Int { value: 1, .. })
        )
    };
    let me = Operand::Copy(*iv);
    if !((*sl == me && one(sr)) || (*sr == me && one(sl))) {
        return None;
    }
    // `i < n` (or `n > i`, `i != n`) continues; its negation exits.
    let (bound, cont) = match op {
        PrimOp::Lt | PrimOp::Ge if *lhs == me => (rhs, *op == PrimOp::Lt),
        PrimOp::Gt | PrimOp::Le if *rhs == me => (lhs, *op == PrimOp::Gt),
        PrimOp::Ne | PrimOp::Eq if *lhs == me => (rhs, *op == PrimOp::Ne),
        PrimOp::Ne | PrimOp::Eq if *rhs == me => (lhs, *op == PrimOp::Ne),
        _ => return None,
    };
    if cont != on_true || *bound == me {
        return None;
    }
    Some((*iv, bound.clone(), si))
}

/// The vector loops of a function: chains by header id with the label
/// of their scalar loops, and the header of each latch.
#[derive(Default)]
pub struct Loops {
    pub chains: HashMap<u32, (Body, Label)>,
    pub latch: HashMap<u32, u32>,
}

/// Index forms, as a loop body computes them in order.
pub(crate) struct Forms {
    pub(crate) iv: LocalId,
    /// Locals holding an index.
    pub(crate) idx: HashMap<LocalId, Idx>,
    /// Locals holding a word the loop never changes, cast: the local
    /// read.
    pub(crate) alias: HashMap<LocalId, LocalId>,
    /// Some index went through `isize` or a checked `+`.
    pub(crate) guard: bool,
    /// Strides the loop never changes that an index is multiplied by
    /// (`i * stride`, a view's): the vector loop runs when each is 1.
    pub(crate) units: Vec<LocalId>,
    /// Bits of a word: integers of that width are indices too.
    pub(crate) pw: u32,
}

impl Forms {
    /// The index operand `o` holds, if one.
    pub(crate) fn idx(&self, o: &Operand) -> Option<Idx> {
        match o {
            Operand::Copy(l) if *l == self.iv => Some(Idx::IV),
            Operand::Copy(l) => self.idx.get(l).copied(),
            Operand::Const(_) => None,
        }
    }

    /// The local of a word `o` holds that the loop never changes.
    pub(crate) fn inv(&self, o: &Operand, written: &HashSet<LocalId>) -> Option<LocalId> {
        match o {
            Operand::Copy(l) if *l != self.iv && !written.contains(l) => Some(*l),
            Operand::Copy(l) => self.alias.get(l).copied(),
            Operand::Const(_) => None,
        }
    }

    /// Record `t = rv` if it makes an index or an alias.
    pub(crate) fn see(&mut self, t: LocalId, rv: &RValue, written: &HashSet<LocalId>) {
        let pw = self.pw;
        let word = |p: &PrimTy| matches!(p, PrimTy::Usize | PrimTy::Isize);
        // A cast through another integer of the width keeps the index.
        let width = |p: &PrimTy| word(p) || (p.is_int() && p.bit_width(pw) == pw);
        match rv {
            RValue::BinOp {
                op: PrimOp::Add | PrimOp::WrappingAdd,
                op_ty: MirTy::Prim(p),
                lhs,
                rhs,
            } if word(p) => {
                let small = |o: &Operand| {
                    match o {
                        Operand::Const(ConstVal::UInt { value, .. }) => u64::try_from(*value).ok(),
                        Operand::Const(ConstVal::Int { value, .. }) => u64::try_from(*value).ok(),
                        _ => None,
                    }
                    .filter(|c| *c < MAX_OFF)
                };
                let one = |a: &Operand, b: &Operand| -> Option<Idx> {
                    let x = self.idx(a)?;
                    if let Some(c) = small(b) {
                        let c = x.c + c;
                        return (c < MAX_OFF).then_some(Idx { inv: x.inv, c });
                    }
                    let l = self.inv(b, written)?;
                    x.inv.is_none().then_some(Idx {
                        inv: Some(l),
                        c: x.c,
                    })
                };
                if let Some(x) = one(lhs, rhs).or_else(|| one(rhs, lhs)) {
                    // `iv + c` in `usize` alone needs no guard: the
                    // accesses at it are in bounds.
                    self.guard |= x.inv.is_some() || *p == PrimTy::Isize;
                    self.idx.insert(t, x);
                }
            }
            // `x * s` is `x` where the stride `s` is 1, tested in front.
            RValue::BinOp {
                op: PrimOp::Mul | PrimOp::WrappingMul,
                op_ty: MirTy::Prim(p),
                lhs,
                rhs,
            } if word(p) => {
                let one = |a: &Operand, b: &Operand| Some((self.idx(a)?, self.inv(b, written)?));
                if let Some((x, l)) = one(lhs, rhs).or_else(|| one(rhs, lhs)) {
                    if !self.units.contains(&l) {
                        self.units.push(l);
                    }
                    self.idx.insert(t, x);
                }
            }
            RValue::Cast {
                source,
                from_ty: MirTy::Prim(a),
                to_ty: MirTy::Prim(b),
            } if width(a) && width(b) => {
                if let Some(x) = self.idx(source) {
                    self.guard |= a != b;
                    self.idx.insert(t, x);
                } else if let Some(l) = self.inv(source, written) {
                    self.alias.insert(t, l);
                }
            }
            RValue::Use(o) => {
                if let Some(x) = self.idx(o) {
                    self.idx.insert(t, x);
                }
            }
            _ => {}
        }
    }
}

/// Carried local `m` updated as `m = if x < m { x } else { m }` (or by
/// `<=`, `>`, `>=`), through a block parameter or an assignment on one
/// way only, or as `m = m.min(x)` (`ext`: whether a value calls the
/// runtime's float `max`, or `min`): `(max, signed)`. Equal elements are
/// equal integers: which one is kept does not show (the vector loop
/// leaves floats that are NaNs or zeros to the scalar one).
pub(crate) fn minmax(
    f: &MirFunction,
    order: &[usize],
    m: LocalId,
    reads: &HashMap<LocalId, u32>,
    assigns: &HashMap<LocalId, u32>,
    ext: &impl Fn(&RValue) -> Option<bool>,
) -> Option<(bool, bool)> {
    let me = Operand::Copy(m);
    let n = |map: &HashMap<LocalId, u32>, l: LocalId| map.get(&l).copied().unwrap_or(0);
    let stmts = || {
        order
            .iter()
            .flat_map(|&b| f.blocks[b].stmts.iter().map(move |s| (b, s)))
    };
    let swap = |op: PrimOp| match op {
        PrimOp::Lt => PrimOp::Gt,
        PrimOp::Le => PrimOp::Ge,
        PrimOp::Gt => PrimOp::Lt,
        _ => PrimOp::Le,
    };
    let one = |map: &HashMap<LocalId, u32>, l: LocalId| n(map, l) == 1;
    for (_, s) in stmts() {
        // `t = m.min(x)` (or `x.min(m)`), then `m = t`.
        let Statement::Assign {
            target: t,
            rvalue: rv @ RValue::Call { args, .. },
        } = s
        else {
            continue;
        };
        let (Some(max), [a, b]) = (ext(rv), args.as_slice()) else {
            continue;
        };
        if *a != me && *b != me {
            continue;
        }
        let x = if *a == me { b } else { a };
        if *x == me
            || !one(reads, m)
            || !one(assigns, m)
            || !one(reads, *t)
            || !one(assigns, *t)
            || matches!(x, Operand::Copy(l) if n(assigns, *l) > 1)
        {
            return None;
        }
        let back = Operand::Copy(*t);
        let set = stmts().any(|(_, s)| {
            matches!(s, Statement::Assign { target, rvalue: RValue::Use(o) }
                if *target == m && *o == back)
        });
        return set.then_some((max, false));
    }
    let mut cmp = None;
    for (_, s) in stmts() {
        if let Statement::Assign {
            target,
            rvalue:
                RValue::BinOp {
                    op: op @ (PrimOp::Lt | PrimOp::Le | PrimOp::Gt | PrimOp::Ge),
                    op_ty: MirTy::Prim(p),
                    lhs,
                    rhs,
                },
        } = s
        {
            if (p.is_int() || p.is_float()) && ((*lhs == me) != (*rhs == me)) {
                if cmp.is_some() {
                    return None;
                }
                // `x rel m`.
                cmp = Some(if *lhs == me {
                    (*target, rhs.clone(), swap(*op), *p)
                } else {
                    (*target, lhs.clone(), *op, *p)
                });
            }
        }
    }
    let (c, x, rel, p) = cmp?;
    if n(reads, c) != 1 || n(assigns, c) != 1 || n(assigns, m) != 1 {
        return None;
    }
    if let Operand::Copy(l) = &x {
        if n(assigns, *l) > 1 {
            return None;
        }
    }
    let (bi, then_bb, then_args, else_bb, else_args) =
        order.iter().find_map(|&b| match &f.blocks[b].terminator {
            Terminator::If {
                cond: Operand::Copy(t),
                then_bb,
                then_args,
                else_bb,
                else_args,
            } if *t == c => Some((b, *then_bb, then_args, *else_bb, else_args)),
            _ => None,
        })?;
    let at = |id: BlockId| order.iter().copied().find(|&b| f.blocks[b].id == id);
    let assigned = |b: usize, v: &Operand| {
        f.blocks[b].stmts.iter().any(|s| {
            matches!(s, Statement::Assign { target, rvalue: RValue::Use(o) }
                if *target == m && o == v)
        })
    };
    // Whether `x` is taken where `c` holds.
    let mut taken = None;
    if then_bb == else_bb {
        let j = at(then_bb)?;
        for (k, (ta, ea)) in then_args.iter().zip(else_args).enumerate() {
            let pick = if *ta == x && *ea == me {
                true
            } else if *ta == me && *ea == x {
                false
            } else {
                continue;
            };
            let q = *f.blocks[j].params.get(k)?;
            let set = order.iter().any(|&b| assigned(b, &Operand::Copy(q)));
            if set && n(reads, q) == 1 && n(reads, m) == 2 {
                taken = Some(pick);
            }
            break;
        }
    } else {
        for (t, pick) in [(then_bb, true), (else_bb, false)] {
            let Some(tb) = at(t) else { continue };
            // Runs only for the lanes the test sends there.
            let from: Vec<usize> = order
                .iter()
                .copied()
                .filter(|&b| nabic_mir::succs(&f.blocks[b]).contains(&t.0))
                .collect();
            if from == [bi] && assigned(tb, &x) && n(reads, m) == 1 {
                taken = Some(pick);
            }
        }
    }
    let rel = if taken? {
        rel
    } else {
        match rel {
            PrimOp::Lt => PrimOp::Ge,
            PrimOp::Le => PrimOp::Gt,
            PrimOp::Gt => PrimOp::Le,
            _ => PrimOp::Lt,
        }
    };
    Some((matches!(rel, PrimOp::Gt | PrimOp::Ge), p.is_signed_int()))
}

impl FnLower<'_, '_> {
    /// `Some(max)` where `rv` calls the runtime's float `max` (or `min`).
    pub(crate) fn float_extreme(&self, rv: &RValue) -> Option<bool> {
        let RValue::Call { callee, args, .. } = rv else {
            return None;
        };
        let sig = self.c.fns.get(callee)?;
        let name = self.c.obj.syms.get(sig.sym as usize)?.name.as_str();
        let max = match name {
            "nabi_f64_min" | "nabi_f32_min" => false,
            "nabi_f64_max" | "nabi_f32_max" => true,
            _ => return None,
        };
        (args.len() == 2).then_some(max)
    }

    /// Whether `rv` calls the runtime's float `mul_add`.
    pub(crate) fn float_fma(&self, rv: &RValue) -> bool {
        let RValue::Call { callee, args, .. } = rv else {
            return false;
        };
        let Some(sig) = self.c.fns.get(callee) else {
            return false;
        };
        let name = self
            .c
            .obj
            .syms
            .get(sig.sym as usize)
            .map(|s| s.name.as_str());
        matches!(name, Some("nabi_f64_mul_add" | "nabi_f32_mul_add")) && args.len() == 3
    }
}

/// The object a load reads, if a local.
pub(crate) const fn load_base(rv: &RValue) -> Option<LocalId> {
    match rv {
        RValue::HeapField {
            ptr: Operand::Copy(l),
            ..
        }
        | RValue::HeapLoad {
            ptr: Operand::Copy(l),
            ..
        }
        | RValue::BufLoad {
            buf: Operand::Copy(l),
            ..
        } => Some(*l),
        _ => None,
    }
}

/// One step of a loop body, in order.
#[derive(Clone, Copy)]
pub(crate) enum Item<'s> {
    /// Into a block (its index): its parameters.
    Start(usize),
    S(&'s Statement),
    /// Out of a block but the latch: its branch.
    End(usize),
}
