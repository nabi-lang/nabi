//! Paths, aliases and associated constants.

#[allow(clippy::wildcard_imports)]
use crate::lower::expr::*;

/// `p` with `Self` and alias prefixes replaced by absolute paths: `B::mk`
/// (`type B = S<i32>`) is `S::<i32>::mk`; `Al::mk` (`type Al<T> = S<T>`)
/// is `S::mk` with inferred arguments (each must be a distinct parameter
/// of the alias). `None` if no type prefix.
pub(crate) fn alias_path(ctx: &LowerCtx<'_>, p: &AstPath) -> Result<Option<AstPath>, String> {
    let Some((last, prefix)) = p.segments.split_last().filter(|(_, t)| !t.is_empty()) else {
        return Ok(None);
    };
    let names = &ctx.env.names;
    let at = ctx.here();
    let mut ty = Ty {
        kind: AstTyKind::Path(AstPath {
            segments: prefix.to_vec(),
            span: p.span,
            origin: None,
        }),
        span: p.span,
    };
    let mut through = false;
    for _ in 0..32 {
        let AstTyKind::Path(tp) = &ty.kind else {
            return Ok(None);
        };
        let segs: Vec<&str> = tp.segments.iter().map(|s| s.name.as_str()).collect();
        let Some(full) = ctx.canon_type(&segs) else {
            break;
        };
        let generic = names
            .generic_alias(&full)
            .filter(|a| !a.generics.params.is_empty());
        let unapplied = tp.segments.last().is_some_and(|s| s.generics.is_empty());
        ty = match generic {
            Some(a) if unapplied => {
                let params: HashMap<String, Ty> = a
                    .generics
                    .params
                    .iter()
                    .map(|gp| {
                        let (nabic_ast::GenericParam::Type { name, .. }
                        | nabic_ast::GenericParam::Const { name, .. }) = gp;
                        (
                            name.clone(),
                            Ty {
                                kind: AstTyKind::Never,
                                span: at,
                            },
                        )
                    })
                    .collect();
                let mut body = a.ty.clone();
                names.absolutize(
                    &mut body,
                    &module_of(&full),
                    &ctx.current_module,
                    &params,
                    at,
                );
                let AstTyKind::Path(bp) = &mut body.kind else {
                    return Ok(None);
                };
                // `let m: Matrix<f64> = Matrix::zeros(..)`: a typed slot of the
                // aliased type that agrees with the alias's fixed arguments
                // gives them all.
                let fits_slot = |bp: &AstPath| {
                    let Some(HirTy::Struct(id)) = ctx.current_expected() else {
                        return false;
                    };
                    let def = ctx.struct_def(*id);
                    let Some(last) = bp.segments.last() else {
                        return false;
                    };
                    let base: Vec<&str> = bp.segments.iter().map(|s| s.name.as_str()).collect();
                    ctx.canon_type(&base).as_deref() == Some(enum_base_name(&def.name))
                        && last.generics.len() == def.generic_args.len()
                        && last.generics.iter().zip(&def.generic_args).all(|(g, have)| {
                            let param = matches!(&g.kind, AstTyKind::Path(gp)
                                if gp.segments.len() == 1 && params.contains_key(&gp.segments[0].name));
                            let want = ctx.resolve_ast_ty(g);
                            param
                                || want == *have
                                || want.as_const().zip(have.as_const()).is_some_and(|(a, b)| a.0 == b.0)
                        })
                };
                let slot = fits_slot(bp);
                let mut seen = nabic_core::HashSet::default();
                let open = bp.segments.last_mut().filter(|s| {
                    slot || s.generics.iter().all(|g| {
                        matches!(&g.kind, AstTyKind::Path(gp) if gp.segments.len() == 1
                            && params.contains_key(&gp.segments[0].name)
                            && seen.insert(gp.segments[0].name.clone()))
                    })
                });
                let Some(open) = open else {
                    let path = segs.join("::");
                    return Err(format!(
                        "`{path}::{}` can't infer `{}`'s arguments: its aliased type's arguments \
                         are not its distinct parameters - write `{path}::<..>::{}`, or give \
                         the slot its type (`let x: {path}<..> = {path}::{}(..)`)",
                        last.name,
                        leaf_name(&full),
                        last.name,
                        last.name
                    ));
                };
                open.generics.clear();
                body
            }
            Some(_) => match names.expand_alias(&ty, &ctx.current_module, at) {
                Some(t) => t,
                None => break,
            },
            None => match names.plain_alias_ty(&full) {
                Some(t) => {
                    let mut body = t.clone();
                    let none = HashMap::default();
                    names.absolutize(&mut body, &module_of(&full), &ctx.current_module, &none, at);
                    body
                }
                None => break,
            },
        };
        through = true;
    }
    let AstTyKind::Path(tp) = ty.kind else {
        return Ok(None);
    };
    Ok(through.then(|| AstPath {
        segments: tp.segments.into_iter().chain([last.clone()]).collect(),
        span: p.span,
        origin: None,
    }))
}

