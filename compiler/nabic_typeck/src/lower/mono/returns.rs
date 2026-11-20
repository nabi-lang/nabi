//! Return types inferred from bodies.

#[allow(clippy::wildcard_imports)]
use crate::lower::mono::*;

/// Body `b` ends in a value or has a `return v` of its own.
pub(crate) fn gives_value(b: &Block) -> bool {
    struct V {
        depth: u32,
        found: bool,
    }
    impl crate::walk::AstVisitor for V {
        fn stmt(&mut self, s: &Stmt) {
            if self.depth == 0 && matches!(s.kind, StmtKind::Return(Some(_))) {
                self.found = true;
            }
        }
        fn enter_body(&mut self) {
            self.depth += 1;
        }
        fn leave_body(&mut self) {
            self.depth -= 1;
        }
    }
    if b.trailing.is_some() {
        return true;
    }
    let mut v = V {
        depth: 0,
        found: false,
    };
    crate::walk::walk_block(&mut v, b);
    v.found
}

/// The ok side of the return type of declaration `id` (in
/// `Env::infer_ret`), inferred on first use.
pub(crate) fn decl_ret_ok(env: &Env<'_>, id: FunctionId) -> HirTy {
    let cached = inferred_ret(env, id);
    match cached {
        Some(Some(t)) => t,
        Some(None) => {
            env.sink.recursive_ret.borrow_mut().push(id);
            HirTy::Unknown
        }
        None => match env.decl(id).expect("a declaration") {
            MethodOrFn::Fn(f) => {
                let module = fn_module(env, f);
                let start = env.sink.next_id.get();
                let keep = !env.sink.bodies_done.get();
                let (t, done) = infer_ret_of(
                    env,
                    f,
                    id,
                    &f.name,
                    None,
                    &module,
                    &HashMap::default(),
                    keep,
                );
                keep_inferred(env, id, start, done);
                t
            }
            MethodOrFn::Method {
                fn_def,
                self_ty,
                module,
                key,
                subst,
                ..
            } => {
                let self_ty = Some(self_ty.clone());
                let start = env.sink.next_id.get();
                let keep = !env.sink.bodies_done.get();
                let (t, done) = infer_ret_of(env, fn_def, id, key, self_ty, module, subst, keep);
                keep_inferred(env, id, start, done);
                t
            }
        },
    }
}

/// A declaration lowered for good while inferring its return type: the
/// decl pass takes it (`lower_fn_inner`).
pub(crate) fn keep_inferred(env: &Env<'_>, id: FunctionId, start: u32, done: Option<HirFunction>) {
    if let Some(f) = done {
        crate::lower::incr::touch(id);
        env.sink.inferred_fns.borrow_mut().insert(id, (start, f));
    }
}

/// `decl_ret_ok` in the declared return type (`()`, or a `Throws`
/// carrier around it).
pub(crate) fn decl_ret(ctx: &LowerCtx<'_>, id: FunctionId, declared: HirTy) -> HirTy {
    with_ok(declared, decl_ret_ok(ctx.env, id))
}

pub(crate) fn has_unknown(t: &HirTy) -> bool {
    match t {
        HirTy::Unknown => true,
        HirTy::Sum(ts) | HirTy::Tuple(ts) => ts.iter().any(has_unknown),
        HirTy::Array(t, _) | HirTy::GcBuf(t) => has_unknown(t),
        HirTy::Fn(ps, r, _) => ps.iter().any(has_unknown) || has_unknown(r),
        HirTy::Closure { args, ret, env, .. } => {
            args.iter().any(has_unknown) || has_unknown(ret) || has_unknown(env)
        }
        HirTy::Throws { ok, err } => has_unknown(ok) || has_unknown(err),
        _ => false,
    }
}

pub fn with_ok(declared: HirTy, ok: HirTy) -> HirTy {
    match declared {
        HirTy::Throws { err, .. } => HirTy::Throws {
            ok: Box::new(ok),
            err,
        },
        _ => ok,
    }
}

