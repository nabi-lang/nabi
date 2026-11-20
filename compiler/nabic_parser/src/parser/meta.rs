//! User meta blocks: `#[impl_derive(Tr)]`,
//! `#[impl_derive_attribute(Tr, attr)]` and `#[impl_attribute(name)]`.
//!
//! Bodies stay tokens. A driver pre-pass collects them into a `MetaTable`
//! (`collect_meta_blocks`) before the real parse; a use then expands by
//! substituting template tokens (`@Self`, `@for_fields`, `@field.name`,
//! ...) and parsing the result, to a fixpoint. Tokens keep their origin's
//! spans; errors point at the template with a label at the use.
//!
//! Hygiene: template locals are renamed `x__meta` so they neither
//! capture nor shadow the item's names; enum payloads get generated names.

// Slice indexing and one flat template match; `meta_block` tells "not a
// block" from "a malformed block".
#![allow(
    clippy::many_single_char_names,
    clippy::cognitive_complexity,
    clippy::option_option,
    clippy::type_complexity,
    clippy::too_many_lines
)]

mod apply;
mod template;

pub(crate) use crate::parser::meta::apply::*;
pub(crate) use crate::parser::meta::template::*;

use crate::parser::Parser;
use TokenKind as K;
use nabic_ast::{AttrArg, AttrBody, Attribute, EnumVariantKind, GenericParam, Item, StructKind};
use nabic_core::{HashSet, Visibility};
use nabic_diagnostic::{Code, NabiDiagnostic, Span};
use nabic_lexer::{Token, TokenKind};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MetaKind {
    /// `#[impl_derive]` / `#[impl_derive_attribute]` - used as `#[derive(Name)]`.
    Derive,
    /// `#[impl_attribute]` - used as `#[name]` / `#[name(args)]`.
    Attribute,
    /// `#[impl_macro]` - used as `name!(..)` at a call site.
    Macro,
}

#[derive(Clone, Debug)]
pub struct MetaDef {
    pub kind: MetaKind,
    /// The derive (trait) or attribute name it registers.
    pub name: String,
    /// `#[impl_derive_attribute(Tr, attr)]`'s field attribute.
    pub field_attr: Option<String>,
    /// The keys `#[impl_derive_attribute(Tr, attr(k, ..))]` declares:
    /// every `#[attr(..)]` on the type and its fields must list only
    /// these. `None`: `#[attr(..)]` is not checked.
    pub attr_keys: Option<Vec<String>>,
    pub public: bool,
    /// The defining file (a private block is usable there only).
    pub file: String,
    /// The defining package (`""` for the one being built).
    pub package: String,
    /// The module that holds the block: its template's names resolve
    /// and are access-checked there. Inline `mod`s of the
    /// file until the driver prefixes the file's module.
    pub module: String,
    /// What the defining file's `use`s import (globs end in `*` / `**`):
    /// where the template's bare meta names come from.
    pub imports: Vec<Vec<String>>,
    /// The `#[impl_..]` attribute.
    pub span: Span,
    /// Tokens between the body's braces.
    pub body: Vec<Token>,
    /// The block's own `#[cfg(..)]` attributes: the driver registers it
    /// only when they hold (cfg runs before expansion).
    pub cfg: Vec<Attribute>,
}

impl MetaDef {
    /// The file holding the block is module `module`.
    pub fn set_module(&mut self, module: &str) {
        self.module = match (module.is_empty(), self.module.is_empty()) {
            (true, _) => self.module.clone(),
            (false, true) => module.to_owned(),
            (false, false) => format!("{module}::{}", self.module),
        };
    }

    /// Where the tokens a template writes come from.
    #[must_use]
    pub fn origin(&self) -> Arc<nabic_lexer::Origin> {
        Arc::new(nabic_lexer::Origin {
            package: self.package.clone(),
            module: self.module.clone(),
            imports: self.imports.clone(),
        })
    }

