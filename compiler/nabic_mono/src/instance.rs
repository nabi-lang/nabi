//! Generic function instantiation: binding type parameters from the
//! call, making and caching instances, open instances under inference.

use crate::lower::collect::collect_unknown_paths;
use crate::lower::mono::{
    OpenInst, cache_key, defers_refs, inferred_ret, infers_ret, lower_fn_inner, lower_fn_with,
    ref_slot_ty, solve_const, typed_const, with_ok,
};
use crate::lower::resolve::{leaf_name, method_target_for, module_of};
use crate::lower::throws::sig_ret_ast;
use crate::lower::{Env, FnSig, LowerCtx, RefOut, fn_module};
use nabic_ast::{FnDef, Ty};
use nabic_core::HashMap;
use nabic_diagnostic::Code;
use nabic_hir::{HirExpr, HirExprKind, HirFunction, HirTy, TraitRef};
use nabic_ir_common::{ConstVal, FunctionId, PrimTy};

/// Match an AST parameter type against an argument's `HirTy` to bind
/// generic parameters (`[T; N]` against `[i32; 4]` binds `T -> i32`).
pub(crate) fn unify_param_ty_with(
    param_ty: &nabic_ast::Ty,
    arg_ty: &HirTy,
    generic_names: &nabic_core::HashSet<String>,
    subst: &mut HashMap<String, HirTy>,
    ctx: &LowerCtx<'_>,
) {
    let mut rec = |p: &nabic_ast::Ty, a: &HirTy| {
        unify_param_ty_with(p, a, generic_names, subst, ctx);
    };
    match (&param_ty.kind, arg_ty) {
        (nabic_ast::TyKind::Path(p), _)
            if p.segments.len() == 1 && generic_names.contains(&p.segments[0].name) =>
        {
            // First binding wins; a conflict surfaces as an argument
            // mismatch. `!` fits any `T`, so a later argument rebinds it.
            let b = subst
                .entry(p.segments[0].name.clone())
                .or_insert_with(|| arg_ty.clone());
            if *b == HirTy::Never {
                *b = arg_ty.clone();
            }
        }
        // Generic struct / enum param `Pair<T>` against arg `Pair__i32`:
        // recover T by reading the instantiation's recorded generic args.
        (nabic_ast::TyKind::Path(p), HirTy::Struct(_) | HirTy::Enum(_)) => {
            let Some(last) = p.segments.last() else {
                return;
            };
            let generic_args = match arg_ty {
                HirTy::Struct(sid) => ctx.struct_def(*sid).generic_args,
                HirTy::Enum(eid) => ctx.enum_def(*eid).generic_args,
                _ => Vec::new(),
            };
            for (param_arg, concrete) in last.generics.iter().zip(generic_args.iter()) {
                rec(param_arg, concrete);
            }
        }
        (nabic_ast::TyKind::Array(elem, len), HirTy::Array(arg_elem, n)) => {
            // `[T; N]` against `[i32; 3]` binds the const parameter `N`.
            let len_name = match len {
                nabic_ast::ConstExpr::Path(p) if p.segments.len() == 1 => {
                    Some(p.segments[0].name.clone())
                }
                nabic_ast::ConstExpr::Expr(e) => match &e.kind {
                    nabic_ast::ExprKind::Path(p) if p.segments.len() == 1 => {
                        Some(p.segments[0].name.clone())
                    }
                    _ => None,
                },
                _ => None,
            };
            if let Some(name) = len_name.filter(|n| generic_names.contains(n)) {
                subst
                    .entry(name)
                    .or_insert_with(|| HirTy::Const(i128::from(*n), None));
            }
            unify_param_ty_with(elem, arg_elem, generic_names, subst, ctx);
        }
        // `Q<{ 2 * A }>` against `Q<2>`: `A = 1`, when one parameter is
        // in it and the value comes out exact.
        (nabic_ast::TyKind::Const(c), HirTy::Const(v, _)) => {
            let e = match c {
                nabic_ast::ConstExpr::Expr(e) => Some(&**e),
                _ => None,
            };
            if let Some((name, x)) = e.and_then(|e| solve_const(e, *v, generic_names)) {
                subst.entry(name).or_insert(HirTy::Const(x, None));
            }
        }
        // `GcBuf<T>` against `GcBuf<i32>`.
        (nabic_ast::TyKind::Path(p), HirTy::GcBuf(e)) => {
            if let Some(g) = p.segments.last().and_then(|s| s.generics.first()) {
                rec(g, e);
            }
        }
        (nabic_ast::TyKind::Tuple(parts), HirTy::Tuple(arg_parts)) => {
            for (p, a) in parts.iter().zip(arg_parts.iter()) {
                rec(p, a);
            }
        }
        // Sums are sets: members named outright cancel and a
        // lone parameter takes the rest (`T | none` against
        // `i32 | str | none` binds `T = i32 | str`).
        (nabic_ast::TyKind::Sum(parts), HirTy::Sum(arg_parts)) => {
            let is_param = |p: &nabic_ast::Ty| {
                matches!(&p.kind, nabic_ast::TyKind::Path(path)
                    if path.segments.len() == 1 && generic_names.contains(&path.segments[0].name))
            };
            // `T?` is `Nest(T) | none`.
            let parts: Vec<&nabic_ast::Ty> =
                parts.iter().map(|p| p.nest_inner().unwrap_or(p)).collect();
            let (params, fixed): (Vec<&nabic_ast::Ty>, Vec<&nabic_ast::Ty>) =
                parts.iter().partition(|p| is_param(p));
            let fixed: Vec<HirTy> = fixed
                .iter()
                .map(|p| match &p.kind {
                    nabic_ast::TyKind::None => HirTy::None,
                    _ => ctx.resolve_ast_ty(p),
                })
                .collect();
            if let [param] = params.as_slice() {
                let rest = arg_parts.iter().filter(|a| !fixed.contains(a)).cloned();
                rec(param, &HirTy::sum(rest));
            } else {
                for (p, a) in parts.iter().zip(arg_parts.iter()) {
                    rec(p, a);
                }
            }
        }
        (
            nabic_ast::TyKind::Fn(args, ret, _),
            HirTy::Fn(arg_args, arg_ret, _)
            | HirTy::Closure {
                args: arg_args,
                ret: arg_ret,
                ..
            },
        ) => {
            for (p, a) in args.iter().zip(arg_args.iter()) {
                rec(p, a);
            }
            rec(ret, arg_ret);
        }
        (nabic_ast::TyKind::Throws { ok, err }, HirTy::Throws { ok: a, err: e }) => {
            rec(ok, a);
            rec(err, e);
        }
        // A non-throwing argument for a `rethrows` parameter: its error
        // parameter binds to "no error".
        (nabic_ast::TyKind::Throws { ok, err }, a) => {
            rec(ok, a);
            rec(err, &HirTy::NoThrow);
        }
        // Nothing to bind: the argument type check reports mismatches.
        _ => {}
    }
}

