//! Building: sessions, the front end, libraries, executables and their caches.

#[allow(clippy::wildcard_imports)]
use crate::*;

/// The package named by the first positional (default: `.`).
pub fn discover_arg(a: &Args) -> Package {
    discover_at(a, a.positional.first().map_or(".", String::as_str))
}

/// The package at `path`, named `--name` when given.
pub fn discover_at(a: &Args, path: &str) -> Package {
    let mut pkg = nabic_interface::discover(Path::new(path)).unwrap_or_else(|e| fail(&e));
    if let Some(n) = a.name.as_ref().filter(|_| !pkg.anonymous) {
        pkg.name.clone_from(n);
    }
    // Code names a library by its package name (an executable beside it
    // too): that name must be one code can spell.
    if pkg.lib || pkg.library.is_some() {
        if let Some(e) = nabic_core::reserved_package(&nabic_interface::code_name(&pkg.name)) {
            fail(&format!("{}: {e}", pkg.root.display()));
        }
    }
    pkg
}

pub fn create_dir(dir: &Path) {
    if let Err(e) = std::fs::create_dir_all(dir) {
        fail(&format!("cannot create {}: {e}", dir.display()));
    }
}

/// A release build writes the package's paths relative to it and a
/// source dependency's as under its name, so where they were built
/// is not in it; `--remap-path-prefix` rules come after (and win).
fn trim_paths(a: &mut Args, pkg: Option<&Package>) {
    if !a.opts.release() {
        return;
    }
    let abs = |p: &Path| std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
    let mut rules = Vec::new();
    if let Some(p) = pkg.filter(|p| !p.anonymous) {
        rules.push((abs(&p.dir), String::new()));
    }
    for (name, root) in &a.externs {
        if root.extension().is_some_and(|e| e == "nabi") {
            let src = root.parent().unwrap_or(root);
            let dir = if src.ends_with("src") {
                src.parent().unwrap_or(src)
            } else {
                src
            };
            rules.push((abs(dir), name.clone()));
        }
    }
    rules.append(&mut a.opts.remap);
    a.opts.remap = rules;
}

/// Prebuild source `--extern`s into `<out>/deps` and use their NABIHIRs.
pub fn prebuild(a: &mut Args, pkg: Option<&Package>) -> Result<PathBuf, String> {
    trim_paths(a, pkg);
    let dir = out_dir(a, pkg).join("deps");
    let build = DepBuild {
        bootstrap: &a.bootstrap,
        cfg: &a.cfg,
        features: &a.extern_features,
        deps: &a.extern_deps,
        opts: &a.opts,
        target: a.spec(),
        dir: &dir,
        quiet: a.quiet,
    };
    a.externs = build.prebuild(&a.externs)?;
    Ok(dir)
}

/// What `compile` builds for.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Build,
    Test,
    Bench,
    /// Front end only, ASTs kept.
    Doc,
}

/// A session for `pkg` with the command line's libraries and flags.
pub fn session(a: &Args, pkg: &Package) -> Session {
    let mut session = Session::new();
    for b in &a.bootstrap {
        session.add_bootstrap_file(b.clone());
    }
    for c in &a.cfg {
        session.add_cfg(c.clone());
    }
    if a.opts.opt_level == "0" {
        session.add_cfg("debug_assertions".to_owned());
    }
    for (n, p) in &a.externs {
        session.add_extern(n.clone(), p.clone());
    }
    for (d, n) in &a.extern_deps {
        session.add_extern_dep(d.clone(), n.clone());
    }
    session.set_anonymous(pkg.anonymous);
    session.set_own_library(pkg.library.as_ref().map(|_| lib_name(pkg)));
    session.set_target(a.spec());
    session.set_incremental(incremental_dir(a, pkg));
    session.set_remap(a.opts.remap.clone());
    session.set_check_library(a.check_library);
    session.set_plugin(a.plugin && pkg.lib, &a.bundle);
    if a.hot && pkg.lib {
        session.set_hot(a.hot_base.clone(), &a.hot_deps);
    }
    session
}

