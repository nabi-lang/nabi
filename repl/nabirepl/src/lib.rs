//! `repl`: cells JIT-linked into one session, the implicit
//! `main.nabi`.
//!
//! Each cell is compiled to an object (`nabic_codegen_native`) and sent
//! to a long-lived host (the runtime's `repl`) that links and
//! runs it. Items are in every cell's program; statements are not.
//! Functions the host already has with the same text are only declared
//! (`ReplCell::known`). A top-level binding is an exported global slot (a
//! GC root) owned by its cell; later cells declare it `extern "C" static
//! mut` and copy it in on entry and out on exit. A cell that panics leaves
//! the session untouched: the host forks a snapshot per cell (Windows:
//! saves and restores memory).
//!
//! `repl <path>` loads a program first (`load_program`).

mod cells;
mod commands;
mod display;
mod eval;
mod host;
mod load;

#[allow(clippy::wildcard_imports)]
pub(crate) use crate::cells::*;
pub use crate::cells::{incomplete, open_brackets, split_cells};
#[allow(clippy::wildcard_imports)]
pub(crate) use crate::commands::*;
#[allow(clippy::wildcard_imports)]
pub(crate) use crate::display::*;
pub use crate::host::*;

use nabic_core::{HashMap, HashSet};
use nabic_diagnostic::{Label, NabiDiagnostic, Span, Tag};
use nabic_hir::{
    HirBlock, HirExpr, HirExprKind, HirFunction, HirModule, HirStaticDef, HirStmt, HirTy,
};
use nabic_interface::{
    BuildOptions, CompileResult, ReplCell, ReplFn, Session, SourceMap, TempDir, link_host,
};
use nabic_ir_common::{ConstVal, LocalId};
use nabic_lexer::TokenKind as T;
use nabic_tools::analysis::{Analysis, ty_name, ty_path};
use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Read, Write};
#[cfg(windows)]
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

const VALUE: &str = "__nabi_repl_v";
/// The host's `__nabi_repl_out`: a cell's shown value.
const SHOWN: &str = "__nabi_repl_out";
/// The host's `main` (`host.rs`, compiled by `build.rs`); empty where
/// no host runs.
const HOST_OBJ: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/repl_host.o"));

const HELP: &str = "  <expr>         evaluate and print its value (`Display`, else `Debug`,
                 else its type); `it` holds the last value shown (a
                 binding named `it` wins)
  <expr>?        evaluate and print its `Debug` form
  let x = ..     bind (`let` may redeclare a name; a cell's last
                 `let` or assignment needs no `;`)
  fn / struct / enum / impl / trait / use ..   define (a new `fn f`,
                 `struct S` or `impl Tr for S` replaces the old one, an
                 `impl S` the one with its methods; one cell may define
                 several)
  :type <expr>, :t <expr>   print the type of an expression
  :doc <name>    print its signature and doc comment
  :time <expr>   evaluate it; print the time of each phase and the total
  :vars          list the bindings (`name: type`) and items
  :load <file>   load a program into the session (as `nabi repl <file>`)
  :reload        load the last loaded program again
  :edit          write cells in `$EDITOR`, then run them
  :save <file>   write the session as a script
  :opt [<level>] print or set the optimization level of later cells
                 (0, 1, 2, 3, s)
  :add <name>[@<req>] | <name> --path <dir> | <name> --git <url>
                 [--rev|--tag|--branch <r>]
                 add a dependency: `nabi install` into the environment, or
                 in a package `nabi add` (`nabi repl`)
  :reset         forget every cell
  :help, :h      print this help
  :exit, :quit, :q, exit, quit
                 leave (also end of input; a binding named `exit` or
                 `quit` wins)
A line ends the cell (`>>>`) unless a bracket, string or comment is still
open, it ends in an operator, `,` or `=`, or it is an item header without
its body: then `...` asks for the next line.
";

/// The items of the session, as source.
#[derive(Clone, Default)]
pub(crate) struct State {
    items: Vec<Item>,
}

/// A cell's number: `N`, or `N.k` for the `k`th cell of a notebook
/// cell that splits into several (0: none, as for the prelude).
#[derive(Clone, Copy, Default, Eq, PartialEq)]
pub struct CellNo {
    pub n: u32,
    pub sub: u16,
}

impl CellNo {
    fn is_none(self) -> bool {
        self == Self::default()
    }
}

impl std::fmt::Display for CellNo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.sub {
            0 => write!(f, "{}", self.n),
            k => write!(f, "{}.{k}", self.n),
        }
    }
}

/// An item of the session.
#[derive(Clone)]
pub(crate) struct Item {
    /// What a redefinition replaces (`fn f`, `struct S`, `impl Tr for S`;
    /// `None` for an inherent impl, see `replaces`).
    key: Option<String>,
    text: String,
    /// The cell that defined it (0: a loaded program or the prelude), its
    /// text, and the item's offset there.
    cell: CellNo,
    source: Arc<str>,
    at: usize,
}

