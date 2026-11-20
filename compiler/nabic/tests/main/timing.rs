//! Timing-leak check of `core::crypto` (`tests/timing.nabi`): each
//! probe times an operation on two classes of secret input, built at -O2
//! and -O3, and fails above a Welch |t| of `NABI_TIMING_LIMIT` (10, the
//! dudect "definitely not constant time" mark). The `control` probe, an
//! early-exit compare, must exceed it. Ignored by default, since
//! timing is noisy on a loaded machine: run with `--ignored` on a quiet
//! host. `NABI_TIMING_SCALE` multiplies the sample counts.

use crate::common::{NABIC, emulated, exe_name, run_command, scratch, setup, test_target, text};
use std::process::Command;

#[test]
#[ignore = "timing measurement: run alone with --ignored"]
fn crypto_runs_in_constant_time() {
    if test_target().is_some() || emulated() {
        eprintln!("timing: host only, skipped for NABI_TEST_TARGET");
        return;
    }
    let env = setup();
    let limit: f64 = std::env::var("NABI_TIMING_LIMIT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(10.0);
    let scale = std::env::var("NABI_TIMING_SCALE").unwrap_or_else(|_| "1".into());
    let src = env.workspace.join("tests/timing.nabi");
    let mut leaks = Vec::new();
    let mut probes = 0;
    for level in ["-O2", "-O3"] {
        let dir = scratch(&format!("timing{level}"));
        let exe = dir.join(exe_name("timing"));
        let out = Command::new(NABIC)
            .arg("build")
            .arg(&src)
            .arg("--bootstrap")
            .arg(&env.bootstrap)
            .arg("--bootstrap")
            .arg(env.workspace.join("library/std/src/lib.nabi"))
            .arg("--bootstrap")
            .arg(env.workspace.join("library/bare/src/lib.nabi"))
            .arg("--nabir")
            .arg(&env.nabir)
            .arg(level)
            .arg("--target-dir")
            .arg(dir.join("target"))
            .arg("-o")
            .arg(&exe)
            .output()
            .unwrap();
        assert!(out.status.success(), "build {level}: {}", text(&out.stderr));
        let out = run_command(&exe)
            .arg(format!("scale={scale}"))
            .output()
            .unwrap();
        assert!(out.status.success(), "run {level}: {}", text(&out.stderr));
        for line in text(&out.stdout).lines() {
            println!("{level} {line}");
            let t: f64 = line
                .rsplit(" (sink")
                .nth(1)
                .and_then(|l| l.rsplit(' ').next())
                .and_then(|t| t.parse().ok())
                .unwrap_or_else(|| panic!("bad probe line {line:?}"));
            if line.starts_with("control ") {
                assert!(
                    t > limit,
                    "{level}: the leaky control shows |t| {t}, not above {limit}: the measurement cannot see a leak"
                );
                continue;
            }
            probes += 1;
            if t > limit {
                leaks.push(format!("{level} {line}"));
            }
        }
    }
    assert!(probes > 0, "timing.nabi printed no probe");
    assert!(
        leaks.is_empty(),
        "|t| above {limit} (timing depends on secret input, or the machine is busy: rerun alone):\n  {}",
        leaks.join("\n  ")
    );
}
