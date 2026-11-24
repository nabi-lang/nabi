//! Lexer driver. Lines are 1-based and end at LF, CRLF or a lone CR;
//! columns count code points.

use crate::escape::{EscapeMode, unescape};
use crate::token::{DocLine, Token, TokenKind, keyword_kind};
use crate::unicode::{
    code_point, describe, is_assigned, is_bidi_control, is_emoji_component, is_ident_continue,
    is_ident_start, is_invisible, is_mark, is_other_space, is_pictographic, latin_twin, lookalike,
    mixed_script, normalize_ident, skeleton,
};
use nabic_diagnostic::{Code, NabiDiagnostic, Span};

/// `i128` / `u128` lex as suffixes so HIR can say what to write instead.
pub const INT_SUFFIXES: &[&str] = &[
    "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize",
    "c_char",
];
pub const FLOAT_SUFFIXES: &[&str] = &["f32", "f64", "f16", "bf16"];

pub struct Lexer<'src> {
    src: &'src str,
    bytes: &'src [u8],
    file: String,
    pos: usize,
    line: u32,
    col: u32,
    diagnostics: Vec<NabiDiagnostic>,
    /// The previous token can end an expression (`x`, `1`, `)`, ...).
    /// Also marks a following `-` as binary (see `prev_minus`).
    prev_ends_expr: bool,
    /// Previous token was a unary `-`, so `-128i8` may use magnitude
    /// `MAX + 1`.
    prev_minus: bool,
    /// First occurrence of each distinct (normalized) identifier, for
    /// the confusable check; `any_non_ascii` gates it.
    idents: Vec<(String, Span)>,
    seen: nabic_core::HashSet<String>,
    any_non_ascii: bool,
    /// Registered extension operator tokens, longest first.
    ext_ops: Vec<String>,
    /// Doc comments seen since the last token.
    docs: Vec<DocLine>,
    /// Open f-strings, innermost last.
    fstr: Vec<Frame>,
    /// Start of the outermost f-string a hole error (backslash,
    /// newline) was reported in: one per literal.
    hole_reported: Option<usize>,
    /// Bracket depth inside a Rust `macro_rules! m { .. }` body, whose
    /// `$` the parser reports with the macro.
    rules_depth: Option<u32>,
    /// Byte offset of each line start, built on the first `line_col`.
    line_starts: Vec<usize>,
    /// Invisible or unassigned characters anywhere in the source, by
    /// offset; those inside strings and comments are warned about at
    /// the end (`check_hidden`), code reports its own.
    hidden: Vec<(usize, char)>,
    /// String bodies and comments, by start, while `hidden` is not
    /// empty: what each hidden character sits in.
    quoted: Vec<(usize, usize, Quoted)>,
    /// End of the previous token when it was a number literal (`5` then
    /// a degree sign).
    number_end: Option<usize>,
}

/// What `Lexer::stray_char` made of a character.
enum Stray {
    /// Not one it names: plain garbage for `skip_invalid`.
    No,
    /// Reported and skipped.
    Skipped,
    /// Reported, and lexed as the token it stands for.
    Token(TokenKind),
}

/// What a range of `Lexer::quoted` is.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Quoted {
    /// A string or char body that takes `\u{..}` escapes.
    Escaped,
    /// A raw string or a byte string: no `\u{..}`.
    Raw,
    Comment,
}

/// One level of f-string nesting. `triple`: of the f-string it is in.
#[derive(Clone, Copy)]
enum Frame {
    /// Literal text of the f-string opened at `start`.
    Text { triple: bool, start: usize },
    /// Hole code from byte `start`; `depth` counts open brackets.
    Hole {
        triple: bool,
        depth: u32,
        start: usize,
    },
    /// A hole's format spec after `:`.
    Spec { triple: bool },
}

impl<'src> Lexer<'src> {
    #[must_use]
    pub fn new(src: &'src str, file: impl Into<String>) -> Self {
        Self {
            src,
            bytes: src.as_bytes(),
            file: file.into(),
            pos: 0,
            line: 1,
            col: 1,
            diagnostics: Vec::new(),
            prev_ends_expr: false,
            rules_depth: None,
            prev_minus: false,
            idents: Vec::new(),
            seen: nabic_core::HashSet::default(),
            any_non_ascii: false,
            ext_ops: Vec::new(),
            docs: Vec::new(),
            fstr: Vec::new(),
            hole_reported: None,
            line_starts: Vec::new(),
            hidden: Vec::new(),
            quoted: Vec::new(),
            number_end: None,
        }
    }

    /// Also lex these registered extension operator tokens
    /// (gathered by the driver's `#[infix]` pre-pass), longest first.
    #[must_use]
    pub fn with_ext_ops(mut self, ops: Vec<String>) -> Self {
        self.ext_ops = ops;
        self
    }

    pub fn lex_all(mut self) -> (Vec<Token>, Vec<NabiDiagnostic>) {
        // A leading BOM is ignored.
        if self.src.starts_with('\u{FEFF}') {
            self.pos = '\u{FEFF}'.len_utf8();
        }
        self.advance(nabic_core::shebang_len(&self.src[self.pos..]));
        self.scan_chars();
        let mut tokens = Vec::new();
        loop {
            let from = self.pos;
            let flat = self.in_flat_hole();
            let piece = matches!(
                self.fstr.last(),
                Some(Frame::Text { .. } | Frame::Spec { .. })
            );
            if !piece {
                self.skip_trivia();
            }
            if self.pos >= self.bytes.len() {
                self.close_fstrings(&mut tokens);
                let mut eof = Token::new(TokenKind::Eof, self.span_here());
                eof.docs = std::mem::take(&mut self.docs);
                tokens.push(eof);
                break;
            }
            // `None`: invalid input was reported and skipped.
            let tok = if piece {
                self.fstr_piece()
            } else {
                self.next_token()
            };
            if let Some(nl) = self.src[from..self.pos].find(['\n', '\r']).filter(|_| flat) {
                self.hole_error(
                    from + nl,
                    "newline in f-string `{...}` code; hole code spans lines only in \
                     `f\"\"\"...\"\"\"`",
                );
            }
            if let Some(mut tok) = tok {
                self.number_end = matches!(tok.kind, TokenKind::IntLit(_) | TokenKind::FloatLit(_))
                    .then_some(tok.span.end as usize);
                tok.docs = std::mem::take(&mut self.docs);
                self.prev_minus = tok.kind == TokenKind::Minus && !self.prev_ends_expr;
                self.prev_ends_expr = matches!(
                    tok.kind,
                    TokenKind::Ident(_)
                        | TokenKind::IntLit(_)
                        | TokenKind::FloatLit(_)
                        | TokenKind::StrLit(_)
                        | TokenKind::RawStrLit(_)
                        | TokenKind::ByteStrLit(_)
                        | TokenKind::FStrEnd(_)
                        | TokenKind::CharLit(_)
                        | TokenKind::CCharLit(_)
                        | TokenKind::ByteCharLit(_)
                        | TokenKind::RParen
                        | TokenKind::RBracket
                        | TokenKind::RBrace
                        | TokenKind::SelfLower
                        | TokenKind::SelfUpper
                        | TokenKind::True
                        | TokenKind::False
                        | TokenKind::None
                        | TokenKind::Question
                );
                self.track_rules(&tokens, &tok.kind);
                tokens.push(tok);
            }
        }
        self.check_confusables();
        self.check_hidden();
        (tokens, self.diagnostics)
    }

    /// One pass over the text: bidirectional formatting controls are
    /// rejected anywhere, comments and strings included (`\u{..}`
    /// escapes only); invisible and unassigned characters are noted for
    /// `check_hidden`.
    fn scan_chars(&mut self) {
        if self.src.is_ascii() {
            return;
        }
        let mut bidi = Vec::new();
        for (at, c) in self.src.char_indices() {
            if c.is_ascii() {
                continue;
            }
            if is_bidi_control(c) {
                bidi.push((at, c));
            } else if (is_invisible(c) && !(at == 0 && c == '\u{FEFF}')) || !is_assigned(c) {
                self.hidden.push((at, c));
            }
        }
        for (at, c) in bidi {
            let span = self.span_at(at, at + c.len_utf8());
            let d = NabiDiagnostic::error(
                Code::BIDI_CHARACTER,
                span,
                format!(
                    "bidirectional control character {} in source: it can make code read \
                     differently from what the compiler sees",
                    code_point(c)
                ),
            )
            .with_file(self.file.clone())
            .with_help(format!(
                "inside a string write it as `\\u{{{:X}}}`; elsewhere delete it",
                c as u32
            ))
            .with_fix(span, "", "delete the bidirectional control character");
            self.diagnostics.push(d);
        }
    }

    /// Warn about the invisible and unassigned characters of strings
    /// and comments (`hidden_chars`); code reported its own.
    fn check_hidden(&mut self) {
        let hidden = std::mem::take(&mut self.hidden);
        let quoted = std::mem::take(&mut self.quoted);
        for (at, c) in hidden {
            let i = quoted.partition_point(|q| q.0 <= at);
            let Some(&(_, end, kind)) = i.checked_sub(1).map(|i| &quoted[i]) else {
                continue;
            };
            if at >= end {
                continue;
            }
            if expected_joiner(self.src[..at].chars().next_back(), c) {
                continue;
            }
            let span = self.span_at(at, at + c.len_utf8());
            let what = if is_assigned(c) {
                format!("invisible character {}", code_point(c))
            } else {
                format!(
                    "U+{:04X}, a code point with no character in Unicode {},",
                    c as u32,
                    crate::unicode::UNICODE_VERSION
                )
            };
            let escape = format!("\\u{{{:X}}}", c as u32);
            let mut d = match kind {
                Quoted::Comment => {
                    NabiDiagnostic::warning(Code::LINT, span, format!("{what} in a comment"))
                        .with_help("delete it")
                        .with_fix(span, "", "delete the character")
                }
                Quoted::Escaped => NabiDiagnostic::warning(
                    Code::LINT,
                    span,
                    format!("{what} written directly in a literal"),
                )
                .with_help(format!("write it as `{escape}` so readers see it"))
                .with_fix(span, escape.clone(), format!("write `{escape}`")),
                Quoted::Raw => NabiDiagnostic::warning(
                    Code::LINT,
                    span,
                    format!("{what} written directly in a literal"),
                )
                .with_help(format!("use a plain string with `{escape}`, or delete it")),
            };
            d = d.with_file(self.file.clone()).with_lint("hidden_chars");
            self.diagnostics.push(d);
        }
    }

