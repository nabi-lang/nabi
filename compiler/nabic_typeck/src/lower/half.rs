//! `f16` / `bf16` are library structs over their bits that the compiler
//! knows by path. A number literal in such a slot, or suffixed `f16` /
//! `bf16`, is its bits rounded once from the decimal:
//!
//! ```text
//! let h: f16 = 1.5;      ==>  F16::from_bits(15872u16)
//! let b = -2bf16;         ==>  BF16::from_bits(49152u16)
//! ```
//!
//! `x as T` with `f16` / `bf16` on either side is a conversion call:
//! `x as f16` -> `F16::from_f64(x)` (`from_f32`, `from_i64`, `from_u64`
//! by the source), `h as i32` -> `h.to_f32() as i32`.

#[allow(clippy::wildcard_imports)]
use crate::lower::*;
use nabic_ast::{CallArg, PathSegment};
use nabic_diagnostic::{Code, Span};
use nabir_abi::half::{self as soft, Format};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Half {
    F16,
    BF16,
}

impl Half {
    const fn slot(self) -> &'static str {
        match self {
            Self::F16 => "f16_type",
            Self::BF16 => "bf16_type",
        }
    }

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::F16 => "f16",
            Self::BF16 => "bf16",
        }
    }

    const fn format(self) -> Format {
        match self {
            Self::F16 => soft::F16,
            Self::BF16 => soft::BF16,
        }
    }

    /// The literal suffix naming it.
    fn of_suffix(text: &str) -> Option<Self> {
        if text.ends_with("bf16") {
            Some(Self::BF16)
        } else if text.ends_with("f16") {
            Some(Self::F16)
        } else {
            None
        }
    }
}

/// Which of `f16` / `bf16` `ty` is.
pub(crate) fn half_of(ctx: &LowerCtx<'_>, ty: &HirTy) -> Option<Half> {
    let HirTy::Struct(id) = ty else { return None };
    [Half::F16, Half::BF16]
        .into_iter()
        .find(|h| ctx.lookup_struct(ctx.lang(h.slot())) == Some(*id))
}

/// The one `f16` / `bf16` member of an expected sum without another
/// number member.
fn expected_half(ctx: &LowerCtx<'_>) -> Option<Half> {
    match ctx.current_expected()? {
        HirTy::Sum(vs) => {
            let mut halves = vs.iter().filter_map(|v| half_of(ctx, v));
            let h = halves.next()?;
            let other = vs.iter().any(|v| match v {
                HirTy::Prim(p) => p.is_int() || p.is_float(),
                v => half_of(ctx, v).is_none() && crate::lower::bignum::big_of(ctx, v).is_some(),
            });
            (halves.next().is_none() && !other).then_some(h)
        }
        t => half_of(ctx, t),
    }
}

fn path_call(h: Half, method: &str, arg: Expr, span: Span) -> Expr {
    let ty = match h {
        Half::F16 => "core::num::half::F16",
        Half::BF16 => "core::num::half::BF16",
    };
    Expr {
        kind: ExprKind::Call {
            callee: Box::new(crate::lower::bignum::path_expr(ty, method, span)),
            args: vec![CallArg::Positional(arg)],
            trailing_closure: None,
        },
        span,
    }
}

/// `lit` (negated if `negate`) as its bits; `None` when it is neither in
/// an `f16` / `bf16` slot nor suffixed so.
pub(crate) fn literal(
    ctx: &mut LowerCtx<'_>,
    lit: &AstLit,
    negate: bool,
    span: Span,
) -> Option<HirExpr> {
    let (h, text) = match lit {
        AstLit::Float(t) => match Half::of_suffix(t) {
            Some(h) => (h, t.trim_end_matches(h.name())),
            None if !crate::lower::expr::has_float_suffix(t) => (expected_half(ctx)?, t.as_str()),
            None => return None,
        },
        AstLit::Int(t) if !crate::lower::expr::has_int_suffix(t) => {
            (expected_half(ctx)?, t.as_str())
        }
        _ => return None,
    };
    let bits = if matches!(lit, AstLit::Int(_)) {
        int_bits(ctx, h, text, span)
    } else {
        float_bits(ctx, h, text, span)
    };
    let bits = match bits {
        Ok(b) => b,
        Err(e) => return Some(e),
    };
    let bits = if negate { bits ^ 0x8000 } else { bits };
    let arg = Expr {
        kind: ExprKind::Literal(AstLit::Int(format!("{bits}u16"))),
        span,
    };
    Some(lower_expr(ctx, &path_call(h, "from_bits", arg, span)))
}

/// An integer literal's bits in `h`: exact, else the error.
fn int_bits(ctx: &LowerCtx<'_>, half: Half, text: &str, span: Span) -> Result<u16, HirExpr> {
    let fmt = half.format();
    let Some((v, _)) = crate::lower::expr::parse_int_literal(text) else {
        return Err(ctx.error_expr(
            Code::LITERAL_OUT_OF_RANGE,
            span,
            format!(
                "integer literal `{text}` is not exactly representable as `{}`",
                half.name()
            ),
        ));
    };
    let b = u64::try_from(v.unsigned_abs())
        .map(|m| soft::from_int(fmt, false, m))
        .ok()
        .filter(|b| soft::to_f64(fmt, *b) as i128 == v.abs());
    let Some(b) = b else {
        let e = ctx.error_expr(
            Code::LITERAL_OUT_OF_RANGE,
            span,
            format!(
                "integer literal `{text}` is not exactly representable as `{}`",
                half.name()
            ),
        );
        if let Ok(m) = u64::try_from(v.unsigned_abs()) {
            let near = soft::to_f64(fmt, soft::from_int(fmt, v < 0, m));
            ctx.help_last(Some(format!(
                "the nearest `{}` is {near}; write that, or use `f32` for the exact value",
                half.name()
            )));
        }
        return Err(e);
    };
    Ok(b)
}

