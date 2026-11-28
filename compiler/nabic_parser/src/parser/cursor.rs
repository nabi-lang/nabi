//! Token cursor: peek, bump, expect, error reporting.

#[allow(clippy::wildcard_imports)]
use crate::parser::*;

impl Parser<'_> {
    /// Consume a closing `>`, splitting `>>`, `>=`, `>>=` and leaving the
    /// rest pending; false (with an error) when there is none.
    pub(crate) fn expect_closing_gt(&mut self) -> bool {
        let rest = match self.peek() {
            TokenKind::Gt => None,
            TokenKind::Shr => Some(TokenKind::Gt),
            TokenKind::GtEq => Some(TokenKind::Eq),
            TokenKind::ShrEq => Some(TokenKind::GtEq),
            other => {
                let msg = format!(
                    "expected `>` to close generic arguments, found {}",
                    other.describe()
                );
                let span = self.peek_span();
                // `Buf<2 * 8>`: arithmetic in an argument needs braces.
                let after_value = matches!(
                    self.prev_kind(),
                    Some(TokenKind::IntLit(_) | TokenKind::Ident(_) | TokenKind::RParen)
                );
                if after_value
                    && matches!(
                        other,
                        TokenKind::Star
                            | TokenKind::Plus
                            | TokenKind::Minus
                            | TokenKind::Slash
                            | TokenKind::Percent
                    )
                {
                    self.error_help(
                        Code::SYNTAX,
                        span,
                        msg,
                        "a const argument that is an expression goes in braces, as in \
                         `<{ 2 * 8 }>`",
                    );
                } else {
                    self.error(Code::SYNTAX, span, msg);
                }
                return false;
            }
        };
        self.bump();
        self.pending = rest;
        true
    }

    /// Open a generic list or `<T as Tr>`: consume a `<`, splitting `<<`
    /// (`Tag<<A as P>::X>`) and leaving the second pending.
    pub(crate) fn eat_opening_lt(&mut self) -> bool {
        match self.peek() {
            TokenKind::Lt => {
                self.bump();
            }
            TokenKind::Shl => {
                self.bump();
                self.pending = Some(TokenKind::Lt);
            }
            _ => return false,
        }
        true
    }

    pub(crate) fn peek(&self) -> &TokenKind {
        if let Some(k) = &self.pending {
            return k;
        }
        self.tokens
            .get(self.pos)
            .map_or(&TokenKind::Eof, |t| &t.kind)
    }

    pub(crate) fn peek_span(&self) -> Span {
        self.tokens.get(self.pos).map_or(Span::DUMMY, |t| t.span)
    }

    pub(crate) fn bump(&mut self) -> Token {
        if let Some(kind) = self.pending.take() {
            // Remainder of a split token; span of the token it came from.
            let span = self
                .tokens
                .get(self.pos.saturating_sub(1))
                .map_or(Span::DUMMY, |t| t.span);
            return Token::new(kind, span);
        }
        // Recovery can bump past the last token: hand back a synthetic Eof.
        let Some(t) = self.tokens.get(self.pos).cloned() else {
            return Token::new(TokenKind::Eof, Span::DUMMY);
        };
        if let TokenKind::Ident(n) = &t.kind {
            let name_pos = std::mem::take(&mut self.name_pos);
            // `@macro { .. }` is meta syntax, not a name.
            if !t.raw
                && !name_pos
                && UNUSED_KEYWORDS.contains(&n.as_str())
                && self.prev_kind() != Some(&TokenKind::At)
            {
                self.unused_keyword(n, t.span);
            }
        }
        self.pos += 1;
        t
    }

    /// `bump` a name where any keyword is one (after `.`, a field).
    pub(crate) fn bump_name(&mut self) -> Token {
        self.name_pos = true;
        let t = self.bump();
        self.name_pos = false;
        t
    }

    /// `name` is a reserved keyword with no syntax yet: not a name.
    /// Backtracking drops what a failed parse reported, and the token is
    /// bumped again: report once per kept parse.
    pub(crate) fn unused_keyword(&mut self, name: &str, span: Span) {
        let msg = format!("`{name}` is a reserved keyword, not a name");
        if span.is_dummy()
            || self
                .diagnostics
                .iter()
                .any(|d| d.message == msg && d.primary.as_ref().is_some_and(|l| l.span == span))
        {
            return;
        }
        self.diagnostics.push(
            NabiDiagnostic::error(Code::RESERVED_NAME, span, msg)
                .with_file(self.file.clone())
                .with_help(format!(
                    "write `r#{name}` to keep this name, or rename it{}",
                    reserved_for(name)
                ))
                .with_fix(span, format!("r#{name}"), format!("write `r#{name}`")),
        );
    }

    pub(crate) fn eat(&mut self, kind: &TokenKind) -> bool {
        if std::mem::discriminant(self.peek()) == std::mem::discriminant(kind) {
            self.bump();
            true
        } else {
            false
        }
    }

    pub(crate) fn expect(&mut self, kind: &TokenKind, what: &str) -> Option<Token> {
        if std::mem::discriminant(self.peek()) == std::mem::discriminant(kind) {
            Some(self.bump())
        } else {
            let d = self.expected(kind, what);
            self.diagnostics.push(d);
            None
        }
    }

    /// "expected X, found Y", with hints for a missing `;` (at the line's
    /// end), an unclosed delimiter (its opener) and `->` / `=>` mixups.
    pub(crate) fn expected(&self, kind: &TokenKind, what: &str) -> NabiDiagnostic {
        let found = self.peek();
        let here = self.peek_span();
        let d = NabiDiagnostic::error(
            Code::SYNTAX,
            Span::DUMMY,
            format!("expected {what}, found {}", found.describe()),
        )
        .with_file(self.file.clone());
        let prev = self.prev_span();
        if let Some(help) = self.bang_help() {
            return d
                .with_span(here, "")
                .with_help(help)
                .with_fix(here, "", "delete `!`");
        }
        if let Some(py) = self.python_expected(kind) {
            let d = d.with_span(py.span, "").with_help(py.help);
            return match py.fix {
                Some((at, text, title)) => d.with_fix(at, text, title),
                None => d,
            };
        }
        // `"a" "b"` (C, Python): literals are not joined by being adjacent.
        let string = |t: &TokenKind| matches!(t, TokenKind::StrLit(_) | TokenKind::RawStrLit(_));
        if (string(found) || matches!(found, TokenKind::FStrStart(_)))
            && self
                .prev_kind()
                .is_some_and(|t| string(t) || matches!(t, TokenKind::FStrEnd(_)))
        {
            let at = Span {
                end: here.start,
                ..here
            };
            return d
                .with_span(here, "")
                .with_help(
                    "adjacent string literals are not joined: write one literal (`\"ab\"`), or \
                     join them with `+` (`\"a\" + \"b\"`)",
                )
                .with_fix(at, "+ ", "join them with `+`");
        }
        // `->` where no return type can start: between two values it is
        // an operator only where a trait binds it, so say whether one does.
        if matches!(found, TokenKind::Arrow) && !matches!(kind, TokenKind::FatArrow) {
            let d = d.with_span(here, "`->` is not an operator here");
            return if self.ext.level("->", Fixity::Infix).is_some() {
                d.with_help(
                    "`->` starts a return type (`fn f() -> T`), or calls the bound method \
                     between two values (`a -> b`)",
                )
            } else {
                d.with_help(
                    "`->` starts a return type (`fn f() -> T`); between two values it needs a \
                     trait in scope with `#[infix(\"->\", like = \"|\")]`",
                )
            };
        }
        match kind {
            TokenKind::Semi if prev.is_some_and(|p| p.line < here.line || here.is_dummy()) => {
                let at = self.after_prev();
                return d
                    .with_span(at, "expected `;`")
                    .with_help("add `;` at the end of the statement");
            }
            TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                if let Some(open) = self.unclosed_opener(kind) {
                    return d
                        .with_span(here, format!("expected {what}"))
                        .with_secondary(open, "unclosed delimiter");
                }
            }
            // `if x = 3 {` (C): a comparison is `==`.
            TokenKind::LBrace if matches!(found, TokenKind::Eq) && self.in_condition() => {
                return d
                    .with_span(here, "")
                    .with_help("did you mean `==`? `=` assigns; `==` compares")
                    .with_fix(here, "==", "use `==`");
            }
            TokenKind::FatArrow if matches!(found, TokenKind::Arrow) => {
                return d
                    .with_span(here, "")
                    .with_help("match arms are written `pattern => value`");
            }
            _ if matches!(found, TokenKind::FatArrow)
                && self.prev_kind() == Some(&TokenKind::RParen) =>
            {
                return d
                    .with_span(here, "")
                    .with_help("a return type is written `-> Type`");
            }
            _ => {}
        }
        // `fucn main`, `retrun x`: an identifier that reads as a
        // misspelled keyword, followed by another word.
        if let Some(TokenKind::Ident(word)) = self.prev_kind() {
            if matches!(found, TokenKind::Ident(_)) || keyword_alias(word).is_some() {
                let import = match found {
                    TokenKind::Ident(name)
                        if matches!(word.as_str(), "import" | "include" | "require") =>
                    {
                        import_help(&self.file, self.tokens, name)
                    }
                    _ => None,
                };
                if let Some(k) =
                    import.or_else(|| keyword_hint(word).map(|k| format!("did you mean `{k}`?")))
                {
                    let at = self.prev_span().unwrap_or(here);
                    return d
                        .with_span(here, "")
                        .with_secondary(at, "not a keyword")
                        .with_help(k);
                }
            }
        }
        d.with_span(here, "")
    }

    /// The cursor is in an `if` or `while` condition.
    pub(crate) fn in_condition(&self) -> bool {
        self.tokens[..self.pos.min(self.tokens.len())]
            .iter()
            .rev()
            .map(|t| &t.kind)
            .take_while(|k| !matches!(k, TokenKind::Semi | TokenKind::LBrace | TokenKind::RBrace))
            .any(|k| matches!(k, TokenKind::If | TokenKind::While))
    }

    /// `v!` (Swift, TypeScript): a postfix `!` to unwrap, which Nabi does
    /// not have.
    pub(crate) fn bang_help(&self) -> Option<String> {
        if !matches!(self.peek(), TokenKind::Bang)
            || matches!(
                self.tokens.get(self.pos + 1).map(|t| &t.kind),
                Some(TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace)
            )
        {
            return None;
        }
        let v = match self.prev_kind()? {
            TokenKind::Ident(v) => v.as_str(),
            TokenKind::RParen | TokenKind::RBracket => "v",
            _ => return None,
        };
        Some(format!(
            "Nabi has no `!` to unwrap; for a value that may be none, write `{v} ?? default`, \
             `if let x = {v} {{ .. }}`, or `match {v} {{ x: T => .., none => .. }}`"
        ))
    }

    pub(crate) fn prev_span(&self) -> Option<Span> {
        self.tokens.get(self.pos.checked_sub(1)?).map(|t| t.span)
    }

    /// `span` widened to the end of the previous token: a compound
    /// expression covers all its tokens.
    pub(crate) fn to_prev(&self, mut span: Span) -> Span {
        if let Some(p) = self.prev_span() {
            if p.file == span.file && p.end > span.end {
                span.end = p.end;
            }
        }
        span
    }

    pub(crate) fn prev_kind(&self) -> Option<&TokenKind> {
        self.tokens.get(self.pos.checked_sub(1)?).map(|t| &t.kind)
    }

    /// Zero-width span just after the previous token.
    pub(crate) fn after_prev(&self) -> Span {
        let Some(t) = self.pos.checked_sub(1).and_then(|i| self.tokens.get(i)) else {
            return self.peek_span();
        };
        let len = u32::try_from(t.kind.text().chars().count()).unwrap_or(1);
        Span {
            start: t.span.end,
            end: t.span.end,
            line: t.span.line,
            col: t.span.col + len.max(1),
            file: t.span.file,
        }
    }

    /// The innermost delimiter still open before the current token
    /// that `close` would close.
    pub(crate) fn unclosed_opener(&self, close: &TokenKind) -> Option<Span> {
        let mut stack: Vec<(&TokenKind, Span)> = Vec::new();
        for t in self.tokens.get(..self.pos)? {
            match &t.kind {
                TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => {
                    stack.push((&t.kind, t.span));
                }
                TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                    stack.pop();
                }
                _ => {}
            }
        }
        let open = match close {
            TokenKind::RParen => TokenKind::LParen,
            TokenKind::RBracket => TokenKind::LBracket,
            _ => TokenKind::LBrace,
        };
        stack
            .iter()
            .rev()
            .find(|(k, _)| **k == open)
            .map(|(_, s)| *s)
    }

    /// "expected {what}" at a name position; a keyword there gets the
    /// `r#` help (spec 1.5.6) instead of a bare syntax error.
    pub(crate) fn expected_name(&mut self, span: Span, what: &str) {
        let found = self.peek();
        match keyword_str(found) {
            Some(kw) if !matches!(kw, "self" | "Self" | "_" | "true" | "false" | "none") => {
                let msg = format!("`{kw}` is a keyword, not a name");
                self.diagnostics.push(
                    NabiDiagnostic::error(Code::RESERVED_NAME, span, msg)
                        .with_file(self.file.clone())
                        .with_help(format!("write `r#{kw}` to keep this name, or rename it"))
                        .with_fix(span, format!("r#{kw}"), format!("write `r#{kw}`")),
                );
            }
            _ => {
                let msg = format!("expected {what}, found {}", found.describe());
                self.error(Code::SYNTAX, span, msg);
            }
        }
    }

    pub(crate) fn error(&mut self, code: Code, span: Span, msg: impl Into<String>) {
        self.diagnostics
            .push(NabiDiagnostic::error(code, span, msg).with_file(self.file.clone()));
    }
}

