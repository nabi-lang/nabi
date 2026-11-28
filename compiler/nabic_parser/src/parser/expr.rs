//! Expressions: Pratt loop, operator chains, returns, call arguments.

#[allow(clippy::wildcard_imports)]
use crate::parser::*;

impl Parser<'_> {
    /// `c ? a : b` (C, JS) at the cursor, just past `c`'s `?`.
    pub(crate) fn ternary_ahead(&self) -> bool {
        if self.prev_kind() != Some(&TokenKind::Question) {
            return false;
        }
        match self.peek() {
            TokenKind::Ident(w) if !matches!(w.as_str(), "catch" | "finally" | "in" | "where") => {}
            TokenKind::IntLit(_)
            | TokenKind::FloatLit(_)
            | TokenKind::StrLit(_)
            | TokenKind::CharLit(_)
            | TokenKind::FStrStart(_)
            | TokenKind::True
            | TokenKind::False
            | TokenKind::None
            | TokenKind::LParen
            | TokenKind::LBracket => {}
            _ => return false,
        }
        let line = self.peek_span().line;
        let mut depth = 0u32;
        for t in &self.tokens[self.pos..] {
            if t.span.line != line {
                return false;
            }
            match t.kind {
                TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => depth += 1,
                TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                    let Some(d) = depth.checked_sub(1) else {
                        return false;
                    };
                    depth = d;
                }
                TokenKind::Semi | TokenKind::Eof => return false,
                TokenKind::Colon if depth == 0 => return true,
                _ => {}
            }
        }
        false
    }

    /// `c ? a : b` (no such operator) or Python's `a if c else b` (`cond`
    /// is `a`): read it whole for the message and keep `cond`.
    pub(crate) fn ternary(&mut self, cond: Expr) -> Option<Expr> {
        if self.prev_kind() != Some(&TokenKind::Question) {
            return self.python_ternary(cond);
        }
        let q = self.prev_span()?;
        let c = self.text_in(cond.span);
        let c = c.strip_suffix('?').unwrap_or(&c).trim_end().to_owned();
        let a = self.parse_expr()?;
        self.expect(&TokenKind::Colon, "`:`")?;
        let b = self.parse_expr()?;
        let (a, b) = (self.text_in(a.span), self.text_in(b.span));
        self.error_help(
            Code::FOREIGN_SYNTAX,
            q,
            "Nabi has no `? :` operator",
            format!("`if` is an expression: `if {c} {{ {a} }} else {{ {b} }}`"),
        );
        Some(cond)
    }

    /// `i++` / `i--` (C, JS) at the cursor, after `lhs`: there are no
    /// such operators. Reports and skips them.
    pub(crate) fn incr_decr(&mut self, lhs: &Expr) -> bool {
        if self.pending.is_some() {
            return false;
        }
        let tok = |i: usize| self.tokens.get(self.pos + i);
        let (Some(a), Some(b), Some(c)) = (tok(0), tok(1), tok(2)) else {
            return false;
        };
        let op = match (&a.kind, &b.kind) {
            (TokenKind::Plus, TokenKind::Plus) => "+",
            (TokenKind::Minus, TokenKind::Minus) => "-",
            _ => return false,
        };
        let ends = matches!(
            c.kind,
            TokenKind::Semi | TokenKind::RParen | TokenKind::RBrace | TokenKind::Eof
        );
        if b.span.start != a.span.end || !ends {
            return false;
        }
        let at = Span {
            end: b.span.end,
            ..a.span
        };
        let x = self.text_in(lhs.span);
        self.error_help(
            Code::FOREIGN_SYNTAX,
            at,
            format!("Nabi has no `{op}{op}` operator"),
            format!("write `{x} {op}= 1`"),
        );
        self.bump();
        self.bump();
        true
    }
}