    /// Mount path of the block's package (`""`: the one being built); its
    /// `pkg::` paths keep naming it wherever the block expands.
    pub fn set_package(&mut self, package: &str) {
        package.clone_into(&mut self.package);
        for k in 0..self.body.len() {
            let after_sep = k > 0 && self.body[k - 1].kind == TokenKind::ColonColon;
            let root = matches!(&self.body[k].kind, TokenKind::Ident(n) if n == "pkg")
                && self
                    .body
                    .get(k + 1)
                    .is_some_and(|t| t.kind == TokenKind::ColonColon);
            if root && !after_sep {
                self.body[k].kind = TokenKind::Ident(format!("{PKG_ROOT}{package}"));
            }
        }
    }
}

/// Prefix of the path root a meta block's `pkg::` becomes; `@` keeps it
/// out of reach of source text.
pub const PKG_ROOT: &str = "pkg@";

/// Every meta block of the compilation, by name.
#[derive(Clone, Debug, Default)]
pub struct MetaTable {
    defs: Vec<MetaDef>,
}

/// Attributes the compiler owns; `#[impl_attribute]` can't
/// take their names. Every name the driver checks is here (a driver test
/// holds it).
pub const BUILTIN_ATTRS: &[&str] = &[
    "allow",
    "available",
    "bench",
    "cfg",
    "cfg_attr",
    "cold",
    "deny",
    "deprecated",
    "deprecated_safe",
    "derive",
    "discardable",
    "display",
    "doc",
    "errno",
    "export",
    "export_name",
    "ignore",
    "impl_attribute",
    "impl_derive",
    "impl_derive_attribute",
    "impl_macro",
    "infix",
    "inline",
    "inspect",
    "internal",
    "intrinsic",
    "keyword",
    "lang",
    "leaf",
    "link",
    "link_name",
    "literal",
    "must_use",
    "niche",
    "no_implicit_nabi",
    "no_mangle",
    "no_safepoint",
    "non_exhaustive",
    "on_panic",
    "path",
    "postfix",
    "prefix",
    "renamed_from",
    "repr",
    "returns_not_retained",
    "returns_retained",
    "runtime_attribute",
    "sealed",
    "selector",
    "should_panic",
    "test",
    "track_caller",
    "warn",
];

/// Compiler built-in `!`-forms; a macro cannot take their names.
const BUILTIN_BANGS: &[&str] = &["cfg", "include_bytes", "include_str"];

