//! Parser entry point. Recursive descent with Pratt-style operator
//! parsing over the fixed precedence table.

pub mod literal;
pub mod meta;

mod attr;
mod block;
mod closure;
mod cursor;
mod desugar;
mod expand;
mod expr;
mod fns;
mod fstring;
mod habits;
mod impls;
mod imports;
mod item;
mod macros;
mod pattern;
mod primary;
mod stmt;
mod ty;

pub(crate) use crate::parser::expr::*;
pub(crate) use crate::parser::ty::*;
pub use macros::expand_to_text;

use crate::prec::{Assoc, CAST_PREC, COALESCE_PREC, ExtOps, Fixity, PREFIX_PREC};
use meta::{MetaDef, MetaKind, MetaUse};
use nabic_ast::{
    AccessTail, AttrArg, AttrBody, Attribute, Block, CallArg, CatchArm, ConstExpr, EnumDef,
    EnumVariant, EnumVariantKind, Expr, ExprKind, ExternBlock, ExternItem, FieldPattern, FnDef,
    Generics, ImplDef, ImplItem, Item, Literal, MatchArm, ModDef, NamedField, Param, Path,
    PathSegment, Pattern, PatternKind, Program, Stmt, StmtKind, StructDef, StructKind, ThrowsKind,
    TraitDef, TraitItem, TryKind, Ty, TyKind, UnionDef,
};
use nabic_core::Visibility;
use nabic_diagnostic::{Code, NabiDiagnostic, Span};
use nabic_lexer::token::{UNUSED_KEYWORDS, keyword_str};
use nabic_lexer::{Token, TokenKind};

/// `rest` of a `let (..)` tuple pattern: splice index + optional name.
type LetTupleRest = (usize, Option<String>);

#[allow(clippy::struct_excessive_bools)] // independent context flags, not a state machine
pub struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
    file: String,
    pub(crate) diagnostics: Vec<NabiDiagnostic>,
    /// Whether `Path { .. }` is a struct literal here; off in `if` /
    /// `while` / `for` heads so the `{` opens the body.
    struct_expr_allowed: bool,
    /// In a match guard, outside any bracket: `->` there ends the guard
    /// (a mistyped `=>`) instead of being `→`.
    arrow_ends_guard: bool,
    /// One struct literal allowed at the start of a block head, already
    /// reported (E0013), so the rest of the head parses as written.
    head_struct_lit: bool,
    /// `|` mid-type expands to sum-type alternation. Inside closure
    /// parameter lists the closing `|` must end the list instead.
    type_sum_allowed: bool,
    /// What is left of a `>`-prefixed token split to close a generic list
    /// (`>>`, `>=`, `>>=`); served before the next real token.
    pending: Option<TokenKind>,
    /// In `expr as T`: a `<` opens generic args only when a matching `>`
    /// follows, so `x as i64 < y` stays a comparison.
    in_cast_type: bool,
    /// Current expr/type/pattern/block nesting; capped at `MAX_DEPTH`
    /// so hostile input gets a diagnostic instead of a stack overflow.
    depth: u32,
    depth_reported: bool,
    /// Links on the operator / postfix chains enclosing this point.
    chain: u32,
    /// Extra statements from one site (nested `let ((a, b), c)`),
    /// drained by `parse_block` into the same scope.
    pending_stmts: Vec<Stmt>,
    /// Counter for unique temp names of nested tuple destructures.
    next_destructure_temp: u32,
    /// Spans of untyped `throws` in fn types not (yet) claimed by a
    /// `rethrows` signature; reported at the end of the parse.
    untyped_throws: Vec<Span>,
    /// Registered extension operators and their levels.
    ext: ExtOps,
    /// Items `#[derive(..)]` generated for the last item parsed.
    pending_items: Vec<Item>,
    /// User meta blocks, gathered by the driver's pre-pass.
    meta: std::sync::Arc<meta::MetaTable>,
    /// The build's cfg: resolves `#[cfg_attr]` and `cfg!(..)`
    /// before meta expansion. `None` outside a build (tools, pre-pass).
    cfg: Option<crate::cfg::CfgEnv>,
    /// Meta expansions in progress (cycle detection).
    meta_stack: Vec<String>,
    /// Source text of the meta expansions so far (`Program::expanded`).
    expanded: Vec<String>,
    /// `#[literal]` registrations, gathered by the pre-pass.
    literals: std::sync::Arc<literal::LiteralTable>,
    /// The last `parse_item` consumed an item that expanded to nothing.
    vanished: bool,
    /// Nested `fn`s and block-local items, lifted
    /// to module items once the enclosing item is parsed.
    lifted: Vec<Item>,
    /// Names of the `fn`s being parsed, outermost first (`Type.m` for
    /// a method).
    fn_names: Vec<String>,
    /// The impl target's or trait's name while parsing its items.
    fn_owner: Option<String>,
    /// `CFG_OFF_ATTR` markers for the methods the impl being parsed
    /// dropped.
    cfg_off: Vec<Attribute>,
    /// Lifted names given out so far.
    lifted_names: nabic_core::HashSet<String>,
    /// The next primary starts a statement: if it is block-like, `(` / `[`
    /// after it begin a new statement (`while c {} (a, b)`), not a call.
    stmt_head: bool,
    /// `(token, inner)` whose doc comments became attributes.
    docs_used: nabic_core::HashSet<(usize, bool)>,
    /// Doc comments (by start) on what keeps no doc (a statement, a
    /// match arm, ..): what, and whether that is an error.
    docs_dropped: nabic_core::HashMap<u32, (&'static str, bool)>,
    /// Token ranges no parse looks into (meta block bodies).
    unparsed: Vec<std::ops::Range<usize>>,
    /// The next `bump` takes a name where any keyword is one (`p.move`,
    /// a field): an unused keyword is fine there.
    name_pos: bool,
    /// Per closure or fn body being parsed, innermost last: the label
    /// `return@name` may name there, and the bare `return;`s in it.
    ret_frames: Vec<RetFrame>,
    /// The bare `return;`s of each trailing closure, by its span start:
    /// one in a statement's trailing closure is an error.
    closure_returns: nabic_core::HashMap<(u32, u32), Vec<Span>>,
    /// Start of the expression statement being parsed: a trailing-closure
    /// call starting it ends it at its `}`.
    stmt_at: Option<u32>,
    /// The bindings of each `using x = e;` statement, by its marker's
    /// span start, until its block is done.
    using_stmts: nabic_core::HashMap<(u32, u32), Vec<UsingBinding>>,
    /// Parameter patterns parsed so far, each with the temporary that
    /// stands for its parameter; a fn or closure takes its own.
    param_pats: Vec<(String, Pattern)>,
    /// Mount path of the package being parsed (`""`: the one being
    /// built); `pkg::name!` names its meta blocks.
    package: String,
    /// Paths the file's `use`s import (a glob ends in `*`): where a
    /// bare meta name may come from.
    meta_imports: Vec<Vec<String>>,
}

