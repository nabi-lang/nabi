//! Integer ranges: a forward dataflow of the interval each integer local
//! holds (constants, steps, masks, casts, and what the branches taken
//! compared), widened to thresholds taken from the function's
//! comparisons and narrowed again. A load of a count the library keeps
//! below what memory holds (`Bounds`: a list's length) starts bounded,
//! so `i + j` of two indices of such lists fits. Checked `+ - *` whose
//! result always fits becomes wrapping (it cannot trap: no check), and
//! a comparison the ranges decide becomes a constant.

use crate::opt::{int_range, int_val};
use crate::{MirFunction, MirTy, Operand, RValue, Statement, Terminator};
use nabic_core::{HashMap, HashSet};
use nabic_ir_common::{ConstVal, LocalId, PrimOp, PrimTy};

type Iv = (i128, i128);

/// Interval per local; a local not in it may hold any value of its type.
type Env = HashMap<LocalId, Iv>;

/// Larger functions are left alone.
const MAX_BLOCKS: usize = 4000;

/// Passes to a fixed point before giving up.
const MAX_PASSES: usize = 40;

/// Narrowing passes at most.
const NARROW_PASSES: usize = 6;

/// A join point widens after this many changes.
const WIDEN_AFTER: u32 = 2;

/// Struct fields holding an element count, by `(StructId, field)`: the
/// most it can be. A list of `T` holds at most `isize::MAX / size_of(T)`
/// of them (its buffer is one object), and its length is never more.
/// `(StructId, AXES)`: an array's axis lengths, its `usize` array elements
/// (the core library keeps each at most `isize::MAX`).
pub type Bounds = HashMap<(u32, u32), i128>;

/// The field of `Bounds` naming an array's axis lengths.
const AXES: u32 = u32::MAX;

struct Cx<'a> {
    f: &'a MirFunction,
    pw: u32,
    bounds: &'a Bounds,
    /// A wrapping step does not wrap: a counter widened to its type's
    /// top stays at or above its start (a wrap would make it any
    /// value, and no narrowing gets the bottom back). Kept only if the
    /// solution shows no step wraps (`wraps`).
    hopeful: bool,
    /// Integer locals tracked, by type.
    ints: HashMap<LocalId, PrimTy>,
    /// Widening stops, ascending.
    stops: Vec<i128>,
    /// Block index by id.
    idx: HashMap<u32, usize>,
    /// Array locals holding an array's axis lengths (`Bounds`, `AXES`):
    /// assigned only from such a field.
    axes: HashMap<LocalId, i128>,
}

