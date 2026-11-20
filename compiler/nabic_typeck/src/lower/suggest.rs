//! Help for an unknown method: the names a type answers to, and the
//! names other languages use for them.

#[allow(clippy::wildcard_imports)]
use crate::lower::*;
use nabic_diagnostic::Code;

/// `(nabi method, names other languages use for it)`.
pub(crate) const METHOD_SYNONYMS: &[(&str, &[&str])] = &[
    ("len", &["length", "size", "count", "lenght"]),
    ("push", &["append", "add", "push_back"]),
    ("to_uppercase", &["upper", "toUpperCase", "toupper"]),
    ("to_lowercase", &["lower", "toLowerCase", "tolower"]),
    ("trim", &["strip"]),
    ("starts_with", &["startswith", "startsWith"]),
    ("ends_with", &["endswith", "endsWith"]),
    ("contains", &["includes", "has"]),
    ("iter", &["items", "entries"]),
    ("remove", &["delete", "del"]),
    ("join", &["implode"]),
];

/// Iterator methods a collection reaches through `.iter()`.
const ITER_METHODS: &[&str] = &[
    "map",
    "filter",
    "filter_map",
    "flat_map",
    "fold",
    "any",
    "all",
    "sum",
    "product",
    "for_each",
    "enumerate",
    "zip",
    "find",
    "position",
    "skip",
    "take",
    "rev",
    "chain",
    "collect",
];

/// Names of the methods (taking `self`) a user calls on `ty`, sorted:
/// visible, and neither they nor their trait's declaration marked
/// `#[doc(hidden)]` or `#[internal]` (left out of completion and docs).
pub(crate) fn method_names(ctx: &LowerCtx<'_>, ty: &HirTy) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let drop_trait = ctx.lang("drop_method");
    let user = |name: &str, info: Option<&MethodInfo>, def: Option<&FnDef>| {
        let tr = info.and_then(|i| i.trait_name.as_deref());
        let hidden_decl = tr
            .and_then(|t| ctx.env.trait_of(t)?.decls.get(name))
            .is_some_and(|d| nabic_ast::doc_hidden(&d.attrs));
        // A method of a trait the caller cannot name, or `drop` (only the
        // compiler calls it), is not one to suggest.
        let callable = tr.is_none_or(|t| {
            t != drop_trait
                && ctx
                    .env
                    .names
                    .type_vis(t)
                    .is_none_or(|v| ctx.is_visible(&module_of(t), v))
        });
        info.is_none_or(|i| ctx.is_visible(&i.module, i.vis))
            && callable
            && !hidden_decl
            && def.is_none_or(|d| {
                !nabic_ast::doc_hidden(&d.attrs)
                    && d.params.first().is_some_and(|p| p.name == "self")
            })
    };
    if let Some(target) = resolve::method_target_for(ty) {
        for ((t, name), ids) in ctx.env.all_methods() {
            if *t == target
                && ids.iter().any(|id| {
                    let def = match ctx.env.decl(*id) {
                        Some(MethodOrFn::Fn(d)) => Some(*d),
                        Some(MethodOrFn::Method { fn_def, .. }) => Some(&**fn_def),
                        None => None,
                    };
                    user(name, ctx.env.method_info(*id), def)
                        && ctx.sig_by_id(*id).param_names.first().map(String::as_str)
                            == Some("self")
                })
            {
                out.push(name.clone());
            }
        }
    }
    if let Some(base) = generic_base(ctx, ty) {
        for ((b, name), gms) in ctx.env.all_generic_methods() {
            if *b == base
                && gms
                    .iter()
                    .any(|gm| user(name, Some(&gm.info), Some(&gm.def)))
            {
                out.push(name.clone());
            }
        }
    }
    let all = |_: &Impl| true;
    for (gi, _) in ctx.generic_impls(ty, Pick::Where(&all), 0) {
        for (name, gm) in &gi.methods {
            if user(name, Some(&gm.info), Some(&gm.def)) {
                out.push(name.clone());
            }
        }
    }
    out.retain(|n| !n.is_empty() && !n.starts_with("__") && !n.contains(['#', '/']));
    out.sort();
    out.dedup();
    out
}