/// Bind `fn_def`'s type params from the type of argument `i`. A generic
/// method's `self` is typed by its impl target `self_ty`.
pub(crate) fn bind_from_arg(
    ctx: &LowerCtx<'_>,
    fn_def: &FnDef,
    self_ty: Option<&Ty>,
    i: usize,
    arg_ty: &HirTy,
    subst: &mut HashMap<String, HirTy>,
) {
    let Some(param) = fn_def.params.get(i) else {
        return;
    };
    let self_param = self_ty.filter(|_| param.name == "self");
    let Some(t) = param.ty.as_ref().or(self_param) else {
        return;
    };
    let generic_names: nabic_core::HashSet<String> = fn_def
        .generics
        .params
        .iter()
        .map(|gp| match gp {
            nabic_ast::GenericParam::Type { name, .. }
            | nabic_ast::GenericParam::Const { name, .. } => name.clone(),
        })
        .collect();
    unify_param_ty_with(t, arg_ty, &generic_names, subst, ctx);
    bind_assoc_bindings(ctx, fn_def, self_param.map(|_| arg_ty), subst);
}

/// What the expected type of a call of `fn_def` says of its type
/// parameters (none for a loose slot: `any`, `()`).
pub(crate) fn expected_subst(ctx: &LowerCtx<'_>, fn_def: &FnDef) -> HashMap<String, HirTy> {
    let mut out = HashMap::default();
    let Some(expected) = ctx.current_expected().filter(|t| {
        !matches!(
            t,
            HirTy::Unknown | HirTy::Unit | HirTy::Any | HirTy::AnyTrait(_) | HirTy::Never
        ) && !crate::lower::infer::has_var(ctx, t)
    }) else {
        return out;
    };
    if fn_def.generics.params.is_empty() {
        return out;
    }
    let ret = sig_ret_ast(fn_def);
    let ok = match &ret.kind {
        nabic_ast::TyKind::Throws { ok, .. } => ok.as_ref(),
        _ => &ret,
    };
    let names = type_param_names(fn_def);
    unify_param_ty_with(ok, expected, &names, &mut out, ctx);
    out
}

fn type_param_names(fn_def: &FnDef) -> nabic_core::HashSet<String> {
    fn_def
        .generics
        .params
        .iter()
        .filter_map(|gp| match gp {
            nabic_ast::GenericParam::Type { name, .. } => Some(name.clone()),
            nabic_ast::GenericParam::Const { .. } => None,
        })
        .collect()
}

/// An `Assoc = T` binding in a bound on a bare parameter or `Self`.
struct AssocBinding<'a> {
    subject: String,
    assoc: &'a str,
    target: &'a Ty,
    /// The bound's trait arguments (`U` of `Add<U, Output = V>`).
    trait_args: Vec<&'a Ty>,
    span: nabic_diagnostic::Span,
}

/// Each `Assoc = T` binding in `fn_def`'s bounds and `where` clause on a
/// bare parameter or `Self`.
fn assoc_bindings(fn_def: &FnDef) -> Vec<AssocBinding<'_>> {
    let bare = |t: &Ty| match &t.kind {
        nabic_ast::TyKind::Path(p) => match p.segments.as_slice() {
            [s] if s.generics.is_empty() => Some(s.name.clone()),
            _ => None,
        },
        nabic_ast::TyKind::SelfUpper => Some("Self".to_owned()),
        _ => None,
    };
    let preds = fn_def
        .generics
        .params
        .iter()
        .filter_map(|gp| match gp {
            nabic_ast::GenericParam::Type { name, bounds, .. } => {
                Some((name.clone(), bounds.as_slice()))
            }
            nabic_ast::GenericParam::Const { .. } => None,
        })
        .chain(
            fn_def
                .where_clause
                .iter()
                .filter_map(|w| Some((bare(&w.ty)?, w.bounds.as_slice()))),
        );
    let mut out = Vec::new();
    for (subject, bounds) in preds {
        for b in bounds {
            let nabic_ast::TyKind::Path(bp) = &b.kind else {
                continue;
            };
            let generics = || bp.segments.iter().flat_map(|s| &s.generics);
            let trait_args: Vec<&Ty> = generics()
                .filter(|g| nabic_ast::as_assoc_binding(g).is_none())
                .collect();
            for g in generics() {
                if let Some((assoc, target)) = nabic_ast::as_assoc_binding(g) {
                    out.push(AssocBinding {
                        subject: subject.clone(),
                        assoc,
                        target,
                        trait_args: trait_args.clone(),
                        span: b.span,
                    });
                }
            }
        }
    }
    out
}

/// `subject::assoc` resolved, if `subject` is known and the result is
/// fully concrete. `Self` is the receiver's type `self_hir`.
fn project_assoc(
    ctx: &LowerCtx<'_>,
    fn_def: &FnDef,
    subst: &HashMap<String, HirTy>,
    self_hir: Option<&HirTy>,
    subject: &str,
    assoc: &str,
    span: nabic_diagnostic::Span,
) -> Option<HirTy> {
    let self_hir = self_hir.filter(|_| subject == "Self");
    if self_hir.is_none() && !subst.contains_key(subject) {
        return None;
    }
    let seg = |n: &str| nabic_ast::PathSegment {
        name: n.to_owned(),
        generics: Vec::new(),
    };
    let proj = Ty {
        kind: nabic_ast::TyKind::Path(nabic_ast::Path {
            segments: vec![seg(subject), seg(assoc)],
            span,
            origin: None,
        }),
        span,
    };
    let t = resolve_in_mono(ctx, fn_def, subst, self_hir.cloned(), &proj);
    let mut unknown = Vec::new();
    collect_unknown_paths(&t, &mut unknown);
    (t != HirTy::Unknown && unknown.is_empty()).then_some(t)
}

/// `I: IntoIterator<Item = T>` with `I` bound: `T = I::Item`, so a later
/// closure argument `fn(T) -> U` learns its parameter type. The binding
/// may be structured (`Item = (A, B)`), from a bound or a `where`;
/// `where Self: ..` binds once the receiver's type `self_hir` is known.
fn bind_assoc_bindings(
    ctx: &LowerCtx<'_>,
    fn_def: &FnDef,
    self_hir: Option<&HirTy>,
    subst: &mut HashMap<String, HirTy>,
) {
    let names = type_param_names(fn_def);
    let unbound = |t: &Ty, subst: &HashMap<String, HirTy>| {
        names
            .iter()
            .any(|n| !subst.contains_key(n) && crate::lower::expr::ast_ty_mentions(t, n))
    };
    for b in assoc_bindings(fn_def) {
        // The impl, and so the associated type, is picked by the trait's
        // arguments too: read it once they are known.
        if !unbound(b.target, subst) || b.trait_args.iter().any(|a| unbound(a, subst)) {
            continue;
        }
        if let Some(t) = project_assoc(ctx, fn_def, subst, self_hir, &b.subject, b.assoc, b.span) {
            unify_param_ty_with(b.target, &t, &names, subst, ctx);
        }
    }
}