impl Cx<'_> {
    const fn range(&self, p: PrimTy) -> Iv {
        int_range(p, self.pw).expect("integer type")
    }

    /// The interval of `o` as a `p`, if known.
    fn iv(&self, env: &Env, o: &Operand, p: PrimTy) -> Option<Iv> {
        match o {
            Operand::Const(c) => int_val(c, p, self.pw).map(|v| (v, v)),
            Operand::Copy(l) if self.ints.get(l) == Some(&p) => {
                Some(env.get(l).copied().unwrap_or_else(|| self.range(p)))
            }
            Operand::Copy(_) => None,
        }
    }

    fn set(&self, env: &mut Env, l: LocalId, iv: Option<Iv>) {
        let Some(&p) = self.ints.get(&l) else {
            env.remove(&l);
            return;
        };
        let (lo, hi) = self.range(p);
        match iv {
            Some((a, b)) if a >= lo && b <= hi && !(a == lo && b == hi) => {
                env.insert(l, (a, b));
            }
            _ => {
                env.remove(&l);
            }
        }
    }

    /// The exact result interval of `a op b` (unbounded by the type).
    fn exact(op: PrimOp, a: Iv, b: Iv) -> Option<Iv> {
        let corners = |g: fn(i128, i128) -> Option<i128>| {
            let vs = [g(a.0, b.0)?, g(a.0, b.1)?, g(a.1, b.0)?, g(a.1, b.1)?];
            Some((*vs.iter().min()?, *vs.iter().max()?))
        };
        match op {
            PrimOp::Add | PrimOp::WrappingAdd => {
                Some((a.0.checked_add(b.0)?, a.1.checked_add(b.1)?))
            }
            PrimOp::Sub | PrimOp::WrappingSub => {
                Some((a.0.checked_sub(b.1)?, a.1.checked_sub(b.0)?))
            }
            PrimOp::Mul | PrimOp::WrappingMul => corners(i128::checked_mul),
            _ => None,
        }
    }

    /// The interval of `rv`, the value of a local of type `p`.
    fn eval(&self, env: &Env, rv: &RValue, p: PrimTy) -> Option<Iv> {
        let (tlo, thi) = self.range(p);
        match rv {
            RValue::Use(o) => self.iv(env, o, p),
            RValue::HeapField {
                ty: MirTy::Struct(id, _),
                index,
                ..
            } if p.is_unsigned_int() => self.bounds.get(&(id.0, *index)).map(|&m| (0, m.min(thi))),
            RValue::ArrayIndex {
                array,
                elem_ty: MirTy::Prim(PrimTy::Usize),
                ..
            } if p == PrimTy::Usize => match &self.f.locals[array.0 as usize].ty {
                MirTy::Struct(id, _) => self.bounds.get(&(id.0, AXES)).map(|&m| (0, m.min(thi))),
                _ => self.axes.get(array).map(|&m| (0, m.min(thi))),
            },
            RValue::BinOp {
                op,
                op_ty: MirTy::Prim(q),
                lhs,
                rhs,
            } if *q == p => {
                let a = self.iv(env, lhs, p)?;
                let b = self.iv(env, rhs, p)?;
                match op {
                    // Past a checked operation, its result fit.
                    PrimOp::Add | PrimOp::Sub | PrimOp::Mul => {
                        let (lo, hi) = Self::exact(*op, a, b).unwrap_or((tlo, thi));
                        Some((lo.max(tlo), hi.min(thi)))
                    }
                    PrimOp::WrappingAdd | PrimOp::WrappingSub | PrimOp::WrappingMul => {
                        match Self::exact(*op, a, b) {
                            Some((lo, hi)) if lo >= tlo && hi <= thi => Some((lo, hi)),
                            // Assumed not to wrap (`hopeful`): checked once
                            // the solution stands.
                            Some((lo, hi)) if self.hopeful => Some((lo.max(tlo), hi.min(thi))),
                            _ => None,
                        }
                    }
                    PrimOp::BitAnd => match (a.0 >= 0, b.0 >= 0) {
                        (true, true) => Some((0, a.1.min(b.1))),
                        (true, false) => Some((0, a.1)),
                        (false, true) => Some((0, b.1)),
                        (false, false) => None,
                    },
                    PrimOp::Shr
                        if b.0 == b.1 && (0..i128::from(p.bit_width(self.pw))).contains(&b.0) =>
                    {
                        Some((a.0 >> b.0, a.1 >> b.0))
                    }
                    PrimOp::Div if a.0 >= 0 && b.0 == b.1 && b.0 > 0 => {
                        Some((a.0 / b.0, a.1 / b.0))
                    }
                    PrimOp::Rem if a.0 >= 0 && b.0 == b.1 && b.0 > 0 => Some((0, a.1.min(b.0 - 1))),
                    _ => None,
                }
            }
            RValue::Cast {
                source,
                from_ty: MirTy::Prim(q),
                ..
            } => {
                let a = match q {
                    PrimTy::Bool => match source {
                        Operand::Const(ConstVal::Bool(b)) => (i128::from(*b), i128::from(*b)),
                        _ => (0, 1),
                    },
                    q if q.is_signed_int() || q.is_unsigned_int() => self.iv(env, source, *q)?,
                    _ => return None,
                };
                (a.0 >= tlo && a.1 <= thi).then_some(a)
            }
            _ => None,
        }
    }

    /// `env` after statement `s`.
    fn step(&self, env: &mut Env, s: &Statement) {
        // Past an array access its bounds check held: the index is
        // below the length.
        if let Statement::Assign {
            rvalue: RValue::ArrayIndex { index, len, .. },
            ..
        }
        | Statement::ArrayWrite { index, len, .. } = s
        {
            if let Some(&p) = match index {
                Operand::Copy(l) => self.ints.get(l),
                Operand::Const(_) => None,
            } {
                // Empty: the check always fails, nothing after runs.
                let _ = self.narrow(env, index, PrimOp::Ge, (0, 0), p);
                let _ = self.narrow(env, index, PrimOp::Lt, (0, i128::from(*len)), p);
            }
        }
        match s {
            Statement::Assign { target, rvalue } => {
                let iv = self
                    .ints
                    .get(target)
                    .and_then(|&p| self.eval(env, rvalue, p));
                self.set(env, *target, iv);
            }
            Statement::StorageDead(l) | Statement::ArrayWrite { array: l, .. } => {
                env.remove(l);
            }
            _ => {}
        }
    }

    /// `x op b` holds: narrow `x`. `None`: it cannot hold.
    fn narrow(&self, env: &mut Env, x: &Operand, op: PrimOp, b: Iv, p: PrimTy) -> Option<()> {
        let Operand::Copy(l) = x else {
            return Some(());
        };
        let Some(a) = self.iv(env, x, p) else {
            return Some(());
        };
        let (mut lo, mut hi) = a;
        match op {
            PrimOp::Lt => hi = hi.min(b.1 - 1),
            PrimOp::Le => hi = hi.min(b.1),
            PrimOp::Gt => lo = lo.max(b.0 + 1),
            PrimOp::Ge => lo = lo.max(b.0),
            PrimOp::Eq => (lo, hi) = (lo.max(b.0), hi.min(b.1)),
            PrimOp::Ne if b.0 == b.1 => {
                if lo == b.0 {
                    lo += 1;
                } else if hi == b.0 {
                    hi -= 1;
                }
            }
            _ => {}
        }
        if lo > hi {
            return None;
        }
        self.set(env, *l, Some((lo, hi)));
        Some(())
    }

    /// The env along edge `k` of block `b` (out env `out`); `None`: the
    /// edge is never taken.
    fn edge(&self, b: usize, k: usize, out: &Env) -> Option<Env> {
        let bb = &self.f.blocks[b];
        let mut env = out.clone();
        let (target, args) = match &bb.terminator {
            Terminator::Goto { target, args } => (*target, args),
            Terminator::If {
                cond,
                then_bb,
                then_args,
                else_bb,
                else_args,
            } => {
                if then_bb != else_bb {
                    if let Some((op, p, l, r)) = self.cmp(b, cond) {
                        let op = if k == 0 {
                            op
                        } else {
                            crate::opt::simplify::negate(op)
                        };
                        if let (Some(li), Some(ri)) = (self.iv(&env, l, p), self.iv(&env, r, p)) {
                            self.narrow(&mut env, l, op, ri, p)?;
                            self.narrow(&mut env, r, crate::opt::simplify::swap(op), li, p)?;
                        }
                    }
                }
                if k == 0 {
                    (*then_bb, then_args)
                } else {
                    (*else_bb, else_args)
                }
            }
            Terminator::Suspend { resume, .. } => (*resume, &Vec::new()),
            Terminator::Return(_) | Terminator::Unreachable => return None,
        };
        let to = &self.f.blocks[*self.idx.get(&target.0)?];
        let vals: Vec<Option<Iv>> = to
            .params
            .iter()
            .zip(args.iter())
            .map(|(p, a)| self.ints.get(p).and_then(|&t| self.iv(&env, a, t)))
            .collect();
        for (p, v) in to.params.iter().zip(vals) {
            self.set(&mut env, *p, v);
        }
        Some(env)
    }

    /// The integer comparison `cond` (computed in block `b`, nothing it
    /// reads assigned after) is.
    fn cmp<'b>(
        &'b self,
        b: usize,
        cond: &Operand,
    ) -> Option<(PrimOp, PrimTy, &'b Operand, &'b Operand)> {
        let Operand::Copy(c) = cond else {
            return None;
        };
        let bb = &self.f.blocks[b];
        let (i, op, p, l, r) = bb
            .stmts
            .iter()
            .enumerate()
            .rev()
            .find_map(|(i, s)| match s {
                Statement::Assign { target, rvalue } if target == c => Some((i, rvalue)),
                _ => None,
            })
            .and_then(|(i, rv)| match rv {
                RValue::BinOp {
                    op,
                    op_ty: MirTy::Prim(p),
                    lhs,
                    rhs,
                } if matches!(
                    op,
                    PrimOp::Lt | PrimOp::Le | PrimOp::Gt | PrimOp::Ge | PrimOp::Eq | PrimOp::Ne
                ) && (p.is_signed_int() || p.is_unsigned_int()) =>
                {
                    Some((i, *op, *p, lhs, rhs))
                }
                _ => None,
            })?;
        let read = |x: LocalId| {
            [l, r]
                .iter()
                .any(|o| matches!(o, Operand::Copy(y) if *y == x))
        };
        let later = bb.stmts[i + 1..].iter().any(|s| match s {
            Statement::Assign { target, .. } => *target == *c || read(*target),
            Statement::StorageDead(x) | Statement::ArrayWrite { array: x, .. } => read(*x),
            _ => false,
        });
        (!later).then_some((op, p, l, r))
    }

    /// `old` grown to take in `new`, past the next stop where it grew.
    fn widen(&self, l: LocalId, old: Iv, new: Iv) -> Iv {
        let (tlo, thi) = self.range(self.ints[&l]);
        let lo = if new.0 < old.0 {
            self.stops
                .iter()
                .rev()
                .find(|&&s| s <= new.0)
                .copied()
                .unwrap_or(tlo)
                .max(tlo)
        } else {
            old.0
        };
        let hi = if new.1 > old.1 {
            self.stops
                .iter()
                .find(|&&s| s >= new.1)
                .copied()
                .unwrap_or(thi)
                .min(thi)
        } else {
            old.1
        };
        (lo, hi)
    }
}

