//! Toolchains: self-contained `$NABI_HOME/toolchains/<version>-<triple>/`.
//!
//! `$NABI_HOME/bin/{nabi,nabic,nabils}` are proxies that run the active
//! toolchain's tool of the same name, picked by `+<version>`, else
//! `NABI_TOOLCHAIN`, else the nearest `nabi.nabion`'s `toolchain`, else
//! `nabi toolchain default`. The proxies are copies of the newest installed
//! toolchain's `nabi`, and each toolchain's `nabi` drives only its own
//! `nabic` (`handoff`).

pub use nabi_manifest::pin::{PIN_FORMS, ToolchainReq, admits, builds_with, series};

use crate::{VERSION, home, manifest, sign};
use nabi_manifest::pin::pin_matches;
use nabic_core::term::{self, Role, Stream};
use std::ffi::OsString;
use std::io::Read;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The triple this `nabi` runs on.
pub const HOST: &str = env!("NABI_HOST");

const TOOLS: [&str; 4] = ["nabi", "nabic", "nabicc", "nabils"];

fn exe_name(tool: &str) -> String {
    format!("{tool}{}", std::env::consts::EXE_SUFFIX)
}

fn root(home: &Path) -> PathBuf {
    home.join("toolchains")
}

/// Installed toolchains for this host, `(dir name, version)`, oldest first.
fn installed(home: &Path) -> Vec<(String, String)> {
    let suffix = format!("-{HOST}");
    let mut v: Vec<(String, String)> = std::fs::read_dir(root(home))
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.path().is_dir())
                .filter_map(|e| e.file_name().into_string().ok())
                .filter_map(|n| Some((n.clone(), n.strip_suffix(&suffix)?.to_owned())))
                .collect()
        })
        .unwrap_or_default();
    // Semver order: 0.10 after 0.9, 0.1.0-rc.1 before 0.1.0.
    v.sort_by_key(|(_, ver)| crate::semver::Version::parse(ver).ok());
    v
}

/// The versions of the installed toolchains, oldest first.
pub fn versions(home: &Path) -> Vec<String> {
    installed(home).into_iter().map(|(_, v)| v).collect()
}

/// `v` is a release, not a prerelease.
fn is_release(v: &str) -> bool {
    crate::semver::Version::parse(v).is_ok_and(|v| v.pre.is_empty())
}

/// The newest installed toolchain matching `pin` (`ToolchainReq`, or a
/// directory name): the newest release, a prerelease only when no
/// release matches (`0.1` takes `0.1.0` over `0.1.1-rc.1`).
fn find(home: &Path, pin: &str) -> Option<PathBuf> {
    let hits: Vec<(String, String)> = installed(home)
        .into_iter()
        .filter(|(name, ver)| name == pin || pin_matches(pin, ver))
        .collect();
    hits.iter()
        .rev()
        .find(|(_, v)| is_release(v))
        .or_else(|| hits.last())
        .map(|(name, _)| root(home).join(name))
}

fn not_installed(pin: &str, why: &str) -> String {
    let Ok(req) = ToolchainReq::parse(pin) else {
        return format!(
            "toolchain {pin} ({why}) is not installed, and `{pin}` is not a toolchain pin; \
             {PIN_FORMS}"
        );
    };
    // A directory name installs as its version.
    let arg = match &req {
        ToolchainReq::Prefix(p) => p.strip_suffix(&format!("-{HOST}")).unwrap_or(p),
        ToolchainReq::Req(..) => pin,
    };
    // A download takes a full version only.
    if crate::semver::Version::parse(arg).is_ok() {
        return format!(
            "toolchain {pin} ({why}) is not installed; install it with `nabi toolchain install \
             {arg}` (or add `--from <dir | .tar.gz | .zip | url>`)"
        );
    }
    format!(
        "toolchain {pin} ({why}) is not installed; install a release it accepts by its full \
         version, `nabi toolchain install <version>` (or `nabi toolchain install {arg} --from <dir \
         | .tar.gz | .zip | url>`)"
    )
}

/// The nearest `nabi.nabion` and its `toolchain` (unparsable manifests pin
/// nothing).
fn manifest_pin() -> Option<(PathBuf, String)> {
    let cwd = std::env::current_dir().ok()?;
    let path = cwd.ancestors().find_map(manifest::find)?;
    let pin = manifest::toolchain_pin(&std::fs::read_to_string(&path).ok()?)?;
    Some((path, pin))
}

/// `toolchain` of `$NABI_HOME/config.nabion`.
fn default_pin(home: &Path) -> Result<Option<String>, String> {
    crate::config(home, "toolchain")
}

/// An installed toolchain picked for a run, and why it is the one.
pub struct Selection {
    pub dir: PathBuf,
    pub why: String,
}

/// `+<version>`, else `NABI_TOOLCHAIN`, else `pin` (a manifest's): the pin
/// and why, or `None` when nothing names one.
fn explicit(plus: Option<&str>, pin: Option<(String, String)>) -> Option<(String, String)> {
    let env = std::env::var("NABI_TOOLCHAIN")
        .ok()
        .filter(|t| !t.is_empty());
    if let Some(p) = plus {
        Some((p.to_owned(), "`+` on the command line".to_owned()))
    } else if let Some(p) = env {
        Some((p, "NABI_TOOLCHAIN".to_owned()))
    } else {
        pin
    }
}

/// The nearest manifest's pin, as `explicit` takes it.
pub fn project_pin() -> Option<(String, String)> {
    manifest_pin().map(|(path, p)| (p, format!("pinned by {}", path.display())))
}

/// The `toolchain` in script `file`'s `//! ```nabion` block, as `explicit`
/// takes it. Only the first 64 KiB are read; a script inside a package
/// pins nothing (the package's manifest does).
pub fn script_pin(file: &Path) -> Option<(String, String)> {
    if crate::package_dir(file).is_some() {
        return None;
    }
    let mut text = Vec::new();
    std::fs::File::open(file)
        .ok()?
        .take(64 * 1024)
        .read_to_end(&mut text)
        .ok()?;
    let block = manifest::script_block(&String::from_utf8_lossy(&text))?;
    let pin = manifest::toolchain_pin(&block.text)?;
    Some((pin, format!("pinned by {}", file.display())))
}

