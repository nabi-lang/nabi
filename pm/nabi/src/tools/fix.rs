//! `nabi fix`: apply the edits that warnings and lints carry, across a
//! package, until none are left.

use nabic::{Args, discover_arg, fail, lib_name, prebuild, session};
use nabic_core::term;
use nabic_diagnostic::{NabiDiagnostic, Severity};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process;

/// Rounds before giving up: a fix may uncover another.
const ROUNDS: usize = 8;

/// One edit: replace bytes `start..end` with `text`.
struct Edit {
    start: usize,
    end: usize,
    text: String,
}

/// `nabi fix [path]`: refuses a work tree with uncommitted changes
/// unless `--allow-dirty`; `--diff` prints the first round's edits and
/// writes nothing.
pub(crate) fn cmd_fix(mut a: Args) {
    let pkg = discover_arg(&a);
    let dir = std::path::absolute(&pkg.dir).unwrap_or_else(|_| pkg.dir.clone());
    if !a.allow_dirty && !a.diff {
        if let Some(files) = uncommitted(&dir) {
            fail(&format!(
                "the work tree has uncommitted changes ({files}); commit them first so the fixes \
                 can be reviewed and undone, or run `nabi fix --allow-dirty`"
            ));
        }
    }
    prebuild(&mut a, Some(&pkg)).unwrap_or_else(|e| fail(&e));
    let mut fixed: BTreeMap<PathBuf, usize> = BTreeMap::new();
    for round in 0..ROUNDS {
        let (edits, errors) = collect(&mut a, &pkg, &dir);
        if errors > 0 {
            let changed = if fixed.is_empty() {
                "nothing was changed"
            } else {
                "the files fixed so far are kept"
            };
            fail(&format!(
                "{errors} error{} to fix by hand first; {changed}",
                if errors == 1 { "" } else { "s" }
            ));
        }
        if edits.is_empty() {
            break;
        }
        for (path, (text, edits)) in edits {
            let new = apply(&text, &edits);
            let shown = path
                .strip_prefix(&dir)
                .unwrap_or(&path)
                .display()
                .to_string();
            if a.diff {
                println!("Diff in {shown}:");
                print!("{}", nabic_tools::fmt::line_diff(&text, &new));
                continue;
            }
            if let Err(e) = std::fs::write(&path, new) {
                fail(&format!("cannot write {}: {e}", path.display()));
            }
            *fixed.entry(path).or_default() += edits.len();
        }
        if a.diff {
            return;
        }
        if round + 1 == ROUNDS {
            term::note("stopped after 8 rounds of fixes; run `nabi fix` again");
        }
    }
    if a.quiet {
        return;
    }
    if fixed.is_empty() {
        term::note(&format!("{}: nothing to fix", pkg.name));
    }
    for (path, n) in &fixed {
        let shown = path
            .strip_prefix(&dir)
            .unwrap_or(path)
            .display()
            .to_string();
        term::status(
            "Fixed",
            &format!("{shown} ({n} fix{})", if *n == 1 { "" } else { "es" }),
        );
    }
}

/// The package's applicable edits by file (with the text they apply to),
/// and the number of errors no edit resolves. Reports what it found.
fn collect(
    a: &mut Args,
    pkg: &nabic_interface::Package,
    dir: &Path,
) -> (BTreeMap<PathBuf, (String, Vec<Edit>)>, usize) {
    let mut out: BTreeMap<PathBuf, (String, Vec<Edit>)> = BTreeMap::new();
    let mut errors = 0;
    let externs = a.externs.len();
    for target in pkg.library_target().into_iter().chain([pkg.clone()]) {
        if let Some(lib) = target.library.as_ref() {
            a.externs.push((lib_name(&target), lib.clone()));
        }
        let mut s = session(a, &target);
        let (result, diags) =
            nabic_tools::analysis::check(&mut s, None, &target.root, &a.lint_levels, None);
        for d in &diags {
            if let Some((path, text, edit)) = fixable(d, &result.source_map, dir) {
                let slot = out.entry(path).or_insert_with(|| (text, Vec::new()));
                slot.1.push(edit);
            } else if d.is_error() && d.lint.is_none() {
                result.report(d);
                errors += 1;
            }
        }
        if errors > 0 {
            break;
        }
    }
    a.externs.truncate(externs);
    (out, errors)
}

/// The one edit of a warning or lint finding in a file of the package
/// at `dir`: a diagnostic with several alternatives is left to the
/// editor.
fn fixable(
    d: &NabiDiagnostic,
    files: &nabic_interface::SourceMap,
    dir: &Path,
) -> Option<(PathBuf, String, Edit)> {
    let [fix] = &*d.fixes else { return None };
    if d.severity != Severity::Warning && d.lint.is_none() {
        return None;
    }
    let (path, text) = files.file(fix.span.file)?;
    let path = std::path::absolute(path).ok()?;
    let (start, end) = (fix.span.start as usize, fix.span.end as usize);
    if !path.starts_with(dir) || start > end || !text.is_char_boundary(end) {
        return None;
    }
    // A file edited since the check is fixed next round.
    if std::fs::read_to_string(&path).ok()? != *text {
        return None;
    }
    let edit = Edit {
        start,
        end,
        text: fix.text.clone(),
    };
    Some((path, text.clone(), edit))
}

/// `text` with `edits` applied; one that overlaps an earlier one waits
/// for the next round.
fn apply(text: &str, edits: &[Edit]) -> String {
    let mut order: Vec<&Edit> = edits.iter().collect();
    order.sort_by_key(|e| (e.start, e.end));
    let (mut out, mut at) = (String::with_capacity(text.len()), 0);
    for e in order {
        if e.start < at || !text.is_char_boundary(e.start) {
            continue;
        }
        out.push_str(&text[at..e.start]);
        out.push_str(&e.text);
        at = e.end;
    }
    out.push_str(&text[at..]);
    out
}

/// What git reports as changed under `dir`, when it is in a repository
/// with uncommitted changes.
fn uncommitted(dir: &Path) -> Option<String> {
    let out = process::Command::new("git")
        .arg("-C")
        .arg(dir)
        // The build output is no source.
        .args(["status", "--porcelain", "--", ".", ":(exclude)target"])
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let lines: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.get(3..).unwrap_or(l).to_owned())
        .collect();
    match lines.as_slice() {
        [] => None,
        [one] => Some(one.clone()),
        [first, rest @ ..] => Some(format!("{first} and {} more", rest.len())),
    }
}

#[cfg(test)]
mod tests {
    use super::{Edit, apply};

    #[test]
    fn edits_apply_in_order_and_overlaps_wait() {
        let e = |start, end, text: &str| Edit {
            start,
            end,
            text: text.to_owned(),
        };
        let text = "while true { x == true }";
        let edits = [e(13, 22, "x"), e(0, 10, "loop"), e(15, 18, "!")];
        assert_eq!(apply(text, &edits), "loop { x }");
        assert_eq!(apply("abc", &[e(1, 1, "_")]), "a_bc");
    }
}
