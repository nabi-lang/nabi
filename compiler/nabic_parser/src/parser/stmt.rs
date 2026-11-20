//! Statements.

#[allow(clippy::wildcard_imports)]
use crate::parser::*;

impl Parser<'_> {
    /// `target = value` in expression position, from the `=`: a block
    /// holding the assignment, of unit type.
    pub(crate) fn assign_tail(&mut self, target: Expr) -> Option<Expr> {
        let span = target.span;
        self.expect(&TokenKind::Eq, "`=`")?;
        let value = self.parse_expr()?;
        Some(Expr {
            kind: ExprKind::Block(Block {
                stmts: vec![Stmt {
                    kind: StmtKind::Assign(target, value),
                    span,
                }],
                trailing: None,
                span,
                label: None,
            }),
            span,
        })
    }

    /// Arm body. `return` / `break` / `continue` / `throw` are statements,
    /// so `none => return 0,` wraps one in a block.
    pub(crate) fn parse_arm_body(&mut self) -> Option<Expr> {
        let span = self.peek_span();
        let label = |p: &mut Self| {
            if let TokenKind::Label(name) = p.peek().clone() {
                p.bump();
                Some(name)
            } else {
                None
            }
        };
        let value = |p: &mut Self| {
            if matches!(
                p.peek(),
                TokenKind::Comma
                    | TokenKind::RBrace
                    | TokenKind::RParen
                    | TokenKind::RBracket
                    | TokenKind::Semi
            ) || (!p.struct_expr_allowed && matches!(p.peek(), TokenKind::LBrace))
            {
                Some(None)
            } else {
                p.parse_expr().map(Some)
            }
        };
        let kind = match self.peek() {
            TokenKind::Return => {
                let at = self.bump().span;
                let labeled = self.return_label(at);
                let v = value(self)?;
                if v.is_none() && !labeled {
                    self.bare_return(at);
                }
                StmtKind::Return(v)
            }
            TokenKind::Break => {
                self.bump();
                let label = label(self);
                StmtKind::Break {
                    label,
                    value: value(self)?,
                }
            }
            TokenKind::Continue => {
                self.bump();
                StmtKind::Continue { label: label(self) }
            }
            TokenKind::Throw => {
                self.bump();
                StmtKind::Throw(self.parse_expr()?)
            }
            _ => {
                let e = self.parse_expr()?;
                return if matches!(self.peek(), TokenKind::Eq) {
                    self.assign_tail(e)
                } else {
                    Some(e)
                };
            }
        };
        Some(Expr {
            kind: ExprKind::Block(Block {
                stmts: vec![Stmt { kind, span }],
                trailing: None,
                span,
                label: None,
            }),
            span,
        })
    }

    /// `let PAT [: T] = init else { .. };` ->
    /// `let (b1, .., bn,) = match init { PAT => (b1, .., bn,), _ => else };`
    /// (one binder: a plain `let`; none: an expression statement).
    pub(crate) fn let_else_pattern(
        &mut self,
        pat: Pattern,
        ty: Option<Ty>,
        init: Expr,
        else_block: Block,
        start: Span,
    ) -> Stmt {
        fn binders(p: &Pattern, out: &mut Vec<String>) {
            match &p.kind {
                PatternKind::Binding { name, sub } => {
                    out.push(name.clone());
                    if let Some(s) = sub {
                        binders(s, out);
                    }
                }
                PatternKind::TypeCheck {
                    binder: Some(n), ..
                }
                | PatternKind::Rest(Some(n)) => out.push(n.clone()),
                PatternKind::Tuple(ps)
                | PatternKind::Slice(ps)
                | PatternKind::EnumVariant { args: ps, .. } => {
                    ps.iter().for_each(|p| binders(p, out));
                }
                PatternKind::Or(ps) => {
                    if let Some(p) = ps.first() {
                        binders(p, out);
                    }
                }
                PatternKind::Struct { fields, .. } => {
                    for f in fields {
                        match &f.pat {
                            Some(p) => binders(p, out),
                            None => out.push(f.name.clone()),
                        }
                    }
                }
                _ => {}
            }
        }
        let span = init.span;
        let path = |name: &str| Expr {
            kind: ExprKind::Path(Path {
                segments: vec![PathSegment {
                    name: name.to_string(),
                    generics: vec![],
                }],
                span,
                origin: None,
            }),
            span,
        };
        let mut names = Vec::new();
        binders(&pat, &mut names);
        let body = match names.as_slice() {
            [one] => path(one),
            _ => Expr {
                kind: ExprKind::Tuple(names.iter().map(|n| path(n)).collect()),
                span,
            },
        };
        let scrutinee = match ty {
            Some(ty) => Expr {
                kind: ExprKind::Block(Block {
                    stmts: vec![Stmt {
                        kind: StmtKind::Let {
                            name: "__letelse_s".to_string(),
                            ty: Some(ty),
                            init,
                        },
                        span,
                    }],
                    trailing: Some(Box::new(path("__letelse_s"))),
                    span,
                    label: None,
                }),
                span,
            },
            None => init,
        };
        let else_span = else_block.span;
        let m = Expr {
            kind: ExprKind::Match {
                scrutinee: Box::new(scrutinee),
                arms: vec![
                    MatchArm {
                        pat,
                        guard: None,
                        body,
                    },
                    MatchArm {
                        pat: Pattern {
                            kind: PatternKind::Wildcard,
                            span: else_span,
                        },
                        guard: None,
                        body: Expr {
                            kind: ExprKind::Block(else_block),
                            span: else_span,
                        },
                    },
                ],
            },
            span,
        };
        let kind = match names.len() {
            0 => StmtKind::Expr(m),
            1 => StmtKind::Let {
                name: names.pop().unwrap_or_default(),
                ty: None,
                init: m,
            },
            _ => {
                self.pending_stmts.push(Stmt {
                    kind: StmtKind::LetTuple {
                        names,
                        rest: None,
                        ty: None,
                        init: path("__letelse_t"),
                    },
                    span: start,
                });
                StmtKind::Let {
                    name: "__letelse_t".to_string(),
                    ty: None,
                    init: m,
                }
            }
        };
        Stmt { kind, span: start }
    }
}

