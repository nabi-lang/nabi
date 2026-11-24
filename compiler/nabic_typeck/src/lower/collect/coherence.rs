//! Impl rules, overlap and auto trait impls.

#[allow(clippy::wildcard_imports)]
use crate::lower::collect::*;

/// One impl per trait and type (a concrete impl a generic one
/// covers is a duplicate); the orphan rule; supertraits (for
/// every instance a generic impl's bounds allow).
pub(crate) fn check_impl_rules(
    env: &Env<'_>,
    checked: &[&Program],
    all: &[&Program],
    own: &dyn Fn(&str) -> bool,
) {
    let no_subst = HashMap::default();
    let traits: HashMap<&str, &nabic_ast::TraitDef> = all
        .iter()
        .flat_map(|p| &p.items)
        .filter_map(|it| match it {
            Item::Trait(t) => Some((t.name.as_str(), t)),
            _ => None,
        })
        .collect();
    for item in checked.iter().flat_map(|p| &p.items) {
        let Item::Impl(i) = item else {
            continue;
        };
        let Some(tp) = &i.trait_path else {
            continue;
        };
        let module = impl_module(i);
        if !own(&module) {
            continue;
        }
        let segs: Vec<&str> = tp.segments.iter().map(|s| s.name.as_str()).collect();
        let key = env
            .names
            .trait_key(&segs, tp.origin.as_deref().unwrap_or(&module), i.span);
        let pkg = package_of(&module);
        let owner_module = module_of(&key);
        let owner = package_of(&owner_module);
        if owner != pkg
            && target_package(env, i, &module).as_deref() != Some(pkg)
            && !local_type_first(env, i, tp, &module, pkg)
        {
            env.sink.diagnostics.borrow_mut().push(
                nabic_diagnostic::NabiDiagnostic::error(
                    Code::ORPHAN_IMPL,
                    i.span,
                    format!(
                        "`impl {} for {}`: neither the trait nor the type is defined in this \
                         package (orphan rule)",
                        leaf_name(&key),
                        impl_def_display(&i.self_ty)
                    ),
                )
                .with_help(format!(
                    "wrap the type in one of this package's: `struct My{w}({t});` and \
                     `impl {} for My{w} {{ .. }}`",
                    leaf_name(&key),
                    t = impl_def_display(&i.self_ty),
                    w = leaf_name(&impl_def_display(&i.self_ty))
                        .split(['<', '(', ' '])
                        .next()
                        .map(|w| {
                            let mut c = w.chars();
                            c.next()
                                .map_or(String::new(), |f| f.to_uppercase().chain(c).collect())
                        })
                        .unwrap_or_default()
                )),
            );
            continue;
        }
        // The library's tiers are one package: core implements bare's
        // sealed traits for its own types (`Pinnable for Array`).
        let tiers = nabic_core::in_toolchain;
        if env.is_sealed(&key) && owner != pkg && !(tiers(owner) && tiers(pkg)) {
            env.sink.diagnostics.borrow_mut().push(
                nabic_diagnostic::NabiDiagnostic::error(
                    Code::INVALID_IMPL,
                    i.span,
                    format!(
                        "`impl {} for {}`: trait `{}` is sealed, so only `{}` implements it",
                        leaf_name(&key),
                        impl_def_display(&i.self_ty),
                        leaf_name(&key),
                        if owner.is_empty() {
                            "its package"
                        } else {
                            owner
                        },
                    ),
                )
                .with_note(
                    "a sealed trait still works in bounds and calls: use the types that \
                         already implement it",
                ),
            );
            continue;
        }
        if i.negative {
            continue;
        }
        let Some(info) = env.trait_info.get(&key) else {
            continue;
        };
        let mut ctx = LowerCtx::new(env, &no_subst, None, i.span);
        ctx.current_module.clone_from(&module);
        if !i.generics.params.is_empty() {
            let td = traits.get(key.as_str()).copied();
            check_generic_supers(&ctx, i, &module, &key, &info.supers, td);
            continue;
        }
        let target = ctx.resolve_ast_ty(&i.self_ty);
        if target == HirTy::Unknown {
            continue;
        }
        for sup in &info.supers {
            if ctx.has_trait(&target, sup) {
                continue;
            }
            ctx.emit_error(
                Code::MISSING_SUPERTRAIT,
                format!(
                    "`{}` implements `{}` but not its supertrait `{}`",
                    ctx.ty_name(&target),
                    leaf_name(&key),
                    leaf_name(sup)
                ),
            );
        }
        // An associated type left out is reported once, not again in
        // each signature that names it.
        let missing = traits.get(key.as_str()).is_some_and(|td| {
            td.items.iter().any(|ti| {
                matches!(ti, nabic_ast::TraitItem::AssocType { name, default: None, .. }
                    if !impl_assoc_types(i).contains_key(name))
            })
        });
        if !missing {
            check_impl_returns(&ctx, info, &key, i, &target);
        }
        if let Some(td) = traits.get(key.as_str()) {
            ctx.self_ty = Some(target);
            check_assoc_bounds(&ctx, td, i);
        }
    }
}

