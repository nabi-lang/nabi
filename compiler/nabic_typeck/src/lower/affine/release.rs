//! Release flags and the finally blocks the lowering adds.

#[allow(clippy::wildcard_imports)]
use crate::lower::affine::*;

/// The finally block releasing local `n` when its flag is set.
pub(crate) fn release_block(n: u32, span: Span) -> Block {
    let call = Expr {
        span,
        kind: ExprKind::MethodCall {
            receiver: Box::new(path_expr(&format!("{REL}{n}"), span)),
            method: nabic_ast::USING_DROP_METHOD.to_owned(),
            type_args: Vec::new(),
            args: Vec::new(),
            trailing_closure: None,
            origin: None,
            written: false,
        },
    };
    let release = Expr {
        span,
        kind: ExprKind::If {
            cond: Box::new(path_expr(&format!("{OWN}{n}"), span)),
            then_branch: Block {
                stmts: vec![Stmt {
                    kind: StmtKind::Expr(call),
                    span,
                }],
                trailing: None,
                span,
                label: None,
            },
            else_branch: None,
        },
    };
    Block {
        stmts: vec![Stmt {
            kind: StmtKind::Expr(release),
            span,
        }],
        trailing: None,
        span,
        label: None,
    }
}

pub(crate) const fn flag_value(on: bool, span: Span) -> HirExpr {
    HirExpr {
        kind: HirExprKind::Const(ConstVal::Bool(on)),
        ty: HirTy::Prim(PrimTy::Bool),
        span,
    }
}

/// `local` (of type `ty`) now holds a value the compiler releases on
/// every exit of the current scope while its flag is set: the flag's
/// `let`, when `ty` is released that way.
pub(crate) fn owe(ctx: &mut LowerCtx<'_>, local: LocalId, ty: &HirTy) -> Option<HirStmt> {
    // A type that only closes (`io::Close` without `Drop`) has nothing
    // to release: the check makes the program close it.
    if affine(ctx, ty).is_none()
        || ctx.lang("drop_method").is_empty()
        || !crate::lower::expr::needs_drop(ctx, ty, 0)
    {
        return None;
    }
    let n = local.0;
    ctx.owed_at.insert(local, ctx.here());
    let flag = ctx.declare(Some(format!("{OWN}{n}")), HirTy::Prim(PrimTy::Bool));
    if let Some(scope) = ctx.scopes.last_mut() {
        scope.insert(format!("{REL}{n}"), local);
    }
    ctx.err_frames.push(ErrFrame::Finally {
        block: release_block(n, Span::DUMMY),
        hir: Vec::new(),
        try_depth: ctx.try_depth,
        breakables: ctx.breakables.len(),
    });
    Some(HirStmt::Let {
        local: flag,
        init: flag_value(true, Span::DUMMY),
    })
}

/// The lets in `stmts` from `from` on that bind a released value: their
/// flags follow each.
pub(crate) fn owe_lets(ctx: &mut LowerCtx<'_>, stmts: &mut Vec<HirStmt>, from: usize) {
    let mut i = from;
    while i < stmts.len() {
        let bound = match &stmts[i] {
            HirStmt::Let { local, init }
                if !is_flag_name(ctx.locals[local.0 as usize].name.as_deref()) =>
            {
                let ty = match ctx.local_ty(*local) {
                    HirTy::Unknown | HirTy::Var(_) => init.ty.clone(),
                    t => t,
                };
                Some((*local, ty))
            }
            _ => None,
        };
        i += 1;
        if let Some((local, ty)) = bound {
            if let Some(s) = owe(ctx, local, &ty) {
                stmts.insert(i, s);
                i += 1;
            }
        }
    }
}

pub(crate) fn is_flag_name(n: Option<&str>) -> bool {
    n.is_some_and(|n| n.starts_with(OWN))
}

