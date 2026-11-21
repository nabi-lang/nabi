//! Shared plumbing of the `nabic` integration tests, and the directives of
//! the `.nabi` cases (`integration.rs`), read from `//` comment lines:
//!   - `// expect-exit: N`      - runtime exit code
//!   - `// expect-stdout: text` - single-line stdout, or an empty value
//!     followed by `// line` comments for a multi-line block (none:
//!     empty stdout); required of every case that runs
//!   - `// expect-error: text`  - build must fail and stderr must contain `text`
//!   - `// forbid-error: text`  - (repeatable) that failed build's stderr must
//!     not contain `text`
//!   - `// expect-warning: text` - (repeatable) build stderr must contain `text`
//!   - `// forbid-warning: text` - (repeatable) build stderr must not contain `text`
//!   - `// expect-stderr: text` - (repeatable) the run's stderr, one
//!     directive per line, leading and trailing blanks aside (`\` read
//!     as `/`, library paths relative to the workspace, `*` any text, as in
//!     `library/std/src/wit.nabi:*:`); none: empty stderr. Under
//!     `NABI_TEST_RUNNER`, a last `qemu: uncaught target signal` line
//!     (qemu-user's) is not compared
//!   - `// expect-lint: L:C message` - (repeatable) `nabic check` on the
//!     case reports exactly these `E0131` lints, in order: line, column
//!     and headline
//!   - `// gc-stress: N`        - run twice more, with
//!     `NABIR_GC_STRESS=N NABIR_GC_EVAC=always` and with
//!     `NABIR_GC_STRESS=N NABIR_GC_EVAC=always NABIR_GC_CONCURRENT=always`,
//!     same expectations
//!   - `// nabic-arg: args`      - extra `nabic` arguments (`--cfg feat`)
//!   - `// run-env: KEY=VALUE`  - (repeatable) set for every run of the
//!     case (`NABIR_WORKERS=3`)
//!   - `// only-os: macos linux` - skipped unless the target's `target_os`
//!     is listed (POSIX commands, paths, signals); `linux` takes Android
//!     too
//!   - `// only-pointer-width: 64` - skipped on targets of another width
//!     (sizes, `usize::BITS`, layouts in the expected output)
//!   - `// only-endian: little` - skipped on targets of the other byte
//!     order (what a case prints of memory's bytes)
//!   - `// skip-target: s390x-unknown-linux-gnu ..` - not run for the
//!     listed `NABI_TEST_TARGET`s (a C library that differs there)
//!   - `// needs: threads net process fs posix-fs os native-run objc dlopen deep-stack` - skipped
//!     on a target without one of them: threads only on `wasm32-wasip1-threads`
//!     among the wasm32 targets, sockets only on `wasm32-wasip2`, no
//!     processes or signals on wasm32 (`process` also covers POSIX C
//!     calls), a browser no file system (`fs`), WASI no `dup` / `flock` /
//!     `mmap` (`posix-fs`), a target without an OS none of `net`
//!     `process` `fs` `posix-fs` `os` (`os`: the clock, time zones,
//!     environment variables, the entropy source); `objc` only on Apple
//!     targets; `dlopen` only where executables are dynamic (not static
//!     musl): `Library::open` / `Library::this()` finding C symbols;
//!     `deep-stack` not in a browser (`NABI_TEST_WASM_BROWSER`), whose
//!     engine stack is far below the 8 MB of a native main thread or of
//!     node's `--stack-size=8000` (a Chromium worker holds about 500
//!     nested awaits at -O0, a `WebKit` one about 100)
//!   - `// skip-wasm-runner: jco ..` - not run when
//!     `NABI_TEST_WASM_RUNNER` is one of the listed (its file name
//!     without extension: `wasmtime`, `jco`, `iwasm`, `wasmer`), with
//!     the runtime's gap or bug in a comment beside it
//!   - `// only-target: wasm32-wasip2 ..` - run only on the listed
//!     `NABI_TEST_TARGET`s (what one target reports that the others do
//!     not, e.g. its `Unsupported` APIs)
//!   - `// wasm-invoke: f(1, "a") => 2` - (repeatable) after the run, the
//!     component's export `f` called through `wasmtime run --invoke`
//!     (WAVE syntax; `jco.sh --invoke` under jco) prints the value after
//!     `=>`
//!   - `// wasm-reactor` - a component without `wasi:cli/run`: not run,
//!     only its `wasm-invoke`s
//!   - `// wasm-plug: dir/main.nabi` - that program (relative to the case
//!     directory) is built too and
//!     `wac plug` (`NABI_TEST_WAC`; the case is skipped without it)
//!     satisfies the case's imports with its exports; the composition
//!     runs
//!   - `// timeout: N`          - each build and run of the case may take
//!     N seconds (default: `NABI_TEST_CASE_TIMEOUT`, else 600 to build and
//!     300 to run); past it the process is killed and the case fails
//!   - `native-run` is skipped when the target runs under CPU emulation
//!     (a foreign arch executed through qemu-user), where some syscalls
//!     (zerocopy, process and signal details) are not faithfully emulated.
//!
//! Environment:
//!   - `NABI_TEST_OPT=-O2` (or `--release`, `-O3`, `-O2 -g`, ...) - build
//!     every case at that level instead of `-O0` (flags split on blanks).
//!   - `NABI_TEST_FILTER=text` - only the integration cases whose name
//!     contains `text`; `a,b` runs those containing any of them; empty
//!     runs every case.
//!   - `NABI_TEST_SHARD=i/n` - only every n-th integration case of the
//!     sorted list from the i-th (1-based), and every n-th differential
//!     suite: CI splits a corpus over n jobs.
//!   - `NABI_TEST_TARGET=<triple>` - build for that target (with a runtime
//!     built for it), e.g. `aarch64-unknown-linux-gnu` run through
//!     `NABI_TEST_RUNNER`.
//!     `wasm32-wasip2` and `wasm32-wasip1-threads` run each module with
//!     `NABI_TEST_WASM_RUNNER`: a `wasmtime` with wasi-threads, or a
//!     wrapper of another runtime from `targets/wasi/` (`jco.sh` for
//!     wasip2; `iwasm.sh`, `wasmer.sh` for wasip1-threads);
//!     `wasm32-unknown-unknown` with `node` and the toolchain's `nabi.mjs`
//!     host, or in a browser (`NABI_TEST_WASM_BROWSER`) (all need
//!     `targets/build.sh <triple>` for the C library).
//!     For a target in progress the harness's nabic runs with
//!     `NABI_IN_PROGRESS_TARGETS=1`.
//!   - `NABI_TEST_WASM_BROWSER=chromium|firefox|webkit` - run the
//!     `wasm32-unknown-unknown` modules in that browser instead of node:
//!     `scripts/test/browser.mjs`, which loads Playwright from the npm
//!     prefix `NABI_TEST_PLAYWRIGHT` (`npm install --prefix <dir>
//!     playwright@<version>`, the version whose browsers are installed).
//!     The test binary starts one browser and a local server; each run is
//!     a `node browser.mjs run` client that hands the module, arguments,
//!     environment and stdin to it and gets back stdout, stderr and the
//!     exit code. The module runs through `nabi.mjs` in a fresh dedicated
//!     Worker of an idle page (one page per concurrent run), fetched as
//!     `application/wasm`; the page is not cross-origin isolated, so the
//!     host sleeps by spinning. A run killed at its time limit terminates
//!     its worker.
//!   - `NABI_TEST_RUNNER=<program> [args]` - run a native target's case
//!     executables through it (split on whitespace), e.g.
//!     `qemu-aarch64 -L <sysroot>` or `wine` where no binfmt handler
//!     is registered, or `targets/android/adb.sh <serial>` for an
//!     Android device or emulator. Its `-L <sysroot>` is also `QEMU_LD_PREFIX` of
//!     the run, so programs a case starts find their loader when binfmt
//!     runs them under qemu.
//!   - `NABI_TEST_CC=<program> [args]` - native.rs's C ABI tests with a
//!     `NABI_TEST_TARGET`: the C compiler for its C side (default
//!     `clang --target=<the target's C triple>`), e.g. with `-isystem`
//!     for a cross sysroot's headers.
//!   - `NABI_TEST_PREBUILT=1` - `libnabir.a` (and for `nabi`'s tests nabic)
//!     is built already, so no test runs `cargo build`: under nextest,
//!     one process per test, a concurrent build relinks the archive
//!     while another test links against it.
//!   - `NABI_TEST_CASE_TIMEOUT=<seconds>` - the limit of every build and
//!     run of a case without `// timeout:` (wasmtime under load).
//!   - `NABI_TEST_TMP=<dir>` - where each test binary's scratch root
//!     `nabi-test-<pid>` goes (default: the temp directory); the root is
//!     removed at exit, unless `NABI_KEEP_TEST_TMP` is set.
//!   - `NABI_TEST_JOBS=<n>` - cases built and run at once (default: one
//!     per core).
#![allow(dead_code)]

