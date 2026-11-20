//! Dependency resolution: from the manifest's `deps` (and `dev_deps`)
//! through every dependency's `nabi.nabion` to one package per name, each
//! after its dependencies.
//!
//! Git and registry packages come from the lock
//! unless `update` names them. Path and git packages are followed first;
//! then one version of each registry package is picked so that every
//! requirement on it holds (`solve`): the locked one while it does, else
//! the highest non-yanked one, backing up to older versions of the
//! requirers when that leaves a package with no version. Native packages
//! are resolved the same way afterwards, in their own namespace.
//!
//! `nabi.lock` pins the whole graph, optional deps included. Features then
//! pick what a build has: from the root's (`--features`, `default`) along
//! every enabled edge, one set per package. `dev_deps` count only for
//! tests; a dependency with `target` only for a build for that target.
//! A renamed dependency (`package`) is the same package under another
//! name: dependents may call one package differently, and each name is
//! one package in a build.

mod solve;

use crate::bundle::Bundle;
use crate::git::Cache;
use crate::lock::{Lock, Locked, LockedNative};
use crate::manifest::{
    self, Dep, Enables, Features, GitRef, Manifest, NativeDep, NativeSource, Source,
};
use crate::semver::{Req, Version};
use crate::{archive, native, registry};
use nabic_core::{HashMap, HashSet};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[derive(Clone, Default)]
#[allow(clippy::struct_excessive_bools)] // independent switches
pub struct Options {
    pub offline: bool,
    /// `--locked`: fail instead of changing `nabi.lock`.
    pub locked: bool,
    /// `nabi update [names]`: re-resolve these git and registry packages
    /// (empty: all).
    pub update: Option<Vec<String>>,
    pub quiet: bool,
    /// `--features`: the root's features, `<dep>/<feature>` and the
    /// other forms of a feature's list.
    pub features: Vec<String>,
    pub all_features: bool,
    pub no_default_features: bool,
    /// A test / bench build: with the root's `dev_deps`.
    pub dev: bool,
    /// The build's target, for dependencies with `target` (`None`: the
    /// host).
    pub target: Option<nabic_target::TargetSpec>,
}

/// What a command works on: a project (a directory with `nabi.nabion`), a
/// named environment, a script's `//! ```nabion` block, or nothing.
pub struct Context {
    pub manifest: Manifest,
    /// Path dependencies are relative to this directory.
    pub base: PathBuf,
    /// `None`: nothing to lock (no dependencies).
    pub lock_path: Option<PathBuf>,
    /// A script: its `deps` are those of its `//! ```nabion` blocks.
    pub script: bool,
    /// The workspace of a member (or of its root): resolved together, one
    /// lock (`lock_path`).
    pub workspace: Option<crate::workspace::Workspace>,
}

#[derive(Clone)]
pub struct Resolved {
    pub name: String,
    /// What code calls it: `name`, or the dependency's name when renamed.
    pub code: String,
    /// What other dependents call it (`package` renames).
    pub aliases: Vec<String>,
    /// The package's `src/lib.nabi`.
    pub root: PathBuf,
    /// Its enabled features, sorted.
    pub features: Vec<String>,
    /// `path+<dir>`, `git+<url>#<commit>` or `registry+<url>`.
    pub source: String,
    /// A registry package's version, else its nabi.nabion's `version`.
    pub version: Option<String>,
}

/// A package of the whole graph.
struct Node {
    name: String,
    /// What its dependents call it.
    code: String,
    /// The native library it links (`links`).
    links: Option<String>,
    root: PathBuf,
    deps: Vec<Dep>,
    features: Features,
    /// As in `Resolved`.
    source: String,
    version: Option<String>,
}

/// What a build uses: packages after their dependencies, native packages
/// before theirs (linker order), and the root's features.
pub struct Graph {
    pub packages: Vec<Resolved>,
    pub natives: Vec<native::Resolved>,
    /// `Dep::system` and `Dep::framework` libraries, one per name.
    pub system: Vec<NativeDep>,
    pub features: Vec<String>,
}

/// A native package's identity.
#[derive(PartialEq, Eq)]
enum NativeId {
    Index { url: String, vers: Version },
    File { sha256: String },
}

/// (dependency, base of its relative url, requirer, requirer's key; `""`
/// is the root).
type NativeReq = (NativeDep, Option<String>, String, String);

/// Where a package came from: two requirements of one name must agree.
#[derive(PartialEq, Eq)]
enum Id {
    Path(PathBuf),
    Git { url: String, commit: String },
    Registry { registry: String, vers: Version },
}

impl std::fmt::Display for Id {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Path(p) => write!(f, "path {}", p.display()),
            Self::Git { url, commit } => {
                write!(f, "git {url} at {}", &commit[..commit.len().min(12)])
            }
            Self::Registry { registry, vers } => write!(f, "version {vers} from {registry}"),
        }
    }
}

struct Resolver<'a> {
    home: PathBuf,
    /// The package being built (`Context::base`, resolved).
    root: PathBuf,
    cache: Cache,
    index: Cache,
    /// For `Dep::version(..)` without `registry`.
    registry: String,
    opts: &'a Options,
    old: Lock,
    new: Lock,
    fetched: Vec<String>,
    /// What each name resolved to, who required it, and how (a registry
    /// package's requirement).
    seen: HashMap<String, (Id, String, String)>,
    /// The package each code name (by key) is, and who named it first.
    names: HashMap<String, (String, String)>,
    /// What code calls each package (by key), first name first.
    codes: HashMap<String, Vec<String>>,
    /// What each dependent calls a package: (dependent, package key).
    named_by: HashMap<(String, String), String>,
    out: Vec<Node>,
    natives: Vec<NativeReq>,
    /// `Dep::system` and `Dep::framework` requirements: (dependency,
    /// user key); found at build time, never locked.
    system: Vec<(NativeDep, String)>,
    /// Registry requirements of the root and of path and git packages:
    /// (dependency by its package name, requirer).
    wanted: Vec<(Dep, String)>,
    /// Index entries read for the search, by (index, package key).
    entries: HashMap<(String, String), Vec<registry::Entry>>,
    /// The official packages this toolchain carries.
    bundle: Option<Bundle>,
}

pub fn resolve(home: &Path, ctx: &Context, opts: &Options) -> Result<Graph, String> {
    let old = match &ctx.lock_path {
        Some(p) => Lock::read(p)?.unwrap_or_default(),
        None => Lock::default(),
    };
    if opts.locked && ctx.lock_path.as_ref().is_some_and(|p| !p.is_file()) {
        let any_git = ctx
            .manifest
            .deps
            .iter()
            .chain(&ctx.manifest.dev_deps)
            .any(|d| !matches!(d.source, Source::Path(_)))
            || ctx
                .manifest
                .native
                .iter()
                .any(|d| matches!(d.source, NativeSource::Registry { .. }));
        if any_git {
            return Err("there is no nabi.lock, and --locked forbids writing one".to_owned());
        }
    }
    if let Some(ws) = &ctx.workspace {
        let lock = resolve_workspace(home, ws, ctx.lock_path.as_deref(), opts, old)?;
        // The member alone, from the shared lock, written nowhere.
        let member = Context {
            manifest: ctx.manifest.clone(),
            base: ctx.base.clone(),
            lock_path: None,
            script: false,
            workspace: None,
        };
        let o = Options {
            update: None,
            quiet: true,
            ..opts.clone()
        };
        return graph_of(home, &member, &o, lock);
    }
    graph_of(home, ctx, opts, old)
}

