//! Loading a program into the session.

#[allow(clippy::wildcard_imports)]
use crate::*;

impl Repl {
    /// `repl <path>`: load `path` (`anonymous`: a lone file); `false` on
    /// failure. Checked whole first. Items join the session; top-level
    /// statements run as cells split after each top-level `let`, so a
    /// failure keeps earlier bindings. An unusable binding is dropped if
    /// nothing later names it, else the statements up to its last use run
    /// as one cell. A `fn main` program runs as one cell and `main` then
    /// leaves the session.
    pub fn load_program(&mut self, path: &std::path::Path, anonymous: bool) -> bool {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("error: cannot read {}: {e}", path.display());
                return false;
            }
        };
        self.loaded = Some((path.to_owned(), anonymous));
        if text.starts_with(SAVED) {
            return self.replay(&text);
        }
        let shown = path.display().to_string();
        let (tokens, _) = nabic_lexer::Lexer::new(&text, shown.clone()).lex_all();
        let tops = nabic_parser::parse_program(&tokens, shown.clone()).0.tops;
        let session = if anonymous {
            self.tmp.path().join("repl.nabi")
        } else {
            path.with_file_name("repl.nabi")
        };
        // Top-level elements: (start, end, is_item).
        let tops: Vec<(usize, usize, bool)> = tops
            .iter()
            .enumerate()
            .map(|(i, &(start, item))| {
                let end = tops.get(i + 1).map_or(text.len(), |t| t.0 as usize);
                let start = start as usize;
                (start, start + text[start..end].trim_end().len(), item)
            })
            .collect();
        let mut items = Vec::new();
        // The statements' text with the items blanked out (newlines
        // kept): offsets in it are offsets in the file.
        let mut body = text.clone().into_bytes();
        for &(start, end, _) in tops.iter().filter(|t| t.2) {
            let item = text[start..end].to_owned();
            let key = match classify(&item) {
                Cell::Item(k) => k,
                _ => None,
            };
            items.push((key, item, start));
            for b in &mut body[start..end] {
                if *b != b'\n' {
                    *b = b' ';
                }
            }
        }
        let body = String::from_utf8_lossy(&body).into_owned();
        let stmts: Vec<(usize, usize)> = tops
            .iter()
            .filter(|t| !t.2)
            .map(|&(start, end, _)| (start, end))
            .collect();
        // One cell per stretch ending at a `let` (or the last statement).
        let mut cells: Vec<(usize, usize)> = Vec::new();
        let mut from = None;
        for (i, &(start, end)) in stmts.iter().enumerate() {
            let from_here = *from.get_or_insert(start);
            if matches!(classify(&text[start..end]), Cell::Let) || i + 1 == stmts.len() {
                cells.push((from_here, end));
                from = None;
            }
        }
        self.program = Some(Program {
            session,
            anonymous,
            path: shown,
            text: Arc::from(text.as_str()),
            items: items.iter().map(|(_, t, at)| (t.clone(), *at)).collect(),
            cell: None,
        });
        let mut next = self.state.clone();
        // Its items replace the session's of the same name (`:reload`).
        next.items
            .retain(|i| i.key.is_none() || !items.iter().any(|(n, ..)| *n == i.key));
        next.items
            .extend(items.into_iter().map(|(k, t, _)| Item::new(k, t)));
        let all = match (cells.first(), cells.last()) {
            (Some(f), Some(l)) => &body[f.0..l.1],
            _ => "",
        };
        self.set_cell(all, cells.first().map(|c| c.0));
        let checked = self
            .compile(&next, &self.session(&next, all, None), false)
            .is_some();
        self.set_cell("", None);
        if !checked {
            if let Some(p) = &mut self.program {
                p.items.clear();
            }
            return false;
        }
        let main = Some("fn main".to_owned());
        if next.items.iter().any(|i| i.key == main) {
            self.state = next.clone();
            next.items.retain(|i| i.key != main);
            let ok = self.run_cell("", None, false, &[]).is_ok();
            self.state = next;
            return ok;
        }
        self.state = next;
        let mut ok = true;
        let mut i = 0;
        while ok && i < cells.len() {
            // Cells `i..=j` run as one; `may_drop`: bindings in them that
            // later cells cannot use and need not.
            let (mut j, mut may_drop) = (i, Vec::new());
            loop {
                let text = &body[cells[i].0..cells[j].1];
                self.set_cell(text, Some(cells[i].0));
                match self.run_cell(text, None, false, &may_drop) {
                    Ok(_) => {
                        self.history.push(Saved {
                            stmts: text.trim().to_owned(),
                            ..Saved::default()
                        });
                        break;
                    }
                    // Merged through the last cell that names it.
                    Err(Fail::Unkept(name, _)) => {
                        match (j + 1..cells.len())
                            .rev()
                            .find(|&k| names(&body[cells[k].0..cells[k].1], &name))
                        {
                            Some(k) => j = k,
                            None => may_drop.push(name),
                        }
                    }
                    Err(Fail::Reported) => {
                        ok = false;
                        if j + 1 < cells.len() {
                            eprintln!("note: the rest of the program did not run");
                        }
                        break;
                    }
                }
            }
            i = j + 1;
        }
        self.set_cell("", None);
        ok
    }

    /// Run a `:save` script's cells (`saved_cells`) one by one, as
    /// typed: a name declared again replaces the old binding.
    pub(crate) fn replay(&mut self, text: &str) -> bool {
        for c in saved_cells(text) {
            self.next_cell();
            if self.eval(&c).is_err() {
                eprintln!("note: the rest of the session did not run");
                return false;
            }
        }
        true
    }

    /// The cell being built is `text`, at `at` in the loaded program.
    pub(crate) fn set_cell(&mut self, text: &str, at: Option<usize>) {
        text.clone_into(&mut self.input);
        if let Some(p) = &mut self.program {
            p.cell = at;
        }
    }
}
