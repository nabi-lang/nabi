//! `for` loops over ranges, arrays, places, iterators and streams.

#[allow(clippy::wildcard_imports)]
use crate::lower::desugar::*;

/// `for pat in iter` over an integer range (bounds evaluated once) or an
/// array: `{ setup; while cond { step; let pat = ..; body } }`, stepping
/// first so `continue` can't skip it; `label` goes on the `While`.
pub(crate) fn lower_for(
    ctx: &mut LowerCtx<'_>,
    label: Option<&String>,
    pat: &AstPattern,
    iter: &Expr,
    body: &Block,
    (is_await, by_ref): (bool, bool),
    span: nabic_diagnostic::Span,
) -> HirExpr {
    let mut out: Vec<HirStmt> = Vec::new();
    // `try for`: the `try` covers the iterable and each step, not the body.
    if let Some(d) = ctx.try_for.take() {
        ctx.try_for_body = Some((body.span, d));
    }
    ctx.push_scope();
    let loop_idx = crate::lower::expr::open_unit_loop(ctx, label);
    let while_expr = match &iter.kind {
        _ if by_ref => lower_for_ref(ctx, label, pat, iter, body, is_await, span, &mut out),
        _ if is_await => {
            let s = lower_expected(ctx, iter, HirTy::Unknown);
            Some(lower_for_stream(ctx, label, pat, s, body, span, &mut out))
        }
        ExprKind::Range {
            start: Some(start_e),
            end,
            inclusive,
        } if end.is_some() || !*inclusive => lower_for_range(
            ctx,
            label,
            pat,
            iter,
            start_e,
            end.as_deref(),
            *inclusive,
            body,
            span,
            &mut out,
        ),
        ExprKind::Range { .. } => Some(ctx.error_expr(
            Code::NOT_ITERABLE,
            span,
            "`for` over an unbounded range needs both bounds (`a..b`)".to_owned(),
        )),
        _ => {
            let iter_h = lower_expected(ctx, iter, HirTy::Unknown);
            warn_resized_while_iterating(ctx, iter, &iter_h.ty, body);
            match iter_h.ty.clone() {
                HirTy::Array(elem, len) => Some(lower_for_array(
                    ctx,
                    label,
                    pat,
                    iter_h,
                    (*elem).clone(),
                    len,
                    body,
                    span,
                    &mut out,
                )),
                HirTy::Unknown => None,
                _ => {
                    // A generator the loop made (not one in a variable, which may go on
                    // after the loop) is closed when the loop leaves early.
                    let owned = !matches!(
                        iter.kind,
                        ExprKind::Path(_)
                            | ExprKind::QPath(_)
                            | ExprKind::SelfLower
                            | ExprKind::Field(..)
                            | ExprKind::TupleField(..)
                            | ExprKind::Index(..)
                    );
                    let w = lower_for_iter(ctx, label, pat, iter_h, body, span, &mut out, owned);
                    Some(w)
                }
            }
        }
    };
    crate::lower::expr::close_unit_loop(ctx, loop_idx);
    ctx.pop_scope();
    match while_expr {
        Some(w) => out.push(HirStmt::Expr(w)),
        None => return poison(span),
    }
    HirExpr {
        kind: HirExprKind::Block(Box::new(HirBlock {
            stmts: out,
            trailing: None,
            label: None,
        })),
        ty: HirTy::Unit,
        span,
    }
}

/// Methods that change a list's length; `for x in xs` reads `xs` as it
/// is at each step, so the loop sees the change.
pub(crate) const RESIZERS: &[&str] = &[
    "push", "pop", "insert", "remove", "clear", "truncate", "drain", "retain", "dedup", "extend",
    "append",
];

/// `for x in xs { xs.push(..) }`: the loop walks the list it grows (or
/// skips what a removal moved). A resize right before a `break`,
/// `return` or `throw` is fine; a body that binds `xs` again is left
/// alone.
pub(crate) fn warn_resized_while_iterating(
    ctx: &LowerCtx<'_>,
    iter: &Expr,
    ty: &HirTy,
    body: &Block,
) {
    let ExprKind::Path(p) = &iter.kind else {
        return;
    };
    let [seg] = p.segments.as_slice() else {
        return;
    };
    if !crate::lower::expr::is_list(ctx, ty) {
        return;
    }
    let mut f = Resizes {
        name: &seg.name,
        found: None,
        rebound: false,
    };
    crate::walk::walk_block(&mut f, body);
    let Some((at, method)) = f.found.filter(|_| !f.rebound) else {
        return;
    };
    let n = &seg.name;
    ctx.emit_lint(
        "resize_while_iterating",
        at,
        format!("`{n}.{method}` changes the length of `{n}` while this `for` walks it: the loop sees the change"),
        format!("walk a copy (`for x in {n}.clone()`), or collect the changes and apply them after the loop"),
    );
}