/// `-v`: what the front end took from the incremental directory.
pub fn reused(a: &Args, result: &CompileResult) {
    if let Some(r) = result.reuse.filter(|_| a.verbose > 0 && !a.quiet) {
        eprintln!(
            "      Reused: {} of {} files parsed, {} of {} bodies lowered{}",
            r.files.0,
            r.files.1,
            r.bodies.0,
            r.bodies.1,
            if r.library {
                ", the library's items collected"
            } else {
                ""
            }
        );
    }
}

pub fn progress(a: &Args, verb: &str, pkg: &Package) {
    if a.quiet {
        return;
    }
    // `doc --std` alone documents a stand-in package in a temporary
    // directory: name what is documented instead.
    let what = if a.std_only {
        "the standard library".to_owned()
    } else {
        format!("{} ({})", pkg.name, pkg.dir.display())
    };
    term::status(verb, &what);
}

/// Front end for `pkg`: reports warnings (also returned, rendered), exits
/// 1 on errors, prints the progress line unless `-q`. The instant
/// is the command's start, for `finished`.
pub fn compile(a: &Args, pkg: &Package, mode: Mode) -> (CompileResult, String, Instant) {
    let t = Instant::now();
    progress(
        a,
        if mode == Mode::Doc {
            "Documenting"
        } else {
            "Compiling"
        },
        pkg,
    );
    let mut session = session(a, pkg);
    // A library's `build` is a prebuilt library: `lib<name>.a` + NABIHIR.
    if pkg.lib && mode == Mode::Build {
        session.set_lib_name(Some(lib_name(pkg)));
    }
    session.set_test(mode == Mode::Test);
    session.set_bench(mode == Mode::Bench);
    // A build lints as `check` does.
    let lints = mode != Mode::Doc;
    let library_files = a.emit.contains(&EmitKind::Expanded) || a.emit.contains(&EmitKind::Ast);
    session.set_keep_ast(true);
    session.set_library_items(lints && !library_files);
    let result = session.compile_file(&pkg.root);
    if result.has_errors() {
        print_rendered(
            &result
                .warnings
                .iter()
                .map(|w| result.render(w))
                .collect::<String>(),
        );
        result.report_errors();
        // A failed compile's list holds its warnings too.
        let e = result.errors.iter().filter(|d| d.is_error()).count();
        let w = result.warnings.len() + result.errors.len() - e;
        could_not_build(a, pkg, e, w);
    }
    let diags = if lints {
        nabic_tools::lint::run(&result, &a.lint_levels)
    } else {
        result.warnings.clone()
    };
    let (errors, warnings): (Vec<_>, Vec<_>) = diags.iter().partition(|d| d.is_error());
    let n = warnings.len();
    let warnings: String = warnings.iter().map(|w| result.render(w)).collect();
    print_rendered(&warnings);
    if !errors.is_empty() {
        errors.iter().for_each(|d| result.report(d));
        could_not_build(a, pkg, errors.len(), n);
    }
    stage(a, "front end", t);
    reused(a, &result);
    if a.verbose > 1 && !a.quiet {
        eprint!("{}", result.trace());
    }
    (result, warnings, started())
}

/// `error: could not build <name> (1 error, 2 warnings)`, then exit 1.
pub fn could_not_build(a: &Args, pkg: &Package, errors: usize, warnings: usize) -> ! {
    if !a.quiet && !a.json {
        eprintln!(
            "{} could not build {} ({})",
            term::label(Stream::Stderr, Role::Error),
            pkg.name,
            counts(errors, warnings)
        );
    }
    process::exit(1);
}

/// `1 error, 2 warnings`.
pub fn counts(errors: usize, warnings: usize) -> String {
    let s = |n: usize| if n == 1 { "" } else { "s" };
    format!(
        "{errors} error{}, {warnings} warning{}",
        s(errors),
        s(warnings)
    )
}

/// Incremental directory: `<out>/incremental/<name>-<hash>`,
/// or under `$NABI_INCREMENTAL`. A lone file gets one only there;
/// `NABI_INCREMENTAL=0` turns it off.
pub fn incremental_dir(a: &Args, pkg: &Package) -> Option<PathBuf> {
    let env = std::env::var_os("NABI_INCREMENTAL").filter(|v| !v.is_empty());
    let root = match env {
        Some(v) if v == "0" => return None,
        Some(v) => PathBuf::from(v),
        None if pkg.anonymous => return None,
        None => out_dir(a, Some(pkg)).join("incremental"),
    };
    let id = std::path::absolute(&pkg.root).unwrap_or_else(|_| pkg.root.clone());
    let h = nabic_core::hash::fnv(id.to_string_lossy().as_bytes());
    Some(root.join(format!("{}-{h:016x}", pkg.name)))
}

