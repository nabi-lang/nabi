//! `const { .. }` blocks and block `const` items that read the generic
//! parameters around them: evaluated once per instance, a failure is an
//! error at the call that made the instance.

#[allow(clippy::wildcard_imports)]
use crate::lower::consteval::*;

/// `const { e }`: `e` lowered here and evaluated now.
pub(crate) fn const_block(ctx: &mut LowerCtx<'_>, b: &nabic_ast::Block, span: Span) -> HirExpr {
    let e = Expr {
        kind: ExprKind::Block(b.clone()),
        span,
    };
    let free = crate::lower::expr::free_in(|f| crate::walk::walk_expr(f, &e));
    if let Some((name, at)) = free.values.iter().find(|(n, _)| ctx.lookup(n).is_some()) {
        ctx.emit_error_help(
            Code::OUTER_LOCAL,
            *at,
            format!("a `const` block can't use `{name}`, a run-time variable"),
            "a `const` block reads only constants and const parameters; for a value drop \
             the `const`: `{ .. }`"
                .to_owned(),
        );
        return poison(span);
    }
    let expected = ctx.current_expected().cloned().unwrap_or(HirTy::Unknown);
    let v = crate::lower::infer::scoped(ctx, |c| lower_expected(c, &e, expected));
    instance_value(ctx, "const block", v)
}

/// Does block `const` `c` read a generic parameter of its fn (`N / 2`,
/// `T::BYTES`)? Then each instance has its own value.
pub(crate) fn per_instance(ctx: &LowerCtx<'_>, c: &nabic_ast::ConstDef) -> bool {
    if ctx.ty_subst.is_empty() {
        return false;
    }
    let free = crate::lower::expr::free_in(|f| {
        crate::walk::walk_expr(f, &c.value);
        crate::walk::walk_ty(f, &c.ty);
    });
    let local = free.values.iter().any(|(n, _)| ctx.lookup(n).is_some());
    !local
        && free
            .types
            .iter()
            .chain(&free.values)
            .any(|(n, _)| ctx.ty_subst.contains_key(n))
}

/// Block `const` `c` of this instance: its value, seen by the block's
/// uses of its name.
pub(crate) fn instance_const(ctx: &mut LowerCtx<'_>, c: &nabic_ast::ConstDef) {
    let ty = ctx.resolve_ast_ty(&c.ty);
    let v = crate::lower::infer::scoped(ctx, |cx| {
        let v = lower_expected(cx, &c.value, ty.clone());
        coerce(cx, &ty, v, &|| {
            format!("const `{}`", nabic_core::local::leaf(&c.name))
        })
    });
    let what = format!("const `{}`", nabic_core::local::leaf(&c.name));
    let v = instance_value(ctx, &what, v);
    ctx.splices.insert(c.name.clone(), v);
}

/// Is const `key` declared in the body of a generic fn? It is checked
/// with that body, per instance.
pub(crate) fn in_generic_fn(env: &Env<'_>, key: &str) -> bool {
    if !key.contains('/') {
        return false;
    }
    let outer = nabic_core::local::outer(key);
    match outer.rsplit_once('.') {
        Some((ty, m)) => {
            env.generic_methods(ty, m).is_some()
                || env.defs.generic_struct(ty).is_some()
                || env.defs.generic_enum(ty).is_some()
        }
        None => env.generic_fn(outer).is_some(),
    }
}

/// Is a generic body being checked at its definition (parameters stand
/// in for every instance)? Its constants are left to the instances.
fn generic_check(ctx: &LowerCtx<'_>) -> bool {
    ctx.ty_subst.values().any(|t| match t {
        HirTy::Param(_) | HirTy::OpenConst(_) => true,
        HirTy::Const(v, _) => crate::lower::bounds::const_name(ctx.env, *v).is_some(),
        _ => false,
    })
}

