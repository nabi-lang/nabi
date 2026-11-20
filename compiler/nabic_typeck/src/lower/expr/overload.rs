//! Qualified paths, impl lookup and overloads.

#[allow(clippy::wildcard_imports)]
use crate::lower::expr::*;

/// A range: each shape calls its `#[literal]` handler (core's `Range`,
/// `RangeInclusive`, ...).
pub(crate) fn lower_range_value(
    ctx: &mut LowerCtx<'_>,
    start: Option<&Expr>,
    end: Option<&Expr>,
    inclusive: bool,
    span: nabic_diagnostic::Span,
) -> HirExpr {
    let (shape, what) = match (start, end, inclusive) {
        (Some(_), Some(_), false) => (Shape::Range, "`a..b`"),
        (Some(_), Some(_), true) => (Shape::RangeInclusive, "`a..=b`"),
        (Some(_), None, _) => (Shape::RangeFrom, "`a..`"),
        (None, Some(_), false) => (Shape::RangeTo, "`..b`"),
        (None, Some(_), true) => (Shape::RangeToInclusive, "`..=b`"),
        (None, None, _) => (Shape::RangeFull, "`..`"),
    };
    let args = [start, end]
        .into_iter()
        .flatten()
        .map(|e| nabic_ast::LitArg::One(e.clone()))
        .collect();
    lower_shape(ctx, shape, args, what, span)
}

/// `Type::method` on a named struct / enum (or each instantiation of a
/// generic base): the concrete methods `pick_method` chooses.
pub(crate) fn path_method_hits(
    ctx: &LowerCtx<'_>,
    full: &str,
    method: &str,
) -> Result<Vec<FunctionId>, String> {
    let mut tys: Vec<HirTy> = Vec::new();
    if let Some(sid) = ctx.lookup_struct(full) {
        tys.push(HirTy::Struct(sid));
    } else if let Some(eid) = ctx.lookup_enum(full) {
        tys.push(HirTy::Enum(eid));
    } else {
        tys.extend(
            struct_instantiations(ctx, full)
                .into_iter()
                .map(HirTy::Struct),
        );
        tys.extend(ctx.enum_instantiations(full).into_iter().map(HirTy::Enum));
    }
    let mut hits = Vec::new();
    for ty in tys {
        if let Some(MethodPick::Concrete(id)) = pick_method(ctx, &ty, method, None)? {
            hits.push(id);
        }
    }
    Ok(hits)
}

/// `<T as Tr>::m(args)`: `Tr`'s `m` for `T`, the first argument as `self`;
/// `<T as Tr>::X::m(args)`: `m` of associated type `X`.
pub(crate) fn qpath_member_call(
    ctx: &mut LowerCtx<'_>,
    q: &nabic_ast::QPath,
    method: &str,
    args: &[CallArg],
    span: nabic_diagnostic::Span,
) -> HirExpr {
    let assoc = nabic_ast::Ty {
        kind: nabic_ast::TyKind::QPath(Box::new(nabic_ast::QPath {
            member: None,
            ..q.clone()
        })),
        span,
    };
    let t = ctx.resolve_ast_ty(&assoc);
    if t == HirTy::Unknown {
        discard_args(ctx, args);
        return poison(span);
    }
    let display = format!("{}::{method}", ctx.ty_name(&t));
    call_impl_method(ctx, &t, method, None, None, args, &display, span)
        .or_else(|| crate::lower::reflect::static_call(ctx, &t, method, args, span))
        .unwrap_or_else(|| {
            discard_args(ctx, args);
            crate::lower::reflect::no_assoc_fn(ctx, &t, method, span)
        })
}

