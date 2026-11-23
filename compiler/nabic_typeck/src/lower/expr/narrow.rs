//! Narrowing by `is` and `none` tests.

#[allow(clippy::wildcard_imports)]
use crate::lower::expr::*;

/// What a condition says about a local: it is `T`, or it is not `none`.
pub(crate) enum Narrow {
    Ty(nabic_ast::Ty),
    Some,
}

/// Is `e` a local whose type has `none` in it?
pub(crate) fn optional_local(ctx: &LowerCtx<'_>, e: &Expr) -> Option<String> {
    let ExprKind::Path(p) = &e.kind else {
        return None;
    };
    let [seg] = p.segments.as_slice() else {
        return None;
    };
    let l = ctx.lookup(&seg.name)?;
    matches!(ctx.var_ty(l), HirTy::Sum(vs) if vs.contains(&HirTy::None)).then(|| seg.name.clone())
}

/// `x == none` / `none == x` / `x is none` (`eq`) or `x != none`: the
/// optional local.
pub(crate) fn none_test(ctx: &LowerCtx<'_>, cond: &Expr, eq: bool) -> Option<String> {
    let none = |e: &Expr| matches!(e.kind, ExprKind::Literal(AstLit::None));
    match &cond.kind {
        ExprKind::OpInfix { op, lhs, rhs } if op == if eq { "==" } else { "!=" } => {
            if none(rhs) {
                optional_local(ctx, lhs)
            } else if none(lhs) {
                optional_local(ctx, rhs)
            } else {
                None
            }
        }
        ExprKind::Is(operand, pat)
            if eq && matches!(pat.kind, AstPatternKind::Literal(AstLit::None)) =>
        {
            optional_local(ctx, operand)
        }
        _ => None,
    }
}

