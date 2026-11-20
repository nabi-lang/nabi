//! `:` commands and session queries.

#[allow(clippy::wildcard_imports)]
use crate::*;

impl Repl {
    /// A `:` command; `false` to stop.
    pub(crate) fn command(&mut self, cmd: &str, arg: &str) -> bool {
        let needs = |what: &str, example: &str| {
            eprintln!("error: `{cmd}` takes {what}: `{cmd} {example}`");
        };
        match cmd {
            ":q" | ":quit" | ":exit" => return false,
            ":reset" => self.reset(),
            ":help" | ":h" => print!("{HELP}"),
            ":type" | ":t" if arg.is_empty() => needs("an expression", "1 + 2"),
            ":type" | ":t" => {
                arg.clone_into(&mut self.input);
                if let Some(ty) = self.type_of(arg, false) {
                    println!("{ty}");
                }
            }
            ":add" if arg.is_empty() => needs("a dependency", "<name> --path <dir>"),
            ":add" => self.add_dep(arg),
            ":doc" => {
                let start = arg
                    .trim_end_matches(|c: char| c.is_alphanumeric() || c == '_')
                    .len();
                if start == arg.len() {
                    needs("a name", "xs.len");
                } else if let Some(text) = self
                    .help(arg, start, arg.len())
                    // A method is found in a call.
                    .or_else(|| self.help(&format!("{arg}()"), start, arg.len()))
                {
                    println!("{text}");
                } else {
                    eprintln!("error: the session defines no `{arg}`; `:vars` lists what it does");
                }
            }
            ":time" if arg.is_empty() => needs("an expression", "fib(30)"),
            ":time" => {
                let t = std::time::Instant::now();
                self.timing = true;
                self.next_cell();
                let r = self.eval(arg);
                self.timing = false;
                if let Ok(Some(shown)) = r {
                    println!("{shown}");
                }
                eprintln!("[repl] total: {:.1}ms", t.elapsed().as_secs_f64() * 1e3);
            }
            ":vars" => self.vars(),
            ":load" if arg.is_empty() => needs("a file", "prog.nabi"),
            ":load" if std::path::Path::new(arg).is_dir() => eprintln!(
                "error: `:load` takes a `.nabi` file; for a package directory start `nabi repl {arg}`"
            ),
            ":load" => {
                self.load_program(std::path::Path::new(arg), true);
            }
            ":reload" => match self.loaded.clone() {
                Some((path, anonymous)) => {
                    self.load_program(&path, anonymous);
                }
                None => eprintln!("error: no program loaded yet; load one with `:load <file>`"),
            },
            ":edit" => return self.edit(),
            ":save" if arg.is_empty() => needs("a file", "session.nabi"),
            ":save" => self.save(arg),
            ":opt" => match arg {
                "" => println!("-O{}", self.opts.opt_level),
                "0" | "1" | "2" | "3" | "s" => {
                    arg.clone_into(&mut self.opts.opt_level);
                    // Code built at the old level is built again.
                    self.known.clear();
                }
                _ => eprintln!(
                    "error: `:opt` takes 0, 1, 2, 3 or s (as `-O`), not `{arg}`: `:opt 2`"
                ),
            },
            _ => eprintln!("unknown command `{cmd}` (:help lists them)"),
        }
        true
    }

    /// `:reset`: forget every cell, then run the prelude again.
    pub(crate) fn reset(&mut self) {
        self.state = State::default();
        self.bindings.clear();
        self.statics.clear();
        self.any_types.clear();
        self.history.clear();
        if let Some(p) = self.prelude.clone() {
            self.run_prelude(&p);
        }
    }

    /// Whether the session's own code names `name`: a binding (not `it`),
    /// an item, or a `use` of it.
    pub(crate) fn defines(&self, name: &str) -> bool {
        self.bindings.iter().any(|b| b.name == name && !b.is_it)
            || self.state.items.iter().any(|i| match &i.key {
                Some(k) => !k.starts_with("impl ") && k.split(' ').nth(1) == Some(name),
                None => i.text.starts_with("use ") && names(&i.text, name),
            })
    }

    /// Help for the name at `code[start..end]`: the language
    /// server's hover as text, from the cell checked in the session, else
    /// from the expression ending at the name alone.
    pub fn help(&self, code: &str, start: usize, end: usize) -> Option<String> {
        let at = |body: &str, off: usize| {
            let (src, base) = self.source(body);
            let (a, key) = self.analyze(&src);
            nabic_tools::hover::hover(&a, &key, &src, base + off)
        };
        let from = nabic_tools::cursor::receiver_start(code, end);
        let md = at(code, start).or_else(|| at(&format!("{};", &code[from..end]), start - from))?;
        let lines: Vec<&str> = md.lines().filter(|l| !l.starts_with("```")).collect();
        Some(lines.join("\n"))
    }

