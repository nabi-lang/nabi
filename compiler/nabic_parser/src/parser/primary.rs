//! Primary expressions, struct literals and ranges.

#[allow(clippy::wildcard_imports)]
use crate::parser::*;

impl Parser<'_> {
    #[allow(clippy::cognitive_complexity)] // one arm per primary-expression token
    pub(crate) fn parse_primary(&mut self) -> Option<Expr> {
        let stmt_head = std::mem::take(&mut self.stmt_head);
        if let Some(e) = self
            .try_magic_literal()
            .or_else(|| self.cfg_macro())
            .or_else(|| self.include_macro())
            .or_else(|| self.macro_call_expr())
            .or_else(|| self.rust_macro())
        {
            // A literal or macro call is a primary: postfix applies to it
            // (`date"2024-01-01".days()`, `m!{ .. }.len()`).
            return self.postfix_chain(e, stmt_head);
        }
        let start = self.peek_span();
        if matches!(
            self.peek(),
            TokenKind::LBracket | TokenKind::LBrace | TokenKind::LParen
        ) && self.comprehension(self.pos)
        {
            self.bump();
            return Some(Expr {
                kind: ExprKind::Tuple(Vec::new()),
                span: self.to_prev(start),
            });
        }
        let kind = match self.peek().clone() {
            // `<Type as Trait>::method(...)`. At the start of
            // a primary, `<` cannot begin a comparison.
            TokenKind::Lt => {
                let mut q = self.parse_qpath()?;
                // `<T as Tr>::X::m`: an associated function of the
                // associated type `X`.
                if self.peek() == &TokenKind::ColonColon
                    && matches!(self.peek_next(), Some(TokenKind::Ident(_)))
                {
                    self.bump();
                    if let TokenKind::Ident(m) = self.peek().clone() {
                        self.bump();
                        q.member = Some(m);
                    }
                }
                ExprKind::QPath(Box::new(q))
            }
            TokenKind::IntLit(s) => {
                self.bump();
                ExprKind::Literal(Literal::Int(s))
            }
            TokenKind::FloatLit(s) => {
                self.bump();
                ExprKind::Literal(Literal::Float(s))
            }
            TokenKind::StrLit(s) => {
                self.bump();
                ExprKind::Literal(Literal::Str(s))
            }
            TokenKind::RawStrLit(s) => {
                self.bump();
                ExprKind::Literal(Literal::RawStr(s))
            }
            TokenKind::ByteStrLit(s) => {
                self.bump();
                ExprKind::Literal(Literal::ByteStr(s))
            }
            TokenKind::FStrStart(_) => self.parse_fstring(),
            TokenKind::Ident(k) if k == "const" && self.peek_next() == Some(TokenKind::LBrace) => {
                self.bump();
                ExprKind::ConstBlock(self.parse_block()?)
            }
            TokenKind::Unsafe => {
                // `unsafe { ... }`.
                self.bump();
                if !matches!(self.peek(), TokenKind::LBrace) {
                    self.error(Code::SYNTAX, start, "expected `{` after `unsafe`");
                    return None;
                }
                ExprKind::Unsafe(self.parse_block()?)
            }
            TokenKind::Using => {
                self.bump();
                // `using_binding ::= pattern "=" expr`: a name,
                // or any irrefutable pattern over a temp that is dropped.
                let mut bindings: Vec<UsingBinding> = Vec::new();
                loop {
                    let span = self.peek_span();
                    let (name, pat) = match self.peek().clone() {
                        TokenKind::Ident(name) if self.peek_next() == Some(TokenKind::Eq) => {
                            self.bump();
                            (name, None)
                        }
                        _ => {
                            let pat = self.with_struct_expr(true, Self::parse_pattern_atom)?;
                            (format!("#using{}", self.pos), Some(pat))
                        }
                    };
                    self.expect(&TokenKind::Eq, "`=`")?;
                    // `using r = R { .. };`: no body follows the literal.
                    let init = if self.braces_then_semi() {
                        self.with_struct_expr(true, Self::parse_expr)?
                    } else {
                        self.parse_head()?
                    };
                    bindings.push((name, span, init, pat));
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                }
                // `using x = e;`: scoped to the rest of the block, which
                // `fold_using_stmts` makes its body.
                if matches!(self.peek(), TokenKind::Semi) {
                    if self.stmt_at == Some(start.start) {
                        self.using_stmts.insert((start.file, start.start), bindings);
                        return Some(Expr {
                            span: start,
                            kind: ExprKind::Tuple(Vec::new()),
                        });
                    }
                    self.error_help(
                        Code::SYNTAX,
                        start,
                        "`using x = e;` is a statement: it covers the rest of its block",
                        "start a statement with it, or write the block form \
                         `using x = e { .. }` here",
                    );
                    return None;
                }
                let body = self.parse_block()?;
                Self::build_using(bindings, body, start).kind
            }
            TokenKind::True => {
                self.bump();
                ExprKind::Literal(Literal::Bool(true))
            }
            TokenKind::False => {
                self.bump();
                ExprKind::Literal(Literal::Bool(false))
            }
            TokenKind::None => {
                self.bump();
                ExprKind::Literal(Literal::None)
            }
            TokenKind::CharLit(s) => {
                self.bump();
                ExprKind::Literal(Literal::Char(s))
            }
            TokenKind::CCharLit(s) => {
                self.bump();
                ExprKind::Literal(Literal::CChar(s))
            }
            TokenKind::ByteCharLit(s) => {
                self.bump();
                ExprKind::Literal(byte_lit(&s))
            }
            TokenKind::LParen => {
                self.bump();
                // `()` unit, `(e)` grouping, `(e,)` / `(a, b)` tuple.
                let (mut items, trailing_comma) =
                    self.with_struct_expr(true, |p| p.expr_list(&TokenKind::RParen))?;
                if items.len() == 1 && !trailing_comma && matches!(self.peek(), TokenKind::Eq) {
                    let target = items.pop()?;
                    let assign = self.with_struct_expr(true, |p| p.assign_tail(target))?;
                    self.expect(&TokenKind::RParen, "`)`")?;
                    return Some(assign);
                }
                self.expect(&TokenKind::RParen, "`)`")?;
                if items.len() == 1 && !trailing_comma {
                    items.pop()?.kind
                } else {
                    ExprKind::Tuple(items)
                }
            }
            TokenKind::Pipe | TokenKind::PipePipe => self.parse_closure_expr()?,
            TokenKind::LBrace => {
                if let Some(lit) = self.try_set_or_dict() {
                    lit
                } else {
                    ExprKind::Block(self.parse_block()?)
                }
            }
            TokenKind::Label(name) => {
                // `'label:` before `loop`, `while`, `for` or a block.
                let label_name = name;
                self.bump();
                self.expect(&TokenKind::Colon, "`:` after label")?;
                match self.peek().clone() {
                    TokenKind::Loop => {
                        self.bump();
                        let body = self.parse_block()?;
                        ExprKind::Loop {
                            label: Some(label_name),
                            body,
                        }
                    }
                    TokenKind::While => {
                        self.bump();
                        let cond = self.parse_head()?;
                        let body = self.parse_block()?;
                        ExprKind::While {
                            label: Some(label_name),
                            cond: Box::new(cond),
                            body,
                        }
                    }
                    TokenKind::For => {
                        self.bump();
                        let is_await = self.eat(&TokenKind::Await);
                        let by_ref = self.eat(&TokenKind::Ref);
                        let (pat, iter) = self.parse_for_head()?;
                        let body = self.parse_block()?;
                        ExprKind::For {
                            label: Some(label_name),
                            pat,
                            iter: Box::new(iter),
                            body,
                            is_await,
                            by_ref,
                        }
                    }
                    TokenKind::LBrace => {
                        let mut block = self.parse_block()?;
                        block.label = Some(label_name);
                        ExprKind::Block(block)
                    }
                    _ => {
                        let s = self.peek_span();
                        self.error(
                            Code::SYNTAX,
                            s,
                            "expected `loop`, `while`, `for`, or `{` after label",
                        );
                        return None;
                    }
                }
            }
            TokenKind::Loop => {
                self.bump();
                let body = self.parse_block()?;
                ExprKind::Loop { label: None, body }
            }
            TokenKind::For => {
                self.bump();
                // `for await pat in stream`.
                let is_await = self.eat(&TokenKind::Await);
                // `for ref x in xs`: each element as a place.
                let by_ref = self.eat(&TokenKind::Ref);
                let (pat, iter) = self.parse_for_head()?;
                let body = self.parse_block()?;
                ExprKind::For {
                    label: None,
                    pat,
                    iter: Box::new(iter),
                    body,
                    is_await,
                    by_ref,
                }
            }
            TokenKind::Match => {
                self.bump();
                let scrutinee = self.parse_head()?;
                self.expect(&TokenKind::LBrace, "`{`")?;
                let arms = self.with_struct_expr(true, |p| {
                    let mut arms = Vec::new();
                    while !matches!(p.peek(), TokenKind::RBrace | TokenKind::Eof) {
                        let attrs = p.cfg_only_attrs("a match arm");
                        let marks = (p.pending_stmts.len(), p.lifted.len());
                        let pat = p.parse_pattern()?;
                        let guard = if p.eat(&TokenKind::If) {
                            Some(p.parse_expr()?)
                        } else {
                            None
                        };
                        p.expect(&TokenKind::FatArrow, "`=>`")?;
                        let body = p.parse_arm_body()?;
                        p.eat(&TokenKind::Comma);
                        if !p.cfg_drops(&attrs, marks) {
                            arms.push(MatchArm { pat, guard, body });
                        }
                    }
                    Some(arms)
                })?;
                self.expect(&TokenKind::RBrace, "`}`")?;
                ExprKind::Match {
                    scrutinee: Box::new(scrutinee),
                    arms,
                }
            }
            TokenKind::Try => {
                // `try e` / `try? e` / `try! e` covers everything to its right
                // up to `catch` / `finally` (`parse_catch_tail`).
                let try_span = self.peek_span();
                let try_end = try_span.end;
                self.bump();
                // `try!` is one word: `try !x` tries the negation.
                let glued = self.peek_span().start == try_end;
                let kind = if matches!(self.peek(), TokenKind::Question) {
                    if !glued {
                        let at = self.peek_span();
                        self.error_help(
                            Code::SYNTAX,
                            at,
                            "space between `try` and `?`",
                            "write `try?` as one word",
                        );
                    }
                    self.bump();
                    TryKind::Optional
                } else if glued && self.eat(&TokenKind::Bang) {
                    TryKind::Force
                } else {
                    TryKind::Plain
                };
                // `try for x in g { .. } (a, b)`: the loop ends there, the
                // `(` starts the next expression.
                if matches!(self.peek(), TokenKind::For) {
                    self.stmt_head = true;
                }
                // `??` ends the operand of `try?` / `try!`: `try? f() ?? d`
                // is `(try? f()) ?? d`, `d` on an error. Pipes stay in it.
                let bp = if kind == TryKind::Plain {
                    0
                } else {
                    COALESCE_PREC + 1
                };
                let e = self.parse_expr_bp(bp)?;
                let operand = self.pipe_tail(e, Some(bp))?;
                ExprKind::Try {
                    kind,
                    operand: Box::new(operand),
                }
            }
            TokenKind::While => {
                self.bump();
                // `while let pat = expr { body }` desugars to
                // `loop { match expr { pat => body, _ => break } }`.
                if self.eat(&TokenKind::Let) {
                    let (pat, destructure) = self.parse_let_cond_pattern()?;
                    self.expect(&TokenKind::Eq, "`=`")?;
                    let scrutinee = self.parse_head()?;
                    let mut body_block = self.parse_block()?;
                    body_block.stmts.splice(0..0, destructure);
                    let body_span = body_block.span;
                    let then_body = Expr {
                        span: body_span,
                        kind: ExprKind::Block(body_block),
                    };
                    let break_expr = Expr {
                        span: start,
                        kind: ExprKind::Tuple(Vec::new()),
                    };
                    let break_block = Block {
                        stmts: vec![Stmt {
                            kind: StmtKind::Break {
                                label: None,
                                value: None,
                            },
                            span: start,
                        }],
                        trailing: None,
                        span: start,
                        label: None,
                    };
                    let break_arm_body = Expr {
                        span: start,
                        kind: ExprKind::Block(break_block),
                    };
                    let _ = break_expr;
                    let wildcard = Pattern {
                        kind: PatternKind::Wildcard,
                        span: start,
                    };
                    let match_expr = Expr {
                        span: start,
                        kind: ExprKind::Match {
                            scrutinee: Box::new(scrutinee),
                            arms: vec![
                                MatchArm {
                                    pat,
                                    guard: None,
                                    body: then_body,
                                },
                                MatchArm {
                                    pat: wildcard,
                                    guard: None,
                                    body: break_arm_body,
                                },
                            ],
                        },
                    };
                    let loop_body = Block {
                        stmts: vec![Stmt {
                            kind: StmtKind::Expr(match_expr),
                            span: start,
                        }],
                        trailing: None,
                        span: start,
                        label: None,
                    };
                    ExprKind::Loop {
                        label: None,
                        body: loop_body,
                    }
                } else {
                    let cond = self.parse_head()?;
                    let body = self.parse_block()?;
                    ExprKind::While {
                        label: None,
                        cond: Box::new(cond),
                        body,
                    }
                }
            }
            TokenKind::If => {
                self.bump();
                // `if let pat = expr block else?` desugars to a `match`
                // with the pattern's arm and a wildcard fallback.
                if self.eat(&TokenKind::Let) {
                    let (pat, destructure) = self.parse_let_cond_pattern()?;
                    self.expect(&TokenKind::Eq, "`=`")?;
                    let scrutinee = self.parse_head()?;
                    let mut then_block = self.parse_block()?;
                    then_block.stmts.splice(0..0, destructure);
                    let then_span = then_block.span;
                    let then_body = Expr {
                        span: then_span,
                        kind: ExprKind::Block(then_block),
                    };
                    let else_body = if self.eat_else() {
                        if matches!(self.peek(), TokenKind::If) {
                            self.parse_primary()?
                        } else {
                            let block = self.parse_block()?;
                            let s = block.span;
                            Expr {
                                span: s,
                                kind: ExprKind::Block(block),
                            }
                        }
                    } else {
                        Expr {
                            span: start,
                            kind: ExprKind::Tuple(Vec::new()),
                        }
                    };
                    let wildcard = Pattern {
                        kind: PatternKind::Wildcard,
                        span: start,
                    };
                    ExprKind::Match {
                        scrutinee: Box::new(scrutinee),
                        arms: vec![
                            MatchArm {
                                pat,
                                guard: None,
                                body: then_body,
                            },
                            MatchArm {
                                pat: wildcard,
                                guard: None,
                                body: else_body,
                            },
                        ],
                    }
                } else {
                    let cond = self.parse_head()?;
                    let then_branch = self.parse_block()?;
                    let else_branch = if self.eat_else() {
                        if matches!(self.peek(), TokenKind::If) {
                            // Chained `else if`: recurse into another primary.
                            Some(Box::new(self.parse_primary()?))
                        } else {
                            let block = self.parse_block()?;
                            let span = block.span;
                            Some(Box::new(Expr {
                                span,
                                kind: ExprKind::Block(block),
                            }))
                        }
                    } else {
                        None
                    };
                    ExprKind::If {
                        cond: Box::new(cond),
                        then_branch,
                        else_branch,
                    }
                }
            }
            TokenKind::Ident(_) | TokenKind::ColonColon
                if !matches!(self.peek(), TokenKind::ColonColon) || self.at_global_path() =>
            {
                let mut path = self.parse_path()?;
                // `Pair<i32, i32> { .. }`: `<` compares unless a
                // balanced `>` is followed by `{` or `(`.
                if matches!(self.peek(), TokenKind::Lt) && self.peek_struct_or_call_after_generics()
                {
                    self.bump(); // `<`
                    let generics = self.nested_type_list(TokenKind::Gt)?;
                    if !self.expect_closing_gt() {
                        return None;
                    }
                    if let Some(last) = path.segments.last_mut() {
                        last.generics = generics;
                    }
                }
                if std::mem::take(&mut self.head_struct_lit)
                    && matches!(self.peek(), TokenKind::LBrace)
                {
                    self.parse_struct_init_body(path)?
                } else if self.struct_expr_allowed && matches!(self.peek(), TokenKind::LBrace) {
                    // `Path { .. }`: struct init or 0-arg trailing closure
                    // by the body's first tokens:
                    //   `}`, `..rest`, `ident :|,|}`  -> struct init
                    //   anything else               -> trailing closure
                    if self.peek_struct_init_body() {
                        self.parse_struct_init_body(path)?
                    } else {
                        // `path { body }` -> call with 0-arg trailing closure.
                        let body_span = self.peek_span();
                        let label = path.segments.last().map(|s| s.name.clone());
                        let (block, bare) = self.ret_frame(label, Self::parse_block);
                        let block = block?;
                        self.note_closure_returns(body_span, bare);
                        let body = Expr {
                            span: body_span,
                            kind: ExprKind::Block(block),
                        };
                        let closure = Expr {
                            span: body_span,
                            kind: ExprKind::Closure {
                                captures: Vec::new(),
                                params: Vec::new(),
                                ret_ty: None,
                                body: Box::new(body),
                                is_async: false,
                                is_generator: false,
                            },
                        };
                        let path_span = start;
                        ExprKind::Call {
                            callee: Box::new(Expr {
                                span: path_span,
                                kind: ExprKind::Path(path),
                            }),
                            args: Vec::new(),
                            trailing_closure: Some(Box::new(closure)),
                        }
                    }
                } else if let Some(closure) =
                    self.paren_less_trailing_closure(path.segments.last().map(|s| s.name.clone()))
                {
                    // `f |x| { .. }` -> `f(|x| { .. })`.
                    ExprKind::Call {
                        callee: Box::new(Expr {
                            span: start,
                            kind: ExprKind::Path(path),
                        }),
                        args: Vec::new(),
                        trailing_closure: Some(closure),
                    }
                } else {
                    ExprKind::Path(path)
                }
            }
            TokenKind::SelfLower => {
                self.bump();
                ExprKind::SelfLower
            }
            // `Self::NAME` / `Self::f(..)` inside an impl.
            TokenKind::SelfUpper if matches!(self.peek_next(), Some(TokenKind::ColonColon)) => {
                self.bump();
                self.bump();
                let mut rest = self.parse_path()?;
                rest.segments.insert(
                    0,
                    PathSegment {
                        name: "Self".to_owned(),
                        generics: Vec::new(),
                    },
                );
                rest.span = start;
                ExprKind::Path(rest)
            }
            // `Self { x, y }` inside an impl.
            TokenKind::SelfUpper
                if self.struct_expr_allowed
                    && matches!(self.peek_next(), Some(TokenKind::LBrace))
                    && self.struct_init_body_at(self.pos + 1) =>
            {
                self.bump();
                let path = Path {
                    segments: vec![PathSegment {
                        name: "Self".to_owned(),
                        generics: Vec::new(),
                    }],
                    span: start,
                    origin: None,
                };
                self.parse_struct_init_body(path)?
            }
            TokenKind::SelfUpper => {
                self.bump();
                ExprKind::SelfUpper
            }
            // `[a, b]` array, or capture list `[ref x] || ..`: `ref` or
            // `move` inside, or names then `]` and `|` / `||`, decide.
            TokenKind::LBracket if self.at_capture_list() => self.parse_capture_closure()?,
            TokenKind::LBracket => {
                self.bump();
                let (mut items, trailing_comma) =
                    self.with_struct_expr(true, |p| p.expr_list(&TokenKind::RBracket))?;
                // `[v; N]` repeat literal.
                if items.len() == 1 && !trailing_comma && self.eat(&TokenKind::Semi) {
                    let len = self.parse_const_len()?;
                    self.expect(&TokenKind::RBracket, "`]`")?;
                    ExprKind::ArrayRepeat {
                        value: Box::new(items.pop()?),
                        len,
                        fill: false,
                    }
                } else {
                    self.expect(&TokenKind::RBracket, "`]`")?;
                    ExprKind::List(items)
                }
            }
            // `async { }`, `async yield { }`, and
            // the closure modifiers `async ||` / `async yield ||`.
            TokenKind::Async => {
                self.bump();
                let is_generator = self.eat(&TokenKind::Yield);
                match self.peek() {
                    TokenKind::LBrace => {
                        let b = self.ret_frame(None, Self::parse_block).0?;
                        if is_generator {
                            ExprKind::AsyncYieldBlock(b)
                        } else {
                            ExprKind::AsyncBlock(b)
                        }
                    }
                    TokenKind::Pipe | TokenKind::PipePipe | TokenKind::LBracket => {
                        self.coro_closure(true, is_generator)?
                    }
                    _ => {
                        self.error(
                            Code::SYNTAX,
                            start,
                            "expected `{`, `|` or `fn` after `async`",
                        );
                        return None;
                    }
                }
            }
            TokenKind::Await => {
                // `await` is a prefix unary operator
                // (`await f() == x` is `(await f()) == x`).
                self.bump();
                let inner = self.parse_expr_bp(PREFIX_PREC)?;
                ExprKind::Await(Box::new(inner))
            }
            // `yield {` is a generator block, `yield |` a
            // generator closure, anything else the `yield` statement.
            TokenKind::Yield if matches!(self.peek_next(), Some(TokenKind::LBrace)) => {
                self.bump();
                ExprKind::YieldBlock(self.ret_frame(None, Self::parse_block).0?)
            }
            TokenKind::Yield
                if matches!(
                    self.peek_next(),
                    Some(TokenKind::Pipe | TokenKind::PipePipe)
                ) || (matches!(self.peek_next(), Some(TokenKind::LBracket))
                    && matches!(
                        self.tokens.get(self.pos + 2).map(|t| &t.kind),
                        Some(TokenKind::Ref)
                    )) =>
            {
                self.bump();
                self.coro_closure(false, true)?
            }
            // A divergent jump in expression position: `match return 0 {}`,
            // `f(return 1)`.
            TokenKind::Return | TokenKind::Break | TokenKind::Continue | TokenKind::Throw => {
                return self.parse_arm_body();
            }
            TokenKind::Yield => {
                self.bump();
                let value = if matches!(
                    self.peek(),
                    TokenKind::Semi | TokenKind::RBrace | TokenKind::Comma | TokenKind::RParen
                ) {
                    None
                } else {
                    Some(Box::new(self.parse_expr_bp(0)?))
                };
                ExprKind::Yield(value)
            }
            other => {
                let msg = format!("expected expression, found {}", other.describe());
                let help = match other {
                    TokenKind::HashBangLBracket => Some(
                        "an inner attribute `#![..]` goes at the top of a file; put an outer \
                         `#[..]` on the function instead",
                    ),
                    TokenKind::Pub => Some(
                        "an item or `use` in a function body is visible only in its block; \
                         remove `pub`",
                    ),
                    TokenKind::ColonColon => Some(
                        "a path starts with a name: write `pkg::a::b` for an item of this \
                         package, from its root, or `::std::a` for the standard library's",
                    ),
                    TokenKind::PipeGt => Some(
                        "`|>` continues the expression before it (`xs |> sum`); remove the \
                         `;` that ends that expression",
                    ),
                    TokenKind::At => Some(
                        "`@` tokens belong to meta templates; the shape tokens (`@Self`, \
                         `@field`, `@variant`) only to `@derive` / `@attribute` blocks",
                    ),
                    _ => None,
                };
                match help {
                    Some(h) => self.error_help(Code::SYNTAX, start, msg, h),
                    None => self.error(Code::SYNTAX, start, msg),
                }
                return None;
            }
        };
        let expr = Expr {
            kind,
            span: self.to_prev(start),
        };
        self.postfix_chain(expr, stmt_head)
    }

    /// Postfix chain: calls, members, indexing, `?`, `?.`. After a
    /// `?.`, member / index links join its tail.
    #[allow(clippy::cognitive_complexity)] // one arm per postfix form
    pub(crate) fn postfix_chain(&mut self, mut expr: Expr, stmt_head: bool) -> Option<Expr> {
        let mut chain_open = false;
        loop {
            expr.span = self.to_prev(expr.span);
            if chain_open {
                match fold_chain(expr) {
                    Ok(e) => expr = e,
                    Err(e) => {
                        expr = e;
                        chain_open = false;
                    }
                }
            }
            self.chain_link()?;
            match self.peek() {
                // A statement's leading `unsafe { .. }` is an operand
                // (`unsafe { f() } != 0`) but is never called or indexed:
                // a `(` / `[` starts the next statement, as after `if`.
                TokenKind::LParen | TokenKind::LBracket
                    if (stmt_head
                        || matches!(expr.kind, ExprKind::Try { .. })
                        || matches!(expr.kind, ExprKind::Unsafe(_))
                            && self.stmt_at == Some(expr.span.start))
                        && is_block_like(&expr) =>
                {
                    break;
                }
                TokenKind::LParen => {
                    self.bump();
                    let args = self.parse_call_args()?;
                    self.expect(&TokenKind::RParen, "`)`")?;
                    // Trailing closure: a `{` or `|..|` right
                    // after `)` is the last argument.
                    let trailing_closure = self.try_parse_trailing_closure(path_name(&expr))?;
                    expr = Expr {
                        span: expr.span,
                        kind: ExprKind::Call {
                            callee: Box::new(expr),
                            args,
                            trailing_closure,
                        },
                    };
                }
                TokenKind::Dot if self.peek_next() == Some(TokenKind::LParen) => {
                    // `f.(xs)`: function broadcast.
                    self.bump();
                    self.bump();
                    let args = self.parse_call_args()?;
                    self.expect(&TokenKind::RParen, "`)`")?;
                    expr = Expr {
                        span: expr.span,
                        kind: ExprKind::Broadcast {
                            callee: Box::new(expr),
                            args,
                        },
                    };
                }
                TokenKind::Dot => {
                    self.bump();
                    let origin = self.tokens.get(self.pos).and_then(Token::origin_module);
                    let name = match self.peek().clone() {
                        _ if self.peek_name().is_some() => {
                            let n = self.peek_name().unwrap_or_default();
                            self.bump_name();
                            n
                        }
                        TokenKind::IntLit(s) | TokenKind::FloatLit(s) => {
                            // Tuple field: `expr.0`, `expr.1`, ... A float
                            // token `0.1` in `t.0.1` is two accesses.
                            let span = self.peek_span();
                            self.bump();
                            for part in s.split('.') {
                                let idx = self.tuple_index(part, span)?;
                                expr = Expr {
                                    span: expr.span,
                                    kind: ExprKind::TupleField(Box::new(expr), idx),
                                };
                            }
                            continue;
                        }
                        _ => {
                            let s = self.peek_span();
                            self.error(
                                Code::SYNTAX,
                                s,
                                format!(
                                    "expected field or method name after `.`, found {}",
                                    self.peek().describe()
                                ),
                            );
                            return None;
                        }
                    };
                    // `x.m::<T>(..)`: explicit generic arguments.
                    let mut type_args = Vec::new();
                    if matches!(self.peek(), TokenKind::ColonColon)
                        && self.peek_next() == Some(TokenKind::Lt)
                    {
                        self.bump();
                        self.bump();
                        type_args = self.nested_type_list(TokenKind::Gt)?;
                        if !self.expect_closing_gt() {
                            return None;
                        }
                        if !matches!(self.peek(), TokenKind::LParen) {
                            let s = self.peek_span();
                            self.error(Code::SYNTAX, s, "expected `(` after a method's `::<..>`");
                            return None;
                        }
                    }
                    if matches!(self.peek(), TokenKind::LParen) {
                        self.bump();
                        let args = self.parse_call_args()?;
                        self.expect(&TokenKind::RParen, "`)`")?;
                        // Trailing closure on a method call, as
                        // for calls.
                        let trailing_closure =
                            self.try_parse_trailing_closure(Some(name.clone()))?;
                        expr = Expr {
                            span: expr.span,
                            kind: ExprKind::MethodCall {
                                receiver: Box::new(expr),
                                method: name,
                                type_args,
                                args,
                                trailing_closure,
                                origin,
                                written: true,
                            },
                        };
                    } else if let Some(closure) = self
                        .paren_less_trailing_closure(Some(name.clone()))
                        .or_else(|| self.paren_less_block_closure(Some(name.clone())))
                    {
                        // `xs.map |x| { .. }`, `s.spawn { .. }`.
                        expr = Expr {
                            span: expr.span,
                            kind: ExprKind::MethodCall {
                                receiver: Box::new(expr),
                                method: name,
                                type_args,
                                args: Vec::new(),
                                trailing_closure: Some(closure),
                                origin,
                                written: true,
                            },
                        };
                    } else {
                        expr = Expr {
                            span: expr.span,
                            kind: ExprKind::Field(Box::new(expr), name),
                        };
                    }
                }
                TokenKind::LBracket => {
                    self.bump();
                    let (mut indices, default) =
                        self.with_struct_expr(true, Self::parse_index_parts)?;
                    if indices.is_empty() {
                        let s = self.peek_span();
                        self.error(Code::SYNTAX, s, "expected index expression");
                    }
                    self.expect(&TokenKind::RBracket, "`]`")?;
                    // A lone slice `a[start? : end? (: step?)?]`.
                    if indices.len() == 1 && default.is_none() {
                        if let ExprKind::AxisSlice { .. } = indices[0].kind {
                            let Some(Expr {
                                kind: ExprKind::AxisSlice { start, end, step },
                                ..
                            }) = indices.pop()
                            else {
                                unreachable!()
                            };
                            expr = Expr {
                                span: expr.span,
                                kind: ExprKind::Slice {
                                    target: Box::new(expr),
                                    start,
                                    end,
                                    step,
                                },
                            };
                            continue;
                        }
                    }
                    // `m[i, j]` is `m[(i, j)]` (`Index<(I, J)>`).
                    let span = expr.span;
                    let idx = if indices.len() == 1 {
                        indices.into_iter().next()?
                    } else {
                        let at = indices.first().map_or(span, |e| e.span);
                        Expr {
                            span: at,
                            kind: ExprKind::Tuple(indices),
                        }
                    };
                    let idx = match default {
                        Some(d) => nabic_ast::index_default(idx, d),
                        None => idx,
                    };
                    expr = Expr {
                        span,
                        kind: ExprKind::Index(Box::new(expr), Box::new(idx)),
                    };
                }
                TokenKind::Question => {
                    self.bump();
                    expr = Expr {
                        span: expr.span,
                        kind: ExprKind::TryQ(Box::new(expr)),
                    };
                }
                // A registered postfix extension token (`x°`): the postfix row,
                // with `?` and member access.
                TokenKind::ExtOp(sym)
                    if self
                        .ext
                        .levels
                        .contains_key(&(sym.clone(), Fixity::Postfix)) =>
                {
                    let op = sym.clone();
                    self.bump();
                    expr = Expr {
                        span: expr.span,
                        kind: ExprKind::OpPostfix {
                            op,
                            operand: Box::new(expr),
                        },
                    };
                }
                TokenKind::QuestionDot => {
                    self.bump();
                    let name = match self.peek().clone() {
                        _ if self.peek_name().is_some() => {
                            let n = self.peek_name().unwrap_or_default();
                            self.bump_name();
                            n
                        }
                        TokenKind::IntLit(s) => {
                            let span = self.peek_span();
                            self.bump();
                            let idx = self.tuple_index(&s, span)?;
                            expr = Expr {
                                span: expr.span,
                                kind: ExprKind::QuestionDot(
                                    Box::new(expr),
                                    vec![AccessTail::TupleField(idx)],
                                ),
                            };
                            chain_open = true;
                            continue;
                        }
                        // No `x?.[i]`.
                        TokenKind::LBracket => {
                            let s = self.peek_span();
                            self.error_help(
                                Code::SYNTAX,
                                s,
                                "`?.` is not followed by `[`: there is no `x?.[i]`",
                                "write `x?.get(i)`",
                            );
                            return None;
                        }
                        _ => {
                            let s = self.peek_span();
                            self.error(
                                Code::SYNTAX,
                                s,
                                format!(
                                    "expected field or method name after `?.`, found {}",
                                    self.peek().describe()
                                ),
                            );
                            return None;
                        }
                    };
                    let tail = if matches!(self.peek(), TokenKind::LParen) {
                        self.bump();
                        let args = self.parse_call_args()?;
                        self.expect(&TokenKind::RParen, "`)`")?;
                        AccessTail::Method {
                            method: name,
                            type_args: Vec::new(),
                            args,
                        }
                    } else {
                        AccessTail::Field(name)
                    };
                    expr = Expr {
                        span: expr.span,
                        kind: ExprKind::QuestionDot(Box::new(expr), vec![tail]),
                    };
                    chain_open = true;
                }
                _ => break,
            }
        }
        if chain_open {
            expr = fold_chain(expr).unwrap_or_else(|e| e);
        }
        expr.span = self.to_prev(expr.span);
        Some(expr)
    }

    /// Tuple index after `.`: plain decimal only (`t.1u8`, `t.0x1` are
    /// errors).
    pub(crate) fn tuple_index(&mut self, text: &str, span: Span) -> Option<u32> {
        let plain = !text.is_empty()
            && text.bytes().all(|b| b.is_ascii_digit())
            && (text == "0" || !text.starts_with('0'));
        match text.parse::<u32>() {
            Ok(i) if plain => Some(i),
            _ => {
                self.error(
                    Code::SYNTAX,
                    span,
                    format!("invalid tuple field index `{text}`"),
                );
                None
            }
        }
    }

    /// `{ k: v, .. }` is a dict, `{ a, b, .. }` a set (two
    /// items or a trailing comma), anything else a block. Decided by a
    /// speculative parse of the first expression.
    pub(crate) fn try_set_or_dict(&mut self) -> Option<ExprKind> {
        let (pos, pending, n_diags) = (self.pos, self.pending.clone(), self.diagnostics.len());
        let n_lifted = self.lifted.len();
        let rewind = |p: &mut Self| {
            p.pos = pos;
            p.pending.clone_from(&pending);
            p.diagnostics.truncate(n_diags);
            p.lifted.truncate(n_lifted);
        };
        self.bump(); // `{`
        // Try only a simple first item, so nested blocks aren't re-parsed
        // exponentially; one starting with a keyword is a block.
        let first = if matches!(
            self.peek(),
            TokenKind::IntLit(_)
                | TokenKind::FloatLit(_)
                | TokenKind::CharLit(_)
                | TokenKind::StrLit(_)
                | TokenKind::RawStrLit(_)
                | TokenKind::Ident(_)
                | TokenKind::True
                | TokenKind::False
                | TokenKind::Minus
                | TokenKind::LParen
                | TokenKind::LBracket
        ) {
            self.with_struct_expr(true, Self::parse_expr)
        } else {
            None
        };
        let kind = match (first, self.peek()) {
            (Some(k), TokenKind::Colon) => {
                self.bump();
                let mut pairs = Vec::new();
                let mut key = k;
                loop {
                    let v = self.with_struct_expr(true, Self::parse_expr)?;
                    pairs.push((key, v));
                    if !self.eat(&TokenKind::Comma) || matches!(self.peek(), TokenKind::RBrace) {
                        break;
                    }
                    key = self.with_struct_expr(true, Self::parse_expr)?;
                    self.expect(&TokenKind::Colon, "`:` in a dict literal")?;
                }
                ExprKind::Dict(pairs)
            }
            (Some(first), TokenKind::Comma) => {
                self.bump();
                let (mut rest, _) =
                    self.with_struct_expr(true, |p| p.expr_list(&TokenKind::RBrace))?;
                rest.insert(0, first);
                ExprKind::Set(rest)
            }
            _ => {
                rewind(self);
                return None;
            }
        };
        self.expect(&TokenKind::RBrace, "`}`")?;
        Some(kind)
    }

    /// After `[`, up to (not consuming) `]`: the comma-separated parts of
    /// an index, each an expression or a slice `start? : end? (: step?)?`
    /// (`AxisSlice`), and the `v` of a last part `default: v`.
    pub(crate) fn parse_index_parts(&mut self) -> Option<(Vec<Expr>, Option<Expr>)> {
        let mut parts = Vec::new();
        while !matches!(self.peek(), TokenKind::RBracket | TokenKind::Eof) {
            let part = self.parse_axis_part()?;
            // `d[k, default: v]`: `k`'s value, `v` when absent.
            if let ExprKind::AxisSlice {
                start: Some(s),
                end: Some(v),
                step: None,
            } = &part.kind
            {
                let named_default = matches!(&s.kind, ExprKind::Path(p) if p.segments.len() == 1
                    && p.segments[0].name == "default"
                    && p.segments[0].generics.is_empty());
                if named_default && !parts.is_empty() {
                    return Some((parts, Some((**v).clone())));
                }
            }
            parts.push(part);
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        Some((parts, None))
    }

    /// One part of an index: an expression, or `start? : end? (: step?)?`.
    pub(crate) fn parse_axis_part(&mut self) -> Option<Expr> {
        let span = self.peek_span();
        let colon = |p: &Self| matches!(p.peek(), TokenKind::Colon | TokenKind::ColonColon);
        let start = if colon(self) {
            None
        } else {
            let e = self.parse_expr()?;
            if !colon(self) {
                return Some(e);
            }
            Some(Box::new(e))
        };
        let opt = |p: &mut Self| {
            if matches!(
                p.peek(),
                TokenKind::Colon | TokenKind::RBracket | TokenKind::Comma
            ) {
                Some(None)
            } else {
                p.parse_expr().map(|e| Some(Box::new(e)))
            }
        };
        // `a::k` lexes `::` as one token: no end, then the step.
        let (end, has_step) = if self.eat(&TokenKind::ColonColon) {
            (None, true)
        } else {
            self.bump(); // `:`
            let end = opt(self)?;
            (end, self.eat(&TokenKind::Colon))
        };
        let step = if has_step { opt(self)? } else { None };
        Some(Expr {
            span: self.to_prev(span),
            kind: ExprKind::AxisSlice { start, end, step },
        })
    }

    /// `e, e, ...` up to (not consuming) `close`, and whether a trailing
    /// comma ended it.
    pub(crate) fn expr_list(&mut self, close: &TokenKind) -> Option<(Vec<Expr>, bool)> {
        let mut items = Vec::new();
        let mut trailing_comma = false;
        while std::mem::discriminant(self.peek()) != std::mem::discriminant(close)
            && !matches!(self.peek(), TokenKind::Eof)
        {
            if *close == TokenKind::RBracket
                && matches!(self.peek(), TokenKind::Colon | TokenKind::ColonColon)
            {
                break;
            }
            items.push(self.parse_expr()?);
            trailing_comma = self.eat(&TokenKind::Comma);
            if !trailing_comma {
                break;
            }
        }
        Some((items, trailing_comma))
    }
}

