//! Macro expansion: arguments, substitution, `#` and `##`, builtins.

#[allow(clippy::wildcard_imports)]
use crate::*;

impl Pp<'_> {
    /// Expand `toks` alone (an argument, a directive line).
    pub(crate) fn expand_isolated(&mut self, mut toks: Vec<Tok>) -> Result<Vec<Tok>, Error> {
        let mut end = tok(Kind::Eof, 0, Loc::default());
        end.flags = END;
        toks.push(end);
        let depth = self.stack.len();
        let back = std::mem::take(&mut self.back);
        self.stack.push(Source::Tokens {
            toks,
            pos: 0,
            enable: None,
        });
        let mut out = Vec::new();
        loop {
            let t = self.next()?;
            if t.has(END) {
                break;
            }
            out.push(t);
        }
        // Expansions above the argument have all ended at its sentinel.
        self.stack.truncate(depth);
        self.back = back;
        Ok(out)
    }

    /// Expand the macro `s` as bindgen evaluates it, after the unit.
    pub fn expand_name(&mut self, s: Sym) -> Result<Vec<Tok>, Error> {
        self.expand_isolated(vec![tok(Kind::Ident, s, Loc::default())])
    }

    pub(crate) fn expand(&mut self, t: Tok, m: &Rc<Macro>) -> Result<bool, Error> {
        if let Some(b) = m.builtin {
            let r = self.builtin(t, b)?;
            self.stack.push(Source::Tokens {
                toks: vec![r],
                pos: 0,
                enable: None,
            });
            return Ok(true);
        }
        let mut body = if let Some(params) = &m.params {
            let n = self.raw()?;
            if !self.is(n, "(") {
                self.back.push(n);
                return Ok(false);
            }
            let args = self.args(t, m, params.len())?;
            self.substitute(m, Some(&args))?
        } else {
            self.substitute(m, None)?
        };
        body.retain(|x| !x.has(PLACEMARKER));
        for x in &mut body {
            x.loc = t.loc;
            x.flags &= !BOL;
        }
        if let Some(f) = body.first_mut() {
            f.flags = (f.flags & !SPACE) | (t.flags & SPACE);
        }
        self.disabled.insert(t.sym);
        self.stack.push(Source::Tokens {
            toks: body,
            pos: 0,
            enable: Some(t.sym),
        });
        Ok(true)
    }

    /// The arguments of an invocation of `m`, after its `(`.
    pub(crate) fn args(&mut self, name: Tok, m: &Macro, n: usize) -> Result<Vec<Vec<Tok>>, Error> {
        let mut args: Vec<Vec<Tok>> = vec![Vec::new()];
        let mut depth = 0u32;
        loop {
            let mut t = self.raw()?;
            if t.kind == Kind::Eof {
                return Err(self.err(name.loc, "unterminated function-like macro invocation"));
            }
            if t.has(BOL) {
                t.flags = (t.flags & !BOL) | SPACE;
            }
            if t.kind == Kind::Ident && self.disabled.contains(&t.sym) {
                t.flags |= NOEXP;
            }
            if t.kind == Kind::Punct {
                match self.spell(t) {
                    "(" => depth += 1,
                    ")" if depth == 0 => break,
                    ")" => depth -= 1,
                    "," if depth == 0 && !(m.variadic && args.len() == n) => {
                        args.push(Vec::new());
                        continue;
                    }
                    _ => {}
                }
            }
            args.last_mut().expect("an argument").push(t);
        }
        if n == 0 && args.len() == 1 && args[0].is_empty() {
            return Ok(Vec::new());
        }
        if args.len() + 1 == n && m.variadic {
            args.push(Vec::new());
        }
        if args.len() != n {
            let few = args.len() < n;
            return Err(self.err(
                name.loc,
                &format!(
                    "too {} arguments provided to function-like macro invocation of `{}` \
                     (it takes {n})",
                    if few { "few" } else { "many" },
                    self.spell(name)
                ),
            ));
        }
        Ok(args)
    }

    pub(crate) fn param(m: &Macro, t: Tok) -> Option<usize> {
        if t.kind != Kind::Ident {
            return None;
        }
        m.params.as_ref()?.iter().position(|&p| p == t.sym)
    }

    /// `m`'s body with `args` in place, `#` and `##` applied.
    pub(crate) fn substitute(
        &mut self,
        m: &Macro,
        args: Option<&[Vec<Tok>]>,
    ) -> Result<Vec<Tok>, Error> {
        let mut expanded: Vec<Option<Vec<Tok>>> = vec![None; args.map_or(0, <[_]>::len)];
        self.subst_range(m, &m.body, args, &mut expanded)
    }

    pub(crate) fn subst_range(
        &mut self,
        m: &Macro,
        body: &[Tok],
        args: Option<&[Vec<Tok>]>,
        expanded: &mut Vec<Option<Vec<Tok>>>,
    ) -> Result<Vec<Tok>, Error> {
        let mut out: Vec<Tok> = Vec::new();
        let mut paste = false;
        let mut i = 0;
        while i < body.len() {
            let b = body[i];
            let before_paste = body.get(i + 1).is_some_and(|n| self.is(*n, "##"));
            // `#param`.
            if args.is_some() && self.is(b, "#") {
                if let Some(p) = body.get(i + 1).and_then(|n| Self::param(m, *n)) {
                    let s = self.stringize(&args.expect("arguments")[p]);
                    let mut st = tok(Kind::Str, self.names.intern(&s), b.loc);
                    st.flags = b.flags & SPACE;
                    self.push(&mut out, vec![st], &mut paste, b)?;
                    i += 2;
                    continue;
                }
            }
            if self.is(b, "##") && i > 0 && i + 1 < body.len() {
                paste = true;
                i += 1;
                continue;
            }
            if b.sym == kw::VA_OPT && m.variadic && args.is_some() {
                // `__VA_OPT__(content)`.
                let mut j = i + 1;
                let mut depth = 0;
                let start = j + 1;
                while j < body.len() {
                    if self.is(body[j], "(") {
                        depth += 1;
                    } else if self.is(body[j], ")") {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    j += 1;
                }
                let va = args.expect("arguments").len() - 1;
                let present = !self.expanded(va, args, expanded)?.is_empty();
                let content = if present && start <= j {
                    self.subst_range(m, &body[start..j], args, expanded)?
                } else {
                    Vec::new()
                };
                self.push(&mut out, content, &mut paste, b)?;
                i = j + 1;
                continue;
            }
            if let (Some(p), Some(a)) = (Self::param(m, b), args) {
                let is_va = m.variadic && p + 1 == a.len();
                // GNU `, ## __VA_ARGS__`: an empty argument takes the comma.
                if paste && is_va && out.last().is_some_and(|l| self.is(*l, ",")) {
                    paste = false;
                    if a[p].is_empty() {
                        out.pop();
                    } else {
                        let mut v = a[p].clone();
                        v[0].flags &= !SPACE;
                        out.extend(v);
                    }
                    i += 1;
                    continue;
                }
                let mut v = if paste || before_paste {
                    a[p].clone()
                } else {
                    self.expanded(p, args, expanded)?
                };
                if let Some(f) = v.first_mut() {
                    f.flags = (f.flags & !SPACE) | (b.flags & SPACE);
                }
                self.push(&mut out, v, &mut paste, b)?;
                i += 1;
                continue;
            }
            self.push(&mut out, vec![b], &mut paste, b)?;
            i += 1;
        }
        Ok(out)
    }

    pub(crate) fn expanded(
        &mut self,
        p: usize,
        args: Option<&[Vec<Tok>]>,
        cache: &mut [Option<Vec<Tok>>],
    ) -> Result<Vec<Tok>, Error> {
        if let Some(v) = &cache[p] {
            return Ok(v.clone());
        }
        let v = self.expand_isolated(args.expect("arguments")[p].clone())?;
        cache[p] = Some(v.clone());
        Ok(v)
    }

    /// Append `v` to `out`, pasting its first token onto `out`'s last if
    /// a `##` came between them.
    pub(crate) fn push(
        &mut self,
        out: &mut Vec<Tok>,
        mut v: Vec<Tok>,
        paste: &mut bool,
        at: Tok,
    ) -> Result<(), Error> {
        if v.is_empty() {
            let mut p = tok(Kind::Punct, 0, at.loc);
            p.flags = PLACEMARKER;
            v.push(p);
        }
        if std::mem::take(paste) {
            let l = out.pop().expect("a token before ##");
            let r = v.remove(0);
            if l.has(PLACEMARKER) {
                out.push(r);
            } else if r.has(PLACEMARKER) {
                out.push(l);
            } else {
                let text = format!("{}{}", self.spell(l), self.spell(r));
                let toks = relex(&text, &mut self.names, l.loc);
                if toks.len() != 1 {
                    return Err(self.err(
                        at.loc,
                        &format!("pasting formed '{text}', an invalid preprocessing token"),
                    ));
                }
                let mut t = toks[0];
                t.flags = l.flags & SPACE;
                out.push(t);
            }
        }
        out.extend(v);
        Ok(())
    }

    pub(crate) fn stringize(&self, toks: &[Tok]) -> String {
        let mut s = String::from("\"");
        for (i, t) in toks.iter().enumerate() {
            if i > 0 && t.has(SPACE) {
                s.push(' ');
            }
            let sp = self.spell(*t);
            if matches!(t.kind, Kind::Str | Kind::Char) {
                for c in sp.chars() {
                    if c == '"' || c == '\\' {
                        s.push('\\');
                    }
                    s.push(c);
                }
            } else {
                s.push_str(sp);
            }
        }
        s.push('"');
        s
    }

    pub(crate) fn builtin(&mut self, t: Tok, b: Builtin) -> Result<Tok, Error> {
        let file = &self.files[t.loc.file as usize];
        let path = file.path.to_string_lossy().into_owned();
        let quote = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
        let (kind, text) = match b {
            // Where the use site is unknown (bindgen's macro values),
            // these have no value.
            Builtin::Line | Builtin::File | Builtin::FileName | Builtin::Counter
                if t.loc.file == 0 =>
            {
                return Err(self.err(t.loc, "no use site for a location macro"));
            }
            Builtin::Line => (Kind::Num, t.loc.line.to_string()),
            Builtin::File => (Kind::Str, quote(&path)),
            Builtin::FileName => (
                Kind::Str,
                quote(
                    Path::new(&path)
                        .file_name()
                        .map(|f| f.to_string_lossy().into_owned())
                        .as_deref()
                        .unwrap_or(""),
                ),
            ),
            Builtin::BaseFile => (Kind::Str, quote(&self.base_file.clone())),
            Builtin::Counter => {
                self.counter += 1;
                (Kind::Num, (self.counter - 1).to_string())
            }
            Builtin::IncludeLevel => {
                let n = self
                    .stack
                    .iter()
                    .filter(|s| matches!(s, Source::File { .. }))
                    .count();
                (Kind::Num, n.saturating_sub(1).to_string())
            }
            Builtin::Date => (Kind::Str, "\"Jan  1 1970\"".to_owned()),
            Builtin::Time => (Kind::Str, "\"00:00:00\"".to_owned()),
            Builtin::Timestamp => (Kind::Str, "\"Thu Jan  1 00:00:00 1970\"".to_owned()),
            Builtin::Query => {
                let v = self.query(t)?;
                (Kind::Num, v.to_string())
            }
        };
        let mut r = tok(kind, self.names.intern(&text), t.loc);
        r.flags = t.flags & SPACE;
        Ok(r)
    }

    /// The raw tokens of `( .. )` after a function-like builtin.
    pub(crate) fn paren_args(&mut self, name: Tok) -> Result<Vec<Tok>, Error> {
        let open = self.raw()?;
        if !self.is(open, "(") {
            return Err(self.err(
                name.loc,
                &format!("missing '(' after '{}'", self.spell(name)),
            ));
        }
        let mut out = Vec::new();
        let mut depth = 0;
        loop {
            let t = self.raw()?;
            if t.kind == Kind::Eof {
                return Err(self.err(
                    name.loc,
                    &format!("missing ')' after '{}'", self.spell(name)),
                ));
            }
            if self.is(t, "(") {
                depth += 1;
            } else if self.is(t, ")") {
                if depth == 0 {
                    return Ok(out);
                }
                depth -= 1;
            }
            out.push(t);
        }
    }
}