/// Every member of `ws` resolved together, the lock written to
/// `lock_path`; that lock.
fn resolve_workspace(
    home: &Path,
    ws: &crate::workspace::Workspace,
    lock_path: Option<&Path>,
    opts: &Options,
    old: Lock,
) -> Result<Lock, String> {
    let mut r = Resolver::new(home, &ws.root, opts, old)?;
    for m in &ws.members {
        let manifest = manifest::load(&m.dir.join(manifest::MANIFEST))?;
        let by = format!("member `{}`", m.name);
        let base = m.dir.display().to_string();
        r.natives.extend(
            manifest
                .native
                .iter()
                .map(|d| (d.clone(), Some(base.clone()), by.clone(), String::new())),
        );
        let all: Vec<Dep> = manifest
            .deps
            .iter()
            .chain(&manifest.dev_deps)
            .cloned()
            .collect();
        r.walk(&all, &m.dir, &by)?;
    }
    r.registry()?;
    r.natives()?;
    if let Some(path) = lock_path {
        write_lock(path, &r.new, opts)?;
    }
    Ok(r.new)
}

/// Write `new` to `path` when it changed (`--locked`: an error).
fn write_lock(path: &Path, new: &Lock, opts: &Options) -> Result<(), String> {
    let text = new.to_text();
    let old_text = std::fs::read_to_string(path).ok();
    let empty = *new == Lock::default();
    if old_text.as_deref() != Some(text.as_str()) && !(empty && old_text.is_none()) {
        if opts.locked {
            return Err(format!(
                "{} is out of date and --locked forbids changing it (run `nabi update`)",
                path.display()
            ));
        }
        std::fs::write(path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    }
    Ok(())
}

/// The graph of `ctx` with lock `old`.
fn graph_of(home: &Path, ctx: &Context, opts: &Options, old: Lock) -> Result<Graph, String> {
    let mut r = Resolver::new(home, &ctx.base, opts, old)?;
    let base = ctx.base.display().to_string();
    r.natives.extend(ctx.manifest.native.iter().map(|d| {
        let by = "the root package".to_owned();
        (d.clone(), Some(base.clone()), by, String::new())
    }));
    let all: Vec<Dep> = ctx
        .manifest
        .deps
        .iter()
        .chain(&ctx.manifest.dev_deps)
        .cloned()
        .collect();
    let by = if ctx.script {
        "the script's `//! nabion` blocks"
    } else {
        "the root package"
    };
    r.walk(&all, &ctx.base, by)?;
    r.registry()?;
    check_links(&r.out)?;
    let natives = r.natives()?;
    if let Some(path) = &ctx.lock_path {
        write_lock(path, &r.new, opts)?;
    }
    let nodes = order(
        r.out,
        |p| &p.name,
        |p| p.deps.iter().map(|d| d.package_name().to_owned()).collect(),
    );
    let mut root_deps = ctx.manifest.deps.clone();
    if opts.dev {
        root_deps.extend(ctx.manifest.dev_deps.iter().cloned());
    }
    let mut on = Features::default();
    let root_features = activate(&nodes, root_deps, &ctx.manifest.features, opts, &mut on)?;
    let packages: Vec<Resolved> = nodes
        .into_iter()
        .filter_map(|n| {
            let features = on
                .iter()
                .find(|(k, _)| *k == manifest::key(&n.name))?
                .1
                .clone();
            let aliases = r
                .codes
                .get(&manifest::key(&n.name))
                .map(|c| c[1..].to_vec())
                .unwrap_or_default();
            Some(Resolved {
                aliases,
                name: n.name,
                code: n.code,
                root: n.root,
                features,
                source: n.source,
                version: n.version,
            })
        })
        .collect();
    let mut natives: Vec<native::Resolved> = order(
        natives,
        |n| &n.name,
        |n| n.entry.deps.iter().map(|d| d.name.clone()).collect(),
    )
    .into_iter()
    .filter(|n| {
        n.users
            .iter()
            .any(|u| u.is_empty() || on.iter().any(|(k, _)| k == u))
    })
    .collect();
    natives.reverse();
    let mut system: Vec<NativeDep> = Vec::new();
    for (d, user) in std::mem::take(&mut r.system) {
        let used = user.is_empty() || on.iter().any(|(k, _)| *k == user);
        if used && !system.iter().any(|o| manifest::same(&o.name, &d.name)) {
            system.push(d);
        }
    }
    Ok(Graph {
        packages,
        natives,
        system,
        features: root_features,
    })
}

/// The features `opts` asks of the root (with `deps` and `features`):
/// the root's, and in `on` each package reached (by key) with its own.
fn activate(
    nodes: &[Node],
    deps: Vec<Dep>,
    features: &Features,
    opts: &Options,
    on: &mut Features,
) -> Result<Vec<String>, String> {
    let root = Node {
        name: String::new(),
        code: String::new(),
        links: None,
        root: PathBuf::new(),
        deps,
        features: features.clone(),
        source: String::new(),
        version: None,
    };
    let mut all: Vec<&Node> = nodes.iter().collect();
    all.push(&root);
    let mut a = Activation {
        index: all
            .iter()
            .enumerate()
            .map(|(i, n)| (manifest::key(&n.name), i))
            .collect(),
        nodes: all,
        target: opts
            .target
            .clone()
            .unwrap_or_else(|| nabic_target::TargetSpec::host().clone()),
        on: HashMap::default(),
        edges: HashSet::default(),
        weak: Vec::new(),
    };
    let r = nodes.len();
    a.enable(r)?;
    let cli = "--features";
    if !opts.no_default_features && features.iter().any(|(n, _)| n == "default") {
        a.apply(r, "default", cli)?;
    }
    if opts.all_features {
        for (f, _) in features {
            a.apply(r, f, cli)?;
        }
    }
    for f in &opts.features {
        a.apply(r, f, cli)?;
    }
    let mine = a.on.remove(&r).unwrap_or_default().into_iter().collect();
    for (i, n) in nodes.iter().enumerate() {
        if let Some(f) = a.on.remove(&i) {
            on.push((manifest::key(&n.name), f.into_iter().collect()));
        }
    }
    Ok(mine)
}

/// Feature resolution over `nodes` (the root last).
struct Activation<'a> {
    nodes: Vec<&'a Node>,
    /// The build's target: a dependency with another `target` stays off.
    target: nabic_target::TargetSpec,
    index: HashMap<String, usize>,
    /// The packages reached, and their features.
    on: HashMap<usize, BTreeSet<String>>,
    /// Dependencies enabled: (package, dependency key).
    edges: HashSet<(usize, String)>,
    /// `<dep>?/<feature>` waiting for its dependency: (package,
    /// dependency key, feature).
    weak: Vec<(usize, String, String)>,
}

