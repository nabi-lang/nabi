//! Blocks, nested fns and local items.

#[allow(clippy::wildcard_imports)]
use crate::parser::*;

impl Parser<'_> {
    pub(crate) fn parse_block(&mut self) -> Option<Block> {
        self.nested(|p| p.with_struct_expr(true, Self::parse_block_inner))
    }

    pub(crate) fn parse_block_inner(&mut self) -> Option<Block> {
        let lifted = self.lifted.len();
        let mut block = self.parse_block_stmts()?;
        self.statement_closure_returns(&block.stmts, block.trailing.as_deref());
        self.rename_local_items(&mut block, lifted);
        // A nested fn and a `use` hold in their whole
        // block - in the bodies of the fns lifted from inside it too.
        let markers: Vec<Stmt> = block
            .stmts
            .iter()
            .filter_map(|s| match &s.kind {
                StmtKind::Item(Item::Fn(f)) => {
                    nabic_ast::nested_marker_of(f).map(|(lifted, _)| Stmt {
                        kind: StmtKind::Item(Item::Fn(nested_marker(f, lifted, true))),
                        span: s.span,
                    })
                }
                StmtKind::Item(Item::Use(u)) => {
                    let mut u = u.clone();
                    u.attrs.push(Attribute {
                        path: vec![nabic_ast::INHERITED_USE_ATTR.to_owned()],
                        body: AttrBody::Empty,
                        inner: false,
                        span: u.span,
                    });
                    Some(Stmt {
                        kind: StmtKind::Item(Item::Use(u)),
                        span: s.span,
                    })
                }
                _ => None,
            })
            .collect();
        if !markers.is_empty() {
            for item in &mut self.lifted[lifted..] {
                let bodies: Vec<&mut FnDef> = match item {
                    Item::Fn(f) => vec![f],
                    Item::Impl(i) => (i.items.iter_mut())
                        .filter_map(|ii| match ii {
                            ImplItem::Method(m) => Some(m),
                            _ => None,
                        })
                        .collect(),
                    Item::Trait(t) => (t.items.iter_mut())
                        .filter_map(|ti| match ti {
                            TraitItem::Method(m) => Some(m),
                            _ => None,
                        })
                        .collect(),
                    _ => Vec::new(),
                };
                for b in bodies.into_iter().filter_map(|f| f.body.as_mut()) {
                    b.stmts.splice(0..0, markers.iter().cloned());
                }
            }
        }
        Some(block)
    }

    /// A `fn` statement: lifted to item `outer/name`
    /// (`outer/name/2` on repeats), leaving a marker statement.
    pub(crate) fn parse_nested_fn(&mut self) -> Option<Stmt> {
        let start = self.peek_span();
        if self.parse_visibility() != Visibility::Private {
            self.error(
                Code::MISPLACED_VISIBILITY,
                start,
                "`pub` is meaningless on a nested function: it is visible only in its block",
            );
        }
        let mut def = self.parse_fn(Visibility::Private, Vec::new())?;
        let lifted = self.lift_name(&def.name);
        let marker = nested_marker(&def, &lifted, false);
        def.name = lifted;
        self.lifted.push(Item::Fn(def));
        Some(Stmt {
            kind: StmtKind::Item(Item::Fn(marker)),
            span: start,
        })
    }

    /// The lifted path of the fn being parsed (`f`, `f/g`, `Type.m`).
    pub(crate) fn lift_outer(&self) -> String {
        if self.fn_names.is_empty() {
            "main".to_owned()
        } else {
            self.fn_names.join("/")
        }
    }

    /// The unique module item name `outer/name` for `name` declared in
    /// the body of the fn being parsed (`nabic_core::local`).
    pub(crate) fn lift_name(&mut self, name: &str) -> String {
        let base = format!("{}/{name}", self.lift_outer());
        let mut lifted = base.clone();
        let mut n = 1;
        while !self.lifted_names.insert(lifted.clone()) {
            n += 1;
            lifted = format!("{base}/{n}");
        }
        lifted
    }

    /// Does a block-local item start here?
    pub(crate) fn at_local_item(&mut self) -> bool {
        let save = self.pos;
        self.pos = self.past_attrs(save);
        if self.eat(&TokenKind::Pub) && matches!(self.peek(), TokenKind::LParen) {
            self.pos += 3;
        }
        if matches!(self.peek(), TokenKind::Unsafe)
            && matches!(self.peek_next(), Some(TokenKind::Impl | TokenKind::Trait))
        {
            self.bump();
        }
        let at = matches!(
            self.peek(),
            TokenKind::Struct | TokenKind::Enum | TokenKind::Trait | TokenKind::Impl
        ) || self.at_union_item()
            || (self.at_kw("type") || self.at_kw("static")) && self.at_ctx_item()
            || self.at_kw("const") && matches!(self.peek_next(), Some(TokenKind::Ident(_)));
        self.pos = save;
        at
    }

    /// A block-local item and its expansions, each lifted to a
    /// private item named by `lift_name` with a copy left as a statement
    /// (renamed at the block's end; HIR checks it captures nothing).
    /// `None` after an error; empty when cfg drops it.
    pub(crate) fn parse_local_item(&mut self) -> Option<Vec<Stmt>> {
        let start = self.peek_span();
        let (pending, lifted) = (self.pending_items.len(), self.lifted.len());
        let first = self.parse_item_lifting();
        if first.is_none() && !std::mem::take(&mut self.vanished) {
            return None;
        }
        let items: Vec<Item> = first
            .into_iter()
            .chain(self.pending_items.drain(pending..))
            .collect();
        let attrs = items.first().map_or(&[][..], |i| i.attrs().as_slice());
        if self.cfg_drops(attrs, (self.pending_stmts.len(), lifted)) {
            return Some(Vec::new());
        }
        let mut stmts = Vec::new();
        for mut item in items {
            let span = item.span();
            let (vis, name) = match &mut item {
                Item::Struct(x) => (x.vis, Some(&mut x.name)),
                Item::Enum(x) => (x.vis, Some(&mut x.name)),
                Item::Union(x) => (x.vis, Some(&mut x.name)),
                Item::Trait(x) => (x.vis, Some(&mut x.name)),
                Item::TypeAlias(x) => (x.vis, Some(&mut x.name)),
                Item::Static(x) => (x.vis, Some(&mut x.name)),
                Item::Fn(x) => (x.vis, Some(&mut x.name)),
                Item::Const(x) => (x.vis, Some(&mut x.name)),
                Item::Impl(x) => (x.vis, None),
                Item::Mod(_) | Item::Use(_) | Item::Extern(_) => {
                    self.error(
                        Code::INVALID_META_BLOCK,
                        span,
                        "a meta expansion in a function body gives only `struct`, `enum`, \
                         `union`, `trait`, `impl`, `type`, `static`, `fn` and `const` items",
                    );
                    continue;
                }
            };
            if vis != Visibility::Private {
                self.error(
                    Code::MISPLACED_VISIBILITY,
                    start,
                    "`pub` is meaningless on an item in a function body: it is visible only \
                     in its block",
                );
            }
            if let Some(name) = name {
                *name = self.lift_name(name);
            }
            item.attrs_mut().push(Attribute {
                path: vec![nabic_ast::LOCAL_ITEM_ATTR.to_owned()],
                body: AttrBody::Args(vec![AttrArg::Lit(Literal::Str(self.lift_outer()))]),
                inner: false,
                span,
            });
            stmts.push(Stmt {
                kind: StmtKind::Item(item.clone()),
                span,
            });
            self.lifted.push(item);
        }
        Some(stmts)
    }

    /// A block's local items shadow outer names of their
    /// namespace in the whole block and in items lifted from it.
    pub(crate) fn rename_local_items(&mut self, block: &mut Block, lifted: usize) {
        let mut r = nabic_ast::rename::Renames::default();
        // Per namespace (types, values): where each name is first.
        let mut first: [nabic_core::HashMap<String, Span>; 2] = Default::default();
        for s in &block.stmts {
            let StmtKind::Item(item) = &s.kind else {
                continue;
            };
            let Some(name) = item.name().filter(|_| item.is_local()) else {
                continue;
            };
            let (ty, value) = match item {
                Item::Struct(x) => (true, !matches!(x.kind, StructKind::Named(_))),
                Item::Enum(_) | Item::Union(_) | Item::Trait(_) | Item::TypeAlias(_) => {
                    (true, false)
                }
                _ => (false, true),
            };
            let source = nabic_core::local::leaf(name).into_owned();
            let twice = ty && r.types.insert(source.clone(), name.to_owned()).is_some()
                || value && r.values.insert(source.clone(), name.to_owned()).is_some();
            let ns: [bool; 2] = (ty, value).into();
            if twice {
                let at = ns
                    .iter()
                    .zip(&first)
                    .find_map(|(in_ns, m)| m.get(&source).filter(|_| *in_ns));
                self.diagnostics.push(
                    nabic_diagnostic::duplicate(
                        Code::DUPLICATE_DEFINITION,
                        format!("`{source}` is defined twice in this block"),
                        &source,
                        s.span,
                        at.copied(),
                        matches!(item, Item::Fn(_)),
                    )
                    .with_file(self.file.clone()),
                );
            }
            for (in_ns, m) in ns.into_iter().zip(&mut first) {
                if in_ns {
                    m.entry(source.clone()).or_insert(s.span);
                }
            }
        }
        // An item and a `use` of its name in one block.
        let nested: Vec<&str> = block
            .stmts
            .iter()
            .filter_map(|s| match &s.kind {
                StmtKind::Item(Item::Fn(f)) if nabic_ast::nested_marker_of(f).is_some() => {
                    Some(f.name.as_str())
                }
                _ => None,
            })
            .collect();
        for s in &block.stmts {
            let StmtKind::Item(Item::Use(u)) = &s.kind else {
                continue;
            };
            let mut used = Vec::new();
            nabic_ast::rename::use_names(&u.tree, &mut used);
            for n in used {
                if r.types.contains_key(n) || r.values.contains_key(n) || nested.contains(&n) {
                    self.error_help(
                        Code::IMPORT_CONFLICT,
                        s.span,
                        format!("`{n}` is imported here and also defined in this block"),
                        format!("rename the import: `use ... as {n}2;`, or remove one of them"),
                    );
                }
            }
        }
        if !r.is_empty() {
            r.block(block);
            for item in &mut self.lifted[lifted..] {
                r.item(item);
            }
        }
        // The block's `use`s hold in its items too: fn bodies get copies
        // (`parse_block_inner`), the rest the paths.
        let mut uses = nabic_ast::rename::Renames {
            headers_only: true,
            ..Default::default()
        };
        // Other globs: their names are known only to HIR.
        let mut module_globs = Vec::new();
        for s in &block.stmts {
            let StmtKind::Item(Item::Use(u)) = &s.kind else {
                continue;
            };
            let (mut named, mut globs) = (Vec::new(), Vec::new());
            nabic_ast::rename::use_paths(&u.tree, &[], &mut named, &mut globs);
            // `use E::*` of a local enum: its variants.
            for mut prefix in globs {
                let variants = block.stmts.iter().find_map(|s| match &s.kind {
                    StmtKind::Item(Item::Enum(e)) if prefix == [e.name.clone()] => Some(
                        e.variants
                            .iter()
                            .map(|v| v.name.clone())
                            .collect::<Vec<_>>(),
                    ),
                    _ => None,
                });
                if let Some(vs) = variants {
                    named.extend(
                        vs.into_iter()
                            .map(|v| (v.clone(), vec![prefix[0].clone(), v])),
                    );
                    continue;
                }
                if let Some(head) = prefix.first().and_then(|h| uses.paths.get(h)) {
                    prefix.splice(0..1, head.clone());
                }
                module_globs.push(prefix.join("::"));
            }
            for (name, mut path) in named {
                // `use a::b; use b::c;`
                if let Some(head) = uses.paths.get(&path[0]).cloned() {
                    path.splice(0..1, head);
                }
                uses.paths.insert(name, path);
            }
        }
        if !uses.is_empty() {
            for item in &mut self.lifted[lifted..] {
                uses.item(item);
            }
        }
        if !module_globs.is_empty() {
            let span = block.span;
            for item in &mut self.lifted[lifted..] {
                item.attrs_mut().push(Attribute {
                    path: vec![nabic_ast::BLOCK_GLOB_ATTR.to_owned()],
                    body: AttrBody::Args(
                        (module_globs.iter())
                            .map(|g| AttrArg::Lit(Literal::Str(g.clone())))
                            .collect(),
                    ),
                    inner: false,
                    span,
                });
            }
        }
    }

    /// Does a nested `fn` (`pub`, `unsafe` / `async` / `yield`) start here?
    pub(crate) fn at_nested_fn(&self) -> bool {
        let mut i = self.pos;
        let kind = |i: usize| self.tokens.get(i).map(|t| &t.kind);
        if matches!(kind(i), Some(TokenKind::Pub)) {
            i += 1;
            if matches!(kind(i), Some(TokenKind::LParen)) {
                i += 3;
            }
        }
        while matches!(
            kind(i),
            Some(TokenKind::Unsafe | TokenKind::Async | TokenKind::Yield)
        ) {
            i += 1;
        }
        matches!(kind(i), Some(TokenKind::Fn)) && matches!(kind(i + 1), Some(TokenKind::Ident(_)))
    }

    pub(crate) fn parse_block_stmts(&mut self) -> Option<Block> {
        let start = self.peek_span();
        self.expect(&TokenKind::LBrace, "`{`")?;
        let mut stmts = Vec::new();
        let mut trailing: Option<Box<Expr>> = None;
        while !matches!(self.peek(), TokenKind::RBrace | TokenKind::Eof) {
            let from = self.pos;
            // An empty statement (`;;`).
            if self.eat(&TokenKind::Semi) {
                continue;
            }
            // `name!(..);` at statement position expands to
            // statements / local items spliced here.
            if let Some(sts) = self.macro_call_stmts() {
                stmts.extend(sts);
                continue;
            }
            if self.at_local_item() {
                match self.parse_local_item() {
                    Some(items) => stmts.extend(items),
                    None => self.recover(from, Recover::Stmt),
                }
                continue;
            }
            let attrs = self.cfg_only_attrs("a statement");
            let marks = (self.pending_stmts.len(), self.lifted.len());
            if let Some((stmt, is_trailing)) = self.parse_stmt() {
                if self.cfg_drops(&attrs, marks) {
                    continue;
                }
                if is_trailing && matches!(self.peek(), TokenKind::RBrace) {
                    if let StmtKind::Trailing(expr) = stmt.kind {
                        trailing = Some(Box::new(expr));
                    } else {
                        stmts.push(stmt);
                    }
                } else {
                    let stmt = match stmt.kind {
                        StmtKind::Trailing(e) if !ends_stmt(&e) => {
                            let d =
                                self.expected(&TokenKind::Semi, "`;` after expression statement");
                            self.diagnostics.push(d);
                            Stmt {
                                kind: StmtKind::Trailing(e),
                                span: stmt.span,
                            }
                        }
                        // A trailing-closure call ending at its `}` is an
                        // expression statement, as if `;` were written:
                        // its value is dropped, not checked.
                        StmtKind::Trailing(e) if !is_block_like(&e) => Stmt {
                            kind: StmtKind::Expr(e),
                            span: stmt.span,
                        },
                        kind => Stmt {
                            kind,
                            span: stmt.span,
                        },
                    };
                    stmts.push(stmt);
                }
                // Follow-ups queued by the statement (nested tuple
                // destructure).
                while let Some(extra) = self.pending_stmts.pop() {
                    stmts.push(extra);
                }
            } else {
                self.pending_stmts.clear();
                self.recover(from, Recover::Stmt);
            }
        }
        self.expect(&TokenKind::RBrace, "`}`")?;
        self.fold_using_stmts(&mut stmts, &mut trailing);
        Some(Block {
            stmts,
            trailing,
            span: start,
            label: None,
        })
    }
}
