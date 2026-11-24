//! Derive, bound and marker checks.

#[allow(clippy::wildcard_imports)]
use crate::lower::collect::*;

/// `#[derive(Tr)]` needs every field to implement `Tr` (here
/// for non-generic types; generic impls bound their parameters), and
/// `Copy` excludes `Drop`. Returns the failed attributes' spans.
pub(crate) fn check_derives(env: &Env<'_>, program: &Program) -> Vec<nabic_diagnostic::Span> {
    let mut failed = Vec::new();
    let no_subst = HashMap::default();
    let ctx = LowerCtx::new(env, &no_subst, None, Span::DUMMY);
    for item in &program.items {
        let (attrs, name, generic, fields): (_, _, _, Vec<(String, HirTy)>) = match item {
            Item::Struct(s) => (
                &s.attrs,
                &s.name,
                !s.generics.params.is_empty(),
                env.defs.struct_id(&s.name).map_or_else(Vec::new, |id| {
                    ctx.struct_def(id)
                        .fields
                        .iter()
                        .map(|f| (f.name.clone(), f.ty.clone()))
                        .collect()
                }),
            ),
            Item::Enum(e) => (
                &e.attrs,
                &e.name,
                !e.generics.params.is_empty(),
                env.defs.enum_id(&e.name).map_or_else(Vec::new, |id| {
                    ctx.enum_def(id)
                        .variants
                        .iter()
                        .flat_map(|v| {
                            crate::lower::expr::variant_arg_tys(v)
                                .into_iter()
                                .map(|t| (v.name.clone(), t))
                        })
                        .collect()
                }),
            ),
            _ => continue,
        };
        if generic {
            continue;
        }
        let self_ty = match item {
            Item::Struct(s) => env.defs.struct_id(&s.name).map(HirTy::Struct),
            Item::Enum(e) => env.defs.enum_id(&e.name).map(HirTy::Enum),
            _ => None,
        };
        ctx.at.set(item.span());
        check_pod(&ctx, attrs, name, self_ty.as_ref(), &fields);
        for (tr, at) in nabic_parser::derive::derive_names(attrs) {
            // `Error` asks nothing of the fields.
            let Some(key) = nabic_parser::derive::derivable(&tr).filter(|_| tr != "Error") else {
                continue;
            };
            ctx.at.set(at);
            // `Default` takes a field's declared default instead.
            let defaulted = |field: &str| {
                tr == "Default"
                    && matches!(item, Item::Struct(s) if matches!(&s.kind,
                        nabic_ast::StructKind::Named(fs)
                            if fs.iter().any(|f| f.name == field && f.default.is_some())))
            };
            for (field, ty) in &fields {
                if !defaulted(field) && !ctx.has_trait(ty, key) {
                    failed.push(at);
                    ctx.emit_error(
                        Code::CANNOT_DERIVE,
                        format!(
                            "cannot derive `{tr}` for `{}`: `{field}` has type `{}`, which does \
                             not implement `{tr}`",
                            leaf_name(name),
                            ctx.ty_name(ty)
                        ),
                    );
                }
            }
            if tr == "Copy"
                && self_ty
                    .as_ref()
                    .is_some_and(|t| ctx.has_trait(t, ctx.lang("drop_method")))
            {
                ctx.emit_error(
                    Code::CANNOT_DERIVE,
                    format!(
                        "cannot derive `Copy` for `{}`: it implements `Drop`",
                        leaf_name(name)
                    ),
                );
            }
        }
    }
    failed
}

