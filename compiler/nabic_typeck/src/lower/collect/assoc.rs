//! Associated types: defaults, projections and substitution.

#[allow(clippy::wildcard_imports)]
use crate::lower::collect::*;

/// `impl_assoc_types` for a trait default lowered in the trait's module:
/// each type resolves in `module`, the impl's, which may name what the
/// trait's module does not see (`type Item = DirEntry`). The impl's own
/// type parameters (`params`) stay as written.
pub(crate) fn impl_assoc_types_from(
    impl_def: &nabic_ast::ImplDef,
    module: &str,
) -> HashMap<String, Ty> {
    fn stamp(t: &mut Ty, module: &str, params: &[&str]) {
        let mut rec = |t: &mut Ty| stamp(t, module, params);
        match &mut t.kind {
            AstTyKind::Path(p) => {
                let param =
                    matches!(p.segments.as_slice(), [s] if params.contains(&s.name.as_str()));
                if p.origin.is_none() && !param {
                    p.origin = Some(module.to_owned());
                }
                for s in &mut p.segments {
                    s.generics.iter_mut().for_each(&mut rec);
                }
            }
            AstTyKind::Sum(ts) | AstTyKind::Tuple(ts) => ts.iter_mut().for_each(rec),
            AstTyKind::Fn(ps, r, _) => {
                ps.iter_mut().for_each(&mut rec);
                rec(r);
            }
            AstTyKind::Nest(x) | AstTyKind::Binding(_, x) | AstTyKind::Array(x, _) => rec(x),
            AstTyKind::Throws { ok, err } => {
                rec(ok);
                rec(err);
            }
            _ => {}
        }
    }
    let params: Vec<&str> = impl_def
        .generics
        .params
        .iter()
        .map(|gp| match gp {
            nabic_ast::GenericParam::Type { name, .. }
            | nabic_ast::GenericParam::Const { name, .. } => name.as_str(),
        })
        .collect();
    let mut assoc = impl_assoc_types(impl_def);
    for t in assoc.values_mut() {
        stamp(t, module, &params);
    }
    assoc
}

/// An impl's associated types: `type Item = T;` maps `Item` to `T`.
pub(crate) fn impl_assoc_types(impl_def: &nabic_ast::ImplDef) -> HashMap<String, Ty> {
    impl_def
        .items
        .iter()
        .filter_map(|i| match i {
            AstImplItem::AssocType { name, ty } => Some((name.clone(), ty.clone())),
            _ => None,
        })
        .collect()
}

/// A trait const argument: a literal, a `const` (from `module`, then the
/// root) or a braced const expression over those.
pub(crate) fn const_arg_value(t: &Ty, names: &TypeNames, module: &str) -> Option<i128> {
    let c = match &t.kind {
        AstTyKind::Const(c) => c.clone(),
        AstTyKind::Path(p) => nabic_ast::ConstExpr::Path(p.clone()),
        _ => return None,
    };
    names.const_int(&c, module, &|_| None)
}

/// `v` as a const type argument.
pub(crate) fn const_literal_ty(v: i128, span: nabic_diagnostic::Span) -> Ty {
    let lit = Expr {
        kind: ExprKind::Literal(nabic_ast::Literal::Int(v.unsigned_abs().to_string())),
        span,
    };
    let e = if v < 0 {
        Expr {
            kind: ExprKind::OpPrefix {
                op: "-".to_owned(),
                operand: Box::new(lit),
            },
            span,
        }
    } else {
        lit
    };
    Ty {
        kind: AstTyKind::Const(nabic_ast::ConstExpr::Expr(Box::new(e))),
        span,
    }
}

