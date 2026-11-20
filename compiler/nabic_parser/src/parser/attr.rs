//! Attributes, `#[cfg]` and doc comments.

#[allow(clippy::wildcard_imports)]
use crate::parser::*;

impl Parser<'_> {
    pub(crate) fn parse_inner_attrs(&mut self) -> Vec<Attribute> {
        let mut attrs = Vec::new();
        loop {
            self.doc_attrs(true, &mut attrs);
            if !matches!(self.peek(), TokenKind::HashBangLBracket) {
                return attrs;
            }
            if let Some(attr) = self.parse_attr(true) {
                match &self.cfg {
                    Some(env) => crate::cfg::expand_cfg_attr(env, attr, &mut attrs),
                    None => attrs.push(attr),
                }
            }
        }
    }

    /// Whether the build's cfg rules out what `attrs` are on.
    pub(crate) fn cfg_off(&self, attrs: &[Attribute]) -> bool {
        self.cfg.as_ref().is_some_and(|env| !env.attrs_pass(attrs))
    }

    /// Does `attrs`' cfg keep this member or statement? A
    /// dropped one's bad predicate is reported here, a kept one's with it.
    pub(crate) fn cfg_keeps(&mut self, attrs: &[Attribute]) -> bool {
        if !self.cfg_off(attrs) {
            return true;
        }
        for a in attrs.iter().filter(|a| a.path == ["cfg"]) {
            if let Some(m) = crate::cfg::attr_problem(a) {
                let d = NabiDiagnostic::error(Code::INVALID_CFG, a.span, m);
                self.diagnostics.push(d.with_file(self.file.clone()));
            }
        }
        false
    }

    /// Whether outer attributes start here and a statement, not an
    /// item, follows them.
    pub(crate) fn at_attributed_stmt(&mut self) -> bool {
        let save = self.pos;
        self.pos = self.past_attrs(save);
        let stmt = self.pos != save && !self.at_item_start();
        self.pos = save;
        stmt
    }

    /// The index after the `#[..]` attributes starting at token `i`.
    pub(crate) fn past_attrs(&self, mut i: usize) -> usize {
        while matches!(
            self.tokens.get(i).map(|t| &t.kind),
            Some(TokenKind::HashLBracket)
        ) {
            let mut depth = 0usize;
            while let Some(t) = self.tokens.get(i) {
                i += 1;
                match t.kind {
                    TokenKind::HashLBracket | TokenKind::LBracket => depth += 1,
                    TokenKind::RBracket => depth -= 1,
                    _ => {}
                }
                if depth == 0 {
                    break;
                }
            }
        }
        i
    }

    /// Outer attributes where only `#[cfg]` may be: a
    /// statement, arm, literal field, parameter or tuple field (`what`).
    pub(crate) fn cfg_only_attrs(&mut self, what: &'static str) -> Vec<Attribute> {
        let attrs = self.parse_outer_attrs();
        // A nested `fn` is a statement here; like a local item, it may
        // have a doc comment (not part of the package's API).
        let item = what == "a statement" && self.at_item_start();
        for a in attrs.iter().filter(|a| a.name() == "doc" && !item) {
            // As in Rust, a parameter's is an error.
            self.docs_dropped
                .insert(a.span.start, (what, what == "a parameter"));
        }
        for a in &attrs {
            let why = match a.name() {
                "doc" => None,
                "cfg" => crate::cfg::attr_problem(a).map(|m| (Code::INVALID_CFG, m)),
                _ => Some((
                    Code::MISPLACED_ATTRIBUTE,
                    format!(
                        "`#[{}]` cannot be attached to {what}; only `#[cfg]` can",
                        a.path.join("::")
                    ),
                )),
            };
            if let Some((code, m)) = why {
                let d = NabiDiagnostic::error(code, a.span, m);
                self.diagnostics.push(d.with_file(self.file.clone()));
            }
        }
        attrs
    }

    /// Do `attrs` drop what was just parsed? Statements and
    /// nested fns queued since `marks` go with it.
    pub(crate) fn cfg_drops(
        &mut self,
        attrs: &[Attribute],
        (pending, lifted): (usize, usize),
    ) -> bool {
        if !self.cfg_off(attrs) {
            return false;
        }
        self.pending_stmts.truncate(pending);
        for it in self.lifted.drain(lifted..) {
            if let Some(n) = it.name() {
                self.lifted_names.remove(n);
            }
        }
        true
    }

    pub(crate) fn parse_outer_attrs(&mut self) -> Vec<Attribute> {
        let mut attrs = Vec::new();
        loop {
            self.doc_attrs(false, &mut attrs);
            if !matches!(self.peek(), TokenKind::HashLBracket) {
                return attrs;
            }
            if let Some(attr) = self.parse_attr(false) {
                match &self.cfg {
                    Some(env) => crate::cfg::expand_cfg_attr(env, attr, &mut attrs),
                    None => attrs.push(attr),
                }
            }
        }
    }

    /// Report doc comments that document nothing, as Rust does: errors for
    /// a misplaced inner one, an outer one before `}` / EOF or on a
    /// parameter; the `unused_doc_comments` warning for one on a non-item.
    /// Skipped after a syntax error.
    pub(crate) fn misplaced_docs(&mut self) {
        if self.diagnostics.iter().any(NabiDiagnostic::is_error) {
            return;
        }
        let plain = "use `//` for a plain comment";
        let mut out = Vec::new();
        for (i, t) in self.tokens.iter().enumerate() {
            if t.docs.is_empty() || self.unparsed.iter().any(|r| r.contains(&i)) {
                continue;
            }
            let closing = matches!(
                t.kind,
                TokenKind::RBrace | TokenKind::RParen | TokenKind::RBracket | TokenKind::Eof
            );
            for d in &t.docs {
                let used = self.docs_used.contains(&(i, d.inner));
                let dropped = self.docs_dropped.get(&d.span.start);
                let (msg, error) = match (d.inner, used, dropped) {
                    (true, true, _) | (false, true, None) => continue,
                    (true, false, _) => (
                        "expected outer doc comment: an inner doc comment (`//!`, `/*!`) \
                         documents the file, module or function body it starts; use `///` for \
                         the next item, or `//` for a plain comment"
                            .to_owned(),
                        true,
                    ),
                    (false, _, Some(&(what, true))) => (
                        format!("a doc comment cannot document {what}; {plain}"),
                        true,
                    ),
                    (false, _, Some(&(what, false))) => (
                        format!("unused doc comment: {what} is not documented; {plain}"),
                        false,
                    ),
                    (false, false, None) if closing => (
                        format!(
                            "doc comment documents nothing: it must come before an item; {plain}"
                        ),
                        true,
                    ),
                    (false, false, None) => (
                        format!("unused doc comment: only items are documented; {plain}"),
                        false,
                    ),
                };
                let span = Span {
                    file: t.span.file,
                    ..d.span
                };
                out.push(if error {
                    NabiDiagnostic::error(Code::MISPLACED_DOC_COMMENT, span, msg)
                } else {
                    NabiDiagnostic::warning(Code::MISPLACED_DOC_COMMENT, span, msg)
                        .with_lint("unused_doc_comments")
                });
            }
        }
        let file = self.file.clone();
        self.diagnostics
            .extend(out.into_iter().map(|d| d.with_file(file.clone())));
    }

    /// The next token's doc comments as `#[doc = ".."]` attributes.
    pub(crate) fn doc_attrs(&mut self, inner: bool, out: &mut Vec<Attribute>) {
        self.doc_attrs_at(self.pos, inner, out);
    }

    pub(crate) fn doc_attrs_at(&mut self, at: usize, inner: bool, out: &mut Vec<Attribute>) {
        if self.pending.is_some() {
            return;
        }
        let Some(t) = self.tokens.get(at) else {
            return;
        };
        if t.docs.iter().any(|d| d.inner == inner) {
            self.docs_used.insert((at, inner));
        }
        out.extend(t.docs.iter().filter(|d| d.inner == inner).map(|d| {
            Attribute {
                path: vec!["doc".to_owned()],
                body: AttrBody::Value(Literal::Str(
                    d.text
                        .replace('\\', "\\\\")
                        .replace('"', "\\\"")
                        .replace('\n', "\\n"),
                )),
                inner,
                span: Span {
                    file: t.span.file,
                    ..d.span
                },
            }
        }));
    }

    pub(crate) fn parse_attr(&mut self, inner: bool) -> Option<Attribute> {
        let start = self.peek_span();
        self.bump(); // `#[` or `#![`
        let path = self.parse_attr_path()?;
        let errors = self.diagnostics.len();
        let body = if path == ["literal"] && matches!(self.peek(), TokenKind::LParen) {
            self.parse_literal_attr_body()
        } else {
            self.parse_attr_body()
        };
        if self.diagnostics.len() == errors {
            self.expect(&TokenKind::RBracket, "`]`");
        } else {
            // A malformed body: on past its `]`, reporting nothing more.
            let mut depth = 0usize;
            loop {
                match self.peek() {
                    TokenKind::Eof => break,
                    TokenKind::RBracket if depth == 0 => {
                        self.bump();
                        break;
                    }
                    TokenKind::LBracket
                    | TokenKind::HashLBracket
                    | TokenKind::LParen
                    | TokenKind::LBrace => depth += 1,
                    TokenKind::RBracket | TokenKind::RParen | TokenKind::RBrace => {
                        depth = depth.saturating_sub(1);
                    }
                    _ => {}
                }
                self.bump();
            }
        }
        Some(Attribute {
            path,
            body,
            inner,
            span: start,
        })
    }

    /// Path inside an attribute body: `name` or `a::b::c`.
    pub(crate) fn parse_attr_path(&mut self) -> Option<Vec<String>> {
        let mut segments = Vec::new();
        loop {
            let name = match self.peek().clone() {
                TokenKind::Ident(n) => n,
                other => {
                    let s = self.peek_span();
                    self.error(
                        Code::INVALID_ATTRIBUTE,
                        s,
                        format!("expected attribute name, found {}", other.describe()),
                    );
                    return None;
                }
            };
            self.bump();
            segments.push(name);
            if !self.eat(&TokenKind::ColonColon) {
                break;
            }
        }
        Some(segments)
    }

    pub(crate) fn parse_attr_body(&mut self) -> AttrBody {
        match self.peek() {
            TokenKind::LParen => {
                self.bump();
                let mut args = Vec::new();
                while !matches!(self.peek(), TokenKind::RParen | TokenKind::Eof) {
                    if let Some(arg) = self.parse_attr_arg() {
                        args.push(arg);
                    } else {
                        // recover until `,` or `)`
                        while !matches!(
                            self.peek(),
                            TokenKind::Comma | TokenKind::RParen | TokenKind::Eof
                        ) {
                            self.bump();
                        }
                    }
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                }
                self.expect(&TokenKind::RParen, "`)`");
                AttrBody::Args(args)
            }
            TokenKind::Eq => {
                self.bump();
                self.parse_attr_literal()
                    .map_or(AttrBody::Empty, AttrBody::Value)
            }
            _ => AttrBody::Empty,
        }
    }

    /// One argument, or a range of two (`a..=b`, `..b`, `a..`).
    pub(crate) fn parse_attr_arg(&mut self) -> Option<AttrArg> {
        let is_range = |k: &TokenKind| matches!(k, TokenKind::DotDot | TokenKind::DotDotEq);
        let lo = if is_range(self.peek()) {
            None
        } else {
            Some(self.parse_attr_atom()?)
        };
        if !is_range(self.peek()) {
            return lo;
        }
        let inclusive = matches!(self.peek(), TokenKind::DotDotEq);
        self.bump();
        let hi = if matches!(self.peek(), TokenKind::Comma | TokenKind::RParen) {
            None
        } else {
            Some(Box::new(self.parse_attr_atom()?))
        };
        Some(AttrArg::Range {
            lo: lo.map(Box::new),
            hi,
            inclusive,
        })
    }

    pub(crate) fn parse_attr_atom(&mut self) -> Option<AttrArg> {
        let ident_name = match self.peek().clone() {
            TokenKind::Ident(n) => Some(n),
            // `#[on_panic(return = -1)]`.
            TokenKind::Return if matches!(self.peek_next(), Some(TokenKind::Eq)) => {
                Some("return".to_owned())
            }
            _ => None,
        };
        if let Some(name) = ident_name {
            // `key = value` form when an identifier is followed by `=`.
            if matches!(self.peek_next(), Some(TokenKind::Eq)) {
                self.bump(); // ident
                self.bump(); // `=`
                let lit = self.parse_attr_literal()?;
                return Some(AttrArg::KeyValue(name, lit));
            }
            if matches!(
                self.peek_next(),
                Some(TokenKind::LParen | TokenKind::ColonColon)
            ) {
                let path = self.parse_attr_path()?;
                let body = self.parse_attr_body();
                return Some(AttrArg::Nested {
                    path,
                    body: Box::new(body),
                });
            }
            // Plain identifier (no `=`, no `(`): treat as a Word arg.
            self.bump();
            return Some(AttrArg::Word(name));
        }
        if let Some(lit) = self.try_parse_attr_literal() {
            return Some(AttrArg::Lit(lit));
        }
        let s = self.peek_span();
        self.error(
            Code::INVALID_ATTRIBUTE,
            s,
            format!(
                "unexpected {} in attribute arguments",
                self.peek().describe()
            ),
        );
        None
    }

    pub(crate) fn parse_attr_literal(&mut self) -> Option<Literal> {
        self.try_parse_attr_literal().or_else(|| {
            let s = self.peek_span();
            self.error(
                Code::INVALID_ATTRIBUTE,
                s,
                format!("expected literal, found {}", self.peek().describe()),
            );
            None
        })
    }

    pub(crate) fn try_parse_attr_literal(&mut self) -> Option<Literal> {
        // `-1`: a negative literal, as Rust attributes allow.
        let neg = self.eat(&TokenKind::Minus);
        let lit = match self.peek().clone() {
            TokenKind::IntLit(s) => {
                self.bump();
                Literal::Int(if neg { format!("-{s}") } else { s })
            }
            TokenKind::FloatLit(s) => {
                self.bump();
                Literal::Float(if neg { format!("-{s}") } else { s })
            }
            TokenKind::StrLit(s) => {
                self.bump();
                Literal::Str(s)
            }
            TokenKind::RawStrLit(s) => {
                self.bump();
                Literal::RawStr(s)
            }
            TokenKind::CharLit(s) => {
                self.bump();
                Literal::Char(s)
            }
            TokenKind::CCharLit(s) => {
                self.bump();
                Literal::CChar(s)
            }
            TokenKind::ByteCharLit(s) => {
                self.bump();
                byte_lit(&s)
            }
            TokenKind::True => {
                self.bump();
                Literal::Bool(true)
            }
            TokenKind::False => {
                self.bump();
                Literal::Bool(false)
            }
            TokenKind::None => {
                self.bump();
                Literal::None
            }
            _ => return None,
        };
        Some(lit)
    }
}