/// `Pod`: a `#[repr(C)]` / `#[repr(transparent)]` struct of
/// `Pod` fields without `Drop`.
pub(crate) fn check_pod(
    ctx: &LowerCtx<'_>,
    attrs: &[nabic_ast::Attribute],
    name: &str,
    self_ty: Option<&HirTy>,
    fields: &[(String, HirTy)],
) {
    const POD: &str = "bare::mem::Pod";
    let Some(ty) = self_ty.filter(|t| ctx.has_trait(t, POD)) else {
        return;
    };
    let fail = |why: String| {
        ctx.emit_error(
            Code::CANNOT_DERIVE,
            format!("`{}` cannot be `Pod`: {why}", leaf_name(name)),
        );
    };
    if !matches!(ty, HirTy::Struct(_)) {
        return fail("only a struct can be".to_owned());
    }
    let repr = repr_of(attrs).unwrap_or_default();
    if !repr.c && !repr.transparent {
        fail("it needs `#[repr(C)]` or `#[repr(transparent)]`".to_owned());
    }
    if ctx.has_trait(ty, ctx.lang("drop_method")) {
        fail("it implements `Drop`".to_owned());
    }
    for (field, fty) in fields {
        if !ctx.has_trait(fty, POD) {
            fail(format!(
                "`{field}` has type `{}`, which is not `Pod`",
                ctx.ty_name(fty)
            ));
        }
    }
}

/// What a `pub` meta block's template names from a root must
/// exist and be reachable from the block's module, so the error is the
/// author's when the package builds, not each user's.
pub(crate) fn check_templates(env: &Env<'_>, templates: &[crate::TemplateRefs]) {
    let names = &env.names;
    let item = |k: &str| {
        let k = names.reexport(k).map_or(k, String::as_str);
        let value = env.sig(k).is_some()
            || env.generic_fn(k).is_some()
            || env.konst(k).is_some()
            || env.static_ty(k).is_some();
        let ty = names.has_type(k) || names.is_trait(k) || names.is_variant(k);
        (value || ty).then(|| {
            let vis = if ty {
                names.type_vis(k)
            } else {
                env.item_vis(k)
            };
            (k.to_owned(), vis.unwrap_or(Visibility::Pub))
        })
    };
    for t in templates {
        for (segs, at) in &t.paths {
            let written: Vec<&str> = segs.iter().map(String::as_str).collect();
            let tier = || {
                resolve::tier_path(&written, &t.module)
                    .map(|p| p.split("::").map(str::to_owned).collect())
            };
            let Some(full) = resolve::root_path(&written, &t.module).or_else(tier) else {
                continue;
            };
            let shown = || {
                let mut s = written.clone();
                if let Some(p) = s
                    .first_mut()
                    .and_then(|r| r.strip_prefix(nabic_parser::PKG_ROOT))
                {
                    *s.first_mut().expect("a root") = if p.is_empty() { "pkg" } else { p };
                }
                s.join("::")
            };
            // The item, or a type (or trait) whose associated item it
            // names: in the root package `pkg::E::new` is `E::new`.
            let found = (1..=full.len())
                .rev()
                .find_map(|n| {
                    item(&full[..n].join("::"))
                        .filter(|(k, _)| n == full.len() || names.has_type(k) || names.is_trait(k))
                })
                .or_else(|| {
                    let m = full.join("::");
                    names
                        .mod_vis
                        .contains_key(&m)
                        .then_some((m, Visibility::Pub))
                });
            let d = match found {
                None => nabic_diagnostic::NabiDiagnostic::error(
                    Code::UNRESOLVED_NAME,
                    *at,
                    format!(
                        "this meta template names `{}`, which does not exist",
                        shown()
                    ),
                ),
                Some((k, vis)) if !visible_from(&t.module, &module_of(&k), vis) => {
                    nabic_diagnostic::NabiDiagnostic::error(
                        Code::PRIVATE_ITEM,
                        *at,
                        format!(
                            "this meta template names `{}`, which its module `{}` cannot see",
                            shown(),
                            module_display(&t.module)
                        ),
                    )
                }
                Some(_) => continue,
            };
            env.sink.diagnostics.borrow_mut().push(d);
        }
    }
}