impl MetaTable {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.defs.is_empty()
    }

    /// Whether `name` is a derive or attribute a meta block registers
    /// (what the parser expands on an item). Macros expand at call sites,
    /// so they are not counted here.
    #[must_use]
    pub fn defines(&self, name: &str) -> bool {
        self.defs
            .iter()
            .any(|d| d.name == name && d.kind != MetaKind::Macro)
    }

    /// Every name the templates of the blocks in `file` write: their
    /// uses, for `dead_code`.
    #[must_use]
    pub fn names_in(&self, file: &str) -> Vec<String> {
        let mut out: Vec<String> = self
            .defs
            .iter()
            .filter(|d| d.file == file)
            .flat_map(|d| &d.body)
            .filter_map(|t| match &t.kind {
                TokenKind::Ident(n) => Some(n.clone()),
                _ => None,
            })
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// The runtime attributes `#[runtime_attribute(name)]` declares, as
    /// `(name, struct)`.
    pub fn runtime_attributes(&self) -> impl Iterator<Item = (&str, &str)> {
        self.defs.iter().filter_map(|d| {
            let ty = d.name.strip_prefix(RUNTIME_ATTR)?;
            Some((d.field_attr.as_deref()?, ty))
        })
    }

    /// The macro `name!` a meta block registers, if any.
    #[must_use]
    pub fn find_macro(&self, name: &str) -> Option<&MetaDef> {
        self.find(MetaKind::Macro, name)
    }

    /// Whether some meta block defines attribute `name` (as the attribute,
    /// a derive's field attribute, or one tested by `@field.has_attr`).
    #[must_use]
    pub fn knows_attribute(&self, name: &str) -> bool {
        let tested = |d: &MetaDef| {
            d.body.windows(3).any(|w| {
                matches!(&w[0].kind, TokenKind::Ident(h) if h == "has_attr")
                    && w[1].kind == TokenKind::LParen
                    && matches!(&w[2].kind, TokenKind::Ident(n) if n == name)
            })
        };
        self.defs.iter().any(|d| {
            (d.kind == MetaKind::Attribute && d.name == name)
                || d.field_attr.as_deref() == Some(name)
                || tested(d)
        })
    }

    #[must_use]
    pub fn find(&self, kind: MetaKind, name: &str) -> Option<&MetaDef> {
        self.defs.iter().find(|d| d.kind == kind && d.name == name)
    }

    /// Every block registering `name` of `kind`, one per package.
    pub fn find_all<'a>(
        &'a self,
        kind: MetaKind,
        name: &'a str,
    ) -> impl Iterator<Item = &'a MetaDef> + 'a {
        self.defs
            .iter()
            .filter(move |d| d.kind == kind && d.name == name)
    }

    /// Register `def`; a second block for the same name in one package,
    /// or one taking a built-in derive / attribute name, is an error. Two
    /// packages may each register a name: a use then says which.
    pub fn add(&mut self, def: MetaDef) -> Option<NabiDiagnostic> {
        let clash = |code: Code, msg: String| NabiDiagnostic::error(code, def.span, msg);
        if let Some(old) = self
            .find_all(def.kind, &def.name)
            .find(|d| d.package == def.package)
        {
            if old.span == def.span && old.file == def.file {
                return None;
            }
            let what = match def.kind {
                MetaKind::Derive => format!("derive `{}`", def.name),
                MetaKind::Attribute => format!("attribute `#[{}]`", def.name),
                MetaKind::Macro => format!("macro `{}!`", def.name),
            };
            return Some(
                clash(
                    Code::DUPLICATE_DEFINITION,
                    format!("{what} is defined by two meta blocks"),
                )
                .with_secondary(old.span, "first defined here"),
            );
        }
        if def.kind == MetaKind::Derive && crate::derive::derivable(&def.name).is_some() {
            return Some(clash(
                Code::INVALID_META_BLOCK,
                format!(
                    "`{}` is a built-in derive; a meta block cannot redefine it",
                    def.name
                ),
            ));
        }
        if def.kind == MetaKind::Attribute && BUILTIN_ATTRS.contains(&def.name.as_str()) {
            return Some(
                clash(
                    Code::INVALID_META_BLOCK,
                    format!(
                        "`#[{}]` is a built-in attribute; a meta block cannot redefine it",
                        def.name
                    ),
                )
                .with_help(format!(
                    "rename it, e.g. `#[impl_attribute(my_{})]`",
                    def.name
                )),
            );
        }
        if def.kind == MetaKind::Macro && BUILTIN_BANGS.contains(&def.name.as_str()) {
            return Some(clash(
                Code::INVALID_META_BLOCK,
                format!(
                    "`{}!` is a built-in form; a macro cannot take its name",
                    def.name
                ),
            ));
        }
        self.defs.push(def);
        None
    }
}

/// The pre-pass: every meta block in `tokens` (a whole file). Malformed
/// blocks are reported by the real parse, so diagnostics are dropped.
#[must_use]
pub fn collect_meta_blocks(tokens: &[Token], file: &str) -> Vec<MetaDef> {
    let mut out = runtime_attributes(tokens, file);
    out.extend(meta_blocks(tokens, file));
    out
}

/// Prefix of the name of the entry a `#[runtime_attribute(name)]` struct
/// makes: no derive or attribute can be written with it.
pub const RUNTIME_ATTR: &str = "#runtime_attribute:";

