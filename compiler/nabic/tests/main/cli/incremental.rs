//! Fresh builds, codegen units and incremental reuse.

#[allow(clippy::wildcard_imports)]
use crate::cli::*;

/// An unchanged package is not rebuilt; a changed source, a
/// different option or a replaced output rebuilds it.
#[test]
fn unchanged_package_is_fresh() {
    let env = setup();
    let dir = scratch("fresh");
    let app = new_package(&dir, "app", false);
    let built = |args: &[&str]| {
        let out = nabic(Some(&env), &app, args);
        assert_exit(&out, 0);
        let stderr = text(&out.stderr);
        assert!(
            stderr.contains("Fresh app") != stderr.contains("Compiling app"),
            "{stderr}"
        );
        (stderr.contains("Compiling app"), text(&out.stdout))
    };
    // A build, then the executable's output.
    let ran = || {
        let (compiled, _) = built(&["build"]);
        let exe = app.join("target/debug").join(exe_name("app"));
        (compiled, text(&Command::new(exe).output().unwrap().stdout))
    };
    assert!(built(&["build"]).0);
    assert_eq!(ran(), (false, "Hello, world!\n".to_owned()));
    put(app.join("src/main.nabi"), "println(\"changed\");\n").unwrap();
    assert_eq!(ran(), (true, "changed\n".to_owned()));
    assert!(!built(&["build"]).0);
    // A debug build has debug info already.
    assert!(!built(&["build", "-g"]).0);
    let other = ["build", "-C", "overflow-checks=no"];
    assert!(built(&other).0);
    assert!(!built(&other).0);
    // A file the package does not use is still one of its sources.
    put(app.join("src/other.nabi"), "fn f() {}\n").unwrap();
    assert!(built(&other).0);
    put(app.join("target/debug").join(exe_name("app")), "").unwrap();
    assert!(built(&other).0);
    // A `#[path]` module outside the package is a source too.
    put(dir.join("shared.nabi"), "pub fn v() -> i32 {\n    1\n}\n").unwrap();
    put(
        app.join("src/main.nabi"),
        "#[path(\"../../shared.nabi\")]\nmod shared;\n\nprintln(f\"{shared::v()}\");\n",
    )
    .unwrap();
    assert_eq!(ran(), (true, "1\n".to_owned()));
    assert_eq!(ran(), (false, "1\n".to_owned()));
    put(dir.join("shared.nabi"), "pub fn v() -> i32 {\n    2\n}\n").unwrap();
    assert_eq!(ran(), (true, "2\n".to_owned()));
}

/// A lone file is linked again when the runtime changes.
#[test]
fn runtime_change_relinks_script() {
    let env = setup();
    let dir = scratch("script-runtime");
    let nabir = dir.join("rt").join(env.nabir.file_name().unwrap());
    fs::create_dir_all(nabir.parent().unwrap()).unwrap();
    fs::copy(&env.nabir, &nabir).unwrap();
    put(dir.join("s.nabi"), "println(\"hi\");\n").unwrap();
    let run = || {
        let out = Command::new(NABIC)
            .current_dir(&dir)
            .env("NABI_HOME", workspace())
            .args(["--target-dir", "t", "build", "s.nabi", "--nabir"])
            .arg(&nabir)
            .output()
            .unwrap();
        assert_exit(&out, 0);
        let exe = dir.join(exe_name("s"));
        assert_eq!(text(&Command::new(exe).output().unwrap().stdout), "hi\n");
        text(&out.stderr).contains("Compiling s")
    };
    assert!(run());
    assert!(!run());
    // A rebuilt runtime: same bytes, a new file.
    fs::remove_file(&nabir).unwrap();
    fs::copy(&env.nabir, &nabir).unwrap();
    assert!(run());
    assert!(!run());
}

/// The `debug` profile has debug info and symbols; the `release`
/// one neither (unless `-g`), and a panic still names its location.
#[test]
fn profiles_differ() {
    let env = setup();
    if TargetSpec::host().object_format == ObjectFormat::Coff {
        return; // a PE executable has no symbol table either way
    }
    let dir = scratch("profiles");
    let app = new_package(&dir, "app", false);
    put(
        app.join("src/main.nabi"),
        "use std::env;\n#[inline(never)]\nfn distinctive_name(n: i64) -> i64 {\n    if n > 2 {\n        panic(\"too big\");\n    }\n    \
         n\n}\n\nfn main() {\n    println(f\"{distinctive_name(env::args().len() as i64)}\");\n}\n",
    )
    .unwrap();
    let exe = |profile: &str| app.join("target").join(profile).join(exe_name("app"));
    let named = |p: &Path| {
        let b = fs::read(p).unwrap();
        b.windows(13).any(|w| w == b"distinctive_0")
    };
    assert_exit(&nabic(Some(&env), &app, &["build"]), 0);
    assert!(named(&exe("debug")));
    let mut dsym = exe("debug").into_os_string();
    dsym.push(".dSYM");
    let debug_info = if cfg!(target_vendor = "apple") {
        Path::new(&dsym).is_dir()
    } else {
        let b = fs::read(exe("debug")).unwrap();
        b.windows(11).any(|w| w == b".debug_info")
    };
    assert!(debug_info, "no debug info in the debug profile");
    assert_exit(&nabic(Some(&env), &app, &["build", "--release"]), 0);
    assert!(!named(&exe("release")));
    let out = Command::new(exe("release"))
        .args(["a", "b"])
        .output()
        .unwrap();
    assert!(
        text(&out.stderr).contains("panicked at src/main.nabi:5:9:\ntoo big"),
        "{}",
        text(&out.stderr)
    );
    assert_exit(&nabic(Some(&env), &app, &["build", "--release", "-g"]), 0);
    assert!(named(&exe("release")));
    let out = nabic(Some(&env), &app, &["build", "--debug"]);
    assert_exit(&out, 2);
    assert!(text(&out.stderr).contains("unknown option `--debug`"));
}

