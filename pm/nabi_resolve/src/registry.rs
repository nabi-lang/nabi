//! Registry packages: a registry index is a git repository laid out like
//! crates.io-index. One file per package, one `Package { .. }` line per
//! version, oldest first:
//!
//! ```text
//! config.nabion     Config { api: "https://packages.nabi-lang.org" }   the registry API
//! 1/a  2/ab  3/a/abc  ab/cd/abcd..      a package's file (by its lowercased name)
//! native/1/a  native/ab/cd/abcd..       a native package's (see `native`)
//! ```
//!
//! ```nabion
//! Package { name: "geom", version: "0.3.1", deps: [{ name: "num", req: "^1.2", kind: "normal" }], source: Dep::git("https://github.com/o/geom", rev: "<commit>"), tree: "<tree>", yanked: false, nabi_version: "0.1.0" }
//! ```
//!
//! A line holds what resolution needs, named as in the manifest; the
//! descriptive fields go to the registry API with the publish (`meta`).
//! Feature fields are written only where not the default, as in
//! `nabi.nabion`. Sources are not uploaded: `source` is a git commit and
//! `tree` its tree hash, checked after checkout. The registry keeps an
//! archive of that commit and writes its SHA-256 as `archive`; `nabi`
//! downloads it from the registry API, checks it, and fetches the git
//! source only when there is none. A dependency without `registry` is on
//! the same index.
//!
//! A bare requirement means [`DEFAULT`]. `$NABI_HOME/sources.nabion`
//! fetches an index from a mirror instead (`Sources { replace: {
//! "<index>": "<mirror>" } }`); the lock still names the index.

// `HEAD^{tree}` is git revision syntax, not a format argument.
#![allow(clippy::literal_string_with_formatting_args)]

use crate::git::{self, Cache};
use crate::manifest::{self, Dep, Features, GitRef, Manifest, Source};
use crate::semver::{Req, Version};
use nabic_nabion::{Decoder, Doc, Kind, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The index of a bare requirement: that of packages.nabi-lang.org.
pub const DEFAULT: &str = "https://github.com/nabi-lang/packages-index";

fn sources_file(home: &Path) -> PathBuf {
    home.join("sources.nabion")
}

/// `$NABI_HOME/sources.nabion`: `(index, mirror)` pairs, keys without a
/// trailing `/`.
pub fn sources(home: &Path) -> Result<Vec<(String, String)>, String> {
    let path = sources_file(home);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };
    let file = path.display().to_string();
    let doc = Doc::parse(&text, &file)?;
    let r = doc.root().record("Sources")?;
    crate::skipped(&file, &r.known(&["replace"]));
    let mut out = Vec::new();
    for (k, v) in r
        .opt("replace")
        .map(|d| d.dict())
        .transpose()?
        .unwrap_or_default()
    {
        out.push((index_key(&k.string()?).to_owned(), v.string()?));
    }
    Ok(out)
}

/// A package's file in the index.
pub fn path(name: &str) -> String {
    let n = name.to_ascii_lowercase();
    match n.len() {
        1 => format!("1/{n}"),
        2 => format!("2/{n}"),
        3 => format!("3/{}/{n}", &n[..1]),
        _ => format!("{}/{}/{n}", &n[..2], &n[2..4]),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)] // the line's fields
pub struct IndexDep {
    pub name: String,
    pub req: String,
    /// `kind: "dev"`: for the package's own tests, not followed.
    pub dev: bool,
    /// `kind: "native"`: a native package (`crate::native`).
    pub native: bool,
    /// Another index (`None`: the package's own).
    pub registry: Option<String>,
    pub optional: bool,
    pub features: Vec<String>,
    pub default_features: bool,
    /// `package`: the package's own name when `name` renames it.
    pub package: Option<String>,
    /// `target`: only builds for a matching target use it.
    pub target: Option<String>,
}

impl IndexDep {
    /// As a dependency of a package from index `url`.
    pub fn to_dep(&self, url: &str) -> Dep {
        Dep {
            optional: self.optional,
            features: self.features.clone(),
            default_features: self.default_features,
            package: self.package.clone(),
            target: self.target.clone(),
            ..Dep::new(
                self.name.clone(),
                Source::Registry {
                    req: self.req.clone(),
                    registry: Some(self.registry.clone().unwrap_or_else(|| url.to_owned())),
                },
            )
        }
    }
}

/// One version of a package: one line of its file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub version: Version,
    pub deps: Vec<IndexDep>,
    pub features: Features,
    /// `source: Dep::git(url, rev: commit)`.
    pub url: String,
    pub rev: String,
    /// The commit's git tree hash.
    pub tree: String,
    /// SHA-256 of the source archive the registry keeps (`None`: only the
    /// git source).
    pub archive: Option<String>,
    pub yanked: bool,
    /// The oldest toolchain that builds it, `x.y.z`; later ones of its
    /// series do too (`toolchain::builds_with`). `None`: any (a line of an
    /// index of one's own; the registry writes it on every line).
    pub nabi_version: Option<String>,
    /// The native library it links (the manifest's `links`).
    pub links: Option<String>,
    /// Published by the Nabi project (`official: true`, written by the
    /// registry): `nabi` adds it when code uses it.
    pub official: bool,
}

