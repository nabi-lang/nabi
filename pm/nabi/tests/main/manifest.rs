//! Manifests: errors, lints, profiles and names.

#[allow(clippy::wildcard_imports)]
use crate::*;

#[test]
fn manifest_errors_and_reserved_forms() {
    let root = scratch("manifest");
    let home = root.join("home");
    let app = root.join("app");
    write(&app.join("src/main.nabi"), "println(\"x\");\n");
    write(
        &app.join("nabi.nabion"),
        "Package {\n    deps: { \"foo\": \"1.0.x.y\" },\n}\n",
    );
    let out = nabi(&home, &app, &["deps"]);
    fails(&out, "is not a version requirement");
    assert!(
        text(&out.stderr).contains("nabi.nabion:2:20"),
        "{}",
        text(&out.stderr)
    );
    write(
        &app.join("nabi.nabion"),
        "Package { deps: { \"foo\": Dep::native(\"z\") } }\n",
    );
    fails(
        &nabi(&home, &app, &["deps"]),
        "a native package goes in the `native` field",
    );
    write(
        &app.join("nabi.nabion"),
        "Package {\n    name: env(\"X\"),\n}\n",
    );
    let out = nabi(&home, &app, &["deps"]);
    fails(&out, "error[E0010]: calls are not allowed in NABION");
    fails(&out, "nabi.nabion:2:11\n");
    write(&app.join("nabi.nabion"), "Package { deps: {\n");
    fails(&nabi(&home, &app, &["deps"]), "nabi.nabion:2:1\n");
    write(&app.join("nabi.nabion"), "Package {}\n");
    ok(&nabi(&home, &app, &["deps"]));
    fs::remove_file(app.join("nabi.nabion")).unwrap();

    // A wrongly cased manifest is ignored with a warning.
    write(&app.join("Nabi.nabion"), "Package { deps: { \"x\": 1 } }\n");
    let out = nabi(&home, &app, &["deps"]);
    ok(&out);
    assert!(
        text(&out.stderr).contains("the manifest is named `nabi.nabion` (lowercase)"),
        "{}",
        text(&out.stderr)
    );
}

#[test]
fn manifest_errors_say_what_to_write() {
    let root = scratch("manifest-help");
    let home = root.join("home");
    let app = root.join("app");
    write(&app.join("src/main.nabi"), "println(\"x\");\n");
    let example = "   = help: a `nabi.nabion` holds one `Package { .. }`: `Package { name: \"app\", \
                   version: \"0.1.0\", deps: {} }`";
    // TOML, JSON.
    write(&app.join("nabi.nabion"), "name = \"app\"\n");
    let out = nabi(&home, &app, &["deps"]);
    fails(
        &out,
        "error[E0010]: expected the end of the file: a NABION file holds one value",
    );
    fails(&out, example);
    write(&app.join("nabi.nabion"), "{\"name\": \"app\"}\n");
    let out = nabi(&home, &app, &["deps"]);
    fails(
        &out,
        "error[E0097]: expected `Package { .. }`, found a dict",
    );
    fails(&out, example);
    // A path where a version requirement goes.
    write(
        &app.join("nabi.nabion"),
        "Package { deps: { \"geom\": \"../geom\" } }\n",
    );
    let out = nabi(&home, &app, &["deps"]);
    fails(
        &out,
        "error[E0097]: `../geom` is a path, not a version requirement",
    );
    fails(
        &out,
        "   = help: a path dependency is `Dep::path(\"../geom\")`",
    );
    // A field without its `,`.
    write(
        &app.join("nabi.nabion"),
        "Package { name: \"app\" version: \"0.1.0\" }\n",
    );
    let out = nabi(&home, &app, &["deps"]);
    fails(
        &out,
        "error[E0010]: expected `,` or `}`, found field `version`",
    );
    fails(
        &out,
        "   = help: separate the fields with `,`: `name: \"app\", version: ..`",
    );
    // A path dependency that is not there.
    write(
        &app.join("nabi.nabion"),
        "Package { deps: { \"geom\": Dep::path(\"../nope\") } }\n",
    );
    fails(
        &nabi(&home, &app, &["deps"]),
        "dependency `geom`: no directory `../nope` (",
    );
    fails(
        &nabi(&home, &app, &["deps"]),
        "); a `Dep::path` is relative to the directory of the `nabi.nabion` that names it",
    );
}

