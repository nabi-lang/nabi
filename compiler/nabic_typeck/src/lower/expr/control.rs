//! Conditions, `if`, loops and `break`.

#[allow(clippy::wildcard_imports)]
use crate::lower::expr::*;

/// A loop or labelled block, lowered again when a later `break` value
/// joined the first one's type into a sum (the earlier `break`s go into
/// it), and a loop body that ends an outer narrowing with it ended from
/// the entry, since the head joins the back edge.
pub(crate) fn lower_loop(ctx: &mut LowerCtx<'_>, expr: &Expr) -> HirExpr {
    let idx = ctx.breakables.len();
    let mark = ctx.probe_mark();
    let log = ctx.unnarrowed.len();
    let e = lower_expr_inner(ctx, expr);
    let joined = ctx.break_joined.remove(&idx);
    let outer: Vec<_> = if matches!(expr.kind, ExprKind::Block(_)) {
        Vec::new()
    } else {
        ctx.unnarrowed[log..]
            .iter()
            .filter(|u| (u.1.0 as usize) < mark.locals)
            .cloned()
            .collect()
    };
    if outer.is_empty() && joined.is_none() {
        return e;
    }
    ctx.rollback(&mark);
    ctx.unnarrowed.truncate(log);
    ctx.unnarrowed.extend(outer);
    ctx.break_preset = joined.map(|t| (idx, t));
    let e = lower_expr_inner(ctx, expr);
    ctx.break_joined.remove(&idx);
    e
}

/// `break` / `continue` must have an enclosing target (a labelled block for
/// a labelled `break`); MIR would emit `unreachable` otherwise.
pub(crate) fn check_jump_target(
    ctx: &LowerCtx<'_>,
    kw: &str,
    label: Option<&str>,
    needs_loop: bool,
) {
    let ok = match label {
        None => ctx.breakables.iter().any(|(_, is_loop)| *is_loop),
        Some(l) => ctx
            .breakables
            .iter()
            .any(|(bl, is_loop)| bl.as_deref() == Some(l) && (*is_loop || !needs_loop)),
    };
    if !ok {
        let what = label.map_or_else(
            || "a loop".to_owned(),
            |l| format!("a loop labelled `'{l}`"),
        );
        ctx.emit_error(Code::OUTSIDE_CONTEXT, format!("`{kw}` outside of {what}"));
    }
}

/// `if` / `while` conditions are `bool`.
pub(crate) fn lower_cond(ctx: &mut LowerCtx<'_>, cond: &Expr) -> HirExpr {
    let c = lower_expected(ctx, cond, HirTy::Prim(PrimTy::Bool));
    let help = (c.ty != HirTy::Prim(PrimTy::Bool))
        .then(|| crate::lower::suggest::truthy_help(ctx, &c))
        .flatten();
    let since = ctx.env.sink.diagnostics.borrow().len();
    let c = coerce(ctx, &HirTy::Prim(PrimTy::Bool), c, &|| {
        "the condition".to_owned()
    });
    if ctx.env.sink.diagnostics.borrow().len() > since {
        ctx.help_last(help);
    }
    c
}

/// Loop bodies yield no value (expected `()`).
pub(crate) fn lower_loop_body(ctx: &mut LowerCtx<'_>, body: &Block) -> HirBlock {
    ctx.push_expected(HirTy::Unit);
    // The body of `try for` is outside the `try`: its throwing calls carry
    // their own.
    let outside = ctx
        .try_for_body
        .take_if(|(at, _)| *at == body.span)
        .map(|(_, d)| d);
    let b = if let Some(d) = outside {
        let depth = std::mem::replace(&mut ctx.try_depth, d);
        let calls = ctx.throwing_calls;
        ctx.in_try_for_body += 1;
        let b = lower_block(ctx, body);
        ctx.in_try_for_body -= 1;
        ctx.try_depth = depth;
        // Calls the body marks do not count for the loop's `try`.
        ctx.throwing_calls = calls;
        b
    } else {
        lower_block(ctx, body)
    };
    ctx.pop_expected();
    b
}

/// Enter a `while` / `for` body: a loop whose value is always `()`.
pub(crate) fn open_unit_loop(ctx: &mut LowerCtx<'_>, label: Option<&String>) -> usize {
    let idx = ctx.breakables.len();
    ctx.break_tys.remove(&idx);
    ctx.unit_loops.insert(idx);
    ctx.breakables.push((label.cloned(), true));
    idx
}

