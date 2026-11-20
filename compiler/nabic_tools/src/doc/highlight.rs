//! Nabi source as HTML token spans, the classes nabi-syntax gives the
//! sites (`k` keyword, `t` type, `s` string, `n` number, `c` comment,
//! `a` attribute, `m` macro; other text bare), so the pages share one
//! stylesheet. A port of nabi-syntax's `highlight_nabi`, names and all.
#![allow(clippy::many_single_char_names, clippy::cognitive_complexity)]

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Keyword,
    Type,
    Str,
    Number,
    Comment,
    Attribute,
    Text,
    Macro,
}

impl Kind {
    const fn class(self) -> &'static str {
        match self {
            Self::Keyword => "k",
            Self::Type => "t",
            Self::Str => "s",
            Self::Number => "n",
            Self::Comment => "c",
            Self::Attribute => "a",
            Self::Text => "",
            Self::Macro => "m",
        }
    }
}

const RESERVED: &[&str] = &[
    "let", "fn", "if", "else", "match", "while", "loop", "for", "return", "break", "continue",
    "try", "throw", "async", "await", "yield", "unsafe", "using", "struct", "enum", "trait",
    "impl", "pub", "mod", "use", "as", "is", "ref", "true", "false", "none", "self", "Self",
    "macro", "defer", "dyn", "move", "gen", "do",
];

const CONTEXTUAL: &[&str] = &[
    "type", "any", "in", "where", "throws", "rethrows", "catch", "finally", "const", "static",
    "extern", "mut", "union",
];

// Keywords that stay keywords before `:`.
const VALUES: &[&str] = &["true", "false", "none", "self", "Self"];

const TYPES: &[&str] = &[
    "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize", "f32",
    "f64", "bool", "char", "str", "int", "float", "c_char",
];

/// `src` as HTML: tokens in `<span class="..">`, `&` `<` `>` escaped.
pub(crate) fn nabi(src: &str) -> String {
    let mut lx = Lexer {
        cs: src.chars().collect(),
        toks: Vec::new(),
        text: String::new(),
    };
    let start = lx.shebang();
    lx.code(start, false);
    lx.flush();
    let mut out = String::new();
    for (kind, text) in lx.toks {
        if kind == Kind::Text {
            push_esc(&mut out, &text);
        } else {
            out.push_str("<span class=\"");
            out.push_str(kind.class());
            out.push_str("\">");
            push_esc(&mut out, &text);
            out.push_str("</span>");
        }
    }
    out
}

/// `&`, `<`, `>` escaped (quotes stay, as nabi-syntax writes them).
fn push_esc(out: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
    }
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn is_ident_start(c: char) -> bool {
    is_ident(c) && !c.is_ascii_digit()
}

const fn is_space(c: char) -> bool {
    c == ' ' || c == '\t'
}

fn is_type(w: &str) -> bool {
    TYPES.contains(&w) || w.chars().next().is_some_and(char::is_uppercase)
}

fn is_radix_digit(c: char, radix: char) -> bool {
    match radix {
        _ if c == '_' => true,
        'x' => c.is_ascii_hexdigit(),
        'b' => c == '0' || c == '1',
        _ => ('0'..='7').contains(&c),
    }
}

struct Lexer {
    cs: Vec<char>,
    toks: Vec<(Kind, String)>,
    /// Pending text, merged until the next other token.
    text: String,
}

impl Lexer {
    fn flush(&mut self) {
        if !self.text.is_empty() {
            self.toks.push((Kind::Text, std::mem::take(&mut self.text)));
        }
    }

    fn emit(&mut self, kind: Kind, a: usize, b: usize) {
        if a >= b {
            return;
        }
        let s: String = self.cs[a..b].iter().collect();
        if kind == Kind::Text {
            self.text.push_str(&s);
            return;
        }
        self.flush();
        self.toks.push((kind, s));
    }

    fn at(&self, k: usize) -> char {
        self.cs.get(k).copied().unwrap_or('\0')
    }

    fn skip_space(&self, k: usize) -> usize {
        let mut j = k;
        while j < self.cs.len() && is_space(self.cs[j]) {
            j += 1;
        }
        j
    }

