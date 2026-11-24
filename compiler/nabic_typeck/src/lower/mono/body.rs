//! Function bodies: lowering one body against its signature.

#[allow(clippy::wildcard_imports)]
use crate::lower::mono::*;

/// Lower one body; `subst` is the instance's substitution (empty if not
/// generic), `module` where its names resolve.
pub fn lower_fn_inner(
    env: &Env<'_>,
    fn_def: &FnDef,
    id: FunctionId,
    name: String,
    self_ty: Option<HirTy>,
    module: String,
    subst: &HashMap<String, HirTy>,
) -> HirFunction {
    let ret = if env.infers_ret(id) {
        let r = decl_ret_ok(env, id);
        if fn_def.body.is_some() {
            crate::lower::incr::touch(id);
            if let Some((_, f)) = env.sink.inferred_fns.borrow_mut().remove(&id) {
                return f;
            }
        }
        Some(r)
    } else if infers_ret(fn_def, |s| env.diverges(s)) {
        let (r, done) = infer_ret_of(
            env,
            fn_def,
            id,
            &name,
            self_ty.clone(),
            &module,
            subst,
            true,
        );
        if let Some(f) = done {
            return f;
        }
        Some(r)
    } else {
        None
    };
    let outer = env.sink.opaque_called.replace(false);
    let from = env.sink.diagnostics.borrow().len();
    let f = lower_fn_with(
        env,
        fn_def,
        id,
        name.clone(),
        self_ty.clone(),
        module.clone(),
        subst,
        ret.clone(),
        None,
    );
    let called = env.sink.opaque_called.replace(outer) || holds_some(env, &f);
    if called && !env.sink.opaque_check.get() {
        let to = env.sink.diagnostics.borrow().len();
        check_opaque_uses(env, fn_def, id, name, self_ty, module, subst, ret, from..to);
    }
    f
}

/// A local of `f` is a struct with a named `some` field.
pub(crate) fn holds_some(env: &Env<'_>, f: &HirFunction) -> bool {
    fn has(t: &HirTy, ids: &[StructId]) -> bool {
        match t {
            HirTy::Struct(id) => ids.contains(id),
            HirTy::Sum(ts) | HirTy::Tuple(ts) => ts.iter().any(|t| has(t, ids)),
            HirTy::Array(t, _) => has(t, ids),
            _ => false,
        }
    }
    let fields = &env.names.some_fields;
    if fields.is_empty() {
        return false;
    }
    let ids: Vec<StructId> = fields.iter().map(|(id, _)| *id).collect();
    f.locals.iter().any(|l| has(&l.ty, &ids))
}

/// Without `-> T`, the return type comes from the body when it
/// has a value or may diverge everywhere. `main` keeps `()`.
pub fn infers_ret(f: &FnDef, may_diverge: impl Fn(nabic_diagnostic::Span) -> bool) -> bool {
    let Some(body) = &f.body else {
        return false;
    };
    if opaque_bounds(f).is_some() {
        return true;
    }
    if f.ret_ty.is_some() || f.is_async || f.is_generator || f.name == "main" {
        return false;
    }
    gives_value(body) || may_diverge(f.span)
}

