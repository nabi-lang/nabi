//! Operators, casts and `?`.

#[allow(clippy::wildcard_imports)]
use crate::lower::expr::*;

/// `operand as ty`: numeric / bool / char casts.
pub(crate) fn lower_cast(
    ctx: &mut LowerCtx<'_>,
    operand: &Expr,
    ty: &Ty,
    span: nabic_diagnostic::Span,
) -> HirExpr {
    let mark = ctx.probe_mark();
    let operand_h = lower_expected(ctx, operand, HirTy::Unknown);
    // In a monomorphized body `as T` names the concrete type.
    let to_ty = ctx.resolve_ast_ty(ty);
    // An open literal casts as the number it holds.
    let from_ty = crate::lower::infer::zonk(ctx, &operand_h.ty);
    // `f16` / `bf16`, `int` / `float`: a conversion call.
    if crate::lower::half::half_of(ctx, &from_ty).is_some()
        || crate::lower::half::half_of(ctx, &to_ty).is_some()
    {
        ctx.rollback(&mark);
        return crate::lower::half::cast(ctx, operand, &from_ty, &to_ty, ty, span);
    }
    if crate::lower::bignum::big_of(ctx, &from_ty).is_some()
        || crate::lower::bignum::big_of(ctx, &to_ty).is_some()
    {
        ctx.rollback(&mark);
        return crate::lower::bignum::cast(ctx, operand, &from_ty, &to_ty, ty, span);
    }
    // A field-less enum casts as its stored tag (a value variant's value).
    let from_ty = match &from_ty {
        HirTy::Enum(id) if matches!(&to_ty, HirTy::Prim(p) if p.is_int()) => {
            let def = ctx.enum_def(*id);
            if def.variants.iter().all(|v| v.payload_ty.is_none()) {
                HirTy::Prim(def.repr.int.unwrap_or(PrimTy::I32))
            } else {
                from_ty
            }
        }
        _ => from_ty,
    };
    let castable = |t: &HirTy| matches!(t, HirTy::Prim(p) if p.is_int() || p.is_float() || matches!(p, PrimTy::Bool | PrimTy::CChar | PrimTy::Char));
    if !(from_ty == to_ty
        || matches!(from_ty, HirTy::Unknown)
        || matches!(to_ty, HirTy::Unknown)
        || (castable(&from_ty) && castable(&to_ty)))
    {
        let to_name = ctx.ty_name(&to_ty);
        if let Some(h) = crate::lower::wide_int_help(&to_name) {
            return ctx.error_expr(
                Code::INVALID_CAST,
                span,
                format!("cannot cast to `{to_name}`: {h}"),
            );
        }
        if let (HirTy::Enum(_), HirTy::Prim(p)) = (&from_ty, &to_ty) {
            if p.is_int() {
                return ctx.error_expr(
                    Code::INVALID_CAST,
                    span,
                    format!(
                        "cannot cast `{}` as `{to_name}`: only an enum whose variants carry no \
                         payload casts to an integer",
                        ctx.ty_name(&from_ty)
                    ),
                );
            }
        }
        return ctx.error_expr(
            Code::INVALID_CAST,
            span,
            format!(
                "cannot cast `{}` as `{}` (only numeric / bool / char casts, and a \
                 payload-less enum to an integer){}",
                ctx.ty_name(&from_ty),
                ctx.ty_name(&to_ty),
                optional_cast_hint(ty)
            ),
        );
    }
    // Like Rust: only `u8` (always a scalar value) casts to `char`; a wider
    // value may be a surrogate or past U+10FFFF.
    if to_ty == HirTy::Prim(PrimTy::Char)
        && !matches!(
            from_ty,
            HirTy::Unknown | HirTy::Prim(PrimTy::U8 | PrimTy::Char)
        )
    {
        return ctx.error_expr(
            Code::INVALID_CAST,
            span,
            format!(
                "only `u8` can be cast as `char`, not `{}`; use `char::from_u32(n)` \
                 (`char | none`) for a wider value",
                ctx.ty_name(&from_ty)
            ),
        );
    }
    // Only `bool` casts as `bool`: a number compares with zero instead.
    if let (HirTy::Prim(PrimTy::Bool), HirTy::Prim(p)) = (&to_ty, &from_ty) {
        if *p != PrimTy::Bool {
            return bool_cast(ctx, operand, *p, span);
        }
    }
    HirExpr {
        kind: HirExprKind::Cast {
            from_ty: if crate::lower::infer::has_var(ctx, &operand_h.ty) {
                operand_h.ty.clone()
            } else {
                from_ty
            },
            operand: Box::new(operand_h),
            to_ty: to_ty.clone(),
        },
        ty: to_ty,
        span,
    }
}

/// `x as bool` from a number or `char`: an error with the comparison to
/// write in its place.
pub(crate) fn bool_cast(
    ctx: &LowerCtx<'_>,
    operand: &Expr,
    from: PrimTy,
    span: nabic_diagnostic::Span,
) -> HirExpr {
    let zero = if from.is_float() {
        "0.0"
    } else if from == PrimTy::Char {
        "'\\0'"
    } else {
        "0"
    };
    let text = operand_text(operand);
    let fix = match ctx.src(operand.span) {
        Some(src) if text == snippet(operand) || wrapped(src) => format!("{src} != {zero}"),
        Some(src) => format!("({src}) != {zero}"),
        None => format!("{text} != {zero}"),
    };
    ctx.emit_error_help(
        Code::INVALID_CAST,
        span,
        format!("cannot cast `{}` as `bool`", from.name()),
        format!("compare instead: `{text} != {zero}`"),
    );
    ctx.fix_last(span, fix.clone(), format!("write `{fix}`"));
    poison(span)
}

/// `src` is one parenthesised whole: `(a + b)`, not `(a) + (b)`.
pub(crate) fn wrapped(src: &str) -> bool {
    let Some(inner) = src.strip_prefix('(').and_then(|s| s.strip_suffix(')')) else {
        return false;
    };
    let mut depth = 0i32;
    for c in inner.chars() {
        match c {
            '(' => depth += 1,
            ')' if depth == 0 => return false,
            ')' => depth -= 1,
            _ => {}
        }
    }
    depth == 0
}

/// `x as T?` casts to the type `T?`; say so when the cast fails, since `?`
/// propagation was likely meant.
pub(crate) fn optional_cast_hint(ty: &Ty) -> &'static str {
    let optional = matches!(&ty.kind, AstTyKind::Sum(ps)
        if matches!(ps.as_slice(), [n, t] if n.nest_inner().is_some()
            && matches!(t.kind, AstTyKind::None)));
    if optional {
        "; the trailing `?` is part of the type `T?` - to propagate, write `(x as T)?`"
    } else {
        ""
    }
}

/// `inner?` (`inner_h` lowered). For Rust's `f()?` on a throwing call,
/// one error saying `?` isn't for errors replaces those about the call.
pub(crate) fn lower_try_q(
    ctx: &mut LowerCtx<'_>,
    inner: &Expr,
    inner_h: HirExpr,
    since: usize,
    span: nabic_diagnostic::Span,
) -> HirExpr {
    let call = matches!(
        inner.kind,
        ExprKind::Call { .. } | ExprKind::MethodCall { .. }
    );
    match ctx.src(inner.span).filter(|_| call) {
        Some(text) if try_q_errors(ctx, text, inner.span, since) => poison(span),
        _ => desugar_try_q(ctx, inner_h, span),
    }
}

pub(crate) fn try_q_errors(
    ctx: &LowerCtx<'_>,
    text: &str,
    at: nabic_diagnostic::Span,
    since: usize,
) -> bool {
    let mut diags = ctx.env.sink.diagnostics.borrow_mut();
    // The two errors a throwing call under `?` gets, by message.
    let caused = |d: &nabic_diagnostic::NabiDiagnostic| {
        d.is_error()
            && (d.message.starts_with("this call can throw")
                || d.message.starts_with("type annotations needed"))
            && d.primary.as_ref().is_some_and(|l| l.span.start == at.start)
    };
    let hit = diags[since..].iter().any(caused);
    if hit {
        let mut i = since;
        while i < diags.len() {
            if caused(&diags[i]) {
                diags.remove(i);
            } else {
                i += 1;
            }
        }
        let d = nabic_diagnostic::NabiDiagnostic::error(
            Code::OPTIONAL_OPERATOR,
            at,
            "`?` only works on `T | none`; it does not pass errors on",
        )
        .with_help(format!("in a `throws` function, write `try {text}`"));
        diags.insert(since, d);
    }
    hit
}

pub(crate) fn is_arith_op(op: &str) -> bool {
    matches!(
        op,
        "+" | "-" | "*" | "/" | "%" | "%%" | "&" | "|" | "^" | "<<" | ">>"
    )
}

