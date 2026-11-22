//! Compiler driver. Threads source through lexer, parser,
//! HIR lowering (name resolution + type checks), and MIR lowering.

pub mod cache;
pub mod codegen;
pub mod fncache;
pub mod hot;
pub mod nabihir;
pub mod units;

mod attrs;
mod backend;
mod cfg;
mod coro;
mod entry;
mod harness;
mod header;
mod incr;
mod objc;
mod package;
mod parse;
mod pipeline;
mod tables;
mod tree;
mod windows;
mod wit;

pub use crate::harness::TestCase;
pub use crate::package::*;
pub use crate::parse::operator_tokens;
#[allow(clippy::wildcard_imports)]
pub(crate) use crate::parse::*;
pub use crate::pipeline::*;
#[allow(clippy::wildcard_imports)]
pub(crate) use crate::tables::*;
#[allow(clippy::wildcard_imports)]
pub(crate) use crate::tree::*;
pub use backend::{
    BuildOptions, EmitKind, TempDir, build_archive, build_staticlib, cache_dir, check_codegen,
    emit, link_executable, link_host, link_object, link_plugin, link_shared,
};
pub use cfg::{CfgEnv, attr_lines_pass, feature_of, target_env};
pub use incr::{LIBRARY_STAGES, Reuse};
pub use nabic_codegen::{CodegenOutput, Demangled, ReplCell, ReplFn, demangle};
pub use nabic_hir::instance;
pub use nabic_mir::{Target, TargetSpec};
pub use windows::{Subsystem, link_args as windows_link_args};

use nabic_ast::{AttrArg, AttrBody, ImplItem, Item, Literal, TraitItem};
use nabic_diagnostic::{Code, NabiDiagnostic, SourceMap};
use nabic_hir::HirModule;
use nabic_ir_common::{FunctionId, Linkage};
use nabic_lexer::{Lexer, TokenKind, TokenKind as K};
use nabic_mir::MirModule;
use nabic_parser::{
    ExtOps, KeywordDef, KeywordTable, LitRule, LiteralDef, LiteralTable, MetaDef, MetaTable,
    PatKind, PrecTable, apply_keywords, check_keyword, collect_keywords, collect_literals,
    collect_meta_blocks, collect_prec_table, parse_pattern, parse_program, parse_program_meta,
};
use std::path::{Path, PathBuf};

/// Whether the REPL's session host runs here: it loads Mach-O / ELF /
/// COFF objects of arm64 / `x86_64`.
#[must_use]
pub fn repl_host() -> bool {
    nabic_mir::TargetSpec::host().repl
}

