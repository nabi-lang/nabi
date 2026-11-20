//! REPL sessions: libraries, statics, programs, commands and failed cells.

#[allow(clippy::wildcard_imports)]
use crate::tools::*;

/// `repl <lib>`: the library is loaded and its items
/// are in scope from the first cell, also after `:reset`.
#[test]
#[cfg_attr(
    not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(
            target_os = "linux",
            not(target_env = "musl"),
            any(target_arch = "aarch64", target_arch = "x86_64")
        ),
        windows
    )),
    ignore = "the REPL host runs on arm64 macOS, Linux glibc and Windows (arm64, x86_64)"
)]
pub(crate) fn repl_loads_and_uses_a_library() {
    let env = setup();
    let nabir = env.nabir.display().to_string();
    let dir = scratch("repl-pkg");
    let lib = dir.join("geom");
    std::fs::create_dir_all(lib.join("src")).unwrap();
    std::fs::write(
        lib.join("src/lib.nabi"),
        "pub fn twice(x: i32) -> i32 { x * 2 }\n",
    )
    .unwrap();
    let pkg = lib.display().to_string();
    let out = piped(
        NABI,
        &["repl", "--nabir", &nabir, &pkg],
        "twice(21)\n:reset\ntwice(2)\n",
    );
    let stdout = text(&out.stdout);
    assert_eq!(stdout, "42\n4\n", "{}", text(&out.stderr));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A static library's `dllimport` call (through `__imp_<name>`) reaches
/// the DLL function.
#[test]
#[cfg(windows)]
fn repl_static_library_calls_a_dll_import() {
    let env = setup();
    let nabir = env.nabir.display().to_string();
    let dir = scratch("repl_imp");
    crate::tools::put(
        dir.join("imp.c"),
        "__declspec(dllimport) unsigned long GetCurrentProcessId(void);\n\
         int imp_pid(void) { return GetCurrentProcessId() != 0; }\n",
    )
    .unwrap();
    for (exe, args) in [
        // MSVC's, beside its `lib`: no clang needed.
        ("cl", &["/nologo", "/c", "imp.c", "/Foimp.o"][..]),
        ("lib", &["/nologo", "/OUT:imp.lib", "imp.o"]),
    ] {
        let out = Command::new(exe)
            .args(args)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(out.status.success(), "{exe}: {}", text(&out.stderr));
    }
    let lib_dir = dir.display().to_string();
    let out = piped(
        NABI,
        &[
            "repl",
            "--nabir",
            &nabir,
            "-L",
            &lib_dir,
            "-l",
            "static=imp",
        ],
        "extern \"C\" { fn imp_pid() -> i32; }\nunsafe { imp_pid() }\n",
    );
    assert_eq!(text(&out.stdout), "1\n", "{}", text(&out.stderr));
}

/// A cell runs once, in one process: its side effects
/// do not repeat, its bindings / items stay, and a cell that dies
/// leaves the session as it was.
#[test]
#[cfg_attr(
    not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(
            target_os = "linux",
            not(target_env = "musl"),
            any(target_arch = "aarch64", target_arch = "x86_64")
        ),
        windows
    )),
    ignore = "the REPL host runs on arm64 macOS, Linux glibc and Windows (arm64, x86_64)"
)]
pub(crate) fn repl_runs_each_cell_once() {
    let env = setup();
    let nabir = env.nabir.display().to_string();
    let log = scratch("repl-once").join("log");
    let log_s = log.display().to_string().replace('\\', "/");
    let _ = std::fs::remove_file(&log);
    // Cell 1 appends to `log`: a rerun would append again.
    let input = format!(
        "\
println(\"once\"); let old = try fs::read_to_str(\"{log_s}\") catch _ {{ \"\" }}; try fs::write(\"{log_s}\", old + \"x\") catch _ {{ }};
let n = 1i32;
#[derive(Debug)]
struct P {{ x: i32; }}
fn twice(p: P) -> i32 {{ p.x * 2 }}
let p = P {{ x: 21 }};
n += 1;
n += 100; panic(\"boom\");
n
twice(p)
p
"
    );
    let out = piped(
        NABI,
        &["repl", "--nabir", &nabir],
        &format!("use std::fs;\n{input}"),
    );
    let stdout = text(&out.stdout);
    let stderr = text(&out.stderr);
    assert_eq!(stdout, "once\n2\n42\nP { x: 21 }\n", "stderr: {stderr}");
    assert!(stderr.contains("boom"), "{stderr}");
    assert_eq!(std::fs::read_to_string(&log).ok().as_deref(), Some("x"));
    let _ = std::fs::remove_file(&log);
}

/// A binding a later cell could not use is refused in the cell that
/// makes it; one a later cell can name is kept,
/// `any` values too (their methods and type tests work in later cells).
#[test]
#[cfg_attr(
    not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(
            target_os = "linux",
            not(target_env = "musl"),
            any(target_arch = "aarch64", target_arch = "x86_64")
        ),
        windows
    )),
    ignore = "the REPL host runs on arm64 macOS, Linux glibc and Windows (arm64, x86_64)"
)]
pub(crate) fn repl_refuses_bindings_later_cells_cannot_use() {
    let env = setup();
    let nabir = env.nabir.display().to_string();
    let input = "\
mod geo { struct Hid { x: i32; } pub fn mk() -> Hid { Hid { x: 1 } } pub struct Pt { pub x: i32; } }
let h = geo::mk();
let d: any Debug = 5i32;
let pt = geo::Pt { x: 3 };
pt.x
#[derive(Debug)]
struct Q { v: i32; }
let q: any Debug = Q { v: 9 };
d
q
match q { x: Q => x.v, _ => 0 }
";
    let out = piped(NABI, &["repl", "--nabir", &nabir], input);
    let stdout = text(&out.stdout);
    let stderr = text(&out.stderr);
    // An `any Debug` shows the `Debug` of what it holds.
    assert_eq!(stdout, "3\n5\nQ { v: 9 }\n9\n", "stderr: {stderr}");
    assert!(
        stderr.contains("`h: Hid` cannot be kept for later cells (`geo::Hid` is private"),
        "{stderr}"
    );
}