pub(crate) struct Resizes<'a> {
    name: &'a str,
    found: Option<(nabic_diagnostic::Span, String)>,
    rebound: bool,
}

impl crate::walk::AstVisitor for Resizes<'_> {
    fn bind(&mut self, name: &str) {
        self.rebound |= name == self.name;
    }

    fn block(&mut self, b: &Block) {
        if self.found.is_some() {
            return;
        }
        for (i, st) in b.stmts.iter().enumerate() {
            let StmtKind::Expr(e) = &st.kind else {
                continue;
            };
            let ExprKind::MethodCall {
                receiver, method, ..
            } = &e.kind
            else {
                continue;
            };
            let ExprKind::Path(r) = &receiver.kind else {
                continue;
            };
            if !matches!(r.segments.as_slice(), [s] if s.name == self.name)
                || !RESIZERS.contains(&method.as_str())
            {
                continue;
            }
            let leaves = b.stmts[i + 1..].iter().any(|s| {
                matches!(
                    s.kind,
                    StmtKind::Break { .. } | StmtKind::Return(_) | StmtKind::Throw(_)
                )
            });
            if !leaves {
                self.found = Some((e.span, method.clone()));
                return;
            }
        }
    }
}

/// `for ref x in xs`: `x` names the place `xs[i]` for each `i` in
/// `0..xs.len()`, so reads and writes of `x` reach the element. A list is
/// walked through a `SliceMut` view of it: resizing the list in the body is
/// an error where the body shows it and a panic where it does not. An
/// array is indexed where it lives, so it must be a variable or a field.
#[allow(clippy::too_many_arguments)]
pub(crate) fn lower_for_ref(
    ctx: &mut LowerCtx<'_>,
    label: Option<&String>,
    pat: &AstPattern,
    iter: &Expr,
    body: &Block,
    is_await: bool,
    span: nabic_diagnostic::Span,
    out: &mut Vec<HirStmt>,
) -> Option<HirExpr> {
    let fail = |ctx: &LowerCtx<'_>, code, at, msg: String, help: String| {
        ctx.emit_error_help(code, at, msg, help);
        Some(poison(span))
    };
    let AstPatternKind::Binding { name, sub: None } = &pat.kind else {
        return fail(
            ctx,
            Code::PATTERN_TYPE,
            pat.span,
            "`for ref` binds each element to one name".to_owned(),
            "bind the element, then destructure it in the body: \
             `for ref x in xs { let (a, b) = x; .. }`"
                .to_owned(),
        );
    };
    if is_await {
        return fail(
            ctx,
            Code::NOT_ITERABLE,
            pat.span,
            "a stream has no element places, so `for await` cannot be `for ref`".to_owned(),
            format!("write `for await {name} in ..`"),
        );
    }
    let iter_h = lower_expected(ctx, iter, HirTy::Unknown);
    let index = format!("#ri{}", ctx.locals.len());
    // A view's element accesses check nothing: each step checks the view.
    let mut step: Option<Expr> = None;
    let (elem_ty, list, len) = match iter_h.ty.clone() {
        HirTy::Unknown => return None,
        HirTy::Array(elem, n) => {
            if !crate::lower::mono::writable_place(ctx, &iter_h) {
                return fail(
                    ctx,
                    Code::INVALID_ASSIGNMENT,
                    iter.span,
                    "`for ref` writes an array where it is kept, and a temporary array is kept \
                     nowhere"
                        .to_owned(),
                    format!(
                        "keep the array in a variable first: `let xs = {}; for ref {name} in xs \
                         {{ .. }}`",
                        nabic_ast::print::print_expr(iter)
                    ),
                );
            }
            let len = crate::lower::expr::hir_int(i128::from(n), PrimTy::Usize, span);
            (*elem, iter.clone(), len)
        }
        ty if crate::lower::expr::is_list(ctx, &ty)
            || crate::lower::expr::is_slice_mut(ctx, &ty) =>
        {
            let view = if crate::lower::expr::is_list(ctx, &ty) {
                let xs = crate::lower::expr::splice(ctx, iter_h);
                let none = Expr {
                    kind: ExprKind::Literal(AstLit::None),
                    span,
                };
                let f = crate::lower::expr::index_fn("slice_mut", vec![xs, none], span);
                lower_expected(ctx, &f, HirTy::Unknown)
            } else {
                iter_h
            };
            let HirTy::Struct(sid) = view.ty else {
                return None;
            };
            let elem = ctx.struct_def(sid).generic_args.first().cloned()?;
            // `#for ref x`: what a resize error names.
            let v = format!("#for ref {name}");
            let local = ctx.declare(Some(v.clone()), view.ty.clone());
            let len = method_call_on(ctx, hir_local(local, &view.ty, span), "len", &[], span);
            out.push(HirStmt::Let { local, init: view });
            step = Some(crate::lower::expr::index_fn(
                "view_live",
                vec![crate::lower::throws::path_expr(&v, span)],
                span,
            ));
            (elem, crate::lower::throws::path_expr(&v, span), len)
        }
        ty => {
            return fail(
                ctx,
                Code::NOT_ITERABLE,
                iter.span,
                format!(
                    "`for ref` needs a `List`, an array or a `SliceMut`, whose elements are \
                     places; found `{}`",
                    ctx.ty_name(&ty)
                ),
                format!("to read the items, write `for {name} in ..`"),
            );
        }
    };
    let x = ctx.declare(Some(name.clone()), elem_ty);
    ctx.places.insert(
        x,
        Expr {
            kind: ExprKind::Index(
                Box::new(list),
                Box::new(crate::lower::throws::path_expr(&index, span)),
            ),
            span,
        },
    );
    let index_pat = AstPattern {
        kind: AstPatternKind::Binding {
            name: index,
            sub: None,
        },
        span,
    };
    let zero = Expr {
        kind: ExprKind::Literal(AstLit::Int("0".to_owned())),
        span,
    };
    let end = crate::lower::expr::splice(ctx, len);
    let range = Expr {
        kind: ExprKind::Range {
            start: Some(Box::new(zero.clone())),
            end: Some(Box::new(end.clone())),
            inclusive: false,
        },
        span,
    };
    let checked;
    let body = match step {
        Some(e) => {
            let mut b = body.clone();
            b.stmts.insert(
                0,
                nabic_ast::Stmt {
                    kind: nabic_ast::StmtKind::Expr(e),
                    span,
                },
            );
            checked = b;
            &checked
        }
        None => body,
    };
    let w = lower_for_range(
        ctx,
        label,
        &index_pat,
        &range,
        &zero,
        Some(&end),
        false,
        body,
        span,
        out,
    );
    ctx.places.remove(&x);
    w
}