pub(crate) fn close_unit_loop(ctx: &mut LowerCtx<'_>, idx: usize) {
    ctx.breakables.pop();
    ctx.break_tys.remove(&idx);
    ctx.unit_loops.remove(&idx);
}

/// Open a `loop` / labelled block at `breakables[idx]`; an annotated slot
/// types its `break` values.
pub(crate) fn open_break_target(ctx: &mut LowerCtx<'_>, idx: usize) {
    ctx.break_tys.remove(&idx);
    if let Some((_, t)) = ctx.break_preset.take_if(|(i, _)| *i == idx) {
        ctx.break_tys.insert(idx, (t, true));
        return;
    }
    if let Some(t) = ctx
        .current_expected()
        .filter(|t| !matches!(t, HirTy::Unknown | HirTy::Unit))
    {
        ctx.break_tys.insert(idx, (t.clone(), false));
    }
}

/// `break value` to the target at `depth`: typed by the slot, else by the
/// first `break`, which the others must match.
pub(crate) fn lower_break_value(ctx: &mut LowerCtx<'_>, depth: usize, e: &Expr) -> HirExpr {
    let slot = ctx.break_tys.get(&depth).cloned();
    let v = lower_expected(
        ctx,
        e,
        slot.as_ref().map_or(HirTy::Unknown, |(t, _)| t.clone()),
    );
    match slot {
        None => {
            if v.ty != HirTy::Unknown && !hir_expr_diverges(&v) {
                ctx.break_tys.insert(depth, (v.ty.clone(), true));
            }
            v
        }
        // An earlier bare `break;` made the loop's value `()`.
        Some((HirTy::Unit, true))
            if !matches!(v.ty, HirTy::Unknown | HirTy::Unit) && !hir_expr_diverges(&v) =>
        {
            let t = nabic_diagnostic::article(&ctx.ty_name(&v.ty));
            let h = ctx.error_expr(
                Code::BRANCH_TYPES,
                v.span,
                format!("`break` with {t} value, but an earlier `break` of this loop has no value"),
            );
            ctx.help_last(Some(format!(
                "write `break <value>;` with {t} value for the bare `break`, or remove this \
                 value"
            )));
            h
        }
        Some((t, true))
            if v.ty != HirTy::Unknown
                && !crate::lower::infer::agree(ctx, &t, &v.ty, false)
                && !coercible(ctx, &t, &v.ty) =>
        {
            // A sum one of them already is takes both: the earlier
            // `break`s are lowered again into it (`lower_loop`).
            if let Some(j) = branch_join(ctx, &[t.clone(), v.ty.clone()]) {
                ctx.break_tys.insert(depth, (j.clone(), true));
                if j != t {
                    ctx.break_joined.insert(depth, j.clone());
                }
                return coerce(ctx, &j, v, &|| "this `break` value".to_owned());
            }
            // No follow-up error for the loop's value.
            ctx.break_tys.insert(depth, (HirTy::Unknown, true));
            ctx.error_expr(
                Code::BRANCH_TYPES,
                v.span,
                format!(
                    "`break` values have different types `{}` and `{}` - Nabi does not infer \
                     a sum type; annotate the slot (`let v: {} = loop ...`)",
                    ctx.ty_name(&t),
                    ctx.ty_name(&v.ty),
                    ctx.sum_hint(&t, &v.ty)
                ),
            )
        }
        Some((t, _)) => coerce(ctx, &t, v, &|| "this `break` value".to_owned()),
    }
}

