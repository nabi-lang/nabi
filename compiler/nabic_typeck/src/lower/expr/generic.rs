//! Generic calls: argument inference and hints.

#[allow(clippy::wildcard_imports)]
use crate::lower::expr::*;

pub(crate) fn call_generic(
    ctx: &mut LowerCtx<'_>,
    def: &FnDef,
    self_ty: Option<&Ty>,
    receiver: Option<HirExpr>,
    args: &[CallArg],
    callee: &str,
    span: nabic_diagnostic::Span,
) -> HirExpr {
    call_generic_on(ctx, def, self_ty, None, receiver, args, callee, span)
}

/// `call_generic` with `Self` known up front (`self_hint`): a generic
/// impl's static method binds the impl's parameters from it.
#[allow(clippy::too_many_arguments)]
pub(crate) fn call_generic_on(
    ctx: &mut LowerCtx<'_>,
    def: &FnDef,
    self_ty: Option<&Ty>,
    self_hint: Option<&HirTy>,
    receiver: Option<HirExpr>,
    args: &[CallArg],
    callee: &str,
    span: nabic_diagnostic::Span,
) -> HirExpr {
    if ctx.env.duplicated(def.span) {
        discard_args(ctx, args);
        return poison(span);
    }
    if args
        .iter()
        .all(|a| matches!(a, CallArg::Positional(_) | CallArg::Ref(_)))
    {
        return call_generic_positional(ctx, def, self_ty, self_hint, receiver, args, callee, span);
    }
    let mut lets = Vec::new();
    let offset = usize::from(receiver.is_some());
    let Some(args) = generic_positional_args(ctx, def, offset, args, callee, &mut lets) else {
        return poison(span);
    };
    let call = call_generic_positional(ctx, def, self_ty, self_hint, receiver, &args, callee, span);
    if lets.is_empty() {
        return call;
    }
    HirExpr {
        ty: call.ty.clone(),
        kind: HirExprKind::Block(Box::new(HirBlock {
            stmts: lets,
            trailing: Some(call),
            label: None,
        })),
        span,
    }
}

/// A generic call's named and spread arguments as positional ones:
/// skipped parameters take defaults, a spread tuple goes through a
/// temp in `lets`, a trailing closure fills the last parameter;
/// `offset` receiver slots first. `None` after an error.
pub(crate) fn generic_positional_args(
    ctx: &mut LowerCtx<'_>,
    def: &FnDef,
    offset: usize,
    args: &[CallArg],
    callee: &str,
    lets: &mut Vec<HirStmt>,
) -> Option<Vec<CallArg>> {
    let params = def.params.get(offset..).unwrap_or_default();
    let n = params.len();
    let mut slots: Vec<Option<CallArg>> = vec![None; n];
    let (mut next, mut extra) = (0usize, 0usize);
    // A variadic `...T` takes every argument past its slot (spread tuple
    // fields too); `call_generic_positional` wraps them.
    let variadic = params.last().is_some_and(|p| p.variadic);
    let mut rest: Vec<CallArg> = Vec::new();
    let mut ok = true;
    let mut dicts: Vec<(String, HirTy)> = Vec::new();
    let mut dict_lits: Vec<&Expr> = Vec::new();
    for (k, a) in args.iter().enumerate() {
        match a {
            CallArg::Positional(e)
                if !variadic && n > next && slots[n - 1].is_none() && is_trailing_closure(e) =>
            {
                slots[n - 1] = Some(a.clone());
                next = n;
            }
            CallArg::Positional(_) | CallArg::Ref(_) => {
                match slots.get_mut(next) {
                    Some(s) => *s = Some(a.clone()),
                    None if variadic => rest.push(a.clone()),
                    None => extra += 1,
                }
                next += 1;
            }
            CallArg::Named { name, value } => match params.iter().position(|p| p.name == *name) {
                Some(i) if slots[i].is_none() => {
                    slots[i] = Some(CallArg::Positional(value.clone()));
                }
                Some(_) => {
                    ok = false;
                    ctx.emit_error(
                        Code::NAMED_ARGUMENT,
                        format!("argument `{name}` provided more than once"),
                    );
                    ctx.help_last(Some(twice_help(name)));
                }
                None => {
                    ok = false;
                    ctx.emit_error(
                        Code::NAMED_ARGUMENT,
                        format!("`{callee}` has no parameter named `{name}`"),
                    );
                }
            },
            // In the variadic slot: `lower_variadic` expands it.
            CallArg::Spread(_) if variadic && next + 1 >= n => {
                match slots.get_mut(next) {
                    Some(s) => *s = Some(a.clone()),
                    None => rest.push(a.clone()),
                }
                next += 1;
            }
            CallArg::Spread(e) => {
                let h = lower_expected(ctx, e, HirTy::Unknown);
                let span = e.span;
                // A tuple's fields, an array's elements, or a `List` / `Slice` up to the
                // next argument (checked at run time).
                let count = match &h.ty {
                    HirTy::Tuple(parts) => parts.len(),
                    HirTy::Array(_, len) => *len as usize,
                    t if spreads_at_run_time(ctx, t) => {
                        let later = args[k + 1..]
                            .iter()
                            .filter(|a| !matches!(a, CallArg::Named { .. }))
                            .count();
                        let end = if variadic {
                            n - 1
                        } else {
                            n.saturating_sub(later)
                        };
                        let fill = end.saturating_sub(next);
                        warn_list_spread(ctx, e, fill);
                        fill
                    }
                    HirTy::Unknown => return None,
                    t if is_str_dict(ctx, t) => {
                        let name = format!("#spread{}", ctx.locals.len());
                        let vty = dict_value_ty(ctx, &h.ty);
                        let local = ctx.declare(Some(name.clone()), h.ty.clone());
                        lets.push(HirStmt::Let { local, init: h });
                        dict_lits.push(e);
                        dicts.push((name, vty));
                        continue;
                    }
                    other => {
                        ctx.emit_error(
                            Code::SPREAD_ARGUMENT,
                            format!(
                                "spread `f(..x)` needs a tuple, a `List`, a `Slice`, an array \
                                 or a `Dict<str, V>`, found `{}`",
                                ctx.ty_name(other)
                            ),
                        );
                        spread_range_help(ctx, e, other);
                        return None;
                    }
                };
                let tuple = matches!(h.ty, HirTy::Tuple(_));
                let run_time = spreads_at_run_time(ctx, &h.ty);
                let name = format!("#spread{}", ctx.locals.len());
                let local = ctx.declare(Some(name.clone()), h.ty.clone());
                lets.push(HirStmt::Let { local, init: h });
                if run_time {
                    let len = Expr {
                        kind: ExprKind::MethodCall {
                            receiver: Box::new(crate::lower::throws::path_expr(&name, span)),
                            method: "len".to_owned(),
                            type_args: Vec::new(),
                            args: Vec::new(),
                            trailing_closure: None,
                            origin: None,
                            written: false,
                        },
                        span,
                    };
                    let want = Expr {
                        kind: ExprKind::Literal(AstLit::Int(format!("{count}usize"))),
                        span,
                    };
                    let check = index_fn("spread_len", vec![len, want], span);
                    lets.push(HirStmt::Expr(lower_expected(ctx, &check, HirTy::Unit)));
                }
                for k in 0..count {
                    let field = if tuple {
                        Expr {
                            kind: ExprKind::TupleField(
                                Box::new(crate::lower::throws::path_expr(&name, span)),
                                u32::try_from(k).expect("tuple field overflow"),
                            ),
                            span,
                        }
                    } else {
                        spread_item(&name, k, span)
                    };
                    match slots.get_mut(next) {
                        Some(s) => *s = Some(CallArg::Positional(field)),
                        None if variadic => rest.push(CallArg::Positional(field)),
                        None => extra += 1,
                    }
                    next += 1;
                }
            }
        }
    }
    if extra > 0 {
        arity_error(ctx, callee, n, n + extra, Some(sig_of_def(callee, def)));
        return None;
    }
    let empty: Vec<_> = params
        .iter()
        .enumerate()
        .filter(|(i, p)| slots[*i].is_none() && !p.variadic)
        .map(|(i, p)| {
            let d = p
                .default
                .clone()
                .map(|d| in_module(&module_of(&def.name), d));
            (
                i,
                p.name.clone(),
                d,
                p.ty.as_ref().map_or(HirTy::Unknown, plain_param_ty),
            )
        })
        .collect();
    warn_dict_spreads(ctx, &dict_lits, &empty);
    let span = args
        .first()
        .map_or(nabic_diagnostic::Span::DUMMY, |a| match a {
            CallArg::Positional(e) | CallArg::Ref(e) | CallArg::Spread(e) => e.span,
            CallArg::Named { value, .. } => value.span,
        });
    for (i, e) in dict_spread(ctx, &dicts, empty, lets, span) {
        slots[i] = Some(CallArg::Positional(e));
    }
    let mut out = Vec::with_capacity(n);
    for (slot, p) in slots.into_iter().zip(params) {
        match (slot, &p.default) {
            (Some(a), _) => out.push(a),
            (None, _) if p.variadic => {}
            (None, Some(d)) => {
                out.push(CallArg::Positional(in_module(
                    &module_of(&def.name),
                    d.clone(),
                )));
            }
            (None, None) => {
                ctx.emit_error(
                    Code::WRONG_ARGUMENT_COUNT,
                    format!("missing argument `{}` in call to `{callee}`", p.name),
                );
                let sig = sig_of_def(callee, def);
                ctx.help_last(Some(format!(
                    "it is declared `{sig}`; pass `{}` too",
                    p.name
                )));
                ok = false;
            }
        }
    }
    out.extend(rest);
    ok.then_some(out)
}