/// A closure or fn body being parsed.
#[derive(Default)]
struct RetFrame {
    /// A trailing closure's callee name: what `return@name` names.
    label: Option<String>,
    /// The `return` tokens of the bare `return;`s directly in it.
    bare: Vec<Span>,
}

/// Nesting limit for expressions, types, patterns and blocks.
const MAX_DEPTH: u32 = 256;
/// One `using` binding: its name (a temporary for a pattern), where it
/// starts, the value and the pattern.
type UsingBinding = (String, Span, Expr, Option<Pattern>);

/// `in` / `!in`: the operator, its token count and a misspelling's
/// error (where, what).
type MemberOp = (&'static str, usize, Option<(Span, &'static str)>);

/// Links in one left-nested operator / postfix chain.
const MAX_CHAIN: u32 = 4096;
/// The path of the type a variadic `xs: ...T` parameter has in the body
/// (`Slice<T>`).
pub const VARIADIC_SLICE: [&str; 5] = ["", "core", "collections", "slice", "Slice"];

/// `Slice<elem>`: a variadic `...elem` parameter's type.
#[must_use]
pub fn variadic_slice_ty(elem: Ty) -> Ty {
    let span = elem.span;
    let n = VARIADIC_SLICE.len();
    Ty {
        kind: TyKind::Path(Path {
            segments: VARIADIC_SLICE
                .iter()
                .enumerate()
                .map(|(k, name)| PathSegment {
                    name: (*name).to_owned(),
                    generics: if k + 1 == n {
                        vec![elem.clone()]
                    } else {
                        Vec::new()
                    },
                })
                .collect(),
            span,
            origin: None,
        }),
        span,
    }
}

/// The file an `include_str!` / `include_bytes!` in `file` names by
/// the string literal body `lit`: relative to `file`'s directory.
#[must_use]
pub fn include_path(file: &str, lit: &str) -> std::path::PathBuf {
    let rel = nabic_lexer::escape::unescape(lit, nabic_lexer::escape::EscapeMode::Str)
        .unwrap_or_else(|_| lit.to_owned());
    std::path::Path::new(file)
        .parent()
        .unwrap_or_else(|| std::path::Path::new(""))
        .join(rel)
}

/// The literal `include_bytes!` (`bytes`) or `include_str!` gives for
/// the path literal `lit` in `file`, or why not.
pub fn include_file(file: &str, lit: &str, bytes: bool) -> Result<Literal, String> {
    let data = std::fs::read(include_path(file, lit)).map_err(|e| {
        let why = if e.kind() == std::io::ErrorKind::NotFound {
            "no such file".to_owned()
        } else {
            e.to_string()
        };
        format!("cannot read \"{lit}\": {why}")
    })?;
    if bytes {
        return Ok(Literal::ByteStr(nabic_lexer::escape::escape_bytes(&data)));
    }
    match String::from_utf8(data) {
        Ok(s) => Ok(Literal::Str(nabic_lexer::escape::escape_str(&s))),
        Err(e) => Err(format!(
            "\"{lit}\" is not UTF-8 (invalid byte at offset {}); use `include_bytes!`",
            e.utf8_error().valid_up_to()
        )),
    }
}

