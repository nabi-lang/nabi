//! `nabi fmt`: the standard layout for `.nabi` and `.nabion`.
//!
//! Layout: re-indent by bracket nesting (4 spaces), strip trailing
//! whitespace, collapse blank-line runs, end with one newline. Nothing
//! within a line changes, so comments survive.
//!
//! The one reordering: the `mod` / `use` items at the top of a file and
//! of an inline `mod x { }` regroup as `pub mod`, `mod`, `pub use`, `use`
//! (any `pub(..)` counts as `pub`), sorted within each group, with one
//! blank line between groups and none inside one. Comments, docs and
//! attributes right above an item move with it.

mod header;

use nabic_lexer::{Lexer, TokenKind};
use std::path::Path;

const INDENT: usize = 4;

/// Format the file `path` whose text is `src`: a `.nabion` file must be
/// one NABION value. `Err` is the rendered error; the file is left alone.
pub fn format_file(path: &Path, src: &str) -> Result<String, String> {
    format_file_with(path, src, &[])
}

/// `format_file`, lexing `ops` as operator tokens: those of the file's
/// package and its dependencies (`Session::operator_tokens`).
pub fn format_file_with(path: &Path, src: &str, ops: &[String]) -> Result<String, String> {
    let file = path.display().to_string();
    if path.extension().is_some_and(|x| x == "nabion") {
        nabic_nabion::parse(src).map_err(|e| e.render(src, &file))?;
        return Ok(layout(src, &lex(src, &file, &[])?));
    }
    format_source_with(src, &file, ops)
}

#[cfg(test)]
fn format_source(src: &str) -> Result<String, String> {
    format_source_with(src, "<fmt>", &[])
}

/// `Err` holds the first lexical error.
fn format_source_with(src: &str, file: &str, ops: &[String]) -> Result<String, String> {
    let out = layout(src, &lex(src, file, ops)?);
    // The same tokens, so the relayout lexes.
    let tokens = lex(&out, file, ops)?;
    Ok(header::reorder(&out, &tokens))
}

/// The tokens of `src`, with `ops` and the file's own extension operators.
fn lex(src: &str, file: &str, ops: &[String]) -> Result<Vec<nabic_lexer::Token>, String> {
    let (mut tokens, mut diags) = Lexer::new(src, file).with_ext_ops(ops.to_vec()).lex_all();
    if diags.iter().any(nabic_diagnostic::NabiDiagnostic::is_error) {
        let (program, _) = nabic_parser::parse_program(&tokens, file);
        let mut all = nabic_parser::collect_prec_table(&program, &[])
            .0
            .ext_ops()
            .symbols();
        all.extend(ops.iter().cloned());
        all.sort();
        all.dedup();
        (tokens, diags) = Lexer::new(src, file).with_ext_ops(all).lex_all();
    }
    if let Some(d) = diags.iter().find(|d| d.is_error()) {
        return Err(nabic_diagnostic::SourceMap::new().line(d));
    }
    Ok(tokens)
}

