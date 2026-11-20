//! The back half of the pipeline, in process: native code generation
//! (`codegen`) and the link, by nabi's own linker (`nabild`), then for
//! `wasm32-wasip2` the componentizer (`nabic_wit::component`). `NABI_CC`
//! links instead when set.

use crate::CompiledModule;
use nabic_core::tmp::{TempEntry, private_dir};
use nabic_diagnostic::{Code, NabiDiagnostic, SourceMap};
use nabic_mir::TargetSpec;
use nabic_target::{Linker, ObjectFormat};
use std::borrow::Cow;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Intermediate artifact selectable with `--emit=<kind>`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EmitKind {
    /// `source`: the package's whole AST after meta expansion and
    /// desugaring, as source (needs `keep_ast`).
    Ast,
    Hir,
    Mir,
    Obj,
    /// `asm`: the object as assembly source, which an assembler turns
    /// back into the same object.
    Asm,
    /// `listing`: each function's instructions with their offsets and
    /// bytes, as `llvm-objdump -dr` prints them.
    Listing,
    /// `expansions`: meta expansions alone, as source text; needs the
    /// parsed sources (`Session::set_keep_ast`), so `CompileResult::emit`
    /// makes it.
    Expanded,
    /// `c-header`: a C header for the package's exports.
    CHeader,
}

impl EmitKind {
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "source" => Self::Ast,
            "hir" => Self::Hir,
            "mir" => Self::Mir,
            "obj" => Self::Obj,
            "asm" => Self::Asm,
            "listing" => Self::Listing,
            "expansions" => Self::Expanded,
            "c-header" => Self::CHeader,
            _ => return None,
        })
    }

    /// File extension used under `target/`.
    #[must_use]
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Ast => "ast.nabi",
            Self::Hir => "hir",
            Self::Mir => "mir",
            Self::Obj => "o",
            Self::Asm => "s",
            Self::Listing => "lst",
            Self::Expanded => "expanded.nabi",
            Self::CHeader => "h",
        }
    }
}

/// How to build native code; the target is the compiled module's.
#[derive(Clone, Debug)]
pub struct BuildOptions {
    /// `0`..`3`, `s` or `z`: `-O<level>`.
    pub opt_level: String,
    /// Runtime library: a path when it has an extension, else `-l<name>`.
    pub nabir: String,
    /// A shipped C library (`Libc::shipped`): the C runtime objects and C library
    /// for the target (`<sysroot>/lib/nabi/targets/<target>/`).
    pub libc: Option<PathBuf>,
    /// Keep the scratch directory instead of deleting it.
    pub keep_temps: bool,
    /// `-g`: DWARF debug info.
    pub debug: bool,
    /// Codegen a REPL cell (`cell_object`).
    pub repl: Option<nabic_codegen::ReplCell>,
    /// Integer overflow panics (`-C overflow-checks`, the manifest's
    /// profile `overflow_checks`); on unless turned off.
    pub overflow_checks: bool,
    /// `-L <dir>`: native library search directories, before the C
    /// driver's `LIBRARY_PATH`.
    pub lib_dirs: Vec<PathBuf>,
    /// `-l [static=|dylib=]<name>`: native libraries linked as if named by
    /// `#[link]` (`true`: static).
    pub libs: Vec<(String, bool)>,
    /// `-l framework=<name>` / `-F <dir>`: Apple frameworks, as if named
    /// by `#[link(kind = "framework")]`, and their search directories.
    pub frameworks: Vec<String>,
    pub framework_dirs: Vec<PathBuf>,
    /// `--native-object <file>`: objects and archives linked in as given
    /// (C sources a package compiled).
    pub objects: Vec<PathBuf>,
    /// `--lib-version <x.y.z>`: a shared library's version, giving its
    /// ELF soname and Mach-O current and compatibility versions.
    pub lib_version: Option<String>,
    /// `--native-runtime <file>`: shared libraries copied beside the
    /// executable, which searches its own directory (`@loader_path` /
    /// `$ORIGIN`; Windows always does).
    pub runtime_files: Vec<PathBuf>,
    /// `-C subsystem=console|windows`: a Windows executable's subsystem.
    pub subsystem: crate::windows::Subsystem,
    /// Build by codegen units, reusing unchanged units' objects from here.
    pub incremental: Option<PathBuf>,
    /// `-C icf=yes|no`: fold identical code at link; `None`: in `release`.
    pub icf: Option<bool>,
    /// `--remap-path-prefix` and a release build's own rules
    /// (`SourceMap::set_remap`): in debug info, so in its units' keys.
    pub remap: Vec<(PathBuf, String)>,
}

impl BuildOptions {
    /// The `release` profile: `-O2`, `-O3` or `-Os`.
    #[must_use]
    pub fn release(&self) -> bool {
        matches!(self.opt_level.as_str(), "2" | "3" | "s")
    }

    /// Identical code folding: in `release` unless `-C icf` says.
    #[must_use]
    pub fn icf(&self) -> bool {
        self.icf.unwrap_or_else(|| self.release())
    }
}

impl Default for BuildOptions {
    fn default() -> Self {
        Self {
            opt_level: "0".to_owned(),
            nabir: "nabir".to_owned(),
            libc: None,
            keep_temps: false,
            debug: false,
            repl: None,
            overflow_checks: true,
            lib_dirs: Vec::new(),
            libs: Vec::new(),
            frameworks: Vec::new(),
            framework_dirs: Vec::new(),
            objects: Vec::new(),
            lib_version: None,
            runtime_files: Vec::new(),
            subsystem: crate::windows::Subsystem::Console,
            incremental: None,
            icf: None,
            remap: Vec::new(),
        }
    }
}

/// Private scratch directory, created exclusively and removed on drop
/// unless `keep` is set.
pub struct TempDir {
    path: PathBuf,
    keep: bool,
    /// What `new` made, removed by its own drop; `beside`'s has none.
    entry: Option<TempEntry>,
}