    /// Report a character in code that starts no token and is not plain
    /// garbage (`skip_invalid` takes the rest): invisible characters,
    /// other spaces, lone marks, look-alikes, unregistered operator
    /// symbols, unassigned code points.
    fn stray_char(&mut self, c: char) -> Stray {
        let start = self.pos;
        let glued_number = self.number_end == Some(start);
        if c.is_ascii() && !glued_number {
            return Stray::No;
        }
        let run = |s: &Self, f: fn(char) -> bool| {
            s.src[start..]
                .char_indices()
                .find(|&(_, c)| !f(c))
                .map_or(s.src.len(), |(i, _)| start + i)
        };
        if is_invisible(c) {
            let end = run(self, is_invisible);
            let span = self.span_at(start, end);
            let msg = if matches!(c, '\u{FE00}'..='\u{FE0F}' | '\u{E0100}'..='\u{E01EF}') {
                format!(
                    "{} cannot stand alone: a variation selector picks how the character \
                     before it is drawn, and here it follows none",
                    code_point(c)
                )
            } else if c == '\u{FEFF}' {
                format!(
                    "{} past the start of the file: it is invisible and is not a space",
                    code_point(c)
                )
            } else {
                format!("invisible character {} in code", code_point(c))
            };
            self.push_fix(Code::INVISIBLE_CHARACTER, span, msg, "delete it", "");
            self.advance(end - start);
            return Stray::Skipped;
        }
        if is_other_space(c) {
            let end = run(self, is_other_space);
            let span = self.span_at(start, end);
            let spaces = " ".repeat(self.src[start..end].chars().count());
            self.push_fix(
                Code::INVISIBLE_CHARACTER,
                span,
                format!("{} is not a space in code", code_point(c)),
                "write a plain space",
                &spaces,
            );
            self.advance(end - start);
            return Stray::Skipped;
        }
        let end = start + c.len_utf8();
        let span = self.span_at(start, end);
        if is_mark(c) {
            let d = NabiDiagnostic::error(
                Code::UNKNOWN_CHARACTER,
                span,
                format!(
                    "{} cannot stand alone: a combining mark changes the letter before it, \
                     and here it follows none",
                    describe(c)
                ),
            )
            .with_file(self.file.clone())
            .with_help(if c == '\u{20E3}' {
                "a keycap emoji is a digit, U+FE0F and this mark, so it can't be in a name; \
                 write it inside a string"
                    .to_owned()
            } else {
                format!(
                    "write it right after a letter (`e\\u{{301}}` is `\u{E9}`), or as \
                     `\\u{{{:X}}}` inside a string",
                    c as u32
                )
            })
            .with_fix(span, "", "delete the mark");
            self.diagnostics.push(d);
            self.advance(end - start);
            return Stray::Skipped;
        }
        if glued_number && !is_ident_start(c) {
            let name = match c {
                '°' => "deg",
                '%' => "pct",
                '$' => "usd",
                '€' => "eur",
                '£' => "gbp",
                '¥' => "jpy",
                '‰' => "permille",
                _ => "name",
            };
            // Past the character before the number, which may be wide.
            let num = self.src[..start]
                .char_indices()
                .rfind(|&(_, c)| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '.')))
                .map_or(0, |(i, c)| i + c.len_utf8());
            let num = &self.src[num..start];
            let shown = if c.is_ascii() {
                format!("`{c}`")
            } else {
                describe(c)
            };
            let d = NabiDiagnostic::error(
                Code::INVALID_LITERAL_HANDLER,
                span,
                format!(
                    "`{num}{c}`: a number suffix is a name glued to the number, and {shown} \
                     is not a name character"
                ),
            )
            .with_file(self.file.clone())
            .with_help(format!(
                "write `{num}{name}` with a handler `#[literal(number \"{name}\")]` in scope"
            ));
            self.diagnostics.push(d);
            self.advance(end - start);
            return Stray::Skipped;
        }
        if let Some(tok) = lookalike(c).filter(|t| self.lexes(t)) {
            let msg = format!(
                "{} looks like `{tok}` but is another character",
                describe(c)
            );
            if tok == "\"" {
                return self
                    .curly_string(c, msg)
                    .map_or(Stray::Skipped, Stray::Token);
            }
            self.push_fix(
                Code::LOOKALIKE_CHARACTER,
                span,
                msg,
                &format!("write `{tok}`"),
                tok,
            );
            self.advance(end - start);
            let (toks, diags) = Lexer::new(tok, String::new()).lex_all();
            return match &toks[..] {
                [t, _] if diags.is_empty() => Stray::Token(t.kind.clone()),
                _ => Stray::Skipped,
            };
        }
        if !is_assigned(c) {
            self.error(
                Code::UNKNOWN_CHARACTER,
                span,
                format!(
                    "U+{:04X} is not a character: Unicode {} assigns nothing there",
                    c as u32,
                    crate::unicode::UNICODE_VERSION
                ),
            );
            self.advance(end - start);
            return Stray::Skipped;
        }
        if is_operator_symbol(c) {
            let d = NabiDiagnostic::error(
                Code::UNKNOWN_CHARACTER,
                span,
                format!(
                    "{} is not an operator here: no trait in scope registers it",
                    describe(c)
                ),
            )
            .with_file(self.file.clone())
            .with_help(format!(
                "add the package that registers it (`nabi add <name>`), or register it on a \
                 trait method: `#[infix(\"{c}\", like = \"*\")]`"
            ));
            self.diagnostics.push(d);
            self.advance(end - start);
            // Between operands it reads as a binary operator, so the
            // rest of the line parses.
            return if self.prev_ends_expr {
                Stray::Token(TokenKind::Star)
            } else {
                Stray::Skipped
            };
        }
        Stray::No
    }

    /// Text in curly quotes: one error with a fix for both quotes, and the string
    /// it was meant to be. A quote with no closing one on its line is
    /// reported alone.
    fn curly_string(&mut self, open: char, msg: String) -> Option<TokenKind> {
        let start = self.pos;
        let body = start + open.len_utf8();
        let line_end = self.src[body..]
            .find(['\n', '\r'])
            .map_or(self.src.len(), |i| body + i);
        let close = self.src[body..line_end]
            .char_indices()
            .find(|&(_, c)| c == '"' || lookalike(c) == Some("\""));
        let Some((i, q)) = close else {
            let span = self.span_at(start, body);
            self.push_fix(Code::LOOKALIKE_CHARACTER, span, msg, "write `\"`", "\"");
            self.advance(body - start);
            return None;
        };
        let end = body + i + q.len_utf8();
        let inner = &self.src[body..body + i];
        let span = self.span_at(start, end);
        let fixed = format!("\"{inner}\"");
        let d = NabiDiagnostic::error(Code::LOOKALIKE_CHARACTER, span, msg)
            .with_file(self.file.clone())
            .with_help(format!(
                "write `{fixed}`: a string is quoted with ASCII `\"`"
            ))
            .with_fix(span, fixed.clone(), format!("write `{fixed}`"));
        self.diagnostics.push(d);
        let inner = inner.to_owned();
        self.advance(end - start);
        Some(TokenKind::StrLit(inner))
    }

    /// `tok` lexes as a token here (a look-alike's replacement).
    fn lexes(&self, tok: &str) -> bool {
        tok != "\\" && tok != "$" || self.ext_ops.iter().any(|o| o == tok)
    }

    fn push_fix(&mut self, code: Code, span: Span, msg: String, help: &str, text: &str) {
        let title = if text.is_empty() {
            "delete it".to_owned()
        } else {
            help.to_owned()
        };
        let d = NabiDiagnostic::error(code, span, msg)
            .with_file(self.file.clone())
            .with_help(help)
            .with_fix(span, text, title);
        self.diagnostics.push(d);
    }

    /// Invisible characters inside the identifier `raw` at `start`: an
    /// error with the name they hide, and a fix that drops them.
    fn check_ident_chars(&mut self, raw: &str, start: usize) -> Option<String> {
        let mut bad = Vec::new();
        let mut prev = None;
        for (i, c) in raw.char_indices() {
            // U+FE0E / U+FE0F are dropped by normalization: same name.
            if is_invisible(c) && !matches!(c, '\u{FE0E}' | '\u{FE0F}') && !expected_joiner(prev, c)
            {
                bad.push((i, c));
            }
            prev = Some(c);
        }
        let &(_, first) = bad.first()?;
        let span = self.span_at(start, start + raw.len());
        let clean: String = raw
            .char_indices()
            .filter(|(i, _)| !bad.iter().any(|(b, _)| b == i))
            .map(|(_, c)| c)
            .collect();
        let shown: String = raw
            .chars()
            .map(|c| {
                if is_invisible(c) {
                    format!("\\u{{{:X}}}", c as u32)
                } else {
                    c.to_string()
                }
            })
            .collect();
        let msg = if clean.is_empty() {
            format!(
                "this name is only the invisible character {}, so it shows nothing",
                code_point(first)
            )
        } else {
            format!(
                "identifier `{clean}` holds the invisible character {}: it reads as `{clean}` \
                 but is another name",
                code_point(first)
            )
        };
        let mut d = NabiDiagnostic::error(Code::INVISIBLE_CHARACTER, span, msg)
            .with_file(self.file.clone())
            .with_note(format!("as written: `{shown}`"));
        d = if clean.is_empty() || !clean.starts_with(is_ident_start) {
            d.with_help("write a visible name")
        } else {
            d.with_help(format!("write `{clean}`")).with_fix(
                span,
                clean.clone(),
                format!("write `{clean}`"),
            )
        };
        self.diagnostics.push(d);
        Some(clean).filter(|c| !c.is_empty())
    }

    /// Warn about distinct identifiers that share a
    /// confusable skeleton (`σ²` / `σ2`, Latin `a` / Cyrillic `а`).
    fn check_confusables(&mut self) {
        if !self.any_non_ascii {
            return;
        }
        let mut by_skel: nabic_core::HashMap<String, &(String, Span)> =
            nabic_core::HashMap::default();
        let mut warns = Vec::new();
        for id in &self.idents {
            match by_skel.entry(skeleton(&id.0)) {
                std::collections::hash_map::Entry::Occupied(o) => {
                    warns.push((
                        id.1,
                        format!(
                            "identifier `{}` looks like `{}` (confusable spelling)",
                            id.0,
                            o.get().0
                        ),
                    ));
                }
                std::collections::hash_map::Entry::Vacant(v) => {
                    v.insert(id);
                }
            }
        }
        for (span, msg) in warns {
            self.warning(span, msg, "confusable_idents");
        }
    }

    fn next_token(&mut self) -> Option<Token> {
        let start = self.pos;
        let line = self.line;
        let col = self.col;

        let b = self.bytes[start];
        let next = self.bytes.get(start + 1).copied();
        if let Some(kind) = self.hole_punct(b, next) {
            return Some(Token::new(
                kind,
                Self::make_span(start, self.pos, line, col),
            ));
        }
        if b == b'\\' && matches!(self.fstr.last(), Some(Frame::Hole { .. })) {
            self.advance(if next == Some(b'"') { 2 } else { 1 });
            self.hole_error(
                start,
                "backslash in f-string `{...}` code; a hole is plain code, so write `\"` \
                 directly: `{x ?? \"a\"}`, `{d[\"k\"]}`",
            );
            return None;
        }
        // A registered token longer than the fixed one here wins
        // (`<-` over `<`); the fixed arm is undone.
        let ext = if fixed_lead(b) && !self.ext_ops.is_empty() {
            self.peek_ext_op()
        } else {
            None
        };
        let errors = self.diagnostics.len();
        let kind = match b {
            b'#' if next == Some(b'[') => {
                self.advance(2);
                TokenKind::HashLBracket
            }
            b'#' if next == Some(b'!') && self.bytes.get(start + 2) == Some(&b'[') => {
                self.advance(3);
                TokenKind::HashBangLBracket
            }
            b'(' => self.single(TokenKind::LParen),
            b')' => self.single(TokenKind::RParen),
            b'[' => self.single(TokenKind::LBracket),
            b']' => self.single(TokenKind::RBracket),
            b'{' => self.single(TokenKind::LBrace),
            b'}' => self.single(TokenKind::RBrace),
            b',' => self.single(TokenKind::Comma),
            b';' => self.single(TokenKind::Semi),
            b'@' => self.single(TokenKind::At),
            b'~' => self.single(TokenKind::Tilde),
            b'+' => self.lex_op2(b'=', TokenKind::PlusEq, TokenKind::Plus),
            b'-' => self.lex_minus(),
            b'*' => self.lex_star(),
            b'/' => self.lex_op2(b'=', TokenKind::SlashEq, TokenKind::Slash),
            b'%' => self.lex_percent(),
            b'^' => self.lex_op2(b'=', TokenKind::CaretEq, TokenKind::Caret),
            b'=' => self.lex_eq(),
            b'!' => self.lex_op2(b'=', TokenKind::NotEq, TokenKind::Bang),
            b'<' => self.lex_lt(),
            b'>' => self.lex_gt(),
            b'&' => self.lex_amp(),
            b'|' => self.lex_pipe(),
            b'.' => self.lex_dot(),
            b':' => self.lex_colon(),
            b'?' => self.lex_question(),
            b'\'' => self.lex_quote_or_label(),
            b'"' => self.lex_str_lit(),
            b'r' if next == Some(b'#') && self.at_raw_ident() => return Some(self.lex_raw_ident()),
            b'r' if next == Some(b'"') || next == Some(b'#') => self.lex_raw_str_lit(),
            b'f' if next == Some(b'"') => {
                let triple = self.src[start + 1..].starts_with("\"\"\"");
                self.advance(if triple { 4 } else { 2 });
                self.fstr.push(Frame::Text { triple, start });
                TokenKind::FStrStart(triple)
            }
            b'c' if next == Some(b'\'') => self.lex_char_lit(Some('c')),
            b'b' if next == Some(b'\'') => self.lex_char_lit(Some('b')),
            b'b' if next == Some(b'"') => self.lex_byte_str_lit(),
            d if d.is_ascii_digit() => self.lex_number(),
            _ if !self.ext_ops.is_empty() && self.peek_ext_op().is_some() => {
                let op = self.peek_ext_op().unwrap_or_default();
                self.advance(op.len());
                TokenKind::ExtOp(op)
            }
            _ => match self.peek_char() {
                Some(c) if is_ident_start(c) || self.at_flag() => self.lex_ident(),
                Some(c) if math_op(c).is_some() => {
                    self.advance(c.len_utf8());
                    math_op(c)?
                }
                Some('$') if self.rules_depth.is_some() => {
                    self.advance(1);
                    return None;
                }
                Some(c) => match self.stray_char(c) {
                    Stray::Token(kind) => kind,
                    Stray::Skipped => return None,
                    Stray::No => {
                        self.skip_invalid();
                        return None;
                    }
                },
                None => {
                    self.skip_invalid();
                    return None;
                }
            },
        };

        let kind = match ext {
            Some(op) if op.len() > self.pos - start => {
                (self.pos, self.line, self.col) = (start, line, col);
                self.diagnostics.truncate(errors);
                self.advance(op.len());
                TokenKind::ExtOp(op)
            }
            _ => kind,
        };
        let span = Self::make_span(start, self.pos, line, col);
        Some(Token::new(kind, span))
    }

    /// Follow the brackets of a `macro_rules! m { .. }` body; `kind` comes
    /// after `tokens`.
    fn track_rules(&mut self, tokens: &[Token], kind: &TokenKind) {
        let open = matches!(
            kind,
            TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace
        );
        let close = matches!(
            kind,
            TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace
        );
        self.rules_depth = match self.rules_depth {
            Some(d) if open => Some(d + 1),
            Some(d) if close => d.checked_sub(1).filter(|&d| d > 0),
            None if open => match tokens {
                [.., a, b, c]
                    if matches!(&a.kind, TokenKind::Ident(n) if n == "macro_rules")
                        && b.kind == TokenKind::Bang
                        && matches!(c.kind, TokenKind::Ident(_)) =>
                {
                    Some(1)
                }
                _ => None,
            },
            d => d,
        };
    }

    /// Report and skip a run of characters that cannot start a token
    /// (one diagnostic per run, never recursion).
    fn skip_invalid(&mut self) {
        let start = self.pos;
        let span = self.span_here();
        while let Some(c) = self.peek_char() {
            let ok_start = c.is_ascii() && b"()[]{},;@~+-*/%^=!<>&|.:?'\"".contains(&(c as u8));
            if self.pos > start
                && (ok_start
                    || is_ident_start(c)
                    || math_op(c).is_some()
                    || is_whitespace(c)
                    || c.is_ascii_digit()
                    || !c.is_ascii())
            {
                break;
            }
            if self.pos > start && c == '#' {
                break;
            }
            self.advance(c.len_utf8());
        }
        let text: String = self.src[start..self.pos].chars().take(16).collect();
        // `#!` past the first line: a misplaced shebang, so the rest of
        // the line is not code.
        if text == "#" && self.peek_char() == Some('!') {
            while let Some(c) = self.peek_char().filter(|&c| c != '\n') {
                self.advance(c.len_utf8());
            }
        }
        let msg = if text == "#" {
            "stray `#` (expected `#[` or `#![`)".to_owned()
        } else {
            format!("unexpected character(s) `{}`", text.escape_debug())
        };
        self.error(
            Code::UNKNOWN_CHARACTER,
            Span::new(span.start, self.pos as u32, span.line, span.col),
            msg,
        );
    }

    /// Remember a string body or comment while hidden characters wait
    /// for `check_hidden`.
    fn note_quoted(&mut self, start: usize, end: usize, kind: Quoted) {
        if !self.hidden.is_empty() && start < end {
            self.quoted.push((start, end, kind));
        }
    }

    /// An escape error at body offset `at` of the body starting at
    /// `body`: spans just the escape, with what to write instead.
    fn escape_error(&mut self, body: usize, at: usize, msg: String) {
        let from = body + at;
        let rest = &self.src[from..self.pos];
        let mut it = rest.char_indices().skip(1);
        let len = match it.next() {
            Some((_, 'u')) => rest
                .find('}')
                .filter(|&e| !rest[..e].contains(['"', '\'']))
                .map_or(2, |e| e + 1),
            Some((_, 'x')) => {
                2 + rest[2..]
                    .chars()
                    .take(2)
                    .take_while(char::is_ascii_hexdigit)
                    .count()
            }
            Some((i, c)) => i + c.len_utf8(),
            None => 1,
        };
        let esc = &rest[..len.min(rest.len())];
        let span = self.span_at(from, from + esc.len());
        let mut d =
            NabiDiagnostic::error(Code::INVALID_ESCAPE, span, msg).with_file(self.file.clone());
        let hex = esc.strip_prefix("\\u{").and_then(|h| h.strip_suffix('}'));
        let v = hex.and_then(|h| u32::from_str_radix(&h.replace('_', ""), 16).ok());
        if let Some(v @ 0xD800..=0xDFFF) = v {
            d = d.with_help(format!(
                "U+{v:04X} is a surrogate, half of a UTF-16 pair, not a character; write the \
                 character's own code point (`\\u{{1F600}}`, not `\\u{{D83D}}\\u{{DE00}}`)"
            ));
        } else if v.is_some_and(|v| v > 0x10FFFF) {
            d = d.with_help("code points end at `\\u{10FFFF}`");
        } else if let Some(h) = esc.strip_prefix("\\x").filter(|h| h.len() == 2) {
            d = d
                .with_help(format!(
                    "`\\xHH` is ASCII here; write `\\u{{{h}}}` for the character U+00{h}"
                ))
                .with_fix(span, format!("\\u{{{h}}}"), format!("write `\\u{{{h}}}`"));
        } else if let Some(e) = esc
            .strip_prefix('\\')
            .filter(|e| e.len() == 1 && !matches!(*e, "x" | "u"))
        {
            let known = "the escapes are `\\n` `\\r` `\\t` `\\0` `\\\\` `\\'` `\\\"` `\\xHH` \
                         `\\u{..}`";
            let backslash = format!("\\{esc}");
            d = if e.bytes().all(|b| b.is_ascii_punctuation()) {
                // `\%`: the character itself was meant.
                d.with_help(format!(
                    "{known}; `{e}` needs no escape, so write `{e}` (or `{backslash}` for a \
                     backslash and `{e}`)"
                ))
                .with_fix(span, e, format!("write `{e}`"))
            } else {
                d.with_help(format!("{known}; for a backslash write `{backslash}`"))
                    .with_fix(span, backslash.clone(), format!("write `{backslash}`"))
            };
        }
        self.diagnostics.push(d);
    }

    fn single(&mut self, kind: TokenKind) -> TokenKind {
        self.advance(1);
        kind
    }

    fn lex_minus(&mut self) -> TokenKind {
        match self.bytes.get(self.pos + 1) {
            Some(b'>') => {
                self.advance(2);
                TokenKind::Arrow
            }
            Some(b'=') => {
                self.advance(2);
                TokenKind::MinusEq
            }
            _ => self.single(TokenKind::Minus),
        }
    }

    fn lex_star(&mut self) -> TokenKind {
        // `**=` > `**` > `*=` > `*` (longest match).
        match (self.bytes.get(self.pos + 1), self.bytes.get(self.pos + 2)) {
            (Some(b'*'), Some(b'=')) => {
                self.advance(3);
                TokenKind::StarStarEq
            }
            (Some(b'*'), _) => {
                self.advance(2);
                TokenKind::StarStar
            }
            (Some(b'='), _) => {
                self.advance(2);
                TokenKind::StarEq
            }
            _ => self.single(TokenKind::Star),
        }
    }

    fn lex_percent(&mut self) -> TokenKind {
        // `%%=` > `%%` > `%=` > `%` (longest match).
        match (self.bytes.get(self.pos + 1), self.bytes.get(self.pos + 2)) {
            (Some(b'%'), Some(b'=')) => {
                self.advance(3);
                TokenKind::PercentPercentEq
            }
            (Some(b'%'), _) => {
                self.advance(2);
                TokenKind::PercentPercent
            }
            (Some(b'='), _) => {
                self.advance(2);
                TokenKind::PercentEq
            }
            _ => self.single(TokenKind::Percent),
        }
    }

    fn lex_eq(&mut self) -> TokenKind {
        match self.bytes.get(self.pos + 1) {
            Some(b'=') => {
                self.advance(2);
                TokenKind::EqEq
            }
            Some(b'>') => {
                self.advance(2);
                TokenKind::FatArrow
            }
            _ => self.single(TokenKind::Eq),
        }
    }

    fn lex_lt(&mut self) -> TokenKind {
        match (self.bytes.get(self.pos + 1), self.bytes.get(self.pos + 2)) {
            (Some(b'='), _) => {
                self.advance(2);
                TokenKind::LtEq
            }
            (Some(b'<'), Some(b'=')) => {
                self.advance(3);
                TokenKind::ShlEq
            }
            (Some(b'<'), _) => {
                self.advance(2);
                TokenKind::Shl
            }
            _ => self.single(TokenKind::Lt),
        }
    }

    fn lex_gt(&mut self) -> TokenKind {
        match (self.bytes.get(self.pos + 1), self.bytes.get(self.pos + 2)) {
            (Some(b'='), _) => {
                self.advance(2);
                TokenKind::GtEq
            }
            (Some(b'>'), Some(b'=')) => {
                self.advance(3);
                TokenKind::ShrEq
            }
            (Some(b'>'), _) => {
                self.advance(2);
                TokenKind::Shr
            }
            _ => self.single(TokenKind::Gt),
        }
    }

    fn lex_amp(&mut self) -> TokenKind {
        match self.bytes.get(self.pos + 1) {
            Some(b'&') => {
                self.advance(2);
                TokenKind::AmpAmp
            }
            Some(b'=') => {
                self.advance(2);
                TokenKind::AmpEq
            }
            _ => self.single(TokenKind::Amp),
        }
    }

    fn lex_pipe(&mut self) -> TokenKind {
        match self.bytes.get(self.pos + 1) {
            Some(b'|') => {
                self.advance(2);
                TokenKind::PipePipe
            }
            Some(b'=') => {
                self.advance(2);
                TokenKind::PipeEq
            }
            Some(b'>') => {
                self.advance(2);
                TokenKind::PipeGt
            }
            _ => self.single(TokenKind::Pipe),
        }
    }

    fn lex_dot(&mut self) -> TokenKind {
        // `..=`, `..`, `.5` float, broadcast operators, plain `.`.
        if self.bytes.get(self.pos + 1) == Some(&b'.') {
            if self.bytes.get(self.pos + 2) == Some(&b'=') {
                self.advance(3);
                return TokenKind::DotDotEq;
            }
            self.advance(2);
            return TokenKind::DotDot;
        }
        // `.5` is always `.` + tuple index; a float literal needs
        // its leading `0`. Where no expression precedes, say so.
        if !self.prev_ends_expr && self.bytes.get(self.pos + 1).is_some_and(u8::is_ascii_digit) {
            self.error_from(
                Code::MALFORMED_NUMBER,
                self.pos,
                "a float literal needs a leading digit (`0.5`); `.5` is a tuple index",
            );
        }
        if let Some(kind) = self.try_broadcast() {
            return kind;
        }
        self.single(TokenKind::Dot)
    }

    fn try_broadcast(&mut self) -> Option<TokenKind> {
        // `.+` `.-` `.*` `./` `.%` `.%%` `.&` `.|` `.^` `.<<` `.>>`
        // `.==` `.!=` `.<` `.>` `.<=` `.>=` `.&&` `.||`
        // `.=` `.+=` `.-=` `.*=` `./=` `.%=` `.**=` `.%%=` `.&=` `.|=` `.^=`
        // `.<<=` `.>>=` `.??`
        let p = self.pos + 1;
        let b1 = *self.bytes.get(p)?;
        let b2 = self.bytes.get(p + 1).copied();
        let b3 = self.bytes.get(p + 2).copied();

        let (len, kind) = match (b1, b2, b3) {
            (b'*', Some(b'*'), Some(b'=')) => (4, TokenKind::DotStarStarEq),
            (b'%', Some(b'%'), Some(b'=')) => (4, TokenKind::DotPercentPercentEq),
            (b'<', Some(b'<'), Some(b'=')) => (4, TokenKind::DotShlEq),
            (b'>', Some(b'>'), Some(b'=')) => (4, TokenKind::DotShrEq),
            (b'&', Some(b'='), _) => (3, TokenKind::DotAmpEq),
            (b'|', Some(b'='), _) => (3, TokenKind::DotPipeEq),
            (b'^', Some(b'='), _) => (3, TokenKind::DotCaretEq),
            (b'+', Some(b'='), _) => (3, TokenKind::DotPlusEq),
            (b'-', Some(b'='), _) => (3, TokenKind::DotMinusEq),
            (b'*', Some(b'='), _) => (3, TokenKind::DotStarEq),
            (b'/', Some(b'='), _) => (3, TokenKind::DotSlashEq),
            (b'%', Some(b'='), _) => (3, TokenKind::DotPercentEq),
            (b'?', Some(b'?'), _) => (3, TokenKind::DotQuestionQuestion),
            (b'=', b2, _) if b2 != Some(b'=') => (2, TokenKind::DotEq),
            _ => Self::try_broadcast_op(b1, b2)?,
        };
        self.advance(len);
        Some(kind)
    }

    /// The dot operator `.` `b1` `b2` starts, with its length.
    const fn try_broadcast_op(b1: u8, b2: Option<u8>) -> Option<(usize, TokenKind)> {
        Some(match (b1, b2) {
            (b'<', Some(b'<')) => (3, TokenKind::DotShl),
            (b'>', Some(b'>')) => (3, TokenKind::DotShr),
            (b'<', Some(b'=')) => (3, TokenKind::DotLtEq),
            (b'>', Some(b'=')) => (3, TokenKind::DotGtEq),
            (b'=', Some(b'=')) => (3, TokenKind::DotEqEq),
            (b'!', Some(b'=')) => (3, TokenKind::DotNotEq),
            (b'%', Some(b'%')) => (3, TokenKind::DotPercentPercent),
            (b'&', Some(b'&')) => (3, TokenKind::DotAmpAmp),
            (b'|', Some(b'|')) => (3, TokenKind::DotPipePipe),
            (b'+', _) => (2, TokenKind::DotPlus),
            (b'-', _) => (2, TokenKind::DotMinus),
            (b'*', Some(b'*')) => (3, TokenKind::DotStarStar),
            (b'*', _) => (2, TokenKind::DotStar),
            (b'/', _) => (2, TokenKind::DotSlash),
            (b'%', _) => (2, TokenKind::DotPercent),
            (b'&', _) => (2, TokenKind::DotAmp),
            (b'|', _) => (2, TokenKind::DotPipe),
            (b'^', _) => (2, TokenKind::DotCaret),
            (b'<', _) => (2, TokenKind::DotLt),
            (b'>', _) => (2, TokenKind::DotGt),
            _ => return None,
        })
    }

    fn lex_colon(&mut self) -> TokenKind {
        self.lex_op2(b':', TokenKind::ColonColon, TokenKind::Colon)
    }

    fn lex_question(&mut self) -> TokenKind {
        match self.bytes.get(self.pos + 1) {
            Some(b'.') => {
                self.advance(2);
                TokenKind::QuestionDot
            }
            Some(b'?') => {
                self.advance(2);
                TokenKind::QuestionQuestion
            }
            _ => self.single(TokenKind::Question),
        }
    }

    fn lex_op2(&mut self, second: u8, two: TokenKind, one: TokenKind) -> TokenKind {
        if self.bytes.get(self.pos + 1) == Some(&second) {
            self.advance(2);
            two
        } else {
            self.single(one)
        }
    }

    /// The longest registered token at `pos`.
    fn peek_ext_op(&self) -> Option<String> {
        let rest = &self.src[self.pos..];
        self.ext_ops
            .iter()
            .filter(|op| !op.is_empty() && rest.starts_with(op.as_str()))
            .max_by_key(|op| op.len())
            .cloned()
    }

    /// A flag - a pair of regional indicators - may start an identifier
    /// (`🇰🇷`); a lone indicator may not.
    fn at_flag(&self) -> bool {
        let ri = |c: char| ('\u{1F1E6}'..='\u{1F1FF}').contains(&c);
        let mut it = self.src[self.pos..].chars();
        it.next().is_some_and(ri) && it.next().is_some_and(ri)
    }

    /// `r#` then an identifier start (`r#"` is a raw string).
    fn at_raw_ident(&self) -> bool {
        self.src[self.pos + 2..]
            .chars()
            .next()
            .is_some_and(is_ident_start)
    }

    /// `r#name`: the identifier `name`, even when `name` is a
    /// keyword.
    fn lex_raw_ident(&mut self) -> Token {
        let start = self.pos;
        let (line, col) = (self.line, self.col);
        self.advance(2);
        let from = self.pos;
        self.eat_ident_continue();
        let name = normalize_ident(&self.src[from..self.pos]);
        let span = Self::make_span(start, self.pos, line, col);
        if matches!(name.as_str(), "self" | "Self" | "_") {
            self.error(
                Code::RAW_IDENTIFIER,
                span,
                format!(
                    "`r#{name}` is not allowed: `{name}` can't be a raw identifier; write \
                     `{name}` or pick another name"
                ),
            );
        }
        if self.seen.insert(name.clone()) {
            self.any_non_ascii |= !name.is_ascii();
            self.idents.push((name.clone(), span));
        }
        let mut tok = Token::new(TokenKind::Ident(name), span);
        tok.raw = true;
        tok
    }

    fn lex_ident(&mut self) -> TokenKind {
        let start = self.pos;
        let (line, col) = (self.line, self.col);
        self.eat_ident_continue();
        // A zero-width space inside a name: one name, reported below.
        while self.peek_char().is_some_and(is_invisible) {
            let mut rest = self.src[self.pos..].chars();
            let skip = rest.by_ref().take_while(|&c| is_invisible(c)).count();
            let after = self.src[self.pos..].chars().nth(skip);
            if !after.is_some_and(is_ident_continue) {
                break;
            }
            let n: usize = self.src[self.pos..]
                .chars()
                .take(skip)
                .map(char::len_utf8)
                .sum();
            self.advance(n);
            self.eat_ident_continue();
        }
        let raw = &self.src[start..self.pos];
        let clean = if raw.is_ascii() {
            None
        } else {
            self.check_ident_chars(raw, start)
        };
        // Identifiers compare after NFC + variation-selector
        // removal.
        let name = normalize_ident(clean.as_deref().unwrap_or(raw));
        if let Some(k) = keyword_kind(&name) {
            return k;
        }
        let span = Self::make_span(start, self.pos, line, col);
        if self.seen.insert(name.clone()) {
            if !raw.is_ascii() {
                self.any_non_ascii = true;
                if mixed_script(&name) {
                    self.mixed_script_warning(&name, span);
                }
            }
            self.idents.push((name.clone(), span));
        }
        TokenKind::Ident(name)
    }

    /// A name mixing Cyrillic with Latin or Greek: name the look-alike
    /// letters and, when each has a Latin twin, offer the Latin name.
    fn mixed_script_warning(&mut self, name: &str, span: Span) {
        let latin = name.chars().any(|c| c.is_ascii_alphabetic());
        let twins: Vec<(char, Option<char>)> = name
            .chars()
            .filter(|&c| ('\u{400}'..='\u{52F}').contains(&c))
            .map(|c| (c, latin_twin(c)))
            .collect();
        let all = latin && twins.iter().all(|(_, t)| t.is_some());
        let mut msg = format!("identifier `{name}` mixes Cyrillic with Latin or Greek letters");
        if let Some((c, Some(t))) = twins.first().filter(|_| all) {
            msg.push_str(&format!(": {} looks like Latin `{t}`", describe(*c)));
        }
        let mut d = NabiDiagnostic::warning(Code::LINT, span, msg)
            .with_file(self.file.clone())
            .with_lint("confusable_idents");
        if all {
            let fixed: String = name
                .chars()
                .map(|c| {
                    twins
                        .iter()
                        .find(|(k, _)| *k == c)
                        .and_then(|(_, t)| *t)
                        .unwrap_or(c)
                })
                .collect();
            d = d
                .with_help(format!("write `{fixed}` in Latin letters"))
                .with_fix(span, fixed.clone(), format!("write `{fixed}`"));
        }
        self.diagnostics.push(d);
    }

    fn eat_ident_continue(&mut self) {
        // Last character that was not an emoji modifier / selector, for
        // the ZWJ rule (a ZWJ only joins two pictographs).
        let mut last_base: Option<char> = None;
        while let Some(c) = self.peek_char() {
            if c == '\u{200D}' {
                let next = self.src[self.pos + c.len_utf8()..].chars().next();
                if !(last_base.is_some_and(is_pictographic) && next.is_some_and(is_pictographic)) {
                    break;
                }
            } else if !is_ident_continue(c) {
                break;
            }
            if !crate::unicode::is_emoji_component(c) {
                last_base = Some(c);
            }
            self.advance(c.len_utf8());
        }
    }

    /// Numeric literal. Malformed input is reported but
    /// still yields a token so the parser does not cascade.
    fn lex_number(&mut self) -> TokenKind {
        let start = self.pos;
        let mut radix = 10;
        let mut is_float = false;

        let prefix = match (self.bytes[start], self.bytes.get(start + 1)) {
            (b'0', Some(b'x' | b'X')) => Some(16),
            (b'0', Some(b'b' | b'B')) => Some(2),
            (b'0', Some(b'o' | b'O')) => Some(8),
            _ => None,
        };
        if let Some(r) = prefix {
            radix = r;
            self.advance(2);
            self.eat_digit_seq(radix, start);
            let stray = self.bytes.get(self.pos).copied().filter(u8::is_ascii_digit);
            if let Some(d) = stray.filter(|_| radix != 16) {
                let from = self.pos;
                while self
                    .bytes
                    .get(self.pos)
                    .is_some_and(|b| b.is_ascii_digit() || *b == b'_')
                {
                    self.advance(1);
                }
                let span = self.span_at(from, from + 1);
                let top = (b'0' + u8::try_from(radix - 1).unwrap_or(0)) as char;
                let diag = NabiDiagnostic::error(
                    Code::MALFORMED_NUMBER,
                    span,
                    format!("invalid digit `{}` in base-{radix} literal", d as char),
                )
                .with_file(self.file.clone())
                .with_help(format!(
                    "a base-{radix} literal takes the digits `0` to `{top}`; for decimal drop \
                     the `{}` prefix",
                    &self.src[start..start + 2]
                ));
                self.diagnostics.push(diag);
            }
        } else {
            self.eat_digit_seq(10, start);
            if self.bytes.get(self.pos) == Some(&b'.') {
                let after = self.src[self.pos + 1..].chars().next();
                if after.is_some_and(|c| c.is_ascii_digit()) {
                    is_float = true;
                    self.advance(1);
                    self.eat_digit_seq(10, start);
                } else if !after.is_some_and(|c| c == '.' || is_ident_start(c)) {
                    // `1.` - `.` not followed by an identifier or `.`.
                    is_float = true;
                    self.advance(1);
                }
            }
        }

        if radix == 10 && matches!(self.bytes.get(self.pos), Some(b'e' | b'E')) {
            is_float = true;
            let e_at = self.pos;
            self.advance(1);
            if matches!(self.bytes.get(self.pos), Some(b'+' | b'-')) {
                self.advance(1);
            }
            if self.bytes.get(self.pos).is_some_and(u8::is_ascii_digit) {
                self.eat_digit_seq(10, start);
            } else {
                let span = self.span_at(e_at, self.pos);
                let mantissa = &self.src[start..e_at];
                let d =
                    NabiDiagnostic::error(Code::MALFORMED_NUMBER, span, "exponent has no digits")
                        .with_file(self.file.clone())
                        .with_help(format!(
                            "write the power of ten after `e` (`{mantissa}e3`); a number suffix \
                         can't start with `e`"
                        ));
                self.diagnostics.push(d);
            }
        }

        let body_end = self.pos;
        // A mark after the digits joins nothing: leave it to `stray_char`.
        if !self.peek_char().is_some_and(is_mark) {
            self.eat_ident_continue();
        }
        let suffix = &self.src[body_end..self.pos];
        let text = self.src[start..self.pos].to_owned();
        if !suffix.is_empty() {
            if FLOAT_SUFFIXES.contains(&suffix) {
                if radix == 10 {
                    is_float = true;
                } else {
                    self.error_from(
                        Code::MALFORMED_NUMBER,
                        start,
                        format!("float suffix `{suffix}` on a base-{radix} literal"),
                    );
                }
            } else if INT_SUFFIXES.contains(&suffix) && is_float {
                self.error_from(
                    Code::MALFORMED_NUMBER,
                    start,
                    format!("integer suffix `{suffix}` on a float literal"),
                );
            }
            // Any other suffix (`2i`) belongs to a `#[literal(number "i")]`
            // handler; the parser resolves it.
        }
        self.check_range(start, &self.src[start..body_end], radix, is_float, suffix);
        let int_suffix = suffix.is_empty() || INT_SUFFIXES.contains(&suffix);
        if radix == 10 && !is_float && int_suffix {
            self.check_leading_zero(start, body_end);
        }
        if is_float {
            TokenKind::FloatLit(text)
        } else {
            TokenKind::IntLit(text)
        }
    }

    /// `0755` is an error: offer `0o755` and `755`.
    fn check_leading_zero(&mut self, start: usize, body_end: usize) {
        let digits: String = self.src[start..body_end]
            .chars()
            .filter(|&c| c != '_')
            .collect();
        if digits.len() < 2 || !digits.starts_with('0') {
            return;
        }
        let text = &self.src[start..self.pos];
        let suffix = &self.src[body_end..self.pos];
        let value = match digits.trim_start_matches('0') {
            "" => "0",
            v => v,
        };
        let decimal = format!("{value}{suffix}");
        let (line, col) = self.line_col(start);
        let span = Self::make_span(start, self.pos, line, col);
        let octal = value != "0" && digits.bytes().all(|b| (b'0'..=b'7').contains(&b));
        let help = if octal {
            format!("write `0o{decimal}` for an octal number, or `{decimal}` for decimal")
        } else {
            format!("write `{decimal}`")
        };
        let mut d = NabiDiagnostic::error(
            Code::MALFORMED_NUMBER,
            span,
            format!("`{text}`: a decimal literal can't start with `0`"),
        )
        .with_file(self.file.clone())
        .with_help(help)
        .with_fix(
            span,
            decimal.clone(),
            format!("write `{decimal}` (the same value)"),
        );
        if octal {
            d = d.with_fix(
                span,
                format!("0o{decimal}"),
                format!("write `0o{decimal}` (octal)"),
            );
        }
        self.diagnostics.push(d);
    }

    /// Consume `digit ("_"? digit)*` in `radix`; report empty runs and
    /// misplaced `_`.
    fn eat_digit_seq(&mut self, radix: u32, lit_start: usize) {
        let start = self.pos;
        while self
            .bytes
            .get(self.pos)
            .is_some_and(|&b| b == b'_' || (b as char).is_digit(radix))
        {
            self.advance(1);
        }
        let run = &self.src[start..self.pos];
        let unders = |at: usize| at + run[at..].len() - run[at..].trim_start_matches('_').len();
        let (msg, from, to) = if run.is_empty() {
            // `0b2`: the caller names the stray digit instead.
            if self.bytes.get(self.pos).is_some_and(u8::is_ascii_digit) {
                return;
            }
            let prefix = &self.src[lit_start..start];
            let msg = if prefix.len() == 2 && prefix.starts_with('0') {
                format!("missing digits after `{prefix}` in numeric literal")
            } else {
                "missing digits in numeric literal".to_owned()
            };
            let help = if prefix.len() == 2 && prefix.starts_with('0') {
                format!("write at least one digit after the prefix: `{prefix}0`")
            } else {
                "write a digit here".to_owned()
            };
            let span = self.span_at(lit_start, self.pos);
            let d = NabiDiagnostic::error(Code::MALFORMED_NUMBER, span, msg)
                .with_file(self.file.clone())
                .with_help(help);
            self.diagnostics.push(d);
            return;
        } else if run.starts_with('_') {
            let msg = "`_` must sit between digits (not after a prefix or `.`)";
            (msg.to_owned(), start, start + unders(0))
        } else if run.ends_with('_') {
            let at = start + run.trim_end_matches('_').len();
            // `1_u8`: the suffix follows the digits directly.
            let rest = &self.src[self.pos..];
            let suffix = INT_SUFFIXES
                .iter()
                .chain(FLOAT_SUFFIXES)
                .filter(|s| rest.starts_with(**s))
                .max_by_key(|s| s.len());
            if let Some(sfx) = suffix {
                let digits = &self.src[lit_start..at];
                let span = self.span_at(at, self.pos);
                let d = NabiDiagnostic::error(
                    Code::MALFORMED_NUMBER,
                    span,
                    "numeric literal cannot end with `_`",
                )
                .with_file(self.file.clone())
                .with_help(format!(
                    "a suffix follows the digits directly: `{digits}{sfx}`"
                ));
                self.diagnostics.push(d);
                return;
            }
            (
                "numeric literal cannot end with `_`".to_owned(),
                at,
                self.pos,
            )
        } else if let Some(i) = run.find("__") {
            let msg = "consecutive `_` in numeric literal".to_owned();
            (msg, start + i, start + unders(i))
        } else {
            return;
        };
        let span = self.span_at(from, to);
        let d = NabiDiagnostic::error(Code::MALFORMED_NUMBER, span, msg)
            .with_file(self.file.clone())
            .with_help("write `_` only between two digits, one at a time: `1_000`, `0xFF_FF`");
        self.diagnostics.push(d);
    }

    /// A suffix fixes the type, so the value must fit. Unsuffixed literals
    /// are checked by HIR against the slot (`int` / `float` hold anything).
    fn check_range(&mut self, start: usize, body: &str, radix: u32, is_float: bool, suffix: &str) {
        if suffix.is_empty() {
            return;
        }
        let clean: String = body.chars().filter(|&c| c != '_').collect();
        if is_float {
            let inf = match suffix {
                "f32" => clean.parse::<f32>().is_ok_and(f32::is_infinite),
                _ => clean.parse::<f64>().is_ok_and(f64::is_infinite),
            };
            if inf {
                self.error_from(
                    Code::LITERAL_OUT_OF_RANGE,
                    start,
                    format!(
                        "float literal out of range for `{}`",
                        if suffix.is_empty() { "f64" } else { suffix }
                    ),
                );
            }
            return;
        }
        let digits = if radix == 10 { &clean[..] } else { &clean[2..] };
        let Ok(value) = u128::from_str_radix(digits, radix) else {
            if !digits.is_empty() && digits.chars().all(|c| c.is_digit(radix)) {
                self.error_from(
                    Code::LITERAL_OUT_OF_RANGE,
                    start,
                    "integer literal is too large",
                );
            }
            return;
        };
        let (bits, signed) = match suffix {
            "i8" => (8, true),
            "i16" => (16, true),
            "i32" => (32, true),
            "i64" | "isize" => (64, true),
            "u8" => (8, false),
            "u16" => (16, false),
            "u32" => (32, false),
            "u64" | "usize" => (64, false),
            _ => return,
        };
        // Only a unary `-` directly before the literal admits
        // `MAX + 1` (`-128i8`); in `a - 128i8` the literal is out of range.
        let max = if signed {
            (1u128 << (bits - 1)) - u128::from(!self.prev_minus)
        } else {
            (1u128 << bits) - 1
        };
        if value > max {
            let neg = signed && self.prev_minus;
            let bound = if neg {
                format!("min -{max}")
            } else {
                format!("max {max}")
            };
            let sign = if neg { "-" } else { "" };
            self.error_from(
                Code::LITERAL_OUT_OF_RANGE,
                start,
                format!("literal `{sign}{body}` out of range for `{suffix}` ({bound})"),
            );
            let wider = match bits {
                8 => Some(16),
                16 => Some(32),
                32 => Some(64),
                _ => None,
            };
            if let (Some(w), Some(d)) = (wider, self.diagnostics.last_mut()) {
                let p = if signed { 'i' } else { 'u' };
                d.notes
                    .push(format!("help: use a wider type: `{sign}{body}{p}{w}`"));
            }
        }
    }

    /// `'` starts a char literal (`'X'`, `'\n'`) or a label (`'name`).
    fn lex_quote_or_label(&mut self) -> TokenKind {
        let mut rest = self.src[self.pos + 1..].chars();
        let c1 = rest.next().unwrap_or('\'');
        if is_ident_start(c1) && rest.next() != Some('\'') {
            // Label unless the identifier run is closed by `'` or goes on
            // with an escape (`'e\u{301}'`): then it is an over-long char
            // literal, reported as such.
            let save = (self.pos, self.line, self.col);
            self.advance(1);
            let start = self.pos;
            self.eat_ident_continue();
            if !matches!(self.bytes.get(self.pos), Some(b'\'' | b'\\')) {
                return TokenKind::Label(normalize_ident(&self.src[start..self.pos]));
            }
            (self.pos, self.line, self.col) = save;
        }
        self.lex_char_lit(None)
    }

    /// `'x'`, `c'x'` or (`prefix` `b`) the byte `b'x'`.
    fn lex_char_lit(&mut self, prefix: Option<char>) -> TokenKind {
        let lit_start = self.pos;
        let c_char = prefix.is_some();
        self.advance(usize::from(c_char) + 1); // optional `c` / `b`, then `'`
        let start = self.pos;
        let Some(end) = self.scan_quoted(b'\'') else {
            self.error_from(Code::UNTERMINATED, lit_start, "unterminated char literal");
            return TokenKind::CharLit(String::new());
        };
        let body = self.src[start..end].to_owned();
        self.note_quoted(start, end, Quoted::Escaped);
        let mode = if c_char {
            EscapeMode::CChar
        } else {
            EscapeMode::Char
        };
        match unescape(&body, mode) {
            Err((at, msg)) => self.escape_error(start, at, msg),
            Ok(v) => {
                let mut chars = v.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) => {
                        if c_char && (c as u32 > 0xFF || (!c.is_ascii() && !body.starts_with('\\')))
                        {
                            self.error_from(
                                Code::CHAR_LITERAL_LENGTH,
                                lit_start,
                                format!(
                                    "`{}'{body}'` must be one byte; write non-ASCII bytes as \
                                     `\\xHH`",
                                    prefix.unwrap_or('c')
                                ),
                            );
                        }
                    }
                    (first, _) => {
                        self.error_from(
                            Code::CHAR_LITERAL_LENGTH,
                            lit_start,
                            "char literal must contain exactly one code point",
                        );
                        // `'hello'` (Python, JS): text is a string.
                        if first.is_some() && prefix.is_none() {
                            if let Some(d) = self.diagnostics.pop() {
                                self.diagnostics.push(d.with_help(format!(
                                    "a string uses double quotes: `\"{body}\"`"
                                )));
                            }
                        }
                    }
                }
            }
        }
        if prefix == Some('b') {
            TokenKind::ByteCharLit(body)
        } else if c_char {
            TokenKind::CCharLit(body)
        } else {
            TokenKind::CharLit(body)
        }
    }

    /// Skip to past the closing `quote`; the body's end, or `None` at EOF
    /// (for chars, at a line end).
    fn scan_quoted(&mut self, quote: u8) -> Option<usize> {
        loop {
            match self.bytes.get(self.pos) {
                None => return None,
                Some(&b) if b == quote => {
                    let end = self.pos;
                    self.advance(1);
                    return Some(end);
                }
                Some(b'\n' | b'\r') if quote == b'\'' => return None,
                Some(b'\\') => {
                    self.advance(1);
                    if let Some(c) = self.peek_char() {
                        self.advance(c.len_utf8());
                    }
                }
                Some(_) => {
                    let n = self.peek_char().map_or(1, char::len_utf8);
                    self.advance(n);
                }
            }
        }
    }

    fn lex_str_lit(&mut self) -> TokenKind {
        let lit_start = self.pos;
        self.advance(1);
        let start = self.pos;
        let Some(end) = self.scan_quoted(b'"') else {
            self.error_from(Code::UNTERMINATED, lit_start, "unterminated string literal");
            return TokenKind::StrLit(String::new());
        };
        let body = self.src[start..end].to_owned();
        self.note_quoted(start, end, Quoted::Escaped);
        if let Err((at, msg)) = unescape(&body, EscapeMode::Str) {
            self.escape_error(start, at, msg);
        }
        TokenKind::StrLit(body)
    }

    /// `b"..."`: the escapes of `c'x'` (any value up to `\xFF`), and
    /// only ASCII written directly.
    fn lex_byte_str_lit(&mut self) -> TokenKind {
        let lit_start = self.pos;
        self.advance(2);
        let start = self.pos;
        let Some(end) = self.scan_quoted(b'"') else {
            self.error_from(
                Code::UNTERMINATED,
                lit_start,
                "unterminated byte string literal",
            );
            return TokenKind::ByteStrLit(String::new());
        };
        let body = self.src[start..end].to_owned();
        self.note_quoted(start, end, Quoted::Raw);
        match unescape(&body, EscapeMode::CChar) {
            Err((at, msg)) => self.escape_error(start, at, msg),
            Ok(_) if !body.is_ascii() => self.error_from(
                Code::INVALID_ESCAPE,
                lit_start,
                "a byte string holds ASCII only; write other bytes as `\\xHH`",
            ),
            Ok(v) if v.chars().any(|c| c as u32 > 0xFF) => {
                self.error_from(
                    Code::INVALID_ESCAPE,
                    lit_start,
                    "a byte string escape must be at most `\\xFF`",
                );
            }
            Ok(_) => {}
        }
        TokenKind::ByteStrLit(body)
    }

    /// Inside a hole: `}` / `:` at depth 0 end or split it;
    /// `None` lexes `b` as code.
    fn hole_punct(&mut self, b: u8, next: Option<u8>) -> Option<TokenKind> {
        let Some(Frame::Hole {
            triple,
            depth,
            start,
        }) = self.fstr.last_mut()
        else {
            return None;
        };
        let (triple, start) = (*triple, *start);
        match b {
            // `{x=}` / `{x = :.2}`: the hole's text up to here, as
            // `FStrText` (never a hole's token otherwise).
            b'=' if *depth == 0 => {
                let mut end = self.pos + 1;
                while self.bytes.get(end) == Some(&b' ') {
                    end += 1;
                }
                let echo = match self.bytes.get(end) {
                    Some(b'}') => true,
                    Some(b':') => self.bytes.get(end + 1) != Some(&b':'),
                    _ => false,
                };
                if !echo {
                    return None;
                }
                let text = self.src[start..end].to_owned();
                self.advance(end - self.pos);
                Some(TokenKind::FStrText(text))
            }
            b'}' if *depth == 0 => {
                self.fstr.pop();
                self.advance(1);
                Some(TokenKind::HoleEnd)
            }
            b':' if *depth == 0 && next != Some(b':') => {
                *self.fstr.last_mut()? = Frame::Spec { triple };
                self.advance(1);
                Some(TokenKind::HoleSpec)
            }
            b'(' | b'[' | b'{' => {
                *depth += 1;
                None
            }
            b')' | b']' | b'}' => {
                *depth = depth.saturating_sub(1);
                None
            }
            _ => None,
        }
    }

    /// The next f-string text run, spec run, `{` / `${` (hole start),
    /// spec-closing `}` or closing quote. `None` after a reported stray `}`.
    fn fstr_piece(&mut self) -> Option<Token> {
        let (start, line, col) = (self.pos, self.line, self.col);
        let (triple, spec) = match *self.fstr.last()? {
            Frame::Text { triple, .. } => (triple, false),
            Frame::Spec { triple } => (triple, true),
            Frame::Hole { .. } => return None,
        };
        let closes = |s: &Self| !triple || s.src[s.pos..].starts_with("\"\"\"");
        let mut text = String::new();
        while let Some(&b) = self.bytes.get(self.pos) {
            let next = self.bytes.get(self.pos + 1).copied();
            match b {
                b'"' if spec || closes(self) => break,
                b'{' | b'}' if !spec && next == Some(b) => {
                    text.push(char::from(b));
                    self.advance(2);
                }
                b'}' if !spec => {
                    let at = self.pos;
                    self.advance(1);
                    self.error_from(
                        Code::FSTRING_SYNTAX,
                        at,
                        "single `}` in f-string (write `}}`)",
                    );
                }
                b'$' if spec && next == Some(b'$') => {
                    text.push('$');
                    self.advance(2);
                }
                b'{' | b'}' => break,
                b'$' if spec && next == Some(b'{') => break,
                _ => {
                    let n = usize::from(b == b'\\' && !spec);
                    let end = self.src[self.pos + n..]
                        .chars()
                        .next()
                        .map_or(self.pos + n, |c| self.pos + n + c.len_utf8());
                    text.push_str(&self.src[self.pos..end]);
                    self.advance(end - self.pos);
                }
            }
        }
        if self.pos > start {
            self.note_quoted(start, self.pos, Quoted::Escaped);
        }
        let kind = if !text.is_empty() {
            if spec {
                TokenKind::SpecText(text)
            } else {
                TokenKind::FStrText(text)
            }
        } else if self.pos > start || self.pos >= self.bytes.len() {
            return None;
        } else {
            match self.bytes[self.pos] {
                b'"' if spec => {
                    self.error_from(
                        Code::FSTRING_SYNTAX,
                        start,
                        "unclosed `{` in f-string (missing `}`)",
                    );
                    self.fstr.pop();
                    TokenKind::HoleEnd
                }
                b'"' => {
                    self.advance(if triple { 3 } else { 1 });
                    self.fstr.pop();
                    TokenKind::FStrEnd(triple)
                }
                b'}' => {
                    self.advance(1);
                    self.fstr.pop();
                    TokenKind::HoleEnd
                }
                b => {
                    self.advance(if b == b'$' { 2 } else { 1 });
                    self.fstr.push(Frame::Hole {
                        triple,
                        depth: 0,
                        start: self.pos,
                    });
                    TokenKind::HoleStart
                }
            }
        };
        Some(Token::new(
            kind,
            Self::make_span(start, self.pos, line, col),
        ))
    }

    /// Inside the code of a hole of a one-line `f"..."`, at any depth.
    fn in_flat_hole(&self) -> bool {
        self.fstr.iter().any(|f| {
            matches!(
                f,
                Frame::Hole { triple: false, .. } | Frame::Spec { triple: false }
            )
        })
    }

    /// One hole error per f-string literal: the rest is likely fallout.
    fn hole_error(&mut self, at: usize, msg: &str) {
        let outer = match self.fstr.first() {
            Some(&Frame::Text { start, .. }) => Some(start),
            _ => None,
        };
        if outer.is_none() || outer != self.hole_reported {
            self.hole_reported = outer;
            let (line, col) = self.line_col(at);
            let end = at + self.src[at..].chars().next().map_or(0, char::len_utf8);
            self.error(
                Code::FSTRING_SYNTAX,
                Self::make_span(at, end, line, col),
                msg,
            );
        }
    }

    /// At EOF: report an unterminated f-string and close its open
    /// holes and literals so the parser sees balanced tokens.
    fn close_fstrings(&mut self, tokens: &mut Vec<Token>) {
        if let Some(&Frame::Text { start, .. }) = self.fstr.first() {
            self.error_from(Code::UNTERMINATED, start, "unterminated f-string literal");
        }
        while let Some(f) = self.fstr.pop() {
            let kind = match f {
                Frame::Text { triple, .. } => TokenKind::FStrEnd(triple),
                Frame::Hole { .. } | Frame::Spec { .. } => TokenKind::HoleEnd,
            };
            tokens.push(Token::new(kind, self.span_here()));
        }
    }

    fn lex_raw_str_lit(&mut self) -> TokenKind {
        // r"..." or r#"..."# / r##"..."##.
        let lit_start = self.pos;
        self.advance(1); // `r`
        let mut hash_count = 0usize;
        while self.bytes.get(self.pos) == Some(&b'#') {
            hash_count += 1;
            self.advance(1);
        }
        if self.bytes.get(self.pos) != Some(&b'"') {
            self.error_from(
                Code::UNTERMINATED,
                lit_start,
                "expected `\"` after raw-string prefix",
            );
            return TokenKind::RawStrLit(String::new());
        }
        self.advance(1);
        let start = self.pos;
        let closing = format!("\"{}", "#".repeat(hash_count));
        let Some(len) = self.src[start..].find(&closing) else {
            self.advance(self.bytes.len() - self.pos);
            self.error_from(Code::UNTERMINATED, lit_start, "unterminated raw string");
            return TokenKind::RawStrLit(String::new());
        };
        self.advance(len + closing.len());
        self.note_quoted(start, start + len, Quoted::Raw);
        TokenKind::RawStrLit(self.src[start..start + len].to_owned())
    }

    fn skip_trivia(&mut self) {
        while let Some(&b) = self.bytes.get(self.pos) {
            match b {
                b' ' | b'\t' | b'\n' | b'\r' | 0x0B | 0x0C => self.advance(1),
                b'/' if self.bytes.get(self.pos + 1) == Some(&b'/') => {
                    let len = self.src[self.pos..]
                        .find(['\n', '\r'])
                        .unwrap_or(self.bytes.len() - self.pos);
                    let line = &self.src[self.pos..self.pos + len];
                    // `////` is a plain comment, as in Rust.
                    let doc = match line.get(..3) {
                        Some("///") if !line.starts_with("////") => Some(false),
                        Some("//!") => Some(true),
                        _ => None,
                    };
                    self.note_quoted(self.pos, self.pos + len, Quoted::Comment);
                    if let Some(inner) = doc {
                        let span = Self::make_span(self.pos, self.pos + len, self.line, self.col);
                        let text = &line[3..];
                        self.docs.push(DocLine {
                            inner,
                            text: text.strip_prefix(' ').unwrap_or(text).to_owned(),
                            span,
                        });
                    }
                    self.advance(len);
                }
                b'/' if self.bytes.get(self.pos + 1) == Some(&b'*') => self.skip_block_comment(),
                _ => break,
            }
        }
    }

    /// A (nested) block comment; `/** .. */` and `/*! .. */` are doc
    /// comments, but `/**/`, `/***/` and `/*** ..` are not (as in Rust).
    fn skip_block_comment(&mut self) {
        let start = self.pos;
        let (line, col) = (self.line, self.col);
        let doc = match (self.bytes.get(start + 2), self.bytes.get(start + 3)) {
            (Some(b'!'), _) => Some(true),
            (Some(b'*'), Some(c)) if !matches!(c, b'*' | b'/') => Some(false),
            _ => None,
        };
        self.advance(2);
        let mut depth = 1usize;
        while depth > 0 {
            match (self.bytes.get(self.pos), self.bytes.get(self.pos + 1)) {
                (None, _) => {
                    self.note_quoted(start, self.pos, Quoted::Comment);
                    self.error_from(Code::UNTERMINATED, start, "unterminated block comment");
                    return;
                }
                (Some(b'/'), Some(b'*')) => {
                    self.advance(2);
                    depth += 1;
                }
                (Some(b'*'), Some(b'/')) => {
                    self.advance(2);
                    depth -= 1;
                }
                _ => self.advance(1),
            }
        }
        self.note_quoted(start, self.pos, Quoted::Comment);
        if let Some(inner) = doc {
            self.docs.push(DocLine {
                inner,
                text: block_doc_text(&self.src[start + 3..self.pos - 2]),
                span: Self::make_span(start, self.pos, line, col),
            });
        }
    }

    /// Move forward `n` bytes, tracking line (LF, CRLF, CR) and
    /// code-point column.
    fn advance(&mut self, n: usize) {
        let end = (self.pos + n).min(self.bytes.len());
        for i in self.pos..end {
            match self.bytes[i] {
                b'\n' if i > 0 && self.bytes[i - 1] == b'\r' => {}
                b'\n' | b'\r' => {
                    self.line += 1;
                    self.col = 1;
                }
                b if b & 0xC0 == 0x80 => {} // UTF-8 continuation byte
                _ => self.col += 1,
            }
        }
        self.pos = end;
    }

    fn peek_char(&self) -> Option<char> {
        self.src.get(self.pos..)?.chars().next()
    }

    fn span_here(&self) -> Span {
        let start = u32::try_from(self.pos).unwrap_or(u32::MAX);
        Span::new(start, start, self.line, self.col)
    }

    fn make_span(start: usize, end: usize, line: u32, col: u32) -> Span {
        Span::new(
            u32::try_from(start).unwrap_or(u32::MAX),
            u32::try_from(end).unwrap_or(u32::MAX),
            line,
            col,
        )
    }

    /// Error spanning `start..pos`, positioned at `start`'s line/column.
    fn error_from(&mut self, code: Code, start: usize, msg: impl Into<String>) {
        let (line, col) = self.line_col(start);
        let span = Self::make_span(start, self.pos, line, col);
        self.error(code, span, msg);
    }

    /// Line and column of `at`, from a line index built once: errors
    /// cost a binary search, not a scan from the start.
    fn line_col(&mut self, at: usize) -> (u32, u32) {
        if self.line_starts.is_empty() {
            self.line_starts.push(0);
            let b = self.bytes;
            for i in 0..b.len() {
                if b[i] == b'\n' || (b[i] == b'\r' && b.get(i + 1) != Some(&b'\n')) {
                    self.line_starts.push(i + 1);
                }
            }
        }
        let line = self.line_starts.partition_point(|&s| s <= at);
        let line_start = self.line_starts[line - 1];
        let col = 1 + self.src[line_start..at].chars().count();
        (
            u32::try_from(line).unwrap_or(u32::MAX),
            u32::try_from(col).unwrap_or(u32::MAX),
        )
    }

    fn span_at(&mut self, start: usize, end: usize) -> Span {
        let (line, col) = self.line_col(start);
        Self::make_span(start, end, line, col)
    }

    fn warning(&mut self, span: Span, msg: impl Into<String>, lint: &str) {
        self.diagnostics.push(
            NabiDiagnostic::warning(Code::LINT, span, msg)
                .with_file(self.file.clone())
                .with_lint(lint),
        );
    }

    fn error(&mut self, code: Code, span: Span, msg: impl Into<String>) {
        self.diagnostics
            .push(NabiDiagnostic::error(code, span, msg).with_file(self.file.clone()));
    }
}