/// Trait type parameter -> the impl's argument (or the parameter's
/// default, `Self` meaning the impl target).
pub(crate) fn trait_param_args(
    t: &nabic_ast::TraitDef,
    impl_def: &nabic_ast::ImplDef,
) -> HashMap<String, Ty> {
    let given: &[Ty] = impl_def
        .trait_path
        .as_ref()
        .and_then(|tp| tp.segments.last())
        .map_or(&[], |s| s.generics.as_slice());
    let mut out = HashMap::default();
    for (i, gp) in t.generics.params.iter().enumerate() {
        let (name, default) = match gp {
            nabic_ast::GenericParam::Type { name, default, .. } => (name, default),
            nabic_ast::GenericParam::Const { name, .. } => (name, &None),
        };
        let ty = given.get(i).cloned().or_else(|| {
            default.as_ref().map(|d| match &d.kind {
                AstTyKind::Path(p) if p.segments.len() == 1 && p.segments[0].name == "Self" => {
                    impl_def.self_ty.clone()
                }
                _ => d.clone(),
            })
        });
        if let Some(ty) = ty {
            out.insert(name.clone(), ty);
        }
    }
    out
}

/// `Self::X` from a supertrait in a default's signature becomes the
/// projection on the impl's target (`ListIter<T>::Item`).
pub(crate) fn supertrait_projections(
    m: &FnDef,
    t: &nabic_ast::TraitDef,
    impl_def: &nabic_ast::ImplDef,
    args: &mut HashMap<String, Ty>,
) {
    let AstTyKind::Path(target) = &impl_def.self_ty.kind else {
        return;
    };
    let mut needs = Vec::new();
    let tys = m.params.iter().filter_map(|p| p.ty.as_ref());
    for ty in tys.chain(m.ret_ty.as_ref()) {
        ast_ty_needs(ty, &[], &[], &mut needs);
    }
    let own = trait_assoc_names(t);
    for x in needs.iter().filter_map(|n| n.strip_prefix("Self::")) {
        if args.contains_key(x) || own.iter().any(|o| o == x) {
            continue;
        }
        let mut p = target.clone();
        p.segments.push(nabic_ast::PathSegment {
            name: x.to_owned(),
            generics: Vec::new(),
        });
        let ty = Ty {
            kind: AstTyKind::Path(p),
            span: impl_def.self_ty.span,
        };
        args.insert(x.to_owned(), ty);
    }
}

pub(crate) fn trait_assoc_names(t: &nabic_ast::TraitDef) -> Vec<String> {
    t.items
        .iter()
        .filter_map(|i| match i {
            nabic_ast::TraitItem::AssocType { name, .. } => Some(name.clone()),
            _ => None,
        })
        .collect()
}

/// Does `m`'s signature name one of `assoc` (bare or `Self::X`)?
pub(crate) fn sig_mentions(m: &FnDef, assoc: &[String]) -> bool {
    let params: Vec<&str> = Vec::new();
    let names: Vec<&str> = assoc.iter().map(String::as_str).collect();
    let mut out = Vec::new();
    for p in &m.params {
        if let Some(t) = &p.ty {
            ast_ty_needs(t, &params, &names, &mut out);
        }
    }
    ast_ty_needs(&sig_ret_ast(m), &params, &names, &mut out);
    out.iter().any(|n| n.starts_with("Self::"))
}

/// Does `m` return a generic over `Self` (`Flatten<Self>`)? Such a default
/// is instantiated only when called, since the adapter may not fit every
/// implementor.
pub(crate) fn returns_self_generic(m: &FnDef) -> bool {
    matches!(&sig_ret_ast(m).kind, AstTyKind::Path(p)
        if p.segments.iter().any(|s| s.generics.iter().any(|g| matches!(g.kind, AstTyKind::SelfUpper))))
}

/// The one-segment path type `name`.
pub(crate) fn path_ty_named(name: &str, span: nabic_diagnostic::Span) -> Ty {
    Ty {
        kind: AstTyKind::Path(nabic_ast::Path {
            segments: vec![nabic_ast::PathSegment {
                name: name.to_owned(),
                generics: Vec::new(),
            }],
            span,
            origin: None,
        }),
        span,
    }
}

