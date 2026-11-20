//! `test` and `bench`: harness runs and their JSON.

#[allow(clippy::wildcard_imports)]
use crate::cli::*;

#[test]
fn lib_package_builds_archive_and_runs_tests() {
    let env = setup();
    let dir = scratch("lib");
    let lib = new_package(&dir, "mylib", true);

    assert_exit(&nabic(Some(&env), &dir, &["build", "mylib"]), 0);
    assert!(lib.join("target/debug/libmylib.a").is_file());

    let (code, stdout, _) = harness(&env, &lib, "test", &[]);
    assert_eq!(code, 0, "{stdout}");
    assert!(stdout.contains("test adds ... ok"), "{stdout}");
    assert!(stdout.contains("1 passed; 0 failed"), "{stdout}");

    let mut src = fs::read_to_string(lib.join("src/lib.nabi")).unwrap();
    src.push_str(
        "\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn boom() {\n        \
         println(\"from boom\");\n        panic(\"boom\");\n    }\n}\n",
    );
    put(lib.join("src/lib.nabi"), src).unwrap();
    let (code, stdout, _) = harness(&env, &lib, "test", &[]);
    assert_eq!(code, 1, "{stdout}");
    assert!(stdout.contains("test tests::boom ... FAILED"), "{stdout}");
    assert!(stdout.contains("from boom"), "{stdout}");
    assert!(stdout.contains("1 passed; 1 failed"), "{stdout}");

    // Filter (a lone non-path argument), regex anchors included.
    let (code, stdout, _) = harness(&env, &lib, "test", &["^add"]);
    assert_eq!(code, 0, "{stdout}");
    assert!(
        stdout.contains("1 passed; 0 failed; 0 ignored; 1 filtered out"),
        "{stdout}"
    );

    // `#[test]` functions are not part of a normal build.
    assert_exit(&nabic(Some(&env), &lib, &["build"]), 0);
}

#[test]
fn tests_in_private_modules_run() {
    let env = setup();
    let dir = scratch("privtests");
    let lib = new_package(&dir, "privlib", true);
    let mut src = fs::read_to_string(lib.join("src/lib.nabi")).unwrap();
    src.push_str(
        "mod util;\nmod inner {\n    mod deeper {\n        #[test]\n        fn f() {}\n    }\n\
         \x20   #[test]\n    fn g() {\n        panic(\"g\");\n    }\n}\n",
    );
    put(lib.join("src/lib.nabi"), src).unwrap();
    put(
        lib.join("src/util.nabi"),
        "mod checks {\n    #[test]\n    fn h() {}\n}\n",
    )
    .unwrap();
    let (code, stdout, _) = harness(&env, &lib, "test", &[]);
    assert_eq!(code, 1, "{stdout}");
    for line in [
        "test inner::deeper::f ... ok",
        "test inner::g ... FAILED",
        "test util::checks::h ... ok",
        "3 passed; 1 failed",
    ] {
        assert!(stdout.contains(line), "{line}\n{stdout}");
    }

    // User code in a test build keeps the visibility rules.
    put(
        lib.join("src/util.nabi"),
        "mod checks {\n    #[test]\n    fn h() {}\n}\npub fn leak() { checks::h(); }\n",
    )
    .unwrap();
    let (code, _, err) = harness(&env, &lib, "test", &[]);
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("`util::checks::h` is private"), "{err}");
}

/// Each line of `out` as JSON.
#[track_caller]
pub(crate) fn records(out: &str) -> Vec<nabic_diagnostic::Json> {
    out.lines()
        .map(|l| nabic_diagnostic::Json::parse(l).unwrap_or_else(|e| panic!("{e}: {l}")))
        .collect()
}

