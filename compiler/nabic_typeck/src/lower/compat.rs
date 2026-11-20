//! Type compatibility, coercion and mismatch help.

#[allow(clippy::wildcard_imports)]
use crate::lower::*;

/// The lock guard `ty` is, if any. Only `using` releases one;
/// the module defining guards hands them over raw.
pub(crate) fn lock_guard(ctx: &LowerCtx<'_>, ty: &HirTy) -> Option<&'static str> {
    let HirTy::Struct(id) = ty else { return None };
    if ctx.current_module == "core::sync" {
        return None;
    }
    let def = ctx.struct_def(*id);
    let name = def.name.split("__").next().unwrap_or(&def.name);
    ["MutexGuard", "RwLockReadGuard", "RwLockWriteGuard"]
        .into_iter()
        .find(|g| name.strip_prefix("core::sync::") == Some(g))
}

/// If `slot_ty` is a Sum and `value.ty` matches one of its variants,
/// wrap the value into the sum's tagged-union representation.
pub(crate) fn wrap_into_sum(slot_ty: &HirTy, value: HirExpr) -> HirExpr {
    let HirTy::Sum(variants) = slot_ty else {
        return value;
    };
    let Some(idx) = sum_variant_index(variants, &value.ty) else {
        // A nested optional: a value of the inner type's
        // members goes in wrapped by the inner sum.
        return match variants.iter().find(|v| nested_fits(v, &value.ty)) {
            Some(inner) => wrap_into_sum(slot_ty, wrap_into_sum(inner, value)),
            None => value,
        };
    };
    let span = value.span;
    if variants[idx].is_none() {
        return wrap_none(slot_ty, idx, value);
    }
    HirExpr {
        kind: HirExprKind::SumWrap {
            sum_ty: slot_ty.clone(),
            variant_index: u32::try_from(idx).expect("variant overflow"),
            value: Some(Box::new(value)),
        },
        ty: slot_ty.clone(),
        span,
    }
}

/// `value: none` as member `idx` of `slot_ty`. The value may have effects
/// (`{ f(); none }`), so the wrap moves into its branches or the value
/// runs first.
pub(crate) fn wrap_none(slot_ty: &HirTy, idx: usize, mut value: HirExpr) -> HirExpr {
    let span = value.span;
    let tag = HirExpr {
        kind: HirExprKind::SumWrap {
            sum_ty: slot_ty.clone(),
            variant_index: u32::try_from(idx).expect("variant overflow"),
            value: None,
        },
        ty: slot_ty.clone(),
        span,
    };
    let wrap_block = |b: &mut HirBlock| {
        if let Some(t) = b.trailing.take() {
            b.trailing = Some(wrap_none(slot_ty, idx, t));
        }
    };
    match &mut value.kind {
        HirExprKind::Const(_) | HirExprKind::Local(_) | HirExprKind::UnresolvedPath(_) => tag,
        HirExprKind::Block(b) => {
            wrap_block(b);
            value.ty = slot_ty.clone();
            value
        }
        HirExprKind::If {
            then_block,
            else_branch: Some(e),
            ..
        } => {
            wrap_block(then_block);
            let inner = std::mem::replace(&mut **e, tag);
            **e = wrap_none(slot_ty, idx, inner);
            value.ty = slot_ty.clone();
            value
        }
        HirExprKind::Match { arms, .. } => {
            for a in arms.iter_mut() {
                let body = std::mem::replace(&mut a.body, tag.clone());
                a.body = wrap_none(slot_ty, idx, body);
            }
            value.ty = slot_ty.clone();
            value
        }
        _ => HirExpr {
            kind: HirExprKind::Block(Box::new(HirBlock {
                stmts: vec![HirStmt::Expr(value)],
                trailing: Some(tag),
                label: None,
            })),
            ty: slot_ty.clone(),
            span,
        },
    }
}

/// Re-tag a sum as `to`, member by member. Widening is total; arms left
/// out for a sub-sum type pattern can't occur.
pub(crate) fn resum(ctx: &mut LowerCtx<'_>, value: HirExpr, to: &HirTy) -> HirExpr {
    // An `any` value (narrowed by `x: A | B`) is matched by tag.
    let members: Vec<(u32, HirTy)> = match (&value.ty, to) {
        (HirTy::Sum(vs), HirTy::Sum(ws)) => vs
            .iter()
            .enumerate()
            .filter(|(_, v)| ws.contains(v))
            .map(|(i, v)| (u32::try_from(i).expect("variant overflow"), v.clone()))
            .collect(),
        // A stored `to` value, or one of its members.
        (HirTy::AnyTrait(_), HirTy::Sum(ws)) => std::iter::once(to)
            .chain(ws)
            .map(|w| (ctx.any_tag(w), w.clone()))
            .collect(),
        // A plain member: wrap it.
        _ => return wrap_into_sum(to, value),
    };
    let span = value.span;
    let arms = members
        .into_iter()
        .map(|(i, v)| {
            let b = ctx.declare(None, v.clone());
            HirMatchArm {
                pat: HirPattern::SumVariant {
                    sum_ty: value.ty.clone(),
                    variant_index: i,
                    binder: Some(b),
                    payload_ty: Some(v.clone()),
                },
                guard: None,
                body: wrap_into_sum(to, hir_local(b, &v, span)),
            }
        })
        .collect();
    HirExpr {
        kind: HirExprKind::Match {
            scrutinee: Box::new(value),
            arms,
        },
        ty: to.clone(),
        span,
    }
}

/// Does nested sum `member` take a `value_ty` by injection (`i32` or
/// `none` into `(i32 | none)?`)?
pub(crate) fn nested_fits(member: &HirTy, value_ty: &HirTy) -> bool {
    match member {
        HirTy::Sum(ws) => {
            sum_variant_index(ws, value_ty).is_some() || ws.iter().any(|w| nested_fits(w, value_ty))
        }
        _ => false,
    }
}

/// Locate `value_ty` in a sum's variant list (structural equality).
pub(crate) fn sum_variant_index(variants: &[HirTy], value_ty: &HirTy) -> Option<usize> {
    variants.iter().position(|v| v == value_ty)
}

/// Can `actual` flow into `expected` (after `wrap_into_sum`)? `Unknown`
/// (already reported) and `Any` fit everything.
pub(crate) fn ty_compatible(expected: &HirTy, actual: &HirTy) -> bool {
    let all = |a: &[HirTy], b: &[HirTy]| {
        a.len() == b.len() && a.iter().zip(b).all(|(x, y)| ty_compatible(x, y))
    };
    let exact = |a: &[HirTy], b: &[HirTy]| {
        a.len() == b.len() && a.iter().zip(b).all(|(x, y)| ty_exact(x, y))
    };
    match (expected, actual) {
        // `!` (a diverging expression) fits any slot.
        (HirTy::Unknown | HirTy::Any, _) | (_, HirTy::Unknown | HirTy::Any | HirTy::Never) => true,
        (HirTy::Sum(vs), a) if !matches!(a, HirTy::Sum(_)) => sum_variant_index(vs, a).is_some(),
        (HirTy::Tuple(a), HirTy::Tuple(b)) | (HirTy::Sum(a), HirTy::Sum(b)) => all(a, b),
        (HirTy::Array(a, n), HirTy::Array(b, m)) => n == m && ty_compatible(a, b),
        (HirTy::GcBuf(a), HirTy::GcBuf(b)) => ty_compatible(a, b),
        // No fn subtyping, except `fn() + Send` into `fn()`.
        // A `fn() -> !` isn't a `fn() -> R` (different return ABI); only
        // `adapt_never_fn` makes one from a named fn.
        (HirTy::Fn(a1, r1, b1), HirTy::Fn(a2, r2, b2)) => {
            exact(a1, a2)
                && ty_exact(r1, r2)
                && (**r2 != HirTy::Never || **r1 == HirTy::Never)
                && fn_bounds_fit(b1, b2)
        }
        // A closure is a value of its `fn` type (`coerce`
        // checks a bound against its captures).
        (
            HirTy::Fn(a1, r1, _),
            HirTy::Closure {
                args: a2, ret: r2, ..
            },
        ) => exact(a1, a2) && ty_exact(r1, r2),
        // Error types must match exactly: a wider error slot has another
        // carrier layout (fn values are adapted by `adapt_fn_value`).
        (HirTy::Throws { ok: o1, err: e1 }, HirTy::Throws { ok: o2, err: e2 }) => {
            ty_compatible(o1, o2) && (e1 == e2 || **e1 == HirTy::Unknown || **e2 == HirTy::Unknown)
        }
        _ => expected == actual,
    }
}

/// Type identity inside a fn type: `ty_compatible` without sum
/// widening (an unresolved type still matches anything).
pub(crate) fn ty_exact(a: &HirTy, b: &HirTy) -> bool {
    let all = |a: &[HirTy], b: &[HirTy]| {
        a.len() == b.len() && a.iter().zip(b).all(|(x, y)| ty_exact(x, y))
    };
    match (a, b) {
        (HirTy::Unknown | HirTy::Any, _) | (_, HirTy::Unknown | HirTy::Any) => true,
        (HirTy::Tuple(a), HirTy::Tuple(b)) | (HirTy::Sum(a), HirTy::Sum(b)) => all(a, b),
        (HirTy::Array(a, n), HirTy::Array(b, m)) => n == m && ty_exact(a, b),
        (HirTy::GcBuf(a), HirTy::GcBuf(b)) => ty_exact(a, b),
        (HirTy::Fn(..), HirTy::Fn(..) | HirTy::Closure { .. }) => ty_compatible(a, b),
        (HirTy::Throws { ok: o1, err: e1 }, HirTy::Throws { ok: o2, err: e2 }) => {
            ty_exact(o1, o2) && ty_exact(e1, e2)
        }
        _ => a == b,
    }
}

/// Wrap `value` into `slot` (sum injection) and report a type mismatch.
/// `what` names the slot for the diagnostic ("argument 1 of `f`").
pub(crate) fn coerce(
    ctx: &mut LowerCtx<'_>,
    slot: &HirTy,
    value: HirExpr,
    what: &dyn Fn() -> String,
) -> HirExpr {
    let ann = ctx.let_ann.take();
    let mut value = value;
    if infer::unify_value(ctx, slot, &mut value) {
        return value;
    }
    if let Some(d) = infer::let_literal_conflict(ctx, slot, &value) {
        ctx.env.sink.diagnostics.borrow_mut().push(d);
        return value;
    }
    // Linked variables compare as their roots below.
    let slot = &infer::resolve(ctx, slot);
    let value = expr::retype_default_literal(ctx, slot, value);
    // A diverging block has type `!`: it fits any slot.
    if hir_expr_diverges(&value) {
        return value;
    }
    let value = c_fn_value(ctx, slot, value);
    let value = adapt_never_fn(ctx, slot, value);
    let value = adapt_fn_value(ctx, slot, value);
    let value = box_into_any(ctx, slot, value);
    let value = drop_fn_bounds(slot, value);
    let value = expr::list_into_view(ctx, slot, value);
    // A sum flows into any sum holding all its members; one
    // that is a layer of the slot goes into that layer.
    let value = match (&value.ty, slot) {
        (HirTy::Sum(vs), HirTy::Sum(ws))
            if vs != ws && !ws.contains(&value.ty) && vs.iter().all(|v| ws.contains(v)) =>
        {
            resum(ctx, value, slot)
        }
        // Into a nested optional's inner sum: a sub-sum of
        // it re-tagged, then wrapped by the outer layer.
        (HirTy::Sum(vs), HirTy::Sum(ws)) if !ws.contains(&value.ty) => {
            let inner = ws
                .iter()
                .find(|w| matches!(w, HirTy::Sum(is) if vs.iter().all(|v| is.contains(v))));
            match inner.cloned() {
                Some(inner) => resum(ctx, value, &inner),
                None => value,
            }
        }
        _ => value,
    };
    // A closure into `fn(..) | none`: first into its `fn` member.
    let value = match closure_into_member(ctx, slot, value, what) {
        Ok(v) => v,
        Err(v) => return v,
    };
    let mut v = wrap_into_sum(slot, value);
    // Same `{code, env}` layout: the closure takes the slot's type once
    // its captures meet the slot's bounds (`+ Send`).
    let closure_fits = |v: &HirExpr| match (slot, &v.ty) {
        (HirTy::Fn(_, _, want), HirTy::Closure { .. }) => {
            let held = infer::zonk(ctx, &v.ty);
            ty_compatible(&infer::zonk(ctx, slot), &held)
                && want.auto.iter().all(|b| ctx.has_trait(&held, b))
        }
        _ => false,
    };
    if closure_fits(&v) {
        v.ty = slot.clone();
    }
    // An unknown type name is reported on its own.
    let unknown_slot = matches!(slot, HirTy::Path(_)) && {
        let mut names = Vec::new();
        collect::collect_unknown_paths(slot, &mut names);
        !names.is_empty()
    };
    if !unknown_slot
        && (!ty_compatible(slot, &v.ty)
            || matches!(v.ty, HirTy::Closure { .. } if matches!(slot, HirTy::Fn(..))))
    {
        if let (HirTy::Fn(_, _, want), Some((name, x))) = (slot, ctx.ref_capture(&v)) {
            if !want.auto.is_empty() {
                ctx.emit_error(
                    Code::THREAD_SAFETY,
                    format!(
                        "`[ref {name}]` can't be used in a closure that may run on another \
                         thread: both threads could change `{name}` at once"
                    ),
                );
                if let Some(d) = ctx.env.sink.diagnostics.borrow_mut().last_mut() {
                    // An open literal (`let count = 0;`) helps as its default.
                    let x = infer::zonk(ctx, &x);
                    let x = infer::literal_default(ctx, &x).unwrap_or(x);
                    d.notes.push(ref_capture_help(&name, &x));
                }
                return v;
            }
        }
        let hint = fn_mismatch_hint(slot, &v.ty);
        // Underline the value, not the statement holding it.
        let here = ctx.here();
        let at = if !v.span.is_dummy() && v.span.file == here.file && here.start <= v.span.start {
            v.span
        } else {
            here
        };
        ctx.emit_error_at(
            Code::MISMATCHED_TYPES,
            at,
            format!(
                "mismatched types: {} expects `{}`, found `{}`{hint}",
                what(),
                ctx.ty_name(slot),
                ctx.ty_name(&v.ty)
            ),
        );
        convert_help(ctx, slot, &v, ann, &what());
        // `let d: Dict<K, V> = {};` (Python, JS): `{}` is an empty block.
        let slot_name = ctx.ty_name(slot);
        let base = slot_name.split('<').next().unwrap_or_default();
        if v.ty == HirTy::Unit && matches!(base, "Dict" | "Set") && ctx.src(v.span) == Some("{}") {
            ctx.help_last(Some(format!(
                "`{{}}` is an empty block, not an empty `{base}`; write `{base}::new()`"
            )));
        }
        // `let fs = []; fs.push(|x| x + k); fs` into `List<fn(..)>`: the
        // closures took their own type where the list was made.
        let name = ctx
            .src(v.span)
            .filter(|s| s.chars().all(|c| c.is_alphanumeric() || c == '_'));
        if let Some(name) = name.filter(|_| closure_for_fn(ctx, slot, &v.ty, 0)) {
            ctx.help_last(Some(format!(
                "the closures in `{name}` keep their own type unless `{name}` is typed where \
                 it is made: `let {name}: {slot_name} = ..;`"
            )));
        }
        mismatch_help(ctx, slot, &v);
        if let (Some((note, name, send)), Some(d)) = (
            ctx.closure_bound_note(&v),
            ctx.env.sink.diagnostics.borrow_mut().last_mut(),
        ) {
            d.notes.push(note);
            d.notes
                .push(capture_fix(ctx, slot, &v, &name, send, &what()));
        }
    }
    v
}

/// What to write for a closure capture `name` that does not meet the
/// slot's bounds (`send`: it lacks `Sync` only).
fn capture_fix(
    ctx: &LowerCtx<'_>,
    slot: &HirTy,
    v: &HirExpr,
    name: &str,
    send: bool,
    what: &str,
) -> String {
    // A plain capture stays shared with the creator; a moved one
    // (or an argument) is handed over instead, and needs only `Send`.
    let sync = ctx.lang("sync_trait");
    let wants_sync = matches!(slot, HirTy::Fn(_, _, b) if b.auto.iter().any(|k| k == sync));
    let fix = ctx.capture_help(v);
    if send && !wants_sync {
        let arg = if what.ends_with("`spawn_blocking`") {
            format!(
                ", or pass it as the argument: `task::spawn_blocking_with_arg({name}, \
                 |{name}| ..)`"
            )
        } else {
            String::new()
        };
        let frozen = if fix.is_some_and(|h| h.starts_with("help: share")) {
            format!("; to share it with no copy, freeze it first: `let {name} = {name}.freeze();`")
        } else {
            String::new()
        };
        return format!(
            "help: move `{name}` into the closure, which then owns its own copy: \
             `[move {name}] || ..`{arg}{frozen}"
        );
    }
    fix.unwrap_or_else(|| {
        "help: drop the capture: compute the value inside the closure, or capture a copy \
         that is `Send + Sync` (an array, not a collection)"
            .to_owned()
    })
}

/// A closure for a sum slot as the slot's `fn` member, to wrap; `Err`
/// when that conversion was reported.
fn closure_into_member(
    ctx: &mut LowerCtx<'_>,
    slot: &HirTy,
    value: HirExpr,
    what: &dyn Fn() -> String,
) -> Result<HirExpr, HirExpr> {
    let (HirTy::Closure { .. }, HirTy::Sum(ms)) = (&value.ty, slot) else {
        return Ok(value);
    };
    let member = ms
        .iter()
        .find(|m| matches!(m, HirTy::Fn(..)) && ty_compatible(m, &value.ty))
        .cloned();
    let Some(m) = member else {
        return Ok(value);
    };
    let since = ctx.env.sink.diagnostics.borrow().len();
    let v = coerce(ctx, &m, value, what);
    if ctx.env.sink.diagnostics.borrow().len() > since {
        return Err(v);
    }
    Ok(v)
}

/// A closure value as its plain `fn` type when `has` holds for that type
/// but not for the closure's own (an impl on `fn(..) -> R`): a closure is
/// a value of its `fn` type.
pub(crate) fn closure_as_fn(
    ctx: &mut LowerCtx<'_>,
    v: HirExpr,
    has: &dyn Fn(&LowerCtx<'_>, &HirTy) -> bool,
) -> HirExpr {
    let HirTy::Closure { args, ret, .. } = &v.ty else {
        return v;
    };
    let plain = HirTy::Fn(args.clone(), ret.clone(), crate::FnBounds::default());
    if has(ctx, &v.ty) || !has(ctx, &plain) {
        return v;
    }
    coerce(ctx, &plain, v, &|| "the closure".to_owned())
}

/// "`f` takes 2 arguments but 1 was supplied", and a help showing the
/// signature `sig` (`f(a: i32, b: i32)`) when known.
pub(crate) fn arity_msg(
    callee: &str,
    want: usize,
    got: usize,
    sig: Option<String>,
) -> (String, Option<String>) {
    let callee = short_callee(callee);
    let args = |n: usize| {
        if n == 1 {
            "1 argument".to_owned()
        } else {
            format!("{n} arguments")
        }
    };
    let was = if got == 1 { "was" } else { "were" };
    let msg = format!("`{callee}` takes {} but {got} {was} supplied", args(want));
    let help = sig.map(|s| format!("it is declared `{s}`; pass {}", args(want)));
    (msg, help)
}

/// A standard library function by its own name (`println`), a method
/// by its type's (`List::push`).
pub(crate) fn short_callee(callee: &str) -> std::borrow::Cow<'_, str> {
    if !nabic_core::in_toolchain(callee) {
        return callee.into();
    }
    let leaf = leaf_name(callee);
    match leaf.split_once("__") {
        Some((ty, method)) => format!("{ty}::{method}").into(),
        None => leaf.into(),
    }
}

/// `f(a: i32, b: i32)` from parameter names and types; `self` and
/// unnamed (`#1`) parameters show their type only, or nothing.
pub(crate) fn sig_text<'n>(
    callee: &str,
    params: impl IntoIterator<Item = (&'n str, String)>,
) -> String {
    let parts: Vec<String> = params
        .into_iter()
        .filter(|(n, _)| *n != "self")
        .map(|(n, t)| {
            if n.starts_with('#') || n.is_empty() {
                t
            } else {
                format!("{n}: {t}")
            }
        })
        .collect();
    format!("{}({})", short_callee(callee), parts.join(", "))
}

/// `have` is `want` with a closure type inside a type argument where
/// `want` has a `fn` type (`List<closure(i64) -> i64>` for
/// `List<fn(i64) -> i64>`).
pub(crate) fn closure_for_fn(ctx: &LowerCtx<'_>, want: &HirTy, have: &HirTy, depth: u32) -> bool {
    match (want, have) {
        (HirTy::Fn(..), HirTy::Closure { .. }) => depth > 0,
        (HirTy::Struct(a), HirTy::Struct(b)) => {
            let (a, b) = (ctx.struct_def(*a), ctx.struct_def(*b));
            expr::enum_base_name(&a.name) == expr::enum_base_name(&b.name)
                && a.generic_args.len() == b.generic_args.len()
                && a.generic_args
                    .iter()
                    .zip(&b.generic_args)
                    .any(|(w, h)| closure_for_fn(ctx, w, h, depth + 1))
        }
        (HirTy::Tuple(ws), HirTy::Tuple(hs)) if ws.len() == hs.len() => ws
            .iter()
            .zip(hs)
            .any(|(w, h)| closure_for_fn(ctx, w, h, depth + 1)),
        (HirTy::Array(w, n), HirTy::Array(h, m)) if n == m => closure_for_fn(ctx, w, h, depth + 1),
        _ => false,
    }
}

/// Help for a value of type `v.ty` in a `slot` it does not fit: an
/// optional where its value is wanted, or a collection of other elements.
pub(crate) fn mismatch_help(ctx: &LowerCtx<'_>, slot: &HirTy, v: &HirExpr) {
    let src = suggest::short_src(ctx, v.span);
    if suggest::some_part(&v.ty).as_ref() == Some(slot) {
        // `try?` over an optional nests.
        if let Some(s) = src.filter(|s| {
            s.starts_with("try?") && matches!(slot, HirTy::Sum(ts) if ts.iter().any(HirTy::is_none))
        }) {
            let fix = format!("({s}) ?? none");
            ctx.fix_last(v.span, fix.clone(), format!("use `{fix}`"));
            ctx.help_last(Some(format!(
                "`try?` keeps a thrown error apart from a returned `none`: write `{fix}` to \
                 merge them, or annotate `{}`",
                ctx.ty_name(&v.ty)
            )));
            return;
        }
        let name = src.unwrap_or("v");
        let help = match (src, suggest::default_lit(slot)) {
            (Some(s), Some(d)) => {
                let fix = format!("{s} ?? {d}");
                ctx.fix_last(v.span, fix.clone(), format!("use `{fix}`"));
                format!("`{s}` may be none: write `{fix}`, or `if let x = {s} {{ .. }}`")
            }
            _ => format!(
                "`{name}` may be none: {}",
                suggest::optional_forms(ctx, name, slot)
            ),
        };
        ctx.help_last(Some(help));
        return;
    }
    let base = |n: &str| expr::enum_base_name(n).to_owned();
    let pointer = |n: &str| {
        let b = base(n);
        let leaf = b.rsplit("::").next().unwrap_or(&b).to_owned();
        matches!(leaf.as_str(), "Ptr" | "ConstPtr" | "CPtr" | "ManyPtr")
    };
    // Text where C wants a pointer.
    if let (HirTy::Struct(a), HirTy::Prim(PrimTy::Str)) = (slot, &v.ty) {
        if pointer(&ctx.struct_def(*a).name) {
            let s = src.unwrap_or("s");
            ctx.help_last(Some(format!(
                "C takes text as a NUL-terminated pointer: pass `CString::new({s})?.as_ptr()` \
                 for a `char*`, or `WideCString::new({s})?.as_ptr()` for a `wchar_t*` \
                 (`bare::ffi`), and keep the value alive across the call"
            )));
        }
        return;
    }
    let (HirTy::Struct(a), HirTy::Struct(b)) = (slot, &v.ty) else {
        return;
    };
    let (want, have) = (ctx.struct_def(*a), ctx.struct_def(*b));
    if base(&want.name) != base(&have.name) || want.generic_args.len() != 1 {
        return;
    }
    let (Some(to), Some(from)) = (want.generic_args.first(), have.generic_args.first()) else {
        return;
    };
    if pointer(&want.name) {
        ctx.help_last(Some(format!(
            "the pointers point at different types: build the data as `{t}` (a `wchar_t*` is \
             `ConstPtr<c_wchar>`, from `WideCString`), or reinterpret the address with \
             `ConstPtr::<{t}>::dangling().with_addr(p.addr())` if the memory really holds them",
            t = ctx.ty_name(to),
        )));
        return;
    }
    let num = |t: &HirTy| matches!(t, HirTy::Prim(p) if p.is_int() || p.is_float());
    if !num(to) || !num(from) {
        return;
    }
    let list = ["List", "Vector", "Slice", "Deque", "Set"].contains(&leaf_name(&base(&want.name)));
    let plain = src.is_some_and(|s| {
        s.chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
    });
    if list && plain {
        let xs = src.unwrap_or("xs");
        ctx.help_last(Some(format!(
            "convert the elements: `{xs}.iter().map(|x| x as {}).collect()`",
            ctx.ty_name(to)
        )));
    } else if !list {
        ctx.help_last(same_elem_help(ctx, slot, src));
    }
}

/// Two instances of one generic type with different element types
/// (`Vec3<f64>` and `Vec3`): what to write so both have `want`'s.
pub(crate) fn same_elem_help(
    ctx: &LowerCtx<'_>,
    want: &HirTy,
    src: Option<&str>,
) -> Option<String> {
    let HirTy::Struct(id) = want else {
        return None;
    };
    let d = ctx.struct_def(*id);
    let leaf = leaf_name(expr::enum_base_name(&d.name)).to_owned();
    let args: Vec<String> = d.generic_args.iter().map(|a| ctx.ty_name(a)).collect();
    let full = format!("{leaf}<{}>", args.join(", "));
    let s = src?;
    // `Vec3::X` / `Vec3::<f32>::X`: the same item of `want`'s instance.
    if let Some(rest) = s.strip_prefix(&format!("{leaf}::")) {
        let item = match rest.strip_prefix('<') {
            Some(r) => r.split_once(">::")?.1,
            None => rest,
        };
        return Some(format!(
            "both sides need one element type; write `{leaf}::<{}>::{item}`",
            args.join(", ")
        ));
    }
    let local = s.chars().all(|c| c.is_alphanumeric() || c == '_');
    Some(if local {
        format!("both sides need one element type; make `{s}` a `{full}`")
    } else {
        format!("both sides need one element type; build this value as a `{full}`")
    })
}

/// Help for a primitive in another primitive's slot: `as` between
/// numbers, `parse` from a `str`, and for a literal under `let x: T`
/// (`ann` the `: T`), dropping the annotation.
pub(crate) fn convert_help(
    ctx: &LowerCtx<'_>,
    slot: &HirTy,
    v: &HirExpr,
    ann: Option<Span>,
    what: &str,
) {
    let half = |t: &HirTy| crate::lower::half::half_of(ctx, t).is_some();
    let prim_num = |t: &HirTy| matches!(t, HirTy::Prim(p) if p.is_int() || p.is_float());
    if (half(slot) || half(&v.ty))
        && (half(slot) || prim_num(slot))
        && (half(&v.ty) || prim_num(&v.ty))
    {
        let to = ctx.ty_name(slot);
        match suggest::short_src(ctx, v.span) {
            Some(s)
                if s.chars()
                    .all(|c| c.is_alphanumeric() || matches!(c, '_' | '.')) =>
            {
                let fix = format!("{s} as {to}");
                ctx.help_last(Some(format!("convert it with `as`: `{fix}`")));
                ctx.fix_last(v.span, fix, format!("convert with `as {to}`"));
            }
            _ => ctx.help_last(Some(format!("convert it with `as {to}`"))),
        }
        return;
    }
    let (HirTy::Prim(to), HirTy::Prim(from)) = (slot, &v.ty) else {
        return;
    };
    let num = |p: &PrimTy| p.is_int() || p.is_float();
    let src = suggest::short_src(ctx, v.span);
    // Needs no parentheses before `as` or `.parse()`.
    let simple = |s: &str| {
        s.chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '\''))
            || s.len() > 1 && s.starts_with('"') && s.ends_with('"')
            || s.ends_with(')') && !s.contains(' ')
    };
    let wrap = |s: &str| {
        if simple(s) {
            s.to_owned()
        } else {
            format!("({s})")
        }
    };
    let to_name = to.name();
    let mut helped = true;
    match src {
        _ if num(to) && num(from) && expr::lit_decl_help(ctx, v, *to) => {}
        Some(s) if num(to) && num(from) => {
            let fix = format!("{} as {to_name}", wrap(s));
            ctx.help_last(Some(format!("convert it with `as`: `{fix}`")));
            ctx.fix_last(v.span, fix, format!("convert with `as {to_name}`"));
        }
        None if num(to) && num(from) => {
            ctx.help_last(Some(format!("convert it with `as {to_name}`")));
        }
        Some(s) if num(to) && *from == PrimTy::Str => {
            let fix = format!("try! {}.parse()", wrap(s));
            ctx.help_last(Some(format!(
                "to read a number from the text, write `{fix}`"
            )));
            ctx.fix_last(v.span, fix, "parse the text");
        }
        // `+` on text has its own f-string help.
        Some(s)
            if *to == PrimTy::Str
                && *from != PrimTy::CChar
                && !s.contains('"')
                && !what.contains("operand of `+`")
                && !what.starts_with("the pattern") =>
        {
            let fix = format!("f\"{{{s}}}\"");
            ctx.help_last(Some(format!("to make text from it, write `{fix}`")));
            ctx.fix_last(v.span, fix, "format as text");
        }
        _ => helped = false,
    }
    let literal = src.is_some_and(|s| {
        s.trim_start_matches('-')
            .starts_with(|c: char| c.is_ascii_digit() || matches!(c, '"' | '\''))
            || matches!(s, "true" | "false")
    });
    let (Some(ann), true) = (ann, literal) else {
        return;
    };
    let Some(text) = ctx.src(ann) else {
        return;
    };
    let or = if helped { "or, " } else { "" };
    ctx.help_last(Some(format!(
        "{or}to keep the `{}`, drop `{text}`",
        from.name()
    )));
    ctx.fix_last(ann, "", format!("remove `{text}`"));
}