impl TempDir {
    pub fn new(keep: bool) -> Result<Self, NabiDiagnostic> {
        let mut e = TempEntry::dir("nabic-").map_err(|e| {
            err(
                Code::IO,
                format!("cannot create a temporary directory: {e}"),
            )
        })?;
        if keep {
            e.keep();
        }
        Ok(Self {
            path: e.path().to_owned(),
            keep,
            entry: Some(e),
        })
    }

    /// `.<name>.objs` beside the output `out`, made anew: the same
    /// path in every build of `out`.
    pub fn beside(out: &Path, keep: bool) -> Result<Self, NabiDiagnostic> {
        let mut name = std::ffi::OsString::from(".");
        name.push(out.file_name().unwrap_or_default());
        name.push(".objs");
        let path = out.with_file_name(name);
        let _ = std::fs::remove_dir_all(&path);
        // The parent may be a shared cache another process just trimmed.
        if let Some(p) = path.parent() {
            let _ = std::fs::create_dir_all(p);
        }
        private_dir(&path)
            .map_err(|e| err(Code::IO, format!("cannot create {}: {e}", path.display())))?;
        Ok(Self {
            path,
            keep,
            entry: None,
        })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        if self.keep {
            eprintln!(
                "{}: kept temporaries in {}",
                nabic_core::cli::invoked_as(),
                self.path.display()
            );
        } else if self.entry.is_none() {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

fn err(code: Code, msg: String) -> NabiDiagnostic {
    NabiDiagnostic::error(code, nabic_diagnostic::Span::DUMMY, msg)
}

/// Codegen for its errors alone (`nabic check`).
pub fn check_codegen(
    module: &CompiledModule,
    sources: &SourceMap,
    opts: &BuildOptions,
) -> Result<(), NabiDiagnostic> {
    crate::codegen::check(module, sources, opts)
}

/// Produce one artifact's bytes.
pub fn emit(
    module: &CompiledModule,
    sources: &SourceMap,
    kind: EmitKind,
    opts: &BuildOptions,
) -> Result<Vec<u8>, NabiDiagnostic> {
    match kind {
        EmitKind::Ast => Err(NabiDiagnostic::error(
            Code::INTERNAL,
            nabic_diagnostic::Span::DUMMY,
            "--emit=source needs the parsed files (keep_ast)",
        )),
        EmitKind::Hir => Ok(nabic_hir::print_hir(&module.hir).into_bytes()),
        EmitKind::CHeader => Ok(crate::header::write(module, sources).into_bytes()),
        EmitKind::Mir => Ok(nabic_mir::print_mir(&crate::codegen::mir(module, opts)).into_bytes()),
        EmitKind::Expanded => Err(err(
            Code::INTERNAL,
            "`--emit=expansions` needs the parsed sources".to_owned(),
        )),
        EmitKind::Obj => Ok(crate::codegen::object(module, sources, opts)?.object),
        EmitKind::Asm => Ok(crate::codegen::asm(module, sources, opts, true)?.into_bytes()),
        EmitKind::Listing => Ok(crate::codegen::asm(module, sources, opts, false)?.into_bytes()),
    }
}

/// Build an executable at `out`; by codegen units with
/// `opts.incremental`, returning how many it compiled.
pub fn link_executable(
    module: &CompiledModule,
    sources: &SourceMap,
    opts: &BuildOptions,
    out: &Path,
) -> Result<Option<crate::units::UnitCounts>, NabiDiagnostic> {
    let mut counts = None;
    let objects = if crate::units::applies(module, opts) {
        let dir = opts.incremental.as_deref();
        let (objects, c, _, _) = crate::units::objects(module, sources, opts, dir)?;
        counts = Some(c);
        objects
    } else {
        vec![crate::codegen::object(module, sources, opts)?.object]
    };
    // A Mach-O debug map names the objects (the .dSYM is made from them
    // before they go): the same names in every build.
    let tmp = TempDir::beside(out, opts.keep_temps)?;
    let mut paths = Vec::new();
    for (i, o) in objects.iter().enumerate() {
        let p = tmp.path().join(format!("module{i}.o"));
        std::fs::write(&p, o)
            .map_err(|e| err(Code::IO, format!("cannot write {}: {e}", p.display())))?;
        paths.push(p);
    }
    link_object(module, &paths, opts, out, tmp.path())?;
    Ok(counts)
}

/// Library build: `libfoo.a`.
///
/// By codegen units with `opts.incremental`, else one member. A
/// prebuilt-library build writes `foo.nabihir`
/// beside it, a sibling rather than a member so no object-section
/// reader is needed.
pub fn build_archive(
    module: &CompiledModule,
    sources: &SourceMap,
    opts: &BuildOptions,
    archive_out: &Path,
) -> Result<Option<crate::units::UnitCounts>, NabiDiagnostic> {
    let mut counts = None;
    let (objects, gc_types, facts) = if crate::units::applies(module, opts) {
        let dir = opts.incremental.as_deref();
        let (objects, c, gc_types, facts) = crate::units::objects(module, sources, opts, dir)?;
        counts = Some(c);
        (objects, gc_types, facts)
    } else {
        let mir = crate::codegen::mir(module, opts);
        let o = crate::codegen::compile(&mir, sources, opts)?;
        let facts = if module.interface.is_some() {
            nabic_diagnostic::time("summaries", || nabic_mir::summary::export(&mir, &o.quiet))
        } else {
            Vec::new()
        };
        (vec![o.object], o.gc_types, facts)
    };
    let tmp = TempDir::new(opts.keep_temps)?;
    let mut paths = Vec::new();
    for (i, o) in objects.iter().enumerate() {
        let p = tmp.path().join(format!("part{i}.o"));
        std::fs::write(&p, o)
            .map_err(|e| err(Code::IO, format!("cannot write {}: {e}", p.display())))?;
        paths.push(p);
    }
    // A fresh archive, not members appended to an old one.
    let _ = std::fs::remove_file(archive_out);
    let t = &module.mir.target.spec;
    let members: Vec<&Path> = paths.iter().map(PathBuf::as_path).collect();
    nabild::write_archive(t, archive_out, &members).map_err(|e| {
        err(
            Code::IO,
            format!("cannot write {}: {e}", archive_out.display()),
        )
    })?;
    if let Some(r) = &module.interface {
        let mut r = r.clone();
        r.type_count = gc_types;
        r.facts = facts;
        let path = archive_out.with_file_name(format!("{}.nabihir", r.name));
        std::fs::write(&path, r.encode())
            .map_err(|e| err(Code::IO, format!("cannot write {}: {e}", path.display())))?;
    }
    Ok(counts)
}

/// Link `objs` with the runtime and dependencies into `out`.
///
/// nabi's linker in process, or `NABI_CC`. Windows import libraries
/// (`windows::link_args`) go into `objs_dir` beside the objects.
pub fn link_object(
    module: &CompiledModule,
    objs: &[PathBuf],
    opts: &BuildOptions,
    out: &Path,
    objs_dir: &Path,
) -> Result<(), NabiDiagnostic> {
    let t = &*module.mir.target.spec;
    let apple = t.object_format == ObjectFormat::MachO;
    let windows = t.object_format == ObjectFormat::Coff;
    let search: Vec<OsString> = opts
        .lib_dirs
        .iter()
        .map(|d| {
            let mut l = OsString::from("-L");
            l.push(d);
            l
        })
        .collect();
    let mut libs = module.mir.link.native_libs.clone();
    crate::objc::link_runtime(&module.mir, &mut libs);
    for l in &opts.libs {
        if !libs.iter().any(|(n, _)| *n == l.0) {
            libs.push(l.clone());
        }
    }
    let frameworks = framework_args(module, opts)?;
    // The loader looks beside the executable for the shared libraries
    // copied there.
    let origin = if opts.runtime_files.is_empty() || windows {
        None
    } else if apple {
        Some("@loader_path")
    } else {
        Some("$ORIGIN")
    };
    let wit = module.mir.link.wit.as_ref();
    let host = module.mir.plugin.mode == nabic_mir::plugin::Mode::Host;
    if host {
        plugin_target(t, "a program that loads plugins")?;
    }
    if let Some(cc) = c_compiler().filter(|_| !host) {
        let tmp = TempDir::new(opts.keep_temps)?;
        let mut link = Command::new(cc);
        link.args(objs)
            .args(&opts.objects)
            .args(&module.archives)
            .arg(nabir_arg(opts, t));
        link.args(&search);
        link.args(&frameworks);
        link.args(cc_wit_args(wit, tmp.path())?);
        for (name, stat) in &libs {
            if *stat && apple {
                link.arg(static_archive(name, opts)?);
            } else if *stat {
                link.args(["-Wl,-Bstatic", &format!("-l{name}"), "-Wl,-Bdynamic"]);
            } else {
                link.arg(format!("-l{name}"));
            }
        }
        if apple {
            link.arg("-Wl,-dead_strip");
        } else if !windows {
            link.arg("-Wl,--gc-sections");
        }
        if let Some(o) = origin {
            link.arg(format!("-Wl,-rpath,{o}"));
        }
        link.args(cc_profile_args(t, opts));
        // Apple's clang links for the machine it runs as unless told.
        if !t.is_host() || apple {
            link.arg(format!("--target={}", cc_target(t)));
        }
        // core's float methods call libm; glibc keeps it out of libc.
        if !windows {
            link.arg("-lm");
        }
        link.arg("-o").arg(out);
        run(&mut link, "the link")?;
    } else if windows {
        let mut inputs: Vec<OsString> = objs.iter().map(|o| o.clone().into_os_string()).collect();
        inputs.extend(opts.objects.iter().map(|o| o.clone().into_os_string()));
        inputs.extend(module.archives.iter().map(|a| a.clone().into_os_string()));
        inputs.push(nabir_arg(opts, t).into());
        inputs.extend(search);
        inputs.extend(
            crate::windows::link_args(t, &libs, opts.subsystem, opts.debug, objs_dir)
                .map_err(|e| err(Code::LINK_FAILED, format!("the link failed:\n{e}")))?,
        );
        inputs.push(if opts.icf() { "/opt:icf" } else { "/opt:noicf" }.into());
        nabic_diagnostic::time("link", || link_program(module, &inputs, out, true, None))
            .map_err(|e| err(Code::LINK_FAILED, format!("the link failed:\n{e}")))?;
    } else {
        let tmp = TempDir::new(opts.keep_temps)?;
        let mut inputs: Vec<OsString> = objs.iter().map(|o| o.clone().into_os_string()).collect();
        if t.object_format == ObjectFormat::Wasm && t.threads {
            let path = tmp.path().join("nabi_tls.o");
            std::fs::write(&path, nabic_codegen_native::wasm_tls_object())
                .map_err(|e| err(Code::IO, format!("cannot write {}: {e}", path.display())))?;
            inputs.push(path.into_os_string());
        }
        inputs.extend(opts.objects.iter().map(|o| o.clone().into_os_string()));
        // A component: the world's exports kept for the componentizer.
        let wit_dir = wit.map(|w| wit_dir(w, tmp.path())).transpose()?;
        if let Some(w) = wit {
            inputs.extend(w.exports.iter().map(|s| format!("--export={s}").into()));
        }
        inputs.extend(module.archives.iter().map(|a| a.clone().into_os_string()));
        inputs.push(nabir_arg(opts, t).into());
        inputs.extend(search);
        inputs.extend(native_lib_args(&libs, apple, opts)?);
        inputs.extend(frameworks);
        if let Some(o) = origin {
            inputs.extend(["-rpath".into(), o.into()]);
        }
        inputs.extend(release_link_args(t, opts).into_iter().map(OsString::from));
        let t = &*entry_model(t, wit);
        let component = t.linker == Linker::WasmComponent;
        let core = if component {
            tmp.path().join("core.wasm")
        } else {
            out.to_owned()
        };
        nabic_diagnostic::time("link", || {
            link_program(module, &inputs, &core, true, opts.libc.as_deref())
        })
        .map_err(|e| err(Code::LINK_FAILED, format!("the link failed:\n{e}")))?;
        if component {
            let reactor = wit.is_some_and(|w| w.reactor);
            nabic_diagnostic::time("componentize", || {
                componentize(
                    &core,
                    opts.libc.as_deref(),
                    wit_dir.as_deref(),
                    reactor,
                    out,
                )
            })
            .map_err(|e| err(Code::LINK_FAILED, format!("the link failed:\n{e}")))?;
        }
    }
    let dir = out.parent().unwrap_or_else(|| Path::new("."));
    for f in &opts.runtime_files {
        let to = dir.join(f.file_name().unwrap_or_default());
        // A new file: one a running program maps stays intact.
        let _ = std::fs::remove_file(&to);
        std::fs::copy(f, &to).map_err(|e| {
            err(
                Code::IO,
                format!("cannot copy {} to {}: {e}", f.display(), to.display()),
            )
        })?;
    }
    // Mach-O keeps DWARF in the objects: gather it into a `.dSYM` while
    // they still exist.
    if opts.debug && apple {
        let left_out = nabic_diagnostic::time("dsym", || nabild::macho::dsym::write(out))
            .map_err(|e| err(Code::IO, e))?;
        for w in left_out {
            eprintln!(
                "{}: warning: no debug info in the .dSYM from {w}",
                nabic_core::cli::invoked_as()
            );
        }
    }
    Ok(())
}

/// A C-ABI shared library `out` (`--crate-type=cdylib`).
///
/// Library `archive` (built by [`build_archive`] from `module`), its
/// dependencies and the runtime, exporting the package's
/// `#[export_name]` functions; the loader runs `nabi_library_init`, which
/// starts the runtime and registers the library's GC types. Windows:
/// `out`'s import library beside it (`.lib`).
pub fn link_shared(
    module: &CompiledModule,
    opts: &BuildOptions,
    archive: &Path,
    out: &Path,
) -> Result<(), NabiDiagnostic> {
    let t = &*module.mir.target.spec;
    if c_compiler().is_some() || !nabild::links_shared(t) {
        return Err(err(
            Code::LINK_FAILED,
            format!(
                "a shared library (`--crate-type=cdylib`) for {} needs Nabi's own linker, which \
                 links them for aarch64-apple-darwin, x86_64-unknown-linux-gnu and the 64-bit \
                 Windows targets; build a static library (the default) and link it with your C \
                 toolchain instead",
                t.name
            ),
        ));
    }
    let lib = module.mir.link.lib.clone().unwrap_or_default();
    let owned = module.mir.functions.iter().filter(|f| {
        f.facts.linkage != nabic_ir_common::Linkage::Extern
            && nabic_mir::Link::owns(&lib, f.path())
            && f.facts.export_name.is_some()
    });
    // C calls an export with the C convention: a nabi-ABI one would get
    // its aggregates, `str`s and narrow integers wrong.
    if let Some(f) = owned
        .clone()
        .find(|f| !matches!(f.facts.abi, nabic_ir_common::Abi::C { .. }))
    {
        let sym = f.facts.export_name.clone().unwrap_or_default();
        let name = f.name.rsplit("::").next().unwrap_or(&f.name);
        return Err(err(
            Code::FFI,
            format!(
                "`#[export_name(\"{sym}\")]` exports `{name}` from the shared library, but \
                 `{name}` uses the nabi calling convention, which C does not"
            ),
        )
        .with_note(format!(
            "help: declare it `pub extern \"C\" fn {name}(..)`, keeping `#[export_name]`"
        )));
    }
    let mut exports: Vec<String> = owned.filter_map(|f| f.facts.export_name.clone()).collect();
    exports.sort();
    exports.dedup();
    let anchors = [(
        "nabi_library_register".to_owned(),
        format!("nabi.register_types.{lib}"),
    )];
    let windows = t.object_format == ObjectFormat::Coff;
    let implib = out.with_extension("lib");
    let file = out.file_name().unwrap_or_default().to_string_lossy();
    let soname = opts.lib_version.as_deref().and_then(|v| soname(&file, v));
    let compat = opts.lib_version.as_deref().map(compat_version);
    let sh = nabild::Shared {
        exports: &exports,
        init: if windows {
            "nabi_dll_main"
        } else {
            "nabi_library_init"
        },
        anchors: &anchors,
        implib: windows.then_some(implib.as_path()),
        soname: soname.as_deref(),
        versions: opts.lib_version.as_deref().zip(compat.as_deref()),
    };
    let search: Vec<OsString> = opts
        .lib_dirs
        .iter()
        .map(|d| {
            let mut l = OsString::from("-L");
            l.push(d);
            l
        })
        .collect();
    let mut libs = module.mir.link.native_libs.clone();
    crate::objc::link_runtime(&module.mir, &mut libs);
    for l in &opts.libs {
        if !libs.iter().any(|(n, _)| *n == l.0) {
            libs.push(l.clone());
        }
    }
    let frameworks = framework_args(module, opts)?;
    let tmp = TempDir::new(opts.keep_temps)?;
    let mut inputs: Vec<OsString> = vec![archive.as_os_str().to_owned()];
    inputs.extend(opts.objects.iter().map(|o| o.clone().into_os_string()));
    inputs.extend(module.archives.iter().map(|a| a.clone().into_os_string()));
    inputs.push(nabir_arg(opts, t).into());
    inputs.extend(search);
    if windows {
        inputs.extend(
            crate::windows::dll_link_args(t, &libs, opts.debug, tmp.path())
                .map_err(|e| err(Code::LINK_FAILED, format!("the link failed:\n{e}")))?,
        );
    } else {
        inputs.extend(native_lib_args(
            &libs,
            t.object_format == ObjectFormat::MachO,
            opts,
        )?);
        inputs.extend(frameworks);
        inputs.extend(release_link_args(t, opts).into_iter().map(OsString::from));
    }
    nabic_diagnostic::time("link", || {
        nabild::link_shared(t, &inputs, out, opts.libc.as_deref(), &sh)
    })
    .map_err(|e| err(Code::LINK_FAILED, format!("the link failed:\n{e}")))?;
    // Executables linked against it ask for the soname: a link of that
    // name beside it serves them.
    #[cfg(unix)]
    if let (Some(so), Some(dir)) = (&soname, out.parent()) {
        if *so != file {
            let at = dir.join(so);
            let _ = std::fs::remove_file(&at);
            std::os::unix::fs::symlink(&*file, &at)
                .map_err(|e| err(Code::IO, format!("cannot create {}: {e}", at.display())))?;
        }
    }
    Ok(())
}

/// What C code links beside a static library: system libraries
/// (`(name, framework)`, e.g. `("objc", false)`, `("UIKit", true)`).
pub type LinkNeeds = Vec<(String, bool)>;

/// A self-contained static library `out` (`--crate-type=staticlib`).
///
/// Library `archive`, its dependencies, the C objects and the runtime:
/// one archive a C, Swift or Xcode link takes alone. The runtime starts
/// on the first export called, which attaches its thread. Returns the
/// system libraries and frameworks the link still needs.
pub fn build_staticlib(
    module: &CompiledModule,
    opts: &BuildOptions,
    archive: &Path,
    out: &Path,
) -> Result<LinkNeeds, NabiDiagnostic> {
    let t = &*module.mir.target.spec;
    if t.object_format == ObjectFormat::Wasm {
        return Err(err(
            Code::LINK_FAILED,
            format!(
                "a static library for C (`--crate-type=staticlib`) needs a native target, not {}; \
                 build a wasm module (`src/main.nabi`) or a component instead",
                t.name
            ),
        ));
    }
    let nabir = PathBuf::from(nabir_arg(opts, t));
    let runtime = if nabir.is_file() {
        nabir
    } else {
        let file = t.runtime_lib();
        opts.lib_dirs
            .iter()
            .map(|d| d.join(&file))
            .find(|p| p.is_file())
            .ok_or_else(|| {
                err(
                    Code::LINK_FAILED,
                    format!("the runtime library {file} is in none of the library directories"),
                )
            })?
    };
    let mut inputs: Vec<PathBuf> = vec![archive.to_owned()];
    inputs.extend(module.archives.iter().cloned());
    inputs.extend(opts.objects.iter().cloned());
    let mut needs: LinkNeeds = Vec::new();
    let mut libs = module.mir.link.native_libs.clone();
    crate::objc::link_runtime(&module.mir, &mut libs);
    libs.extend(opts.libs.iter().cloned());
    for (name, stat) in libs {
        if stat {
            inputs.push(static_archive(&name, opts)?);
        } else if !needs.iter().any(|(n, _)| *n == name) {
            needs.push((name, false));
        }
    }
    inputs.push(runtime);
    for f in module.mir.link.frameworks.iter().chain(&opts.frameworks) {
        if !needs.iter().any(|(n, _)| n == f) {
            needs.push((f.clone(), true));
        }
    }
    let paths: Vec<&Path> = inputs.iter().map(PathBuf::as_path).collect();
    nabild::merge_archives(t, out, &paths)
        .map_err(|e| err(Code::IO, format!("cannot write {}: {e}", out.display())))?;
    Ok(needs)
}

/// The ELF soname of shared library `file` (`libgeo.so`) at version
/// `v`: `libgeo.so.<x>`, or `libgeo.so.0.<y>` below 1.0, where a minor
/// version breaks; `None` for a file not named `*.so`.
fn soname(file: &str, v: &str) -> Option<String> {
    let base = file.strip_suffix(".so")?;
    let mut it = v.split('.');
    let x = it.next().unwrap_or("0");
    let y = it.next().unwrap_or("0");
    Some(if x == "0" {
        format!("{base}.so.0.{y}")
    } else {
        format!("{base}.so.{x}")
    })
}

/// The Mach-O compatibility version of version `v`: the version every
/// compatible later one keeps (`x.0.0`, or `0.y.0` below 1.0).
fn compat_version(v: &str) -> String {
    let mut it = v.split('.');
    let x = it.next().unwrap_or("0");
    let y = it.next().unwrap_or("0");
    if x == "0" {
        format!("0.{y}.0")
    } else {
        format!("{x}.0.0")
    }
}

/// `-F <dir>` and `-framework <name>` of `#[link(kind = "framework")]`
/// and `-l framework=`; an error off Apple targets.
fn framework_args(
    module: &CompiledModule,
    opts: &BuildOptions,
) -> Result<Vec<OsString>, NabiDiagnostic> {
    let t = &*module.mir.target.spec;
    let mut names: Vec<&String> = module.mir.link.frameworks.iter().collect();
    for f in &opts.frameworks {
        if !names.contains(&f) {
            names.push(f);
        }
    }
    if names.is_empty() {
        return Ok(Vec::new());
    }
    if t.object_format != ObjectFormat::MachO {
        return Err(err(
            Code::LINK_FAILED,
            format!(
                "framework `{}` is linked, but frameworks exist only on Apple targets, not {}",
                names[0], t.name
            ),
        )
        .with_note(
            "help: put the `extern` block under `#[cfg(any(target_os = \"macos\", target_os = \"ios\"))]`".to_owned(),
        ));
    }
    let mut a: Vec<OsString> = Vec::new();
    for d in &opts.framework_dirs {
        a.extend(["-F".into(), d.clone().into_os_string()]);
    }
    for n in names {
        a.extend(["-framework".into(), n.into()]);
    }
    Ok(a)
}

/// Whether `t` takes plugins, and if not why.
fn plugin_target(t: &TargetSpec, what: &str) -> Result<(), NabiDiagnostic> {
    let why = if t.object_format == ObjectFormat::Wasm {
        format!(
            "{what} needs a native target, and {} has no way to load code at run time; on wasm, \
             build one program: make the plugin's package a dependency",
            t.name
        )
    } else if t.libc == nabic_target::Libc::Musl {
        format!(
            "{what} needs a C library that loads shared objects, and {} links one statically; \
             build for the glibc target (`--target {}`), or build one program",
            t.name,
            t.name.replace("-musl", "-gnu")
        )
    } else if nabic_target::apple::Platform::of(t) == Some(nabic_target::apple::Platform::Ios) {
        format!(
            "{what} loads code at run time, and an iOS device loads only code signed into the \
             app; load it on the simulator: `--target aarch64-apple-ios-sim`"
        )
    } else if !nabild::links_plugin(t) {
        format!("{what} is not supported for {} yet", t.name)
    } else {
        return Ok(());
    };
    Err(err(Code::LINK_FAILED, why))
}

/// A plugin `out` (`--crate-type=plugin`): library `archive` with the
/// dependencies it bundles, and no runtime: the module loader binds the
/// rest to the host and the plugins already loaded.
pub fn link_plugin(
    module: &CompiledModule,
    opts: &BuildOptions,
    archive: &Path,
    out: &Path,
) -> Result<(), NabiDiagnostic> {
    let t = &*module.mir.target.spec;
    plugin_target(t, "a plugin")?;
    if c_compiler().is_some() {
        return Err(err(
            Code::LINK_FAILED,
            "a plugin needs Nabi's own linker; unset NABI_CC to build it".to_owned(),
        ));
    }
    let bundle = &module.mir.plugin.bundle;
    let mut inputs: Vec<OsString> = vec![archive.as_os_str().to_owned()];
    inputs.extend(
        module
            .archives
            .iter()
            .filter(|a| {
                let stem = a.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                bundle
                    .iter()
                    .any(|b| stem == format!("lib{b}") || stem == b)
            })
            .map(|a| a.clone().into_os_string()),
    );
    inputs.extend(opts.lib_dirs.iter().map(|d| {
        let mut l = OsString::from("-L");
        l.push(d);
        l
    }));
    let mut libs = module.mir.link.native_libs.clone();
    for l in &opts.libs {
        if !libs.iter().any(|(n, _)| *n == l.0) {
            libs.push(l.clone());
        }
    }
    let tmp = TempDir::new(opts.keep_temps)?;
    if t.object_format == ObjectFormat::Coff {
        // The C runtime and system libraries, not the cdylib's startup:
        // the link adds a plugin's DLL entry.
        let args = crate::windows::dll_link_args(t, &libs, opts.debug, tmp.path())
            .map_err(|e| err(Code::LINK_FAILED, format!("the link failed:\n{e}")))?;
        inputs.extend(args.into_iter().skip(1));
    } else {
        inputs.extend(native_lib_args(
            &libs,
            t.object_format == ObjectFormat::MachO,
            opts,
        )?);
        // Its descriptor stays exported: no `-no_exported_symbols`.
        let keep = release_link_args(t, opts)
            .into_iter()
            .filter(|a| *a != "-no_exported_symbols");
        inputs.extend(keep.map(OsString::from));
    }
    let sh = nabild::Shared {
        exports: &[],
        init: "",
        anchors: &[],
        implib: None,
        soname: None,
        versions: None,
    };
    let hot_deps: Vec<String> = module
        .mir
        .plugin
        .hot
        .iter()
        .flat_map(|h| {
            h.packages[1..]
                .iter()
                .map(|p| module.mir.link.hash_of(&format!("{p}::")).to_owned())
        })
        .collect();
    // A hot build calls its own functions through slots.
    let p = nabild::plugin::Plugin {
        exports: &[],
        import_own: module.mir.plugin.hot.is_some(),
        host: false,
        hot_deps: &hot_deps,
    };
    let glue = |s: &nabild::plugin::Scan| nabic_codegen_native::glue::object(t, s);
    nabic_diagnostic::time("link", || {
        nabild::link_plugin(t, &inputs, out, opts.libc.as_deref(), &sh, &p, &glue)
    })
    .map_err(|e| err(Code::LINK_FAILED, format!("the link failed:\n{e}")))?;
    Ok(())
}

/// An executable's link; a program that loads plugins keeps its runtime
/// and lists what plugins bind to.
fn link_program(
    module: &CompiledModule,
    inputs: &[OsString],
    out: &Path,
    strip: bool,
    libc: Option<&Path>,
) -> Result<String, String> {
    let t = &*module.mir.target.spec;
    if module.mir.plugin.mode != nabic_mir::plugin::Mode::Host {
        return link_with(t, inputs, out, strip, libc);
    }
    let p = nabild::plugin::Plugin {
        exports: &[],
        import_own: false,
        host: true,
        hot_deps: &[],
    };
    let glue = |s: &nabild::plugin::Scan| nabic_codegen_native::glue::object(t, s);
    nabild::link_host(t, inputs, out, strip, libc, &p, &glue)
}

/// Linker arguments of the profile: identical code folding (`-C icf`)
/// and, in a `release` link without `-g`, only the symbols the loader
/// needs. Wasm keeps its name section.
fn release_link_args(t: &TargetSpec, opts: &BuildOptions) -> Vec<&'static str> {
    let mut a = Vec::new();
    if opts.icf() {
        a.push("--icf=safe");
    }
    if opts.release() && !opts.debug {
        match t.object_format {
            ObjectFormat::MachO => a.extend(["-x", "-no_exported_symbols"]),
            ObjectFormat::Elf => a.push("--strip-all"),
            _ => {}
        }
    }
    a
}

/// `NABI_CC` arguments for the profile: `-g`, or a release link's
/// stripping.
fn cc_profile_args(t: &TargetSpec, opts: &BuildOptions) -> &'static [&'static str] {
    match t.object_format {
        _ if opts.debug => &["-g"],
        _ if !opts.release() => &[],
        ObjectFormat::MachO => &["-Wl,-x,-no_exported_symbols"],
        ObjectFormat::Elf => &["-s"],
        _ => &[],
    }
}

/// `wasm32-wasip2`: core module `core` to component `out`, wrapped with
/// the `wasi_snapshot_preview1` adapter of the target's directory `libc`
/// (`targets/build.sh`), the world in `wit` (`wit_dir`).
fn componentize(
    core: &Path,
    libc: Option<&Path>,
    wit: Option<&Path>,
    reactor: bool,
    out: &Path,
) -> Result<(), String> {
    let dir = libc.ok_or("no WASI adapter for wasm32-wasip2 (targets/build.sh)")?;
    let kind = if reactor { "reactor" } else { "command" };
    let adapter = dir.join(format!("wasi_snapshot_preview1.{kind}.wasm"));
    let read = |p: &Path| std::fs::read(p).map_err(|e| format!("cannot read {}: {e}", p.display()));
    let c = nabic_wit::componentize(&read(core)?, &read(&adapter)?, wit, reactor)?;
    std::fs::write(out, c).map_err(|e| format!("cannot write {}: {e}", out.display()))
}

/// The world of `ffi::wit` in a directory under `tmp`, its sources under
/// `deps/`.
fn wit_dir(w: &nabic_mir::WitLink, tmp: &Path) -> Result<PathBuf, NabiDiagnostic> {
    let io =
        |p: &Path, e: std::io::Error| err(Code::IO, format!("cannot write {}: {e}", p.display()));
    let dir = tmp.join("wit");
    let deps = dir.join("deps");
    std::fs::create_dir_all(&deps).map_err(|e| io(&deps, e))?;
    let world = dir.join("component.wit");
    std::fs::write(&world, &w.world).map_err(|e| io(&world, e))?;
    for (i, src) in w.sources.iter().enumerate() {
        let stem = src
            .path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("wit");
        let to = deps.join(format!("{i}-{stem}"));
        if src.dir {
            std::fs::create_dir_all(&to).map_err(|e| io(&to, e))?;
        }
        for (path, text) in &src.files {
            let dst = if src.dir {
                to.join(path.file_name().unwrap_or_default())
            } else {
                deps.join(format!("{i}-{stem}.wit"))
            };
            std::fs::write(&dst, text).map_err(|e| io(&dst, e))?;
        }
    }
    Ok(dir)
}

/// The target to link for: a component without `main` is a reactor
/// (`crt1-reactor.o`, no entry; the host calls `_initialize`).
fn entry_model<'t>(t: &'t TargetSpec, wit: Option<&nabic_mir::WitLink>) -> Cow<'t, TargetSpec> {
    if wit.is_some_and(|w| w.reactor) {
        Cow::Owned(TargetSpec {
            crt_objects: vec!["crt1-reactor.o".to_owned()],
            pre_link_args: [&t.pre_link_args[..], &["--no-entry".to_owned()]].concat(),
            ..t.clone()
        })
    } else {
        Cow::Borrowed(t)
    }
}