fn join(a: &Env, b: &Env) -> Env {
    a.iter()
        .filter_map(|(l, x)| b.get(l).map(|y| (*l, (x.0.min(y.0), x.1.max(y.1)))))
        .collect()
}

/// Decides comparisons and drops overflow checks; whether it changed
/// anything.
pub fn run(f: &mut MirFunction, pw: u32, bounds: &Bounds) -> bool {
    let n = f.blocks.len();
    if n == 0 || n > MAX_BLOCKS {
        return false;
    }
    // Nothing it could rewrite.
    let some = f.blocks.iter().flat_map(|b| &b.stmts).any(|s| {
        matches!(s, Statement::Assign {
            rvalue: RValue::BinOp { op, op_ty: MirTy::Prim(p), .. },
            ..
        } if (p.is_signed_int() || p.is_unsigned_int())
            && matches!(
                op,
                PrimOp::Add
                    | PrimOp::Sub
                    | PrimOp::Mul
                    | PrimOp::Lt
                    | PrimOp::Le
                    | PrimOp::Gt
                    | PrimOp::Ge
                    | PrimOp::Eq
                    | PrimOp::Ne
            ))
    });
    if !some {
        return false;
    }
    let Some((cx, entry)) = solved(f, pw, bounds) else {
        return false;
    };
    let edits = rewrite(&cx, &entry);
    let changed = !edits.is_empty();
    drop(cx);
    for (b, i, rv) in edits {
        if let Statement::Assign { rvalue, .. } = &mut f.blocks[b].stmts[i] {
            *rvalue = rv;
        }
    }
    changed
}

