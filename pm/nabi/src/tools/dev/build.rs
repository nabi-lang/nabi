//! What a dev session builds and how: the host, once, with every package
//! that does not change while the program runs, and one hot plugin per
//! local package per version. Every build is a `nabic` process on the
//! root's resolved graph, so all of them agree on package hashes, and
//! they share one target directory, so a rebuild is incremental.

use crate::Global;
use crate::manifest::{self, Source};
use nabic_core::term;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// A package built as a hot plugin.
pub(crate) struct Hot {
    pub(crate) name: String,
    pub(crate) dir: PathBuf,
    /// The hot packages it depends on (indices, all smaller).
    pub(crate) deps: Vec<usize>,
    /// Its `src/lib.nabi`, as its dependents' `--extern` names it.
    lib: Option<PathBuf>,
    /// Its features (`--cfg=feature=..` when it is built).
    features: Vec<String>,
    /// The next version to build.
    pub(crate) next: u32,
    /// The version the program runs (its layout is the next build's base).
    pub(crate) live: Option<u32>,
}

/// A session's build plan.
pub(crate) struct Plan {
    /// Hot packages, dependencies first; the root is the last.
    pub(crate) hot: Vec<Hot>,
    /// `<target>/dev`.
    pub(crate) dev: PathBuf,
    /// The manifests and locks whose change restarts the session.
    pub(crate) manifests: Vec<PathBuf>,
    /// The program's arguments.
    pub(crate) args: Vec<std::ffi::OsString>,
    nabic: PathBuf,
    /// nabic's options before the command (`--color`, ..).
    pre: Vec<String>,
    /// `--extern`/`--extern-feature` of the packages linked into the host.
    fixed: Vec<String>,
    /// `--extern` of the hot packages: (lib.nabi, argument).
    hot_externs: Vec<(PathBuf, String)>,
    /// `--extern-feature` of the hot packages: (code name, argument).
    hot_features: Vec<(String, String)>,
    /// The rest: link and lint options, the user's options.
    common: Vec<String>,
    root_cfg: Vec<String>,
    handoffs: Vec<(&'static str, Option<std::ffi::OsString>)>,
    verbose: bool,
}

/// `<pkg>.<n>.<ext>`: version `n` of a hot package.
pub(crate) fn plugin_file(name: &str, n: u32) -> String {
    let t = nabic_target::TargetSpec::host();
    let ext = t.dylib("@");
    let ext = ext.split('@').nth(1).unwrap_or(".so");
    format!("{}.{n}{ext}", nabic_interface::code_name(name))
}

/// The host executable's file name.
pub(crate) fn host_file(root: &str) -> String {
    format!(
        "{}-host{}",
        nabic_interface::code_name(root),
        std::env::consts::EXE_SUFFIX
    )
}

/// The host's source: the runtime's dev entry loads the first builds in
/// order, then calls the root's `main`.
pub(crate) const HOST_MAIN: &str = "\
// Written by `nabi dev`: the program's host. It loads the hot packages,
// then runs the root's `main`; edits reach it through the dev channel.

fn main() {
    std::reload::host_main();
}
";

/// The exit code of a host whose plugins did not load.
pub(crate) const LOAD_FAILED: i32 = 101;

impl Plan {
    /// Plan the session of the package in the current directory.
    pub(crate) fn new(
        g: &Global,
        pre: &[String],
        post: &[String],
        args: Vec<std::ffi::OsString>,
    ) -> Result<Self, String> {
        let dir = std::env::current_dir().map_err(|e| e.to_string())?;
        let pkg = nabic_interface::discover(&dir)?;
        if pkg.anonymous || !dir.join("src/main.nabi").is_file() {
            return Err(format!(
                "`nabi dev` runs a program, and {} has no src/main.nabi: run it in the program's \
                 package, or name a workspace member with `-p <member>`",
                crate::shown(&dir)
            ));
        }
        let inv = crate::invocation(g, "build", pre, post, None)?;
        for (name, value) in &inv.handoffs {
            nabic_core::set_handoff(name, value.as_deref());
        }
        let t = crate::target_for(g, None)?;
        let mut opts = crate::options(g);
        let post = crate::feature_flags(post, &mut opts)?;
        opts.target = nabic_target::TargetSpec::resolve(crate::target_arg(&post)).ok();
        let graph = crate::resolve::resolve(&crate::home(), &t.ctx, &opts)?;
        let root_name = t.ctx.manifest.name.clone().unwrap_or(pkg.name);
        // The packages edited here: those of `path` sources.
        let mut hot: Vec<Hot> = Vec::new();
        for p in graph
            .packages
            .iter()
            .filter(|p| p.source.starts_with("path+"))
        {
            let Some(d) = p.root.parent().and_then(Path::parent) else {
                continue;
            };
            hot.push(Hot {
                name: p.name.clone(),
                dir: real(d),
                deps: Vec::new(),
                lib: Some(p.root.clone()),
                features: p.features.clone(),
                next: 1,
                live: None,
            });
        }
        hot.push(Hot {
            name: root_name,
            dir: real(&dir),
            deps: Vec::new(),
            lib: None,
            features: graph.features,
            next: 1,
            live: None,
        });
        let hot = order(hot)?;
        // Split nabic's arguments: [pre.., "build", extra.., post..].
        let at = g.color.iter().count() + pre.len();
        let (head, rest) = inv.args.split_at(at.min(inv.args.len()));
        let rest = rest.get(1..).unwrap_or_default();
        let mut plan = Self {
            dev: PathBuf::new(),
            manifests: Vec::new(),
            args,
            nabic: crate::toolchain::nabic_for()?,
            pre: head.to_vec(),
            fixed: Vec::new(),
            hot_externs: Vec::new(),
            hot_features: Vec::new(),
            common: Vec::new(),
            root_cfg: Vec::new(),
            handoffs: inv.handoffs,
            verbose: g.verbose,
            hot,
        };
        let libs: Vec<(PathBuf, String)> = (plan.hot.iter())
            .filter_map(|h| Some((h.lib.clone()?, h.name.clone())))
            .collect();
        let mut target_dir = None;
        for a in rest {
            if let Some(v) = a.strip_prefix("--extern=") {
                let (_, path) = v.split_once('=').unwrap_or((v, ""));
                match libs.iter().find(|(l, _)| Path::new(path) == l) {
                    Some((l, _)) => plan.hot_externs.push((l.clone(), a.clone())),
                    None => plan.fixed.push(a.clone()),
                }
            } else if let Some(v) = a.strip_prefix("--extern-feature=") {
                let (name, _) = v.split_once('=').unwrap_or((v, ""));
                let hot = libs
                    .iter()
                    .any(|(_, n)| nabic_interface::code_name(n) == name);
                if hot {
                    plan.hot_features.push((name.to_owned(), a.clone()));
                } else {
                    plan.fixed.push(a.clone());
                }
            } else if a.starts_with("--cfg=feature=") {
                plan.root_cfg.push(a.clone());
            } else if let Some(d) = a.strip_prefix("--target-dir=") {
                target_dir = Some(PathBuf::from(d));
            } else if a == "--" {
                break;
            } else if !a.starts_with("--name=") {
                plan.common.push(a.clone());
            }
        }
        let target = target_dir
            .or_else(|| std::env::var_os("NABI_TARGET_DIR").map(PathBuf::from))
            .unwrap_or_else(|| dir.join("target"));
        let target = std::path::absolute(&target).unwrap_or(target);
        plan.common
            .push(format!("--target-dir={}", target.display()));
        plan.dev = target.join("dev");
        // What restarts the session.
        for h in &plan.hot {
            plan.manifests.extend(manifest::find(&h.dir));
        }
        if let Some(ws) = &t.ctx.workspace {
            plan.manifests.extend(manifest::find(&ws.root));
        }
        plan.manifests.extend(t.ctx.lock_path);
        plan.manifests.sort();
        plan.manifests.dedup();
        Ok(plan)
    }