/// An empty `[]` for an open `List<T>` waits for the other arguments to
/// bind `T` (`first_or([], 9)`), a parameter's `= []` default too.
pub(crate) fn empty_list_waits(e: &Expr, hint: &HirTy) -> bool {
    let e = match &e.kind {
        ExprKind::InModule(_, inner) => inner,
        _ => e,
    };
    matches!(&e.kind, ExprKind::List(v) if v.is_empty()) && crate::lower::mono::has_unknown(hint)
}

/// The type parameter parameter `i` of `def` is declared as (`f: F`).
fn type_param_of(def: &FnDef, i: usize) -> Option<&str> {
    let nabic_ast::TyKind::Path(p) = &def.params.get(i)?.ty.as_ref()?.kind else {
        return None;
    };
    let [seg] = p.segments.as_slice() else {
        return None;
    };
    def.generics
        .params
        .iter()
        .any(|g| matches!(g, nabic_ast::GenericParam::Type { name, .. } if *name == seg.name))
        .then_some(seg.name.as_str())
}

/// The trait bounds of type parameter `name` of `def`, inline and in
/// `where`, under `subst`.
fn param_bounds(
    ctx: &LowerCtx<'_>,
    def: &FnDef,
    subst: &HashMap<String, HirTy>,
    name: &str,
) -> Vec<TraitRef> {
    let module = module_of(&def.name);
    let inline = def.generics.params.iter().filter_map(|g| match g {
        nabic_ast::GenericParam::Type {
            name: n, bounds, ..
        } if n == name => Some(bounds),
        _ => None,
    });
    let wheres = def.where_clause.iter().filter_map(|w| {
        matches!(&w.ty.kind, nabic_ast::TyKind::Path(p)
            if p.segments.len() == 1 && p.segments[0].name == name)
        .then_some(&w.bounds)
    });
    inline
        .chain(wheres)
        .flatten()
        .filter_map(|b| {
            ctx.bound_ref(b, &module, |t| {
                crate::lower::mono::resolve_in_mono(ctx, def, subst, None, t)
            })
        })
        .collect()
}

/// `fn(..)` bounds for `wants`' auto traits (`F: Tr + Send`).
fn auto_bounds(ctx: &LowerCtx<'_>, wants: &[TraitRef]) -> crate::FnBounds {
    let mut auto: Vec<String> = wants
        .iter()
        .filter(|w| ctx.env.names.auto_traits.contains(w.path.as_str()))
        .map(|w| w.path.clone())
        .collect();
    auto.sort();
    auto.dedup();
    crate::FnBounds {
        auto,
        ..crate::FnBounds::default()
    }
}

