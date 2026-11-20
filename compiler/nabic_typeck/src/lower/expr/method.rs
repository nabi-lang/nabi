//! Method calls: receivers, write-back and method lookup.

#[allow(clippy::wildcard_imports)]
use crate::lower::expr::*;

/// A method call's receiver. One with an unfixed generic parameter
/// (`B::new().with(1)`) is retried against the call's expected type when
/// that fixes it exactly: builder chains.
pub(crate) fn lower_receiver(ctx: &mut LowerCtx<'_>, receiver: &Expr) -> HirExpr {
    // `let n: i32 = 7.max(-2)`: a literal receiver of a number slot has
    // the slot's type.
    if is_untyped_literal(receiver) {
        let int = is_untyped_int(receiver);
        let slot = ctx.current_expected().and_then(|t| match t {
            HirTy::Prim(p) if (int && p.is_int() && *p != PrimTy::Char) || p.is_float() => {
                Some(t.clone())
            }
            _ => None,
        });
        if let Some(t) = slot {
            return lower_expected(ctx, receiver, t);
        }
    }
    let mark = ctx.probe_mark();
    let recv = lower_expected(ctx, receiver, HirTy::Unknown);
    if ctx.env.sink.uninferred.get() == mark.uninferred {
        return recv;
    }
    let Some(expected) = ctx
        .current_expected()
        .filter(|t| !matches!(t, HirTy::Unknown | HirTy::Unit | HirTy::Any))
        .cloned()
    else {
        return recv;
    };
    let retry = ctx.probe_mark();
    let mut probe = ctx.clone();
    let fits = lower_expected(&mut probe, receiver, expected.clone()).ty == expected
        && probe.probe_clean(&retry);
    ctx.rollback(&retry);
    if !fits {
        return recv;
    }
    ctx.rollback(&mark);
    lower_expected(ctx, receiver, expected)
}

/// Core collection methods that change the collection in place.
pub(crate) const MUTATORS: &[&str] = &[
    "push",
    "pop",
    "set",
    "insert",
    "remove",
    "clear",
    "truncate",
    "drain",
    "retain",
    "fill",
    "swap",
    "dedup",
    "sort",
    "sort_by",
    "reverse",
    "reserve",
    "shrink_to_fit",
    "push_back",
    "push_front",
    "pop_back",
    "pop_front",
    "entry",
];

/// Core collections: a `const` of one is built afresh at each use.
pub(crate) const HEAP_CONSTS: &[&str] = &["List", "Dict", "Set", "Deque", "BinaryHeap"];

/// A method call whose change to `receiver` is lost: any change to a
/// `const` or a `ref self` write to a static (`dropped`) is an error.
/// An element or entry is written back (`element_write_back`).
pub(crate) fn receiver_write(
    ctx: &LowerCtx<'_>,
    receiver: &Expr,
    method: &str,
    dropped: bool,
    span: nabic_diagnostic::Span,
) {
    let mut root = receiver;
    while let ExprKind::Field(inner, _) | ExprKind::TupleField(inner, _) = &root.kind {
        root = inner;
    }
    let text = ctx
        .src(receiver.span)
        .map_or_else(|| snippet(receiver), str::to_owned);
    match &root.kind {
        ExprKind::Path(p) if p.segments.len() == 1 => {
            let name = p.segments[0].name.as_str();
            if ctx.lookup(name).is_some() {
                return;
            }
            if let Some(full) = ctx.resolve_in(&[name], false) {
                let heap = std::ptr::eq(root, receiver)
                    && MUTATORS.contains(&method)
                    && ctx.env.konst(&full).is_some_and(|(ty, _)| {
                        matches!(&ty.kind, nabic_ast::TyKind::Path(tp)
                            if tp.segments.last().is_some_and(|s| HEAP_CONSTS.contains(&s.name.as_str())))
                    });
                if dropped || heap {
                    let low = name.to_lowercase();
                    ctx.emit_error_help(
                        Code::INVALID_ASSIGNMENT,
                        span,
                        format!(
                            "cannot call `{method}` on `{name}`: it is a const, and each use \
                             builds a new value, so the change would be dropped"
                        ),
                        format!(
                            "copy it into a variable and change that: `let {low} = {name}; \
                             {low}.{method}(..);`"
                        ),
                    );
                }
            } else if dropped && ctx.resolve_in(&[name], true).is_some() {
                ctx.emit_error_help(
                    Code::INVALID_ASSIGNMENT,
                    span,
                    format!(
                        "cannot call `{method}` on `{text}`: it takes `ref self`, and a static \
                         is never written"
                    ),
                    format!(
                        "copy it into a variable and call `{method}` there: `let v = {text}; \
                         v.{method}(..);`"
                    ),
                );
            }
        }
        _ => {}
    }
}

/// A `ref self` call of `method` on a value-captured variable (or a field
/// of one) changes the closure's copy, which each call starts afresh: an
/// error, as assigning a field of it is.
pub(crate) fn captured_copy_write(ctx: &LowerCtx<'_>, recv: &HirExpr, method: &str) {
    let Some(l) = crate::lower::mono::place_root(recv) else {
        return;
    };
    if !ctx.value_captured.contains(&l) || !plain_value(ctx, &ctx.locals[l.0 as usize].ty, 0) {
        return;
    }
    let name = ctx.locals[l.0 as usize].name.clone().unwrap_or_default();
    ctx.emit_error_help(
        Code::INVALID_ASSIGNMENT,
        ctx.here(),
        format!(
            "`{method}` takes `ref self` and changes `{name}`, but `{name}` is value-captured: \
             the change would be lost with the closure's copy"
        ),
        format!(
            "add `[ref {name}]` to the closure's capture list; to change only a copy inside \
             the closure, copy it first (`let v = {name};`)"
        ),
    );
}

/// Plain data all the way down (numbers, `bool`, `str`, and structs,
/// tuples, arrays and enums of them, none with a `Drop`): a copy of it
/// shares nothing, so a change to the copy reaches no one. Handles and
/// resources (`Cell`, `File`, a socket) share their state.
fn plain_value(ctx: &LowerCtx<'_>, ty: &HirTy, depth: u32) -> bool {
    if depth > DROP_GLUE_DEPTH {
        return false;
    }
    let all = |ts: &[HirTy]| ts.iter().all(|t| plain_value(ctx, t, depth + 1));
    match ty {
        HirTy::Prim(_) | HirTy::Unit | HirTy::None => true,
        HirTy::Tuple(ts) => all(ts),
        HirTy::Array(t, _) => plain_value(ctx, t, depth + 1),
        HirTy::Struct(sid) => {
            let def = ctx.struct_def(*sid);
            def.union_kind.is_none()
                && !ctx.has_trait(ty, ctx.lang("drop_method"))
                && all(&def.fields.iter().map(|f| f.ty.clone()).collect::<Vec<_>>())
        }
        HirTy::Enum(eid) => {
            !ctx.has_trait(ty, ctx.lang("drop_method"))
                && all(&ctx
                    .enum_def(*eid)
                    .variants
                    .iter()
                    .flat_map(variant_arg_tys)
                    .collect::<Vec<_>>())
        }
        _ => false,
    }
}