/// `v` evaluated for this instance and embedded; a failure is reported
/// at the call that made the instance.
fn instance_value(ctx: &LowerCtx<'_>, what: &str, v: HirExpr) -> HirExpr {
    if generic_check(ctx) || crate::lower::collect::hir_expr_has_unknown(&v) {
        return v;
    }
    let (op, why, runtime) = match eval_in(ctx, &v) {
        Ok(val) => return embed(ctx, &val, &v.ty, v.span).unwrap_or(v),
        Err(Stop::Fail(op, why)) => {
            let why = why.strip_prefix("panicked: ").unwrap_or(&why);
            let why = why.strip_prefix("assertion failed: ").unwrap_or(why);
            (op, why.to_owned(), false)
        }
        Err(Stop::Runtime(op, why) | Stop::Limit(op, why)) => (
            op,
            format!("cannot be evaluated at compile time: {why}"),
            true,
        ),
        Err(_) => return v,
    };
    let help = |f: Option<&str>| {
        if runtime {
            "a `const` reads only constants, const parameters, `T::size()` of a plain \
             type and `const fn` calls; compute anything else at run time with `let`"
                .to_owned()
        } else if let Some(f) = f {
            format!(
                "every instance of `{}` must pass it: call it with other arguments, or \
                 change the check",
                shown(f)
            )
        } else {
            "it runs at compile time: change the values it reads".to_owned()
        }
    };
    let sites = ctx.env.sink.call_sites.borrow();
    let d = match (sites.first(), sites.last()) {
        (Some((_, at, _)), Some((inner, _, _))) => {
            let inst = instance_name(ctx, inner);
            nabic_diagnostic::NabiDiagnostic::error(
                Code::CONST_EVAL,
                *at,
                format!("{what} failed in `{inst}`: {why}"),
            )
            .with_secondary(op, "fails here")
            .with_help(help(Some(inner)))
        }
        _ => nabic_diagnostic::NabiDiagnostic::error(
            Code::CONST_EVAL,
            op,
            format!("{what} failed: {why}"),
        )
        .with_help(help(None)),
    };
    drop(sites);
    ctx.emit_diag(d);
    poison(v.span)
}

fn shown(f: &str) -> String {
    let leaf = leaf_name(f);
    leaf.split(nabic_ir_common::MONO_MARK)
        .next()
        .unwrap_or(leaf)
        .replace("__", "::")
}

/// `chunk::<0>`: the generic fn `f` with this instance's arguments.
fn instance_name(ctx: &LowerCtx<'_>, f: &str) -> String {
    let arg = |name: &str| {
        ctx.ty_subst.get(name).map(|t| match t {
            HirTy::Const(v, Some(PrimTy::Bool)) => (*v != 0).to_string(),
            HirTy::Const(v, Some(PrimTy::Char)) => u32::try_from(*v)
                .ok()
                .and_then(char::from_u32)
                .map_or_else(|| v.to_string(), |c| format!("'{c}'")),
            HirTy::Const(v, _) => v.to_string(),
            t => ctx.ty_name(t),
        })
    };
    let args: Vec<String> = if let Some(def) = ctx.env.generic_fn(f) {
        def.generics
            .params
            .iter()
            .filter_map(|gp| match gp {
                nabic_ast::GenericParam::Type { name, .. }
                | nabic_ast::GenericParam::Const { name, .. } => arg(name.as_str()),
            })
            .collect()
    } else {
        let mut names: Vec<&String> = ctx.ty_subst.keys().collect();
        names.sort();
        names.into_iter().filter_map(|n| arg(n)).collect()
    };
    if args.is_empty() {
        return shown(f);
    }
    format!("{}::<{}>", shown(f), args.join(", "))
}

/// What to write for a length that asks for a layout (`[u8; T::size()]`).
pub(crate) fn layout_help(ctx: &LowerCtx<'_>, e: &Expr) -> Option<String> {
    #[derive(Default)]
    struct Find(Option<String>);
    impl crate::walk::AstVisitor for Find {
        fn expr(&mut self, e: &Expr) {
            if let ExprKind::Call { callee, .. } = &e.kind {
                if let ExprKind::Path(p) = &callee.kind {
                    if let [head, m] = p.segments.as_slice() {
                        if matches!(m.name.as_str(), "size" | "align") {
                            self.0.get_or_insert_with(|| head.name.clone());
                        }
                    }
                }
            }
        }
    }
    let mut f = Find::default();
    crate::walk::walk_expr(&mut f, e);
    let t = f.0?;
    let concrete = match ctx.ty_subst.get(&t) {
        Some(HirTy::Param(_)) => None,
        Some(ty) => Some(ctx.ty_name(ty)),
        None => Some(t.clone()),
    };
    Some(match concrete {
        None => format!(
            "the size of `{t}` is known only per instance; use a const parameter \
             (`const N: usize`) or `List<u8>`"
        ),
        Some(ty) => format!(
            "the size of `{ty}` is known only once it is laid out; write the length \
             (`[u8; 8]`), or use a const parameter (`const N: usize`) or `List<u8>`"
        ),
    })
}