/// The library's package name as code spells it.
pub fn lib_name(pkg: &Package) -> String {
    nabic_interface::code_name(&pkg.name)
}

/// Codegen-unit name: `lib<name>` for a library, to keep it apart from a
/// package's executable.
pub fn unit_name(pkg: &Package) -> String {
    if pkg.lib {
        format!("lib{}", pkg.name)
    } else {
        pkg.name.clone()
    }
}

/// Package with both root files: build the library and mount it as dep
/// `<name>` of the executable.
pub fn mount_library(a: &mut Args, pkg: &Package) {
    let Some(lib) = pkg.library_target() else {
        return;
    };
    let archive = build_lib(a, &lib, None);
    a.externs.push((
        lib_name(&lib),
        nabic_interface::nabihir::companions(&archive).0,
    ));
}

/// Build library `pkg` into `out` (default `target/<profile>/lib<name>.a`
/// plus `.nabihir`) unless up to date; the archive.
pub fn build_lib(a: &Args, pkg: &Package, out: Option<PathBuf>) -> PathBuf {
    let target = out_dir(a, Some(pkg));
    let default = || {
        create_dir(&target);
        target.join(format!("lib{}.a", lib_name(pkg)))
    };
    // `--crate-type=cdylib` / `staticlib` / `plugin`: `-o` names the
    // shared or static library.
    let (out, shared) = if a.cdylib || a.plugin {
        let so = out.unwrap_or_else(|| {
            create_dir(&target);
            target.join(shared_file(a, &lib_name(pkg)))
        });
        (default(), Some(so))
    } else if a.staticlib {
        let st = out.unwrap_or_else(|| {
            let dir = target.join("static");
            create_dir(&dir);
            dir.join(a.spec().staticlib(&lib_name(pkg)))
        });
        (default(), Some(st))
    } else {
        (out.unwrap_or_else(default), None)
    };
    let key = shared.as_ref().unwrap_or(&out);
    build_fresh(a, pkg, fingerprint(a, pkg, key), |a, result| {
        if !a.quiet {
            term::status("Building", &key.display().to_string());
        }
        let opts = unit_opts(a, pkg, &unit_name(pkg));
        let counts = fail_on(result.archive(&opts, &out), &result.source_map);
        match &shared {
            Some(so) if a.plugin => {
                fail_on(result.plugin(&opts, &out, so), &result.source_map);
                if let Some(text) = result.hot_layout() {
                    let l = nabic_interface::hot_layout_path(so);
                    if let Err(e) = std::fs::write(&l, text) {
                        fail(&format!("cannot write {}: {e}", l.display()));
                    }
                }
            }
            Some(st) if a.staticlib => {
                let needs = fail_on(result.staticlib(&opts, &out, st), &result.source_map);
                let dir = write_header(result, &opts, &target, pkg);
                let map = module_map(&lib_name(pkg), &needs);
                let m = dir.join("module.modulemap");
                if let Err(e) = std::fs::write(&m, map) {
                    fail(&format!("cannot write {}: {e}", m.display()));
                }
                if a.xcframework {
                    let xc = root_out_dir(a, pkg).join(format!("{}.xcframework", lib_name(pkg)));
                    if let Err(e) = crate::xcframework::add(&xc, a.spec(), st, &dir) {
                        fail(&e);
                    }
                    if !a.quiet {
                        term::status("Adding", &format!("{} to {}", a.spec().name, xc.display()));
                    }
                }
            }
            Some(so) => {
                fail_on(result.shared(&opts, &out, so), &result.source_map);
                write_header(result, &opts, &target, pkg);
            }
            None => {}
        }
        report_units(a, counts);
    });
    out
}

/// C code includes `<target>/include/<name>.h`; the directory.
fn write_header(
    result: &nabic_interface::CompileResult,
    opts: &nabic_interface::BuildOptions,
    target: &Path,
    pkg: &Package,
) -> PathBuf {
    let header = fail_on(result.emit(EmitKind::CHeader, opts), &result.source_map);
    let dir = target.join("include");
    create_dir(&dir);
    let h = dir.join(format!("{}.h", lib_name(pkg)));
    if let Err(e) = std::fs::write(&h, header) {
        fail(&format!("cannot write {}: {e}", h.display()));
    }
    dir
}