/// `for` over literals whose type the loop body picks:
/// `for i in 0..10 { f_i32(i) }` is `{ let #fr = 0; for i in #fr..10 { .. } }`,
/// `for x in [1, 2] { .. }` is `{ let #fl = [1, 2]; for x in #fl { .. } }`.
pub(crate) fn literal_for(ctx: &mut LowerCtx<'_>, expr: &Expr) -> Option<HirExpr> {
    let span = expr.span;
    let ExprKind::For {
        label,
        pat,
        iter,
        body,
        is_await: false,
        by_ref,
    } = &expr.kind
    else {
        return None;
    };
    let lit = |e: &Expr| crate::lower::infer::literal_tree(e).is_some();
    let (t, init, iter) = match &iter.kind {
        ExprKind::Range {
            start: Some(start),
            end: Some(end),
            inclusive,
        } if lit(start) && lit(end) => {
            let t = tmp_path(ctx, "fr", start.span);
            let range = Expr {
                kind: ExprKind::Range {
                    start: Some(Box::new(t.clone())),
                    end: Some(end.clone()),
                    inclusive: *inclusive,
                },
                span: iter.span,
            };
            (t, (**start).clone(), range)
        }
        ExprKind::List(es) if !es.is_empty() && es.iter().all(lit) => {
            let t = tmp_path(ctx, "fl", iter.span);
            (t.clone(), (**iter).clone(), t)
        }
        _ => return None,
    };
    let for_expr = Expr {
        kind: ExprKind::For {
            label: label.clone(),
            pat: pat.clone(),
            iter: Box::new(iter),
            body: body.clone(),
            is_await: false,
            by_ref: *by_ref,
        },
        span,
    };
    let block = Expr {
        kind: ExprKind::Block(Block {
            stmts: vec![
                let_stmt(&t, init),
                Stmt {
                    kind: StmtKind::Expr(for_expr),
                    span,
                },
            ],
            trailing: None,
            span,
            label: None,
        }),
        span,
    };
    Some(lower_expr(ctx, &block))
}

pub(crate) fn lower_infix(
    ctx: &mut LowerCtx<'_>,
    op: &str,
    lhs: &Expr,
    rhs: &Expr,
    span: nabic_diagnostic::Span,
) -> HirExpr {
    // `&&` and `||` short-circuit: desugar before lowering `rhs` so its side
    // effects run only when needed.
    if op == "&&" || op == "||" {
        return lower_short_circuit(ctx, op, lhs, rhs, span);
    }
    // Arithmetic operands share the result's type (so `let x: i64 = 1 + 2`
    // types both literals as i64); comparison operands don't.
    let lhs_expected = if is_arith_op(op) || op == "**" {
        ctx.current_expected().cloned().unwrap_or(HirTy::Unknown)
    } else {
        HirTy::Unknown
    };
    if op == "**" {
        return lower_pow(ctx, lhs, rhs, lhs_expected, span);
    }
    let (mut lhs_h, mut rhs_h) = lower_sibling_pair(ctx, lhs, rhs, lhs_expected);
    if lhs_h.ty == HirTy::Unknown || rhs_h.ty == HirTy::Unknown {
        return poison(span);
    }
    // `x == none` tests for `none`: the other members need no `PartialEq`.
    if op == "==" || op == "!=" {
        let none = |e: &Expr| matches!(e.kind, ExprKind::Literal(AstLit::None));
        let optional = |t: &HirTy| matches!(t, HirTy::Sum(vs) if vs.contains(&HirTy::None));
        let side = if none(rhs) && optional(&lhs_h.ty) {
            Some((lhs_h.clone(), rhs.span))
        } else if none(lhs) && optional(&rhs_h.ty) {
            Some((rhs_h.clone(), lhs.span))
        } else {
            None
        };
        if let Some((scrut, at)) = side {
            let pat = nabic_ast::Pattern {
                kind: AstPatternKind::Literal(AstLit::None),
                span: at,
            };
            return lower_is_on(ctx, scrut, &pat, op == "==", span);
        }
    }
    let open_tys = (lhs_h.ty.clone(), rhs_h.ty.clone());
    let open = crate::lower::infer::meet(ctx, &mut lhs_h, &mut rhs_h);
    let since = ctx.env.sink.diagnostics.borrow().len();
    let tys = (lhs_h.ty.clone(), rhs_h.ty.clone());
    let mut e = lower_infix_typed(ctx, op, lhs_h, rhs_h, rhs, span);
    // The operands keep their open types: the body's end gives both the
    // solved one.
    if let (Some(_), HirExprKind::PrimBinOp { lhs, rhs, .. }) = (&open, &mut e.kind) {
        for (side, t) in [(lhs, open_tys.0), (rhs, open_tys.1)] {
            if crate::lower::infer::has_var(ctx, &t)
                && crate::lower::infer::zonk(ctx, &t) == side.ty
            {
                side.ty = t;
            }
        }
    }
    // An open operand's arithmetic stays open.
    if let Some(open) = open.filter(|_| is_arith_op(op) && e.ty == tys.0) {
        e.ty = open;
    }
    let tys = (
        crate::lower::infer::zonk(ctx, &tys.0),
        crate::lower::infer::zonk(ctx, &tys.1),
    );
    if ctx.env.sink.diagnostics.borrow().len() > since
        && !lit_operand_help(ctx, (lhs, &tys.0), (rhs, &tys.1))
    {
        if let Some(help) = operand_help(ctx, op, (lhs, &tys.0), (rhs, &tys.1)) {
            ctx.help_since(since, help);
        }
    }
    e
}

/// `lhs ** rhs`: the exponent is not the base's sibling. On an integer
/// base it is a `u32` (a literal is one; a negative or float literal is
/// an error), on a float base a literal takes the base's type.
pub(crate) fn lower_pow(
    ctx: &mut LowerCtx<'_>,
    lhs: &Expr,
    rhs: &Expr,
    lhs_expected: HirTy,
    span: nabic_diagnostic::Span,
) -> HirExpr {
    let lit_base = is_untyped_literal(lhs);
    let lhs_h = lower_expected(ctx, lhs, lhs_expected);
    let lit = is_untyped_literal(rhs);
    // An open literal base (`2 ** n`) is classed by its default.
    let base = crate::lower::infer::zonk(ctx, &lhs_h.ty);
    let base = crate::lower::infer::literal_default(ctx, &base).unwrap_or(base);
    let rhs_want = match &base {
        HirTy::Prim(p) if p.is_int() && !matches!(p, PrimTy::Char | PrimTy::CChar) => {
            // A float base, as written.
            let float = if matches!(lhs.kind, ExprKind::Literal(AstLit::Int(_))) && lit_base {
                format!("{}.0", snippet(lhs))
            } else {
                format!("({} as f64)", snippet(lhs))
            };
            let base_name = ctx.ty_name(&base);
            if lit && !is_untyped_int(rhs) {
                ctx.emit_error_help(
                    Code::INVALID_OPERATOR,
                    span,
                    format!("operator `**` cannot be applied to `{base_name}` and `f64`"),
                    format!(
                        "Nabi never converts numbers implicitly; raise a float: `{float} ** {}`",
                        snippet(rhs)
                    ),
                );
                return poison(span);
            }
            if lit && matches!(&rhs.kind, ExprKind::OpPrefix { op, .. } if op == "-") {
                ctx.emit_error_help(
                    Code::NEGATIVE_INT_POWER,
                    rhs.span,
                    format!("`{base_name}` to a negative power is a fraction, not an integer"),
                    format!("raise a float instead: `{float} ** {}.0`", snippet(rhs)),
                );
                return poison(span);
            }
            HirTy::Prim(PrimTy::U32)
        }
        HirTy::Prim(p) if p.is_float() && lit => lhs_h.ty.clone(),
        _ => HirTy::Unknown,
    };
    let rhs_h = lower_expected(ctx, rhs, rhs_want);
    if lhs_h.ty == HirTy::Unknown || rhs_h.ty == HirTy::Unknown {
        return poison(span);
    }
    lower_infix_typed(ctx, "**", lhs_h, rhs_h, rhs, span)
}

