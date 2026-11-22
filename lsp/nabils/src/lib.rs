//! Language server over stdio (JSON-RPC 2.0 with `Content-Length`
//! framing, `nabils_protocol`), full-text sync.
//!
//! Checks (`analysis::check`: front end and lints, no codegen) run in the
//! background, one at a time, on the latest text; queries answer at once
//! from the last check that type-checked, through the edit since when it
//! does not touch the position.

mod actions;
mod definition;
mod diagnostics;
mod signature;
#[cfg(test)]
mod tests;
mod tokens;

#[allow(clippy::wildcard_imports)]
pub(crate) use crate::definition::*;
#[allow(clippy::wildcard_imports)]
pub(crate) use crate::diagnostics::*;
#[allow(clippy::wildcard_imports)]
pub(crate) use crate::tokens::*;
pub use nabils_protocol::path_to_uri;

use nabic_ast::{Expr, ExprKind, ImplItem, Item, TraitItem};
use nabic_core::HashMap;
use nabic_diagnostic::{Code, Json, NabiDiagnostic, Span};
use nabic_hir::HirExprKind;
use nabic_hir::walk::{AstVisitor, walk_item, walk_stmt};
use nabic_interface::Session;
use nabic_lexer::{Lexer, TokenKind, TokenKind as K};
use nabic_tools::analysis::{Analysis, analyze, short_name};
use nabic_tools::cursor::{
    Delta, balanced_close, ident_at, is_ident_char, locals, name_range, parse_doc, resolve_local,
};
use nabic_tools::hover::{Member, hover, indexed_member, macro_hover, member_at};
use nabic_tools::lint::Levels;
use nabils_protocol::{line_col, offset, path_uri, range, read_message, uri_path, write_message};
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// The package manifest, beside its lock `nabi.lock`.
const MANIFEST: &str = "nabi.nabion";

pub(crate) struct Doc {
    text: String,
    /// Needs a compile (changed since the last one).
    dirty: bool,
    /// Inputs of the last check, including one that reported errors.
    inputs: Vec<PathBuf>,
    /// The last compile that type-checked: hover / definition /
    /// completion keep working while the buffer has errors.
    good: Option<Analysis>,
    /// The last check, when it had type errors: what it lowered, for
    /// hover where nothing type-checked yet (or an error is newer).
    partial: Option<Analysis>,
    /// The last published diagnostics and what they came from, for
    /// code actions.
    diags: Vec<(Json, NabiDiagnostic)>,
    /// The snapshot of the last check applied: an older one that ends
    /// later is dropped.
    shown: u64,
    /// The snapshot of the last check started: past `shown` while it runs.
    asked: u64,
    /// Another input or the dependencies changed after snapshot `n`
    /// (`Some(n)`), and no later check is in: positions do not tell what
    /// `good` still knows.
    moved: Option<u64>,
}

/// What the server waits on: a client message, the end of input or a
/// finished check (its result waits in `Server::running`).
enum Event {
    Msg(Json),
    End,
    Checked,
}

/// A background check's result: the buffer, its snapshot and the check.
type Done = (String, u64, Analysis);

/// A check ready to run off the server thread.
type Job = Box<dyn FnOnce() -> Analysis + Send>;

/// Quiet time after an edit before a check starts: a burst of keystrokes
/// costs one check.
const SETTLE: Duration = Duration::from_millis(30);

/// `(dependent, dependency)` of `--extern-dep`.
type Pair = (String, String);

/// A package's dependencies as `nabic` takes them.
#[derive(Clone, Default)]
pub struct Externs {
    /// `--extern` pairs ready to check with.
    pub externs: Vec<(String, PathBuf)>,
    /// `--extern-dep` pairs: each package sees only its own.
    pub deps: Vec<(String, String)>,
}

/// The dependencies of the package in a directory (or, outside every
/// package, the environment's).
pub type Deps = Box<dyn FnMut(&Path) -> Result<Externs, String> + Send>;

struct Server<W> {
    out: W,
    bootstrap: Vec<PathBuf>,
    /// For a buffer no resolved directory covers.
    externs: Vec<(String, PathBuf)>,
    deps: Option<Deps>,
    /// `deps` by directory, until a manifest or lock changes.
    resolved: HashMap<PathBuf, Externs>,
    /// The editor's workspace folders.
    folders: Vec<PathBuf>,
    /// The client registers file watchers on request.
    watch: bool,
    levels: Levels,
    docs: HashMap<String, Doc>,
    /// Operator tokens by package directory (`operator_tokens`), until
    /// the next edit.
    ops: HashMap<PathBuf, Vec<String>>,
    /// `initialize` came.
    initialized: bool,
    shutdown: bool,
    /// Where finished background checks report.
    events: mpsc::Sender<Event>,
    /// The running background check's result, once it is done.
    running: Option<mpsc::Receiver<Done>>,
    /// Snapshots taken so far.
    seq: u64,
    /// The last buffer edit.
    edited: Option<Instant>,
}

/// Serve `input` until `exit` or EOF, linting at `levels`; exit code 0
/// after `shutdown`, else 1 (LSP spec).
pub fn serve(
    input: impl BufRead + Send + 'static,
    out: impl Write,
    bootstrap: Vec<PathBuf>,
    externs: Vec<(String, PathBuf)>,
    levels: impl Into<Levels>,
) -> i32 {
    serve_with(input, out, bootstrap, externs, levels, None)
}