/// What to write instead of a `[ref name]` capture (`x` its variable's
/// type) in a closure that may run on another thread.
pub(crate) fn ref_capture_help(name: &str, x: &HirTy) -> String {
    let share = if matches!(x, HirTy::Prim(p) if p.is_int() || *p == PrimTy::Bool) {
        format!(
            "`let {name} = Atomic::new({name});` before the closure, capture `{name}` without \
             `ref`, and use `{name}.load()` / `{name}.store(..)`"
        )
    } else {
        format!(
            "`let {name} = Mutex::new({name});` before the closure, capture `{name}` without \
             `ref`, and use `using g = {name}.lock() {{ g.set(..); }}`"
        )
    };
    format!("help: write {share}; or return the value from the closure instead")
}

/// A closure with a `[ref]` capture, or one capturing such a closure.
pub(crate) fn holds_ref_cell(ty: &HirTy) -> bool {
    match ty {
        HirTy::Closure { env, shares, .. } => *shares || holds_ref_cell(env),
        HirTy::Tuple(ts) => ts.iter().any(holds_ref_cell),
        _ => false,
    }
}

/// Why a function value does not fit a `fn` slot (`""`: no hint).
pub(crate) fn fn_mismatch_hint(slot: &HirTy, ty: &HirTy) -> &'static str {
    if matches!(slot, HirTy::Fn(_, _, want) if !want.auto.is_empty()) && holds_ref_cell(ty) {
        " - the closure holds a `[ref ..]` capture, which the code that made it could change at \
         the same time; share the value through a `Mutex` or an `Atomic` instead"
    } else if matches!((slot, ty), (HirTy::Fn(_, _, want), HirTy::Closure { .. }) if *want != crate::FnBounds::default())
    {
        " - the closure's captures don't meet the `fn` type's bounds"
    } else if matches!((slot, ty), (HirTy::Fn(_, _, want), HirTy::Fn(_, _, have))
        if have.unsafe_fn && !want.unsafe_fn)
    {
        " - an `unsafe fn` value only fits an `unsafe fn` type"
    } else if matches!((slot, ty), (HirTy::Fn(_, _, want), HirTy::Fn(_, _, have))
        if want.c_abi != have.c_abi)
    {
        " - an `extern \"C\" fn` value is a bare C function pointer, not a Nabi `fn` value"
    } else if matches!((slot, ty), (HirTy::Fn(_, _, want), HirTy::Fn(_, _, have))
        if want.stdcall != have.stdcall)
    {
        " - `extern \"system\" fn` is `stdcall` on this target, not `extern \"C\" fn`"
    } else if matches!((slot, ty), (HirTy::Fn(_, _, want), HirTy::Fn(_, _, have))
        if !fn_bounds_fit(want, have))
    {
        " - a `fn` value must already carry the bound in its type; only a named \
         function or a capture-free closure is always `Send + Sync`"
    } else if matches!((slot, ty), (HirTy::Fn(_, r, _), HirTy::Fn(..))
        if matches!(**r, HirTy::Throws { .. }))
    {
        " - a function value fits a `fn` type that throws more only with the same \
         parameters and result"
    } else {
        ""
    }
}