pub(crate) fn qpath_call(
    ctx: &mut LowerCtx<'_>,
    q: &nabic_ast::QPath,
    args: &[CallArg],
    span: nabic_diagnostic::Span,
) -> HirExpr {
    if let Some(m) = &q.member {
        return qpath_member_call(ctx, q, m, args, span);
    }
    let method = q.item.as_str();
    let t = ctx.resolve_ast_ty(&q.self_ty);
    let key = qpath_trait(ctx, q);
    let display = format!(
        "<{} as {}>::{method}",
        ctx.ty_name(&t),
        key.as_deref().unwrap_or("?")
    );
    let Some(key) = key else {
        discard_args(ctx, args);
        return ctx.error_expr(
            Code::NOT_A_TRAIT,
            span,
            format!("`{display}`: not a trait after `as`"),
        );
    };
    if method == "drop" && key == ctx.lang("drop_method") {
        if let Some(e) = direct_drop_error(ctx, span) {
            discard_args(ctx, args);
            return e;
        }
    }
    let (recv, rest) = match args.split_first() {
        Some((a @ (CallArg::Positional(e) | CallArg::Ref(e)), rest)) => {
            let r = lower_expected(ctx, e, t.clone());
            ctx.explicit_recv = Some(matches!(a, CallArg::Ref(_)));
            (Some(r), rest)
        }
        _ => (None, args),
    };
    // `<P as Convert<f64>>::convert(p)`: the impl of those arguments.
    if let Some(fid) = method_by_trait_args(ctx, &t, method, &key, q) {
        ctx.explicit_recv = None;
        check_method_visible(ctx, fid, method);
        let sig = ctx.sig_by_id(fid);
        return call_sig(ctx, &sig, &display, recv, rest, span);
    }
    // `<T as Tr<A>>::m(x)` on a type parameter: the bound `Tr<A>`; the
    // written arguments are the bound template's trait parameters.
    if let (HirTy::Param(_), Some(info), Some(seg)) =
        (&t, ctx.env.trait_of(&key), q.trait_path.segments.last())
    {
        let written = crate::lower::resolve::trait_args(&seg.generics, |a| ctx.resolve_ast_ty(a)).0;
        let names = info.generics.params.iter().map(|gp| match gp {
            nabic_ast::GenericParam::Type { name, .. }
            | nabic_ast::GenericParam::Const { name, .. } => name.clone(),
        });
        let bound: HashMap<String, HirTy> = names.zip(written).collect();
        ctx.explicit_generics.extend(bound);
    }
    let want = q.trait_path.segments.last().map(|seg| {
        let args = crate::lower::resolve::trait_args(&seg.generics, |a| ctx.resolve_ast_ty(a)).0;
        TraitRef::new(key.clone(), args, Vec::new())
    });
    let saved = std::mem::replace(&mut ctx.want_trait, want);
    let call = call_impl_method(ctx, &t, method, Some(&key), recv, rest, &display, span);
    ctx.want_trait = saved;
    ctx.explicit_recv = None;
    call.unwrap_or_else(|| qualified_miss(ctx, &t, &key, method, span))
}

/// `<T as Tr>::m` that resolves to nothing: the trait lacks `m`, or `T`
/// lacks the impl.
pub(crate) fn qualified_miss(
    ctx: &LowerCtx<'_>,
    t: &HirTy,
    key: &str,
    method: &str,
    span: nabic_diagnostic::Span,
) -> HirExpr {
    let tr = leaf_name(key);
    let methods: Option<Vec<String>> = ctx.env.trait_of(key).map(|info| info.methods.clone());
    match methods {
        Some(ms) if !ms.iter().any(|m| m == method) => {
            let h = ctx.error_expr(
                Code::NO_METHOD,
                span,
                format!("trait `{tr}` has no method `{method}`"),
            );
            if !ms.is_empty() {
                let list: Vec<String> = ms.iter().map(|m| format!("`{m}`")).collect();
                ctx.help_last(Some(format!("its methods: {}", list.join(", "))));
            }
            h
        }
        _ => ctx.error_expr(
            Code::NO_METHOD,
            span,
            format!("`{}` does not implement `{tr}`", ctx.ty_name(t)),
        ),
    }
}

