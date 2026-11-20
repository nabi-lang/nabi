//! Meta uses during the parse: lookup, imports, derives and attributes.

#[allow(clippy::wildcard_imports)]
use crate::parser::meta::*;

/// A meta name `find_meta` resolved; short-lived, so the block is held
/// whole.
#[allow(clippy::large_enum_variant)]
pub(crate) enum MetaUse {
    Def(MetaDef),
    /// No one meta block: a wrong qualifier or an ambiguous bare name
    /// (reported).
    Bad,
}

/// Whether `imports` (a file's `use`s) import `def`: its path (`use
/// json::log!;`, or `use mdep::Tag;` for a derive named like its trait)
/// or a glob of its package.
pub(crate) fn meta_imported(imports: &[Vec<String>], def: &MetaDef) -> bool {
    let pkg: Vec<&str> = def.package.split("::").collect();
    imports.iter().any(|p| {
        p.len() > pkg.len()
            && p.iter().zip(&pkg).all(|(a, b)| a == b)
            && p.last().is_some_and(|l| {
                *l == def.name || (p.len() == pkg.len() + 1 && (l == "*" || l == "**"))
            })
    })
}

/// The standard library's meta names stay bare (`#[derive(Inspect)]`).
pub(crate) fn is_std(package: &str) -> bool {
    nabic_core::in_toolchain(package)
}

/// How a use of meta name `name` of `kind` reads, qualified by `qual`.
pub(crate) fn meta_use(kind: MetaKind, qual: &str, name: &str) -> String {
    let path = if qual.is_empty() {
        name.to_owned()
    } else {
        format!("{qual}::{name}")
    };
    match kind {
        MetaKind::Derive => format!("`#[derive({path})]`"),
        MetaKind::Attribute => format!("`#[{path}]`"),
        MetaKind::Macro => format!("`{path}!`"),
    }
}

pub(crate) const fn kind_word(kind: MetaKind) -> &'static str {
    match kind {
        MetaKind::Derive => "derive",
        MetaKind::Attribute => "attribute",
        MetaKind::Macro => "macro",
    }
}

/// Every path `t` imports, a glob ending in `*` / `**`; a macro
/// import's `!` is dropped.
pub(crate) fn use_leaves(t: &nabic_ast::UseTree, outer: &[String], out: &mut Vec<Vec<String>>) {
    let mut path = outer.to_vec();
    path.extend(
        t.prefix
            .segments
            .iter()
            .map(|s| s.name.trim_end_matches('!').to_owned()),
    );
    match &t.kind {
        nabic_ast::UseTreeKind::Simple(_) => out.push(path),
        nabic_ast::UseTreeKind::Glob => {
            path.push("*".to_owned());
            out.push(path);
        }
        nabic_ast::UseTreeKind::GlobRecursive => {
            path.push("**".to_owned());
            out.push(path);
        }
        nabic_ast::UseTreeKind::Group(ts) => {
            for c in ts {
                use_leaves(c, &path, out);
            }
        }
    }
}

