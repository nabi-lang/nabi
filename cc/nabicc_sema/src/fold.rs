//! Constant expressions: what a static object's initializer stores.

use crate::check::Cx;
use crate::{Arith, Cmp, FuncId, GlobalId, TExpr, TK};
use nabicc_parse::ty::{Type, TypeId, Types};
use nabicc_target::Prim;
use std::cmp::Ordering::{Equal, Greater, Less};

/// A constant value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Const {
    Int(i128),
    Float(f64),
    /// An address: an object's or function's plus a byte offset.
    Addr(Sym, i64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sym {
    Global(GlobalId),
    Func(FuncId),
}

impl Cx<'_> {
    /// `e`'s value, if it is a constant expression.
    pub(crate) fn fold(&self, e: &TExpr) -> Option<Const> {
        fold(&self.types, e)
    }
}

/// What folding asks of the types.
struct Ty<'a>(&'a Types);

impl Ty<'_> {
    fn prim_of(&self, t: TypeId) -> Option<Prim> {
        let types = self.0;
        match types.get(types.canon(t)) {
            Type::Prim(p) => Some(*p),
            Type::Enum(e) => Some(
                types.enums[*e as usize]
                    .int
                    .and_then(|i| types.prim_of(i))
                    .unwrap_or(Prim::Int),
            ),
            _ => None,
        }
    }

    fn is_void(&self, t: TypeId) -> bool {
        self.prim_of(t) == Some(Prim::Void)
    }

    fn wrap(&self, p: Prim, v: i128) -> i128 {
        wrap(self.0, p, v)
    }
}

/// `v` wrapped to integer type `p` of the target.
#[must_use]
pub fn wrap(types: &Types, p: Prim, v: i128) -> i128 {
    let t = &types.t;
    if p == Prim::Bool {
        return i128::from(v != 0);
    }
    let bits = t.bits(p);
    if bits >= 128 {
        return v;
    }
    let m = v & ((1i128 << bits) - 1);
    if t.signed(p) && (m >> (bits - 1)) & 1 == 1 {
        m - (1i128 << bits)
    } else {
        m
    }
}

/// `e`'s value if it is a constant expression.
#[must_use]
pub fn fold(types: &Types, e: &TExpr) -> Option<Const> {
    fold_in(&Ty(types), e)
}

