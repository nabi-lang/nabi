//! Calls: signatures, paths, values and spawn.

#[allow(clippy::wildcard_imports)]
use crate::lower::expr::*;

/// C default argument promotions past a variadic declaration's
/// parameters: narrow integers and `bool` to `i32`, `f32` to `f64`.
/// Aggregates are refused.
pub(crate) fn c_vararg_promote(ctx: &LowerCtx<'_>, callee: &str, a: HirExpr) -> HirExpr {
    let to = match &a.ty {
        HirTy::Prim(
            PrimTy::I8 | PrimTy::I16 | PrimTy::U8 | PrimTy::U16 | PrimTy::CChar | PrimTy::Bool,
        ) => PrimTy::I32,
        HirTy::Prim(PrimTy::F32) => PrimTy::F64,
        HirTy::Prim(_) | HirTy::Fn(..) | HirTy::Unknown => return a,
        other => {
            let name = nabic_diagnostic::article(&ctx.ty_name(other));
            let help = if crate::lower::half::half_of(ctx, other).is_some() {
                "pass `x.to_f64()`: C reads a `double`"
            } else {
                "pass a pointer to it, or its fields one by one"
            };
            ctx.emit_error_help(
                Code::FFI,
                a.span,
                format!(
                    "`{callee}`: {name} cannot be passed as a C variadic `...` argument \
                     (only numbers, `bool`, `char`, `str` and C function pointers)"
                ),
                help.to_owned(),
            );
            return a;
        }
    };
    let span = a.span;
    let from_ty = a.ty.clone();
    HirExpr {
        kind: HirExprKind::Cast {
            operand: Box::new(a),
            from_ty,
            to_ty: HirTy::Prim(to),
        },
        ty: HirTy::Prim(to),
        span,
    }
}

/// A path call's first argument standing for `self` (`T::m(ref x)`): `ref`
/// exactly when `self` is. `explicit`: `None` for a method call.
pub(crate) fn check_path_receiver(
    ctx: &LowerCtx<'_>,
    explicit: Option<bool>,
    by_ref: bool,
    arg: &HirExpr,
    pname: &str,
    callee: &str,
) {
    match explicit {
        Some(false) if by_ref => ctx.emit_error(
            Code::REF_ARGUMENT,
            match &arg.kind {
                HirExprKind::Local(l) => format!(
                    "parameter `{pname}` of `{callee}` is `ref` - pass `ref {}` at the call site",
                    ctx.locals
                        .get(l.0 as usize)
                        .and_then(|x| x.name.clone())
                        .unwrap_or_else(|| "x".to_owned())
                ),
                HirExprKind::FieldRead { .. } if crate::lower::mono::place_root(arg).is_some() => {
                    format!(
                        "parameter `{pname}` of `{callee}` is `ref` - pass `ref {}` at the call \
                         site",
                        ctx.src(arg.span).unwrap_or("x")
                    )
                }
                _ => format!(
                    "parameter `{pname}` of `{callee}` is `ref`, so it changes a variable - bind \
                     the value first, `let v = ..;`, and pass `ref v`"
                ),
            },
        ),
        Some(true) if !by_ref => ctx.emit_error(
            Code::REF_ARGUMENT,
            format!(
                "argument `{pname}` of `{callee}` is passed with `ref`, but the parameter is \
                 not `ref`"
            ),
        ),
        Some(true) if crate::lower::mono::place_root(arg).is_none() && arg.ty != HirTy::Unknown => {
            ctx.emit_error(
                Code::REF_ARGUMENT,
                format!(
                    "`ref` argument `{pname}` of `{callee}` must name a variable or a field of \
                     one"
                ),
            );
        }
        _ => {}
    }
}

/// Field names from the root of a place (`p.a.b` -> `[a, b]`).
pub(crate) fn field_path(ctx: &LowerCtx<'_>, e: &HirExpr) -> Vec<String> {
    match &e.kind {
        HirExprKind::FieldRead {
            target,
            struct_id,
            field_index,
            ..
        } => {
            let mut v = field_path(ctx, target);
            let def = ctx.struct_def(*struct_id);
            v.extend(
                def.fields
                    .get(*field_index as usize)
                    .map(|f| f.name.clone()),
            );
            v
        }
        _ => Vec::new(),
    }
}

/// A place passed `ref` may not be passed `ref` again or read by another
/// argument of the same call through an overlapping path, when visible
/// syntactically.
pub(crate) fn check_ref_exclusive(
    ctx: &LowerCtx<'_>,
    callee: &str,
    args: &[HirExpr],
    places: &[Option<HirExpr>],
    reads: &[(String, Vec<String>)],
    has_receiver: bool,
) {
    let refs: Vec<(LocalId, Vec<String>)> = places
        .iter()
        .flatten()
        .filter_map(|p| Some((crate::lower::mono::place_root(p)?, field_path(ctx, p))))
        .collect();
    let name = |l: LocalId| {
        ctx.locals
            .get(l.0 as usize)
            .and_then(|x| x.name.clone())
            .unwrap_or_default()
    };
    let overlaps = |p: &[String], q: &[String]| {
        let n = p.len().min(q.len());
        p[..n] == q[..n]
    };
    let shown = |l: LocalId, path: &[String]| {
        std::iter::once(name(l))
            .chain(path.iter().cloned())
            .collect::<Vec<_>>()
            .join(".")
    };
    for (k, (a, path)) in refs.iter().enumerate() {
        if let Some((_, q)) = refs[..k].iter().find(|(b, q)| b == a && overlaps(path, q)) {
            let what = if q == path {
                format!("`{}` is passed as `ref` twice", shown(*a, path))
            } else {
                format!(
                    "`{}` and `{}` overlap and are both passed as `ref`",
                    shown(*a, q),
                    shown(*a, path)
                )
            };
            ctx.emit_error(
                Code::REF_ARGUMENT,
                format!("{what} in one call to `{callee}`"),
            );
            return;
        }
    }
    // A by-value receiver is read by the call too.
    let receiver_read = if has_receiver && places.first().is_some_and(Option::is_none) {
        args.first()
            .and_then(|r| Some((crate::lower::mono::place_root(r)?, field_path(ctx, r))))
    } else {
        None
    };
    for (root, path) in &refs {
        let n = name(*root);
        let read = receiver_read
            .as_ref()
            .is_some_and(|(r, q)| r == root && overlaps(path, q))
            || reads
                .iter()
                .any(|(x, q)| *x == n && ctx.lookup(x) == Some(*root) && overlaps(path, q));
        if read {
            ctx.emit_error(
                Code::REF_ARGUMENT,
                format!(
                    "`{}` is passed as `ref` to `{callee}` and also read by another argument \
                     of the same call",
                    shown(*root, path)
                ),
            );
        }
    }
}