/// Every trait in a bound position must exist, with no more type arguments
/// than parameters. An impl gives every parameter without a default, and
/// only the associated types its trait declares.
pub(crate) fn check_bound_names(
    program: &Program,
    names: &TypeNames,
    base: &HashMap<String, &nabic_ast::TraitDef>,
    pre_diags: &mut Vec<nabic_diagnostic::NabiDiagnostic>,
) {
    let traits: HashMap<&str, &nabic_ast::TraitDef> = program
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Trait(t) => Some(t),
            _ => None,
        })
        .chain(base.values().copied())
        .map(|t| (t.name.as_str(), t))
        .collect();
    // `role` says where the trait was written: "in a bound", "in an impl", ..
    let mut check = |b: &Ty, module: &str, role: &str, imp: Option<&nabic_ast::ImplDef>| {
        let AstTyKind::Path(p) = &b.kind else {
            return;
        };
        let segs: Vec<&str> = p.segments.iter().map(|s| s.name.as_str()).collect();
        let module = p.origin.as_deref().unwrap_or(module);
        let key = names.trait_key(&segs, module, b.span);
        // A bootstrap trait keys by its leaf (see `trait_key`).
        let Some(found) = names.traits.get(&key) else {
            let d = nabic_diagnostic::NabiDiagnostic::error(
                Code::UNRESOLVED_TYPE,
                b.span,
                format!("cannot find trait `{}` (named {role})", segs.join("::")),
            );
            let keys = || {
                names
                    .traits
                    .iter()
                    .filter(|t| leaf_name(t) == key)
                    .cloned()
                    .collect()
            };
            // `core::ops::Index`: the trait of that name lives elsewhere.
            let leaf = segs.last().copied().unwrap_or_default();
            let elsewhere = || {
                let found: Vec<&String> = names
                    .traits
                    .iter()
                    .filter(|t| leaf_name(t) == leaf)
                    .collect();
                match found.as_slice() {
                    [one] => Some(format!("`{leaf}` is `{one}`: write that, or `use {one};`")),
                    _ => None,
                }
            };
            let help = if segs.len() == 1 {
                names.import_help(module, keys(), |k| names.type_vis(k))
            } else {
                elsewhere()
            };
            pre_diags.push(match help {
                Some(h) => d.with_help(h),
                None => d,
            });
            return;
        };
        let written = p.segments.last().map_or(0, |s| {
            s.generics
                .iter()
                .filter(|g| nabic_ast::as_assoc_binding(g).is_none())
                .count()
        });
        let Some(&td) = traits.get(found.as_str()) else {
            return;
        };
        let g = &td.generics;
        // Fewer is allowed: the bound holds for any of the rest
        // (`S: Source<Item = i32>`).
        let max = g.params.len();
        if written > max {
            let (msg, help) = mono::misfit(leaf_name(found), g, written);
            pre_diags.push(
                nabic_diagnostic::NabiDiagnostic::error(Code::GENERIC_ARGUMENTS, b.span, msg)
                    .with_help(help),
            );
        }
        if let Some(i) = imp {
            check_impl_header(i, td, &segs, written, b.span, pre_diags);
        }
    };
    let mut generics = |g: &nabic_ast::Generics, w: &[nabic_ast::WherePred], module: &str| {
        for gp in &g.params {
            if let nabic_ast::GenericParam::Type { bounds, .. } = gp {
                bounds
                    .iter()
                    .for_each(|b| check(b, module, "in a bound", None));
            }
        }
        for p in w {
            p.bounds
                .iter()
                .for_each(|b| check(b, module, "in a bound", None));
        }
    };
    let fn_bounds =
        |f: &FnDef,
         generics: &mut dyn FnMut(&nabic_ast::Generics, &[nabic_ast::WherePred], &str)| {
            generics(&f.generics, &f.where_clause, &module_of(&f.name));
        };
    let mut impl_paths: Vec<(Ty, String, &str, Option<&nabic_ast::ImplDef>)> = Vec::new();
    for item in &program.items {
        let module = item_module(item);
        match item {
            Item::Fn(f) => fn_bounds(f, &mut generics),
            Item::Struct(s) => generics(&s.generics, &s.where_clause, &module),
            Item::Enum(e) => generics(&e.generics, &e.where_clause, &module),
            Item::Union(u) => generics(&u.generics, &u.where_clause, &module),
            Item::TypeAlias(a) => generics(&a.generics, &a.where_clause, &module),
            Item::Trait(t) => {
                generics(&t.generics, &t.where_clause, &module);
                for s in &t.supertraits {
                    impl_paths.push((s.clone(), module.clone(), "as a supertrait", None));
                }
                for ti in &t.items {
                    match ti {
                        // A trait method's name is its leaf: the trait's module.
                        nabic_ast::TraitItem::Method(m) => {
                            generics(&m.generics, &m.where_clause, &module);
                        }
                        nabic_ast::TraitItem::AssocType { bounds, .. } => {
                            impl_paths.extend(
                                bounds
                                    .iter()
                                    .map(|b| (b.clone(), module.clone(), "in a bound", None)),
                            );
                        }
                        nabic_ast::TraitItem::Const { .. } => {}
                    }
                }
            }
            Item::Impl(i) => {
                let module = impl_module(i);
                generics(&i.generics, &i.where_clause, &module);
                if let Some(tp) = &i.trait_path {
                    impl_paths.push((
                        Ty {
                            kind: AstTyKind::Path(tp.clone()),
                            span: tp.span,
                        },
                        module.clone(),
                        "in an impl",
                        Some(i),
                    ));
                }
                for ii in &i.items {
                    if let AstImplItem::Method(m) = ii {
                        fn_bounds(m, &mut generics);
                    }
                }
            }
            _ => {}
        }
    }
    for (t, module, role, imp) in &impl_paths {
        check(t, module, role, *imp);
    }
}