/// `serve`, each buffer checked with the dependencies `deps` gives for
/// its package, resolved again when a manifest or lock changes;
/// `externs` serve the buffers outside every package.
pub fn serve_with(
    input: impl BufRead + Send + 'static,
    out: impl Write,
    bootstrap: Vec<PathBuf>,
    externs: Vec<(String, PathBuf)>,
    levels: impl Into<Levels>,
    deps: Option<Deps>,
) -> i32 {
    if std::io::stdin().is_terminal() {
        eprintln!(
            "nabils speaks the language server protocol on stdin; start it from an editor \
             (see `nabi help lsp`)"
        );
    }
    // A panic of the server loop ends it; only a compile's is isolated.
    nabic_tools::analysis::fatal_here();
    let (tx, rx) = mpsc::channel();
    let reader = tx.clone();
    std::thread::spawn(move || {
        let mut input = input;
        while let Some(msg) = read_message(&mut input) {
            if reader.send(Event::Msg(msg)).is_err() {
                break;
            }
        }
        let _ = reader.send(Event::End);
    });
    let mut server = Server {
        out,
        bootstrap,
        externs,
        deps,
        resolved: HashMap::default(),
        folders: Vec::new(),
        watch: false,
        levels: levels.into(),
        docs: HashMap::default(),
        ops: HashMap::default(),
        initialized: false,
        shutdown: false,
        events: tx,
        running: None,
        seq: 0,
        edited: None,
    };
    let mut ended = false;
    loop {
        // Wait for the next event; with a check due, only until it is.
        let due = server.due().map(|w| if ended { Duration::ZERO } else { w });
        let first = match due {
            Some(wait) if wait.is_zero() => rx.try_recv().ok(),
            Some(wait) => match rx.recv_timeout(wait) {
                Ok(e) => Some(e),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            },
            None if ended && server.running.is_none() => break,
            None => match rx.recv() {
                Ok(e) => Some(e),
                Err(_) => break,
            },
        };
        // Everything already queued first: a burst of edits costs one
        // check, and a request cancelled meanwhile is not answered.
        let batch: Vec<Event> = first.into_iter().chain(rx.try_iter()).collect();
        let cancelled: Vec<String> = batch
            .iter()
            .filter_map(|e| match e {
                Event::Msg(m) if m.get("method").as_str() == Some("$/cancelRequest") => {
                    Some(m.get("params").get("id").to_string())
                }
                _ => None,
            })
            .collect();
        for event in batch {
            match event {
                Event::Msg(msg) => {
                    let id = msg.get("id");
                    if !matches!(id, Json::Null)
                        && msg.get("method").as_str().is_some()
                        && cancelled.contains(&id.to_string())
                    {
                        server.error(id, -32800, "request cancelled".to_owned());
                        continue;
                    }
                    if let Some(code) = server.handle(&msg) {
                        return code;
                    }
                }
                Event::End => ended = true,
                Event::Checked => server.checked(),
            }
        }
        if server.due().is_some_and(|w| ended || w.is_zero()) {
            server.start_check();
        }
    }
    if !server.initialized {
        eprintln!("nabils: input ended before the editor sent `initialize`");
        return 1;
    }
    i32::from(!server.shutdown)
}

impl<W: Write> Server<W> {
    fn send(&mut self, msg: &Json) {
        write_message(&mut self.out, msg);
    }

    fn error(&mut self, id: &Json, code: i32, message: String) {
        self.send(&Json::obj([
            ("jsonrpc", Json::str("2.0")),
            ("id", id.clone()),
            (
                "error",
                Json::obj([("code", Json::num(code)), ("message", Json::str(message))]),
            ),
        ]));
    }

    fn reply(&mut self, id: &Json, result: Json) {
        self.send(&Json::obj([
            ("jsonrpc", Json::str("2.0")),
            ("id", id.clone()),
            ("result", result),
        ]));
    }

