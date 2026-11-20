//! The header of a file and of each inline `mod x { }`: its leading
//! `mod` / `use` items, regrouped as `pub mod`, `mod`, `pub use`, `use`.

use crate::fmt::{is_close, is_open};
use nabic_lexer::{Token, TokenKind};
use std::cmp::Ordering;

/// One header item over lines `start..=end`: the comments, docs and
/// attributes right above it, then the item.
struct Chunk {
    start: usize,
    end: usize,
    /// 0 `pub mod`, 1 `mod`, 2 `pub use`, 3 `use`.
    group: u8,
    key: Vec<String>,
}

/// A header to rewrite: lines `start..rest`, the items from `chunks[0]`.
struct Plan {
    start: usize,
    chunks: Vec<Chunk>,
    rest: usize,
}

/// `src` (already laid out, `tokens` its tokens) with every header
/// regrouped and sorted.
pub fn reorder(src: &str, tokens: &[Token]) -> String {
    let lines: Vec<&str> = src.lines().collect();
    let mut first = vec![None; lines.len()];
    for (i, t) in tokens.iter().enumerate() {
        if t.kind == TokenKind::Eof {
            break;
        }
        let l = t.span.line as usize - 1;
        if l < first.len() && first[l].is_none() {
            first[l] = Some(i);
        }
    }
    let cx = Cx {
        src,
        lines: &lines,
        first: &first,
        tokens,
    };
    // The file, then each inline `mod x {` that ends its line.
    let mut starts = vec![0];
    for (i, w) in tokens.windows(3).enumerate() {
        if let [m, n, b] = w {
            if m.kind == TokenKind::Mod
                && matches!(n.kind, TokenKind::Ident(_))
                && b.kind == TokenKind::LBrace
                && cx.line(i + 3) > b.span.line as usize - 1
            {
                starts.push(b.span.line as usize);
            }
        }
    }
    let mut out: Vec<String> = lines.iter().map(|&l| l.to_owned()).collect();
    let mut changed = false;
    // Last first: a rewrite moves only the lines after it.
    for &s in starts.iter().rev() {
        if let Some(p) = cx.scan(s) {
            let new = cx.emit(&p);
            changed |= out[p.start..p.rest] != new[..];
            out.splice(p.start..p.rest, new);
        }
    }
    if !changed {
        return src.to_owned();
    }
    let mut s = out.join("\n");
    s.push('\n');
    s
}

struct Cx<'a> {
    src: &'a str,
    lines: &'a [&'a str],
    /// The first token starting on each line.
    first: &'a [Option<usize>],
    tokens: &'a [Token],
}