    pub(crate) fn root(&self) -> &Hot {
        &self.hot[self.hot.len() - 1]
    }

    /// The root's `main` as the host asks for it.
    pub(crate) fn entry(&self) -> String {
        format!("{}::main", nabic_interface::code_name(&self.root().name))
    }

    /// Empty `<target>/dev` of an earlier session's files.
    pub(crate) fn clean(&self) -> Result<(), String> {
        if let Ok(rd) = std::fs::read_dir(&self.dev) {
            for e in rd.flatten() {
                let p = e.path();
                // A file still mapped by a program elsewhere (Windows) stays.
                if p.is_dir() {
                    let _ = std::fs::remove_dir_all(&p);
                } else {
                    let _ = std::fs::remove_file(&p);
                }
            }
        }
        std::fs::create_dir_all(self.dev.join("host"))
            .map_err(|e| format!("cannot create {}: {e}", self.dev.display()))
    }

    fn nabic(&self) -> Command {
        let mut c = Command::new(&self.nabic);
        c.args(&self.pre).arg("build");
        if !self.verbose {
            c.arg("-q");
        }
        for (name, value) in &self.handoffs {
            crate::handoff(&mut c, name, value.as_deref());
        }
        c.stdin(Stdio::null());
        c
    }

    /// The host executable.
    pub(crate) fn host_path(&self) -> PathBuf {
        self.dev.join(host_file(&self.root().name))
    }

