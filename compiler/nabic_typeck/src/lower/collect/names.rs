//! Local names, tier checks, re-export aliases and marks.

#[allow(clippy::wildcard_imports)]
use crate::lower::collect::*;

/// Show block-declared items as written: `f/S` reads
/// `S`, or `f::S` when the file has another `S`. A name not found where
/// such an item has it gets a note on its scope.
pub(crate) fn show_local_names(program: &Program, diags: &mut [nabic_diagnostic::NabiDiagnostic]) {
    if diags.is_empty() {
        return;
    }
    let names: Vec<&str> = program
        .items
        .iter()
        .filter_map(Item::name)
        .map(leaf_name)
        .collect();
    let mut lifted: Vec<&str> = names.iter().copied().filter(|n| n.contains('/')).collect();
    if lifted.is_empty() {
        return;
    }
    let mut count: HashMap<std::borrow::Cow<'_, str>, usize> = HashMap::default();
    for n in &names {
        *count.entry(nabic_core::local::leaf(n)).or_default() += 1;
    }
    lifted.sort_by_key(|l| std::cmp::Reverse(l.len()));
    let shown: Vec<(&str, String)> = lifted
        .iter()
        .map(|l| {
            let short = nabic_core::local::leaf(l);
            let s = if count[&short] > 1 {
                nabic_core::local::qualified(l)
            } else {
                short.into_owned()
            };
            (*l, s)
        })
        .collect();
    let fix = |s: &mut String| {
        for (l, shown) in &shown {
            if s.contains(l) {
                *s = s.replace(l, shown);
            }
        }
    };
    for d in diags {
        let missing = d
            .message
            .strip_prefix("cannot find ")
            .and_then(|r| r.split('`').nth(1));
        if let Some(l) =
            missing.and_then(|m| lifted.iter().find(|l| nabic_core::local::leaf(l) == m))
        {
            let outer = nabic_core::local::qualified(nabic_core::local::outer(l));
            d.notes.push(format!(
                "`{}` is declared inside `{outer}` and visible only in its block; move it to \
                 module level to use it here",
                nabic_core::local::leaf(l)
            ));
        }
        fix(&mut d.message);
        if let Some(p) = &mut d.primary {
            fix(&mut p.message);
        }
        d.secondary.iter_mut().for_each(|l| fix(&mut l.message));
        d.notes.iter_mut().for_each(fix);
    }
}

/// `bare` names neither `core` nor `std`, and
/// `core` doesn't name `std`.
pub(crate) fn check_tiers(
    raw_uses: &[(String, Visibility, nabic_ast::UseTree)],
    items: &[Item],
    modules: &[String],
) -> Vec<nabic_diagnostic::NabiDiagnostic> {
    struct Paths<'a> {
        tier: &'static str,
        out: &'a mut Vec<nabic_diagnostic::NabiDiagnostic>,
    }
    impl Paths<'_> {
        fn check(&mut self, p: &nabic_ast::Path) {
            let mut segs = p.segments.iter().map(|s| s.name.as_str());
            let Some(t) = segs
                .next()
                .filter(|r| !r.is_empty())
                .or_else(|| segs.next())
            else {
                return;
            };
            if p.segments.len() < 2 {
                return;
            }
            let forbidden = match self.tier {
                "bare" => ["core", "std"].contains(&t),
                _ => t == "std",
            };
            if forbidden {
                self.out.push(nabic_diagnostic::NabiDiagnostic::error(
                    Code::DEPENDENCY,
                    p.span,
                    format!("`{}` must not depend on `{t}`", self.tier),
                ));
            }
        }
    }
    impl crate::walk::AstVisitor for Paths<'_> {
        fn path(&mut self, p: &nabic_ast::Path) {
            self.check(p);
        }
    }
    // The prelude is what user code imports, not core code: on unix
    // targets it names std's OS Ext traits.
    let tier = |module: &str| match package_of(module) {
        _ if module == crate::lower::resolve::PRELUDE => None,
        "bare" => Some("bare"),
        "core" => Some("core"),
        _ => None,
    };
    let mut out = Vec::new();
    for (item, module) in items.iter().zip(modules) {
        if let Some(tier) = tier(module) {
            crate::walk::walk_item(
                &mut Paths {
                    tier,
                    out: &mut out,
                },
                item,
            );
        }
    }
    for (module, _, tree) in raw_uses {
        if let Some(tier) = tier(module) {
            Paths {
                tier,
                out: &mut out,
            }
            .check(&tree.prefix);
        }
    }
    out
}

