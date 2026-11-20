//! Directives: `#define`, `#include`, skipped groups and pragmas.

#[allow(clippy::wildcard_imports)]
use crate::*;

impl Pp<'_> {
    pub(crate) fn directive(&mut self) -> Result<Option<Tok>, Error> {
        let Some(d) = self.line_tok() else {
            return Ok(None);
        };
        let name = self.spell(d).to_owned();
        {
            let (_, conds, guard, _, _) = self.top_file();
            match *guard {
                Guard::After(_) => *guard = Guard::None,
                Guard::Start if name != "ifndef" && name != "if" => *guard = Guard::None,
                Guard::In(_) if conds.len() == 1 && matches!(name.as_str(), "else" | "elif") => {
                    *guard = Guard::None;
                }
                _ => {}
            }
        }
        match name.as_str() {
            "define" => self.define()?,
            "undef" => {
                let l = self.line();
                match l.first() {
                    Some(t) if t.kind == Kind::Ident => {
                        self.macros.remove(&t.sym);
                    }
                    _ => return Err(self.err(d.loc, "macro name must be an identifier")),
                }
            }
            "include" | "include_next" => self.include(d, name == "include_next", false)?,
            "import" => self.include(d, false, true)?,
            "if" | "ifdef" | "ifndef" => {
                let l = self.line();
                let start = {
                    let (_, conds, guard, _, _) = self.top_file();
                    *guard == Guard::Start && conds.is_empty()
                };
                let v = if name == "if" {
                    if start {
                        // `#if !defined(X)` guards like `#ifndef X`.
                        let g = self.not_defined(&l);
                        let (_, _, guard, _, _) = self.top_file();
                        *guard = g.map_or(Guard::None, Guard::In);
                    }
                    self.eval_if(&l, d.loc)?
                } else {
                    let Some(x) = l.first().filter(|t| t.kind == Kind::Ident) else {
                        return Err(self.err(d.loc, "macro name must be an identifier"));
                    };
                    if start {
                        let (_, _, guard, _, _) = self.top_file();
                        *guard = if name == "ifndef" {
                            Guard::In(x.sym)
                        } else {
                            Guard::None
                        };
                    }
                    self.macros.contains_key(&x.sym) == (name == "ifdef")
                };
                let (_, conds, _, _, _) = self.top_file();
                conds.push(Cond {
                    taken: v,
                    loc: d.loc,
                });
                if !v {
                    self.skip_group()?;
                }
            }
            "elif" | "elifdef" | "elifndef" | "else" => {
                self.line();
                let (_, conds, _, _, _) = self.top_file();
                if conds.is_empty() {
                    return Err(self.err(d.loc, &format!("#{name} without #if")));
                }
                // The group before was taken: skip to the #endif.
                self.skip_group()?;
            }
            "endif" => {
                self.line();
                let (_, conds, guard, _, _) = self.top_file();
                if conds.pop().is_none() {
                    return Err(self.err(d.loc, "#endif without #if"));
                }
                if conds.is_empty() {
                    if let Guard::In(g) = *guard {
                        *guard = Guard::After(g);
                    }
                }
            }
            "error" => {
                let (lx, _, _, _, _) = self.top_file();
                let msg = lx.rest_of_line();
                return Err(self.err(d.loc, &format!("#error {msg}")));
            }
            "pragma" => {
                let l = self.line();
                return self.pragma(&l, d.loc);
            }
            "warning" | "line" | "ident" | "sccs" | "assert" | "unassert" => {
                let (lx, _, _, _, _) = self.top_file();
                lx.rest_of_line();
            }
            _ if d.kind == Kind::Num => {
                self.line();
            }
            _ => {
                return Err(self.err(d.loc, &format!("invalid preprocessing directive `#{name}`")));
            }
        }
        Ok(None)
    }

    /// `X` of `!defined X` / `!defined(X)` alone.
    pub(crate) fn not_defined(&self, l: &[Tok]) -> Option<Sym> {
        let s: Vec<&str> = l.iter().map(|t| self.spell(*t)).collect();
        match s.as_slice() {
            ["!", "defined", _] | ["!", "defined", "(", _, ")"] => {
                let x = if s.len() == 3 { l[2] } else { l[3] };
                (x.kind == Kind::Ident).then_some(x.sym)
            }
            _ => None,
        }
    }

    /// Skip lines to the `#else` / `#elif` that takes over, or the
    /// `#endif`, of the innermost group.
    pub(crate) fn skip_group(&mut self) -> Result<(), Error> {
        enum Act {
            Nest,
            Endif,
            Else,
            Elif(Tok),
        }
        let mut depth = 0u32;
        loop {
            let act = {
                let names = &mut self.names;
                let Some(Source::File { lx, conds, .. }) = self.stack.last_mut() else {
                    unreachable!("skipping outside a file")
                };
                let t = lx.next(names);
                if t.kind == Kind::Eof {
                    let loc = conds.last().map_or(t.loc, |c| c.loc);
                    return Err(Error::at(
                        &self.files,
                        loc,
                        "unterminated conditional directive",
                    ));
                }
                if !(t.has(BOL) && t.kind == Kind::Punct && names.get(t.sym) == "#") {
                    lx.skip_line();
                    continue;
                }
                let Some(d) = lx.next_in_line(names) else {
                    continue;
                };
                match names.get(d.sym) {
                    "if" | "ifdef" | "ifndef" => Act::Nest,
                    "endif" => Act::Endif,
                    "else" => Act::Else,
                    "elif" | "elifdef" | "elifndef" if depth == 0 => Act::Elif(d),
                    _ => {
                        lx.skip_line();
                        continue;
                    }
                }
            };
            match act {
                Act::Nest => {
                    depth += 1;
                    self.top_file().0.skip_line();
                }
                Act::Endif if depth > 0 => {
                    depth -= 1;
                    self.top_file().0.skip_line();
                }
                Act::Endif => {
                    let (lx, conds, guard, _, _) = self.top_file();
                    lx.skip_line();
                    conds.pop();
                    if conds.is_empty() {
                        if let Guard::In(g) = *guard {
                            *guard = Guard::After(g);
                        }
                    }
                    return Ok(());
                }
                Act::Else => {
                    let (lx, conds, _, _, _) = self.top_file();
                    lx.skip_line();
                    if depth == 0 {
                        let c = conds.last_mut().expect("a group");
                        if !c.taken {
                            c.taken = true;
                            return Ok(());
                        }
                    }
                }
                Act::Elif(d) => {
                    let taken = self.top_file().1.last().is_some_and(|c| c.taken);
                    if taken {
                        self.top_file().0.skip_line();
                        continue;
                    }
                    let name = self.spell(d).to_owned();
                    let l = self.line();
                    let v = if name == "elif" {
                        self.eval_if(&l, d.loc)?
                    } else {
                        let Some(x) = l.first().filter(|t| t.kind == Kind::Ident) else {
                            return Err(self.err(d.loc, "macro name must be an identifier"));
                        };
                        self.macros.contains_key(&x.sym) == (name == "elifdef")
                    };
                    if v {
                        let (_, conds, _, _, _) = self.top_file();
                        conds.last_mut().expect("a group").taken = true;
                        return Ok(());
                    }
                }
            }
        }
    }

    pub(crate) fn define(&mut self) -> Result<(), Error> {
        let l = self.line();
        let Some(&name) = l.first().filter(|t| t.kind == Kind::Ident) else {
            let loc = l.first().map_or_else(Loc::default, |t| t.loc);
            return Err(self.err(loc, "macro name must be an identifier"));
        };
        let mut i = 1;
        let mut params = None;
        let mut variadic = false;
        if l.get(1).is_some_and(|t| self.is(*t, "(") && !t.has(SPACE)) {
            let mut ps = Vec::new();
            i = 2;
            loop {
                let Some(&t) = l.get(i) else {
                    return Err(self.err(name.loc, "missing ')' in macro parameter list"));
                };
                i += 1;
                if self.is(t, ")") && ps.is_empty() {
                    break;
                }
                if self.is(t, "...") {
                    ps.push(kw::VA_ARGS);
                    variadic = true;
                } else if t.kind == Kind::Ident {
                    ps.push(t.sym);
                    if l.get(i).is_some_and(|t| self.is(*t, "...")) {
                        variadic = true;
                        i += 1;
                    }
                } else {
                    return Err(self.err(t.loc, "invalid token in macro parameter list"));
                }
                match l.get(i) {
                    Some(t) if self.is(*t, ",") && !variadic => i += 1,
                    Some(t) if self.is(*t, ")") => {
                        i += 1;
                        break;
                    }
                    Some(t) => {
                        return Err(self.err(t.loc, "expected ',' or ')' in macro parameter list"));
                    }
                    None => {
                        return Err(self.err(name.loc, "missing ')' in macro parameter list"));
                    }
                }
            }
            params = Some(ps);
        }
        let mut body: Vec<Tok> = l[i.min(l.len())..].to_vec();
        if let Some(b) = body.first_mut() {
            b.flags &= !SPACE;
        }
        self.defs.push(MacroDef {
            name: name.sym,
            loc: name.loc,
            function_like: params.is_some(),
            body: body.clone(),
        });
        self.macros.insert(
            name.sym,
            Rc::new(Macro {
                params,
                variadic,
                body,
                builtin: None,
            }),
        );
        Ok(())
    }

    /// The header name of an `#include` line: `<..>` or `".."`.
    pub(crate) fn header_name(&mut self, d: Tok) -> Result<(String, Loc), Error> {
        let (lx, _, _, _, _) = self.top_file();
        if let Some(h) = lx.header_name() {
            self.line();
            return Ok(h);
        }
        // `#include MACRO`.
        let l = self.line();
        let loc = l.first().map_or(d.loc, |t| t.loc);
        let toks = self.expand_isolated(l)?;
        let text = self.join(&toks);
        if (text.starts_with('<') && text.ends_with('>'))
            || (text.starts_with('"') && text.ends_with('"') && text.len() >= 2)
        {
            return Ok((text, loc));
        }
        Err(self.err(loc, "expected \"FILENAME\" or <FILENAME>"))
    }

    /// Tokens' spellings, a space where there was whitespace.
    pub(crate) fn join(&self, toks: &[Tok]) -> String {
        let mut s = String::new();
        for (i, t) in toks.iter().enumerate() {
            if i > 0 && t.has(SPACE) {
                s.push(' ');
            }
            s.push_str(self.spell(*t));
        }
        s
    }

    /// Find `name`: its path, search list index, whether it is a system
    /// header, and whether it is one of the built-in headers.
    pub(crate) fn find(
        &mut self,
        name: &str,
        angled: bool,
        next: bool,
    ) -> Option<(PathBuf, Option<usize>, bool, bool)> {
        let (cur, cur_dir) = self
            .stack
            .iter()
            .rev()
            .find_map(|s| match s {
                Source::File { file, dir, .. } => Some((*file, *dir)),
                Source::Tokens { .. } => None,
            })
            .unwrap_or((0, None));
        if Path::new(name).is_absolute() {
            let p = PathBuf::from(name);
            return self.exists(&p).then_some((p, None, false, false));
        }
        if !angled && !next {
            let f = &self.files[cur as usize];
            let (system, builtin) = (f.system, f.builtin);
            if builtin && f.path.starts_with(BUILTIN_DIR) {
                if crate::builtin::header(name).is_some() {
                    let bi = self.dirs.iter().position(|d| d.kind == DirKind::Builtin);
                    return Some((Path::new(BUILTIN_DIR).join(name), bi, true, true));
                }
            } else if !builtin {
                let p = f.path.parent().unwrap_or_else(|| Path::new("")).join(name);
                if self.exists(&p) {
                    return Some((p, None, system, false));
                }
            }
        }
        // A framework's own sub-frameworks first, as clang looks:
        // `<AE/AE.h>` from `CoreServices.framework/Headers/..` is in
        // `CoreServices.framework/Frameworks/AE.framework/Headers`.
        if let Some((fw, rest)) = name.split_once('/').filter(|_| !next) {
            let f = &self.files[cur as usize];
            let system = f.system;
            let umbrellas: Vec<PathBuf> = f
                .path
                .ancestors()
                .filter(|a| a.extension().is_some_and(|e| e == "framework"))
                .map(Path::to_path_buf)
                .collect();
            for u in umbrellas {
                let p = u
                    .join("Frameworks")
                    .join(format!("{fw}.framework"))
                    .join("Headers")
                    .join(rest);
                if self.exists(&p) {
                    return Some((p, cur_dir, system, false));
                }
            }
        }
        let start = if next {
            cur_dir.map_or(0, |d| d + 1)
        } else {
            0
        };
        for i in start..self.dirs.len() {
            let d = &self.dirs[i];
            let system = d.system;
            match d.kind {
                DirKind::Quote if angled => {}
                DirKind::Builtin => {
                    if crate::builtin::header(name).is_some() {
                        return Some((Path::new(BUILTIN_DIR).join(name), Some(i), true, true));
                    }
                }
                DirKind::Framework => {
                    let Some((fw, rest)) = name.split_once('/') else {
                        continue;
                    };
                    let p = d
                        .path
                        .join(format!("{fw}.framework"))
                        .join("Headers")
                        .join(rest);
                    if self.exists(&p) {
                        return Some((p, Some(i), system, false));
                    }
                }
                DirKind::Quote | DirKind::Normal => {
                    let p = d.path.join(name);
                    if self.exists(&p) {
                        return Some((p, Some(i), system, false));
                    }
                }
            }
        }
        None
    }

    /// `#include`; `#import` (`once`) reads each file once.
    pub(crate) fn include(&mut self, d: Tok, next: bool, once: bool) -> Result<(), Error> {
        let (spec, loc) = self.header_name(d)?;
        let angled = spec.starts_with('<');
        let name = &spec[1..spec.len() - 1];
        let Some((path, dir, system, builtin)) = self.find(name, angled, next) else {
            return Err(self.err(loc, &format!("'{name}' file not found")));
        };
        if self.stack.len() > 300 {
            return Err(self.err(loc, "#include nested too deeply"));
        }
        if let Some(g) = self.guards.get(&path) {
            if self.macros.contains_key(g) {
                return Ok(());
            }
        }
        if !builtin && (once || !self.once.is_empty()) {
            let real = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
            if self.once.contains(&real) {
                return Ok(());
            }
            if once {
                self.once.insert(real);
            }
        }
        let src: Rc<[u8]> = if builtin {
            Rc::from(crate::builtin::header(name).unwrap_or("").as_bytes())
        } else {
            match self.read(&path) {
                Some(s) => s,
                None => return Err(self.err(loc, &format!("cannot read '{}'", path.display()))),
            }
        };
        let file = self.file_id(&path, system, builtin);
        self.stack.push(Source::File {
            lx: Lexer::new(file, src),
            file,
            dir,
            conds: Vec::new(),
            guard: Guard::Start,
            key: path,
        });
        Ok(())
    }
}