/// The script a `nabi` command line works on: `nabi <file>`, or `run`,
/// `build`, `check` or `test` with a `.nabi` file.
fn script_arg(args: &[OsString]) -> Option<PathBuf> {
    let mut words = args
        .iter()
        .map_while(|a| a.to_str())
        .take_while(|w| *w != "--");
    let mut after_cmd = false;
    while let Some(w) = words.next() {
        if crate::VALUE_OPTIONS.contains(&w) {
            words.next();
            continue;
        }
        if w.starts_with('-') {
            continue;
        }
        if nabic_core::cli::looks_like_script(w) {
            return Some(PathBuf::from(w)).filter(|p| p.is_file());
        }
        if after_cmd || !matches!(w, "run" | "build" | "check" | "test") {
            return None;
        }
        after_cmd = true;
    }
    None
}

/// `+<version>`, else `NABI_TOOLCHAIN`, else `script`'s pin, else the
/// manifest's, else the default.
pub fn select(home: &Path, plus: Option<&str>, script: Option<&Path>) -> Result<Selection, String> {
    let pin = script.and_then(script_pin).or_else(project_pin);
    let (pin, why) = match explicit(plus, pin) {
        Some(p) => p,
        None => match default_pin(home)? {
            Some(p) => (p, "the default".to_owned()),
            None => {
                return Err(
                    "no default toolchain; install one (`nabi toolchain install`) or choose one \
                     with `nabi toolchain default <version>`"
                        .to_owned(),
                );
            }
        },
    };
    let dir = find(home, &pin).ok_or_else(|| not_installed(&pin, &why))?;
    Ok(Selection { dir, why })
}

/// The newest installed toolchain for this host with a `bin/nabi`; a
/// prerelease only when no release is installed.
pub fn newest_installed(home: &Path) -> Option<PathBuf> {
    let all: Vec<(String, String)> = installed(home)
        .into_iter()
        .filter(|(name, _)| {
            root(home)
                .join(name)
                .join("bin")
                .join(exe_name("nabi"))
                .is_file()
        })
        .collect();
    all.iter()
        .rev()
        .find(|(_, v)| is_release(v))
        .or_else(|| all.last())
        .map(|(name, _)| root(home).join(name))
}

/// The first `tool` on `PATH`, as the shell finds it (on Windows, with
/// each `PATHEXT` extension).
fn first_on_path(tool: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let names: Vec<String> = if cfg!(windows) {
        let exts = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_owned());
        exts.split(';')
            .filter(|e| !e.is_empty())
            .map(|e| format!("{tool}{}", e.to_ascii_lowercase()))
            .collect()
    } else {
        vec![tool.to_owned()]
    };
    std::env::split_paths(&path)
        .filter(|d| !d.as_os_str().is_empty())
        .flat_map(|d| names.iter().map(move |n| d.join(n)))
        .find(|p| runnable(p))
}

#[cfg(unix)]
fn runnable(p: &Path) -> bool {
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn runnable(p: &Path) -> bool {
    p.is_file()
}

/// Warn when the shell's `nabi` is not `home`'s proxy: an update or a new
/// default would not take. Not with `-q`, as the installers run it.
fn path_warning(home: &Path, quiet: bool) {
    if quiet {
        return;
    }
    let ours = home.join("bin").join(exe_name("nabi"));
    let Some(first) = first_on_path("nabi") else {
        return;
    };
    let real = |p: &Path| nabic_core::real_path(p).unwrap_or_else(|_| p.to_path_buf());
    if real(&first) == real(&ours) {
        return;
    }
    let version = version_of(&first).map_or_else(String::new, |v| format!(" (nabi {v})"));
    let bin = shown(&home.join("bin"));
    eprintln!(
        "nabi: {} your shell runs {}{version} before {}; put {bin} first on PATH or remove the \
         other one",
        term::label(Stream::Stderr, Role::Warning),
        first.display(),
        shown(&ours),
    );
}

/// `<exe> --version`'s version, given a few seconds.
fn version_of(exe: &Path) -> Option<String> {
    let line = version_line_of(exe)?;
    line.split_whitespace().next().map(str::to_owned)
}

/// `<exe> --version`'s line after `nabi `, as `VERSION_LINE` is.
fn version_line_of(exe: &Path) -> Option<String> {
    let mut child = Command::new(exe)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if start.elapsed() < std::time::Duration::from_secs(3) => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let mut out = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take()?, &mut out).ok()?;
    // `nabi 0.1.0 (..)`.
    let rest = out.lines().next()?.strip_prefix("nabi ")?.trim();
    (!rest.is_empty()).then(|| rest.to_owned())
}