/// Whether every array index of `f` that is not a constant is known to
/// be below its array's length, where the statement may run: codegen's
/// bounds checks there never fail.
pub fn indexes_fit(f: &MirFunction, pw: u32) -> bool {
    if f.blocks.is_empty() || f.blocks.len() > MAX_BLOCKS {
        return false;
    }
    let bounds = Bounds::default();
    let Some((cx, entry)) = solved(f, pw, &bounds) else {
        return false;
    };
    let fits = |env: &Env, index: &Operand, len: u32| match index {
        Operand::Copy(l) => cx
            .ints
            .get(l)
            .and_then(|&p| cx.iv(env, index, p))
            .is_some_and(|(lo, hi)| lo >= 0 && hi < i128::from(len)),
        Operand::Const(_) => true,
    };
    for (b, bb) in f.blocks.iter().enumerate() {
        let Some(e) = &entry[b] else { continue };
        let mut env = e.clone();
        for s in &bb.stmts {
            let ok = match s {
                Statement::Assign {
                    rvalue: RValue::ArrayIndex { index, len, .. },
                    ..
                }
                | Statement::ArrayWrite { index, len, .. } => fits(&env, index, *len),
                _ => true,
            };
            if !ok {
                return false;
            }
            cx.step(&mut env, s);
        }
    }
    true
}

