//! Dependencies and prebuilt libraries.

#[allow(clippy::wildcard_imports)]
use crate::cli::*;

#[test]
fn extern_dependencies_are_top_level_names() {
    let env = setup();
    let dir = scratch("extern");
    // `greet` (a library with a submodule and a `pkg::` import) and
    // `app`, which uses it from its root and from a submodule.
    fs::create_dir_all(dir.join("greet")).unwrap();
    put(
        dir.join("greet/src/lib.nabi"),
        "pub mod words;\nuse pkg::words::hello;\n\
         pub fn greet(n: i32) -> i32 { return hello() + n; }\n",
    )
    .unwrap();
    put(
        dir.join("greet/src/words.nabi"),
        "pub fn hello() -> i32 { return 40; }\n",
    )
    .unwrap();
    fs::create_dir_all(dir.join("app")).unwrap();
    put(
        dir.join("app/src/main.nabi"),
        "mod sub;\nuse greet::greet;\nprintln(f\"{greet(1)} {sub::two()}\");\n",
    )
    .unwrap();
    put(
        dir.join("app/src/sub.nabi"),
        "use greet::words::hello;\npub fn two() -> i32 { return hello() + 2; }\n",
    )
    .unwrap();
    let out = build_run(Some(&env), &dir, &["app", "--extern", "greet=greet"]);
    assert_exit(&out, 0);
    assert_eq!(text(&out.stdout), "41 42\n");
    // Prebuilt into deps/ with its fingerprint.
    for f in ["libgreet.a", "greet.nabihir", "greet.fp"] {
        assert!(dir.join("app/target/debug/deps").join(f).is_file(), "{f}");
    }
    // `--extern=name=path/src/lib.nabi` works too; a missing prebuilt one
    // fails.
    let out = nabic(
        Some(&env),
        &dir,
        &[
            "build",
            "--release",
            "app",
            "--extern=greet=greet/src/lib.nabi",
        ],
    );
    assert_exit(&out, 0);
    assert!(
        dir.join("app/target/release")
            .join(exe_name("app"))
            .is_file()
    );
    assert_exit(
        &nabic(None, &dir, &["build", "app", "--extern", "g=x.a"]),
        1,
    );
    // Without it, `greet` is unknown.
    assert_exit(&nabic(Some(&env), &dir, &["check", "app"]), 1);
    // The standard library's names are taken, as a dependency's own (an
    // alias of one is fine) and as a library's.
    let reserved = "`std` is the standard library, which ships with the toolchain; name the \
                    package something else, e.g. `std_util`";
    let out = nabic(None, &dir, &["check", "app", "--extern", "std=greet"]);
    assert_exit(&out, 2);
    assert!(
        text(&out.stderr).contains(reserved),
        "{}",
        text(&out.stderr)
    );
    let out = nabic(None, &dir, &["build", "greet", "--name", "core"]);
    assert_exit(&out, 1);
    assert!(
        text(&out.stderr).contains("`core` is the standard library"),
        "{}",
        text(&out.stderr)
    );
    // Nor can a package take a name code can't spell as its path.
    let out = nabic(None, &dir, &["check", "app", "--extern", "pkg=greet"]);
    assert_exit(&out, 2);
    assert!(
        text(&out.stderr).contains("`pkg` already names a package's own root (`pkg::`)"),
        "{}",
        text(&out.stderr)
    );
    let out = nabic(None, &dir, &["build", "greet", "--name", "match"]);
    assert_exit(&out, 1);
    assert!(
        text(&out.stderr).contains("`match` is a keyword"),
        "{}",
        text(&out.stderr)
    );
}

#[test]
fn extern_dependency_is_a_package_boundary() {
    let env = setup();
    let dir = scratch("extern-boundary");
    // `pkg::` inside `dep` is `dep`; `dep::..` expression paths work in a
    // submodule; `pub(pkg)` / private items of `dep` stay inside it.
    fs::create_dir_all(dir.join("dep")).unwrap();
    put(
        dir.join("dep/src/lib.nabi"),
        "pub mod words;\nfn secret() -> i32 { return 1; }\n\
         pub(pkg) fn internal() -> i32 { return 2; }\n\
         pub fn via_pkg() -> i32 { return pkg::words::deep() + secret(); }\n",
    )
    .unwrap();
    put(
        dir.join("dep/src/words.nabi"),
        "pub fn hello() -> i32 { return 40; }\n\
         pub fn deep() -> i32 { return pkg::internal() * 10; }\n",
    )
    .unwrap();
    fs::create_dir_all(dir.join("app")).unwrap();
    put(
        dir.join("app/src/main.nabi"),
        "mod sub;\nfn internal() -> i32 { return 7; }\n\
         println(f\"{sub::two()} {dep::via_pkg()} {pkg::internal()}\");\n",
    )
    .unwrap();
    put(
        dir.join("app/src/sub.nabi"),
        "pub fn two() -> i32 { return dep::words::hello() + 2; }\n",
    )
    .unwrap();
    let out = build_run(Some(&env), &dir, &["app", "--extern", "dep=dep"]);
    assert_exit(&out, 0);
    assert_eq!(text(&out.stdout), "42 21 7\n");

    fs::create_dir_all(dir.join("bad")).unwrap();
    put(
        dir.join("bad/src/main.nabi"),
        "println(f\"{dep::secret()} {dep::internal()}\");\n",
    )
    .unwrap();
    let out = nabic(None, &dir, &["check", "bad", "--extern", "dep=dep"]);
    assert_exit(&out, 1);
    let err = text(&out.stderr);
    assert!(err.contains("`dep::secret` is private"), "{err}");
    assert!(err.contains("`dep::internal` is `pub(pkg)`"), "{err}");

    // A user module may not take a dependency's name.
    fs::create_dir_all(dir.join("clash")).unwrap();
    put(
        dir.join("clash/src/main.nabi"),
        "mod dep;\nprintln(f\"{dep::own()}\");\n",
    )
    .unwrap();
    put(
        dir.join("clash/src/dep.nabi"),
        "pub fn own() -> i32 { return 3; }\n",
    )
    .unwrap();
    let out = nabic(None, &dir, &["check", "clash", "--extern", "dep=dep"]);
    assert_exit(&out, 1);
    assert!(
        text(&out.stderr).contains("module `dep` has the name of the dependency `dep`"),
        "{}",
        text(&out.stderr)
    );
}