/// `for pat in xs` otherwise: `let it = xs.into_iter();
/// loop { match it.next() { v: T => { let pat = v; body } none => break } }`.
/// A `TryIterator` in a variable or a field of one has no `into_iter`: it
/// is stepped in place (`xs.next()`), so the variable keeps its progress.
/// A generator the loop owns (`owned`) is closed after the loop and on
/// every exit leaving it: a `finally { it.close() }` that
/// `continue` and `break` of this loop skip.
#[allow(clippy::too_many_arguments)]
pub(crate) fn lower_for_iter(
    ctx: &mut LowerCtx<'_>,
    label: Option<&String>,
    pat: &AstPattern,
    iter_h: HirExpr,
    body: &Block,
    span: nabic_diagnostic::Span,
    out: &mut Vec<HirStmt>,
    owned: bool,
) -> HirExpr {
    let has_into_iter = !matches!(
        crate::lower::expr::pick_method(ctx, &iter_h.ty, "into_iter", None),
        Ok(None)
    );
    // A `TryIterator` is stepped directly; its `next` must sit under the
    // loop's `try` (`try for x in it { .. }`).
    let try_iter = ctx.lang("try_iterator_trait");
    let is_try = !has_into_iter && !try_iter.is_empty() && ctx.has_trait(&iter_h.ty, try_iter);
    if !has_into_iter && !is_try {
        let e = ctx.error_expr(
            Code::NOT_ITERABLE,
            span,
            format!(
                "cannot iterate over a value of type `{}` - `for` needs an integer range, \
                 an array, an `IntoIterator` / `Iterator` impl, or a `TryIterator` \
                 (`try for`)",
                ctx.ty_name(&iter_h.ty)
            ),
        );
        if iter_h.ty == HirTy::Prim(PrimTy::Str) {
            let s = ctx.src(iter_h.span).unwrap_or("s");
            ctx.help_last(Some(format!(
                "a `str` is iterated by what you want of it: `{s}.chars()`, `{s}.bytes()` or \
                 `{s}.lines()`"
            )));
        }
        return e;
    }
    if is_try && ctx.try_depth == 0 && iter_h.ty != HirTy::Unknown {
        let t = ctx.ty_name(&iter_h.ty);
        let mut d = nabic_diagnostic::NabiDiagnostic::error(
            Code::UNHANDLED_ERROR,
            span,
            format!("`next` of `{t}` can throw, so this loop must be `try for`"),
        )
        .with_help(
            "write `try for x in ..` (`try!` / `try?` to panic / stop on an error), \
             or loop over `<iterator>.results()` to handle each error as a `Result`"
                .to_owned(),
        );
        if label.is_none() {
            let at = nabic_diagnostic::Span {
                end: span.start,
                ..span
            };
            d = d.with_fix(at, "try ", "write `try for`");
        }
        ctx.emit_diag(d);
        return poison(span);
    }
    let it_h = if has_into_iter {
        method_call_on(ctx, iter_h, "into_iter", &[], span)
    } else {
        iter_h
    };
    if it_h.ty == HirTy::Unknown {
        return it_h;
    }
    let it_ty = it_h.ty.clone();
    // An iterator in a variable (or a field of one) stepped directly is
    // stepped in place, as a stream is polled.
    if !has_into_iter && !owned && crate::lower::mono::writable_place(ctx, &it_h) {
        let root = crate::lower::mono::place_root(&it_h);
        let next = method_call_on(ctx, it_h, "next", &[], span);
        let next = write_back_arm(ctx, root, next, span);
        return for_next_loop(ctx, label, pat, next, body, span);
    }
    let close = owned && is_generator(ctx, &it_ty);
    let name = close.then(|| format!("#for_gen{}", ctx.locals.len()));
    // `#for`: the iterator a loop steps (a view of a resource it is given).
    let it = ctx.declare(
        Some(
            name.clone()
                .unwrap_or_else(|| format!("#for{}", ctx.locals.len())),
        ),
        it_ty.clone(),
    );
    out.push(HirStmt::Let {
        local: it,
        init: it_h,
    });
    let next = method_call_on(ctx, hir_local(it, &it_ty, span), "next", &[], span);
    let Some(name) = name else {
        return for_next_loop(ctx, label, pat, next, body, span);
    };
    let call = Expr {
        kind: ExprKind::MethodCall {
            receiver: Box::new(Expr {
                kind: ExprKind::Path(nabic_ast::Path {
                    segments: vec![nabic_ast::PathSegment {
                        name,
                        generics: Vec::new(),
                    }],
                    span,
                    origin: None,
                }),
                span,
            }),
            method: "close".to_owned(),
            type_args: Vec::new(),
            args: Vec::new(),
            trailing_closure: None,
            // `close` is the library's, not the user's.
            origin: Some(GEN_MODULE.to_owned()),
            written: false,
        },
        span,
    };
    let fin = Block {
        stmts: vec![nabic_ast::Stmt {
            kind: nabic_ast::StmtKind::Expr(call),
            span,
        }],
        trailing: None,
        span,
        label: None,
    };
    // Outside this loop's breakable: its own `break` / `continue` do not
    // run it.
    ctx.err_frames.push(ErrFrame::Finally {
        block: fin,
        hir: Vec::new(),
        try_depth: ctx.try_depth,
        breakables: ctx.breakables.len() - 1,
    });
    let lp = for_next_loop(ctx, label, pat, next, body, span);
    ctx.err_frames.pop();
    let outer = ctx.set_access(Some(GEN_MODULE));
    let closed = method_call_on(ctx, hir_local(it, &it_ty, span), "close", &[], span);
    ctx.access.replace(outer);
    HirExpr {
        kind: HirExprKind::Block(Box::new(HirBlock {
            stmts: vec![HirStmt::Expr(lp), HirStmt::Expr(closed)],
            trailing: None,
            label: None,
        })),
        ty: HirTy::Unit,
        span,
    }
}