/// A float literal's bits in `h`, rounded once; inf is the error.
fn float_bits(ctx: &LowerCtx<'_>, half: Half, text: &str, span: Span) -> Result<u16, HirExpr> {
    let fmt = half.format();
    let clean: String = text.chars().filter(|c| *c != '_').collect();
    let Some(b) = soft::parse(fmt, &clean) else {
        return Err(ctx.error_expr(
            Code::MALFORMED_NUMBER,
            span,
            format!("malformed float literal `{text}`"),
        ));
    };
    if soft::to_f32(fmt, b).is_infinite() {
        let (max, wide) = if half == Half::F16 {
            ("65504", "f32")
        } else {
            ("3.39e38", "f64")
        };
        let e = ctx.error_expr(
            Code::LITERAL_OUT_OF_RANGE,
            span,
            format!("float literal out of range for `{}`", half.name()),
        );
        ctx.help_last(Some(format!(
            "the largest `{}` is about {max}; write the value as `{wide}` instead",
            half.name()
        )));
        return Err(e);
    }
    Ok(b)
}

/// `operand as to` when either side is `f16` / `bf16` (`from_ty` is the
/// operand's type).
pub(crate) fn cast(
    ctx: &mut LowerCtx<'_>,
    operand: &Expr,
    from_ty: &HirTy,
    to_ty: &HirTy,
    to: &nabic_ast::Ty,
    span: Span,
) -> HirExpr {
    if from_ty == to_ty {
        return lower_expr(ctx, operand);
    }
    let prim_ty = |name: &str| nabic_ast::Ty {
        kind: nabic_ast::TyKind::Path(AstPath {
            segments: vec![PathSegment {
                name: name.to_owned(),
                generics: Vec::new(),
            }],
            span,
            origin: None,
        }),
        span,
    };
    let as_ty = |e: Expr, name: &str| Expr {
        kind: ExprKind::Cast(Box::new(e), prim_ty(name)),
        span,
    };
    let to_f32 = |e: Expr| Expr {
        kind: ExprKind::MethodCall {
            receiver: Box::new(e),
            method: "to_f32".to_owned(),
            type_args: Vec::new(),
            args: Vec::new(),
            trailing_closure: None,
            origin: Some("core::num::half".to_owned()),
            written: false,
        },
        span,
    };
    let e = match (half_of(ctx, from_ty), half_of(ctx, to_ty), to_ty) {
        // Between the two: exact into `f32`, rounded once out of it.
        (Some(_), Some(h), _) => path_call(h, "from_f32", to_f32(operand.clone()), span),
        (Some(_), None, HirTy::Prim(p)) if p.is_int() || p.is_float() => {
            let wide = to_f32(operand.clone());
            if *p == PrimTy::F32 {
                wide
            } else {
                Expr {
                    kind: ExprKind::Cast(Box::new(wide), to.clone()),
                    span,
                }
            }
        }
        (None, Some(h), _) => match from_ty {
            HirTy::Prim(PrimTy::F32) => path_call(h, "from_f32", operand.clone(), span),
            HirTy::Prim(PrimTy::F64) => path_call(h, "from_f64", operand.clone(), span),
            HirTy::Prim(PrimTy::I64) => path_call(h, "from_i64", operand.clone(), span),
            HirTy::Prim(PrimTy::U64) => path_call(h, "from_u64", operand.clone(), span),
            HirTy::Prim(p) if p.is_signed_int() => {
                path_call(h, "from_i64", as_ty(operand.clone(), "i64"), span)
            }
            HirTy::Prim(p) if p.is_unsigned_int() => {
                path_call(h, "from_u64", as_ty(operand.clone(), "u64"), span)
            }
            _ => return bad_cast(ctx, from_ty, to_ty, span),
        },
        _ => return bad_cast(ctx, from_ty, to_ty, span),
    };
    lower_expr(ctx, &e)
}

fn bad_cast(ctx: &LowerCtx<'_>, from_ty: &HirTy, to_ty: &HirTy, span: Span) -> HirExpr {
    let (from, to) = (ctx.ty_name(from_ty), ctx.ty_name(to_ty));
    let e = ctx.error_expr(
        Code::INVALID_CAST,
        span,
        format!("cannot cast `{from}` as `{to}`"),
    );
    ctx.help_last(Some(
        "`as` converts `f16` / `bf16` to and from the integer and float primitives; \
         for `int` / `float` go through `f64` (`x.to_f64() as f16`)"
            .to_owned(),
    ));
    e
}

/// Whether a float literal of value `v` stays finite in half type `ty`.
pub(crate) fn fits(ctx: &LowerCtx<'_>, ty: &HirTy, v: f64) -> bool {
    half_of(ctx, ty)
        .is_some_and(|h| soft::to_f32(h.format(), soft::from_f64(h.format(), v)).is_finite())
}