/// A file `include_str!` / `include_bytes!` reads is an input: editing
/// it rebuilds the package, parses the including file and lowers its
/// body again, and recompiles its unit alone.
#[test]
fn included_file_is_a_build_input() {
    let env = setup();
    let dir = scratch("include-input");
    let app = dir.join("app");
    fs::create_dir_all(app.join("res")).unwrap();
    put(
        app.join("src/main.nabi"),
        "mod a;\nmod b;\n\nfn main() {\n    println(f\"{a::text()} {a::BYTES.len()} {b::two()}\");\n}\n",
    )
    .unwrap();
    put(
        app.join("src/a.nabi"),
        "pub const BYTES: [u8; 2] = include_bytes!(\"res/b.bin\");\n\n\
         pub fn text() -> str {\n    include_str!(\"res/t.txt\")\n}\n",
    )
    .unwrap();
    put(app.join("src/b.nabi"), "pub fn two() -> i64 {\n    2\n}\n").unwrap();
    put(app.join("src/res/t.txt"), "one").unwrap();
    put(app.join("src/res/b.bin"), [0u8, 255]).unwrap();
    // (rebuilt, files parsed afresh, units compiled, output).
    let build = || {
        let out = nabic(Some(&env), &app, &["build", "-vv"]);
        assert_exit(&out, 0);
        let stderr = text(&out.stderr);
        let parsed = stderr
            .lines()
            .find_map(|l| l.trim().strip_prefix("Reused: "))
            .map(|l| {
                let n: Vec<usize> = l
                    .split(|c: char| !c.is_ascii_digit())
                    .filter_map(|w| w.parse().ok())
                    .collect();
                n[1] - n[0]
            });
        let mut units: Vec<String> = stderr
            .lines()
            .filter_map(|l| l.trim().strip_prefix("Compiled unit "))
            .map(str::to_owned)
            .collect();
        units.sort();
        let run = build_run(Some(&env), &app, &["-q"]);
        assert_exit(&run, 0);
        (parsed, units, text(&run.stdout))
    };
    assert_eq!(build().2, "one 2 2\n");
    let (parsed, units, out) = build();
    assert_eq!((parsed, units.len(), out.as_str()), (None, 0, "one 2 2\n"));
    put(app.join("src/res/t.txt"), "two").unwrap();
    assert_eq!(
        build(),
        (Some(1), vec!["a".to_owned()], "two 2 2\n".to_owned())
    );
    put(app.join("src/res/b.bin"), b"").unwrap();
    let out = nabic(Some(&env), &app, &["build"]);
    assert!(
        text(&out.stderr).contains("expects `[u8; 2]`, found `[u8; 0]`"),
        "{}",
        text(&out.stderr)
    );
}

/// An edit recompiles only the units it changed (and callers' units for a
/// signature; above `-O0`, those it was inlined into); the result is a
/// clean build's, byte for byte.
#[test]
fn edited_module_recompiles_its_unit_alone() {
    let env = setup();
    for opt in ["-O0", "-O2"] {
        let dir = scratch(&format!("units{opt}"));
        let app = dir.join("app");
        fs::create_dir_all(&app).unwrap();
        put(
            app.join("src/main.nabi"),
            "mod a;\nmod b;\n\nfn main() -> i32 {\n    let xs: List<i64> = [b::three(), 4];\n    \
             println(f\"{a::twice(xs[0])} {b::greet()} {xs.len()}\");\n    return 0;\n}\n",
        )
        .unwrap();
        put(
            app.join("src/a.nabi"),
            "pub fn twice(x: i64) -> i64 {\n    x * 2\n}\n",
        )
        .unwrap();
        // A struct's method and a closure: their names must not move
        // when `a` gains some.
        let b = "struct B { v: i64; }\n\nimpl B {\n    fn get(self) -> i64 { self.v }\n}\n\n\
                 pub fn three() -> i64 {\n    B { v: 3 }.get()\n}\n\n\
                 pub fn greet() -> str {\n    let f = |s: str| s;\n    f(\"hi\")\n}\n";
        put(app.join("src/b.nabi"), b).unwrap();
        // The units compiled, and what the program prints.
        let build = || {
            let out = nabic(Some(&env), &app, &["build", "-vv", opt]);
            assert_exit(&out, 0);
            let stderr = text(&out.stderr);
            let mut units: Vec<String> = stderr
                .lines()
                .filter_map(|l| l.trim().strip_prefix("Compiled unit "))
                .map(str::to_owned)
                .collect();
            units.sort();
            let run = build_run(Some(&env), &app, &["-q", opt]);
            assert_exit(&run, 0);
            (units, text(&run.stdout))
        };
        let (units, out) = build();
        assert!(
            units.iter().any(|u| u == "a") && units.len() > 3,
            "{units:?}"
        );
        assert_eq!(out, "6 hi 2\n");
        // A body, and a new struct, method and closure (lowered before
        // `b`'s).
        put(
            app.join("src/a.nabi"),
            "struct A { v: i64; }\n\nimpl A {\n    fn one(self) -> i64 { self.v }\n}\n\n\
             pub fn extra() -> i64 {\n    let f = |y: i64| y * 2;\n    f(A { v: 1 }.one())\n}\n\n\
             pub fn twice(x: i64) -> i64 {\n    x * 2 + 1\n}\n",
        )
        .unwrap();
        // `-O2` inlines `twice` into `main`.
        let edited: &[&str] = if opt == "-O2" {
            &["<root>", "a"]
        } else {
            &["a"]
        };
        assert_eq!(
            build(),
            (
                edited.iter().map(|u| (*u).to_owned()).collect(),
                "7 hi 2\n".to_owned()
            ),
            "{opt}"
        );
        // A signature: its callers' unit too.
        put(
            app.join("src/b.nabi"),
            b.replace("three() -> i64", "three(k: i64) -> i64")
                .replace("B { v: 3 }.get()", "B { v: 3 }.get() * k"),
        )
        .unwrap();
        let main = fs::read_to_string(app.join("src/main.nabi")).unwrap();
        put(
            app.join("src/main.nabi"),
            main.replace("b::three()", "b::three(1)"),
        )
        .unwrap();
        let (units, out) = build();
        assert_eq!(units, ["<root>", "b"], "{opt}");
        assert_eq!(out, "7 hi 2\n");
        // A new GC type, then gone again: the type ids are a clean
        // build's.
        let a = fs::read_to_string(app.join("src/a.nabi")).unwrap();
        put(
            app.join("src/a.nabi"),
            format!(
                "{a}\npub fn rows() -> i64 {{\n    \
                 let t: List<(str, str, i64, str, i64)> = [(\"x\", \"y\", 1, \"z\", 2)];\n    \
                 t.len() as i64\n}}\n"
            ),
        )
        .unwrap();
        let main = fs::read_to_string(app.join("src/main.nabi")).unwrap();
        put(
            app.join("src/main.nabi"),
            main.replace("{xs.len()}", "{xs.len()} {a::rows()}"),
        )
        .unwrap();
        assert_eq!(build().1, "7 hi 2 1\n");
        put(app.join("src/a.nabi"), a).unwrap();
        put(app.join("src/main.nabi"), main).unwrap();
        assert_eq!(build().1, "7 hi 2\n");
        // The same executable as a clean build's.
        let exe = |name: &str| {
            let profile = if opt == "-O2" { "release" } else { "debug" };
            fs::read(app.join("target").join(profile).join(name)).unwrap()
        };
        let name = if cfg!(windows) { "app.exe" } else { "app" };
        let incremental = exe(name);
        fs::remove_dir_all(app.join("target")).unwrap();
        let (units, _) = build();
        assert!(units.len() > 3, "{units:?}");
        assert!(
            incremental == exe(name),
            "{opt}: not the clean build's executable"
        );
    }
}

