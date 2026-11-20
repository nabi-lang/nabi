//! Stack overflow and shutdown.

#[allow(clippy::wildcard_imports)]
use crate::native::*;

pub(crate) const OVERFLOW: &str = r#"
use std::thread;

// Not a tail call, and no accumulator an optimizer could turn into a loop.
fn down(n: i64) -> i64 {
    if n == 0 { return 0; }
    return down(n - 1) % 7 + 1;
}

async fn deep(n: i64) -> i64 {
    if n == 0 { return 0; }
    return (await deep(n - 1)) % 7 + 1;
}

fn main() {
    print("before\n");
    let args = std::env::args();
    let mode = args[1];
    if mode == "main" {
        println(f"{down(1000000000)}");
    } else if mode == "thread" {
        let h = thread::spawn(|| down(1000000000));
        println(f"{try! h.join()}");
    } else {
        println(f"{task::block_on(deep(1000000000))}");
    }
}
"#;

/// Runaway recursion on the main thread, a spawned thread and through
/// nested `await`s: the runtime reports it (after flushing stdout) and
/// aborts, instead of a bare SIGSEGV.
#[test]
fn stack_overflow_is_reported() {
    let env = setup();
    let dir = scratch("overflow");
    fs::write(dir.join("main.nabi"), OVERFLOW).unwrap();
    let exe = dir.join("out");
    let out = Command::new(NABIC)
        .args(["-q", "build"])
        .arg(dir.join("main.nabi"))
        .arg("--bootstrap")
        .arg(&env.bootstrap)
        .arg("--bootstrap")
        .arg(env.workspace.join("library/std/src/lib.nabi"))
        .arg("--nabir")
        .arg(&env.nabir)
        .args(
            std::env::var("NABI_TEST_OPT")
                .ok()
                .filter(|o| !o.is_empty()),
        )
        .arg("-o")
        .arg(&exe)
        .output()
        .unwrap();
    ok(&out);
    // The main thread's limit counts from the end of its stack mapping,
    // above the argument and environment strings: an environment of a
    // few pages moves that end a page or more past what the C library
    // reports.
    for pad in [0, 5000, 20000] {
        for (mode, name) in [("main", "main"), ("thread", "<unnamed>"), ("async", "main")] {
            let run = Command::new(&exe)
                .arg(mode)
                .env("NABI_TEST_PAD", "x".repeat(pad))
                .output()
                .unwrap();
            let stderr = text(&run.stderr);
            assert!(!run.status.success(), "{mode} {pad}: {:?}", run.status);
            assert!(
                stderr.contains(&format!("thread '{name}' has overflowed its stack")),
                "{mode} {pad}: {stderr}"
            );
            assert_eq!(text(&run.stdout), "before\n", "{mode} {pad}");
        }
    }
}

// A 2.4 MB frame: without stack probes its first store lands far below
// the guard page.
pub(crate) const WIDE: &str = r#"
fn wide(n: i64) -> i64 {
    let a: [i64; 300000] = [n; 300000];
    if n == 0 { return a[7]; }
    return wide(n - 1) % 7 + a[(n % 300000) as usize];
}

fn main() {
    print("before\n");
    println(f"{wide(1000)}");
}
"#;

/// Recursion through huge frames is reported like any overflow (stack
/// probes).
#[test]
fn big_frame_overflow_is_reported() {
    let env = setup();
    let dir = scratch("wide");
    fs::write(dir.join("main.nabi"), WIDE).unwrap();
    let exe = dir.join("out");
    let out = Command::new(NABIC)
        .args(["-q", "build"])
        .arg(dir.join("main.nabi"))
        .arg("--bootstrap")
        .arg(&env.bootstrap)
        .arg("--nabir")
        .arg(&env.nabir)
        .args(
            std::env::var("NABI_TEST_OPT")
                .ok()
                .filter(|o| !o.is_empty()),
        )
        .arg("-o")
        .arg(&exe)
        .output()
        .unwrap();
    ok(&out);
    for pad in [0, 20000] {
        let run = Command::new(&exe)
            .env("NABI_TEST_PAD", "x".repeat(pad))
            .output()
            .unwrap();
        let stderr = text(&run.stderr);
        assert!(!run.status.success(), "{pad}: {:?}", run.status);
        assert!(
            stderr.contains("thread 'main' has overflowed its stack"),
            "{pad}: {stderr}"
        );
        assert_eq!(text(&run.stdout), "before\n", "{pad}");
    }
}