    fn ident_end(&self, k: usize) -> usize {
        let mut j = k;
        if j < self.cs.len() && is_ident_start(self.cs[j]) {
            while j < self.cs.len() && is_ident(self.cs[j]) {
                j += 1;
            }
        }
        j
    }

    fn shebang(&mut self) -> usize {
        if self.at(0) != '#' || self.at(1) != '!' || self.at(self.skip_space(2)) == '[' {
            return 0;
        }
        let mut k = 0;
        while k < self.cs.len() && self.cs[k] != '\n' {
            k += 1;
        }
        self.emit(Kind::Comment, 0, k);
        k
    }

    fn quote_end(&self, i: usize, q: char) -> usize {
        let mut k = i + 1;
        while k < self.cs.len() {
            let c = self.cs[k];
            if c == '\\' {
                k += 2;
                continue;
            }
            if c == q {
                return k + 1;
            }
            if c == '\n' && q == '\'' {
                return k;
            }
            k += 1;
        }
        self.cs.len()
    }

    /// End of the char literal at `i`, or 0 when `'` opens none.
    fn char_end(&self, i: usize) -> usize {
        let a = self.at(i + 1);
        if a == '\\' {
            let k = self.quote_end(i, '\'');
            return if k > i + 2 && self.at(k - 1) == '\'' {
                k
            } else {
                0
            };
        }
        if a != '\'' && a != '\n' && a != '\0' && self.at(i + 2) == '\'' {
            return i + 3;
        }
        0
    }

    fn raw_end(&self, i: usize) -> usize {
        let n = self.cs.len();
        let mut k = i + 1;
        while k < n && self.cs[k] == '#' {
            k += 1;
        }
        let hashes = k - i - 1;
        if k >= n || self.cs[k] != '"' {
            return k;
        }
        k += 1;
        while k < n {
            if self.cs[k] == '"' {
                let mut h = 0;
                while h < hashes && k + 1 + h < n && self.cs[k + 1 + h] == '#' {
                    h += 1;
                }
                if h == hashes {
                    return k + 1 + h;
                }
            }
            k += 1;
        }
        n
    }

    fn block_comment_end(&self, i: usize) -> usize {
        let n = self.cs.len();
        let mut k = i + 2;
        let mut depth = 1;
        while k < n {
            if self.cs[k] == '/' && self.at(k + 1) == '*' {
                depth += 1;
                k += 2;
            } else if self.cs[k] == '*' && self.at(k + 1) == '/' {
                depth -= 1;
                k += 2;
                if depth == 0 {
                    return k;
                }
            } else {
                k += 1;
            }
        }
        n
    }

    fn attribute_end(&self, i: usize) -> usize {
        let n = self.cs.len();
        let mut k = if self.at(i + 1) == '!' { i + 3 } else { i + 2 };
        let mut depth = 1;
        while k < n {
            let c = self.cs[k];
            if c == '"' {
                k = self.quote_end(k, '"');
            } else if c == '\'' && self.char_end(k) > 0 {
                k = self.char_end(k);
            } else if c == 'r'
                && (self.at(k + 1) == '"' || self.at(k + 1) == '#')
                && !(k > 0 && is_ident(self.cs[k - 1]))
            {
                k = self.raw_end(k);
            } else {
                if c == '[' {
                    depth += 1;
                }
                if c == ']' {
                    depth -= 1;
                    if depth == 0 {
                        return k + 1;
                    }
                }
                k += 1;
            }
        }
        n
    }

    fn digits(&self, mut k: usize) -> usize {
        while k < self.cs.len() && (self.cs[k].is_ascii_digit() || self.cs[k] == '_') {
            k += 1;
        }
        k
    }