/// The clang module map beside the header of static library `name`:
/// Swift says `import <name>`, and links the library and what it needs
/// (`needs`: system libraries, frameworks `true`).
#[must_use]
pub fn module_map(name: &str, needs: &[(String, bool)]) -> String {
    let mut m = format!("module {name} {{\n    header \"{name}.h\"\n    link \"{name}\"\n");
    for (n, framework) in needs {
        let kind = if *framework { "framework " } else { "" };
        m.push_str(&format!("    link {kind}\"{n}\"\n"));
    }
    m.push_str("    export *\n}\n");
    m
}

/// The shared library file of library `name` for the build's target:
/// `lib<name>.so`, `lib<name>.dylib` or `<name>.dll`.
pub fn shared_file(a: &Args, name: &str) -> String {
    let t = a.spec();
    match t.object_format {
        nabic_target::ObjectFormat::MachO => format!("lib{name}.dylib"),
        nabic_target::ObjectFormat::Coff => format!("{name}.dll"),
        _ => format!("lib{name}.so"),
    }
}

/// `-v`: per-stage timing.
pub fn stage(a: &Args, what: &str, since: Instant) {
    if a.verbose > 0 && !a.quiet {
        eprintln!("   Stage: {what} ({}ms)", since.elapsed().as_millis());
    }
}

pub fn finished(a: &Args, started: Instant) {
    if !a.quiet {
        term::status(
            "Finished",
            &format!("in {:.2}s", started.elapsed().as_secs_f64()),
        );
    }
}

/// `r`'s value, or report its error and exit 1.
pub fn fail_on<T>(r: Result<T, nabic_interface::NabiDiagnostic>, sources: &SourceMap) -> T {
    r.unwrap_or_else(|d| {
        sources.report(&d);
        process::exit(1)
    })
}

/// `<target>/[<triple>/]<profile>/`: `release` at `-O2`+ or `-Os`, else
/// `debug`. Without a package, `<target>` is `lone_target_dir`.
/// [`out_dir`] without the target: what every target's build shares.
fn root_out_dir(a: &Args, pkg: &Package) -> PathBuf {
    a.target_dir
        .clone()
        .unwrap_or_else(|| pkg.target_dir())
        .join(if a.opts.release() { "release" } else { "debug" })
}

pub fn out_dir(a: &Args, pkg: Option<&Package>) -> PathBuf {
    let mut dir = a
        .target_dir
        .clone()
        .or_else(|| pkg.map(Package::target_dir))
        .or_else(|| std::env::var_os("NABI_TARGET_DIR").map(PathBuf::from))
        .unwrap_or_else(lone_target_dir);
    if a.target.is_some() {
        dir.push(&a.spec().name);
    }
    dir.join(if a.opts.release() { "release" } else { "debug" })
}

/// `$TMPDIR/nabic-target-<uid>` (Windows: `%TEMP%\nabic-target`), the
/// target directory of lone files.
///
/// A cache, private to the user, as it runs what it finds. Once a day,
/// files nobody wrote for a week go (`evict`).
pub fn lone_target_dir() -> PathBuf {
    let dir = nabic_core::tmp::user_cache_dir("nabic-target")
        .unwrap_or_else(|e| fail(&format!("{e}; remove it or set NABI_TARGET_DIR")));
    let stamp = dir.join(".evicted");
    let day = std::time::Duration::from_secs(86_400);
    let due = std::fs::metadata(&stamp)
        .and_then(|m| m.modified())
        .map_or(true, |t| t.elapsed().map_or(true, |a| a > day));
    // Rewriting an empty file leaves its time on Windows: set it.
    let stamped =
        || std::fs::File::create(&stamp).and_then(|f| f.set_modified(std::time::SystemTime::now()));
    if due && std::fs::create_dir_all(&dir).is_ok() && stamped().is_ok() {
        nabic_core::tmp::evict(&dir, 7 * day);
    }
    dir
}