/// Does `ty` have another shape than `want` (a tuple of another arity, or
/// no tuple / fn at all)? Parameters and open types fit anything.
fn shape_differs(want: &Ty, ty: &HirTy) -> bool {
    match (&want.kind, ty) {
        (nabic_ast::TyKind::Tuple(ws), HirTy::Tuple(ts)) => {
            ws.len() != ts.len() || ws.iter().zip(ts).any(|(w, t)| shape_differs(w, t))
        }
        (
            nabic_ast::TyKind::Fn(wa, _, _),
            HirTy::Fn(ta, _, _) | HirTy::Closure { args: ta, .. },
        ) => wa.len() != ta.len(),
        (nabic_ast::TyKind::Tuple(_) | nabic_ast::TyKind::Fn(..), t) => matches!(
            t,
            HirTy::Unit
                | HirTy::Prim(_)
                | HirTy::None
                | HirTy::Sum(_)
                | HirTy::Tuple(_)
                | HirTy::Struct(_)
                | HirTy::Enum(_)
                | HirTy::Array(..)
                | HirTy::Fn(..)
                | HirTy::Closure { .. }
                | HirTy::GcBuf(_)
        ),
        _ => false,
    }
}

/// A call's `Assoc = (A, B)` binding that the argument's associated type
/// cannot take (`unzip` on items that are not pairs), as an error naming
/// the form to give.
fn assoc_shape_error(
    ctx: &LowerCtx<'_>,
    fn_def: &FnDef,
    self_hir: Option<&HirTy>,
    subst: &HashMap<String, HirTy>,
) -> Option<String> {
    let names = type_param_names(fn_def);
    assoc_bindings(fn_def).into_iter().find_map(|b| {
        let (subject, assoc, target, span) = (b.subject, b.assoc, b.target, b.span);
        // Only a tuple or fn form can mismatch in shape; a bare `T`
        // is left to the bound check.
        let shaped = matches!(
            target.kind,
            nabic_ast::TyKind::Tuple(_) | nabic_ast::TyKind::Fn(..)
        );
        if !shaped
            || !names
                .iter()
                .any(|n| crate::lower::expr::ast_ty_mentions(target, n))
        {
            return None;
        }
        let owner = match subject.as_str() {
            "Self" => self_hir,
            s => subst.get(s),
        };
        // A generic body's call: the owner is still a parameter.
        if owner.is_none_or(|o| crate::lower::bounds::has_param(ctx, o)) {
            return None;
        }
        let t = project_assoc(ctx, fn_def, subst, self_hir, &subject, assoc, span)?;
        if !shape_differs(target, &t) {
            return None;
        }
        let owner = owner.map_or_else(|| subject.clone(), |o| ctx.ty_name(o));
        let leaf = leaf_name(&fn_def.name);
        // An impl's copy of a trait method is `__impl<tag>_<name>`.
        let shown = leaf
            .strip_prefix("__impl")
            .and_then(|r| r.split_once('_'))
            .map_or(leaf, |(_, m)| m);
        let want = nabic_ast::print::print_ty(target);
        let fix = if assoc == "Item" {
            format!(" - turn each item into `{want}` first, e.g. with `.map(|x| ..)`")
        } else {
            format!(" - pass a type whose `{assoc}` is `{want}`")
        };
        Some(format!(
            "`{shown}` needs `{assoc}` of the form `{want}`, but `{owner}` has \
                 `{assoc} = {}`{fix}",
            ctx.ty_name(&t)
        ))
    })
}

/// Resolve `ty` under the substitution of a monomorphization in
/// progress, from `fn_def`'s module. `self_ty` (resolved) fills `Self`.
pub(crate) fn resolve_in_mono(
    ctx: &LowerCtx<'_>,
    fn_def: &FnDef,
    subst: &HashMap<String, HirTy>,
    self_ty: Option<HirTy>,
    ty: &Ty,
) -> HirTy {
    let mut rcx = LowerCtx::new(ctx.env, subst, self_ty, ctx.here());
    rcx.current_module = module_of(&fn_def.name);
    rcx.set_bounds(&fn_def.generics, &fn_def.where_clause);
    rcx.resolve_ast_ty(ty)
}

/// `Set::from(xs)` names `Set` with `S` omitted: an impl parameter that
/// the arguments leave unbound and that stands for a defaulted parameter
/// of `Self`'s type (`impl<T, S> .. for Set<T, S>`, `S = RandomState`)
/// takes that default.
fn omitted_self_defaults(
    ctx: &LowerCtx<'_>,
    fn_def: &FnDef,
    self_ty: &Ty,
    subst: &mut HashMap<String, HirTy>,
) {
    let nabic_ast::TyKind::Path(p) = &self_ty.kind else {
        return;
    };
    let Some(last) = p.segments.last() else {
        return;
    };
    let segs: Vec<&str> = p.segments.iter().map(|s| s.name.as_str()).collect();
    let module = module_of(&fn_def.name);
    let at = ctx.here();
    let Some(full) = ctx
        .env
        .names
        .canon(&segs, p.origin.as_deref().unwrap_or(&module), &[], at)
    else {
        return;
    };
    let (decl_name, decl) = match ctx.env.defs.generic_struct(&full) {
        Some(d) => (&d.name, &d.generics),
        None => match ctx.env.defs.generic_enum(&full) {
            Some(d) => (&d.name, &d.generics),
            None => return,
        },
    };
    let own: nabic_core::HashSet<&str> = fn_def
        .generics
        .params
        .iter()
        .filter_map(|gp| match gp {
            nabic_ast::GenericParam::Type { name, .. } => Some(name.as_str()),
            nabic_ast::GenericParam::Const { .. } => None,
        })
        .collect();
    // The declaration's parameters as this call binds them, for a
    // default naming an earlier one (`S = Hasher<T>`).
    let mut decl_subst: HashMap<String, HirTy> = HashMap::default();
    for (gp, arg) in decl.params.iter().zip(&last.generics) {
        let (name, default) = match gp {
            nabic_ast::GenericParam::Type { name, default, .. } => (name, default.as_ref()),
            nabic_ast::GenericParam::Const { name, .. } => (name, None),
        };
        let bare = match &arg.kind {
            nabic_ast::TyKind::Path(ap) if ap.segments.len() == 1 => {
                Some(ap.segments[0].name.as_str()).filter(|n| own.contains(n))
            }
            _ => None,
        };
        let open = bare.filter(|n| !subst.contains_key(*n));
        let resolve_default = |d: &Ty| {
            let mut rcx = LowerCtx::new(ctx.env, &decl_subst, None, at);
            rcx.current_module = module_of(decl_name);
            rcx.resolve_ast_ty(d)
        };
        if let (Some(n), Some(d)) = (open, default) {
            subst.insert(n.to_owned(), resolve_default(d));
        } else if let (Some(t), Some(d)) = (bare.and_then(|n| subst.get(n)), default) {
            // Filled by literals alone: they take the declared default.
            crate::lower::infer::adopt_default(ctx, t, &resolve_default(d), &|| {
                format!(
                    "the default of `{}`'s `{name}`",
                    ctx.display_path(decl_name)
                )
            });
        }
        if let Some(t) = bare.and_then(|n| subst.get(n)) {
            decl_subst.insert(name.clone(), t.clone());
        }
    }
}