/// Lower the body with an open return type and join its `return` and tail
/// types (they must agree). With `keep`, a body whose paths agree (no
/// `throws`, no `ref` parameter) is final as lowered; else rolled back.
#[allow(clippy::too_many_arguments)]
pub(crate) fn infer_ret_of(
    env: &Env<'_>,
    fn_def: &FnDef,
    id: FunctionId,
    name: &str,
    self_ty: Option<HirTy>,
    module: &str,
    subst: &HashMap<String, HirTy>,
    keep: bool,
) -> (HirTy, Option<HirFunction>) {
    if let Some(Some(t)) = inferred_ret(env, id) {
        return (t, None);
    }
    crate::lower::incr::touch(id);
    env.sink.inferred_ret.borrow_mut().insert(id, None);
    let hits = env.sink.recursive_ret.borrow().len();
    let tentatives = env.sink.tentative_ret.borrow().len();
    let mut probe = LowerCtx::new(env, subst, None, fn_def.span);
    let mark = probe.probe_mark();
    let mut tys = Vec::new();
    // `-> some fn(A) -> R`: the body's closure takes its parameter types.
    let hint = opaque_bounds(fn_def)
        .and_then(|bs| bs.iter().find(|b| matches!(b.kind, AstTyKind::Fn(..))))
        .map_or(HirTy::Unknown, |b| {
            let mut rcx = LowerCtx::new(env, subst, self_ty.clone(), fn_def.span);
            module.clone_into(&mut rcx.current_module);
            rcx.set_bounds(&fn_def.generics, &fn_def.where_clause);
            rcx.resolve_ast_ty(b)
        });
    let hinted = hint != HirTy::Unknown;
    let mut f = lower_fn_with(
        env,
        fn_def,
        id,
        name.to_owned(),
        self_ty.clone(),
        module.to_owned(),
        subst,
        Some(hint),
        Some(&mut tys),
    );
    // A call of itself leaves its type open (`Unknown`); one of a fn
    // still being inferred further out makes this result tentative.
    let (self_hit, tentative) = {
        let mut hit = env.sink.recursive_ret.borrow_mut();
        let own = hit[hits..].contains(&id);
        let mut i = hits;
        while i < hit.len() {
            if hit[i] == id {
                hit.remove(i);
            } else {
                i += 1;
            }
        }
        (own, hit.len() > hits)
    };
    tys.retain(|t| *t != HirTy::Never);
    let plain = match &fn_def.throws {
        nabic_ast::ThrowsKind::None => true,
        nabic_ast::ThrowsKind::Typed(e) => e.is_empty(),
        nabic_ast::ThrowsKind::Untyped => false,
    } && !fn_def.params.iter().any(|p| p.by_ref);
    let agreed = tys.windows(2).all(|w| w[0] == w[1]);
    let final_as_is = keep && plain && agreed && !self_hit && !tentative && !hinted;
    if !final_as_is {
        probe.rollback(&mark);
    }
    let unit = tys.contains(&HirTy::Unit);
    tys.retain(|t| *t != HirTy::Unit);
    let fail = |why: &str| {
        probe.emit_error_at(
            Code::TYPE_ANNOTATIONS_NEEDED,
            fn_def.span,
            format!(
                "cannot infer the return type of `{}`: {why} - write `-> T`",
                leaf_name(&fn_def.name)
            ),
        );
        HirTy::Unknown
    };
    // No path reaches the end or a `return`: `!`.
    let ret = if tys.is_empty() && !unit {
        HirTy::Never
    } else if tys.is_empty() {
        HirTy::Unit
    } else if unit {
        if tentative {
            HirTy::Unknown
        } else {
            fail("one path returns a value and another none")
        }
    } else if let Some(t) = (!tys.iter().any(has_unknown))
        .then(|| branch_join(&probe, &tys))
        .flatten()
    {
        // A sum one path already is takes the others (`none` beside `T?`).
        t
    } else if let Some(other) = tys.iter().find(|t| **t != tys[0] && !has_unknown(t)) {
        // No sum is invented for differing return paths.
        if !tentative && !has_unknown(&tys[0]) {
            let (a, b) = (probe.ty_name(&tys[0]), probe.ty_name(other));
            let sum = probe.sum_hint(&tys[0], other);
            probe.emit_error_at(
                Code::BRANCH_TYPES,
                fn_def.span,
                format!(
                    "the return paths of `{}` have different types `{a}` and `{b}` - nabi \
                     does not infer a sum type",
                    leaf_name(&fn_def.name)
                ),
            );
            if let Some(bounds) = opaque_bounds(fn_def) {
                let shown: Vec<String> = bounds.iter().map(nabic_ast::print::print_ty).collect();
                let shown = match shown.as_slice() {
                    [one] => one.clone(),
                    _ => format!("({})", shown.join(" + ")),
                };
                probe.help_last(Some(format!(
                    "`some {shown}` is one concrete type; to return several types, write \
                     `-> any {shown}`"
                )));
            } else {
                probe.help_last(Some(format!("write the return type: `-> {sum}`")));
            }
        }
        HirTy::Unknown
    } else {
        tys.swap_remove(0)
    };
    let ret = unit_ret(&probe, fn_def, ret, !tentative, !subst.is_empty());
    if !tentative {
        check_opaque(env, fn_def, module, self_ty, subst, &ret);
    }
    if tentative {
        env.sink
            .inferred_ret
            .borrow_mut()
            .insert(id, Some(ret.clone()));
        env.sink.tentative_ret.borrow_mut().push(id);
        return (ret, None);
    }
    let stale: Vec<FunctionId> = env.sink.tentative_ret.borrow_mut().split_off(tentatives);
    for t in stale {
        env.sink.inferred_ret.borrow_mut().remove(&t);
    }
    let ret = if self_hit && has_unknown(&ret) {
        fail("its body calls it again")
    } else {
        ret
    };
    env.sink
        .inferred_ret
        .borrow_mut()
        .insert(id, Some(ret.clone()));
    if let Some(r) = env.sink.ref_out.borrow_mut().get_mut(&id) {
        r.ret = with_ok(r.ret.clone(), ret.clone());
    }
    // An instance's preview signature: a call made while its body is
    // lowered again (a recursive cycle through other instances) reads it.
    if let Some(sig) = env.sink.mono_sigs.borrow_mut().get_mut(&id) {
        sig.ret_ty = with_ok(sig.ret_ty.clone(), ret.clone());
    }
    if final_as_is {
        f.ret_ty = ret.clone();
        return (ret, Some(f));
    }
    (ret, None)
}