/// The file name of executable `name` for the build's target.
pub fn exe_file(a: &Args, name: &str) -> String {
    format!("{name}{}", a.spec().exe_suffix)
}

/// `target/<profile>/<name>` (`.exe`, `.wasm`), `lib<name>.a` + `.nabihir`
/// for a library (a package with both builds both; `-o` names the
/// executable), and `build/<name>.<ext>` for `--emit`.
pub fn cmd_build(mut a: Args) {
    let mut pkg = discover_arg(&a);
    if a.hot && !pkg.lib {
        hot_root(&mut pkg);
    }
    prebuild(&mut a, Some(&pkg)).unwrap_or_else(|e| fail(&e));
    let target = out_dir(&a, Some(&pkg));
    if a.plugin && pkg.library_target().is_some() {
        option_error(
            "`--crate-type=plugin` builds a plugin, and this package also has `src/main.nabi`",
            "a plugin is loaded by a program: move `src/main.nabi` to a package of its own that \
             loads this one with `std::plugin::load`",
        );
    }
    if a.plugin && !pkg.lib {
        option_error(
            "`--crate-type=plugin` builds a library, and this package has no `src/lib.nabi`",
            "put what the program calls in `src/lib.nabi` as `pub fn`s; the program gets them \
             with `plugin.get::<fn(..) -> ..>(\"name::item\")`",
        );
    }
    if a.xcframework && !a.staticlib {
        option_error(
            "`--xcframework` puts a static library into an XCFramework, and this build makes none",
            "write `--crate-type=staticlib --xcframework`",
        );
    }
    if a.xcframework && a.spec().vendor != "apple" {
        option_error(
            format!(
                "`--xcframework` is for Apple targets, not {}",
                a.spec().name
            ),
            "build for `aarch64-apple-ios`, `aarch64-apple-ios-sim` or `aarch64-apple-darwin`",
        );
    }
    if a.staticlib && !pkg.lib && pkg.library_target().is_none() {
        option_error(
            "`--crate-type=staticlib` builds a library, and this package has no `src/lib.nabi`",
            "put the functions C calls in `src/lib.nabi`, marked `#[export_name(\"name\")] extern \
             \"C\" fn`",
        );
    }
    if a.cdylib && !pkg.lib && pkg.library_target().is_none() {
        option_error(
            "`--crate-type=cdylib` builds a library, and this package has no `src/lib.nabi`",
            "put the functions C calls in `src/lib.nabi`, marked `#[export_name(\"name\")] extern \
             \"C\" fn`",
        );
    }
    if pkg.lib && a.emit.is_empty() {
        build_lib(&a, &pkg, a.output.clone());
        return;
    }
    mount_library(&mut a, &pkg);
    if !a.emit.is_empty() {
        let (result, _, started) = compile(&a, &pkg, Mode::Build);
        if a.output.is_some() && a.emit.len() > 1 {
            option_error(
                "`-o` with several `--emit` kinds",
                "give one kind with `-o` (`--emit=mir -o out.mir`), or leave out `-o` to write \
                 each kind under the target directory",
            );
        }
        for &kind in &a.emit {
            let t = Instant::now();
            let bytes = fail_on(result.emit(kind, &a.opts), &result.source_map);
            let out = a.output.clone().unwrap_or_else(|| {
                let build = target.join("build");
                create_dir(&build);
                build.join(format!("{}.{}", pkg.name, kind.extension()))
            });
            if let Err(e) = std::fs::write(&out, bytes) {
                fail(&format!("cannot write {}: {e}", out.display()));
            }
            stage(&a, &format!("emit {}", kind.extension()), t);
            if !a.quiet {
                term::status("Wrote", &out.display().to_string());
            }
        }
        finished(&a, started);
        return;
    }
    // A lone file's executable goes beside it, built once with `run`'s.
    if pkg.anonymous && a.output.is_none() {
        let exe = script_exe(&a, &pkg);
        let dest = pkg.root.with_file_name(exe_file(&a, &pkg.name));
        place(&a, &pkg, &exe, &dest);
        for f in also(&a, &pkg, &exe) {
            place(
                &a,
                &pkg,
                &f,
                &dest.with_file_name(f.file_name().unwrap_or_default()),
            );
        }
        if !a.quiet {
            term::status(
                "Built",
                &format!("{} (use -o to put it elsewhere)", shown(&dest)),
            );
        }
        return;
    }
    let out = a.output.clone().unwrap_or_else(|| {
        create_dir(&target);
        target.join(exe_file(&a, &pkg.name))
    });
    build_fresh(&a, &pkg, fingerprint(&a, &pkg, &out), |a, result| {
        if !a.quiet {
            term::status("Building", &out.display().to_string());
        }
        link_exe(a, &pkg, result, &out, &pkg.name);
    });
}