use nabic_target::{Linker, ObjectFormat, TargetSpec};
use std::env;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

pub const NABIC: &str = env!("CARGO_BIN_EXE_nabic");

/// The repository root (`compiler/nabic/../..`).
pub fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// A fresh, empty scratch directory for this test binary, removed when
/// it exits (`nabic_core::tmp::test_root`).
pub fn scratch(name: &str) -> PathBuf {
    nabic_core::tmp::test_dir(name)
}

/// The temp directory case programs see: inside the scratch root, so
/// what they leave goes with it.
pub fn case_tmp() -> &'static Path {
    static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| scratch("t"))
}

/// `fs::write`, creating the parent directories first (`app/src/..`).
pub fn put(path: impl AsRef<Path>, data: impl AsRef<[u8]>) -> std::io::Result<()> {
    if let Some(parent) = path.as_ref().parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, data)
}

pub fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

#[track_caller]
pub fn assert_exit(out: &Output, code: i32) {
    assert_eq!(
        out.status.code(),
        Some(code),
        "stdout: {}\nstderr: {}",
        text(&out.stdout),
        text(&out.stderr)
    );
}

pub struct Env {
    pub workspace: PathBuf,
    pub bootstrap: PathBuf,
    pub nabir: PathBuf,
}

/// Locate the workspace and rebuild `libnabir.a`.
pub fn setup() -> Env {
    let workspace = nabic_core::real_path(workspace()).unwrap();
    let bootstrap = workspace.join("library/core/src/lib.nabi");
    assert!(bootstrap.exists(), "missing {bootstrap:?}");

    // `cargo test` doesn't rebuild the staticlib on its own (nabic must not
    // depend on nabir), so build it here to never link a stale runtime.
    // Once per test binary: parallel tests rebuilding it at the same time
    // race on the archive file.
    #[allow(clippy::items_after_statements)]
    static BUILT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let ok = *BUILT.get_or_init(|| {
        if env::var_os("NABI_TEST_PREBUILT").is_some() {
            return true;
        }
        let cargo = env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        Command::new(cargo)
            .args(["build", "-p", "nabir"])
            .args(rust_target().map(|t| format!("--target={t}")))
            .current_dir(&workspace)
            .status()
            .expect("spawn cargo build -p nabir")
            .success()
    });
    assert!(ok, "cargo build -p nabir failed");
    let target =
        env::var_os("CARGO_TARGET_DIR").map_or_else(|| workspace.join("target"), PathBuf::from);
    let nabir = target
        .join(rust_target().unwrap_or_default())
        .join("debug")
        .join(spec().runtime_lib());
    assert!(
        nabir.exists(),
        "missing {nabir:?} after cargo build -p nabir"
    );

    Env {
        workspace,
        bootstrap,
        nabir,
    }
}