/// A concrete impl's method takes and returns the types the
/// trait declares, with `Self`, the trait's parameters and associated
/// types replaced by the impl's. Generic methods and unresolved types are left
/// to the call sites.
pub(crate) fn check_impl_returns(
    ctx: &LowerCtx<'_>,
    info: &TraitInfo,
    key: &str,
    i: &nabic_ast::ImplDef,
    target: &HirTy,
) {
    let Some(tp) = &i.trait_path else {
        return;
    };
    // Resolving again must not repeat what the impl's own lowering reports.
    let quiet = |f: &dyn Fn() -> HirTy| {
        let n = ctx.env.sink.diagnostics.borrow().len();
        let t = f();
        ctx.env.sink.diagnostics.borrow_mut().truncate(n);
        t
    };
    let args: Vec<HirTy> = tp
        .segments
        .last()
        .map(|s| {
            s.generics
                .iter()
                .map(|g| quiet(&|| ctx.resolve_ast_ty(g)))
                .collect()
        })
        .unwrap_or_default();
    if args.len() != info.params.len() {
        return;
    }
    let assoc = impl_assoc_types(i);
    let mut subst: HashMap<String, HirTy> = info.params.iter().cloned().zip(args).collect();
    for (name, ty) in &assoc {
        subst.insert(name.clone(), quiet(&|| ctx.resolve_ast_ty(ty)));
    }
    // A supertrait's associated types (`Item` in `DoubleEndedIterator`)
    // are the target's from its impl of that supertrait; one it lacks
    // stays unknown, its missing impl already reported.
    let mut supers = info.supers.clone();
    let mut seen: Vec<String> = Vec::new();
    while let Some(sup) = supers.pop() {
        if seen.contains(&sup) {
            continue;
        }
        let Some(si) = ctx.env.trait_info.get(&sup) else {
            continue;
        };
        for (name, _) in &si.assoc {
            if subst.contains_key(name) {
                continue;
            }
            let ty = quiet(&|| {
                ctx.project(target, name, Some(std::slice::from_ref(&sup)), &[])
                    .unwrap_or(HirTy::Unknown)
            });
            subst.insert(name.clone(), ty);
        }
        supers.extend(si.supers.iter().cloned());
        seen.push(sup);
    }
    let unit = |t: &Option<Ty>| {
        t.as_ref()
            .is_none_or(|t| matches!(&t.kind, AstTyKind::Tuple(v) if v.is_empty()))
    };
    for it in &i.items {
        let AstImplItem::Method(m) = it else {
            continue;
        };
        let leaf = leaf_name(&m.name);
        let Some(tm) = info.decls.get(leaf) else {
            continue;
        };
        if !m.generics.params.is_empty() || !tm.generics.params.is_empty() {
            continue;
        }
        let mut def = m.clone();
        subst_assoc_sig(&mut def, &assoc);
        let mine_of = |t: &Ty| {
            quiet(&|| {
                let mut rc = LowerCtx::new(ctx.env, &subst, Some(target.clone()), m.span);
                rc.current_module.clone_from(&ctx.current_module);
                rc.resolve_ast_ty(t)
            })
        };
        let theirs_of = |t: &Ty| {
            quiet(&|| {
                let mut rc = LowerCtx::new(ctx.env, &subst, Some(target.clone()), m.span);
                rc.current_module.clone_from(&info.module);
                let mut t = t.clone();
                expr::strip_self_assoc(&mut t);
                rc.resolve_ast_ty(&t)
            })
        };
        let differ = |got: &HirTy, want: &HirTy| {
            got != want && !mono::has_unknown(got) && !mono::has_unknown(want)
        };
        // Written parameter types are the trait's; an untyped
        // one takes the trait's.
        if def.params.len() == tm.params.len() {
            let bad = def.params.iter().zip(&tm.params).find_map(|(p, tp)| {
                if p.name == "self" {
                    return None;
                }
                let (got, want) = (mine_of(p.ty.as_ref()?), theirs_of(tp.ty.as_ref()?));
                differ(&got, &want).then_some((p, got, want))
            });
            if let Some((p, got, want)) = bad {
                ctx.env.sink.diagnostics.borrow_mut().push(
                    nabic_diagnostic::NabiDiagnostic::error(
                        Code::IMPL_MISMATCH,
                        p.span,
                        format!(
                            "`{leaf}` does not match its declaration in trait `{}`: parameter \
                             `{}` is `{}` where the trait's is `{}`",
                            leaf_name(key),
                            p.name,
                            ctx.ty_name(&got),
                            ctx.ty_name(&want)
                        ),
                    )
                    .with_help(format!(
                        "write `{}: {}`, the trait's `{}`",
                        p.name,
                        ctx.ty_name(&want),
                        nabic_ast::print::print_fn_sig(tm)
                    )),
                );
                continue;
            }
        }
        let (Some(mine), Some(theirs)) = (&m.ret_ty, &tm.ret_ty) else {
            continue;
        };
        if unit(&m.ret_ty) || unit(&tm.ret_ty) {
            continue;
        }
        let got = mine_of(def.ret_ty.as_ref().unwrap_or(mine));
        let want = theirs_of(theirs);
        if !differ(&got, &want) {
            continue;
        }
        ctx.env.sink.diagnostics.borrow_mut().push(
            nabic_diagnostic::NabiDiagnostic::error(
                Code::IMPL_MISMATCH,
                m.span,
                format!(
                    "`{leaf}` does not match its declaration in trait `{}`: it returns `{}` \
                     where the trait's returns `{}`",
                    leaf_name(key),
                    ctx.ty_name(&got),
                    ctx.ty_name(&want)
                ),
            )
            .with_help(format!(
                "write `-> {}`, the trait's `{}`",
                ctx.ty_name(&want),
                nabic_ast::print::print_fn_sig(tm)
            )),
        );
    }
}

/// The bounds a trait declares on its associated types
/// (`type Label: Display;`), as `(name, the impl's type, bound key)`.
pub(crate) fn assoc_bound_goals<'i>(
    ctx: &LowerCtx<'_>,
    td: &nabic_ast::TraitDef,
    i: &'i nabic_ast::ImplDef,
) -> Vec<(String, &'i Ty, String)> {
    let trait_module = module_of(&td.name);
    let mut out = Vec::new();
    for item in &td.items {
        let nabic_ast::TraitItem::AssocType { name, bounds, .. } = item else {
            continue;
        };
        let Some(ty) = i.items.iter().find_map(|it| match it {
            AstImplItem::AssocType { name: n, ty } if n == name => Some(ty),
            _ => None,
        }) else {
            continue;
        };
        for b in bounds {
            if let AstTyKind::Path(bp) = &b.kind {
                let segs: Vec<&str> = bp.segments.iter().map(|s| s.name.as_str()).collect();
                let key = ctx.env.names.trait_key(&segs, &trait_module, ctx.here());
                out.push((name.clone(), ty, key));
            }
        }
    }
    out
}

pub(crate) fn assoc_bound_error(
    ctx: &LowerCtx<'_>,
    td: &str,
    name: &str,
    ty: &str,
    bound: &str,
    every: &str,
) {
    ctx.emit_error(
        Code::UNSATISFIED_BOUND,
        format!(
            "associated type `{name}` = `{ty}` does not implement `{}`{every}, a bound of \
             `{}::{name}`",
            leaf_name(bound),
            leaf_name(td)
        ),
    );
}

/// A concrete impl's associated types meet their bounds.
pub(crate) fn check_assoc_bounds(
    ctx: &LowerCtx<'_>,
    td: &nabic_ast::TraitDef,
    i: &nabic_ast::ImplDef,
) {
    for (name, ty, bound) in assoc_bound_goals(ctx, td, i) {
        let t = ctx.resolve_ast_ty(ty);
        if !matches!(t, HirTy::Unknown) && !ctx.has_trait(&t, &bound) {
            assoc_bound_error(ctx, &td.name, &name, &ctx.ty_name(&t), &bound, "");
        }
    }
}

/// For a generic impl: its target, parameters opaque under their
/// bounds, must implement each supertrait.
pub(crate) fn check_generic_supers(
    ctx: &LowerCtx<'_>,
    i: &nabic_ast::ImplDef,
    module: &str,
    key: &str,
    supers: &[String],
    td: Option<&nabic_ast::TraitDef>,
) {
    let vars: HashMap<String, String> = i
        .generics
        .params
        .iter()
        .map(|gp| match gp {
            nabic_ast::GenericParam::Type { name, .. }
            | nabic_ast::GenericParam::Const { name, .. } => (name.clone(), format!("#r.{name}")),
        })
        .collect();
    let Some(target) = canon_ast_ty(ctx, module, &vars, &i.self_ty) else {
        return;
    };
    let target = rigid(&target);
    let mut assume: HashMap<String, Vec<String>> = HashMap::default();
    let mut add = |subject: &Ty, bounds: &[Ty]| {
        let AstTyKind::Path(p) = &subject.kind else {
            return;
        };
        let [seg] = p.segments.as_slice() else {
            return;
        };
        let Some(v) = vars.get(&seg.name) else {
            return;
        };
        let have = assume.entry(v.clone()).or_default();
        for b in bounds {
            if let AstTyKind::Path(bp) = &b.kind {
                let segs: Vec<&str> = bp.segments.iter().map(|s| s.name.as_str()).collect();
                have.push(ctx.env.names.trait_key(&segs, module, ctx.here()));
            }
        }
    };
    for gp in &i.generics.params {
        if let nabic_ast::GenericParam::Type { name, bounds, .. } = gp {
            add(&path_ty_named(name, i.span), bounds);
        }
    }
    for w in &i.where_clause {
        add(&w.ty, &w.bounds);
    }
    let params = i
        .generics
        .params
        .iter()
        .map(|gp| match gp {
            nabic_ast::GenericParam::Type { name, .. }
            | nabic_ast::GenericParam::Const { name, .. } => name.as_str(),
        })
        .collect::<Vec<_>>()
        .join(", ");
    for sup in supers {
        if entails(ctx, &target, sup, &assume, 0) {
            continue;
        }
        ctx.emit_error(
            Code::MISSING_SUPERTRAIT,
            format!(
                "`{}` implements `{}` but not its supertrait `{}` for every `{params}` the \
                 impl allows",
                target.to_string().replace("#r.", ""),
                leaf_name(key),
                leaf_name(sup),
            ),
        );
    }
    for (name, ty, bound) in td
        .map(|td| assoc_bound_goals(ctx, td, i))
        .unwrap_or_default()
    {
        let Some(t) = canon_ast_ty(ctx, module, &vars, ty) else {
            continue;
        };
        let t = rigid(&t);
        if !entails(ctx, &t, &bound, &assume, 0) {
            let every = format!(" for every `{params}` the impl allows");
            let shown = t.to_string().replace("#r.", "");
            assoc_bound_error(ctx, key, &name, &shown, &bound, &every);
        }
    }
}