/// Build unless up to date; a fresh build reprints its warnings.
pub fn build_fresh(
    a: &Args,
    pkg: &Package,
    fp: Fingerprint,
    link: impl FnOnce(&Args, &CompileResult),
) {
    if let Some(warnings) = fp.fresh() {
        if !a.quiet {
            term::status("Fresh", &format!("{} ({})", pkg.name, pkg.dir.display()));
        }
        print_rendered(&warnings);
        return;
    }
    fp.clear();
    let (result, warnings, started) = compile(a, pkg, Mode::Build);
    let t = Instant::now();
    // Codegen and link warnings are replayed too.
    let ((), more) = nabic_interface::capture_warnings(|| link(a, &result));
    fp.save(&result.files, &(warnings + &more));
    stage(a, "codegen + link", t);
    finished(a, started);
}

/// Codegen-unit build options reusing unchanged units' objects;
/// a lone file gets its own directory under the target dir.
pub fn unit_opts(a: &Args, pkg: &Package, unit: &str) -> BuildOptions {
    let mut opts = a.opts.clone();
    opts.incremental = Some(if pkg.anonymous {
        let file = pkg.root.to_string_lossy();
        let hash = nabic_core::hash::fnv(file.as_bytes());
        out_dir(a, None)
            .join("incremental")
            .join(format!("{unit}-{hash:016x}"))
    } else {
        out_dir(a, Some(pkg)).join("incremental").join(unit)
    });
    // A hot build's frames and GC tables are the whole module's.
    if a.hot {
        opts.incremental = None;
    }
    opts
}

/// `-v`: how many units a build reused and compiled; `-vv`: which.
pub fn report_units(a: &Args, counts: Option<nabic_interface::UnitCounts>) {
    let Some(c) = counts.filter(|_| a.verbose > 0 && !a.quiet) else {
        return;
    };
    term::status(
        "Units",
        &format!("{} reused, {} compiled", c.reused, c.compiled.len()),
    );
    if a.verbose > 1 {
        for u in &c.compiled {
            term::status("Compiled", &format!("unit {u}"));
        }
    }
}

/// Link `result`'s executable at `out`, by codegen units (`unit_opts`).
pub fn link_exe(a: &Args, pkg: &Package, result: &CompileResult, out: &Path, unit: &str) {
    let opts = unit_opts(a, pkg, unit);
    let counts = fail_on(result.link(&opts, out), &result.source_map);
    report_units(a, counts);
}

/// The incremental-build key of `pkg` built into `out` (see `Fingerprint`).
pub fn fingerprint(a: &Args, pkg: &Package, out: &Path) -> Fingerprint {
    let mut words = vec![
        format!("{:?}", a.opts),
        format!("lints {:?}", a.lint_levels),
        format!("target {:016x}", a.spec().hash()),
        out.display().to_string(),
        pkg.root.display().to_string(),
        std::env::var("PATH").unwrap_or_default(),
        std::env::var("NABI_CC").unwrap_or_default(),
    ];
    words.extend(a.cfg.iter().cloned());
    // What each package sees and enables.
    words.extend(a.extern_deps.iter().map(|(d, n)| format!("dep {d}={n}")));
    words.extend((a.extern_features.iter()).map(|(d, f)| format!("feature {d}={f}")));
    words.extend(pkg.lib.then(|| lib_name(pkg)));
    words.extend(a.cdylib.then(|| "cdylib".to_owned()));
    words.extend(a.staticlib.then(|| "staticlib".to_owned()));
    words.extend(a.plugin.then(|| format!("plugin {:?}", a.bundle)));
    // A package's sources are its `src/`.
    let mut inputs = vec![if pkg.anonymous {
        pkg.root.clone()
    } else {
        pkg.root
            .parent()
            .map_or_else(|| pkg.dir.clone(), Path::to_path_buf)
    }];
    let mut stamped = vec![PathBuf::from(&a.opts.nabir)];
    // Shipped C runtime objects, libc and linker for the target.
    if let Some(dir) = &a.opts.libc {
        let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_file())
            .collect();
        files.sort();
        stamped.extend(files);
    }
    for b in &a.bootstrap {
        words.push(b.display().to_string());
        inputs.push(b.parent().map_or_else(|| b.clone(), Path::to_path_buf));
    }
    for (n, p) in &a.externs {
        words.push(format!("{n}={}", p.display()));
        let (nabihir, archive) = nabic_interface::nabihir::companions(p);
        inputs.push(nabihir);
        stamped.push(archive);
    }
    // Shared libraries are copied beside the executable; a missing copy
    // rebuilds.
    stamped.extend(a.opts.runtime_files.iter().cloned());
    stamped.extend(a.opts.objects.iter().cloned());
    Fingerprint::new(out, also(a, pkg, out), &words, &inputs, &stamped)
}