/// Where `GenIter` / `TryGenIter` and their `pub(pkg)` `close` live.
pub(crate) const GEN_MODULE: &str = "core::iter";

/// A `GenIter` / `TryGenIter`.
pub(crate) fn is_generator(ctx: &LowerCtx<'_>, ty: &HirTy) -> bool {
    let HirTy::Struct(sid) = ty else {
        return false;
    };
    let def = ctx.struct_def(*sid);
    ["core::iter::GenIter", "core::iter::TryGenIter"]
        .iter()
        .any(|base| def.name == mangle_inst(base, &def.generic_args))
}

/// `for await pat in s`: `s.into_stream()` unless already a `Stream`,
/// then loop awaiting `poll_next(ref ctx)`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn lower_for_stream(
    ctx: &mut LowerCtx<'_>,
    label: Option<&String>,
    pat: &AstPattern,
    s: HirExpr,
    body: &Block,
    span: nabic_diagnostic::Span,
    out: &mut Vec<HirStmt>,
) -> HirExpr {
    if s.ty == HirTy::Unknown {
        return s;
    }
    let is_stream = matches!(
        crate::lower::expr::pick_method(ctx, &s.ty, "poll_next", None),
        Ok(Some(_))
    );
    let s = if is_stream {
        s
    } else {
        method_call_on(ctx, s, "into_stream", &[], span)
    };
    // A stream in a variable (or a field of one) is polled in place: a
    // copy would leave the variable's owner with the state before the loop.
    if is_stream && crate::lower::mono::writable_place(ctx, &s) {
        let root = crate::lower::mono::place_root(&s);
        let next = crate::lower::coro::poll_loop_on(ctx, s, "poll_next", span);
        let next = write_back_arm(ctx, root, next, span);
        return for_next_loop(ctx, label, pat, next, body, span);
    }
    let ty = s.ty.clone();
    // `#for`: the owner of a temporary stream.
    let st = ctx.declare(Some(format!("#for{}", ctx.locals.len())), ty.clone());
    out.push(HirStmt::Let { local: st, init: s });
    let next = crate::lower::coro::poll_loop(ctx, st, &ty, "poll_next", span);
    for_next_loop(ctx, label, pat, next, body, span)
}