/// `receiver.method(args)` (`expr`).
pub(crate) fn lower_method_call(ctx: &mut LowerCtx<'_>, expr: &Expr) -> HirExpr {
    let ExprKind::MethodCall {
        receiver,
        method,
        args,
        trailing_closure,
        type_args,
        written,
        ..
    } = &expr.kind
    else {
        unreachable!("a method call")
    };
    // A `ref self` method on a `for ref` element writes the element back.
    if let Some(place) = unplace(ctx, receiver) {
        let mut on_place = expr.clone();
        if let ExprKind::MethodCall { receiver, .. } = &mut on_place.kind {
            **receiver = place;
        }
        return lower_method_call(ctx, &on_place);
    }
    let span = expr.span;
    // `d[k, default: v].push(x)`: the receiver is the stored value,
    // `v` inserted first when `k` is absent.
    if let ExprKind::Index(d, idx) = &receiver.kind {
        if nabic_ast::as_index_default(idx).is_some() {
            let mut call = expr.clone();
            if let ExprKind::MethodCall { receiver, .. } = &mut call.kind {
                **receiver = index_default_call(d, idx, "get_or_insert");
            }
            return lower_method_call(ctx, &call);
        }
    }
    let all_args = with_trailing_closure(args, trailing_closure.as_deref());
    let elem = element_receiver(receiver).map(|_| (ctx.probe_mark(), ctx.throwing_calls));
    let recv_h = literal_conv_receiver(ctx, receiver, method)
        .unwrap_or_else(|| lower_receiver(ctx, receiver));
    ctx.method_type_args.clone_from(type_args);
    ctx.user_method = *written;
    // A release the compiler added has no span: its binding's.
    let at = match recv_h.kind {
        HirExprKind::Local(l) if span.is_dummy() => {
            ctx.owed_at.get(&l).copied().unwrap_or_else(|| ctx.here())
        }
        _ => span,
    };
    let method = match drop_method(ctx, &recv_h.ty, method, at) {
        Ok(m) => m,
        Err(e) => return e,
    };
    if method == nabic_ast::USING_DROP_METHOD {
        if let HirExprKind::Local(l) = recv_h.kind {
            ctx.guard_lets.retain(|g| g.0 != l);
        }
        let stmts = drop_glue(ctx, recv_h, span, 0);
        return HirExpr {
            kind: HirExprKind::Block(Box::new(HirBlock {
                stmts,
                trailing: None,
                label: None,
            })),
            ty: HirTy::Unit,
            span,
        };
    }
    let mark = ctx.dropped_ref_recv.len();
    let call = method_call_on(ctx, recv_h, method, &all_args, span);
    let dropped = ctx.dropped_ref_recv[mark..].contains(&span);
    ctx.dropped_ref_recv.truncate(mark);
    receiver_write(ctx, receiver, method, dropped, span);
    let call = unpack_call(ctx, call);
    match elem {
        Some((mark, calls)) if dropped => {
            element_write_back(ctx, expr, &mark, calls).unwrap_or(call)
        }
        _ => call,
    }
}

/// The `xs` and `i` of the `xs[i]` under the field chain of `receiver`,
/// unless `i` is a range (a slice view writes through by itself).
pub(crate) const fn element_receiver(receiver: &Expr) -> Option<(&Expr, &Expr)> {
    let mut root = receiver;
    while let ExprKind::Field(inner, _) | ExprKind::TupleField(inner, _) = &root.kind {
        root = inner;
    }
    match &root.kind {
        ExprKind::Index(arr, idx) if !matches!(idx.kind, ExprKind::Range { .. }) => {
            Some((arr, idx))
        }
        _ => None,
    }
}

/// Replace the root under the field chain of `e` with `root`.
pub(crate) fn put_root(e: &mut Expr, root: Expr) {
    match &mut e.kind {
        ExprKind::Field(inner, _) | ExprKind::TupleField(inner, _) => put_root(inner, root),
        _ => *e = root,
    }
}

/// Whether reading `e` twice finds the same storage: a variable, `self`,
/// a field of one, or an element at a literal or named index.
pub(crate) fn stable_place(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Path(_) | ExprKind::SelfLower => true,
        ExprKind::Field(inner, _) | ExprKind::TupleField(inner, _) => stable_place(inner),
        ExprKind::Index(inner, idx) => {
            matches!(idx.kind, ExprKind::Literal(_) | ExprKind::Path(_)) && stable_place(inner)
        }
        _ => false,
    }
}

/// `xs[i].m(..)` (`call`) with `m` taking `ref self`: copy the element
/// out, call `m` on the copy and store the copy back, after a throw too:
///
/// `{ let e = xs[i]; let r = e.m(..); xs[i] = e; r }`
///
/// `mark` and `calls` are from before `call` was lowered the plain way,
/// which this undoes. `None` keeps that lowering: the store does not
/// type, so the element is a temporary.
pub(crate) fn element_write_back(
    ctx: &mut LowerCtx<'_>,
    call: &Expr,
    mark: &ProbeMark,
    calls: u32,
) -> Option<HirExpr> {
    let ExprKind::MethodCall { receiver, .. } = &call.kind else {
        unreachable!("a method call")
    };
    let (arr, idx) = element_receiver(receiver)?;
    let throws = ctx.throwing_calls != calls;
    let span = call.span;
    let mut stmts = Vec::new();
    let arr = if stable_place(arr) {
        arr.clone()
    } else {
        let a = tmp_path(ctx, "arr", span);
        stmts.push(let_stmt(&a, arr.clone()));
        a
    };
    let idx = if matches!(idx.kind, ExprKind::Literal(_) | ExprKind::Path(_)) {
        idx.clone()
    } else {
        let i = tmp_path(ctx, "idx", span);
        stmts.push(let_stmt(&i, idx.clone()));
        i
    };
    let at = Expr {
        kind: ExprKind::Index(Box::new(arr), Box::new(idx)),
        span: receiver.span,
    };
    let elem = tmp_path(ctx, "elem", span);
    stmts.push(let_stmt(&elem, at.clone()));
    let mut on_copy = call.clone();
    if let ExprKind::MethodCall { receiver, .. } = &mut on_copy.kind {
        put_root(receiver, elem.clone());
    }
    let store = assign_stmt(at, elem);
    let trailing = if throws {
        Expr {
            kind: ExprKind::Catch {
                body: Box::new(on_copy),
                catches: Vec::new(),
                finally: Some(Block {
                    stmts: vec![store],
                    trailing: None,
                    span,
                    label: None,
                }),
            },
            span,
        }
    } else {
        let r = tmp_path(ctx, "ret", span);
        stmts.push(let_stmt(&r, on_copy));
        stmts.push(store);
        r
    };
    let block = Expr {
        kind: ExprKind::Block(Block {
            stmts,
            trailing: Some(Box::new(trailing)),
            span,
            label: None,
        }),
        span,
    };
    let retry = ctx.probe_mark();
    let mut probe = ctx.clone();
    lower_expr(&mut probe, &block);
    let failed = ctx
        .rollback(&retry)
        .iter()
        .any(|d| d.severity == nabic_diagnostic::Severity::Error);
    if failed {
        return None;
    }
    ctx.rollback(mark);
    ctx.throwing_calls = calls;
    Some(lower_expr(ctx, &block))
}