/// The name generic impls of `ty` are filed under (`List__i32` gives
/// `List`).
fn generic_base(ctx: &LowerCtx<'_>, ty: &HirTy) -> Option<String> {
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
        HirTy::GcBuf(_) => (resolve::GC_BUF.to_owned(), Vec::new()),
        _ => return None,
    };
    Some(if generic_args.is_empty() {
        name
    } else {
        expr::enum_base_name(&name).to_owned()
    })
}

/// A type of the standard library or a primitive: never suggest an impl
/// on it.
fn is_std(ctx: &LowerCtx<'_>, ty: &HirTy) -> bool {
    match ty {
        HirTy::Struct(id) => nabic_core::in_toolchain(&ctx.struct_def(*id).name),
        HirTy::Enum(id) => nabic_core::in_toolchain(&ctx.enum_def(*id).name),
        HirTy::Prim(_) | HirTy::GcBuf(_) | HirTy::Array(..) => true,
        _ => false,
    }
}

/// The span of the first `name` after `recv` within the call `span`.
fn method_span(ctx: &LowerCtx<'_>, recv: Span, span: Span, name: &str) -> Option<Span> {
    let text = ctx.src(span)?;
    let from = recv.end.checked_sub(span.start)? as usize;
    let at = from + text.get(from..)?.find(name)?;
    ctx.sub_span(span, at, at + name.len())
}

/// `r.f(x)` where `f` is a field holding a function or a callable value:
/// a call through a field needs parentheses, `(r.f)(x)`, as a method call
/// never looks at fields.
fn callable_field_help(
    ctx: &LowerCtx<'_>,
    recv: &HirExpr,
    method: &str,
    recv_src: Option<&str>,
    span: Span,
) -> Option<String> {
    let HirTy::Struct(id) = &recv.ty else {
        return None;
    };
    let field = ctx
        .struct_def(*id)
        .fields
        .into_iter()
        .find(|f| f.name == method)?;
    let call = ctx.lang("call_trait");
    let callable = matches!(field.ty, HirTy::Fn(..) | HirTy::Closure { .. })
        || !call.is_empty() && ctx.has_trait(&field.ty, call);
    if !callable {
        return None;
    }
    let r = recv_src.unwrap_or("x");
    if let (Some(src), Some(at)) = (recv_src, method_span(ctx, recv.span, span, method)) {
        let whole = Span {
            start: recv.span.start,
            end: at.end,
            ..recv.span
        };
        ctx.fix_last(whole, format!("({src}.{method})"), "call the field");
    }
    Some(format!(
        "`{method}` is a field of `{}` of type `{}`, which is callable: call it as \
         `({r}.{method})(..)`",
        ctx.ty_name(&recv.ty),
        ctx.ty_name(&field.ty)
    ))
}

