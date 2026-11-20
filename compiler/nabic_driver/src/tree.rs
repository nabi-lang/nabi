//! Module trees: walking a package's files into one item list.

#[allow(clippy::wildcard_imports)]
use crate::*;

/// Mount tests/ (or benches/) beside src/ as a synthetic root module:
/// a generated body declares `mod <file>;` for each
/// top-level .nabi, so the ordinary machinery resolves the folder form
/// and the submodules under it, rooted at `tests` / `benches`.
#[allow(clippy::too_many_arguments)] // mirrors collect_module_tree
pub(crate) fn collect_extra_dir(
    pkg_root: &Path,
    bench: bool,
    ext: &ExtOps,
    metas: &MetaCtx,
    diags: &mut Vec<NabiDiagnostic>,
    out: &mut Vec<ModItem>,
    uses: &mut Vec<(String, nabic_core::Visibility, nabic_ast::UseTree)>,
    files: &mut SourceMap,
) {
    let dirname = if bench { "benches" } else { "tests" };
    let dir = pkg_root.join(dirname);
    if !source_dir_exists(&dir) {
        return;
    }
    let mut stems: Vec<String> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "nabi"))
        .filter_map(|e| e.path().file_stem()?.to_str().map(str::to_owned))
        .collect();
    stems.sort();
    let mut body = String::new();
    for s in &stems {
        body.push_str("mod ");
        body.push_str(s);
        body.push_str(";\n");
    }
    if body.is_empty() {
        return;
    }
    collect_module_tree(
        &dir.join(format!("<{dirname}>")),
        Some(&body),
        Some(&dir),
        &[dirname.to_owned()],
        ext,
        metas,
        diags,
        out,
        uses,
        None,
        files,
    );
}

/// Walk a package's module tree from `body_path`, flattening items into
/// (module path, item); names stay as written for HIR to qualify.
#[allow(clippy::too_many_arguments)] // recursive walker threading accumulators; a context struct buys nothing
pub(crate) fn collect_module_tree(
    body_path: &Path,
    preloaded: Option<&str>,
    submodule_dir: Option<&Path>,
    module_path: &[String],
    ext: &ExtOps,
    metas: &MetaCtx,
    diags: &mut Vec<NabiDiagnostic>,
    out: &mut Vec<ModItem>,
    uses: &mut Vec<(String, nabic_core::Visibility, nabic_ast::UseTree)>,
    mut keep: Option<&mut Vec<SourceFile>>,
    files: &mut SourceMap,
) -> Vec<nabic_ast::Stmt> {
    let parsed = match PARSED.with(|p| p.borrow_mut().remove(body_path)) {
        Some(p) => p,
        // A file the pre-pass did not see (its result was cached).
        None => {
            let read = preloaded.map_or_else(|| read_source(body_path), |s| Ok(s.to_owned()));
            let source = match read {
                Ok(s) => s,
                Err(e) => {
                    diags.push(NabiDiagnostic::error(
                        Code::MODULE_FILE,
                        nabic_diagnostic::Span::DUMMY,
                        format!("cannot read module file {}: {e}", body_path.display()),
                    ));
                    return Vec::new();
                }
            };
            let file = body_path.display().to_string();
            // Every span parsed from here on knows its file (diagnostic locations).
            let file_id = files.add(file.clone(), source.clone());
            let (program, d) = parse_file(&source, file, file_id, ext, metas);
            let keeping = keep.is_some();
            settle(body_path, file_id, program, d, metas, keeping)
        }
    };
    let Parsed {
        items,
        stmts,
        no_implicit,
        kept,
        diags: mut file_diags,
        ..
    } = parsed;
    if no_implicit {
        NO_IMPLICIT.with(|n| n.borrow_mut().push(module_path.join("::")));
    }
    diags.append(&mut file_diags);
    if let (Some(keep), Some(mut f)) = (keep.as_deref_mut(), kept) {
        f.module = module_path.to_vec();
        keep.push(f);
    }

    // `#[cfg(...)]`-gated items are gone: excluded modules get no files
    // resolved and no inline items flattened.
    flatten_module_items(
        items,
        module_path,
        submodule_dir,
        ext,
        metas,
        diags,
        out,
        uses,
        keep,
        files,
    );
    stmts
}