/// Text of a block doc comment: drop a blank first and last
/// line, a common `*` prefix, common indentation and the whitespace
/// before `*/`; line breaks become LF.
fn block_doc_text(body: &str) -> String {
    let body = body.replace("\r\n", "\n").replace('\r', "\n");
    let mut lines: Vec<&str> = body.split('\n').collect();
    if lines.len() > 1 && lines[0].trim().is_empty() {
        lines.remove(0);
    }
    if lines.len() > 1 && lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    let starred = |l: &&str| l.trim_start().starts_with('*');
    let from = usize::from(!lines.first().is_some_and(starred));
    if body.contains('\n') && lines.len() > from && lines[from..].iter().all(starred) {
        for l in &mut lines[from..] {
            *l = &l.trim_start()[1..];
        }
    }
    let indent = lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start_matches([' ', '\t']).len())
        .min()
        .unwrap_or(0);
    let text: Vec<&str> = lines
        .iter()
        .map(|l| l.get(indent..).unwrap_or(""))
        .collect();
    text.join("\n").trim_end().to_owned()
}

/// A byte that starts a fixed operator token. A registered token that
/// starts with one is scoped to its package (`ExtOps::in_package`).
#[must_use]
pub const fn fixed_lead(b: u8) -> bool {
    matches!(
        b,
        b'~' | b'@'
            | b'+'
            | b'-'
            | b'*'
            | b'/'
            | b'%'
            | b'^'
            | b'='
            | b'!'
            | b'<'
            | b'>'
            | b'&'
            | b'|'
            | b'.'
            | b':'
            | b'?'
    )
}