/// Help for `recv.method(..)`, which `recv`'s type lacks; a fix where
/// the help is a concrete replacement. Call right after the error.
pub(crate) fn no_method_help(
    ctx: &LowerCtx<'_>,
    recv: &HirExpr,
    method: &str,
    span: Span,
) -> String {
    let ty = &recv.ty;
    let shown = ctx.ty_name(ty);
    let names = method_names(ctx, ty);
    let has = |m: &str| names.iter().any(|n| n == m);
    let recv_src = ctx
        .src(recv.span)
        .filter(|_| recv.span.file == span.file && recv.span.end <= span.end);
    if let Some(h) = read_only_view_help(ctx, ty, method, recv_src) {
        return h;
    }
    if let Some(h) = callable_field_help(ctx, recv, method, recv_src, span) {
        return h;
    }
    // `xs.map(..)` on a List: an iterator method.
    if ITER_METHODS.contains(&method) && has("iter") {
        let r = recv_src.unwrap_or("xs");
        if let Some(src) = recv_src {
            ctx.fix_last(recv.span, format!("{src}.iter()"), "iterate first");
        }
        return format!("`{shown}` is not an iterator: write `{r}.iter().{method}(..)`");
    }
    let r = recv_src.unwrap_or("x");
    // A frozen collection never changes: what to change is a copy.
    if let Some(base) = ["FrozenList", "FrozenDict", "FrozenSet"]
        .into_iter()
        .find(|b| shown.starts_with(&format!("{b}<")))
    {
        if !has(method) {
            let to = match base {
                "FrozenList" => "to_list",
                "FrozenDict" => "to_dict",
                _ => "to_set",
            };
            return format!(
                "a `{base}` never changes, so other threads can read it without a lock; to \
                 change the elements, make a copy that can: `let ys = {r}.{to}();`"
            );
        }
    }
    // `xs.set(i, v)`: the write is the index operator.
    if method == "set" && ["List<", "Deque<"].iter().any(|b| shown.starts_with(b)) {
        return format!(
            "write `{r}[i] = v`, which panics past the end; check `i < {r}.len()` first to \
             skip it"
        );
    }
    if let Some((plain, help)) = twin_help(ctx, ty, method, &format!("{r}.")) {
        if let Some(at) = method_span(ctx, recv.span, span, method) {
            ctx.fix_last(at, plain, format!("use `{plain}`"));
        }
        return help;
    }
    // Python's `Counter(xs).most_common(n)`: the counts are a `Dict`.
    if method == "most_common" && !shown.starts_with("Dict<") && (has("iter") || has("counts")) {
        return format!(
            "`most_common` is a method of the counts: write `{r}.counts().most_common(..)`"
        );
    }
    // `n.to_f64()`: a number converts with `as` (only floats have `to_f64`).
    if let (HirTy::Prim(p), Some(to)) = (ty, method.strip_prefix("to_")) {
        let num = |t: &PrimTy| t.is_int() || t.is_float();
        if num(p) && crate::ty::prim_named(to).is_some_and(|t| num(&t)) {
            return if p.is_int() && to.starts_with(['i', 'u']) {
                format!("convert with `{r} as {to}` (wraps), or `try {r}.try_into::<{to}>()`")
            } else {
                format!("a number converts with `as`: `{r} as {to}`")
            };
        }
    }
    let pick = nabic_diagnostic::suggest_or_synonym(
        method,
        names.iter().map(String::as_str),
        METHOD_SYNONYMS,
    );
    if let Some(s) = pick {
        if let Some(at) = method_span(ctx, recv.span, span, method) {
            ctx.fix_last(at, s, format!("use `{s}`"));
        }
        return format!("did you mean `{s}`?");
    }
    if is_std(ctx, ty) {
        let mut close: Vec<&str> = names.iter().map(String::as_str).collect();
        close.sort_by_key(|n| nabic_diagnostic::edit_distance(method, n));
        close.truncate(6);
        if close.is_empty() {
            return format!("`{shown}` has no methods; pass it to a function instead");
        }
        let list: Vec<String> = close.iter().map(|n| format!("`{n}`")).collect();
        return format!(
            "`{shown}` is a standard type; call one of its methods, such as {}",
            list.join(", ")
        );
    }
    format!(
        "add `fn {method}(self, ..)` in an `impl {shown} {{ .. }}` block, or import a trait \
         that provides it"
    )
}