impl Activation<'_> {
    fn who(&self, n: usize) -> String {
        match self.nodes[n].name.as_str() {
            "" => "the root package".to_owned(),
            name => format!("package `{name}`"),
        }
    }

    /// Package `n` is in the build, with its non-optional dependencies.
    fn enable(&mut self, n: usize) -> Result<(), String> {
        if self.on.contains_key(&n) {
            return Ok(());
        }
        self.on.insert(n, BTreeSet::new());
        let deps: Vec<String> = self.nodes[n]
            .deps
            .iter()
            .filter(|d| !d.optional)
            .map(|d| d.name.clone())
            .collect();
        for d in deps {
            self.edge(n, &d)?;
        }
        Ok(())
    }

    /// Enable `n`'s dependency `dep` with the features `n` asks; the
    /// package, `None` when the build's target does not use it.
    fn edge(&mut self, n: usize, dep: &str) -> Result<Option<usize>, String> {
        let Some(d) = self.nodes[n]
            .deps
            .iter()
            .find(|d| manifest::same(&d.name, dep))
            .cloned()
        else {
            return Err(format!("{} has no dependency `{dep}`", self.who(n)));
        };
        if !crate::platform::matches(d.target.as_deref(), &self.target) {
            return Ok(None);
        }
        let key = manifest::key(dep);
        let Some(&t) = self.index.get(&manifest::key(d.package_name())) else {
            return Err(format!("dependency `{dep}` was not resolved"));
        };
        if !self.edges.insert((n, key.clone())) {
            return Ok(Some(t));
        }
        self.enable(t)?;
        let by = self.who(n);
        for f in &d.features {
            self.feature(t, f, &by)?;
        }
        if d.default_features && self.nodes[t].features.iter().any(|(f, _)| f == "default") {
            self.feature(t, "default", &by)?;
        }
        let waiting: Vec<String> = self
            .weak
            .iter()
            .filter(|(w, k, _)| *w == n && *k == key)
            .map(|(_, _, f)| f.clone())
            .collect();
        for f in waiting {
            self.feature(t, &f, &by)?;
        }
        Ok(Some(t))
    }

    /// Feature `f` of package `n` (in the build), required by `by`.
    fn feature(&mut self, n: usize, f: &str, by: &str) -> Result<(), String> {
        let Some((_, list)) = self.nodes[n].features.iter().find(|(x, _)| x == f) else {
            return Err(format!(
                "{} has no feature `{f}` (required by {by})",
                self.who(n)
            ));
        };
        let list = list.clone();
        if !self.on.entry(n).or_default().insert(f.to_owned()) {
            return Ok(());
        }
        let by = self.who(n);
        for e in &list {
            self.apply(n, e, &by)?;
        }
        Ok(())
    }

    /// Entry `e` of a feature list of package `n`.
    fn apply(&mut self, n: usize, e: &str, by: &str) -> Result<(), String> {
        match Enables::parse(e) {
            Enables::Feature(f) => self.feature(n, f, by),
            Enables::Dep(d) => self.edge(n, d).map(drop),
            Enables::DepFeature {
                dep,
                feature,
                weak: false,
            } => match self.edge(n, dep)? {
                Some(t) => self.feature(t, feature, by),
                None => Ok(()),
            },
            Enables::DepFeature { dep, feature, .. } => {
                let key = manifest::key(dep);
                let Some(d) = self.nodes[n]
                    .deps
                    .iter()
                    .find(|d| manifest::same(&d.name, dep))
                else {
                    return Err(format!("{} has no dependency `{dep}`", self.who(n)));
                };
                if self.edges.contains(&(n, key.clone())) {
                    let t = self.index[&manifest::key(d.package_name())];
                    return self.feature(t, feature, by);
                }
                self.weak.push((n, key, feature.to_owned()));
                Ok(())
            }
        }
    }
}

/// The entry's `features` must match the package's own `nabi.nabion` at its
/// commit (any order).
fn check_index_features(dir: &Path, name: &str, e: &registry::Entry) -> Result<(), String> {
    let own = match manifest::find(dir) {
        Some(p) => manifest::load(&p)?.features,
        None => Features::new(),
    };
    let set = |f: &Features| -> BTreeSet<(String, BTreeSet<String>)> {
        f.iter()
            .map(|(k, v)| (k.clone(), v.iter().cloned().collect()))
            .collect()
    };
    if set(&own) != set(&e.features) {
        return Err(format!(
            "package `{name}` {}: the index's features differ from its nabi.nabion's at commit {}",
            e.version, e.rev
        ));
    }
    Ok(())
}

/// `items` each after the ones its `deps` name.
fn order<T>(
    items: Vec<T>,
    name: impl Fn(&T) -> &String,
    deps: impl Fn(&T) -> Vec<String>,
) -> Vec<T> {
    // 0 new, 1 on the stack (a cycle is cut there), 2 placed.
    fn visit(i: usize, edges: &[Vec<usize>], state: &mut [u8], out: &mut Vec<usize>) {
        if state[i] != 0 {
            return;
        }
        state[i] = 1;
        for &j in &edges[i] {
            visit(j, edges, state, out);
        }
        state[i] = 2;
        out.push(i);
    }
    let edges: Vec<Vec<usize>> = items
        .iter()
        .map(|x| {
            deps(x)
                .iter()
                .filter_map(|n| items.iter().position(|y| manifest::same(name(y), n)))
                .collect()
        })
        .collect();
    let mut state = vec![0; items.len()];
    let mut idx = Vec::new();
    for i in 0..items.len() {
        visit(i, &edges, &mut state, &mut idx);
    }
    let mut slots: Vec<Option<T>> = items.into_iter().map(Some).collect();
    idx.into_iter().filter_map(|i| slots[i].take()).collect()
}

/// Directory of `dep`, fetched into the cache if missing (paths relative
/// to `base`), and its exact source: `=<version>`, a commit, or the path.
pub fn fetch_package(
    home: &Path,
    dep: &Dep,
    base: &Path,
    opts: &Options,
) -> Result<(PathBuf, Source), String> {
    let mut r = Resolver::new(home, base, opts, Lock::default())?;
    let (dir, id, _) = r.locate(dep, base)?;
    let exact = match (id, &dep.source) {
        (Id::Registry { vers, .. }, Source::Registry { registry, .. }) => Source::Registry {
            req: format!("={vers}"),
            registry: registry.clone(),
        },
        (Id::Git { commit, .. }, Source::Git { url, .. }) => Source::Git {
            url: url.clone(),
            reference: GitRef::Rev(commit),
        },
        _ => dep.source.clone(),
    };
    Ok((dir, exact))
}

impl<'a> Resolver<'a> {
    fn new(home: &Path, base: &Path, opts: &'a Options, old: Lock) -> Result<Self, String> {
        Ok(Resolver {
            home: home.to_path_buf(),
            root: nabic_core::real_path(base).unwrap_or_else(|_| base.to_path_buf()),
            cache: Cache::new(home),
            index: Cache::index(home, registry::sources(home)?),
            registry: registry::DEFAULT.to_owned(),
            opts,
            old,
            new: Lock::default(),
            fetched: Vec::new(),
            seen: HashMap::default(),
            names: HashMap::default(),
            codes: HashMap::default(),
            named_by: HashMap::default(),
            out: Vec::new(),
            natives: Vec::new(),
            system: Vec::new(),
            wanted: Vec::new(),
            entries: HashMap::default(),
            bundle: Bundle::here(),
        })
    }

