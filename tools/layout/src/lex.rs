//! A comment- and string-aware token scanner for Rust and Nabi.
//!
//! Not a full lexer: it keeps identifiers, `::` and single punctuation,
//! folds literals into `STR` / `CHR` / `NUM`, turns doc comments into
//! `#doc` / `#!doc` tokens and records which lines hold code and which
//! hold plain comments only.

/// One token and the line it starts on (1-based).
#[derive(Clone, Debug)]
pub struct Tok {
    pub text: String,
    pub line: usize,
}

pub struct Lexed {
    pub toks: Vec<Tok>,
    /// `code[l]`: line `l` holds a token (doc comments count).
    pub code: Vec<bool>,
    /// `comment[l]`: line `l` holds a plain comment.
    pub comment: Vec<bool>,
    pub err: Option<String>,
}

/// String prefixes: `r"..."`, `b"..."`, `f"..."` (Nabi), `c"..."`, ...
const PREFIXES: &[&str] = &["r", "b", "br", "rb", "f", "c", "fr", "rf", "cr", "rc", "bf"];

struct Lexer {
    s: Vec<char>,
    i: usize,
    line: usize,
    out: Lexed,
}

fn ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || !c.is_ascii()
}

impl Lexer {
    fn at(&self, k: usize) -> Option<char> {
        self.s.get(k).copied()
    }

    fn starts(&self, k: usize, pat: &str) -> bool {
        pat.chars()
            .enumerate()
            .all(|(n, c)| self.at(k + n) == Some(c))
    }

    fn mark(v: &mut Vec<bool>, l: usize) {
        if v.len() <= l {
            v.resize(l + 1, false);
        }
        v[l] = true;
    }

    fn push(&mut self, text: &str, line: usize) {
        Self::mark(&mut self.out.code, line);
        self.out.toks.push(Tok {
            text: text.to_owned(),
            line,
        });
    }

    fn run(mut self) -> Lexed {
        while let Some(c) = self.at(self.i) {
            if c == '\n' {
                self.line += 1;
                self.i += 1;
            } else if c.is_whitespace() {
                self.i += 1;
            } else if self.starts(self.i, "//") {
                self.line_comment();
            } else if self.starts(self.i, "/*") {
                self.block_comment();
            } else if c.is_alphabetic() || c == '_' || !c.is_ascii() {
                self.word();
            } else if c.is_ascii_digit() {
                let mut j = self.i;
                while let Some(d) = self.at(j) {
                    let frac = d == '.' && self.at(j + 1).is_some_and(|e| e.is_ascii_digit());
                    if !(d.is_alphanumeric() || d == '_' || frac) {
                        break;
                    }
                    j += 1;
                }
                self.push("NUM", self.line);
                self.i = j;
            } else if c == '"' {
                self.string(self.i, false, false);
            } else if c == '\'' {
                let quoted = self.at(self.i + 1) == Some('\\') || self.at(self.i + 2) == Some('\'');
                if quoted {
                    self.char_lit(self.i);
                } else {
                    // lifetime or label
                    self.i += 1;
                }
            } else if self.starts(self.i, "::") {
                self.push("::", self.line);
                self.i += 2;
            } else {
                self.push(&c.to_string(), self.line);
                self.i += 1;
            }
        }
        self.out
    }

    fn line_comment(&mut self) {
        let mut j = self.i;
        while self.at(j).is_some_and(|c| c != '\n') {
            j += 1;
        }
        if self.starts(self.i, "//!") {
            self.push("#!doc", self.line);
        } else if self.starts(self.i, "///") && !self.starts(self.i, "////") {
            self.push("#doc", self.line);
        } else {
            Self::mark(&mut self.out.comment, self.line);
        }
        self.i = j;
    }

    fn block_comment(&mut self) {
        let l0 = self.line;
        let kind = if self.starts(self.i, "/**")
            && !self.starts(self.i, "/**/")
            && !self.starts(self.i, "/***")
        {
            Some("#doc")
        } else if self.starts(self.i, "/*!") {
            Some("#!doc")
        } else {
            None
        };
        let mut depth = 0;
        let mut j = self.i;
        while j < self.s.len() {
            if self.starts(j, "/*") {
                depth += 1;
                j += 2;
            } else if self.starts(j, "*/") {
                depth -= 1;
                j += 2;
                if depth == 0 {
                    break;
                }
            } else {
                if self.s[j] == '\n' {
                    self.line += 1;
                }
                j += 1;
            }
        }
        for l in l0..=self.line {
            if kind.is_some() {
                Self::mark(&mut self.out.code, l);
            } else {
                Self::mark(&mut self.out.comment, l);
            }
        }
        if let Some(k) = kind {
            self.out.toks.push(Tok {
                text: k.to_owned(),
                line: l0,
            });
        }
        self.i = j;
    }