/// Unchanged parses and lowerings replay from the incremental directory;
/// `NABI_INCREMENTAL_VERIFY` fails a replay that differs from a clean lowering.
#[test]
fn front_end_reuses_parses_and_bodies() {
    let env = setup();
    let dir = scratch("incr-front");
    let app = dir.join("app");
    fs::create_dir_all(&app).unwrap();
    put(
        app.join("src/main.nabi"),
        "mod m;\n\nfn pure(x: i64) -> i64 { x }\n\nfn main() {\n    \
         let a = try pure(1) catch _ { 0 };\n    println(f\"{a} {m::f(2)} {m::g()}\");\n}\n",
    )
    .unwrap();
    let m = "pub fn f(x: i64) -> i64 {\n    x * 2\n}\n\n\
             pub fn g() -> str {\n    let h = |s: str| s;\n    h(\"g\")\n}\n";
    put(app.join("src/m.nabi"), m).unwrap();
    let nabic_with = |vars: &[(&str, &str)], args: &[&str]| {
        let mut cmd = Command::new(NABIC);
        cmd.current_dir(&app)
            .env("NABI_HOME", workspace())
            .env("NABI_INCREMENTAL_VERIFY", "1")
            .envs(vars.iter().copied())
            .args(args)
            .arg("--nabir")
            .arg(&env.nabir);
        let out = cmd.output().unwrap();
        assert_exit(&out, 0);
        out
    };
    // (files reused, files, bodies replayed, bodies), the warnings, the
    // program's output.
    let build = || {
        let out = nabic_with(&[], &["check", "-v"]);
        let stderr = text(&out.stderr);
        let line = stderr
            .lines()
            .find_map(|l| l.trim().strip_prefix("Reused: "))
            .unwrap_or_else(|| panic!("{stderr}"))
            .to_owned();
        let n: Vec<usize> = line
            .split(|c: char| !c.is_ascii_digit())
            .filter_map(|w| w.parse().ok())
            .collect();
        let warnings: Vec<String> = stderr
            .lines()
            .filter(|l| l.starts_with("warning"))
            .map(str::to_owned)
            .collect();
        nabic_with(&[], &["build", "-q"]);
        let run = Command::new(app.join("target/debug").join(exe_name("app")))
            .output()
            .unwrap();
        ((n[0], n[1], n[2], n[3]), warnings, text(&run.stdout))
    };
    let clean = || {
        let out = nabic_with(&[("NABI_INCREMENTAL", "0")], &["check"]);
        let stderr = text(&out.stderr);
        let warnings: Vec<String> = stderr
            .lines()
            .filter(|l| l.starts_with("warning"))
            .map(str::to_owned)
            .collect();
        warnings
    };
    let ((files0, files, bodies0, bodies), warnings, out) = build();
    assert_eq!((files0, bodies0), (0, 0));
    assert!(files > 2 && bodies > 3, "{files} {bodies}");
    assert_eq!(out, "1 4 g\n");
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("this `try` covers no throwing call")),
        "{warnings:?}"
    );
    assert_eq!(warnings, clean());
    // A body, with a closure more: that file is parsed and that body
    // lowered again, all else is taken.
    put(
        app.join("src/m.nabi"),
        m.replace("x * 2", "let k = |y: i64| y + 1;\n    k(x * 2)"),
    )
    .unwrap();
    let (n, w, out) = build();
    assert_eq!((n.0, n.1, n.3), (files - 1, files, bodies), "{n:?}");
    assert!(n.2 + 2 >= bodies && n.2 < bodies, "{n:?}");
    assert_eq!((w, out), (clean(), "1 5 g\n".to_owned()));
    // Nothing changed: everything is taken, though a build of other
    // library roots ran between (its pre-pass summaries are its own).
    put(dir.join("bare.nabi"), "fn main() {}\n").unwrap();
    let core = workspace().join("library/core/src/lib.nabi");
    let out = Command::new(NABIC)
        .current_dir(&dir)
        .env("NABI_HOME", workspace())
        .args(["check", "bare.nabi", "--bootstrap"])
        .arg(&core)
        .output()
        .unwrap();
    assert_exit(&out, 0);
    let (n, w, _) = build();
    assert_eq!(n, (files, files, bodies, bodies));
    assert_eq!(w, warnings);
    // A signature: that function, its caller (`main`, which read it)
    // and `g` (moved) are lowered again.
    let sig = m
        .replace("-> i64 {", "-> i32 {")
        .replace("x * 2", "(x * 2) as i32");
    put(app.join("src/m.nabi"), &sig).unwrap();
    let (n, w, out) = build();
    assert_eq!((n.0, n.1, n.3, bodies - n.2), (files - 1, files, bodies, 3));
    assert_eq!((w, out), (clean(), "1 4 g\n".to_owned()));
    // Another item no body reads: every body is taken.
    let item = sig + "\npub struct S {\n    v: i64;\n}\n";
    put(app.join("src/m.nabi"), &item).unwrap();
    let (n, w, out) = build();
    assert_eq!((n.0, n.2, n.3), (files - 1, bodies, bodies), "{n:?}");
    assert_eq!((w, out), (clean(), "1 4 g\n".to_owned()));
    // A new method: it is lowered, every other body is taken.
    put(
        app.join("src/m.nabi"),
        item + "\nimpl S {\n    pub fn get(self) -> i64 {\n        self.v\n    }\n}\n",
    )
    .unwrap();
    let (n, w, out) = build();
    assert_eq!((n.0, n.3 - n.2), (files - 1, 1), "{n:?}");
    assert_eq!((w, out), (clean(), "1 4 g\n".to_owned()));
}