    /// Every package `deps` reach, optional ones too.
    fn walk(&mut self, deps: &[Dep], base: &Path, by: &str) -> Result<(), String> {
        let mut queue: Vec<(Dep, PathBuf, String)> = deps
            .iter()
            .map(|d| (d.clone(), base.to_path_buf(), by.to_owned()))
            .collect();
        let mut i = 0;
        while i < queue.len() {
            let (dep, base, by) = queue[i].clone();
            i += 1;
            // From here on the dependency is named by its package.
            let code = dep.name.clone();
            let dep = Dep {
                name: dep.package_name().to_owned(),
                package: None,
                ..dep
            };
            self.name(&dep.name, &code, &by)?;
            // Registry packages are picked together, after the rest.
            if matches!(dep.source, Source::Registry { .. }) {
                self.wanted.push((dep, by));
                continue;
            }
            let (dir, id, index_deps) = self.locate(&dep, &base)?;
            if dir == self.root {
                return Err(format!(
                    "dependency `{}` (required by {by}) is the package being built; a package \
                     names its own items with `pkg::`",
                    dep.name
                ));
            }
            if let Some((prev, prev_by, _)) = self.seen.get(&manifest::key(&dep.name)) {
                if *prev == id {
                    continue;
                }
                return Err(format!(
                    "conflicting requirements for package `{}`:\n  {prev} (required by {prev_by})\n  \
                     {id} (required by {by})\none build has one version of a package",
                    dep.name
                ));
            }
            let root = library_of(&dep.name, &id, &dir)?;
            let me = format!("`{}`", dep.name);
            let mut version = None;
            let (next, natives, features, links) = match index_deps {
                // The index has no system libraries: the package's own
                // manifest names them.
                Some((next, mut natives, features, links)) => {
                    if let Some(m) = manifest::find(&dir).and_then(|m| manifest::load(&m).ok()) {
                        natives.extend(m.native.into_iter().filter(|n| {
                            matches!(n.source, NativeSource::System(_) | NativeSource::Framework)
                        }));
                    }
                    (next, natives, features, links)
                }
                None => match manifest::find(&dir) {
                    Some(m) => {
                        let m = manifest::load(&m)?;
                        if let (Some(req), Some(have)) = (&dep.version, &m.version) {
                            if !Req::parse(req)?.matches(&Version::parse(have)?) {
                                return Err(format!(
                                    "dependency `{}` ({id}) is version {have}, which its \
                                     `version: \"{req}\"` does not accept; change the \
                                     requirement to one {have} meets, e.g. `version: \"^{have}\"`",
                                    dep.name
                                ));
                            }
                        }
                        version = m.version;
                        (m.deps, m.native, m.features, m.links)
                    }
                    None => (Vec::new(), Vec::new(), Vec::new(), None),
                },
            };
            let source = match &id {
                Id::Path(p) => format!("path+{}", p.display()),
                Id::Git { url, commit } => format!("git+{url}#{commit}"),
                Id::Registry { registry, vers } => {
                    version = Some(vers.to_string());
                    format!("registry+{registry}")
                }
            };
            let base = dir.display().to_string();
            let user = manifest::key(&dep.name);
            self.natives.extend(
                natives
                    .into_iter()
                    .map(|n| (n, Some(base.clone()), me.clone(), user.clone())),
            );
            queue.extend(next.iter().map(|d| (d.clone(), dir.clone(), me.clone())));
            let req = match &dep.source {
                Source::Registry { req, .. } => req.clone(),
                _ => String::new(),
            };
            self.seen.insert(manifest::key(&dep.name), (id, by, req));
            self.out.push(Node {
                name: dep.name,
                code,
                links,
                root,
                deps: next,
                features,
                source,
                version,
            });
        }
        Ok(())
    }

    /// Package `name` is `code` in code for `by`. Dependents may call a
    /// package differently, but a name is one package in a build, and a
    /// dependent calls a package by one name.
    fn name(&mut self, name: &str, code: &str, by: &str) -> Result<(), String> {
        let key = manifest::key(name);
        match self.names.get(&manifest::key(code)) {
            Some((p, prev)) if *p != key => {
                return Err(format!(
                    "`{code}` is package `{p}` for {prev} and package `{name}` for {by}; one \
                     name is one package in a build: rename one of them (`\"<new name>\": \
                     Dep::..(.., package: \"{name}\")`)"
                ));
            }
            Some(_) => {}
            None => {
                self.names
                    .insert(manifest::key(code), (key.clone(), by.to_owned()));
            }
        }
        let mine = (by.to_owned(), key.clone());
        match self.named_by.get(&mine) {
            Some(c) if !manifest::same(c, code) => {
                return Err(format!(
                    "{by} depends on package `{name}` twice, as `{c}` and `{code}`; depend on \
                     it once"
                ));
            }
            Some(_) => {}
            None => {
                self.named_by.insert(mine, code.to_owned());
            }
        }
        let codes = self.codes.entry(key).or_default();
        if !codes.iter().any(|c| manifest::same(c, code)) {
            codes.push(code.to_owned());
        }
        Ok(())
    }

    fn registry_of(&self, source: &Source) -> String {
        match source {
            Source::Registry {
                registry: Some(r), ..
            } => r.clone(),
            _ => self.registry.clone(),
        }
    }

    fn updating(&self, name: &str) -> bool {
        self.opts
            .update
            .as_ref()
            .is_some_and(|names| names.is_empty() || names.iter().any(|n| manifest::same(n, name)))
    }