/// Help for `try_x` / `x_or_none`, which `ty` lacks while it has `x`: a
/// function has one failure channel and `try?` turns a throw into `none`,
/// so the plain name is the one to call. `owner` is what goes before the
/// name (`s.`, `Path::`). `None` when `x` is missing, or for `try_x` when
/// `x` does not throw (another meaning).
pub(crate) fn twin_help<'n>(
    ctx: &LowerCtx<'_>,
    ty: &HirTy,
    name: &'n str,
    owner: &str,
) -> Option<(&'n str, String)> {
    let (plain, try_form) = match name.strip_prefix("try_") {
        Some(p) => (p, true),
        None => (name.strip_suffix("_or_none")?, false),
    };
    if plain.is_empty() {
        return None;
    }
    let throws = |t: &nabic_ast::ThrowsKind| !matches!(t, nabic_ast::ThrowsKind::None);
    let throws = match crate::lower::expr::pick_method(ctx, ty, plain, None).ok()?? {
        crate::lower::expr::MethodPick::Concrete(id) => match ctx.env.decl(id)? {
            MethodOrFn::Fn(d) => throws(&d.throws),
            MethodOrFn::Method { fn_def, .. } => throws(&fn_def.throws),
        },
        crate::lower::expr::MethodPick::Generic(gm) => throws(&gm.def.throws),
    };
    let call = format!("{owner}{plain}(..)");
    let help = match (try_form, throws) {
        (true, true) => format!(
            "`{plain}` throws its error itself; there is no `try_` form: write `try {call}`"
        ),
        (true, false) => return None,
        (false, true) => format!("`try?` gives `none` for `{plain}`'s error: write `try? {call}`"),
        (false, false) => format!("`{plain}` gives `none` itself: write `{call}`"),
    };
    Some((plain, help))
}

/// Help for `Type::name(..)` (`display`), which type `full` lacks. Call
/// right after the error.
pub(crate) fn no_assoc_help(ctx: &LowerCtx<'_>, full: &str, name: &str, display: &str, span: Span) {
    if !path_twin_help(ctx, full, name, display, span) {
        ctx.help_last(crate::lower::reflect::inspect_help_key(ctx, full, name));
    }
}

/// `twin_help` for `Type::name(..)`: the help and a fix naming the plain
/// function; false when there is none.
fn path_twin_help(ctx: &LowerCtx<'_>, full: &str, name: &str, display: &str, span: Span) -> bool {
    let Some(ty) = ctx
        .lookup_struct(full)
        .map(HirTy::Struct)
        .or_else(|| ctx.lookup_enum(full).map(HirTy::Enum))
    else {
        return false;
    };
    let owner = display
        .rsplit_once("::")
        .map_or(String::new(), |(o, _)| format!("{o}::"));
    let Some((plain, help)) = twin_help(ctx, &ty, name, &owner) else {
        return false;
    };
    let at = ctx
        .src(span)
        .and_then(|t| t.find(name))
        .and_then(|i| ctx.sub_span(span, i, i + name.len()));
    if let Some(at) = at {
        ctx.fix_last(at, plain, format!("use `{plain}`"));
    }
    ctx.help_last(Some(help));
    true
}

/// Methods a `SliceMut` has and the read-only `Slice` lacks.
const VIEW_WRITES: &[&str] = &[
    "index_set",
    "fill",
    "swap",
    "reverse",
    "sort",
    "sort_by",
    "sort_by_key",
    "copy_from_slice",
];

/// `xs[a:b].sort()` / `s[i] = x` on a `Slice`: name the writable view.
fn read_only_view_help(
    ctx: &LowerCtx<'_>,
    ty: &HirTy,
    method: &str,
    recv_src: Option<&str>,
) -> Option<String> {
    let HirTy::Struct(id) = ty else {
        return None;
    };
    let def = ctx.struct_def(*id);
    if expr::enum_base_name(&def.name) != "core::collections::slice::Slice"
        || !VIEW_WRITES.contains(&method)
    {
        return None;
    }
    let elem = def
        .generic_args
        .first()
        .map_or_else(|| "T".to_owned(), |t| ctx.ty_name(t));
    let src = recv_src
        .filter(|s| s.contains('[') && s.ends_with(']'))
        .unwrap_or("xs[a:b]");
    Some(format!(
        "`{}` is a read-only view; a view that writes the list is a `SliceMut<{elem}>`: \
         `let s: SliceMut<{elem}> = {src};`",
        ctx.ty_name(ty)
    ))
}

/// `name` is another language's word for `target` (`length` for `len`).
pub(crate) fn is_synonym(name: &str, target: &str) -> bool {
    METHOD_SYNONYMS
        .iter()
        .any(|(t, names)| *t == target && names.contains(&name))
}