/// `nabic check` reads the library's items from the cached stage too.
#[test]
fn check_takes_the_library_stage() {
    let _ = setup();
    let dir = scratch("check-stage");
    put(
        dir.join("dep/src/lib.nabi"),
        "#[must_use]\npub fn token() -> i64 {\n    7\n}\n\npub trait Mark {}\n\n\
         pub fn plain() -> i64 {\n    1\n}\n",
    )
    .unwrap();
    put(
        dir.join("app/src/main.nabi"),
        "use dep::Mark;\nuse dep::plain;\nuse core::ops::Add;\n\n\
         fn main() {\n    dep::token();\n}\n",
    )
    .unwrap();
    let check = || {
        let out = Command::new(NABIC)
            .current_dir(&dir)
            .env("NABI_HOME", workspace())
            .env("NABI_TIME_PASSES", "1")
            .args(["check", "app", "--extern", "dep=dep"])
            .output()
            .unwrap();
        assert_exit(&out, 0);
        let err = text(&out.stderr);
        let lints: Vec<&str> = err.lines().filter(|l| l.starts_with("warning")).collect();
        assert_eq!(
            lints,
            [
                "warning[E0131]: unused import `dep::plain`",
                "warning[E0131]: unused result of `token`, which is `#[must_use]`",
            ],
            "{err}"
        );
        err.contains("hir library write")
    };
    // Collected and kept, then taken.
    assert!(check());
    assert!(!check());
}

/// The collected library stage is cached across processes and taken or
/// redone per edit; a corrupt store is a miss.
#[test]
fn library_stage_persists_across_builds() {
    let env = setup();
    let dir = scratch("incr-library");
    let dep = "pub const LIMIT: i64 = 3;\n\n\
               pub trait Shape {\n    fn area(self) -> i64;\n}\n\n\
               pub fn scale(x: i64) -> i64 {\n    x * LIMIT\n}\n";
    put(dir.join("dep/src/lib.nabi"), dep).unwrap();
    put(
        dir.join("app/src/main.nabi"),
        "mod m;\n\nfn main() {\n    let s = m::Sq { side: 2 };\n    \
         let w = try dep::scale(0) catch _ { 1 };\n    \
         println(f\"{m::f(1)} {dep::scale(m::K)} {m::total(s)} {w}\");\n}\n",
    )
    .unwrap();
    let m = "pub const K: i64 = 5;\n\n\
             pub struct Sq {\n    pub side: i64;\n}\n\n\
             impl dep::Shape for Sq {\n    fn area(self) -> i64 {\n        self.side * self.side\n    }\n}\n\n\
             pub fn total<T: dep::Shape>(x: T) -> i64 {\n    x.area()\n}\n\n\
             pub fn f(x: i64) -> i64 {\n    x + 1\n}\n";
    put(dir.join("app/src/m.nabi"), m).unwrap();
    let run = |vars: &[(&str, &str)], args: &[&str]| {
        // The executable goes into the build's own target directory.
        let target = args
            .get(1)
            .map_or_else(|| dir.join("app/target"), PathBuf::from);
        let exe = target.join(exe_name("run"));
        let out = Command::new(NABIC)
            .current_dir(&dir)
            .env("NABI_HOME", workspace())
            .env("NABI_INCREMENTAL_VERIFY", "1")
            .envs(vars.iter().copied())
            .args(["build", "-v", "app", "--extern", "dep=dep", "-o"])
            .arg(&exe)
            .args(args)
            .arg("--nabir")
            .arg(&env.nabir)
            .output()
            .unwrap();
        assert_exit(&out, 0);
        let ran = Command::new(&exe).output().unwrap();
        let err = text(&out.stderr);
        let warnings: Vec<String> = err
            .lines()
            .filter(|l| l.starts_with("warning"))
            .map(str::to_owned)
            .collect();
        let kept = err.contains("the library's items collected");
        (kept, warnings, text(&ran.stdout))
    };
    // Whether the library stage was taken, and the program's output; the
    // warnings and output are a clean build's.
    let build = || {
        let (kept, w, out) = run(&[], &[]);
        let clean = dir.join("clean");
        let target = ["--target-dir", clean.to_str().unwrap()];
        let (_, clean_w, clean_out) = run(&[("NABI_INCREMENTAL", "0")], &target);
        assert_eq!((&w, &out), (&clean_w, &clean_out));
        assert!(
            w.iter().any(|l| l.contains("covers no throwing call")),
            "{w:?}"
        );
        (kept, out)
    };
    assert_eq!(build(), (false, "2 15 4 0\n".to_owned()));
    let edit = |from: &str, to: &str| {
        let path = dir.join("app/src/m.nabi");
        let src = fs::read_to_string(&path).unwrap();
        assert!(src.contains(from), "{from}");
        put(path, src.replace(from, to)).unwrap();
    };
    // A body.
    edit("x + 1", "x + 2");
    assert_eq!(build(), (true, "3 15 4 0\n".to_owned()));
    // A signature.
    edit(
        "pub fn f(x: i64) -> i64 {\n    x + 2",
        "pub fn f(x: i32) -> i64 {\n    (x as i64) + 2",
    );
    assert_eq!(build(), (true, "3 15 4 0\n".to_owned()));
    // A const the package uses.
    edit("K: i64 = 5", "K: i64 = 7");
    assert_eq!(build(), (true, "3 21 4 0\n".to_owned()));
    // A trait impl.
    edit("self.side * self.side", "self.side * self.side * self.side");
    assert_eq!(build(), (true, "3 21 8 0\n".to_owned()));
    // A dependency's item the package uses: collected again, then kept.
    put(
        dir.join("dep/src/lib.nabi"),
        dep.replace("LIMIT: i64 = 3", "LIMIT: i64 = 4"),
    )
    .unwrap();
    assert_eq!(build(), (false, "3 28 8 0\n".to_owned()));
    edit(") + 2", ") + 3");
    assert_eq!(build(), (true, "4 28 8 0\n".to_owned()));
    // A corrupt store: a miss, written anew. The stages are the
    // toolchain's (beside a repository build of nabic); this one's name
    // the dependency's files.
    let stages_dir = Path::new(NABIC)
        .parent()
        .unwrap()
        .join("nabi-cache/library");
    let dep_dir = nabic_core::real_path(dir.join("dep"))
        .unwrap()
        .display()
        .to_string();
    let stages: Vec<PathBuf> = fs::read_dir(&stages_dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("library-"))
                && fs::read(p)
                    .is_ok_and(|b| b.windows(dep_dir.len()).any(|w| w == dep_dir.as_bytes()))
        })
        .collect();
    assert!(
        !stages.is_empty(),
        "no library stage of {} under {}",
        dir.display(),
        stages_dir.display()
    );
    for p in &stages {
        let mut bytes = fs::read(p).unwrap();
        let n = bytes.len();
        bytes.truncate(n / 2);
        fs::write(p, bytes).unwrap();
    }
    edit(") + 3", ") + 4");
    assert_eq!(build(), (false, "5 28 8 0\n".to_owned()));
    edit(") + 4", ") + 5");
    assert_eq!(build(), (true, "6 28 8 0\n".to_owned()));
}

