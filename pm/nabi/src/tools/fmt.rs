//! `nabi fmt`: format .nabi and .nabion files.

use nabic::{Args, fail};
use nabic_core::cli;
use nabic_core::term::{self, Stream};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process;
use std::time::Instant;

/// A walk's progress on a terminal, once it has taken a second.
pub(crate) struct Scanning {
    on: bool,
    start: Instant,
    last: Option<Instant>,
    width: usize,
}

impl Scanning {
    pub(crate) fn new(quiet: bool) -> Self {
        Self {
            on: !quiet && std::io::stderr().is_terminal(),
            start: Instant::now(),
            last: None,
            width: 0,
        }
    }

    pub(crate) fn tick(&mut self, n: usize) {
        let now = Instant::now();
        if !self.on
            || now - self.start < std::time::Duration::from_secs(1)
            || self
                .last
                .is_some_and(|l| now - l < std::time::Duration::from_millis(100))
        {
            return;
        }
        self.last = Some(now);
        let msg = format!("{n} files…");
        // The verb takes 12 columns.
        self.width = self.width.max(13 + msg.chars().count());
        let d = term::depth(Stream::Stderr);
        eprint!("\r{}", term::status_in(d, "Scanning", &msg));
    }

    pub(crate) fn clear(&mut self) {
        if self.width > 0 {
            eprint!("\r{}\r", " ".repeat(self.width));
            self.width = 0;
        }
    }
}

/// `n file(s)`.
pub(crate) fn files_n(n: usize) -> String {
    format!("{n} file{}", if n == 1 { "" } else { "s" })
}

/// What `nabi fmt` formats: each path given, a package's (`true`) or
/// not; without a path, the package the working directory is in.
pub(crate) fn fmt_roots(a: &Args) -> Vec<(PathBuf, bool)> {
    if !a.positional.is_empty() {
        return a
            .positional
            .iter()
            .map(|p| {
                (
                    PathBuf::from(p),
                    nabic_interface::root_file(Path::new(p)).is_some(),
                )
            })
            .collect();
    }
    let Some(pkg) = nabic_interface::package_root(Path::new(".")) else {
        fail("not inside a package; pass a file or directory: `nabi fmt <path>`");
    };
    // `..` steps up to it, so paths print short.
    let cwd = std::path::absolute(".").unwrap_or_default();
    let up = cwd.strip_prefix(&pkg).map_or(0, |r| r.components().count());
    let dir = if up == 0 {
        PathBuf::from(".")
    } else {
        std::iter::repeat_n("..", up).collect()
    };
    vec![(dir, true)]
}

/// The files under `roots`: a package's `src/`, `tests/`, `benches/`,
/// `examples/` and manifest, a directory's sources, or the file.
pub(crate) fn fmt_files(a: &Args, roots: &[(PathBuf, bool)]) -> Vec<PathBuf> {
    let mut scan = Scanning::new(a.quiet);
    let mut files = Vec::new();
    for (r, package) in roots {
        if *package || r.is_dir() {
            let mut found = Vec::new();
            if *package {
                for d in ["src", "tests", "benches", "examples"] {
                    found.extend(nabic_interface::source_files_with(&r.join(d), &mut |n| {
                        scan.tick(n);
                    }));
                }
                found.extend(Some(r.join("nabi.nabion")).filter(|m| m.is_file()));
            } else {
                found = nabic_interface::source_files_with(r, &mut |n| scan.tick(n));
            }
            scan.clear();
            if found.is_empty() && !a.quiet {
                term::note(&format!("no .nabi or .nabion files under {}", r.display()));
            }
            files.extend(found);
        } else if r.is_file() {
            files.push(r.clone());
        } else {
            scan.clear();
            fail(&format!("`{}` does not exist", r.display()));
        }
    }
    scan.clear();
    files
}

