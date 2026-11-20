//! Parsing manifest text.

#[allow(clippy::wildcard_imports)]
use crate::manifest::*;

/// Parse manifest text; `file` names it in errors.
pub fn parse(text: &str, file: &str) -> Result<Manifest, String> {
    // TOML (`name = "app"`) or JSON (`{"name": ..}`): show the one value.
    let example = |e: String| {
        nabic_nabion::with_help(
            &e,
            "a `nabi.nabion` holds one `Package { .. }`: `Package { name: \"app\", version: \
             \"0.1.0\", deps: {} }`",
        )
    };
    let doc = Doc::parse(text, file).map_err(|e| {
        if e.contains("holds one value") {
            example(e)
        } else {
            e
        }
    })?;
    if crate::workspace::named(&doc) {
        return Err(format!(
            "{file} is a workspace root (`Workspace {{ .. }}`), not a package: run this in a \
             member's directory, or name one with `-p <member>`"
        ));
    }
    let pkg = doc.root().record("Package").map_err(example)?;
    let mut warnings = Vec::new();
    let mut warn = |d: Decoder<'_>, msg: &str| {
        let (line, col) = nabic_nabion::line_col(text, d.span().start as usize);
        warnings.push(format!("{file}:{line}:{col}: {msg}"));
    };
    // Unknown fields warn: a newer nabi's, or a typo.
    let mut extra = Vec::new();
    for k in pkg.known(&FIELDS) {
        let Some(d) = pkg.get(k) else { continue };
        let near = nabic_core::suggest::suggest(k, FIELDS.iter().copied())
            .map(|n| format!("; did you mean `{n}`?"))
            .unwrap_or_default();
        warn(d, &format!("unknown field `{k}` is ignored{near}"));
        extra.push((k.to_owned(), d.value().clone()));
    }
    let string = |f: &str| pkg.opt(f).map(|d| d.string()).transpose();
    let deps = |f: &str| {
        pkg.opt(f)
            .map_or(Ok(Vec::new()), |d| deps(d, f == "dev_deps"))
    };
    // A list of non-empty strings.
    let list = |f: &str| -> Result<Vec<String>, String> {
        let mut out = Vec::new();
        for d in pkg
            .opt(f)
            .map(|d| d.list())
            .transpose()?
            .unwrap_or_default()
        {
            let s = d.string()?;
            if s.trim().is_empty() {
                return Err(d.error(&format!(
                    "an entry of `{f}` is empty; remove it or write a value"
                )));
            }
            out.push(s);
        }
        Ok(out)
    };
    let keywords = list("keywords")?;
    let categories = list("categories")?;
    for (f, l, eg) in [
        ("keywords", &keywords, "[\"json\", \"parser\"]"),
        ("categories", &categories, "[\"encoding\"]"),
    ] {
        if let Some(d) = pkg.opt(f).filter(|_| l.len() > MAX_KEYWORDS) {
            return Err(d.error(&format!(
                "`{f}` has {} entries; keep at most {MAX_KEYWORDS}, e.g. `{f}: {eg}`",
                l.len()
            )));
        }
    }
    let metadata = match pkg.opt("metadata") {
        Some(d)
            if matches!(
                d.value().kind,
                Kind::Dict(_) | Kind::Struct { name: None, .. }
            ) =>
        {
            Some(d.value().clone())
        }
        Some(d) => return Err(d.expected("a table (`metadata: { key: .. }`)")),
        None => None,
    };
    let mut lints = Vec::new();
    if let Some(d) = pkg.opt("lints") {
        for (k, v) in d.dict()? {
            let Some(level) = Level::parse(&v.string()?) else {
                return Err(v.error(&format!(
                    "lint `{}` takes \"allow\", \"warn\" or \"deny\"",
                    k.string()?
                )));
            };
            lints.push((k.string()?, level));
        }
    }
    let mut no_overflow_checks = Vec::new();
    if let Some(d) = pkg.opt("profile") {
        for (k, v) in d.dict()? {
            let name = k.string()?;
            if !PROFILES.contains(&name.as_str()) {
                return Err(k.error(&format!(
                    "there is no profile `{name}`; write \"debug\" or \"release\", e.g. \
                     `profile: {{ \"release\": {{ overflow_checks: false }} }}`"
                )));
            }
            let p = v.record("Profile")?;
            p.only(&["overflow_checks"])?;
            if p.opt("overflow_checks").map(|d| d.bool()).transpose()? == Some(false) {
                no_overflow_checks.push(name);
            }
        }
    }
    // The package name is the dependency name others give it.
    if let Some(d) = pkg.opt("name") {
        let name = d.string()?;
        check_name(&name).map_err(|m| d.error(&m))?;
        if let Err(m) = nabic_core::check_os_name("package", &name) {
            warn(d, &m);
        }
    }
    if let Some(d) = pkg.opt("version") {
        if let Err(e) = crate::semver::Version::parse(&d.string()?) {
            warn(
                d,
                &format!("{e}; `nabi publish` needs one, e.g. `version: \"0.1.0\"`"),
            );
        }
    }
    if let Some(d) = pkg.opt("nabi_version") {
        crate::pin::nabi_version(&d.string()?).map_err(|e| d.error(&e))?;
    }
    if let Some(d) = pkg.opt("toolchain") {
        let pin = d.string()?;
        if crate::pin::ToolchainReq::parse(&pin).is_err() {
            warn(
                d,
                &format!("`toolchain: {pin:?}`: {}", crate::pin::PIN_FORMS),
            );
        }
    }
    let normal = deps("deps")?;
    let mut features = Features::new();
    let mut at = Vec::new();
    if let Some(d) = pkg.opt("features") {
        for (k, v) in d.dict()? {
            let items = v.list()?;
            let list = items
                .iter()
                .map(Decoder::string)
                .collect::<Result<_, _>>()?;
            features.push((k.string()?, list));
            at.push((k, items));
        }
    }
    check_features(&features, &normal).map_err(|(a, m)| match a {
        At::Feature(i) => at[i].0.error(&m),
        At::Entry(i, j) => at[i].1[j].error(&m),
        At::Dep(k) => pkg
            .opt("deps")
            .and_then(|d| d.dict().ok())
            .and_then(|kv| kv.into_iter().nth(k))
            .map_or_else(|| m.clone(), |(key, _)| key.error(&m)),
    })?;
    let dev = deps("dev_deps")?;
    let native = pkg.opt("native").map_or(Ok(Vec::new()), natives)?;
    let mut xcframework = false;
    let lib = match pkg.opt("lib") {
        None => LibKind::Nabi,
        Some(d) => {
            let r = d.record("Lib")?;
            r.only(&["kind", "xcframework"])?;
            xcframework = r
                .opt("xcframework")
                .map(|x| x.bool())
                .transpose()?
                .unwrap_or(false);
            match r.opt("kind").map(|k| k.string()).transpose()?.as_deref() {
                None | Some("nabi") => LibKind::Nabi,
                Some("cdylib") => LibKind::Cdylib,
                Some("staticlib") => LibKind::Staticlib,
                Some("plugin") => LibKind::Plugin,
                Some(k) => {
                    return Err(r.field_error(
                        "kind",
                        &format!(
                            "there is no library kind `{k}`; write \"nabi\" (the default), \
                             \"cdylib\" (a shared library C loads), \"staticlib\" (a static \
                             library C links alone) or \"plugin\" (a shared library a Nabi \
                             program loads)"
                        ),
                    ));
                }
            }
        }
    };
    if xcframework && lib != LibKind::Staticlib {
        return Err(pkg.field_error(
            "lib",
            "`xcframework: true` packs a static library: write `lib: { kind: \"staticlib\", \
             xcframework: true }`",
        ));
    }
    let c = pkg.opt("c").map(c_sources).transpose()?;
    for f in ["deps", "dev_deps", "native"] {
        for (k, _) in pkg
            .opt(f)
            .map(|d| d.dict())
            .transpose()?
            .unwrap_or_default()
        {
            if let Err(m) = nabic_core::check_os_name("package", &k.string()?) {
                warn(k, &m);
            }
        }
    }
    if let Some(b) = normal
        .iter()
        .find(|d| d.load == Load::Bundled && lib != LibKind::Plugin)
    {
        return Err(pkg.field_error(
            "deps",
            &format!(
                "`bundle: true` on `{}` belongs in a plugin's dependencies: give this package \
             `lib: {{ kind: \"plugin\" }}`, or remove `bundle: true`",
                b.name
            ),
        ));
    }
    let deployment = pkg
        .opt("deployment_target")
        .map(deployment_targets)
        .transpose()?
        .unwrap_or_default();
    Ok(Manifest {
        name: string("name")?,
        version: string("version")?,
        nabi_version: string("nabi_version")?,
        toolchain: string("toolchain")?,
        description: string("description")?,
        license: string("license")?,
        license_file: string("license_file")?,
        repository: string("repository")?,
        authors: list("authors")?,
        homepage: string("homepage")?,
        documentation: string("documentation")?,
        readme: string("readme")?,
        keywords,
        categories,
        include: list("include")?,
        exclude: list("exclude")?,
        links: string("links")?,
        default_run: string("default_run")?,
        deployment_target: deployment.0,
        ios_deployment_target: deployment.1,
        metadata,
        extra,
        publish: pkg.opt("publish").map(|d| d.bool()).transpose()?,
        lib,
        xcframework,
        deps: normal,
        dev_deps: dev,
        lints,
        lints_from: string("lints_from")?,
        native,
        features,
        no_overflow_checks,
        c,
        warnings,
    })
}