impl Pp<'_> {
    /// `_Pragma("..")` and `__pragma(..)`.
    pub(crate) fn pragma_operator(&mut self, t: Tok) -> Result<Option<Tok>, Error> {
        let args = self.paren_args(t)?;
        let toks = if t.sym == kw::PRAGMA_OP {
            // clang expands the operand.
            let args = if args.len() == 1 && args[0].kind == Kind::Str {
                args
            } else {
                self.expand_isolated(args)?
            };
            let [s] = args.as_slice() else {
                return Err(self.err(t.loc, "_Pragma takes a parenthesized string literal"));
            };
            let s = self.spell(*s).to_owned();
            let s = s.trim_start_matches(['L', 'u', 'U', '8']);
            let inner = s
                .get(1..s.len().saturating_sub(1))
                .unwrap_or("")
                .replace("\\\"", "\"")
                .replace("\\\\", "\\");
            relex(&inner, &mut self.names, t.loc)
        } else {
            args
        };
        self.pragma(&toks, t.loc)
    }

    pub(crate) fn pragma(&mut self, l: &[Tok], loc: Loc) -> Result<Option<Tok>, Error> {
        let words: Vec<String> = l
            .iter()
            .take(2)
            .map(|t| self.spell(*t).to_owned())
            .collect();
        let w: Vec<&str> = words.iter().map(String::as_str).collect();
        match w.as_slice() {
            ["once", ..] => {
                if let Some(Source::File { key, .. }) = self
                    .stack
                    .iter()
                    .rev()
                    .find(|s| matches!(s, Source::File { .. }))
                {
                    let real = std::fs::canonicalize(key).unwrap_or_else(|_| key.clone());
                    self.once.insert(real);
                }
            }
            ["pack", ..] => return self.pragma_pack(&l[1..], loc).map(Some),
            ["clang", "assume_nonnull"] => {
                let begin = l.get(2).is_some_and(|t| self.spell(*t) == "begin");
                let mut t = tok(Kind::Pragma, Sym::from(begin), loc);
                t.flags = ASSUME_NONNULL;
                return Ok(Some(t));
            }
            ["GCC" | "clang", "system_header"] => {
                if let Some(f) = self.stack.iter().rev().find_map(|s| match s {
                    Source::File { file, .. } => Some(*file),
                    Source::Tokens { .. } => None,
                }) {
                    self.files[f as usize].system = true;
                }
            }
            ["push_macro" | "pop_macro", ..] => {
                let push = w[0] == "push_macro";
                let name = l
                    .iter()
                    .find(|t| t.kind == Kind::Str)
                    .map(|t| self.spell(*t).trim_matches('"').to_owned());
                if let Some(n) = name {
                    let s = self.names.intern(&n);
                    if push {
                        let cur = self.macros.get(&s).cloned();
                        self.pushed.entry(s).or_default().push(cur);
                    } else if let Some(prev) = self.pushed.get_mut(&s).and_then(Vec::pop) {
                        match prev {
                            Some(m) => self.macros.insert(s, m),
                            None => self.macros.remove(&s),
                        };
                    }
                }
            }
            ["GCC", "error"] => {
                return Err(self.err(loc, &format!("#pragma GCC error {}", self.join(&l[2..]))));
            }
            _ => {}
        }
        Ok(None)
    }

    /// `#pragma pack(..)`: the new packing as a `Pragma` token.
    pub(crate) fn pragma_pack(&mut self, l: &[Tok], loc: Loc) -> Result<Tok, Error> {
        let inner: Vec<Tok> = match l {
            [o, rest @ .., c] if self.is(*o, "(") && self.is(*c, ")") => rest.to_vec(),
            _ => return Err(self.err(loc, "missing '(' after '#pragma pack'")),
        };
        let inner = self.expand_isolated(inner)?;
        let mut parts: Vec<Vec<Tok>> = vec![Vec::new()];
        for t in inner {
            if self.is(t, ",") {
                parts.push(Vec::new());
            } else {
                parts.last_mut().expect("a part").push(t);
            }
        }
        let num = |pp: &Self, p: &[Tok]| -> Option<u64> {
            match p {
                [t] if t.kind == Kind::Num => parse_int(pp.spell(*t)).map(|(v, _)| v),
                _ => None,
            }
        };
        let ident = |p: &[Tok]| match p {
            [t] if t.kind == Kind::Ident => Some(t.sym),
            _ => None,
        };
        let valid = |n: u64| matches!(n, 1 | 2 | 4 | 8 | 16);
        let first = parts[0].first().map(|t| self.spell(*t).to_owned());
        match first.as_deref() {
            None => self.pack_now = 0,
            Some("push") => {
                self.pack.push((None, self.pack_now));
                for p in &parts[1..] {
                    if let Some(n) = num(self, p) {
                        if valid(n) {
                            self.pack_now = n;
                        }
                    } else if let Some(id) = ident(p) {
                        self.pack.last_mut().expect("pushed").0 = Some(id);
                    }
                }
            }
            Some("pop") => {
                let label = parts[1..].iter().find_map(|p| ident(p));
                match label {
                    Some(id) => {
                        if let Some(i) = self.pack.iter().rposition(|e| e.0 == Some(id)) {
                            self.pack_now = self.pack[i].1;
                            self.pack.truncate(i);
                        }
                    }
                    None => {
                        if let Some((_, v)) = self.pack.pop() {
                            self.pack_now = v;
                        }
                    }
                }
                if let Some(n) = parts[1..].iter().find_map(|p| num(self, p)) {
                    if valid(n) {
                        self.pack_now = n;
                    }
                }
            }
            Some("show") => {}
            Some(_) => {
                if let Some(n) = num(self, &parts[0]) {
                    if valid(n) {
                        self.pack_now = n;
                    }
                }
            }
        }
        Ok(tok(Kind::Pragma, self.pack_now as Sym, loc))
    }
}
