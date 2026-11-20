//! Unrolling of whole strips, at `-O3`. A strip loop entered for a whole
//! strip (`strip`'s `lim = ctr + SHORT_TRIPS`) runs its body exactly that
//! many times, but for a side exit: it need test its counter only once
//! every `n` iterations. Whole strips go to a copy of the loop with its
//! body `n` times in a row, the test in the last copy only; a short strip
//! runs the loop as it was.
//!
//! ```text
//! F1: lim = ctr + K; goto H        F1: lim = ctr + K; goto H1
//! H:  ..; ctr += 1                 H1: ..; ctr += 1; goto H2
//!     c = ctr < lim                H2: ..; ctr += 1; c = ctr < lim
//!     if c then H else S               if c then H1 else S
//! ```
//!
//! Only loops the vectorizer leaves scalar (a float sum, kept in order,
//! or a call of a body; the runtime's own it may take): a vector loop
//! does more per test already.

use crate::rotate::SHORT_TRIPS;
use crate::{BasicBlock, MirFunction, MirTy, Operand, RValue, Statement, Terminator};
use nabic_core::{HashMap, HashSet};
use nabic_ir_common::{BlockId, FunctionId, LocalId, PrimOp};

/// Largest body (in statements) copied four times, and twice.
const FOUR: usize = 16;
const TWO: usize = 32;

/// Unrolls the whole-strip runs of `f`'s strip loops; whether any.
/// `decls`: the functions without a body here.
pub fn run(f: &mut MirFunction, pw: u32, decls: &HashSet<FunctionId>) -> bool {
    let mut latches: HashMap<u32, Vec<u32>> = HashMap::default();
    for (l, hs) in crate::rotate::back_edges(f) {
        for h in hs {
            latches.entry(h).or_default().push(l);
        }
    }
    let preds = crate::rotate::preds(f);
    let mut todo = Vec::new();
    let mut seen = HashSet::default();
    for &h in &f.strips {
        if !seen.insert(h) {
            continue;
        }
        let Some([l]) = latches.get(&h).map(Vec::as_slice) else {
            continue;
        };
        let body = crate::rotate::body_of(&preds, h, &[*l]);
        if body.iter().any(|b| *b != h && latches.contains_key(b)) {
            continue;
        }
        if let Some(p) = plan(f, &preds, h, *l, &body, pw, decls) {
            todo.push(p);
        }
    }
    let any = !todo.is_empty();
    for p in todo {
        apply(f, &p);
    }
    any
}

/// A loop to unroll: header, latch, its blocks in `f`'s order, the
/// whole-strip entry, the test's local, copies.
struct Plan {
    h: u32,
    l: u32,
    body: Vec<u32>,
    entry: u32,
    cond: LocalId,
    n: usize,
    /// Locals of one iteration: each copy gets its own.
    temps: Vec<LocalId>,
}