/// `c: { sources: [..], include: [..], define: { "N": "1" }, std: "c17",
/// flags: [..] }`.
fn c_sources(d: Decoder<'_>) -> Result<CSources, String> {
    let r = d.record("C")?;
    r.only(&["sources", "include", "define", "std", "flags"])?;
    let list = |k: &str| -> Result<Vec<String>, String> {
        r.opt(k)
            .map(|d| d.list())
            .transpose()?
            .unwrap_or_default()
            .into_iter()
            .map(|d| d.string())
            .collect()
    };
    let mut define = Vec::new();
    for (k, v) in r
        .opt("define")
        .map(|d| d.dict())
        .transpose()?
        .unwrap_or_default()
    {
        define.push((k.string()?, v.string()?));
    }
    let std = r.opt("std").map(|d| d.string()).transpose()?;
    if let Some(s) = &std {
        const STDS: [&str; 10] = [
            "c89", "c99", "c11", "c17", "c23", "gnu89", "gnu99", "gnu11", "gnu17", "gnu23",
        ];
        if !STDS.contains(&s.as_str()) {
            return Err(r.field_error(
                "std",
                &format!(
                    "`std: \"{s}\"` is not a C standard; write one of {}",
                    STDS.join(", ")
                ),
            ));
        }
    }
    let flags = list("flags")?;
    // Only flags that change code, never ones that run tools or read files.
    let allowed = |f: &str| {
        (f.starts_with("-O") || f.starts_with("-W") || f.starts_with("-f") || f.starts_with("-m"))
            && !f.starts_with("-flto")
            || f == "-g"
    };
    if let Some(f) = flags.iter().find(|f| !allowed(f)) {
        return Err(r.field_error(
            "flags",
            &format!(
                "`{f}` is not a flag `c.flags` takes: it takes -O*, -g, -W*, -f* (not -flto) and \
                 -m*; put include directories in `include` and macros in `define`"
            ),
        ));
    }
    let sources = list("sources")?;
    if sources.is_empty() {
        return Err(r.field_error(
            "sources",
            "`c` needs `sources`, e.g. `sources: [\"c/shim.c\"]`",
        ));
    }
    Ok(CSources {
        sources,
        include: list("include")?,
        define,
        std,
        flags,
    })
}

