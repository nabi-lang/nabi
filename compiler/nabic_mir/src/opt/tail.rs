//! Self tail calls as loops. `return f(args)` in `f` assigns the
//! arguments to the parameters (through temporaries: an argument may
//! read a parameter) and jumps back to the start of the body, so
//! recursion that ends in its own call runs in one frame. The jump is a
//! back edge, so it polls like any loop.
//!
//! A function taking a local's address is left alone: a pointer passed
//! down could point at the frame the loop reuses.

use crate::{MirFunction, MirTy, Operand, RValue, Statement, Terminator};
use nabic_ir_common::{Abi, FnRole};

pub fn self_calls(f: &mut MirFunction) -> bool {
    if f.facts.abi != Abi::Nabi
        || f.facts.role != FnRole::Plain
        || !f.blocks[0].params.is_empty()
        || !crate::addressed(f).is_empty()
    {
        return false;
    }
    let idx = crate::opt::index(f);
    let unit = f.ret_ty == MirTy::Unit;
    let mut sites = Vec::new();
    for (bi, b) in f.blocks.iter().enumerate() {
        let Some(k) = b.stmts.iter().rposition(|s| !crate::opt::marker(s)) else {
            continue;
        };
        let Statement::Assign {
            target,
            rvalue: RValue::Call { callee, args, .. },
        } = &b.stmts[k]
        else {
            continue;
        };
        if *callee != f.id || args.len() != f.params.len() {
            continue;
        }
        // What is returned: here, or by an empty block jumped to (a
        // parameter of it: the argument passed).
        let ret = match &b.terminator {
            Terminator::Return(v) => Some(v.clone()),
            Terminator::Goto { target: j, args } => {
                let jb = &f.blocks[idx[&j.0]];
                let empty = jb.params.len() == args.len()
                    && jb.stmts.iter().all(|s| matches!(s, Statement::Loc(..)));
                match &jb.terminator {
                    Terminator::Return(Some(Operand::Copy(x))) if empty => {
                        match jb.params.iter().position(|p| p == x) {
                            Some(i) => Some(Some(args[i].clone())),
                            None if args.is_empty() => Some(Some(Operand::Copy(*x))),
                            None => None,
                        }
                    }
                    Terminator::Return(v) if empty && args.is_empty() => Some(v.clone()),
                    _ => None,
                }
            }
            _ => None,
        };
        let tail = match ret {
            Some(Some(Operand::Copy(x))) => x == *target,
            Some(None | Some(Operand::Const(nabic_ir_common::ConstVal::Unit))) => unit,
            _ => false,
        };
        if tail {
            sites.push((bi, k));
        }
    }
    if sites.is_empty() {
        return false;
    }
    let body = f.blocks[0].id;
    let entry = crate::opt::fresh_block(f);
    for (bi, k) in sites {
        let rest: Vec<Statement> = f.blocks[bi].stmts.drain(k..).collect();
        let Some(Statement::Assign {
            rvalue: RValue::Call { args, .. },
            ..
        }) = rest.first().cloned()
        else {
            unreachable!()
        };
        let params = f.params.clone();
        let tmps: Vec<_> = params
            .iter()
            .map(|p| {
                let ty = f.locals[p.0 as usize].ty.clone();
                crate::opt::fresh_local(f, ty)
            })
            .collect();
        let b = &mut f.blocks[bi];
        for (t, a) in tmps.iter().zip(args) {
            b.stmts.push(Statement::Assign {
                target: *t,
                rvalue: RValue::Use(a),
            });
        }
        b.stmts.extend(
            rest.into_iter()
                .skip(1)
                .filter(|s| matches!(s, Statement::StorageDead(_))),
        );
        for (p, t) in params.iter().zip(&tmps) {
            b.stmts.push(Statement::Assign {
                target: *p,
                rvalue: RValue::Use(Operand::Copy(*t)),
            });
        }
        b.terminator = Terminator::Goto {
            target: body,
            args: Vec::new(),
        };
    }
    f.blocks.insert(
        0,
        crate::opt::empty_block(
            entry,
            Terminator::Goto {
                target: body,
                args: Vec::new(),
            },
        ),
    );
    true
}
