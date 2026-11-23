//! Impl blocks and traits.

#[allow(clippy::wildcard_imports)]
use crate::parser::*;

impl Parser<'_> {
    /// `impl [Trait for] Type { .. }`: an inherent or trait impl.
    pub(crate) fn parse_impl(&mut self, vis: Visibility, attrs: Vec<Attribute>) -> Option<ImplDef> {
        let start = self.peek_span();
        self.expect(&TokenKind::Impl, "`impl`")?;
        // `<` opens generics only before a parameter (`<>`, `<T>`, `<T:`,
        // `<T,`, `<T =`, `<const N`); else it starts a projection
        // (`impl <A as P>::Out { .. }`).
        let tok = |i: usize| self.tokens.get(self.pos + i).map(|t| &t.kind);
        let params = match (tok(1), tok(2)) {
            // `impl<'a>`: a lifetime, reported with the parameters.
            (Some(TokenKind::Gt | TokenKind::Label(_)), _)
            | (
                Some(TokenKind::Ident(_)),
                Some(TokenKind::Colon | TokenKind::Comma | TokenKind::Gt | TokenKind::Eq),
            ) => true,
            (Some(TokenKind::Ident(k)), Some(TokenKind::Ident(_))) => k == "const",
            _ => false,
        };
        let generics = if matches!(self.peek(), TokenKind::Lt) && !params {
            Generics::default()
        } else {
            self.parse_generic_params()
        };
        // `impl !Send for T {}` - auto-trait opt-out.
        let negative = self.eat(&TokenKind::Bang);
        let first_ty = self.parse_type()?;
        if negative && !matches!(self.peek(), TokenKind::For) {
            let s = self.peek_span();
            self.error(Code::SYNTAX, s, "`impl !Trait` needs `for Type`");
        }
        let (trait_path, self_ty) = if self.eat(&TokenKind::For) {
            // `impl TRAIT for SELF` - the first parsed type was the trait.
            let trait_path = match &first_ty.kind {
                TyKind::Path(p) => Some(p.clone()),
                _ => None,
            };
            let self_ty = self.parse_type()?;
            (trait_path, self_ty)
        } else {
            (None, first_ty)
        };
        let where_clause = self.parse_where_clause();
        self.expect(&TokenKind::LBrace, "`{`")?;
        let owner = match &self_ty.kind {
            TyKind::Path(p) => p.segments.last().map(|s| s.name.clone()),
            _ => None,
        };
        let outer_owner = std::mem::replace(&mut self.fn_owner, owner);
        let items = self.parse_impl_items();
        self.fn_owner = outer_owner;
        if trait_path.is_some() {
            for it in items.iter().flatten() {
                let (vis, at) = match it {
                    ImplItem::Method(m) => (m.vis, m.span),
                    ImplItem::Const(c) => (c.vis, c.span),
                    ImplItem::AssocType { .. } => continue,
                };
                if vis != Visibility::Private {
                    self.error_help(
                        Code::MISPLACED_VISIBILITY,
                        at,
                        "a trait impl's item has no visibility of its own: it is as visible \
                         as the trait",
                        "remove `pub`",
                    );
                }
            }
        }
        let mut attrs = attrs;
        attrs.append(&mut self.cfg_off);
        let items = items?;
        Some(ImplDef {
            vis,
            attrs,
            generics,
            trait_path,
            is_unsafe: false,
            negative,
            self_ty,
            where_clause,
            items,
            module: String::new(),
            span: start,
        })
    }

    /// An impl's items through its `}`.
    pub(crate) fn parse_impl_items(&mut self) -> Option<Vec<ImplItem>> {
        let mut items = Vec::new();
        while !matches!(self.peek(), TokenKind::RBrace | TokenKind::Eof) {
            let attrs_from = self.pos;
            let (item_attrs, item_vis) = self.parse_attrs_vis();
            let (on, before) = (self.cfg_keeps(&item_attrs), items.len());
            match self.peek() {
                TokenKind::Fn | TokenKind::Unsafe | TokenKind::Async | TokenKind::Yield => {
                    let from = self.pos;
                    if let Some(f) = self.parse_fn(item_vis, item_attrs) {
                        let fs = if on {
                            self.expand_method(f, attrs_from)
                        } else {
                            self.cfg_off.push(cfg_off_marker(&f));
                            Vec::new()
                        };
                        items.extend(fs.into_iter().map(ImplItem::Method));
                    } else {
                        self.recover(from, Recover::NestedItem);
                    }
                }
                // `impl_item ::= fn_decl | "type" ident "=" ty ";" | const_decl`.
                // Associated-type concrete binding: `type Output = V;`.
                TokenKind::Ident(k) if k == "type" => {
                    let _ = (item_vis, item_attrs);
                    self.bump();
                    let assoc_name = if let TokenKind::Ident(n) = self.peek().clone() {
                        self.bump();
                        n
                    } else {
                        let s = self.peek_span();
                        self.error(Code::SYNTAX, s, "associated-type name expected");
                        String::new()
                    };
                    self.expect(&TokenKind::Eq, "`=`")?;
                    let aty = self.parse_type()?;
                    self.expect(&TokenKind::Semi, "`;`")?;
                    items.push(ImplItem::AssocType {
                        name: assoc_name,
                        ty: aty,
                    });
                }
                TokenKind::Ident(k) if k == "const" && self.at_const_fn() => {
                    let from = self.pos;
                    if let Some(f) = self.parse_const_fn(item_vis, item_attrs) {
                        let fs = if on {
                            self.expand_method(f, attrs_from)
                        } else {
                            Vec::new()
                        };
                        items.extend(fs.into_iter().map(ImplItem::Method));
                    } else {
                        self.recover(from, Recover::NestedItem);
                    }
                }
                // `impl_item ::= .. | const_decl`.
                TokenKind::Ident(k) if k == "const" => {
                    let from = self.pos;
                    match self.parse_const(item_vis, item_attrs) {
                        Some(c) => items.push(ImplItem::Const(c)),
                        None => self.recover(from, Recover::NestedItem),
                    }
                }
                _ => {
                    let s = self.peek_span();
                    let (code, msg) = if self.at_kw("static") && self.at_ctx_item() {
                        (
                            Code::MISPLACED_ITEM,
                            "a `static` is declared only at module level, not in an `impl`; \
                             an associated `const` goes there"
                                .to_owned(),
                        )
                    } else {
                        (
                            Code::SYNTAX,
                            format!("expected impl item, found {}", self.peek().describe()),
                        )
                    };
                    self.error(code, s, msg);
                    let from = self.pos;
                    self.recover(from, Recover::NestedItem);
                }
            }
            if !on {
                items.truncate(before);
            }
        }
        self.expect(&TokenKind::RBrace, "`}`")?;
        Some(items)
    }

    /// A method with a user attribute becomes its expansion,
    /// which must be methods.
    pub(crate) fn expand_method(&mut self, f: FnDef, from: usize) -> Vec<FnDef> {
        if self.meta.is_empty() {
            return vec![f];
        }
        let span = f.span;
        let item = Item::Fn(f);
        let Some(items) = self.expand_user_attribute(&item, from) else {
            let Item::Fn(f) = item else { unreachable!() };
            return vec![f];
        };
        let mut out = Vec::new();
        for i in items {
            match i {
                Item::Fn(f) => out.push(f),
                _ => self.error(
                    Code::INVALID_META_BLOCK,
                    span,
                    "an attribute on a method must expand to methods only",
                ),
            }
        }
        out
    }
}