/// `lower_fn_inner` with the ok side of the return type given (`ret`;
/// `Unknown` with `ret_tys` to collect the body's value types).
#[allow(clippy::too_many_arguments)]
pub fn lower_fn_with(
    env: &Env<'_>,
    fn_def: &FnDef,
    id: FunctionId,
    name: String,
    self_ty: Option<HirTy>,
    module: String,
    subst: &HashMap<String, HirTy>,
    ret: Option<HirTy>,
    ret_tys: Option<&mut Vec<HirTy>>,
) -> HirFunction {
    // A `rethrows` instance with a non-throwing argument doesn't vote on
    // warnings (its `try`s cover nothing by construction).
    let nothrow = HirTy::NoThrow;
    let instance =
        (!subst.is_empty() && !subst.values().any(|t| *t == nothrow)).then_some((fn_def.span, id));
    if let Some((t, id)) = instance {
        env.sink.instance_warns.borrow_mut().instance(t, id);
    }
    env.sink.instances.borrow_mut().push(instance);
    env.sink.bodies.borrow_mut().push(fn_def.span);
    let is_const = fn_def
        .attrs
        .iter()
        .any(|a| a.path == [nabic_ast::CONST_FN_ATTR]);
    let is_std = nabic_core::in_toolchain(&module);
    crate::lower::incr::touch(id);
    env.sink
        .fn_origin
        .borrow_mut()
        .insert(id, (is_const, is_std));
    let mut ctx = LowerCtx::new(env, subst, self_ty, fn_def.span);
    if let Some(MethodOrFn::Method { default: true, .. }) = env.decl(id) {
        ctx.default_of = env
            .method_info(id)
            .and_then(|i| Some((i.trait_name.clone()?, i.trait_args.clone())));
    }
    let scope = fn_def
        .body
        .as_ref()
        .map(|b| crate::lower::infer::open_scope(env, b.span));
    if let Some(body) = &fn_def.body {
        ctx.sole_names = sole_names(body);
    }
    if fn_def.attrs.iter().any(|a| a.path == ["track_caller"]) {
        let fixed_abi = fn_def
            .attrs
            .iter()
            .any(|a| a.path == [nabic_ast::EXTERN_FN_ATTR] || a.name() == "export_name");
        // The location is a hidden parameter: no fixed signature.
        if fixed_abi || name == "main" {
            ctx.emit_error(
                Code::MISPLACED_ATTRIBUTE,
                format!(
                    "`#[track_caller]` on `{}`: not on `main` or a fixed-ABI \
                     (`extern \"C\"`, `#[export_name]`) function",
                    fn_def.name
                ),
            );
        } else {
            env.sink.track_caller.borrow_mut().insert(id);
        }
    }
    ctx.current_module = module;
    ctx.set_bounds(&fn_def.generics, &fn_def.where_clause);
    ctx.assoc_names = fn_def
        .attrs
        .iter()
        .filter(|a| a.path.first().is_some_and(|p| p == crate::ASSOC_TYPES_ATTR))
        .flat_map(|a| a.path[1..].iter().cloned())
        .collect();
    ctx.unsafe_depth = u32::from(fn_def.is_unsafe);
    // The synthesized entry.
    ctx.implicit_main = env.implicit_main && name == "main" && ctx.self_ty.is_none();
    ctx.harness_main = env.harness_main
        && name == "main"
        && ctx.current_module.is_empty()
        && ctx.self_ty.is_none();

    if let Some(b) = &fn_def.body {
        ctx.ref_captured = ref_captured_bindings(&fn_def.params, b);
    }
    // A default is a `const` expression, checked here once
    // (an instance's defaults are its template's).
    if subst.is_empty() {
        for p in &fn_def.params {
            let (Some(d), Some(t)) = (&p.default, &p.ty) else {
                continue;
            };
            let mut cx = ctx.clone();
            let ty = cx.resolve_ast_ty(t);
            if has_unknown(&ty) {
                continue;
            }
            let v = crate::lower::infer::scoped(&mut cx, |cx| lower_expected(cx, d, ty));
            crate::lower::consteval::constant(
                &cx,
                &format!("the default of parameter `{}`", p.name),
                v,
            );
        }
    }
    let mut params = Vec::with_capacity(fn_def.params.len());
    for p in &fn_def.params {
        params.push(lower_param(&mut ctx, p));
    }
    // A `[ref]`-captured parameter moves into a cell (`let x = x;`).
    let mut prelude = Vec::new();
    // An `async` method's `ref self` is shared with its future through a
    // cell, which the call hands back (`defers_refs`).
    ctx.ref_cells = defers_refs(fn_def);
    for (p, ap) in params.iter().zip(&fn_def.params) {
        if ctx.ref_captured.contains(&(p.name.clone(), ap.span)) || (ctx.ref_cells && p.by_ref) {
            // A by-value `self` is not a slot to write.
            if p.name == "self" && !p.by_ref {
                ctx.emit_error_at(
                    Code::INVALID_CAPTURE,
                    ap.span,
                    "`[ref self]` needs a `ref self` method".to_owned(),
                );
                ctx.help_last(Some(
                    "write `ref self` in the method's parameters, or capture `self` by value \
                     (drop it from the list)"
                        .to_owned(),
                ));
            }
            let init = hir_local(p.local, &p.ty, fn_def.span);
            prelude.push(ctx.cell_let(&p.name, p.ty.clone(), init));
        }
    }
    ctx.owe_params = params
        .iter()
        .zip(&fn_def.params)
        .filter(|(p, ap)| {
            // An `async` / generator body runs in its coroutine, which
            // owns the captured parameters instead.
            !fn_def.is_async
                && !fn_def.is_generator
                && !p.by_ref
                && !ctx.ref_captured.contains(&(p.name.clone(), ap.span))
        })
        .map(|(p, _)| (p.local, p.ty.clone()))
        .collect();
    // A captured `ref` parameter hands back its cell, where the writes go.
    ctx.ref_params = params
        .iter()
        .filter(|p| p.by_ref)
        .map(|p| {
            let l = ctx.lookup(&p.name).unwrap_or(p.local);
            (l, ref_slot_ty(ctx.ref_cells, p.ty.clone()))
        })
        .collect();

    let mut ret_ty = ctx.resolve_ast_ty(&sig_ret_ast(fn_def));
    if let Some(r) = ret {
        ret_ty = with_ok(ret_ty, r);
    }
    if ret_tys.is_some() {
        ctx.ret_tys = Some(Vec::new());
    }
    if let HirTy::Throws { ok, err } = &ret_ty {
        ctx.fn_ret_ty = (**ok).clone();
        // An error escaping a throwing `main` is reported by
        // the entry itself (exit 1); `main` keeps a plain C return type.
        if name == "main" && ctx.self_ty.is_none() {
            ctx.uncaught_in_main = true;
            ret_ty = ctx.fn_ret_ty.clone();
        } else {
            ctx.fn_throws = Some((**err).clone());
        }
    } else {
        ctx.fn_ret_ty = ret_ty.clone();
    }

    let body = fn_def.body.as_ref().map(|b| {
        let mut block = lower_fn_body(&mut ctx, b, &fn_def.name);
        block.stmts.splice(0..0, prelude);
        block
    });
    let mut ret_tys = ret_tys;
    if let Some(out) = ret_tys.as_deref_mut() {
        *out = ctx.ret_tys.take().unwrap_or_default();
    }
    env.sink.instances.borrow_mut().pop();
    env.sink.bodies.borrow_mut().pop();
    // `ref` parameters: the function hands their values back too.
    let ref_tys: Vec<HirTy> = ctx.ref_params.iter().map(|(_, t)| t.clone()).collect();
    ret_ty = ref_packed(&ret_ty, &ref_tys);

    let mut f = HirFunction {
        id,
        def_id: None,
        name,
        params,
        locals: std::mem::take(&mut ctx.locals),
        ret_ty,
        body,
        is_async: false,
        is_generator: false,
        span: fn_def.span,
        facts: nabic_ir_common::FnFacts {
            inline: inline_hint(&fn_def.attrs),
            simd: simd_op(&fn_def.attrs),
            ..nabic_ir_common::FnFacts::default()
        },
    };
    if let Some(s) = scope {
        crate::lower::infer::close_scope(&ctx, s, &mut f, ret_tys);
    }
    f
}