pub(crate) fn lower_if(
    ctx: &mut LowerCtx<'_>,
    cond: &Expr,
    then_block: &Block,
    else_branch: Option<&Expr>,
    span: nabic_diagnostic::Span,
) -> HirExpr {
    let cond_h = lower_cond(ctx, cond);
    let tests = test_shape(ctx, cond);
    let cond_h = match skip_then(ctx, tests.as_ref(), cond_h, else_branch, span) {
        Ok(e) => return e,
        Err(c) => c,
    };
    let known = const_cond(ctx, &cond_h);
    let expected = ctx
        .current_expected()
        .cloned()
        .filter(|t| *t != HirTy::Unknown);
    let ended = ctx.unnarrowed.len();
    let dead_then = u32::from(known == Some(false));
    ctx.dead += dead_then;
    let guard = crate::lower::objc::guard(cond);
    if let Some(v) = guard {
        ctx.available.push(v);
    }
    let (mut then_h, by) =
        then_under_tests(ctx, cond, then_block, tests.as_ref(), expected.as_ref());
    if guard.is_some() {
        ctx.available.pop();
    }
    ctx.dead -= dead_then;
    let ended = branch_unnarrowings(ctx, ended);
    // Type tests that hold in this instance: the `else` is never lowered.
    if tests_take_then(ctx, tests.as_ref(), else_branch.is_some()) {
        rejoin_unnarrowings(ctx, ended);
        return then_only(cond_h, then_h, span);
    }
    let build = |cond_h: HirExpr, then_h: HirBlock, else_h: Option<HirExpr>, ty: HirTy| HirExpr {
        kind: HirExprKind::If {
            cond: Box::new(cond_h),
            then_block: Box::new(then_h),
            else_branch: else_h.map(Box::new),
        },
        ty,
        span,
    };
    let Some(else_e) = else_branch else {
        // No else: the if is a statement; any then-value is discarded.
        rejoin_unnarrowings(ctx, ended);
        return build(cond_h, then_h, None, HirTy::Unit);
    };
    // `if x == none { .. } else { x is not none here }`.
    // Only names the branch reads: a narrowed binding reads the value,
    // and a bare tag test must not (a value handed over since).
    let used = crate::lower::expr::narrow::mentioned(else_e);
    let else_list: Vec<_> = else_narrowings(ctx, cond)
        .into_iter()
        .filter(|(x, _)| used.contains(x))
        .collect();
    let since_else = ctx.env.sink.diagnostics.borrow().len();
    let dead_else = u32::from(known == Some(true));
    ctx.dead += dead_else;
    let mut else_h = if else_list.is_empty() {
        lower_expr(ctx, else_e)
    } else {
        ctx.push_scope();
        let lets = bind_narrowings(ctx, else_list, cond.span);
        let e = lower_expr(ctx, else_e);
        ctx.pop_scope();
        with_lets(lets, e)
    };
    ctx.dead -= dead_else;
    else_under_tests(ctx, tests.as_ref(), &by, since_else, &mut then_h, &else_h);
    rejoin_unnarrowings(ctx, ended);
    // An unsuffixed integer literal branch takes the other branch's
    // integer type: `if c { 1 } else { n }` with `n: usize`.
    let int_ty = |h: &HirTy| match h {
        HirTy::Prim(p) if p.is_int() => Some(h.clone()),
        _ => None,
    };
    let then_lit = then_block.trailing.as_deref().filter(|e| is_untyped_int(e));
    let else_lit = match &else_e.kind {
        ExprKind::Block(b) => b.trailing.as_deref().filter(|e| is_untyped_int(e)),
        _ => None,
    };
    if let (Some(lit), Some(t)) = (then_lit, int_ty(&else_h.ty)) {
        if then_h.trailing.as_ref().is_some_and(|e| e.ty != t) {
            then_h.trailing = Some(lower_expected(ctx, lit, t));
        }
    } else if let (Some(lit), Some(t)) = (
        else_lit,
        then_h.trailing.as_ref().and_then(|e| int_ty(&e.ty)),
    ) {
        if else_h.ty != t {
            let v = lower_expected(ctx, lit, t.clone());
            if let HirExprKind::Block(b) = &mut else_h.kind {
                b.trailing = Some(v);
            }
            else_h.ty = t;
        }
    }
    // A diverging branch (`return` / `throw` / ...) takes the other branch's
    // type.
    let then_ty = then_h
        .trailing
        .as_ref()
        .map_or(HirTy::Unit, |e| e.ty.clone());
    let else_ty = else_h.ty.clone();
    let then_div = hir_block_diverges(&then_h);
    let else_div = hir_expr_diverges(&else_h);
    // Branches already fitted to the slot are not coerced again.
    let mut fitted = false;
    let result_ty = if expected == Some(HirTy::Unit) {
        // Statement position: branch values are discarded.
        HirTy::Unit
    } else if then_div {
        else_ty
    } else if else_div
        || then_ty == else_ty
        || then_h
            .trailing
            .as_ref()
            .is_some_and(|t| crate::lower::infer::join(ctx, t, &else_h))
    {
        then_ty
    } else if then_ty == HirTy::Unknown || else_ty == HirTy::Unknown {
        HirTy::Unknown
    } else if let Some(t) = expected.filter(|t| !crate::lower::infer::has_var(ctx, t)) {
        // A typed slot: each branch must fit it, as a `match` arm does.
        if !then_div {
            then_h.trailing = fit_if_branch(ctx, &t, then_h.trailing.take(), "then", &else_ty);
        }
        if !else_div {
            else_h = match else_h.kind {
                HirExprKind::Block(mut b) => {
                    b.trailing = fit_if_branch(ctx, &t, b.trailing.take(), "else", &then_ty);
                    HirExpr {
                        ty: t.clone(),
                        kind: HirExprKind::Block(b),
                        span: else_h.span,
                    }
                }
                _ => {
                    fit_if_branch(ctx, &t, Some(else_h), "else", &then_ty).expect("a branch value")
                }
            };
        }
        fitted = true;
        t
    } else if let Some(t) = branch_join(ctx, &[then_ty.clone(), else_ty.clone()]) {
        t
    } else {
        // Branches of different types are an error, not an inferred sum: the
        // slot's annotation must name the sum.
        ctx.emit_error(
            Code::BRANCH_TYPES,
            format!(
                "`if` branches have different types `{}` and `{}` - Nabi does not infer a \
                 sum type; annotate the slot (`let v: {} = if ...`)",
                ctx.ty_name(&then_ty),
                ctx.ty_name(&else_ty),
                ctx.sum_hint(&then_ty, &else_ty)
            ),
        );
        HirTy::Unknown
    };
    if !fitted && matches!(result_ty, HirTy::Sum(_) | HirTy::AnyTrait(_)) {
        let what = || "the `if` branch".to_owned();
        if let Some(t) = then_h.trailing.take() {
            then_h.trailing = Some(coerce(ctx, &result_ty, t, &what));
        }
        else_h = match else_h.kind {
            HirExprKind::Block(mut b) => {
                if let Some(t) = b.trailing.take() {
                    b.trailing = Some(coerce(ctx, &result_ty, t, &what));
                }
                HirExpr {
                    ty: result_ty.clone(),
                    kind: HirExprKind::Block(b),
                    span: else_h.span,
                }
            }
            _ => coerce(ctx, &result_ty, else_h, &what),
        };
    }
    build(cond_h, then_h, Some(else_h), result_ty)
}