/// `MAX_DEPTH` levels need ~5 MiB (the dev profile builds the parser at
/// opt-level 1; ~4 MiB at opt-level 3: `Expr` values are large).
const PARSER_STACK: usize = 16 << 20;

pub fn parse_program(tokens: &[Token], file: impl Into<String>) -> (Program, Vec<NabiDiagnostic>) {
    parse_program_ext(tokens, file, &ExtOps::default())
}

/// `parse_program` that also knows registered extension operators.
pub fn parse_program_ext(
    tokens: &[Token],
    file: impl Into<String>,
    ext: &ExtOps,
) -> (Program, Vec<NabiDiagnostic>) {
    parse_program_meta(
        tokens,
        file,
        "",
        ext,
        &std::sync::Arc::default(),
        &std::sync::Arc::default(),
        None,
    )
}

/// `parse_program_ext` that also expands user meta blocks
/// and matches `#[literal]` patterns.
pub fn parse_program_meta(
    tokens: &[Token],
    file: impl Into<String>,
    package: &str,
    ext: &ExtOps,
    metas: &std::sync::Arc<meta::MetaTable>,
    literals: &std::sync::Arc<literal::LiteralTable>,
    cfg: Option<&crate::cfg::CfgEnv>,
) -> (Program, Vec<NabiDiagnostic>) {
    let file = file.into();
    let ext = ext.clone();
    let metas = std::sync::Arc::clone(metas);
    let literals = std::sync::Arc::clone(literals);
    let cfg = cfg.cloned();
    let package = package.to_owned();
    let run = move || {
        let mut p = Parser::new(tokens, file);
        p.ext = ext;
        p.package = package;
        p.meta = metas;
        p.literals = literals;
        p.cfg = cfg;
        let program = p.parse_program();
        (program, p.diagnostics)
    };
    on_parser_stack(run)
}

/// A file that is one expression and nothing else (a `.nabion` file: the package
/// manifest): parse it without top-level statement rules (no `;`).
pub fn parse_expr_file(
    tokens: &[Token],
    file: impl Into<String>,
) -> (Option<Expr>, Vec<NabiDiagnostic>) {
    let file = file.into();
    on_parser_stack(move || {
        let mut p = Parser::new(tokens, file);
        let e = p.parse_expr();
        if e.is_some() && !matches!(p.peek(), TokenKind::Eof) {
            let span = p.peek_span();
            p.error(
                Code::SYNTAX,
                span,
                "expected the end of the file after the expression",
            );
        }
        (e, p.diagnostics)
    })
}

/// `MAX_DEPTH` recursion needs more than a default 1-2 MiB stack, so
/// parse on a thread sized for the cap (only address space until used).
fn on_parser_stack<T: Send>(run: impl FnOnce() -> T + Send) -> T {
    std::thread::scope(|s| {
        std::thread::Builder::new()
            .stack_size(PARSER_STACK)
            .spawn_scoped(s, run)
            .expect("spawn parser thread")
            .join()
            .unwrap_or_else(|e| std::panic::resume_unwind(e))
    })
}

impl<'a> Parser<'a> {
    pub fn new(tokens: &'a [Token], file: impl Into<String>) -> Self {
        Self {
            tokens,
            pos: 0,
            file: file.into(),
            diagnostics: Vec::new(),
            struct_expr_allowed: true,
            arrow_ends_guard: false,
            head_struct_lit: false,
            stmt_head: false,
            type_sum_allowed: true,
            pending: None,
            in_cast_type: false,
            depth: 0,
            depth_reported: false,
            chain: 0,
            pending_stmts: Vec::new(),
            next_destructure_temp: 0,
            untyped_throws: Vec::new(),
            ext: ExtOps::default(),
            pending_items: Vec::new(),
            meta: std::sync::Arc::default(),
            cfg: None,
            meta_stack: Vec::new(),
            expanded: Vec::new(),
            literals: std::sync::Arc::default(),
            vanished: false,
            lifted: Vec::new(),
            fn_names: Vec::new(),
            fn_owner: None,
            cfg_off: Vec::new(),
            lifted_names: nabic_core::HashSet::default(),
            docs_used: nabic_core::HashSet::default(),
            docs_dropped: nabic_core::HashMap::default(),
            unparsed: Vec::new(),
            name_pos: false,
            stmt_at: None,
            using_stmts: nabic_core::HashMap::default(),
            param_pats: Vec::new(),
            ret_frames: Vec::new(),
            closure_returns: nabic_core::HashMap::default(),
            package: String::new(),
            meta_imports: Vec::new(),
        }
    }

