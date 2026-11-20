//! Packages: layout, library and binary roots, macros.

#[allow(clippy::wildcard_imports)]
use crate::cli::*;

#[test]
fn declarative_macros_run_end_to_end() {
    let env = setup();
    let dir = scratch("macros");
    let app = new_package(&dir, "app", false);
    // An item macro (repetition + @concat name synthesis) and an expression
    // macro (with a hygienic local), both used from the same program.
    put(
        app.join("src/main.nabi"),
        "#[impl_macro(sq)] pub @macro { (@e: expr) => { { let v = @e; v * v } } }\n\
         #[impl_macro(mk)] pub @macro { (@rep[sep = \",\"] { @n: ident }) => { \
         @for @n { fn @concat(f_, @n)() -> i64 { 10i64 } } } }\n\
         mk!(a, b);\n\
         fn main() {\n    let v = 1i64;\n    println(f\"{sq!(f_a() + f_b())} {v}\");\n}\n",
    )
    .unwrap();
    let out = build_run(Some(&env), &app, &["-q"]);
    assert_exit(&out, 0);
    // (10 + 10)^2; `sq!`'s own `v` leaves the caller's alone.
    assert_eq!(text(&out.stdout), "400 1\n");
}

// `WrappingFrom` (the `as` matrix) is macro-generated in convert.nabi.
#[test]
fn macro_generated_wrapping_casts_work() {
    let env = setup();
    let dir = scratch("wrap");
    let app = new_package(&dir, "app", false);
    put(
        app.join("src/main.nabi"),
        "fn main() {\n\
         \x20   let a = (300i32).wrapping_into::<u8>();\n\
         \x20   let b = (200i32).wrapping_into::<i8>();\n\
         \x20   println(f\"{a} {b}\");\n\
         }\n",
    )
    .unwrap();
    let out = build_run(Some(&env), &app, &["-q"]);
    assert_exit(&out, 0);
    // 300 as u8 = 44;  200 as i8 = -56
    assert_eq!(text(&out.stdout), "44 -56\n");
}

// The integer operator impls (Add, Sub, .. Ord) are macro-generated in
// num.nabi.
#[test]
fn macro_generated_integer_ops_work() {
    let env = setup();
    let dir = scratch("intops");
    let app = new_package(&dir, "app", false);
    put(
        app.join("src/main.nabi"),
        "fn main() {\n\
         \x20   let a = 7i32 + 3i32; let b = 7i32 - 3i32; let c = 7i32 * 3i32;\n\
         \x20   let d = 7i32 / 3i32; let e = 7i32 % 3i32;\n\
         \x20   let f = 6i32 & 3i32; let g = 6i32 | 1i32; let h = 6i32 ^ 3i32;\n\
         \x20   let i = 1i32 << 3i32; let j = 16i32 >> 2i32; let k = -5i32;\n\
         \x20   let eq = if 3i32 == 3i32 { 1i32 } else { 0i32 };\n\
         \x20   let lt = if 2i32 < 3i32 { 1i32 } else { 0i32 };\n\
         \x20   let u = 200u8 + 1u8;\n\
         \x20   println(f\"{a} {b} {c} {d} {e} {f} {g} {h} {i} {j} {k} {eq} {lt} {u}\");\n\
         }\n",
    )
    .unwrap();
    let out = build_run(Some(&env), &app, &["-q"]);
    assert_exit(&out, 0);
    assert_eq!(text(&out.stdout), "10 4 21 2 1 2 7 5 8 4 -5 1 1 201\n");
}