/// The concrete `method` of `t` from the impl of `key` with the arguments
/// `<.. as Tr<A, ..>>` writes (defaults fill the rest); `None` without.
pub(crate) fn method_by_trait_args(
    ctx: &LowerCtx<'_>,
    t: &HirTy,
    method: &str,
    key: &str,
    q: &nabic_ast::QPath,
) -> Option<FunctionId> {
    let written = &q.trait_path.segments.last()?.generics;
    if written.is_empty() {
        return None;
    }
    let args: Vec<HirTy> = written.iter().map(|a| ctx.resolve_ast_ty(a)).collect();
    let ids = ctx.env.methods_on(t, method)?;
    let mut hits = ids.iter().filter(|id| {
        ctx.env.method_info(**id).is_some_and(|i| {
            i.trait_name.as_deref() == Some(key) && i.trait_args.starts_with(&args)
        })
    });
    let hit = *hits.next()?;
    hits.next().is_none().then_some(hit)
}

/// `Tr::m(..)` (`Self` from the first argument, else the expected type),
/// or `T::f(..)` / `i32::f(..)` / `Self::f(..)` on a non-struct/enum type.
/// `None`: not such a path.
#[allow(clippy::too_many_arguments)]
pub(crate) fn path_call_via_impls(
    ctx: &mut LowerCtx<'_>,
    all_segs: &[nabic_ast::PathSegment],
    ty_segs: &[&str],
    method: &str,
    args: &[CallArg],
    display: &str,
    span: nabic_diagnostic::Span,
) -> Option<HirExpr> {
    let trait_key = ctx
        .canon_type(ty_segs)
        .filter(|f| ctx.env.names.is_trait(f));
    if let Some(key) = trait_key {
        // A static method (`TryFrom::try_from(8)`) takes `Self` from the
        // expected type; its first argument is not `self`.
        let static_self = ctx
            .env
            .trait_of(&key)
            .and_then(|i| i.decls.get(method))
            .is_some_and(|d| d.params.first().is_none_or(|p| p.name != "self"))
            .then(|| ctx.current_expected().cloned())
            .flatten()
            .filter(|t| *t != HirTy::Unknown);
        let (mut recv, rest) = match args.split_first() {
            _ if static_self.is_some() => (None, args),
            Some((a @ (CallArg::Positional(e) | CallArg::Ref(e)), rest)) => {
                let r = lower_expected(ctx, e, HirTy::Unknown);
                ctx.explicit_recv = Some(matches!(a, CallArg::Ref(_)));
                (Some(r), rest)
            }
            _ => (None, args),
        };
        // `Wave::hi(3)`: the one number type with the trait, else the default,
        // here.
        if let Some(r) = recv
            .as_mut()
            .filter(|r| crate::lower::infer::has_var(ctx, &r.ty))
        {
            crate::lower::infer::pick_literals(ctx, &r.ty, &|t| {
                ctx.has_impl(t, &TraitRef::plain(&key), 0)
            });
            r.ty = crate::lower::infer::resolve(ctx, &r.ty);
        }
        let self_ty = static_self
            .or_else(|| recv.as_ref().map(|r| r.ty.clone()))
            .or_else(|| ctx.current_expected().cloned())
            .filter(|t| *t != HirTy::Unknown);
        let Some(t) = self_ty else {
            discard_args(ctx, rest);
            return Some(ctx.error_expr(
                Code::TYPE_ANNOTATIONS_NEEDED,
                span,
                format!(
                    "cannot tell which type's `{key}` impl `{display}` calls - annotate the \
                     expected type"
                ),
            ));
        };
        if key == crate::lower::reflect::REFLECT {
            ctx.explicit_recv = None;
            if let Some(r) =
                crate::lower::reflect::trait_call(ctx, &t, recv.clone(), method, rest, span)
            {
                return Some(r);
            }
        }
        match (&t, &recv) {
            (HirTy::Sum(_), Some(r)) if method == "fmt" && key == ctx.lang("debug_trait") => {
                ctx.explicit_recv = None;
                return Some(sum_structural_call(
                    ctx,
                    r.clone(),
                    method,
                    rest,
                    None,
                    span,
                ));
            }
            // `Trait::m(v)` on an `any` value: that trait's `m`, dispatched.
            (HirTy::AnyTrait(bounds), Some(r)) => {
                ctx.explicit_recv = None;
                let (bounds, r) = (bounds.clone(), r.clone());
                return Some(any_method_call(
                    ctx,
                    r,
                    &bounds,
                    method,
                    Some(&key),
                    &[],
                    rest,
                    span,
                ));
            }
            _ => {}
        }
        // `Conv::<f64>::conv(x)`: the impl of those arguments.
        let want = all_segs
            .len()
            .checked_sub(2)
            .map(|i| &all_segs[i].generics)
            .filter(|g| !g.is_empty())
            .map(|g| {
                let args = crate::lower::resolve::trait_args(g, |a| ctx.resolve_ast_ty(a)).0;
                TraitRef::new(key.clone(), args, Vec::new())
            });
        let saved = std::mem::replace(&mut ctx.want_trait, want);
        let call = call_impl_method(ctx, &t, method, Some(&key), recv, rest, display, span);
        ctx.want_trait = saved;
        ctx.explicit_recv = None;
        return Some(call.unwrap_or_else(|| qualified_miss(ctx, &t, &key, method, span)));
    }
    // Only paths the struct / enum lookup can't take: type parameters,
    // primitives, `Self`, associated types.
    let ty_path = nabic_ast::Path {
        segments: all_segs[..all_segs.len() - 1].to_vec(),
        span,
        origin: None,
    };
    let ty_ast = nabic_ast::Ty {
        kind: nabic_ast::TyKind::Path(ty_path.clone()),
        span,
    };
    let (name, projected) = match ty_segs {
        [name] => (*name, None),
        _ if ctx.canon_type(ty_segs).is_none() && ty_path.segments.len() > 1 => {
            ("", Some(ctx.projection(&ty_path)?))
        }
        _ => return None,
    };
    let is_type_param = ctx.ty_subst.contains_key(name);
    let prim = matches!(crate::ty::resolve_ty(&ty_ast), HirTy::Prim(_));
    let assoc = ctx.self_ty.is_some() && ctx.assoc_names.iter().any(|a| a == name);
    if !(projected.is_some() || is_type_param || prim || assoc || name == "Self") {
        return None;
    }
    let t = projected.unwrap_or_else(|| ctx.resolve_ast_ty(&ty_ast));
    if matches!(t, HirTy::Unknown) {
        discard_args(ctx, args);
        return Some(poison(span));
    }
    // `T::default()` for a `T | none` sum is `none`.
    if let (HirTy::Sum(vs), "default", []) = (&t, method, args) {
        if let Some(i) = vs.iter().position(HirTy::is_none) {
            return Some(HirExpr {
                kind: HirExprKind::SumWrap {
                    sum_ty: t.clone(),
                    variant_index: u32::try_from(i).expect("variant overflow"),
                    value: None,
                },
                ty: t.clone(),
                span,
            });
        }
    }
    // `T::from_str(s)` under `T: FromStr` is the bound's method, not an
    // inherent one of the same name (`int::from_str(s, radix)`).
    let bound_key = ctx
        .ty_bounds
        .get(name)
        .filter(|_| is_type_param)
        .and_then(|bs| {
            bs.iter()
                .map(|b| b.split('<').next().unwrap_or(b))
                .find(|k| {
                    ctx.env
                        .trait_of(k)
                        .is_some_and(|i| i.decls.contains_key(method))
                })
                .map(str::to_owned)
        });
    let call = call_impl_method(
        ctx,
        &t,
        method,
        bound_key.as_deref(),
        None,
        args,
        display,
        span,
    )
    .or_else(|| crate::lower::reflect::static_call(ctx, &t, method, args, span));
    Some(call.unwrap_or_else(|| {
        discard_args(ctx, args);
        crate::lower::reflect::no_assoc_fn(ctx, &t, method, span)
    }))
}