/// A prebuilt library: code from the archive, generics instantiated by the
/// user, `any` across the boundary, one lazy static, defaults resolved in
/// the declaring package.
#[test]
fn defaults_resolve_in_the_declaring_package() {
    let env = setup();
    let dir = scratch("extern-defaults");
    fs::create_dir_all(dir.join("dep")).unwrap();
    put(
        dir.join("dep/src/lib.nabi"),
        "fn one() -> i32 { 1 }\n\
         const LIMIT: i32 = 9;\n\
         pub struct S {\n    pub n: i32 = 5;\n}\n\
         pub struct Cfg {\n    pub f: fn() -> i32 = one;\n    pub k: i32 = LIMIT;\n}\n\
         pub fn use_s(s: S = S {}) -> i32 { s.n }\n\
         pub fn lim(x: i32 = LIMIT) -> i32 { x }\n\
         pub fn via_one<T>(t: T, x: i32 = one()) -> i32 { x }\n",
    )
    .unwrap();
    fs::create_dir_all(dir.join("app")).unwrap();
    put(
        dir.join("app/src/main.nabi"),
        "fn one() -> i32 { 100 }\nconst LIMIT: i32 = 200;\nstruct S {}\n\
         let c = dep::Cfg {};\n\
         println(f\"{(c.f)()} {c.k} {dep::use_s()} {dep::lim()} {dep::via_one(0)}\");\n",
    )
    .unwrap();
    let out = build_run(Some(&env), &dir, &["app", "--extern", "dep=dep"]);
    assert_exit(&out, 0);
    assert_eq!(text(&out.stdout), "1 9 5 9 1\n");
}

/// A sum in a prebuilt dependency's signature has the same tags in the
/// app: member order does not follow the build's type numbering (the app
/// instantiates `Dict<str, i32>` before `List<i32>`, the library after).
#[test]
fn prebuilt_dependency_sums_keep_their_tags() {
    let env = setup();
    let dir = scratch("prebuilt-sums");
    fs::create_dir_all(dir.join("dep")).unwrap();
    put(
        dir.join("dep/src/lib.nabi"),
        "pub struct Zebra { pub n: i32; }\n\
         pub fn pick(b: bool) -> List<i32> | Dict<str, i32> {\n\
             if b { let xs: List<i32> = [1, 2, 3]; return xs; }\n\
             let d: Dict<str, i32> = Dict::new();\n\
             d[\"k\"] = 7;\n\
             return d;\n\
         }\n\
         pub fn size(x: List<i32> | Dict<str, i32> | Zebra) -> str {\n\
             match x {\n\
                 xs: List<i32> => f\"list {xs.len()}\",\n\
                 d: Dict<str, i32> => f\"dict {d.len()}\",\n\
                 z: Zebra => f\"zebra {z.n}\",\n\
             }\n\
         }\n",
    )
    .unwrap();
    fs::create_dir_all(dir.join("app")).unwrap();
    put(
        dir.join("app/src/main.nabi"),
        "struct Mine { a: i32; }\n\
         let early: Dict<str, i32> = Dict::new();\n\
         let m = Mine { a: early.len() as i32 };\n\
         println(dep::size(dep::pick(true)));\n\
         println(dep::size(dep::pick(false)));\n\
         println(dep::size(dep::Zebra { n: 4 + m.a }));\n\
         let own: Dict<str, i32> = Dict::new();\n\
         println(dep::size(own));\n",
    )
    .unwrap();
    let out = nabic(Some(&env), &dir, &["build", "dep"]);
    assert_exit(&out, 0);
    let nabihir = dir.join("dep/target/debug/dep.nabihir");
    let ext = format!("dep={}", nabihir.display());
    let out = build_run(Some(&env), &dir, &["-q", "app", "--extern", &ext]);
    assert_exit(&out, 0);
    assert_eq!(text(&out.stdout), "list 3\ndict 1\nzebra 4\ndict 0\n");
}

/// A prebuilt library's `any` seeds name types by full path: its root
/// imports neither the errors an untyped `throws` carries nor the std
/// `List` (it declares its own), yet the dependent builds.
#[test]
fn prebuilt_any_seeds_name_types_by_path() {
    let env = setup();
    let dir = scratch("prebuilt-seed-paths");
    fs::create_dir_all(dir.join("dep/src")).unwrap();
    put(
        dir.join("dep/src/lib.nabi"),
        "pub mod a;\npub struct List { pub k: i32; }\n",
    )
    .unwrap();
    put(
        dir.join("dep/src/a.nabi"),
        "use std::fs;\n\
         pub fn size(path: str) throws -> usize {\n\
             let b = try fs::read(path);\n\
             if b.len() < 4 { throw ValueError::new(\"short\"); }\n\
             b.len()\n\
         }\n\
         pub fn boxed() -> any {\n\
             let xs: List<i32> = [1, 2];\n\
             let out: any = xs;\n\
             out\n\
         }\n",
    )
    .unwrap();
    fs::create_dir_all(dir.join("app")).unwrap();
    put(
        dir.join("app/src/main.nabi"),
        "fn main() throws {\n\
             println(f\"{try dep::a::size(\"app/src/main.nabi\")}\");\n\
             println(f\"{dep::a::boxed() is List<i32>} {dep::List { k: 3 }.k}\");\n\
         }\n",
    )
    .unwrap();
    let out = nabic(Some(&env), &dir, &["build", "dep"]);
    assert_exit(&out, 0);
    let nabihir = dir.join("dep/target/debug/dep.nabihir");
    let ext = format!("dep={}", nabihir.display());
    let out = build_run(Some(&env), &dir, &["-q", "app", "--extern", &ext]);
    assert_exit(&out, 0);
    let len = fs::read(dir.join("app/src/main.nabi")).unwrap().len();
    assert_eq!(text(&out.stdout), format!("{len}\ntrue 3\n"));
}

