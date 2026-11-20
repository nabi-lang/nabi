//! CLI behavior: spawn the built `nabic` on
//! scratch package directories (see `common::setup`).

mod args;
mod deps;
mod diagnostics;
mod harness;
mod incremental;
mod lints;
mod lone;
mod packages;
mod symbols;
mod targets;
mod tls;

use crate::common::{
    Env, NABIC, assert_exit, exe_name, invoke_command, put, scratch, setup, text, wasm_runner,
    workspace,
};
use nabic_target::{Libc, ObjectFormat, TargetSpec};
#[cfg(unix)]
use std::ffi::OsStr;
#[cfg(windows)]
use std::ffi::OsString;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write as _};
#[cfg(unix)]
use std::os::unix::ffi::{OsStrExt, OsStringExt};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
#[cfg(windows)]
use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// `nabic <args>` in `cwd`, with the repo's library and (when given) the
/// freshly built runtime.
fn nabic(env: Option<&Env>, cwd: &Path, args: &[&str]) -> Output {
    let mut cmd = Command::new(NABIC);
    cmd.current_dir(cwd)
        .env("NABI_HOME", workspace())
        .env(nabic_target::IN_PROGRESS_VAR, "1")
        .env_remove("CLICOLOR_FORCE")
        .args(args);
    if let Some(env) = env {
        cmd.arg("--nabir").arg(&env.nabir);
    }
    cmd.output().unwrap()
}

/// `nabic build <args>` (the options and path before `--`) into
/// `<cwd>/run/`, then the executable with the arguments after `--`, as
/// `nabi run` runs it: the program's stdout and exit code, its stderr
/// after the build's. A build that fails is the output.
fn build_run(env: Option<&Env>, cwd: &Path, args: &[&str]) -> Output {
    let at = args.iter().position(|a| *a == "--").unwrap_or(args.len());
    let exe = cwd.join("run").join(exe_name("prog"));
    fs::create_dir_all(exe.parent().unwrap()).unwrap();
    let exe_s = exe.display().to_string();
    let build = nabic(
        env,
        cwd,
        &[&["build"], &args[..at], &["-o", &exe_s]].concat(),
    );
    if !build.status.success() {
        return build;
    }
    let mut run = Command::new(&exe)
        .current_dir(cwd)
        .args(args.get(at + 1..).unwrap_or_default())
        .output()
        .unwrap();
    let mut stderr = build.stderr;
    stderr.append(&mut run.stderr);
    run.stderr = stderr;
    run
}