/// A named C-ABI function fills an `extern "C" fn(..)` slot
/// (or sum member) as its own address; MIR takes the C entry. Where
/// `"system"` is `stdcall`, it and `"C"` only fill their own kind.
pub(crate) fn c_fn_value(ctx: &LowerCtx<'_>, slot: &HirTy, mut value: HirExpr) -> HirExpr {
    let (HirExprKind::FnRef(id), HirTy::Fn(va, vr, have)) = (&value.kind, &value.ty) else {
        return value;
    };
    if have.c_abi {
        return value;
    }
    let fits = |t: &&HirTy| matches!(t, HirTy::Fn(a, r, want) if want.c_abi && a == va && r == vr);
    let target = match slot {
        HirTy::Sum(vs) => vs.iter().find(fits),
        s => Some(s).filter(fits),
    };
    let Some(HirTy::Fn(_, _, want)) = target else {
        return value;
    };
    if !(ctx.env.is(FnSet::ExternC, *id) || ctx.env.is(FnSet::CDef, *id)) {
        let name = ctx
            .env
            .decl(*id)
            .map_or("f", |d| crate::lower::resolve::leaf_name(&d.def().name));
        ctx.emit_error_help(
            Code::FFI,
            value.span,
            "only an `extern \"C\"` function (an `extern` block declaration or an \
             `extern \"C\" fn` definition) is an `extern \"C\" fn(..)` value"
                .to_owned(),
            format!("C calls it with the C ABI: declare it `extern \"C\" fn {name}(..)`"),
        );
    }
    let stdcall = ctx.env.is(FnSet::Stdcall, *id);
    if stdcall != want.stdcall {
        let (is, slot) = if stdcall {
            ("an `extern \"system\"`", "`extern \"C\" fn(..)`")
        } else {
            ("a C", "`extern \"system\" fn(..)`")
        };
        ctx.emit_error(
            Code::FFI,
            format!(
                "{is} function is no {slot} value: `\"system\"` is `stdcall` on this \
                 target"
            ),
        );
    }
    let mut bounds = have.clone();
    bounds.c_abi = true;
    bounds.stdcall = want.stdcall;
    value.ty = HirTy::Fn(va.clone(), vr.clone(), bounds);
    value
}