/// The C compiler driver's name of target `t`: clang calls a WASI
/// preview 2 component by its Rust name (`wasm32-wasip2`).
fn cc_target(t: &TargetSpec) -> String {
    if t.linker == Linker::WasmComponent {
        t.rust_target.clone()
    } else {
        t.c_triple()
    }
}

/// The world (`wit_dir`) and its exports for the C compiler driver
/// (`NABI_CC`), which hands them to its `wasm-component-ld`; a reactor's
/// exec model.
fn cc_wit_args(
    w: Option<&nabic_mir::WitLink>,
    tmp: &Path,
) -> Result<Vec<OsString>, NabiDiagnostic> {
    let Some(w) = w else {
        return Ok(Vec::new());
    };
    let mut args: Vec<OsString> =
        vec!["--component-type".into(), wit_dir(w, tmp)?.into_os_string()];
    args.extend(w.exports.iter().map(|s| format!("--export={s}").into()));
    let mut out = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut wl = OsString::from("-Wl,");
        wl.push(a);
        if a == "--component-type" {
            wl.push(",");
            wl.push(it.next().map_or_else(OsString::new, Clone::clone));
        }
        out.push(wl);
    }
    if w.reactor {
        out.push("-mexec-model=reactor".into());
    }
    Ok(out)
}

