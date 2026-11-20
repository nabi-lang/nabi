//! The C preprocessor: includes, conditionals, macro expansion and the
//! pragmas that change declarations (`pack`, `once`, `push_macro`).

// Expression and layout code names values as C does (`a`, `b`, `l`, `r`).
#![allow(clippy::many_single_char_names)]

pub mod builtin;
pub mod sysinc;

mod cond;
mod directive;
mod expand;
mod query;

pub(crate) use crate::query::QUERIES;

use nabic_core::{HashMap, HashSet};
use nabicc_lex::{
    ASSUME_NONNULL, BOL, Kind, Lexer, Loc, NOEXP, Names, PLACEMARKER, SPACE, Sym, Tok, is_keyword,
    kw, relex,
};
use nabicc_target::{Arch, Os, Target};
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// A diagnostic, formatted as `file:line:col: error: msg`.
#[derive(Debug)]
pub struct Error(pub String);

impl Error {
    pub fn at(files: &[File], loc: Loc, msg: &str) -> Self {
        match files.get(loc.file as usize).filter(|_| loc.file != 0) {
            Some(f) => Self(format!(
                "{}:{}:{}: error: {msg}",
                f.path.display(),
                loc.line,
                loc.col
            )),
            None => Self(format!("error: {msg}")),
        }
    }