    /// Returns the exit code on `exit`.
    fn handle(&mut self, msg: &Json) -> Option<i32> {
        let method = msg.get("method").as_str().unwrap_or("");
        let id = msg.get("id");
        let params = msg.get("params");
        let uri = params
            .get("textDocument")
            .get("uri")
            .as_str()
            .unwrap_or("")
            .to_owned();
        self.initialized |= method == "initialize";
        let result = match method {
            // A reply to one of our requests (`client/registerCapability`).
            "" if !matches!(id, Json::Null) => return None,
            "initialize" => {
                self.workspace(params);
                Self::initialize_result()
            }
            "initialized" => {
                if self.watch {
                    self.watch_manifests();
                }
                return None;
            }
            "workspace/didChangeWorkspaceFolders" => {
                let event = params.get("event");
                let paths = |k: &str| match event.get(k) {
                    Json::Arr(fs) => fs
                        .iter()
                        .filter_map(|f| f.get("uri").as_str().map(uri_path))
                        .collect(),
                    _ => Vec::new(),
                };
                let (added, removed): (Vec<PathBuf>, Vec<PathBuf>) =
                    (paths("added"), paths("removed"));
                self.folders.retain(|f| !removed.contains(f));
                self.folders.extend(added);
                self.deps_changed();
                return None;
            }
            "workspace/didChangeWatchedFiles" => {
                let changed = match params.get("changes") {
                    Json::Arr(cs) => cs
                        .iter()
                        .filter_map(|c| c.get("uri").as_str())
                        .any(|u| is_manifest(&uri_path(u))),
                    _ => false,
                };
                if changed {
                    self.deps_changed();
                }
                return None;
            }
            "textDocument/didSave" => {
                if is_manifest(&uri_path(&uri)) {
                    self.deps_changed();
                }
                return None;
            }
            "shutdown" => {
                // What was typed last is checked and published first.
                self.flush();
                self.shutdown = true;
                Json::Null
            }
            "exit" => return Some(i32::from(!self.shutdown)),
            "textDocument/didOpen" | "textDocument/didChange" => {
                let text = if method == "textDocument/didOpen" {
                    params.get("textDocument").get("text")
                } else {
                    match params.get("contentChanges") {
                        Json::Arr(cs) => cs.last().map_or(&Json::Null, |c| c.get("text")),
                        _ => &Json::Null,
                    }
                };
                if let Some(text) = text.as_str() {
                    let path = uri_path(&uri);
                    let doc = self.docs.entry(uri).or_insert(Doc {
                        text: String::new(),
                        dirty: true,
                        inputs: vec![path.clone()],
                        good: None,
                        partial: None,
                        diags: Vec::new(),
                        shown: 0,
                        asked: 0,
                        moved: None,
                    });
                    text.clone_into(&mut doc.text);
                    doc.dirty = true;
                    self.edited = Some(Instant::now());
                    self.invalidate(&path);
                }
                return None;
            }
            "textDocument/didClose" => {
                self.docs.remove(&uri);
                self.invalidate(&uri_path(&uri));
                self.publish(&uri, Vec::new());
                return None;
            }
            "textDocument/semanticTokens/full" => {
                let ops = self.operator_tokens(&uri_path(&uri));
                let data = self
                    .docs
                    .get(&uri)
                    .map(|d| semantic_tokens(&d.text, &ops))
                    .unwrap_or_default();
                Json::obj([("data", Json::Arr(data.into_iter().map(Json::num).collect()))])
            }
            "workspace/executeCommand" => {
                let args: Vec<String> = match params.get("arguments") {
                    Json::Arr(a) => a
                        .iter()
                        .filter_map(|x| x.as_str().map(str::to_owned))
                        .collect(),
                    _ => Vec::new(),
                };
                if params.get("command").as_str() != Some(actions::NABI_ADD) || args.is_empty() {
                    self.error(id, -32602, "unknown command".to_owned());
                    return None;
                }
                if let Err(why) = actions::nabi_add(&args[0], &args[1..]) {
                    self.error(id, -32603, why);
                    return None;
                }
                // The manifest changed: every check may.
                self.deps_changed();
                Json::Null
            }
            "textDocument/codeAction" => {
                self.wait_check(&uri);
                if self.docs.get(&uri).is_some_and(|d| d.dirty) {
                    self.refresh(&uri);
                }
                let found = self.docs.get(&uri).map(|d| {
                    let (ctx, at) = (params.get("context"), params.get("range"));
                    actions::code_actions(&uri, &d.text, &d.diags, ctx, at)
                });
                Json::Arr(found.unwrap_or_default())
            }
            "textDocument/hover"
            | "textDocument/definition"
            | "textDocument/documentSymbol"
            | "textDocument/completion"
            | "textDocument/signatureHelp"
            | "textDocument/references"
            | "textDocument/rename"
            | "textDocument/prepareRename"
            | "textDocument/formatting" => {
                let pos = params.get("position");
                let line = pos.get("line").as_u32().unwrap_or(0);
                let col = pos.get("character").as_u32().unwrap_or(0);
                if self.needs_check(method, &uri, line, col) {
                    // The running check of this text, else one now.
                    self.wait_check(&uri);
                    if self
                        .docs
                        .get(&uri)
                        .is_some_and(|d| d.dirty || d.moved.is_some())
                    {
                        self.refresh(&uri);
                    }
                }
                let new_name = params.get("newName").as_str();
                let renamed = match method {
                    "textDocument/rename" => {
                        Some(self.rename(&uri, line, col, new_name.unwrap_or("")))
                    }
                    "textDocument/prepareRename" => Some(self.prepare_rename(&uri, line, col)),
                    _ => None,
                };
                if let Some(r) = renamed {
                    match r {
                        Ok(v) => v,
                        Err(why) => {
                            self.error(id, -32602, why);
                            return None;
                        }
                    }
                } else {
                    let ops = if method == "textDocument/formatting" {
                        self.operator_tokens(&uri_path(&uri))
                    } else {
                        Vec::new()
                    };
                    self.query(method, &uri, line, col, &ops)
                        .unwrap_or(Json::Null)
                }
            }
            _ if matches!(id, Json::Null) => return None,
            _ => {
                self.error(id, -32601, format!("unsupported method `{method}`"));
                return None;
            }
        };
        self.reply(id, result);
        None
    }

    fn initialize_result() -> Json {
        Json::obj([
            (
                "capabilities",
                Json::obj([
                    (
                        "textDocumentSync",
                        Json::obj([
                            ("openClose", Json::Bool(true)),
                            ("change", Json::num(1)),
                            ("save", Json::obj([("includeText", Json::Bool(false))])),
                        ]),
                    ),
                    (
                        "workspace",
                        Json::obj([(
                            "workspaceFolders",
                            Json::obj([
                                ("supported", Json::Bool(true)),
                                ("changeNotifications", Json::Bool(true)),
                            ]),
                        )]),
                    ),
                    ("hoverProvider", Json::Bool(true)),
                    ("definitionProvider", Json::Bool(true)),
                    ("referencesProvider", Json::Bool(true)),
                    (
                        "renameProvider",
                        Json::obj([("prepareProvider", Json::Bool(true))]),
                    ),
                    (
                        "codeActionProvider",
                        Json::obj([("codeActionKinds", Json::Arr(vec![Json::str("quickfix")]))]),
                    ),
                    (
                        "signatureHelpProvider",
                        Json::obj([(
                            "triggerCharacters",
                            Json::Arr(vec![Json::str("("), Json::str(",")]),
                        )]),
                    ),
                    (
                        "executeCommandProvider",
                        Json::obj([("commands", Json::Arr(vec![Json::str(actions::NABI_ADD)]))]),
                    ),
                    ("documentSymbolProvider", Json::Bool(true)),
                    ("documentFormattingProvider", Json::Bool(true)),
                    (
                        "completionProvider",
                        Json::obj([(
                            "triggerCharacters",
                            Json::Arr(vec![Json::str("."), Json::str(":")]),
                        )]),
                    ),
                    // Highlight what the static grammar cannot know:
                    // macro calls and user-defined literal suffixes.
                    (
                        "semanticTokensProvider",
                        Json::obj([
                            (
                                "legend",
                                Json::obj([
                                    (
                                        "tokenTypes",
                                        Json::Arr(vec![
                                            Json::str("macro"),
                                            Json::str("decorator"),
                                            Json::str("keyword"),
                                            Json::str("operator"),
                                        ]),
                                    ),
                                    ("tokenModifiers", Json::Arr(Vec::new())),
                                ]),
                            ),
                            ("full", Json::Bool(true)),
                        ]),
                    ),
                ]),
            ),
            (
                "serverInfo",
                Json::obj([
                    ("name", Json::str("nabils")),
                    ("version", Json::str(env!("CARGO_PKG_VERSION"))),
                ]),
            ),
        ])
    }