/// Impl parameters as opaque named types (`#r.T`), so unification with
/// another impl's target can bind its variables to them but not them.
pub(crate) fn rigid(t: &CanonTy) -> CanonTy {
    match t {
        CanonTy::Var(v) => CanonTy::Named(v.clone(), Vec::new()),
        CanonTy::Named(n, a) => CanonTy::Named(n.clone(), a.iter().map(rigid).collect()),
        CanonTy::Tuple(a) => CanonTy::Tuple(a.iter().map(rigid).collect()),
        CanonTy::Sum(a) => CanonTy::Sum(a.iter().map(rigid).collect()),
        CanonTy::Array(e, n) => CanonTy::Array(Box::new(rigid(e)), Box::new(rigid(n))),
        // Its `#r.` terms are never bound.
        CanonTy::Const(l) => CanonTy::Const(l.clone()),
    }
}

pub(crate) fn has_rigid(t: &CanonTy) -> bool {
    match t {
        CanonTy::Var(_) => false,
        CanonTy::Named(n, a) => n.starts_with("#r.") || a.iter().any(has_rigid),
        CanonTy::Tuple(a) | CanonTy::Sum(a) => a.iter().any(has_rigid),
        CanonTy::Array(e, n) => has_rigid(e) || has_rigid(n),
        CanonTy::Const(l) => l.terms.keys().any(|v| v.starts_with("#r.")),
    }
}

/// Does `subject` implement `key` for any opaque `#r.T`s given their bounds
/// `assume`? Opaque types have their bounds and supertraits; others need a
/// unifying impl whose bounds hold. Structural rules and unreadable types
/// are taken as met.
pub(crate) fn entails(
    ctx: &LowerCtx<'_>,
    subject: &CanonTy,
    key: &str,
    assume: &HashMap<String, Vec<String>>,
    depth: u32,
) -> bool {
    if depth > 8
        || key == crate::lower::reflect::REFLECT
        || ctx.env.names.auto_traits.contains(key)
        || !has_rigid(subject)
    {
        return true;
    }
    if let CanonTy::Named(n, args) = subject {
        if args.is_empty() && n.starts_with("#r.") {
            let mut stack: Vec<String> = assume.get(n).cloned().unwrap_or_default();
            let mut seen: Vec<String> = Vec::new();
            while let Some(h) = stack.pop() {
                if h == key {
                    return true;
                }
                if let (false, Some(info)) = (seen.contains(&h), ctx.env.trait_info.get(&h)) {
                    stack.extend(info.supers.iter().cloned());
                }
                seen.push(h);
            }
            return false;
        }
    }
    if matches!(subject, CanonTy::Sum(_)) {
        return true;
    }
    ctx.env
        .impls
        .generic()
        .filter(|gi| gi.trait_key == key)
        .any(|gi| {
            let vars: HashMap<String, String> = gi
                .generics
                .params
                .iter()
                .map(|gp| match gp {
                    nabic_ast::GenericParam::Type { name, .. }
                    | nabic_ast::GenericParam::Const { name, .. } => {
                        (name.clone(), format!("#c.{name}"))
                    }
                })
                .collect();
            let Some(target) = canon_ast_ty(ctx, &gi.module, &vars, &gi.self_ty) else {
                return true;
            };
            let mut u = Unifier::default();
            if u.unify(&target, subject).is_none() {
                return false;
            }
            let mut goals: Vec<(CanonTy, &Vec<Ty>)> = Vec::new();
            for gp in &gi.generics.params {
                if let nabic_ast::GenericParam::Type { name, bounds, .. } = gp {
                    goals.push((u.resolve(&vars[name]), bounds));
                }
            }
            for w in &gi.where_clause {
                match canon_ast_ty(ctx, &gi.module, &vars, &w.ty) {
                    Some(t) => goals.push((u.walk(&t), &w.bounds)),
                    None => return true,
                }
            }
            goals.iter().all(|(t, bounds)| {
                bounds.iter().all(|b| {
                    let AstTyKind::Path(bp) = &b.kind else {
                        return true;
                    };
                    let segs: Vec<&str> = bp.segments.iter().map(|s| s.name.as_str()).collect();
                    let k = ctx.env.names.trait_key(&segs, &gi.module, ctx.here());
                    entails(ctx, t, &k, assume, depth + 1)
                })
            })
        })
}

/// The package defining an impl's target type: a struct / enum's own,
/// `core` for a built-in type, `None` for a bare type parameter.
pub(crate) fn target_package(
    env: &Env<'_>,
    i: &nabic_ast::ImplDef,
    module: &str,
) -> Option<String> {
    if let AstTyKind::Path(p) = &i.self_ty.kind {
        let segs: Vec<&str> = p.segments.iter().map(|s| s.name.as_str()).collect();
        let param = i.generics.params.iter().any(
            |gp| matches!(gp, nabic_ast::GenericParam::Type { name, .. } if segs == [name.as_str()]),
        );
        if param {
            return None;
        }
        if let Some(full) = env.names.canon(&segs, module, &[], i.span) {
            return Some(package_of(&module_of(&full)).to_owned());
        }
    }
    Some("core".to_owned())
}

/// A foreign trait's impl is this package's when, among `Self` and the
/// trait's type arguments in that order, a type defined here comes
/// before any bare type parameter (Rust's rule): `impl Mul<Matrix> for
/// f64` is, `impl<T> Mul<Matrix> for T` is not, since another package
/// could write the same impl for its own `T`.
pub(crate) fn local_type_first(
    env: &Env<'_>,
    i: &nabic_ast::ImplDef,
    tp: &nabic_ast::Path,
    module: &str,
    pkg: &str,
) -> bool {
    let args = tp
        .segments
        .last()
        .map_or(&[][..], |s| s.generics.as_slice());
    for t in std::iter::once(&i.self_ty).chain(args) {
        let AstTyKind::Path(p) = &t.kind else {
            continue;
        };
        let segs: Vec<&str> = p.segments.iter().map(|s| s.name.as_str()).collect();
        let param = i.generics.params.iter().any(
            |gp| matches!(gp, nabic_ast::GenericParam::Type { name, .. } if segs == [name.as_str()]),
        );
        if param {
            return false;
        }
        if let Some(full) = env.names.canon(&segs, module, &[], i.span) {
            if package_of(&module_of(&full)) == pkg {
                return true;
            }
        }
    }
    false
}