pub(crate) fn self_path_segs(ctx: &LowerCtx<'_>, p: &AstPath) -> Vec<String> {
    let segs: Vec<String> = p.segments.iter().map(|s| s.name.clone()).collect();
    let owner = match (segs.first().map(String::as_str), &ctx.self_ty) {
        (Some("Self"), Some(HirTy::Struct(id))) if segs.len() > 1 => ctx.struct_def(*id).name,
        (Some("Self"), Some(HirTy::Enum(id))) if segs.len() > 1 => ctx.enum_def(*id).name,
        _ => return segs,
    };
    let root = (!nabic_core::in_toolchain(&owner)).then(|| "pkg".to_owned());
    root.into_iter()
        .chain(owner.split("::").map(str::to_owned))
        .chain(segs.into_iter().skip(1))
        .collect()
}

/// Impls (with their substitution) giving `ty` associated const `name`,
/// from `traits` if given, with `wants`' trait arguments (`<T as
/// Tr<A>>::N`, `T::N` under `T: Tr<A>`).
pub(crate) fn assoc_const_hits<'e>(
    ctx: &LowerCtx<'e>,
    ty: &HirTy,
    name: &str,
    traits: Option<&[String]>,
    wants: &[TraitRef],
) -> Vec<(&'e Impl, HashMap<String, HirTy>)> {
    let env: &'e Env<'e> = ctx.env;
    env.impls
        .with_const(name)
        .filter(|imp| traits.is_none_or(|ks| ks.contains(&imp.trait_key)))
        .filter_map(|imp| {
            let (subst, have) = if let Some((t, r)) = &imp.concrete {
                (
                    (t == ty).then(HashMap::default)?,
                    r.as_ref().map(|r| r.args.clone()).unwrap_or_default(),
                )
            } else {
                let subst = ctx.generic_impl_subst(imp, ty, 0)?;
                let mut rcx = LowerCtx::new(ctx.env, &subst, None, ctx.here());
                rcx.current_module.clone_from(&imp.module);
                let have =
                    crate::lower::resolve::trait_args(&imp.trait_args, |t| rcx.resolve_ast_ty(t)).0;
                (subst, have)
            };
            let mut of = wants.iter().filter(|w| w.path == imp.trait_key).peekable();
            let fits =
                of.peek().is_none() || of.any(|w| w.args.iter().zip(&have).all(|(w, h)| w == h));
            fits.then_some((imp, subst))
        })
        .collect()
}