/// `static` items keep their state across cells (a redefinition starts
/// it over); cells are JIT-compiled.
#[test]
#[cfg_attr(
    not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(
            target_os = "linux",
            not(target_env = "musl"),
            any(target_arch = "aarch64", target_arch = "x86_64")
        ),
        windows
    )),
    ignore = "the REPL host runs on arm64 macOS, Linux glibc and Windows (arm64, x86_64)"
)]
pub(crate) fn repl_statics_persist() {
    let env = setup();
    let nabir = env.nabir.display().to_string();
    let input = "\
static N: Atomic<i32> = Atomic::new(0);
fn bump() -> i32 { return N.fetch_add(1) + 1; }
bump()
bump()
N.load()
static N: Atomic<i32> = Atomic::new(100);
bump()
bump()
";
    let out = piped_env(
        NABI,
        &["repl", "--nabir", &nabir],
        input,
        &[("NABI_REPL_TIME", "1")],
    );
    let stderr = text(&out.stderr);
    assert_eq!(text(&out.stdout), "1\n2\n2\n101\n102\n", "stderr: {stderr}");
    assert!(stderr.contains("[repl] jit + run"), "{stderr}");
}

/// Library code and unchanged items compile once per session; a
/// redefinition takes effect from the next cell, also in older code.
#[test]
#[cfg_attr(
    not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(
            target_os = "linux",
            not(target_env = "musl"),
            any(target_arch = "aarch64", target_arch = "x86_64")
        ),
        windows
    )),
    ignore = "the REPL host runs on arm64 macOS, Linux glibc and Windows (arm64, x86_64)"
)]
pub(crate) fn repl_compiles_library_code_once() {
    let env = setup();
    let nabir = env.nabir.display().to_string();
    let input = "\
let l = [1, 2, 3];
l.len()
fn f() -> i32 { 1 }
fn g() -> i32 { f() + 10 }
g()
fn f() -> i32 { 2 }
f()
g()
";
    let out = piped_env(
        NABI,
        &["repl", "--nabir", &nabir],
        input,
        &[("NABI_REPL_TIME", "1")],
    );
    let stderr = text(&out.stderr);
    assert_eq!(text(&out.stdout), "3\n11\n2\n12\n", "stderr: {stderr}");
    let counts: Vec<(usize, usize, usize)> = stderr
        .lines()
        .filter_map(|l| l.strip_prefix("[repl] functions: "))
        .filter_map(|l| {
            let (new, old) = l.split_once(" compiled, ")?;
            let (old, bytes) = old.split_once(" from earlier cells; object: ")?;
            let bytes = bytes.strip_suffix(" bytes")?;
            Some((new.parse().ok()?, old.parse().ok()?, bytes.parse().ok()?))
        })
        .collect();
    assert_eq!(counts.len(), 5, "{stderr}");
    // `l.len()`: `List`'s methods come from the first cell, and its
    // object holds only its own code (no copies of what it does not
    // call, no symbols it does not use).
    assert!(
        counts[0].0 > 10 && counts[1].0 <= 3 && counts[1].1 > 10,
        "{stderr}"
    );
    assert!(counts[1].2 < 4096, "{stderr}");
}