impl Item {
    fn new(key: Option<String>, text: String) -> Self {
        Self {
            key,
            source: Arc::from(text.as_str()),
            text,
            cell: CellNo::default(),
            at: 0,
        }
    }

    /// How messages name it: `fn f`, `impl Display for P`.
    fn name(&self) -> String {
        self.key.clone().unwrap_or_else(|| impl_header(&self.text))
    }
}

/// A program loaded into the session (`repl <path>`).
struct Program {
    /// The session source's path: beside the program's root file, so
    /// its `mod` files resolve.
    session: PathBuf,
    anonymous: bool,
    /// The program's path and text, where its diagnostics and panic
    /// locations point.
    path: String,
    text: Arc<str>,
    /// Its items as the session has them, with their offsets in `text`.
    items: Vec<(String, usize)>,
    /// The offset in `text` of the cell being built (`None`: a typed cell).
    cell: Option<usize>,
}

/// How a cell failed.
pub(crate) enum Fail {
    /// Reported.
    Reported,
    /// A binding later cells could not use (not reported): its name and
    /// why.
    Unkept(String, String),
}

pub(crate) enum Cell {
    Item(Option<String>),
    /// `let ..;`.
    Let,
    Stmt,
    Expr,
}

/// `NABI_REPL_TIME=1` or `:time`: each phase of a cell's build on stderr.
struct Lap(Option<std::time::Instant>);

impl Lap {
    fn start(on: bool) -> Self {
        Self((on || nabic_core::env_flag("NABI_REPL_TIME")).then(std::time::Instant::now))
    }

    fn note(&self, what: &str) {
        if self.0.is_some() {
            eprintln!("[repl] {what}");
        }
    }

    fn mark(&mut self, what: &str) {
        if let Some(t) = &mut self.0 {
            eprintln!("[repl] {what}: {:.1}ms", t.elapsed().as_secs_f64() * 1e3);
            *t = std::time::Instant::now();
        }
    }
}

/// The host's reply to a cell: its shown value, or `None` when it
/// failed (reported; the session is unchanged).
type Loaded = Option<String>;

/// A top-level binding.
#[derive(Clone)]
pub(crate) struct Binding {
    name: String,
    /// The exported global holding its value.
    sym: String,
    /// Its type, as source.
    ty: String,
    /// Its type as `:vars` shows it.
    shown_ty: String,
    /// It holds an `any` value (whose type a redefinition may change).
    holds_any: bool,
    /// The session's `it`, not a user's binding.
    is_it: bool,
}

/// A cell `:save` writes: its statements, then the expression whose
/// value it showed (`it`: kept as `it`).
#[derive(Default)]
struct Saved {
    stmts: String,
    expr: Option<String>,
    it: bool,
}

pub struct Repl {
    bootstrap: Vec<PathBuf>,
    /// Dependencies (`--extern name=<name>.nabihir`).
    pub externs: Vec<(String, PathBuf)>,
    /// Where `:add` prebuilds dependencies.
    pub deps_dir: PathBuf,
    /// The dependency archives the host has linked.
    linked: HashSet<PathBuf>,
    /// The program `repl <path>` loaded.
    program: Option<Program>,
    /// The first cell (`repl <lib>`: `use <lib>::*;`); `:reset`
    /// runs it again.
    pub prelude: Option<String>,
    opts: BuildOptions,
    state: State,
    host: Option<Host>,
    /// The host being built while the first cell compiles.
    starting: Option<std::thread::JoinHandle<Option<Host>>>,
    bindings: Vec<Binding>,
    /// Statics (MIR names) a loaded cell defined: later cells use its
    /// storage (`ReplCell::statics`), so their state carries over.
    statics: HashSet<String>,
    /// Types stored into `any` slots so far; every cell re-seeds them
    /// (`any_seeds`) so its `any` methods know all of them.
    any_types: std::collections::BTreeSet<String>,
    /// Cells built (slot names).
    built: u32,
    /// The `ScanTable`s loaded cells registered (`ReplCell::gc_tables`).
    gc_tables: Vec<Vec<u32>>,
    /// Symbol -> (text hash, symbols used) of what the host has compiled;
    /// unchanged functions are declared, not recompiled.
    known: HashMap<String, (u64, Vec<String>)>,
    tmp: TempDir,
    /// The text the user typed for the cell being built: diagnostics
    /// point into it, not into the generated session source.
    input: String,
    /// The number of the cell being built, `<repl cell N>` in runtime
    /// locations.
    pub cell_no: CellNo,
    /// The number `next_cell` gives instead of the one after `cell_no`.
    pub next: Option<CellNo>,
    /// The cell being built defines items (those with its number): errors
    /// in earlier ones say how to go on.
    defining: bool,
    /// The last compile failed only because an expression cell never
    /// yields a value (`panic(..)`).
    diverged: std::cell::Cell<bool>,
    /// The host process running the current cell (0: none). `SIGINT`
    /// to it ends the cell; the session goes on as before it.
    pub running: Arc<AtomicU32>,
    /// `:time`: the phases of the cell being built go to stderr.
    timing: bool,
    /// The last program loaded and whether it is a lone file (`:reload`).
    loaded: Option<(PathBuf, bool)>,
    /// The cells that ran, for `:save`.
    history: Vec<Saved>,
    /// The source directory of the package the session runs in without
    /// loading it: names it defines get a hint.
    pub package: Option<PathBuf>,
    /// What a running cell reads as stdin.
    pub stdin: CellStdin,
    /// Called with `true` before a cell runs in the host and `false`
    /// after it ends (the kernel streams the output written meanwhile).
    pub live: Option<fn(bool)>,
    /// Under the kernel: a value with `RichDisplay` shows through
    /// `display`, and the host sees `NABI_JUPYTER`.
    pub rich: bool,
    /// The check of the session source completion last used: the
    /// source, its analysis and the path its spans name.
    checked: std::cell::RefCell<Option<(String, Analysis, String)>>,
}

