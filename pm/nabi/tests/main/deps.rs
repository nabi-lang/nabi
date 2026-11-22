//! Dependencies: adding, locking, renaming, prebuilt reuse.

#[allow(clippy::wildcard_imports)]
use crate::*;

pub(crate) const LIB_V1: &str = "pub fn greet() -> i32 { return 1; }\n";

pub(crate) const LIB_V2: &str = "pub fn greet() -> i32 { return 2; }\n";

/// A manifest with comments, which `add` / `remove` keep.
pub(crate) const APP0: &str = "// my app\nPackage {\n    name: \"app\", // keep\n    deps: {\n        // deps go here\n    },\n}\n";

/// `package` renames a dependency for code, one name per dependent; a
/// dependency with another `target` stays out of the build; two
/// packages may not link one native library.
#[test]
fn renamed_target_and_links_dependencies() {
    let root = scratch("rename-target");
    let home = root.join("home");
    let lib = |name: &str, links: &str| {
        let dir = root.join(name);
        write(&dir.join("src/lib.nabi"), LIB_V1);
        write(
            &dir.join("nabi.nabion"),
            &format!("Package {{ name: \"{name}\", {links}deps: {{}} }}\n"),
        );
    };
    lib("greet", "");
    lib("other", "links: \"z\", ");
    lib("third", "links: \"z\", ");
    let app = root.join("app");
    write(&app.join("src/main.nabi"), "println(1);\n");
    let deps_of = |d: &str| {
        write(
            &app.join("nabi.nabion"),
            &format!("Package {{ name: \"app\", deps: {{ {d} }} }}\n"),
        );
        nabi(&home, &app, &["deps"])
    };
    let out = deps_of(
        "\"g\": Dep::path(\"../greet\", package: \"greet\"), \"other\": Dep::path(\"../other\", \
         target: \"cfg(target_os = \\\"none\\\")\")",
    );
    let names: Vec<String> = ok(&out)
        .lines()
        .map(|l| l.split('=').next().unwrap().to_owned())
        .collect();
    assert_eq!(names, ["g"]);
    write(
        &app.join("src/main.nabi"),
        "use g::greet;\nprintln(f\"{greet()}\");\n",
    );
    ok(&nabi(&home, &app, &["check"]));
    fails(
        &deps_of(
            "\"g\": Dep::path(\"../greet\", package: \"greet\"), \"greet\": Dep::path(\"../greet\")",
        ),
        "the root package depends on package `greet` twice, as `g` and `greet`; depend on it \
         once",
    );
    fails(
        &deps_of("\"other\": Dep::path(\"../other\"), \"third\": Dep::path(\"../third\")"),
        "packages `other` and `third` both link the native library `z`",
    );
}

/// A library's `#[literal]` handlers work in its own source when it is
/// built as a dependency; its `pub` ones also apply in the packages
/// that depend on it, its others do not.
#[test]
fn literal_handlers_across_packages() {
    let root = scratch("literal-dep");
    let home = root.join("home");
    let units = root.join("units");
    write(
        &units.join("src/lib.nabi"),
        "pub struct Meters { pub v: f64; }\n\
         #[literal(number \"m\")]\n\
         pub fn meters(n: f64) -> Meters { Meters { v: n } }\n\
         #[literal(number \"km\")]\n\
         pub(pkg) fn km(n: f64) -> Meters { Meters { v: n * 1000.0 } }\n\
         pub fn marathon() -> Meters {\n let k = 42.0km;\n let m = 195.0m;\n Meters { v: k.v + m.v }\n}\n",
    );
    write(&units.join("nabi.nabion"), "Package { name: \"units\" }\n");
    let app = root.join("app");
    write(
        &app.join("nabi.nabion"),
        "Package { name: \"app\", deps: { \"units\": Dep::path(\"../units\") } }\n",
    );
    write(
        &app.join("src/main.nabi"),
        "let d = 2.5m;\nprintln(f\"{units::marathon().v} {d.v}\");\n",
    );
    assert_eq!(ok(&nabi(&home, &app, &["run", "-q"])), "42195 2.5\n");
    write(
        &app.join("src/main.nabi"),
        "let d = 1.0km;\nprintln(d.v);\n",
    );
    fails(
        &nabi(&home, &app, &["build"]),
        "unknown numeric literal suffix `km`",
    );
}