pub(crate) fn lower_assoc_const(
    ctx: &mut LowerCtx<'_>,
    ty: &HirTy,
    name: &str,
    traits: Option<&[String]>,
    wants: &[TraitRef],
) -> Option<HirExpr> {
    if let HirTy::Param(p) = ty {
        return crate::lower::bounds::assoc_const(ctx, *p, name, traits, wants);
    }
    let env = ctx.env;
    let mut hits = assoc_const_hits(ctx, ty, name, traits, wants);
    // An inherent const shadows the trait ones (`i8::BITS`).
    if hits.len() > 1 && traits.is_none() && hits.iter().any(|(s, _)| s.trait_key.is_empty()) {
        hits.retain(|(s, _)| s.trait_key.is_empty());
    }
    if hits.len() > 1 {
        let from: Vec<String> = hits
            .iter()
            .map(|(s, _)| {
                if s.trait_key.is_empty() {
                    "an inherent impl".to_owned()
                } else {
                    format!("`{}`", leaf_name(&s.trait_key))
                }
            })
            .collect();
        return Some(ctx.error_expr(
            Code::AMBIGUOUS_ITEM,
            ctx.here(),
            format!(
                "associated const `{name}` of `{}` is ambiguous: {} define it - name the \
                 trait (`<T as Tr>::{name}`)",
                ctx.ty_name(ty),
                from.join(" and ")
            ),
        ));
    }
    let (imp, subst) = hits.into_iter().next()?;
    let (_, cty, value) = imp.consts.iter().find(|(n, ..)| n == name)?;
    let sc = Scope {
        names: &env.names,
        module: &imp.module,
        at: ctx.here(),
    };
    let cty = resolve_ty_with_structs(cty, &env.defs, Some(&subst), sc);
    // The value is the impl's: its names resolve in the impl's module,
    // with the impl's type parameters and `Self` bound for `ty`.
    let saved_subst = std::mem::replace(&mut ctx.ty_subst, std::borrow::Cow::Owned(subst));
    let saved_module = std::mem::replace(&mut ctx.current_module, imp.module.clone());
    let saved_self = std::mem::replace(&mut ctx.self_ty, Some(ty.clone()));
    let saved_uses = std::mem::replace(&mut ctx.local_uses, vec![Vec::new()]);
    let saved_scopes = std::mem::replace(&mut ctx.scopes, vec![HashMap::default()]);
    // A trait's const may be defined by the inherent one of its name
    // (`const EPSILON: f64 = f64::EPSILON;`): not a cycle.
    let key = if imp.trait_key.is_empty() {
        format!("{}::{name}", ctx.ty_name(ty))
    } else {
        format!("<{} as {}>::{name}", ctx.ty_name(ty), imp.trait_key)
    };
    let span = ctx.here();
    let v = crate::lower::consteval::guard(ctx, &key, span, |ctx| {
        let v = lower_expected(ctx, value, cty.clone());
        coerce(ctx, &cty, v, &|| format!("associated const `{name}`"))
    });
    ctx.ty_subst = saved_subst;
    ctx.current_module = saved_module;
    ctx.self_ty = saved_self;
    ctx.local_uses = saved_uses;
    ctx.scopes = saved_scopes;
    // The use's span: diagnostics on it show `Vec3::X`, not its value.
    Some(v.map_or_else(
        || poison(span),
        |v| HirExpr {
            span,
            ..crate::lower::consteval::const_item(ctx, &key, v)
        },
    ))
}

/// The name a type's `Type::NAME` consts are keyed under.
pub(crate) fn const_owner(ctx: &LowerCtx<'_>, ty: &HirTy) -> Option<String> {
    match ty {
        HirTy::Struct(id) => Some(enum_base_name(&ctx.struct_def(*id).name).to_owned()),
        HirTy::Enum(id) => Some(enum_base_name(&ctx.enum_def(*id).name).to_owned()),
        HirTy::Prim(p) => Some(p.name().to_owned()),
        _ => None,
    }
}

/// The trait key of `<T as Tr>`'s `Tr`, `None` when it names no trait.
pub(crate) fn qpath_trait(ctx: &LowerCtx<'_>, q: &nabic_ast::QPath) -> Option<String> {
    let segs: Vec<&str> = q
        .trait_path
        .segments
        .iter()
        .map(|s| s.name.as_str())
        .collect();
    Some(
        ctx.env
            .names
            .trait_key(&segs, &ctx.current_module, ctx.here()),
    )
    .filter(|k| ctx.env.names.is_trait(k))
}