/// `impl Tr<..> for X`: a missing parameter, or a `type Name = ..` the
/// trait does not declare. An undeclared associated type standing in for
/// a missing parameter moves into the header in the help.
pub(crate) fn check_impl_header(
    i: &nabic_ast::ImplDef,
    td: &nabic_ast::TraitDef,
    segs: &[&str],
    written: usize,
    at: Span,
    pre_diags: &mut Vec<nabic_diagnostic::NabiDiagnostic>,
) {
    if i.negative {
        return;
    }
    let declared: Vec<&str> = td
        .items
        .iter()
        .filter_map(|ti| match ti {
            nabic_ast::TraitItem::AssocType { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    let extra: Vec<(&str, &Ty)> = i
        .items
        .iter()
        .filter_map(|ii| match ii {
            AstImplItem::AssocType { name, ty } if !declared.contains(&name.as_str()) => {
                Some((name.as_str(), ty))
            }
            _ => None,
        })
        .collect();
    let g = &td.generics;
    let min = g
        .params
        .iter()
        .take_while(|p| {
            !matches!(
                p,
                nabic_ast::GenericParam::Type {
                    default: Some(_),
                    ..
                }
            )
        })
        .count();
    let name = leaf_name(&td.name);
    let self_ty = impl_def_display(&i.self_ty);
    let mut moved = false;
    if written < min {
        let missing: Vec<&str> = g.params[written..min]
            .iter()
            .map(mono::param_name)
            .collect();
        moved = extra.len() == missing.len();
        let fill: Vec<String> = if moved {
            extra
                .iter()
                .map(|(_, t)| nabic_ast::print::print_ty(t))
                .collect()
        } else {
            missing.iter().map(|&p| p.to_owned()).collect()
        };
        let args: Vec<String> = i
            .trait_path
            .iter()
            .filter_map(|tp| tp.segments.last())
            .flat_map(|s| &s.generics)
            .filter(|t| nabic_ast::as_assoc_binding(t).is_none())
            .map(nabic_ast::print::print_ty)
            .chain(fill)
            .collect();
        let header = format!(
            "impl {}<{}> for {self_ty}",
            segs.join("::"),
            args.join(", ")
        );
        let help = if moved {
            format!(
                "`{name}` is declared `{}`; its parameters are not associated types: \
                 write `{header}` and remove {}",
                mono::declared(name, g),
                extra
                    .iter()
                    .map(|(n, _)| format!("`type {n}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        } else {
            format!(
                "`{name}` is declared `{}`; write `{header}` with a type for {}",
                mono::declared(name, g),
                missing
                    .iter()
                    .map(|p| format!("`{p}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        pre_diags.push(
            nabic_diagnostic::NabiDiagnostic::error(
                Code::GENERIC_ARGUMENTS,
                at,
                mono::arity_msg(name, min, g.params.len(), written),
            )
            .with_help(help),
        );
    }
    if moved {
        return;
    }
    for (n, t) in extra {
        let help = if declared.is_empty() {
            format!("remove `type {n}`; `{name}` declares no associated types")
        } else {
            let names: Vec<String> = declared.iter().map(|d| format!("`{d}`")).collect();
            format!("remove `type {n}`; `{name}` declares {}", names.join(", "))
        };
        pre_diags.push(
            nabic_diagnostic::NabiDiagnostic::error(
                Code::NOT_A_TRAIT_MEMBER,
                t.span,
                format!("associated type `{n}` is not a member of trait `{name}`"),
            )
            .with_help(help),
        );
    }
}

/// Every generic struct / union / enum instantiation must
/// meet its parameters' bounds, inline (`T: Tr`) and `where` alike.
pub(crate) fn check_type_bounds(env: &Env<'_>) {
    // (display name, item's module, arguments, generics, `where`, use site)
    type Inst<'d> = (
        String,
        String,
        Vec<HirTy>,
        &'d nabic_ast::Generics,
        &'d [nabic_ast::WherePred],
        nabic_diagnostic::Span,
    );
    let no_subst = HashMap::default();
    let ctx = LowerCtx::new(env, &no_subst, None, Span::DUMMY);
    let n_structs = env.defs.structs.borrow().len();
    let n_enums = env.defs.enums.borrow().len();
    let mut insts: Vec<Inst<'_>> = Vec::new();
    for i in 0..n_structs {
        let id = StructId(u32::try_from(i).expect("StructId overflow"));
        let d = ctx.struct_def(id);
        let base = d.name.split("__").next().unwrap_or_default().to_owned();
        if d.generic_args.is_empty() {
            continue;
        }
        let tmpl = env
            .defs
            .generic_structs
            .get(&base)
            .map(|s| (&s.generics, &s.where_clause));
        let tmpl = tmpl.or_else(|| {
            env.defs
                .generic_unions
                .get(&base)
                .map(|u| (&u.generics, &u.where_clause))
        });
        if let Some((generics, where_clause)) = tmpl {
            insts.push((
                ctx.ty_name(&HirTy::Struct(id)),
                module_of(&base),
                d.generic_args,
                generics,
                where_clause,
                d.span,
            ));
        }
    }
    for i in 0..n_enums {
        let id = EnumId(u32::try_from(i).expect("EnumId overflow"));
        let d = ctx.enum_def(id);
        let base = enum_base_name(&d.name).to_owned();
        if let Some(def) = env
            .defs
            .generic_enums
            .get(&base)
            .filter(|_| !d.generic_args.is_empty())
        {
            insts.push((
                ctx.ty_name(&HirTy::Enum(id)),
                module_of(&base),
                d.generic_args,
                &def.generics,
                &def.where_clause,
                d.span,
            ));
        }
    }
    // A non-generic alias's `where` holds once (at the
    // alias); a generic alias's bounds hold at each use.
    for (full, def) in &env.names.generic_aliases {
        if def.generics.params.is_empty() {
            insts.push((
                leaf_name(full).to_owned(),
                module_of(full),
                Vec::new(),
                &def.generics,
                &def.where_clause,
                def.span,
            ));
        }
    }
    for (shown, module, args, generics, where_clause, span) in insts {
        let errors = unmet_bounds(
            env,
            &shown,
            module,
            &args,
            generics,
            where_clause,
            span,
            |_| None,
        );
        env.sink.diagnostics.borrow_mut().extend(errors);
    }
    // Generic alias uses naming a type parameter are checked where its
    // bounds are known: by the body's definition check, or here for impl
    // targets.
    let mut seen = nabic_core::HashSet::default();
    let uses = env.names.alias_uses.borrow().clone();
    for u in uses {
        if !seen.insert((u.0.clone(), u.3)) {
            continue;
        }
        let errors = if let Some(gi) = env.impls.generic().find(|gi| gi.self_ty.span == u.3) {
            let params: Vec<_> = gi.generics.params.iter().collect();
            let where_clause: Vec<_> = gi.where_clause.iter().collect();
            let self_of = crate::lower::bounds::SelfOf::Impl(&gi.self_ty);
            let owner = |_: &str| Some("the impl's generics".to_owned());
            crate::lower::bounds::in_scope(
                env,
                &params,
                &where_clause,
                (u.2.clone(), u.3, &[]),
                &self_of,
                |rcx, _, _| alias_use_unmet(rcx, &u, false, owner),
            )
            .0
        } else {
            let mut rcx = LowerCtx::new(env, &no_subst, None, u.3);
            let mark = rcx.probe_mark();
            let errors = alias_use_unmet(&mut rcx, &u, false, |_| None);
            rcx.rollback(&mark);
            errors
        };
        env.sink.diagnostics.borrow_mut().extend(errors);
    }
}

/// Bounds that generic alias use `u` fails, arguments resolved by `rcx`.
/// None when an argument names a type `rcx` doesn't know, or with
/// `params_only` when none names a parameter.
pub(crate) fn alias_use_unmet(
    rcx: &mut LowerCtx<'_>,
    (full, args, module, span): &AliasUse,
    params_only: bool,
    owner: impl Fn(&str) -> Option<String>,
) -> Vec<nabic_diagnostic::NabiDiagnostic> {
    let env = rcx.env;
    let Some(def) = env.names.generic_aliases.get(full) else {
        return Vec::new();
    };
    let outer = std::mem::replace(&mut rcx.current_module, module.clone());
    // An argument this context can't resolve (`Self::Elem` written in a
    // trait signature, checked from a caller) is reported where it is
    // written, not here.
    let mark = env.sink.diagnostics.borrow().len();
    let args: Vec<HirTy> = def
        .generics
        .params
        .iter()
        .zip(args)
        .map(|(gp, a)| match (gp, &a.kind) {
            (nabic_ast::GenericParam::Const { .. }, AstTyKind::Const(c)) => rcx
                .eval_const(c)
                .map_or(HirTy::Unknown, |v| HirTy::Const(v, None)),
            _ => rcx.resolve_ast_ty(a),
        })
        .collect();
    rcx.current_module = outer;
    if env.sink.diagnostics.borrow().len() > mark {
        env.sink.diagnostics.borrow_mut().truncate(mark);
        return Vec::new();
    }
    let unknown =
        |a: &HirTy| crate::lower::bounds::mentions(rcx, a, &|t| matches!(t, HirTy::Path(_)));
    let param = |a: &HirTy| crate::lower::bounds::has_param(rcx, a);
    if args.iter().any(unknown) || params_only && !args.iter().any(param) {
        return Vec::new();
    }
    let shown: Vec<String> = args.iter().map(|a| rcx.ty_name(a)).collect();
    let shown = format!("{}<{}>", leaf_name(full), shown.join(", "));
    let (generics, where_clause) = (&def.generics, &def.where_clause);
    unmet_bounds(
        env,
        &shown,
        module_of(full),
        &args,
        generics,
        where_clause,
        *span,
        owner,
    )
}

/// Bounds of `generics` / `where_clause` that `args` fail, as errors at
/// `span`; `owner` names the declaring item for the help.
#[allow(clippy::too_many_arguments)]
pub(crate) fn unmet_bounds(
    env: &Env<'_>,
    shown: &str,
    module: String,
    args: &[HirTy],
    generics: &nabic_ast::Generics,
    where_clause: &[nabic_ast::WherePred],
    span: Span,
    owner: impl Fn(&str) -> Option<String>,
) -> Vec<nabic_diagnostic::NabiDiagnostic> {
    let subst = generic_subst(generics, args);
    let mut rcx = LowerCtx::new(env, &subst, None, span);
    rcx.current_module = module;
    let mut goals: Vec<(HirTy, &Vec<Ty>)> = Vec::new();
    for gp in &generics.params {
        if let nabic_ast::GenericParam::Type { name, bounds, .. } = gp {
            if let Some(t) = subst.get(name) {
                goals.push((t.clone(), bounds));
            }
        }
    }
    for w in where_clause {
        goals.push((rcx.resolve_ast_ty(&w.ty), &w.bounds));
    }
    let mut out = Vec::new();
    for (ty, bounds) in goals {
        // An instance over an inference variable is a probe's, dropped.
        if ty == HirTy::Unknown || crate::lower::infer::has_var(&rcx, &ty) {
            continue;
        }
        for b in bounds {
            let module = rcx.current_module.clone();
            let Some(want) = rcx.bound_ref(b, &module, |t| rcx.resolve_ast_ty(t)) else {
                continue;
            };
            if rcx.implements_trait(&ty, &want, 0) {
                continue;
            }
            let name = rcx.ty_name(&ty);
            let mut d = nabic_diagnostic::NabiDiagnostic::error(
                Code::UNSATISFIED_BOUND,
                span,
                format!(
                    "type `{name}` does not satisfy trait bound `{}` required by `{shown}`",
                    rcx.trait_name(&want),
                ),
            );
            if let (HirTy::Param(_), Some(o)) = (&ty, owner(&name)) {
                let bound = want.render(leaf_name(&want.path), |t| rcx.ty_name(t));
                d.notes.push(format!("help: add `{name}: {bound}` to {o}"));
            } else if want.path == crate::lower::reflect::INSPECT {
                if let Some(h) = crate::lower::reflect::derive_inspect_help(&rcx, &ty) {
                    d.notes.push(format!("help: {h}"));
                }
            } else if let Some(h) = crate::lower::bounds::implementors_help(&rcx, &want) {
                d.notes.push(format!("help: {h}"));
            }
            out.push(d);
        }
    }
    out
}

/// A `static` is read by every thread, so it must be `Sync`; the
/// error suggests a `Mutex`, or an `Atomic` for a `Cell` of a number.
pub(crate) fn check_statics_sync(env: &Env<'_>, programs: &[&Program]) {
    let no_subst = HashMap::default();
    let ctx = LowerCtx::new(env, &no_subst, None, Span::DUMMY);
    let sync = ctx.lang("sync_trait");
    if sync.is_empty() {
        return;
    }
    let text = |sp: Span| {
        let (_, src) = env.sources.file(sp.file)?;
        src.get(sp.start as usize..sp.end as usize)
    };
    for s in programs
        .iter()
        .flat_map(|p| &p.items)
        .filter_map(|i| match i {
            Item::Static(s) => Some(s),
            _ => None,
        })
    {
        let Some(ty) = env.statics.get(&s.name) else {
            continue;
        };
        if ctx.has_trait(ty, sync) {
            continue;
        }
        // `Cell<T>` / `RwLock<T>` around a `T`: the wrapper is replaced,
        // with the argument of its `::new(..)`.
        let wrapped = match ty {
            HirTy::Struct(id) => {
                let d = ctx.struct_def(*id);
                let base = d.name.split("__").next().unwrap_or_default();
                ["core::sync::Cell", "core::sync::RwLock"]
                    .contains(&base)
                    .then(|| d.generic_args.first().cloned())
                    .flatten()
                    .map(|t| (base == "core::sync::Cell", t))
            }
            _ => None,
        };
        let arg = match s.value.as_ref().map(|v| &v.kind) {
            Some(ExprKind::Call { args, .. }) if wrapped.is_some() && args.len() == 1 => {
                match &args[0] {
                    nabic_ast::CallArg::Positional(e) => text(e.span),
                    _ => None,
                }
            }
            _ => s
                .value
                .as_ref()
                .and_then(|v| text(v.span))
                .filter(|_| wrapped.is_none()),
        }
        .unwrap_or("..");
        let name = leaf_name(&s.name);
        let (inner, cell) = match &wrapped {
            Some((cell, t)) => (t, *cell),
            None => (ty, false),
        };
        let atomic = cell && matches!(inner, HirTy::Prim(p) if p.is_int() || *p == PrimTy::Bool);
        let inner_name = ctx.ty_name(inner);
        let help = if atomic {
            format!(
                "help: write `static {name}: Atomic<{inner_name}> = Atomic::new({arg});` and use \
                 `{name}.load()` / `{name}.store(..)`"
            )
        } else {
            format!(
                "help: write `static {name}: Mutex<{inner_name}> = Mutex::new({arg});` and use it \
                 inside `using g = {name}.lock() {{ .. }}`"
            )
        };
        ctx.emit_error_at(
            Code::THREAD_SAFETY,
            s.span,
            format!(
                "a `static` is shared by every thread, so it can't be {}: two threads could \
                 change it at once",
                nabic_diagnostic::article(&ctx.ty_name(ty))
            ),
        );
        if let Some(d) = env.sink.diagnostics.borrow_mut().last_mut() {
            d.notes.push(help);
        }
    }
}

/// `#[internal]` marks what the toolchain's packages share;
/// on any other package's item it is an error.
pub(crate) fn check_internal_marks(
    env: &Env<'_>,
    checked: &[&Program],
    own: &dyn Fn(&str) -> bool,
) {
    let mut found: Vec<(String, Span)> = Vec::new();
    let mut add = |name: &str, attrs: &[nabic_ast::Attribute], span: Span| {
        if attrs.iter().any(|a| a.path == ["internal"]) {
            found.push((leaf_name(name).to_owned(), span));
        }
    };
    for item in checked.iter().flat_map(|p| &p.items) {
        let module = match item {
            Item::Impl(i) => i.module.clone(),
            Item::Fn(f) => module_of(&f.name),
            Item::Struct(s) => module_of(&s.name),
            Item::Const(c) => module_of(&c.name),
            Item::Static(s) => module_of(&s.name),
            _ => continue,
        };
        if !own(&module) || is_toolchain_module(&module) {
            continue;
        }
        match item {
            Item::Fn(f) => add(&f.name, &f.attrs, f.span),
            Item::Const(c) => add(&c.name, &c.attrs, c.span),
            Item::Static(s) => add(&s.name, &s.attrs, s.span),
            Item::Struct(s) => {
                if let AstStructKind::Named(fs) = &s.kind {
                    for f in fs {
                        add(&f.name, &f.attrs, f.span);
                    }
                }
            }
            Item::Impl(i) => {
                for it in &i.items {
                    match it {
                        nabic_ast::ImplItem::Method(m) => add(&m.name, &m.attrs, m.span),
                        nabic_ast::ImplItem::Const(c) => add(&c.name, &c.attrs, c.span),
                        nabic_ast::ImplItem::AssocType { .. } => {}
                    }
                }
            }
            _ => {}
        }
    }
    let mut sink = env.sink.diagnostics.borrow_mut();
    for (name, span) in found {
        sink.push(nabic_diagnostic::NabiDiagnostic::error(
            Code::MISPLACED_ATTRIBUTE,
            span,
            format!(
                "`#[internal]` on `{name}`: it marks items the Nabi toolchain's own packages \
                 share; to keep an item inside this package, write `pub(pkg)` \
                 instead"
            ),
        ));
    }
}