fn plan(
    f: &MirFunction,
    preds: &HashMap<u32, Vec<u32>>,
    h: u32,
    l: u32,
    body: &HashSet<u32>,
    pw: u32,
    decls: &HashSet<FunctionId>,
) -> Option<Plan> {
    let block = |id: u32| f.blocks.iter().find(|b| b.id.0 == id);
    let hb = block(h)?;
    let lb = block(l)?;
    if !hb.params.is_empty() {
        return None;
    }
    // The latch: `ctr += 1; c = ctr < lim; if c then H else S`.
    let Terminator::If {
        cond: Operand::Copy(c),
        then_bb,
        then_args,
        else_bb,
        ..
    } = &lb.terminator
    else {
        return None;
    };
    if then_bb.0 != h || !then_args.is_empty() || body.contains(&else_bb.0) {
        return None;
    }
    let real: Vec<&Statement> = lb.stmts.iter().filter(|s| !crate::opt::marker(s)).collect();
    let [.., step, test] = real.as_slice() else {
        return None;
    };
    let Statement::Assign {
        target,
        rvalue:
            RValue::BinOp {
                op: PrimOp::Lt,
                op_ty: MirTy::Prim(ty),
                lhs: Operand::Copy(ctr),
                rhs: Operand::Copy(lim),
            },
    } = test
    else {
        return None;
    };
    if target != c || !ty.is_int() {
        return None;
    }
    let one =
        |o: &Operand| matches!(o, Operand::Const(k) if crate::opt::int_val(k, *ty, pw) == Some(1));
    match step {
        Statement::Assign {
            target,
            rvalue:
                RValue::BinOp {
                    op: PrimOp::Add | PrimOp::WrappingAdd,
                    lhs: Operand::Copy(x),
                    rhs,
                    ..
                },
        } if target == ctr && x == ctr && one(rhs) => {}
        _ => return None,
    }
    // The counter stepped there only, the limit never written, the test
    // read by the branch alone.
    let pinned = crate::addressed(f);
    if pinned.contains(ctr) || pinned.contains(lim) || pinned.contains(c) {
        return None;
    }
    let mut size = 0;
    let mut blocker = false;
    let mut order = Vec::new();
    for b in f.blocks.iter().filter(|b| body.contains(&b.id.0)) {
        order.push(b.id.0);
        if b.id.0 != h && !b.params.is_empty() {
            return None;
        }
        for s in &b.stmts {
            if crate::opt::marker(s) {
                continue;
            }
            size += 1;
            blocker |= scalar(s, decls);
            if let Statement::Assign { target, .. } = s {
                let mine = std::ptr::eq(s, *step) || std::ptr::eq(s, *test);
                if !mine && [*ctr, *lim, *c].contains(target) {
                    return None;
                }
            }
            let mut read_c = false;
            crate::stmt_reads(s, &mut |x| read_c |= x == *c);
            if read_c {
                return None;
            }
        }
        if b.id.0 != l {
            let mut read_c = false;
            crate::term_reads(&b.terminator, &mut |x| read_c |= x == *c);
            if read_c {
                return None;
            }
        }
    }
    if !blocker || size > TWO {
        return None;
    }
    let n = if size <= FOUR { 4 } else { 2 };
    if SHORT_TRIPS % n as i128 != 0 {
        return None;
    }
    // Entered for a whole strip: `lim = ctr + SHORT_TRIPS; goto H`.
    let entry = preds.get(&h)?.iter().copied().find(|&p| {
        let Some(pb) = block(p) else { return false };
        let real: Vec<&Statement> = pb.stmts.iter().filter(|s| !crate::opt::marker(s)).collect();
        !body.contains(&p)
            && matches!(&pb.terminator, Terminator::Goto { target, args } if target.0 == h && args.is_empty())
            && matches!(real.as_slice(), [Statement::Assign {
                target,
                rvalue: RValue::BinOp {
                    op: PrimOp::Add | PrimOp::WrappingAdd,
                    lhs: Operand::Copy(x),
                    rhs: Operand::Const(k),
                    ..
                },
            }] if target == lim && x == ctr && crate::opt::int_val(k, *ty, pw) == Some(SHORT_TRIPS))
    })?;
    let temps = temps(f, body, &[*ctr, *lim, *c]);
    Some(Plan {
        h,
        l,
        body: order,
        entry,
        cond: *c,
        n,
        temps,
    })
}

/// The unnamed locals of loop `body` (but `keep`) written before they
/// are read, all in one block of it: what one iteration computes and
/// drops. A copy with its own keeps them apart for the register
/// allocator.
fn temps(f: &MirFunction, body: &HashSet<u32>, keep: &[LocalId]) -> Vec<LocalId> {
    // Per local: its block, and whether it is a temporary there so far.
    let mut at: HashMap<LocalId, (u32, bool)> = HashMap::default();
    let mut out: HashSet<LocalId> = HashSet::default();
    for b in &f.blocks {
        let id = b.id.0;
        let see = |x: LocalId, write: bool, at: &mut HashMap<LocalId, (u32, bool)>| match at
            .get_mut(&x)
        {
            None => {
                at.insert(x, (id, write && body.contains(&id)));
            }
            Some((blk, ok)) => *ok &= *blk == id,
        };
        for &p in &b.params {
            see(p, false, &mut at);
        }
        for s in &b.stmts {
            match s {
                Statement::StorageDead(x) => see(*x, false, &mut at),
                Statement::Loc(..) => {}
                _ => {
                    crate::stmt_reads(s, &mut |x| see(x, false, &mut at));
                    if let Statement::Assign { target, .. } = s {
                        see(*target, true, &mut at);
                    }
                }
            }
        }
        crate::term_reads(&b.terminator, &mut |x| see(x, false, &mut at));
    }
    let pinned = crate::opt::pinned(f);
    // An inlined call's result keeps its own. (A variable shown as
    // another local's value shows the loop's copy, at `-O3`.)
    let debug: HashSet<LocalId> = f.dbg.decls.iter().map(|d| d.0).collect();
    for (x, (_, ok)) in at {
        // A word, never an aggregate a projection reads in place.
        let named = f
            .locals
            .get(x.0 as usize)
            .is_none_or(|d| d.name.is_some() || !matches!(d.ty, MirTy::Prim(_)));
        if ok && !named && !keep.contains(&x) && !pinned.contains(&x) && !debug.contains(&x) {
            out.insert(x);
        }
    }
    let mut v: Vec<LocalId> = out.into_iter().collect();
    v.sort_unstable_by_key(|x| x.0);
    v
}