impl Parser<'_> {
    /// The package a meta qualifier names; `pkg` is this one.
    pub(crate) fn meta_package(&self, qual: &[String]) -> String {
        match qual {
            [p] if p == "pkg" => self.package.clone(),
            [p] if p.starts_with(PKG_ROOT) => p[PKG_ROOT.len()..].to_owned(),
            _ => qual.join("::"),
        }
    }

    /// How this package writes `package` as a qualifier; `None` when it
    /// cannot (the package being built, seen from a dependency).
    pub(crate) fn meta_qualifier(&self, package: &str) -> Option<String> {
        if package == self.package {
            Some("pkg".to_owned())
        } else if package.is_empty() {
            None
        } else {
            Some(package.to_owned())
        }
    }

    /// The meta block of `kind` that `qual::name` (bare `name` when `qual`
    /// is empty) at `at` uses. `None`: not a meta use, i.e. a bare name no
    /// block registers, or a qualified derive or attribute no block of
    /// that name registers (built in, or unknown).
    pub(crate) fn find_meta(
        &mut self,
        kind: MetaKind,
        qual: &[String],
        name: &str,
        at: Span,
    ) -> Option<MetaUse> {
        let meta = std::sync::Arc::clone(&self.meta);
        let all: Vec<&MetaDef> = meta.find_all(kind, name).collect();
        if qual.is_empty() {
            if all.is_empty() {
                return None;
            }
            // This package's block, then a `use`d one, then the standard
            // library's.
            let (package, imports) = self.meta_site(at, name);
            if let Some(d) = all.iter().find(|d| d.package == package) {
                return Some(MetaUse::Def((*d).clone()));
            }
            let used: Vec<&MetaDef> = all
                .iter()
                .filter(|d| meta_imported(&imports, d))
                .copied()
                .collect();
            return Some(match used.as_slice() {
                [d] => MetaUse::Def((*d).clone()),
                [_, _, ..] => self.ambiguous_meta(kind, name, &used, at),
                [] => match all.iter().find(|d| is_std(&d.package)) {
                    Some(d) => MetaUse::Def((*d).clone()),
                    None => self.unscoped_meta(kind, name, &all, at),
                },
            });
        }
        let package = self.meta_package(qual);
        if let Some(d) = all.iter().find(|d| d.package == package) {
            return Some(MetaUse::Def((*d).clone()));
        }
        let def = all
            .iter()
            .find(|d| d.package == self.package)
            .or_else(|| all.first())
            .copied();
        if def.is_none() && kind == MetaKind::Attribute {
            return None;
        }
        let help = match &def {
            Some(d) => {
                let q = self.meta_qualifier(&d.package).unwrap_or_default();
                format!("write {}", meta_use(kind, &q, name))
            }
            None if kind == MetaKind::Macro => format!(
                "no package defines `{name}!`; a package defines one with \
                 `#[impl_macro({name})] pub @macro {{ .. }}`"
            ),
            None if crate::derive::derivable(name).is_some() => {
                format!("write `#[derive({name})]`: it is built in")
            }
            None => "remove it, or name the package whose meta block defines it".to_owned(),
        };
        let at = self.name_span(at, name);
        let shown = if package.is_empty() { "pkg" } else { &package };
        let msg = format!(
            "package `{shown}` defines no {} `{name}`, so {} names nothing",
            kind_word(kind),
            meta_use(kind, &qual.join("::"), name)
        );
        self.diagnostics.push(
            meta_err(Code::META_NAME, at, msg)
                .with_file(self.file.clone())
                .with_help(help),
        );
        Some(MetaUse::Bad)
    }

    /// A bare `name` that `use`s of several packages import: an error
    /// saying which to write.
    pub(crate) fn ambiguous_meta(
        &mut self,
        kind: MetaKind,
        name: &str,
        all: &[&MetaDef],
        at: Span,
    ) -> MetaUse {
        let forms: Vec<String> = all
            .iter()
            .filter_map(|d| self.meta_qualifier(&d.package))
            .map(|q| meta_use(kind, &q, name))
            .collect();
        let mut d = meta_err(
            Code::META_NAME,
            at,
            format!(
                "{} is defined by several packages, so the bare name is ambiguous",
                meta_use(kind, "", name)
            ),
        )
        .with_file(self.file.clone())
        .with_help(format!("write one of {}", forms.join(", ")));
        for def in all {
            d = d.with_secondary(def.span, "defined here");
        }
        self.diagnostics.push(d);
        MetaUse::Bad
    }

    /// A bare `name` that none of the blocks `all` in reach of `site`
    /// registers: an error naming the qualified forms.
    pub(crate) fn unscoped_meta(
        &mut self,
        kind: MetaKind,
        name: &str,
        all: &[&MetaDef],
        at: Span,
    ) -> MetaUse {
        let forms: Vec<String> = all
            .iter()
            .filter_map(|d| self.meta_qualifier(&d.package))
            .collect();
        let at = self.name_span(at, name);
        let mut help = match forms.as_slice() {
            [q] => format!("write {}", meta_use(kind, q, name)),
            _ => format!(
                "write one of {}",
                forms
                    .iter()
                    .map(|q| meta_use(kind, q, name))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        };
        if let [q] = forms.as_slice() {
            let import = if kind == MetaKind::Macro {
                format!("{q}::{name}!")
            } else {
                format!("{q}::{name}")
            };
            help.push_str(&format!(", or add `use {import};`"));
        }
        let owner = match forms.as_slice() {
            [q] => format!("package `{q}`'s {}", kind_word(kind)),
            _ => format!("a {} of another package", kind_word(kind)),
        };
        let mut d = meta_err(
            Code::META_NAME,
            at,
            format!(
                "{} is {owner}, which a bare name does not reach: it names this package's \
                 blocks, `use`d ones and the standard library's",
                meta_use(kind, "", name)
            ),
        )
        .with_file(self.file.clone())
        .with_help(help);
        if let [q] = forms.as_slice() {
            d = d.with_fix(
                at,
                format!("{q}::{name}"),
                format!("write {}", meta_use(kind, q, name)),
            );
        }
        self.diagnostics.push(d);
        MetaUse::Bad
    }

    /// The package and `use` imports a bare meta name at `at` is looked
    /// up from: its definition's for a name a template wrote,
    /// else this file's.
    pub(crate) fn meta_site(&self, at: Span, name: &str) -> (String, Vec<Vec<String>>) {
        let is_name = |t: &&Token| matches!(&t.kind, TokenKind::Ident(n) if n == name);
        let tok = match self
            .tokens
            .iter()
            .position(|t| t.span == at && t.kind == TokenKind::HashLBracket)
        {
            Some(k) => self.tokens[k..]
                .iter()
                .take_while(|t| t.kind != TokenKind::RBracket)
                .find(is_name),
            None => self.tokens.iter().filter(|t| t.span == at).find(is_name),
        };
        match tok.and_then(|t| t.origin.as_deref()) {
            Some(o) => (o.package.clone(), o.imports.clone()),
            None => (self.package.clone(), self.meta_imports.clone()),
        }
    }

    /// Where `name` is written in the attribute at `at` (`#[name]`,
    /// `#[derive(dep::name)]`); `at` itself for a macro call.
    pub(crate) fn name_span(&self, at: Span, name: &str) -> Span {
        let Some(k) = self
            .tokens
            .iter()
            .position(|t| t.span == at && t.kind == TokenKind::HashLBracket)
        else {
            return at;
        };
        self.tokens[k..]
            .iter()
            .take_while(|t| t.kind != TokenKind::RBracket)
            .find(|t| matches!(&t.kind, TokenKind::Ident(n) if n == name))
            .map_or(at, |t| t.span)
    }

    /// Note every path the file's `use`s import (`meta_imports`) before
    /// the parse meets a meta name.
    pub(crate) fn scan_meta_imports(&mut self) {
        let mut out = Vec::new();
        for (k, t) in self.tokens.iter().enumerate() {
            if t.kind != TokenKind::Use {
                continue;
            }
            let mut p = Parser::new(self.tokens, self.file.clone());
            p.pos = k + 1;
            if let Some(tree) = p.parse_use_tree() {
                use_leaves(&tree, &[], &mut out);
            }
        }
        self.meta_imports = out;
    }

    /// Drop the macro imports (`use dep::name!;`) from `t`, checking
    /// each; `None` when nothing is left. They import no item, only say
    /// where a bare `name!` comes from.
    pub(crate) fn strip_meta_imports(
        &mut self,
        t: nabic_ast::UseTree,
        outer: &[String],
        public: bool,
        at: Span,
    ) -> Option<nabic_ast::UseTree> {
        let mut path = outer.to_vec();
        path.extend(t.prefix.segments.iter().map(|s| s.name.clone()));
        match t.kind {
            nabic_ast::UseTreeKind::Simple(_) if path.last().is_some_and(|n| n.ends_with('!')) => {
                let last = path.pop().unwrap_or_default();
                let name = last.trim_end_matches('!');
                let shown = if path.is_empty() {
                    "dep".to_owned()
                } else {
                    path.join("::")
                };
                if public {
                    self.diagnostics.push(
                        meta_err(
                            Code::INVALID_REEXPORT,
                            at,
                            format!("`pub use` cannot re-export the macro `{name}!`"),
                        )
                        .with_file(self.file.clone())
                        .with_help(format!("write `use {shown}::{name}!;`")),
                    );
                } else if path.is_empty() {
                    self.diagnostics.push(
                        meta_err(
                            Code::META_NAME,
                            at,
                            format!("`use {name}!;` does not name the package of `{name}!`"),
                        )
                        .with_file(self.file.clone())
                        .with_help(format!(
                            "write `use dep::{name}!;`, where `dep` is the package that defines it"
                        )),
                    );
                } else if !self.meta.is_empty() {
                    if let Some(MetaUse::Def(d)) = self.find_meta(MetaKind::Macro, &path, name, at)
                    {
                        self.meta_visible(&d, at);
                    }
                }
                None
            }
            nabic_ast::UseTreeKind::Group(ts) if !ts.is_empty() => {
                let kept: Vec<_> = ts
                    .into_iter()
                    .filter_map(|c| self.strip_meta_imports(c, &path, public, at))
                    .collect();
                (!kept.is_empty()).then(|| nabic_ast::UseTree {
                    prefix: t.prefix,
                    spans: t.spans,
                    kind: nabic_ast::UseTreeKind::Group(kept),
                })
            }
            kind => Some(nabic_ast::UseTree {
                prefix: t.prefix,
                spans: t.spans,
                kind,
            }),
        }
    }

    /// `#[cfg(..)] mod m {`: enter it; its closing `}` index and `cfg`s.
    /// `None` consumes nothing.
    pub(crate) fn cfg_mod_open(&mut self) -> Option<(usize, Vec<Attribute>)> {
        if self.peek() != &TokenKind::HashLBracket {
            return None;
        }
        let (start, n_diags) = (self.pos, self.diagnostics.len());
        let attrs = self.parse_outer_attrs();
        self.parse_visibility();
        let open = self.pos + 2;
        let is_mod = self.peek() == &TokenKind::Mod
            && matches!(
                self.tokens.get(open).map(|t| &t.kind),
                Some(TokenKind::LBrace)
            );
        let cfg: Vec<Attribute> = attrs.into_iter().filter(|a| a.name() == "cfg").collect();
        let Some(end) = (is_mod && !cfg.is_empty())
            .then(|| close_of(self.tokens, open))
            .flatten()
        else {
            self.pos = start;
            self.diagnostics.truncate(n_diags);
            return None;
        };
        self.pos = open + 1;
        Some((end, cfg))
    }

    /// Consume a meta block at `#[..] pub? @derive { .. }`. `None`: not one
    /// (nothing consumed); `Some(None)`: malformed (reported).
    pub(crate) fn meta_block(&mut self) -> Option<Option<MetaDef>> {
        if self.peek() != &TokenKind::HashLBracket {
            return None;
        }
        let (start, n_diags) = (self.pos, self.diagnostics.len());
        let attrs = self.parse_outer_attrs();
        let Some(ma) = attrs.iter().find(|a| is_meta_attr(a)).cloned() else {
            self.pos = start;
            self.diagnostics.truncate(n_diags);
            return None;
        };
        let vis = self.parse_visibility();
        let starter = if self.eat(&TokenKind::At) {
            match self.peek().clone() {
                TokenKind::Ident(n) => {
                    self.bump();
                    n
                }
                _ => String::new(),
            }
        } else {
            String::new()
        };
        let want = match ma.name() {
            "impl_attribute" => "attribute",
            "impl_macro" => "macro",
            _ => "derive",
        };
        let body_start = self.pos;
        let Some(end) = (self.peek() == &TokenKind::LBrace)
            .then(|| close_of(self.tokens, self.pos))
            .flatten()
        else {
            self.diagnostics.push(meta_err(
                Code::INVALID_META_BLOCK,
                ma.span,
                format!(
                    "`#[{}(..)]` needs a `{}@{want} {{ .. }}` body",
                    ma.name(),
                    if vis == Visibility::Private {
                        ""
                    } else {
                        "pub "
                    }
                ),
            ));
            return Some(None);
        };
        self.pos = end + 1;
        self.unparsed.push(body_start..self.pos);
        if starter != want {
            self.diagnostics.push(meta_err(
                Code::INVALID_META_BLOCK,
                ma.span,
                format!(
                    "`#[{}(..)]` takes an `@{want} {{ .. }}` body, not `@{starter}`",
                    ma.name()
                ),
            ));
            return Some(None);
        }
        let mut words = attr_words(&ma);
        let mut attr_keys = None;
        if ma.name() == "impl_derive_attribute" {
            if let Some((attr, keys)) = keyed_attr(&ma) {
                words.push(attr);
                attr_keys = Some(keys);
            }
        }
        let arity = if ma.name() == "impl_derive_attribute" {
            2
        } else {
            1
        };
        if words.len() != arity {
            self.diagnostics.push(meta_err(
                Code::INVALID_META_BLOCK,
                ma.span,
                format!("`#[{}]` takes {arity} name argument(s)", ma.name()),
            ));
            return Some(None);
        }
        Some(Some(MetaDef {
            kind: match want {
                "derive" => MetaKind::Derive,
                "macro" => MetaKind::Macro,
                _ => MetaKind::Attribute,
            },
            name: words[0].clone(),
            field_attr: words.get(1).cloned(),
            attr_keys,
            public: vis != Visibility::Private,
            file: self.file.clone(),
            package: String::new(),
            module: String::new(),
            imports: Vec::new(),
            span: ma.span,
            body: self.tokens[body_start + 1..end].to_vec(),
            cfg: attrs.into_iter().filter(|a| a.name() == "cfg").collect(),
        }))
    }

    /// Whether `def` may be used from this file (a private block
    /// only in its own module).
    pub(crate) fn meta_visible(&mut self, def: &MetaDef, at: Span) -> bool {
        if def.public || def.file == self.file {
            return true;
        }
        self.diagnostics.push(
            meta_err(
                Code::PRIVATE_ITEM,
                at,
                format!(
                    "meta block `{}` is private to `{}` (mark it `pub @..`)",
                    def.name, def.file
                ),
            )
            .with_secondary(def.span, "defined here"),
        );
        false
    }

    /// Expand the first user attribute (`#[impl_attribute]`) on the item
    /// parsed from `self.tokens[from..self.pos]`; `None` if it has none.
    pub(crate) fn expand_user_attribute(&mut self, item: &Item, from: usize) -> Option<Vec<Item>> {
        let mut found = None;
        for a in item.attrs() {
            let Some((name, qual)) = a.path.split_last() else {
                continue;
            };
            if a.path == ["wit", "export"] {
                continue;
            }
            if let Some(MetaUse::Def(d)) = self.find_meta(MetaKind::Attribute, qual, name, a.span) {
                found = Some((d, a.clone()));
                break;
            }
        }
        let (def, a) = found?;
        if !self.meta_visible(&def, a.span) {
            return Some(Vec::new());
        }
        let toks = &self.tokens[from..self.pos];
        let at = find_span(toks, 0, a.span).filter(|k| toks[*k].kind == TokenKind::HashLBracket);
        let (item_toks, attr_args) = match at.and_then(|k| Some((k, close_of(toks, k)?))) {
            Some((k, end)) => {
                let mut v = toks[..k].to_vec();
                v.extend_from_slice(&toks[end + 1..]);
                (v, attr_arg_tokens(toks, k))
            }
            None => (toks.to_vec(), Vec::new()),
        };
        let mut target = self.target(item, from, a.span);
        target.item = item_toks;
        target.attr_args = attr_args;
        Some(self.run_meta(&def, &target, item))
    }

    /// Expand the user derives of a struct / enum / union and drop them
    /// from its `#[derive(..)]` (the rest are built-in).
    pub(crate) fn expand_user_derives(&mut self, mut item: Item, from: usize) -> (Item, Vec<Item>) {
        let mut out = Vec::new();
        if !matches!(item, Item::Struct(_) | Item::Enum(_) | Item::Union(_)) {
            return (item, out);
        }
        // What expands, and what leaves `#[derive(..)]`: those and the
        // ones reported.
        let mut users: Vec<(MetaDef, Span)> = Vec::new();
        let mut taken: Vec<Vec<String>> = Vec::new();
        for (path, at) in crate::derive::derive_paths(item.attrs()) {
            let Some((name, qual)) = path.split_last() else {
                continue;
            };
            match self.find_meta(MetaKind::Derive, qual, name, at) {
                Some(MetaUse::Def(d)) => users.push((d, at)),
                Some(MetaUse::Bad) => {}
                None => continue,
            }
            taken.push(path);
        }
        if taken.is_empty() {
            return (item, out);
        }
        for (def, at) in &users {
            if self.meta_visible(def, *at) {
                let mut target = self.target(&item, from, *at);
                if let Some(attr) = &def.field_attr {
                    if let Some(keys) = &def.attr_keys {
                        self.check_attr_keys(&item, attr, keys);
                    }
                    target.self_keys = item
                        .attrs()
                        .iter()
                        .filter(|a| a.name() == attr)
                        .flat_map(attr_words)
                        .collect();
                }
                out.extend(self.run_meta(def, &target, &item));
            }
        }
        {
            let attrs = item.attrs_mut();
            for a in attrs.iter_mut().filter(|a| a.path == ["derive"]) {
                if let AttrBody::Args(args) = &mut a.body {
                    args.retain(|x| {
                        let path = match x {
                            AttrArg::Word(w) => std::slice::from_ref(w),
                            AttrArg::Nested { path, body } if matches!(**body, AttrBody::Empty) => {
                                path.as_slice()
                            }
                            _ => return true,
                        };
                        !taken.iter().any(|p| p == path)
                    });
                }
            }
            attrs.retain(|a| {
                !(a.path == ["derive"] && matches!(&a.body, AttrBody::Args(v) if v.is_empty()))
            });
        }
        (item, out)
    }

    /// Report every `#[attr(..)]` on `item`, its variants and fields that
    /// lists something other than one of `keys`.
    pub(crate) fn check_attr_keys(&mut self, item: &Item, attr: &str, keys: &[String]) {
        fn named(fs: &[nabic_ast::NamedField]) -> impl Iterator<Item = &Attribute> {
            fs.iter().flat_map(|f| f.attrs.iter())
        }
        let mut attrs: Vec<&Attribute> = item.attrs().iter().collect();
        match item {
            Item::Struct(s) => match &s.kind {
                StructKind::Named(fs) => attrs.extend(named(fs)),
                StructKind::Tuple(fs) => attrs.extend(fs.iter().flat_map(|f| f.attrs.iter())),
                StructKind::Unit => {}
            },
            Item::Union(u) => attrs.extend(named(&u.fields)),
            Item::Enum(e) => {
                for v in &e.variants {
                    attrs.extend(v.attrs.iter());
                    if let EnumVariantKind::Named(fs) = &v.kind {
                        attrs.extend(named(fs));
                    }
                }
            }
            _ => {}
        }
        let valid = keys
            .iter()
            .map(|k| format!("`{k}`"))
            .collect::<Vec<_>>()
            .join(", ");
        for a in attrs.into_iter().filter(|a| a.name() == attr) {
            let bad: Vec<String> = match &a.body {
                AttrBody::Args(args) if !args.is_empty() => args
                    .iter()
                    .filter_map(|x| match x {
                        AttrArg::Word(w) if keys.contains(w) => None,
                        AttrArg::Word(w) => Some(format!("unknown key `{w}`")),
                        _ => Some("only key names are allowed".to_owned()),
                    })
                    .collect(),
                _ => vec!["it needs a key".to_owned()],
            };
            for why in bad {
                self.diagnostics.push(meta_err(
                    Code::INVALID_ATTRIBUTE,
                    a.span,
                    format!("`#[{attr}(..)]`: {why}; write one of {valid}"),
                ));
            }
        }
    }

    /// The item parsed from `self.tokens[from..self.pos]` as a target.
    pub(crate) fn target(&self, item: &Item, from: usize, at: Span) -> Target {
        let toks = &self.tokens[from..self.pos];
        let mut cursor = 0;
        let mut field =
            |name: String, vis: Visibility, attrs: &[Attribute], span: Span, named: bool| {
                let mut fa = Vec::new();
                for a in attrs {
                    if let Some(k) = find_span(toks, cursor, a.span) {
                        fa.push((a.name().to_owned(), attr_arg_tokens(toks, k)));
                        cursor = k + 1;
                    }
                }
                let mut ty = Vec::new();
                let mut default = Vec::new();
                if let Some(k) = find_span(toks, cursor, span) {
                    // A named field's type follows `name :`.
                    let start = if named { k + 2 } else { k };
                    let mut p = Parser::new(toks, self.file.clone());
                    p.pos = start;
                    if p.parse_type().is_some() {
                        ty = toks[start..p.pos].to_vec();
                        cursor = p.pos;
                        // `= expr` up to the field's `;` / `,` / `}`.
                        if named && toks.get(cursor).map(|t| &t.kind) == Some(&TokenKind::Eq) {
                            let mut end = cursor + 1;
                            while let Some(t) = toks.get(end) {
                                match t.kind {
                                    TokenKind::Semi | TokenKind::Comma | TokenKind::RBrace => break,
                                    TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => {
                                        end = close_of(toks, end).unwrap_or(toks.len());
                                    }
                                    _ => {}
                                }
                                end += 1;
                            }
                            default = toks[cursor + 1..end.min(toks.len())].to_vec();
                            cursor = end;
                        }
                    }
                }
                FieldInfo {
                    name,
                    ty,
                    is_pub: vis != Visibility::Private,
                    attrs: fa,
                    default,
                    span,
                }
            };
        let named = |fs: &[nabic_ast::NamedField],
                     field: &mut dyn FnMut(
            String,
            Visibility,
            &[Attribute],
            Span,
            bool,
        ) -> FieldInfo| {
            fs.iter()
                .map(|f| field(f.name.clone(), f.vis, &f.attrs, f.span, true))
                .collect::<Vec<_>>()
        };
        let shape = match item {
            Item::Struct(s) => match &s.kind {
                StructKind::Named(_) => VariantShape::Named,
                StructKind::Tuple(_) => VariantShape::Tuple,
                StructKind::Unit => VariantShape::Unit,
            },
            _ => VariantShape::Named,
        };
        let (kind, name, generics, fields, variants) = match item {
            Item::Struct(s) => {
                let fields = match &s.kind {
                    StructKind::Named(fs) => named(fs, &mut field),
                    StructKind::Tuple(fs) => fs
                        .iter()
                        .enumerate()
                        .map(|(i, f)| field(i.to_string(), f.vis, &f.attrs, f.ty.span, false))
                        .collect(),
                    StructKind::Unit => Vec::new(),
                };
                (
                    "struct",
                    s.name.clone(),
                    s.generics.params.clone(),
                    fields,
                    Vec::new(),
                )
            }
            Item::Union(u) => (
                "union",
                u.name.clone(),
                u.generics.params.clone(),
                named(&u.fields, &mut field),
                Vec::new(),
            ),
            Item::Enum(e) => {
                let variants = e
                    .variants
                    .iter()
                    .map(|v| {
                        let (shape, fields) = match &v.kind {
                            EnumVariantKind::Unit | EnumVariantKind::Value(_) => {
                                (VariantShape::Unit, Vec::new())
                            }
                            EnumVariantKind::Tuple(tys) => (
                                VariantShape::Tuple,
                                tys.iter()
                                    .enumerate()
                                    .map(|(i, t)| {
                                        field(i.to_string(), Visibility::Pub, &[], t.span, false)
                                    })
                                    .collect(),
                            ),
                            EnumVariantKind::Named(fs) => {
                                (VariantShape::Named, named(fs, &mut field))
                            }
                        };
                        VariantInfo {
                            name: v.name.clone(),
                            shape,
                            fields,
                        }
                    })
                    .collect();
                (
                    "enum",
                    e.name.clone(),
                    e.generics.params.clone(),
                    Vec::new(),
                    variants,
                )
            }
            Item::Fn(f) => ("fn", f.name.clone(), Vec::new(), Vec::new(), Vec::new()),
            Item::Trait(t) => ("trait", t.name.clone(), Vec::new(), Vec::new(), Vec::new()),
            Item::Impl(_) => ("impl", String::new(), Vec::new(), Vec::new(), Vec::new()),
            _ => ("item", String::new(), Vec::new(), Vec::new(), Vec::new()),
        };
        Target {
            kind,
            name,
            generics,
            shape,
            fields,
            variants,
            item: toks.to_vec(),
            attr_args: Vec::new(),
            self_keys: Vec::new(),
            at,
        }
    }

    /// Expand `def` over `target` and parse the result (expanding again
    /// what it produces).
    pub(crate) fn run_meta(&mut self, def: &MetaDef, target: &Target, item: &Item) -> Vec<Item> {
        let key = format!("{:?}:{}:{}", def.kind, def.name, def.package);
        if self.meta_stack.contains(&key) || self.meta_stack.len() >= 32 {
            let chain: Vec<&str> = self
                .meta_stack
                .iter()
                .map(|k| k.split(':').nth(1).unwrap_or(""))
                .collect();
            self.diagnostics.push(
                meta_err(
                    Code::EXPANSION_LIMIT,
                    target.at,
                    format!(
                        "cyclic meta expansion: {} → {}",
                        chain.join(" → "),
                        def.name
                    ),
                )
                .with_secondary(def.span, "this meta block"),
            );
            return Vec::new();
        }
        let mut ex = Expander {
            def,
            origin: def.origin(),
            t: target,
            out: Vec::new(),
            diags: Vec::new(),
            where_all: Vec::new(),
            bound: Vec::new(),
            bind_pos: HashSet::default(),
        };
        ex.scan_binders();
        ex.expand(0, def.body.len(), Ctx::default());
        let (mut tokens, diags) = (ex.out, ex.diags);
        place_declared(&mut tokens);
        let failed = !diags.is_empty();
        self.diagnostics.extend(diags);
        if failed {
            return Vec::new();
        }
        let what = match def.kind {
            MetaKind::Derive => format!("expanded from this `#[derive({})]`", def.name),
            MetaKind::Attribute => format!("expanded from this `#[{}]`", def.name),
            MetaKind::Macro => format!("expanded from this `{}!` macro", def.name),
        };
        let (file, at) = (&self.file, target.at);
        self.expanded.push(format!(
            "// {what} ({file}:{}:{})\n{}",
            at.line,
            at.col,
            render(&tokens)
        ));
        let mut p = Parser::new(&tokens, self.file.clone());
        p.ext = self.ext.clone();
        p.meta = Arc::clone(&self.meta);
        p.literals = Arc::clone(&self.literals);
        p.cfg.clone_from(&self.cfg);
        // Use-site tokens look meta names up as the use site does; the
        // template's carry their definition's origin.
        p.package.clone_from(&self.package);
        p.meta_imports.clone_from(&self.meta_imports);
        p.meta_stack.clone_from(&self.meta_stack);
        p.meta_stack.push(key);
        let mut items = Vec::new();
        while !p.at_eof() {
            let from = p.pos;
            if p.meta_block().is_some() {
                continue;
            }
            match p.parse_item() {
                Some(i) => {
                    items.push(i);
                    items.append(&mut p.pending_items);
                }
                None if std::mem::take(&mut p.vanished) => {}
                None if p.pos == from => break,
                None => {}
            }
        }
        if p.pos < tokens.len() && p.diagnostics.is_empty() {
            let s = tokens[p.pos].span;
            p.diagnostics.push(meta_err(
                Code::INVALID_META_BLOCK,
                s,
                "expected an item in the meta block's expansion",
            ));
        }
        self.expanded.append(&mut p.expanded);
        // Errors in generated code point at the template and at the use.
        for mut d in p.diagnostics {
            if d.primary.as_ref().is_some_and(|l| l.span.file != 0) {
                d.file = None;
            }
            if !d.secondary.iter().any(|l| l.span == target.at) {
                d = d.with_secondary(target.at, what.clone());
            }
            self.diagnostics.push(d);
        }
        // `#[cfg]` runs before expansion - what the item's cfg
        // removes, its expansion goes with it.
        let cfgs: Vec<&Attribute> = item.attrs().iter().filter(|a| a.name() == "cfg").collect();
        for i in &mut items {
            i.attrs_mut().extend(cfgs.iter().map(|a| (*a).clone()));
        }
        items
    }
}