/// `m.lints` after its `lints_from` chain, outermost first; later wins.
pub fn inherit_lints(m: &Manifest, path: &Path) -> Result<Vec<(String, Level)>, String> {
    let real = nabic_core::real_path(path).map_err(|e| format!("{}: {e}", path.display()))?;
    chain_lints(m, path, &mut vec![real])
}

pub(crate) fn chain_lints(
    m: &Manifest,
    path: &Path,
    seen: &mut Vec<PathBuf>,
) -> Result<Vec<(String, Level)>, String> {
    let mut out = Vec::new();
    if let Some(from) = &m.lints_from {
        let err = |what: String| format!("{}: `lints_from: {from:?}`: {what}", path.display());
        let dir = path.parent().unwrap_or_else(|| Path::new(".")).join(from);
        if !dir.is_dir() {
            return Err(err(format!("{} is not a directory", dir.display())));
        }
        let next = find(&dir).ok_or_else(|| err(format!("no {MANIFEST} in {}", dir.display())))?;
        let real = nabic_core::real_path(&next).map_err(|e| err(e.to_string()))?;
        if seen.contains(&real) {
            let names: Vec<String> = seen
                .iter()
                .chain([&real])
                .map(|p| p.display().to_string())
                .collect();
            return Err(err(format!("cycle: {}", names.join(" -> "))));
        }
        seen.push(real);
        out = chain_lints(&load(&next)?, &next, seen)?;
    }
    out.extend(m.lints.iter().cloned());
    Ok(out)
}

