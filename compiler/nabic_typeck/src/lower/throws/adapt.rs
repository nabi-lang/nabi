//! Function values adapted to the error type of their slot.

#[allow(clippy::wildcard_imports)]
use crate::lower::throws::*;

/// Does a captured function value of type `ty` meet `fn` slot bounds
/// `want` (a thunk capturing it is `Send` only if the callee is)?
pub(crate) fn value_meets(ctx: &LowerCtx<'_>, ty: &HirTy, want: &crate::FnBounds) -> bool {
    match ty {
        HirTy::Fn(_, _, have) => {
            want.auto.iter().all(|b| have.auto.contains(b)) && (!want.unsafe_fn || have.unsafe_fn)
        }
        t => !want.unsafe_fn && want.auto.iter().all(|b| ctx.has_trait(t, b)),
    }
}

/// A function value throwing less than its slot goes through
/// `fn #thunk(args) throws (E) -> T { try f(args) }`; a named fn is called
/// directly, other values captured.
pub(crate) fn adapt_fn_value(ctx: &LowerCtx<'_>, slot: &HirTy, value: HirExpr) -> HirExpr {
    let (
        HirTy::Fn(args, sret, want),
        HirTy::Fn(vargs, vret, _)
        | HirTy::Closure {
            args: vargs,
            ret: vret,
            ..
        },
    ) = (slot, &value.ty)
    else {
        return value;
    };
    // An `extern "C" fn` slot takes a C function as is (no thunk).
    if want.c_abi {
        return value;
    }
    let target = match &value.kind {
        HirExprKind::FnRef(t) => Some(*t),
        _ => None,
    };
    let HirTy::Throws { ok, err } = &**sret else {
        return value;
    };
    let fits = args == vargs
        && match &**vret {
            HirTy::Throws { ok: vok, err: verr } => {
                vok == ok && verr != err && err_fits(ctx, verr, err)
            }
            t => t == &**ok,
        };
    // The thunk captures a non-static callee: it is `Send` only if that
    // callee is.
    let bounds_ok = target.is_some() || value_meets(ctx, &value.ty, want);
    if !fits || !bounds_ok {
        return value;
    }
    let span = value.span;
    let callee_ty = HirTy::Fn(vargs.clone(), vret.clone(), want.clone());
    let env_ty = HirTy::Tuple(vec![callee_ty.clone()]);
    // One thunk per (callee, slot type): a named callee by id, a
    // captured one by its type.
    let key = thunk_key("throws", target, slot, &callee_ty);
    let cached = ctx.env.sink.thunks.borrow().get(&key).copied();
    crate::lower::incr::log(
        || format!("h|{key}"),
        || {
            let k =
                crate::lower::incr::Key::Thunk("throws", target, slot.clone(), callee_ty.clone());
            crate::lower::incr::Lookup::Thunk(k, cached)
        },
    );
    let id = cached.unwrap_or_else(|| {
        let id = FunctionId(ctx.env.sink.next_id.get());
        ctx.env.sink.next_id.set(id.0 + 1);
        let mut a = LowerCtx::with_subst(ctx.env, ctx.ty_subst.clone(), None, ctx.here());
        // The captured callee: `env.0` of a one-field env tuple.
        let env_param = target.is_none().then(|| HirParam {
            name: "#env".to_owned(),
            local: a.declare(None, env_ty.clone()),
            ty: env_ty.clone(),
            by_ref: false,
        });
        let params: Vec<HirParam> = args
            .iter()
            .enumerate()
            .map(|(i, t)| HirParam {
                name: format!("#a{i}"),
                local: a.declare(None, t.clone()),
                ty: t.clone(),
                by_ref: false,
            })
            .collect();
        let call_args: Vec<HirExpr> = params
            .iter()
            .map(|p| hir_local(p.local, &p.ty, span))
            .collect();
        let kind = match (target, &env_param) {
            (Some(t), _) => HirExprKind::Call {
                callee: t,
                args: call_args,
            },
            (None, Some(env)) => HirExprKind::CallIndirect {
                callee: Box::new(HirExpr {
                    kind: HirExprKind::TupleField {
                        target: Box::new(hir_local(env.local, &env_ty, span)),
                        index: 0,
                        field_ty: callee_ty.clone(),
                    },
                    ty: callee_ty.clone(),
                    span,
                }),
                args: call_args,
                ret_ty: (**vret).clone(),
            },
            (None, None) => unreachable!("env param exists without a static target"),
        };
        let call = HirExpr {
            kind,
            ty: (**vret).clone(),
            span,
        };
        a.fn_throws = Some((**err).clone());
        a.fn_ret_ty = (**ok).clone();
        a.try_depth = 1;
        let v = unpack_call(&mut a, call);
        let mut body = if no_ok_slot(ok) {
            HirBlock {
                stmts: vec![HirStmt::Expr(v)],
                trailing: None,
                label: None,
            }
        } else {
            HirBlock {
                stmts: Vec::new(),
                trailing: Some(v),
                label: None,
            }
        };
        finish_throwing_body(&a, &mut body);
        let mut all_params: Vec<HirParam> = env_param.into_iter().collect();
        all_params.extend(params);
        ctx.env.sink.extras.borrow_mut().push(HirFunction {
            id,
            def_id: None,
            name: format!("__thunk_{}", id.0),
            params: all_params,
            locals: a.into_locals(),
            ret_ty: (**sret).clone(),
            body: Some(body),
            is_async: false,
            is_generator: false,
            span,
            facts: nabic_ir_common::FnFacts::default(),
        });
        crate::lower::incr::touch(id);
        crate::lower::incr::touch_key(|| {
            crate::lower::incr::Key::Thunk("throws", target, slot.clone(), callee_ty.clone())
        });
        ctx.env.sink.thunks.borrow_mut().insert(key, id);
        id
    });
    let kind = if target.is_some() {
        HirExprKind::FnRef(id)
    } else {
        let mut v = value;
        v.ty = callee_ty;
        HirExprKind::ClosureValue {
            fn_id: id,
            env: Box::new(HirExpr {
                kind: HirExprKind::Tuple { fields: vec![v] },
                ty: env_ty.clone(),
                span,
            }),
            env_ty,
            arg_tys: args.clone(),
            ret_ty: (**sret).clone(),
        }
    };
    HirExpr {
        kind,
        ty: slot.clone(),
        span,
    }
}