/// `Call` to a statically known function with checked arguments.
pub(crate) fn call_sig(
    ctx: &mut LowerCtx<'_>,
    sig: &FnSig,
    callee: &str,
    receiver: Option<HirExpr>,
    args: &[CallArg],
    span: nabic_diagnostic::Span,
) -> HirExpr {
    if ctx.env.is(crate::lower::FnSet::Unsafe, sig.id) {
        check_unsafe_call(ctx, callee);
    }
    fn_marked(ctx, sig.id, span);
    // By span: reading the declaration would tie the caller's incremental
    // unit to the callee's body.
    let decl = ctx.env.decls.get(sig.id.0 as usize);
    if decl.is_some_and(|d| ctx.env.duplicated(d.def().span)) {
        discard_args(ctx, args);
        return poison(span);
    }
    let (lowered, lets, places) = lower_args_ref(ctx, sig, callee, receiver, args, span);
    let out = ctx.env.sink.ref_out.borrow().get(&sig.id).cloned();
    if let Some(mut out) = out {
        // The caller's view of a named `some` result.
        if ctx.env.sink.opaque_check.get() {
            out.ret.clone_from(&sig.ret_ty);
        }
        let tys: Vec<HirTy> = out.refs.iter().map(|(_, t)| t.clone()).collect();
        // `nabi.OP_assign.TYPE`: the op's result is the new `ref self`.
        let kind = match (ctx.env.intrinsic(sig.id), <[HirExpr; 2]>::try_from(lowered)) {
            (Some(&Intrinsic::Prim(op)), Ok([lhs, rhs])) => HirExprKind::PrimBinOp {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            },
            (_, Ok(two)) => HirExprKind::Call {
                callee: sig.id,
                args: two.into(),
            },
            (_, Err(args)) => HirExprKind::Call {
                callee: sig.id,
                args,
            },
        };
        let call = HirExpr {
            kind,
            ty: crate::lower::mono::ref_packed(&out.ret, &tys),
            span,
        };
        let v = crate::lower::mono::ref_unpack(ctx, call, &out, &places);
        return with_lets(lets, v);
    }
    // A C function's `fn` parameter is a bare code pointer (an `extern "nabi"`
    // one takes the whole `{code, env}`).
    if ctx.env.is(crate::lower::FnSet::ExternC, sig.id) {
        for a in &lowered {
            // An `extern "C" fn` value is already a C function pointer.
            if matches!(a.ty, HirTy::Fn(..) | HirTy::Closure { .. })
                && !a.ty.is_c_fn()
                && !matches!(a.kind, HirExprKind::FnRef(_))
            {
                ctx.emit_error_help(
                    Code::FFI,
                    a.span,
                    format!(
                        "`{callee}` is a C function: a `fn` argument must name a function \
                         (a C callback has no room for a closure's captures)"
                    ),
                    "a C callback cannot capture: keep the state in `Handle::new(..)` \
                     (`bare::ffi`), pass `h.as_ptr()` as the `void*` argument, and read it back \
                     in an `extern \"C\" fn` with `Handle::<T>::from_ptr(p).get()`"
                        .to_owned(),
                );
            }
        }
    }
    // An intrinsic method's call is its primitive op.
    let kind = match (ctx.env.intrinsic(sig.id), <[HirExpr; 2]>::try_from(lowered)) {
        (Some(&Intrinsic::Prim(op)), Ok([lhs, rhs])) => HirExprKind::PrimBinOp {
            op,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        },
        (Some(&Intrinsic::Convert), Err(mut one)) if one.len() == 1 => {
            let operand = one.remove(0);
            if operand.ty == sig.ret_ty {
                return with_lets(lets, operand);
            }
            HirExprKind::Cast {
                from_ty: operand.ty.clone(),
                to_ty: sig.ret_ty.clone(),
                operand: Box::new(operand),
            }
        }
        (Some(&Intrinsic::Prim(op)), Err(mut one)) if one.len() == 1 => {
            let operand = one.remove(0);
            let op = if op == PrimOp::Not && operand.ty == HirTy::Prim(PrimTy::Bool) {
                PrimOp::LogicalNot
            } else {
                op
            };
            HirExprKind::PrimUnaryOp {
                op,
                operand: Box::new(operand),
            }
        }
        (_, Ok(two)) => HirExprKind::Call {
            callee: sig.id,
            args: two.into(),
        },
        (_, Err(args)) => HirExprKind::Call {
            callee: sig.id,
            args,
        },
    };
    check_const_overflow(ctx, &kind, &sig.ret_ty, span);
    with_lets(
        lets,
        HirExpr {
            kind,
            ty: sig.ret_ty.clone(),
            span,
        },
    )
}

/// The type of a value an `if` / `match` / block arm leaves (lowered in
/// statement position, the value is otherwise silently dropped).
pub(crate) fn arm_value_ty(e: &HirExpr) -> Option<HirTy> {
    let block = |b: &HirBlock| b.trailing.as_ref().and_then(arm_value_ty);
    match &e.kind {
        HirExprKind::Block(b) => block(b),
        HirExprKind::If {
            then_block,
            else_branch,
            ..
        } => block(then_block).or_else(|| else_branch.as_deref().and_then(arm_value_ty)),
        HirExprKind::Match { arms, .. } => arms.iter().find_map(|a| arm_value_ty(&a.body)),
        _ if matches!(e.ty, HirTy::Unit | HirTy::Never | HirTy::Unknown) => None,
        _ => Some(e.ty.clone()),
    }
}

/// The value of integer arithmetic over literals (`2 * 200`), if it is
/// one; `None` past an overflow (reported where it happened), and for a
/// `const` parameter of the generic body being checked (`stand_in`):
/// its stand-in value is not the instance's (`M - N`).
pub(crate) fn const_int(e: &HirExpr, stand_in: &dyn Fn(i128) -> bool) -> Option<i128> {
    let HirTy::Prim(p) = e.ty else {
        return None;
    };
    if !p.is_int() {
        return None;
    }
    let v = match &e.kind {
        HirExprKind::Const(ConstVal::Int { value, .. }) if stand_in(*value) => return None,
        HirExprKind::Const(ConstVal::Int { value, .. }) => *value,
        HirExprKind::Const(ConstVal::UInt { value, .. }) => i128::try_from(*value).ok()?,
        HirExprKind::PrimUnaryOp {
            op: PrimOp::Neg,
            operand,
        } => const_int(operand, stand_in)?.checked_neg()?,
        HirExprKind::PrimBinOp { op, lhs, rhs } => {
            // The right side first: a left-leaning chain's is shallow.
            let b = const_int(rhs, stand_in)?;
            const_arith(*op, const_int(lhs, stand_in)?, b)?
        }
        _ => return None,
    };
    crate::lower::pattern::int_fits(v, p).then_some(v)
}

/// The value of a condition over literals and constants (`N == 0` in
/// an instance), if it is one.
pub(crate) fn const_cond(ctx: &LowerCtx<'_>, e: &HirExpr) -> Option<bool> {
    let stand_in = |v: i128| crate::lower::bounds::const_name(ctx.env, v).is_some();
    const_bool(e, &stand_in)
}

fn const_bool(e: &HirExpr, stand_in: &dyn Fn(i128) -> bool) -> Option<bool> {
    match &e.kind {
        HirExprKind::Const(ConstVal::Bool(b)) => Some(*b),
        HirExprKind::PrimUnaryOp {
            op: PrimOp::LogicalNot | PrimOp::Not,
            operand,
        } if operand.ty == HirTy::Prim(PrimTy::Bool) => const_bool(operand, stand_in).map(|b| !b),
        // `a && b` and `a || b` lower to an `if`.
        HirExprKind::If {
            cond,
            then_block,
            else_branch: Some(e),
        } if then_block.stmts.is_empty() => {
            if const_bool(cond, stand_in)? {
                const_bool(then_block.trailing.as_ref()?, stand_in)
            } else {
                const_bool(e, stand_in)
            }
        }
        HirExprKind::Block(b) if b.stmts.is_empty() => const_bool(b.trailing.as_ref()?, stand_in),
        HirExprKind::PrimBinOp {
            op: PrimOp::LogicalAnd,
            lhs,
            rhs,
        } => Some(const_bool(lhs, stand_in)? && const_bool(rhs, stand_in)?),
        HirExprKind::PrimBinOp {
            op: PrimOp::LogicalOr,
            lhs,
            rhs,
        } => Some(const_bool(lhs, stand_in)? || const_bool(rhs, stand_in)?),
        HirExprKind::PrimBinOp { op, lhs, rhs } => {
            let (a, b) = (const_int(lhs, stand_in)?, const_int(rhs, stand_in)?);
            match op {
                PrimOp::Eq => Some(a == b),
                PrimOp::Ne => Some(a != b),
                PrimOp::Lt => Some(a < b),
                PrimOp::Le => Some(a <= b),
                PrimOp::Gt => Some(a > b),
                PrimOp::Ge => Some(a >= b),
                _ => None,
            }
        }
        _ => None,
    }
}