/// `nabi --version -v`: this binary, the toolchain it resolves and why,
/// the `nabic` it runs, and the host, as `key`, `value`.
pub fn describe(plus: Option<&str>) -> [(&'static str, String); 4] {
    let home = home();
    let toolchain = match select(&home, plus, None) {
        Ok(s) => format!(
            "{} ({})",
            s.dir.file_name().unwrap_or_default().to_string_lossy(),
            s.why
        ),
        Err(_) => format!(
            "none (this nabi is {VERSION}, not from {})",
            shown(&root(&home))
        ),
    };
    // A command here hands off to that toolchain's nabi, then its nabic.
    let nabic = match handoff(plus, project_pin(), false) {
        Ok(Some(h)) => h
            .dir
            .join("bin")
            .join(exe_name("nabic"))
            .display()
            .to_string(),
        Ok(None) => nabic_for().map_or_else(|e| e, |p| p.display().to_string()),
        Err(e) => e,
    };
    let binary = nabic_core::current_exe()
        .or_else(|| std::env::current_exe().ok())
        .map_or_else(|| "unknown".to_owned(), |p| p.display().to_string());
    [
        ("binary", binary),
        ("toolchain", toolchain),
        ("nabic", nabic),
        ("host", HOST.to_owned()),
    ]
}

/// If this executable is a proxy (`$NABI_HOME/bin/<tool>` beside
/// `toolchains/`), run the active toolchain's `<tool>`; the exit code.
/// `None` for `nabi toolchain ..`, which the proxy handles itself.
pub fn proxy(args: &[OsString]) -> Option<i32> {
    let exe = nabic_core::current_exe()?;
    let h = exe.parent()?.parent()?;
    if !h.join("toolchains").is_dir() {
        return None;
    }
    let tool = exe.file_stem()?.to_str()?;
    let tool = TOOLS.into_iter().find(|t| *t == tool)?;
    let (plus, rest) = match args.first().and_then(|a| a.to_str()) {
        Some(p) if p.len() > 1 && p.starts_with('+') => (Some(&p[1..]), &args[1..]),
        _ => (None, args),
    };
    let command = rest
        .iter()
        .find(|a| !a.to_str().is_some_and(|a| a.starts_with('-')));
    if tool == "nabi" && plus.is_none() && command.is_some_and(|c| c == "toolchain") {
        return None;
    }
    let script = if tool == "nabi" {
        script_arg(rest)
    } else {
        None
    };
    let dir = match select(h, plus, script.as_deref()) {
        Ok(s) => s.dir,
        Err(e) => {
            eprintln!("{tool}: {} {e}", term::label(Stream::Stderr, Role::Error));
            return Some(1);
        }
    };
    let target = dir.join("bin").join(exe_name(tool));
    let mut c = Command::new(&target);
    c.args(rest).env("NABI_HOME", h);
    if let Some(p) = plus {
        // The toolchain's `nabi` runs the same toolchain's `nabic`.
        c.env("NABI_TOOLCHAIN", p);
    }
    Some(crate::exec(c, &target))
}

/// Set on a `nabi` that `handoff` runs: the toolchain directories handed
/// off to so far, as a path list.
const GUARD: &str = "__NABI_REEXEC";

static GUARD_DIRS: std::sync::OnceLock<Vec<PathBuf>> = std::sync::OnceLock::new();

/// Take the guard a parent `nabi` set out of the environment, so programs
/// this run starts do not inherit it. Call first in `main`, while no
/// other thread runs.
pub fn take_guard() {
    if let Some(g) = std::env::var_os(GUARD) {
        // SAFETY: single-threaded here (see above).
        unsafe { std::env::remove_var(GUARD) };
        let _ = GUARD_DIRS.set(std::env::split_paths(&g).collect());
    }
}

fn guard_dirs() -> &'static [PathBuf] {
    GUARD_DIRS.get().map_or(&[], Vec::as_slice)
}

/// A toolchain whose own `nabi` runs this command instead.
pub struct Handoff {
    pub dir: PathBuf,
    pub nabi: PathBuf,
}

/// One toolchain per run: when `+<version>`,
/// `NABI_TOOLCHAIN` or `pin` names an installed toolchain other than this
/// nabi's, that toolchain.
///
/// Compared by directory, then by `nabi --version`,
/// never by the pin's text: a pin naming a directory matches no version.
/// A pin naming nothing installed is an error when `strict`, else this
/// nabi goes on.
pub fn handoff(
    plus: Option<&str>,
    pin: Option<(String, String)>,
    strict: bool,
) -> Result<Option<Handoff>, String> {
    let Some((pin, why)) = explicit(plus, pin) else {
        return Ok(None);
    };
    if pin_matches(&pin, VERSION) {
        return Ok(None);
    }
    let Some(dir) = find(&home(), &pin) else {
        return if strict {
            Err(not_installed(&pin, &why))
        } else {
            Ok(None)
        };
    };
    let dir = nabic_core::real_path(&dir).unwrap_or(dir);
    let me = nabic_core::current_exe();
    let own = me.as_deref().and_then(|e| e.parent()?.parent());
    if own == Some(dir.as_path()) {
        return Ok(None);
    }
    let nabi = dir.join("bin").join(exe_name("nabi"));
    if !nabi.is_file() {
        return Err(format!(
            "toolchain {} ({why}) has no bin/{}; install it again with `nabi toolchain install \
             {pin} --from <dir | .tar.gz | .zip | url>`",
            dir.file_name().unwrap_or_default().to_string_lossy(),
            exe_name("nabi")
        ));
    }
    // The same build placed elsewhere (a dev build, nix) is this one.
    if version_line_of(&nabi).as_deref() == Some(crate::VERSION_LINE) {
        return Ok(None);
    }
    if guard_dirs().contains(&dir) {
        return Err(format!(
            "{} ran for toolchain {pin} ({why}) already and handed the command back to {}; run \
             {} directly, or install that toolchain again (`nabi toolchain install {pin} --from \
             ..`)",
            nabi.display(),
            me.unwrap_or_default().display(),
            nabi.display()
        ));
    }
    Ok(Some(Handoff { dir, nabi }))
}

/// Run `h`'s `nabi` with `args` from `cwd`; its exit code.
pub fn run_handoff(h: &Handoff, args: &[std::ffi::OsString], cwd: Option<&Path>) -> i32 {
    let dirs = guard_dirs().iter().chain([&h.dir]);
    let mut c = Command::new(&h.nabi);
    c.args(args).env("NABI_HOME", home());
    if let Ok(g) = std::env::join_paths(dirs) {
        c.env(GUARD, g);
    }
    if let Some(d) = cwd {
        c.current_dir(d);
    }
    crate::exec(c, &h.nabi)
}

/// This `nabi`'s own `tool` (`nabicc`, ..), beside it.
pub fn tool_for(tool: &str) -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let p = exe.with_file_name(exe_name(tool));
    if !p.is_file() {
        return Err(format!("no `{tool}` next to {}", exe.display()));
    }
    Ok(p)
}

/// This `nabi`'s own `nabic`, beside it.
pub fn nabic_for() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let nabic = exe.with_file_name(exe_name("nabic"));
    if !nabic.is_file() {
        return Err(format!("no `nabic` next to {}", exe.display()));
    }
    Ok(nabic)
}

