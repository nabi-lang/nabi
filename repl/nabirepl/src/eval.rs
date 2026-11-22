//! Building, running and keeping a cell.

#[allow(clippy::wildcard_imports)]
use crate::*;

impl Repl {
    /// `:add <args>`: `nabi install --env` in an environment's session, else
    /// `nabi add`; then prebuild `nabi deps`.
    pub(crate) fn add_dep(&mut self, args: &str) {
        let Some(nabi) = nabic_core::handoff_var("NABI") else {
            eprintln!("`:add` needs a session started by `nabi repl`");
            return;
        };
        let env: Vec<String> = nabic_core::handoff_var("NABI_ENV")
            .and_then(|e| e.into_string().ok())
            .filter(|e| !e.is_empty())
            .map(|e| vec!["--env".to_owned(), e])
            .unwrap_or_default();
        let run = |cmd: &str, extra: &[&str]| {
            Command::new(&nabi)
                .arg(cmd)
                .args(&env)
                .args(extra)
                .stderr(std::process::Stdio::inherit())
                .output()
                .ok()
                .filter(|o| o.status.success())
        };
        let words: Vec<&str> = args.split_whitespace().collect();
        if run(if env.is_empty() { "add" } else { "install" }, &words).is_none() {
            return;
        }
        let Some(out) = run("deps", &[]) else { return };
        // `name=<lib.nabi>` lines, each followed by `--extern-feature` lines,
        // then native nabic arguments under `# native ..` lines.
        let text = String::from_utf8_lossy(&out.stdout);
        let o = &mut self.opts;
        let mut deps: Vec<(String, PathBuf)> = Vec::new();
        let mut features: Vec<(String, String)> = Vec::new();
        for l in text.lines().filter(|l| !l.starts_with('#')) {
            if let Some((n, f)) = l
                .strip_prefix("--extern-feature=")
                .and_then(|v| v.split_once('='))
            {
                features.push((n.to_owned(), f.to_owned()));
            } else if let Some(d) = l.strip_prefix("-L") {
                push_new(&mut o.lib_dirs, PathBuf::from(d));
            } else if let Some(f) = l.strip_prefix("--native-runtime=") {
                push_new(&mut o.runtime_files, PathBuf::from(f));
            } else if let Some((kind, name)) = l.strip_prefix("-l").and_then(|k| k.split_once('='))
            {
                push_new(&mut o.libs, (name.to_owned(), kind == "static"));
            } else if let Some((n, p)) = l.split_once('=') {
                deps.push((n.to_owned(), PathBuf::from(p)));
            }
        }
        let build = nabic_interface::DepBuild {
            bootstrap: &self.bootstrap,
            cfg: &[],
            features: &features,
            deps: &[],
            opts: &self.opts,
            target: nabic_interface::TargetSpec::host(),
            dir: &self.deps_dir,
            quiet: true,
        };
        match build.prebuild(&deps) {
            Ok(externs) => self.externs = externs,
            Err(e) => eprintln!("error: {e}"),
        }
    }

    /// Add each official package `cell` uses that the session lacks, as
    /// `:add` does (`nabi __official <root>` prints the package and the
    /// requirement, or nothing).
    pub(crate) fn add_official(&mut self, cell: &str) {
        let Some(nabi) = nabic_core::handoff_var("NABI") else {
            return;
        };
        for code in nabic_tools::roots::missing_roots([cell]) {
            if self.externs.iter().any(|(n, _)| *n == code) {
                continue;
            }
            let Some(out) = Command::new(&nabi)
                .args(["__official", &code])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
            else {
                continue;
            };
            let Some((name, req)) = out.split_once(' ') else {
                continue;
            };
            self.add_dep(&format!("-q {name}@{req}"));
            if self.externs.iter().any(|(n, _)| *n == code) {
                nabic_core::term::status("Added", &format!("{name} {req} (official)"));
            }
        }
    }

