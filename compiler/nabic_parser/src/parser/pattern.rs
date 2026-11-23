//! Patterns and destructuring lets.

#[allow(clippy::wildcard_imports)]
use crate::parser::*;

impl Parser<'_> {
    pub(crate) fn fresh_destructure_temp(&mut self) -> String {
        let n = self.next_destructure_temp;
        self.next_destructure_temp = self.next_destructure_temp.wrapping_add(1);
        format!("__destruct{n}")
    }

    /// `if let` / `while let` pattern with an optional `: T`;
    /// `(a, b): T` matches a `__destructN: T` and returns the statement that
    /// destructures it.
    pub(crate) fn parse_let_cond_pattern(&mut self) -> Option<(Pattern, Option<Stmt>)> {
        let pat = self.parse_pattern()?;
        if !self.eat(&TokenKind::Colon) {
            return Some((pat, None));
        }
        let ty = self.parse_type()?;
        let span = pat.span;
        let typed = |binder| Pattern {
            kind: PatternKind::TypeCheck { binder, ty },
            span,
        };
        if matches!(pat.kind, PatternKind::Wildcard) {
            return Some((typed(None), None));
        }
        let tmp = self.fresh_destructure_temp();
        let init = Expr {
            span,
            kind: ExprKind::Path(Path {
                segments: vec![PathSegment {
                    name: tmp.clone(),
                    generics: Vec::new(),
                }],
                span,
                origin: None,
            }),
        };
        let stmt = Stmt {
            kind: StmtKind::LetPattern {
                pat,
                ty: None,
                init,
            },
            span,
        };
        Some((typed(Some(tmp)), Some(stmt)))
    }
}

impl Parser<'_> {
    /// `(elem, ...)` of a `let` destructure: names, `..` / `..name`, or
    /// nested groups, which become a temp plus a queued
    /// `LetTuple` in `pending_stmts`. The flag: ended in `,` (`(a)` groups,
    /// `(a,)` is a 1-tuple).
    pub(crate) fn parse_let_tuple_pattern(
        &mut self,
        outer_span: Span,
    ) -> Option<(Vec<String>, Option<LetTupleRest>, bool)> {
        self.expect(&TokenKind::LParen, "`(`")?;
        let mut names: Vec<String> = Vec::new();
        let mut rest: Option<LetTupleRest> = None;
        let mut trailing_comma = false;
        while !matches!(self.peek(), TokenKind::RParen | TokenKind::Eof) {
            if matches!(self.peek(), TokenKind::DotDot) {
                self.bump();
                if rest.is_some() {
                    let s = self.peek_span();
                    self.error(
                        Code::REST_PATTERN,
                        s,
                        "tuple destructure: only one `..` rest pattern allowed",
                    );
                    return None;
                }
                let name = match self.peek().clone() {
                    TokenKind::Ident(n) => {
                        self.bump();
                        Some(n)
                    }
                    _ => None,
                };
                rest = Some((names.len(), name));
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
                continue;
            }
            // Nested `((a, b), c)`: bind a temp and queue
            // `let (a, b) = __destructN;` so the names land in this scope.
            if matches!(self.peek(), TokenKind::LParen) {
                let inner_span = self.peek_span();
                let (mut inner_names, inner_rest, inner_comma) =
                    self.parse_let_tuple_pattern(inner_span)?;
                // `((a), b)`: the inner parens only group.
                if inner_names.len() == 1 && inner_rest.is_none() && !inner_comma {
                    names.extend(inner_names.pop());
                    trailing_comma = self.eat(&TokenKind::Comma);
                    if !trailing_comma {
                        break;
                    }
                    continue;
                }
                let temp_name = self.fresh_destructure_temp();
                names.push(temp_name.clone());
                let init = Expr {
                    span: inner_span,
                    kind: ExprKind::Path(Path {
                        segments: vec![PathSegment {
                            name: temp_name,
                            generics: Vec::new(),
                        }],
                        span: inner_span,
                        origin: None,
                    }),
                };
                self.pending_stmts.push(Stmt {
                    kind: StmtKind::LetTuple {
                        names: inner_names,
                        rest: inner_rest,
                        ty: None,
                        init,
                    },
                    span: outer_span,
                });
            } else {
                let n = if let TokenKind::Ident(n) = self.peek().clone() {
                    self.bump();
                    n
                } else {
                    let s = self.peek_span();
                    self.expected_name(s, "binding name");
                    return None;
                };
                // `n @ (x, y)`: bind `n`, then destructure it.
                if self.eat(&TokenKind::At) {
                    let inner_span = self.peek_span();
                    let sub = self.parse_let_sub_pattern(&n, inner_span)?;
                    self.pending_stmts.push(sub);
                }
                names.push(n);
            }
            trailing_comma = self.eat(&TokenKind::Comma);
            if !trailing_comma {
                break;
            }
        }
        self.expect(&TokenKind::RParen, "`)`")?;
        Some((names, rest, trailing_comma))
    }

    /// The pattern after `let name @`: an irrefutable destructure of
    /// `name`, checked in HIR.
    pub(crate) fn parse_let_sub_pattern(&mut self, name: &str, span: Span) -> Option<Stmt> {
        let init = Expr {
            span,
            kind: ExprKind::Path(Path {
                segments: vec![PathSegment {
                    name: name.to_owned(),
                    generics: Vec::new(),
                }],
                span,
                origin: None,
            }),
        };
        let pat = self.with_struct_expr(true, Self::parse_pattern_atom)?;
        Some(Stmt {
            kind: StmtKind::LetPattern {
                pat,
                ty: None,
                init,
            },
            span,
        })
    }

    /// `Path { f, g: alias, .. }` of a `let` struct destructure:
    /// the path, `(field, binding)` pairs, and whether `..` ends it.
    #[allow(clippy::type_complexity)]
    pub(crate) fn parse_let_struct_fields(
        &mut self,
    ) -> Option<(Path, Vec<(String, String)>, bool)> {
        let path = self.parse_path()?;
        self.expect(&TokenKind::LBrace, "`{`")?;
        let mut fields: Vec<(String, String)> = Vec::new();
        let mut rest = false;
        while !matches!(self.peek(), TokenKind::RBrace | TokenKind::Eof) {
            if matches!(self.peek(), TokenKind::DotDot) {
                self.bump();
                rest = true;
                break;
            }
            let field_name = if let Some(n) = self.peek_name() {
                self.bump_name();
                n
            } else {
                let s = self.peek_span();
                self.error(Code::SYNTAX, s, "expected field name");
                return None;
            };
            let bind_name = if self.eat(&TokenKind::Colon) {
                if let TokenKind::Ident(n) = self.peek().clone() {
                    self.bump();
                    n
                } else {
                    let s = self.peek_span();
                    self.expected_name(s, "rebind name");
                    return None;
                }
            } else {
                field_name.clone()
            };
            fields.push((field_name, bind_name));
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        self.expect(&TokenKind::RBrace, "`}`")?;
        Some((path, fields, rest))
    }
}