/// Linker arguments for the `#[link]` libraries `libs` (name, static).
fn native_lib_args(
    libs: &[(String, bool)],
    apple: bool,
    opts: &BuildOptions,
) -> Result<Vec<OsString>, NabiDiagnostic> {
    let mut a: Vec<OsString> = Vec::new();
    for (name, stat) in libs {
        if *stat && apple {
            a.push(static_archive(name, opts)?.into_os_string());
        } else if *stat {
            a.extend([
                "-Bstatic".into(),
                format!("-l{name}").into(),
                "-Bdynamic".into(),
            ]);
        } else {
            a.push(format!("-l{name}").into());
        }
    }
    Ok(a)
}

/// `#[link(name, kind = "static")]` for ld64, which has no `-Bstatic` and
/// prefers `.dylib`: the first `lib<name>.a` in `-L` dirs, then
/// `LIBRARY_PATH`.
fn static_archive(name: &str, opts: &BuildOptions) -> Result<PathBuf, NabiDiagnostic> {
    let env = std::env::var_os("LIBRARY_PATH").unwrap_or_default();
    let dirs: Vec<PathBuf> = opts
        .lib_dirs
        .iter()
        .cloned()
        .chain(std::env::split_paths(&env).filter(|d| !d.as_os_str().is_empty()))
        .collect();
    let file = format!("lib{name}.a");
    dirs.iter()
        .map(|d| d.join(&file))
        .find(|p| p.is_file())
        .ok_or_else(|| {
            err(Code::LINK_FAILED, format!(
                "`#[link(name = \"{name}\", kind = \"static\")]`: no {file} in the -L directories \
             or LIBRARY_PATH"
            ))
        })
}

