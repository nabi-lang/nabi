//! Compiling a package: front end, MIR and codegen units.

#[allow(clippy::wildcard_imports)]
use crate::*;

thread_local! {
    /// Keep a failed check's HIR for `take_partial` (an editor's hover).
    static KEEP_PARTIAL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static PARTIAL: std::cell::RefCell<Option<(HirModule, Vec<SourceFile>)>> =
        const { std::cell::RefCell::new(None) };
}

/// On this thread, a compile that fails type checking keeps what it
/// lowered (expressions with errors untyped) for `take_partial`.
pub fn keep_partial(on: bool) {
    KEEP_PARTIAL.set(on);
    if on {
        PARTIAL.take();
    }
}

fn keep_failed(hir: HirModule, sources: Vec<SourceFile>) {
    if KEEP_PARTIAL.get() {
        PARTIAL.replace(Some((hir, sources)));
    }
}

/// The HIR and sources of this thread's last compile that failed type
/// checking, under `keep_partial`.
pub fn take_partial() -> Option<(HirModule, Vec<SourceFile>)> {
    PARTIAL.take()
}

/// Compile the package rooted at `path`; `files` gets every source file
/// read, by the ids the diagnostics' spans carry.
pub fn compile_file(
    path: &Path,
    config: &CompilerConfig,
    files: &mut SourceMap,
) -> Result<CompiledProject, Vec<NabiDiagnostic>> {
    let source = std::fs::read_to_string(path).map_err(|e| {
        vec![NabiDiagnostic::error(
            Code::IO,
            nabic_diagnostic::Span::DUMMY,
            format!("cannot read {}: {e}", path.display()),
        )]
    })?;
    compile_source(&source, path, config, files)
}

/// Where the standard library's sources show in what a build writes:
/// the same on every machine and install.
pub const STD_SHOWN: &str = "/nabi/library";

/// The directory holding the bootstrap tiers (`<dir>/core/src/lib.nabi`).
#[must_use]
pub fn std_dir(config: &CompilerConfig) -> Option<PathBuf> {
    let root = config.bootstrap_files.first()?;
    Some(root.parent()?.parent()?.parent()?.to_path_buf())
}

/// The build's path rules: the standard library's, then `config.remap`
/// (later rules win).
#[must_use]
pub fn remap_rules(config: &CompilerConfig) -> Vec<(PathBuf, String)> {
    let std = std_dir(config).map(|d| {
        let d = std::path::absolute(&d).unwrap_or(d);
        (d, STD_SHOWN.to_owned())
    });
    std.into_iter()
        .chain(config.remap.iter().cloned())
        .collect()
}

