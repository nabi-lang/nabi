//! `nabi.nabion` (a package's or an environment's) and a script's
//! `//! ```nabion` block: one data-only NABION value, `Package { .. }` (the
//! name is optional).
//!
//! ```nabion
//! Package {
//!     name: "app",
//!     version: "0.1.0",
//!     nabi_version: "0.3.1",            // the oldest nabi it builds with (0.3.1 to 0.3.x)
//!     toolchain: "0.3",                 // the toolchain this project builds with
//!     description: "A JSON app",
//!     license: "MIT OR Apache-2.0",
//!     repository: "https://example.com/app.git",
//!     authors: ["Ann <ann@example.com>"],
//!     homepage: "https://example.com/app",
//!     documentation: "https://example.com/app/docs",
//!     readme: "README.md",              // the default when README.md exists
//!     keywords: ["json", "parser"],     // at most 5
//!     categories: ["encoding"],         // at most 5
//!     license_file: "LICENSE",          // a license file instead of an SPDX `license`
//!     include: ["src/**", "README.md"], // what a publish holds (default: every file)
//!     exclude: ["bench/**"],            // files left out of it
//!     links: "z",                       // the native library it links, one package per name
//!     default_run: "app",               // the executable `nabi run` runs
//!     deployment_target: "13.0",        // the oldest macOS it runs on (default 11.0);
//!                                       // { macos: "13.0", ios: "16.0" } (iOS: 13.4)
//!     metadata: { docs: { all_features: true } },  // free-form, for other tools
//!     publish: true,                    // false: `nabi publish` refuses
//!     deps: {
//!         "json": Dep::git("https://example.com/json.git", tag: "v1.2"),
//!         "util": Dep::path("../util"),
//!         "geom": Dep::version("^0.3"),   // from the index; also `"geom": "^0.3"`
//!         "tls": Dep::version("^2", optional: true, default_features: false),
//!         "j": Dep::version("^1", package: "json5"),   // `json5`, named `j` in code
//!         "nix": Dep::version("^0.4", target: "cfg(target_family = \"unix\")"),
//!         "noise": Dep::version("^1.2", bundle: true),  // a plugin links it in
//!         "physics": Dep::path("../physics", plugin: true),  // built as a plugin beside the program
//!     },
//!     features: { "default": ["json"], "json": [], "tls": ["dep:tls", "geom/fast"] },
//!     dev_deps: {},
//!     lints_from: "..",                 // ../nabi.nabion's lints first
//!     lints: { "dead_code": "allow" },
//!     profile: { "release": { overflow_checks: false } },
//!     native: {                              // C libraries (`native`)
//!         "zlib": "^1.3",                    // = Dep::native("^1.3"), prebuilt
//!         "png": Dep::native(url: "vendor/png.nabion", sha256: "<hex>"),
//!         "z": Dep::system(pkg_config: "zlib", apt: "zlib1g-dev"),  // installed
//!         "Foundation": Dep::framework(),    // an Apple framework
//!     },
//!     lib: { kind: "cdylib" },               // a shared library C loads
//!                                            // ("staticlib": one .a C links)
//!     c: { sources: ["c/shim.c"], include: ["c"], define: { "N": "1" } },
//! }
//! ```

mod check;
mod parse;
mod write;

pub use crate::manifest::check::*;
pub use crate::manifest::parse::*;
pub use crate::manifest::write::*;

use nabic_core::lint::Level;
use nabic_core::term::{self, Role, Stream};
use nabic_nabion::edit::{self, Step};
use nabic_nabion::{Decoder, Doc, Field, Kind, Value};
use std::path::{Path, PathBuf};

pub const MANIFEST: &str = "nabi.nabion";
pub const LOCK: &str = "nabi.lock";
/// A manifest with nothing in it, for `nabi add` to fill.
pub const EMPTY: &str = "Package {\n    deps: {},\n}\n";

/// `keywords` and `categories` hold at most this many.
pub const MAX_KEYWORDS: usize = 5;

const FIELDS: [&str; 30] = [
    "name",
    "version",
    "nabi_version",
    "toolchain",
    "description",
    "license",
    "license_file",
    "repository",
    "authors",
    "homepage",
    "documentation",
    "readme",
    "keywords",
    "categories",
    "include",
    "exclude",
    "links",
    "default_run",
    "deployment_target",
    "metadata",
    "publish",
    "deps",
    "dev_deps",
    "lints",
    "lints_from",
    "native",
    "features",
    "profile",
    "lib",
    "c",
];

/// The build profiles `profile` may set.
const PROFILES: [&str; 2] = ["debug", "release"];

