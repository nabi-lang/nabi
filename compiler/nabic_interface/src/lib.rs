//! The compiler as a library: sessions, package discovery, prebuilt
//! dependencies and incremental-build records, for `nabic` and the tools
//! `nabi` runs in-process (REPL, Jupyter, LSP).

mod deps;
mod fresh;
mod package;

pub use deps::DepBuild;
pub use fresh::Fingerprint;
pub use nabic_diagnostic::{
    Code, ErrorKind, Json, NabiDiagnostic, SourceMap, Span, capture_warnings, end_if_output_closed,
    explain, print_rendered, set_report_depth, set_report_style,
};
pub use nabic_driver::units::UnitCounts;
pub use nabic_driver::{
    BuildOptions, CfgEnv, Demangled, EmitKind, ReplCell, ReplFn, SourceFile, Subsystem, TargetSpec,
    TempDir, TestCase, attr_lines_pass, bootstrap_tiers, cache, cache_dir, demangle, feature_of,
    fncache, keep_partial, link_host, nabihir, repl_host, take_partial, target_env,
};
pub use nabic_target::{available as available_targets, cpu as target_cpu};
pub use package::{
    Package, STDIN, code_name, discover, package_root, root_file, source_files, source_files_with,
};

use nabic_driver::{CompiledModule, CompilerConfig, compile_file, compile_source};
use std::path::{Path, PathBuf};

#[derive(Default)]
pub struct Session {
    config: CompilerConfig,
}

impl Session {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a `.nabi` file to the bootstrap operator pre-pass; order only
    /// affects diagnostic order.
    pub fn add_bootstrap_file(&mut self, path: PathBuf) {
        self.config.bootstrap_files.push(path);
    }

    /// A dependency package (`--extern name=root`): its
    /// `lib.nabi` is compiled with this build and mounted at `name`.
    pub fn add_extern(&mut self, name: String, root: PathBuf) {
        self.config.externs.push((name, root));
    }

    /// Package `dependent` (`pkg`: the one being built) depends on extern
    /// `name` (`--extern-dep`); with any, packages see only their own.
    pub fn add_extern_dep(&mut self, dependent: String, name: String) {
        self.config.extern_deps.push((dependent, name));
    }

    /// Extern `name` is the package's own library, which the executable
    /// being built uses.
    pub fn set_own_library(&mut self, name: Option<String>) {
        self.config.own_library = name;
    }

    /// Add a `--cfg` flag (either `"flag"` or `"key=value"`). Consulted
    /// by the `#[cfg(...)]` filter pass before HIR.
    pub fn add_cfg(&mut self, flag: String) {
        self.config.cfg_flags.push(flag);
    }

    /// The active cfg, one `flag` / `key="value"` per line
    /// (`nabic --print cfg`).
    #[must_use]
    pub fn cfg_lines(&self) -> Vec<String> {
        nabic_driver::cfg_env(&self.config).lines()
    }

    /// `nabic test` build: keep `#[test]` functions, generate the runner
    /// `main`, set `cfg(test)`.
    pub const fn set_test(&mut self, on: bool) {
        self.config.test = on;
    }

    /// `nabic bench` build: keep `#[bench]` functions, generate the
    /// timing `main`, set `cfg(test)`.
    pub const fn set_bench(&mut self, on: bool) {
        self.config.bench = on;
    }

    /// Keep each parsed file's AST in `CompileResult::sources` (lint,
    /// doc).
    pub const fn set_keep_ast(&mut self, on: bool) {
        self.config.keep_ast = on;
    }

    /// With `set_keep_ast`: a kept library stage may stand for the
    /// library's files (`CompileResult::library`).
    pub const fn set_library_items(&mut self, on: bool) {
        self.config.library_items = on;
    }

    /// Build the library package as prebuilt dependency `name`
    /// (`archive` then also writes `<name>.nabihir`).
    pub fn set_lib_name(&mut self, name: Option<String>) {
        self.config.lib_name = name;
    }

    /// The input is a lone file outside any package.
    pub const fn set_anonymous(&mut self, on: bool) {
        self.config.anonymous = on;
    }

    /// Compile for `target` (`--target`; the host by default).
    pub fn set_target(&mut self, target: &TargetSpec) {
        self.config.target = nabic_driver::Target::new(target.clone());
    }

    /// Keep the library's parse across compiles (a long-lived session
    /// over a library that does not change: the REPL).
    pub const fn set_cache_library(&mut self, on: bool) {
        self.config.cache_library = on;
    }

    /// Path prefix rules for what the build writes (`--remap-path-prefix`).
    pub fn set_remap(&mut self, rules: Vec<(PathBuf, String)>) {
        self.config.remap = rules;
    }

    /// Read `path` (absolute) as `text` instead of from the disk.
    pub fn add_buffer(&mut self, path: PathBuf, text: String) {
        self.config.buffers.push((path, text));
    }