/// `snippet(e)`, parenthesised unless it is a single term.
pub(crate) fn operand_text(e: &Expr) -> String {
    let s = snippet(e);
    match e.kind {
        ExprKind::Path(_)
        | ExprKind::Literal(_)
        | ExprKind::Field(..)
        | ExprKind::TupleField(..)
        | ExprKind::Index(..)
        | ExprKind::Call { .. }
        | ExprKind::MethodCall { .. }
        | ExprKind::SelfLower => s,
        _ => format!("({s})"),
    }
}

/// Python's `s[0]` on a `str`: its characters.
pub(crate) fn str_index_help(ctx: &LowerCtx<'_>, s: &Expr, i: &Expr) {
    if let (Some(s), Some(i)) = (ctx.src(s.span), ctx.src(i.span)) {
        ctx.help_last(Some(format!(
            "a `str` is UTF-8 text, not an array of characters; take one with \
             `{s}.chars().nth({i})`"
        )));
    }
}

/// Python's `"a {} b".format(x)` as an f-string, `f"a {x} b"`.
pub(crate) fn format_help(
    ctx: &LowerCtx<'_>,
    args: &[CallArg],
    span: nabic_diagnostic::Span,
) -> String {
    let lit = ctx
        .src(span)
        .and_then(|t| t.rsplit_once(".format(").map(|(r, _)| r.trim()))
        .filter(|r| r.len() >= 2 && r.starts_with('"') && r.ends_with('"'));
    let vals: Vec<&str> = args
        .iter()
        .filter_map(|a| match a {
            CallArg::Positional(e) => ctx.src(e.span),
            _ => None,
        })
        .collect();
    let filled = lit.and_then(|l| {
        let parts: Vec<&str> = l[1..l.len() - 1].split("{}").collect();
        (parts.len() == vals.len() + 1).then(|| {
            let mut out = parts[0].to_owned();
            for (v, p) in vals.iter().zip(&parts[1..]) {
                out.push_str(&format!("{{{v}}}{p}"));
            }
            out
        })
    });
    match filled {
        Some(f) => format!("use an f-string: `f\"{f}\"`"),
        None => "use an f-string: `f\"..{x}..\"` puts `x` in the text".to_owned(),
    }
}

/// A method call on a receiver over open literals: the one choice of
/// number types that has the method (`9.once()`, `(1, 2).foo()`), else
/// their defaults, fixed here.
pub(crate) fn literal_receiver(ctx: &LowerCtx<'_>, ty: &HirTy, method: &str) -> Option<HirTy> {
    if !crate::lower::infer::active(ctx) || !crate::lower::infer::has_var(ctx, ty) {
        return None;
    }
    let mut vs = Vec::new();
    crate::lower::infer::open_literals_in(ctx, ty, &mut vs);
    // Found as it is (`xs.push(1)` on a `List<#var>`, a blanket impl's
    // method): its bounds fix the literal, or the body's end does.
    if vs.is_empty() || matches!(pick_method(ctx, ty, method, None), Ok(Some(_))) {
        return None;
    }
    crate::lower::infer::pick_literals_or(
        ctx,
        ty,
        &|t| matches!(pick_method(ctx, t, method, None), Ok(Some(_))),
        &|t| {
            ctx.env
                .methods_on(t, method)
                .is_some_and(|ids| !ids.is_empty())
        },
        // A bare literal is fixed here; one inside a type may still meet
        // the method another way (`xs.map(f)` through `xs.iter()`).
        matches!(ty, HirTy::Var(_)),
    );
    Some(crate::lower::infer::resolve(ctx, ty))
}