/// Where a corpus run keeps its build outputs and programs:
/// `NABI_TEST_TMP` when set, else the workspace's `target/`. On macOS a
/// program run from an external volume asks for removable-volume access
/// each time it is rebuilt, so point it at the internal disk there.
pub fn scratch_root(workspace: &Path) -> PathBuf {
    env::var_os("NABI_TEST_TMP").map_or_else(|| workspace.join("target"), PathBuf::from)
}

/// Scratch-directory suffix, so runs at different `NABI_TEST_OPT` levels
/// don't share build outputs.
pub fn opt_suffix() -> String {
    env::var("NABI_TEST_OPT").unwrap_or_default()
}

/// `NABI_TEST_OPT`'s flags (`-O2 -g`: two).
pub fn opt_flags() -> Vec<String> {
    opt_suffix().split_whitespace().map(str::to_owned).collect()
}

/// `opt_flags()` for the case `source`: without the level when its
/// `nabic-arg:` sets one, so a case pinned to `-O2` (a tail call that
/// needs it) stays there under `NABI_TEST_OPT=-O0`.
fn case_opt_flags(source: &str) -> Vec<String> {
    let level = |a: &str| a.starts_with("-O") || a == "--release";
    let pinned = directives(source, "nabic-arg:")
        .flat_map(str::split_whitespace)
        .any(level);
    opt_flags()
        .into_iter()
        .filter(|a| !(pinned && level(a)))
        .collect()
}

/// Built above `-O0` (`NABI_TEST_OPT`).
pub fn optimized() -> bool {
    !matches!(opt_suffix().as_str(), "" | "-O0")
}

/// What a case's time limit covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Build,
    Run,
}

/// The limit of one build or run of the case `source`: its `// timeout:`,
/// else `NABI_TEST_CASE_TIMEOUT`, else 600 s to build and 300 s to run.
pub fn case_timeout(source: &str, step: Step) -> Duration {
    case_timeout_in(
        env::var("NABI_TEST_CASE_TIMEOUT").ok().as_deref(),
        source,
        step,
    )
}

/// [`case_timeout`] with `var` as `NABI_TEST_CASE_TIMEOUT`.
pub fn case_timeout_in(var: Option<&str>, source: &str, step: Step) -> Duration {
    let secs = |s: &str, what: &str| {
        s.parse::<f64>()
            .ok()
            .filter(|s| s.is_finite() && *s > 0.0)
            .map_or_else(
                || panic!("{what} {s}: expected a positive number of seconds"),
                Duration::from_secs_f64,
            )
    };
    if let Some(d) = directive(source, "timeout:") {
        return secs(d, "// timeout:");
    }
    if let Some(v) = var.map(str::trim).filter(|v| !v.is_empty()) {
        return secs(v, "NABI_TEST_CASE_TIMEOUT=");
    }
    Duration::from_secs(match step {
        Step::Build => 600,
        Step::Run => 300,
    })
}

