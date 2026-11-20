//! Control-flow cleanup: constant branches become jumps, jumps through
//! empty blocks go straight on, a block whose only predecessor jumps to
//! it joins that predecessor, and unreachable blocks go.

use crate::{BasicBlock, MirFunction, Operand, RValue, Statement, Terminator};
use nabic_core::{HashMap, HashSet};
use nabic_ir_common::{ConstVal, LocalId};

pub fn run(f: &mut MirFunction) -> bool {
    let mut changed = branches(f);
    changed |= thread(f);
    changed |= unreachable(f);
    changed |= merge(f);
    changed
}

/// `if true` / `if false`, and `if c` to one place.
fn branches(f: &mut MirFunction) -> bool {
    let mut changed = false;
    for b in &mut f.blocks {
        let Terminator::If {
            cond,
            then_bb,
            then_args,
            else_bb,
            else_args,
        } = &b.terminator
        else {
            continue;
        };
        let to = match cond {
            Operand::Const(ConstVal::Bool(true)) => Some((*then_bb, then_args.clone())),
            Operand::Const(ConstVal::Bool(false)) => Some((*else_bb, else_args.clone())),
            _ if then_bb == else_bb && then_args == else_args => {
                Some((*then_bb, then_args.clone()))
            }
            _ => None,
        };
        if let Some((target, args)) = to {
            b.terminator = Terminator::Goto { target, args };
            changed = true;
        }
    }
    changed
}

/// A block of no statements (debug positions aside) and no parameters.
fn empty(b: &BasicBlock) -> bool {
    b.params.is_empty() && b.stmts.iter().all(|s| matches!(s, Statement::Loc(..)))
}

/// Edges into an empty block that only jumps on go to its target; a jump
/// to an empty block that returns returns.
fn thread(f: &mut MirFunction) -> bool {
    let entry = f.blocks[0].id;
    let strips: HashSet<u32> = f.strips.iter().copied().collect();
    // Empty block -> where it goes.
    let hop: HashMap<u32, (nabic_ir_common::BlockId, Vec<Operand>)> = f
        .blocks
        .iter()
        .filter(|b| b.id != entry && empty(b) && !strips.contains(&b.id.0))
        .filter_map(|b| match &b.terminator {
            Terminator::Goto { target, args } if *target != b.id => {
                Some((b.id.0, (*target, args.clone())))
            }
            _ => None,
        })
        .collect();
    // Blocks that only end the function, by id, with their parameters.
    let ends: HashMap<u32, (Vec<LocalId>, Terminator)> = f
        .blocks
        .iter()
        .filter(|b| b.id != entry && b.stmts.iter().all(|s| matches!(s, Statement::Loc(..))))
        .filter_map(|b| match &b.terminator {
            t @ (Terminator::Return(_) | Terminator::Unreachable) => {
                Some((b.id.0, (b.params.clone(), t.clone())))
            }
            _ => None,
        })
        .collect();
    let mut changed = false;
    for b in &mut f.blocks {
        match &mut b.terminator {
            Terminator::Goto { target, args } => {
                for _ in 0..8 {
                    match hop.get(&target.0) {
                        Some((t, a)) if args.is_empty() && *t != b.id => {
                            *target = *t;
                            args.clone_from(a);
                            changed = true;
                        }
                        _ => break,
                    }
                }
                if let Some((ps, t)) = ends.get(&target.0).filter(|(ps, _)| ps.len() == args.len())
                {
                    // A parameter returned is the argument passed.
                    let mut t = t.clone();
                    crate::opt::term_ops_mut(&mut t, &mut |o| {
                        if let Operand::Copy(l) = o {
                            if let Some(k) = ps.iter().position(|p| p == l) {
                                *o = args[k].clone();
                            }
                        }
                    });
                    b.terminator = t;
                    changed = true;
                }
            }
            Terminator::If {
                then_bb,
                then_args,
                else_bb,
                else_args,
                ..
            } => {
                for (target, args) in [(then_bb, then_args), (else_bb, else_args)] {
                    for _ in 0..8 {
                        match hop.get(&target.0) {
                            Some((t, a)) if args.is_empty() => {
                                *target = *t;
                                args.clone_from(a);
                                changed = true;
                            }
                            _ => break,
                        }
                    }
                }
            }
            _ => {}
        }
    }
    changed
}

/// Drops blocks the entry does not reach.
fn unreachable(f: &mut MirFunction) -> bool {
    let idx = crate::opt::index(f);
    let live: HashSet<usize> = crate::opt::rpo(f, &idx).into_iter().collect();
    if live.len() == f.blocks.len() {
        return false;
    }
    let mut i = 0;
    f.blocks.retain(|_| {
        i += 1;
        live.contains(&(i - 1))
    });
    let ids: HashSet<u32> = f.blocks.iter().map(|b| b.id.0).collect();
    f.strips.retain(|h| ids.contains(h));
    true
}

/// `b: .. goto c` where `b` is `c`'s only predecessor: `c` joins `b`.
fn merge(f: &mut MirFunction) -> bool {
    let entry = f.blocks[0].id;
    let strips: HashSet<u32> = f.strips.iter().copied().collect();
    let mut npreds: HashMap<u32, usize> = HashMap::default();
    for b in &f.blocks {
        for s in crate::opt::succs(&b.terminator) {
            *npreds.entry(s.0).or_default() += 1;
        }
    }
    let idx = crate::opt::index(f);
    let mut gone: HashSet<u32> = HashSet::default();
    let mut changed = false;
    for i in 0..f.blocks.len() {
        if gone.contains(&f.blocks[i].id.0) {
            continue;
        }
        loop {
            let Terminator::Goto { target, .. } = &f.blocks[i].terminator else {
                break;
            };
            let c = *target;
            if c == f.blocks[i].id
                || c == entry
                || npreds.get(&c.0) != Some(&1)
                || strips.contains(&c.0)
                || gone.contains(&c.0)
            {
                break;
            }
            let j = idx[&c.0];
            let cb = std::mem::replace(
                &mut f.blocks[j],
                crate::opt::empty_block(c, Terminator::Unreachable),
            );
            gone.insert(c.0);
            let Terminator::Goto { args, .. } =
                std::mem::replace(&mut f.blocks[i].terminator, Terminator::Unreachable)
            else {
                unreachable!()
            };
            let params: Vec<LocalId> = cb.params.clone();
            let clash = args
                .iter()
                .any(|a| matches!(a, Operand::Copy(l) if params.contains(l)));
            let mut pre = Vec::new();
            if clash {
                let tmps: Vec<LocalId> = params
                    .iter()
                    .map(|p| {
                        let ty = f.locals[p.0 as usize].ty.clone();
                        crate::opt::fresh_local(f, ty)
                    })
                    .collect();
                for (t, a) in tmps.iter().zip(&args) {
                    pre.push(Statement::Assign {
                        target: *t,
                        rvalue: RValue::Use(a.clone()),
                    });
                }
                for (p, t) in params.iter().zip(&tmps) {
                    pre.push(Statement::Assign {
                        target: *p,
                        rvalue: RValue::Use(Operand::Copy(*t)),
                    });
                }
            } else {
                for (p, a) in params.iter().zip(args) {
                    pre.push(Statement::Assign {
                        target: *p,
                        rvalue: RValue::Use(a),
                    });
                }
            }
            let b = &mut f.blocks[i];
            b.stmts.extend(pre);
            b.stmts.extend(cb.stmts);
            b.terminator = cb.terminator;
            changed = true;
        }
    }
    if changed {
        f.blocks.retain(|b| !gone.contains(&b.id.0));
    }
    changed
}