/// The ranges of `f` and the entry envs they solve to.
fn solved<'a>(
    f: &'a MirFunction,
    pw: u32,
    bounds: &'a Bounds,
) -> Option<(Cx<'a>, Vec<Option<Env>>)> {
    let pinned = crate::opt::pinned(f);
    let ints: HashMap<LocalId, PrimTy> = f
        .locals
        .iter()
        .filter_map(|d| match d.ty {
            MirTy::Prim(p)
                if (p.is_signed_int() || p.is_unsigned_int()) && !pinned.contains(&d.id) =>
            {
                Some((d.id, p))
            }
            _ => None,
        })
        .collect();
    let mut stops: HashSet<i128> = HashSet::default();
    for s in f.blocks.iter().flat_map(|b| &b.stmts) {
        if let Statement::Assign {
            rvalue:
                RValue::BinOp {
                    op: PrimOp::Lt | PrimOp::Le | PrimOp::Gt | PrimOp::Ge,
                    op_ty: MirTy::Prim(p),
                    lhs,
                    rhs,
                },
            ..
        } = s
        {
            for o in [lhs, rhs] {
                if let Operand::Const(c) = o {
                    if let Some(v) = int_val(c, *p, pw) {
                        stops.extend([v - 1, v, v + 1]);
                    }
                }
            }
        }
    }
    let mut stops: Vec<i128> = stops.into_iter().collect();
    stops.sort_unstable();
    let axes = axes(f, bounds);
    let mut cx = Cx {
        f,
        pw,
        bounds,
        hopeful: true,
        ints,
        stops,
        idx: crate::opt::index(f),
        axes,
    };
    let mut entry = solve(&cx).filter(|e| !wraps(&cx, e));
    if entry.is_none() {
        cx.hopeful = false;
        entry = solve(&cx);
    }
    let entry = entry?;
    Some((cx, entry))
}

/// Array locals of `f` assigned only an array's axis lengths (a field of
/// `usize` elements of a struct `bounds` names with `AXES`), never
/// written in place: the most each element can be.
fn axes(f: &MirFunction, bounds: &Bounds) -> HashMap<LocalId, i128> {
    let mut out: HashMap<LocalId, Option<i128>> = HashMap::default();
    let mut put = |l: LocalId, v: Option<i128>| {
        let e = out.entry(l).or_insert(v);
        *e = match (*e, v) {
            (Some(a), Some(b)) => Some(a.max(b)),
            _ => None,
        };
    };
    for b in &f.blocks {
        for p in &b.params {
            put(*p, None);
        }
        for s in &b.stmts {
            match s {
                Statement::Assign {
                    target,
                    rvalue:
                        RValue::FieldRead {
                            target: Operand::Copy(x),
                            field_ty: MirTy::Array(e, _),
                            ..
                        },
                } if **e == MirTy::Prim(PrimTy::Usize) => {
                    let v = match &f.locals[x.0 as usize].ty {
                        MirTy::Struct(id, _) => bounds.get(&(id.0, AXES)).copied(),
                        _ => None,
                    };
                    put(*target, v);
                }
                Statement::Assign { target, .. } => put(*target, None),
                Statement::ArrayWrite { array, .. } => put(*array, None),
                _ => {}
            }
        }
    }
    for p in &f.params {
        put(*p, None);
    }
    let addr = crate::addressed(f);
    out.into_iter()
        .filter(|(l, _)| !addr.contains(l))
        .filter_map(|(l, v)| Some((l, v?)))
        .collect()
}