/// `c.output()`, killed past `limit`: `Err("timed out after Ns")`, on
/// Linux followed by where each thread of the process waited.
pub fn output_within(c: &mut Command, limit: Duration) -> Result<Output, String> {
    let mut child = c
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("exec: {e}"))?;
    let out = drain(child.stdout.take());
    let err = drain(child.stderr.take());
    let deadline = Instant::now() + limit;
    let mut nap = Duration::from_millis(1);
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if Instant::now() < deadline => {
                thread::sleep(nap);
                nap = (nap * 2).min(Duration::from_millis(50));
            }
            Ok(None) => {
                let threads = threads_of(child.id());
                let _ = child.kill();
                let _ = child.wait();
                // The readers are left: a grandchild may still hold the pipes.
                return Err(format!("timed out after {}s{threads}", limit.as_secs_f64()));
            }
            Err(e) => return Err(format!("wait: {e}")),
        }
    };
    Ok(Output {
        status,
        stdout: out.join().unwrap_or_default(),
        stderr: err.join().unwrap_or_default(),
    })
}

/// A hung case's threads, one line each: the name, the kernel function
/// it sleeps in and its system call (number and arguments), from
/// /proc. Empty elsewhere and when /proc tells nothing.
fn threads_of(pid: u32) -> String {
    let Ok(tasks) = std::fs::read_dir(format!("/proc/{pid}/task")) else {
        return String::new();
    };
    let mut lines: Vec<String> = tasks
        .flatten()
        .map(|t| {
            let p = t.path();
            let get = |f: &str| {
                std::fs::read_to_string(p.join(f))
                    .unwrap_or_default()
                    .trim()
                    .to_owned()
            };
            format!(
                "\n    thread {} {}: wchan {}, syscall {}",
                t.file_name().to_string_lossy(),
                get("comm"),
                get("wchan"),
                get("syscall")
            )
        })
        .collect();
    lines.sort();
    lines.concat()
}

/// A thread reading `pipe` to its end.
fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut p) = pipe {
            let _ = p.read_to_end(&mut buf);
        }
        buf
    })
}

/// `nabic build <src> -o <exe>` with the harness bootstrap + runtime,
/// within the case's [`case_timeout`].
pub fn nabic_build(env: &Env, src: &Path, exe: &Path) -> Result<Output, String> {
    let source = std::fs::read_to_string(src).unwrap_or_default();
    let mut c = Command::new(NABIC);
    // From the root, so a case's own locations read `tests/cases/..`.
    c.current_dir(&env.workspace)
        .args(directives(&source, "nabic-arg:").flat_map(str::split_whitespace))
        .arg("build")
        .arg(src)
        .arg("--bootstrap")
        .arg(&env.bootstrap)
        .arg("--bootstrap")
        .arg(env.workspace.join("library/std/src/lib.nabi"))
        .arg("--bootstrap")
        .arg(env.workspace.join("library/bare/src/lib.nabi"))
        .arg("--nabir")
        .arg(&env.nabir)
        .args(case_opt_flags(&source))
        .args(test_target().map(|t| format!("--target={t}")))
        .envs(in_progress_env())
        // Prebuilt `--extern`s go beside the case's output.
        .arg("--target-dir")
        .arg(exe.with_file_name("target"))
        .arg("-o")
        .arg(exe);
    output_within(&mut c, case_timeout(&source, Step::Build))
}

/// `nabic check src`, with the build's library and target.
pub fn nabic_check(env: &Env, src: &Path, dir: &Path) -> std::io::Result<Output> {
    let source = std::fs::read_to_string(src).unwrap_or_default();
    Command::new(NABIC)
        .current_dir(&env.workspace)
        .args(directives(&source, "nabic-arg:").flat_map(str::split_whitespace))
        .arg("check")
        .arg(src)
        .arg("--bootstrap")
        .arg(&env.bootstrap)
        .arg("--bootstrap")
        .arg(env.workspace.join("library/std/src/lib.nabi"))
        .arg("--bootstrap")
        .arg(env.workspace.join("library/bare/src/lib.nabi"))
        .args(test_target().map(|t| format!("--target={t}")))
        .envs(in_progress_env())
        .arg("--target-dir")
        .arg(dir)
        .output()
}

/// `NABI_TEST_TARGET`: the target to build the cases for.
pub fn test_target() -> Option<String> {
    env::var("NABI_TEST_TARGET").ok().filter(|t| !t.is_empty())
}