// The integer `*Assign` impls are macro-generated.
#[test]
fn macro_generated_compound_assigns_work() {
    let env = setup();
    let dir = scratch("assigns");
    let app = new_package(&dir, "app", false);
    put(
        app.join("src/main.nabi"),
        "fn main() {\n    let a = 10i64;\n    a += 5i64;\n    a *= 2i64;\n    a -= 1i64;\n\
         \x20   let b = 12u8;\n    b <<= 1u8;\n    b &= 254u8;\n    println(f\"{a} {b}\");\n}\n",
    )
    .unwrap();
    let out = build_run(Some(&env), &app, &["-q"]);
    assert_exit(&out, 0);
    // a: ((10+5)*2)-1 = 29;  b: (12<<1) & 254 = 24
    assert_eq!(text(&out.stdout), "29 24\n");
}

#[test]
fn new_bin_package_runs_from_its_directory() {
    let env = setup();
    let dir = scratch("bin");
    let app = new_package(&dir, "app", false);
    let out = build_run(Some(&env), &app, &[]);
    assert_exit(&out, 0);
    assert_eq!(text(&out.stdout), "Hello, world!\n");
    assert!(text(&out.stderr).contains("Compiling app"));

    // A file inside the package builds the package; `-q` silences progress.
    put(app.join("src/util/x.nabi"), "fn unused() {}\n").unwrap();
    let out = nabic(Some(&env), &dir, &["-q", "build", "app/src/util/x.nabi"]);
    assert_exit(&out, 0);
    assert!(out.stderr.is_empty(), "{}", text(&out.stderr));
    assert!(app.join("target/debug").join(exe_name("app")).is_file());
}

/// The package is `src/`; `.nabi` files beside it are not part of
/// it (not compiled, not build inputs), and one of them builds as a
/// lone file of its own.
#[test]
fn files_outside_src_are_not_the_package() {
    let env = setup();
    let dir = scratch("stray");
    let app = new_package(&dir, "app", false);
    put(app.join("old.nabi"), "fn (\n").unwrap();
    put(app.join("scratch.nabi"), "println(\"scratch\");\n").unwrap();
    put(app.join("tests/t.nabi"), "fn t() {\n  1;\n}\n").unwrap();
    let out = build_run(Some(&env), &app, &[]);
    assert_exit(&out, 0);
    assert_eq!(text(&out.stdout), "Hello, world!\n");
    put(app.join("old.nabi"), "fn ((\n").unwrap();
    assert!(text(&build_run(Some(&env), &app, &[]).stderr).contains("Fresh app"));
    let out = build_run(Some(&env), &app, &["scratch.nabi"]);
    assert_exit(&out, 0);
    assert_eq!(text(&out.stdout), "scratch\n");
}

