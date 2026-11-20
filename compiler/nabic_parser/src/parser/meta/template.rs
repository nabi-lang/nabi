//! Template expansion: substitution, hygiene, `@if` conditions.

#[allow(clippy::wildcard_imports)]
use crate::parser::meta::*;

#[derive(Clone, Debug)]
pub(crate) struct FieldInfo {
    /// Field name, or its index in a tuple struct / variant.
    pub(crate) name: String,
    pub(crate) ty: Vec<Token>,
    pub(crate) is_pub: bool,
    /// `(name, argument tokens)` of each attribute on the field.
    pub(crate) attrs: Vec<(String, Vec<Token>)>,
    /// The `= expr` default of a named struct field (empty: none).
    pub(crate) default: Vec<Token>,
    pub(crate) span: Span,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum VariantShape {
    Unit,
    Tuple,
    Named,
}

#[derive(Clone, Debug)]
pub(crate) struct VariantInfo {
    pub(crate) name: String,
    pub(crate) shape: VariantShape,
    pub(crate) fields: Vec<FieldInfo>,
}

/// What a meta block is applied to.
pub(crate) struct Target {
    /// `struct` / `enum` / `union` / `fn` / `impl` / `trait` / other.
    pub(crate) kind: &'static str,
    pub(crate) name: String,
    pub(crate) generics: Vec<GenericParam>,
    /// A struct's shape (`@Self.is_unit` / `.is_tuple` / `.is_named`).
    pub(crate) shape: VariantShape,
    pub(crate) fields: Vec<FieldInfo>,
    pub(crate) variants: Vec<VariantInfo>,
    /// The item's tokens without the applied attribute.
    pub(crate) item: Vec<Token>,
    /// `#[name(args)]`'s arguments (`@attribute`).
    pub(crate) attr_args: Vec<Token>,
    /// The words of the type's own `#[attr(..)]` (`@Self.has_derive_key`).
    pub(crate) self_keys: Vec<String>,
    /// The `#[derive(..)]` / `#[name]` use.
    pub(crate) at: Span,
}

#[derive(Clone, Copy, Default)]
pub(crate) struct Ctx {
    pub(crate) variant: Option<usize>,
    /// (variant or `usize::MAX` for the item's own fields, field index).
    pub(crate) field: Option<(usize, usize)>,
}

pub(crate) const TOP: usize = usize::MAX;

pub(crate) struct Expander<'a> {
    pub(crate) def: &'a MetaDef,
    /// What the template's own tokens are stamped with.
    pub(crate) origin: Arc<nabic_lexer::Origin>,
    pub(crate) t: &'a Target,
    pub(crate) out: Vec<Token>,
    pub(crate) diags: Vec<NabiDiagnostic>,
    /// `@where_all(Tr)` bounds for the type parameters of later impls.
    pub(crate) where_all: Vec<Vec<Token>>,
    /// Hygiene: names the template binds (with the token range each is
    /// in scope over), and token indices that are binding positions.
    pub(crate) bound: Vec<(String, usize, usize)>,
    pub(crate) bind_pos: HashSet<usize>,
}

/// Hygienic name of a template-introduced local.
pub(crate) fn hygienic(name: &str) -> String {
    format!("{name}__meta")
}