/// A dependency's feature arguments (`Dep::..(.., optional: true)`).
const DEP_FLAGS: [&str; 7] = [
    "optional",
    "features",
    "default_features",
    "package",
    "target",
    "plugin",
    "bundle",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitRef {
    Default,
    Rev(String),
    Tag(String),
    Branch(String),
}

impl GitRef {
    /// `(name, value)` of the `Dep::git(.., name: value)` argument.
    pub fn pair(&self) -> Option<(&'static str, &str)> {
        match self {
            Self::Default => None,
            Self::Rev(r) => Some(("rev", r)),
            Self::Tag(t) => Some(("tag", t)),
            Self::Branch(b) => Some(("branch", b)),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// As written: relative to the manifest's directory.
    Path(String),
    Git {
        url: String,
        reference: GitRef,
    },
    /// A registry package: a version requirement (`crate::semver::Req`
    /// syntax) on a registry index (`None`: the configured one).
    Registry {
        req: String,
        registry: Option<String>,
    },
}

impl Source {
    /// The `Dep::..` value.
    pub fn to_value(&self) -> Value {
        match self {
            Self::Path(p) => Value::call("Dep::path", vec![Value::str(p)], Vec::new()),
            Self::Git { url, reference } => Value::call(
                "Dep::git",
                vec![Value::str(url)],
                reference
                    .pair()
                    .map(|(k, v)| (k, Value::str(v)))
                    .into_iter()
                    .collect(),
            ),
            Self::Registry { req, registry } => Value::call(
                "Dep::version",
                vec![Value::str(req)],
                registry
                    .iter()
                    .map(|r| ("registry", Value::str(r)))
                    .collect(),
            ),
        }
    }

    /// The `Dep::..` text for `nabi.nabion`, on one line (it is spliced in).
    pub fn to_nabi(&self) -> String {
        nabic_nabion::to_line(&self.to_value())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dep {
    pub name: String,
    pub source: Source,
    /// Built only when a feature enables it (`dep:<name>`, `<name>/<f>`).
    pub optional: bool,
    /// Its features this package enables.
    pub features: Vec<String>,
    /// `default_features: false` turns it off.
    pub default_features: bool,
    /// `package: "real"`: the package's own name; `name` is what code
    /// calls it.
    pub package: Option<String>,
    /// `target: "cfg(..)"` or a target name: only builds for a target it
    /// matches use it (`crate::platform`).
    pub target: Option<String>,
    /// `Dep::path(.., version: "^0.1")`: the requirement a published
    /// package carries in place of the path.
    pub version: Option<String>,
    /// `plugin: true` / `bundle: true`.
    pub load: Load,
}

/// How a dependency's code gets into the build.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Load {
    /// Linked in (a plugin: taken from the program that loads it).
    #[default]
    Linked,
    /// `plugin: true`: built as a plugin beside the program, which loads
    /// it with `std::plugin`.
    Plugin,
    /// `bundle: true`, a plugin's dependency: linked into the plugin.
    Bundled,
}

impl Dep {
    pub const fn new(name: String, source: Source) -> Self {
        Self {
            name,
            source,
            optional: false,
            features: Vec::new(),
            default_features: true,
            package: None,
            target: None,
            version: None,
            load: Load::Linked,
        }
    }

    /// The package's own name: `package`, else the dependency's name.
    pub fn package_name(&self) -> &str {
        self.package.as_deref().unwrap_or(&self.name)
    }

    /// The `Dep::..` value; feature arguments only where not the default.
    pub fn to_value(&self) -> Value {
        let mut v = self.source.to_value();
        if let Kind::Variant { named, .. } = &mut v.kind {
            if let Some(r) = &self.version {
                named.push(Field::new("version", Value::str(r)));
            }
            if self.optional {
                named.push(Field::new("optional", Value::new(Kind::Bool(true))));
            }
            if !self.features.is_empty() {
                named.push(Field::new("features", strings(&self.features)));
            }
            if !self.default_features {
                named.push(Field::new(
                    "default_features",
                    Value::new(Kind::Bool(false)),
                ));
            }
            if let Some(p) = &self.package {
                named.push(Field::new("package", Value::str(p)));
            }
            if let Some(t) = &self.target {
                named.push(Field::new("target", Value::str(t)));
            }
            let flag = match self.load {
                Load::Linked => None,
                Load::Plugin => Some("plugin"),
                Load::Bundled => Some("bundle"),
            };
            if let Some(f) = flag {
                named.push(Field::new(f, Value::new(Kind::Bool(true))));
            }
        }
        v
    }

    /// `to_value` on one line (it is spliced into `nabi.nabion`).
    pub fn to_nabi(&self) -> String {
        nabic_nabion::to_line(&self.to_value())
    }
}

pub fn strings(items: &[String]) -> Value {
    Value::new(Kind::List(items.iter().map(Value::str).collect()))
}

/// `features`: name -> what it enables, in file order.
pub type Features = Vec<(String, Vec<String>)>;

/// One entry of a feature's list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Enables<'a> {
    /// Another feature of the package.
    Feature(&'a str),
    /// `dep:<name>`: the optional dependency.
    Dep(&'a str),
    /// `<dep>/<feature>` (enabling the dependency too), or with `weak`
    /// `<dep>?/<feature>` (only when something else enables it).
    DepFeature {
        dep: &'a str,
        feature: &'a str,
        weak: bool,
    },
}

impl<'a> Enables<'a> {
    pub fn parse(s: &'a str) -> Self {
        if let Some(d) = s.strip_prefix("dep:") {
            return Self::Dep(d);
        }
        match s.split_once('/') {
            Some((dep, feature)) => match dep.strip_suffix('?') {
                Some(dep) => Self::DepFeature {
                    dep,
                    feature,
                    weak: true,
                },
                None => Self::DepFeature {
                    dep,
                    feature,
                    weak: false,
                },
            },
            None => Self::Feature(s),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Manifest {
    pub name: Option<String>,
    pub version: Option<String>,
    /// The oldest toolchain the package builds with, `x.y.z`; later ones of
    /// its series build it too (`toolchain::builds_with`).
    pub nabi_version: Option<String>,
    /// The toolchain this project builds with (a pin).
    pub toolchain: Option<String>,
    // The descriptive fields: `nabi publish` sends them to the registry
    // API beside the index line (`registry::meta`).
    pub description: Option<String>,
    /// An SPDX license expression.
    pub license: Option<String>,
    /// A license file inside the package, for a license SPDX does not name.
    pub license_file: Option<String>,
    pub repository: Option<String>,
    /// `"Name <email>"` each.
    pub authors: Vec<String>,
    pub homepage: Option<String>,
    pub documentation: Option<String>,
    /// As written: relative to the manifest's directory (`readme_path`).
    pub readme: Option<String>,
    /// At most `MAX_KEYWORDS`, none empty.
    pub keywords: Vec<String>,
    /// At most `MAX_KEYWORDS`, none empty.
    pub categories: Vec<String>,
    /// Globs of the files a published package holds (empty: all) and of
    /// those it leaves out (`registry::packaged`).
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    /// The native library the package links; one package per name in a
    /// build.
    pub links: Option<String>,
    /// The executable `nabi run` runs.
    pub default_run: Option<String>,
    /// The oldest macOS the program runs on (`"13.0"`): Objective-C APIs
    /// newer than it need `objc::available`. `MACOSX_DEPLOYMENT_TARGET`
    /// beats it.
    pub deployment_target: Option<String>,
    /// The oldest iOS (`deployment_target: { ios: "16.0" }`).
    /// `IPHONEOS_DEPLOYMENT_TARGET` beats it.
    pub ios_deployment_target: Option<String>,
    /// Free-form data for other tools.
    pub metadata: Option<Value>,
    /// Fields this nabi does not know, as written (sent on with the
    /// metadata, so a newer registry gets them).
    pub extra: Vec<(String, Value)>,
    /// `publish: false`: `nabi publish` refuses the package (`None`: true).
    pub publish: Option<bool>,
    pub deps: Vec<Dep>,
    pub dev_deps: Vec<Dep>,
    /// `lints`: `"name": "allow" | "warn" | "deny"`, in file order
    /// (lint names are checked by `nabi lint`).
    pub lints: Vec<(String, Level)>,
    /// `lints_from`: a directory (relative to this manifest's) whose
    /// manifest's lints come before `lints` (`inherit_lints`).
    pub lints_from: Option<String>,
    /// `native`: prebuilt C libraries (a namespace of their own).
    pub native: Vec<NativeDep>,
    pub features: Features,
    /// `profile`: the profiles whose `overflow_checks` is false.
    pub no_overflow_checks: Vec<String>,
    /// `lib: { kind: .. }`: what the library builds.
    pub lib: LibKind,
    /// `lib: { kind: "staticlib", xcframework: true }`: also into an
    /// `XCFramework` (`--xcframework`).
    pub xcframework: bool,
    /// `c`: C sources built into the package.
    pub c: Option<CSources>,
    /// Problems that do not stop a build: a name no file can carry on
    /// some OS, a `version` that is not semver (`load` prints them).
    pub warnings: Vec<String>,
}

/// `lib: { kind: .. }`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LibKind {
    /// `"nabi"`: a library Nabi packages use (`lib<name>.a` + NABIHIR).
    #[default]
    Nabi,
    /// `"cdylib"`: also a shared library C loads, with its header.
    Cdylib,
    /// `"staticlib"`: also one static library C links alone, with its
    /// header and module map (an Xcode app's Nabi code).
    Staticlib,
    /// `"plugin"`: a shared library a Nabi program loads with
    /// `std::plugin`.
    Plugin,
}

/// `c: { sources, include, define, std, flags }`: C files compiled by
/// the built-in C compiler and linked into the package. Paths are
/// relative to the manifest's directory.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CSources {
    pub sources: Vec<String>,
    pub include: Vec<String>,
    pub define: Vec<(String, String)>,
    /// `-std=`: `c89` .. `c23`, `gnu17`.
    pub std: Option<String>,
    pub flags: Vec<String>,
}

/// `Dep::system(..)`: a library installed on the machine, found at build
/// time (`crate::system`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SystemLib {
    /// The pkg-config module (`.pc` name); the dependency's name when
    /// `None`.
    pub pkg_config: Option<String>,
    /// The Homebrew formula, the vcpkg port, the apt package: where to
    /// look, and what the "not found" help installs.
    pub brew: Option<String>,
    pub vcpkg: Option<String>,
    pub apt: Option<String>,
    /// The libraries to link when no `.pc` file says (default: the
    /// dependency's name).
    pub libs: Vec<String>,
    /// `static: true`: link the static archives (`pkg-config --static`).
    pub statik: bool,
}

/// Where a native package comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativeSource {
    /// `Dep::native("req"[, registry: "url"])` or `"req"`: the index's
    /// `native/` files.
    Registry {
        req: String,
        registry: Option<String>,
    },
    /// `Dep::native(url: "..", sha256: "..")`: a `Native { .. }` file (a
    /// url, or a path relative to the file naming it) with that SHA-256.
    Direct { url: String, sha256: String },
    /// `Dep::system(..)`: installed on the machine.
    System(SystemLib),
    /// `Dep::framework()`: an Apple framework of the SDK (ignored on
    /// other targets).
    Framework,
}

impl NativeSource {
    pub fn to_value(&self) -> Value {
        match self {
            Self::Registry { req, registry } => Value::call(
                "Dep::native",
                vec![Value::str(req)],
                registry
                    .iter()
                    .map(|r| ("registry", Value::str(r)))
                    .collect(),
            ),
            Self::Direct { url, sha256 } => Value::call(
                "Dep::native",
                Vec::new(),
                vec![("url", Value::str(url)), ("sha256", Value::str(sha256))],
            ),
            Self::System(s) => {
                let mut f = Vec::new();
                for (k, v) in [
                    ("pkg_config", &s.pkg_config),
                    ("brew", &s.brew),
                    ("vcpkg", &s.vcpkg),
                    ("apt", &s.apt),
                ] {
                    if let Some(v) = v {
                        f.push((k, Value::str(v)));
                    }
                }
                if !s.libs.is_empty() {
                    f.push(("libs", strings(&s.libs)));
                }
                if s.statik {
                    f.push(("static", Value::new(Kind::Bool(true))));
                }
                Value::call("Dep::system", Vec::new(), f)
            }
            Self::Framework => Value::call("Dep::framework", Vec::new(), Vec::new()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeDep {
    pub name: String,
    pub source: NativeSource,
}

/// `dir/nabi.nabion`, if present. A case variant (`Nabi.nabion`) only warns,
/// whatever the file system.
pub fn find(dir: &Path) -> Option<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return None;
    };
    let mut found = None;
    let mut names: Vec<(String, PathBuf)> = rd
        .flatten()
        .filter_map(|e| Some((e.file_name().into_string().ok()?, e.path())))
        .collect();
    names.sort();
    for (name, path) in names {
        if name == MANIFEST {
            found = Some(path);
        } else if name.eq_ignore_ascii_case(MANIFEST) {
            eprintln!(
                "nabi: {} ignoring `{}`: the manifest is named `{MANIFEST}` (lowercase)",
                term::label(Stream::Stderr, Role::Warning),
                path.display()
            );
        }
    }
    found
}

/// `parse` the file at `path`; its warnings go to stderr, once each.
pub fn load(path: &Path) -> Result<Manifest, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let m = parse(&text, &path.display().to_string())?;
    for w in &m.warnings {
        crate::warn_once(w);
    }
    Ok(m)
}

/// `toolchain` of the manifest text, read alone: fields this version does
/// not know (a newer toolchain's) do not drop the pin.
pub fn toolchain_pin(text: &str) -> Option<String> {
    let doc = Doc::parse(text, MANIFEST).ok()?;
    let pkg = doc.root().record("Package").ok()?;
    pkg.opt("toolchain")?.string().ok()
}

impl Manifest {
    /// The readme `nabi publish` takes: `readme` (a file inside the
    /// package), else `README.md` when there is one.
    pub fn readme_path(&self, dir: &Path) -> Result<Option<PathBuf>, String> {
        let Some(r) = &self.readme else {
            return Ok(Some(dir.join("README.md")).filter(|p| p.is_file()));
        };
        // Lexical: `a/../b` stays inside, `../x` and `/x` do not.
        let mut depth = 0usize;
        let inside = Path::new(r).components().all(|c| match c {
            std::path::Component::Normal(_) => {
                depth += 1;
                true
            }
            std::path::Component::CurDir => true,
            std::path::Component::ParentDir => depth.checked_sub(1).map(|d| depth = d).is_some(),
            std::path::Component::RootDir | std::path::Component::Prefix(_) => false,
        });
        if !inside || r.is_empty() {
            return Err(format!(
                "`readme: {r:?}` in {MANIFEST} is outside the package; write a path inside it, \
                 relative to {MANIFEST} (e.g. `readme: \"README.md\"`)"
            ));
        }
        let p = dir.join(r);
        if p.is_file() {
            Ok(Some(p))
        } else {
            Err(format!(
                "`readme: {r:?}` in {MANIFEST}: {} is not a file; write the path relative to \
                 {MANIFEST}, or remove `readme` to use README.md",
                p.display()
            ))
        }
    }

    /// `Err` with how to change it when `publish: false`.
    pub fn check_publish(&self) -> Result<(), String> {
        if self.publish == Some(false) {
            let name = self.name.as_deref().unwrap_or("this package");
            return Err(format!(
                "`{name}` has `publish: false` in {MANIFEST}, so it is not published; remove \
                 that line (or write `publish: true`) to publish it"
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const APP: &str = "// the app\nPackage {\n    name: \"app\",\n    toolchain: \"0.3\",\n    \
                       deps: {\n        \"a\": Dep::path(\"../a\"), // local\n        \"b\": \
                       Dep::git(\"u\", tag: \"v1\"),\n    },\n    dev_deps: { \"c\": \
                       Dep::git(\"u2\") },\n}\n";

    #[test]
    fn package_metadata() {
        let m = parse(
            "Package { name: \"app\", authors: [\"Ann <a@x.org>\"], homepage: \"https://x.org\", \
             documentation: \"https://x.org/doc\", readme: \"doc/README.md\", keywords: [\"a\", \
             \"b\", \"c\", \"d\", \"e\"], publish: false }",
            "f",
        )
        .unwrap();
        assert_eq!(m.authors, ["Ann <a@x.org>"]);
        assert_eq!(m.homepage.as_deref(), Some("https://x.org"));
        assert_eq!(m.documentation.as_deref(), Some("https://x.org/doc"));
        assert_eq!(m.readme.as_deref(), Some("doc/README.md"));
        assert_eq!(m.keywords.len(), MAX_KEYWORDS);
        assert_eq!(m.publish, Some(false));
        let e = m.check_publish().unwrap_err();
        assert!(e.contains("`app` has `publish: false`"), "{e}");
        assert!(e.contains("write `publish: true`"), "{e}");
        let d = parse("Package {}", "f").unwrap();
        assert!(d.authors.is_empty() && d.keywords.is_empty() && d.publish.is_none());
        d.check_publish().unwrap();
        for (src, want) in [
            (
                "Package { keywords: [\"a\", \"b\", \"c\", \"d\", \"e\", \"f\"] }",
                "`keywords` has 6 entries; keep at most 5",
            ),
            (
                "Package { keywords: [\"a\", \"\"] }",
                "an entry of `keywords` is empty",
            ),
            (
                "Package { authors: [\" \"] }",
                "an entry of `authors` is empty",
            ),
            ("Package { keywords: \"a\" }", "must be a list"),
            ("Package { publish: \"no\" }", "must be a bool"),
        ] {
            let e = parse(src, "f").unwrap_err();
            assert!(e.contains(want), "{src}: {e}");
        }
        // readme: as written, else README.md when present.
        let dir = nabic_core::tmp::test_dir("manifest-readme");
        assert_eq!(d.readme_path(&dir).unwrap(), None);
        std::fs::write(dir.join("README.md"), "x").unwrap();
        assert_eq!(d.readme_path(&dir).unwrap(), Some(dir.join("README.md")));
        let e = m.readme_path(&dir).unwrap_err();
        assert!(e.contains("`readme: \"doc/README.md\"`"), "{e}");
        assert!(e.contains("remove `readme` to use README.md"), "{e}");
        let at = |r: &str| {
            Manifest {
                readme: Some(r.into()),
                ..Manifest::default()
            }
            .readme_path(&dir)
        };
        std::fs::create_dir(dir.join("doc")).unwrap();
        assert_eq!(
            at("doc/../README.md").unwrap(),
            Some(dir.join("doc/../README.md"))
        );
        assert_eq!(at("./README.md").unwrap(), Some(dir.join("./README.md")));
        let abs = dir.join("README.md").display().to_string();
        for r in ["../README.md", "doc/../../README.md", abs.as_str(), ""] {
            let e = at(r).unwrap_err();
            assert!(
                e.contains("is outside the package; write a path inside it"),
                "{r}: {e}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn manifest_new_fields() {
        let m = parse(
            "Package { name: \"app\", license_file: \"LICENSE\", categories: [\"encoding\"], \
             include: [\"src/**\"], exclude: [\"bench\"], links: \"z\", default_run: \"app\", \
             metadata: { docs: { all: true } }, deps: { \"j\": Dep::version(\"^1\", package: \
             \"json5\", target: \"cfg(target_os = \\\"linux\\\")\") } }",
            "f",
        )
        .unwrap();
        assert_eq!(m.license_file.as_deref(), Some("LICENSE"));
        assert_eq!(m.categories, ["encoding"]);
        assert_eq!(
            (m.include.as_slice(), m.exclude.as_slice()),
            (&["src/**".to_owned()][..], &["bench".to_owned()][..])
        );
        assert_eq!(
            (m.links.as_deref(), m.default_run.as_deref()),
            (Some("z"), Some("app"))
        );
        assert!(m.metadata.is_some() && m.warnings.is_empty());
        let d = &m.deps[0];
        assert_eq!((d.name.as_str(), d.package_name()), ("j", "json5"));
        assert_eq!(d.target.as_deref(), Some("cfg(target_os = \"linux\")"));
        assert!(
            d.to_nabi().contains("package: \"json5\", target: "),
            "{}",
            d.to_nabi()
        );
        for (src, want) in [
            ("Package { metadata: 1 }", "must be a table"),
            (
                "Package { categories: [\"a\", \"b\", \"c\", \"d\", \"e\", \"f\"] }",
                "keep at most 5",
            ),
            (
                "Package { deps: { \"a\": Dep::version(\"1\", target: \"cfg(unix)\") } }",
                "not a target key",
            ),
            (
                "Package { deps: { \"a\": Dep::version(\"1\", package: \"A B\") } }",
                "not a valid",
            ),
        ] {
            let e = parse(src, "f").unwrap_err();
            assert!(e.contains(want), "{src}: {e}");
        }
    }

    #[test]
    fn manifest_unknown_key_warns() {
        let m = parse(
            "Package {\n    name: \"a\",\n    edtion: \"x\",\n}",
            "nabi.nabion",
        )
        .unwrap();
        assert_eq!(m.warnings.len(), 1);
        let w = &m.warnings[0];
        assert!(
            w.starts_with("nabi.nabion:3:13: unknown field `edtion` is ignored"),
            "{w}"
        );
        assert_eq!(m.extra.len(), 1);
        assert_eq!(m.extra[0].0, "edtion");
    }

    #[test]
    fn lib_kinds_and_plugin_deps() {
        let m = parse(
            "Package { lib: { kind: \"plugin\" }, deps: { \"n\": Dep::path(\"../n\", bundle: true) } }",
            "f",
        )
        .unwrap();
        assert_eq!(m.lib, LibKind::Plugin);
        assert_eq!(m.deps[0].load, Load::Bundled);
        assert_eq!(m.deps[0].to_nabi(), "Dep::path(\"../n\", bundle: true)");
        let m = parse(
            "Package { deps: { \"p\": Dep::path(\"../p\", plugin: true) } }",
            "f",
        )
        .unwrap();
        assert_eq!(m.lib, LibKind::Nabi);
        assert_eq!(m.deps[0].load, Load::Plugin);
        let m = parse(
            "Package { deployment_target: { macos: \"13.0\", ios: \"16.0\" } }",
            "f",
        )
        .unwrap();
        assert_eq!(
            (
                m.deployment_target.as_deref(),
                m.ios_deployment_target.as_deref()
            ),
            (Some("13.0"), Some("16.0"))
        );
        let m = parse(
            "Package { lib: { kind: \"staticlib\", xcframework: true } }",
            "f",
        )
        .unwrap();
        assert!(m.lib == LibKind::Staticlib && m.xcframework);
        let e = parse(
            "Package { lib: { kind: \"cdylib\", xcframework: true } }",
            "f",
        )
        .unwrap_err();
        assert!(
            e.contains("`xcframework: true` packs a static library"),
            "{e}"
        );
        let m = parse("Package { deployment_target: \"12.0\" }", "f").unwrap();
        assert_eq!(
            (m.deployment_target.as_deref(), m.ios_deployment_target),
            (Some("12.0"), None)
        );
        for (src, want) in [
            (
                "Package { deployment_target: { ios: \"16.x\" } }",
                "`deployment_target`: \"16.x\" is not a version; write the iOS version as numbers",
            ),
            (
                "Package { deployment_target: { tvos: \"16.0\" } }",
                "unknown field `tvos`",
            ),
            (
                "Package { lib: { kind: \"dylib\" }, deps: {} }",
                "there is no library kind `dylib`; write \"nabi\" (the default), \"cdylib\"",
            ),
            (
                "Package { deps: { \"n\": Dep::path(\"../n\", bundle: true) } }",
                "`bundle: true` on `n` belongs in a plugin's dependencies: give this package `lib: { kind: \"plugin\" }`",
            ),
            (
                "Package { lib: { kind: \"plugin\" }, deps: { \"n\": Dep::path(\"../n\", bundle: true, plugin: true) } }",
                "either `plugin: true` (loaded at run time) or `bundle: true`",
            ),
        ] {
            let e = parse(src, "f").unwrap_err();
            assert!(e.contains(want), "{src}: {e}");
        }
    }

    #[test]
    fn toolchain_pin_skips_unknown_fields() {
        let text = "Package { toolchain: \"0.3\", from_a_newer_nabi: [1], deps: {} }";
        assert_eq!(parse(text, "f").unwrap().warnings.len(), 1);
        assert_eq!(toolchain_pin(text).as_deref(), Some("0.3"));
        assert_eq!(toolchain_pin("Package { deps: {} }"), None);
        assert_eq!(toolchain_pin("not nabion {"), None);
    }

    #[test]
    fn manifest_forms_and_errors() {
        let m = parse(APP, "nabi.nabion").unwrap();
        assert_eq!(m.toolchain.as_deref(), Some("0.3"));
        assert_eq!(m.deps[0].source, Source::Path("../a".into()));
        assert_eq!(
            m.deps[1].source,
            Source::Git {
                url: "u".into(),
                reference: GitRef::Tag("v1".into())
            }
        );
        assert_eq!(m.dev_deps[0].name, "c");
        let m = parse(
            "Package { lints: { \"unused\": \"allow\", \"all\": \"deny\" }, deps: {} }",
            "nabi.nabion",
        )
        .unwrap();
        assert_eq!(
            m.lints,
            [
                ("unused".to_owned(), Level::Allow),
                ("all".to_owned(), Level::Deny)
            ]
        );
        let m = parse(
            "Package { profile: { \"release\": { overflow_checks: false }, \
             \"debug\": Profile { overflow_checks: true } } }",
            "nabi.nabion",
        )
        .unwrap();
        assert_eq!(m.no_overflow_checks, ["release"]);
        let e = parse(
            "{ profile: { \"dev\": { overflow_checks: false } } }",
            "nabi.nabion",
        )
        .unwrap_err();
        assert!(e.contains("no profile `dev`"), "{e}");
        let e = parse("{ profile: { \"release\": { lto: true } } }", "nabi.nabion").unwrap_err();
        assert!(e.contains("unknown field `lto`"), "{e}");
        assert_eq!(
            parse("Package { name: \"a\\u{62}\" }", "f")
                .unwrap()
                .name
                .as_deref(),
            Some("ab")
        );
        // The root name is optional; `none` is a missing field.
        let m = parse("{ name: \"x\", toolchain: none }", "f").unwrap();
        assert_eq!(m.name.as_deref(), Some("x"));
        assert!(m.toolchain.is_none());
        assert!(parse("{}", "f").unwrap().deps.is_empty());
        for (src, want) in [
            (
                "Package { deps: { \"a\": \"1.0.x.y\" } }",
                "is not a version requirement",
            ),
            (
                "Package { deps: { \"a\": Dep::version(\"1\", tag: \"x\") } }",
                "unknown argument `tag`",
            ),
            (
                "Package { deps: { \"a\": Dep::version() } }",
                "takes one string, the version requirement",
            ),
            (
                "Package { deps: { \"a\": Dep::registry(\"u\") } }",
                "unknown dependency kind `Dep::registry`",
            ),
            (
                "Package { deps: { \"a\": Dep::native(\"z\") } }",
                "goes in the `native` field",
            ),
            ("Package { native: 1 }", "`native` must be a dict"),
            (
                "Package { native: { \"z\": Dep::path(\"x\") } }",
                "expected `Dep::native(..)`",
            ),
            (
                "Package { native: { \"z\": Dep::native(url: \"u\", sha256: \"ab\") } }",
                "64 hex digits",
            ),
            (
                "Package { native: { \"z\": Dep::native(url: \"u\") } }",
                "missing argument `sha256`",
            ),
            (
                "Package { native: { \"z\": Dep::native(\"1\", url: \"u\") } }",
                "unknown argument `url`",
            ),
            (
                "Package { deps: { \"My Dep\": Dep::path(\"x\") } }",
                "try `my-dep`",
            ),
            (
                "Package { deps: { \"a-b\": Dep::path(\"x\"), \"a_b\": Dep::path(\"y\") } }",
                "`a_b` and `a-b` are the same package",
            ),
            ("Package { name: \"1x\" }", "starting with a letter"),
            (
                "Package { deps: { \"a\": Dep::git(\"u\", tag: \"t\", rev: \"r\") } }",
                "at most one",
            ),
            (
                "Package { deps: { \"a\": Dep::git(\"u\", sha: \"t\") } }",
                "unknown argument `sha`",
            ),
            (
                "Package { deps: { \"a\": Dep::path(\"p\", tag: \"t\") } }",
                "unknown argument `tag`",
            ),
            (
                "Package { deps: { \"a\": Dep::path() } }",
                "takes one string, the path",
            ),
            (
                "Package { deps: { \"a\": Dep::svn(\"u\") } }",
                "unknown dependency kind `Dep::svn`",
            ),
            ("Package { deps: { \"a\": 1 } }", "found an integer"),
            (
                "Package { deps: { \"a\": Dep::path(\"x\"), \"a\": Dep::path(\"y\") } }",
                "given twice",
            ),
            ("Package { deps: [] }", "`deps` must be a dict"),
            ("Package { name: \"a\", name: \"b\" }", "given twice"),
            ("Package { name: 1 + 2 }", "operator `+` is not allowed"),
            ("Package { name: x }", "bare identifier `x`"),
            (
                "Package { name: 1 }",
                "`name` must be a string, found an integer",
            ),
            (
                "Package { lints: { \"unused\": \"off\" } }",
                "takes \"allow\"",
            ),
            (
                "Manifest { }",
                "expected `Package { .. }`, found a struct `Manifest",
            ),
            ("Package { } Package { }", "holds one value"),
            ("", "expected a value"),
            ("Package { name: \"x }", "unterminated"),
            (
                "Package { features: { \"a\": [\"b\"] } }",
                "`b` is not a feature of this package",
            ),
            (
                "Package { features: { \"a\": [\"b\"], \"b\": [\"c\"], \"c\": [\"b\"] } }",
                "feature cycle: b -> c -> b",
            ),
            (
                "Package { features: { \"A\": [] } }",
                "not a valid feature name",
            ),
            (
                "Package { deps: { \"x\": Dep::path(\"p\", optional: true) } }",
                "optional dependency `x` is not enabled by any feature (add \"dep:x\" to one)",
            ),
            (
                "Package { deps: { \"x\": Dep::path(\"p\") }, features: { \"a\": [\"dep:x\"] } }",
                "dependency `x` is not `optional: true`",
            ),
            (
                "Package { deps: { \"x\": Dep::path(\"p\") }, features: { \"a\": [\"x?/f\"] } }",
                "`?` is for an optional dependency",
            ),
            (
                "Package { features: { \"a\": [\"y/f\"] } }",
                "there is no dependency `y`",
            ),
            (
                "Package { dev_deps: { \"x\": Dep::path(\"p\", optional: true) } }",
                "a dev dependency cannot be optional",
            ),
            (
                "Package { deps: { \"x\": Dep::git(\"u\", features: [\"B\"]) } }",
                "`B` is not a valid feature name",
            ),
            (
                "Package { deps: { \"x\": Dep::version(\"1\", optional: 1) } }",
                "found an integer",
            ),
        ] {
            let e = parse(src, "nabi.nabion").unwrap_err();
            assert!(e.contains(want), "{src:?}: {e}");
            assert!(e.contains("--> nabi.nabion:1:"), "{e}");
        }
        let e = parse(APP.replace("tag", "tga").as_str(), "nabi.nabion").unwrap_err();
        assert!(
            e.contains("unknown argument `tga`") && e.contains("--> nabi.nabion:7:28\n"),
            "{e}"
        );
    }

    #[test]
    fn edits_keep_the_rest() {
        let src = Source::Path("x".into()).to_nabi();
        assert_eq!(src, "Dep::path(\"x\")");
        let t = set_dep(APP, "f", "deps", "a", &src).unwrap();
        assert!(t.contains("\"a\": Dep::path(\"x\"), // local\n"), "{t}");
        let t = set_dep("Package {\n    name: \"n\"\n}\n", "f", "deps", "d", &src).unwrap();
        assert_eq!(parse(&t, "f").unwrap().deps[0].name, "d");
        let t = remove_dep(APP, "f", "dev_deps", "c").unwrap().unwrap();
        assert!(parse(&t, "f").unwrap().dev_deps.is_empty());
        assert!(remove_dep(APP, "f", "deps", "zz").unwrap().is_none());
        // An edit that makes a bad manifest is refused.
        assert!(set_dep(APP, "f", "deps", "My.x", &src).is_err());
        // `-` is a package name's, `_` its code name's.
        assert_eq!(code_name("nabi-syntax"), "nabi_syntax");
        assert!(same("nabi-syntax", "Nabi_Syntax") && !same("nabi-syntax", "nabisyntax"));
        let git = Source::Git {
            url: "u".into(),
            reference: GitRef::Branch("b".into()),
        };
        assert_eq!(git.to_nabi(), "Dep::git(\"u\", branch: \"b\")");
        let m = parse(
            "{ deps: { \"a\": \"^1.2\", \"b\": Dep::version(\"~0.3\", registry: \"file:///i\") } }",
            "f",
        )
        .unwrap();
        let back: Vec<String> = m.deps.iter().map(|d| d.source.to_nabi()).collect();
        assert_eq!(
            back,
            [
                "Dep::version(\"^1.2\")",
                "Dep::version(\"~0.3\", registry: \"file:///i\")"
            ]
        );
        // Feature arguments: on every kind, written when not the default.
        let m = parse(
            "{ deps: { \"t\": Dep::git(\"u\", tag: \"v\", optional: true, features: [\"a\"], \
             default_features: false), \"j\": Dep::path(\"j\", default_features: true) }, \
             features: { \"default\": [\"t?/b\", \"j/c\"], \"tls\": [\"dep:t\"] } }",
            "f",
        )
        .unwrap();
        let back: Vec<String> = m.deps.iter().map(Dep::to_nabi).collect();
        assert_eq!(
            back,
            [
                "Dep::git(\"u\", tag: \"v\", optional: true, features: [\"a\"], default_features: \
                 false)",
                "Dep::path(\"j\")"
            ]
        );
        assert_eq!(m.features[0].1, ["t?/b", "j/c"]);
    }

    #[test]
    fn script_blocks() {
        let text = |s: &str| script_block(s).map(|b| b.text);
        let src = "//! ```nabion\n//! Package { deps: {} }\n//! ```\nprintln(\"hi\");\n";
        assert_eq!(text(src).as_deref(), Some("Package { deps: {} }\n"));
        let shebang = format!("#!/usr/bin/env nabi\n{src}");
        assert_eq!(text(&shebang).as_deref(), Some("Package { deps: {} }\n"));
        assert_eq!(text("//! ```nabion\n//! x\nfn main() {}"), None);
        assert_eq!(text("println(1);\n//! ```nabion\n//! ```"), None);
        assert_eq!(text("//! ```nabi\n//! let x = 1;\n//! ```\n"), None);
        // Written after the shebang and the doc lines, read back, removed.
        let deps = "Package {\n    deps: {\n        \"a\": \"^1\",\n    },\n}\n";
        for src in [
            "println(1);\n",
            "#!/usr/bin/env nabi\nprintln(1);\n",
            "#!/usr/bin/env nabi\n//! Says one.\n//!\n//! More.\nprintln(1);\n",
            "//! Doc, no newline",
        ] {
            let with = set_script_block(src, Some(deps));
            assert_eq!(text(&with).as_deref(), Some(deps), "{with}");
            let again = set_script_block(&with, Some(deps));
            assert_eq!(again, with);
            let back = set_script_block(&with, None);
            let want = if src.ends_with('\n') {
                src.to_owned()
            } else {
                format!("{src}\n")
            };
            assert_eq!(back, want);
        }
        let with = set_script_block("#!/usr/bin/env nabi\n//! Says one.\nx\n", Some(deps));
        assert!(
            with.starts_with("#!/usr/bin/env nabi\n//! Says one.\n//! ```nabion\n//! Package {\n")
        );
    }
}