/// Hand-off summaries survive in a prebuilt dependency's NABIHIR.
#[test]
fn prebuilt_dependency_keeps_handoff_summaries() {
    let env = setup();
    let dir = scratch("prebuilt-handoff");
    fs::create_dir_all(dir.join("dep")).unwrap();
    put(
        dir.join("dep/src/lib.nabi"),
        "pub struct Bag { pub items: List<i32>; }\n\
         fn lock(xs: List<i32>) -> Mutex<List<i32>> {\n\
             return Mutex::new(xs);\n\
         }\n\
         pub fn store(xs: List<i32>) -> Mutex<List<i32>> { return lock(xs); }\n\
         pub fn count(xs: List<i32>) -> usize { return xs.len(); }\n\
         pub fn bag(xs: List<i32>) -> Bag { return Bag { items: xs }; }\n\
         impl Bag {\n\
             pub fn share(self) -> Mutex<List<i32>> { return Mutex::new(self.items); }\n\
         }\n\
         pub fn store_if(xs: List<i32>, ok: bool) -> Mutex<List<i32>> | none {\n\
             if !ok { return none; }\n\
             return lock(xs);\n\
         }\n",
    )
    .unwrap();
    fs::create_dir_all(dir.join("app")).unwrap();
    let app = |body: &str| {
        put(
            dir.join("app/src/main.nabi"),
            format!("let xs: List<i32> = [1];\n{body}\nprintln(f\"{{xs.len()}}\");\n"),
        )
        .unwrap();
    };
    let out = nabic(Some(&env), &dir, &["build", "dep"]);
    assert_exit(&out, 0);
    let nabihir = dir.join("dep/target/debug/dep.nabihir");
    let ext = format!("dep={}", nabihir.display());
    let dump = inspect_dump(&nabihir);
    assert!(dump.contains("--- summaries\ndep::lock\n"), "{dump}");
    // By exit: the `Mutex` (variant 0) took `xs`, `none` (1) nothing.
    assert!(dump.contains("dep::store_if\n"), "{dump}");
    assert!(dump.contains("    x 0 0\n    x 1\n"), "{dump}");
    assert!(
        !dump.contains("pub fn store(xs: List<i32>) -> Mutex<List<i32>> { return"),
        "{dump}"
    );

    app("let n = dep::count(xs);\nlet _m = dep::store(xs.clone());\nxs.push(n as i32);");
    let out = build_run(Some(&env), &dir, &["-q", "app", "--extern", &ext]);
    assert_exit(&out, 0);
    assert_eq!(text(&out.stdout), "2\n");

    app("let _m = dep::store(xs);\nxs.push(2);");
    let out = nabic(Some(&env), &dir, &["build", "app", "--extern", &ext]);
    assert_exit(&out, 1);
    let err = text(&out.stderr);
    assert!(
        err.contains(
            "error[E0105]: `xs` was moved into a `Mutex` by `dep::store(xs)` at line 2 (inside \
             `lock`, lib.nabi:3)"
        ),
        "{err}"
    );

    app("if dep::store_if(xs, false) is none { xs.push(2); } else { xs = [7]; }");
    let out = build_run(Some(&env), &dir, &["-q", "app", "--extern", &ext]);
    assert_exit(&out, 0);
    assert_eq!(text(&out.stdout), "2\n");

    app("if let _m: Mutex<List<i32>> = dep::store_if(xs, true) { xs.push(2); }");
    let out = nabic(Some(&env), &dir, &["build", "app", "--extern", &ext]);
    assert_exit(&out, 1);
    let err = text(&out.stderr);
    assert!(
        err.contains("error[E0105]: `xs` was moved into a `Mutex` by `dep::store_if(xs, true)`"),
        "{err}"
    );

    app("let b = dep::bag(xs);\nlet _m = b.share();\nxs.push(2);");
    let out = nabic(Some(&env), &dir, &["build", "app", "--extern", &ext]);
    assert_exit(&out, 1);
    let err = text(&out.stderr);
    assert!(
        err.contains(
            "error[E0105]: `xs` shares what `b.share()` moved into a `Mutex` at line 3 (inside \
             `Bag::share`, lib.nabi:9)"
        ),
        "{err}"
    );
}

/// Aggregates over 64 bytes cross a prebuilt dependency by address,
/// both ways and through function values, at `-O0` and `-O2`.
#[test]
fn prebuilt_dependency_passes_large_aggregates_by_address() {
    let env = setup();
    let dir = scratch("prebuilt-big");
    fs::create_dir_all(dir.join("dep")).unwrap();
    put(
        dir.join("dep/src/lib.nabi"),
        "pub struct Big { pub a: [i64; 12]; pub tag: u8; }\n\
         pub fn make(k: i64) -> Big {\n\
             let b = Big { a: [0; 12], tag: 7 };\n\
             for i in 0..12 { b.a[i] = k * i as i64; }\n\
             return b;\n\
         }\n\
         pub fn total(b: Big) -> i64 { let t = 0i64; for x in b.a { t += x; } return t + b.tag as i64; }\n\
         pub fn twice(f: fn(Big) -> Big, b: Big) -> Big { return f(f(b)); }\n\
         pub fn summer() -> fn(Big) -> i64 { return total; }\n",
    )
    .unwrap();
    fs::create_dir_all(dir.join("app")).unwrap();
    put(
        dir.join("app/src/main.nabi"),
        "fn inc(b: dep::Big) -> dep::Big { let c = b; c.a[11] += 1; c.tag += 1; return c; }\n\
         let b = dep::make(2);\n\
         let c = dep::twice(inc, b);\n\
         let s = dep::summer();\n\
         println(f\"{dep::total(b)} {c.a[11]} {c.tag} {s(c)} {b.a[11]}\");\n",
    )
    .unwrap();
    for (dep_opt, app_opt) in [("-O2", "-O0"), ("-O0", "-O2")] {
        let out = nabic(Some(&env), &dir, &["build", dep_opt, "dep"]);
        assert_exit(&out, 0);
        let profile = if dep_opt == "-O2" { "release" } else { "debug" };
        let nabihir = dir.join(format!("dep/target/{profile}/dep.nabihir"));
        let ext = format!("dep={}", nabihir.display());
        let out = build_run(Some(&env), &dir, &["-q", app_opt, "app", "--extern", &ext]);
        assert_exit(&out, 0);
        assert_eq!(text(&out.stdout), "139 24 9 143 22\n", "dep {dep_opt}");
    }
}

