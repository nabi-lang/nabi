//! `catch` arms, `finally` blocks and error dispatch.

#[allow(clippy::wildcard_imports)]
use crate::lower::throws::*;

/// Reject `return`, `break` and `continue` that leave `fin`: they would
/// drop an error in flight. Jumps inside a closure, or to a loop or
/// labelled block within `fin`, stay.
pub(crate) fn finally_jumps(ctx: &LowerCtx<'_>, fin: &Block) {
    #[derive(Default)]
    struct Jumps {
        bodies: u32,
        loops: Vec<(nabic_diagnostic::Span, Option<String>)>,
        blocks: Vec<(nabic_diagnostic::Span, String)>,
        jumps: Vec<(nabic_diagnostic::Span, &'static str, Option<String>)>,
    }
    impl crate::walk::AstVisitor for Jumps {
        fn expr(&mut self, e: &Expr) {
            match &e.kind {
                ExprKind::Loop { label, .. }
                | ExprKind::While { label, .. }
                | ExprKind::For { label, .. } => self.loops.push((e.span, label.clone())),
                ExprKind::Block(b) | ExprKind::Unsafe(b) => {
                    if let Some(l) = &b.label {
                        self.blocks.push((b.span, l.clone()));
                    }
                }
                _ => {}
            }
        }
        fn stmt(&mut self, s: &nabic_ast::Stmt) {
            if self.bodies > 0 {
                return;
            }
            let (what, label) = match &s.kind {
                StmtKind::Return(_) => ("return", None),
                StmtKind::Break { label, .. } => ("break", label.clone()),
                StmtKind::Continue { label } => ("continue", label.clone()),
                _ => return,
            };
            self.jumps.push((s.span, what, label));
        }
        fn enter_body(&mut self) {
            self.bodies += 1;
        }
        fn leave_body(&mut self) {
            self.bodies -= 1;
        }
    }
    let mut w = Jumps::default();
    crate::walk::walk_block(&mut w, fin);
    let within = |outer: nabic_diagnostic::Span, at: nabic_diagnostic::Span| {
        outer.start <= at.start && at.end <= outer.end
    };
    for (at, what, label) in w.jumps {
        let stays = what != "return"
            && match &label {
                None => w.loops.iter().any(|(s, _)| within(*s, at)),
                Some(l) => {
                    w.loops
                        .iter()
                        .any(|(s, ll)| ll.as_deref() == Some(l.as_str()) && within(*s, at))
                        || w.blocks.iter().any(|(s, bl)| bl == l && within(*s, at))
                }
            };
        if stays {
            continue;
        }
        ctx.emit_error_help(
            Code::OUTSIDE_CONTEXT,
            at,
            format!("`{what}` cannot leave a `finally` block: it would drop the error in flight"),
            format!("move the `{what}` after the `try` expression; `finally` only cleans up"),
        );
    }
}

/// Postfix `body catch pat { .. } ... finally { .. }`.
pub(crate) fn lower_catch(
    ctx: &mut LowerCtx<'_>,
    body: &Expr,
    arms: &[nabic_ast::CatchArm],
    finally: Option<&Block>,
    span: nabic_diagnostic::Span,
) -> HirExpr {
    let expected = ctx.current_expected().cloned().unwrap_or(HirTy::Unknown);
    if let Some(fin) = finally {
        finally_jumps(ctx, fin);
        ctx.err_frames.push(ErrFrame::Finally {
            block: fin.clone(),
            hir: Vec::new(),
            try_depth: ctx.try_depth,
            breakables: ctx.breakables.len(),
        });
    }
    let value = if arms.is_empty() {
        lower_expected(ctx, body, expected)
    } else {
        lower_catch_arms(ctx, body, arms, &expected, span)
    };
    if finally.is_none() {
        return value;
    }
    let Some(ErrFrame::Finally { block, .. }) = ctx.err_frames.pop() else {
        unreachable!("frames are pushed and popped in pairs")
    };
    if hir_expr_diverges(&value) {
        return value;
    }
    // Normal completion (body or handler value): run `finally`, then
    // yield the value.
    ctx.push_expected(HirTy::Unit);
    let fin = lower_block(ctx, &block);
    ctx.pop_expected();
    let ty = value.ty.clone();
    let mut stmts = Vec::new();
    let trailing = if ty == HirTy::Unit {
        stmts.push(HirStmt::Expr(value));
        None
    } else {
        let t = ctx.declare(None, ty.clone());
        stmts.push(HirStmt::Let {
            local: t,
            init: value,
        });
        Some(hir_local(t, &ty, span))
    };
    stmts.push(HirStmt::Expr(HirExpr {
        kind: HirExprKind::Block(Box::new(fin)),
        ty: HirTy::Unit,
        span,
    }));
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

pub(crate) fn lower_catch_arms(
    ctx: &mut LowerCtx<'_>,
    body: &Expr,
    arms: &[nabic_ast::CatchArm],
    expected: &HirTy,
    span: nabic_diagnostic::Span,
) -> HirExpr {
    let filters: Vec<Option<HirTy>> = arms.iter().map(|a| arm_filter(ctx, &a.pat)).collect();
    let which = ctx.declare(None, HirTy::Prim(PrimTy::I32));
    let label = fresh_label(ctx);
    ctx.err_frames.push(ErrFrame::Catch(CatchFrame {
        label,
        arms: Some((filters.clone(), which)),
        errs: Vec::new(),
        hit: false,
        raised: Vec::new(),
        site: None,
    }));
    let body_h = lower_expected(ctx, body, expected.clone());
    let frame = pop_catch(ctx);
    // Arms no raised error type can take never run, but are still
    // type-checked (and flagged).
    let check_hint = if *expected == HirTy::Unknown && !hir_expr_diverges(&body_h) {
        body_h.ty.clone()
    } else {
        expected.clone()
    };
    if frame.errs.is_empty() {
        ctx.emit_warning(
            Code::UNNEEDED_TRY,
            "no error reaches this `catch`: its body can't throw".to_owned(),
        );
    }
    for (i, (arm, filter)) in arms.iter().zip(&filters).enumerate() {
        // Reached by an error no earlier arm takes whole.
        let earlier = |t: &HirTy| {
            arms[..i]
                .iter()
                .zip(&filters)
                .any(|(a, f)| takes_all(&a.pat, f.as_ref(), t))
        };
        if frame
            .errs
            .iter()
            .any(|(t, _)| may_take(filter.as_ref(), t) && !earlier(t))
        {
            continue;
        }
        if frame.errs.iter().any(|(t, _)| may_take(filter.as_ref(), t)) {
            ctx.emit_warning(
                Code::UNNEEDED_TRY,
                "this `catch` arm never runs: the arms before it take every error that \
                 reaches it"
                    .to_owned(),
            );
        } else if !frame.errs.is_empty() {
            ctx.emit_warning(
                Code::UNNEEDED_TRY,
                format!(
                    "this `catch` arm never runs: no `{}` error reaches it",
                    filter
                        .as_ref()
                        .map_or_else(|| "_".to_owned(), |f| ctx.ty_name(f))
                ),
            );
        }
        check_dead_arm(ctx, arm, filter.as_ref(), check_hint.clone(), span);
    }
    if frame.errs.is_empty() {
        return body_h;
    }
    // Dispatch on the parked error, built as AST so arms lower like any
    // `match`: `if #which == k { match #err_k { <arms for E_k>, _ => throw
    // #err_k } } else ..`.
    ctx.next_synth += 1;
    let n = ctx.next_synth;
    ctx.push_scope();
    let which_name = format!("#which{n}");
    bind_name(ctx, &which_name, which);
    let mut branches = Vec::with_capacity(frame.errs.len());
    let mut passes_on = false;
    for (k, (ety, local)) in frame.errs.iter().enumerate() {
        let name = format!("#err{n}_{k}");
        bind_name(ctx, &name, *local);
        let (b, passes) = dispatch_one(ctx, arms, &filters, ety, &name, span);
        passes_on |= passes;
        branches.push(b);
    }
    let mut chain = branches.pop().expect("at least one parked error");
    for (k, b) in branches.into_iter().enumerate().rev() {
        chain = Expr {
            kind: ExprKind::If {
                cond: Box::new(Expr {
                    kind: ExprKind::OpInfix {
                        op: "==".to_owned(),
                        lhs: Box::new(path_expr(&which_name, span)),
                        rhs: Box::new(Expr {
                            kind: ExprKind::Literal(AstLit::Int(k.to_string())),
                            span,
                        }),
                    },
                    span,
                }),
                then_branch: Block {
                    stmts: Vec::new(),
                    trailing: Some(Box::new(b)),
                    span,
                    label: None,
                },
                else_branch: Some(Box::new(chain)),
            },
            span,
        };
    }
    let body_div = hir_expr_diverges(&body_h);
    let hint = if *expected != HirTy::Unknown || body_div {
        expected.clone()
    } else {
        body_h.ty.clone()
    };
    // The handler took the error, unless it passes it on: then its site
    // is kept for the rethrow. A handler that calls `nabi_throw_site_take`
    // itself (the library carrying the site across threads) clears it
    // that way.
    let take = ctx.env.sig("nabi_throw_site_take").map(|s| s.id);
    let saved = take
        .filter(|_| passes_on)
        .map(|_| ctx.declare(None, HirTy::Prim(PrimTy::Str)));
    let outer = std::mem::replace(&mut ctx.rethrow_site, saved);
    let handler = lower_expected(ctx, &chain, hint.clone());
    ctx.rethrow_site = outer;
    ctx.pop_scope();
    let first = match (saved, take) {
        (Some(local), Some(callee)) => vec![HirStmt::Let {
            local,
            init: HirExpr {
                kind: HirExprKind::Call {
                    callee,
                    args: Vec::new(),
                },
                ty: HirTy::Prim(PrimTy::Str),
                span,
            },
        }],
        _ if take.is_some_and(|t| calls(&handler, t)) => Vec::new(),
        _ => clear_site(ctx, span),
    };
    let result_ty = if hint == HirTy::Unknown {
        handler.ty.clone()
    } else {
        hint
    };
    let fit = |ctx: &mut LowerCtx<'_>, e: HirExpr, what: &'static str| {
        if result_ty == HirTy::Unit || hir_expr_diverges(&e) {
            e
        } else {
            coerce(ctx, &result_ty, e, &|| what.to_owned())
        }
    };
    let body_h = fit(ctx, body_h, "the `catch` body");
    let handler = fit(ctx, handler, "the `catch` handler");
    shell(ctx, &frame, body_h, result_ty, (first, Some(handler)), span)
}

/// Lower a `catch` arm no error reaches only for its diagnostics; result,
/// locals and raised errors are discarded.
pub(crate) fn check_dead_arm(
    ctx: &mut LowerCtx<'_>,
    arm: &nabic_ast::CatchArm,
    filter: Option<&HirTy>,
    hint: HirTy,
    span: nabic_diagnostic::Span,
) {
    let ety = filter.cloned().unwrap_or(HirTy::Unknown);
    let saved_frames = std::mem::take(&mut ctx.err_frames);
    let saved_calls = ctx.throwing_calls;
    let saved_locals = ctx.locals.len();
    catch_all_frame(ctx);
    ctx.push_scope();
    ctx.next_synth += 1;
    let name = format!("#dead{}", ctx.next_synth);
    let local = ctx.declare(None, ety.clone());
    bind_name(ctx, &name, local);
    let m = match &arm.pat.kind {
        // No type to match against: lower the body with the binding
        // poisoned (a `match` on an `Unknown` scrutinee checks nothing).
        AstPatternKind::Wildcard | AstPatternKind::Binding { sub: None, .. }
            if filter.is_none() =>
        {
            if let AstPatternKind::Binding { name, .. } = &arm.pat.kind {
                ctx.declare(Some(name.clone()), HirTy::Unknown);
            }
            Expr {
                kind: ExprKind::Block(arm.body.clone()),
                span: arm.body.span,
            }
        }
        _ => {
            let filters = [filter.cloned()];
            dispatch_one(ctx, std::slice::from_ref(arm), &filters, &ety, &name, span).0
        }
    };
    let h = lower_expected(ctx, &m, hint.clone());
    if !matches!(hint, HirTy::Unit | HirTy::Unknown) && !hir_expr_diverges(&h) {
        let _ = coerce(ctx, &hint, h, &|| "the `catch` handler".to_owned());
    }
    ctx.pop_scope();
    ctx.err_frames = saved_frames;
    ctx.throwing_calls = saved_calls;
    // Its locals (some typed `Unknown`) belong to the dropped HIR.
    ctx.locals.truncate(saved_locals);
}

pub(crate) fn bind_name(ctx: &mut LowerCtx<'_>, name: &str, local: LocalId) {
    if let Some(scope) = ctx.scopes.last_mut() {
        scope.insert(name.to_owned(), local);
    }
}

pub(crate) fn path_expr(name: &str, span: nabic_diagnostic::Span) -> Expr {
    Expr {
        kind: ExprKind::Path(AstPath {
            segments: vec![nabic_ast::PathSegment {
                name: name.to_owned(),
                generics: Vec::new(),
            }],
            span,
            origin: None,
        }),
        span,
    }
}

/// `match <err> { <arms that can take ety>, _ => throw <err> }`; the
/// rethrow arm is left out when the arms cover `ety` (`false`).
pub(crate) fn dispatch_one(
    ctx: &LowerCtx<'_>,
    arms: &[nabic_ast::CatchArm],
    filters: &[Option<HirTy>],
    ety: &HirTy,
    err_name: &str,
    span: nabic_diagnostic::Span,
) -> (Expr, bool) {
    let mut m_arms: Vec<AstMatchArm> = arms
        .iter()
        .zip(filters)
        .filter(|(_, f)| may_take(f.as_ref(), ety))
        .map(|(a, f)| {
            let mut pat = a.pat.clone();
            let mut body = a.body.clone();
            // `catch e: A | B` sees the error variant by variant: take
            // the variant whole and rebind it as the arm's sum type.
            if let (AstPatternKind::TypeCheck { binder, ty }, Some(HirTy::Sum(vs))) =
                (&a.pat.kind, f)
            {
                if vs.contains(ety) {
                    pat.kind = AstPatternKind::Wildcard;
                    if let Some(name) = binder {
                        body.stmts.insert(
                            0,
                            Stmt {
                                kind: StmtKind::Let {
                                    name: name.clone(),
                                    ty: Some(ty.clone()),
                                    init: path_expr(err_name, span),
                                },
                                span,
                            },
                        );
                    }
                }
            }
            AstMatchArm {
                pat,
                guard: None,
                body: Expr {
                    span: body.span,
                    kind: ExprKind::Block(body),
                },
            }
        })
        .collect();
    let passes = !covers(ctx, &m_arms, ety);
    if passes {
        m_arms.push(AstMatchArm {
            pat: AstPattern {
                kind: AstPatternKind::Wildcard,
                span,
            },
            guard: None,
            body: Expr {
                kind: ExprKind::Block(Block {
                    stmts: vec![Stmt {
                        kind: StmtKind::Throw(path_expr(err_name, span)),
                        span,
                    }],
                    trailing: None,
                    span,
                    label: None,
                }),
                span,
            },
        });
    }
    let m = Expr {
        kind: ExprKind::Match {
            scrutinee: Box::new(path_expr(err_name, span)),
            arms: m_arms,
        },
        span,
    };
    (m, passes)
}

/// Do `arms` cover every `ety`: a catch-all, its exact type, type patterns
/// over a sum's members, or every variant of an enum?
pub(crate) fn covers(ctx: &LowerCtx<'_>, arms: &[AstMatchArm], ety: &HirTy) -> bool {
    let irrefutable = |p: &AstPattern| {
        matches!(
            p.kind,
            AstPatternKind::Wildcard | AstPatternKind::Binding { sub: None, .. }
        )
    };
    let mut tys = Vec::new();
    let mut variants = Vec::new();
    for a in arms {
        match &a.pat.kind {
            _ if irrefutable(&a.pat) => return true,
            AstPatternKind::TypeCheck { ty, .. } => {
                let t = ctx.resolve_ast_ty(ty);
                if t == *ety || matches!(&t, HirTy::Sum(vs) if vs.contains(ety)) {
                    return true;
                }
                tys.push(t);
            }
            // `HttpError { code, .. }`: every value of that struct.
            AstPatternKind::Struct { fields, .. }
                if fields
                    .iter()
                    .all(|f| f.pat.as_ref().is_none_or(irrefutable)) =>
            {
                if let Some(t) = arm_filter(ctx, &a.pat) {
                    if t == *ety {
                        return true;
                    }
                    tys.push(t);
                }
            }
            AstPatternKind::EnumVariant { path, args } if args.iter().all(irrefutable) => {
                let segs: Vec<&str> = path.segments.iter().map(|s| s.name.as_str()).collect();
                if let EnumRes::Found(_, i) = resolve_enum_variant(ctx, &segs, Some(ety), None) {
                    variants.push(i);
                }
            }
            _ => {}
        }
    }
    match ety {
        HirTy::Sum(vs) => vs.iter().all(|v| tys.contains(v)),
        HirTy::Enum(eid) => (0..ctx.enum_def(*eid).variants.len()).all(|i| variants.contains(&i)),
        _ => false,
    }
}

/// The error type a catch arm's pattern names (`None`: any type).
pub(crate) fn arm_filter(ctx: &LowerCtx<'_>, pat: &AstPattern) -> Option<HirTy> {
    match &pat.kind {
        AstPatternKind::TypeCheck { ty, .. } => Some(ctx.resolve_ast_ty(ty)),
        AstPatternKind::EnumVariant { path, .. } | AstPatternKind::Struct { path, .. } => {
            let segs: Vec<&str> = path.segments.iter().map(|s| s.name.as_str()).collect();
            if let Some(e) = ctx
                .enum_path(&segs)
                .and_then(|(full, _)| ctx.lookup_enum(&full))
            {
                return Some(HirTy::Enum(e));
            }
            ctx.canon_type(&segs)
                .and_then(|full| ctx.lookup_struct(&full))
                .map(HirTy::Struct)
        }
        _ => None,
    }
}
