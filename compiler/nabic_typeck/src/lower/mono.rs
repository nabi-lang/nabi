//! Function lowering entry points, generic struct / enum instantiation,
//! and the hook to generic function instantiation (`Mono`, in
//! `nabic_mono`).

mod body;
mod generics;
mod opaque;
mod refs;
mod returns;

pub use crate::lower::mono::body::*;
pub(crate) use crate::lower::mono::generics::arity_msg;
pub use crate::lower::mono::generics::*;
pub(crate) use crate::lower::mono::opaque::*;
pub use crate::lower::mono::refs::*;
pub use crate::lower::mono::returns::*;

#[allow(clippy::wildcard_imports)]
use crate::lower::*;
use nabic_ast::{AttrArg, AttrBody};
use nabic_diagnostic::Code;
use nabic_ir_common::Inline;

/// Generic function instantiation, which `nabic_mono` provides: it
/// lowers instances through this crate, so it sits above it. The one way
/// typeck calls into it.
pub trait Mono: Sync {
    /// Find or make the instance of a generic function for the call's
    /// lowered argument types, `Self` known up front when `self_hint` is
    /// set (see `bind_self`); `(fn id, return type)`, or `None` after
    /// reporting.
    fn monomorphize_on(
        &self,
        ctx: &LowerCtx<'_>,
        fn_def: &FnDef,
        self_ty: Option<&Ty>,
        self_hint: Option<&HirTy>,
        args: &[HirExpr],
        span: nabic_diagnostic::Span,
    ) -> Option<(FunctionId, HirTy)>;

    /// Bind the impl parameters of `self_ty` (`Box<T>`) from the known
    /// `Self` type `hint` (`Box<i32>` -> `T = i32`).
    fn bind_self(
        &self,
        ctx: &LowerCtx<'_>,
        fn_def: &FnDef,
        self_ty: &Ty,
        hint: &HirTy,
        subst: &mut HashMap<String, HirTy>,
    );

    /// Bind `fn_def`'s type params from the type of argument `i`. A
    /// generic method's `self` is typed by its impl target `self_ty`.
    fn bind_from_arg(
        &self,
        ctx: &LowerCtx<'_>,
        fn_def: &FnDef,
        self_ty: Option<&Ty>,
        i: usize,
        arg_ty: &HirTy,
        subst: &mut HashMap<String, HirTy>,
    );

    /// Match an AST parameter type against an argument's `HirTy` to bind
    /// generic parameters (`[T; N]` against `[i32; 4]` binds `T -> i32`).
    fn unify_param_ty_with(
        &self,
        param_ty: &Ty,
        arg_ty: &HirTy,
        generic_names: &nabic_core::HashSet<String>,
        subst: &mut HashMap<String, HirTy>,
        ctx: &LowerCtx<'_>,
    );