/// Can a `fn` with bounds `have` fill `want`? It needs every auto trait
/// the slot names; `unsafe fn` fills only `unsafe fn` slots.
pub(crate) fn fn_bounds_fit(want: &crate::FnBounds, have: &crate::FnBounds) -> bool {
    want.auto.iter().all(|b| have.auto.contains(b))
        && (!have.unsafe_fn || want.unsafe_fn)
        && want.c_abi == have.c_abi
        && want.stdcall == have.stdcall
}

/// A `fn` with extra auto-trait bounds takes the slot's type, so
/// `fn() + Send` -> `fn()` (or `fn() | none`) needs no conversion.
pub(crate) fn drop_fn_bounds(slot: &HirTy, mut value: HirExpr) -> HirExpr {
    let HirTy::Fn(va, vr, have) = &value.ty else {
        return value;
    };
    let fits = |t: &&HirTy| {
        matches!(t, HirTy::Fn(a, r, want)
            if a == va && r == vr && fn_bounds_fit(want, have))
    };
    let target = match slot {
        HirTy::Sum(vs) => vs.iter().find(fits),
        s => Some(s).filter(fits),
    };
    if let Some(t) = target {
        value.ty = t.clone();
    }
    value
}

/// Does `any` with `have` satisfy `want`: some bound or transitive
/// supertrait is `want` (`any Tr<i32>` satisfies `any Tr`).
pub(crate) fn bound_covers(ctx: &LowerCtx<'_>, have: &[TraitRef], want: &TraitRef) -> bool {
    let mut stack: Vec<TraitRef> = have.to_vec();
    let mut seen: Vec<String> = Vec::new();
    while let Some(h) = stack.pop() {
        if h == *want || (want.is_plain() && h.path == want.path) {
            return true;
        }
        if let (false, Some(info)) = (seen.contains(&h.path), ctx.env.trait_of(&h.path)) {
            stack.extend(info.supers.iter().map(|s| TraitRef::plain(s)));
        }
        seen.push(h.path);
    }
    false
}