/// A fix for `lhs op rhs` whose operand types don't fit: convert, unwrap,
/// await or format the user's operands.
pub(crate) fn operand_help(
    ctx: &LowerCtx<'_>,
    op: &str,
    lhs: (&Expr, &HirTy),
    rhs: (&Expr, &HirTy),
) -> Option<String> {
    let src = |e: &Expr| ctx.src(e.span).map(str::to_owned);
    let (l, r) = (src(lhs.0)?, src(rhs.0)?);
    let numeric = |t: &HirTy| match t {
        HirTy::Prim(p) => p.is_int() || p.is_float(),
        _ => {
            crate::lower::bignum::big_of(ctx, t).is_some()
                || crate::lower::half::half_of(ctx, t).is_some()
        }
    };
    let is_str = |t: &HirTy| *t == HirTy::Prim(PrimTy::Str);
    let optional = |t: &HirTy| match t {
        HirTy::Sum(vs) if vs.contains(&HirTy::None) => {
            let rest: Vec<&HirTy> = vs.iter().filter(|v| **v != HirTy::None).collect();
            Some(match rest[..] {
                [one] => one.clone(),
                _ => HirTy::Sum(rest.into_iter().cloned().collect()),
            })
        }
        _ => None,
    };
    let future = |t: &HirTy| {
        matches!(t, HirTy::Struct(_)) && matches!(pick_method(ctx, t, "poll", None), Ok(Some(_)))
    };
    // `x == none` on a type without `none`.
    let none = |e: &Expr| matches!(e.kind, ExprKind::Literal(AstLit::None));
    if matches!(op, "==" | "!=") && none(lhs.0) != none(rhs.0) {
        let (side, t) = if none(rhs.0) {
            (&l, lhs.1)
        } else {
            (&r, rhs.1)
        };
        if *t == HirTy::None {
            return Some(format!(
                "`{side}` has type `none`, so it is always `none`; for a value that may be \
                 absent, declare it `T | none` with its type for `T`"
            ));
        }
        if optional(t).is_none() {
            let t = ctx.ty_name(t);
            return Some(format!(
                "`{side}` is `{t}`, which is never `none`; for a value that may be absent, \
                 declare it `{t} | none`"
            ));
        }
    }
    if op == "+" && (is_str(lhs.1) != is_str(rhs.1)) {
        // A plain literal's text goes in as is; anything else as `{..}`.
        let piece = |e: &Expr, s: &str| match &e.kind {
            ExprKind::Literal(AstLit::Str(_))
                if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 =>
            {
                s[1..s.len() - 1].replace('{', "{{").replace('}', "}}")
            }
            _ => format!("{{{s}}}"),
        };
        let (a, b) = (piece(lhs.0, &l), piece(rhs.0, &r));
        return Some(format!(
            "`+` joins a `str` to a `str` or a `char`; put the value in an f-string: `f\"{a}{b}\"`"
        ));
    }
    for (side, t) in [(&l, lhs.1), (&r, rhs.1)] {
        if let Some(inner) = optional(t) {
            let default = match inner {
                HirTy::Prim(p) if p.is_float() => "0.0",
                HirTy::Prim(p) if p.is_int() => "0",
                HirTy::Prim(PrimTy::Str) => "\"\"",
                _ => "..",
            };
            return Some(format!(
                "`{side}` may be `none`; unwrap it first: `{side} ?? {default}`, `if let n = \
                 {side} {{ .. }}`, or `{side}?`"
            ));
        }
        if future(t) {
            return Some(format!(
                "`{side}` is a future, not its value; did you forget `await`? `await {side}`"
            ));
        }
        // `v: i64 | str`: one member at a time.
        if let HirTy::Sum(parts) = t {
            let arms: Vec<String> = parts
                .iter()
                .map(|p| format!("x: {} => ..", ctx.ty_name(p)))
                .collect();
            return Some(format!(
                "`{side}` is one of several types; take one first: `match {side} {{ {} }}`, \
                 or `if {side} is {} {{ .. }}`",
                arms.join(", "),
                ctx.ty_name(&parts[0])
            ));
        }
    }
    if numeric(lhs.1) && numeric(rhs.1) && lhs.1 != rhs.1 {
        let name = |t: &HirTy| ctx.ty_name(t);
        let (to_l, to_r) = match widen(ctx, lhs.1, rhs.1) {
            Widen::Left => (Some(name(rhs.1)), None),
            Widen::Right => (None, Some(name(lhs.1))),
            Widen::Both(t) => (Some(t.name().to_owned()), Some(t.name().to_owned())),
        };
        let cast = |s: &str, to: Option<String>| match to {
            Some(t) => format!("({s} as {t})"),
            None => s.to_owned(),
        };
        let both = to_l.is_some() && to_r.is_some();
        return Some(format!(
            "Nabi never converts numbers implicitly; convert {}: `{} {op} {}`",
            if both { "both sides" } else { "one side" },
            cast(&l, to_l),
            cast(&r, to_r)
        ));
    }
    None
}

/// Which side of mixed numbers to convert so no value is lost.
pub(crate) enum Widen {
    /// Convert the left operand to the right one's type.
    Left,
    Right,
    Both(PrimTy),
}

/// An integer goes to a float, `int` / `float` take any primitive, and
/// the narrower of two of a kind goes to the wider. A signed type takes
/// an unsigned one; of one width below 64 bits both go one width up.
pub(crate) fn widen(ctx: &LowerCtx<'_>, a: &HirTy, b: &HirTy) -> Widen {
    // (class, bits, signed): a higher class holds every lower one.
    let rank = |t: &HirTy| match t {
        HirTy::Prim(p) if p.is_float() => (2, if *p == PrimTy::F32 { 32 } else { 64 }, true),
        HirTy::Prim(p) => {
            let bits = match p {
                PrimTy::I8 | PrimTy::U8 => 8,
                PrimTy::I16 | PrimTy::U16 => 16,
                PrimTy::I32 | PrimTy::U32 => 32,
                _ => 64,
            };
            (0, bits, p.is_signed_int())
        }
        _ if crate::lower::half::half_of(ctx, t).is_some() => (2, 16, true),
        _ => match crate::lower::bignum::big_of(ctx, t) {
            Some(Big::Float) => (3, 0, true),
            _ => (1, 0, true),
        },
    };
    let (ra, rb) = (rank(a), rank(b));
    // `f16` and `bf16`: neither holds the other.
    if ra == (2, 16, true) && rb == ra {
        return Widen::Both(PrimTy::F32);
    }
    if ra.0 != rb.0 {
        return if ra.0 < rb.0 {
            Widen::Left
        } else {
            Widen::Right
        };
    }
    if ra.0 != 0 || ra.2 == rb.2 {
        return if ra.1 < rb.1 {
            Widen::Left
        } else {
            Widen::Right
        };
    }
    if ra.1 == rb.1 && ra.1 < 64 {
        let up = match ra.1 {
            8 => PrimTy::I16,
            16 => PrimTy::I32,
            _ => PrimTy::I64,
        };
        return Widen::Both(up);
    }
    // Into the signed side: a length (`usize`) with an `i32` reads as
    // `(xs.len() as i32)`.
    if ra.2 { Widen::Right } else { Widen::Left }
}

pub(crate) fn lower_infix_typed(
    ctx: &mut LowerCtx<'_>,
    op: &str,
    lhs_h: HirExpr,
    rhs_h: HirExpr,
    rhs: &Expr,
    span: nabic_diagnostic::Span,
) -> HirExpr {
    if matches!(lhs_h.ty, HirTy::AnyTrait(_)) || matches!(rhs_h.ty, HirTy::AnyTrait(_)) {
        return dyn_op(ctx, op, lhs_h, Some(rhs_h), span);
    }
    // `swap`: `a ∈ s` is `s.contains(a)`. Operands still evaluate left to
    // right: `a` goes to a temp first.
    if ctx
        .env
        .prec
        .lookup(op, Fixity::Infix)
        .is_some_and(|e| e.swap)
    {
        let ty = lhs_h.ty.clone();
        let tmp = ctx.declare(None, ty.clone());
        let call = dispatch_user_op(
            ctx,
            op,
            Fixity::Infix,
            rhs_h,
            Some(hir_local(tmp, &ty, span)),
            span,
        );
        let call_ty = call.ty.clone();
        return HirExpr {
            kind: HirExprKind::Block(Box::new(HirBlock {
                stmts: vec![HirStmt::Let {
                    local: tmp,
                    init: lhs_h,
                }],
                trailing: Some(call),
                label: None,
            })),
            ty: call_ty,
            span,
        };
    }
    // On user types, `str`, tuples / arrays (core's component-wise impls)
    // and with any Unicode math operator (primitives too) the operator is its
    // trait's method.
    if matches!(
        lhs_h.ty,
        HirTy::Struct(_)
            | HirTy::Enum(_)
            | HirTy::Prim(PrimTy::Str)
            | HirTy::Tuple(_)
            | HirTy::Array(..)
            | HirTy::Param(_)
    ) || !op.is_ascii()
    {
        return dispatch_user_op_lit(ctx, op, Fixity::Infix, lhs_h, Some(rhs_h), Some(rhs), span);
    }
    // A primitive on the left of a user type (`impl Mul<Duration> for f64`),
    // or an operator with no primitive lowering.
    if let HirTy::Prim(lp) = crate::lower::infer::zonk(ctx, &lhs_h.ty) {
        if matches!(rhs_h.ty, HirTy::Struct(_) | HirTy::Enum(_)) || infix_for_prim(op, lp).is_none()
        {
            return dispatch_user_op_lit(
                ctx,
                op,
                Fixity::Infix,
                lhs_h,
                Some(rhs_h),
                Some(rhs),
                span,
            );
        }
    }
    // Comparisons on a sum dispatch on the variant (`sum_structural_call`).
    if let HirTy::Sum(_) = lhs_h.ty {
        let method = match op {
            "==" => Some("eq"),
            "!=" => Some("ne"),
            "<" => Some("lt"),
            ">" => Some("gt"),
            "<=" => Some("le"),
            ">=" => Some("ge"),
            _ => None,
        };
        let key = match method {
            Some("eq" | "ne") => ctx.lang("eq_trait"),
            _ => ctx.lang("cmp_trait"),
        };
        if matches!(method, Some("lt" | "gt" | "le" | "ge")) && sum_value_terms(&lhs_h.ty) > 1 {
            return sum_order_error(ctx, &lhs_h.ty, op, span);
        }
        if let Some(m) = method.filter(|_| ctx.has_trait(&lhs_h.ty, key)) {
            return sum_structural_call(ctx, lhs_h, m, &[], Some(rhs_h), span);
        }
    }
    if let (HirTy::Prim(lp), HirTy::Prim(rp)) = (&lhs_h.ty, &rhs_h.ty) {
        if lp == rp {
            if let Some((primop, result_ty)) = infix_for_prim(op, *lp) {
                let e = hir_binop(primop, lhs_h, rhs_h, result_ty, span);
                check_const_overflow(ctx, &e.kind, &e.ty, span);
                return e;
            }
        }
    }
    ctx.error_expr(
        Code::INVALID_OPERATOR,
        span,
        format!(
            "operator `{op}` cannot be applied to `{}` and `{}`",
            ctx.ty_name(&lhs_h.ty),
            ctx.ty_name(&rhs_h.ty)
        ),
    )
}