/// Instantiation limits: deeper nesting means the generic recursion
/// never reaches a fixed point (`fn r<T>(x: T) { r((x, x)) }`).
pub(crate) const MONO_DEPTH_LIMIT: u32 = 64;
pub(crate) const MONO_TYPE_SIZE_LIMIT: usize = 256;

pub(crate) fn ty_size(ty: &HirTy) -> usize {
    1 + match ty {
        HirTy::Sum(p) | HirTy::Tuple(p) => p.iter().map(ty_size).sum(),
        HirTy::Array(e, _) => ty_size(e),
        HirTy::Fn(a, r, _) => a.iter().map(ty_size).sum::<usize>() + ty_size(r),
        HirTy::Closure { args, ret, env, .. } => {
            args.iter().map(ty_size).sum::<usize>() + ty_size(ret) + ty_size(env)
        }
        HirTy::Throws { ok, err } => ty_size(ok) + ty_size(err),
        _ => 0,
    }
}

/// Bind the impl parameters of `self_ty` (`Box<T>`) from the known
/// `Self` type `hint` (`Box<i32>` -> `T = i32`).
pub(crate) fn bind_self(
    ctx: &LowerCtx<'_>,
    fn_def: &FnDef,
    self_ty: &Ty,
    hint: &HirTy,
    subst: &mut HashMap<String, HirTy>,
) {
    let names: nabic_core::HashSet<String> = fn_def
        .generics
        .params
        .iter()
        .map(|gp| match gp {
            nabic_ast::GenericParam::Type { name, .. }
            | nabic_ast::GenericParam::Const { name, .. } => name.clone(),
        })
        .collect();
    unify_param_ty_with(self_ty, hint, &names, subst, ctx);
    if let HirTy::Param(p) = hint {
        crate::lower::bounds::bind_trait_args(ctx, fn_def, *p, subst);
    }
}

