//! Name resolution and type checking: the AST lowered to HIR (`nabic_hir`,
//! whose items the root imports so `crate::` paths name them). Generic
//! function instantiation is `nabic_mono`'s, reached through `Mono`.

/// Lowering internals, for `nabic_mono` (not a stable interface, so the
/// lints for one are off).
#[doc(hidden)]
#[allow(
    clippy::derive_partial_eq_without_eq,
    clippy::implicit_hasher,
    clippy::too_long_first_doc_paragraph
)]
pub mod lower;

mod input;
#[cfg(test)]
mod tests;
mod ty;
mod uses;

pub use input::{HirInput, RootMain, Script, TemplateRefs};
pub use lower::mono::{Mono, set_mono};
pub use lower::{Bodies, BodyCache, FN_BLOCK_LABEL, SHADOW, Unit, type_visible, unshadowed};
pub use lower::{
    KeptLibrary, LibraryStage, keep_library, kept_library, lower, lower_full, lower_with_uses,
    stage,
};
pub use ty::resolve_ty;
pub use uses::{ReExport, UseEntry, UseTable, build_use_table, check_use_targets};

#[allow(clippy::wildcard_imports)]
use nabic_hir::*;