/// Unicode math operators. `÷ ≡ ≠ ≤ ≥` are synonyms of
/// `/ == != <= >=` and lex as those tokens.
const fn math_op(c: char) -> Option<TokenKind> {
    Some(match c {
        '÷' => TokenKind::Slash,
        '≡' => TokenKind::EqEq,
        '≠' => TokenKind::NotEq,
        '≤' => TokenKind::LtEq,
        '≥' => TokenKind::GtEq,
        '×' | '⋅' | '∘' | '√' | '∈' | '∉' | '⊂' | '⊆' | '∪' | '∩' => {
            TokenKind::MathOp(c)
        }
        _ => return None,
    })
}

/// A joiner, selector or tag where it belongs: inside an emoji
/// sequence (`\u{1F468}\u{200D}\u{1F469}`, `\u{2764}\u{FE0F}`), or a ZWNJ after
/// a letter of a script that writes it (Persian, the Indic scripts).
fn expected_joiner(prev: Option<char>, c: char) -> bool {
    let Some(p) = prev else {
        return false;
    };
    let emoji = is_pictographic(p) || is_emoji_component(p) || p == '\u{200D}';
    match c {
        '\u{200D}' | '\u{FE0E}' | '\u{FE0F}' | '\u{E0020}'..='\u{E007F}' => emoji,
        '\u{200C}' => !p.is_ascii() && p.is_alphabetic() && latin_twin(p).is_none() && !is_latin(p),
        _ => false,
    }
}