    /// `dep`'s directory and identity; for a registry package also its dependencies
    /// and native packages from the index.
    fn locate(
        &mut self,
        dep: &Dep,
        base: &Path,
    ) -> Result<(PathBuf, Id, Option<IndexDeps>), String> {
        let (url, reference) = match &dep.source {
            Source::Path(p) => {
                let dir = base.join(p);
                let dir = nabic_core::real_path(&dir).map_err(|e| {
                    if e.kind() == std::io::ErrorKind::NotFound {
                        format!(
                            "dependency `{}`: no directory `{p}` ({}); a `Dep::path` is \
                             relative to the directory of the `nabi.nabion` that names it",
                            dep.name,
                            dir.display()
                        )
                    } else {
                        format!("dependency `{}`: path {}: {e}", dep.name, dir.display())
                    }
                })?;
                return Ok((dir.clone(), Id::Path(dir), None));
            }
            Source::Git { url, reference } => (url, reference),
            Source::Registry { req, .. } => {
                let url = self.registry_of(&dep.source);
                return self.locate_registry(&dep.name, req, &url);
            }
        };
        let updating = self.updating(&dep.name);
        let pinned = self
            .old
            .find(&dep.name)
            .filter(|l| {
                !updating && l.version.is_none() && l.url == *url && l.reference == *reference
            })
            .cloned();
        let commit = if let Some(l) = &pinned {
            if !self.cache.has_commit(url, &l.commit) {
                self.fetch(url, &dep.name)?;
                if !self.cache.has_commit(url, &l.commit) {
                    return Err(format!(
                        "`{}`: locked commit {} is not in {url} (run `nabi update {}`)",
                        dep.name, l.commit, dep.name
                    ));
                }
            }
            l.commit.clone()
        } else {
            if self.opts.locked && !updating {
                return Err(format!(
                    "`{}` is not in nabi.lock (or its source changed), and --locked forbids \
                     changing it",
                    dep.name
                ));
            }
            if self.opts.offline {
                self.cache
                    .resolve(url, reference)
                    .map_err(|e| format!("`{}`: {e} (--offline: nothing is fetched)", dep.name))?
            } else {
                self.fetch(url, &dep.name)?;
                self.cache.resolve(url, reference)?
            }
        };
        let (dir, tree) = self.cache.checkout(url, &commit)?;
        if let Some(l) = &pinned {
            if l.tree != tree {
                return Err(format!(
                    "`{}`: commit {} has tree {tree}, but nabi.lock says {}",
                    dep.name, l.commit, l.tree
                ));
            }
        }
        if !self
            .new
            .packages
            .iter()
            .any(|p| manifest::same(&p.name, &dep.name))
        {
            self.new.packages.push(Locked {
                name: dep.name.clone(),
                version: None,
                url: url.clone(),
                reference: reference.clone(),
                commit: commit.clone(),
                tree,
            });
        }
        Ok((
            dir,
            Id::Git {
                url: url.clone(),
                commit,
            },
            None,
        ))
    }

    /// Pick a version of every registry package the graph reaches, so
    /// that each requirement holds, and add them to it.
    fn registry(&mut self) -> Result<(), String> {
        if self.wanted.is_empty() {
            return Ok(());
        }
        let mut roots = Vec::new();
        for (dep, by) in &self.wanted {
            let Source::Registry { req, .. } = &dep.source else {
                continue;
            };
            let url = self.registry_of(&dep.source);
            roots.push((
                Self::need(&dep.name, req, &url)?,
                solve::By::Root(by.clone()),
            ));
        }
        let picks = match self.search(roots.clone()) {
            Ok(p) => p,
            // A stale index may lack what fits: fetch what was read, then again.
            Err(_) if !self.opts.offline && self.unfetched() => {
                for url in self.read_urls() {
                    self.fetch_index(&url, "")?;
                }
                self.entries.clear();
                self.search(roots)?
            }
            Err(e) => return Err(e),
        };
        for p in picks {
            let key = manifest::key(&p.name);
            if let Some((prev, prev_by, _)) = self.seen.get(&key) {
                let (n, by) = &p.needs[0];
                return Err(format!(
                    "conflicting requirements for package `{}`:\n  {prev} (required by \
                     {prev_by})\n  `{}` from {} (required by {by})\none build has one version \
                     of a package",
                    p.name, n.text, p.url
                ));
            }
            let e = self.entries[&(p.url.clone(), key.clone())]
                .iter()
                .find(|e| e.version == p.version)
                .cloned()
                .ok_or_else(|| format!("package `{}` {}: not in the index", p.name, p.version))?;
            if !self.opts.quiet {
                if let Some(note) = self.newer_elsewhere(&p) {
                    nabic_core::term::note(&format!("{note}; using {}", p.version));
                }
            }
            let (dir, id, Some((next, natives, features, links))) =
                self.place_registry(&p.name, &e, &p.url)?
            else {
                continue;
            };
            let me = format!("`{}`", p.name);
            for d in &next {
                self.name(d.package_name(), &d.name, &me)?;
            }
            let root = library_of(&p.name, &id, &dir)?;
            let base = dir.display().to_string();
            self.natives.extend(
                natives
                    .into_iter()
                    .map(|n| (n, Some(base.clone()), me.clone(), key.clone())),
            );
            let code = self
                .codes
                .get(&key)
                .and_then(|c| c.first())
                .cloned()
                .unwrap_or_else(|| p.name.clone());
            let (n, by) = &p.needs[0];
            self.seen.insert(key, (id, by.to_string(), n.text.clone()));
            self.out.push(Node {
                name: e.name.clone(),
                code,
                links,
                root,
                deps: next,
                features,
                source: format!("registry+{}", p.url),
                version: Some(p.version.to_string()),
            });
        }
        Ok(())
    }

    /// The search over the index for `roots`, its failure explained.
    fn search(&mut self, roots: Vec<(solve::Need, solve::By)>) -> Result<Vec<solve::Pick>, String> {
        let locked = self.opts.locked;
        let mut fetch = |url: &str, name: &str| self.versions(url, name);
        let mut s = solve::Solver::new(&mut fetch);
        let c = match s.solve(roots) {
            Ok(p) => return Ok(p),
            Err(solve::Failure::Fatal(e)) => return Err(e),
            Err(solve::Failure::Conflict(c)) => c,
        };
        // One requirement no version meets is said alone.
        let mut lone = None;
        for (n, _) in &c.needs {
            let all = s.candidates(&n.url, &n.name)?;
            if !all.iter().any(|x| n.req.matches(&x.version)) {
                lone = Some(n.clone());
                break;
            }
        }
        let explained = lone.is_none().then(|| s.explain(&c));
        drop(s);
        if locked {
            return Err(format!(
                "`{}` is not in nabi.lock (or its requirement changed), and --locked forbids \
                 changing it",
                c.name
            ));
        }
        match (lone, explained) {
            (Some(n), _) => Err(self.no_version(&n.name, &n.text, &n.url)),
            (None, Some(e)) => Err(e),
            (None, None) => Err(format!("no version of package `{}` fits", c.name)),
        }
    }

    /// Indexes read for the search that were not fetched in this run.
    fn unfetched(&self) -> bool {
        self.read_urls()
            .iter()
            .any(|u| !self.fetched.iter().any(|f| f == u))
    }

    fn read_urls(&self) -> Vec<String> {
        let mut urls: Vec<String> = self.entries.keys().map(|(u, _)| u.clone()).collect();
        urls.sort();
        urls.dedup();
        urls
    }

    /// "no package .." / "no version .." for one requirement.
    fn no_version(&self, name: &str, req: &str, url: &str) -> String {
        let all = self
            .entries
            .get(&(url.to_owned(), manifest::key(name)))
            .cloned()
            .unwrap_or_default();
        let mut e = registry::no_match(&self.index, &all, name, req, url);
        if let Ok(r) = Req::parse(req) {
            if let (_, Some(s)) = registry::best_here(&all, &r) {
                e = format!("{e}\nhelp: {s}");
            }
        }
        e
    }

    /// A requirement of `name` from index `url`.
    fn need(name: &str, req: &str, url: &str) -> Result<solve::Need, String> {
        Ok(solve::Need {
            name: name.to_owned(),
            url: url.to_owned(),
            req: Req::parse(req).map_err(|e| format!("package `{name}`: {e}"))?,
            text: req.to_owned(),
        })
    }

