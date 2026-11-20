//! Preprocessing tokens of C source: identifiers, pp-numbers, character
//! and string literals and punctuators, with line splices and comments
//! removed.

use nabic_core::HashMap;

/// An interned spelling.
pub type Sym = u32;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Ident,
    Num,
    Char,
    Str,
    Punct,
    /// A byte no other token starts with (`\`, stray `'`, ...).
    Other,
    /// `#pragma pack` state for the parser; `sym` indexes the pragmas.
    Pragma,
    Eof,
}

/// Whitespace came before the token.
pub const SPACE: u8 = 1;
/// The first token on its line.
pub const BOL: u8 = 2;
/// Never macro-expanded (it named a macro being expanded).
pub const NOEXP: u8 = 4;
/// The empty result of an argument or `##` (dropped after pasting).
pub const PLACEMARKER: u8 = 8;
/// On a `Pragma` token: `#pragma clang assume_nonnull begin` (sym 1) or
/// `end` (sym 0), not a packing.
pub const ASSUME_NONNULL: u8 = 16;

/// A source position: file index (0 for none), line and column, 1-based.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Loc {
    pub file: u32,
    pub line: u32,
    pub col: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct Tok {
    pub kind: Kind,
    pub flags: u8,
    pub sym: Sym,
    pub loc: Loc,
}

impl Tok {
    pub const fn has(&self, f: u8) -> bool {
        self.flags & f != 0
    }
}

/// Spellings by symbol, symbols by spelling. The first [`KEYWORDS`]
/// spellings get symbols `0..`.
pub struct Names {
    map: HashMap<Box<str>, Sym>,
    strs: Vec<Box<str>>,
}

/// Spellings interned first, in this order, so that their symbols are
/// constants (see `kw`).
pub const KEYWORDS: &[&str] = &[
    "auto",
    "break",
    "case",
    "char",
    "const",
    "continue",
    "default",
    "do",
    "double",
    "else",
    "enum",
    "extern",
    "float",
    "for",
    "goto",
    "if",
    "inline",
    "int",
    "long",
    "register",
    "restrict",
    "return",
    "short",
    "signed",
    "sizeof",
    "static",
    "struct",
    "switch",
    "typedef",
    "union",
    "unsigned",
    "void",
    "volatile",
    "while",
    "_Alignas",
    "_Alignof",
    "_Atomic",
    "_Bool",
    "_Complex",
    "_Generic",
    "_Imaginary",
    "_Noreturn",
    "_Static_assert",
    "_Thread_local",
    "__attribute__",
    "__attribute",
    "__asm__",
    "__asm",
    "asm",
    "__extension__",
    "__inline",
    "__inline__",
    "__restrict",
    "__restrict__",
    "__const",
    "__const__",
    "__volatile",
    "__volatile__",
    "__signed",
    "__signed__",
    "typeof",
    "__typeof",
    "__typeof__",
    "typeof_unqual",
    "__typeof_unqual__",
    "__alignof",
    "__alignof__",
    "__thread",
    "__int128",
    "_Float16",
    "__fp16",
    "__bf16",
    "__float128",
    "__ibm128",
    "__builtin_va_arg",
    "__builtin_offsetof",
    "__builtin_types_compatible_p",
    "__builtin_choose_expr",
    "__real__",
    "__imag__",
    "_Nonnull",
    "_Nullable",
    "_Null_unspecified",
    "_Nullable_result",
    "__cdecl",
    "__stdcall",
    "__fastcall",
    "__vectorcall",
    "__thiscall",
    "__regcall",
    "__auto_type",
    "_BitInt",
    "__declspec",
    "__forceinline",
    "__int64",
    "__int32",
    "__int16",
    "__int8",
    "__ptr32",
    "__ptr64",
    "__unaligned",
    "__w64",
    "__sptr",
    "__uptr",
    "_cdecl",
    "_stdcall",
    "_fastcall",
    "static_assert",
    "alignas",
    "alignof",
    "bool",
    "true",
    "false",
    "nullptr",
    "thread_local",
    "constexpr",
    "defined",
    "__VA_ARGS__",
    "__VA_OPT__",
    "__has_include",
    "__has_include_next",
    "_Pragma",
    "__pragma",
    "__builtin_va_list",
    "__builtin_ms_va_list",
    "__int128_t",
    "__uint128_t",
];

/// The symbols of [`KEYWORDS`] by name.
pub mod kw {
    use super::Sym;

