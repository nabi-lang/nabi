//! Opaque `-> some Tr` returns and named `some` types.

#[allow(clippy::wildcard_imports)]
use crate::lower::mono::*;

/// The bounds of a `-> some ..` return type.
pub(crate) fn opaque_bounds(f: &FnDef) -> Option<&[Ty]> {
    match &f.ret_ty.as_ref()?.kind {
        AstTyKind::Some(bounds) => Some(bounds),
        _ => None,
    }
}

/// A call of `-> some Tr` fn `id` while a body is checked for what it
/// takes from it: an opaque parameter that has only `Tr`. Otherwise
/// `None` (the concrete type), noting the call.
pub(crate) fn opaque_ret(ctx: &LowerCtx<'_>, id: FunctionId) -> Option<HirTy> {
    let env = ctx.env;
    let decl = env.decl(id)?;
    let f = decl.def();
    let bounds = opaque_bounds(f)?;
    if let Some(x) = defines_some(env, &f.name) {
        let t = some_type_ty(ctx, x);
        return matches!(t, HirTy::Param(_)).then_some(t);
    }
    // Generic ones (the bound may name a parameter) and function types
    // (only callable anyway) stay transparent.
    let plain = f.generics.params.is_empty()
        && matches!(decl, MethodOrFn::Fn(_))
        && bounds.iter().all(|b| matches!(b.kind, AstTyKind::Path(_)));
    if !plain {
        return None;
    }
    if !env.sink.opaque_check.get() {
        env.sink.opaque_called.set(true);
        return None;
    }
    if let Some(p) = env.sink.opaque_params.borrow().get(&id) {
        return Some(p.clone());
    }
    let module = fn_module(env, f);
    let shown: Vec<String> = bounds.iter().map(nabic_ast::print::print_ty).collect();
    let name = match shown.as_slice() {
        [one] => format!("some {one}"),
        _ => format!("some ({})", shown.join(" + ")),
    };
    let p = bound_param(env, &module, bounds, name, f.span);
    env.sink.opaque_params.borrow_mut().insert(id, p.clone());
    Some(p)
}

/// An opaque type parameter `name` that has only `bounds` (written in
/// `module`).
pub(crate) fn bound_param(
    env: &Env<'_>,
    module: &str,
    bounds: &[Ty],
    name: String,
    at: Span,
) -> HirTy {
    let none = HashMap::default();
    let mut rcx = LowerCtx::new(env, &none, None, at);
    module.clone_into(&mut rcx.current_module);
    let refs: Vec<TraitRef> = bounds
        .iter()
        .filter_map(|b| rcx.bound_ref(b, module, |t| rcx.resolve_ast_ty(t)))
        .filter(|r| env.names.is_trait(&r.path))
        .collect();
    crate::lower::bounds::opaque_param(env, name, refs)
}

/// The named `some` type defining function `name` returns.
pub(crate) fn defines_some<'e>(env: &'e Env<'_>, name: &str) -> Option<&'e str> {
    env.names
        .some_types
        .iter()
        .find(|(_, st)| st.definers.iter().any(|d| d == name))
        .map(|(x, _)| x.as_str())
}

/// Whether a body of `module` sees named `some` type `x` as its concrete
/// type: only X's own module does.
pub(crate) fn sees_some(module: &str, x: &str) -> bool {
    module == module_of(x)
}

/// Named `some` type `x` where `ctx` writes it: its concrete type, or
/// while `opaque_check` looks at a body of another module, an opaque
/// parameter that has only its bounds.
pub(crate) fn some_type_ty(ctx: &LowerCtx<'_>, x: &str) -> HirTy {
    let env = ctx.env;
    let sink = &env.sink;
    // A function type is only callable anyway: it stays transparent.
    let callable = env.names.some_types[x]
        .bounds
        .iter()
        .any(|b| matches!(b.kind, AstTyKind::Fn(..)));
    if callable {
        return some_concrete(env, x);
    }
    if sink.opaque_check.get() {
        let view = sink.some_view.borrow().clone();
        if view.is_some_and(|v| !sees_some(&v, x)) {
            return some_param(env, x);
        }
    } else if !sees_some(&ctx.current_module, x) {
        sink.opaque_called.set(true);
    }
    some_concrete(env, x)
}