/// A generic body is checked against its bounds in
/// every build, incremental or not: breaking it fails the build while
/// the plain bodies replay, and fixing it builds again.
#[test]
fn incremental_checks_generic_bodies() {
    let env = setup();
    let dir = scratch("incr-generic");
    let app = dir.join("app");
    let src = app.join("src");
    fs::create_dir_all(&src).unwrap();
    let main = "mod m;\n\nfn main() {\n    println(f\"{m::twice(3)} {m::one()}\");\n}\n";
    fs::write(src.join("main.nabi"), main).unwrap();
    let ok = "use core::ops::Add;\n\n\
              pub fn twice<T: Add<T, Output = T> + Copy>(x: T) -> T {\n    x + x\n}\n\n\
              pub fn one() -> i64 {\n    1\n}\n";
    fs::write(src.join("m.nabi"), ok).unwrap();
    let check = || {
        let out = Command::new(NABIC)
            .current_dir(&app)
            .env("NABI_HOME", workspace())
            .env("NABI_INCREMENTAL_VERIFY", "1")
            .args(["check"])
            .arg("--nabir")
            .arg(&env.nabir)
            .output()
            .unwrap();
        (out.status.success(), text(&out.stderr))
    };
    assert!(check().0, "{}", check().1);
    // The bound gone: the body no longer checks.
    fs::write(
        src.join("m.nabi"),
        ok.replace(": Add<T, Output = T> + Copy", ""),
    )
    .unwrap();
    let want = "operator `+` on `T` needs the bound `T: Add`";
    let (pass, err) = check();
    assert!(!pass && err.contains(want), "{err}");
    // Another body edited: still reported.
    fs::write(src.join("main.nabi"), main.replace("one()}", "one() + 1}")).unwrap();
    let (pass, err) = check();
    assert!(!pass && err.contains(want), "{err}");
    fs::write(src.join("m.nabi"), ok).unwrap();
    let (pass, err) = check();
    assert!(pass, "{err}");
    let out = build_run(Some(&env), &app, &["-q"]);
    assert_exit(&out, 0);
    assert_eq!(text(&out.stdout), "6 2\n");
}

/// A body that calls a `-> some Tr` fn is checked again with an opaque
/// type in its place; what that check made is not kept with the body,
/// so a rebuild lowers the body again instead of replaying it.
#[test]
fn incremental_some_calls_rebuild() {
    let env = setup();
    let dir = scratch("incr-some");
    // A dependency's named `some` type.
    put(
        dir.join("dep/src/lib.nabi"),
        "pub type Evens = some Iterator<Item = i64>;\n\n\
         pub fn evens(n: i64) -> Evens {\n    (0..n).filter(|x| x % 2 == 0)\n}\n",
    )
    .unwrap();
    // A package's own `-> some` fns, used by its executable.
    let app = dir.join("app");
    put(
        app.join("src/lib.nabi"),
        "pub fn odds(n: i32) -> some Iterator<Item = i32> {\n    \
         (0..n).filter(|x| x % 2 == 1)\n}\n\n\
         pub fn adder(k: i32) -> some fn(i32) -> i32 {\n    |x| x + k\n}\n",
    )
    .unwrap();
    put(
        app.join("src/main.nabi"),
        "use app::{adder, odds};\nuse dep::Evens;\n\n\
         fn sum(e: Evens) -> i64 { e.sum() }\n\n\
         fn main() {\n    let it = odds(6);\n    let f = adder(3);\n    \
         println(f\"{it.map(|x| x * 10).sum()} {f(4)} {sum(dep::evens(7))}\");\n}\n",
    )
    .unwrap();
    let dep = dir.join("dep").display().to_string();
    let ext = format!("dep={dep}");
    let build = || {
        let out = nabic(Some(&env), &app, &["build", "-q", "--extern", &ext]);
        assert_exit(&out, 0);
        let exe = app.join("target/debug").join(exe_name("app"));
        text(&Command::new(exe).output().unwrap().stdout)
    };
    assert_eq!(build(), "90 7 12\n");
    // Another source: the package builds again, every body unchanged.
    put(app.join("src/other.nabi"), "fn f() {}\n").unwrap();
    assert_eq!(build(), "90 7 12\n");
}

