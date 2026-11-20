//! Declarations and bodies to lower, and signature stubs.

#[allow(clippy::wildcard_imports)]
use crate::lower::collect::*;

/// Lower declaration `i` (`stub`: its signature only).
pub(crate) fn lower_decl(env: &Env<'_>, i: usize, stub: bool) -> HirFunction {
    let id = FunctionId(u32::try_from(i).expect("FunctionId overflow"));
    let decl = env.decl(id).expect("a declaration");
    let f = decl.def();
    let bodiless;
    let f: &FnDef = if stub {
        bodiless = FnDef {
            vis: f.vis,
            attrs: f.attrs.clone(),
            name: f.name.clone(),
            generics: f.generics.clone(),
            params: f.params.clone(),
            ret_ty: f.ret_ty.clone(),
            body: None,
            is_async: f.is_async,
            is_generator: f.is_generator,
            is_unsafe: f.is_unsafe,
            throws: f.throws.clone(),
            where_clause: f.where_clause.clone(),
            span: f.span,
        };
        &bodiless
    } else {
        f
    };
    match decl {
        MethodOrFn::Fn(_) => lower_fn(env, f, id, None, fn_module(env, f)),
        MethodOrFn::Method {
            self_ty,
            module,
            key,
            subst,
            ..
        } => crate::lower::mono::lower_fn_inner(
            env,
            f,
            id,
            key.clone(),
            Some(self_ty.clone()),
            module.clone(),
            subst,
        ),
    }
}

/// The `roots` (user code, exports, lazy static inits) are lowered, then
/// every decl a lowered body names, lowest id first. An unreached library
/// body stays a signature-only stub that MIR drops; `--check-library`
/// lowers all. Prebuilt (`linked`) functions keep only their signatures.
pub(crate) fn lower_bodies(
    env: &Env<'_>,
    roots: &[bool],
    linked: &nabic_core::HashSet<FunctionId>,
    incr: Option<&crate::lower::incr::Incr<'_>>,
) -> (Vec<HirFunction>, Vec<FunctionId>) {
    let decls = &env.decls;
    let lower = |i: usize, stub: bool| match incr {
        Some(incr) if !stub => incr.lower(env, i),
        _ => lower_decl(env, i, stub),
    };
    let mut done: Vec<Option<HirFunction>> = (0..decls.len()).map(|_| None).collect();
    let mut pending: std::collections::BTreeSet<usize> =
        (0..decls.len()).filter(|&i| roots[i]).collect();
    let named = |f: &HirFunction, pending: &mut std::collections::BTreeSet<usize>| {
        let Some(body) = &f.body else { return };
        crate::each_block(body, &mut |e| {
            if let HirExprKind::Call { callee: id, .. }
            | HirExprKind::FnRef(id)
            | HirExprKind::ClosureValue { fn_id: id, .. }
            | HirExprKind::CoroNew { body_fn: id, .. } = &e.kind
            {
                pending.insert(id.0 as usize);
            }
        });
    };
    // Lifted closures / monomorphizations / dispatchers already looked at.
    let mut scanned = 0;
    loop {
        while let Some(i) = pending.pop_first() {
            if done.get(i).is_none_or(Option::is_some) {
                continue; // an extra (already lowered) or a done decl
            }
            #[allow(clippy::cast_possible_truncation)]
            let id = FunctionId(i as u32);
            let f = lower(i, linked.contains(&id));
            named(&f, &mut pending);
            done[i] = Some(f);
            let extras = env.sink.extras.borrow();
            extras[scanned..]
                .iter()
                .for_each(|f| named(f, &mut pending));
            scanned = extras.len();
        }
        build_any_dispatchers(env);
        let extras = env.sink.extras.borrow();
        extras[scanned..]
            .iter()
            .for_each(|f| named(f, &mut pending));
        drop(extras);
        pending.retain(|&i| done.get(i).is_some_and(Option::is_none));
        if pending.is_empty() {
            break;
        }
        // The new bodies may add `any` tags: drop the dispatchers (the
        // last ones in) and build them again after.
        let n = env.sink.any_dispatch.borrow().len();
        let mut extras = env.sink.extras.borrow_mut();
        scanned = extras.len() - n;
        extras.truncate(scanned);
    }
    env.sink.bodies_done.set(true);
    let mut unlowered = Vec::new();
    let functions = done
        .into_iter()
        .enumerate()
        .map(|(i, f)| {
            f.unwrap_or_else(|| {
                let stub = sig_stub(env, i);
                unlowered.push(stub.id);
                stub
            })
        })
        .collect();
    (functions, unlowered)
}

/// The body-less stub of unreached declaration `i`, from its resolved
/// signature (for tools; MIR drops it).
pub(crate) fn sig_stub(env: &Env<'_>, i: usize) -> HirFunction {
    let id = FunctionId(u32::try_from(i).expect("FunctionId overflow"));
    let f = env
        .decl(FunctionId(u32::try_from(i).expect("FunctionId overflow")))
        .expect("a declaration")
        .def();
    let name = match &env.decls[i] {
        MethodOrFn::Fn(f) => f.name.clone(),
        MethodOrFn::Method { key, .. } => key.clone(),
    };
    let sig = &env.fn_table[&env.key_of[&id]];
    let mut params = Vec::with_capacity(f.params.len());
    let mut locals = Vec::with_capacity(f.params.len());
    let mut refs = Vec::new();
    for (k, (p, ty)) in f.params.iter().zip(&sig.param_tys).enumerate() {
        let local = nabic_ir_common::LocalId(u32::try_from(k).expect("LocalId overflow"));
        if p.by_ref {
            refs.push(crate::lower::mono::ref_slot_ty(
                crate::lower::mono::defers_refs(f),
                ty.clone(),
            ));
        }
        locals.push(HirLocal {
            id: local,
            name: Some(p.name.clone()),
            ty: ty.clone(),
            boxed: false,
        });
        params.push(HirParam {
            name: p.name.clone(),
            local,
            ty: ty.clone(),
            by_ref: p.by_ref,
        });
    }
    HirFunction {
        id,
        def_id: None,
        name,
        params,
        locals,
        ret_ty: crate::lower::mono::ref_packed(&sig.ret_ty, &refs),
        body: None,
        is_async: false,
        is_generator: false,
        span: f.span,
        facts: nabic_ir_common::FnFacts::default(),
    }
}
