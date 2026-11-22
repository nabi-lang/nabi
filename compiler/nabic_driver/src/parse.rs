//! Parsing a package: the operator pre-pass, then the real parse.

#[allow(clippy::wildcard_imports)]
use crate::*;

pub(crate) fn build_prec_table(
    roots: &[LibRoot],
    diags: &mut Vec<NabiDiagnostic>,
    metas: &mut PrePass,
    sources: &mut SourceMap,
) -> PrecTable {
    // The library's summaries are kept apart from a package's: every
    // build of the toolchain shares them (a lone file's too).
    let store = incr::library_prepass(&format!("{roots:?}"));
    let mut files = prepass_parse(
        roots
            .iter()
            .map(|(_, root)| (root.clone(), None, root.parent().map(Path::to_path_buf)))
            .collect(),
        sources,
        store.as_ref(),
    );
    if let Some(s) = &store {
        incr::library_prepass_save(s);
    }
    let mut table = PrecTable::new();
    for (pkg, root) in roots {
        let mut tables = Vec::new();
        let first = metas.metas.len();
        prepass_tree(root, root.parent(), pkg, &mut files, metas, &mut tables);
        let package = pkg.join("::");
        for d in &mut metas.metas[first..] {
            d.set_package(&package);
        }
        for (sub_table, mut collect_diags) in tables {
            diags.append(&mut collect_diags);
            diags.extend(sub_table.entries().cloned().filter_map(|mut e| {
                e.package.clone_from(&package);
                table.merge(e)
            }));
        }
    }
    table
}

/// The rooted paths the built package's `pub` meta blocks write
/// (`pkg::f`, `std::..`, `dep::..`; not macros), for HIR to check from
/// each block's module.
pub(crate) fn template_refs(defs: &[MetaDef], deps: &[String]) -> Vec<nabic_typeck::TemplateRefs> {
    let mut out = Vec::new();
    for d in defs.iter().filter(|d| d.public) {
        let b = &d.body;
        let mut paths = Vec::new();
        let mut k = 0;
        while k < b.len() {
            let K::Ident(root) = &b[k].kind else {
                k += 1;
                continue;
            };
            let after_sep = k > 0 && matches!(b[k - 1].kind, K::ColonColon | K::Dot | K::At);
            let rooted = root.starts_with(nabic_parser::PKG_ROOT)
                || root == "sup"
                || nabic_core::TIERS.contains(&root.as_str())
                || deps.contains(root);
            if after_sep || !rooted || b.get(k + 1).map(|t| &t.kind) != Some(&K::ColonColon) {
                k += 1;
                continue;
            }
            let mut segs = vec![root.clone()];
            let mut end = k + 1;
            while end + 1 < b.len() && b[end].kind == K::ColonColon {
                let K::Ident(n) = &b[end + 1].kind else {
                    break;
                };
                segs.push(n.clone());
                end += 2;
            }
            // A macro (`pkg::m!`) is the meta table's.
            if segs.len() > 1 && b.get(end).map(|t| &t.kind) != Some(&K::Bang) {
                paths.push((segs, b[k].span));
            }
            k = end;
        }
        if !paths.is_empty() {
            out.push(nabic_typeck::TemplateRefs {
                module: d.module.clone(),
                paths,
            });
        }
    }
    out
}

/// Pre-pass parses by path, with their file ids.
pub(crate) type PreFiles = nabic_core::HashMap<PathBuf, (u32, Prepassed)>;

/// `job(i)` for each `i < n` on a thread per core (`stack` bytes each);
/// `None` where no thread got to it.
pub(crate) fn on_cores<T: Send>(
    n: usize,
    stack: usize,
    job: impl Fn(usize) -> T + Sync,
) -> std::io::Result<Vec<Option<T>>> {
    let next = std::sync::atomic::AtomicUsize::new(0);
    let cores = std::thread::available_parallelism().map_or(1, usize::from);
    let results: Vec<std::sync::Mutex<Option<T>>> =
        (0..n).map(|_| std::sync::Mutex::new(None)).collect();
    std::thread::scope(|s| {
        for _ in 0..cores.min(n) {
            std::thread::Builder::new()
                .stack_size(stack)
                .spawn_scoped(s, || {
                    loop {
                        let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        if i >= n {
                            break;
                        }
                        let r = job(i);
                        *results[i]
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(r);
                    }
                })?;
        }
        Ok::<(), std::io::Error>(())
    })?;
    Ok(results
        .into_iter()
        .map(|r| {
            r.into_inner()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        })
        .collect())
}

/// `on_cores`, each job run (or the compile stops).
pub(crate) fn on_cores_all<T: Send>(
    n: usize,
    stack: usize,
    job: impl Fn(usize) -> T + Sync,
) -> Vec<T> {
    on_cores(n, stack, job)
        .expect("start a worker thread")
        .into_iter()
        .map(|r| r.expect("a worker ran every job"))
        .collect()
}