pub fn compile_source(
    source: &str,
    path: &Path,
    config: &CompilerConfig,
    files: &mut SourceMap,
) -> Result<CompiledProject, Vec<NabiDiagnostic>> {
    // Tests of the tools' survival: a debug nabic panics on a root file
    // containing `NABI_TEST_ICE`.
    if cfg!(debug_assertions) {
        if let Some(m) = std::env::var("NABI_TEST_ICE")
            .ok()
            .filter(|m| !m.is_empty())
        {
            assert!(!source.contains(&m), "NABI_TEST_ICE");
        }
    }
    let mut diags: Vec<NabiDiagnostic> = Vec::new();
    let (mut config, prebuilt) = load_prebuilt(config, &mut diags);
    if !diags.is_empty() {
        return Err(diags);
    }
    // The build's path rules, a relative NABIHIR's last.
    files.set_remap(remap_rules(&config));
    let archives: Vec<PathBuf> = prebuilt.iter().rev().map(|p| p.archive.clone()).collect();
    let Some(lib) = config.lib_name.take() else {
        let r = compile_unit(source, path, &config, &prebuilt, None, files);
        OVERLAY.with(|o| o.borrow_mut().clear());
        let mut p = r?;
        if let Some(m) = p.modules.first_mut() {
            m.archives = archives;
            m.facts = prebuilt_facts(&m.mir, &prebuilt);
        }
        return Ok(p);
    };
    // The library is a dependency `lib` of an empty package (whose own
    // hash is unique per library: closures etc. are named in it).
    let src_dir = path.parent().unwrap_or_else(|| Path::new(""));
    let dir = std::path::absolute(src_dir).unwrap_or_default();
    let keep_ast = config.keep_ast;
    config.keep_ast = true;
    config.externs.push((lib.clone(), path.to_path_buf()));
    let root = dir.join(format!("{lib}.prebuilt")).join("lib.nabi");
    let stub = format!("// prebuilt library `{lib}`\n");
    let r = compile_unit(&stub, &root, &config, &prebuilt, Some(&lib), files);
    OVERLAY.with(|o| o.borrow_mut().clear());
    let mut p = r?;
    let seeds = p
        .modules
        .first()
        .map(|m| m.hir.any_seeds.clone())
        .unwrap_or_default();
    let mut interface = nabihir::interface(&p.sources, files, &lib, src_dir, &seeds);
    // Its WIT, so a dependent needs no `.wit` file on disk.
    if let Some(w) = p.modules.first().and_then(|m| m.mir.link.wit.as_ref()) {
        interface.extend(nabihir::wit_files(w, &lib, &dir));
    }
    if !keep_ast {
        p.sources.clear();
    }
    if let Some(m) = p.modules.first_mut() {
        m.archives = archives;
        m.facts = prebuilt_facts(&m.mir, &prebuilt);
        let hash = m
            .mir
            .link
            .packages
            .iter()
            .find(|(n, _)| *n == lib)
            .map(|(_, h)| h.clone())
            .unwrap_or_default();
        // Its copies of other packages' code are not its interface. A
        // `#<kind>\u{1f}<function>` key is about that function.
        let subject = |f: &str| {
            f.strip_prefix('#')
                .and_then(|r| r.split_once('\u{1f}'))
                .map_or(f, |(_, n)| n)
                .to_owned()
        };
        let summaries = (m.hir.summaries.iter())
            .filter(|(f, _)| nabic_mir::Link::owns(&lib, &subject(f)))
            .cloned()
            .collect();
        m.interface = Some(nabihir::Nabihir {
            producer: nabihir::producer(),
            hash,
            name: lib,
            src_dir: shown_dir(files, &dir),
            type_offset: m.mir.link.type_offset,
            type_count: 0,
            features: config
                .cfg_flags
                .iter()
                .filter_map(|f| cfg::feature_of(f).map(str::to_owned))
                .collect(),
            deps: config.externs.iter().map(|(n, _)| n.clone()).collect(),
            files: interface,
            summaries,
            facts: Vec::new(),
        });
    }
    Ok(p)
}

/// `dir` as the build's path rules show it: a dependent overlays the
/// NABIHIR's files there, so its own locations in them agree.
fn shown_dir(files: &SourceMap, dir: &Path) -> PathBuf {
    files
        .remapped(&dir.display().to_string())
        .map_or_else(|| dir.to_path_buf(), PathBuf::from)
}

/// The summaries `prebuilt` hold of the functions `m` declares from
/// `HirInput::apple`: whether `t` is iOS and its deployment target, on
/// Apple targets.
fn apple_deployment(t: &nabic_mir::Target) -> Option<(bool, (u32, u32, u32))> {
    if !matches!(t.os.as_str(), "macos" | "ios") {
        return None;
    }
    let mut v = t
        .deployment_target()
        .split('.')
        .map(|p| p.parse::<u32>().unwrap_or(0));
    let version = (
        v.next().unwrap_or(11),
        v.next().unwrap_or(0),
        v.next().unwrap_or(0),
    );
    Some((t.os == "ios", version))
}

/// them.
pub(crate) fn prebuilt_facts(
    m: &MirModule,
    prebuilt: &[Prebuilt],
) -> Vec<(FunctionId, nabic_ir_common::Summary)> {
    if prebuilt.iter().all(|p| p.nabihir.facts.is_empty()) {
        return Vec::new();
    }
    m.functions
        .iter()
        .filter(|f| f.facts.linkage == Linkage::Prebuilt)
        .filter_map(|f| {
            let s = prebuilt.iter().find_map(|p| p.nabihir.fact(&f.name))?;
            Some((f.id, s))
        })
        .collect()
}