/// An operator on `any` switches on the tag over the primitives that have
/// it (same type both sides; a static operand fixes it). Arithmetic
/// results are boxed back into `any`, comparisons give `bool`; a type
/// without the operator panics.
pub(crate) fn dyn_op(
    ctx: &mut LowerCtx<'_>,
    op: &str,
    lhs: HirExpr,
    rhs: Option<HirExpr>,
    span: nabic_diagnostic::Span,
) -> HirExpr {
    const PRIMS: [PrimTy; 15] = [
        P::I8,
        P::I16,
        P::I32,
        P::I64,
        P::Isize,
        P::U8,
        P::U16,
        P::U32,
        P::U64,
        P::Usize,
        P::F32,
        P::F64,
        P::Bool,
        P::CChar,
        P::Char,
    ];
    // An unsuffixed literal beside an `any` takes the tag's type.
    let lit = |e: &HirExpr| {
        matches!(
            e.kind,
            HirExprKind::Const(ConstVal::Int { .. } | ConstVal::Float { .. })
        ) && (crate::lower::infer::open_literal(ctx, &e.ty).is_some()
            || ctx.env.sink.default_lits.borrow().contains(&e.span))
    };
    let is_any = |t: &HirTy| matches!(t, HirTy::AnyTrait(_));
    if let Some(r) = rhs.as_ref() {
        if is_any(&lhs.ty) && lit(r) {
            return dyn_lit_op(ctx, op, lhs, r.clone(), false, span);
        }
        if is_any(&r.ty) && lit(&lhs) {
            return dyn_lit_op(ctx, op, r.clone(), lhs, true, span);
        }
    }
    // The dispatch is over the other operand's type: an open literal
    // takes its default here.
    let (mut lhs, mut rhs) = (lhs, rhs);
    for e in [Some(&mut lhs), rhs.as_mut()].into_iter().flatten() {
        e.ty = crate::lower::infer::fix_literals(ctx, &e.ty);
    }
    let is_dyn = |t: &HirTy| matches!(t, HirTy::AnyTrait(_));
    let fixed = [Some(&lhs.ty), rhs.as_ref().map(|r| &r.ty)]
        .into_iter()
        .flatten()
        .find(|t| !is_dyn(t))
        .cloned();
    let lookup = |p: PrimTy| {
        if rhs.is_some() {
            infix_for_prim(op, p)
        } else {
            prefix_for_prim(op, p)
        }
    };
    let cands: Vec<(PrimTy, PrimOp, HirTy)> = PRIMS
        .into_iter()
        .filter(|p| fixed.as_ref().is_none_or(|f| *f == HirTy::Prim(*p)))
        .filter_map(|p| lookup(p).map(|(o, t)| (p, o, t)))
        .collect();
    if cands.is_empty() {
        let tys: Vec<String> = [Some(&lhs.ty), rhs.as_ref().map(|r| &r.ty)]
            .into_iter()
            .flatten()
            .map(|t| format!("`{}`", ctx.ty_name(t)))
            .collect();
        return ctx.error_expr(
            Code::INVALID_OPERATOR,
            span,
            format!(
                "operator `{op}` cannot be applied to {} - on an `any` value it dispatches \
                 only over primitive types (narrow it with `is` first)",
                tys.join(" and ")
            ),
        );
    }
    let bool_ty = HirTy::Prim(PrimTy::Bool);
    let result_ty = if cands.iter().all(|c| c.2 == bool_ty) {
        bool_ty
    } else {
        HirTy::AnyTrait(Vec::new())
    };
    // Operands are evaluated once, left to right.
    let mut lets = Vec::new();
    let mut temp = |ctx: &mut LowerCtx<'_>, e: HirExpr| {
        let ty = e.ty.clone();
        let l = ctx.declare(None, ty.clone());
        lets.push(HirStmt::Let { local: l, init: e });
        hir_local(l, &ty, span)
    };
    let l = temp(ctx, lhs);
    let r = rhs.map(|r| temp(ctx, r));
    // With one static operand the tag must be its type.
    let why = match &fixed {
        Some(t) => format!(
            "operator `{op}` on an `any` that does not hold a `{}`, the other operand's type",
            ctx.ty_name(t)
        ),
        None => format!("operator `{op}` is not defined for the runtime type of an `any` value"),
    };
    let panic_arm = |ctx: &LowerCtx<'_>| HirMatchArm {
        pat: HirPattern::Wildcard,
        guard: None,
        body: HirExpr {
            kind: HirExprKind::Block(Box::new(HirBlock {
                stmts: vec![HirStmt::Panic(crate::lower::throws::hook_call(
                    ctx,
                    "nabi_panic_at",
                    &why,
                    span,
                ))],
                trailing: None,
                label: None,
            })),
            ty: result_ty.clone(),
            span,
        },
    };
    // `match v { x: P => body(x), _ => panic }`, or `v` itself if static.
    let narrow = |ctx: &mut LowerCtx<'_>,
                  v: &HirExpr,
                  p: PrimTy,
                  body: &dyn Fn(&mut LowerCtx<'_>, HirExpr) -> HirExpr| {
        let pty = HirTy::Prim(p);
        if !is_dyn(&v.ty) {
            return body(ctx, v.clone());
        }
        let x = ctx.declare(None, pty.clone());
        let arm = HirMatchArm {
            pat: HirPattern::SumVariant {
                sum_ty: v.ty.clone(),
                variant_index: ctx.any_tag(&pty),
                binder: Some(x),
                payload_ty: Some(pty.clone()),
            },
            guard: None,
            body: body(ctx, hir_local(x, &pty, span)),
        };
        HirExpr {
            kind: HirExprKind::Match {
                scrutinee: Box::new(v.clone()),
                arms: vec![arm, panic_arm(ctx)],
            },
            ty: result_ty.clone(),
            span,
        }
    };
    let mut arms = Vec::new();
    for (p, primop, ty) in cands {
        let finish = |ctx: &mut LowerCtx<'_>, e: HirExpr| {
            coerce(ctx, &result_ty, e, &|| format!("the result of `{op}`"))
        };
        let pty = HirTy::Prim(p);
        let x = ctx.declare(None, pty.clone());
        let xv = hir_local(x, &pty, span);
        let body = if let Some(r) = &r {
            narrow(ctx, r, p, &|ctx, y| {
                finish(ctx, hir_binop(primop, xv.clone(), y, ty.clone(), span))
            })
        } else {
            let e = HirExpr {
                kind: HirExprKind::PrimUnaryOp {
                    op: primop,
                    operand: Box::new(xv.clone()),
                },
                ty: ty.clone(),
                span,
            };
            finish(ctx, e)
        };
        if !is_dyn(&l.ty) {
            // Static lhs: exactly one candidate; bind it plainly.
            lets.push(HirStmt::Let { local: x, init: l });
            return with_lets(lets, body);
        }
        arms.push(HirMatchArm {
            pat: HirPattern::SumVariant {
                sum_ty: l.ty.clone(),
                variant_index: ctx.any_tag(&pty),
                binder: Some(x),
                payload_ty: Some(pty),
            },
            guard: None,
            body,
        });
    }
    arms.push(panic_arm(ctx));
    with_lets(
        lets,
        HirExpr {
            kind: HirExprKind::Match {
                scrutinee: Box::new(l),
                arms,
            },
            ty: result_ty,
            span,
        },
    )
}