impl Parser<'_> {
    /// Expression including pipes: `x |> f(a)` is `f(x, a)`, looser
    /// than any other operator (ranges too), left-assoc.
    pub(crate) fn parse_expr(&mut self) -> Option<Expr> {
        let lhs = self.parse_range_expr(true)?;
        if self.peek() != &TokenKind::PipeGt {
            return Some(lhs);
        }
        let piped = self.pipe_tail(lhs, None)?;
        // `catch` binds looser: `try s |> parse catch _ { 0 }`.
        self.parse_catch_tail(piped)
    }

    /// The `|> f` links after `lhs`; their right sides are ranges when
    /// `min_bp` is `None`, else operands binding at least `min_bp` (in a
    /// `try`'s, before its `catch`; a `try?` / `try!` one ends at `??`).
    pub(crate) fn pipe_tail(&mut self, mut lhs: Expr, min_bp: Option<u32>) -> Option<Expr> {
        while self.peek() == &TokenKind::PipeGt {
            let at = self.peek_span();
            self.bump();
            let rhs = match min_bp {
                None => self.parse_range_expr(false)?,
                Some(bp) => self.parse_expr_bp(bp)?,
            };
            lhs = self.pipe(lhs, rhs, at)?;
        }
        Some(lhs)
    }

    /// `lhs |> rhs`: `lhs` becomes the first argument of the call `rhs`,
    /// or the only one of the function `rhs` names.
    pub(crate) fn pipe(&mut self, lhs: Expr, rhs: Expr, at: Span) -> Option<Expr> {
        let span = Span {
            end: rhs.span.end,
            ..lhs.span
        };
        let first = CallArg::Positional(lhs);
        let kind = match rhs.kind {
            ExprKind::Call {
                callee,
                mut args,
                trailing_closure,
            } => {
                args.insert(0, first);
                ExprKind::Call {
                    callee,
                    args,
                    trailing_closure,
                }
            }
            ExprKind::MethodCall {
                receiver,
                method,
                type_args,
                mut args,
                trailing_closure,
                origin,
                written,
            } => {
                args.insert(0, first);
                ExprKind::MethodCall {
                    receiver,
                    method,
                    type_args,
                    args,
                    trailing_closure,
                    origin,
                    written,
                }
            }
            ExprKind::Path(_)
            | ExprKind::QPath(_)
            | ExprKind::Closure { .. }
            | ExprKind::Field(..) => ExprKind::Call {
                callee: Box::new(rhs),
                args: vec![first],
                trailing_closure: None,
            },
            kind => {
                let help = match &kind {
                    ExprKind::OpInfix { op, lhs: f, .. } if is_callee(f) => format!(
                        "`|>` binds loosest, so its right side is all of `.. {op} ..`; write \
                         `(x |> f) {op} ..`"
                    ),
                    _ => "pass a function or a call (`x |> f`, `x |> f(a)` for `f(x, a)`); for \
                          another expression, a closure in parentheses: `x |> (|v| v + 1)`"
                        .to_owned(),
                };
                self.error_help(
                    Code::SYNTAX,
                    at,
                    "the right side of `|>` is not a function or a call",
                    help,
                );
                return None;
            }
        };
        Some(Expr { kind, span })
    }

    /// Expression including ranges: `a..b`, `a..=b`, `..b`,
    /// `a..`, `..`. Ranges bind weakest but for `|>` and do not chain.
    /// `catch`: a `catch` / `finally` tail may follow the start.
    pub(crate) fn parse_range_expr(&mut self, catch: bool) -> Option<Expr> {
        let span = self.peek_span();
        let from = self.pos;
        let start = if matches!(self.peek(), TokenKind::DotDot | TokenKind::DotDotEq) {
            None
        } else {
            let e = self.parse_expr_bp(0)?;
            Some(Box::new(if catch { self.parse_catch_tail(e)? } else { e }))
        };
        let inclusive = match self.peek() {
            TokenKind::DotDot => false,
            TokenKind::DotDotEq => true,
            _ => return start.map(|e| *e),
        };
        let dots = self.pos;
        self.bump();
        let end = if !inclusive && !self.can_start_range_end() {
            None
        } else {
            Some(Box::new(self.parse_expr_bp(0)?))
        };
        let dot = if inclusive { "..=" } else { ".." };
        let text = |e: Option<&Expr>| e.map(nabic_ast::print::print_expr).unwrap_or_default();
        let (lo, hi) = (text(start.as_deref()), text(end.as_deref()));
        if let Some((op, a, b)) = start
            .as_deref()
            .and_then(|e| self.bare_comparison(e, from, dots))
        {
            self.error_help(
                Code::NEEDS_PARENTHESES,
                self.to_prev(span),
                format!("`{lo}{dot}{hi}` is a range of `bool`s: `{op}` binds tighter than `{dot}`"),
                format!(
                    "to compare with a range, write `{a} {op} ({b}{dot}{hi})`; for the range, \
                     write `({lo}){dot}{hi}`"
                ),
            );
        } else if let Some((op, a, b)) = end
            .as_deref()
            .and_then(|e| self.bare_comparison(e, dots + 1, self.pos))
        {
            self.error_help(
                Code::NEEDS_PARENTHESES,
                self.to_prev(span),
                format!("`{lo}{dot}{hi}` ends at a `bool`: `{op}` binds tighter than `{dot}`"),
                format!(
                    "to compare the range, write `({lo}{dot}{a}) {op} {b}`; for the range, \
                     write `{lo}{dot}({hi})`"
                ),
            );
        }
        if matches!(self.peek(), TokenKind::DotDot | TokenKind::DotDotEq) {
            let s = self.peek_span();
            self.error(
                Code::NEEDS_PARENTHESES,
                s,
                "range operators cannot be chained",
            );
            // Skip the rest of the chain, so it is not a second error.
            while matches!(self.peek(), TokenKind::DotDot | TokenKind::DotDotEq) {
                self.bump();
                if self.can_start_range_end() {
                    self.parse_expr_bp(0)?;
                }
            }
        }
        Some(Expr {
            span: self.to_prev(span),
            kind: ExprKind::Range {
                start,
                end,
                inclusive,
            },
        })
    }

    /// The operator and operands of `e`, a comparison not in parentheses
    /// (tokens `from..to`): a range operand that reads as a comparison's.
    fn bare_comparison(
        &self,
        e: &Expr,
        from: usize,
        to: usize,
    ) -> Option<(String, String, String)> {
        let ExprKind::OpInfix { op, lhs, rhs } = &e.kind else {
            return None;
        };
        if crate::prec::fixed_prec(op, crate::prec::Fixity::Infix).is_none_or(|(p, _)| p != 4) {
            return None;
        }
        let kind = |i: usize| self.tokens.get(i).map(|t| &t.kind);
        // `(a == b)..c`: one pair of parentheses around all of it.
        if to > from + 1
            && matches!(kind(from), Some(TokenKind::LParen))
            && matches!(kind(to - 1), Some(TokenKind::RParen))
        {
            let mut depth = 0i32;
            let mut whole = true;
            for i in from..to {
                match kind(i) {
                    Some(TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace) => depth += 1,
                    Some(TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace) => {
                        depth -= 1;
                        if depth == 0 && i + 1 < to {
                            whole = false;
                        }
                    }
                    _ => {}
                }
            }
            if whole {
                return None;
            }
        }
        let text = nabic_ast::print::print_expr;
        Some((op.clone(), text(lhs), text(rhs)))
    }

    /// `catch pat? { .. }` arms and `finally { .. }` around `body`;
    /// loosest binding, so `try a() + b() catch _ { 0 }` covers both.
    pub(crate) fn parse_catch_tail(&mut self, body: Expr) -> Option<Expr> {
        if !(self.at_kw("catch") || self.at_kw("finally") || self.at_except()) {
            return Some(body);
        }
        let mut catches: Vec<CatchArm> = Vec::new();
        while self.at_kw("catch") || self.at_except() {
            let span = self.peek_span();
            let except_as = if self.at_except() {
                self.python_except()
            } else {
                self.bump();
                false
            };
            let pat = if except_as {
                self.python_except_as()?
            } else if matches!(self.peek(), TokenKind::LBrace) {
                Pattern {
                    kind: PatternKind::Wildcard,
                    span,
                }
            } else {
                // `catch E { code, .. } { body }`: a struct pattern when a
                // second block follows the first.
                let struct_pat = self.braces_then_block();
                self.with_struct_expr(struct_pat, Self::parse_pattern)?
            };
            let arm_body = self.parse_block()?;
            catches.push(CatchArm {
                pat,
                body: arm_body,
            });
        }
        let finally = if self.eat_kw("finally") {
            Some(self.parse_block()?)
        } else {
            None
        };
        Some(Expr {
            span: self.to_prev(body.span),
            kind: ExprKind::Catch {
                body: Box::new(body),
                catches,
                finally,
            },
        })
    }

    /// After `..`: does an end operand follow (vs. `a..` / `..`)?
    pub(crate) fn can_start_range_end(&self) -> bool {
        match self.peek() {
            TokenKind::RParen
            | TokenKind::RBracket
            | TokenKind::RBrace
            | TokenKind::Comma
            | TokenKind::Semi
            | TokenKind::FatArrow
            | TokenKind::HoleEnd
            | TokenKind::HoleSpec
            | TokenKind::Eq
            | TokenKind::Eof => false,
            TokenKind::LBrace => self.struct_expr_allowed,
            _ => true,
        }
    }

    /// Pratt loop. `min_bp` is the minimum binding power that the next
    /// infix/postfix operator must exceed for the loop to consume it.
    pub(crate) fn parse_expr_bp(&mut self, min_bp: u32) -> Option<Expr> {
        let chain = self.chain;
        let r = self.nested(|p| p.parse_expr_bp_inner(min_bp));
        self.chain = chain;
        r
    }

    pub(crate) fn parse_expr_bp_inner(&mut self, min_bp: u32) -> Option<Expr> {
        let mut lhs = if let Some((sym, span)) = self.peek_op_symbol(Fixity::Prefix) {
            let (prec, _) = self.ext.level(&sym, Fixity::Prefix)?;
            self.bump();
            let rhs = self.parse_expr_bp(prec)?;
            Expr {
                kind: ExprKind::OpPrefix {
                    op: sym,
                    operand: Box::new(rhs),
                },
                span: self.to_prev(span),
            }
        } else if let Some(e) = self.rust_prefix() {
            e?
        } else {
            self.parse_primary()?
        };

        // The last non-associative op and its level: `a < b < c` chains,
        // `a < b > c` is an error.
        let mut last_none: Option<(u32, String)> = None;
        // A comparison chain so far: its operands and operators.
        let mut cmp_chain: Option<(Vec<Expr>, Vec<String>)> = None;

        loop {
            self.chain_link()?;
            // `as` / `is` sit at `CAST_PREC`, left-assoc: below prefix unary
            // (`-x as i64` is `(-x) as i64`), above arithmetic.
            if matches!(self.peek(), TokenKind::As | TokenKind::Is) {
                if CAST_PREC < min_bp {
                    break;
                }
                let is_cast = matches!(self.peek(), TokenKind::As);
                self.bump();
                self.python_is_not(lhs.span, is_cast);
                let ty_span = self.peek_span();
                let ty = self.parse_cast_type()?;
                let span = lhs.span;
                let kind = if is_cast {
                    ExprKind::Cast(Box::new(lhs), ty)
                } else {
                    // `v is T` - runtime type test.
                    ExprKind::Is(
                        Box::new(lhs),
                        nabic_ast::Pattern {
                            kind: nabic_ast::PatternKind::TypeCheck { binder: None, ty },
                            span: ty_span,
                        },
                    )
                };
                lhs = Expr {
                    kind,
                    span: self.to_prev(span),
                };
                continue;
            }

            if self.incr_decr(&lhs) {
                break;
            }
            let member = self.peek_member_op();
            let Some((sym, _)) = member
                .map(|(s, _, _)| (s.to_owned(), lhs.span))
                .or_else(|| self.peek_op_symbol(Fixity::Infix))
            else {
                break;
            };
            // `??` is a language-level desugar, level 1,
            // right-assoc.
            if sym == "??" {
                if COALESCE_PREC < min_bp || self.closure_stmt_ends(&lhs) {
                    break;
                }
                self.bump();
                let rhs = self.parse_expr_bp(COALESCE_PREC)?;
                let span = self.to_prev(lhs.span);
                lhs = Expr {
                    kind: ExprKind::NullishCoalesce(Box::new(lhs), Box::new(rhs)),
                    span,
                };
                continue;
            }
            let Some((prec, assoc)) = self.ext.level(&sym, Fixity::Infix) else {
                break;
            };
            let (left_bp, right_bp) = match assoc {
                Assoc::Left | Assoc::None => (prec, prec + 1),
                Assoc::Right => (prec + 1, prec),
            };
            if left_bp < min_bp || self.closure_stmt_ends(&lhs) || self.percent_suffix(&sym, &lhs) {
                break;
            }
            // Comparisons of one direction chain (`a < b <= c` is
            // `a < b && b <= c`); other non-associative ops don't.
            if let Some((_, prev)) = last_none.as_ref().filter(|(p, _)| *p == prec) {
                let prev = prev.clone();
                let at = self.peek_span();
                if cmp_dir(&prev).is_some() && cmp_dir(&prev) == cmp_dir(&sym) {
                    let (mut operands, mut ops) = match (cmp_chain.take(), lhs.kind) {
                        (Some(c), _) => c,
                        (None, ExprKind::OpInfix { op, lhs: a, rhs: b }) => {
                            (vec![*a, *b], vec![op])
                        }
                        (None, kind) => unreachable!("a comparison before `{sym}`: {kind:?}"),
                    };
                    self.bump();
                    let rhs = self.parse_expr_bp(right_bp)?;
                    operands.push(rhs);
                    ops.push(sym.clone());
                    let span = self.to_prev(operands[0].span);
                    lhs = self.fold_cmp_chain(&operands, &ops, span);
                    cmp_chain = Some((operands, ops));
                    last_none = Some((prec, sym));
                    continue;
                }
                self.unchained(&prev, &sym, at);
            }
            self.bump_infix(member);
            let rhs = if matches!(sym.as_str(), "∈" | "∉") {
                self.parse_member_rhs(right_bp)?
            } else {
                self.parse_expr_bp(right_bp)?
            };
            let span = self.to_prev(lhs.span);
            lhs = Expr {
                kind: ExprKind::OpInfix {
                    op: sym,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                },
                span,
            };
            last_none = matches!(assoc, Assoc::None).then(|| (prec, op_of(&lhs)));
            cmp_chain = None;
        }
        if min_bp == 0 && (self.ternary_ahead() || self.python_ternary_ahead()) {
            return self.ternary(lhs);
        }
        Some(lhs)
    }

    /// `95%` with nothing after the `%`: a percentage written as a
    /// suffix. Reports it, takes the `%` and leaves `lhs` as the value.
    pub(crate) fn percent_suffix(&mut self, sym: &str, lhs: &Expr) -> bool {
        let ExprKind::Literal(Literal::Int(n) | Literal::Float(n)) = &lhs.kind else {
            return false;
        };
        if sym != "%" {
            return false;
        }
        let at = self.peek_span();
        let ends = matches!(
            self.peek_next(),
            None | Some(
                TokenKind::Semi
                    | TokenKind::Comma
                    | TokenKind::RParen
                    | TokenKind::RBracket
                    | TokenKind::RBrace
                    | TokenKind::Eof
            )
        );
        if lhs.span.end != at.start || !ends || self.pending.is_some() {
            return false;
        }
        let pct = self
            .literals
            .rules
            .iter()
            .any(|r| matches!(&r.kind, crate::PatKind::Suffix(s) if s == "pct"));
        let span = Span {
            end: at.end,
            ..lhs.span
        };
        let mut d = NabiDiagnostic::error(
            Code::INVALID_LITERAL_HANDLER,
            span,
            format!(
                "`{n}%` is not a percentage: `%` is the remainder operator (`a % b`), and a \
                 number suffix is a name"
            ),
        )
        .with_file(self.file.clone());
        d = if pct {
            d.with_help(format!("write `{n}pct`")).with_fix(
                span,
                format!("{n}pct"),
                format!("write `{n}pct`"),
            )
        } else {
            d.with_help(format!(
                "write `{n}pct` with a handler `#[literal(number \"pct\")]` in scope, or \
                 `{n} / 100.0`"
            ))
        };
        self.diagnostics.push(d);
        self.bump();
        true
    }

    /// `prev` then `sym` without parentheses, where they do not chain.
    pub(crate) fn unchained(&mut self, prev: &str, sym: &str, at: Span) {
        let help = if cmp_dir(prev).is_some() && cmp_dir(sym).is_some() {
            format!(
                "a chain compares in one direction (`a < b <= c`, `a > b >= c`, \
                 `a == b == c`); write `a {prev} b && b {sym} c`"
            )
        } else {
            format!("write `a {prev} b && b {sym} c`, or parenthesise one side")
        };
        self.error_help(
            Code::NEEDS_PARENTHESES,
            at,
            format!("`{prev}` and `{sym}` cannot be chained"),
            help,
        );
    }

    /// Past an infix operator: one token, or `!in` / a misspelt one.
    pub(crate) fn bump_infix(&mut self, member: Option<MemberOp>) {
        if let Some((_, _, Some((at, msg)))) = member {
            self.error_help(Code::SYNTAX, at, msg, "write `x !in xs` (or `x ∉ xs`)");
        }
        for _ in 0..member.map_or(1, |(_, n, _)| n) {
            self.bump();
        }
    }

    /// `e0 o1 e1 o2 e2 ..` as `e0 o1 e1 && e1 o2 e2 && ..`, each operand
    /// evaluated once and in order, the rest skipped once one is false.
    /// A middle operand other than a name, field or literal goes in a
    /// temporary (and then so does a left operand that may have effects).
    pub(crate) fn fold_cmp_chain(&mut self, operands: &[Expr], ops: &[String], span: Span) -> Expr {
        let [first, mid, rest @ ..] = operands else {
            unreachable!("a chain has two operands or more")
        };
        let infix = |op: &str, a: Expr, b: Expr| Expr {
            span: Span {
                end: b.span.end,
                ..a.span
            },
            kind: ExprKind::OpInfix {
                op: op.to_owned(),
                lhs: Box::new(a),
                rhs: Box::new(b),
            },
        };
        if rest.is_empty() {
            return infix(&ops[0], first.clone(), mid.clone());
        }
        let mut lets = Vec::new();
        let mut temp = |p: &mut Self, e: &Expr| {
            let name = p.fresh_destructure_temp();
            lets.push(Stmt {
                kind: StmtKind::Let {
                    name: name.clone(),
                    ty: None,
                    init: e.clone(),
                },
                span: e.span,
            });
            Expr {
                span: e.span,
                kind: ExprKind::Path(Path::new(
                    vec![PathSegment {
                        name,
                        generics: Vec::new(),
                    }],
                    e.span,
                )),
            }
        };
        let (first, mid) = if is_plain_operand(mid) {
            (first.clone(), mid.clone())
        } else if effect_free(first) {
            (first.clone(), temp(self, mid))
        } else {
            let f = temp(self, first);
            (f, temp(self, mid))
        };
        let mut tail = vec![mid.clone()];
        tail.extend(rest.iter().cloned());
        let tail_span = Span {
            start: mid.span.start,
            ..span
        };
        let and = Expr {
            span,
            kind: ExprKind::OpInfix {
                op: "&&".to_owned(),
                lhs: Box::new(infix(&ops[0], first, mid)),
                rhs: Box::new(self.fold_cmp_chain(&tail, &ops[1..], tail_span)),
            },
        };
        if lets.is_empty() {
            return and;
        }
        Expr {
            span,
            kind: ExprKind::Block(Block {
                stmts: lets,
                trailing: Some(Box::new(and)),
                span,
                label: None,
            }),
        }
    }

    /// `f` parsing a closure or fn body whose `return@name` may name
    /// `label`; with the bare `return;`s directly in it.
    pub(crate) fn ret_frame<T>(
        &mut self,
        label: Option<String>,
        f: impl FnOnce(&mut Self) -> Option<T>,
    ) -> (Option<T>, Vec<Span>) {
        self.ret_frames.push(RetFrame {
            label,
            bare: Vec::new(),
        });
        let r = f(self);
        let frame = self.ret_frames.pop().unwrap_or_default();
        (r, frame.bare)
    }

    /// The bare `return;`s of the closure starting at `span`.
    pub(crate) fn note_closure_returns(&mut self, span: Span, bare: Vec<Span>) {
        let key = (span.file, span.start);
        if bare.is_empty() {
            self.closure_returns.remove(&key);
        } else {
            self.closure_returns.insert(key, bare);
        }
    }

    /// A bare `return;` (its `return` at `at`).
    pub(crate) fn bare_return(&mut self, at: Span) {
        if let Some(f) = self.ret_frames.last_mut() {
            f.bare.push(at);
        }
    }

    /// `return@name` (`@name` glued to the `return` at `at`): leaves the
    /// innermost closure, which must be the trailing closure of a call
    /// to `name`. Whether there is one.
    pub(crate) fn return_label(&mut self, at: Span) -> bool {
        let glued = self.peek_span().start == at.end;
        if !glued || self.peek() != &TokenKind::At {
            return false;
        }
        let Some(TokenKind::Ident(name)) = self.tokens.get(self.pos + 1).map(|t| t.kind.clone())
        else {
            return false;
        };
        self.bump(); // `@`
        let span = self.bump_name().span;
        let inner = self.ret_frames.last().and_then(|f| f.label.as_deref());
        if inner == Some(name.as_str()) {
            return true;
        }
        let help = match inner {
            Some(l) => format!(
                "the innermost closure here is the trailing closure of `{l}`: write `return@{l}`"
            ),
            None => "`return@name` leaves the trailing closure of a call to `name`; \
                     here, write `return`"
                .to_owned(),
        };
        self.diagnostics.push(
            NabiDiagnostic::error(
                Code::TRAILING_CLOSURE_RETURN,
                span,
                format!("`return@{name}`: no enclosing trailing closure of a call to `{name}`"),
            )
            .with_file(self.file.clone())
            .with_help(help),
        );
        true
    }

    /// A bare `return;` in a statement's trailing closure is an error: it
    /// would leave only the closure, not the function.
    pub(crate) fn statement_closure_returns(&mut self, stmts: &[Stmt], tail: Option<&Expr>) {
        let exprs = stmts.iter().filter_map(|s| match &s.kind {
            StmtKind::Expr(e) | StmtKind::Trailing(e) => Some(e),
            _ => None,
        });
        let mut found = Vec::new();
        for e in exprs.chain(tail) {
            let (ExprKind::Call {
                trailing_closure: Some(c),
                ..
            }
            | ExprKind::MethodCall {
                trailing_closure: Some(c),
                ..
            }) = &e.kind
            else {
                continue;
            };
            if let Some(bare) = self.closure_returns.get(&(c.span.file, c.span.start)) {
                found.extend(bare.iter().map(|&at| (at, callee_name(e))));
            }
        }
        for (at, name) in found {
            let mut d = NabiDiagnostic::error(
                Code::TRAILING_CLOSURE_RETURN,
                at,
                "a bare `return` in a statement's trailing closure: it would leave only \
                 the closure, not the function",
            )
            .with_file(self.file.clone());
            d = match name {
                Some(n) => d
                    .with_help(format!(
                        "write `return@{n}` to end this call of the closure; to leave the \
                         function, write a `for` loop instead of the call"
                    ))
                    .with_fix(at, format!("return@{n}"), format!("write `return@{n}`")),
                None => d.with_help(
                    "to leave the function, write a `for` loop instead of the call; to end \
                     this call of the closure, pass it in the parentheses",
                ),
            };
            self.diagnostics.push(d);
        }
    }

    /// `lhs` is a trailing-closure call that starts a statement: like a
    /// block-like statement, it ends at its `}`, so a binary operator
    /// after it starts a new statement (`run { .. }` then `-x;`).
    pub(crate) fn closure_stmt_ends(&self, lhs: &Expr) -> bool {
        self.stmt_at == Some(lhs.span.start) && !is_block_like(lhs) && ends_stmt(lhs)
    }

    /// `in` / `!in` (ASCII for U+2208 / U+2209) at the cursor: the operator,
    /// its token count and, for a misspelling (`! in`, Python's
    /// `not in`), the error to give when it is taken.
    pub(crate) fn peek_member_op(&self) -> Option<MemberOp> {
        if self.pending.is_some() {
            return None;
        }
        let tok = |k: usize| self.tokens.get(self.pos + k);
        let is_in = |t: Option<&Token>| {
            t.is_some_and(|t| !t.raw && matches!(&t.kind, TokenKind::Ident(w) if w == "in"))
        };
        if is_in(tok(0)) {
            return Some(("∈", 1, None));
        }
        let (Some(first), true) = (tok(0), is_in(tok(1))) else {
            return None;
        };
        let glued = tok(1).is_some_and(|t| t.span.start == first.span.end);
        let bad = match &first.kind {
            TokenKind::Bang if glued => None,
            TokenKind::Bang => Some("space between `!` and `in`"),
            TokenKind::Ident(w) if w == "not" && !first.raw => Some("`not in` is not an operator"),
            _ => return None,
        };
        Some(("∉", 2, bad.map(|m| (first.span, m))))
    }

    /// The right side of `in` / `!in`: a range needs no parentheses
    /// (`i in 0..n`), its end binding like the left side's operand.
    pub(crate) fn parse_member_rhs(&mut self, right_bp: u32) -> Option<Expr> {
        let span = self.peek_span();
        let start = if matches!(self.peek(), TokenKind::DotDot | TokenKind::DotDotEq) {
            None
        } else {
            let e = self.parse_expr_bp(right_bp)?;
            if !matches!(self.peek(), TokenKind::DotDot | TokenKind::DotDotEq) {
                return Some(e);
            }
            Some(Box::new(e))
        };
        let inclusive = matches!(self.peek(), TokenKind::DotDotEq);
        self.bump();
        let end = if !inclusive && !self.can_start_range_end() {
            None
        } else {
            Some(Box::new(self.parse_expr_bp(right_bp)?))
        };
        Some(Expr {
            span: self.to_prev(span),
            kind: ExprKind::Range {
                start,
                end,
                inclusive,
            },
        })
    }

    /// The current token as a `fixity` operator; tokens that are also
    /// syntax (`|`, `<`) are the caller's to filter.
    pub(crate) fn peek_op_symbol(&self, fixity: Fixity) -> Option<(String, Span)> {
        let tok = self.tokens.get(self.pos)?;
        let sym = match (&tok.kind, fixity) {
            // Infix-only tokens.
            (TokenKind::Plus, Fixity::Infix) => "+".into(),
            (TokenKind::Minus, Fixity::Infix | Fixity::Prefix) => "-".into(),
            (TokenKind::Star, Fixity::Infix) => "*".into(),
            (TokenKind::StarStar, Fixity::Infix) => "**".into(),
            (TokenKind::DotStarStar, Fixity::Infix) => ".**".into(),
            (TokenKind::Slash, Fixity::Infix) => "/".into(),
            (TokenKind::Percent, Fixity::Infix) => "%".into(),
            (TokenKind::PercentPercent, Fixity::Infix) => "%%".into(),
            (TokenKind::DotPercentPercent, Fixity::Infix) => ".%%".into(),
            (TokenKind::DotAmpAmp, Fixity::Infix) => ".&&".into(),
            (TokenKind::DotPipePipe, Fixity::Infix) => ".||".into(),
            (TokenKind::EqEq, Fixity::Infix) => "==".into(),
            (TokenKind::NotEq, Fixity::Infix) => "!=".into(),
            (TokenKind::Lt, Fixity::Infix) => "<".into(),
            (TokenKind::Gt, Fixity::Infix) => ">".into(),
            (TokenKind::LtEq, Fixity::Infix) => "<=".into(),
            (TokenKind::GtEq, Fixity::Infix) => ">=".into(),
            (TokenKind::AmpAmp, Fixity::Infix) => "&&".into(),
            (TokenKind::PipePipe, Fixity::Infix) => "||".into(),
            (TokenKind::QuestionQuestion, Fixity::Infix) => "??".into(),
            (TokenKind::Amp, Fixity::Infix) => "&".into(),
            (TokenKind::Pipe, Fixity::Infix) => "|".into(),
            // `->` between two values is the ASCII spelling of `→`, as
            // `<=` is of `≤`; it only binds when a trait registers `→`.
            (TokenKind::Arrow, Fixity::Infix) if !self.arrow_ends_guard => "→".into(),
            (TokenKind::Caret, Fixity::Infix) => "^".into(),
            (TokenKind::Shl, Fixity::Infix) => "<<".into(),
            (TokenKind::Shr, Fixity::Infix) => ">>".into(),
            (TokenKind::DotPlus, Fixity::Infix) => ".+".into(),
            (TokenKind::DotMinus, Fixity::Infix) => ".-".into(),
            (TokenKind::DotStar, Fixity::Infix) => ".*".into(),
            (TokenKind::DotSlash, Fixity::Infix) => "./".into(),
            (TokenKind::DotPercent, Fixity::Infix) => ".%".into(),
            (TokenKind::DotEqEq, Fixity::Infix) => ".==".into(),
            (TokenKind::DotNotEq, Fixity::Infix) => ".!=".into(),
            (TokenKind::DotLt, Fixity::Infix) => ".<".into(),
            (TokenKind::DotGt, Fixity::Infix) => ".>".into(),
            (TokenKind::DotLtEq, Fixity::Infix) => ".<=".into(),
            (TokenKind::DotGtEq, Fixity::Infix) => ".>=".into(),
            (TokenKind::DotAmp, Fixity::Infix) => ".&".into(),
            (TokenKind::DotPipe, Fixity::Infix) => ".|".into(),
            (TokenKind::DotCaret, Fixity::Infix) => ".^".into(),
            (TokenKind::DotShl, Fixity::Infix) => ".<<".into(),
            (TokenKind::DotShr, Fixity::Infix) => ".>>".into(),
            (TokenKind::DotQuestionQuestion, Fixity::Infix) => ".??".into(),
            // Prefix-only tokens.
            (TokenKind::Bang, Fixity::Prefix) => "!".into(),
            (TokenKind::Tilde, Fixity::Prefix) => "~".into(),
            (TokenKind::MathOp('√'), Fixity::Prefix) => "√".into(),
            (TokenKind::MathOp(c), Fixity::Infix) if *c != '√' => c.to_string(),
            (TokenKind::ExtOp(s), f) if self.ext.levels.contains_key(&(s.clone(), f)) => s.clone(),
            // Binary `@` / `~`, where a trait binds it.
            (TokenKind::At | TokenKind::Tilde, Fixity::Infix)
                if self
                    .ext
                    .levels
                    .contains_key(&(tok.kind.text(), Fixity::Infix)) =>
            {
                tok.kind.text()
            }
            _ => return None,
        };
        Some((sym, tok.span))
    }
}