/// A library's `pub` operator traits bind their tokens in its tests and
/// in the packages that depend on it, `<~` and binary `@` included, and
/// `nabi fmt` lexes them there; a private trait's `<-` stays its own.
#[test]
fn operator_tokens_across_packages() {
    let root = scratch("operator-dep");
    let home = root.join("home");
    let ops = root.join("ops");
    write(
        &ops.join("src/lib.nabi"),
        "pub trait Conv<Rhs = Self> {\n    type Output;\n    #[infix(\"⊛\", like = \"*\")]\n    \
         fn conv(self, rhs: Rhs) -> Output;\n}\n\n\
         pub trait Pipe<F> {\n    type Output;\n    #[infix(\"<~\", like = \"|\")]\n    \
         fn pipe(self, f: F) -> Output;\n}\n\n\
         pub trait MatMul<Rhs = Self> {\n    type Output;\n    #[infix(\"@\", like = \"*\")]\n    \
         fn matmul(self, rhs: Rhs) -> Output;\n}\n\n\
         trait Back {\n    #[infix(\"<-\", like = \"+\")]\n    fn back(self, v: i64) -> i64;\n}\n\n\
         impl Conv for i64 {\n    type Output = i64;\n    \
         fn conv(self, rhs: i64) -> i64 { self * rhs + 1 }\n}\n\n\
         impl Pipe<fn(i64) -> i64> for i64 {\n    type Output = i64;\n    \
         fn pipe(self, f: fn(i64) -> i64) -> i64 { f(self) }\n}\n\n\
         impl MatMul for i64 {\n    type Output = i64;\n    \
         fn matmul(self, rhs: i64) -> i64 { self * rhs + 2 }\n}\n\n\
         impl Back for i64 {\n    fn back(self, v: i64) -> i64 { self * 10 + v }\n}\n\n\
         pub fn own() -> i64 { 1 <- 2 }\n",
    );
    write(&ops.join("nabi.nabion"), "Package { name: \"ops\" }\n");
    write(
        &ops.join("tests/suite.nabi"),
        "#[test]\nfn ops() {\n    assert_eq(3 ⊛ 2, 7);\n    assert_eq(3 @ 2, 8);\n}\n",
    );
    let app = root.join("app");
    write(
        &app.join("nabi.nabion"),
        "Package { name: \"app\", deps: { \"ops\": Dep::path(\"../ops\") } }\n",
    );
    write(
        &app.join("src/main.nabi"),
        "fn inc(x: i64) -> i64 { x + 1 }\n\n\
         fn main() {\n    let e = 1;\n    let lt = e<-1;\n    \
         println(f\"{3 ⊛ 2} {3 @ 2} {3 <~ inc} {lt} {ops::own()}\");\n}\n",
    );
    assert_eq!(ok(&nabi(&home, &app, &["run", "-q"])), "7 8 4 false 12\n");
    ok(&nabi(&home, &ops, &["test", "-q"]));
    ok(&nabi(&home, &ops, &["fmt", "--check"]));
    ok(&nabi(&home, &app, &["fmt", "--check"]));
    // A token nothing registers: the error names the file.
    write(&app.join("src/extra.nabi"), "pub fn f() -> i64 { 1 ⋈ 2 }\n");
    fails(
        &nabi(&home, &app, &["fmt", "--check"]),
        "extra.nabi:1:23: `⋈` (U+22C8 BOWTIE) is not an operator here",
    );
}

/// Dependents may call one package by different names: each name is
/// one package, built once, so its types are the same under both. A
/// name may not stand for two packages.
#[test]
fn per_dependent_renames() {
    let root = scratch("rename-per-dependent");
    let home = root.join("home");
    let greet = root.join("greet");
    write(
        &greet.join("src/lib.nabi"),
        "pub struct Hi { pub n: i32 }\npub fn hi() -> Hi { return Hi { n: 1 }; }\n",
    );
    write(&greet.join("nabi.nabion"), "Package { name: \"greet\" }\n");
    let mid = root.join("mid");
    write(
        &mid.join("src/lib.nabi"),
        "pub fn twice(h: hello::Hi) -> hello::Hi { return hello::Hi { n: h.n * 2 }; }\n",
    );
    write(
        &mid.join("nabi.nabion"),
        "Package { name: \"mid\", deps: { \"hello\": Dep::path(\"../greet\", package: \
         \"greet\") } }\n",
    );
    let other = root.join("other");
    write(&other.join("src/lib.nabi"), LIB_V1);
    write(&other.join("nabi.nabion"), "Package { name: \"other\" }\n");
    let app = root.join("app");
    write(
        &app.join("src/main.nabi"),
        "let h: g::Hi = mid::twice(g::hi());\nprintln(h.n);\n",
    );
    write(
        &app.join("nabi.nabion"),
        "Package { name: \"app\", deps: { \"g\": Dep::path(\"../greet\", package: \
         \"greet\"), \"mid\": Dep::path(\"../mid\") } }\n",
    );
    let names: Vec<String> = deps(&home, &app, &[]).into_iter().map(|d| d.0).collect();
    assert_eq!(names, ["g", "hello", "mid"]);
    let out = ok(&nabi(&home, &app, &["run"]));
    assert_eq!(out.trim(), "2");
    // `hello` is `greet` for `mid`, so it can't also be `other`.
    write(
        &app.join("nabi.nabion"),
        "Package { name: \"app\", deps: { \"hello\": Dep::path(\"../other\", package: \
         \"other\"), \"mid\": Dep::path(\"../mid\") } }\n",
    );
    fails(
        &nabi(&home, &app, &["deps"]),
        "`hello` is package `other` for the root package and package `greet` for `mid`",
    );
}