/// `test --format json`: a versioned record per test and a summary on
/// stdout; the human text and the tests' output elsewhere.
#[test]
fn test_format_json() {
    let env = setup();
    let dir = scratch("test-json");
    let lib = new_package(&dir, "jlib", true);
    let mut src = fs::read_to_string(lib.join("src/lib.nabi")).unwrap();
    src.push_str("\n#[test]\nfn boom() {\n    println(\"noise\");\n    panic(\"boom\");\n}\n");
    put(lib.join("src/lib.nabi"), src).unwrap();
    let (code, out, human, _) = harness_json(&env, &lib, "test", &["--format", "json"]);
    assert_eq!(code, 1, "{out}\n{human}");
    let recs = records(&out);
    assert_eq!(recs.len(), 3, "{out}");
    for r in &recs {
        assert_eq!(r.get("schema").as_u32(), Some(1), "{out}");
        assert_eq!(r.get("kind").as_str(), Some("test"), "{out}");
    }
    assert_eq!(recs[0].get("type").as_str(), Some("case"), "{out}");
    assert_eq!(recs[0].get("name").as_str(), Some("adds"), "{out}");
    assert_eq!(recs[0].get("result").as_str(), Some("ok"), "{out}");
    assert_eq!(recs[1].get("name").as_str(), Some("boom"), "{out}");
    assert_eq!(recs[1].get("result").as_str(), Some("failed"), "{out}");
    assert!(
        matches!(recs[1].get("ms"), nabic_diagnostic::Json::Num(_)),
        "{out}"
    );
    assert_eq!(recs[2].get("type").as_str(), Some("summary"), "{out}");
    assert_eq!(recs[2].get("result").as_str(), Some("failed"), "{out}");
    assert_eq!(recs[2].get("passed").as_u32(), Some(1), "{out}");
    assert_eq!(recs[2].get("failed").as_u32(), Some(1), "{out}");
    assert!(!out.contains("noise"), "{out}");
    assert!(human.contains("test boom ... FAILED"), "{human}");
    assert!(human.contains("noise"), "{human}");

    // Another format is named, with the ones there are; `nabibench` is
    // bench's alone.
    let take = |a: &[&str]| {
        let a: Vec<String> = a.iter().map(|&s| s.to_owned()).collect();
        nabic_harness::take_options("test", &a).unwrap_err()
    };
    assert_eq!(
        take(&["--format", "xml"]),
        "`--format` takes human or json, not `xml`"
    );
    assert_eq!(
        take(&["--format=nabibench"]),
        "`--format` takes human or json for test; `nabibench` is for bench"
    );
    // The runner's options are not nabic's.
    let out = nabic(Some(&env), &lib, &["test", "--format", "json"]);
    assert_exit(&out, 2);
    assert!(
        text(&out.stderr).contains("unknown option `--format`"),
        "{}",
        text(&out.stderr)
    );
}

/// `bench --format json`: the `#nabibench` line becomes a record; what the
/// bench prints itself stays off stdout.
#[test]
fn bench_format_json_separates_noise() {
    let env = setup();
    let dir = scratch("bench-json");
    let app = new_package(&dir, "japp", false);
    put(
        app.join("src/main.nabi"),
        "use core::bench::Bench;\nuse core::hint::black_box;\nfn main() {}\n\
         #[bench]\nfn sum(b: Bench) {\n    println(\"noise from bench\");\n    \
         b.iter(|| black_box(1i64 + 2i64));\n}\n#[bench]\nfn idle(b: Bench) { let _ = b; }\n",
    )
    .unwrap();
    let opts = ["--iters=3", "--samples=2"];
    let (code, out, human, _) = harness_json(
        &env,
        &app,
        "bench",
        &[&opts[..], &["--format=json"]].concat(),
    );
    assert_eq!(code, 0, "{out}\n{human}");
    let recs = records(&out);
    assert_eq!(recs.len(), 3, "{out}");
    let sum = recs.iter().find(|r| r.get("name").as_str() == Some("sum"));
    let sum = sum.unwrap_or_else(|| panic!("{out}"));
    assert_eq!(sum.get("kind").as_str(), Some("bench"), "{out}");
    assert_eq!(sum.get("result").as_str(), Some("ok"), "{out}");
    assert_eq!(sum.get("samples").as_u32(), Some(2), "{out}");
    assert!(
        matches!(sum.get("median_ns"), nabic_diagnostic::Json::Num(_)),
        "{out}"
    );
    let idle = recs.iter().find(|r| r.get("name").as_str() == Some("idle"));
    assert_eq!(
        idle.unwrap().get("result").as_str(),
        Some("ignored"),
        "{out}"
    );
    let last = recs.last().unwrap();
    assert_eq!(last.get("type").as_str(), Some("summary"), "{out}");
    assert_eq!(last.get("run").as_u32(), Some(1), "{out}");
    assert_eq!(last.get("ignored").as_u32(), Some(1), "{out}");
    assert!(
        !out.contains("noise") && !out.contains("#nabibench"),
        "{out}"
    );
    assert!(human.contains("noise from bench\n"), "{human}");
    assert!(human.contains("bench result: ok. 1 run;"), "{human}");
}