/// Put each declaration's signature in `fn_table` (aliases included).
pub(crate) fn install_sigs(
    fn_table: &mut HashMap<String, std::sync::Arc<FnSig>>,
    sigs: &HashMap<FunctionId, FnSig>,
) {
    for sig in fn_table.values_mut() {
        if let Some(s) = sigs.get(&sig.id) {
            *sig = std::sync::Arc::new(s.clone());
        }
    }
}

/// Re-resolve fields and payloads left open (a projection) now that impls
/// are known, an instance's under its arguments. Union storage is fixed at
/// registration.
pub(crate) fn resolve_open_members(
    env: &Env<'_>,
    structs: &[(StructId, &AstStructDef)],
    enums: &[(EnumId, &AstEnumDef)],
) {
    let structs: HashMap<usize, &AstStructDef> =
        structs.iter().map(|(id, d)| (id.0 as usize, *d)).collect();
    let enums: HashMap<usize, &AstEnumDef> =
        enums.iter().map(|(id, d)| (id.0 as usize, *d)).collect();
    let open = |t: &HirTy| {
        let mut o = Vec::new();
        collect_unknown_paths(t, &mut o);
        !o.is_empty()
    };
    let struct_open: Vec<(usize, &AstStructDef, Vec<HirTy>)> = env
        .defs
        .structs
        .borrow()
        .iter()
        .enumerate()
        .filter(|(_, d)| d.union_kind.is_none() && d.fields.iter().any(|f| open(&f.ty)))
        .filter_map(|(i, d)| {
            let def = match structs.get(&i) {
                Some(s) if d.generic_args.is_empty() => *s,
                _ => *env.defs.generic_structs.get(enum_base_name(&d.name))?,
            };
            Some((i, def, d.generic_args.clone()))
        })
        .collect();
    for (i, def, args) in struct_open {
        let subst = generic_subst(&def.generics, &args);
        let mut rcx = LowerCtx::new(env, &subst, None, def.span);
        rcx.current_module = module_of(enum_base_name(&def.name));
        let fields = generic_struct_fields(def, &|t| rcx.resolve_ast_ty(t));
        env.defs.structs.borrow_mut()[i].fields = fields;
    }
    let enum_open: Vec<(usize, &AstEnumDef, Vec<HirTy>)> = env
        .defs
        .enums
        .borrow()
        .iter()
        .enumerate()
        .filter(|(_, d)| {
            d.variants
                .iter()
                .any(|v| v.payload_ty.as_ref().is_some_and(open))
        })
        .filter_map(|(i, d)| {
            let def = match enums.get(&i) {
                Some(e) if d.generic_args.is_empty() => *e,
                _ => *env.defs.generic_enums.get(enum_base_name(&d.name))?,
            };
            Some((i, def, d.generic_args.clone()))
        })
        .collect();
    for (i, def, args) in enum_open {
        let subst = generic_subst(&def.generics, &args);
        let mut rcx = LowerCtx::new(env, &subst, None, def.span);
        rcx.current_module = module_of(enum_base_name(&def.name));
        let payloads = generic_enum_variants(def, &|t| rcx.resolve_ast_ty(t));
        let mut all = env.defs.enums.borrow_mut();
        for (v, fresh) in all[i].variants.iter_mut().zip(payloads) {
            v.payload_ty = fresh.payload_ty;
        }
    }
}

pub(crate) fn reexport_key(re: &crate::uses::ReExport) -> String {
    if re.module_path.is_empty() {
        re.short_name.clone()
    } else {
        format!("{}::{}", re.module_path, re.short_name)
    }
}