/// Entry envs per block (`None`: never reached), at a fixed point.
fn solve(cx: &Cx<'_>) -> Option<Vec<Option<Env>>> {
    let f = cx.f;
    let n = f.blocks.len();
    let idx = crate::opt::index(f);
    let order = crate::opt::rpo(f, &idx);
    // Without a cycle one pass in order is exact: no widening, nothing
    // to narrow.
    let loops = crate::opt::cyclic(f, &idx, &order);
    let mut entry: Vec<Option<Env>> = vec![None; n];
    let mut grew = vec![0u32; n];
    entry[0] = Some(Env::default());
    let back = if loops {
        back_edges(f, &idx)
    } else {
        HashMap::default()
    };
    let held = |b: usize, j: usize, e: &mut Env, from: Option<&Env>| held(&back, b, j, e, from);
    let out = |b: usize, env: &Env| {
        let mut e = env.clone();
        for s in &f.blocks[b].stmts {
            cx.step(&mut e, s);
        }
        e
    };
    let mut done = false;
    for _ in 0..MAX_PASSES {
        let mut changed = false;
        for &b in &order {
            let Some(inn) = &entry[b] else { continue };
            let o = out(b, inn);
            let ss = crate::opt::succs(&f.blocks[b].terminator);
            for (k, s) in ss.iter().enumerate() {
                let Some(&j) = idx.get(&s.0) else { continue };
                let Some(mut e) = cx.edge(b, k, &o) else {
                    continue;
                };
                if entry[j].is_some() {
                    held(b, j, &mut e, entry[j].as_ref());
                }
                let new = match &entry[j] {
                    None => e,
                    Some(old) => {
                        let mut m = join(old, &e);
                        if m == *old {
                            continue;
                        }
                        grew[j] += 1;
                        if loops && grew[j] > WIDEN_AFTER {
                            for (l, v) in &mut m {
                                *v = cx.widen(*l, old[l], *v);
                            }
                        }
                        m
                    }
                };
                if entry[j].as_ref() != Some(&new) {
                    entry[j] = Some(new);
                    changed = true;
                }
            }
        }
        if !changed || !loops {
            done = true;
            break;
        }
    }
    if !loops {
        return Some(entry);
    }
    if !done {
        return None;
    }
    // Narrow: recompute each entry from its predecessors, in order and
    // in place. Started at a fixed point, every step stays one, above
    // the least.
    let preds = crate::opt::preds(f, &idx);
    for _ in 0..NARROW_PASSES {
        let mut changed = false;
        for &j in order.iter().skip(1) {
            let mut acc: Option<Env> = None;
            let mut seen: HashSet<usize> = HashSet::default();
            // Back edges last: they take the invariants from the rest.
            let mut ps: Vec<usize> = preds[j].clone();
            ps.sort_by_key(|&p| back.contains_key(&(p, j)));
            for p in ps {
                if !seen.insert(p) {
                    continue;
                }
                let Some(inn) = &entry[p] else { continue };
                let o = out(p, inn);
                for (k, s) in crate::opt::succs(&f.blocks[p].terminator)
                    .iter()
                    .enumerate()
                {
                    if s.0 != f.blocks[j].id.0 {
                        continue;
                    }
                    if let Some(mut e) = cx.edge(p, k, &o) {
                        held(p, j, &mut e, acc.as_ref());
                        acc = Some(match acc {
                            None => e,
                            Some(a) => join(&a, &e),
                        });
                    }
                }
            }
            if entry[j] != acc {
                entry[j] = acc;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    Some(entry)
}

/// Per back edge `(latch, header)` (block indices): the locals its loop
/// assigns. Any other holds on the back edge what it held entering the
/// header, so the edge adds nothing to it (else a loop-invariant bound
/// widened once would stay wide: the edge would feed it back).
fn back_edges(
    f: &MirFunction,
    idx: &HashMap<u32, usize>,
) -> HashMap<(usize, usize), HashSet<LocalId>> {
    let mut back = HashMap::default();
    let edges = crate::rotate::back_edges(f);
    if edges.is_empty() {
        return back;
    }
    let preds = crate::rotate::preds(f);
    for (latch, hs) in edges {
        for h in hs {
            let mut set: HashSet<LocalId> = HashSet::default();
            for id in crate::rotate::body_of(&preds, h, &[latch]) {
                let bb = &f.blocks[idx[&id]];
                set.extend(bb.params.iter().copied());
                for s in &bb.stmts {
                    match s {
                        Statement::Assign { target: l, .. }
                        | Statement::StorageDead(l)
                        | Statement::ArrayWrite { array: l, .. } => {
                            set.insert(*l);
                        }
                        _ => {}
                    }
                }
            }
            back.insert((idx[&latch], idx[&h]), set);
        }
    }
    back
}

/// `e` along edge `b -> j`: on a back edge, the locals its loop leaves
/// alone as `from` (what the other edges bring) holds them.
fn held(
    back: &HashMap<(usize, usize), HashSet<LocalId>>,
    b: usize,
    j: usize,
    e: &mut Env,
    from: Option<&Env>,
) {
    if let Some(set) = back.get(&(b, j)) {
        e.retain(|l, v| {
            if set.contains(l) {
                return true;
            }
            match from.and_then(|o| o.get(l)) {
                Some(x) => {
                    *v = *x;
                    true
                }
                None => false,
            }
        });
    }
}

/// Whether a wrapping step may wrap under `entry`, where `hopeful`
/// assumed it would not: the assumption is then wrong.
fn wraps(cx: &Cx<'_>, entry: &[Option<Env>]) -> bool {
    for (b, bb) in cx.f.blocks.iter().enumerate() {
        let Some(e) = &entry[b] else { continue };
        let mut env = e.clone();
        for s in &bb.stmts {
            if let Statement::Assign {
                rvalue:
                    RValue::BinOp {
                        op: op @ (PrimOp::WrappingAdd | PrimOp::WrappingSub | PrimOp::WrappingMul),
                        op_ty: MirTy::Prim(p),
                        lhs,
                        rhs,
                    },
                ..
            } = s
            {
                if let (Some(a), Some(c)) = (cx.iv(&env, lhs, *p), cx.iv(&env, rhs, *p)) {
                    let (tlo, thi) = cx.range(*p);
                    if let Some((lo, hi)) = Cx::exact(*op, a, c) {
                        let clamped = (lo.max(tlo), hi.min(thi));
                        if (lo < tlo || hi > thi) && clamped != (tlo, thi) {
                            return true;
                        }
                    }
                }
            }
            cx.step(&mut env, s);
        }
    }
    false
}

/// The statements to rewrite: `(block, statement, new rvalue)`.
fn rewrite(cx: &Cx<'_>, entry: &[Option<Env>]) -> Vec<(usize, usize, RValue)> {
    let mut edits = Vec::new();
    for (b, bb) in cx.f.blocks.iter().enumerate() {
        let Some(e) = &entry[b] else { continue };
        let mut env = e.clone();
        for (i, s) in bb.stmts.iter().enumerate() {
            if let Statement::Assign {
                rvalue:
                    RValue::BinOp {
                        op,
                        op_ty: MirTy::Prim(p),
                        lhs,
                        rhs,
                    },
                ..
            } = s
            {
                let p = *p;
                let known = (p.is_signed_int() || p.is_unsigned_int())
                    .then(|| cx.iv(&env, lhs, p).zip(cx.iv(&env, rhs, p)))
                    .flatten();
                if let Some((a, c)) = known {
                    let (tlo, thi) = cx.range(p);
                    let wrap = match op {
                        PrimOp::Add => Some(PrimOp::WrappingAdd),
                        PrimOp::Sub => Some(PrimOp::WrappingSub),
                        PrimOp::Mul => Some(PrimOp::WrappingMul),
                        _ => None,
                    };
                    if let Some(w) = wrap {
                        if Cx::exact(*op, a, c).is_some_and(|(lo, hi)| lo >= tlo && hi <= thi) {
                            edits.push((
                                b,
                                i,
                                RValue::BinOp {
                                    op: w,
                                    op_ty: MirTy::Prim(p),
                                    lhs: lhs.clone(),
                                    rhs: rhs.clone(),
                                },
                            ));
                        }
                    } else if let Some(v) = decide(*op, a, c) {
                        edits.push((b, i, RValue::Use(Operand::Const(ConstVal::Bool(v)))));
                    } else if let Some(rv) = by_two(*op, p, lhs, a, c) {
                        edits.push((b, i, rv));
                    }
                }
            }
            cx.step(&mut env, s);
        }
    }
    edits
}

/// `x / 2^k` and `x % 2^k` of an `x` never negative: a shift and a
/// mask (a signed division rounds toward zero, which takes more).
fn by_two(op: PrimOp, p: PrimTy, x: &Operand, a: Iv, c: Iv) -> Option<RValue> {
    let d = c.0;
    if c.0 != c.1 || a.0 < 0 || d < 2 || d.count_ones() != 1 {
        return None;
    }
    let (op, k) = match op {
        PrimOp::Div => (PrimOp::Shr, i128::from(d.trailing_zeros())),
        PrimOp::Rem => (PrimOp::BitAnd, d - 1),
        _ => return None,
    };
    Some(RValue::BinOp {
        op,
        op_ty: MirTy::Prim(p),
        lhs: x.clone(),
        rhs: Operand::Const(crate::opt::int_const(k, p)),
    })
}

/// The value of `a op b`, if the intervals decide it.
fn decide(op: PrimOp, a: Iv, b: Iv) -> Option<bool> {
    match op {
        PrimOp::Lt if a.1 < b.0 => Some(true),
        PrimOp::Lt if a.0 >= b.1 => Some(false),
        PrimOp::Le if a.1 <= b.0 => Some(true),
        PrimOp::Le if a.0 > b.1 => Some(false),
        PrimOp::Gt => decide(PrimOp::Lt, b, a),
        PrimOp::Ge => decide(PrimOp::Le, b, a),
        PrimOp::Eq if a.0 == a.1 && b.0 == b.1 && a.0 == b.0 => Some(true),
        PrimOp::Eq | PrimOp::Ne if a.1 < b.0 || b.1 < a.0 => Some(op == PrimOp::Ne),
        PrimOp::Ne => decide(PrimOp::Eq, a, b).map(|v| !v),
        _ => None,
    }
}

/// The `Bounds` of a module's structs: the length of a list of elements
/// whose size its type name tells (`raw::buf_len` keeps a capacity below
/// `isize::MAX` bytes).
pub fn bounds(m: &crate::MirModule) -> Bounds {
    let t = &m.target;
    let mut out = Bounds::default();
    let top = (1i128 << (t.pointer_width - 1)) - 1;
    for (id, key) in &m.struct_keys {
        if key.starts_with("core::array::Array<") {
            out.insert((*id, AXES), top);
            continue;
        }
        let Some(elem) = key
            .strip_prefix("core::collections::list::ListHead<")
            .and_then(|r| r.strip_suffix('>'))
        else {
            continue;
        };
        let Some((_, fields)) = m.struct_names.get(id) else {
            continue;
        };
        let p = match elem {
            "u8" => PrimTy::U8,
            "i8" => PrimTy::I8,
            "bool" => PrimTy::Bool,
            "u16" => PrimTy::U16,
            "i16" => PrimTy::I16,
            "u32" => PrimTy::U32,
            "i32" => PrimTy::I32,
            "f32" => PrimTy::F32,
            "char" => PrimTy::Char,
            "u64" => PrimTy::U64,
            "i64" => PrimTy::I64,
            "f64" => PrimTy::F64,
            "usize" => PrimTy::Usize,
            "isize" => PrimTy::Isize,
            "str" => PrimTy::Str,
            _ => continue,
        };
        let size = i128::from(t.size_align(&MirTy::Prim(p)).0);
        if size == 0 {
            continue;
        }
        let max = ((1i128 << (t.pointer_width - 1)) - 1) / size;
        if let Some(k) = fields.iter().position(|f| f == "len") {
            out.insert((*id, k as u32), max);
        }
    }
    out
}