/// Close the releases owed since `mark` (err frames): on normal
/// completion of the scope they run after its value.
pub(crate) fn settle(
    ctx: &mut LowerCtx<'_>,
    mark: usize,
    stmts: &mut Vec<HirStmt>,
    trailing: &mut Option<HirExpr>,
    span: Span,
) {
    if ctx.err_frames.len() <= mark {
        return;
    }
    let mut blocks = Vec::new();
    while ctx.err_frames.len() > mark {
        match ctx.err_frames.pop() {
            Some(ErrFrame::Finally { block, .. }) => blocks.push(block),
            Some(f) => {
                // Not ours: frames nest, so this cannot happen.
                ctx.err_frames.push(f);
                break;
            }
            None => break,
        }
    }
    let ends = trailing
        .as_ref()
        .map_or_else(|| stmts_diverge(stmts), crate::lower::hir_expr_diverges);
    if ends {
        return;
    }
    if let Some(t) = trailing.take() {
        if t.ty == HirTy::Unit {
            stmts.push(HirStmt::Expr(t));
        } else {
            let ty = t.ty.clone();
            let tmp = ctx.declare(Some(format!("#val{}", ctx.locals.len())), ty.clone());
            stmts.push(HirStmt::Let {
                local: tmp,
                init: t,
            });
            *trailing = Some(crate::lower::expr::hir_local(tmp, &ty, span));
        }
    }
    for b in blocks {
        ctx.push_expected(HirTy::Unit);
        // The release has no span of its own: what it reports is here.
        let h = ctx.at_span(span, |ctx| crate::lower::expr::lower_block(ctx, &b));
        ctx.pop_expected();
        stmts.push(HirStmt::Expr(HirExpr {
            kind: HirExprKind::Block(Box::new(h)),
            ty: HirTy::Unit,
            span,
        }));
    }
}

pub(crate) fn stmts_diverge(stmts: &[HirStmt]) -> bool {
    stmts.last().is_some_and(|s| match s {
        HirStmt::Return(_)
        | HirStmt::Panic(_)
        | HirStmt::Break { .. }
        | HirStmt::Continue { .. } => true,
        HirStmt::Expr(e) => crate::lower::hir_expr_diverges(e),
        _ => false,
    })
}

/// `settle` for a body that is one expression (a closure's, a coroutine's).
pub(crate) fn settle_expr(ctx: &mut LowerCtx<'_>, mark: usize, e: HirExpr) -> HirExpr {
    if ctx.err_frames.len() <= mark {
        return e;
    }
    let (ty, span) = (e.ty.clone(), e.span);
    let mut stmts = Vec::new();
    let mut trailing = Some(e);
    settle(ctx, mark, &mut stmts, &mut trailing, span);
    let ty = if trailing.is_none() && ty != HirTy::Never {
        HirTy::Unit
    } else {
        ty
    };
    HirExpr {
        kind: HirExprKind::Block(Box::new(HirBlock {
            stmts,
            trailing,
            label: None,
        })),
        ty,
        span,
    }
}

/// `x = value` for a local with a release flag: the old value is
/// released first (after `value`, which may move it), then the flag set.
pub(crate) fn reassign(ctx: &mut LowerCtx<'_>, stmt: HirStmt, span: Span) -> HirStmt {
    let HirStmt::Assign { local, value } = stmt else {
        return stmt;
    };
    let Some(flag) = ctx.lookup(&format!("{OWN}{}", local.0)) else {
        return HirStmt::Assign { local, value };
    };
    let ty = value.ty.clone();
    let tmp = ctx.declare(Some(format!("#mv{}", local.0)), ty.clone());
    ctx.push_expected(HirTy::Unit);
    let release = crate::lower::expr::lower_block(ctx, &release_block(local.0, Span::DUMMY));
    ctx.pop_expected();
    let stmts = vec![
        HirStmt::Let {
            local: tmp,
            init: value,
        },
        HirStmt::Expr(HirExpr {
            kind: HirExprKind::Block(Box::new(release)),
            ty: HirTy::Unit,
            span,
        }),
        HirStmt::Assign {
            local,
            value: crate::lower::expr::hir_local(tmp, &ty, span),
        },
        HirStmt::Assign {
            local: flag,
            value: flag_value(true, Span::DUMMY),
        },
    ];
    HirStmt::Expr(HirExpr {
        kind: HirExprKind::Block(Box::new(HirBlock {
            stmts,
            trailing: None,
            label: None,
        })),
        ty: HirTy::Unit,
        span,
    })
}