/// The `which` (`then` / `else`) branch value of an `if` in a slot of
/// type `t`: coerced, or reported naming the branch (`other` the other
/// branch's type, for the help). `None` is a unit branch.
pub(crate) fn fit_if_branch(
    ctx: &mut LowerCtx<'_>,
    t: &HirTy,
    v: Option<HirExpr>,
    which: &str,
    other: &HirTy,
) -> Option<HirExpr> {
    let before = ctx.env.sink.diagnostics.borrow().len();
    let what = || format!("the `{which}` branch of `if`");
    let v = match v {
        Some(v) => {
            let at = if v.span.is_dummy() {
                ctx.here()
            } else {
                v.span
            };
            let outer = ctx.at.replace(at);
            let v = coerce(ctx, t, v, &what);
            ctx.at.set(outer);
            Some(v)
        }
        None if !coercible(ctx, t, &HirTy::Unit) => {
            ctx.emit_error(
                Code::MISMATCHED_TYPES,
                format!(
                    "mismatched types: {} expects `{}`, found `()`",
                    what(),
                    ctx.ty_name(t)
                ),
            );
            None
        }
        None => None,
    };
    if ctx.env.sink.diagnostics.borrow().len() > before {
        let have = v.as_ref().map_or(HirTy::Unit, |v| v.ty.clone());
        let t_name = ctx.ty_name(t);
        let mut help = format!(
            "the slot is typed `{t_name}`; give the `{which}` branch a value of type `{t_name}`"
        );
        if coercible(ctx, t, other) {
            help += &format!(
                ", or widen the slot's type to `{t_name} | {}`",
                ctx.ty_name(&have)
            );
        }
        ctx.help_last(Some(help));
    }
    v
}
