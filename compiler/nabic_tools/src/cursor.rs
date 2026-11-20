//! The text and scopes around a cursor, shared by completion, hover and
//! the language server: identifiers, receivers, the edit between two texts
//! and the locals in scope at an offset.

use nabic_ast::{Expr, ExprKind, Item};
use nabic_diagnostic::Span;
use nabic_hir::walk::{AstVisitor, walk_item, walk_stmt};
use nabic_lexer::TokenKind as K;

/// The one changed region between an older and a newer text of a
/// buffer: bytes `pre..old_end` of the old became `pre..new_end`.
pub struct Delta {
    pub pre: usize,
    pub old_end: usize,
    pub new_end: usize,
}

impl Delta {
    pub fn new(old: &str, new: &str) -> Self {
        let (o, n) = (old.as_bytes(), new.as_bytes());
        let mut pre = o.iter().zip(n).take_while(|(a, b)| a == b).count();
        while !old.is_char_boundary(pre) {
            pre -= 1;
        }
        let room = o.len().min(n.len()) - pre;
        let mut suf = o
            .iter()
            .rev()
            .zip(n.iter().rev())
            .take(room)
            .take_while(|(a, b)| a == b)
            .count();
        while !old.is_char_boundary(o.len() - suf) {
            suf -= 1;
        }
        Self {
            pre,
            old_end: o.len() - suf,
            new_end: n.len() - suf,
        }
    }

    /// Where new offset `at` was in the old text; `None` inside or
    /// touching the change.
    pub const fn to_old(&self, at: usize) -> Option<usize> {
        if at < self.pre {
            Some(at)
        } else if at > self.new_end {
            Some(at - self.new_end + self.old_end)
        } else {
            None
        }
    }

    /// Where old offset `at` is in the new text; `None` inside the
    /// change.
    pub const fn to_new(&self, at: usize) -> Option<usize> {
        if at <= self.pre {
            Some(at)
        } else if at >= self.old_end {
            Some(at - self.old_end + self.new_end)
        } else {
            None
        }
    }
}

/// The buffer alone, parsed for its shape (no library operators).
pub fn parse_doc(text: &str, file: &str) -> nabic_ast::Program {
    let (tokens, _) = nabic_lexer::Lexer::new(text, file.to_owned()).lex_all();
    nabic_parser::parse_program(&tokens, file).0
}

pub fn is_ident_char(c: char) -> bool {
    c == '_' || c.is_alphanumeric()
}

/// Where the receiver of the `.` at `dot` starts: back over a chain of
/// identifiers, `.`, `::` and bracketed groups (`a.b(x)[0].`).
pub fn receiver_start(text: &str, dot: usize) -> usize {
    let b = text.as_bytes();
    let (mut i, mut depth) = (dot, 0u32);
    while i > 0 {
        let c = b[i - 1];
        match c {
            b')' | b']' => depth += 1,
            b'(' | b'[' if depth > 0 => depth -= 1,
            _ if depth > 0 => {}
            _ if c.is_ascii_alphanumeric() || c >= 0x80 || b"_.:".contains(&c) => {}
            _ => break,
        }
        i -= 1;
    }
    i
}

/// The identifier around byte `off` and its start.
pub fn ident_at(text: &str, off: usize) -> Option<(&str, usize)> {
    let off = off.min(text.len());
    let start = text[..off]
        .rfind(|c: char| !is_ident_char(c))
        .map_or(0, |i| {
            i + text[i..].chars().next().map_or(1, char::len_utf8)
        });
    let end = text[off..]
        .find(|c: char| !is_ident_char(c))
        .map_or(text.len(), |i| off + i);
    let s = &text[start..end];
    (!s.is_empty() && !s.starts_with(|c: char| c.is_ascii_digit())).then_some((s, start))
}