// Unknown fields of `Package` and `Req` are skipped, not rejected: a later
// field must not hide a package's other versions.
fn entry(d: Decoder<'_>) -> Result<Entry, String> {
    let r = d.record("Package")?;
    let string = |k: &str| r.req(k).and_then(|d| d.string());
    let name = string("name")?;
    manifest::check_name(&name).map_err(|e| r.field_error("name", &e))?;
    let version = Version::parse(&string("version")?).map_err(|e| r.field_error("version", &e))?;
    let mut deps = Vec::new();
    for item in r
        .opt("deps")
        .map(|l| l.list())
        .transpose()?
        .unwrap_or_default()
    {
        let q = item.record("Req")?;
        let name = q.req("name")?.string()?;
        manifest::check_name(&name).map_err(|e| q.field_error("name", &e))?;
        let req = q.req("req")?.string()?;
        Req::parse(&req).map_err(|e| q.field_error("req", &e))?;
        let (dev, native) = match q.opt("kind").map(|k| k.string()).transpose()?.as_deref() {
            None | Some("normal") => (false, false),
            Some("dev") => (true, false),
            Some("native") => (false, true),
            Some(_) => {
                return Err(q.field_error("kind", "`kind` is \"normal\", \"dev\" or \"native\""));
            }
        };
        let registry = q.opt("registry").map(|r| r.string()).transpose()?;
        let optional = q.opt("optional").map(|o| o.bool()).transpose()?;
        let package = q.opt("package").map(|p| p.string()).transpose()?;
        if let (Some(p), Some(d)) = (&package, q.opt("package")) {
            manifest::check_name(p).map_err(|e| d.error(&e))?;
        }
        let target = q.opt("target").map(|t| t.string()).transpose()?;
        if let (Some(t), Some(d)) = (&target, q.opt("target")) {
            crate::platform::check(t).map_err(|e| d.error(&e))?;
        }
        if optional == Some(true) && (dev || native) {
            return Err(q.field_error("optional", "only a normal dependency is optional"));
        }
        let mut features = Vec::new();
        for f in q
            .opt("features")
            .map(|l| l.list())
            .transpose()?
            .unwrap_or_default()
        {
            let n = f.string()?;
            manifest::check_feature(&n).map_err(|e| f.error(&e))?;
            features.push(n);
        }
        if native && !features.is_empty() {
            return Err(q.field_error("features", "a native package has no features"));
        }
        deps.push(IndexDep {
            name,
            req,
            dev,
            native,
            registry,
            optional: optional.unwrap_or(false),
            features,
            default_features: q
                .opt("default_features")
                .map(|o| o.bool())
                .transpose()?
                .unwrap_or(true),
            package,
            target,
        });
    }
    let mut features = Features::new();
    for (k, v) in r
        .opt("features")
        .map(|f| f.dict())
        .transpose()?
        .unwrap_or_default()
    {
        let list = v
            .list()?
            .iter()
            .map(Decoder::string)
            .collect::<Result<_, _>>()?;
        features.push((k.string()?, list));
    }
    let normal: Vec<Dep> = deps
        .iter()
        .filter(|d| !d.dev && !d.native)
        .map(|d| d.to_dep(""))
        .collect();
    manifest::check_features(&features, &normal).map_err(|(_, e)| r.field_error("features", &e))?;
    let src = r.req("source")?;
    let Source::Git {
        url,
        reference: GitRef::Rev(rev),
    } = manifest::source(src)?
    else {
        return Err(
            src.error("a registry package's source is `Dep::git(\"url\", rev: \"<commit>\")`")
        );
    };
    // Not a SHA-256 (a newer form): as if absent, the git source serves.
    let archive = r
        .opt("archive")
        .and_then(|a| a.string().ok())
        .filter(|a| a.len() == 64 && a.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')));
    Ok(Entry {
        name,
        version,
        deps,
        features,
        url,
        rev,
        tree: string("tree")?,
        archive,
        yanked: r
            .opt("yanked")
            .map(|y| y.bool())
            .transpose()?
            .unwrap_or(false),
        nabi_version: r.opt("nabi_version").map(|t| t.string()).transpose()?,
        links: r.opt("links").map(|t| t.string()).transpose()?,
        official: r
            .opt("official")
            .map(|o| o.bool())
            .transpose()?
            .unwrap_or(false),
    })
}

/// Entries of one index file; blank lines skipped.
pub fn parse_file(text: &str, file: &str) -> Result<Vec<Entry>, String> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let at = format!("{file} (line {})", i + 1);
        let doc = Doc::parse(line, &at)?;
        // `Library { name: "std", .. }`: a standard library tier, which
        // ships with the toolchain and has no versions here.
        if doc.root().record("Library").is_ok() {
            continue;
        }
        out.push(entry(doc.root())?);
    }
    Ok(out)
}

impl Entry {
    /// The index line (canonical NABION on one line).
    pub fn to_line(&self) -> String {
        let deps = self
            .deps
            .iter()
            .map(|d| {
                let kind = match (d.dev, d.native) {
                    (true, _) => "dev",
                    (_, true) => "native",
                    _ => "normal",
                };
                let mut f = vec![
                    ("name", Value::str(&d.name)),
                    ("req", Value::str(&d.req)),
                    ("kind", Value::str(kind)),
                ];
                if let Some(r) = &d.registry {
                    f.push(("registry", Value::str(r)));
                }
                if d.optional {
                    f.push(("optional", Value::new(Kind::Bool(true))));
                }
                if !d.features.is_empty() {
                    f.push(("features", manifest::strings(&d.features)));
                }
                if !d.default_features {
                    f.push(("default_features", Value::new(Kind::Bool(false))));
                }
                if let Some(p) = &d.package {
                    f.push(("package", Value::str(p)));
                }
                if let Some(t) = &d.target {
                    f.push(("target", Value::str(t)));
                }
                Value::record(None, f)
            })
            .collect();
        let source = Source::Git {
            url: self.url.clone(),
            reference: GitRef::Rev(self.rev.clone()),
        };
        let features = (!self.features.is_empty()).then(|| {
            let kv = self
                .features
                .iter()
                .map(|(k, l)| (Value::str(k), manifest::strings(l)))
                .collect();
            ("features", Value::new(Kind::Dict(kv)))
        });
        nabic_nabion::to_line(&Value::record(
            Some("Package"),
            [
                ("name", Value::str(&self.name)),
                ("version", Value::str(self.version.to_string())),
                ("deps", Value::new(Kind::List(deps))),
            ]
            .into_iter()
            .chain(features)
            .chain([
                ("source", source.to_value()),
                ("tree", Value::str(&self.tree)),
            ])
            .chain(self.archive.iter().map(|a| ("archive", Value::str(a))))
            .chain([("yanked", Value::new(Kind::Bool(self.yanked)))])
            .chain((self.nabi_version.iter()).map(|t| ("nabi_version", Value::str(t))))
            .chain(self.links.iter().map(|l| ("links", Value::str(l))))
            .chain(
                self.official
                    .then(|| ("official", Value::new(Kind::Bool(true)))),
            )
            .collect(),
        ))
    }
}

/// The file of `name` under `prefix` (`""` / `"native/"`) in the cached
/// index, and its text. `a_b` finds `a-b` and back.
pub fn index_file(
    cache: &Cache,
    url: &str,
    prefix: &str,
    name: &str,
) -> Result<Option<(String, String)>, String> {
    for n in [
        name.to_owned(),
        name.replace('_', "-"),
        name.replace('-', "_"),
    ] {
        let file = format!("{prefix}{}", path(&n));
        if let Some(text) = cache.show(url, &file)? {
            return Ok(Some((file, text)));
        }
    }
    Ok(None)
}

/// Versions of package `name` in the cached index (none if absent).
pub fn entries(cache: &Cache, url: &str, name: &str) -> Result<Vec<Entry>, String> {
    let Some((file, text)) = index_file(cache, url, "", name)? else {
        return Ok(Vec::new());
    };
    let mut all = parse_file(&text, &format!("{url}: {file}"))?;
    all.retain(|e| manifest::same(&e.name, name));
    Ok(all)
}

/// The highest version matching `req` that is not yanked.
pub fn best<'a>(entries: &'a [Entry], req: &Req) -> Option<&'a Entry> {
    entries
        .iter()
        .filter(|e| !e.yanked && req.matches(&e.version))
        .max_by(|a, b| a.version.cmp(&b.version))
}

/// Whether a package is official: the flag of its latest version's line
/// (the highest not yanked; a release is above its pre-releases).
pub fn is_official(entries: &[Entry]) -> bool {
    entries
        .iter()
        .filter(|e| !e.yanked)
        .max_by(|a, b| a.version.cmp(&b.version))
        .is_some_and(|e| e.official)
}

/// `e` builds with this toolchain: its `nabi_version` admits `VERSION`
/// (none: any). One that does not parse is a newer nabi's form: no.
pub fn builds_here(e: &Entry) -> bool {
    (e.nabi_version.as_deref()).is_none_or(|v| crate::toolchain::builds_with(v, crate::VERSION))
}