/// A running cell's stdin.
pub enum CellStdin {
    /// Empty: a read is the end of input.
    Null,
    /// The terminal, while the cell runs.
    Tty,
    /// Each read asks for a line; `None` is the end of input.
    Ask(Box<dyn FnMut() -> Option<String>>),
}

/// The terminal's mode before a cell that reads it; put back after.
struct TermMode {
    #[cfg(unix)]
    saved: [u8; 256],
    #[cfg(windows)]
    saved: (*mut std::ffi::c_void, u32),
}

#[cfg(unix)]
unsafe extern "C" {
    fn tcgetattr(fd: i32, termios: *mut u8) -> i32;
    fn tcsetattr(fd: i32, action: i32, termios: *const u8) -> i32;
}

#[cfg(windows)]
unsafe extern "system" {
    fn GetStdHandle(which: u32) -> *mut std::ffi::c_void;
    fn GetConsoleMode(h: *mut std::ffi::c_void, mode: *mut u32) -> i32;
    fn SetConsoleMode(h: *mut std::ffi::c_void, mode: u32) -> i32;
}

impl TermMode {
    #[cfg(unix)]
    fn save() -> Option<Self> {
        let mut saved = [0u8; 256];
        // SAFETY: the buffer is larger than any `struct termios`.
        (unsafe { tcgetattr(0, saved.as_mut_ptr()) } == 0).then_some(Self { saved })
    }

    #[cfg(windows)]
    fn save() -> Option<Self> {
        // SAFETY: a std handle and an out parameter.
        let h = unsafe { GetStdHandle(-10i32 as u32) };
        let mut m = 0;
        (unsafe { GetConsoleMode(h, &mut m) } != 0).then_some(Self { saved: (h, m) })
    }
}

impl Drop for TermMode {
    fn drop(&mut self) {
        // SAFETY: what `save` read, back (`TCSANOW` keeps typed-ahead input).
        #[cfg(unix)]
        unsafe {
            tcsetattr(0, 0, self.saved.as_ptr());
        }
        // SAFETY: the mode `save` read, back on its console handle.
        #[cfg(windows)]
        unsafe {
            SetConsoleMode(self.saved.0, self.saved.1);
        }
    }
}

impl Drop for Repl {
    /// A host still being built writes into `tmp`: wait for it (and end
    /// it) before `tmp` goes.
    fn drop(&mut self) {
        if let Some(h) = self.starting.take() {
            drop(h.join());
        }
    }
}

impl Repl {
    pub fn new(bootstrap: Vec<PathBuf>, opts: BuildOptions) -> Result<Self, String> {
        Ok(Self {
            bootstrap,
            externs: Vec::new(),
            deps_dir: PathBuf::new(),
            linked: HashSet::default(),
            program: None,
            prelude: None,
            tmp: TempDir::new(opts.keep_temps).map_err(|d| d.message)?,
            opts,
            state: State::default(),
            host: None,
            starting: None,
            bindings: Vec::new(),
            statics: HashSet::default(),
            any_types: std::collections::BTreeSet::new(),
            built: 0,
            gc_tables: Vec::new(),
            known: HashMap::default(),
            input: String::new(),
            cell_no: CellNo::default(),
            next: None,
            defining: false,
            diverged: std::cell::Cell::new(false),
            running: Arc::default(),
            timing: false,
            loaded: None,
            history: Vec::new(),
            package: None,
            stdin: CellStdin::Null,
            live: None,
            rich: false,
            checked: std::cell::RefCell::default(),
        })
    }