pub(crate) fn prepass_parse(
    roots: Vec<(PathBuf, Option<String>, Option<PathBuf>)>,
    sources: &mut SourceMap,
    store: Option<&incr::Store>,
) -> PreFiles {
    let mut files = nabic_core::HashMap::default();
    let mut seen = nabic_core::HashSet::default();
    let mut level = roots;
    while !level.is_empty() {
        let jobs: Vec<(PathBuf, Option<PathBuf>, String, u32)> = level
            .into_iter()
            .filter(|(path, ..)| seen.insert(path.clone()))
            .filter_map(|(path, source, sub_dir)| {
                let source = source.or_else(|| read_source(&path).ok())?;
                let id = sources.add(path.display().to_string(), source.clone());
                Some((path, sub_dir, source, id))
            })
            .collect();
        let ids = store.is_some().then(|| incr::Ids::of(sources));
        let parsed: Vec<Prepassed> = on_cores_all(jobs.len(), 2 << 20, |i| {
            let (path, _, source, id) = &jobs[i];
            prepass_file(path, source, *id, store.zip(ids.as_ref()))
        });
        level = Vec::new();
        for ((path, sub_dir, _, id), file) in jobs.into_iter().zip(parsed) {
            mod_files(&file.sum.skeleton, sub_dir.as_deref(), &mut level);
            files.insert(path, (id, file));
        }
    }
    files
}

/// One pre-pass file `id`: lexed without extension operators and parsed
/// (or its summary from `store`).
pub(crate) fn prepass_file(
    path: &Path,
    source: &str,
    id: u32,
    store: Option<(&incr::Store, &incr::Ids)>,
) -> Prepassed {
    let file = path.display().to_string();
    let k = incr::key(&[b"pre", file.as_bytes(), source.as_bytes()]);
    if let Some(sum) = store.and_then(|(s, ids)| s.get::<Summary>(k, ids)) {
        return Prepassed {
            source: source.to_owned(),
            sum,
            program: None,
        };
    }
    let (program, sum) = plain_parse(&file, source, id, |tokens, program, quiet| {
        Summary::of(tokens, program, quiet, &file, source)
    });
    if let Some((s, ids)) = store {
        s.put(k, &sum, ids);
    }
    Prepassed {
        source: source.to_owned(),
        sum,
        program: Some(program),
    }
}

/// Lex file `id` without extension operators, parse it, and pass `then`
/// the tokens, program and whether it was diagnostic-free.
pub(crate) fn plain_parse<T>(
    file: &str,
    source: &str,
    id: u32,
    then: impl FnOnce(&[nabic_lexer::Token], &nabic_ast::Program, bool) -> T,
) -> (nabic_ast::Program, T) {
    let (mut tokens, lex) = Lexer::new(source, file.to_owned()).lex_all();
    for t in &mut tokens {
        t.span.file = id;
    }
    let (program, parse) = parse_program(&tokens, file.to_owned());
    let t = then(&tokens, &program, lex.is_empty() && parse.is_empty());
    (program, t)
}

/// The files of the `mod name;` among `items` (inline modules' too),
/// their submodules in `sub_dir`.
pub(crate) fn mod_files(
    items: &[nabic_ast::Item],
    sub_dir: Option<&Path>,
    out: &mut Vec<(PathBuf, Option<String>, Option<PathBuf>)>,
) {
    for item in items {
        if let nabic_ast::Item::Mod(m) = item {
            match &m.items {
                Some(inner) => mod_files(inner, sub_dir, out),
                None => {
                    if let Ok((body, next)) = mod_file(m, sub_dir) {
                        out.push((body, None, next));
                    }
                }
            }
        }
    }
}

/// The pre-pass over a package's module tree: each module parsed without
/// extension operators (that's what it finds), submodules first, gathering
/// meta blocks, keywords and literals. Errors are the real parse's.
pub(crate) fn prepass_tree(
    path: &Path,
    sub_dir: Option<&Path>,
    module: &[String],
    files: &mut PreFiles,
    metas: &mut PrePass,
    out: &mut Vec<(PrecTable, Vec<NabiDiagnostic>)>,
) {
    let Some((id, f)) = files.remove(path) else {
        return;
    };
    let first = metas.metas.len();
    prepass_gather(&f.sum, &f.source, &path.display().to_string(), id, metas);
    let file_module = module.join("::");
    for d in &mut metas.metas[first..] {
        d.set_module(&file_module);
    }
    prepass_mods(&f.sum.skeleton, sub_dir, module, files, metas, out);
    let skeleton = nabic_ast::Program {
        attrs: Vec::new(),
        items: f.sum.skeleton.clone(),
        stmts: Vec::new(),
        expanded: Vec::new(),
        tops: Vec::new(),
    };
    out.push(collect_prec_table(&skeleton, module));
    PREPASSED.with(|p| p.borrow_mut().insert(path.to_path_buf(), (id, f)));
}