/// The cases' target (`NABI_TEST_TARGET`, else the host).
pub fn spec() -> TargetSpec {
    TargetSpec::resolve_any(test_target().as_deref()).unwrap()
}

/// `NABI_IN_PROGRESS_TARGETS=1` when the cases' target is in progress:
/// nabic builds it only then.
pub fn in_progress_env() -> Option<(&'static str, &'static str)> {
    test_target()
        .filter(|t| nabic_target::in_progress(t))
        .map(|_| (nabic_target::IN_PROGRESS_VAR, "1"))
}

/// The Rust target the runtime is built for (WASI preview 2 for
/// `wasm32-wasip2`, ...); `None` for the host.
pub fn rust_target() -> Option<String> {
    test_target().map(|_| spec().rust_target)
}

/// The cases are built for wasm32.
pub fn wasm() -> bool {
    spec().object_format == ObjectFormat::Wasm
}

/// The OS the cases are built for, as `target_os` names it.
pub fn target_os() -> String {
    spec().os
}

/// The pointer width of the cases' target, in bits.
pub fn target_pointer_width() -> u32 {
    spec().pointer_width
}

/// The cases run under CPU emulation: a foreign-arch target executed
/// through qemu-user, where some syscalls (zerocopy, process and signal
/// details) are not faithfully emulated. `x86_64` also runs `i686`
/// natively, and an `aarch64` Linux host with `AArch32` (`CONFIG_COMPAT`)
/// `armv7` when nothing stands in for it (see [`aarch32_native`]); every
/// other arch mismatch is emulated. wasm runs under its own engine and
/// is not counted here.
pub fn emulated() -> bool {
    let Some(t) = test_target() else { return false };
    let arch = t.split('-').next().unwrap_or("");
    if arch.starts_with("wasm") {
        return false;
    }
    let host = std::env::consts::ARCH;
    !(arch == host || (host == "x86_64" && arch == "i686") || aarch32_native(host, arch))
}

/// `armv7` executables run on this `aarch64` Linux host's own cores:
/// neither `NABI_TEST_RUNNER` nor a binfmt `qemu-arm` handler is set up,
/// so nothing could run them but the CPU.
fn aarch32_native(host: &str, arch: &str) -> bool {
    host == "aarch64"
        && arch == "armv7"
        && cfg!(target_os = "linux")
        && env::var("NABI_TEST_RUNNER").map_or(true, |r| r.trim().is_empty())
        && !Path::new("/proc/sys/fs/binfmt_misc/qemu-arm").exists()
}

/// A case's `// only-os: <os> ..` excludes the target's OS, its
/// `// only-pointer-width: <bits>` the target's width, its
/// `// only-endian:` the target's byte order, its `// skip-target:` the
/// target, its `// needs:`
/// something the target lacks.
pub fn skipped_on_target(source: &str) -> bool {
    directive(source, "only-os:").is_some_and(|oses| {
        let os = target_os();
        // Android's kernel is Linux: `linux` cases run there too.
        !oses
            .split_whitespace()
            .any(|o| o == os || (o == "linux" && os == "android"))
    }) || directive(source, "only-pointer-width:")
        .is_some_and(|w| w.trim() != target_pointer_width().to_string())
        || directive(source, "only-endian:").is_some_and(|e| e.trim() != spec().endian)
        || directive(source, "skip-target:").is_some_and(|ts| {
            let t = test_target().unwrap_or_default();
            ts.split_whitespace().any(|x| x == t)
        })
        || directive(source, "needs:").is_some_and(|needs| needs.split_whitespace().any(lacks))
        || directive(source, "only-target:").is_some_and(|ts| {
            let t = test_target().unwrap_or_default();
            !ts.split_whitespace().any(|x| x == t)
        })
        || directive(source, "skip-wasm-runner:").is_some_and(|rs| {
            wasm_runner_name().is_some_and(|n| rs.split_whitespace().any(|r| r == n))
        })
}

/// The name of the WASI runner the cases run under (`NABI_TEST_WASM_RUNNER`
/// without directory and extension, `jco` for `targets/wasi/jco.sh`);
/// `None` off WASI or without one.
pub fn wasm_runner_name() -> Option<String> {
    let t = spec();
    if t.object_format != ObjectFormat::Wasm || t.reactor() {
        return None;
    }
    let r = env::var_os("NABI_TEST_WASM_RUNNER").filter(|r| !r.is_empty())?;
    Some(Path::new(&r).file_stem()?.to_string_lossy().into_owned())
}

/// The cases' target lacks `// needs:` capability `need`.
fn lacks(need: &str) -> bool {
    let t = spec();
    // Without an OS: no files, processes or sockets.
    let bare = t.os == "none";
    match need {
        "threads" => !t.threads,
        "process" | "posix-fs" => wasm() || bare,
        // Sockets: WASI preview 2 (a component).
        "net" => bare || (wasm() && t.linker != Linker::WasmComponent),
        "fs" => t.reactor() || bare,
        "os" => bare,
        "native-run" => emulated(),
        "objc" => t.vendor != "apple",
        "dlopen" => !t.dynamic_linking,
        "deep-stack" => env::var("NABI_TEST_WASM_BROWSER").is_ok_and(|b| !b.is_empty()),
        _ => panic!("unknown `// needs: {need}`"),
    }
}

