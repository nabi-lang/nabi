//! The REPL host's `main` (`host.rs`) as an object, linked with
//! the runtime (which holds the host) when a session starts; empty where
//! the host can't run (`REPL_HOST`). `$RUSTC` builds it for the target
//! alone: it refers to nothing but the runtime's entry.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=host.rs");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("repl_host.o");
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let host = matches!(
        (os.as_str(), arch.as_str()),
        ("macos", "aarch64") | ("linux" | "windows", "aarch64" | "x86_64")
    );
    if !host {
        std::fs::write(&out, []).unwrap();
        return;
    }
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let target = std::env::var("TARGET").unwrap();
    let status = Command::new(&rustc)
        .args([
            "--crate-type=lib",
            "--crate-name=nabi_repl_main",
            "--emit=obj",
            "--edition=2024",
            "-Copt-level=2",
            "-Cpanic=abort",
            "-Ccodegen-units=1",
            "-Cdebuginfo=0",
            "--target",
            &target,
            "host.rs",
            "-o",
        ])
        .arg(&out)
        .status()
        .unwrap_or_else(|e| panic!("cannot run {}: {e}", rustc.to_string_lossy()));
    assert!(status.success(), "compiling host.rs failed");
}
