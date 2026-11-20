//! The `test` and `bench` harness builds, which `nabi` runs.

#[allow(clippy::wildcard_imports)]
use crate::*;
use nabic_harness::test::{Case, Settled};

/// `[path] [filter]`: a lone argument that isn't an existing path is the
/// filter.
pub fn path_and_filter(a: &Args) -> (&str, Option<&str>) {
    match a.positional.as_slice() {
        [] => (".", None),
        [p] if Path::new(p).exists() => (p.as_str(), None),
        [f] => (".", Some(f.as_str())),
        [p, f] => (p.as_str(), Some(f.as_str())),
        _ => usage_error(&format!("usage: nabic {} [path] [filter]", a.cmd)),
    }
}

/// `nabic test` / `nabic bench [path] [filter]` (`-O2` unless given): build
/// the harness and write the manifest `nabi` runs (`--emit-harness`).
pub fn cmd_harness(mut a: Args, mode: Mode) {
    if mode == Mode::Bench && !a.opt_set {
        "2".clone_into(&mut a.opts.opt_level);
        set_overflow_checks(&mut a);
    }
    let (path, filter) = path_and_filter(&a);
    let filter = filter.map(str::to_owned);
    let pkg = discover_at(&a, path);
    prebuild(&mut a, Some(&pkg)).unwrap_or_else(|e| fail(&e));
    // `nabic` builds the harness; the `nabi` runner runs it. A
    // package with both roots builds the library's first, the
    // executable's second. Each goes into the target directory so it
    // survives this process, and its path and cases go to the manifest.
    // Doc tests come last, on the library: `--doc` only those, `--no-doc`
    // none.
    let mut harnesses: Vec<(PathBuf, Vec<Case>)> = Vec::new();
    let lib = if pkg.lib {
        Some(pkg.clone())
    } else {
        pkg.library_target()
    };
    let mut lib_files = Vec::new();
    if a.doc_tests != Some(true) {
        for target in pkg.library_target().into_iter().chain([pkg.clone()]) {
            mount_library(&mut a, &target);
            let (exe, cases, files) = build_harness(&a, &target, mode);
            if target.lib {
                lib_files = files;
            }
            harnesses.push((exe, cases));
        }
    }
    if mode == Mode::Test && a.doc_tests != Some(false) {
        if let Some(lib) = lib.filter(|_| !pkg.anonymous) {
            if lib_files.is_empty() {
                lib_files = nabic_interface::source_files(&lib.dir.join("src"));
            }
            harnesses.extend(crate::doctest::build(&mut a, &lib, &lib_files));
        }
    }
    write_harness_manifest(&a, mode, filter.as_deref(), &harnesses);
}

/// Build `pkg`'s test or bench harness into the target directory; its
/// executable, the cases in it and the source files it read.
pub fn build_harness(a: &Args, pkg: &Package, mode: Mode) -> (PathBuf, Vec<Case>, Vec<PathBuf>) {
    let kind = if mode == Mode::Bench { "bench" } else { "test" };
    let (result, _, started) = compile(a, pkg, mode);
    let unit = format!("{}-{kind}", unit_name(pkg));
    let dir = out_dir(a, Some(pkg)).join("harness");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        fail(&format!("{}: {e}", dir.display()));
    }
    let exe = dir.join(exe_file(a, &unit));
    let t = Instant::now();
    link_exe(a, pkg, &result, &exe, &unit);
    stage(a, "codegen + link", t);
    finished(a, started);
    let cases = result
        .tests
        .iter()
        .map(|t| Case {
            name: t.name.clone(),
            ignore: t.ignore.clone(),
            panics: t.panics.clone(),
            ..Case::default()
        })
        .collect();
    (exe, cases, result.files)
}

/// Write the harness manifest `nabi` runs.
///
/// Its kind and filter, the runner a non-host target's executables need
/// (`runner` words, or `norunner` and why none was found), then each
/// executable with its cases. Without `--emit-harness`, name the built
/// executables for a human instead.
pub fn write_harness_manifest(
    a: &Args,
    mode: Mode,
    filter: Option<&str>,
    harnesses: &[(PathBuf, Vec<Case>)],
) {
    let Some(path) = a.emit_harness.as_ref() else {
        if !a.quiet {
            for (exe, _) in harnesses.iter().filter(|(e, _)| !e.as_os_str().is_empty()) {
                term::status("Built", &exe.display().to_string());
            }
            eprintln!(
                "{} run the harness with `nabi {}`",
                term::label(Stream::Stderr, Role::Note),
                a.cmd
            );
        }
        return;
    };
    // `nabic_harness::VERSION`; a newer runner reads older formats.
    let mut m = String::from("version 2\n");
    m.push_str(if mode == Mode::Bench {
        "kind bench\n"
    } else {
        "kind test\n"
    });
    // The filter is raw to end of line, so it may hold spaces.
    if let Some(f) = filter {
        m.push_str(&format!("filter {f}\n"));
    }
    // One line per word: a word may hold spaces.
    match runner(a.spec()) {
        Ok(words) => {
            for w in words {
                m.push_str(&format!("runner {}\n", w.to_string_lossy()));
            }
        }
        Err(e) => m.push_str(&format!("norunner {}\n", e.replace('\n', " "))),
    }
    for (exe, cases) in harnesses {
        // Absolute, so the runner finds it from any working directory;
        // empty when every case is settled.
        if exe.as_os_str().is_empty() {
            m.push_str("exe\n");
        } else {
            let exe = std::path::absolute(exe).unwrap_or_else(|_| exe.clone());
            m.push_str(&format!("exe {}\n", exe.display()));
        }
        for c in cases {
            m.push_str(&format!("case {}\n", c.name));
            // One line each: `\` and newlines escaped.
            let one_line = |s: &str| s.replace('\\', "\\\\").replace('\n', "\\n");
            if let Some(call) = &c.call {
                m.push_str(&format!("call {call}\n"));
            }
            if let Some(why) = &c.ignore {
                m.push_str(&format!("ignore {}\n", one_line(why)));
            }
            if let Some(text) = &c.panics {
                m.push_str(&format!("panic {}\n", one_line(text)));
            }
            match &c.outcome {
                Some(Settled::Passed) => m.push_str("outcome ok\n"),
                Some(Settled::Failed(why)) => {
                    m.push_str(&format!("outcome failed {}\n", one_line(why)));
                }
                Some(Settled::Ignored(why)) => {
                    m.push_str(&format!("outcome ignored {}\n", one_line(why)));
                }
                None => {}
            }
        }
    }
    if let Err(e) = std::fs::write(path, m) {
        fail(&format!("{}: {e}", path.display()));
    }
}
