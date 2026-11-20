//! Native code against the platform: the C ABI against clang-built C, the
//! stack-overflow report, `-g` DWARF. The C ABI tests (`abi`, `ffi`, built
//! by [`ffi::c_program`]) run on every host and, with `NABI_TEST_TARGET`,
//! for that target; the rest on Unix hosts.

mod abi;
#[cfg(unix)]
mod debug;
mod ffi;
#[cfg(unix)]
mod formatters;
mod hot;
#[cfg(unix)]
mod link;
mod plugin;
#[cfg(unix)]
mod runtime;
#[cfg(unix)]
mod windows;

use crate::common::{Env, NABIC, scratch, setup, text};
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

/// `nabic build <src> <extra..>` with the harness library and runtime.
fn build(env: &Env, src: &Path, extra: &[&str]) -> Output {
    Command::new(NABIC)
        .arg("-q")
        .arg("build")
        .arg(src)
        .arg("--bootstrap")
        .arg(&env.bootstrap)
        .arg("--nabir")
        .arg(&env.nabir)
        .args(crate::common::test_target().map(|t| format!("--target={t}")))
        .envs(crate::common::in_progress_env())
        .args(crate::common::opt_flags())
        .args(extra)
        .output()
        .unwrap()
}

#[track_caller]
fn ok(out: &Output) {
    assert!(
        out.status.success(),
        "status {:?}\nstdout: {}\nstderr: {}",
        out.status,
        text(&out.stdout),
        text(&out.stderr)
    );
}

/// A nest of counted loops gets a copy entered when its bounds are at
/// most 8.
#[cfg(unix)]
#[test]
fn short_nest_polls_nowhere() {
    let env = setup();
    let dir = scratch("shortnest");
    fs::write(
        dir.join("main.nabi"),
        r#"
fn square(n: usize) -> usize {
    let c: usize = 0;
    for i in 0..n {
        for j in 0..n {
            c += i * 10 + j;
        }
    }
    c
}

fn main() -> i32 {
    println(f"{square(3)}");
    return 0;
}
"#,
    )
    .unwrap();
    let path = dir.join("main.mir");
    ok(&build(
        &env,
        &dir.join("main.nabi"),
        &["--emit=mir", "-o", path.to_str().unwrap()],
    ));
    let mir = fs::read_to_string(path).unwrap();
    let square = &mir[mir.find("fn square").unwrap()..];
    let square = &square[..square.find("\nfn ").unwrap_or(square.len())];
    assert_eq!(square.matches(", 8usize)").count(), 2, "{square}");
}

/// `print` / `println` take any `Display` value and default the argument to
/// the empty string, so `println()` writes a blank line.
#[cfg(unix)]
const PRINTING: &str = "fn main() {
    println(\"line\");
    println(42);
    println(3.5f64);
    println(true);
    print(\"a\");
    print(\"b\");
    println();
    println(f\"x={1 + 2}\");
    println();
}
";

#[cfg(unix)]
#[test]
fn printing_display_and_blank_line() {
    let env = setup();
    let dir = scratch("printing");
    fs::write(dir.join("main.nabi"), PRINTING).unwrap();
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
    let run = Command::new(&exe).output().unwrap();
    ok(&run);
    assert_eq!(text(&run.stdout), "line\n42\n3.5\ntrue\nab\nx=3\n\n");
}