/// The label of a loop's write-back of a `match` arm's binding.
pub(crate) const ARM_BACK: &str = "#arm_back";

/// `next`, then, when the stepped variable `root` is a `match` arm's
/// binding of a place, that place takes its new state: the binding names
/// the place, not a copy of it.
pub(crate) fn write_back_arm(
    ctx: &mut LowerCtx<'_>,
    root: Option<LocalId>,
    next: HirExpr,
    span: nabic_diagnostic::Span,
) -> HirExpr {
    let mut stmts = Vec::new();
    let mut cur = root;
    for _ in 0..8 {
        let Some((l, place)) = cur.and_then(|l| Some((l, ctx.arm_bound.get(&l)?.clone()))) else {
            break;
        };
        let ty = ctx.local_ty(l);
        let back = coerce(ctx, &place.ty, hir_local(l, &ty, span), &|| {
            "the matched place".to_owned()
        });
        // Marked for the checks: the place and its binding are one value,
        // so nothing moves here.
        let back = HirExpr {
            ty: back.ty.clone(),
            span,
            kind: HirExprKind::Block(Box::new(HirBlock {
                stmts: Vec::new(),
                trailing: Some(back),
                label: Some(ARM_BACK.to_owned()),
            })),
        };
        let Some(st) = crate::lower::mono::assign_place(ctx, &place, back) else {
            break;
        };
        stmts.push(st);
        cur = crate::lower::mono::place_root(&place);
    }
    if stmts.is_empty() || next.ty == HirTy::Unknown {
        return next;
    }
    let ty = next.ty.clone();
    let v = ctx.declare(None, ty.clone());
    stmts.insert(
        0,
        HirStmt::Let {
            local: v,
            init: next,
        },
    );
    HirExpr {
        kind: HirExprKind::Block(Box::new(HirBlock {
            stmts,
            trailing: Some(hir_local(v, &ty, span)),
            label: None,
        })),
        ty,
        span,
    }
}