/// `fn(..) -> !` fits a `fn(..) -> R` slot through a thunk
/// `fn #thunk(args) -> R { f(args) }` (a throwing one passes its error
/// on); a named fn is called directly, other values captured.
pub(crate) fn adapt_never_fn(ctx: &LowerCtx<'_>, slot: &HirTy, value: HirExpr) -> HirExpr {
    let (
        HirTy::Fn(args, sret, want),
        HirTy::Fn(vargs, vret, _)
        | HirTy::Closure {
            args: vargs,
            ret: vret,
            ..
        },
    ) = (slot, &value.ty)
    else {
        return value;
    };
    // An `extern "C" fn` slot takes a C function as is (no thunk).
    if want.c_abi {
        return value;
    }
    let split = |t: &HirTy| match t {
        HirTy::Throws { ok, err } => ((**ok).clone(), Some((**err).clone())),
        t => (t.clone(), None),
    };
    let ((v_ok, v_err), (s_ok, s_err)) = (split(vret), split(sret));
    if v_ok != HirTy::Never || s_ok == HirTy::Never || args != vargs || v_err != s_err {
        return value;
    }
    let target = match &value.kind {
        HirExprKind::FnRef(t) => Some(*t),
        _ => None,
    };
    let bounds_ok = target.is_some() || value_meets(ctx, &value.ty, want);
    if !bounds_ok {
        return value;
    }
    let span = value.span;
    let callee_ty = HirTy::Fn(vargs.clone(), vret.clone(), want.clone());
    let env_ty = HirTy::Tuple(vec![callee_ty.clone()]);
    // One thunk per (callee, slot type): a named callee by id, a
    // captured one by its type.
    let key = thunk_key("never", target, slot, &callee_ty);
    let cached = ctx.env.sink.thunks.borrow().get(&key).copied();
    crate::lower::incr::log(
        || format!("h|{key}"),
        || {
            let k =
                crate::lower::incr::Key::Thunk("never", target, slot.clone(), callee_ty.clone());
            crate::lower::incr::Lookup::Thunk(k, cached)
        },
    );
    let id = cached.unwrap_or_else(|| {
        let id = FunctionId(ctx.env.sink.next_id.get());
        ctx.env.sink.next_id.set(id.0 + 1);
        let mut a = LowerCtx::with_subst(ctx.env, ctx.ty_subst.clone(), None, ctx.here());
        let env_param = target.is_none().then(|| HirParam {
            name: "#env".to_owned(),
            local: a.declare(None, env_ty.clone()),
            ty: env_ty.clone(),
            by_ref: false,
        });
        let params: Vec<HirParam> = args
            .iter()
            .enumerate()
            .map(|(i, t)| HirParam {
                name: format!("#a{i}"),
                local: a.declare(None, t.clone()),
                ty: t.clone(),
                by_ref: false,
            })
            .collect();
        let call_args: Vec<HirExpr> = params
            .iter()
            .map(|p| hir_local(p.local, &p.ty, span))
            .collect();
        let kind = match (target, &env_param) {
            (Some(t), _) => HirExprKind::Call {
                callee: t,
                args: call_args,
            },
            (None, Some(env)) => HirExprKind::CallIndirect {
                callee: Box::new(HirExpr {
                    kind: HirExprKind::TupleField {
                        target: Box::new(hir_local(env.local, &env_ty, span)),
                        index: 0,
                        field_ty: callee_ty.clone(),
                    },
                    ty: callee_ty.clone(),
                    span,
                }),
                args: call_args,
                ret_ty: (**vret).clone(),
            },
            (None, None) => unreachable!("env param exists without a static target"),
        };
        let stmts = match &v_err {
            None => vec![HirStmt::Panic(HirExpr {
                kind,
                ty: HirTy::Unit,
                span,
            })],
            Some(e) => {
                let t = a.declare(None, (**vret).clone());
                let err = HirExpr {
                    kind: HirExprKind::TupleField {
                        target: Box::new(hir_local(t, vret, span)),
                        index: err_index(&HirTy::Never),
                        field_ty: e.clone(),
                    },
                    ty: e.clone(),
                    span,
                };
                vec![
                    HirStmt::Let {
                        local: t,
                        init: HirExpr {
                            kind,
                            ty: (**vret).clone(),
                            span,
                        },
                    },
                    HirStmt::Return(Some(carrier(&s_ok, e, Err(err), span))),
                ]
            }
        };
        let mut all_params: Vec<HirParam> = env_param.into_iter().collect();
        all_params.extend(params);
        let params = all_params;
        ctx.env.sink.extras.borrow_mut().push(HirFunction {
            id,
            def_id: None,
            name: format!("__thunk_{}", id.0),
            params,
            locals: a.into_locals(),
            ret_ty: (**sret).clone(),
            body: Some(HirBlock {
                stmts,
                trailing: None,
                label: None,
            }),
            is_async: false,
            is_generator: false,
            span,
            facts: nabic_ir_common::FnFacts::default(),
        });
        crate::lower::incr::touch(id);
        crate::lower::incr::touch_key(|| {
            crate::lower::incr::Key::Thunk("never", target, slot.clone(), callee_ty.clone())
        });
        ctx.env.sink.thunks.borrow_mut().insert(key, id);
        id
    });
    let kind = if target.is_some() {
        HirExprKind::FnRef(id)
    } else {
        let mut v = value;
        v.ty = callee_ty;
        HirExprKind::ClosureValue {
            fn_id: id,
            env: Box::new(HirExpr {
                kind: HirExprKind::Tuple { fields: vec![v] },
                ty: env_ty.clone(),
                span,
            }),
            env_ty,
            arg_tys: args.clone(),
            ret_ty: (**sret).clone(),
        }
    };
    HirExpr {
        kind,
        ty: slot.clone(),
        span,
    }
}