/// Does a value of `ty` (not `any`) or an `any` value meet bound `b`?
pub(crate) fn meets(ctx: &LowerCtx<'_>, ty: &HirTy, b: &TraitRef) -> bool {
    match ty {
        HirTy::AnyTrait(have) => bound_covers(ctx, have, b),
        _ => ctx.implements_trait(ty, b, 0),
    }
}

/// The `any Tr` slot a `ty` value goes into: `slot`, or the `any Tr`
/// member of a sum without an exact member for `ty`.
pub(crate) fn any_slot_for<'s>(slot: &'s HirTy, ty: &HirTy) -> Option<&'s HirTy> {
    match slot {
        HirTy::AnyTrait(_) => Some(slot),
        HirTy::Sum(vs) if !matches!(ty, HirTy::Sum(_)) && sum_variant_index(vs, ty).is_none() => {
            vs.iter().find(|v| matches!(v, HirTy::AnyTrait(_)))
        }
        _ => None,
    }
}

/// Store into `any Tr`: check every bound and box as `SumWrap { tag,
/// value }` (program-wide type number, heap copy). `any (A + B)` -> `any A`
/// keeps the value (same tags).
pub(crate) fn box_into_any(ctx: &LowerCtx<'_>, slot: &HirTy, value: HirExpr) -> HirExpr {
    let Some(target @ HirTy::AnyTrait(bounds)) = any_slot_for(slot, &value.ty) else {
        return value;
    };
    match &value.ty {
        HirTy::AnyTrait(have) if bounds.iter().all(|b| bound_covers(ctx, have, b)) => HirExpr {
            ty: target.clone(),
            ..value
        },
        // No implicit `any` -> `any Tr` (it would hide a
        // runtime check); `any Tr` -> `any` is the arm above.
        HirTy::AnyTrait(_) => ctx.error_expr(
            Code::MISMATCHED_TYPES,
            value.span,
            format!(
                "cannot store an `{}` value in an `{}` slot without narrowing - test it with \
                 `is` or a type pattern (`match v {{ x: T => ... }}`) first",
                ctx.ty_name(&value.ty),
                ctx.ty_name(target)
            ),
        ),
        HirTy::Unknown | HirTy::Any => value,
        ty => {
            let names: Vec<String> = bounds
                .iter()
                .filter(|b| !ctx.implements_trait(ty, b, 0))
                .map(|b| format!("`{}`", ctx.trait_name(b)))
                .collect();
            if !names.is_empty() {
                return ctx.error_expr(
                    Code::UNSATISFIED_BOUND,
                    value.span,
                    format!(
                        "`{}` does not implement {}, so it cannot be stored in an `{}` slot",
                        ctx.ty_name(ty),
                        names.join(", "),
                        ctx.ty_name(target)
                    ),
                );
            }
            let span = value.span;
            HirExpr {
                kind: HirExprKind::SumWrap {
                    sum_ty: target.clone(),
                    variant_index: ctx.any_tag(ty),
                    value: Some(Box::new(value)),
                },
                ty: target.clone(),
                span,
            }
        }
    }
}