/// In a trait or impl, `X` and `Self::X` name
/// the associated type, shadowing outer `X`. Impl signatures get the
/// impl's type; methods are marked `ASSOC_TYPES_ATTR` so bodies read `X`
/// as `Self::X`.
pub(crate) fn add_self_assoc_sigs(program: &mut Program) {
    let mark = |d: &mut FnDef, names: Vec<String>| {
        d.attrs.push(nabic_ast::Attribute {
            path: std::iter::once(crate::ASSOC_TYPES_ATTR.to_owned())
                .chain(names)
                .collect(),
            body: nabic_ast::AttrBody::default(),
            inner: false,
            span: d.span,
        });
    };
    for it in &mut program.items {
        match it {
            Item::Impl(i) => {
                let assoc = impl_assoc_types(i);
                if assoc.is_empty() {
                    continue;
                }
                for m in &mut i.items {
                    if let AstImplItem::Method(d) = m {
                        for prm in &mut d.params {
                            if let Some(t) = &mut prm.ty {
                                subst_assoc_ty(t, &assoc);
                            }
                        }
                        if let Some(t) = &mut d.ret_ty {
                            subst_assoc_ty(t, &assoc);
                        }
                        if let nabic_ast::ThrowsKind::Typed(ts) = &mut d.throws {
                            ts.iter_mut().for_each(|t| subst_assoc_ty(t, &assoc));
                        }
                        let mut names: Vec<String> = assoc.keys().cloned().collect();
                        names.sort_unstable();
                        mark(d, names);
                    }
                }
            }
            Item::Trait(t) => {
                let names = trait_assoc_names(t);
                if names.is_empty() {
                    continue;
                }
                for ti in &mut t.items {
                    if let nabic_ast::TraitItem::Method(d) = ti {
                        mark(d, names.clone());
                    }
                }
            }
            _ => {}
        }
    }
}

/// Associated type defaults: an impl omitting `type X` gets the trait's,
/// resolved in the trait's module with its parameters replaced. Returns
/// the additions by item index.
pub(crate) fn assoc_defaults(
    program: &Program,
    names: &TypeNames,
    base: &HashMap<String, &nabic_ast::TraitDef>,
) -> Vec<(usize, Vec<AstImplItem>)> {
    let traits: HashMap<&str, &nabic_ast::TraitDef> = program
        .items
        .iter()
        .filter_map(|it| match it {
            Item::Trait(t) => Some((t.name.as_str(), t)),
            _ => None,
        })
        .chain(base.iter().map(|(k, t)| (k.as_str(), *t)))
        .filter(|(_, t)| {
            t.items.iter().any(|i| {
                matches!(
                    i,
                    nabic_ast::TraitItem::AssocType {
                        default: Some(_),
                        ..
                    }
                )
            })
        })
        .collect();
    let mut out = Vec::new();
    if traits.is_empty() {
        return out;
    }
    for (idx, it) in program.items.iter().enumerate() {
        let Item::Impl(i) = it else { continue };
        let Some(tp) = &i.trait_path else { continue };
        let module = impl_module(i);
        let segs: Vec<&str> = tp.segments.iter().map(|s| s.name.as_str()).collect();
        let Some(td) = traits.get(
            names
                .trait_key(&segs, tp.origin.as_deref().unwrap_or(&module), i.span)
                .as_str(),
        ) else {
            continue;
        };
        let have = impl_assoc_types(i);
        let params = trait_param_args(td, i);
        let mut added = Vec::new();
        for ti in &td.items {
            let nabic_ast::TraitItem::AssocType {
                name,
                default: Some(d),
                ..
            } = ti
            else {
                continue;
            };
            if have.contains_key(name) {
                continue;
            }
            let mut ty = d.clone();
            names.absolutize(&mut ty, &module_of(&td.name), &module, &params, i.span);
            subst_assoc_ty(&mut ty, &params);
            added.push(AstImplItem::AssocType {
                name: name.clone(),
                ty,
            });
        }
        if !added.is_empty() {
            out.push((idx, added));
        }
    }
    out
}