/// Would the real parse match the pre-pass's? Yes when the file uses no
/// extension operators, keywords, meta blocks, literals or cfg.
pub(crate) fn reusable(p: &Prepassed, ext: &ExtOps, metas: &MetaCtx) -> bool {
    let sum = &p.sum;
    let prefixes: Vec<Vec<u8>> = metas
        .literals
        .rules
        .iter()
        .filter(|r| r.kind == PatKind::Prefix)
        .filter_map(|r| match r.pattern.elems.first() {
            Some(nabic_parser::PatElem::Tok(k, _)) => Some(kind_bytes(k)),
            _ => None,
        })
        .collect();
    sum.quiet
        && !sum.ext_op
        && !sum.cfg_bang
        && !ext.symbols().iter().any(|s| p.source.contains(s.as_str()))
        && !sum.idents.iter().any(|(n, _)| {
            matches!(n.as_str(), "cfg" | "cfg_attr")
                || metas.table.defines(n)
                || metas.keywords.iter().any(|(k, ..)| k == n)
                || metas.aliases.iter().any(|(a, _)| a == n)
        })
        && !sum.kinds.iter().any(|k| prefixes.contains(k))
}

/// A token kind's encoding (kinds compare by it).
pub(crate) fn kind_bytes(k: &nabic_lexer::TokenKind) -> Vec<u8> {
    let mut e = nabic_core::codec::Enc::new(false);
    nabic_core::codec::Codec::enc(k, &mut e);
    e.buf
}

pub(crate) fn prepass_mods(
    items: &[nabic_ast::Item],
    sub_dir: Option<&Path>,
    module: &[String],
    files: &mut PreFiles,
    metas: &mut PrePass,
    out: &mut Vec<(PrecTable, Vec<NabiDiagnostic>)>,
) {
    for item in items {
        let nabic_ast::Item::Mod(m) = item else {
            continue;
        };
        let before = metas.metas.len();
        let mut sub = module.to_vec();
        sub.push(m.name.clone());
        match &m.items {
            Some(inner) => prepass_mods(inner, sub_dir, &sub, files, metas, out),
            None => {
                if let Ok((body, next)) = mod_file(m, sub_dir) {
                    prepass_tree(&body, next.as_deref(), &sub, files, metas, out);
                }
            }
        }
        // The module's `#[cfg]` holds for the meta blocks of its files
        // (a block is registered only when its cfgs hold).
        let cfg = m.attrs.iter().filter(|a| a.name() == "cfg");
        for d in &mut metas.metas[before..] {
            d.cfg.extend(cfg.clone().cloned());
        }
    }
}

/// The file of `mod name;` in a module whose submodules live in `dir`
/// and its own submodule directory. `#[path("f.nabi")]`
/// is relative to `dir`, submodules in `f/`.
pub(crate) fn mod_file(
    m: &nabic_ast::ModDef,
    dir: Option<&Path>,
) -> Result<(PathBuf, Option<PathBuf>), NabiDiagnostic> {
    let name = &m.name;
    let err =
        |msg: String| NabiDiagnostic::error(Code::MODULE_FILE, nabic_diagnostic::Span::DUMMY, msg);
    let custom = m.attrs.iter().find(|a| a.path == ["path"]).map(|a| {
        match &a.body {
            nabic_ast::AttrBody::Args(args) => match args.as_slice() {
                [nabic_ast::AttrArg::Lit(nabic_ast::Literal::Str(p))] => Ok(p.clone()),
                _ => Err(()),
            },
            _ => Err(()),
        }
        .map_err(|()| {
            err("`#[path]` takes one file path: `#[path(\"file.nabi\")]`".to_owned())
                .with_span(a.span, "")
        })
    });
    let Some(dir) = dir else {
        return Err(err(format!(
            "`mod {name};` loads `{name}.nabi` beside the file, and this code has no file \
             (stdin or a REPL session): write `mod {name} {{ .. }}` here instead"
        )));
    };
    let (body, folder) = match custom.transpose()? {
        Some(p) => {
            let body = dir.join(&p);
            let folder = body.with_extension("");
            (body, folder)
        }
        None => (dir.join(format!("{name}.nabi")), dir.join(name)),
    };
    // A module's file is named after the module; mod.nabi is
    // never a body file, so reject it even when reached through #[path].
    if body.file_name().is_some_and(|n| n == "mod.nabi") {
        return Err(err(format!(
            "Nabi has no `mod.nabi`: name a module's file after the module \
             (`{name}.nabi`), with a `{name}/` folder beside it for its submodules"
        )));
    }
    if !source_exists(&body) {
        // A mod.rs habit puts the body in foo/mod.nabi; point at foo.nabi.
        let stray = folder.join("mod.nabi");
        let hint = if source_exists(&stray) {
            format!("; rename `{}` to `{}`", stray.display(), body.display())
        } else {
            String::new()
        };
        // A single-file module's submodules go in a folder not made yet.
        let missing = body.parent().filter(|d| !source_dir_exists(d));
        let msg = match missing {
            Some(d) => format!(
                "module `{name}` body not found at `{}`: there is no `{}/` folder",
                body.display(),
                d.display()
            ),
            None => format!(
                "module `{name}` body not found at `{}`{hint}",
                body.display()
            ),
        };
        return Err(err(msg).with_help(format!(
            "create `{}`, or write `mod {name} {{ .. }}` in place of `mod {name};`",
            body.display()
        )));
    }
    // Kept when missing too: a `mod x;` inside then names `folder/x.nabi`.
    Ok((body, Some(folder)))
}

