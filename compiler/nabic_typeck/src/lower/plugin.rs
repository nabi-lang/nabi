//! Plugins: the keys of the `any` tags, a plugin's exports, the `any Tr`
//! dispatchers other modules reach, and the intrinsics behind
//! `Plugin::get`.

use crate::lower::expr::discard_args;
use crate::lower::reflect::desc::key_of;
#[allow(clippy::wildcard_imports)]
use crate::lower::*;
use crate::plugin::{Dispatcher, Export, Info, Mode};
use nabic_diagnostic::Code;

/// The module's plugin tables; empty unless it is a plugin or reaches
/// `std::plugin`.
pub(crate) fn info(
    env: &Env<'_>,
    plugin: bool,
    lib: Option<&str>,
    functions: &[HirFunction],
) -> Info {
    let extras = env.sink.extras.borrow();
    let all = || functions.iter().chain(extras.iter());
    // A program (not a library) that calls into `std::plugin`.
    let hosts = || all().any(|f| f.body.is_some() && f.name.starts_with("std::plugin::"));
    let mode = if plugin {
        Mode::Plugin
    } else if lib.is_none() && hosts() {
        Mode::Host
    } else {
        return Info::default();
    };
    let empty = HashMap::default();
    let ctx = LowerCtx::new(env, &empty, None, nabic_diagnostic::Span::DUMMY);
    let tag_keys = env
        .sink
        .any_tags
        .borrow()
        .iter()
        .map(|(t, _)| key_of(&ctx, t))
        .collect();
    let dispatchers = env
        .sink
        .any_dispatch
        .borrow()
        .iter()
        .map(|d| Dispatcher {
            id: d.id,
            method: format!("{}.{}", d.trait_key, d.method),
            tags: all()
                .find(|f| f.id == d.id)
                .map(arm_tags)
                .unwrap_or_default(),
        })
        .collect();
    let mut exports = Vec::new();
    if let (Mode::Plugin, Some(lib)) = (mode, lib) {
        let under = format!("{lib}::");
        for f in functions {
            let Some(MethodOrFn::Fn(def)) = env.decls.get(f.id.0 as usize) else {
                continue;
            };
            if def.vis != nabic_core::Visibility::Pub
                || !def.generics.params.is_empty()
                || f.body.is_none()
                || !f.name.starts_with(&under)
                || f.facts.export_name.is_some()
            {
                continue;
            }
            let params: Vec<HirTy> = f.params.iter().map(|p| p.ty.clone()).collect();
            let fn_ty = HirTy::Fn(
                params,
                Box::new(f.ret_ty.clone()),
                crate::FnBounds::default(),
            );
            exports.push(Export {
                id: f.id,
                path: f.name.clone(),
                sig: key_of(&ctx, &fn_ty),
                shown: ctx.ty_name(&fn_ty),
            });
        }
    }
    Info {
        hot: None,
        mode,
        tag_keys,
        exports,
        dispatchers,
    }
}

/// The tags a dispatcher's arms take.
fn arm_tags(f: &HirFunction) -> Vec<u32> {
    let Some(body) = &f.body else {
        return Vec::new();
    };
    let m = body.trailing.as_ref().or_else(|| match body.stmts.first() {
        Some(HirStmt::Expr(e)) => Some(e),
        _ => None,
    });
    let Some(HirExprKind::Match { arms, .. }) = m.map(|e| &e.kind) else {
        return Vec::new();
    };
    arms.iter()
        .filter_map(|a| match &a.pat {
            HirPattern::SumVariant { variant_index, .. } => Some(*variant_index),
            _ => None,
        })
        .collect()
}

/// `nabi.plugin_get::<F>(code)`: function value `{code, null}` of type
/// `F`; `nabi.plugin_sig::<F>()`: the hash of F's key (`u64`).
pub(crate) fn intrinsic(
    ctx: &mut LowerCtx<'_>,
    op: crate::lower::collect::Intrinsic,
    name: &str,
    targs: &[Ty],
    args: &[CallArg],
    span: nabic_diagnostic::Span,
) -> HirExpr {
    let Some(t) = targs.first().filter(|_| targs.len() == 1) else {
        discard_args(ctx, args);
        return ctx.error_expr(
            Code::BUILTIN_CALL,
            span,
            format!("`{name}` is called as `{name}::<F>(..)`"),
        );
    };
    let f = ctx.resolve_ast_ty(t);
    // A generic body checked before instantiation: `F` is still open.
    let open = matches!(
        f,
        HirTy::Param(_) | HirTy::Path(_) | HirTy::Var(_) | HirTy::Unknown | HirTy::Any
    );
    if open && op == crate::lower::collect::Intrinsic::PluginSig {
        discard_args(ctx, args);
        return HirExpr {
            kind: HirExprKind::Const(nabic_ir_common::ConstVal::UInt {
                value: 0,
                ty: PrimTy::U64,
            }),
            ty: HirTy::Prim(PrimTy::U64),
            span,
        };
    }
    if !open && (!matches!(f, HirTy::Fn(..)) || f.is_c_fn()) {
        discard_args(ctx, args);
        let shown = crate::lower::reflect::type_name(ctx, &f);
        return ctx.error_expr(
            Code::BUILTIN_CALL,
            span,
            format!(
                "`Plugin::get` returns a function, and `{shown}` is not a function type; write \
                 the function's type, as `p.get::<fn(i32) -> str>(\"enemies::name\")`, or read a \
                 static with `p.get_static::<{shown}>(\"enemies::COUNT\")`"
            ),
        );
    }
    if op == crate::lower::collect::Intrinsic::PluginSig {
        discard_args(ctx, args);
        let key = key_of(ctx, &f);
        return HirExpr {
            kind: HirExprKind::StaticRef(crate::plugin::sig_static(&key)),
            ty: HirTy::Prim(PrimTy::U64),
            span,
        };
    }
    let [CallArg::Positional(arg)] = args else {
        discard_args(ctx, args);
        return ctx.error_expr(
            Code::BUILTIN_CALL,
            span,
            format!("`{name}` takes the code address"),
        );
    };
    let usize_ty = HirTy::Prim(PrimTy::Usize);
    let code = lower_expected(ctx, arg, usize_ty.clone());
    let code = coerce(ctx, &usize_ty, code, &|| format!("the value of `{name}`"));
    HirExpr {
        kind: HirExprKind::Cast {
            operand: Box::new(code),
            from_ty: usize_ty,
            to_ty: f.clone(),
        },
        ty: f,
        span,
    }
}