    /// `:vars`: the bindings as `name: type`, types aligned, then the
    /// named items' headers.
    pub(crate) fn vars(&self) {
        let rows: Vec<(String, String)> = self
            .bindings
            .iter()
            .map(|b| (format!("{}:", b.name), b.shown_ty.clone()))
            .collect();
        print!("{}", nabic_core::term::list(&rows, None, ""));
        let named = self
            .state
            .items
            .iter()
            .filter(|i| i.key.as_deref().is_some_and(|k| !k.starts_with("impl ")));
        for item in named {
            println!("{}", header(&item.text));
        }
    }

    /// `:edit`: cells written in `$EDITOR` run as typed; `false` to stop.
    /// The text of a run that failed opens again.
    pub(crate) fn edit(&mut self) -> bool {
        let file = self.tmp.path().join("edit.nabi");
        if !file.exists() {
            if let Err(e) = std::fs::write(&file, "") {
                eprintln!("error: cannot write {}: {e}", file.display());
                return true;
            }
        }
        let editor = ["VISUAL", "EDITOR"]
            .iter()
            .filter_map(std::env::var_os)
            .find(|e| !e.is_empty())
            .and_then(|e| e.into_string().ok())
            .unwrap_or_else(|| if cfg!(windows) { "notepad" } else { "vi" }.to_owned());
        let mut words = editor.split_whitespace();
        let Some(exe) = words.next() else { return true };
        match Command::new(exe).args(words).arg(&file).status() {
            Ok(s) if s.success() => {}
            Ok(s) => {
                eprintln!("error: `{editor}` failed ({s}); nothing ran");
                return true;
            }
            Err(e) => {
                eprintln!("error: cannot run `{editor}`: {e}; set `EDITOR` to your editor");
                return true;
            }
        }
        let text = std::fs::read_to_string(&file).unwrap_or_default();
        for c in split_cells(&text) {
            let r = if c.starts_with(':') || EXIT.contains(&c.as_str()) {
                if !self.cell(&c) {
                    return false;
                }
                Ok(None)
            } else {
                self.next_cell();
                self.eval(&c)
            };
            match r {
                Ok(Some(shown)) => println!("{shown}"),
                Ok(None) => {}
                Err(()) => return true,
            }
        }
        let _ = std::fs::write(&file, "");
        true
    }

    /// `:save <file>`: the session's items, then the cells that ran, as a
    /// script. An expression cell is a statement, or `let it = ..` where a
    /// later cell reads `it`.
    pub(crate) fn save(&self, path: &str) {
        match std::fs::write(path, self.script()) {
            Ok(()) => eprintln!("wrote {path}"),
            Err(e) => eprintln!("error: cannot write {path}: {e}"),
        }
    }

    /// The session as a script (`:save`): each cell as typed, a
    /// redeclared name a new binding as in a file. `SAVED` heads it and
    /// `CELL` marks each cell, so `:load` replays the cells (`replay`).
    pub(crate) fn script(&self) -> String {
        let mut out = format!("{SAVED}\n");
        for item in &self.state.items {
            out.push_str(&item.text);
            out.push('\n');
        }
        for (i, s) in self.history.iter().enumerate() {
            let mut read = false;
            for later in &self.history[i + 1..] {
                let text = format!("{}{}", later.stmts, later.expr.as_deref().unwrap_or(""));
                read = s.it && names(&text, "it");
                if read || later.it {
                    break;
                }
            }
            let _ = writeln!(out, "{CELL}");
            match s.stmts.strip_suffix("\n;") {
                Some(t) => {
                    let _ = write!(out, "{t};");
                }
                None => out.push_str(&s.stmts),
            }
            // `it` is declared only where a later cell reads it.
            match &s.expr {
                Some(e) if read => {
                    let _ = write!(out, "let it = {e};");
                }
                Some(e) => {
                    let _ = write!(out, "{e};");
                }
                None => {}
            }
            out.push('\n');
        }
        out
    }

    /// Run the prelude (`repl <lib>`), as no numbered cell.
    pub fn run_prelude(&mut self, cell: &str) {
        let n = std::mem::take(&mut self.cell_no);
        if let Ok(Some(shown)) = self.eval(cell) {
            println!("{shown}");
        }
        self.cell_no = n;
    }

