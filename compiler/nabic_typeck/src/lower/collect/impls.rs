//! Impl signatures, heads, associated items and consts.

#[allow(clippy::wildcard_imports)]
use crate::lower::collect::*;

/// What a trait method's signature type depends on besides concrete types:
/// `Self`, an associated type (pushed as `Self::X`) or a trait parameter.
pub(crate) fn ast_ty_needs(
    ty: &nabic_ast::Ty,
    params: &[&str],
    assoc: &[&str],
    out: &mut Vec<String>,
) {
    let kids: Vec<&nabic_ast::Ty> = match &ty.kind {
        AstTyKind::SelfUpper => {
            out.push("Self".to_owned());
            Vec::new()
        }
        AstTyKind::Path(p) => {
            match p.segments.as_slice() {
                [s, x, ..] if s.name == "Self" => out.push(format!("Self::{}", x.name)),
                [s] | [s, _] if assoc.contains(&s.name.as_str()) => {
                    out.push(format!("Self::{}", s.name));
                }
                [s, ..] if s.name == "Self" || params.contains(&s.name.as_str()) => {
                    out.push(s.name.clone());
                }
                _ => {}
            }
            p.segments.iter().flat_map(|s| &s.generics).collect()
        }
        AstTyKind::QPath(q) => std::iter::once(&q.self_ty)
            .chain(q.trait_path.segments.iter().flat_map(|s| &s.generics))
            .collect(),
        AstTyKind::AnyTrait(bounds) => bounds
            .iter()
            .flat_map(|b| b.segments.iter().flat_map(|s| &s.generics))
            .collect(),
        AstTyKind::Sum(parts) | AstTyKind::Tuple(parts) | AstTyKind::Some(parts) => {
            parts.iter().collect()
        }
        AstTyKind::Fn(args, ret, _) => args.iter().chain(std::iter::once(&**ret)).collect(),
        AstTyKind::Throws { ok, err } => vec![&**ok, &**err],
        AstTyKind::Array(elem, _) | AstTyKind::Nest(elem) | AstTyKind::Binding(_, elem) => {
            vec![&**elem]
        }
        AstTyKind::Any | AstTyKind::None | AstTyKind::Never | AstTyKind::Const(_) => Vec::new(),
    };
    for k in kids {
        ast_ty_needs(k, params, assoc, out);
    }
}

/// Report impl method `m` when its parameter count, `ref` modes or return
/// differ from the trait's `tm`; the declaration is the fix.
pub(crate) fn sig_mismatch(
    m: &FnDef,
    tm: &FnDef,
    trait_name: &str,
    diags: &mut Vec<nabic_diagnostic::NabiDiagnostic>,
) -> bool {
    let mut why = Vec::new();
    if m.params.len() == tm.params.len() {
        // The receiver may differ: `self` fills a `ref self` slot.
        for (p, tp) in m
            .params
            .iter()
            .zip(&tm.params)
            .filter(|(p, _)| p.name != "self")
        {
            match (p.by_ref, tp.by_ref) {
                (false, true) => why.push(format!("`{}` must be `ref {}`", p.name, p.name)),
                (true, false) => why.push(format!("`{}` must not be `ref`", p.name)),
                _ => {}
            }
        }
    } else {
        why.push(format!(
            "it takes {} parameters where the trait's takes {}",
            m.params.len(),
            tm.params.len()
        ));
    }
    match (m.is_unsafe, tm.is_unsafe) {
        (true, false) => why.push("it is `unsafe` where the trait's is not".to_owned()),
        (false, true) => why.push("the trait's is `unsafe fn` and it is not".to_owned()),
        _ => {}
    }
    let ret = |f: &FnDef| {
        f.ret_ty
            .as_ref()
            .filter(|t| !matches!(&t.kind, AstTyKind::Tuple(v) if v.is_empty()))
            .map(nabic_ast::print::print_ty)
    };
    match (ret(m), ret(tm)) {
        (Some(r), None) => why.push(format!(
            "it returns `{r}` where the trait's returns nothing"
        )),
        (None, Some(r)) => why.push(format!(
            "it returns nothing where the trait's returns `{r}`"
        )),
        _ => {}
    }
    if why.is_empty() {
        return false;
    }
    let name = leaf_name(&m.name);
    diags.push(
        nabic_diagnostic::NabiDiagnostic::error(
            Code::IMPL_MISMATCH,
            m.span,
            format!(
                "`{name}` does not match its declaration in trait `{}`: {}",
                leaf_name(trait_name),
                why.join(", and ")
            ),
        )
        .with_help(format!("write `{}`", nabic_ast::print::print_fn_sig(tm))),
    );
    true
}