/// A package sees only its own dependencies, under the names it gives
/// them: another package's `csv` does not take the place of its own
/// `mod csv`, while its renamed `csvio` is that `csv`. A dependency named
/// like one of its modules is ambiguous; a package of the build it does
/// not depend on is not there.
#[test]
fn package_sees_only_its_dependencies() {
    let root = scratch("dep-scope");
    let home = root.join("home");
    let lib = |name: &str, deps: &str, files: &[(&str, &str)]| {
        let dir = root.join(name);
        write(
            &dir.join("nabi.nabion"),
            &format!("Package {{ name: \"{name}\", deps: {{ {deps} }} }}\n"),
        );
        for (f, text) in files {
            write(&dir.join(f), text);
        }
    };
    let app = |deps: &str, main: &str| {
        let dir = root.join("app");
        write(
            &dir.join("nabi.nabion"),
            &format!("Package {{ name: \"app\", deps: {{ {deps} }} }}\n"),
        );
        write(&dir.join("src/main.nabi"), main);
        nabi(&home, &dir, &["-q", "run"])
    };
    lib(
        "csv",
        "",
        &[("src/lib.nabi", "pub fn read() -> i32 { return 1; }\n")],
    );
    // `table`: `csv` renamed `csvio`, and a module `csv` of its own.
    lib(
        "table",
        "\"csvio\": Dep::path(\"../csv\", package: \"csv\")",
        &[
            (
                "src/lib.nabi",
                "pub mod csv;\npub use csv::parse;\n\
                 pub fn both() -> i32 { return csv::parse() + csvio::read(); }\n",
            ),
            ("src/csv.nabi", "pub fn parse() -> i32 { return 10; }\n"),
        ],
    );
    let both = "\"csv\": Dep::path(\"../csv\"), \"table\": Dep::path(\"../table\")";
    let out = app(
        both,
        "println(f\"{table::both()} {table::parse()} {csv::read()}\");\n",
    );
    assert_eq!(ok(&out), "11 10 1\n");
    // In either order.
    let out = app(
        "\"table\": Dep::path(\"../table\"), \"csv\": Dep::path(\"../csv\")",
        "println(table::both());\n",
    );
    assert_eq!(ok(&out), "11\n");
    // `amb` depends on `csv` as `csv` and has a module `csv`.
    lib(
        "amb",
        "\"csv\": Dep::path(\"../csv\")",
        &[
            (
                "src/lib.nabi",
                "pub mod csv;\npub fn both() -> i32 { return csv::parse() + csv::read(); }\n",
            ),
            ("src/csv.nabi", "pub fn parse() -> i32 { return 10; }\n"),
        ],
    );
    let amb = "\"amb\": Dep::path(\"../amb\")";
    let out = app(amb, "println(amb::both());\n");
    fails(
        &out,
        "`csv` is ambiguous: it names the dependency `csv` and the module `pkg::csv`",
    );
    fails(
        &out,
        "write `::csv::..` for the dependency or `pkg::csv::..` for the module, or give the \
         dependency another name in nabi.nabion",
    );
    write(
        &root.join("amb/src/lib.nabi"),
        "pub mod csv;\npub fn both() -> i32 { return pkg::csv::parse() + ::csv::read(); }\n",
    );
    assert_eq!(ok(&app(amb, "println(amb::both());\n")), "11\n");
    // `plain` uses `csv` without depending on it.
    lib(
        "plain",
        "",
        &[(
            "src/lib.nabi",
            "use csv::read;\npub fn one() -> i32 { return read(); }\n",
        )],
    );
    let out = app(
        "\"csv\": Dep::path(\"../csv\"), \"plain\": Dep::path(\"../plain\")",
        "println(plain::one());\n",
    );
    fails(
        &out,
        "`csv` is not a dependency of package `plain`: add it to the `deps` of that package's \
         nabi.nabion",
    );
    // `ren` has `csv` as `csvio` only.
    lib(
        "ren",
        "\"csvio\": Dep::path(\"../csv\", package: \"csv\")",
        &[(
            "src/lib.nabi",
            "use csv::read;\npub fn one() -> i32 { return read(); }\n",
        )],
    );
    let out = app(
        "\"csv\": Dep::path(\"../csv\"), \"ren\": Dep::path(\"../ren\")",
        "println(ren::one());\n",
    );
    fails(
        &out,
        "this package calls the package `csv` `csvio` (its `deps` key): write `csvio::..`",
    );
}