/// `<T as Tr>::N` (an associated const) or `<T as Tr>::m` (a method
/// value) not in call position.
pub(crate) fn lower_qpath_value(
    ctx: &mut LowerCtx<'_>,
    expr: &Expr,
    q: &nabic_ast::QPath,
    span: nabic_diagnostic::Span,
) -> HirExpr {
    if let Some(m) = &q.member {
        return ctx.error_expr(
            Code::WRONG_ITEM_FORM,
            span,
            format!(
                "`<.. as ..>::{}::{m}` without a call: write a closure `|..| <..>::{}::{m}(..)`",
                q.item, q.item
            ),
        );
    }
    let t = ctx.resolve_ast_ty(&q.self_ty);
    let Some(key) = qpath_trait(ctx, q) else {
        return ctx.error_expr(
            Code::NOT_A_TRAIT,
            span,
            format!(
                "`<{} as ..>::{}`: not a trait after `as`",
                ctx.ty_name(&t),
                q.item
            ),
        );
    };
    let want = crate::lower::resolve::trait_ref(
        &ctx.env.names,
        &q.trait_path,
        &ctx.current_module,
        ctx.here(),
        |t| ctx.resolve_ast_ty(t),
    );
    if let Some(v) = lower_assoc_const(ctx, &t, &q.item, Some(&[key]), &[want]) {
        return v;
    }
    if let Some(v) = crate::lower::methodpath::method_path_value(ctx, &expr.kind, span) {
        return v;
    }
    ctx.error_expr(
        Code::NO_METHOD,
        span,
        format!(
            "`<{} as {}>` has no associated const or method `{}`",
            ctx.ty_name(&t),
            q.trait_path
                .segments
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
                .join("::"),
            q.item
        ),
    )
}

/// `Type::N` / `Box::<i32>::N`, `T::N` / `Self::N`: an associated const
/// from the impl covering that type.
/// The owner of `Vec3::X` when `Vec3` is written without its defaulted
/// arguments: the instance the expected type names (`let d: DVec3 =
/// Vec3::X`), else the one impl defining `X`, else `None` (the default
/// instance, as a written type).
fn defaulted_owner(
    ctx: &LowerCtx<'_>,
    prefix: &[nabic_ast::PathSegment],
    segs: &[&str],
    name: &str,
) -> Option<HirTy> {
    if prefix.last().is_none_or(|s| !s.generics.is_empty()) {
        return None;
    }
    let full = ctx.canon_type(segs)?;
    let generics = ctx
        .env
        .defs
        .generic_struct(&full)
        .map(|d| &d.generics)
        .or_else(|| ctx.env.defs.generic_enum(&full).map(|d| &d.generics))?;
    let defaulted = generics.params.iter().any(|gp| {
        matches!(
            gp,
            nabic_ast::GenericParam::Type {
                default: Some(_),
                ..
            }
        )
    });
    if !defaulted {
        return None;
    }
    let base_of = |t: &HirTy| match t {
        HirTy::Struct(id) => ctx
            .struct_def(*id)
            .name
            .split("__")
            .next()
            .map(str::to_owned),
        HirTy::Enum(id) => ctx.enum_def(*id).name.split("__").next().map(str::to_owned),
        _ => None,
    };
    if let Some(want) = ctx.current_expected().cloned() {
        let t = crate::lower::infer::zonk(ctx, &want);
        if base_of(&t).as_deref() == Some(full.as_str()) {
            // An open literal argument takes the type it holds, for good.
            crate::lower::infer::unify(ctx, &want, &t);
            return Some(t);
        }
    }
    let mut hits = ctx
        .env
        .impls
        .with_const(name)
        .filter(|imp| imp.trait_key.is_empty())
        .filter_map(|imp| match &imp.concrete {
            Some((t, _)) => (base_of(t).as_deref() == Some(full.as_str())).then(|| Some(t.clone())),
            // A generic impl of the type: no one instance.
            None => match &imp.self_ty.kind {
                AstTyKind::Path(p)
                    if p.segments
                        .last()
                        .is_some_and(|s| s.name == leaf_name(&full)) =>
                {
                    Some(None)
                }
                _ => None,
            },
        });
    match (hits.next(), hits.next()) {
        (Some(Some(t)), None) => Some(t),
        _ => None,
    }
}