/// A method written in the impl may not reuse a name of the impl's
/// generic parameters.
pub(crate) fn outer_generic_clash(
    m: &FnDef,
    outer: &nabic_ast::Generics,
) -> Option<nabic_diagnostic::NabiDiagnostic> {
    let name = |p: &nabic_ast::GenericParam| match p {
        nabic_ast::GenericParam::Type { name, .. }
        | nabic_ast::GenericParam::Const { name, .. } => name.clone(),
    };
    let taken: Vec<String> = outer.params.iter().map(name).collect();
    let n = m
        .generics
        .params
        .iter()
        .map(name)
        .find(|n| taken.contains(n))?;
    let leaf = leaf_name(&m.name);
    Some(
        nabic_diagnostic::NabiDiagnostic::error(
            Code::GENERIC_PARAM_SHADOWS,
            m.span,
            format!("generic parameter `{n}` of `{leaf}` is already a parameter of its `impl`"),
        )
        .with_help(format!(
            "rename the method's (`fn {leaf}<{n}2>`), or drop it to use the impl's `{n}`"
        )),
    )
}

/// A trait default method's generic parameter named like one of its
/// impl's shadows it: the method's is renamed, with its uses, to a name no
/// source writes (`K'`) before the two lists are joined. After the
/// `clashed` error of a method written in the impl, its parameter is
/// dropped instead, so its uses mean the impl's and say nothing more.
pub(crate) fn shadow_outer_generics(def: &mut FnDef, outer: &nabic_ast::Generics, clashed: bool) {
    let name = |p: &nabic_ast::GenericParam| match p {
        nabic_ast::GenericParam::Type { name, .. }
        | nabic_ast::GenericParam::Const { name, .. } => name.clone(),
    };
    let taken: nabic_core::HashSet<String> = outer.params.iter().map(name).collect();
    if clashed {
        def.generics.params.retain(|p| !taken.contains(&name(p)));
        return;
    }
    let mut r = nabic_ast::rename::Renames::default();
    for p in &def.generics.params {
        let n = name(p);
        if !taken.contains(&n) {
            continue;
        }
        let to = format!("{n}'");
        match p {
            nabic_ast::GenericParam::Type { .. } => r.types.insert(n, to),
            nabic_ast::GenericParam::Const { .. } => r.values.insert(n, to),
        };
    }
    if r.is_empty() {
        return;
    }
    let mut item = Item::Fn(def.clone());
    r.item(&mut item);
    if let Item::Fn(f) = item {
        *def = f;
    }
}

/// Impl method `m` without `throws` takes its trait method's, the error
/// types resolving in the trait's module; `None` when nothing changes.
pub(crate) fn inherit_throws(m: &FnDef, td: &nabic_ast::TraitDef) -> Option<FnDef> {
    if !matches!(m.throws, nabic_ast::ThrowsKind::None) {
        return None;
    }
    let leaf = leaf_name(&m.name);
    let tm = td.items.iter().find_map(|ti| match ti {
        nabic_ast::TraitItem::Method(tm) if tm.name == leaf => Some(tm),
        _ => None,
    })?;
    if !is_throwing(&tm.throws) {
        return None;
    }
    let mut d = m.clone();
    d.throws = tm.throws.clone();
    if let nabic_ast::ThrowsKind::Typed(ts) = &mut d.throws {
        let module = resolve::module_of(&td.name);
        ts.iter_mut().for_each(|t| set_origin(t, &module));
    }
    Some(d)
}