/// Bodies without `-> T` that may diverge on every path, by
/// span, so their return type is inferred (`!` if lowering confirms): each
/// ends in `throw`, a `loop`, a call of a known diverging name, or
/// branches that all do. Names match by leaf (lowering settles it);
/// `names` accumulates across stages.
pub(crate) fn may_diverge(
    program: &Program,
    names: &mut nabic_core::HashSet<String>,
) -> nabic_core::HashSet<nabic_diagnostic::Span> {
    fn tail_block(b: &Block, d: &HashSet<String>) -> bool {
        if let Some(t) = &b.trailing {
            return tail(t, d);
        }
        b.stmts.last().is_some_and(|s| match &s.kind {
            StmtKind::Throw(_) => true,
            StmtKind::Expr(e) => tail(e, d),
            _ => false,
        })
    }
    /// A `loop` with no `break` / `return` in it (outside closures).
    fn endless(body: &Block) -> bool {
        struct V {
            depth: u32,
            exits: bool,
        }
        impl crate::walk::AstVisitor for V {
            fn stmt(&mut self, s: &nabic_ast::Stmt) {
                if self.depth == 0 && matches!(s.kind, StmtKind::Break { .. } | StmtKind::Return(_))
                {
                    self.exits = true;
                }
            }
            fn enter_body(&mut self) {
                self.depth += 1;
            }
            fn leave_body(&mut self) {
                self.depth -= 1;
            }
        }
        let mut v = V {
            depth: 0,
            exits: false,
        };
        crate::walk::walk_block(&mut v, body);
        !v.exits
    }
    fn tail(e: &Expr, d: &HashSet<String>) -> bool {
        match &e.kind {
            ExprKind::Loop { body, .. } => endless(body),
            ExprKind::Call { callee, .. } => match &callee.kind {
                ExprKind::Path(p) => p.segments.last().is_some_and(|s| d.contains(&s.name)),
                ExprKind::QPath(q) => d.contains(q.member.as_ref().unwrap_or(&q.item)),
                _ => false,
            },
            ExprKind::MethodCall { method, .. } => d.contains(method),
            ExprKind::Try { operand, .. } => tail(operand, d),
            ExprKind::Block(b) | ExprKind::Unsafe(b) | ExprKind::ConstBlock(b) => tail_block(b, d),
            ExprKind::If {
                then_branch,
                else_branch: Some(eb),
                ..
            } => tail_block(then_branch, d) && tail(eb, d),
            ExprKind::Match { arms, .. } => {
                !arms.is_empty() && arms.iter().all(|a| tail(&a.body, d))
            }
            _ => false,
        }
    }
    let mut fns: Vec<&FnDef> = Vec::new();
    for it in &program.items {
        match it {
            Item::Fn(f) => fns.push(f),
            Item::Impl(i) => fns.extend(i.items.iter().filter_map(|m| match m {
                AstImplItem::Method(f) => Some(f),
                _ => None,
            })),
            Item::Trait(t) => fns.extend(t.items.iter().filter_map(|m| match m {
                nabic_ast::TraitItem::Method(f) => Some(f),
                _ => None,
            })),
            _ => {}
        }
    }
    names.extend(
        fns.iter()
            .filter(|f| {
                f.ret_ty
                    .as_ref()
                    .is_some_and(|t| matches!(t.kind, AstTyKind::Never))
            })
            .map(|f| leaf_name(&f.name).to_owned()),
    );
    let open: Vec<&FnDef> = fns
        .into_iter()
        .filter(|f| {
            (f.ret_ty.is_none() || mono::opaque_bounds(f).is_some())
                && f.body.is_some()
                && !f.attrs.iter().any(|a| a.path == [nabic_ast::UNIT_RET_ATTR])
        })
        .collect();
    let mut out = HashSet::default();
    loop {
        let before = out.len();
        for f in &open {
            if !out.contains(&f.span) && f.body.as_ref().is_some_and(|b| tail_block(b, names)) {
                out.insert(f.span);
                names.insert(leaf_name(&f.name).to_owned());
            }
        }
        if out.len() == before {
            return out;
        }
    }
}

/// Does generic impl `i` cover every instance of its base: arguments are
/// distinct impl parameters (`Box<T>`, not `P<str, V>` or `P<T, T>`)?
pub(crate) fn covers_base(i: &nabic_ast::ImplDef) -> bool {
    let AstTyKind::Path(p) = &i.self_ty.kind else {
        return false;
    };
    let args = p.segments.last().map_or(&[][..], |s| s.generics.as_slice());
    let mut seen = nabic_core::HashSet::default();
    args.iter().all(|a| {
        matches!(&a.kind, AstTyKind::Path(ap)
            if ap.segments.len() == 1
                && ap.segments[0].generics.is_empty()
                && i.generics.params.iter().any(|gp| matches!(gp,
                    nabic_ast::GenericParam::Type { name, .. }
                    | nabic_ast::GenericParam::Const { name, .. } if *name == ap.segments[0].name))
                && seen.insert(ap.segments[0].name.as_str()))
    })
}