const fn is_latin(c: char) -> bool {
    matches!(c, '\u{C0}'..='\u{24F}' | '\u{1E00}'..='\u{1EFF}')
}

/// A symbol that reads as an operator (math operators, arrows,
/// technical and geometric symbols): one no trait registers is named so.
const fn is_operator_symbol(c: char) -> bool {
    matches!(c,
        '\u{B1}' | '\u{AC}' | '\u{B0}' | '\u{A7}' | '\u{B6}' | '\u{2016}'..='\u{2027}'
        | '\u{2030}'..='\u{205E}' | '\u{2190}'..='\u{2BFF}' | '\u{2E00}'..='\u{2E7F}'
        | '\u{3008}'..='\u{3020}')
}

const fn is_whitespace(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0B' | '\x0C')
}

#[cfg(test)]
mod tests {
    use super::Lexer;
    use crate::TokenKind as T;

    fn lex(src: &str) -> (Vec<T>, Vec<String>) {
        let (toks, diags) = Lexer::new(src, "t").lex_all();
        (
            toks.into_iter().map(|t| t.kind).collect(),
            diags.into_iter().map(|d| d.message).collect(),
        )
    }

    fn ok(src: &str) -> Vec<T> {
        let (t, d) = lex(src);
        assert!(d.is_empty(), "{src:?}: {d:?}");
        t
    }