/// `|x| ..` for a bare `F: Tr`: the one `fn` type `Tr` is implemented for
/// with that many parameters (`impl Integrand for fn(f64) -> f64`) types
/// the closure's parameters.
fn closure_hint(ctx: &LowerCtx<'_>, wants: &[TraitRef], e: &Expr) -> Option<HirTy> {
    let ExprKind::Closure { params, .. } = &e.kind else {
        return None;
    };
    if params.iter().all(|p| p.ty.is_some()) {
        return None;
    }
    let auto = |w: &&TraitRef| ctx.env.names.auto_traits.contains(w.path.as_str());
    let first = wants.iter().find(|w| !auto(w))?;
    let mut cands: Vec<HirTy> = Vec::new();
    for t in ctx.env.impls.tys_of(&first.path) {
        if let HirTy::Fn(a, r, _) = t {
            let plain = HirTy::Fn(a.clone(), r.clone(), auto_bounds(ctx, wants));
            if a.len() == params.len() && !cands.contains(&plain) {
                cands.push(plain);
            }
        }
    }
    cands.retain(|c| {
        wants
            .iter()
            .filter(|w| !auto(w))
            .all(|w| ctx.implements_trait(c, w, 0))
    });
    match <[HirTy; 1]>::try_from(cands) {
        Ok([one]) => Some(one),
        Err(_) => None,
    }
}

/// A closure for a bare `F: Tr` where `Tr` is implemented for its `fn`
/// type, not for the closure's own: the value as that `fn` type (an auto
/// bound checked against the captures, with what to write instead).
pub(crate) fn closure_into_bound(
    ctx: &mut LowerCtx<'_>,
    h: HirExpr,
    wants: &[TraitRef],
) -> HirExpr {
    let HirTy::Closure { args, ret, .. } = &h.ty else {
        return h;
    };
    if wants.iter().all(|w| ctx.implements_trait(&h.ty, w, 0)) {
        return h;
    }
    let auto = |w: &TraitRef| ctx.env.names.auto_traits.contains(w.path.as_str());
    let as_fn = HirTy::Fn(args.clone(), ret.clone(), auto_bounds(ctx, wants));
    let plain = HirTy::Fn(args.clone(), ret.clone(), crate::FnBounds::default());
    // Only for a trait the `fn` type has: `T: Send` alone asks the
    // captures, as the closure's own type answers.
    let mut traits = wants.iter().filter(|w| !auto(w)).peekable();
    if traits.peek().is_none() || !traits.all(|w| ctx.implements_trait(&plain, w, 0)) {
        return h;
    }
    coerce(ctx, &as_fn, h, &|| "the closure".to_owned())
}

/// Argument `i` of a generic call: a default with the callee's const
/// parameters, a closure for a bare `F: Tr` as its `fn` type.
fn lower_arg(
    ctx: &mut LowerCtx<'_>,
    def: &FnDef,
    subst: &HashMap<String, HirTy>,
    i: usize,
    e: &Expr,
    mut hint: HirTy,
) -> HirExpr {
    let open_param = (hint == HirTy::Unknown)
        .then(|| type_param_of(def, i))
        .flatten()
        .filter(|n| !subst.contains_key(*n))
        .map(str::to_owned);
    let wants = |ctx: &mut LowerCtx<'_>| {
        let n = open_param.as_deref()?;
        // A bound naming what is not known yet (`Item = Item`) says
        // nothing here; its errors are the body's.
        let mark = ctx.probe_mark();
        let w = param_bounds(ctx, def, subst, n);
        let clean = ctx.probe_clean(&mark);
        ctx.rollback(&mark);
        (clean && !w.is_empty()).then_some(w)
    };
    if matches!(e.kind, ExprKind::Closure { .. }) {
        if let Some(t) = wants(ctx).and_then(|w| closure_hint(ctx, &w, e)) {
            hint = t;
        }
    }
    let h = if is_default_of(def, i, e) {
        lower_default(ctx, def, subst, e, hint)
    } else {
        lower_expected(ctx, e, hint)
    };
    match matches!(h.ty, HirTy::Closure { .. })
        .then(|| wants(ctx))
        .flatten()
    {
        Some(w) => closure_into_bound(ctx, h, &w),
        None => h,
    }
}

/// `e` is parameter `i`'s default, filled in for an omitted argument.
fn is_default_of(def: &FnDef, i: usize, e: &Expr) -> bool {
    let ExprKind::InModule(_, inner) = &e.kind else {
        return false;
    };
    def.params
        .get(i)
        .and_then(|p| p.default.as_ref())
        .is_some_and(|d| d.span == inner.span)
}

/// A parameter default at a call: the callee's const parameters bound
/// so far are values in it (`axis: usize = N - 1`).
fn lower_default(
    ctx: &mut LowerCtx<'_>,
    def: &FnDef,
    subst: &HashMap<String, HirTy>,
    d: &Expr,
    hint: HirTy,
) -> HirExpr {
    let consts: Vec<(String, HirTy)> = def
        .generics
        .params
        .iter()
        .filter_map(|gp| match gp {
            nabic_ast::GenericParam::Const { name, ty } => {
                let v = subst.get(name).and_then(HirTy::as_const)?;
                crate::lower::mono::typed_const(ctx, name, ty, v).map(|c| (name.clone(), c))
            }
            nabic_ast::GenericParam::Type { .. } => None,
        })
        .collect();
    if consts.is_empty() {
        return lower_expected(ctx, d, hint);
    }
    let mut own = ctx.ty_subst.clone().into_owned();
    own.extend(consts);
    let saved = std::mem::replace(&mut ctx.ty_subst, std::borrow::Cow::Owned(own));
    let h = lower_expected(ctx, d, hint);
    ctx.ty_subst = saved;
    h
}

