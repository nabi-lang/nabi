//! Counted loops out of threaded iterator loops. Once threading took
//! the `Option` of an inlined `next()` apart, `for x in xs` is a loop
//! testing an index against a length at the top, stepped through a
//! temporary at the bottom:
//!
//! ```text
//! H: c = i >= n; if c then X else B
//! B: t = i + 1; .. xs[i] ..; i = t; goto H
//! ```
//!
//! Lowering rotated and strip mined the counted loops it saw; this one
//! it never saw. The test is put in `i < n` form, the step in `i = i + 1`
//! form, and the loop rotated and strip mined as lowering would have
//! (`rotate`, `strip`): no poll per iteration, and the vectorizer takes
//! it.

use crate::{MirFunction, MirTy, Operand, RValue, Statement, Terminator};
use nabic_core::HashSet;
use nabic_ir_common::{BlockId, FunctionId, LocalId, PrimOp};

/// Rotates and strip mines the innermost loops of `f` not rotated yet
/// whose test and step take the counted form; whether anything changed.
pub fn run(f: &mut MirFunction, leaf: &HashSet<FunctionId>) -> bool {
    let mut latches: nabic_core::HashMap<u32, Vec<u32>> = nabic_core::HashMap::default();
    for (l, hs) in crate::rotate::back_edges(f) {
        for h in hs {
            latches.entry(h).or_default().push(l);
        }
    }
    let idx = crate::opt::index(f);
    let mut hs: Vec<(u32, Vec<u32>)> = latches.into_iter().collect();
    hs.sort_unstable_by_key(|(h, _)| *h);
    let headers: HashSet<u32> = hs.iter().map(|(h, _)| *h).collect();
    // Innermost loops testing a comparison at the top, not in a latch.
    let loops: Vec<(u32, Vec<u32>, HashSet<u32>)> = hs
        .into_iter()
        .filter(|(h, ls)| shape(f, &idx, *h, ls))
        .map(|(h, ls)| {
            let body = crate::rotate::loop_body(f, h, &ls);
            (h, ls, body)
        })
        .filter(|(h, _, body)| !body.iter().any(|b| b != h && headers.contains(b)))
        .collect();
    if loops.is_empty() {
        return false;
    }
    let cx = Cx {
        uses: crate::opt::use_counts(f),
        addressed: crate::addressed(f),
        dom: crate::opt::Dom::new(f, &idx, &crate::opt::preds(f, &idx)),
        idx,
    };
    let mut todo: HashSet<u32> = HashSet::default();
    // Counters with the blocks of their loops; latches made for a step.
    let mut ctrs: Vec<(LocalId, HashSet<u32>)> = Vec::new();
    let mut mine: Vec<u32> = Vec::new();
    for (h, ls, body) in loops {
        if let Some((ctr, body, made)) = counted(f, &cx, h, &ls, body) {
            todo.insert(h);
            ctrs.push((ctr, body));
            mine.extend(made);
        }
    }
    if todo.is_empty() {
        return false;
    }
    let made = crate::rotate::rotate_some(f, Some(&todo));
    finish(f, &mine, &made, ctrs, leaf);
    true
}

/// Whether header `h` ends in a test of an integer comparison made in
/// it, and no latch (`ls`) tests into it: a loop not rotated.
fn shape(f: &MirFunction, idx: &nabic_core::HashMap<u32, usize>, h: u32, ls: &[u32]) -> bool {
    let hb = &f.blocks[idx[&h]];
    let Terminator::If {
        cond: Operand::Copy(c),
        ..
    } = &hb.terminator
    else {
        return false;
    };
    let cmp = hb.stmts.iter().rev().find_map(|s| match s {
        Statement::Assign { target, rvalue } if target == c => Some(rvalue),
        _ => None,
    });
    matches!(cmp, Some(RValue::BinOp {
        op: PrimOp::Lt | PrimOp::Le | PrimOp::Gt | PrimOp::Ge,
        op_ty: MirTy::Prim(p),
        ..
    }) if p.is_int())
        && !ls
            .iter()
            .any(|l| crate::version::test_into(&f.blocks[idx[l]], h).is_some())
}

