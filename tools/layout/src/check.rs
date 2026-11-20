//! Rules A, D and E over one file's tokens.
//!
//! - A: `#[path]` modules (`mod.rs` / `mod.nabi` files are found by name).
//! - D: the header (`pub mod`, `mod`, `pub use`, `use`, in that order, one
//!   blank line between groups and none inside one, all before any other
//!   item) and no `use` inside an item body.
//! - E: no `super::` (Rust) / `sup::` (Nabi) path and no `pub(super)` /
//!   `pub(sub)` outside an inline `mod x { }`.

use crate::lex::{Lexed, Tok, lex};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Rust,
    Nabi,
}

/// One violation; `line` 0 is the whole file.
pub struct Finding {
    pub rule: char,
    pub line: usize,
    pub kind: String,
    pub detail: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Inner,
    PubMod,
    Mod,
    PubUse,
    Use,
    InlineMod,
    Other,
}

impl Kind {
    const fn rank(self) -> Option<usize> {
        match self {
            Self::PubMod => Some(0),
            Self::Mod => Some(1),
            Self::PubUse => Some(2),
            Self::Use => Some(3),
            _ => None,
        }
    }
}

const GROUPS: [&str; 4] = ["pub mod", "mod", "pub use", "use"];

struct Item {
    kind: Kind,
    first: usize,
    last: usize,
}

struct Checker<'a> {
    lx: &'a Lexed,
    t: &'a [Tok],
    sup: &'static str,
    out: Vec<Finding>,
    /// Token ranges of inline module bodies.
    inline: Vec<(usize, usize)>,
    /// Token ranges of item bodies that may not hold a `use`.
    bodies: Vec<(usize, usize)>,
}

fn closer(open: &str) -> Option<&'static str> {
    match open {
        "(" => Some(")"),
        "[" => Some("]"),
        "{" => Some("}"),
        _ => None,
    }
}

/// The index of the closer of the opener at `i` (or the last token).
fn matching(t: &[Tok], i: usize) -> usize {
    let mut depth = 0usize;
    for (k, tok) in t.iter().enumerate().skip(i) {
        if closer(&tok.text).is_some() {
            depth += 1;
        } else if matches!(tok.text.as_str(), ")" | "]" | "}") {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return k;
            }
        }
    }
    t.len().saturating_sub(1)
}