    /// Read cells until `:quit` or EOF. `false` when input ends mid-cell.
    pub fn run(&mut self, input: impl BufRead, interactive: bool) -> bool {
        let prompt = |cont: bool| {
            if interactive {
                print!("{}", if cont { "... " } else { ">>> " });
                let _ = std::io::stdout().flush();
            }
        };
        let mut buf = String::new();
        prompt(false);
        for line in input.lines() {
            let Ok(line) = line else { break };
            buf.push_str(&line);
            buf.push('\n');
            if incomplete(&buf) {
                prompt(true);
                continue;
            }
            let cell = std::mem::take(&mut buf);
            if !self.cell(cell.trim()) {
                return true;
            }
            prompt(false);
        }
        if interactive {
            println!();
        }
        let rest = buf.trim();
        if !rest.is_empty() {
            eprintln!(
                "error: input ended inside a cell; not run (missing `;`, a closing bracket or \
                 a quote):\n{rest}"
            );
            return false;
        }
        true
    }

    /// Read cells from the terminal with line editing and history
    /// (`lineedit`) until `:quit` or end of input.
    pub fn run_tty(&mut self) {
        #[cfg(windows)]
        forward_ctrl_c(self.running.clone());
        #[cfg(unix)]
        survive_ctrl_c();
        self.stdin = CellStdin::Tty;
        let mut ed = nabirepl_lineedit::Editor::new();
        let mut buf = String::new();
        loop {
            let prompt = if buf.is_empty() { ">>> " } else { "... " };
            let complete = &mut |line: &str, at: usize| {
                let (start, found) = self.complete(&format!("{buf}{line}"), buf.len() + at);
                (start.saturating_sub(buf.len()), found)
            };
            match ed.read_line(prompt, complete) {
                nabirepl_lineedit::Input::Eof => return,
                nabirepl_lineedit::Input::Interrupt => buf.clear(),
                nabirepl_lineedit::Input::Line(line) => {
                    buf.push_str(&line);
                    buf.push('\n');
                    if incomplete(&buf) {
                        continue;
                    }
                    let cell = std::mem::take(&mut buf);
                    ed.add(cell.trim_end());
                    if !self.cell(cell.trim()) {
                        return;
                    }
                }
            }
        }
    }

    /// One input; `false` to stop.
    pub fn cell(&mut self, cell: &str) -> bool {
        cell.clone_into(&mut self.input);
        match cell {
            _ if blank(cell) => {}
            // As in Python: the bare word ends the session.
            _ if EXIT.contains(&cell) && !self.defines(&cell[..4]) => return false,
            _ if cell.starts_with(':') => {
                let (cmd, arg) = cell
                    .split_once(char::is_whitespace)
                    .map_or((cell, ""), |(c, a)| (c, a.trim()));
                return self.command(cmd, arg);
            }
            _ => {
                self.next_cell();
                if let Ok(Some(shown)) = self.eval(cell) {
                    println!("{shown}");
                }
            }
        }
        true
    }

    /// Number the cell about to be built.
    pub fn next_cell(&mut self) {
        let n = self.cell_no.n + 1;
        self.cell_no = self.next.take().unwrap_or(CellNo { n, sub: 0 });
    }
}

/// What a session loads before its first cell.
#[derive(Default)]
pub struct Start<'a> {
    /// The first cell (`repl <lib>`: `use <lib>::*;`).
    pub prelude: Option<&'a str>,
    /// A program (its root file, whether it is a lone file): `repl <path>`.
    pub program: Option<(&'a std::path::Path, bool)>,
    /// The source directory of the package the session runs in, not
    /// loaded (`Repl::package`).
    pub package: Option<PathBuf>,
    /// The toolchain line (`Nabi <version> on <host>`): a terminal session
    /// prints it first, the Jupyter kernel names itself with it.
    pub banner: Option<String>,
}

impl Repl {
    /// Run `s`'s prelude, then load its program.
    pub fn start(&mut self, s: Start<'_>) {
        self.package = s.package;
        if let Some(p) = s.prelude {
            self.prelude = Some(p.to_owned());
            self.run_prelude(p);
        }
        if let Some((path, anonymous)) = s.program {
            self.load_program(path, anonymous);
        }
    }
}

