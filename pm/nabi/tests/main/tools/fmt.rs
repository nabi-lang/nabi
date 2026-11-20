//! `nabi fmt`.

#[allow(clippy::wildcard_imports)]
use crate::tools::*;

#[test]
fn fmt_check_and_rewrite() {
    let dir = scratch("fmt");
    let messy = "fn f() {\n  // keep me\n      let x = 1;   \n\n\n  x;\n}\n";
    let clean = "fn f() {\n    // keep me\n    let x = 1;\n\n    x;\n}\n";
    fs::create_dir_all(dir.join("target")).unwrap();
    put(dir.join("a.nabi"), messy).unwrap();
    put(dir.join("target/skip.nabi"), messy).unwrap();

    let out = nabi_in(None, &dir, &["fmt", "--check", "."]);
    assert_exit(&out, 1);
    assert!(text(&out.stdout).contains("a.nabi"));
    assert!(
        text(&out.stderr).contains("1 file needs formatting; `nabi fmt` without `--check`"),
        "{}",
        text(&out.stderr)
    );
    assert_eq!(fs::read_to_string(dir.join("a.nabi")).unwrap(), messy);

    let out = nabi_in(None, &dir, &["fmt", "."]);
    assert_exit(&out, 0);
    assert_eq!(text(&out.stderr), "   Formatted 1 file\n");
    assert_eq!(fs::read_to_string(dir.join("a.nabi")).unwrap(), clean);
    assert_eq!(
        fs::read_to_string(dir.join("target/skip.nabi")).unwrap(),
        messy
    );
    assert_exit(&nabi_in(None, &dir, &["fmt", "--check", "a.nabi"]), 0);

    // `.nabion` data files too: re-indented, and checked to be NABION.
    // With a nabi.nabion, the directory is a package: `fmt` alone is it.
    put(
        dir.join("nabi.nabion"),
        "Package {\n  deps: {}, // none\n}\n",
    )
    .unwrap();
    assert_exit(&nabi_in(None, &dir, &["fmt"]), 0);
    assert_eq!(
        fs::read_to_string(dir.join("nabi.nabion")).unwrap(),
        "Package {\n    deps: {}, // none\n}\n"
    );
    put(dir.join("bad.nabion"), "Package { name: x }\n").unwrap();
    let out = nabi_in(None, &dir, &["fmt", "--check", "."]);
    assert_exit(&out, 1);
    assert!(
        text(&out.stderr).contains("error[E0010]: bare identifier `x`")
            && text(&out.stderr).contains("bad.nabion:1:17\n"),
        "{}",
        text(&out.stderr)
    );
}

/// A file saved with CRLF is formatted to LF line ends;
/// a line break inside a string literal is its value and stays.
#[test]
fn fmt_writes_lf_and_keeps_literal_crlf() {
    let dir = scratch("fmt-crlf");
    let crlf = "fn f() {\r\n    let s = \"a\r\nb\";\r\n}\r\n";
    put(dir.join("w.nabi"), crlf).unwrap();
    assert_exit(&nabi_in(None, &dir, &["fmt", "--check", "w.nabi"]), 1);
    assert_exit(&nabi_in(None, &dir, &["fmt", "w.nabi"]), 0);
    assert_eq!(
        fs::read_to_string(dir.join("w.nabi")).unwrap(),
        "fn f() {\n    let s = \"a\r\nb\";\n}\n"
    );
}