/// `x` as an opaque parameter, named as written.
pub(crate) fn some_param(env: &Env<'_>, x: &str) -> HirTy {
    if let Some(p) = env.sink.some_params.borrow().get(x) {
        return p.clone();
    }
    let st = &env.names.some_types[x];
    let p = bound_param(
        env,
        &module_of(x),
        &st.bounds,
        leaf_name(x).to_owned(),
        st.span,
    );
    env.sink
        .some_params
        .borrow_mut()
        .insert(x.to_owned(), p.clone());
    p
}

/// Named `some` type `x`'s module, as messages name it.
pub(crate) fn some_home(x: &str) -> String {
    match module_of(x) {
        m if m.is_empty() => "the root module".to_owned(),
        m => format!("`{m}`"),
    }
}

/// The named `some` type behind opaque parameter `p`, if it is one.
pub(crate) fn some_of_param(env: &Env<'_>, p: &HirTy) -> Option<String> {
    env.sink
        .some_params
        .borrow()
        .iter()
        .find(|(_, q)| *q == p)
        .map(|(x, _)| x.clone())
}

/// The concrete type of named `some` type `x`: its first defining
/// function's return type. Before the signatures are in, a placeholder
/// path, replaced once they are (`collect`).
pub(crate) fn some_concrete(env: &Env<'_>, x: &str) -> HirTy {
    if env.sink.sig_pass.get().is_some() {
        return HirTy::Path(vec![x.to_owned()]);
    }
    let Some(first) = env.names.some_types[x].definers.first() else {
        return HirTy::Unknown;
    };
    let Some(id) = env.sig(first).map(|s| s.id) else {
        return HirTy::Unknown;
    };
    // Through the inferred return type, so an incremental unit that
    // reads `x` depends on its definer.
    let t = decl_ret_ok(env, id);
    // Open while its definer is still being inferred.
    if !has_unknown(&t) {
        env.names
            .some_concrete
            .borrow_mut()
            .insert(x.to_owned(), t.clone());
    }
    t
}

/// Every named `some` type's concrete type, once the signatures are in;
/// defining functions that disagree are an error.
pub(crate) fn solve_some_types(env: &Env<'_>) {
    let mut xs: Vec<&String> = env.names.some_types.keys().collect();
    xs.sort();
    // The bodies are lowered for good in their own pass.
    let done = env.sink.bodies_done.replace(true);
    for x in xs {
        let st = &env.names.some_types[x];
        let Some((first, rest)) = st.definers.split_first() else {
            continue;
        };
        let t = some_concrete(env, x);
        for d in rest {
            let Some(sig) = env.sig(d) else { continue };
            let other = decl_ret_ok(env, sig.id);
            if other == t || has_unknown(&other) || has_unknown(&t) {
                continue;
            }
            let none = HashMap::default();
            let rcx = LowerCtx::new(env, &none, None, st.span);
            let (a, b) = (rcx.ty_name(&t), rcx.ty_name(&other));
            let at = env.decl(sig.id).map_or(st.span, |f| f.def().span);
            let leaf = leaf_name(x);
            rcx.emit_error_at(
                Code::SOME_TYPE_DEFINITION,
                at,
                format!(
                    "`{}` returns `{b}`, but `{leaf}` is `{a}` (from `{}`)",
                    leaf_name(d),
                    leaf_name(first)
                ),
            );
            rcx.help_last(Some(format!(
                "`{leaf}` is one concrete type: return {} here too, or call `{}` for it",
                nabic_diagnostic::article(&a),
                leaf_name(first)
            )));
        }
    }
    env.sink.bodies_done.set(done);
}

/// Named `some` types (full names) that `f`'s signature writes.
pub(crate) fn sig_some_types(env: &Env<'_>, f: &FnDef) -> Vec<String> {
    if env.names.some_types.is_empty() {
        return Vec::new();
    }
    let mut v = SomePaths(Vec::new());
    for p in &f.params {
        if let Some(t) = &p.ty {
            crate::walk::walk_ty(&mut v, t);
        }
    }
    if let Some(t) = &f.ret_ty {
        crate::walk::walk_ty(&mut v, t);
    }
    some_named(env, &v.0)
}