/// Re-indent, trim and collapse blank lines; `tokens` are `src`'s.
fn layout(src: &str, tokens: &[nabic_lexer::Token]) -> String {
    // Multi-line regions whose inside must stay byte-for-byte: tokens
    // (strings), whole f-strings and block comments that span a newline.
    let mut verbatim: Vec<(usize, usize)> = Vec::new();
    // A shebang line is not trivia to scan.
    let bom = if src.starts_with('\u{FEFF}') { 3 } else { 0 };
    let mut prev_end = bom + nabic_core::shebang_len(&src[bom..]);
    let mut fstrings = Vec::new();
    for t in tokens {
        let (mut start, end) = (t.span.start as usize, t.span.end as usize);
        block_comments(src, prev_end, start, &mut verbatim);
        match t.kind {
            TokenKind::FStrStart(_) => fstrings.push(start),
            TokenKind::FStrEnd(_) => start = fstrings.pop().unwrap_or(start),
            _ => {}
        }
        if src.get(start..end).is_some_and(|s| s.contains('\n')) {
            verbatim.push((start, end));
        }
        prev_end = end.max(prev_end);
    }
    let inside = |pos: usize| verbatim.iter().any(|&(s, e)| s < pos && pos < e);

    let mut out = String::with_capacity(src.len());
    // Per open bracket: (original, new) indent of the line opening it.
    let mut stack: Vec<(usize, usize)> = Vec::new();
    let mut next_tok = 0;
    let mut blank_run = 0usize;
    let mut ls = 0;
    while ls < src.len() {
        let le = src[ls..].find('\n').map_or(src.len(), |i| ls + i);
        let line = &src[ls..le];
        let lead = line.len() - line.trim_start_matches([' ', '\t']).len();
        let orig: usize = line[..lead]
            .chars()
            .map(|c| if c == '\t' { INDENT } else { 1 })
            .sum();
        let starts_verbatim = inside(ls);
        let body = if inside(le) {
            &line[lead..]
        } else {
            line[lead..].trim_end()
        };
        if !starts_verbatim && body.is_empty() {
            blank_run += 1;
            ls = le + 1;
            continue;
        }
        if blank_run > 0 && !out.is_empty() {
            out.push('\n');
        }
        blank_run = 0;
        let first = tokens
            .get(next_tok)
            .map(|t| (t.span.start as usize, &t.kind));
        let closes_first = first.is_some_and(|(at, k)| at == ls + lead && is_close(k));
        // Keep extra indentation only on a continuation line, whose previous
        // token doesn't end a statement, item or list entry.
        let continuation = next_tok > 0
            && !matches!(
                tokens[next_tok - 1].kind,
                TokenKind::Semi
                    | TokenKind::Comma
                    | TokenKind::LBrace
                    | TokenKind::RBrace
                    | TokenKind::LParen
                    | TokenKind::LBracket
                    | TokenKind::RBracket
                    | TokenKind::HashLBracket
                    | TokenKind::HashBangLBracket
            );
        let new = match (closes_first, stack.last()) {
            _ if starts_verbatim => orig,
            (true, Some(&(_, n))) => n,
            (false, Some(&(o, n))) if continuation => n + INDENT + orig.saturating_sub(o + INDENT),
            (false, Some(&(_, n))) => n + INDENT,
            (_, None) if continuation => orig,
            (_, None) => 0,
        };
        if starts_verbatim {
            out.push_str(&line[..lead]);
        } else {
            out.extend(std::iter::repeat_n(' ', new));
        }
        out.push_str(body);
        out.push('\n');
        while let Some(t) = tokens.get(next_tok) {
            if t.span.start as usize >= le || t.kind == TokenKind::Eof {
                break;
            }
            if is_open(&t.kind) {
                stack.push((orig, new));
            } else if is_close(&t.kind) {
                stack.pop();
            }
            next_tok += 1;
        }
        ls = le + 1;
    }
    out
}

const fn is_open(k: &TokenKind) -> bool {
    matches!(
        k,
        TokenKind::LParen
            | TokenKind::LBracket
            | TokenKind::LBrace
            | TokenKind::HashLBracket
            | TokenKind::HashBangLBracket
    )
}

const fn is_close(k: &TokenKind) -> bool {
    matches!(
        k,
        TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace
    )
}