    /// The versions of `name` in index `url` to try, in order: the locked
    /// one (`--locked`: only it), then the others this toolchain builds
    /// that are not yanked, newest first.
    fn versions(&mut self, url: &str, name: &str) -> Result<Vec<solve::Cand>, String> {
        let updating = self.updating(name);
        let bundled = self.bundled(url, name);
        // The toolchain's own versions need no index; when none fits, the
        // search fetches it and tries again.
        let bundle_only = !updating && !self.index.has(url) && !bundled.is_empty();
        if (updating || !self.index.has(url)) && !bundle_only {
            self.fetch_index(url, name)?;
        }
        let mut all = self.indexed(url, name, &bundled)?;
        let pinned = self
            .old
            .find(name)
            .filter(|l| !updating && l.url == url)
            .and_then(|l| Version::parse(l.version.as_deref()?).ok());
        if let Some(v) = &pinned {
            if !all.iter().any(|e| e.version == *v)
                && !self.fetched.iter().any(|u| u == url)
                && !self.opts.offline
            {
                self.fetch_index(url, name)?;
                all = self.indexed(url, name, &bundled)?;
            }
            if !all.iter().any(|e| e.version == *v) {
                return Err(format!(
                    "`{name}` {v} (in nabi.lock) is not in the index {url} (run `nabi update {name}`)"
                ));
            }
        }
        let mut order: Vec<&registry::Entry> = Vec::new();
        if let Some(v) = &pinned {
            order.extend(all.iter().filter(|e| e.version == *v));
        }
        if self.opts.locked && !updating && pinned.is_none() && !all.is_empty() {
            return Err(format!(
                "`{name}` is not in nabi.lock (or its requirement changed), and --locked forbids \
                 changing it"
            ));
        }
        if !self.opts.locked || updating {
            let mut rest: Vec<&registry::Entry> = all
                .iter()
                .filter(|e| !e.yanked && registry::builds_here(e))
                .filter(|e| pinned.as_ref() != Some(&e.version))
                .collect();
            // A version the toolchain carries first, unless updating.
            let carried =
                |e: &registry::Entry| !updating && bundled.iter().any(|b| b.version == e.version);
            rest.sort_by(|a, b| {
                carried(b)
                    .cmp(&carried(a))
                    .then_with(|| b.version.cmp(&a.version))
            });
            order.extend(rest);
        }
        let mut out = Vec::new();
        for e in order {
            let mut needs = Vec::new();
            for d in e.deps.iter().filter(|d| !d.dev && !d.native) {
                let at = d.registry.clone().unwrap_or_else(|| url.to_owned());
                let pkg = d.package.as_deref().unwrap_or(&d.name);
                needs.push(Self::need(pkg, &d.req, &at)?);
            }
            out.push(solve::Cand {
                name: e.name.clone(),
                version: e.version.clone(),
                needs,
            });
        }
        self.entries
            .insert((url.to_owned(), manifest::key(name)), all);
        Ok(out)
    }

    /// The versions of `name` this toolchain carries, for the default index.
    fn bundled(&self, url: &str, name: &str) -> Vec<registry::Entry> {
        match &self.bundle {
            Some(b) if registry::same_index(url, registry::DEFAULT) => b.entries(name),
            _ => Vec::new(),
        }
    }

    /// The versions of `name` in the cached index `url`, with the
    /// `bundled` ones it lacks.
    fn indexed(
        &self,
        url: &str,
        name: &str,
        bundled: &[registry::Entry],
    ) -> Result<Vec<registry::Entry>, String> {
        let mut all = if self.index.has(url) {
            registry::entries(&self.index, url, name)?
        } else {
            Vec::new()
        };
        for b in bundled {
            if !all.iter().any(|e| e.version == b.version) {
                all.push(b.clone());
            }
        }
        Ok(all)
    }

    /// A newer version every requirement on `p` accepts that only another
    /// toolchain builds, as a note.
    fn newer_elsewhere(&self, p: &solve::Pick) -> Option<String> {
        let all = self.entries.get(&(p.url.clone(), manifest::key(&p.name)))?;
        let n = all
            .iter()
            .filter(|e| !e.yanked && p.needs.iter().all(|(n, _)| n.req.matches(&e.version)))
            .max_by(|a, b| a.version.cmp(&b.version))?;
        (n.version > p.version && !registry::builds_here(n)).then(|| registry::needs_other(n))
    }

    /// The version of `name` that `req` alone picks, placed.
    fn locate_registry(
        &mut self,
        name: &str,
        req: &str,
        url: &str,
    ) -> Result<(PathBuf, Id, Option<IndexDeps>), String> {
        let need = Self::need(name, req, url)?;
        let p = self
            .search(vec![(need, solve::By::Root("the root package".to_owned()))])?
            .into_iter()
            .next()
            .ok_or_else(|| format!("package `{name}`: nothing picked"))?;
        let e = self.entries[&(p.url.clone(), manifest::key(&p.name))]
            .iter()
            .find(|e| e.version == p.version)
            .cloned()
            .ok_or_else(|| format!("package `{name}` {}: not in the index", p.version))?;
        self.place_registry(name, &e, url)
    }

    /// Entry `e` of index `url`: its files in the cache, checked against
    /// the lock and the index, and locked.
    fn place_registry(
        &mut self,
        name: &str,
        e: &registry::Entry,
        url: &str,
    ) -> Result<(PathBuf, Id, Option<IndexDeps>), String> {
        let updating = self.updating(name);
        let pinned = self
            .old
            .find(name)
            .filter(|l| !updating && l.url == url && l.version == Some(e.version.to_string()))
            .cloned();
        if let Some(l) = &pinned {
            if l.commit != e.rev {
                return Err(format!(
                    "the index {url} now says {name} {} is commit {}, but nabi.lock says {} \
                     (run `nabi update {name}` to accept it)",
                    e.version, e.rev, l.commit
                ));
            }
        }
        let dir = self.source_dir(name, e, url)?;
        check_index_features(&dir, name, e)?;
        if !self
            .new
            .packages
            .iter()
            .any(|p| manifest::same(&p.name, name))
        {
            self.new.packages.push(Locked {
                name: name.to_owned(),
                version: Some(e.version.to_string()),
                url: url.to_owned(),
                reference: crate::manifest::GitRef::Default,
                commit: e.rev.clone(),
                tree: e.tree.clone(),
            });
        }
        let registry =
            |d: &registry::IndexDep| Some(d.registry.clone().unwrap_or_else(|| url.to_owned()));
        let deps = e
            .deps
            .iter()
            .filter(|d| !d.dev && !d.native)
            .map(|d| d.to_dep(url))
            .collect();
        let natives = e
            .deps
            .iter()
            .filter(|d| d.native)
            .map(|d| NativeDep {
                name: d.name.clone(),
                source: NativeSource::Registry {
                    req: d.req.clone(),
                    registry: registry(d),
                },
            })
            .collect();
        Ok((
            dir,
            Id::Registry {
                registry: url.to_owned(),
                vers: e.version.clone(),
            },
            Some((deps, natives, e.features.clone(), e.links.clone())),
        ))
    }