pub(crate) fn compile_unit(
    source: &str,
    path: &Path,
    config: &CompilerConfig,
    prebuilt: &[Prebuilt],
    lib: Option<&str>,
    files: &mut SourceMap,
) -> Result<CompiledProject, Vec<NabiDiagnostic>> {
    // One store per kind of build of the package (a check, a build, a
    // test build ..), so that alternating them keeps each one's.
    // Locations are in lowered bodies: path rules key the store too.
    let flavor = format!(
        "{}|{}|{}|{:?}|{:?}|{:?}",
        incr::target_key(&cfg_env(config), &config.target),
        config.check_library,
        config.keep_ast,
        lib,
        prebuilt.iter().map(|p| &p.nabihir.name).collect::<Vec<_>>(),
        config.remap,
    );
    let dir = config
        .incremental
        .as_ref()
        .map(|d| d.join(format!("{:016x}", incr::key(&[flavor.as_bytes()]))));
    let (r, reuse) = incr::with(
        dir.as_deref(),
        || compile_unit_in(source, path, config, prebuilt, lib, files),
        Result::is_ok,
    );
    r.map(|mut p| {
        p.reuse = reuse;
        p
    })
}

pub(crate) fn compile_unit_in(
    source: &str,
    path: &Path,
    config: &CompilerConfig,
    prebuilt: &[Prebuilt],
    lib: Option<&str>,
    files: &mut SourceMap,
) -> Result<CompiledProject, Vec<NabiDiagnostic>> {
    let mut diags: Vec<NabiDiagnostic> = Vec::new();

    // Bootstrap the PrecTable from operator declarations; the same pre-pass
    // gathers every meta block before derives expand.
    let roots = lib_roots(config);
    // Cache the library's parses across builds (REPL) unless prebuilt deps
    // (sources are an overlay) or kept ASTs are involved.
    let cache = config.cache_library && prebuilt.is_empty() && !config.keep_ast && lib.is_none();
    let (mut prec, mut d, mut metas) = nabic_diagnostic::time("prepass library", || {
        cached(
            &PREPASS_CACHE,
            cache,
            format!("{roots:?}"),
            files,
            |files| {
                let (mut diags, mut metas) = (Vec::new(), PrePass::default());
                let prec = build_prec_table(&roots, &mut diags, &mut metas, files);
                (prec, diags, metas)
            },
        )
    });
    diags.append(&mut d);
    let lib_metas = metas.metas.len();
    // The user package's own `#[infix]` registrations are
    // gathered by the same pre-pass, so its extension tokens lex.
    metas.idents = Some(Vec::new());
    let mut user = Vec::new();
    let sub_dir = path.parent();
    let mut pre = prepass_parse(
        vec![(
            path.to_path_buf(),
            Some(source.to_owned()),
            sub_dir.map(Path::to_path_buf),
        )],
        files,
        incr::current().as_deref().map(|i| &i.parse),
    );
    prepass_tree(path, sub_dir, &[], &mut pre, &mut metas, &mut user);
    for (sub, mut d) in user {
        diags.append(&mut d);
        diags.extend(sub.entries().cloned().filter_map(|e| prec.merge(e)));
    }
    diags.extend(cross_file_confusables(
        metas.idents.take().unwrap_or_default(),
    ));
    for d in &mut metas.metas[lib_metas..] {
        d.set_package("");
    }
    let deps: Vec<String> = config.externs.iter().map(|(n, _)| n.clone()).collect();
    let templates = template_refs(&metas.metas[lib_metas..], &deps);
    let user_metas = format!("{:?}", &metas.metas[lib_metas..]);
    let ext = ext_ops(&prec, config, prebuilt, lib);
    relex_metas(&mut metas, &ext);
    let scopes = scopes(config);
    alias_metas(&mut metas, &config.externs, scopes.as_deref());
    let cfg_env = cfg_env(config);
    // Per package: the user's, then each library root's.
    let cfgs: Vec<CfgEnv> = std::iter::once(cfg_env.clone())
        .chain(roots.iter().map(|(prefix, _)| {
            let p = prefix.join("::");
            // The library being built has the build's features; a
            // prebuilt one those it was built with; others none.
            if lib == Some(p.as_str()) {
                cfg_env.clone()
            } else {
                let built = prebuilt.iter().find(|b| b.nabihir.name == p);
                package_cfg(config, Some(built.map_or(&[], |b| &b.nabihir.features)))
            }
        }))
        .collect();
    let root_dir_kw = path
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let pkgs = package_roots(&root_dir_kw, &roots);
    // A meta block or keyword handler is judged by the cfg of the
    // package that defines it (a false one registers nothing).
    let holds = |file: &str, attrs: &[nabic_ast::Attribute]| {
        package_of(&pkgs, Path::new(file)).is_some_and(|(i, _)| cfg::attrs_pass(attrs, &cfgs[i]))
    };
    let mut meta_table = MetaTable::default();
    diags.extend(
        metas
            .metas
            .into_iter()
            .filter(|m| holds(&m.file, &m.cfg))
            .filter_map(|m| meta_table.add(m)),
    );
    let meta_table = std::sync::Arc::new(meta_table);
    let kw_defs = metas
        .keywords
        .into_iter()
        .filter(|d| holds(&d.file, &d.cfg))
        .collect();
    let keywords = keyword_tables(kw_defs, path, &root_dir_kw, &roots, &mut diags);
    let lits = literal_tables(
        metas.literals,
        path,
        &root_dir_kw,
        &roots,
        &deps,
        lib,
        &ext,
        &mut diags,
    );
    // Per package: the user's, then each library root's.
    let metas: Vec<MetaCtx> = (0..=roots.len())
        .map(|i| {
            let package = i
                .checked_sub(1)
                .map_or_else(String::new, |r| roots[r].0.join("::"));
            let aliases = package_aliases(&config.externs, scopes.as_deref(), &package);
            let deps = package_deps(&config.externs, scopes.as_deref(), &package);
            MetaCtx {
                package,
                table: std::sync::Arc::clone(&meta_table),
                keywords: keywords.get(i).cloned().unwrap_or_default(),
                literals: std::sync::Arc::clone(&lits[i]),
                key: std::sync::OnceLock::new(),
                target: config.target.clone(),
                cfg: cfgs[i].clone(),
                aliases: std::sync::Arc::new(aliases),
                deps: std::sync::Arc::new(deps),
            }
        })
        .collect();
    let incr = incr::current();
    let ids = incr::Ids::of(files);
    // The library's collected stage, kept or read back, spares parsing the
    // library's files (except under `NABI_INCREMENTAL_VERIFY`).
    let verify = incr::verify().is_some();
    let flags = format!(
        "{:?} {:?} {lib:?} {:?} {} {} {} {} {:?} {} {}",
        config.externs,
        config.extern_deps,
        prebuilt.iter().map(|p| &p.nabihir.name).collect::<Vec<_>>(),
        config.check_library,
        config.target.pointer_width,
        config.target.stdcall_system(),
        incr::target_key(&cfg_env, &config.target),
        config.cfg_flags,
        config.test,
        config.bench,
    );
    let stage_key = ((!config.keep_ast || config.library_items) && lib.is_none())
        .then(|| library_key(&flags, &roots, &root_dir_kw, &metas, &ext, &prec, files));
    let (kept_parts, kept_stage) = stage_key
        .and_then(|k| kept_library(k, incr.as_deref(), &ids))
        .unzip();
    if kept_parts.is_some() && !verify {
        let pkgs = package_roots(&root_dir_kw, &roots);
        PREPASSED.with(|p| {
            p.borrow_mut().retain(|path, _| {
                let user = package_of(&pkgs, path).is_some_and(|(i, _)| i == 0);
                if !user {
                    if let Some(i) = &incr {
                        i.count_parse(true);
                    }
                }
                user
            });
        });
    }
    nabic_diagnostic::time("parse", || {
        real_parse(
            incr.as_deref().map(|i| (i, &ids)),
            path,
            &root_dir_kw,
            &roots,
            &metas,
            &ext,
            config.keep_ast,
        );
    });
    // Walk the package's module tree from `main.nabi` / `lib.nabi` and merge
    // every file's items into one flat `Program`.
    let root_dir = path
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    // A script's file submodules are beside it, as in a
    // package's `src/`; stdin and a REPL session have none.
    let script_file = config.anonymous && !config.cache_library && path.is_file();
    let sub_dir = (!config.anonymous || script_file).then_some(root_dir.as_path());
    let mut sources = Vec::new();

    let mut all_items: Vec<ModItem> = Vec::new();
    let mut all_uses: Vec<(String, nabic_core::Visibility, nabic_ast::UseTree)> = Vec::new();
    NO_IMPLICIT.with(|n| n.borrow_mut().clear());
    let top_stmts = nabic_diagnostic::time("parse package", || {
        collect_module_tree(
            path,
            Some(source),
            sub_dir,
            &[],
            &ext,
            &metas[0],
            &mut diags,
            &mut all_items,
            &mut all_uses,
            config.keep_ast.then_some(&mut sources),
            files,
        )
    });
    let is_lib = !config.anonymous && path.file_name().is_some_and(|n| n == "lib.nabi");
    let mut tests = Vec::new();
    let mut root_main = nabic_typeck::RootMain::Declared;
    // An entry file with neither `main` nor statements.
    let mut no_entry = false;
    if config.test || config.bench {
        // Mount tests/ (test build) or benches/ (bench build), beside src/
        // at the package root, as a synthetic root module so their #[test] /
        // #[bench] functions are collected. Its synthesized body declares
        // `mod <file>;` per top-level .nabi; the usual machinery resolves
        // folder forms and submodules.
        if let Some(pkg_root) = root_dir.parent().filter(|_| !config.anonymous) {
            collect_extra_dir(
                pkg_root,
                config.bench,
                &ext,
                &metas[0],
                &mut diags,
                &mut all_items,
                &mut all_uses,
                files,
            );
        }
        // Top-level statements are the package's own entry: dropped.
        tests = harness::install(&mut all_items, &ext, &mut diags, config.bench);
        root_main = nabic_typeck::RootMain::Harness;
    } else if !is_lib {
        let has_main = all_items
            .iter()
            .any(|(m, i)| m.is_empty() && matches!(i, nabic_ast::Item::Fn(f) if f.name == "main"));
        no_entry = top_stmts.is_empty() && !has_main;
        if entry::resolve_entry(top_stmts, &mut all_items, &mut diags) {
            root_main = nabic_typeck::RootMain::Implicit;
        }
    } else if !top_stmts.is_empty() {
        diags.push(entry::stmts_outside_entry(path));
    }
    check_dependency_names(&all_items, &all_uses, &config.externs, &mut diags);
    // Mount each library root (its tier, then dependencies); HIR gives
    // every module its implicit import.
    let key = format!(
        "{roots:?}|{}|{}|{}|{ext:?}|{user_metas}|{keywords:?}",
        incr::target_key(&cfg_env, &config.target),
        config.test,
        config.bench
    );
    let parse_libs = nabic_diagnostic::time_passes().then(std::time::Instant::now);
    // A kept library stage holds what HIR collected of the library's
    // items: they are not needed again.
    let mut library = stage_key;
    let kept = kept_parts.is_some();
    let lib_no_implicit = NO_IMPLICIT.with(|n| n.borrow().len());
    let files_before = files.len();
    let (mut items, mut uses, mut d) = match &kept_parts {
        Some(p) if !verify => (Vec::new(), p.uses.clone(), p.diags.clone()),
        _ => cached(&LIB_CACHE, cache, key, files, |files| {
            let (mut items, mut uses, mut diags) = (Vec::new(), Vec::new(), Vec::new());
            for (i, (prefix, root)) in roots.iter().enumerate() {
                let first = sources.len();
                let stray = collect_module_tree(
                    root,
                    None,
                    root.parent(),
                    prefix,
                    &ext,
                    &metas[i + 1],
                    &mut diags,
                    &mut items,
                    &mut uses,
                    config.keep_ast.then_some(&mut sources),
                    files,
                );
                if !stray.is_empty() {
                    diags.push(entry::stmts_outside_entry(root));
                }
                let pkg = prefix.join("::");
                for f in &mut sources[first..] {
                    f.package.clone_from(&pkg);
                }
            }
            (items, uses, diags)
        }),
    };
    let parts = kept_parts.unwrap_or_else(|| {
        let mut native = Vec::new();
        native_libs(items.iter().map(|(_, i)| i), &mut native);
        LibParts {
            uses: uses.clone(),
            diags: d.clone(),
            native,
            no_implicit: NO_IMPLICIT.with(|n| n.borrow()[lib_no_implicit..].to_vec()),
            hashes: Vec::new(),
            dropped: nabic_hir::cfg_dropped(items.iter().map(|(m, i)| (m.as_str(), i))),
        }
    });
    // A kept stage stands for the library's items, and so for what
    // `#[cfg]` dropped from them.
    let mut lib_dropped = Vec::new();
    if kept && !verify {
        NO_IMPLICIT.with(|n| n.borrow_mut().extend(parts.no_implicit.iter().cloned()));
        lib_dropped.clone_from(&parts.dropped);
    }
    // A file the key did not see (the pre-pass did not read it).
    if !kept && files.len() != files_before {
        library = None;
    }
    if let Some(t) = parse_libs {
        eprintln!(
            "time: {:>9.1}ms  parse library",
            t.elapsed().as_secs_f64() * 1e3
        );
    }
    // Parses not taken (cached library, cfg'd-out modules), dropped on
    // another thread.
    let rest = PARSED.with(std::cell::RefCell::take);
    let _ = std::thread::Builder::new()
        .stack_size(AST_STACK)
        .spawn(move || drop(rest));
    all_items.append(&mut items);
    all_uses.append(&mut uses);
    diags.append(&mut d);
    // `ffi::wit` of the package and of its dependencies.
    let packages: Vec<String> = roots.iter().map(|(p, _)| p.join("::")).collect();
    let prebuilt_names: Vec<String> = prebuilt.iter().map(|p| p.nabihir.name.clone()).collect();
    let mut wit_link = wit::expand(
        &mut all_items,
        &mut all_uses,
        &packages,
        &prebuilt_names,
        &config.target,
        &ext,
        &metas[0],
        &mut diags,
        files,
    );
    objc::expand(
        &mut all_items,
        &mut all_uses,
        &packages,
        &config.target,
        &ext,
        &metas[0],
        &mut diags,
        files,
    );
    // A WIT component whose entry has neither `main` nor statements is a
    // reactor: exports only, no `wasi:cli/run`.
    if let Some(w) = wit_link
        .as_mut()
        .filter(|w| no_entry && w.exports.iter().any(|e| e != "cabi_realloc"))
    {
        w.reactor = true;
    }
    if wit_link.is_some() && !kept {
        library = None;
    }
    coro::rewrite_items(&mut all_items, &mut diags);
    let shadows = tier_modules(&all_items);
    let (item_modules, items): (Vec<String>, Vec<nabic_ast::Item>) = all_items.into_iter().unzip();
    let input = nabic_typeck::HirInput {
        packages: mounted(&config.externs).map(|(n, _)| n.clone()).collect(),
        scopes: scopes.clone(),
        own_library: config.own_library.clone(),
        lib: lib.map(str::to_owned),
        prebuilt: prebuilt.iter().map(|p| p.nabihir.name.clone()).collect(),
        check_library: config.check_library,
        root_main,
        pointer_width: config.target.pointer_width,
        stdcall_system: config.target.stdcall_system(),
        apple: apple_deployment(&config.target),
        item_modules,
        no_implicit_nabi: NO_IMPLICIT.with(std::cell::RefCell::take),
        shadows,
        fingerprint: incr::key(&[incr::target_key(&cfg_env, &config.target).as_bytes()]),
        bodies: incr::current().map(|i| incr::bodies(i, files)),
        summaries: prebuilt
            .iter()
            .flat_map(|p| p.nabihir.summaries.iter().cloned())
            .collect(),
        script: config.anonymous.then(|| nabic_typeck::Script {
            file: script_file.then(|| path.display().to_string()),
            env: script_env(),
        }),
        library,
        stage: kept_stage.clone(),
        cfg_dropped: lib_dropped,
        templates,
        plugin: config.plugin,
        hot: config.hot.as_ref().map(|h| {
            let mut pkgs: Vec<String> = lib.map(str::to_owned).into_iter().collect();
            pkgs.extend(h.deps.iter().cloned());
            pkgs
        }),
    };
    let program = nabic_ast::Program {
        attrs: Vec::new(),
        items,
        stmts: Vec::new(),
        expanded: Vec::new(),
        tops: Vec::new(),
    };

    // Names, types and exhaustiveness are checked in HIR lowering. Warnings
    // go out with errors.
    if diags.iter().any(NabiDiagnostic::is_error) {
        return Err(diags);
    }

    // Use trees -> per-module resolution tables, plus the `pub use`
    // re-exports.
    let (use_table, reexports) = nabic_diagnostic::time("use table", || {
        nabic_typeck::set_scopes(input.scopes.clone());
        nabic_typeck::build_use_table(&all_uses, &input.packages, &input.shadows)
    });
    let mut libs = Vec::new();
    native_libs(&program.items, &mut libs);
    let frameworks = crate::tree::frameworks(&program.items);
    if kept && !verify {
        libs.extend(parts.native.iter().cloned());
    }
    let mut hir = nabic_diagnostic::time("hir", || {
        let lower = |program, input: &nabic_typeck::HirInput| {
            nabic_mono::install();
            nabic_typeck::lower_full(
                program, &prec, &all_uses, &use_table, &reexports, input, files,
            )
        };
        match input.bodies.is_some().then(incr::verify).flatten() {
            Some(check) => {
                let clean = lower(
                    program.clone(),
                    &nabic_typeck::HirInput {
                        bodies: None,
                        library: None,
                        stage: None,
                        ..input.clone()
                    },
                );
                let mut hir = lower(program, &input);
                check(&clean, &mut hir);
                hir
            }
            None => lower(program, &input),
        }
    });
    if !input.shadows.is_empty() {
        hir.diagnostics.iter_mut().for_each(unshadow);
    }
    diags.append(&mut hir.diagnostics);
    // The library's files are the stage's: so are their hashes.
    let lib_hashes = if let (Some(k), false) = (library, kept) {
        let mut parts = parts;
        parts.hashes = package_hashes(&root_dir, &roots, files, &[])
            .into_iter()
            .filter(|(p, _)| !p.is_empty())
            .collect();
        let hashes = parts.hashes.clone();
        keep_library(k, parts, files);
        hashes
    } else if kept {
        parts.hashes
    } else {
        Vec::new()
    };
    let library_items = library.and_then(|k| {
        kept_stage
            .or_else(|| nabic_typeck::kept_library(k))
            .map(|s| nabic_typeck::stage(&s).items())
    });
    if library_items.is_some() {
        sources.retain(|f| f.package.is_empty());
    }
    if let Some(i) = incr::current() {
        i.bodies_reused(hir.reuse.replayed, hir.reuse.lowered);
    }
    // Warnings go out with the errors (a failed build still shows them).
    if diags.iter().any(NabiDiagnostic::is_error) {
        keep_failed(hir, sources);
        return Err(diags);
    }
    let mut mir = nabic_diagnostic::time("mir", || nabic_mir::lower(&hir, &config.target));
    mir.link.packages = package_hashes(&root_dir, &roots, files, &lib_hashes);
    for p in prebuilt {
        let (name, hash) = (&p.nabihir.name, &p.nabihir.hash);
        if let Some(h) = mir.link.packages.iter_mut().find(|(n, _)| n == name) {
            hash.clone_into(&mut h.1);
        }
    }
    mir.link.native_libs = libs;
    mir.link.frameworks = frameworks;
    mir.link.wit = wit_link;
    mir.link.lib = input.lib;
    mir.link.prebuilt = input.prebuilt;
    mir.link.type_offset = prebuilt
        .iter()
        .map(|p| p.nabihir.type_end())
        .max()
        .unwrap_or(0);
    plugin_facts(&mut mir, config, path);
    if let Some(h) = &config.hot {
        if let Err(mut e) = crate::hot::prepare(&mut mir, h) {
            diags.append(&mut e);
            return Err(diags);
        }
    }

    Ok(CompiledProject {
        modules: vec![CompiledModule {
            name: path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("module")
                .to_owned(),
            path: path.to_path_buf(),
            hir,
            mir,
            archives: Vec::new(),
            interface: None,
            facts: Vec::new(),
        }],
        warnings: diags.into_iter().filter(|d| !d.is_error()).collect(),
        tests,
        sources,
        library: library_items,
        files: READ.with(std::cell::RefCell::take),
        reuse: None,
    })
}