/// A `native` dict: name -> `Dep::native(..)` or a requirement string.
pub fn natives(d: Decoder<'_>) -> Result<Vec<NativeDep>, String> {
    let mut out: Vec<NativeDep> = Vec::new();
    for (k, v) in d.dict()? {
        let name = k.string()?;
        // A framework keeps Apple's name (`Foundation`).
        let framework = v.variant("Dep").is_ok_and(|n| n.name == "framework");
        if !framework {
            check_name(&name).map_err(|m| k.error(&m))?;
        }
        if let Some(o) = out.iter().find(|o| same(&o.name, &name)) {
            return Err(k.error(&format!(
                "`{name}` and `{}` are the same native package (`-` and `_` are one)",
                o.name
            )));
        }
        let source = if v.string().is_ok() {
            NativeSource::Registry {
                req: req(v)?,
                registry: None,
            }
        } else {
            let n = v
                .variant("Dep")
                .ok()
                .filter(|n| matches!(n.name, "native" | "system" | "framework"))
                .ok_or_else(|| {
                    v.expected(
                        "`Dep::native(..)`, `Dep::system(..)`, `Dep::framework()` or a version \
                         requirement",
                    )
                })?;
            if n.name != "native" && !n.args.is_empty() {
                return Err(v.error(&format!(
                    "`Dep::{}` takes named arguments only, e.g. `Dep::system(pkg_config: \"zlib\")`",
                    n.name
                )));
            }
            match n.args.as_slice() {
                [] if n.name == "framework" => {
                    if n.fields.names().next().is_some() {
                        return Err(v.error(
                            "`Dep::framework()` takes no arguments: the key names the framework",
                        ));
                    }
                    NativeSource::Framework
                }
                [] if n.name == "system" => {
                    n.fields
                        .only(&["pkg_config", "brew", "vcpkg", "apt", "libs", "static"])?;
                    let opt = |k: &str| n.fields.opt(k).map(|d| d.string()).transpose();
                    let mut libs = Vec::new();
                    for d in n
                        .fields
                        .opt("libs")
                        .map(|d| d.list())
                        .transpose()?
                        .unwrap_or_default()
                    {
                        libs.push(d.string()?);
                    }
                    NativeSource::System(SystemLib {
                        pkg_config: opt("pkg_config")?,
                        brew: opt("brew")?,
                        vcpkg: opt("vcpkg")?,
                        apt: opt("apt")?,
                        libs,
                        statik: n
                            .fields
                            .opt("static")
                            .map(|d| d.bool())
                            .transpose()?
                            .unwrap_or(false),
                    })
                }
                [r] => {
                    n.fields.only(&["registry"])?;
                    NativeSource::Registry {
                        req: req(*r)?,
                        registry: n.fields.opt("registry").map(|r| r.string()).transpose()?,
                    }
                }
                [] => {
                    n.fields.only(&["url", "sha256"])?;
                    let sha = n.fields.req("sha256")?;
                    let sha256 = sha.string()?;
                    if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
                        return Err(sha.error("`sha256` is 64 hex digits"));
                    }
                    NativeSource::Direct {
                        url: n.fields.req("url")?.string()?,
                        sha256: sha256.to_ascii_lowercase(),
                    }
                }
                _ => {
                    return Err(v.error(
                        "`Dep::native` takes a version requirement, or `url:` and `sha256:`",
                    ));
                }
            }
        };
        out.push(NativeDep { name, source });
    }
    Ok(out)
}