/// `call_generic_on` with positional (and `ref`) arguments only.
#[allow(clippy::too_many_arguments)]
pub(crate) fn call_generic_positional(
    ctx: &mut LowerCtx<'_>,
    def: &FnDef,
    self_ty: Option<&Ty>,
    self_hint: Option<&HirTy>,
    receiver: Option<HirExpr>,
    args: &[CallArg],
    callee: &str,
    span: nabic_diagnostic::Span,
) -> HirExpr {
    if def.is_unsafe {
        check_unsafe_call(ctx, callee);
    }
    template_marked(ctx, def, span);
    // A variadic call's trailing arguments become one `Slice<T>` once the
    // others are in (`lower_variadic`).
    let (args, tail) = match def.params.last().filter(|p| p.variadic) {
        Some(_) => {
            let k = (def.params.len() - 1).saturating_sub(usize::from(receiver.is_some()));
            args.split_at_checked(k)
                .map_or((args, None), |(a, t)| (a, Some(t)))
        }
        None => (args, None),
    };
    // A method's `Self` is its receiver's type.
    let recv_ty = receiver
        .as_ref()
        .filter(|_| self_hint.is_none() && self_ty.is_some())
        .map(|r| r.ty.clone());
    let self_hint = self_hint.or(recv_ty.as_ref());
    // A bound template's signature names `Self` (a type parameter).
    let hint_self = self_hint.filter(|_| ctx.env.bound_trait(&def.name).is_some());
    // Explicit `::<..>` arguments apply to this call only, not to the
    // generic calls inside its arguments.
    let explicit = std::mem::take(&mut ctx.explicit_generics);
    let mut subst: HashMap<String, HirTy> = explicit.clone();
    if let (Some(t), Some(hint)) = (self_ty, self_hint) {
        bind_self(ctx, def, t, hint, &mut subst);
    }
    // The slot the call's value goes to hints the arguments' types
    // (`let t: Task<i32> = spawn(|| 5)`); arguments still decide.
    let expected_subst = crate::lower::mono::expected_subst(ctx, def);
    let mut lowered: Vec<HirExpr> = Vec::with_capacity(args.len() + 1);
    let mut places: Vec<Option<HirExpr>> = vec![None; def.params.len()];
    let has_receiver = receiver.is_some();
    let explicit_recv = ctx.explicit_recv.take();
    if let Some(r) = receiver {
        let by_ref = def.params.first().is_some_and(|p| p.by_ref);
        check_path_receiver(ctx, explicit_recv, by_ref, &r, "self", callee);
        bind_from_arg(ctx, def, self_ty, 0, &r.ty, &mut subst);
        if let (Some(p), true) = (
            places.first_mut(),
            def.params.first().is_some_and(|p| p.by_ref),
        ) {
            crate::lower::expr::method::captured_copy_write(ctx, &r, callee);
            if !crate::lower::mono::writable_place(ctx, &r) {
                ctx.dropped_ref_recv.push(span);
            }
            *p = Some(r.clone());
        }
        lowered.push(r);
    }
    let mut reads: Vec<(String, Vec<String>)> = Vec::new();
    let mut deferred: Vec<(usize, &Expr)> = Vec::new();
    for a in args {
        let (CallArg::Positional(e) | CallArg::Ref(e)) = a else {
            unreachable!("named / spread arguments are made positional first")
        };
        let i = lowered.len();
        let is_ref = matches!(a, CallArg::Ref(_));
        let by_ref = def.params.get(i).is_some_and(|p| p.by_ref);
        if is_ref != by_ref {
            ref_mismatch(ctx, is_ref, i, callee, e);
        }
        if !is_ref {
            reads.extend(crate::walk::free_places(&[], e));
        }
        let mut hint = expected_arg_hint(ctx, def, &subst, &expected_subst, hint_self, i, e);
        if !bind_thrown(
            ctx,
            def,
            &mut subst,
            &expected_subst,
            hint_self,
            (i, e),
            callee,
            &mut hint,
        ) {
            return poison(span);
        }
        // A slot several bounds of the trait type differently: an unsuffixed
        // literal takes the one it fits.
        if let (HirTy::Unknown, Some(HirTy::Param(p))) = (&hint, hint_self) {
            let choices = crate::lower::bounds::slot_choices(ctx, def, *p, i);
            if !choices.is_empty() {
                match pick_literal_impl(ctx, e, &choices) {
                    Ok(Some(k)) => hint = choices[k].clone(),
                    Ok(None) => {}
                    Err(fits) => {
                        return ctx.error_expr(
                            Code::AMBIGUOUS_LITERAL,
                            span,
                            format!(
                                "`{callee}` on `{}`: the literal fits several of its bounds \
                                 ({fits}); add a suffix or `as`",
                                ctx.ty_name(&HirTy::Param(*p))
                            ),
                        );
                    }
                }
            }
        }
        // An untyped literal for an open `T` waits for a sibling argument of
        // the same bare `T` (`range(0, n)` with `n: i64` gives `i64`); with
        // none, the literal binds it (`fold(0, ..)`).
        let bare = |k: usize| {
            def.params
                .get(k)
                .and_then(|p| p.ty.as_ref())
                .and_then(|t| match &t.kind {
                    nabic_ast::TyKind::Path(p) if p.segments.len() == 1 => {
                        Some(p.segments[0].name.as_str())
                    }
                    _ => None,
                })
                .filter(|n| {
                    def.generics.params.iter().any(
                        |g| matches!(g, nabic_ast::GenericParam::Type { name, .. } if name == n),
                    )
                })
        };
        let offset = usize::from(has_receiver);
        let bare_param = bare(i).is_some_and(|n| {
            args.iter().enumerate().any(|(j, a)| {
                j + offset != i
                    && bare(j + offset) == Some(n)
                    && matches!(a, CallArg::Positional(x) if !is_untyped_literal(x))
            })
        });
        let literal = hint == HirTy::Unknown && bare_param && is_untyped_literal(e);
        if !is_ref && (literal || args.len() > 1 && empty_list_waits(e, &hint)) {
            deferred.push((i, e));
            lowered.push(poison(e.span));
            continue;
        }
        ctx.want_view = !matches!(hint, HirTy::Struct(_)) && view_param(def, i, e);
        // `f: F` with `F: Tr`: a closure meets `Tr` through its `fn` type.
        let h = lower_arg(ctx, def, &subst, i, e, hint);
        ctx.want_view = false;
        bind_from_arg(ctx, def, self_ty, i, &h.ty, &mut subst);
        if is_ref && crate::lower::mono::place_root(&h).is_some() {
            if let Some(p) = places.get_mut(i) {
                *p = Some(h.clone());
            }
        } else if is_ref && h.ty != HirTy::Unknown {
            ctx.emit_error(
                Code::REF_ARGUMENT,
                format!(
                    "`ref` argument {} of `{callee}` must name a variable or a field of one",
                    i + 1
                ),
            );
        }
        lowered.push(h);
    }
    for (i, e) in deferred {
        let hint = generic_arg_hint(ctx, def, &subst, hint_self, i, e);
        let h = lower_expected(ctx, e, hint);
        bind_from_arg(ctx, def, self_ty, i, &h.ty, &mut subst);
        lowered[i] = h;
    }
    // Missing trailing arguments take their declared defaults, lowered
    // at the call site like a non-generic call's (`ordering = SeqCst`).
    while let Some(d) = def
        .params
        .get(lowered.len())
        .and_then(|p| p.default.clone())
        .map(|d| in_module(&module_of(&def.name), d))
    {
        // The parameter's type as the arguments bound it (`xs: List<T> = []`).
        let hint = generic_arg_hint(ctx, def, &subst, hint_self, lowered.len(), &d);
        let h = lower_default(ctx, def, &subst, &d, hint);
        bind_from_arg(ctx, def, self_ty, lowered.len(), &h.ty, &mut subst);
        lowered.push(h);
    }
    if let Some(tail) = tail.filter(|_| lowered.len() + 1 == def.params.len()) {
        let i = lowered.len();
        let hint = generic_arg_hint(
            ctx,
            def,
            &subst,
            hint_self,
            i,
            &crate::lower::throws::path_expr("#", span),
        );
        let mut elem = variadic_elem(ctx, &hint);
        // `...[T; N]` with `N` unbound: the first array literal's length
        // is `N` (as for a `[T; N]` parameter).
        if let (HirTy::Unknown, Some(first)) = (&elem, tail.first()) {
            let slice_elem =
                def.params
                    .get(i)
                    .and_then(|p| p.ty.as_ref())
                    .and_then(|t| match &t.kind {
                        nabic_ast::TyKind::Path(p) => p.segments.last()?.generics.first(),
                        _ => None,
                    });
            if let (Some(t), CallArg::Positional(e)) = (slice_elem, first) {
                elem = ty_hint(ctx, def, &subst, hint_self, Some(t), e);
            }
        }
        let parts = tail
            .iter()
            .filter_map(|a| match a {
                CallArg::Positional(e) | CallArg::Ref(e) => Some(VarPart::Arg(e)),
                CallArg::Spread(e) => Some(VarPart::Spread(e)),
                CallArg::Named { .. } => None,
            })
            .collect();
        let h = lower_variadic(ctx, callee, elem, parts, span);
        bind_from_arg(ctx, def, self_ty, i, &h.ty, &mut subst);
        lowered.push(h);
    }
    check_ref_exclusive(ctx, callee, &lowered, &places, &reads, has_receiver);
    if lowered.len() != def.params.len() {
        let sig = sig_of_def(callee, def);
        arity_error(ctx, callee, def.params.len(), lowered.len(), Some(sig));
        return poison(span);
    }
    ctx.explicit_generics = explicit;
    let mono = monomorphize_on(ctx, def, self_ty, self_hint, &lowered, span);
    ctx.explicit_generics.clear();
    let Some((mono_id, ret_ty)) = mono else {
        return poison(span);
    };
    let param_tys = ctx
        .env
        .sink
        .mono_sigs
        .borrow()
        .get(&mono_id)
        .map(|s| s.param_tys.clone())
        .unwrap_or_default();
    let shown = ctx.display_path(callee);
    let lowered: Vec<HirExpr> = lowered
        .into_iter()
        .enumerate()
        .map(|(i, a)| match param_tys.get(i) {
            Some(t) => {
                let marks = ctx.env.sink.diagnostics.borrow().len();
                let (slot, have) = (t.clone(), a.ty.clone());
                let a = coerce(ctx, t, a, &|| format!("argument {} of `{shown}`", i + 1));
                if ctx.env.sink.diagnostics.borrow().len() > marks {
                    elem_arg_help(ctx, &slot, &have, &ret_ty, &a);
                }
                a
            }
            None => a,
        })
        .collect();
    let out = ctx.env.sink.ref_out.borrow().get(&mono_id).cloned();
    if let Some(out) = out {
        let tys: Vec<HirTy> = out.refs.iter().map(|(_, t)| t.clone()).collect();
        let call = HirExpr {
            kind: HirExprKind::Call {
                callee: mono_id,
                args: lowered,
            },
            ty: crate::lower::mono::ref_packed(&out.ret, &tys),
            span,
        };
        return crate::lower::mono::ref_unpack(ctx, call, &out, &places);
    }
    HirExpr {
        kind: HirExprKind::Call {
            callee: mono_id,
            args: lowered,
        },
        ty: ret_ty,
        span,
    }
}