/// The library is collected (or its stage read back) once per session:
/// later cells take the stage the first one kept, whatever their text.
#[test]
#[cfg_attr(
    not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(
            target_os = "linux",
            not(target_env = "musl"),
            any(target_arch = "aarch64", target_arch = "x86_64")
        ),
        windows
    )),
    ignore = "the REPL host runs on arm64 macOS, Linux glibc and Windows (arm64, x86_64)"
)]
pub(crate) fn repl_collects_the_library_once() {
    let env = setup();
    let nabir = env.nabir.display().to_string();
    let out = piped_env(
        NABI,
        &["repl", "--nabir", &nabir],
        "let a = 1;\na + 1\nlet b = a * 3;\nb\n",
        &[("NABI_TIME_PASSES", "1")],
    );
    let stderr = text(&out.stderr);
    assert_eq!(text(&out.stdout), "2\n3\n", "stderr: {stderr}");
    // Looked up in the store by the first cell only.
    let reads = stderr
        .lines()
        .filter(|l| l.ends_with("hir library read"))
        .count();
    assert_eq!(reads, 1, "{stderr}");
}

/// `repl <path>` runs the program in the session; a failure keeps the
/// bindings made before the failing cell.
#[test]
#[cfg_attr(
    not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(
            target_os = "linux",
            not(target_env = "musl"),
            any(target_arch = "aarch64", target_arch = "x86_64")
        ),
        windows
    )),
    ignore = "the REPL host runs on arm64 macOS, Linux glibc and Windows (arm64, x86_64)"
)]
pub(crate) fn repl_path_keeps_the_programs_bindings() {
    let env = setup();
    let nabir = env.nabir.display().to_string();
    let dir = scratch("run_i");
    let prog = dir.join("prog.nabi");
    std::fs::write(
        &prog,
        "\
mod geo { struct Hid { x: i32; } pub fn mk() -> Hid { Hid { x: 1 } } pub fn get(h: Hid) -> i32 { h.x } }
#[derive(Debug)]
struct P { x: i32; }
let n = 1i32;
let h = geo::mk();
let m = geo::get(h) + n;
fn twice(p: P) -> i32 { p.x * 2 }
let p = P { x: m };
let xs = [1, 2, 3];
println(f\"ran {twice(p)}\");
n += 100;
let bad = xs[m + 5];
let never = 0;
",
    )
    .unwrap();
    let path = prog.display().to_string();
    let input = "n\n:type p\ntwice(p) + m\nxs\nnever\n";
    let out = piped(NABI, &["repl", "--nabir", &nabir, &path], input);
    let stdout = text(&out.stdout);
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stdout}\n{stderr}");
    // `n += 100` ran in the cell that failed: `n` is as before it.
    assert_eq!(stdout, "ran 4\n1\nP\n6\n[1, 2, 3]\n", "stderr: {stderr}");
    assert!(stderr.contains(&format!("{path}:12:11")), "{stderr}");
    assert!(
        stderr.contains("the rest of the program did not run"),
        "{stderr}"
    );
    assert!(stderr.contains("cannot find value `never`"), "{stderr}");
    // `h`'s type is private to `geo`: not kept, and nothing after names it.
    assert!(stderr.contains("`h: Hid` cannot be kept"), "{stderr}");

    // `fn main`: it runs; the items stay, its locals do not.
    std::fs::write(
        &prog,
        "fn helper(x: i32) -> i32 { x * 2 }\nfn main() {\n    let local = helper(21);\n    println(f\"main {local}\");\n}\n",
    )
    .unwrap();
    let out = piped(
        NABI,
        &["repl", "--nabir", &nabir, &path],
        "helper(5)\nlocal\n",
    );
    let stderr = text(&out.stderr);
    assert_eq!(text(&out.stdout), "main 42\n10\n", "stderr: {stderr}");
    assert!(stderr.contains("cannot find value `local`"), "{stderr}");

    // A program that does not compile runs nothing; its errors point
    // into it, and the session starts empty.
    std::fs::write(&prog, "println(\"no\");\nlet y = nope + 1;\n").unwrap();
    let out = piped(NABI, &["repl", "--nabir", &nabir, &path], "1 + 1\n");
    let stderr = text(&out.stderr);
    assert_eq!(text(&out.stdout), "2\n", "stderr: {stderr}");
    assert!(stderr.contains(&format!("{path}:2:9")), "{stderr}");

    // A package: its `mod` files resolve.
    let pkg = dir.join("pkg");
    std::fs::create_dir_all(pkg.join("src")).unwrap();
    std::fs::write(
        pkg.join("src/main.nabi"),
        "mod util;\nlet k = util::three();\n",
    )
    .unwrap();
    std::fs::write(pkg.join("src/util.nabi"), "pub fn three() -> i32 { 3 }\n").unwrap();
    let pkg_s = pkg.display().to_string();
    let out = piped(
        NABI,
        &["repl", "--nabir", &nabir, &pkg_s],
        "k + util::three()\n",
    );
    assert_eq!(text(&out.stdout), "6\n", "{}", text(&out.stderr));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A failing cell is undone: its definitions, its writes to earlier
/// objects and statics, and the thread it started are gone.
#[test]
#[cfg_attr(
    not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(
            target_os = "linux",
            not(target_env = "musl"),
            any(target_arch = "aarch64", target_arch = "x86_64")
        ),
        windows
    )),
    ignore = "the REPL host runs on arm64 macOS, Linux glibc and Windows (arm64, x86_64)"
)]
pub(crate) fn repl_survives_failed_cells() {
    let env = setup();
    let nabir = env.nabir.display().to_string();
    let input = "\
use bare::ptr::Ptr;
let kept = 7;
fn peek() -> i64 { let p: Ptr<i64> = Ptr::dangling(); unsafe { p.read() } }
peek()
kept + 1
fn down(n: i32) -> i32 { if n == 0 { panic(\"deep\") } else { down(n - 1) + 1 } }
down(50)
kept + 2
std::process::exit(3);
fn f() -> i32 { 1 }
f()
fn f() -> i32 { panic(\"v2\") }
f()
fn f() -> i32 { 1 }
f()
kept + 3
use std::thread;
static C: Atomic<i64> = Atomic::new(0);
let xs = [1, 2, 3];
C.store(1);
xs.push(4); C.store(5); thread::spawn(|| { while true { C.fetch_add(1); } }); thread::sleep(Duration::ms(50)); panic(\"after\");
xs
C.load()
thread::sleep(Duration::ms(200));
C.load()
";
    let out = piped(NABI, &["repl", "--nabir", &nabir], input);
    let stderr = text(&out.stderr);
    assert_eq!(
        text(&out.stdout),
        "8\n9\n1\n1\n10\n[1, 2, 3]\n1\n1\n",
        "stderr: {stderr}"
    );
    let failed = |why: &str| {
        stderr
            .matches(&format!("error: {why}; the session is unchanged"))
            .count()
    };
    assert_eq!(failed("the cell failed"), 1, "{stderr}");
    assert_eq!(failed("the cell panicked"), 3, "{stderr}");
    assert_eq!(failed("the cell called exit(3)"), 1, "{stderr}");
    assert!(stderr.contains("deep") && stderr.contains("v2"), "{stderr}");
}

/// Executor threads survive a failed cell; a thread running when it
/// failed is gone.
#[test]
#[cfg_attr(
    not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(
            target_os = "linux",
            not(target_env = "musl"),
            any(target_arch = "aarch64", target_arch = "x86_64")
        ),
        windows
    )),
    ignore = "the REPL host runs on arm64 macOS, Linux glibc and Windows (arm64, x86_64)"
)]
pub(crate) fn repl_threads_after_a_failed_cell() {
    let env = setup();
    let nabir = env.nabir.display().to_string();
    let input = "\
use core::task;
use std::thread;
task::block_on(async { let t = task::spawn(async { 20 + 1 }); await t })
task::block_on(async { await task::spawn_blocking(|| 7) })
task::block_on(async { let t = task::spawn(async { 5 }); await t }); panic(\"in the middle\");
task::block_on(async { let t = task::spawn(async { 40 + 2 }); await t })
task::block_on(async { await task::spawn_blocking(|| 8) })
let h = thread::spawn(|| { let n: i64 = 0; while n >= 0 { n += 1; } n });
thread::sleep(Duration::ms(100)); panic(\"busy\");
let j = try h.join() catch e { println(f\"{e}\"); -1i64 };
j
task::block_on(async { let ts = [task::spawn(async { 1 }), task::spawn(async { 2 })]; let s = 0; for t in ts { s += await t; } s })
";
    let out = piped(NABI, &["repl", "--nabir", &nabir], input);
    let stdout = text(&out.stdout);
    let stderr = text(&out.stderr);
    assert_eq!(
        stdout,
        "21\n7\n42\n8\nthread join failed: the thread ended with a failed REPL cell\n-1\n3\n",
        "stderr: {stderr}"
    );
    assert_eq!(
        stderr.matches("error: the cell panicked").count(),
        2,
        "{stderr}"
    );
}

/// A failed cell loses nothing that ran on the session's threads, and the
/// locks of a thread gone afterwards are released.
#[test]
#[cfg_attr(
    not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(
            target_os = "linux",
            not(target_env = "musl"),
            any(target_arch = "aarch64", target_arch = "x86_64")
        ),
        windows
    )),
    ignore = "the REPL host runs on arm64 macOS, Linux glibc and Windows (arm64, x86_64)"
)]
pub(crate) fn repl_failed_cell_loses_no_work_and_no_lock() {
    let env = setup();
    let nabir = env.nabir.display().to_string();
    let input = "\
use core::sync::Once;
use core::task;
use core::time::Instant;
use std::thread;
static POLLED: Atomic<i64> = Atomic::new(0);
static JOB: Atomic<i64> = Atomic::new(0);
fn spin(ms: i64) { let t = Instant::now(); while t.elapsed().as_ms() < ms {} }
let t = task::block_on(async { task::spawn(async { spin(1500); POLLED.store(1); 1 }) }); let b = task::block_on(async { task::spawn_blocking(|| { spin(1500); JOB.store(1); 2 }) });
panic(\"at once\");
(POLLED.load(), JOB.load())
task::block_on(async { (await t) + (await b) })
static IN: Atomic<i64> = Atomic::new(0);
static GO: Atomic<i64> = Atomic::new(0);
let m = Mutex::new(5); let rw = RwLock::new(6); let once = Once::new();
let h = thread::spawn(|| { using g = m.lock() { using r = rw.read() { once.call_once(|| { IN.store(1); while GO.load() == 0 { thread::park(); } }); } } });
while IN.load() == 0 { thread::sleep(Duration::ms(5)); } thread::sleep(Duration::ms(100));
panic(\"holder gone\");
GO.store(1); h.thread().unpark();
using g = m.lock() { g.get() }
using w = rw.write() { w.get() }
once.call_once(|| { println(\"ran again\"); });
";
    let out = piped(NABI, &["repl", "--nabir", &nabir], input);
    let stdout = text(&out.stdout);
    let stderr = text(&out.stderr);
    // The holder sits in `call_once` with both locks until `GO`, parked
    // in one wait so a rollback finds it where the checkpoint did (a
    // thread that ran since counts as gone). Windows keeps that thread,
    // which then lets them go; a fork host has none.
    let again = if cfg!(windows) { "" } else { "ran again\n" };
    assert_eq!(
        stdout,
        format!("(1, 1)\n3\n5\n6\n{again}"),
        "stderr: {stderr}"
    );
    assert_eq!(
        stderr.matches("error: the cell panicked").count(),
        2,
        "{stderr}"
    );
}

/// A compiler panic on a cell fails that cell; the session goes on.
#[test]
#[cfg_attr(
    not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(
            target_os = "linux",
            not(target_env = "musl"),
            any(target_arch = "aarch64", target_arch = "x86_64")
        ),
        windows
    )),
    ignore = "the REPL host runs on arm64 macOS, Linux glibc and Windows (arm64, x86_64)"
)]
pub(crate) fn repl_survives_a_compiler_panic() {
    let env = setup();
    let nabir = env.nabir.display().to_string();
    let out = piped_env(
        NABI,
        &["repl", "--nabir", &nabir],
        "let x = 40;\nx + 1 // ice-here\nx + 2\n",
        &[("NABI_TEST_ICE", "ice-here")],
    );
    let stderr = text(&out.stderr);
    assert_eq!(text(&out.stdout), "42\n", "{stderr}");
    assert!(
        stderr.contains("internal compiler error") && stderr.contains("NABI_TEST_ICE"),
        "{stderr}"
    );
    assert_eq!(out.status.code(), Some(0), "{stderr}");
}

/// A cell with a top-level `await` keeps its bindings and shows its
/// value; `it` holds the last value shown, and the user's `it` wins.
#[test]
#[cfg_attr(
    not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(
            target_os = "linux",
            not(target_env = "musl"),
            any(target_arch = "aarch64", target_arch = "x86_64")
        ),
        windows
    )),
    ignore = "the REPL host runs on arm64 macOS, Linux glibc and Windows (arm64, x86_64)"
)]
pub(crate) fn repl_await_cells_and_it() {
    let env = setup();
    let nabir = env.nabir.display().to_string();
    let input = "\
async fn af() -> i32 { return 3; }
let w: i32 = 10;
let v = await af(); println(f\"got {v}\");
v + w
await af()
it + 1
it
xs
let it = 7;
5
it
:reset
fn it() -> i32 { 9 }
2
it()
";
    let out = piped(NABI, &["repl", "--nabir", &nabir], input);
    let stdout = text(&out.stdout);
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stdout}\n{stderr}");
    assert_eq!(
        stdout, "got 3\n13\n3\n4\n4\n5\n7\n2\n9\n",
        "stderr: {stderr}"
    );
    // A failed cell leaves `it` alone.
    assert!(stderr.contains("cannot find value `xs`"), "{stderr}");
}