/// The `core::simd` op of `#[intrinsic("nabi.simd.OP")]`.
fn simd_op(attrs: &[nabic_ast::Attribute]) -> Option<nabic_ir_common::SimdOp> {
    let a = attrs.iter().find(|a| a.name() == "intrinsic")?;
    let AttrBody::Args(xs) = &a.body else {
        return None;
    };
    xs.iter().find_map(|x| match x {
        AttrArg::Lit(nabic_ast::Literal::Str(s)) => nabic_ir_common::SimdOp::parse(s),
        _ => None,
    })
}

/// The `#[inline]` hint; `#[cold]` wins unless `#[inline(always)]`;
/// a `core::simd` op is never inlined, so the back end sees its calls.
pub(crate) fn inline_hint(attrs: &[nabic_ast::Attribute]) -> nabic_ir_common::Inline {
    if simd_op(attrs).is_some() {
        return Inline::Never;
    }
    let hint = match attrs.iter().find(|a| a.name() == "inline").map(|a| &a.body) {
        None => Inline::Auto,
        Some(AttrBody::Args(xs)) => match xs.as_slice() {
            [AttrArg::Word(w)] if w == "always" => Inline::Always,
            [AttrArg::Word(w)] if w == "never" => Inline::Never,
            _ => Inline::Hint,
        },
        Some(_) => Inline::Hint,
    };
    if hint != Inline::Always && attrs.iter().any(|a| a.name() == "cold") {
        return Inline::Cold;
    }
    hint
}

pub(crate) fn lower_fn(
    env: &Env<'_>,
    fn_def: &FnDef,
    id: FunctionId,
    method: Option<(HirTy, String)>,
    module: String,
) -> HirFunction {
    // Methods are named by `fn_table` key (`Ty__m`) so same-named methods
    // don't collide as symbols.
    let (method_self_ty, name) = match method {
        Some((ty, key)) => (Some(ty), key),
        None => (None, fn_def.name.clone()),
    };
    lower_fn_inner(
        env,
        fn_def,
        id,
        name,
        method_self_ty,
        module,
        &HashMap::default(),
    )
}