/// `any` across scoped packages: each prebuilt package's seeds name its
/// own items from `pkg::` and a dependency's by the name it gives it
/// (`any al::Pre` too), and a package built after one it does not depend
/// on neither fails on that one's seeds nor carries them.
#[test]
fn any_seeds_across_scoped_packages() {
    let root = scratch("dep-scope-seeds");
    let home = root.join("home");
    let lib = |name: &str, deps: &str, files: &[(&str, &str)]| {
        let dir = root.join(name);
        write(
            &dir.join("nabi.nabion"),
            &format!("Package {{ name: \"{name}\", deps: {{ {deps} }} }}\n"),
        );
        for (f, text) in files {
            write(&dir.join(f), text);
        }
    };
    // An enum of a child module stored in `any`, a trait-object dispatch.
    lib(
        "alpha",
        "",
        &[
            (
                "src/lib.nabi",
                "pub mod kind;\n\
                 pub trait Pre { fn apply(self, x: i32) -> i32; }\n\
                 pub struct Twice {}\n\
                 impl Pre for Twice { fn apply(self, x: i32) -> i32 { x * 2 } }\n\
                 pub fn run(p: any Pre, x: i32) -> i32 { return p.apply(x); }\n",
            ),
            (
                "src/kind.nabi",
                "pub enum Order { Xyz, Zyx }\n\
                 pub fn boxed() -> any { let o: any = Order::Zyx; return o; }\n",
            ),
        ],
    );
    // Built after `alpha`, with no dependencies.
    lib(
        "zone",
        "",
        &[(
            "src/lib.nabi",
            "pub enum Side { Left, Right }\n\
             pub fn boxed() -> any { let s: any = Side::Right; return s; }\n",
        )],
    );
    // `alpha` renamed `al`.
    lib(
        "wrap",
        "\"al\": Dep::path(\"../alpha\", package: \"alpha\")",
        &[(
            "src/lib.nabi",
            "pub struct Half {}\n\
             impl al::Pre for Half { fn apply(self, x: i32) -> i32 { x / 2 } }\n\
             pub fn half() -> any al::Pre { let p: any al::Pre = Half {}; return p; }\n\
             pub fn mine(p: any al::Pre) -> i32 { return p.apply(10); }\n\
             pub fn order() -> any { let o: any = al::kind::Order::Xyz; return o; }\n",
        )],
    );
    let app = root.join("app");
    write(
        &app.join("nabi.nabion"),
        "Package { name: \"app\", deps: { \"alpha\": Dep::path(\"../alpha\"), \"zone\": \
         Dep::path(\"../zone\"), \"wrap\": Dep::path(\"../wrap\") } }\n",
    );
    write(
        &app.join("src/main.nabi"),
        "println(f\"{alpha::kind::boxed() is alpha::kind::Order} {zone::boxed() is \
         zone::Side} {wrap::order() is alpha::kind::Order}\");\n\
         let t: any alpha::Pre = alpha::Twice {};\n\
         println(f\"{alpha::run(t, 4)} {alpha::run(wrap::half(), 4)} {wrap::mine(t)}\");\n",
    );
    let out = nabi(&home, &app, &["-q", "run"]);
    assert_eq!(ok(&out), "true true true\n8 2 20\n");
    let nabihir = |name: &str| {
        let bytes = fs::read(app.join(format!("target/debug/deps/{name}.nabihir"))).unwrap();
        String::from_utf8_lossy(&bytes).into_owned()
    };
    let zone = nabihir("zone");
    assert!(
        zone.contains("x: pkg::Side") && !zone.contains("Pre"),
        "{zone}"
    );
    let wrap = nabihir("wrap");
    assert!(wrap.contains("x: any ::al::Pre"), "{wrap}");
}

#[test]
fn add_lock_update_offline_remove() {
    let root = scratch("lock");
    let home = root.join("home");
    let repo = root.join("greet");
    let c1 = commit(&repo, &[("src/lib.nabi", LIB_V1)]);
    let url = repo.display().to_string();
    let qurl = q(&url);
    let app = root.join("app");
    write(
        &app.join("src/main.nabi"),
        "use greet::greet;\nprintln(f\"{greet()}\");\n",
    );
    write(&app.join("nabi.nabion"), APP0);

    ok(&nabi(&home, &app, &["add", "greet", "--git", &url]));
    let manifest = read(&app.join("nabi.nabion"));
    assert_eq!(
        manifest,
        APP0.replace(
            "    },",
            &format!("        \"greet\": Dep::git({qurl}),\n    }},")
        )
    );
    let lock = read(&app.join("nabi.lock"));
    let tree = git(&repo, &["rev-parse", "HEAD^{tree}"]);
    assert!(
        lock.starts_with(
            "// nabi.lock: generated by `nabi`; do not edit. Commit it with your sources.\n\
             Lock {\n    version: 1,\n    packages: [\n        Locked {\n            \
             name: \"greet\",\n"
        ),
        "{lock}"
    );
    for want in [
        qurl.clone(),
        format!("commit: \"{c1}\",\n            tree: \"{tree}\",\n        }},\n    ],\n}}\n"),
    ] {
        assert!(lock.contains(&want), "{lock}");
    }
    let checkout = home.join("cache/git/checkouts").join(&c1);
    let slashes = |s: String| s.replace('\\', "/");
    assert_eq!(
        slashes(deps(&home, &app, &[])[0].1.clone()),
        slashes(checkout.join("src/lib.nabi").display().to_string())
    );

    // A new commit upstream: builds keep the locked one; `update` moves.
    let c2 = commit(&repo, &[("src/lib.nabi", LIB_V2)]);
    assert!(deps(&home, &app, &["--locked"])[0].1.contains(&c1));
    let out = nabi(&home, &app, &["update", "greet"]);
    ok(&out);
    assert!(
        text(&out.stderr).contains(&format!("Updated greet {} -> {}\n", &c1[..7], &c2[..7])),
        "{}",
        text(&out.stderr)
    );
    assert!(read(&app.join("nabi.lock")).contains(&c2));
    // Nothing new upstream: it says so.
    let out = nabi(&home, &app, &["update"]);
    ok(&out);
    assert!(
        text(&out.stderr).ends_with("note: all dependencies are up to date\n"),
        "{}",
        text(&out.stderr)
    );
    assert!(deps(&home, &app, &[])[0].1.contains(&c2));
    fails(&nabi(&home, &app, &["update", "nope"]), "not a dependency");

    // A lock format other than 1 is an error.
    let v1 = read(&app.join("nabi.lock"));
    write(
        &app.join("nabi.lock"),
        &v1.replace("version: 1,", "version: 0,"),
    );
    fails(
        &nabi(&home, &app, &["--locked", "deps"]),
        "unsupported lockfile version",
    );
    write(&app.join("nabi.lock"), &v1);

    // Offline: a modified or missing checkout comes back from the cached
    // clone; nothing is fetched.
    let c2_dir = home.join("cache/git/checkouts").join(&c2);
    write(&c2_dir.join("src/lib.nabi"), "tampered");
    let out = nabi(&home, &app, &["--offline", "deps"]);
    ok(&out);
    assert!(
        text(&out.stderr).contains("was modified"),
        "{}",
        text(&out.stderr)
    );
    assert_eq!(read(&c2_dir.join("src/lib.nabi")), LIB_V2);
    fs::remove_dir_all(&c2_dir).unwrap();
    ok(&nabi(&home, &app, &["--offline", "deps"]));
    // `update --offline` re-resolves against the cached clone.
    ok(&nabi(&home, &app, &["--offline", "update"]));
    // A fresh NABI_HOME has no cache: offline fails, online fetches the
    // locked commit.
    let home2 = root.join("home2");
    fails(&nabi(&home2, &app, &["--offline", "deps"]), "--offline");
    assert!(deps(&home2, &app, &[])[0].1.contains(&c2));

    // --locked refuses to change the lock.
    let other = "\n        \"other\": Dep::git(\"/nope\"),";
    write(
        &app.join("nabi.nabion"),
        &read(&app.join("nabi.nabion")).replace("deps: {", &format!("deps: {{{other}")),
    );
    fails(&nabi(&home, &app, &["--locked", "deps"]), "--locked");
    write(
        &app.join("nabi.nabion"),
        &read(&app.join("nabi.nabion")).replace(other, ""),
    );

    // A tag; `remove` edits the file and prunes the lock.
    git(&repo, &["tag", "v1", &c1]);
    ok(&nabi(
        &home,
        &app,
        &["add", "greet", "--git", &url, "--tag", "v1"],
    ));
    assert!(
        read(&app.join("nabi.nabion"))
            .contains(&format!("\"greet\": Dep::git({qurl}, tag: \"v1\"),"))
    );
    assert!(read(&app.join("nabi.lock")).contains("tag: \"v1\""));
    assert!(read(&app.join("nabi.lock")).contains(&format!("commit: \"{c1}\"")));
    ok(&nabi(&home, &app, &["remove", "greet"]));
    assert_eq!(read(&app.join("nabi.nabion")), APP0);
    assert!(!read(&app.join("nabi.lock")).contains("Locked"));
    fails(&nabi(&home, &app, &["remove", "greet"]), "not a dependency");

    // A bad source leaves nabi.nabion as it was.
    let before = read(&app.join("nabi.nabion"));
    fails(
        &nabi(
            &home,
            &app,
            &[
                "add",
                "x",
                "--git",
                &root.join("missing").display().to_string(),
            ],
        ),
        "cannot fetch",
    );
    assert_eq!(read(&app.join("nabi.nabion")), before);
    fails(
        &nabi(&home, &app, &["add", "My.X", "--path", "."]),
        "try `my-x`",
    );
}