/// A prebuilt dependency's call summaries (`--- calls`): an optimized
/// dependent takes them on, and a rebuilt dependency whose function now
/// writes recompiles the units calling it.
#[test]
fn prebuilt_call_summaries_reach_dependents() {
    let env = setup();
    let dir = scratch("prebuilt-calls");
    fs::create_dir_all(dir.join("dep")).unwrap();
    let lib = |touch: &str| {
        put(
            dir.join("dep/src/lib.nabi"),
            format!(
                "pub fn touch(xs: List<i64>) {{ {touch} }}\n\
                 pub fn bump(xs: List<i64>) {{ xs[0] = xs[0] + 1; }}\n\
                 pub fn step(p: usize) -> usize {{ p + 8 }}\n\
                 pub fn keep(xs: List<usize>, p: usize) {{ xs.push(p); }}\n\
                 #[cold]\npub fn rare(x: i64) -> i64 {{ x }}\n"
            ),
        )
        .unwrap();
    };
    lib("let _n = xs.len();");
    fs::create_dir_all(dir.join("app")).unwrap();
    put(
        dir.join("app/src/main.nabi"),
        "let xs: List<i64> = [5];\n\
         let a = xs[0];\n\
         dep::touch(xs);\n\
         let b = xs[0];\n\
         dep::bump(xs);\n\
         let ps: List<usize> = [];\n\
         dep::keep(ps, dep::step(1));\n\
         println(f\"{a} {b} {xs[0]} {ps[0]} {dep::rare(3)}\");\n",
    )
    .unwrap();
    let nabihir = dir.join("dep/target/release/dep.nabihir");
    let ext = format!("dep={}", nabihir.display());
    let build = |want: &str| {
        assert_exit(&nabic(Some(&env), &dir, &["build", "-O2", "dep"]), 0);
        let out = nabic(
            Some(&env),
            &dir,
            &["build", "-vv", "-O2", "app", "--extern", &ext],
        );
        assert_exit(&out, 0);
        let units: Vec<String> = text(&out.stderr)
            .lines()
            .filter_map(|l| l.trim().strip_prefix("Compiled unit "))
            .map(str::to_owned)
            .collect();
        let exe = if cfg!(windows) { "app.exe" } else { "app" };
        let run = Command::new(dir.join("app/target/release").join(exe))
            .output()
            .unwrap();
        assert_exit(&run, 0);
        assert_eq!(text(&run.stdout), want);
        (units, inspect_dump(&nabihir))
    };
    let (units, dump) = build("5 5 6 9 3\n");
    assert!(units.iter().any(|u| u == "<root>"), "{units:?}");
    let line = |dump: &str, name: &str| {
        dump.lines()
            .skip_while(|l| *l != "--- calls")
            .find(|l| l.split(' ').next() == Some(name))
            .unwrap_or_default()
            .to_owned()
    };
    assert!(line(&dump, "dep::touch").contains(" readonly"), "{dump}");
    assert!(!line(&dump, "dep::bump").contains(" readonly"), "{dump}");
    assert!(
        line(&dump, "dep::step").ends_with(" keeps-none:0x1 returns:0x1"),
        "{dump}"
    );
    assert!(!line(&dump, "dep::keep").contains("keeps-none"), "{dump}");
    assert!(line(&dump, "dep::rare").contains(" cold"), "{dump}");
    // `touch` writes now: `main`'s unit, unchanged itself, recompiles
    // and loads `xs[0]` again after the call.
    lib("xs[0] = 9;");
    let (units, dump) = build("5 9 10 9 3\n");
    assert!(units.iter().any(|u| u == "<root>"), "{units:?}");
    assert!(!line(&dump, "dep::touch").contains(" readonly"), "{dump}");
}

/// An `extern "C"` declaration of a prebuilt dependency's
/// `#[export_name]` function is that function, at every level.
#[test]
fn extern_declaration_names_a_dependency_export() {
    let env = setup();
    let dir = scratch("prebuilt-export");
    fs::create_dir_all(dir.join("dep")).unwrap();
    put(
        dir.join("dep/src/lib.nabi"),
        "#[export_name(\"dep_sum\")]\npub extern \"C\" fn sum(x: i64, y: i64) -> i64 { x + y }\n",
    )
    .unwrap();
    fs::create_dir_all(dir.join("app")).unwrap();
    put(
        dir.join("app/src/main.nabi"),
        "extern \"C\" { fn dep_sum(x: i64, y: i64) -> i64; }\n\
         println(f\"{dep::sum(1, 2)} {unsafe { dep_sum(3, 4) }}\");\n",
    )
    .unwrap();
    let out = nabic(Some(&env), &dir, &["build", "dep"]);
    assert_exit(&out, 0);
    let ext = format!("dep={}", dir.join("dep/target/debug/dep.nabihir").display());
    for opts in [&["-O0"][..], &["-O2"]] {
        let mut args = vec!["-q"];
        args.extend_from_slice(opts);
        args.extend_from_slice(&["app", "--extern", &ext]);
        let out = build_run(Some(&env), &dir, &args);
        assert_exit(&out, 0);
        assert_eq!(text(&out.stdout), "3 7\n", "{opts:?}");
    }
}