/// `--nabir`: a library path when it has an extension, else `-l<name>`
/// (COFF: the library's file name).
fn nabir_arg(opts: &BuildOptions, t: &TargetSpec) -> String {
    if Path::new(&opts.nabir).extension().is_some() {
        opts.nabir.clone()
    } else if t.object_format == ObjectFormat::Coff {
        t.staticlib(&opts.nabir)
    } else {
        format!("-l{}", opts.nabir)
    }
}

/// The REPL host: its `main` object with the whole runtime, which holds
/// the host (`nabir/src/repl.rs`), linked in and exported, for the
/// cells it links itself.
pub fn link_host(
    host_object: &Path,
    opts: &BuildOptions,
    out: &Path,
) -> Result<(), NabiDiagnostic> {
    let t = TargetSpec::host();
    if t.object_format == ObjectFormat::Coff {
        return link_windows_host(t, host_object, opts, out);
    }
    let cc = c_compiler();
    let flag = |f: &str| -> OsString {
        if cc.is_some() {
            format!("-Wl,{f}").into()
        } else {
            f.into()
        }
    };
    let obj = host_object.as_os_str().to_owned();
    let nabir = OsString::from(nabir_arg(opts, t));
    // ELF: the runtime in the dynamic symbol table, for `dlsym`.
    let inputs = if t.object_format == ObjectFormat::MachO {
        let mut v = vec![obj, flag("-all_load"), nabir];
        if cc.is_some() {
            v.push(format!("--target={}", t.c_triple()).into());
        }
        v
    } else {
        vec![
            flag("--export-dynamic"),
            obj,
            flag("--whole-archive"),
            nabir,
            flag("--no-whole-archive"),
        ]
    };
    if let Some(cc) = cc {
        let mut link = Command::new(cc);
        link.args(&inputs).arg("-lm").arg("-o").arg(out);
        return run(&mut link, "the REPL host link");
    }
    link_with(t, &inputs, out, false, opts.libc.as_deref())
        .map(drop)
        .map_err(|e| {
            err(
                Code::LINK_FAILED,
                format!("cannot link the REPL host:\n{e}"),
            )
        })
}