impl Cx<'_> {
    /// The 0-based line of token `i` (past the end: after the last line).
    fn line(&self, i: usize) -> usize {
        match self.tokens.get(i) {
            Some(t) if t.kind != TokenKind::Eof => t.span.line as usize - 1,
            _ => usize::MAX,
        }
    }

    fn kind(&self, i: usize) -> Option<&TokenKind> {
        self.tokens.get(i).map(|t| &t.kind)
    }

    /// The bracket closing the one opened at `i`.
    fn close(&self, i: usize) -> Option<usize> {
        let mut depth = 0usize;
        for (j, t) in self.tokens.iter().enumerate().skip(i) {
            if is_open(&t.kind) {
                depth += 1;
            } else if is_close(&t.kind) {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(j);
                }
            } else if t.kind == TokenKind::Eof {
                return None;
            }
        }
        None
    }

    /// The `mod x;` / `use ..;` item at token `i`: its group, sort key
    /// and the index of its `;`.
    fn item(&self, mut i: usize) -> Option<(u8, Vec<String>, usize)> {
        let public = self.kind(i) == Some(&TokenKind::Pub);
        if public {
            i += 1;
            if self.kind(i) == Some(&TokenKind::LParen) {
                i = self.close(i)? + 1;
            }
        }
        match self.kind(i)? {
            TokenKind::Mod => match (self.kind(i + 1)?, self.kind(i + 2)?) {
                (TokenKind::Ident(name), TokenKind::Semi) => {
                    Some((u8::from(!public), vec![name.clone()], i + 2))
                }
                _ => None,
            },
            TokenKind::Use => {
                let mut j = i + 1;
                let mut path = String::new();
                loop {
                    match self.kind(j)? {
                        TokenKind::Semi => break,
                        TokenKind::Eof => return None,
                        k if is_open(k) => {
                            let c = self.close(j)?;
                            for t in &self.tokens[j..=c] {
                                path.push_str(self.text(t));
                            }
                            j = c + 1;
                        }
                        k if is_close(k) => return None,
                        _ => {
                            path.push_str(self.text(&self.tokens[j]));
                            j += 1;
                        }
                    }
                }
                let key = path.split("::").map(str::to_owned).collect();
                Some((2 + u8::from(!public), key, j))
            }
            _ => None,
        }
    }

    fn text(&self, t: &Token) -> &str {
        self.src
            .get(t.span.start as usize..t.span.end as usize)
            .unwrap_or("")
    }

    /// The header of the module whose body starts at line `start`.
    fn scan(&self, start: usize) -> Option<Plan> {
        let mut chunks: Vec<Chunk> = Vec::new();
        // The first line of the comments / attributes above the next item.
        let mut pending: Option<usize> = None;
        let mut l = start;
        while l < self.lines.len() {
            let text = self.lines[l].trim();
            let Some(i) = self.first[l] else {
                if text.is_empty() {
                    if chunks.is_empty() {
                        pending = None;
                    }
                } else if l == 0 && text.starts_with("#!") {
                    // A shebang.
                } else if text.starts_with("//!") {
                    if !chunks.is_empty() || pending.is_some() {
                        break;
                    }
                } else if text.starts_with("///") && !text.starts_with("////") {
                    pending.get_or_insert(l);
                } else if text.starts_with("//") {
                    // A plain comment above the first item stays put.
                    if !chunks.is_empty() {
                        pending.get_or_insert(l);
                    }
                } else {
                    break;
                }
                l += 1;
                continue;
            };
            if self.tokens[i].kind == TokenKind::HashBangLBracket {
                if !chunks.is_empty() || pending.is_some() {
                    break;
                }
                l = self.line(self.close(i)?) + 1;
                continue;
            }
            let mut j = i;
            while self.kind(j) == Some(&TokenKind::HashLBracket) {
                j = match self.close(j) {
                    Some(c) => c + 1,
                    None => break,
                };
            }
            let Some((group, key, semi)) = self.item(j) else {
                break;
            };
            let end = self.line(semi);
            if self.line(semi + 1) == end {
                break;
            }
            chunks.push(Chunk {
                start: pending.take().unwrap_or(l),
                end,
                group,
                key,
            });
            l = end + 1;
        }
        let first = chunks.first()?.start;
        let rest = pending.unwrap_or(l).min(self.lines.len());
        Some(Plan {
            start: first,
            chunks,
            rest,
        })
    }

    /// The new lines for `p.start..p.rest`.
    fn emit(&self, p: &Plan) -> Vec<String> {
        let mut out = Vec::new();
        for g in 0..4 {
            let mut group: Vec<&Chunk> = p.chunks.iter().filter(|c| c.group == g).collect();
            if group.is_empty() {
                continue;
            }
            group.sort_by(|a, b| cmp_path(&a.key, &b.key));
            if !out.is_empty() {
                out.push(String::new());
            }
            for c in group {
                out.extend(self.lines[c.start..=c.end].iter().map(|&l| l.to_owned()));
            }
        }
        let last = p.chunks.last().map_or(p.start, |c| c.end + 1);
        let blank = (last..p.rest).any(|l| self.lines[l].trim().is_empty());
        if blank && p.rest < self.lines.len() {
            out.push(String::new());
        }
        out
    }
}

/// `self`, `sup`, `pkg` first, then by name, segment by segment.
fn cmp_path(a: &[String], b: &[String]) -> Ordering {
    let rank = |s: &str| match s {
        "self" => 0,
        "sup" => 1,
        "pkg" => 2,
        _ => 3,
    };
    for (x, y) in a.iter().zip(b) {
        let o = rank(x).cmp(&rank(y)).then_with(|| x.cmp(y));
        if o != Ordering::Equal {
            return o;
        }
    }
    a.len().cmp(&b.len())
}