/// `exit` / `quit` alone end the session unless the session defines
/// the name.
#[test]
#[cfg_attr(
    not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(
            target_os = "linux",
            not(target_env = "musl"),
            any(target_arch = "aarch64", target_arch = "x86_64")
        ),
        windows
    )),
    ignore = "the REPL host runs on arm64 macOS, Linux glibc and Windows (arm64, x86_64)"
)]
pub(crate) fn repl_exit_words_end_the_session() {
    let env = setup();
    let nabir = env.nabir.display().to_string();
    let args = ["repl", "--nabir", &nabir];
    for (input, want) in [
        ("1\nexit\n2\n", "1\n"),
        ("quit()\n2\n", ""),
        ("let exit = 5;\nexit\nquit\n2\n", "5\n"),
        ("fn quit() -> i32 { 6 }\nquit()\n:q\n2\n", "6\n"),
    ] {
        let out = piped(NABI, &args, input);
        let stderr = text(&out.stderr);
        assert_eq!(out.status.code(), Some(0), "{stderr}");
        assert_eq!(text(&out.stdout), want, "{input}: {stderr}");
    }
}

/// `:doc`, `:time`, `:vars`, `:load`, `:reload`, `:opt`, `:save` (read back
/// with `repl <file>`) and the commands' usage errors.
#[test]
#[cfg_attr(
    not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(
            target_os = "linux",
            not(target_env = "musl"),
            any(target_arch = "aarch64", target_arch = "x86_64")
        ),
        windows
    )),
    ignore = "the REPL host runs on arm64 macOS, Linux glibc and Windows (arm64, x86_64)"
)]
pub(crate) fn repl_session_commands() {
    let env = setup();
    let nabir = env.nabir.display().to_string();
    let dir = scratch("repl-commands");
    std::fs::create_dir_all(&dir).unwrap();
    let prog = dir.join("p.nabi");
    std::fs::write(
        &prog,
        "let base = 40i32;\nfn add(a: i32, b: i32) -> i32 { a + b }\nprintln(\"loaded\");\n",
    )
    .unwrap();
    let saved = dir.join("s.nabi");
    let (prog_s, saved_s) = (prog.display().to_string(), saved.display().to_string());
    let input = format!(
        "\
/// Doubles it.
fn twice(x: i32) -> i32 {{ x * 2 }}
:doc twice
:time twice(4)
:load {prog_s}
add(base, 2)
:reload
let xs = [1, 2];
xs.push(3)
it
it * 2
let xs = \"s\"
:vars
:opt
:opt 9
:opt 0
:save {saved_s}
:doc
:load
:load {dir}
:frob
:help
",
        dir = dir.display()
    );
    let out = piped(NABI, &["repl", "--nabir", &nabir], &input);
    let stdout = text(&out.stdout);
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stdout}\n{stderr}");
    let help = stdout.find("  <expr> ").unwrap_or(stdout.len());
    assert_eq!(
        &stdout[..help],
        "\
fn twice(x: i32) -> i32

Doubles it.
8
loaded
42
loaded
42
84
base: i32
it:   i32
xs:   str
fn twice(x: i32) -> i32
fn add(a: i32, b: i32) -> i32
-O0
",
        "stderr: {stderr}"
    );
    for cmd in [
        ":doc <name>",
        ":time <expr>",
        ":vars",
        ":load <file>",
        ":reload",
        ":edit",
        ":save <file>",
        ":opt",
        ":add <name>[@<req>]",
        ":type <expr>, :t <expr>",
        ":help, :h",
        ":exit, :quit, :q, exit, quit",
    ] {
        assert!(stdout.contains(cmd), "{cmd}: {stdout}");
    }
    for e in [
        "[repl] front end: ",
        "[repl] total: ",
        "`:opt` takes 0, 1, 2, 3 or s",
        "`:doc` takes a name: `:doc xs.len`",
        "`:load` takes a file: `:load prog.nabi`",
        "`:load` takes a `.nabi` file; for a package directory start `nabi repl",
        "unknown command `:frob`",
    ] {
        assert!(stderr.contains(e), "{e}: {stderr}");
    }
    // The script keeps each cell as typed, a name declared again
    // redeclared; read back, it runs the cells again, one by one: `xs`
    // is the last one declared.
    let script = std::fs::read_to_string(&saved).unwrap();
    assert_eq!(
        script.matches("\nlet base = 40i32;\n").count(),
        2,
        "{script}"
    );
    assert!(script.contains("\nlet it = add(base, 2);\n"), "{script}");
    assert!(
        script.ends_with("\n// :cell\nlet xs = \"s\";\n"),
        "{script}"
    );
    let out = piped(
        NABI,
        &["repl", "--nabir", &nabir, &saved_s],
        "base + twice(1)\nit\nxs\n",
    );
    let stderr = text(&out.stderr);
    assert_eq!(
        text(&out.stdout),
        "loaded\nloaded\n42\n42\ns\n",
        "{script}\n{stderr}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// `:load` of a `:save`d session replays its cells: a name declared
/// again with another type keeps its last binding.
#[test]
#[cfg_attr(
    not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(
            target_os = "linux",
            not(target_env = "musl"),
            any(target_arch = "aarch64", target_arch = "x86_64")
        ),
        windows
    )),
    ignore = "the REPL host runs on arm64 macOS, Linux glibc and Windows (arm64, x86_64)"
)]
pub(crate) fn load_of_saved_session_keeps_redeclared_binding() {
    let env = setup();
    let nabir = env.nabir.display().to_string();
    let dir = scratch("repl-save-load");
    std::fs::create_dir_all(&dir).unwrap();
    let saved = dir.join("s.nabi").display().to_string();
    let input = format!("let a = 1\nlet a = \"s\"\n:save {saved}\n:reset\n:load {saved}\na\n");
    let out = piped(NABI, &["repl", "--nabir", &nabir], &input);
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert_eq!(text(&out.stdout), "s\n", "{stderr}");
    // `nabi run` of the script still works: the second `let a` redeclares it.
    let out = piped(NABI, &["run", "-q", "--nabir", &nabir, &saved], "");
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let _ = std::fs::remove_dir_all(&dir);
}