/// Find or make the instance of a generic function for the call's lowered
/// argument types, `Self` known up front when `self_hint` is set (see
/// `bind_self`); `(fn id, return type)`, or `None` after reporting.
pub(crate) fn monomorphize_on(
    ctx: &LowerCtx<'_>,
    fn_def: &FnDef,
    self_ty: Option<&Ty>,
    self_hint: Option<&HirTy>,
    args: &[HirExpr],
    span: nabic_diagnostic::Span,
) -> Option<(FunctionId, HirTy)> {
    // Unify each parameter's AST type with its argument's type; the
    // argument type check reports conflicts.
    let mut subst: HashMap<String, HirTy> = ctx.explicit_generics.clone();
    if let (Some(t), Some(hint)) = (self_ty, self_hint) {
        bind_self(ctx, fn_def, t, hint, &mut subst);
    }
    // An unsuffixed literal binds a parameter last: the other arguments and
    // the expected type win (`id(5)` into an `i32`, `apply(f_i32, 5)`).
    let default_lit = |a: &HirExpr| {
        matches!(
            a.kind,
            HirExprKind::Const(ConstVal::Int { .. } | ConstVal::Float { .. })
        ) && ctx.env.sink.default_lits.borrow().contains(&a.span)
    };
    for (i, arg) in args.iter().enumerate().filter(|(_, a)| !default_lit(a)) {
        bind_from_arg(ctx, fn_def, self_ty, i, &arg.ty, &mut subst);
    }
    let before: nabic_core::HashSet<String> = subst.keys().cloned().collect();
    for (i, arg) in args.iter().enumerate().filter(|(_, a)| default_lit(a)) {
        bind_from_arg(ctx, fn_def, self_ty, i, &arg.ty, &mut subst);
    }
    let from_literal: Vec<String> = subst
        .keys()
        .filter(|k| !before.contains(*k))
        .cloned()
        .collect();
    // A parameter the arguments leave open (only in the return type, or
    // only met `!`, or a literal) binds from the expected type.
    if let Some(expected) = ctx.current_expected().filter(|t| **t != HirTy::Unknown) {
        let literal_bound: Vec<(String, HirTy)> = from_literal
            .iter()
            .filter_map(|k| Some((k.clone(), subst.remove(k)?)))
            .collect();
        let names: nabic_core::HashSet<String> = fn_def
            .generics
            .params
            .iter()
            .filter_map(|gp| match gp {
                nabic_ast::GenericParam::Type { name, .. }
                | nabic_ast::GenericParam::Const { name, .. }
                    if subst.get(name).is_none_or(|t| *t == HirTy::Never) =>
                {
                    Some(name.clone())
                }
                _ => None,
            })
            .collect();
        let restore = |subst: &mut HashMap<String, HirTy>| {
            for (k, t) in &literal_bound {
                subst.entry(k.clone()).or_insert_with(|| t.clone());
            }
        };
        if !names.is_empty() {
            let ret = sig_ret_ast(fn_def);
            let ok = match &ret.kind {
                nabic_ast::TyKind::Throws { ok, .. } => ok.as_ref(),
                _ => &ret,
            };
            unify_param_ty_with(ok, expected, &names, &mut subst, ctx);
        }
        restore(&mut subst);
    }

    if let Some(HirTy::Param(p)) = self_hint {
        if let Err(msg) = crate::lower::bounds::pick_bound(ctx, fn_def, *p, &mut subst) {
            ctx.emit_error(Code::UNSATISFIED_BOUND, msg);
            return None;
        }
    }

    // Unbound params take their defaults. One pass: a default
    // can't depend on another default.
    for gp in &fn_def.generics.params {
        if let nabic_ast::GenericParam::Type {
            name,
            default: Some(d),
            ..
        } = gp
        {
            let origin = || {
                format!(
                    "the default of `{}`'s `{name}`",
                    ctx.display_path(&fn_def.name)
                )
            };
            // Earlier parameters stay in scope: `C = List<T>`. A bound
            // method's `C = List<Self::Item>` needs the receiver.
            let resolved = resolve_in_mono(ctx, fn_def, &subst, self_hint.cloned(), d);
            if let Some(t) = subst.get(name) {
                crate::lower::infer::adopt_default(ctx, t, &resolved, &origin);
            } else {
                // A number default in a `let` initializer stays open: a
                // later use may still fix it.
                let number = matches!(resolved, HirTy::Prim(p) if p.is_int() || p.is_float());
                let resolved = if number && ctx.infer_vars && crate::lower::infer::active(ctx) {
                    crate::lower::infer::fallback(ctx, resolved, origin())
                } else {
                    resolved
                };
                subst.insert(name.clone(), resolved);
            }
        }
    }

    if let (Some(t), None) = (self_ty, self_hint) {
        omitted_self_defaults(ctx, fn_def, t, &mut subst);
    }

    if let Some(msg) = assoc_shape_error(ctx, fn_def, self_hint, &subst) {
        ctx.emit_error(Code::UNSATISFIED_BOUND, msg);
        return None;
    }

    // Every generic param must be inferable.
    let mut subst_tys: Vec<HirTy> = Vec::new();
    for gp in &fn_def.generics.params {
        match gp {
            nabic_ast::GenericParam::Type { name, .. } => {
                if let Some(t) = subst.get(name) {
                    subst_tys.push(t.clone());
                } else if ctx.infer_vars {
                    let v = crate::lower::infer::fresh(ctx);
                    subst.insert(name.clone(), v.clone());
                    subst_tys.push(v);
                } else {
                    let msg = if fn_def.name == "core::collections::list::list_literal" {
                        "cannot infer the element type of the list literal from its \
                         elements, the expected type or a later use - annotate the \
                         binding: `let xs: List<i32> = [];`"
                            .to_owned()
                    } else {
                        format!(
                            "cannot infer type parameter `{name}` of `{}` from the call \
                             arguments, the expected type or a later use - annotate the \
                             binding or write `::<..>`",
                            ctx.display_path(&fn_def.name)
                        )
                    };
                    ctx.emit_uninferred(msg);
                    ctx.tag_last(nabic_diagnostic::Tag::TypeParamInfer);
                    return None;
                }
            }
            nabic_ast::GenericParam::Const { name, ty } => {
                let Some(v) = subst.get(name).and_then(HirTy::as_const) else {
                    // A written argument that failed to evaluate is
                    // reported already.
                    if subst.get(name) == Some(&HirTy::Unknown) {
                        return None;
                    }
                    ctx.emit_uninferred(format!(
                        "cannot infer const parameter `{name}` of `{}` from the call \
                         arguments or the expected type - pass it explicitly (`::<..>`)",
                        ctx.display_path(&fn_def.name)
                    ));
                    return None;
                };
                let c = typed_const(ctx, name, ty, v)?;
                subst.insert(name.clone(), c.clone());
                subst_tys.push(c);
            }
        }
    }
    if subst_tys.iter().any(|t| matches!(t, HirTy::Unknown)) {
        // An argument already failed to lower (reported).
        return None;
    }
    let open_self = self_hint.is_some_and(|t| crate::lower::infer::has_var(ctx, t));
    if open_self
        || subst_tys
            .iter()
            .any(|t| crate::lower::infer::has_var(ctx, t))
    {
        unify_bounds(ctx, fn_def, self_ty, self_hint, &subst);
        let mut inst = var_instance(ctx, fn_def, self_ty, self_hint, &subst);
        if let Some(r) = open_body(ctx, fn_def, inst.0, self_hint, &subst) {
            let ret = with_ok(inst.1, r);
            if let Some(sig) = ctx.env.sink.mono_sigs.borrow_mut().get_mut(&inst.0) {
                sig.ret_ty = ret.clone();
            }
            inst.1 = ret;
        }
        // Made for the solved types when the body ends.
        if crate::lower::infer::active(ctx) {
            ctx.env.sink.open_insts.borrow_mut().push(OpenInst {
                id: inst.0,
                def: std::sync::Arc::new(fn_def.clone()),
                self_ty: self_ty.cloned(),
                self_hint: self_hint.cloned(),
                subst,
                at: span,
            });
        }
        return Some(inst);
    }
    instance_for(ctx, fn_def, self_ty, self_hint, &subst, subst_tys, span)
}

/// The instance `p` stands for, under the solved types (`None`: still
/// open, or it failed and said why).
pub(crate) fn solve_instance(ctx: &LowerCtx<'_>, p: &OpenInst) -> Option<FunctionId> {
    let subst: HashMap<String, HirTy> = p
        .subst
        .iter()
        .map(|(k, t)| (k.clone(), crate::lower::infer::zonk(ctx, t)))
        .collect();
    let hint = p
        .self_hint
        .as_ref()
        .map(|t| crate::lower::infer::zonk(ctx, t));
    if subst
        .values()
        .chain(&hint)
        .any(|t| crate::lower::infer::has_var(ctx, t))
    {
        return None;
    }
    let subst_tys: Vec<HirTy> = p
        .def
        .generics
        .params
        .iter()
        .filter_map(|gp| match gp {
            nabic_ast::GenericParam::Type { name, .. }
            | nabic_ast::GenericParam::Const { name, .. } => subst.get(name).cloned(),
        })
        .collect();
    let at = ctx.at.replace(p.at);
    let made = instance_for(
        ctx,
        &p.def,
        p.self_ty.as_ref(),
        hint.as_ref(),
        &subst,
        subst_tys,
        p.at,
    );
    ctx.at.set(at);
    made.map(|(id, _)| id)
}