/// Resolve the paths of `t` in `module`, where they were written.
pub(crate) fn set_origin(t: &mut Ty, module: &str) {
    match &mut t.kind {
        AstTyKind::Path(p) => {
            p.origin.get_or_insert_with(|| module.to_owned());
            for g in p.segments.iter_mut().flat_map(|s| &mut s.generics) {
                set_origin(g, module);
            }
        }
        AstTyKind::AnyTrait(bounds) => {
            for p in bounds {
                p.origin.get_or_insert_with(|| module.to_owned());
            }
        }
        AstTyKind::Sum(parts) | AstTyKind::Tuple(parts) => {
            parts.iter_mut().for_each(|k| set_origin(k, module));
        }
        _ => {}
    }
}

pub(crate) fn is_throwing(t: &nabic_ast::ThrowsKind) -> bool {
    match t {
        nabic_ast::ThrowsKind::None => false,
        nabic_ast::ThrowsKind::Untyped => true,
        nabic_ast::ThrowsKind::Typed(v) => !v.is_empty(),
    }
}

/// `impl<..> Tr for Target` as method templates (trait defaults included),
/// monomorphized per receiver.
pub(crate) fn generic_trait_impl(
    impl_def: &nabic_ast::ImplDef,
    trait_key: String,
    trait_def: Option<&nabic_ast::TraitDef>,
    module: &str,
    tag: &str,
    diags: &mut Vec<nabic_diagnostic::NabiDiagnostic>,
    duplicated: &mut nabic_core::HashSet<Span>,
) -> Impl {
    let err = |code: Code, pre: &mut Vec<nabic_diagnostic::NabiDiagnostic>, msg: String| {
        pre.push(nabic_diagnostic::NabiDiagnostic::error(
            code,
            impl_def.span,
            msg,
        ));
    };
    let target_display = impl_def_display(&impl_def.self_ty);
    let assoc = impl_assoc_types_from(impl_def, module);
    let template = |m: &FnDef, def_module: &str, clashed: bool| {
        let mut def = m.clone();
        // Before the impl's associated types bring its names in.
        shadow_outer_generics(&mut def, &impl_def.generics, clashed);
        subst_assoc_sig(&mut def, &assoc);
        let leaf = leaf_name(&m.name);
        // `__impl1f2e3d4c_show` is both the mono cache key and the name
        // diagnostics print.
        def.name = if def_module.is_empty() {
            format!("__impl{tag}_{leaf}")
        } else {
            format!("{def_module}::__impl{tag}_{leaf}")
        };
        // The method's own (assoc-substituted) generics after the impl's.
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
        GenericMethod {
            def: std::sync::Arc::new(def),
            self_ty: impl_def.self_ty.clone(),
            target: None,
            info: MethodInfo {
                module: def_module.to_owned(),
                vis: Visibility::Pub,
                trait_name: (!trait_key.is_empty()).then(|| trait_key.clone()),
                trait_args: Vec::new(),
            },
        }
    };
    let mut methods: HashMap<String, GenericMethod> = HashMap::default();
    // Methods unlike the trait's: reported, not missing.
    let mut mismatched = nabic_core::HashSet::default();
    for ii in &impl_def.items {
        let AstImplItem::Method(m) = ii else {
            continue;
        };
        let leaf = leaf_name(&m.name).to_owned();
        if let Some(td) = trait_def {
            let tm = td.items.iter().find_map(|ti| match ti {
                nabic_ast::TraitItem::Method(tm) if tm.name == leaf => Some(tm),
                _ => None,
            });
            match tm {
                None => err(
                    Code::NOT_A_TRAIT_MEMBER,
                    diags,
                    format!("method `{leaf}` is not a member of trait `{}`", td.name),
                ),
                Some(tm) if sig_mismatch(m, tm, &td.name, diags) => {
                    mismatched.insert(leaf);
                    continue;
                }
                Some(_) => {}
            }
        }
        if let Some(first) = methods.get(&leaf) {
            duplicated.insert(first.def.span);
            diags.push(nabic_diagnostic::duplicate(
                Code::DUPLICATE_DEFINITION,
                format!("method `{leaf}` is defined more than once for `{target_display}`"),
                &leaf,
                m.span,
                Some(first.def.span),
                true,
            ));
            continue;
        }
        let clash = outer_generic_clash(m, &impl_def.generics);
        let inherited = trait_def.and_then(|td| inherit_throws(m, td));
        let def = template(inherited.as_ref().unwrap_or(m), module, clash.is_some());
        diags.extend(clash);
        methods.insert(leaf.clone(), def);
    }
    if let Some(td) = trait_def {
        let trait_module = module_of(&td.name);
        for ti in &td.items {
            let nabic_ast::TraitItem::Method(tm) = ti else {
                continue;
            };
            if methods.contains_key(&tm.name) || mismatched.contains(&tm.name) {
                continue;
            }
            if tm.body.is_none() {
                err(
                    Code::MISSING_TRAIT_ITEM,
                    diags,
                    format!(
                        "impl of trait `{}` for `{target_display}` is missing method `{}`",
                        td.name, tm.name
                    ),
                );
                continue;
            }
            // `trait Contains<Item>` + `impl<T> Contains<T> for L<T>`: the
            // default's signature sees `Item = T`.
            let mut tm = tm.clone();
            let mut args = trait_param_args(td, impl_def);
            supertrait_projections(&tm, td, impl_def, &mut args);
            subst_assoc_sig(&mut tm, &args);
            methods.insert(tm.name.clone(), template(&tm, &trait_module, false));
        }
    }
    Impl {
        trait_key,
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
        module: module.to_owned(),
        methods,
        consts: Vec::new(),
        concrete: None,
    }
}