impl Expander<'_> {
    pub(crate) fn body(&self) -> &[Token] {
        &self.def.body
    }

    pub(crate) fn err(&mut self, at: usize, msg: impl Into<String>) {
        let span = self.body().get(at).map_or(self.def.span, |t| t.span);
        let what = match self.def.kind {
            MetaKind::Derive => format!("in this `#[derive({})]`", self.def.name),
            MetaKind::Attribute => format!("in this `#[{}]`", self.def.name),
            MetaKind::Macro => format!("in this `{}!` macro", self.def.name),
        };
        self.diags
            .push(meta_err(Code::INVALID_TEMPLATE, span, msg).with_secondary(self.t.at, what));
    }

    pub(crate) fn field(&self, ctx: Ctx) -> Option<&FieldInfo> {
        let (v, f) = ctx.field?;
        if v == TOP {
            self.t.fields.get(f)
        } else {
            self.t.variants.get(v)?.fields.get(f)
        }
    }

    /// Name a variant payload field is bound to by `@variant.pattern`.
    pub(crate) fn binding(&self, ctx: Ctx) -> Option<String> {
        let (v, f) = ctx.field?;
        (v != TOP).then(|| format!("{}__meta{f}", self.t.variants[v].name))
    }

    /// `Name` or `Name<T, N>`.
    pub(crate) fn self_tokens(&self, span: Span) -> Vec<Token> {
        let mut v = vec![tok(TokenKind::Ident(self.t.name.clone()), span)];
        if !self.t.generics.is_empty() {
            v.push(tok(TokenKind::Lt, span));
            for (i, g) in self.t.generics.iter().enumerate() {
                if i > 0 {
                    v.push(tok(TokenKind::Comma, span));
                }
                let (GenericParam::Type { name, .. } | GenericParam::Const { name, .. }) = g;
                v.push(tok(TokenKind::Ident(name.clone()), span));
            }
            v.push(tok(TokenKind::Gt, span));
        }
        v
    }

    /// `<T: B1 + B2, const N: usize>` after a template `impl`.
    pub(crate) fn impl_params(&self, span: Span) -> Vec<Token> {
        let mut v = vec![tok(TokenKind::Lt, span)];
        for (i, g) in self.t.generics.iter().enumerate() {
            if i > 0 {
                v.push(tok(TokenKind::Comma, span));
            }
            match g {
                GenericParam::Type { name, .. } => {
                    v.push(tok(TokenKind::Ident(name.clone()), span));
                    for (j, b) in self.where_all.iter().enumerate() {
                        v.push(tok(
                            if j == 0 {
                                TokenKind::Colon
                            } else {
                                TokenKind::Plus
                            },
                            span,
                        ));
                        v.extend(b.iter().cloned());
                    }
                }
                GenericParam::Const { name, ty } => {
                    v.push(tok(TokenKind::Ident("const".to_owned()), span));
                    v.push(tok(TokenKind::Ident(name.clone()), span));
                    v.push(tok(TokenKind::Colon, span));
                    v.push(tok(TokenKind::Ident(crate::derive::ty_text(ty)), span));
                }
            }
        }
        v.push(tok(TokenKind::Gt, span));
        v
    }

    /// `Name::V`, `Name::V(a, b)` or `Name::V { x: a }` binding payload
    /// fields to hygienic names.
    /// `Name { a: __meta0, .. }` / `Name(__meta0, ..)` / `Name`: a value
    /// built from the `@field.binding` locals.
    pub(crate) fn self_pattern(&self, span: Span) -> Vec<Token> {
        let mut out = vec![tok(TokenKind::Ident(self.t.name.clone()), span)];
        let fields = &self.t.fields;
        let bind = |f: usize| tok(TokenKind::Ident(format!("__meta{f}")), span);
        match (self.t.kind, self.t.shape) {
            ("struct", VariantShape::Tuple) => {
                out.push(tok(TokenKind::LParen, span));
                for f in 0..fields.len() {
                    if f > 0 {
                        out.push(tok(TokenKind::Comma, span));
                    }
                    out.push(bind(f));
                }
                out.push(tok(TokenKind::RParen, span));
            }
            ("struct", VariantShape::Named) => {
                out.push(tok(TokenKind::LBrace, span));
                for (f, fi) in fields.iter().enumerate() {
                    if f > 0 {
                        out.push(tok(TokenKind::Comma, span));
                    }
                    out.push(tok(TokenKind::Ident(fi.name.clone()), fi.span));
                    out.push(tok(TokenKind::Colon, span));
                    out.push(bind(f));
                }
                out.push(tok(TokenKind::RBrace, span));
            }
            _ => {}
        }
        out
    }

    pub(crate) fn pattern(&self, v: usize, span: Span) -> Vec<Token> {
        let var = &self.t.variants[v];
        let mut out = vec![
            tok(TokenKind::Ident(self.t.name.clone()), span),
            tok(TokenKind::ColonColon, span),
            tok(TokenKind::Ident(var.name.clone()), span),
        ];
        let bind = |f: usize| {
            self.binding(Ctx {
                variant: Some(v),
                field: Some((v, f)),
            })
            .unwrap_or_default()
        };
        match var.shape {
            VariantShape::Unit => {}
            VariantShape::Tuple => {
                out.push(tok(TokenKind::LParen, span));
                for f in 0..var.fields.len() {
                    if f > 0 {
                        out.push(tok(TokenKind::Comma, span));
                    }
                    out.push(tok(TokenKind::Ident(bind(f)), span));
                }
                out.push(tok(TokenKind::RParen, span));
            }
            VariantShape::Named => {
                out.push(tok(TokenKind::LBrace, span));
                for (f, fi) in var.fields.iter().enumerate() {
                    if f > 0 {
                        out.push(tok(TokenKind::Comma, span));
                    }
                    out.push(tok(TokenKind::Ident(fi.name.clone()), fi.span));
                    out.push(tok(TokenKind::Colon, span));
                    out.push(tok(TokenKind::Ident(bind(f)), span));
                }
                out.push(tok(TokenKind::RBrace, span));
            }
        }
        out
    }

    /// Substitute `@..` names inside string / f-string text.
    pub(crate) fn string_text(&self, s: &str, ctx: Ctx) -> String {
        let mut s = s.replace("@Self", &self.t.name);
        s = s.replace("@item.name", &self.t.name);
        if let Some(f) = self.field(ctx) {
            s = s.replace("@field.name", &f.name);
        }
        if let Some(v) = ctx.variant {
            s = s.replace("@variant.name", &self.t.variants[v].name);
        }
        s
    }

    /// Hygiene pre-scan: names bound by `let` / `for` / closure params
    /// written in the template itself (not those spliced in by `@..`),
    /// each with the token range it is in scope over.
    pub(crate) fn scan_binders(&mut self) {
        let body = &self.def.body;
        let mut names = Vec::new();
        let mut pos = HashSet::default();
        let mut mark = |k: usize, end: usize, names: &mut Vec<(String, usize, usize)>| {
            if let TokenKind::Ident(n) = &body[k].kind {
                if n != "mut" && n != "in" && n != "_" {
                    names.push((n.clone(), k, end));
                    pos.insert(k);
                }
            }
        };
        for k in 0..body.len() {
            let prev_at = k > 0 && body[k - 1].kind == TokenKind::At;
            match body[k].kind {
                TokenKind::Let | TokenKind::For if !prev_at => {
                    let mut j = k + 1;
                    if matches!(&body.get(j).map(|t| &t.kind), Some(TokenKind::Ident(m)) if m == "mut")
                    {
                        j += 1;
                    }
                    // `for x in` binds; `impl Tr for Ty` doesn't.
                    let after = match body.get(j).map(|t| &t.kind) {
                        Some(TokenKind::LParen) => close_of(body, j).map(|e| e + 1),
                        _ => Some(j + 1),
                    };
                    let is_in = matches!(
                        after.and_then(|a| body.get(a)).map(|t| &t.kind),
                        Some(TokenKind::Ident(w)) if w == "in"
                    );
                    if body[k].kind == TokenKind::For && !is_in {
                        continue;
                    }
                    // A `let` holds to the end of its block, a `for`
                    // binding over the loop body.
                    let scope = if body[k].kind == TokenKind::For {
                        after.and_then(|a| block_after(body, a))
                    } else {
                        None
                    }
                    .unwrap_or_else(|| enclosing_close(body, k, true));
                    match body.get(j).map(|t| &t.kind) {
                        Some(TokenKind::Ident(_)) => mark(j, scope, &mut names),
                        Some(TokenKind::LParen) => {
                            let end = close_of(body, j).unwrap_or(j);
                            for m in j + 1..end {
                                if matches!(
                                    body.get(m + 1).map(|t| &t.kind),
                                    Some(TokenKind::Comma | TokenKind::RParen)
                                ) && body[m - 1].kind != TokenKind::At
                                    && body[m - 1].kind != TokenKind::Dot
                                {
                                    mark(m, scope, &mut names);
                                }
                            }
                        }
                        _ => {}
                    }
                }
                // A closure's `|a, b: T|` (a `|` where an expression starts).
                TokenKind::Pipe
                    if k > 0
                        && matches!(
                            body[k - 1].kind,
                            TokenKind::LParen
                                | TokenKind::Comma
                                | TokenKind::Eq
                                | TokenKind::LBrace
                                | TokenKind::Semi
                                | TokenKind::FatArrow
                                | TokenKind::Return
                        ) =>
                {
                    let mut params = Vec::new();
                    let mut m = k + 1;
                    let mut expect_name = true;
                    while m < body.len() && body[m].kind != TokenKind::Pipe {
                        match body[m].kind {
                            TokenKind::Ident(_) if expect_name => {
                                params.push(m);
                                expect_name = false;
                            }
                            TokenKind::Comma => expect_name = true,
                            _ => {}
                        }
                        m += 1;
                    }
                    // The closure body: a block, else up to the end of the
                    // group the closure is written in.
                    let scope = match body.get(m + 1).map(|t| &t.kind) {
                        Some(TokenKind::LBrace) => close_of(body, m + 1),
                        _ => None,
                    }
                    .unwrap_or_else(|| enclosing_close(body, k, false));
                    for p in params {
                        mark(p, scope, &mut names);
                    }
                }
                _ => {}
            }
        }
        self.bound = names;
        self.bind_pos = pos;
    }

    /// Emit template token `k` (renamed if it is a hygienic local).
    pub(crate) fn emit_plain(&mut self, k: usize, ctx: Ctx) {
        let t = self.body()[k].clone();
        let kind = match &t.kind {
            TokenKind::Ident(n)
                if self
                    .bound
                    .iter()
                    .any(|(b, from, to)| b == n && (*from..=*to).contains(&k))
                    && self.renames(k) =>
            {
                TokenKind::Ident(hygienic(n))
            }
            TokenKind::StrLit(s) => TokenKind::StrLit(self.string_text(s, ctx)),
            TokenKind::FStrText(s) => TokenKind::FStrText(self.string_text(s, ctx)),
            k => k.clone(),
        };
        self.out
            .push(self.stamp(tok(kind, t.span), t.origin.as_ref()));
    }

    /// `t` as written by the template: its definition's origin, unless
    /// it already has one (a template that is itself expanded code).
    pub(crate) fn stamp(&self, mut t: Token, had: Option<&Arc<nabic_lexer::Origin>>) -> Token {
        t.origin = Some(Arc::clone(had.unwrap_or(&self.origin)));
        t
    }

    /// Whether the bound name at `k` is a variable use / binding (not a
    /// member, path segment or `label:`).
    pub(crate) fn renames(&self, k: usize) -> bool {
        if self.bind_pos.contains(&k) {
            return true;
        }
        let b = self.body();
        let prev = k.checked_sub(1).map(|p| &b[p].kind);
        let next = b.get(k + 1).map(|t| &t.kind);
        if matches!(
            prev,
            Some(TokenKind::Dot | TokenKind::ColonColon | TokenKind::At)
        ) || matches!(next, Some(TokenKind::ColonColon))
        {
            return false;
        }
        let label = matches!(
            prev,
            Some(TokenKind::LBrace | TokenKind::Comma | TokenKind::LParen)
        ) && matches!(next, Some(TokenKind::Colon));
        !label
    }

    /// Expand `body[lo..hi]`.
    pub(crate) fn expand(&mut self, lo: usize, hi: usize, ctx: Ctx) {
        let mut k = lo;
        while k < hi {
            if self.body()[k].kind != TokenKind::At {
                if self.body()[k].kind == TokenKind::Impl
                    && !self.t.generics.is_empty()
                    && self.body().get(k + 1).map(|t| &t.kind) != Some(&TokenKind::Lt)
                {
                    // `impl Tr for @Self` on a generic item: the item's
                    // parameters (bounded by `@where_all`).
                    self.emit_plain(k, ctx);
                    let span = self.body()[k].span;
                    let ps = self.impl_params(span);
                    self.out.extend(ps);
                } else {
                    self.emit_plain(k, ctx);
                }
                k += 1;
                continue;
            }
            k = self.template_token(k, hi, ctx);
        }
    }

    /// `@head[.member][(args)][{block}]` at `k`; returns the index after it.
    pub(crate) fn template_token(&mut self, k: usize, hi: usize, ctx: Ctx) -> usize {
        let b = &self.def.body;
        let span = b[k].span;
        let head = match b.get(k + 1).map(|t| &t.kind) {
            Some(TokenKind::SelfUpper) => "Self".to_owned(),
            Some(TokenKind::Ident(n)) => n.clone(),
            // `@for` and `@concat` are macro-only.
            Some(TokenKind::For) => {
                self.err(
                    k,
                    "`@for` belongs to `@macro` templates only; a derive or attribute \
                     repeats with `@for_fields { .. }` / `@for_variants { .. }`",
                );
                return k + 2;
            }
            _ => {
                self.err(k, "expected a template token name after `@`");
                return k + 1;
            }
        };
        if head == "concat" {
            return self.concat(k, hi, ctx);
        }
        let mut j = k + 2;
        let members: &[&str] = match head.as_str() {
            "field" => &[
                "name",
                "type",
                "is_pub",
                "has_attr",
                "has_default",
                "default",
                "index",
                "binding",
            ],
            "Self" => &[
                "name",
                "field_count",
                "variant_count",
                "is_unit",
                "is_tuple",
                "is_named",
                "has_derive_key",
                "pattern",
            ],
            "variant" => &[
                "name", "pattern", "is_unit", "is_tuple", "is_named", "index",
            ],
            "item" => &["name", "params", "return_type", "body"],
            _ => &[],
        };
        let mut member = String::new();
        if b.get(j).map(|t| &t.kind) == Some(&TokenKind::Dot) {
            if let Some(TokenKind::Ident(m)) = b.get(j + 1).map(|t| &t.kind) {
                if members.contains(&m.as_str()) {
                    member.clone_from(m);
                    j += 2;
                }
            }
        }
        let dot = if member.is_empty() { "" } else { "." };
        let full = format!("{head}{dot}{member}");
        let takes_args = matches!(
            full.as_str(),
            "field.has_attr"
                | "Self.has_derive_key"
                | "has_derive_attr"
                | "derive_attr"
                | "apply_if"
                | "accept"
                | "where_all"
                | "use_site"
        );
        let takes_block = matches!(
            full.as_str(),
            "for_fields"
                | "for_variants"
                | "for_variant_fields"
                | "if_struct"
                | "if_enum"
                | "if_union"
                | "apply_if"
        );
        let mut args = (j, j);
        if takes_args {
            match (b.get(j).map(|t| &t.kind), close_of(b, j)) {
                (Some(TokenKind::LParen), Some(end)) if end < hi => {
                    args = (j + 1, end);
                    j = end + 1;
                }
                _ => {
                    self.err(k, format!("`@{full}` needs `(..)` arguments"));
                    return j;
                }
            }
        }
        let mut block = (j, j);
        if takes_block {
            match (b.get(j).map(|t| &t.kind), close_of(b, j)) {
                (Some(TokenKind::LBrace), Some(end)) if end < hi => {
                    block = (j + 1, end);
                    j = end + 1;
                }
                _ => {
                    self.err(k, format!("`@{full}` needs a `{{ .. }}` block"));
                    return j;
                }
            }
        }
        // `@x.name.as_str()` - the name as a string literal.
        let as_str = matches!(
            full.as_str(),
            "field.name" | "variant.name" | "item.name" | "Self" | "Self.name"
        ) && matches!(
            (
                b.get(j).map(|t| &t.kind),
                b.get(j + 1).map(|t| &t.kind),
                b.get(j + 2).map(|t| &t.kind),
                b.get(j + 3).map(|t| &t.kind),
            ),
            (
                Some(TokenKind::Dot),
                Some(TokenKind::Ident(m)),
                Some(TokenKind::LParen),
                Some(TokenKind::RParen)
            ) if m == "as_str"
        );
        let attr_only = head == "item" || head == "attribute" || head == "accept";
        if attr_only && self.def.kind != MetaKind::Attribute {
            self.err(
                k,
                format!("`@{full}` is only meaningful in an `@attribute` block"),
            );
            return j;
        }
        if matches!(
            full.as_str(),
            "has_derive_attr" | "derive_attr" | "Self.has_derive_key"
        ) && self.def.field_attr.is_none()
        {
            self.err(
                k,
                format!("`@{full}` needs `#[impl_derive_attribute(Trait, attr)]`"),
            );
            return j;
        }
        let toks: Vec<Token> = match full.as_str() {
            "Self" => self.self_tokens(span),
            "Self.pattern" => self.self_pattern(span),
            "field.index" | "field.binding" => {
                let Some((_, f)) = ctx.field else {
                    self.err(
                        k,
                        format!(
                            "`@{full}` outside `@for_fields`: `@field` tokens work inside \
                             `@for_fields {{ .. }}`"
                        ),
                    );
                    return j;
                };
                if full == "field.index" {
                    vec![tok(TokenKind::IntLit(f.to_string()), span)]
                } else {
                    let name = self.binding(ctx).unwrap_or_else(|| format!("__meta{f}"));
                    vec![tok(TokenKind::Ident(name), span)]
                }
            }
            "Self.field_count" => {
                if self.t.kind == "enum" {
                    self.err(
                        k,
                        "`@Self.field_count` on an enum: count a variant's fields with \
                         `0 @for_variant_fields { + 1 }` inside `@for_variants { .. }`",
                    );
                    return j;
                }
                vec![tok(
                    TokenKind::IntLit(self.t.fields.len().to_string()),
                    span,
                )]
            }
            "Self.variant_count" => {
                if self.t.kind != "enum" {
                    let kind = self.t.kind;
                    let a = nabic_diagnostic::a_or_an(kind);
                    self.err(
                        k,
                        format!(
                            "`@Self.variant_count` on {a} {kind}: it counts an enum's \
                             variants; guard it with `@if_enum {{ .. }}`"
                        ),
                    );
                    return j;
                }
                vec![tok(
                    TokenKind::IntLit(self.t.variants.len().to_string()),
                    span,
                )]
            }
            "variant.index" => {
                let Some(v) = ctx.variant else {
                    self.err(k, "`@variant.index` outside `@for_variants`");
                    return j;
                };
                vec![tok(TokenKind::IntLit(v.to_string()), span)]
            }
            "for_fields" => {
                let (v, n) = match ctx.variant {
                    Some(v) => (v, self.t.variants[v].fields.len()),
                    None if self.t.kind == "enum" => {
                        self.err(
                            k,
                            "`@for_fields` on an enum needs `@for_variants { .. }` around it",
                        );
                        return j;
                    }
                    None => (TOP, self.t.fields.len()),
                };
                for f in 0..n {
                    let c = Ctx {
                        field: Some((v, f)),
                        ..ctx
                    };
                    self.expand(block.0, block.1, c);
                }
                Vec::new()
            }
            "for_variant_fields" => {
                let Some(v) = ctx.variant else {
                    self.err(k, "`@for_variant_fields` outside `@for_variants`");
                    return j;
                };
                for f in 0..self.t.variants[v].fields.len() {
                    let c = Ctx {
                        field: Some((v, f)),
                        ..ctx
                    };
                    self.expand(block.0, block.1, c);
                }
                Vec::new()
            }
            "for_variants" => {
                if self.t.kind != "enum" {
                    self.err(
                        k,
                        format!(
                            "`@for_variants` on {} {} (needs an enum)",
                            nabic_diagnostic::a_or_an(self.t.kind),
                            self.t.kind
                        ),
                    );
                    return j;
                }
                for v in 0..self.t.variants.len() {
                    let c = Ctx {
                        variant: Some(v),
                        field: None,
                    };
                    self.expand(block.0, block.1, c);
                }
                Vec::new()
            }
            "if_struct" | "if_enum" | "if_union" => {
                if self.t.kind == &head[3..] {
                    self.expand(block.0, block.1, ctx);
                }
                Vec::new()
            }
            "apply_if" => {
                let mut c = Cond {
                    pos: args.0,
                    end: args.1,
                };
                let val = self.cond_or(&mut c, ctx);
                if c.pos != c.end {
                    self.err(c.pos, "unexpected token in `@apply_if` condition");
                }
                if val == Some(true) {
                    self.expand(block.0, block.1, ctx);
                }
                Vec::new()
            }
            "accept" => {
                let ok = b[args.0..args.1].iter().any(|t| {
                    let w = match &t.kind {
                        TokenKind::Ident(w) => w.as_str(),
                        k => nabic_lexer::token::keyword_str(k).unwrap_or(""),
                    };
                    w == self.t.kind
                });
                if !ok {
                    self.err(
                        k,
                        format!(
                            "`#[{}]` cannot be applied to {} {} (`@accept` at the \
                             template)",
                            self.def.name,
                            nabic_diagnostic::a_or_an(self.t.kind),
                            self.t.kind
                        ),
                    );
                }
                Vec::new()
            }
            "where_all" => {
                let bounds = b[args.0..args.1]
                    .iter()
                    .map(|t| self.stamp(t.clone(), t.origin.as_ref()))
                    .collect();
                self.where_all.push(bounds);
                Vec::new()
            }
            // The path resolves where the meta block is used.
            "use_site" => {
                let Some(p) = use_site_path(&b[args.0..args.1]) else {
                    self.err(
                        k,
                        "`@use_site(..)` takes a path, e.g. `@use_site(validate)`",
                    );
                    return j;
                };
                p
            }
            "field" | "field.name" | "field.type" | "field.default" => {
                let binding = self.binding(ctx);
                let Some(f) = self.field(ctx) else {
                    self.err(
                        k,
                        format!(
                            "`@{full}` outside `@for_fields`: `@field` tokens work inside \
                             `@for_fields {{ .. }}`"
                        ),
                    );
                    return j;
                };
                match (full.as_str(), binding) {
                    ("field.type", _) => f.ty.clone(),
                    ("field.default", _) if f.default.is_empty() => {
                        let name = f.name.clone();
                        self.err(
                            k,
                            format!(
                                "field `{name}` has no default (guard with \
                                 `@apply_if(@field.has_default)`)"
                            ),
                        );
                        Vec::new()
                    }
                    ("field.default", _) => f.default.clone(),
                    // A variant's payload is reached through its binding.
                    ("field", Some(bn)) => vec![tok(TokenKind::Ident(bn), span)],
                    _ => vec![ident(&f.name, f.span)],
                }
            }
            "Self.has_derive_key" => {
                let key = self.word(args);
                let known = self
                    .def
                    .attr_keys
                    .as_ref()
                    .is_some_and(|ks| ks.contains(&key));
                if !known {
                    let attr = self.def.field_attr.clone().unwrap_or_default();
                    self.err(
                        k,
                        format!(
                            "`{key}` is not a declared key of `#[{attr}(..)]` (declare it: \
                             `#[impl_derive_attribute(Trait, {attr}({key}))]`)"
                        ),
                    );
                }
                let v = self.t.self_keys.contains(&key);
                vec![tok(
                    if v { TokenKind::True } else { TokenKind::False },
                    span,
                )]
            }
            "field.is_pub" | "field.has_attr" | "field.has_default" | "has_derive_attr"
            | "variant.is_unit" | "variant.is_tuple" | "variant.is_named" | "Self.is_unit"
            | "Self.is_tuple" | "Self.is_named" => {
                let v = self.bool_token(k, &full, args, ctx).unwrap_or(false);
                vec![tok(
                    if v { TokenKind::True } else { TokenKind::False },
                    span,
                )]
            }
            "derive_attr" => {
                let name = self.word(args);
                let Some(f) = self.field(ctx) else {
                    self.err(k, "`@derive_attr` outside `@for_fields`");
                    return j;
                };
                if let Some((_, a)) = f.attrs.iter().find(|(n, _)| *n == name) {
                    a.clone()
                } else {
                    let fname = f.name.clone();
                    self.err(
                        k,
                        format!(
                            "field `{fname}` has no `#[{name}(..)]` (guard with \
                             `@apply_if(@has_derive_attr({name}))`)"
                        ),
                    );
                    Vec::new()
                }
            }
            "variant" | "variant.name" | "variant.pattern" => {
                let Some(v) = ctx.variant else {
                    self.err(k, format!("`@{full}` outside `@for_variants`"));
                    return j;
                };
                if full == "variant.pattern" {
                    self.pattern(v, span)
                } else {
                    vec![tok(TokenKind::Ident(self.t.variants[v].name.clone()), span)]
                }
            }
            "item" => self.t.item.clone(),
            // `@Self.name`: the bare name (no `<T>`), `@Self.name::V { .. }`.
            "item.name" | "Self.name" => vec![tok(TokenKind::Ident(self.t.name.clone()), span)],
            "item.params" | "item.return_type" | "item.body" => {
                let Some((p, r, bd)) = fn_parts(&self.t.item).filter(|_| self.t.kind == "fn")
                else {
                    let kind = self.t.kind;
                    let a = nabic_diagnostic::a_or_an(kind);
                    self.err(k, format!("`@{full}` needs a `fn` item, not {a} {kind}"));
                    return j;
                };
                match full.as_str() {
                    "item.params" => p,
                    "item.return_type" if r.is_empty() => {
                        vec![tok(TokenKind::LParen, span), tok(TokenKind::RParen, span)]
                    }
                    "item.return_type" => r,
                    _ => bd,
                }
            }
            "attribute" => self.t.attr_args.clone(),
            _ => {
                self.err(k, format!("unknown template token `@{full}`"));
                return j;
            }
        };
        if as_str {
            // `@Self.as_str()` is the bare name (no `<T>`).
            let text: String = toks
                .iter()
                .take(1)
                .map(|t| match &t.kind {
                    TokenKind::Ident(n) | TokenKind::IntLit(n) => n.as_str(),
                    _ => "",
                })
                .collect();
            self.out.push(tok(TokenKind::StrLit(text), span));
            return j + 4;
        }
        self.out.extend(toks);
        j
    }

    /// `@concat(a, @Self.name, "s")` at `k`: the pieces expanded and
    /// joined into one identifier; returns the index after it.
    pub(crate) fn concat(&mut self, k: usize, hi: usize, ctx: Ctx) -> usize {
        let b = &self.def.body;
        let open = k + 2;
        let close = match (b.get(open).map(|t| &t.kind), close_of(b, open)) {
            (Some(TokenKind::LParen), Some(end)) if end < hi => end,
            _ => {
                self.err(
                    k,
                    "`@concat` needs `(..)` arguments: `@concat(@Self.name, Columns)`",
                );
                return open;
            }
        };
        let span = b[k].span;
        let mark = self.out.len();
        self.expand(open + 1, close, ctx);
        let mut name = String::new();
        for t in self.out.drain(mark..) {
            match t.kind {
                TokenKind::Comma => {}
                TokenKind::Ident(n) | TokenKind::IntLit(n) | TokenKind::StrLit(n) => {
                    name.push_str(&n);
                }
                other => name.push_str(&other.text()),
            }
        }
        if name.is_empty() || name.starts_with(|c: char| c.is_ascii_digit()) {
            self.err(
                k,
                format!(
                    "`@concat` makes `{name}`, which is not a name: start it with a letter \
                     or `_`"
                ),
            );
            return close + 1;
        }
        let t = self.stamp(tok(TokenKind::Ident(name), span), None);
        self.out.push(t);
        close + 1
    }

    /// The single word argument of `@x(word)`.
    pub(crate) fn word(&self, args: (usize, usize)) -> String {
        self.body()[args.0..args.1]
            .iter()
            .find_map(|t| match &t.kind {
                TokenKind::Ident(w) => Some(w.clone()),
                k => nabic_lexer::token::keyword_str(k).map(str::to_owned),
            })
            .unwrap_or_default()
    }

    pub(crate) fn bool_token(
        &mut self,
        k: usize,
        full: &str,
        args: (usize, usize),
        ctx: Ctx,
    ) -> Option<bool> {
        if let Some(rest) = full
            .strip_prefix("variant.is_")
            .or_else(|| full.strip_prefix("Self.is_"))
        {
            let shape = if full.starts_with("Self") {
                self.t.shape
            } else if let Some(v) = ctx.variant {
                self.t.variants[v].shape
            } else {
                self.err(k, format!("`@{full}` outside `@for_variants`"));
                return None;
            };
            return Some(
                (rest == "unit" && shape == VariantShape::Unit)
                    || (rest == "tuple" && shape == VariantShape::Tuple)
                    || (rest == "named" && shape == VariantShape::Named),
            );
        }
        let name = self.word(args);
        let Some(f) = self.field(ctx) else {
            self.err(
                k,
                format!(
                    "`@{full}` outside `@for_fields`: `@field` tokens work inside \
                             `@for_fields {{ .. }}`"
                ),
            );
            return None;
        };
        Some(match full {
            "field.is_pub" => f.is_pub,
            "field.has_default" => !f.default.is_empty(),
            _ => f.attrs.iter().any(|(n, _)| *n == name),
        })
    }

    /// `@apply_if` condition: `||` / `&&` / `!` / `( )` over template
    /// booleans and `true` / `false`, evaluated now.
    pub(crate) fn cond_or(&mut self, c: &mut Cond, ctx: Ctx) -> Option<bool> {
        let mut v = self.cond_and(c, ctx)?;
        while c.pos < c.end && self.body()[c.pos].kind == TokenKind::PipePipe {
            c.pos += 1;
            let r = self.cond_and(c, ctx)?;
            v = v || r;
        }
        Some(v)
    }

    pub(crate) fn cond_and(&mut self, c: &mut Cond, ctx: Ctx) -> Option<bool> {
        let mut v = self.cond_not(c, ctx)?;
        while c.pos < c.end && self.body()[c.pos].kind == TokenKind::AmpAmp {
            c.pos += 1;
            let r = self.cond_not(c, ctx)?;
            v = v && r;
        }
        Some(v)
    }

    pub(crate) fn cond_not(&mut self, c: &mut Cond, ctx: Ctx) -> Option<bool> {
        if c.pos >= c.end {
            self.err(c.end, "`@apply_if` condition is incomplete");
            return None;
        }
        match self.body()[c.pos].kind.clone() {
            TokenKind::Bang => {
                c.pos += 1;
                self.cond_not(c, ctx).map(|v| !v)
            }
            TokenKind::LParen => {
                let end = close_of(self.body(), c.pos).filter(|e| *e < c.end)?;
                let mut inner = Cond {
                    pos: c.pos + 1,
                    end,
                };
                let v = self.cond_or(&mut inner, ctx);
                c.pos = end + 1;
                v
            }
            TokenKind::True => {
                c.pos += 1;
                Some(true)
            }
            TokenKind::False => {
                c.pos += 1;
                Some(false)
            }
            TokenKind::At => {
                // Expand the one template token and read its boolean.
                let start = self.out.len();
                let at = c.pos;
                c.pos = self.template_token(at, c.end, ctx);
                let got: Vec<Token> = self.out.drain(start..).collect();
                match got.as_slice() {
                    [t] if t.kind == TokenKind::True => Some(true),
                    [t] if t.kind == TokenKind::False => Some(false),
                    _ => {
                        self.err(at, "`@apply_if` takes template booleans (`@field.is_pub`, `@field.has_attr(x)`, ...)");
                        None
                    }
                }
            }
            _ => {
                self.err(c.pos, "unexpected token in `@apply_if` condition");
                None
            }
        }
    }
}