    pub(crate) fn item(&mut self, cell: &str) -> bool {
        // A new definition replaces the old one of the same name; a cell
        // may define several, so dependents can be redefined with it.
        let source: Arc<str> = Arc::from(cell);
        let new: Vec<Item> = items_of(cell)
            .into_iter()
            .map(|(key, at, end)| Item {
                key,
                text: cell[at..end].to_owned(),
                cell: self.cell_no,
                source: Arc::clone(&source),
                at,
            })
            .collect();
        let mut next = self.state.clone();
        next.items
            .retain(|old| !new.iter().any(|n| replaces(n, old)));
        next.items.extend(new.iter().cloned());
        self.defining = true;
        let r = self.compile(&next, &self.session(&next, "", None), false);
        self.defining = false;
        let Some(r) = r else { return false };
        // Codegen too, like `check`: it rejects what cannot be lowered.
        if let Err(d) = r.check_codegen(&self.opts) {
            r.report(&d);
            return false;
        }
        for key in new.iter().filter_map(|n| n.key.as_deref()) {
            self.forget(key);
            // A user's `it` wins over the session's.
            if key.split(' ').nth(1) == Some("it") {
                self.bindings.retain(|b| !b.is_it);
            }
        }
        self.state = next;
        true
    }

    /// Bindings and statics the redefinition of `key` makes stale.
    pub(crate) fn forget(&mut self, key: &str) {
        // A binding of a redefined type would be read with the new
        // layout: forget it.
        let old = Some(key).filter(|k| {
            !k.starts_with("fn ")
                && !k.starts_with("impl ")
                && self.state.items.iter().any(|o| o.key.as_deref() == Some(k))
        });
        if let Some(ty) = old.and_then(|k| k.split(' ').nth(1)) {
            self.bindings.retain(|b| {
                let keep = !b
                    .ty
                    .split(|c: char| !(c.is_alphanumeric() || c == '_'))
                    .any(|w| w == ty);
                if !keep {
                    eprintln!("note: `{}` is gone: its type `{ty}` was redefined", b.name);
                } else if b.holds_any {
                    // It may hold an old `{ty}` the new tag would claim;
                    // telling them apart needs the value's tag.
                    eprintln!(
                        "note: `{}` is gone: it holds an `any` value and `{ty}` was redefined",
                        b.name
                    );
                    return false;
                }
                keep
            });
        }
        // A redefined static starts over: the next cell defines it anew.
        if let Some(name) = key.strip_prefix("static ") {
            let lib = |s: &str| {
                nabic_core::in_toolchain(s)
                    || self
                        .externs
                        .iter()
                        .any(|(n, _)| s.starts_with(&format!("{n}::")))
            };
            self.statics
                .retain(|s| lib(s) || s.rsplit("::").next() != Some(name));
        }
    }

    /// Cell source: items, binding slots, `body`, and the store of `show`
    /// to the host. `keep` adds the slot stores to the HIR.
    pub(crate) fn session(&self, state: &State, body: &str, show: Option<&str>) -> String {
        let mut src = String::new();
        for item in &state.items {
            src.push_str(&item.text);
            src.push('\n');
        }
        // Every type earlier cells stored in `any` slots: stored here too.
        for (i, t) in self.any_types.iter().enumerate() {
            let _ = writeln!(src, "fn __nabi_repl_any_{i}(x: {t}) -> any {{ return x; }}");
        }
        let _ = writeln!(src, "extern \"C\" {{\n    static mut {SHOWN}: str;");
        for b in &self.bindings {
            let _ = writeln!(src, "    static mut {}: {};", b.sym, b.ty);
        }
        src.push_str("}\n");
        for b in &self.bindings {
            let _ = writeln!(src, "let {} = {};", b.name, b.sym);
        }
        // The cell runs in a block of its own; none without a body (a
        // loaded program's `fn main` is the entry then).
        if body.is_empty() && show.is_none() {
            return src;
        }
        src.push_str("{\n");
        src.push_str(body);
        src.push('\n');
        if let Some(text) = show {
            let _ = writeln!(src, "{SHOWN} = {text};");
        }
        // `;`: a cell ending in a value is a statement, not E0067.
        src.push_str("};\n");
        src
    }

    /// Build `cell` as a library, run it in the host, keep its top-level
    /// bindings; the `Display` (or `Debug`) of `show` (`eval`).
    pub(crate) fn run_once(
        &mut self,
        cell: &str,
        show: Option<&str>,
        debug: bool,
    ) -> Result<Option<String>, ()> {
        self.run_cell(cell, show, debug, &[]).map_err(|f| {
            if let Fail::Unkept(_, e) = f {
                eprintln!("error: {e}");
            }
        })
    }

