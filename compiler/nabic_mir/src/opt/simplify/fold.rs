//! Folding and algebraic identities.

#[allow(clippy::wildcard_imports)]
use crate::opt::simplify::*;

/// Folding, identities, field forwarding.
#[allow(clippy::too_many_lines)]
pub(crate) fn fold(cx: &Ctx<'_>, st: &State, tt: &MirTy, rv: &RValue) -> Option<RValue> {
    let pw = cx.pw;
    let konst = |c: ConstVal| Some(RValue::Use(Operand::Const(c)));
    match rv {
        // Built again from the fields of one value: that value.
        RValue::StructInit { fields, .. } => {
            let mut whole: Option<&Operand> = None;
            for (k, o) in fields.iter().enumerate() {
                let Operand::Copy(h) = o else { return None };
                let x = st.exprs.iter().find_map(|(e, eh, _)| match &**e {
                    RValue::FieldRead { target, index, .. } if eh == h && *index as usize == k => {
                        Some(target)
                    }
                    _ => None,
                })?;
                if whole.is_some_and(|w| w != x) {
                    return None;
                }
                whole = Some(x);
            }
            let x = whole?;
            let ok = cx.is(x, tt) && !matches!(x, Operand::Copy(l) if cx.pinned.contains(l));
            ok.then(|| RValue::Use(x.clone()))
        }
        // An element of an array built here.
        RValue::ArrayIndex {
            array,
            offset: 0,
            index: Operand::Const(c),
            ..
        } if matches!(cx.ty(*array), MirTy::Array(..)) => {
            let Agg::Fields(es) = &**st.aggs.get(array)? else {
                return None;
            };
            let o = es.get(usize::try_from(int_val(c, PrimTy::Usize, pw)?).ok()?)?;
            (cx.is(o, tt)).then(|| RValue::Use(o.clone()))
        }
        RValue::FieldRead { target, index, .. } => {
            let Operand::Copy(s) = target else {
                return None;
            };
            match &**st.aggs.get(s)? {
                Agg::Fields(fs) => {
                    let o = fs.get(*index as usize)?;
                    (cx.is(o, tt)).then(|| RValue::Use(o.clone()))
                }
                Agg::Elem0(buf, ty) => {
                    let fty = match ty {
                        MirTy::Struct(_, fs) | MirTy::Tuple(fs) => fs.get(*index as usize)?,
                        _ => return None,
                    };
                    (fty == tt).then(|| RValue::HeapField {
                        ptr: buf.clone(),
                        ty: ty.clone(),
                        index: *index,
                    })
                }
            }
        }
        RValue::UnaryOp {
            op,
            op_ty: MirTy::Prim(p),
            operand,
        } => {
            let p = *p;
            if *op == PrimOp::LogicalNot && p == PrimTy::Bool {
                return bool_of(operand).and_then(|b| konst(ConstVal::Bool(!b)));
            }
            let Operand::Const(c) = operand else {
                return None;
            };
            if tt != &MirTy::Prim(p) {
                return None;
            }
            let v = int_val(c, p, pw)?;
            let (lo, hi) = int_range(p, pw)?;
            let bits = p.bit_width(pw);
            let wrap = |x: i128| wrap_to(x, p, pw);
            let r = match op {
                PrimOp::Neg if p.is_signed_int() && v != lo => -v,
                PrimOp::Not => wrap(!v),
                PrimOp::CountOnes => i128::from((v & ((1i128 << bits) - 1)).count_ones()),
                _ => return None,
            };
            (lo..=hi).contains(&r).then_some(())?;
            konst(int_const(r, p))
        }
        // `(x + c) - c` wrapping is `x` (an iterator's `i += 1` then
        // `at(i - 1)`).
        RValue::BinOp {
            op: PrimOp::WrappingSub,
            op_ty: MirTy::Prim(p),
            lhs: Operand::Copy(y),
            rhs: c @ Operand::Const(_),
        } if st.exprs.iter().any(|(e, h, _)| {
            h == y
                && matches!(&**e,
                RValue::BinOp { op: PrimOp::WrappingAdd, op_ty: MirTy::Prim(p2), rhs: c2, .. }
                    if p2 == p && c2 == c)
        }) =>
        {
            st.exprs.iter().find_map(|(e, h, _)| match &**e {
                RValue::BinOp {
                    op: PrimOp::WrappingAdd,
                    lhs: x,
                    rhs: c2,
                    ..
                } if h == y && c2 == c && cx.is(x, tt) => Some(RValue::Use(x.clone())),
                _ => None,
            })
        }
        RValue::BinOp {
            op,
            op_ty: MirTy::Prim(p),
            lhs,
            rhs,
        } => fold_bin(cx, st, tt, *op, *p, lhs, rhs)
            .or_else(|| rotate(cx, st, tt, *op, *p, lhs, rhs)),
        RValue::Cast {
            source: Operand::Const(c),
            from_ty: MirTy::Prim(a),
            to_ty: MirTy::Prim(b),
        } if tt == &MirTy::Prim(*b) => {
            let v = int_val(c, *a, pw)?;
            int_range(*b, pw)?;
            konst(int_const(wrap_to(v, *b, pw), *b))
        }
        RValue::Cast {
            source,
            from_ty,
            to_ty,
        } if from_ty == to_ty && cx.is(source, tt) && tt == to_ty => {
            Some(RValue::Use(source.clone()))
        }
        // `((x as C) as B) as A` of same-width integers, `x` an `A`, is
        // `x` (an index through `u64` and `isize` back to `usize`).
        RValue::Cast {
            source: Operand::Copy(y),
            from_ty: MirTy::Prim(b),
            to_ty: to @ MirTy::Prim(a),
        } if tt == to && same_width(*a, *b, pw) && round_trip(cx, st, *y, *a, pw).is_some() => {
            round_trip(cx, st, *y, *a, pw).map(RValue::Use)
        }
        // `(x as B) as A` of same-width integers is `x`.
        RValue::Cast {
            source: Operand::Copy(y),
            from_ty: MirTy::Prim(b),
            to_ty: to @ MirTy::Prim(a),
        } if tt == to && same_width(*a, *b, pw) => {
            st.exprs.iter().find_map(|(rv, h, _)| match &**rv {
                RValue::Cast {
                    source: x,
                    from_ty,
                    to_ty: MirTy::Prim(b2),
                } if h == y && from_ty == to && b2 == b && cx.is(x, tt) => {
                    Some(RValue::Use(x.clone()))
                }
                _ => None,
            })
        }
        _ => None,
    }
}