/// `T` of `T | none` (a sum of the rest when there are more).
pub(crate) fn some_part(ty: &HirTy) -> Option<HirTy> {
    let HirTy::Sum(cases) = ty else {
        return None;
    };
    if !cases.contains(&HirTy::None) {
        return None;
    }
    let rest: Vec<HirTy> = cases
        .iter()
        .filter(|c| **c != HirTy::None)
        .cloned()
        .collect();
    match rest.as_slice() {
        [] => None,
        [t] => Some(t.clone()),
        _ => Some(HirTy::sum(rest)),
    }
}

/// A literal to write after `??` for a value of type `ty`.
pub(crate) const fn default_lit(ty: &HirTy) -> Option<&'static str> {
    let HirTy::Prim(p) = ty else {
        return None;
    };
    Some(match p {
        PrimTy::F32 | PrimTy::F64 => "0.0",
        PrimTy::Bool => "false",
        PrimTy::Char => "'\\0'",
        PrimTy::Str => "\"\"",
        PrimTy::CChar => return None,
        _ => "0",
    })
}

/// Source text of `span` short enough to quote in a help.
pub(crate) fn short_src<'c>(ctx: &'c LowerCtx<'_>, span: Span) -> Option<&'c str> {
    ctx.src(span)
        .filter(|t| !t.is_empty() && t.len() <= 40 && !t.contains('\n'))
}

/// The ways to get the value out of `v: T | none`.
pub(crate) fn optional_forms(ctx: &LowerCtx<'_>, v: &str, some: &HirTy) -> String {
    let t = ctx.ty_name(some);
    let d = default_lit(some).unwrap_or("default");
    let x = if v == "x" { "y" } else { "x" };
    format!(
        "write `{v} ?? {d}`, `if let {x} = {v} {{ .. }}`, or \
         `match {v} {{ {x}: {t} => .., none => .. }}`"
    )
}

/// `v.unwrap()` and kin on `v: T | none`: Nabi has no such methods, so
/// show the forms that get the value out.
pub(crate) fn optional_method(
    ctx: &LowerCtx<'_>,
    recv: &HirExpr,
    method: &str,
    args: &[CallArg],
    span: Span,
) -> Option<HirExpr> {
    const NAMES: &[&str] = &[
        "unwrap",
        "expect",
        "unwrap_or",
        "unwrap_or_default",
        "unwrap_or_else",
        "is_some",
        "is_none",
        "ok_or",
        "get",
        "value",
    ];
    if !NAMES.contains(&method) {
        return None;
    }
    let some = some_part(&recv.ty)?;
    let v = short_src(ctx, recv.span).filter(|_| recv.span.file == span.file);
    let what = v.map_or("this value".to_owned(), |v| format!("`{v}`"));
    let e = ctx.error_expr(
        Code::NO_METHOD,
        span,
        format!(
            "{what} is `{}` and may be none; Nabi has no `{method}`",
            ctx.ty_name(&recv.ty)
        ),
    );
    let v = v.unwrap_or("v");
    let arg = match args {
        [CallArg::Positional(a)] => short_src(ctx, a.span),
        _ => None,
    };
    let help = match (method, arg) {
        ("unwrap_or", Some(d)) => {
            let fix = format!("{v} ?? {d}");
            ctx.fix_last(span, fix.clone(), format!("use `{fix}`"));
            format!("write `{fix}`")
        }
        ("is_none" | "is_some", _) => {
            let fix = if method == "is_none" {
                format!("{v} == none")
            } else {
                format!("{v} != none")
            };
            ctx.fix_last(span, fix.clone(), format!("use `{fix}`"));
            format!("write `{fix}`")
        }
        _ => optional_forms(ctx, v, &some),
    };
    ctx.help_last(Some(help));
    Some(e)
}

/// `src` as the receiver of a method call: parenthesised unless it is a
/// name, call, index or string literal.
fn receiver(src: &str) -> String {
    let plain = src
        .chars()
        .all(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '(' | ')' | '[' | ']'));
    let string = src.len() >= 2 && src.starts_with('"') && src.ends_with('"');
    if plain || string {
        src.to_owned()
    } else {
        format!("({src})")
    }
}