/// A file that defines meta blocks (`@`), keywords or literals: the
/// pre-pass gathers them, and its interface copy is kept whole.
pub(crate) fn defines_metas(tokens: &[nabic_lexer::Token], source: &str) -> bool {
    tokens.iter().any(|t| t.kind == nabic_lexer::TokenKind::At)
        || source.contains("#[keyword(")
        || source.contains("#[literal(")
        || source.contains("#[runtime_attribute(")
}

/// Gather the meta blocks, keywords and literals of a pre-pass file
/// `id` (their template tokens keep spans into file `id`).
pub(crate) fn prepass_gather(
    sum: &Summary,
    source: &str,
    file: &str,
    id: u32,
    metas: &mut PrePass,
) {
    if let Some(idents) = &mut metas.idents {
        // The user package's names, first use per file.
        idents.extend(sum.idents.iter().cloned());
    }
    if !sum.metas.is_empty() {
        metas
            .meta_files
            .push((file.to_owned(), source.to_owned(), id));
    }
    metas.metas.extend(sum.metas.iter().cloned());
    metas.keywords.extend(sum.keywords.iter().cloned());
    metas.literals.extend(sum.literals.iter().cloned());
}

/// Confusable names across a package's files warn (the lexer
/// checks within a file).
pub(crate) fn cross_file_confusables(
    idents: Vec<(String, nabic_diagnostic::Span)>,
) -> Vec<NabiDiagnostic> {
    if idents.iter().all(|(n, _)| n.is_ascii()) {
        return Vec::new();
    }
    let mut first: nabic_core::HashMap<String, &(String, nabic_diagnostic::Span)> =
        nabic_core::HashMap::default();
    let mut out = Vec::new();
    for id in &idents {
        match first.entry(nabic_lexer::unicode::skeleton(&id.0)) {
            std::collections::hash_map::Entry::Occupied(o) => {
                let (name, at) = o.get();
                if *name != id.0 && at.file != id.1.file {
                    out.push(
                        NabiDiagnostic::warning(
                            Code::LINT,
                            id.1,
                            format!(
                                "identifier `{}` looks like `{name}` (confusable spelling)",
                                id.0
                            ),
                        )
                        .with_secondary(*at, "the other spelling")
                        .with_lint("confusable_idents"),
                    );
                }
            }
            std::collections::hash_map::Entry::Vacant(v) => {
                v.insert(id);
            }
        }
    }
    out
}

/// Re-gather meta block bodies with the extension operators
/// so templates can use `⊕`.
pub(crate) fn relex_metas(metas: &mut PrePass, ext: &nabic_parser::ExtOps) {
    let symbols = ext.symbols();
    if symbols.is_empty() || metas.meta_files.is_empty() {
        return;
    }
    // What the module tree gave each block: its package, module and the
    // cfgs of enclosing modules.
    let placed: nabic_core::HashMap<_, _> = std::mem::take(&mut metas.metas)
        .into_iter()
        .map(|d| ((d.file, d.span), (d.package, d.module, d.cfg)))
        .collect();
    for (file, source, id) in &metas.meta_files {
        let package = placed
            .iter()
            .find(|((f, _), _)| f == file)
            .map_or("", |(_, (p, ..))| p.as_str());
        let symbols = ext.in_package(package).symbols();
        let lexer = Lexer::new(source, file.clone()).with_ext_ops(symbols);
        let (mut tokens, _) = lexer.lex_all();
        for t in &mut tokens {
            t.span.file = *id;
        }
        for mut d in collect_meta_blocks(&tokens, file) {
            if let Some((package, module, cfg)) = placed.get(&(d.file.clone(), d.span)) {
                d.set_package(package);
                d.module.clone_from(module);
                d.cfg.clone_from(cfg);
            }
            metas.metas.push(d);
        }
    }
}

/// What the pre-pass gathers besides operators: meta blocks and
/// keyword registrations.
#[derive(Clone, Default)]
pub(crate) struct PrePass {
    pub(crate) metas: Vec<MetaDef>,
    /// While the user package is pre-lexed: its identifiers, first use
    /// per file (`cross_file_confusables`).
    pub(crate) idents: Option<Vec<(String, nabic_diagnostic::Span)>>,
    /// Files with meta blocks (path, source, file id): lexed again once
    /// the extension operators are known (`relex_metas`).
    pub(crate) meta_files: Vec<(String, String, u32)>,
    pub(crate) keywords: Vec<KeywordDef>,
    pub(crate) literals: Vec<LiteralDef>,
}

/// A pre-pass parse, for the real parse to take when `reusable`.
pub(crate) struct Prepassed {
    pub(crate) source: String,
    pub(crate) sum: Summary,
    /// The parse, unless the summary came from the incremental store.
    pub(crate) program: Option<nabic_ast::Program>,
}