    /// The files of `e`: the toolchain's own copy, a git checkout already
    /// in the cache, else the registry's archive (`archive`), else its git
    /// source.
    fn source_dir(
        &mut self,
        name: &str,
        e: &registry::Entry,
        url: &str,
    ) -> Result<PathBuf, String> {
        if registry::same_index(url, registry::DEFAULT) {
            if let Some(dir) = self.bundle.as_ref().and_then(|b| b.src(e)) {
                return Ok(dir);
            }
        }
        if !self.cache.has_commit(&e.url, &e.rev) {
            if let Some(sum) = &e.archive {
                let api = registry::api_cached(&self.index, url);
                match archive::fetch(
                    &self.home,
                    api.as_deref(),
                    e,
                    sum,
                    self.opts.offline,
                    self.opts.quiet,
                ) {
                    Ok(dir) => return Ok(dir),
                    Err(archive::Miss::Bad(m)) => return Err(m),
                    Err(archive::Miss::Absent(why)) => {
                        if !self.opts.quiet && !self.opts.offline {
                            nabic_core::term::note(&format!(
                                "{name} {}: {why}; fetching its git source",
                                e.version
                            ));
                        }
                    }
                }
            }
            if self.opts.offline {
                let keep = if e.official {
                    "; `nabi component add packages` (or `update`) keeps the official packages for \
                     offline use"
                } else {
                    ""
                };
                return Err(format!(
                    "package `{name}` {} is not in the cache, and --offline is set: run the \
                     command once without --offline{keep}",
                    e.version
                ));
            }
            self.fetch(&e.url, name)?;
            if !self.cache.has_commit(&e.url, &e.rev) {
                return Err(format!(
                    "package `{name}` {}: commit {} is not in {}",
                    e.version, e.rev, e.url
                ));
            }
        }
        let (dir, tree) = self.cache.checkout(&e.url, &e.rev)?;
        if tree != e.tree {
            return Err(format!(
                "package `{name}` {}: commit {} has tree {tree}, but the index says {}",
                e.version, e.rev, e.tree
            ));
        }
        Ok(dir)
    }

    /// The native packages every requirement reaches: one per name.
    fn natives(&mut self) -> Result<Vec<native::Resolved>, String> {
        let mut out: Vec<native::Resolved> = Vec::new();
        // Per name: its place in `out`, identity, requirer and requirement.
        let mut seen: HashMap<String, (usize, NativeId, String, String)> = HashMap::default();
        let mut i = 0;
        while i < self.natives.len() {
            let (dep, base, by, user) = self.natives[i].clone();
            i += 1;
            if matches!(
                dep.source,
                NativeSource::System(_) | NativeSource::Framework
            ) {
                self.system.push((dep, user));
                continue;
            }
            let how = match &dep.source {
                NativeSource::Registry { req, registry } => format!(
                    "`{req}` from {}",
                    registry.as_deref().unwrap_or(&self.registry)
                ),
                NativeSource::Direct { url, sha256 } => format!("{url} (sha256 {sha256})"),
                NativeSource::System(_) | NativeSource::Framework => continue,
            };
            if let Some((j, id, prev_by, prev_how)) = seen.get(&manifest::key(&dep.name)) {
                let fits = match (&dep.source, id) {
                    (NativeSource::Registry { req, registry }, NativeId::Index { url, vers }) => {
                        registry.as_deref().unwrap_or(&self.registry) == url
                            && Req::parse(req)?.matches(vers)
                    }
                    (NativeSource::Direct { sha256, .. }, NativeId::File { sha256: s }) => {
                        sha256 == s
                    }
                    _ => false,
                };
                if !fits {
                    let e = &out[*j].entry;
                    return Err(format!(
                        "conflicting requirements for native package `{}`:\n  {prev_how} \
                         (required by {prev_by}) chose {}\n  {how} (required by {by})\none build \
                         has one version of a native package",
                        dep.name, e.version
                    ));
                }
                if !out[*j].users.contains(&user) {
                    out[*j].users.push(user);
                }
                continue;
            }
            let (entry, id, base, registry) = match &dep.source {
                NativeSource::Registry { req, registry } => {
                    let url = registry.clone().unwrap_or_else(|| self.registry.clone());
                    let e = self.locate_native(&dep.name, req, &url)?;
                    let id = NativeId::Index {
                        url: url.clone(),
                        vers: e.version.clone(),
                    };
                    (e, id, None, url)
                }
                NativeSource::System(_) | NativeSource::Framework => continue,
                NativeSource::Direct { url, sha256 } => {
                    let loc = native::locate(base.as_deref(), url)?;
                    let e = native::direct(&self.home, &dep.name, &loc, sha256, self.opts.offline)?;
                    let id = NativeId::File {
                        sha256: sha256.clone(),
                    };
                    (e, id, Some(native::dir_of(&loc)), self.registry.clone())
                }
            };
            let me = format!("native package `{}`", dep.name);
            for d in &entry.deps {
                let mut d = d.clone();
                if let NativeSource::Registry {
                    registry: r @ None, ..
                } = &mut d.source
                {
                    *r = Some(registry.clone());
                }
                self.natives
                    .push((d, base.clone(), me.clone(), user.clone()));
            }
            seen.insert(manifest::key(&dep.name), (out.len(), id, by, how));
            out.push(native::Resolved {
                name: dep.name,
                entry,
                base,
                users: vec![user],
            });
        }
        // A native package's dependencies serve its users too.
        let mut changed = true;
        while changed {
            changed = false;
            for i in 0..out.len() {
                for d in out[i].entry.deps.clone() {
                    let Some(j) = out.iter().position(|o| manifest::same(&o.name, &d.name)) else {
                        continue;
                    };
                    for u in out[i].users.clone() {
                        if !out[j].users.contains(&u) {
                            out[j].users.push(u);
                            changed = true;
                        }
                    }
                }
            }
        }
        Ok(out)
    }

    /// The version `req` picks (locked while it matches), checked against
    /// the lock.
    fn locate_native(&mut self, name: &str, req: &str, url: &str) -> Result<native::Entry, String> {
        let parsed = Req::parse(req).map_err(|e| format!("native package `{name}`: {e}"))?;
        let updating = self.updating(name);
        let pinned = self
            .old
            .find_native(name)
            .filter(|l| !updating && l.url == url)
            .and_then(|l| Some((l.clone(), Version::parse(&l.version).ok()?)))
            .filter(|(_, v)| parsed.matches(v));
        if pinned.is_none() && self.opts.locked && !updating {
            return Err(format!(
                "native package `{name}` is not in nabi.lock (or its requirement changed), and \
                 --locked forbids changing it"
            ));
        }
        if updating || !self.index.has(url) {
            self.fetch_index(url, name)?;
        }
        let pick = |all: &[native::Entry]| match &pinned {
            Some((_, v)) => all.iter().find(|e| e.version == *v).cloned(),
            None => native::best(all, &parsed).cloned(),
        };
        let mut all = native::entries(&self.index, url, name)?;
        let mut chosen = pick(&all);
        if chosen.is_none() && !self.fetched.iter().any(|u| u == url) && !self.opts.offline {
            self.fetch_index(url, name)?;
            all = native::entries(&self.index, url, name)?;
            chosen = pick(&all);
        }
        let e = match (chosen, &pinned) {
            (Some(e), _) => e,
            (None, Some((_, v))) => {
                return Err(format!(
                    "native package `{name}` {v} (in nabi.lock) is not in the index {url} (run \
                     `nabi update {name}`)"
                ));
            }
            (None, None) => return Err(native::no_match(&self.index, &all, name, req, url)),
        };
        let sum = e.checksum();
        if let Some((l, _)) = &pinned {
            if l.sha256 != sum {
                return Err(format!(
                    "the index {url} changed native package `{name}` {} (entry sha256 {sum}, \
                     nabi.lock says {}; run `nabi update {name}` to accept it)",
                    e.version, l.sha256
                ));
            }
        }
        if self.new.find_native(name).is_none() {
            self.new.natives.push(LockedNative {
                name: name.to_owned(),
                version: e.version.to_string(),
                url: url.to_owned(),
                sha256: sum,
            });
        }
        Ok(e)
    }

