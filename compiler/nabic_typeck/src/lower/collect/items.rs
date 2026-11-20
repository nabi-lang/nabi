//! The item pass: one stage's declarations into the tables.

#[allow(clippy::wildcard_imports)]
use crate::lower::collect::*;

/// Collect `owned` on top of `base`: the library's on an empty base, then
/// the package's on the library's (`lib`). `use`s and re-exports are the
/// stage's own.
#[allow(clippy::cognitive_complexity, clippy::too_many_lines)] // linear sequence of collecting passes
pub(crate) fn collect<'a>(
    owned: &'a mut Program,
    item_modules: &[String],
    lib: &'a [Item],
    base: Collected<'a>,
    raw_uses: &[(String, Visibility, nabic_ast::UseTree)],
    reexports: &[crate::uses::ReExport],
    at: &Stage<'_>,
) -> Result<(Collected<'a>, &'a Program), Box<HirModule>> {
    let Stage {
        prec,
        uses,
        input,
        sources,
    } = *at;
    let Collected {
        t: base_t,
        mut early,
        mut pre_diags,
        reexports: base_reexports,
        mut enum_decls,
        mut struct_decls,
        mut inherent_keys,
        mut static_defs,
        static_asts: base_static_asts,
        mut lib_decl,
        mut link_names,
        mut templated,
        mut system_fns,
        mut no_safepoint,
        mut ffi,
        mut wasm_imports,
        mut extern_symbols,
        trait_defs: base_trait_defs,
        mut exports,
        mut prebuilt,
        mut c_defs,
        mut diverging,
    } = base;
    let Tables {
        fn_table,
        fn_leaves: _,
        extern_fns,
        method_table,
        key_of: base_key_of,
        infer_ret: base_infer_ret,
        implicit_main: _,
        main_lets: _,
        harness_main: _,
        may_diverge: base_may_diverge,
        duplicated: _,
        method_info,
        defs,
        consts,
        const_leaves: _,
        generic_fns,
        generic_methods,
        intrinsics,
        generic_intrinsics,
        extern_fn_ids,
        nabi_externs,
        c_variadic,
        c_defs: base_c_defs,
        stdcall_fns: base_stdcall,
        sink,
        item_visibility,
        field_vis,
        impls,
        unsafe_fns,
        neg_auto,
        auto_memo: _,
        affine_memo: _,
        trait_info: base_trait_info,
        statics: static_tys,
        static_leaves: _,
        mut_statics,
        extern_statics,
        literal_shapes,
        names: base_names,
        decls,
        non_exhaustive,
        sealed,
        fn_marks,
        marks,
        mark_pkg: _,
        lint_library: _,
        bound_methods: _,
        bound_traits: _,
    } = base_t;
    for (item, module) in owned.items.iter_mut().zip(item_modules) {
        qualify(item, module);
    }
    // `#[lang(...)]` slots over library + user items; a slot
    // bound twice stops lowering.
    let (lang, lang_diags) = nabic_parser::collect_lang_table_onto(base_names.lang.clone(), owned);
    if lang_diags
        .iter()
        .any(nabic_diagnostic::NabiDiagnostic::is_error)
    {
        return Err(Box::new(HirModule {
            diagnostics: lang_diags,
            ..HirModule::default()
        }));
    }
    early.extend(lang_diags);
    let all_items: Vec<&Item> = lib.iter().chain(&owned.items).collect();
    let (mut stage_reexports, glob_diags) =
        crate::uses::glob_reexports(raw_uses, &all_items, uses, reexports);
    // A `pub` function of an `extern` block in module `m` is
    // also `m::f` (its symbol stays `f`).
    for (item, module) in owned.items.iter().zip(item_modules) {
        let Item::Extern(b) = item else { continue };
        for ei in &b.items {
            match ei {
                nabic_ast::ExternItem::Fn(f)
                    if !module.is_empty() && f.vis != Visibility::Private =>
                {
                    stage_reexports.push(crate::uses::ReExport {
                        module_path: module.clone(),
                        short_name: f.name.clone(),
                        full_path: f.name.clone(),
                        vis: f.vis,
                    });
                }
                _ => {}
            }
        }
    }
    early.extend(glob_diags);
    let mut reexports = base_reexports;
    reexports.extend(stage_reexports);
    let reexports = reexports;
    // A prebuilt dependency's build has no user package to advise.
    let missing = crate::uses::Missing {
        script: input.script.as_ref(),
        packages: &input.packages,
    };
    let (use_diags, hiding) = crate::uses::check_use_targets(
        raw_uses,
        &all_items,
        uses,
        &reexports,
        input.lib.is_none().then_some(missing),
    );
    early.extend(use_diags);
    let values: nabic_core::HashSet<&str> = (all_items.iter())
        .filter_map(|i| match i {
            Item::Fn(f) => Some(f.name.as_str()),
            Item::Const(c) => Some(c.name.as_str()),
            Item::Static(s) => Some(s.name.as_str()),
            _ => None,
        })
        .collect();
    early.extend(crate::uses::check_use_clashes(
        raw_uses,
        uses,
        &reexports,
        &|k| values.contains(k),
    ));
    drop(values);
    let (hiding_diags, hiding): (Vec<_>, Vec<_>) = hiding.into_iter().unzip();
    pre_diags.extend(hiding_diags);
    early.extend(check_tiers(raw_uses, &owned.items, item_modules));
    let block_globs = block_glob_renames(&owned.items, item_modules, &all_items, &reexports, uses);
    drop(all_items);
    for (i, r) in block_globs {
        r.item(&mut owned.items[i]);
    }
    add_lazy_static_inits(owned);
    pub_signatures(owned, &mut pre_diags);
    implicit_generics(owned);
    some_params(owned);
    let some_tys = some_types(owned, &mut pre_diags);
    let err =
        |code: Code, pre: &mut Vec<nabic_diagnostic::NabiDiagnostic>, at: Span, msg: String| {
            pre.push(nabic_diagnostic::NabiDiagnostic::error(code, at, msg));
        };
    let program: &Program = &*owned;
    // `#[derive]` expanded into impls by the parser; a union has
    // no field-wise meaning.
    for item in &program.items {
        if let Item::Union(u) = item {
            if let Some((_, at)) = nabic_parser::derive::derive_names(&u.attrs).first() {
                err(
                    Code::CANNOT_DERIVE,
                    &mut pre_diags,
                    *at,
                    format!("cannot derive traits for union `{}`", u.name),
                );
            }
        }
    }

    // A field / variant name declared twice in one type.
    for item in &program.items {
        let (kind, owner, names): (&str, &str, Vec<(&str, nabic_diagnostic::Span)>) = match item {
            Item::Struct(s) => match &s.kind {
                nabic_ast::StructKind::Named(fs) => (
                    "field",
                    &s.name,
                    fs.iter().map(|f| (f.name.as_str(), f.span)).collect(),
                ),
                _ => continue,
            },
            Item::Union(u) => (
                "field",
                &u.name,
                u.fields.iter().map(|f| (f.name.as_str(), f.span)).collect(),
            ),
            Item::Enum(e) => (
                "variant",
                &e.name,
                e.variants
                    .iter()
                    .map(|v| (v.name.as_str(), v.span))
                    .collect(),
            ),
            _ => continue,
        };
        for (i, (n, at)) in names.iter().enumerate() {
            if names[..i].iter().any(|(m, _)| m == n) {
                err(
                    Code::DUPLICATE_DEFINITION,
                    &mut pre_diags,
                    *at,
                    format!(
                        "{kind} `{n}` is declared more than once in `{}`",
                        leaf_name(owner)
                    ),
                );
            }
        }
    }

    // The type namespace: every type-like (already qualified) name goes into
    // one index; a duplicate is an error and skipped so ids stay consistent.
    let mut names = base_names;
    // An answer for a package module may change with its items.
    names
        .canon_memo
        .get_mut()
        .retain(|(_, module), _| !package_of(module).is_empty());
    let mut type_spans: HashMap<&str, Span> = HashMap::default();
    for item in &program.items {
        let (name, vis) = match item {
            Item::Struct(s) => (&s.name, s.vis),
            Item::Enum(e) => (&e.name, e.vis),
            Item::Union(u) => (&u.name, u.vis),
            Item::TypeAlias(a) => (&a.name, a.vis),
            Item::Trait(t) => {
                names.traits.insert(t.name.clone());
                if t.is_unsafe {
                    names.unsafe_traits.insert(t.name.clone());
                }
                let mut methods: HashMap<&str, Span> = HashMap::default();
                for ti in &t.items {
                    let nabic_ast::TraitItem::Method(m) = ti else {
                        continue;
                    };
                    if let Some(first) = methods.insert(&m.name, m.span) {
                        pre_diags.push(nabic_diagnostic::duplicate(
                            Code::DUPLICATE_DEFINITION,
                            format!(
                                "method `{}` is defined more than once in trait `{}`",
                                m.name, t.name
                            ),
                            &m.name,
                            m.span,
                            Some(first),
                            true,
                        ));
                    }
                }
                (&t.name, t.vis)
            }
            _ => continue,
        };
        if names.names.insert(name.clone()) {
            names.visibility.insert(name.clone(), vis);
            type_spans.insert(name, item.span());
        } else {
            pre_diags.push(nabic_diagnostic::duplicate(
                Code::DUPLICATE_DEFINITION,
                format!("type `{name}` is defined more than once"),
                name,
                item.span(),
                type_spans.get(name.as_str()).copied(),
                false,
            ));
        }
    }
    // `pub use` aliases -> what they name, chains followed to the item.
    let aliases = reexport_aliases(&reexports);
    names.reexports = first_alias(aliases.iter().filter(|(_, end)| names.names.contains(end)));
    // `Enum::Variant` keys, so `use E::{A}` / `use E::*` (and the
    // library's implicit `core::**`) can bring variants in bare.
    names.variants.extend(
        program
            .items
            .iter()
            .filter_map(|it| match it {
                Item::Enum(e) => Some(e),
                _ => None,
            })
            .flat_map(|e| e.variants.iter().map(|v| format!("{}::{}", e.name, v.name))),
    );
    // Structural auto traits: a type has them unless a component lacks them.
    names.auto_traits = ["send_trait", "sync_trait"]
        .into_iter()
        .filter_map(|s| lang.lookup(s))
        .map(|e| e.item_name.clone())
        .collect();
    names.lang = lang;
    names
        .mod_vis
        .extend(crate::lower::resolve::module_visibility(&program.items));
    let public: nabic_core::HashSet<String> = (reexports.iter())
        .filter(|r| r.vis == Visibility::Pub)
        .map(reexport_key)
        .collect();
    names.visible = crate::lower::visible_paths(
        (aliases.iter())
            .filter(|(alias, _)| public.contains(alias))
            .map(|(alias, end)| (alias.as_str(), end.as_str())),
        &names.mod_vis,
    );
    names.uses.clone_from(uses);
    names.some_types.extend(some_tys);
    for it in &program.items {
        if matches!(it, Item::TypeAlias(a) if matches!(a.ty.kind, AstTyKind::Some(_))) {
            continue;
        }
        match it {
            Item::TypeAlias(a) if !a.generics.params.is_empty() || !a.where_clause.is_empty() => {
                names.generic_aliases.insert(a.name.clone(), a.clone());
            }
            _ => {}
        }
        match it {
            Item::TypeAlias(a) if a.generics.params.is_empty() => {
                names.plain_aliases.insert(a.name.clone(), a.ty.clone());
            }
            Item::Const(c) => {
                names
                    .consts
                    .insert(c.name.clone(), (c.ty.clone(), c.value.clone()));
            }
            _ => {}
        }
    }
    // The leaf indexes are built on the first lookup, over every stage.
    names.leaves = std::cell::OnceCell::new();
    names.variant_leaves = std::cell::OnceCell::new();
    names.const_leaves = std::cell::OnceCell::new();
    let statics: nabic_core::HashSet<&str> = program
        .items
        .iter()
        .filter_map(|it| match it {
            Item::Static(s) => Some(s.name.as_str()),
            _ => None,
        })
        .chain(static_tys.keys().map(String::as_str))
        .collect();
    names.value_reexports = first_alias(
        aliases
            .iter()
            .filter(|(_, end)| names.consts.contains_key(end) || statics.contains(end.as_str())),
    );
    drop(statics);
    let aliased: Vec<(String, (Ty, Expr))> = names
        .value_reexports
        .iter()
        .filter_map(|(a, end)| Some((a.clone(), names.consts.get(end)?.clone())))
        .collect();
    for (alias, c) in aliased {
        names.consts.entry(alias).or_insert(c);
    }
    check_assoc_shadowing(program, &names, &mut pre_diags);
    let defaults = assoc_defaults(program, &names, &base_trait_defs);
    for (i, added) in defaults {
        if let Item::Impl(imp) = &mut owned.items[i] {
            imp.items.extend(added);
        }
    }
    expand_impl_aliases(owned, &names);
    impl_param_types(owned, &names, &base_trait_defs);
    add_self_assoc_sigs(owned);
    let program: &'a Program = owned;
    check_bound_names(program, &names, &base_trait_defs, &mut pre_diags);
    check_niche_attrs(program, &mut pre_diags);
    check_repr_attrs(program, &mut pre_diags);

    // Every struct / union / enum gets its id before any type resolves;
    // generic ones are templates instantiated in the same id space
    // (`TypeDefs`). Unions follow the structs.
    let mut defs = defs;
    let (first_enum, first_struct) = (defs.enums.borrow().len(), defs.structs.borrow().len());
    let enum_id = |n: usize| EnumId(u32::try_from(first_enum + n).expect("EnumId overflow"));
    let struct_id =
        |n: usize| StructId(u32::try_from(first_struct + n).expect("StructId overflow"));
    let mut new_enums: Vec<&AstEnumDef> = Vec::new();
    let mut new_structs: Vec<&AstStructDef> = Vec::new();
    let mut union_decls: Vec<&nabic_ast::UnionDef> = Vec::new();
    for item in &program.items {
        match item {
            Item::Enum(e) => {
                if defs.enum_table.borrow().contains_key(&e.name)
                    || defs.generic_enums.contains_key(&e.name)
                {
                    continue;
                }
                if !e.generics.params.is_empty() {
                    if mono::all_defaulted(&e.generics) {
                        defs.defaulted_leaves.insert(leaf_name(&e.name).to_owned());
                    }
                    defs.generic_enums.insert(e.name.clone(), e);
                    continue;
                }
                let id = enum_id(new_enums.len());
                crate::name_type(true, id.0, &e.name, &[]);
                defs.enum_table.borrow_mut().insert(e.name.clone(), id);
                new_enums.push(e);
            }
            Item::Struct(s) => {
                if defs.struct_table.borrow().contains_key(&s.name)
                    || defs.generic_structs.contains_key(&s.name)
                {
                    continue;
                }
                if !s.generics.params.is_empty() {
                    if mono::all_defaulted(&s.generics) {
                        defs.defaulted_leaves.insert(leaf_name(&s.name).to_owned());
                    }
                    defs.generic_structs.insert(s.name.clone(), s);
                    continue;
                }
                let id = struct_id(new_structs.len());
                crate::name_type(false, id.0, &s.name, &[]);
                defs.struct_table.borrow_mut().insert(s.name.clone(), id);
                new_structs.push(s);
            }
            _ => {}
        }
    }
    for item in &program.items {
        if let Item::Union(u) = item {
            if defs.struct_table.borrow().contains_key(&u.name)
                || defs.generic_structs.contains_key(&u.name)
                || defs.generic_unions.contains_key(&u.name)
            {
                continue;
            }
            if !u.generics.params.is_empty() {
                if u.fields.is_empty() {
                    union_storage_strategy(&[], false, &u.name, u.span, &mut pre_diags);
                }
                if mono::all_defaulted(&u.generics) {
                    defs.defaulted_leaves.insert(leaf_name(&u.name).to_owned());
                }
                defs.generic_unions.insert(u.name.clone(), u);
                continue;
            }
            let id = struct_id(new_structs.len() + union_decls.len());
            crate::name_type(false, id.0, &u.name, &[]);
            defs.struct_table.borrow_mut().insert(u.name.clone(), id);
            union_decls.push(u);
        }
    }
    // Declared ones first (field types not resolved yet), so an instance
    // made while resolving them takes the next id.
    for (i, e) in new_enums.iter().enumerate() {
        defs.enums.borrow_mut().push(HirEnumDef {
            id: enum_id(i),
            repr: crate::Repr::default(),
            def_id: None,
            name: e.name.clone(),
            variants: Vec::new(),
            span: e.span,
            generic_args: Vec::new(),
        });
    }
    let struct_shells = new_structs
        .iter()
        .map(|s| (&s.name, s.span))
        .chain(union_decls.iter().map(|u| (&u.name, u.span)));
    for (i, (name, span)) in struct_shells.enumerate() {
        defs.structs.borrow_mut().push(HirStructDef {
            id: struct_id(i),
            repr: crate::Repr::default(),
            def_id: None,
            name: name.clone(),
            fields: Vec::new(),
            span,
            generic_args: Vec::new(),
            union_kind: None,
            niche: Vec::new(),
        });
    }
    for (i, e) in new_enums.iter().enumerate() {
        let id = enum_id(i);
        let module = module_of(&e.name);
        let sc = Scope {
            names: &names,
            module: &module,
            at: e.span,
        };
        let def = lower_enum_def(e, id, &defs, sc);
        defs.enums.borrow_mut()[id.0 as usize] = def;
    }
    for (i, s) in new_structs.iter().enumerate() {
        let id = struct_id(i);
        let module = module_of(&s.name);
        let sc = Scope {
            names: &names,
            module: &module,
            at: s.span,
        };
        let def = lower_struct_def(s, id, &defs, sc);
        defs.structs.borrow_mut()[id.0 as usize] = def;
    }
    for (i, u) in union_decls.iter().enumerate() {
        let id = struct_id(new_structs.len() + i);
        let module = module_of(&u.name);
        let sc = Scope {
            names: &names,
            module: &module,
            at: u.span,
        };
        let def = lower_union_def(u, id, &defs, sc, &mut pre_diags);
        defs.structs.borrow_mut()[id.0 as usize] = def;
    }
    let first_new_enum = enum_decls.len();
    enum_decls.extend(new_enums.iter().enumerate().map(|(i, e)| (enum_id(i), *e)));
    struct_decls.extend(
        new_structs
            .iter()
            .enumerate()
            .map(|(i, s)| (struct_id(i), *s)),
    );
    if !names.some_types.is_empty() {
        let leaves: Vec<&str> = names.some_types.keys().map(|x| leaf_name(x)).collect();
        for (i, s) in new_structs.iter().enumerate() {
            let tys: Vec<Ty> = match &s.kind {
                nabic_ast::StructKind::Named(fs) => fs.iter().map(|f| f.ty.clone()).collect(),
                nabic_ast::StructKind::Tuple(fs) => fs.iter().map(|f| f.ty.clone()).collect(),
                nabic_ast::StructKind::Unit => continue,
            };
            let mut v = mono::SomePaths(Vec::new());
            for t in &tys {
                crate::walk::walk_ty(&mut v, t);
            }
            if s.generics.params.is_empty() && v.0.iter().any(|l| leaves.contains(&l.as_str())) {
                names.some_fields.push((struct_id(i), tys));
            }
        }
    }

    // Module-level `const`s, kept as AST and inlined at each use; `static`s
    // get global storage (`HirStaticDef`).
    let mut consts = consts;
    // Per impl item: its associated consts, for its `Impl`.
    let mut impl_const_lists: HashMap<usize, Vec<AssocConst>> = HashMap::default();
    // Where this stage's consts are, for a later item of their name.
    let mut const_spans: HashMap<String, Span> = HashMap::default();
    // Module-level consts and statics: one value namespace with fns.
    let mut value_spans: HashMap<String, (Span, &str)> = HashMap::default();
    // The kept definition of each name defined again (`Env::duplicated`),
    // and each item hiding an import.
    let mut duplicated: nabic_core::HashSet<Span> = hiding.into_iter().collect();
    // A tuple or unit struct's constructor is a value too.
    for item in &program.items {
        if let Item::Struct(s) = item {
            match s.kind {
                nabic_ast::StructKind::Tuple(_) => {
                    value_spans.insert(s.name.clone(), (s.span, "tuple struct"))
                }
                nabic_ast::StructKind::Unit => {
                    value_spans.insert(s.name.clone(), (s.span, "unit struct"))
                }
                nabic_ast::StructKind::Named(_) => None,
            };
        }
    }
    for (idx, item) in program.items.iter().enumerate() {
        // An impl's `const` is `Type::NAME`; a trait impl also gets
        // the trait's defaults.
        let item_consts: Vec<(String, Ty, Expr)> = match item {
            Item::Const(c) => vec![(c.name.clone(), c.ty.clone(), c.value.clone())],
            Item::Impl(i) => {
                let (keyed, all) =
                    impl_consts(i, program, &base_trait_defs, &names, &mut pre_diags);
                impl_const_lists.insert(idx, all);
                keyed
            }
            _ => Vec::new(),
        };
        // Two trait impls may both give `S::N` (ambiguous where read); an
        // inherent `S::N` shadows them, as for methods.
        let trait_impl = matches!(item, Item::Impl(i) if i.trait_path.is_some());
        // An inherent impl's own consts come first, in order.
        let spans: Vec<Span> = match item {
            Item::Impl(i) => i
                .items
                .iter()
                .filter_map(|it| match it {
                    AstImplItem::Const(c) => Some(c.span),
                    _ => None,
                })
                .collect(),
            _ => vec![item.span()],
        };
        for (n, (key, ty, value)) in item_consts.into_iter().enumerate() {
            let here = spans.get(n).copied().unwrap_or_else(|| item.span());
            if trait_impl {
                if !inherent_keys.contains(&key) {
                    consts.insert(key, (ty, value));
                }
            } else if let Some(&(first, _)) = value_spans
                .get(&key)
                .filter(|_| matches!(item, Item::Const(_)))
            {
                duplicated.insert(first);
                pre_diags.push(nabic_diagnostic::duplicate(
                    Code::DUPLICATE_DEFINITION,
                    value_dup("const", &key, &value_spans),
                    &key,
                    here,
                    Some(first),
                    false,
                ));
            } else if inherent_keys.insert(key.clone()) {
                if matches!(item, Item::Const(_)) {
                    value_spans.insert(key.clone(), (here, "const"));
                }
                const_spans.insert(key.clone(), here);
                consts.insert(key, (ty, value));
            } else {
                pre_diags.push(nabic_diagnostic::duplicate(
                    Code::DUPLICATE_DEFINITION,
                    format!("const `{key}` is defined more than once"),
                    &key,
                    here,
                    const_spans.get(&key).copied(),
                    false,
                ));
            }
        }
    }
    for (alias, end) in &names.value_reexports {
        if let Some(c) = consts.get(end).cloned() {
            consts.entry(alias.clone()).or_insert(c);
        }
    }
    let first_static = static_defs.len();
    let mut static_asts = base_static_asts;
    let mut static_tys = static_tys;
    for (idx, item) in program.items.iter().enumerate() {
        let here = item.span();
        if let Item::Static(s) = item {
            if static_tys.contains_key(&s.name) || value_spans.contains_key(&s.name) {
                let first = static_defs
                    .iter()
                    .find(|d| d.name == s.name)
                    .map(|d| d.span)
                    .or_else(|| value_spans.get(&s.name).map(|(at, _)| *at));
                duplicated.extend(first);
                pre_diags.push(nabic_diagnostic::duplicate(
                    Code::DUPLICATE_DEFINITION,
                    value_dup("static", &s.name, &value_spans),
                    &s.name,
                    here,
                    first,
                    false,
                ));
                continue;
            }
            value_spans.insert(s.name.clone(), (here, "static"));
            // Its type (and literal initializer) once every item is
            // registered.
            static_asts.push((&s.ty, module_of(&s.name)));
            static_tys.insert(s.name.clone(), HirTy::Unknown);
            static_defs.push(HirStaticDef {
                name: s.name.clone(),
                ty: HirTy::Unknown,
                init: ConstVal::Unit,
                is_mut: s.is_mut,
                is_extern: false,
                dll_import: false,
                init_fn: None,
                span: s.span,
            });
        }
        // `extern "C" { static errno: i32; }`: a C global, read (and with
        // `mut` written) like a `static`.
        if let Item::Extern(blk) = item {
            let dll_import = !links_static(&blk.attrs);
            for ei in &blk.items {
                let nabic_ast::ExternItem::Static(s) = ei else {
                    continue;
                };
                if static_tys.contains_key(&s.name) {
                    pre_diags.push(nabic_diagnostic::duplicate(
                        Code::DUPLICATE_DEFINITION,
                        format!("static `{}` is defined more than once", s.name),
                        &s.name,
                        s.span,
                        static_defs
                            .iter()
                            .find(|d| d.name == s.name)
                            .map(|d| d.span),
                        false,
                    ));
                    continue;
                }
                // Its type is resolved in the block's module.
                let module = item_modules.get(idx).cloned().unwrap_or_default();
                static_asts.push((&s.ty, module));
                static_tys.insert(s.name.clone(), HirTy::Unknown);
                static_defs.push(HirStaticDef {
                    name: s.name.clone(),
                    ty: HirTy::Unknown,
                    init: ConstVal::Unit,
                    is_mut: s.is_mut,
                    is_extern: true,
                    dll_import,
                    init_fn: None,
                    span: s.span,
                });
            }
        }
    }
    // Each plain type alias resolved once, for its diagnostics (a use
    // expands the alias where it is resolved).
    for item in &program.items {
        if let Item::TypeAlias(a) = item {
            if !a.generics.params.is_empty() || matches!(a.ty.kind, AstTyKind::Some(_)) {
                continue;
            }
            let module = module_of(&a.name);
            let sc = Scope {
                names: &names,
                module: &module,
                at: item.span(),
            };
            resolve_ty_with_structs(&a.ty, &defs, None, sc);
        }
    }
    // An alias naming itself, directly or through other aliases and
    // anywhere in the type (`type A = B; type B = A;`, `type T = List<T>;`).
    {
        let alias_items: HashMap<&str, &nabic_ast::TypeAliasDef> = program
            .items
            .iter()
            .filter_map(|i| match i {
                Item::TypeAlias(a)
                    if a.generics.params.is_empty() && !matches!(a.ty.kind, AstTyKind::Some(_)) =>
                {
                    Some((a.name.as_str(), a))
                }
                _ => None,
            })
            .collect();
        let mentions = |a: &nabic_ast::TypeAliasDef| {
            struct Names(Vec<String>);
            impl crate::walk::AstVisitor for Names {
                fn ty(&mut self, t: &nabic_ast::Ty) {
                    // `B` and `B::Out`'s base `B`.
                    if let nabic_ast::TyKind::Path(p) = &t.kind {
                        if let [s] | [s, _] = p.segments.as_slice() {
                            self.0.push(s.name.clone());
                        }
                    }
                }
            }
            let mut v = Names(Vec::new());
            crate::walk::walk_ty(&mut v, &a.ty);
            let module = module_of(&a.name);
            v.0.into_iter()
                .map(|n| {
                    if module.is_empty() {
                        n
                    } else {
                        format!("{module}::{n}")
                    }
                })
                .filter(|n| alias_items.contains_key(n.as_str()))
                .collect::<Vec<_>>()
        };
        let mut cyclic: Vec<&nabic_ast::TypeAliasDef> = alias_items
            .values()
            .filter(|a| {
                let mut stack = mentions(a);
                let mut seen = nabic_core::HashSet::default();
                while let Some(n) = stack.pop() {
                    if n == a.name {
                        return true;
                    }
                    if seen.insert(n.clone()) {
                        stack.extend(mentions(alias_items[n.as_str()]));
                    }
                }
                false
            })
            .copied()
            .collect();
        cyclic.sort_by(|a, b| a.name.cmp(&b.name));
        for a in cyclic {
            names.broken_aliases.borrow_mut().insert(a.name.clone());
            err(
                Code::CYCLIC_DEFINITION,
                &mut pre_diags,
                a.span,
                format!("type alias `{}` is cyclic", a.name),
            );
        }
    }

    // Functions, then `impl` methods. Generic functions stay AST templates,
    // monomorphized per call, outside `decls` / `fn_table`.
    let mut fn_table = fn_table;
    let mut extern_fns = extern_fns;
    // Later extern declarations of a name already declared: whether
    // `extern "nabi"`, the first one's id.
    let mut extern_redecls: Vec<(&FnDef, bool, FunctionId)> = Vec::new();
    let mut impls = impls;
    let mut decls = decls;
    let first_decl = decls.len();
    let mut generic_fns = generic_fns;
    // Generic impl method templates are `__impl{tag}_m`, the tag hashing the
    // impl's module, trait and target so it is stable; repeats get a count.
    let mut intrinsics = intrinsics;
    let mut generic_intrinsics = generic_intrinsics;
    let mut unsafe_fns = unsafe_fns;
    let mut fn_marks = fn_marks;
    let mut extern_fn_ids = extern_fn_ids;
    let mut c_variadic = c_variadic;
    // `extern "nabi"` declarations: the runtime's own ABI.
    let mut nabi_externs = nabi_externs;
    // A declaration's signature: its types are resolved once every item
    // is registered (`mono::decl_sig`), the rest is read off it here.
    let sig_of = |fn_def: &FnDef, id: FunctionId, sc: Scope<'_>| FnSig {
        id,
        ret_ty: HirTy::Unknown,
        param_defaults: fn_def
            .params
            .iter()
            .map(|p| p.default.clone().map(|d| in_module(sc.module, d)))
            .collect(),
        param_tys: vec![HirTy::Unknown; fn_def.params.len()],
        param_names: fn_def.params.iter().map(|p| p.name.clone()).collect(),
        param_refs: fn_def.params.iter().map(|p| p.by_ref).collect(),
        variadic: fn_def.params.last().is_some_and(|p| p.variadic),
    };
    for (idx, item) in program.items.iter().enumerate() {
        let here = item.span();
        match item {
            Item::Fn(fn_def) => {
                let name = &fn_def.name;
                if fn_table.contains_key(name)
                    || generic_fns.contains_key(name)
                    || value_spans.contains_key(name)
                {
                    let first = generic_fns
                        .get(name)
                        .map(|f| f.span)
                        .or_else(|| {
                            let id = fn_table.get(name)?.id.0 as usize;
                            decls.get(id).map(decl_span)
                        })
                        .or_else(|| value_spans.get(name).map(|(at, _)| *at));
                    duplicated.extend(first);
                    pre_diags.push(nabic_diagnostic::duplicate(
                        Code::DUPLICATE_DEFINITION,
                        value_dup("function", name, &value_spans),
                        name,
                        here,
                        first,
                        !value_spans.contains_key(name),
                    ));
                    continue;
                }
                let module = module_of(&fn_def.name);
                if !fn_def.generics.params.is_empty() {
                    if let Some(i) = intrinsic_of(fn_def, &module, &mut pre_diags) {
                        generic_intrinsics.insert(fn_def.name.clone(), i);
                    }
                    generic_fns.insert(fn_def.name.clone(), fn_def);
                    continue;
                }
                let id = FunctionId(u32::try_from(decls.len()).expect("FunctionId overflow"));
                if let Some(i) = intrinsic_of(fn_def, &module, &mut pre_diags) {
                    intrinsics.insert(id, i);
                }
                let sc = Scope {
                    names: &names,
                    module: &module,
                    at: here,
                };
                let sig = sig_of(fn_def, id, sc);
                fn_table.insert(fn_def.name.clone(), std::sync::Arc::new(sig));
                decls.push(MethodOrFn::Fn(fn_def));
                lib_decl.push(!package_of(&module).is_empty());
            }
            Item::Extern(blk) => {
                let nabi_abi = blk.abi == "nabi";
                let quiet = blk.attrs.iter().any(|a| a.path == ["no_safepoint"]);
                if quiet && !nabi_abi {
                    err(
                        Code::MISPLACED_ATTRIBUTE,
                        &mut pre_diags,
                        here,
                        "`#[no_safepoint]` is for `extern \"nabi\"` blocks".to_owned(),
                    );
                }
                // `"system"` is `"C"` but on 32-bit x86 Windows (the
                // driver rejects it there).
                if !nabi_abi && blk.abi != "C" && blk.abi != "system" {
                    err(
                        Code::INVALID_ABI,
                        &mut pre_diags,
                        here,
                        format!(
                            "unsupported ABI `\"{}\"` (`\"C\"`, `\"system\"`, `\"objc\"`, or `\"nabi\"` for the runtime)",
                            blk.abi
                        ),
                    );
                }
                let import_module = wasm_import_module(&blk.attrs);
                let module = item_modules.get(idx).cloned().unwrap_or_default();
                for ei in &blk.items {
                    let nabic_ast::ExternItem::Fn(fn_def) = ei else {
                        continue;
                    };
                    // `nabi_*` is the runtime's prefix, whose `str` is
                    // `(ptr, len)`; `extern "C"` would make it a `char*`.
                    let is_str = |t: &nabic_ast::Ty| matches!(&t.kind, AstTyKind::Path(p) if p.segments.len() == 1 && p.segments[0].name == "str");
                    if !nabi_abi
                        && fn_def.name.starts_with("nabi_")
                        && (fn_def
                            .params
                            .iter()
                            .filter_map(|p| p.ty.as_ref())
                            .any(is_str)
                            || fn_def.ret_ty.as_ref().is_some_and(is_str))
                    {
                        err(
                            Code::FFI,
                            &mut pre_diags,
                            fn_def.span,
                            format!(
                                "`{}`: a `nabi_*` symbol is the runtime's, whose `str` is \
                                 `(ptr, len)`; an `extern \"C\"` `str` is a `char*` \
                                 - give the C function another name",
                                fn_def.name
                            ),
                        );
                    }
                    // A repeated extern declaration names the same symbol:
                    // keep the first, check the signatures agree below.
                    if let Some(first) = fn_table.get(&fn_def.name) {
                        if extern_fns.contains_key(&fn_def.name) {
                            extern_redecls.push((fn_def, nabi_abi, first.id));
                        }
                        continue;
                    }
                    extern_fns.insert(fn_def.name.clone(), module.clone());
                    let id = FunctionId(u32::try_from(decls.len()).expect("FunctionId overflow"));
                    // An extern function is `unsafe` to call.
                    unsafe_fns.insert(id);
                    let sc = Scope {
                        names: &names,
                        module: "",
                        at: here,
                    };
                    fn_table.insert(
                        fn_def.name.clone(),
                        std::sync::Arc::new(sig_of(fn_def, id, sc)),
                    );
                    if nabi_abi {
                        nabi_externs.insert(id);
                        if quiet {
                            no_safepoint.insert(id);
                        }
                    } else {
                        extern_fn_ids.insert(id);
                        if blk.abi == "system" {
                            system_fns.insert(id);
                        }
                    }
                    let bits = ffi_flags(&blk.attrs, fn_def);
                    if bits != 0 && nabi_abi {
                        err(
                            Code::MISPLACED_ATTRIBUTE,
                            &mut pre_diags,
                            fn_def.span,
                            "`#[errno]` and `#[leaf]` are for C declarations: put them on an \
                             `extern \"C\" { .. }` block or its functions"
                                .to_owned(),
                        );
                    } else if bits != 0 {
                        ffi.insert(id, bits);
                    }
                    if fn_def
                        .attrs
                        .iter()
                        .any(|a| a.path == [nabic_ast::C_VARIADIC_ATTR])
                    {
                        c_variadic.insert(id);
                    }
                    let link_name = link_name_of(fn_def);
                    if let Some(m) = &import_module {
                        let name = link_name.unwrap_or_else(|| fn_def.name.clone());
                        wasm_imports.insert(id, (m.clone(), name));
                    } else if let Some(n) = link_name {
                        extern_symbols.insert(id, n);
                    }
                    decls.push(MethodOrFn::Fn(fn_def));
                    lib_decl.push(false);
                }
            }
            _ => {}
        }
    }
    // Pre-collect every trait declaration so `impl Trait for T` blocks
    // can pick up default method bodies.
    let mut trait_defs = base_trait_defs;
    for item in &program.items {
        if let Item::Trait(t) = item {
            trait_defs.entry(t.name.clone()).or_insert(t);
        }
    }
    // `any Tr`: each trait's method names and supertrait keys, and
    // (filled per impl below) its implementing types in source order.
    let new_info: Vec<(String, TraitInfo)> = trait_defs
        .iter()
        .filter(|(name, _)| !base_trait_info.contains_key(*name))
        .map(|(name, t)| {
            let module = module_of(name);
            let supers = t
                .supertraits
                .iter()
                .filter_map(|st| match &st.kind {
                    AstTyKind::Path(p) => {
                        let segs: Vec<&str> = p.segments.iter().map(|s| s.name.as_str()).collect();
                        Some(names.trait_key(&segs, &module, t.span))
                    }
                    _ => None,
                })
                .collect();
            let fns = || {
                t.items.iter().filter_map(|i| match i {
                    nabic_ast::TraitItem::Method(m) => Some(m),
                    _ => None,
                })
            };
            let methods = fns().map(|m| leaf_name(&m.name).to_owned()).collect();
            let params: Vec<&str> = t
                .generics
                .params
                .iter()
                .filter_map(|p| match p {
                    nabic_ast::GenericParam::Type { name, .. } => Some(name.as_str()),
                    nabic_ast::GenericParam::Const { .. } => None,
                })
                .collect();
            let assoc: Vec<&str> = t
                .items
                .iter()
                .filter_map(|i| match i {
                    nabic_ast::TraitItem::AssocType { name, .. } => Some(name.as_str()),
                    _ => None,
                })
                .collect();
            let needs = fns()
                .map(|m| {
                    let mut out = Vec::new();
                    for ty in std::iter::once(sig_ret_ast(m)).chain(
                        m.params
                            .iter()
                            .filter(|p| p.name != "self")
                            .filter_map(|p| p.ty.clone()),
                    ) {
                        ast_ty_needs(&ty, &params, &assoc, &mut out);
                    }
                    (leaf_name(&m.name).to_owned(), out)
                })
                .collect();
            (
                name.clone(),
                TraitInfo {
                    methods,
                    supers,
                    needs,
                    decls: fns()
                        .map(|m| (leaf_name(&m.name).to_owned(), m.clone()))
                        .collect(),
                    module,
                    params: params.iter().map(|p| (*p).to_owned()).collect(),
                    generics: t.generics.clone(),
                    super_tys: t.supertraits.clone(),
                    assoc: t
                        .items
                        .iter()
                        .filter_map(|i| match i {
                            nabic_ast::TraitItem::AssocType { name, bounds, .. } => {
                                Some((name.clone(), bounds.clone()))
                            }
                            _ => None,
                        })
                        .collect(),
                    consts: t
                        .items
                        .iter()
                        .filter_map(|i| match i {
                            nabic_ast::TraitItem::Const { name, ty, .. } => {
                                Some((name.clone(), ty.clone()))
                            }
                            _ => None,
                        })
                        .collect(),
                },
            )
        })
        .collect();
    let mut trait_info = base_trait_info;
    trait_info.extend(new_info);
    let mut neg_auto = neg_auto;
    // Impls registered by their written target, resolved once every item
    // is (`impl_heads`).
    let mut pending: Vec<PendingImpl<'_>> = Vec::new();

    for (idx, item) in program.items.iter().enumerate() {
        let here = item.span();
        let Item::Impl(impl_def) = item else { continue };
        let module = impl_module(impl_def);
        if !check_auto_impl(impl_def, &names, &module, &mut neg_auto, &mut pre_diags) {
            continue;
        }
        let impl_generic = !impl_def.generics.params.is_empty();
        let consts = impl_const_lists.remove(&idx).unwrap_or_default();
        let trait_name: Option<String> = impl_def.trait_path.as_ref().map(|tp| {
            let segs: Vec<&str> = tp.segments.iter().map(|s| s.name.as_str()).collect();
            names.trait_key(&segs, tp.origin.as_deref().unwrap_or(&module), here)
        });
        // A generic trait impl, or a generic inherent impl on a structural
        // type or on some instances of a base (`impl<V> P<str, V>`): matched
        // against the receiver, its methods templates.
        if impl_generic && (trait_name.is_some() || !covers_base(impl_def)) {
            let trait_key = trait_name.unwrap_or_default();
            let td = trait_defs.get(&trait_key).copied();
            let id = format!(
                "{module}|{trait_key}|{}",
                impl_def_display(&impl_def.self_ty)
            );
            let mut tag = format!("{:08x}", nabic_core::hash::fnv(id.as_bytes()) as u32);
            let seen = templated.entry(tag.clone()).or_insert(0);
            if *seen > 0 {
                tag = format!("{tag}_{seen}");
            }
            *seen += 1;
            let mut gi = generic_trait_impl(
                impl_def,
                trait_key,
                td,
                &module,
                &tag,
                &mut pre_diags,
                &mut duplicated,
            );
            gi.consts = consts;
            impls.push(gi);
            continue;
        }
        // Generic inherent methods (and generic methods anywhere) are
        // templates keyed by the target's generic base.
        let generic_base = match &impl_def.self_ty.kind {
            AstTyKind::Path(p) => {
                let segs: Vec<&str> = p.segments.iter().map(|s| s.name.as_str()).collect();
                // `impl str { fn parse<T>(..) }`: a primitive's generic
                // methods key by the primitive's name.
                names.canon(&segs, &module, &[], here).or_else(|| {
                    match crate::ty::resolve_ty(&impl_def.self_ty) {
                        HirTy::Prim(prim) => Some(prim.name().to_owned()),
                        _ => None,
                    }
                })
            }
            _ => None,
        };
        if impl_generic && generic_base.is_none() {
            let target = impl_def_display(&impl_def.self_ty);
            let is_param = impl_def.generics.params.iter().any(
                |gp| matches!(gp, nabic_ast::GenericParam::Type { name, .. } if *name == target),
            );
            if is_param {
                err(
                    Code::INVALID_IMPL,
                    &mut pre_diags,
                    here,
                    format!(
                        "an inherent `impl` attaches methods to a type, and `{target}` is a \
                         type parameter of this `impl`; methods for every type \
                         belong to a trait: `impl<{target}> Tr for {target} {{ .. }}`"
                    ),
                );
            } else {
                // A path that names no type (other targets are templates above).
                let name = target.split('<').next().unwrap_or(&target);
                let d = nabic_diagnostic::NabiDiagnostic::error(
                    Code::UNRESOLVED_TYPE,
                    here,
                    format!("cannot find type `{name}` in this scope"),
                );
                let help = (!name.contains("::"))
                    .then(|| {
                        names.import_help(&module, names.type_leaf(name), |k| names.type_vis(k))
                    })
                    .flatten();
                pre_diags.push(match help {
                    Some(h) => d.with_help(h),
                    None => d,
                });
            }
            continue;
        }
        pending.push(PendingImpl {
            def: impl_def,
            module: module.clone(),
            trait_name: trait_name.clone(),
            generic_base,
            at: impls.all.len(),
        });
        impls.push(Impl {
            trait_key: trait_name.unwrap_or_default(),
            trait_args: impl_def
                .trait_path
                .as_ref()
                .and_then(|tp| tp.segments.last())
                .map(|s| s.generics.clone())
                .unwrap_or_default(),
            assoc: impl_assoc_list(impl_def),
            generics: impl_def.generics.clone(),
            where_clause: impl_def.where_clause.clone(),
            self_ty: impl_def.self_ty.clone(),
            module,
            methods: HashMap::default(),
            consts,
            concrete: None,
        });
    }

    // Install `pub use` re-exports as fn / static table aliases (types go
    // through `TypeNames`).
    for (alias_key, end) in &aliases {
        if let Some(sig) = fn_table.get(end).cloned() {
            fn_table.entry(alias_key.clone()).or_insert(sig);
        }
        if let Some(def) = generic_fns.get(end).copied() {
            generic_fns.entry(alias_key.clone()).or_insert(def);
        }
        if let Some(ty) = static_tys.get(end).cloned() {
            static_tys.entry(alias_key.clone()).or_insert(ty);
        }
    }

    // Name -> Visibility for every value-namespace item (fns, consts,
    // statics) plus type items; consulted by path resolution.
    let mut item_visibility = item_visibility;
    for item in &program.items {
        match item {
            Item::Fn(f) => {
                item_visibility.entry(f.name.clone()).or_insert(f.vis);
            }
            Item::Const(c) => {
                item_visibility.entry(c.name.clone()).or_insert(c.vis);
            }
            Item::Static(s) => {
                item_visibility.entry(s.name.clone()).or_insert(s.vis);
            }
            _ => {}
        }
    }
    let mut field_vis = field_vis;
    field_vis.extend(collect_field_visibility(program));

    // Function bodies.
    let (bound_methods, bound_traits) = crate::lower::bounds::templates(&trait_info);
    let mut_statics: nabic_core::HashSet<String> = mut_statics
        .into_iter()
        .chain(
            static_defs
                .iter()
                .filter(|s| s.is_mut)
                .map(|s| s.name.clone()),
        )
        .collect();
    let extern_statics: nabic_core::HashSet<String> = extern_statics
        .into_iter()
        .chain(
            static_defs
                .iter()
                .filter(|s| s.is_extern)
                .map(|s| s.name.clone()),
        )
        .collect();
    // The package's handlers ahead of the library's.
    let mut stage_shapes = literal_handlers(program, &mut pre_diags);
    stage_shapes.extend(literal_shapes);
    let literal_shapes = stage_shapes;
    let mut env = EnvIn {
        sources,
        prec,
        t: Tables {
            infer_ret: base_infer_ret,
            implicit_main: input.root_main == crate::RootMain::Implicit,
            main_lets: if input.root_main == crate::RootMain::Implicit {
                main_lets(program)
            } else {
                HashMap::default()
            },
            harness_main: input.root_main == crate::RootMain::Harness,
            may_diverge: base_may_diverge
                .into_iter()
                .chain(may_diverge(program, &mut diverging))
                .collect(),
            duplicated: std::cell::RefCell::default(),
            fn_table,
            fn_leaves: std::cell::OnceCell::new(),
            extern_fns,
            method_table,
            key_of: base_key_of,
            method_info,
            defs,
            consts,
            const_leaves: std::cell::RefCell::default(),
            generic_fns,
            generic_methods,
            intrinsics,
            generic_intrinsics,
            extern_fn_ids: extern_fn_ids.clone(),
            nabi_externs: nabi_externs.clone(),
            c_variadic: c_variadic.clone(),
            c_defs: base_c_defs,
            stdcall_fns: base_stdcall,
            sink,
            item_visibility,
            field_vis,
            impls,
            unsafe_fns: nabic_core::HashSet::default(),
            neg_auto,
            auto_memo: std::cell::RefCell::default(),
            affine_memo: std::cell::RefCell::default(),
            trait_info,
            statics: static_tys,
            static_leaves: std::cell::RefCell::default(),
            mut_statics,
            extern_statics,
            literal_shapes,
            names,
            decls,
            non_exhaustive: non_exhaustive
                .into_iter()
                .chain(program.items.iter().filter_map(|i| match i {
                    Item::Struct(s) if s.attrs.iter().any(|a| a.path == ["non_exhaustive"]) => {
                        Some(s.name.clone())
                    }
                    Item::Enum(e) if e.attrs.iter().any(|a| a.path == ["non_exhaustive"]) => {
                        Some(e.name.clone())
                    }
                    _ => None,
                }))
                .collect(),
            sealed: sealed
                .into_iter()
                .chain(program.items.iter().filter_map(|i| match i {
                    Item::Trait(t) if t.attrs.iter().any(|a| a.path == ["sealed"]) => {
                        Some(t.name.clone())
                    }
                    _ => None,
                }))
                .collect(),
            fn_marks: HashMap::default(),
            marks: marks.into_iter().chain(item_marks(program)).collect(),
            // The std's own uses never warn.
            mark_pkg: match input.lib.as_deref() {
                Some("nabi") => None,
                lib => Some(lib.unwrap_or_default().to_owned()),
            },
            lint_library: input.check_library,
            bound_methods,
            bound_traits,
        },
    };
    // Impl targets and trait arguments, now that every item is
    // registered; then each impl's methods, keyed by them.
    let heads = nabic_diagnostic::time("hir impls", || {
        impl_heads(&mut env, &pending, &trait_defs, &mut pre_diags)
    });
    let mut fn_table = std::mem::take(&mut env.fn_table);
    let mut decls = std::mem::take(&mut env.decls);
    let mut method_table = std::mem::take(&mut env.method_table);
    let mut method_info = std::mem::take(&mut env.method_info);
    let mut intrinsics = std::mem::take(&mut env.intrinsics);
    let mut generic_methods = std::mem::take(&mut env.generic_methods);
    // Trait default templates by name, with the type each is for: two
    // types of one leaf name (`a::Iter`, `b::Iter`) need two names, the
    // name being the mono cache key and the symbol.
    let mut default_names: HashMap<String, String> = generic_methods
        .iter()
        .flat_map(|((base, _), gs)| gs.iter().map(move |g| (g.def.name.clone(), base.clone())))
        .collect();
    let names = &env.names;
    let defs = &env.defs;
    for (p, head) in pending.iter().zip(heads) {
        // `None`: a repeated impl, reported.
        let Some((target_ty, provides)) = head else {
            continue;
        };
        let impl_def = p.def;
        let here = impl_def.span;
        let module = p.module.clone();
        let sc = Scope {
            names,
            module: &module,
            at: here,
        };
        let impl_generic = !impl_def.generics.params.is_empty();
        let trait_name = p.trait_name.clone();
        let generic_base = p.generic_base.clone();
        let target = method_target_for(&target_ty);
        // `From<i32>` / `From<i8>` impls on one type keep their methods
        // apart by the trait's arguments (`trait_args_key`).
        let trait_args_key: String = provides.as_ref().map_or_else(String::new, |r| {
            r.args
                .iter()
                .map(|a| "_".to_owned() + &ty_mangle(a))
                .collect()
        });
        let trait_args: Vec<HirTy> = provides
            .as_ref()
            .map(|r| r.args.clone())
            .unwrap_or_default();
        let trait_def = trait_name
            .as_ref()
            .and_then(|tn| trait_defs.get(tn).copied());
        let target_display = impl_def_display(&impl_def.self_ty);
        let mut dup = |leaf: &str, at: Span, first: Option<Span>| {
            duplicated.extend(first);
            nabic_diagnostic::duplicate(
                Code::DUPLICATE_DEFINITION,
                format!("method `{leaf}` is defined more than once for `{target_display}`"),
                leaf,
                at,
                first,
                true,
            )
        };
        // Methods this impl provides: their trait defaults are skipped.
        let mut explicit_methods: HashMap<String, Span> = HashMap::default();
        for ii in &impl_def.items {
            let orig = match ii {
                AstImplItem::Method(m) => m,
                AstImplItem::Const(_) | AstImplItem::AssocType { .. } => continue,
            };
            let inherited = trait_def.and_then(|td| inherit_throws(orig, td));
            let m = inherited.as_ref().unwrap_or(orig);
            let leaf = leaf_name(&m.name).to_owned();
            if let Some(first) = explicit_methods.get(&leaf) {
                pre_diags.push(dup(&leaf, m.span, Some(*first)));
                continue;
            }
            explicit_methods.insert(leaf.clone(), m.span);
            if let (Some(base), true) =
                (&generic_base, impl_generic || !m.generics.params.is_empty())
            {
                // Impl generics first, then the method's own; `self` and
                // `Self` take the impl target type at instantiation.
                let clash = outer_generic_clash(m, &impl_def.generics);
                let mut def = m.clone();
                shadow_outer_generics(&mut def, &impl_def.generics, clash.is_some());
                pre_diags.extend(clash);
                let base_leaf = leaf_name(base);
                // `Box__get` is both the mono cache key and the name
                // diagnostics print.
                def.name = if module.is_empty() {
                    format!("{base_leaf}__{leaf}")
                } else {
                    format!("{module}::{base_leaf}__{leaf}")
                };
                def.generics.params = [&impl_def.generics.params, &def.generics.params]
                    .into_iter()
                    .flatten()
                    .cloned()
                    .collect();
                def.where_clause = [&impl_def.where_clause, &def.where_clause]
                    .into_iter()
                    .flatten()
                    .cloned()
                    .collect();
                let slot = generic_methods
                    .entry((base.clone(), leaf.clone()))
                    .or_default();
                if let Some(first) = slot
                    .iter()
                    .find(|g| g.info.trait_name.is_none() && trait_name.is_none())
                {
                    pre_diags.push(dup(&leaf, m.span, Some(first.def.span)));
                    continue;
                }
                // `impl Tr for Range<i64>` beside `impl Tr for Range<f64>`:
                // the mono cache key tells the targets apart.
                if !impl_generic
                    && slot
                        .iter()
                        .any(|g| g.def.name == def.name && g.target.as_ref() != Some(&target_ty))
                {
                    def.name = format!("{}__{}", def.name, ty_mangle(&target_ty));
                }
                // A trait impl's generic method (`fn from_iter<I>` of
                // `impl FromIterator<char> for str`): as visible as the trait.
                slot.push(GenericMethod {
                    def: std::sync::Arc::new(def),
                    self_ty: impl_def.self_ty.clone(),
                    target: (!impl_generic).then(|| target_ty.clone()),
                    info: MethodInfo {
                        module: module.clone(),
                        vis: if trait_name.is_some() {
                            Visibility::Pub
                        } else {
                            m.vis
                        },
                        trait_name: trait_name.clone(),
                        trait_args: trait_args.clone(),
                    },
                });
                continue;
            }
            if let Some(td) = trait_def {
                let tm = td.items.iter().find_map(|ti| match ti {
                    nabic_ast::TraitItem::Method(tm) if tm.name == leaf => Some(tm),
                    _ => None,
                });
                match tm {
                    None => err(
                        Code::NOT_A_TRAIT_MEMBER,
                        &mut pre_diags,
                        here,
                        format!("method `{leaf}` is not a member of trait `{}`", td.name),
                    ),
                    // A throwing fn can't fill a non-throwing
                    // slot. The operator traits never throw.
                    Some(tm) if is_throwing(&m.throws) && !is_throwing(&tm.throws) => {
                        err(
                            Code::IMPL_MISMATCH,
                            &mut pre_diags,
                            here,
                            format!(
                                "`{leaf}` throws but `{}::{leaf}` is declared without \
                                 `throws`, so an impl can't add one{}",
                                td.name,
                                if td.attrs.iter().any(|a| a.path == ["lang"]) {
                                    " - operators don't throw; return a `Result` or use \
                                     a named method"
                                } else {
                                    ""
                                }
                            ),
                        );
                    }
                    // Mismatched with the trait: reported here and left out,
                    // so trait-method callers report nothing more.
                    Some(tm) if sig_mismatch(m, tm, &td.name, &mut pre_diags) => continue,
                    Some(_) => {}
                }
            }
            let Some(key) = method_key(
                &fn_table,
                &method_info,
                &target_ty,
                &leaf,
                trait_name.as_deref(),
                &trait_args_key,
            ) else {
                let first = fn_table
                    .get(&format!("{}__{leaf}", ty_mangle(&target_ty)))
                    .and_then(|sig| decls.get(sig.id.0 as usize))
                    .map(decl_span);
                pre_diags.push(dup(&leaf, m.span, first));
                continue;
            };
            let id = FunctionId(u32::try_from(decls.len()).expect("FunctionId overflow"));
            let sig = sig_of(m, id, sc);
            fn_table.insert(key.clone(), std::sync::Arc::new(sig));
            if let Some(i) = intrinsic_of(m, &module, &mut pre_diags) {
                intrinsics.insert(id, i);
            }
            if let Some(t) = target.clone() {
                method_table.entry((t, leaf.clone())).or_default().push(id);
            }
            // Trait-impl methods are as visible as the trait itself;
            // inherent methods carry their own visibility.
            let vis = if trait_name.is_some() {
                Visibility::Pub
            } else {
                m.vis
            };
            method_info.insert(
                id,
                MethodInfo {
                    module: module.clone(),
                    vis,
                    trait_name: trait_name.clone(),
                    trait_args: trait_args.clone(),
                },
            );
            // A library's method symbol must not depend on this build's
            // struct numbering, so prebuilt and cached code and their
            // users agree.
            if !package_of(&module).is_empty() {
                let mut name = format!("{module}::{}.{leaf}", impl_def_display(&impl_def.self_ty));
                if let Some(tp) = &impl_def.trait_path {
                    name.push_str("__by_");
                    name.push_str(&impl_def_display(&Ty {
                        kind: AstTyKind::Path(tp.clone()),
                        span: tp.span,
                    }));
                }
                link_names.insert(decls.len(), name);
            }
            decls.push(MethodOrFn::Method {
                fn_def: match &inherited {
                    Some(d) => std::borrow::Cow::Owned(d.clone()),
                    None => std::borrow::Cow::Borrowed(orig),
                },
                self_ty: target_ty.clone(),
                module: module.clone(),
                key,
                subst: HashMap::default(),
                default: false,
            });
            lib_decl.push(!package_of(&module).is_empty());
        }

        // Trait defaults the impl doesn't override, lowered as methods
        // with `Self = target_ty`. A missing required method is an error.
        if let Some(trait_def) = trait_def {
            let trait_module = module_of(&trait_def.name);
            for ti in &trait_def.items {
                let nabic_ast::TraitItem::Method(tm) = ti else {
                    continue;
                };
                if explicit_methods.insert(tm.name.clone(), tm.span).is_some() {
                    continue;
                }
                if tm.body.is_none() {
                    err(
                        Code::MISSING_TRAIT_ITEM,
                        &mut pre_diags,
                        here,
                        format!(
                            "impl of trait `{}` for `{target_display}` is missing method `{}`",
                            trait_def.name, tm.name
                        ),
                    );
                    continue;
                }
                let Some(key) = method_key(
                    &fn_table,
                    &method_info,
                    &target_ty,
                    &tm.name,
                    trait_name.as_deref(),
                    &trait_args_key,
                ) else {
                    err(
                        Code::DUPLICATE_DEFINITION,
                        &mut pre_diags,
                        here,
                        format!(
                            "method `{}` is defined more than once for `{target_display}`",
                            tm.name
                        ),
                    );
                    continue;
                };
                // A default with its own type parameters or naming the
                // trait's associated types becomes a template, instantiated
                // per call with this impl's types.
                if !tm.generics.params.is_empty()
                    || sig_mentions(tm, &trait_assoc_names(trait_def))
                    || returns_self_generic(tm)
                {
                    let base = match &target_ty {
                        HirTy::Struct(id) => {
                            Some(defs.structs.borrow()[id.0 as usize].name.clone())
                        }
                        HirTy::Enum(id) => Some(defs.enums.borrow()[id.0 as usize].name.clone()),
                        _ => None,
                    };
                    if let Some(base) = base {
                        let mut def = tm.clone();
                        subst_assoc_sig(&mut def, &impl_assoc_types_from(impl_def, &module));
                        def.name = format!("{trait_module}::{}__{}", leaf_name(&base), tm.name);
                        if default_names.get(&def.name).is_some_and(|b| *b != base) {
                            def.name =
                                format!("{trait_module}::{}__{}", base.replace("::", "_"), tm.name);
                        }
                        default_names.insert(def.name.clone(), base.clone());
                        let slot = generic_methods.entry((base, tm.name.clone())).or_default();
                        if !slot.is_empty() {
                            // Another trait's same-named default: its own
                            // mono cache key / symbol.
                            def.name = format!(
                                "{}__by_{}",
                                def.name,
                                leaf_name(trait_name.as_deref().unwrap_or_default())
                            );
                        }
                        slot.push(GenericMethod {
                            def: std::sync::Arc::new(def),
                            self_ty: impl_def.self_ty.clone(),
                            target: (!impl_generic).then(|| target_ty.clone()),
                            info: MethodInfo {
                                module: trait_module.clone(),
                                vis: Visibility::Pub,
                                trait_name: trait_name.clone(),
                                trait_args: Vec::new(),
                            },
                        });
                    }
                    // A primitive / tuple target gets no such defaults.
                    continue;
                }
                // `trait Contains<Item>` + `impl Contains<i32> for S`: the
                // default's signature sees `Item = i32`.
                let mut targs = trait_param_args(trait_def, impl_def);
                // Const arguments (`3`, `W`, `{ W + 1 }`) become literals.
                for gp in &trait_def.generics.params {
                    if let nabic_ast::GenericParam::Const { name, .. } = gp {
                        if let Some(t) = targs.get_mut(name) {
                            if let Some(v) = const_arg_value(t, names, sc.module) {
                                *t = const_literal_ty(v, t.span);
                            }
                        }
                    }
                }
                let body_subst: HashMap<String, HirTy> = trait_def
                    .generics
                    .params
                    .iter()
                    .enumerate()
                    .filter_map(|(i, gp)| match gp {
                        nabic_ast::GenericParam::Type { name, .. } => {
                            Some((name.clone(), provides.as_ref()?.args.get(i)?.clone()))
                        }
                        nabic_ast::GenericParam::Const { name, ty } => {
                            let v = match &targs.get(name)?.kind {
                                AstTyKind::Const(c) => {
                                    crate::lower::consteval::syntactic_int(c, &|_| None)?
                                }
                                _ => return None,
                            };
                            let prim = match resolve_ty(ty) {
                                HirTy::Prim(p) => Some(p),
                                _ => None,
                            };
                            Some((name.clone(), HirTy::Const(v, prim)))
                        }
                    })
                    .collect();
                let tm: std::borrow::Cow<'_, FnDef> = if targs.is_empty() {
                    std::borrow::Cow::Borrowed(tm)
                } else {
                    let mut d = tm.clone();
                    subst_assoc_sig(&mut d, &targs);
                    std::borrow::Cow::Owned(d)
                };
                let id = FunctionId(u32::try_from(decls.len()).expect("FunctionId overflow"));
                let tsc = Scope {
                    names,
                    module: &trait_module,
                    at: here,
                };
                let sig = sig_of(&tm, id, tsc);
                fn_table.insert(key.clone(), std::sync::Arc::new(sig));
                if let Some(t) = target.clone() {
                    method_table
                        .entry((t, tm.name.clone()))
                        .or_default()
                        .push(id);
                }
                method_info.insert(
                    id,
                    MethodInfo {
                        module: trait_module.clone(),
                        vis: Visibility::Pub,
                        trait_name: trait_name.clone(),
                        trait_args: trait_args.clone(),
                    },
                );
                decls.push(MethodOrFn::Method {
                    fn_def: tm,
                    self_ty: target_ty.clone(),
                    module: trait_module.clone(),
                    key,
                    subst: body_subst,
                    default: true,
                });
                // A library default in a user impl is checked with it.
                lib_decl.push(!package_of(&module).is_empty());
            }
        }
    }
    for (i, d) in decls.iter().enumerate().skip(first_decl) {
        let f = d.def();
        let here = f.span;
        let id = FunctionId(u32::try_from(i).expect("FunctionId overflow"));
        if f.is_unsafe {
            unsafe_fns.insert(id);
        }
        if let Some(abi) = nabic_ast::extern_fn_abi(&f.attrs) {
            check_extern_fn(f, abi, &mut pre_diags);
            if abi == "system" {
                system_fns.insert(id);
            }
            c_defs.push(id);
        } else if f.attrs.iter().any(|a| a.path == ["on_panic"]) {
            err(
                Code::MISPLACED_ATTRIBUTE,
                &mut pre_diags,
                here,
                format!(
                    "`#[on_panic]` is for functions C calls: write `#[on_panic(return = -1)] \
                     extern \"C\" fn {}(..)`",
                    leaf_name(&f.name)
                ),
            );
        }
        if !c_variadic.contains(&id)
            && f.attrs
                .iter()
                .any(|a| a.path == [nabic_ast::C_VARIADIC_ATTR])
        {
            err(
                Code::INVALID_SIGNATURE,
                &mut pre_diags,
                here,
                format!(
                    "`{}`: a C variadic `...` parameter is only allowed on a declaration in an \
                     `extern \"C\" {{ .. }}` block",
                    f.name
                ),
            );
        }
        let module = match d {
            MethodOrFn::Fn(f) => fn_module(&env, f),
            MethodOrFn::Method { module, .. } => module.clone(),
        };
        // A trait method's marks reach every impl of it.
        let declared = method_info
            .get(&id)
            .and_then(|m| m.trait_name.as_ref())
            .and_then(|t| env.trait_info.get(t))
            .and_then(|t| t.decls.get(leaf_name(&f.name)));
        let mark = mark_of(&f.attrs).or_else(|| declared.and_then(|t| mark_of(&t.attrs)));
        // `#[internal]` outside the toolchain is reported where it is
        // written (`check_internal_marks`), not at every use.
        let mark = mark.and_then(|mut m| {
            if !is_toolchain_module(&module) {
                m.internal = None;
            }
            (m != crate::marks::Mark::default()).then_some(m)
        });
        if let Some(mark) = mark {
            if mark.deprecated_safe.is_some()
                && package_of(&module).is_empty()
                && input.lib.is_none()
            {
                err(
                    Code::MISPLACED_ATTRIBUTE,
                    &mut pre_diags,
                    here,
                    format!(
                        "`#[deprecated_safe]` on `{}`: it marks a library function that a later \
                         release makes `unsafe`; in a program, write `unsafe fn` \
                         instead",
                        leaf_name(&f.name)
                    ),
                );
            }
            fn_marks.insert(id, (mark_name(&env, d), mark));
        }
        // A prebuilt package's `.a` initializes its lazy statics.
        let linked = f.body.is_none() || is_lazy_init_fn(&f.name);
        let prebuilt_fn = linked
            && !(extern_fn_ids.contains(&id) || nabi_externs.contains(&id))
            && input
                .prebuilt
                .iter()
                .any(|p| within(package_of(&module), p));
        if prebuilt_fn {
            prebuilt.insert(id);
        }
        if let Some(sym) = export_name(f) {
            let c_abi = nabic_ast::extern_fn_abi(&f.attrs).is_some();
            if !c_abi && !sym.starts_with("__nabi_") && !prebuilt_fn {
                let name = leaf_name(&f.name);
                err(
                    Code::FFI,
                    &mut pre_diags,
                    here,
                    format!(
                        "`{name}` is exported to C but uses the Nabi ABI: write `pub extern \"C\" \
                         fn {name}(..)`"
                    ),
                );
            } else if f.body.is_none() && !prebuilt_fn {
                err(
                    Code::MISPLACED_ATTRIBUTE,
                    &mut pre_diags,
                    here,
                    format!("`#[export_name]` on `{}` needs a function body", f.name),
                );
            } else if exports.iter().any(|(_, s)| *s == sym) {
                err(
                    Code::DUPLICATE_REGISTRATION,
                    &mut pre_diags,
                    here,
                    format!("symbol `{sym}` is exported twice (`#[export_name]`)"),
                );
            } else {
                exports.push((id, sym));
            }
        }
    }
    let stage_generics = program.items.iter().filter_map(|i| match i {
        Item::Fn(f) if !f.generics.params.is_empty() => Some(f),
        _ => None,
    });
    for f in stage_generics {
        if let Some(abi) = nabic_ast::extern_fn_abi(&f.attrs) {
            check_extern_fn(f, abi, &mut pre_diags);
        }
        if export_name(f).is_some() {
            err(
                Code::MISPLACED_ATTRIBUTE,
                &mut pre_diags,
                f.span,
                format!(
                    "`#[export_name]` on generic function `{}`: only a concrete function \
                     has one symbol",
                    f.name
                ),
            );
        }
    }
    let mut key_of: HashMap<FunctionId, String> = HashMap::default();
    for (k, s) in &fn_table {
        // Re-export aliases share the id; keep the shortest (original) key.
        let e = key_of.entry(s.id).or_insert_with(|| k.clone());
        if k.len() < e.len() {
            e.clone_from(k);
        }
    }
    let mut infer_ret = std::mem::take(&mut env.infer_ret);
    infer_ret.extend(
        decls
            .iter()
            .enumerate()
            .skip(first_decl)
            .filter(|(_, d)| mono::infers_ret(d.def(), |s| env.may_diverge.contains(&s)))
            .map(|(i, _)| FunctionId(u32::try_from(i).expect("FunctionId overflow"))),
    );
    env.fn_table = fn_table;
    env.decls = decls;
    env.duplicated = duplicated.into();
    env.method_table = method_table;
    env.method_info = method_info;
    env.intrinsics = intrinsics;
    env.generic_methods = generic_methods;
    env.key_of = key_of;
    env.infer_ret = infer_ret;
    env.unsafe_fns = unsafe_fns;
    env.fn_marks = fn_marks;
    env.c_defs = c_defs.iter().copied().collect();
    if input.stdcall_system {
        env.stdcall_fns.clone_from(&system_fns);
    }
    env.sink
        .next_id
        .set(u32::try_from(env.decls.len()).expect("FunctionId overflow"));
    // Fields and payloads naming projections waited for the impls; resolve
    // them now.
    resolve_projection_aliases(&env, program);
    // Every signature, now that all items and impls exist. Array lengths
    // calling `const fn`s wait for a second round.
    let mut waiting: Vec<FunctionId> = Vec::new();
    let mut sigs: HashMap<FunctionId, FnSig> = (first_decl..env.decls.len())
        .map(|i| {
            let id = FunctionId(u32::try_from(i).expect("FunctionId overflow"));
            let base = &env.fn_table[&env.key_of[&id]];
            env.sink.sig_pass.set(Some(false));
            let sig = mono::decl_sig(&env, i, base);
            if env.sink.sig_pass.get() == Some(true) {
                waiting.push(id);
            }
            (id, sig)
        })
        .collect();
    env.sink.sig_pass.set(None);
    install_sigs(&mut env.fn_table, &sigs);
    for id in waiting {
        let base = &env.fn_table[&env.key_of[&id]];
        let sig = mono::decl_sig(&env, id.0 as usize, base);
        sigs.insert(id, sig);
    }
    install_sigs(&mut env.fn_table, &sigs);
    if !env.names.some_types.is_empty() {
        // Named `some` types: their defining bodies give them, and the
        // signatures that wrote one take it.
        mono::solve_some_types(&env);
        let open: HashMap<FunctionId, FnSig> = (first_decl..env.decls.len())
            .filter_map(|i| {
                let id = FunctionId(u32::try_from(i).expect("FunctionId overflow"));
                let base = &env.fn_table[&env.key_of[&id]];
                let mut paths = Vec::new();
                for t in base.param_tys.iter().chain([&base.ret_ty]) {
                    collect_unknown_paths(t, &mut paths);
                }
                // Inside a type argument (`List<X>`) the open path is the
                // generic type's: the declaration names the `some` type.
                let named = || {
                    !paths.is_empty()
                        && env
                            .decl(id)
                            .is_some_and(|d| !mono::sig_some_types(&env, d.def()).is_empty())
                };
                (paths.iter().any(|p| env.names.some_types.contains_key(p)) || named())
                    .then(|| (id, mono::decl_sig(&env, i, base)))
            })
            .collect();
        install_sigs(&mut env.fn_table, &open);
        sigs.extend(open);
        env.sink.opaque_called.set(false);
    }
    resolve_open_members(&env, &struct_decls, &enum_decls);
    env.sink.opaque_called.set(false);
    // One extern namespace; a second declaration with another
    // signature is an error, reported against the runtime's declaration
    // (`core::rt`) when one is involved.
    let no_subst = HashMap::default();
    for (f, nabi_abi, first) in &extern_redecls {
        let (params, ret) = mono::sig_tys(&env, f, None, fn_module(&env, f), &no_subst);
        // The first declaration may be an earlier stage's.
        let s = sigs
            .get(first)
            .unwrap_or_else(|| &env.fn_table[&env.key_of[first]]);
        let c_var = |f: &FnDef| {
            f.attrs
                .iter()
                .any(|a| a.path == [nabic_ast::C_VARIADIC_ATTR])
        };
        let MethodOrFn::Fn(first_def) = env.decls[first.0 as usize] else {
            continue;
        };
        if params == s.param_tys
            && ret == s.ret_ty
            && c_var(f) == c_var(first_def)
            && f.params.last().is_some_and(|p| p.variadic) == s.variadic
        {
            continue;
        }
        let cx = LowerCtx::new(&env, &no_subst, None, f.span);
        let show = |ps: &[HirTy], r: &HirTy| {
            let ps: Vec<String> = ps.iter().map(|t| cx.ty_name(t)).collect();
            format!("fn({}) -> {}", ps.join(", "), cx.ty_name(r))
        };
        let (here, other, at) = if *nabi_abi && !nabi_externs.contains(first) {
            (
                show(&s.param_tys, &s.ret_ty),
                show(&params, &ret),
                first_def.span,
            )
        } else {
            (show(&params, &ret), show(&s.param_tys, &s.ret_ty), f.span)
        };
        let elsewhere = if at == f.span { first_def.span } else { f.span };
        // Calls are checked against neither declaration.
        env.duplicated.borrow_mut().insert(first_def.span);
        pre_diags.push(
            nabic_diagnostic::NabiDiagnostic::error(
                Code::FFI,
                at,
                format!(
                    "extern function `{}` is declared twice with different signatures: `{here}` \
                     here, `{other}` elsewhere{}",
                    f.name,
                    if f.name.starts_with("nabi_") {
                        "; a `nabi_*` symbol is the runtime's: declare it with the runtime's signature"
                    } else {
                        ""
                    }
                ),
            )
            .with_secondary(elsewhere, format!("declared as `{other}` here"))
            .with_help(format!(
                "a C symbol has one signature: declare `{}` as `{other}` here too, or drop \
                 this declaration",
                f.name
            )),
        );
    }
    let mut ref_out: Vec<(FunctionId, RefOut)> = sigs
        .values()
        .filter_map(|s| Some((s.id, ref_out_of(&env, s)?)))
        .collect();
    ref_out.sort_by_key(|(id, _)| *id);
    env.sink.ref_out.borrow_mut().extend(ref_out);
    // Every static's type, now that every item is registered.
    let no_subst = HashMap::default();
    let static_types: Vec<HirTy> = static_defs[first_static..]
        .iter()
        .zip(&static_asts[first_static..])
        .map(|(d, (ast, module))| {
            let mut rcx = LowerCtx::new(&env, &no_subst, None, d.span);
            rcx.current_module.clone_from(module);
            rcx.resolve_ast_ty(ast)
        })
        .collect();
    for (d, ty) in static_defs[first_static..].iter_mut().zip(static_types) {
        env.statics.insert(d.name.clone(), ty.clone());
        d.ty = ty;
    }
    assign_discriminants(&env, &enum_decls[first_new_enum..]);
    let collected = Collected {
        t: env.t,
        early,
        pre_diags,
        reexports,
        enum_decls,
        struct_decls,
        inherent_keys,
        static_defs,
        static_asts,
        lib_decl,
        link_names,
        templated,
        system_fns,
        no_safepoint,
        ffi,
        wasm_imports,
        extern_symbols,
        trait_defs,
        exports,
        prebuilt,
        c_defs,
        diverging,
    };
    Ok((collected, program))
}