/// The last segments of the type paths a walk meets.
pub(crate) struct SomePaths(pub(crate) Vec<String>);

impl crate::walk::AstVisitor for SomePaths {
    fn ty(&mut self, t: &Ty) {
        if let AstTyKind::Path(p) = &t.kind {
            if let Some(s) = p.segments.last() {
                self.0.push(s.name.clone());
            }
        }
    }
}

/// The named `some` types whose last segment is among `leaves`.
pub(crate) fn some_named(env: &Env<'_>, leaves: &[String]) -> Vec<String> {
    let mut out: Vec<String> = env
        .names
        .some_types
        .keys()
        .filter(|x| leaves.iter().any(|l| l == leaf_name(x)))
        .cloned()
        .collect();
    out.sort();
    out
}

/// Lower `fn_def`'s body again with every `-> some Tr` call's result
/// seen only as `Tr`: what it takes beyond the bounds is reported, the
/// rest (said by the real lowering already) dropped.
#[allow(clippy::too_many_arguments)]
pub(crate) fn check_opaque_uses(
    env: &Env<'_>,
    fn_def: &FnDef,
    id: FunctionId,
    name: String,
    self_ty: Option<HirTy>,
    module: String,
    subst: &HashMap<String, HirTy>,
    ret: Option<HirTy>,
    real: std::ops::Range<usize>,
) {
    let seen: Vec<(Code, String)> = env.sink.diagnostics.borrow()[real.clone()]
        .iter()
        .map(|d| (d.code, d.message.clone()))
        .collect();
    let mut probe = LowerCtx::new(env, subst, None, fn_def.span);
    let mark = probe.probe_mark();
    let from = env.sink.diagnostics.borrow().len();
    env.sink.opaque_check.set(true);
    let view = env.sink.some_view.replace(Some(module.clone()));
    let kept = some_fields_view(env);
    let _ = lower_fn_with(env, fn_def, id, name, self_ty, module, subst, ret, None);
    env.sink.opaque_check.set(false);
    *env.sink.some_view.borrow_mut() = view;
    {
        let mut structs = env.defs.structs.borrow_mut();
        for (i, fields) in kept {
            structs[i].fields = fields;
        }
    }
    let new: Vec<nabic_diagnostic::NabiDiagnostic> = env.sink.diagnostics.borrow()[from..]
        .iter()
        .filter(|d| d.is_error() && !seen.contains(&(d.code, d.message.clone())))
        .cloned()
        .map(|d| some_hidden(env, d))
        .collect();
    probe.rollback(&mark);
    let mut all = env.sink.diagnostics.borrow_mut();
    for d in new {
        // The real lowering's error at that spot is about the concrete
        // type: the bounds' one replaces it.
        let at = d.primary.as_ref().map(|l| l.span);
        let same = (real.clone()).find(|&i| {
            let r = &all[i];
            r.is_error() && at.is_some() && r.primary.as_ref().map(|l| l.span) == at
        });
        match same {
            Some(i) => all[i] = d,
            None => all.push(d),
        }
    }
}

/// While a body is checked, struct fields that write a named `some`
/// type take what the body's module sees of it; the fields they replace,
/// by struct.
pub(crate) fn some_fields_view(env: &Env<'_>) -> Vec<(usize, Vec<HirField>)> {
    let mut kept = Vec::new();
    for (id, tys) in &env.names.some_fields {
        let i = id.0 as usize;
        let home = module_of(&env.defs.structs.borrow()[i].name);
        let none = HashMap::default();
        let mut rcx = LowerCtx::new(env, &none, None, Span::DUMMY);
        rcx.current_module = home;
        let fresh: Vec<HirTy> = tys.iter().map(|t| rcx.resolve_ast_ty(t)).collect();
        let mut structs = env.defs.structs.borrow_mut();
        let old = structs[i].fields.clone();
        for (f, t) in structs[i].fields.iter_mut().zip(fresh) {
            f.ty = t;
        }
        kept.push((i, old));
    }
    kept
}