/// Why `e` does not build here, and what to do about it.
pub fn needs_other(e: &Entry) -> String {
    let want = e.nabi_version.as_deref().unwrap_or_default();
    format!(
        "package `{}` {} needs {} (this is nabi {}); to use it, run `nabi +{want} ..` or pin \
         that toolchain (`toolchain: \"{want}\"` in nabi.nabion)",
        e.name,
        e.version,
        crate::toolchain::admits(want),
        crate::VERSION,
    )
}

/// `best` among the entries this toolchain builds, and a note when that
/// hides a newer match.
pub fn best_here<'a>(entries: &'a [Entry], req: &Req) -> (Option<&'a Entry>, Option<String>) {
    let pick = entries
        .iter()
        .filter(|e| !e.yanked && req.matches(&e.version) && builds_here(e))
        .max_by(|a, b| a.version.cmp(&b.version));
    let note = (best(entries, req))
        .filter(|n| pick != Some(*n))
        .map(needs_other);
    (pick, note)
}

/// The package (`native`: native package) names in the cached index `url`.
pub fn names(cache: &Cache, url: &str, native: bool) -> Result<Vec<String>, String> {
    Ok(cache
        .files(url)?
        .into_iter()
        .filter(|f| f.contains('/') && f.starts_with("native/") == native)
        .filter_map(|f| f.rsplit('/').next().map(str::to_owned))
        .collect())
}

/// What to do about `name`, which the index `url` lacks: the closest name
/// in it, and the search.
pub fn missing(cache: &Cache, url: &str, name: &str, native: bool) -> String {
    let all = names(cache, url, native).unwrap_or_default();
    let search = if native {
        format!("nabi search --native {name}")
    } else {
        format!("nabi search {name}")
    };
    let lower = name.to_ascii_lowercase().replace('_', "-");
    match nabic_core::suggest::suggest(&lower, all.iter().map(String::as_str)) {
        Some(near) => format!("\nhelp: did you mean `{near}`? (or search: `{search}`)"),
        None => format!("\nhelp: search the index: `{search}`"),
    }
}

/// "no package .." / "no version of .." for a requirement nothing meets.
pub fn no_match(cache: &Cache, entries: &[Entry], name: &str, req: &str, url: &str) -> String {
    if entries.is_empty() {
        let help = missing(cache, url, name, false);
        return format!("no package named `{name}` in {url}{help}");
    }
    let vs: Vec<String> = entries
        .iter()
        .map(|e| {
            let y = if e.yanked { " (yanked)" } else { "" };
            format!("{}{y}", e.version)
        })
        .collect();
    format!(
        "no version of package `{name}` matches `{req}` (in {url}: {})",
        vs.join(", ")
    )
}

/// The index `url`, fetched first unless `offline`.
pub fn open(home: &Path, url: &str, offline: bool, quiet: bool) -> Result<Cache, String> {
    let cache = Cache::index(home, sources(home)?);
    if !offline {
        if !quiet {
            nabic_core::term::status("Updating", &format!("index {url}"));
        }
        cache.fetch(url)?;
    } else if !cache.has(url) {
        return Err(format!(
            "the index {url} is not cached, and --offline is set"
        ));
    }
    Ok(cache)
}

/// The index entry for the package at `dir`: `origin`, HEAD commit and
/// tree. The tree must be clean, the package at the repository root.
pub fn entry_for(dir: &Path, m: &Manifest) -> Result<Entry, String> {
    m.check_publish()?;
    let name = m.name.clone().ok_or("nabi.nabion has no `name`")?;
    manifest::check_new_name(&name)?;
    let version = Version::parse(m.version.as_deref().ok_or("nabi.nabion has no `version`")?)?;
    // An executable-only package is for `nabi install`.
    if !dir.join("src/lib.nabi").is_file() && !dir.join("src/main.nabi").is_file() {
        return Err(format!(
            "`{name}` is not a package: no src/lib.nabi (a library) or src/main.nabi (an \
             executable)"
        ));
    }
    m.readme_path(dir)?;
    let mut deps = Vec::new();
    for d in &m.native {
        // Found on the user's machine; the published manifest names them.
        if matches!(
            d.source,
            manifest::NativeSource::System(_) | manifest::NativeSource::Framework
        ) {
            continue;
        }
        let manifest::NativeSource::Registry { req, registry } = &d.source else {
            return Err(format!(
                "native dependency `{}` is not in an index: a registry package's native packages are \
                 `Dep::native(\"<req>\")`",
                d.name
            ));
        };
        deps.push(IndexDep {
            name: d.name.clone(),
            req: req.clone(),
            dev: false,
            native: true,
            registry: registry.clone(),
            optional: false,
            features: Vec::new(),
            default_features: true,
            package: None,
            target: None,
        });
    }
    for (d, dev) in m
        .deps
        .iter()
        .map(|d| (d, false))
        .chain(m.dev_deps.iter().map(|d| (d, true)))
    {
        // A path dependency with a `version` is published as that requirement.
        let req = match (&d.source, &d.version) {
            (Source::Registry { req, registry }, _) => Some((req.clone(), registry.clone())),
            (Source::Path(_), Some(req)) => Some((req.clone(), None)),
            _ => None,
        };
        match (req, &d.source) {
            (Some((req, registry)), _) => deps.push(IndexDep {
                name: d.name.clone(),
                req,
                dev,
                native: false,
                registry,
                optional: d.optional,
                features: d.features.clone(),
                default_features: d.default_features,
                package: d.package.clone(),
                target: d.target.clone(),
            }),
            // A package's users never build its tests.
            _ if dev => {}
            (None, Source::Path(p)) => {
                return Err(format!(
                    "dependency `{}` is a path: a registry package depends only on registry \
                     packages; add the version it needs, published from that path: \
                     `Dep::path(\"{p}\", version: \"^0.1\")`",
                    d.name
                ));
            }
            _ => {
                return Err(format!(
                    "dependency `{}` is not from an index: a registry package depends only on \
                     registry packages (`Dep::version(..)`)",
                    d.name
                ));
            }
        }
    }
    let top = git::run(dir, &["rev-parse", "--show-toplevel"])?;
    let same = nabic_core::real_path(&top).ok() == nabic_core::real_path(dir).ok();
    if !same {
        return Err(format!(
            "the package must be at the root of its git repository ({top})"
        ));
    }
    if !git::run(dir, &["status", "--porcelain"])?.is_empty() {
        return Err("the work tree has uncommitted changes; commit them first".into());
    }
    // The url as configured: `remote get-url` applies the publisher's own
    // `insteadOf` rewrites, which other users do not have.
    let url = git::run(dir, &["config", "--get", "remote.origin.url"]).map_err(|_| {
        "the repository has no remote `origin` (where users fetch the source); add one with \
         `git remote add origin https://<host>/<owner>/<repo>`"
    })?;
    check_source(&url)?;
    let nabi_version = m.nabi_version.clone().ok_or_else(|| {
        nabic_nabion::with_help(
            "nabi.nabion has no `nabi_version`, the oldest nabi this package builds with; the \
             registry needs it",
            &format!(
                "add `nabi_version: \"{}\"` (this nabi), or the oldest one you have built it with",
                crate::VERSION
                    .split(['-', '+'])
                    .next()
                    .unwrap_or(crate::VERSION)
            ),
        )
    })?;
    if !crate::toolchain::builds_with(&nabi_version, crate::VERSION) {
        return Err(format!(
            "`nabi_version: \"{nabi_version}\"` admits {}, not this nabi {}; publish with one it \
             admits (`nabi +{nabi_version} publish`)",
            crate::toolchain::admits(&nabi_version),
            crate::VERSION
        ));
    }
    Ok(Entry {
        name,
        version,
        deps,
        features: m.features.clone(),
        url,
        rev: git::run(dir, &["rev-parse", "HEAD"])?,
        tree: git::run(dir, &["rev-parse", "HEAD^{tree}"])?,
        archive: None,
        yanked: false,
        nabi_version: Some(nabi_version),
        links: m.links.clone(),
        official: false,
    })
}