pub fn copy_tree(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|e| format!("{}: {e}", to.display()))?;
    for e in std::fs::read_dir(from).map_err(|e| format!("{}: {e}", from.display()))? {
        let e = e.map_err(|e| e.to_string())?;
        let (src, dst) = (e.path(), to.join(e.file_name()));
        if src.is_dir() {
            copy_tree(&src, &dst)?;
        } else {
            // `copy` keeps the permission bits (executables stay so).
            std::fs::copy(&src, &dst).map_err(|e| format!("{}: {e}", src.display()))?;
        }
    }
    Ok(())
}

fn run(c: &mut Command, what: &str) -> Result<String, String> {
    let out = c.output().map_err(|e| format!("cannot run {what}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "{what} failed:\n{}",
            String::from_utf8_lossy(&out.stderr).trim_end()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Check `archive` against `sum` (hex digest, optionally the file name).
fn check_sum(archive: &Path, sum: &Path, what: &str) -> Result<(), String> {
    let text = std::fs::read_to_string(sum).map_err(|e| format!("{}: {e}", sum.display()))?;
    let want = text
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    if want.len() != 64 || !want.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("{}: not a SHA-256 digest", sum.display()));
    }
    let bytes = std::fs::read(archive).map_err(|e| format!("{}: {e}", archive.display()))?;
    let got = nabic_core::hash::hex(&nabic_core::hash::sha256(&bytes));
    if got != want {
        return Err(format!(
            "{what} has SHA-256 {got}, but its .sha256 says {want} (a damaged or altered download)"
        ));
    }
    Ok(())
}

/// Check `archive`, named `name` in its release, against the release's
/// `SHA256SUMS` once its signature `sig` verifies with the release key
/// (`sign`); `origin` is where both came from, for errors.
fn check_signed(
    archive: &Path,
    name: &str,
    [sums, sig]: [&Path; 2],
    origin: &str,
    key: Option<&Path>,
) -> Result<(), String> {
    let text = std::fs::read(sums).map_err(|e| format!("{}: {e}", sums.display()))?;
    let sig = std::fs::read_to_string(sig).map_err(|e| format!("{}: {e}", sig.display()))?;
    let key = sign::trusted(key)?;
    sign::verify(&key, &text, &sig, &format!("{origin}/SHA256SUMS"))?;
    let want = sign::digest_of(&String::from_utf8_lossy(&text), name).ok_or_else(|| {
        format!(
            "{origin}/SHA256SUMS lists no {name}, so it cannot be checked; \
             name a release that has this host's archive"
        )
    })?;
    let bytes = std::fs::read(archive).map_err(|e| format!("{}: {e}", archive.display()))?;
    let got = nabic_core::hash::hex(&nabic_core::hash::sha256(&bytes));
    if got != want {
        return Err(format!(
            "{name} has SHA-256 {got}, but the signed SHA256SUMS says {want} \
             (a damaged or altered download); nothing was installed, try again"
        ));
    }
    Ok(())
}

/// Download `url` to `to`; `what` says what to do when it fails.
fn curl(url: &str, to: &Path, what: &str) -> Result<(), String> {
    run(
        Command::new("curl")
            .args(["-fsSL", "--proto", "=https,http", "-o"])
            .arg(to)
            .arg(url),
        &format!("curl {url}"),
    )
    .map(drop)
    .map_err(|e| format!("{e}\n{what}"))
}

/// Unpack `from` (a directory, `.tar[.gz]`, `.zip`, or a URL of one) into
/// `tmp`; returns the toolchain's top directory. A download must match
/// the signed `SHA256SUMS` beside it (`check_signed`), or nothing is
/// installed. A local archive is checked the same way when `SHA256SUMS`
/// is beside it, else against its `.sha256` if there is one.
fn unpack(from: &str, tmp: &Path, key: Option<&Path>) -> Result<PathBuf, String> {
    let mut archive = PathBuf::from(from);
    if from.starts_with("https://") || from.starts_with("http://") {
        let (base, name) = from.rsplit_once('/').unwrap_or(("", from));
        // `tar` tells the format by the content.
        archive = tmp.join("download");
        let sums = tmp.join("SHA256SUMS");
        let sig = tmp.join("SHA256SUMS.minisig");
        let unsigned = "a release is installed only with its signed SHA256SUMS and \
                        SHA256SUMS.minisig; check the version, or install an archive \
                        you checked yourself with `--from <file>`";
        curl(from, &archive, "check the version and the network")?;
        curl(&format!("{base}/SHA256SUMS"), &sums, unsigned)?;
        curl(&format!("{base}/SHA256SUMS.minisig"), &sig, unsigned)?;
        check_signed(&archive, name, [&sums, &sig], base, key)?;
    } else if archive.is_file() {
        let dir = archive.parent().unwrap_or_else(|| Path::new("."));
        let sums = dir.join("SHA256SUMS");
        let mut sum = archive.clone().into_os_string();
        sum.push(".sha256");
        let sum = PathBuf::from(sum);
        if sums.is_file() {
            let name = archive
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let sig = dir.join("SHA256SUMS.minisig");
            if !sig.is_file() {
                return Err(format!(
                    "{} has no SHA256SUMS.minisig beside it, so it cannot be checked; \
                     download it from the same release, or move SHA256SUMS aside to \
                     check against {name}.sha256",
                    sums.display()
                ));
            }
            check_signed(
                &archive,
                &name,
                [&sums, &sig],
                &dir.display().to_string(),
                key,
            )?;
        } else if sum.is_file() {
            check_sum(&archive, &sum, from)?;
        }
    }
    let files = tmp.join("files");
    if archive.is_dir() {
        copy_tree(&archive, &files)?;
    } else if archive.is_file() {
        std::fs::create_dir_all(&files).map_err(|e| e.to_string())?;
        run(
            Command::new("tar")
                .arg("-xf")
                .arg(&archive)
                .arg("-C")
                .arg(&files),
            &format!("tar -xf {}", archive.display()),
        )?;
    } else {
        return Err(format!("{from} does not exist"));
    }
    // An archive holding one top directory: that directory.
    if !files.join("bin").is_dir() {
        let entries: Vec<PathBuf> = std::fs::read_dir(&files)
            .map_err(|e| e.to_string())?
            .flatten()
            .map(|e| e.path())
            .collect();
        if let [only] = entries.as_slice() {
            return Ok(only.clone());
        }
    }
    Ok(files)
}

/// Check `top` is complete and is `version` for this host; its full
/// version.
fn verify(top: &Path, version: &str) -> Result<String, String> {
    let nabic = top.join("bin").join(exe_name("nabic"));
    if !nabic.is_file() {
        return Err("not a Nabi toolchain: no bin/nabic".to_owned());
    }
    for need in [
        format!("bin/{}", exe_name("nabi")),
        format!("lib/{}", nabic_target::TargetSpec::host().runtime_lib()),
        "lib/nabi/core/src/lib.nabi".to_owned(),
    ] {
        if !top.join(&need).exists() {
            return Err(format!("incomplete toolchain: no {need}"));
        }
    }
    let out = run(
        Command::new(&nabic).args(["--version", "--verbose"]),
        &nabic.display().to_string(),
    )?;
    let full = out
        .lines()
        .find_map(|l| l.strip_prefix("nabic "))
        .and_then(|l| l.split_whitespace().next())
        .ok_or_else(|| format!("unexpected `nabic --version` output: {out:?}"))?;
    if !pin_matches(version, full) {
        return Err(format!(
            "the toolchain is nabic {full}, not {version} (its `bin/nabic --version`)"
        ));
    }
    if let Some(h) = out.lines().find_map(|l| l.strip_prefix("host: ")) {
        if h.trim() != HOST {
            return Err(format!(
                "the toolchain is built for {}, but this machine is {HOST}",
                h.trim()
            ));
        }
    }
    Ok(full.to_owned())
}

/// A scratch directory under `toolchains/`, removed when dropped.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Unpack and verify `from` in scratch: (scratch, top dir, full version).
fn stage(
    home: &Path,
    version: &str,
    from: &str,
    key: Option<&Path>,
) -> Result<(Scratch, PathBuf, String), String> {
    std::fs::create_dir_all(root(home)).map_err(|e| e.to_string())?;
    let tmp = Scratch(root(home).join(format!(".tmp-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&tmp.0);
    std::fs::create_dir_all(&tmp.0).map_err(|e| format!("{}: {e}", tmp.0.display()))?;
    let top = unpack(from, &tmp.0, key)?;
    let full = verify(&top, version)?;
    Ok((tmp, top, full))
}

/// Move `top` to `toolchains/<full>-<host>`, moving an existing one aside
/// into `tmp` and back on failure; its name.
fn place(home: &Path, tmp: &Scratch, top: &Path, full: &str) -> Result<String, String> {
    let name = format!("{full}-{HOST}");
    let dest = root(home).join(&name);
    let old = tmp.0.join("old");
    let had = dest.exists();
    if had {
        std::fs::rename(&dest, &old).map_err(|e| format!("{}: {e}", dest.display()))?;
    }
    if let Err(e) = std::fs::rename(top, &dest) {
        if had {
            let _ = std::fs::rename(&old, &dest);
        }
        return Err(format!("{}: {e}", dest.display()));
    }
    Ok(name)
}

fn install(home: &Path, version: &str, from: &str, key: Option<&Path>) -> Result<String, String> {
    let (tmp, top, full) = stage(home, version, from, key)?;
    place(home, &tmp, &top, &full)
}

/// `$NABI_HOME/bin/{nabi,nabic,nabils}`: copies of the newest installed
/// toolchain's `nabi`, so the default going back never brings back an
/// older proxy.
fn write_proxies(home: &Path) -> Result<(), String> {
    let Some(newest) = newest_installed(home) else {
        return Ok(());
    };
    let bin = home.join("bin");
    std::fs::create_dir_all(&bin).map_err(|e| format!("{}: {e}", bin.display()))?;
    let src = newest.join("bin").join(exe_name("nabi"));
    for t in TOOLS {
        let dst = bin.join(exe_name(t));
        // Copy beside, then rename: a running proxy keeps its file.
        let tmp = bin.join(format!(".{t}.tmp"));
        std::fs::copy(&src, &tmp).map_err(|e| format!("{}: {e}", src.display()))?;
        if cfg!(windows) {
            set_aside(&bin, &dst);
        }
        std::fs::rename(&tmp, &dst).map_err(|e| format!("{}: {e}", dst.display()))?;
    }
    Ok(())
}

/// Windows can rename a running executable but not replace it: move `exe`
/// to `bin/.old-<n>` and delete older ones no longer running.
fn set_aside(bin: &Path, exe: &Path) {
    for e in std::fs::read_dir(bin).into_iter().flatten().flatten() {
        if e.file_name().to_string_lossy().starts_with(".old-") {
            let _ = std::fs::remove_file(e.path());
        }
    }
    if exe.exists() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let _ = std::fs::rename(exe, bin.join(format!(".old-{nanos:x}")));
    }
}

/// `write_proxies` when a proxy is missing or is not the newest
/// toolchain's `nabi`.
fn refresh_proxies(home: &Path) -> Result<(), String> {
    let Some(newest) = newest_installed(home) else {
        return Ok(());
    };
    let Ok(want) = std::fs::read(newest.join("bin").join(exe_name("nabi"))) else {
        return write_proxies(home);
    };
    let bin = home.join("bin");
    let current = TOOLS.iter().all(|t| {
        let p = bin.join(exe_name(t));
        std::fs::metadata(&p).is_ok_and(|m| m.len() == want.len() as u64)
            && std::fs::read(&p).is_ok_and(|b| b == want)
    });
    if current { Ok(()) } else { write_proxies(home) }
}

fn set_default(home: &Path, pin: &str) -> Result<PathBuf, String> {
    let dir = find(home, pin).ok_or_else(|| not_installed(pin, "the default"))?;
    crate::set_config(home, "toolchain", pin)?;
    refresh_proxies(home)?;
    Ok(dir)
}

/// The completion scripts install.sh and install.ps1 write: file, shell
/// and the tools whose scripts it holds.
fn completion_files(home: &Path) -> Vec<(PathBuf, &'static str, &'static [&'static str])> {
    let var = |k: &str| {
        std::env::var_os(k)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    let user = nabic_core::user_home();
    let data = var("XDG_DATA_HOME").or_else(|| user.as_ref().map(|h| h.join(".local/share")));
    let config = var("XDG_CONFIG_HOME").or_else(|| user.as_ref().map(|h| h.join(".config")));
    let mut files = Vec::new();
    let tools: &[&str] = &["nabi"];
    if let Some(d) = &data {
        files.push((d.join("bash-completion/completions/nabi"), "bash", tools));
    }
    files.push((home.join("zsh/_nabi"), "zsh", tools));
    if let Some(c) = &config {
        files.push((c.join("fish/completions/nabi.fish"), "fish", tools));
    }
    files.push((home.join("completions/nabi.ps1"), "powershell", tools));
    files
}

/// Rewrite the completion scripts that exist from `toolchain`'s tools;
/// what was rewritten, or why not. Never creates one, and a toolchain
/// without `completions` leaves them as they are.
fn refresh_completions(home: &Path, toolchain: &Path) -> Vec<Result<String, String>> {
    let script = |tool: &str, shell: &str| {
        let exe = toolchain.join("bin").join(exe_name(tool));
        let out = Command::new(exe)
            .args(["completions", shell])
            .env("NABI_HOME", home)
            .env_remove("NABI_TOOLCHAIN")
            .current_dir(home)
            .output()
            .ok()?;
        let mut text = out.stdout;
        if !out.status.success() || text.is_empty() {
            return None;
        }
        if !text.ends_with(b"\n") {
            text.push(b'\n');
        }
        Some(text)
    };
    let mut done = Vec::new();
    for (file, shell, tools) in completion_files(home) {
        if !file.is_file() {
            continue;
        }
        let Some(parts) = tools
            .iter()
            .map(|t| script(t, shell))
            .collect::<Option<Vec<_>>>()
        else {
            continue;
        };
        let mut tmp = file.clone().into_os_string();
        tmp.push(".new");
        let tmp = PathBuf::from(tmp);
        let r = std::fs::write(&tmp, parts.concat()).and_then(|()| std::fs::rename(&tmp, &file));
        done.push(match r {
            Ok(()) => Ok(format!(
                "{shell} completions for {} in {}",
                tools.join(" and "),
                shown(&file)
            )),
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                Err(format!("cannot refresh {}: {e}", file.display()))
            }
        });
    }
    done
}

/// Report `refresh_completions`: failures always, the files under `-v`.
fn report_completions(done: &[Result<String, String>], verbose: bool) {
    for r in done {
        match r {
            Ok(m) if verbose => term::note(&format!("refreshed {m}")),
            Ok(_) => {}
            Err(e) => eprintln!("nabi: {} {e}", term::label(Stream::Stderr, Role::Warning)),
        }
    }
}

/// Release server in GitHub releases' layout:
/// `download/v<version>/nabi-<version>-<triple>.<ext>` (+ `.sha256`) and
/// `latest/download/nabi-version`. `NABI_DIST` overrides it.
fn dist() -> String {
    std::env::var("NABI_DIST")
        .ok()
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| "https://github.com/nabi-lang/nabi/releases".to_owned())
        .trim_end_matches('/')
        .to_owned()
}

const ARCHIVE: &str = if cfg!(windows) { "zip" } else { "tar.gz" };

/// `v` if it can be a release's full version.
fn release_version(v: &str, from: &str) -> Result<String, String> {
    let ok = v.starts_with(|c: char| c.is_ascii_digit())
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'));
    if ok {
        Ok(v.to_owned())
    } else {
        Err(format!("{from}: `{v}` is not a version"))
    }
}

/// The text at `url`; `None` when the server has no such file (404).
fn fetch(url: &str) -> Result<Option<String>, String> {
    let out = run(
        Command::new("curl").args([
            "-sSL",
            "--proto",
            "=https,http",
            "-w",
            "\n%{http_code}",
            url,
        ]),
        &format!("curl {url}"),
    )?;
    let (body, code) = out.rsplit_once('\n').unwrap_or(("", &out));
    match code.trim() {
        "404" => Ok(None),
        c if c.starts_with('2') => Ok(Some(body.to_owned())),
        c => Err(format!("curl {url}: HTTP status {c}")),
    }
}

/// The newest release, from `<dist>/latest/download/nabi-version`.
fn latest(dist: &str) -> Result<String, String> {
    let url = format!("{dist}/latest/download/nabi-version");
    let text = fetch(&url)?.ok_or_else(|| {
        format!(
            "no release is published at {url}; name a version: `nabi toolchain update <version>`"
        )
    })?;
    release_version(text.trim(), &url)
}

/// The release `want` names: a full version only; there is no series
/// or requirement lookup.
fn exact(want: &str, from: &str) -> Result<String, String> {
    if crate::semver::Version::parse(want).is_ok() {
        return release_version(want, from);
    }
    let newest = if from == "nabi toolchain update" {
        ", or nothing for the newest release"
    } else {
        ""
    };
    Err(format!(
        "{from}: `{want}` is not a full version; write one like `0.1.0`{newest}"
    ))
}

/// Download release `full` for this host into `toolchains/`.
fn download(
    home: &Path,
    dist: &str,
    full: &str,
    quiet: bool,
    key: Option<&Path>,
) -> Result<(), String> {
    let url = format!("{dist}/download/v{full}/nabi-{full}-{HOST}.{ARCHIVE}");
    if !quiet {
        term::status("Downloading", &format!("nabi {full} ({HOST})"));
    }
    let (tmp, top, _) = stage(home, full, &url, key)?;
    place(home, &tmp, &top, full)?;
    Ok(())
}

/// Check the proxies run `full` as the default (no `NABI_TOOLCHAIN`, no
/// project).
fn check_proxies(home: &Path, full: &str) -> Result<(), String> {
    let version = |tool: &str| {
        let exe = home.join("bin").join(exe_name(tool));
        run(
            Command::new(&exe)
                .arg("--version")
                .env("NABI_HOME", home)
                .env_remove("NABI_TOOLCHAIN")
                .current_dir(home),
            &exe.display().to_string(),
        )
    };
    version("nabi")?;
    let out = version("nabic")?;
    let first = out.lines().next().unwrap_or("");
    let want = format!("nabic {full}");
    if first != want && !first.starts_with(&format!("{want} ")) {
        return Err(format!("the nabic proxy says {first:?}, not {want}"));
    }
    Ok(())
}

/// Make `full` the default, write and check its proxies; on failure
/// restore config and proxies, and remove `dir` if `fresh`.
fn switch(home: &Path, full: &str, dir: &Path, fresh: bool) -> Result<(), String> {
    let bin = home.join("bin");
    let config = home.join("config.nabion");
    let old_config = std::fs::read(&config).ok();
    let mut kept = Vec::new();
    for t in TOOLS {
        let p = bin.join(exe_name(t));
        if p.is_file() {
            let prev = bin.join(format!(".{t}.prev"));
            std::fs::copy(&p, &prev).map_err(|e| format!("{}: {e}", p.display()))?;
            kept.push((p, prev));
        }
    }
    let Err(e) = set_default(home, full).and_then(|_| check_proxies(home, full)) else {
        for (_, prev) in &kept {
            let _ = std::fs::remove_file(prev);
        }
        return Ok(());
    };
    let mut lost = Vec::new();
    let restored = match &old_config {
        Some(c) => std::fs::write(&config, c),
        None => std::fs::remove_file(&config),
    };
    if let Err(e) = restored {
        lost.push(format!("{}: {e}", config.display()));
    }
    for t in TOOLS {
        let p = bin.join(exe_name(t));
        let r = match kept.iter().find(|(q, _)| *q == p) {
            Some((_, prev)) => {
                if cfg!(windows) {
                    set_aside(&bin, &p);
                }
                std::fs::rename(prev, &p)
            }
            None if p.exists() => std::fs::remove_file(&p),
            None => Ok(()),
        };
        if let Err(e) = r {
            lost.push(format!("{}: {e}", p.display()));
        }
    }
    if fresh {
        let _ = std::fs::remove_dir_all(dir);
    }
    if lost.is_empty() {
        Err(format!(
            "{e}\nrolled back: the default toolchain and the proxies are as they were"
        ))
    } else {
        Err(format!("{e}\nrolling back failed: {}", lost.join("; ")))
    }
}

/// `nabi toolchain update`: install `version` (default: the newest
/// release, in any series), make it the default, update the proxies.
fn update(
    home: &Path,
    version: Option<&str>,
    [quiet, verbose]: [bool; 2],
    key: Option<&Path>,
) -> Result<(), String> {
    let dist = dist();
    let current = default_pin(home)?.and_then(|p| find(home, &p));
    let full = match version {
        Some(v) => exact(v, "nabi toolchain update")?,
        None => latest(&dist)?,
    };
    let name = format!("{full}-{HOST}");
    let dir = root(home).join(&name);
    if current.as_ref() == Some(&dir) && check_proxies(home, &full).is_ok() {
        if !quiet {
            eprintln!("nabi {full} is up to date");
        }
        return Ok(());
    }
    let fresh = !dir.is_dir();
    if fresh {
        download(home, &dist, &full, quiet, key)?;
    }
    switch(home, &full, &dir, fresh)?;
    let refreshed = refresh_completions(home, &dir);
    if quiet {
        return Ok(());
    }
    let suffix = format!("-{HOST}");
    let old = current
        .as_deref()
        .and_then(Path::file_name)
        .map(|n| n.to_string_lossy().into_owned());
    match &old {
        // The default already, with broken proxies.
        Some(n) if *n == name => term::status("Repaired", &format!("nabi {full} proxies")),
        Some(n) => {
            let from = n.strip_suffix(&suffix).unwrap_or(n);
            term::status("Updated", &format!("nabi {from} -> {full}"));
        }
        None => term::status("Installed", &format!("nabi {full} to {}", shown(home))),
    }
    if let Some(old) = old.filter(|n| *n != name) {
        let from = old.strip_suffix(&suffix).unwrap_or(&old);
        match series(from).filter(|s| series(&full).as_ref() != Some(s)) {
            // Another series: only the newest gets fixes, so say to move.
            Some(s) => term::note(&format!(
                "nabi {from} stays installed, but {s} gets no more fixes; move projects to \
                 {full}. Until a project moves it can pin {s} (`toolchain: \"{s}\"` in \
                 nabi.nabion or a script's `//! nabion` block) or run `nabi +{s} ..`"
            )),
            None => term::note(&format!(
                "{old} stays installed (`nabi toolchain remove {old}`)"
            )),
        }
    }
    report_completions(&refreshed, verbose);
    Ok(())
}

/// `path` as `~/..` when under the user's home, for display only.
fn shown(path: &Path) -> String {
    let user = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .filter(|h| !h.is_empty());
    match user.and_then(|h| path.strip_prefix(h).ok().map(Path::to_path_buf)) {
        Some(rest) if !rest.as_os_str().is_empty() => {
            format!("~{}{}", std::path::MAIN_SEPARATOR, rest.display())
        }
        _ => path.display().to_string(),
    }
}

/// `nabi toolchain clean`: remove every installed toolchain but the
/// current default. Opt-in tidy-up; a version a project pins is removed
/// too, so it is never run implicitly.
fn clean(home: &Path, quiet: bool) -> Result<(), String> {
    let def = default_pin(home)?
        .and_then(|p| find(home, &p))
        .ok_or_else(|| {
            "no default toolchain to keep; set one with `nabi toolchain default <version>`, \
         or remove a toolchain by name (`nabi toolchain remove <version>`)"
                .to_owned()
        })?;
    let mut removed = 0;
    for (name, _) in installed(home) {
        let dir = root(home).join(&name);
        if dir == def {
            continue;
        }
        std::fs::remove_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        removed += 1;
        if !quiet {
            term::status("Removed", &format!("toolchain {name}"));
        }
    }
    refresh_proxies(home)?;
    if !quiet {
        let kept = def.file_name().unwrap_or_default().to_string_lossy();
        if removed == 0 {
            eprintln!("nothing to clean; only the default ({kept}) is installed");
        } else {
            term::status("Kept", &format!("the default toolchain {kept}"));
        }
    }
    Ok(())
}

pub const USAGE: &str = "\
usage: nabi toolchain list
       nabi toolchain install <version> [--from <dir | .tar[.gz] | .zip | url>] [--key <file>]
       nabi toolchain default [<version>]
       nabi toolchain remove <version>
       nabi toolchain update [<version>] [--key <file>]
       nabi toolchain clean
--key <file>: check downloads with this minisign public key instead of the
nabi release key (a release set signed for a test)";

pub fn cmd(args: &[String], quiet: bool, verbose: bool) -> Result<i32, String> {
    let h = home();
    // `--key <file>`, for install and update only.
    let mut key_file = None;
    let mut words: Vec<&str> = Vec::new();
    let mut it = args.iter().map(String::as_str);
    while let Some(a) = it.next() {
        if a == "--key" {
            key_file = Some(it.next().ok_or_else(|| USAGE.to_owned())?);
        } else if let Some(f) = a.strip_prefix("--key=") {
            key_file = Some(f);
        } else {
            words.push(a);
        }
    }
    if key_file.is_some() && !matches!(words.first(), Some(&("install" | "update"))) {
        return Err(USAGE.into());
    }
    let key = key_file.map(Path::new);
    let args = words;
    match args.as_slice() {
        ["list"] => {
            let def = default_pin(&h)?.and_then(|p| find(&h, &p));
            let all = installed(&h);
            if all.is_empty() {
                println!("no toolchains installed (this nabi is {VERSION})");
            }
            let current = all
                .iter()
                .position(|(name, _)| def.as_ref().is_some_and(|d| d.ends_with(name)));
            let rows: Vec<(String, String)> =
                all.into_iter().map(|(n, _)| (n, String::new())).collect();
            print!("{}", term::list(&rows, current, "(default)"));
        }
        ["install", version, rest @ ..] if !version.starts_with('-') => {
            let from = match rest {
                [] => None,
                ["--from", f] => Some(*f),
                [f] if f.starts_with("--from=") => Some(&f[7..]),
                _ => return Err(USAGE.into()),
            };
            let name = match from {
                Some(f) => install(&h, version, f, key)?,
                // A release, downloaded; the default stays.
                None => {
                    let dist = dist();
                    let full = exact(version, "nabi toolchain install")?;
                    let name = format!("{full}-{HOST}");
                    if root(&h).join(&name).is_dir() {
                        if !quiet {
                            eprintln!("toolchain {name} is already installed");
                        }
                        return Ok(0);
                    }
                    download(&h, &dist, &full, quiet, key)?;
                    name
                }
            };
            if !quiet {
                term::status("Installed", &format!("toolchain {name}"));
            }
            // The first toolchain becomes the default; proxies once.
            let def = default_pin(&h)?;
            if def.is_none() {
                set_default(&h, &name)?;
                if !quiet {
                    term::status("Default", &format!("toolchain {name}"));
                }
            } else {
                refresh_proxies(&h)?;
            }
        }
        ["update"] => {
            update(&h, None, [quiet, verbose], key)?;
            path_warning(&h, quiet);
        }
        ["update", v] if !v.starts_with('-') => {
            update(&h, Some(v), [quiet, verbose], key)?;
            path_warning(&h, quiet);
        }
        ["clean"] => clean(&h, quiet)?,
        ["default"] => match default_pin(&h)? {
            Some(p) => println!("{p}"),
            None => return Err("no default toolchain".into()),
        },
        ["default", pin] => {
            let dir = set_default(&h, pin)?;
            let refreshed = refresh_completions(&h, &dir);
            if !quiet {
                let name = dir.file_name().unwrap_or_default().to_string_lossy();
                term::status("Default", &format!("toolchain {pin} ({name})"));
                report_completions(&refreshed, verbose);
            }
            path_warning(&h, quiet);
        }
        ["remove", name] if !name.contains(['/', '\\']) && !name.starts_with('.') => {
            let dir = [root(&h).join(name), root(&h).join(format!("{name}-{HOST}"))]
                .into_iter()
                .find(|d| d.is_dir())
                .ok_or_else(|| format!("toolchain {name} is not installed"))?;
            std::fs::remove_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            // It may have been the newest, whose nabi the proxies are.
            refresh_proxies(&h)?;
        }
        _ => return Err(USAGE.into()),
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    #[test]
    fn installed_in_semver_order() {
        let home = nabic_core::tmp::test_dir("tc-order");
        for v in [
            "0.10.0",
            "0.1.0",
            "0.1.0-rc.1",
            "0.1.1-rc.1",
            "0.9.0",
            "0.11.0-rc.1",
            "0.11.0-rc.2",
        ] {
            std::fs::create_dir_all(super::root(&home).join(format!("{v}-{}", super::HOST)))
                .unwrap();
        }
        let got: Vec<String> = super::installed(&home)
            .into_iter()
            .map(|(_, v)| v)
            .collect();
        let pick = |pin: &str| {
            super::find(&home, pin)
                .and_then(|d| d.file_name().map(|n| n.to_string_lossy().into_owned()))
                .map(|n| n.trim_end_matches(&format!("-{}", super::HOST)).to_owned())
        };
        // A series takes its newest release over any candidate; a
        // candidate only when the series has no release.
        let picks = [
            pick("0.1"),
            pick("0.11"),
            pick("0.1.0"),
            pick("0.1.1-rc.1"),
            pick("^0.1.0"),
        ];
        let _ = std::fs::remove_dir_all(&home);
        assert_eq!(
            got,
            [
                "0.1.0-rc.1",
                "0.1.0",
                "0.1.1-rc.1",
                "0.9.0",
                "0.10.0",
                "0.11.0-rc.1",
                "0.11.0-rc.2"
            ]
        );
        let want = ["0.1.0", "0.11.0-rc.2", "0.1.0", "0.1.1-rc.1", "0.1.0"];
        assert_eq!(picks, want.map(|v| Some(v.to_owned())));
    }
}