fn fold_in(cx: &Ty<'_>, e: &TExpr) -> Option<Const> {
    Some(match &e.kind {
        TK::Int(v) => Const::Int(*v),
        TK::Float(v) => Const::Float(*v),
        TK::Conv(x) => {
            let v = fold_in(cx, x)?;
            if cx.is_void(e.ty) {
                return None;
            }
            match (v, cx.prim_of(e.ty)) {
                (Const::Int(i), Some(p)) if p.is_integer() => Const::Int(cx.wrap(p, i)),
                (Const::Int(i), Some(p)) if p.is_float() => Const::Float(round(p, i as f64)),
                (Const::Float(f), Some(p)) if p.is_integer() => {
                    if p == Prim::Bool {
                        Const::Int(i128::from(f != 0.0))
                    } else {
                        Const::Int(cx.wrap(p, f as i128))
                    }
                }
                (Const::Float(f), Some(p)) if p.is_float() => Const::Float(round(p, f)),
                (Const::Int(i), None) => Const::Int(i),
                (Const::Addr(s, o), None) => Const::Addr(s, o),
                // An address as an integer as wide as a pointer.
                (Const::Addr(s, o), Some(p))
                    if p.is_integer() && cx.0.t.prim(p).0 == cx.0.t.ptr =>
                {
                    Const::Addr(s, o)
                }
                (Const::Addr(..), Some(Prim::Bool)) => Const::Int(1),
                _ => return None,
            }
        }
        TK::AddrOf(x) | TK::Decay(x) => addr(cx, x)?,
        TK::PtrAdd(p, i, step) => match (fold_in(cx, p)?, fold_in(cx, i)?) {
            (Const::Addr(s, o), Const::Int(n)) => Const::Addr(s, o + (n as i64) * (*step as i64)),
            (Const::Int(a), Const::Int(n)) => Const::Int(a + n * i128::from(*step)),
            _ => return None,
        },
        TK::PtrDiff(a, b, step) => match (fold_in(cx, a)?, fold_in(cx, b)?) {
            (Const::Addr(s, x), Const::Addr(t, y)) if s == t => {
                Const::Int(i128::from((x - y) / (*step).max(1) as i64))
            }
            (Const::Int(x), Const::Int(y)) => Const::Int((x - y) / i128::from((*step).max(1))),
            _ => return None,
        },
        TK::Neg(x) => match fold_in(cx, x)? {
            Const::Int(v) => Const::Int(cx.wrap(cx.prim_of(e.ty)?, -v)),
            Const::Float(v) => Const::Float(-v),
            Const::Addr(..) => return None,
        },
        TK::BitNot(x) => match fold_in(cx, x)? {
            Const::Int(v) => Const::Int(cx.wrap(cx.prim_of(e.ty)?, !v)),
            _ => return None,
        },
        TK::Not(x) => match fold_in(cx, x)? {
            Const::Int(v) => Const::Int(i128::from(v == 0)),
            Const::Float(v) => Const::Int(i128::from(v == 0.0)),
            Const::Addr(..) => Const::Int(0),
        },
        TK::Bin(op, a, b) => {
            let p = cx.prim_of(e.ty)?;
            match (fold_in(cx, a)?, fold_in(cx, b)?) {
                (Const::Int(x), Const::Int(y)) => {
                    let bits = cx.0.t.bits(p);
                    let v = match op {
                        Arith::Add => x.wrapping_add(y),
                        Arith::Sub => x.wrapping_sub(y),
                        Arith::Mul => x.wrapping_mul(y),
                        Arith::Div if y != 0 => x / y,
                        Arith::Rem if y != 0 => x % y,
                        Arith::Shl if (0..i128::from(bits)).contains(&y) => x << y,
                        Arith::Shr if (0..i128::from(bits)).contains(&y) => x >> y,
                        Arith::And => x & y,
                        Arith::Or => x | y,
                        Arith::Xor => x ^ y,
                        _ => return None,
                    };
                    Const::Int(cx.wrap(p, v))
                }
                (Const::Float(x), Const::Float(y)) => Const::Float(round(
                    p,
                    match op {
                        Arith::Add => x + y,
                        Arith::Sub => x - y,
                        Arith::Mul => x * y,
                        Arith::Div => x / y,
                        _ => return None,
                    },
                )),
                _ => return None,
            }
        }
        TK::Cmp(c, a, b) => {
            let r = match (fold_in(cx, a)?, fold_in(cx, b)?) {
                (Const::Int(x), Const::Int(y)) => cmp(*c, x.cmp(&y)),
                (Const::Float(x), Const::Float(y)) => match x.partial_cmp(&y) {
                    Some(o) => cmp(*c, o),
                    None => *c == Cmp::Ne,
                },
                _ => return None,
            };
            Const::Int(i128::from(r))
        }
        TK::And(a, b) => Const::Int(i128::from(truth(fold_in(cx, a)?) && truth(fold_in(cx, b)?))),
        TK::Or(a, b) => Const::Int(i128::from(truth(fold_in(cx, a)?) || truth(fold_in(cx, b)?))),
        TK::Cond(c, a, b) => {
            if truth(fold_in(cx, c)?) {
                fold_in(cx, a)?
            } else {
                fold_in(cx, b)?
            }
        }
        _ => return None,
    })
}

fn truth(c: Const) -> bool {
    match c {
        Const::Int(v) => v != 0,
        Const::Float(v) => v != 0.0,
        Const::Addr(..) => true,
    }
}

fn cmp(c: Cmp, o: std::cmp::Ordering) -> bool {
    match c {
        Cmp::Eq => o == Equal,
        Cmp::Ne => o != Equal,
        Cmp::Lt => o == Less,
        Cmp::Le => o != Greater,
        Cmp::Gt => o == Greater,
        Cmp::Ge => o != Less,
    }
}

fn round(p: Prim, v: f64) -> f64 {
    if p == Prim::Float {
        f64::from(v as f32)
    } else {
        v
    }
}

/// The address of lvalue (or function) `e`, if constant.
fn addr(cx: &Ty<'_>, e: &TExpr) -> Option<Const> {
    Some(match &e.kind {
        TK::Global(g) => Const::Addr(Sym::Global(*g), 0),
        TK::Func(f) => Const::Addr(Sym::Func(*f), 0),
        TK::Member(b, off, None) => match addr(cx, b)? {
            Const::Addr(s, o) => Const::Addr(s, o + *off as i64),
            Const::Int(v) => Const::Int(v + i128::from(*off)),
            Const::Float(_) => return None,
        },
        TK::Deref(p) => fold_in(cx, p)?,
        _ => return None,
    })
}