pub(crate) fn check_impl_overlap(env: &Env<'_>, own: &dyn Fn(&str) -> bool) {
    let no_subst = HashMap::default();
    let ctx = LowerCtx::new(env, &no_subst, None, Span::DUMMY);
    let impls = &env.impls;
    let mine: Vec<bool> = impls.iter().map(|i| own(&i.module)).collect();
    for i in impls.iter().zip(&mine).filter(|(_, m)| **m).map(|(i, _)| i) {
        unknown_trait_args(&ctx, i);
    }
    for (g, gi) in impls.iter().enumerate().filter(|(_, i)| i.is_generic()) {
        ctx.at.set(gi.self_ty.span);
        let concrete = impls.by_trait.get(&gi.trait_key).into_iter().flatten();
        for &c in concrete {
            let ci = &impls.all[c];
            let Some((ty, _)) = &ci.concrete else {
                continue;
            };
            if !mine[g] && !mine[c] {
                continue;
            }
            if ctx
                .generic_impl_subst(gi, ty, 0)
                .is_some_and(|s| trait_args_overlap(&ctx, gi, &s, ty))
            {
                ctx.emit_error_at(
                    Code::CONFLICTING_IMPLS,
                    gi.self_ty.span,
                    format!(
                        "conflicting implementations of trait `{}` for `{}`: the concrete \
                         impl overlaps `impl<..> {} for {}` (one impl per \
                         trait and type; no specialization)",
                        ctx.display_path(&gi.trait_key),
                        ctx.ty_name(ty),
                        ctx.display_path(&gi.trait_key),
                        impl_def_display(&gi.self_ty)
                    ),
                );
                let t = ctx.ty_name(ty);
                ctx.help_last(Some(format!(
                    "put the `{t}` case in the generic impl: `if Self is {t} {{ .. }}`"
                )));
            }
        }
    }
    // Two generic impls conflict when their targets (and trait arguments)
    // unify; bounds don't separate them (`impl<T: A> X for T` and
    // `impl<T: B> X for T` clash). Beyond `symbolic_overlap`, look for a
    // witness among the program's types.
    let gis: Vec<(&Impl, bool)> = impls
        .iter()
        .zip(&mine)
        .filter(|(i, _)| i.is_generic())
        .map(|(i, &m)| (i, m))
        .collect();
    let mut candidates: Option<Vec<HirTy>> = None;
    for (i, &(a, a_mine)) in gis.iter().enumerate() {
        // Inherent ones (empty key) clash when they share a method name.
        let shared = |b: &Impl| {
            let mut m: Vec<&String> = a
                .methods
                .keys()
                .filter(|k| b.methods.contains_key(*k))
                .collect();
            m.sort();
            m.first().map(|s| (*s).clone())
        };
        for &(b, _) in gis.iter().skip(i + 1).filter(|(b, b_mine)| {
            b.trait_key == a.trait_key
                && (a_mine || *b_mine)
                && (!a.trait_key.is_empty() || shared(b).is_some())
        }) {
            ctx.at.set(b.self_ty.span);
            let witness = symbolic_overlap(&ctx, a, b).unwrap_or_else(|| {
                let cands = candidates.get_or_insert_with(|| named_types(env));
                cands
                    .iter()
                    .find(|t| {
                        ctx.generic_impl_subst(a, t, 0).is_some()
                            && ctx.generic_impl_subst(b, t, 0).is_some()
                    })
                    .map(|t| ctx.ty_name(t))
            });
            if let (Some(t), Some(m)) = (
                &witness,
                a.trait_key.is_empty().then(|| shared(b)).flatten(),
            ) {
                ctx.emit_error_at(
                    Code::CONFLICTING_IMPLS,
                    b.self_ty.span,
                    format!(
                        "duplicate method `{m}`: `impl<..> {}` and `impl<..> {}` both define it \
                         for `{t}` (bounds don't make impls disjoint)",
                        impl_def_display(&a.self_ty),
                        impl_def_display(&b.self_ty),
                    ),
                );
            } else if let Some(t) = witness {
                // With its arguments: `Tilde<D>` and `Tilde<Iid<D>>`.
                let head = |gi: &Impl| {
                    let args: Vec<String> = gi.trait_args.iter().map(impl_def_display).collect();
                    let path = ctx.display_path(&gi.trait_key);
                    if args.is_empty() {
                        path
                    } else {
                        format!("{path}<{}>", args.join(", "))
                    }
                };
                ctx.emit_error_at(
                    Code::CONFLICTING_IMPLS,
                    b.self_ty.span,
                    format!(
                        "conflicting implementations of trait `{}`: `impl<..> {} for {}` and \
                         `impl<..> {} for {}` overlap on `{t}` - bounds don't make impls \
                         disjoint (no specialization)",
                        ctx.display_path(&a.trait_key),
                        head(a),
                        impl_def_display(&a.self_ty),
                        head(b),
                        impl_def_display(&b.self_ty),
                    ),
                );
            }
        }
    }
}

/// `impl Mul<Nope> for M`: a trait argument naming no type. Calls read
/// the arguments only when they pick an impl, so nothing else says so.
fn unknown_trait_args(ctx: &LowerCtx<'_>, i: &Impl) {
    let params: Vec<&str> = i
        .generics
        .params
        .iter()
        .map(|gp| match gp {
            nabic_ast::GenericParam::Type { name, .. }
            | nabic_ast::GenericParam::Const { name, .. } => name.as_str(),
        })
        .collect();
    let mut unknown = Vec::new();
    for t in &i.trait_args {
        unknown_paths(ctx, &i.module, &params, t, &mut unknown);
    }
    let names = &ctx.env.names;
    for (n, at) in unknown {
        let d = nabic_diagnostic::NabiDiagnostic::error(
            Code::UNRESOLVED_TYPE,
            at,
            format!("cannot find type `{n}` in this scope"),
        );
        let help = (!n.contains("::"))
            .then(|| names.import_help(&i.module, names.type_leaf(&n), |k| names.type_vis(k)))
            .flatten();
        ctx.emit_diag(match help {
            Some(h) => d.with_help(h),
            None => d,
        });
    }
}

/// The paths in `t` that name neither a type, a parameter nor a const.
fn unknown_paths(
    ctx: &LowerCtx<'_>,
    module: &str,
    params: &[&str],
    t: &Ty,
    out: &mut Vec<(String, Span)>,
) {
    if let AstTyKind::Path(p) = &t.kind {
        let segs: Vec<&str> = p.segments.iter().map(|s| s.name.as_str()).collect();
        let names = &ctx.env.names;
        let known = params.contains(&segs[0])
            || segs[0] == "Self"
            || (segs.len() == 1 && crate::lower::PRIM_NAMES.contains(&segs[0]))
            || names.canon(&segs, module, &[], t.span).is_some()
            || names.const_key(&segs, module).is_some();
        if !known {
            out.push((segs.join("::"), t.span));
        }
    }
    let mut rec = |t: &Ty| unknown_paths(ctx, module, params, t, out);
    match &t.kind {
        AstTyKind::Path(p) => {
            p.segments.iter().flat_map(|s| &s.generics).for_each(rec);
        }
        AstTyKind::Sum(ts) | AstTyKind::Tuple(ts) => ts.iter().for_each(rec),
        AstTyKind::Nest(t) | AstTyKind::Binding(_, t) | AstTyKind::Array(t, _) => rec(t),
        AstTyKind::Fn(ps, r, _) => {
            ps.iter().for_each(&mut rec);
            rec(r);
        }
        AstTyKind::Throws { ok, err } => {
            rec(ok);
            rec(err);
        }
        _ => {}
    }
}

/// Does generic `gi`, matched on `ty`, also match a concrete impl's trait
/// arguments? A parameter only the trait arguments name is open: assume
/// overlap.
pub(crate) fn trait_args_overlap(
    ctx: &LowerCtx<'_>,
    gi: &Impl,
    subst: &HashMap<String, HirTy>,
    ty: &HirTy,
) -> bool {
    let open = gi.generics.params.iter().any(|gp| match gp {
        nabic_ast::GenericParam::Type { name, .. }
        | nabic_ast::GenericParam::Const { name, .. } => !subst.contains_key(name),
    });
    if gi.trait_args.is_empty() {
        return true;
    }
    if open {
        // A parameter only the trait arguments name: they still differ
        // where a named type heads both (`DotDiv<Array<T, N>>` against
        // `DotDiv<V>`).
        return ctx
            .concrete_impls(ty)
            .filter(|r| r.path == gi.trait_key)
            .any(|r| trait_heads_may_match(ctx, gi, &r.args));
    }
    let want = ctx.generic_impl_ref(gi, subst);
    if want.args.contains(&HirTy::Unknown) {
        return true;
    }
    ctx.concrete_impls(ty)
        .any(|r| r.path == want.path && r.args == want.args)
}