/// One entry per `#[runtime_attribute(name)] struct S`: it makes
/// `#[name(..)]` known (as a field attribute of no derive), so the
/// attribute checks accept it on types, fields and variants.
fn runtime_attributes(tokens: &[Token], file: &str) -> Vec<MetaDef> {
    let mut out = Vec::new();
    let word = |k: usize| match tokens.get(k).map(|t| &t.kind) {
        Some(K::Ident(n)) => Some(n.clone()),
        _ => None,
    };
    for k in 0..tokens.len() {
        let head = tokens[k].kind == K::HashLBracket
            && word(k + 1).as_deref() == Some("runtime_attribute")
            && tokens.get(k + 2).map(|t| &t.kind) == Some(&K::LParen)
            && tokens.get(k + 4).map(|t| &t.kind) == Some(&K::RParen);
        let Some(name) = word(k + 3).filter(|_| head) else {
            continue;
        };
        let Some(s) = (k + 5..tokens.len().min(k + 64)).find(|&j| tokens[j].kind == K::Struct)
        else {
            continue;
        };
        let Some(ty) = word(s + 1) else { continue };
        out.push(MetaDef {
            kind: MetaKind::Derive,
            name: format!("{RUNTIME_ATTR}{ty}"),
            field_attr: Some(name),
            attr_keys: None,
            public: true,
            file: file.to_owned(),
            package: String::new(),
            module: String::new(),
            imports: Vec::new(),
            span: tokens[k].span,
            body: Vec::new(),
            cfg: Vec::new(),
        });
    }
    out
}

fn meta_blocks(tokens: &[Token], file: &str) -> Vec<MetaDef> {
    let mut p = Parser::new(tokens, file);
    p.scan_meta_imports();
    let imports = std::mem::take(&mut p.meta_imports);
    let inline = inline_mods(tokens);
    let mut out = Vec::new();
    // Open inline `mod`s: the index of their closing `}` and their
    // `#[cfg(..)]`s, which hold for every block inside too.
    let mut mods: Vec<(usize, Vec<Attribute>)> = Vec::new();
    while !p.at_eof() {
        mods.retain(|(end, _)| p.pos < *end);
        if let Some(m) = p.cfg_mod_open() {
            mods.push(m);
            continue;
        }
        match p.meta_block() {
            Some(Some(mut d)) => {
                d.cfg
                    .extend(mods.iter().flat_map(|(_, c)| c.iter().cloned()));
                d.imports.clone_from(&imports);
                let at = p.pos;
                d.module = inline
                    .iter()
                    .filter(|(open, close, _)| *open < at && at < *close)
                    .map(|(.., name)| name.as_str())
                    .collect::<Vec<_>>()
                    .join("::");
                out.push(d);
            }
            Some(None) => {}
            None => {
                p.bump();
            }
        }
    }
    out
}

/// The inline `mod name { .. }`s of a file: their braces and names,
/// outermost first.
fn inline_mods(tokens: &[Token]) -> Vec<(usize, usize, String)> {
    let mut out = Vec::new();
    for k in 0..tokens.len() {
        if tokens[k].kind != TokenKind::Mod {
            continue;
        }
        let (Some(TokenKind::Ident(name)), Some(TokenKind::LBrace)) = (
            tokens.get(k + 1).map(|t| &t.kind),
            tokens.get(k + 2).map(|t| &t.kind),
        ) else {
            continue;
        };
        if let Some(close) = close_of(tokens, k + 2) {
            out.push((k + 2, close, name.clone()));
        }
    }
    out
}

fn is_meta_attr(a: &Attribute) -> bool {
    matches!(
        a.name(),
        "impl_derive" | "impl_attribute" | "impl_derive_attribute" | "impl_macro"
    )
}