    /// `initialize`'s workspace folders (else `rootUri` / `rootPath`) and
    /// whether the client watches files for us.
    fn workspace(&mut self, params: &Json) {
        self.folders = match params.get("workspaceFolders") {
            Json::Arr(fs) => fs
                .iter()
                .filter_map(|f| f.get("uri").as_str().map(uri_path))
                .collect(),
            _ => Vec::new(),
        };
        if self.folders.is_empty() {
            let root = params.get("rootUri").as_str().map(uri_path);
            let root = root.or_else(|| params.get("rootPath").as_str().map(PathBuf::from));
            self.folders.extend(root);
        }
        self.watch = params
            .get("capabilities")
            .get("workspace")
            .get("didChangeWatchedFiles")
            .get("dynamicRegistration")
            == &Json::Bool(true);
    }

    /// Ask the client to report changes of every manifest and lock.
    fn watch_manifests(&mut self) {
        let watcher = |glob: &str| Json::obj([("globPattern", Json::str(glob))]);
        self.send(&Json::obj([
            ("jsonrpc", Json::str("2.0")),
            ("id", Json::str("nabils-watch")),
            ("method", Json::str("client/registerCapability")),
            (
                "params",
                Json::obj([(
                    "registrations",
                    Json::Arr(vec![Json::obj([
                        ("id", Json::str("nabils-manifests")),
                        ("method", Json::str("workspace/didChangeWatchedFiles")),
                        (
                            "registerOptions",
                            Json::obj([(
                                "watchers",
                                Json::Arr(vec![watcher("**/nabi.nabion"), watcher("**/nabi.lock")]),
                            )]),
                        ),
                    ])]),
                )]),
            ),
        ]));
    }

    /// A manifest, lock or workspace folder changed: resolve again, check
    /// every buffer again.
    fn deps_changed(&mut self) {
        self.resolved.clear();
        self.ops.clear();
        let seq = self.seq;
        for doc in self.docs.values_mut() {
            doc.dirty = true;
            doc.moved = Some(seq);
        }
    }

    /// Where the dependencies of a check of `path` come from: its package
    /// (the nearest directory above with a manifest or a root file), else
    /// the workspace folder holding it.
    fn deps_dir(&self, path: &Path) -> Option<PathBuf> {
        self.deps.as_ref()?;
        let package = path.ancestors().skip(1).find(|d| {
            d.join(MANIFEST).is_file()
                || d.join("src/main.nabi").is_file()
                || d.join("src/lib.nabi").is_file()
        });
        package
            .or_else(|| {
                self.folders
                    .iter()
                    .map(PathBuf::as_path)
                    .find(|f| path.starts_with(f))
            })
            .map(Path::to_path_buf)
    }

    /// Resolve the dependencies of `path`'s directory, once until they
    /// change; a failure is said once and checks without them.
    fn ensure_deps(&mut self, path: &Path) {
        let Some(dir) = self.deps_dir(path) else {
            return;
        };
        if self.resolved.contains_key(&dir) {
            return;
        }
        let Some(deps) = self.deps.as_mut() else {
            return;
        };
        let found = deps(&dir).unwrap_or_else(|e| {
            let msg = format!("nabils: dependencies of {}: {e}", dir.display());
            self.send(&Json::obj([
                ("jsonrpc", Json::str("2.0")),
                ("method", Json::str("window/showMessage")),
                (
                    "params",
                    Json::obj([("type", Json::num(2)), ("message", Json::str(msg))]),
                ),
            ]));
            Externs::default()
        });
        self.resolved.insert(dir, found);
    }

    /// The `--extern`s a check of `path` uses, and their `--extern-dep`s.
    fn externs_of(&self, path: &Path) -> (&[(String, PathBuf)], &[Pair]) {
        match self.deps_dir(path).and_then(|d| self.resolved.get(&d)) {
            Some(e) => (&e.externs, &e.deps),
            None => (&self.externs, &[]),
        }
    }

    /// The operator tokens the files of `path`'s package lex: its own
    /// and its dependencies' exported ones (none outside a package).
    fn operator_tokens(&mut self, path: &Path) -> Vec<String> {
        let Some(pkg) = nabic_interface::discover(path)
            .ok()
            .filter(|p| !p.anonymous)
        else {
            return Vec::new();
        };
        if let Some(ops) = self.ops.get(&pkg.dir) {
            return ops.clone();
        }
        let roots: Vec<PathBuf> = std::iter::once(pkg.root.clone())
            .chain(pkg.library.clone())
            .collect();
        self.ensure_deps(path);
        let ops = self.session(false, path).operator_tokens(&roots);
        self.ops.insert(pkg.dir, ops.clone());
        ops
    }