pub(crate) fn method_call_on(
    ctx: &mut LowerCtx<'_>,
    recv_h: HirExpr,
    method: &str,
    args: &[CallArg],
    span: nabic_diagnostic::Span,
) -> HirExpr {
    let lower_args_only = |ctx: &mut LowerCtx<'_>| discard_args(ctx, args);
    let type_args = std::mem::take(&mut ctx.method_type_args);
    let user = std::mem::take(&mut ctx.user_method);
    let mut recv_h = recv_h;
    if let Some(t) = literal_receiver(ctx, &recv_h.ty, method) {
        recv_h.ty = t;
    }
    if recv_h.ty == HirTy::Unknown {
        lower_args_only(ctx);
        return recv_h;
    }
    if matches!(
        method,
        "lt" | "gt" | "le" | "ge" | "cmp" | "partial_cmp" | "min" | "max" | "clamp"
    ) && sum_value_terms(&recv_h.ty) > 1
    {
        lower_args_only(ctx);
        return sum_order_error(ctx, &recv_h.ty, method, span);
    }
    if matches!(recv_h.ty, HirTy::Sum(_)) && SUM_METHODS.contains(&method) {
        return sum_structural_call(ctx, recv_h, method, args, None, span);
    }
    let recv_h = if matches!(recv_h.ty, HirTy::Sum(_)) {
        match crate::lower::reflect::method_call(ctx, recv_h, method, args) {
            Ok(r) => return r,
            Err(r) => r,
        }
    } else {
        recv_h
    };
    if matches!(recv_h.ty, HirTy::Sum(_)) && matches!(method, "min" | "max" | "clamp") {
        return sum_min_max(ctx, recv_h, method, args, span);
    }
    if let HirTy::AnyTrait(bounds) = recv_h.ty.clone() {
        return any_method_call(ctx, recv_h, &bounds, method, None, &type_args, args, span);
    }
    let recv_h = closure_as_fn(ctx, recv_h, &|ctx, t| {
        matches!(pick_call_method(ctx, t, method), Ok(Some(_)))
    });
    let pick = match pick_call_method_in(ctx, &recv_h.ty, method, user) {
        Ok(None) if trait_out_of_scope(ctx, &recv_h.ty, method, span) => {
            lower_args_only(ctx);
            return poison(span);
        }
        Ok(p) => p,
        Err(msg) => {
            let recv = Some(recv_h.clone());
            if let Some(call) =
                call_overload(ctx, &recv_h.ty, method, None, recv, args, method, span)
            {
                return call;
            }
            // As the operator form picks: by the argument's type, then by
            // the impls whose bounds hold for it.
            if let Some(p) = pick_by_args(ctx, &recv_h.ty, method, args)
                .or_else(|| pick_by_arg(ctx, &recv_h.ty, method, None, args))
                .or_else(|| {
                    pick_generic_by_bounds(ctx, &recv_h.ty, method, args).map(MethodPick::Generic)
                })
            {
                Some(p)
            } else {
                lower_args_only(ctx);
                return ctx.error_expr(Code::AMBIGUOUS_ITEM, span, msg);
            }
        }
    };
    // `x.sqrt()` with `x: T`, `T: Root`, in an instance over a type with an
    // inherent `sqrt`: the bound's method, as the generic body meant.
    let pick = bound_pick(ctx, &recv_h.ty, method).or(pick);
    // `Ord::min` / `max` take an argument: `xs.max()` is the items'.
    let unary_ord = args.is_empty() && matches!(method, "min" | "max");
    if pick.is_none() || unary_ord {
        if let Some(it) = iter_forward(ctx, &recv_h, method, span) {
            ctx.method_type_args = type_args;
            return method_call_on(ctx, it, method, args, span);
        }
    }
    let fn_id = match pick {
        Some(MethodPick::Concrete(id)) => Some(id),
        Some(MethodPick::Generic(_)) | None => None,
    };
    let Some(fn_id) = fn_id else {
        let gm = match pick {
            Some(MethodPick::Generic(gm)) => Some(gm),
            _ => None,
        };
        if let Some(gm) = gm {
            check_method_info_visible(ctx, &gm.info, method);
            if gm.def.params.first().map(|p| p.name.as_str()) != Some("self") {
                lower_args_only(ctx);
                let e = ctx.error_expr(
                    Code::WRONG_ITEM_FORM,
                    span,
                    format!(
                        "`{method}` is an associated function, not a method - call it as \
                         `{}::{method}(...)`",
                        ctx.ty_name(&recv_h.ty)
                    ),
                );
                ctx.help_last(not_a_method_help(ctx, &recv_h.ty, method));
                return e;
            }
            if is_addr_intrinsic(&gm.def, &gm.info.module) {
                // `MaybeUninit<T>::as_mut_ptr`: the receiver's place, as a pointer to its
                // type argument.
                lower_args_only(ctx);
                let pointee = match &recv_h.ty {
                    HirTy::Struct(sid) => ctx.struct_def(*sid).generic_args.first().cloned(),
                    _ => None,
                };
                let pointee = pointee.unwrap_or_else(|| recv_h.ty.clone());
                return addr_expr(ctx, recv_h, &gm.def, method, pointee, span);
            }
            match bind_explicit(ctx, &gm.def, Some(&gm.self_ty), &type_args, method) {
                Ok(bound) => ctx.explicit_generics = bound,
                Err(msg) => {
                    lower_args_only(ctx);
                    return ctx.error_expr(Code::GENERIC_ARGUMENTS, span, msg);
                }
            }
            return call_generic(
                ctx,
                &gm.def,
                Some(&gm.self_ty),
                Some(recv_h),
                args,
                method,
                span,
            );
        }
        let since = ctx.env.sink.diagnostics.borrow().len();
        lower_args_only(ctx);
        if let Some(e) = generic_miss(ctx, &recv_h, method, span) {
            return e;
        }
        // A missing method is a hard error, not a silent unit return, unless
        // `Reflect` (every type's) answers it.
        let recv_h = match crate::lower::reflect::method_call(ctx, recv_h, method, args) {
            Ok(r) => return r,
            Err(r) => r,
        };
        let ty = ctx.ty_name(&recv_h.ty);
        if let HirTy::Param(p) = recv_h.ty {
            return crate::lower::bounds::no_method(ctx, p, method, span);
        }
        if let Some(e) = crate::lower::suggest::optional_method(ctx, &recv_h, method, args, span) {
            return e;
        }
        if let Some(e) = sum_no_method(ctx, &recv_h, method, args.is_empty(), span) {
            return e;
        }
        if let Some(e) = crate::lower::habits::throwing_receiver(ctx, &recv_h, method, args, span) {
            return e;
        }
        // No method, so no parameter types for its closures: drop the
        // "cannot infer" errors they gave.
        retain_since(ctx, since, |d| {
            d.tag != nabic_diagnostic::Tag::ClosureParamInfer
        });
        // `s[a:b:k]` desugars to `.slice(..).step(k)`.
        if method == "step" && recv_h.ty == HirTy::Prim(PrimTy::Str) {
            let e = ctx.error_expr(
                Code::INVALID_INDEX,
                span,
                "a `str` slice has no step: `s[a:b:k]` is for lists and arrays".to_owned(),
            );
            ctx.help_last(Some(
                "step over the characters: `s.chars().step_by(k)`".to_owned(),
            ));
            return e;
        }
        let e = ctx.error_expr(
            Code::NO_METHOD,
            span,
            format!("no method `{method}` found for type `{ty}`"),
        );
        let format = (method == "format" && recv_h.ty == HirTy::Prim(PrimTy::Str))
            .then(|| format_help(ctx, args, span));
        let join = (method == "join" && recv_h.ty == HirTy::Prim(PrimTy::Str))
            .then(|| crate::lower::suggest::join_help(ctx, &recv_h, args));
        let help = format
            .or(join)
            .or_else(|| crate::lower::reflect::inspect_help(ctx, &recv_h.ty, method))
            .unwrap_or_else(|| crate::lower::suggest::no_method_help(ctx, &recv_h, method, span));
        ctx.help_last(Some(help));
        return e;
    };
    check_method_visible(ctx, fn_id, method);
    if !type_args.is_empty() {
        lower_args_only(ctx);
        return ctx.error_expr(
            Code::GENERIC_ARGUMENTS,
            span,
            crate::lower::mono::arity_msg(method, 0, 0, type_args.len()),
        );
    }
    let sig = ctx.sig_by_id(fn_id);
    if sig.param_names.first().map(String::as_str) != Some("self") {
        lower_args_only(ctx);
        let e = ctx.error_expr(
            Code::WRONG_ITEM_FORM,
            span,
            format!(
                "`{method}` is an associated function, not a method - call it as \
                 `{}::{method}(...)`",
                ctx.ty_name(&recv_h.ty)
            ),
        );
        ctx.help_last(not_a_method_help(ctx, &recv_h.ty, method));
        return e;
    }
    call_sig(ctx, &sig, method, Some(recv_h), args, span)
}

/// A generic impl that would provide `method` but does not apply: its
/// bound fails (named rather than "no method"), or its header does not
/// solve for the receiver (`unsolved_header`).
fn generic_miss(
    ctx: &LowerCtx<'_>,
    recv: &HirExpr,
    method: &str,
    span: nabic_diagnostic::Span,
) -> Option<HirExpr> {
    let unmet = ctx
        .env
        .impls
        .generic_by(crate::lower::Pick::Method(method))
        .find_map(|gi| {
            ctx.generic_impl_match(gi, &recv.ty, 0)
                .err()
                .flatten()
                .map(|b| (gi, b))
        });
    let Some((gi, (bty, bound))) = unmet else {
        return unsolved_header(ctx, recv, method, span);
    };
    Some(ctx.error_expr(
        Code::UNSATISFIED_BOUND,
        span,
        format!(
            "method `{method}` of the generic impl of `{}` does not apply to `{}`: its bound \
             needs `{}: {}`, which does not hold",
            gi.trait_key,
            ctx.ty_name(&recv.ty),
            ctx.ty_name(&bty),
            ctx.trait_name(&bound)
        ),
    ))
}

