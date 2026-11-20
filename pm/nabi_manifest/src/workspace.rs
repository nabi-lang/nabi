//! Workspaces: packages developed together under one root whose
//! `nabi.nabion` holds `Workspace { members: [..] }` instead of a package.
//!
//! ```nabion
//! Workspace {
//!     members: ["json", "table", "tools/*"],   // directories; `dir/*`: each package in dir
//! }
//! ```
//!
//! The members share one `nabi.lock` and one `target/` at the root: their
//! dependencies are resolved together, so every member builds against the
//! same versions. A member is found by walking up from its directory to a
//! root that lists it. `-p <name>` runs a command on one member,
//! `--workspace` (or the command at the root) on each.

use crate::manifest::{self, MANIFEST};
use nabic_nabion::{Doc, Kind};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Workspace {
    pub root: PathBuf,
    pub members: Vec<Member>,
}

#[derive(Clone, Debug)]
pub struct Member {
    pub name: String,
    pub dir: PathBuf,
}

/// The commands `-p` / `--workspace` apply to.
pub const COMMANDS: [&str; 9] = [
    "build", "check", "test", "bench", "lint", "fmt", "doc", "clean", "run",
];

/// The member patterns of manifest text, `None` when it is not a
/// workspace root.
pub fn members_of(text: &str, file: &str) -> Result<Option<Vec<String>>, String> {
    let doc = Doc::parse(text, file)?;
    if !named(&doc) {
        return Ok(None);
    }
    let w = doc.root().record("Workspace")?;
    w.only(&["members"])?;
    let Some(list) = w.opt("members") else {
        return Err(format!(
            "{file}: a workspace lists its packages: `Workspace {{ members: [\"app\", \"lib\"] }}`"
        ));
    };
    let mut out = Vec::new();
    for d in list.list()? {
        let m = d.string()?;
        let star = m.contains('*');
        let dir_star = m.ends_with("/*") && !m[..m.len() - 2].contains('*');
        let bad = m.is_empty()
            || Path::new(&m).is_absolute()
            || m.split('/').any(|s| s == "..")
            || (star && !dir_star);
        if bad {
            return Err(d.error(&format!(
                "`{m}` is not a member: write a directory under the workspace root, or `dir/*` \
                 for each package in `dir`"
            )));
        }
        out.push(m);
    }
    Ok(Some(out))
}

/// Whether the value is spelled `Workspace { .. }` (a package's name may
/// be left out, a workspace's not).
pub fn named(doc: &Doc<'_>) -> bool {
    matches!(&doc.root().value().kind, Kind::Struct { name: Some(n), .. } if n == "Workspace")
}

/// Whether `dir`'s manifest is a workspace root.
pub fn is_root(dir: &Path) -> bool {
    std::fs::read_to_string(dir.join(MANIFEST))
        .ok()
        .and_then(|t| members_of(&t, MANIFEST).ok().flatten())
        .is_some()
}

/// The workspace whose root is `root`, if its manifest is one.
pub fn load(root: &Path) -> Result<Option<Workspace>, String> {
    let path = root.join(MANIFEST);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(None);
    };
    let file = path.display().to_string();
    let Some(patterns) = members_of(&text, &file)? else {
        return Ok(None);
    };
    let real = |p: &Path| nabic_core::real_path(p).unwrap_or_else(|_| p.to_path_buf());
    let root = real(root);
    let mut dirs: Vec<PathBuf> = Vec::new();
    for m in &patterns {
        if let Some(parent) = m.strip_suffix("/*") {
            let at = root.join(parent);
            let rd = std::fs::read_dir(&at)
                .map_err(|e| format!("{file}: member `{m}`: cannot read {}: {e}", at.display()))?;
            let mut found: Vec<PathBuf> = rd
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.join(MANIFEST).is_file())
                .collect();
            found.sort();
            dirs.extend(found);
        } else {
            let d = root.join(m);
            if !d.join(MANIFEST).is_file() {
                return Err(format!(
                    "{file}: member `{m}` has no {MANIFEST} ({}); create the package with \
                     `nabi new {m}`, or remove it from `members`",
                    d.display()
                ));
            }
            dirs.push(d);
        }
    }
    let mut members: Vec<Member> = Vec::new();
    for d in dirs {
        let d = real(&d);
        if members.iter().any(|m| m.dir == d) {
            continue;
        }
        let m = manifest::load(&d.join(MANIFEST))?;
        let Some(name) = m.name else {
            return Err(format!(
                "workspace member {} has no `name`; give it one: `Package {{ name: \"..\", .. }}`",
                d.join(MANIFEST).display()
            ));
        };
        if let Some(o) = members.iter().find(|o| manifest::same(&o.name, &name)) {
            return Err(format!(
                "workspace members {} and {} are both named `{name}`; rename one",
                o.dir.display(),
                d.display()
            ));
        }
        members.push(Member { name, dir: d });
    }
    Ok(Some(Workspace { root, members }))
}

/// The workspace `dir` (a package directory) is a member of.
pub fn of(dir: &Path) -> Result<Option<Workspace>, String> {
    let dir = nabic_core::real_path(dir).unwrap_or_else(|_| dir.to_path_buf());
    for up in dir.ancestors().skip(1) {
        if !is_root(up) {
            continue;
        }
        let ws = load(up)?.filter(|w| w.members.iter().any(|m| m.dir == dir));
        if ws.is_some() {
            return Ok(ws);
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn members() {
        let m = members_of("Workspace { members: [\"a\", \"libs/*\"] }", "f").unwrap();
        assert_eq!(m.unwrap(), ["a", "libs/*"]);
        assert!(
            members_of("Package { name: \"a\" }", "f")
                .unwrap()
                .is_none()
        );
        let e = members_of("Workspace { members: [\"../x\"] }", "f").unwrap_err();
        assert!(
            e.contains("write a directory under the workspace root"),
            "{e}"
        );
        assert!(members_of("Workspace { members: [\"a*b\"] }", "f").is_err());
        assert!(
            members_of("Workspace {}", "f")
                .unwrap_err()
                .contains("members: [\"app\"")
        );
    }
}