/// The instance of `fn_def` for `subst` (in `subst_tys` order), lowered
/// on first use.
fn instance_for(
    ctx: &LowerCtx<'_>,
    fn_def: &FnDef,
    self_ty: Option<&Ty>,
    self_hint: Option<&HirTy>,
    subst: &HashMap<String, HirTy>,
    subst_tys: Vec<HirTy>,
    span: nabic_diagnostic::Span,
) -> Option<(FunctionId, HirTy)> {
    let fn_module = module_of(&fn_def.name);
    let subst = subst.clone();
    if subst_tys.iter().any(|t| ty_size(t) > MONO_TYPE_SIZE_LIMIT)
        || ctx.env.sink.mono_depth.get() >= MONO_DEPTH_LIMIT
    {
        ctx.emit_error(
            Code::INSTANTIATION_LIMIT,
            format!(
                "instantiating generic function `{}` recursed past the limit \
                 (depth {MONO_DEPTH_LIMIT}) - the instantiation never reaches a \
                 fixed point",
                fn_def.name
            ),
        );
        return None;
    }

    // A known `Self` (the receiver's type) is used as is: the template's
    // own module may not see the impl's type (a trait default method).
    let self_hir = self_hint
        .cloned()
        .or_else(|| self_ty.map(|t| resolve_in_mono(ctx, fn_def, &subst, None, t)));
    // A body over a type its bounds reject only repeats the error.
    if !check_call_bounds(ctx, fn_def, &subst, self_hir.as_ref(), &fn_module) {
        return None;
    }

    // A generic body being checked (`bounds`): the callee's signature
    // only, unless its return type comes from its body.
    if !infers_ret(fn_def, |s| ctx.env.diverges(s))
        && subst_tys
            .iter()
            .chain(&self_hir)
            .any(|t| crate::lower::bounds::has_param(ctx, t))
    {
        return Some(var_instance(ctx, fn_def, self_ty, self_hint, &subst));
    }
    let key = cache_key(&fn_def.name, &subst_tys);
    let existing = ctx.env.sink.mono_cache.borrow().get(&key).copied();
    crate::lower::incr::log(
        || format!("m|{}|{}", key.0, key.1.join(",")),
        || crate::lower::incr::Lookup::Mono(key.0.clone(), subst_tys.clone(), existing),
    );
    if let Some(existing_id) = existing {
        if being_inferred(ctx.env, existing_id) {
            return None;
        }
        let ret_ty = ctx
            .env
            .sink
            .mono_sigs
            .borrow()
            .get(&existing_id)
            .map(|s| s.ret_ty.clone())
            .expect("mono signature registered with its cache entry");
        return Some((existing_id, ret_ty));
    }

    let new_id = FunctionId(ctx.env.sink.next_id.get());
    ctx.env.sink.next_id.set(new_id.0 + 1);
    // Pre-cache so a recursive call inside the body finds the in-progress mono.
    crate::lower::incr::touch(new_id);
    crate::lower::incr::touch_key(|| {
        crate::lower::incr::Key::Mono(key.0.clone(), subst_tys.clone())
    });
    ctx.env.sink.mono_cache.borrow_mut().insert(key, new_id);
    // Resolve the return type up front for recursive calls in the body.
    let mut preview_ret_ty =
        resolve_in_mono(ctx, fn_def, &subst, self_hir.clone(), &sig_ret_ast(fn_def));
    if infers_ret(fn_def, |s| ctx.env.diverges(s)) {
        preview_ret_ty = with_ok(preview_ret_ty, HirTy::Unknown);
    }
    let deferred = defers_refs(fn_def);
    let refs: Vec<(usize, HirTy)> = fn_def
        .params
        .iter()
        .enumerate()
        .filter(|(_, p)| p.by_ref)
        .map(|(i, p)| {
            let ty = match (&p.ty, &self_hir) {
                (_, Some(st)) if p.name == "self" => st.clone(),
                (Some(t), _) => resolve_in_mono(ctx, fn_def, &subst, self_hir.clone(), t),
                (None, _) => HirTy::Any,
            };
            (i, ref_slot_ty(deferred, ty))
        })
        .collect();
    if !refs.is_empty() {
        ctx.env.sink.ref_out.borrow_mut().insert(
            new_id,
            RefOut {
                ret: preview_ret_ty.clone(),
                refs,
                deferred,
            },
        );
    }
    ctx.env.sink.mono_sigs.borrow_mut().insert(
        new_id,
        FnSig {
            id: new_id,
            ret_ty: preview_ret_ty,
            param_defaults: Vec::new(),
            param_tys: Vec::new(),
            param_names: Vec::new(),
            param_refs: Vec::new(),
            variadic: false,
        },
    );

    // Stable keys (types by name) so the symbol is the same in every build
    // that makes it (prebuilt users, REPL cells).
    let mono_args = subst_tys
        .iter()
        .map(HirTy::stable_key)
        .collect::<Vec<_>>()
        .join("_");
    let mangled_name = format!("{}{}{mono_args}", fn_def.name, nabic_ir_common::MONO_MARK);

    ctx.env
        .sink
        .mono_depth
        .set(ctx.env.sink.mono_depth.get() + 1);
    let mono_fn = lower_instance(
        ctx,
        fn_def,
        new_id,
        mangled_name,
        self_hir,
        fn_module,
        &subst,
        span,
    );
    ctx.env
        .sink
        .mono_depth
        .set(ctx.env.sink.mono_depth.get() - 1);
    let mut mono_fn = mono_fn;
    mono_fn.facts.mono_args = Some(mono_args);

    let ret_ty = mono_fn.ret_ty.clone();
    let mono_param_tys: Vec<HirTy> = mono_fn.params.iter().map(|p| p.ty.clone()).collect();
    let mono_param_names: Vec<String> = mono_fn.params.iter().map(|p| p.name.clone()).collect();
    ctx.env.sink.mono_sigs.borrow_mut().insert(
        new_id,
        FnSig {
            id: new_id,
            ret_ty: ret_ty.clone(),
            param_defaults: Vec::new(),
            param_tys: mono_param_tys,
            param_names: mono_param_names,
            param_refs: mono_fn.params.iter().map(|p| p.by_ref).collect(),
            variadic: false,
        },
    );
    ctx.env.sink.extras.borrow_mut().push(mono_fn);
    Some((new_id, ret_ty))
}