    /// In-place broadcast and `.??`: longest match, `.==` kept.
    #[test]
    fn dot_assign_tokens() {
        let id = |s: &str| T::Ident(s.to_owned());
        assert_eq!(
            ok(
                "y .= x .+= x .-= x .*= x ./= x .%= x .**= x .?? x .== x .%%= x .&= x .|= x .^= x .<<= x .>>= x .& x .<= x"
            ),
            [
                id("y"),
                T::DotEq,
                id("x"),
                T::DotPlusEq,
                id("x"),
                T::DotMinusEq,
                id("x"),
                T::DotStarEq,
                id("x"),
                T::DotSlashEq,
                id("x"),
                T::DotPercentEq,
                id("x"),
                T::DotStarStarEq,
                id("x"),
                T::DotQuestionQuestion,
                id("x"),
                T::DotEqEq,
                id("x"),
                T::DotPercentPercentEq,
                id("x"),
                T::DotAmpEq,
                id("x"),
                T::DotPipeEq,
                id("x"),
                T::DotCaretEq,
                id("x"),
                T::DotShlEq,
                id("x"),
                T::DotShrEq,
                id("x"),
                T::DotAmp,
                id("x"),
                T::DotLtEq,
                id("x"),
                T::Eof,
            ]
        );
    }