/// Argument `e` is a slice (`xs[a:b]`, `xs[..]`) for parameter `i` of
/// `def`, declared `SliceMut<..>`.
pub(crate) fn view_param(def: &FnDef, i: usize, e: &Expr) -> bool {
    let slice = match &e.kind {
        ExprKind::Slice { step, .. } => step.is_none(),
        ExprKind::Index(_, idx) => matches!(
            idx.kind,
            ExprKind::Range {
                inclusive: false,
                ..
            }
        ),
        _ => false,
    };
    slice
        && def
            .params
            .get(i)
            .and_then(|p| p.ty.as_ref())
            .is_some_and(|t| match &t.kind {
                AstTyKind::Path(p) => p.segments.last().is_some_and(|s| s.name == "SliceMut"),
                _ => false,
            })
}

/// The expected type of argument `i` of a generic call, under the
/// parameters bound so far.
pub(crate) fn generic_arg_hint(
    ctx: &LowerCtx<'_>,
    def: &FnDef,
    subst: &HashMap<String, HirTy>,
    self_ty: Option<&HirTy>,
    i: usize,
    e: &Expr,
) -> HirTy {
    ty_hint(
        ctx,
        def,
        subst,
        self_ty,
        def.params.get(i).and_then(|p| p.ty.as_ref()),
        e,
    )
}

