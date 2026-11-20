//! The code generator: nabi's own backend (`nabic_codegen_native`), at
//! every `-O` level.

use crate::{BuildOptions, CompiledModule};
use nabic_diagnostic::{NabiDiagnostic, SourceMap};
use nabic_mir::Opt;

/// The object of `module`. A target without the backend is an error
/// (`nabic_codegen_native::unsupported`).
pub fn object(
    module: &CompiledModule,
    sources: &SourceMap,
    opts: &BuildOptions,
) -> Result<nabic_codegen_native::Output, NabiDiagnostic> {
    compile(&mir(module, opts), sources, opts)
}

/// The object of `mir` (`mir`'s result).
pub fn compile(
    mir: &nabic_mir::MirModule,
    sources: &SourceMap,
    opts: &BuildOptions,
) -> Result<nabic_codegen_native::Output, NabiDiagnostic> {
    nabic_codegen_native::compile(mir, &options(opts), sources)
}

/// The MIR of `module` the backend compiles: optimized above `-O0`.
///
/// Optimized with what the prebuilt callees' summaries tell
/// (`CompiledModule::facts`), the same with `-g` or without. A REPL cell
/// is not (a later cell may redefine what it would inline).
#[must_use]
pub fn mir<'m>(
    module: &'m CompiledModule,
    opts: &BuildOptions,
) -> std::borrow::Cow<'m, nabic_mir::MirModule> {
    mir_with(module, opts, None)
}

/// `mir`, taking the results of unchanged bodies from `memo`.
pub fn mir_with<'m>(
    module: &'m CompiledModule,
    opts: &BuildOptions,
    memo: Option<&dyn nabic_mir::Memo>,
) -> std::borrow::Cow<'m, nabic_mir::MirModule> {
    let opt = match opts.opt_level.as_str() {
        "1" => Opt::Speed(1),
        "2" => Opt::Speed(2),
        "3" => Opt::Speed(3),
        "s" | "z" => Opt::Size,
        _ => return std::borrow::Cow::Borrowed(&module.mir),
    };
    if opts.repl.is_some() {
        return std::borrow::Cow::Borrowed(&module.mir);
    }
    let mut m = module.mir.clone();
    take_facts(&mut m, &module.facts);
    nabic_diagnostic::time("opt", || nabic_mir::optimize_with(&mut m, opt, memo));
    std::borrow::Cow::Owned(m)
}

/// Set `facts` (`CompiledModule::facts`) on `m`'s declarations.
fn take_facts(
    m: &mut nabic_mir::MirModule,
    facts: &[(nabic_ir_common::FunctionId, nabic_ir_common::Summary)],
) {
    if facts.is_empty() {
        return;
    }
    let at: nabic_core::HashMap<_, _> = facts.iter().copied().collect();
    for f in &mut m.functions {
        if let Some(s) = at.get(&f.id) {
            f.facts.summary = *s;
        }
    }
}

/// The backend's options of a build.
pub(crate) fn options(opts: &BuildOptions) -> nabic_codegen_native::Options {
    nabic_codegen_native::Options {
        opt_level: nabic_codegen_native::OptLevel::parse(&opts.opt_level).unwrap_or_default(),
        overflow_checks: opts.overflow_checks,
        debug: opts.debug,
        repl: opts.repl.clone(),
        fresh_fns: None,
        asm: None,
    }
}

/// `module`'s code as text: the object as assembly source (`--emit=asm`,
/// `source`), else each function's listing (`--emit=listing`).
pub fn asm(
    module: &CompiledModule,
    sources: &SourceMap,
    opts: &BuildOptions,
    source: bool,
) -> Result<String, NabiDiagnostic> {
    let how = if source {
        nabic_codegen_native::Text::Source
    } else {
        nabic_codegen_native::Text::Listing
    };
    if let Some(why) = nabic_codegen_native::unlisted(&module.mir, how) {
        return Err(NabiDiagnostic::error(
            nabic_diagnostic::Code::UNSUPPORTED,
            nabic_diagnostic::Span::DUMMY,
            why,
        ));
    }
    let o = nabic_codegen_native::compile(
        &mir(module, opts),
        &nabic_codegen_native::Options {
            asm: Some(how),
            ..options(opts)
        },
        sources,
    )?;
    Ok(o.asm.unwrap_or_default())
}

/// A REPL cell as an object, compiling only functions the session lacks.
pub fn cell_object(
    module: &CompiledModule,
    sources: &SourceMap,
    opts: &BuildOptions,
) -> Result<(Vec<u8>, nabic_codegen::CodegenOutput), NabiDiagnostic> {
    nabic_codegen_native::cell(&module.mir, &options(opts), sources)
}

/// Codegen for its errors alone, no code written (`nabic check`).
pub fn check(
    module: &CompiledModule,
    sources: &SourceMap,
    opts: &BuildOptions,
) -> Result<(), NabiDiagnostic> {
    nabic_diagnostic::time("codegen check", || {
        nabic_codegen_native::check(&module.mir, &options(opts), sources)
    })
}