    fn err(src: &str) {
        let (_, d) = lex(src);
        assert!(!d.is_empty(), "{src:?} should be rejected");
    }

    /// `(inner, text)` of the doc comments before the first token.
    fn docs(src: &str) -> Vec<(bool, String)> {
        let (toks, diags) = Lexer::new(src, "t").lex_all();
        assert!(diags.is_empty(), "{src:?}: {diags:?}");
        toks[0]
            .docs
            .iter()
            .map(|d| (d.inner, d.text.clone()))
            .collect()
    }

    /// A number glued to a stray character, after a wide one: the
    /// diagnostic quotes the number without cutting a character.
    #[test]
    fn glued_suffix_after_wide_char() {
        for src in ["\"\"\u{6c3}.2\u{1c}", "\u{6c3}+3\u{a7}", "x\u{e9}9\u{1c}"] {
            let (_, d) = Lexer::new(src, "t").lex_all();
            assert!(!d.is_empty(), "{src:?}");
        }
    }

    #[test]
    fn shebang_line() {
        assert_eq!(ok("#!/usr/bin/env nabi\nx"), [T::Ident("x".into()), T::Eof]);
        assert_eq!(ok("\u{FEFF}#!nabi -q\r\nx"), [T::Ident("x".into()), T::Eof]);
        assert_eq!(ok("#!nabi"), [T::Eof]);
        assert_eq!(ok("#![a]")[0], T::HashBangLBracket);
        // Only the first line.
        err("x\n#!/usr/bin/env nabi\n");
        let (toks, _) = Lexer::new("#!/bin/nabi\nx", "t").lex_all();
        assert_eq!((toks[0].span.line, toks[0].span.col), (2, 1));
    }

    #[test]
    fn block_doc_comments() {
        let d = |s: &str| (false, s.to_owned());
        assert_eq!(docs("/** One. */x"), [d("One.")]);
        assert_eq!(docs("/*! Mod. */x"), [(true, "Mod.".to_owned())]);
        assert_eq!(
            docs("/**\n * A.\n *\n *     code\n */x"),
            [d("A.\n\n    code")]
        );
        assert_eq!(docs("/**\r\n    A.\r\n      B.\r\n*/x"), [d("A.\n  B.")]);
        assert_eq!(docs("/** a /* b */ c */x"), [d("a /* b */ c")]);
        assert_eq!(docs("/**/x"), []);
        assert_eq!(docs("/***/x"), []);
        assert_eq!(docs("/*** no */x"), []);
        assert_eq!(docs("/**/ /*!*/x"), [(true, String::new())]);
        assert_eq!(docs("//// no\n/// yes\nx"), [d("yes")]);
        assert_eq!(docs("r\"/// no\"")[..], []);
        err("/** open");
    }

    #[test]
    fn raw_identifiers() {
        let (toks, d) = Lexer::new("r#match r#move r#\"s\"# r##\"t\"## x", "t").lex_all();
        assert!(d.is_empty(), "{d:?}");
        let kinds: Vec<&T> = toks.iter().map(|t| &t.kind).collect();
        assert_eq!(
            kinds,
            [
                &T::Ident("match".into()),
                &T::Ident("move".into()),
                &T::RawStrLit("s".into()),
                &T::RawStrLit("t".into()),
                &T::Ident("x".into()),
                &T::Eof
            ]
        );
        assert!(toks[0].raw && toks[1].raw && !toks[4].raw);
        assert_eq!(toks[0].span.end - toks[0].span.start, 7);
        assert_eq!(T::Ident("match".into()).text(), "r#match");
        for bad in ["r#self", "r#Self", "r#_"] {
            err(bad);
        }
    }

    #[test]
    fn leading_zero_is_an_error() {
        for (src, help) in [
            (
                "0755",
                "help: write `0o755` for an octal number, or `755` for decimal",
            ),
            (
                "0_1u8",
                "help: write `0o1u8` for an octal number, or `1u8` for decimal",
            ),
            ("089", "help: write `89`"),
            ("00", "help: write `0`"),
        ] {
            let (toks, d) = Lexer::new(src, "t").lex_all();
            assert!(matches!(toks[0].kind, T::IntLit(_)), "{src}");
            assert_eq!(d.len(), 1, "{src}: {d:?}");
            assert!(
                d[0].lint.is_none() && d[0].severity == nabic_diagnostic::Severity::Error,
                "{src}"
            );
            assert_eq!(d[0].notes, [help], "{src}");
        }
        for quiet in ["0", "0.5", "00.5", "0e3", "0o17", "10", "0f64"] {
            ok(quiet);
        }
    }

    #[test]
    fn numeric_literals() {
        for good in [
            "1_000",
            "0xFF_FF",
            "0b1010_0101",
            "3.141_592",
            "1.",
            "2e10",
            "1.5e-3f32",
            "0xFFu8",
            "1f32",
            "-128i8",
            "255u8",
            "2i", // a `#[literal]` suffix: the parser resolves it
        ] {
            ok(good);
        }
        for bad in [
            "0x", "0b", "0o", "1__0", "1_", "0x_FF", "1e", "0b102", "0o8", "1.5i32", "300i8",
            "256u8", "128i8", "1e999f32", "0b1f32",
        ] {
            err(bad);
        }
        assert_eq!(ok("1f32")[0], T::FloatLit("1f32".into()));
        assert_eq!(
            ok("1.foo"),
            vec![
                T::IntLit("1".into()),
                T::Dot,
                T::Ident("foo".into()),
                T::Eof
            ]
        );
        assert_eq!(
            ok("1..2"),
            vec![
                T::IntLit("1".into()),
                T::DotDot,
                T::IntLit("2".into()),
                T::Eof
            ]
        );
        // `.5` after an expression is `.` + tuple index.
        assert_eq!(
            ok("t.5"),
            vec![T::Ident("t".into()), T::Dot, T::IntLit("5".into()), T::Eof]
        );
        err("x = .5");
        err("5 - 128i8");
        err("0xFFi8");
        ok("x = -128i8");
    }