impl Parser<'_> {
    pub(crate) fn parse_trait(
        &mut self,
        vis: Visibility,
        attrs: Vec<Attribute>,
    ) -> Option<TraitDef> {
        let start = self.peek_span();
        self.expect(&TokenKind::Trait, "`trait`")?;
        let name = if let TokenKind::Ident(n) = self.peek().clone() {
            self.bump();
            n
        } else {
            let s = self.peek_span();
            self.expected_name(s, "trait name");
            return None;
        };
        let generics = self.parse_generic_params();
        let supertraits = if self.eat(&TokenKind::Colon) {
            self.parse_bounds()
        } else {
            Vec::new()
        };
        let where_clause = self.parse_where_clause();
        self.expect(&TokenKind::LBrace, "`{`")?;
        let mut items = Vec::new();
        let outer_owner = self.fn_owner.replace(name.clone());
        while !matches!(self.peek(), TokenKind::RBrace | TokenKind::Eof) {
            let from = self.pos;
            if let Some((item, on)) = self.parse_trait_item() {
                match item {
                    _ if !on => {}
                    TraitItem::Method(f) => {
                        let fs = self.expand_method(f, from);
                        items.extend(fs.into_iter().map(TraitItem::Method));
                    }
                    item => items.push(item),
                }
            } else {
                self.recover(from, Recover::NestedItem);
            }
        }
        self.fn_owner = outer_owner;
        self.expect(&TokenKind::RBrace, "`}`")?;
        Some(TraitDef {
            vis,
            attrs,
            is_unsafe: false,
            name,
            generics,
            supertraits,
            items,
            where_clause,
            span: start,
        })
    }

    /// A trait item and whether its cfg keeps it.
    pub(crate) fn parse_trait_item(&mut self) -> Option<(TraitItem, bool)> {
        let at = self.peek_span();
        let (attrs, vis) = self.parse_attrs_vis();
        if vis != Visibility::Private {
            self.error_help(
                Code::MISPLACED_VISIBILITY,
                at,
                "a trait item has no visibility of its own: it is as visible as the trait",
                "remove `pub`; write `pub trait` to export the trait with its items",
            );
        }
        let on = self.cfg_keeps(&attrs);
        self.parse_trait_member(attrs, vis).map(|i| (i, on))
    }

    pub(crate) fn parse_trait_member(
        &mut self,
        attrs: Vec<Attribute>,
        vis: Visibility,
    ) -> Option<TraitItem> {
        match self.peek() {
            TokenKind::Fn | TokenKind::Unsafe | TokenKind::Async | TokenKind::Yield => {
                let f = self.parse_fn(vis, attrs)?;
                Some(TraitItem::Method(f))
            }
            TokenKind::Ident(k) if k == "const" && self.at_const_fn() => {
                self.parse_const_fn(vis, attrs).map(TraitItem::Method)
            }
            TokenKind::Ident(k) if k == "const" => {
                self.bump();
                let TokenKind::Ident(name) = self.peek().clone() else {
                    let s = self.peek_span();
                    self.expected_name(s, "associated const name");
                    return None;
                };
                self.bump();
                self.expect(&TokenKind::Colon, "`:`")?;
                let ty = self.parse_type()?;
                let default = if self.eat(&TokenKind::Eq) {
                    Some(self.parse_expr()?)
                } else {
                    None
                };
                self.expect(&TokenKind::Semi, "`;`")?;
                Some(TraitItem::Const { name, ty, default })
            }
            TokenKind::Ident(k) if k == "type" => {
                self.bump();
                let name = if let TokenKind::Ident(n) = self.peek().clone() {
                    self.bump();
                    n
                } else {
                    let s = self.peek_span();
                    self.expected_name(s, "associated type name");
                    return None;
                };
                // Optional bounds (`: A + B`) and default (`= T`).
                let mut bounds = if self.eat(&TokenKind::Colon) {
                    self.parse_bounds()
                } else {
                    Vec::new()
                };
                // `type Item where Item: Clone;`: the
                // predicates bound the associated type itself.
                for pred in self.parse_where_clause() {
                    let own = matches!(&pred.ty.kind, TyKind::Path(p)
                        if matches!(p.segments.as_slice(), [s] if s.name == name)
                            || matches!(p.segments.as_slice(),
                                [a, s] if a.name == "Self" && s.name == name));
                    if own {
                        bounds.extend(pred.bounds);
                    } else {
                        self.error(
                            Code::INVALID_GENERIC_PARAMS,
                            pred.ty.span,
                            format!(
                                "a `where` clause on associated type `{name}` can only \
                                 bound `{name}` itself"
                            ),
                        );
                    }
                }
                let default = if self.eat(&TokenKind::Eq) {
                    self.parse_type()
                } else {
                    None
                };
                self.expect(&TokenKind::Semi, "`;`")?;
                let _ = attrs;
                Some(TraitItem::AssocType {
                    name,
                    bounds,
                    default,
                })
            }
            _ => {
                let s = self.peek_span();
                self.error(
                    Code::SYNTAX,
                    s,
                    format!("expected trait item, found {}", self.peek().describe()),
                );
                None
            }
        }
    }
}