// Independent switches, one per session setter.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, Default)]
pub struct CompilerConfig {
    /// Files containing operator declarations to pre-load before parsing
    /// the user input. Populates the `PrecTable`.
    pub bootstrap_files: Vec<PathBuf>,
    /// `--cfg` flags: bare (`"test"`) or `key=value` (`"target_os=macos"`).
    pub cfg_flags: Vec<String>,
    /// Test build (`nabic test`): `#[test]` functions are kept and a
    /// generated `main` runs the one named by `argv[1]`.
    pub test: bool,
    /// Bench build (`nabic bench`): a test build whose generated `main`
    /// times the `#[bench]` function named by `argv[1]`.
    pub bench: bool,
    /// Keep every parsed file's AST in `CompiledProject::sources`
    /// (`nabi lint` / `nabi doc`).
    pub keep_ast: bool,
    /// With `keep_ast`: a library stage may stand for the library's files,
    /// items in `CompiledProject::library` (lints and tools read items;
    /// `nabi doc` reads files).
    pub library_items: bool,
    /// The input is a lone file outside any package: it is
    /// the anonymous package's `main.nabi` and cannot have `mod foo;`.
    pub anonymous: bool,
    /// Source files read from these texts instead of the disk (an
    /// editor's unsaved buffers), by absolute path.
    pub buffers: Vec<(PathBuf, String)>,
    /// Dependencies (`--extern name=path`), each mounted at
    /// top-level `name`: a `lib.nabi` compiles with the package, a
    /// `<name>.nabihir` is prebuilt and links its `lib<name>.a`.
    /// A name whose path an earlier one has is an alias of that one.
    pub externs: Vec<(String, PathBuf)>,
    /// `--extern-dep <package>=<name>`: `package` (`pkg`: the one being
    /// built) depends on extern `name`. With any, a package sees only the
    /// externs it depends on (`package::scopes`); without, every extern.
    pub extern_deps: Vec<(String, String)>,
    /// The extern that is the package's own library (`src/lib.nabi` of
    /// the `src/main.nabi` being built).
    pub own_library: Option<String>,
    /// Build as prebuilt dependency `name`: mounted as its users see it,
    /// every body compiled, NABIHIR in `CompiledModule::interface`.
    pub lib_name: Option<String>,
    /// Keep the library's parse for the next build with the same library
    /// and lexing-relevant declarations (`#[infix]`, meta blocks,
    /// keywords): a REPL session.
    pub cache_library: bool,
    /// The target (`--target`): `#[cfg]`, layout and code follow it.
    pub target: nabic_mir::Target,
    /// Lower every library body, not only the reachable ones.
    pub check_library: bool,
    /// The package's incremental directory: parses and
    /// lowered bodies of the last build are kept there for the next.
    pub incremental: Option<PathBuf>,
    /// Path prefixes shown otherwise in what the build writes (panic
    /// locations, debug info, NABIHIR), after the standard library's
    /// own (`remap_rules`).
    pub remap: Vec<(PathBuf, String)>,
    /// `--crate-type=plugin`: the library loads into a Nabi program.
    pub plugin: bool,
    /// `--bundle <dep>`: dependencies a plugin links in.
    pub bundle: Vec<String>,
    /// `--hot`: a hot-reload plugin.
    pub hot: Option<hot::Hot>,
}

/// The environment `nabi` runs a script from (`__NABI_ENV`) and its
/// installed libraries (`__NABI_ENV_LIBS`), which the script doesn't see.
fn script_env() -> Option<(String, Vec<String>)> {
    let var = |n| nabic_core::handoff_var(n).and_then(|v| v.into_string().ok());
    let env = var("NABI_ENV").filter(|e| !e.is_empty())?;
    let libs = var("NABI_ENV_LIBS").unwrap_or_default();
    let libs = libs.split(',').filter(|l| !l.is_empty()).map(str::to_owned);
    Some((env, libs.collect()))
}

/// The build's cfg: the target's keys, `--cfg` and `test`.
#[must_use]
pub fn cfg_env(config: &CompilerConfig) -> CfgEnv {
    package_cfg(config, None)
}

/// The build's cfg for one package, `feature` keys replaced by `features`
/// (features are per package).
fn package_cfg(config: &CompilerConfig, features: Option<&[String]>) -> CfgEnv {
    let mut env = CfgEnv::new();
    cfg::add_target(&mut env, &config.target);
    for f in &config.cfg_flags {
        if features.is_none() || cfg::feature_of(f).is_none() {
            env.add(f);
        }
    }
    for f in features.unwrap_or_default() {
        env.add_pair("feature", f);
    }
    if config.test || config.bench {
        env.add("test");
    }
    env
}