impl Parser<'_> {
    /// Arguments are inside `( )`, so a condition's struct-literal
    /// restriction does not reach them (`if f(S { a: 1 }) { .. }`).
    pub(crate) fn parse_call_args(&mut self) -> Option<Vec<CallArg>> {
        self.with_struct_expr(true, Self::parse_call_args_inner)
    }

    pub(crate) fn parse_call_args_inner(&mut self) -> Option<Vec<CallArg>> {
        let mut args = Vec::new();
        if self.prev_kind() == Some(&TokenKind::LParen) && self.comprehension(self.pos - 1) {
            return Some(args);
        }
        while !matches!(self.peek(), TokenKind::RParen | TokenKind::Eof) {
            // `name: expr` is a named argument.
            if let Some(name) = self.peek_name() {
                if matches!(self.peek_next(), Some(TokenKind::Colon)) || self.python_named_arg() {
                    self.bump_name(); // ident
                    self.bump(); // `:`
                    let value = self.parse_expr()?;
                    args.push(CallArg::Named { name, value });
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                    continue;
                }
            }
            // `ref x` passes the binding by reference.
            if matches!(self.peek(), TokenKind::Ref) {
                self.bump();
                let value = self.parse_expr()?;
                args.push(CallArg::Ref(value));
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
                continue;
            }
            // `..expr` is a spread argument.
            if matches!(self.peek(), TokenKind::DotDot) {
                self.bump();
                let value = self.parse_expr()?;
                args.push(CallArg::Spread(value));
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
                continue;
            }
            let value = self.parse_expr()?;
            args.push(CallArg::Positional(value));
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        // Positional arguments come first.
        if let Some(first) = args.iter().position(|a| matches!(a, CallArg::Named { .. })) {
            let late = args[first + 1..].iter().find_map(|a| match a {
                CallArg::Positional(e) | CallArg::Ref(e) => Some(e),
                _ => None,
            });
            if let Some(e) = late {
                self.error_help(
                    Code::NAMED_ARGUMENT,
                    e.span,
                    "positional argument after a named argument",
                    "write the positional arguments first, or name this one too",
                );
            }
        }
        Some(args)
    }
}