    #[test]
    fn strings_and_chars() {
        ok(r##""a\n\u{AC00}\x41" 'x' '\'' '\u{1F600}' c'\xFF' r"\q" r#"a"b"# '세'"##);
        for bad in [
            r#""\q""#,
            r#""\x80""#,
            r#""\u{110000}""#,
            "'ab'",
            "''",
            "c'세'",
            "c'é'",
            "\"abc",
            "'\\n",
        ] {
            err(bad);
        }
        assert_eq!(ok("\"a\\\n   b\"")[0], T::StrLit("a\\\n   b".into()));
    }

    #[test]
    fn byte_strings() {
        assert_eq!(
            ok(r#"b"a\xFF\n" b"" b"#),
            [
                T::ByteStrLit(r"a\xFF\n".into()),
                T::ByteStrLit(String::new()),
                T::Ident("b".into()),
                T::Eof
            ]
        );
        for bad in [r#"b"é""#, r#"b"\u{100}""#, r#"b"\q""#, "b\"ab"] {
            err(bad);
        }
    }

    #[test]
    fn fstrings() {
        let id = |s: &str| T::Ident(s.into());
        let text = |s: &str| T::FStrText(s.into());
        assert_eq!(
            ok(r#"f"a {x} {{b}}""#),
            [
                T::FStrStart(false),
                text("a "),
                T::HoleStart,
                id("x"),
                T::HoleEnd,
                text(" {b}"),
                T::FStrEnd(false),
                T::Eof
            ]
        );
        // Hole code is ordinary tokens: the enclosing quote is reused.
        assert_eq!(
            ok(r#"f"{a + "}"}""#)[2..5],
            [id("a"), T::Plus, T::StrLit("}".into())]
        );
        assert_eq!(
            ok(r#"f"{d["k"]}""#)[2..6],
            [id("d"), T::LBracket, T::StrLit("k".into()), T::RBracket]
        );
        assert_eq!(ok(r#"f"{"a\tb"}""#)[2], T::StrLit("a\\tb".into()));
        // Specs: text runs (`$$` folded), `${..}` / `{..}` nested holes,
        // `::` stays a path separator.
        assert_eq!(
            ok(r#"f"{f"{a::b:>{w}.${p}$$}"}""#),
            [
                T::FStrStart(false),
                T::HoleStart,
                T::FStrStart(false),
                T::HoleStart,
                id("a"),
                T::ColonColon,
                id("b"),
                T::HoleSpec,
                T::SpecText(">".into()),
                T::HoleStart,
                id("w"),
                T::HoleEnd,
                T::SpecText(".".into()),
                T::HoleStart,
                id("p"),
                T::HoleEnd,
                T::SpecText("$".into()),
                T::HoleEnd,
                T::FStrEnd(false),
                T::HoleEnd,
                T::FStrEnd(false),
                T::Eof
            ]
        );
        // `f"""`: raw `"` in text, hole code across lines.
        assert_eq!(
            ok("f\"\"\"say \"{\n  x\n}\" \"\"\"")[..5],
            [
                T::FStrStart(true),
                text("say \""),
                T::HoleStart,
                id("x"),
                T::HoleEnd
            ]
        );
        assert_eq!(ok("f\"a\nb\"")[1], text("a\nb"));
        for bad in [
            "f\"a } b\"",
            "f\"{x\n}\"",
            "f\"{f\"\n\"}\"",
            "f\"{x",
            "f\"{x:5\"",
            "f\"{(}\"",
        ] {
            err(bad);
        }
        // A backslash in hole code: one error at the first one.
        let (toks, d) = Lexer::new("\n  f\"{n ?? \\\"a\\\"}\"", "t").lex_all();
        assert_eq!(toks[toks.len() - 2].kind, T::FStrEnd(false));
        assert_eq!(d.len(), 1, "{d:?}");
        assert!(d[0].message.contains("write `\"` directly"), "{d:?}");
        let at = d[0].primary.as_ref().unwrap().span;
        assert_eq!((at.line, at.col), (2, 11));
    }

    #[test]
    fn labels_and_idents() {
        assert_eq!(ok("'바깥: loop")[0], T::Label("바깥".into()));
        assert_eq!(ok("'a'")[0], T::CharLit("a".into()));
        for id in ["α", "Δt", "σ²", "변수1", "データ", "_internal", "e\u{301}"] {
            assert_eq!(ok(id), vec![T::Ident(crate::unicode::nfc(id)), T::Eof]);
        }
        assert_eq!(ok("unsafe using")[..2], [T::Unsafe, T::Using]);
        assert_eq!(ok("a %% b %%= c .%% d .&& e .|| f")[1], T::PercentPercent);
    }

    #[test]
    fn positions_bom_whitespace_and_garbage() {
        let (toks, _) = Lexer::new("\u{FEFF}가나 x\ry\r\nz\x0B\x0Cw", "t").lex_all();
        let pos: Vec<_> = toks.iter().map(|t| (t.span.line, t.span.col)).collect();
        assert_eq!(pos[..5], [(1, 1), (1, 4), (2, 1), (3, 1), (3, 4)]);
        // A long run of invalid bytes: one diagnostic, no recursion.
        let (toks, d) = lex(&format!("a {} b", "$".repeat(200_000)));
        assert_eq!(d.len(), 1);
        assert_eq!(
            toks,
            vec![T::Ident("a".into()), T::Ident("b".into()), T::Eof]
        );
        err("/* unterminated");
        err("a # b");
    }

    #[test]
    fn token_text_lexes_back() {
        let src = r##"#[derive(X)] fn f<T>(x: ...T) -> i32 throws { 'l: loop { a .+= b; } }
            let s = "q\"\n" + r#"a"b"# + 'c' + c'\x01';
            x %%= 1e-3f32 ?? y?.z .<= 0xFFu8 << 2 ∈ ws; @x; #![a] .. ..= => ::"##;
        let toks = ok(src);
        let back: Vec<String> = toks.iter().map(T::text).collect();
        assert_eq!(ok(&back.join(" ")), toks);
        // F-string pieces carry their text exactly (no spaces between).
        let toks = ok(r#"f"""{{a}} "{f"{x:>{w}$$}"}" """"#);
        let back: String = toks.iter().map(T::text).collect();
        assert_eq!(ok(&back), toks);
    }

    /// `(code, message, first fix)` of each diagnostic of `src`.
    fn diags(src: &str) -> Vec<(String, String, Option<String>)> {
        let (_, d) = Lexer::new(src, "t").lex_all();
        d.into_iter()
            .map(|d| {
                let code = d
                    .lint
                    .as_deref()
                    .map_or_else(|| d.code.to_string(), Clone::clone);
                let fix = d.fixes.iter().next().map(|f| f.text.clone());
                (code, d.message, fix)
            })
            .collect()
    }

    #[test]
    fn stray_characters() {
        let one = |src: &str| {
            let d = diags(src);
            assert_eq!(d.len(), 1, "{src:?}: {d:?}");
            d.into_iter().next().unwrap()
        };
        let (code, msg, fix) = one("a \u{200B}b");
        assert_eq!(code, "E0147");
        assert!(msg.contains("U+200B ZERO WIDTH SPACE"), "{msg}");
        assert_eq!(fix.as_deref(), Some(""));
        let (code, msg, fix) = one("a\u{A0}= 1");
        assert_eq!((code.as_str(), fix.as_deref()), ("E0147", Some(" ")));
        assert!(msg.contains("NO-BREAK SPACE"), "{msg}");
        let (code, msg, _) = one("x \u{FEFF}");
        assert_eq!(code, "E0147");
        assert!(msg.contains("past the start of the file"), "{msg}");
        let (code, msg, _) = one("x \u{301}y");
        assert_eq!(code, "E0001");
        assert!(msg.contains("COMBINING ACUTE ACCENT") && msg.contains("cannot stand alone"));
        let (code, msg, _) = one("x \u{FE0F}");
        assert!(
            code == "E0147" && msg.contains("VARIATION SELECTOR-16"),
            "{msg}"
        );
        let (code, msg, fix) = one("3 \u{2212} 1");
        assert_eq!((code.as_str(), fix.as_deref()), ("E0148", Some("-")));
        assert!(msg.contains("MINUS SIGN"), "{msg}");
        assert_eq!(
            ok_with_errors("3 \u{2212} 1"),
            [
                T::IntLit("3".into()),
                T::Minus,
                T::IntLit("1".into()),
                T::Eof
            ]
        );
        let (code, _, fix) = one("f(\u{201C}a b\u{201D})");
        assert_eq!((code.as_str(), fix.as_deref()), ("E0148", Some("\"a b\"")));
        assert_eq!(
            ok_with_errors("\u{201C}a\u{201D}")[0],
            T::StrLit("a".into())
        );
        let (code, msg, _) = one("1 \u{229B} 2");
        assert_eq!(code, "E0001");
        assert!(msg.contains("CIRCLED ASTERISK OPERATOR") && msg.contains("not an operator"));
        let (code, msg, _) = one("x \u{378}");
        assert!(
            code == "E0001" && msg.contains("U+0378 is not a character"),
            "{msg}"
        );
        let (code, msg, _) = one("5\u{B0}");
        assert!(code == "E0114" && msg.contains("DEGREE SIGN"), "{msg}");
        // A `\` look-alike is only a token where `\` is registered.
        assert_eq!(one("a \u{29F5} b").0, "E0001");
        let (toks, d) = Lexer::new("a \u{29F5} b", "t")
            .with_ext_ops(vec!["\u{29F5}".into()])
            .lex_all();
        assert!(d.is_empty() && toks[1].kind == T::ExtOp("\u{29F5}".into()));
    }

    /// Tokens of `src`, which has errors.
    fn ok_with_errors(src: &str) -> Vec<T> {
        let (t, d) = lex(src);
        assert!(!d.is_empty(), "{src:?}");
        t
    }

    #[test]
    fn invisible_in_names() {
        let d = diags("let a\u{200B}b = 1;");
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!(d[0].0, "E0147");
        assert_eq!(d[0].2.as_deref(), Some("ab"));
        assert_eq!(ok_with_errors("a\u{200B}b")[0], T::Ident("ab".into()));
        assert_eq!(diags("a\u{200C}b")[0].2.as_deref(), Some("ab"));
        assert_eq!(diags("\u{3164} = 1")[0].0, "E0147");
        // Joiners where a script or an emoji needs them.
        ok("\u{645}\u{6CC}\u{200C}\u{62E}\u{648}\u{627}\u{647}\u{645}");
        ok("\u{1F468}\u{200D}\u{1F469} \u{2764}\u{FE0F}x");
        // Cyrillic look-alikes in a Latin name: the Latin spelling.
        let d = diags("p\u{430}y");
        assert_eq!(d[0].0, "confusable_idents");
        assert!(d[0].1.contains("CYRILLIC SMALL LETTER A"), "{d:?}");
        assert_eq!(d[0].2.as_deref(), Some("pay"));
    }

    #[test]
    fn hidden_in_strings_and_comments() {
        let d = diags("\"a\u{200B}b\" // c\u{2060}d\n/* \u{E0001} */ r\"\u{FEFF}\"");
        let got: Vec<(&str, Option<&str>)> = d
            .iter()
            .map(|(c, _, f)| (c.as_str(), f.as_deref()))
            .collect();
        assert_eq!(
            got,
            [
                ("hidden_chars", Some("\\u{200B}")),
                ("hidden_chars", Some("")),
                ("hidden_chars", Some("")),
                ("hidden_chars", None),
            ]
        );
        assert!(
            diags("\"\u{50000}\"")[0]
                .1
                .contains("no character in Unicode")
        );
        ok("\"\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467} \u{1F3F4}\u{E0067}\u{E0062}\u{E007F}\"");
        ok("\"\u{645}\u{6CC}\u{200C}\u{62E}\u{648}\u{627}\u{647}\u{645}\"");
        // Bidi controls: errors in comments and strings too.
        let d = diags("// \u{202E}\n\"\u{2066}\"");
        assert_eq!(d.len(), 2);
        assert!(
            d.iter()
                .all(|d| d.0 == "E0002" && d.2.as_deref() == Some(""))
        );
        assert!(d[0].1.contains("RIGHT-TO-LEFT OVERRIDE"), "{d:?}");
    }

    #[test]
    fn precise_spans() {
        let span = |src: &str| -> String {
            let (_, d) = Lexer::new(src, "t").lex_all();
            let s = d[0].primary.as_ref().unwrap().span;
            src[s.start as usize..s.end as usize].to_owned()
        };
        assert_eq!(span("x = 0b1021"), "2");
        assert_eq!(span("x = 1__0"), "__");
        assert_eq!(span("x = 10_"), "_");
        assert_eq!(span("x = 0x_1"), "_");
        assert_eq!(span("x = 2.5e"), "e");
        assert_eq!(span("x = 0x;"), "0x");
        assert_eq!(span(r#"x = "ab\qc""#), r"\q");
        assert_eq!(span(r#"x = "a\u{D800}""#), r"\u{D800}");
        assert_eq!(span(r"x = 'a\x80'"), r"\x80");
        // One error for `0b2`, not a second for the empty digit run.
        assert_eq!(diags("0b2").len(), 1);
        let (_, d) = Lexer::new(r#""\x80""#, "t").lex_all();
        assert_eq!(d[0].fixes.iter().next().unwrap().text, r"\u{80}");
    }

    /// Many errors in a large file: positions come from a line index,
    /// so this stays linear.
    #[test]
    fn many_errors_large_file() {
        let src = "x \u{2212} 1\n".repeat(50_000);
        let (_, d) = Lexer::new(&src, "t").lex_all();
        assert_eq!(d.len(), 50_000);
        let last = d[49_999].primary.as_ref().unwrap().span;
        assert_eq!((last.line, last.col), (50_000, 3));
        let src = format!("\"{}\"", "\u{200B}".repeat(100_000));
        assert_eq!(Lexer::new(&src, "t").lex_all().1.len(), 100_000);
    }

    #[test]
    fn ext_ops_longest_match() {
        let ops = |src: &str, ops: &[&str]| -> Vec<T> {
            let ops = ops.iter().map(|s| (*s).to_owned()).collect();
            let (toks, d) = Lexer::new(src, "t").with_ext_ops(ops).lex_all();
            assert!(d.is_empty(), "{src:?}: {d:?}");
            toks.into_iter().map(|t| t.kind).collect()
        };
        let x = || T::Ident("x".into());
        let op = |s: &str| T::ExtOp(s.into());
        assert_eq!(ops("x<-x", &["<-"]), [x(), op("<-"), x(), T::Eof]);
        assert_eq!(ops("x*^x", &["*^"]), [x(), op("*^"), x(), T::Eof]);
        // The fixed token is as long or longer: it stays.
        assert_eq!(ops("x<=x", &["<"]), [x(), T::LtEq, x(), T::Eof]);
        assert_eq!(ops("x<<=x", &["<<"]), [x(), T::ShlEq, x(), T::Eof]);
        // Longest registered token, whatever the order.
        assert_eq!(ops("x$$$x", &["$", "$$$"]), [x(), op("$$$"), x(), T::Eof]);
        assert_eq!(
            ops("x<-->x", &["<-", "<-->"]),
            [x(), op("<-->"), x(), T::Eof]
        );
        // No registration: `x < -x`.
        assert_eq!(ok("x<-x"), [x(), T::Lt, T::Minus, x(), T::Eof]);
    }
}
