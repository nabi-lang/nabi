//! Method calls on `any` and their dispatchers.

#[allow(clippy::wildcard_imports)]
use crate::lower::expr::*;

/// `v.m(args)` on `any Tr`: only the bound traits' methods; the call
/// switches on the tag over every implementor in the program. A method
/// with type parameters of its own gets one dispatcher per instantiation,
/// fixed at the call (`type_args`, else the arguments' types).
#[allow(clippy::too_many_arguments)]
pub(crate) fn any_method_call(
    ctx: &mut LowerCtx<'_>,
    recv_h: HirExpr,
    bounds: &[TraitRef],
    method: &str,
    only: Option<&str>,
    type_args: &[Ty],
    args: &[CallArg],
    span: nabic_diagnostic::Span,
) -> HirExpr {
    let mut seen: Vec<String> = Vec::new();
    let mut stack = bounds.to_vec();
    // The trait declaring `method`, and the bound that named it (with
    // its arguments / bindings; a supertrait reached indirectly has none).
    let mut provider: Option<(String, TraitRef)> = None;
    while let Some(t) = stack.pop() {
        let base = t.path.clone();
        if seen.contains(&base) {
            continue;
        }
        if let Some(info) = ctx.env.trait_of(&base) {
            if provider.is_none()
                && only.is_none_or(|k| k == base)
                && info.methods.iter().any(|m| m == method)
            {
                provider = Some((base.clone(), t.clone()));
            }
            stack.extend(info.supers.iter().map(|s| TraitRef::plain(s)));
        }
        seen.push(base);
    }
    let recv_ty = recv_h.ty.clone();
    let Some((provider, bound)) = provider else {
        if let Ok(r) = crate::lower::reflect::method_call(ctx, recv_h, method, args) {
            return r;
        }
        discard_args(ctx, args);
        return ctx.error_expr(
            Code::NO_METHOD,
            span,
            format!(
                "no method `{method}` on `{}` - only methods of its trait bound(s) and their \
                 supertraits can be called on an `any` value; narrow it with a type pattern \
                 (`match v {{ x: T => ... }}`) to reach other methods",
                ctx.ty_name(&recv_ty)
            ),
        );
    };
    if method == "fmt" && is_fmt_trait(ctx, &provider) {
        return any_fmt(ctx, recv_h, &provider, args, span);
    }
    // Implementors share one signature only if it doesn't mention `Self`,
    // trait arguments the bound leaves open, or unbound associated types.
    let has_args = !bound.args.is_empty();
    let pinfo = ctx
        .env
        .trait_of(&provider)
        .expect("the provider is a trait");
    let needs = pinfo.needs.get(method).cloned().unwrap_or_default();
    let no_self = pinfo
        .decls
        .get(method)
        .is_some_and(|d| d.params.first().is_none_or(|p| p.name != "self"));
    let why = no_self.then(|| {
        "it takes no `self`, so there is no value to dispatch on (not object-safe)".to_owned()
    });
    let why = why.or_else(|| {
        needs.iter().find_map(|n| {
            if n == "Self" {
                Some(
                    "its signature mentions `Self`, which is a different type for each implementor"
                        .to_owned(),
                )
            } else if let Some(x) = n.strip_prefix("Self::") {
                (!bound.binds.iter().any(|(n, _)| n == x)).then(|| {
                    format!(
                        "its signature uses `Self::{x}`, which the bound leaves open - bind it \
                     (`any {provider}<{x} = ..>`)"
                    )
                })
            } else {
                (!has_args).then(|| {
                    format!(
                        "its signature uses type parameters of `{provider}` that the bound leaves \
                     open - name them (`any {provider}<..>`)"
                    )
                })
            }
        })
    });
    if let Some(why) = why {
        discard_args(ctx, args);
        return ctx.error_expr(
            Code::NO_METHOD,
            span,
            format!(
                "cannot call `{method}` on `{}`: {why}; narrow the value with `is` or a type \
                 pattern first",
                ctx.ty_name(&recv_ty)
            ),
        );
    }
    let decl = pinfo.decls.get(method);
    let own: Vec<String> = decl.map(own_type_params).unwrap_or_default();
    if own.is_empty() && !type_args.is_empty() {
        discard_args(ctx, args);
        return ctx.error_expr(
            Code::GENERIC_ARGUMENTS,
            span,
            crate::lower::mono::arity_msg(method, 0, 0, type_args.len()),
        );
    }
    let mut method_subst = HashMap::default();
    if let (Some(def), false) = (decl, own.is_empty()) {
        match any_method_generics(ctx, &provider, &bound, def, &own, type_args, args, &recv_ty) {
            // Each instantiation dispatches on its own: open literals take their
            // defaults here.
            Ok(s) => {
                method_subst = s;
                for t in method_subst.values_mut() {
                    *t = crate::lower::infer::fix_literals(ctx, t);
                }
            }
            Err(msg) => {
                discard_args(ctx, args);
                return match msg {
                    Some(msg) => ctx.error_expr(Code::GENERIC_ARGUMENTS, span, msg),
                    None => poison(span),
                };
            }
        }
    }
    // The signature, the same for every implementor (checked above):
    // a concrete impl's, else the trait declaration's (the only one for
    // a generic method).
    let concrete = ctx
        .env
        .impls
        .tys_of(&bounds[0].path)
        .filter(|_| own.is_empty())
        .find(|t| {
            bounds
                .iter()
                .all(|b| ctx.env.impls.refs_of(t, &b.path).any(|r| r.satisfies(b)))
        })
        .and_then(|t| match pick_method(ctx, t, method, Some(&provider)) {
            Ok(Some(MethodPick::Concrete(id))) => Some(ctx.sig_by_id(id)),
            _ => None,
        })
        .map(|sig| (sig.param_tys[1..].to_vec(), sig.ret_ty.clone()));
    let Some((params, ret)) =
        concrete.or_else(|| trait_decl_sig(ctx, &provider, &bound, method, &method_subst))
    else {
        discard_args(ctx, args);
        return ctx.error_expr(
            Code::NO_METHOD,
            span,
            format!(
                "cannot call `{method}` on `{}`: its signature can't be fixed from the bound \
                 `{}`",
                ctx.ty_name(&recv_ty),
                ctx.trait_name(&bound)
            ),
        );
    };
    let mut param_tys = vec![recv_ty.clone()];
    param_tys.extend(params);
    let sig = FnSig {
        id: FunctionId(u32::MAX),
        ret_ty: ret.clone(),
        param_defaults: vec![None; param_tys.len()],
        // The declaration's names, for named arguments.
        param_names: (0..param_tys.len())
            .map(|i| match decl.and_then(|d| d.params.get(i)) {
                Some(p) if i > 0 && decl.is_some_and(|d| d.params.len() == param_tys.len()) => {
                    p.name.clone()
                }
                _ => format!("#{i}"),
            })
            .collect(),
        param_refs: Vec::new(),
        param_tys,
        variadic: false,
    };
    let (lowered, lets) = lower_args_for(ctx, &sig, method, Some(recv_h), args, span);
    let id = any_dispatcher(ctx, &recv_ty, &provider, method, &sig, span);
    with_lets(
        lets,
        HirExpr {
            kind: HirExprKind::Call {
                callee: id,
                args: lowered,
            },
            ty: ret,
            span,
        },
    )
}

