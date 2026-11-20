//! `nabi update`, `nabi deps` and `nabi search`.

#[allow(clippy::wildcard_imports)]
use crate::*;

pub(crate) fn cmd_update(g: &Global, names: &[String]) -> Result<i32, String> {
    let in_package = g.env.is_none() && package_dir(Path::new(".")).is_some();
    let t = target_for(g, None)?;
    let manifest = &t.ctx.manifest;
    let has_deps =
        !(manifest.deps.is_empty() && manifest.dev_deps.is_empty() && manifest.native.is_empty());
    // Outside a package, the ambient environment only when it has
    // libraries; else this is a mistake, not an empty update.
    if !in_package && g.env.is_none() && !has_deps {
        return Err(
            "not inside a package (no src/main.nabi, src/lib.nabi or nabi.nabion here or above): \
             make one with `nabi init`; for an environment's libraries: `nabi update --env <name>`"
                .to_owned(),
        );
    }
    for n in names {
        if !manifest
            .deps
            .iter()
            .chain(&manifest.dev_deps)
            .any(|d| manifest::same(&d.name, n))
            && !manifest.native.iter().any(|d| manifest::same(&d.name, n))
        {
            let lock = t
                .ctx
                .lock_path
                .as_deref()
                .and_then(|p| lock::Lock::read(p).ok().flatten());
            if lock
                .as_ref()
                .is_none_or(|l| l.find(n).is_none() && l.find_native(n).is_none())
            {
                return Err(format!("`{n}` is not a dependency"));
            }
        }
    }
    let read = || {
        t.ctx
            .lock_path
            .as_deref()
            .and_then(|p| lock::Lock::read(p).ok().flatten())
            .unwrap_or_default()
    };
    let before = read();
    let mut opts = options(g);
    opts.update = Some(names.to_vec());
    opts.dev = true;
    resolve::resolve(&home(), &t.ctx, &opts)?;
    if g.quiet {
        return Ok(0);
    }
    let changes = lock_changes(&before, &read());
    for (verb, what) in &changes {
        term::status(verb, what);
    }
    let what = match &t.env {
        Some(e) => format!(" in environment `{e}`"),
        None => String::new(),
    };
    if !has_deps {
        let add = if t.env.is_some() {
            "nabi install <name>"
        } else {
            "nabi add <name>"
        };
        term::note(&format!(
            "no dependencies{what} to update; add one: `{add}`"
        ));
    } else if changes.is_empty() {
        term::note(&format!("all dependencies{what} are up to date"));
    } else if let Some(e) = &t.env {
        term::status("Updated", &format!("environment `{e}`"));
    }
    Ok(0)
}

/// What `nabi update` changed in the lock: (status verb, line) per
/// package, as `json v0.1.0 -> v0.1.1`.
pub(crate) fn lock_changes(old: &lock::Lock, new: &lock::Lock) -> Vec<(&'static str, String)> {
    let short = |c: &str| c.chars().take(7).collect::<String>();
    let label = |p: &lock::Locked| {
        p.version
            .as_ref()
            .map_or_else(|| short(&p.commit), |v| format!("v{v}"))
    };
    let mut out = Vec::new();
    for p in &new.packages {
        match old.find(&p.name) {
            None => out.push(("Locked", format!("{} {}", p.name, label(p)))),
            Some(o) if o.commit == p.commit && o.version == p.version => {}
            Some(o) if label(o) == label(p) => out.push((
                "Updated",
                format!("{} {} -> {}", p.name, short(&o.commit), short(&p.commit)),
            )),
            Some(o) => out.push((
                "Updated",
                format!("{} {} -> {}", p.name, label(o), label(p)),
            )),
        }
    }
    for p in &new.natives {
        match old.find_native(&p.name) {
            None => out.push(("Locked", format!("{} v{}", p.name, p.version))),
            Some(o) if o.version == p.version => {}
            Some(o) => out.push((
                "Updated",
                format!("{} v{} -> v{}", p.name, o.version, p.version),
            )),
        }
    }
    for p in &old.packages {
        if new.find(&p.name).is_none() {
            out.push(("Removed", format!("{} {}", p.name, label(p))));
        }
    }
    for p in &old.natives {
        if new.find_native(&p.name).is_none() {
            out.push(("Removed", format!("{} v{}", p.name, p.version)));
        }
    }
    out
}