#[test]
fn prebuilt_dependency_is_linked_and_instantiated() {
    let env = setup();
    let dir = scratch("prebuilt");
    fs::create_dir_all(dir.join("geo")).unwrap();
    put(
        dir.join("geo/src/lib.nabi"),
        "pub mod shapes;\n\
         static COUNT: Atomic<i32> = Atomic::new(0);\n\
         pub fn bump() -> i32 { return COUNT.fetch_add(1) + 1; }\n\
         pub struct Bad { pub at: i32; }\n\
         impl Display for Bad { fn fmt(self, ref f: Formatter) throws (FmtError) { try f.write_str(f\"bad {self.at}\"); } }\n\
         impl Debug for Bad { fn fmt(self, ref f: Formatter) throws (FmtError) { try Display::fmt(self, ref f); } }\n\
         impl Error for Bad {}\n\
         pub fn check(x: i32) throws -> i32 { if x < 0 { throw Bad { at: x }; } return x * 2; }\n\
         pub struct Stack<T> { items: List<T>; }\n\
         impl<T> Stack<T> {\n\
             pub fn new() -> Stack<T> { return Stack { items: List::new() }; }\n\
             pub fn push(self, v: T) { self.items.push(v); bump(); }\n\
             pub fn len(self) -> usize { return self.items.len(); }\n\
         }\n\
         pub fn largest(xs: List<any shapes::Shape>) -> i32 {\n\
             let best = 0;\n\
             for s in xs { if s.area() > best { best = s.area(); } }\n\
             return best;\n\
         }\n",
    )
    .unwrap();
    put(
        dir.join("geo/src/shapes.nabi"),
        "pub trait Shape { fn area(self) -> i32; }\n\
         pub struct Sq { pub s: i32; }\n\
         impl Shape for Sq { fn area(self) -> i32 { return self.s * self.s; } }\n\
         pub fn sq(s: i32) -> any Shape { return Sq { s: s }; }\n",
    )
    .unwrap();
    fs::create_dir_all(dir.join("app")).unwrap();
    put(
        dir.join("app/src/main.nabi"),
        "use geo::shapes::{Shape, sq};\n\
         struct Rect { w: i32; h: i32; }\n\
         impl Shape for Rect { fn area(self) -> i32 { return self.w * self.h; } }\n\
         let st: geo::Stack<str> = geo::Stack::new();\n\
         st.push(\"a\");\n\
         println(f\"{st.len()} {geo::bump()}\");\n\
         try geo::check(-3) catch e { println(f\"caught {e}\"); };\n\
         let xs: List<any Shape> = List::new();\n\
         xs.push(Rect { w: 2, h: 9 });\n\
         xs.push(sq(4));\n\
         println(f\"{geo::largest(xs)}\");\n",
    )
    .unwrap();
    let out = nabic(Some(&env), &dir, &["build", "geo"]);
    assert_exit(&out, 0);
    let nabihir = dir.join("geo/target/debug/geo.nabihir");
    assert!(nabihir.is_file() && dir.join("geo/target/debug/libgeo.a").is_file());
    let out = build_run(
        Some(&env),
        &dir,
        &[
            "-q",
            "app",
            "--extern",
            &format!("geo={}", nabihir.display()),
        ],
    );
    assert_exit(&out, 0);
    assert_eq!(text(&out.stdout), "1 2\ncaught bad -3\n18\n");
    // The `.a` names it too; the build used the archive, not the source.
    put(dir.join("geo/src/lib.nabi"), "this is not nabi\n").unwrap();
    let out = build_run(
        Some(&env),
        &dir,
        &["-q", "app", "--extern", "geo=geo/target/debug/libgeo.a"],
    );
    assert_exit(&out, 0);
    assert_eq!(text(&out.stdout), "1 2\ncaught bad -3\n18\n");

    // The interface: concrete bodies cut, generic ones kept.
    let dump = inspect_dump(&dir.join("geo/target/debug/geo.nabihir"));
    let old = nabic(
        None,
        &dir,
        &["nabihir-text", "geo/target/debug/geo.nabihir"],
    );
    assert_exit(&old, 2);
    assert!(
        text(&old.stderr).contains("unknown command `nabihir-text`"),
        "{}",
        text(&old.stderr)
    );
    assert!(dump.contains("package: geo (hash: "), "{dump}");
    assert!(dump.contains("pub fn bump() -> i32 ;"), "{dump}");
    assert!(
        dump.contains("pub fn new() -> Stack<T> { return Stack"),
        "{dump}"
    );
    let info = nabic_interface::nabihir::inspect(&dir.join("geo/target/debug/libgeo.a")).unwrap();
    for want in [
        "Package: geo\n",
        "NABIHIR version: 1\n",
        "Exports:\n",
        "  pub fn check(x: i32) throws -> i32\n",
        "Generic items (NABIHIR):\n  pub struct Stack<T>\n",
        "Concrete .o files:\n  part0.o (",
    ] {
        assert!(info.contains(want), "{want:?} in\n{info}");
    }
    // The producer's version and id, without the tool name again.
    let line = info.lines().find(|l| l.starts_with("nabic version: "));
    assert!(
        line.is_some_and(|l| l[15..].starts_with(|c: char| c.is_ascii_digit())),
        "{info}"
    );
}

/// A prebuilt library's interface carries its `include_str!` /
/// `include_bytes!` data: moved away from its sources and
/// data files, its generic and `const` users still get the bytes.
#[test]
fn moved_prebuilt_library_keeps_its_included_files() {
    let env = setup();
    let dir = scratch("prebuilt-include");
    fs::create_dir_all(dir.join("dep/src/data")).unwrap();
    put(dir.join("dep/src/data/blob.bin"), "\x01\x02z\n").unwrap();
    put(dir.join("dep/src/data/note.txt"), "hi \"there\"\n").unwrap();
    put(
        dir.join("dep/src/lib.nabi"),
        "pub const NOTE: str = include_str!(\"data/note.txt\");\n\
         pub fn blob_len<T>(t: T) -> usize {\n    let b = include_bytes!(\"data/blob.bin\");\n    b.len()\n}\n\
         #[inline]\npub fn first() -> u8 { let b = include_bytes!(\"data/blob.bin\"); b[0] }\n",
    )
    .unwrap();
    fs::create_dir_all(dir.join("app")).unwrap();
    put(
        dir.join("app/src/main.nabi"),
        "print(dep::NOTE);\nprintln(f\"{dep::blob_len(0)} {dep::first()}\");\n",
    )
    .unwrap();
    let out = nabic(Some(&env), &dir, &["build", "dep"]);
    assert_exit(&out, 0);
    fs::rename(dir.join("dep"), dir.join("moved")).unwrap();
    let ext = format!(
        "dep={}",
        dir.join("moved/target/debug/dep.nabihir").display()
    );
    let out = build_run(Some(&env), &dir, &["-q", "app", "--extern", &ext]);
    assert_exit(&out, 0);
    assert_eq!(text(&out.stdout), "hi \"there\"\n4 1\n");
}