pub(crate) fn is_fmt_trait(ctx: &LowerCtx<'_>, key: &str) -> bool {
    key == ctx.lang("display_trait") || key == ctx.lang("debug_trait")
}

/// `Display::fmt` / `Debug::fmt` on `any`: the dispatcher renders the text
/// (`build_dispatcher`), then it is written to `f`.
pub(crate) fn any_fmt(
    ctx: &mut LowerCtx<'_>,
    recv_h: HirExpr,
    provider: &str,
    args: &[CallArg],
    span: nabic_diagnostic::Span,
) -> HirExpr {
    let str_ty = HirTy::Prim(PrimTy::Str);
    let sig = FnSig {
        id: FunctionId(u32::MAX),
        ret_ty: str_ty.clone(),
        param_defaults: vec![None],
        param_names: vec!["#0".to_owned()],
        param_refs: Vec::new(),
        param_tys: vec![recv_h.ty.clone()],
        variadic: false,
    };
    let id = any_dispatcher(ctx, &recv_h.ty, provider, "fmt", &sig, span);
    let text = HirExpr {
        kind: HirExprKind::Call {
            callee: id,
            args: vec![recv_h],
        },
        ty: str_ty.clone(),
        span,
    };
    ctx.push_scope();
    let name = format!("#t{}", ctx.locals.len());
    let local = ctx.declare(Some(name.clone()), str_ty);
    let path = |names: &[&str]| Expr {
        kind: ExprKind::Path(AstPath {
            segments: names
                .iter()
                .map(|n| nabic_ast::PathSegment {
                    name: (*n).to_owned(),
                    generics: Vec::new(),
                })
                .collect(),
            span,
            origin: None,
        }),
        span,
    };
    let mut call_args = args.to_vec();
    call_args.push(CallArg::Positional(path(&[&name])));
    let write = Expr {
        kind: ExprKind::Call {
            callee: Box::new(path(&["", "core", "fmt", "Formatter", "write_str"])),
            args: call_args,
            trailing_closure: None,
        },
        span,
    };
    let write = lower_expr(ctx, &write);
    ctx.pop_scope();
    with_lets(vec![HirStmt::Let { local, init: text }], write)
}

