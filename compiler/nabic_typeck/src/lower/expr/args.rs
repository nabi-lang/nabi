//! Call arguments: named, spread, variadic and arity.

#[allow(clippy::wildcard_imports)]
use crate::lower::expr::*;

thread_local! {
    /// Spans of trailing closures: they fill the last parameter, past
    /// defaulted or named ones.
    static TRAILING_CLOSURES: std::cell::RefCell<nabic_core::HashSet<(u32, u32)>> =
        std::cell::RefCell::default();
}

pub(crate) fn is_trailing_closure(e: &Expr) -> bool {
    TRAILING_CLOSURES.with(|t| t.borrow().contains(&(e.span.file, e.span.start)))
}

/// `args` plus the trailing closure as the last positional argument.
pub(crate) fn with_trailing_closure(args: &[CallArg], tc: Option<&Expr>) -> Vec<CallArg> {
    let mut all = args.to_vec();
    if let Some(tc) = tc {
        TRAILING_CLOSURES.with(|t| t.borrow_mut().insert((tc.span.file, tc.span.start)));
        all.push(CallArg::Positional(tc.clone()));
    }
    all
}

/// Lower arguments against a signature: named ones to their slot, tuple
/// spreads to consecutive slots, missing trailing ones to defaults, each
/// checked. `receiver` fills slot 0.
pub(crate) fn lower_args_for(
    ctx: &mut LowerCtx<'_>,
    sig: &FnSig,
    callee: &str,
    receiver: Option<HirExpr>,
    args: &[CallArg],
    span: nabic_diagnostic::Span,
) -> (Vec<HirExpr>, Vec<HirStmt>) {
    let (out, lets, _) = lower_args_ref(ctx, sig, callee, receiver, args, span);
    (out, lets)
}

/// `lower_args_for`, also returning each `ref` parameter's argument
/// place (by parameter index) for the copy-out.
pub(crate) fn lower_args_ref(
    ctx: &mut LowerCtx<'_>,
    sig: &FnSig,
    callee: &str,
    receiver: Option<HirExpr>,
    args: &[CallArg],
    span: nabic_diagnostic::Span,
) -> (Vec<HirExpr>, Vec<HirStmt>, Vec<Option<HirExpr>>) {
    // A trailing closure fills the last parameter only; for a function
    // parameter elsewhere, its mismatches say little.
    let fn_like = |t: &HirTy| {
        matches!(
            t,
            HirTy::Fn(..)
                | HirTy::Closure { .. }
                | HirTy::Unknown
                | HirTy::Param(_)
                | HirTy::Var(_)
                | HirTy::Path(_)
                | HirTy::AnyTrait(_)
        )
    };
    let misplaced = !sig.variadic
        && matches!(args.last(), Some(CallArg::Positional(e)) if is_trailing_closure(e))
        && sig.param_tys.last().is_some_and(|t| !fn_like(t))
        && sig
            .param_tys
            .iter()
            .any(|t| matches!(t, HirTy::Fn(..) | HirTy::Closure { .. }));
    let since = ctx.env.sink.diagnostics.borrow().len();
    let out = lower_args_ref_inner(ctx, sig, callee, receiver, args, span);
    if misplaced {
        retain_since(ctx, since, |d| {
            !matches!(
                d.code,
                Code::MISMATCHED_TYPES | Code::TYPE_ANNOTATIONS_NEEDED
            )
        });
        let last = sig.param_names.last().cloned().unwrap_or_default();
        ctx.emit_error_at(
            Code::MISMATCHED_TYPES,
            span,
            format!(
                "a trailing closure fills the last parameter, `{last}`, which is not a \
                 function"
            ),
        );
        ctx.help_last(Some(
            "pass the closure inside the parentheses, in its parameter's position".to_owned(),
        ));
    }
    out
}