/// A default value (`x: T = e`, `f: T = e`) resolved where it is
/// declared, not at the use that splices it in.
pub(crate) fn in_module(module: &str, e: Expr) -> Expr {
    Expr {
        span: e.span,
        kind: ExprKind::InModule(module.to_owned(), Box::new(e)),
    }
}

/// `ty_compatible`, or storable through `box_into_any`.
pub(crate) fn coercible(ctx: &LowerCtx<'_>, slot: &HirTy, ty: &HirTy) -> bool {
    ty_compatible(slot, ty)
        || matches!(any_slot_for(slot, ty), Some(HirTy::AnyTrait(bs))
        if bs.iter().all(|b| meets(ctx, ty, b)))
}

/// The one type of branch values `tys` (`if` branches, `match` arms,
/// `break` values, return paths): the first that every other one is, or
/// is a member of. A sum counts by its members, so `none` and `T | none`
/// give `T | none`; no new sum is made. `None` if they don't meet.
pub(crate) fn branch_join(ctx: &LowerCtx<'_>, tys: &[HirTy]) -> Option<HirTy> {
    let open = |t: &HirTy| crate::lower::infer::has_var(ctx, t);
    // An open literal goes in as the sum's one number of its kind.
    let lit = |t: &HirTy| crate::lower::infer::literal_default(ctx, t).is_some();
    if tys
        .iter()
        .any(|t| *t == HirTy::Unknown || (open(t) && !lit(t)))
    {
        return None;
    }
    let within = |c: &HirTy, t: &HirTy| match (c, t) {
        _ if c == t => true,
        (HirTy::Sum(ms), t) if lit(t) => crate::lower::infer::literal_member(ctx, ms, t),
        (HirTy::Sum(ms), HirTy::Sum(ts)) => ts.iter().all(|m| sum_variant_index(ms, m).is_some()),
        (HirTy::Sum(ms), t) => sum_variant_index(ms, t).is_some(),
        _ => false,
    };
    tys.iter()
        .filter(|c| !open(c))
        .find(|c| tys.iter().all(|t| within(c, t)))
        .cloned()
}