/// `method`'s signature from `provider`'s declaration with the trait's
/// parameters and associated types from `bound`, the method's own type
/// parameters from `own`; `None` for too many arguments.
pub(crate) fn trait_decl_sig(
    ctx: &LowerCtx<'_>,
    provider: &str,
    bound: &TraitRef,
    method: &str,
    own: &HashMap<String, HirTy>,
) -> Option<(Vec<HirTy>, HirTy)> {
    let info = ctx.env.trait_of(provider)?;
    let def = info.decls.get(method)?;
    let mut subst = trait_subst(info, bound)?;
    subst.extend(own.iter().map(|(k, v)| (k.clone(), v.clone())));
    let resolve = |t: &Ty| trait_decl_ty(ctx, info, &subst, t);
    let params = def
        .params
        .iter()
        .filter(|p| p.name != "self")
        .map(|p| p.ty.as_ref().map_or(HirTy::Unknown, resolve))
        .collect();
    Some((params, resolve(&sig_ret_ast(def))))
}

/// The trait's parameters and associated types as `bound` names them;
/// `None` for too many arguments.
pub(crate) fn trait_subst(info: &TraitInfo, bound: &TraitRef) -> Option<HashMap<String, HirTy>> {
    (bound.args.len() <= info.params.len()).then(|| {
        info.params
            .iter()
            .cloned()
            .zip(bound.args.iter().cloned())
            .chain(bound.binds.iter().cloned())
            .collect()
    })
}

/// Type `t` of a trait method declaration, resolved in the trait's
/// module under `subst`.
pub(crate) fn trait_decl_ty(
    ctx: &LowerCtx<'_>,
    info: &TraitInfo,
    subst: &HashMap<String, HirTy>,
    t: &Ty,
) -> HirTy {
    let mut rc = LowerCtx::new(ctx.env, subst, None, ctx.here());
    rc.current_module.clone_from(&info.module);
    let mut t = t.clone();
    strip_self_assoc(&mut t);
    rc.resolve_ast_ty(&t)
}

/// The type parameters a method declares itself.
pub(crate) fn own_type_params(def: &FnDef) -> Vec<String> {
    def.generics
        .params
        .iter()
        .filter_map(|gp| match gp {
            nabic_ast::GenericParam::Type { name, .. } => Some(name.clone()),
            nabic_ast::GenericParam::Const { .. } => None,
        })
        .collect()
}