#[allow(clippy::cognitive_complexity)]
pub(crate) fn lower_args_ref_inner(
    ctx: &mut LowerCtx<'_>,
    sig: &FnSig,
    callee: &str,
    receiver: Option<HirExpr>,
    args: &[CallArg],
    span: nabic_diagnostic::Span,
) -> (Vec<HirExpr>, Vec<HirStmt>, Vec<Option<HirExpr>>) {
    let has_receiver = receiver.is_some();
    let explicit_recv = ctx.explicit_recv.take();
    let n = sig.param_tys.len();
    let mut lets: Vec<HirStmt> = Vec::new();
    let mut slots: Vec<Option<ArgSrc<'_>>> = (0..n).map(|_| None).collect();
    let mut next = 0usize;
    let mut extra = 0usize;
    let mut c_varargs: Vec<HirExpr> = Vec::new();
    if let Some(r) = receiver {
        if n > 0 {
            slots[0] = Some(ArgSrc::Lowered(r));
        }
        next = 1;
    }
    // A variadic `...T` (the last slot) takes every argument from its
    // position on, spreads included.
    let var_at = if sig.variadic { n - 1 } else { usize::MAX };
    let mut var_parts: Vec<VarPart<'_>> = Vec::new();
    // Slot -> its argument's written position.
    let mut written: Vec<Option<usize>> = vec![None; n];
    // `..d` with `d: Dict<str, V>`: its temp and written position.
    let mut dicts: Vec<(String, usize, HirTy)> = Vec::new();
    let mut dict_lits: Vec<&Expr> = Vec::new();
    for (k, a) in args.iter().enumerate() {
        match a {
            CallArg::Positional(e) | CallArg::Ref(e) | CallArg::Spread(e) if next >= var_at => {
                var_parts.push(if matches!(a, CallArg::Spread(_)) {
                    VarPart::Spread(e)
                } else {
                    VarPart::Arg(e)
                });
                next += 1;
            }
            CallArg::Positional(e)
                if !sig.variadic
                    && n > next
                    && slots[n - 1].is_none()
                    && is_trailing_closure(e) =>
            {
                slots[n - 1] = Some(ArgSrc::Ast(e));
                written[n - 1] = Some(k);
                next = n;
            }
            CallArg::Positional(e) | CallArg::Ref(e) => {
                if next < n {
                    written[next] = Some(k);
                    slots[next] = Some(if matches!(a, CallArg::Ref(_)) {
                        ArgSrc::Ref(e)
                    } else {
                        ArgSrc::Ast(e)
                    });
                } else {
                    // An unsuffixed integer is a C `int` there, a float a `double`.
                    let want = if is_untyped_int(e) {
                        HirTy::Prim(PrimTy::I32)
                    } else if is_untyped_literal(e) {
                        HirTy::Prim(PrimTy::F64)
                    } else {
                        HirTy::Unknown
                    };
                    c_varargs.push(lower_expected(ctx, e, want));
                }
                next += 1;
            }
            CallArg::Named { name, value } => {
                match sig.param_names.iter().position(|p| p == name) {
                    Some(i) if slots[i].is_none() => {
                        slots[i] = Some(ArgSrc::Ast(value));
                        written[i] = Some(k);
                    }
                    Some(i) => {
                        let p = sig.param_names.get(i).map_or(name.as_str(), String::as_str);
                        ctx.emit_error(
                            Code::NAMED_ARGUMENT,
                            format!("argument `{p}` provided more than once"),
                        );
                        ctx.help_last(Some(twice_help(p)));
                    }
                    None => ctx.emit_error(
                        Code::NAMED_ARGUMENT,
                        format!("`{callee}` has no parameter named `{name}`"),
                    ),
                }
            }
            CallArg::Spread(e) => {
                // `f(..t)` with a tuple value. A list of literals takes the next
                // parameter's type (`add(..[1, 2])`).
                let hint = match (&e.kind, sig.param_tys.get(next)) {
                    (ExprKind::List(_), Some(t @ HirTy::Prim(_))) => ctx
                        .instantiate_struct(LIST, vec![t.clone()], ctx.here())
                        .map_or(HirTy::Unknown, HirTy::Struct),
                    _ => HirTy::Unknown,
                };
                let lh = lower_expected(ctx, e, hint);
                match lh.ty.clone() {
                    HirTy::Tuple(parts) => {
                        // Evaluate the tuple once into a temp, then read its fields.
                        let tuple_ty = lh.ty.clone();
                        let tmp = ctx.declare(None, tuple_ty.clone());
                        lets.push(HirStmt::Let {
                            local: tmp,
                            init: lh,
                        });
                        for (i, part_ty) in parts.iter().enumerate() {
                            let field = HirExpr {
                                kind: HirExprKind::TupleField {
                                    target: Box::new(hir_local(tmp, &tuple_ty, span)),
                                    index: u32::try_from(i).expect("tuple field overflow"),
                                    field_ty: part_ty.clone(),
                                },
                                ty: part_ty.clone(),
                                span,
                            };
                            if next >= var_at {
                                var_parts.push(VarPart::Value(field));
                            } else if next < n {
                                slots[next] = Some(ArgSrc::Lowered(field));
                            } else {
                                extra += 1;
                            }
                            next += 1;
                        }
                    }
                    HirTy::Unknown => {}
                    // An array fills its length's slots; a `List` / `Slice` fills up to the
                    // next argument, checked at run time.
                    ty @ HirTy::Array(..) => {
                        let HirTy::Array(_, len) = ty else {
                            unreachable!()
                        };
                        let name = format!("#spread{}", ctx.locals.len());
                        let local = ctx.declare(Some(name.clone()), ty);
                        lets.push(HirStmt::Let { local, init: lh });
                        for i in 0..len {
                            let item = spread_item(&name, i as usize, span);
                            if next >= var_at {
                                let v = lower_expected(ctx, &item, HirTy::Unknown);
                                var_parts.push(VarPart::Value(v));
                            } else if next < n {
                                let v = lower_expected(ctx, &item, sig.param_tys[next].clone());
                                slots[next] = Some(ArgSrc::Lowered(v));
                            } else {
                                extra += 1;
                            }
                            next += 1;
                        }
                    }
                    ty if spreads_at_run_time(ctx, &ty) => {
                        let later = args[k + 1..]
                            .iter()
                            .filter(|a| !matches!(a, CallArg::Named { .. }))
                            .count();
                        let end = if sig.variadic {
                            var_at
                        } else {
                            n.saturating_sub(later)
                        };
                        let fill = end.saturating_sub(next);
                        warn_list_spread(ctx, e, fill);
                        let name = format!("#spread{}", ctx.locals.len());
                        let local = ctx.declare(Some(name.clone()), ty);
                        lets.push(HirStmt::Let { local, init: lh });
                        let path = crate::lower::throws::path_expr(&name, span);
                        let len = Expr {
                            kind: ExprKind::MethodCall {
                                receiver: Box::new(path),
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
                            kind: ExprKind::Literal(AstLit::Int(format!("{fill}usize"))),
                            span,
                        };
                        let check = index_fn("spread_len", vec![len, want], span);
                        lets.push(HirStmt::Expr(lower_expected(ctx, &check, HirTy::Unit)));
                        for i in 0..fill {
                            let item = spread_item(&name, i, span);
                            let v = lower_expected(ctx, &item, sig.param_tys[next].clone());
                            slots[next] = Some(ArgSrc::Lowered(v));
                            next += 1;
                        }
                    }
                    ty if is_str_dict(ctx, &ty) => {
                        let name = format!("#spread{}", ctx.locals.len());
                        let v = dict_value_ty(ctx, &ty);
                        let local = ctx.declare(Some(name.clone()), ty);
                        lets.push(HirStmt::Let { local, init: lh });
                        dict_lits.push(e);
                        dicts.push((name, k, v));
                    }
                    other => {
                        ctx.emit_error(
                            Code::SPREAD_ARGUMENT,
                            format!(
                                "spread `f(..x)` needs a tuple, a `List`, a `Slice`, an array or \
                                 a `Dict<str, V>`, found `{}`",
                                ctx.ty_name(&other)
                            ),
                        );
                        spread_range_help(ctx, e, &other);
                    }
                }
            }
        }
    }
    let end = if sig.variadic { n - 1 } else { n };
    let empty: Vec<_> = (0..end)
        .filter(|i| slots[*i].is_none())
        .map(|i| {
            let d = sig.param_defaults.get(i).cloned().flatten();
            (i, sig.param_names[i].clone(), d, sig.param_tys[i].clone())
        })
        .collect();
    let names: Vec<(String, HirTy)> = dicts.iter().map(|d| (d.0.clone(), d.2.clone())).collect();
    warn_dict_spreads(ctx, &dict_lits, &empty);
    let mut dict_slots = Vec::new();
    for (i, e) in dict_spread(ctx, &names, empty, &mut lets, span) {
        slots[i] = Some(ArgSrc::Dict(dict_slots.len()));
        written[i] = dicts.first().map(|d| d.1);
        dict_slots.push(e);
    }
    if sig.variadic && slots[var_at].is_none() {
        let elem = variadic_elem(ctx, &sig.param_tys[var_at]);
        slots[var_at] = Some(ArgSrc::Lowered(lower_variadic(
            ctx, callee, elem, var_parts, span,
        )));
    }
    let c_varargs = check_arity(ctx, sig, callee, has_receiver, extra, c_varargs);
    let mut out = Vec::with_capacity(n);
    let mut places: Vec<Option<HirExpr>> = vec![None; n];
    // Places other (non-`ref`) arguments read, for the exclusivity check.
    let mut reads: Vec<(String, Vec<String>)> = Vec::new();
    for (i, slot) in slots.into_iter().enumerate() {
        let pty = sig.param_tys[i].clone();
        let pname = sig.param_names.get(i).cloned().unwrap_or_default();
        let by_ref = sig.param_refs.get(i).copied().unwrap_or(false);
        let is_receiver = i == 0 && has_receiver;
        match &slot {
            Some(ArgSrc::Ref(_)) if !by_ref => ctx.emit_error(
                Code::REF_ARGUMENT,
                format!(
                    "argument `{pname}` of `{callee}` is passed with `ref`, but the parameter \
                     is not `ref`"
                ),
            ),
            Some(ArgSrc::Ast(e)) if by_ref && !is_receiver => ctx.emit_error(
                Code::REF_ARGUMENT,
                missing_ref(&format!("`{pname}`"), callee, e),
            ),
            Some(ArgSrc::Ast(e)) => reads.extend(crate::walk::free_places(&[], e)),
            _ => {}
        }
        let v = match slot {
            Some(ArgSrc::Ast(e)) => lower_expected(ctx, e, pty.clone()),
            Some(ArgSrc::Dict(i)) => lower_expected(ctx, &dict_slots[i], pty.clone()),
            Some(ArgSrc::Ref(e)) => {
                let h = lower_expected(ctx, e, pty.clone());
                if crate::lower::mono::place_root(&h).is_some() {
                    places[i] = Some(h.clone());
                } else if h.ty != HirTy::Unknown {
                    ctx.emit_error(
                        Code::REF_ARGUMENT,
                        format!(
                            "`ref` argument `{pname}` of `{callee}` must name a variable or \
                             a field of one"
                        ),
                    );
                }
                h
            }
            Some(ArgSrc::Lowered(h)) => {
                if is_receiver {
                    check_path_receiver(ctx, explicit_recv, by_ref, &h, &pname, callee);
                }
                if is_receiver && by_ref {
                    crate::lower::expr::method::captured_copy_write(ctx, &h, callee);
                    if !crate::lower::mono::writable_place(ctx, &h) {
                        ctx.dropped_ref_recv.push(span);
                    }
                    places[i] = Some(h.clone());
                }
                h
            }
            None => {
                if let Some(d) = sig.param_defaults.get(i).cloned().flatten() {
                    lower_expected(ctx, &d, pty.clone())
                } else {
                    ctx.emit_error(
                        Code::WRONG_ARGUMENT_COUNT,
                        format!("missing argument `{pname}` in call to `{callee}`"),
                    );
                    let sig = sig_of(ctx, callee, sig);
                    ctx.help_last(Some(format!("it is declared `{sig}`; pass `{pname}` too")));
                    poison(span)
                }
            }
        };
        // A mismatch points at the argument, not the whole call.
        let at = if v.span.is_dummy() {
            ctx.here()
        } else {
            v.span
        };
        let outer = ctx.at.replace(at);
        let shown = ctx.display_path(callee);
        out.push(coerce(ctx, &pty, v, &|| {
            format!("argument `{pname}` of `{shown}`")
        }));
        ctx.at.set(outer);
    }
    check_ref_exclusive(ctx, callee, &out, &places, &reads, has_receiver);
    // Written order. Named arguments out of parameter order go through
    // temps, after the receiver.
    let mut order: Vec<(usize, usize)> = written
        .iter()
        .enumerate()
        .filter_map(|(i, w)| w.filter(|_| places[i].is_none()).map(|w| (w, i)))
        .collect();
    if order.windows(2).any(|w| w[0].0 > w[1].0) {
        order.sort_unstable();
        let recv = (has_receiver && places.first().is_some_and(Option::is_none)).then_some(0);
        for i in recv.into_iter().chain(order.into_iter().map(|(_, i)| i)) {
            let v = std::mem::replace(&mut out[i], poison(span));
            let ty = v.ty.clone();
            let tmp = ctx.declare(None, ty.clone());
            lets.push(HirStmt::Let {
                local: tmp,
                init: v,
            });
            out[i] = hir_local(tmp, &ty, span);
        }
    }
    out.extend(c_varargs);
    (out, lets, places)
}

/// Dict spreads as named arguments. Each parameter in `empty` reads
/// `#d["p"]` from the first dict with key `p`, else its default (or panics
/// in `Dict::index`); every key must name one (`spread_names`, checked at
/// run time). A defaulted parameter skips a dict whose value type does not
/// fit it; that dict holding its key panics (`spread_misfit`).
pub(crate) fn dict_spread(
    ctx: &mut LowerCtx<'_>,
    dicts: &[(String, HirTy)],
    empty: Vec<(usize, String, Option<Expr>, HirTy)>,
    lets: &mut Vec<HirStmt>,
    span: nabic_diagnostic::Span,
) -> Vec<(usize, Expr)> {
    if dicts.is_empty() {
        return Vec::new();
    }
    let lit = |s: &str| Expr {
        kind: ExprKind::Literal(AstLit::Str(s.to_owned())),
        span,
    };
    let mut names = Vec::new();
    let mut misfits = Vec::new();
    let mut out = Vec::new();
    for (i, pname, default, pty) in empty {
        names.push(lit(&pname));
        let fill: Vec<&String> = dicts
            .iter()
            .filter(|(d, v)| {
                let fits = default.is_none() || dict_value_fits(ctx, &pty, v);
                if !fits {
                    let why = format!(
                        "whose value type `{}` does not fit parameter `{pname}: {}`",
                        ctx.ty_name(v),
                        ctx.ty_name(&pty)
                    );
                    misfits.push((d.clone(), pname.clone(), why));
                }
                fits
            })
            .map(|(d, _)| d)
            .collect();
        let Some(last) = fill.last() else {
            continue;
        };
        let skip_last = usize::from(default.is_none());
        let mut e = default.unwrap_or_else(|| spread_item_at(last, &lit(&pname), span));
        for d in fill.iter().rev().skip(skip_last) {
            let has = Expr {
                kind: ExprKind::MethodCall {
                    receiver: Box::new(crate::lower::throws::path_expr(d, span)),
                    method: "contains_key".to_owned(),
                    type_args: Vec::new(),
                    args: vec![CallArg::Positional(lit(&pname))],
                    trailing_closure: None,
                    origin: None,
                    written: false,
                },
                span,
            };
            e = Expr {
                kind: ExprKind::If {
                    cond: Box::new(has),
                    then_branch: Block {
                        stmts: Vec::new(),
                        trailing: Some(Box::new(spread_item_at(d, &lit(&pname), span))),
                        span,
                        label: None,
                    },
                    else_branch: Some(Box::new(e)),
                },
                span,
            };
        }
        out.push((i, e));
    }
    for (d, pname, why) in misfits {
        let check = index_fn(
            "spread_misfit",
            vec![
                crate::lower::throws::path_expr(&d, span),
                lit(&pname),
                lit(&why),
            ],
            span,
        );
        lets.push(HirStmt::Expr(lower_expected(ctx, &check, HirTy::Unit)));
    }
    let names = Expr {
        kind: ExprKind::List(names),
        span,
    };
    for (d, _) in dicts {
        let check = index_fn(
            "spread_names",
            vec![crate::lower::throws::path_expr(d, span), names.clone()],
            span,
        );
        lets.push(HirStmt::Expr(lower_expected(ctx, &check, HirTy::Unit)));
    }
    out
}

/// Can a dict value of type `v` fill a parameter of type `p`? An open
/// type (generic, not yet inferred) is taken to fit.
pub(crate) fn dict_value_fits(ctx: &LowerCtx<'_>, p: &HirTy, v: &HirTy) -> bool {
    let open = |t: &HirTy| {
        crate::lower::bounds::mentions(ctx, t, &|t| {
            matches!(
                t,
                HirTy::Unknown
                    | HirTy::Path(_)
                    | HirTy::Param(_)
                    | HirTy::Var(_)
                    | HirTy::OpenConst(_)
            )
        })
    };
    if open(p) {
        return true;
    }
    // An open literal fits what it can become.
    if crate::lower::infer::literal_default(ctx, v).is_some() {
        return crate::lower::infer::trial(ctx, &|| crate::lower::infer::unify(ctx, p, v));
    }
    open(v) || coercible(ctx, p, v)
}

/// What to write for an argument `p` given twice.
pub(crate) fn twice_help(p: &str) -> String {
    format!("pass `{p}` once: remove the extra `{p}: ..` (or the positional argument it repeats)")
}

/// A generic fn's parameter type, when it is plain (primitives, unit and
/// tuples of them) and so known before inference; else `Unknown`.
pub(crate) fn plain_param_ty(ty: &Ty) -> HirTy {
    fn plain(t: &HirTy) -> bool {
        match t {
            HirTy::Prim(_) | HirTy::Unit => true,
            HirTy::Tuple(ps) => ps.iter().all(plain),
            _ => false,
        }
    }
    let t = crate::ty::resolve_ty(ty);
    if plain(&t) { t } else { HirTy::Unknown }
}

/// `V` of a `Dict<str, V>`.
pub(crate) fn dict_value_ty(ctx: &LowerCtx<'_>, ty: &HirTy) -> HirTy {
    match ty {
        HirTy::Struct(id) => ctx
            .struct_def(*id)
            .generic_args
            .get(1)
            .cloned()
            .unwrap_or(HirTy::Unknown),
        _ => HirTy::Unknown,
    }
}

/// `Dict<str, V>`: what `..d` spreads as named arguments.
pub(crate) fn is_str_dict(ctx: &LowerCtx<'_>, ty: &HirTy) -> bool {
    let HirTy::Struct(id) = ty else {
        return false;
    };
    let def = ctx.struct_def(*id);
    enum_base_name(&def.name) == DICT && def.generic_args.first() == Some(&HirTy::Prim(PrimTy::Str))
}

pub(crate) const DICT: &str = "core::collections::dict::Dict";

pub(crate) const LIST: &str = "core::collections::list::List";

pub(crate) const SLICE: &str = "core::collections::slice::Slice";

pub(crate) const SLICE_MUT: &str = "core::collections::slice::SliceMut";

/// `ty` is `SliceMut<T>`.
pub(crate) fn is_slice_mut(ctx: &LowerCtx<'_>, ty: &HirTy) -> bool {
    matches!(ty, HirTy::Struct(id) if enum_base_name(&ctx.struct_def(*id).name) == SLICE_MUT)
}

/// `ty` is `List<T>`.
pub(crate) fn is_list(ctx: &LowerCtx<'_>, ty: &HirTy) -> bool {
    matches!(ty, HirTy::Struct(id) if enum_base_name(&ctx.struct_def(*id).name) == LIST)
}

/// A `List<T>` in a `SliceMut<T>` slot: a writable view of the whole
/// list. Anything else is left to `coerce`.
pub(crate) fn list_into_view(ctx: &mut LowerCtx<'_>, slot: &HirTy, value: HirExpr) -> HirExpr {
    let (HirTy::Struct(want), HirTy::Struct(have)) = (slot, &value.ty) else {
        return value;
    };
    if !is_slice_mut(ctx, slot) || !is_list(ctx, &value.ty) {
        return value;
    }
    if ctx.struct_def(*want).generic_args != ctx.struct_def(*have).generic_args {
        return value;
    }
    let span = value.span;
    let none = Expr {
        kind: ExprKind::Literal(AstLit::None),
        span,
    };
    let xs = splice(ctx, value);
    lower_expected(
        ctx,
        &index_fn("slice_mut", vec![xs, none], span),
        slot.clone(),
    )
}

/// `f(..[a, b, c])` into `fill` parameters: the run-time check is sure
/// to panic when the literal's length differs.
pub(crate) fn warn_list_spread(ctx: &LowerCtx<'_>, e: &Expr, fill: usize) {
    let ExprKind::List(items) = &e.kind else {
        return;
    };
    if items.iter().any(is_list_spread_item) || items.len() == fill {
        return;
    }
    let s = |k: usize| if k == 1 { "" } else { "s" };
    let n = items.len();
    ctx.emit_lint(
        "literal_spread_mismatch",
        e.span,
        format!(
            "spread of {n} element{} into {fill} parameter{}; this call panics",
            s(n),
            s(fill)
        ),
        format!(
            "spread a list of exactly {fill} element{}, or pass the arguments without `..`",
            s(fill)
        ),
    );
}

/// `..xs` inside a `List` literal: its length is known only at run time.
pub(crate) const fn is_list_spread_item(e: &Expr) -> bool {
    matches!(
        e.kind,
        ExprKind::Range {
            start: None,
            end: Some(_),
            inclusive: false,
        }
    )
}

/// `f(..{"a": 1, "c": 2})`: a literal key that names none of the
/// parameters left to fill is sure to panic in `spread_names`.
pub(crate) fn warn_dict_spreads(
    ctx: &LowerCtx<'_>,
    dicts: &[&Expr],
    empty: &[(usize, String, Option<Expr>, HirTy)],
) {
    for e in dicts {
        let ExprKind::Dict(entries) = &e.kind else {
            continue;
        };
        for (key, _) in entries {
            let Some(k) = crate::lower::resolve::str_lit_of(key) else {
                continue;
            };
            if k.contains('\\') || empty.iter().any(|p| p.1 == k) {
                continue;
            }
            let help = if empty.is_empty() {
                "remove the key: every parameter already has an argument".to_owned()
            } else {
                let names: Vec<String> = empty.iter().map(|p| format!("`\"{}\"`", p.1)).collect();
                format!("use a key that names a parameter: {}", names.join(", "))
            };
            ctx.emit_lint(
                "literal_spread_mismatch",
                key.span,
                format!(
                    "spread of key {k:?}, which names no parameter left to fill; this call panics"
                ),
                help,
            );
        }
    }
}

/// A `List` / `Slice`: what `..x` spreads with a run-time length.
pub(crate) fn spreads_at_run_time(ctx: &LowerCtx<'_>, ty: &HirTy) -> bool {
    let HirTy::Struct(id) = ty else {
        return false;
    };
    let name = ctx.struct_def(*id).name;
    matches!(enum_base_name(&name), LIST | SLICE)
}

/// A `List` literal item: `..x` spreads a tuple, array, `List` or `Slice`
/// (the handler is variadic); otherwise it's a range.
pub(crate) fn list_item(ctx: &mut LowerCtx<'_>, e: &Expr) -> CallArg {
    let ExprKind::Range {
        start: None,
        end: Some(x),
        inclusive: false,
    } = &e.kind
    else {
        return CallArg::Positional(e.clone());
    };
    let h = lower_expected(ctx, x, HirTy::Unknown);
    let spread =
        matches!(h.ty, HirTy::Tuple(_) | HirTy::Array(..)) || spreads_at_run_time(ctx, &h.ty);
    let x = splice(ctx, h);
    if spread {
        return CallArg::Spread(x);
    }
    CallArg::Positional(Expr {
        kind: ExprKind::Range {
            start: None,
            end: Some(Box::new(x)),
            inclusive: false,
        },
        span: e.span,
    })
}

/// One trailing argument of a variadic `...T` call.
pub(crate) enum VarPart<'e> {
    Arg(&'e Expr),
    /// `..x`: a tuple's fields, or a `List` / `Slice` / array's elements.
    Spread(&'e Expr),
    /// A field of a tuple spread that started in a fixed slot.
    Value(HirExpr),
}

/// The element type `T` of the `Slice<T>` a variadic parameter takes
/// (`Unknown` when `ty` is not one yet).
pub(crate) fn variadic_elem(ctx: &LowerCtx<'_>, ty: &HirTy) -> HirTy {
    match ty {
        HirTy::Struct(sid) => ctx
            .struct_def(*sid)
            .generic_args
            .first()
            .cloned()
            .unwrap_or(HirTy::Unknown),
        _ => HirTy::Unknown,
    }
}

/// A variadic call's trailing arguments as one `Slice<T>` (`elem`
/// `Unknown`: the first argument's type). Plain arguments become a fixed
/// array viewed as a slice; a spread makes a fresh `List`.
pub(crate) fn lower_variadic(
    ctx: &mut LowerCtx<'_>,
    callee: &str,
    mut elem: HirTy,
    parts: Vec<VarPart<'_>>,
    span: nabic_diagnostic::Span,
) -> HirExpr {
    // `[1, 2, 3]` calls `list_literal(...T)`: name the element, not the
    // argument.
    let list = leaf_name(callee) == "list_literal";
    let mut stmts: Vec<HirStmt> = Vec::new();
    // Each argument once into a temp, in order: (is a spread collection, temp).
    let mut temps: Vec<(bool, String, HirTy)> = Vec::new();
    let temp = |ctx: &mut LowerCtx<'_>, stmts: &mut Vec<HirStmt>, init: HirExpr| {
        let name = format!("#va{}", ctx.locals.len());
        let ty = init.ty.clone();
        let local = ctx.declare(Some(name.clone()), ty.clone());
        stmts.push(HirStmt::Let { local, init });
        (name, ty)
    };
    let what = if list {
        "list element"
    } else {
        "variadic argument"
    };
    // Without a given element type the elements agree as branches do: the
    // one whose sum holds every other's type is it, whichever comes first
    // (`[none, x]` with `x: f64?`). Earlier elements are coerced at the end.
    let given = elem != HirTy::Unknown;
    let one = |ctx: &mut LowerCtx<'_>, elem: &mut HirTy, v: HirExpr| {
        if *elem == HirTy::Unknown {
            elem.clone_from(&v.ty);
            return v;
        }
        let differ = !given
            && v.ty != HirTy::Unknown
            && !crate::lower::infer::agree(ctx, elem, &v.ty, false)
            && !coercible(ctx, elem, &v.ty);
        if differ {
            if let Some(j) = branch_join(ctx, &[elem.clone(), v.ty.clone()]) {
                elem.clone_from(&j);
                return coerce(ctx, &j, v, &|| what.to_owned());
            }
            let (a, b) = (ctx.ty_name(elem), ctx.ty_name(&v.ty));
            let hint = ctx.sum_hint(elem, &v.ty);
            let h = coerce(ctx, elem, v, &|| what.to_owned());
            ctx.help_last(Some(if list {
                format!(
                    "the elements are `{a}` and `{b}`, and Nabi does not infer a sum type; \
                     give the list its type: `let xs: List<{hint}> = [..];`"
                )
            } else {
                format!(
                    "the arguments are `{a}` and `{b}`, and Nabi does not infer a sum type; \
                     convert them to one type, or pass a typed `List<{hint}>` with `..`"
                )
            }));
            return h;
        }
        coerce(ctx, elem, v, &|| what.to_owned())
    };
    for part in parts {
        let v = match part {
            VarPart::Arg(e) => lower_expected(ctx, e, elem.clone()),
            VarPart::Value(v) => v,
            VarPart::Spread(e) => {
                let h = lower_expected(ctx, e, HirTy::Unknown);
                match h.ty.clone() {
                    HirTy::Tuple(fields) => {
                        let (name, ty) = temp(ctx, &mut stmts, h);
                        let local = ctx.lookup(&name).expect("temp just declared");
                        for (k, fty) in fields.iter().enumerate() {
                            let field = HirExpr {
                                kind: HirExprKind::TupleField {
                                    target: Box::new(hir_local(local, &ty, span)),
                                    index: u32::try_from(k).expect("tuple field overflow"),
                                    field_ty: fty.clone(),
                                },
                                ty: fty.clone(),
                                span,
                            };
                            let v = one(ctx, &mut elem, field);
                            let (name, ty) = temp(ctx, &mut stmts, v);
                            temps.push((false, name, ty));
                        }
                    }
                    HirTy::Array(..) | HirTy::Struct(_) => {
                        if elem == HirTy::Unknown {
                            elem = match &h.ty {
                                HirTy::Array(t, _) => (**t).clone(),
                                t => variadic_elem(ctx, t),
                            };
                        }
                        let (name, ty) = temp(ctx, &mut stmts, h);
                        temps.push((true, name, ty));
                    }
                    HirTy::Unknown => {}
                    other => ctx.emit_error(
                        Code::SPREAD_ARGUMENT,
                        format!(
                            "spread into a variadic `...T` needs a tuple, a `List`, a `Slice` or \
                             an array, found `{}`",
                            ctx.ty_name(&other)
                        ),
                    ),
                }
                continue;
            }
        };
        let v = one(ctx, &mut elem, v);
        let (name, ty) = temp(ctx, &mut stmts, v);
        temps.push((false, name, ty));
    }
    // No argument fixed `T` (`count()`): the call reports it uninferred.
    if elem == HirTy::Unknown {
        return poison(span);
    }
    let path = |name: &str| crate::lower::throws::path_expr(name, span);
    let ast = |kind: ExprKind| Expr { kind, span };
    let slice_of = |target: Expr| {
        ast(ExprKind::Slice {
            target: Box::new(target),
            start: None,
            end: None,
            step: None,
        })
    };
    let trailing = if temps.iter().all(|(many, ..)| !many) {
        // `[a, b, c][:]`: a fixed array (built here, not by the list literal
        // handler, which is itself variadic) viewed as a slice.
        let n = u32::try_from(temps.len()).expect("variadic argument count overflow");
        let elements = temps
            .iter()
            .map(|(_, name, ty)| {
                let local = ctx.lookup(name).expect("temp declared");
                let v = hir_local(local, ty, span);
                // Only an element typed before the join: a wrong one was
                // reported already.
                if !given && *ty != elem && coercible(ctx, &elem, ty) {
                    coerce(ctx, &elem, v, &|| what.to_owned())
                } else {
                    v
                }
            })
            .collect();
        let arr = HirExpr {
            kind: HirExprKind::ArrayInit {
                elem_ty: elem.clone(),
                elements,
            },
            ty: HirTy::Array(Box::new(elem), n),
            span,
        };
        let (name, _) = temp(ctx, &mut stmts, arr);
        lower_expected(ctx, &slice_of(path(&name)), HirTy::Unknown)
    } else {
        // `let l = List::new(); l.push(a); for x in xs { l.push(x); } l[:]`
        let new = ast(ExprKind::Call {
            callee: Box::new(crate::lower::bignum::path_expr(LIST, "new", span)),
            args: Vec::new(),
            trailing_closure: None,
        });
        let list_ty = ctx
            .instantiate_struct(LIST, vec![elem], ctx.here())
            .map_or(HirTy::Unknown, HirTy::Struct);
        let init = lower_expected(ctx, &new, list_ty);
        let (list, _) = temp(ctx, &mut stmts, init);
        let push = |x: Expr| {
            ast(ExprKind::MethodCall {
                receiver: Box::new(path(&list)),
                method: "push".to_owned(),
                type_args: Vec::new(),
                args: vec![CallArg::Positional(x)],
                trailing_closure: None,
                origin: None,
                written: false,
            })
        };
        for (many, name, _) in &temps {
            let e = if *many {
                let x = format!("#vx{}", ctx.locals.len());
                ast(ExprKind::For {
                    label: None,
                    pat: nabic_ast::Pattern {
                        kind: nabic_ast::PatternKind::Binding {
                            name: x.clone(),
                            sub: None,
                        },
                        span,
                    },
                    iter: Box::new(path(name)),
                    body: nabic_ast::Block {
                        stmts: Vec::new(),
                        trailing: Some(Box::new(push(path(&x)))),
                        span,
                        label: None,
                    },
                    is_await: false,
                    by_ref: false,
                })
            } else {
                push(path(name))
            };
            let h = lower_expected(ctx, &e, HirTy::Unknown);
            stmts.push(HirStmt::Expr(h));
        }
        lower_expected(ctx, &slice_of(path(&list)), HirTy::Unknown)
    };
    HirExpr {
        ty: trailing.ty.clone(),
        kind: HirExprKind::Block(Box::new(HirBlock {
            stmts,
            trailing: Some(trailing),
            label: None,
        })),
        span,
    }
}