    pub const AUTO: Sym = 0;
    pub const BREAK: Sym = 1;
    pub const CASE: Sym = 2;
    pub const CHAR: Sym = 3;
    pub const CONST: Sym = 4;
    pub const CONTINUE: Sym = 5;
    pub const DEFAULT: Sym = 6;
    pub const DO: Sym = 7;
    pub const DOUBLE: Sym = 8;
    pub const ELSE: Sym = 9;
    pub const ENUM: Sym = 10;
    pub const EXTERN: Sym = 11;
    pub const FLOAT: Sym = 12;
    pub const FOR: Sym = 13;
    pub const GOTO: Sym = 14;
    pub const IF: Sym = 15;
    pub const INLINE: Sym = 16;
    pub const INT: Sym = 17;
    pub const LONG: Sym = 18;
    pub const REGISTER: Sym = 19;
    pub const RESTRICT: Sym = 20;
    pub const RETURN: Sym = 21;
    pub const SHORT: Sym = 22;
    pub const SIGNED: Sym = 23;
    pub const SIZEOF: Sym = 24;
    pub const STATIC: Sym = 25;
    pub const STRUCT: Sym = 26;
    pub const SWITCH: Sym = 27;
    pub const TYPEDEF: Sym = 28;
    pub const UNION: Sym = 29;
    pub const UNSIGNED: Sym = 30;
    pub const VOID: Sym = 31;
    pub const VOLATILE: Sym = 32;
    pub const WHILE: Sym = 33;
    pub const ALIGNAS2: Sym = 34;
    pub const ALIGNOF2: Sym = 35;
    pub const ATOMIC: Sym = 36;
    pub const BOOL2: Sym = 37;
    pub const COMPLEX: Sym = 38;
    pub const GENERIC: Sym = 39;
    pub const IMAGINARY: Sym = 40;
    pub const NORETURN: Sym = 41;
    pub const STATIC_ASSERT2: Sym = 42;
    pub const THREAD_LOCAL2: Sym = 43;
    pub const ATTRIBUTE: Sym = 44;
    pub const ATTRIBUTE2: Sym = 45;
    pub const ASM: Sym = 46;
    pub const ASM2: Sym = 47;
    pub const ASM3: Sym = 48;
    pub const EXTENSION: Sym = 49;
    pub const INLINE2: Sym = 50;
    pub const INLINE3: Sym = 51;
    pub const RESTRICT2: Sym = 52;
    pub const RESTRICT3: Sym = 53;
    pub const CONST2: Sym = 54;
    pub const CONST3: Sym = 55;
    pub const VOLATILE2: Sym = 56;
    pub const VOLATILE3: Sym = 57;
    pub const SIGNED2: Sym = 58;
    pub const SIGNED3: Sym = 59;
    pub const TYPEOF: Sym = 60;
    pub const TYPEOF2: Sym = 61;
    pub const TYPEOF3: Sym = 62;
    pub const TYPEOF_UNQUAL: Sym = 63;
    pub const TYPEOF_UNQUAL2: Sym = 64;
    pub const ALIGNOF3: Sym = 65;
    pub const ALIGNOF4: Sym = 66;
    pub const THREAD: Sym = 67;
    pub const INT128: Sym = 68;
    pub const FLOAT16: Sym = 69;
    pub const FP16: Sym = 70;
    pub const BF16: Sym = 71;
    pub const FLOAT128: Sym = 72;
    pub const IBM128: Sym = 73;
    pub const VA_ARG: Sym = 74;
    pub const OFFSETOF: Sym = 75;
    pub const TYPES_COMPATIBLE: Sym = 76;
    pub const CHOOSE_EXPR: Sym = 77;
    pub const REAL: Sym = 78;
    pub const IMAG: Sym = 79;
    pub const NONNULL: Sym = 80;
    pub const NULLABLE: Sym = 81;
    pub const NULL_UNSPECIFIED: Sym = 82;
    pub const NULLABLE_RESULT: Sym = 83;
    pub const CDECL: Sym = 84;
    pub const STDCALL: Sym = 85;
    pub const FASTCALL: Sym = 86;
    pub const VECTORCALL: Sym = 87;
    pub const THISCALL: Sym = 88;
    pub const REGCALL: Sym = 89;
    pub const AUTO_TYPE: Sym = 90;
    pub const BITINT: Sym = 91;
    pub const DECLSPEC: Sym = 92;
    pub const FORCEINLINE: Sym = 93;
    pub const INT64: Sym = 94;
    pub const INT32: Sym = 95;
    pub const INT16: Sym = 96;
    pub const INT8: Sym = 97;
    pub const PTR32: Sym = 98;
    pub const PTR64: Sym = 99;
    pub const UNALIGNED: Sym = 100;
    pub const W64: Sym = 101;
    pub const SPTR: Sym = 102;
    pub const UPTR: Sym = 103;
    pub const CDECL2: Sym = 104;
    pub const STDCALL2: Sym = 105;
    pub const FASTCALL2: Sym = 106;
    pub const STATIC_ASSERT: Sym = 107;
    pub const ALIGNAS: Sym = 108;
    pub const ALIGNOF: Sym = 109;
    pub const BOOL: Sym = 110;
    pub const TRUE: Sym = 111;
    pub const FALSE: Sym = 112;
    pub const NULLPTR: Sym = 113;
    pub const THREAD_LOCAL: Sym = 114;
    pub const CONSTEXPR: Sym = 115;
    pub const DEFINED: Sym = 116;
    pub const VA_ARGS: Sym = 117;
    pub const VA_OPT: Sym = 118;
    pub const HAS_INCLUDE: Sym = 119;
    pub const HAS_INCLUDE_NEXT: Sym = 120;
    pub const PRAGMA_OP: Sym = 121;
    pub const MS_PRAGMA: Sym = 122;
    pub const BUILTIN_VA_LIST: Sym = 123;
    pub const BUILTIN_MS_VA_LIST: Sym = 124;
    pub const INT128_T: Sym = 125;
    pub const UINT128_T: Sym = 126;
    /// Symbols up to this one are keywords on every target.
    pub const LAST_KEYWORD: Sym = BITINT;
    /// Up to this one, keywords on MSVC targets.
    pub const LAST_MS_KEYWORD: Sym = FASTCALL2;
}