/// A generic trait method called on `any`: its own type parameters, from
/// `::<..>` first, the rest from the arguments' types (a probe lowering,
/// rolled back before the real one) or the expected type, checked
/// against their bounds; `Err(None)` after reporting.
#[allow(clippy::too_many_arguments)]
pub(crate) fn any_method_generics(
    ctx: &mut LowerCtx<'_>,
    provider: &str,
    bound: &TraitRef,
    def: &FnDef,
    own: &[String],
    type_args: &[Ty],
    args: &[CallArg],
    recv: &HirTy,
) -> Result<HashMap<String, HirTy>, Option<String>> {
    let method = leaf_name(&def.name);
    let Some(info) = ctx.env.trait_of(provider) else {
        return Err(Some(format!("`{provider}` is not a trait")));
    };
    if type_args.len() > own.len() {
        return Err(Some(crate::lower::mono::arity_msg(
            method,
            own.len(),
            own.len(),
            type_args.len(),
        )));
    }
    let expected = ctx.current_expected().cloned();
    let mut subst: HashMap<String, HirTy> = own
        .iter()
        .cloned()
        .zip(type_args.iter().map(|t| ctx.resolve_ast_ty(t)))
        .collect();
    let outer = trait_subst(info, bound).unwrap_or_default();
    let names: nabic_core::HashSet<String> = own.iter().cloned().collect();
    let params: Vec<&nabic_ast::Param> = def.params.iter().filter(|p| p.name != "self").collect();
    let mark = ctx.probe_mark();
    let mut next = 0;
    for a in args {
        let (i, e) = match a {
            CallArg::Positional(e) | CallArg::Ref(e) => {
                next += 1;
                (Some(next - 1), e)
            }
            CallArg::Named { name, value } => {
                let Some(i) = params.iter().position(|p| p.name == *name) else {
                    ctx.rollback(&mark);
                    return Err(Some(format!("`{method}` has no parameter named `{name}`")));
                };
                (Some(i), value)
            }
            CallArg::Spread(_) => continue,
        };
        let Some(t) = i.and_then(|i| params.get(i)).and_then(|p| p.ty.as_ref()) else {
            continue;
        };
        // A slot naming a parameter not bound yet takes the argument's own type.
        let open = own
            .iter()
            .any(|n| !subst.contains_key(n) && ast_ty_mentions(t, n));
        let hint = if open {
            HirTy::Unknown
        } else {
            let mut all = outer.clone();
            all.extend(subst.iter().map(|(k, v)| (k.clone(), v.clone())));
            trait_decl_ty(ctx, info, &all, t)
        };
        let h = lower_expected(ctx, e, hint);
        if h.ty != HirTy::Unknown {
            crate::lower::mono::unify_param_ty_with(t, &h.ty, &names, &mut subst, ctx);
        }
    }
    ctx.rollback(&mark);
    // `let x: T = v.m()`: a parameter only the return type names.
    if let Some(want) = expected.filter(|t| !matches!(t, HirTy::Unknown | HirTy::Unit)) {
        crate::lower::mono::unify_param_ty_with(&sig_ret_ast(def), &want, &names, &mut subst, ctx);
    }
    // The rest take their defaults (`C = List<Item>`).
    for gp in &def.generics.params {
        if let nabic_ast::GenericParam::Type {
            name,
            default: Some(d),
            ..
        } = gp
        {
            if names.contains(name) && !subst.contains_key(name) {
                let mut all = outer.clone();
                all.extend(subst.iter().map(|(k, v)| (k.clone(), v.clone())));
                let t = trait_decl_ty(ctx, info, &all, d);
                subst.insert(name.clone(), t);
            }
        }
    }
    if let Some(n) = own.iter().find(|n| !subst.contains_key(*n)) {
        return Err(Some(format!(
            "cannot call `{method}` on `{}`: its type parameter `{n}` can't be inferred from \
             the arguments or the expected type; name it: `.{method}::<{}>(..)`",
            ctx.ty_name(recv),
            own.join(", ")
        )));
    }
    let mut all = outer;
    all.extend(subst.iter().map(|(k, v)| (k.clone(), v.clone())));
    let mark = ctx.probe_mark();
    crate::lower::mono::check_call_bounds(ctx, def, &all, None, &info.module);
    if !ctx.probe_clean(&mark) {
        return Err(None);
    }
    Ok(subst)
}

