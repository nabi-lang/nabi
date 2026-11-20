//! Prebuilt dependencies: each source `--extern` is
//! built once into `lib<name>.a` + `<name>.nabihir` and the build goes on
//! with the NABIHIR; rebuilt when its fingerprint changes. Each sees the
//! ones before it (its deps, and the GC type ids to start after), so
//! `externs` come dependencies first; each has its own features.

use crate::fresh::Fingerprint;
use crate::{BuildOptions, Session, TargetSpec, nabihir};
use nabic_diagnostic::print_rendered;
use std::path::{Path, PathBuf};

/// What dependency builds share with the build that needs them.
pub struct DepBuild<'a> {
    pub bootstrap: &'a [PathBuf],
    pub cfg: &'a [String],
    /// `(dependency, feature)`: `--extern-feature`.
    pub features: &'a [(String, String)],
    pub opts: &'a BuildOptions,
    pub target: &'a TargetSpec,
    /// `target/[<target>/]<profile>/deps`.
    pub dir: &'a Path,
    pub quiet: bool,
}

impl DepBuild<'_> {
    /// `externs` with every source dependency (a `src/lib.nabi`) replaced by
    /// its prebuilt `<name>.nabihir`; dependency errors are reported.
    pub fn prebuild(
        &self,
        externs: &[(String, PathBuf)],
    ) -> Result<Vec<(String, PathBuf)>, String> {
        let mut out: Vec<(String, PathBuf)> = Vec::new();
        for (i, (name, root)) in externs.iter().enumerate() {
            if root.extension().is_some_and(|e| e == "nabihir") {
                out.push((name.clone(), root.clone()));
                continue;
            }
            // Another name of an earlier path: an alias, built once.
            if let Some(j) = externs[..i].iter().position(|(_, r)| r == root) {
                let built = out[j].1.clone();
                out.push((name.clone(), built));
                continue;
            }
            std::fs::create_dir_all(self.dir)
                .map_err(|e| format!("cannot create {}: {e}", self.dir.display()))?;
            let archive = self.dir.join(format!("lib{name}.a"));
            let nabihir = nabihir::companions(&archive).0;
            let fp = self.fingerprint(name, root, &archive, &out);
            if let Some(warnings) = fp.fresh() {
                print_rendered(&warnings);
            } else {
                fp.clear();
                let (read, warnings) = self.build(name, root, &archive, &out)?;
                fp.save(&read, &warnings);
            }
            out.push((name.clone(), nabihir));
        }
        Ok(out)
    }

    /// Dependency `name`'s cfg: the build's without its features, and
    /// its own.
    fn cfg_of(&self, name: &str) -> Vec<String> {
        let mut out: Vec<String> = self
            .cfg
            .iter()
            .filter(|c| crate::feature_of(c).is_none())
            .cloned()
            .collect();
        out.extend(
            self.features
                .iter()
                .filter(|(n, _)| n == name)
                .map(|(_, f)| format!("feature=\"{f}\"")),
        );
        out
    }

    fn fingerprint(
        &self,
        name: &str,
        root: &Path,
        archive: &Path,
        before: &[(String, PathBuf)],
    ) -> Fingerprint {
        let mut words = vec![
            format!("{:?}", self.opts),
            format!("target {:016x}", self.target.hash()),
            name.to_owned(),
        ];
        words.extend(self.cfg_of(name));
        let mut inputs = vec![root.parent().unwrap_or(root).to_path_buf()];
        let mut stamped = Vec::new();
        for b in self.bootstrap {
            words.push(b.display().to_string());
            inputs.push(b.parent().unwrap_or(b).to_path_buf());
        }
        for (n, p) in before {
            words.push(format!("{n}={}", p.display()));
            let (nabihir, a) = nabihir::companions(p);
            inputs.push(nabihir);
            stamped.push(a);
        }
        let also = vec![nabihir::companions(archive).0];
        Fingerprint::new(archive, also, &words, &inputs, &stamped)
            .kept_in(self.dir.join(format!("{name}.fp")))
    }

    /// Build one dependency; the files it read and its rendered warnings.
    fn build(
        &self,
        name: &str,
        root: &Path,
        archive: &Path,
        before: &[(String, PathBuf)],
    ) -> Result<(Vec<PathBuf>, String), String> {
        if !self.quiet {
            // The package directory, above `src/lib.nabi`.
            let dir = root.parent().unwrap_or(root);
            let dir = if dir.ends_with("src") {
                dir.parent().unwrap_or(dir)
            } else {
                dir
            };
            // `src/lib.nabi` relative to the current directory.
            let dir = if dir.as_os_str().is_empty() {
                Path::new(".")
            } else {
                dir
            };
            nabic_core::term::status("Compiling", &shown(name, dir));
        }
        let mut s = Session::new();
        for b in self.bootstrap {
            s.add_bootstrap_file(b.clone());
        }
        for c in self.cfg_of(name) {
            s.add_cfg(c);
        }
        for (n, p) in before {
            s.add_extern(n.clone(), p.clone());
        }
        s.set_lib_name(Some(name.to_owned()));
        s.set_target(self.target);
        s.set_remap(self.opts.remap.clone());
        let r = s.compile_file(root);
        // Its uses of its own deprecated items are for its author.
        let warnings: String = r
            .warnings
            .iter()
            .filter(|w| {
                !w.lint
                    .as_deref()
                    .is_some_and(|l| l.starts_with("deprecated"))
            })
            .map(|w| r.render(w))
            .collect();
        print_rendered(&warnings);
        let failed = || format!("could not build dependency `{name}`");
        if r.has_errors() {
            r.report_errors();
            return Err(failed());
        }
        r.archive(self.opts, archive).map_err(|d| {
            r.report(&d);
            failed()
        })?;
        Ok((r.files, warnings))
    }
}