impl Default for Names {
    fn default() -> Self {
        let mut n = Self {
            map: HashMap::default(),
            strs: Vec::new(),
        };
        for k in KEYWORDS {
            n.intern(k);
        }
        n
    }
}

impl Names {
    pub fn intern(&mut self, s: &str) -> Sym {
        if let Some(&y) = self.map.get(s) {
            return y;
        }
        let y = self.strs.len() as Sym;
        self.strs.push(s.into());
        self.map.insert(s.into(), y);
        y
    }

    pub fn get(&self, s: Sym) -> &str {
        &self.strs[s as usize]
    }

    pub fn lookup(&self, s: &str) -> Option<Sym> {
        self.map.get(s).copied()
    }
}

/// Is `s` a keyword of C as clang lexes it (gnu17, with the GNU and Clang
/// extensions, and Microsoft's on MSVC targets)?
pub const fn is_keyword(s: Sym, msvc: bool) -> bool {
    s <= kw::LAST_KEYWORD || (msvc && s <= kw::LAST_MS_KEYWORD)
}

const PUNCTS: &[&str] = &[
    "%:%:", "...", "<<=", ">>=", "->", "++", "--", "<<", ">>", "<=", ">=", "==", "!=", "&&", "||",
    "*=", "/=", "%=", "+=", "-=", "&=", "^=", "|=", "##", "<:", ":>", "<%", "%>", "%:", "::", "[",
    "]", "(", ")", "{", "}", ".", "&", "*", "+", "-", "~", "!", "/", "%", "<", ">", "^", "|", "?",
    ":", ";", "=", ",", "#",
];

/// The tokens of one file.
pub struct Lexer {
    src: std::rc::Rc<[u8]>,
    pos: usize,
    line: u32,
    line_start: usize,
    file: u32,
    bol: bool,
    buf: String,
}

const fn ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$' || b >= 0x80
}

impl Lexer {
    pub fn new(file: u32, src: std::rc::Rc<[u8]>) -> Self {
        // A UTF-8 byte order mark is not text.
        let pos = if src.starts_with(&[0xef, 0xbb, 0xbf]) {
            3
        } else {
            0
        };
        Self {
            src,
            pos,
            line: 1,
            line_start: pos,
            file,
            bol: true,
            buf: String::new(),
        }
    }