fn has_method(ctx: &LowerCtx<'_>, ty: &HirTy, name: &str) -> bool {
    method_names(ctx, ty).iter().any(|n| n == name)
}

/// Nabi has no truthiness: the test Python's `if x:` means for `c`.
pub(crate) fn truthy_help(ctx: &LowerCtx<'_>, c: &HirExpr) -> Option<String> {
    let x = ctx.src(c.span).unwrap_or("x");
    let test = match &c.ty {
        HirTy::Sum(ms) if ms.contains(&HirTy::None) => format!("{x} != none"),
        HirTy::Prim(p) if p.is_int() => format!("{x} != 0"),
        HirTy::Prim(p) if p.is_float() => format!("{x} != 0.0"),
        ty if has_method(ctx, ty, "is_empty") => format!("!{}.is_empty()", receiver(x)),
        _ => return None,
    };
    Some(format!("Nabi has no truthiness: write `{test}`"))
}

/// Help for operator `sym` on `lhs` (and `rhs`) that Python reads
/// another way: `not xs` for emptiness, `s * 3`.
pub(crate) fn op_habit_help(
    ctx: &LowerCtx<'_>,
    sym: &str,
    lhs: &HirExpr,
    rhs: Option<&HirExpr>,
) -> Option<String> {
    let x = receiver(ctx.src(lhs.span).unwrap_or("x"));
    match rhs {
        None if sym == "!" && has_method(ctx, &lhs.ty, "is_empty") => Some(format!(
            "Nabi has no truthiness: an emptiness test is `{x}.is_empty()`"
        )),
        Some(r)
            if sym == "*"
                && lhs.ty == HirTy::Prim(PrimTy::Str)
                && matches!(crate::lower::infer::zonk(ctx, &r.ty), HirTy::Prim(p) if p.is_int()) =>
        {
            Some(format!(
                "write `{x}.repeat({})`",
                ctx.src(r.span).unwrap_or("n")
            ))
        }
        _ => None,
    }
}

/// `", ".join(xs)`: Nabi joins from the list.
pub(crate) fn join_help(ctx: &LowerCtx<'_>, sep: &HirExpr, args: &[CallArg]) -> String {
    let sep = ctx.src(sep.span).unwrap_or("\", \"");
    let xs = match args {
        [CallArg::Positional(e)] => receiver(ctx.src(e.span).unwrap_or("xs")),
        _ => "xs".to_owned(),
    };
    format!("Nabi joins from the list: `{xs}.join({sep})`")
}

/// The Nabi form of a Python builtin call `name(args)`.
pub(crate) fn python_builtin_help(name: &str, args: &[&str]) -> Option<String> {
    let r = |i: usize| receiver(args[i]);
    let method = |m: &str| format!("write `{}.{m}()`", r(0));
    Some(match (name, args.len()) {
        ("len" | "abs" | "round" | "sorted" | "sum" | "enumerate" | "min" | "max", 1) => {
            method(name)
        }
        ("min" | "max", 2) => format!("write `{}.{name}({})`", r(0), args[1]),
        ("reversed", 1) => method("rev"),
        ("str", 1) => format!("write `{}.to_str()` or `f\"{{{}}}\"`", r(0), args[0]),
        ("repr", 1) => format!("write `f\"{{{}:?}}\"`", args[0]),
        ("list", 1) => method("to_list"),
        ("set", 1) => method("to_set"),
        ("Counter", 1) => method("counts"),
        ("zip", 2) => format!("write `{}.zip({}.iter())`", r(0), r(1)),
        ("any" | "all", 1) => format!("write `{}.{name}(|x| x)`", r(0)),
        ("map" | "filter", 2) => format!("write `{}.{name}({})`", r(1), args[0]),
        ("isinstance", 2) => format!("write `{} is {}`", args[0], args[1]),
        ("input", 0 | 1) => format!(
            "write `try io::input({})`",
            args.first().copied().unwrap_or_default()
        ),
        ("open", 1..) => format!(
            "write `try File::open({})` after `use std::fs::File;`",
            args.join(", ")
        ),
        _ => return None,
    })
}