    fn number_end(&self, i: usize) -> usize {
        let p = self.at(i + 1).to_ascii_lowercase();
        if self.cs[i] == '0' && matches!(p, 'x' | 'b' | 'o') && is_radix_digit(self.at(i + 2), p) {
            let mut k = i + 2;
            while k < self.cs.len() && is_radix_digit(self.cs[k], p) {
                k += 1;
            }
            return self.ident_end(k);
        }
        let mut k = self.digits(i);
        if self.at(k) == '.' && self.at(k + 1) != '.' && !is_ident_start(self.at(k + 1)) {
            k += 1;
            if self.at(k).is_ascii_digit() {
                k = self.digits(k);
            }
        }
        if matches!(self.at(k), 'e' | 'E') {
            let d = if matches!(self.at(k + 1), '+' | '-') {
                k + 2
            } else {
                k + 1
            };
            if self.at(d).is_ascii_digit() {
                k = self.digits(d);
            }
        }
        self.ident_end(k)
    }

    fn fstr(&mut self, i: usize) -> usize {
        let n = self.cs.len();
        let triple = i + 3 < n && self.cs[i + 2] == '"' && self.cs[i + 3] == '"';
        let mut k = if triple { i + 4 } else { i + 2 };
        let mut run = i;
        while k < n {
            let c = self.cs[k];
            let nx = self.at(k + 1);
            if c == '\\' || ((c == '{' || c == '}') && nx == c) {
                k += 2;
            } else if c == '"' && (!triple || (nx == '"' && self.at(k + 2) == '"')) {
                let e = if triple { k + 3 } else { k + 1 };
                self.emit(Kind::Str, run, e);
                return e;
            } else if c == '{' {
                self.emit(Kind::Str, run, k + 1);
                k = self.hole(k + 1);
                run = k;
                if k < n {
                    k += 1;
                }
            } else {
                k += 1;
            }
        }
        self.emit(Kind::Str, run, n);
        n
    }

    fn hole(&mut self, i: usize) -> usize {
        let n = self.cs.len();
        let mut k = self.code(i, true);
        if k >= n || self.cs[k] != ':' {
            return k;
        }
        let mut run = k;
        while k < n && self.cs[k] != '}' && self.cs[k] != '"' {
            let nx = self.at(k + 1);
            if self.cs[k] == '$' && nx == '$' {
                k += 2;
            } else if self.cs[k] == '{' || (self.cs[k] == '$' && nx == '{') {
                let open = if self.cs[k] == '$' { k + 2 } else { k + 1 };
                self.emit(Kind::Str, run, open);
                k = self.hole(open);
                run = k;
                if k < n {
                    k += 1;
                }
            } else {
                k += 1;
            }
        }
        self.emit(Kind::Str, run, k);
        k
    }

    fn after_dot(&self, i: usize) -> bool {
        i > 0 && self.cs[i - 1] == '.' && !(i > 1 && self.cs[i - 2] == '.')
    }

    /// Whether the word at `i` follows the word `w` and a space.
    fn after_word(&self, i: usize, w: &str) -> bool {
        let mut j = i;
        while j > 0 && is_space(self.cs[j - 1]) {
            j -= 1;
        }
        let wl = w.chars().count();
        if j == i || j < wl {
            return false;
        }
        let p = j - wl;
        w.chars().enumerate().all(|(m, c)| self.cs[p + m] == c)
            && !(p > 0 && is_ident(self.cs[p - 1]))
    }

    fn after_return(&self, i: usize) -> bool {
        i >= 6
            && "return"
                .chars()
                .enumerate()
                .all(|(m, c)| self.cs[i - 6 + m] == c)
            && !(i > 6 && is_ident(self.cs[i - 7]))
    }

    fn after_pub_paren(&self, i: usize) -> bool {
        let mut j = i;
        while j > 0 && is_space(self.cs[j - 1]) {
            j -= 1;
        }
        if j == 0 || self.cs[j - 1] != '(' {
            return false;
        }
        j -= 1;
        while j > 0 && is_space(self.cs[j - 1]) {
            j -= 1;
        }
        j >= 3 && self.cs[j - 3..j] == ['p', 'u', 'b'] && !(j > 3 && is_ident(self.cs[j - 4]))
    }

    fn colon_ahead(&self, k: usize) -> bool {
        let j = self.skip_space(k);
        self.at(j) == ':' && self.at(j + 1) != ':'
    }