pub(crate) const fn const_arith(op: PrimOp, a: i128, b: i128) -> Option<i128> {
    match op {
        PrimOp::Add => a.checked_add(b),
        PrimOp::Sub => a.checked_sub(b),
        PrimOp::Mul => a.checked_mul(b),
        PrimOp::Div => a.checked_div(b),
        PrimOp::Rem => a.checked_rem(b),
        PrimOp::Mod => match a.checked_rem(b) {
            Some(r) if r != 0 && (r < 0) != (b < 0) => Some(r + b),
            r => r,
        },
        _ => None,
    }
}

/// `let m: u8 = 2 * 200;`: integer arithmetic folded from literals that
/// leaves its type (or divides by zero) is a compile error.
pub(crate) fn check_const_overflow(
    ctx: &LowerCtx<'_>,
    kind: &HirExprKind,
    ty: &HirTy,
    span: nabic_diagnostic::Span,
) {
    let HirTy::Prim(p) = *ty else {
        return;
    };
    if ctx.dead > 0 {
        return;
    }
    let (op, lhs, rhs) = match kind {
        HirExprKind::PrimBinOp { op, lhs, rhs } => (*op, lhs, rhs),
        _ => return,
    };
    if !p.is_int()
        || !matches!(
            op,
            PrimOp::Add | PrimOp::Sub | PrimOp::Mul | PrimOp::Div | PrimOp::Rem | PrimOp::Mod
        )
    {
        return;
    }
    let stand_in = |v: i128| crate::lower::bounds::const_name(ctx.env, v).is_some();
    let Some(b) = const_int(rhs, &stand_in) else {
        return;
    };
    let Some(a) = const_int(lhs, &stand_in) else {
        return;
    };
    let msg = match const_arith(op, a, b) {
        None if b == 0 => format!("constant expression divides by zero (`{a}` by `0`)"),
        Some(v) if crate::lower::pattern::int_fits(v, p) => return,
        Some(v) => format!(
            "constant expression overflows `{}`: the result is {v}",
            p.name()
        ),
        None => format!("constant expression overflows `{}`", p.name()),
    };
    let diag = nabic_diagnostic::NabiDiagnostic::error(Code::CONST_EVAL, span, msg);
    ctx.emit_diag(instance_site(ctx, diag));
}

/// In an instance the operands may be its const arguments: `diag` names
/// them and the call that made the instance, which is where the user
/// can look.
fn instance_site(
    ctx: &LowerCtx<'_>,
    mut diag: nabic_diagnostic::NabiDiagnostic,
) -> nabic_diagnostic::NabiDiagnostic {
    let mut consts: Vec<(&String, i128)> = ctx
        .ty_subst
        .iter()
        .filter_map(|(name, t)| match t {
            HirTy::Const(v, _) => Some((name, *v)),
            _ => None,
        })
        .collect();
    consts.sort();
    let sites = ctx.env.sink.call_sites.borrow();
    let (Some((outer, at, _)), Some((inner, _, _))) = (sites.first(), sites.last()) else {
        return diag;
    };
    let shown = |f: &str| leaf_name(f).replace("__", "::");
    let with = consts
        .iter()
        .map(|(name, v)| format!("`{name} = {v}`"))
        .collect::<Vec<_>>()
        .join(", ");
    let with = if with.is_empty() {
        with
    } else {
        format!(" with {with}")
    };
    let label = if outer == inner {
        format!("`{}` is instantiated{with} by this call", shown(inner))
    } else {
        format!(
            "`{}` is instantiated{with} through this call of `{}`",
            shown(inner),
            shown(outer)
        )
    };
    diag = diag.with_secondary(*at, label);
    if let Some((name, _)) = consts.first() {
        diag = diag.with_help(format!(
            "a branch whose constant condition is false is not checked: write \
             `if {name} > 0 {{ .. }}` around it, or use `{name}.saturating_sub(..)`"
        ));
    }
    diag
}

/// `{ lets; e }`: temporaries a spread argument introduced, so the spread
/// tuple is evaluated once.
pub(crate) fn with_lets(lets: Vec<HirStmt>, e: HirExpr) -> HirExpr {
    if lets.is_empty() {
        return e;
    }
    let (ty, span) = (e.ty.clone(), e.span);
    HirExpr {
        kind: HirExprKind::Block(Box::new(HirBlock {
            stmts: lets,
            trailing: Some(e),
            label: None,
        })),
        ty,
        span,
    }
}

/// Bind explicit `::<..>` arguments by position to `def`'s parameters not
/// fixed by `self_ty`. Too many is an error; too few leave the rest to
/// inference.
pub(crate) fn bind_explicit(
    ctx: &LowerCtx<'_>,
    def: &FnDef,
    self_ty: Option<&Ty>,
    targs: &[Ty],
    callee: &str,
) -> Result<HashMap<String, HirTy>, String> {
    let own: Vec<&String> = def
        .generics
        .params
        .iter()
        .map(|gp| {
            let (nabic_ast::GenericParam::Type { name, .. }
            | nabic_ast::GenericParam::Const { name, .. }) = gp;
            name
        })
        // A bound template's trait parameters come from the bound.
        .skip(
            ctx.env
                .bound_trait(&def.name)
                .and_then(|k| ctx.env.trait_of(k))
                .map_or(0, |t| t.generics.params.len()),
        )
        .filter(|n| !self_ty.is_some_and(|t| ast_ty_mentions(t, n)))
        .filter(|n| !n.starts_with(nabic_ast::SOME_PARAM_PREFIX))
        .collect();
    if targs.len() > own.len() {
        let msg =
            crate::lower::mono::arity_msg(leaf_name(callee), own.len(), own.len(), targs.len());
        return Err(match some_params_note(def) {
            Some(note) => format!("{msg}; {note}"),
            None => msg,
        });
    }
    Ok(own
        .into_iter()
        .zip(targs)
        .map(|(n, t)| (n.clone(), turbofish_ty(ctx, t)))
        .collect())
}

/// What to write when `::<..>` names more types than `def` has names
/// for: its `some` parameters take their types from the arguments.
pub(crate) fn some_params_note(def: &FnDef) -> Option<String> {
    let some: Vec<String> = def
        .params
        .iter()
        .filter(|p| {
            p.ty.as_ref().is_some_and(|t| {
                matches!(&t.kind, nabic_ast::TyKind::Path(path)
                    if path.segments.len() == 1
                        && path.segments[0].name.starts_with(nabic_ast::SOME_PARAM_PREFIX))
            })
        })
        .map(|p| format!("`{}`", p.name))
        .collect();
    (!some.is_empty()).then(|| {
        format!(
            "the type of a `some` parameter ({}) can't be written at the call: it is the \
             argument's own type; pass the argument, or declare a named parameter `<T: Trait>` \
             to choose `T` with `::<..>`",
            some.join(", ")
        )
    })
}

/// Every type argument of generic fn `def` inferred from the expected
/// `fn(..) -> R` slot of its value, or `None`.
pub(crate) fn generic_fn_args_from_slot(
    ctx: &LowerCtx<'_>,
    def: &FnDef,
) -> Option<HashMap<String, HirTy>> {
    let Some(HirTy::Fn(ptys, ret, _)) = ctx.current_expected() else {
        return None;
    };
    if ptys.len() != def.params.len() {
        return None;
    }
    let names: nabic_core::HashSet<String> = def
        .generics
        .params
        .iter()
        .map(|gp| match gp {
            nabic_ast::GenericParam::Type { name, .. }
            | nabic_ast::GenericParam::Const { name, .. } => name.clone(),
        })
        .collect();
    let mut subst = HashMap::default();
    for (p, t) in def.params.iter().zip(ptys) {
        crate::lower::mono::unify_param_ty_with(p.ty.as_ref()?, t, &names, &mut subst, ctx);
    }
    if let Some(r) = &def.ret_ty {
        crate::lower::mono::unify_param_ty_with(r, ret, &names, &mut subst, ctx);
    }
    (subst.len() == names.len()).then_some(subst)
}