impl Checker<'_> {
    fn add(&mut self, rule: char, line: usize, kind: &str, detail: String) {
        self.out.push(Finding {
            rule,
            line,
            kind: kind.to_owned(),
            detail,
        });
    }

    fn tok(&self, k: usize) -> &str {
        self.t.get(k).map_or("", |t| t.text.as_str())
    }

    fn line(&self, k: usize) -> usize {
        self.t
            .get(k)
            .or_else(|| self.t.last())
            .map_or(0, |t| t.line)
    }

    /// The `;` ending the `use` at `k`.
    fn use_end(&self, k: usize, j: usize) -> usize {
        let mut e = k;
        while e < j && self.tok(e) != ";" {
            if closer(self.tok(e)).is_some() {
                e = matching(self.t, e);
            }
            e += 1;
        }
        e
    }

    /// The last token of any other item at `k`: its `;` or the `}` of
    /// its body.
    fn item_end(&self, k: usize, j: usize) -> usize {
        let mut e = k;
        while e < j {
            let x = self.tok(e);
            if x == ";" {
                break;
            }
            if x == "{" {
                e = matching(self.t, e);
                break;
            }
            if x == "(" || x == "[" {
                e = matching(self.t, e);
            }
            e += 1;
        }
        e.min(j.saturating_sub(1))
    }

    /// The items of the token range `i..j` (a file or an inline module body).
    fn body(&mut self, from: usize, j: usize, inline: bool) {
        let mut items = Vec::new();
        // `macro_rules!` names defined so far in this body
        let mut macros: Vec<String> = Vec::new();
        let mut k = from;
        while k < j {
            if self.tok(k) == ";" {
                k += 1;
                continue;
            }
            let st = k;
            if self.tok(k) == "#!doc" {
                items.push(Item {
                    kind: Kind::Inner,
                    first: st,
                    last: k,
                });
                k += 1;
                continue;
            }
            if self.tok(k) == "#" && self.tok(k + 1) == "!" {
                let e = matching(self.t, k + 2);
                items.push(Item {
                    kind: Kind::Inner,
                    first: st,
                    last: e,
                });
                k = e + 1;
                continue;
            }
            let mut path_attr = None;
            while k < j && (self.tok(k) == "#doc" || self.tok(k) == "#" && self.tok(k + 1) == "[") {
                if self.tok(k) == "#doc" {
                    k += 1;
                    continue;
                }
                let e = matching(self.t, k + 1);
                let cfg_path = self.tok(k + 2) == "cfg_attr"
                    && (k + 3..e).any(|q| self.tok(q) == "path" && self.tok(q + 1) == "=");
                if self.tok(k + 2) == "path" || cfg_path {
                    path_attr = Some(self.line(k));
                }
                k = e + 1;
            }
            if k >= j {
                break;
            }
            let mut public = false;
            if self.tok(k) == "pub" {
                public = true;
                k += 1;
                if self.tok(k) == "(" {
                    k = matching(self.t, k) + 1;
                }
            }
            let kw = self.tok(k).to_owned();
            if kw == "mod" && self.tok(k + 2) == ";" {
                if let Some(l) = path_attr {
                    self.add('A', l, "path-attr", String::new());
                }
                let kind = if public { Kind::PubMod } else { Kind::Mod };
                items.push(Item {
                    kind,
                    first: st,
                    last: k + 2,
                });
                k += 3;
                continue;
            }
            if kw == "mod" && self.tok(k + 2) == "{" {
                let e = matching(self.t, k + 2);
                self.inline.push((k + 2, e));
                self.body(k + 3, e, true);
                items.push(Item {
                    kind: Kind::InlineMod,
                    first: st,
                    last: e,
                });
                k = e + 1;
                continue;
            }
            if kw == "use" {
                let e = self.use_end(k, j);
                // `use m;` re-exporting a `macro_rules! m` above it must
                // follow the macro: not a header item
                let reexport = e == k + 2 && macros.iter().any(|m| m == self.tok(k + 1));
                let kind = if reexport {
                    Kind::Other
                } else if public {
                    Kind::PubUse
                } else {
                    Kind::Use
                };
                items.push(Item {
                    kind,
                    first: st,
                    last: e.min(self.t.len() - 1),
                });
                k = e + 1;
                continue;
            }
            let e = self.item_end(k, j);
            if kw == "macro_rules" && self.tok(k + 1) == "!" {
                macros.push(self.tok(k + 2).to_owned());
            }
            // a macro body is a template, not code
            if kw != "macro_rules" && kw != "macro" {
                self.bodies.push((k, e));
            }
            items.push(Item {
                kind: Kind::Other,
                first: st,
                last: e,
            });
            k = e + 1;
        }
        self.header(&items, inline);
    }

    fn blank(&self, l: usize) -> bool {
        !self.lx.code.get(l).copied().unwrap_or(false)
            && !self.lx.comment.get(l).copied().unwrap_or(false)
    }

    fn plain_comment(&self, l: usize) -> bool {
        self.lx.comment.get(l).copied().unwrap_or(false)
            && !self.lx.code.get(l).copied().unwrap_or(false)
    }

    fn header(&mut self, items: &[Item], inline: bool) {
        let pre = if inline { "inline-" } else { "" };
        let mut seen_other = None;
        let mut max_rank = None;
        // the previous header decl, while the header is contiguous
        let mut prev: Option<(usize, usize)> = None;
        let mut runs = [0usize; 4];
        let mut last_rank = None;
        for it in items {
            let sl = self.line(it.first);
            let el = self.line(it.last);
            let Some(r) = it.kind.rank() else {
                if it.kind == Kind::Inner {
                    if prev.is_some() || seen_other.is_some() {
                        self.add('D', sl, &format!("{pre}inner-attr-late"), String::new());
                    }
                } else {
                    seen_other.get_or_insert(sl);
                    prev = None;
                }
                continue;
            };
            if let Some(o) = seen_other {
                let d = format!("{} after an item at line {o}", GROUPS[r]);
                self.add('D', sl, &format!("{pre}decl-after-items"), d);
            }
            if let Some(m) = max_rank.filter(|&m| r < m) {
                let d = format!("{} after {}", GROUPS[r], GROUPS[m]);
                self.add('D', sl, &format!("{pre}order"), d);
            }
            max_rank = max_rank.max(Some(r));
            if last_rank != Some(r) {
                runs[r] += 1;
            }
            last_rank = Some(r);
            if let Some((pr, pel)) = prev {
                let gap = pel + 1..sl;
                let blanks: Vec<usize> = gap.clone().filter(|&l| self.blank(l)).collect();
                if let Some(c) = gap.clone().find(|&l| self.plain_comment(l)) {
                    self.add('D', c, &format!("{pre}comment-in-header"), String::new());
                }
                if pr == r && !blanks.is_empty() {
                    let k = format!("{pre}blank-in-group");
                    self.add('D', blanks[0], &k, GROUPS[r].to_owned());
                }
                if pr != r && blanks.is_empty() {
                    let d = format!("{} then {}", GROUPS[pr], GROUPS[r]);
                    self.add('D', sl, &format!("{pre}no-blank-between-groups"), d);
                }
                if pr != r && blanks.len() > 1 {
                    let k = format!("{pre}multi-blank-between-groups");
                    self.add('D', blanks[0], &k, String::new());
                }
            }
            prev = Some((r, el));
        }
        for (r, &n) in runs.iter().enumerate() {
            if n > 1 {
                let d = format!("{} in {n} runs", GROUPS[r]);
                self.add('D', 0, &format!("{pre}group-split"), d);
            }
        }
    }

    fn in_inline(&self, k: usize) -> bool {
        self.inline.iter().any(|&(a, b)| a < k && k < b)
    }

    fn check_super(&mut self) {
        let sup = self.sup;
        let vis = if sup == "super" { "super" } else { "sub" };
        for k in 0..self.t.len() {
            let x = self.tok(k);
            if x == vis
                && k >= 2
                && self.tok(k - 1) == "("
                && self.tok(k - 2) == "pub"
                && self.tok(k + 1) == ")"
            {
                if !self.in_inline(k) {
                    self.add('E', self.line(k), &format!("vis-pub({vis})"), String::new());
                }
                continue;
            }
            if x != sup || self.tok(k + 1) != "::" || self.in_inline(k) {
                continue;
            }
            // only the first of a `super::super::` chain
            if k >= 2 && self.tok(k - 1) == "::" && self.tok(k - 2) == sup {
                continue;
            }
            let mut q = k;
            while q > 0 && !matches!(self.tok(q - 1), ";" | "{" | "}") {
                q -= 1;
            }
            let seg: Vec<&str> = (q..k).map(|n| self.tok(n)).collect();
            let kind = if seg.len() >= 2 && seg[seg.len() - 1] == "in" && seg[seg.len() - 2] == "("
            {
                format!("vis-in-{sup}")
            } else if seg.contains(&"use") {
                format!("use-{sup}")
            } else {
                "path".to_owned()
            };
            let mut chain = 1;
            let mut m = k + 2;
            while self.tok(m) == sup && self.tok(m + 1) == "::" {
                chain += 1;
                m += 2;
            }
            self.add('E', self.line(k), &kind, format!("{sup}::").repeat(chain));
        }
    }

    fn check_local_use(&mut self) {
        let mut found = Vec::new();
        for &(st, e) in &self.bodies {
            for k in st + 1..=e.min(self.t.len() - 1) {
                let starts = matches!(self.tok(k - 1), "{" | ";" | "}" | "]" | "pub");
                if self.tok(k) == "use" && starts {
                    found.push(self.line(k));
                }
            }
        }
        for l in found {
            self.add('D', l, "local-use", "use inside an item body".to_owned());
        }
    }
}

/// Rules A (`#[path]`), D and E for one source file.
#[must_use]
pub fn check(src: &str, lang: Lang) -> Vec<Finding> {
    let lx = lex(src);
    let mut c = Checker {
        lx: &lx,
        t: &lx.toks,
        sup: if lang == Lang::Rust { "super" } else { "sup" },
        out: Vec::new(),
        inline: Vec::new(),
        bodies: Vec::new(),
    };
    if let Some(e) = &lx.err {
        c.add('X', 0, "lex-error", e.clone());
    }
    c.body(0, c.t.len(), false);
    c.check_super();
    c.check_local_use();
    c.out.sort_by_key(|f| f.line);
    c.out
}