/// nabi's own linker.
fn link_with(
    t: &TargetSpec,
    inputs: &[OsString],
    out: &Path,
    strip: bool,
    libc: Option<&Path>,
) -> Result<String, String> {
    nabild::link_executable(t, inputs, out, strip, libc)
}

/// Windows host: every runtime member, every symbol exported (found with
/// `GetProcAddress`), linked like a program.
fn link_windows_host(
    t: &TargetSpec,
    host_object: &Path,
    opts: &BuildOptions,
    out: &Path,
) -> Result<(), NabiDiagnostic> {
    let tmp = TempDir::new(opts.keep_temps)?;
    let mut inputs: Vec<OsString> = vec![
        host_object.into(),
        format!("/wholearchive:{}", nabir_arg(opts, t)).into(),
        // Honored in lld-link's MinGW mode only (nabi's linker too).
        "-lldmingw".into(),
        "/export-all-symbols".into(),
    ];
    let fail = |e: String| {
        err(
            Code::LINK_FAILED,
            format!("cannot link the REPL host:\n{e}"),
        )
    };
    inputs.extend(
        crate::windows::link_args(t, &[], crate::Subsystem::Console, false, tmp.path())
            .map_err(fail)?,
    );
    link_with(t, &inputs, out, false, None)
        .map(drop)
        .map_err(fail)
}