impl Parser<'_> {
    /// `(stmt, may be the tail)`; the block builder decides.
    #[allow(clippy::cognitive_complexity)] // one arm per statement form
    pub(crate) fn parse_stmt(&mut self) -> Option<(Stmt, bool)> {
        let start = self.peek_span();
        if let Some(s) = self.python_stmt().or_else(|| self.rust_stmt()) {
            return Some((s, false));
        }
        if self.at_nested_fn() {
            return Some((self.parse_nested_fn()?, false));
        }
        // Module-level only (other items lift out of blocks).
        let module_only = match self.peek() {
            TokenKind::Mod => Some("a `mod`"),
            _ if self.at_kw("extern") && self.at_ctx_item() => Some("an `extern` block"),
            _ => None,
        };
        if let Some(what) = module_only {
            self.error(
                Code::MISPLACED_ITEM,
                start,
                format!("{what} is declared only at module level, not in a function body"),
            );
            return None;
        }
        match self.peek() {
            TokenKind::Use => {
                let vis = Visibility::Private;
                let attrs = Vec::new();
                let Some(use_def) = self.parse_use(vis, attrs) else {
                    if std::mem::take(&mut self.vanished) {
                        self.error(
                            Code::MISPLACED_ITEM,
                            start,
                            "a macro import goes at module level: move this `use dep::name!;` \
                             to the top of the file",
                        );
                    }
                    return None;
                };
                Some((
                    Stmt {
                        kind: StmtKind::Item(Item::Use(use_def)),
                        span: start,
                    },
                    false,
                ))
            }
            TokenKind::Let => {
                self.bump();
                // Flat forms (`Point { x, y: b }`, `(a, ..rest)`, `name`)
                // have their own statements; any other irrefutable pattern
                // is a `LetPattern`.
                let general = match self.peek() {
                    TokenKind::Ident(_) => {
                        matches!(
                            self.peek_next(),
                            Some(TokenKind::LParen | TokenKind::ColonColon)
                        ) && !self.peek_path_then_lbrace()
                    }
                    TokenKind::LParen => {
                        self.backtracks(|p| p.parse_let_tuple_pattern(start))
                            || self.stmt_has_else()
                    }
                    // `let [a, ..rest] = ..`: a slice pattern.
                    TokenKind::LBracket => true,
                    _ => false,
                } || (self.peek_path_then_lbrace()
                    && self.backtracks(|p| {
                        p.parse_let_struct_fields()
                            .filter(|_| matches!(p.peek(), TokenKind::Colon | TokenKind::Eq))
                    }));
                if general {
                    let pat = self.with_struct_expr(true, Self::parse_pattern_atom)?;
                    let ty = if self.eat(&TokenKind::Colon) {
                        Some(self.parse_type()?)
                    } else {
                        None
                    };
                    self.expect(&TokenKind::Eq, "`=`")?;
                    let init = self.parse_expr()?;
                    if self.eat(&TokenKind::Else) {
                        let else_block = self.parse_block()?;
                        self.stmt_semi()?;
                        return Some((
                            self.let_else_pattern(pat, ty, init, else_block, start),
                            false,
                        ));
                    }
                    self.stmt_semi()?;
                    return Some((
                        Stmt {
                            kind: StmtKind::LetPattern { pat, ty, init },
                            span: start,
                        },
                        false,
                    ));
                }
                // `let Point { x, y, .. } [: ty] = init;`: `let Path {`
                // can't start a plain binding, so it is a struct pattern.
                if self.peek_path_then_lbrace() {
                    let (path, fields, rest) = self.parse_let_struct_fields()?;
                    let ty = if self.eat(&TokenKind::Colon) {
                        Some(self.parse_type()?)
                    } else {
                        None
                    };
                    self.expect(&TokenKind::Eq, "`=`")?;
                    let init = self.parse_expr()?;
                    self.stmt_semi()?;
                    return Some((
                        Stmt {
                            kind: StmtKind::LetStruct {
                                path,
                                fields,
                                rest,
                                ty,
                                init,
                            },
                            span: start,
                        },
                        false,
                    ));
                }
                // `let (a, b) = ..;`. Nested groups become
                // a chain of `LetTuple`s queued on `pending_stmts`.
                if matches!(self.peek(), TokenKind::LParen) {
                    let (mut names, rest, trailing_comma) = self.parse_let_tuple_pattern(start)?;
                    let ty = if self.eat(&TokenKind::Colon) {
                        Some(self.parse_type()?)
                    } else {
                        None
                    };
                    self.expect(&TokenKind::Eq, "`=`")?;
                    let init = self.parse_expr()?;
                    self.stmt_semi()?;
                    // `let (a) = ..` only groups; `let (a,) = ..` is a 1-tuple.
                    let kind = if names.len() == 1 && rest.is_none() && !trailing_comma {
                        StmtKind::Let {
                            name: names.pop()?,
                            ty,
                            init,
                        }
                    } else {
                        StmtKind::LetTuple {
                            names,
                            rest,
                            ty,
                            init,
                        }
                    };
                    return Some((Stmt { kind, span: start }, false));
                }
                let mut name = if let TokenKind::Ident(n) = self.peek() {
                    n.clone()
                } else {
                    let s = self.peek_span();
                    self.error(
                        Code::SYNTAX,
                        s,
                        format!("expected binding name, found {}", self.peek().describe()),
                    );
                    return None;
                };
                self.bump();
                // `let mut x` (Rust): every binding is mutable already.
                if let (true, TokenKind::Ident(n)) = (name == "mut", self.peek().clone()) {
                    let at = self.prev_span().unwrap_or(start);
                    self.error_help(
                        Code::FOREIGN_SYNTAX,
                        at,
                        "Nabi has no `let mut`: variables are mutable by default",
                        format!("drop `mut`: `let {n} = ..;`"),
                    );
                    self.bump();
                    name = n;
                }
                // `let whole @ (a, b) = init;` -> `let whole = init;` then
                // a destructure of `whole`.
                let sub = if self.eat(&TokenKind::At) {
                    let s = self.peek_span();
                    Some(self.parse_let_sub_pattern(&name, s)?)
                } else {
                    None
                };
                let ty = if self.eat(&TokenKind::Colon) {
                    Some(self.parse_type()?)
                } else {
                    None
                };
                // `let x: i32;` (C, Java, TypeScript): no value yet.
                if let (TokenKind::Semi | TokenKind::RBrace, Some(t)) = (self.peek(), &ty) {
                    let t = self.text_in(t.span);
                    let at = self.peek_span();
                    self.error_help(
                        Code::LET_WITHOUT_VALUE,
                        at,
                        format!("`let {name}` needs a value"),
                        format!(
                            "give it one, `let {name}: {t} = ..;`, or start it empty: `let \
                             {name}: {t} | none = none;`"
                        ),
                    );
                    return None;
                }
                self.expect(&TokenKind::Eq, "`=`")?;
                let init = self.parse_expr()?;
                if let Some(sub) = sub {
                    self.stmt_semi()?;
                    self.pending_stmts.push(sub);
                    return Some((
                        Stmt {
                            kind: StmtKind::Let { name, ty, init },
                            span: start,
                        },
                        false,
                    ));
                }
                // `let name [: T] = expr else { .. };` ->
                // `let name = match expr { v: T => v, _ => { else } };`
                if matches!(self.peek(), TokenKind::Else) {
                    self.bump();
                    let else_block = self.parse_block()?;
                    self.stmt_semi()?;
                    let init_span = init.span;
                    let bk_span = else_block.span;
                    let binder = "__letelse_v".to_string();
                    let success_pat = if let Some(t) = &ty {
                        Pattern {
                            kind: PatternKind::TypeCheck {
                                binder: Some(binder.clone()),
                                ty: t.clone(),
                            },
                            span: init_span,
                        }
                    } else {
                        Pattern {
                            kind: PatternKind::Binding {
                                name: binder.clone(),
                                sub: None,
                            },
                            span: init_span,
                        }
                    };
                    let success_body = Expr {
                        kind: ExprKind::Path(Path {
                            segments: vec![PathSegment {
                                name: binder,
                                generics: vec![],
                            }],
                            span: init_span,
                            origin: None,
                        }),
                        span: init_span,
                    };
                    let else_expr = Expr {
                        kind: ExprKind::Block(else_block),
                        span: bk_span,
                    };
                    let arms = vec![
                        MatchArm {
                            pat: success_pat,
                            guard: None,
                            body: success_body,
                        },
                        MatchArm {
                            pat: Pattern {
                                kind: PatternKind::Wildcard,
                                span: bk_span,
                            },
                            guard: None,
                            body: else_expr,
                        },
                    ];
                    let match_expr = Expr {
                        kind: ExprKind::Match {
                            scrutinee: Box::new(init),
                            arms,
                        },
                        span: init_span,
                    };
                    return Some((
                        Stmt {
                            kind: StmtKind::Let {
                                name,
                                ty,
                                init: match_expr,
                            },
                            span: start,
                        },
                        false,
                    ));
                }
                self.stmt_semi()?;
                Some((
                    Stmt {
                        kind: StmtKind::Let { name, ty, init },
                        span: start,
                    },
                    false,
                ))
            }
            TokenKind::Return => {
                let at = self.bump().span;
                let labeled = self.return_label(at);
                let value = if matches!(self.peek(), TokenKind::Semi | TokenKind::RBrace) {
                    if !labeled {
                        self.bare_return(at);
                    }
                    None
                } else {
                    Some(self.parse_expr()?)
                };
                self.stmt_semi()?;
                Some((
                    Stmt {
                        kind: StmtKind::Return(value),
                        span: start,
                    },
                    false,
                ))
            }
            TokenKind::Break => {
                self.bump();
                // Optional `'label` after `break`.
                let label = if let TokenKind::Label(name) = self.peek().clone() {
                    self.bump();
                    Some(name)
                } else {
                    None
                };
                let value = if matches!(self.peek(), TokenKind::Semi | TokenKind::RBrace) {
                    None
                } else {
                    Some(self.parse_expr()?)
                };
                self.stmt_semi()?;
                Some((
                    Stmt {
                        kind: StmtKind::Break { label, value },
                        span: start,
                    },
                    false,
                ))
            }
            TokenKind::Continue => {
                self.bump();
                let label = if let TokenKind::Label(name) = self.peek().clone() {
                    self.bump();
                    Some(name)
                } else {
                    None
                };
                self.stmt_semi()?;
                Some((
                    Stmt {
                        kind: StmtKind::Continue { label },
                        span: start,
                    },
                    false,
                ))
            }
            TokenKind::Throw => {
                self.bump();
                let value = self.parse_expr()?;
                self.stmt_semi()?;
                Some((
                    Stmt {
                        kind: StmtKind::Throw(value),
                        span: start,
                    },
                    false,
                ))
            }
            // A block-like expression ends its statement; no `;`,
            // and a following operator starts a new one (`if c {} -1` is
            // two). Only postfix links (`.m()`, `?`) continue it.
            TokenKind::If
            | TokenKind::Match
            | TokenKind::Loop
            | TokenKind::While
            | TokenKind::For
            | TokenKind::LBrace
            | TokenKind::Label(_) => {
                self.stmt_head = true;
                let expr = self.parse_expr_bp(u32::MAX)?;
                let ends = is_block_like(&expr) || matches!(self.peek(), TokenKind::RBrace);
                if !ends || self.eat(&TokenKind::Semi) {
                    if !ends {
                        self.expect(&TokenKind::Semi, "`;`")?;
                    }
                    return Some((
                        Stmt {
                            kind: StmtKind::Expr(expr),
                            span: start,
                        },
                        false,
                    ));
                }
                Some((
                    Stmt {
                        kind: StmtKind::Trailing(expr),
                        span: start,
                    },
                    true,
                ))
            }
            _ => {
                let outer = self.stmt_at.replace(start.start);
                let expr = self.parse_expr();
                self.stmt_at = outer;
                let expr = expr?;
                if matches!(self.peek(), TokenKind::Eq) {
                    self.bump();
                    let value = self.parse_expr()?;
                    self.stmt_semi()?;
                    if !matches!(expr.kind, ExprKind::Tuple(_)) {
                        return Some((
                            Stmt {
                                kind: StmtKind::Assign(expr, value),
                                span: start,
                            },
                            false,
                        ));
                    }
                    // `(a, b) = (b, a + b)`: the right side first, then
                    // each place from left to right.
                    let mut stmts = Vec::new();
                    self.destructure_assign(expr, value, &mut stmts);
                    let block = Block {
                        stmts,
                        trailing: None,
                        span: start,
                        label: None,
                    };
                    return Some((
                        Stmt {
                            kind: StmtKind::Expr(Expr {
                                kind: ExprKind::Block(block),
                                span: start,
                            }),
                            span: start,
                        },
                        false,
                    ));
                }
                // `target op= value;`: HIR uses the `*Assign` method, else
                // `target = target op value`, the target evaluated once.
                if let Some(op) = compound_assign_op(self.peek()) {
                    self.bump();
                    let value = self.parse_expr()?;
                    self.stmt_semi()?;
                    return Some((
                        Stmt {
                            kind: StmtKind::CompoundAssign {
                                target: expr,
                                op: format!("{op}="),
                                value,
                            },
                            span: start,
                        },
                        false,
                    ));
                }
                if self.eat(&TokenKind::Semi) {
                    Some((
                        Stmt {
                            kind: StmtKind::Expr(expr),
                            span: start,
                        },
                        false,
                    ))
                } else {
                    Some((
                        Stmt {
                            kind: StmtKind::Trailing(expr),
                            span: start,
                        },
                        true,
                    ))
                }
            }
        }
    }

    /// The `;` ending a statement; optional before `}`,
    /// where leaving it out means the same as writing it.
    pub(crate) fn stmt_semi(&mut self) -> Option<()> {
        if !matches!(self.peek(), TokenKind::RBrace) {
            self.expect(&TokenKind::Semi, "`;`")?;
        }
        Some(())
    }
}
