//! A loop that fills a short array of scalars with one constant
//! (`[0u8; 20]`, as lowering writes it) becomes the array's literal:
//!
//! ```text
//! P: ..; i = 0; ..; goto L
//! L: a[i] = c; i = i + 1; t = i < N; if t then L else X
//! ```
//!
//! `P` then sets `a`, `i` and `t` as the loop left them and goes to `X`:
//! no loop, no poll, and codegen clears or stores the bytes at once.

use crate::{MirFunction, MirTy, Operand, RValue, Statement, Terminator};
use nabic_ir_common::{ConstVal, LocalId, PrimOp, PrimTy};

/// Bytes of the largest array made a literal.
const MAX_BYTES: u32 = 256;

fn usize_is(o: &Operand, v: i128) -> bool {
    matches!(o, Operand::Const(c) if crate::opt::int_val(c, PrimTy::Usize, 64) == Some(v))
}

/// The fill loop at block `l`: `(array, index, test, value, length,
/// element type)`.
fn shape(f: &MirFunction, l: usize) -> Option<(LocalId, LocalId, LocalId, Operand, u32, MirTy)> {
    let b = &f.blocks[l];
    let body: Vec<&Statement> = b.stmts.iter().filter(|s| !crate::opt::marker(s)).collect();
    let [
        Statement::ArrayWrite {
            array,
            offset: 0,
            index: Operand::Copy(i),
            value: value @ Operand::Const(_),
            elem_ty: elem @ MirTy::Prim(p),
            len,
        },
        Statement::Assign {
            target: i2,
            rvalue:
                RValue::BinOp {
                    op: PrimOp::Add | PrimOp::WrappingAdd,
                    op_ty: MirTy::Prim(PrimTy::Usize),
                    lhs: Operand::Copy(i3),
                    rhs: one,
                },
        },
        Statement::Assign {
            target: t,
            rvalue:
                RValue::BinOp {
                    op: PrimOp::Lt,
                    op_ty: MirTy::Prim(PrimTy::Usize),
                    lhs: Operand::Copy(i4),
                    rhs: n,
                },
        },
    ] = body[..]
    else {
        return None;
    };
    let Terminator::If {
        cond: Operand::Copy(c),
        then_bb,
        then_args,
        else_args,
        ..
    } = &b.terminator
    else {
        return None;
    };
    let size = (p.bit_width(64) / 8).max(1);
    let ok = i2 == i
        && i3 == i
        && i4 == i
        && c == t
        && t != i
        && array != i
        && array != t
        && *then_bb == b.id
        && then_args.is_empty()
        && else_args.is_empty()
        && usize_is(one, 1)
        && usize_is(n, i128::from(*len))
        && *len > 0
        && size * len <= MAX_BYTES
        && *p != PrimTy::Str
        && f.locals[i.0 as usize].ty == MirTy::Prim(PrimTy::Usize)
        && f.locals[array.0 as usize].ty == MirTy::Array(Box::new(elem.clone()), *len);
    ok.then(|| (*array, *i, *t, value.clone(), *len, elem.clone()))
}

/// Whether `s` reads or writes `l`.
fn touches(s: &Statement, l: LocalId) -> bool {
    let mut hit = false;
    crate::stmt_reads(s, &mut |r| hit |= r == l);
    hit || match s {
        Statement::Assign { target, .. } => *target == l,
        Statement::StorageDead(x) | Statement::ArrayWrite { array: x, .. } => *x == l,
        _ => false,
    }
}

/// Makes the fill loops of `f` literals; whether anything changed.
pub fn run(f: &mut MirFunction) -> bool {
    let idx = crate::opt::index(f);
    let preds = crate::opt::preds(f, &idx);
    // The array is written in place (`pinned` would name it), not its
    // address taken.
    let addressed = crate::addressed(f);
    let mut changed = false;
    for (l, from) in preds.iter().enumerate() {
        let Some((a, i, t, value, len, elem)) = shape(f, l) else {
            continue;
        };
        if [a, i, t].iter().any(|x| addressed.contains(x)) {
            continue;
        }
        let ps: Vec<usize> = from.iter().copied().filter(|&p| p != l).collect();
        let [p] = ps[..] else { continue };
        if from.len() != 2 {
            continue;
        }
        let pb = &f.blocks[p];
        let Terminator::Goto { target, args } = &pb.terminator else {
            continue;
        };
        if *target != f.blocks[l].id || !args.is_empty() {
            continue;
        }
        // `i = 0` last of what `p` does to `i`, nothing of `a` after it.
        let Some(k) = pb.stmts.iter().rposition(|s| touches(s, i)) else {
            continue;
        };
        let zero = matches!(&pb.stmts[k], Statement::Assign {
            target,
            rvalue: RValue::Use(o),
        } if *target == i && usize_is(o, 0));
        if !zero || pb.stmts[k + 1..].iter().any(|s| touches(s, a)) {
            continue;
        }
        let Terminator::If { else_bb, .. } = f.blocks[l].terminator.clone() else {
            unreachable!()
        };
        let n = i128::from(len);
        let pb = &mut f.blocks[p];
        pb.stmts.push(Statement::Assign {
            target: a,
            rvalue: RValue::ArrayInit {
                elem_ty: elem,
                elements: vec![value; len as usize],
            },
        });
        pb.stmts.push(Statement::Assign {
            target: i,
            rvalue: RValue::Use(Operand::Const(ConstVal::Int {
                value: n,
                ty: PrimTy::Usize,
            })),
        });
        pb.stmts.push(Statement::Assign {
            target: t,
            rvalue: RValue::Use(Operand::Const(ConstVal::Bool(false))),
        });
        pb.terminator = Terminator::Goto {
            target: else_bb,
            args: Vec::new(),
        };
        changed = true;
    }
    changed
}