/// `x.m()` where a generic impl over `x`'s struct has `m` but a computed
/// argument of its header (`Q<{ 2 * L }>`) never takes `x`'s value.
fn unsolved_header(
    ctx: &LowerCtx<'_>,
    recv: &HirExpr,
    method: &str,
    span: nabic_diagnostic::Span,
) -> Option<HirExpr> {
    let HirTy::Struct(id) = &recv.ty else {
        return None;
    };
    let def = ctx.struct_def(*id);
    let base = enum_base_name(&def.name).to_owned();
    let env = ctx.env;
    let empty = HashMap::default();
    let mut hit = None;
    for gi in env.impls.generic_by(crate::lower::Pick::Method(method)) {
        let AstTyKind::Path(p) = &gi.self_ty.kind else {
            continue;
        };
        let Some(last) = p.segments.last() else {
            continue;
        };
        let names: nabic_core::HashSet<String> = gi
            .generics
            .params
            .iter()
            .map(|gp| match gp {
                nabic_ast::GenericParam::Type { name, .. }
                | nabic_ast::GenericParam::Const { name, .. } => name.clone(),
            })
            .collect();
        let mut rcx = LowerCtx::new(env, &empty, None, ctx.here());
        rcx.current_module.clone_from(&gi.module);
        let segs: Vec<&str> = p.segments.iter().map(|s| s.name.as_str()).collect();
        if rcx.canon_type(&segs).as_deref() != Some(base.as_str()) {
            continue;
        }
        for (a, v) in last.generics.iter().zip(&def.generic_args) {
            let (AstTyKind::Const(nabic_ast::ConstExpr::Expr(e)), HirTy::Const(v, _)) =
                (&a.kind, v)
            else {
                continue;
            };
            let free: Vec<String> = crate::walk::free_names(&[], e)
                .into_iter()
                .filter(|n| names.contains(n))
                .collect();
            if let [one] = free.as_slice() {
                if crate::lower::mono::solve_const(e, *v, &names).is_none() {
                    hit = Some((
                        gi,
                        crate::lower::collect::impl_def_display(a),
                        one.clone(),
                        *v,
                    ));
                    break;
                }
            }
        }
        if hit.is_some() {
            break;
        }
    }
    let (gi, arg, param, v) = hit?;
    let tr = gi.trait_key.strip_prefix("core::").unwrap_or(&gi.trait_key);
    let ty = ctx.ty_name(&recv.ty);
    let header = crate::lower::collect::impl_def_display(&gi.self_ty);
    let e = ctx.error_expr(
        Code::NO_METHOD,
        span,
        format!("no method `{method}` found for type `{ty}`"),
    );
    let of = if tr.is_empty() {
        format!("`impl {header}`")
    } else {
        format!("the impl of `{tr}` for `{header}`")
    };
    ctx.help_last(Some(format!(
        "{of} has it, but its argument `{arg}` is `{v}` for no integer `{param}`: that impl \
         covers only the values its header can produce"
    )));
    Some(e)
}

/// `xs.m(..)` on a collection without `m` is `xs.iter().m(..)` when
/// `xs` has a parameterless `iter()` whose iterator has `m`.
pub(crate) fn iter_forward(
    ctx: &mut LowerCtx<'_>,
    recv_h: &HirExpr,
    method: &str,
    span: nabic_diagnostic::Span,
) -> Option<HirExpr> {
    if method == "iter"
        || matches!(
            recv_h.ty,
            HirTy::Param(_) | HirTy::Unknown | HirTy::Sum(_) | HirTy::AnyTrait(_)
        )
    {
        return None;
    }
    if !matches!(pick_method(ctx, &recv_h.ty, "iter", None), Ok(Some(_))) {
        return None;
    }
    let since = ctx.env.sink.diagnostics.borrow().len();
    let it = method_call_on(ctx, recv_h.clone(), "iter", &[], span);
    let clean = ctx.env.sink.diagnostics.borrow().len() == since;
    if clean
        && it.ty != HirTy::Unknown
        && matches!(pick_method(ctx, &it.ty, method, None), Ok(Some(_)))
    {
        return Some(it);
    }
    ctx.env.sink.diagnostics.borrow_mut().truncate(since);
    None
}

/// The method a call `x.m(..)` names: `pick_method`, after `default_member`.
pub(crate) fn pick_call_method<'e>(
    ctx: &LowerCtx<'e>,
    ty: &HirTy,
    method: &str,
) -> Result<Option<MethodPick<'e>>, String> {
    pick_call_method_in(ctx, ty, method, false)
}

/// `pick_call_method`; `strict`: a call as written, where only the traits
/// in scope give the method.
pub(crate) fn pick_call_method_in<'e>(
    ctx: &LowerCtx<'e>,
    ty: &HirTy,
    method: &str,
    strict: bool,
) -> Result<Option<MethodPick<'e>>, String> {
    match default_member(ctx, ty, method) {
        Some(id) => Ok(Some(MethodPick::Concrete(id))),
        None if strict => pick_method_in(ctx, ty, method, None, TraitScope::Strict),
        None => pick_method(ctx, ty, method, None),
    }
}

/// In a trait's default body, `self.m(..)` with `m` of that trait is the
/// impl's own `m`: an inherent method of the same name does not win, nor
/// another impl of the trait for other arguments.
pub(crate) fn default_member(ctx: &LowerCtx<'_>, ty: &HirTy, method: &str) -> Option<FunctionId> {
    let (key, args) = ctx.default_of.as_ref()?;
    if ctx.self_ty.as_ref() != Some(ty) {
        return None;
    }
    let mut own = ctx
        .env
        .methods_on(ty, method)?
        .iter()
        .copied()
        .filter(|id| {
            ctx.env
                .method_info(*id)
                .is_some_and(|i| i.trait_name.as_ref() == Some(key) && i.trait_args == *args)
        });
    match (own.next(), own.next()) {
        (Some(id), None) => Some(id),
        _ => None,
    }
}

/// Keep only the diagnostics after the first `since` that `keep` accepts.
pub(crate) fn retain_since(
    ctx: &LowerCtx<'_>,
    since: usize,
    keep: impl Fn(&nabic_diagnostic::NabiDiagnostic) -> bool,
) {
    let mut diags = ctx.env.sink.diagnostics.borrow_mut();
    if since < diags.len() {
        let tail = diags.split_off(since);
        diags.extend(tail.into_iter().filter(keep));
    }
}

/// `a op= b` is `a.op_assign(b)` when `a`'s type implements the `op=`
/// trait; `None` otherwise, or for a primitive's `#[intrinsic]` impl, and
/// the caller writes `a = a op b`.
pub(crate) fn assign_trait_call(
    ctx: &mut LowerCtx<'_>,
    target: &Expr,
    op: &str,
    value: &Expr,
    span: nabic_diagnostic::Span,
) -> Option<HirExpr> {
    let entry = ctx.env.prec.lookup(op, Fixity::Infix)?;
    let method = entry.method_name.clone();
    let key = entry.trait_name.clone();
    ctx.env.trait_of(&key)?;
    let mark = ctx.probe_mark();
    let ty = lower_expected(ctx, target, HirTy::Unknown).ty;
    ctx.rollback(&mark);
    // An open literal takes `a = a op b`, where its operand meets it.
    if matches!(ty, HirTy::Unknown | HirTy::Var(_)) || !ctx.has_impl(&ty, &TraitRef::plain(&key), 0)
    {
        return None;
    }
    let env = ctx.env;
    let intrinsic = env
        .methods_on(&ty, &method)
        .is_some_and(|ids| ids.iter().any(|id| env.intrinsic(*id).is_some()));
    if intrinsic {
        return None;
    }
    // `{ let #t = a; #t.op_assign(b); a = #t; }`: `ref self` updates the
    // temp, written back to any place (`xs[i] op= b`, `s.f op= b`).
    let t = tmp_path(ctx, "oa", span);
    let call = Expr {
        kind: ExprKind::MethodCall {
            receiver: Box::new(t.clone()),
            method,
            type_args: Vec::new(),
            args: vec![CallArg::Positional(value.clone())],
            trailing_closure: None,
            origin: None,
            written: false,
        },
        span,
    };
    let stmts = vec![
        let_stmt(&t, target.clone()),
        Stmt {
            kind: StmtKind::Expr(call),
            span,
        },
        Stmt {
            kind: StmtKind::Assign(target.clone(), t),
            span,
        },
    ];
    let block = Expr {
        kind: ExprKind::Block(Block {
            stmts,
            trailing: None,
            span,
            label: None,
        }),
        span,
    };
    let mark = ctx.env.sink.diagnostics.borrow().len();
    let out = lower_expected(ctx, &block, HirTy::Unit);
    // The call is the operator's: say it in its terms.
    let call = format!("method `{}` on ", entry.method_name);
    for d in &mut ctx.env.sink.diagnostics.borrow_mut()[mark..] {
        if d.message.starts_with(&call) && d.message.contains("is ambiguous: several") {
            d.message = format!(
                "operator `{op}` on `{}` is ambiguous: several `{}` impls take this right \
                 operand",
                ctx.ty_name(&ty),
                leaf_name(&key)
            );
            d.notes.push(
                "help: give the right operand the type of one impl: annotate it, or add a \
                 suffix or `as`"
                    .to_owned(),
            );
        }
    }
    Some(out)
}

