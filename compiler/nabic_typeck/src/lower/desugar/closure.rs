//! Closures lifted into anonymous functions with an `env` tuple.

#[allow(clippy::wildcard_imports)]
use crate::lower::desugar::*;

/// A closure or coroutine names a `for ref` binding: it has no value of
/// its own to capture.
pub(crate) fn place_capture(ctx: &LowerCtx<'_>, name: &str) {
    ctx.emit_error(
        Code::INVALID_CAPTURE,
        format!("a closure cannot capture `{name}`: it names an element of a `for ref` loop"),
    );
    ctx.help_last(Some(format!(
        "copy the element first and capture the copy: `let v = {name};`"
    )));
}

/// Lift a closure into an anonymous function and return its value.
/// Captures copy at creation; `[ref x]` shares `x`'s cell. A
/// non-capturing closure is a plain function pointer.
pub(crate) fn lower_closure_expr(
    ctx: &mut LowerCtx<'_>,
    capture_list: &[nabic_ast::CaptureSpec],
    params: &[Param],
    ret_ty: Option<&Ty>,
    body: &Expr,
    span: nabic_diagnostic::Span,
) -> HirExpr {
    // Names referenced but not bound inside the closure (exhaustive AST
    // walk), filtered to those that resolve to outer locals.
    let by_ref: Vec<&str> = capture_list
        .iter()
        .filter_map(|c| match c {
            nabic_ast::CaptureSpec::Ref(n) => Some(n.as_str()),
            nabic_ast::CaptureSpec::Value(_) | nabic_ast::CaptureSpec::Move(_) => None,
        })
        .collect();
    let moves: Vec<&str> = capture_list
        .iter()
        .filter_map(|c| match c {
            nabic_ast::CaptureSpec::Move(n) => Some(n.as_str()),
            _ => None,
        })
        .collect();
    for n in &moves {
        if by_ref.contains(n) {
            ctx.emit_error(
                Code::INVALID_CAPTURE,
                format!(
                    "`{n}` can't be both `[ref {n}]` and `[move {n}]`: keep `ref` to share it \
                     with this code, or `move` to give it to the closure"
                ),
            );
        } else if ctx.lookup(n).is_none() {
            ctx.emit_error(
                Code::INVALID_CAPTURE,
                format!("`[move {n}]` names no variable in scope"),
            );
        }
    }
    // Inside a narrowing of `x`, `[ref x]` shares the cell itself.
    let cell_of = |l: LocalId| {
        let mut l = l;
        while let (false, Some(&(orig, _))) = (ctx.is_boxed(l), ctx.narrowed.get(&l)) {
            l = orig;
        }
        l
    };
    for n in &by_ref {
        match ctx.lookup(n).map(cell_of) {
            Some(l) if ctx.is_boxed(l) => {}
            Some(_) => ctx.emit_error(
                Code::INVALID_CAPTURE,
                format!("`[ref {n}]` needs `{n}` to be a `let` variable or a parameter"),
            ),
            None => ctx.emit_error(
                Code::INVALID_CAPTURE,
                format!("`[ref {n}]` names no variable in scope"),
            ),
        }
    }
    let captures: Vec<Capture> = crate::walk::free_names(params, body)
        .into_iter()
        .filter_map(|name| {
            let mut local = ctx.lookup(&name)?;
            if ctx.places.contains_key(&local) {
                place_capture(ctx, &name);
            }
            let by_ref = by_ref.contains(&name.as_str()) && ctx.is_boxed(cell_of(local));
            if by_ref {
                local = cell_of(local);
            }
            let ty = if by_ref {
                ctx.local_ty(local)
            } else {
                ctx.var_ty(local)
            };
            Some((name, local, ty, by_ref))
        })
        .collect();
    for p in params.iter().filter(|p| p.by_ref) {
        ctx.emit_error(
            Code::INVALID_SIGNATURE,
            format!(
                "closure parameter `{}` cannot be `ref` - a `fn` type has no `ref` \
                 parameters",
                p.name
            ),
        );
    }

    let closure_id = FunctionId(ctx.env.sink.next_id.get());
    ctx.env.sink.next_id.set(closure_id.0 + 1);

    // With an expected function type (or a sum holding
    // exactly one), unannotated parameters and the return come from it.
    let expected = ctx.current_expected().and_then(|t| match t {
        HirTy::Sum(vs) => {
            let mut fns = vs
                .iter()
                .filter(|v| matches!(v, HirTy::Fn(..) | HirTy::Closure { .. }));
            match (fns.next(), fns.next()) {
                (Some(f), None) => Some(f),
                _ => None,
            }
        }
        t => Some(t),
    });
    let (expected_param_tys, mut expected_ret): (Option<Vec<HirTy>>, Option<HirTy>) = match expected
    {
        Some(HirTy::Fn(args, ret, _) | HirTy::Closure { args, ret, .. }) => {
            (Some(args.clone()), Some((**ret).clone()))
        }
        _ => (None, None),
    };
    // An expected `fn(..) throws (E) -> T`: the body's value is a `T`; an
    // unknown `E` (a `rethrows` parameter) is inferred like no `E`.
    let mut expected_err = None;
    if let Some(HirTy::Throws { ok, err }) = expected_ret.clone() {
        expected_ret = Some(*ok);
        expected_err = Some(*err).filter(|e| *e != HirTy::Unknown);
    }
    let declared_ret = ret_ty.map(|t| ctx.resolve_ast_ty(t));
    // `return` inside the closure targets the closure: its declared (or
    // expected) return type, else the first `return` value's type.
    let mut fixed_ret = declared_ret.or_else(|| expected_ret.filter(|t| *t != HirTy::Unknown));

    // A throwing closure needs its error and value types before the body
    // (they shape its error returns): the expected ones, else a probe
    // lowering.
    let mut throws = expected_err;
    if (throws.is_some() || may_throw(body)) && (throws.is_none() || fixed_ret.is_none()) {
        let mark = ctx.probe_mark();
        let (mut probe, ..) =
            closure_ctx(ctx, params, &captures, expected_param_tys.as_ref(), span);
        probe.fn_ret_ty = fixed_ret.clone().unwrap_or(HirTy::Unknown);
        probe.infer_ret = fixed_ret.is_none();
        probe.throws_probe = Some(Vec::new());
        let hint = probe.fn_ret_ty.clone();
        crate::lower::expr::guard_sink(&mut probe, body);
        let value = lower_expected(&mut probe, body, hint);
        ctx.rollback(&mark);
        let errs = probe.throws_probe.take().unwrap_or_default();
        if throws.is_none() {
            throws = match crate::ty::throws_ty(HirTy::Unit, HirTy::Sum(errs)) {
                HirTy::Throws { err, .. } => Some(*err),
                _ => None,
            };
        }
        if throws.is_some() && fixed_ret.is_none() {
            fixed_ret = Some(match probe.fn_ret_ty {
                HirTy::Unknown => value.ty,
                t => t,
            });
        }
    }

    let (mut cl_ctx, hir_params, prelude, user_arg_tys) =
        closure_ctx(ctx, params, &captures, expected_param_tys.as_ref(), span);
    cl_ctx.fn_ret_ty = fixed_ret.clone().unwrap_or(HirTy::Unknown);
    cl_ctx.infer_ret = fixed_ret.is_none();
    cl_ctx.fn_throws.clone_from(&throws);
    let ret_hint = cl_ctx.fn_ret_ty.clone();
    crate::lower::expr::guard_sink(&mut cl_ctx, body);
    let body_h = lower_expected(&mut cl_ctx, body, ret_hint);
    let diverges = hir_expr_diverges(&body_h);
    let final_ret_ty = match (fixed_ret, cl_ctx.fn_ret_ty.clone()) {
        // A `return` fixed the type; a value-producing body must agree.
        (None, HirTy::Unknown) => body_h.ty.clone(),
        (Some(t), _) | (None, t) => t,
    };
    let body_h = if diverges || (body_h.ty == HirTy::Unit && final_ret_ty == HirTy::Unit) {
        body_h
    } else {
        coerce(&mut cl_ctx, &final_ret_ty, body_h, &|| {
            "the closure body".to_owned()
        })
    };

    let mut body_block = HirBlock {
        stmts: prelude,
        trailing: Some(body_h),
        label: None,
    };
    cl_ctx.fn_ret_ty = final_ret_ty.clone();
    finish_throwing_body(&cl_ctx, &mut body_block);
    let final_ret_ty = match throws {
        Some(err) => HirTy::Throws {
            ok: Box::new(final_ret_ty),
            err: Box::new(err),
        },
        None => final_ret_ty,
    };
    let cl_fn = HirFunction {
        id: closure_id,
        def_id: None,
        name: format!("__closure_{}", closure_id.0),
        params: hir_params,
        locals: cl_ctx.into_locals(),
        ret_ty: final_ret_ty.clone(),
        body: Some(body_block),
        is_async: false,
        is_generator: false,
        span,
        facts: nabic_ir_common::FnFacts::default(),
    };
    ctx.env.sink.extras.borrow_mut().push(cl_fn);

    let env_ty = (!captures.is_empty())
        .then(|| HirTy::Tuple(captures.iter().map(|(_, _, t, _)| t.clone()).collect()));
    let Some(env_ty_v) = env_ty else {
        return HirExpr {
            kind: HirExprKind::FnRef(closure_id),
            // No captures: Send + Sync like a named function.
            ty: HirTy::Fn(
                user_arg_tys,
                Box::new(final_ret_ty),
                ctx.env.names.all_auto(),
            ),
            span,
        };
    };
    let mut moved = 0u64;
    for (i, (name, ..)) in captures.iter().enumerate() {
        if !moves.contains(&name.as_str()) {
            continue;
        }
        if i >= 64 {
            ctx.emit_error(
                Code::INVALID_CAPTURE,
                format!(
                    "`[move {name}]`: a closure can move only its first 64 captures; move \
                     fewer values, or gather them into a struct and move that"
                ),
            );
        } else {
            moved |= 1 << i;
        }
    }
    // The env: captured values snapshotted at the creation site; a
    // `[move x]` hands `x` over to the closure.
    let move_into = ctx.env.generic_fn(MOVE_INTO);
    let env_value = HirExpr {
        kind: HirExprKind::Tuple {
            fields: captures
                .iter()
                .enumerate()
                .map(|(i, (name, outer_local, ty, by_ref))| {
                    if *by_ref {
                        hir_local(*outer_local, ty, span)
                    } else if let Some(def) = move_into.filter(|_| i < 64 && moved & (1 << i) != 0)
                    {
                        let arg = CallArg::Positional(crate::lower::throws::path_expr(name, span));
                        crate::lower::expr::call_generic(
                            ctx,
                            def,
                            None,
                            None,
                            &[arg],
                            MOVE_INTO,
                            span,
                        )
                    } else {
                        ctx.read_local(*outer_local, span)
                    }
                })
                .collect(),
        },
        ty: env_ty_v.clone(),
        span,
    };
    HirExpr {
        kind: HirExprKind::ClosureValue {
            fn_id: closure_id,
            env: Box::new(env_value),
            arg_tys: user_arg_tys.clone(),
            ret_ty: final_ret_ty.clone(),
            env_ty: env_ty_v.clone(),
        },
        ty: HirTy::Closure {
            args: user_arg_tys,
            ret: Box::new(final_ret_ty),
            env: Box::new(env_ty_v),
            shares: captures.iter().any(|c| c.3),
            moved,
        },
        span,
    }
}