    /// Does an `else` sit at this statement's top level, before its `;`
    /// (`let (a, b) = e else { .. };`)? An `if .. else` in the
    /// initializer also says yes; the general `let` form handles that.
    fn stmt_has_else(&self) -> bool {
        let mut depth = 0usize;
        for t in &self.tokens[self.pos..] {
            match &t.kind {
                TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => depth += 1,
                TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                    if depth == 0 {
                        return false;
                    }
                    depth -= 1;
                }
                TokenKind::Else if depth == 0 => return true,
                TokenKind::Semi | TokenKind::Eof if depth == 0 => return false,
                _ => {}
            }
        }
        false
    }

    fn backtracks<T>(&mut self, f: impl FnOnce(&mut Self) -> Option<T>) -> bool {
        let (pos, n_diags, n_pending, n_lifted, n_pats) = (
            self.pos,
            self.diagnostics.len(),
            self.pending_stmts.len(),
            self.lifted.len(),
            self.param_pats.len(),
        );
        let failed = f(self).is_none();
        self.pos = pos;
        self.diagnostics.truncate(n_diags);
        self.pending_stmts.truncate(n_pending);
        self.lifted.truncate(n_lifted);
        self.param_pats.truncate(n_pats);
        failed
    }

    pub(crate) fn at_eof(&self) -> bool {
        matches!(self.peek(), TokenKind::Eof)
    }

    pub(crate) const fn position(&self) -> usize {
        self.pos
    }

    pub(crate) fn parse_item_pub(&mut self) -> Option<Item> {
        self.parse_item()
    }

    /// Run `f` with `struct_expr_allowed` = `allowed`, restoring it even
    /// when `f` bails out with `None`. `allowed` is a bracket's inside,
    /// where a guard's `->` is `→` again.
    fn with_struct_expr<T>(&mut self, allowed: bool, f: impl FnOnce(&mut Self) -> T) -> T {
        let prev = std::mem::replace(&mut self.struct_expr_allowed, allowed);
        let guard = self.arrow_ends_guard;
        self.arrow_ends_guard = guard && !allowed;
        let r = f(self);
        self.struct_expr_allowed = prev;
        self.arrow_ends_guard = guard;
        r
    }

    /// A match guard: `n if n > 2 -> 1` reads `->` as a mistyped `=>`.
    fn parse_guard(&mut self) -> Option<Expr> {
        let prev = std::mem::replace(&mut self.arrow_ends_guard, true);
        let r = self.parse_expr();
        self.arrow_ends_guard = prev;
        r
    }

    /// Same as `with_struct_expr` for `type_sum_allowed`.
    fn with_type_sum<T>(&mut self, allowed: bool, f: impl FnOnce(&mut Self) -> T) -> T {
        let prev = std::mem::replace(&mut self.type_sum_allowed, allowed);
        let r = f(self);
        self.type_sum_allowed = prev;
        r
    }
}

