//! The ownership and safety checks on finished HIR: use after hand-off
//! (`handoff`), `Send` frames (`send`), stale views (`views`), frame
//! pointers across a suspend (`frameptr`), and lost writes on copies
//! (`lostwrite`, on the AST). They only read; what they ask of the
//! environment goes through `Facts`, which `nabic_typeck` answers.

pub mod frameptr;
pub mod handoff;
pub mod lostwrite;
pub mod send;
pub mod views;

use nabic_core::HashSet;
use nabic_diagnostic::{Code, SourceMap, Span};
use nabic_hir::{EnumId, HirEnumDef, HirFunction, HirStructDef, HirTy, StructId, TraitRef};
use nabic_ir_common::FunctionId;
use std::cell::Ref;

/// The environment's type queries and error sink.
pub trait Facts {
    /// Every struct, by `StructId`.
    fn structs(&self) -> Ref<'_, Vec<HirStructDef>>;
    /// Every enum, by `EnumId`.
    fn enums(&self) -> Ref<'_, Vec<HirEnumDef>>;
    fn struct_def(&self, id: StructId) -> HirStructDef;
    fn enum_def(&self, id: EnumId) -> HirEnumDef;
    /// The bodies lowering lifted out (closures, instances), after the
    /// module's own functions.
    fn extras(&self) -> Ref<'_, Vec<HirFunction>>;
    /// The item bound to lang slot `slot`, `""` if unbound.
    fn lang(&self, slot: &str) -> &str;
    /// Does `ty` implement trait `path` (whatever its arguments)?
    fn has_trait(&self, ty: &HirTy, path: &str) -> bool;
    /// Does an impl cover `ty` for `want`?
    fn has_impl(&self, ty: &HirTy, want: &TraitRef, depth: u32) -> bool;
    /// Does `ty` mention a type parameter?
    fn has_param(&self, ty: &HirTy) -> bool;
    /// The part of `ty` that keeps it from auto trait `key`.
    fn auto_trait_cause(&self, ty: &HirTy, key: &str, depth: u32) -> String;
    /// `ty` as the user writes it.
    fn ty_name(&self, ty: &HirTy) -> String;
    fn sources(&self) -> &SourceMap;
    /// The standard library's plain functions (no generics, coroutine,
    /// `#[inline]` or `impl Trait`): the flows take a call of one as
    /// handing its arguments to its result, as if it had no body. Followed
    /// into, its stores into the library's own fields would reach every
    /// reader of them in the program.
    fn library_plain(&self) -> HashSet<FunctionId>;
    /// Report an error at `at`.
    fn error_at(&self, code: Code, at: Span, msg: String);
}

/// What a call keeps of its arguments, as the affine check sees it.
pub trait Keeps {
    /// Does a call of `f` keep (move, close or store) argument `i`?
    fn keeps(&self, f: FunctionId, i: u32) -> bool;
}

/// The scoped views of a stream (`r.lines()`): they do not own it, and
/// the hand-off check keeps them in its owner's scope.
pub const STREAM_VIEWS: &[&str] = &[
    "core::io::Lines",
    "core::io::Split",
    "core::io::Bytes",
    "core::io::Take",
    "core::io::BytesStream",
    "core::io::LinesStream",
    "std::net::Incoming",
];

/// The temporary an f-string binds a hole's value to: a view of a place.
pub fn is_hole_temp(name: &str) -> bool {
    name.strip_prefix("#f")
        .is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
}