/// A mark added to or removed from an item in another file reaches the
/// uses in bodies kept from the last build.
#[test]
fn incremental_deprecation_marks() {
    let _env = setup();
    let dir = scratch("incr-deprecated");
    let app = dir.join("app");
    let src = app.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(
        src.join("main.nabi"),
        "mod m;\n\nfn main() {\n    let s = m::S { x: 1 };\n    \
         println(f\"{m::old()} {s.x} {m::C}\");\n}\n",
    )
    .unwrap();
    let plain = "pub struct S { pub x: i32; }\npub const C: i32 = 2;\n\
                 pub fn old() -> i32 {\n    1\n}\n";
    let marked = "pub struct S { #[deprecated] pub x: i32; }\n\
                  #[deprecated(\"use D\")]\npub const C: i32 = 2;\n\
                  #[deprecated(since = \"1.2.0\", note = \"use new\")]\n\
                  pub fn old() -> i32 {\n    1\n}\n";
    fs::write(src.join("m.nabi"), plain).unwrap();
    let check = || {
        let out = Command::new(NABIC)
            .current_dir(&app)
            .env("NABI_HOME", workspace())
            .env("NABI_INCREMENTAL_VERIFY", "1")
            .args(["check"])
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", text(&out.stderr));
        text(&out.stderr)
    };
    let warned = [
        "use of deprecated function `old`",
        "use of deprecated field `S.x`",
        "use of deprecated constant `C`",
    ];
    let err = check();
    assert!(!err.contains("deprecated"), "{err}");
    fs::write(src.join("m.nabi"), marked).unwrap();
    for _ in 0..2 {
        let err = check();
        for w in warned {
            assert!(err.contains(w), "missing {w:?}:\n{err}");
        }
        assert!(err.contains("deprecated since 1.2.0"), "{err}");
    }
    fs::write(src.join("m.nabi"), plain).unwrap();
    let err = check();
    assert!(!err.contains("deprecated"), "{err}");
}

/// A library builds by units, links with another holding the same
/// instances, and names no symbol of a dependency it does not use.
#[test]
fn library_builds_by_units() {
    let env = setup();
    let targets = Path::new(NABIC)
        .parent()
        .unwrap()
        .join("../lib/nabi/targets");
    let cross = ["x86_64-pc-windows-msvc", "x86_64-unknown-linux-gnu"]
        .into_iter()
        .filter(|t| targets.join(t).is_dir());
    for target in std::iter::once(None).chain(cross.map(Some)) {
        for opt in ["-O0", "-O2"] {
            library_units(&env, target, opt);
        }
    }
}

pub(crate) fn library_units(env: &Env, target: Option<&str>, opt: &str) {
    let dir = scratch(&format!("units_lib{}{opt}", target.unwrap_or("")));
    let (a, b) = (dir.join("liba"), dir.join("libb"));
    let app = dir.join("app");
    for d in [&a, &b, &app] {
        fs::create_dir_all(d).unwrap();
    }
    put(
        a.join("src/lib.nabi"),
        "mod util;\n\nstatic NAMES: [str; 2] = [\"x\", \"yy\"];\n\n\
         pub fn total(n: i64) -> i64 {\n    let t: i64 = 0;\n    for i in 0..n {\n        \
         t += util::weight(f\"p{i}\");\n    }\n    t + NAMES.len() as i64\n}\n",
    )
    .unwrap();
    // Inlined at `-O2` whatever the `Dict` calls in it grow to.
    let util = "#[inline(always)]\npub fn weight(s: str) -> i64 {\n    \
                let d: Dict<str, i64> = Dict::new();\n    \
                d[s] = s.len() as i64;\n    d.get(s) ?? 0\n}\n";
    put(a.join("src/util.nabi"), util).unwrap();
    put(
        b.join("src/lib.nabi"),
        "pub fn words(n: i64) -> i64 {\n    let d: Dict<str, i64> = Dict::new();\n    \
         for i in 0..n {\n        d[f\"w{i}\"] = i;\n    }\n    d.get(\"w2\") ?? 0\n}\n",
    )
    .unwrap();
    put(
        app.join("src/main.nabi"),
        "use liba::total;\nuse libb::words;\n\nprintln(f\"{total(3)} {words(4)}\");\n",
    )
    .unwrap();
    let profile = if opt == "-O2" { "release" } else { "debug" };
    let out_dir = target.map_or(profile.to_owned(), |t| format!("{t}/{profile}"));
    let archive = |d: &Path, n: &str| d.join(format!("target/{out_dir}/lib{n}.a"));
    let (lib_a, lib_b) = (archive(&a, "liba"), archive(&b, "libb"));
    let extern_a = format!("liba={}", lib_a.display());
    let target_arg = target.map(|t| format!("--target={t}"));
    // The host's runtime, or the target's beside nabic.
    let nabic_t = |args: &[&str]| {
        let args: Vec<&str> = args.iter().copied().chain(target_arg.as_deref()).collect();
        nabic(target.is_none().then_some(env), &dir, &args)
    };
    let what = format!("{target:?} {opt}");
    let build = |args: &[&str]| {
        let out = nabic_t(&[&["build", "-vv", opt], args].concat());
        assert_exit(&out, 0);
        let mut units: Vec<String> = text(&out.stderr)
            .lines()
            .filter_map(|l| l.trim().strip_prefix("Compiled unit "))
            .map(str::to_owned)
            .collect();
        units.sort();
        units
    };
    // The program's output (a cross target's links only).
    let run = |want: &str| {
        let extern_b = format!("libb={}", lib_b.display());
        let exe = dir.join("app.bin");
        let exe_s = exe.display().to_string();
        let out = nabic_t(&[
            "build", "-q", opt, "app", "--extern", &extern_a, "--extern", &extern_b, "-o", &exe_s,
        ]);
        assert_exit(&out, 0);
        if target.is_none() {
            let got = text(&Command::new(&exe).output().unwrap().stdout);
            assert_eq!(got, want, "{what}");
        }
    };
    assert!(build(&["liba"]).len() > 2);
    // Built after `liba`: its GC types follow `liba`'s.
    build(&["libb", "--extern", &extern_a]);
    run("8 2\n");
    put(
        a.join("src/util.nabi"),
        util.replace("as i64;", "as i64 + 1;"),
    )
    .unwrap();
    // `-O2` inlines `weight` into `total`.
    let edited: &[&str] = if opt == "-O2" {
        &["liba", "liba::util"]
    } else {
        &["liba::util"]
    };
    assert_eq!(build(&["liba"]), edited, "{what}");
    run("11 2\n");
    let old_b = fs::read(&lib_b).unwrap();
    let units: Vec<String> = Vec::new();
    assert_eq!(build(&["libb", "--extern", &extern_a]), units, "{what}");
    assert!(old_b == fs::read(&lib_b).unwrap(), "{what}");
    let incremental = fs::read(&lib_a).unwrap();
    fs::remove_dir_all(a.join("target")).unwrap();
    build(&["liba"]);
    assert!(
        incremental == fs::read(&lib_a).unwrap(),
        "{what}: not the clean build's archive"
    );
}