/// `name[at]`.
pub(crate) fn spread_item_at(name: &str, at: &Expr, span: nabic_diagnostic::Span) -> Expr {
    Expr {
        kind: ExprKind::Index(
            Box::new(crate::lower::throws::path_expr(name, span)),
            Box::new(at.clone()),
        ),
        span,
    }
}

/// `#spread[i]` (`i: usize`).
pub(crate) fn spread_item(name: &str, i: usize, span: nabic_diagnostic::Span) -> Expr {
    Expr {
        kind: ExprKind::Index(
            Box::new(crate::lower::throws::path_expr(name, span)),
            Box::new(Expr {
                kind: ExprKind::Literal(AstLit::Int(format!("{i}usize"))),
                span,
            }),
        ),
        span,
    }
}

/// `f(a: i32, b: i32)` of a resolved signature.
pub(crate) fn sig_of(ctx: &LowerCtx<'_>, callee: &str, sig: &FnSig) -> String {
    let tys = sig.param_tys.iter().map(|t| ctx.ty_name(t));
    crate::lower::sig_text(callee, sig.param_names.iter().map(String::as_str).zip(tys))
}

/// `f(a: i32, b: i32)` of a declaration.
pub(crate) fn sig_of_def(callee: &str, def: &FnDef) -> String {
    let params = def.params.iter().map(|p| {
        let t =
            p.ty.as_ref()
                .map_or("_".to_owned(), nabic_ast::print::print_ty);
        let t = if p.variadic { format!("...{t}") } else { t };
        (p.name.as_str(), t)
    });
    crate::lower::sig_text(callee, params)
}