/// Call `t`'s method `method` (of trait `trait_key` when given) from a
/// concrete impl, else a generic one. `recv` fills `self`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn call_impl_method(
    ctx: &mut LowerCtx<'_>,
    t: &HirTy,
    method: &str,
    trait_key: Option<&str>,
    recv: Option<HirExpr>,
    args: &[CallArg],
    display: &str,
    span: nabic_diagnostic::Span,
) -> Option<HirExpr> {
    if let Some(call) = call_overload(ctx, t, method, trait_key, recv.clone(), args, display, span)
    {
        return Some(call);
    }
    let gm = match pick_method(ctx, t, method, trait_key) {
        Ok(Some(MethodPick::Concrete(fid))) => {
            check_method_visible(ctx, fid, method);
            let sig = ctx.sig_by_id(fid);
            return Some(call_sig(ctx, &sig, display, recv, args, span));
        }
        Ok(Some(MethodPick::Generic(gm))) => gm,
        Ok(None) => return None,
        Err(msg) => {
            match pick_by_arg(ctx, t, method, trait_key, args) {
                Some(MethodPick::Concrete(fid)) => {
                    check_method_visible(ctx, fid, method);
                    let sig = ctx.sig_by_id(fid);
                    return Some(call_sig(ctx, &sig, display, recv, args, span));
                }
                Some(MethodPick::Generic(gm)) => {
                    check_method_info_visible(ctx, &gm.info, method);
                    return Some(call_generic_on(
                        ctx,
                        &gm.def,
                        Some(&gm.self_ty),
                        Some(t),
                        recv,
                        args,
                        display,
                        span,
                    ));
                }
                None => {}
            }
            let Some(gm) = pick_generic_by_bounds(ctx, t, method, args) else {
                discard_args(ctx, args);
                return Some(ctx.error_expr(Code::AMBIGUOUS_ITEM, span, msg));
            };
            gm
        }
    };
    check_method_info_visible(ctx, &gm.info, method);
    // `Self` = `t` binds the impl's parameters, so a static method
    // (`fn zero() -> Self`) instantiates too.
    Some(call_generic_on(
        ctx,
        &gm.def,
        Some(&gm.self_ty),
        Some(t),
        recv,
        args,
        display,
        span,
    ))
}