/// `false` when some trait argument of `gi` names a type whose head
/// differs from the concrete argument's.
pub(crate) fn trait_heads_may_match(ctx: &LowerCtx<'_>, gi: &Impl, args: &[HirTy]) -> bool {
    let params: Vec<&str> = gi
        .generics
        .params
        .iter()
        .map(|gp| match gp {
            nabic_ast::GenericParam::Type { name, .. }
            | nabic_ast::GenericParam::Const { name, .. } => name.as_str(),
        })
        .collect();
    for (t, have) in gi.trait_args.iter().zip(args) {
        let mut t = std::borrow::Cow::Borrowed(t);
        for _ in 0..32 {
            match ctx.env.names.expand_alias_quiet(&t, &gi.module) {
                Some(x) => t = std::borrow::Cow::Owned(x),
                None => break,
            }
        }
        let AstTyKind::Path(p) = &t.kind else {
            continue;
        };
        let segs: Vec<&str> = p.segments.iter().map(|s| s.name.as_str()).collect();
        if segs.len() == 1 && params.contains(&segs[0]) {
            continue;
        }
        let Some(want) = ctx.env.names.canon(&segs, &gi.module, &[], t.span) else {
            continue;
        };
        let head = match have {
            HirTy::Struct(id) => enum_base_name(&ctx.struct_def(*id).name).to_owned(),
            HirTy::Enum(id) => enum_base_name(&ctx.enum_def(*id).name).to_owned(),
            HirTy::Prim(p) => p.name().to_owned(),
            _ => continue,
        };
        if want != head && leaf_name(&want) != head {
            return false;
        }
    }
    true
}

/// Concrete impl targets and generic struct / enum instantiations: the
/// witnesses for impl targets `symbolic_overlap` can't read.
pub(crate) fn named_types(env: &Env<'_>) -> Vec<HirTy> {
    let mut out: Vec<HirTy> = Vec::new();
    let named = env.impls.all.iter().filter_map(|i| match &i.concrete {
        Some((t, Some(_))) => Some(t.clone()),
        _ => None,
    });
    let structs: Vec<HirTy> = env
        .defs
        .structs
        .borrow()
        .iter()
        .filter(|s| !s.generic_args.is_empty())
        .map(|s| HirTy::Struct(s.id))
        .collect();
    let enums: Vec<HirTy> = env
        .defs
        .enums
        .borrow()
        .iter()
        .filter(|e| !e.generic_args.is_empty())
        .map(|e| HirTy::Enum(e.id))
        .collect();
    for t in named.chain(structs).chain(enums) {
        if !out.contains(&t) {
            out.push(t);
        }
    }
    out
}

/// Do two generic impls overlap? Unify targets and trait arguments over
/// impl parameters. Bounds left generic are assumed satisfiable
/// (open-world); bounds made concrete are checked. `Some(Some(shown))`
/// overlaps, `Some(None)` disjoint, `None` unreadable (fn types, aliases).
#[allow(clippy::option_option)] // overlap / disjoint / can't tell
pub(crate) fn symbolic_overlap(ctx: &LowerCtx<'_>, a: &Impl, b: &Impl) -> Option<Option<String>> {
    let names = |gi: &Impl, tag: &str| -> HashMap<String, String> {
        gi.generics
            .params
            .iter()
            .map(|gp| match gp {
                nabic_ast::GenericParam::Type { name, .. }
                | nabic_ast::GenericParam::Const { name, .. } => {
                    (name.clone(), format!("{tag}{name}"))
                }
            })
            .collect()
    };
    let (va, vb) = (names(a, "#a."), names(b, "#b."));
    let canon =
        |gi: &Impl, vars: &HashMap<String, String>, t: &Ty| canon_ast_ty(ctx, &gi.module, vars, t);
    let a_self = canon(a, &va, &a.self_ty)?;
    let b_self = canon(b, &vb, &b.self_ty)?;
    if a.trait_args.len() != b.trait_args.len() {
        return Some(None);
    }
    let mut args = Vec::new();
    for (x, y) in a.trait_args.iter().zip(&b.trait_args) {
        args.push((canon(a, &va, x)?, canon(b, &vb, y)?));
    }
    let mut u = Unifier::default();
    for (gi, tag) in [(a, "#a."), (b, "#b.")] {
        for gp in &gi.generics.params {
            if let nabic_ast::GenericParam::Const { name, ty } = gp {
                if is_unsigned(ty) {
                    u.nonneg.insert(format!("{tag}{name}"));
                }
            }
        }
    }
    if u.unify(&a_self, &b_self).is_none() || args.iter().any(|(x, y)| u.unify(x, y).is_none()) {
        return Some(None);
    }
    // Negative reasoning only on known constructors (`List<T>: Iterator`
    // fails if no impl targets `List`); a bare variable is open.
    for (gi, vars) in [(a, &va), (b, &vb)] {
        let mut goals: Vec<(CanonTy, &Vec<Ty>)> = Vec::new();
        for gp in &gi.generics.params {
            if let nabic_ast::GenericParam::Type { name, bounds, .. } = gp {
                goals.push((u.resolve(&vars[name]), bounds));
            }
        }
        for w in &gi.where_clause {
            if let Some(t) = canon(gi, vars, &w.ty) {
                goals.push((u.walk(&t), &w.bounds));
            }
        }
        for (subject, bounds) in goals {
            if matches!(subject, CanonTy::Var(_)) {
                continue;
            }
            for bound in bounds {
                let AstTyKind::Path(bp) = &bound.kind else {
                    continue;
                };
                if bp.segments.iter().any(|s| !s.generics.is_empty()) {
                    continue;
                }
                let segs: Vec<&str> = bp.segments.iter().map(|s| s.name.as_str()).collect();
                let key = ctx.env.names.trait_key(&segs, &gi.module, ctx.here());
                if !may_implement(ctx, &subject, &key) {
                    return Some(None);
                }
            }
        }
    }
    Some(Some(u.walk(&a_self).to_string()))
}

pub(crate) fn is_unsigned(t: &Ty) -> bool {
    let AstTyKind::Path(p) = &t.kind else {
        return false;
    };
    matches!(
        p.segments.as_slice(),
        [s] if matches!(s.name.as_str(), "usize" | "u8" | "u16" | "u32" | "u64" | "u128")
    )
}

/// May non-variable `subject` implement `key`? Concrete types are asked
/// directly; others need an impl targeting their constructor or a bare
/// parameter.
pub(crate) fn may_implement(ctx: &LowerCtx<'_>, subject: &CanonTy, key: &str) -> bool {
    if let Some(t) = canon_hir(ctx, subject) {
        return ctx.has_trait(&t, key);
    }
    let head = canon_head(subject);
    let env = ctx.env;
    let concrete = env.impls.tys_of(key).any(|t| {
        let h = match t {
            HirTy::Struct(id) => enum_base_name(&ctx.struct_def(*id).name).to_owned(),
            HirTy::Enum(id) => enum_base_name(&ctx.enum_def(*id).name).to_owned(),
            HirTy::Tuple(ts) => format!("({})", ts.len()),
            HirTy::Array(..) => "[]".to_owned(),
            HirTy::Sum(_) => "|".to_owned(),
            _ => return false,
        };
        h == head
    });
    concrete
        || env
            .impls
            .generic()
            .filter(|gi| gi.trait_key == key)
            .any(|gi| {
                let vars = gi
                    .generics
                    .params
                    .iter()
                    .filter_map(|gp| match gp {
                        nabic_ast::GenericParam::Type { name, .. } => {
                            Some((name.clone(), format!("#c.{name}")))
                        }
                        nabic_ast::GenericParam::Const { .. } => None,
                    })
                    .collect();
                match canon_ast_ty(ctx, &gi.module, &vars, &gi.self_ty) {
                    Some(CanonTy::Var(_)) | None => true,
                    Some(t) => canon_head(&t) == head,
                }
            })
}