thread_local! {
    /// Files served instead of the disk: the interface sources of the
    /// prebuilt dependencies (`nabihir::Nabihir::files`) at the paths they
    /// were built from.
    static OVERLAY: std::cell::RefCell<nabic_core::HashMap<PathBuf, String>> =
        std::cell::RefCell::new(nabic_core::HashMap::default());
    /// The source files this compile read from disk.
    static READ: std::cell::RefCell<Vec<PathBuf>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn read_source(path: &Path) -> std::io::Result<String> {
    if let Some(text) = OVERLAY.with(|o| o.borrow().get(path).cloned()) {
        return Ok(text);
    }
    READ.with(|r| r.borrow_mut().push(path.to_path_buf()));
    std::fs::read_to_string(path)
}

fn source_exists(path: &Path) -> bool {
    OVERLAY.with(|o| o.borrow().contains_key(path)) || path.exists()
}

fn source_dir_exists(dir: &Path) -> bool {
    OVERLAY.with(|o| o.borrow().keys().any(|p| p.starts_with(dir) && p != dir)) || dir.is_dir()
}

/// A parsed file as tools see it: after `#[cfg]`, before modules are
/// flattened.
#[derive(Clone, Debug)]
pub struct SourceFile {
    pub path: PathBuf,
    /// Its `Span::file` id.
    pub file_id: u32,
    /// `""` for the user package, else the library root's mount path
    /// (`core`, a dependency's name).
    pub package: String,
    /// Module path (with the package's mount prefix).
    pub module: Vec<String>,
    /// Inner (`#![..]`) attributes.
    pub attrs: Vec<nabic_ast::Attribute>,
    pub items: Vec<nabic_ast::Item>,
    /// Top-level statements (the entry file's implicit `main`).
    pub stmts: Vec<nabic_ast::Stmt>,
    /// Its meta expansions as source text (`--emit=expansions`).
    pub expanded: Vec<String>,
    /// The names its meta blocks' templates write (uses for `dead_code`).
    pub template_names: Vec<String>,
}

/// A user module named like a dependency would merge with it.
/// Empty modules can't clash.
fn check_dependency_names(
    items: &[ModItem],
    uses: &[(String, nabic_core::Visibility, nabic_ast::UseTree)],
    externs: &[(String, PathBuf)],
    diags: &mut Vec<NabiDiagnostic>,
) {
    let item_modules = items.iter().filter_map(|(module, item)| match item {
        Item::Fn(_)
        | Item::Struct(_)
        | Item::Enum(_)
        | Item::Union(_)
        | Item::Trait(_)
        | Item::Const(_)
        | Item::Static(_)
        | Item::TypeAlias(_)
            if !module.is_empty() =>
        {
            Some(module.as_str())
        }
        _ => None,
    });
    let modules: Vec<&str> = item_modules
        .chain(uses.iter().map(|(m, _, _)| m.as_str()))
        .collect();
    for (dep, _) in externs {
        let under = format!("{dep}::");
        if modules.iter().any(|m| m == dep || m.starts_with(&under)) {
            diags.push(NabiDiagnostic::error(
                Code::IMPORT_CONFLICT,
                nabic_diagnostic::Span::DUMMY,
                format!(
                    "module `{dep}` has the name of the dependency `{dep}` - dependency names \
                     are top-level paths; rename the module"
                ),
            ));
        }
    }
}

#[derive(Clone, Debug)]
pub struct CompiledModule {
    pub name: String,
    pub path: PathBuf,
    pub hir: HirModule,
    pub mir: MirModule,
    /// Prebuilt dependencies' archives, in link order.
    pub archives: Vec<PathBuf>,
    /// A `CompilerConfig::lib_name` build: its NABIHIR, but the GC type
    /// count and the summaries (known after codegen).
    pub interface: Option<nabihir::Nabihir>,
    /// The summaries of the prebuilt functions `mir` declares, from their
    /// NABIHIRs: what an optimized build takes on (`codegen::mir`).
    pub facts: Vec<(FunctionId, nabic_ir_common::Summary)>,
}

#[derive(Clone, Debug, Default)]
pub struct CompiledProject {
    pub modules: Vec<CompiledModule>,
    pub warnings: Vec<NabiDiagnostic>,
    /// `#[test]` functions of a test build (`#[bench]` functions of a
    /// bench build), `::`-qualified.
    pub tests: Vec<TestCase>,
    /// Parsed files, when `CompilerConfig::keep_ast`.
    pub sources: Vec<SourceFile>,
    /// The library's items when a library stage stood for its files
    /// (`CompilerConfig::library_items`).
    pub library: Option<&'static nabic_ast::Program>,
    /// The source files read from disk (`#[path]` ones outside the
    /// package too).
    pub files: Vec<PathBuf>,
    /// What was taken from the incremental directory.
    pub reuse: Option<Reuse>,
}

#[cfg(test)]
mod tests {
    /// The prelude's names (normative): modules, types and
    /// traits share a namespace, functions have their own.
    const PRELUDE_TYPES: &[&str] = &[
        "array",
        "c_double",
        "c_float",
        "c_int",
        "c_intptr_t",
        "c_long",
        "c_longlong",
        "c_ptrdiff_t",
        "c_schar",
        "c_short",
        "c_size_t",
        "c_ssize_t",
        "c_uchar",
        "c_uint",
        "c_uintptr_t",
        "c_ulong",
        "c_ulonglong",
        "c_ushort",
        "c_void",
        "c_wchar",
        "bench",
        "channel",
        "cmp",
        "collections",
        "convert",
        "crypto",
        "encoding",
        "error",
        "fmt",
        "future",
        "hint",
        "index",
        "io",
        "iter",
        "marker",
        "num",
        "ops",
        "panic",
        "par",
        "print",
        "range",
        "reflect",
        "str",
        "stream",
        "sync",
        "task",
        "time",
        "tuple",
        "List",
        "Dict",
        "Set",
        "Deque",
        "BinaryHeap",
        "Slice",
        "SliceMut",
        "StrBuf",
        "FrozenList",
        "FrozenDict",
        "FrozenSet",
        "Range",
        "RangeInclusive",
        "Int",
        "Float",
        "Result",
        "Ordering",
        "Duration",
        "Instant",
        "Mutex",
        "RwLock",
        "Atomic",
        "Cell",
        "Sender",
        "Receiver",
        "Task",
        "Formatter",
        "FmtError",
        "ParseError",
        "ValueError",
        "OverflowError",
        "ConversionError",
        "TypeError",
        "StackOverflowError",
        "OutOfMemoryError",
        "InternalError",
        "Clone",
        "Copy",
        "Drop",
        "Default",
        "Send",
        "Sync",
        "PartialEq",
        "Eq",
        "PartialOrd",
        "Ord",
        "Hash",
        "Debug",
        "Display",
        "RichDisplay",
        "Error",
        "Iterator",
        "IntoIterator",
        "FromIterator",
        "TryIterator",
        "Future",
        "IntoFuture",
        "Read",
        "Write",
        "AsyncRead",
        "AsyncWrite",
        "From",
        "Into",
        "TryFrom",
        "TryInto",
        "FromStr",
        "Inspect",
    ];
    /// Added on unix targets, then on Linux: the OS Ext traits.
    const PRELUDE_UNIX: &[&str] = &["PermissionsExt", "CommandExt", "ExitStatusExt"];
    const PRELUDE_LINUX: &[&str] = &["ChildExt", "TcpStreamExt"];
    const PRELUDE_FNS: &[&str] = &[
        "channel",
        "channel_bounded",
        "print",
        "println",
        "eprint",
        "eprintln",
        "dbg",
        "display",
        "panic",
        "assert",
        "assert_eq",
        "assert_ne",
        "unreachable",
    ];

    /// Each name `core::prelude` brings in, per target: once in its
    /// namespace, and the set is the normative one.
    #[test]
    fn prelude_names_unique() {
        fn leaves(t: &nabic_ast::UseTree, out: &mut Vec<(String, String)>) {
            let path = |t: &nabic_ast::UseTree| {
                t.prefix
                    .segments
                    .iter()
                    .map(|s| s.name.clone())
                    .collect::<Vec<_>>()
            };
            match &t.kind {
                nabic_ast::UseTreeKind::Simple(alias) => {
                    let p = path(t);
                    let leaf = alias
                        .clone()
                        .or_else(|| p.last().cloned())
                        .unwrap_or_default();
                    out.push((leaf, p.join("::")));
                }
                nabic_ast::UseTreeKind::Group(ts) => {
                    for c in ts {
                        let mut sub = Vec::new();
                        leaves(c, &mut sub);
                        let base = path(t).join("::");
                        out.extend(sub.into_iter().map(|(l, p)| (l, format!("{base}::{p}"))));
                    }
                }
                _ => panic!("the prelude names each item: no globs"),
            }
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../library/core/src");
        let source = std::fs::read_to_string(root.join("prelude.nabi")).unwrap();
        let lib = std::fs::read_to_string(root.join("lib.nabi")).unwrap();
        let is_module = |p: &str| {
            p.strip_prefix("core::")
                .is_some_and(|m| !m.contains("::") && lib.contains(&format!("pub mod {m};")))
        };
        let (tokens, lex) = nabic_lexer::Lexer::new(&source, "prelude.nabi".to_owned()).lex_all();
        let (program, parse) = super::parse_program(&tokens, "prelude.nabi");
        assert!(lex.is_empty() && parse.is_empty(), "{lex:?} {parse:?}");
        let sorted = |v: &[&str]| {
            let mut v: Vec<String> = v.iter().map(|s| (*s).to_owned()).collect();
            v.sort();
            v
        };
        for triple in nabic_target::BUILTIN {
            let mut env = super::CfgEnv::new();
            super::cfg::add_target(&mut env, &super::Target::builtin(triple));
            let items = super::cfg::filter_items(program.items.clone(), &env);
            let mut names = Vec::new();
            for item in &items {
                match item {
                    nabic_ast::Item::Use(u) => {
                        assert_eq!(
                            u.vis,
                            nabic_core::Visibility::Pub,
                            "{triple}: a `use` that is not `pub`"
                        );
                        leaves(&u.tree, &mut names);
                    }
                    _ => panic!("{triple}: the prelude holds `pub use`s only"),
                }
            }
            let (mut fns, mut types): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
            for (leaf, path) in names {
                let ns = if !is_module(&path) && PRELUDE_FNS.contains(&leaf.as_str()) {
                    &mut fns
                } else {
                    &mut types
                };
                assert!(
                    !ns.contains(&leaf),
                    "{triple}: `{leaf}` twice in the prelude"
                );
                ns.push(leaf);
            }
            types.sort();
            fns.sort();
            let cfg = env.lines();
            let mut want = PRELUDE_TYPES.to_vec();
            if cfg.iter().any(|l| l == "target_family=\"unix\"") {
                want.extend(PRELUDE_UNIX);
            }
            // Android's kernel is Linux.
            if cfg
                .iter()
                .any(|l| l == "target_os=\"linux\"" || l == "target_os=\"android\"")
            {
                want.extend(PRELUDE_LINUX);
            }
            assert_eq!(types, sorted(&want), "{triple}: modules, types, traits");
            assert_eq!(fns, sorted(PRELUDE_FNS), "{triple}: functions");
        }
    }

    #[test]
    fn package_hash_known_answer() {
        // python3: sha256(b"lib.nabi\0pub mod m;\n\0m.nabi\0fn f() {}\n\0")
        let mut files = vec![
            ("m.nabi".to_owned(), "fn f() {}\n"),
            ("lib.nabi".to_owned(), "pub mod m;\n"),
        ];
        assert_eq!(super::hash_package(&mut files), "12436437");
    }

    /// Compile `src` as an anonymous program for `triple` with the core
    /// library.
    fn compile(src: &str, triple: &str) -> super::CompiledModule {
        // One directory per call: the tests run in parallel, and a
        // shared one is removed under another test's feet.
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let core = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../library/core/src/lib.nabi");
        let config = super::CompilerConfig {
            bootstrap_files: vec![core],
            anonymous: true,
            target: super::Target::builtin(triple),
            ..super::CompilerConfig::default()
        };
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = nabic_core::tmp::test_dir(&format!("driver-main{n}")).join("main.nabi");
        let mut files = super::SourceMap::new();
        let p = super::compile_source(src, &path, &config, &mut files).unwrap_or_else(|d| {
            panic!(
                "{}",
                d.iter()
                    .map(|d| files.line(d))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        });
        p.modules.into_iter().next().expect("one module")
    }

    #[test]
    fn cfg_follows_the_target() {
        let src = "#[cfg(target_pointer_width = \"32\")]\nfn narrow() {}\n\
                   #[cfg(target_os = \"windows\")]\nfn win() {}\nfn main() {}\n";
        let has = |m: &super::CompiledModule, n: &str| m.hir.functions.iter().any(|f| f.name == n);
        let i686 = compile(src, "i686-pc-windows-msvc");
        assert!(has(&i686, "narrow") && has(&i686, "win"));
        let x64 = compile(src, "x86_64-unknown-linux-gnu");
        assert!(!has(&x64, "narrow") && !has(&x64, "win"));
    }

    /// `usize` literals and constants are checked at the target's width.
    #[test]
    fn usize_range_follows_the_pointer_width() {
        let src = "const BIG: usize = 4294967295 + 1;\nfn main() { let n = BIG; }\n";
        let core = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../library/core/src/lib.nabi");
        let errs = |triple: &str| {
            let config = super::CompilerConfig {
                bootstrap_files: vec![core.clone()],
                anonymous: true,
                target: super::Target::builtin(triple),
                ..super::CompilerConfig::default()
            };
            let path = nabic_core::tmp::test_dir("driver-usize").join("main.nabi");
            let mut files = super::SourceMap::new();
            super::compile_source(src, &path, &config, &mut files)
                .err()
                .map(|d| d.iter().map(|d| files.line(d)).collect::<String>())
        };
        assert!(errs("x86_64-unknown-linux-gnu").is_none());
        let e = errs("i686-unknown-linux-gnu").expect("an overflow on 32-bit");
        assert!(e.contains("overflow"), "{e}");
        assert!(
            compile(
                "fn main() { let n: usize = 4294967295; }\n",
                "armv7-unknown-linux-gnueabihf"
            )
            .hir
            .functions
            .iter()
            .any(|f| f.name == "main")
        );
    }

    #[test]
    fn any_payload_boxing_follows_the_pointer_width() {
        // An `i64` fits the payload word on a 64-bit target, not on a
        // 32-bit one: there the tag carries `ANY_BOXED`.
        let src = "fn main() { let v: any = 5i64; }\n";
        let boxed = |m: &super::CompiledModule| {
            m.mir
                .functions
                .iter()
                .flat_map(|f| &f.blocks)
                .flat_map(|b| &b.stmts)
                .any(|s| {
                    matches!(s, nabic_mir::Statement::Assign {
                    rvalue: nabic_mir::RValue::StructInit { fields, .. }, ..
                } if matches!(fields.first(), Some(nabic_mir::Operand::Const(
                    nabic_ir_common::ConstVal::Int { value, .. })) if *value < 0))
                })
        };
        assert!(boxed(&compile(src, "i686-unknown-linux-gnu")));
        assert!(!boxed(&compile(src, "x86_64-unknown-linux-gnu")));
    }

    /// Builds of two targets in one process each keep the library stage
    /// they took, whichever the other keeps meanwhile.
    #[test]
    fn concurrent_targets_keep_their_library_stage() {
        let src = "#[cfg(target_pointer_width = \"32\")]\nfn narrow() {}\nfn main() {}\n";
        let has = |m: &super::CompiledModule| m.hir.functions.iter().any(|f| f.name == "narrow");
        let runs = |t| move || (0..4).map(|_| has(&compile(src, t))).collect::<Vec<_>>();
        std::thread::scope(|s| {
            let narrow = s.spawn(runs("i686-unknown-linux-gnu"));
            let wide = s.spawn(runs("x86_64-unknown-linux-gnu"));
            assert_eq!(narrow.join().unwrap(), [true; 4]);
            assert_eq!(wide.join().unwrap(), [false; 4]);
        });
    }
}