impl Parser<'_> {
    /// `Path<...>` in an expression: a balanced `>` followed by `{` or `(`.
    /// Stops at tokens no generic list holds, so comparisons fail fast.
    pub(crate) fn peek_struct_or_call_after_generics(&self) -> bool {
        // Caller has already confirmed `peek() == Lt`.
        let mut depth: i32 = 0;
        let mut i = self.pos;
        let max = i.saturating_add(64).min(self.tokens.len());
        while i < max {
            let kind = match self.tokens.get(i) {
                Some(t) => &t.kind,
                None => return false,
            };
            match kind {
                TokenKind::Lt => depth += 1,
                TokenKind::Shl => depth += 2,
                TokenKind::Gt => {
                    depth -= 1;
                    if depth == 0 {
                        let next = self.tokens.get(i + 1).map(|t| &t.kind);
                        return matches!(next, Some(TokenKind::LBrace | TokenKind::LParen));
                    }
                }
                TokenKind::Shr => {
                    depth -= 2;
                    if depth <= 0 {
                        let next = self.tokens.get(i + 1).map(|t| &t.kind);
                        return matches!(next, Some(TokenKind::LBrace | TokenKind::LParen));
                    }
                }
                // Tokens that may appear inside a generic-arg type list.
                TokenKind::Ident(_)
                | TokenKind::Comma
                | TokenKind::ColonColon
                | TokenKind::Amp
                | TokenKind::Star
                | TokenKind::LBracket
                | TokenKind::RBracket
                | TokenKind::LParen
                | TokenKind::RParen
                | TokenKind::Semi
                | TokenKind::IntLit(_) => {}
                _ => return false,
            }
            i += 1;
        }
        false
    }

    /// `Ident (:: Ident)* {` lookahead (no tokens consumed).
    pub(crate) fn peek_path_then_lbrace(&self) -> bool {
        let mut i = self.pos;
        loop {
            if !matches!(
                self.tokens.get(i).map(|t| &t.kind),
                Some(TokenKind::Ident(_))
            ) {
                return false;
            }
            match self.tokens.get(i + 1).map(|t| &t.kind) {
                Some(TokenKind::ColonColon) => i += 2,
                Some(TokenKind::LBrace) => return true,
                _ => return false,
            }
        }
    }

    /// `Path { .. }`: struct literal (`{}`, `{ .. }`, `{ x: `, `{ x, `,
    /// `{ x }`) or 0-arg trailing closure? Peeks two tokens.
    pub(crate) fn peek_struct_init_body(&self) -> bool {
        // Caller has just confirmed `peek() == LBrace`.
        self.struct_init_body_at(self.pos)
    }

    /// `peek_struct_init_body` for the `{` at token `brace`.
    pub(crate) fn struct_init_body_at(&self, brace: usize) -> bool {
        // A first field's `#[cfg]` is skipped.
        let brace = self.past_attrs(brace + 1) - 1;
        let after_brace = self.tokens.get(brace + 1).map(|t| &t.kind);
        match after_brace {
            Some(TokenKind::RBrace | TokenKind::DotDot) => true,
            Some(TokenKind::Ident(_)) => {
                let third = self.tokens.get(brace + 2).map(|t| &t.kind);
                matches!(
                    third,
                    Some(TokenKind::Colon | TokenKind::Comma | TokenKind::RBrace)
                )
            }
            // A keyword field name, `Ev { match: 1 }`.
            Some(k) if keyword_str(k).is_some_and(|w| w != "self") => matches!(
                self.tokens.get(brace + 2).map(|t| &t.kind),
                Some(TokenKind::Colon)
            ),
            _ => false,
        }
    }

    /// `{ field: expr, .., ..base? }` after a path, from the `{`.
    pub(crate) fn parse_struct_init_body(&mut self, path: Path) -> Option<ExprKind> {
        self.expect(&TokenKind::LBrace, "`{`")?;
        let mut fields = Vec::new();
        let mut spread: Option<Box<Expr>> = None;
        while !matches!(self.peek(), TokenKind::RBrace | TokenKind::Eof) {
            if self.eat(&TokenKind::DotDot) {
                let base = self.with_struct_expr(true, Self::parse_expr)?;
                spread = Some(Box::new(base));
                break;
            }
            let attrs = self.cfg_only_attrs("a struct literal field");
            let marks = (self.pending_stmts.len(), self.lifted.len());
            let name = if let Some(n) = self.peek_name() {
                self.bump_name();
                n
            } else {
                let s = self.peek_span();
                self.error(Code::SYNTAX, s, "expected field name");
                return None;
            };
            // Shorthand `Foo { x }` (using a local named `x`) is allowed
            // when the next token is not `:`.
            let value = if self.eat(&TokenKind::Colon) {
                self.with_struct_expr(true, Self::parse_expr)?
            } else {
                Expr {
                    span: self.peek_span(),
                    kind: ExprKind::Path(Path {
                        segments: vec![PathSegment {
                            name: name.clone(),
                            generics: Vec::new(),
                        }],
                        span: self.peek_span(),
                        origin: None,
                    }),
                }
            };
            if !self.cfg_drops(&attrs, marks) {
                fields.push((name, value));
            }
            if !self.eat(&TokenKind::Comma) {
                // `S { a: 1 b: 2 }`: a field follows without its `,`.
                if let (TokenKind::Ident(next), Some(TokenKind::Colon)) =
                    (self.peek().clone(), self.peek_next())
                {
                    let at = self.peek_span();
                    let last = fields
                        .last()
                        .map_or_else(String::new, |(n, v): &(String, Expr)| {
                            format!("{n}: {}, ", self.text_in(v.span))
                        });
                    self.error_help(
                        Code::SYNTAX,
                        at,
                        format!("expected `,` or `}}`, found field `{next}`"),
                        format!("separate the fields with `,`: `{last}{next}: ..`"),
                    );
                    return None;
                }
                break;
            }
        }
        self.expect(&TokenKind::RBrace, "`}`")?;
        Some(ExprKind::StructInit {
            path,
            fields,
            spread,
        })
    }

    /// If a literal pattern is followed by `..` or `..=`, extend it into
    /// a range pattern. The end is optional: `0..` is open-ended.
    pub(crate) fn maybe_extend_range(&mut self, span: Span, start: Literal) -> Pattern {
        let inclusive = match self.peek() {
            TokenKind::DotDotEq => true,
            TokenKind::DotDot => false,
            _ => {
                return Pattern {
                    kind: PatternKind::Literal(start),
                    span,
                };
            }
        };
        self.bump();
        let end = match self.peek().clone() {
            TokenKind::IntLit(s) => {
                self.bump();
                Some(Literal::Int(s))
            }
            TokenKind::FloatLit(s) => {
                self.bump();
                Some(Literal::Float(s))
            }
            TokenKind::CharLit(s) => {
                self.bump();
                Some(Literal::Char(s))
            }
            TokenKind::CCharLit(s) => {
                self.bump();
                Some(Literal::CChar(s))
            }
            TokenKind::ByteCharLit(s) => {
                self.bump();
                Some(byte_lit(&s))
            }
            TokenKind::Minus => {
                self.bump();
                match self.peek().clone() {
                    TokenKind::IntLit(s) => {
                        self.bump();
                        Some(Literal::Int(format!("-{s}")))
                    }
                    TokenKind::FloatLit(s) => {
                        self.bump();
                        Some(Literal::Float(format!("-{s}")))
                    }
                    _ => None,
                }
            }
            _ => None,
        };
        Pattern {
            kind: PatternKind::Range {
                start: Some(start),
                end,
                inclusive,
            },
            span,
        }
    }
}