/// The C compiler driver that links instead of nabi's linker: `NABI_CC`.
fn c_compiler() -> Option<OsString> {
    std::env::var_os("NABI_CC").filter(|c| !c.is_empty())
}

/// nabic's build cache, beside the toolchain when writable.
///
/// `<sysroot>/lib/nabi-cache` when installed, else `nabi-cache/` beside
/// `nabic`. A read-only install (a Nix store path, a system package) uses
/// `nabi/nabic-<hash of the sysroot>` in the user's cache directory,
/// unless it ships a cache of its own (a sandbox image warmed when it
/// was built): that one is read as is, and nothing new is kept.
#[must_use]
pub fn cache_dir() -> Option<PathBuf> {
    static DIR: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let exe = nabic_core::current_exe()?;
        let bin = exe.parent()?;
        let sysroot = bin.parent()?;
        let own = if sysroot.join("lib/nabi/core/src").is_dir() {
            sysroot.join("lib/nabi-cache")
        } else {
            bin.join("nabi-cache")
        };
        if writable(&own) || shipped(&own) {
            return Some(own);
        }
        let key = nabic_core::hash::fnv(sysroot.as_os_str().as_encoded_bytes());
        Some(user_cache()?.join(format!("nabic-{key:016x}")))
    })
    .clone()
}