/// `(x << c) | (x >> (w - c))` of a 32- or 64-bit unsigned `x`, the
/// shifts in either order: `x` rotated left by `c`.
pub(crate) fn rotate(
    cx: &Ctx<'_>,
    st: &State,
    tt: &MirTy,
    op: PrimOp,
    p: PrimTy,
    lhs: &Operand,
    rhs: &Operand,
) -> Option<RValue> {
    let w = i128::from(p.bit_width(cx.pw));
    if op != PrimOp::BitOr || !p.is_unsigned_int() || !(w == 32 || w == 64) {
        return None;
    }
    if tt != &MirTy::Prim(p) {
        return None;
    }
    // The shift a local holds: its operand and constant amount.
    let shift = |o: &Operand, want: PrimOp| {
        let Operand::Copy(y) = o else { return None };
        st.exprs.iter().find_map(|(e, h, _)| match &**e {
            RValue::BinOp {
                op,
                op_ty: MirTy::Prim(q),
                lhs: x,
                rhs: Operand::Const(c),
            } if h == y && *op == want && *q == p => Some((x.clone(), int_val(c, p, cx.pw)?)),
            _ => None,
        })
    };
    let (l, r) = match shift(lhs, PrimOp::Shl) {
        Some(l) => (l, shift(rhs, PrimOp::Shr)?),
        None => (shift(rhs, PrimOp::Shl)?, shift(lhs, PrimOp::Shr)?),
    };
    let ((x, a), (y, b)) = (l, r);
    if x != y || !cx.is(&x, tt) || !(1..w).contains(&a) || a + b != w {
        return None;
    }
    Some(RValue::BinOp {
        op: PrimOp::RotateLeft,
        op_ty: MirTy::Prim(p),
        lhs: x,
        rhs: Operand::Const(int_const(a, p)),
    })
}

/// The value of type `a` that `y` holds cast through two or three
/// integer types of its width.
fn round_trip(cx: &Ctx<'_>, st: &State, y: LocalId, a: PrimTy, pw: u32) -> Option<Operand> {
    let def = |l: LocalId| {
        st.exprs.iter().find_map(|(rv, h, _)| match &**rv {
            RValue::Cast {
                source,
                from_ty: MirTy::Prim(c),
                ..
            } if *h == l && same_width(*c, a, pw) => Some((source.clone(), *c)),
            _ => None,
        })
    };
    let (mut x, _) = def(y)?;
    for _ in 0..2 {
        let Operand::Copy(l) = x else { return None };
        let (src, c) = def(l)?;
        if c == a && cx.is(&src, &MirTy::Prim(a)) {
            return Some(src);
        }
        x = src;
    }
    None
}