pub(crate) struct Cond {
    pub(crate) pos: usize,
    pub(crate) end: usize,
}

/// A `fn` item's parameter, return-type and body tokens.
pub(crate) fn fn_parts(item: &[Token]) -> Option<(Vec<Token>, Vec<Token>, Vec<Token>)> {
    // Skip attributes, then find `fn name`.
    let mut k = 0;
    while k < item.len() && item[k].kind != TokenKind::Fn {
        if item[k].kind == TokenKind::HashLBracket {
            k = close_of(item, k)?;
        }
        k += 1;
    }
    k += 2; // `fn name`
    if item.get(k).map(|t| &t.kind) == Some(&TokenKind::Lt) {
        let mut depth = 0i32;
        while k < item.len() {
            match item[k].kind {
                TokenKind::Lt => depth += 1,
                TokenKind::Gt => depth -= 1,
                TokenKind::Shr => depth -= 2,
                _ => {}
            }
            k += 1;
            if depth <= 0 {
                break;
            }
        }
    }
    if item.get(k).map(|t| &t.kind) != Some(&TokenKind::LParen) {
        return None;
    }
    let rp = close_of(item, k)?;
    let params = item[k + 1..rp].to_vec();
    let lb = (rp + 1..item.len()).find(|&m| item[m].kind == TokenKind::LBrace)?;
    let ret = if item.get(rp + 1).map(|t| &t.kind) == Some(&TokenKind::Arrow) {
        item[rp + 2..lb]
            .iter()
            .take_while(
                |t| !matches!(&t.kind, TokenKind::Ident(w) if w == "throws" || w == "where"),
            )
            .cloned()
            .collect()
    } else {
        Vec::new()
    };
    let rb = close_of(item, lb)?;
    Some((params, ret, item[lb + 1..rb].to_vec()))
}