    /// What the expected type of a call of `fn_def` says of its type
    /// parameters (none for a loose slot: `any`, `()`).
    fn expected_subst(&self, ctx: &LowerCtx<'_>, fn_def: &FnDef) -> HashMap<String, HirTy>;

    /// Resolve `ty` under the substitution of a monomorphization in
    /// progress, from `fn_def`'s module. `self_ty` (resolved) fills `Self`.
    fn resolve_in_mono(
        &self,
        ctx: &LowerCtx<'_>,
        fn_def: &FnDef,
        subst: &HashMap<String, HirTy>,
        self_ty: Option<HirTy>,
        ty: &Ty,
    ) -> HirTy;

    /// Each `T: Bound` and `where` predicate of a call of `fn_def` under
    /// `subst` needs an impl.
    fn check_call_bounds(
        &self,
        ctx: &LowerCtx<'_>,
        fn_def: &FnDef,
        subst: &HashMap<String, HirTy>,
        self_hir: Option<&HirTy>,
        fn_module: &str,
    );

    /// The instance `p` stands for, under the solved types (`None`: still
    /// open, or it failed and said why).
    fn solve_instance(&self, ctx: &LowerCtx<'_>, p: &OpenInst) -> Option<FunctionId>;
}

static MONO: std::sync::OnceLock<&'static dyn Mono> = std::sync::OnceLock::new();

/// Set the `Mono` provider (`nabic_mono::install`); the first one stays.
pub fn set_mono(m: &'static dyn Mono) {
    let _ = MONO.set(m);
}

fn mono() -> &'static dyn Mono {
    *MONO
        .get()
        .expect("no generic instantiation: call `nabic_mono::install()` before lowering")
}

/// `Mono::monomorphize_on` with `Self` from the arguments.
pub(crate) fn monomorphize(
    ctx: &LowerCtx<'_>,
    fn_def: &FnDef,
    self_ty: Option<&Ty>,
    args: &[HirExpr],
    span: nabic_diagnostic::Span,
) -> Option<(FunctionId, HirTy)> {
    mono().monomorphize_on(ctx, fn_def, self_ty, None, args, span)
}

pub(crate) fn monomorphize_on(
    ctx: &LowerCtx<'_>,
    fn_def: &FnDef,
    self_ty: Option<&Ty>,
    self_hint: Option<&HirTy>,
    args: &[HirExpr],
    span: nabic_diagnostic::Span,
) -> Option<(FunctionId, HirTy)> {
    mono().monomorphize_on(ctx, fn_def, self_ty, self_hint, args, span)
}

pub(crate) fn bind_self(
    ctx: &LowerCtx<'_>,
    fn_def: &FnDef,
    self_ty: &Ty,
    hint: &HirTy,
    subst: &mut HashMap<String, HirTy>,
) {
    mono().bind_self(ctx, fn_def, self_ty, hint, subst);
}

pub(crate) fn bind_from_arg(
    ctx: &LowerCtx<'_>,
    fn_def: &FnDef,
    self_ty: Option<&Ty>,
    i: usize,
    arg_ty: &HirTy,
    subst: &mut HashMap<String, HirTy>,
) {
    mono().bind_from_arg(ctx, fn_def, self_ty, i, arg_ty, subst);
}

pub(crate) fn unify_param_ty_with(
    param_ty: &Ty,
    arg_ty: &HirTy,
    generic_names: &nabic_core::HashSet<String>,
    subst: &mut HashMap<String, HirTy>,
    ctx: &LowerCtx<'_>,
) {
    mono().unify_param_ty_with(param_ty, arg_ty, generic_names, subst, ctx);
}

pub(crate) fn expected_subst(ctx: &LowerCtx<'_>, fn_def: &FnDef) -> HashMap<String, HirTy> {
    mono().expected_subst(ctx, fn_def)
}

pub(crate) fn resolve_in_mono(
    ctx: &LowerCtx<'_>,
    fn_def: &FnDef,
    subst: &HashMap<String, HirTy>,
    self_ty: Option<HirTy>,
    ty: &Ty,
) -> HirTy {
    mono().resolve_in_mono(ctx, fn_def, subst, self_ty, ty)
}

pub(crate) fn check_call_bounds(
    ctx: &LowerCtx<'_>,
    fn_def: &FnDef,
    subst: &HashMap<String, HirTy>,
    self_hir: Option<&HirTy>,
    fn_module: &str,
) {
    mono().check_call_bounds(ctx, fn_def, subst, self_hir, fn_module);
}

pub(crate) fn solve_instance(ctx: &LowerCtx<'_>, p: &OpenInst) -> Option<FunctionId> {
    mono().solve_instance(ctx, p)
}

/// The parameter of `names` that makes `e` equal `want`, and its value:
/// `e` names it once, under `+`, `-`, `*` and negation by constants.
pub fn solve_const(
    e: &nabic_ast::Expr,
    want: i128,
    names: &nabic_core::HashSet<String>,
) -> Option<(String, i128)> {
    let known = |x: &nabic_ast::Expr| {
        crate::lower::consteval::syntactic_int(
            &nabic_ast::ConstExpr::Expr(Box::new(x.clone())),
            &|_| None,
        )
    };
    match &e.kind {
        nabic_ast::ExprKind::Path(p)
            if p.segments.len() == 1 && names.contains(&p.segments[0].name) =>
        {
            Some((p.segments[0].name.clone(), want))
        }
        nabic_ast::ExprKind::Block(b) if b.stmts.is_empty() => {
            solve_const(b.trailing.as_deref()?, want, names)
        }
        nabic_ast::ExprKind::OpPrefix { op, operand } if op == "-" => {
            solve_const(operand, want.checked_neg()?, names)
        }
        nabic_ast::ExprKind::OpInfix { op, lhs, rhs } => {
            let (l, r) = (known(lhs), known(rhs));
            match (op.as_str(), l, r) {
                ("+", _, Some(k)) => solve_const(lhs, want.checked_sub(k)?, names),
                ("+", Some(k), _) => solve_const(rhs, want.checked_sub(k)?, names),
                ("-", _, Some(k)) => solve_const(lhs, want.checked_add(k)?, names),
                ("-", Some(k), _) => solve_const(rhs, k.checked_sub(want)?, names),
                ("*", _, Some(k)) if k != 0 && want % k == 0 => solve_const(lhs, want / k, names),
                ("*", Some(k), _) if k != 0 && want % k == 0 => solve_const(rhs, want / k, names),
                _ => None,
            }
        }
        _ => None,
    }
}

/// An instance asked for over open types, made once they are solved.
#[derive(Clone)]
pub struct OpenInst {
    pub id: FunctionId,
    pub def: std::sync::Arc<FnDef>,
    pub self_ty: Option<Ty>,
    pub self_hint: Option<HirTy>,
    pub subst: HashMap<String, HirTy>,
    pub at: nabic_diagnostic::Span,
}

nabic_core::codec_struct!(OpenInst {
    id,
    def,
    self_ty,
    self_hint,
    subst,
    at
});

/// `mono_cache`'s key of `name` instantiated with `args`.
pub fn cache_key(name: &str, args: &[HirTy]) -> (String, Vec<String>) {
    (name.to_owned(), args.iter().map(instance_mangle).collect())
}
