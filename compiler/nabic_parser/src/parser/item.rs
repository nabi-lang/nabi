//! Items: structs, unions, enums, aliases, consts, statics, extern blocks.

#[allow(clippy::wildcard_imports)]
use crate::parser::*;

impl Parser<'_> {
    /// At `name!( .. )` for a registered macro: an item at module level (it
    /// may expand to items).
    pub(crate) fn at_item_macro_call(&self) -> bool {
        self.at_macro_rules()
            || !self.meta.is_empty()
                && self
                    .macro_head()
                    .is_some_and(|(q, n, _)| !q.is_empty() || self.meta.find_macro(&n).is_some())
    }

    /// An item rather than a top-level statement? `unsafe` / `async` /
    /// `yield` start one only before `fn`, `impl`, `trait` or another of them.
    pub(crate) fn at_item_start(&self) -> bool {
        match self.peek() {
            TokenKind::Unsafe | TokenKind::Async | TokenKind::Yield => matches!(
                self.peek_next(),
                Some(
                    TokenKind::Fn
                        | TokenKind::Impl
                        | TokenKind::Trait
                        | TokenKind::Unsafe
                        | TokenKind::Async
                        | TokenKind::Yield
                )
            ),
            _ if self.at_union_item() || self.at_ctx_item() || self.at_wit_item() => true,
            k => starts_item(k),
        }
    }

    /// `extern "C" {` / `extern {`, `const NAME` / `const fn`,
    /// `static [mut] NAME`, `type Name`: contextual at item start.
    pub(crate) fn at_ctx_item(&self) -> bool {
        let next = self.peek_next();
        let TokenKind::Ident(k) = self.peek() else {
            return false;
        };
        if self.peek_raw() {
            return false;
        }
        match k.as_str() {
            "extern" => matches!(next, Some(TokenKind::StrLit(_) | TokenKind::LBrace)),
            "const" => matches!(
                next,
                Some(TokenKind::Ident(_) | TokenKind::Fn | TokenKind::Unsafe)
            ),
            "static" | "type" => matches!(next, Some(TokenKind::Ident(_))),
            _ => false,
        }
    }

    /// `union` is an item keyword only as `union Name` at item
    /// start (`Set::union`, `a.union(b)` are identifiers).
    pub(crate) fn at_union_item(&self) -> bool {
        self.at_kw("union") && matches!(self.peek_next(), Some(TokenKind::Ident(_)))
    }

    /// Top-level statement of the entry file. Block-like
    /// expressions need no `;`; no tail expression. A bare `return;`
    /// ends the program (exit code 0); `return v;` is an error.
    pub(crate) fn parse_top_stmt(&mut self) -> Option<Stmt> {
        let (stmt, _) = self.parse_stmt()?;
        match stmt.kind {
            StmtKind::Trailing(expr) if !ends_stmt(&expr) => {
                let d = self.expected(&TokenKind::Semi, "`;` after top-level expression");
                self.diagnostics.push(d);
                None
            }
            // Kept `Trailing`: a block-like statement must still be `()`
            // (E0067), as in a function body.
            _ => Some(stmt),
        }
    }
}