/// Each `T: Bound` and `where` predicate of a call of `fn_def` under
/// `subst` needs an impl; `false` if one was reported missing.
pub(crate) fn check_call_bounds(
    ctx: &LowerCtx<'_>,
    fn_def: &FnDef,
    subst: &HashMap<String, HirTy>,
    self_hir: Option<&HirTy>,
    fn_module: &str,
) -> bool {
    let mut met = true;
    let mut goals: Vec<(HirTy, &Vec<nabic_ast::Ty>)> = Vec::new();
    for gp in &fn_def.generics.params {
        if let nabic_ast::GenericParam::Type { name, bounds, .. } = gp {
            if let Some(concrete) = subst.get(name) {
                goals.push((concrete.clone(), bounds));
            }
        }
    }
    for w in &fn_def.where_clause {
        goals.push((
            resolve_in_mono(ctx, fn_def, subst, self_hir.cloned(), &w.ty),
            &w.bounds,
        ));
    }
    for (concrete, bounds) in goals {
        let concrete = &concrete;
        if method_target_for(concrete).is_none() {
            continue;
        }
        for bound in bounds {
            // `T: Iterator<Item = i32>` also checks the impl's arguments
            // and associated types.
            let Some(want) = ctx.bound_ref(bound, fn_module, |t| {
                resolve_in_mono(ctx, fn_def, subst, self_hir.cloned(), t)
            }) else {
                continue;
            };
            // A bound naming no trait was reported where it is written.
            if !ctx.env.names.is_trait(&want.path) {
                continue;
            }
            if !ctx.implements_trait(concrete, &want, 0) {
                met = false;
                // An auto trait says which part lacks it, and what to do.
                // `xs.freeze()` of a list of lists: what is frozen must not
                // change, the elements included.
                let freeze = fn_def
                    .name
                    .split(nabic_ir_common::MONO_MARK)
                    .next()
                    .is_some_and(|n| {
                        n.ends_with("__freeze") && n.starts_with("core::collections::")
                    })
                    && want.path == ctx.lang("sync_trait");
                let why = if freeze {
                    String::new()
                } else if ctx.env.names.auto_traits.contains(want.path.as_str()) {
                    ctx.auto_trait_cause(concrete, &want.path, 0)
                } else {
                    String::new()
                };
                ctx.emit_error(
                    Code::UNSATISFIED_BOUND,
                    format!(
                        "type `{}` does not satisfy trait bound `{}` required by `{}`{why}",
                        ctx.ty_name(concrete),
                        ctx.trait_name(&want),
                        // `task::spawn` lowered to its plain twin is `spawn`.
                        ctx.display_path(if fn_def.name == "core::task::spawn_plain" {
                            "core::task::spawn"
                        } else {
                            fn_def.name.as_str()
                        })
                    ),
                );
                if want.path == crate::lower::reflect::INSPECT {
                    ctx.help_last(crate::lower::reflect::derive_inspect_help(ctx, concrete));
                } else if !freeze && why.is_empty() {
                    ctx.help_last(crate::lower::bounds::implementors_help(ctx, &want));
                }
                if freeze {
                    let t = ctx.ty_name(concrete);
                    let base = t.split('<').next().unwrap_or_default();
                    let frozen = match base {
                        "List" | "Dict" | "Set" => format!(" (`Frozen{t}`)"),
                        _ => String::new(),
                    };
                    ctx.help_last(Some(format!(
                        "a frozen collection never changes, and neither may what it holds, but \
                         a `{t}` can: freeze each element first{frozen}, or share the whole \
                         collection through a `Mutex`"
                    )));
                }
                if crate::lower::expr::is_order_trait(ctx, &want.path)
                    && crate::lower::expr::sum_value_terms(concrete) > 1
                {
                    ctx.help_last(Some(format!(
                        "a sum of different types has no order; {}",
                        crate::lower::expr::sum_order_help()
                    )));
                }
                if crate::lower::expr::is_order_trait(ctx, &want.path)
                    && matches!(concrete, HirTy::Prim(p) if p.is_float())
                {
                    ctx.help_last(Some(float_order_help(&ctx.display_path(&fn_def.name))));
                }
                // `int` / `float` into a float rounds, so it is no `From`.
                let to = ctx.ty_name(concrete);
                let from = ctx.trait_name(&want);
                if matches!(to.as_str(), "f32" | "f64")
                    && matches!(from.as_str(), "From<int>" | "From<float>")
                {
                    ctx.help_last(Some(format!(
                        "`From` is lossless; use `x.to_f64()` (rounds), or `try {to}::try_from(x)` \
                         to throw past the range"
                    )));
                }
                // `ThreadLocal::get` copies; other values are borrowed.
                if fn_def.name.starts_with("std::thread::ThreadLocal")
                    && fn_def.name.ends_with("get")
                {
                    ctx.help_last(Some(
                        "use the value inside `with`: `tl.with(|v| ..)`, or take it out with \
                         `tl.replace(v)` / `tl.take()`"
                            .to_owned(),
                    ));
                }
            }
        }
    }
    met
}

/// Floats are not ordered (NaN), so `fn_name` needing an order gets the
/// float method that works instead.
fn float_order_help(fn_name: &str) -> String {
    let form = match fn_name.rsplit("::").next() {
        Some(m @ ("min_by_key" | "max_by_key")) => {
            let op = if m == "min_by_key" { '<' } else { '>' };
            format!(
                "compare the keys in `.reduce`, `k` being your key: \
                 `.reduce(|a, b| if k(b) {op} k(a) {{ b }} else {{ a }})`"
            )
        }
        Some("sort") => "sort by the total order: `xs.sort_by(f64::total_cmp)`".to_owned(),
        _ => "compare with `<` / `>` or `partial_cmp`, which floats have".to_owned(),
    };
    format!("floats are not `Ord`, since NaN has no order; {form}")
}

/// Is `id`'s return type being inferred (a call of it from its own
/// body)? Recorded for `infer_ret_of`.
fn being_inferred(env: &Env<'_>, id: FunctionId) -> bool {
    let open = matches!(inferred_ret(env, id), Some(None));
    if open {
        env.sink.recursive_ret.borrow_mut().push(id);
    }
    open
}

/// `lower_fn_inner` for an instance, its call site recorded for the
/// body's errors.
#[allow(clippy::too_many_arguments)]
fn lower_instance(
    ctx: &LowerCtx<'_>,
    fn_def: &FnDef,
    id: FunctionId,
    name: String,
    self_hir: Option<HirTy>,
    module: String,
    subst: &HashMap<String, HirTy>,
    span: nabic_diagnostic::Span,
) -> HirFunction {
    let implicit = fn_def.generics.params.iter().any(
        |gp| matches!(gp, nabic_ast::GenericParam::Type { name, .. } if name.starts_with("T#")),
    );
    let site = (fn_def.name.clone(), span, implicit);
    ctx.env.sink.call_sites.borrow_mut().push(site);
    let f = lower_fn_inner(ctx.env, fn_def, id, name, self_hir, module, subst);
    ctx.env.sink.call_sites.borrow_mut().pop();
    f
}

/// An implicit generic (`fn add(a, b) { a + b }`) has no signature to
/// say what its parameters must be, nor one without `-> T` what it
/// returns: its body is lowered over the open arguments, so its uses fix
/// them (`add(1, x_i32)`), then undone. Returns the type it returns.
fn open_body(
    ctx: &LowerCtx<'_>,
    fn_def: &FnDef,
    id: FunctionId,
    self_hint: Option<&HirTy>,
    subst: &HashMap<String, HirTy>,
) -> Option<HirTy> {
    let sink = &ctx.env.sink;
    let implicit =
        fn_def.generics.params.iter().any(
            |g| matches!(g, nabic_ast::GenericParam::Type { name, .. } if name.starts_with("T#")),
        );
    let infers = infers_ret(fn_def, |s| ctx.env.diverges(s));
    if !(implicit || infers) || !crate::lower::infer::active(ctx) || sink.implicit_depth.get() > 0 {
        return None;
    }
    let mut marker = LowerCtx::new(ctx.env, subst, None, fn_def.span);
    let mark = marker.probe_mark();
    sink.implicit_depth.set(1);
    let mut tys = Vec::new();
    let _ = lower_fn_with(
        ctx.env,
        fn_def,
        id,
        fn_def.name.clone(),
        self_hint.cloned(),
        fn_module(ctx.env, fn_def),
        subst,
        infers.then_some(HirTy::Unknown),
        infers.then_some(&mut tys),
    );
    sink.implicit_depth.set(0);
    marker.rollback(&mark);
    if !infers {
        return None;
    }
    tys.retain(|t| !matches!(t, HirTy::Never | HirTy::Unit));
    let (first, rest) = tys.split_first()?;
    rest.iter()
        .all(|t| crate::lower::infer::unify(ctx, first, t))
        .then(|| crate::lower::infer::resolve(ctx, first))
}