impl Parser<'_> {
    /// Recover from a failed item / statement that began at `from`: one
    /// diagnostic per mistake. Close braces opened since `from` (not
    /// parens / brackets: an unclosed one is often the error), then stop
    /// after `;`, before an item or statement keyword, or at the enclosing
    /// `}`, consumed only at top level or when it closes the failed item.
    fn recover(&mut self, from: usize, ctx: Recover) {
        self.pending = None;
        let opened: i32 = self.tokens[from.min(self.pos)..self.pos]
            .iter()
            .map(|t| match t.kind {
                TokenKind::LBrace => 1,
                TokenKind::RBrace => -1,
                _ => 0,
            })
            .sum();
        let mut depth = opened.max(0);
        if self.pos == from {
            // Failed on its first token: make progress.
            match self.bump().kind {
                TokenKind::LBrace => depth += 1,
                TokenKind::Semi | TokenKind::RBrace if depth == 0 => return,
                _ => {}
            }
        }
        loop {
            let kind = self.peek();
            match kind {
                TokenKind::Eof => return,
                TokenKind::LBrace => depth += 1,
                TokenKind::RBrace if depth == 0 => {
                    if ctx == Recover::TopItem {
                        self.bump();
                    }
                    return;
                }
                TokenKind::RBrace => {
                    depth -= 1;
                    if depth == 0 && ctx != Recover::Stmt {
                        self.bump();
                        return;
                    }
                }
                TokenKind::Semi if depth == 0 => {
                    self.bump();
                    return;
                }
                _ if depth == 0 && ctx != Recover::Stmt && starts_item(kind) => return,
                _ if depth == 0 && ctx == Recover::Stmt && starts_stmt(kind) => return,
                _ => {}
            }
            self.bump();
        }
    }

    /// Run a recursive production one nesting level deeper, or report
    /// (once) and fail when `MAX_DEPTH` is reached.
    fn nested<T>(&mut self, f: impl FnOnce(&mut Self) -> Option<T>) -> Option<T> {
        if self.depth >= MAX_DEPTH {
            if !self.depth_reported {
                self.depth_reported = true;
                let span = self.peek_span();
                self.error(
                    Code::NESTING_LIMIT,
                    span,
                    format!("nesting too deep (limit {MAX_DEPTH})"),
                );
            }
            return None;
        }
        self.depth += 1;
        let r = f(self);
        self.depth -= 1;
        r
    }

    /// Count one more link of a left-nested chain (`1 + 1 + ..`,
    /// `a.f().g()`), which deepens the tree without `nested`.
    fn chain_link(&mut self) -> Option<()> {
        self.chain += 1;
        if self.chain <= MAX_CHAIN {
            return Some(());
        }
        if !self.depth_reported {
            self.depth_reported = true;
            let span = self.peek_span();
            self.error(
                Code::NESTING_LIMIT,
                span,
                format!("expression chain too long (limit {MAX_CHAIN})"),
            );
        }
        None
    }

    /// From a path (`a::E`) at the cursor: is its `{ .. }` group followed
    /// by another `{`?
    fn braces_then_block(&self) -> bool {
        self.braces_then(true)
    }

    /// `Path { .. };` / `Path { .. },` at the cursor: a struct literal
    /// ending a `using` statement's binding.
    fn braces_then_semi(&self) -> bool {
        let mut i = self.pos;
        let kind = |i: usize| self.tokens.get(i).map(|t| &t.kind);
        while matches!(kind(i), Some(TokenKind::Ident(_) | TokenKind::ColonColon)) {
            i += 1;
        }
        if i == self.pos || !matches!(kind(i), Some(TokenKind::LBrace)) {
            return false;
        }
        // Fields (`{ a: 1 }`), or `{}` ending the statement: not a body.
        let fields = matches!(
            (kind(i + 1), kind(i + 2)),
            (Some(TokenKind::Ident(_)), Some(TokenKind::Colon))
                | (Some(TokenKind::RBrace), Some(TokenKind::Semi))
        );
        if !fields {
            return false;
        }
        let mut depth = 0u32;
        while let Some(k) = kind(i) {
            match k {
                TokenKind::LBrace => depth += 1,
                TokenKind::RBrace => {
                    depth -= 1;
                    if depth == 0 {
                        return matches!(kind(i + 1), Some(TokenKind::Semi | TokenKind::Comma));
                    }
                }
                _ => {}
            }
            i += 1;
        }
        false
    }

    /// `braces_then_block`; with `strict` off, a continued head
    /// (`S { a: 1 }.ok() {`) counts too.
    fn braces_then(&self, strict: bool) -> bool {
        let mut i = self.pos;
        let kind = |i: usize| self.tokens.get(i).map(|t| &t.kind);
        while matches!(kind(i), Some(TokenKind::Ident(_) | TokenKind::ColonColon)) {
            i += 1;
        }
        if i == self.pos || !matches!(kind(i), Some(TokenKind::LBrace)) {
            return false;
        }
        let mut depth = 0u32;
        while let Some(k) = kind(i) {
            match k {
                TokenKind::LBrace => depth += 1,
                TokenKind::RBrace => {
                    depth -= 1;
                    if depth == 0 {
                        return matches!(kind(i + 1), Some(TokenKind::LBrace))
                            || !strict && self.continued_head(i + 1);
                    }
                }
                _ => {}
            }
            i += 1;
        }
        false
    }

    /// At token `i`: a member access or an operator, then (at bracket
    /// depth 0) the `{` of the block.
    fn continued_head(&self, i: usize) -> bool {
        let kind = |i: usize| self.tokens.get(i).map(|t| &t.kind);
        if !matches!(
            kind(i),
            Some(
                TokenKind::Dot
                    | TokenKind::Gt
                    | TokenKind::Lt
                    | TokenKind::GtEq
                    | TokenKind::LtEq
                    | TokenKind::EqEq
                    | TokenKind::NotEq
            )
        ) {
            return false;
        }
        let mut depth = 0i32;
        for j in i.. {
            match kind(j) {
                Some(TokenKind::LParen | TokenKind::LBracket) => depth += 1,
                Some(TokenKind::RParen | TokenKind::RBracket) => depth -= 1,
                Some(TokenKind::LBrace) if depth == 0 => return true,
                None | Some(TokenKind::Semi | TokenKind::LBrace | TokenKind::RBrace) => {
                    return false;
                }
                _ => {}
            }
        }
        false
    }

    /// A `fn(..)` type's parameter types through `)`. It names no
    /// parameters and is never `rethrows`.
    fn fn_type_params(&mut self) -> Option<Vec<Ty>> {
        if matches!(self.peek(), TokenKind::Ident(_))
            && matches!(self.peek_next(), Some(TokenKind::Colon))
        {
            let s = self.peek_span();
            self.error_help(
                Code::SYNTAX,
                s,
                "a function type has no parameter names",
                "write the types only: `fn(i32, str) -> bool`",
            );
            return None;
        }
        let args = self.nested_type_list(TokenKind::RParen)?;
        self.expect(&TokenKind::RParen, "`)`")?;
        if self.at_kw("rethrows") {
            let s = self.peek_span();
            self.error_help(
                Code::SYNTAX,
                s,
                "a function type cannot be `rethrows`",
                "write the error it throws: `fn(i32) throws (E) -> i32`",
            );
            return None;
        }
        Some(args)
    }

    /// `S { a: 1 } {` at the cursor: a struct literal at the top of a
    /// block head, where its `{` reads as the body.
    fn struct_lit_head(&self) -> bool {
        if !self.braces_then(false) {
            return false;
        }
        let kind = |i: usize| self.tokens.get(i).map(|t| &t.kind);
        let mut i = self.pos;
        while matches!(kind(i), Some(TokenKind::Ident(_) | TokenKind::ColonColon)) {
            i += 1;
        }
        if !matches!(
            (kind(i + 1), kind(i + 2)),
            (Some(TokenKind::Ident(_)), Some(TokenKind::Colon))
        ) {
            return false;
        }
        // A match arm `x: T => ..` is a body, not fields.
        let mut depth = 0u32;
        while let Some(k) = kind(i) {
            match k {
                TokenKind::LBrace => depth += 1,
                TokenKind::RBrace => {
                    depth -= 1;
                    if depth == 0 {
                        // `S { a: 1 } {` or `S { a: 1 }.ok() {`.
                        return matches!(kind(i + 1), Some(TokenKind::LBrace))
                            || self.continued_head(i + 1);
                    }
                }
                TokenKind::FatArrow if depth == 1 => return false,
                _ => {}
            }
            i += 1;
        }
        false
    }

    /// The expression heading a block (`if` / `while` / `match` / `for` /
    /// `using`), where a struct literal needs parentheses.
    fn parse_head(&mut self) -> Option<Expr> {
        if !self.struct_lit_head() {
            return self.with_struct_expr(false, Self::parse_expr);
        }
        // The literal's closing `}`.
        let close = (self.pos..self.tokens.len())
            .find(|&i| matches!(self.tokens[i].kind, TokenKind::LBrace))
            .and_then(|open| {
                let mut depth = 0u32;
                (open..self.tokens.len()).find(|&i| {
                    match self.tokens[i].kind {
                        TokenKind::LBrace => depth += 1,
                        TokenKind::RBrace => depth -= 1,
                        _ => {}
                    }
                    depth == 0
                })
            });
        if let Some(close) = close.filter(|&c| {
            !matches!(
                self.tokens.get(c + 1).map(|t| &t.kind),
                Some(TokenKind::LBrace)
            )
        }) {
            // `S { a: 1 }.ok() {`: the literal, then the rest of the head.
            let lit_span = Span {
                end: self.tokens[close].span.end,
                ..self.peek_span()
            };
            self.head_struct_lit = true;
            let head = self.with_struct_expr(false, Self::parse_expr);
            self.head_struct_lit = false;
            let text = self.text_in(lit_span);
            self.error_help(
                Code::STRUCT_LITERAL_IN_HEAD,
                lit_span,
                "a struct literal can't start a block's head: its `{` would open the body",
                format!("wrap the struct literal in parentheses: `({text})`"),
            );
            return head;
        }
        let lit = self.with_struct_expr(true, Self::parse_primary)?;
        let text = self.text_in(lit.span);
        self.error_help(
            Code::STRUCT_LITERAL_IN_HEAD,
            lit.span,
            "a struct literal can't start a block's head: its `{` would open the body",
            format!("wrap the struct literal in parentheses: `({text})`"),
        );
        Some(lit)
    }

    /// The code of the tokens inside `span`, spaced as written.
    fn text_in(&self, span: Span) -> String {
        let mut out = String::new();
        let mut end = None;
        for t in self.tokens.iter().filter(|t| {
            t.span.file == span.file && t.span.start >= span.start && t.span.end <= span.end
        }) {
            if end.is_some_and(|e| e < t.span.start) {
                out.push(' ');
            }
            out.push_str(&t.kind.text());
            end = Some(t.span.end);
        }
        out
    }

    fn error_help(
        &mut self,
        code: Code,
        span: Span,
        msg: impl Into<String>,
        help: impl Into<String>,
    ) {
        self.diagnostics.push(
            NabiDiagnostic::error(code, span, msg)
                .with_file(self.file.clone())
                .with_help(help),
        );
    }
}