/// `nabi <args>` in `cwd` with `HOME` (and `USERPROFILE`) at `home`;
/// panics if it runs past a minute.
pub(crate) fn nabi_within(cwd: &Path, home: &Path, args: &[&str]) -> Output {
    let mut child = Command::new(NABI)
        .current_dir(cwd)
        .env("NABI_HOME", tool_home())
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env_remove("CLICOLOR_FORCE")
        .env_remove("__NABI_CLI")
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let start = std::time::Instant::now();
    while child.try_wait().unwrap().is_none() {
        if start.elapsed() > std::time::Duration::from_secs(60) {
            let _ = child.kill();
            panic!(
                "nabi {args:?} in {} still running after 60 s",
                cwd.display()
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    child.wait_with_output().unwrap()
}

/// A home-like directory: no package, a big `node_modules/`, and
/// `.nabi` files deep down.
pub(crate) fn home_like(dir: &Path) {
    for i in 0..200 {
        put(
            dir.join(format!("node_modules/m{i}/x.nabi")),
            "fn f() {\n  1;\n}\n",
        )
        .unwrap();
    }
    put(dir.join("Library/Caches/a/b/c.nabi"), "fn f() {}\n").unwrap();
    put(dir.join(".cache/d.nabi"), "fn f() {\n  1;\n}\n").unwrap();
    put(dir.join("work/target/e.nabi"), "fn f() {\n  1;\n}\n").unwrap();
}

/// Outside a package `fmt` needs a path: from a home directory or an
/// empty one it says so at once, and writes nothing.
#[test]
fn fmt_outside_package_needs_path() {
    let root = scratch("fmt-outside");
    let home = root.join("home");
    home_like(&home);
    let empty = root.join("empty");
    fs::create_dir_all(&empty).unwrap();
    for (cwd, args) in [
        (&home, &["fmt"][..]),
        (&home, &["fmt", "--check"]),
        (&empty, &["fmt"]),
    ] {
        let out = nabi_within(cwd, &home, args);
        assert_exit(&out, 1);
        assert_eq!(
            text(&out.stderr),
            "nabi: error: not inside a package; pass a file or directory: `nabi fmt <path>`\n"
        );
        assert_eq!(text(&out.stdout), "");
    }
    // Through nabi, the message names `nabi`.
    let out = Command::new(NABI)
        .current_dir(&empty)
        .env("__NABI_CLI", "nabi")
        .arg("fmt")
        .output()
        .unwrap();
    assert_exit(&out, 1);
    assert_eq!(
        text(&out.stderr),
        "nabi: error: not inside a package; pass a file or directory: `nabi fmt <path>`\n"
    );
    assert_eq!(
        fs::read_to_string(home.join("node_modules/m0/x.nabi")).unwrap(),
        "fn f() {\n  1;\n}\n"
    );
}

/// A directory walk skips dot directories, `target/`, `node_modules/`
/// and what the `.gitignore` there or at the package root ignores.
#[test]
fn fmt_skips_node_modules_and_ignored() {
    let root = scratch("fmt-skips");
    let home = root.join("home");
    home_like(&home);
    let messy = "fn f() {\n  1;\n}\n";
    put(home.join("work/a.nabi"), messy).unwrap();
    put(home.join("work/gen/b.nabi"), messy).unwrap();
    put(home.join("work/c.gen.nabi"), messy).unwrap();
    put(home.join("work/.gitignore"), "# made\ngen/\n*.gen.nabi\n").unwrap();
    let out = nabi_within(&home, &home, &["fmt", "--check", "."]);
    assert_exit(&out, 1);
    let diff = text(&out.stdout);
    assert!(diff.contains("a.nabi"), "{diff}");
    for skipped in ["node_modules", "Caches", ".cache", "target"] {
        assert!(!diff.contains(skipped), "{skipped}: {diff}");
    }
    // `.gitignore` at the walk root.
    let out = nabi_within(&home.join("work"), &home, &["fmt", "--check", "."]);
    let diff = text(&out.stdout);
    assert!(diff.contains("a.nabi") && !diff.contains("b.nabi") && !diff.contains("c.gen.nabi"));
    // At the package root, for a walk below it.
    let pkg = root.join("pkg");
    put(pkg.join("src/main.nabi"), "fn main() {}\n").unwrap();
    put(pkg.join("src/gen/x.nabi"), messy).unwrap();
    put(pkg.join("src/y.nabi"), messy).unwrap();
    put(pkg.join(".gitignore"), "/target\ngen/\n").unwrap();
    let out = nabi_within(&pkg, &home, &["fmt", "--check", "src"]);
    let diff = text(&out.stdout);
    assert!(
        diff.contains("y.nabi") && !diff.contains("x.nabi"),
        "{diff}"
    );
}

/// `fmt` says what it did: files formatted, already formatted, or none
/// found; inside a package, without a path, it formats that package from
/// any directory in it.
#[test]
fn fmt_reports_counts() {
    let pkg = scratch("fmt-counts");
    let messy = "fn f() {\n  1;\n}\n";
    put(pkg.join("src/main.nabi"), "fn main() {}\n").unwrap();
    put(pkg.join("src/m/a.nabi"), messy).unwrap();
    put(pkg.join("tests/t.nabi"), messy).unwrap();
    put(pkg.join("benches/b.nabi"), messy).unwrap();
    put(pkg.join("examples/e.nabi"), "fn main() {}\n").unwrap();
    put(pkg.join("old.nabi"), messy).unwrap();
    let out = nabi_within(&pkg.join("src/m"), &pkg, &["fmt"]);
    assert_exit(&out, 0);
    assert_eq!(
        text(&out.stderr),
        "   Formatted 3 files, 2 already formatted\n"
    );
    assert_eq!(fs::read_to_string(pkg.join("old.nabi")).unwrap(), messy);
    let out = nabi_within(&pkg, &pkg, &["fmt"]);
    assert_eq!(
        text(&out.stderr),
        "        note: 5 files already formatted\n"
    );
    let out = nabi_within(&pkg, &pkg, &["-q", "fmt"]);
    assert_eq!(text(&out.stderr), "");
    // `--check` keeps exit 1 and says how to fix it.
    put(pkg.join("src/m/a.nabi"), messy).unwrap();
    let out = nabi_within(&pkg, &pkg, &["fmt", "--check"]);
    assert_exit(&out, 1);
    assert!(
        text(&out.stderr)
            .ends_with("note: 1 file needs formatting; `nabi fmt` without `--check` rewrites it\n"),
        "{}",
        text(&out.stderr)
    );
}

#[test]
fn fmt_empty_dir_says_so() {
    let dir = scratch("fmt-empty");
    fs::create_dir_all(dir.join("sub")).unwrap();
    let out = nabi_within(&dir, &dir, &["fmt", "sub"]);
    assert_exit(&out, 0);
    assert_eq!(
        text(&out.stderr),
        "        note: no .nabi or .nabion files under sub\n"
    );
    let out = nabi_within(&dir, &dir, &["fmt", "nope.nabi"]);
    assert_exit(&out, 1);
    assert!(text(&out.stderr).contains("`nope.nabi` does not exist"));
}

/// `fmt --diff` shows the old line before the new one.
#[test]
fn fmt_diff_old_before_new() {
    let dir = scratch("fmt-diff-order");
    put(dir.join("a.nabi"), "fn f() {\nlet x = 1;\n}\n").unwrap();
    let out = nabi_in(None, &dir, &["fmt", "--diff", "a.nabi"]);
    let s = text(&out.stdout);
    let old = s.find("-    2 let x = 1;\n").expect(&s);
    let new = s.find("+    2     let x = 1;\n").expect(&s);
    assert!(old < new, "{s}");
}