/// A trait implemented several times with generic methods
/// (`FromIterator<char>` / `<str>` for `str`): the one template whose
/// bounds hold with the argument types (rolled-back probe).
pub(crate) fn pick_generic_by_bounds<'e>(
    ctx: &mut LowerCtx<'e>,
    t: &HirTy,
    method: &str,
    args: &[CallArg],
) -> Option<&'e GenericMethod> {
    let cands = generic_method_for(ctx, t, method);
    let tr = cands.first()?.info.trait_name.as_deref()?;
    if cands.len() < 2
        || cands
            .iter()
            .any(|g| g.info.trait_name.as_deref() != Some(tr))
    {
        return None;
    }
    let mark = ctx.probe_mark();
    let tys: Vec<HirTy> = args
        .iter()
        .map(|a| match a {
            CallArg::Positional(e) => lower_expected(ctx, e, HirTy::Unknown).ty,
            _ => HirTy::Unknown,
        })
        .collect();
    let clean = ctx.probe_clean(&mark);
    ctx.rollback(&mark);
    if !clean {
        return None;
    }
    let holds = |gm: &GenericMethod| {
        let def = &gm.def;
        let mut subst: HashMap<String, HirTy> = HashMap::default();
        // A method call's arguments follow its receiver.
        let k = usize::from(
            def.params.first().is_some_and(|p| p.name == "self") && tys.len() < def.params.len(),
        );
        for (p, ty) in def.params.iter().skip(k).zip(&tys) {
            let Some(nabic_ast::TyKind::Path(path)) = p.ty.as_ref().map(|t| &t.kind) else {
                continue;
            };
            if let [seg] = path.segments.as_slice() {
                subst.insert(seg.name.clone(), ty.clone());
            }
        }
        let module = module_of(&def.name);
        def.generics.params.iter().all(|gp| {
            let nabic_ast::GenericParam::Type { name, bounds, .. } = gp else {
                return true;
            };
            let Some(concrete) = subst.get(name) else {
                return true;
            };
            bounds.iter().all(|b| {
                ctx.bound_ref(b, &module, |t| {
                    crate::lower::mono::resolve_in_mono(ctx, def, &subst, None, t)
                })
                .is_none_or(|want| ctx.implements_trait(concrete, &want, 0))
            })
        })
    };
    let mut hits = cands.iter().filter(|gm| holds(gm));
    let hit = hits.next()?;
    hits.next().is_none().then_some(hit)
}