impl Parser<'_> {
    pub fn parse_program(&mut self) -> Program {
        let mut items = Vec::new();
        let mut stmts = Vec::new();
        let mut tops = Vec::new();
        let attrs = self.parse_inner_attrs();
        if !self.meta.is_empty() && self.meta_stack.is_empty() {
            self.scan_meta_imports();
        }
        while !matches!(self.peek(), TokenKind::Eof) {
            let from = self.pos;
            if self.meta_block().is_some() || self.eat(&TokenKind::Semi) {
                continue;
            }
            let at_item =
                (self.at_item_start() || self.at_item_macro_call()) && !self.at_attributed_stmt();
            tops.push((self.peek_span().start, at_item));
            if at_item {
                if let Some(item) = self.parse_item() {
                    items.push(item);
                    items.append(&mut self.pending_items);
                } else if !std::mem::take(&mut self.vanished) {
                    self.recover(from, Recover::TopItem);
                }
                continue;
            }
            // Not a statement: it would also be one beside `fn main`.
            if self.skip_python_def() {
                continue;
            }
            let attrs = self.cfg_only_attrs("a statement");
            let marks = (self.pending_stmts.len(), self.lifted.len());
            if let Some(stmt) = self.parse_top_stmt() {
                if self.cfg_drops(&attrs, marks) {
                    continue;
                }
                stmts.push(stmt);
                while let Some(extra) = self.pending_stmts.pop() {
                    stmts.push(extra);
                }
                items.append(&mut self.lifted);
            } else {
                self.pending_stmts.clear();
                self.recover(from, Recover::Stmt);
            }
        }
        let mut tail = None;
        self.fold_using_stmts(&mut stmts, &mut tail);
        if let Some(e) = tail {
            stmts.push(Stmt {
                span: e.span,
                kind: StmtKind::Expr(*e),
            });
        }
        self.statement_closure_returns(&stmts, None);
        // Untyped `throws` in fn types that no `rethrows` claimed keep the
        // marker; HIR reads it as `any Error`.
        self.untyped_throws.clear();
        self.misplaced_docs();
        Program {
            attrs,
            items,
            stmts,
            expanded: std::mem::take(&mut self.expanded),
            tops,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Recover {
    /// Program level: a stray `}` is consumed.
    TopItem,
    /// Inside `impl` / `trait` / `mod` / `extern` braces.
    NestedItem,
    /// Inside a block.
    Stmt,
}

const fn starts_item(k: &TokenKind) -> bool {
    matches!(
        k,
        TokenKind::Fn
            | TokenKind::Struct
            | TokenKind::Enum
            | TokenKind::Trait
            | TokenKind::Impl
            | TokenKind::Mod
            | TokenKind::Use
            | TokenKind::Pub
            | TokenKind::HashLBracket
            | TokenKind::Unsafe
            | TokenKind::Async
    )
}

/// May omit its `;`: a block-like expression or a call with a
/// block-bodied trailing closure (`with_lock(m) { .. }`); the
/// value is discarded.
const fn ends_stmt(e: &Expr) -> bool {
    let (ExprKind::Call {
        trailing_closure: Some(c),
        ..
    }
    | ExprKind::MethodCall {
        trailing_closure: Some(c),
        ..
    }) = &e.kind
    else {
        return is_block_like(e);
    };
    matches!(&c.kind, ExprKind::Closure { body, .. } if matches!(body.kind, ExprKind::Block(_)))
}

/// The name `return@name` gives the trailing closure of call `e`: the
/// method's, or a path callee's last segment.
fn callee_name(e: &Expr) -> Option<String> {
    match &e.kind {
        ExprKind::Call { callee, .. } => path_name(callee),
        ExprKind::MethodCall { method, .. } => Some(method.clone()),
        _ => None,
    }
}

/// A path expression's last segment.
fn path_name(e: &Expr) -> Option<String> {
    match &e.kind {
        ExprKind::Path(p) => p.segments.last().map(|s| s.name.clone()),
        _ => None,
    }
}

/// `b'x'`: the `u8` literal of its byte (the lexer checked the body).
fn byte_lit(body: &str) -> Literal {
    let v = nabic_lexer::escape::unescape(body, nabic_lexer::escape::EscapeMode::CChar)
        .ok()
        .and_then(|s| s.chars().next())
        .map_or(0, u32::from);
    Literal::Int(format!("{v}u8"))
}

const fn is_block_like(e: &Expr) -> bool {
    // `try for x in g { .. }` (a throwing iterator) ends like its loop.
    if let ExprKind::Try { operand, .. } = &e.kind {
        return matches!(operand.kind, ExprKind::For { .. });
    }
    matches!(
        e.kind,
        ExprKind::Block(_)
            | ExprKind::Unsafe(_)
            | ExprKind::ConstBlock(_)
            | ExprKind::If { .. }
            | ExprKind::Match { .. }
            | ExprKind::Loop { .. }
            | ExprKind::While { .. }
            | ExprKind::For { .. }
            | ExprKind::Catch { .. }
    )
}

const fn starts_stmt(k: &TokenKind) -> bool {
    matches!(
        k,
        TokenKind::Let
            | TokenKind::Return
            | TokenKind::Break
            | TokenKind::Continue
            | TokenKind::Throw
            | TokenKind::Use
    )
}

/// The base operator symbol of a compound-assign token.
const fn compound_assign_op(tok: &nabic_lexer::TokenKind) -> Option<&'static str> {
    Some(match tok {
        TokenKind::PlusEq => "+",
        TokenKind::MinusEq => "-",
        TokenKind::StarEq => "*",
        TokenKind::StarStarEq => "**",
        TokenKind::SlashEq => "/",
        TokenKind::PercentEq => "%",
        TokenKind::PercentPercentEq => "%%",
        TokenKind::AmpEq => "&",
        TokenKind::PipeEq => "|",
        TokenKind::CaretEq => "^",
        TokenKind::ShlEq => "<<",
        TokenKind::ShrEq => ">>",
        TokenKind::DotEq => ".",
        TokenKind::DotPlusEq => ".+",
        TokenKind::DotMinusEq => ".-",
        TokenKind::DotStarEq => ".*",
        TokenKind::DotStarStarEq => ".**",
        TokenKind::DotSlashEq => "./",
        TokenKind::DotPercentEq => ".%",
        TokenKind::DotPercentPercentEq => ".%%",
        TokenKind::DotAmpEq => ".&",
        TokenKind::DotPipeEq => ".|",
        TokenKind::DotCaretEq => ".^",
        TokenKind::DotShlEq => ".<<",
        TokenKind::DotShrEq => ".>>",
        _ => return None,
    })
}

/// Keywords of other languages and their Nabi spelling.
fn keyword_alias(word: &str) -> Option<&'static str> {
    Some(match word {
        "func" | "fun" | "function" | "def" | "fucn" | "fnc" => "fn",
        "elif" | "elsif" | "elseif" => "else if",
        "switch" => "match",
        "var" | "val" => "let",
        "null" | "nil" | "None" | "NULL" => "none",
        "class" => "struct",
        "interface" | "protocol" => "trait",
        "import" | "include" | "require" => "use",
        "module" | "namespace" | "package" => "mod",
        "raise" => "throw",
        "except" => "catch",
        _ => return None,
    })
}