/// Dependency `name` in `dir` for its `Compiling` line: `json v0.1.0` when
/// nabi fetched it (under `$NABI_HOME/cache/`), else `json (<dir>)`, relative
/// to the current directory when they share more than the root.
fn shown(name: &str, dir: &Path) -> String {
    let real = |p: &Path| nabic_core::real_path(p).unwrap_or_else(|_| p.to_path_buf());
    let at = real(dir);
    let fetched = nabic_core::nabi_home().is_some_and(|h| at.starts_with(real(&h.join("cache"))));
    if let Some(v) = fetched.then(|| version(&at)).flatten() {
        return format!("{name} v{v}");
    }
    let rel = std::env::current_dir()
        .ok()
        .and_then(|c| relative(&real(&c), &at));
    format!("{name} ({})", rel.as_deref().unwrap_or(dir).display())
}

/// `to` from directory `from` (both real), with `..` steps; `None` when
/// they share no more than the root (another drive, `/tmp` from `/home`).
fn relative(from: &Path, to: &Path) -> Option<PathBuf> {
    if !to.is_absolute() {
        return Some(to.to_path_buf());
    }
    let f: Vec<_> = from.components().collect();
    let t: Vec<_> = to.components().collect();
    let common = f.iter().zip(&t).take_while(|(a, b)| a == b).count();
    let root = t
        .iter()
        .take_while(|c| !matches!(c, std::path::Component::Normal(_)))
        .count();
    if common <= root {
        return None;
    }
    let mut out: PathBuf = f[common..].iter().map(|_| "..").collect();
    out.extend(&t[common..]);
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    Some(out)
}

/// The `version` of the nabi.nabion in `dir`.
fn version(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join("nabi.nabion")).ok()?;
    let nabic_nabion::Kind::Struct { fields, .. } = nabic_nabion::parse(&text).ok()?.kind else {
        return None;
    };
    fields.into_iter().find_map(|f| match f.value.kind {
        nabic_nabion::Kind::Str(v) if f.name == "version" => Some(v),
        _ => None,
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn relative_dirs() {
        let rel =
            |a: &str, b: &str| relative(Path::new(a), Path::new(b)).map(PathBuf::into_os_string);
        assert_eq!(rel("/h/w/app", "/h/w/app/local"), Some("local".into()));
        assert_eq!(rel("/h/w/app", "/h/w/geom"), Some("../geom".into()));
        assert_eq!(rel("/h/w/app", "/h/w/app"), Some(".".into()));
        assert_eq!(rel("/h/w/app", "/tmp/geom"), None);
    }
}
