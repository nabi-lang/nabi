//! `nabi add` and `nabi remove`: editing dependencies.

#[allow(clippy::wildcard_imports)]
use crate::*;

/// `nabi add` arguments and `--dev`. A registry package without `@req`
/// gets an empty `req` for the caller to fill.
pub fn parse_dep_args(args: &[String]) -> Result<(Dep, bool), String> {
    let mut name = None;
    let (mut path, mut git, mut reference, mut dev) = (None, None, GitRef::Default, false);
    let mut registry = None;
    let (mut features, mut optional, mut default_features) = (Vec::new(), false, true);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let (flag, inline) = match a.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_owned())),
            _ => (a.as_str(), None),
        };
        let mut value = || {
            inline
                .clone()
                .or_else(|| it.next().cloned())
                .ok_or_else(|| format!("`{flag}` needs a value"))
        };
        match flag {
            "--path" => path = Some(value()?),
            "--git" => git = Some(value()?),
            "--rev" | "--tag" | "--branch" if reference != GitRef::Default => {
                return Err("give at most one of --rev, --tag, --branch".into());
            }
            "--rev" => reference = GitRef::Rev(value()?),
            "--tag" => reference = GitRef::Tag(value()?),
            "--branch" => reference = GitRef::Branch(value()?),
            "--dev" => dev = true,
            "--registry" => registry = Some(value()?),
            "--features" => {
                for f in split_features(&value()?) {
                    manifest::check_feature(&f)?;
                    features.push(f);
                }
            }
            "--no-default-features" => default_features = false,
            "--optional" => optional = true,
            f if f.starts_with('-') => return Err(format!("unknown option `{f}`")),
            n if name.is_none() => name = Some(n.to_owned()),
            n => return Err(format!("unexpected argument `{n}`")),
        }
    }
    let usage = "usage: nabi add <name>[@<req>] [--registry <url>] | <name> --path <dir> | \
                 <name> --git <url> [--rev|--tag|--branch <r>]  [--dev] [--features <f,..>] \
                 [--no-default-features] [--optional]";
    if optional && dev {
        return Err("a dev dependency cannot be --optional".into());
    }
    // `nabi install --path / --git` may leave the name to the package.
    let name = match name {
        Some(n) => n,
        None if path.is_some() || git.is_some() => String::new(),
        None => return Err(usage.into()),
    };
    let (name, req) = match name.split_once('@') {
        Some((n, r)) => (n.to_owned(), Some(r.to_owned())),
        None => (name, None),
    };
    if nabic_core::TIERS.contains(&manifest::code_name(&name).as_str()) {
        return Err(format!(
            "`{name}` is the standard library, which ships with the toolchain: there is \
             nothing to add; `use {name}::..` works as it is"
        ));
    }
    if !name.is_empty() {
        manifest::check_new_name(&name)?;
    }
    let source = match (path, git) {
        (None, None) if reference == GitRef::Default => {
            let req = req.unwrap_or_default();
            if !req.is_empty() {
                semver::Req::parse(&req)?;
            }
            Source::Registry { req, registry }
        }
        _ if req.is_some() || registry.is_some() => {
            return Err(
                "`@<req>` / --registry go with a registry package, not --path / --git".into(),
            );
        }
        (Some(p), None) if reference == GitRef::Default => Source::Path(p),
        (None, Some(url)) => Source::Git { url, reference },
        (_, None) => return Err("--rev / --tag / --branch go with --git".into()),
        _ => return Err(usage.into()),
    };
    Ok((
        Dep {
            optional,
            features,
            default_features,
            ..Dep::new(name, source)
        },
        dev,
    ))
}

/// The manifest to edit, created if missing: the package's for `add` /
/// `remove` (`env: None`), environment `env`'s for `install` / `uninstall`.
pub(crate) fn manifest_to_edit(env: Option<&str>) -> Result<(PathBuf, bool), String> {
    if let Some(name) = env {
        let dir = env_dir(name);
        if !dir.is_dir() {
            env::create(name)?;
        }
        return Ok((dir.join(MANIFEST), true));
    }
    let dir = package_dir(Path::new(".")).ok_or(
        "not inside a package (no src/main.nabi, src/lib.nabi or nabi.nabion here or above): make \
         one with `nabi init`; for a script, name it: `nabi add <name> <file.nabi>`",
    )?;
    if workspace::is_root(&dir) {
        return Err(format!(
            "{} is a workspace root (`Workspace {{ .. }}`), not a package: run this in a \
             member's directory",
            dir.join(MANIFEST).display()
        ));
    }
    Ok((
        manifest::find(&dir).unwrap_or_else(|| dir.join(MANIFEST)),
        false,
    ))
}