/// The matching close of a bracket at `open` (`#[` / `#![` count as opens).
pub fn balanced_close(toks: &[nabic_lexer::Token], open: usize) -> Option<usize> {
    let mut depth = 0i32;
    for (i, t) in toks.iter().enumerate().skip(open) {
        match &t.kind {
            K::LParen | K::LBracket | K::LBrace | K::HashLBracket | K::HashBangLBracket => {
                depth += 1;
            }
            K::RParen | K::RBracket | K::RBrace => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Scope walk of the buffer's AST: the binding a one-segment path at
/// `off` refers to.
pub struct Locals {
    off: usize,
    scopes: Vec<Vec<(String, Span)>>,
    /// The declaring span of the binding the path at `off` names.
    pub found: Option<Span>,
    /// Every binding: name and declaring span.
    pub binds: Vec<(String, Span)>,
    /// The names in scope at the innermost expression around `off`.
    around: Vec<String>,
    /// The names in scope at the first construct after `off`.
    after: Option<Vec<String>>,
}

impl Locals {
    pub fn in_scope(&self) -> Vec<String> {
        self.scopes
            .iter()
            .flatten()
            .map(|(n, _)| n.clone())
            .collect()
    }

    /// A construct at `span`: snapshot the scope the first time one
    /// starts past `off`.
    pub fn reach(&mut self, span: Span) {
        if self.after.is_none() && span.start as usize >= self.off {
            self.after = Some(self.in_scope());
        }
    }

    /// The locals in scope at `off` (a statement the parser dropped
    /// still sees what precedes it).
    pub fn visible(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .around
            .iter()
            .chain(self.after.iter().flatten())
            .cloned()
            .collect();
        v.sort();
        v.dedup();
        v
    }
}

impl AstVisitor for Locals {
    fn stmt(&mut self, s: &nabic_ast::Stmt) {
        self.reach(s.span);
    }
    fn expr(&mut self, e: &Expr) {
        self.reach(e.span);
        if (e.span.start as usize..=e.span.end as usize).contains(&self.off) {
            self.around = self.in_scope();
        }
        if let ExprKind::Path(p) = &e.kind {
            if let [seg] = p.segments.as_slice() {
                if (e.span.start as usize..=e.span.end as usize).contains(&self.off) {
                    let hit = self
                        .scopes
                        .iter()
                        .rev()
                        .flat_map(|s| s.iter().rev())
                        .find(|(n, _)| *n == seg.name);
                    if let Some((_, span)) = hit {
                        self.found = Some(*span);
                    }
                }
            }
        }
    }
    fn bind_at(&mut self, name: &str, at: Span) {
        self.reach(at);
        if let Some(s) = self.scopes.last_mut() {
            s.push((name.to_owned(), at));
        }
        self.binds.push((name.to_owned(), at));
    }
    fn push_scope(&mut self) {
        self.scopes.push(Vec::new());
    }
    fn pop_scope(&mut self) {
        self.scopes.pop();
    }
}

pub fn walk_items(v: &mut Locals, items: &[Item]) {
    for item in items {
        match item {
            Item::Mod(m) => walk_items(v, m.items.as_deref().unwrap_or(&[])),
            item => walk_item(v, item),
        }
    }
}

/// The scope walk of `program` for a path at `off`.
pub fn locals(program: &nabic_ast::Program, off: usize) -> Locals {
    let mut v = Locals {
        off,
        scopes: vec![Vec::new()],
        found: None,
        binds: Vec::new(),
        around: Vec::new(),
        after: None,
    };
    walk_items(&mut v, &program.items);
    program.stmts.iter().for_each(|s| walk_stmt(&mut v, s));
    // Nothing past `off`: the top-level scope at the end.
    if v.after.is_none() {
        v.after = Some(v.in_scope());
    }
    v
}

/// The declaring span of the local a one-segment path at `off` names.
pub fn resolve_local(program: &nabic_ast::Program, off: usize) -> Option<Span> {
    locals(program, off).found
}

/// The first whole-word `name` at or after byte `from` of `text`.
pub fn name_range(text: &str, from: usize, name: &str) -> Option<(usize, usize)> {
    let mut at = from;
    while let Some(i) = text.get(at..)?.find(name) {
        let (s, e) = (at + i, at + i + name.len());
        let before = text[..s].chars().next_back().is_some_and(is_ident_char);
        let after = text[e..].chars().next().is_some_and(is_ident_char);
        if !before && !after {
            return Some((s, e));
        }
        at = e;
    }
    None
}