/// The tokens of `@use_site(path)`'s argument, written at the use site;
/// `None` unless it is a path (`a::b`, `pkg::f`).
pub(crate) fn use_site_path(arg: &[Token]) -> Option<Vec<Token>> {
    let mut want_name = true;
    for t in arg {
        match (&t.kind, want_name) {
            (TokenKind::Ident(_), true) => want_name = false,
            (TokenKind::ColonColon, false) => want_name = true,
            _ => return None,
        }
    }
    if want_name {
        return None;
    }
    Some(arg.iter().map(|t| tok(t.kind.clone(), t.span)).collect())
}

/// An expansion's own items land at the use site, so the
/// names it declares there are looked up there too.
pub(crate) fn place_declared(tokens: &mut [Token]) {
    let mut depth = 0i32;
    let mut names = HashSet::default();
    for k in 0..tokens.len() {
        match &tokens[k].kind {
            TokenKind::LBrace | TokenKind::LParen | TokenKind::LBracket => depth += 1,
            TokenKind::RBrace | TokenKind::RParen | TokenKind::RBracket => depth -= 1,
            kind if depth == 0 => {
                let declares = matches!(
                    kind,
                    TokenKind::Fn
                        | TokenKind::Struct
                        | TokenKind::Enum
                        | TokenKind::Trait
                        | TokenKind::Mod
                ) || matches!(kind, TokenKind::Ident(w) if matches!(w.as_str(), "union" | "type" | "const" | "static"));
                if let (true, Some(TokenKind::Ident(n))) =
                    (declares, tokens.get(k + 1).map(|t| &t.kind))
                {
                    names.insert(n.clone());
                }
            }
            _ => {}
        }
    }
    for t in tokens.iter_mut() {
        if matches!(&t.kind, TokenKind::Ident(n) if names.contains(n)) {
            t.origin = None;
        }
    }
}