/// Record the newline-spanning (nested) block comments in `src[from..to]`
/// (the trivia between two tokens).
fn block_comments(src: &str, from: usize, to: usize, out: &mut Vec<(usize, usize)>) {
    let b = src.as_bytes();
    let mut i = from;
    while i + 1 < to {
        match (b[i], b[i + 1]) {
            (b'/', b'/') => {
                while i < to && b[i] != b'\n' {
                    i += 1;
                }
            }
            (b'/', b'*') => {
                let start = i;
                let mut depth = 0usize;
                while i + 1 < to {
                    match (b[i], b[i + 1]) {
                        (b'/', b'*') => {
                            depth += 1;
                            i += 2;
                        }
                        (b'*', b'/') => {
                            depth -= 1;
                            i += 2;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => i += 1,
                    }
                }
                if src[start..i].contains('\n') {
                    out.push((start, i));
                }
            }
            _ => i += 1,
        }
    }
}

/// A minimal line diff (`--diff`): `-old` / `+new` lines with their
/// 1-based line numbers. O(n*m) LCS: source files are small.
#[must_use]
pub fn line_diff(old: &str, new: &str) -> String {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    let mut lcs = vec![vec![0u32; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            lcs[i][j] = if a[i] == b[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut out = String::new();
    while i < a.len() || j < b.len() {
        if i < a.len() && j < b.len() && a[i] == b[j] {
            i += 1;
            j += 1;
        } else if i < a.len() && (j == b.len() || lcs[i + 1][j] >= lcs[i][j + 1]) {
            // The old line first, as `diff` prints it.
            out.push_str(&format!("-{:>5} {}\n", i + 1, a[i]));
            i += 1;
        } else {
            out.push_str(&format!("+{:>5} {}\n", j + 1, b[j]));
            j += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reindents_and_keeps_comments() {
        let src =
            "\n\nfn f() {\n  // c\n      let x = foo(\n 1,\n   2);   \n\n\n\n  /* a\n  b */\n}\n\n";
        let want = "fn f() {\n    // c\n    let x = foo(\n        1,\n        2);\n\n    /* a\n  b */\n}\n";
        assert_eq!(format_source(src).unwrap(), want);
        assert_eq!(format_source(want).unwrap(), want);
    }

    #[test]
    fn keeps_shebang_and_doc_comment_forms() {
        let src = "#!/usr/bin/env nabi /*\n/** A.\n * B.\n */\nfn f() {\n    /*! C */\n}\n\
                   /// D\n//! E\n";
        assert_eq!(format_source(src).unwrap(), src);
    }

    #[test]
    fn keeps_multiline_strings_and_continuations() {
        let src = "fn f() {\n    let s = \"a  \n\n  b\";\n    let y = x\n        .g();\n}\n";
        assert_eq!(format_source(src).unwrap(), src);
        let src =
            "fn f() {\n    let s = f\"\"\"{g(\n  a,\n      b)}  \n\n x {d[\"k\"]}\"\"\";\n}\n";
        assert_eq!(format_source(src).unwrap(), src);
    }

    #[test]
    fn keeps_raw_identifiers() {
        let src = "fn r#match(r#type: i32) -> i32 {\n  let r#move = r#type;\n    r#move\n}\n";
        let want = "fn r#match(r#type: i32) -> i32 {\n    let r#move = r#type;\n    r#move\n}\n";
        assert_eq!(format_source(src).unwrap(), want);
        assert_eq!(format_source(want).unwrap(), want);
    }

    #[test]
    fn keeps_optional_last_semi() {
        let src = "fn f() {\n  while i < 2 { i += 1 }\n    { let x = 1; }\n  let y = 2\n}\n";
        assert_eq!(
            format_source(src).unwrap(),
            "fn f() {\n    while i < 2 { i += 1 }\n    { let x = 1; }\n    let y = 2\n}\n"
        );
    }

    #[test]
    fn nabion_files() {
        let src = "// cfg\nCfg {\n  tags: [\n      \"a\", // first\n  ],\n}\n";
        assert_eq!(
            format_file(Path::new("c.nabion"), src).unwrap(),
            "// cfg\nCfg {\n    tags: [\n        \"a\", // first\n    ],\n}\n"
        );
        let e = format_file(Path::new("c.nabion"), "Cfg { a: x }").unwrap_err();
        assert!(
            e.starts_with("error[E0010]: bare identifier") && e.contains("--> c.nabion:1:10\n"),
            "{e}"
        );
    }

    #[test]
    fn diff_marks_changed_lines() {
        assert_eq!(line_diff("a\nb\n", "a\nc\n"), "-    2 b\n+    2 c\n");
    }

    #[test]
    fn regroups_headers() {
        let src = "// file\n\nuse b;\nmod z;\n\n/// the a\npub mod a;\nuse a::x;\n\
                   pub(pkg) mod m;\n#[cfg(unix)]\npub use z::{\n    p,\n};\n\nfn f() {}\n\
                   mod tests {\n    use pkg::q;\n    use sup::*;\n    #[test]\n    fn t() {}\n}\n";
        let want = "// file\n\n/// the a\npub mod a;\npub(pkg) mod m;\n\nmod z;\n\n\
                    #[cfg(unix)]\npub use z::{\n    p,\n};\n\nuse a::x;\nuse b;\n\nfn f() {}\n\
                    mod tests {\n    use sup::*;\n    use pkg::q;\n    #[test]\n    fn t() {}\n}\n";
        assert_eq!(format_source(src).unwrap(), want);
        assert_eq!(format_source(want).unwrap(), want);
        // A comment between items moves with the next one; one on the
        // same line as an item, or an item sharing its line, ends the header.
        let src = "pub mod b;\n// on a\npub mod a;\nuse x; fn f() {}\nuse c;\n";
        let want = "// on a\npub mod a;\npub mod b;\nuse x; fn f() {}\nuse c;\n";
        assert_eq!(format_source(src).unwrap(), want);
        let src = "#!/usr/bin/env nabi\nuse b;\nuse a;\n";
        let want = "#!/usr/bin/env nabi\nuse a;\nuse b;\n";
        assert_eq!(format_source(src).unwrap(), want);
    }

    /// Idempotent and token-preserving (up to header order) over every
    /// `.nabi` in the repo.
    #[test]
    fn repo_sources_round_trip() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut files = Vec::new();
        for dir in ["library", "compiler/nabic/tests"] {
            collect(&root.join(dir), &mut files);
        }
        assert!(files.len() > 50, "found only {} files", files.len());
        let kinds = |s: &str| -> Vec<String> {
            let mut k: Vec<String> = Lexer::new(s, "t")
                .lex_all()
                .0
                .into_iter()
                .map(|t| format!("{:?}", t.kind))
                .collect();
            k.sort();
            k
        };
        for f in files {
            let src = std::fs::read_to_string(&f).unwrap();
            let Ok(once) = format_source(&src) else {
                continue;
            };
            assert_eq!(kinds(&src), kinds(&once), "tokens changed: {}", f.display());
            let twice = format_source(&once).unwrap();
            assert_eq!(once, twice, "not idempotent: {}", f.display());
        }
    }

    fn collect(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                collect(&p, out);
            } else if p.extension().is_some_and(|x| x == "nabi") {
                out.push(p);
            }
        }
    }
}
