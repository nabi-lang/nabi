//! Per-package keyword and literal tables.

#[allow(clippy::wildcard_imports)]
use crate::*;

/// What a package's parse needs from the pre-pass: every meta block,
/// and the package's own keywords with their handler paths.
pub(crate) struct MetaCtx {
    /// `""` for the package being built, else its library root's path.
    pub(crate) package: String,
    pub(crate) table: std::sync::Arc<MetaTable>,
    pub(crate) keywords: KeywordTable,
    /// `#[literal]` patterns: the package's own, plus the standard libraries'.
    pub(crate) literals: std::sync::Arc<LiteralTable>,
    /// `tables_key`, once asked for.
    pub(crate) key: std::sync::OnceLock<u64>,
    pub(crate) target: nabic_mir::Target,
    /// The package's cfg (`package_cfg`).
    pub(crate) cfg: CfgEnv,
    /// `(alias, name)` of the dependencies (`extern_aliases`).
    pub(crate) aliases: std::sync::Arc<Vec<(String, String)>>,
}

impl MetaCtx {
    /// Key of what a file's parse here depends on besides its text:
    /// operators, meta blocks, keywords, literals, cfg.
    pub(crate) fn tables_key(&self, ext: &ExtOps) -> u64 {
        *self.key.get_or_init(|| {
            let mut levels: Vec<String> = ext.levels.iter().map(|l| format!("{l:?}")).collect();
            levels.sort();
            incr::key(&[
                levels.join(";").as_bytes(),
                self.package.as_bytes(),
                format!("{:?}", self.table).as_bytes(),
                format!("{:?}", self.keywords).as_bytes(),
                format!("{:?}", self.literals).as_bytes(),
                incr::target_key(&self.cfg, &self.target).as_bytes(),
                format!("{:?}", self.aliases).as_bytes(),
            ])
        })
    }
}

/// Per package (user first), its keywords and handler paths. A
/// keyword registered twice is an error. Type keywords
/// (`#[keyword("int")] struct Int`) apply everywhere and only the
/// standard library may register them.
pub(crate) fn keyword_tables(
    defs: Vec<KeywordDef>,
    entry: &Path,
    user_dir: &Path,
    roots: &[LibRoot],
    diags: &mut Vec<NabiDiagnostic>,
) -> Vec<KeywordTable> {
    let pkgs = package_roots(user_dir, roots);
    let mut out = vec![Vec::new(); pkgs.len()];
    let mut seen: Vec<&KeywordDef> = Vec::new();
    for d in &defs {
        if let Some(e) = check_keyword(d) {
            diags.push(e);
            continue;
        }
        if let Some(old) = seen.iter().find(|o| o.name == d.name) {
            diags.push(
                NabiDiagnostic::error(
                    Code::DUPLICATE_REGISTRATION,
                    d.span,
                    format!("keyword `{}` is registered twice", d.name),
                )
                .with_secondary(old.span, "first registered here"),
            );
            continue;
        }
        seen.push(d);
        let Some((i, path)) = handler_path(&pkgs, entry, &d.file, &d.handler) else {
            continue;
        };
        if !d.ty {
            out[i].push((d.name.clone(), path, false));
        } else if path.first().is_some_and(|p| std_root(p)) {
            for t in &mut out {
                t.push((d.name.clone(), path.clone(), true));
            }
        } else {
            diags.push(NabiDiagnostic::error(
                Code::INVALID_KEYWORD_DECL,
                d.span,
                format!(
                    "`#[keyword(\"{}\")]` must be on a function - its handler; \
                         only the standard library names a type with it",
                    d.name
                ),
            ));
        }
    }
    out
}

/// (package dir, path root) of the user package, then each library root.
pub(crate) fn package_roots(user_dir: &Path, roots: &[LibRoot]) -> Vec<(PathBuf, Vec<String>)> {
    let mut pkgs: Vec<(PathBuf, Vec<String>)> = vec![(
        user_dir.to_path_buf(),
        vec![nabic_parser::PKG_ROOT.to_owned()],
    )];
    for (prefix, r) in roots {
        let dir = r
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        // A package's handlers are named from its root, `pkg@<dep>`
        // (the user's `pkg@`, std's `pkg@std`): the same item whichever
        // package's source the call lands in.
        let path = vec![format!("{}{}", nabic_parser::PKG_ROOT, prefix.join("::"))];
        pkgs.push((dir, path));
    }
    pkgs
}