    /// One cell: what it shows (the value, or its type), or `Err`
    /// after its errors went to stderr.
    #[allow(clippy::result_unit_err)]
    pub fn eval(&mut self, cell: &str) -> Result<Option<String>, ()> {
        // Diagnostics quote the cell as typed (`regions`).
        cell.clone_into(&mut self.input);
        let cell = &close_stmt(cell);
        self.add_official(cell);
        if let Cell::Item(_) = classify(cell) {
            return self.item(cell).then_some(None).ok_or(());
        }
        // The cell's value is its last top-level statement's.
        let (stmts, last) = split_last(cell);
        let saved = |stmts: &str, expr: Option<&str>, it: bool| Saved {
            stmts: stmts.to_owned(),
            expr: expr.map(str::to_owned),
            it,
        };
        let (r, ran) = match classify(last) {
            // A statement (a `let` too) shows nothing.
            Cell::Item(_) | Cell::Let | Cell::Stmt => {
                (self.run_once(cell, None, false), saved(cell, None, false))
            }
            Cell::Expr => {
                // A trailing `?` on a bare top-level expression asks for the
                // `Debug` form: error propagation needs a `throws` context,
                // which a top-level expression has not, so the slot is free.
                let (expr, debug) = match last.trim_end().strip_suffix('?') {
                    Some(e) => (e.trim_end(), true),
                    None => (last, false),
                };
                let before = self.it().map(str::to_owned);
                let r = self.run_once(
                    &format!("{stmts}let {VALUE} = ({expr});"),
                    Some(VALUE),
                    debug,
                );
                if r.is_err() && self.diverged.take() {
                    let text = format!("{stmts}{expr};");
                    (self.run_once(&text, None, false), saved(&text, None, false))
                } else {
                    let it = self.it().is_some() && self.it() != before.as_deref();
                    (r, saved(stmts, Some(expr), it))
                }
            }
        };
        if r.is_ok() {
            self.history.push(ran);
        }
        r
    }

    /// The slot of the session's `it`.
    pub(crate) fn it(&self) -> Option<&str> {
        let b = self.bindings.iter().find(|b| b.is_it)?;
        Some(&b.sym)
    }

    /// The type of `expr` in the session, without running it; `quiet`
    /// keeps its errors unreported.
    pub(crate) fn type_of(&self, expr: &str, quiet: bool) -> Option<String> {
        let src = self.session(&self.state, &format!("let {VALUE} = ({expr});"), None);
        self.compile(&self.state, &src, quiet)
            .and_then(|r| binding_ty(&r, VALUE))
    }

    /// The names the session defines: bindings, then items.
    pub(crate) fn names(&self) -> Vec<&str> {
        let items = self.state.items.iter().filter_map(|i| i.key.as_deref());
        let items = items
            .filter(|k| !k.starts_with("impl "))
            .filter_map(|k| k.split(' ').nth(1));
        self.bindings
            .iter()
            .map(|b| b.name.as_str())
            .chain(items)
            .collect()
    }

    /// Completions of the word ending at byte `end` of the cell `code`:
    /// where the word starts and the candidates, sorted: the language
    /// server's completion, with the session's names. The terminal and
    /// the kernel share it.
    pub fn complete(&self, code: &str, end: usize) -> (usize, Vec<String>) {
        let session = self.source("").0;
        let mut checked = self.checked.borrow_mut();
        if checked.as_ref().is_none_or(|(s, ..)| *s != session) {
            let (a, key) = self.analyze(&session);
            *checked = Some((session, a, key));
        }
        let (_, good, key) = checked.as_ref().expect("checked above");
        let (src, at) = self.source(code);
        let compile = |t: &str| self.analyze(t).0;
        let names = self.names();
        let (start, found) =
            nabic_tools::complete::complete(&src, key, at + end, compile, Some(good), &names);
        let mut found: Vec<String> = found.into_iter().map(|(l, ..)| l).collect();
        found.sort();
        (start - at, found)
    }
}

/// The first line of a `:save` script, and the line before each cell.
pub(crate) const SAVED: &str = "// nabi repl session (:save); :load replays it cell by cell";

pub(crate) const CELL: &str = "// :cell";

/// The cells of a `:save` script (`Repl::script`): its items, then the
/// text after each `CELL` line.
pub(crate) fn saved_cells(text: &str) -> Vec<String> {
    let mut cells = vec![String::new()];
    for line in text.lines().skip(1) {
        if line == CELL {
            cells.push(String::new());
        } else if let Some(c) = cells.last_mut() {
            c.push_str(line);
            c.push('\n');
        }
    }
    cells
        .into_iter()
        .filter(|c| !blank(c))
        .map(|c| c.trim().to_owned())
        .collect()
}