/// A `rethrows` function as a value: the instance the expected `fn` type
/// asks for, else the throwing one with unbound errors as `any Error`.
/// `None` for real type parameters.
pub(crate) fn rethrows_value(
    ctx: &LowerCtx<'_>,
    def: &FnDef,
    span: nabic_diagnostic::Span,
) -> Option<HirExpr> {
    let mut def = def.clone();
    for gp in &mut def.generics.params {
        match gp {
            nabic_ast::GenericParam::Type { name, default, .. } if name.starts_with('#') => {
                *default = Some(nabic_ast::untyped_throws_ty(span));
            }
            _ => return None,
        }
    }
    let args: Vec<HirExpr> = match ctx.current_expected() {
        Some(HirTy::Fn(ps, _, _)) if ps.len() == def.params.len() => {
            ps.iter().map(|t| undef(t, span)).collect()
        }
        _ => Vec::new(),
    };
    let (id, ret) = monomorphize(ctx, &def, None, &args, span)?;
    let params = ctx
        .env
        .sink
        .mono_sigs
        .borrow()
        .get(&id)
        .map(|s| s.param_tys.clone())
        .unwrap_or_default();
    Some(HirExpr {
        kind: HirExprKind::FnRef(id),
        ty: HirTy::Fn(params, Box::new(ret), ctx.env.names.all_auto()),
        span,
    })
}

/// The `thunks` key of a `kind` adapter of `target` (a captured callee:
/// `None`) into `slot`, fn bounds included: a thunk into a `+ Send`
/// slot and one into a plain slot type their env apart.
pub(crate) fn thunk_key(
    kind: &str,
    target: Option<FunctionId>,
    slot: &HirTy,
    callee: &HirTy,
) -> String {
    format!(
        "{kind}|{target:?}|{}|{}",
        instance_mangle(slot),
        instance_mangle(callee)
    )
}