/// An anonymous file builds by units too, kept per file in the target
/// directory without a package (`NABI_TARGET_DIR`, else the system's
/// temporary one), not in a `target/` beside the file.
#[test]
fn anonymous_file_reuses_its_units() {
    let env = setup();
    let dir = scratch("units_anon");
    put(dir.join("hello.nabi"), "println(f\"{[1, 2].len()}\");\n").unwrap();
    let cache = dir.join("cache");
    let run = || {
        let out = Command::new(NABIC)
            .current_dir(&dir)
            .env("NABI_HOME", workspace())
            .env("NABI_TARGET_DIR", &cache)
            .args(["build", "-v", "hello.nabi", "--nabir"])
            .arg(&env.nabir)
            .output()
            .unwrap();
        assert_exit(&out, 0);
        let ran = Command::new(dir.join(exe_name("hello"))).output().unwrap();
        assert_eq!(text(&ran.stdout), "2\n");
        text(&out.stderr)
    };
    assert!(!run().contains(" 0 compiled"));
    // Unchanged, it is not built again.
    assert!(run().contains("Fresh hello"));
    // Built again, it reuses every unit.
    fs::remove_dir_all(cache.join("debug/scripts")).unwrap();
    assert!(run().contains(" 0 compiled"));
    let units = fs::read_dir(cache.join("debug/incremental")).unwrap();
    let names: Vec<String> = units
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        names.len() == 1 && names[0].starts_with("hello-"),
        "{names:?}"
    );
    assert!(!dir.join("target").exists());
}

/// Under `-g` a unit's debug info names only its own files: a new file
/// recompiles what imports it (and the C `main`, whose line moved), not
/// every unit.
#[test]
fn new_file_under_debug_info_recompiles_its_importers() {
    let env = setup();
    let app = scratch("units_g").join("app");
    fs::create_dir_all(&app).unwrap();
    let main = "fn main() -> i32 {\n    println(f\"{a::one()} {b::two()}\");\n    return 0;\n}\n";
    put(
        app.join("src/main.nabi"),
        format!("mod a;\nmod b;\n\n{main}"),
    )
    .unwrap();
    put(app.join("src/a.nabi"), "pub fn one() -> i64 {\n    1\n}\n").unwrap();
    put(app.join("src/b.nabi"), "pub fn two() -> i64 {\n    2\n}\n").unwrap();
    let build = || {
        let out = nabic(Some(&env), &app, &["build", "-vv", "-g"]);
        assert_exit(&out, 0);
        let mut units: Vec<String> = text(&out.stderr)
            .lines()
            .filter_map(|l| l.trim().strip_prefix("Compiled unit "))
            .map(str::to_owned)
            .collect();
        units.sort();
        units
    };
    assert!(build().len() > 3);
    // `a` < `aa` < `b`: the new file comes between the others.
    put(
        app.join("src/aa.nabi"),
        "pub fn three() -> i64 {\n    3\n}\n",
    )
    .unwrap();
    let main = main.replace("b::two()}", "b::two()} {aa::three()}");
    put(
        app.join("src/main.nabi"),
        format!("mod a;\nmod aa;\nmod b;\n\n{main}"),
    )
    .unwrap();
    assert_eq!(build(), ["<glue>", "<root>", "aa"]);
    let run = build_run(Some(&env), &app, &["-q", "-g"]);
    assert_eq!(text(&run.stdout), "1 2 3\n");
}

/// A release build of one package and dependency in two directories
/// writes the same objects, archives, NABIHIRs and executable, and none
/// of them names where it was built or the home directory.
#[test]
fn release_builds_in_two_places_are_byte_identical() {
    let env = setup();
    let root = scratch("two-places");
    let write = |dir: &Path| {
        put(
            dir.join("greet/src/lib.nabi"),
            "pub fn pick(xs: List<i64>, i: usize) -> i64 { return xs[i]; }\n\
             pub fn greet<T>(xs: List<T>) -> usize { return xs.len(); }\n",
        )
        .unwrap();
        put(
            dir.join("app/src/main.nabi"),
            "use greet::{greet, pick};\n\
             fn main() {\n    let xs: List<i64> = [1, 2, 3];\n    \
             println(f\"{greet(xs)} {pick(xs, 2)} {xs[xs.len() - 1]}\");\n}\n",
        )
        .unwrap();
    };
    let build = |dir: &Path| {
        let app = dir.join("app");
        let out = nabic(
            Some(&env),
            &app,
            &["build", ".", "--release", "--extern", "greet=../greet"],
        );
        assert_exit(&out, 0);
        let rel = app.join("target/release");
        let mut files = Vec::new();
        let mut todo = vec![rel.clone()];
        while let Some(d) = todo.pop() {
            for e in fs::read_dir(&d).unwrap() {
                let p = e.unwrap().path();
                // The incremental stores are caches keyed by paths.
                if p.is_dir() && !p.ends_with("incremental") {
                    todo.push(p);
                } else if p.is_file() && p.extension().is_none_or(|x| x != "fp") {
                    let bytes = fs::read(&p).unwrap();
                    files.push((p.strip_prefix(&rel).unwrap().to_path_buf(), bytes));
                }
            }
        }
        files.sort();
        files
    };
    let (a, b) = (root.join("a"), root.join("somewhere/else/b"));
    write(&a);
    write(&b);
    let (first, second) = (build(&a), build(&b));
    let names = |f: &[(PathBuf, Vec<u8>)]| f.iter().map(|(p, _)| p.clone()).collect::<Vec<_>>();
    assert_eq!(names(&first), names(&second));
    assert!(
        first.iter().any(|(p, _)| p.ends_with("deps/greet.nabihir")),
        "{:?}",
        names(&first)
    );
    // The executable links this checkout's runtime, built without
    // `--remap-path-prefix` (a release toolchain's is): only the package
    // path is checked there.
    let home = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE"));
    let ours = [
        Some(workspace().display().to_string()),
        home.ok().map(|h| format!("{h}/")),
    ];
    for ((p, x), (_, y)) in first.iter().zip(&second) {
        assert!(x == y, "{} differs between the two places", p.display());
        let exe = p.extension().is_none() || p.extension().is_some_and(|e| e == "exe");
        let leaks = std::iter::once(a.display().to_string())
            .chain(ours.iter().flatten().filter(|_| !exe).cloned());
        for l in leaks {
            let hit = x.windows(l.len()).any(|w| w == l.as_bytes());
            assert!(!hit, "{} names {l}", p.display());
        }
    }
    let exe = a.join("app/target/release").join(exe_name("app"));
    let run = Command::new(&exe).output().unwrap();
    assert_eq!(text(&run.stdout), "3 3 3\n", "{}", text(&run.stderr));
}