/// `nabi fmt`'s last line: what changed, or that nothing had to.
pub(crate) fn fmt_summary(a: &Args, total: usize, changed: usize, errors: usize) {
    let same = total - changed - errors;
    if a.quiet || total == 0 {
        return;
    }
    if changed > 0 && (a.check || a.diff) {
        let flag = if a.check { "--check" } else { "--diff" };
        let (need, them) = if changed == 1 {
            ("needs", "it")
        } else {
            ("need", "them")
        };
        term::note(&format!(
            "{} {need} formatting; `nabi fmt` without `{flag}` rewrites {them}",
            files_n(changed)
        ));
    } else if changed > 0 {
        let rest = if same > 0 {
            format!(", {same} already formatted")
        } else {
            String::new()
        };
        term::status("Formatted", &format!("{}{rest}", files_n(changed)));
    } else if errors == 0 {
        term::note(&format!("{} already formatted", files_n(same)));
    }
}

/// The operator tokens the files of the package `dir` lex: its own and
/// the exported ones of the dependencies resolvable offline from
/// `nabi.lock` (none for files outside a package); with why the
/// dependencies could not be read, when they could not.
fn package_ops(dir: &Path) -> (Vec<String>, Option<String>) {
    let Some(root) = nabic_interface::package_root(dir) else {
        return (Vec::new(), None);
    };
    let mut session = nabic_interface::Session::new();
    let opts = crate::resolve::Options {
        offline: true,
        locked: true,
        quiet: true,
        dev: true,
        ..Default::default()
    };
    let graph = crate::delegate::load_dir(&root)
        .and_then(|ctx| crate::resolve::resolve(&crate::home(), &ctx, &opts));
    let (packages, unread) = match graph {
        Ok(g) => (g.packages, None),
        Err(e) => (Vec::new(), Some(e)),
    };
    for p in packages {
        for n in std::iter::once(&p.code).chain(&p.aliases) {
            session.add_extern(crate::manifest::code_name(n), p.root.clone());
        }
    }
    let roots: Vec<PathBuf> = ["src/main.nabi", "src/lib.nabi"]
        .iter()
        .map(|f| root.join(f))
        .filter(|p| p.is_file())
        .collect();
    (session.operator_tokens(&roots), unread)
}

/// `nabi fmt [path..]`: see `fmt_roots` and `fmt_files`.
pub(crate) fn cmd_fmt(a: &Args) {
    let files = fmt_files(a, &fmt_roots(a));
    let (mut changed, mut errors) = (0, 0);
    let mut ops: std::collections::HashMap<Option<PathBuf>, (Vec<String>, Option<String>)> =
        std::collections::HashMap::new();
    for f in &files {
        let src = match std::fs::read_to_string(f) {
            Ok(s) => s,
            Err(e) => fail(&format!("{}: {e}", f.display())),
        };
        let dir = f.parent().unwrap_or_else(|| Path::new("."));
        let pkg = nabic_interface::package_root(dir);
        let (ops, unread) = ops.entry(pkg).or_insert_with(|| package_ops(dir));
        let formatted = match nabic_tools::fmt::format_file_with(f, &src, ops) {
            Ok(s) => s,
            Err(e) if e.starts_with("error[") => {
                eprintln!("{e}");
                // A dependency's operator is unknown while its source is
                // not there: say so once, and how to get it.
                if let Some(why) = unread.take().filter(|_| e.contains("registers it")) {
                    term::note(&format!(
                        "the dependencies' operators could not be read ({why}); run `nabi build` once, then `nabi fmt` again"
                    ));
                }
                errors += 1;
                continue;
            }
            Err(e) => {
                eprintln!("{}: cannot format {}: {e}", cli::invoked_as(), f.display());
                errors += 1;
                continue;
            }
        };
        if formatted == src {
            continue;
        }
        changed += 1;
        if a.diff || a.check {
            println!("Diff in {}:", f.display());
            if a.diff || !a.quiet {
                print!("{}", nabic_tools::fmt::line_diff(&src, &formatted));
            }
        }
        if !a.check && !a.diff {
            if let Err(e) = std::fs::write(f, formatted) {
                fail(&format!("cannot write {}: {e}", f.display()));
            }
            if a.verbose > 0 {
                term::status("Formatted", &f.display().to_string());
            }
        }
    }
    fmt_summary(a, files.len(), changed, errors);
    if errors > 0 || (a.check && changed > 0) {
        process::exit(1);
    }
}