/// Integer types of one width (a cast between them keeps the bits).
pub(crate) fn same_width(a: PrimTy, b: PrimTy, pw: u32) -> bool {
    let int = |p: PrimTy| p.is_signed_int() || p.is_unsigned_int();
    int(a) && int(b) && a.bit_width(pw) == b.bit_width(pw)
}

/// `v` wrapped into `p`'s range (two's complement).
pub(crate) const fn wrap_to(v: i128, p: PrimTy, pw: u32) -> i128 {
    let bits = p.bit_width(pw);
    let m = v & ((1i128 << bits) - 1);
    if p.is_signed_int() && m >= 1i128 << (bits - 1) {
        m - (1i128 << bits)
    } else {
        m
    }
}

pub(crate) const fn konst(c: ConstVal) -> RValue {
    RValue::Use(Operand::Const(c))
}

/// `o` itself, where it has the result's type `tt`.
pub(crate) fn same(cx: &Ctx<'_>, tt: &MirTy, o: &Operand) -> Option<RValue> {
    (cx.is(o, tt)).then(|| RValue::Use(o.clone()))
}

pub(crate) fn fold_bin(
    cx: &Ctx<'_>,
    st: &State,
    tt: &MirTy,
    op: PrimOp,
    p: PrimTy,
    lhs: &Operand,
    rhs: &Operand,
) -> Option<RValue> {
    if is_cmp(op) {
        return if tt == &MirTy::Prim(PrimTy::Bool) {
            fold_cmp(cx, st, op, p, lhs, rhs)
        } else {
            None
        };
    }
    if tt != &MirTy::Prim(p) {
        return None;
    }
    if p == PrimTy::Bool {
        return fold_bool(cx, tt, op, lhs, rhs);
    }
    if !(p.is_signed_int() || p.is_unsigned_int()) {
        return None;
    }
    fold_int(cx, tt, op, p, lhs, rhs).or_else(|| step(st, op, p, lhs, rhs))
}

pub(crate) fn fold_cmp(
    cx: &Ctx<'_>,
    st: &State,
    op: PrimOp,
    p: PrimTy,
    lhs: &Operand,
    rhs: &Operand,
) -> Option<RValue> {
    let pw = cx.pw;
    if p == PrimTy::Bool {
        if let (Some(a), Some(b)) = (bool_of(lhs), bool_of(rhs)) {
            return match op {
                PrimOp::Eq => Some(konst(ConstVal::Bool(a == b))),
                PrimOp::Ne => Some(konst(ConstVal::Bool(a != b))),
                _ => None,
            };
        }
    }
    if p.is_signed_int() || p.is_unsigned_int() {
        if let (Operand::Const(a), Operand::Const(b)) = (lhs, rhs) {
            let (a, b) = (int_val(a, p, pw)?, int_val(b, p, pw)?);
            return Some(konst(ConstVal::Bool(match op {
                PrimOp::Lt => a < b,
                PrimOp::Le => a <= b,
                PrimOp::Gt => a > b,
                PrimOp::Ge => a >= b,
                PrimOp::Eq => a == b,
                _ => a != b,
            })));
        }
        if lhs == rhs {
            return Some(konst(ConstVal::Bool(matches!(
                op,
                PrimOp::Le | PrimOp::Ge | PrimOp::Eq
            ))));
        }
        // Unsigned `x >= 0` / `x < 0`.
        let zero = matches!(rhs, Operand::Const(b) if int_val(b, p, pw) == Some(0));
        if p.is_unsigned_int() && zero {
            match op {
                PrimOp::Ge => return Some(konst(ConstVal::Bool(true))),
                PrimOp::Lt => return Some(konst(ConstVal::Bool(false))),
                _ => {}
            }
        }
    }
    if !fact_ty(&MirTy::Prim(p)) {
        return None;
    }
    let q = Fact {
        op,
        ty: p,
        a: lhs.clone(),
        b: rhs.clone(),
    };
    decide(&st.facts, &q).map(|v| konst(ConstVal::Bool(v)))
}