/// Headers of block-lifted items with `use p::*`s (`#block_glob`) name
/// what the globs bring in by path, as named `use`s do.
pub(crate) fn block_glob_renames(
    items: &[Item],
    item_modules: &[String],
    all: &[&Item],
    reexports: &[crate::uses::ReExport],
    uses: &crate::uses::UseTable,
) -> Vec<(usize, nabic_ast::rename::Renames)> {
    let blocks = |item: &Item| -> Vec<Vec<String>> {
        (item.attrs().iter())
            .filter(|a| a.path == [nabic_ast::BLOCK_GLOB_ATTR])
            .map(|a| match &a.body {
                nabic_ast::AttrBody::Args(args) => (args.iter())
                    .filter_map(|x| match x {
                        nabic_ast::AttrArg::Lit(nabic_ast::Literal::Str(s)) => Some(s.clone()),
                        _ => None,
                    })
                    .collect(),
                _ => Vec::new(),
            })
            .collect()
    };
    let mut names = None;
    let mut out = Vec::new();
    for (i, (item, module)) in items.iter().zip(item_modules).enumerate() {
        let blocks = blocks(item);
        if blocks.is_empty() {
            continue;
        }
        let names = names.get_or_insert_with(|| crate::uses::GlobNames::new(all, reexports));
        let paths = names.block_paths(&blocks, module, uses);
        if !paths.is_empty() {
            out.push((
                i,
                nabic_ast::rename::Renames {
                    paths,
                    headers_only: true,
                    ..Default::default()
                },
            ));
        }
    }
    out
}

pub(crate) fn qualify(item: &mut Item, module: &str) {
    if module.is_empty() {
        return;
    }
    let q = |name: &mut String| *name = format!("{module}::{name}");
    match item {
        Item::Fn(f) => q(&mut f.name),
        Item::Struct(s) => q(&mut s.name),
        Item::Enum(e) => q(&mut e.name),
        Item::Const(c) => q(&mut c.name),
        Item::Static(s) => q(&mut s.name),
        Item::TypeAlias(a) => q(&mut a.name),
        Item::Trait(t) => q(&mut t.name),
        Item::Union(u) => q(&mut u.name),
        Item::Mod(m) => q(&mut m.name),
        Item::Impl(i) => {
            for it in &mut i.items {
                match it {
                    nabic_ast::ImplItem::Method(m) => q(&mut m.name),
                    nabic_ast::ImplItem::Const(c) => q(&mut c.name),
                    nabic_ast::ImplItem::AssocType { .. } => {}
                }
            }
            module.clone_into(&mut i.module);
        }
        Item::Use(_) | Item::Extern(_) => {}
    }
}

/// Resolve plain aliases of projections once impls exist, reporting at the
/// body; a broken one is `{unknown}` where used.
pub(crate) fn resolve_projection_aliases(env: &Env<'_>, program: &Program) {
    let names = &env.names;
    let stage: nabic_core::HashSet<&str> = program
        .items
        .iter()
        .filter_map(|it| match it {
            Item::TypeAlias(a) => Some(a.name.as_str()),
            _ => None,
        })
        .collect();
    let mut aliases: Vec<(&String, &Ty)> = names
        .plain_aliases
        .iter()
        .filter(|(full, body)| {
            stage.contains(full.as_str())
                && !names.broken_aliases.borrow().contains(*full)
                && names.names_projection(body, &module_of(full), body.span)
        })
        .collect();
    aliases.sort_by_key(|(full, _)| *full);
    let no_subst = HashMap::default();
    for (full, body) in aliases {
        let count = || names.diags.borrow().len() + env.sink.diagnostics.borrow().len();
        let before = count();
        let mut rcx = LowerCtx::new(env, &no_subst, None, body.span);
        rcx.current_module = module_of(full);
        rcx.resolve_ast_ty(body);
        if count() != before {
            names.broken_aliases.borrow_mut().insert(full.clone());
        }
    }
}