    /// Bring index `url` up to date, once per run.
    fn fetch_index(&mut self, url: &str, name: &str) -> Result<(), String> {
        if self.fetched.iter().any(|u| u == url) {
            return Ok(());
        }
        if self.opts.offline {
            if self.index.has(url) {
                return Ok(());
            }
            return Err(format!(
                "package `{name}` needs the index {url}, but --offline is set (it is not in the cache)"
            ));
        }
        if !self.opts.quiet {
            nabic_core::term::status("Updating", &format!("index {url}"));
        }
        self.index.fetch(url)?;
        self.fetched.push(url.to_owned());
        Ok(())
    }

    fn fetch(&mut self, url: &str, name: &str) -> Result<(), String> {
        if self.fetched.iter().any(|u| u == url) {
            return Ok(());
        }
        if self.opts.offline {
            return Err(format!(
                "`{name}` needs a fetch of {url}, but --offline is set (it is not in the cache)"
            ));
        }
        if !self.opts.quiet {
            nabic_core::term::status("Fetching", url);
        }
        self.cache.fetch(url)?;
        self.fetched.push(url.to_owned());
        Ok(())
    }
}

/// A registry package's dependencies, native packages, features and `links`.
type IndexDeps = (Vec<Dep>, Vec<NativeDep>, Features, Option<String>);

/// The library root of dependency `name` (`id`) at `dir`.
fn library_of(name: &str, id: &Id, dir: &Path) -> Result<PathBuf, String> {
    let root = dir.join("src/lib.nabi");
    if !root.is_file() && dir.join("src/main.nabi").is_file() {
        return Err(format!(
            "dependency `{name}` ({id}) is an executable package (src/main.nabi, no \
             src/lib.nabi): a dependency must be a library; a program is installed with \
             `nabi install`"
        ));
    }
    if !root.is_file() {
        return Err(format!(
            "dependency `{name}` ({id}) is not a library package: no src/lib.nabi in {}",
            dir.display()
        ));
    }
    Ok(root)
}

/// At most one package of the graph links each native library.
fn check_links(nodes: &[Node]) -> Result<(), String> {
    let mut seen: HashMap<&str, &str> = HashMap::default();
    for n in nodes {
        let Some(l) = n.links.as_deref() else {
            continue;
        };
        if let Some(prev) = seen.insert(l, &n.name) {
            return Err(format!(
                "packages `{prev}` and `{}` both link the native library `{l}` (`links: \"{l}\"`); \
                 one build has one package per native library: depend on only one of them",
                n.name
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dependencies_come_first() {
        let r = |n: &str, deps: &[&'static str]| (n.to_owned(), deps.to_vec());
        // Breadth-first from the root: b, a, c; c needs a.
        let all = vec![r("b", &["a", "c"]), r("a", &[]), r("c", &["a"])];
        let deps = |p: &(String, Vec<&str>)| p.1.iter().map(|d| (*d).to_owned()).collect();
        let names: Vec<String> = order(all, |p| &p.0, deps)
            .into_iter()
            .map(|d| d.0)
            .collect();
        assert_eq!(names, ["a", "c", "b"]);
    }

    /// A node: name, dependencies (`?` optional), features.
    type Spec<'a> = (&'a str, &'a [&'a str], &'a [(&'a str, &'a [&'a str])]);

    fn graph(nodes: &[Spec<'_>]) -> Vec<Node> {
        nodes
            .iter()
            .map(|(name, deps, features)| Node {
                name: (*name).to_owned(),
                code: (*name).to_owned(),
                links: None,
                root: PathBuf::new(),
                source: String::new(),
                version: None,
                deps: deps
                    .iter()
                    .map(|d| Dep {
                        optional: d.ends_with('?'),
                        ..Dep::new(
                            d.trim_end_matches('?').to_owned(),
                            Source::Path(String::new()),
                        )
                    })
                    .collect(),
                features: features
                    .iter()
                    .map(|(f, l)| ((*f).to_owned(), l.iter().map(|e| (*e).to_owned()).collect()))
                    .collect(),
            })
            .collect()
    }

    #[test]
    fn features_unify_and_follow_dependencies() {
        let nodes = graph(&[
            (
                "tls",
                &[],
                &[("default", &["ring"]), ("ring", &[]), ("fast", &[])],
            ),
            ("json", &[], &[("pretty", &[])]),
            (
                "http",
                &["tls?", "json"],
                &[
                    ("tls", &["dep:tls"]),
                    ("both", &["tls?/fast", "json/pretty"]),
                ],
            ),
            ("cli", &["http"], &[]),
        ]);
        let run = |root: &[&str], fs: &[(&str, &[&str])], o: Options| {
            let root = graph(&[("", root, fs)]).remove(0);
            let mut on = Features::default();
            activate(&nodes, root.deps, &root.features, &o, &mut on).map(|mine| (mine, on))
        };
        let opts = |features: &[&str]| Options {
            features: features.iter().map(|f| (*f).to_owned()).collect(),
            ..Options::default()
        };
        // `http` without `tls`: the weak `tls?/fast` waits, `tls` is out.
        let (_, on) = run(&["http", "cli"], &[], opts(&["http/both"])).unwrap();
        let names: Vec<&str> = on.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(names, ["json", "http", "cli"]);
        assert_eq!(on[0].1, ["pretty"]);
        // `tls` comes in (with its default) and the weak feature applies.
        let (_, on) = run(&["http"], &[], opts(&["http/both", "http/tls"])).unwrap();
        assert_eq!(
            on[0],
            (
                "tls".into(),
                vec!["default".into(), "fast".into(), "ring".into()]
            )
        );
        // The root's default, and `--no-default-features`.
        let fs: &[(&str, &[&str])] = &[("default", &["x"]), ("x", &["http/tls"])];
        let (mine, on) = run(&["http"], fs, opts(&[])).unwrap();
        assert_eq!((mine.len(), on.len()), (2, 3));
        let o = Options {
            no_default_features: true,
            ..Options::default()
        };
        let (mine, on) = run(&["http"], fs, o).unwrap();
        assert_eq!((mine.len(), on.len()), (0, 2));
        let e = run(&["http"], &[], opts(&["http/nope"])).unwrap_err();
        assert_eq!(
            e,
            "package `http` has no feature `nope` (required by --features)"
        );
        let e = run(&["http"], &[], opts(&["zip/x"])).unwrap_err();
        assert_eq!(e, "the root package has no dependency `zip`");
    }
}