/// A type's constructor for `may_implement`: a path's name, `(n)` for an
/// n-tuple, `[]`, `|`.
pub(crate) fn canon_head(t: &CanonTy) -> String {
    match t {
        CanonTy::Named(n, _) => n.clone(),
        CanonTy::Tuple(ts) => format!("({})", ts.len()),
        CanonTy::Array(..) => "[]".to_owned(),
        CanonTy::Sum(_) => "|".to_owned(),
        CanonTy::Var(_) | CanonTy::Const(_) => String::new(),
    }
}

/// A type for `symbolic_overlap`: paths by canonical name, impl
/// parameters as variables.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum CanonTy {
    Var(String),
    Named(String, Vec<CanonTy>),
    Tuple(Vec<CanonTy>),
    /// Element and length (a `Const` or a const parameter).
    Array(Box<CanonTy>, Box<CanonTy>),
    Sum(Vec<CanonTy>),
    /// A const argument that isn't a lone parameter.
    Const(Lin),
}

std::thread_local! {
    static ALIAS_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

pub(crate) fn canon_ast_ty(
    ctx: &LowerCtx<'_>,
    module: &str,
    vars: &HashMap<String, String>,
    t: &Ty,
) -> Option<CanonTy> {
    let rec = |t: &Ty| canon_ast_ty(ctx, module, vars, t);
    Some(match &t.kind {
        AstTyKind::Path(p) => {
            let segs: Vec<&str> = p.segments.iter().map(|s| s.name.as_str()).collect();
            if let ([one], true) = (segs.as_slice(), p.segments[0].generics.is_empty()) {
                if let Some(v) = vars.get(*one) {
                    return Some(CanonTy::Var(v.clone()));
                }
            }
            let names = &ctx.env.names;
            if let Some(full) = names.plain_alias(t, module, ctx.here()) {
                let body = names.plain_alias_ty(&full)?;
                return canon_ast_ty(ctx, &module_of(&full), &HashMap::default(), body);
            }
            let full = names.canon(&segs, module, &[], ctx.here());
            if full
                .as_ref()
                .is_some_and(|f| names.generic_alias(f).is_some())
            {
                // `Matrix<T>` compares as `Array<T, 2>`; a cycle of
                // aliases has no form.
                let body = names.expand_alias_quiet(t, module)?;
                return ALIAS_DEPTH.with(|d| {
                    if d.get() >= 32 {
                        return None;
                    }
                    d.set(d.get() + 1);
                    let c = rec(&body);
                    d.set(d.get() - 1);
                    c
                });
            }
            let name = full.unwrap_or_else(|| segs.join("::"));
            let args = p
                .segments
                .last()
                .map_or(Some(Vec::new()), |s| s.generics.iter().map(rec).collect())?;
            CanonTy::Named(name, args)
        }
        AstTyKind::Tuple(ts) => CanonTy::Tuple(ts.iter().map(rec).collect::<Option<_>>()?),
        AstTyKind::Sum(ts) => {
            // Written sums flatten (a nested layer `Nest` stays
            // one member); duplicates collapse.
            let mut set: Vec<CanonTy> = Vec::new();
            for t in ts {
                let members = match (rec(t)?, &t.kind) {
                    (CanonTy::Sum(inner), k) if !matches!(k, AstTyKind::Nest(_)) => inner,
                    (one, _) => vec![one],
                };
                for m in members {
                    if !set.contains(&m) {
                        set.push(m);
                    }
                }
            }
            CanonTy::Sum(set)
        }
        AstTyKind::Nest(x) => rec(x)?,
        AstTyKind::None => CanonTy::Named("none".to_owned(), Vec::new()),
        AstTyKind::Never => CanonTy::Named("!".to_owned(), Vec::new()),
        AstTyKind::Any => CanonTy::Named("any".to_owned(), Vec::new()),
        AstTyKind::AnyTrait(bounds) => {
            let mut keys: Vec<String> = bounds
                .iter()
                .map(|b| {
                    let segs: Vec<&str> = b.segments.iter().map(|s| s.name.as_str()).collect();
                    ctx.env.names.trait_key(&segs, module, ctx.here())
                })
                .collect();
            if bounds
                .iter()
                .any(|b| b.segments.iter().any(|s| !s.generics.is_empty()))
            {
                return None;
            }
            keys.sort();
            CanonTy::Named(format!("any {}", keys.join(" + ")), Vec::new())
        }
        // `fn(A) -> R`: kind and ABI name the constructor, return then
        // parameters are its arguments; `+ Send` doesn't separate.
        AstTyKind::Fn(params, ret, b) => {
            let stdcall = b.system && crate::lower::consteval::stdcall_system();
            let kind = format!("fn/{}/{}/{stdcall}", b.unsafe_fn, b.c_abi);
            let args = std::iter::once(&**ret)
                .chain(params)
                .map(rec)
                .collect::<Option<_>>()?;
            CanonTy::Named(kind, args)
        }
        AstTyKind::Throws { ok, err } => {
            CanonTy::Named("throws".to_owned(), vec![rec(ok)?, rec(err)?])
        }
        AstTyKind::Array(e, len) => {
            CanonTy::Array(Box::new(rec(e)?), Box::new(canon_const(ctx, vars, len)))
        }
        AstTyKind::Const(c) => canon_const(ctx, vars, c),
        _ => return None,
    })
}

/// A const argument by value: a linear form over const parameters
/// (`N + 1`, `M - N`, `0x1`). Anything else is a variable of its own,
/// which unifies with anything (may overlap).
pub(crate) fn canon_const(
    ctx: &LowerCtx<'_>,
    vars: &HashMap<String, String>,
    c: &nabic_ast::ConstExpr,
) -> CanonTy {
    let lin = match c {
        nabic_ast::ConstExpr::Literal(_) => ctx.eval_const(c).map(Lin::num),
        nabic_ast::ConstExpr::Path(p) => match p.segments.as_slice() {
            [seg] if vars.contains_key(&seg.name) => Some(Lin::var(&vars[&seg.name])),
            _ => ctx.eval_const(c).map(Lin::num),
        },
        nabic_ast::ConstExpr::Expr(x) => lin_of(ctx, vars, x),
    };
    lin.map_or_else(|| CanonTy::Var(format!("#k.{c:p}")), Lin::canon)
}

pub(crate) fn lin_of(ctx: &LowerCtx<'_>, vars: &HashMap<String, String>, e: &Expr) -> Option<Lin> {
    let rec = |e: &Expr| lin_of(ctx, vars, e);
    match &e.kind {
        ExprKind::Literal(nabic_ast::Literal::Int(s)) => {
            Some(Lin::num(expr::parse_int_literal(s)?.0))
        }
        ExprKind::Path(p) if p.segments.len() == 1 && vars.contains_key(&p.segments[0].name) => {
            Some(Lin::var(&vars[&p.segments[0].name]))
        }
        ExprKind::Block(b) if b.stmts.is_empty() => rec(b.trailing.as_deref()?),
        ExprKind::OpPrefix { op, operand } if op == "-" => rec(operand)?.scale(-1),
        ExprKind::OpInfix { op, lhs, rhs } if matches!(op.as_str(), "+" | "-" | "*") => {
            let (l, r) = (rec(lhs)?, rec(rhs)?);
            match op.as_str() {
                "+" => l.add(&r, 1),
                "-" => l.add(&r, -1),
                _ if l.terms.is_empty() => r.scale(l.k),
                _ if r.terms.is_empty() => l.scale(r.k),
                _ => None,
            }
        }
        _ if !mentions(e, vars) => {
            let c = nabic_ast::ConstExpr::Expr(Box::new(e.clone()));
            ctx.eval_const(&c).map(Lin::num)
        }
        _ => None,
    }
}

/// Does `e` name an impl parameter? Then it can't be evaluated alone.
pub(crate) fn mentions(e: &Expr, vars: &HashMap<String, String>) -> bool {
    match &e.kind {
        ExprKind::Path(p) => p.segments.len() == 1 && vars.contains_key(&p.segments[0].name),
        ExprKind::Literal(_) => false,
        ExprKind::Block(b) if b.stmts.is_empty() => {
            b.trailing.as_deref().is_some_and(|t| mentions(t, vars))
        }
        ExprKind::OpPrefix { operand, .. } => mentions(operand, vars),
        ExprKind::OpInfix { lhs, rhs, .. } => mentions(lhs, vars) || mentions(rhs, vars),
        _ => true,
    }
}

/// `k + sum of c * v`; no zero coefficients, so equal values compare equal.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Lin {
    k: i128,
    terms: std::collections::BTreeMap<String, i128>,
}