    /// `--crate-type=plugin`: tags and type ids come from the loader;
    /// `bundle`: the dependencies it links in.
    pub fn set_plugin(&mut self, on: bool, bundle: &[String]) {
        self.config.plugin = on;
        bundle.clone_into(&mut self.config.bundle);
    }

    /// `--hot`: a hot-reload plugin (`set_plugin` too), checked against
    /// the `.layout` of `base`; `deps` are hot packages as well.
    pub fn set_hot(&mut self, base: Option<PathBuf>, deps: &[String]) {
        self.config.hot = Some(nabic_driver::hot::Hot {
            base,
            deps: deps.to_vec(),
        });
    }

    /// Lower (and so check) every library body, reached or not.
    pub const fn set_check_library(&mut self, on: bool) {
        self.config.check_library = on;
    }

    /// Keep parses and lowered bodies in `dir` for the next compile of
    /// the package.
    pub fn set_incremental(&mut self, dir: Option<PathBuf>) {
        self.config.incremental = dir;
    }

    /// The operator tokens the files of the package with root files
    /// `roots` lex: its own registrations and its externs' exported ones.
    #[must_use]
    pub fn operator_tokens(&self, roots: &[PathBuf]) -> Vec<String> {
        nabic_driver::operator_tokens(roots, &self.config)
    }

    pub fn compile_file(&self, path: &Path) -> CompileResult {
        let mut map = SourceMap::new();
        let r = compile_file(path, &self.config, &mut map);
        Self::result(r, map)
    }

    /// `compile_file` with the root's text given (stdin, an editor, the
    /// REPL); `path` still locates `mod foo;` files, `map` holds known views.
    pub fn compile_source(&self, source: &str, path: &Path, mut map: SourceMap) -> CompileResult {
        let r = compile_source(source, path, &self.config, &mut map);
        Self::result(r, map)
    }

