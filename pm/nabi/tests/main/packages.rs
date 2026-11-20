//! Library and binary packages from the templates.

#[allow(clippy::wildcard_imports)]
use crate::*;

/// Every `nabi new` template builds, runs and tests as written.
#[test]
fn new_templates_build_run_and_test() {
    let root = scratch("new-templates");
    let home = root.join("home");
    for (args, name, runs) in [
        (&["new"][..], "plain", "Hello, world!\n"),
        (&["new", "--lib"][..], "onlylib", ""),
        (&["new", "--lib", "--bin"][..], "my-calc", "2 + 2 = 4\n"),
    ] {
        let mut new = args.to_vec();
        new.push(name);
        ok(&nabi(&home, &root, &new));
        let dir = root.join(name);
        ok(&nabi(&home, &dir, &["build"]));
        ok(&nabi(&home, &dir, &["build", "--release"]));
        if !runs.is_empty() {
            assert_eq!(script_out(&home, &dir, &["run"]), runs, "{name}");
        }
        let tests = script_out(&home, &dir, &["test"]);
        assert!(tests.contains("test result: ok"), "{name}: {tests}");
    }
    // The executable reaches its library's items and modules by the
    // library's code name (`my-calc` -> `my_calc`).
    let calc = root.join("my-calc");
    write(
        &calc.join("src/lib.nabi"),
        "pub mod ops;\n\npub fn add(a: i32, b: i32) -> i32 {\n    return a + b;\n}\n\n\
         pub struct Pair {\n    pub a: i32;\n    pub b: i32;\n}\n",
    );
    write(
        &calc.join("src/ops.nabi"),
        "pub fn mul(a: i32, b: i32) -> i32 {\n    return a * b;\n}\n",
    );
    write(
        &calc.join("src/main.nabi"),
        "use my_calc::{Pair, add};\nuse my_calc::ops::mul;\n\n\
         let p = Pair { a: 3, b: 4 };\nprintln(f\"{add(p.a, p.b)} {mul(p.a, p.b)} {my_calc::ops::mul(2, 5)}\");\n",
    );
    assert_eq!(script_out(&home, &calc, &["run"]), "7 12 10\n");
}