/// `nabi repl` with the stdin it was started on, after `start`.
pub fn main(
    bootstrap: Vec<PathBuf>,
    externs: Vec<(String, PathBuf)>,
    deps_dir: PathBuf,
    opts: BuildOptions,
    interactive: bool,
    start: Start<'_>,
) -> i32 {
    match Repl::new(bootstrap, opts) {
        Ok(mut r) => {
            r.externs = externs;
            r.deps_dir = deps_dir;
            if interactive {
                r.stdin = CellStdin::Tty;
            }
            if let Some(b) = start.banner.as_ref().filter(|_| interactive) {
                println!("{b}\nType :help for help, :exit to quit.");
            }
            r.start(start);
            if interactive {
                r.run_tty();
                0
            } else {
                i32::from(!r.run(std::io::stdin().lock(), false))
            }
        }
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cells_classify_and_continue() {
        assert!(matches!(classify("1 + 2"), Cell::Expr));
        assert!(matches!(classify("println(\"x\");"), Cell::Stmt));
        assert!(matches!(classify("let mut x = 1;"), Cell::Let));
        assert!(matches!(classify("let (a, b) = (1, 2);"), Cell::Let));
        assert!(
            matches!(classify("#[derive(Debug)]\nstruct P { x: i32; }"), Cell::Item(Some(k)) if k == "struct P")
        );
        assert!(matches!(classify("pub fn f() {}"), Cell::Item(Some(k)) if k == "fn f"));
        assert!(matches!(classify("impl P {}"), Cell::Item(None)));
        assert!(incomplete("let x =\n"));
        assert!(incomplete("fn f() {\n"));
        assert!(incomplete("#[derive(Debug)]\n"));
        assert!(incomplete("let s = \"a\n"));
        assert!(!incomplete("let x =\n42i32;\n"));
        assert!(!incomplete("f(\"(\")\n"));
    }

    #[test]
    fn cells_end_by_tokens() {
        let complete = [
            // Brackets in literals and comments do not count.
            "'('",
            "'\"'",
            "/* ( */ 1",
            "1 + 1 /* c */",
            "f(x) // (",
            "r\"(\"",
            // A balanced line ends the cell; `;` is the compiler's to ask for.
            "let x = 1",
            "f(); let x = 1",
            "x as List<i32>",
            "0..",
            "x?",
            "return",
            "fn g(x: i32)\n-> i32 { x }",
            "struct P(i32);",
            "const N: i32 = 1",
            "union",
            "(1, 2)",
            "\"s\"",
        ];
        let open = [
            "1 /* (",
            "f\"{x}",
            // A last token that needs more.
            "1 +",
            "xs.",
            "f(a,",
            "a ->",
            "x ==",
            "if a {} else",
            "for x in",
            "|x|",
            "a &&",
            // Item headers wait for their body.
            "fn g(x: i32)",
            "pub(crate) async fn g<T>(x: T) -> T",
            "struct P",
            "#[derive(Debug)]\nenum E",
            "impl Display for P",
            "trait T",
            "fn f() {}\nfn g()",
        ];
        for t in complete {
            assert!(!incomplete(&format!("{t}\n")), "{t}");
        }
        for t in open {
            assert!(incomplete(&format!("{t}\n")), "{t}");
        }
        assert!(blank("// c\n/* d */\n"));
        assert!(!blank("1 // c\n"));
    }

    #[test]
    fn a_last_let_or_assignment_gets_its_semicolon() {
        for (cell, closed) in [
            ("let x = 1", true),
            ("f(); let x = 1 // c", true),
            ("let x = if c { 1 } else { 2 }", true),
            ("k += 2", true),
            ("v = 5", true),
            ("xs[0] -= 1", true),
            ("let x = 1;", false),
            ("let x = 1; x", false),
            ("f(1)", false),
            ("f(a = 1)", false),
            ("x == 1", false),
            ("while true { let y = 1 }", false),
            ("{ let q = 2; q * 3 }", false),
            ("using g = m.lock() { g.get() }", false),
            ("if let Some(x) = o { x } else { 0 }", false),
            ("(a, b) = (b, a)", true),
            ("use core::io::stdin", true),
            ("use std::fs::{read, write}", true),
            ("pub(crate) use a::b", true),
            ("const N: i32 = 3", true),
            ("static S: str = \"s\"", true),
            ("type Id = i64", true),
            ("use a::b;", false),
            ("pub fn f() {}", false),
        ] {
            let want = if closed {
                format!("{cell}\n;")
            } else {
                cell.to_owned()
            };
            assert_eq!(close_stmt(cell), want);
        }
        assert!(matches!(classify("{ let q = 2; q * 3 }"), Cell::Expr));
        assert!(matches!(classify("{ fn f() {} f() }"), Cell::Expr));
    }

    #[test]
    fn several_statements_end_at_the_last() {
        assert!(!incomplete("let a = 1; a + 1\n"));
        assert!(!incomplete("println(\"x\"); while true { break; }\n"));
        assert_eq!(
            split_last("println(\"x\"); while true { break; }").1,
            "while true { break; }"
        );
        assert_eq!(split_last("let a = 1; a + 1").1, "a + 1");
    }

    fn repl() -> Repl {
        Repl::new(Vec::new(), BuildOptions::default()).unwrap()
    }

    fn binding(name: &str, is_it: bool) -> Binding {
        Binding {
            name: name.to_owned(),
            sym: format!("__nabi_repl_1_{name}"),
            ty: "i32".to_owned(),
            shown_ty: "i32".to_owned(),
            holds_any: false,
            is_it,
        }
    }

    fn saved(stmts: &str, expr: Option<&str>, it: bool) -> Saved {
        Saved {
            stmts: stmts.to_owned(),
            expr: expr.map(str::to_owned),
            it,
        }
    }

    #[test]
    fn doc_comments_and_commands_end_cells() {
        // A doc comment waits for its item, which it belongs to.
        assert!(incomplete("/// Doubles it.\n"));
        assert!(!incomplete(
            "/// Doubles it.\nfn twice(x: i32) -> i32 { x * 2 }\n"
        ));
        assert!(!incomplete("//// not a doc comment\n"));
        assert!(
            matches!(classify("/// Doubles it.\nfn twice(x: i32) -> i32 { x * 2 }"), Cell::Item(Some(k)) if k == "fn twice")
        );
        // A command is its line, even ending in `.` or an operator.
        assert!(!incomplete(":load .\n"));
        assert!(!incomplete(":type 1 +\n"));
        assert_eq!(
            split_cells("/// d\nfn f() {}\n:vars\nf()"),
            ["/// d\nfn f() {}", ":vars", "f()"]
        );
    }

    #[test]
    fn vars_shows_item_headers() {
        assert_eq!(
            header("/// Doubles it.\n#[inline]\nfn twice(x: i32) -> i32 { x * 2 }"),
            "fn twice(x: i32) -> i32"
        );
        assert_eq!(
            header("fn f(a: [i32; 3]) -> bool { a[0] == 1 }"),
            "fn f(a: [i32; 3]) -> bool"
        );
        assert_eq!(header("const N: i32 = 1;"), "const N: i32");
        assert_eq!(header("struct P(i32);"), "struct P(i32)");
        assert_eq!(
            header("#[derive(Debug)]\nstruct Q {\n  x: i32;\n}"),
            "struct Q"
        );
        assert_eq!(
            header("fn g(x: i32)\n    -> i32 where i32: Copy { x }"),
            "fn g(x: i32) -> i32 where i32: Copy"
        );
    }

    #[test]
    fn exit_and_it_yield_to_user_names() {
        let mut r = repl();
        assert!(!r.defines("exit") && !r.defines("it"));
        // The session's `it` is not the user's.
        r.bindings.push(binding("it", true));
        assert!(!r.defines("it"));
        r.bindings.push(binding("exit", false));
        assert!(r.defines("exit") && !r.defines("quit"));
        r.state.items.push(Item::new(
            Some("fn quit".to_owned()),
            "fn quit() {}".to_owned(),
        ));
        assert!(r.defines("quit"));
        r.bindings.clear();
        r.state
            .items
            .push(Item::new(None, "use std::process::exit;".to_owned()));
        assert!(r.defines("exit"));
        // Without one the bare word ends the session, as `:q` does.
        let mut r = repl();
        assert!(!r.cell("exit"));
        assert!(!r.cell("quit()"));
        assert!(!r.cell(":q"));
        assert!(r.cell(":frob"));
        assert!(r.cell(":opt 9"));
        assert_eq!(r.opts.opt_level, BuildOptions::default().opt_level);
        assert!(r.cell(":opt 2"));
        assert_eq!(r.opts.opt_level, "2");
        assert!(r.cell(":reload"));
    }

    #[test]
    fn completion_knows_core_imported_and_session_names() {
        let lib = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../library");
        let roots = [lib.join("core/src/lib.nabi"), lib.join("std/src/lib.nabi")];
        let mut r = Repl::new(
            nabic_interface::bootstrap_tiers(&roots),
            BuildOptions::default(),
        )
        .unwrap();
        let has = |r: &Repl, code: &str, name: &str| {
            let (start, found) = r.complete(code, code.len());
            assert!(
                found.iter().all(|f| f.starts_with(&code[start..])),
                "{found:?}"
            );
            found.iter().any(|f| f == name)
        };
        assert!(has(&r, "pri", "println"));
        assert!(has(&r, "let xs = Lis", "List"));
        assert!(has(&r, "whi", "while") && has(&r, "x = typ", "type"));
        assert!(!has(&r, "Comm", "Command"));
        r.state
            .items
            .push(Item::new(None, "use std::process::*;".to_owned()));
        r.state
            .items
            .push(Item::new(None, "use std::fs as files;".to_owned()));
        r.state.items.push(Item::new(
            Some("fn twice".to_owned()),
            "fn twice(x: i32) -> i32 { x * 2 }".to_owned(),
        ));
        assert!(has(&r, "Comm", "Command"));
        assert!(has(&r, "fil", "files"));
        assert!(has(&r, "1 + tw", "twice"));
        assert!(!has(&r, "x", "x25519") && has(&r, "tas", "task"));
        // Members after `x.` when x's type is known.
        r.state.items.push(Item::new(
            Some("struct P".to_owned()),
            "struct P { x: i32; y: i32; }".to_owned(),
        ));
        r.state.items.push(Item::new(
            None,
            "impl P { fn sum(self) -> i32 { self.x + self.y } }".to_owned(),
        ));
        let code = "let p = P { x: 1, y: 2 };\np.";
        assert_eq!(
            r.complete(code, code.len()),
            (
                code.len(),
                vec!["sum".to_owned(), "x".to_owned(), "y".to_owned()]
            )
        );
    }

    #[test]
    fn bare_completion_includes_locals_and_prelude() {
        let lib = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../library");
        let roots = [lib.join("core/src/lib.nabi"), lib.join("std/src/lib.nabi")];
        let mut r = Repl::new(
            nabic_interface::bootstrap_tiers(&roots),
            BuildOptions::default(),
        )
        .unwrap();
        r.bindings.push(binding("total", false));
        let code = "let width = 3;\nprintln(wi";
        let (start, found) = r.complete(code, code.len());
        assert_eq!(&code[start..], "wi");
        assert!(found.iter().any(|f| f == "width"), "{found:?}");
        let (_, found) = r.complete("pri", 3);
        assert!(found.iter().any(|f| f == "println"), "{found:?}");
        let (_, found) = r.complete("1 + to", 6);
        assert!(found.iter().any(|f| f == "total"), "{found:?}");
        // A list's methods, not its internals.
        let code = "let xs: List<i32> = [1];\nxs.";
        let (_, found) = r.complete(code, code.len());
        assert!(found.iter().any(|f| f == "push"), "{found:?}");
        assert!(
            !found.iter().any(|f| f == "h" || f.contains('(')),
            "{found:?}"
        );
    }

    #[test]
    fn save_writes_each_cell_as_typed() {
        let mut r = repl();
        r.state.items.push(Item::new(
            Some("fn twice".to_owned()),
            "fn twice(x: i32) -> i32 { x * 2 }".to_owned(),
        ));
        r.history = vec![
            saved("let a = 1\n;", None, false),
            // Read by the next cell: kept as `it`.
            saved("", Some("twice(a)"), true),
            saved("", Some("it + 1"), true),
            // Not read again: a statement.
            saved("", Some("it * 2"), true),
            // Declared again, with another type too: a new binding.
            saved("let a = 5;", None, false),
            saved("let a = \"s\"; println(a);", None, false),
            saved("", Some("a"), true),
        ];
        assert_eq!(
            r.script(),
            "\
// nabi repl session (:save); :load replays it cell by cell
fn twice(x: i32) -> i32 { x * 2 }
// :cell
let a = 1;
// :cell
let it = twice(a);
// :cell
let it = it + 1;
// :cell
it * 2;
// :cell
let a = 5;
// :cell
let a = \"s\"; println(a);
// :cell
a;
"
        );
        assert_eq!(
            saved_cells(&r.script()),
            [
                "fn twice(x: i32) -> i32 { x * 2 }",
                "let a = 1;",
                "let it = twice(a);",
                "let it = it + 1;",
                "it * 2;",
                "let a = 5;",
                "let a = \"s\"; println(a);",
                "a;",
            ]
        );
        let path = r.tmp.path().join("s.nabi");
        r.save(&path.display().to_string());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), r.script());
    }

    #[test]
    fn unformattable_uses_tag() {
        let src = format!("let a = 1;\n{SHOWN} = f\"{{v}}\";\n");
        let show = src.find(SHOWN).unwrap() as u32;
        let at = |start: u32| Span {
            start,
            end: start + 1,
            line: 1,
            col: 1,
            file: 7,
        };
        // The message does not matter, only the tag.
        let err = |tag: Tag, start: u32| {
            NabiDiagnostic::error(
                nabic_diagnostic::Code::MISMATCHED_TYPES,
                at(start),
                "reworded",
            )
            .with_tag(tag)
        };
        let one = |tag: Tag, fmt: Fmt| unformattable(&[err(tag, show + 4)], &src, 7, fmt);
        assert_eq!(one(Tag::NoDisplayHasDebug, Fmt::Display), Some(Fmt::Debug));
        assert_eq!(one(Tag::NoDisplay, Fmt::Display), Some(Fmt::Type));
        assert_eq!(one(Tag::NoDebug, Fmt::Debug), Some(Fmt::Type));
        assert_eq!(one(Tag::NoDebug, Fmt::Type), None);
        assert_eq!(one(Tag::None, Fmt::Display), None);
        // Outside the shown value's store, or in another file.
        let before = err(Tag::NoDisplayHasDebug, 0);
        assert_eq!(unformattable(&[before], &src, 7, Fmt::Display), None);
        let elsewhere = err(Tag::NoDisplayHasDebug, show + 4);
        assert_eq!(unformattable(&[elsewhere], &src, 8, Fmt::Display), None);
        assert_eq!(unformattable(&[], &src, 7, Fmt::Display), None);
    }

    #[test]
    fn package_names_get_a_hint() {
        let dir = nabic_core::tmp::test_dir("repl-hint");
        std::fs::create_dir_all(dir.join("src/geo")).unwrap();
        std::fs::write(
            dir.join("src/lib.nabi"),
            "pub fn add(a: i32, b: i32) -> i32 { a + b }\n",
        )
        .unwrap();
        std::fs::write(dir.join("src/geo.nabi"), "pub struct Point { x: i32; }\n").unwrap();
        let src = dir.join("src");
        assert!(defined_in(&src, "add") && defined_in(&src, "Point"));
        assert!(!defined_in(&src, "x"));
        let missing = |what: &str| {
            NabiDiagnostic::error(
                nabic_diagnostic::Code::UNRESOLVED_NAME,
                Span::DUMMY,
                format!("cannot find {what} in this scope"),
            )
        };
        let mut r = repl();
        assert!(r.package_hint(missing("function `add`")).notes.is_empty());
        r.package = Some(src);
        assert_eq!(
            r.package_hint(missing("function `add`")).notes,
            ["help: `add` is defined in this package; load this package: `nabi repl .`"]
        );
        assert!(r.package_hint(missing("value `nope`")).notes.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn failures_say_what_happened() {
        let same = "the session is unchanged";
        assert_eq!(
            failure("exit 3"),
            format!("the cell called exit(3); {same} (`exit` or `:quit` ends the session)")
        );
        assert_eq!(
            failure("throw\n<repl cell 2>:1:5\n\nbad"),
            format!("the cell threw `bad`; {same}\n  --> <repl cell 2>:1:5")
        );
        assert_eq!(
            failure("throw\n<repl cell 2>:1:5\n<repl cell 1>:2:9\nbad"),
            format!(
                "the cell threw `bad`; {same}\n  --> <repl cell 2>:1:5\n  raised at <repl cell 1>:2:9"
            )
        );
        assert_eq!(
            failure("throw\n\n\nbad"),
            format!("the cell threw `bad`; {same}")
        );
        assert_eq!(
            failure("interrupted"),
            format!("interrupted (Ctrl-C); {same}")
        );
        assert_eq!(failure("panic"), format!("the cell panicked; {same}"));
        assert_eq!(failure(""), format!("the cell failed; {same}"));
        for why in ["exit 3", "throw\n\n\nbad", "interrupted", "panic", ""] {
            assert!(!failure(why).contains("main"), "{why}");
        }
    }

    #[test]
    fn a_cell_defines_items_one_by_one() {
        let cell = "struct P { y: i32 } impl P { fn y(self) -> i32 { self.y } }";
        let items = items_of(cell);
        let keys: Vec<_> = items.iter().map(|i| i.0.as_deref()).collect();
        assert_eq!(keys, [Some("struct P"), None]);
        assert_eq!(
            &cell[items[1].1..items[1].2],
            "impl P { fn y(self) -> i32 { self.y } }"
        );
        assert_eq!(
            items_of("impl Display for P { fn fmt(self) {} }")[0]
                .0
                .as_deref(),
            Some("impl Display for P")
        );
        // Not only items: one, as written.
        assert_eq!(items_of("fn f() {} f()").len(), 1);
        assert!(matches!(
            classify("impl P { fn y(self) {} }"),
            Cell::Item(None)
        ));
    }

    #[test]
    fn an_inherent_impl_replaces_those_it_redefines() {
        let item = |t: &str| Item::new(classify_key(t), t.to_owned());
        let old = item("impl P { fn a(self) {} fn b(self) {} }");
        assert!(replaces(&item("impl P { fn b(self) {} }"), &old));
        assert!(!replaces(&item("impl P { fn c(self) {} }"), &old));
        assert!(!replaces(&item("impl Q { fn a(self) {} }"), &old));
        assert!(!replaces(&item("fn a() {}"), &old));
        assert!(replaces(&item("fn a() {}"), &item("fn a() -> i32 { 1 }")));
        assert_eq!(old.name(), "impl P");
    }

    fn classify_key(t: &str) -> Option<String> {
        items_of(t).remove(0).0
    }
}