/// A run's exit code `got` is the expected one. A `wasm32-wasip2`
/// component exits through `wasi:cli/exit`, which carries only success
/// or failure (1); wasmtime fails (1) a preview 1 module's exit with a
/// code of 126 or more (a trap, such as `abort`'s, still gives 134).
pub fn exit_matches(got: i32, expected: i32) -> bool {
    let t = spec();
    if t.linker == Linker::WasmComponent {
        got == i32::from(expected != 0)
    } else if wasm() && !t.reactor() {
        got == expected || (expected >= 126 && got == 1)
    } else {
        got == expected
    }
}

/// File name of executable `name` for the cases' target.
pub fn exe_name(name: &str) -> String {
    format!("{name}{}", spec().exe_suffix)
}

/// A command running the built program `exe`: itself, or a wasm module
/// under `NABI_TEST_WASM_RUNNER` ([`wasm_runner`]) or
/// `node` with the toolchain's `nabi.mjs`; a native one under
/// `NABI_TEST_RUNNER` when set. Its temp directory is [`case_tmp`];
/// `PWD` names its working directory, as `nabi run` does (a WASI
/// program starts there).
pub fn run_command(exe: &Path) -> Command {
    let mut c = runner_command(exe);
    for v in ["TMPDIR", "TMP", "TEMP"] {
        c.env(v, case_tmp());
    }
    if let Ok(cwd) = env::current_dir() {
        c.env("PWD", cwd);
    }
    c
}

// `-C cache=n`: every case is a fresh module, so wasmtime's code cache
// only grows.
fn runner_command(exe: &Path) -> Command {
    let t = spec();
    match t.object_format {
        ObjectFormat::Wasm if t.linker == Linker::WasmComponent => {
            let (runner, wasmtime) = wasm_runner(&t.name);
            let mut c = Command::new(runner);
            if wasmtime {
                c.args([
                    "run",
                    "-C",
                    "cache=n",
                    "-S",
                    "inherit-env",
                    "-S",
                    "inherit-network",
                    "-S",
                    "allow-ip-name-lookup",
                    "-W",
                    "max-wasm-stack=8388608",
                    "-W",
                    COMPONENT_FEATURES,
                    "--dir",
                    "/::/",
                ]);
            }
            c.arg(exe);
            c
        }
        ObjectFormat::Wasm if !t.reactor() => {
            let (runner, wasmtime) = wasm_runner(&t.name);
            let mut c = Command::new(runner);
            if wasmtime {
                c.args([
                    "run",
                    "-C",
                    "cache=n",
                    "-W",
                    "threads=y,shared-memory=y",
                    "-S",
                    "threads=y",
                    "-S",
                    "inherit-env",
                    "-W",
                    "max-wasm-stack=8388608",
                    "--dir",
                    "/::/",
                ]);
            }
            c.arg(exe);
            c
        }
        ObjectFormat::Wasm => {
            let mut c = Command::new("node");
            if let Some(url) = browser() {
                c.arg(workspace().join("scripts/test/browser.mjs"))
                    .args(["run", url]);
            } else {
                c.arg("--stack-size=8000")
                    .arg(workspace().join("targets/nabi.mjs"));
            }
            c.arg(exe);
            c
        }
        _ => match env::var("NABI_TEST_RUNNER")
            .ok()
            .filter(|r| !r.trim().is_empty())
        {
            Some(runner) => {
                let words: Vec<&str> = runner.split_whitespace().collect();
                let mut c = Command::new(words[0]);
                c.args(&words[1..]).arg(exe);
                // qemu-user's `-L <sysroot>`: also for the programs the
                // case starts, which binfmt runs under a qemu of its own.
                let at = words.iter().position(|w| *w == "-L");
                if let Some(root) = at.and_then(|at| words.get(at + 1)) {
                    c.env("QEMU_LD_PREFIX", root);
                }
                c
            }
            None => Command::new(exe),
        },
    }
}

