// `NABI_COMMIT_INFO` (` (<commit> <date>)` from git, else
// `NABI_COMMIT_HASH` / `NABI_COMMIT_DATE`, else empty) and `NABI_HOST` for
// `--version`. `nabi` includes this file as its build script too.

use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    let s = String::from_utf8(out.stdout).ok()?;
    (out.status.success() && !s.trim().is_empty()).then(|| s.trim().to_owned())
}

fn main() {
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    println!("cargo:rerun-if-env-changed=NABI_COMMIT_HASH");
    println!("cargo:rerun-if-env-changed=NABI_COMMIT_DATE");
    println!("cargo:rerun-if-changed=build.rs");
    // Rerun when HEAD or the branch it names moves (watching a missing
    // file would rerun every build).
    let branch = git(&["symbolic-ref", "-q", "HEAD"]);
    for p in ["HEAD", "packed-refs"].into_iter().chain(branch.as_deref()) {
        if let Some(f) = git(&["rev-parse", "--git-path", p]) {
            if std::path::Path::new(&f).exists() {
                println!("cargo:rerun-if-changed={f}");
            }
        }
    }
    let hash = env("NABI_COMMIT_HASH").or_else(|| git(&["rev-parse", "--short=9", "HEAD"]));
    let date = env("NABI_COMMIT_DATE").or_else(|| git(&["log", "-1", "--format=%cs"]));
    let info = match (hash, date) {
        (Some(h), Some(d)) => format!(" ({h} {d})"),
        (Some(h), None) => format!(" ({h})"),
        _ => String::new(),
    };
    println!("cargo:rustc-env=NABI_COMMIT_INFO={info}");
    // musl threads get 128 KiB of stack; the compiler's worker threads
    // recurse deeper. PT_GNU_STACK sets musl's default.
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("musl") {
        println!("cargo:rustc-link-arg=-Wl,-z,stack-size=8388608");
    }
    println!(
        "cargo:rustc-env=NABI_HOST={}",
        std::env::var("TARGET").unwrap_or_default()
    );
}