/// What the loops of a function share.
struct Cx {
    idx: nabic_core::HashMap<u32, usize>,
    uses: Vec<u32>,
    addressed: HashSet<LocalId>,
    dom: crate::opt::Dom,
}

/// The loop of header `h` (latches `ls`, blocks `body`) with its test
/// and step put in the counted form: its counter, its blocks and the
/// latch holding the step (made for it, or the loop's own) that joins
/// the test copy.
fn counted(
    f: &mut MirFunction,
    cx: &Cx,
    h: u32,
    ls: &[u32],
    mut body: HashSet<u32>,
) -> Option<(LocalId, HashSet<u32>, Option<u32>)> {
    let idx = &cx.idx;
    let t = test(f, idx[&h], &body, &cx.uses, &cx.addressed)?;
    // The loop sets the counter once, in its step, which then runs with
    // `ctr < bound` holding: it cannot overflow.
    let mut sets = Vec::new();
    for &b in &body {
        let bb = &f.blocks[idx[&b]];
        if bb.params.contains(&t.ctr) {
            sets.push(usize::MAX);
        }
        for s in &bb.stmts {
            if crate::liveness::stmt_uses(s).1 == Some(t.ctr) {
                sets.push(idx[&b]);
            }
        }
    }
    let [sb] = sets[..] else {
        return None;
    };
    if sb == usize::MAX || !ls.iter().all(|l| cx.dom.dominates(sb, idx[l])) {
        return None;
    }
    let st = step(f, sb, t.ctr, &cx.uses)?;
    let one = ls.len() == 1
        && idx[&ls[0]] == sb
        && matches!(&f.blocks[sb].terminator,
            Terminator::Goto { target, args } if target.0 == h && args.is_empty());
    // Else the step moves to a latch of its own: nothing may read the
    // counter after it but the header's test.
    if !one && !only_before(f, t.ctr, idx[&h], sb, st.at) {
        return None;
    }
    let hb = &mut f.blocks[idx[&h]];
    hb.stmts[t.at] = t.stmt;
    if t.swap {
        if let Terminator::If {
            then_bb,
            then_args,
            else_bb,
            else_args,
            ..
        } = &mut hb.terminator
        {
            std::mem::swap(then_bb, else_bb);
            std::mem::swap(then_args, else_args);
        }
    }
    let made = if one {
        f.blocks[sb].stmts[st.at] = st.stmt;
        // A latch below the loop's first block (a branch between): it
        // joins the test copy (`finish`), so `strip` finds the step there.
        let entry = match &f.blocks[idx[&h]].terminator {
            Terminator::If { then_bb, .. } => then_bb.0,
            _ => h,
        };
        let b = &f.blocks[sb];
        (b.id.0 != h && b.id.0 != entry && b.params.is_empty()).then_some(b.id.0)
    } else {
        f.blocks[sb].stmts.remove(st.at);
        let latch = crate::opt::fresh_block(f);
        let mut l = crate::opt::empty_block(
            latch,
            Terminator::Goto {
                target: BlockId(h),
                args: Vec::new(),
            },
        );
        l.stmts.push(st.stmt);
        for x in ls {
            crate::rotate::retarget(&mut f.blocks[idx[x]].terminator, h, latch.0);
        }
        f.blocks.push(l);
        body.insert(latch.0);
        Some(latch.0)
    };
    if let Some(k) = st.temp {
        f.blocks[sb].stmts.remove(k);
    }
    merge_ivs(f, h, &body, &cx.dom, ls, t.ctr);
    Some((t.ctr, body, made))
}