fn attr_words(a: &Attribute) -> Vec<String> {
    match &a.body {
        AttrBody::Args(args) => args
            .iter()
            .filter_map(|x| match x {
                AttrArg::Word(w) => Some(w.clone()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// `attr(k, ..)` as the field attribute of `#[impl_derive_attribute]`:
/// the attribute name and the keys it takes.
fn keyed_attr(a: &Attribute) -> Option<(String, Vec<String>)> {
    let AttrBody::Args(args) = &a.body else {
        return None;
    };
    args.iter().find_map(|x| match x {
        AttrArg::Nested { path, body } if path.len() == 1 => {
            let keys = match &**body {
                AttrBody::Args(ks) => ks
                    .iter()
                    .filter_map(|k| match k {
                        AttrArg::Word(w) => Some(w.clone()),
                        _ => None,
                    })
                    .collect(),
                _ => Vec::new(),
            };
            Some((path[0].clone(), keys))
        }
        _ => None,
    })
}

fn meta_err(code: Code, span: Span, msg: impl Into<String>) -> NabiDiagnostic {
    NabiDiagnostic::error(code, span, msg)
}

/// Index of the token closing the group opened at `open`.
fn close_of(toks: &[Token], open: usize) -> Option<usize> {
    let mut depth = 0i32;
    for (k, t) in toks.iter().enumerate().skip(open) {
        match t.kind {
            TokenKind::LParen
            | TokenKind::LBracket
            | TokenKind::LBrace
            | TokenKind::HashLBracket
            | TokenKind::HashBangLBracket => depth += 1,
            TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                depth -= 1;
                if depth == 0 {
                    return Some(k);
                }
            }
            _ => {}
        }
    }
    None
}

/// Index of the token closing the innermost group around `at` (a `{..}`
/// only, with `braces`); the end of `toks` when there is none.
fn enclosing_close(toks: &[Token], at: usize, braces: bool) -> usize {
    let mut depth = 0u32;
    for k in (0..at).rev() {
        match toks[k].kind {
            TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => depth += 1,
            TokenKind::LBrace if depth == 0 => return close_of(toks, k).unwrap_or(toks.len()),
            TokenKind::LParen
            | TokenKind::LBracket
            | TokenKind::HashLBracket
            | TokenKind::HashBangLBracket
                if depth == 0 =>
            {
                if !braces {
                    return close_of(toks, k).unwrap_or(toks.len());
                }
            }
            TokenKind::LParen
            | TokenKind::LBracket
            | TokenKind::LBrace
            | TokenKind::HashLBracket
            | TokenKind::HashBangLBracket => depth -= 1,
            _ => {}
        }
    }
    toks.len()
}

/// Index of the token closing the first `{..}` at or after `from` outside
/// any other group (a `for` loop's body after its `in`).
fn block_after(toks: &[Token], from: usize) -> Option<usize> {
    let mut depth = 0u32;
    for (k, t) in toks.iter().enumerate().skip(from) {
        match t.kind {
            TokenKind::LBrace if depth == 0 => return close_of(toks, k),
            TokenKind::LParen
            | TokenKind::LBracket
            | TokenKind::LBrace
            | TokenKind::HashLBracket
            | TokenKind::HashBangLBracket => depth += 1,
            TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                depth = depth.checked_sub(1)?;
            }
            _ => {}
        }
    }
    None
}

/// First token at or after `from` written at `span`.
fn find_span(toks: &[Token], from: usize, span: Span) -> Option<usize> {
    (from..toks.len()).find(|&k| toks[k].span == span)
}

/// The tokens inside the parentheses of the attribute at `toks[at]`.
fn attr_arg_tokens(toks: &[Token], at: usize) -> Vec<Token> {
    let Some(end) = close_of(toks, at) else {
        return Vec::new();
    };
    match (at..end).find(|&k| toks[k].kind == TokenKind::LParen) {
        Some(lp) => close_of(toks, lp).map_or_else(Vec::new, |rp| toks[lp + 1..rp].to_vec()),
        None => Vec::new(),
    }
}

const fn tok(kind: TokenKind, span: Span) -> Token {
    Token::new(kind, span)
}

fn ident(name: &str, span: Span) -> Token {
    // A tuple field's "name" is its index (`self.0`).
    if name.bytes().all(|b| b.is_ascii_digit()) {
        return tok(TokenKind::IntLit(name.to_owned()), span);
    }
    tok(TokenKind::Ident(name.to_owned()), span)
}

/// Tokens as source text: one statement / brace per line, indented.
/// Long expressions are not line-broken.
pub(crate) fn render(tokens: &[Token]) -> String {
    let mut out = String::new();
    let mut depth = 0usize;
    let mut prev: Option<&K> = None;
    // `},` ends a match arm / struct field: break after the comma too.
    let mut brace_comma = false;
    // Open `[` / `#[` (an attribute's `]` ends its line).
    let mut brackets: Vec<bool> = Vec::new();
    let mut attr_end = false;
    let mut fstr = 0u32;
    for t in tokens {
        let k = &t.kind;
        if *k == K::RBrace {
            depth = depth.saturating_sub(1);
        }
        // A new line after `{` / `;`, and after `}` unless what follows
        // continues it (`},` `})` `} else` `}.m()` ..).
        let continues = matches!(
            k,
            K::Comma
                | K::Semi
                | K::RParen
                | K::RBracket
                | K::Dot
                | K::Question
                | K::Else
                | K::FatArrow
        ) || matches!(k, K::Ident(w) if w == "catch" || w == "finally");
        // F-string pieces stay on one line, glued to their neighbours.
        let in_fstr = fstr > 0;
        match k {
            K::FStrStart(_) => fstr += 1,
            K::FStrEnd(_) => fstr = fstr.saturating_sub(1),
            _ => {}
        }
        let new_line = match prev {
            _ if in_fstr => false,
            None | Some(K::LBrace | K::Semi) => true,
            Some(K::RBracket) if attr_end => true,
            Some(K::Comma) if brace_comma => *k != K::RParen,
            Some(K::RBrace) => !continues,
            _ => *k == K::RBrace,
        };
        if new_line {
            if prev.is_some() {
                out.push('\n');
            }
            out.push_str(&"    ".repeat(depth));
        } else {
            let glued_after = matches!(
                prev,
                Some(
                    K::LParen
                        | K::LBracket
                        | K::Dot
                        | K::ColonColon
                        | K::HashLBracket
                        | K::Bang
                        | K::FStrStart(_)
                        | K::FStrText(_)
                        | K::HoleStart
                        | K::HoleSpec
                        | K::SpecText(_)
                        | K::HoleEnd
                )
            );
            let glued_before = matches!(
                k,
                K::FStrText(_)
                    | K::HoleStart
                    | K::HoleSpec
                    | K::SpecText(_)
                    | K::HoleEnd
                    | K::FStrEnd(_)
                    | K::RParen
                    | K::RBracket
                    | K::Comma
                    | K::Semi
                    | K::Dot
                    | K::ColonColon
                    | K::Colon
                    | K::Question
            ) || (matches!(k, K::LParen | K::LBracket)
                && matches!(
                    prev,
                    Some(
                        K::Ident(_) | K::RParen | K::RBracket | K::Gt | K::SelfLower | K::SelfUpper
                    )
                ));
            if !glued_after && !glued_before {
                out.push(' ');
            }
        }
        match k {
            // A block's `pkg::` as the package it names.
            K::Ident(w) if w.starts_with(PKG_ROOT) => {
                let package = &w[PKG_ROOT.len()..];
                out.push_str(if package.is_empty() { "pkg" } else { package });
            }
            _ => out.push_str(&k.text()),
        }
        if *k == K::LBrace {
            depth += 1;
        }
        brace_comma = *k == K::Comma && prev == Some(&K::RBrace);
        attr_end = false;
        match k {
            K::LBracket => brackets.push(false),
            K::HashLBracket | K::HashBangLBracket => brackets.push(true),
            K::RBracket => attr_end = brackets.pop().unwrap_or(false),
            _ => {}
        }
        prev = Some(k);
    }
    out.push('\n');
    out
}

nabic_core::codec_enum!(MetaKind {
    Derive,
    Attribute,
    Macro
});
nabic_core::codec_struct!(MetaDef {
    kind,
    name,
    field_attr,
    attr_keys,
    public,
    file,
    package,
    module,
    imports,
    span,
    body,
    cfg
});