    /// `run_once`; a binding later cells can't use fails the cell before it
    /// runs, unless it is in `may_drop` (dropped with a note).
    pub(crate) fn run_cell(
        &mut self,
        cell: &str,
        show: Option<&str>,
        debug: bool,
        may_drop: &[String],
    ) -> Result<Option<String>, Fail> {
        self.start_host();
        let mut lap = Lap::start(self.timing);
        // A bare expression shows its `Display`, else its `Debug`, else its
        // type; a trailing `?` (`debug`) its `Debug`, else its type. The
        // first build's errors say which traits the type lacks.
        let mut fmt = show.map(|_| match (debug, self.rich) {
            (true, _) => Fmt::Debug,
            (false, true) => Fmt::Rich,
            (false, false) => Fmt::Display,
        });
        let mut r = loop {
            let text = show.zip(fmt).map(|(n, f)| f.text(n));
            let src = self.session(&self.state, cell, text.as_deref());
            let lacks = std::cell::Cell::new(None);
            let claim = |r: &CompileResult, file: u32| {
                let next = fmt.and_then(|f| unformattable(&r.errors, &src, file, f));
                lacks.set(next);
                next.is_some()
            };
            match self.front(&self.state, &src, false, claim) {
                Ok(r) => break r,
                Err(true) => fmt = lacks.get(),
                Err(false) => return Err(Fail::Reported),
            }
        };
        lap.mark("front end");
        let it = show.is_some() && !self.defines("it");
        let new = self
            .keep(&mut r, may_drop, it)
            .map_err(|(n, e)| Fail::Unkept(n, e))?;
        let edited = new.is_some();
        let new = new.unwrap_or_default();
        lap.mark("bindings");
        self.built += 1;
        let mut opts = self.opts.clone();
        // `-g` cells compile everything (their text carries
        // locations, which differ from cell to cell anyway).
        let known = if opts.debug {
            HashMap::default()
        } else {
            self.known
                .iter()
                .map(|(k, (h, _))| (k.clone(), *h))
                .collect()
        };
        opts.repl = Some(ReplCell {
            gc_tables: self.gc_tables.clone(),
            known,
            define: new.iter().map(|b| b.sym.clone()).collect(),
            statics: self.statics.iter().cloned().collect(),
        });
        // MIR lowering reads the front end's per-thread state: on the
        // compile thread too.
        let code = nabic_tools::analysis::isolate(move || {
            if edited {
                r.relower();
            }
            let code = r.cell_code(&opts);
            (r, code)
        });
        let (r, code) = code.map_err(|e| {
            SourceMap::new().report(&nabic_tools::analysis::ice(&e));
            Fail::Reported
        })?;
        let (ir, out) = match code {
            Ok(x) => x,
            Err(d) => {
                r.report(&d);
                return Err(Fail::Reported);
            }
        };
        lap.mark("codegen");
        lap.note(&format!(
            "functions: {} compiled, {} from earlier cells; object: {} bytes",
            out.fns.len(),
            out.reused,
            ir.len()
        ));
        self.link_archives(&r);
        let value = self.load(&ir);
        lap.mark("jit + run");
        let value = value.ok_or(Fail::Reported)?;
        self.gc_tables = out.gc_tables;
        self.learn(out.fresh, out.fns);
        self.bindings
            .retain(|b| !new.iter().any(|n| n.name == b.name));
        self.bindings.extend(new);
        if let Some(m) = r.modules.first() {
            let defined = m.mir.statics.iter().filter(|s| !s.is_extern);
            self.statics.extend(defined.map(|s| s.name.clone()));
            // Names a later cell writes at its top level: visible there.
            let visible = |t: &&String| {
                t.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == ':'))
                    .all(|p| p.is_empty() || nabic_typeck::type_visible(&m.hir, p, ""))
            };
            let stored = m.hir.any_types.iter().filter(visible);
            self.any_types.extend(stored.cloned());
        }
        // A rich value went out as `display_data` already.
        Ok(show
            .filter(|_| fmt != Some(Fmt::Rich))
            .and_then(|n| binding_ty(&r, n))
            .filter(|ty| ty != "()")
            .map(|_| value))
    }

    /// Adds slots for the cell's new top-level `let`s (last of each name)
    /// and, at the block's end, stores of every binding back to its slot.
    /// An unusable binding is an error unless in `may_drop`. `it`: the
    /// shown value becomes `it`, unless the cell binds `it` itself. `None`:
    /// HIR unchanged; else MIR must be lowered again.
    pub(crate) fn keep(
        &self,
        r: &mut CompileResult,
        may_drop: &[String],
        it: bool,
    ) -> Result<Option<Vec<Binding>>, (String, String)> {
        let Some(hir) = r.modules.first_mut().map(|m| &mut m.hir) else {
            return Ok(None);
        };
        // Where the cell's block starts in the session source.
        let at = u32::try_from(self.session(&self.state, "", None).len()).unwrap_or(u32::MAX);
        let Some((f, outer, block)) = top_fns(hir)
            .into_iter()
            .find_map(|f| cell_scope(&hir.functions[f], at).map(|(o, b)| (f, o, b)))
        else {
            return Ok(None);
        };
        // The `let`s before the cell's block load the old bindings.
        let outer: Vec<LocalId> = outer
            .iter()
            .filter_map(|s| match s {
                HirStmt::Let { local, .. } => Some(*local),
                _ => None,
            })
            .collect();
        let locals = &hir.functions[f].locals;
        let local = |id: LocalId| locals.iter().find(|l| l.id == id);
        let named = |id: LocalId| {
            local(id)?
                .name
                .as_deref()
                .filter(|n| !n.starts_with("__") && *n != "_")
        };
        let lets = block.stmts.iter().filter_map(|s| match s {
            HirStmt::Let { local, .. } => Some(*local),
            _ => None,
        });
        let mut new: Vec<(Binding, LocalId)> = Vec::new();
        for (i, id) in lets.enumerate() {
            let Some(l) = local(id) else { continue };
            let shown = it && l.name.as_deref() == Some(VALUE);
            let name = match named(id) {
                Some(n) => n,
                // A unit or unkeepable value leaves `it` as it was.
                None if shown
                    && !matches!(l.ty, HirTy::Unit | HirTy::Never)
                    && !new.iter().any(|(b, _)| b.name == "it") =>
                {
                    "it"
                }
                None => continue,
            };
            // Full paths, so later cells name the type without a `use`.
            let ty = ty_path(hir, &l.ty);
            // Closures, unresolved types, and types the session's top level
            // can't see can't be kept.
            let why = if ty.contains('{') || ty.starts_with("closure") {
                Some("its type has no name".to_owned())
            } else {
                ty.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == ':'))
                    .find(|p| !nabic_typeck::type_visible(hir, p, ""))
                    .map(|p| format!("`{p}` is private to its module"))
            };
            if let Some(why) = why {
                if shown {
                    continue;
                }
                let msg = format!(
                    "`{name}: {}` cannot be kept for later cells ({why})",
                    ty_name(hir, &l.ty)
                );
                if !may_drop.iter().any(|n| n == name) {
                    return Err((name.to_owned(), msg));
                }
                eprintln!("note: {msg}");
                continue;
            }
            let holds_any = holds_any(hir, &l.ty, &mut Vec::new());
            new.retain(|(b, _)| b.name != name);
            let sym = format!("__nabi_repl_{}_{i}", self.built + 1);
            let name = name.to_owned();
            new.push((
                Binding {
                    name,
                    sym,
                    ty,
                    shown_ty: ty_name(hir, &l.ty),
                    holds_any,
                    is_it: shown,
                },
                id,
            ));
        }
        // Every binding goes back (a `let` can be assigned);
        // only the ones the cell assigns would need to.
        let old = self
            .bindings
            .iter()
            .filter(|b| !new.iter().any(|(n, _)| n.name == b.name))
            .filter_map(|b| {
                let id = outer.iter().rev().find(|&&id| named(id) == Some(&b.name))?;
                Some((b.sym.clone(), *id))
            });
        let stores: Vec<HirStmt> = old
            .chain(new.iter().map(|(b, id)| (b.sym.clone(), *id)))
            .filter_map(|(name, id)| {
                let ty = local(id)?.ty.clone();
                let kind = HirExprKind::Local(id);
                let span = nabic_diagnostic::Span::DUMMY;
                Some(HirStmt::StaticAssign {
                    name,
                    value: HirExpr { kind, ty, span },
                })
            })
            .collect();
        let slots: Vec<HirStaticDef> = new
            .iter()
            .filter_map(|(b, id)| {
                Some(HirStaticDef {
                    name: b.sym.clone(),
                    ty: local(*id)?.ty.clone(),
                    init: ConstVal::Unit,
                    is_mut: true,
                    is_extern: true,
                    dll_import: false,
                    init_fn: None,
                    span: nabic_diagnostic::Span::DUMMY,
                })
            })
            .collect();
        hir.statics.extend(slots);
        let mut stores = Some(stores);
        let mut store = |scope: &mut HirBlock| {
            let Some(HirStmt::Expr(e)) = scope.stmts.iter_mut().find(|s| is_cell(s, at)) else {
                return;
            };
            let (HirExprKind::Block(b), Some(stores)) = (&mut e.kind, stores.take()) else {
                return;
            };
            if let Some(t) = b.trailing.take() {
                b.stmts.push(HirStmt::Expr(t));
                e.ty = HirTy::Unit;
            }
            b.stmts.extend(stores);
        };
        if let Some(body) = hir.functions[f].body.as_mut() {
            store(body);
            nabic_hir::each_block_mut(body, &mut |e| {
                if let HirExprKind::Block(b) = &mut e.kind {
                    store(b);
                }
            });
        }
        Ok(Some(new.into_iter().map(|(b, _)| b).collect()))
    }
}