#[allow(clippy::too_many_arguments)] // mutually recursive with `collect_module_tree`, same accumulators
pub(crate) fn flatten_module_items(
    items: Vec<nabic_ast::Item>,
    module_path: &[String],
    submodule_dir: Option<&Path>,
    ext: &ExtOps,
    metas: &MetaCtx,
    diags: &mut Vec<NabiDiagnostic>,
    out: &mut Vec<ModItem>,
    uses: &mut Vec<(String, nabic_core::Visibility, nabic_ast::UseTree)>,
    mut keep: Option<&mut Vec<SourceFile>>,
    files: &mut SourceMap,
) {
    let module_key = module_path.join("::");
    let mut mods: Vec<String> = Vec::new();
    for item in items {
        match item {
            nabic_ast::Item::Use(u) => {
                uses.push((module_key.clone(), u.vis, u.tree));
            }
            nabic_ast::Item::Mod(m) => {
                let err = |msg: String| NabiDiagnostic::error(Code::MODULE_FILE, m.span, msg);
                if mods.contains(&m.name) {
                    diags.push(err(format!(
                        "module `{}` is declared twice in the same module",
                        m.name
                    )));
                    continue;
                }
                mods.push(m.name.clone());
                // An inline module has no file or folder of its name.
                if let (Some(_), Some(dir)) = (&m.items, submodule_dir) {
                    let file = dir.join(format!("{}.nabi", m.name));
                    let folder = dir.join(&m.name);
                    if let Some(p) = [file, folder].into_iter().find(|p| {
                        if p.extension().is_some() {
                            source_exists(p)
                        } else {
                            source_dir_exists(p)
                        }
                    }) {
                        diags.push(err(format!(
                            "inline module `{}` conflicts with `{}`: a module is either \
                             inline or a file",
                            m.name,
                            p.file_name().unwrap_or_default().to_string_lossy()
                        )));
                        continue;
                    }
                }
                // The root's `std` / `core` / `bare` stay apart from the
                // library's.
                let name = if module_path.is_empty() && nabic_core::TIERS.contains(&m.name.as_str())
                {
                    format!("{}{}", m.name, nabic_typeck::SHADOW)
                } else {
                    m.name.clone()
                };
                let mut child_path = module_path.to_vec();
                child_path.push(name.clone());
                // An empty stub keeps the module's visibility for HIR.
                out.push((
                    module_key.clone(),
                    nabic_ast::Item::Mod(nabic_ast::ModDef {
                        vis: m.vis,
                        attrs: Vec::new(),
                        name,
                        items: Some(Vec::new()),
                        span: m.span,
                    }),
                ));
                if let Some(inner) = m.items {
                    flatten_module_items(
                        inner,
                        &child_path,
                        submodule_dir,
                        ext,
                        metas,
                        diags,
                        out,
                        uses,
                        keep.as_deref_mut(),
                        files,
                    );
                } else {
                    let (body, next_dir) = match mod_file(&m, submodule_dir) {
                        Ok(f) => f,
                        Err(e) => {
                            diags.push(e);
                            continue;
                        }
                    };
                    let stray = collect_module_tree(
                        &body,
                        None,
                        next_dir.as_deref(),
                        &child_path,
                        ext,
                        metas,
                        diags,
                        out,
                        uses,
                        keep.as_deref_mut(),
                        files,
                    );
                    if !stray.is_empty() {
                        diags.push(entry::stmts_outside_entry(&body));
                    }
                }
            }
            other => out.push((module_key.clone(), other)),
        }
    }
}

/// `#[link("z")]` / `#[link(name = "z", kind = "static")]` on the
/// `extern` blocks of `items`.
pub(crate) fn native_libs<'i>(
    items: impl IntoIterator<Item = &'i nabic_ast::Item>,
    out: &mut Vec<(String, bool)>,
) {
    let mut frameworks = Vec::new();
    link_attrs(items, out, &mut frameworks);
}

/// `#[link(name = "Foundation", kind = "framework")]` on the `extern`
/// blocks of `items`.
pub(crate) fn frameworks<'i>(items: impl IntoIterator<Item = &'i nabic_ast::Item>) -> Vec<String> {
    let mut out = Vec::new();
    link_attrs(items, &mut Vec::new(), &mut out);
    out
}

fn link_attrs<'i>(
    items: impl IntoIterator<Item = &'i nabic_ast::Item>,
    out: &mut Vec<(String, bool)>,
    frameworks: &mut Vec<String>,
) {
    for item in items {
        match item {
            Item::Mod(m) => link_attrs(m.items.as_deref().unwrap_or_default(), out, frameworks),
            Item::Extern(b) => {
                for a in b.attrs.iter().filter(|a| a.path == ["link"]) {
                    let AttrBody::Args(args) = &a.body else {
                        continue;
                    };
                    let mut name = None;
                    let mut stat = false;
                    let mut framework = false;
                    for arg in args {
                        match arg {
                            AttrArg::Lit(Literal::Str(n)) => name = Some(n.clone()),
                            AttrArg::KeyValue(k, Literal::Str(v)) if k == "name" => {
                                name = Some(v.clone());
                            }
                            AttrArg::KeyValue(k, Literal::Str(v)) if k == "kind" => {
                                stat = v == "static";
                                framework = v == "framework";
                            }
                            _ => {}
                        }
                    }
                    match name {
                        Some(n) if framework => {
                            if !frameworks.contains(&n) {
                                frameworks.push(n);
                            }
                        }
                        Some(n) if !out.iter().any(|(m, _)| *m == n) => out.push((n, stat)),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
}