/// The `#nabibench` line is frozen; new fields go only in the JSON records.
#[test]
fn nabibench_line_unchanged() {
    let env = setup();
    let dir = scratch("nabibench-frozen");
    let app = new_package(&dir, "vapp", false);
    put(
        app.join("src/main.nabi"),
        "use core::bench::Bench;\nfn main() {}\n\
         #[bench]\nfn one(b: Bench) { b.iter(|| 1i64); }\n",
    )
    .unwrap();
    let man = app.join("harness.manifest");
    let args = ["bench", "--iters=3", "--samples=2", "--format", "nabibench"];
    let (code, _, err) = harness(&env, &app, args[0], &args[1..]);
    assert_eq!(code, 0, "{err}");
    let manifest = fs::read_to_string(&man).unwrap();
    let exe = manifest
        .lines()
        .find_map(|l| l.strip_prefix("exe "))
        .unwrap();
    let o = Command::new(exe)
        .args(["one", "3", "2", "0", "3"])
        .output()
        .unwrap();
    let s = text(&o.stdout);
    let line = s.lines().next().unwrap_or_default();
    let keys: Vec<&str> = line
        .split(' ')
        .skip(1)
        .filter_map(|kv| kv.split_once('=').map(|(k, _)| k))
        .collect();
    assert!(line.starts_with("#nabibench name=one "), "{s}");
    assert_eq!(
        keys,
        [
            "name",
            "median_ns",
            "mad_ns",
            "samples",
            "outliers",
            "gc",
            "bytes",
            "items"
        ],
        "{s}"
    );
}