/// Files a build makes besides `out` (NABIHIR, copied shared libraries).
pub fn also(a: &Args, pkg: &Package, out: &Path) -> Vec<PathBuf> {
    if pkg.lib {
        vec![nabic_interface::nabihir::companions(out).0]
    } else {
        let dir = out.parent().unwrap_or_else(|| Path::new("."));
        a.opts
            .runtime_files
            .iter()
            .map(|f| dir.join(f.file_name().unwrap_or_default()))
            .collect()
    }
}

/// Lone-file executables unused this long go when another is built.
pub const SCRIPT_KEPT: std::time::Duration = std::time::Duration::from_secs(24 * 3600);

/// Lone file `pkg`'s executable, cached at `scripts/<key>/<name>`.
///
/// `<key>` hashes everything the build reads. A
/// hit compiles nothing and reprints the warnings. Racing runs build one
/// at a time, under a lock named after the file's units, and a waiter finds
/// the build fresh; linking to a scratch name and renaming keeps a
/// running executable safe.
pub fn script_exe(a: &Args, pkg: &Package) -> PathBuf {
    let root = out_dir(a, None).join("scripts");
    let name = exe_file(a, &pkg.name);
    let fp = fingerprint(a, pkg, Path::new(&name));
    let dir = root.join(fp.key());
    let exe = dir.join(&name);
    let fp = fp.moved_to(&exe, also(a, pkg, &exe));
    let units = unit_opts(a, pkg, &pkg.name).incremental.unwrap_or_default();
    let mut lock = units.file_name().unwrap_or_default().to_os_string();
    lock.push(".lock");
    let lock = out_dir(a, None).join("locks").join(lock);
    let _held = nabic_core::lock_file(&lock)
        .unwrap_or_else(|e| fail(&format!("cannot lock {}: {e}", lock.display())));
    let mut built = false;
    build_fresh(a, pkg, fp, |a, result| {
        create_dir(&dir);
        let tmp = dir.join(format!(".{name}.{}", process::id()));
        link_exe(a, pkg, result, &tmp, &pkg.name);
        // Windows can't replace a running executable; if one is there, it
        // is this one.
        if std::fs::rename(&tmp, &exe).is_err() && exe.is_file() {
            let _ = std::fs::remove_file(&tmp);
        }
        // Another run's `.dSYM` is as good.
        let dsym = |p: &Path| {
            let name = p.file_name().unwrap_or_default().to_string_lossy();
            p.with_file_name(format!("{name}.dSYM"))
        };
        if dsym(&tmp).exists() && std::fs::rename(dsym(&tmp), dsym(&exe)).is_err() {
            let _ = std::fs::remove_dir_all(dsym(&tmp));
        }
        built = true;
    });
    let _ = std::fs::write(dir.join("used"), "");
    if built {
        sweep_scripts(&root, &dir);
    }
    exe
}