impl Lin {
    pub(crate) const fn num(k: i128) -> Self {
        Self {
            k,
            terms: std::collections::BTreeMap::new(),
        }
    }

    pub(crate) fn var(v: &str) -> Self {
        let mut l = Self::num(0);
        l.terms.insert(v.to_owned(), 1);
        l
    }

    pub(crate) fn scale(mut self, by: i128) -> Option<Self> {
        if by == 0 {
            return Some(Self::num(0));
        }
        self.k = self.k.checked_mul(by)?;
        for c in self.terms.values_mut() {
            *c = c.checked_mul(by)?;
        }
        Some(self)
    }

    /// `self + sign * o`.
    pub(crate) fn add(mut self, o: &Self, sign: i128) -> Option<Self> {
        self.k = self.k.checked_add(o.k.checked_mul(sign)?)?;
        for (v, c) in &o.terms {
            let n = self.terms.get(v).copied().unwrap_or(0);
            let n = n.checked_add(c.checked_mul(sign)?)?;
            if n == 0 {
                self.terms.remove(v);
            } else {
                self.terms.insert(v.clone(), n);
            }
        }
        Some(self)
    }

    /// A lone parameter stays a plain variable (or rigid name).
    pub(crate) fn canon(self) -> CanonTy {
        match self.terms.iter().next() {
            Some((v, 1)) if self.k == 0 && self.terms.len() == 1 => {
                if v.starts_with("#r.") {
                    CanonTy::Named(v.clone(), Vec::new())
                } else {
                    CanonTy::Var(v.clone())
                }
            }
            _ => CanonTy::Const(self),
        }
    }
}

impl std::fmt::Display for Lin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.terms.is_empty() {
            return write!(f, "{}", self.k);
        }
        let mut out = String::new();
        for (v, c) in &self.terms {
            let name = v.rsplit('.').next().unwrap_or(v);
            let mag = match c.unsigned_abs() {
                1 => name.to_owned(),
                m => format!("{m} * {name}"),
            };
            let sign = match (out.is_empty(), *c < 0) {
                (true, false) => "",
                (true, true) => "-",
                (false, false) => " + ",
                (false, true) => " - ",
            };
            out = format!("{out}{sign}{mag}");
        }
        match self.k {
            0 => write!(f, "{{ {out} }}"),
            k if k < 0 => write!(f, "{{ {out} - {} }}", k.unsigned_abs()),
            k => write!(f, "{{ {out} + {k} }}"),
        }
    }
}

/// A concrete `CanonTy` as a type: a primitive or a non-generic struct
/// / enum; `None` for anything else (the bound check gives up).
pub(crate) fn canon_hir(ctx: &LowerCtx<'_>, t: &CanonTy) -> Option<HirTy> {
    let CanonTy::Named(name, args) = t else {
        return None;
    };
    if !args.is_empty() {
        return None;
    }
    match crate::ty::resolve_ty(&Ty {
        kind: AstTyKind::Path(AstPath {
            segments: vec![nabic_ast::PathSegment {
                name: name.clone(),
                generics: Vec::new(),
            }],
            span: nabic_diagnostic::Span::default(),
            origin: None,
        }),
        span: nabic_diagnostic::Span::default(),
    }) {
        p @ HirTy::Prim(_) => Some(p),
        _ => ctx
            .lookup_struct(name)
            .map(HirTy::Struct)
            .or_else(|| ctx.lookup_enum(name).map(HirTy::Enum)),
    }
}

impl std::fmt::Display for CanonTy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let list =
            |ts: &[Self], sep: &str| ts.iter().map(Self::to_string).collect::<Vec<_>>().join(sep);
        match self {
            Self::Var(v) => write!(f, "{}", v.rsplit('.').next().unwrap_or(v)),
            Self::Named(n, args) if n.starts_with("fn/") => {
                let ret = args.first().map_or_else(String::new, Self::to_string);
                write!(
                    f,
                    "fn({}) -> {ret}",
                    list(args.get(1..).unwrap_or_default(), ", ")
                )
            }
            Self::Named(n, args) if n == "throws" && args.len() == 2 => {
                write!(f, "throws ({}) {}", args[1], args[0])
            }
            Self::Named(n, args) if args.is_empty() => {
                write!(f, "{}", n.rsplit("::").next().unwrap_or(n))
            }
            Self::Named(n, args) => {
                write!(
                    f,
                    "{}<{}>",
                    n.rsplit("::").next().unwrap_or(n),
                    list(args, ", ")
                )
            }
            Self::Tuple(ts) => write!(f, "({})", list(ts, ", ")),
            Self::Sum(ts) => write!(f, "{}", list(ts, " | ")),
            Self::Array(e, n) => write!(f, "[{e}; {n}]"),
            Self::Const(l) => write!(f, "{l}"),
        }
    }
}

/// First-order unification over `CanonTy` (no occurs check: the two
/// impls' variables are distinct).
#[derive(Default)]
pub(crate) struct Unifier {
    sub: HashMap<String, CanonTy>,
    /// Const variables of an unsigned type.
    nonneg: nabic_core::HashSet<String>,
}

impl Unifier {
    pub(crate) fn resolve(&self, v: &str) -> CanonTy {
        match self.sub.get(v) {
            Some(CanonTy::Var(w)) => self.resolve(w),
            Some(t) => self.walk(t),
            None => CanonTy::Var(v.to_owned()),
        }
    }

    pub(crate) fn walk(&self, t: &CanonTy) -> CanonTy {
        match t {
            CanonTy::Var(v) => self.resolve(v),
            CanonTy::Named(n, args) => {
                CanonTy::Named(n.clone(), args.iter().map(|a| self.walk(a)).collect())
            }
            CanonTy::Tuple(ts) => CanonTy::Tuple(ts.iter().map(|a| self.walk(a)).collect()),
            CanonTy::Sum(ts) => CanonTy::Sum(ts.iter().map(|a| self.walk(a)).collect()),
            CanonTy::Array(e, n) => CanonTy::Array(Box::new(self.walk(e)), Box::new(self.walk(n))),
            CanonTy::Const(l) => {
                let mut out = Lin::num(l.k);
                for (v, c) in &l.terms {
                    let t = as_lin(&self.resolve(v)).unwrap_or_else(|| Lin::var(v));
                    match t.scale(*c).and_then(|t| out.clone().add(&t, 1)) {
                        Some(n) => out = n,
                        None => return CanonTy::Const(l.clone()),
                    }
                }
                out.canon()
            }
        }
    }