/// A package with `src/lib.nabi` and `src/main.nabi` is both, the executable
/// using the library by the package's name.
#[test]
fn lib_and_main_package() {
    let env = setup();
    let dir = scratch("libmain");
    let app = new_package(&dir, "app", true);
    put(
        app.join("src/main.nabi"),
        "use app::add;\n\nprintln(f\"{add(2, 3)}\");\n\n#[test]\nfn main_adds() {\n    if \
         add(1, 1) != 2 {\n        panic(\"main\");\n    }\n}\n",
    )
    .unwrap();

    let out = nabic(Some(&env), &app, &["build"]);
    assert_exit(&out, 0);
    let debug = app.join("target/debug");
    for f in [exe_name("app"), "libapp.a".into(), "app.nabihir".into()] {
        assert!(debug.join(&f).is_file(), "{f}");
    }
    // Each target keeps its own front-end and codegen-unit state.
    let incr: Vec<String> = fs::read_dir(debug.join("incremental"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(incr.len(), 4, "{incr:?}");
    for unit in ["app", "libapp"] {
        assert!(incr.iter().any(|n| n == unit), "{incr:?}");
    }
    let out = nabic(Some(&env), &app, &["build"]);
    assert_eq!(text(&out.stderr).matches("Fresh app").count(), 2);

    let out = build_run(Some(&env), &app, &["-q"]);
    assert_exit(&out, 0);
    assert_eq!(text(&out.stdout), "5\n");

    // The library's tests run before the executable's (manifest order).
    let (code, stdout, _) = harness(&env, &app, "test", &[]);
    assert_eq!(code, 0, "{stdout}");
    let lib_at = stdout.find("test adds ... ok").expect(&stdout);
    let main_at = stdout.find("test main_adds ... ok").expect(&stdout);
    assert!(lib_at < main_at, "{stdout}");
    // A failing library test stops before the executable's.
    let lib = fs::read_to_string(app.join("src/lib.nabi")).unwrap();
    put(
        app.join("src/lib.nabi"),
        lib.replace("add(2, 2) != 4", "add(2, 2) != 5"),
    )
    .unwrap();
    let (code, stdout, _) = harness(&env, &app, "test", &[]);
    assert_eq!(code, 1, "{stdout}");
    assert!(stdout.contains("test adds ... FAILED"), "{stdout}");
    assert!(!stdout.contains("main_adds"), "{stdout}");
    put(app.join("src/lib.nabi"), &lib).unwrap();

    // `check`: the library's lints and the executable's, once each.
    put(
        app.join("src/lib.nabi"),
        format!("{lib}\nfn lib_unused() {{}}\n"),
    )
    .unwrap();
    put(
        app.join("src/main.nabi"),
        "use app::add;\n\nfn main_unused() {}\n\nprintln(f\"{add(2, 3)}\");\n",
    )
    .unwrap();
    let out = nabic(Some(&env), &app, &["check"]);
    assert_exit(&out, 0);
    let stderr = text(&out.stderr);
    assert_eq!(
        stderr.matches("`lib_unused` is never used").count(),
        1,
        "{stderr}"
    );
    assert_eq!(
        stderr.matches("`main_unused` is never used").count(),
        1,
        "{stderr}"
    );
    assert!(
        stderr.contains("Checked app: 0 errors, 2 warnings"),
        "{stderr}"
    );
}

/// `--emit=source`: the package after meta expansion, as nabi
/// source (derived impls, `if let` as `match`; no comments).
#[test]
fn emit_ast_shows_derives_and_desugaring() {
    let dir = scratch("ast");
    put(
        dir.join("src/main.nabi"),
        "// gone\n#[derive(Clone)]\nstruct P { x: i32; }\n\
         fn main() {\n    let v: i32 | none = 3;\n    if let n: i32 = v { println(f\"{n}\"); }\n}\n",
    )
    .unwrap();
    let out = nabic(None, &dir, &["build", "--emit=source", "-o", "x.nabi"]);
    assert_exit(&out, 0);
    let text = fs::read_to_string(dir.join("x.nabi")).unwrap();
    assert!(
        text.contains("impl core::marker::Clone for P {")
            && text.contains("return P { x: self.x.clone() };")
            && text.contains("match v {\n        n: i32 => {")
            && !text.contains("gone"),
        "{text}"
    );
}

/// In a library build every function of the package is a root
/// (even a private one nothing calls), library code it does not reach
/// is dropped.
#[test]
fn library_build_drops_code_its_package_does_not_reach() {
    let dir = scratch("libprune");
    let lib = dir.join("plib");
    fs::create_dir_all(&lib).unwrap();
    put(
        lib.join("src/lib.nabi"),
        "pub fn f() -> f64 { 2.0.sqrt() }\nfn unused_private() -> i32 { 2 }\n",
    )
    .unwrap();
    let out = nabic(None, &dir, &["emit", "plib", "--emit=mir", "-o", "p.mir"]);
    assert_exit(&out, 0);
    let mir = fs::read_to_string(dir.join("p.mir")).unwrap();
    assert!(mir.contains("fn plib::unused_private()"), "{mir}");
    assert!(mir.contains("fn plib::f()"), "{mir}");
    assert!(!mir.contains("fn nabi_f64_cbrt("), "{mir}");
    assert!(mir.matches("\nfn ").count() < 58, "{mir}");
}
