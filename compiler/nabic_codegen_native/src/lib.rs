//! nabi's code generator: MIR straight to machine code and an object file,
//! for programs, libraries and REPL cells.
//!
//! MIR -> LIR (`lower`: layouts, conventions, GC frames, barriers, checks)
//! -> machine code (one module per ISA) -> object (one module per format).

// Instruction selection names registers and encodings tersely; its
// dispatch matches are long by nature.
#![allow(clippy::many_single_char_names, clippy::cognitive_complexity)]

pub mod glue;
pub mod hot;

mod asm;
mod cc;
mod debug;
mod frame;
mod isa;
mod lir;
mod lower;
mod module;
mod obj;
mod plugin;
mod regalloc;
mod relax;
mod repl;
mod roots;
mod typeinfo;
mod winstart;
mod wrap;

pub use crate::isa::wasm::tls_object as wasm_tls_object;
pub use winstart::{dll_startup as windows_dll_startup, startup as windows_startup};

use nabic_codegen::repl::{Def, Kind};
use nabic_diagnostic::{Code, NabiDiagnostic};
use nabic_mir::MirModule;

/// `-O<level>`. Instruction selection is the same at every level; the
/// driver optimizes the MIR above `-O0` (`nabic_mir::optimize`).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum OptLevel {
    #[default]
    O0,
    O1,
    O2,
    O3,
    Os,
    Oz,
}

impl OptLevel {
    /// `0`..`3`, `s`, `z`.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "0" => Self::O0,
            "1" => Self::O1,
            "2" => Self::O2,
            "3" => Self::O3,
            "s" => Self::Os,
            "z" => Self::Oz,
            _ => return None,
        })
    }
}

/// How to generate code for a module (its target is `MirModule::target`).
#[derive(Clone, Debug, Default)]
pub struct Options {
    pub opt_level: OptLevel,
    /// Panic on integer overflow.
    pub overflow_checks: bool,
    /// `-g`: debug info (DWARF; `CodeView` on COFF).
    pub debug: bool,
    /// Generate a REPL cell instead of a program.
    pub repl: Option<nabic_codegen::ReplCell>,
    /// A REPL cell: the exported functions it defines anew (the rest the
    /// session has: declared only, `cell`).
    pub fresh_fns: Option<nabic_core::HashSet<String>>,
    /// `--emit=listing` / `--emit=asm`: the code as text (`Output::asm`).
    pub asm: Option<Text>,
}

/// The code as text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Text {
    /// Each function's instructions as `llvm-objdump -dr` prints them.
    Listing,
    /// The object as assembly source an assembler makes it from.
    Source,
}

/// A compiled module.
pub struct Output {
    /// The relocatable object file.
    pub object: Vec<u8>,
    /// Number of GC type ids used.
    pub gc_types: u32,
    /// Every `ScanTable` so far (a REPL cell: the next one's
    /// `ReplCell::gc_tables`).
    pub gc_tables: Vec<Vec<u32>>,
    /// `Options::asm`: the functions' listings, or the object's source.
    pub asm: Option<String>,
    /// The functions that never reach a safepoint (a library's
    /// `Summary::QUIET`).
    pub quiet: std::sync::Arc<nabic_core::HashSet<nabic_ir_common::FunctionId>>,
}

/// Why `module` can't be compiled: no instruction selector or object
/// writer for its target.
#[must_use]
pub fn unsupported(module: &MirModule) -> Option<String> {
    let t = &module.target;
    // Mach-O objects are arm64 only.
    let macho_x86 = t.object_format == nabic_mir::ObjectFormat::MachO && t.arch != "aarch64";
    (!t.native_backend || macho_x86).then(|| format!("no native backend for {}", t.name))
}

/// Why `module`'s code can't be text `how`: only the `x86_64`, arm64,
/// i386, 32-bit ARM, RISC-V, powerpc64, s390x and wasm32 encoders record
/// it.
#[must_use]
pub fn unlisted(module: &MirModule, how: Text) -> Option<String> {
    let t = &module.target;
    (!matches!(
        t.arch.as_str(),
        "x86_64"
            | "aarch64"
            | "x86"
            | "arm"
            | "riscv64"
            | "riscv32"
            | "powerpc64"
            | "s390x"
            | "wasm32"
    ))
    .then(|| {
        format!(
            "`--emit={}` covers x86_64, aarch64, x86, arm, riscv64, riscv32, powerpc64, \
             s390x and wasm32 code only, not {}; write `--emit=obj` and disassemble \
             the object",
            if how == Text::Source {
                "asm"
            } else {
                "listing"
            },
            t.name
        )
    })
}

/// Compile `module` to an object file.
pub fn compile(
    module: &MirModule,
    opts: &Options,
    sources: &nabic_diagnostic::SourceMap,
) -> Result<Output, NabiDiagnostic> {
    if let Some(why) = unsupported(module) {
        return Err(NabiDiagnostic::error(
            Code::UNSUPPORTED,
            nabic_diagnostic::Span::DUMMY,
            why,
        ));
    }
    finish(module, opts, sources, None)
}

