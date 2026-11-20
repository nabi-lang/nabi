//! `layout`: checks the file and module layout of the Rust and Nabi
//! sources under a tree.
//!
//! - A: no `mod.rs` / `mod.nabi`; `#[path]` modules are listed.
//! - B: no `_` in file or directory names, except a crate directory
//!   named after its package.
//! - D: the header order `pub mod`, `mod`, `pub use`, `use` at the top of
//!   each file and inline module, one blank line between groups, none
//!   inside one; no `use` inside an item body.
//! - E: `crate::` / `pkg::` paths, never `super::` / `sup::` (nor
//!   `pub(super)` / `pub(sub)`) outside an inline `mod x { }`.
//!
//! `layout [--deny <rules>] [--quiet] [<dir>...]` (default: the repository)
//! prints each violation and a summary; it fails only on a rule `--deny`
//! names. `exempt.txt` beside this crate lists the exempt paths.

mod check;
mod lex;
#[cfg(test)]
mod tests;

use crate::check::{Lang, check};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// One violation in one file.
pub struct Violation {
    pub rule: char,
    /// Relative to the checked root.
    pub path: String,
    pub line: usize,
    pub kind: String,
    pub detail: String,
}

/// `RULES PATH` lines: `PATH` (a file, or a directory ending in `/`) is
/// exempt from each rule letter in `RULES`.
const EXEMPT: &str = include_str!("../exempt.txt");

fn exempt(rel: &str, rule: char) -> bool {
    EXEMPT
        .lines()
        .map(|l| l.split('#').next().unwrap_or("").trim())
        .filter_map(|l| l.split_once(char::is_whitespace))
        .any(|(rules, path)| {
            let path = path.trim();
            rules.contains(rule)
                && if path.ends_with('/') {
                    rel.starts_with(path)
                } else {
                    rel == path
                }
        })
}

fn lang_of(name: &str) -> Option<Lang> {
    match Path::new(name).extension()?.to_str()? {
        "rs" => Some(Lang::Rust),
        "nabi" => Some(Lang::Nabi),
        _ => None,
    }
}

fn skip_dir(name: &str) -> bool {
    name == "target" || name.starts_with('.') || name == "node_modules"
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = rd.filter_map(Result::ok).collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for e in entries {
        let p = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if p.is_dir() {
            if !skip_dir(&name) {
                walk(&p, out);
            }
        } else if lang_of(&name).is_some() {
            out.push(p);
        }
    }
}

/// Every violation under `root`, paths relative to the repository root
/// (or to `root` outside the repository).
#[must_use]
pub fn scan(root: &Path) -> Vec<Violation> {
    // Paths inside the repository are named from its root, so the
    // exemptions hold whichever directory is checked.
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let mut files = Vec::new();
    walk(&root, &mut files);
    let base = match repo_root().canonicalize() {
        Ok(r) if root.starts_with(&r) => r,
        _ => root,
    };
    let mut out = Vec::new();
    let mut dirs = Vec::new();
    let mut dirs_seen = std::collections::BTreeSet::new();
    for p in files {
        let rel = p
            .strip_prefix(&base)
            .unwrap_or(&p)
            .to_string_lossy()
            .replace('\\', "/");
        let mut push = |rule: char, line: usize, kind: &str, detail: String| {
            if !exempt(&rel, rule) {
                out.push(Violation {
                    rule,
                    path: rel.clone(),
                    line,
                    kind: kind.to_owned(),
                    detail,
                });
            }
        };
        let name = rel.rsplit('/').next().unwrap_or(&rel).to_owned();
        if name == "mod.rs" || name == "mod.nabi" {
            push('A', 0, "mod-file", String::new());
        }
        let stem = name.split('.').next().unwrap_or("");
        if stem.contains('_') {
            push('B', 0, "file-underscore", String::new());
        }
        let mut dir = String::new();
        for part in rel
            .split('/')
            .rev()
            .skip(1)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
        {
            dir = if dir.is_empty() {
                part.to_owned()
            } else {
                format!("{dir}/{part}")
            };
            if part.contains('_')
                && dirs_seen.insert(dir.clone())
                && !crate_dir(&base.join(&dir), part)
                && !exempt(&format!("{dir}/"), 'B')
            {
                dirs.push(Violation {
                    rule: 'B',
                    path: format!("{dir}/"),
                    line: 0,
                    kind: "dir-underscore".to_owned(),
                    detail: String::new(),
                });
            }
        }
        let Some(lang) = lang_of(&name) else {
            continue;
        };
        let src = match std::fs::read(&p) {
            Ok(b) => String::from_utf8_lossy(&b).into_owned(),
            Err(e) => {
                push('X', 0, "read-error", e.to_string());
                continue;
            }
        };
        for f in check(&src, lang) {
            push(f.rule, f.line, &f.kind, f.detail);
        }
    }
    out.extend(dirs);
    out
}

/// Counts per rule and kind.
fn summary(v: &[Violation]) -> String {
    let mut by: BTreeMap<(char, &str), usize> = BTreeMap::new();
    for x in v {
        *by.entry((x.rule, x.kind.as_str())).or_default() += 1;
    }
    let mut s = String::new();
    for ((r, k), n) in &by {
        s.push_str(&format!("{r} {k}: {n}\n"));
    }
    s.push_str(&format!("total: {}\n", v.len()));
    s
}

/// A crate directory keeps its package name, `_` and all.
fn crate_dir(dir: &Path, name: &str) -> bool {
    let Ok(toml) = std::fs::read_to_string(dir.join("Cargo.toml")) else {
        return false;
    };
    let want = format!("name = \"{name}\"");
    toml.lines().any(|l| l.trim() == want)
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn main() -> ExitCode {
    let mut deny = String::new();
    let mut quiet = false;
    let mut roots = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--deny" => deny = args.next().unwrap_or_default(),
            "--quiet" => quiet = true,
            "-h" | "--help" => {
                println!("usage: layout [--deny <rules>] [--quiet] [<dir>...]");
                return ExitCode::SUCCESS;
            }
            _ => roots.push(PathBuf::from(a)),
        }
    }
    if roots.is_empty() {
        roots.push(repo_root());
    }
    let mut all = Vec::new();
    for root in &roots {
        all.extend(scan(root));
    }
    if !quiet {
        for v in &all {
            let detail = if v.detail.is_empty() {
                String::new()
            } else {
                format!(": {}", v.detail)
            };
            println!("{}:{}: {} {}{detail}", v.path, v.line, v.rule, v.kind);
        }
    }
    print!("{}", summary(&all));
    if all.iter().any(|v| deny.contains(v.rule)) {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