/// Each `pub use` alias key with the end of its re-export chain (a cycle
/// stops where it closes), in re-export order. A key may name a module
/// and a value at once (`channel`); a chain goes on through a key with
/// one target only.
pub(crate) fn reexport_aliases(reexports: &[crate::uses::ReExport]) -> Vec<(String, String)> {
    let mut direct: HashMap<String, Vec<&str>> = HashMap::default();
    for re in reexports {
        let to = direct.entry(reexport_key(re)).or_default();
        if !to.contains(&re.full_path.as_str()) {
            to.push(&re.full_path);
        }
    }
    let mut out: Vec<(String, String)> = Vec::new();
    let mut have = nabic_core::HashSet::default();
    for re in reexports {
        let alias = reexport_key(re);
        let mut seen = nabic_core::HashSet::from_iter([alias.as_str()]);
        let mut end = re.full_path.as_str();
        while let Some([next]) = direct
            .get(end)
            .map(Vec::as_slice)
            .filter(|_| seen.insert(end))
        {
            end = next;
        }
        if have.insert((alias.clone(), end.to_owned())) {
            out.push((alias, end.to_owned()));
        }
    }
    out
}

/// Alias -> end, the first re-export of a key winning.
pub(crate) fn first_alias<'p>(
    pairs: impl Iterator<Item = &'p (String, String)>,
) -> HashMap<String, String> {
    let mut out = HashMap::default();
    for (alias, end) in pairs {
        out.entry(alias.clone()).or_insert_with(|| end.clone());
    }
    out
}

/// Source-ish rendering of an AST type for diagnostics.
pub(crate) fn impl_def_display(ty: &Ty) -> String {
    let list = |ts: &[Ty], sep: &str| {
        ts.iter()
            .map(impl_def_display)
            .collect::<Vec<_>>()
            .join(sep)
    };
    match &ty.kind {
        AstTyKind::Path(p) => {
            let path = p
                .segments
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
                .join("::");
            match p.segments.last().map(|s| s.generics.as_slice()) {
                Some(args) if !args.is_empty() => format!("{path}<{}>", list(args, ", ")),
                _ => path,
            }
        }
        AstTyKind::Tuple(ts) => format!("({})", list(ts, ", ")),
        AstTyKind::Sum(ts) => list(ts, " | "),
        AstTyKind::Nest(x) => impl_def_display(x),
        AstTyKind::Binding(n, t) => format!("{n} = {}", impl_def_display(t)),
        AstTyKind::QPath(q) => format!(
            "<{} as {}>::{}",
            impl_def_display(&q.self_ty),
            impl_def_display(&Ty {
                kind: AstTyKind::Path(q.trait_path.clone()),
                span: ty.span,
            }),
            q.item
        ),
        AstTyKind::None => "none".to_owned(),
        AstTyKind::Array(e, nabic_ast::ConstExpr::Literal(nabic_ast::Literal::Int(n))) => {
            format!("[{}; {n}]", impl_def_display(e))
        }
        AstTyKind::Array(e, nabic_ast::ConstExpr::Path(p)) if p.segments.len() == 1 => {
            format!("[{}; {}]", impl_def_display(e), p.segments[0].name)
        }
        AstTyKind::Array(e, nabic_ast::ConstExpr::Expr(x)) => match &x.kind {
            ExprKind::Path(p) if p.segments.len() == 1 => {
                format!("[{}; {}]", impl_def_display(e), p.segments[0].name)
            }
            _ => format!("[{}; _]", impl_def_display(e)),
        },
        _ => nabic_ast::print::print_ty(ty),
    }
}

/// The name a warning shows for marked function `d`: `f`, `P::new`.
pub(crate) fn mark_name(env: &Env<'_>, d: &MethodOrFn<'_>) -> String {
    let leaf = leaf_name(&d.def().name).to_owned();
    let MethodOrFn::Method { self_ty, .. } = d else {
        return leaf;
    };
    let ty = match self_ty {
        HirTy::Struct(id) => env.defs.structs.borrow()[id.0 as usize].name.clone(),
        HirTy::Enum(id) => env.defs.enums.borrow()[id.0 as usize].name.clone(),
        t => ty_mangle(t),
    };
    let base = ty.split("__").next().unwrap_or(&ty);
    format!("{}::{leaf}", leaf_name(base))
}