/// `v op 1` with `v: any`: the literal takes the type of `v`'s tag, an
/// arm per primitive with the operator; a tag the literal does not fit
/// (`300` against a `u8`) panics naming both. `flip`: the literal is the
/// left operand.
fn dyn_lit_op(
    ctx: &mut LowerCtx<'_>,
    op: &str,
    dynv: HirExpr,
    lit: HirExpr,
    flip: bool,
    span: nabic_diagnostic::Span,
) -> HirExpr {
    const NUMS: [PrimTy; 12] = [
        P::I8,
        P::I16,
        P::I32,
        P::I64,
        P::Isize,
        P::U8,
        P::U16,
        P::U32,
        P::U64,
        P::Usize,
        P::F32,
        P::F64,
    ];
    let HirExprKind::Const(value) = lit.kind.clone() else {
        return poison(span);
    };
    let src = ctx.src(lit.span).unwrap_or("..").to_owned();
    // The literal as a `p`, if it can be one.
    let as_prim = |p: PrimTy| -> Option<ConstVal> {
        match value {
            ConstVal::Int { value: v, .. } if p.is_int() => {
                crate::lower::pattern::int_fits(v, p).then_some(ConstVal::Int { value: v, ty: p })
            }
            ConstVal::Int { value: v, .. } if p.is_float() => {
                #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
                let (f, exact) = if p == P::F32 {
                    let f = v as f32;
                    (u64::from(f.to_bits()), f as i128 == v)
                } else {
                    let f = v as f64;
                    (f.to_bits(), f as i128 == v)
                };
                exact.then_some(ConstVal::Float { bits: f, ty: p })
            }
            ConstVal::Float { bits, .. } if p.is_float() => {
                #[allow(clippy::cast_possible_truncation)]
                let b = if p == P::F32 {
                    u64::from((f64::from_bits(bits) as f32).to_bits())
                } else {
                    bits
                };
                Some(ConstVal::Float { bits: b, ty: p })
            }
            _ => None,
        }
    };
    let cands: Vec<(PrimTy, PrimOp, HirTy)> = NUMS
        .into_iter()
        .filter_map(|p| infix_for_prim(op, p).map(|(o, t)| (p, o, t)))
        .collect();
    let bool_ty = HirTy::Prim(PrimTy::Bool);
    let result_ty = if cands.iter().all(|c| c.2 == bool_ty) {
        bool_ty
    } else {
        HirTy::AnyTrait(Vec::new())
    };
    let panic = |ctx: &LowerCtx<'_>, msg: &str| HirExpr {
        kind: HirExprKind::Block(Box::new(HirBlock {
            stmts: vec![HirStmt::Panic(crate::lower::throws::hook_call(
                ctx,
                "nabi_panic_at",
                msg,
                span,
            ))],
            trailing: None,
            label: None,
        })),
        ty: result_ty.clone(),
        span,
    };
    let dty = dynv.ty.clone();
    let held = ctx.declare(None, dty.clone());
    let lets = vec![HirStmt::Let {
        local: held,
        init: dynv,
    }];
    let mut arms = Vec::new();
    for (p, primop, ty) in cands {
        let pty = HirTy::Prim(p);
        let x = ctx.declare(None, pty.clone());
        let body = match as_prim(p) {
            Some(value) => {
                let k = HirExpr {
                    kind: HirExprKind::Const(value),
                    ty: pty.clone(),
                    span: lit.span,
                };
                let xv = hir_local(x, &pty, span);
                let (lhs, rhs) = if flip { (k, xv) } else { (xv, k) };
                let op_e = hir_binop(primop, lhs, rhs, ty, span);
                coerce(ctx, &result_ty, op_e, &|| format!("the result of `{op}`"))
            }
            None => panic(
                ctx,
                &format!(
                    "`{op}` on an `any` holding `{}`: `{src}` does not fit `{}`",
                    p.name(),
                    p.name()
                ),
            ),
        };
        arms.push(HirMatchArm {
            pat: HirPattern::SumVariant {
                sum_ty: dty.clone(),
                variant_index: ctx.any_tag(&pty),
                binder: Some(x),
                payload_ty: Some(pty),
            },
            guard: None,
            body,
        });
    }
    arms.push(HirMatchArm {
        pat: HirPattern::Wildcard,
        guard: None,
        body: panic(
            ctx,
            &format!("operator `{op}` is not defined for the runtime type of an `any` value"),
        ),
    });
    with_lets(
        lets,
        HirExpr {
            kind: HirExprKind::Match {
                scrutinee: Box::new(hir_local(held, &dty, span)),
                arms,
            },
            ty: result_ty,
            span,
        },
    )
}

pub(crate) fn lower_short_circuit(
    ctx: &mut LowerCtx<'_>,
    op: &str,
    lhs: &Expr,
    rhs: &Expr,
    span: nabic_diagnostic::Span,
) -> HirExpr {
    let bool_ty = HirTy::Prim(PrimTy::Bool);
    let lhs_h = lower_expected(ctx, lhs, bool_ty.clone());
    // A user `LogicalAnd` / `LogicalOr` impl: a plain call, both operands
    // evaluated.
    if matches!(lhs_h.ty, HirTy::Struct(_) | HirTy::Enum(_)) {
        let rhs_h = lower_expr(ctx, rhs);
        return dispatch_user_op(ctx, op, Fixity::Infix, lhs_h, Some(rhs_h), span);
    }
    let lhs_h = coerce(ctx, &bool_ty, lhs_h, &|| "the condition".to_owned());
    // `N > 0 && a[N - 1] == x` at `N = 0`: the right side never runs.
    let dead = u32::from(const_cond(ctx, &lhs_h) == Some(op != "&&"));
    ctx.dead += dead;
    // `x is T && rhs`: `x` is `T` in `rhs`.
    let rhs_h = if op == "&&" {
        ctx.push_scope();
        let lets = bind_narrowed(ctx, lhs);
        let r = lower_cond(ctx, rhs);
        ctx.pop_scope();
        with_lets(lets, r)
    } else {
        lower_cond(ctx, rhs)
    };
    ctx.dead -= dead;
    let mk_const_bool = |b: bool| HirExpr {
        kind: HirExprKind::Const(ConstVal::Bool(b)),
        ty: bool_ty.clone(),
        span,
    };
    let mk_block = |trailing: HirExpr| HirBlock {
        stmts: Vec::new(),
        trailing: Some(trailing),
        label: None,
    };
    //   a && b  ->  if a { b } else { false }
    //   a || b  ->  if a { true } else { b }
    let (then_expr, else_expr) = if op == "&&" {
        (rhs_h, mk_const_bool(false))
    } else {
        (mk_const_bool(true), rhs_h)
    };
    HirExpr {
        kind: HirExprKind::If {
            cond: Box::new(lhs_h),
            then_block: Box::new(mk_block(then_expr)),
            else_branch: Some(Box::new(else_expr)),
        },
        ty: bool_ty,
        span,
    }
}

pub(crate) fn lower_prefix(
    ctx: &mut LowerCtx<'_>,
    op: &str,
    operand: &Expr,
    span: nabic_diagnostic::Span,
) -> HirExpr {
    // `-5` / `-1.5`: a negative literal (range-checked as negative).
    if op == "-" {
        if let ExprKind::Literal(lit @ (AstLit::Int(_) | AstLit::Float(_))) = &operand.kind {
            return crate::lower::half::literal(ctx, lit, true, span)
                .or_else(|| crate::lower::bignum::literal(ctx, lit, true, span))
                .unwrap_or_else(|| lower_literal(ctx, lit, true, span));
        }
    }
    let mut operand_h = lower_expr(ctx, operand);
    if operand_h.ty == HirTy::Unknown {
        return operand_h;
    }
    // `-total` of an open literal stays open (`abs(-total)` fixes it).
    if let (true, Some(d)) = (
        matches!(op, "-" | "~"),
        crate::lower::infer::literal_default(ctx, &operand_h.ty),
    ) {
        let open = std::mem::replace(&mut operand_h.ty, d);
        let mut e = lower_prefix_typed(ctx, op, operand_h, span);
        if matches!(e.ty, HirTy::Prim(_)) {
            e.ty = open.clone();
        }
        if let HirExprKind::PrimUnaryOp { operand, .. } = &mut e.kind {
            operand.ty = open;
        }
        return e;
    }
    lower_prefix_typed(ctx, op, operand_h, span)
}