impl Repl {
    /// Front end of `src`, the session source of `state`; errors unless
    /// `quiet`. Diagnostics name the cell `<repl>:line:col`, an earlier
    /// cell `<repl cell N>:line:col`, a loaded program its own file.
    pub(crate) fn compile(&self, state: &State, src: &str, quiet: bool) -> Option<CompileResult> {
        self.front(state, src, quiet, |_, _| false).ok()
    }

    /// `compile`; a failed build that `mine` claims (given the session
    /// source's file id) is `Err(true)`, unreported.
    pub(crate) fn front(
        &self,
        state: &State,
        src: &str,
        quiet: bool,
        mine: impl Fn(&CompileResult, u32) -> bool,
    ) -> Result<CompileResult, bool> {
        let (s, path) = self.compiler();
        // Added first so the views are in place while HIR lowering
        // writes caller locations (`#[track_caller]`).
        let mut map = SourceMap::new();
        let session = map.add(path.display().to_string(), src.to_owned());
        let regions = self.regions(state, src, &mut map);
        for r in &regions {
            let (line, col) = line_col(src, r.off);
            let lines = src[r.off..r.off + r.len].matches('\n').count();
            let last = line + u32::try_from(lines).map_err(|_| false)?;
            let at = line_col(&r.text, r.at);
            map.view(
                session,
                r.path.clone(),
                (line, col + 1),
                last,
                (at.0, at.1 + 1),
            );
        }
        let (text, at) = (src.to_owned(), path.clone());
        let r = match nabic_tools::analysis::isolate(move || s.compile_source(&text, &at, map)) {
            Ok(r) => r,
            Err(e) => {
                SourceMap::new().report(&nabic_tools::analysis::ice(&e));
                return Err(false);
            }
        };
        if !r.has_errors() {
            return Ok(r);
        }
        // A diverging expression cell (`panic(..)`) has no value to show:
        // `eval` runs it again as a statement, which reports the rest.
        let never = format!("`{VALUE}`: the initializer never produces a value");
        if r.errors.iter().any(|d| d.message.contains(&never)) {
            self.diverged.set(true);
            return Err(false);
        }
        if mine(&r, session) {
            return Err(true);
        }
        if !quiet {
            let path = path.display().to_string();
            // The shown value's errors come with the cell's own.
            let user = r.errors.iter().filter(|d| !in_show(d, src, session));
            for d in user {
                let (mut d, owner) = localize(d, session, &path, &regions);
                if let Some(owner) = owner.filter(|_| self.defining) {
                    d.notes.push(format!(
                        "help: {owner} no longer compiles with this definition: redefine it \
                         in this cell too, or `:reset` to start over"
                    ));
                }
                r.report(&self.package_hint(d));
            }
        }
        Err(false)
    }