/// A NABIHIR from another format version or compiler, or built under
/// another name, is refused with a "rebuild" message.
#[test]
fn stale_nabihir_is_rejected() {
    let env = setup();
    let dir = scratch("stale-nabihir");
    fs::create_dir_all(dir.join("dep")).unwrap();
    put(
        dir.join("dep/src/lib.nabi"),
        "pub fn one() -> i32 { return 1; }\n",
    )
    .unwrap();
    fs::create_dir_all(dir.join("app")).unwrap();
    put(
        dir.join("app/src/main.nabi"),
        "println(f\"{dep::one()}\");\n",
    )
    .unwrap();
    assert_exit(&nabic(Some(&env), &dir, &["build", "dep"]), 0);
    let nabihir = dir.join("dep/target/debug/dep.nabihir");
    let ext = format!("dep={}", nabihir.display());
    let out = build_run(Some(&env), &dir, &["-q", "app", "--extern", &ext]);
    assert_exit(&out, 0);
    assert_eq!(text(&out.stdout), "1\n");

    let other = format!("other={}", nabihir.display());
    let out = nabic(Some(&env), &dir, &["check", "app", "--extern", &other]);
    assert_exit(&out, 1);
    assert!(
        text(&out.stderr).contains("built as `dep`"),
        "{}",
        text(&out.stderr)
    );

    let good = fs::read(&nabihir).unwrap();
    // The layout: header, sized strings (producer, meta), then at the
    // body offset the cut files, hand-off summaries and call summaries.
    let word = |i: usize| u32::from_le_bytes(good[i..i + 4].try_into().unwrap());
    assert_eq!(&good[..4], b"NABH");
    assert_eq!((word(4), word(8)), (1, 0));
    let varint = |at: &mut usize| -> usize {
        let extra = good[*at].leading_ones() as usize;
        let mut v = usize::from(good[*at] & 0xffu8.checked_shr(extra as u32 + 1).unwrap_or(0));
        for b in &good[*at + 1..=*at + extra] {
            v = (v << 8) | usize::from(*b);
        }
        *at += 1 + extra;
        v
    };
    let string = |at: &mut usize| -> String {
        let n = varint(at);
        *at += n;
        String::from_utf8(good[*at - n..*at].to_vec()).unwrap()
    };
    let mut at = 16;
    assert!(string(&mut at).starts_with("nabic "));
    assert_eq!(string(&mut at), "dep");
    let src = "pub fn one() -> i32 { return 1; }\n";
    let digest = nabic_core::hash::sha256(format!("lib.nabi\0{src}\0").as_bytes());
    assert_eq!(string(&mut at), nabic_core::hash::hex(&digest)[..8]);
    let mut at = word(12) as usize;
    assert_eq!(varint(&mut at), 1);
    assert_eq!(string(&mut at), "lib.nabi");
    assert_eq!(string(&mut at), "pub fn one() -> i32 ;\n");
    assert_eq!(varint(&mut at), 0);
    // `one` never reaches a safepoint (quiet, flag 2): nothing else is
    // known at -O0.
    assert_eq!(varint(&mut at), 1);
    assert_eq!(string(&mut at), "dep::one");
    assert_eq!(
        (varint(&mut at), varint(&mut at), varint(&mut at)),
        (2, 0, 0)
    );
    assert_eq!(at, good.len());
    // The archive's functions are global `_Y` symbols.
    if !cfg!(windows) && Command::new("nm").arg("--version").output().is_ok() {
        let nm = Command::new("nm")
            .arg(dir.join("dep/target/debug/libdep.a"))
            .output()
            .unwrap();
        let want = format!("_Y{}_dep_one_", &nabic_core::hash::hex(&digest)[..8]);
        assert!(
            text(&nm.stdout)
                .lines()
                .any(|l| l.contains(" T ") && l.ends_with(&want)),
            "no {want}: {}",
            text(&nm.stdout)
        );
    }
    let mut bad = good.clone();
    bad[4] = 99; // format version
    put(&nabihir, &bad).unwrap();
    let out = nabic(Some(&env), &dir, &["check", "app", "--extern", &ext]);
    assert_exit(&out, 1);
    let err = text(&out.stderr);
    assert!(
        err.contains("NABIHIR format version 99") && err.contains("rebuild"),
        "{err}"
    );

    // Another compiler's: the producer string (after the 16-byte
    // header and its length byte) differs.
    let mut bad = good;
    bad[17 + 5] ^= 1; // inside "nabic 0.."
    put(&nabihir, &bad).unwrap();
    let out = nabic(Some(&env), &dir, &["check", "app", "--extern", &ext]);
    assert_exit(&out, 1);
    let err = text(&out.stderr);
    assert!(
        err.contains("was built by") && err.contains("rebuild"),
        "{err}"
    );
}

/// Library bodies nothing reaches are skipped by default;
/// `--check-library` lowers (so type-checks) every one of them.
#[test]
fn check_library_checks_every_body() {
    let dir = scratch("check-lib");
    put(dir.join("main.nabi"), "fn main() {}\n").unwrap();
    let out = nabic(None, &dir, &["check", "--check-library", "main.nabi"]);
    assert_exit(&out, 0);
    // The library drops no result it does not mean to.
    let err = text(&out.stderr);
    assert!(!err.contains("is never used"), "{err}");
}

/// A build compiles only the library code it reaches: `--check-library`
/// compiles all of it, optimized too, so code no program reaches still
/// builds. All of it loads plugins: only where nabild links them.
#[test]
fn check_library_compiles_every_body() {
    if !nabild::links_plugin(nabic_target::TargetSpec::host()) {
        eprintln!("check_library_compiles_every_body: no plugin host link here, skipped");
        return;
    }
    let env = setup();
    let dir = scratch("check-lib-build");
    put(dir.join("main.nabi"), "fn main() { println(\"ok\"); }\n").unwrap();
    for opt in ["-O0", "-O2"] {
        let out = build_run(Some(&env), &dir, &[opt, "--check-library", "main.nabi"]);
        assert_exit(&out, 0);
        assert_eq!(text(&out.stdout), "ok\n", "{opt}");
    }
}