/// The direction a comparison chains in: `<` / `<=`, `>` / `>=`, `==`.
pub(crate) fn cmp_dir(op: &str) -> Option<u8> {
    match op {
        "<" | "<=" => Some(0),
        ">" | ">=" => Some(1),
        "==" => Some(2),
        _ => None,
    }
}

/// The operator of an infix `e` (empty for anything else).
pub(crate) fn op_of(e: &Expr) -> String {
    match &e.kind {
        ExprKind::OpInfix { op, .. } => op.clone(),
        _ => String::new(),
    }
}

/// A name, a field of one or a literal: read again rather than kept in a
/// temporary, so a literal still takes its type from the other side.
pub(crate) fn is_plain_operand(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Path(_) | ExprKind::SelfLower | ExprKind::Literal(_) => true,
        ExprKind::Field(b, _) | ExprKind::TupleField(b, _) => is_plain_operand(b),
        ExprKind::OpPrefix { op, operand } => op == "-" && is_plain_operand(operand),
        _ => false,
    }
}

/// No call, index, block or other code that may have an effect.
pub(crate) fn effect_free(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::OpInfix { lhs, rhs, .. } => effect_free(lhs) && effect_free(rhs),
        ExprKind::OpPrefix { operand, .. } | ExprKind::Cast(operand, _) => effect_free(operand),
        _ => is_plain_operand(e),
    }
}

