//! What a backend builds on: calling conventions (`abi`), symbols
//! (`mangle`), codegen units (`units`), MIR analyses (`analysis`) and REPL
//! cell reuse (`repl`). Backend-neutral: no backend's IR in here.

pub mod abi;
pub mod analysis;
pub mod mangle;
pub mod repl;
pub mod units;

pub use mangle::{Demangled, demangle, pkg_hash};
pub use repl::{ReplCell, ReplFn};

use nabic_diagnostic::{Code, NabiDiagnostic};

/// What codegen says about a module, besides its code.
#[derive(Clone, Debug, Default)]
pub struct CodegenOutput {
    /// Number of GC type ids used.
    pub gc_types: u32,
    /// A REPL cell: every `ScanTable` of the session so far (the next
    /// cell's `ReplCell::gc_tables`).
    pub gc_tables: Vec<Vec<u32>>,
    /// A REPL cell: the exported symbols it defines anew (`repl::reuse`).
    pub fresh: Vec<String>,
    /// A REPL cell: the functions it defines anew.
    pub fns: Vec<ReplFn>,
    /// A REPL cell: how many functions it takes from earlier cells.
    pub reused: usize,
}

/// One diagnostic for codegen's errors: the first at its function, the
/// rest as notes.
#[must_use]
pub fn codegen_error(errors: &[(nabic_diagnostic::Span, String)]) -> Option<NabiDiagnostic> {
    let ((at, first), rest) = errors.split_first()?;
    let mut d = NabiDiagnostic::error(Code::UNSUPPORTED, *at, first.clone());
    for (_, more) in rest {
        d = d.with_note(more.clone());
    }
    Some(d)
}