/// `--list` names the cases the filter keeps and runs none.
#[test]
fn list_names_cases_without_running() {
    let env = setup();
    let dir = scratch("list");
    let lib = new_package(&dir, "listlib", true);
    let mut src = fs::read_to_string(lib.join("src/lib.nabi")).unwrap();
    src.push_str("\n#[test]\nfn boom() {\n    panic(\"ran\");\n}\n");
    put(lib.join("src/lib.nabi"), src).unwrap();
    let (code, out, _) = harness(&env, &lib, "test", &["--list"]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(out, "adds\nboom\n");
    let (code, out, _) = harness(&env, &lib, "test", &["--list", "^bo"]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(out, "boom\n");
    let (code, out, _, _) = harness_json(&env, &lib, "test", &["--list", "--format=json"]);
    assert_eq!(code, 0, "{out}");
    let recs = records(&out);
    assert_eq!(recs.len(), 2, "{out}");
    assert_eq!(recs[0].get("type").as_str(), Some("listed"), "{out}");
    assert_eq!(recs[1].get("name").as_str(), Some("boom"), "{out}");
}

/// `bench` is `-O2` (no debug info) unless a level is given; `-O1` is the
/// `debug` profile and has debug info, as for `test -O1`.
#[test]
fn bench_level_keeps_debug_info() {
    let env = setup();
    if TargetSpec::host().object_format == ObjectFormat::Coff {
        return; // debug info goes to a PDB
    }
    let dir = scratch("bench-debug");
    let app = new_package(&dir, "dapp", false);
    put(
        app.join("src/main.nabi"),
        "use core::bench::Bench;\nfn main() {}\n#[bench]\nfn one(b: Bench) {\n    \
         b.iter(|| 1i64);\n}\n",
    )
    .unwrap();
    let debug_info = |args: &[&str]| {
        let man = app.join("bench.manifest");
        let mut all = vec!["bench", "--emit-harness", man.to_str().unwrap()];
        all.extend(args);
        assert_exit(&nabic(Some(&env), &app, &all), 0);
        let manifest = fs::read_to_string(&man).unwrap();
        let exe = manifest
            .lines()
            .find_map(|l| l.strip_prefix("exe "))
            .unwrap()
            .to_owned();
        if cfg!(target_vendor = "apple") {
            Path::new(&format!("{exe}.dSYM")).is_dir()
        } else {
            fs::read(&exe)
                .unwrap()
                .windows(11)
                .any(|w| w == b".debug_info")
        }
    };
    assert!(!debug_info(&[]), "a default bench has debug info");
    assert!(debug_info(&["-O1"]), "`bench -O1` has no debug info");
    assert!(debug_info(&["-O0"]), "`bench -O0` has no debug info");
}

/// `#[should_panic]`, `#[ignore]`, `--exact`, `--skip`, `--ignored` and the
/// per-test timeout.
#[test]
fn test_attributes_and_selection() {
    let env = setup();
    let dir = scratch("test-attrs");
    let lib = new_package(&dir, "alib", true);
    let mut src = fs::read_to_string(lib.join("src/lib.nabi")).unwrap();
    src.push_str(
        "\n#[test]\n#[should_panic]\nfn any_panic() { panic(\"x\"); }\n\
         \n#[test]\n#[should_panic(expected = \"bad index\")]\nfn right_panic() { panic(\"a bad index here\"); }\n\
         \n#[test]\n#[should_panic(expected = \"bad index\")]\nfn wrong_panic() { panic(\"other\"); }\n\
         \n#[test]\n#[should_panic]\nfn no_panic() {}\n\
         \n#[test]\n#[ignore = \"needs the network\"]\nfn slow() { loop { } }\n",
    );
    put(lib.join("src/lib.nabi"), src).unwrap();
    let (code, out, err) = harness(&env, &lib, "test", &[]);
    assert_eq!(code, 1, "{out}\n{err}");
    for line in [
        "test any_panic ... ok",
        "test right_panic ... ok",
        "test wrong_panic ... FAILED",
        "test no_panic ... FAILED",
        "test slow ... ignored, needs the network",
        "does not contain `bad index`",
        "returned without panicking",
        "3 passed; 2 failed; 1 ignored; 0 filtered out",
    ] {
        assert!(out.contains(line), "{line}\n{out}");
    }
    // `--exact` takes the whole name; `--skip` leaves names out.
    let (code, out, _) = harness(&env, &lib, "test", &["--exact", "adds"]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("1 passed; 0 failed; 0 ignored; 5 filtered out"),
        "{out}"
    );
    let (code, out, _) = harness(&env, &lib, "test", &["--skip", "wrong", "--skip=no_"]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("3 passed; 0 failed; 1 ignored; 2 filtered out"),
        "{out}"
    );
    // An ignored test runs on request, under the timeout.
    let (code, out, _) = harness(
        &env,
        &lib,
        "test",
        &["--ignored", "--timeout=1", "--format=json"],
    );
    assert_eq!(code, 1, "{out}");
    let recs = records(&out);
    assert_eq!(recs[0].get("name").as_str(), Some("slow"), "{out}");
    assert_eq!(recs[0].get("result").as_str(), Some("timeout"), "{out}");
    let (_, out, _) = harness(&env, &lib, "test", &["--list"]);
    assert!(out.contains("slow (ignored)\n"), "{out}");

    // The attributes go with `#[test]`, in their one form.
    put(
        lib.join("src/lib.nabi"),
        "#[ignore]\nfn a() {}\n\n#[test]\n#[should_panic(\"x\")]\nfn b() {}\n",
    )
    .unwrap();
    let (code, _, err) = harness(&env, &lib, "test", &[]);
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("write `#[test]` above `fn a` too"), "{err}");
    assert!(
        err.contains("`#[should_panic(expected = \"text\")]`"),
        "{err}"
    );
}

/// Doc tests: the `nabi` code blocks of the library's docs and README.md,
/// built on the library, with their fence words.
#[test]
fn doc_tests() {
    let env = setup();
    let dir = scratch("doc-tests");
    let lib = new_package(&dir, "dlib", true);
    let src = "//! ```nabi\n//! assert(dlib::add(1, 2) == 3);\n//! ```\n\n\
               /// Adds.\n///\n/// ```nabi\n/// use dlib::add;\n/// # let base = 10;\n/// assert(add(base, 2) == 12); let _f = boom;\n/// ```\n\
               ///\n/// ```text\n/// not code\n/// ```\n\
               ///\n/// ```nabi,should_panic\n/// dlib::boom();\n/// ```\n\
               ///\n/// ```nabi norun\n/// loop { }\n/// ```\n\
               ///\n/// ```nabi,compile_fail,E0040\n/// let x: i32 = \"no\";\n/// ```\n\
               ///\n/// ```nabi,ignore\n/// not code either\n/// ```\n\
               pub fn add(a: i32, b: i32) -> i32 { a + b }\n\n\
               pub fn boom() { panic(\"boom\"); }\n";
    put(lib.join("src/lib.nabi"), src).unwrap();
    put(
        lib.join("README.md"),
        "# dlib\n\n```nabi\nprintln(dlib::add(40, 2));\n```\n",
    )
    .unwrap();
    let (code, out, err) = harness(&env, &lib, "test", &["--doc"]);
    assert_eq!(code, 0, "{out}\n{err}");
    for line in [
        "test doc src/lib.nabi:1 ... ok",
        "test doc src/lib.nabi:7 (add) ... ok",
        "test doc src/lib.nabi:17 (add) ... ok",
        "test doc src/lib.nabi:21 (add) ... ok",
        "test doc src/lib.nabi:25 (add) ... ok",
        "test doc src/lib.nabi:29 (add) ... ignored",
        "test doc README.md:3 ... ok",
        "6 passed; 0 failed; 1 ignored",
    ] {
        assert!(out.contains(line), "{line}\n{out}");
    }
    // Filters and `--list` see doc cases; `--no-doc` drops them.
    let (_, out, _) = harness(&env, &lib, "test", &["--doc", "--list", "README"]);
    assert_eq!(out, "doc README.md:3\n");
    let (code, out, _) = harness(&env, &lib, "test", &["--no-doc"]);
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains("doc "), "{out}");

    // A failing block, and one that builds against its `compile_fail`.
    let bad = src
        .replace("dlib::boom();\n/// ```", "dlib::add(1, 1);\n/// ```")
        .replace("let x: i32 = \"no\";", "let x: i32 = 1;");
    put(lib.join("src/lib.nabi"), bad).unwrap();
    let (code, out, _) = harness(&env, &lib, "test", &["--doc"]);
    assert_eq!(code, 1, "{out}");
    assert!(
        out.contains("test doc src/lib.nabi:17 (add) ... FAILED"),
        "{out}"
    );
    assert!(
        out.contains("a `compile_fail` doc block built without errors"),
        "{out}"
    );

    // A block that does not build fails at its own line; the others run.
    put(
        lib.join("src/lib.nabi"),
        "/// ```nabi\n/// let y: i32 = \"bad\";\n/// ```\n///\n/// ```nabi\n/// assert(dlib::f() == 1);\n/// ```\n\
         pub fn f() -> i32 { 1 }\n",
    )
    .unwrap();
    let (code, out, _) = harness(&env, &lib, "test", &["--doc"]);
    assert_eq!(code, 1, "{out}");
    assert!(
        out.contains("test doc src/lib.nabi:1 (f) ... FAILED"),
        "{out}"
    );
    assert!(out.contains("test doc src/lib.nabi:5 (f) ... ok"), "{out}");
    assert!(out.contains("the doc block does not build"), "{out}");
    assert!(out.contains("lib-1.nabi:2:"), "{out}");
}
