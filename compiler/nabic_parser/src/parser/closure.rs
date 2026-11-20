//! Closures and trailing closures.

#[allow(clippy::wildcard_imports)]
use crate::parser::*;

impl Parser<'_> {
    /// Trailing closure: `|..|` or `{ .. }` right after a
    /// call's `)`. `||` there is logical-or (0-arg is `{ .. }`), and `{`
    /// is off in a control-flow head (`struct_expr_allowed`).
    #[allow(clippy::option_option)] // outer None = parse error (for `?`), inner None = no closure
    pub(crate) fn try_parse_trailing_closure(
        &mut self,
        label: Option<String>,
    ) -> Option<Option<Box<Expr>>> {
        match self.peek() {
            TokenKind::LBrace if self.struct_expr_allowed => {
                let span = self.peek_span();
                let (block, bare) = self.ret_frame(label, Self::parse_block);
                let block = block?;
                self.note_closure_returns(span, bare);
                let body = Expr {
                    span,
                    kind: ExprKind::Block(block),
                };
                let closure = Expr {
                    span,
                    kind: ExprKind::Closure {
                        captures: Vec::new(),
                        params: Vec::new(),
                        ret_ty: None,
                        body: Box::new(body),
                        is_async: false,
                        is_generator: false,
                    },
                };
                Some(Some(Box::new(closure)))
            }
            TokenKind::Pipe if self.at_trailing_closure_shape() => {
                let span = self.peek_span();
                let kind = self.closure_expr(label, span)?;
                Some(Some(Box::new(Expr { span, kind })))
            }
            _ => Some(None),
        }
    }

    /// At `|`: `|params|`, an optional `-> R`, then `{`; anything else
    /// after a call is binary `|` (`f(1) | g(2)`).
    fn at_trailing_closure_shape(&self) -> bool {
        let tok = |i: usize| self.tokens.get(self.pos + i).map(|t| &t.kind);
        let mut depth = 0i32;
        let mut i = 1;
        // The parameter list, up to its closing `|`.
        loop {
            match tok(i) {
                None
                | Some(TokenKind::Eof | TokenKind::Semi | TokenKind::LBrace | TokenKind::RBrace) => {
                    return false;
                }
                Some(TokenKind::LParen | TokenKind::LBracket) => depth += 1,
                Some(TokenKind::RParen | TokenKind::RBracket) => {
                    depth -= 1;
                    if depth < 0 {
                        return false;
                    }
                }
                Some(TokenKind::Pipe) if depth == 0 => break,
                _ => {}
            }
            i += 1;
        }
        i += 1;
        match tok(i) {
            Some(TokenKind::LBrace) => true,
            Some(TokenKind::Arrow) => {
                // The return type, up to the body's `{`.
                loop {
                    i += 1;
                    match tok(i) {
                        None | Some(TokenKind::Eof | TokenKind::Semi | TokenKind::RBrace) => {
                            return false;
                        }
                        Some(TokenKind::LBrace) => return true,
                        _ => {}
                    }
                }
            }
            _ => false,
        }
    }

    /// At `[`: a capture list, `[ref x ..`, `[move x ..` or `[x, y] |..|`.
    pub(crate) fn at_capture_list(&self) -> bool {
        let tok = |i: usize| self.tokens.get(self.pos + i).map(|t| &t.kind);
        let mut i = 1;
        loop {
            if matches!(tok(i), Some(TokenKind::Ref)) || self.at_move(self.pos + i) {
                return true;
            }
            if !matches!(tok(i), Some(TokenKind::Ident(_) | TokenKind::SelfLower)) {
                return false;
            }
            i += 1;
            match tok(i) {
                Some(TokenKind::Comma) if matches!(tok(i + 1), Some(TokenKind::RBracket)) => {
                    i += 1;
                    break;
                }
                Some(TokenKind::Comma) => i += 1,
                Some(TokenKind::RBracket) => break,
                _ => return false,
            }
        }
        matches!(tok(i + 1), Some(TokenKind::Pipe | TokenKind::PipePipe))
    }

    /// Token `at` is `move` (not `r#move`) before a capture name.
    pub(crate) fn at_move(&self, at: usize) -> bool {
        let t = |i: usize| self.tokens.get(i);
        t(at).is_some_and(|t| !t.raw && matches!(&t.kind, TokenKind::Ident(n) if n == "move"))
            && t(at + 1)
                .is_some_and(|t| matches!(t.kind, TokenKind::Ident(_) | TokenKind::SelfLower))
    }

    /// `[ref x, move y, z] |..| body` - a closure with a capture list.
    pub(crate) fn parse_capture_closure(&mut self) -> Option<ExprKind> {
        self.bump(); // `[`
        let mut captures: Vec<nabic_ast::CaptureSpec> = Vec::new();
        while !matches!(self.peek(), TokenKind::RBracket | TokenKind::Eof) {
            if self.eat(&TokenKind::Ref) {
                let Some(name) = self.capture_name() else {
                    let s = self.peek_span();
                    self.error(
                        Code::SYNTAX,
                        s,
                        format!(
                            "expected capture name after `ref`, found {}",
                            self.peek().describe()
                        ),
                    );
                    return None;
                };
                captures.push(nabic_ast::CaptureSpec::Ref(name));
            } else if self.at_move(self.pos) {
                // `move` is reserved as a name: skip `bump`'s check.
                self.pos += 1;
                let name = self.capture_name()?;
                captures.push(nabic_ast::CaptureSpec::Move(name));
            } else if let Some(n) = self.capture_name() {
                captures.push(nabic_ast::CaptureSpec::Value(n));
            } else {
                let s = self.peek_span();
                self.error(Code::SYNTAX, s, "capture-list item expected");
                return None;
            }
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        self.expect(&TokenKind::RBracket, "`]`")?;
        if !matches!(self.peek(), TokenKind::Pipe | TokenKind::PipePipe) {
            let s = self.peek_span();
            self.error(Code::SYNTAX, s, "`|` or `||` expected after capture list");
            return None;
        }
        let mut closure_kind = self.parse_closure_expr()?;
        if let ExprKind::Closure { captures: c, .. } = &mut closure_kind {
            *c = captures;
        }
        Some(closure_kind)
    }

    /// A capture-list name: a variable, or a method's `self`.
    pub(crate) fn capture_name(&mut self) -> Option<String> {
        let name = match self.peek() {
            TokenKind::Ident(n) => n.clone(),
            TokenKind::SelfLower => "self".to_owned(),
            _ => return None,
        };
        self.bump();
        Some(name)
    }

    /// `async ||` / `yield ||` / `async yield ||` closure.
    pub(crate) fn coro_closure(&mut self, is_async: bool, is_generator: bool) -> Option<ExprKind> {
        let mut k = if matches!(self.peek(), TokenKind::LBracket) {
            self.parse_capture_closure()?
        } else {
            self.parse_closure_expr()?
        };
        if let ExprKind::Closure {
            is_async: a,
            is_generator: g,
            ..
        } = &mut k
        {
            *a = is_async;
            *g = is_generator;
        }
        Some(k)
    }

    /// `|args| body`, `|args| -> R body` or `|| body`.
    pub(crate) fn parse_closure_expr(&mut self) -> Option<ExprKind> {
        let span = self.peek_span();
        self.closure_expr(None, span)
    }

    /// `parse_closure_expr` for a trailing closure of a call to `label`
    /// (`span`: the closure's).
    pub(crate) fn closure_expr(&mut self, label: Option<String>, span: Span) -> Option<ExprKind> {
        let mark = self.param_pats.len();
        let (params, ret_ty) = self.parse_closure_head()?;
        let pats: Vec<(String, Pattern)> = self.param_pats.drain(mark..).collect();
        let body_at = self.peek_span();
        let (body, bare) = self.ret_frame(label, Self::parse_expr);
        let body = self.with_param_lets(body?, pats);
        // `|v| v += i`: an assignment is a statement, so it needs a block.
        if !matches!(body.kind, ExprKind::Block(_))
            && (matches!(self.peek(), TokenKind::Eq) || compound_assign_op(self.peek()).is_some())
        {
            let op_at = self.peek_span();
            let op = compound_assign_op(self.peek());
            let target = self.text_in(self.to_prev(body_at));
            self.bump();
            let rhs_at = self.peek_span();
            self.parse_expr()?;
            let rhs = self.text_in(self.to_prev(rhs_at));
            let head = self.text_in(Span {
                end: body_at.start,
                ..span
            });
            let stmt = self.text_in(self.to_prev(body_at));
            let mut help = format!("put the body in a block: `{head} {{ {stmt}; }}`");
            // A parameter is the closure's own copy: the write ends with it.
            if params.iter().any(|p| p.name == target) {
                let value = op.map_or_else(|| rhs.clone(), |o| format!("{target} {o} {rhs}"));
                help = format!(
                    "{help}; `{target}` is the closure's own copy, so to hand back the new \
                     value return it: `{head} {value}`"
                );
            }
            self.error_help(
                Code::SYNTAX,
                op_at,
                "a closure body without braces is an expression, and an assignment is a \
                 statement",
                help,
            );
        }
        // `-> T` only before a block body.
        if ret_ty.is_some() && !matches!(&body.kind, ExprKind::Block(b) if b.label.is_none()) {
            self.error_help(
                Code::SYNTAX,
                body_at,
                "a closure with `-> T` needs a block body",
                "wrap the body in braces: `|x: i32| -> i32 { x * 2 }`",
            );
        }
        self.note_closure_returns(span, bare);
        Some(ExprKind::Closure {
            captures: Vec::new(),
            params,
            ret_ty,
            body: Box::new(body),
            is_async: false,
            is_generator: false,
        })
    }

    /// `s.spawn { body }`: a `{` after a method name, outside a condition
    /// head.
    pub(crate) fn paren_less_block_closure(&mut self, label: Option<String>) -> Option<Box<Expr>> {
        if !self.struct_expr_allowed || !matches!(self.peek(), TokenKind::LBrace) {
            return None;
        }
        self.try_parse_trailing_closure(label).flatten()
    }

    /// `xs.map |x| { .. }`: a `|` after a callee is a closure only when
    /// `|params| (-> R)? {` follows; `||` stays logical-or. Off in a
    /// control-flow head.
    pub(crate) fn paren_less_trailing_closure(
        &mut self,
        label: Option<String>,
    ) -> Option<Box<Expr>> {
        if !self.struct_expr_allowed || !matches!(self.peek(), TokenKind::Pipe) {
            return None;
        }
        let not_closure = self.backtracks(|p| {
            p.parse_closure_head()?;
            matches!(p.peek(), TokenKind::LBrace).then_some(())
        });
        if not_closure {
            return None;
        }
        let span = self.peek_span();
        let mark = self.param_pats.len();
        let (params, ret_ty) = self.parse_closure_head()?;
        let pats: Vec<(String, Pattern)> = self.param_pats.drain(mark..).collect();
        let body_span = self.peek_span();
        let (block, bare) = self.ret_frame(label, Self::parse_block);
        let mut block = block?;
        block.stmts.splice(0..0, self.param_lets(pats));
        self.note_closure_returns(span, bare);
        Some(Box::new(Expr {
            span,
            kind: ExprKind::Closure {
                captures: Vec::new(),
                params,
                ret_ty,
                body: Box::new(Expr {
                    span: body_span,
                    kind: ExprKind::Block(block),
                }),
                is_async: false,
                is_generator: false,
            },
        }))
    }

    /// `|params|` / `||`, then an optional `-> R`.
    pub(crate) fn parse_closure_head(&mut self) -> Option<(Vec<Param>, Option<Ty>)> {
        let mut params: Vec<Param> = Vec::new();
        if self.eat(&TokenKind::PipePipe) {
            // Empty param list `||`.
        } else {
            self.expect(&TokenKind::Pipe, "`|`")?;
            params = self.with_type_sum(false, |p| {
                let mut params = Vec::new();
                while !matches!(p.peek(), TokenKind::Pipe | TokenKind::Eof) {
                    let param = p.parse_param()?;
                    if let Some(
                        t @ Ty {
                            kind: TyKind::Some(_),
                            ..
                        },
                    ) = &param.ty
                    {
                        p.error_help(
                            Code::SYNTAX,
                            t.span,
                            "`some Trait` is only a function's parameter or return type, or the whole right side of a `type` alias",
                            "a closure parameter has one type: write it, or leave it out for \
                             the call to give it",
                        );
                    }
                    params.push(param);
                    if !p.eat(&TokenKind::Comma) {
                        break;
                    }
                }
                Some(params)
            })?;
            self.expect(&TokenKind::Pipe, "`|`")?;
        }
        let ret_ty = if self.eat(&TokenKind::Arrow) {
            Some(self.parse_type()?)
        } else {
            None
        };
        Some((params, ret_ty))
    }
}