/// Does AST type `ty` name `name` (a type parameter)?
pub fn ast_ty_mentions(ty: &Ty, name: &str) -> bool {
    let rec = |t: &Ty| ast_ty_mentions(t, name);
    match &ty.kind {
        AstTyKind::Path(p) => p
            .segments
            .iter()
            .any(|s| s.name == name || s.generics.iter().any(rec)),
        AstTyKind::Sum(ts) | AstTyKind::Tuple(ts) => ts.iter().any(rec),
        AstTyKind::Fn(a, r, _) => a.iter().any(rec) || rec(r),
        AstTyKind::Throws { ok, err } => rec(ok) || rec(err),
        AstTyKind::Array(e, _) | AstTyKind::Nest(e) | AstTyKind::Binding(_, e) => rec(e),
        AstTyKind::QPath(q) => rec(&q.self_ty),
        AstTyKind::Const(nabic_ast::ConstExpr::Path(p)) => {
            matches!(p.segments.as_slice(), [s] if s.name == name)
        }
        AstTyKind::Const(nabic_ast::ConstExpr::Expr(e)) => {
            crate::walk::free_names(&[], e).iter().any(|n| n == name)
        }
        _ => false,
    }
}

/// `v(a, b)` on a value whose type implements `ops::Call`:
/// `v.call((a, b))`, one argument as itself, none as `()`.
fn call_through_trait(
    ctx: &mut LowerCtx<'_>,
    callee_h: HirExpr,
    key: &str,
    args: &[CallArg],
    span: nabic_diagnostic::Span,
) -> HirExpr {
    let mut pos = Vec::with_capacity(args.len());
    for a in args {
        let CallArg::Positional(e) = a else {
            discard_args(ctx, args);
            let h = ctx.error_expr(
                Code::NAMED_ARGUMENT,
                span,
                "a call through `ops::Call` takes its arguments by position".to_owned(),
            );
            ctx.help_last(Some(
                "pass them by position; several arrive as one tuple".to_owned(),
            ));
            return h;
        };
        pos.push(e.clone());
    }
    let packed = match <[Expr; 1]>::try_from(pos) {
        Ok([one]) => one,
        Err(pos) => Expr {
            kind: ExprKind::Tuple(pos),
            span,
        },
    };
    let ty = callee_h.ty.clone();
    let args = [CallArg::Positional(packed)];
    crate::lower::expr::overload::call_impl_method(
        ctx,
        &ty,
        "call",
        Some(key),
        Some(callee_h),
        &args,
        "call",
        span,
    )
    .unwrap_or_else(|| poison(span))
}

/// Call through a function-pointer or closure value.
pub(crate) fn call_value(
    ctx: &mut LowerCtx<'_>,
    callee_h: HirExpr,
    args: &[CallArg],
    span: nabic_diagnostic::Span,
) -> HirExpr {
    let (arg_tys, ret, env) = match callee_h.ty.clone() {
        HirTy::Fn(a, r, b) => {
            if b.unsafe_fn {
                check_unsafe_call(ctx, "(unsafe fn value)");
            }
            (a, *r, None)
        }
        HirTy::Closure { args, ret, env, .. } => (args, *ret, Some(*env)),
        HirTy::Unknown => {
            discard_args(ctx, args);
            return callee_h;
        }
        other => {
            let key = ctx.lang("call_trait");
            if !key.is_empty() && ctx.has_trait(&other, key) {
                return call_through_trait(ctx, callee_h, key, args, span);
            }
            discard_args(ctx, args);
            let name = ctx.ty_name(&other);
            let e = ctx.error_expr(
                Code::NOT_CALLABLE,
                span,
                format!("a value of type `{name}` is not callable"),
            );
            if matches!(other, HirTy::Struct(_) | HirTy::Enum(_)) {
                ctx.help_last(Some(format!(
                    "to call it like a function, implement `ops::Call` for it: `impl \
                     ops::Call<f64> for {name} {{ type Output = f64; fn call(self, x: f64) -> \
                     f64 {{ .. }} }}` (several arguments come as a tuple)"
                )));
            }
            return e;
        }
    };
    if args.iter().any(|a| matches!(a, CallArg::Named { .. })) {
        let h = ctx.error_expr(
            Code::NAMED_ARGUMENT,
            span,
            "a `fn` value takes no named arguments: its type has no parameter names".to_owned(),
        );
        ctx.help_last(Some("pass the arguments by position".to_owned()));
        return h;
    }
    if args.iter().any(|a| !matches!(a, CallArg::Positional(_))) {
        return ctx.error_expr(
            Code::SPREAD_ARGUMENT,
            span,
            "spread arguments need a statically known callee".to_owned(),
        );
    }
    let sig = FnSig {
        id: FunctionId(u32::MAX),
        ret_ty: ret.clone(),
        param_defaults: vec![None; arg_tys.len()],
        param_tys: arg_tys.clone(),
        param_names: (0..arg_tys.len()).map(|i| format!("#{}", i + 1)).collect(),
        param_refs: Vec::new(),
        variadic: false,
    };
    let (lowered, lets) = lower_args_for(ctx, &sig, "the function value", None, args, span);
    let kind = match env {
        None => HirExprKind::CallIndirect {
            callee: Box::new(callee_h),
            args: lowered,
            ret_ty: ret.clone(),
        },
        Some(env_ty) => HirExprKind::CallClosure {
            closure: Box::new(callee_h),
            args: lowered,
            arg_tys,
            ret_ty: ret.clone(),
            env_ty,
        },
    };
    with_lets(
        lets,
        HirExpr {
            kind,
            ty: ret,
            span,
        },
    )
}

/// A use at `at` of const or static `key`: its marks' warnings.
pub(crate) fn item_marked(ctx: &LowerCtx<'_>, what: &str, key: &str, at: nabic_diagnostic::Span) {
    if !ctx.marks_checked() {
        return;
    }
    // Checking a const's own value is no use of it.
    let own = ctx.env.consts.get(key).is_some_and(|(_, v)| {
        v.span.file == at.file && v.span.start <= at.start && at.end <= v.span.end
    });
    if let (Some(mark), false) = (ctx.env.mark(key), own) {
        ctx.use_marked(what, leaf_name(key), mark, at);
    }
}

/// A use at `at` of function `id`: its marks' warnings.
pub(crate) fn fn_marked(ctx: &LowerCtx<'_>, id: FunctionId, at: nabic_diagnostic::Span) {
    if ctx.hook_call.replace(false) || !(ctx.marks_checked() || ctx.results_apply()) {
        return;
    }
    if let Some((name, mark)) = ctx.env.fn_mark(id) {
        ctx.call_marked(mark, at);
        let what = if name.contains("::") {
            "method"
        } else {
            "function"
        };
        ctx.use_marked(what, name, mark, at);
    }
}

/// A use at `at` of generic function or method template `def`
/// (`m::Box__get` for `Box::get`).
pub(crate) fn template_marked(ctx: &LowerCtx<'_>, def: &FnDef, at: nabic_diagnostic::Span) {
    if ctx.hook_call.replace(false) || !(ctx.marks_checked() || ctx.results_apply()) {
        return;
    }
    let Some(mark) = crate::marks::mark_of(&def.attrs) else {
        return;
    };
    ctx.call_marked(&mark, at);
    let leaf = leaf_name(&def.name);
    let leaf = leaf.split("__by_").next().unwrap_or(leaf);
    let (what, name) = match leaf.split_once("__") {
        Some((ty, m)) => ("method", format!("{ty}::{m}")),
        None => ("function", leaf.to_owned()),
    };
    ctx.use_marked(what, &name, &mark, at);
}