pub(crate) fn deps(d: Decoder<'_>, dev: bool) -> Result<Vec<Dep>, String> {
    let mut out = Vec::new();
    for (k, v) in d.dict()? {
        let name = k.string()?;
        // `std: "..."`: the standard library is no dependency. A tier's
        // name may still be another package's name in code (`package:`).
        let tier = nabic_core::TIERS.contains(&code_name(&name).as_str());
        if tier
            && !v
                .variant("Dep")
                .is_ok_and(|x| x.fields.opt("package").is_some())
        {
            return Err(std_dep(k, &name));
        }
        if !tier {
            check_name(&name).map_err(|m| k.error(&m))?;
        }
        if let Some(o) = out.iter().find(|o: &&Dep| same(&o.name, &name)) {
            return Err(k.error(&format!(
                "`{name}` and `{}` are the same package (`-` and `_` are one)",
                o.name
            )));
        }
        let mut dep = dep(v, true, false)?;
        if dev && dep.optional {
            return Err(v.error("a dev dependency cannot be optional"));
        }
        dep.name = name;
        out.push(dep);
    }
    Ok(out)
}

/// A dependency on standard library tier `name`, which ships with the
/// toolchain.
fn std_dep(d: Decoder<'_>, name: &str) -> String {
    nabic_nabion::with_help(
        &d.error(&format!(
            "`{name}` is the standard library, not a package to depend on"
        )),
        &format!("{name} ships with the toolchain; remove this line"),
    )
}

/// A requirement string, checked.
pub(crate) fn req(d: Decoder<'_>) -> Result<String, String> {
    let s = d.string()?;
    crate::semver::Req::parse(&s).map_err(|e| d.error(&e))?;
    Ok(s)
}

/// A `Dep::path(..)` / `Dep::git(..)` / `Dep::version(..)` value, or a
/// requirement string (`"^1.2"`, short for `Dep::version("^1.2")`).
pub fn source(d: Decoder<'_>) -> Result<Source, String> {
    dep(d, false, false).map(|d| d.source)
}