/// Parameter and return types of declaration `i` resolved as its body
/// sees them: in its module, `Self` and trait parameters bound.
pub(crate) fn decl_sig(env: &Env<'_>, i: usize, base: &FnSig) -> FnSig {
    let no_subst = HashMap::default();
    let id = FunctionId(u32::try_from(i).expect("FunctionId overflow"));
    let (f, self_ty, module, subst) = match env.decl(id).expect("a declaration") {
        MethodOrFn::Fn(f) => (*f, None, fn_module(env, f), &no_subst),
        MethodOrFn::Method {
            fn_def,
            self_ty,
            module,
            subst,
            ..
        } => (&**fn_def, Some(self_ty.clone()), module.clone(), subst),
    };
    let (param_tys, ret_ty) = sig_tys(env, f, self_ty, module, subst);
    FnSig {
        ret_ty,
        param_tys,
        ..base.clone()
    }
}

/// `f`'s parameter and result types, resolved in `module`.
pub(crate) fn sig_tys(
    env: &Env<'_>,
    f: &FnDef,
    self_ty: Option<HirTy>,
    module: String,
    subst: &HashMap<String, HirTy>,
) -> (Vec<HirTy>, HirTy) {
    let mut ctx = LowerCtx::new(env, subst, self_ty, f.span);
    ctx.current_module = module;
    ctx.set_bounds(&f.generics, &f.where_clause);
    ctx.assoc_names = f
        .attrs
        .iter()
        .filter(|a| a.path.first().is_some_and(|p| p == crate::ASSOC_TYPES_ATTR))
        .flat_map(|a| a.path[1..].iter().cloned())
        .collect();
    let param_tys = f
        .params
        .iter()
        .map(|p| match (&ctx.self_ty, p.name.as_str(), &p.ty) {
            (Some(st), "self", _) => st.clone(),
            (_, _, Some(t)) => ctx.resolve_ast_ty(t),
            (_, _, None) => HirTy::Any,
        })
        .collect();
    let ret_ty = ctx.resolve_ast_ty(&crate::lower::throws::sig_ret_ast(f));
    (param_tys, ret_ty)
}

pub(crate) fn lower_param(ctx: &mut LowerCtx<'_>, p: &Param) -> HirParam {
    let ty = if p.name == "self" {
        // Implicit `self` receiver inherits the impl target's type.
        ctx.self_ty.clone().unwrap_or(HirTy::Any)
    } else {
        p.ty.as_ref().map_or(HirTy::Any, |t| ctx.resolve_ast_ty(t))
    };
    let local = ctx.declare(Some(p.name.clone()), ty.clone());
    HirParam {
        name: p.name.clone(),
        local,
        ty,
        by_ref: p.by_ref,
    }
}

/// Lower a body against its return type: the tail must match, and a
/// non-unit function must not fall off the end.
pub(crate) fn lower_fn_body(ctx: &mut LowerCtx<'_>, b: &Block, fn_name: &str) -> HirBlock {
    ctx.push_expected(ctx.fn_ret_ty.clone());
    if ctx.uncaught_in_main {
        main_frame(ctx);
    }
    if let Some(t) = &b.trailing {
        crate::lower::expr::guard_sink(ctx, t);
    }
    // Parameters the body owns are released on its exits.
    ctx.push_scope();
    let owed = ctx.err_frames.len();
    let mut flags = Vec::new();
    for (l, ty) in std::mem::take(&mut ctx.owe_params) {
        flags.extend(crate::lower::affine::owe(ctx, l, &ty));
    }
    let mut block = lower_block(ctx, b);
    if !flags.is_empty() {
        flags.append(&mut block.stmts);
        block.stmts = flags;
        crate::lower::affine::settle(ctx, owed, &mut block.stmts, &mut block.trailing, b.span);
    }
    ctx.pop_scope();
    ctx.pop_expected();
    let mut block = check_fn_body(ctx, block, fn_name);
    if ctx.uncaught_in_main {
        block = uncaught_in_main(ctx, block, b.span);
    }
    finish_throwing_body(ctx, &mut block);
    if !ctx.ref_params.is_empty() && !hir_block_diverges(&block) {
        let span = b.span;
        let v = block
            .trailing
            .take()
            .unwrap_or_else(|| crate::lower::throws::unit_value(span));
        block.trailing = Some(ref_pack(ctx, v, span));
    }
    block
}