/// An extern static is a C global Nabi cannot check; reading or writing one
/// needs `unsafe`.
pub(crate) fn extern_static_use(
    ctx: &LowerCtx<'_>,
    full: &str,
    write: bool,
    at: nabic_diagnostic::Span,
) {
    let name = leaf_name(full);
    // `nabi_*` and `__nabi_*` (the REPL's cells) are the toolchain's own.
    if ctx.unsafe_depth > 0
        || at.is_dummy()
        || name.starts_with("nabi_")
        || name.starts_with("__nabi_")
        || !ctx.env.is_extern_static(full)
    {
        return;
    }
    // A write quotes its statement (`optind += 1`).
    let stmt = ctx
        .env
        .sources
        .file(at.file)
        .and_then(|(_, t)| t.get(at.start as usize..))
        .map(|r| r.split(['\n', ';']).next().unwrap_or(r).trim_end())
        .filter(|t| t.len() <= 40 && t.contains('='));
    let (what, fix) = match (write, stmt) {
        (false, _) => ("reads", format!("`unsafe {{ {name} }}`")),
        (true, Some(t)) => ("writes", format!("`unsafe {{ {t}; }}`")),
        (true, None) => ("writes", format!("`unsafe {{ {name} = ..; }}`")),
    };
    ctx.emit_error_at(
        Code::UNSAFE_REQUIRED,
        at,
        format!(
            "this {what} extern static `{name}` outside `unsafe`: Nabi cannot check a C global"
        ),
    );
    ctx.help_last(Some(format!("write {fix}")));
    if !write && ctx.src(at) == Some(name) {
        ctx.fix_last(
            at,
            format!("unsafe {{ {name} }}"),
            "wrap the read in `unsafe`",
        );
    }
}

/// An `unsafe fn` (or C function) is callable only in an `unsafe fn` body
/// or `unsafe { }` block.
pub(crate) fn check_unsafe_call(ctx: &LowerCtx<'_>, callee: &str) {
    if ctx.unsafe_depth == 0 {
        ctx.emit_error(
            Code::UNSAFE_REQUIRED,
            format!(
                "call to unsafe function `{}` outside an `unsafe` block or `unsafe fn`",
                leaf_name(callee)
            ),
        );
        ctx.help_last(Some(
            "wrap the call in `unsafe { .. }`; every `extern` function, `extern \"nabi\"` \
             included, is unsafe to call"
                .to_owned(),
        ));
    }
}

pub(crate) fn lower_call(
    ctx: &mut LowerCtx<'_>,
    callee: &Expr,
    args: &[CallArg],
    span: nabic_diagnostic::Span,
) -> HirExpr {
    let ExprKind::Path(p) = &callee.kind else {
        // `(s.f)(x)`, `make()(x)`, ...: call through the value.
        let callee_h = lower_expected(ctx, callee, HirTy::Unknown);
        return call_value(ctx, callee_h, args, span);
    };
    // A local of `fn(...)` / closure type shadows items.
    if let [s] = p.segments.as_slice() {
        if let Some(local) = ctx.lookup(&s.name) {
            let callee_h = read_var(ctx, local, callee.span);
            return call_value(ctx, callee_h, args, span);
        }
    }
    match alias_path(ctx, p) {
        Ok(a) => lower_path_call(ctx, a.as_ref().unwrap_or(p), args, span),
        Err(msg) => {
            discard_args(ctx, args);
            ctx.error_expr(Code::GENERIC_ARGUMENTS, span, msg)
        }
    }
}

pub(crate) const SPAWN: &str = "core::task::spawn";

pub(crate) const SPAWN_PLAIN: &str = "core::task::spawn_plain";

/// `task::spawn(e)`'s `e` makes the future right there (an `async` block,
/// an `async fn` call) and captures nothing a hand-off copies (a
/// collection, a future, a `[ref x]` cell): no other handle has its frame,
/// so `spawn_plain` hands nothing over.
pub(crate) fn plain_fresh_future(ctx: &LowerCtx<'_>, args: &[CallArg]) -> bool {
    let [CallArg::Positional(e)] = args else {
        return false;
    };
    // A local such a future went into, named nowhere else, at the loop
    // depth it was made at (another pass would hand the same one over).
    if let ExprKind::Path(p) = &e.kind {
        if let [s] = p.segments.as_slice() {
            return ctx
                .lookup(&s.name)
                .and_then(|l| ctx.sole_fresh.get(&l))
                .is_some_and(|&depth| depth == ctx.breakables.len());
        }
    }
    fresh_future(ctx, e)
}

/// `e` makes a future right there that captures nothing a hand-off
/// copies (see `plain_fresh_future`).
pub(crate) fn fresh_future(ctx: &LowerCtx<'_>, e: &Expr) -> bool {
    let plain = |t: &HirTy| *t != HirTy::Unknown && !transfer_needs(ctx, t, 0);
    match &e.kind {
        ExprKind::AsyncBlock(_) => crate::walk::free_names(&[], e).iter().all(|n| {
            ctx.lookup(n)
                .is_none_or(|l| !ctx.is_boxed(l) && plain(&ctx.var_ty(l)))
        }),
        ExprKind::Call { callee, .. } => {
            let ExprKind::Path(p) = &callee.kind else {
                return false;
            };
            let [s] = p.segments.as_slice() else {
                return false;
            };
            if ctx.lookup(&s.name).is_some() {
                return false;
            }
            let Some(key) = ctx.resolve_fn_name(&[s.name.as_str()]) else {
                return false;
            };
            let Some(sig) = ctx.env.sig(&key) else {
                return false;
            };
            let coro = ctx.env.decls.get(sig.id.0 as usize).is_some_and(|d| {
                d.def()
                    .attrs
                    .iter()
                    .any(|a| a.path.first().map(String::as_str) == Some(nabic_ast::CORO_FN_ATTR))
            });
            coro && sig.param_tys.iter().all(plain)
        }
        _ => false,
    }
}

/// `spawn_plain` for a `task::spawn` call that may skip the hand-off.
pub(crate) fn plain_spawn<'a>(
    ctx: &LowerCtx<'a>,
    full: &str,
    args: &[CallArg],
) -> Option<&'a FnDef> {
    (full == SPAWN && plain_fresh_future(ctx, args))
        .then(|| ctx.env.generic_fn(SPAWN_PLAIN))
        .flatten()
}

/// A hook's call needs no visibility (`lang_fn`); any other does.
pub(crate) fn hook_visible(ctx: &LowerCtx<'_>, hook: bool, full: &str) {
    if !hook {
        ctx.check_item_visible(full, "function");
    }
}

/// `call` with `hook_call` set when it calls a hook.
pub(crate) fn as_hook<'a, R>(
    ctx: &mut LowerCtx<'a>,
    hook: bool,
    call: impl FnOnce(&mut LowerCtx<'a>) -> R,
) -> R {
    ctx.hook_call.set(hook);
    let r = call(ctx);
    ctx.hook_call.set(false);
    r
}

