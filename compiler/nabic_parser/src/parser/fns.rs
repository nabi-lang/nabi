//! Functions: signatures, generics, bounds, throws, parameters.

#[allow(clippy::wildcard_imports)]
use crate::parser::*;

impl Parser<'_> {
    /// Attributes and visibility in either order, or attributes on both
    /// sides (`pub #[inline] fn`).
    pub(crate) fn parse_attrs_vis(&mut self) -> (Vec<Attribute>, Visibility) {
        let mut attrs = self.parse_outer_attrs();
        let vis = self.parse_visibility();
        if vis != Visibility::Private {
            attrs.extend(self.parse_outer_attrs());
        }
        (attrs, vis)
    }

    pub(crate) fn parse_visibility(&mut self) -> Visibility {
        if !self.eat(&TokenKind::Pub) {
            return Visibility::Private;
        }
        if !self.eat(&TokenKind::LParen) {
            return Visibility::Pub;
        }
        if let Some(v) = self.rust_visibility() {
            return v;
        }
        // `pub(reach)`. Contextual identifier; only valid inside `pub(...)`.
        let v = if let TokenKind::Ident(name) = self.peek().clone() {
            match name.as_str() {
                "pkg" => Visibility::Pkg,
                "sub" => Visibility::Sub,
                _ => {
                    let s = self.peek_span();
                    self.error(Code::SYNTAX, s, "expected `pkg` or `sub`");
                    Visibility::Pub
                }
            }
        } else {
            let s = self.peek_span();
            self.error(Code::SYNTAX, s, "expected `pkg` or `sub`");
            Visibility::Pub
        };
        self.bump();
        self.expect(&TokenKind::RParen, "`)`");
        v
    }

    /// A function's name. Contextual keywords lex as identifiers,
    /// so `fn union` (`Set::union`) and `fn any` (`Iterator::any`) work.
    pub(crate) fn fn_name(&mut self) -> Option<String> {
        let TokenKind::Ident(name) = self.peek().clone() else {
            let s = self.peek_span();
            self.expected_name(s, "function name");
            return None;
        };
        self.bump();
        Some(name)
    }

    /// At `const fn` / `const unsafe fn`: a plain fn carrying `CONST_FN_ATTR`.
    pub(crate) fn at_const_fn(&self) -> bool {
        matches!(self.peek_next(), Some(TokenKind::Fn | TokenKind::Unsafe))
    }

    pub(crate) fn parse_const_fn(
        &mut self,
        vis: Visibility,
        mut attrs: Vec<Attribute>,
    ) -> Option<FnDef> {
        let span = self.peek_span();
        self.bump(); // `const`
        attrs.push(Attribute {
            path: vec![nabic_ast::CONST_FN_ATTR.to_owned()],
            body: nabic_ast::AttrBody::Empty,
            inner: false,
            span,
        });
        let def = self.parse_fn(vis, attrs)?;
        // No type arguments; introspection is meta blocks'.
        for p in &def.params {
            if let Some(nabic_ast::Ty {
                kind: nabic_ast::TyKind::Path(path),
                span,
            }) = &p.ty
            {
                if matches!(path.segments.as_slice(), [s] if s.name == "type") {
                    self.error_help(
                        Code::INVALID_SIGNATURE,
                        *span,
                        "a `const fn` cannot take a type as an argument: use a \
                         generic parameter, or a meta block for type introspection",
                        format!(
                            "take a type parameter: `const fn {}<T>() -> usize {{ \
                             T::size() }}`",
                            def.name
                        ),
                    );
                }
            }
        }
        Some(def)
    }

    pub(crate) fn parse_fn(&mut self, vis: Visibility, attrs: Vec<Attribute>) -> Option<FnDef> {
        // Modifiers in fixed order `unsafe async yield`; a
        // misordered or repeated one is reported and kept.
        const MODS: [&str; 3] = ["unsafe", "async", "yield"];
        let start = self.peek_span();
        let mut seen: Vec<(usize, Span)> = Vec::new();
        loop {
            let rank = match self.peek() {
                TokenKind::Unsafe => 0,
                TokenKind::Async => 1,
                TokenKind::Yield => 2,
                _ => break,
            };
            seen.push((rank, self.peek_span()));
            self.bump();
        }
        if let Some(w) = seen.windows(2).find(|w| w[1].0 <= w[0].0) {
            let (this, prev) = (MODS[w[1].0], MODS[w[0].0]);
            let mut ranks: Vec<usize> = seen.iter().map(|m| m.0).collect();
            ranks.sort_unstable();
            ranks.dedup();
            let fixed: Vec<&str> = ranks.iter().map(|r| MODS[*r]).collect();
            let msg = if w[1].0 == w[0].0 {
                format!("fn modifier `{this}` written twice")
            } else {
                format!(
                    "fn modifier `{this}` after `{prev}`: modifiers go in the order `unsafe \
                     async yield`"
                )
            };
            self.error_help(
                Code::MODIFIER_ORDER,
                w[1].1,
                msg,
                format!("write `{} fn`", fixed.join(" ")),
            );
        }
        let is_unsafe = seen.iter().any(|m| m.0 == 0);
        let is_async = seen.iter().any(|m| m.0 == 1);
        let is_generator = seen.iter().any(|m| m.0 == 2);
        self.expect(&TokenKind::Fn, "`fn`")?;
        let name = self.fn_name()?;
        // Optional generic parameters.
        let generics = if matches!(self.peek(), TokenKind::Lt) {
            self.parse_generic_params()
        } else {
            Generics::default()
        };
        self.expect(&TokenKind::LParen, "`(`")?;
        let untyped_mark = self.untyped_throws.len();
        let pats_mark = self.param_pats.len();
        let mut params = Vec::new();
        let mut attrs = attrs;
        while !matches!(self.peek(), TokenKind::RParen | TokenKind::Eof) {
            if self.eat_c_variadic(&mut attrs) {
                break;
            }
            let p_attrs = self.cfg_only_attrs("a parameter");
            let marks = (self.pending_stmts.len(), self.lifted.len());
            let param = self.parse_param()?;
            if !self.cfg_drops(&p_attrs, marks) {
                params.push(param);
            }
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        self.expect(&TokenKind::RParen, "`)`")?;
        let rparen = self.prev_span();
        let pats: Vec<(String, Pattern)> = self.param_pats.drain(pats_mark..).collect();
        // `fn Vec(comptime T: type) type`: the `type` result goes with the
        // `comptime` error.
        if self.at_kw("type") && matches!(self.peek_next(), Some(TokenKind::LBrace)) {
            let span = self.peek_span();
            self.bump();
            self.error_help(
                Code::INVALID_SIGNATURE,
                span,
                "a function cannot return a type",
                "make it a generic type: `struct Vec<T> { .. }`",
            );
        }

        let mut generics = generics;
        self.desugar_variadic(&mut params);
        let throws = if matches!(self.peek(), TokenKind::Ident(n) if n == "rethrows") {
            let span = self.peek_span();
            self.bump();
            self.untyped_throws.truncate(untyped_mark);
            let errs = desugar_rethrows(&mut params, &mut generics);
            if errs.is_empty() {
                self.error(
                    Code::INVALID_SIGNATURE,
                    span,
                    "`rethrows` needs a parameter of throwing function type \
                     (`f: fn(T) throws -> U`)",
                );
            }
            ThrowsKind::Typed(errs)
        } else {
            match self.parse_throws() {
                ThrowsKind::Untyped => ThrowsKind::Typed(vec![nabic_ast::untyped_throws_ty(start)]),
                t => t,
            }
        };
        let ret_ty = self.parse_fn_ret()?;
        let throws = self.throws_after_ret(throws, ret_ty.as_ref(), start, rparen);
        let where_clause = self.parse_where_clause();

        let body = if matches!(self.peek(), TokenKind::LBrace) {
            let owned = match (&self.fn_owner, self.fn_names.is_empty()) {
                (Some(o), true) => format!("{o}.{name}"),
                _ => name.clone(),
            };
            self.fn_names.push(owned);
            // `//!` at the start of the body documents the function.
            self.doc_attrs_at(self.pos + 1, true, &mut attrs);
            let (b, _) = self.ret_frame(None, Self::parse_block);
            self.fn_names.pop();
            let mut b = b?;
            b.stmts.splice(0..0, self.param_lets(pats));
            Some(b)
        } else {
            self.expect(&TokenKind::Semi, "`;` or function body")?;
            None
        };

        Some(FnDef {
            vis,
            attrs,
            name,
            generics,
            params,
            ret_ty,
            body: body.map(Into::into),
            is_async,
            is_generator,
            is_unsafe,
            throws,
            where_clause,
            span: start,
        })
    }

    /// `fn printf(fmt: str, ...)`: C variadic marker, kept as
    /// an attribute; HIR checks it is last and in an `extern` block.
    pub(crate) fn eat_c_variadic(&mut self, attrs: &mut Vec<Attribute>) -> bool {
        if !(matches!(self.peek(), TokenKind::DotDot)
            && matches!(self.peek_next(), Some(TokenKind::Dot)))
        {
            return false;
        }
        let span = self.peek_span();
        self.bump();
        self.bump();
        attrs.push(Attribute {
            path: vec![nabic_ast::C_VARIADIC_ATTR.to_owned()],
            body: nabic_ast::AttrBody::Empty,
            inner: false,
            span,
        });
        self.eat(&TokenKind::Comma);
        if !matches!(self.peek(), TokenKind::RParen) {
            self.error(
                Code::INVALID_SIGNATURE,
                span,
                "`...` must be the last parameter",
            );
        }
        true
    }

    /// `xs: ...T` is `xs: Slice<T>` over the trailing
    /// arguments, so there is no instance per argument count. Last only.
    pub(crate) fn desugar_variadic(&mut self, params: &mut [Param]) {
        // Defaults are trailing. A variadic may follow them,
        // and so may a last `fn` parameter, which a trailing closure
        // fills.
        if let Some(first) = params.iter().position(|p| p.default.is_some()) {
            let last = params.len() - 1;
            let late = params[first + 1..].iter().enumerate().find_map(|(k, p)| {
                let closure_slot = first + 1 + k == last
                    && p.ty
                        .as_ref()
                        .is_some_and(|t| matches!(t.kind, TyKind::Fn(..)));
                (p.default.is_none() && !p.variadic && !closure_slot).then_some(p)
            });
            if let Some(p) = late {
                let msg = format!(
                    "parameter `{}` has no default but follows one that does; defaults are \
                     trailing",
                    p.name
                );
                let help = format!(
                    "give `{}` a default, or move it before `{}`",
                    p.name, params[first].name
                );
                self.error_help(Code::INVALID_SIGNATURE, p.span, msg, help);
            }
        }
        let last = params.len().saturating_sub(1);
        for (i, p) in params.iter_mut().enumerate() {
            if !p.variadic {
                continue;
            }
            if i != last {
                self.error(
                    Code::INVALID_SIGNATURE,
                    p.span,
                    "only the last parameter can be variadic `...T`",
                );
                continue;
            }
            let Some(elem) = p.ty.take() else { continue };
            p.ty = Some(variadic_slice_ty(elem));
        }
    }

    /// Optional `<T: Bound = Default, const N: usize>`; empty when not at `<`.
    pub(crate) fn parse_generic_params(&mut self) -> Generics {
        let mut params: Vec<nabic_ast::GenericParam> = Vec::new();
        if !self.eat(&TokenKind::Lt) {
            return Generics { params };
        }
        let mut defaulted = false;
        while !matches!(self.peek(), TokenKind::Gt | TokenKind::Eof) {
            if self.rust_lifetime(true) {
                continue;
            }
            let start = self.peek_span();
            if matches!(self.peek_next(), Some(TokenKind::Ident(_))) && self.eat_kw("const") {
                let TokenKind::Ident(name) = self.peek().clone() else {
                    let s = self.peek_span();
                    self.error(Code::SYNTAX, s, "const-generic name expected");
                    break;
                };
                self.bump();
                self.expect(&TokenKind::Colon, "`:`");
                if let Some(ty) = self.parse_type() {
                    if !const_param_ty(&ty) {
                        self.error_help(
                            Code::INVALID_GENERIC_PARAMS,
                            ty.span,
                            format!(
                                "const parameter `{name}` has type `{}`; only integers, `bool`, \
                                 `char` and `c_char` may be const parameters",
                                nabic_ast::print::print_ty(&ty)
                            ),
                            "use an integer type, such as `usize`",
                        );
                    }
                    params.push(nabic_ast::GenericParam::Const { name, ty });
                }
                // Optional `= const_expr` default (not modelled).
                if self.eat(&TokenKind::Eq) {
                    let _ = self.parse_expr_bp(0);
                }
            } else {
                let TokenKind::Ident(name) = self.peek().clone() else {
                    let s = self.peek_span();
                    self.expected_name(s, "generic param name");
                    break;
                };
                self.bump();
                let bounds = if self.eat(&TokenKind::Colon) {
                    self.parse_bounds()
                } else {
                    Vec::new()
                };
                // Optional default `= T2`.
                let default = if self.eat(&TokenKind::Eq) {
                    self.parse_type()
                } else {
                    None
                };
                params.push(nabic_ast::GenericParam::Type {
                    name,
                    bounds,
                    default,
                });
            }
            // Defaults are trailing.
            match params.last() {
                Some(nabic_ast::GenericParam::Type {
                    default: Some(_), ..
                }) => defaulted = true,
                Some(
                    nabic_ast::GenericParam::Type { name, .. }
                    | nabic_ast::GenericParam::Const { name, .. },
                ) if defaulted => {
                    let msg = format!(
                        "generic parameter `{name}` has no default but follows one that does; \
                         defaults must be trailing"
                    );
                    self.error(Code::INVALID_GENERIC_PARAMS, start, msg);
                }
                _ => {}
            }
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        self.expect_closing_gt();
        Generics { params }
    }

    /// A declaration's `-> T` (`-> some Tr` too); `Some(None)` without one.
    #[allow(clippy::option_option)] // a parse error, or no `->`
    pub(crate) fn parse_fn_ret(&mut self) -> Option<Option<Ty>> {
        if !self.eat(&TokenKind::Arrow) {
            return Some(None);
        }
        Some(Some(if self.at_some_type() {
            self.parse_some_type()?
        } else {
            self.parse_type()?
        }))
    }

    /// At `some Tr`, `some (A + B)` or `some fn(..)`?
    pub(crate) fn at_some_type(&self) -> bool {
        self.at_kw("some")
            && matches!(
                self.peek_next(),
                Some(TokenKind::Ident(_) | TokenKind::LParen | TokenKind::Fn)
            )
    }

    /// `some Tr` / `some (A + B)` / `some fn(A) -> R`. The bound binds
    /// tighter than `|`, as `any`'s does.
    pub(crate) fn parse_some_type(&mut self) -> Option<Ty> {
        let start = self.peek_span();
        self.bump();
        let one = |p: &mut Self| -> Option<Ty> {
            let at = p.peek_span();
            match p.peek() {
                TokenKind::Fn => p.parse_type_atom_inner(),
                TokenKind::Ident(n) if n == "Fn" || n == "FnMut" || n == "FnOnce" => {
                    p.error_help(
                        Code::SYNTAX,
                        at,
                        format!("Nabi has no `{n}` trait"),
                        "a function type is written `fn(A) -> R`: `some fn(i32) -> i32`",
                    );
                    p.bump();
                    p.parse_type_atom_inner()
                }
                _ => Some(Ty {
                    kind: TyKind::Path(p.parse_type_path()?),
                    span: at,
                }),
            }
        };
        let bounds = if self.eat(&TokenKind::LParen) {
            let mut bounds = vec![one(self)?];
            while self.eat(&TokenKind::Plus) {
                bounds.push(one(self)?);
            }
            self.expect(&TokenKind::RParen, "`)`")?;
            bounds
        } else {
            vec![one(self)?]
        };
        Some(Ty {
            kind: TyKind::Some(bounds),
            span: start,
        })
    }

    /// `Bound (+ Bound)*`.
    pub(crate) fn parse_bounds(&mut self) -> Vec<Ty> {
        let mut bounds = Vec::new();
        loop {
            if self.rust_lifetime(false) {
            } else if let Some(t) = self.parse_type() {
                bounds.push(t);
            }
            if !self.eat(&TokenKind::Plus) {
                return bounds;
            }
        }
    }

    /// The `N` of `[T; N]` / `[v; N]`: a const expression (`3`, `N`,
    /// `M * N`).
    pub(crate) fn parse_const_len(&mut self) -> Option<ConstExpr> {
        Some(match (self.peek().clone(), self.peek_next()) {
            (TokenKind::IntLit(s), Some(TokenKind::RBracket)) => {
                self.bump();
                ConstExpr::Literal(Literal::Int(s))
            }
            _ => ConstExpr::Expr(Box::new(self.with_struct_expr(false, Self::parse_expr)?)),
        })
    }

    /// Optional `where T: A + B, U::Out: C`.
    pub(crate) fn parse_where_clause(&mut self) -> Vec<nabic_ast::WherePred> {
        let mut preds = Vec::new();
        if !self.eat_kw("where") {
            return preds;
        }
        while !matches!(
            self.peek(),
            TokenKind::LBrace | TokenKind::Semi | TokenKind::Eof
        ) {
            let Some(ty) = self.parse_type() else { break };
            if self
                .expect(&TokenKind::Colon, "`:` in where clause")
                .is_none()
            {
                break;
            }
            let bounds = self.parse_bounds();
            preds.push(nabic_ast::WherePred { ty, bounds });
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        preds
    }

    pub(crate) fn parse_throws(&mut self) -> ThrowsKind {
        if !self.eat_kw("throws") {
            return ThrowsKind::None;
        }
        if !self.eat(&TokenKind::LParen) {
            return ThrowsKind::Untyped;
        }
        let mut tys = Vec::new();
        while !matches!(self.peek(), TokenKind::RParen | TokenKind::Eof) {
            if let Some(ty) = self.parse_type() {
                tys.push(ty);
            }
            if !self.eat(&TokenKind::Pipe) {
                break;
            }
        }
        self.expect(&TokenKind::RParen, "`)`");
        ThrowsKind::Typed(tys)
    }

    pub(crate) fn parse_param(&mut self) -> Option<Param> {
        let start = self.peek_span();
        if let Some(p) = self.rust_param() {
            return Some(p);
        }
        // Zig's `comptime T: type`: a type parameter.
        if self.at_kw("comptime") && matches!(self.peek_next(), Some(TokenKind::Ident(_))) {
            self.error_help(
                Code::SYNTAX,
                start,
                "Nabi has no `comptime` parameters",
                "a type parameter goes in `<..>`: `struct Vec<T> { .. }` or `fn f<T>(x: T)`",
            );
            self.bump();
        }
        let mut by_ref = self.eat(&TokenKind::Ref);

        // `self` / `ref self`; its type is filled in with the trait or impl.
        if matches!(self.peek(), TokenKind::SelfLower) {
            self.bump();
            return Some(Param {
                name: "self".to_owned(),
                ty: None,
                by_ref,
                default: None,
                variadic: false,
                span: start,
            });
        }

        // `(a, b): (i32, i32)` / `P { x, y }: P`: a temporary parameter,
        // destructured at the start of the body (`param_pats`).
        let pattern_start = matches!(self.peek(), TokenKind::LParen)
            || matches!(self.peek(), TokenKind::Ident(_))
                && matches!(
                    self.peek_next(),
                    Some(TokenKind::LBrace | TokenKind::ColonColon)
                );
        let name = if pattern_start {
            let pat = self.with_struct_expr(true, Self::parse_pattern_atom)?;
            if by_ref {
                self.error_help(
                    Code::SYNTAX,
                    start,
                    "a `ref` parameter is a name, not a pattern",
                    "write `ref p: T` and destructure `p` in the body",
                );
            }
            let name = self.fresh_destructure_temp();
            self.param_pats.push((name.clone(), pat));
            name
        } else if let TokenKind::Ident(n) = self.peek().clone() {
            self.bump();
            n
        } else {
            let s = self.peek_span();
            self.expected_name(s, "parameter name or pattern");
            return None;
        };
        // `xs: ...T` - `...` precedes the element type; the
        // param is marked `variadic` (its type becomes `Slice<T>`).
        let (ty, variadic) = if self.eat(&TokenKind::Colon) {
            if self.eat(&TokenKind::DotDot) && self.eat(&TokenKind::Dot) {
                // `...T` - three dots. Lexed as `..` then `.`.
                let elem = self.parse_type()?;
                (Some(elem), true)
            } else if matches!(self.peek(), TokenKind::Amp | TokenKind::AmpAmp) {
                let (ty, is_mut) = self.rust_ref_type(Some(&name))?;
                by_ref |= is_mut;
                (Some(ty), false)
            } else if self.at_some_type() {
                // `x: some Tr`: an anonymous type parameter.
                (Some(self.parse_some_type()?), false)
            } else {
                (Some(self.parse_type()?), false)
            }
        } else {
            (None, false)
        };
        let default = if self.eat(&TokenKind::Eq) {
            let value = self.with_struct_expr(true, Self::parse_expr)?;
            Some(value)
        } else {
            None
        };
        Some(Param {
            name,
            ty,
            by_ref,
            default,
            variadic,
            span: start,
        })
    }
}

/// The types a const parameter may have.
pub(crate) fn const_param_ty(ty: &Ty) -> bool {
    let TyKind::Path(p) = &ty.kind else {
        return false;
    };
    match p.segments.as_slice() {
        [s] if s.generics.is_empty() => matches!(
            s.name.as_str(),
            "i8" | "i16"
                | "i32"
                | "i64"
                | "isize"
                | "u8"
                | "u16"
                | "u32"
                | "u64"
                | "usize"
                | "bool"
                | "char"
                | "c_char"
        ),
        // A path (`ffi::c_char`) or an alias is left to the type check.
        [..] => p.segments.len() > 1,
    }
}

/// `fn f(g: fn(T) throws -> U) rethrows -> R` -> `fn f<#E0>(g: fn(T)
/// throws (#E0) -> U) throws (#E0) -> R`; monomorphization binds `#E0` to
/// the argument's error, or none. Returns the error types.
pub(crate) fn desugar_rethrows(params: &mut [Param], generics: &mut Generics) -> Vec<Ty> {
    fn walk(ty: &mut Ty, generics: &mut Generics, errs: &mut Vec<Ty>) {
        if nabic_ast::is_untyped_throws(ty) {
            let name = format!("#E{}", errs.len());
            *ty = path_ty(&name, ty.span);
            generics.params.push(nabic_ast::GenericParam::Type {
                name,
                bounds: Vec::new(),
                default: None,
            });
            errs.push(ty.clone());
            return;
        }
        match &mut ty.kind {
            TyKind::Path(p) => {
                for seg in &mut p.segments {
                    for g in &mut seg.generics {
                        walk(g, generics, errs);
                    }
                }
            }
            TyKind::QPath(q) => walk(&mut q.self_ty, generics, errs),
            TyKind::Sum(ts) | TyKind::Tuple(ts) | TyKind::Some(ts) => {
                for t in ts {
                    walk(t, generics, errs);
                }
            }
            TyKind::Fn(args, ret, _) => {
                for t in args {
                    walk(t, generics, errs);
                }
                walk(ret, generics, errs);
            }
            TyKind::Throws { ok, err } => {
                walk(ok, generics, errs);
                walk(err, generics, errs);
            }
            TyKind::Array(t, _) | TyKind::Nest(t) | TyKind::Binding(_, t) => {
                walk(t, generics, errs);
            }
            TyKind::Const(_)
            | TyKind::AnyTrait(_)
            | TyKind::Any
            | TyKind::None
            | TyKind::SelfUpper
            | TyKind::Never => {}
        }
    }
    let mut errs = Vec::new();
    for p in params {
        if let Some(t) = &mut p.ty {
            walk(t, generics, &mut errs);
        }
    }
    errs
}