/// The `X` of parameter `i`'s `IntoIterator<Item = X>` /
/// `Iterator<Item = X>` bound, when its type is a bare type parameter.
pub(crate) fn iter_item_bound(def: &FnDef, i: usize) -> Option<&Ty> {
    let nabic_ast::TyKind::Path(p) = &def.params.get(i)?.ty.as_ref()?.kind else {
        return None;
    };
    let [seg] = p.segments.as_slice() else {
        return None;
    };
    def.generics.params.iter().find_map(|gp| match gp {
        nabic_ast::GenericParam::Type { name, bounds, .. } if *name == seg.name => {
            bounds.iter().find_map(|b| {
                let nabic_ast::TyKind::Path(bp) = &b.kind else {
                    return None;
                };
                let last = bp.segments.last()?;
                if !matches!(last.name.as_str(), "IntoIterator" | "Iterator") {
                    return None;
                }
                last.generics.iter().find_map(|g| match &g.kind {
                    nabic_ast::TyKind::Binding(n, t) if n == "Item" => Some(&**t),
                    _ => None,
                })
            })
        }
        _ => None,
    })
}

/// `generic_arg_hint`, else with the parameters the call's slot fixes
/// (`expected_subst`) where the other arguments have not.
pub(crate) fn expected_arg_hint(
    ctx: &LowerCtx<'_>,
    def: &FnDef,
    subst: &HashMap<String, HirTy>,
    expected: &HashMap<String, HirTy>,
    self_ty: Option<&HirTy>,
    i: usize,
    e: &Expr,
) -> HirTy {
    let hint = generic_arg_hint(ctx, def, subst, self_ty, i, e);
    if hint != HirTy::Unknown {
        return hint;
    }
    // A list literal for `I: IntoIterator<Item = X>`: a `List<X>`
    // (`xs.extend([1, 2])` on a `List<i32>`).
    if let Some(item) = matches!(e.kind, ExprKind::List(_))
        .then(|| iter_item_bound(def, i))
        .flatten()
    {
        let list = Ty {
            kind: nabic_ast::TyKind::Path(nabic_ast::Path::new(
                vec![nabic_ast::PathSegment {
                    name: "List".to_owned(),
                    generics: vec![item.clone()],
                }],
                item.span,
            )),
            span: item.span,
        };
        let h = ty_hint(ctx, def, subst, self_ty, Some(&list), e);
        if !matches!(&h, HirTy::Struct(id) if ctx.struct_def(*id).generic_args.iter().any(|a| matches!(a, HirTy::Path(_))))
        {
            return h;
        }
    }
    if expected.is_empty() {
        return hint;
    }
    let mut merged = expected.clone();
    merged.extend(subst.iter().map(|(k, v)| (k.clone(), v.clone())));
    generic_arg_hint(ctx, def, &merged, self_ty, i, e)
}

/// The expected type of argument `e` for a parameter of `def` typed
/// `param` (see `generic_arg_hint`).
pub(crate) fn ty_hint(
    ctx: &LowerCtx<'_>,
    def: &FnDef,
    subst: &HashMap<String, HirTy>,
    self_ty: Option<&HirTy>,
    param: Option<&Ty>,
    e: &Expr,
) -> HirTy {
    let hint = param.map_or(HirTy::Unknown, |t| {
        // Type params not bound yet stay opaque paths, never resolved as
        // same-named types visible from `def`.
        let mut partial = subst.clone();
        for gp in &def.generics.params {
            match gp {
                nabic_ast::GenericParam::Type { name, .. } => {
                    partial
                        .entry(name.clone())
                        .or_insert_with(|| HirTy::Path(vec![name.clone()]));
                }
                // An unbound const parameter: `#N` (no value yet).
                nabic_ast::GenericParam::Const { name, .. } => {
                    partial
                        .entry(name.clone())
                        .or_insert_with(|| HirTy::OpenConst(name.clone()));
                }
            }
        }
        let mut ty = resolve_in_mono(ctx, def, &partial, self_ty.cloned(), t);
        erase_unbound_errors(&mut ty);
        let mut leftover = Vec::new();
        collect_unknown_paths(&ty, &mut leftover);
        let open = |t: &HirTy| {
            let mut l = Vec::new();
            collect_unknown_paths(t, &mut l);
            !l.is_empty()
        };
        match ty {
            _ if leftover.is_empty() => ty,
            // `fn(T) -> U` with `U` open: the closure learns its parameters;
            // `U` binds from it afterwards.
            HirTy::Fn(args, ret, b) => HirTy::Fn(
                args.into_iter()
                    .map(|a| if open(&a) { HirTy::Unknown } else { a })
                    .collect(),
                Box::new(if open(&ret) { HirTy::Unknown } else { *ret }),
                b,
            ),
            _ => HirTy::Unknown,
        }
    });
    // `[T; N]` with `N` open and an array literal: the literal's length,
    // not a `List` default.
    match (&hint, param, &e.kind) {
        (HirTy::Unknown, Some(t), ExprKind::List(items)) => match &t.kind {
            nabic_ast::TyKind::Array(elem, _) => {
                let mut et = resolve_in_mono(ctx, def, subst, None, elem);
                let mut open = Vec::new();
                collect_unknown_paths(&et, &mut open);
                if !open.is_empty() {
                    et = HirTy::Unknown;
                }
                u32::try_from(items.len()).map_or(hint, |n| HirTy::Array(Box::new(et), n))
            }
            nabic_ast::TyKind::Path(p) => {
                nd_literal_hint(ctx, def, subst, self_ty, t, p, e).unwrap_or(hint)
            }
            _ => hint,
        },
        _ => hint,
    }
}

/// `Array<T, N>` with `N` open and a list literal: the rank is the
/// literal's nesting depth (`[[1.0], [2.0]]` is rank 2).
fn nd_literal_hint(
    ctx: &LowerCtx<'_>,
    def: &FnDef,
    subst: &HashMap<String, HirTy>,
    self_ty: Option<&HirTy>,
    t: &Ty,
    p: &nabic_ast::Path,
    e: &Expr,
) -> Option<HirTy> {
    let [_, rank] = p.segments.last()?.generics.as_slice() else {
        return None;
    };
    let name = match &rank.kind {
        nabic_ast::TyKind::Path(r) => match r.segments.as_slice() {
            [seg] if seg.generics.is_empty() => seg.name.as_str(),
            _ => return None,
        },
        nabic_ast::TyKind::Const(nabic_ast::ConstExpr::Path(r)) => match r.segments.as_slice() {
            [seg] => seg.name.as_str(),
            _ => return None,
        },
        _ => return None,
    };
    let is_const = def
        .generics
        .params
        .iter()
        .any(|g| matches!(g, nabic_ast::GenericParam::Const { name: n, .. } if n == name));
    if !is_const || subst.contains_key(name) {
        return None;
    }
    let depth = list_depth(e);
    let mut bound = subst.clone();
    bound.insert(
        name.to_owned(),
        HirTy::Const(i128::from(depth), Some(PrimTy::Usize)),
    );
    let ty = ty_hint(ctx, def, &bound, self_ty, Some(t), e);
    let nd = ctx.lang("ndarray_type");
    match &ty {
        HirTy::Struct(id) if !nd.is_empty() && enum_base_name(&ctx.struct_def(*id).name) == nd => {
            Some(ty)
        }
        _ => None,
    }
}

