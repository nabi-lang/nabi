//! Modules and `use` trees.

#[allow(clippy::wildcard_imports)]
use crate::parser::*;

impl Parser<'_> {
    pub(crate) fn parse_mod(
        &mut self,
        vis: Visibility,
        mut attrs: Vec<Attribute>,
    ) -> Option<ModDef> {
        let start = self.peek_span();
        self.expect(&TokenKind::Mod, "`mod`")?;
        let name = if let TokenKind::Ident(n) = self.peek().clone() {
            self.bump();
            n
        } else {
            let s = self.peek_span();
            self.error(
                Code::SYNTAX,
                s,
                format!("expected module name, found {}", self.peek().describe()),
            );
            return None;
        };
        let items = if matches!(self.peek(), TokenKind::LBrace) {
            self.bump();
            // `#![..]` / `//!` at the body's head are the module's.
            attrs.extend(self.parse_inner_attrs());
            let mut items = Vec::new();
            while !matches!(self.peek(), TokenKind::RBrace | TokenKind::Eof) {
                let from = self.pos;
                if self.meta_block().is_some() {
                    continue;
                }
                if !self.at_item_start() {
                    let span = self.peek_span();
                    self.error(
                        Code::MISPLACED_ITEM,
                        span,
                        format!(
                            "expected an item in `mod {name}`: module bodies are \
                             definitions-only; statements belong in `main.nabi`"
                        ),
                    );
                    self.recover(from, Recover::NestedItem);
                } else if let Some(item) = self.parse_item() {
                    items.push(item);
                    items.append(&mut self.pending_items);
                } else if !std::mem::take(&mut self.vanished) {
                    self.recover(from, Recover::NestedItem);
                }
            }
            self.expect(&TokenKind::RBrace, "`}`")?;
            Some(items)
        } else {
            self.expect(&TokenKind::Semi, "`;` or `{` after module name")?;
            None
        };
        Some(ModDef {
            vis,
            attrs,
            name,
            items,
            span: start,
        })
    }

    /// `use path[::name | ::* | ::** | ::{ ... } | as alias];`
    pub(crate) fn parse_use(
        &mut self,
        vis: Visibility,
        attrs: Vec<Attribute>,
    ) -> Option<nabic_ast::UseDef> {
        let start = self.peek_span();
        self.expect(&TokenKind::Use, "`use`")?;
        let tree = self.parse_use_tree()?;
        self.expect(&TokenKind::Semi, "`;`")?;
        let Some(tree) = self.strip_meta_imports(tree, &[], vis != Visibility::Private, start)
        else {
            self.vanished = true;
            return None;
        };
        Some(nabic_ast::UseDef {
            vis,
            attrs,
            tree,
            span: start,
        })
    }

    pub(crate) fn parse_use_tree(&mut self) -> Option<nabic_ast::UseTree> {
        // Path prefix - every form starts with at least one segment.
        let prefix_start = self.peek_span();
        let mut segments: Vec<PathSegment> = Vec::new();
        let mut spans = Vec::new();
        // `use ::std::fs;`: from the packages, past any local `std`.
        if self.at_global_path() {
            spans.push(self.peek_span());
            self.bump();
            segments.push(PathSegment {
                name: String::new(),
                generics: Vec::new(),
            });
        }
        loop {
            // `use` roots: an identifier (absolute), `pkg::` or `sup::`. No
            // `self::`: children resolve through the ancestor walk.
            let seg_name = if let TokenKind::Ident(n) = self.peek().clone() {
                spans.push(self.peek_span());
                self.bump();
                n
            } else {
                let s = self.peek_span();
                let help = match self.peek() {
                    TokenKind::SelfLower => Some(
                        "a `use` path has no `self`: name a child module directly \
                         (`use c::x;`), a module itself as `use a::b;`, this package's root \
                         as `pkg::`",
                    ),
                    TokenKind::ColonColon => Some(
                        "a `use` path starts with a name: `pkg::` for this package's root, \
                         `std::` / `core::` / `bare::` for the standard library, a \
                         dependency's name otherwise",
                    ),
                    _ => None,
                };
                let msg = format!("expected an import path, found {}", self.peek().describe());
                match help {
                    Some(h) => self.error_help(Code::SYNTAX, s, msg, h),
                    None => self.error(Code::SYNTAX, s, msg),
                }
                return None;
            };
            segments.push(PathSegment {
                name: seg_name,
                generics: Vec::new(),
            });
            if !matches!(self.peek(), TokenKind::ColonColon) {
                break;
            }
            // Commit only if `*`, `{` or a segment follows; else the `::`
            // is the caller's.
            let after_colons = self.peek_next();
            match after_colons {
                Some(
                    TokenKind::Star | TokenKind::StarStar | TokenKind::LBrace | TokenKind::Ident(_),
                ) => {
                    self.bump(); // consume `::`
                    if matches!(
                        self.peek(),
                        TokenKind::Star | TokenKind::StarStar | TokenKind::LBrace
                    ) {
                        break;
                    }
                }
                _ => break,
            }
        }
        let prefix = Path {
            segments,
            span: prefix_start,
            origin: None,
        };
        // Disambiguate the trailing form.
        let kind = match self.peek().clone() {
            TokenKind::Star | TokenKind::StarStar => {
                let kind = if matches!(self.peek(), TokenKind::StarStar) {
                    nabic_ast::UseTreeKind::GlobRecursive
                } else {
                    nabic_ast::UseTreeKind::Glob
                };
                self.bump();
                if matches!(self.peek(), TokenKind::As) {
                    let s = self.peek_span();
                    self.error_help(
                        Code::SYNTAX,
                        s,
                        "a glob import takes no alias",
                        "import the module under another name instead: `use a as x;`",
                    );
                    return None;
                }
                kind
            }
            TokenKind::LBrace => {
                self.bump();
                let mut children = Vec::new();
                while !matches!(self.peek(), TokenKind::RBrace | TokenKind::Eof) {
                    let child = self.parse_use_tree()?;
                    children.push(child);
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                }
                self.expect(&TokenKind::RBrace, "`}`")?;
                nabic_ast::UseTreeKind::Group(children)
            }
            TokenKind::As => {
                self.bump();
                let alias = if let TokenKind::Ident(n) = self.peek().clone() {
                    self.bump();
                    n
                } else {
                    let s = self.peek_span();
                    self.error(
                        Code::SYNTAX,
                        s,
                        format!(
                            "expected alias name after `as`, found {}",
                            self.peek().describe()
                        ),
                    );
                    return None;
                };
                nabic_ast::UseTreeKind::Simple(Some(alias))
            }
            // `use dep::name!;`: a macro, marked by its `!`
            // until `strip_meta_imports` takes it out.
            TokenKind::Bang => {
                self.bump();
                let mut prefix = prefix;
                if let Some(last) = prefix.segments.last_mut() {
                    last.name.push('!');
                }
                if self.peek() == &TokenKind::As {
                    let s = self.peek_span();
                    let path: Vec<&str> = prefix.segments.iter().map(|s| s.name.as_str()).collect();
                    self.error(
                        Code::SYNTAX,
                        s,
                        format!(
                            "a macro import cannot be renamed: write `use {};`",
                            path.join("::")
                        ),
                    );
                    return None;
                }
                return Some(nabic_ast::UseTree {
                    prefix,
                    spans,
                    kind: nabic_ast::UseTreeKind::Simple(None),
                });
            }
            _ => nabic_ast::UseTreeKind::Simple(None),
        };
        Some(nabic_ast::UseTree {
            prefix,
            spans,
            kind,
        })
    }
}