/// Expand generic alias targets (`impl<T> Al<T>` with `type Al<T> = S<T>`)
/// before impls are keyed.
pub(crate) fn expand_impl_aliases(program: &mut Program, names: &TypeNames) {
    for it in &mut program.items {
        let Item::Impl(i) = it else { continue };
        let module = impl_module(i);
        for _ in 0..32 {
            let Some(mut ty) = names.expand_alias(&i.self_ty, &module, i.span) else {
                break;
            };
            ty.span = i.self_ty.span;
            i.self_ty = ty;
        }
    }
}

pub(crate) fn impl_param_types(
    program: &mut Program,
    names: &TypeNames,
    base: &HashMap<String, &nabic_ast::TraitDef>,
) {
    let traits: HashMap<&str, &nabic_ast::TraitDef> = program
        .items
        .iter()
        .filter_map(|it| match it {
            Item::Trait(t) => Some((t.name.as_str(), t)),
            _ => None,
        })
        .chain(base.iter().map(|(k, t)| (k.as_str(), *t)))
        .collect();
    let untyped = |p: &Param| p.ty.is_none() && p.name != "self" && !p.variadic;
    let mut fills: Vec<(usize, usize, usize, Ty)> = Vec::new();
    for (ii, it) in program.items.iter().enumerate() {
        let Item::Impl(i) = it else { continue };
        let Some(tp) = &i.trait_path else { continue };
        let has_untyped =
            |m: &AstImplItem| matches!(m, AstImplItem::Method(d) if d.params.iter().any(untyped));
        if !i.items.iter().any(has_untyped) {
            continue;
        }
        let module = impl_module(i);
        let segs: Vec<&str> = tp.segments.iter().map(|s| s.name.as_str()).collect();
        let Some(td) = traits.get(
            names
                .trait_key(&segs, tp.origin.as_deref().unwrap_or(&module), i.span)
                .as_str(),
        ) else {
            continue;
        };
        let params = trait_param_args(td, i);
        for (mi, m) in i.items.iter().enumerate() {
            let AstImplItem::Method(d) = m else { continue };
            let decl = td.items.iter().find_map(|ti| match ti {
                nabic_ast::TraitItem::Method(t) if leaf_name(&t.name) == leaf_name(&d.name) => {
                    Some(t)
                }
                _ => None,
            });
            let Some(decl) = decl else { continue };
            let mut own = params.clone();
            for gp in &decl.generics.params {
                let (nabic_ast::GenericParam::Type { name, .. }
                | nabic_ast::GenericParam::Const { name, .. }) = gp;
                own.entry(name.clone())
                    .or_insert_with(|| path_ty_named(name, decl.span));
            }
            for (pi, (p, tp)) in d.params.iter().zip(&decl.params).enumerate() {
                let (true, Some(t)) = (untyped(p), &tp.ty) else {
                    continue;
                };
                let mut ty = t.clone();
                names.absolutize(&mut ty, &module_of(&td.name), &module, &own, i.span);
                subst_assoc_ty(&mut ty, &params);
                fills.push((ii, mi, pi, ty));
            }
        }
    }
    for (ii, mi, pi, ty) in fills {
        if let Item::Impl(i) = &mut program.items[ii] {
            if let AstImplItem::Method(d) = &mut i.items[mi] {
                d.params[pi].ty = Some(ty);
            }
        }
    }
}

