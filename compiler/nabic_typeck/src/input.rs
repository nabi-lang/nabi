//! What the driver tells lowering about the build besides the program.

use crate::lower::{Bodies, KeptLibrary};
use nabic_hir::CfgDropped;

/// Origin of the root `main`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RootMain {
    /// Declared by the program (or none, for a library).
    #[default]
    Declared,
    /// Made from the entry file's top-level statements.
    Implicit,
    /// The `nabic test` / `nabic bench` harness: it reaches every test, so
    /// visibility doesn't apply to its paths.
    Harness,
}

/// A lone script's build (`HirInput::script`).
#[derive(Clone, Debug, Default)]
pub struct Script {
    /// The file `nabi add` edits, as the user named it; `None`: no file
    /// (stdin, a REPL session).
    pub file: Option<String>,
    /// The active environment and the libraries installed in it, which
    /// a script does not see.
    pub env: Option<(String, Vec<String>)>,
}

/// What the driver tells HIR about the build besides the program.
#[derive(Clone, Debug, Default)]
#[allow(clippy::struct_excessive_bools)] // independent switches
pub struct HirInput {
    /// Dependencies' top-level names (`--extern`). Visibility stops at a
    /// package, `pkg::` in a dependency is that dependency, and a path
    /// starting with its name reaches it from anywhere.
    pub packages: Vec<String>,
    /// The dependency that is this package's own library, so
    /// errors say how the executable sees its items.
    pub own_library: Option<String>,
    /// The dependency package this build compiles as a prebuilt library
    /// (`lib<name>.a` + `<name>.nabihir`): every body of it is lowered.
    pub lib: Option<String>,
    /// Packages loaded from NABIHIR: their body-less functions
    /// are declarations of the symbols in their archives.
    pub prebuilt: Vec<String>,
    /// Lower (and so check) every library body, reached or not
    /// (`nabic check --check-library`).
    pub check_library: bool,
    /// Where the root `main` comes from.
    pub root_main: RootMain,
    /// Bits of `usize` / `isize` (the target's pointer width) for
    /// literal ranges and constant evaluation; 0 means 64.
    pub pointer_width: u32,
    /// `extern "system"` is `stdcall`, not `"C"` (32-bit x86 Windows).
    pub stdcall_system: bool,
    /// The deployment target on Apple targets, and whether it is iOS's
    /// (else macOS's): an Objective-C API newer than it is called only
    /// under `objc::available`.
    pub apple: Option<(bool, (u32, u32, u32))>,
    /// Module path of each item by index (missing: root); HIR qualifies
    /// item names with it.
    pub item_modules: Vec<String>,
    /// The modules without the implicit import
    /// (`#![no_implicit_nabi]`).
    pub no_implicit_nabi: Vec<String>,
    /// The modules named like a standard library tier (`a::std`, the
    /// root's `std@`): paths starting with that name in their parent
    /// are theirs.
    pub shadows: Vec<String>,
    /// What lowering depends on besides the program (the target and
    /// cfg, the compiler): part of every key of `bodies`.
    pub fingerprint: u64,
    /// Where lowered bodies are kept for the next build.
    pub bodies: Option<Bodies>,
    /// The use-after-hand-off summaries of the prebuilt libraries'
    /// functions (their NABIHIR): `(function, summary)`.
    pub summaries: Vec<(String, String)>,
    /// The program is a lone script (an anonymous package), not a
    /// package: a `use` of a package it lacks says how a script gets one.
    pub script: Option<Script>,
    /// The library items equal those of an earlier compile in this process
    /// with this key (REPL cells); reuse what collecting them made.
    pub library: Option<u64>,
    /// The stage of `library`, when the driver took it: what collecting
    /// the library's items made is taken from it.
    pub stage: Option<KeptLibrary>,
    /// What `#[cfg]` dropped from library items HIR does not get (a kept
    /// `stage`), for not-found errors.
    pub cfg_dropped: Vec<CfgDropped>,
    /// The rooted paths the package's `pub` meta blocks write, checked
    /// from each block's module.
    pub templates: Vec<TemplateRefs>,
    /// `--crate-type=plugin`.
    pub plugin: bool,
    /// `--hot`: the hot packages (the plugin's own and `--hot-dep`s).
    pub hot: Option<Vec<String>>,
}

/// The rooted paths (`pkg::f`, `std::..`, `dep::..`) one meta block's
/// template writes, and the module that holds the block.
#[derive(Clone, Debug, Default)]
pub struct TemplateRefs {
    pub module: String,
    pub paths: Vec<(Vec<String>, nabic_diagnostic::Span)>,
}
