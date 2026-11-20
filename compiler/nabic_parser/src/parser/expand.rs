//! Built-in and user macro calls.

#[allow(clippy::wildcard_imports)]
use crate::parser::*;

impl Parser<'_> {
    /// `cfg!(pred)`: a `bool` literal, folded here. Without
    /// a build's cfg (tools) it reads as the empty environment.
    pub(crate) fn cfg_macro(&mut self) -> Option<Expr> {
        let start = self.peek_span();
        let at = |k: usize| self.tokens.get(self.pos + k).map(|t| &t.kind);
        if self.pending.is_some()
            || !matches!(at(0), Some(TokenKind::Ident(n)) if n == "cfg")
            || at(1) != Some(&TokenKind::Bang)
            || at(2) != Some(&TokenKind::LParen)
        {
            return None;
        }
        self.bump();
        self.bump();
        let body = self.parse_attr_body();
        let value = match &body {
            AttrBody::Args(args) if args.len() == 1 => {
                if let Some(why) = crate::cfg::check_pred(&args[0]) {
                    self.error(Code::INVALID_CFG, start, why);
                }
                self.cfg.as_ref().is_some_and(|env| env.eval(&args[0]))
            }
            _ => {
                self.error(
                    Code::INVALID_CFG,
                    start,
                    "`cfg!` takes exactly one predicate".to_owned(),
                );
                false
            }
        };
        Some(Expr {
            kind: ExprKind::Literal(Literal::Bool(value)),
            span: start,
        })
    }

    /// `include_str!` / `include_bytes!`: the file (relative to this one),
    /// read now, as a literal.
    pub(crate) fn include_macro(&mut self) -> Option<Expr> {
        let start = self.peek_span();
        let at = |k: usize| self.tokens.get(self.pos + k).map(|t| &t.kind);
        let (name, bytes) = match at(0) {
            Some(TokenKind::Ident(n)) if n == "include_str" => ("include_str", false),
            Some(TokenKind::Ident(n)) if n == "include_bytes" => ("include_bytes", true),
            _ => return None,
        };
        if self.pending.is_some()
            || at(1) != Some(&TokenKind::Bang)
            || at(2) != Some(&TokenKind::LParen)
        {
            return None;
        }
        let lit = match (at(3), at(4)) {
            (Some(TokenKind::StrLit(s)), Some(TokenKind::RParen)) => Some(s.clone()),
            _ => None,
        };
        self.bump();
        self.bump();
        let read = if let Some(lit) = lit {
            for _ in 0..3 {
                self.bump();
            }
            include_file(&self.file, &lit, bytes)
        } else {
            let _ = self.parse_attr_body();
            Err("takes one string literal path".to_owned())
        };
        let lit = read.unwrap_or_else(|msg| {
            self.error(Code::INCLUDE_FAILED, start, format!("`{name}!` {msg}"));
            Literal::Str(String::new())
        });
        Some(Expr {
            kind: ExprKind::Literal(lit),
            span: start,
        })
    }

    /// `name!( .. )` / `name![ .. ]` / `name!{ .. }` where `name` is a user
    /// macro: capture the token tree, expand, and re-parse the
    /// result as an expression.
    // `Parser::parse_*` is not general over the expansion's lifetime.
    #[allow(clippy::redundant_closure_for_method_calls)]
    pub(crate) fn macro_call_expr(&mut self) -> Option<Expr> {
        let (def, name, start, call) = self.take_macro_call(false)?;
        let poison = || Expr {
            kind: ExprKind::Tuple(Vec::new()),
            span: start,
        };
        let e =
            def.and_then(|d| self.expand_call(&d, &name, start, &call, false, |p| p.parse_expr()));
        Some(e.flatten().unwrap_or_else(poison))
    }

    /// The user macro call `self.pos` starts, `name!(..)` or
    /// `dep::name!(..)`: its qualifier, name and the index of
    /// its opening bracket. Nothing is consumed.
    pub(crate) fn macro_head(&self) -> Option<(Vec<String>, String, usize)> {
        if self.pending.is_some() {
            return None;
        }
        let mut k = self.pos;
        let mut path = Vec::new();
        loop {
            let TokenKind::Ident(n) = &self.tokens.get(k)?.kind else {
                return None;
            };
            path.push(n.clone());
            match self.tokens.get(k + 1)?.kind {
                TokenKind::ColonColon => k += 2,
                TokenKind::Bang => break,
                _ => return None,
            }
        }
        let open = k + 2;
        if !matches!(
            self.tokens.get(open)?.kind,
            TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace
        ) {
            return None;
        }
        let name = path.pop()?;
        Some((path, name, open))
    }

    /// Take the macro call at `self.pos` (only one ending in `;`, with
    /// the `;`, when `semi`): its block (`None`: a qualified name that
    /// names none, reported), name, span and argument tokens. `None`, with
    /// nothing consumed, when no macro call is there.
    pub(crate) fn take_macro_call(
        &mut self,
        semi: bool,
    ) -> Option<(Option<MetaDef>, String, Span, Vec<Token>)> {
        if self.meta.is_empty() {
            return None;
        }
        let (qual, name, open) = self.macro_head()?;
        if qual.is_empty() && self.meta.find_macro(&name).is_none() {
            return None;
        }
        let close = macros::close_of(self.tokens, open)?;
        if semi && self.tokens.get(close + 1).map(|t| &t.kind) != Some(&TokenKind::Semi) {
            return None;
        }
        let start = self.peek_span();
        let at = self.tokens[open - 2].span;
        let def = match self.find_meta(MetaKind::Macro, &qual, &name, at)? {
            MetaUse::Def(d) => Some(d),
            MetaUse::Bad => None,
        };
        let call = self.tokens[open + 1..close].to_vec();
        self.pos = close + 1 + usize::from(semi);
        Some((def, name, start, call))
    }

    /// Expand the call of macro `def` and parse the result with `parse`
    /// (in braces when `braced`). `None`: it failed (reported).
    pub(crate) fn expand_call<R>(
        &mut self,
        def: &MetaDef,
        name: &str,
        start: Span,
        call: &[Token],
        braced: bool,
        parse: impl FnOnce(&mut Parser<'_>) -> R,
    ) -> Option<R> {
        let key = format!("macro:{name}:{}", def.package);
        if self.meta_stack.iter().any(|k| *k == key) || self.meta_stack.len() >= 256 {
            self.error(
                Code::EXPANSION_LIMIT,
                start,
                format!("`{name}!` expands too deeply or cyclically"),
            );
            return None;
        }
        let (out, diags) = macros::expand_macro(def, call, start);
        self.diagnostics.extend(diags);
        let out = out?;
        self.expanded
            .push(format!("// expanded from {name}!\n{}", meta::render(&out)));
        let toks = if braced {
            let mut toks = vec![Token::new(TokenKind::LBrace, start)];
            toks.extend(out);
            toks.push(Token::new(TokenKind::RBrace, start));
            toks
        } else {
            out
        };
        let mut p = Parser::new(&toks, self.file.clone());
        p.ext = self.ext.clone();
        p.meta = std::sync::Arc::clone(&self.meta);
        p.literals = std::sync::Arc::clone(&self.literals);
        p.cfg.clone_from(&self.cfg);
        p.package.clone_from(&self.package);
        p.meta_imports.clone_from(&self.meta_imports);
        p.meta_stack.clone_from(&self.meta_stack);
        p.meta_stack.push(key);
        let r = parse(&mut p);
        self.diagnostics.extend(p.diagnostics);
        self.expanded.append(&mut p.expanded);
        Some(r)
    }

    /// `name!( .. );` at item position: expand and re-parse as items (maybe
    /// none). `None` when no user-macro call is here.
    pub(crate) fn macro_call_items(&mut self) -> Option<Vec<Item>> {
        let (def, name, start, call) = self.take_macro_call(false)?;
        if self.peek() == &TokenKind::Semi {
            self.bump();
        }
        let items = def.and_then(|d| {
            self.expand_call(&d, &name, start, &call, false, |p| {
                let mut items = Vec::new();
                while !p.at_eof() {
                    let before = p.pos;
                    if p.meta_block().is_some() {
                        continue;
                    }
                    match p.parse_item() {
                        Some(i) => {
                            items.push(i);
                            items.append(&mut p.pending_items);
                        }
                        None if std::mem::take(&mut p.vanished) => {}
                        None if p.pos == before => break,
                        None => {}
                    }
                }
                items
            })
        });
        Some(items.unwrap_or_default())
    }

    /// `name!( .. );` at statement position: splice the expansion into the
    /// block as statements. Only a `;`-terminated call; a block-tail
    /// `name!(..)` stays an expression.
    // `Parser::parse_*` is not general over the expansion's lifetime.
    #[allow(clippy::redundant_closure_for_method_calls)]
    pub(crate) fn macro_call_stmts(&mut self) -> Option<Vec<Stmt>> {
        let (def, name, start, call) = self.take_macro_call(true)?;
        let block = def
            .and_then(|d| {
                self.expand_call(&d, &name, start, &call, true, |p| p.parse_block_stmts())
            })
            .flatten();
        let Some(block) = block else {
            return Some(Vec::new());
        };
        let mut stmts = block.stmts;
        if let Some(e) = block.trailing {
            stmts.push(Stmt {
                kind: StmtKind::Expr(*e),
                span: start,
            });
        }
        Some(stmts)
    }
}