/// What the pre-pass needs of a file's plain parse.
pub(crate) struct Summary {
    /// Its `mod` and `trait` items (`trait` method bodies dropped): the
    /// module tree and the operator declarations.
    pub(crate) skeleton: Vec<nabic_ast::Item>,
    /// Lexed and parsed without a diagnostic.
    pub(crate) quiet: bool,
    /// Each identifier at its first use.
    pub(crate) idents: Vec<(String, nabic_diagnostic::Span)>,
    /// Its other token kinds (`kind_bytes`), each once.
    pub(crate) kinds: Vec<Vec<u8>>,
    /// An extension-operator token; `cfg!`.
    pub(crate) ext_op: bool,
    pub(crate) cfg_bang: bool,
    /// The path literals of its `include_str!("..")` / `include_bytes!("..")`.
    pub(crate) includes: Vec<String>,
    pub(crate) metas: Vec<MetaDef>,
    pub(crate) keywords: Vec<KeywordDef>,
    pub(crate) literals: Vec<LiteralDef>,
}

nabic_core::codec_struct!(Summary {
    skeleton,
    quiet,
    idents,
    kinds,
    ext_op,
    cfg_bang,
    includes,
    metas,
    keywords,
    literals,
});

impl Summary {
    pub(crate) fn of(
        tokens: &[nabic_lexer::Token],
        program: &nabic_ast::Program,
        quiet: bool,
        file: &str,
        source: &str,
    ) -> Self {
        let mut seen = nabic_core::HashSet::default();
        let mut kinds = nabic_core::HashSet::default();
        let mut sum = Self {
            skeleton: skeleton(&program.items),
            quiet,
            idents: Vec::new(),
            kinds: Vec::new(),
            ext_op: false,
            cfg_bang: false,
            includes: Vec::new(),
            metas: Vec::new(),
            keywords: Vec::new(),
            literals: Vec::new(),
        };
        for (i, t) in tokens.iter().enumerate() {
            match &t.kind {
                K::Ident(n) => {
                    if seen.insert(n.as_str()) {
                        sum.idents.push((n.clone(), t.span));
                    }
                    if n == "cfg" && tokens.get(i + 1).is_some_and(|b| b.kind == K::Bang) {
                        sum.cfg_bang = true;
                    }
                    if let (
                        "include_str" | "include_bytes",
                        [
                            nabic_lexer::Token { kind: K::Bang, .. },
                            nabic_lexer::Token {
                                kind: K::LParen, ..
                            },
                            nabic_lexer::Token {
                                kind: K::StrLit(s), ..
                            },
                            ..,
                        ],
                    ) = (n.as_str(), tokens.get(i + 1..).unwrap_or_default())
                    {
                        sum.includes.push(s.clone());
                    }
                }
                K::ExtOp(_) => sum.ext_op = true,
                k => {
                    let b = kind_bytes(k);
                    if kinds.insert(b.clone()) {
                        sum.kinds.push(b);
                    }
                }
            }
        }
        if defines_metas(tokens, source) {
            sum.metas = collect_meta_blocks(tokens, file);
            sum.keywords = collect_keywords(&program.items, file);
            sum.literals = collect_literals(tokens, file);
        }
        sum
    }
}

/// What `collect_prec_table` reads: `mod` and `trait` items and the
/// functions / methods with an operator attribute, bodies dropped.
pub(crate) fn skeleton(items: &[nabic_ast::Item]) -> Vec<nabic_ast::Item> {
    let op = |f: &nabic_ast::FnDef| {
        f.attrs
            .iter()
            .any(|a| matches!(a.name(), "infix" | "prefix" | "postfix"))
    };
    let op_method = |m: &ImplItem| matches!(m, ImplItem::Method(f) if op(f));
    items
        .iter()
        .filter_map(|item| match item {
            Item::Mod(m) => {
                let mut m = m.clone();
                m.items = m.items.as_deref().map(skeleton);
                Some(Item::Mod(m))
            }
            Item::Trait(t) => {
                let mut t = t.clone();
                for ti in &mut t.items {
                    if let TraitItem::Method(f) = ti {
                        f.body = None;
                    }
                }
                Some(Item::Trait(t))
            }
            Item::Fn(f) if op(f) => Some(Item::Fn(nabic_ast::FnDef {
                body: None,
                ..f.clone()
            })),
            Item::Impl(i) if i.items.iter().any(op_method) => {
                let mut i = i.clone();
                i.items.retain(op_method);
                for m in &mut i.items {
                    if let ImplItem::Method(f) = m {
                        f.body = None;
                    }
                }
                Some(Item::Impl(i))
            }
            _ => None,
        })
        .collect()
}