/// Impls of one trait over different argument types, generic ones among
/// them (`Call<f64>` beside `impl<const N: usize> Call<Array<f64, N>>`):
/// the one the first argument's type matches, a concrete impl before a
/// generic one. An unsuffixed literal takes the impl its value fits, a
/// list literal a `List` impl, else the `Array` impl of its nesting depth.
pub(crate) fn pick_by_arg<'e>(
    ctx: &mut LowerCtx<'e>,
    t: &HirTy,
    method: &str,
    trait_key: Option<&str>,
    args: &[CallArg],
) -> Option<MethodPick<'e>> {
    let Some(CallArg::Positional(e)) = args.first() else {
        return None;
    };
    let mut concrete: Vec<(FunctionId, HirTy)> = Vec::new();
    let mut tn: Option<String> = trait_key.map(str::to_owned);
    let mut same = |name: Option<&str>| match (name, &tn) {
        (None, _) => false,
        (Some(n), Some(k)) => n == k,
        (Some(n), None) => {
            tn = Some(n.to_owned());
            true
        }
    };
    for id in ctx.env.methods_on(t, method).into_iter().flatten() {
        let info = ctx.env.method_info(*id)?;
        if !same(info.trait_name.as_deref()) {
            return None;
        }
        let sig = ctx.sig_by_id(*id);
        let k = usize::from(sig.param_names.first().is_some_and(|n| n == "self"));
        concrete.push((*id, sig.param_tys.get(k)?.clone()));
    }
    let mut generic: Vec<&'e GenericMethod> = generic_method_for(ctx, t, method);
    generic.extend(
        ctx.generic_impls(t, crate::lower::Pick::Method(method), 0)
            .map(|(gi, _)| &gi.methods[method]),
    );
    for gm in &generic {
        if !same(gm.info.trait_name.as_deref()) {
            return None;
        }
    }
    if concrete.len() + generic.len() < 2 {
        return None;
    }
    // A generic template's parameter, with the impl's parameters bound
    // from the receiver type.
    let slot = |ctx: &LowerCtx<'e>, gm: &'e GenericMethod| {
        let def = &gm.def;
        let k = usize::from(def.params.first().is_some_and(|p| p.name == "self"));
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
        crate::lower::mono::unify_param_ty_with(&gm.self_ty, t, &names, &mut subst, ctx);
        def.params
            .get(k)
            .and_then(|p| p.ty.as_ref())
            .map(|ty| (ty, names, subst))
    };
    let one = |hits: Vec<MethodPick<'e>>| match hits.as_slice() {
        [p] => Some(*p),
        _ => None,
    };
    if untyped_lit_value(e).is_some() {
        let params: Vec<HirTy> = concrete.iter().map(|(_, p)| p.clone()).collect();
        return match pick_literal_impl(ctx, e, &params) {
            Ok(Some(i)) => Some(MethodPick::Concrete(concrete[i].0)),
            _ => None,
        };
    }
    if matches!(e.kind, ExprKind::List(_)) {
        let lists: Vec<MethodPick<'e>> = concrete
            .iter()
            .filter(|(_, p)| crate::lower::expr::is_list(ctx, p))
            .map(|(id, _)| MethodPick::Concrete(*id))
            .collect();
        if !lists.is_empty() {
            return one(lists);
        }
        let depth = crate::lower::expr::list_depth(e);
        let mut hits: Vec<MethodPick<'e>> = concrete
            .iter()
            .filter(|(_, p)| nd_rank(ctx, p) == Some(depth))
            .map(|(id, _)| MethodPick::Concrete(*id))
            .collect();
        if hits.is_empty() {
            for gm in &generic {
                let Some((ty, _, subst)) = slot(ctx, gm) else {
                    continue;
                };
                let h = crate::lower::expr::ty_hint(ctx, &gm.def, &subst, Some(t), Some(ty), e);
                if nd_rank(ctx, &h) == Some(depth) {
                    hits.push(MethodPick::Generic(gm));
                }
            }
        }
        return one(hits);
    }
    let mark = ctx.probe_mark();
    let arg = lower_expected(ctx, e, HirTy::Unknown).ty;
    let arg = crate::lower::infer::resolve(ctx, &arg);
    let clean = ctx.probe_clean(&mark);
    ctx.rollback(&mark);
    if !clean || arg == HirTy::Unknown {
        return None;
    }
    // Open literals inside (`Array<{float}, 1>` of `linspace(0.0, 1.0, 3)`):
    // as the types they hold.
    let arg = crate::lower::infer::zonk(ctx, &arg);
    if crate::lower::infer::has_var(ctx, &arg) {
        return None;
    }
    let exact: Vec<MethodPick<'e>> = concrete
        .iter()
        .filter(|(_, p)| *p == arg)
        .map(|(id, _)| MethodPick::Concrete(*id))
        .collect();
    if !exact.is_empty() {
        return one(exact);
    }
    let mut hits = Vec::new();
    for gm in &generic {
        let Some((ty, names, mut subst)) = slot(ctx, gm) else {
            continue;
        };
        crate::lower::mono::unify_param_ty_with(ty, &arg, &names, &mut subst, ctx);
        // An unbound parameter would resolve as a missing name.
        if names
            .iter()
            .any(|n| !subst.contains_key(n) && crate::lower::expr::ast_ty_mentions(ty, n))
        {
            continue;
        }
        let mark = ctx.probe_mark();
        let got = crate::lower::mono::resolve_in_mono(ctx, &gm.def, &subst, Some(t.clone()), ty);
        let clean = ctx.probe_clean(&mark);
        ctx.rollback(&mark);
        if clean && got == arg {
            hits.push((gm, subst));
        }
    }
    // Several fit the argument's shape (`Tr<D>` and `Tr<Iid<D>>`): the
    // ones whose bounds hold for what it binds.
    if hits.len() > 1 {
        hits.retain(|(gm, subst)| bounds_hold(ctx, gm, subst));
    }
    one(hits
        .into_iter()
        .map(|(gm, _)| MethodPick::Generic(gm))
        .collect())
}