/// A tuple or list literal into a sum takes the one member of its shape as
/// its expected type (`let t: (i32, Id) | none = (13, Id(1))`), so its
/// literals take that member's types.
pub(crate) fn sum_member_for(ctx: &LowerCtx<'_>, e: &Expr, ty: &HirTy) -> Option<HirTy> {
    let HirTy::Sum(ms) = ty else {
        return None;
    };
    let fits = |m: &HirTy| match (&e.kind, m) {
        (ExprKind::Tuple(es), HirTy::Tuple(ts)) => es.len() == ts.len() && !es.is_empty(),
        (ExprKind::List(_), HirTy::Struct(id)) => {
            ctx.struct_def(*id).name.split("__").next() == Some(expr::LIST)
        }
        _ => false,
    };
    let mut hits = ms.iter().filter(|m| fits(m));
    match (hits.next(), hits.next()) {
        (Some(one), None) => return Some(one.clone()),
        (Some(_), Some(_)) => return None,
        _ => {}
    }
    // No `List` member: the one `Array` (`Vector<f64> | none`) or fixed
    // array of the literal's length a list literal builds.
    let ExprKind::List(items) = &e.kind else {
        return None;
    };
    let nd = ctx.lang("ndarray_type");
    let builds = |m: &HirTy| match m {
        HirTy::Struct(id) => !nd.is_empty() && enum_base_name(&ctx.struct_def(*id).name) == nd,
        HirTy::Array(_, n) => usize::try_from(*n).is_ok_and(|n| n == items.len()),
        _ => false,
    };
    let mut hits = ms.iter().filter(|m| builds(m));
    match (hits.next(), hits.next()) {
        (Some(one), None) => Some(one.clone()),
        _ => None,
    }
}