#[test]
fn new_and_init_write_a_manifest() {
    let root = scratch("new");
    let home = root.join("home");
    ok(&nabi(&home, &root, &["new", "--lib", "mylib"]));
    assert!(read(&root.join("mylib/src/lib.nabi")).contains("#[test]"));
    assert!(!root.join("mylib/lib.nabi").exists());
    ok(&nabi(&home, &root, &["new", "mybin"]));
    assert_eq!(
        read(&root.join("mybin/src/main.nabi")),
        "println(\"Hello, world!\");\n"
    );
    let mut files: Vec<String> = fs::read_dir(root.join("mybin"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    files.sort();
    assert_eq!(files, [".git", ".gitignore", "nabi.nabion", "src"]);
    assert_eq!(
        read(&root.join("mylib/nabi.nabion")),
        format!(
            "Package {{ name: \"mylib\", version: \"0.1.0\", nabi_version: \"{}\", toolchain: \
             \"{}\", deps: {{}} }}\n",
            nabi_version(),
            series()
        )
    );
    // Its own git repository, ignoring target/.
    assert_eq!(read(&root.join("mylib/.gitignore")), "target/\n");
    assert!(root.join("mylib/.git").is_dir());
    // Package names: `[a-z][a-z0-9_-]*`.
    fails(&nabi(&home, &root, &["new", "My App"]), "try `my-app`");
    assert!(!root.join("My App").exists());
    // The standard library's tiers' names are taken.
    fails(
        &nabi(&home, &root, &["new", "--lib", "std"]),
        "`std` is the standard library, which ships with the toolchain; name the package \
         something else, e.g. `std_util`",
    );
    assert!(!root.join("std").exists());
    let dir = root.join("Existing.Dir");
    fs::create_dir_all(&dir).unwrap();
    // Inside a repository: no new one.
    git(&dir, &["init", "-q"]);
    let out = nabi(&home, &dir, &["init"]);
    ok(&out);
    assert!(text(&out.stderr).contains("the package name is `existing-dir`"));
    assert!(dir.join("src/main.nabi").is_file());
    assert!(read(&dir.join("nabi.nabion")).contains("name: \"existing-dir\""));
    fs::create_dir_all(dir.join("sub")).unwrap();
    ok(&nabi(&home, &dir.join("sub"), &["new", "inner"]));
    assert!(!dir.join("sub/inner/.git").exists());
    fails(&nabi(&home, &dir, &["init"]), "already exists");
    let version = ok(&nabi(&home, &dir, &["--version"]));
    assert!(
        version.starts_with(&format!("nabi {}", env!("CARGO_PKG_VERSION"))),
        "{version}"
    );
    // Usage errors exit 2; the rest 1.
    let usage = |args: &[&str]| nabi(&home, &root, args).status.code();
    assert_eq!(usage(&["new"]), Some(2));
    assert_eq!(usage(&["add", "x", "--frob"]), Some(2));
    assert_eq!(usage(&["frob"]), Some(2));
    assert_eq!(usage(&["toolchain", "frob"]), Some(2));
    // A command on a package, outside one: nabi says so.
    let out = nabi(&home, &root, &["check"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).starts_with("nabi: error: not inside a package"),
        "{}",
        text(&out.stderr)
    );
    // Per-command help.
    let help = ok(&nabi(&home, &root, &["add", "--help"]));
    assert!(help.contains("add <name> --path <dir>") && !help.contains("publish"));
    assert_eq!(ok(&nabi(&home, &root, &["help", "add"])), help);
    let help = ok(&nabi(&home, &root, &["test", "--help"]));
    assert!(
        help.contains("--threads") && !help.contains("--iters"),
        "{help}"
    );
}

/// Overflow checks are on in both profiles; `profile` in nabi.nabion turns
/// them off for one, and `-C overflow-checks` beats it.
/// The standard library is no dependency: a `deps` line for it says to
/// remove it, `nabi add std` that there is nothing to add; a tier's name
/// with `package:` is another package's name in code.
#[test]
fn standard_library_is_no_dependency() {
    let root = scratch("manifest-std");
    let home = root.join("home");
    let app = root.join("app");
    write(&app.join("src/main.nabi"), "println(\"x\");\n");
    for (key, line) in [
        ("std", "\"std\": \"0.1\""),
        ("core", "\"core\": Dep::path(\"../h\")"),
    ] {
        write(
            &app.join("nabi.nabion"),
            &format!("Package {{ name: \"app\", deps: {{ {line} }} }}\n"),
        );
        let out = nabi(&home, &app, &["deps"]);
        fails(
            &out,
            &format!("`{key}` is the standard library, not a package to depend on"),
        );
        fails(
            &out,
            &format!("= help: {key} ships with the toolchain; remove this line"),
        );
    }
    write(
        &app.join("nabi.nabion"),
        "Package { name: \"app\", deps: { \"x\": Dep::path(\"../h\", package: \"bare\") } }\n",
    );
    fails(
        &nabi(&home, &app, &["deps"]),
        "= help: bare ships with the toolchain; remove this line",
    );
    write(
        &app.join("nabi.nabion"),
        "Package { name: \"app\", deps: {} }\n",
    );
    fails(
        &nabi(&home, &app, &["add", "std"]),
        "`std` is the standard library, which ships with the toolchain: there is nothing to add",
    );
    // Another package under a tier's name: `std::` is it, `::std::` the library.
    let h = root.join("h");
    write(
        &h.join("nabi.nabion"),
        "Package { name: \"h\", version: \"0.1.0\" }\n",
    );
    write(&h.join("src/lib.nabi"), "pub fn answer() -> i32 { 42 }\n");
    write(
        &app.join("nabi.nabion"),
        "Package { name: \"app\", deps: { \"std\": Dep::path(\"../h\", package: \"h\") } }\n",
    );
    write(
        &app.join("src/main.nabi"),
        "fn main() { println(f\"{std::answer()} {::std::path::Path::empty().is_relative()}\"); }\n",
    );
    assert_eq!(ok(&nabi(&home, &app, &["run", "-q"])), "42 true\n");
}

#[test]
fn manifest_profile_overflow_checks() {
    let root = scratch("ovfprofile");
    let home = root.join("home");
    let app = root.join("app");
    write(
        &app.join("nabi.nabion"),
        "Package {\n    name: \"app\",\n    \
         profile: { \"release\": { overflow_checks: false } },\n}\n",
    );
    write(
        &app.join("src/main.nabi"),
        "fn add(a: i32, b: i32) -> i32 { a + b }\nprintln(f\"{add(2147483647, 1)}\");\n",
    );
    fails(
        &nabi(&home, &app, &["-q", "run"]),
        "attempt to add with overflow",
    );
    assert_eq!(
        ok(&nabi(&home, &app, &["-q", "run", "--release"])),
        "-2147483648\n"
    );
    fails(
        &nabi(
            &home,
            &app,
            &["-q", "run", "--release", "-C", "overflow-checks"],
        ),
        "attempt to add with overflow",
    );
}

/// `nabi build` lints through std's `using` cleanup,
/// `File.drop__by_Drop`: that is a close, not a use that leaks.
#[test]
fn build_lints_using_through_std() {
    let root = scratch("usinglint");
    let home = root.join("home");
    let app = root.join("app");
    write(
        &app.join("nabi.nabion"),
        "Package {\n    name: \"app\",\n}\n",
    );
    write(
        &app.join("src/main.nabi"),
        "use std::fs::File;\n\n\
         fn leaks(p: str) -> str {\n    let f = try! File::open(p);\n    try! f.read_to_str()\n}\n\n\
         fn scoped(p: str) -> str {\n    using h = try! File::open(p) { try! h.read_to_str() }\n}\n\n\
         fn main() {\n    println(leaks(\"nabi.nabion\"));\n    println(scoped(\"nabi.nabion\"));\n}\n",
    );
    // A `File` left open is an error (closing can fail); the one bound
    // with `using` is not reported.
    let out = nabi(&home, &app, &["build"]);
    assert!(!out.status.success());
    let err = text(&out.stderr);
    assert!(
        err.contains("`f` holds a `File` that is never closed"),
        "{err}"
    );
    assert!(!err.contains("`h` holds"), "{err}");
}

#[test]
fn lints_from_and_files_in_a_package() {
    let root = scratch("lintsfrom");
    let home = root.join("home");
    let base = "Package {\n    lints: { \"unused_variables\": \"deny\", \"non_snake_case\": \"deny\" },\n}\n";
    write(&root.join("base/nabi.nabion"), base);
    write(
        &root.join("mid/nabi.nabion"),
        "Package {\n    lints_from: \"../base\",\n    lints: { \"non_snake_case\": \"allow\" },\n}\n",
    );
    let app = root.join("app");
    write(
        &app.join("nabi.nabion"),
        "Package { lints_from: \"../mid\" }\n",
    );
    write(
        &app.join("src/main.nabi"),
        "fn BadName() -> i32 { 1 }\nlet x = BadName();\nprintln(\"hi\");\n",
    );
    // base's deny reaches app through mid; mid's allow overrides base's.
    let out = nabi(&home, &app, &["lint"]);
    fails(&out, "unused variable `x`");
    assert!(
        !text(&out.stderr).contains("snake"),
        "{}",
        text(&out.stderr)
    );
    // A file names the package it is in (nabic compiles that package), and
    // a lone file in a directory with only nabi.nabion is linted alone: both
    // use the enclosing manifest's lints, from inside or outside.
    write(&root.join("mid/t.nabi"), "let y = 1;\nprintln(\"t\");\n");
    for (cwd, path, want) in [
        (&app, "src/main.nabi", "unused variable `x`"),
        (&root, "app/src/main.nabi", "unused variable `x`"),
        (&root.join("mid"), "t.nabi", "unused variable `y`"),
        (&root, "mid/t.nabi", "unused variable `y`"),
    ] {
        for cmd in ["lint", "check"] {
            fails(&nabi(&home, cwd, &[cmd, path]), want);
        }
    }
    // This manifest's own entry wins over the inherited one.
    write(
        &app.join("nabi.nabion"),
        "Package {\n    lints_from: \"../mid\",\n    lints: { \"unused_variables\": \"warn\" },\n}\n",
    );
    let out = nabi(&home, &app, &["lint"]);
    ok(&out);
    assert!(text(&out.stderr).contains("unused variable `x`"));
    // An imported environment keeps pointing at the same directory.
    ok(&nabi(&home, &root, &["env", "import", "shared", "app"]));
    let env = read(&home.join("envs/shared/nabi.nabion"));
    assert!(
        env.contains(&format!(
            "lints_from: {:?}",
            root.join("mid").display().to_string()
        )),
        "{env}"
    );

    // Errors: a cycle, a missing directory, a directory without a manifest.
    write(
        &root.join("base/nabi.nabion"),
        "Package { lints_from: \"../app\" }\n",
    );
    fails(&nabi(&home, &app, &["lint"]), "cycle:");
    write(&root.join("base/nabi.nabion"), base);
    write(
        &app.join("nabi.nabion"),
        "Package { lints_from: \"../nope\" }\n",
    );
    fails(&nabi(&home, &app, &["lint"]), "is not a directory");
    fs::create_dir_all(root.join("empty")).unwrap();
    write(
        &app.join("nabi.nabion"),
        "Package { lints_from: \"../empty\" }\n",
    );
    fails(&nabi(&home, &app, &["lint"]), "no nabi.nabion in");
}

/// Files every toolchain in `NABI_HOME` shares skip what a newer nabi
/// wrote, keep it when written, and name it under `-v`.
#[test]
fn config_with_unknown_field_still_works() {
    let root = scratch("config-newer");
    let home = root.join("home");
    write(
        &home.join("config.nabion"),
        "Config { toolchain: \"x\", future: 1 }\n",
    );
    assert_eq!(ok(&nabi(&home, &root, &["toolchain", "default"])), "x\n");
    let out = nabi(&home, &root, &["-v", "toolchain", "default"]);
    assert!(
        text(&out.stderr).contains("config.nabion: skipped `future`, written by a newer nabi"),
        "{}",
        text(&out.stderr)
    );
    assert!(!text(&nabi(&home, &root, &["toolchain", "default"]).stderr).contains("skipped"));
}

#[test]
fn installs_skip_unknown() {
    let root = scratch("installs-newer");
    let home = root.join("home");
    let installs = home.join("installs.nabion");
    write(
        &installs,
        "Package {\n    deps: {\n        \"a\": Dep::path(\"/srv/a\", target: \"t\"),\n        \
         \"b\": Dep::workspace(\"w\"),\n    },\n    bins: {},\n}\n",
    );
    let out = ok(&nabi(&home, &root, &["install", "--list"]));
    assert!(out.contains("executables") && out.contains("  a "), "{out}");
    assert!(!out.contains("  b "), "{out}");
    ok(&nabi(&home, &root, &["uninstall", "a"]));
    let after = read(&installs);
    assert!(!after.contains("\"a\""), "{after}");
    assert!(
        after.contains("Dep::workspace(\"w\")") && after.contains("bins: {}"),
        "{after}"
    );
}

/// A lock from a newer nabi names the toolchain to use, and is never to be
/// deleted.
#[test]
fn newer_lock_version_says_which_toolchain() {
    let root = scratch("lock-newer");
    let home = root.join("home");
    let app = root.join("app");
    write(&app.join("src/main.nabi"), "println(\"x\");\n");
    write(&app.join("nabi.nabion"), "Package { name: \"app\" }\n");
    write(
        &app.join("nabi.lock"),
        "Lock { version: 2, packages: [], v2: {} }\n",
    );
    let out = nabi(&home, &app, &["check"]);
    fails(&out, "was written by a newer nabi (lock version 2)");
    fails(&out, "or run `nabi +<version> ..`");
    assert!(
        !text(&out.stderr).contains("delete"),
        "{}",
        text(&out.stderr)
    );
    assert!(app.join("nabi.lock").is_file());
}

/// Names code can't spell as a path are refused: `use pkg::add;` in an
/// executable is its own root, never a library called `pkg`.
#[test]
fn new_rejects_names_code_cannot_spell() {
    let root = scratch("new-names");
    let home = root.join("home");
    for (name, why) in [
        ("pkg", "`pkg` already names a package's own root (`pkg::`)"),
        ("sup", "`sup` already names the parent module (`sup::`)"),
        ("match", "`match` is a keyword"),
        ("self", "`self` is a keyword"),
    ] {
        let out = nabi(&home, &root, &["new", "--lib", "--bin", name]);
        fails(&out, why);
        assert!(text(&out.stderr).contains(&format!("e.g. `{name}_util`")));
        assert!(!root.join(name).exists());
    }
    // Names no file can carry on some OS.
    for (name, why) in [
        ("nul", "`nul` is a device name on Windows"),
        ("com1", "`com1` is a device name on Windows"),
        (
            &"a".repeat(65),
            "is 65 characters long; keep it to 64 or fewer",
        ),
    ] {
        fails(&nabi(&home, &root, &["new", name]), why);
        // On Windows `nul` and `com1` exist as devices.
        assert!(cfg!(windows) || !root.join(name).exists());
    }
    fails(
        &nabi(&home, &root, &["add", "aux", "--path", "x"]),
        "`aux` is a device name on Windows",
    );
    write(
        &root.join("named-pkg/nabi.nabion"),
        "Package { name: \"pkg\", version: \"0.1.0\", deps: {} }\n",
    );
    write(&root.join("named-pkg/src/main.nabi"), "println(\"hi\");\n");
    fails(
        &nabi(&home, &root.join("named-pkg"), &["build"]),
        "`pkg` already names a package's own root",
    );
}

/// An existing manifest with a name some OS can't store or a version
/// that is not semver still builds; each problem is a warning with its
/// place. `nabi init` in such a directory picks a name that works.
#[test]
fn existing_manifest_bad_version_warns() {
    let root = scratch("bad-version");
    let home = root.join("home");
    let app = root.join("app");
    write(&app.join("src/main.nabi"), "println(\"hi\");\n");
    write(
        &app.join("nabi.nabion"),
        "Package {\n    name: \"con\",\n    version: \"banana\",\n    deps: {},\n}\n",
    );
    let out = nabi(&home, &app, &["check"]);
    ok(&out);
    let err = text(&out.stderr);
    for want in [
        "nabi.nabion:2:11: `con` is a device name on Windows",
        "nabi.nabion:3:14: `banana` is not a version (major.minor.patch): it needs three numbers; \
         `nabi publish` needs one, e.g. `version: \"0.1.0\"`",
    ] {
        assert!(err.contains(want), "stderr lacks {want:?}:\n{err}");
    }
    assert_eq!(err.matches("`banana`").count(), 1, "{err}");

    // No directory can be called `nul` on Windows.
    if !cfg!(windows) {
        let dir = root.join("nul");
        fs::create_dir_all(&dir).unwrap();
        let out = nabi(&home, &dir, &["init"]);
        ok(&out);
        assert!(
            read(&dir.join("nabi.nabion")).contains("name: \"pkg-nul\""),
            "{}",
            text(&out.stderr)
        );
    }
}

/// `nabi new` / `init` end with what to run next; `nabi add` names the
/// version it added.
#[test]
fn new_and_add_say_what_happened() {
    let root = scratch("new-next");
    let home = root.join("home");
    let err = |cwd: &Path, args: &[&str]| {
        let out = nabi(&home, cwd, args);
        ok(&out);
        text(&out.stderr)
    };
    // Windows PowerShell 5.1 has no `&&`: one command per line there.
    let next = |dir: &str, then: &str| {
        if cfg!(windows) {
            format!("             cd {dir}\n             {then}\n")
        } else {
            format!("             cd {dir} && {then}\n")
        }
    };
    assert_eq!(
        err(&root, &["new", "app"]),
        format!(
            "     Created executable package (app)\n{}",
            next("app", "nabi run")
        )
    );
    assert!(err(&root, &["new", "--lib", "geo"]).ends_with(&next("geo", "nabi test")));
    fs::create_dir_all(root.join("here")).unwrap();
    assert!(err(&root.join("here"), &["init"]).ends_with("\n             nabi run\n"));
    assert_eq!(err(&root, &["-q", "new", "quiet"]), "");

    let app = root.join("app");
    write(&root.join("u/src/lib.nabi"), LIB_V1);
    write(
        &root.join("u/nabi.nabion"),
        "Package { name: \"u\", version: \"1.2.3\" }\n",
    );
    assert_eq!(
        err(&app, &["add", "u", "--path", "../u"]),
        "       Added u v1.2.3\n"
    );
    assert_eq!(
        err(&app, &["add", "geo", "--path", "../geo", "--dev"]),
        "       Added geo v0.1.0 (dev_deps)\n"
    );
    // No `version`: the name alone.
    write(&root.join("v/src/lib.nabi"), LIB_V1);
    assert_eq!(
        err(&app, &["add", "v", "--path", "../v"]),
        "       Added v\n"
    );
}