/// Every bound of `gm`'s type parameters bound in `subst` holds.
fn bounds_hold(ctx: &LowerCtx<'_>, gm: &GenericMethod, subst: &HashMap<String, HirTy>) -> bool {
    let def = &gm.def;
    let module = module_of(&def.name);
    def.generics.params.iter().all(|gp| {
        let nabic_ast::GenericParam::Type { name, bounds, .. } = gp else {
            return true;
        };
        let Some(concrete) = subst.get(name) else {
            return true;
        };
        bounds.iter().all(|b| {
            ctx.bound_ref(b, &module, |t| {
                crate::lower::mono::resolve_in_mono(ctx, def, subst, None, t)
            })
            .is_none_or(|want| ctx.implements_trait(concrete, &want, 0))
        })
    })
}

/// The rank of an `Array<T, N>` type.
pub(crate) fn nd_rank(ctx: &LowerCtx<'_>, ty: &HirTy) -> Option<u32> {
    let HirTy::Struct(id) = ty else {
        return None;
    };
    let def = ctx.struct_def(*id);
    let nd = ctx.lang("ndarray_type");
    if nd.is_empty() || enum_base_name(&def.name) != nd {
        return None;
    }
    def.fields
        .iter()
        .find(|f| f.name == "shape")
        .and_then(|f| match &f.ty {
            HirTy::Array(_, n) => Some(*n),
            _ => None,
        })
}

/// `call_overload` on the struct / enum named `full`.
pub(crate) fn named_type_overload(
    ctx: &mut LowerCtx<'_>,
    full: &str,
    method: &str,
    args: &[CallArg],
    display: &str,
    span: nabic_diagnostic::Span,
) -> Option<HirExpr> {
    let t = ctx
        .lookup_struct(full)
        .map(HirTy::Struct)
        .or_else(|| ctx.lookup_enum(full).map(HirTy::Enum))?;
    call_overload(ctx, &t, method, None, None, args, display, span)
}