/// The `x is T` and `x != none` conjuncts of `cond` (`a && b && ..`):
/// locals that are `T` (not `none`) wherever `cond` holds.
pub(crate) fn narrowings(ctx: &LowerCtx<'_>, cond: &Expr) -> Vec<(String, Narrow)> {
    if let Some(x) = none_test(ctx, cond, false) {
        return vec![(x, Narrow::Some)];
    }
    // `!(x is none)`.
    if let ExprKind::OpPrefix { op, operand } = &cond.kind {
        if let Some(x) = none_test(ctx, operand, true).filter(|_| op == "!") {
            return vec![(x, Narrow::Some)];
        }
    }
    match &cond.kind {
        ExprKind::OpInfix { op, lhs, rhs } if op == "&&" => {
            let mut v = narrowings(ctx, lhs);
            v.extend(narrowings(ctx, rhs));
            v
        }
        ExprKind::Is(operand, pat) => match (&operand.kind, &pat.kind) {
            (ExprKind::Path(p), AstPatternKind::TypeCheck { binder: None, ty })
                if p.segments.len() == 1
                    && ctx.lookup(&p.segments[0].name).is_some_and(|l| {
                        // An enum's `x is E::V` is a variant test: no narrowing.
                        // A plain member of the target sum stays as it is.
                        let lt = ctx.var_ty(l);
                        !matches!(lt, HirTy::Enum(_))
                            && !matches!(ctx.resolve_ast_ty(ty), HirTy::Sum(ws) if ws.contains(&lt))
                    }) =>
            {
                vec![(p.segments[0].name.clone(), Narrow::Ty(ty.clone()))]
            }
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

/// The `x == none` disjuncts of `cond` (`a || b || ..`): locals that are
/// not `none` wherever `cond` fails.
pub(crate) fn else_narrowings(ctx: &LowerCtx<'_>, cond: &Expr) -> Vec<(String, Narrow)> {
    if let Some(x) = none_test(ctx, cond, true) {
        return vec![(x, Narrow::Some)];
    }
    match &cond.kind {
        ExprKind::OpInfix { op, lhs, rhs } if op == "||" => {
            let mut v = else_narrowings(ctx, lhs);
            v.extend(else_narrowings(ctx, rhs));
            v
        }
        _ => Vec::new(),
    }
}

/// What holds after an `if` statement one of whose branches always
/// leaves: the other branch's narrowings.
pub(crate) fn guard_narrowings(ctx: &LowerCtx<'_>, s: &Stmt, h: &HirStmt) -> Vec<(String, Narrow)> {
    let (StmtKind::Expr(Expr {
        kind: ExprKind::If { cond, .. },
        ..
    })
    | StmtKind::Trailing(Expr {
        kind: ExprKind::If { cond, .. },
        ..
    })) = &s.kind
    else {
        return Vec::new();
    };
    let HirStmt::Expr(HirExpr {
        kind:
            HirExprKind::If {
                then_block,
                else_branch,
                ..
            },
        ..
    }) = h
    else {
        return Vec::new();
    };
    if hir_block_diverges(then_block) {
        else_narrowings(ctx, cond)
    } else if else_branch.as_deref().is_some_and(hir_expr_diverges) {
        narrowings(ctx, cond)
    } else {
        Vec::new()
    }
}

/// Rebind each narrowed local in the current scope as
/// `let x = match x { x: T => x };`; writes go through to the original
/// (`narrowed`). A `[ref]` cell is re-tested on every read (`cell_views`),
/// since a capture may store a non-`T`.
pub(crate) fn bind_narrowed(ctx: &mut LowerCtx<'_>, cond: &Expr) -> Vec<HirStmt> {
    let list = narrowings(ctx, cond);
    bind_narrowings(ctx, list, cond.span)
}

pub(crate) fn bind_narrowings(
    ctx: &mut LowerCtx<'_>,
    list: Vec<(String, Narrow)>,
    at: nabic_diagnostic::Span,
) -> Vec<HirStmt> {
    let mut lets = Vec::new();
    for (name, how) in list {
        // A `for ref` element is read afresh at each use: not narrowed.
        let Some(orig) = ctx.lookup(&name).filter(|l| !ctx.places.contains_key(l)) else {
            continue;
        };
        let orig_ty = ctx.var_ty(orig);
        let pat_h = match (how, &orig_ty) {
            (Narrow::Ty(ty), _) => {
                let pat = nabic_ast::Pattern {
                    kind: AstPatternKind::TypeCheck {
                        binder: Some(name.clone()),
                        ty,
                    },
                    span: at,
                };
                lower_pattern(ctx, &pat, &orig_ty)
            }
            (Narrow::Some, HirTy::Sum(vs)) => {
                let idxs: Vec<usize> = (0..vs.len()).filter(|i| vs[*i] != HirTy::None).collect();
                crate::lower::pattern::narrow_to(ctx, &name, &orig_ty, &idxs, at)
            }
            (Narrow::Some, _) => continue,
        };
        // A narrowing rebinding is never a `[ref]` cell.
        ctx.pending_cells.clear();
        ctx.pending_views.clear();
        ctx.pending_views.clear();
        let resums = ctx.take_resums(at);
        let Some(new) = ctx.lookup_in_current_scope(&name) else {
            continue;
        };
        let new_ty = ctx.local_ty(new);
        ctx.narrowed.insert(new, (orig, orig_ty.clone()));
        let mut arms = vec![HirMatchArm {
            pat: pat_h,
            guard: None,
            body: with_lets(resums, hir_local(new, &new_ty, at)),
        }];
        let is_cell = ctx.is_boxed(orig) || ctx.cell_views.contains_key(&orig);
        if is_cell {
            let msg = format!(
                "`{name}` no longer holds `{}`: a `[ref {name}]` capture changed it inside \
                 its narrowing",
                ctx.ty_name(&new_ty)
            );
            arms.push(HirMatchArm {
                pat: HirPattern::Wildcard,
                guard: None,
                body: HirExpr {
                    kind: HirExprKind::Block(Box::new(HirBlock {
                        stmts: vec![HirStmt::Panic(crate::lower::throws::hook_call(
                            ctx,
                            "nabi_panic_at",
                            &msg,
                            at,
                        ))],
                        trailing: None,
                        label: None,
                    })),
                    ty: new_ty.clone(),
                    span: at,
                },
            });
        }
        let read = HirExpr {
            kind: HirExprKind::Match {
                scrutinee: Box::new(ctx.read_local(orig, at)),
                arms,
            },
            ty: new_ty,
            span: at,
        };
        if is_cell {
            ctx.cell_views.insert(new, read.clone());
        }
        lets.push(HirStmt::Let {
            local: new,
            init: read,
        });
    }
    lets
}

/// A block that runs only when `cond` held, with its narrowings bound.
pub(crate) fn lower_narrowed_block(ctx: &mut LowerCtx<'_>, cond: &Expr, block: &Block) -> HirBlock {
    ctx.push_scope();
    let lets = bind_narrowed(ctx, cond);
    let mut b = lower_block(ctx, block);
    ctx.pop_scope();
    if !lets.is_empty() {
        b.stmts.splice(0..0, lets);
    }
    b
}

/// `v is T`: `match v { _: T => true, _ => false }`.
pub(crate) fn lower_is(
    ctx: &mut LowerCtx<'_>,
    operand: &Expr,
    pat: &nabic_ast::Pattern,
    span: nabic_diagnostic::Span,
) -> HirExpr {
    if let Some(e) = lower_test(ctx, operand, pat, span) {
        return e;
    }
    let scrut = lower_expected(ctx, operand, HirTy::Unknown);
    if scrut.ty == HirTy::Unknown {
        return scrut;
    }
    lower_is_on(ctx, scrut, pat, true, span)
}

/// `scrut is pat`: `hit` when it matches, `!hit` otherwise.
pub(crate) fn lower_is_on(
    ctx: &mut LowerCtx<'_>,
    mut scrut: HirExpr,
    pat: &nabic_ast::Pattern,
    hit: bool,
    span: nabic_diagnostic::Span,
) -> HirExpr {
    // A type test of a plain value needs its type now: an open literal
    // takes its default (`let v = 3; v is i64` is true).
    if !matches!(scrut.ty, HirTy::Sum(_)) && crate::lower::infer::has_var(ctx, &scrut.ty) {
        scrut.ty = crate::lower::infer::fix_literals(ctx, &scrut.ty);
    }
    let lit = |b: bool| HirExpr {
        kind: HirExprKind::Const(ConstVal::Bool(b)),
        ty: HirTy::Prim(PrimTy::Bool),
        span,
    };
    // `e is Enum::Variant` on an enum value: a variant test (any payload),
    // not a type test.
    let variant_pat;
    let pat = match (&scrut.ty, &pat.kind) {
        (
            HirTy::Enum(_),
            AstPatternKind::TypeCheck {
                binder: None,
                ty:
                    nabic_ast::Ty {
                        kind: nabic_ast::TyKind::Path(path),
                        ..
                    },
            },
        ) if path.segments.len() >= 2 => {
            variant_pat = nabic_ast::Pattern {
                kind: AstPatternKind::EnumVariant {
                    path: path.clone(),
                    args: vec![nabic_ast::Pattern {
                        kind: AstPatternKind::Rest(None),
                        span: pat.span,
                    }],
                },
                span: pat.span,
            };
            &variant_pat
        }
        _ => pat,
    };
    // A static (non-`any`, non-sum) operand: the answer is known now.
    if let (false, AstPatternKind::TypeCheck { ty, .. }) = (
        matches!(scrut.ty, HirTy::AnyTrait(_) | HirTy::Sum(_)),
        &pat.kind,
    ) {
        // `A is (A | B)`: membership of the value's type in the sum.
        let same = match ctx.resolve_ast_ty(ty) {
            HirTy::Sum(ws) => ws.contains(&scrut.ty),
            t => t == scrut.ty,
        };
        return with_lets(vec![HirStmt::Expr(scrut)], lit(same == hit));
    }
    ctx.push_scope();
    let pat_h = lower_pattern(ctx, pat, &scrut.ty);
    // `is` binders aren't `[ref]` cells (no scope to start one in); capturing
    // one by `ref` is reported at the closure.
    ctx.pending_cells.clear();
    ctx.pending_views.clear();
    // An `is v: A | B` sub-sum binder stays unconverted; the
    // `if x is (A | B)` narrowing form converts.
    ctx.pending_resums.clear();
    ctx.pop_scope();
    HirExpr {
        kind: HirExprKind::Match {
            scrutinee: Box::new(scrut),
            arms: vec![
                HirMatchArm {
                    pat: pat_h,
                    guard: None,
                    body: lit(hit),
                },
                HirMatchArm {
                    pat: HirPattern::Wildcard,
                    guard: None,
                    body: lit(!hit),
                },
            ],
        },
        ty: HirTy::Prim(PrimTy::Bool),
        span,
    }
}