/// What a `[move x]` capture's field is made with: `x` handed over.
pub(crate) const MOVE_INTO: &str = "core::marker::move_into";

/// A closure capture: name, outer local, env field type (the cell for a
/// `[ref]` capture) and whether it is by reference.
pub(crate) type Capture = (String, LocalId, HirTy, bool);

/// Does a closure body `throw` or plain `try` (so its error type must be
/// inferred)? Nested closures count: an over-approximation costing a
/// probe.
pub(crate) fn may_throw(body: &Expr) -> bool {
    struct V(bool);
    impl crate::walk::AstVisitor for V {
        fn expr(&mut self, e: &Expr) {
            if let ExprKind::Try {
                kind: nabic_ast::TryKind::Plain,
                ..
            } = e.kind
            {
                self.0 = true;
            }
        }
        fn stmt(&mut self, s: &Stmt) {
            if matches!(s.kind, StmtKind::Throw(_)) {
                self.0 = true;
            }
        }
    }
    let mut v = V(false);
    crate::walk::walk_expr(&mut v, body);
    v.0
}

/// A closure body's context: the `env` tuple unpacked into one local per
/// capture, then the parameters. Returns (context, parameters, capture
/// prelude, visible argument types).
pub(crate) fn closure_ctx<'a>(
    ctx: &LowerCtx<'a>,
    params: &[Param],
    captures: &[Capture],
    expected_param_tys: Option<&Vec<HirTy>>,
    span: nabic_diagnostic::Span,
) -> (LowerCtx<'a>, Vec<HirParam>, Vec<HirStmt>, Vec<HirTy>) {
    let mut cl_ctx = LowerCtx::with_subst(
        ctx.env,
        ctx.ty_subst.clone(),
        ctx.self_ty.clone(),
        ctx.here(),
    );
    cl_ctx.current_module.clone_from(&ctx.current_module);
    cl_ctx.local_uses.clone_from(&ctx.local_uses);
    cl_ctx.nested_fns.clone_from(&ctx.nested_fns);
    cl_ctx.ref_captured.clone_from(&ctx.ref_captured);
    cl_ctx.sole_names.clone_from(&ctx.sole_names);
    cl_ctx.ty_bounds.clone_from(&ctx.ty_bounds);
    cl_ctx.ty_bound_args.clone_from(&ctx.ty_bound_args);
    cl_ctx.assoc_names.clone_from(&ctx.assoc_names);
    cl_ctx.unsafe_depth = ctx.unsafe_depth;
    // The body's `let`s infer on their own: an enclosing `let`'s inference
    // solves the closure's type, not the locals of its body (`[]` there is
    // reported, not left open).
    cl_ctx.infer_vars = false;

    // `env` is declared first so its LocalId is stable.
    let env_ty = (!captures.is_empty())
        .then(|| HirTy::Tuple(captures.iter().map(|(_, _, t, _)| t.clone()).collect()));
    let mut hir_params: Vec<HirParam> = Vec::with_capacity(params.len() + 1);
    let mut prelude: Vec<HirStmt> = Vec::new();
    if let Some(et) = &env_ty {
        let env_local = cl_ctx.declare(Some("__env".to_owned()), et.clone());
        hir_params.push(HirParam {
            name: "__env".to_owned(),
            local: env_local,
            ty: et.clone(),
            by_ref: false,
        });
        for (i, (cap_name, _, cap_ty, by_ref)) in captures.iter().enumerate() {
            let inner_local = cl_ctx.declare(Some(cap_name.clone()), cap_ty.clone());
            if *by_ref {
                cl_ctx.locals[inner_local.0 as usize].boxed = true;
            } else {
                cl_ctx.value_captured.insert(inner_local);
            }
            prelude.push(HirStmt::Let {
                local: inner_local,
                init: HirExpr {
                    kind: HirExprKind::TupleField {
                        target: Box::new(HirExpr {
                            kind: HirExprKind::Local(env_local),
                            ty: et.clone(),
                            span,
                        }),
                        index: u32::try_from(i).expect("capture index overflow"),
                        field_ty: cap_ty.clone(),
                    },
                    ty: cap_ty.clone(),
                    span,
                },
            });
        }
    }

    let mut user_arg_tys: Vec<HirTy> = Vec::with_capacity(params.len());
    let mut cell_prelude: Vec<HirStmt> = Vec::new();
    for (pi, p) in params.iter().enumerate() {
        let ty = match &p.ty {
            Some(t) => {
                let t = ctx.resolve_ast_ty(t);
                // `apply(2, |x: i32| x + 1)`: the annotation fixes the
                // slot's open literal.
                if let Some(e) = expected_param_tys.as_ref().and_then(|v| v.get(pi)) {
                    if crate::lower::infer::active(ctx) && crate::lower::infer::has_var(ctx, e) {
                        crate::lower::infer::unify(ctx, e, &t);
                    }
                }
                t
            }
            None => match expected_param_tys.as_ref().and_then(|v| v.get(pi)) {
                Some(t) if *t != HirTy::Unknown => t.clone(),
                _ if ctx.infer_vars => crate::lower::infer::fresh(ctx),
                _ => {
                    ctx.emit_error(
                        Code::TYPE_ANNOTATIONS_NEEDED,
                        format!(
                            "cannot infer the type of closure parameter `{}` - annotate \
                             it (`|{}: i32| ...`)",
                            p.name, p.name
                        ),
                    );
                    ctx.tag_last(nabic_diagnostic::Tag::ClosureParamInfer);
                    HirTy::Unknown
                }
            },
        };
        let local = cl_ctx.declare(Some(p.name.clone()), ty.clone());
        if cl_ctx.ref_captured.contains(&(p.name.clone(), p.span)) {
            let init = hir_local(local, &ty, span);
            cell_prelude.push(cl_ctx.cell_let(&p.name, ty.clone(), init));
        }
        hir_params.push(HirParam {
            name: p.name.clone(),
            local,
            ty: ty.clone(),
            by_ref: p.by_ref,
        });
        user_arg_tys.push(ty);
    }
    prelude.extend(cell_prelude);
    (cl_ctx, hir_params, prelude, user_arg_tys)
}