pub(crate) fn fold_bool(
    cx: &Ctx<'_>,
    tt: &MirTy,
    op: PrimOp,
    lhs: &Operand,
    rhs: &Operand,
) -> Option<RValue> {
    match (op, bool_of(lhs), bool_of(rhs)) {
        (BitAnd | LogicalAnd, Some(x), Some(y)) => Some(konst(ConstVal::Bool(x && y))),
        (BitOr | LogicalOr, Some(x), Some(y)) => Some(konst(ConstVal::Bool(x || y))),
        (BitXor, Some(x), Some(y)) => Some(konst(ConstVal::Bool(x != y))),
        (BitAnd | LogicalAnd, Some(true), None)
        | (BitOr | LogicalOr | BitXor, Some(false), None) => same(cx, tt, rhs),
        (BitAnd | LogicalAnd, None, Some(true))
        | (BitOr | LogicalOr | BitXor, None, Some(false)) => same(cx, tt, lhs),
        (BitAnd | LogicalAnd, _, Some(false)) | (BitAnd | LogicalAnd, Some(false), _) => {
            Some(konst(ConstVal::Bool(false)))
        }
        (BitOr | LogicalOr, _, Some(true)) | (BitOr | LogicalOr, Some(true), _) => {
            Some(konst(ConstVal::Bool(true)))
        }
        (BitAnd | LogicalAnd | BitOr | LogicalOr, ..) if lhs == rhs => same(cx, tt, lhs),
        (BitXor, ..) if lhs == rhs => Some(konst(ConstVal::Bool(false))),
        _ => None,
    }
}

/// Integer folding and identities. Checked `+ - *` fold only where they
/// do not overflow: else they trap, and still must.
pub(crate) fn fold_int(
    cx: &Ctx<'_>,
    tt: &MirTy,
    op: PrimOp,
    p: PrimTy,
    lhs: &Operand,
    rhs: &Operand,
) -> Option<RValue> {
    let pw = cx.pw;
    let (lo, hi) = int_range(p, pw)?;
    let bits = i128::from(p.bit_width(pw));
    let ci = |o: &Operand| match o {
        Operand::Const(c) => int_val(c, p, pw),
        Operand::Copy(_) => None,
    };
    let (a, b) = (ci(lhs), ci(rhs));
    let fits = |v: &i128| (lo..=hi).contains(v);
    let wrap = |v: i128| wrap_to(v, p, pw);
    let divides = |d: i128| d != 0 && !(p.is_signed_int() && d == -1);
    if let (Some(a), Some(b)) = (a, b) {
        let v = match op {
            PrimOp::Add => a.checked_add(b).filter(fits),
            PrimOp::Sub => a.checked_sub(b).filter(fits),
            PrimOp::Mul => a.checked_mul(b).filter(fits),
            PrimOp::WrappingAdd => Some(wrap(a.wrapping_add(b))),
            PrimOp::WrappingSub => Some(wrap(a.wrapping_sub(b))),
            PrimOp::WrappingMul => Some(wrap(a.wrapping_mul(b))),
            PrimOp::Div if divides(b) => Some(a / b),
            PrimOp::Rem if divides(b) => Some(a % b),
            PrimOp::BitAnd => Some(a & b),
            PrimOp::BitOr => Some(a | b),
            PrimOp::BitXor => Some(wrap(a ^ b)),
            PrimOp::Shl if (0..bits).contains(&b) => Some(wrap(a << b)),
            // Arithmetic for signed, logical for unsigned (`a` >= 0 then).
            PrimOp::Shr if (0..bits).contains(&b) => Some(a >> b),
            PrimOp::RotateLeft if a >= 0 => {
                let (m, s) = ((1i128 << bits) - 1, b.rem_euclid(bits));
                Some(((a << s) | (a >> ((bits - s) % bits))) & m)
            }
            _ => None,
        };
        return v.filter(fits).map(|v| konst(int_const(v, p)));
    }
    let zero = |c: Option<i128>| c == Some(0);
    let one = |c: Option<i128>| c == Some(1);
    let (add, sub, mul) = (
        op == PrimOp::Add || op == PrimOp::WrappingAdd,
        op == PrimOp::Sub || op == PrimOp::WrappingSub,
        op == PrimOp::Mul || op == PrimOp::WrappingMul,
    );
    if (add && zero(b)) || (sub && zero(b)) || (mul && one(b)) || (op == PrimOp::Div && one(b)) {
        return same(cx, tt, lhs);
    }
    if (add && zero(a)) || (mul && one(a)) {
        return same(cx, tt, rhs);
    }
    match op {
        _ if mul && (zero(a) || zero(b)) => Some(konst(int_const(0, p))),
        _ if sub && lhs == rhs => Some(konst(int_const(0, p))),
        PrimOp::BitOr | PrimOp::BitXor | PrimOp::Shl | PrimOp::Shr if zero(b) => same(cx, tt, lhs),
        PrimOp::BitOr | PrimOp::BitXor if zero(a) => same(cx, tt, rhs),
        PrimOp::BitAnd if zero(a) || zero(b) => Some(konst(int_const(0, p))),
        PrimOp::BitAnd | PrimOp::BitOr if lhs == rhs => same(cx, tt, lhs),
        PrimOp::BitXor if lhs == rhs => Some(konst(int_const(0, p))),
        _ => None,
    }
}