    pub fn plain(msg: &str) -> Self {
        Self(msg.to_owned())
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A source file: as found, and whether it is a system header.
#[derive(Clone, Debug)]
pub struct File {
    pub path: PathBuf,
    pub system: bool,
    /// One of the built-in headers or the predefines (no file on disk).
    pub builtin: bool,
}

/// An include directory.
#[derive(Clone, Debug)]
pub struct Dir {
    pub path: PathBuf,
    pub system: bool,
    pub kind: DirKind,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DirKind {
    /// `-iquote`: only for `#include "..."`.
    Quote,
    Normal,
    /// The built-in headers.
    Builtin,
    /// Apple frameworks: `<A/B.h>` is `A.framework/Headers/B.h`.
    Framework,
}

/// A `#define` as written, for bindgen's constants.
pub struct MacroDef {
    pub name: Sym,
    pub loc: Loc,
    pub function_like: bool,
    pub body: Vec<Tok>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Builtin {
    Line,
    File,
    FileName,
    BaseFile,
    Counter,
    IncludeLevel,
    Date,
    Time,
    Timestamp,
    /// `__has_feature` and the other function-like ones.
    Query,
}

pub(crate) struct Macro {
    params: Option<Vec<Sym>>,
    variadic: bool,
    body: Vec<Tok>,
    builtin: Option<Builtin>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Guard {
    Start,
    In(Sym),
    After(Sym),
    None,
}

struct Cond {
    /// Some branch of this group was taken.
    taken: bool,
    loc: Loc,
}

enum Source {
    File {
        lx: Lexer,
        file: u32,
        /// Search list index the file was found at, for `#include_next`.
        dir: Option<usize>,
        conds: Vec<Cond>,
        guard: Guard,
        key: PathBuf,
    },
    Tokens {
        toks: Vec<Tok>,
        pos: usize,
        /// Re-enabled when the tokens run out.
        enable: Option<Sym>,
    },
}

/// The sentinel that ends an isolated expansion.
const END: u8 = 0x80;

pub struct Pp<'t> {
    pub names: Names,
    pub files: Vec<File>,
    pub defs: Vec<MacroDef>,
    target: &'t Target,
    macros: HashMap<Sym, Rc<Macro>>,
    disabled: HashSet<Sym>,
    stack: Vec<Source>,
    dirs: Vec<Dir>,
    file_ids: HashMap<PathBuf, u32>,
    once: HashSet<PathBuf>,
    guards: HashMap<PathBuf, Sym>,
    cache: HashMap<PathBuf, Option<Rc<[u8]>>>,
    pushed: HashMap<Sym, Vec<Option<Rc<Macro>>>>,
    pack: Vec<(Option<Sym>, u64)>,
    pack_now: u64,
    counter: u64,
    in_if: bool,
    base_file: String,
    /// Tokens put back, read before the sources.
    back: Vec<Tok>,
}

const fn tok(kind: Kind, sym: Sym, loc: Loc) -> Tok {
    Tok {
        kind,
        flags: 0,
        sym,
        loc,
    }
}

impl<'t> Pp<'t> {
    pub fn new(target: &'t Target, dirs: Vec<Dir>) -> Self {
        let mut pp = Self {
            names: Names::default(),
            files: vec![File {
                path: PathBuf::new(),
                system: false,
                builtin: true,
            }],
            defs: Vec::new(),
            target,
            macros: HashMap::default(),
            disabled: HashSet::default(),
            stack: Vec::new(),
            dirs,
            file_ids: HashMap::default(),
            once: HashSet::default(),
            guards: HashMap::default(),
            cache: HashMap::default(),
            pushed: HashMap::default(),
            pack: Vec::new(),
            pack_now: 0,
            counter: 0,
            in_if: false,
            base_file: String::new(),
            back: Vec::new(),
        };
        let builtins = [
            ("__LINE__", Builtin::Line),
            ("__FILE__", Builtin::File),
            ("__FILE_NAME__", Builtin::FileName),
            ("__BASE_FILE__", Builtin::BaseFile),
            ("__COUNTER__", Builtin::Counter),
            ("__INCLUDE_LEVEL__", Builtin::IncludeLevel),
            ("__DATE__", Builtin::Date),
            ("__TIME__", Builtin::Time),
            ("__TIMESTAMP__", Builtin::Timestamp),
        ];
        let queries = QUERIES.iter().map(|q| (*q, Builtin::Query));
        for (name, b) in builtins.into_iter().chain(queries) {
            let s = pp.names.intern(name);
            pp.macros.insert(
                s,
                Rc::new(Macro {
                    params: None,
                    variadic: false,
                    body: Vec::new(),
                    builtin: Some(b),
                }),
            );
        }
        pp
    }

    pub fn err(&self, loc: Loc, msg: &str) -> Error {
        Error::at(&self.files, loc, msg)
    }

    fn spell(&self, t: Tok) -> &str {
        self.names.get(t.sym)
    }

    fn is(&self, t: Tok, p: &str) -> bool {
        matches!(t.kind, Kind::Punct) && self.names.get(t.sym) == p
    }

    /// Run `text` (the predefines, `-D` / `-U`) as a file of its own.
    pub fn run_text(&mut self, text: &str) -> Result<(), Error> {
        let file = self.files.len() as u32;
        self.files.push(File {
            path: PathBuf::from("<built-in>"),
            system: false,
            builtin: true,
        });
        self.stack.push(Source::File {
            lx: Lexer::new(file, text.as_bytes().into()),
            file,
            dir: None,
            conds: Vec::new(),
            guard: Guard::None,
            key: PathBuf::new(),
        });
        let t = self.next()?;
        if t.kind != Kind::Eof {
            return Err(self.err(t.loc, "a predefined macro or -D option left text behind"));
        }
        Ok(())
    }

    /// Start on the main file, with `pre` (`-include`) read first.
    pub fn enter_main(&mut self, path: &Path, pre: &[PathBuf]) -> Result<(), Error> {
        self.base_file = path.to_string_lossy().into_owned();
        for p in std::iter::once(path).chain(pre.iter().rev().map(PathBuf::as_path)) {
            let Some(src) = self.read(p) else {
                return Err(Error::plain(&format!(
                    "{}: cannot read the file",
                    p.display()
                )));
            };
            let file = self.file_id(p, false, false);
            self.stack.push(Source::File {
                lx: Lexer::new(file, src),
                file,
                dir: None,
                conds: Vec::new(),
                guard: Guard::Start,
                key: p.to_path_buf(),
            });
        }
        Ok(())
    }

    fn file_id(&mut self, p: &Path, system: bool, builtin: bool) -> u32 {
        if let Some(&id) = self.file_ids.get(p) {
            return id;
        }
        let id = self.files.len() as u32;
        self.files.push(File {
            path: p.to_path_buf(),
            system,
            builtin,
        });
        self.file_ids.insert(p.to_path_buf(), id);
        id
    }

    fn read(&mut self, path: &Path) -> Option<Rc<[u8]>> {
        if let Some(Some(c)) = self.cache.get(path) {
            return Some(c.clone());
        }
        let data: Option<Rc<[u8]>> = std::fs::read(path).ok().map(Rc::from);
        self.cache.insert(path.to_path_buf(), data.clone());
        data
    }

    fn exists(&mut self, path: &Path) -> bool {
        if let Some(c) = self.cache.get(path) {
            return c.is_some();
        }
        if path.is_file() {
            true
        } else {
            self.cache.insert(path.to_path_buf(), None);
            false
        }
    }

    pub fn unget(&mut self, t: Tok) {
        self.back.push(t);
    }

    pub fn is_defined(&self, s: Sym) -> bool {
        self.macros.contains_key(&s)
    }

    /// The current `#pragma pack` value in bytes (0: none).
    pub const fn pack(&self) -> u64 {
        self.pack_now
    }

    /// The next token of the translation unit, macro-expanded.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Result<Tok, Error> {
        loop {
            let mut t = self.raw()?;
            if t.kind == Kind::Ident && !t.has(NOEXP) {
                if self.in_if {
                    if let Some(v) = self.if_operator(t)? {
                        return Ok(v);
                    }
                }
                if let Some(m) = self.macros.get(&t.sym).cloned() {
                    if self.disabled.contains(&t.sym) {
                        t.flags |= NOEXP;
                        return Ok(t);
                    }
                    if self.expand(t, &m)? {
                        continue;
                    }
                } else if t.sym == kw::PRAGMA_OP || (t.sym == kw::MS_PRAGMA && self.target.msvc) {
                    if let Some(p) = self.pragma_operator(t)? {
                        return Ok(p);
                    }
                    continue;
                }
            }
            if t.has(PLACEMARKER) {
                continue;
            }
            return Ok(t);
        }
    }

    /// The next token, unexpanded: directives run, finished sources popped.
    fn raw(&mut self) -> Result<Tok, Error> {
        if let Some(t) = self.back.pop() {
            return Ok(t);
        }
        loop {
            let Some(top) = self.stack.last_mut() else {
                return Ok(tok(Kind::Eof, 0, Loc::default()));
            };
            match top {
                Source::Tokens { toks, pos, enable } => {
                    if let Some(&t) = toks.get(*pos) {
                        if !t.has(END) {
                            *pos += 1;
                        }
                        return Ok(t);
                    }
                    if let Some(m) = enable.take() {
                        self.disabled.remove(&m);
                    }
                    self.stack.pop();
                }
                Source::File { lx, guard, .. } => {
                    let t = lx.next(&mut self.names);
                    if t.kind == Kind::Eof {
                        self.end_file()?;
                        continue;
                    }
                    if t.has(BOL) && t.kind == Kind::Punct && self.names.get(t.sym) == "#" {
                        if let Some(p) = self.directive()? {
                            return Ok(p);
                        }
                        continue;
                    }
                    if matches!(guard, Guard::Start | Guard::After(_)) {
                        *guard = Guard::None;
                    }
                    return Ok(t);
                }
            }
        }
    }

    fn end_file(&mut self) -> Result<(), Error> {
        let Some(Source::File {
            conds, guard, key, ..
        }) = self.stack.pop()
        else {
            return Ok(());
        };
        if let Some(c) = conds.last() {
            return Err(self.err(c.loc, "unterminated conditional directive"));
        }
        if let Guard::After(g) = guard {
            self.guards.insert(key, g);
        }
        Ok(())
    }

    /// The top source, a file while a directive runs.
    fn top_file(&mut self) -> (&mut Lexer, &mut Vec<Cond>, &mut Guard, u32, Option<usize>) {
        match self.stack.last_mut() {
            Some(Source::File {
                lx,
                conds,
                guard,
                file,
                dir,
                ..
            }) => (lx, conds, guard, *file, *dir),
            _ => unreachable!("a directive outside a file"),
        }
    }

    /// The next token of the directive line.
    fn line_tok(&mut self) -> Option<Tok> {
        let names = &mut self.names;
        match self.stack.last_mut() {
            Some(Source::File { lx, .. }) => lx.next_in_line(names),
            _ => None,
        }
    }

    /// The rest of the directive line.
    fn line(&mut self) -> Vec<Tok> {
        let mut out = Vec::new();
        while let Some(t) = self.line_tok() {
            out.push(t);
        }
        out
    }
}

/// Built-in headers' paths start here.
pub const BUILTIN_DIR: &str = "<nabic>";

/// An integer literal's value and whether its suffix makes it unsigned.
pub fn parse_int(s: &str) -> Option<(u64, bool)> {
    let l = s.to_ascii_lowercase();
    let (radix, rest) = if let Some(h) = l.strip_prefix("0x") {
        (16, h)
    } else if let Some(b) = l.strip_prefix("0b") {
        (2, b)
    } else if l.starts_with('0') {
        (8, l.as_str())
    } else {
        (10, l.as_str())
    };
    let end = rest
        .find(|c: char| !c.is_digit(radix))
        .unwrap_or(rest.len());
    let (digits, suffix) = rest.split_at(end);
    let suffix = suffix
        .strip_suffix("i64")
        .or_else(|| suffix.strip_suffix("i32"))
        .or_else(|| suffix.strip_suffix("i16"))
        .or_else(|| suffix.strip_suffix("i8"))
        .unwrap_or(suffix);
    if !matches!(suffix, "" | "u" | "l" | "ul" | "lu" | "ll" | "ull" | "llu") || digits.is_empty() {
        return None;
    }
    let v = u64::from_str_radix(digits, radix).ok()?;
    Some((v, suffix.contains('u')))
}

/// A character literal's value (as `int`, or its type's for prefixed ones).
pub fn char_value(s: &str, char_signed: bool) -> Option<i64> {
    let (prefix, body) = s.split_at(s.find('\'')?);
    let body = body.strip_prefix('\'')?.strip_suffix('\'')?;
    let mut vals: Vec<u32> = Vec::new();
    let b = body.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' {
            i += 1;
            let c = *b.get(i)?;
            let v = match c {
                b'n' => 10,
                b't' => 9,
                b'r' => 13,
                b'a' => 7,
                b'b' => 8,
                b'f' => 12,
                b'v' => 11,
                b'e' => 27,
                b'x' => {
                    let start = i + 1;
                    let mut end = start;
                    while end < b.len() && b[end].is_ascii_hexdigit() {
                        end += 1;
                    }
                    let v =
                        u32::from_str_radix(std::str::from_utf8(&b[start..end]).ok()?, 16).ok()?;
                    i = end;
                    vals.push(v);
                    continue;
                }
                b'0'..=b'7' => {
                    let start = i;
                    let mut end = start;
                    while end < b.len() && end < start + 3 && (b'0'..=b'7').contains(&b[end]) {
                        end += 1;
                    }
                    let v =
                        u32::from_str_radix(std::str::from_utf8(&b[start..end]).ok()?, 8).ok()?;
                    i = end;
                    vals.push(v);
                    continue;
                }
                c => u32::from(c),
            };
            vals.push(v);
            i += 1;
        } else {
            let ch = body[i..].chars().next()?;
            vals.push(ch as u32);
            i += ch.len_utf8();
        }
    }
    if prefix.is_empty() {
        // `int` from `char`s, multi-character ones big-endian.
        let mut v: i64 = 0;
        for c in &vals {
            v = (v << 8) | i64::from(*c as u8);
        }
        if vals.len() == 1 && char_signed {
            Some(i64::from(vals[0] as u8 as i8))
        } else if vals.len() == 1 {
            Some(i64::from(vals[0] as u8))
        } else {
            Some(i64::from(v as i32))
        }
    } else {
        vals.last().map(|v| i64::from(*v))
    }
}