/// What a plugin's or host's descriptor carries besides the program:
/// the compiler id, the program's name, the bundled packages, the
/// package's version.
fn plugin_facts(mir: &mut nabic_mir::MirModule, config: &CompilerConfig, path: &Path) {
    if mir.plugin.mode == nabic_mir::plugin::Mode::None {
        return;
    }
    // A test stands for a plugin of another toolchain build.
    mir.plugin.toolchain =
        std::env::var("NABI_TEST_PLUGIN_TOOLCHAIN").unwrap_or_else(|_| crate::nabihir::producer());
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("main")
        .clone_into(&mut mir.plugin.name);
    mir.plugin.bundle.clone_from(&config.bundle);
    // `nabi` says the package's version.
    let v = nabic_core::handoff_var("NABI_PACKAGE_VERSION").and_then(|v| v.into_string().ok());
    if let (Some(lib), Some(v)) = (&mir.link.lib, v) {
        mir.plugin.versions.push((lib.clone(), v));
    }
}

/// Lower MIR again after an embedder edited the HIR (the REPL's slot
/// stores).
pub fn relower(m: &mut CompiledModule) {
    let link = std::mem::take(&mut m.mir.link);
    m.mir = nabic_mir::lower(&m.hir, &m.mir.target);
    m.mir.link = link;
}