    /// An edited or closed buffer changes every check that read it.
    fn invalidate(&mut self, path: &Path) {
        self.ops.clear();
        let seq = self.seq;
        for (uri, doc) in &mut self.docs {
            if doc.inputs.iter().any(|p| p == path) {
                doc.dirty = true;
                if uri_path(uri) != path {
                    doc.moved = Some(seq);
                }
            }
        }
    }

    /// A session for checking `path`, with its dependencies.
    fn session(&self, anonymous: bool, path: &Path) -> Session {
        let mut s = Session::new();
        for b in &self.bootstrap {
            s.add_bootstrap_file(b.clone());
        }
        let (externs, deps) = self.externs_of(path);
        for (n, p) in externs {
            s.add_extern(n.clone(), p.clone());
        }
        for (d, n) in deps {
            s.add_extern_dep(d.clone(), n.clone());
        }
        s.set_anonymous(anonymous);
        // Submodule files as edited.
        for (uri, d) in &self.docs {
            s.add_buffer(uri_path(uri), d.text.clone());
        }
        s
    }

    /// Check `text` as buffer `path` now.
    fn compile(&self, path: &Path, text: &str) -> Analysis {
        (self.prepare(path, text))()
    }

    /// The check of `text` as buffer `path`: the package root with this
    /// buffer as root or the root as open / on disk; files outside any
    /// package are anonymous. Open buffers stand for their files. With both
    /// root files, a file the library reaches is the library's, any
    /// other the executable's, which mounts the library (as edited) as
    /// dependency `<name>`.
    fn prepare(&self, path: &Path, text: &str) -> Job {
        let levels = self.levels.clone();
        let Some(pkg) = nabic_interface::discover(path)
            .ok()
            .filter(|p| !p.anonymous)
        else {
            let root = Root {
                session: self.session(true, path),
                text: text.to_owned(),
                path: path.to_path_buf(),
            };
            return Box::new(move || root.check(&levels));
        };
        let abs = |p: &Path| std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
        let lib = pkg
            .library_target()
            .filter(|_| abs(&pkg.root) != path)
            .map(|lib| self.root(&abs(&lib.root), path, text, None));
        let library = pkg.library.as_deref().map(|l| (pkg.name.as_str(), abs(l)));
        let main = self.root(&abs(&pkg.root), path, text, library);
        let key = path.display().to_string();
        Box::new(move || {
            if let Some(lib) = lib {
                let a = lib.check(&levels);
                if a.source_map.files().iter().any(|(p, _)| *p == key) {
                    return a;
                }
            }
            main.check(&levels)
        })
    }

    /// Root `root` for buffer `path`, with `library` (name and
    /// `src/lib.nabi`) as a dependency.
    fn root(&self, root: &Path, path: &Path, text: &str, library: Option<(&str, PathBuf)>) -> Root {
        let mut session = self.session(false, path);
        if let Some((name, lib)) = library {
            let name = nabic_interface::code_name(name);
            session.add_extern(name.clone(), lib);
            session.set_own_library(Some(name));
        }
        if root == path {
            return Root {
                session,
                text: text.to_owned(),
                path: path.to_path_buf(),
            };
        }
        // By path: the editor's URI spelling may differ from ours.
        let open = self
            .docs
            .iter()
            .find(|(uri, _)| uri_path(uri) == root)
            .map(|(_, d)| d.text.clone());
        let text = open
            .or_else(|| std::fs::read_to_string(root).ok())
            .unwrap_or_default();
        Root {
            session,
            text,
            path: root.to_path_buf(),
        }
    }

    /// Whether `method` at `line:col` waits for a check of the buffer:
    /// references and rename edit by its current text, and a query with
    /// no type-checked compile yet or on text edited since has nothing
    /// to answer from.
    fn needs_check(&self, method: &str, uri: &str, line: u32, col: u32) -> bool {
        let Some(doc) = self.docs.get(uri) else {
            return false;
        };
        let text_only = matches!(
            method,
            "textDocument/documentSymbol" | "textDocument/formatting"
        );
        if doc.moved.is_some() && !text_only {
            return true;
        }
        // A check of the current text still running counts as not done.
        if !doc.dirty && doc.asked <= doc.shown {
            return false;
        }
        match method {
            "textDocument/references" | "textDocument/rename" | "textDocument/prepareRename" => {
                true
            }
            "textDocument/hover" | "textDocument/definition" => {
                let key = uri_path(uri).display().to_string();
                let off = offset(&doc.text, line, col);
                doc.good
                    .as_ref()
                    .is_none_or(|a| view(a, &key, &doc.text, off).is_none())
            }
            "textDocument/documentSymbol" | "textDocument/formatting" => false,
            _ => doc.good.is_none(),
        }
    }

    /// How long until a background check is due; `None` while one runs
    /// or no buffer needs one.
    fn due(&self) -> Option<Duration> {
        if self.running.is_some() || !self.docs.values().any(|d| d.dirty) {
            return None;
        }
        Some(
            self.edited
                .map_or(Duration::ZERO, |t| SETTLE.saturating_sub(t.elapsed())),
        )
    }

    /// Check a dirty buffer in the background; `checked` takes the
    /// result.
    fn start_check(&mut self) {
        let Some(uri) = self
            .docs
            .iter()
            .filter(|(_, d)| d.dirty)
            .map(|(u, _)| u.clone())
            .min()
        else {
            return;
        };
        let Some((seq, job)) = self.snapshot(&uri) else {
            return;
        };
        let (done, result) = mpsc::channel();
        self.running = Some(result);
        let events = self.events.clone();
        std::thread::spawn(move || {
            let _ = done.send((uri, seq, job()));
            let _ = events.send(Event::Checked);
        });
    }