thread_local! {
    /// This compile's pre-pass parses by file path, with their file ids.
    pub(crate) static PREPASSED: std::cell::RefCell<PreFiles> =
        std::cell::RefCell::new(nabic_core::HashMap::default());
    /// This compile's real parses by file path (`real_parse`).
    pub(crate) static PARSED: std::cell::RefCell<nabic_core::HashMap<PathBuf, Parsed>> =
        std::cell::RefCell::new(nabic_core::HashMap::default());
    /// The modules whose file opts out of the implicit import
    /// (`#![no_implicit_nabi]`).
    pub(crate) static NO_IMPLICIT: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// A file's real parse: items with `#[cfg]` applied, top-level
/// statements, a `keep_ast` copy, diagnostics.
pub(crate) struct Parsed {
    pub(crate) items: Vec<nabic_ast::Item>,
    pub(crate) stmts: Vec<nabic_ast::Stmt>,
    /// `#![no_implicit_nabi]`.
    pub(crate) no_implicit: bool,
    pub(crate) kept: Option<SourceFile>,
    pub(crate) diags: Vec<NabiDiagnostic>,
    /// The files its `include_str!` / `include_bytes!` read.
    pub(crate) includes: Vec<PathBuf>,
}

// AST depth follows the longest operator chain and dropping or cloning
// recurses that deep, so those threads get the main thread's stack.
pub(crate) const AST_STACK: usize = 1 << 30;

/// Real-parse every pre-passed file in parallel into `PARSED`, each with
/// its package's `metas`. File ids are the pre-pass's, so thread order
/// doesn't matter; token buffers are dropped and `keep` copies made on the
/// workers.
pub(crate) fn real_parse(
    incr: Option<(&incr::Incr, &incr::Ids)>,
    entry: &Path,
    user_dir: &Path,
    roots: &[LibRoot],
    metas: &[MetaCtx],
    ext: &ExtOps,
    keep: bool,
) {
    let files: Vec<(PathBuf, (u32, Prepassed))> =
        PREPASSED.with(|p| p.borrow_mut().drain().collect());
    let dirs: Vec<PathBuf> = package_roots(user_dir, roots)
        .into_iter()
        .map(|(d, _)| d)
        .collect();
    let files: Vec<std::sync::Mutex<Option<_>>> = files
        .into_iter()
        .map(|f| std::sync::Mutex::new(Some(f)))
        .collect();
    let parsed: Vec<(PathBuf, Parsed)> = on_cores_all(files.len(), AST_STACK, |i| {
        let (path, (file_id, pre)) = files[i]
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()?;
        let (pkg, _) = dirs
            .iter()
            .enumerate()
            .filter(|(_, d)| path.starts_with(d))
            .max_by_key(|(_, d)| d.components().count())?;
        let metas = metas.get(pkg)?;
        let includes = included(&path, &pre.sum.includes);
        let module = (path != entry)
            .then(|| file_module(&dirs[pkg], &path))
            .unwrap_or_default();
        let clashes = dep_clashes(&pre, &path, file_id, &module, metas, pkg == 0);
        let (program, mut diags) = cached_parse(&path, file_id, pre, &includes, incr, ext, metas);
        diags.extend(clashes);
        let mut parsed = settle(&path, file_id, program, diags, metas, keep);
        parsed.includes = includes.into_iter().map(|(p, _)| p).collect();
        Some((path, parsed))
    })
    .into_iter()
    .flatten()
    .collect();
    READ.with(|r| {
        let mut r = r.borrow_mut();
        r.extend(parsed.iter().flat_map(|(_, p)| p.includes.iter().cloned()));
    });
    PARSED.with(|p| p.borrow_mut().extend(parsed));
}

/// The module path of `file` below its package directory `dir` (empty:
/// the root file).
pub(crate) fn file_module(dir: &Path, file: &Path) -> Vec<String> {
    let rel = file.strip_prefix(dir).unwrap_or(file).with_extension("");
    let segs: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    if segs.len() == 1 && (segs[0] == "lib" || segs[0] == "main") {
        return Vec::new();
    }
    segs
}

/// A path written `x::..` in a module with a child module `x`, where `x`
/// also names one of the package's dependencies: both would be meant.
/// Not at the root of the package being built, whose modules may not
/// take a dependency's name at all (`check_dependency_names`).
pub(crate) fn dep_clashes(
    pre: &Prepassed,
    path: &Path,
    file_id: u32,
    module: &[String],
    metas: &MetaCtx,
    built: bool,
) -> Vec<NabiDiagnostic> {
    fn mods<'a>(items: &'a [nabic_ast::Item], out: &mut Vec<&'a str>) {
        for item in items {
            if let Item::Mod(m) = item {
                out.push(&m.name);
                mods(m.items.as_deref().unwrap_or_default(), out);
            }
        }
    }
    let mut names = Vec::new();
    mods(&pre.sum.skeleton, &mut names);
    let dep = |n: &str| metas.deps.iter().find(|(d, _)| d == n);
    if !names.iter().any(|n| dep(n).is_some()) {
        return Vec::new();
    }
    let (tokens, _) = Lexer::new(&pre.source, path.display().to_string()).lex_all();
    // Each token's module (index into `regions`): the file's, or an
    // inline `mod x { .. }`'s; each module's path and child modules.
    let mut regions: Vec<(Vec<String>, Vec<&str>)> = vec![(module.to_vec(), Vec::new())];
    let mut at = vec![0; tokens.len()];
    let mut open: Vec<(usize, usize)> = vec![(0, 0)];
    let mut pending = None;
    let mut depth = 0;
    for (i, t) in tokens.iter().enumerate() {
        let cur = open.last().map_or(0, |o| o.0);
        at[i] = cur;
        match &t.kind {
            K::Mod => {
                if let Some(K::Ident(n)) = tokens.get(i + 1).map(|t| &t.kind) {
                    regions[cur].1.push(n);
                    if tokens.get(i + 2).map(|t| &t.kind) == Some(&K::LBrace) {
                        let mut p = regions[cur].0.clone();
                        p.push(n.clone());
                        regions.push((p, Vec::new()));
                        pending = Some((i + 2, regions.len() - 1));
                    }
                }
            }
            K::LBrace => {
                depth += 1;
                if let Some((_, r)) = pending.take_if(|(j, _)| *j == i) {
                    open.push((r, depth));
                }
            }
            K::RBrace => {
                if open.len() > 1 && open.last().is_some_and(|o| o.1 == depth) {
                    open.pop();
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    let mut in_use = false;
    for (i, t) in tokens.iter().enumerate() {
        let prev = i.checked_sub(1).map(|j| &tokens[j].kind);
        match &t.kind {
            K::Use => in_use = true,
            K::Semi => in_use = false,
            _ => {}
        }
        let K::Ident(n) = &t.kind else {
            continue;
        };
        let rooted = tokens.get(i + 1).map(|t| &t.kind) == Some(&K::ColonColon)
            && !t.raw
            && !matches!(prev, Some(K::ColonColon | K::Dot | K::QuestionDot))
            && (!in_use || matches!(prev, Some(K::Use)));
        let (region, children) = &regions[at[i]];
        if !rooted || (built && at[i] == 0 && module.is_empty()) || !children.contains(&n.as_str())
        {
            continue;
        }
        let Some((_, to)) = dep(n) else {
            continue;
        };
        let own: Vec<&str> = std::iter::once("pkg")
            .chain(region.iter().map(String::as_str))
            .chain([n.as_str()])
            .collect();
        let own = own.join("::");
        let what = if to == n {
            format!("the dependency `{n}`")
        } else {
            format!("the dependency `{n}` (package `{to}`)")
        };
        let mut span = t.span;
        span.file = file_id;
        out.push(
            NabiDiagnostic::error(
                Code::AMBIGUOUS_NAME,
                span,
                format!("`{n}` is ambiguous: it names {what} and the module `{own}`"),
            )
            .with_help(format!(
                "write `::{n}::..` for the dependency or `{own}::..` for the module, or give \
                 the dependency another name in nabi.nabion (a key like `\"{n}_dep\"` with \
                 `package:` naming its package)"
            )),
        );
    }
    out
}

/// The files `includes` (`Summary::includes`) of the file at `path`
/// name, and the key part of their contents.
pub(crate) fn included(path: &Path, includes: &[String]) -> Vec<(PathBuf, Vec<u8>)> {
    let file = path.display().to_string();
    includes
        .iter()
        .map(|lit| {
            let p = nabic_parser::include_path(&file, lit);
            let data = std::fs::read(&p).map_or_else(
                |_| b"-".to_vec(),
                |d| format!("+{:016x}", incr::key(&[&d])).into_bytes(),
            );
            (p, data)
        })
        .collect()
}

/// The real parse of `file_id`: the pre-pass's when `reusable`, else with
/// the package's tables; from the incremental store when present and its
/// includes are unchanged.
pub(crate) fn cached_parse(
    path: &Path,
    file_id: u32,
    pre: Prepassed,
    includes: &[(PathBuf, Vec<u8>)],
    incr: Option<(&incr::Incr, &incr::Ids)>,
    ext: &ExtOps,
    metas: &MetaCtx,
) -> (nabic_ast::Program, Vec<NabiDiagnostic>) {
    let ext = &*ext.in_package(&metas.package);
    let file = path.display().to_string();
    let source = pre.source.as_bytes();
    let inc: Vec<u8> = includes
        .iter()
        .flat_map(|(p, k)| [p.display().to_string().as_bytes(), b"\0", k].concat())
        .collect();
    if reusable(&pre, ext, metas) {
        let k = incr::key(&[b"plain", file.as_bytes(), source, &inc]);
        let cached = match pre.program {
            Some(_) => None,
            None => incr.and_then(|(i, ids)| i.parse.get(k, ids)),
        };
        if let Some((i, _)) = incr {
            i.count_parse(cached.is_some());
        }
        let program = cached.unwrap_or_else(|| {
            let program = pre
                .program
                .unwrap_or_else(|| plain_parse(&file, &pre.source, file_id, |_, _, _| ()).0);
            if let Some((i, ids)) = incr {
                i.parse.put(k, &program, ids);
            }
            program
        });
        return (program, Vec::new());
    }
    let tables = metas.tables_key(ext).to_le_bytes();
    let k = incr::key(&[b"parse", file.as_bytes(), source, &tables, &inc]);
    let cached = incr.and_then(|(i, ids)| i.parse.get(k, ids));
    if let Some((i, _)) = incr {
        i.count_parse(cached.is_some());
    }
    cached.unwrap_or_else(|| {
        let parsed = parse_file(&pre.source, file, file_id, ext, metas);
        if let Some((i, ids)) = incr {
            i.parse.put(k, &parsed, ids);
        }
        parsed
    })
}

/// Check `#[cfg]` predicates (before filtering drops what they guard),
/// filter, check other attributes, and copy the file if `keep`.
pub(crate) fn settle(
    path: &Path,
    file_id: u32,
    program: nabic_ast::Program,
    mut diags: Vec<NabiDiagnostic>,
    metas: &MetaCtx,
    keep: bool,
) -> Parsed {
    attrs::check_cfg(&program.attrs, &program.items, &metas.table, &mut diags);
    let items = cfg::filter_items(program.items, &metas.cfg);
    attrs::check(&program.attrs, &items, &metas.table, &mut diags);
    let no_implicit = program.attrs.iter().any(|a| a.path == ["no_implicit_nabi"]);
    let kept = keep.then(|| SourceFile {
        path: path.to_path_buf(),
        file_id,
        package: String::new(),
        module: Vec::new(),
        attrs: program.attrs,
        items: items.clone(),
        stmts: program.stmts.clone(),
        expanded: program.expanded,
        template_names: metas.table.names_in(&path.display().to_string()),
    });
    Parsed {
        items,
        stmts: program.stmts,
        no_implicit,
        kept,
        diags,
        includes: Vec::new(),
    }
}

/// Lex and parse `source`, file `file_id`, with the build's extension
/// operators, the package's keywords and meta blocks, and cfg.
pub(crate) fn parse_file(
    source: &str,
    file: String,
    file_id: u32,
    ext: &ExtOps,
    metas: &MetaCtx,
) -> (nabic_ast::Program, Vec<NabiDiagnostic>) {
    let ext = &*ext.in_package(&metas.package);
    let lexer = Lexer::new(source, file.clone()).with_ext_ops(ext.symbols());
    let (mut tokens, mut diags) = lexer.lex_all();
    for t in &mut tokens {
        t.span.file = file_id;
    }
    // The lexer's own diagnostics and their fixes too.
    for d in &mut diags {
        if let Some(p) = d.primary.as_mut().filter(|p| p.span.file == 0) {
            p.span.file = file_id;
        }
        for f in d.fixes.iter_mut().filter(|f| f.span.file == 0) {
            f.span.file = file_id;
        }
    }
    apply_aliases(&mut tokens, &metas.aliases);
    apply_keywords(&mut tokens, &metas.keywords, &mut diags);
    let (program, mut parse_diags) = parse_program_meta(
        &tokens,
        file,
        &metas.package,
        ext,
        &metas.table,
        &metas.literals,
        Some(&metas.cfg),
    );
    diags.append(&mut parse_diags);
    (program, diags)
}

/// `prec`'s extension tokens, with whose tokens each package sees: the
/// one being built (and `lib`) its `--extern`s, a prebuilt one those it
/// was built with.
pub(crate) fn ext_ops(
    prec: &PrecTable,
    config: &CompilerConfig,
    prebuilt: &[Prebuilt],
    lib: Option<&str>,
) -> ExtOps {
    let mut ext = prec.ext_ops();
    let externs: Vec<String> = config.externs.iter().map(|(n, _)| n.clone()).collect();
    for p in prebuilt {
        ext.deps
            .insert(p.nabihir.name.clone(), p.nabihir.deps.clone());
    }
    if let Some(lib) = lib {
        ext.deps.insert(lib.to_owned(), externs.clone());
    }
    ext.deps.insert(String::new(), externs);
    ext
}

/// The operator tokens the files of the package with root files `roots`
/// lex (`nabi fmt`, the language server): its own registrations and the
/// `pub` ones of `config.externs`, from sources or `.nabihir`.
#[must_use]
pub fn operator_tokens(roots: &[PathBuf], config: &CompilerConfig) -> Vec<String> {
    // The standard libraries register no extension token.
    let mut config = config.clone();
    config.bootstrap_files.clear();
    let mut diags = Vec::new();
    let (config, prebuilt) = load_prebuilt(&config, &mut diags);
    let mut files = SourceMap::new();
    let mut metas = PrePass::default();
    let mut prec = build_prec_table(&lib_roots(&config), &mut diags, &mut metas, &mut files);
    for root in roots {
        let sub_dir = root.parent();
        let mut pre = prepass_parse(
            vec![(root.clone(), None, sub_dir.map(Path::to_path_buf))],
            &mut files,
            None,
        );
        let mut own = Vec::new();
        prepass_tree(root, sub_dir, &[], &mut pre, &mut metas, &mut own);
        for (table, _) in own {
            for e in table.entries() {
                let _ = prec.merge(e.clone());
            }
        }
    }
    ext_ops(&prec, &config, &prebuilt, None)
        .in_package("")
        .symbols()
}