/// Where a trait or impl writes a path starting with `name`, outside its
/// header.
pub(crate) fn bare_uses(it: &Item, name: &str) -> Vec<nabic_diagnostic::Span> {
    struct Uses<'n> {
        name: &'n str,
        out: Vec<nabic_diagnostic::Span>,
    }
    impl crate::walk::AstVisitor for Uses<'_> {
        fn path(&mut self, p: &nabic_ast::Path) {
            if p.segments.first().is_some_and(|s| s.name == self.name) {
                self.out.push(p.span);
            }
        }
    }
    let mut v = Uses {
        name,
        out: Vec::new(),
    };
    match it {
        Item::Impl(i) => {
            for ii in &i.items {
                match ii {
                    AstImplItem::Method(m) => crate::walk::walk_fn(&mut v, m),
                    AstImplItem::AssocType { ty, .. } => crate::walk::walk_ty(&mut v, ty),
                    AstImplItem::Const(c) => {
                        crate::walk::walk_ty(&mut v, &c.ty);
                        crate::walk::walk_expr(&mut v, &c.value);
                    }
                }
            }
        }
        _ => crate::walk::walk_item(&mut v, it),
    }
    v.out
}

/// An associated type that hides an outer type of the same
/// name inside its trait / impl. A bare use of the name there is an
/// error; the declaration alone warns.
pub(crate) fn check_assoc_shadowing(
    program: &Program,
    names: &TypeNames,
    diags: &mut Vec<nabic_diagnostic::NabiDiagnostic>,
) {
    for it in &program.items {
        let (assoc, module, owner) = match it {
            Item::Impl(i) => (
                i.items
                    .iter()
                    .filter_map(|m| match m {
                        AstImplItem::AssocType { name, .. } => Some(name.clone()),
                        _ => None,
                    })
                    .collect(),
                impl_module(i),
                format!("impl for `{}`", impl_def_display(&i.self_ty)),
            ),
            Item::Trait(t) => (
                trait_assoc_names(t),
                item_module(it),
                format!("trait `{}`", leaf_name(&t.name)),
            ),
            _ => continue,
        };
        for a in assoc {
            let prim = matches!(
                crate::ty::resolve_ty(&path_ty_named(&a, it.span())),
                HirTy::Prim(_)
            );
            // A trait of that name is not a type (`trait IntoFuture { type
            // IntoFuture; }`).
            let outer = names
                .canon(&[a.as_str()], &module, &[], it.span())
                .filter(|full| !names.traits.contains(full));
            if !prim && outer.is_none() {
                continue;
            }
            let uses = bare_uses(it, &a);
            let outer_help = match &outer {
                // Own items from the root; library and dependency items
                // by their path.
                Some(full) if package_of(&module_of(full)).is_empty() => {
                    format!(" or `pkg::{full}` for the outer one")
                }
                Some(full) => format!(" or `{full}` for the outer one"),
                None => String::new(),
            };
            for at in &uses {
                diags.push(
                    nabic_diagnostic::NabiDiagnostic::error(
                        Code::AMBIGUOUS_ITEM,
                        *at,
                        format!(
                            "`{a}` is ambiguous here: the associated type `{a}` shadows the \
                             outer type `{a}` inside the {owner}"
                        ),
                    )
                    .with_help(format!(
                        "write `Self::{a}` for the associated type{outer_help}"
                    )),
                );
            }
            if uses.is_empty() {
                diags.push(nabic_diagnostic::NabiDiagnostic::warning(
                    Code::AMBIGUOUS_ITEM,
                    it.span(),
                    format!(
                        "associated type `{a}` shadows the outer type `{a}` inside the {owner}; \
                         there a bare `{a}` is ambiguous"
                    ),
                )
                .with_help(format!(
                    "write `Self::{a}` for the associated type{outer_help}"
                ))
                .with_lint("assoc_type_shadowing"));
            }
        }
    }
}

pub(crate) fn subst_assoc_sig(def: &mut FnDef, assoc: &HashMap<String, Ty>) {
    if assoc.is_empty() {
        return;
    }
    for p in &mut def.params {
        if let Some(t) = &mut p.ty {
            subst_assoc_ty(t, assoc);
        }
    }
    if let Some(t) = &mut def.ret_ty {
        subst_assoc_ty(t, assoc);
    }
    if let nabic_ast::ThrowsKind::Typed(ts) = &mut def.throws {
        ts.iter_mut().for_each(|t| subst_assoc_ty(t, assoc));
    }
    // Bounds and defaults too: `fn chain<J: IntoIterator<Item = Item>>`,
    // `fn collect<C = List<Item>>`.
    for gp in &mut def.generics.params {
        if let nabic_ast::GenericParam::Type {
            bounds, default, ..
        } = gp
        {
            bounds.iter_mut().for_each(|b| subst_assoc_ty(b, assoc));
            if let Some(d) = default {
                subst_assoc_ty(d, assoc);
            }
        }
    }
    // `where Item: Ord` names the bounded type through `assoc` as well.
    for w in &mut def.where_clause {
        subst_assoc_ty(&mut w.ty, assoc);
        w.bounds.iter_mut().for_each(|b| subst_assoc_ty(b, assoc));
    }
}