/// After rotation (test copies `made`): a latch holding a step joins
/// the test copy after it, as `strip` wants the step there; the loops
/// are strip mined, and their steps, each below the bound, made
/// wrapping (`strip` takes a checked one).
fn finish(
    f: &mut MirFunction,
    mine: &[u32],
    made: &[u32],
    ctrs: Vec<(LocalId, HashSet<u32>)>,
    leaf: &HashSet<FunctionId>,
) {
    for &l in mine {
        let Some(li) = f.blocks.iter().position(|b| b.id.0 == l) else {
            continue;
        };
        let Terminator::Goto { target: c, .. } = f.blocks[li].terminator else {
            continue;
        };
        if !made.contains(&c.0) {
            continue;
        }
        let lb = f.blocks.remove(li);
        for b in &mut f.blocks {
            crate::rotate::retarget(&mut b.terminator, l, c.0);
        }
        if let Some(cb) = f.blocks.iter_mut().find(|b| b.id == c) {
            cb.stmts.splice(0..0, lb.stmts);
        }
    }
    if !made.is_empty() {
        crate::strip::strip_some(f, made, leaf);
    }
    for (ctr, mut body) in ctrs {
        body.extend(made.iter().copied());
        for b in f.blocks.iter_mut().filter(|b| body.contains(&b.id.0)) {
            for s in &mut b.stmts {
                if let Statement::Assign {
                    target,
                    rvalue:
                        RValue::BinOp {
                            op,
                            lhs: Operand::Copy(x),
                            ..
                        },
                } = s
                {
                    if *target == ctr && *x == ctr && *op == PrimOp::Add {
                        *op = PrimOp::WrappingAdd;
                    }
                }
            }
        }
    }
}

/// Puts header `h`'s exit test in the form `c = ctr < bound; if c then
/// <in> else <out>`, `ctr` changed by the loop `body`, `bound` not;
/// `ctr` if it could.
fn test(
    f: &MirFunction,
    h: usize,
    body: &HashSet<u32>,
    uses: &[u32],
    addressed: &HashSet<LocalId>,
) -> Option<Test> {
    let hb = &f.blocks[h];
    let Terminator::If {
        cond: Operand::Copy(c),
        then_bb,
        else_bb,
        ..
    } = &hb.terminator
    else {
        return None;
    };
    let c = *c;
    let stay_then = match (body.contains(&then_bb.0), body.contains(&else_bb.0)) {
        (true, false) => true,
        (false, true) => false,
        _ => return None,
    };
    if !hb.params.is_empty() || uses[c.0 as usize] != 1 {
        return None;
    }
    let at = hb
        .stmts
        .iter()
        .rposition(|s| matches!(s, Statement::Assign { target, .. } if *target == c))?;
    let Statement::Assign {
        rvalue:
            RValue::BinOp {
                op,
                op_ty: MirTy::Prim(ty),
                lhs,
                rhs,
            },
        ..
    } = &hb.stmts[at]
    else {
        return None;
    };
    if !ty.is_int() {
        return None;
    }
    let mut moved: HashSet<LocalId> = HashSet::default();
    for b in f.blocks.iter().filter(|b| body.contains(&b.id.0)) {
        moved.extend(b.params.iter().copied());
        for s in &b.stmts {
            moved.extend(crate::liveness::stmt_uses(s).1);
        }
    }
    let fixed = |o: &Operand| match o {
        Operand::Const(_) => true,
        Operand::Copy(l) => !moved.contains(l) && !addressed.contains(l),
    };
    let var =
        |o: &Operand| matches!(o, Operand::Copy(l) if moved.contains(l) && !addressed.contains(l));
    // `ctr < bound` holding while the loop stays, as (ctr, bound).
    let (ctr, bound) = match (op, stay_then) {
        (PrimOp::Lt, true) | (PrimOp::Ge, false) if var(lhs) && fixed(rhs) => (lhs, rhs),
        (PrimOp::Gt, true) | (PrimOp::Le, false) if fixed(lhs) && var(rhs) => (rhs, lhs),
        _ => return None,
    };
    let Operand::Copy(ctr) = *ctr else {
        return None;
    };
    Some(Test {
        ctr,
        at,
        stmt: Statement::Assign {
            target: c,
            rvalue: RValue::BinOp {
                op: PrimOp::Lt,
                op_ty: MirTy::Prim(*ty),
                lhs: Operand::Copy(ctr),
                rhs: bound.clone(),
            },
        },
        swap: !stay_then,
    })
}