/// The most README text a publish sends.
pub const MAX_README: usize = 512 * 1024;

/// Whether glob `pat` matches `path` (`/`-separated, relative): `*` and
/// `?` within a name, `**` across names. A pattern without `/` matches a
/// name at any depth; one that matches a directory matches what is in it.
fn glob(pat: &str, path: &str) -> bool {
    fn one(p: &[u8], s: &[u8]) -> bool {
        match p {
            [] => s.is_empty(),
            [b'*', b'*', rest @ ..] => {
                let rest = rest.strip_prefix(b"/").unwrap_or(rest);
                if rest.is_empty() {
                    return true;
                }
                (0..=s.len()).any(|i| (i == 0 || s[i - 1] == b'/') && one(rest, &s[i..]))
            }
            [b'*', rest @ ..] => (0..=s.len())
                .take_while(|&i| i == 0 || s[i - 1] != b'/')
                .any(|i| one(rest, &s[i..])),
            [b'?', rest @ ..] => s.first().is_some_and(|&c| c != b'/') && one(rest, &s[1..]),
            [c, rest @ ..] => s.first() == Some(c) && one(rest, &s[1..]),
        }
    }
    let pat = pat.trim_start_matches("./").trim_end_matches('/');
    let anywhere = !pat.contains('/');
    let mut prefix = String::new();
    for part in path.split('/') {
        if !prefix.is_empty() {
            prefix.push('/');
        }
        prefix.push_str(part);
        // The path itself or a directory it is in.
        if one(pat.as_bytes(), prefix.as_bytes())
            || (anywhere && one(pat.as_bytes(), part.as_bytes()))
        {
            return true;
        }
    }
    false
}

/// Whether the package holds file `path`: `include` (empty: everything)
/// and not `exclude`. The manifest is always in.
pub fn packaged(m: &Manifest, path: &str) -> bool {
    path == manifest::MANIFEST
        || ((m.include.is_empty() || m.include.iter().any(|p| glob(p, path)))
            && !m.exclude.iter().any(|p| glob(p, path)))
}

/// A path the manifest names, inside the package and a file there.
fn inner_file(dir: &Path, field: &str, p: &str) -> Result<(), String> {
    let inside = !p.is_empty()
        && Path::new(p).components().all(|c| {
            matches!(
                c,
                std::path::Component::Normal(_) | std::path::Component::CurDir
            )
        });
    if !inside || !dir.join(p).is_file() {
        return Err(format!(
            "`{field}: {p:?}` in {}: not a file inside the package; write a path relative to it \
             (e.g. `{field}: \"LICENSE\"`)",
            manifest::MANIFEST
        ));
    }
    Ok(())
}

/// What `nabi publish` sends beside the index line: the manifest's
/// descriptive fields, whether it has a library and its executables, its
/// README text and the bytes of the files it holds at HEAD.
///
/// Fields this nabi does not know go as written.
pub fn meta(dir: &Path, m: &Manifest) -> Result<Value, String> {
    let text = |v: &Option<String>| v.as_deref().map(Value::str);
    let list = |v: &[String]| (!v.is_empty()).then(|| manifest::strings(v));
    if let Some(f) = &m.license_file {
        inner_file(dir, "license_file", f)?;
    }
    let lib = dir.join("src/lib.nabi").is_file();
    let bins: Vec<String> = if dir.join("src/main.nabi").is_file() {
        m.name.iter().cloned().collect()
    } else {
        Vec::new()
    };
    if let Some(r) = m.default_run.as_ref().filter(|r| !bins.contains(r)) {
        return Err(format!(
            "`default_run: {r:?}` in {}: the package's executables are [{}]; name one of them \
             or remove `default_run`",
            manifest::MANIFEST,
            bins.join(", ")
        ));
    }
    let readme_content = match m.readme_path(dir)? {
        Some(p) => {
            let t = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
            if t.len() > MAX_README {
                return Err(format!(
                    "{} is {} bytes; a registry package's README holds at most {MAX_README}: shorten it, or \
                     set `readme` to a shorter file",
                    p.display(),
                    t.len()
                ));
            }
            Some(Value::str(t))
        }
        None => None,
    };
    let mut size: u64 = 0;
    for row in git::run(dir, &["ls-tree", "-r", "-l", "-z", "HEAD"])?.split('\0') {
        // `<mode> blob <hash> <size>\t<path>`
        let Some((info, path)) = row.split_once('\t') else {
            continue;
        };
        if packaged(m, path) {
            size += info
                .split_whitespace()
                .nth(3)
                .and_then(|n| n.parse::<u64>().ok())
                .unwrap_or(0);
        }
    }
    let fields: Vec<(&str, Option<Value>)> = vec![
        ("description", text(&m.description)),
        ("license", text(&m.license)),
        ("license_file", text(&m.license_file)),
        ("authors", list(&m.authors)),
        ("homepage", text(&m.homepage)),
        ("documentation", text(&m.documentation)),
        ("repository", text(&m.repository)),
        ("readme", text(&m.readme)),
        ("keywords", list(&m.keywords)),
        ("categories", list(&m.categories)),
        ("include", list(&m.include)),
        ("exclude", list(&m.exclude)),
        ("publish", m.publish.map(|p| Value::new(Kind::Bool(p)))),
        ("metadata", m.metadata.clone()),
        ("links", text(&m.links)),
        ("default_run", text(&m.default_run)),
        ("has_lib", Some(Value::new(Kind::Bool(lib)))),
        ("bin_names", Some(manifest::strings(&bins))),
        ("readme_content", readme_content),
        ("size", Some(Value::new(Kind::Int(i128::from(size))))),
    ];
    let mut out: Vec<(&str, Value)> = fields
        .into_iter()
        .filter_map(|(k, v)| Some((k, v?)))
        .collect();
    out.extend(m.extra.iter().map(|(k, v)| (k.as_str(), v.clone())));
    Ok(Value::record(None, out))
}

/// A native package file's descriptive fields, as `meta`.
pub fn native_meta(e: &crate::native::Entry) -> Value {
    Value::record(
        None,
        e.meta.iter().map(|(k, v)| (*k, Value::str(v))).collect(),
    )
}

