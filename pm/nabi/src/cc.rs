//! The C build step: a package's `c.sources` compiled by nabicc, Nabi's
//! own C compiler, into objects nabic links in (`--native-object`).
//! Nothing runs a system C compiler. Only sources whose inputs changed
//! compile again.

use crate::manifest::CSources;
use nabic_target::TargetSpec;
use std::path::{Path, PathBuf};
use std::process::Command;

/// What one package's C build needs.
pub struct Unit<'a> {
    /// The package (the error names it).
    pub package: &'a str,
    /// The manifest's directory: `c` paths are relative to it.
    pub dir: &'a Path,
    pub c: &'a CSources,
    /// Header directories of its `Dep::system` libraries, after `c.include`.
    pub include: &'a [PathBuf],
    pub target: &'a TargetSpec,
    /// `-O<level>` of the build (`0`..`3`, `s`).
    pub opt: &'a str,
    /// Where its objects go (`target/<profile>/c/<package>/`).
    pub out: &'a Path,
    pub quiet: bool,
}

/// Whether `obj` was built with `key` and is newer than every file its
/// make rule `dep` lists.
fn fresh(obj: &Path, dep: &Path, stamp: &Path, key: &str) -> bool {
    let Ok(built) = std::fs::metadata(obj).and_then(|m| m.modified()) else {
        return false;
    };
    if std::fs::read_to_string(stamp).ok().as_deref() != Some(key) {
        return false;
    }
    let Ok(rule) = std::fs::read_to_string(dep) else {
        return false;
    };
    let rule = rule.replace("\\\n", " ");
    let Some((_, inputs)) = rule.split_once(':') else {
        return false;
    };
    inputs.split_whitespace().all(|f| {
        std::fs::metadata(f)
            .and_then(|m| m.modified())
            .is_ok_and(|t| t <= built)
    })
}

/// The objects of `u`'s C sources, compiled when changed.
pub fn compile(u: &Unit<'_>) -> Result<Vec<PathBuf>, String> {
    let c = u.c;
    std::fs::create_dir_all(u.out).map_err(|e| format!("{}: {e}", u.out.display()))?;
    let nabicc = crate::toolchain::tool_for("nabicc")?;
    let mut args: Vec<String> = vec![
        "-c".to_owned(),
        format!("--target={}", u.target.name),
        format!("-O{}", u.opt),
    ];
    if u.opt == "0" {
        args.push("-g".to_owned());
    }
    if let Some(s) = &c.std {
        args.push(format!("-std={s}"));
    }
    for i in &c.include {
        args.push(format!("-I{}", u.dir.join(i).display()));
    }
    for i in u.include {
        args.push(format!("-I{}", i.display()));
    }
    for (k, v) in &c.define {
        args.push(format!("-D{k}={v}"));
    }
    args.extend(c.flags.iter().cloned());
    let key = args.join("\n");
    let mut objects = Vec::new();
    let mut jobs = Vec::new();
    for s in &c.sources {
        let src = u.dir.join(s);
        if !src.is_file() {
            return Err(format!(
                "{}: no such C source (named in `c.sources` of package `{}`)",
                src.display(),
                u.package
            ));
        }
        let stem: String = s
            .chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() || ch == '.' {
                    ch
                } else {
                    '_'
                }
            })
            .collect();
        let obj = u.out.join(format!("{stem}.o"));
        let dep = u.out.join(format!("{stem}.d"));
        let stamp = u.out.join(format!("{stem}.flags"));
        if !fresh(&obj, &dep, &stamp, &key) {
            jobs.push((src, obj.clone(), dep, stamp));
        }
        objects.push(obj);
    }
    if !jobs.is_empty() && !u.quiet {
        eprintln!("   Compiling {} C source(s) of {}", jobs.len(), u.package);
    }
    // Side by side, one nabicc each.
    let children: Vec<_> = jobs
        .iter()
        .map(|(src, obj, dep, _)| {
            Command::new(&nabicc)
                .args(&args)
                .arg(src)
                .arg("-o")
                .arg(obj)
                .arg("-MD")
                .arg("-MF")
                .arg(dep)
                .env("__NABI_CLI", "nabi")
                .spawn()
                .map_err(|e| format!("cannot run {}: {e}", nabicc.display()))
        })
        .collect::<Result<_, _>>()?;
    let mut failed = false;
    for (mut ch, (_, _, _, stamp)) in children.into_iter().zip(&jobs) {
        let ok = ch.wait().is_ok_and(|s| s.success());
        if ok {
            std::fs::write(stamp, &key).map_err(|e| format!("{}: {e}", stamp.display()))?;
        }
        failed |= !ok;
    }
    if failed {
        return Err(format!(
            "the C sources of package `{}` did not compile",
            u.package
        ));
    }
    Ok(objects)
}