/// `root` (a handler path's first segment) is a standard library tier's.
fn std_root(root: &str) -> bool {
    root.strip_prefix(nabic_parser::PKG_ROOT)
        .is_some_and(nabic_core::in_toolchain)
}

/// The package (index, entry) of `pkgs` owning `file`: the closest
/// package directory.
pub(crate) fn package_of<'a>(
    pkgs: &'a [(PathBuf, Vec<String>)],
    file: &Path,
) -> Option<(usize, &'a (PathBuf, Vec<String>))> {
    pkgs.iter()
        .enumerate()
        .filter(|(_, (dir, _))| file.starts_with(dir))
        .max_by_key(|(_, (dir, _))| dir.components().count())
}

/// Package index and absolute path of `handler` in `file`: the closest
/// package directory owns it; the path below is the module path, the entry
/// file its root; `handler` may go through inline `mod`s.
pub(crate) fn handler_path(
    pkgs: &[(PathBuf, Vec<String>)],
    entry: &Path,
    file: &str,
    handler: &str,
) -> Option<(usize, Vec<String>)> {
    let file = Path::new(file);
    let (i, (dir, root)) = package_of(pkgs, file)?;
    let mut path = root.clone();
    let rel = file.strip_prefix(dir).unwrap_or(file).with_extension("");
    let segs: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    if file != entry && !(segs.len() == 1 && (segs[0] == "lib" || segs[0] == "main")) {
        path.extend(segs);
    }
    path.extend(handler.split("::").map(str::to_owned));
    Some((i, path))
}

/// The `#[literal]` patterns the parser matches, per package (the
/// package being built first): its own, the `pub` ones of the packages
/// it depends on (`deps`), then the standard libraries', which apply
/// everywhere. `lib` is the library being built, when it is one of
/// `roots`. HIR dispatches the built-in shapes; the real parse reports
/// bad patterns.
#[allow(clippy::too_many_arguments)]
pub(crate) fn literal_tables(
    defs: Vec<LiteralDef>,
    entry: &Path,
    user_dir: &Path,
    roots: &[LibRoot],
    deps: &[String],
    lib: Option<&str>,
    ext: &ExtOps,
    diags: &mut Vec<NabiDiagnostic>,
) -> Vec<std::sync::Arc<LiteralTable>> {
    let pkgs = package_roots(user_dir, roots);
    let std_pkg = |i: usize| pkgs[i].1.first().is_some_and(|s| std_root(s));
    let mut own: Vec<Vec<LitRule>> = vec![Vec::new(); pkgs.len()];
    let mut exported: Vec<(usize, LitRule)> = Vec::new();
    let mut std = Vec::new();
    for d in defs {
        if d.handler.is_empty() {
            diags.push(NabiDiagnostic::error(
                Code::INVALID_LITERAL_HANDLER,
                d.span,
                "`#[literal(..)]` must be on a function - its handler",
            ));
            continue;
        }
        let Ok(pattern) = parse_pattern(&d.tokens, &ext.symbols()) else {
            continue;
        };
        let Ok(kind) = pattern.kind() else { continue };
        if matches!(kind, PatKind::Shape(_)) {
            continue;
        }
        let Some((i, handler)) = handler_path(&pkgs, entry, &d.file, &d.handler) else {
            continue;
        };
        let rule = LitRule {
            pattern,
            kind,
            handler,
            span: d.span,
        };
        if std_pkg(i) {
            std.push(rule);
            continue;
        }
        if d.public {
            exported.push((i, rule.clone()));
        }
        own[i].push(rule);
    }
    // The packages built from this source: the user's and `lib`.
    let name = |i: usize| i.checked_sub(1).map(|r| roots[r].0.join("::"));
    let built = |i: usize| i == 0 || (lib.is_some() && name(i).as_deref() == lib);
    let dep = |j: usize| name(j).is_some_and(|n| deps.contains(&n));
    own.into_iter()
        .enumerate()
        .map(|(i, mut rules)| {
            if built(i) {
                let from = exported.iter().filter(|(j, _)| *j != i && dep(*j));
                rules.extend(from.map(|(_, r)| r.clone()));
            }
            rules.extend(std.iter().cloned());
            std::sync::Arc::new(LiteralTable { rules })
        })
        .collect()
}
