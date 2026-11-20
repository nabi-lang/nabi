//! `nabi cache`: the toolchain's build cache, where nabic keeps the
//! standard library code it compiled (the function cache) and the
//! library's collected stages, for every build.

use nabic_core::term;
use nabic_interface::{cache, fncache};
use std::path::PathBuf;
use std::time::Duration;

const USAGE: &str = "usage: nabi cache dir | prune [--unused <days>] [--max-size <size>] | clean";

pub fn cmd(args: &[String], quiet: bool) -> Result<i32, String> {
    let dir = || {
        nabic_interface::cache_dir()
            .ok_or_else(|| "this toolchain has no cache directory".to_owned())
    };
    match args.first().map(String::as_str) {
        Some("dir") if args.len() == 1 => println!("{}", dir()?.display()),
        Some("prune") => prune(&dir()?, &args[1..], quiet)?,
        Some("clean") if args.len() == 1 => clean(&dir()?, quiet)?,
        _ => return Err(USAGE.into()),
    }
    Ok(0)
}

/// `prune`: the entries unused for `--unused` days (default 30), then
/// the least recently used past `--max-size` (default the cap,
/// `NABI_CACHE_MAX_SIZE`).
fn prune(dir: &std::path::Path, args: &[String], quiet: bool) -> Result<(), String> {
    let mut unused = fncache::UNUSED;
    let mut max = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = |name: &str| {
            it.next()
                .ok_or_else(|| format!("`{name}` needs a value"))
                .cloned()
        };
        match a.as_str() {
            "--unused" => {
                let v = value("--unused")?;
                let days: u64 = v.parse().map_err(|_| {
                    format!("`--unused {v}`: write a number of days, as in `--unused 7`")
                })?;
                unused = Duration::from_secs(days * 24 * 3600);
            }
            "--max-size" => {
                let v = value("--max-size")?;
                max = Some(cache::parse_size(&v).ok_or_else(|| {
                    format!("`--max-size {v}`: write bytes with K, M or G, as in `--max-size 2G`")
                })?);
            }
            other => return Err(format!("unknown option `{other}`\n{USAGE}")),
        }
    }
    let r = cache::prune(dir, unused, max.or_else(cache::max_size));
    if !quiet {
        term::status(
            "Pruned",
            &format!(
                "{} entries ({}), kept {} ({})",
                r.removed,
                mib(r.freed),
                r.kept,
                mib(r.size)
            ),
        );
    }
    Ok(())
}

/// `clean`: everything in the cache; the next build fills it again.
fn clean(dir: &std::path::Path, quiet: bool) -> Result<(), String> {
    let entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|d| d.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    for p in entries {
        let r = if p.is_dir() {
            std::fs::remove_dir_all(&p)
        } else {
            std::fs::remove_file(&p)
        };
        r.map_err(|e| format!("{}: {e}", p.display()))?;
    }
    if !quiet {
        term::status("Removed", &dir.display().to_string());
    }
    Ok(())
}

#[allow(clippy::cast_precision_loss)]
fn mib(bytes: u64) -> String {
    format!("{:.1} MiB", bytes as f64 / f64::from(1 << 20))
}