/// A local checkout of an index: file `rel` there, and its text.
pub fn local(index: &Path, rel: &str) -> Result<(PathBuf, String), String> {
    if !index.join(".git").exists() {
        return Err(format!(
            "{} is not a git checkout of an index",
            index.display()
        ));
    }
    let file = index.join(rel);
    let text = match std::fs::read_to_string(&file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("{}: {e}", file.display())),
    };
    Ok((file, text))
}

pub fn commit(index: &Path, file: &Path, text: &str, msg: &str) -> Result<(), String> {
    std::fs::create_dir_all(file.parent().unwrap_or(index))
        .and_then(|()| std::fs::write(file, text))
        .map_err(|e| format!("{}: {e}", file.display()))?;
    let rel = file
        .strip_prefix(index)
        .unwrap_or(file)
        .display()
        .to_string();
    git::run(index, &["add", "--", &rel])?;
    git::run(index, &["commit", "--quiet", "-m", msg, "--", &rel]).map(drop)
}

/// Add `e` to the index checkout (checked); with `write`, as a commit.
/// Returns the file.
pub fn publish(index: &Path, e: &Entry, write: bool) -> Result<PathBuf, String> {
    let (file, text) = local(index, &path(&e.name))?;
    let have = parse_file(&text, &file.display().to_string())?;
    let names = have.iter().map(|o| o.name.clone());
    taken(index, "", &e.name, names)?;
    if have.iter().any(|o| o.version == e.version) {
        return Err(format!("{} {} is already in the index", e.name, e.version));
    }
    if write {
        let mut out = text;
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&e.to_line());
        out.push('\n');
        commit(
            index,
            &file,
            &out,
            &format!("Publish {} {}", e.name, e.version),
        )?;
    }
    Ok(file)
}

/// Fail when the checkout has `name` under another spelling (`a-b`,
/// `a_b`, case): one package, but each spelling would get its own file.
pub fn taken(
    index: &Path,
    prefix: &str,
    name: &str,
    have: impl Iterator<Item = String>,
) -> Result<(), String> {
    let files = git::run(index, &["ls-files"])?;
    let other = files
        .lines()
        .filter(|f| f.starts_with("native/") == (prefix == "native/"))
        .filter_map(|f| f.rsplit('/').next().map(str::to_owned))
        .chain(have)
        .find(|n| n != name && manifest::same(n, name));
    match other {
        Some(o) => Err(format!(
            "the index has {}`{o}`; `{name}` is the same name (`-` / `_` and case are one)",
            if prefix.is_empty() {
                "package "
            } else {
                "native package "
            }
        )),
        None => Ok(()),
    }
}

/// Set `yanked` on `name` `version` in the index checkout, as a commit.
pub fn yank(
    index: &Path,
    name: &str,
    vers: &str,
    yanked: bool,
    native: bool,
) -> Result<(), String> {
    let vers = Version::parse(vers)?;
    let rel = if native {
        crate::native::path(name)
    } else {
        path(name)
    };
    let (file, text) = local(index, &rel)?;
    let shown = file.display().to_string();
    let mut found = false;
    let mut out = String::new();
    for (i, line) in text.lines().enumerate() {
        let at = format!("{shown} (line {})", i + 1);
        // The line's name and version, and the line with `yanked` set.
        let e = if line.trim().is_empty() {
            None
        } else if native {
            let mut e = crate::native::parse_one(line, &at)?;
            e.yanked = yanked;
            Some((e.name.clone(), e.version.clone(), e.to_line()))
        } else {
            let mut e = entry(Doc::parse(line, &at)?.root())?;
            e.yanked = yanked;
            Some((e.name.clone(), e.version.clone(), e.to_line()))
        };
        match e {
            Some((n, v, l)) if manifest::same(&n, name) && v == vers => {
                found = true;
                out.push_str(&l);
            }
            _ => out.push_str(line),
        }
        out.push('\n');
    }
    let what = if native { "native " } else { "" };
    if !found {
        return Err(format!(
            "{what}{name} {vers} is not in the index {}",
            index.display()
        ));
    }
    if out == text {
        return Ok(());
    }
    let verb = if yanked { "Yank" } else { "Unyank" };
    commit(index, &file, &out, &format!("{verb} {what}{name} {vers}"))
}

/// A token per index: `Tokens { tokens: { "<index url>": "<token>" } }`.
fn tokens_file(home: &Path) -> PathBuf {
    home.join("tokens.nabion")
}

/// The key of index `url` in `tokens.nabion`.
fn index_key(url: &str) -> &str {
    url.trim_end_matches('/')
}

/// Whether index URLs `a` and `b` are one index.
pub fn same_index(a: &str, b: &str) -> bool {
    index_key(a) == index_key(b)
}

/// A token goes into an HTTP header: printable ASCII, no spaces.
pub fn check_token(token: &str) -> Result<(), String> {
    if token.is_empty() || !token.bytes().all(|b| b.is_ascii_graphic()) {
        return Err("a token is printable ASCII without spaces".into());
    }
    Ok(())
}

/// The token for `index`: `$NABI_REGISTRY_TOKEN` for [`DEFAULT`], else
/// `nabi login`'s for that index. `login` is the command that logs in to
/// it.
pub fn token(home: &Path, index: &str, login: &str) -> Result<String, String> {
    pick_token(
        std::env::var("NABI_REGISTRY_TOKEN").ok(),
        home,
        index,
        login,
    )
}

fn pick_token(
    env: Option<String>,
    home: &Path,
    index: &str,
    login: &str,
) -> Result<String, String> {
    if let Some(t) = env.filter(|t| !t.is_empty()) {
        if same_index(index, DEFAULT) {
            check_token(&t).map_err(|e| format!("NABI_REGISTRY_TOKEN: {e}"))?;
            return Ok(t);
        }
        crate::warn_once(&format!(
            "NABI_REGISTRY_TOKEN goes only to {DEFAULT}; for {index} run `{login}` (its token \
             goes in tokens.nabion)"
        ));
    }
    keyed_token(home, index)?
        .ok_or_else(|| format!("publishing needs a token: run `{login}` first"))
}

/// `tokens.nabion`'s token for `index`, if any.
fn keyed_token(home: &Path, index: &str) -> Result<Option<String>, String> {
    let path = tokens_file(home);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    // Parse errors would quote the line holding a token.
    let bad = |_| format!("{} is malformed; run `nabi login` again", path.display());
    let doc = Doc::parse(&text, "tokens").map_err(bad)?;
    let r = doc.root().record("Tokens").map_err(bad)?;
    crate::skipped(&path.display().to_string(), &r.known(&["tokens"]));
    let Some(d) = r.get("tokens") else {
        return Ok(None);
    };
    for (k, v) in d.dict().map_err(bad)? {
        if index_key(&k.string().map_err(bad)?) == index_key(index) {
            let t = v.string().map_err(bad)?;
            check_token(&t).map_err(bad)?;
            return Ok(Some(t));
        }
    }
    Ok(None)
}

