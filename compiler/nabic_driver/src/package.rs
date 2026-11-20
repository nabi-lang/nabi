//! Prebuilt and library packages, and the caches of their stages.

#[allow(clippy::wildcard_imports)]
use crate::*;

/// A prebuilt dependency of this build (`--extern name=x.nabihir`).
pub(crate) struct Prebuilt {
    pub(crate) nabihir: nabihir::Nabihir,
    pub(crate) archive: PathBuf,
}

/// Load the `.nabihir` externs into the overlay; `config` with each one's
/// root replaced by its (overlaid) `lib.nabi`.
pub(crate) fn load_prebuilt(
    config: &CompilerConfig,
    diags: &mut Vec<NabiDiagnostic>,
) -> (CompilerConfig, Vec<Prebuilt>) {
    let mut config = config.clone();
    let mut out: Vec<Prebuilt> = Vec::new();
    OVERLAY.with(|o| *o.borrow_mut() = config.buffers.iter().cloned().collect());
    READ.with(|r| r.borrow_mut().clear());
    // An alias's `.nabihir` and the `lib.nabi` it was overlaid as.
    let mut loaded: Vec<(PathBuf, PathBuf)> = Vec::new();
    for (name, root) in &mut config.externs {
        if root.extension().is_none_or(|e| e != "nabihir") {
            continue;
        }
        if let Some((_, lib)) = loaded.iter().find(|(r, _)| r == root) {
            *root = lib.clone();
            continue;
        }
        let mut r = match nabihir::Nabihir::load(root) {
            Ok(r) if r.name == *name => r,
            Ok(r) => {
                diags.push(NabiDiagnostic::error(
                    Code::DEPENDENCY,
                    nabic_diagnostic::Span::DUMMY,
                    format!(
                        "`--extern {name}={}`: it was built as `{}`, and the name is in its \
                         symbols - rebuild it as `{name}`",
                        root.display(),
                        r.name
                    ),
                ));
                continue;
            }
            Err(e) => {
                diags.push(NabiDiagnostic::error(
                    Code::DEPENDENCY,
                    nabic_diagnostic::Span::DUMMY,
                    e,
                ));
                continue;
            }
        };
        if let Some(p) = out
            .iter()
            .find(|p| r.type_offset < p.nabihir.type_end() && p.nabihir.type_offset < r.type_end())
        {
            diags.push(NabiDiagnostic::error(
                Code::DEPENDENCY,
                nabic_diagnostic::Span::DUMMY,
                format!(
                    "prebuilt `{}` and `{}` use the same GC type ids - build each dependency \
                     with the ones before it as `--extern` (`nabi build` does)",
                    p.nabihir.name, r.name
                ),
            ));
        }
        // Remapped to a relative path (`json/src`, a release build's):
        // overlaid at a path of its own, written back as the NABIHIR says.
        if r.src_dir.is_relative() {
            let at = std::path::absolute(root.with_extension("src"))
                .unwrap_or_else(|_| root.with_extension("src"));
            config
                .remap
                .push((at.clone(), r.src_dir.display().to_string()));
            r.src_dir = at;
        }
        OVERLAY.with(|o| {
            let mut o = o.borrow_mut();
            for (rel, text) in &r.files {
                o.insert(nabic_wit::normal(&r.src_dir.join(rel)), text.clone());
            }
        });
        let archive = nabihir::companions(root).1;
        let lib = r.src_dir.join("lib.nabi");
        loaded.push((root.clone(), lib.clone()));
        *root = lib;
        out.push(Prebuilt {
            nabihir: r,
            archive,
        });
    }
    (config, out)
}

/// A library root mounted beside the user package: its module path
/// prefix and root file.
pub(crate) type LibRoot = (Vec<String>, PathBuf);

/// The tier a bootstrap root names: `core` for `<libs>/core/src/lib.nabi`.
pub(crate) fn tier_of(root: &Path) -> Option<&str> {
    root.parent()?.parent()?.file_name()?.to_str()
}

/// The bootstrap roots `files` names, with `bare/src/lib.nabi` beside a
/// `core/src/lib.nabi` added when missing: core is built on bare.
#[must_use]
pub fn bootstrap_tiers(files: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = files.to_vec();
    if !files.iter().any(|f| tier_of(f) == Some("bare")) {
        out.extend(
            files
                .iter()
                .filter(|f| tier_of(f) == Some("core"))
                .filter_map(|c| Some(c.parent()?.parent()?.parent()?.join("bare/src/lib.nabi")))
                .filter(|b| b.is_file())
                .take(1),
        );
    }
    out
}

/// Each bootstrap file as its tier (`library/core/src/lib.nabi` ->
/// `core`), then each `--extern` dependency under its own name.
pub(crate) fn lib_roots(config: &CompilerConfig) -> Vec<LibRoot> {
    let mut out: Vec<LibRoot> = bootstrap_tiers(&config.bootstrap_files)
        .iter()
        .map(|root| {
            let tier = tier_of(root).unwrap_or("core").to_owned();
            (vec![tier], root.clone())
        })
        .collect();
    out.extend(mounted(&config.externs).map(|(name, root)| (vec![name.clone()], root.clone())));
    out
}