/// The marks of `program`'s fields (`m::P.x`, `m::P.0`), consts and
/// statics (`extern` ones too).
pub(crate) fn item_marks(program: &Program) -> Vec<(String, crate::marks::Mark)> {
    let mut out = Vec::new();
    let mut add = |key: String, attrs: &[nabic_ast::Attribute]| {
        if let Some(m) = mark_of(attrs) {
            out.push((key, m));
        }
    };
    for item in &program.items {
        match item {
            Item::Struct(s) => match &s.kind {
                AstStructKind::Named(fs) => {
                    for f in fs {
                        add(format!("{}.{}", s.name, f.name), &f.attrs);
                    }
                }
                AstStructKind::Tuple(fs) => {
                    for (i, f) in fs.iter().enumerate() {
                        add(format!("{}.{i}", s.name), &f.attrs);
                    }
                }
                AstStructKind::Unit => {}
            },
            Item::Union(u) => {
                for f in &u.fields {
                    add(format!("{}.{}", u.name, f.name), &f.attrs);
                }
            }
            Item::Const(c) => add(c.name.clone(), &c.attrs),
            Item::Static(st) => add(st.name.clone(), &st.attrs),
            Item::Extern(b) => {
                for ei in &b.items {
                    if let nabic_ast::ExternItem::Static(st) = ei {
                        add(st.name.clone(), &st.attrs);
                    }
                }
            }
            _ => {}
        }
    }
    out
}

pub(crate) fn collect_field_visibility(
    program: &Program,
) -> HashMap<String, HashMap<String, Visibility>> {
    let mut out: HashMap<String, HashMap<String, Visibility>> = HashMap::default();
    for item in &program.items {
        match item {
            Item::Struct(s) => {
                let vis: HashMap<String, Visibility> = match &s.kind {
                    AstStructKind::Named(fs) => {
                        fs.iter().map(|f| (f.name.clone(), f.vis)).collect()
                    }
                    // Positional fields are named "0", "1", ... as in `HirField`.
                    AstStructKind::Tuple(fs) => fs
                        .iter()
                        .enumerate()
                        .map(|(i, f)| (i.to_string(), f.vis))
                        .collect(),
                    AstStructKind::Unit => continue,
                };
                out.entry(s.name.clone()).or_insert(vis);
            }
            Item::Union(u) => {
                out.entry(u.name.clone())
                    .or_insert_with(|| u.fields.iter().map(|f| (f.name.clone(), f.vis)).collect());
            }
            _ => {}
        }
    }
    out
}

/// What Nabi writes for another language's type, such as `Option<T>`,
/// with `T` as written at `span`.
pub(crate) fn habit_type_help(
    cur: &str,
    name: &str,
    span: nabic_diagnostic::Span,
    sources: &nabic_diagnostic::SourceMap,
) -> Option<String> {
    if let Some(h) = crate::lower::wide_int_help(name) {
        return Some(h);
    }
    let (_, text) = sources.file(span.file)?;
    let rest = text.get(span.start as usize..)?.strip_prefix(name)?;
    let args = rest.strip_prefix('<').and_then(|r| {
        let mut depth = 1;
        let end = r.find(|c| {
            depth += match c {
                '<' => 1,
                '>' => -1,
                _ => 0,
            };
            depth == 0
        })?;
        Some(r[..end].trim())
    });
    if let Some(h) = crate::lower::habits::type_help(cur, name, args) {
        return Some(h);
    }
    (name == "Option").then(|| {
        format!(
            "Nabi has no `Option`: an optional is `T | none`; write `{} | none`",
            args.map_or_else(|| "T".to_owned(), crate::lower::habits::nabi_type)
        )
    })
}
