//! The environment's answers to the checks on finished HIR (`Facts`).

#[allow(clippy::wildcard_imports)]
use crate::lower::*;
use nabic_core::HashSet;
use nabic_diagnostic::SourceMap;
use nabic_hir_check::Facts;
use std::cell::Ref;

/// `env` and a context outside any body, for one check.
pub(crate) struct EnvFacts<'a> {
    env: &'a Env<'a>,
    ctx: LowerCtx<'a>,
}

impl<'a> EnvFacts<'a> {
    pub(crate) fn new(env: &'a Env<'a>, no_subst: &'a HashMap<String, HirTy>) -> Self {
        Self {
            env,
            ctx: LowerCtx::new(env, no_subst, None, Span::DUMMY),
        }
    }
}

impl Facts for EnvFacts<'_> {
    fn structs(&self) -> Ref<'_, Vec<HirStructDef>> {
        self.env.defs.structs.borrow()
    }

    fn enums(&self) -> Ref<'_, Vec<HirEnumDef>> {
        self.env.defs.enums.borrow()
    }

    fn struct_def(&self, id: StructId) -> HirStructDef {
        self.ctx.struct_def(id)
    }

    fn enum_def(&self, id: EnumId) -> HirEnumDef {
        self.ctx.enum_def(id)
    }

    fn extras(&self) -> Ref<'_, Vec<HirFunction>> {
        self.env.sink.extras.borrow()
    }

    fn lang(&self, slot: &str) -> &str {
        self.env.names.lang_item(slot)
    }

    fn has_trait(&self, ty: &HirTy, path: &str) -> bool {
        self.ctx.has_trait(ty, path)
    }

    fn has_impl(&self, ty: &HirTy, want: &TraitRef, depth: u32) -> bool {
        self.ctx.has_impl(ty, want, depth)
    }

    fn has_param(&self, ty: &HirTy) -> bool {
        bounds::has_param(&self.ctx, ty)
    }

    fn auto_trait_cause(&self, ty: &HirTy, key: &str, depth: u32) -> String {
        self.ctx.auto_trait_cause(ty, key, depth)
    }

    fn ty_name(&self, ty: &HirTy) -> String {
        self.ctx.ty_name(ty)
    }

    fn sources(&self) -> &SourceMap {
        self.env.sources
    }

    fn library_plain(&self) -> HashSet<FunctionId> {
        let env = self.env;
        (0..env.decls.len())
            .filter(|&i| {
                let f = env.decls[i].def();
                let module = match &env.decls[i] {
                    MethodOrFn::Fn(f) => fn_module(env, f),
                    MethodOrFn::Method { module, .. } => module.clone(),
                };
                nabic_core::in_toolchain(&module)
                    && f.generics.params.is_empty()
                    && !f.is_async
                    && !f.is_generator
                    && !nabic_ast::inline_wanted(&f.attrs)
                    && mono::opaque_bounds(f).is_none()
            })
            .map(|i| FunctionId(u32::try_from(i).unwrap_or(u32::MAX)))
            .collect()
    }

    fn error_at(&self, code: Code, at: Span, msg: String) {
        self.ctx.emit_error_at(code, at, msg);
    }
}