/// The externs mounted: the first name of each path.
pub(crate) fn mounted(externs: &[(String, PathBuf)]) -> impl Iterator<Item = &(String, PathBuf)> {
    externs
        .iter()
        .enumerate()
        .filter(|(i, (_, p))| !externs[..*i].iter().any(|(_, q)| q == p))
        .map(|(_, e)| e)
}

/// `(alias, name)`: another name of a path is an alias of its first
/// name; packages may call one dependency differently.
pub(crate) fn extern_aliases(externs: &[(String, PathBuf)]) -> Vec<(String, String)> {
    externs
        .iter()
        .enumerate()
        .filter_map(|(i, (alias, p))| {
            let (name, _) = externs[..i].iter().find(|(_, q)| q == p)?;
            (name != alias).then(|| (alias.clone(), name.clone()))
        })
        .collect()
}

/// The externs' aliases, applied to every meta block's template: they
/// expand where no parse renames their paths.
pub(crate) fn alias_metas(
    metas: &mut PrePass,
    externs: &[(String, PathBuf)],
) -> Vec<(String, String)> {
    let aliases = extern_aliases(externs);
    // A tier's name stays the library in every package but the one built.
    let others: Vec<(String, String)> = (aliases.iter())
        .filter(|(a, _)| !nabic_core::TIERS.contains(&a.as_str()))
        .cloned()
        .collect();
    for m in &mut metas.metas {
        let own = if m.package.is_empty() {
            &aliases
        } else {
            &others
        };
        apply_aliases(&mut m.body, own);
    }
    aliases
}

/// A path that starts with an alias (`alias::..`, not after `::` or `.`)
/// names the package it stands for.
pub(crate) fn apply_aliases(tokens: &mut [nabic_lexer::Token], aliases: &[(String, String)]) {
    if aliases.is_empty() {
        return;
    }
    for i in 0..tokens.len() {
        let TokenKind::Ident(name) = &tokens[i].kind else {
            continue;
        };
        let Some((_, to)) = aliases.iter().find(|(a, _)| a == name) else {
            continue;
        };
        let path = tokens
            .get(i + 1)
            .is_some_and(|t| t.kind == TokenKind::ColonColon);
        let inner = i > 0
            && matches!(
                tokens[i - 1].kind,
                TokenKind::ColonColon | TokenKind::Dot | TokenKind::QuestionDot
            );
        if path && !inner && !tokens[i].raw {
            tokens[i].kind = TokenKind::Ident(to.clone());
        }
    }
}

/// An item and the module path (`a::b`, `""` at the root) it is in.
pub(crate) type ModItem = (String, nabic_ast::Item);

pub(crate) type UseEntry = (String, nabic_core::Visibility, nabic_ast::UseTree);

/// One cached parse (`cached`): its key, the files it added, its
/// result.
pub(crate) type Cached<T> = std::sync::Mutex<Option<(String, Vec<(String, String)>, T)>>;

/// `use` trees by module.
pub(crate) type RawUses = Vec<(String, nabic_core::Visibility, nabic_ast::UseTree)>;

pub(crate) type LibParse = (Vec<ModItem>, RawUses, Vec<NabiDiagnostic>);

pub(crate) static PREPASS_CACHE: Cached<(PrecTable, Vec<NabiDiagnostic>, PrePass)> =
    std::sync::Mutex::new(None);

pub(crate) static LIB_CACHE: Cached<LibParse> = std::sync::Mutex::new(None);

/// `#[link]` libraries: name, static.
pub(crate) type NativeLibs = Vec<(String, bool)>;

/// What the library's module tree gave besides its items, kept with its
/// collected stage (`nabic_typeck::LibraryStage`).
#[derive(Clone)]
pub(crate) struct LibParts {
    pub(crate) uses: RawUses,
    pub(crate) diags: Vec<NabiDiagnostic>,
    pub(crate) native: NativeLibs,
    /// Its modules without the implicit import.
    pub(crate) no_implicit: Vec<String>,
    /// Each library package's hash (`package_hashes`).
    pub(crate) hashes: Vec<(String, String)>,
    /// What `#[cfg]` dropped from its items (`nabic_hir::cfg_dropped`).
    pub(crate) dropped: Vec<nabic_hir::CfgDropped>,
}

nabic_core::codec_struct!(LibParts {
    uses,
    diags,
    native,
    no_implicit,
    hashes,
    dropped
});

/// The parts of the library stage kept in this process, by key.
pub(crate) static LIB_PARTS: std::sync::Mutex<Option<(u64, LibParts)>> =
    std::sync::Mutex::new(None);