/// `:edit` runs what the editor leaves in its file.
#[test]
#[cfg_attr(
    not(all(
        any(
            target_os = "macos",
            all(target_os = "linux", not(target_env = "musl"))
        ),
        any(target_arch = "aarch64", target_arch = "x86_64")
    )),
    ignore = "the editor is `cp`: the REPL host on macOS and Linux glibc, arm64 and x86_64"
)]
pub(crate) fn repl_edit_runs_the_editors_cells() {
    let env = setup();
    let nabir = env.nabir.display().to_string();
    let dir = scratch("repl-edit");
    std::fs::create_dir_all(&dir).unwrap();
    let cells = dir.join("cells.nabi");
    std::fs::write(&cells, "fn g() -> i32 { 5 }\nlet b = g() + 1\nb * 2\n").unwrap();
    // An editor that writes the cells into the file it is given.
    let editor = format!("cp {}", cells.display());
    let out = piped_env(
        NABI,
        &["repl", "--nabir", &nabir],
        ":edit\nb + g()\n",
        &[("VISUAL", ""), ("EDITOR", &editor)],
    );
    let stderr = text(&out.stderr);
    assert_eq!(text(&out.stdout), "12\n11\n", "{stderr}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// In a package the session does not load, a name the package defines
/// gets a hint; `repl .` names the package's directory.
#[test]
#[cfg_attr(
    not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(
            target_os = "linux",
            not(target_env = "musl"),
            any(target_arch = "aarch64", target_arch = "x86_64")
        ),
        windows
    )),
    ignore = "the REPL host runs on arm64 macOS, Linux glibc and Windows (arm64, x86_64)"
)]
pub(crate) fn repl_hints_at_the_package_it_runs_in() {
    let env = setup();
    let nabir = env.nabir.display().to_string();
    let dir = scratch("repl-here").join("nabipk");
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("src/lib.nabi"),
        "pub fn add(a: i32, b: i32) -> i32 { a + b }\n",
    )
    .unwrap();
    let run = |args: &[&str], input: &str| {
        let mut child = Command::new(NABI)
            .args(args)
            .args(["--nabir", &nabir])
            .current_dir(&dir)
            .env("NABI_HOME", tool_home())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    };
    let out = run(&["repl"], "add(1, 2)\nnope(1)\n");
    let stderr = text(&out.stderr);
    assert_eq!(text(&out.stdout), "", "{stderr}");
    assert_eq!(
        stderr
            .matches("help: `add` is defined in this package; load this package: `nabi repl .`")
            .count(),
        1,
        "{stderr}"
    );
    // The package's library built, the cell runs.
    let out = run(&["repl", "."], "add(1, 2)\n");
    let stderr = text(&out.stderr);
    assert_eq!(text(&out.stdout), "3\n", "{stderr}");
    assert!(stderr.contains("Compiling nabipk (.)"), "{stderr}");
    let _ = std::fs::remove_dir_all(dir.parent().unwrap());
}