pub(crate) fn assoc_const_path(ctx: &mut LowerCtx<'_>, path: &AstPath) -> Option<HirExpr> {
    // `Type::N` / `Box::<i32>::N`: the impl covering that type.
    if let [prefix @ .., name] = path.segments.as_slice() {
        let segs: Vec<&str> = prefix.iter().map(|s| s.name.as_str()).collect();
        let owner = nabic_ast::Ty {
            kind: AstTyKind::Path(AstPath {
                segments: prefix.to_vec(),
                span: path.span,
                origin: None,
            }),
            span: path.span,
        };
        if !prefix.is_empty()
            && segs != ["Self"]
            && (ctx
                .canon_type(&segs)
                .is_some_and(|f| !ctx.env.names.is_trait(&f))
                || matches!(crate::ty::resolve_ty(&owner), HirTy::Prim(_)))
        {
            let ty = defaulted_owner(ctx, prefix, &segs, &name.name)
                .unwrap_or_else(|| ctx.resolve_ast_ty(&owner));
            if let Some(v) = lower_assoc_const(ctx, &ty, &name.name, None, &[]) {
                return Some(v);
            }
        }
    }
    // `T::N` / `Self::N` on a concrete type: the impl that covers it.
    if let [owner, name] = path.segments.as_slice() {
        let ty = match owner.name.as_str() {
            "Self" => ctx.self_ty.clone(),
            n => ctx
                .ty_subst
                .get(n)
                .or_else(|| ctx.explicit_generics.get(n))
                .cloned(),
        };
        // A type parameter reads the const of a trait it is bounded by.
        let bounds = ctx.ty_bounds.get(&owner.name).cloned();
        let wants = ctx.bound_wants(&owner.name);
        if let Some(v) = ty
            .filter(|_| owner.generics.is_empty())
            .and_then(|t| lower_assoc_const(ctx, &t, &name.name, bounds.as_deref(), &wants))
        {
            return Some(v);
        }
    }
    None
}

/// A read of variable `local`; of the element a `for ref` binding names.
pub(crate) fn read_var(
    ctx: &mut LowerCtx<'_>,
    local: LocalId,
    span: nabic_diagnostic::Span,
) -> HirExpr {
    match ctx.places.get(&local) {
        Some(place) => {
            let place = respan(place, span);
            lower_expected(ctx, &place, HirTy::Unknown)
        }
        None => ctx.read_local(local, span),
    }
}

/// `place` (an element place of `for ref`) placed at `span`.
pub(crate) fn respan(place: &Expr, span: nabic_diagnostic::Span) -> Expr {
    let kind = match &place.kind {
        ExprKind::Index(xs, i) => ExprKind::Index(Box::new(respan(xs, span)), i.clone()),
        k => k.clone(),
    };
    Expr { kind, span }
}

/// `e` with a `for ref` binding at its root (under fields and indexes)
/// replaced by the element place it names.
pub(crate) fn unplace(ctx: &LowerCtx<'_>, e: &Expr) -> Option<Expr> {
    let kind = match &e.kind {
        ExprKind::Path(p) if p.segments.len() == 1 => {
            let l = ctx.lookup(&p.segments[0].name)?;
            return ctx.places.get(&l).map(|place| respan(place, e.span));
        }
        ExprKind::Field(x, f) => ExprKind::Field(Box::new(unplace(ctx, x)?), f.clone()),
        ExprKind::TupleField(x, i) => ExprKind::TupleField(Box::new(unplace(ctx, x)?), *i),
        ExprKind::Index(x, i) => ExprKind::Index(Box::new(unplace(ctx, x)?), i.clone()),
        _ => return None,
    };
    Some(Expr { kind, span: e.span })
}