    /// Build the host: the runtime and the packages that do not change.
    pub(crate) fn build_host(&self) -> Result<(), String> {
        let src = self.dev.join("host/main.nabi");
        std::fs::write(&src, HOST_MAIN)
            .map_err(|e| format!("cannot write {}: {e}", src.display()))?;
        let mut c = self.nabic();
        c.arg(&src);
        c.args(&self.fixed).args(&self.common);
        c.arg("-o").arg(self.host_path());
        run(c, "the host")
    }

    /// The command that builds version `n` of hot package `i`.
    fn hot_command(&self, i: usize, n: u32) -> Command {
        let h = &self.hot[i];
        let mut c = self.nabic();
        c.arg(&h.dir).arg(format!("--name={}", h.name)).arg("--hot");
        if let Some(live) = h.live {
            c.arg(format!(
                "--hot-base={}",
                self.dev.join(layout_file(&h.name, live)).display()
            ));
        }
        c.args(&self.fixed);
        let own = nabic_interface::code_name(&h.name);
        for j in reach(&self.hot, i) {
            c.arg("--hot-dep")
                .arg(nabic_interface::code_name(&self.hot[j].name));
        }
        for (lib, a) in &self.hot_externs {
            if h.lib.as_ref() != Some(lib) {
                c.arg(a);
            }
        }
        for (name, a) in &self.hot_features {
            if *name != own {
                c.arg(a);
            }
        }
        if i == self.hot.len() - 1 {
            c.args(&self.root_cfg);
        } else {
            c.args(h.features.iter().map(|f| format!("--cfg=feature={f}")));
        }
        c.args(&self.common);
        c.arg("-o").arg(self.dev.join(plugin_file(&h.name, n)));
        c
    }

    /// Build the next version of hot package `i`; its file.
    pub(crate) fn build_hot(&mut self, i: usize) -> Result<PathBuf, String> {
        let n = self.hot[i].next;
        let c = self.hot_command(i, n);
        run(c, &format!("`{}`", self.hot[i].name))?;
        self.hot[i].next = n + 1;
        Ok(self.dev.join(plugin_file(&self.hot[i].name, n)))
    }

    /// The packages to rebuild when the sources of `changed` change: those
    /// and every hot package depending on them, dependencies first.
    pub(crate) fn affected(&self, changed: &[usize]) -> Vec<usize> {
        affected(&self.hot, changed)
    }
}

/// The hot packages `i` depends on, directly or not, sorted.
fn reach(hot: &[Hot], i: usize) -> Vec<usize> {
    let mut out: Vec<usize> = Vec::new();
    let mut todo = hot[i].deps.clone();
    while let Some(j) = todo.pop() {
        if !out.contains(&j) {
            out.push(j);
            todo.extend(&hot[j].deps);
        }
    }
    out.sort_unstable();
    out
}

fn affected(hot: &[Hot], changed: &[usize]) -> Vec<usize> {
    let mut out: Vec<usize> = Vec::new();
    for (i, h) in hot.iter().enumerate() {
        if changed.contains(&i) || h.deps.iter().any(|d| out.contains(d)) {
            out.push(i);
        }
    }
    out
}

/// `<pkg>.<n>.layout`, beside the plugin.
pub(crate) fn layout_file(name: &str, n: u32) -> String {
    format!("{}.{n}.layout", nabic_interface::code_name(name))
}

/// Run build `c` of `what`; its diagnostics stream through.
fn run(mut c: Command, what: &str) -> Result<(), String> {
    let status = c
        .status()
        .map_err(|e| format!("cannot build {what}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{what} did not build"))
    }
}

fn real(d: &Path) -> PathBuf {
    nabic_core::real_path(d).unwrap_or_else(|_| d.to_path_buf())
}