/// A trait implemented with several arguments (`Index<usize>` and
/// `Index<(usize, usize)>`, `IndexSet<I, T>` and `IndexSet<I, Array<T, 1>>`):
/// the one impl whose parameters take the arguments (tried in
/// rolled-back probes), or with no argument the one returning the
/// expected type.
pub(crate) fn pick_by_args<'e>(
    ctx: &mut LowerCtx<'e>,
    ty: &HirTy,
    method: &str,
    args: &[CallArg],
) -> Option<MethodPick<'e>> {
    let mut pos: Vec<&Expr> = Vec::new();
    for a in args {
        match a {
            CallArg::Positional(e) => pos.push(e),
            _ => return None,
        }
    }
    let expected = ctx.current_expected().cloned();
    if pos.is_empty() && expected.as_ref().is_none_or(|t| *t == HirTy::Unknown) {
        return None;
    }
    let env = ctx.env;
    // (trait, slots: the parameters' types after `self`, else the return type).
    let mut cands: Vec<(String, Vec<HirTy>, MethodPick<'e>)> = Vec::new();
    let ids = env.methods_on(ty, method);
    for id in ids.into_iter().flatten() {
        let tr = env.method_info(*id).and_then(|i| i.trait_name.clone())?;
        let sig = ctx.sig_by_id(*id);
        let slots: Option<Vec<HirTy>> = if pos.is_empty() {
            Some(vec![sig.ret_ty.clone()])
        } else {
            (1..=pos.len())
                .map(|i| sig.param_tys.get(i).cloned())
                .collect()
        };
        if let Some(slots) = slots {
            cands.push((tr, slots, MethodPick::Concrete(*id)));
        }
    }
    let gis: Vec<_> = ctx
        .generic_impls(ty, crate::lower::Pick::Method(method), 0)
        .collect();
    'impls: for (gi, subst) in gis {
        let gm = &gi.methods[method];
        let asts: Vec<Option<Ty>> = if pos.is_empty() {
            vec![Some(crate::lower::throws::sig_ret_ast(&gm.def))]
        } else {
            (1..=pos.len())
                .map(|i| gm.def.params.get(i).and_then(|p| p.ty.clone()))
                .collect()
        };
        let mut slots = Vec::with_capacity(asts.len());
        for p in asts {
            let Some(p) = p else {
                continue 'impls;
            };
            // A slot naming a parameter the receiver leaves open (`[T; N]`
            // of `impl<T, const N: usize> Tr<[T; N]> for T`) is no candidate.
            let mark = ctx.probe_mark();
            let mut rcx = LowerCtx::new(env, &subst, None, ctx.here());
            rcx.current_module.clone_from(&gi.module);
            let slot = rcx.resolve_ast_ty(&p);
            if !ctx.probe_clean(&mark) {
                ctx.rollback(&mark);
                continue 'impls;
            }
            slots.push(slot);
        }
        cands.push((gi.trait_key.clone(), slots, MethodPick::Generic(gm)));
    }
    if cands.len() < 2 || cands.iter().any(|c| c.0 != cands[0].0) {
        return None;
    }
    let mut hits = Vec::new();
    for (_, slots, pick) in cands {
        let ok = if pos.is_empty() {
            expected.as_ref() == slots.first()
        } else {
            pos.iter().zip(&slots).all(|(arg, slot)| {
                let mark = ctx.probe_mark();
                let h = lower_expected(ctx, arg, slot.clone());
                // An open literal (`let p = 1.0; a[i, j] = p`) fits a slot
                // it can still take, on either side (`v += w` with `v`'s
                // element type open).
                let open = crate::lower::infer::has_var(ctx, &h.ty)
                    || crate::lower::infer::has_var(ctx, slot);
                let ok = ctx.probe_clean(&mark)
                    && (coercible(ctx, &crate::lower::infer::zonk(ctx, slot), &h.ty)
                        || open
                            && crate::lower::infer::trial(ctx, &|| {
                                crate::lower::infer::agree(ctx, slot, &h.ty, false)
                            }));
                ctx.rollback(&mark);
                ok
            })
        };
        if ok {
            hits.push(pick);
        }
    }
    match hits[..] {
        [p] => Some(p),
        _ => None,
    }
}

/// An inherent method without `pub` is private to the module of its impl
/// block.
pub(crate) fn check_method_visible(ctx: &LowerCtx<'_>, fn_id: FunctionId, method: &str) {
    if let Some(info) = ctx.env.method_info(fn_id) {
        check_method_info_visible(ctx, info, method);
    }
}

/// Generic-impl method `method` for a receiver of type `ty`, looked up by
/// the receiver's generic base (`Box__i32` -> `Box`).
pub(crate) fn generic_method_for<'e>(
    ctx: &LowerCtx<'e>,
    ty: &HirTy,
    method: &str,
) -> Vec<&'e GenericMethod> {
    let (name, generic_args) = match ty {
        HirTy::Struct(id) => {
            let d = ctx.struct_def(*id);
            (d.name, d.generic_args)
        }
        HirTy::Enum(id) => {
            let d = ctx.enum_def(*id);
            (d.name, d.generic_args)
        }
        HirTy::Prim(p) => (p.name().to_owned(), Vec::new()),
        // `impl<T> GcBuf<T>`.
        HirTy::GcBuf(_) => (crate::lower::resolve::GC_BUF.to_owned(), Vec::new()),
        _ => return Vec::new(),
    };
    let base = if generic_args.is_empty() {
        name
    } else {
        enum_base_name(&name).to_owned()
    };
    ctx.env
        .generic_methods(&base, method)
        .map_or(&[][..], Vec::as_slice)
        .iter()
        .filter(|gm| gm.target.as_ref().is_none_or(|t| t == ty))
        .collect()
}

/// The bound's method over an inherent one of the same name in an
/// instance (`x.sqrt()` with `x: T`, `T: Root`, `T = Complex<f64>`).
fn bound_pick<'e>(ctx: &LowerCtx<'e>, ty: &HirTy, method: &str) -> Option<MethodPick<'e>> {
    if !has_inherent(ctx, ty, method) {
        return None;
    }
    let k = bound_trait_of(ctx, ty, method)?;
    pick_method(ctx, ty, method, Some(&k)).ok().flatten()
}