/// A call of path `p`, not a local.
pub(crate) fn lower_path_call(
    ctx: &mut LowerCtx<'_>,
    p: &AstPath,
    args: &[CallArg],
    span: nabic_diagnostic::Span,
) -> HirExpr {
    let self_segs = self_path_segs(ctx, p);
    let segs: Vec<&str> = self_segs.iter().map(String::as_str).collect();
    let hook = segs.first().is_some_and(|s| lang_slot(s).is_some());
    let display = segs[usize::from(hook)..].join("::");

    let errors = ctx.env.sink.diagnostics.borrow().len();
    let concrete = ctx.resolve_fn_name(&segs);
    let generic = concrete
        .is_none()
        .then(|| ctx.resolve_generic_fn_name(&segs))
        .flatten();
    // An ambiguous name (reported) picks one: checking the arguments
    // against it would read as if that was meant.
    if ctx.env.sink.diagnostics.borrow().len() > errors {
        discard_args(ctx, args);
        return poison(span);
    }
    // Free function / extern / re-export, module-aware.
    if let Some(full) = concrete {
        hook_visible(ctx, hook, &full);
        let sig = ctx.sig_of_key(&full);
        return as_hook(ctx, hook, |ctx| {
            call_sig(ctx, &sig, &full, None, args, span)
        });
    }

    // Generic function: monomorphize on the argument types.
    if let Some(full) = generic {
        hook_visible(ctx, hook, &full);
        let generic_def = ctx.env.generic_fn(&full).expect("a generic function");
        if let Some(&op) = ctx.env.generic_intrinsic(&generic_def.name) {
            if generic_def.is_unsafe {
                check_unsafe_call(ctx, &full);
            }
            template_marked(ctx, generic_def, span);
            let targs = p.segments.last().map_or(&[][..], |s| s.generics.as_slice());
            let name = leaf_name(&generic_def.name).to_owned();
            if op == Intrinsic::AddrOf {
                return lower_addr_of(ctx, generic_def, &name, args, span);
            }
            return lower_intrinsic(ctx, op, &name, targs, args, span);
        }
        let explicit = p.segments.last().map_or(&[][..], |s| s.generics.as_slice());
        if !bind_explicit_generics(ctx, generic_def, explicit, &full) {
            return poison(span);
        }
        // Named as written in its errors.
        let def = plain_spawn(ctx, &full, args).unwrap_or(generic_def);
        return as_hook(ctx, hook, |ctx| {
            call_generic(ctx, def, None, None, args, &full, span)
        });
    }

    // `Box::get(b)` / `Box::new(v)`: generic-impl method by path.
    if let Some((method, ty_segs)) = segs.split_last() {
        let env = ctx.env;
        let gm = ctx
            .canon_type(ty_segs)
            .filter(|full| !ty_segs.is_empty() && !env.names.is_trait(full))
            .and_then(|full| env.generic_methods(&full, method))
            .and_then(|v| {
                v.iter()
                    .find(|g| g.info.trait_name.is_none())
                    .or_else(|| v.first())
            });
        if let Some(gm) = gm {
            check_method_info_visible(ctx, &gm.info, method);
            // `Box::<i32>::new(v)`: the type's arguments fix `Self`;
            // `Box::m::<U>(..)`: the method's own.
            let n = p.segments.len();
            let self_hint = (!p.segments[n - 2].generics.is_empty()).then(|| {
                ctx.resolve_ast_ty(&Ty {
                    kind: AstTyKind::Path(nabic_ast::Path {
                        segments: p.segments[..n - 1].to_vec(),
                        span: p.span,
                        origin: None,
                    }),
                    span: p.span,
                })
            });
            // `let a: Atomic<i64> = Atomic::new(0)`: a constructor that returns
            // `Self` takes `Self` from the expected type.
            let returns_self = gm.def.ret_ty.as_ref().is_some_and(|t| {
                matches!(&t.kind, AstTyKind::Path(rp) if rp.segments.len() == 1
                    && (rp.segments[0].name == "Self" || rp.segments[0].name == *ty_segs[ty_segs.len() - 1]))
            });
            let self_hint = self_hint.or_else(|| {
                let full = ctx.canon_type(ty_segs)?;
                ctx.current_expected().cloned().filter(|t| {
                    returns_self
                        && matches!(t, HirTy::Struct(id)
                            if enum_base_name(&ctx.struct_def(*id).name) == full)
                })
            });
            match bind_explicit(
                ctx,
                &gm.def,
                Some(&gm.self_ty),
                &p.segments[n - 1].generics,
                method,
            ) {
                Ok(bound) => ctx.explicit_generics = bound,
                Err(msg) => {
                    discard_args(ctx, args);
                    return ctx.error_expr(Code::GENERIC_ARGUMENTS, span, msg);
                }
            }
            return call_generic_on(
                ctx,
                &gm.def,
                Some(&gm.self_ty),
                self_hint.as_ref(),
                None,
                args,
                &display,
                span,
            );
        }
    }

    // `Pair(3, 4)` of a generic tuple struct: a struct literal with the
    // positional field names, which infers the instantiation.
    let generic_tuple = ctx.canon_type(&segs).is_some_and(|full| {
        ctx.lookup_struct(&full).is_none()
            && ctx
                .env
                .defs
                .generic_struct(&full)
                .is_some_and(|d| matches!(d.kind, nabic_ast::StructKind::Tuple(_)))
    });
    let dup_ctor = ctx.canon_type(&segs).is_some_and(|full| {
        let at = ctx
            .lookup_struct(&full)
            .map(|sid| ctx.struct_def(sid).span)
            .or_else(|| ctx.env.defs.generic_struct(&full).map(|d| d.span));
        at.is_some_and(|at| ctx.env.duplicated(at))
    });
    if dup_ctor {
        discard_args(ctx, args);
        return poison(span);
    }
    if generic_tuple && args.iter().all(|a| matches!(a, CallArg::Positional(_))) {
        let fields: Vec<(String, Expr)> = args
            .iter()
            .enumerate()
            .filter_map(|(i, a)| match a {
                CallArg::Positional(e) => Some((i.to_string(), e.clone())),
                _ => None,
            })
            .collect();
        return lower_struct_init(ctx, p, &fields, None, span);
    }
    // `Foo(arg, ...)`: tuple-struct constructor.
    if let Some(sid) = ctx
        .canon_type(&segs)
        .and_then(|full| ctx.lookup_struct(&full))
    {
        let def = ctx.struct_def(sid);
        let is_tuple_form = !def.fields.is_empty()
            && def
                .fields
                .iter()
                .enumerate()
                .all(|(i, f)| f.name == i.to_string());
        if !is_tuple_form {
            let fields: Vec<String> = def.fields.iter().map(|f| f.name.clone()).collect();
            return not_tuple_struct(ctx, p, &display, &fields, args, span);
        }
        // Constructing needs every field visible here.
        def.fields
            .iter()
            .for_each(|f| ctx.check_field_visible(sid, &f.name));
        let sig = FnSig {
            id: FunctionId(u32::MAX),
            ret_ty: HirTy::Struct(sid),
            param_defaults: vec![None; def.fields.len()],
            param_tys: def.fields.iter().map(|f| f.ty.clone()).collect(),
            param_names: def.fields.iter().map(|f| f.name.clone()).collect(),
            param_refs: Vec::new(),
            variadic: false,
        };
        let (fields, lets) = lower_args_for(ctx, &sig, &display, None, args, span);
        return with_lets(
            lets,
            HirExpr {
                kind: HirExprKind::StructInit {
                    struct_id: sid,
                    fields,
                },
                ty: HirTy::Struct(sid),
                span,
            },
        );
    }

    // `Enum::Variant(payload)`; a generic enum without an expected type is
    // instantiated from the arguments. `Enum::f(..)` that names no variant
    // is an associated function (below).
    let not_variant = enum_lacks_variant(ctx, &segs);
    let hint = explicit_enum(ctx, p).or_else(|| ctx.current_expected().cloned());
    let is_generic_enum_base = ctx.enum_path(&segs).is_some_and(|(full, _)| {
        ctx.lookup_enum(&full).is_none() && ctx.env.defs.generic_enum(&full).is_some()
    });
    let pre_args: Option<Vec<HirExpr>> = if !not_variant
        && is_generic_enum_base
        && !matches!(&hint, Some(HirTy::Enum(_)))
        && args.iter().all(|a| matches!(a, CallArg::Positional(_)))
    {
        Some(
            args.iter()
                .map(|a| match a {
                    CallArg::Positional(e) => lower_expected(ctx, e, HirTy::Unknown),
                    _ => unreachable!("checked positional"),
                })
                .collect(),
        )
    } else {
        None
    };
    let arg_tys: Option<Vec<HirTy>> = pre_args
        .as_ref()
        .map(|v| v.iter().map(|e| e.ty.clone()).collect());
    let res = if not_variant {
        EnumRes::NotEnum
    } else {
        resolve_enum_variant(ctx, &segs, hint.as_ref(), arg_tys.as_deref())
    };
    match res {
        EnumRes::Error => return poison(span),
        EnumRes::Found(enum_id, idx) => {
            let variant = ctx.enum_def(enum_id).variants[idx].clone();
            let slots = variant_arg_tys(&variant);
            if !variant.named_fields.is_empty() {
                return ctx.error_expr(
                    Code::WRONG_ITEM_FORM,
                    span,
                    format!("`{display}` has named fields - use `{display} {{ ... }}`"),
                );
            }
            let mut lets = Vec::new();
            let args_h = if let Some(pre) = pre_args {
                if pre.len() != slots.len() {
                    let tys = slots.iter().map(|t| ("", ctx.ty_name(t)));
                    let sig = crate::lower::sig_text(&display, tys);
                    arity_error(ctx, &display, slots.len(), pre.len(), Some(sig));
                    return poison(span);
                }
                pre.into_iter()
                    .zip(&slots)
                    .enumerate()
                    .map(|(i, (a, t))| {
                        coerce(ctx, t, a, &|| format!("argument {} of `{display}`", i + 1))
                    })
                    .collect()
            } else {
                let sig = FnSig {
                    id: FunctionId(u32::MAX),
                    ret_ty: HirTy::Enum(enum_id),
                    param_defaults: vec![None; slots.len()],
                    param_tys: slots.clone(),
                    param_names: (0..slots.len()).map(|i| format!("#{}", i + 1)).collect(),
                    param_refs: Vec::new(),
                    variadic: false,
                };
                let (a, l) = lower_args_for(ctx, &sig, &display, None, args, span);
                lets = l;
                a
            };
            return with_lets(
                lets,
                HirExpr {
                    kind: HirExprKind::EnumVariantInit {
                        enum_id,
                        variant_index: u32::try_from(idx).expect("variant overflow"),
                        args: args_h,
                    },
                    ty: HirTy::Enum(enum_id),
                    span,
                },
            );
        }
        EnumRes::NotEnum => {}
    }

    // `Type::method(args)` through the method table; a generic base
    // (`Stack::new()`) resolves when one instance has the method.
    // `Tr::m(x)`, `T::f()`, `i32::add(a, b)`, `Self::f()` go through the
    // trait's impls for the type.
    if let Some((method, ty_segs)) = segs.split_last().filter(|(_, t)| !t.is_empty()) {
        if let Some(r) =
            path_call_via_impls(ctx, &p.segments, ty_segs, method, args, &display, span)
        {
            return r;
        }
    }

    if let Some((method, ty_segs)) = segs.split_last() {
        if !ty_segs.is_empty() {
            if let Some(full) = ctx.canon_type(ty_segs) {
                let method = (*method).to_owned();
                if let Some(r) = named_type_overload(ctx, &full, &method, args, &display, span) {
                    return r;
                }
                let hits = match path_method_hits(ctx, &full, &method) {
                    Ok(h) => h,
                    Err(msg) => {
                        discard_args(ctx, args);
                        return ctx.error_expr(Code::AMBIGUOUS_ITEM, span, msg);
                    }
                };
                match hits.as_slice() {
                    [fn_id] => {
                        check_method_visible(ctx, *fn_id, &method);
                        let sig = ctx.sig_by_id(*fn_id);
                        return call_sig(ctx, &sig, &display, None, args, span);
                    }
                    [] => {
                        if let Some(r) =
                            named_type_call(ctx, &p.segments, &full, &method, args, span)
                        {
                            return r;
                        }
                        // A generic impl's method: `Self` is the expected type
                        // (`let b: Box<i32> = Box::zero()`), else `self`'s.
                        let of_base = |ctx: &LowerCtx<'_>, t: &HirTy| match t {
                            HirTy::Struct(id) => enum_base_name(&ctx.struct_def(*id).name) == full,
                            HirTy::Enum(id) => enum_base_name(&ctx.enum_def(*id).name) == full,
                            _ => false,
                        };
                        let expected = ctx.current_expected().cloned().filter(|t| of_base(ctx, t));
                        if let Some(r) =
                            static_call_on_base(ctx, expected, &full, &method, args, &display, span)
                        {
                            return r;
                        }
                        if let Some((a @ (CallArg::Positional(e) | CallArg::Ref(e)), rest)) =
                            args.split_first()
                        {
                            let recv = lower_expected(ctx, e, HirTy::Unknown);
                            let t = recv.ty.clone();
                            if of_base(ctx, &t) {
                                ctx.explicit_recv = Some(matches!(a, CallArg::Ref(_)));
                                let r = call_impl_method(
                                    ctx,
                                    &t,
                                    &method,
                                    None,
                                    Some(recv),
                                    rest,
                                    &display,
                                    span,
                                );
                                ctx.explicit_recv = None;
                                if let Some(r) = r {
                                    return r;
                                }
                            }
                            discard_args(ctx, rest);
                        }
                        let ty = ctx
                            .lookup_struct(&full)
                            .map(HirTy::Struct)
                            .or_else(|| ctx.lookup_enum(&full).map(HirTy::Enum));
                        if ty.is_some_and(|t| trait_out_of_scope(ctx, &t, &method, span)) {
                            return poison(span);
                        }
                        let e = ctx.error_expr(
                            Code::NO_METHOD,
                            span,
                            format!(
                                "no associated function `{method}` on `{}`",
                                ctx.env.names.visible_path(&full)
                            ),
                        );
                        crate::lower::suggest::no_assoc_help(ctx, &full, &method, &display, span);
                        return e;
                    }
                    _ => {
                        return ctx.error_expr(
                            Code::GENERIC_ARGUMENTS,
                            span,
                            format!(
                                "`{display}` is ambiguous: several instantiations of `{}` \
                                 provide `{method}` - name the type arguments",
                                ctx.env.names.visible_path(&full)
                            ),
                        );
                    }
                }
            }
        }
    }

    call_item_value(ctx, p, &segs, &display, args, span)
}