#[test]
fn transitive_dependencies_and_conflicts() {
    let root = scratch("transitive");
    let home = root.join("home");
    let b = root.join("b");
    commit(&b, &[("src/lib.nabi", "pub fn b() -> i32 { return 2; }\n")]);
    let a = root.join("a");
    commit(
        &a,
        &[
            ("src/lib.nabi", "pub fn a() -> i32 { return 1; }\n"),
            (
                "nabi.nabion",
                &format!(
                    "Package {{ name: \"a\", deps: {{ \"b\": Dep::git({}) }} }}\n",
                    q(b.display())
                ),
            ),
        ],
    );
    let app = root.join("app");
    write(&app.join("src/main.nabi"), "println(\"x\");\n");
    ok(&nabi(
        &home,
        &app,
        &["add", "a", "--git", &a.display().to_string()],
    ));
    let names: Vec<String> = deps(&home, &app, &[]).into_iter().map(|(n, _)| n).collect();
    // Each package after its own dependencies.
    assert_eq!(names, ["b", "a"]);
    let lock = read(&app.join("nabi.lock"));
    assert!(
        lock.contains("name: \"a\"") && lock.contains("name: \"b\""),
        "{lock}"
    );

    // `b` from a path as well: two packages named `b`.
    let other_b = root.join("other_b");
    write(
        &other_b.join("src/lib.nabi"),
        "pub fn b() -> i32 { return 3; }\n",
    );
    let before = read(&app.join("nabi.nabion"));
    fails(
        &nabi(&home, &app, &["add", "b", "--path", "../other_b"]),
        "conflicting requirements for package `b`",
    );
    assert_eq!(read(&app.join("nabi.nabion")), before);

    // Path dependencies are written relative to the project; a
    // non-library is refused.
    write(&root.join("tool/src/main.nabi"), "println(\"t\");\n");
    fails(
        &nabi(&home, &app, &["add", "tool", "--path", "../tool"]),
        "is an executable package (src/main.nabi, no src/lib.nabi): a dependency must be a library",
    );
    ok(&nabi(
        &home,
        &app,
        &["add", "c", "--path", "../other_b", "--dev"],
    ));
    // On one line or one entry per line, by the other entries' widths
    // (the scratch path's).
    let manifest = read(&app.join("nabi.nabion")).replace(['\n', ' ', ','], "");
    assert!(
        manifest.contains("dev_deps:{\"c\":Dep::path(\"../other_b\")}"),
        "{manifest}"
    );
    // Dev dependencies are for tests only.
    assert_eq!(deps(&home, &app, &[]).len(), 2);
    assert_eq!(
        ok(&nabi(&home, &app, &["deps", "--dev"])).lines().count(),
        3
    );
}