/// Library stage `k`, kept in process or read back (and kept). The compile
/// holds its copy, since a concurrent compile may replace the kept one.
pub(crate) fn kept_library(
    k: u64,
    incr: Option<&incr::Incr>,
    ids: &incr::Ids,
) -> Option<(LibParts, nabic_typeck::KeptLibrary)> {
    let lock = || {
        LIB_PARTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    };
    let here = lock()
        .as_ref()
        .filter(|(at, _)| *at == k)
        .map(|(_, p)| p.clone())
        .zip(nabic_typeck::kept_library(k));
    let kept = if let Some(kept) = here {
        kept
    } else {
        let (parts, stage) = nabic_diagnostic::time("hir library read", || {
            incr::library_get(k, ids, |d| {
                Some((
                    <LibParts as nabic_core::codec::Codec>::dec(d)?,
                    nabic_typeck::LibraryStage::dec(d)?,
                ))
            })
        })?;
        let stage = nabic_typeck::keep_library(k, stage);
        *lock() = Some((k, parts.clone()));
        (parts, stage)
    };
    if let Some(i) = incr {
        i.library_reused
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
    Some(kept)
}

/// Keep the library stage of key `k` HIR collected (with `parts`) for
/// the next compile: in this process, and in the store.
pub(crate) fn keep_library(k: u64, parts: LibParts, files: &SourceMap) {
    let ids = incr::Ids::of(files);
    nabic_diagnostic::time("hir library write", || {
        if let Some(s) = nabic_typeck::kept_library(k) {
            incr::library_put(k, &ids, |e| {
                nabic_core::codec::Codec::enc(&parts, e);
                nabic_typeck::stage(&s).enc(e)
            });
        }
    });
    *LIB_PARTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((k, parts));
}

/// Key of the library's collected stage: build flags, each library
/// package's parse tables, operators, every library file (id, path, text,
/// includes) and the source files under the library directories. Files of
/// no package (`<repl>`) are views of the user's text.
pub(crate) fn library_key(
    flags: &str,
    roots: &[LibRoot],
    user_dir: &Path,
    metas: &[MetaCtx],
    ext: &ExtOps,
    prec: &PrecTable,
    files: &SourceMap,
) -> u64 {
    let pkgs = package_roots(user_dir, roots);
    let mut h = incr::key(&[flags.as_bytes(), format!("{roots:?}").as_bytes()]);
    let mut add =
        |b: &[u8]| h = nabic_core::hash::fast_add(nabic_core::hash::fast_add(h, b), b"\0");
    for m in &metas[1..] {
        add(&m.tables_key(ext).to_le_bytes());
    }
    let mut ops: Vec<String> = prec.entries().map(|e| format!("{e:?}")).collect();
    ops.sort();
    add(ops.join(";").as_bytes());
    let lib_file = |p: &Path| package_of(&pkgs, p).is_some_and(|(i, _)| i != 0);
    for (i, (path, text)) in files.files().iter().enumerate() {
        if lib_file(Path::new(path)) {
            add(&i.to_le_bytes());
            add(path.as_bytes());
            add(text.as_bytes());
        }
    }
    let mut includes: Vec<(PathBuf, Vec<u8>)> = PREPASSED.with(|p| {
        p.borrow()
            .iter()
            .filter(|(path, _)| lib_file(path))
            .flat_map(|(path, (_, pre))| included(path, &pre.sum.includes))
            .collect()
    });
    includes.sort();
    for (p, data) in includes {
        add(p.to_string_lossy().as_bytes());
        add(&data);
    }
    for (_, root) in roots {
        let dir = root.parent().unwrap_or_else(|| Path::new("."));
        for p in source_listing(dir) {
            add(p.to_string_lossy().as_bytes());
        }
    }
    h
}

/// The source files and folders under `dir`, the overlay's included,
/// sorted.
pub(crate) fn source_listing(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = OVERLAY.with(|o| {
        o.borrow()
            .keys()
            .filter(|p| p.starts_with(dir))
            .cloned()
            .collect()
    });
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                todo.push(p.clone());
                out.push(p);
            } else if p.extension().is_some_and(|x| x == "nabi") {
                out.push(p);
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// `parse(files)`, or (`on`) a copy of its last result for the same `key`
/// and file count, its files re-added so spans keep their ids.
// One entry per parse; files aren't rechecked on disk (`cache_library`
// assumes a fixed library).
pub(crate) fn cached<T: Clone>(
    slot: &Cached<T>,
    on: bool,
    key: String,
    files: &mut SourceMap,
    parse: impl FnOnce(&mut SourceMap) -> T,
) -> T {
    if !on {
        return parse(files);
    }
    let key = format!("{}|{key}", files.len());
    let lock = || {
        slot.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    };
    if let Some((k, added, v)) = lock().as_ref() {
        if *k == key {
            for (path, text) in added {
                files.add(path.clone(), text.clone());
            }
            return v.clone();
        }
    }
    // Not under the lock: a parse that never returns (a tool's
    // compiler panic, `nabic_tools::analysis::isolate`) must not hold it.
    let before = files.len();
    let v = parse(files);
    let added = files.files()[before..].to_vec();
    *lock() = Some((key, added, v.clone()));
    v
}