pub(crate) fn lower_prefix_typed(
    ctx: &mut LowerCtx<'_>,
    op: &str,
    operand_h: HirExpr,
    span: nabic_diagnostic::Span,
) -> HirExpr {
    if matches!(
        operand_h.ty,
        HirTy::Struct(_) | HirTy::Enum(_) | HirTy::Param(_)
    ) || !op.is_ascii()
    {
        return dispatch_user_op(ctx, op, Fixity::Prefix, operand_h, None, span);
    }
    if matches!(operand_h.ty, HirTy::AnyTrait(_)) {
        return dyn_op(ctx, op, operand_h, None, span);
    }
    if let HirTy::Prim(p) = &operand_h.ty {
        if let Some((primop, result_ty)) = prefix_for_prim(op, *p) {
            return HirExpr {
                kind: HirExprKind::PrimUnaryOp {
                    op: primop,
                    operand: Box::new(operand_h),
                },
                ty: result_ty,
                span,
            };
        }
    }
    let e = ctx.error_expr(
        Code::INVALID_OPERATOR,
        span,
        format!(
            "operator `{op}` cannot be applied to `{}`",
            ctx.ty_name(&operand_h.ty)
        ),
    );
    ctx.help_last(crate::lower::suggest::op_habit_help(
        ctx, op, &operand_h, None,
    ));
    e
}

/// An operator on a user type calls its trait method (`a + b` is
/// `Add::add(a, b)`); only from a trait impl, not a same-named inherent
/// method.
pub(crate) fn dispatch_user_op(
    ctx: &mut LowerCtx<'_>,
    sym: &str,
    fixity: Fixity,
    lhs: HirExpr,
    rhs: Option<HirExpr>,
    span: nabic_diagnostic::Span,
) -> HirExpr {
    dispatch_user_op_lit(ctx, sym, fixity, lhs, rhs, None, span)
}

/// `dispatch_user_op`; `lit_rhs`: the right operand's source when it may
/// be an unsuffixed literal, free to take the impl's `Rhs` type.
pub(crate) fn dispatch_user_op_lit(
    ctx: &mut LowerCtx<'_>,
    sym: &str,
    fixity: Fixity,
    lhs: HirExpr,
    rhs: Option<HirExpr>,
    lit_rhs: Option<&Expr>,
    span: nabic_diagnostic::Span,
) -> HirExpr {
    let ty_name = ctx.ty_name(&lhs.ty);
    let Some(entry) = ctx.env.prec.lookup(sym, fixity) else {
        return ctx.error_expr(
            Code::INVALID_OPERATOR,
            span,
            format!("operator `{sym}` cannot be applied to `{ty_name}`"),
        );
    };
    let method = entry.method_name.clone();
    let op_trait = &entry.trait_name;
    // The binding works only where its trait is visible.
    let names = &ctx.env.names;
    let cur = &ctx.current_module;
    let visible = names
        .type_vis(op_trait)
        .is_none_or(|v| crate::lower::resolve::visible_from(cur, &module_of(op_trait), v))
        && names.hidden(cur, op_trait).is_none();
    if !visible {
        return ctx.error_expr(
            Code::INVALID_OPERATOR,
            span,
            format!("operator `{sym}` needs trait `{op_trait}`, which is not visible here"),
        );
    }
    // The impl must be of the trait that registered the operator (by its
    // full path), not another trait that happens to share its name.
    let is_op_trait = |t: &str| t == op_trait;
    // A core operator trait is not in the prelude: `impl ops::Add for V`.
    let shown = op_trait
        .strip_prefix("core::")
        .filter(|t| t.starts_with("ops::"))
        .unwrap_or_else(|| leaf_name(op_trait));
    // `180°`, `2 ∈ [1, 2]`: open literals take the number types with the
    // operator, else their defaults, here.
    let mut lhs = lhs;
    if crate::lower::infer::active(ctx) && crate::lower::infer::has_var(ctx, &lhs.ty) {
        // With a typed right operand, the impl for it (`84 / nz`).
        let want = match rhs.as_ref().map(|r| &r.ty) {
            Some(r) if !crate::lower::infer::has_var(ctx, r) && *r != HirTy::Unknown => {
                TraitRef::new(op_trait.to_owned(), vec![r.clone()], Vec::new())
            }
            _ => TraitRef::plain(op_trait),
        };
        crate::lower::infer::pick_literals(ctx, &lhs.ty, &|t| ctx.has_impl(t, &want, 0));
        lhs.ty = crate::lower::infer::resolve(ctx, &lhs.ty);
    }
    if let HirTy::Param(p) = lhs.ty {
        return param_op(ctx, p, sym, &method, op_trait, lhs, rhs, lit_rhs, span);
    }
    let want = TraitRef::plain(op_trait);
    let lhs = closure_as_fn(ctx, lhs, &|ctx, t| ctx.implements_trait(t, &want, 0));
    let impls: Vec<FunctionId> = ctx
        .env
        .methods_on(&lhs.ty, &method)
        .map(|ids| {
            ids.iter()
                .copied()
                .filter(|id| {
                    ctx.env
                        .method_info(*id)
                        .and_then(|i| i.trait_name.as_deref())
                        .is_some_and(is_op_trait)
                })
                .collect()
        })
        .unwrap_or_default();
    // Several impls (`Sub<Duration>` and `Sub<Instant>` for `Instant`): the
    // one whose `Rhs` is the right operand's type, else the one a literal
    // fits (`pick_literal_impl`), else the first.
    let rhs_param = |ctx: &LowerCtx<'_>, id: FunctionId| {
        ctx.sig_by_id(id)
            .param_tys
            .get(1)
            .cloned()
            .unwrap_or(HirTy::Unknown)
    };
    let exact = rhs.as_ref().and_then(|r| {
        let t = crate::lower::infer::zonk(ctx, &r.ty);
        impls.iter().copied().find(|id| rhs_param(ctx, *id) == t)
    });
    // An open right operand that is not a literal itself (`let v =
    // W::new(1.0); m * v`): the one choice of its literals with an impl.
    let open_rhs = rhs.as_ref().filter(|r| {
        crate::lower::infer::active(ctx)
            && crate::lower::infer::has_var(ctx, &r.ty)
            && lit_rhs.is_none_or(|e| crate::lower::infer::literal_tree(e).is_none())
    });
    let exact = match (exact, open_rhs) {
        (None, Some(r)) if !impls.is_empty() => {
            let params: Vec<HirTy> = impls.iter().map(|id| rhs_param(ctx, *id)).collect();
            crate::lower::infer::pick_literals_or(
                ctx,
                &r.ty,
                &|t| params.contains(t),
                &|_| false,
                false,
            );
            let t = crate::lower::infer::zonk(ctx, &r.ty);
            impls.iter().copied().find(|id| rhs_param(ctx, *id) == t)
        }
        (e, _) => e,
    };
    let mut rhs = rhs;
    let mut lit_pick = None;
    if let (None, Some(e)) = (exact, lit_rhs) {
        let params: Vec<HirTy> = impls.iter().map(|id| rhs_param(ctx, *id)).collect();
        match pick_literal_impl(ctx, e, &params) {
            Ok(Some(i)) => {
                rhs = Some(lower_expected(ctx, e, params[i].clone()));
                lit_pick = Some(impls[i]);
            }
            Ok(None) => {}
            Err(fits) => {
                return ctx.error_expr(
                    Code::AMBIGUOUS_LITERAL,
                    span,
                    format!(
                        "operator `{sym}` on `{ty_name}`: the literal fits several \
                         `{shown}` impls ({fits}); add a suffix or `as`"
                    ),
                );
            }
        }
    }
    // No concrete impl takes the typed right operand: a generic one may
    // (`impl<const L: i32> Mul<G<L>> for f64` beside core's concrete ones).
    let typed_rhs = rhs
        .as_ref()
        .is_some_and(|r| r.ty != HirTy::Unknown && !crate::lower::infer::has_var(ctx, &r.ty));
    if exact.is_none() && lit_pick.is_none() && typed_rhs && !impls.is_empty() {
        let gm = generic_op_by_rhs(ctx, &lhs.ty, rhs.as_ref(), None, &method)
            .filter(|gm| gm.info.trait_name.as_deref().is_some_and(is_op_trait));
        if let Some(gm) = gm {
            let mut args = vec![lhs];
            args.extend(rhs);
            let Some((callee, ty)) = monomorphize(ctx, &gm.def, Some(&gm.self_ty), &args, span)
            else {
                return poison(span);
            };
            return op_call(ctx, callee, args, ty, &method, span);
        }
    }
    let fn_id = exact.or(lit_pick).or_else(|| impls.first().copied());
    let Some(fn_id) = fn_id else {
        // `impl<T> Add for Pair<T>`: monomorphize on the operand types.
        let gm = if rhs.is_some() {
            generic_op_by_rhs(ctx, &lhs.ty, rhs.as_ref(), lit_rhs, &method)
        } else {
            // The operator's trait only: an inherent `fn sqrt` beside
            // `impl Sqrt` does not hide `√q`.
            let mut hits = ctx
                .generic_impls(&lhs.ty, crate::lower::Pick::Method(&method), 0)
                .filter(|(gi, _)| is_op_trait(&gi.trait_key))
                .filter_map(|(gi, _)| gi.methods.get(&method));
            match (hits.next(), hits.next()) {
                (Some(gm), None) => Some(gm),
                _ => None,
            }
        };
        if let Some(gm) = gm {
            if gm.info.trait_name.as_deref().is_some_and(is_op_trait) {
                let mut args = vec![lhs];
                args.extend(rhs);
                let Some((callee, ty)) = monomorphize(ctx, &gm.def, Some(&gm.self_ty), &args, span)
                else {
                    return poison(span);
                };
                return op_call(ctx, callee, args, ty, &method, span);
            }
        }
        // The trait is there for another right operand (`Mul<f32>`, not
        // `Mul<f64>`): name both sides and the impl that is missing.
        if let Some(r) = rhs.as_ref().filter(|r| {
            r.ty != HirTy::Unknown && ctx.has_impl(&lhs.ty, &TraitRef::plain(op_trait), 0)
        }) {
            let rt = crate::lower::infer::zonk(ctx, &r.ty);
            let rn = ctx.ty_name(&rt);
            let e = ctx.error_expr(
                Code::INVALID_OPERATOR,
                span,
                format!(
                    "operator `{sym}` cannot be applied to `{ty_name}` and `{rn}`: there is \
                     no `impl {shown}<{rn}> for {ty_name}`"
                ),
            );
            ctx.help_last(rhs_mismatch_help(ctx, &lhs, r, &rt));
            return e;
        }
        let e = ctx.error_expr(
            Code::INVALID_OPERATOR,
            span,
            format!(
                "operator `{sym}` cannot be applied to `{ty_name}`: it needs \
                 `impl {shown} for {ty_name}` (`{method}` from any other trait or an \
                 inherent `fn {method}` is not enough)"
            ),
        );
        ctx.help_last(crate::lower::suggest::op_habit_help(
            ctx,
            sym,
            &lhs,
            rhs.as_ref(),
        ));
        return e;
    };
    let sig = ctx.sig_by_id(fn_id);
    let expected_arity = if rhs.is_some() { 2 } else { 1 };
    if sig.param_tys.len() != expected_arity {
        return ctx.error_expr(
            Code::INVALID_OPERATOR,
            span,
            format!(
                "`{method}` for `{ty_name}` must take {expected_arity} parameter(s) to implement \
                 operator `{sym}`"
            ),
        );
    }
    let mut args = vec![lhs];
    if let Some(mut r) = rhs {
        let pty = sig.param_tys[1].clone();
        // A literal takes its slot's type, the impl's `Rhs` (`t ⊲ [1.0, 2.0]`
        // into a `Vector<f64>`), as a call argument does.
        if let Some(e) = lit_rhs.filter(|e| r.ty != pty && slot_typed(e)) {
            r = lower_expected(ctx, e, pty.clone());
        }
        args.push(coerce(ctx, &pty, r, &|| {
            format!("the right operand of `{sym}`")
        }));
    }
    op_call(ctx, fn_id, args, sig.ret_ty, &method, span)
}