    /// Wait for the background check of `uri` when one runs: until it is
    /// in, the buffer is clean but its diagnostics and analysis are older.
    fn wait_check(&mut self, uri: &str) {
        if self.docs.get(uri).is_none_or(|d| d.asked <= d.shown) {
            return;
        }
        if let Some(Ok((uri, seq, a))) = self.running.take().map(|r| r.recv()) {
            self.apply(&uri, seq, a);
        }
    }

    /// Take the background check's result if it is done.
    fn checked(&mut self) {
        let Some(Ok((uri, seq, a))) = self.running.as_ref().map(mpsc::Receiver::try_recv) else {
            return;
        };
        self.running = None;
        self.apply(&uri, seq, a);
    }

    /// Wait for the running check, then check every buffer edited since.
    fn flush(&mut self) {
        if let Some(Ok((uri, seq, a))) = self.running.take().map(|r| r.recv()) {
            self.apply(&uri, seq, a);
        }
        let dirty: Vec<String> = self
            .docs
            .iter()
            .filter(|(_, d)| d.dirty)
            .map(|(u, _)| u.clone())
            .collect();
        for uri in dirty {
            self.refresh(&uri);
        }
    }

    /// Recompile a buffer now and publish its diagnostics.
    fn refresh(&mut self, uri: &str) {
        if let Some((seq, job)) = self.snapshot(uri) {
            let a = job();
            self.apply(uri, seq, a);
        }
    }

    /// The check of `uri`'s text as it is now, marking the buffer clean;
    /// `None` when there is nothing to compile (a `.nabion` file is
    /// checked here, at once).
    fn snapshot(&mut self, uri: &str) -> Option<(u64, Job)> {
        let doc = self.docs.get(uri)?;
        let path = uri_path(uri);
        // A `.nabion` data file (`nabi.nabion`, the package manifest) is one
        // NABION value, not a module: only its syntax is checked here.
        if path.extension().is_some_and(|x| x == "nabion") {
            let diags = match nabic_nabion::parse(&doc.text) {
                Ok(_) => Vec::new(),
                Err(e) => {
                    let d = NabiDiagnostic::error(Code::SYNTAX, e.span, &e.message);
                    vec![(diag_json(&d, uri, &doc.text), d)]
                }
            };
            let doc = self.docs.get_mut(uri).expect("checked above");
            doc.dirty = false;
            let json = diags.iter().map(|(j, _)| j.clone()).collect();
            doc.diags = diags;
            self.publish(uri, json);
            return None;
        }
        self.ensure_deps(&path);
        let doc = self.docs.get(uri).expect("checked above");
        let job = self.prepare(&path, &doc.text);
        self.seq += 1;
        let doc = self.docs.get_mut(uri).expect("checked above");
        doc.dirty = false;
        doc.asked = self.seq;
        Some((self.seq, job))
    }

    /// Take the check of snapshot `seq` of `uri`: its type-checked
    /// result serves queries; its diagnostics are published unless the
    /// buffer changed since (they would point into older text).
    fn apply(&mut self, uri: &str, seq: u64, a: Analysis) {
        let Some(doc) = self.docs.get_mut(uri).filter(|d| d.shown < seq) else {
            return;
        };
        doc.shown = seq;
        if doc.moved.is_some_and(|m| seq > m) {
            doc.moved = None;
        }
        let key = uri_path(uri).display().to_string();
        if !a.source_map.files().is_empty() {
            doc.inputs = a
                .source_map
                .files()
                .iter()
                .map(|(p, _)| PathBuf::from(p))
                .collect();
        }
        let diags = (!doc.dirty).then(|| {
            a.diags
                .iter()
                .filter(|d| a.diag_path(d).is_none_or(|p| p == key))
                .map(|d| {
                    let span = d.primary.as_ref().map(|l| l.span).unwrap_or_default();
                    let j = diag_json(d, uri, a.file(span).map_or("", |(_, t)| t.as_str()));
                    (j, d.clone())
                })
                .collect::<Vec<_>>()
        });
        match (&a.hir, a.partial) {
            (Some(_), true) => doc.partial = Some(a),
            (Some(_), false) => {
                doc.good = Some(a);
                doc.partial = None;
            }
            (None, _) => doc.partial = None,
        }
        if let Some(diags) = diags {
            let json = diags.iter().map(|(j, _)| j.clone()).collect();
            doc.diags = diags;
            self.publish(uri, json);
        }
    }

    fn publish(&mut self, uri: &str, diags: Vec<Json>) {
        self.send(&Json::obj([
            ("jsonrpc", Json::str("2.0")),
            ("method", Json::str("textDocument/publishDiagnostics")),
            (
                "params",
                Json::obj([("uri", Json::str(uri)), ("diagnostics", Json::Arr(diags))]),
            ),
        ]));
    }

    /// `textDocument/rename`: every reference in the buffer (see
    /// `references`), when the name is declared in it.
    fn rename(&self, uri: &str, line: u32, col: u32, new_name: &str) -> Result<Json, String> {
        let ident = new_name.starts_with(|c: char| c == '_' || c.is_alphabetic())
            && new_name.chars().all(is_ident_char)
            && nabic_lexer::token::keyword_kind(new_name).is_none();
        if !ident {
            return Err(format!("`{new_name}` is not a valid name"));
        }
        let doc = self.docs.get(uri).ok_or("unknown document")?;
        let text = doc.text.as_str();
        let key = uri_path(uri).display().to_string();
        let off = offset(text, line, col);
        let refs = renamable(doc, &key, off)?.ok_or("nothing to rename here")?;
        let edits = refs
            .into_iter()
            .map(|(s, e)| {
                Json::obj([
                    ("range", range(text, s, e)),
                    ("newText", Json::str(new_name)),
                ])
            })
            .collect();
        Ok(Json::obj([(
            "changes",
            Json::obj([(uri, Json::Arr(edits))]),
        )]))
    }