/// Each package's hash (a tier per library root, `""`
/// for the user's) over its files; a file belongs to its closest package
/// directory, longest prefix first. `known`: hashes already taken.
pub(crate) fn package_hashes(
    user_dir: &Path,
    roots: &[LibRoot],
    sources: &SourceMap,
    known: &[(String, String)],
) -> Vec<(String, String)> {
    let mut pkgs: Vec<(String, PathBuf)> = roots
        .iter()
        .filter_map(|(prefix, r)| Some((prefix.join("::"), r.parent()?.to_path_buf())))
        .collect();
    pkgs.push((String::new(), user_dir.to_path_buf()));
    let files = sources.files();
    // The closest package directory owns a file.
    let owner = |p: &Path| {
        pkgs.iter()
            .filter(|(_, d)| p.starts_with(d))
            .max_by_key(|(_, d)| d.components().count())
            .map(|(_, d)| d)
    };
    let mut out: Vec<(String, String)> = pkgs
        .iter()
        .map(|(prefix, dir)| {
            if let Some(k) = known.iter().find(|(p, _)| p == prefix) {
                return k.clone();
            }
            let mut mine: Vec<(String, &str)> = files
                .iter()
                .filter(|(path, _)| {
                    let p = Path::new(path);
                    p.starts_with(dir) && owner(p) == Some(dir)
                })
                .map(|(path, text)| {
                    let p = Path::new(path);
                    let rel = p.strip_prefix(dir).unwrap_or(p);
                    let rel: Vec<String> = rel
                        .components()
                        .map(|c| c.as_os_str().to_string_lossy().into_owned())
                        .collect();
                    (rel.join("/"), text.as_str())
                })
                .collect();
            (prefix.clone(), hash_package(&mut mine))
        })
        .collect();
    out.sort_by_key(|(p, _)| std::cmp::Reverse(p.len()));
    out
}