/// What to write when an operator's trait exists for the left operand
/// but not with this right operand: one element type on both sides
/// (`Vec3::<f64>::X`), or a number converted to the element type.
fn rhs_mismatch_help(
    ctx: &LowerCtx<'_>,
    lhs: &HirExpr,
    rhs: &HirExpr,
    rt: &HirTy,
) -> Option<String> {
    let HirTy::Struct(id) = crate::lower::infer::zonk(ctx, &lhs.ty) else {
        return None;
    };
    let d = ctx.struct_def(id);
    let leaf = leaf_name(enum_base_name(&d.name)).to_owned();
    let lsrc = crate::lower::suggest::short_src(ctx, lhs.span);
    let rsrc = crate::lower::suggest::short_src(ctx, rhs.span);
    if let HirTy::Struct(r) = rt {
        if enum_base_name(&ctx.struct_def(*r).name) == enum_base_name(&d.name) {
            return crate::lower::compat::same_elem_help(ctx, &lhs.ty, rsrc);
        }
    }
    let HirTy::Prim(p) = *rt else {
        return None;
    };
    let elem = d.generic_args.iter().find_map(|a| match a {
        HirTy::Prim(q) if *q != p && (q.is_int() || q.is_float()) => Some(*q),
        _ => None,
    })?;
    let conv = rsrc.map_or_else(
        || format!("convert the right operand with `as {}`", elem.name()),
        |s| format!("convert `{s} as {}`", elem.name()),
    );
    let shown = format!("{leaf}<{}>", elem.name());
    let item = lsrc
        .and_then(|s| s.strip_prefix(&format!("{leaf}::")))
        .filter(|r| !r.starts_with('<') && r.chars().all(|c| c.is_alphanumeric() || c == '_'));
    Some(match (item, lsrc) {
        (Some(item), Some(s)) => {
            format!(
                "`{s}` is `{shown}` here; write `{leaf}::<{}>::{item}`, or {conv}",
                p.name()
            )
        }
        _ => format!("Nabi never converts numbers implicitly; {conv}"),
    })
}

/// A literal whose type comes from its slot: `[..]`, `{..}`, a number.
fn slot_typed(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::List(_)
        | ExprKind::Dict(_)
        | ExprKind::Set(_)
        | ExprKind::MagicLiteral(_)
        | ExprKind::Literal(AstLit::Int(_) | AstLit::Float(_)) => true,
        ExprKind::Tuple(es) => es.iter().all(slot_typed),
        ExprKind::OpPrefix { op, operand } if op == "-" => slot_typed(operand),
        _ => false,
    }
}

/// The call of an operator's method `callee`. A `ref self` method takes
/// the left operand as a method call takes its receiver: a variable (or a
/// field of one) is written back, so `g ∼ d` advances `g`.
fn op_call(
    ctx: &mut LowerCtx<'_>,
    callee: FunctionId,
    args: Vec<HirExpr>,
    ty: HirTy,
    method: &str,
    span: nabic_diagnostic::Span,
) -> HirExpr {
    let out = ctx.env.sink.ref_out.borrow().get(&callee).cloned();
    let Some(out) = out else {
        return HirExpr {
            kind: HirExprKind::Call { callee, args },
            ty,
            span,
        };
    };
    let mut places = vec![None; args.len()];
    if out.refs.iter().any(|(i, _)| *i == 0) {
        captured_copy_write(ctx, &args[0], method);
        if crate::lower::mono::writable_place(ctx, &args[0]) {
            places[0] = Some(args[0].clone());
        }
    }
    let tys: Vec<HirTy> = out.refs.iter().map(|(_, t)| t.clone()).collect();
    let call = HirExpr {
        kind: HirExprKind::Call { callee, args },
        ty: crate::lower::mono::ref_packed(&out.ret, &tys),
        span,
    };
    crate::lower::mono::ref_unpack(ctx, call, &out, &places)
}