pub(crate) const CHATTY: &str = r#"
use std::thread;

async fn main() {
    let busy = Atomic::new(0);
    for i in 0..4 {
        let t = thread::spawn(|| {
            while true {
                println("tick");
                busy.store(1);
            }
        });
        t.detach();
    }
    let s = task::spawn(async {
        while true {
            println("task");
            await task::yield_now();
        }
    });
    while busy.load() == 0 {}
    print("end\n");
}
"#;

/// Once `main` returns, other threads stop at their next safepoint outside
/// runtime code, so the exit flush never waits on a lock they hold.
#[test]
fn shutdown_stops_other_threads() {
    let env = setup();
    let dir = scratch("chatty");
    fs::write(dir.join("main.nabi"), CHATTY).unwrap();
    let exe = dir.join("out");
    let out = Command::new(NABIC)
        .args(["-q", "build"])
        .arg(dir.join("main.nabi"))
        .arg("--bootstrap")
        .arg(&env.bootstrap)
        .arg("--bootstrap")
        .arg(env.workspace.join("library/std/src/lib.nabi"))
        .arg("--nabir")
        .arg(&env.nabir)
        .args(
            std::env::var("NABI_TEST_OPT")
                .ok()
                .filter(|o| !o.is_empty()),
        )
        .arg("-o")
        .arg(&exe)
        .output()
        .unwrap();
    ok(&out);
    for _ in 0..20 {
        let run = Command::new(&exe).output().unwrap();
        ok(&run);
        let stdout = text(&run.stdout);
        assert!(stdout.contains("end\n"), "{stdout:?}");
    }
}

/// `NABI_BACKTRACE=full`: a panic on the main thread and on a spawned
/// one lists every frame with its address, the panic machinery's too,
/// down to the first frame outside compiled code; unset, nothing.
#[test]
fn panic_backtrace_full() {
    let env = setup();
    let dir = scratch("backtrace");
    fs::write(
        dir.join("main.nabi"),
        r#"
use std::thread;

#[inline(never)]
fn leaf(xs: List<i64>, i: usize) -> i64 { xs[i] }

#[inline(never)]
fn middle(i: usize) -> i64 { leaf([1, 2], i) + 1 }

fn main() {
    if std::env::args()[1] == "thread" {
        let h = thread::spawn(|| middle(5));
        println(f"{try! h.join()}");
    } else {
        println(f"{middle(5)}");
    }
}
"#,
    )
    .unwrap();
    let exe = dir.join("out");
    ok(&build(
        &env,
        &dir.join("main.nabi"),
        &[
            "--bootstrap",
            &env.workspace
                .join("library/std/src/lib.nabi")
                .to_string_lossy(),
            "-o",
            &exe.to_string_lossy(),
        ],
    ));
    for mode in ["main", "thread"] {
        let run = Command::new(&exe)
            .arg(mode)
            .env("NABI_BACKTRACE", "full")
            .output()
            .unwrap();
        let stderr = text(&run.stderr);
        let frames: Vec<&str> = stderr
            .lines()
            .skip_while(|l| *l != "stack backtrace:")
            .skip(1)
            .collect();
        let names: Vec<&str> = frames
            .iter()
            .filter_map(|l| l.split_once(" - ").map(|(_, n)| n))
            .collect();
        let at = |n: &str| names.iter().position(|x| *x == n);
        assert!(names[0].starts_with("core::"), "{mode}: {stderr}");
        assert!(
            at("leaf") < at("middle") && at("leaf").is_some(),
            "{mode}: {stderr}"
        );
        // The walk ends past the C `main` or the thread's start, or on
        // x86-64 static musl at them: its libc start code (__libc_start_main,
        // the thread start) has no unwind tables.
        let last = names.last().copied().unwrap_or("");
        let at_start = last == "main" || last.starts_with("std::thread::");
        assert!(
            last == "<unknown>" || (cfg!(target_env = "musl") && at_start),
            "{mode}: {stderr}"
        );
        assert!(
            frames
                .iter()
                .all(|l| l.trim_start().split_once(": 0x").is_some())
        );
        if mode == "main" {
            assert!(at("middle") < at("main"), "{stderr}");
        }
        let plain = Command::new(&exe).arg(mode).output().unwrap();
        assert!(!text(&plain.stderr).contains("stack backtrace"));
    }
}