/// Help for `import name` (or `include` / `require`): `mod` for a sibling
/// `name.nabi`; `None` (the `use` hint) for a name the file knows; else
/// `nabi add`.
fn import_help(file: &str, tokens: &[Token], name: &str) -> Option<String> {
    let path = std::path::Path::new(file);
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    if dir.join(format!("{name}.nabi")).is_file() {
        return Some(format!(
            "to load {name}.nabi, write `mod {name};` then call `{name}::f()` or `use {name}::f;`"
        ));
    }
    if let Some(help) = habits::python::python_module(name) {
        return Some(help.to_owned());
    }
    let declared = tokens.windows(2).any(|w| {
        w[0].kind == TokenKind::Mod && matches!(&w[1].kind, TokenKind::Ident(n) if n == name)
    });
    if declared
        || matches!(
            name,
            "std" | "core" | "bare" | "crate" | "self" | "super" | "pkg" | "sup"
        )
    {
        return None;
    }
    // A package's file (under its `src/`) adds to `nabi.nabion`.
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let package = abs
        .ancestors()
        .skip(1)
        .find(|d| d.join("src/main.nabi").is_file() || d.join("src/lib.nabi").is_file())
        .is_some_and(|d| abs.starts_with(d.join("src")));
    let add = if package {
        format!("nabi add {name}")
    } else {
        format!("nabi add {name} {file}")
    };
    Some(format!(
        "to use library `{name}`, run `{add}`, then write `use {name}::f;`"
    ))
}

