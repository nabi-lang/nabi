//! Primitive operators and casts on values.

#[allow(clippy::wildcard_imports)]
use crate::*;

/// A primitive's bits read back as primitive `to` (a union read).
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub(crate) fn reinterpret(v: Value, to: &HirTy) -> Option<Value> {
    let tp = prim_of(to)?;
    let bits: u64 = match v {
        Value::Int(i, p) => (i as u64) & (u64::MAX >> (64 - p.bit_width(pointer_width()))),
        Value::Char(c) => u64::from(c),
        Value::Bool(b) => u64::from(b),
        Value::Float(x, PrimTy::F32) => u64::from((x as f32).to_bits()),
        Value::Float(x, _) => x.to_bits(),
        _ => return None,
    };
    Some(match tp {
        PrimTy::Bool => Value::Bool(bits & 1 == 1),
        PrimTy::Char | PrimTy::CChar => Value::Char(bits as u32),
        PrimTy::F32 => Value::Float(f64::from(f32::from_bits(bits as u32)), tp),
        PrimTy::F64 => Value::Float(f64::from_bits(bits), tp),
        p if p.is_int() => Value::Int(wrap(i128::from(bits), p), p),
        _ => return None,
    })
}

/// The zeroed element of a fresh buffer.
pub(crate) const fn zero(ty: &HirTy) -> Value {
    match ty {
        HirTy::Prim(PrimTy::Bool) => Value::Bool(false),
        HirTy::Prim(p) if p.is_float() => Value::Float(0.0, *p),
        HirTy::Prim(PrimTy::Char | PrimTy::CChar) => Value::Char(0),
        HirTy::Prim(p) if p.is_int() => Value::Int(0, *p),
        _ => Value::Unit,
    }
}

pub(crate) fn index_of(i: &Value, len: usize, span: Span) -> R<usize> {
    let Value::Int(i, _) = i else {
        return Err(Stop::Poisoned);
    };
    match usize::try_from(*i) {
        Ok(slot) if slot < len => Ok(slot),
        _ => Err(Stop::Fail(
            span,
            format!("index out of bounds: the len is {len} but the index is {i}"),
        )),
    }
}

pub(crate) fn checked(v: Option<i128>, p: PrimTy, what: &str, span: Span) -> R<Value> {
    let (lo, hi) = int_range(p);
    match v {
        Some(v) if (lo..=hi).contains(&v) => Ok(Value::Int(v, p)),
        _ => Err(Stop::Fail(span, format!("attempt to {what} with overflow"))),
    }
}

pub(crate) fn shift_amount(r: i128, p: PrimTy, span: Span, dir: &str) -> R<u32> {
    let bits = p.bit_width(pointer_width());
    match u32::try_from(r) {
        Ok(n) if n < bits => Ok(n),
        _ => Err(Stop::Fail(
            span,
            format!("attempt to shift {dir} with overflow"),
        )),
    }
}

