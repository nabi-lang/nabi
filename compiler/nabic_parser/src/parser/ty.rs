//! Types and paths.

#[allow(clippy::wildcard_imports)]
use crate::parser::*;

impl Parser<'_> {
    /// A `fn` type behind `unsafe` and / or `extern "C"` /
    /// `extern "system"` (a C function pointer).
    pub(crate) fn parse_marked_fn_type(&mut self, start: Span) -> Option<TyKind> {
        let unsafe_fn = self.eat(&TokenKind::Unsafe);
        let mut system = false;
        if !unsafe_fn {
            self.bump(); // `extern`
            if let TokenKind::StrLit(abi) = self.peek().clone() {
                system = abi == "system";
                if abi != "C" && !system {
                    self.error(
                        Code::INVALID_ABI,
                        start,
                        format!("unsupported ABI `\"{abi}\"` (only \"C\" or \"system\")"),
                    );
                }
            }
            self.bump(); // ABI string
        }
        let mut t = self.parse_type_atom_inner()?;
        if let TyKind::Fn(_, _, bounds) = &mut t.kind {
            if unsafe_fn {
                bounds.unsafe_fn = true;
            } else {
                bounds.c_abi = true;
                bounds.system = system;
            }
        }
        Some(t.kind)
    }

    /// `extern "<abi>" fn` starting `k` tokens ahead (a C fn pointer type).
    pub(crate) fn at_c_fn_type(&self, k: usize) -> bool {
        let at = |i: usize| self.tokens.get(self.pos + k + i).map(|t| &t.kind);
        matches!(at(0), Some(TokenKind::Ident(n)) if n == "extern")
            && matches!(at(1), Some(TokenKind::StrLit(_)))
            && matches!(at(2), Some(TokenKind::Fn))
    }
}

