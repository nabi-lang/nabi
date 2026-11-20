//! `a % c` beside `a / c` of the same block: the remainder from the
//! quotient, `a - (a / c) * c`, so the division by a constant is made
//! once (digit loops take both).

#[allow(clippy::wildcard_imports)]
use crate::opt::simplify::*;

/// The constant divisor `c` of `a op c` that cannot trap (not 0, not -1
/// when signed) nor be a no-op (not 1), and the dividend.
fn by_const(rv: &RValue, op: PrimOp, pw: u32) -> Option<(PrimTy, &Operand, &Operand)> {
    let RValue::BinOp {
        op: o,
        op_ty: MirTy::Prim(p),
        lhs,
        rhs: rhs @ Operand::Const(c),
    } = rv
    else {
        return None;
    };
    if *o != op || !(p.is_signed_int() || p.is_unsigned_int()) {
        return None;
    }
    let v = int_val(c, *p, pw)?;
    (!matches!(v, -1..=1)).then_some((*p, lhs, rhs))
}

/// Pairs each remainder with a quotient of the same block; whether
/// anything changed. Late: the ranges know a remainder, not the
/// difference it becomes.
pub fn pair(f: &mut MirFunction, pw: u32) -> bool {
    let any = f.blocks.iter().flat_map(|b| &b.stmts).any(|s| {
        matches!(s, Statement::Assign { rvalue, .. } if by_const(rvalue, PrimOp::Rem, pw).is_some())
    });
    if !any {
        return false;
    }
    let pinned = &crate::opt::pinned(f);
    let mut changed = false;
    for bi in 0..f.blocks.len() {
        let mut i = 0;
        while i < f.blocks[bi].stmts.len() {
            if let Some(n) = pair_at(f, bi, i, pw, pinned) {
                changed = true;
                i += n;
            } else {
                i += 1;
            }
        }
    }
    changed
}

/// Whether statement `s` assigns `l` (or may change it).
fn sets(s: &Statement, l: LocalId) -> bool {
    match s {
        Statement::Assign { target, .. } => *target == l,
        Statement::StorageDead(x) | Statement::ArrayWrite { array: x, .. } => *x == l,
        _ => false,
    }
}

/// Rewrites the remainder at statement `i` of block `bi` if a quotient
/// pairs with it; the statements it now takes.
fn pair_at(
    f: &mut MirFunction,
    bi: usize,
    i: usize,
    pw: u32,
    pinned: &HashSet<LocalId>,
) -> Option<usize> {
    let stmts = &f.blocks[bi].stmts;
    let Statement::Assign { target: t, rvalue } = &stmts[i] else {
        return None;
    };
    let t = *t;
    let (p, a, c) = by_const(rvalue, PrimOp::Rem, pw)?;
    let (a, c) = (a.clone(), c.clone());
    let al = match &a {
        Operand::Copy(l) => Some(*l),
        Operand::Const(_) => return None,
    };
    let pin = |l: LocalId| pinned.contains(&l);
    if pin(t) || al.is_some_and(pin) {
        return None;
    }
    let quot = |s: &Statement| match s {
        Statement::Assign { target, rvalue } => by_const(rvalue, PrimOp::Div, pw)
            .filter(|(q, x, y)| *q == p && **x == a && **y == c)
            .map(|_| *target),
        _ => None,
    };
    let keeps_a = |s: &Statement| al.is_none_or(|l| !sets(s, l));
    let ty = MirTy::Prim(p);
    let bin = |op, lhs, rhs| RValue::BinOp {
        op,
        op_ty: ty.clone(),
        lhs,
        rhs,
    };
    // A quotient before, still held: the remainder from it.
    let mut before = None;
    for s in stmts[..i].iter().rev() {
        if let Some(q) = quot(s).filter(|q| !pin(*q) && Some(*q) != al) {
            before = Some(q);
            break;
        }
        if !keeps_a(s) {
            break;
        }
    }
    if let Some(q) = before {
        let held = stmts[..i]
            .iter()
            .rev()
            .take_while(|s| quot(s) != Some(q))
            .all(|s| !sets(s, q));
        if held {
            let m = crate::opt::fresh_local(f, ty.clone());
            let stmts = &mut f.blocks[bi].stmts;
            stmts[i] = Statement::Assign {
                target: t,
                rvalue: bin(PrimOp::WrappingSub, a, Operand::Copy(m)),
            };
            stmts.insert(
                i,
                Statement::Assign {
                    target: m,
                    rvalue: bin(PrimOp::WrappingMul, Operand::Copy(q), c),
                },
            );
            return Some(2);
        }
    }
    // A quotient after, `a` unchanged up to it: made here first, and
    // taken there.
    if al == Some(t) {
        return None;
    }
    let mut after = None;
    for (k, s) in stmts.iter().enumerate().skip(i + 1) {
        if quot(s).is_some_and(|q| !pin(q)) {
            after = Some(k);
            break;
        }
        if !keeps_a(s) {
            break;
        }
    }
    let k = after?;
    let q = crate::opt::fresh_local(f, ty.clone());
    let m = crate::opt::fresh_local(f, ty.clone());
    let stmts = &mut f.blocks[bi].stmts;
    let Statement::Assign { rvalue, .. } = &mut stmts[k] else {
        unreachable!()
    };
    *rvalue = RValue::Use(Operand::Copy(q));
    stmts[i] = Statement::Assign {
        target: t,
        rvalue: bin(PrimOp::WrappingSub, a.clone(), Operand::Copy(m)),
    };
    stmts.insert(
        i,
        Statement::Assign {
            target: m,
            rvalue: bin(PrimOp::WrappingMul, Operand::Copy(q), c.clone()),
        },
    );
    stmts.insert(
        i,
        Statement::Assign {
            target: q,
            rvalue: bin(PrimOp::Div, a, c),
        },
    );
    Some(3)
}