/// `loop { match <next> { v: T => { let pat = v; body } none => break } }`
/// for `next` of type `T | none`.
pub(crate) fn for_next_loop(
    ctx: &mut LowerCtx<'_>,
    label: Option<&String>,
    pat: &AstPattern,
    next: HirExpr,
    body: &Block,
    span: nabic_diagnostic::Span,
) -> HirExpr {
    let sum_ty = next.ty.clone();
    let parts = match &sum_ty {
        HirTy::Sum(parts) => parts.clone(),
        HirTy::Unknown => return next,
        other => {
            return ctx.error_expr(
                Code::NOT_ITERABLE,
                span,
                format!(
                    "`next` of an iterator must return `T | none`, found `{}`",
                    ctx.ty_name(other)
                ),
            );
        }
    };
    let (Some(none_i), Some(item_i)) = (
        parts.iter().position(HirTy::is_none),
        parts.iter().position(|p| !p.is_none()),
    ) else {
        return ctx.error_expr(
            Code::NOT_ITERABLE,
            span,
            "`next` must return `T | none`".to_owned(),
        );
    };
    if parts.len() > 2 {
        return for_next_loop_sum(ctx, label, pat, next, &parts, none_i, body, span);
    }
    let item_ty = parts[item_i].clone();
    let v = ctx.declare(None, item_ty.clone());
    let mut arm_stmts: Vec<HirStmt> = Vec::new();
    bind_irrefutable(ctx, pat, hir_local(v, &item_ty, span), &mut arm_stmts);
    let lowered_body = lower_loop_body(ctx, body);
    arm_stmts.push(HirStmt::Expr(HirExpr {
        kind: HirExprKind::Block(Box::new(lowered_body)),
        ty: HirTy::Unit,
        span,
    }));
    let block = |stmts: Vec<HirStmt>| HirExpr {
        kind: HirExprKind::Block(Box::new(HirBlock {
            stmts,
            trailing: None,
            label: None,
        })),
        ty: HirTy::Unit,
        span,
    };
    let arms = vec![
        HirMatchArm {
            pat: HirPattern::SumVariant {
                sum_ty: sum_ty.clone(),
                variant_index: u32::try_from(item_i).expect("variant overflow"),
                binder: Some(v),
                payload_ty: Some(item_ty),
            },
            guard: None,
            body: block(arm_stmts),
        },
        HirMatchArm {
            pat: HirPattern::SumVariant {
                sum_ty,
                variant_index: u32::try_from(none_i).expect("variant overflow"),
                binder: None,
                payload_ty: None,
            },
            guard: None,
            body: block(vec![HirStmt::Break {
                label: label.cloned(),
                value: None,
            }]),
        },
    ];
    let step = HirExpr {
        kind: HirExprKind::Match {
            scrutinee: Box::new(next),
            arms,
        },
        ty: HirTy::Unit,
        span,
    };
    HirExpr {
        kind: HirExprKind::Loop {
            label: label.cloned(),
            body: Box::new(HirBlock {
                stmts: vec![HirStmt::Expr(step)],
                trailing: None,
                label: None,
            }),
            result_ty: HirTy::Unit,
        },
        ty: HirTy::Unit,
        span,
    }
}

/// `for_next_loop` for a sum item (`next` is `A | B | none`): match it into
/// `item: A | B` or `break`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn for_next_loop_sum(
    ctx: &mut LowerCtx<'_>,
    label: Option<&String>,
    pat: &AstPattern,
    next: HirExpr,
    parts: &[HirTy],
    none_i: usize,
    body: &Block,
    span: nabic_diagnostic::Span,
) -> HirExpr {
    let sum_ty = next.ty.clone();
    let item_ty = HirTy::sum(
        parts
            .iter()
            .enumerate()
            .filter(|&(i, _)| i != none_i)
            .map(|(_, p)| p.clone()),
    );
    let mut arms = Vec::new();
    for (i, p) in parts.iter().enumerate() {
        let index = u32::try_from(i).expect("variant overflow");
        if i == none_i {
            arms.push(HirMatchArm {
                pat: HirPattern::SumVariant {
                    sum_ty: sum_ty.clone(),
                    variant_index: index,
                    binder: None,
                    payload_ty: None,
                },
                guard: None,
                body: HirExpr {
                    kind: HirExprKind::Block(Box::new(HirBlock {
                        stmts: vec![HirStmt::Break {
                            label: label.cloned(),
                            value: None,
                        }],
                        trailing: None,
                        label: None,
                    })),
                    ty: HirTy::Never,
                    span,
                },
            });
            continue;
        }
        let v = ctx.declare(None, p.clone());
        let value = coerce(ctx, &item_ty, hir_local(v, p, span), &|| {
            "the loop item".to_owned()
        });
        arms.push(HirMatchArm {
            pat: HirPattern::SumVariant {
                sum_ty: sum_ty.clone(),
                variant_index: index,
                binder: Some(v),
                payload_ty: Some(p.clone()),
            },
            guard: None,
            body: value,
        });
    }
    let item = ctx.declare(None, item_ty.clone());
    let mut stmts = vec![HirStmt::Let {
        local: item,
        init: HirExpr {
            kind: HirExprKind::Match {
                scrutinee: Box::new(next),
                arms,
            },
            ty: item_ty.clone(),
            span,
        },
    }];
    bind_irrefutable(ctx, pat, hir_local(item, &item_ty, span), &mut stmts);
    let lowered_body = lower_loop_body(ctx, body);
    stmts.push(HirStmt::Expr(HirExpr {
        kind: HirExprKind::Block(Box::new(lowered_body)),
        ty: HirTy::Unit,
        span,
    }));
    HirExpr {
        kind: HirExprKind::Loop {
            label: label.cloned(),
            body: Box::new(HirBlock {
                stmts,
                trailing: None,
                label: None,
            }),
            result_ty: HirTy::Unit,
        },
        ty: HirTy::Unit,
        span,
    }
}