/// `dir` exists or can be made, and takes a new file.
fn writable(dir: &Path) -> bool {
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let probe = dir.join(format!(".probe-{}", std::process::id()));
    let ok = std::fs::write(&probe, b"").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

/// `dir` is a read-only cache with something in it.
fn shipped(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|mut d| d.next().is_some())
}

/// `nabi/` in the user's cache directory.
fn user_cache() -> Option<PathBuf> {
    let env = |k: &str| {
        std::env::var_os(k)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    let base = if cfg!(windows) {
        env("LOCALAPPDATA")
    } else if cfg!(target_os = "macos") {
        env("HOME").map(|h| h.join("Library/Caches"))
    } else {
        env("XDG_CACHE_HOME").or_else(|| env("HOME").map(|h| h.join(".cache")))
    };
    Some(base?.join("nabi"))
}

fn run(cmd: &mut Command, what: &str) -> Result<(), NabiDiagnostic> {
    let out = nabic_diagnostic::time(what, || cmd.output())
        .map_err(|e| err(Code::TOOL_FAILED, format!("cannot run {what}: {e}")))?;
    if !out.status.success() {
        return Err(err(
            Code::TOOL_FAILED,
            format!("{what} failed:\n{}", String::from_utf8_lossy(&out.stderr)),
        ));
    }
    Ok(())
}
