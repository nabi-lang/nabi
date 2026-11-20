//! Errors leaving `main` and failed `try!`: reported and exited on.

#[allow(clippy::wildcard_imports)]
use crate::lower::throws::*;

/// Wrap a throwing `main`'s body: an error escaping it
/// prints a message and exits with status 1.
pub(crate) fn uncaught_in_main(
    ctx: &mut LowerCtx<'_>,
    block: HirBlock,
    span: nabic_diagnostic::Span,
) -> HirBlock {
    let frame = pop_catch(ctx);
    if !frame.hit {
        return block;
    }
    let ty = if block.trailing.is_some() {
        ctx.fn_ret_ty.clone()
    } else {
        HirTy::Unit
    };
    let body = HirExpr {
        kind: HirExprKind::Block(Box::new(block)),
        ty: ty.clone(),
        span,
    };
    // `if #which == 0 { report(describe(#err0)) } ... report(describe(#errN))`
    let Some(sig) = ctx.env.sig("nabi_uncaught_error_at").cloned() else {
        return HirBlock {
            stmts: vec![HirStmt::Expr(ctx.error_expr(
                Code::INTERNAL,
                span,
                "internal: runtime hook `nabi_uncaught_error_at` is not declared".to_owned(),
            ))],
            trailing: None,
            label: None,
        };
    };
    let site = hir_local(
        frame.site.expect("main frame records sites"),
        &HirTy::Prim(PrimTy::Str),
        span,
    );
    let report = per_error(ctx, &frame, span, |_, msg| {
        HirStmt::Panic(HirExpr {
            kind: HirExprKind::Call {
                callee: sig.id,
                args: vec![msg, site.clone()],
            },
            ty: HirTy::Unit,
            span,
        })
    });
    let valued = ty != HirTy::Unit;
    let wrapped = shell(ctx, &frame, body, ty, (report, None), span);
    // A unit / diverging body stays a statement: falling off the end of
    // an `i32` main is not a unit return.
    let (stmts, trailing) = if valued {
        (Vec::new(), Some(wrapped))
    } else {
        (vec![HirStmt::Expr(wrapped)], None)
    };
    HirBlock {
        stmts,
        trailing,
        label: None,
    }
}

/// A failed `try!`'s panic (`nabi_try_failed`): the error, where it was
/// thrown and where it reached the `try!`; the panic's own position is
/// the `try!`.
pub(crate) fn force_report(
    ctx: &mut LowerCtx<'_>,
    frame: &CatchFrame,
    span: nabic_diagnostic::Span,
) -> Vec<HirStmt> {
    let site = hir_local(
        frame.site.expect("try! frame records sites"),
        &HirTy::Prim(PrimTy::Str),
        span,
    );
    per_error(ctx, frame, span, |ctx, msg| {
        let call = hook_call(ctx, "nabi_try_failed", "", span);
        let HirExprKind::Call { callee, .. } = call.kind else {
            return HirStmt::Panic(call);
        };
        HirStmt::Panic(HirExpr {
            kind: HirExprKind::Call {
                callee,
                args: vec![msg, site.clone()],
            },
            ty: HirTy::Unit,
            span,
        })
    })
}

/// `if #which == 0 { report(describe(#err0)) } ... report(describe(#errN))`
/// over the errors `frame` parked.
pub(crate) fn per_error(
    ctx: &mut LowerCtx<'_>,
    frame: &CatchFrame,
    span: nabic_diagnostic::Span,
    mut report_one: impl FnMut(&mut LowerCtx<'_>, HirExpr) -> HirStmt,
) -> Vec<HirStmt> {
    let which = frame
        .arms
        .as_ref()
        .map(|a| a.1)
        .expect("frame parks errors");
    let mut report = Vec::new();
    for (k, (ety, local)) in frame.errs.iter().enumerate() {
        let msg = describe_err(ctx, hir_local(*local, ety, span), span);
        let call = report_one(ctx, msg);
        if k + 1 == frame.errs.len() {
            report.push(call);
            break;
        }
        let cond = hir_binop(
            PrimOp::Eq,
            hir_local(which, &HirTy::Prim(PrimTy::I32), span),
            hir_int(
                i128::try_from(k).expect("error index fits"),
                PrimTy::I32,
                span,
            ),
            HirTy::Prim(PrimTy::Bool),
            span,
        );
        report.push(HirStmt::Expr(HirExpr {
            kind: HirExprKind::If {
                cond: Box::new(cond),
                then_block: Box::new(HirBlock {
                    stmts: vec![call],
                    trailing: None,
                    label: None,
                }),
                else_branch: None,
            },
            ty: HirTy::Unit,
            span,
        }));
    }
    report
}

/// Push the frame of a throwing `main`'s body: it takes every error and
/// parks it for the report.
pub(crate) fn main_frame(ctx: &mut LowerCtx<'_>) {
    let which = ctx.declare(None, HirTy::Prim(PrimTy::I32));
    let site = ctx.declare(None, HirTy::Prim(PrimTy::Str));
    let label = fresh_label(ctx);
    ctx.err_frames.push(ErrFrame::Catch(CatchFrame {
        label,
        arms: Some((vec![None], which)),
        errs: Vec::new(),
        hit: false,
        raised: Vec::new(),
        site: Some(site),
    }));
}

/// An uncaught error prints as its `Display`, else its
/// `Debug`, else its type name.
pub(crate) fn describe_err(
    ctx: &mut LowerCtx<'_>,
    err: HirExpr,
    span: nabic_diagnostic::Span,
) -> HirExpr {
    if ctx.has_trait(&err.ty, ctx.lang("display_trait")) {
        return format_value(ctx, err, false, span);
    }
    if ctx.has_trait(&err.ty, ctx.lang("debug_trait")) {
        return format_value(ctx, err, true, span);
    }
    HirExpr {
        kind: HirExprKind::Const(ConstVal::Str(ctx.ty_name(&err.ty))),
        ty: HirTy::Prim(PrimTy::Str),
        span,
    }
}

/// `{ let #eN = v; f"{#eN}" }`, `{#eN:?}` when `debug`; the caller checks
/// the trait.
pub(crate) fn format_value(
    ctx: &mut LowerCtx<'_>,
    v: HirExpr,
    debug: bool,
    span: nabic_diagnostic::Span,
) -> HirExpr {
    let spec = if debug {
        vec![nabic_ast::SpecPiece::Text("?".to_owned())]
    } else {
        Vec::new()
    };
    ctx.push_scope();
    let name = format!("#e{}", ctx.locals.len());
    let local = ctx.declare(Some(name.clone()), v.ty.clone());
    let hole = nabic_ast::FStringPart::Hole {
        expr: Expr {
            kind: ExprKind::Path(AstPath {
                segments: vec![nabic_ast::PathSegment {
                    name,
                    generics: Vec::new(),
                }],
                span,
                origin: None,
            }),
            span,
        },
        spec,
    };
    let text = crate::lower::fstring::lower_fstring(ctx, &[hole], span);
    ctx.pop_scope();
    HirExpr {
        ty: HirTy::Prim(PrimTy::Str),
        kind: HirExprKind::Block(Box::new(HirBlock {
            stmts: vec![HirStmt::Let { local, init: v }],
            trailing: Some(text),
            label: None,
        })),
        span,
    }
}
