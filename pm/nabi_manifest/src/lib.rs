//! Packages on disk: `nabi.nabion` manifests and `nabi.lock` files,
//! workspaces, version requirements, target predicates and toolchain
//! pins, as every part of `nabi` reads them.

pub mod lock;
pub mod manifest;
pub mod pin;
pub mod platform;
pub mod semver;
pub mod workspace;

use nabic_core::term::{self, Role, Stream};
use std::path::{Path, PathBuf};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The user's nabi directory (`nabic_core::nabi_home`).
pub fn home() -> PathBuf {
    nabic_core::nabi_home().unwrap_or_else(|| PathBuf::from(".nabi"))
}

/// `nabi: warning: <msg>` on stderr, unless already printed.
pub fn warn_once(msg: &str) {
    static SEEN: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
    let mut seen = SEEN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if seen.iter().any(|s| s == msg) {
        return;
    }
    seen.push(msg.to_owned());
    drop(seen);
    eprintln!("nabi: {} {msg}", term::label(Stream::Stderr, Role::Warning));
}

/// `-v`, for readers too deep to be passed it.
pub static VERBOSE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Under `-v`, name the fields of shared file `file` this nabi skipped;
/// once per file.
pub fn skipped(file: &str, fields: &[&str]) {
    static SEEN: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
    if fields.is_empty() || !VERBOSE.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    {
        let mut seen = SEEN
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if seen.iter().any(|f| f == file) {
            return;
        }
        seen.push(file.to_owned());
    }
    let names: Vec<String> = fields.iter().map(|f| format!("`{f}`")).collect();
    term::note(&format!(
        "{file}: skipped {}, written by a newer nabi (kept in the file)",
        names.join(", ")
    ));
}

/// The package directory `start` belongs to: the nearest directory at
/// or above it holding `src/main.nabi`, `src/lib.nabi` or `nabi.nabion`.
pub fn package_dir(start: &Path) -> Option<PathBuf> {
    let abs = std::path::absolute(start).ok()?;
    let from = if abs.is_file() { abs.parent()? } else { &abs };
    from.ancestors()
        .find(|d| {
            d.join("src/main.nabi").is_file()
                || d.join("src/lib.nabi").is_file()
                || manifest::find(d).is_some()
        })
        .map(Path::to_path_buf)
}

/// `new`, an edit of NABION text `old`: in canonical form
/// when `old` was, else as edited (comments and layout kept).
pub fn canonical_edit(old: &str, new: &str) -> String {
    let canon = |t: &str| {
        nabic_nabion::parse(t)
            .ok()
            .map(|v| nabic_nabion::to_string(&v))
    };
    match (canon(old), canon(new)) {
        (Some(o), Some(n)) if o.trim() == old.trim() => n + "\n",
        _ => new.to_owned(),
    }
}