/// The keyword `word` most likely meant: a known alias, else the
/// closest reserved or contextual keyword.
pub(crate) fn keyword_hint(word: &str) -> Option<&'static str> {
    const CONTEXTUAL: &[&str] = &[
        "type", "any", "in", "where", "throws", "rethrows", "catch", "finally", "const", "static",
        "extern", "mut", "union", "some",
    ];
    keyword_alias(word).or_else(|| {
        nabic_diagnostic::suggest(
            word,
            nabic_lexer::token::RESERVED
                .iter()
                .chain(CONTEXTUAL)
                .copied(),
        )
    })
}

/// The marker statement's fn of a nested fn `f` lifted as `lifted`.
fn nested_marker(f: &FnDef, lifted: &str, inherited: bool) -> FnDef {
    let mut args = vec![nabic_ast::AttrArg::Lit(Literal::Str(lifted.to_owned()))];
    if inherited {
        args.push(nabic_ast::AttrArg::Lit(Literal::Bool(true)));
    }
    FnDef {
        vis: Visibility::Private,
        attrs: vec![Attribute {
            path: vec![nabic_ast::NESTED_FN_ATTR.to_owned()],
            body: nabic_ast::AttrBody::Args(args),
            inner: false,
            span: f.span,
        }],
        name: f.name.clone(),
        generics: Generics::default(),
        params: Vec::new(),
        ret_ty: None,
        body: None,
        is_async: false,
        is_generator: false,
        is_unsafe: false,
        throws: f.throws.clone(),
        where_clause: Vec::new(),
        span: f.span,
    }
}

/// The `CFG_OFF_ATTR` marker for method `f`, which its `#[cfg]` dropped.
fn cfg_off_marker(f: &nabic_ast::FnDef) -> Attribute {
    let mut args = vec![nabic_ast::AttrArg::Word(f.name.clone())];
    args.extend(
        f.attrs
            .iter()
            .filter(|a| a.path == ["cfg"])
            .filter_map(|a| match &a.body {
                nabic_ast::AttrBody::Args(xs) if xs.len() == 1 => Some(xs[0].clone()),
                _ => None,
            }),
    );
    Attribute {
        path: vec![nabic_ast::CFG_OFF_ATTR.to_owned(), "method".to_owned()],
        body: nabic_ast::AttrBody::Args(args),
        inner: false,
        span: f.span,
    }
}