    fn word(&mut self) {
        let mut j = self.i;
        while self.at(j).is_some_and(ident_char) {
            j += 1;
        }
        let word: String = self.s[self.i..j].iter().collect();
        let next = self.at(j);
        if PREFIXES.contains(&word.as_str()) {
            let raw = word.contains('r');
            let hashed = next == Some('#') && raw && matches!(self.at(j + 1), Some('#' | '"'));
            if next == Some('"') || hashed {
                self.string(j, raw, word.contains('f'));
                return;
            }
        }
        if next == Some('\'') && (word == "b" || word == "c") {
            self.char_lit(j);
            return;
        }
        if word == "r" && next == Some('#') {
            // raw identifier
            let mut k = j + 1;
            while self.at(k).is_some_and(|c| c.is_alphanumeric() || c == '_') {
                k += 1;
            }
            let id: String = self.s[j + 1..k].iter().collect();
            self.push(&id, self.line);
            self.i = k;
            return;
        }
        self.push(&word, self.line);
        self.i = j;
    }

    fn char_lit(&mut self, i: usize) {
        let mut j = i + 1;
        while self.at(j).is_some_and(|c| c != '\'') {
            if self.s[j] == '\\' {
                j += 1;
            }
            j += 1;
        }
        self.push("CHR", self.line);
        self.i = j + 1;
    }

    /// A string starting at `i` (at its `#`s or its quote).
    fn string(&mut self, i: usize, raw: bool, fmt: bool) {
        let l0 = self.line;
        let mut j = i;
        let mut hashes = 0;
        while self.at(j) == Some('#') {
            hashes += 1;
            j += 1;
        }
        j += 1;
        if raw {
            let end: String = std::iter::once('"')
                .chain(std::iter::repeat_n('#', hashes))
                .collect();
            loop {
                match self.at(j) {
                    None => {
                        self.out.err = Some(format!("unterminated raw string at line {l0}"));
                        break;
                    }
                    Some('\n') => self.line += 1,
                    Some(_) if self.starts(j, &end) => {
                        j += end.chars().count();
                        break;
                    }
                    Some(_) => {}
                }
                j += 1;
            }
        } else {
            j = self.cooked(j, fmt);
        }
        for l in l0..=self.line {
            Self::mark(&mut self.out.code, l);
        }
        self.out.toks.push(Tok {
            text: "STR".to_owned(),
            line: l0,
        });
        self.i = j;
    }

    /// The rest of a cooked string from `j`; returns the index past its
    /// closing quote. `fmt`: `{expr}` holes may hold strings.
    fn cooked(&mut self, mut j: usize, fmt: bool) -> usize {
        while let Some(c) = self.at(j) {
            match c {
                '\\' => {
                    if self.at(j + 1) == Some('\n') {
                        self.line += 1;
                    }
                    j += 2;
                    continue;
                }
                '\n' => self.line += 1,
                '"' => return j + 1,
                '{' if fmt => {
                    if self.at(j + 1) == Some('{') {
                        j += 2;
                        continue;
                    }
                    j = self.hole(j + 1);
                    continue;
                }
                _ => {}
            }
            j += 1;
        }
        self.out.err = Some("unterminated string".to_owned());
        j
    }

    /// An f-string hole from just past its `{`; returns the index past `}`.
    fn hole(&mut self, mut j: usize) -> usize {
        let mut depth = 1;
        while depth > 0 {
            match self.at(j) {
                None => break,
                Some('{') => depth += 1,
                Some('}') => depth -= 1,
                Some('"') => {
                    j = self.cooked(j + 1, false);
                    continue;
                }
                Some('\n') => self.line += 1,
                Some(_) => {}
            }
            j += 1;
        }
        j
    }
}

#[must_use]
pub fn lex(src: &str) -> Lexed {
    Lexer {
        s: src.chars().collect(),
        i: 0,
        line: 1,
        out: Lexed {
            toks: Vec::new(),
            code: Vec::new(),
            comment: Vec::new(),
            err: None,
        },
    }
    .run()
}