/// `x?.a` followed by a member / index link: the link joins the `?.`
/// tail; anything else (`Err`) ends the optional chain.
pub(crate) fn fold_chain(e: Expr) -> Result<Expr, Expr> {
    let span = e.span;
    let (recv, tail) = match e.kind {
        ExprKind::QuestionDot(..) => return Ok(e),
        ExprKind::Field(r, name) if matches!(r.kind, ExprKind::QuestionDot(..)) => {
            (r, AccessTail::Field(name))
        }
        ExprKind::TupleField(r, i) if matches!(r.kind, ExprKind::QuestionDot(..)) => {
            (r, AccessTail::TupleField(i))
        }
        ExprKind::Index(r, i) if matches!(r.kind, ExprKind::QuestionDot(..)) => {
            (r, AccessTail::Index(i))
        }
        ExprKind::MethodCall {
            receiver,
            method,
            type_args,
            mut args,
            trailing_closure,
            ..
        } if matches!(receiver.kind, ExprKind::QuestionDot(..)) => {
            args.extend(trailing_closure.map(|c| CallArg::Positional(*c)));
            (
                receiver,
                AccessTail::Method {
                    method,
                    type_args,
                    args,
                },
            )
        }
        kind => return Err(Expr { kind, span }),
    };
    let ExprKind::QuestionDot(x, mut tails) = recv.kind else {
        unreachable!("checked above")
    };
    tails.push(tail);
    Ok(Expr {
        kind: ExprKind::QuestionDot(x, tails),
        span,
    })
}

/// A function the pipe can call: a path, closure or field.
const fn is_callee(e: &Expr) -> bool {
    matches!(
        e.kind,
        ExprKind::Path(_) | ExprKind::QPath(_) | ExprKind::Closure { .. } | ExprKind::Field(..)
    )
}