/// A header's test in the counted form: statement `at` becomes `stmt`,
/// the branch's sides swapped with `swap`.
struct Test {
    ctr: LocalId,
    at: usize,
    stmt: Statement,
    swap: bool,
}

/// A latch's step in the counted form: statement `at` becomes `stmt`,
/// the temporary's assignment `temp` goes.
struct Step {
    at: usize,
    stmt: Statement,
    temp: Option<usize>,
}

/// Counters stepping along with `ctr`: from the same constant, by one
/// an iteration (an `enumerate`'s index next to the slice's). One read
/// only in its step's block, before the step, and the step before
/// `ctr`'s, reads what `ctr` holds there: it becomes `ctr`, its step
/// goes.
fn merge_ivs(
    f: &mut MirFunction,
    h: u32,
    body: &HashSet<u32>,
    dom: &crate::opt::Dom,
    latches: &[u32],
    ctr: LocalId,
) {
    let idx = crate::opt::index(f);
    let uses = crate::opt::use_counts(f);
    let ty = &f.locals[ctr.0 as usize].ty;
    // Per local: assignments in the loop (block, statement), and the
    // constants it is set to outside.
    let mut inside: nabic_core::HashMap<LocalId, Vec<(usize, usize)>> =
        nabic_core::HashMap::default();
    let mut outside: nabic_core::HashMap<LocalId, Vec<Option<(usize, i128)>>> =
        nabic_core::HashMap::default();
    for (i, b) in f.blocks.iter().enumerate() {
        let inb = body.contains(&b.id.0);
        for p in &b.params {
            outside.entry(*p).or_default().push(None);
        }
        for (k, s) in b.stmts.iter().enumerate() {
            let Some(d) = crate::liveness::stmt_uses(s).1 else {
                continue;
            };
            if inb {
                inside.entry(d).or_default().push((i, k));
            } else {
                let c = match s {
                    Statement::Assign {
                        rvalue: RValue::Use(Operand::Const(c)),
                        ..
                    } => num(c).map(|v| (i, v)),
                    _ => None,
                };
                outside.entry(d).or_default().push(c);
            }
        }
    }
    for p in &f.params {
        outside.entry(*p).or_default().push(None);
    }
    let start = |l: LocalId| match outside.get(&l).map(Vec::as_slice) {
        Some([Some(c)]) => Some(*c),
        _ => None,
    };
    // Both set in the one block every way into the loop comes from.
    let preds = crate::opt::preds(f, &idx);
    let entries: Vec<usize> = preds[idx[&h]]
        .iter()
        .copied()
        .filter(|p| !body.contains(&f.blocks[*p].id.0))
        .collect();
    let Some(c0) = start(ctr) else {
        return;
    };
    if entries.iter().any(|&p| p != c0.0) {
        return;
    }
    let addressed = crate::addressed(f);
    let mut cands: Vec<(LocalId, usize)> = inside
        .iter()
        .filter(|(v, ds)| {
            **v != ctr
                && ds.len() == 1
                && f.locals[v.0 as usize].ty == *ty
                && !addressed.contains(v)
                && start(**v).as_ref() == Some(&c0)
        })
        .map(|(v, ds)| (*v, ds[0].0))
        .collect();
    cands.sort_unstable_by_key(|(v, _)| v.0);
    for (v, sv) in cands {
        if !latches.iter().all(|l| dom.dominates(sv, idx[l])) {
            continue;
        }
        let Some(st) = step(f, sv, v, &uses) else {
            continue;
        };
        if !only_before(f, v, usize::MAX, sv, st.at) {
            continue;
        }
        // `ctr` not stepped yet where `v` is read (its step aside).
        let b = &f.blocks[sv];
        let last = (0..st.at).rev().find(|&k| {
            let mut hit = false;
            crate::stmt_reads(&b.stmts[k], &mut |l| hit |= l == v);
            hit && Some(k) != st.temp
        });
        let stepped = last.is_some_and(|r| {
            b.stmts[..r]
                .iter()
                .any(|s| crate::liveness::stmt_uses(s).1 == Some(ctr))
        });
        if stepped {
            continue;
        }
        let b = &mut f.blocks[sv];
        b.stmts.remove(st.at);
        if let Some(k) = st.temp {
            b.stmts.remove(k);
        }
        for s in &mut b.stmts {
            crate::opt::stmt_ops_mut(s, &mut |o| {
                if *o == Operand::Copy(v) {
                    *o = Operand::Copy(ctr);
                }
            });
        }
    }
}

