//! Builds by codegen units: a package's objects from its units
//! (`nabic_codegen::units`), each recompiled only when its key changed.
//!
//! A standard library unit is kept in the function cache (`fncache`) for
//! every build of the toolchain, the package's own in its incremental
//! directory:
//!
//! - `stamp`: compiler and options the rest is for (a change empties it);
//! - `<key>.o`: a unit's object.
//!
//! Units use a placeholder for the package hash, patched before the link.
//! Unused entries are deleted.

use crate::{BuildOptions, CompiledModule, incr};
use nabic_codegen::units::{self, Plan, PlanOptions};
use nabic_diagnostic::{Code, NabiDiagnostic, SourceMap};
use nabic_ir_common::Summary;
use std::path::{Path, PathBuf};

/// What units a build compiled, and how many it reused.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct UnitCounts {
    /// The names of the units compiled (their home modules).
    pub compiled: Vec<String>,
    pub reused: usize,
}

/// Whether `module` builds by units: a program or library with an
/// incremental directory, or a program with the function cache; not a
/// REPL cell. A dependency's build makes one object.
pub fn applies(module: &CompiledModule, opts: &BuildOptions) -> bool {
    let program = module.mir.link.lib.is_none();
    // A plugin's or host's descriptor holds the real package hashes.
    opts.repl.is_none()
        && module.mir.plugin.mode == nabic_mir::plugin::Mode::None
        && (opts.incremental.is_some() || (program && crate::fncache::shared().is_some()))
}

/// What `objects` built: the objects, the units compiled, how many GC
/// types there are, and a library's summaries (`Nabihir::facts`).
pub type Objects = (Vec<Vec<u8>>, UnitCounts, u32, Vec<(String, Summary)>);

/// The objects of `module` (`applies`), from the function cache and the
/// incremental directory `dir` where its units are unchanged, and how many
/// GC types it has.
pub fn objects(
    module: &CompiledModule,
    sources: &SourceMap,
    opts: &BuildOptions,
    dir: Option<&Path>,
) -> Result<Objects, NabiDiagnostic> {
    let salt = salt(opts, &module.mir.target.spec);
    if let Some(dir) = dir {
        open(dir, &salt)?;
    }
    let memo = Memo {
        store: dir.map(|dir| incr::Store::open(dir.join("opt.nabiopt"), OPT)),
        shared: crate::fncache::shared(),
        ids: incr::Ids::of(sources),
    };
    let optimized = crate::codegen::mir_with(module, opts, Some(&memo));
    let mir = &*optimized;
    if incr::verify().is_some() {
        verify(&crate::codegen::mir(module, opts), mir)?;
    }
    if let Some(why) = nabic_codegen_native::unsupported(mir) {
        return Err(err(Code::UNSUPPORTED, why));
    }
    let plan = nabic_diagnostic::time("units", || {
        units::plan(
            mir,
            &PlanOptions {
                salt: &salt,
                debug: opts.debug,
                sources,
            },
        )
    });
    let (objects, counts, keep) =
        nabic_diagnostic::time("native", || by_native(mir, sources, opts, &plan, dir))?;
    if let Some(dir) = dir {
        prune(dir, &keep);
    }
    if let Some(s) = &memo.store {
        s.save(OPT);
    }
    // The function cache's new entries may pass the cap.
    crate::cache::settle();
    let real = mir
        .link
        .packages
        .iter()
        .find(|(p, _)| p == units::own_package(&mir.link))
        .map_or("", |(_, h)| h.as_str());
    let objects = nabic_diagnostic::time("rehash", || {
        objects
            .into_iter()
            .map(|o| rehash(o, plan.placeholder(), real))
            .collect()
    });
    let facts = if module.interface.is_some() {
        nabic_mir::summary::export(mir, &plan.core.quiet)
    } else {
        Vec::new()
    };
    Ok((objects, counts, plan.gc_tables.len() as u32, facts))
}

/// Magic of the store of optimized functions.
const OPT: &str = "NABIOPT";

