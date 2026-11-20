//! The official packages a toolchain carries: a read-only cache beside
//! its standard library, filled when the release archive is built.
//!
//! The default index's versions there are tried first when they meet the
//! requirements, so a build of them needs neither the index nor a
//! download. `nabi component remove packages` deletes it.
//!
//! ```text
//! <toolchain>/lib/nabi/packages/index          their index lines (one per version)
//! <toolchain>/lib/nabi/packages/src/<sha256>/  each version's files, as in cache/registry/src
//! ```

use crate::manifest;
use crate::registry::{self, Entry};
use std::path::{Path, PathBuf};

/// This toolchain's bundle directory: `lib/nabi/packages` of an installed
/// toolchain (one with `lib/nabi/core/src` beside `bin/`).
pub fn dir() -> Option<PathBuf> {
    let exe = nabic_core::current_exe()?;
    let root = exe.parent()?.parent()?;
    root.join("lib/nabi/core/src")
        .is_dir()
        .then(|| root.join("lib/nabi/packages"))
}

/// The packages a toolchain carries.
pub struct Bundle {
    root: PathBuf,
    entries: Vec<Entry>,
}

impl Bundle {
    /// The bundle at `root`; `None` when there is none or it does not read.
    pub fn at(root: &Path) -> Option<Self> {
        let file = root.join("index");
        let text = std::fs::read_to_string(&file).ok()?;
        let entries = registry::parse_file(&text, &file.display().to_string()).ok()?;
        Some(Self {
            root: root.to_path_buf(),
            entries,
        })
    }

    /// This toolchain's bundle, if it carries one.
    pub fn here() -> Option<Self> {
        Self::at(&dir()?)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The versions of `name` it holds (`_` and `-` alike).
    pub fn entries(&self, name: &str) -> Vec<Entry> {
        self.entries
            .iter()
            .filter(|e| manifest::same(&e.name, name))
            .cloned()
            .collect()
    }

    /// The files of `e` when the bundle holds its archive.
    pub fn src(&self, e: &Entry) -> Option<PathBuf> {
        let dir = self.root.join("src").join(e.archive.as_deref()?);
        dir.is_dir().then_some(dir)
    }
}

/// Write a bundle of `kept` (each version and its unpacked files) to
/// `out`, replacing what is there.
pub fn write(out: &Path, kept: &[(Entry, PathBuf)]) -> Result<(), String> {
    match std::fs::remove_dir_all(out) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            return Err(format!("cannot remove {}: {e}", out.display()));
        }
        _ => {}
    }
    let mut lines = String::new();
    for (e, files) in kept {
        let Some(sum) = &e.archive else {
            continue;
        };
        crate::toolchain::copy_tree(files, &out.join("src").join(sum))?;
        lines.push_str(&e.to_line());
        lines.push('\n');
    }
    std::fs::create_dir_all(out).map_err(|e| format!("{}: {e}", out.display()))?;
    let file = out.join("index");
    std::fs::write(&file, lines).map_err(|e| format!("cannot write {}: {e}", file.display()))
}