/// The value checks of `lower_fn_body`.
pub(crate) fn check_fn_body(
    ctx: &mut LowerCtx<'_>,
    mut block: HirBlock,
    fn_name: &str,
) -> HirBlock {
    let ret = ctx.fn_ret_ty.clone();
    let diverges = hir_block_diverges(&block);
    if let Some(tys) = ctx.ret_tys.as_mut() {
        match &block.trailing {
            Some(t) if !hir_expr_diverges(t) => tys.push(t.ty.clone()),
            None if !diverges => tys.push(HirTy::Unit),
            _ => {}
        }
    }
    let falls_off = || {
        let name = leaf_name(fn_name);
        if ret == HirTy::Never {
            format!(
                "function `{name}` returns `!` but its body can finish; it must end in a \
                 call of a `-> !` function or a `loop` without `break`"
            )
        } else {
            format!(
                "function `{name}` returns `{}` but its body can finish without returning a \
                 value (missing `return`)",
                ctx.ty_name(&ret)
            )
        }
    };
    match block.trailing.take() {
        // A unit function discards a trailing value (`fn f() { g() }`).
        Some(t) if ret == HirTy::Unit && t.ty != HirTy::Unit => block.stmts.push(HirStmt::Expr(t)),
        Some(t) if hir_expr_diverges(&t) => block.trailing = Some(t),
        // `fn f() -> i32 { if c { return 1; } }`: a unit-valued tail
        // means some path falls off the end.
        Some(t) if t.ty == HirTy::Unit && !matches!(ret, HirTy::Unknown | HirTy::Unit) => {
            ctx.emit_error(Code::MISMATCHED_TYPES, falls_off());
            block.trailing = Some(t);
        }
        Some(t) => {
            let name = leaf_name(fn_name).to_owned();
            block.trailing = Some(coerce(ctx, &ret, t, &|| format!("the body of `{name}`")));
        }
        None => {
            if !matches!(ret, HirTy::Unit | HirTy::Unknown) && !hir_block_diverges(&block) {
                // `fn f() -> i32 { a + b; }`: the `;` drops the value.
                let last = match block.stmts.last() {
                    Some(HirStmt::Expr(e)) if e.ty == ret || literal_fits(ctx, e, &ret) => {
                        ctx.src(e.span).map(|t| (e.span, t))
                    }
                    _ => None,
                };
                if let Some((at, text)) = last {
                    ctx.emit_error_at(
                        Code::MISMATCHED_TYPES,
                        at,
                        format!(
                            "function `{}` returns `{}`, but the `;` after its last expression \
                             drops the value",
                            leaf_name(fn_name),
                            ctx.ty_name(&ret)
                        ),
                    );
                    ctx.help_last(Some(format!("remove the `;` to return it: `{text}`")));
                } else {
                    ctx.emit_error(Code::MISMATCHED_TYPES, falls_off());
                }
            }
        }
    }
    block
}

/// `1;` in `fn f() -> i32`: a number literal the return type would take.
fn literal_fits(ctx: &LowerCtx<'_>, e: &HirExpr, ret: &HirTy) -> bool {
    let ty = crate::lower::infer::zonk(ctx, &e.ty);
    let (HirExprKind::Const(_), HirTy::Prim(a), HirTy::Prim(b)) = (&e.kind, &ty, ret) else {
        return false;
    };
    a.is_int() && b.is_int() || a.is_float() && b.is_float()
}

/// Names `body` binds exactly once and writes exactly once as a path
/// after: a value in one has no other handle in this function (nested
/// closures and blocks count too).
pub(crate) fn sole_names(body: &nabic_ast::Block) -> nabic_core::HashSet<String> {
    #[derive(Default)]
    struct Count {
        binds: HashMap<String, u32>,
        paths: HashMap<String, u32>,
    }
    impl crate::walk::AstVisitor for Count {
        fn path(&mut self, p: &nabic_ast::Path) {
            for seg in &p.segments {
                *self.paths.entry(seg.name.clone()).or_default() += 1;
            }
        }
        fn bind(&mut self, name: &str) {
            *self.binds.entry(name.to_owned()).or_default() += 1;
        }
    }
    let mut c = Count::default();
    crate::walk::walk_block(&mut c, body);
    c.binds
        .into_iter()
        .filter(|(n, b)| *b == 1 && c.paths.get(n) == Some(&1))
        .map(|(n, _)| n)
        .collect()
}