/// `Self::Item` -> `Item` (the associated type, bound by name).
pub(crate) fn strip_self_assoc(ty: &mut Ty) {
    match &mut ty.kind {
        AstTyKind::Path(p) => {
            if p.segments.len() >= 2 && p.segments[0].name == "Self" {
                p.segments.remove(0);
            }
            for s in &mut p.segments {
                s.generics.iter_mut().for_each(strip_self_assoc);
            }
        }
        AstTyKind::Sum(parts) | AstTyKind::Tuple(parts) => {
            parts.iter_mut().for_each(strip_self_assoc);
        }
        AstTyKind::Fn(args, ret, _) => {
            args.iter_mut().for_each(strip_self_assoc);
            strip_self_assoc(ret);
        }
        AstTyKind::Throws { ok, err } => {
            strip_self_assoc(ok);
            strip_self_assoc(err);
        }
        AstTyKind::Array(elem, _) | AstTyKind::Nest(elem) | AstTyKind::Binding(_, elem) => {
            strip_self_assoc(elem);
        }
        AstTyKind::QPath(q) => strip_self_assoc(&mut q.self_ty),
        _ => {}
    }
}

/// One dispatcher per (`any` type, method); bodies are built by
/// `build_any_dispatchers` once all stored types are known (closed world).
#[derive(Clone)]
pub(crate) struct AnyDispatch {
    pub(crate) id: FunctionId,
    pub(crate) recv_ty: HirTy,
    /// The trait declaring `method` (each implementor's method of it,
    /// whatever other traits share the name).
    pub(crate) trait_key: String,
    pub(crate) method: String,
    pub(crate) params: Vec<HirTy>,
    pub(crate) ret: HirTy,
    pub(crate) span: nabic_diagnostic::Span,
}

impl AnyDispatch {
    /// The dispatcher of `method` on `recv_ty` with parameter and return types
    /// `shape`.
    pub(crate) fn is(
        &self,
        recv_ty: &HirTy,
        trait_key: &str,
        method: &str,
        shape: &[HirTy],
    ) -> bool {
        self.recv_ty == *recv_ty
            && self.method == method
            && self.trait_key == trait_key
            && shape
                .split_last()
                .is_some_and(|(ret, params)| *ret == self.ret && params == self.params.as_slice())
    }
}

pub(crate) fn any_dispatcher(
    ctx: &LowerCtx<'_>,
    recv_ty: &HirTy,
    trait_key: &str,
    method: &str,
    sig: &FnSig,
    span: nabic_diagnostic::Span,
) -> FunctionId {
    let mut all = ctx.env.sink.any_dispatch.borrow_mut();
    // A generic method has one per instantiation: its signature is part
    // of the key.
    let mut shape = sig.param_tys[1..].to_vec();
    shape.push(sig.ret_ty.clone());
    let found = all
        .iter()
        .find(|d| d.is(recv_ty, trait_key, method, &shape))
        .map(|d| d.id);
    crate::lower::incr::log(
        || format!("d|{recv_ty:?}|{trait_key}|{method}|{shape:?}"),
        || {
            crate::lower::incr::Lookup::AnyDispatch(
                recv_ty.clone(),
                trait_key.to_owned(),
                method.to_owned(),
                shape.clone(),
                found,
            )
        },
    );
    if let Some(id) = found {
        return id;
    }
    let id = FunctionId(ctx.env.sink.next_id.get());
    ctx.env.sink.next_id.set(id.0 + 1);
    all.push(AnyDispatch {
        id,
        recv_ty: recv_ty.clone(),
        trait_key: trait_key.to_owned(),
        method: method.to_owned(),
        params: sig.param_tys[1..].to_vec(),
        ret: sig.ret_ty.clone(),
        span,
    });
    id
}