/// Integer range loop. Inclusive ranges use a `go` flag so `a..=MAX` ends;
/// other bounds (a user `Step`) iterate `range` like any iterable.
#[allow(clippy::too_many_arguments)]
pub(crate) fn lower_for_range(
    ctx: &mut LowerCtx<'_>,
    label: Option<&String>,
    pat: &AstPattern,
    range: &Expr,
    start_e: &Expr,
    end_e: Option<&Expr>,
    inclusive: bool,
    body: &Block,
    span: nabic_diagnostic::Span,
    out: &mut Vec<HirStmt>,
) -> Option<HirExpr> {
    // `a..` counts up until a `break` (or overflow panics).
    let (start_h, end_h) = match end_e {
        Some(end_e) => {
            let (s, e) = lower_sibling_pair(ctx, start_e, end_e, HirTy::Unknown);
            (s, Some(e))
        }
        None => (lower_expected(ctx, start_e, HirTy::Unknown), None),
    };
    if start_h.ty == HirTy::Unknown || end_h.as_ref().is_some_and(|e| e.ty == HirTy::Unknown) {
        return None;
    }
    let elem_ty = start_h.ty.clone();
    if !matches!(elem_ty, HirTy::Prim(p) if p.is_int()) {
        let iter_h = lower_expected(ctx, range, HirTy::Unknown);
        return Some(lower_for_iter(
            ctx, label, pat, iter_h, body, span, out, false,
        ));
    }
    let prim = match &elem_ty {
        HirTy::Prim(p) if p.is_int() && end_h.as_ref().is_none_or(|e| e.ty == start_h.ty) => *p,
        _ => {
            return Some(ctx.error_expr(
                Code::MISMATCHED_TYPES,
                span,
                format!(
                    "range bounds must be integers of the same type, found `{}` and `{}`",
                    ctx.ty_name(&start_h.ty),
                    ctx.ty_name(end_h.as_ref().map_or(&start_h.ty, |e| &e.ty))
                ),
            ));
        }
    };
    let bool_ty = HirTy::Prim(PrimTy::Bool);
    let counter = ctx.declare(None, elem_ty.clone());
    let end = ctx.declare(None, elem_ty.clone());
    out.push(HirStmt::Let {
        local: counter,
        init: start_h,
    });
    let unbounded = end_h.is_none();
    if let Some(end_h) = end_h {
        out.push(HirStmt::Let {
            local: end,
            init: end_h,
        });
    }
    let c = || hir_local(counter, &elem_ty, span);
    let e = || hir_local(end, &elem_ty, span);
    let inc = |c: HirExpr| {
        hir_binop(
            PrimOp::Add,
            c,
            hir_int(1, prim, span),
            elem_ty.clone(),
            span,
        )
    };

    let mut body_stmts: Vec<HirStmt> = Vec::new();
    let snap = ctx.declare(None, elem_ty.clone());
    body_stmts.push(HirStmt::Let {
        local: snap,
        init: c(),
    });
    let cond = if inclusive {
        // go = start <= end; while go { snap = c; go = c < end; if go { c += 1 } ... }
        let go = ctx.declare(None, bool_ty.clone());
        out.push(HirStmt::Let {
            local: go,
            init: hir_binop(PrimOp::Le, c(), e(), bool_ty.clone(), span),
        });
        body_stmts.push(HirStmt::Assign {
            local: go,
            value: hir_binop(PrimOp::Lt, c(), e(), bool_ty.clone(), span),
        });
        body_stmts.push(HirStmt::Expr(HirExpr {
            kind: HirExprKind::If {
                cond: Box::new(hir_local(go, &bool_ty, span)),
                then_block: Box::new(HirBlock {
                    stmts: vec![HirStmt::Assign {
                        local: counter,
                        value: inc(c()),
                    }],
                    trailing: None,
                    label: None,
                }),
                else_branch: None,
            },
            ty: HirTy::Unit,
            span,
        }));
        hir_local(go, &bool_ty, span)
    } else {
        body_stmts.push(HirStmt::Assign {
            local: counter,
            value: inc(c()),
        });
        if unbounded {
            HirExpr {
                kind: HirExprKind::Const(ConstVal::Bool(true)),
                ty: bool_ty,
                span,
            }
        } else {
            hir_binop(PrimOp::Lt, c(), e(), bool_ty, span)
        }
    };
    bind_irrefutable(ctx, pat, hir_local(snap, &elem_ty, span), &mut body_stmts);
    let lowered_body = lower_loop_body(ctx, body);
    body_stmts.push(HirStmt::Expr(HirExpr {
        kind: HirExprKind::Block(Box::new(lowered_body)),
        ty: HirTy::Unit,
        span,
    }));
    Some(HirExpr {
        ty: HirTy::Unit,
        span,
        kind: HirExprKind::While {
            label: label.cloned(),
            cond: Box::new(cond),
            body: Box::new(HirBlock {
                stmts: body_stmts,
                trailing: None,
                label: None,
            }),
        },
    })
}