/// The optimized functions of earlier builds (`nabic_mir::Memo`): the
/// standard library's in the function cache, the package's in its store.
struct Memo {
    store: Option<incr::Store>,
    shared: Option<&'static crate::fncache::FnCache>,
    ids: incr::Ids,
}

impl nabic_mir::Memo for Memo {
    fn get(&self, key: u64, shared: bool) -> Option<Vec<u8>> {
        match self.shared.filter(|_| shared) {
            Some(c) => c.get(crate::fncache::Kind::Opt, &format!("{key:016x}")),
            None => self
                .store
                .as_ref()?
                .get_with(key, &self.ids, |d| d.bytes().map(<[u8]>::to_vec)),
        }
    }

    fn put(&self, key: u64, shared: bool, value: &[u8]) {
        match (self.shared.filter(|_| shared), &self.store) {
            (Some(c), _) => c.put(crate::fncache::Kind::Opt, &format!("{key:016x}"), value),
            (None, Some(s)) => s.put_with(key, &self.ids, |e| {
                e.bytes(value);
                true
            }),
            (None, None) => {}
        }
    }

    fn path(&self, file: u32) -> Option<&str> {
        self.ids.path(file)
    }

    fn file(&self, path: &str) -> Option<u32> {
        self.ids.id(path)
    }
}

/// `NABI_INCREMENTAL_VERIFY=1`: the functions optimized with the
/// store are those of a clean optimization.
fn verify(
    clean: &nabic_mir::MirModule,
    cached: &nabic_mir::MirModule,
) -> Result<(), NabiDiagnostic> {
    let differ = clean
        .functions
        .iter()
        .zip(&cached.functions)
        .find(|(a, b)| format!("{a:?}") != format!("{b:?}"))
        .map(|(a, _)| a.name.clone())
        .or_else(|| {
            (clean.functions.len() != cached.functions.len())
                .then(|| format!("{} functions", cached.functions.len()))
        });
    match differ {
        Some(what) => Err(err(
            Code::INTERNAL,
            format!(
                "NABI_INCREMENTAL_VERIFY: the optimized MIR from the store differs from a clean one at `{what}`"
            ),
        )),
        None => Ok(()),
    }
}

/// What units' entries are for: compiler, target, code-changing options;
/// under `-g` also the base directory of source paths and the path rules
/// (debug info is absolute or remapped, unit keys hold their own spans'
/// paths).
fn salt(opts: &BuildOptions, t: &nabic_mir::TargetSpec) -> String {
    let mut s = format!(
        "{}\nopt {}\noverflow {:?}\ndebug {}\ntarget {:016x}\n",
        crate::nabihir::producer(),
        opts.opt_level,
        opts.overflow_checks,
        opts.debug,
        t.hash(),
    );
    if opts.debug {
        let cwd = std::env::current_dir().unwrap_or_default();
        s.push_str(&format!("{}\n{:?}\n", cwd.display(), opts.remap));
    }
    s
}

/// `dir`, emptied when its stamp is not `salt`'s.
fn open(dir: &Path, salt: &str) -> Result<(), NabiDiagnostic> {
    let stamp = dir.join("stamp");
    if std::fs::read_to_string(&stamp).ok().as_deref() == Some(salt) {
        return Ok(());
    }
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir)
        .map_err(|e| err(Code::IO, format!("cannot create {}: {e}", dir.display())))?;
    write(&stamp, salt.as_bytes())
}

/// `path` holds `bytes`, or what it held: written beside it, then
/// renamed over it.
fn write(path: &Path, bytes: &[u8]) -> Result<(), NabiDiagnostic> {
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    let put = || std::fs::write(&tmp, bytes).and_then(|()| std::fs::rename(&tmp, path));
    // Another build may have emptied the directory meanwhile.
    put()
        .or_else(|e| match path.parent() {
            Some(dir) if e.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir_all(dir).and_then(|()| put())
            }
            _ => Err(e),
        })
        .map_err(|e| err(Code::IO, format!("cannot write {}: {e}", path.display())))
}