/// Replace `X` and `Self::X` by `assoc[X]` (associated types, generic
/// alias parameters).
pub(crate) fn subst_assoc_ty(ty: &mut Ty, assoc: &HashMap<String, Ty>) {
    match &mut ty.kind {
        AstTyKind::Path(p) => {
            let name = match p.segments.as_slice() {
                [s] if s.generics.is_empty() => Some(s.name.clone()),
                [s, x] if s.name == "Self" && x.generics.is_empty() => Some(x.name.clone()),
                _ => None,
            };
            if let Some(t) = name.and_then(|n| assoc.get(&n)) {
                *ty = t.clone();
                return;
            }
            // `Item::IntoIter` -> `List<T>::IntoIter` (a projection on
            // the impl's type).
            if let [s, x] = p.segments.as_slice() {
                if let (true, Some(AstTyKind::Path(ap))) = (
                    s.generics.is_empty() && x.generics.is_empty(),
                    assoc.get(&s.name).map(|t| &t.kind),
                ) {
                    let mut segs = ap.segments.clone();
                    segs.push(x.clone());
                    p.segments = segs;
                    return;
                }
            }
            for s in &mut p.segments {
                for g in &mut s.generics {
                    subst_assoc_ty(g, assoc);
                }
            }
        }
        AstTyKind::Sum(ps) => {
            for t in ps.iter_mut() {
                // A `Nest` (`X?`) is already one member.
                let before = matches!(&t.kind, AstTyKind::Path(p) if p.segments.len() <= 2)
                    .then(|| format!("{:?}", t.kind));
                subst_assoc_ty(t, assoc);
                // A replaced parameter stays one member.
                if before.is_some_and(|b| b != format!("{:?}", t.kind)) {
                    let span = t.span;
                    let inner = std::mem::replace(
                        t,
                        Ty {
                            kind: AstTyKind::Never,
                            span,
                        },
                    );
                    t.kind = AstTyKind::Nest(Box::new(inner));
                }
            }
        }
        AstTyKind::Nest(x) | AstTyKind::Binding(_, x) => subst_assoc_ty(x, assoc),
        AstTyKind::QPath(q) => {
            subst_assoc_ty(&mut q.self_ty, assoc);
            for s in &mut q.trait_path.segments {
                s.generics.iter_mut().for_each(|g| subst_assoc_ty(g, assoc));
            }
        }
        AstTyKind::Tuple(ps) => ps.iter_mut().for_each(|t| subst_assoc_ty(t, assoc)),
        AstTyKind::Array(e, len) => {
            subst_assoc_ty(e, assoc);
            // `[T; D]` with const parameter `D` given as `3`.
            let name = match len {
                nabic_ast::ConstExpr::Path(p) => {
                    p.segments.first().filter(|_| p.segments.len() == 1)
                }
                nabic_ast::ConstExpr::Expr(e) => match &e.kind {
                    ExprKind::Path(p) if p.segments.len() == 1 => p.segments.first(),
                    _ => None,
                },
                nabic_ast::ConstExpr::Literal(_) => None,
            };
            if let Some(AstTyKind::Const(c)) =
                name.and_then(|s| assoc.get(&s.name)).map(|t| &t.kind)
            {
                *len = c.clone();
            }
        }
        AstTyKind::Fn(args, ret, _) => {
            args.iter_mut().for_each(|t| subst_assoc_ty(t, assoc));
            subst_assoc_ty(ret, assoc);
        }
        AstTyKind::Throws { ok, err } => {
            subst_assoc_ty(ok, assoc);
            subst_assoc_ty(err, assoc);
        }
        _ => {}
    }
}