/// `any` across packages. Dispatchers are closed-world, so each gets a
/// stable symbol `nabi.any.<hash>` (weak in a prebuilt library, strong in
/// its user), and a library ships its stored types and dispatchers as Nabi
/// source `seeds` in its NABIHIR for the user to compile into its own.
/// Returns `(seeds, dispatcher exports)`.
// Seeds name types by full path (`ty_path`), rooted as the library's
// code roots them: they are compiled in a module of the library that need
// not import them and sees only the library's dependencies. Closures are skipped;
// symbol keys use `ty_name`, as the user's own dispatchers do.
#[allow(clippy::type_complexity)] // two lists, each read once
pub(crate) fn any_link(
    env: &Env<'_>,
    lib: Option<&str>,
) -> (Vec<(String, String)>, Vec<(FunctionId, String)>) {
    let empty = HashMap::default();
    let c = LowerCtx::new(env, &empty, None, nabic_diagnostic::Span::DUMMY);
    let dispatch = env.sink.any_dispatch.borrow().clone();
    let names = |tys: &[HirTy]| tys.iter().map(|t| c.ty_name(t)).collect::<Vec<_>>();
    let exports = dispatch
        .iter()
        .map(|d| {
            let key = format!(
                "{}|{}|{}|{}|{}",
                c.ty_name(&d.recv_ty),
                d.trait_key,
                d.method,
                names(&d.params).join(","),
                c.ty_name(&d.ret)
            );
            (
                d.id,
                format!("nabi.any.{:016x}", nabic_core::hash::fnv(key.as_bytes())),
            )
        })
        .collect();
    let Some(lib) = lib else {
        return (Vec::new(), exports);
    };
    let under = format!("{lib}::");
    // The module of the first `lib` item a type names (else the root).
    let home = |text: &str| -> String {
        text.split(|ch: char| !(ch.is_alphanumeric() || ch == '_' || ch == ':'))
            .find(|w| w.starts_with(&under))
            .map_or_else(|| lib.to_owned(), module_of)
    };
    // `text` as `lib`'s code names it, where the seed compiles: its own
    // items from `pkg::`, another package's by its public path through
    // the name `lib` gives it. `None` when it names a package `lib` does
    // not depend on (that package's own seeds carry it).
    let public = |text: &str| -> Option<String> {
        let mut out = String::with_capacity(text.len());
        let mut word = String::new();
        let mut ok = true;
        let mut flush = |w: &mut String, out: &mut String| {
            if let Some(rest) = w.strip_prefix(&under) {
                out.push_str("pkg::");
                out.push_str(rest);
            } else if w.contains("::") && !nabic_core::in_toolchain(w) {
                let full = env.names.visible_path(w);
                let (top, rest) = full.split_once("::").unwrap_or((full, ""));
                let root = if top == lib {
                    Some("pkg".to_owned())
                } else {
                    crate::lower::dependency_name(lib, top).map(|n| format!("::{n}"))
                };
                match root {
                    Some(r) => {
                        out.push_str(&r);
                        out.push_str("::");
                        out.push_str(rest);
                    }
                    None => ok = false,
                }
            } else {
                out.push_str(w);
            }
            w.clear();
        };
        for ch in text.chars() {
            if ch.is_alphanumeric() || ch == '_' || ch == ':' {
                word.push(ch);
            } else {
                flush(&mut word, &mut out);
                out.push(ch);
            }
        }
        flush(&mut word, &mut out);
        ok.then_some(out)
    };
    let nameable = |s: &str| {
        !["closure", "{unknown}", "Self", "#", "GcBuf"]
            .iter()
            .any(|b| s.contains(b))
    };
    let mut seeds = Vec::new();
    let stored: Vec<HirTy> = env
        .sink
        .any_tags
        .borrow()
        .iter()
        .map(|(t, _)| t.clone())
        .collect();
    for t in stored {
        let path = c.ty_path(&t);
        if matches!(t, HirTy::AnyTrait(_)) || !nameable(&path) {
            continue;
        }
        let Some(name) = public(&path) else {
            continue;
        };
        let n = seeds.len();
        seeds.push((
            home(&path),
            format!(
                "#[export_name(\"__nabi_seed_{lib}_{n}\")]\nfn __nabi_seed_{n}(x: {name}) -> any {{ return x; }}\n"
            ),
        ));
    }
    for d in &dispatch {
        let recv_path = c.ty_path(&d.recv_ty);
        let param_paths: Vec<String> = d.params.iter().map(|t| c.ty_path(t)).collect();
        if !nameable(&recv_path) || !param_paths.iter().all(|p| nameable(p)) {
            continue;
        }
        let Some(recv) = public(&recv_path) else {
            continue;
        };
        let Some(params) = param_paths
            .iter()
            .map(|p| public(p))
            .collect::<Option<Vec<String>>>()
        else {
            continue;
        };
        let n = seeds.len();
        let decl: Vec<String> = params
            .iter()
            .enumerate()
            .map(|(i, p)| format!(", a{i}: {p}"))
            .collect();
        let args: Vec<String> = (0..params.len()).map(|i| format!("a{i}")).collect();
        let (throws, try_) = if matches!(d.ret, HirTy::Throws { .. }) {
            (" throws", "try ")
        } else {
            ("", "")
        };
        // A `fmt` dispatcher renders text (`any_fmt`); an f-string makes it.
        let body = if d.method == "fmt" && is_fmt_trait(&c, &d.trait_key) {
            let q = if d.trait_key == c.lang("debug_trait") {
                ":?"
            } else {
                ""
            };
            format!("let _ = f\"{{x{q}}}\";")
        } else {
            format!("{try_}x.{}({});", d.method, args.join(", "))
        };
        seeds.push((
            home(&recv_path),
            format!(
                "#[export_name(\"__nabi_seed_{lib}_{n}\")]\nfn __nabi_seed_{n}(x: {recv}{}){throws} {{ {body} }}\n",
                decl.concat(),
            ),
        ));
    }
    (seeds, exports)
}