/// `for x in arr` over a `[T; N]` value: the array is bound to a local
/// once, then indexed by a counter.
#[allow(clippy::too_many_arguments)]
pub(crate) fn lower_for_array(
    ctx: &mut LowerCtx<'_>,
    label: Option<&String>,
    pat: &AstPattern,
    iter_h: HirExpr,
    elem_ty: HirTy,
    len: u32,
    body: &Block,
    span: nabic_diagnostic::Span,
    out: &mut Vec<HirStmt>,
) -> HirExpr {
    let arr_ty = iter_h.ty.clone();
    let arr_local = if let HirExprKind::Local(l) = iter_h.kind {
        l
    } else {
        let l = ctx.declare(None, arr_ty.clone());
        out.push(HirStmt::Let {
            local: l,
            init: iter_h,
        });
        l
    };
    let idx_prim = PrimTy::I32;
    let idx_ty = HirTy::Prim(idx_prim);
    let counter = ctx.declare(None, idx_ty.clone());
    out.push(HirStmt::Let {
        local: counter,
        init: hir_int(0, idx_prim, span),
    });
    let cond = hir_binop(
        PrimOp::Lt,
        hir_local(counter, &idx_ty, span),
        hir_int(i128::from(len), idx_prim, span),
        HirTy::Prim(PrimTy::Bool),
        span,
    );
    let mut body_stmts: Vec<HirStmt> = Vec::new();
    let snap = ctx.declare(None, idx_ty.clone());
    body_stmts.push(HirStmt::Let {
        local: snap,
        init: hir_local(counter, &idx_ty, span),
    });
    body_stmts.push(HirStmt::Assign {
        local: counter,
        value: hir_binop(
            PrimOp::Add,
            hir_local(counter, &idx_ty, span),
            hir_int(1, idx_prim, span),
            idx_ty.clone(),
            span,
        ),
    });
    let elem_read = HirExpr {
        ty: elem_ty.clone(),
        span,
        kind: HirExprKind::ArrayIndex {
            array: Box::new(hir_local(arr_local, &arr_ty, span)),
            index: Box::new(hir_local(snap, &idx_ty, span)),
            elem_ty,
        },
    };
    bind_irrefutable(ctx, pat, elem_read, &mut body_stmts);
    let lowered_body = lower_loop_body(ctx, body);
    body_stmts.push(HirStmt::Expr(HirExpr {
        kind: HirExprKind::Block(Box::new(lowered_body)),
        ty: HirTy::Unit,
        span,
    }));
    HirExpr {
        ty: HirTy::Unit,
        span,
        kind: HirExprKind::While {
            label: label.cloned(),
            cond: Box::new(cond),
            body: Box::new(HirBlock {
                stmts: body_stmts,
                trailing: None,
                label: None,
            }),
        },
    }
}