/// `Base::f(..)` on a generic base `full`: `Self` is the `expected` type
/// (`let b: Box<i32> = Box::zero()`); without one, a static method of a
/// generic trait impl on the base (`impl<T> From<List<T>> for Set<T>`)
/// infers it from the arguments (`Set::from(xs)`). `None`: neither, or
/// several such impls.
pub(crate) fn static_call_on_base(
    ctx: &mut LowerCtx<'_>,
    expected: Option<HirTy>,
    full: &str,
    method: &str,
    args: &[CallArg],
    display: &str,
    span: nabic_diagnostic::Span,
) -> Option<HirExpr> {
    if let Some(t) = expected {
        if let Some(r) = call_impl_method(ctx, &t, method, None, None, args, display, span) {
            return Some(r);
        }
    }
    let env = ctx.env;
    let cands: Vec<&GenericMethod> = env
        .impls
        .generic_by(crate::lower::Pick::Method(method))
        .filter(|gi| {
            let k = &gi.trait_key;
            !k.is_empty() && (nabic_core::in_toolchain(k) || trait_in_scope(ctx, k, true))
        })
        .filter(|gi| {
            let AstTyKind::Path(p) = &gi.self_ty.kind else {
                return false;
            };
            let segs: Vec<&str> = p.segments.iter().map(|s| s.name.as_str()).collect();
            let module = p.origin.as_deref().unwrap_or(&gi.module);
            env.names.canon(&segs, module, &[], span).as_deref() == Some(full)
        })
        .filter_map(|gi| gi.methods.get(method))
        .filter(|gm| gm.def.params.first().is_none_or(|p| p.name != "self"))
        .collect();
    let [gm] = cands.as_slice() else {
        return None;
    };
    check_method_info_visible(ctx, &gm.info, method);
    Some(call_generic_on(
        ctx,
        &gm.def,
        Some(&gm.self_ty),
        None,
        None,
        args,
        display,
        span,
    ))
}