/// A recursive value in a recursive function gives the hand-off walk
/// thousands of aliases at its exit, more with each `Env` field; a walk
/// that rescans a local's edges per step takes minutes in a debug nabic.
#[test]
fn check_recursive_value_aliases_quickly() {
    let dir = scratch("check-recursive-aliases");
    let (mut decl, mut copy, mut init) = (String::new(), String::new(), String::new());
    for i in 0..4 {
        decl += &format!(" f{i}: List<Dict<str, Value>>;");
        copy += &format!(", f{i}: captured.f{i}");
        init += &format!(", f{i}: [Dict::new()]");
    }
    let pushes = "    for i in 1..items.len() { args.push(eval(items[i], env)); }\n";
    let src = format!(
        "enum Value {{\n    Sym(str),\n    List(List<Value>),\n    Lambda(List<Value>, Env),\n}}\n\
         struct Env {{ frames: List<Dict<str, Value>>;{decl} }}\n\n\
         fn eval(x: Value, env: Env) -> Value {{\n    \
             let items = match x {{\n        Value::List(items) => items,\n        _ => return x,\n    }};\n    \
             match items[0] {{\n        Value::Sym(s) => {{\n            \
                 let v = eval(items[2], env);\n            env.frames[0].insert(s, v);\n            \
                 return v;\n        }},\n        _ => {{}},\n    }}\n    \
             let args: List<Value> = [];\n{pushes}    apply(eval(items[0], env), args)\n}}\n\n\
         fn apply(f: Value, args: List<Value>) -> Value {{\n    \
             let Value::Lambda(body, captured) = f else {{ return f; }};\n    \
             eval(body[0], Env {{ frames: captured.frames{copy} }})\n}}\n\n\
         println(f\"{{eval(Value::Sym(\"a\"), Env {{ frames: [Dict::new()]{init} }}) is Value}}\");\n"
    );
    put(dir.join("interp.nabi"), src).unwrap();
    let out = nabic_within(&dir, &dir, &["check", "interp.nabi"]);
    assert_exit(&out, 0);
}

/// `bare` names neither `core` nor `std`, and
/// `core` does not name `std` (copies of the tiers, with one
/// module each that does).
#[test]
fn library_tiers_depend_only_downward() {
    fn copy(from: &Path, to: &Path) {
        for e in fs::read_dir(from).unwrap() {
            let p = e.unwrap().path();
            let dest = to.join(p.file_name().unwrap());
            if p.is_dir() {
                copy(&p, &dest);
            } else {
                put(&dest, fs::read(&p).unwrap()).unwrap();
            }
        }
    }
    let dir = scratch("tiers");
    let lib = workspace().join("library");
    for tier in ["bare", "core"] {
        copy(&lib.join(tier), &dir.join(tier));
        let root = dir.join(tier).join("src/lib.nabi");
        let text = fs::read_to_string(&root).unwrap();
        put(&root, format!("{text}\npub mod leak;\n")).unwrap();
    }
    put(
        dir.join("bare/src/leak.nabi"),
        "use core::sync::Mutex;\n\npub fn n() -> usize { std::env::args().len() }\n",
    )
    .unwrap();
    put(
        dir.join("core/src/leak.nabi"),
        "pub fn n() -> usize { std::env::args().len() }\n",
    )
    .unwrap();
    put(dir.join("main.nabi"), "fn main() {}\n").unwrap();
    let (core, bare) = (dir.join("core/src/lib.nabi"), dir.join("bare/src/lib.nabi"));
    let std = lib.join("std/src/lib.nabi");
    let out = nabic(
        None,
        &dir,
        &[
            "check",
            "main.nabi",
            "--bootstrap",
            core.to_str().unwrap(),
            "--bootstrap",
            std.to_str().unwrap(),
            "--bootstrap",
            bare.to_str().unwrap(),
        ],
    );
    assert_exit(&out, 1);
    let err = text(&out.stderr);
    for want in [
        "`bare` must not depend on `core`",
        "`bare` must not depend on `std`",
        "`core` must not depend on `std`",
    ] {
        assert!(err.contains(want), "{want}\n{err}");
    }
}

#[test]
fn dependency_trait_is_not_the_current_package() {
    let env = setup();
    let dir = scratch("clash");
    // A dependency's trait is not in scope unless `use`d;
    // `use`d, it beats core's.
    fs::create_dir_all(dir.join("dz")).unwrap();
    put(
        dir.join("dz/src/lib.nabi"),
        "pub trait DepZero { fn zero() -> Self; }\n\
         impl DepZero for i32 { fn zero() -> i32 { return 7; } }\n",
    )
    .unwrap();
    fs::create_dir_all(dir.join("app")).unwrap();
    put(
        dir.join("app/src/main.nabi"),
        "println(f\"{i32::zero()}\");\n",
    )
    .unwrap();
    let out = build_run(Some(&env), &dir, &["app", "--extern", "dz=dz"]);
    assert_exit(&out, 0);
    assert_eq!(text(&out.stdout), "0\n");
    put(
        dir.join("app/src/main.nabi"),
        "use dz::DepZero;\nprintln(f\"{i32::zero()}\");\n",
    )
    .unwrap();
    let out = build_run(Some(&env), &dir, &["app", "--extern", "dz=dz"]);
    assert_exit(&out, 0);
    assert_eq!(text(&out.stdout), "7\n");
}

/// `pkg::` in a meta block names the block's own package
/// wherever it expands, not the use site's.
#[test]
fn meta_block_pkg_names_its_package() {
    let env = setup();
    let dir = scratch("meta_pkg");
    fs::create_dir_all(dir.join("dl")).unwrap();
    put(
        dir.join("dl/src/lib.nabi"),
        "pub trait Named { fn nm(self) -> str; }\n\
         pub fn tag(s: str) -> str { f\"<{s}>\" }\n\
         #[impl_derive(Named)]\n\
         pub @derive {\n    impl pkg::Named for @Self {\n        \
         fn nm(self) -> str { pkg::tag(@Self.as_str()) }\n    }\n}\n\
         #[derive(Named)]\npub struct Inner {}\n",
    )
    .unwrap();
    fs::create_dir_all(dir.join("app")).unwrap();
    put(
        dir.join("app/src/main.nabi"),
        "use dl::Named;\n\
         fn tag(s: str) -> str { \"wrong\" }\n\
         #[derive(Named)]\nstruct Outer {}\n\
         println(Outer {}.nm());\nprintln(dl::Inner {}.nm());\n",
    )
    .unwrap();
    let out = build_run(Some(&env), &dir, &["app", "--extern", "dl=dl"]);
    assert_exit(&out, 0);
    assert_eq!(text(&out.stdout), "<Outer>\n<Inner>\n");
}