    /// `textDocument/prepareRename`: the name under the cursor when
    /// `rename` would take it, else why not.
    fn prepare_rename(&self, uri: &str, line: u32, col: u32) -> Result<Json, String> {
        let doc = self.docs.get(uri).ok_or("unknown document")?;
        let text = doc.text.as_str();
        let key = uri_path(uri).display().to_string();
        let off = offset(text, line, col);
        if renamable(doc, &key, off)?.is_none() {
            return Ok(Json::Null);
        }
        let (name, start) = ident_at(text, off).expect("renamable found it");
        Ok(Json::obj([
            ("range", range(text, start, start + name.len())),
            ("placeholder", Json::str(name)),
        ]))
    }

    /// `ops`: the operator tokens of `formatting`.
    fn query(&self, method: &str, uri: &str, line: u32, col: u32, ops: &[String]) -> Option<Json> {
        let doc = self.docs.get(uri)?;
        let text = doc.text.as_str();
        let path = uri_path(uri);
        let key = path.display().to_string();
        let off = offset(text, line, col);
        match method {
            "textDocument/formatting" => {
                let new = nabic_tools::fmt::format_file_with(&path, text, ops).ok()?;
                Some(Json::Arr(if new == text {
                    Vec::new()
                } else {
                    vec![Json::obj([
                        ("range", range(text, 0, text.len())),
                        ("newText", Json::Str(new)),
                    ])]
                }))
            }
            "textDocument/documentSymbol" => {
                let program = parse_doc(text, &key);
                let (toks, _) = nabic_lexer::Lexer::new(text, key).lex_all();
                let shape = Shape { text, toks };
                Some(Json::Arr(
                    program
                        .items
                        .iter()
                        .filter_map(|i| shape.symbol(i))
                        .collect(),
                ))
            }
            "textDocument/hover" => {
                // On a `name!(..)` call, show its expansion in place.
                // A check with type errors: what it typed; else the last
                // check that type-checked.
                let from = |a: &Analysis| {
                    let (old, at) = view(a, &key, text, off)?;
                    hover(a, &key, old, at)
                };
                let value = macro_hover(text, off)
                    .or_else(|| {
                        doc.partial
                            .as_ref()
                            .and_then(from)
                            .filter(|h| !h.contains("{unknown}"))
                    })
                    .or_else(|| from(doc.good.as_ref()?))?;
                Some(Json::obj([(
                    "contents",
                    Json::obj([("kind", Json::str("markdown")), ("value", Json::str(value))]),
                )]))
            }
            "textDocument/definition" => {
                let good = doc.good.as_ref();
                let (vtext, voff) = match good {
                    Some(a) => view(a, &key, text, off)?,
                    None => (text, off),
                };
                let (file, def_text, from, to) = definition_in(good, &key, vtext, voff)?;
                if file != key {
                    return Some(location_at(&path_uri(Path::new(&file)), def_text, from, to));
                }
                // This buffer: from the checked text to the text now.
                let (from, to) = if vtext == text {
                    (from, to)
                } else {
                    let d = Delta::new(vtext, text);
                    (d.to_new(from)?, d.to_new(to)?)
                };
                Some(location_at(uri, text, from, to))
            }
            "textDocument/references" => {
                let refs = references(doc, &key, off)?;
                Some(Json::Arr(
                    refs.into_iter()
                        .map(|(s, e)| location_at(uri, text, s, e))
                        .collect(),
                ))
            }
            "textDocument/signatureHelp" => {
                let compile = |t: &str| self.compile(&path, t);
                signature::signature_help(text, &key, off, compile, doc.good.as_ref())
            }
            "textDocument/completion" => {
                let compile = |t: &str| self.compile(&path, t);
                let good = doc.good.as_ref();
                let (_, found) =
                    nabic_tools::complete::complete(text, &key, off, compile, good, &[]);
                Some(Json::Arr(
                    found
                        .into_iter()
                        .map(|(label, kind, detail)| {
                            Json::obj([
                                ("label", Json::Str(label)),
                                ("kind", Json::num(kind)),
                                ("detail", Json::Str(detail)),
                            ])
                        })
                        .collect(),
                ))
            }
            _ => None,
        }
    }
}

/// A root file to check and the session to check it in.
struct Root {
    session: Session,
    text: String,
    path: PathBuf,
}

impl Root {
    fn check(self, levels: &Levels) -> Analysis {
        analyze(self.session, &self.text, &self.path, levels)
    }
}

/// `a`'s text of buffer `key` and the position there of `off` in
/// `text`, the buffer now; `None` when the name at `off` was edited
/// since `a`.
fn view<'a>(a: &'a Analysis, key: &str, text: &'a str, off: usize) -> Option<(&'a str, usize)> {
    let Some(old) = a.text_of(key).filter(|t| *t != text) else {
        return Some((text, off));
    };
    let d = Delta::new(old, text);
    let (from, to) = ident_at(text, off).map_or((off, off), |(n, s)| (s, s + n.len()));
    let at = d.to_old(from).and(d.to_old(to)).and(d.to_old(off))?;
    Some((old, at))
}