impl Parser<'_> {
    /// The current token is the contextual keyword `w` (an identifier
    /// token).
    pub(crate) fn at_kw(&self, w: &str) -> bool {
        matches!(self.peek(), TokenKind::Ident(n) if n == w) && !self.peek_raw()
    }

    /// The current token is an identifier written `r#name`.
    pub(crate) fn peek_raw(&self) -> bool {
        self.pending.is_none() && self.tokens.get(self.pos).is_some_and(|t| t.raw)
    }

    pub(crate) fn eat_kw(&mut self, w: &str) -> bool {
        let hit = self.at_kw(w);
        if hit {
            self.bump();
        }
        hit
    }

    pub(crate) fn expect_kw(&mut self, w: &str) -> Option<()> {
        if self.eat_kw(w) {
            return Some(());
        }
        let s = self.peek_span();
        self.error(
            Code::SYNTAX,
            s,
            format!("expected `{w}`, found {}", self.peek().describe()),
        );
        None
    }

    /// A member / field / argument-label name: an identifier or any
    /// reserved keyword but `self` (`p.type`, `f(match: 1)`).
    pub(crate) fn peek_name(&self) -> Option<String> {
        match self.peek() {
            TokenKind::Ident(n) => Some(n.clone()),
            k => keyword_str(k).filter(|w| *w != "self").map(str::to_owned),
        }
    }

    pub(crate) fn peek_next(&self) -> Option<TokenKind> {
        self.tokens.get(self.pos + 1).map(|t| t.kind.clone())
    }
}

/// What Nabi writes today for the form a reserved keyword would start.
fn reserved_for(name: &str) -> &'static str {
    match name {
        "macro" => "; a macro is a meta block's `@macro`",
        "defer" => "; code run when a scope ends is `using` or `finally`",
        "gen" => "; a generator is `yield fn`, `yield { .. }` or `yield || ..`",
        "do" => "; a loop that tests after the body is `loop { ..; if !cond { break } }`",
        "dyn" => "; a trait object is `any Trait`",
        _ => "",
    }
}