/// SHA-256 over files in byte order of their package-relative
/// `/` paths, each `path \0 content \0`; the first 8 hex digits. Cached
/// per process by a fast hash of the files (a REPL rehashes every cell).
pub(crate) fn hash_package(files: &mut Vec<(String, &str)>) -> String {
    static DONE: std::sync::Mutex<Option<nabic_core::HashMap<u64, String>>> =
        std::sync::Mutex::new(None);
    files.sort_unstable();
    files.dedup();
    let blob: String = files
        .iter()
        .flat_map(|(rel, text)| [rel.as_str(), "\0", text, "\0"])
        .collect();
    let key = nabic_core::hash::fast(blob.as_bytes());
    let mut done = DONE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    done.get_or_insert_with(nabic_core::HashMap::default)
        .entry(key)
        .or_insert_with(|| nabic_codegen::pkg_hash(&blob))
        .clone()
}

/// The modules named like a standard library tier outside the library:
/// `a::std`, the root's `std@`.
pub(crate) fn tier_modules(items: &[(String, nabic_ast::Item)]) -> Vec<String> {
    items
        .iter()
        .filter(|(m, _)| !nabic_core::in_toolchain(m))
        .filter_map(|(m, i)| match i {
            nabic_ast::Item::Mod(d)
                if nabic_core::TIERS.contains(&d.name.trim_end_matches(nabic_typeck::SHADOW)) =>
            {
                Some(if m.is_empty() {
                    d.name.clone()
                } else {
                    format!("{m}::{}", d.name)
                })
            }
            _ => None,
        })
        .collect()
}

/// `d` as source writes the root's tier-named modules (`std`, not `std@`).
fn unshadow(d: &mut NabiDiagnostic) {
    let fix = |s: &mut String| {
        if s.contains(nabic_typeck::SHADOW) {
            *s = nabic_typeck::unshadowed(s);
        }
    };
    fix(&mut d.message);
    d.notes.iter_mut().for_each(fix);
    if let Some(p) = d.primary.as_mut() {
        fix(&mut p.message);
    }
    d.secondary.iter_mut().for_each(|l| fix(&mut l.message));
}