/// How many `[..]` levels a list literal nests, following first elements.
pub(crate) fn list_depth(e: &Expr) -> u32 {
    let mut depth = 0;
    let mut cur = e;
    while let ExprKind::List(items) = &cur.kind {
        depth += 1;
        match items.first() {
            Some(first) => cur = first,
            None => break,
        }
    }
    depth
}

/// `f::<T, 3>(..)`: stash explicit arguments in `ctx.explicit_generics` for
/// the next generic call; `false` after reporting too many.
pub(crate) fn bind_explicit_generics(
    ctx: &mut LowerCtx<'_>,
    def: &FnDef,
    explicit: &[nabic_ast::Ty],
    full: &str,
) -> bool {
    // A `some` parameter's type has no name to give.
    let named: Vec<&nabic_ast::GenericParam> = def
        .generics
        .params
        .iter()
        .filter(|gp| {
            let (nabic_ast::GenericParam::Type { name, .. }
            | nabic_ast::GenericParam::Const { name, .. }) = gp;
            !name.starts_with(nabic_ast::SOME_PARAM_PREFIX)
        })
        .collect();
    if explicit.len() > named.len() {
        ctx.emit_error(
            Code::GENERIC_ARGUMENTS,
            crate::lower::mono::arity_msg(
                leaf_name(full),
                named.len(),
                named.len(),
                explicit.len(),
            ),
        );
        ctx.help_last(some_params_note(def));
        return false;
    }
    // Seeded by a literal handler call (`lower_magic`); empty otherwise.
    let mut bound = std::mem::take(&mut ctx.explicit_generics);
    for (gp, t) in named.into_iter().zip(explicit) {
        let (nabic_ast::GenericParam::Type { name, .. }
        | nabic_ast::GenericParam::Const { name, .. }) = gp;
        bound.insert(name.clone(), turbofish_ty(ctx, t));
    }
    ctx.explicit_generics = bound;
    true
}

/// `g(..5)` spreads `5`; a range argument needs parentheses.
pub(crate) fn spread_range_help(ctx: &LowerCtx<'_>, e: &Expr, ty: &HirTy) {
    if matches!(crate::lower::infer::zonk(ctx, ty), HirTy::Prim(p) if p.is_int()) {
        ctx.help_last(Some(format!(
            "to pass the range `..{0}`, write it in parentheses: `(..{0})`",
            snippet(e)
        )));
    }
}

/// A `::<T>` argument; a name that resolves to no type is E0028 here,
/// since nothing later sees it (`size_of::<Nope>()`).
pub(crate) fn turbofish_ty(ctx: &LowerCtx<'_>, t: &Ty) -> HirTy {
    let ty = ctx.resolve_ast_ty(t);
    let mut open = Vec::new();
    crate::lower::collect::collect_unknown_paths(&ty, &mut open);
    let Some(name) = open.into_iter().find(|n| {
        n != "_"
            && !ctx.ty_subst.contains_key(n)
            && ctx.env.names.unknown_spans.borrow().contains_key(n)
    }) else {
        return ty;
    };
    let at = ctx.env.names.unknown_spans.borrow()[&name];
    ctx.emit_error_at(
        Code::UNRESOLVED_TYPE,
        at,
        format!("cannot find type `{name}` in this scope"),
    );
    ctx.help_last(crate::lower::habits::type_help(
        &ctx.current_module,
        &name,
        None,
    ));
    HirTy::Unknown
}

/// Argument `i` (`e`) of `callee` has `ref` and its parameter not
/// (`is_ref`), or the other way round.
fn ref_mismatch(ctx: &LowerCtx<'_>, is_ref: bool, i: usize, callee: &str, e: &Expr) {
    ctx.emit_error(
        Code::REF_ARGUMENT,
        if is_ref {
            format!(
                "argument {} of `{callee}` is passed with `ref`, but the parameter is not `ref`",
                i + 1
            )
        } else {
            missing_ref(&(i + 1).to_string(), callee, e)
        },
    );
}

/// `thrown_param` for argument `i` (`e`): bind the parameter and redo the
/// argument's hint, or report it; `false` after an error.
#[allow(clippy::too_many_arguments)]
fn bind_thrown(
    ctx: &mut LowerCtx<'_>,
    def: &FnDef,
    subst: &mut HashMap<String, HirTy>,
    expected_subst: &HashMap<String, HirTy>,
    self_ty: Option<&HirTy>,
    (i, e): (usize, &Expr),
    callee: &str,
    hint: &mut HirTy,
) -> bool {
    match thrown_param(ctx, def, subst, self_ty, i, e, hint) {
        Thrown::Bound(name, err) => {
            subst.insert(name, err);
            *hint = expected_arg_hint(ctx, def, subst, expected_subst, self_ty, i, e);
            true
        }
        Thrown::None(name) => {
            unthrown_param_error(ctx, def, subst, i, e, callee, &name);
            false
        }
        Thrown::Skip => true,
    }
}

/// What a closure argument's thrown error says of a type parameter.
enum Thrown {
    Bound(String, HirTy),
    /// The closure throws nothing, and nothing else names the parameter.
    None(String),
    Skip,
}