/// `ty` has an inherent method `method` (an `impl Ty { .. }` one).
fn has_inherent(ctx: &LowerCtx<'_>, ty: &HirTy, method: &str) -> bool {
    let env = ctx.env;
    env.methods_on(ty, method).is_some_and(|ids| {
        ids.iter()
            .any(|id| env.method_info(*id).is_some_and(|i| i.trait_name.is_none()))
    }) || generic_method_for(ctx, ty, method)
        .iter()
        .any(|gm| gm.info.trait_name.is_none())
        || ctx
            .generic_impls(ty, crate::lower::Pick::Method(method), 0)
            .any(|(gi, _)| gi.trait_key.is_empty())
}

/// The one bound trait declaring `method` of the type parameter this
/// instance binds to `ty` (`T: Root` with `T = Complex<f64>`).
fn bound_trait_of(ctx: &LowerCtx<'_>, ty: &HirTy, method: &str) -> Option<String> {
    let mut hits: Vec<&String> = ctx
        .ty_bounds
        .iter()
        .filter(|(n, _)| ctx.ty_subst.get(n.as_str()) == Some(ty))
        .flat_map(|(_, bs)| bs)
        .filter(|k| {
            ctx.env
                .trait_of(k)
                .is_some_and(|i| i.decls.contains_key(method))
        })
        .collect();
    hits.sort();
    hits.dedup();
    match hits.as_slice() {
        [one] => Some((*one).clone()),
        _ => None,
    }
}

/// A method candidate for a receiver type: a concrete function or a
/// generic template.
#[derive(Clone, Copy)]
pub(crate) enum MethodPick<'e> {
    Concrete(FunctionId),
    Generic(&'e GenericMethod),
}

/// Method `method` of `ty`, from `trait_key` if given: inherent first,
/// then methods of the traits in scope, where a trait defined or `use`d in
/// this package beats one seen only through the implicit `core::**`;
/// other ties are ambiguous.
pub(crate) fn pick_method<'e>(
    ctx: &LowerCtx<'e>,
    ty: &HirTy,
    method: &str,
    trait_key: Option<&str>,
) -> Result<Option<MethodPick<'e>>, String> {
    pick_method_in(ctx, ty, method, trait_key, TraitScope::Library)
}

/// Which traits' methods a lookup sees.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum TraitScope {
    /// Every trait.
    All,
    /// The traits in scope, and every toolchain trait: what the compiler
    /// itself calls (`index`, `into_iter`, `drop`).
    Library,
    /// The traits in scope and core's: `x.m(..)` as written.
    Strict,
}

/// `pick_method`; `scope` drops the candidates of traits out of it unless
/// `trait_key` names the trait.
pub(crate) fn pick_method_in<'e>(
    ctx: &LowerCtx<'e>,
    ty: &HirTy,
    method: &str,
    trait_key: Option<&str>,
    scope: TraitScope,
) -> Result<Option<MethodPick<'e>>, String> {
    let env = ctx.env;
    let mut cands: Vec<(Option<&'e str>, MethodPick<'e>)> = Vec::new();
    // The generic impls of the candidates, by index, with their bindings.
    let mut generic_impls: Vec<(usize, &'e Impl, HashMap<String, HirTy>)> = Vec::new();
    // A type parameter's bounds declare it; one candidate per bound trait.
    if let HirTy::Param(p) = ty {
        for gm in crate::lower::bounds::bound_methods(ctx, *p, method, trait_key) {
            cands.push((gm.info.trait_name.as_deref(), MethodPick::Generic(gm)));
        }
    }
    if cands.is_empty() {
        if let Some(ids) = env.methods_on(ty, method) {
            for id in ids {
                let tn = env.method_info(*id).and_then(|i| i.trait_name.as_deref());
                cands.push((tn, MethodPick::Concrete(*id)));
            }
        }
        for gm in generic_method_for(ctx, ty, method) {
            cands.push((gm.info.trait_name.as_deref(), MethodPick::Generic(gm)));
        }
        for (gi, subst) in ctx.generic_impls(ty, crate::lower::Pick::Method(method), 0) {
            // An empty key: a generic inherent impl (`impl<..> [T; N]`).
            let key = Some(gi.trait_key.as_str()).filter(|k| !k.is_empty());
            cands.push((key, MethodPick::Generic(&gi.methods[method])));
            if key.is_some() {
                generic_impls.push((cands.len() - 1, gi, subst));
            }
        }
    }
    if (trait_key.is_none() || ctx.want_trait.is_some()) && cands.len() > 1 {
        narrow_by_bound(ctx, ty, &mut cands, &generic_impls);
    }
    if let Some(k) = trait_key {
        cands.retain(|(t, _)| *t == Some(k));
    } else if scope != TraitScope::All && !matches!(ty, HirTy::Param(_)) {
        // Core's traits are the built-in types' own methods, in scope
        // everywhere; std's and the packages' take a `use`.
        let seen = |t: &str| match scope {
            TraitScope::Library => nabic_core::in_toolchain(t),
            _ => t.starts_with("core::"),
        };
        cands.retain(|(t, _)| {
            t.is_none_or(|t| t.is_empty() || seen(t) || trait_in_scope(ctx, t, true))
        });
    }
    if let Some((_, p)) = cands.iter().find(|(t, _)| t.is_none()) {
        return Ok(Some(*p));
    }
    if cands.len() > 1 {
        let own: Vec<_> = cands
            .iter()
            .filter(|(t, _)| t.is_some_and(|t| !core_only_trait(ctx, t)))
            .copied()
            .collect();
        if own.len() == 1 {
            return Ok(Some(own[0].1));
        }
        // A trait defined or `use`d here beats the others.
        let mut near = cands
            .iter()
            .filter(|(t, _)| t.is_some_and(|t| trait_in_scope(ctx, t, false)));
        if let (Some((_, p)), None) = (near.next(), near.next()) {
            return Ok(Some(*p));
        }
        let mut traits: Vec<String> = cands
            .iter()
            .map(|(t, _)| format!("`{}`", t.unwrap_or_default()))
            .collect();
        traits.sort();
        traits.dedup();
        let leaf = |t: &str| leaf_name(t).to_owned();
        // Overlapping impls are reported where they are declared.
        let overlap = ctx
            .env
            .sink
            .diagnostics
            .borrow()
            .iter()
            .any(|d| d.code == Code::CONFLICTING_IMPLS);
        if traits.len() == 1 && overlap {
            return Ok(Some(cands[0].1));
        }
        if let [one] = traits.as_slice() {
            // A generic trait's impls differ in its arguments: name them.
            let key = cands[0].0.unwrap_or_default();
            if ctx.env.trait_of(key).is_some_and(|i| !i.params.is_empty()) {
                let params = ctx
                    .env
                    .trait_of(key)
                    .map(|i| i.params.join(", "))
                    .unwrap_or_default();
                let tr = leaf(key);
                return Err(format!(
                    "method `{method}` on `{}` is ambiguous: several {one} impls provide it - \
                     give the argument the type of one impl (annotate it, or add a suffix or \
                     `as`), or name the trait's arguments of one: `{tr}::<{params}>::{method}(x)`",
                    ctx.ty_name(ty),
                ));
            }
            return Err(format!(
                "method `{method}` on `{}` is ambiguous: several {one} impls provide it - give \
                 the argument the type of one impl (annotate it, or add a suffix or `as`)",
                ctx.ty_name(ty),
            ));
        }
        return Err(format!(
            "method `{method}` on `{}` is ambiguous: traits {} all provide it - call it as \
             `Trait::{method}(x)` (e.g. `{}::{method}(x)`)",
            ctx.ty_name(ty),
            traits.join(", "),
            leaf(cands[0].0.unwrap_or_default())
        ));
    }
    Ok(cands.first().map(|(_, p)| *p))
}