/// Every type the program stores into an `any` slot that source can
/// name (`HirModule::any_types`), by full path (`ty_path`).
pub(crate) fn any_types(env: &Env<'_>) -> Vec<String> {
    let empty = HashMap::default();
    let c = LowerCtx::new(env, &empty, None, nabic_diagnostic::Span::DUMMY);
    let stored: Vec<HirTy> = env
        .sink
        .any_tags
        .borrow()
        .iter()
        .map(|(t, _)| t.clone())
        .collect();
    stored
        .iter()
        .filter(|t| !matches!(t, HirTy::AnyTrait(_)))
        .map(|t| c.ty_path(t))
        .filter(|s| {
            !["closure", "{unknown}", "Self", "#", "GcBuf"]
                .iter()
                .any(|b| s.contains(b))
        })
        .collect()
}

/// Build every dispatcher body. Bodies can instantiate code that stores
/// new types or needs new dispatchers, so repeat to a fixpoint.
pub(crate) fn build_any_dispatchers(env: &Env<'_>) {
    loop {
        let (n_tags, todo) = (
            env.sink.any_tags.borrow().len(),
            env.sink.any_dispatch.borrow().clone(),
        );
        let fns: Vec<HirFunction> = todo.iter().map(|d| build_dispatcher(env, d)).collect();
        if env.sink.any_tags.borrow().len() == n_tags
            && env.sink.any_dispatch.borrow().len() == todo.len()
        {
            env.sink.extras.borrow_mut().extend(fns);
            return;
        }
    }
}

