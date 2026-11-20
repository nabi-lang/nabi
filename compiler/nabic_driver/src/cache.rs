//! The size cap of nabic's build cache (`cache_dir()`): the function
//! cache's entries (`fn/`) and the library's stages (`library/`) are
//! evicted least recently used first once they pass the cap.
//!
//! A build that wrote entries appends their size to the ledger
//! `cache_dir()/usage`, whose first line is the total of the last scan
//! and the total that calls for the next one. When the sum passes it (or
//! the scan is a day old) one build scans: past the cap it removes the
//! oldest entries down to 80% of it, so the next scan waits for another
//! fifth of the cap. One process scans at a time (`prune.lock`).
//!
//! An entry's mtime is its last use: written, or read (stamped at most
//! hourly). An automatic scan leaves entries used in the last hour and
//! scratch files alone: they are what running builds write and read.
//! Entries are renamed into place whole, so a reader sees an entry or
//! none, and an entry removed under it is a miss, computed again.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

/// The cap without `NABI_CACHE_MAX_SIZE`: 5 GiB.
pub const DEFAULT_MAX: u64 = 5 << 30;

/// A scan past the cap leaves this percentage of it.
const LOW: u64 = 80;

/// An automatic scan never takes an entry used this recently.
pub const GRACE: Duration = Duration::from_secs(3600);

/// A scratch file of a build that died goes after this long.
const SCRATCH: Duration = Duration::from_secs(3600);

/// The ledger is scanned again at least this often.
const RESCAN: Duration = Duration::from_secs(24 * 3600);

/// A scan lock older than this is left by a process that died.
const STALE_LOCK: Duration = Duration::from_secs(600);

const LEDGER: &str = "usage";
const LOCK: &str = "prune.lock";

/// Bytes this process wrote to the cache since its last `settle`.
static WRITTEN: AtomicU64 = AtomicU64::new(0);

/// Count `bytes` just written to the cache.
pub fn wrote(bytes: u64) {
    WRITTEN.fetch_add(bytes, Ordering::Relaxed);
}

/// `2G`, `500M`, `64K` or plain bytes.
#[must_use]
pub fn parse_size(s: &str) -> Option<u64> {
    let (n, unit) = match s.char_indices().last()? {
        (i, 'K' | 'k') => (&s[..i], 1u64 << 10),
        (i, 'M' | 'm') => (&s[..i], 1 << 20),
        (i, 'G' | 'g') => (&s[..i], 1 << 30),
        _ => (s, 1),
    };
    n.parse::<u64>().ok()?.checked_mul(unit)
}

/// The cap: `NABI_CACHE_MAX_SIZE` (a size, or `off` for none), else
/// `DEFAULT_MAX`.
#[must_use]
pub fn max_size() -> Option<u64> {
    let v = nabic_core::env_knob(
        "NABI_CACHE_MAX_SIZE",
        "a size such as `5G`, `500M` or `off`",
        |v| {
            if v == "off" {
                Some(None)
            } else {
                parse_size(v).map(Some)
            }
        },
    );
    v.unwrap_or(Some(DEFAULT_MAX))
}

/// After a build wrote to the cache: note what, and scan when due.
pub fn settle() {
    let n = WRITTEN.swap(0, Ordering::Relaxed);
    if n == 0 {
        return;
    }
    let (Some(dir), Some(max)) = (crate::cache_dir(), max_size()) else {
        return;
    };
    settle_in(&dir, n, max, GRACE);
}

/// `settle` for the cache at `dir`; whether it scanned.
fn settle_in(dir: &Path, written: u64, max: u64, grace: Duration) -> bool {
    let ledger = dir.join(LEDGER);
    let line = format!("+{written}\n");
    let appended = std::fs::File::options()
        .append(true)
        .create(true)
        .open(&ledger)
        .and_then(|mut f| f.write_all(line.as_bytes()));
    if appended.is_err() {
        return false;
    }
    if !due(&ledger) {
        return false;
    }
    let Some(_lock) = Lock::take(dir) else {
        return false;
    };
    let low = max / 100 * LOW;
    let r = evict(dir, None, Some((max, low)), grace);
    record(dir, r.size, max, low);
    true
}