    fn value_end_ahead(&self, k: usize) -> bool {
        let j = self.skip_space(k);
        let (c, d) = (self.at(j), self.at(j + 1));
        (c == '=' && d != '=' && d != '>')
            || c == ','
            || c == ')'
            || c == ']'
            || (c == '.' && d != '.')
    }

    fn alias_ahead(&self, k: usize) -> bool {
        let mut j = self.skip_space(k);
        if j == k {
            return false;
        }
        if self.at(j) == 'r' && self.at(j + 1) == '#' {
            j += 2;
        }
        let e = self.ident_end(j);
        e != j && matches!(self.at(self.skip_space(e)), '=' | '<' | ';' | ':')
    }

    fn some_type_at(&self, i: usize, k: usize) -> bool {
        let mut j = i;
        while j > 0 && is_space(self.cs[j - 1]) {
            j -= 1;
        }
        let p = if j > 0 { self.cs[j - 1] } else { ' ' };
        let after = (p == ':' && !(j > 1 && self.cs[j - 2] == ':'))
            || (p == '>' && j > 1 && self.cs[j - 2] == '-');
        let n = self.skip_space(k);
        let c = self.at(n);
        after && n > k && (c == '(' || is_ident_start(c))
    }

    fn word_kind(&self, w: &str, i: usize, k: usize) -> Kind {
        let reserved = RESERVED.contains(&w);
        let contextual = CONTEXTUAL.contains(&w);
        if (reserved || contextual) && !VALUES.contains(&w) && self.colon_ahead(k) {
            return Kind::Text;
        }
        if contextual && self.value_end_ahead(k) {
            return Kind::Text;
        }
        match w {
            "type" if !self.alias_ahead(k) => return Kind::Text,
            "some" => {
                return if self.some_type_at(i, k) {
                    Kind::Keyword
                } else {
                    Kind::Text
                };
            }
            "union" => {
                let j = self.skip_space(k);
                let c = self.at(j);
                let raw = c == 'r' && self.at(j + 1) == '#';
                return if j > k && (is_ident_start(c) || raw) {
                    Kind::Keyword
                } else {
                    Kind::Text
                };
            }
            "pkg" | "sup" if self.at(k) == ':' && self.at(k + 1) == ':' => {
                return Kind::Keyword;
            }
            "pkg" | "sub" if self.after_pub_paren(i) && self.at(self.skip_space(k)) == ')' => {
                return Kind::Keyword;
            }
            _ => {}
        }
        if reserved || contextual {
            Kind::Keyword
        } else if is_type(w) {
            Kind::Type
        } else {
            Kind::Text
        }
    }

    fn word(&mut self, i: usize, k: usize) -> usize {
        let w: String = self.cs[i..k].iter().collect();
        if self.after_dot(i) || self.after_word(i, "fn") || self.after_word(i, "mod") {
            self.emit(Kind::Text, i, k);
            return k;
        }
        if self.at(k) == '!' && self.at(k + 1) != '=' && !RESERVED.contains(&w.as_str()) {
            self.emit(Kind::Macro, i, k + 1);
            return k + 1;
        }
        let kind = self.word_kind(&w, i, k);
        self.emit(kind, i, k);
        k
    }