/// Where the name at `off` in `text` (buffer `key` as `good` checked
/// it) is defined: file, its text and the name's byte range.
fn definition_in<'a>(
    good: Option<&'a Analysis>,
    key: &str,
    text: &'a str,
    off: usize,
) -> Option<(String, &'a str, usize, usize)> {
    let (ident, start) = ident_at(text, off)?;
    if let Some(m) = good.and_then(|a| member_at(a, key, text, ident, start)) {
        let (file, def_text) = good?.file(m.def_span())?;
        let (from, to) = m.name_range(def_text)?;
        return Some((file.clone(), def_text, from, to));
    }
    if let Some(m) = good.and_then(|a| indexed_member(a, key, text, ident, start)) {
        let (file, def_text) = good?.file(m.span)?;
        let (from, to) = name_range(def_text, m.span.start as usize, ident)
            .unwrap_or((m.span.start as usize, m.span.end as usize));
        return Some((file.clone(), def_text, from, to));
    }
    // A local: resolve through the scopes of the buffer's AST.
    let program = parse_doc(text, key);
    if let Some(span) = resolve_local(&program, off) {
        let (from, to) = name_range(text, span.start as usize, ident)
            .unwrap_or((span.start as usize, span.end as usize));
        return Some((key.to_owned(), text, from, to));
    }
    let good = good?;
    let span = definition(good, key, ident, off)?;
    let (file, def_text) = good.file(span)?;
    let (from, to) = name_range(def_text, span.start as usize, ident)
        .unwrap_or((span.start as usize, span.end as usize));
    Some((file.clone(), def_text, from, to))
}

/// The buffer's tokens, for item extents.
struct Shape<'t> {
    text: &'t str,
    toks: Vec<nabic_lexer::Token>,
}

impl Shape<'_> {
    /// The item whose keyword is at `start`: from its modifiers to its
    /// closing `}` or `;`.
    fn extent(&self, start: usize) -> (usize, usize) {
        let mut from = start;
        loop {
            let head = self.text[..from].trim_end();
            let Some(m) = ["pub(pkg)", "pub", "const", "unsafe", "async", "extern"]
                .into_iter()
                .find(|m| {
                    head.ends_with(m)
                        && !head[..head.len() - m.len()]
                            .chars()
                            .next_back()
                            .is_some_and(is_ident_char)
                })
            else {
                break;
            };
            from = head.len() - m.len();
        }
        let first = self
            .toks
            .partition_point(|t| (t.span.start as usize) < start);
        let mut depth = 0u32;
        for (i, t) in self.toks.iter().enumerate().skip(first) {
            match &t.kind {
                K::LParen | K::LBracket => depth += 1,
                K::RParen | K::RBracket => depth = depth.saturating_sub(1),
                K::LBrace if depth == 0 => {
                    let end =
                        balanced_close(&self.toks, i).map_or(t.span.end, |c| self.toks[c].span.end);
                    return (from, end as usize);
                }
                K::Semi if depth == 0 => return (from, t.span.end as usize),
                _ => {}
            }
        }
        (from, self.text.len())
    }

    /// LSP `DocumentSymbol` of an item.
    fn symbol(&self, item: &Item) -> Option<Json> {
        let fn_sym = |f: &nabic_ast::FnDef, kind: u32| self.sym(&f.name, kind, f.span, Vec::new());
        let (name, kind, children): (String, u32, Vec<Json>) = match item {
            Item::Fn(f) => return Some(fn_sym(f, 12)),
            Item::Struct(s) => (s.name.clone(), 23, Vec::new()),
            Item::Union(u) => (u.name.clone(), 23, Vec::new()),
            Item::Enum(e) => (e.name.clone(), 10, Vec::new()),
            Item::Trait(t) => (
                t.name.clone(),
                11,
                t.items
                    .iter()
                    .filter_map(|i| match i {
                        TraitItem::Method(f) => Some(fn_sym(f, 6)),
                        _ => None,
                    })
                    .collect(),
            ),
            Item::Impl(i) => {
                // `impl Tr for T` as written, up to the body. A derive's
                // generated impl (its span is the attribute's) is no symbol.
                let head = self.text.get(i.span.start as usize..)?;
                if !(head.starts_with("impl") || head.starts_with("unsafe")) {
                    return None;
                }
                let head = head.split('{').next().unwrap_or(head);
                (
                    head.split_whitespace().collect::<Vec<_>>().join(" "),
                    5,
                    i.items
                        .iter()
                        .filter_map(|i| match i {
                            ImplItem::Method(f) => Some(fn_sym(f, 6)),
                            _ => None,
                        })
                        .collect(),
                )
            }
            Item::Const(c) => (c.name.clone(), 14, Vec::new()),
            Item::Static(s) => (s.name.clone(), 13, Vec::new()),
            Item::TypeAlias(t) => (t.name.clone(), 26, Vec::new()),
            Item::Mod(m) => (
                m.name.clone(),
                2,
                m.items
                    .iter()
                    .flatten()
                    .filter_map(|i| self.symbol(i))
                    .collect(),
            ),
            Item::Use(_) | Item::Extern(_) => return None,
        };
        Some(self.sym(&name, kind, item.span(), children))
    }

    /// A symbol whose keyword is at `span`: `range` covers the item,
    /// `selectionRange` its name (an impl's head).
    fn sym(&self, name: &str, kind: u32, span: Span, children: Vec<Json>) -> Json {
        let start = span.start as usize;
        let (from, to) = self.extent(start);
        let (s, e) = if kind == 5 {
            let head = self.text[start..to].split('{').next().unwrap_or_default();
            (start, start + head.trim_end().len())
        } else {
            name_range(self.text, start, name)
                .filter(|r| r.1 <= to)
                .unwrap_or((start, span.end as usize))
        };
        Json::obj([
            ("name", Json::str(name)),
            ("kind", Json::num(kind)),
            ("range", range(self.text, from, to.max(e))),
            ("selectionRange", range(self.text, s, e)),
            ("children", Json::Arr(children)),
        ])
    }
}

fn location_at(uri: &str, text: &str, start: usize, end: usize) -> Json {
    Json::obj([("uri", Json::str(uri)), ("range", range(text, start, end))])
}

/// A manifest or lock: its change changes the dependencies.
fn is_manifest(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|n| n == MANIFEST || n == "nabi.lock")
}