/// `nabi deps [path] [--dev] [--format json]`: `name=<lib.nabi>` per
/// dependency in build order, then per native package a `# native <name>
/// <vers>` line and its nabic arguments (read by the REPL's `:add`).
pub(crate) fn cmd_deps(g: &Global, args: &[String]) -> Result<i32, String> {
    let mut opts = options(g);
    let args = feature_flags(args, &mut opts)?;
    let json = json_format(&args, "deps")?;
    let mut path = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--format" {
            it.next();
        } else if !a.starts_with('-') {
            path = Some(a.as_str());
            break;
        }
    }
    let t = target_for(g, path)?;
    opts.dev = args.iter().any(|a| a == "--dev");
    let graph = resolve::resolve(&home(), &t.ctx, &opts)?;
    if json {
        return deps_json(g, &graph);
    }
    for p in &graph.packages {
        // `name=<lib.nabi>` per name, then the features.
        for l in extern_args(p) {
            println!("{}", l.strip_prefix("--extern=").unwrap_or(&l));
        }
    }
    for n in &graph.natives {
        println!("# native {} {}", n.entry.name, n.entry.version);
        for a in native::nabic_args(
            &home(),
            &[n],
            nabic_target::TargetSpec::host(),
            g.offline,
            g.quiet,
        )? {
            println!("{a}");
        }
    }
    Ok(0)
}

/// `nabi deps --format json`: a `package` record per dependency in build
/// order, a `native` record per native package, then a `summary`.
pub(crate) fn deps_json(g: &Global, graph: &resolve::Graph) -> Result<i32, String> {
    let record = |ty: &str, fields: Vec<(&str, Json)>| {
        let mut all = vec![
            (
                "schema".to_owned(),
                Json::num(nabic_diagnostic::RECORD_SCHEMA),
            ),
            ("type".to_owned(), Json::str(ty)),
        ];
        all.extend(fields.into_iter().map(|(k, v)| (k.to_owned(), v)));
        println!("{}", Json::Obj(all));
    };
    let strs = |v: &[String]| Json::Arr(v.iter().map(Json::str).collect());
    for p in &graph.packages {
        record(
            "package",
            vec![
                ("name", Json::str(&p.name)),
                (
                    "version",
                    p.version.as_deref().map_or(Json::Null, Json::str),
                ),
                ("source", Json::str(&p.source)),
                ("path", Json::str(p.root.display().to_string())),
                ("features", strs(&p.features)),
            ],
        );
    }
    for n in &graph.natives {
        let args = native::nabic_args(
            &home(),
            &[n],
            nabic_target::TargetSpec::host(),
            g.offline,
            g.quiet,
        )?;
        record(
            "native",
            vec![
                ("name", Json::str(&n.entry.name)),
                ("version", Json::str(n.entry.version.to_string())),
                ("args", strs(&args)),
            ],
        );
    }
    let count = |n: usize| Json::num(u32::try_from(n).unwrap_or(u32::MAX));
    record(
        "summary",
        vec![
            ("packages", count(graph.packages.len())),
            ("natives", count(graph.natives.len())),
        ],
    );
    Ok(0)
}

/// `nabi search [--native] <query>`: `name newest - description` for each
/// name containing the query (any case).
pub(crate) fn cmd_search(g: &Global, args: &[String]) -> Result<i32, String> {
    let (q, native) = match args {
        [q] if !q.starts_with('-') => (q, false),
        [f, q] if f == "--native" => (q, true),
        _ => return Err("usage: nabi search [--native] <query>".into()),
    };
    let url = registry::DEFAULT.to_owned();
    let index = registry::open(&home(), &url, g.offline, g.quiet)?;
    // `-` and `_` are one; file names are lowercase.
    let q = q.to_ascii_lowercase().replace('_', "-");
    let mut names: Vec<String> = registry::names(&index, &url, native)?
        .into_iter()
        .filter(|n| n.replace('_', "-").contains(&q))
        .collect();
    names.sort();
    let star = semver::Req::parse("*")?;
    // Descriptions live in the registry API, not the index.
    let api = if g.offline {
        None
    } else {
        registry::api(&home(), &url, true).ok()
    };
    for n in names {
        let newest = if native {
            native::best(&native::entries(&index, &url, &n)?, &star)
                .map(|e| (e.name.clone(), e.version.clone()))
        } else {
            registry::best(&registry::entries(&index, &url, &n)?, &star)
                .map(|e| (e.name.clone(), e.version.clone()))
        };
        if let Some((name, version)) = newest {
            let v = version.to_string();
            match api
                .as_deref()
                .and_then(|a| registry::description(a, &name, &v, native))
            {
                Some(d) => println!("{name} {v} - {d}"),
                None => println!("{name} {v}"),
            }
        }
    }
    Ok(0)
}