#[allow(clippy::too_many_lines, clippy::float_cmp)]
pub(crate) fn binop(op: PrimOp, l: Value, r: Value, span: Span) -> R<Value> {
    let cmp = |o: std::cmp::Ordering| -> Value {
        Value::Bool(match op {
            O::Eq => o.is_eq(),
            O::Ne => o.is_ne(),
            O::Lt => o.is_lt(),
            O::Le => o.is_le(),
            O::Gt => o.is_gt(),
            _ => o.is_ge(),
        })
    };
    let is_cmp = matches!(op, O::Eq | O::Ne | O::Lt | O::Le | O::Gt | O::Ge);
    match (l, r) {
        (Value::Int(a, p), Value::Int(b, _)) => {
            if is_cmp {
                return Ok(cmp(a.cmp(&b)));
            }
            #[allow(clippy::cast_sign_loss, clippy::cast_possible_wrap)]
            match op {
                O::Add => checked(a.checked_add(b), p, "add", span),
                O::Sub => checked(a.checked_sub(b), p, "subtract", span),
                O::Mul => checked(a.checked_mul(b), p, "multiply", span),
                O::Div | O::Rem | O::Mod => {
                    let div = op == O::Div;
                    if b == 0 {
                        return Err(Stop::Fail(
                            span,
                            if div {
                                "attempt to divide by zero"
                            } else {
                                "attempt to calculate the remainder with a divisor of zero"
                            }
                            .to_owned(),
                        ));
                    }
                    let what = if div {
                        "divide"
                    } else {
                        "calculate the remainder"
                    };
                    if p.is_signed_int() && b == -1 && a == int_range(p).0 {
                        return Err(Stop::Fail(span, format!("attempt to {what} with overflow")));
                    }
                    let r = if div { a / b } else { a % b };
                    let floor = op == O::Mod && r != 0 && (r < 0) != (b < 0);
                    Ok(Value::Int(if floor { r + b } else { r }, p))
                }
                O::BitAnd => Ok(Value::Int(a & b, p)),
                O::BitOr => Ok(Value::Int(a | b, p)),
                O::BitXor => Ok(Value::Int(a ^ b, p)),
                O::Shl => {
                    let n = shift_amount(b, p, span, "left")?;
                    Ok(Value::Int(wrap(((a as u128) << n) as i128, p), p))
                }
                O::Shr => {
                    let n = shift_amount(b, p, span, "right")?;
                    Ok(Value::Int(a >> n, p))
                }
                O::WrappingAdd => Ok(Value::Int(wrap(a.wrapping_add(b), p), p)),
                O::WrappingSub => Ok(Value::Int(wrap(a.wrapping_sub(b), p), p)),
                O::WrappingMul => Ok(Value::Int(wrap(a.wrapping_mul(b), p), p)),
                _ => Err(Stop::Poisoned),
            }
        }
        (Value::Float(a, p), Value::Float(b, _)) => {
            if is_cmp {
                return Ok(match a.partial_cmp(&b) {
                    Some(o) => cmp(o),
                    None => Value::Bool(op == O::Ne),
                });
            }
            let v = match op {
                O::Add => a + b,
                O::Sub => a - b,
                O::Mul => a * b,
                O::Div => a / b,
                O::Rem => a % b,
                O::Mod => {
                    let r = a % b;
                    if r == 0.0 {
                        0.0f64.copysign(b)
                    } else if (r < 0.0) != (b < 0.0) {
                        r + b
                    } else {
                        r
                    }
                }
                _ => return Err(Stop::Poisoned),
            };
            // `f32` arithmetic on `f32`-exact operands, rounded once:
            // the same as computing in `f32`.
            Ok(Value::Float(round(v, p), p))
        }
        (Value::Bool(a), Value::Bool(b)) => Ok(Value::Bool(match op {
            O::BitAnd | O::LogicalAnd => a & b,
            O::BitOr | O::LogicalOr => a | b,
            O::BitXor => a ^ b,
            _ if is_cmp => return Ok(cmp(a.cmp(&b))),
            _ => return Err(Stop::Poisoned),
        })),
        (Value::Char(a), Value::Char(b)) if is_cmp => Ok(cmp(a.cmp(&b))),
        (Value::Str(a), Value::Str(b)) if is_cmp => Ok(cmp(a.as_bytes().cmp(b.as_bytes()))),
        (Value::Unit, Value::Unit) | (Value::None, Value::None) if is_cmp => {
            Ok(cmp(std::cmp::Ordering::Equal))
        }
        _ => Err(Stop::Poisoned),
    }
}