/// An integer constant's value, of either kind.
fn num(c: &nabic_ir_common::ConstVal) -> Option<i128> {
    match c {
        nabic_ir_common::ConstVal::Int { value, .. } => Some(*value),
        nabic_ir_common::ConstVal::UInt { value, .. } => i128::try_from(*value).ok(),
        _ => None,
    }
}

/// Whether `ctr` is read only in block `h` and in block `s` up to its
/// statement `at`.
fn only_before(f: &MirFunction, ctr: LocalId, h: usize, s: usize, at: usize) -> bool {
    f.blocks.iter().enumerate().all(|(i, b)| {
        if i == h {
            return true;
        }
        let reads = |st: &Statement| {
            let mut hit = false;
            crate::stmt_reads(st, &mut |l| hit |= l == ctr);
            hit
        };
        let mut hit = false;
        crate::term_reads(&b.terminator, &mut |l| hit |= l == ctr);
        !hit && b
            .stmts
            .iter()
            .enumerate()
            .all(|(k, st)| (i == s && k <= at) || !reads(st))
    })
}

/// Block `l`'s step of `ctr` by one, once, as `ctr = ctr + 1`; also
/// one through a temporary (`t = ctr + 1; ..; ctr = t`).
fn step(f: &MirFunction, l: usize, ctr: LocalId, uses: &[u32]) -> Option<Step> {
    // `x + 1`: its type and the one.
    let inc = |rv: &RValue, x: LocalId| match rv {
        RValue::BinOp {
            op: PrimOp::Add | PrimOp::WrappingAdd,
            op_ty,
            lhs: Operand::Copy(y),
            rhs:
                one @ Operand::Const(
                    nabic_ir_common::ConstVal::Int { value: 1, .. }
                    | nabic_ir_common::ConstVal::UInt { value: 1, .. },
                ),
        } if *y == x => Some((op_ty.clone(), one.clone())),
        _ => None,
    };
    let b = &f.blocks[l];
    let defs: Vec<usize> = (0..b.stmts.len())
        .filter(|&i| crate::liveness::stmt_uses(&b.stmts[i]).1 == Some(ctr))
        .collect();
    let [d] = defs[..] else {
        return None;
    };
    let Statement::Assign { rvalue, .. } = &b.stmts[d] else {
        return None;
    };
    let (temp, (op_ty, one)) = match rvalue {
        RValue::Use(Operand::Copy(t)) if uses[t.0 as usize] == 1 => {
            // The last assignment of `t` before (`ctr` is set only at `d`).
            let k = (0..d)
                .rev()
                .find(|&k| crate::liveness::stmt_uses(&b.stmts[k]).1 == Some(*t))?;
            let Statement::Assign { rvalue: r, .. } = &b.stmts[k] else {
                return None;
            };
            (Some(k), inc(r, ctr)?)
        }
        r => (None, inc(r, ctr)?),
    };
    // The loop test bounds it: no overflow check needed, cleanup proves
    // as much again.
    Some(Step {
        at: d,
        stmt: Statement::Assign {
            target: ctr,
            rvalue: RValue::BinOp {
                op: PrimOp::Add,
                op_ty,
                lhs: Operand::Copy(ctr),
                rhs: one,
            },
        },
        temp,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BasicBlock, LocalDecl};
    use nabic_diagnostic::Span;
    use nabic_ir_common::{ConstVal, FunctionId, PrimTy};

    const fn usize_op(t: u32, op: PrimOp, lhs: Operand, rhs: Operand) -> Statement {
        Statement::Assign {
            target: LocalId(t),
            rvalue: RValue::BinOp {
                op,
                op_ty: MirTy::Prim(PrimTy::Usize),
                lhs,
                rhs,
            },
        }
    }

    const fn one() -> Operand {
        Operand::Const(ConstVal::UInt {
            value: 1,
            ty: PrimTy::Usize,
        })
    }

    /// What threading leaves of `for x in xs`: x0 the index, x1 the
    /// length, x2 the test, x3 the step's temporary.
    ///
    /// bb0: x0 = 0; goto bb1
    /// bb1: x2 = x0 >= x1; if x2 then bb3 else bb2
    /// bb2: x3 = x0 + 1; x0 = x3; goto bb1
    /// bb3: return
    fn walk() -> MirFunction {
        let block = |id, stmts, terminator| BasicBlock {
            id: BlockId(id),
            params: vec![],
            stmts,
            terminator,
        };
        MirFunction {
            id: FunctionId(0),
            def_id: None,
            name: "f".into(),
            facts: nabic_ir_common::FnFacts::default(),
            strips: Vec::new(),
            pins: Vec::new(),
            dbg: crate::FnDbg::default(),
            params: vec![LocalId(1)],
            locals: (0..4)
                .map(|i| LocalDecl {
                    id: LocalId(i),
                    name: None,
                    ty: MirTy::Prim(if i == 2 { PrimTy::Bool } else { PrimTy::Usize }),
                })
                .collect(),
            blocks: vec![
                block(
                    0,
                    vec![Statement::Assign {
                        target: LocalId(0),
                        rvalue: RValue::Use(Operand::Const(ConstVal::UInt {
                            value: 0,
                            ty: PrimTy::Usize,
                        })),
                    }],
                    Terminator::Goto {
                        target: BlockId(1),
                        args: vec![],
                    },
                ),
                block(
                    1,
                    vec![usize_op(
                        2,
                        PrimOp::Ge,
                        Operand::Copy(LocalId(0)),
                        Operand::Copy(LocalId(1)),
                    )],
                    Terminator::If {
                        cond: Operand::Copy(LocalId(2)),
                        then_bb: BlockId(3),
                        then_args: vec![],
                        else_bb: BlockId(2),
                        else_args: vec![],
                    },
                ),
                block(
                    2,
                    vec![
                        usize_op(3, PrimOp::WrappingAdd, Operand::Copy(LocalId(0)), one()),
                        Statement::Assign {
                            target: LocalId(0),
                            rvalue: RValue::Use(Operand::Copy(LocalId(3))),
                        },
                    ],
                    Terminator::Goto {
                        target: BlockId(1),
                        args: vec![],
                    },
                ),
                block(3, vec![], Terminator::Return(None)),
            ],
            param_tys: vec![MirTy::Prim(PrimTy::Usize)],
            ret_ty: MirTy::Unit,
            span: Span::default(),
        }
    }

    #[test]
    fn an_iterator_walk_becomes_a_strip_mined_loop() {
        let mut f = walk();
        assert!(run(&mut f, &HashSet::default()));
        // bb2 runs the strips; the test moved below it, `i < n`.
        assert_eq!(f.strips, vec![2]);
        assert!(crate::rotate::short_loops(&f).contains_key(&2));
        // The step lost its temporary and wraps: the bound keeps it small.
        let steps: Vec<&Statement> = f
            .blocks
            .iter()
            .flat_map(|b| &b.stmts)
            .filter(|s| crate::liveness::stmt_uses(s).1 == Some(LocalId(0)))
            .collect();
        assert_eq!(steps.len(), 2);
        assert!(steps.iter().any(|s| matches!(
            s,
            Statement::Assign {
                rvalue: RValue::BinOp {
                    op: PrimOp::WrappingAdd,
                    ..
                },
                ..
            }
        )));
        // Run again: rotated now, nothing to do.
        assert!(!run(&mut f, &HashSet::default()));
    }
}
