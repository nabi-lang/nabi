//! Change detection by polling: the files under the watched trees and
//! single files, by modification time and size. Portable and needs no
//! FFI; a tick costs one `stat` per file.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// How often the trees are scanned.
pub(crate) const TICK: Duration = Duration::from_millis(100);
/// How long a change must stay still before it is taken.
pub(crate) const SETTLE: Duration = Duration::from_millis(50);

/// What a watched path stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// A source file of hot package `n`.
    Source(usize),
    /// A manifest or lock file.
    Manifest,
    /// A toolchain binary.
    Toolchain,
}

/// One file's state: modification time and length; `None` when gone.
type Stamp = Option<(SystemTime, u64)>;

pub(crate) struct Watcher {
    /// Directories scanned for `.nabi` files.
    trees: Vec<(PathBuf, Kind)>,
    /// Single files.
    files: Vec<(PathBuf, Kind)>,
    seen: HashMap<PathBuf, (Kind, Stamp)>,
}

/// The changes of one settled poll.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Changes {
    /// Hot packages with changed sources, by index, sorted.
    pub(crate) sources: Vec<usize>,
    /// Changed manifests or locks.
    pub(crate) manifests: Vec<PathBuf>,
    pub(crate) toolchain: bool,
}

impl Changes {
    pub(crate) fn is_empty(&self) -> bool {
        self.sources.is_empty() && self.manifests.is_empty() && !self.toolchain
    }
}

impl Watcher {
    pub(crate) fn new() -> Self {
        Self {
            trees: Vec::new(),
            files: Vec::new(),
            seen: HashMap::new(),
        }
    }

    pub(crate) fn tree(&mut self, dir: &Path, kind: Kind) {
        self.trees.push((dir.to_path_buf(), kind));
    }

    pub(crate) fn file(&mut self, file: &Path, kind: Kind) {
        self.files.push((file.to_path_buf(), kind));
    }

    /// The manifest and lock of directory `dir`, as they may appear.
    pub(crate) fn files_of(&mut self, dir: &Path) {
        self.file(&dir.join(crate::manifest::MANIFEST), Kind::Manifest);
        self.file(&dir.join(crate::manifest::LOCK), Kind::Manifest);
    }

    /// Take the current state as the base: later polls report what
    /// differs from it.
    pub(crate) fn arm(&mut self) {
        self.seen = self.scan();
    }

    /// What changed since the last poll, once it has been still for
    /// `SETTLE`; an editor's save of several files comes as one.
    pub(crate) fn poll(&mut self) -> Changes {
        let mut now = self.scan();
        if same(&self.seen, &now) {
            return Changes::default();
        }
        // Wait until two scans `SETTLE` apart agree, at most a second.
        let start = Instant::now();
        loop {
            std::thread::sleep(SETTLE);
            let next = self.scan();
            let still = same(&now, &next);
            now = next;
            if still || start.elapsed() > Duration::from_secs(1) {
                break;
            }
        }
        let c = diff(&self.seen, &now);
        self.seen = now;
        c
    }

    fn scan(&self) -> HashMap<PathBuf, (Kind, Stamp)> {
        let mut out = HashMap::new();
        for (dir, kind) in &self.trees {
            walk(dir, *kind, &mut out);
        }
        for (f, kind) in &self.files {
            out.insert(f.clone(), (*kind, stamp(f)));
        }
        out
    }
}

fn stamp(p: &Path) -> Stamp {
    let m = std::fs::metadata(p).ok()?;
    Some((m.modified().ok()?, m.len()))
}

/// The `.nabi` files under `dir`, skipping hidden directories and
/// `target`.
fn walk(dir: &Path, kind: Kind, out: &mut HashMap<PathBuf, (Kind, Stamp)>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name();
        let name = name.to_string_lossy();
        let Ok(t) = e.file_type() else {
            continue;
        };
        if t.is_dir() {
            if !name.starts_with('.') && name != "target" {
                walk(&p, kind, out);
            }
        } else if p.extension().is_some_and(|x| x == "nabi") {
            let s = stamp(&p);
            out.insert(p, (kind, s));
        }
    }
}

fn same(a: &HashMap<PathBuf, (Kind, Stamp)>, b: &HashMap<PathBuf, (Kind, Stamp)>) -> bool {
    a.len() == b.len() && a.iter().all(|(p, v)| b.get(p) == Some(v))
}

/// What differs between `old` and `new`: added, removed and changed files.
fn diff(old: &HashMap<PathBuf, (Kind, Stamp)>, new: &HashMap<PathBuf, (Kind, Stamp)>) -> Changes {
    let mut c = Changes::default();
    let changed = new
        .iter()
        .filter(|(p, v)| old.get(*p) != Some(v))
        .chain(old.iter().filter(|(p, _)| !new.contains_key(*p)));
    for (p, (kind, _)) in changed {
        match kind {
            Kind::Source(n) => c.sources.push(*n),
            Kind::Manifest => c.manifests.push(p.clone()),
            Kind::Toolchain => c.toolchain = true,
        }
    }
    c.sources.sort_unstable();
    c.sources.dedup();
    c.manifests.sort();
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(p: &Path, text: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    #[test]
    fn sees_edits_adds_and_removes() {
        let dir = nabic_core::tmp::test_dir("dev-watch");
        let main = dir.join("a/src/main.nabi");
        let lib = dir.join("b/src/lib.nabi");
        let m = dir.join("a/nabi.nabion");
        touch(&main, "fn main() {}\n");
        touch(&lib, "pub fn f() {}\n");
        touch(&m, "Package {}\n");
        touch(&dir.join("a/src/target/x.nabi"), "");
        let mut w = Watcher::new();
        w.tree(&dir.join("a/src"), Kind::Source(0));
        w.tree(&dir.join("b/src"), Kind::Source(1));
        w.file(&m, Kind::Manifest);
        w.arm();
        assert!(w.poll().is_empty());
        // A length change shows even within the clock's resolution.
        touch(&lib, "pub fn f() { }\n");
        touch(&dir.join("a/src/new.nabi"), "");
        assert_eq!(w.poll().sources, [0, 1]);
        assert!(w.poll().is_empty());
        std::fs::remove_file(dir.join("a/src/new.nabi")).unwrap();
        touch(&dir.join("a/src/target/x.nabi"), "ignored");
        assert_eq!(w.poll().sources, [0]);
        touch(&m, "Package { name: \"a\" }\n");
        let c = w.poll();
        assert_eq!(c.manifests, [m]);
        assert!(c.sources.is_empty() && !c.toolchain);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