/// An open instance's bounds fix its variables where one impl decides
/// (`collect::<List<i32>>()` over `#var` items: `FromIterator<#var>`).
fn unify_bounds(
    ctx: &LowerCtx<'_>,
    fn_def: &FnDef,
    self_ty: Option<&Ty>,
    self_hint: Option<&HirTy>,
    subst: &HashMap<String, HirTy>,
) {
    let module = module_of(&fn_def.name);
    let self_hir = self_hint
        .cloned()
        .or_else(|| self_ty.map(|t| resolve_in_mono(ctx, fn_def, subst, None, t)));
    let mut goals: Vec<(HirTy, &Vec<Ty>, Option<&str>)> = Vec::new();
    for gp in &fn_def.generics.params {
        if let nabic_ast::GenericParam::Type { name, bounds, .. } = gp {
            if let Some(t) = subst.get(name) {
                goals.push((t.clone(), bounds, Some(name)));
            }
        }
    }
    for w in &fn_def.where_clause {
        let t = resolve_in_mono(ctx, fn_def, subst, self_hir.clone(), &w.ty);
        goals.push((t, &w.bounds, None));
    }
    for (t, bounds, name) in goals {
        let t = &t;
        for b in bounds {
            let want = ctx.bound_ref(b, &module, |x| {
                resolve_in_mono(ctx, fn_def, subst, self_hir.clone(), x)
            });
            if let Some(want) = want {
                if !ctx.implements_trait(t, &want, 0) {
                    if let Some(n) = name {
                        literal_kind_error(ctx, t, &want, n);
                    }
                }
            }
        }
    }
}

/// Integer literals filling `T: FloatingPoint`: no integer type has the
/// bound but a float type does (`lerp(0, 10, 1)`). Said once, with the
/// call spelled with float literals; the literals take that float type.
fn literal_kind_error(ctx: &LowerCtx<'_>, t: &HirTy, want: &TraitRef, param: &str) {
    let Some(HirTy::Prim(held)) = crate::lower::infer::literal_default(ctx, t) else {
        return;
    };
    if !held.is_int() {
        return;
    }
    let works = |p: PrimTy| {
        crate::lower::infer::trial(ctx, &|| ctx.implements_trait(&HirTy::Prim(p), want, 0))
    };
    let Some(float) = [PrimTy::F64, PrimTy::F32].into_iter().find(|p| works(*p)) else {
        return;
    };
    ctx.emit_error(
        Code::UNSATISFIED_BOUND,
        format!(
            "integer literals cannot fill `{param}: {}`",
            ctx.trait_name(want)
        ),
    );
    if let Some(call) = ctx.src(ctx.here()) {
        ctx.help_last(Some(format!(
            "write float literals: `{}`",
            float_literals(call)
        )));
    }
    crate::lower::infer::force_literal(ctx, t, float);
}

/// `call` with its integer literal arguments written as floats
/// (`lerp(0, 10, 1)` -> `lerp(0.0, 10.0, 1.0)`).
fn float_literals(call: &str) -> String {
    let Some(open) = call.find('(') else {
        return call.to_owned();
    };
    let (head, args) = call.split_at(open);
    let b = args.as_bytes();
    let mut out = String::from(head);
    let mut i = 0;
    while i < b.len() {
        let start = i;
        let word = |c: u8| c.is_ascii_alphanumeric() || c == b'_' || c == b'.';
        if b[i].is_ascii_digit() && (i == 0 || !word(b[i - 1])) {
            while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'_') {
                i += 1;
            }
            out.push_str(&args[start..i]);
            if i >= b.len() || !word(b[i]) {
                out.push_str(".0");
            }
            continue;
        }
        out.push(char::from(b[i]));
        i += 1;
    }
    out
}

fn var_instance(
    ctx: &LowerCtx<'_>,
    fn_def: &FnDef,
    self_ty: Option<&Ty>,
    self_hint: Option<&HirTy>,
    subst: &HashMap<String, HirTy>,
) -> (FunctionId, HirTy) {
    let id = FunctionId(ctx.env.sink.next_id.get());
    ctx.env.sink.next_id.set(id.0 + 1);
    crate::lower::incr::touch(id);
    let self_hir = self_hint
        .cloned()
        .or_else(|| self_ty.map(|t| resolve_in_mono(ctx, fn_def, subst, None, t)));
    let ret_ty = resolve_in_mono(ctx, fn_def, subst, self_hir.clone(), &sig_ret_ast(fn_def));
    let param_tys: Vec<HirTy> = fn_def
        .params
        .iter()
        .map(|p| match (&p.ty, &self_hir) {
            (_, Some(st)) if p.name == "self" => st.clone(),
            (Some(t), _) => resolve_in_mono(ctx, fn_def, subst, self_hir.clone(), t),
            (None, _) => HirTy::Any,
        })
        .collect();
    let refs: Vec<(usize, HirTy)> = fn_def
        .params
        .iter()
        .zip(&param_tys)
        .enumerate()
        .filter(|(_, (p, _))| p.by_ref)
        .map(|(i, (_, t))| (i, ref_slot_ty(defers_refs(fn_def), t.clone())))
        .collect();
    if !refs.is_empty() {
        ctx.env.sink.ref_out.borrow_mut().insert(
            id,
            RefOut {
                ret: ret_ty.clone(),
                refs,
                deferred: defers_refs(fn_def),
            },
        );
    }
    ctx.env.sink.mono_sigs.borrow_mut().insert(
        id,
        FnSig {
            id,
            ret_ty: ret_ty.clone(),
            param_defaults: Vec::new(),
            param_names: fn_def.params.iter().map(|p| p.name.clone()).collect(),
            param_refs: fn_def.params.iter().map(|p| p.by_ref).collect(),
            param_tys,
            variadic: false,
        },
    );
    (id, ret_ty)
}