/// The ledger calls for a scan: missing or unreadable, a day old, or its
/// total past the mark.
fn due(ledger: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(ledger) else {
        return true;
    };
    let mut lines = text.lines();
    let head: Vec<u64> = lines
        .next()
        .unwrap_or("")
        .split(' ')
        .filter_map(|w| w.parse().ok())
        .collect();
    let &[total, next, at] = head.as_slice() else {
        return true;
    };
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    if now.saturating_sub(at) > RESCAN.as_secs() {
        return true;
    }
    let added: u64 = lines
        .filter_map(|l| l.strip_prefix('+')?.parse::<u64>().ok())
        .fold(0, u64::saturating_add);
    total.saturating_add(added) > next
}

/// Start the ledger again from a scan that left `size` bytes: the next
/// scan when the cap is passed, or another `max - low` is written when
/// recent entries keep it over the low mark.
fn record(dir: &Path, size: u64, max: u64, low: u64) {
    let next = max.max(size.saturating_add(max - low));
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let ledger = dir.join(LEDGER);
    let tmp = dir.join(format!(".{LEDGER}.{}", std::process::id()));
    let text = format!("{size} {next} {now}\n");
    if std::fs::write(&tmp, text).is_err() || std::fs::rename(&tmp, &ledger).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// The scan lock of a cache: held by one process at a time.
struct Lock(PathBuf);

impl Lock {
    fn take(dir: &Path) -> Option<Self> {
        let path = dir.join(LOCK);
        for _ in 0..2 {
            if std::fs::File::options()
                .write(true)
                .create_new(true)
                .open(&path)
                .is_ok()
            {
                return Some(Self(path));
            }
            if age(&path).is_none_or(|a| a < STALE_LOCK) {
                return None;
            }
            let _ = std::fs::remove_file(&path);
        }
        None
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn age(path: &Path) -> Option<Duration> {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .map(|t| t.elapsed().unwrap_or_default())
}

/// What `prune` did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pruned {
    pub removed: usize,
    pub freed: u64,
    pub kept: usize,
    pub size: u64,
}

/// `nabi cache prune`: trim the cache at `dir`.
///
/// Removes the entries unused for `unused`, then, with `max`, the least
/// recently used ones until at most `max` bytes are left. Scratch files
/// of builds that died go too.
pub fn prune(dir: &Path, unused: Duration, max: Option<u64>) -> Pruned {
    let _lock = Lock::take(dir);
    let r = evict(dir, Some(unused), max.map(|m| (m, m)), Duration::ZERO);
    if let Some(m) = max_size() {
        record(dir, r.size, m, m / 100 * LOW);
    }
    r
}

/// An entry: its last use, size and path.
struct Entry {
    used: SystemTime,
    len: u64,
    path: PathBuf,
}

/// Remove what is unused for `unused`, then, past `max.0`, the least
/// recently used entries until at most `max.1` bytes are left; never an
/// entry used within `grace`.
fn evict(dir: &Path, unused: Option<Duration>, max: Option<(u64, u64)>, grace: Duration) -> Pruned {
    let mut out = Pruned::default();
    let mut entries = Vec::new();
    let now = SystemTime::now();
    let age = |t: SystemTime| now.duration_since(t).unwrap_or_default();
    for (path, m, scratch) in files(dir) {
        let used = m.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        let limit = if scratch { Some(SCRATCH) } else { unused };
        if limit.is_some_and(|l| age(used) > l) {
            if std::fs::remove_file(&path).is_ok() {
                out.removed += 1;
                out.freed += m.len();
            }
        } else if !scratch {
            entries.push(Entry {
                used,
                len: m.len(),
                path,
            });
        }
    }
    let mut size: u64 = entries.iter().map(|e| e.len).sum();
    if let Some((_, low)) = max.filter(|&(cap, _)| size > cap) {
        entries.sort_by_key(|e| e.used);
        entries.retain(|e| {
            let old = age(e.used) >= grace;
            if size > low && old && std::fs::remove_file(&e.path).is_ok() {
                size -= e.len;
                out.removed += 1;
                out.freed += e.len;
                false
            } else {
                true
            }
        });
    }
    out.kept = entries.len();
    out.size = size;
    out
}

/// The files the cap counts, and whether each is a scratch file: the
/// function cache's `fn/<compiler>/<kind>/<fan>/<key>` and the stages in
/// `library/`.
fn files(dir: &Path) -> Vec<(PathBuf, std::fs::Metadata, bool)> {
    let mut out = Vec::new();
    let list = |p: &Path| -> Vec<std::fs::DirEntry> {
        std::fs::read_dir(p)
            .map(|d| d.flatten().collect())
            .unwrap_or_default()
    };
    let dirs = |p: &Path| -> Vec<PathBuf> {
        list(p)
            .into_iter()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .map(|e| e.path())
            .collect()
    };
    for compiler in dirs(&dir.join("fn")) {
        for kind in dirs(&compiler) {
            for fan in dirs(&kind) {
                for e in list(&fan) {
                    let Ok(m) = e.metadata() else { continue };
                    if m.is_file() {
                        let scratch = e.file_name().to_string_lossy().starts_with('.');
                        out.push((e.path(), m, scratch));
                    }
                }
            }
        }
    }
    for e in list(&dir.join(crate::incr::LIBRARY_STAGES)) {
        let name = e.file_name();
        let name = name.to_string_lossy();
        if !(name.starts_with("library-") || name.starts_with("prepass-")) {
            continue;
        }
        let Ok(m) = e.metadata() else { continue };
        if m.is_file() {
            let scratch = !(name.ends_with(".nabicol") || name.ends_with(".nabiast"));
            out.push((e.path(), m, scratch));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(dir: &Path, rel: &str, len: usize, age_secs: u64) -> PathBuf {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, vec![7u8; len]).unwrap();
        let t = SystemTime::now() - Duration::from_secs(age_secs);
        std::fs::File::options()
            .append(true)
            .open(&p)
            .unwrap()
            .set_modified(t)
            .unwrap();
        p
    }

    #[test]
    fn sizes_read_with_units() {
        assert_eq!(parse_size("2G"), Some(2 << 30));
        assert_eq!(parse_size("64k"), Some(64 << 10));
        assert_eq!(parse_size("100"), Some(100));
        assert_eq!(parse_size("x"), None);
    }

    #[test]
    fn eviction_takes_the_least_recently_used_first() {
        let d = nabic_core::tmp::test_dir("cache-lru");
        let first = entry(&d, "fn/c0/obj/aa/aa01", 100, 5 * 3600);
        let second = entry(
            &d,
            "library/library-0000000000000001.nabicol",
            100,
            4 * 3600,
        );
        let third = entry(&d, "fn/c0/opt/cc/cc01", 100, 3 * 3600);
        let last = entry(&d, "fn/c0/obj/ee/ee01", 100, 2 * 3600);
        let r = evict(&d, None, Some((300, 200)), GRACE);
        assert_eq!((r.removed, r.kept, r.size), (2, 2, 200));
        assert!(!first.exists() && !second.exists() && third.exists() && last.exists());
        // Under the cap nothing goes.
        let r = evict(&d, None, Some((300, 100)), GRACE);
        assert_eq!((r.removed, r.size), (0, 200));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn recent_and_scratch_files_stay() {
        let d = nabic_core::tmp::test_dir("cache-grace");
        let old = entry(&d, "fn/c0/obj/aa/aa01", 100, 5 * 3600);
        let fresh = entry(&d, "fn/c0/obj/bb/bb01", 100, 60);
        let writing = entry(&d, "fn/c0/obj/bb/.bb02.123", 100, 60);
        let stage = entry(&d, "library/library-0000000000000002.tmp9.0", 100, 60);
        let dead = entry(&d, "fn/c0/obj/bb/.bb03.124", 100, 2 * 3600);
        let r = evict(&d, None, Some((0, 0)), GRACE);
        assert!(!old.exists() && !dead.exists());
        assert!(fresh.exists() && writing.exists() && stage.exists());
        assert_eq!((r.kept, r.size), (1, 100));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_cap_holds_with_hysteresis() {
        let d = nabic_core::tmp::test_dir("cache-cap");
        for i in 0..10 {
            entry(
                &d,
                &format!("fn/c0/obj/{i:02}/{i:02}ff"),
                100,
                7200 + i * 60,
            );
        }
        // No ledger yet: the first settle scans, 1000 > 500, down to 400.
        assert!(settle_in(&d, 100, 500, GRACE));
        let r = evict(&d, None, None, GRACE);
        assert_eq!((r.kept, r.size), (4, 400));
        // The next scan waits for the cap: 400 + 50 is under it.
        assert!(!settle_in(&d, 50, 500, GRACE));
        assert!(settle_in(&d, 200, 500, GRACE));
        // Entries kept by the grace: the mark moves past them.
        for i in 0..8 {
            entry(&d, &format!("fn/c0/opt/{i:02}/{i:02}ee"), 100, 0);
        }
        assert!(settle_in(&d, 800, 500, GRACE));
        let r = evict(&d, None, None, GRACE);
        assert_eq!(r.size, 800);
        assert!(!settle_in(&d, 50, 500, GRACE));
        assert!(settle_in(&d, 60, 500, GRACE));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn one_process_scans_at_a_time() {
        let d = nabic_core::tmp::test_dir("cache-lock");
        entry(&d, "fn/c0/obj/aa/aa01", 100, 5 * 3600);
        let held = Lock::take(&d).unwrap();
        assert!(Lock::take(&d).is_none());
        assert!(!settle_in(&d, 100, 0, GRACE));
        drop(held);
        // A lock left by a process that died is taken over.
        entry(&d, LOCK, 0, 3600);
        assert!(settle_in(&d, 100, 0, GRACE));
        assert!(!d.join(LOCK).exists());
        assert!(!d.join("fn/c0/obj/aa/aa01").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn readers_and_writers_race_eviction_safely() {
        let d = nabic_core::tmp::test_dir("cache-race");
        let c = crate::fncache::at(&d.join("fn"), "c0");
        let body = |i: usize| vec![u8::try_from(i % 251).unwrap(); 4096 + i];
        std::thread::scope(|s| {
            for t in 0..4 {
                let c = &c;
                s.spawn(move || {
                    for round in 0..50 {
                        for i in (t * 16)..(t * 16 + 16) {
                            let key = format!("{i:02x}{round:04x}");
                            c.put(crate::fncache::Kind::Object, &key, &body(i));
                            // A whole entry, or a miss after an eviction.
                            if let Some(got) = c.get(crate::fncache::Kind::Object, &key) {
                                assert_eq!(got, body(i));
                            }
                        }
                    }
                });
            }
            s.spawn(|| {
                for _ in 0..50 {
                    evict(&d, None, Some((0, 0)), Duration::ZERO);
                }
            });
        });
        // Nothing half-written was left in place.
        for (p, _, scratch) in files(&d) {
            assert!(!scratch, "{}", p.display());
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn an_open_entry_reads_whole_after_eviction() {
        let d = nabic_core::tmp::test_dir("cache-open");
        let p = entry(&d, "fn/c0/obj/aa/aa01", 1 << 16, 5 * 3600);
        let mut f = std::fs::File::open(&p).unwrap();
        let r = evict(&d, None, Some((0, 0)), Duration::ZERO);
        assert_eq!(r.removed, 1);
        let mut data = Vec::new();
        std::io::Read::read_to_end(&mut f, &mut data).unwrap();
        assert_eq!(data.len(), 1 << 16);
        drop(f);
        let _ = std::fs::remove_dir_all(&d);
    }
}