/// Store `token` for `index` in `$NABI_HOME/tokens.nabion` (0600 on Unix);
/// its path.
pub fn save_token(home: &Path, index: &str, token: &str) -> Result<PathBuf, String> {
    std::fs::create_dir_all(home).map_err(|e| format!("{}: {e}", home.display()))?;
    let path = tokens_file(home);
    let step = [
        nabic_nabion::edit::Step::Field("tokens"),
        nabic_nabion::edit::Step::Key(index_key(index)),
    ];
    let set = |old: &str| nabic_nabion::edit::set(old, &step, &nabic_nabion::quote(token)).ok();
    // Fields and other indexes stay; a file that does not read is replaced.
    let text = std::fs::read_to_string(&path)
        .ok()
        .filter(|old| record_is(old, "Tokens"))
        .and_then(|old| set(&old))
        .or_else(|| set("Tokens {}\n"))
        .ok_or_else(|| format!("cannot write {}", path.display()))?;
    write_private(&path, &text)?;
    Ok(path)
}

/// Write `text` to `path` anew, so a 0600 mode applies on Unix.
fn write_private(path: &Path, text: &str) -> Result<(), String> {
    let _ = std::fs::remove_file(path);
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    o.open(path)
        .and_then(|mut f| f.write_all(text.as_bytes()))
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// Whether `text` is a `<name> { .. }`.
fn record_is(text: &str, name: &str) -> bool {
    Doc::parse(text, "nabion").is_ok_and(|d| d.root().record(name).is_ok())
}

/// Remove `nabi login`'s token for `index` (`None`: for every index);
/// whether there was one.
pub fn logout(home: &Path, index: Option<&str>) -> Result<bool, String> {
    let path = tokens_file(home);
    let Ok(old) = std::fs::read_to_string(&path) else {
        return Ok(false);
    };
    let key = index.map(index_key);
    let mut step = vec![nabic_nabion::edit::Step::Field("tokens")];
    step.extend(key.map(nabic_nabion::edit::Step::Key));
    // Errors would quote the line holding a token.
    let left = nabic_nabion::edit::remove(&old, &step)
        .map_err(|_| format!("{} is malformed; delete it", path.display()))?;
    let Some(text) = left else {
        return Ok(false);
    };
    write_private(&path, &text)?;
    Ok(true)
}

/// The registry API of the cached index `url`, read without a fetch:
/// where its source archives are (`None`: it names none).
pub fn api_cached(cache: &Cache, url: &str) -> Option<String> {
    let text = cache.show(url, "config.nabion").ok()??;
    let doc = Doc::parse(&text, "config.nabion").ok()?;
    let a = doc
        .root()
        .record("Config")
        .ok()?
        .req("api")
        .ok()?
        .string()
        .ok()?;
    check_api(&a).ok()?;
    Some(a.trim_end_matches('/').to_owned())
}

/// The index's registry API (`config.nabion` `api`, no trailing `/`).
/// HTTPS only, except loopback.
pub fn api(home: &Path, url: &str, quiet: bool) -> Result<String, String> {
    let cache = open(home, url, false, quiet)?;
    let text = cache.show(url, "config.nabion")?.ok_or_else(|| {
        format!("the index {url} has no config.nabion naming its API; use --index-dir <checkout>")
    })?;
    let file = format!("{url}: config.nabion");
    let doc = Doc::parse(&text, &file)?;
    let a = doc.root().record("Config")?.req("api")?.string()?;
    check_api(&a).map_err(|e| format!("{url}: config.nabion: {e}"))?;
    Ok(a.trim_end_matches('/').to_owned())
}

/// `https://`, or plain http to this machine (tests, a local server).
fn https_or_local(a: &str) -> bool {
    a.starts_with("https://")
        || ["http://127.0.0.1", "http://localhost"].iter().any(|h| {
            a.strip_prefix(h)
                .is_some_and(|r| r.is_empty() || r.starts_with([':', '/']))
        })
}

fn check_api(a: &str) -> Result<(), String> {
    if https_or_local(a) {
        Ok(())
    } else {
        Err(format!("api `{a}` must be an https:// URL"))
    }
}

/// Everyone fetches a registry package's source: it must be an https:// URL. For an
/// ssh remote the error names the https form of the same repository.
fn check_source(url: &str) -> Result<(), String> {
    if https_or_local(url) {
        return Ok(());
    }
    // `ssh://[user@]host[:port]/path` and `[user@]host:path`.
    let ssh = match url.strip_prefix("ssh://") {
        Some(r) => r
            .split_once('/')
            .map(|(h, p)| (h.split(':').next().unwrap_or(h), p)),
        None if !url.contains("://") => url.split_once(':').filter(|(h, _)| h.contains('@')),
        None => None,
    };
    let https = match ssh {
        Some((host, path)) => {
            let host = host.rsplit('@').next().unwrap_or(host);
            format!("https://{host}/{}", path.trim_start_matches('/'))
        }
        None => "https://<host>/<owner>/<repo>".into(),
    };
    Err(format!(
        "remote `origin` is `{url}`, which other users may not be able to fetch: a \
         registry package's source must be an https:// URL; run `git remote set-url origin {https}`"
    ))
}

/// `GET` (or `POST` of `body`) `{api}{path}`, with a bearer token when
/// given; the reply on 2xx, else its `Error { message }`. The token goes
/// to curl on stdin, never argv, and is stripped from errors.
fn call(api: &str, path: &str, token: Option<&str>, body: Option<&str>) -> Result<String, String> {
    // A curl config file: values quoted, `\` and `"` escaped.
    let q = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
    let mut config = String::new();
    if let Some(t) = token {
        config += &format!("header = {}\n", q(&format!("Authorization: Bearer {t}")));
    }
    if let Some(b) = body {
        config += "header = \"Content-Type: application/nabion; charset=utf-8\"\n";
        config += &format!("data-raw = {}\n", q(b));
    }
    let url = format!("{api}{path}");
    let hide = |e: String| match token {
        Some(t) => e.replace(t, "***"),
        None => e,
    };
    let mut child = Command::new("curl")
        .args(["-sS", "--proto", "=https,http", "-w", "\n%{http_code}"])
        .args(["--config", "-"])
        .arg(&url)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run `curl` (is it installed?): {e}"))?;
    // Dropped after writing: curl reads to the end.
    let sent = child
        .stdin
        .take()
        .map(|mut i| i.write_all(config.as_bytes()));
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if !out.status.success() || !matches!(sent, Some(Ok(()))) {
        return Err(hide(format!(
            "cannot reach {url}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let all = String::from_utf8_lossy(&out.stdout);
    let (text, code) = all.rsplit_once('\n').unwrap_or(("", &all));
    let code: u16 = code.trim().parse().unwrap_or(0);
    if (200..300).contains(&code) {
        return Ok(text.to_owned());
    }
    if code == 401 {
        return Err(format!(
            "{api} refused the token: create one at {api}/me, then run `nabi login`"
        ));
    }
    let message = Doc::parse(text, &url).ok().and_then(|d| {
        let r = d.root().record("Error").ok()?;
        r.req("message").and_then(|m| m.string()).ok()
    });
    Err(hide(match message {
        Some(m) => format!("{api}: {m}"),
        None => format!("{url}: HTTP status {code}"),
    }))
}

/// The login `token` belongs to, and whether it may publish.
pub fn me(api: &str, token: &str) -> Result<(String, bool), String> {
    let text = call(api, "/api/v1/me", Some(token), None)?;
    let file = format!("{api}/api/v1/me");
    let doc = Doc::parse(&text, &file)?;
    let r = doc.root().record("Me")?;
    Ok((r.req("login")?.string()?, r.req("publish")?.bool()?))
}

/// What the registry answered a publish: the name and version taken, and
/// its warnings.
pub struct Published {
    pub name: String,
    pub version: String,
    pub warnings: Vec<String>,
}

/// Publish index line `line` with its `meta` through `api`: the body is
/// `Publish { entry: <line>, meta: { .. } }`. Fields of the reply this
/// nabi does not know are skipped.
pub fn publish_api(api: &str, token: &str, line: &str, meta: Value) -> Result<Published, String> {
    let entry = nabic_nabion::parse(line).map_err(|e| e.render(line, "index line"))?;
    let body = nabic_nabion::to_line(&Value::record(
        Some("Publish"),
        vec![("entry", entry), ("meta", meta)],
    ));
    let text = call(api, "/api/v1/publish", Some(token), Some(&body))?;
    let file = format!("{api}/api/v1/publish");
    let doc = Doc::parse(&text, &file)?;
    let r = doc.root().record("Published")?;
    let mut warnings = Vec::new();
    for w in r
        .opt("warnings")
        .map(|w| w.list())
        .transpose()?
        .unwrap_or_default()
    {
        warnings.push(w.string()?);
    }
    Ok(Published {
        name: r.req("name")?.string()?,
        version: r.req("version")?.string()?,
        warnings,
    })
}

/// Yank (`undo`: unyank) `name` `version` through `api`; `message`
/// says why (a yank only).
pub fn yank_api(
    api: &str,
    token: &str,
    name: &str,
    version: &str,
    undo: bool,
    native: bool,
    message: Option<&str>,
) -> Result<(), String> {
    let version = Version::parse(version)?.to_string();
    let flag = |b| Value::new(Kind::Bool(b));
    let mut fields = vec![
        ("name", Value::str(name)),
        ("version", Value::str(version)),
        ("undo", flag(undo)),
        ("native", flag(native)),
    ];
    if let Some(m) = message {
        fields.push(("message", Value::str(m)));
    }
    let body = nabic_nabion::to_line(&Value::record(Some("Yank"), fields));
    call(api, "/api/v1/yank", Some(token), Some(&body)).map(drop)
}

/// The description of package (`native`: native package) `name` at
/// `version`, from the public `GET /api/v1/packages/<name>`; `None` when
/// the API has none or does not answer.
pub fn description(api: &str, name: &str, version: &str, native: bool) -> Option<String> {
    let kind = if native { "native" } else { "packages" };
    let text = call(api, &format!("/api/v1/{kind}/{name}"), None, None).ok()?;
    let doc = Doc::parse(&text, "api").ok()?;
    let r = doc.root().record("Package").ok()?;
    r.req("versions")
        .ok()?
        .list()
        .ok()?
        .into_iter()
        .find_map(|v| {
            let v = v.record("Version").ok()?;
            if v.req("version").ok()?.string().ok()? != version {
                return None;
            }
            v.opt("meta")?
                .record("Meta")
                .ok()?
                .opt("description")?
                .string()
                .ok()
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn tokens() {
        for a in [
            "https://packages.nabi-lang.org",
            "http://127.0.0.1:8080",
            "http://localhost",
        ] {
            assert!(check_api(a).is_ok(), "{a}");
        }
        for a in [
            "http://packages.nabi-lang.org",
            "http://127.0.0.1.x",
            "file:///x",
            "ftp://x",
        ] {
            assert!(check_api(a).is_err(), "{a}");
        }
        assert!(check_source("https://github.com/o/geom").is_ok());
        for (url, fix) in [
            ("git@github.com:o/geom.git", "https://github.com/o/geom.git"),
            (
                "ssh://git@github.com:22/o/geom",
                "https://github.com/o/geom",
            ),
            ("file:///srv/geom", "https://<host>/<owner>/<repo>"),
            ("/srv/geom", "https://<host>/<owner>/<repo>"),
            ("http://github.com/o/geom", "https://<host>/<owner>/<repo>"),
        ] {
            let e = check_source(url).unwrap_err();
            assert!(
                e.contains(&format!("run `git remote set-url origin {fix}`")),
                "{e}"
            );
        }
        let home = nabic_core::tmp::test_dir("token");
        let pick =
            |env: Option<&str>| pick_token(env.map(Into::into), &home, DEFAULT, "nabi login");
        let e = pick(None).unwrap_err();
        assert!(e.contains("run `nabi login` first"), "{e}");
        let file = save_token(&home, DEFAULT, "ghp_file").unwrap();
        assert_eq!(pick(None).unwrap(), "ghp_file");
        // The environment wins over the file.
        assert_eq!(pick(Some("ghp_env")).unwrap(), "ghp_env");
        assert_eq!(pick(Some("")).unwrap(), "ghp_file");
        assert!(pick(Some("a b")).is_err());
        #[cfg(unix)]
        {
            let mode = std::fs::metadata(&file).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        std::fs::write(&file, "Tokens { tokens: 1 }").unwrap();
        let e = pick(None).unwrap_err();
        assert!(e.contains("malformed"), "{e}");
        std::fs::write(&file, "Tokens { tokens: { \"a\": \"ghp_secret\" x } }").unwrap();
        let e = pick(None).unwrap_err();
        assert!(e.contains("malformed") && !e.contains("ghp_secret"), "{e}");
        std::fs::remove_file(&file).unwrap();
        save_token(&home, DEFAULT, "ghp_new").unwrap();
        assert!(logout(&home, Some(DEFAULT)).unwrap());
        assert!(!logout(&home, Some(DEFAULT)).unwrap());
        let _ = std::fs::remove_dir_all(&home);
    }

    /// Each index gets its own token; `NABI_REGISTRY_TOKEN` goes only to
    /// the default index.
    #[test]
    fn token_per_index() {
        let home = nabic_core::tmp::test_dir("token-index");
        let (a, b) = ("https://a.example/index", "https://b.example/index/");
        let pick = |i: &str| pick_token(None, &home, i, "nabi login --registry x");
        save_token(&home, a, "ghp_a").unwrap();
        save_token(&home, b, "ghp_b").unwrap();
        assert_eq!(pick(a).unwrap(), "ghp_a");
        // Keys ignore a trailing `/`.
        assert_eq!(pick("https://b.example/index").unwrap(), "ghp_b");
        let e = pick(DEFAULT).unwrap_err();
        assert!(e.contains("run `nabi login --registry x` first"), "{e}");
        let env = pick_token(Some("ghp_env".into()), &home, a, "nabi login --registry x");
        assert_eq!(env.unwrap(), "ghp_a");
        let env = pick_token(
            Some("ghp_env".into()),
            &home,
            "https://c.example",
            "nabi login",
        );
        assert!(env.is_err());
        assert!(logout(&home, Some(b)).unwrap());
        assert!(!logout(&home, Some(b)).unwrap());
        assert!(pick(b).is_err());
        assert!(logout(&home, None).unwrap());
        assert!(pick(a).is_err());
        let _ = std::fs::remove_dir_all(&home);
    }

    /// A newer nabi's fields in tokens.nabion: read past, kept by `nabi
    /// login` and `nabi logout`.
    #[test]
    fn tokens_skip_unknown() {
        let home = nabic_core::tmp::test_dir("token-newer");
        let pick = || pick_token(None, &home, DEFAULT, "nabi login");
        let keyed = tokens_file(&home);
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            &keyed,
            format!("Tokens {{ tokens: {{ \"{DEFAULT}\": \"ghp_k\" }}, more: 1 }}\n"),
        )
        .unwrap();
        assert_eq!(pick().unwrap(), "ghp_k");
        save_token(&home, "https://x", "ghp_x").unwrap();
        let text = std::fs::read_to_string(&keyed).unwrap();
        assert!(text.contains("more: 1") && text.contains("ghp_k"), "{text}");
        assert!(logout(&home, Some(DEFAULT)).unwrap());
        let text = std::fs::read_to_string(&keyed).unwrap();
        assert!(text.contains("ghp_x") && !text.contains("ghp_k"), "{text}");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// `sources.nabion` maps an index to its mirror.
    #[test]
    fn sources_replace() {
        let home = nabic_core::tmp::test_dir("sources");
        assert!(sources(&home).unwrap().is_empty());
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join("sources.nabion"),
            format!("Sources {{ replace: {{ \"{DEFAULT}/\": \"file:///m\" }}, later: 1 }}\n"),
        )
        .unwrap();
        let s = sources(&home).unwrap();
        assert_eq!(s, [(DEFAULT.to_owned(), "file:///m".to_owned())]);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn globs() {
        for (p, path, want) in [
            ("src/**", "src/a/b.nabi", true),
            ("src/**", "srcx/a.nabi", false),
            ("*.md", "docs/a.md", true),
            ("bench", "bench/x/y.nabi", true),
            ("docs/*.md", "docs/a/b.md", false),
            ("**/*.png", "a/b/c.png", true),
            ("?.nabi", "ab.nabi", false),
        ] {
            assert_eq!(glob(p, path), want, "{p} {path}");
        }
    }

    #[test]
    fn index_new_names() {
        assert_eq!(path("a"), "1/a");
        assert_eq!(path("ab"), "2/ab");
        assert_eq!(path("Abc"), "3/a/abc");
        assert_eq!(path("geom_x"), "ge/om/geom_x");
        let e = Entry {
            name: "geom".into(),
            version: Version::parse("0.3.1").unwrap(),
            deps: vec![IndexDep {
                name: "num".into(),
                req: "^1.2".into(),
                dev: false,
                native: false,
                registry: None,
                optional: false,
                features: Vec::new(),
                default_features: true,
                package: None,
                target: None,
            }],
            features: Vec::new(),
            url: "https://github.com/o/geom".into(),
            rev: "c".repeat(40),
            tree: "t".repeat(40),
            archive: None,
            yanked: false,
            nabi_version: Some(crate::VERSION.split('-').next().unwrap().to_owned()),
            links: None,
            official: false,
        };
        let s = e.nabi_version.clone().unwrap();
        let line = e.to_line();
        assert_eq!(
            line,
            format!(
                "Package {{ name: \"geom\", version: \"0.3.1\", deps: [{{ name: \"num\", req: \
                 \"^1.2\", kind: \"normal\" }}], source: Dep::git(\"https://github.com/o/geom\", \
                 rev: \"{}\"), tree: \"{}\", yanked: false, nabi_version: \"{s}\" }}",
                "c".repeat(40),
                "t".repeat(40)
            )
        );
        let two = format!("{line}\n\n{}\n", line.replace("0.3.1", "0.4.0"));
        let back = parse_file(&two, "ge/om/geom").unwrap();
        assert_eq!(back[0], e);
        assert_eq!(back[1].version.to_string(), "0.4.0");
        assert_eq!(best(&back, &Req::parse("^0.3").unwrap()), Some(&back[0]));
        // `nabi_version` and `links` round-trip; an entry for another
        // series is skipped.
        let mut other = back[1].clone();
        other.nabi_version = Some("99.0.0".into());
        other.links = Some("z".into());
        let tl = other.to_line();
        assert!(
            tl.ends_with("nabi_version: \"99.0.0\", links: \"z\" }"),
            "{tl}"
        );
        assert_eq!(parse_file(&tl, "f").unwrap()[0], other);
        // The registry's `archive` follows `tree`.
        let mut kept = back[1].clone();
        kept.archive = Some("a".repeat(64));
        let kl = kept.to_line();
        assert!(
            kl.contains(&format!("\", archive: \"{}\", yanked:", "a".repeat(64))),
            "{kl}"
        );
        assert_eq!(parse_file(&kl, "f").unwrap()[0], kept);
        let mut off = kept;
        off.official = true;
        let ol = off.to_line();
        assert!(ol.ends_with(", official: true }"), "{ol}");
        assert_eq!(parse_file(&ol, "f").unwrap()[0], off);
        assert!(parse_file(&ol.replace("official: true", "official: 1"), "f").is_err());
        let odd = parse_file(&kl.replace(&"a".repeat(64), "abc"), "f").unwrap();
        assert_eq!(odd[0].archive, None);
        let both = [back[0].clone(), other];
        let (pick, note) = best_here(&both, &Req::parse("*").unwrap());
        assert_eq!(pick.map(|e| &e.version), Some(&both[0].version));
        let note = note.unwrap();
        assert!(
            note.contains("`geom` 0.4.0 needs nabi 99.0.0 or a later 99.x"),
            "{note}"
        );
        // A `nabi_version` of a newer nabi's form builds nowhere here.
        assert!(!builds_here(&Entry {
            nabi_version: Some("banana".into()),
            ..back[0].clone()
        }));
        // Features, renames and targets round-trip; only what differs
        // from the defaults is written.
        let mut f = e;
        f.deps[0].optional = true;
        f.deps[0].features = vec!["fast".into()];
        f.deps[0].default_features = false;
        f.deps[0].package = Some("numeric".into());
        f.deps[0].target = Some("cfg(target_os = \"linux\")".into());
        f.features = vec![
            ("default".into(), vec!["simd".into()]),
            ("simd".into(), vec!["dep:num".into(), "num?/x".into()]),
        ];
        let fl = f.to_line();
        assert!(
            fl.contains(
                "kind: \"normal\", optional: true, features: [\"fast\"], default_features: \
                 false, package: \"numeric\", target: \"cfg(target_os = \\\"linux\\\")\" }], \
                 features: { \"default\": [\"simd\"], \"simd\": [\"dep:num\", \"num?/x\"] }, \
                 source:"
            ),
            "{fl}"
        );
        assert_eq!(parse_file(&fl, "f").unwrap()[0], f);
        let bad = fl.replace("\"dep:num\"", "\"dep:nope\"");
        let err = parse_file(&bad, "f").unwrap_err();
        assert!(err.contains("there is no dependency `nope`"), "{err}");
        let e = parse_file(&format!("\n{}", line.replace("rev:", "tag:")), "f").unwrap_err();
        assert!(e.contains("--> f (line 2):1:"), "{e}");
        assert!(e.contains("a registry package's source"), "{e}");
    }
}