/// Operator `sym` on a type parameter: the bound on the operator's trait
/// gives the method, and its `Rhs` types the right side.
#[allow(clippy::too_many_arguments)]
pub(crate) fn param_op(
    ctx: &mut LowerCtx<'_>,
    p: u32,
    sym: &str,
    method: &str,
    op_trait: &str,
    lhs: HirExpr,
    rhs: Option<HirExpr>,
    lit_rhs: Option<&Expr>,
    span: nabic_diagnostic::Span,
) -> HirExpr {
    let t = ctx.ty_name(&lhs.ty);
    let bounds = crate::lower::bounds::of_trait(ctx, p, op_trait);
    let gm = crate::lower::bounds::bound_methods(ctx, p, method, Some(op_trait));
    let (false, [gm]) = (bounds.is_empty(), gm.as_slice()) else {
        return ctx.error_expr(
            Code::INVALID_OPERATOR,
            span,
            format!(
                "operator `{sym}` on `{t}` needs the bound `{t}: {}`",
                leaf_name(op_trait)
            ),
        );
    };
    let mut args = vec![lhs];
    if let Some(r) = rhs {
        // The bound whose `Rhs` is the right operand's type, else the one an
        // unsuffixed literal takes.
        let rhs_of = |b: &TraitRef| b.args.first().cloned().unwrap_or(HirTy::Unknown);
        let params: Vec<HirTy> = bounds.iter().map(rhs_of).collect();
        let lit = lit_rhs.filter(|e| is_untyped_literal(e));
        let want = if params.contains(&r.ty) || bounds.len() == 1 {
            params
                .iter()
                .find(|w| **w == r.ty)
                .unwrap_or(&params[0])
                .clone()
        } else {
            match lit.map(|e| pick_literal_impl(ctx, e, &params)) {
                Some(Ok(Some(i))) => params[i].clone(),
                Some(Err(fits)) => {
                    return ctx.error_expr(
                        Code::AMBIGUOUS_LITERAL,
                        span,
                        format!(
                            "operator `{sym}` on `{t}`: the literal fits several `{}` bounds \
                             ({fits}); add a suffix or `as`",
                            leaf_name(op_trait)
                        ),
                    );
                }
                _ => {
                    let all: Vec<String> = bounds
                        .iter()
                        .map(|b| format!("`{}`", b.render(leaf_name(op_trait), |t| ctx.ty_name(t))))
                        .collect();
                    return ctx.error_expr(
                        Code::INVALID_OPERATOR,
                        span,
                        format!(
                            "operator `{sym}` on `{t}` needs a bound `{}<{}>`, but `{t}` has {}",
                            leaf_name(op_trait),
                            ctx.ty_name(&r.ty),
                            all.join(" and ")
                        ),
                    );
                }
            }
        };
        let r = match lit {
            Some(e) => lower_expected(ctx, e, want.clone()),
            None => r,
        };
        args.push(coerce(ctx, &want, r, &|| {
            format!("the right operand of `{sym}`")
        }));
    }
    let self_ty = args[0].ty.clone();
    let Some((callee, ty)) =
        monomorphize_on(ctx, &gm.def, Some(&gm.self_ty), Some(&self_ty), &args, span)
    else {
        return poison(span);
    };
    op_call(ctx, callee, args, ty, method, span)
}

/// The generic operator impl covering the left operand whose `Rhs` (under
/// the left's binding) is the right operand's type, else one it coerces
/// to, else for a literal a primitive it fits.
pub(crate) fn generic_op_by_rhs<'e>(
    ctx: &LowerCtx<'e>,
    lhs: &HirTy,
    rhs: Option<&HirExpr>,
    lit_rhs: Option<&Expr>,
    method: &str,
) -> Option<&'e GenericMethod> {
    let rhs = rhs?;
    let mut exact = Vec::new();
    let mut loose = Vec::new();
    let mut lit = Vec::new();
    for (gi, mut subst) in ctx.generic_impls(lhs, crate::lower::Pick::Method(method), 0) {
        let gm = &gi.methods[method];
        // No written `Rhs`: the operator traits' default, `Self`.
        let want = match gi.trait_args.first() {
            Some(arg) => {
                // Parameters only `Rhs` names
                // (`impl<A, B, C> Compose<fn(A) -> B> for fn(B) -> C`) bind from the
                // right operand.
                let open: nabic_core::HashSet<String> = gi
                    .generics
                    .params
                    .iter()
                    .filter_map(|gp| match gp {
                        nabic_ast::GenericParam::Type { name, .. }
                        | nabic_ast::GenericParam::Const { name, .. } => {
                            (!subst.contains_key(name)).then(|| name.clone())
                        }
                    })
                    .collect();
                if !open.is_empty() {
                    unify_param_ty_with(arg, &rhs.ty, &open, &mut subst, ctx);
                }
                let mut rcx = LowerCtx::new(ctx.env, &subst, None, ctx.here());
                rcx.current_module.clone_from(&gi.module);
                // A parameter bound from the right operand must meet its
                // bounds: `impl<D: Dist> Tr<D>` does not take an `Iid<D>`
                // that is no `Dist`, so `impl<D: Dist> Tr<Iid<D>>` does.
                let unmet = gi.generics.params.iter().any(|gp| {
                    let nabic_ast::GenericParam::Type { name, bounds, .. } = gp else {
                        return false;
                    };
                    let Some(t) = subst.get(name).filter(|_| open.contains(name)) else {
                        return false;
                    };
                    let t = crate::lower::infer::resolve(ctx, t);
                    !crate::lower::infer::unsolved(ctx, &t)
                        && bounds.iter().any(|b| {
                            ctx.bound_ref(b, &gi.module, |x| rcx.resolve_ast_ty(x))
                                .is_some_and(|want| !ctx.implements_trait(&t, &want, 1))
                        })
                });
                if unmet {
                    continue;
                }
                rcx.resolve_ast_ty(arg)
            }
            // A trait with no parameters (`fn at_most(self, b: f64)`): the
            // method's own type.
            None if ctx
                .env
                .trait_of(&gi.trait_key)
                .is_some_and(|t| t.params.is_empty()) =>
            {
                let mut rcx = LowerCtx::new(ctx.env, &subst, Some(lhs.clone()), ctx.here());
                rcx.current_module.clone_from(&gi.module);
                match gm.def.params.get(1).and_then(|p| p.ty.as_ref()) {
                    Some(t) => rcx.resolve_ast_ty(t),
                    None => continue,
                }
            }
            None => lhs.clone(),
        };
        // An open literal operand as the type it holds.
        let rhs_ty = crate::lower::infer::zonk(ctx, &rhs.ty);
        let open_fits = crate::lower::infer::literal_default(ctx, &rhs.ty).is_some()
            && crate::lower::infer::trial(ctx, &|| crate::lower::infer::unify(ctx, &want, &rhs.ty));
        if want == rhs_ty || want == rhs.ty || open_fits {
            exact.push(gm);
        } else if coercible(ctx, &want, &rhs_ty) {
            loose.push(gm);
        } else if let (HirTy::Prim(_), Some(v)) = (&want, lit_rhs.and_then(untyped_lit_value)) {
            if literal_fits(ctx, v, &want) {
                lit.push(gm);
            }
        }
    }
    match (exact.as_slice(), loose.as_slice(), lit.as_slice()) {
        ([gm], ..) | ([], [gm], _) | ([], [], [gm]) => Some(*gm),
        _ => None,
    }
}

/// Primitive op and result type for `sym` on `prim`; `None` if invalid.
pub(crate) fn infix_for_prim(sym: &str, prim: PrimTy) -> Option<(PrimOp, HirTy)> {
    let bool_ty = HirTy::Prim(PrimTy::Bool);
    let same_ty = HirTy::Prim(prim);
    let arith = |op: PrimOp| Some((op, same_ty.clone()));
    let cmp = |op: PrimOp| Some((op, bool_ty.clone()));
    let num = prim.is_int() || prim.is_float();
    match sym {
        "+" if num => arith(PrimOp::Add),
        "-" if num => arith(PrimOp::Sub),
        "*" if num => arith(PrimOp::Mul),
        "/" if num => arith(PrimOp::Div),
        "%" if num => arith(PrimOp::Rem),
        "%%" if num => arith(PrimOp::Mod),
        "&" if prim.is_int() || matches!(prim, PrimTy::Bool) => arith(PrimOp::BitAnd),
        "|" if prim.is_int() || matches!(prim, PrimTy::Bool) => arith(PrimOp::BitOr),
        "^" if prim.is_int() || matches!(prim, PrimTy::Bool) => arith(PrimOp::BitXor),
        "<<" if prim.is_int() => arith(PrimOp::Shl),
        ">>" if prim.is_int() => arith(PrimOp::Shr),
        // `str` compares contents through its `PartialEq` impl.
        "==" if prim != PrimTy::Str => cmp(PrimOp::Eq),
        "!=" if prim != PrimTy::Str => cmp(PrimOp::Ne),
        "<" if num => cmp(PrimOp::Lt),
        "<=" if num => cmp(PrimOp::Le),
        ">" if num => cmp(PrimOp::Gt),
        ">=" if num => cmp(PrimOp::Ge),
        _ => None,
    }
}

pub(crate) fn prefix_for_prim(sym: &str, prim: PrimTy) -> Option<(PrimOp, HirTy)> {
    let same_ty = HirTy::Prim(prim);
    match sym {
        "-" if prim.is_signed_int() || prim.is_float() => Some((PrimOp::Neg, same_ty)),
        "!" if matches!(prim, PrimTy::Bool) => Some((PrimOp::LogicalNot, same_ty)),
        // On an integer `!` is the bitwise complement, as `~`.
        "~" | "!" if prim.is_int() => Some((PrimOp::Not, same_ty)),
        _ => None,
    }
}
