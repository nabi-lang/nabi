//! The tools `nabi` runs itself, through the binaries: `nabi repl` fed by
//! a pipe, `nabils` / `nabi lsp` framing, the Jupyter kernel, `nabi fmt`,
//! `nabi doc`, `nabi bindgen` and the rest. (The language server's queries
//! are tested in `nabic_tools`.)

mod bindgen;
mod dev;
mod doc;
mod fix;
mod fmt;
mod jupyter;
mod lsp;
mod output;
mod repl;
mod run;
mod session;
mod terminal;

pub(crate) use crate::tools::terminal::*;

use crate::{NABI, build_nabic, exe as exe_name, scratch, text};
use nabic_interface::Json;
use nabirepl_jupyter::{encode, handshake, parse_msg, read_msg, write_msg};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
#[cfg(unix)]
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

/// The runtime built beside `nabi`.
pub(crate) struct Env {
    pub(crate) nabir: PathBuf,
}

/// Build `nabic` and the runtime beside `nabi` (`build_nabic`).
pub(crate) fn setup() -> Env {
    build_nabic();
    let nabir = Path::new(NABI).with_file_name(nabic_target::TargetSpec::host().runtime_lib());
    assert!(nabir.exists(), "missing {}", nabir.display());
    Env { nabir }
}

/// The `NABI_HOME` of the tools' runs: scratch, its environments empty.
pub(crate) fn tool_home() -> &'static Path {
    static HOME: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    HOME.get_or_init(|| scratch("tool-home"))
}

/// `nabi <args>` in `cwd`, with `--nabir` (before any `--`) when `env`
/// is given.
pub(crate) fn nabi_in(env: Option<&Env>, cwd: &Path, args: &[&str]) -> Output {
    build_nabic();
    let at = args.iter().position(|a| *a == "--").unwrap_or(args.len());
    let mut c = Command::new(NABI);
    c.current_dir(cwd)
        .env("NABI_HOME", tool_home())
        .env_remove("NABI_ENV")
        .env_remove("CLICOLOR_FORCE")
        .args(&args[..at]);
    if let Some(env) = env {
        c.arg("--nabir").arg(&env.nabir);
    }
    c.args(&args[at..]).output().unwrap()
}

/// `NABI_TEST_TARGET`: the target the compiler tests build for.
pub(crate) fn test_target() -> Option<String> {
    std::env::var("NABI_TEST_TARGET")
        .ok()
        .filter(|t| !t.is_empty())
}

#[track_caller]
pub(crate) fn assert_exit(out: &Output, code: i32) {
    assert_eq!(
        out.status.code(),
        Some(code),
        "stdout: {}\nstderr: {}",
        text(&out.stdout),
        text(&out.stderr)
    );
}

/// `fs::write`, creating the parent directories first.
pub(crate) fn put(path: impl AsRef<Path>, data: impl AsRef<[u8]>) -> std::io::Result<()> {
    if let Some(parent) = path.as_ref().parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, data)
}

/// `exe args` with `input` on stdin.
fn piped(exe: &str, args: &[&str], input: &str) -> Output {
    piped_env(exe, args, input, &[])
}

/// `piped` with environment variables `env`.
fn piped_env(exe: &str, args: &[&str], input: &str, env: &[(&str, &str)]) -> Output {
    build_nabic();
    let mut child = Command::new(exe)
        .args(args)
        .env("NABI_HOME", tool_home())
        .env_remove("NABI_ENV")
        .envs(env.iter().copied())
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
}
