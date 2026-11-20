//! Symbol names, demangling and the runtime ABI.

#[allow(clippy::wildcard_imports)]
use crate::cli::*;

/// Symbol mangling in a host executable's symbol table.
#[test]
fn symbols_follow_the_mangling_scheme() {
    let env = setup();
    if cfg!(windows) {
        eprintln!("skipped: a PE executable's symbols are in its PDB");
        return;
    }
    if Command::new("nm").arg("--version").output().is_err() {
        eprintln!("skipped: no nm");
        return;
    }
    let dir = scratch("mangle");
    let ident = "mod geom {\n    pub fn ident<T>(x: T) -> T { x }\n}\n";
    for (name, body) in [
        (
            "a",
            "#[export_name(\"nabi_marker_fn\")]\npub extern \"C\" fn marker() -> i32 { 3 }\n\
             extern \"C\" { fn nabi_marker_fn() -> i32; }\n\
             println(f\"{geom::ident(2)} {geom::ident(\"s\")} {unsafe { nabi_marker_fn() }}\");\n",
        ),
        ("b", "println(f\"{geom::ident(5)}\");\n"),
    ] {
        fs::create_dir_all(dir.join(name)).unwrap();
        put(
            dir.join(name).join("src/main.nabi"),
            format!("{ident}{body}"),
        )
        .unwrap();
        assert_exit(&nabic(Some(&env), &dir, &["-q", "build", name, "-O0"]), 0);
    }
    // `(pkg hash, typehash)` of each `geom::ident` instance, and every
    // symbol (Mach-O's leading `_` dropped).
    let symbols = |name: &str| {
        let exe = dir.join(name).join("target/debug").join(name);
        let out = Command::new("nm").arg(&exe).output().unwrap();
        let all: Vec<String> = text(&out.stdout)
            .lines()
            .filter_map(|l| l.split_whitespace().nth(2))
            .map(|s| {
                s.strip_prefix('_')
                    .filter(|_| cfg!(target_os = "macos"))
                    .unwrap_or(s)
            })
            .map(str::to_owned)
            .collect();
        let hex = |s: &str, n: usize| s.len() == n && s.bytes().all(|b| b.is_ascii_hexdigit());
        let mut inst: Vec<(String, String)> = all
            .iter()
            .filter_map(|s| {
                let rest = s.strip_prefix("_Y")?;
                let (pkg, rest) = rest.split_once('_')?;
                let th = rest.strip_prefix("geom_ident_")?;
                (hex(pkg, 8) && hex(th, 16)).then(|| (pkg.to_owned(), th.to_owned()))
            })
            .collect();
        inst.sort();
        (inst, all)
    };
    let (a, a_all) = symbols("a");
    let (b, b_all) = symbols("b");
    assert_eq!(a.len(), 2, "{a_all:?}");
    assert_eq!(b.len(), 1, "{b_all:?}");
    assert_ne!(a[0].0, b[0].0, "package hashes");
    assert!(a.iter().any(|(_, th)| *th == b[0].1), "{a:?} {b:?}");
    for want in ["nabi_marker_fn", "main", &format!("_Y{}__main_", a[0].0)] {
        assert!(a_all.iter().any(|s| s == want), "no {want}: {a_all:?}");
    }
    // `_Y` is followed by the lowercase hex package hash, never
    // by the reserved `_YK` / `_YL` / `_YI` / `_YM`.
    for s in a_all.iter().filter_map(|s| s.strip_prefix("_Y")) {
        assert!(
            s.get(..8)
                .is_some_and(|h| h.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))),
            "_Y{s}"
        );
    }
}

#[test]
fn usage_errors() {
    let dir = scratch("misc");
    assert_exit(&nabic(None, &dir, &["build", "--bogus"]), 2);
    assert_exit(&nabic(None, &dir, &["frobnicate"]), 2);
    // An unknown command or option says what to write instead.
    let out = nabic(None, &dir, &["fc.nabi"]);
    assert_exit(&out, 2);
    assert_eq!(
        text(&out.stderr),
        "nabic: unknown command `fc.nabi`; did you mean `nabic build fc.nabi`?\nrun `nabic help` for usage\n"
    );
    let out = nabic(None, &dir, &["biuld"]);
    assert!(text(&out.stderr).contains("did you mean `build`?"));
    let out = nabic(None, &dir, &["build", "--relase"]);
    assert_exit(&out, 2);
    assert_eq!(
        text(&out.stderr),
        "nabic: unknown option `--relase`; did you mean `--release`?\nrun `nabic help build` for usage\n"
    );
    // One path (`test`: and a filter).
    assert_exit(&nabic(None, &dir, &["build", "a", "b"]), 2);
    assert_exit(&nabic(None, &dir, &["test", "a", "b", "c"]), 2);
    assert_exit(&nabic(None, &dir, &["--version"]), 0);
    let out = nabic(None, &dir, &["-V", "-v"]);
    assert!(
        text(&out.stdout).contains("\nhost: "),
        "{}",
        text(&out.stdout)
    );
    // Per-command help: the entry and its options only.
    let out = nabic(None, &dir, &["test", "--help"]);
    assert_exit(&out, 0);
    let help = text(&out.stdout);
    assert!(help.contains("test [path] [filter]") && help.contains("`nabi test` runs it"));
    assert!(
        !help.contains("--check-library")
            && !help.contains("--iters")
            && !help.contains("--threads"),
        "{help}"
    );
}