    /// `d`, with a hint when the name it cannot find is one the package
    /// the session runs in (not loaded) defines.
    pub(crate) fn package_hint(&self, d: NabiDiagnostic) -> NabiDiagnostic {
        let Some(dir) = &self.package else { return d };
        let name = d
            .message
            .strip_prefix("cannot find ")
            .filter(|m| m.ends_with("` in this scope"))
            .and_then(|m| m.split('`').nth(1))
            .map(str::to_owned);
        match name {
            Some(n) if defined_in(dir, &n) => d.with_help(format!(
                "`{n}` is defined in this package; load this package: `nabi repl .`"
            )),
            _ => d,
        }
    }

    /// A compiler session for session sources, and the path they stand at.
    pub(crate) fn compiler(&self) -> (Session, PathBuf) {
        let mut s = Session::new();
        for b in &self.bootstrap {
            s.add_bootstrap_file(b.clone());
        }
        for (n, p) in &self.externs {
            s.add_extern(n.clone(), p.clone());
        }
        let path = self
            .program
            .as_ref()
            .map_or_else(|| self.tmp.path().join("repl.nabi"), |p| p.session.clone());
        s.set_anonymous(self.program.as_ref().is_none_or(|p| p.anonymous));
        s.set_cache_library(true);
        (s, path)
    }