/// A value shows with `Display`, else `Debug`, else its type name.
#[test]
#[cfg_attr(
    not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(
            target_os = "linux",
            not(target_env = "musl"),
            any(target_arch = "aarch64", target_arch = "x86_64")
        ),
        windows
    )),
    ignore = "the REPL host runs on arm64 macOS, Linux glibc and Windows (arm64, x86_64)"
)]
pub(crate) fn repl_shows_values_in_full() {
    let env = setup();
    let nabir = env.nabir.display().to_string();
    let input = "\
let xs = [1, 2, 3]
xs
(1, \"a\", [true])
enum E { A, B(i32) }
E::B(3)
#[derive(Debug)]
enum F { A, B(i32) }
F::B(3)
struct S { a: i32 }
S { a: 1 }
S { a: 1 }?
#[derive(Debug)]
struct D { a: i32 }
D { a: 1 }
struct T { a: i32 }
impl Display for T { fn fmt(self, ref f: Formatter) throws (FmtError) { try f.write_str(f\"T={self.a}\") } }
T { a: 2 }
T { a: 2 }?
nope?
nope(); S { a: 1 }
";
    let out = piped(NABI, &["repl", "--nabir", &nabir], input);
    let stderr = text(&out.stderr);
    assert_eq!(
        text(&out.stdout),
        "[1, 2, 3]\n(1, \"a\", [true])\nE\nB(3)\nS\nS\nD { a: 1 }\nT=2\nT\n",
        "stderr: {stderr}"
    );
    // A trailing `?` keeps the error's location; a value that cannot show
    // adds no error to the cell's.
    assert_eq!(
        stderr,
        "\
error[E0027]: cannot find value `nope` in this scope
  --> <repl>:1:1
   |
 1 | nope?
   | ^^^^
error[E0027]: cannot find function `nope` in this scope
  --> <repl>:1:1
   |
 1 | nope(); S { a: 1 }
   | ^^^^^^
"
    );
}

/// A cell's failure says what happened: a panic in an earlier cell's
/// function names that cell, a throw and `exit` say so, and the generated
/// `main` is never named.
#[test]
#[cfg_attr(
    not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(
            target_os = "linux",
            not(target_env = "musl"),
            any(target_arch = "aarch64", target_arch = "x86_64")
        ),
        windows
    )),
    ignore = "the REPL host runs on arm64 macOS, Linux glibc and Windows (arm64, x86_64)"
)]
pub(crate) fn repl_reports_how_a_cell_failed() {
    let env = setup();
    let nabir = env.nabir.display().to_string();
    let input = "\
let kept = 1
fn boom(n: i32) {
    panic(f\"boom {n}\");
}
boom(5)
fn t() throws { throw ValueError::new(\"bad\"); }
kept += 1; try t()
kept += 1; std::process::exit(3);
kept
";
    let out = piped(NABI, &["repl", "--nabir", &nabir], input);
    let stderr = text(&out.stderr);
    assert_eq!(text(&out.stdout), "1\n", "stderr: {stderr}");
    assert_eq!(
        stderr,
        "\
thread 'main' panicked at <repl cell 2>:2:5:
boom 5
error: the cell panicked; the session is unchanged
error: the cell threw `bad`; the session is unchanged
  --> <repl cell 5>:1:16
  raised at <repl cell 4>:1:17
error: the cell called exit(3); the session is unchanged (`exit` or `:quit` ends the session)
"
    );
}

/// A redefinition that breaks an earlier item names it, its cell and its
/// text, and says how to go on; redefining both in one cell works.
#[test]
#[cfg_attr(
    not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(
            target_os = "linux",
            not(target_env = "musl"),
            any(target_arch = "aarch64", target_arch = "x86_64")
        ),
        windows
    )),
    ignore = "the REPL host runs on arm64 macOS, Linux glibc and Windows (arm64, x86_64)"
)]
pub(crate) fn repl_redefinition_names_broken_dependents() {
    let env = setup();
    let nabir = env.nabir.display().to_string();
    let input = "\
struct P { x: i32 }
impl P { fn get(self) -> i32 { self.x } }
impl P { fn two(self) -> i32 { 2 } }
struct P { y: i32 }
struct P { y: i32 } impl P { fn get(self) -> i32 { self.y } }
P { y: 4 }.get() + P { y: 0 }.two()
fn f() -> i32 { 1 }
fn g() -> i32 { f() + 1 }
fn f() -> str { \"x\" } fn g() -> str { f() }
g()
";
    let out = piped(NABI, &["repl", "--nabir", &nabir], input);
    let stderr = text(&out.stderr);
    assert_eq!(text(&out.stdout), "6\nx\n", "stderr: {stderr}");
    assert_eq!(
        stderr,
        "\
error[E0043]: struct `P` has no field `x`
  --> <repl cell 2>:1:32
   |
 1 | impl P { fn get(self) -> i32 { self.x } }
   |                                ^^^^^^
   |
   = help: available fields: `y`
   = help: `impl P` from cell 2 no longer compiles with this definition: redefine it in this cell too, or `:reset` to start over
"
    );
}