/// `nabic <args>` in `cwd` with `HOME` (and `USERPROFILE`) at `home`;
/// panics if it runs past a minute.
pub(crate) fn nabic_within(cwd: &Path, home: &Path, args: &[&str]) -> Output {
    let mut child = Command::new(NABIC)
        .current_dir(cwd)
        .env("NABI_HOME", workspace())
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env_remove("CLICOLOR_FORCE")
        .env_remove("__NABI_CLI")
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let start = std::time::Instant::now();
    while child.try_wait().unwrap().is_none() {
        if start.elapsed() > std::time::Duration::from_secs(60) {
            let _ = child.kill();
            panic!(
                "nabic {args:?} in {} still running after 60 s",
                cwd.display()
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    child.wait_with_output().unwrap()
}

/// A prebuilt library's whole NABIHIR as text (`nabi inspect --dump`).
fn inspect_dump(nabihir: &Path) -> String {
    let bytes = fs::read(nabihir).unwrap();
    nabic_interface::nabihir::Nabihir::decode(&bytes)
        .unwrap()
        .text()
}

/// Build the `kind` (`test` | `bench`) harness in `cwd` with `nabic`, then
/// run it the way `nabi` does (the `nabic_harness` runner). Returns the exit
/// code, the runner's stdout, and the build's stderr (its warnings, or a
/// build error when the harness does not build).
#[track_caller]
fn harness(env: &Env, cwd: &Path, kind: &str, extra: &[&str]) -> (i32, String, String) {
    let (code, out, _, err) = harness_json(env, cwd, kind, extra);
    (code, out, err)
}

/// [`harness`], and the human text a `--format json` run keeps off its
/// stdout (third).
#[track_caller]
fn harness_json(
    env: &Env,
    cwd: &Path,
    kind: &str,
    extra: &[&str],
) -> (i32, String, String, String) {
    // The runner's options are `nabi`'s; the rest goes to nabic, as there.
    let extra: Vec<String> = extra.iter().map(|&a| a.to_owned()).collect();
    let (opts, extra) = nabic_harness::take_options(kind, &extra).unwrap();
    let man = cwd.join("harness.manifest");
    let mut args = vec![kind, "--emit-harness", man.to_str().unwrap()];
    args.extend(extra.iter().map(String::as_str));
    let build = nabic(Some(env), cwd, &args);
    let build_err = text(&build.stderr);
    if !build.status.success() {
        return (
            build.status.code().unwrap_or(1),
            text(&build.stdout),
            String::new(),
            build_err,
        );
    }
    let manifest = fs::read_to_string(&man).unwrap();
    // The format version comes first.
    assert!(manifest.starts_with("version 2\n"), "{manifest}");
    let (mut out, mut human) = (Vec::new(), Vec::new());
    let code = nabic_harness::run_manifest_to(&manifest, &opts, &mut out, &mut human);
    (code, text(&out), text(&human), build_err)
}

/// `dir/name/src/main.nabi` (or `src/lib.nabi` with a test), as `nabi new`
/// makes it.
fn new_package(dir: &Path, name: &str, lib: bool) -> PathBuf {
    let pkg = dir.join(name);
    fs::create_dir_all(&pkg).unwrap();
    if lib {
        let src = "pub fn add(a: i32, b: i32) -> i32 {\n    return a + b;\n}\n\n#[test]\n\
                   fn adds() {\n    if add(2, 2) != 4 {\n        panic(\"add(2, 2) != 4\");\n    }\n}\n";
        put(pkg.join("src/lib.nabi"), src).unwrap();
    } else {
        put(pkg.join("src/main.nabi"), "println(\"Hello, world!\");\n").unwrap();
    }
    pkg
}

/// A nested fn's item (and symbol) name: the enclosing fn's (`Type.m`
/// for a method), `/`, its own; `/2` .. for a repeated name.
#[test]
fn nested_fn_item_names() {
    let dir = scratch("nested_fn_names");
    put(
        dir.join("src/main.nabi"),
        "struct S {}\nimpl S {\n    fn m(self) -> i32 {\n        fn h() -> i32 { 1 }\n        h()\n    }\n}\n\
         fn f() -> i32 {\n    let a = { fn g() -> i32 { 2 } g() };\n    let b = { fn g() -> i32 { 3 } g() };\n    a + b\n}\n\
         fn main() {\n    println(f\"{S {}.m() + f()}\");\n}\n",
    )
    .unwrap();
    let out = nabic(None, &dir, &["emit", "--emit=hir", "-o", "h.txt"]);
    assert_exit(&out, 0);
    let hir = fs::read_to_string(dir.join("h.txt")).unwrap();
    for f in ["fn S.m/h()", "fn f/g()", "fn f/g/2()"] {
        assert!(hir.contains(f), "{f}");
    }
}

/// `[v; N]` of a Copy element is one fill; of a `List` element, a
/// `clone()` per element but the last.
#[test]
fn repeat_literal_fills_copy_and_clones_the_rest() {
    let dir = scratch("repeat_fill");
    put(
        dir.join("src/main.nabi"),
        "struct P {\n    x: i32;\n}\nfn main() {\n    let w = [[7u8; 64]; 64];\n    \
         let p = [P { x: 1 }; 32];\n    println(f\"{w[0][0]} {p[0].x}\");\n}\n",
    )
    .unwrap();
    let out = nabic(None, &dir, &["emit", "--emit=hir", "-o", "h.txt"]);
    assert_exit(&out, 0);
    let hir = fs::read_to_string(dir.join("h.txt")).unwrap();
    let main = &hir[hir.find("fn main()").unwrap()..];
    let main = &main[..main.find("\n}\n").unwrap()];
    assert!(
        main.contains("[_0; 64]") && !main.contains("while"),
        "{main}"
    );

    put(
        dir.join("src/main.nabi"),
        "fn main() {\n    let q = [List::<i32>::new(); 40];\n    println(f\"{q.len()}\");\n}\n",
    )
    .unwrap();
    let out = nabic(None, &dir, &["emit", "--emit=hir", "-o", "h.txt"]);
    assert_exit(&out, 0);
    let hir = fs::read_to_string(dir.join("h.txt")).unwrap();
    let main = &hir[hir.find("fn main()").unwrap()..];
    assert!(main.contains("39usize") && main.contains("while"), "{main}");
}

/// A fresh future kept in a local nothing else names is spawned with no
/// hand-off; one with a second handle still goes through the copy.
#[test]
fn spawn_of_a_sole_local_future_skips_the_hand_off() {
    let hir = |name: &str, body: &str| {
        let dir = scratch(name);
        put(
            dir.join("src/main.nabi"),
            format!(
                "async fn f(a: i32, b: i32) -> i32 {{ a + b }}\nasync fn main() {{\n{body}}}\n"
            ),
        )
        .unwrap();
        let out = nabic(None, &dir, &["emit", "--emit=hir", "-o", "h.txt"]);
        assert_exit(&out, 0);
        fs::read_to_string(dir.join("h.txt")).unwrap()
    };
    let copy = "fn core::task::spawn__mono__";
    let plain = "fn core::task::spawn_plain__mono__";
    let sole = hir(
        "spawn_sole",
        "    let ts: List<Task<i32>> = [];\n    for i in 0..3 { let g = f(i, 1); ts.push(task::spawn(g)); }\n    \
         for t in ts { println(f\"{await t}\"); }\n",
    );
    assert!(sole.contains(plain) && !sole.contains(copy));
    let aliased = hir(
        "spawn_aliased",
        "    let h = f(9, 1);\n    let k = h;\n    println(f\"{await task::spawn(k)}\");\n",
    );
    assert!(aliased.contains(copy) && !aliased.contains(plain));
}

/// The hash key is drawn per process, so hashes
/// differ between runs, but what a `Dict` prints and writes (insertion
/// order) does not.
#[test]
fn dict_seed_changes_placement_not_output() {
    let env = setup();
    let dir = scratch("dict_seed");
    put(
        dir.join("src/main.nabi"),
        "const C: Dict<str, i32> = {\"q\": 1, \"r\": 2};\n\
         fn main() {\n\
         \x20   let d: Dict<str, i32> = Dict::new();\n\
         \x20   for i in 0..40 { d[f\"k{i}\"] = i; }\n\
         \x20   d.remove(\"k3\");\n\
         \x20   d[\"k3\"] = 3;\n\
         \x20   println(f\"{d:?} {C[\"r\"]} {C.contains_key(\"q\")}\");\n\
         \x20   println(try! std::nabion::to_str(d));\n\
         \x20   let h = core::hash::RandomState::new().build_hasher();\n\
         \x20   h.write_u64(1u64);\n\
         \x20   println(f\"{h.finish()}\");\n\
         }\n",
    )
    .unwrap();
    let out = nabic(Some(&env), &dir, &["build"]);
    assert_exit(&out, 0);
    let exe = dir.join("target/debug").join(exe_name("dict_seed"));
    let run = || {
        let o = Command::new(&exe).output().unwrap();
        assert_exit(&o, 0);
        let s = text(&o.stdout);
        let (body, seed) = s.trim_end().rsplit_once('\n').unwrap();
        (body.to_owned(), seed.to_owned())
    };
    let (a, seed_a) = run();
    let (b, seed_b) = run();
    assert_eq!(a, b);
    assert!(
        a.starts_with("{\"k0\": 0, \"k1\": 1, \"k2\": 2, \"k4\": 4,"),
        "{a}"
    );
    assert!(a.contains("\"k39\": 39, \"k3\": 3} 2 true"), "{a}");
    assert_ne!(seed_a, seed_b);
}

#[test]
fn emit_expanded_prints_meta_expansions() {
    // `--emit=expansions` lists the nested expansion of `Tag`'s derive
    // (used at 11:5) after the attribute's.
    let dir = scratch("expanded");
    let src = "trait Named { fn type_name() -> str; }\n\
               #[impl_derive(Named)]\n\
               pub @derive {\n    impl Named for @Self {\n        \
               fn type_name() -> str { return @Self.as_str(); }\n    }\n}\n\
               #[impl_attribute(with_tag)]\n\
               pub @attribute {\n    @item\n    #[derive(Named)]\n    struct Tag {}\n}\n\
               #[with_tag]\nfn f() -> i32 { 1 }\n\
               println(Tag::type_name());\n";
    put(dir.join("m.nabi"), src).unwrap();
    let out = nabic(
        None,
        &dir,
        &[
            "build",
            "m.nabi",
            "--emit=expansions",
            "-o",
            "m.expanded.nabi",
        ],
    );
    assert_exit(&out, 0);
    let got = fs::read_to_string(dir.join("m.expanded.nabi")).unwrap();
    let want = "// expanded from this `#[with_tag]` (m.nabi:14:1)\n\
                fn f() -> i32 {\n    1\n}\n\
                #[derive(Named)]\nstruct Tag {\n}\n\n\
                // expanded from this `#[derive(Named)]` (m.nabi:11:5)\n\
                impl Named for Tag {\n    fn type_name() -> str {\n        \
                return \"Tag\";\n    }\n}\n";
    assert_eq!(got, want);
}

#[test]
fn overflow_checks_on_in_every_profile() {
    let env = setup();
    let dir = scratch("ovf");
    put(
        dir.join("ovf.nabi"),
        "fn add(a: i32, b: i32) -> i32 { return a + b; }\nprintln(f\"{add(2147483647, 1)}\");\n",
    )
    .unwrap();
    let run = |args: &[&str]| build_run(Some(&env), &dir, &[&["ovf.nabi"], args].concat());
    assert_exit(&run(&[]), 101);
    assert_exit(&run(&["--release"]), 101);
    assert_exit(&run(&["-O2", "-C", "overflow-checks"]), 101);
    assert_exit(&run(&["-O0", "-C", "overflow-checks=no"]), 0);
    assert_exit(&run(&["--release", "-C", "overflow-checks=no"]), 0);
    // `nabi` passes the manifest's profiles; `-C` beats them.
    let off = ["--no-overflow-checks-in=release"];
    assert_exit(&run(&off), 101);
    assert_exit(&run(&[&off[..], &["--release"]].concat()), 0);
    assert_exit(
        &run(&[&off[..], &["--release", "-C", "overflow-checks"]].concat()),
        101,
    );
    assert_exit(&run(&["-C", "bogus"]), 2);
}

/// A release build folds identical code, the thunks of function values
/// too; `-C icf` turns it on and off in either profile, and the program
/// does the same.
#[test]
fn identical_code_folding_flag() {
    let env = setup();
    let dir = scratch("icf");
    put(
        dir.join("icf.nabi"),
        "fn sum_a(n: i64) -> i64 {\n    let s: i64 = 0;\n    for i in 0..n {\n        s += i * 3;\n    }\n    return s;\n}\n\
         fn sum_b(n: i64) -> i64 {\n    let s: i64 = 0;\n    for i in 0..n {\n        s += i * 3;\n    }\n    return s;\n}\n\
         fn apply(f: fn(i64) -> i64, x: i64) -> i64 { f(x) }\n\
         fn low_a(v: i64) -> i64 { apply(|x: i64| x & 7, v) }\n\
         fn low_b(v: i64) -> i64 { apply(|x: i64| x & 7, v) }\n\
         println(f\"{sum_a(10)} {sum_b(20)} {low_a(9) + low_b(10)}\");\n",
    )
    .unwrap();
    let mut sizes = Vec::new();
    for args in [
        &["-O2"][..],
        &["-O2", "-C", "icf=no"],
        &["-C", "icf"],
        &["-C", "icf=no"],
    ] {
        let exe = dir.join(exe_name(&format!("icf{}", sizes.len())));
        let out = nabic(
            Some(&env),
            &dir,
            &[&["build", "icf.nabi", "-o", exe.to_str().unwrap()], args].concat(),
        );
        assert_exit(&out, 0);
        let run = Command::new(&exe).output().unwrap();
        assert_eq!(text(&run.stdout), "135 570 3\n");
        sizes.push(fs::metadata(&exe).unwrap().len());
    }
    assert!(sizes[0] <= sizes[1] && sizes[2] <= sizes[3], "{sizes:?}");
    assert_exit(
        &nabic(Some(&env), &dir, &["build", "icf.nabi", "-C", "icf=maybe"]),
        2,
    );
}

/// `--out` is not an option: `-o` and `--output` name the output.
#[test]
fn out_flag_removed() {
    let dir = scratch("out-flag");
    put(dir.join("src/main.nabi"), "fn main() {}\n").unwrap();
    let out = nabic(None, &dir, &["emit", "--emit=hir", "--out", "a.hir"]);
    assert_exit(&out, 2);
    assert!(
        text(&out.stderr).contains("unknown option `--out`"),
        "{}",
        text(&out.stderr)
    );
    assert!(!dir.join("a.hir").exists());
    for f in ["-o", "--output"] {
        let out = nabic(None, &dir, &["emit", "--emit=hir", f, "b.hir"]);
        assert_exit(&out, 0);
        assert!(
            !text(&out.stderr).contains("warning"),
            "{}",
            text(&out.stderr)
        );
    }
}

/// `--emit=source` and `--emit=expansions`; `ast` and `expanded` are
/// not emit kinds.
#[test]
fn emit_names() {
    let dir = scratch("emit-names");
    put(
        dir.join("src/main.nabi"),
        "#[derive(Clone)]\nstruct P { x: i32; }\nfn main() {}\n",
    )
    .unwrap();
    for (new, old) in [("source", "ast"), ("expansions", "expanded")] {
        let out = nabic(
            None,
            &dir,
            &["emit", &format!("--emit={new}"), "-o", "n.nabi"],
        );
        assert_exit(&out, 0);
        assert!(
            !text(&out.stderr).contains("warning"),
            "{}",
            text(&out.stderr)
        );
        let out = nabic(
            None,
            &dir,
            &["emit", &format!("--emit={old}"), "-o", "o.nabi"],
        );
        assert_exit(&out, 2);
        assert!(
            text(&out.stderr).contains(&format!("unknown emit kind `{old}`")),
            "{}",
            text(&out.stderr)
        );
    }
}

/// A debug nabic brackets each call to a `#[no_safepoint]` runtime
/// function with `nabi_nosp_enter` / `nabi_nosp_leave`; a release one
/// emits neither (the test and nabic share a profile).
#[test]
fn debug_nabic_brackets_no_safepoint_calls() {
    let dir = scratch("nosp");
    put(
        dir.join("h.nabi"),
        "fn main() {\n    let s = \"hello\";\n    println(f\"{s.len()}\");\n}\n",
    )
    .unwrap();
    let out = nabic(None, &dir, &["build", "h.nabi", "--emit=obj", "-o", "h.o"]);
    assert_exit(&out, 0);
    let o = fs::read(dir.join("h.o")).unwrap();
    let has = |s: &[u8]| o.windows(s.len()).any(|w| w == s);
    assert!(has(b"nabi_str_len"));
    assert_eq!(has(b"nabi_nosp_enter"), cfg!(debug_assertions));
    assert_eq!(has(b"nabi_nosp_leave"), cfg!(debug_assertions));
    let _ = fs::remove_dir_all(&dir);
}