/// `hot` with each package's hot dependencies found, dependencies first.
fn order(hot: Vec<Hot>) -> Result<Vec<Hot>, String> {
    let n = hot.len();
    let mut edges: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (i, h) in hot.iter().enumerate() {
        let Some(m) = manifest::find(&h.dir) else {
            continue;
        };
        let m = manifest::load(&m)?;
        for d in &m.deps {
            let Source::Path(p) = &d.source else {
                continue;
            };
            let to = real(&h.dir.join(p));
            if let Some(j) = hot.iter().position(|o| o.dir == to) {
                edges[i].push(j);
            }
        }
    }
    // Depth-first, keeping the root last.
    let mut done: Vec<usize> = Vec::new();
    let mut busy = vec![false; n];
    for i in std::iter::once(n - 1).chain(0..n) {
        if let Err(j) = visit(i, &edges, &mut busy, &mut done) {
            return Err(format!(
                "the path dependencies form a cycle through `{}`: remove one of the `Dep::path` lines",
                hot[j].name
            ));
        }
    }
    // The root's dependencies come before it; unused ones are kept
    // before it too (they are in the graph through another package).
    let root = n - 1;
    done.retain(|&i| i != root);
    done.push(root);
    let pos: Vec<usize> = (0..n)
        .map(|i| done.iter().position(|&d| d == i).unwrap_or(0))
        .collect();
    let mut taken: Vec<Option<Hot>> = hot.into_iter().map(Some).collect();
    let mut out = Vec::with_capacity(n);
    for &i in &done {
        let mut h = taken[i].take().expect("each package once");
        h.deps = edges[i].iter().map(|&j| pos[j]).collect();
        out.push(h);
    }
    Ok(out)
}

/// Visit `i` after its dependencies; `Err` names a package on a cycle.
fn visit(
    i: usize,
    edges: &[Vec<usize>],
    busy: &mut [bool],
    done: &mut Vec<usize>,
) -> Result<(), usize> {
    if done.contains(&i) {
        return Ok(());
    }
    if busy[i] {
        return Err(i);
    }
    busy[i] = true;
    for &j in &edges[i] {
        visit(j, edges, busy, done)?;
    }
    done.push(i);
    Ok(())
}

/// Say what the session builds, under `-v`.
pub(crate) fn describe(plan: &Plan) {
    let names: Vec<&str> = plan.hot.iter().map(|h| h.name.as_str()).collect();
    term::note(&format!("hot packages: {}", names.join(", ")));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hot(name: &str, dir: &Path) -> Hot {
        Hot {
            name: name.to_owned(),
            dir: dir.to_path_buf(),
            deps: Vec::new(),
            lib: None,
            features: Vec::new(),
            next: 1,
            live: None,
        }
    }

    #[test]
    fn orders_dependencies_first() {
        let dir = nabic_core::tmp::test_dir("dev-order");
        let w = |p: &str, t: &str| {
            let p = dir.join(p);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, t).unwrap();
        };
        w(
            "app/nabi.nabion",
            "Package { name: \"app\", deps: { \"b\": Dep::path(\"../b\") } }\n",
        );
        w(
            "b/nabi.nabion",
            "Package { name: \"b\", deps: { \"a\": Dep::path(\"../a\") } }\n",
        );
        w("a/nabi.nabion", "Package { name: \"a\" }\n");
        let d = |p: &str| real(&dir.join(p));
        let list = vec![hot("b", &d("b")), hot("a", &d("a")), hot("app", &d("app"))];
        let out = order(list).unwrap();
        let names: Vec<&str> = out.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(names, ["a", "b", "app"]);
        assert_eq!(out[1].deps, [0]);
        assert_eq!(out[2].deps, [1]);
        assert_eq!(affected(&out, &[0]), [0, 1, 2]);
        assert_eq!(affected(&out, &[1]), [1, 2]);
        assert_eq!(affected(&out, &[2]), [2]);
        assert_eq!(reach(&out, 2), [0, 1]);
        assert!(reach(&out, 0).is_empty());
        w(
            "a/nabi.nabion",
            "Package { name: \"a\", deps: { \"b\": Dep::path(\"../b\") } }\n",
        );
        let list = vec![hot("b", &d("b")), hot("a", &d("a")), hot("app", &d("app"))];
        let e = order(list).err().unwrap();
        assert!(e.contains("form a cycle"), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_names() {
        let f = plugin_file("my-game", 3);
        assert!(f.starts_with("my_game.3."), "{f}");
        assert!(!f.ends_with('.'), "{f}");
        assert_eq!(layout_file("my-game", 3), "my_game.3.layout");
        assert!(host_file("g").starts_with("g-host"));
    }
}