/// A check error that comes from a named `some` type seen through its
/// bounds: `SOME_TYPE_HIDDEN`, with what to write.
pub(crate) fn some_hidden(
    env: &Env<'_>,
    mut d: nabic_diagnostic::NabiDiagnostic,
) -> nabic_diagnostic::NabiDiagnostic {
    if d.code != Code::MISMATCHED_TYPES && d.code != Code::NO_FIELD {
        return d;
    }
    let params = env.sink.some_params.borrow();
    let Some(x) = params
        .keys()
        .find(|x| d.message.contains(&format!("`{}`", leaf_name(x))))
    else {
        return d;
    };
    let leaf = leaf_name(x);
    let home = some_home(x);
    let makers = env.names.some_types[x].definers.join("`, `");
    if d.code == Code::NO_FIELD {
        d.message = format!("{}: outside {home}, only its bounds are visible", d.message);
        d.notes.retain(|n| !n.starts_with("help: "));
        return d.with_help(format!(
            "read it in a function of {home}, where `{leaf}` is its concrete type, or use a \
             method of its bounds"
        ));
    }
    d.code = Code::SOME_TYPE_HIDDEN;
    d.message = format!("{}: outside {home}, `{leaf}` is opaque", d.message);
    d.notes.retain(|n| !n.starts_with("help: "));
    d.with_help(format!(
        "only {home} sees what `{leaf}` is: keep the value as `{leaf}` and use it through \
         its bounds, get one from `{makers}`, or move this code into {home}"
    ))
}

/// A `-> some ..` fn's body type must have every bound.
#[allow(clippy::too_many_arguments)]
pub(crate) fn check_opaque(
    env: &Env<'_>,
    fn_def: &FnDef,
    module: &str,
    self_ty: Option<HirTy>,
    subst: &HashMap<String, HirTy>,
    ret: &HirTy,
) {
    let Some(bounds) = opaque_bounds(fn_def) else {
        return;
    };
    if matches!(ret, HirTy::Unknown) {
        return;
    }
    let mut rcx = LowerCtx::new(env, subst, self_ty, fn_def.span);
    module.clone_into(&mut rcx.current_module);
    rcx.set_bounds(&fn_def.generics, &fn_def.where_clause);
    let leaf = leaf_name(&fn_def.name).to_owned();
    for b in bounds {
        let shown = nabic_ast::print::print_ty(b);
        let ok = match &b.kind {
            AstTyKind::Fn(..) => {
                let want = rcx.resolve_ast_ty(b);
                coercible(&rcx, &want, ret)
            }
            _ => match rcx.bound_ref(b, module, |t| rcx.resolve_ast_ty(t)) {
                Some(want) if env.names.is_trait(&want.path) => rcx.implements_trait(ret, &want, 0),
                _ => {
                    if env.sink.unit_ret_errs.borrow_mut().insert(b.span) {
                        rcx.emit_error_at(
                            Code::NOT_A_TRAIT,
                            b.span,
                            format!("`{shown}` in `some {shown}` is not a trait"),
                        );
                        rcx.help_last(Some(
                            "a `some` bound names a trait (`some Iterator<Item = i32>`) or a \
                             function type (`some fn(i32) -> i32`)"
                                .to_owned(),
                        ));
                    }
                    continue;
                }
            },
        };
        if !ok && env.sink.unit_ret_errs.borrow_mut().insert(b.span) {
            let have = rcx.ty_name(ret);
            rcx.emit_error_at(
                Code::UNSATISFIED_BOUND,
                b.span,
                format!("`{leaf}` returns `{have}`, which is not `{shown}`"),
            );
            let write = match defines_some(env, &fn_def.name) {
                Some(x) => format!("type {} = {have};", leaf_name(x)),
                None => format!("-> {have}"),
            };
            rcx.help_last(Some(format!(
                "the body's value is the one concrete type behind `some {shown}`: return a \
                 value that is `{shown}`, or write the type it has: `{write}`"
            )));
        }
    }
}