/// The rest of a path call: a `const` / `static` of `fn(...)` type is
/// called through its value; anything else is unknown.
pub(crate) fn call_item_value(
    ctx: &mut LowerCtx<'_>,
    p: &AstPath,
    segs: &[&str],
    display: &str,
    args: &[CallArg],
    span: nabic_diagnostic::Span,
) -> HirExpr {
    if ctx.resolve_in(segs, false).is_some() || ctx.resolve_in(segs, true).is_some() {
        let callee_h = lower_path(ctx, p, p.span);
        return call_value(ctx, callee_h, args, span);
    }
    let since = ctx.env.sink.diagnostics.borrow().len();
    // Python's `isinstance(x, T)`: `T` is a type, not an unknown value.
    let typed = usize::from(segs == ["isinstance"] && args.len() == 2);
    discard_args(ctx, &args[..args.len() - typed]);
    // `Rc::new(RefCell::new(x))`: the outer call's one error has the whole
    // Nabi form.
    let nested = crate::lower::habits::nested_ctors(ctx, segs, args);
    if !nested.is_empty() {
        let mut diags = ctx.env.sink.diagnostics.borrow_mut();
        let mut i = since.min(diags.len());
        while i < diags.len() {
            let d = &diags[i];
            if d.code == Code::UNRESOLVED_NAME
                && d.primary
                    .as_ref()
                    .is_some_and(|l| nested.contains(&l.span.start))
            {
                diags.remove(i);
            } else {
                i += 1;
            }
        }
    }
    if let Some(help) = (segs.len() > 1).then(|| ctx.import_help(segs)).flatten() {
        return ctx.unknown_name(
            Code::UNRESOLVED_NAME,
            span,
            format!(
                "cannot find function `{display}`: module `{}` is not imported here",
                segs[0]
            ),
            Some(help),
        );
    }
    if let Some((msg, help)) = segs
        .first()
        .filter(|_| segs.len() == 1)
        .and_then(|n| ctx.variant_not_in_scope(n))
    {
        let arg = match args {
            [CallArg::Positional(e)] => ctx.src(e.span),
            _ => None,
        };
        let help = crate::lower::habits::result_help(segs[0], arg, ctx.fn_throws.is_some(), help);
        return ctx.unknown_name(Code::UNRESOLVED_VARIANT, span, msg, Some(help));
    }
    let help = habit_call_help(ctx, segs, args).or_else(|| ctx.value_help(segs));
    ctx.unknown_name(
        Code::UNRESOLVED_NAME,
        span,
        format!("cannot find function `{display}` in this scope"),
        help,
    )
}

/// `Name(args)` on a struct with named fields.
pub(crate) fn not_tuple_struct(
    ctx: &LowerCtx<'_>,
    p: &AstPath,
    display: &str,
    fields: &[String],
    args: &[CallArg],
    span: nabic_diagnostic::Span,
) -> HirExpr {
    let written = ctx.src(p.span).map_or_else(
        || {
            p.segments
                .last()
                .map(|s| s.name.clone())
                .unwrap_or_default()
        },
        str::to_owned,
    );
    let msg = match written.as_str() {
        "int" | "float" => format!("`{written}` is a type, not a conversion function"),
        _ => format!("`{display}` is not a tuple struct - use `{display} {{ field: ... }}`"),
    };
    let e = ctx.error_expr(Code::WRONG_ITEM_FORM, span, msg);
    ctx.help_last(class_call_help(ctx, &written, fields, args));
    e
}

/// Help for `Name(args)` on a struct with named fields: Python's class
/// call, or `int(s)` / `float(s)` meaning a parse.
pub(crate) fn class_call_help(
    ctx: &LowerCtx<'_>,
    written: &str,
    fields: &[String],
    args: &[CallArg],
) -> Option<String> {
    let texts: Vec<&str> = args
        .iter()
        .filter_map(|a| match a {
            CallArg::Positional(e) => ctx.src(e.span),
            _ => None,
        })
        .collect();
    let arg = texts.first().copied().unwrap_or("s");
    match written {
        "int" => {
            return Some(format!(
                "parse text with `try {arg}.parse::<int>()`; a number converts with `x as i64`"
            ));
        }
        "float" => {
            return Some(format!(
                "parse text with `try {arg}.parse::<f64>()`; a number converts with `x as f64`"
            ));
        }
        _ => {}
    }
    if fields.is_empty() || texts.len() != args.len() || texts.len() != fields.len() {
        return None;
    }
    let inits: Vec<String> = fields
        .iter()
        .zip(&texts)
        .map(|(f, v)| format!("{f}: {v}"))
        .collect();
    Some(format!("write `{written} {{ {} }}`", inits.join(", ")))
}

/// `x.f()` on an associated function `f` of a user type: Python's method
/// without `self`.
pub(crate) fn not_a_method_help(ctx: &LowerCtx<'_>, ty: &HirTy, method: &str) -> Option<String> {
    let user = match ty {
        HirTy::Struct(id) => !nabic_core::in_toolchain(&ctx.struct_def(*id).name),
        HirTy::Enum(id) => !nabic_core::in_toolchain(&ctx.enum_def(*id).name),
        _ => false,
    };
    user.then(|| format!("or, to call it on a value, take `self` first: `fn {method}(self, ..)`"))
}

/// What Nabi writes for a call another language has: `Some(x)`, `range(n)`.
pub(crate) fn habit_call_help(
    ctx: &LowerCtx<'_>,
    segs: &[&str],
    args: &[CallArg],
) -> Option<String> {
    let texts: Vec<&str> = args
        .iter()
        .filter_map(|a| match a {
            CallArg::Positional(e) => ctx.src(e.span),
            _ => None,
        })
        .collect();
    match (segs, texts.as_slice()) {
        (["Some"], [x]) => Some(format!(
            "Nabi has no `Some`: a `T | none` holds the value itself; write `{x}`"
        )),
        (["range"], [n]) => Some(format!("use a range: `0..{n}`")),
        (["range"], [a, b]) => Some(format!("use a range: `{a}..{b}`")),
        ([name], _) if texts.len() == args.len() => {
            crate::lower::suggest::python_builtin_help(name, &texts)
        }
        _ => crate::lower::habits::call_help(ctx, segs, args),
    }
}

/// `Box::<f64>::f()` on a generic impl, or `Reflect`'s `type_name()` /
/// `size()` / `align()` on a struct or enum instance.
pub(crate) fn named_type_call(
    ctx: &mut LowerCtx<'_>,
    segs: &[nabic_ast::PathSegment],
    full: &str,
    method: &str,
    args: &[CallArg],
    span: nabic_diagnostic::Span,
) -> Option<HirExpr> {
    let ty_segs = &segs[..segs.len() - 1];
    let own = if ty_segs.last().is_some_and(|s| !s.generics.is_empty()) {
        ctx.resolve_ast_ty(&Ty {
            kind: AstTyKind::Path(AstPath {
                segments: ty_segs.to_vec(),
                span,
                origin: None,
            }),
            span,
        })
    } else {
        ctx.lookup_struct(full)
            .map(HirTy::Struct)
            .or_else(|| ctx.lookup_enum(full).map(HirTy::Enum))?
    };
    if let Some(r) = call_impl_method(ctx, &own, method, None, None, args, method, span) {
        return Some(r);
    }
    crate::lower::reflect::static_call(ctx, &own, method, args, span)
}