/// `source`, plus feature arguments when `flags` (name left empty);
/// `lenient` skips arguments it does not know (a file a newer nabi wrote).
pub(crate) fn dep(d: Decoder<'_>, flags: bool, lenient: bool) -> Result<Dep, String> {
    if let Ok(s) = d.string() {
        // `"geom": "../geom"`: a path where a requirement goes.
        if s.starts_with(['.', '/', '~']) || s.contains(['/', '\\']) {
            return Err(nabic_nabion::with_help(
                &d.error(&format!("`{s}` is a path, not a version requirement")),
                &format!("a path dependency is `Dep::path(\"{s}\")`"),
            ));
        }
        return Ok(Dep::new(
            String::new(),
            Source::Registry {
                req: req(d)?,
                registry: None,
            },
        ));
    }
    let v = d.variant("Dep").map_err(|_| {
        d.expected("`Dep::path(\"..\")`, `Dep::git(\"..\")` or `Dep::version(\"..\")`")
    })?;
    let one = |what: &str| match v.args.as_slice() {
        [x] => x.string(),
        _ => Err(d.error(&format!("`Dep::{}` takes one string, the {what}", v.name))),
    };
    let only = |own: &[&str]| {
        if lenient {
            return Ok(());
        }
        let extra: &[&str] = if flags { &DEP_FLAGS } else { &[] };
        v.fields.only(&[own, extra].concat())
    };
    let source = match v.name {
        "path" => {
            only(&["version"])?;
            Source::Path(one("path")?)
        }
        "git" => {
            only(&["rev", "tag", "branch"])?;
            let mut refs = Vec::new();
            for k in v
                .fields
                .names()
                .filter(|k| ["rev", "tag", "branch"].contains(k))
            {
                let s = v.fields.req(k)?.string()?;
                refs.push(match k {
                    "rev" => GitRef::Rev(s),
                    "tag" => GitRef::Tag(s),
                    _ => GitRef::Branch(s),
                });
            }
            Source::Git {
                url: one("url")?,
                reference: match refs.len() {
                    0 => GitRef::Default,
                    1 => refs.remove(0),
                    _ => return Err(d.error("give at most one of rev, tag, branch")),
                },
            }
        }
        "version" => {
            only(&["registry"])?;
            let [r] = v.args.as_slice() else {
                return Err(d.error("`Dep::version` takes one string, the version requirement"));
            };
            Source::Registry {
                req: req(*r)?,
                registry: v.fields.opt("registry").map(|r| r.string()).transpose()?,
            }
        }
        "native" => {
            return Err(
                d.error("a native package goes in the `native` field, not in `deps` / `dev_deps`")
            );
        }
        other => {
            return Err(d.error(&format!(
                "unknown dependency kind `Dep::{other}` (expected Dep::path, Dep::git, \
                 Dep::version)"
            )));
        }
    };
    let mut out = Dep::new(String::new(), source);
    if let (Source::Path(_), Some(r)) = (&out.source, v.fields.opt("version")) {
        out.version = Some(req(r)?);
    }
    if let Some(o) = v.fields.opt("optional") {
        out.optional = o.bool()?;
    }
    if let Some(f) = v.fields.opt("features") {
        for x in f.list()? {
            let name = x.string()?;
            check_feature(&name).map_err(|m| x.error(&m))?;
            out.features.push(name);
        }
    }
    if let Some(o) = v.fields.opt("default_features") {
        out.default_features = o.bool()?;
    }
    if let Some(p) = v.fields.opt("package") {
        let name = p.string()?;
        if nabic_core::TIERS.contains(&code_name(&name).as_str()) {
            return Err(std_dep(p, &name));
        }
        check_name(&name).map_err(|m| p.error(&m))?;
        out.package = Some(name);
    }
    if let Some(t) = v.fields.opt("target") {
        let s = t.string()?;
        crate::platform::check(&s).map_err(|m| t.error(&m))?;
        out.target = Some(s);
    }
    let flag = |f: &str| v.fields.opt(f).map(|o| o.bool()).transpose();
    out.load = match (flag("plugin")? == Some(true), flag("bundle")? == Some(true)) {
        (false, false) => Load::Linked,
        (true, false) => Load::Plugin,
        (false, true) => Load::Bundled,
        (true, true) => {
            return Err(d.error(
                "a dependency is either `plugin: true` (loaded at run time) or `bundle: true` \
             (linked into a plugin), not both",
            ));
        }
    };
    Ok(out)
}

/// `deployment_target`: `"13.0"` (macOS), or `{ macos: "13.0", ios: "16.0" }`.
fn deployment_targets(d: Decoder<'_>) -> Result<(Option<String>, Option<String>), String> {
    let version = |os: &str, example: &str, v: String| {
        if v.split('.')
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
        {
            Ok(v)
        } else {
            Err(format!(
                "`deployment_target`: \"{v}\" is not a version; write the {os} version as \
                 numbers: `{example}`"
            ))
        }
    };
    if let Ok(v) = d.string() {
        return Ok((
            Some(version("macOS", "deployment_target: \"13.0\"", v)?),
            None,
        ));
    }
    let r = d.record("DeploymentTarget").map_err(|_| {
        d.expected("a macOS version (`\"13.0\"`) or `{ macos: \"13.0\", ios: \"16.0\" }`")
    })?;
    r.only(&["macos", "ios"])?;
    let get = |k: &str, os: &str, example: &str| {
        r.opt(k)
            .map(|v| v.string().and_then(|v| version(os, example, v)))
            .transpose()
    };
    Ok((
        get("macos", "macOS", "deployment_target: { macos: \"13.0\" }")?,
        get("ios", "iOS", "deployment_target: { ios: \"16.0\" }")?,
    ))
}