/// A step the facts keep in range needs no overflow check: `x + 1` with
/// `x < y` for some `y`, `x - 1` with `x > y`, and unsigned `a - b` with
/// `b <= a` (or `a` is `x - 1` and `b < x`: then `x` is at least 1).
pub(crate) fn step(
    st: &State,
    op: PrimOp,
    p: PrimTy,
    lhs: &Operand,
    rhs: &Operand,
) -> Option<RValue> {
    if op == PrimOp::Sub && p.is_unsigned_int() && fits_sub(st, p, lhs, rhs) {
        return Some(RValue::BinOp {
            op: PrimOp::WrappingSub,
            op_ty: MirTy::Prim(p),
            lhs: lhs.clone(),
            rhs: rhs.clone(),
        });
    }
    let one = matches!(rhs, Operand::Const(c) if int_val(c, p, st.pw) == Some(1));
    let (want, to) = match op {
        PrimOp::Add if one => (PrimOp::Lt, PrimOp::WrappingAdd),
        PrimOp::Sub if one => (PrimOp::Gt, PrimOp::WrappingSub),
        _ => return None,
    };
    let known = st
        .facts
        .iter()
        .filter(|x| x.ty == p)
        .any(|x| (x.a == *lhs && x.op == want) || (x.b == *lhs && swap(x.op) == want));
    known.then(|| RValue::BinOp {
        op: to,
        op_ty: MirTy::Prim(p),
        lhs: lhs.clone(),
        rhs: rhs.clone(),
    })
}

/// Unsigned `a - b` the facts keep at or above zero.
fn fits_sub(st: &State, p: PrimTy, a: &Operand, b: &Operand) -> bool {
    let holds = |op, x: &Operand, y: &Operand| {
        decide(
            &st.facts,
            &Fact {
                op,
                ty: p,
                a: x.clone(),
                b: y.clone(),
            },
        ) == Some(true)
    };
    if holds(PrimOp::Le, b, a) {
        return true;
    }
    let def = |l: &Operand| {
        let Operand::Copy(l) = l else { return None };
        st.exprs
            .iter()
            .find(|(_, h, load)| h == l && !load)
            .map(|(rv, ..)| rv.clone())
    };
    // `b` is `(a / c) * c` (or `c * (a / c)`): at most `a`.
    if let Some(m) = def(b) {
        if let RValue::BinOp {
            op: PrimOp::Mul | PrimOp::WrappingMul,
            op_ty: MirTy::Prim(q),
            lhs,
            rhs,
        } = &*m
        {
            let quot = |x: &Operand, c: &Operand| {
                matches!(c, Operand::Const(_))
                    && def(x).is_some_and(|d| {
                        matches!(&*d, RValue::BinOp { op: PrimOp::Div, op_ty: MirTy::Prim(r), lhs: y, rhs: z }
                            if r == q && y == a && z == c)
                    })
            };
            if *q == p && (quot(lhs, rhs) || quot(rhs, lhs)) {
                return true;
            }
        }
    }
    let Operand::Copy(t) = a else {
        return false;
    };
    // `a` is `x - 1` (wrapping or not): `b < x` makes it `b <= a`.
    st.exprs.iter().any(|(rv, h, _)| {
        h == t
            && matches!(&**rv, RValue::BinOp {
                op: PrimOp::Sub | PrimOp::WrappingSub,
                op_ty: MirTy::Prim(q),
                lhs: x,
                rhs: Operand::Const(c),
            } if *q == p && int_val(c, p, st.pw) == Some(1) && holds(PrimOp::Lt, b, x))
    })
}