/// `NABI_TEST_WASM_BROWSER`: the URL of the `scripts/test/browser.mjs`
/// server this test binary started, once, for that browser. The server
/// reads its stdin, which this process holds open: it closes the browser
/// when the process exits. `None` when unset (node runs the modules).
fn browser() -> Option<&'static str> {
    static URL: std::sync::OnceLock<Option<(Child, String)>> = std::sync::OnceLock::new();
    let got = URL.get_or_init(|| {
        let name = env::var("NABI_TEST_WASM_BROWSER")
            .ok()
            .filter(|b| !b.is_empty())?;
        let mut child = Command::new("node")
            .arg(workspace().join("scripts/test/browser.mjs"))
            .args(["serve", &name])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn node scripts/test/browser.mjs serve");
        let mut url = String::new();
        let out = child.stdout.as_mut().expect("piped stdout");
        BufReader::new(out).read_line(&mut url).unwrap_or_default();
        let url = url.trim().to_owned();
        assert!(
            url.starts_with("http://"),
            "NABI_TEST_WASM_BROWSER={name}: browser.mjs serve did not start"
        );
        Some((child, url))
    });
    got.as_ref().map(|(_, url)| url.as_str())
}

/// `wasmtime run --invoke <call> <exe>`: export `call` of the component
/// `exe` (`NABI_TEST_WASM_RUNNER`), without running `main`; a wrapper
/// gets `--invoke <call> <exe>`.
pub fn invoke_command(exe: &Path, call: &str) -> Command {
    let (runner, wasmtime) = wasm_runner("`// wasm-invoke:`");
    let mut c = Command::new(runner);
    if wasmtime {
        c.args(["run", "-C", "cache=n", "-W", COMPONENT_FEATURES]);
    }
    c.args(["--invoke", call]).arg(exe);
    c
}

/// `NABI_TEST_WASM_RUNNER`, and whether it is wasmtime (by file name),
/// which gets its own flags. Anything else is a wrapper in wasmtime's
/// place (`targets/wasi/`) run as `<runner> <module>`, which grants the
/// module the host's environment, file system and network and an engine
/// stack as deep as a native main thread's.
pub fn wasm_runner(what: &str) -> (std::ffi::OsString, bool) {
    let runner = env::var_os("NABI_TEST_WASM_RUNNER")
        .filter(|r| !r.is_empty())
        .unwrap_or_else(|| {
            panic!("{what} needs NABI_TEST_WASM_RUNNER (wasmtime or a targets/wasi/ wrapper)")
        });
    let wasmtime = Path::new(&runner)
        .file_stem()
        .is_some_and(|n| n == "wasmtime");
    (runner, wasmtime)
}

/// The component model features `wasm32-wasip2` components may use beyond
/// what wasmtime enables by default (async is on).
const COMPONENT_FEATURES: &str =
    "component-model-fixed-length-lists=y,component-model-error-context=y";

/// The `NABI_TEST_SHARD=i/n` part of `items` (all when unset).
pub fn shard<T>(items: Vec<T>) -> Vec<T> {
    match env::var("NABI_TEST_SHARD") {
        Ok(spec) if !spec.is_empty() => shard_of(&spec, items),
        _ => items,
    }
}

/// Shard `spec` (`i/n`, 1 <= i <= n) of `items`: indices `i-1, i-1+n, ...`.
pub fn shard_of<T>(spec: &str, items: Vec<T>) -> Vec<T> {
    let (i, n) = spec
        .split_once('/')
        .and_then(|(i, n)| Some((i.parse::<usize>().ok()?, n.parse::<usize>().ok()?)))
        .filter(|&(i, n)| (1..=n).contains(&i))
        .unwrap_or_else(|| panic!("NABI_TEST_SHARD={spec}: expected i/n with 1 <= i <= n"));
    items.into_iter().skip(i - 1).step_by(n).collect()
}

/// Items running in every `par_map` call at once: the tests of the one
/// binary run side by side, and each call alone fills all cores.
static BUSY: Mutex<usize> = Mutex::new(0);
static FREED: Condvar = Condvar::new();

/// One of `max` places to run a `par_map` item, given back on drop.
struct Slot;

impl Slot {
    fn take(max: usize) -> Self {
        let mut busy = BUSY.lock().unwrap_or_else(PoisonError::into_inner);
        while *busy >= max {
            busy = FREED.wait(busy).unwrap_or_else(PoisonError::into_inner);
        }
        *busy += 1;
        Self
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        *BUSY.lock().unwrap_or_else(PoisonError::into_inner) -= 1;
        FREED.notify_one();
    }
}

