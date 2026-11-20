//! Registry source archives: the gzipped tar of a published version's
//! commit that the registry keeps and serves at
//! `<api>/api/v1/packages/<name>/<version>/download`.
//!
//! The index line's `archive` is its SHA-256; a download that does not match it is refused.
//! Each is unpacked once:
//!
//! ```text
//! $NABI_HOME/cache/registry/src/<sha256>/   a version's files (the repository root)
//! ```

use crate::native;
use crate::registry::Entry;
use nabic_core::hash::{hex, sha256};
use std::path::{Path, PathBuf};

/// Why no archive served.
pub enum Miss {
    /// None to be had (no API, not kept, unreachable, offline): the git
    /// source serves instead.
    Absent(String),
    /// One came and is wrong: the build stops.
    Bad(String),
}

fn cache(home: &Path) -> PathBuf {
    home.join("cache/registry/src")
}

/// Where the archive with SHA-256 `sum` is unpacked.
pub fn dir(home: &Path, sum: &str) -> PathBuf {
    cache(home).join(sum)
}

/// The files of `e` from its archive with SHA-256 `sum`, from the cache
/// or downloaded from `api`.
pub fn fetch(
    home: &Path,
    api: Option<&str>,
    e: &Entry,
    sum: &str,
    offline: bool,
    quiet: bool,
) -> Result<PathBuf, Miss> {
    let dir = cache(home).join(sum);
    if dir.is_dir() {
        return Ok(dir);
    }
    let Some(api) = api else {
        return Err(Miss::Absent("its index names no registry API".to_owned()));
    };
    if offline {
        return Err(Miss::Absent(
            "its archive is not in the cache (--offline)".to_owned(),
        ));
    }
    let url = format!("{api}/api/v1/packages/{}/{}/download", e.name, e.version);
    if !quiet {
        nabic_core::term::status("Downloading", &format!("{} {}", e.name, e.version));
    }
    let root = cache(home);
    let tmp = root.join(format!(".tmp-{sum}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).map_err(|x| Miss::Bad(format!("{}: {x}", tmp.display())))?;
    let r = unpack(&tmp, &url, e, sum, &dir);
    let _ = std::fs::remove_dir_all(&tmp);
    r.map(|()| dir)
}

fn unpack(tmp: &Path, url: &str, e: &Entry, sum: &str, dir: &Path) -> Result<(), Miss> {
    let file = tmp.join("source.tar.gz");
    let bytes = native::fetch(url, &file).map_err(Miss::Absent)?;
    let got = hex(&sha256(&bytes));
    if got != sum {
        return Err(Miss::Bad(format!(
            "package `{}` {}: {url} has SHA-256 {got}, but the index says {sum}; the archive or \
             the index is damaged (run again later, or report it to the registry)",
            e.name, e.version
        )));
    }
    std::fs::write(&file, &bytes).map_err(|x| Miss::Bad(format!("{}: {x}", file.display())))?;
    let files = tmp.join("files");
    native::extract(&file, &files)
        .map_err(|m| Miss::Bad(format!("package `{}` {}: {m}", e.name, e.version)))?;
    // `git archive --prefix=<name>-<version>/`: one directory at the top.
    let entries: Vec<PathBuf> = std::fs::read_dir(&files)
        .map_err(|x| Miss::Bad(format!("{}: {x}", files.display())))?
        .filter_map(|d| d.ok().map(|d| d.path()))
        .collect();
    let top = match entries.as_slice() {
        [one] if one.is_dir() => one.clone(),
        _ => files,
    };
    // Another process may have won the race: theirs is as good.
    if std::fs::rename(&top, dir).is_err() && !dir.is_dir() {
        return Err(Miss::Bad(format!("cannot create {}", dir.display())));
    }
    Ok(())
}