/// A package with `src/lib.nabi` and `src/main.nabi`: `nabi new
/// --lib --bin` makes one, a dependent gets its library, `nabi install`
/// installs its executable (`--lib`: its library), and `nabi publish`
/// takes it, as it takes an executable-only package; a registry package without a
/// library is not a dependency.
#[test]
fn lib_and_main_packages() {
    let root = scratch("lib-and-main");
    let home = root.join("home");
    let out = nabi(&home, &root, &["new", "--lib", "--bin", "both"]);
    ok(&out);
    assert!(
        text(&out.stderr).contains("Created library and executable package"),
        "{}",
        text(&out.stderr)
    );
    let both = root.join("both");
    assert!(read(&both.join("src/lib.nabi")).contains("pub fn add"));
    assert_eq!(
        read(&both.join("src/main.nabi")),
        "use both::add;\n\nprintln(f\"2 + 2 = {add(2, 2)}\");\n"
    );
    assert_eq!(script_out(&home, &both, &["run"]), "2 + 2 = 4\n");
    let tests = script_out(&home, &both, &["test"]);
    assert!(tests.contains("test adds ... ok"), "{tests}");
    assert!(both.join("target/debug/libboth.a").is_file());

    // A dependent sees the library.
    let user = root.join("user");
    write(
        &user.join("nabi.nabion"),
        "Package { name: \"user\", deps: { \"both\": Dep::path(\"../both\") } }\n",
    );
    write(
        &user.join("src/main.nabi"),
        "use both::add;\nprintln(f\"{add(1, 2)}\");\n",
    );
    assert_eq!(script_out(&home, &user, &["run"]), "3\n");

    // `nabi install`: an error naming --bin and --lib; `--bin` installs
    // the executable, `--lib` the library.
    fails(
        &nabi(&home, &root, &["install", "--path", "both"]),
        "`both` has an executable and a library (src/lib.nabi): write `nabi install --bin ..` for \
         the executable, or `nabi install --lib ..` to put the library into environment \
         `default` (for `nabi repl` / `nabi jupyter`)",
    );
    assert!(!home.join("bin").join(exe("both")).exists());
    let out = nabi(&home, &root, &["install", "--bin", "--path", "both"]);
    ok(&out);
    assert!(
        !text(&out.stderr).contains("warning"),
        "{}",
        text(&out.stderr)
    );
    let installed = Command::new(home.join("bin").join(exe("both")))
        .output()
        .unwrap();
    assert_eq!(ok(&installed), "2 + 2 = 4\n");
    fails(
        &nabi(
            &home,
            &root,
            &["install", "--bin", "--lib", "--path", "both"],
        ),
        "`--lib` and `--bin` go apart",
    );
    write(
        &root.join("lone/nabi.nabion"),
        "Package { name: \"lone\" }\n",
    );
    write(&root.join("lone/src/lib.nabi"), "pub fn f() {}\n");
    fails(
        &nabi(&home, &root, &["install", "--bin", "--path", "lone"]),
        "`lone` has no executable (src/main.nabi) for --bin; without --bin, its library goes \
         into environment `default`",
    );
    ok(&nabi(&home, &root, &["install", "--lib", "--path", "both"]));
    assert!(read(&home.join("envs/default/nabi.nabion")).contains("\"both\": Dep::path("));
    // For the REPL (`nabi deps` outside a package).
    assert!(deps(&home, &root, &[]).iter().any(|(n, _)| n == "both"));
    write(&root.join("s.nabi"), "println(7);\n");
    let list = ok(&nabi(&home, &root, &["install", "--list"]));
    assert_eq!(list.matches("  both ").count(), 2, "{list}");
    let hello = root.join("hello");
    write(&hello.join("src/main.nabi"), "println(\"hello\");\n");
    fails(
        &nabi(&home, &root, &["install", "--lib", "--path", "hello"]),
        "`hello` has no library (src/lib.nabi) for --lib; without --lib, its executable \
         (src/main.nabi) goes into",
    );
    fails(
        &nabi(&home, &root, &["install", "--lib", "--path", "s.nabi"]),
        "`s.nabi` is a script: --lib installs a package's library (src/lib.nabi)",
    );

    // Registry packages: one with both, and one with only an executable.
    let index = root.join("index");
    commit(
        &index,
        &[("config.nabion", "Config { api: \"http://127.0.0.1:1\" }\n")],
    );
    for kv in [
        ["user.name", "t"],
        ["user.email", "t@t"],
        ["commit.gpgsign", "false"],
    ] {
        git(&index, &["config", kv[0], kv[1]]);
    }
    let reg = index.display().to_string();
    mirror(&home, &reg);
    let calc = root.join("calc");
    let main = "use calc::one;\nprintln(f\"calc {one()}\");\n";
    package(
        &calc,
        "calc",
        "1.0.0",
        "",
        &[("src/lib.nabi", ONE), ("src/main.nabi", main)],
    );
    let tool = root.join("tool");
    package(
        &tool,
        "tool",
        "0.1.0",
        "",
        &[("src/main.nabi", "println(\"tool\");\n")],
    );
    for dir in [&calc, &tool] {
        let line = ok(&nabi(
            &home,
            dir,
            &["publish", "--index-dir", &reg, "--dry-run"],
        ));
        assert!(line.starts_with("Package { name: "), "{line}");
        ok(&nabi(&home, dir, &["publish", "--index-dir", &reg]));
    }
    let empty = root.join("empty");
    package(&empty, "empty", "0.1.0", "", &[("README", "")]);
    fails(
        &nabi(&home, &empty, &["publish", "--dry-run"]),
        "`empty` is not a package: no src/lib.nabi (a library) or src/main.nabi (an executable)",
    );
    let installed = nabi(&home, &root, &["install", "tool"]);
    assert!(
        text(&installed.stderr).contains("   Installed tool 0.1.0 ("),
        "{}",
        text(&installed.stderr)
    );
    fails(
        &nabi(&home, &root, &["install", "calc"]),
        "`calc` has an executable and a library",
    );
    ok(&nabi(&home, &root, &["install", "--bin", "calc"]));
    // Reinstalling: the same version is kept unless --force; another
    // version, newer or older, replaces it.
    let says = |args: &[&str], want: &str| {
        let out = nabi(&home, &root, args);
        assert!(text(&out.stderr).contains(want), "{}", text(&out.stderr));
    };
    says(&["install", "tool"], "`tool` 0.1.0 is installed already (");
    says(
        &["install", "tool"],
        "to rebuild it, run `nabi install tool --force`",
    );
    says(&["install", "tool", "--force"], " Reinstalled tool 0.1.0 (");
    package(
        &tool,
        "tool",
        "0.2.0",
        "",
        &[("src/main.nabi", "println(\"tool\");\n")],
    );
    ok(&nabi(&home, &tool, &["publish", "--index-dir", &reg]));
    says(
        &["install", "tool"],
        "    Upgraded tool from 0.1.0 to 0.2.0 (",
    );
    says(
        &["install", "tool@0.1"],
        "  Downgraded tool from 0.2.0 to 0.1.0 (",
    );
    for (name, want) in [("tool", "tool\n"), ("calc", "calc 1\n")] {
        let out = Command::new(home.join("bin").join(exe(name)))
            .output()
            .unwrap();
        assert_eq!(ok(&out), want);
    }
    ok(&nabi(&home, &root, &["install", "--lib", "calc"]));
    assert!(
        read(&home.join("envs/default/nabi.nabion")).contains("\"calc\": Dep::version(\"^1.0.0\")")
    );
    write(
        &root.join("c.nabi"),
        "use calc::one;\nprintln(f\"{one()}\");\n",
    );
    ok(&nabi(&home, &root, &["add", "calc", "c.nabi"]));
    assert_eq!(script_out(&home, &root, &["c.nabi"]), "1\n");
    fails(
        &nabi(&home, &root, &["install", "--lib", "tool"]),
        "`tool` has no library (src/lib.nabi) for --lib",
    );
    // An executable-only package is no dependency.
    let app = root.join("app");
    write(
        &app.join("nabi.nabion"),
        "Package { name: \"app\", deps: { \"tool\": Dep::version(\"^0.1\") } }\n",
    );
    write(&app.join("src/main.nabi"), "println(\"app\");\n");
    fails(
        &nabi(&home, &app, &["build"]),
        "is an executable package (src/main.nabi, no src/lib.nabi): a dependency must be a \
         library; a program is installed with `nabi install`",
    );
}