/// Run `f` over `items` on `NABI_TEST_JOBS` workers (default: all cores),
/// shared with the other `par_map` calls running; results keep input order.
pub fn par_map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let workers = env::var("NABI_TEST_JOBS")
        .ok()
        .and_then(|n| n.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or_else(|| thread::available_parallelism().map_or(4, std::num::NonZero::get));
    let next = AtomicUsize::new(0);
    let mut out: Vec<(usize, R)> = thread::scope(|s| {
        // Collect first so every worker is spawned before any join.
        #[allow(clippy::needless_collect)]
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                s.spawn(|| {
                    let mut local = Vec::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(item) = items.get(i) else { break };
                        let _slot = Slot::take(workers);
                        local.push((i, f(item)));
                    }
                    local
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect()
    });
    out.sort_by_key(|(i, _)| *i);
    out.into_iter().map(|(_, r)| r).collect()
}

/// Values after `// <key>` on every matching line.
pub fn directives<'a>(source: &'a str, key: &'a str) -> impl Iterator<Item = &'a str> {
    source
        .lines()
        .filter_map(move |l| l.trim_start().strip_prefix("// ")?.strip_prefix(key))
        .map(str::trim)
}

/// Value after `// <key>` on the first matching line.
pub fn directive<'a>(source: &'a str, key: &str) -> Option<&'a str> {
    source
        .lines()
        .find_map(|l| l.trim_start().strip_prefix("// ")?.strip_prefix(key))
        .map(str::trim)
}

pub fn parse_expect_exit(source: &str) -> Option<i32> {
    directive(source, "expect-exit:")?.parse().ok()
}

pub fn parse_expect_error(source: &str) -> Option<&str> {
    directive(source, "expect-error:")
}

/// `None` = no stdout check. Multi-line form: `// expect-stdout:` followed by
/// `// <line>` / `//` comment lines.
pub fn parse_expect_stdout(source: &str) -> Option<String> {
    let mut lines = source.lines();
    while let Some(line) = lines.next() {
        let Some(rest) = line.trim_start().strip_prefix("// expect-stdout:") else {
            continue;
        };
        let rest = rest.trim();
        if !rest.is_empty() {
            return Some(rest.to_owned());
        }
        let mut out = String::new();
        for l in lines {
            let t = l.trim_start();
            if let Some(content) = t.strip_prefix("// ") {
                out.push_str(content);
            } else if t != "//" {
                break;
            }
            out.push('\n');
        }
        return Some(out);
    }
    None
}

/// `stderr` plus one `path:line:col: message` line per rendered
/// diagnostic (`error[E..]: message` over `--> path:line:col`), so an
/// expectation can pin a message to its location.
pub fn flatten_diagnostics(stderr: &str) -> String {
    let mut out = stderr.to_owned();
    let lines: Vec<&str> = stderr.lines().collect();
    for w in lines.windows(2) {
        let msg = w[0].split_once("]: ").map(|(_, m)| m);
        let loc = w[1].trim_start().strip_prefix("--> ");
        if let (Some(msg), Some(loc)) = (msg, loc) {
            out.push_str(&format!("\n{loc}: {msg}"));
        }
    }
    out
}

/// Check a failed build's stderr against every `// expect-error:` and
/// `// forbid-error:`: each expected text is present, and each
/// `error[..]` headline (or its `path:line:col: message` form) contains
/// one of them. `// expect-error-extra: allow` lifts the second rule,
/// for a case whose further errors are legitimate (it says which).
pub fn check_expect_error(source: &str, stderr: &str) -> Result<(), String> {
    let flat = &flatten_diagnostics(stderr);
    if let Some(unwanted) = directives(source, "forbid-error:").find(|w| flat.contains(w)) {
        return Err(format!("build stderr has error {unwanted:?}"));
    }
    let wants: Vec<&str> = directives(source, "expect-error:").collect();
    if let Some(want) = wants.iter().find(|want| !flat.contains(*want)) {
        return Err(format!(
            "build failed but stderr lacks expect-error {want:?}\n    stderr: {:?}",
            flat.lines()
                .find(|l| l.starts_with("error"))
                .unwrap_or_else(|| flat.trim())
        ));
    }
    if directive(source, "expect-error-extra:") == Some("allow") {
        return Ok(());
    }
    let lines: Vec<&str> = stderr.lines().collect();
    for (i, l) in lines.iter().enumerate() {
        if !l.starts_with("error[") {
            continue;
        }
        let msg = l.split_once("]: ").map_or("", |(_, m)| m);
        let at = lines
            .get(i + 1)
            .and_then(|n| n.trim_start().strip_prefix("--> "))
            .map(|loc| format!("{loc}: {msg}"))
            .unwrap_or_default();
        if !wants.iter().any(|w| l.contains(w) || at.contains(w)) {
            return Err(format!("build stderr has unexpected error {l:?} ({at})"));
        }
    }
    Ok(())
}

/// Deterministic PRNG (splitmix64) for the fuzz and differential tests.
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Uniform in `0..n` (`n > 0`).
    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    /// True with probability `pct`%.
    pub fn chance(&mut self, pct: u64) -> bool {
        self.next() % 100 < pct
    }

    pub fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len())]
    }

    /// Uniform in `lo..=hi`.
    pub fn range(&mut self, lo: i64, hi: i64) -> i64 {
        let span = hi.wrapping_sub(lo) as u64;
        let r = if span == u64::MAX {
            self.next()
        } else {
            self.next() % (span + 1)
        };
        lo.wrapping_add(r as i64)
    }
}