/// `a` relative to directory `from` (both absolute), with `..` steps.
pub(crate) fn relative(from: &Path, a: &Path) -> PathBuf {
    let f: Vec<_> = from.components().collect();
    let t: Vec<_> = a.components().collect();
    let common = f.iter().zip(&t).take_while(|(x, y)| x == y).count();
    let mut out = PathBuf::new();
    for _ in common..f.len() {
        out.push("..");
    }
    for c in &t[common..] {
        out.push(c);
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

/// Write `text` and resolve; restore `old` if resolution fails.
pub(crate) fn write_and_resolve(
    g: &Global,
    path: &Path,
    old: Option<&str>,
    text: &str,
) -> Result<resolve::Graph, String> {
    let text = canonical_edit(old.unwrap_or(manifest::EMPTY), text);
    std::fs::write(path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let r = load_dir(dir).and_then(|ctx| resolve::resolve(&home(), &ctx, &options(g)));
    r.or_else(|e| {
        match old {
            Some(o) => std::fs::write(path, o),
            None => std::fs::remove_file(path),
        }
        .map_err(|e2| format!("{e}\n(and cannot restore {}: {e2})", path.display()))?;
        Err(e)
    })
}

/// The `version` in the nabi.nabion of resolved dependency `name`.
pub(crate) fn dep_version(graph: &resolve::Graph, name: &str) -> Option<String> {
    let p = graph
        .packages
        .iter()
        .find(|p| manifest::same(&p.name, name))?;
    // `<dir>/src/lib.nabi`
    let dir = p.root.parent()?.parent()?;
    manifest::load(&manifest::find(dir)?).ok()?.version
}

/// `--native <name>[@<req>] [--registry <url>]` into the package's,
/// environment `env`'s or `script`'s `native`.
pub(crate) fn add_native(
    g: &Global,
    args: &[String],
    env: Option<&str>,
    script: Option<&Path>,
) -> Result<i32, String> {
    let (vals, _, pos) = flags(args, &["--registry"], &["--native"])?;
    let [spec] = pos.as_slice() else {
        return Err("usage: nabi add --native <name>[@<req>] [--registry <url>]".into());
    };
    let (name, req) = spec.split_once('@').unwrap_or((spec, ""));
    manifest::check_new_name(name)?;
    let registry = val(&vals, "--registry").map(str::to_owned);
    let req = if req.is_empty() {
        let url = match &registry {
            Some(r) => r.clone(),
            None => registry::DEFAULT.to_owned(),
        };
        let index = registry::open(&home(), &url, g.offline, g.quiet)?;
        let all = native::entries(&index, &url, name)?;
        let e = native::best(&all, &semver::Req::parse("*")?)
            .ok_or_else(|| native::no_match(&index, &all, name, "*", &url))?;
        format!("^{}", e.version)
    } else {
        semver::Req::parse(req)?;
        req.to_owned()
    };
    let source = NativeSource::Registry { req, registry };
    if let Some(file) = script {
        let value = nabic_nabion::to_line(&source.to_value());
        edit_script(g, file, |old, label| {
            manifest::set_dep(old, label, "native", name, &value).map(Some)
        })?;
        if !g.quiet {
            term::status("Added", &format!("{name} to native of {}", file.display()));
        }
        return Ok(0);
    }
    let (path, _) = manifest_to_edit(env)?;
    let old = std::fs::read_to_string(&path).ok();
    let text = manifest::set_dep(
        old.as_deref().unwrap_or(manifest::EMPTY),
        &path.display().to_string(),
        "native",
        name,
        &nabic_nabion::to_line(&source.to_value()),
    )?;
    write_and_resolve(g, &path, old.as_deref(), &text)?;
    if !g.quiet {
        term::status("Added", &format!("{name} to native of {}", path.display()));
    }
    Ok(0)
}

/// `nabi add` / `remove` work on a package; `--env` is `nabi install`'s.
pub(crate) fn package_only(g: &Global, cmd: &str, instead: &str) -> Result<(), String> {
    match &g.env {
        Some(e) => Err(format!(
            "`nabi {cmd}` edits a package's nabi.nabion or a script's block; for the libraries the \
             REPL / Jupyter see in environment `{e}`, run `nabi {instead} --env {e} <name>`"
        )),
        None => Ok(()),
    }
}

pub(crate) fn cmd_add(g: &Global, args: &[String]) -> Result<i32, String> {
    package_only(g, "add", "install")?;
    let (script, args) = script_arg(args)?;
    if args.iter().any(|a| a == "--native") {
        return add_native(g, &args, None, script.as_deref());
    }
    let (dep, dev) = parse_dep_args(&args)?;
    if dep.name.is_empty() {
        return Err("usage: nabi add <name> --path <dir> | <name> --git <url> ..".into());
    }
    match script {
        Some(file) => add_to_script(g, dep, dev, &file),
        None => add_dep(g, dep, dev, None),
    }
}

/// The options of `nabi add` / `remove` that take a value.
pub(crate) const DEP_VALUE_OPTIONS: [&str; 7] = [
    "--path",
    "--git",
    "--rev",
    "--tag",
    "--branch",
    "--registry",
    "--features",
];

/// Split off the script argument (a last `.nabi` that isn't an option
/// value).
pub(crate) fn script_arg(args: &[String]) -> Result<(Option<PathBuf>, Vec<String>), String> {
    let Some((last, rest)) = args.split_last().filter(|(l, r)| {
        Path::new(l.as_str())
            .extension()
            .is_some_and(|x| x == "nabi")
            && r.last()
                .is_none_or(|p| !DEP_VALUE_OPTIONS.contains(&p.as_str()))
    }) else {
        return Ok((None, args.to_vec()));
    };
    let file = PathBuf::from(last);
    if !file.is_file() {
        return Err(format!("`{last}` does not exist"));
    }
    if let Some(dir) = package_dir(&file) {
        return Err(format!(
            "`{last}` belongs to the package in {}, which uses its nabi.nabion: run the command \
             there without `{last}`",
            dir.display(),
        ));
    }
    Ok((Some(file), rest.to_vec()))
}

/// Edit `file`'s `//! ```nabion` block with `edit` (`None`: no change) and
/// resolve, restoring the file on failure; an emptied block is removed.
/// Whether it changed.
pub(crate) fn edit_script(
    g: &Global,
    file: &Path,
    edit: impl FnOnce(&str, &str) -> Result<Option<String>, String>,
) -> Result<bool, String> {
    let src = std::fs::read_to_string(file).map_err(|e| format!("{}: {e}", file.display()))?;
    let old = manifest::script_block(&src).map(|b| b.text);
    let old = old.as_deref().unwrap_or(manifest::EMPTY);
    let label = format!("{} (//! nabion block)", file.display());
    let Some(text) = edit(old, &label)? else {
        return Ok(false);
    };
    let text = canonical_edit(old, &text);
    let m = manifest::parse(&text, &label)?;
    let keep = !(m.deps.is_empty() && m.native.is_empty());
    let new = manifest::set_script_block(&src, keep.then_some(text.as_str()));
    let write = |t: &str| {
        std::fs::write(file, t).map_err(|e| format!("cannot write {}: {e}", file.display()))
    };
    write(&new)?;
    let r = script_target(file, &ambient_env()?)
        .and_then(|t| resolve::resolve(&home(), &t.ctx, &options(g)));
    if let Err(e) = r {
        write(&src).map_err(|e2| format!("{e}\n(and {e2})"))?;
        return Err(e);
    }
    Ok(true)
}

/// `nabi add <dep> <file.nabi>`; a path dep is made relative to the script.
pub(crate) fn add_to_script(
    g: &Global,
    mut dep: Dep,
    dev: bool,
    file: &Path,
) -> Result<i32, String> {
    if dev || dep.optional {
        return Err(
            "a script's block holds only `deps`: no --dev, and no --optional (it has no \
             features)"
                .into(),
        );
    }
    if let Source::Path(p) = &dep.source {
        let abs = nabic_core::real_path(p).map_err(|e| format!("--path {p}: {e}"))?;
        let script = nabic_core::real_path(file).map_err(|e| e.to_string())?;
        let dir = script.parent().unwrap_or_else(|| Path::new("/"));
        let rel = relative(dir, &abs).display().to_string();
        dep.source = Source::Path(rel.replace('\\', "/"));
    }
    pin_newest(g, &mut dep)?;
    edit_script(g, file, |old, label| {
        manifest::add_dep(old, label, "deps", &dep).map(Some)
    })?;
    if !g.quiet {
        term::status("Added", &format!("{} to {}", dep.name, file.display()));
    }
    Ok(0)
}

/// A registry package without a requirement: `^` its newest version.
pub(crate) fn pin_newest(g: &Global, dep: &mut Dep) -> Result<(), String> {
    if let Source::Registry { req, registry } = &mut dep.source {
        if req.is_empty() {
            let url = match registry {
                Some(r) => r.clone(),
                None => registry::DEFAULT.to_owned(),
            };
            let index = registry::open(&home(), &url, g.offline, g.quiet)?;
            let all = registry::entries(&index, &url, &dep.name)?;
            let star = semver::Req::parse("*")?;
            let (e, _) = registry::best_here(&all, &star);
            let e = e.ok_or_else(|| registry::no_match(&index, &all, &dep.name, "*", &url))?;
            *req = format!("^{}", e.version);
        }
    }
    Ok(())
}

/// `dep` into the package's (`env: None`) or environment's `deps` /
/// `dev_deps`, then resolve.
pub(crate) fn add_dep(
    g: &Global,
    mut dep: Dep,
    dev: bool,
    env: Option<&str>,
) -> Result<i32, String> {
    let (path, is_env) = manifest_to_edit(env)?;
    // Path deps: relative in a project (it moves as a whole), absolute in
    // an environment.
    if let Source::Path(p) = &dep.source {
        let abs = nabic_core::real_path(p).map_err(|e| format!("--path {p}: {e}"))?;
        let dir = nabic_core::real_path(path.parent().unwrap_or_else(|| Path::new(".")))
            .map_err(|e| e.to_string())?;
        let shown = if is_env { abs } else { relative(&dir, &abs) };
        dep.source = Source::Path(shown.display().to_string().replace('\\', "/"));
    }
    pin_newest(g, &mut dep)?;
    let old = std::fs::read_to_string(&path).ok();
    let section = if dev { "dev_deps" } else { "deps" };
    let text = manifest::add_dep(
        old.as_deref().unwrap_or(manifest::EMPTY),
        &path.display().to_string(),
        section,
        &dep,
    )?;
    let graph = write_and_resolve(g, &path, old.as_deref(), &text)?;
    if !g.quiet {
        if let Some(e) = env {
            term::status(
                "Installed",
                &format!(
                    "{n} into environment `{e}` (for `nabi repl` / `nabi jupyter`; a script gets \
                     it with `nabi add {n} <file.nabi>`)",
                    n = dep.name
                ),
            );
        } else {
            // A dev dependency is not in the resolved graph: its pin, or
            // its directory's manifest.
            let pinned = || match &dep.source {
                Source::Registry { req, .. } => req.strip_prefix('^').map(str::to_owned),
                Source::Path(p) => {
                    let dir = path.parent()?.join(p);
                    manifest::load(&manifest::find(&dir)?).ok()?.version
                }
                Source::Git { .. } => None,
            };
            let v = dep_version(&graph, &dep.name)
                .or_else(pinned)
                .map_or_else(String::new, |v| format!(" v{v}"));
            let dev = if dev { " (dev_deps)" } else { "" };
            term::status("Added", &format!("{}{v}{dev}", dep.name));
        }
    }
    Ok(0)
}

pub(crate) fn cmd_remove(g: &Global, args: &[String]) -> Result<i32, String> {
    package_only(g, "remove", "uninstall")?;
    let (script, args) = script_arg(args)?;
    let (native, name) = match args.as_slice() {
        [name] => (false, name),
        [f, name] if f == "--native" => (true, name),
        _ => return Err("usage: nabi remove [--native] <name> [<file.nabi>]".into()),
    };
    if let Some(file) = script {
        let section = if native { "native" } else { "deps" };
        let removed = edit_script(g, &file, |old, label| {
            manifest::remove_dep(old, label, section, name)
        })?;
        if !removed {
            return Err(format!(
                "`{name}` is not a dependency in the block of {}",
                file.display()
            ));
        }
        if !g.quiet {
            term::status("Removed", &format!("{name} from {}", file.display()));
        }
        return Ok(0);
    }
    let (path, _) = manifest_to_edit(None)?;
    if !remove_from(g, &path, native, name)? {
        return Err(format!(
            "`{name}` is not a dependency in {}",
            path.display()
        ));
    }
    if !g.quiet {
        term::status("Removed", &format!("{name} from {}", path.display()));
    }
    Ok(0)
}

/// Remove `name` from `path`'s deps (with `native`, its `native`) and
/// resolve; whether it was there.
pub(crate) fn remove_from(
    g: &Global,
    path: &Path,
    native: bool,
    name: &str,
) -> Result<bool, String> {
    let sections: &[&str] = if native {
        &["native"]
    } else {
        &["deps", "dev_deps"]
    };
    let Ok(old) = std::fs::read_to_string(path) else {
        return Ok(false);
    };
    let file = path.display().to_string();
    let mut text: Option<String> = None;
    for section in sections {
        let now = text.as_deref().unwrap_or(&old);
        if let Some(t) = manifest::remove_dep(now, &file, section, name)? {
            text = Some(t);
        }
    }
    let Some(text) = text else {
        return Ok(false);
    };
    write_and_resolve(g, path, Some(&old), &text)?;
    Ok(true)
}