/// Lower `e` with `ty` as its expected type (`Unknown` = no expectation).
pub(crate) fn lower_expected(ctx: &mut LowerCtx<'_>, e: &Expr, ty: HirTy) -> HirExpr {
    ctx.push_expected(ty);
    let r = lower_expr(ctx, e);
    ctx.pop_expected();
    r
}

/// Never completes normally: ends in `return` / `throw` / `break` /
/// `continue`, a `loop` without `break`, or all-diverging branches.
pub(crate) fn hir_block_diverges(b: &HirBlock) -> bool {
    b.stmts.iter().any(|s| match s {
        HirStmt::Return(_)
        | HirStmt::Panic(_)
        | HirStmt::Break { .. }
        | HirStmt::Continue { .. } => true,
        HirStmt::Expr(e) | HirStmt::Let { init: e, .. } => hir_expr_diverges(e),
        _ => false,
    }) || b.trailing.as_ref().is_some_and(hir_expr_diverges)
}

pub(crate) fn hir_expr_diverges(e: &HirExpr) -> bool {
    match &e.kind {
        // `'l: { break 'l v; }` exits with a value: not divergence.
        HirExprKind::Block(b) => {
            hir_block_diverges(b)
                && !(b.label.is_some() && block_breaks_out(b, b.label.as_deref(), false))
        }
        HirExprKind::If {
            then_block,
            else_branch: Some(eb),
            ..
        } => hir_block_diverges(then_block) && hir_expr_diverges(eb),
        HirExprKind::Match { arms, .. } => {
            !arms.is_empty() && arms.iter().all(|a| hir_expr_diverges(&a.body))
        }
        HirExprKind::Loop { label, body, .. } => !block_breaks_out(body, label.as_deref(), true),
        _ => false,
    }
}

/// Does `b` contain a `break` that exits the loop labelled `label`?
/// `direct`: not inside a nested loop, so unlabelled breaks count.
pub(crate) fn block_breaks_out(b: &HirBlock, label: Option<&str>, direct: bool) -> bool {
    let in_expr = |e: &HirExpr| expr_breaks_out(e, label, direct);
    b.stmts.iter().any(|s| match s {
        HirStmt::Break { label: bl, .. } => {
            (bl.is_none() && direct) || (bl.is_some() && bl.as_deref() == label)
        }
        HirStmt::Let { init: e, .. }
        | HirStmt::Assign { value: e, .. }
        | HirStmt::StaticAssign { value: e, .. }
        | HirStmt::Expr(e)
        | HirStmt::Return(Some(e))
        | HirStmt::Panic(e) => in_expr(e),
        _ => false,
    }) || b.trailing.as_ref().is_some_and(in_expr)
}

pub(crate) fn expr_breaks_out(e: &HirExpr, label: Option<&str>, direct: bool) -> bool {
    match &e.kind {
        HirExprKind::Block(b) => block_breaks_out(b, label, direct),
        HirExprKind::If {
            then_block,
            else_branch,
            ..
        } => {
            block_breaks_out(then_block, label, direct)
                || else_branch
                    .as_deref()
                    .is_some_and(|x| expr_breaks_out(x, label, direct))
        }
        HirExprKind::Match { arms, .. } => {
            arms.iter().any(|a| expr_breaks_out(&a.body, label, direct))
        }
        HirExprKind::While { body, .. } | HirExprKind::Loop { body, .. } => {
            block_breaks_out(body, label, false)
        }
        _ => false,
    }
}