pub(crate) fn build_dispatcher(env: &Env<'_>, d: &AnyDispatch) -> HirFunction {
    let span = d.span;
    let empty = HashMap::default();
    let mut c = LowerCtx::new(env, &empty, None, span);
    let HirTy::AnyTrait(bounds) = &d.recv_ty else {
        unreachable!("dispatcher on a non-`any` type")
    };
    let mut params = vec![HirParam {
        name: "#recv".to_owned(),
        local: c.declare(Some("#recv".to_owned()), d.recv_ty.clone()),
        ty: d.recv_ty.clone(),
        by_ref: false,
    }];
    for (i, t) in d.params.iter().enumerate() {
        let name = format!("#a{i}");
        params.push(HirParam {
            name: name.clone(),
            local: c.declare(Some(name), t.clone()),
            ty: t.clone(),
            by_ref: false,
        });
    }
    let args: Vec<CallArg> = (0..d.params.len())
        .map(|i| {
            CallArg::Positional(Expr {
                kind: ExprKind::Path(AstPath {
                    segments: vec![nabic_ast::PathSegment {
                        name: format!("#a{i}"),
                        generics: Vec::new(),
                    }],
                    span,
                    origin: None,
                }),
                span,
            })
        })
        .collect();
    let stored: Vec<HirTy> = env
        .sink
        .any_tags
        .borrow()
        .iter()
        .map(|(t, _)| t.clone())
        .collect();
    let mut arms = Vec::new();
    for t in stored {
        if matches!(t, HirTy::AnyTrait(_)) || !bounds.iter().all(|b| c.implements_trait(&t, b, 0)) {
            continue;
        }
        c.push_scope();
        let x = c.declare(Some("#x".to_owned()), t.clone());
        let recv = hir_local(x, &t, span);
        let call = if d.method == "fmt" && is_fmt_trait(&c, &d.trait_key) {
            // The text, `f"{x}"` / `f"{x:?}"` (`any_fmt`).
            let debug = d.trait_key == c.lang("debug_trait");
            let hole = nabic_ast::FStringPart::Hole {
                expr: Expr {
                    kind: ExprKind::Path(AstPath {
                        segments: vec![nabic_ast::PathSegment {
                            name: "#x".to_owned(),
                            generics: Vec::new(),
                        }],
                        span,
                        origin: None,
                    }),
                    span,
                },
                spec: if debug {
                    vec![nabic_ast::SpecPiece::Text("?".to_owned())]
                } else {
                    Vec::new()
                },
            };
            crate::lower::fstring::lower_fstring(&mut c, &[hole], span)
        } else if d.trait_key == crate::lower::reflect::REFLECT {
            crate::lower::reflect::dynamic(&mut c, recv, &d.method)
        } else {
            // The return type fixes a type parameter only it names.
            c.push_expected(d.ret.clone());
            let call = call_impl_method(
                &mut c,
                &t,
                &d.method,
                Some(&d.trait_key),
                Some(recv),
                &args,
                &d.method,
                span,
            );
            c.pop_expected();
            call.unwrap_or_else(|| poison(span))
        };
        let body = coerce(&mut c, &d.ret, call, &|| {
            format!("`{}` of an `any` value", d.method)
        });
        c.pop_scope();
        arms.push(HirMatchArm {
            pat: HirPattern::SumVariant {
                sum_ty: d.recv_ty.clone(),
                variant_index: c.any_tag(&t),
                binder: Some(x),
                payload_ty: Some(t),
            },
            guard: None,
            body,
        });
    }
    let m = HirExpr {
        kind: HirExprKind::Match {
            scrutinee: Box::new(hir_local(params[0].local, &d.recv_ty, span)),
            arms,
        },
        ty: d.ret.clone(),
        span,
    };
    let body = if d.ret == HirTy::Unit {
        HirBlock {
            stmts: vec![HirStmt::Expr(m)],
            trailing: None,
            label: None,
        }
    } else {
        HirBlock {
            stmts: Vec::new(),
            trailing: Some(m),
            label: None,
        }
    };
    HirFunction {
        id: d.id,
        def_id: None,
        name: format!("__any_{}_{}", d.method, d.id.0),
        params,
        locals: c.into_locals(),
        ret_ty: d.ret.clone(),
        body: Some(body),
        is_async: false,
        is_generator: false,
        span,
        facts: nabic_ir_common::FnFacts {
            role: nabic_ir_common::FnRole::AnyDispatcher,
            ..nabic_ir_common::FnFacts::default()
        },
    }
}