impl Parser<'_> {
    /// A type. `|` (sum) binds weakest and is off in closure
    /// parameters and cast targets, where it ends the type.
    pub(crate) fn parse_type(&mut self) -> Option<Ty> {
        let start = self.peek_span();
        let first = self.parse_type_atom()?;
        if !(self.type_sum_allowed && matches!(self.peek(), TokenKind::Pipe)) {
            return Some(first);
        }
        let mut variants = vec![first];
        while self.eat(&TokenKind::Pipe) {
            variants.push(self.parse_type_atom()?);
        }
        Some(Ty {
            kind: TyKind::Sum(variants),
            span: start,
        })
    }

    /// Everything but `|`: paths, `fn(..) -> R`, tuples / grouping,
    /// arrays, `any`, `none`, `Self`, and their `T?`.
    pub(crate) fn parse_type_atom(&mut self) -> Option<Ty> {
        let mut t = self.nested(Self::parse_type_atom_inner)?;
        // `T?` binds tighter than `|`. After `x as T` / `x is
        // T` a `?` is the type's (write `(x as T)?`), but `??` coalesces.
        loop {
            let layers = match self.peek() {
                TokenKind::Question => 1,
                TokenKind::QuestionQuestion if !self.in_cast_type => 2,
                _ => break,
            };
            self.bump();
            for _ in 0..layers {
                t = optional_ty(t);
            }
        }
        Some(t)
    }

    /// `name!( .. )` where a type is expected: expand and
    /// re-parse the result as a type.
    // `Parser::parse_*` is not general over the expansion's lifetime.
    #[allow(clippy::redundant_closure_for_method_calls)]
    pub(crate) fn macro_call_type(&mut self) -> Option<Ty> {
        let (def, name, start, call) = self.take_macro_call(false)?;
        let poison = || Ty {
            kind: TyKind::Tuple(Vec::new()),
            span: start,
        };
        let t =
            def.and_then(|d| self.expand_call(&d, &name, start, &call, false, |p| p.parse_type()));
        Some(t.flatten().unwrap_or_else(poison))
    }

    pub(crate) fn parse_type_atom_inner(&mut self) -> Option<Ty> {
        if let Some(ty) = self.macro_call_type() {
            return Some(ty);
        }
        if let Some(ty) = self.rust_type() {
            return ty;
        }
        let start = self.peek_span();
        if self.at_some_type() {
            self.error_help(
                Code::SYNTAX,
                start,
                "`some Trait` is only a function's parameter or return type, or the whole right side of a `type` alias",
                "for a variable, leave the type out (`let x = ..`); for a field or a type \
                 argument, declare `<T: Trait>` and write `T`, or name the type with \
                 `type Name = some Trait;`; for a value of several types, write `any Trait`",
            );
            return self.parse_some_type();
        }
        let kind = match self.peek().clone() {
            // `any` | `any Tr` | `any (A + B)`. The bound binds
            // tighter than `|`: `any Error | none` is `(any Error) | none`.
            TokenKind::Ident(_)
                if self.at_kw("any")
                    && (!matches!(self.peek_next(), Some(TokenKind::ColonColon))
                        || self.at_any_rooted()) =>
            {
                self.bump();
                match self.peek() {
                    TokenKind::Ident(_) => TyKind::AnyTrait(vec![self.parse_type_path()?]),
                    TokenKind::ColonColon if self.at_global_path() => {
                        TyKind::AnyTrait(vec![self.parse_type_path()?])
                    }
                    TokenKind::LParen => {
                        self.bump();
                        let mut bounds = vec![self.parse_type_path()?];
                        while self.eat(&TokenKind::Plus) {
                            bounds.push(self.parse_type_path()?);
                        }
                        self.expect(&TokenKind::RParen, "`)`")?;
                        TyKind::AnyTrait(bounds)
                    }
                    _ => TyKind::Any,
                }
            }
            TokenKind::None => {
                self.bump();
                TyKind::None
            }
            // `Self::Item`: an associated type of the impl / trait.
            TokenKind::SelfUpper if self.peek_next() == Some(TokenKind::ColonColon) => {
                self.bump();
                self.bump();
                let mut p = self.parse_type_path()?;
                p.segments.insert(
                    0,
                    PathSegment {
                        name: "Self".to_owned(),
                        generics: Vec::new(),
                    },
                );
                p.span = start;
                TyKind::Path(p)
            }
            TokenKind::SelfUpper => {
                self.bump();
                TyKind::SelfUpper
            }
            TokenKind::Bang => {
                self.bump();
                TyKind::Never
            }
            TokenKind::Unsafe
                if self.peek_next() == Some(TokenKind::Fn) || self.at_c_fn_type(1) =>
            {
                self.parse_marked_fn_type(start)?
            }
            TokenKind::Ident(_) if self.at_c_fn_type(0) => self.parse_marked_fn_type(start)?,
            TokenKind::Ident(_) => tuple_or_path(self.parse_type_path()?),
            TokenKind::ColonColon if self.at_global_path() => TyKind::Path(self.parse_type_path()?),
            // `<T as Tr>::X`.
            TokenKind::Lt | TokenKind::Shl => TyKind::QPath(Box::new(self.parse_qpath()?)),
            TokenKind::Fn => {
                self.bump();
                self.fn_type_rest(start)?
            }
            TokenKind::LParen => {
                // `()` unit, `(T)` grouping, `(T,)` / `(A, B)` tuple.
                self.bump();
                let mut tys = Vec::new();
                let mut trailing_comma = false;
                let ok = self.with_type_sum(true, |p| {
                    let prev_cast = std::mem::replace(&mut p.in_cast_type, false);
                    let mut ok = true;
                    while !matches!(p.peek(), TokenKind::RParen | TokenKind::Eof) {
                        let Some(t) = p.parse_type() else {
                            ok = false;
                            break;
                        };
                        tys.push(t);
                        trailing_comma = p.eat(&TokenKind::Comma);
                        if !trailing_comma {
                            break;
                        }
                    }
                    p.in_cast_type = prev_cast;
                    ok
                });
                if !ok {
                    return None;
                }
                self.expect(&TokenKind::RParen, "`)`")?;
                if tys.len() == 1 && !trailing_comma {
                    return tys.pop();
                }
                TyKind::Tuple(tys)
            }
            // `[T; N]` fixed-size array.
            TokenKind::LBracket => {
                self.bump();
                let elem = self.with_type_sum(true, Self::parse_type)?;
                self.expect(&TokenKind::Semi, "`;`")?;
                let len = self.parse_const_len()?;
                self.expect(&TokenKind::RBracket, "`]`")?;
                TyKind::Array(Box::new(elem), len)
            }
            other => {
                self.error(
                    Code::SYNTAX,
                    start,
                    format!("expected type, found {}", other.describe()),
                );
                return None;
            }
        };
        Some(Ty { kind, span: start })
    }

    /// `(..) -> R` of a `fn` type, after `fn`.
    pub(crate) fn fn_type_rest(&mut self, start: Span) -> Option<TyKind> {
        self.expect(&TokenKind::LParen, "`(`")?;
        let args = self.fn_type_params()?;
        let throws_span = self.peek_span();
        let throws = self.parse_throws();
        // `fn(..) -> R` binds tighter than `|`:
        // `fn() -> A | B` is `(fn() -> A) | B`.
        let mut ret = if self.eat(&TokenKind::Arrow) {
            self.parse_type_atom()?
        } else {
            Ty {
                kind: TyKind::Tuple(Vec::new()),
                span: start,
            }
        };
        let err = match throws {
            ThrowsKind::None => None,
            // Claimed by an enclosing `rethrows` signature, else
            // reported at the end of the parse.
            ThrowsKind::Untyped => {
                self.untyped_throws.push(throws_span);
                Some(nabic_ast::untyped_throws_ty(throws_span))
            }
            ThrowsKind::Typed(mut tys) if tys.len() == 1 => tys.pop(),
            ThrowsKind::Typed(tys) => Some(Ty {
                kind: TyKind::Sum(tys),
                span: throws_span,
            }),
        };
        if let Some(err) = err {
            let span = ret.span;
            ret = Ty {
                kind: TyKind::Throws {
                    ok: Box::new(ret),
                    err: Box::new(err),
                },
                span,
            };
        }
        // `fn(..) -> R + Send + Sync` - auto-trait bounds.
        let mut bounds = nabic_ast::FnTyBounds::default();
        while self.eat(&TokenKind::Plus) {
            bounds.auto.push(self.parse_type_path()?);
        }
        Some(TyKind::Fn(args, Box::new(ret), bounds))
    }

    /// Comma-separated types up to (not including) `close`. Sum types are
    /// allowed inside the brackets even where a bare `|` would end a type.
    pub(crate) fn nested_type_list(&mut self, close: TokenKind) -> Option<Vec<Ty>> {
        self.with_type_sum(true, |p| {
            let prev_cast = std::mem::replace(&mut p.in_cast_type, false);
            let mut tys = Vec::new();
            let mut ok = true;
            while std::mem::discriminant(p.peek()) != std::mem::discriminant(&close)
                && !matches!(
                    p.peek(),
                    TokenKind::Eof | TokenKind::Shr | TokenKind::GtEq | TokenKind::ShrEq
                )
            {
                if p.rust_lifetime(true) {
                    continue;
                }
                // Const argument: a literal, `-lit`, or
                // `{ expr }`.
                if matches!(
                    p.peek(),
                    TokenKind::IntLit(_)
                        | TokenKind::CharLit(_)
                        | TokenKind::Minus
                        | TokenKind::True
                        | TokenKind::False
                        | TokenKind::LBrace
                ) {
                    let span = p.peek_span();
                    let e = if p.eat(&TokenKind::LBrace) {
                        let e = p.with_struct_expr(true, Self::parse_expr);
                        p.expect(&TokenKind::RBrace, "`}`")?;
                        e
                    } else {
                        p.parse_expr_bp(PREFIX_PREC)
                    };
                    let Some(e) = e else {
                        ok = false;
                        break;
                    };
                    tys.push(Ty {
                        kind: TyKind::Const(ConstExpr::Expr(Box::new(e))),
                        span,
                    });
                    if !p.eat(&TokenKind::Comma) {
                        break;
                    }
                    continue;
                }
                // `Name = Ty`: an associated-type binding.
                let binding = match (p.peek().clone(), p.tokens.get(p.pos + 1).map(|t| &t.kind)) {
                    (TokenKind::Ident(name), Some(TokenKind::Eq)) => {
                        p.bump();
                        p.bump();
                        Some(name)
                    }
                    _ => None,
                };
                let Some(t) = p.parse_type() else {
                    ok = false;
                    break;
                };
                tys.push(match binding {
                    Some(name) => Ty {
                        span: t.span,
                        kind: TyKind::Binding(name, Box::new(t)),
                    },
                    None => t,
                });
                if !p.eat(&TokenKind::Comma) {
                    break;
                }
            }
            p.in_cast_type = prev_cast;
            ok.then_some(tys)
        })
    }

    /// Target type of `expr as T`: no bare `|` sum, and `<` only opens
    /// generic args when a matching `>` follows.
    pub(crate) fn parse_cast_type(&mut self) -> Option<Ty> {
        let prev = std::mem::replace(&mut self.in_cast_type, true);
        let ty = self.with_type_sum(false, Self::parse_type);
        self.in_cast_type = prev;
        ty
    }

    /// Lookahead from a `<`: does a matching `>` follow, with only
    /// type-shaped tokens in between?
    pub(crate) fn generic_close_ahead(&self) -> bool {
        let mut angle = 0i32;
        let mut nest = 0i32;
        for t in self.tokens.iter().skip(self.pos) {
            match &t.kind {
                TokenKind::Lt => angle += 1,
                TokenKind::Gt => angle -= 1,
                TokenKind::Shr => angle -= 2,
                TokenKind::LParen | TokenKind::LBracket => nest += 1,
                TokenKind::RParen | TokenKind::RBracket => {
                    nest -= 1;
                    if nest < 0 {
                        return false;
                    }
                }
                TokenKind::Semi if nest > 0 => {}
                TokenKind::Ident(_)
                | TokenKind::ColonColon
                | TokenKind::Comma
                | TokenKind::IntLit(_)
                | TokenKind::CharLit(_)
                | TokenKind::Fn
                | TokenKind::Arrow
                | TokenKind::None
                | TokenKind::SelfUpper
                | TokenKind::Pipe
                | TokenKind::Question
                | TokenKind::QuestionQuestion => {}
                _ => return false,
            }
            if angle <= 0 {
                return nest == 0;
            }
        }
        false
    }

    pub(crate) fn parse_path(&mut self) -> Option<Path> {
        self.parse_path_inner(false)
    }

    /// At `any ::name` with the `::` apart from `any` (as written, or a
    /// renamed dependency's `::<name>`): a bound from the packages, where
    /// `any::name` is a path.
    pub(crate) fn at_any_rooted(&self) -> bool {
        let t = |i: usize| self.tokens.get(self.pos + i);
        match (t(0), t(1), t(2)) {
            (Some(a), Some(sep), Some(n)) => {
                self.pending.is_none()
                    && sep.kind == TokenKind::ColonColon
                    && matches!(n.kind, TokenKind::Ident(_))
                    && sep.span.start != a.span.end
            }
            _ => false,
        }
    }

    /// At `::name`: a path from the packages (`::std::fs`).
    pub(crate) fn at_global_path(&self) -> bool {
        matches!(self.peek(), TokenKind::ColonColon)
            && matches!(self.peek_next(), Some(TokenKind::Ident(_)))
    }

    /// `<T as Tr>::item`.
    pub(crate) fn parse_qpath(&mut self) -> Option<nabic_ast::QPath> {
        if !self.eat_opening_lt() {
            self.expect(&TokenKind::Lt, "`<`")?;
        }
        let self_ty = self.with_type_sum(false, Self::parse_type_atom)?;
        self.expect(&TokenKind::As, "`as`")?;
        let tr = self.parse_type_path()?;
        if !self.expect_closing_gt() {
            return None;
        }
        self.expect(&TokenKind::ColonColon, "`::`")?;
        let TokenKind::Ident(name) = self.peek().clone() else {
            let s = self.peek_span();
            self.error(
                Code::SYNTAX,
                s,
                format!(
                    "expected associated item name after `<.. as ..>::`, found {}",
                    self.peek().describe()
                ),
            );
            return None;
        };
        self.bump();
        Some(nabic_ast::QPath {
            self_ty,
            trait_path: tr,
            item: name,
            member: None,
        })
    }

    pub(crate) fn parse_type_path(&mut self) -> Option<Path> {
        self.parse_path_inner(true)
    }

    pub(crate) fn parse_path_inner(&mut self, allow_generics: bool) -> Option<Path> {
        let start = self.peek_span();
        let origin = self.tokens.get(self.pos).and_then(Token::origin_module);
        let mut segments = Vec::new();
        // `::std::fs`: the empty first segment roots it at the packages.
        if self.at_global_path() {
            self.bump();
            segments.push(PathSegment {
                name: String::new(),
                generics: Vec::new(),
            });
        }
        loop {
            let TokenKind::Ident(name) = self.peek().clone() else {
                let s = self.peek_span();
                self.error(
                    Code::SYNTAX,
                    s,
                    format!("expected path segment, found {}", self.peek().describe()),
                );
                return None;
            };
            self.bump();
            // Generic args only in type position; in expressions `<` could
            // be a comparison.
            let mut generics: Vec<Ty> = Vec::new();
            if allow_generics
                && (matches!(self.peek(), TokenKind::Shl) && !self.in_cast_type
                    || matches!(self.peek(), TokenKind::Lt)
                        && (!self.in_cast_type || self.generic_close_ahead()))
            {
                self.eat_opening_lt();
                generics = self.nested_type_list(TokenKind::Gt)?;
                if !self.expect_closing_gt() {
                    return None;
                }
            }
            segments.push(PathSegment { name, generics });
            if !self.eat(&TokenKind::ColonColon) {
                break;
            }
            // `f::<T, 3>(..)`; type paths too.
            if matches!(self.peek(), TokenKind::Lt) {
                self.bump();
                let args = self.nested_type_list(TokenKind::Gt)?;
                if !self.expect_closing_gt() {
                    return None;
                }
                if let Some(last) = segments.last_mut() {
                    last.generics = args;
                }
                if !self.eat(&TokenKind::ColonColon) {
                    break;
                }
            }
        }
        Some(Path {
            segments,
            span: start,
            origin,
        })
    }
}

/// `T?` is `Nest(T) | none`, so `(i32 | none)?` keeps two layers while
/// `i32?` is `i32 | none`. `Tuple<A, B>` is `(A, B)`.
pub(crate) fn tuple_or_path(mut p: Path) -> TyKind {
    match p.segments.as_mut_slice() {
        [s] if s.name == "Tuple" && !s.generics.is_empty() => {
            TyKind::Tuple(std::mem::take(&mut s.generics))
        }
        _ => TyKind::Path(p),
    }
}

pub(crate) fn optional_ty(t: Ty) -> Ty {
    let span = t.span;
    let nest = Ty {
        kind: TyKind::Nest(Box::new(t)),
        span,
    };
    let none = Ty {
        kind: TyKind::None,
        span,
    };
    Ty {
        kind: TyKind::Sum(vec![nest, none]),
        span,
    }
}

pub(crate) fn path_ty(name: &str, span: Span) -> Ty {
    Ty {
        kind: TyKind::Path(Path {
            segments: vec![PathSegment {
                name: name.to_owned(),
                generics: Vec::new(),
            }],
            span,
            origin: None,
        }),
        span,
    }
}