/// Statement `s` keeps its loop scalar: a call of a body (not of the
/// runtime: a float `min` the vectorizer takes), a store or load it has
/// no lanes for, or a float sum (its order kept).
fn scalar(s: &Statement, decls: &HashSet<FunctionId>) -> bool {
    match s {
        Statement::Assign { target, rvalue } => match rvalue {
            RValue::Call { callee, .. } => !decls.contains(callee),
            RValue::CallIndirect { .. }
            | RValue::RawLoad { .. }
            | RValue::HeapLoad { .. }
            | RValue::StaticLoad { .. }
            | RValue::BufAtomic { .. } => true,
            RValue::BinOp {
                op: PrimOp::Add | PrimOp::Sub | PrimOp::Mul,
                op_ty: MirTy::Prim(p),
                lhs,
                rhs,
            } => p.is_float() && [lhs, rhs].contains(&&Operand::Copy(*target)),
            _ => false,
        },
        Statement::BufStore { .. } | Statement::Loc(..) | Statement::StorageDead(_) => false,
        _ => true,
    }
}

fn apply(f: &mut MirFunction, p: &Plan) {
    let mut next = crate::opt::fresh_block(f).0;
    let maps: Vec<HashMap<u32, u32>> = (0..p.n)
        .map(|_| {
            p.body
                .iter()
                .map(|&b| {
                    next += 1;
                    (b, next - 1)
                })
                .collect()
        })
        .collect();
    // Each copy its own temporaries, apart from the loop's.
    let mut names: Vec<HashMap<LocalId, LocalId>> = Vec::new();
    for _ in 0..p.n {
        let m = p
            .temps
            .iter()
            .map(|&x| {
                let ty = f.locals[x.0 as usize].ty.clone();
                (x, crate::opt::fresh_local(f, ty))
            })
            .collect();
        names.push(m);
    }
    let mut copies = Vec::new();
    for (k, map) in maps.iter().enumerate() {
        for &id in &p.body {
            let src = f
                .blocks
                .iter()
                .find(|b| b.id.0 == id)
                .expect("a body block");
            let mut b = BasicBlock {
                id: BlockId(map[&id]),
                params: src.params.clone(),
                stmts: src.stmts.clone(),
                terminator: src.terminator.clone(),
            };
            for s in crate::opt::succs_mut(&mut b.terminator) {
                if let Some(&to) = map.get(&s.0) {
                    *s = BlockId(to);
                }
            }
            let name = &names[k];
            if !name.is_empty() {
                let mut op = |o: &mut Operand| {
                    if let Operand::Copy(x) = o {
                        if let Some(&y) = name.get(x) {
                            *x = y;
                        }
                    }
                };
                for s in &mut b.stmts {
                    crate::opt::stmt_ops_mut(s, &mut op);
                    match s {
                        Statement::Assign { target: x, .. } | Statement::StorageDead(x) => {
                            if let Some(&y) = name.get(x) {
                                *x = y;
                            }
                        }
                        _ => {}
                    }
                }
                crate::opt::term_ops_mut(&mut b.terminator, &mut op);
            }
            if id == p.l {
                if k + 1 < p.n {
                    // Not the last copy: the test is known to hold.
                    let at = b
                        .stmts
                        .iter()
                        .rposition(
                            |s| matches!(s, Statement::Assign { target, .. } if *target == p.cond),
                        )
                        .expect("the test");
                    b.stmts.remove(at);
                    b.terminator = Terminator::Goto {
                        target: BlockId(maps[k + 1][&p.h]),
                        args: Vec::new(),
                    };
                } else if let Terminator::If { then_bb, .. } = &mut b.terminator {
                    *then_bb = BlockId(maps[0][&p.h]);
                }
            }
            copies.push(b);
        }
    }
    f.blocks.extend(copies);
    let first = maps[0][&p.h];
    if let Some(e) = f.blocks.iter_mut().find(|b| b.id.0 == p.entry) {
        crate::rotate::retarget(&mut e.terminator, p.h, first);
    }
    f.strips.push(first);
}

#[cfg(test)]
mod tests {
    use super::*;
    use nabic_ir_common::{ConstVal, FunctionId, PrimTy};

    const fn local(n: u32) -> LocalId {
        LocalId(n)
    }

    const fn assign(t: u32, rvalue: RValue) -> Statement {
        Statement::Assign {
            target: local(t),
            rvalue,
        }
    }