impl Parser<'_> {
    /// `(p, .., q)` after a variant path (empty without parentheses).
    /// `..` stands for the fields not written.
    pub(crate) fn parse_variant_pattern_args(&mut self) -> Option<Vec<Pattern>> {
        Some(if self.eat(&TokenKind::LParen) {
            let mut sub = Vec::new();
            while !matches!(self.peek(), TokenKind::RParen | TokenKind::Eof) {
                // `V(a, ..)`: the remaining fields, any value.
                if matches!(self.peek(), TokenKind::DotDot) {
                    let span = self.peek_span();
                    self.bump();
                    sub.push(Pattern {
                        kind: PatternKind::Rest(None),
                        span,
                    });
                } else {
                    sub.push(self.parse_pattern()?);
                }
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
            }
            self.expect(&TokenKind::RParen, "`)`")?;
            sub
        } else {
            Vec::new()
        })
    }

    /// An item of a tuple or slice pattern: `..`, `..name` (the items
    /// between, bound as one value) or a pattern.
    pub(crate) fn parse_rest_or_pattern(&mut self) -> Option<Pattern> {
        if !matches!(self.peek(), TokenKind::DotDot) {
            return self.parse_pattern();
        }
        let span = self.peek_span();
        self.bump();
        let name = match self.peek().clone() {
            TokenKind::Ident(n) if n != "_" => {
                self.bump();
                Some(n)
            }
            TokenKind::Ident(_) => {
                self.bump();
                None
            }
            _ => None,
        };
        Some(Pattern {
            kind: PatternKind::Rest(name),
            span,
        })
    }

    /// The items of `[a, ..rest, z]` after its `[`: patterns, and `..` or
    /// `..name` for the elements between.
    pub(crate) fn parse_slice_pattern_items(&mut self) -> Option<Vec<Pattern>> {
        let mut items = Vec::new();
        while !matches!(self.peek(), TokenKind::RBracket | TokenKind::Eof) {
            items.push(self.parse_rest_or_pattern()?);
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        self.expect(&TokenKind::RBracket, "`]`")?;
        Some(items)
    }

    pub(crate) fn parse_struct_pattern_body(&mut self, path: Path) -> Option<PatternKind> {
        self.expect(&TokenKind::LBrace, "`{`")?;
        let mut fields = Vec::new();
        let mut rest = false;
        while !matches!(self.peek(), TokenKind::RBrace | TokenKind::Eof) {
            if self.eat(&TokenKind::DotDot) {
                rest = true;
                break;
            }
            let name = if let Some(n) = self.peek_name() {
                self.bump_name();
                n
            } else {
                let s = self.peek_span();
                self.error(Code::SYNTAX, s, "expected field name");
                return None;
            };
            // `Foo { x: pat }` or shorthand `Foo { x }` (binds `x`).
            let pat = if self.eat(&TokenKind::Colon) {
                Some(self.parse_pattern()?)
            } else {
                None
            };
            fields.push(FieldPattern { name, pat });
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        self.expect(&TokenKind::RBrace, "`}`")?;
        Some(PatternKind::Struct { path, fields, rest })
    }

    pub(crate) fn parse_pattern(&mut self) -> Option<Pattern> {
        let first = self.parse_pattern_atom()?;
        if !matches!(self.peek(), TokenKind::Pipe) {
            return Some(first);
        }
        let span = first.span;
        let mut alts = vec![first];
        while self.eat(&TokenKind::Pipe) {
            alts.push(self.parse_pattern_atom()?);
        }
        Some(Pattern {
            kind: PatternKind::Or(alts),
            span,
        })
    }

    /// After a binding's name: `name` or `name @ pat` (binds
    /// the whole value matched by `pat`).
    pub(crate) fn parse_binding_rest(&mut self, name: String) -> Option<PatternKind> {
        let sub = if self.eat(&TokenKind::At) {
            Some(Box::new(self.parse_pattern_atom()?))
        } else {
            None
        };
        Some(PatternKind::Binding { name, sub })
    }

    pub(crate) fn parse_pattern_atom(&mut self) -> Option<Pattern> {
        self.nested(Self::parse_pattern_atom_inner)
    }

    /// `name!( .. )` where a pattern is expected: expand and
    /// re-parse the result as a pattern.
    // `Parser::parse_*` is not general over the expansion's lifetime.
    #[allow(clippy::redundant_closure_for_method_calls)]
    pub(crate) fn macro_call_pattern(&mut self) -> Option<Pattern> {
        let (def, name, span, call) = self.take_macro_call(false)?;
        let poison = || Pattern {
            kind: PatternKind::Wildcard,
            span,
        };
        let pat = def
            .and_then(|d| self.expand_call(&d, &name, span, &call, false, |p| p.parse_pattern()));
        Some(pat.flatten().unwrap_or_else(poison))
    }

    #[allow(clippy::cognitive_complexity)] // one arm per pattern form
    pub(crate) fn parse_pattern_atom_inner(&mut self) -> Option<Pattern> {
        if let Some(pat) = self.macro_call_pattern() {
            return Some(pat);
        }
        if let Some(pat) = self.rust_pattern() {
            return pat;
        }
        let span = self.peek_span();
        let num = match self.peek() {
            TokenKind::Minus => self.tokens.get(self.pos + 1).map(|t| &t.kind),
            k => Some(k),
        };
        if let Some(TokenKind::IntLit(t) | TokenKind::FloatLit(t)) = num {
            if let Some(sfx) = literal::user_suffix(t) {
                self.error(
                    Code::PATTERN_TYPE,
                    span,
                    format!(
                        "a `{sfx}` suffix literal is a `#[literal]` handler call and can't be a \
                         pattern"
                    ),
                );
                return None;
            }
        }
        // Negative numeric literal: `-NUM`.
        if matches!(self.peek(), TokenKind::Minus) {
            self.bump();
            let lit = match self.peek().clone() {
                TokenKind::IntLit(s) => {
                    self.bump();
                    Literal::Int(format!("-{s}"))
                }
                TokenKind::FloatLit(s) => {
                    self.bump();
                    Literal::Float(format!("-{s}"))
                }
                _ => {
                    let s = self.peek_span();
                    self.error(
                        Code::SYNTAX,
                        s,
                        format!(
                            "expected literal after `-`, found {}",
                            self.peek().describe()
                        ),
                    );
                    return None;
                }
            };
            return Some(self.maybe_extend_range(span, lit));
        }
        let kind = match self.peek().clone() {
            TokenKind::IntLit(s) => {
                self.bump();
                return Some(self.maybe_extend_range(span, Literal::Int(s)));
            }
            TokenKind::FloatLit(s) => {
                self.bump();
                return Some(self.maybe_extend_range(span, Literal::Float(s)));
            }
            TokenKind::StrLit(s) => {
                self.bump();
                PatternKind::Literal(Literal::Str(s))
            }
            TokenKind::RawStrLit(s) => {
                self.bump();
                PatternKind::Literal(Literal::RawStr(s))
            }
            TokenKind::CharLit(s) => {
                self.bump();
                return Some(self.maybe_extend_range(span, Literal::Char(s)));
            }
            TokenKind::CCharLit(s) => {
                self.bump();
                return Some(self.maybe_extend_range(span, Literal::CChar(s)));
            }
            TokenKind::ByteCharLit(s) => {
                self.bump();
                return Some(self.maybe_extend_range(span, byte_lit(&s)));
            }
            TokenKind::True => {
                self.bump();
                PatternKind::Literal(Literal::Bool(true))
            }
            TokenKind::False => {
                self.bump();
                PatternKind::Literal(Literal::Bool(false))
            }
            TokenKind::None => {
                self.bump();
                PatternKind::Literal(Literal::None)
            }
            TokenKind::Ident(name) if name == "_" => {
                self.bump();
                // `_: T` - type-check pattern without a binding.
                if self.eat(&TokenKind::Colon) {
                    let ty = self.parse_type()?;
                    PatternKind::TypeCheck { binder: None, ty }
                } else {
                    PatternKind::Wildcard
                }
            }
            TokenKind::LParen => {
                self.bump();
                let mut subs: Vec<Pattern> = Vec::new();
                let mut trailing_comma = false;
                if !matches!(self.peek(), TokenKind::RParen) {
                    loop {
                        subs.push(self.parse_rest_or_pattern()?);
                        trailing_comma = self.eat(&TokenKind::Comma);
                        if !trailing_comma || matches!(self.peek(), TokenKind::RParen) {
                            break;
                        }
                    }
                }
                self.expect(&TokenKind::RParen, "`)`")?;
                // `(p)` groups; a 1-tuple pattern is `(p,)`. `(..)` is
                // any tuple.
                if subs.len() == 1
                    && !trailing_comma
                    && !matches!(subs[0].kind, PatternKind::Rest(_))
                {
                    return subs.pop();
                }
                PatternKind::Tuple(subs)
            }
            TokenKind::LBracket => {
                self.bump();
                PatternKind::Slice(self.parse_slice_pattern_items()?)
            }
            TokenKind::Ident(_) => {
                // Multi-segment path, or `Name(..)` (an imported variant
                // such as `Ok(v)`) -> enum variant pattern.
                if matches!(
                    self.peek_next(),
                    Some(TokenKind::ColonColon | TokenKind::LParen)
                ) {
                    let path = self.parse_path()?;
                    // Not where a block follows (`catch E::V { body }`).
                    if self.struct_expr_allowed && matches!(self.peek(), TokenKind::LBrace) {
                        self.parse_struct_pattern_body(path)?
                    } else {
                        let args = self.parse_variant_pattern_args()?;
                        PatternKind::EnumVariant { path, args }
                    }
                } else if self.struct_expr_allowed
                    && matches!(self.peek_next(), Some(TokenKind::LBrace))
                {
                    // `Name { .. }` struct pattern. Disabled where a block
                    // follows the pattern (`catch e { body }`).
                    let path = self.parse_path()?;
                    self.parse_struct_pattern_body(path)?
                } else if matches!(self.peek_next(), Some(TokenKind::Colon)) {
                    // `name: T` - type-narrow pattern (sum-type slot).
                    let TokenKind::Ident(name) = self.peek().clone() else {
                        unreachable!()
                    };
                    self.bump(); // ident
                    self.bump(); // ':'
                    let ty = self.parse_type()?;
                    PatternKind::TypeCheck {
                        binder: Some(name),
                        ty,
                    }
                } else {
                    let TokenKind::Ident(name) = self.peek().clone() else {
                        unreachable!()
                    };
                    self.bump();
                    self.parse_binding_rest(name)?
                }
            }
            TokenKind::ColonColon if self.at_global_path() => {
                let path = self.parse_path()?;
                if self.struct_expr_allowed && matches!(self.peek(), TokenKind::LBrace) {
                    self.parse_struct_pattern_body(path)?
                } else {
                    let args = self.parse_variant_pattern_args()?;
                    PatternKind::EnumVariant { path, args }
                }
            }
            TokenKind::SelfUpper
                if self.struct_expr_allowed
                    && matches!(self.peek_next(), Some(TokenKind::LBrace)) =>
            {
                self.bump();
                let path = Path {
                    segments: vec![PathSegment {
                        name: "Self".to_owned(),
                        generics: Vec::new(),
                    }],
                    span,
                    origin: None,
                };
                self.parse_struct_pattern_body(path)?
            }
            other => {
                self.error(
                    Code::SYNTAX,
                    span,
                    format!("expected pattern, found {}", other.describe()),
                );
                return None;
            }
        };
        Some(Pattern { kind, span })
    }
}