/// `-vv`: the files read and the generic instances made.
#[test]
fn very_verbose_lists_files_and_instances() {
    let dir = scratch("very_verbose");
    let app = dir.join("app");
    fs::create_dir_all(&app).unwrap();
    put(
        app.join("src/main.nabi"),
        "fn id<T>(x: T) -> T {\n    x\n}\n\nprintln(f\"{id(1)}\");\n",
    )
    .unwrap();
    let out = nabic(None, &app, &["emit", "-vv", "--emit=hir"]);
    assert_exit(&out, 0);
    let stderr = text(&out.stderr);
    assert!(stderr.contains("     Reading src/main.nabi\n"), "{stderr}");
    assert!(
        stderr.contains(" Specialized id(x: i64) -> i64\n"),
        "{stderr}"
    );
    let out = nabic(None, &app, &["emit", "-v", "--emit=hir"]);
    assert!(!text(&out.stderr).contains("Specialized"));
}

/// Clean builds of the same package and dependency write the same bytes:
/// objects, archives, NABIHIRs and the incremental stores. The `.fp`
/// records hold file stamps and are left out.
#[test]
fn clean_builds_are_byte_identical() {
    let env = setup();
    let dir = scratch("deterministic");
    put(
        dir.join("greet/src/lib.nabi"),
        "pub mod words;\npub mod shapes;\nuse pkg::words::hello;\n\
         pub fn greet(n: i32) -> i32 { return hello() + n; }\n",
    )
    .unwrap();
    put(
        dir.join("greet/src/words.nabi"),
        "pub fn hello() -> i32 { return 40; }\n\
         pub fn names() -> Dict<str, i32> { return {\"a\": 1, \"b\": 2, \"c\": 3}; }\n",
    )
    .unwrap();
    put(
        dir.join("greet/src/shapes.nabi"),
        "pub enum Shape { Circle(f64), Square(f64), Tri(f64, f64) }\n\
         pub trait Area { fn area(self) -> f64; }\n\
         impl Area for Shape {\n    fn area(self) -> f64 {\n        match self {\n            \
         Shape::Circle(r) => 3.0 * r * r,\n            Shape::Square(s) => s * s,\n            \
         Shape::Tri(b, h) => b * h / 2.0,\n        }\n    }\n}\n",
    )
    .unwrap();
    // Enough files that parsing them in parallel finishes in no fixed order.
    let mut main = String::from("mod sub;\nuse greet::greet;\nuse greet::shapes::{Area, Shape};\n");
    let mut sum = String::from("0");
    for i in 0..24 {
        main += &format!("mod m{i};\n");
        sum += &format!(" + m{i}::f({i})");
        put(
            dir.join(format!("app/src/m{i}.nabi")),
            format!(
                "pub fn f(x: i64) -> i64 {{ return x * {i} + {}; }}\n",
                i * 7
            ),
        )
        .unwrap();
    }
    main += &format!(
        "let s: Set<str> = Set::new();\nfor (k, _) in greet::words::names() {{ s.insert(k); }}\n\
         println(f\"{{greet(1)}} {{sub::two()}} {{Shape::Tri(2.0, 3.0).area()}} {{s.len()}} {{{sum}}}\");\n"
    );
    put(dir.join("app/src/main.nabi"), main).unwrap();
    put(
        dir.join("app/src/sub.nabi"),
        "use greet::words::hello;\npub fn two() -> i32 { return hello() + 2; }\n",
    )
    .unwrap();
    let build = || {
        let _ = fs::remove_dir_all(dir.join("app/target"));
        let out = nabic(
            Some(&env),
            &dir,
            &["build", "app", "--extern", "greet=greet"],
        );
        assert_exit(&out, 0);
        let mut files = Vec::new();
        let mut todo = vec![dir.join("app/target")];
        while let Some(d) = todo.pop() {
            for e in fs::read_dir(&d).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    todo.push(p);
                } else if p.extension().is_none_or(|x| x != "fp") {
                    let bytes = fs::read(&p).unwrap();
                    files.push((p, bytes));
                }
            }
        }
        files.sort();
        files
    };
    // The first build may make the toolchain's library stage, which
    // later builds read instead of keeping their own parses.
    build();
    let first = build();
    assert!(
        first.iter().any(|(p, _)| p.ends_with("deps/greet.nabihir")),
        "{:?}",
        first.iter().map(|(p, _)| p).collect::<Vec<_>>()
    );
    let second = build();
    let names = |f: &[(PathBuf, Vec<u8>)]| f.iter().map(|(p, _)| p.clone()).collect::<Vec<_>>();
    assert_eq!(names(&first), names(&second));
    for ((p, a), (_, b)) in first.iter().zip(&second) {
        assert!(a == b, "{} differs between two clean builds", p.display());
    }
}
