//! The function cache: what a build made of standard library code (its
//! optimized functions, its codegen units' objects), kept by content for
//! every later build of the toolchain, in `cache_dir()/fn`.
//!
//! An entry is a file `<compiler>/<kind>/<first two hex>/<key>`, written
//! beside its place and renamed into it: a key names one content, so
//! builds racing on it write the same bytes, and a reader sees a whole
//! entry or none. `<compiler>` hashes the compiler's identity: another
//! compiler's entries go once it runs. Reading an entry older than an
//! hour stamps it; once a day a build removes those unused for long, and
//! the cache's cap (`cache`) the least recently used past its size. A
//! cache the toolchain ships read-only (a sandbox image warmed when it
//! was built) is only read.

pub use crate::cache::{Pruned, prune};

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Bumped on any change to the file layout.
const MAGIC: &[u8; 8] = b"NABIFN01";

/// An entry read this long after its last stamp is stamped again: the
/// cap's grace, so an entry in use is never evicted.
const RESTAMP: Duration = crate::cache::GRACE;

/// The daily trim runs when this mark is older than a day.
const TRIM: Duration = Duration::from_secs(24 * 3600);

/// Entries unused this long go in the daily trim.
pub const UNUSED: Duration = Duration::from_secs(30 * 24 * 3600);

/// What kind of result an entry holds: its folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// An optimized function (`nabic_mir::Memo`).
    Opt,
    /// A codegen unit's object.
    Object,
}

impl Kind {
    const fn dir(self) -> &'static str {
        match self {
            Self::Opt => "opt",
            Self::Object => "obj",
        }
    }
}

/// The cache of this toolchain.
pub struct FnCache {
    /// `cache_dir()/fn`, and this compiler's folder in it.
    dir: PathBuf,
    own: PathBuf,
    write: bool,
}

/// The function cache of the running toolchain: `None` without a cache
/// directory or under `NABI_FN_CACHE=0`.
pub fn shared() -> Option<&'static FnCache> {
    static CACHE: std::sync::OnceLock<Option<FnCache>> = std::sync::OnceLock::new();
    CACHE
        .get_or_init(|| {
            if std::env::var_os("NABI_FN_CACHE").is_some_and(|v| v == "0") {
                return None;
            }
            let dir = crate::cache_dir()?.join("fn");
            let write = writable(&dir);
            if !write && !dir.is_dir() {
                return None;
            }
            let id = nabic_core::hash::fnv(crate::nabihir::producer().as_bytes());
            let own = dir.join(format!("{id:016x}"));
            let c = FnCache { dir, own, write };
            if write {
                c.trim();
            }
            Some(c)
        })
        .as_ref()
}

/// The cache of compiler `id` in `dir` (tests).
#[cfg(test)]
pub(crate) fn at(dir: &Path, id: &str) -> FnCache {
    FnCache {
        dir: dir.to_path_buf(),
        own: dir.join(id),
        write: true,
    }
}

fn writable(dir: &Path) -> bool {
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let probe = dir.join(format!(".probe-{}", std::process::id()));
    let ok = std::fs::write(&probe, b"").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

impl FnCache {
    /// The folder of the cache.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path(&self, kind: Kind, key: &str) -> PathBuf {
        let fan = key.get(..2).unwrap_or("00");
        self.own.join(kind.dir()).join(fan).join(key)
    }

    /// The entry of `kind` at `key`.
    #[must_use]
    pub fn get(&self, kind: Kind, key: &str) -> Option<Vec<u8>> {
        let path = self.path(kind, key);
        let mut f = std::fs::File::open(&path).ok()?;
        let mut data = Vec::new();
        std::io::Read::read_to_end(&mut f, &mut data).ok()?;
        let body = data.get(16..)?;
        let sum = u64::from_le_bytes(data.get(8..16)?.try_into().ok()?);
        if !data.starts_with(MAGIC) || nabic_core::hash::fast(body) != sum {
            return None;
        }
        if self.write {
            let old = f
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_none_or(|age| age > RESTAMP);
            if old {
                stamp(&path);
            }
        }
        data.drain(..16);
        Some(data)
    }

    /// Keep `bytes` as the entry of `kind` at `key` (a no-op when it is
    /// there or the cache is read-only).
    pub fn put(&self, kind: Kind, key: &str, bytes: &[u8]) {
        if !self.write {
            return;
        }
        let path = self.path(kind, key);
        if path.is_file() {
            return;
        }
        let Some(dir) = path.parent() else { return };
        if std::fs::create_dir_all(dir).is_err() {
            return;
        }
        let mut data = Vec::with_capacity(bytes.len() + 16);
        data.extend_from_slice(MAGIC);
        data.extend_from_slice(&nabic_core::hash::fast(bytes).to_le_bytes());
        data.extend_from_slice(bytes);
        let tmp = dir.join(format!(".{key}.{}", std::process::id()));
        if std::fs::write(&tmp, &data).is_err() || std::fs::rename(&tmp, &path).is_err() {
            let _ = std::fs::remove_file(&tmp);
        } else {
            crate::cache::wrote(data.len() as u64);
        }
    }

    /// Remove other compilers' entries when this one is new here, and
    /// once a day the entries unused for `UNUSED`.
    fn trim(&self) {
        let current = self.dir.join("compiler");
        let name = self.own.file_name().unwrap_or_default().to_string_lossy();
        if std::fs::read_to_string(&current).ok().as_deref() != Some(&*name) {
            for e in std::fs::read_dir(&self.dir).into_iter().flatten().flatten() {
                if e.path().is_dir() && e.path() != self.own {
                    let _ = std::fs::remove_dir_all(e.path());
                }
            }
            let _ = std::fs::write(&current, name.as_bytes());
        }
        let mark = self.dir.join("trimmed");
        let due = std::fs::metadata(&mark)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_none_or(|age| age > TRIM);
        if !due || std::fs::write(&mark, b"").is_err() {
            return;
        }
        if let Some(root) = self.dir.parent() {
            prune(root, UNUSED, None);
        }
    }
}

/// Mark `path` as just used.
fn stamp(path: &Path) {
    if let Ok(f) = std::fs::File::options().append(true).open(path) {
        let _ = f.set_modified(SystemTime::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_read_back_and_prune_by_use() {
        let root = nabic_core::tmp::test_dir("fn-cache");
        let c = at(&root.join("fn"), "0123456789abcdef");
        c.put(Kind::Object, "ab12", b"one");
        c.put(Kind::Opt, "cd34", b"two");
        assert_eq!(c.get(Kind::Object, "ab12").as_deref(), Some(&b"one"[..]));
        assert_eq!(c.get(Kind::Opt, "ab12"), None);
        // A damaged entry is a miss.
        let p = c.path(Kind::Opt, "cd34");
        let mut bytes = std::fs::read(&p).unwrap();
        *bytes.last_mut().unwrap() ^= 1;
        std::fs::write(&p, bytes).unwrap();
        assert_eq!(c.get(Kind::Opt, "cd34"), None);
        let old = std::fs::File::options().append(true).open(&p).unwrap();
        old.set_modified(SystemTime::UNIX_EPOCH).unwrap();
        let r = crate::cache::prune(&root, UNUSED, None);
        assert_eq!((r.removed, r.kept), (1, 1));
        let r = crate::cache::prune(&root, UNUSED, Some(0));
        assert_eq!((r.removed, r.kept, r.size), (1, 0, 0));
        let _ = std::fs::remove_dir_all(&root);
    }
}