/// A user impl overlapping a dependency's bounded blanket
/// impl is an error at its declaration; without the bound's impl for
/// the user type, the two are disjoint.
#[test]
fn dependency_blanket_impl_overlaps_user_impl() {
    let env = setup();
    let dir = scratch("overlap_dep");
    fs::create_dir_all(dir.join("dd")).unwrap();
    put(
        dir.join("dd/src/lib.nabi"),
        "pub trait Shape {}\n\
         pub trait Describe { fn d(self) -> i32; }\n\
         impl<T: Shape> Describe for T { fn d(self) -> i32 { return 1; } }\n",
    )
    .unwrap();
    fs::create_dir_all(dir.join("app")).unwrap();
    let main = "use dd::{Shape, Describe};\n\
                struct Mine<T> { v: T; }\n\
                impl<T> Describe for Mine<T> { fn d(self) -> i32 { return 2; } }\n\
                println(f\"{Mine { v: 1 }.d()}\");\n";
    put(dir.join("app/src/main.nabi"), main).unwrap();
    let out = build_run(Some(&env), &dir, &["app", "--extern", "dd=dd"]);
    assert_exit(&out, 0);
    assert_eq!(text(&out.stdout), "2\n");
    put(
        dir.join("app/src/main.nabi"),
        format!("{main}impl<T> Shape for Mine<T> {{}}\n"),
    )
    .unwrap();
    let out = nabic(Some(&env), &dir, &["check", "app", "--extern", "dd=dd"]);
    assert_exit(&out, 1);
    assert!(
        text(&out.stderr).contains("overlap on `Mine<T>` - bounds don't make impls disjoint"),
        "{}",
        text(&out.stderr)
    );
}

/// `#[non_exhaustive]`: a match on the type from another
/// package needs a `_` arm even when it names every variant; inside its
/// own package it does not.
#[test]
fn non_exhaustive_needs_a_catch_all_across_packages() {
    let dir = scratch("non-exhaustive");
    fs::create_dir_all(dir.join("dep")).unwrap();
    put(
        dir.join("dep/src/lib.nabi"),
        "#[non_exhaustive]\npub enum E { A, B }\n\
         pub fn n(e: E) -> i32 { return match e { E::A => 1, E::B => 2 }; }\n",
    )
    .unwrap();
    fs::create_dir_all(dir.join("app")).unwrap();
    let main = |arms: &str| {
        put(
            dir.join("app/src/main.nabi"),
            format!("use dep::E;\nlet v = match E::A {{ {arms} }};\nprintln(f\"{{v}}\");\n"),
        )
        .unwrap();
    };
    main("E::A => 1, E::B => 2");
    let out = nabic(None, &dir, &["check", "app", "--extern", "dep=dep"]);
    assert_exit(&out, 1);
    assert!(
        text(&out.stderr).contains("needs a `_` arm"),
        "{}",
        text(&out.stderr)
    );
    main("E::A => 1, E::B => 2, _ => 3");
    assert_exit(
        &nabic(None, &dir, &["check", "app", "--extern", "dep=dep"]),
        0,
    );
}

/// A prebuilt dependency's NABIHIR carries the `.wit` files
/// its `ffi::wit` items read (one outside its source dir, with its
/// `deps/`): a dependent builds, links and runs with them gone.
#[test]
fn wit_prebuilt_dependency_carries_its_wit() {
    let _env = setup();
    let rt = workspace().join("target/lib/nabi/targets/wasm32-wasip2/libnabir.a");
    if !rt.is_file() {
        eprintln!("skipped: no wasm32-wasip2 runtime (targets/build.sh wasm32-wasip2)");
        return;
    }
    let dir = scratch("wit-prebuilt");
    put(
        dir.join("calc/wit/calc.wit"),
        "package test:calc;\n\nworld calc {\n    use test:num/types.{n};\n    \
         export add: func(a: n, b: n) -> n;\n}\n",
    )
    .unwrap();
    put(
        dir.join("calc/wit/deps/num.wit"),
        "package test:num;\n\ninterface types {\n    type n = s32;\n}\n",
    )
    .unwrap();
    put(
        dir.join("calc/src/lib.nabi"),
        "ffi::wit::export { \"../wit/calc.wit\" }\n\n\
         #[wit::export]\npub fn add(a: i32, b: i32) -> i32 { a + b }\n",
    )
    .unwrap();
    put(
        dir.join("app/main.nabi"),
        "fn main() { println(f\"{calc::add(2, 3)}\"); }\n",
    )
    .unwrap();
    let out = nabic(
        None,
        &dir,
        &["-q", "build", "calc", "--target", "wasm32-wasip2"],
    );
    assert_exit(&out, 0);
    fs::remove_dir_all(dir.join("calc/src")).unwrap();
    fs::remove_dir_all(dir.join("calc/wit")).unwrap();
    let out = nabic(
        None,
        &dir,
        &[
            "-q",
            "build",
            "app/main.nabi",
            "--target",
            "wasm32-wasip2",
            "--extern",
            "calc=calc/target/wasm32-wasip2/debug/calc.nabihir",
            "-o",
            "app.wasm",
        ],
    );
    assert_exit(&out, 0);
    if std::env::var_os("NABI_TEST_WASM_RUNNER").is_none_or(|r| r.is_empty()) {
        return;
    }
    // A component: wasmtime or targets/wasi/jco.sh (iwasm.sh and
    // wasmer.sh run core modules).
    let (runner, wasmtime) = wasm_runner("the wit prebuilt dependency test");
    if !wasmtime && Path::new(&runner).file_stem().is_none_or(|n| n != "jco") {
        return;
    }
    let mut main = Command::new(runner);
    if wasmtime {
        main.arg("run");
    }
    main.arg("app.wasm");
    for (mut c, want) in [
        (main, "5\n"),
        (invoke_command(Path::new("app.wasm"), "add(4, 5)"), "9\n"),
    ] {
        let run = c.current_dir(&dir).output().unwrap();
        assert_exit(&run, 0);
        assert_eq!(text(&run.stdout), want);
    }
}