/// Report a wrong argument count, with the signature as help.
pub(crate) fn arity_error(
    ctx: &LowerCtx<'_>,
    callee: &str,
    want: usize,
    got: usize,
    sig: Option<String>,
) {
    let (msg, help) = crate::lower::arity_msg(&ctx.display_path(callee), want, got, sig);
    ctx.emit_error(Code::WRONG_ARGUMENT_COUNT, msg);
    ctx.help_last(help);
}

/// Report surplus arguments, unless `sig` is C variadic: then `surplus`
/// are its `...` arguments, returned promoted. A method's receiver
/// (`recv`) is not counted.
pub(crate) fn check_arity(
    ctx: &LowerCtx<'_>,
    sig: &FnSig,
    callee: &str,
    recv: bool,
    extra: usize,
    surplus: Vec<HirExpr>,
) -> Vec<HirExpr> {
    if ctx.env.is(crate::lower::FnSet::CVariadic, sig.id) {
        return surplus
            .into_iter()
            .map(|a| c_vararg_promote(ctx, callee, a))
            .collect();
    }
    let extra = extra + surplus.len();
    if extra > 0 {
        let n = sig.param_tys.len() - usize::from(recv && !sig.param_tys.is_empty());
        arity_error(ctx, callee, n, n + extra, Some(sig_of(ctx, callee, sig)));
    }
    Vec::new()
}

/// The error for argument `e` of `ref` parameter `what` without `ref`:
/// `ref e` for a place, else a variable to bind it to first.
pub(crate) fn missing_ref(what: &str, callee: &str, e: &Expr) -> String {
    // A variable or a field of one; an element is copied in and stored back.
    fn place(e: &Expr) -> bool {
        match &e.kind {
            ExprKind::Path(p) => p.segments.len() == 1,
            ExprKind::Field(b, _) | ExprKind::TupleField(b, _) => place(b),
            _ => false,
        }
    }
    let text = snippet(e);
    if place(e) {
        format!("parameter {what} of `{callee}` is `ref` - pass `ref {text}` at the call site")
    } else if matches!(e.kind, ExprKind::Index(..)) {
        format!(
            "parameter {what} of `{callee}` is `ref`, and an element cannot be passed `ref` - \
             copy it into a variable, `let v = {text};`, pass `ref v`, and store it back, \
             `{text} = v;`"
        )
    } else {
        format!(
            "parameter {what} of `{callee}` is `ref`, so it changes a variable, and `{text}` is \
             not one - bind it first, `let v = {text};`, and pass `ref v`"
        )
    }
}
