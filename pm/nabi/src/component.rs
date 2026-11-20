//! `nabi component`: optional parts. `packages` is the registry archive
//! of the newest version (this toolchain builds) of every official
//! package, so they resolve and come in on use without a network. A
//! release toolchain carries them (`bundle`); `add` / `update` download
//! them into `$NABI_HOME`. Manifests and locks still choose the versions.
//!
//! ```text
//! $NABI_HOME/components.nabion   Components { packages: ["json@0.2.0", ..] }
//! ```

use crate::bundle::Bundle;
use crate::registry::Entry;
#[allow(clippy::wildcard_imports)]
use crate::*;

/// The components there are, and what each is.
const ALL: [(&str, &str); 1] = [(
    "packages",
    "the official packages' archives, for offline builds",
)];

fn record_file(home: &Path) -> PathBuf {
    home.join("components.nabion")
}

/// The archives `packages` holds (`name@version`), `None`: not added.
fn kept(home: &Path) -> Result<Option<Vec<String>>, String> {
    let path = record_file(home);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(None);
    };
    let file = path.display().to_string();
    let doc = nabic_nabion::Doc::parse(&text, &file)?;
    let r = doc.root().record("Components")?;
    let Some(list) = r.opt("packages") else {
        return Ok(None);
    };
    list.list()?
        .iter()
        .map(nabic_nabion::Decoder::string)
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

fn write_record(home: &Path, packages: Option<&[String]>) -> Result<(), String> {
    let path = record_file(home);
    let Some(p) = packages else {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(format!("cannot remove {}: {e}", path.display()))
            }
            _ => Ok(()),
        };
    };
    let v = Value::record(Some("Components"), vec![("packages", manifest::strings(p))]);
    let text = format!(
        "// components.nabion: written by `nabi component`.\n{}\n",
        nabic_nabion::to_string(&v)
    );
    std::fs::write(&path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// Download the archive of every official package's newest version; each
/// version kept and its files.
fn fetch_packages(g: &Global, home: &Path) -> Result<Vec<(Entry, PathBuf)>, String> {
    if g.offline {
        return Err(
            "`nabi component add packages` downloads the official packages; run it without \
             --offline"
                .to_owned(),
        );
    }
    let url = registry::DEFAULT;
    let index = registry::open(home, url, false, g.quiet)?;
    let api = registry::api_cached(&index, url);
    let star = semver::Req::parse("*")?;
    let mut out = Vec::new();
    let mut names = registry::names(&index, url, false)?;
    names.sort();
    for name in names {
        let all = registry::entries(&index, url, &name)?;
        if !registry::is_official(&all) {
            continue;
        }
        let (Some(e), _) = registry::best_here(&all, &star) else {
            continue;
        };
        let Some(sum) = &e.archive else {
            if !g.quiet {
                term::note(&format!(
                    "{name} {}: the registry keeps no archive of it; it is fetched from git when \
                     used",
                    e.version
                ));
            }
            continue;
        };
        match archive::fetch(home, api.as_deref(), e, sum, false, g.quiet) {
            Ok(dir) => out.push((e.clone(), dir)),
            Err(archive::Miss::Bad(m) | archive::Miss::Absent(m)) => {
                return Err(format!("{name} {}: {m}", e.version));
            }
        }
    }
    Ok(out)
}

/// Fetch the archives and record them; `verb` for the status line.
fn fill(g: &Global, home: &Path, verb: &str) -> Result<i32, String> {
    let got: Vec<String> = fetch_packages(g, home)?
        .iter()
        .map(|(e, _)| format!("{}@{}", e.name, e.version))
        .collect();
    write_record(home, Some(&got))?;
    if !g.quiet {
        term::status(
            verb,
            &format!("component packages ({} official packages)", got.len()),
        );
    }
    Ok(0)
}

/// Delete the archives of `kept` (`name@version`) from the cache; a build
/// that needs one downloads it again.
fn drop_archives(home: &Path, kept: &[String]) {
    let url = registry::DEFAULT;
    let Ok(sources) = registry::sources(home) else {
        return;
    };
    let index = git::Cache::index(home, sources);
    for k in kept {
        let Some((name, version)) = k.split_once('@') else {
            continue;
        };
        let all = registry::entries(&index, url, name).unwrap_or_default();
        for sum in all
            .iter()
            .filter(|e| e.version.to_string() == version)
            .filter_map(|e| e.archive.as_ref())
        {
            let _ = std::fs::remove_dir_all(archive::dir(home, sum));
        }
    }
}

/// `nabi component list | add <name> | remove <name> | update [<name>]`.
pub(crate) fn cmd(g: &Global, args: &[String]) -> Result<i32, String> {
    let home = home();
    let known = |n: &str| {
        if ALL.iter().any(|(c, _)| *c == n) {
            Ok(())
        } else {
            let list: Vec<&str> = ALL.iter().map(|(c, _)| *c).collect();
            Err(format!(
                "there is no component `{n}` (components: {})",
                list.join(", ")
            ))
        }
    };
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["list"] | [] => {
            let have = kept(&home)?;
            let bundled = Bundle::here().map(|b| b.len());
            let rows: Vec<(String, String)> = ALL
                .iter()
                .map(|(n, what)| {
                    let state = match (&have, bundled) {
                        (Some(p), Some(b)) => {
                            format!(
                                "added: {b} package(s) with the toolchain, {} downloaded",
                                p.len()
                            )
                        }
                        (None, Some(b)) => format!("added: {b} package(s) with the toolchain"),
                        (Some(p), None) => format!("added: {} package(s)", p.len()),
                        (None, None) => "not added".to_owned(),
                    };
                    ((*n).to_owned(), format!("{state}; {what}"))
                })
                .collect();
            print!("{}", term::list(&rows, None, ""));
            Ok(0)
        }
        ["add", n, "--bundle", out] => {
            known(n)?;
            let got = fetch_packages(g, &home)?;
            bundle::write(Path::new(out), &got)?;
            if !g.quiet {
                term::status(
                    "Bundled",
                    &format!("{} official packages into {out}", got.len()),
                );
            }
            Ok(0)
        }
        ["add", n] => {
            known(n)?;
            if kept(&home)?.is_some() || Bundle::here().is_some() {
                if !g.quiet {
                    term::note(
                        "the `packages` component is already added; `nabi component update \
                         packages` fetches newer versions",
                    );
                }
                return Ok(0);
            }
            fill(g, &home, "Added")
        }
        ["update"] => fill(g, &home, "Updated"),
        ["update", n] => {
            known(n)?;
            fill(g, &home, "Updated")
        }
        ["remove", n] => {
            known(n)?;
            let have = kept(&home)?;
            let bundled = Bundle::here();
            if have.is_none() && bundled.is_none() {
                return Err(
                    "the `packages` component is not added (add it: `nabi component add \
                     packages`)"
                        .to_owned(),
                );
            }
            if let Some(b) = bundled {
                std::fs::remove_dir_all(b.root()).map_err(|e| {
                    format!(
                        "cannot remove the toolchain's packages {}: {e}",
                        b.root().display()
                    )
                })?;
            }
            if let Some(have) = have {
                drop_archives(&home, &have);
                write_record(&home, None)?;
            }
            if !g.quiet {
                term::status("Removed", "component packages");
            }
            Ok(0)
        }
        _ => Err(
            "usage: nabi component list | add <name> [--bundle <dir>] | remove <name> | update \
             [<name>]"
                .to_owned(),
        ),
    }
}