/// `try_scope(|s| ..)`: parameter `i` is `fn(.. E ..) throws (E)` with `E`
/// open, so the closure's parameters wait for `E`, which only the error
/// its body throws decides. A probe lowering of the body finds it.
fn thrown_param(
    ctx: &mut LowerCtx<'_>,
    def: &FnDef,
    subst: &HashMap<String, HirTy>,
    self_ty: Option<&HirTy>,
    i: usize,
    e: &Expr,
    hint: &HirTy,
) -> Thrown {
    let ExprKind::Closure { params, .. } = &e.kind else {
        return Thrown::Skip;
    };
    let waiting = |a: &HirTy| *a == HirTy::Unknown;
    let HirTy::Fn(args, ..) = hint else {
        return Thrown::Skip;
    };
    if !params.iter().any(|p| p.ty.is_none()) || !args.iter().any(waiting) {
        return Thrown::Skip;
    }
    let Some(param) = def.params.get(i).and_then(|p| p.ty.as_ref()) else {
        return Thrown::Skip;
    };
    let AstTyKind::Fn(_, ret, _) = &param.kind else {
        return Thrown::Skip;
    };
    let AstTyKind::Throws { err, .. } = &ret.kind else {
        return Thrown::Skip;
    };
    let AstTyKind::Path(p) = &err.kind else {
        return Thrown::Skip;
    };
    let name = match p.segments.as_slice() {
        [s] if s.generics.is_empty() => s.name.clone(),
        _ => return Thrown::Skip,
    };
    let generic = def
        .generics
        .params
        .iter()
        .any(|g| matches!(g, nabic_ast::GenericParam::Type { name: n, .. } if *n == name));
    if !generic || subst.contains_key(&name) {
        return Thrown::Skip;
    }
    let mut partial = subst.clone();
    partial.insert(name.clone(), HirTy::Unknown);
    let probe_hint = ty_hint(ctx, def, &partial, self_ty, Some(param), e);
    let mark = ctx.probe_mark();
    let h = lower_expected(ctx, e, probe_hint);
    ctx.rollback(&mark);
    match &h.ty {
        HirTy::Fn(_, ret, _) | HirTy::Closure { ret, .. } => match &**ret {
            HirTy::Throws { err, .. } if !matches!(**err, HirTy::Unknown | HirTy::NoThrow) => {
                Thrown::Bound(name, (**err).clone())
            }
            // Another argument may still bind it.
            _ if def.params.len() > 1 => Thrown::Skip,
            _ => Thrown::None(name),
        },
        _ => Thrown::Skip,
    }
}

/// The closure argument of `try_scope(|s| ..)` throws nothing, so its
/// error type `name` is unknown.
fn unthrown_param_error(
    ctx: &LowerCtx<'_>,
    def: &FnDef,
    subst: &HashMap<String, HirTy>,
    idx: usize,
    arg: &Expr,
    callee: &str,
    name: &str,
) {
    // As a module-qualified call: `par::try_scope`.
    let segs: Vec<&str> = ctx
        .env
        .names
        .visible_path(callee)
        .rsplitn(3, "::")
        .collect();
    let (callee, module) = match segs.as_slice() {
        [leaf, m, ..] => (format!("{m}::{leaf}"), Some(*m)),
        _ => (ctx.display_path(callee), None),
    };
    let ExprKind::Closure { params, .. } = &arg.kind else {
        return;
    };
    // The parameter's type as the library writes it, `E` named.
    let param = match def
        .params
        .get(idx)
        .and_then(|p| p.ty.as_ref())
        .map(|t| &t.kind)
    {
        Some(AstTyKind::Fn(args, ..)) => args.first(),
        _ => None,
    };
    let annotated = match (param, params.first()) {
        (Some(ty), Some(first)) if first.ty.is_none() && subst.is_empty() => {
            let shown = rename_word(&nabic_ast::print::print_ty(ty), name, "MyError");
            let shown = match (module, &ty.kind) {
                (Some(m), AstTyKind::Path(path)) if path.segments.len() == 1 => {
                    format!("{m}::{shown}")
                }
                _ => shown,
            };
            format!(
                ", or annotate the parameter: `|{}: {shown}| ..`",
                first.name
            )
        }
        _ => String::new(),
    };
    ctx.emit_error_at(
        Code::TYPE_ANNOTATIONS_NEEDED,
        arg.span,
        format!(
            "cannot infer `{name}` of `{callee}`: it is the error type the closure throws, and \
             this closure throws none"
        ),
    );
    ctx.help_last(Some(format!(
        "name the error type: `{callee}::<MyError>(..)`{annotated}"
    )));
}

/// `text` with each whole word `from` written `to`.
fn rename_word(text: &str, from: &str, to: &str) -> String {
    let mut out = String::new();
    let mut word = String::new();
    for c in text.chars().map(Some).chain([None]) {
        match c {
            Some(c) if c.is_alphanumeric() || c == '_' => word.push(c),
            _ => {
                out.push_str(if word == from { to } else { &word });
                word.clear();
                out.extend(c);
            }
        }
    }
    out
}

/// A number argument of another type than the element type a generic
/// result takes from the expected type (`let v: Vec3 = Vec3::new(x, ..)`
/// with `x: f64`): convert it, or make the result the other instance.
fn elem_arg_help(ctx: &LowerCtx<'_>, slot: &HirTy, have: &HirTy, ret: &HirTy, a: &HirExpr) {
    let (HirTy::Prim(to), HirTy::Prim(from)) = (slot, have) else {
        return;
    };
    let num = |p: &PrimTy| p.is_int() || p.is_float();
    let HirTy::Struct(id) = crate::lower::infer::zonk(ctx, ret) else {
        return;
    };
    let d = ctx.struct_def(id);
    if !num(to) || !num(from) || !d.generic_args.contains(slot) {
        return;
    }
    let Some(src) = crate::lower::suggest::short_src(ctx, a.span) else {
        return;
    };
    let leaf = leaf_name(enum_base_name(&d.name));
    let other: Vec<String> = d
        .generic_args
        .iter()
        .map(|g| {
            if g == slot {
                from.name().to_owned()
            } else {
                ctx.ty_name(g)
            }
        })
        .collect();
    let help = format!(
        "Nabi never converts numbers implicitly; convert it: `{src} as {}`, or make the value a \
         `{leaf}<{}>`",
        to.name(),
        other.join(", ")
    );
    ctx.with_last(|d| {
        d.notes
            .retain(|n| !n.starts_with("help: convert it with `as`"));
        d.notes.push(format!("help: {help}"));
    });
}