pub(crate) fn unop(op: PrimOp, v: Value, span: Span) -> R<Value> {
    let bits_of = |p: PrimTy| p.bit_width(pointer_width());
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_wrap)]
    let raw = |a: i128, p: PrimTy| (a as u128) & ((1u128 << bits_of(p)) - 1);
    match (op, v) {
        (PrimOp::Neg, Value::Int(a, p)) => checked(0i128.checked_sub(a), p, "negate", span),
        (PrimOp::Neg, Value::Float(a, p)) => Ok(Value::Float(-a, p)),
        (PrimOp::Not | PrimOp::LogicalNot, Value::Bool(b)) => Ok(Value::Bool(!b)),
        (PrimOp::Not, Value::Int(a, p)) => Ok(Value::Int(wrap(!a, p), p)),
        (PrimOp::Sqrt, Value::Float(a, p)) => {
            #[allow(clippy::cast_possible_truncation)]
            let r = if p == PrimTy::F32 {
                f64::from((a as f32).sqrt())
            } else {
                a.sqrt()
            };
            Ok(Value::Float(r, p))
        }
        (PrimOp::CountOnes, Value::Int(a, p)) => {
            Ok(Value::Int(i128::from(raw(a, p).count_ones()), p))
        }
        (PrimOp::LeadingZeros, Value::Int(a, p)) => Ok(Value::Int(
            i128::from(raw(a, p).leading_zeros() - (128 - bits_of(p))),
            p,
        )),
        (PrimOp::TrailingZeros, Value::Int(a, p)) => {
            let r = raw(a, p);
            let n = if r == 0 {
                bits_of(p)
            } else {
                r.trailing_zeros()
            };
            Ok(Value::Int(i128::from(n), p))
        }
        #[allow(clippy::cast_possible_wrap)]
        (PrimOp::SwapBytes, Value::Int(a, p)) => {
            let swapped = raw(a, p).swap_bytes() >> (128 - bits_of(p));
            Ok(Value::Int(wrap(swapped as i128, p), p))
        }
        _ => Err(Stop::Poisoned),
    }
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
pub(crate) fn cast(v: Value, from: &HirTy, to: &HirTy, span: Span) -> R<Value> {
    let Some(tp) = prim_of(to) else {
        // A buffer seen as another element type (`buf_erase`) is the
        // same buffer.
        let bufs = matches!((from, to), (HirTy::GcBuf(_), HirTy::GcBuf(_)));
        return if from == to || bufs {
            Ok(v)
        } else {
            Err(Stop::Runtime(
                span,
                "casts a non-primitive value".to_owned(),
            ))
        };
    };
    let int_value = match &v {
        Value::Int(a, _) => Some(*a),
        Value::Bool(b) => Some(i128::from(*b)),
        Value::Char(c) => Some(i128::from(*c)),
        _ => None,
    };
    match (int_value, &v) {
        (Some(a), _) if tp == PrimTy::Bool => Ok(Value::Bool(a != 0)),
        (Some(a), _) if matches!(tp, PrimTy::Char | PrimTy::CChar) => {
            Ok(Value::Char(wrap(a, PrimTy::U32) as u32))
        }
        (Some(a), _) if tp.is_int() => Ok(Value::Int(wrap(a, tp), tp)),
        (Some(a), _) if tp == PrimTy::F32 => Ok(Value::Float(f64::from(a as f32), tp)),
        (Some(a), _) if tp == PrimTy::F64 => Ok(Value::Float(a as f64, tp)),
        (None, Value::Float(x, _)) if tp.is_float() => Ok(Value::Float(round(*x, tp), tp)),
        (None, Value::Float(x, _)) if tp.is_int() => {
            let t = x.trunc();
            let (lo, hi) = int_range(tp);
            // `fptosi` / `fptoui` of a value out of range is poison.
            if t.is_nan() || t < lo as f64 || t > hi as f64 || t as i128 > hi {
                return Err(Stop::Fail(
                    span,
                    format!("`{x}` does not fit in `{}`", tp.name()),
                ));
            }
            Ok(Value::Int(t as i128, tp))
        }
        (None, Value::Str(_)) if tp == PrimTy::Str => Ok(v),
        _ => Err(Stop::Runtime(span, "casts a run-time value".to_owned())),
    }
}