    const fn bin(op: PrimOp, ty: PrimTy, lhs: Operand, rhs: Operand) -> RValue {
        RValue::BinOp {
            op,
            op_ty: MirTy::Prim(ty),
            lhs,
            rhs,
        }
    }

    const fn goto(t: u32) -> Terminator {
        Terminator::Goto {
            target: BlockId(t),
            args: Vec::new(),
        }
    }

    // _0: ctr, _1: lim, _2: bound, _3: test, _4: sum (f64), _5: x
    fn strip_loop() -> MirFunction {
        let u = PrimTy::Usize;
        let c = |v: u128| {
            Operand::Const(ConstVal::UInt {
                value: v,
                ty: PrimTy::Usize,
            })
        };
        let cp = |n: u32| Operand::Copy(local(n));
        let tys = [u, u, u, PrimTy::Bool, PrimTy::F64, PrimTy::F64];
        let blocks = vec![
            BasicBlock {
                id: BlockId(0),
                params: vec![],
                stmts: vec![assign(1, RValue::Use(cp(2)))],
                terminator: Terminator::If {
                    cond: Operand::Const(ConstVal::Bool(true)),
                    then_bb: BlockId(1),
                    then_args: vec![],
                    else_bb: BlockId(2),
                    else_args: vec![],
                },
            },
            BasicBlock {
                id: BlockId(1),
                params: vec![],
                stmts: vec![assign(1, bin(PrimOp::WrappingAdd, u, cp(0), c(64)))],
                terminator: goto(2),
            },
            BasicBlock {
                id: BlockId(2),
                params: vec![],
                stmts: vec![
                    assign(4, bin(PrimOp::Add, PrimTy::F64, cp(4), cp(5))),
                    assign(0, bin(PrimOp::WrappingAdd, u, cp(0), c(1))),
                    assign(3, bin(PrimOp::Lt, u, cp(0), cp(1))),
                ],
                terminator: Terminator::If {
                    cond: cp(3),
                    then_bb: BlockId(2),
                    then_args: vec![],
                    else_bb: BlockId(3),
                    else_args: vec![],
                },
            },
            BasicBlock {
                id: BlockId(3),
                params: vec![],
                stmts: vec![],
                terminator: Terminator::Return(None),
            },
        ];
        MirFunction {
            id: FunctionId(0),
            def_id: None,
            name: "f".into(),
            params: vec![local(0), local(2), local(4), local(5)],
            locals: tys
                .iter()
                .enumerate()
                .map(|(i, t)| crate::LocalDecl {
                    id: local(i as u32),
                    name: None,
                    ty: MirTy::Prim(*t),
                })
                .collect(),
            blocks,
            param_tys: vec![],
            ret_ty: MirTy::Unit,
            span: nabic_diagnostic::Span::DUMMY,
            facts: nabic_ir_common::FnFacts::default(),
            strips: vec![2],
            pins: Vec::new(),
            dbg: crate::FnDbg::default(),
        }
    }

    #[test]
    fn a_float_sum_runs_whole_strips_four_at_a_time() {
        let mut f = strip_loop();
        assert!(run(&mut f, 64, &HashSet::default()));
        // Four copies after the four blocks; the entry goes to the first.
        assert_eq!(f.blocks.len(), 8);
        let to = |b: &BasicBlock| match b.terminator {
            Terminator::Goto { target, .. } => Some(target.0),
            _ => None,
        };
        assert_eq!(to(&f.blocks[1]), Some(4));
        for k in 0..3 {
            let b = &f.blocks[4 + k];
            assert_eq!(b.stmts.len(), 2);
            assert_eq!(to(b), Some(5 + k as u32));
        }
        let last = &f.blocks[7];
        assert_eq!(last.stmts.len(), 3);
        assert!(matches!(
            last.terminator,
            Terminator::If {
                then_bb: BlockId(4),
                else_bb: BlockId(3),
                ..
            }
        ));
        assert_eq!(f.strips, [2, 4]);
    }

    #[test]
    fn a_loop_the_vectorizer_takes_stays() {
        let mut f = strip_loop();
        // An integer sum: a vector reduction.
        f.locals[4].ty = MirTy::Prim(PrimTy::U64);
        f.locals[5].ty = MirTy::Prim(PrimTy::U64);
        if let Statement::Assign {
            rvalue: RValue::BinOp { op, op_ty, .. },
            ..
        } = &mut f.blocks[2].stmts[0]
        {
            *op = PrimOp::WrappingAdd;
            *op_ty = MirTy::Prim(PrimTy::U64);
        }
        assert!(!run(&mut f, 64, &HashSet::default()));
        assert_eq!(f.blocks.len(), 4);
    }
}