/// Lower `module` for its codegen errors alone, writing no code
/// (`nabic check`). A target without this backend is not checked.
pub fn check(
    module: &MirModule,
    opts: &Options,
    sources: &nabic_diagnostic::SourceMap,
) -> Result<(), NabiDiagnostic> {
    if unsupported(module).is_some() {
        return Ok(());
    }
    let c = module::compile(module, opts, sources, None, &mut |_, _| {}, false);
    nabic_codegen::codegen_error(&c.errors).map_or(Ok(()), Err)
}

/// Compile REPL cell `module` (`opts.repl`) to an object file, only the
/// functions the session lacks (`ReplCell::known`): a first pass lowers
/// everything to find them.
pub fn cell(
    module: &MirModule,
    opts: &Options,
    sources: &nabic_diagnostic::SourceMap,
) -> Result<(Vec<u8>, nabic_codegen::CodegenOutput), NabiDiagnostic> {
    if let Some(why) = unsupported(module) {
        return Err(NabiDiagnostic::error(
            Code::UNSUPPORTED,
            nabic_diagnostic::Span::DUMMY,
            why,
        ));
    }
    let mut opts = opts.clone();
    opts.fresh_fns = None;
    let c = module::compile(module, &opts, sources, None, &mut |_, _| {}, true);
    if let Some(d) = nabic_codegen::codegen_error(&c.errors) {
        return Err(d);
    }
    let defs: Vec<Def<'_>> = c
        .defs
        .iter()
        .map(|d| Def {
            name: &d.name,
            kind: d.kind,
            hash: d.hash,
            refs: d.refs.iter().map(String::as_str).collect(),
        })
        .collect();
    let none = nabic_core::HashMap::default();
    let known = opts.repl.as_ref().map_or(&none, |r| &r.known);
    let (reused, fresh, fns) = nabic_codegen::repl::reuse(&defs, known);
    opts.fresh_fns = Some(
        c.defs
            .iter()
            .filter(|d| d.kind == Kind::Candidate && !reused.contains(d.name.as_str()))
            .filter_map(|d| d.key.clone())
            .collect(),
    );
    let o = finish(module, &opts, sources, None)?;
    Ok((
        o.object,
        nabic_codegen::CodegenOutput {
            gc_types: o.gc_types,
            gc_tables: o.gc_tables,
            fresh,
            fns,
            reused: reused.len(),
        },
    ))
}

/// Compile unit `u` of `plan` (`nabic_codegen::units`) to an object
/// file (the module must be one `unsupported` accepts).
pub fn compile_unit(
    module: &MirModule,
    opts: &Options,
    sources: &nabic_diagnostic::SourceMap,
    plan: &nabic_codegen::units::Plan,
    u: usize,
) -> Result<Output, NabiDiagnostic> {
    let unit = module::UnitArgs {
        core: &plan.core,
        u,
        glue: plan.units[u].glue,
        tables: &plan.gc_tables,
    };
    let o = finish(module, opts, sources, Some(&unit))?;
    if o.gc_tables.len() != plan.gc_tables.len() {
        return Err(NabiDiagnostic::error(
            Code::INTERNAL,
            nabic_diagnostic::Span::DUMMY,
            format!(
                "unit `{}` made a GC type its plan lacks",
                plan.units[u].name
            ),
        ));
    }
    Ok(o)
}

fn finish(
    module: &MirModule,
    opts: &Options,
    sources: &nabic_diagnostic::SourceMap,
    unit: Option<&module::UnitArgs<'_>>,
) -> Result<Output, NabiDiagnostic> {
    let emit = match module.target.arch.as_str() {
        "x86_64" => isa::x64::emit,
        "x86" => isa::i386::emit,
        "arm" => isa::arm32::emit,
        "riscv64" | "riscv32" => isa::riscv::emit,
        "powerpc64" => isa::ppc64::emit,
        "s390x" => isa::s390x::emit,
        "wasm32" => isa::wasm::emit,
        _ => isa::a64::emit,
    };
    let mut c = module::compile(module, opts, sources, unit, &mut { emit }, false);
    // COFF: `CodeView`, which Windows debuggers read, below.
    if opts.debug && !c.obj.coff {
        debug::dwarf::write(&mut c.obj, module, sources);
    }
    if let Some(d) = nabic_codegen::codegen_error(&c.errors) {
        return Err(d);
    }
    if c.obj.wasm.is_some() {
        let (object, asm) = isa::wasm::object::write_listed(&c.obj);
        return Ok(Output {
            object,
            gc_types: c.gc_types.len() as u32,
            gc_tables: c.gc_types,
            asm,
            quiet: c.quiet,
        });
    }
    let mut asm = c.obj.asm.take();
    obj::objc::define(&mut c.obj);
    c.obj.drop_unreferenced();
    let object = if c.obj.coff {
        let cv = opts
            .debug
            .then(|| debug::codeview::write(&c.obj, module, sources))
            .flatten();
        obj::coff::write(&c.obj, cv.as_ref())
    } else if c.obj.elf {
        obj::elf::write(&c.obj)
    } else {
        obj::macho::write(&c.obj)
    };
    if c.obj.src.is_some() {
        asm = Some(asm::source::write(&c.obj, &object));
    }
    Ok(Output {
        object,
        gc_types: c.gc_types.len() as u32,
        gc_tables: c.gc_types,
        asm,
        quiet: c.quiet,
    })
}