impl Parser<'_> {
    pub(crate) fn parse_item(&mut self) -> Option<Item> {
        let lifted = self.lifted.len();
        let item = self.parse_item_lifting();
        let nested: Vec<Item> = self.lifted.drain(lifted..).collect();
        self.pending_items.extend(nested);
        item
    }

    pub(crate) fn parse_item_lifting(&mut self) -> Option<Item> {
        let span = self.peek_span();
        let from = self.pos;
        if self.rust_macro_rules() {
            self.vanished = true;
            return None;
        }
        // A `name!(..)` macro at item position expands to items.
        if let Some(items) = self.macro_call_items() {
            let mut it = items.into_iter();
            let first = it.next();
            self.pending_items.extend(it);
            self.vanished = first.is_none();
            return first;
        }
        let item = self.parse_item_inner()?;
        // `pkg` / `sup` / `nabi` are path roots and `any` is the any
        // type, so no module or type may take those names.
        let name = match &item {
            Item::Struct(d) => Some(&d.name),
            Item::Enum(d) => Some(&d.name),
            Item::Union(d) => Some(&d.name),
            Item::Trait(d) => Some(&d.name),
            Item::TypeAlias(d) => Some(&d.name),
            Item::Mod(d) => Some(&d.name),
            _ => None,
        };
        if let Some(n) = name.filter(|n| matches!(n.as_str(), "pkg" | "sup" | "any")) {
            let what = if matches!(item, Item::Mod(_)) {
                "module"
            } else {
                "type"
            };
            self.error(
                Code::RESERVED_NAME,
                span,
                format!(
                    "`{n}` cannot name {} {what}: it is reserved as a path root / type keyword",
                    nabic_diagnostic::a_or_an(what)
                ),
            );
        }
        // Cfg runs before meta expansion; a cfg-false item
        // expands nothing and its cfg-false members are already gone.
        if self.cfg_off(item.attrs()) {
            return Some(item);
        }
        if !self.meta.is_empty() {
            // A user attribute replaces the item with its
            // expansion (which expands again).
            if let Some(items) = self.expand_user_attribute(&item, from) {
                let mut it = items.into_iter();
                let first = it.next();
                self.pending_items.extend(it);
                self.vanished = first.is_none();
                return first;
            }
        }
        let (item, user_derived) = if self.meta.is_empty() {
            (item, Vec::new())
        } else {
            self.expand_user_derives(item, from)
        };
        let (derived, diags) = crate::derive::derive_items(&item, &self.file);
        self.pending_items.extend(derived);
        self.pending_items.extend(user_derived);
        self.diagnostics.extend(diags);
        Some(item)
    }

    pub(crate) fn parse_item_inner(&mut self) -> Option<Item> {
        let (attrs, vis) = self.parse_attrs_vis();
        if self.at_union_item() {
            return self.parse_union(vis, attrs).map(Item::Union);
        }
        // Contextual item keywords: `extern "C"`, `const NAME` /
        // `const fn`, `static [mut] NAME`, `type Name`.
        if self.at_wit_item() {
            return self.parse_wit_item(attrs).map(Item::Extern);
        }
        if self.at_kw("extern") {
            // `extern "C" fn f(..) { .. }`: a Nabi function
            // with an external ABI, marked with it for HIR.
            let after_abi = self.tokens.get(self.pos + 2).map(|t| &t.kind);
            if let (Some(TokenKind::StrLit(abi)), Some(TokenKind::Fn | TokenKind::Unsafe)) =
                (self.peek_next(), after_abi)
            {
                let span = self.peek_span();
                self.bump(); // `extern`
                self.bump(); // ABI string
                let mut attrs = attrs;
                attrs.push(Attribute {
                    path: vec![nabic_ast::EXTERN_FN_ATTR.to_owned()],
                    body: nabic_ast::AttrBody::Value(nabic_ast::Literal::Str(abi)),
                    inner: false,
                    span,
                });
                return self.parse_fn(vis, attrs).map(Item::Fn);
            }
            return self.parse_extern_block(attrs).map(Item::Extern);
        }
        if self.at_kw("const") {
            if self.at_const_fn() {
                return self.parse_const_fn(vis, attrs).map(Item::Fn);
            }
            return self.parse_const(vis, attrs).map(Item::Const);
        }
        if self.at_kw("static") {
            return self.parse_static(vis, attrs).map(Item::Static);
        }
        if self.at_kw("type") {
            return self.parse_type_alias(vis, attrs).map(Item::TypeAlias);
        }
        match self.peek() {
            TokenKind::Unsafe if self.peek_next() == Some(TokenKind::Trait) => {
                self.bump();
                self.parse_trait(vis, attrs).map(|t| {
                    Item::Trait(TraitDef {
                        is_unsafe: true,
                        ..t
                    })
                })
            }
            TokenKind::Unsafe if self.peek_next() == Some(TokenKind::Impl) => {
                self.bump();
                self.parse_impl(vis, attrs).map(|i| {
                    Item::Impl(ImplDef {
                        is_unsafe: true,
                        ..i
                    })
                })
            }
            TokenKind::Fn | TokenKind::Unsafe | TokenKind::Async | TokenKind::Yield => {
                self.parse_fn(vis, attrs).map(Item::Fn)
            }
            TokenKind::Trait => self.parse_trait(vis, attrs).map(Item::Trait),
            TokenKind::Struct => self.parse_struct(vis, attrs).map(Item::Struct),
            TokenKind::Enum => self.parse_enum(vis, attrs).map(Item::Enum),
            TokenKind::Impl => self.parse_impl(vis, attrs).map(Item::Impl),
            TokenKind::Mod => self.parse_mod(vis, attrs).map(Item::Mod),
            TokenKind::Use => self.parse_use(vis, attrs).map(Item::Use),
            _ => {
                let span = self.peek_span();
                self.error(Code::SYNTAX, span, "expected an item");
                None
            }
        }
    }

    /// `type NAME[<G>] = ty;`
    pub(crate) fn parse_type_alias(
        &mut self,
        vis: Visibility,
        attrs: Vec<Attribute>,
    ) -> Option<nabic_ast::TypeAliasDef> {
        let start = self.peek_span();
        self.bump(); // contextual `type`
        let name = if let TokenKind::Ident(n) = self.peek().clone() {
            self.bump();
            n
        } else {
            let s = self.peek_span();
            self.error(
                Code::SYNTAX,
                s,
                format!("expected type alias name, found {}", self.peek().describe()),
            );
            return None;
        };
        let generics = self.parse_generic_params();
        let mut where_clause = self.parse_where_clause();
        self.expect(&TokenKind::Eq, "`=`")?;
        // `type X = some Tr;`: a named opaque type.
        let ty = if self.at_some_type() {
            self.parse_some_type()?
        } else {
            self.parse_type()?
        };
        // `type A<T> = B<T> where T: X;` reads the same.
        where_clause.extend(self.parse_where_clause());
        self.expect(&TokenKind::Semi, "`;`")?;
        Some(nabic_ast::TypeAliasDef {
            vis,
            attrs,
            name,
            generics,
            ty,
            where_clause,
            span: start,
        })
    }

    /// `-> T throws` (Swift, Java order): `throws` after the return type
    /// `ret` of a signature from `start` to its `)` at `rparen`.
    pub(crate) fn throws_after_ret(
        &mut self,
        throws: ThrowsKind,
        ret: Option<&Ty>,
        start: Span,
        rparen: Option<Span>,
    ) -> ThrowsKind {
        let Some(ret) = ret.filter(|_| matches!(throws, ThrowsKind::None) && self.at_kw("throws"))
        else {
            return throws;
        };
        let at = self.peek_span();
        let ty = self.text_in(ret.span);
        let head = self.text_in(Span {
            end: rparen.map_or(start.end, |p| p.end),
            ..start
        });
        let throws = self.parse_throws();
        let clause = self.text_in(Span {
            end: self.prev_span().map_or(at.end, |p| p.end),
            ..at
        });
        self.error_help(
            Code::MODIFIER_ORDER,
            at,
            "`throws` goes before the return type",
            format!("write `{head} {clause} -> {ty}`"),
        );
        throws
    }

    /// `const NAME: T = expr;`
    pub(crate) fn parse_const(
        &mut self,
        vis: Visibility,
        attrs: Vec<Attribute>,
    ) -> Option<nabic_ast::ConstDef> {
        let start = self.peek_span();
        self.bump(); // contextual `const`
        let name = if let TokenKind::Ident(n) = self.peek().clone() {
            self.bump();
            n
        } else {
            let s = self.peek_span();
            self.error(
                Code::SYNTAX,
                s,
                format!("expected const name, found {}", self.peek().describe()),
            );
            return None;
        };
        self.expect(&TokenKind::Colon, "`:`")?;
        let ty = self.parse_type()?;
        self.expect(&TokenKind::Eq, "`=`")?;
        let value = self.parse_expr()?;
        self.expect(&TokenKind::Semi, "`;`")?;
        Some(nabic_ast::ConstDef {
            vis,
            attrs,
            name,
            ty,
            value,
            span: start,
        })
    }

    /// `static [mut] NAME: T = expr;` - a module-level global.
    pub(crate) fn parse_static(
        &mut self,
        vis: Visibility,
        attrs: Vec<Attribute>,
    ) -> Option<nabic_ast::StaticDef> {
        let start = self.peek_span();
        self.bump(); // contextual `static`
        let is_mut = matches!(self.peek_next(), Some(TokenKind::Ident(_))) && self.eat_kw("mut");
        // `static mut` only inside `extern "C" { .. }` (FFI),
        // which doesn't come through here.
        if is_mut {
            let mut_span = self.peek_span();
            self.error_help(
                Code::MISPLACED_ITEM,
                mut_span,
                "`static mut` is only allowed inside `extern \"C\" { ... }` (FFI)",
                "a static is shared by every thread: hold the value in an `Atomic` or a \
                 `Mutex`, as in `static N: Atomic<i32> = Atomic::new(0);`",
            );
        }
        let name = if let TokenKind::Ident(n) = self.peek().clone() {
            self.bump();
            n
        } else {
            let s = self.peek_span();
            self.error(
                Code::SYNTAX,
                s,
                format!("expected static name, found {}", self.peek().describe()),
            );
            return None;
        };
        self.expect(&TokenKind::Colon, "`:`")?;
        let ty = self.parse_type()?;
        let value = if self.eat(&TokenKind::Eq) {
            Some(self.parse_expr()?)
        } else {
            None
        };
        self.expect(&TokenKind::Semi, "`;`")?;
        Some(nabic_ast::StaticDef {
            vis,
            attrs,
            name,
            ty,
            value,
            is_mut,
            span: start,
        })
    }

    pub(crate) fn parse_struct(
        &mut self,
        vis: Visibility,
        attrs: Vec<Attribute>,
    ) -> Option<StructDef> {
        let start = self.peek_span();
        self.expect(&TokenKind::Struct, "`struct`")?;
        let name = if let TokenKind::Ident(n) = self.peek().clone() {
            self.bump();
            n
        } else {
            let s = self.peek_span();
            self.error(
                Code::SYNTAX,
                s,
                format!("expected struct name, found {}", self.peek().describe()),
            );
            return None;
        };
        let generics = if matches!(self.peek(), TokenKind::Lt) {
            self.parse_generic_params()
        } else {
            Generics::default()
        };
        let mut where_clause = self.parse_where_clause();

        let kind = match self.peek().clone() {
            TokenKind::Semi => {
                self.bump();
                StructKind::Unit
            }
            TokenKind::LParen => {
                self.bump();
                let mut fields = Vec::new();
                while !matches!(self.peek(), TokenKind::RParen | TokenKind::Eof) {
                    let (f_attrs, f_vis) = self.parse_attrs_vis();
                    let span = self.peek_span();
                    let ty = self.parse_type()?;
                    if self.cfg_keeps(&f_attrs) {
                        fields.push(nabic_ast::TupleField {
                            vis: f_vis,
                            attrs: f_attrs,
                            ty,
                            span,
                        });
                    }
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                }
                self.expect(&TokenKind::RParen, "`)`")?;
                // `struct P<T>(T) where T: X;` reads the same.
                where_clause.extend(self.parse_where_clause());
                self.expect(&TokenKind::Semi, "`;`")?;
                StructKind::Tuple(fields)
            }
            _ => {
                let fields = self.parse_named_fields(true)?;
                StructKind::Named(fields)
            }
        };

        Some(StructDef {
            vis,
            attrs,
            name,
            generics,
            kind,
            where_clause,
            span: start,
        })
    }

    /// `union Name<G> { f: T; g: U; }` - fields share storage.
    pub(crate) fn parse_union(
        &mut self,
        vis: Visibility,
        attrs: Vec<Attribute>,
    ) -> Option<UnionDef> {
        let start = self.peek_span();
        self.bump(); // contextual `union`
        let name = if let TokenKind::Ident(n) = self.peek().clone() {
            self.bump();
            n
        } else {
            let s = self.peek_span();
            self.error(
                Code::SYNTAX,
                s,
                format!("expected union name, found {}", self.peek().describe()),
            );
            return None;
        };
        let generics = if matches!(self.peek(), TokenKind::Lt) {
            self.parse_generic_params()
        } else {
            Generics::default()
        };
        let where_clause = self.parse_where_clause();
        let fields = self.parse_named_fields(false)?;
        Some(UnionDef {
            vis,
            attrs,
            name,
            generics,
            fields,
            where_clause,
            span: start,
        })
    }

    /// `{ [attrs] [vis] name: T [= default]; ... }` - struct, union and
    /// enum-variant bodies. Defaults only where `allow_default`.
    pub(crate) fn parse_named_fields(&mut self, allow_default: bool) -> Option<Vec<NamedField>> {
        self.expect(&TokenKind::LBrace, "`{`")?;
        let mut fields = Vec::new();
        let mut comma_reported = false;
        while !matches!(self.peek(), TokenKind::RBrace | TokenKind::Eof) {
            let (attrs, vis) = self.parse_attrs_vis();
            let span = self.peek_span();
            let Some(name) = self.peek_name() else {
                self.error(
                    Code::SYNTAX,
                    span,
                    format!("expected field name, found {}", self.peek().describe()),
                );
                return None;
            };
            self.bump_name();
            self.expect(&TokenKind::Colon, "`:`")?;
            let ty = self.parse_type()?;
            let default = if allow_default && self.eat(&TokenKind::Eq) {
                Some(self.parse_expr()?)
            } else {
                None
            };
            if self.cfg_keeps(&attrs) {
                fields.push(NamedField {
                    vis,
                    attrs,
                    name,
                    ty,
                    default,
                    span,
                });
            }
            if self.eat(&TokenKind::Semi) {
                continue;
            }
            // `x: i32, y: i32` (Rust, Go): fields end with `;`.
            if matches!(self.peek(), TokenKind::Comma) {
                let at = self.peek_span();
                if !comma_reported {
                    let field = fields.last().map_or_else(String::new, |f: &NamedField| {
                        // To the `,`: `List<T>` and a default too.
                        format!("{}: {};", f.name, self.text_in(self.to_prev(f.ty.span)))
                    });
                    self.error_help(
                        Code::SYNTAX,
                        at,
                        "expected `;` after a struct field, found `,`",
                        format!("struct fields end with `;`: `{field}`"),
                    );
                    comma_reported = true;
                }
                self.bump();
                continue;
            }
            break;
        }
        self.expect(&TokenKind::RBrace, "`}`")?;
        Some(fields)
    }
}