/// Deletes the unit entries of `dir` other than `keep`, and scratch
/// files left for a minute.
fn prune(dir: &Path, keep: &[PathBuf]) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        // Another build of this directory may be writing a scratch file.
        let stale = || {
            e.metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age.as_secs() > 60)
        };
        let unit = p
            .extension()
            .is_some_and(|x| x == "o" || x.to_string_lossy().starts_with("tmp") && stale());
        if unit && !keep.contains(&p) {
            let _ = std::fs::remove_file(&p);
        }
    }
}

/// `object` with the placeholder package hash in its symbols replaced by
/// the real one (the same length: `_Y<8 hex>_`).
fn rehash(mut object: Vec<u8>, placeholder: &str, real: &str) -> Vec<u8> {
    let from = format!("_Y{placeholder}_");
    let to = format!("_Y{real}_");
    if from == to || from.len() != to.len() {
        return object;
    }
    let (from, to) = (from.as_bytes(), to.as_bytes());
    let mut i = 0;
    while let Some(at) = object[i..].windows(from.len()).position(|w| w == from) {
        let at = i + at;
        object[at..at + to.len()].copy_from_slice(to);
        i = at + to.len();
    }
    object
}

/// Runs `job` for each index below `n` on a thread per core (each with
/// a deep stack, as the compile thread has).
fn in_parallel<T: Send>(
    n: usize,
    job: impl Fn(usize) -> Result<T, NabiDiagnostic> + Sync,
) -> Result<Vec<T>, NabiDiagnostic> {
    crate::on_cores(n, 256 << 20, job)
        .map_err(|e| {
            err(
                Code::INTERNAL,
                format!("cannot start a codegen thread: {e}"),
            )
        })?
        .into_iter()
        .map(|r| {
            r.unwrap_or_else(|| Err(err(Code::INTERNAL, "a codegen thread failed".to_owned())))
        })
        .collect()
}

type Built = (Vec<Vec<u8>>, UnitCounts, Vec<PathBuf>);

/// Objects by the native backend; a unit's object is its entry, in the
/// function cache for a standard library unit, else in `dir`.
fn by_native(
    mir: &nabic_mir::MirModule,
    sources: &SourceMap,
    opts: &BuildOptions,
    plan: &Plan,
    dir: Option<&Path>,
) -> Result<Built, NabiDiagnostic> {
    let nopts = crate::codegen::options(opts);
    let shared = crate::fncache::shared();
    let cached = |u: &units::Unit| shared.filter(|_| u.std);
    let paths: Vec<Option<PathBuf>> = plan
        .units
        .iter()
        .map(|u| {
            dir.filter(|_| cached(u).is_none())
                .map(|d| d.join(format!("{}.o", u.key)))
        })
        .collect();
    let objects = in_parallel(plan.units.len(), |i| {
        let u = &plan.units[i];
        let fresh = match (cached(u), &paths[i]) {
            (Some(c), _) => c.get(crate::fncache::Kind::Object, &u.key),
            (None, Some(p)) => std::fs::read(p).ok(),
            (None, None) => None,
        };
        if let Some(o) = fresh {
            return Ok((o, false));
        }
        let o = nabic_codegen_native::compile_unit(mir, &nopts, sources, plan, i)?.object;
        match (cached(u), &paths[i]) {
            (Some(c), _) => c.put(crate::fncache::Kind::Object, &u.key, &o),
            (None, Some(p)) => write(p, &o)?,
            (None, None) => {}
        }
        Ok((o, true))
    })?;
    Ok(finish_counts(
        plan,
        objects,
        paths.into_iter().flatten().collect(),
    ))
}

fn finish_counts(plan: &Plan, objects: Vec<(Vec<u8>, bool)>, keep: Vec<PathBuf>) -> Built {
    let compiled: Vec<String> = objects
        .iter()
        .zip(&plan.units)
        .filter(|((_, c), _)| *c)
        .map(|(_, u)| u.name.clone())
        .collect();
    let counts = UnitCounts {
        reused: objects.len() - compiled.len(),
        compiled,
    };
    (objects.into_iter().map(|(o, _)| o).collect(), counts, keep)
}

fn err(code: Code, msg: String) -> NabiDiagnostic {
    NabiDiagnostic::error(code, nabic_diagnostic::Span::DUMMY, msg)
}
