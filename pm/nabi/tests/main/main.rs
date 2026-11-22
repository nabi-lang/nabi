//! `nabi` end to end: scratch `NABI_HOME`s, local git repositories made
//! with the `git` CLI, and the `nabic` built next to `nabi`.

// `HEAD^{tree}` is git revision syntax, not a format argument.
#![allow(clippy::literal_string_with_formatting_args)]

mod audit;
mod commands;
mod deps;
mod envs;
mod features;
#[cfg(unix)]
mod install;
mod manifest;
#[cfg(unix)]
mod native;
#[cfg(unix)]
mod official;
mod packages;
mod registry;
mod scripts;
mod system;
mod toolchain;
mod tools;
mod workspace;

pub(crate) use crate::commands::*;
pub(crate) use crate::deps::*;
pub(crate) use crate::envs::*;
pub(crate) use crate::registry::*;
#[cfg(unix)]
pub(crate) use crate::toolchain::*;

use nabic_diagnostic::Json;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex, OnceLock};

const NABI: &str = env!("CARGO_BIN_EXE_nabi");

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// `nabi` runs the `nabic` beside it (and tests that link want the
/// runtime, C sources nabicc, and `nabils` runs `nabi`): build them once, unless
/// `NABI_TEST_PREBUILT` says they are
/// (nabic's tests/main/common.rs).
fn build_nabic() {
    static BUILT: OnceLock<bool> = OnceLock::new();
    let ok = *BUILT.get_or_init(|| {
        if std::env::var_os("NABI_TEST_PREBUILT").is_some() {
            return true;
        }
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        Command::new(cargo)
            .args([
                "build", "-p", "nabic", "-p", "nabir", "-p", "nabicc", "-p", "nabils",
            ])
            .current_dir(workspace())
            .status()
            .unwrap()
            .success()
    });
    assert!(
        ok,
        "cargo build -p nabic -p nabir -p nabicc -p nabils failed"
    );
}

/// The executable file `name` builds to.
fn exe(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

/// `s` as a NABION string literal (Windows paths have backslashes).
fn q(s: impl std::fmt::Display) -> String {
    nabic_nabion::quote(&s.to_string())
}

fn scratch(name: &str) -> PathBuf {
    nabic_core::tmp::test_dir(name)
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn nabi_cmd(home: &Path, cwd: &Path, args: &[&str]) -> Command {
    build_nabic();
    let mut c = Command::new(NABI);
    c.current_dir(cwd)
        .env("NABI_HOME", home)
        .env_remove("NABI_ENV")
        .env_remove("NABI_REGISTRY_TOKEN")
        .env_remove("CLICOLOR_FORCE")
        // A CI runner sets `CI`, which stops official packages coming in on use.
        .env_remove("CI")
        .envs(
            tools::test_target()
                .filter(|t| nabic_target::in_progress(t))
                .map(|_| (nabic_target::IN_PROGRESS_VAR, "1")),
        )
        .args(args);
    if cfg!(unix) {
        user_env(&mut c, &home.join("user"));
    }
    proxies_first(&mut c, home);
    // Registry packages' https `origin` (see `origin`) fetches from the scratch tree.
    let root = nabic_core::tmp::test_root().display().to_string();
    c.env("GIT_CONFIG_COUNT", "1")
        .env(
            "GIT_CONFIG_KEY_0",
            format!("url.{}/.insteadOf", root.replace('\\', "/")),
        )
        .env("GIT_CONFIG_VALUE_0", ORIGIN);
    c
}

/// `home`'s proxies first on PATH, as install.sh puts them: no warning
/// about another `nabi` there.
fn proxies_first(c: &mut Command, home: &Path) {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let dirs = std::iter::once(home.join("bin")).chain(std::env::split_paths(&path));
    c.env("PATH", std::env::join_paths(dirs).unwrap());
}

/// A user whose home is `user`, with no XDG directories set: the
/// completion files `nabi toolchain` refreshes stay in the test tree.
fn user_env(c: &mut Command, user: &Path) {
    c.env("HOME", user)
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_DATA_HOME");
}

/// Where `origin` points registry packages; `nabi_cmd` maps it to the test root.
const ORIGIN: &str = "https://example.invalid/";

/// The https URL `nabi publish` takes for the repository at `dir`.
fn origin(dir: &Path) -> String {
    let rel = dir.strip_prefix(nabic_core::tmp::test_root()).unwrap();
    format!("{ORIGIN}{}", rel.display().to_string().replace('\\', "/"))
}

fn nabi(home: &Path, cwd: &Path, args: &[&str]) -> Output {
    nabi_cmd(home, cwd, args).output().unwrap()
}

#[track_caller]
fn ok(out: &Output) -> String {
    assert_eq!(
        out.status.code(),
        Some(0),
        "stdout: {}\nstderr: {}",
        text(&out.stdout),
        text(&out.stderr)
    );
    text(&out.stdout)
}

/// This nabi's version without a pre-release, the `nabi_version` `nabi new`
/// writes.
fn nabi_version() -> String {
    env!("CARGO_PKG_VERSION")
        .split('-')
        .next()
        .unwrap_or_default()
        .to_owned()
}

/// This nabi's series (`0.minor`, else `major`), the pin `nabi new` writes.
fn series() -> String {
    let v: Vec<&str> = env!("CARGO_PKG_VERSION").split('.').collect();
    if v[0] == "0" {
        format!("0.{}", v[1])
    } else {
        v[0].to_owned()
    }
}

#[track_caller]
fn fails(out: &Output, want: &str) {
    let err = text(&out.stderr);
    assert_ne!(out.status.code(), Some(0), "unexpected success: {err}");
    assert!(err.contains(want), "stderr lacks {want:?}:\n{err}");
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .env_remove("GIT_DIR")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", text(&out.stderr));
    text(&out.stdout).trim().to_owned()
}

/// A git repository at `dir` holding `files`; the new commit.
fn commit(dir: &Path, files: &[(&str, &str)]) -> String {
    if !dir.join(".git").exists() {
        fs::create_dir_all(dir).unwrap();
        git(dir, &["init", "-q", "-b", "main"]);
    }
    for (name, body) in files {
        write(&dir.join(name), body);
    }
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "c"]);
    git(dir, &["rev-parse", "HEAD"])
}

/// `name=path` lines of `nabi deps`.
fn deps(home: &Path, cwd: &Path, extra: &[&str]) -> Vec<(String, String)> {
    let mut args = extra.to_vec();
    args.push("deps");
    ok(&nabi(home, cwd, &args))
        .lines()
        .map(|l| {
            let (n, p) = l.split_once('=').unwrap();
            (n.to_owned(), p.to_owned())
        })
        .collect()
}