    /// Skip line splices at `pos`.
    fn splice(&mut self) {
        loop {
            let s = &self.src[self.pos..];
            let n = match s {
                [b'\\', b'\n', ..] => 2,
                [b'\\', b'\r', b'\n', ..] => 3,
                _ => return,
            };
            self.pos += n;
            self.line += 1;
            self.line_start = self.pos;
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.splice();
        self.src.get(self.pos).copied()
    }

    /// The byte after the current one (splices skipped).
    fn peek2(&mut self) -> Option<u8> {
        self.splice();
        let mut p = self.pos + 1;
        loop {
            match &self.src[p.min(self.src.len())..] {
                [b'\\', b'\n', ..] => p += 2,
                [b'\\', b'\r', b'\n', ..] => p += 3,
                [b, ..] => return Some(*b),
                [] => return None,
            }
        }
    }

    fn bump(&mut self) -> u8 {
        let b = self.src[self.pos];
        self.pos += 1;
        if b == b'\n' {
            self.line += 1;
            self.line_start = self.pos;
        }
        b
    }

    const fn loc(&self) -> Loc {
        Loc {
            file: self.file,
            line: self.line,
            col: (self.pos - self.line_start + 1) as u32,
        }
    }

    pub const fn file(&self) -> u32 {
        self.file
    }

    /// Skip whitespace and comments; true if any.
    fn space(&mut self) -> bool {
        let mut any = false;
        while let Some(b) = self.peek() {
            match b {
                b'\n' => {
                    self.bump();
                    self.bol = true;
                    any = false;
                }
                b' ' | b'\t' | b'\r' | 0x0b | 0x0c => {
                    self.bump();
                    any = true;
                }
                b'/' if self.peek2() == Some(b'*') => {
                    self.bump();
                    self.bump();
                    loop {
                        match self.peek() {
                            None => break,
                            Some(b'*') if self.peek2() == Some(b'/') => {
                                self.bump();
                                self.bump();
                                break;
                            }
                            Some(_) => {
                                self.bump();
                            }
                        }
                    }
                    any = true;
                }
                b'/' if self.peek2() == Some(b'/') => {
                    while let Some(b) = self.peek() {
                        if b == b'\n' {
                            break;
                        }
                        self.bump();
                    }
                    any = true;
                }
                _ => break,
            }
        }
        any
    }

    /// The next token if it is on the current line.
    pub fn next_in_line(&mut self, names: &mut Names) -> Option<Tok> {
        let space = self.space();
        if self.bol || self.pos >= self.src.len() {
            return None;
        }
        let mut t = self.next(names);
        if space {
            t.flags |= SPACE;
        }
        Some(t)
    }

    /// The rest of the line as text (`#error`, `#warning`), raw.
    pub fn rest_of_line(&mut self) -> String {
        let mut s = String::new();
        while let Some(b) = self.peek() {
            if b == b'\n' {
                break;
            }
            s.push(self.bump() as char);
        }
        s.trim().to_owned()
    }

    /// Skip to the end of the line (a skipped group's text).
    pub fn skip_line(&mut self) {
        while let Some(b) = self.peek() {
            match b {
                b'\n' => break,
                b'/' if matches!(self.peek2(), Some(b'*' | b'/')) => {
                    // A block comment can span lines; `space` handles it.
                    self.space();
                    if self.bol {
                        return;
                    }
                }
                b'"' | b'\'' => {
                    // Literals may hold `/*`; unterminated ones end at the line.
                    let q = self.bump();
                    while let Some(c) = self.peek() {
                        if c == b'\n' {
                            break;
                        }
                        self.bump();
                        if c == b'\\' {
                            if self.peek().is_some_and(|c| c != b'\n') {
                                self.bump();
                            }
                        } else if c == q {
                            break;
                        }
                    }
                }
                _ => {
                    self.bump();
                }
            }
        }
    }

    /// A header name after `#include`: `<...>` or `"..."` as written, if
    /// the line has one.
    pub fn header_name(&mut self) -> Option<(String, Loc)> {
        self.space();
        if self.bol {
            return None;
        }
        let loc = self.loc();
        let close = match self.peek()? {
            b'<' => b'>',
            b'"' => b'"',
            _ => return None,
        };
        let save = (self.pos, self.line, self.line_start);
        let open = self.bump();
        let mut s = String::from(open as char);
        while let Some(b) = self.peek() {
            if b == b'\n' {
                break;
            }
            self.bump();
            s.push(b as char);
            if b == close {
                return Some((s, loc));
            }
        }
        (self.pos, self.line, self.line_start) = save;
        None
    }

    pub fn next(&mut self, names: &mut Names) -> Tok {
        let space = self.space();
        let mut flags = if space { SPACE } else { 0 };
        if self.bol {
            flags |= BOL;
            self.bol = false;
        }
        let loc = self.loc();
        let tok = |kind, sym| Tok {
            kind,
            flags,
            sym,
            loc,
        };
        let Some(b) = self.peek() else {
            return tok(Kind::Eof, 0);
        };
        self.buf.clear();
        // Literal prefixes: L, u, U, u8.
        if matches!(b, b'L' | b'u' | b'U') {
            let save = (self.pos, self.line, self.line_start);
            let mut pre = String::new();
            pre.push(self.bump() as char);
            if b == b'u' && self.peek() == Some(b'8') {
                pre.push(self.bump() as char);
            }
            if let Some(q @ (b'"' | b'\'')) = self.peek() {
                self.buf.push_str(&pre);
                return self.quoted(q, names, flags, loc);
            }
            (self.pos, self.line, self.line_start) = save;
        }
        if ident_byte(b) && !b.is_ascii_digit() {
            while let Some(c) = self.peek() {
                if !ident_byte(c) {
                    break;
                }
                let c = self.bump();
                self.buf.push(c as char);
            }
            let s = if self.buf.is_ascii() {
                names.intern(&self.buf)
            } else {
                let raw: Vec<u8> = self.buf.chars().map(|c| c as u8).collect();
                names.intern(&String::from_utf8_lossy(&raw))
            };
            return tok(Kind::Ident, s);
        }
        if b.is_ascii_digit() || (b == b'.' && self.peek2().is_some_and(|c| c.is_ascii_digit())) {
            let mut prev = 0u8;
            while let Some(c) = self.peek() {
                let sign = matches!(c, b'+' | b'-') && matches!(prev, b'e' | b'E' | b'p' | b'P');
                if !(ident_byte(c) || c == b'.' || sign || c == b'\'') {
                    break;
                }
                if c == b'\'' {
                    // C23 digit separator.
                    self.bump();
                    prev = c;
                    continue;
                }
                prev = self.bump();
                self.buf.push(prev as char);
            }
            return tok(Kind::Num, names.intern(&self.buf));
        }
        if b == b'"' || b == b'\'' {
            return self.quoted(b, names, flags, loc);
        }
        for p in PUNCTS {
            if p.as_bytes()[0] == b && self.matches(p) {
                for _ in 0..p.len() {
                    self.peek();
                    self.bump();
                }
                let canon = match *p {
                    "<:" => "[",
                    ":>" => "]",
                    "<%" => "{",
                    "%>" => "}",
                    "%:" => "#",
                    "%:%:" => "##",
                    q => q,
                };
                return tok(Kind::Punct, names.intern(canon));
            }
        }
        let c = self.bump();
        let s = String::from_utf8_lossy(&[c]).into_owned();
        tok(Kind::Other, names.intern(&s))
    }

    fn matches(&mut self, p: &str) -> bool {
        self.splice();
        let save = (self.pos, self.line, self.line_start);
        let mut ok = true;
        for &want in p.as_bytes() {
            if self.peek() != Some(want) {
                ok = false;
                break;
            }
            self.pos += 1;
        }
        (self.pos, self.line, self.line_start) = save;
        ok
    }

    /// A character or string literal from its quote; `buf` holds any prefix.
    fn quoted(&mut self, q: u8, names: &mut Names, flags: u8, loc: Loc) -> Tok {
        let mut raw = std::mem::take(&mut self.buf).into_bytes();
        raw.push(self.bump());
        let mut closed = false;
        while let Some(c) = self.peek() {
            if c == b'\n' {
                break;
            }
            raw.push(self.bump());
            if c == b'\\' {
                if let Some(d) = self.peek().filter(|&d| d != b'\n') {
                    raw.push(d);
                    self.bump();
                }
            } else if c == q {
                closed = true;
                break;
            }
        }
        let kind = if !closed {
            Kind::Other
        } else if q == b'"' {
            Kind::Str
        } else {
            Kind::Char
        };
        let s = String::from_utf8_lossy(&raw).into_owned();
        Tok {
            kind,
            flags,
            sym: names.intern(&s),
            loc,
        }
    }
}

/// Re-lex a spelling made by `##` or stringizing: its tokens.
pub fn relex(text: &str, names: &mut Names, loc: Loc) -> Vec<Tok> {
    let mut lx = Lexer::new(loc.file, text.as_bytes().into());
    let mut out = Vec::new();
    loop {
        let mut t = lx.next(names);
        if t.kind == Kind::Eof {
            return out;
        }
        t.loc = loc;
        t.flags &= !BOL;
        out.push(t);
    }
}