/// A program over the runtime ABI (the TLS block, GC allocation, throws,
/// volatile access, recursion) runs at -O2.
#[test]
fn emitted_code_follows_the_runtime_abi() {
    let env = setup();
    let dir = scratch("abi");
    let pkg = dir.join("abi");
    let src = "use bare::volatile;\nuse bare::ptr::Ptr;\nuse std::env;\n\
               struct P { a: i64; b: str; }\n\
               #[inline(never)]\nfn keep(x: i64) -> i64 { x * 3 + 1 }\n\
               fn mk(n: i64) -> P { P { a: n, b: \"s\" } }\n\
               fn len_of(xs: List<i64>) -> usize { xs.len() }\n\
               fn fails(n: i64) throws -> i64 {\n\
               \x20   if n > 5 { throw ValueError::new(\"big\"); }\n\
               \x20   n\n\
               }\n\
               fn down(n: i64, acc: i64) -> i64 { if n == 0 { acc } else { down(n - 1, acc + 1) } }\n\
               fn main() {\n\
               \x20   let t = 0i64;\n\
               \x20   for i in 0..10i64 { t += keep(i) + mk(i).a; }\n\
               \x20   let p: Ptr<i64> = Ptr::dangling();\n\
               \x20   let r = try? fails(t);\n\
               \x20   let deep = down(env::args().len() as i64 * 20000000, 0);\n\
               \x20   println(f\"{t} {len_of([1, 2])} {r ?? -1} {deep}\");\n\
               \x20   if t < 0 {\n\
               \x20       unsafe { volatile::write(p, 1i64); println(f\"{volatile::read(p.as_const())}\"); }\n\
               \x20   }\n\
               }\n";
    put(pkg.join("src/main.nabi"), src).unwrap();
    let out = build_run(Some(&env), &pkg, &["-q", "-O2"]);
    assert_exit(&out, 0);
    assert!(
        text(&out.stdout).ends_with(" 20000000\n"),
        "{}",
        text(&out.stdout)
    );
}

/// Every C symbol the runtime defines is `nabi_`-prefixed.
#[test]
fn runtime_symbols_carry_the_nabi_prefix() {
    if cfg!(windows) || Command::new("nm").arg("--version").output().is_err() {
        eprintln!("skipped: no nm");
        return;
    }
    let env = crate::common::setup();
    let out = Command::new("nm")
        .arg("-g")
        .arg(&env.nabir)
        .output()
        .unwrap();
    assert_exit(&out, 0);
    let listing = text(&out.stdout);
    let defined: std::collections::BTreeSet<&str> = listing
        .lines()
        .filter_map(|l| match l.split_whitespace().collect::<Vec<_>>()[..] {
            [_, kind, name] if "TDBRSC".contains(kind) => Some(name),
            _ => None,
        })
        .map(|s| {
            s.strip_prefix('_')
                .filter(|_| cfg!(target_os = "macos"))
                .unwrap_or(s)
        })
        .collect();
    let toolchain = |s: &str| {
        // Rust-mangled and reserved `_` names, compiler builtins.
        ["_", "rust_", "anon.", "ltmp", "atomic_", "."]
            .iter()
            .any(|p| s.starts_with(p))
    };
    let foreign: Vec<&&str> = defined
        .iter()
        .filter(|s| !toolchain(s) && !s.starts_with("nabi_"))
        .collect();
    assert!(
        foreign.is_empty(),
        "runtime symbols without `nabi_`: {foreign:?}"
    );
    for want in [
        "nabi_gc_alloc_slow",
        "nabi_runtime_init",
        "nabi_thread_spawn",
        "nabi_coro_park",
        "nabi_task_key",
        "nabi_io_poll",
        "nabi_timer_register",
        "nabi_panic",
    ] {
        assert!(defined.contains(want), "the runtime lacks `{want}`");
    }
}
