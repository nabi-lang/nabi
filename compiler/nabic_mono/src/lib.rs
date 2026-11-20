//! Generic function instantiation (monomorphization): binding a call's
//! type parameters, making and caching instances, open instances under
//! inference. Instances are lowered by `nabic_typeck`, whose lowering
//! this crate reaches through `nabic_typeck::lower`; typeck reaches it
//! only through `nabic_typeck::Mono`, which `install` fills.

mod instance;

use nabic_hir::{HirExpr, HirTy};
use nabic_typeck::lower;

/// Fill typeck's `Mono` hook; needed before lowering, and idempotent.
pub fn install() {
    nabic_typeck::set_mono(&Hook);
}

struct Hook;

impl nabic_typeck::Mono for Hook {
    fn monomorphize_on(
        &self,
        ctx: &lower::LowerCtx<'_>,
        fn_def: &nabic_ast::FnDef,
        self_ty: Option<&nabic_ast::Ty>,
        self_hint: Option<&HirTy>,
        args: &[HirExpr],
        span: nabic_diagnostic::Span,
    ) -> Option<(nabic_ir_common::FunctionId, HirTy)> {
        instance::monomorphize_on(ctx, fn_def, self_ty, self_hint, args, span)
    }

    fn bind_self(
        &self,
        ctx: &lower::LowerCtx<'_>,
        fn_def: &nabic_ast::FnDef,
        self_ty: &nabic_ast::Ty,
        hint: &HirTy,
        subst: &mut nabic_core::HashMap<String, HirTy>,
    ) {
        instance::bind_self(ctx, fn_def, self_ty, hint, subst);
    }

    fn bind_from_arg(
        &self,
        ctx: &lower::LowerCtx<'_>,
        fn_def: &nabic_ast::FnDef,
        self_ty: Option<&nabic_ast::Ty>,
        i: usize,
        arg_ty: &HirTy,
        subst: &mut nabic_core::HashMap<String, HirTy>,
    ) {
        instance::bind_from_arg(ctx, fn_def, self_ty, i, arg_ty, subst);
    }

    fn unify_param_ty_with(
        &self,
        param_ty: &nabic_ast::Ty,
        arg_ty: &HirTy,
        generic_names: &nabic_core::HashSet<String>,
        subst: &mut nabic_core::HashMap<String, HirTy>,
        ctx: &lower::LowerCtx<'_>,
    ) {
        instance::unify_param_ty_with(param_ty, arg_ty, generic_names, subst, ctx);
    }

    fn expected_subst(
        &self,
        ctx: &lower::LowerCtx<'_>,
        fn_def: &nabic_ast::FnDef,
    ) -> nabic_core::HashMap<String, HirTy> {
        instance::expected_subst(ctx, fn_def)
    }

    fn resolve_in_mono(
        &self,
        ctx: &lower::LowerCtx<'_>,
        fn_def: &nabic_ast::FnDef,
        subst: &nabic_core::HashMap<String, HirTy>,
        self_ty: Option<HirTy>,
        ty: &nabic_ast::Ty,
    ) -> HirTy {
        instance::resolve_in_mono(ctx, fn_def, subst, self_ty, ty)
    }

    fn check_call_bounds(
        &self,
        ctx: &lower::LowerCtx<'_>,
        fn_def: &nabic_ast::FnDef,
        subst: &nabic_core::HashMap<String, HirTy>,
        self_hir: Option<&HirTy>,
        fn_module: &str,
    ) {
        instance::check_call_bounds(ctx, fn_def, subst, self_hir, fn_module);
    }

    fn solve_instance(
        &self,
        ctx: &lower::LowerCtx<'_>,
        p: &lower::mono::OpenInst,
    ) -> Option<nabic_ir_common::FunctionId> {
        instance::solve_instance(ctx, p)
    }
}
