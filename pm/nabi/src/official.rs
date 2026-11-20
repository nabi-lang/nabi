//! Official packages (the index marks them `official: true`) come in on
//! use: a build, run, check, test, bench, REPL or script whose code uses
//! `<name>::..` for a name it neither has as a dependency nor declares
//! gets `"<name>": "^x.y"` added to its manifest (a script: to its
//! `//! nabion` block) before the command goes on. Other names keep the
//! compiler's "not a dependency" error. `--locked` and CI (`CI` set)
//! never edit a manifest.

#[allow(clippy::wildcard_imports)]
use crate::*;

/// `CI` set and not empty: a manifest is never edited.
fn in_ci() -> bool {
    std::env::var_os("CI").is_some_and(|v| !v.is_empty())
}

/// The package an official code name `root` is (the index folds `_` and
/// `-`: `nabi_syntax` is `nabi-syntax`) and the requirement it is added with
/// (`^x.y` of the version the toolchain carries, else of the newest it
/// builds), from the toolchain's packages or the default index: the cached
/// one, fetched once when there is none. `None`: not official, or no index.
pub(crate) fn requirement(root: &str, offline: bool) -> Option<(String, String)> {
    let star = semver::Req::parse("*").ok()?;
    let carried = crate::bundle::Bundle::here().map(|b| b.entries(root));
    if let Some(all) = carried.filter(|a| registry::is_official(a)) {
        if let (Some(best), _) = registry::best_here(&all, &star) {
            return Some((best.name.clone(), caret(&best.version)));
        }
    }
    let home = home();
    let url = registry::DEFAULT;
    let index = git::Cache::index(&home, registry::sources(&home).ok()?);
    if !index.has(url) && (offline || index.fetch(url).is_err()) {
        return None;
    }
    let all = registry::entries(&index, url, root).ok()?;
    if !registry::is_official(&all) {
        return None;
    }
    let (best, _) = registry::best_here(&all, &star);
    let best = best?;
    Some((best.name.clone(), caret(&best.version)))
}

/// `^x.y`, or `^x.y.z` below 0.1.
fn caret(v: &semver::Version) -> String {
    if v.major > 0 || v.minor > 0 {
        format!("^{}.{}", v.major, v.minor)
    } else {
        format!("^{v}")
    }
}

/// The `.nabi` files under `dir`, recursively.
fn sources_in(dir: &Path, out: &mut Vec<String>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            sources_in(&p, out);
        } else if p.extension().is_some_and(|x| x == "nabi") {
            if let Ok(t) = std::fs::read_to_string(&p) {
                out.push(t);
            }
        }
    }
}

/// The names the code of `package` (or `script`) uses as path roots that
/// its manifest does not have.
fn wanted(package: Option<&Path>, script: Option<&Path>) -> Vec<String> {
    let (texts, have): (Vec<String>, Vec<String>) = if let Some(dir) = package {
        let mut texts = Vec::new();
        for sub in ["src", "tests", "benches"] {
            sources_in(&dir.join(sub), &mut texts);
        }
        let m = manifest::find(dir).and_then(|p| manifest::load(&p).ok());
        let mut have: Vec<String> = m
            .iter()
            .flat_map(|m| m.deps.iter().chain(&m.dev_deps).map(|d| d.name.clone()))
            .collect();
        have.extend(m.and_then(|m| m.name));
        (texts, have)
    } else if let Some(file) = script {
        let files = script_files(file);
        let mut have = Vec::new();
        for (_, src) in &files {
            if let Some(m) = manifest::script_block(src)
                .and_then(|b| manifest::parse(&b.text, "nabi.nabion").ok())
            {
                have.extend(m.deps.into_iter().map(|d| d.name));
            }
        }
        (files.into_iter().map(|(_, t)| t).collect(), have)
    } else {
        return Vec::new();
    };
    let have: Vec<String> = have.iter().map(|n| manifest::code_name(n)).collect();
    nabic_tools::roots::missing_roots(texts.iter().map(String::as_str))
        .into_iter()
        .filter(|n| !have.contains(n))
        .collect()
}

/// Add each official package the code of `package` / `script` uses and
/// lacks, before command `cmd` runs; nothing when `skip`.
pub(crate) fn add_used(
    g: &Global,
    cmd: &str,
    skip: bool,
    package: Option<&Path>,
    script: Option<&Path>,
) -> Result<(), String> {
    if skip
        || !matches!(
            cmd,
            "build" | "run" | "check" | "test" | "bench" | "repl" | "jupyter"
        )
    {
        return Ok(());
    }
    for root in wanted(package, script) {
        let Some((name, req)) = requirement(&root, g.offline) else {
            continue;
        };
        let target = script.map_or_else(
            || "nabi.nabion".to_owned(),
            |s| format!("the `//! nabion` block of {}", shown(s)),
        );
        let add = match script {
            Some(s) => format!("nabi add {name} {}", shown(s)),
            None => format!("nabi add {name}"),
        };
        if g.locked || in_ci() {
            let why = if g.frozen {
                "--frozen"
            } else if g.locked {
                "--locked"
            } else {
                "CI (`CI` is set)"
            };
            return Err(format!(
                "`{name}` is an official package the code uses, but it is not in {target}, and \
                 {why} never changes it; add it: `{add}`"
            ));
        }
        let value = format!("\"{req}\"");
        let r = match (script, package) {
            (Some(file), _) => edit_script(g, file, |old, label| {
                manifest::set_dep(old, label, "deps", &name, &value).map(Some)
            })
            .map(drop),
            (None, Some(dir)) => {
                let path = manifest::find(dir).unwrap_or_else(|| dir.join(MANIFEST));
                let old = std::fs::read_to_string(&path).ok();
                manifest::set_dep(
                    old.as_deref().unwrap_or(manifest::EMPTY),
                    &path.display().to_string(),
                    "deps",
                    &name,
                    &value,
                )
                .and_then(|text| write_and_resolve(g, &path, old.as_deref(), &text).map(drop))
            }
            (None, None) => Ok(()),
        };
        r.map_err(|e| {
            if g.offline {
                format!(
                    "`{name}` is an official package the code uses, but it is not in the cache \
                     and --offline is set: run `{add}` online, or keep the official packages \
                     for offline use with `nabi component add packages`\n({e})"
                )
            } else {
                e
            }
        })?;
        if !g.quiet {
            term::status("Added", &format!("{name} {req} (official)"));
        }
    }
    Ok(())
}