    fn result(
        r: Result<nabic_driver::CompiledProject, Vec<NabiDiagnostic>>,
        source_map: SourceMap,
    ) -> CompileResult {
        match r {
            Ok(p) => CompileResult {
                modules: p.modules,
                errors: Vec::new(),
                warnings: p.warnings,
                tests: p.tests,
                sources: p.sources,
                library: p.library,
                files: p.files,
                source_map,
                reuse: p.reuse,
            },
            Err(errors) => CompileResult {
                modules: Vec::new(),
                errors,
                warnings: Vec::new(),
                tests: Vec::new(),
                sources: Vec::new(),
                library: None,
                files: Vec::new(),
                source_map,
                reuse: None,
            },
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct CompileResult {
    pub modules: Vec<CompiledModule>,
    pub errors: Vec<NabiDiagnostic>,
    pub warnings: Vec<NabiDiagnostic>,
    /// Test names of a `set_test` build (bench names of `set_bench`).
    pub tests: Vec<nabic_driver::TestCase>,
    /// Parsed files of a `set_keep_ast` build (user package first).
    pub sources: Vec<SourceFile>,
    /// The library's items when a library stage stood for its files
    /// (`set_library_items`).
    pub library: Option<&'static nabic_ast::Program>,
    /// The source files read from disk.
    pub files: Vec<PathBuf>,
    /// The compile's source files by span id: what its diagnostics and
    /// codegen's locations name.
    pub source_map: SourceMap,
    /// What was taken from the incremental directory.
    pub reuse: Option<nabic_driver::Reuse>,
}

impl CompileResult {
    #[must_use]
    pub fn has_errors(&self) -> bool {
        !self.errors.is_empty()
    }

    pub fn report_errors(&self) {
        for d in &self.errors {
            self.source_map.report(d);
        }
    }

    /// Print a diagnostic of this compile (`SourceMap::report`).
    pub fn report(&self, d: &NabiDiagnostic) {
        self.source_map.report(d);
    }

    /// A diagnostic of this compile as `report` prints it.
    #[must_use]
    pub fn render(&self, d: &NabiDiagnostic) -> String {
        self.source_map.render(d)
    }

    fn module(&self) -> Result<&CompiledModule, NabiDiagnostic> {
        self.modules.first().ok_or_else(|| {
            NabiDiagnostic::error(
                Code::INTERNAL,
                nabic_diagnostic::Span::DUMMY,
                "no module produced",
            )
        })
    }

    /// One intermediate artifact (`--emit=<kind>`).
    pub fn emit(&self, kind: EmitKind, opts: &BuildOptions) -> Result<Vec<u8>, NabiDiagnostic> {
        if kind == EmitKind::Expanded {
            // The user package's files (a `set_keep_ast` build).
            let text: Vec<&str> = self
                .sources
                .iter()
                .filter(|s| s.package.is_empty())
                .flat_map(|s| &s.expanded)
                .map(String::as_str)
                .collect();
            return Ok(text.join("\n").into_bytes());
        }
        if kind == EmitKind::Ast && !self.sources.is_empty() {
            return Ok(self.ast_source().into_bytes());
        }
        nabic_driver::emit(self.module()?, &self.source_map, kind, opts)
    }

    /// `-vv`: the source files read, then the generic instances
    /// made, one progress line each.
    #[must_use]
    pub fn trace(&self) -> String {
        let files = self
            .source_map
            .files()
            .iter()
            .map(|(p, _)| ("Reading", p.clone()));
        let instances = self
            .modules
            .iter()
            .flat_map(|m| &m.hir.functions)
            .filter_map(nabic_driver::instance)
            .map(|sig| ("Specialized", sig));
        let depth = nabic_core::term::depth(nabic_core::term::Stream::Stderr);
        let mut out = String::new();
        for (verb, what) in files.chain(instances) {
            out.push_str(&nabic_core::term::status_in(depth, verb, &what));
            out.push('\n');
        }
        out
    }

    /// Codegen for its errors alone (`check`), without the toolchain.
    pub fn check_codegen(&self, opts: &BuildOptions) -> Result<(), NabiDiagnostic> {
        nabic_driver::check_codegen(self.module()?, &self.source_map, opts)
    }

    /// The package's files after meta expansion as source, each under a
    /// `// <file> (<module>)` line (`--emit=source`; needs `set_keep_ast`).
    #[must_use]
    pub fn ast_source(&self) -> String {
        let mut out = String::new();
        for f in self.sources.iter().filter(|f| f.package.is_empty()) {
            let module = if f.module.is_empty() {
                "pkg".to_owned()
            } else {
                f.module.join("::")
            };
            out.push_str(&format!("// {} ({module})\n", f.path.display()));
            out.push_str(&nabic_ast::print::print_file(&f.attrs, &f.items, &f.stmts));
            out.push('\n');
        }
        out
    }

    /// Lower MIR again after editing `modules[0].hir`.
    pub fn relower(&mut self) {
        if let Some(m) = self.modules.first_mut() {
            nabic_driver::relower(m);
        }
    }

    /// Build an executable at `out`; by codegen units with
    /// `opts.incremental`, and then how many it compiled.
    pub fn link(
        &self,
        opts: &BuildOptions,
        out: &Path,
    ) -> Result<Option<UnitCounts>, NabiDiagnostic> {
        nabic_driver::link_executable(self.module()?, &self.source_map, opts, out)
    }

    /// A REPL cell as an object for the host, with what codegen says
    /// about it (GC tables, new symbols).
    pub fn cell_code(
        &self,
        opts: &BuildOptions,
    ) -> Result<(Vec<u8>, nabic_driver::CodegenOutput), NabiDiagnostic> {
        nabic_driver::codegen::cell_object(self.module()?, &self.source_map, opts)
    }

    /// Link the C-ABI shared library `out` from library `archive`, built
    /// from this result by [`Self::archive`].
    pub fn shared(
        &self,
        opts: &BuildOptions,
        archive: &Path,
        out: &Path,
    ) -> Result<(), NabiDiagnostic> {
        nabic_driver::link_shared(self.module()?, opts, archive, out)
    }

    /// The self-contained static library `out` from library `archive`,
    /// built from this result by [`Self::archive`]; the system libraries
    /// and frameworks (`true`) its users link.
    pub fn staticlib(
        &self,
        opts: &BuildOptions,
        archive: &Path,
        out: &Path,
    ) -> Result<Vec<(String, bool)>, NabiDiagnostic> {
        nabic_driver::build_staticlib(self.module()?, opts, archive, out)
    }

    /// Link plugin `out` from library `archive`, built from this result
    /// by [`Self::archive`].
    pub fn plugin(
        &self,
        opts: &BuildOptions,
        archive: &Path,
        out: &Path,
    ) -> Result<(), NabiDiagnostic> {
        nabic_driver::link_plugin(self.module()?, opts, archive, out)
    }

    /// A hot build's `.layout` text (`--hot`).
    #[must_use]
    pub fn hot_layout(&self) -> Option<String> {
        let m = self.module().ok()?;
        m.mir.plugin.hot.as_ref()?;
        Some(nabic_driver::hot::dump(&m.mir))
    }

    /// Build a library archive at `out`; with
    /// `opts.incremental` by codegen units, returning how many compiled.
    pub fn archive(
        &self,
        opts: &BuildOptions,
        out: &Path,
    ) -> Result<Option<UnitCounts>, NabiDiagnostic> {
        nabic_driver::build_archive(self.module()?, &self.source_map, opts, out)
    }
}

/// Where a hot build of `out` writes its `.layout`.
#[must_use]
pub fn hot_layout_path(out: &Path) -> PathBuf {
    nabic_driver::hot::layout_path(out)
}

/// The standard library and runtime of this `nabic`.
///
/// Under its sysroot (`lib/nabi/{core,std,bare}/src/`, `lib/libnabir.a`,
/// `lib\nabir.lib`), else in a repository build the `library/` and the
/// runtime beside `nabic`. `NABI_HOME` plays no part; each `Err` lists where
/// it looked.
pub struct Sysroot {
    /// The library roots to preload (`core`, then `std` / `bare`).
    pub libs: Result<Vec<PathBuf>, String>,
    pub nabir: Result<PathBuf, String>,
    /// `lib/nabi/targets/<target>/`: what `--target` links (its runtime;
    /// Linux: C runtime objects and libc). In the repository:
    /// `target/lib/nabi/targets/` (`targets/build.sh`).
    pub targets: PathBuf,
    /// `lib/nabi/lldb/nabi.py`: lldb formatters for `-g` programs.
    pub lldb: Result<PathBuf, String>,
    /// `lib/nabi/gdb/nabi.py`: gdb pretty-printers.
    pub gdb: Result<PathBuf, String>,
    /// `lib/nabi/natvis/nabi.natvis`: Visual Studio and `WinDbg` visualizers.
    pub natvis: Result<PathBuf, String>,
}

/// The sysroot's library directory.
pub const SYSROOT_LIBS: &str = "lib/nabi";
/// The repository's `library/` (a dev build); none in a release
/// toolchain (`NABI_DIST`), which keeps the build machine's paths out.
const DEV_LIBS: Option<&str> = match option_env!("NABI_DIST") {
    Some(_) => None,
    None => Some(concat!(env!("CARGO_MANIFEST_DIR"), "/../../library")),
};

#[must_use]
pub fn sysroot() -> Sysroot {
    let exe = nabic_core::current_exe().unwrap_or_default();
    let bin = exe.parent().unwrap_or_else(|| Path::new("."));
    let root = bin.parent().unwrap_or(bin);
    let dev = DEV_LIBS.map(|d| nabic_core::real_path(d).unwrap_or_else(|_| PathBuf::from(d)));
    let lib_dirs: Vec<PathBuf> = std::iter::once(root.join(SYSROOT_LIBS))
        .chain(dev)
        .collect();
    // " nor <dev>/<file> (a build in the repository)" when there is one.
    let nor = |file: &str| {
        lib_dirs.get(1).map_or_else(String::new, |d| {
            format!(
                " nor {} (a build in the repository)",
                d.join(file).display()
            )
        })
    };
    let libs = lib_dirs
        .iter()
        .find(|d| d.join("core/src/lib.nabi").is_file())
        .map(|d| bootstrap_roots(d))
        .ok_or_else(|| {
            format!(
                "the standard library is missing: no {} (the sysroot of {}){}; reinstall the \
                 toolchain, or pass --bootstrap <core/src/lib.nabi>",
                lib_dirs[0].join("core/src/lib.nabi").display(),
                exe.display(),
                nor("core/src/lib.nabi"),
            )
        });
    let shipped = |file: &str, what: &str| {
        lib_dirs
            .iter()
            .map(|d| d.join(file))
            .find(|p| p.is_file())
            .ok_or_else(|| {
                format!(
                    "the {what} are missing: no {}{}",
                    lib_dirs[0].join(file).display(),
                    nor(file),
                )
            })
    };
    let lldb = shipped("lldb/nabi.py", "lldb formatters");
    let gdb = shipped("gdb/nabi.py", "gdb pretty-printers");
    let natvis = shipped("natvis/nabi.natvis", "natvis visualizers");
    let rt_lib = TargetSpec::host().runtime_lib();
    let rt = [root.join("lib").join(&rt_lib), bin.join(&rt_lib)];
    let nabir = rt.iter().find(|p| p.is_file()).cloned().ok_or_else(|| {
        format!(
            "the runtime library is missing: no {} (the sysroot of {}) nor {} (a build in the \
             repository: `cargo build -p nabir`); reinstall the toolchain, or pass --nabir <lib>",
            rt[0].display(),
            exe.display(),
            rt[1].display(),
        )
    });
    Sysroot {
        libs,
        nabir,
        targets: root.join("lib/nabi/targets"),
        lldb,
        gdb,
        natvis,
    }
}

/// Library roots to preload: `core/src/lib.nabi`, then `std` / `bare` if
/// present (no implicit glob: user code names them).
fn bootstrap_roots(libs: &Path) -> Vec<PathBuf> {
    let mut out = vec![libs.join("core/src/lib.nabi")];
    out.extend(
        ["std", "bare"]
            .iter()
            .map(|tier| libs.join(tier).join("src/lib.nabi"))
            .filter(|p| p.is_file()),
    );
    out
}