/// A trait implemented over several types (`From<i32>` + `From<i8>`): pick
/// by the first parameter the impls disagree on, an unsuffixed literal via
/// `pick_literal_impl`, anything else by exact type. `None` if `method`
/// isn't overloaded so, or (method call) no impl has a non-literal
/// argument's type.
#[allow(clippy::too_many_arguments)]
pub(crate) fn call_overload(
    ctx: &mut LowerCtx<'_>,
    t: &HirTy,
    method: &str,
    trait_key: Option<&str>,
    recv: Option<HirExpr>,
    args: &[CallArg],
    display: &str,
    span: nabic_diagnostic::Span,
) -> Option<HirExpr> {
    let ids = ctx.env.methods_on(t, method)?;
    let traits: Vec<Option<&str>> = ids
        .iter()
        .map(|id| {
            ctx.env
                .method_info(*id)
                .and_then(|i| i.trait_name.as_deref())
        })
        .collect();
    let tn = *traits
        .iter()
        .flatten()
        .find(|t| trait_key.is_none_or(|k| k == **t))?;
    let sigs: Vec<FnSig> = ids
        .iter()
        .zip(&traits)
        .filter(|(_, t)| **t == Some(tn))
        .map(|(id, _)| ctx.sig_by_id(*id))
        .collect();
    if sigs.len() < 2 || traits.contains(&None) {
        return None;
    }
    let arity = sigs.iter().map(|s| s.param_tys.len()).min()?;
    let slot = (0..arity).find(|&k| sigs.iter().any(|s| s.param_tys[k] != sigs[0].param_tys[k]))?;
    let params: Vec<HirTy> = sigs.iter().map(|s| s.param_tys[slot].clone()).collect();
    let mut literal = false;
    let arg_ty = if let (Some(r), 0) = (&recv, slot) {
        r.ty.clone()
    } else {
        let Some(CallArg::Positional(e)) = args.get(slot - usize::from(recv.is_some())) else {
            return None;
        };
        literal = untyped_lit_value(e).is_some();
        match pick_literal_impl(ctx, e, &params) {
            Ok(Some(i)) => params[i].clone(),
            Ok(None) => {
                let mark = ctx.probe_mark();
                let ty = lower_expected(ctx, e, HirTy::Unknown).ty;
                ctx.rollback(&mark);
                ty
            }
            Err(fits) => {
                discard_args(ctx, args);
                return Some(ctx.error_expr(
                    Code::AMBIGUOUS_LITERAL,
                    span,
                    format!(
                        "`{display}`: the literal fits several `{}` impls of `{}` ({fits}); \
                         add a suffix or `as`",
                        leaf_name(tn),
                        ctx.ty_name(t)
                    ),
                ));
            }
        }
    };
    // An open literal picks the overload: the one number type with one,
    // else its default.
    let arg_ty = if crate::lower::infer::has_var(ctx, &arg_ty) {
        crate::lower::infer::pick_literals(ctx, &arg_ty, &|t| params.contains(t));
        crate::lower::infer::resolve(ctx, &arg_ty)
    } else {
        arg_ty
    };
    let Some(i) = params.iter().position(|p| *p == arg_ty) else {
        if recv.is_some() && !literal {
            return None;
        }
        discard_args(ctx, args);
        if arg_ty == HirTy::Unknown {
            return Some(poison(span));
        }
        return Some(ctx.error_expr(
            Code::UNSATISFIED_BOUND,
            span,
            format!(
                "`{display}`: no `{}<{}>` impl for `{}`",
                leaf_name(tn),
                ctx.ty_name(&arg_ty),
                ctx.ty_name(t)
            ),
        ));
    };
    check_method_visible(ctx, sigs[i].id, method);
    Some(call_sig(ctx, &sigs[i], display, recv, args, span))
}