/// Copy `from` to `to` for a lone file's build.
///
/// A file at `to` that an
/// earlier build did not put there stays: the build fails and names `-o`.
/// A record per destination in the target directory's `placed/` holds
/// the size and time of what was written.
pub fn place(a: &Args, pkg: &Package, from: &Path, to: &Path) {
    let root = a
        .target_dir
        .clone()
        .or_else(|| std::env::var_os("NABI_TARGET_DIR").map(PathBuf::from))
        .unwrap_or_else(lone_target_dir);
    let abs = std::path::absolute(to).unwrap_or_else(|_| to.to_path_buf());
    let h = nabic_core::hash::fnv(abs.to_string_lossy().as_bytes());
    let record = root.join("placed").join(format!("{h:016x}"));
    let stamp = |p: &Path| {
        let m = std::fs::symlink_metadata(p)
            .ok()
            .filter(std::fs::Metadata::is_file)?;
        Some(format!("{} {:?}", m.len(), m.modified().ok()?))
    };
    if std::fs::symlink_metadata(to).is_ok() && std::fs::read_to_string(&record).ok() != stamp(to) {
        fail(&format!(
            "{} is in the way: it was not built from {}; to keep it, write the executable \
             elsewhere: `{} build {} -o <file>`",
            shown(to),
            shown(&pkg.root),
            cli::invoked_as(),
            shown(&pkg.root),
        ));
    }
    // A scratch copy renamed into place: a running executable stays.
    let name = to.file_name().unwrap_or_default().to_string_lossy();
    let tmp = to.with_file_name(format!(".{name}.{}", process::id()));
    if let Err(e) = std::fs::copy(from, &tmp).and_then(|_| std::fs::rename(&tmp, to)) {
        let _ = std::fs::remove_file(&tmp);
        fail(&format!("cannot write {}: {e}", shown(to)));
    }
    if let Some(s) = stamp(to) {
        create_dir(&root.join("placed"));
        let _ = std::fs::write(&record, s);
    }
}

/// `p` relative to the current directory when under it.
pub fn shown(p: &Path) -> String {
    let rel = std::env::current_dir().ok().and_then(|cwd| {
        let cwd = nabic_core::real_path(cwd).ok()?;
        let p = nabic_core::real_path(p).ok()?;
        p.strip_prefix(&cwd).ok().map(Path::to_path_buf)
    });
    rel.filter(|r| !r.as_os_str().is_empty())
        .unwrap_or_else(|| p.to_path_buf())
        .display()
        .to_string()
}

/// Remove the entries of `root` but `keep` unused for `SCRIPT_KEPT`.
pub fn sweep_scripts(root: &Path, keep: &Path) {
    for e in std::fs::read_dir(root).into_iter().flatten().flatten() {
        let p = e.path();
        let old = std::fs::metadata(p.join("used"))
            .or_else(|_| e.metadata())
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > SCRIPT_KEPT);
        if p != keep && old {
            let _ = std::fs::remove_dir_all(&p);
        }
    }
}

/// `--hot` on a program: its `src/main.nabi` is built as the root
/// plugin, `main` exported as `<name>::main` for the dev host to call.
fn hot_root(pkg: &mut Package) {
    if pkg.library.is_some() || pkg.anonymous {
        option_error(
            "`--hot` builds one root file, and this package has both `src/main.nabi` and \
             `src/lib.nabi`",
            "move the library into a package of its own (a path dependency, hot too), or its \
             items into modules of `src/main.nabi`",
        );
    }
    pkg.lib = true;
}

/// A plugin needs a target that loads code at run time.
pub fn plugin_target(a: &Args) {
    let t = a.spec();
    if t.object_format == nabic_target::ObjectFormat::Wasm {
        option_error(
            format!(
                "`--crate-type=plugin` needs a native target, and {} cannot load code at run time",
                t.name
            ),
            "on wasm, build one program: make the plugin's package an ordinary dependency of the \
             program",
        );
    }
    if nabic_target::apple::Platform::of(t) == Some(nabic_target::apple::Platform::Ios) {
        option_error(
            format!(
                "`--crate-type=plugin` and `--hot` load code at run time, and an iOS device loads \
                 only code signed into the app ({})",
                t.name
            ),
            "load plugins and reload code on the simulator: `--target aarch64-apple-ios-sim`",
        );
    }
    if t.libc == nabic_target::Libc::Musl {
        option_error(
            format!(
                "`--crate-type=plugin`: {} links its C library statically and cannot load plugins",
                t.name
            ),
            format!(
                "build plugins and the program for `{}`, or build one program",
                t.name.replace("-musl", "-gnu")
            ),
        );
    }
}