    /// Code from `start`; in a hole, up to its `}` or spec `:`.
    fn code(&mut self, start: usize, in_hole: bool) -> usize {
        let n = self.cs.len();
        let mut i = start;
        let mut depth = 0i32;
        while i < n {
            let c = self.cs[i];
            let nx = self.at(i + 1);
            if in_hole && depth == 0 && (c == '}' || (c == ':' && nx != ':')) {
                return i;
            }
            if matches!(c, '(' | '[' | '{') {
                depth += 1;
            }
            if matches!(c, ')' | ']' | '}') {
                depth -= 1;
            }
            if c == ':' && nx == ':' {
                self.text.push_str("::");
                i += 2;
            } else if c == '/' && nx == '/' {
                let mut k = i;
                while k < n && self.cs[k] != '\n' {
                    k += 1;
                }
                self.emit(Kind::Comment, i, k);
                i = k;
            } else if c == '/' && nx == '*' {
                let k = self.block_comment_end(i);
                self.emit(Kind::Comment, i, k);
                i = k;
            } else if c == '"' {
                let k = self.quote_end(i, '"');
                self.emit(Kind::Str, i, k);
                i = k;
            } else if c == '\'' {
                let k = self.char_end(i);
                if k > 0 {
                    self.emit(Kind::Str, i, k);
                    i = k;
                } else if is_ident_start(nx) {
                    let e = self.ident_end(i + 1);
                    self.emit(Kind::Text, i, e);
                    i = e;
                } else {
                    self.text.push(c);
                    i += 1;
                }
            } else if c == '#' && (nx == '[' || (nx == '!' && self.at(i + 2) == '[')) {
                let k = self.attribute_end(i);
                self.emit(Kind::Attribute, i, k);
                i = k;
            } else if c == '!' && nx == 'i' && self.at(i + 2) == 'n' && !is_ident(self.at(i + 3)) {
                self.emit(Kind::Keyword, i, i + 3);
                i += 3;
            } else if c == '@' && is_ident_start(nx) {
                let k = self.ident_end(i + 1);
                let kind = if self.after_return(i) {
                    Kind::Text
                } else {
                    Kind::Macro
                };
                self.emit(kind, i, k);
                i = k;
            } else if c.is_ascii_digit() {
                let k = self.number_end(i);
                self.emit(Kind::Number, i, k);
                i = k;
            } else if is_ident(c) {
                let k = self.ident_end(i);
                let one = k == i + 1;
                if one && c == 'f' && self.at(k) == '"' {
                    i = self.fstr(i);
                } else if one && c == 'r' && self.at(k) == '#' && is_ident_start(self.at(k + 1)) {
                    let e = self.ident_end(k + 1);
                    if !self.after_dot(i) && self.at(e) == '!' && self.at(e + 1) != '=' {
                        self.emit(Kind::Macro, i, e + 1);
                        i = e + 1;
                    } else {
                        self.emit(Kind::Text, i, e);
                        i = e;
                    }
                } else if one && c == 'r' && (self.at(k) == '"' || self.at(k) == '#') {
                    let e = self.raw_end(i);
                    self.emit(Kind::Str, i, e);
                    i = e;
                } else if one && c == 'b' && self.at(k) == '"' {
                    let e = self.quote_end(k, '"');
                    self.emit(Kind::Str, i, e);
                    i = e;
                } else if one
                    && (c == 'c' || c == 'b')
                    && self.at(k) == '\''
                    && self.char_end(k) > 0
                {
                    let e = self.char_end(k);
                    self.emit(Kind::Str, i, e);
                    i = e;
                } else {
                    i = self.word(i, k);
                }
            } else {
                self.text.push(c);
                i += 1;
            }
        }
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The classes nabi-syntax gives the same source (the sites' API
    /// pages show these).
    #[test]
    #[allow(clippy::literal_string_with_formatting_args)]
    fn as_nabi_syntax() {
        assert_eq!(
            nabi("pub struct BufReader<R>"),
            "<span class=\"k\">pub</span> <span class=\"k\">struct</span> \
             <span class=\"t\">BufReader</span>&lt;<span class=\"t\">R</span>&gt;"
        );
        assert_eq!(
            nabi("fn f(self) -> usize"),
            "<span class=\"k\">fn</span> f(<span class=\"k\">self</span>) -&gt; \
             <span class=\"t\">usize</span>"
        );
        assert_eq!(
            nabi("let s = f\"{x:>4}\"; // hi"),
            "<span class=\"k\">let</span> s = <span class=\"s\">f\"{</span>x\
             <span class=\"s\">:&gt;4</span><span class=\"s\">}\"</span>; <span class=\"c\">// hi</span>"
        );
        assert_eq!(
            nabi("#[inline] type T = u8; vec![1.5]"),
            "<span class=\"a\">#[inline]</span> <span class=\"k\">type</span> \
             <span class=\"t\">T</span> = <span class=\"t\">u8</span>; \
             <span class=\"m\">vec!</span>[<span class=\"n\">1.5</span>]"
        );
    }
}