/// An impl that is not a generic trait impl, registered by its written
/// target (`at`: its `Impl`).
pub(crate) struct PendingImpl<'p> {
    pub(crate) def: &'p nabic_ast::ImplDef,
    pub(crate) module: String,
    pub(crate) trait_name: Option<String>,
    /// The target's generic base / primitive name (method templates).
    pub(crate) generic_base: Option<String>,
    pub(crate) at: usize,
}

/// Each pending impl's target and trait ref, once all items exist (a
/// generic inherent impl's target is its base). Projection targets resolve
/// through earlier impls, so rounds repeat while progress is made; the last
/// reports what's left. `None`: a duplicate trait impl for the target.
pub(crate) fn impl_heads(
    env: &mut EnvIn<'_, '_>,
    pending: &[PendingImpl<'_>],
    trait_defs: &HashMap<String, &nabic_ast::TraitDef>,
    diags: &mut Vec<nabic_diagnostic::NabiDiagnostic>,
) -> Vec<Option<(HirTy, Option<TraitRef>)>> {
    let mut heads = vec![None; pending.len()];
    let mut left: Vec<usize> = Vec::new();
    for (i, p) in pending.iter().enumerate() {
        if p.def.generics.params.is_empty() {
            left.push(i);
        } else {
            let base = p.generic_base.clone().unwrap_or_default();
            heads[i] = Some((HirTy::Path(vec![base]), None));
        }
    }
    let open = |t: &HirTy| {
        let mut paths = Vec::new();
        collect_unknown_paths(t, &mut paths);
        *t == HirTy::Unknown || !paths.is_empty()
    };
    let mut last = false;
    let no_subst = HashMap::default();
    while !left.is_empty() {
        // One round's heads, against the impls resolved before it; one
        // still open waits (its diagnostics dropped) unless it is the last.
        let round: Vec<Option<(HirTy, Option<TraitRef>)>> = {
            let mut rcx = LowerCtx::new(env, &no_subst, None, Span::DUMMY);
            left.iter()
                .map(|&i| {
                    let p = &pending[i];
                    rcx.at.set(p.def.span);
                    let mark = env.sink.diagnostics.borrow().len();
                    let td = p
                        .trait_name
                        .as_ref()
                        .and_then(|t| trait_defs.get(t).copied());
                    let (ty, provides) = impl_head(&mut rcx, p, td);
                    let unresolved = open(&ty)
                        || provides.as_ref().is_some_and(|r| {
                            r.args
                                .iter()
                                .chain(r.binds.iter().map(|(_, t)| t))
                                .any(open)
                        });
                    if unresolved && !last {
                        env.sink.diagnostics.borrow_mut().truncate(mark);
                        return None;
                    }
                    Some((ty, provides))
                })
                .collect()
        };
        let mut waiting = Vec::new();
        for (&i, head) in left.iter().zip(round) {
            let p = &pending[i];
            let Some((ty, provides)) = head else {
                waiting.push(i);
                continue;
            };
            if let Some(r) = provides.as_ref() {
                if env
                    .impls
                    .refs_on(&ty)
                    .any(|o| o.path == r.path && o.args == r.args)
                {
                    // A clash with another package's impl is an orphan,
                    // reported by `check_impl_rules`.
                    let pkg = package_of(&p.module);
                    let here = method_target_for(&ty)
                        .and_then(|t| env.impls.by_target.get(&t))
                        .into_iter()
                        .flatten()
                        .map(|&o| &env.impls.all[o])
                        .any(|o| {
                            package_of(&o.module) == pkg
                                && o.concrete
                                    .as_ref()
                                    .and_then(|(_, r)| r.as_ref())
                                    .is_some_and(|o| o.path == r.path && o.args == r.args)
                        });
                    if !here {
                        continue;
                    }
                    let shown = TraitRef::new(r.path.clone(), r.args.clone(), Vec::new());
                    let shown = shown.render(&r.path, |t| match t {
                        HirTy::Struct(id) => env.defs.structs.borrow()[id.0 as usize].name.clone(),
                        HirTy::Enum(id) => env.defs.enums.borrow()[id.0 as usize].name.clone(),
                        t => ty_mangle(t),
                    });
                    diags.push(
                        nabic_diagnostic::NabiDiagnostic::error(
                            Code::CONFLICTING_IMPLS,
                            p.def.span,
                            format!(
                                "`{shown}` is implemented more than once for `{}`",
                                impl_def_display(&p.def.self_ty)
                            ),
                        )
                        .with_help("keep one of the two impls; a type implements a trait once"),
                    );
                    continue;
                }
            }
            env.impls.resolve(p.at, ty.clone(), provides.clone());
            heads[i] = Some((ty, provides));
        }
        last = waiting.len() == left.len();
        left = waiting;
    }
    heads
}

/// A concrete impl's target and trait ref: `impl Get<i32> for T { type
/// Item = u8; }` also answers `Get<Item = u8>` and `Get<i32, Item = u8>`.
pub(crate) fn impl_head(
    rcx: &mut LowerCtx<'_>,
    p: &PendingImpl<'_>,
    trait_def: Option<&nabic_ast::TraitDef>,
) -> (HirTy, Option<TraitRef>) {
    rcx.self_ty = None;
    rcx.current_module.clone_from(&p.module);
    let target = rcx.resolve_ast_ty(&p.def.self_ty);
    let Some(tn) = &p.trait_name else {
        return (target, None);
    };
    if method_target_for(&target).is_none() {
        return (target, None);
    }
    rcx.self_ty = Some(target.clone());
    let mut args: Vec<HirTy> = p
        .def
        .trait_path
        .as_ref()
        .and_then(|tp| tp.segments.last())
        .map(|s| s.generics.iter().map(|g| rcx.resolve_ast_ty(g)).collect())
        .unwrap_or_default();
    // Omitted trait arguments take the parameter's default
    // (`trait Add<Rhs = Self>`: `impl Add for V` is `impl Add<V> for V`).
    if let Some(td) = trait_def {
        for gp in td.generics.params.iter().skip(args.len()) {
            let nabic_ast::GenericParam::Type {
                default: Some(d), ..
            } = gp
            else {
                break;
            };
            args.push(rcx.resolve_ast_ty(d));
        }
    }
    let assoc = impl_assoc_list(p.def)
        .iter()
        .map(|(n, t)| (n.clone(), rcx.resolve_ast_ty(t)))
        .collect();
    (target, Some(TraitRef::new(tn.clone(), args, assoc)))
}

/// An impl's `type X = T;` items, in order.
pub(crate) fn impl_assoc_list(impl_def: &nabic_ast::ImplDef) -> Vec<(String, Ty)> {
    impl_def
        .items
        .iter()
        .filter_map(|i| match i {
            AstImplItem::AssocType { name, ty } => Some((name.clone(), ty.clone())),
            _ => None,
        })
        .collect()
}

/// What the impl's methods write where the trait's signatures name the
/// associated type `name` (`-> i32 | none` for `-> Item | none`).
pub(crate) fn assoc_from_sigs(
    t: &nabic_ast::TraitDef,
    i: &nabic_ast::ImplDef,
    name: &str,
) -> Option<String> {
    fn find(theirs: &Ty, mine: &Ty, name: &str) -> Option<String> {
        let names = |p: &nabic_ast::Path| match p.segments.as_slice() {
            [s] if s.generics.is_empty() => s.name == name,
            [o, s] if o.name == "Self" && s.generics.is_empty() => s.name == name,
            _ => false,
        };
        let all = |a: &[Ty], b: &[Ty]| {
            (a.len() == b.len())
                .then(|| a.iter().zip(b).find_map(|(x, y)| find(x, y, name)))
                .flatten()
        };
        match (&theirs.kind, &mine.kind) {
            // The impl writing the name itself says nothing.
            (AstTyKind::Path(_), AstTyKind::Path(q)) if names(q) => None,
            (AstTyKind::Path(p), _) if names(p) => Some(nabic_ast::print::print_ty(mine)),
            (AstTyKind::Path(a), AstTyKind::Path(b)) => {
                let (x, y) = (a.segments.last()?, b.segments.last()?);
                all(&x.generics, &y.generics)
            }
            (AstTyKind::Sum(a), AstTyKind::Sum(b)) | (AstTyKind::Tuple(a), AstTyKind::Tuple(b)) => {
                all(a, b)
            }
            (AstTyKind::Nest(a), _) => find(a, mine, name),
            (_, AstTyKind::Nest(b)) => find(theirs, b, name),
            (AstTyKind::Array(a, _), AstTyKind::Array(b, _)) => find(a, b, name),
            (AstTyKind::Throws { ok: a, err: x }, AstTyKind::Throws { ok: b, err: y }) => {
                find(a, b, name).or_else(|| find(x, y, name))
            }
            _ => None,
        }
    }
    i.items.iter().find_map(|it| {
        let AstImplItem::Method(m) = it else {
            return None;
        };
        let tm = t.items.iter().find_map(|ti| match ti {
            nabic_ast::TraitItem::Method(tm) if tm.name == leaf_name(&m.name) => Some(tm),
            _ => None,
        })?;
        let ret = match (&tm.ret_ty, &m.ret_ty) {
            (Some(want), Some(got)) => find(want, got, name),
            _ => None,
        };
        ret.or_else(|| {
            tm.params
                .iter()
                .zip(&m.params)
                .find_map(|(want, got)| find(want.ty.as_ref()?, got.ty.as_ref()?, name))
        })
    })
}

/// An impl's consts, plus a trait impl's omitted defaults: `(Type::NAME,
/// type, value)` keyed by generic base or primitive name where one names
/// it, and all of them.
/// One key per base: two instances' impls can't give a const two values.
pub(crate) fn impl_consts(
    i: &nabic_ast::ImplDef,
    program: &Program,
    base: &HashMap<String, &nabic_ast::TraitDef>,
    names: &TypeNames,
    pre_diags: &mut Vec<nabic_diagnostic::NabiDiagnostic>,
) -> (Vec<AssocConst>, Vec<AssocConst>) {
    let mut out: Vec<(String, Ty, Expr)> = i
        .items
        .iter()
        .filter_map(|it| match it {
            AstImplItem::Const(c) => {
                Some((leaf_name(&c.name).to_owned(), c.ty.clone(), c.value.clone()))
            }
            _ => None,
        })
        .collect();
    let module = impl_module(i);
    let trait_def = i.trait_path.as_ref().and_then(|tp| {
        let segs: Vec<&str> = tp.segments.iter().map(|s| s.name.as_str()).collect();
        let key = names.trait_key(&segs, tp.origin.as_deref().unwrap_or(&module), i.span);
        let fits = |t: &nabic_ast::TraitDef| {
            t.name == key || (!key.contains("::") && leaf_name(&t.name) == key)
        };
        program
            .items
            .iter()
            .find_map(|it| match it {
                Item::Trait(t) if fits(t) => Some(t),
                _ => None,
            })
            .or_else(|| base.get(key.as_str()).copied())
            .or_else(|| {
                let mut leaf: Vec<&nabic_ast::TraitDef> =
                    base.values().copied().filter(|t| fits(t)).collect();
                leaf.sort_by(|a, b| a.name.cmp(&b.name));
                leaf.first().copied()
            })
    });
    if let Some(t) = trait_def.filter(|_| !i.negative) {
        for ti in &t.items {
            let nabic_ast::TraitItem::AssocType {
                name,
                default: None,
                ..
            } = ti
            else {
                continue;
            };
            let given = i
                .items
                .iter()
                .any(|it| matches!(it, AstImplItem::AssocType { name: n, .. } if n == name));
            if !given {
                pre_diags.push(
                    nabic_diagnostic::NabiDiagnostic::error(
                        Code::MISSING_TRAIT_ITEM,
                        i.span,
                        format!(
                            "impl of `{}` for `{}` is missing associated type `{name}`",
                            leaf_name(&t.name),
                            impl_def_display(&i.self_ty)
                        ),
                    )
                    .with_help(match assoc_from_sigs(t, i, name) {
                        Some(ty) => format!(
                            "the trait declares `type {name}` with no default: add \
                             `type {name} = {ty};` to the impl"
                        ),
                        None => format!(
                            "the trait declares `type {name}` with no default: add \
                             `type {name} = ..;` to the impl, naming the type it stands for here"
                        ),
                    }),
                );
            }
        }
        for ti in &t.items {
            let nabic_ast::TraitItem::Const { name, ty, default } = ti else {
                continue;
            };
            if out.iter().any(|(n, ..)| n == name) {
                continue;
            }
            match default {
                Some(v) => out.push((name.clone(), ty.clone(), v.clone())),
                None => pre_diags.push(nabic_diagnostic::NabiDiagnostic::error(
                    Code::MISSING_TRAIT_ITEM,
                    i.span,
                    format!(
                        "impl of `{}` for `{}` is missing associated const `{name}`",
                        leaf_name(&t.name),
                        impl_def_display(&i.self_ty)
                    ),
                )),
            }
        }
    }
    if out.is_empty() {
        return (Vec::new(), out);
    }
    let params: Vec<&str> = i
        .generics
        .params
        .iter()
        .filter_map(|gp| match gp {
            nabic_ast::GenericParam::Type { name, .. } => Some(name.as_str()),
            nabic_ast::GenericParam::Const { .. } => None,
        })
        .collect();
    // `Type::N` names the const when the impl covers that type or every
    // instance of a base; others are found from a concrete type (`T::N`,
    // `Self::N`, `<T as Tr>::N`).
    let keyed_target = match &i.self_ty.kind {
        AstTyKind::Path(p) => {
            let args = p.segments.last().map_or(&[][..], |s| s.generics.as_slice());
            let all_params = args.len() == params.len()
                && args.iter().zip(&params).all(|(a, n)| {
                    matches!(&a.kind, AstTyKind::Path(ap)
                        if ap.segments.len() == 1 && ap.segments[0].name == *n)
                });
            if let HirTy::Prim(prim) = resolve_ty(&i.self_ty) {
                Some(prim.name().to_owned())
            } else if (params.is_empty() && args.is_empty())
                || (i.trait_path.is_none() && !args.is_empty() && all_params)
            {
                let segs: Vec<&str> = p.segments.iter().map(|s| s.name.as_str()).collect();
                names.canon(&segs, &module, &[], i.span)
            } else {
                None
            }
        }
        _ => None,
    };
    let keyed = keyed_target.map_or_else(Vec::new, |t| {
        out.iter()
            .map(|(n, ty, v)| (format!("{t}::{n}"), ty.clone(), v.clone()))
            .collect()
    });
    (keyed, out)
}