pub(crate) fn lower_path(
    ctx: &mut LowerCtx<'_>,
    path: &AstPath,
    span: nabic_diagnostic::Span,
) -> HirExpr {
    let aliased = match alias_path(ctx, path) {
        Ok(a) => a,
        Err(msg) => return ctx.error_expr(Code::GENERIC_ARGUMENTS, span, msg),
    };
    let path = aliased.as_ref().unwrap_or(path);
    let self_segs = self_path_segs(ctx, path);
    let segs: Vec<&str> = self_segs.iter().map(String::as_str).collect();
    if let [name] = segs.as_slice() {
        if let Some(h) = ctx.splices.get(*name) {
            return h.clone();
        }
        if let Some(local) = ctx.lookup(name) {
            return read_var(ctx, local, span);
        }
    }
    // A const generic parameter is a value.
    if let [name] = segs.as_slice() {
        if let Some((v, Some(p))) = ctx.ty_subst.get(*name).and_then(HirTy::as_const) {
            let value = match p {
                PrimTy::Bool => ConstVal::Bool(v != 0),
                PrimTy::Char => ConstVal::Char(u32::try_from(v).unwrap_or_default()),
                _ => ConstVal::Int { value: v, ty: p },
            };
            return HirExpr {
                kind: HirExprKind::Const(value),
                ty: HirTy::Prim(p),
                span,
            };
        }
    }
    // `const NAME: T = expr;` is inlined, typed by its declaration;
    // `T::N` / `Self::N` reads the concrete type's const.
    if let Some(v) = assoc_const_path(ctx, path) {
        return v;
    }
    let assoc = match path.segments.as_slice() {
        [owner, name] if owner.generics.is_empty() => {
            let ty = if owner.name == "Self" {
                ctx.self_ty.clone()
            } else {
                ctx.ty_subst
                    .get(&owner.name)
                    .or_else(|| ctx.explicit_generics.get(&owner.name))
                    .cloned()
            };
            ty.and_then(|t| const_owner(ctx, &t))
                .map(|o| format!("{o}::{}", name.name))
                .filter(|k| ctx.env.konst(k).is_some())
        }
        _ => None,
    };
    if let Some(full) = assoc.or_else(|| ctx.resolve_in(&segs, false)) {
        let written = full.clone();
        let full = ctx.env.names.value_reexport(&full).cloned().unwrap_or(full);
        ctx.check_reached_visible(&written, &full, "const");
        item_marked(ctx, "constant", &full, span);
        let (ty, value) = ctx.env.konst(&full).expect("a const").clone();
        // An impl const `m::T::N` resolves its type and value in `m`.
        let owner = module_of(&full);
        let owner_segs: Vec<&str> = owner.split("::").collect();
        let module = if ctx.canon_type(&owner_segs).as_deref() == Some(owner.as_str()) {
            module_of(&owner)
        } else {
            owner
        };
        let sc = Scope {
            names: &ctx.env.names,
            module: &module,
            at: ctx.here(),
        };
        let ty = resolve_ty_with_structs(&ty, &ctx.env.defs, None, sc);
        // The value's names (and field privacy) are the defining module's.
        let saved_module = std::mem::replace(&mut ctx.current_module, module);
        let saved_uses = std::mem::replace(&mut ctx.local_uses, vec![Vec::new()]);
        let saved_scopes = std::mem::replace(&mut ctx.scopes, vec![HashMap::default()]);
        let Some(v) = crate::lower::consteval::guard(ctx, &full, span, |ctx| {
            let v = lower_expected(ctx, &value, ty.clone());
            let shown = ctx.env.names.visible_path(&full).to_owned();
            coerce(ctx, &ty, v, &|| format!("const `{shown}`"))
        }) else {
            ctx.current_module = saved_module;
            ctx.local_uses = saved_uses;
            ctx.scopes = saved_scopes;
            return poison(span);
        };
        ctx.current_module = saved_module;
        ctx.local_uses = saved_uses;
        ctx.scopes = saved_scopes;
        return crate::lower::consteval::const_item(ctx, &full, v);
    }
    // Module-level `static`: a load through the global address.
    if let Some(full) = ctx.resolve_in(&segs, true) {
        let written = full.clone();
        let full = ctx.env.names.value_reexport(&full).cloned().unwrap_or(full);
        ctx.check_reached_visible(&written, &full, "static");
        item_marked(ctx, "static", &full, span);
        if !ctx.addr_place {
            extern_static_use(ctx, &full, false, span);
        }
        let ty = ctx.env.static_ty(&full).expect("a static").clone();
        return HirExpr {
            kind: HirExprKind::StaticRef(full),
            ty,
            span,
        };
    }
    // A function used as a value: a function pointer.
    if let Some(full) = ctx.resolve_fn_name(&segs) {
        ctx.check_item_visible(&full, "function");
        let sig = ctx.sig_of_key(&full);
        fn_marked(ctx, sig.id, span);
        if sig.param_refs.iter().any(|r| *r) {
            return ctx.error_expr(
                Code::REF_ARGUMENT,
                span,
                format!(
                    "`{}` has `ref` parameters and cannot be used as a `fn` value (a `fn` type \
                     has no `ref` parameters)",
                    ctx.env.names.visible_path(&full)
                ),
            );
        }
        return HirExpr {
            kind: HirExprKind::FnRef(sig.id),
            // A named function captures nothing: Send + Sync.
            ty: HirTy::Fn(sig.param_tys, Box::new(sig.ret_ty), {
                // An `unsafe fn` / C function value keeps its unsafety.
                let mut b = ctx.env.names.all_auto();
                b.unsafe_fn = ctx.env.is(crate::lower::FnSet::Unsafe, sig.id);
                b
            }),
            span,
        };
    }
    if let Some(full) = ctx.resolve_generic_fn_name(&segs) {
        ctx.check_item_visible(&full, "function");
        let def = ctx.env.generic_fn(&full).expect("a generic function");
        template_marked(ctx, def, span);
        // `id::<i32>`: every type argument given, that instance's value.
        let targs = path
            .segments
            .last()
            .map_or(&[][..], |s| s.generics.as_slice());
        // `let f: fn(i32) -> i32 = id;`: the receiving fn type picks the instance.
        let from_slot = if targs.is_empty() {
            generic_fn_args_from_slot(ctx, def)
        } else {
            None
        };
        if !targs.is_empty() || from_slot.is_some() {
            let bound = match from_slot {
                Some(b) => b,
                None => match bind_explicit(ctx, def, None, targs, &full) {
                    Ok(b) => b,
                    Err(msg) => return ctx.error_expr(Code::GENERIC_ARGUMENTS, span, msg),
                },
            };
            if bound.len() < def.generics.params.len() {
                return ctx.error_expr(
                    Code::GENERIC_ARGUMENTS,
                    span,
                    format!(
                        "`{}` as a value needs all {} generic argument(s)",
                        leaf_name(&full),
                        def.generics.params.len()
                    ),
                );
            }
            ctx.explicit_generics = bound;
            let mono = monomorphize(ctx, def, None, &[], span);
            ctx.explicit_generics.clear();
            let Some((id, ret)) = mono else {
                return poison(span);
            };
            let params = ctx
                .env
                .sink
                .mono_sigs
                .borrow()
                .get(&id)
                .map(|s| s.param_tys.clone())
                .unwrap_or_default();
            return HirExpr {
                kind: HirExprKind::FnRef(id),
                ty: HirTy::Fn(params, Box::new(ret), ctx.env.names.all_auto()),
                span,
            };
        }
        if let Some(v) = rethrows_value(ctx, def, span) {
            return v;
        }
        return ctx.error_expr(
            Code::TYPE_ANNOTATIONS_NEEDED,
            span,
            format!(
                "generic function `{}` as a value: its type arguments are not determined \
                 - write `{}::<..>` or give the slot a `fn(..)` type",
                segs.join("::"),
                segs.join("::")
            ),
        );
    }
    // `S::new` / `i32::add` / `Tr::m` without a call.
    let callee = ExprKind::Path(path.clone());
    if let Some(v) = crate::lower::methodpath::method_path_value(ctx, &callee, span) {
        return v;
    }
    // `EnumName::Variant` (unit form). Tuple variants come through
    // `lower_call`: the parser wraps them in a call.
    let hint = explicit_enum(ctx, path).or_else(|| ctx.current_expected().cloned());
    match resolve_enum_variant(ctx, &segs, hint.as_ref(), None) {
        EnumRes::Found(enum_id, idx) => {
            let variant = &ctx.enum_def(enum_id).variants[idx];
            if variant.payload_ty.is_some() {
                return ctx.error_expr(
                    Code::WRONG_ITEM_FORM,
                    span,
                    format!(
                        "enum variant `{}` takes arguments: `{}(...)`",
                        segs.join("::"),
                        segs.join("::")
                    ),
                );
            }
            return HirExpr {
                kind: HirExprKind::EnumVariantInit {
                    enum_id,
                    variant_index: u32::try_from(idx).expect("variant overflow"),
                    args: Vec::new(),
                },
                ty: HirTy::Enum(enum_id),
                span,
            };
        }
        EnumRes::Error => return poison(span),
        EnumRes::NotEnum => {}
    }
    // A unit struct `Unit` is its own value.
    if let Some(sid) = ctx
        .canon_type(&segs)
        .and_then(|full| ctx.lookup_struct(&full))
    {
        if ctx.env.duplicated(ctx.struct_def(sid).span) {
            return poison(span);
        }
        if ctx.struct_def(sid).fields.is_empty() {
            return HirExpr {
                kind: HirExprKind::StructInit {
                    struct_id: sid,
                    fields: Vec::new(),
                },
                ty: HirTy::Struct(sid),
                span,
            };
        }
    }
    // A generic unit struct (`M::<i64>`, or `M` in an `M<i64>` slot): a
    // struct literal with no fields, which infers the instantiation.
    let generic_unit = ctx.canon_type(&segs).is_some_and(|full| {
        ctx.lookup_struct(&full).is_none()
            && ctx
                .env
                .defs
                .generic_struct(&full)
                .is_some_and(|d| matches!(d.kind, nabic_ast::StructKind::Unit))
    });
    if generic_unit {
        return lower_struct_init(ctx, path, &[], None, span);
    }
    let top_let = ctx
        .env
        .main_lets
        .get(segs[0])
        .filter(|_| !ctx.implicit_main);
    if let (Some(konst), [name]) = (top_let, segs.as_slice()) {
        return ctx.unknown_name(
            Code::UNRESOLVED_NAME,
            span,
            format!(
                "cannot find value `{name}`: it is a top-level `let`, a variable of the \
                 program's statements, and a `fn` cannot see those"
            ),
            Some(format!(
                "make it a constant and use that name, `{konst}`, or pass `{name}` to the \
                 function as an argument"
            )),
        );
    }
    if let Some((msg, help)) = segs
        .first()
        .filter(|_| segs.len() == 1)
        .and_then(|n| ctx.variant_not_in_scope(n))
    {
        return ctx.unknown_name(Code::UNRESOLVED_VARIANT, span, msg, Some(help));
    }
    let help = ctx.value_help(&segs);
    ctx.unknown_name(
        Code::UNRESOLVED_NAME,
        span,
        format!("cannot find value `{}` in this scope", segs.join("::")),
        help,
    )
}