/// A `pub` fn without `->` (`UNIT_RET_ATTR`) returns `()`,
/// also when its body diverges; a body with a value is an error (with
/// `report`) that names the `-> T` to write (in a generic `instance`,
/// the type of that instance).
pub(crate) fn unit_ret(
    ctx: &LowerCtx<'_>,
    f: &FnDef,
    ret: HirTy,
    report: bool,
    instance: bool,
) -> HirTy {
    if !f.attrs.iter().any(|a| a.path == [nabic_ast::UNIT_RET_ATTR]) {
        return ret;
    }
    let Some(body) = f.body.as_deref() else {
        return ret;
    };
    // An `async fn`'s body is the `async` block its fn returns.
    let (val, body, coro) = match (&ret, body.trailing.as_deref().map(|e| &e.kind)) {
        (HirTy::Struct(id), Some(ExprKind::AsyncBlock(b)))
            if b.label.as_deref() == Some(crate::lower::FN_BLOCK_LABEL) =>
        {
            (
                ctx.struct_def(*id)
                    .generic_args
                    .first()
                    .cloned()
                    .unwrap_or(HirTy::Unknown),
                b,
                true,
            )
        }
        _ => (ret.clone(), body, false),
    };
    let ty = match val {
        HirTy::Unit | HirTy::Unknown => return ret,
        HirTy::Never if coro => return ret,
        HirTy::Never => return HirTy::Unit,
        t => ctx.ty_name(&t),
    };
    // An untyped parameter (`T#x`) already has its error.
    let untyped =
        f.generics.params.iter().any(
            |g| matches!(g, nabic_ast::GenericParam::Type { name, .. } if name.starts_with("T#")),
        );
    // A probe of an implicit body reports nothing (and must not use up
    // the once).
    if !report
        || untyped
        || ctx.env.sink.implicit_depth.get() > 0
        || !ctx.env.sink.unit_ret_errs.borrow_mut().insert(f.span)
    {
        return ret;
    }
    let leaf = leaf_name(&f.name).replacen("__", "::", 1);
    let vis = if f.vis == Visibility::Pkg {
        "pub(pkg)"
    } else {
        "pub"
    };
    let at = body.trailing.as_deref().map_or(f.span, |e| e.span);
    let mut d = nabic_diagnostic::NabiDiagnostic::error(
        Code::PUB_SIGNATURE,
        at,
        format!("`{vis} fn {leaf}` has no `->`, so it returns `()`, but its body gives `{ty}`"),
    );
    if !instance && f.generics.params.is_empty() && f.where_clause.is_empty() {
        let at = nabic_diagnostic::Span {
            end: body.span.start,
            ..body.span
        };
        d = d
            .with_help(format!("write `-> {ty}` before the body"))
            .with_fix(at, format!("-> {ty} "), format!("add `-> {ty}`"));
    } else {
        d = d.with_help(format!(
            "write the return type before the body: `-> {ty}` here, or the type \
             parameter it comes from"
        ));
    }
    ctx.emit_diag(d);
    ret
}

/// The inferred return type of `id` so far (`Some(None)`: being
/// inferred).
#[allow(clippy::option_option)] // the table's entry, if any
pub fn inferred_ret(env: &Env<'_>, id: FunctionId) -> Option<Option<HirTy>> {
    let r = env.sink.inferred_ret.borrow().get(&id).cloned();
    crate::lower::incr::log(
        || format!("r|{}", id.0),
        || crate::lower::incr::Lookup::InferredRet(id, r.clone()),
    );
    r
}