impl Parser<'_> {
    /// `enum Name { V, V(T), V { x: T }, .. }`.
    pub(crate) fn parse_enum(&mut self, vis: Visibility, attrs: Vec<Attribute>) -> Option<EnumDef> {
        let start = self.peek_span();
        self.expect(&TokenKind::Enum, "`enum`")?;
        let name = if let TokenKind::Ident(n) = self.peek().clone() {
            self.bump();
            n
        } else {
            let s = self.peek_span();
            self.error(
                Code::SYNTAX,
                s,
                format!("expected enum name, found {}", self.peek().describe()),
            );
            return None;
        };
        let generics = if matches!(self.peek(), TokenKind::Lt) {
            self.parse_generic_params()
        } else {
            Generics::default()
        };
        let where_clause = self.parse_where_clause();
        self.expect(&TokenKind::LBrace, "`{`")?;

        let mut variants = Vec::new();
        while !matches!(self.peek(), TokenKind::RBrace | TokenKind::Eof) {
            let v_attrs = self.parse_outer_attrs();
            // Variants take the enum's visibility.
            if matches!(self.peek(), TokenKind::Pub) {
                let s = self.peek_span();
                self.error(
                    Code::MISPLACED_VISIBILITY,
                    s,
                    "enum variants take the enum's visibility; remove the visibility \
                     keyword",
                );
                self.parse_visibility();
            }
            let v_span = self.peek_span();
            let v_name = if let TokenKind::Ident(n) = self.peek().clone() {
                self.bump();
                n
            } else {
                let s = self.peek_span();
                self.error(
                    Code::SYNTAX,
                    s,
                    format!("expected variant name, found {}", self.peek().describe()),
                );
                return None;
            };
            let kind = match self.peek() {
                TokenKind::LParen => {
                    self.bump();
                    let mut tys = Vec::new();
                    while !matches!(self.peek(), TokenKind::RParen | TokenKind::Eof) {
                        let f_attrs = self.cfg_only_attrs("a tuple variant field");
                        if let Some(t) = self.parse_type().filter(|_| !self.cfg_off(&f_attrs)) {
                            tys.push(t);
                        }
                        if !self.eat(&TokenKind::Comma) {
                            break;
                        }
                    }
                    self.expect(&TokenKind::RParen, "`)`")?;
                    EnumVariantKind::Tuple(tys)
                }
                TokenKind::LBrace => {
                    let fields = self.parse_named_fields(false)?;
                    EnumVariantKind::Named(fields)
                }
                TokenKind::Eq => {
                    self.bump();
                    let value = self.parse_expr()?;
                    EnumVariantKind::Value(value)
                }
                _ => EnumVariantKind::Unit,
            };
            if self.cfg_keeps(&v_attrs) {
                variants.push(EnumVariant {
                    attrs: v_attrs,
                    name: v_name,
                    kind,
                    span: v_span,
                });
            }
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        self.expect(&TokenKind::RBrace, "`}`")?;

        Some(EnumDef {
            vis,
            attrs,
            name,
            generics,
            variants,
            where_clause,
            span: start,
        })
    }

    /// `extern "ABI" { fn name(args) -> ret; ... }`; a missing ABI is
    /// reported and read as "C".
    pub(crate) fn parse_extern_block(&mut self, attrs: Vec<Attribute>) -> Option<ExternBlock> {
        let start = self.peek_span();
        self.bump(); // contextual `extern`
        let abi = if let TokenKind::StrLit(s) = self.peek().clone() {
            self.bump();
            s
        } else {
            self.error(
                Code::INVALID_ABI,
                start,
                "an `extern` block names its ABI: `extern \"C\" { .. }`",
            );
            "C".to_owned()
        };
        self.expect(&TokenKind::LBrace, "`{`")?;
        let mut items = Vec::new();
        while !matches!(self.peek(), TokenKind::RBrace | TokenKind::Eof) {
            let (item_attrs, item_vis) = self.parse_attrs_vis();
            let (on, before) = (self.cfg_keeps(&item_attrs), items.len());
            match self.peek() {
                TokenKind::Fn | TokenKind::Unsafe => {
                    let from = self.pos;
                    if let Some(f) = self.parse_fn(item_vis, item_attrs) {
                        items.push(ExternItem::Fn(f));
                    } else {
                        self.recover(from, Recover::NestedItem);
                    }
                }
                // `static mut? name: T;` defined outside.
                TokenKind::Ident(k) if k == "static" => {
                    let from = self.pos;
                    let span = self.peek_span();
                    self.bump();
                    let is_mut =
                        matches!(self.peek_next(), Some(TokenKind::Ident(_))) && self.eat_kw("mut");
                    let name = match self.peek().clone() {
                        TokenKind::Ident(n) => {
                            self.bump();
                            Some(n)
                        }
                        _ => None,
                    };
                    let ty = name.as_ref().and_then(|_| {
                        self.expect(&TokenKind::Colon, "`:`")?;
                        let ty = self.parse_type()?;
                        self.expect(&TokenKind::Semi, "`;`")?;
                        Some(ty)
                    });
                    if let (Some(name), Some(ty)) = (name, ty) {
                        items.push(ExternItem::Static(nabic_ast::StaticDef {
                            vis: item_vis,
                            attrs: item_attrs,
                            name,
                            ty,
                            value: None,
                            is_mut,
                            span,
                        }));
                    } else {
                        self.error(
                            Code::SYNTAX,
                            span,
                            "expected `static name: Type;` in `extern` block",
                        );
                        self.recover(from, Recover::NestedItem);
                    }
                }
                _ => {
                    let s = self.peek_span();
                    self.error(
                        Code::SYNTAX,
                        s,
                        format!("expected extern item, found {}", self.peek().describe()),
                    );
                    let from = self.pos;
                    self.recover(from, Recover::NestedItem);
                }
            }
            if !on {
                items.truncate(before);
            }
        }
        self.expect(&TokenKind::RBrace, "`}`")?;
        Some(ExternBlock {
            attrs,
            abi,
            items,
            span: start,
            wit: None,
            headers: None,
        })
    }

    /// `ffi::wit::import` / `ffi::wit::export` / `ffi::objc::import`
    /// then `{`.
    pub(crate) fn at_wit_item(&self) -> bool {
        let k = |n: usize| self.tokens.get(self.pos + n).map(|t| &t.kind);
        let word = |n: usize, w: &str| matches!(k(n), Some(TokenKind::Ident(x)) if x == w);
        word(0, "ffi")
            && k(1) == Some(&TokenKind::ColonColon)
            && (word(2, "wit") && (word(4, "import") || word(4, "export"))
                || word(2, "objc") && word(4, "import"))
            && k(3) == Some(&TokenKind::ColonColon)
            && k(5) == Some(&TokenKind::LBrace)
    }

    /// `ffi::wit::import { "a.wit", "dir" }`: string paths, commas
    /// between.
    pub(crate) fn parse_wit_item(&mut self, attrs: Vec<Attribute>) -> Option<ExternBlock> {
        let start = self.peek_span();
        let objc = matches!(self.tokens.get(self.pos + 2).map(|t| &t.kind), Some(TokenKind::Ident(x)) if x == "objc");
        for _ in 0..4 {
            self.bump();
        }
        let export = matches!(self.peek(), TokenKind::Ident(w) if w == "export");
        self.bump();
        self.expect(&TokenKind::LBrace, "`{`")?;
        let mut paths = Vec::new();
        while !matches!(self.peek(), TokenKind::RBrace | TokenKind::Eof) {
            let TokenKind::StrLit(p) = self.peek().clone() else {
                let s = self.peek_span();
                self.error(
                    Code::SYNTAX,
                    s,
                    if objc {
                        format!(
                            "expected a header path string (`\"AppKit/AppKit.h\"`), found {}",
                            self.peek().describe()
                        )
                    } else {
                        format!(
                            "expected a `.wit` path string, found {}",
                            self.peek().describe()
                        )
                    },
                );
                return None;
            };
            self.bump();
            paths.push(p);
            if !matches!(self.peek(), TokenKind::RBrace) {
                self.expect(&TokenKind::Comma, "`,`")?;
            }
        }
        self.expect(&TokenKind::RBrace, "`}`")?;
        if objc {
            return Some(ExternBlock {
                attrs,
                abi: "objc".to_owned(),
                items: Vec::new(),
                span: start,
                wit: None,
                headers: Some(paths),
            });
        }
        Some(ExternBlock {
            attrs,
            abi: "C".to_owned(),
            items: Vec::new(),
            span: start,
            wit: Some(nabic_ast::WitRef { export, paths }),
            headers: None,
        })
    }
}