/// In an instance of `fn f<X: Conv<f64>>`, `x.conv()` with `X = List<i64>`
/// is the impl of `Conv<f64>`, not another `Conv` impl of `List<i64>`:
/// keep the candidates whose trait arguments the bound (or the call's
/// `Conv::<f64>`) names.
fn narrow_by_bound(
    ctx: &LowerCtx<'_>,
    ty: &HirTy,
    cands: &mut Vec<(Option<&str>, MethodPick<'_>)>,
    generic_impls: &[(usize, &Impl, HashMap<String, HirTy>)],
) {
    let wants: Vec<TraitRef> = ctx
        .ty_bounds
        .keys()
        .filter(|n| ctx.ty_subst.get(n.as_str()) == Some(ty))
        .flat_map(|n| ctx.bound_wants(n))
        .chain(ctx.want_trait.clone())
        .filter(|w| !w.args.is_empty())
        .collect();
    if wants.is_empty() {
        return;
    }
    // A generic impl's trait arguments under its bindings; resolving them
    // reports nothing (an argument the receiver leaves open is `Unknown`).
    let since = ctx.env.sink.diagnostics.borrow().len();
    let generic_args: Vec<(usize, Vec<HirTy>)> = generic_impls
        .iter()
        .map(|(i, gi, subst)| (*i, ctx.generic_impl_ref(gi, subst).args))
        .collect();
    ctx.env.sink.diagnostics.borrow_mut().truncate(since);
    let args_of = |i: usize, p: &MethodPick<'_>| -> Option<Vec<HirTy>> {
        match p {
            MethodPick::Concrete(id) => ctx.env.method_info(*id).map(|m| m.trait_args.clone()),
            MethodPick::Generic(gm) => generic_args
                .iter()
                .find(|(j, _)| *j == i)
                .map(|(_, a)| a.clone())
                .or_else(|| gm.target.is_some().then(|| gm.info.trait_args.clone())),
        }
    };
    let fits = |t: &str, have: &[HirTy]| {
        wants.iter().any(|w| {
            w.path == t
                && w.args.len() <= have.len()
                && w.args
                    .iter()
                    .zip(have)
                    .all(|(a, b)| a == b || *a == HirTy::Unknown || *b == HirTy::Unknown)
        })
    };
    let keep: Vec<bool> = cands
        .iter()
        .enumerate()
        .map(|(i, (t, p))| match (t, args_of(i, p)) {
            (Some(t), Some(have)) if wants.iter().any(|w| w.path == *t) => fits(t, &have),
            _ => true,
        })
        .collect();
    if keep.iter().any(|k| *k) && keep.iter().any(|k| !*k) {
        let mut it = keep.iter();
        cands.retain(|_| *it.next().unwrap_or(&true));
    }
}

/// Is trait `key` from another package and not `use`d here (seen only
/// through the implicit core glob, or a user trait seen from core)?
pub(crate) fn core_only_trait(ctx: &LowerCtx<'_>, key: &str) -> bool {
    let pkg = crate::lower::resolve::package_of;
    if pkg(&module_of(key)) == pkg(&ctx.access_module()) {
        return false;
    }
    let named = |e: &crate::uses::UseEntry| match e {
        crate::uses::UseEntry::Single { full, .. } => full == key,
        crate::uses::UseEntry::Glob { prefix } => module_of(key) == *prefix,
        crate::uses::UseEntry::GlobDeep { prefix } => key.starts_with(&format!("{prefix}::")),
    };
    !ctx.reach_uses()
        .iter()
        .flatten()
        .chain(ctx.env.uses_of(&ctx.access_module()).into_iter().flatten())
        .any(named)
}

/// Is trait `key` in scope here: defined in this module, `use`d (also
/// through a `pub use`), a bound of a type parameter, or (`implicit`)
/// brought in by the implicit import (the prelude; in the library, core)?
pub(crate) fn trait_in_scope(ctx: &LowerCtx<'_>, key: &str, implicit: bool) -> bool {
    if module_of(key) == *ctx.access_module()
        // A bound that failed to resolve keeps its bare name (reported).
        || ctx
            .ty_bounds
            .values()
            .flatten()
            .any(|b| b == key || (!b.contains("::") && b == leaf_name(key)))
    {
        return true;
    }
    let names = &ctx.env.names;
    // Follow `pub use` aliases to the trait they name.
    let is_key = |k: &str| {
        let mut k = k.to_owned();
        for _ in 0..8 {
            if k == key {
                return true;
            }
            match names.reexport(&k) {
                Some(n) => k.clone_from(n),
                None => return false,
            }
        }
        false
    };
    let leaf = leaf_name(key);
    let named = |e: &crate::uses::UseEntry| match e {
        crate::uses::UseEntry::Single { full, .. } => is_key(full),
        crate::uses::UseEntry::Glob { prefix } => {
            module_of(key) == *prefix || is_key(&format!("{prefix}::{leaf}"))
        }
        crate::uses::UseEntry::GlobDeep { prefix } => key.starts_with(&format!("{prefix}::")),
    };
    let implicit = implicit
        .then(|| crate::lower::implicit_use(&ctx.access_module()))
        .flatten();
    ctx.reach_uses()
        .iter()
        .flatten()
        .chain(ctx.env.uses_of(&ctx.access_module()).into_iter().flatten())
        .chain(implicit.as_ref())
        .any(named)
}

/// Report a method of `ty` that only a trait out of scope provides,
/// with the `use` that brings the trait in; false if there is none.
pub(crate) fn trait_out_of_scope(ctx: &LowerCtx<'_>, ty: &HirTy, method: &str, at: Span) -> bool {
    if matches!(ty, HirTy::Param(_)) {
        return false;
    }
    let key = match pick_method_in(ctx, ty, method, None, TraitScope::All) {
        Ok(Some(MethodPick::Concrete(id))) => {
            ctx.env.method_info(id).and_then(|i| i.trait_name.clone())
        }
        Ok(Some(MethodPick::Generic(gm))) => gm.info.trait_name.clone(),
        _ => None,
    };
    let Some(key) = key.filter(|k| !k.is_empty()) else {
        return false;
    };
    // Another package's trait by its public path (`linalg::AsVector`).
    let shown = ctx.env.names.visible_path(&key);
    ctx.emit_error_at(
        Code::TRAIT_NOT_IN_SCOPE,
        at,
        format!("method `{method}` comes from trait `{shown}`, which is not in scope"),
    );
    let path = crate::lower::resolve::use_path(&ctx.access_module(), shown);
    ctx.help_last(Some(format!("add `use {path};`")));
    true
}

pub(crate) fn check_method_info_visible(ctx: &LowerCtx<'_>, info: &MethodInfo, method: &str) {
    // The generated harness builds the library's `Bench` itself.
    if ctx.harness_main {
        return;
    }
    if !ctx.is_visible(&info.module, info.vis) {
        ctx.emit_error(
            Code::PRIVATE_ITEM,
            format!(
                "method `{method}` is private to module `{}` and cannot be called from `{}` \
                 (add `{}` to expose it)",
                module_display(&info.module),
                module_display(&ctx.current_module),
                crate::lower::resolve::needed_vis(&ctx.current_module, &info.module).0
            ),
        );
    }
}