    #[allow(clippy::many_single_char_names)]
    pub(crate) fn unify(&mut self, x: &CanonTy, y: &CanonTy) -> Option<()> {
        match (self.walk(x), self.walk(y)) {
            (CanonTy::Var(v), CanonTy::Var(w)) if v == w => Some(()),
            (a, b) if matches!(a, CanonTy::Const(_)) || matches!(b, CanonTy::Const(_)) => {
                self.unify_const(&a, &b)
            }
            // No cyclic binding (`T` against `List<T>`): no overlap.
            (CanonTy::Var(v), t) | (t, CanonTy::Var(v)) => {
                if occurs(&v, &t) {
                    return None;
                }
                self.sub.insert(v, t);
                Some(())
            }
            (CanonTy::Named(n, xs), CanonTy::Named(m, ys)) if n == m && xs.len() == ys.len() => {
                xs.iter().zip(&ys).try_for_each(|(p, q)| self.unify(p, q))
            }
            (CanonTy::Tuple(xs), CanonTy::Tuple(ys)) if xs.len() == ys.len() => {
                xs.iter().zip(&ys).try_for_each(|(p, q)| self.unify(p, q))
            }
            (CanonTy::Array(p, n), CanonTy::Array(q, m)) => {
                self.unify(&n, &m)?;
                self.unify(&p, &q)
            }
            (CanonTy::Sum(xs), CanonTy::Sum(ys)) if xs.len() == ys.len() => {
                self.unify_set(&xs, &ys)
            }
            _ => None,
        }
    }

    /// Is `x - y == 0` solvable over the integers? Solve for a unit
    /// coefficient variable when there is one; otherwise assume it is.
    pub(crate) fn unify_const(&mut self, x: &CanonTy, y: &CanonTy) -> Option<()> {
        let d = as_lin(x)?;
        let Some(d) = d.add(&as_lin(y)?, -1) else {
            return Some(());
        };
        if d.terms.is_empty() {
            return (d.k == 0).then_some(());
        }
        let g = d.terms.values().fold(0, |g, c| gcd(g, c.unsigned_abs()));
        if d.k.unsigned_abs() % g != 0 {
            return None;
        }
        // Unsigned terms all of the constant's sign can't cancel it.
        if d.k != 0
            && d.terms
                .iter()
                .all(|(v, c)| self.nonneg.contains(v) && (*c > 0) == (d.k > 0))
        {
            return None;
        }
        let unit = d
            .terms
            .iter()
            .find(|(v, c)| c.unsigned_abs() == 1 && !v.starts_with("#r."))
            .map(|(v, c)| (v.clone(), *c));
        if let Some((v, c)) = unit {
            let mut rest = d;
            rest.terms.remove(&v);
            if let Some(val) = rest.scale(-c) {
                self.sub.insert(v, val.canon());
            }
        }
        Some(())
    }

    /// Sums are sets: some pairing of the members unifies.
    pub(crate) fn unify_set(&mut self, xs: &[CanonTy], ys: &[CanonTy]) -> Option<()> {
        let Some((x, rest)) = xs.split_first() else {
            return Some(());
        };
        for k in 0..ys.len() {
            let saved = self.sub.clone();
            let mut others = ys.to_vec();
            let y = others.remove(k);
            if self.unify(x, &y).is_some() && self.unify_set(rest, &others).is_some() {
                return Some(());
            }
            self.sub = saved;
        }
        None
    }
}

/// A const variable, rigid parameter or `Const` as a linear form.
pub(crate) fn as_lin(t: &CanonTy) -> Option<Lin> {
    match t {
        CanonTy::Var(v) => Some(Lin::var(v)),
        CanonTy::Named(n, a) if a.is_empty() && n.starts_with("#r.") => Some(Lin::var(n)),
        CanonTy::Const(l) => Some(l.clone()),
        _ => None,
    }
}

pub(crate) fn gcd(a: u128, b: u128) -> u128 {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// Does variable `v` occur in `t` (already walked)?
pub(crate) fn occurs(v: &str, t: &CanonTy) -> bool {
    match t {
        CanonTy::Var(w) => w == v,
        CanonTy::Named(_, ts) | CanonTy::Tuple(ts) | CanonTy::Sum(ts) => {
            ts.iter().any(|t| occurs(v, t))
        }
        CanonTy::Array(e, n) => occurs(v, e) || occurs(v, n),
        CanonTy::Const(l) => l.terms.contains_key(v),
    }
}

/// Auto trait impls: opt in with `unsafe impl`, out with `impl !Tr for S`
/// (recorded in `neg`); both only for auto traits. An `unsafe trait`
/// takes `unsafe impl` only. Whether to register the impl normally (false
/// for opt-outs and errors).
pub(crate) fn check_auto_impl(
    impl_def: &nabic_ast::ImplDef,
    names: &TypeNames,
    module: &str,
    neg: &mut nabic_core::HashSet<(String, String)>,
    diags: &mut Vec<nabic_diagnostic::NabiDiagnostic>,
) -> bool {
    let err = |d: &mut Vec<nabic_diagnostic::NabiDiagnostic>, msg: String| {
        d.push(nabic_diagnostic::NabiDiagnostic::error(
            Code::INVALID_IMPL,
            impl_def.span,
            msg,
        ));
    };
    let Some(tp) = &impl_def.trait_path else {
        if impl_def.is_unsafe || impl_def.negative {
            err(diags, "`unsafe impl` / `impl !` needs a trait".to_owned());
        }
        return true;
    };
    let segs: Vec<&str> = tp.segments.iter().map(|s| s.name.as_str()).collect();
    let key = names.trait_key(&segs, tp.origin.as_deref().unwrap_or(module), impl_def.span);
    let target = impl_def_display(&impl_def.self_ty);
    let tr = segs.join("::");
    if key == crate::lower::reflect::REFLECT {
        err(
            diags,
            format!(
                "`Reflect` is implemented by the compiler for every type; remove `impl {tr} for \
                 {target}`"
            ),
        );
        return false;
    }
    if names.unsafe_traits.contains(&key) {
        if impl_def.negative {
            err(
                diags,
                format!(
                    "`impl !{tr}`: `impl !Trait` is only for the auto traits `Send` / `Sync`; \
                     remove it (a type without `unsafe impl {tr}` does not have it)"
                ),
            );
            return false;
        }
        if !impl_def.is_unsafe {
            err(
                diags,
                format!(
                    "`{tr}` is an `unsafe trait`: the impl promises what the compiler cannot \
                     check; write `unsafe impl {tr} for {target}`"
                ),
            );
            return false;
        }
        return true;
    }
    if !names.auto_traits.contains(&key) {
        if impl_def.is_unsafe || impl_def.negative {
            err(
                diags,
                format!(
                    "`{tr}` is not an auto trait or an `unsafe trait` - `unsafe impl` is only \
                     for `Send`, `Sync` and `unsafe trait`s, `impl !Trait` only for `Send` / \
                     `Sync`; write `impl {tr} for {target}`"
                ),
            );
            return false;
        }
        return true;
    }
    if impl_def.negative {
        let base = match &impl_def.self_ty.kind {
            AstTyKind::Path(p) => {
                let segs: Vec<&str> = p.segments.iter().map(|s| s.name.as_str()).collect();
                names.canon(&segs, module, &[], impl_def.span)
            }
            _ => None,
        };
        match base {
            Some(b) if impl_def.items.is_empty() && !impl_def.is_unsafe => {
                neg.insert((b, key));
            }
            Some(_) => err(
                diags,
                format!("`impl !{tr} for {target}` takes no `unsafe` and has an empty body"),
            ),
            None => err(
                diags,
                format!("`impl !{tr}` target `{target}` must be a struct or enum"),
            ),
        }
        return false;
    }
    if !impl_def.is_unsafe {
        err(
            diags,
            format!(
                "`{tr}` is an auto trait the compiler derives from the fields; a manual \
                 impl for `{target}` must be `unsafe impl {tr} for {target} {{}}`"
            ),
        );
        return false;
    }
    true
}