    /// The session source with `body` as the cell, and where `body`
    /// starts in it.
    pub fn source(&self, body: &str) -> (String, usize) {
        let src = self.session(&self.state, body, None);
        let at = if body.is_empty() {
            src.len()
        } else {
            src.len() - body.len() - "\n};\n".len()
        };
        (src, at)
    }

    /// Check a session source (`source`) for the tools, reporting
    /// nothing: the analysis and the path its spans name.
    pub fn analyze(&self, src: &str) -> (Analysis, String) {
        let (s, path) = self.compiler();
        let a =
            nabic_tools::analysis::analyze(s, src, &path, &nabic_tools::lint::Levels::default());
        (a, path.display().to_string())
    }

    /// Where the user's text is in `src`, the session source of `state`:
    /// the cell being built, earlier cells' items, and the loaded program's
    /// items.
    pub(crate) fn regions(&self, state: &State, src: &str, map: &mut SourceMap) -> Vec<Region> {
        let program = self.program.as_ref();
        let shown = |map: &mut SourceMap, p: &Program, off: usize, len: usize, at: usize| Region {
            off,
            len,
            file: map.add(p.path.clone(), p.text.to_string()),
            path: p.path.clone(),
            text: Arc::clone(&p.text),
            at,
            owner: None,
        };
        let now = cell_path(self.cell_no);
        let mut regions = Vec::new();
        // Items, which the session source starts with.
        let mut off = 0;
        for it in &state.items {
            if !src[off..].starts_with(it.text.as_str()) {
                continue;
            }
            if !it.cell.is_none() {
                let (file, owner) = if self.defining && it.cell == self.cell_no {
                    (map.add(REPL_FILE.to_owned(), it.source.to_string()), None)
                } else {
                    let owner = format!("`{}` from cell {}", it.name(), it.cell);
                    (
                        map.add(cell_path(it.cell), it.source.to_string()),
                        Some(owner),
                    )
                };
                regions.push(Region {
                    off,
                    len: it.text.len(),
                    file,
                    path: cell_path(it.cell),
                    text: Arc::clone(&it.source),
                    at: it.at,
                    owner,
                });
            }
            off += it.text.len() + 1;
        }
        // An expression cell is `{stmts}let VALUE = ({last});`, `{stmts}let
        // VALUE = ({expr});` for `{expr}?`; the shown value's store follows.
        let body = &src[..src.rfind(&format!("\n{SHOWN} = ")).unwrap_or(src.len())];
        let (stmts, last) = split_last(&self.input);
        let expr = last.trim_end().strip_suffix('?').map(str::trim_end);
        let found = [Some(last), expr]
            .into_iter()
            .flatten()
            .filter(|t| !t.is_empty())
            .find_map(|t| Some((body.rfind(t)?, t)));
        let stmts_off = found
            .and_then(|(o, _)| body[..o].rfind(stmts))
            .filter(|_| !stmts.is_empty());
        let cell = [
            stmts_off.map(|o| (o, stmts, 0)),
            found.map(|(o, t)| (o, t, stmts.len())),
        ];
        for (off, part, at) in cell.into_iter().flatten() {
            let len = part.len();
            regions.push(match program.and_then(|p| Some((p, p.cell?))) {
                Some((p, cell)) => shown(map, p, off, len, cell + at),
                None => Region {
                    off,
                    len,
                    file: map.add(REPL_FILE.to_owned(), self.input.clone()),
                    path: now.clone(),
                    text: Arc::from(self.input.as_str()),
                    at,
                    owner: None,
                },
            });
        }
        if let Some(p) = program {
            for (item, at) in &p.items {
                if let Some(off) = src.find(item.as_str()) {
                    regions.push(shown(map, p, off, item.len(), *at));
                }
            }
        }
        regions
    }
}