/// `nabi deps --format json`: a versioned record per package in build
/// order, then a summary, also with no dependencies.
#[test]
fn deps_format_json() {
    let root = scratch("deps-json");
    let home = root.join("home");
    let app = root.join("app");
    write(&app.join("src/main.nabi"), "println(\"x\");\n");
    write(&app.join("nabi.nabion"), "Package { name: \"app\" }\n");
    let out = ok(&nabi(&home, &app, &["deps"]));
    assert_eq!(out, "");
    let out = ok(&nabi(&home, &app, &["deps", "--format", "json"]));
    assert_eq!(
        out,
        "{\"schema\":1,\"type\":\"summary\",\"packages\":0,\"natives\":0}\n"
    );

    write(
        &root.join("a/src/lib.nabi"),
        "pub fn a() -> i32 { return 1; }\n",
    );
    write(
        &root.join("a/nabi.nabion"),
        "Package { name: \"a\", version: \"0.2.0\", deps: { \"b\": Dep::path(\"../b\") } }\n",
    );
    write(
        &root.join("b/src/lib.nabi"),
        "pub fn b() -> i32 { return 2; }\n",
    );
    write(
        &app.join("nabi.nabion"),
        "Package { name: \"app\", deps: { \"a\": Dep::path(\"../a\") } }\n",
    );
    let out = ok(&nabi(&home, &app, &["deps", "--format=json"]));
    let recs: Vec<Json> = out.lines().map(|l| Json::parse(l).unwrap()).collect();
    assert_eq!(recs.len(), 3, "{out}");
    assert!(
        recs.iter().all(|r| r.get("schema").as_u32() == Some(1)),
        "{out}"
    );
    // `b` first: a package comes after its dependencies.
    assert_eq!(recs[0].get("name").as_str(), Some("b"), "{out}");
    assert_eq!(recs[0].get("version"), &Json::Null, "{out}");
    assert_eq!(recs[1].get("name").as_str(), Some("a"), "{out}");
    assert_eq!(recs[1].get("type").as_str(), Some("package"), "{out}");
    assert_eq!(recs[1].get("version").as_str(), Some("0.2.0"), "{out}");
    let a = nabic_core::real_path(root.join("a")).unwrap();
    assert_eq!(
        recs[1].get("source").as_str(),
        Some(format!("path+{}", a.display()).as_str()),
        "{out}"
    );
    let path = recs[1].get("path").as_str().unwrap();
    assert!(path.ends_with("lib.nabi"), "{out}");
    assert_eq!(recs[2].get("type").as_str(), Some("summary"), "{out}");
    assert_eq!(recs[2].get("packages").as_u32(), Some(2), "{out}");
    // A path is still found after the format's value.
    let out = ok(&nabi(&home, &root, &["deps", "--format", "json", "app"]));
    assert_eq!(out.lines().count(), 3, "{out}");
    let out = nabi(&home, &app, &["deps", "--format", "toml"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        text(&out.stderr).contains("`--format` takes human or json, not `toml`"),
        "{}",
        text(&out.stderr)
    );
}

#[test]
fn nabi_add_writes_canonical_nabion() {
    let root = scratch("canon");
    let home = root.join("home");
    write(&root.join("u/src/lib.nabi"), LIB_V1);
    ok(&nabi(&home, &root, &["new", "app"]));
    let app = root.join("app");
    ok(&nabi(&home, &app, &["add", "u", "--path", "../u"]));
    // Past 100 columns: a field per line.
    assert_eq!(
        read(&app.join("nabi.nabion")),
        format!(
            "Package {{\n    name: \"app\",\n    version: \"0.1.0\",\n    nabi_version: \"{}\",\n    \
             toolchain: \"{}\",\n    deps: {{ \"u\": Dep::path(\"../u\") }},\n}}\n",
            nabi_version(),
            series()
        )
    );
    ok(&nabi(
        &home,
        &app,
        &["add", "a_rather_long_dependency_name", "--path", "../u"],
    ));
    assert_eq!(
        read(&app.join("nabi.nabion")),
        format!(
            "Package {{\n    name: \"app\",\n    version: \"0.1.0\",\n    nabi_version: \"{}\",\n    \
             toolchain: \"{}\",\n    deps: {{ \"u\": Dep::path(\"../u\"), \
             \"a_rather_long_dependency_name\": Dep::path(\"../u\") }},\n}}\n",
            nabi_version(),
            series()
        )
    );
    // A file with comments keeps its layout.
    let mine = "// mine\nPackage { name: \"app\", deps: { \"u\": Dep::path(\"../u\") } }\n";
    write(&app.join("nabi.nabion"), mine);
    ok(&nabi(&home, &app, &["remove", "u"]));
    assert!(read(&app.join("nabi.nabion")).starts_with("// mine\nPackage { name: \"app\""));
    // The manifest's name is the package's.
    write(
        &app.join("nabi.nabion"),
        "Package { name: \"hello\", deps: {} }\n",
    );
    let out = nabi(&home, &app, &["build"]);
    ok(&out);
    assert!(app.join("target/debug").join(exe("hello")).is_file());
    write(&app.join("nabi.nabion"), "Package { name: \"My_App\" }\n");
    fails(&nabi(&home, &app, &["check"]), "try `my_app`");

    // `my-lib` is a package name; code says `my_lib`. `-` and `_` are one
    // name, so both spellings cannot be dependencies.
    ok(&nabi(&home, &root, &["new", "--lib", "my-lib"]));
    assert!(read(&root.join("my-lib/nabi.nabion")).contains("name: \"my-lib\""));
    write(&app.join("nabi.nabion"), "Package { name: \"my-app\" }\n");
    write(
        &app.join("src/main.nabi"),
        "use my_lib::add;\nprintln(f\"{add(1, 2)}\");\n",
    );
    ok(&nabi(
        &home,
        &app,
        &["add", "my-lib", "--path", "../my-lib"],
    ));
    ok(&nabi(&home, &app, &["-q", "check"]));
    assert!(ok(&nabi(&home, &app, &["deps"])).starts_with("my_lib="));
    fails(
        &nabi(&home, &app, &["add", "my_lib", "--path", "../my-lib"]),
        "are the same package",
    );
    assert_eq!(ok(&nabi(&home, &app, &["-q", "run"])), "3\n");
    assert!(app.join("target/debug").join(exe("my-app")).is_file());
}

#[test]
fn lint_and_doc_with_dependencies() {
    let root = scratch("lintdoc");
    let home = root.join("home");
    write(
        &root.join("util/src/lib.nabi"),
        "/// Says hi.\npub fn hello() -> i32 { let unused = 1; 1 }\n",
    );
    let app = root.join("app");
    write(
        &app.join("nabi.nabion"),
        "Package {\n    name: \"app\",\n    deps: { \"util\": Dep::path(\"../util\") },\n    \
         lints: { \"unused_variables\": \"deny\" },\n}\n",
    );
    write(
        &app.join("src/main.nabi"),
        "use util::hello;\nlet x = hello();\nprintln(\"hi\");\n",
    );
    // `lints` sets the level, the command line overrides it, and the
    // dependency is not linted.
    let out = nabi(&home, &app, &["lint"]);
    fails(&out, "unused variable `x`");
    assert!(
        !text(&out.stderr).contains("`unused`")
            && text(&out.stderr)
                .contains("`\"unused_variables\": \"deny\"` in `lints` of nabi.nabion"),
        "{}",
        text(&out.stderr)
    );
    let out = nabi(&home, &app, &["lint", "-W", "unused_variables"]);
    ok(&out);
    assert!(text(&out.stderr).contains("warning[E0131]"));
    fails(
        &nabi(&home, &app, &["lint", "-D", "all"]),
        "unused variable `x`",
    );

    ok(&nabi(&home, &app, &["-q", "doc"]));
    assert!(read(&app.join("target/doc/util/index.html")).contains("Says hi."));
    assert!(read(&app.join("target/doc/app/index.html")).contains("../util/index.html"));
    assert!(!app.join("target/doc/index.html").exists());
}

#[test]
fn build_run_and_repl_with_dependencies() {
    let root = scratch("e2e");
    let home = root.join("home");
    let repo = root.join("greet");
    commit(&repo, &[("src/lib.nabi", LIB_V2)]);
    let app = root.join("app");
    ok(&nabi(&home, &root, &["new", "app"]));
    write(
        &app.join("src/main.nabi"),
        "use greet::greet;\nprintln(f\"{greet()}\");\n",
    );
    ok(&nabi(
        &home,
        &app,
        &["add", "greet", "--git", &repo.display().to_string()],
    ));
    assert_eq!(ok(&nabi(&home, &app, &["-q", "run"])), "2\n");
    assert!(app.join("target/debug").join(exe("app")).is_file());
    // `repl <path>`: the session has the program's imports and dependencies
    // (on a host the REPL runs on: not FreeBSD or a static musl nabic).
    let repl = nabic_interface::repl_host();
    if repl {
        let mut child = nabi_cmd(&home, &root, &["-q", "repl", "app"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b":type greet()\ngreet() + 1\n")
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert_eq!(ok(&out), "2\ni32\n3\n", "{}", text(&out.stderr));
    }
    ok(&nabi(&home, &app, &["-q", "build", "--release"]));
    assert!(app.join("target/release").join(exe("app")).is_file());
    ok(&nabi(&home, &app, &["-q", "clean"]));
    assert!(!app.join("target").exists());
    if !repl {
        return;
    }

    // `:add` in the REPL goes to the active environment; bare `nabi` is `nabi repl`.
    write(&root.join("lib1/src/lib.nabi"), LIB_V1);
    ok(&nabi(&home, &root, &["env", "create", "r"]));
    let mut child = nabi_cmd(&home, &root, &["--env", "r"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let input = format!(
        ":add g --path {}\nuse g::greet;\ngreet() + 40\n",
        root.join("lib1").display()
    );
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let stdout = ok(&out);
    assert!(stdout.contains("41\n"), "{stdout}\n{}", text(&out.stderr));
    assert!(read(&home.join("envs/r/nabi.nabion")).contains("\"g\": Dep::path("));
}

/// `nabi build` builds each dependency once into `target/<profile>/deps`
/// (`lib<name>.a` + `<name>.nabihir`), reuses it while nothing it depends
/// on changes, and rebuilds it (and the ones after it) when its source
/// does. A dependency of a dependency is built first.
#[test]
fn prebuilt_dependencies_are_reused_and_rebuilt() {
    let root = scratch("prebuilt");
    let home = root.join("home");
    write(
        &root.join("base/src/lib.nabi"),
        "pub struct Cell { pub v: i32; }\npub fn cell(v: i32) -> Cell { return Cell { v: v }; }\n",
    );
    write(
        &root.join("base/nabi.nabion"),
        "Package { name: \"base\" }\n",
    );
    write(
        &root.join("mid/src/lib.nabi"),
        "pub fn twice<T>(x: T) -> (T, T) { return (x, x); }\n\
         pub fn get() -> i32 { return base::cell(20).v + 1; }\n",
    );
    write(
        &root.join("mid/nabi.nabion"),
        "Package {\n    name: \"mid\",\n    deps: { \"base\": Dep::path(\"../base\") },\n}\n",
    );
    let app = root.join("app");
    ok(&nabi(&home, &root, &["new", "app"]));
    write(
        &app.join("src/main.nabi"),
        "let (a, b) = mid::twice(\"x\");\nprintln(f\"{mid::get()} {a}{b}\");\n",
    );
    write(
        &app.join("nabi.nabion"),
        "Package {\n    name: \"app\",\n    deps: { \"mid\": Dep::path(\"../mid\") },\n}\n",
    );
    let built = |out: &Output, dep: &str| text(&out.stderr).contains(&format!("Compiling {dep} "));
    let out = nabi(&home, &app, &["run"]);
    assert_eq!(ok(&out), "21 xx\n");
    assert!(
        built(&out, "base") && built(&out, "mid"),
        "{}",
        text(&out.stderr)
    );
    let deps = app.join("target/debug/deps");
    for f in ["libbase.a", "base.nabihir", "libmid.a", "mid.nabihir"] {
        assert!(deps.join(f).is_file(), "{f}");
    }
    // Nothing changed: both reused.
    let out = nabi(&home, &app, &["run"]);
    assert_eq!(ok(&out), "21 xx\n");
    assert!(
        !built(&out, "base") && !built(&out, "mid"),
        "{}",
        text(&out.stderr)
    );
    // `base` changed: it and `mid` (built after it) are rebuilt.
    write(
        &root.join("base/src/lib.nabi"),
        "pub struct Cell { pub v: i32; }\npub fn cell(v: i32) -> Cell { return Cell { v: v * 2 }; }\n",
    );
    let out = nabi(&home, &app, &["run"]);
    assert_eq!(ok(&out), "41 xx\n");
    assert!(
        built(&out, "base") && built(&out, "mid"),
        "{}",
        text(&out.stderr)
    );
    // Another profile is another build.
    let out = nabi(&home, &app, &["build", "--release"]);
    ok(&out);
    assert!(built(&out, "mid"), "{}", text(&out.stderr));
    assert!(app.join("target/release/deps/mid.nabihir").is_file());
}

/// A prebuilt dependency's fns with a `some Tr` or an untyped parameter
/// are generic: their bodies stay in its interface, so a dependent
/// instantiates them.
#[test]
fn implicit_generics_of_prebuilt_dependencies() {
    let root = scratch("implicit-generics");
    let home = root.join("home");
    write(
        &root.join("lib/src/lib.nabi"),
        "pub trait Size { fn size(self) -> usize; }\n\
         impl Size for List<f64> { fn size(self) -> usize { self.len() } }\n\
         pub fn g(x: some Size) -> usize { x.size() }\n\
         fn first(x) { x }\n\
         pub fn k<T>(x: T) -> T { first(x) }\n\
         pub struct B { pub n: usize; }\n\
         impl B { pub fn add(self, x: some Size) -> usize { self.n + x.size() } }\n",
    );
    write(&root.join("lib/nabi.nabion"), "Package { name: \"lib\" }\n");
    let app = root.join("app");
    write(
        &app.join("src/main.nabi"),
        "fn main() {\n    println(\"s\");\n    println(lib::g([1.0, 2.0]));\n    \
         println(lib::g([3.0]));\n    println(lib::k(5));\n    \
         println(lib::B { n: 1 }.add([1.0]));\n}\n",
    );
    write(
        &app.join("nabi.nabion"),
        "Package { name: \"app\", deps: { \"lib\": Dep::path(\"../lib\") } }\n",
    );
    assert_eq!(ok(&nabi(&home, &app, &["run"])), "s\n2\n1\n5\n2\n");
}

/// A dependency nabi fetched builds as `<name> v<version>`, not as its
/// directory in the cache; a local one shows its directory.
#[test]
fn fetched_dependency_builds_by_version() {
    let root = scratch("dep-version");
    let home = root.join("home");
    let repo = root.join("greet");
    commit(
        &repo,
        &[
            ("src/lib.nabi", LIB_V2),
            (
                "nabi.nabion",
                "Package { name: \"greet\", version: \"0.3.0\" }\n",
            ),
        ],
    );
    ok(&nabi(&home, &root, &["new", "app"]));
    let app = root.join("app");
    write(&app.join("local/src/lib.nabi"), LIB_V1);
    write(
        &app.join("src/main.nabi"),
        "println(f\"{greet::greet()} {local::greet()}\");\n",
    );
    ok(&nabi(
        &home,
        &app,
        &["add", "greet", "--git", &repo.display().to_string()],
    ));
    ok(&nabi(&home, &app, &["add", "local", "--path", "local"]));
    let out = nabi(&home, &app, &["build"]);
    ok(&out);
    let err = text(&out.stderr).replace('\\', "/");
    assert!(err.contains("   Compiling greet v0.3.0\n"), "{err}");
    assert!(err.contains("   Compiling local (local)\n"), "{err}");
    assert!(!err.contains("checkouts"), "{err}");
}
