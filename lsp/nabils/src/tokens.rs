//! Semantic tokens.

#[allow(clippy::wildcard_imports)]
use crate::*;

/// LSP semantic tokens (delta-encoded) for what the grammar cannot know:
/// macro calls `name!(..)` (type `macro`) and user-defined literal
/// suffixes `123suffix` (type `decorator`). `ops`: the operator tokens of
/// the package and its dependencies.
pub(crate) fn semantic_tokens(text: &str, ops: &[String]) -> Vec<u32> {
    let (raw, _) = Lexer::new(text, "").with_ext_ops(ops.to_vec()).lex_all();
    // This buffer's own `#[infix/prefix/postfix("X")]`, as edited, valid
    // or not: re-lex with them so their uses tokenize as operators.
    let mut all: Vec<String> = raw
        .windows(4)
        .filter_map(|w| {
            let (
                TokenKind::HashLBracket,
                TokenKind::Ident(a),
                TokenKind::LParen,
                TokenKind::StrLit(s),
            ) = (&w[0].kind, &w[1].kind, &w[2].kind, &w[3].kind)
            else {
                return None;
            };
            let s = nabic_lexer::escape::unescape(s, nabic_lexer::escape::EscapeMode::Str).ok()?;
            matches!(a.as_str(), "infix" | "prefix" | "postfix").then_some(s)
        })
        .collect();
    all.extend(ops.iter().cloned());
    let (toks, _) = Lexer::new(text, "").with_ext_ops(all).lex_all();
    // User-defined literal suffixes registered in this file by
    // `#[literal(number "x")]` - only these are highlighted, so
    // `5kg` lights up when the user defined `kg`, not for any stray suffix.
    let suffixes: nabic_core::HashSet<String> = nabic_parser::collect_literals(&toks, "")
        .iter()
        .filter_map(|d| match d.tokens.as_slice() {
            [a, b] if matches!(&a.kind, TokenKind::Ident(n) if n == "number") => match &b.kind {
                TokenKind::StrLit(s) => Some(s.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect();
    // User-defined keywords (#[keyword]) and derives / attributes (meta
    // blocks); the macro / operator / literal forms are recognised from the
    // token stream directly.
    let keywords: nabic_core::HashSet<String> =
        nabic_parser::collect_keywords(&parse_doc(text, "").items, "")
            .into_iter()
            .map(|k| k.name)
            .collect();
    let metas = nabic_parser::collect_meta_blocks(&toks, "");
    let is_kind =
        |want: nabic_parser::MetaKind, n: &str| metas.iter().any(|d| d.kind == want && d.name == n);
    // Token type indices in the legend: macro, decorator, keyword, operator.
    let mut spans: Vec<(usize, usize, u32)> = Vec::new();
    let span =
        |t: &nabic_lexer::Token| (t.span.start as usize, (t.span.end - t.span.start) as usize);
    for (i, t) in toks.iter().enumerate() {
        match &t.kind {
            // A macro call `name!( .. )`.
            TokenKind::Ident(_)
                if toks.get(i + 1).map(|x| &x.kind) == Some(&TokenKind::Bang)
                    && matches!(
                        toks.get(i + 2).map(|x| &x.kind),
                        Some(TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace)
                    ) =>
            {
                let start = t.span.start as usize;
                let end = toks[i + 1].span.end as usize; // through the `!`
                spans.push((start, end - start, 0));
            }
            // A user-defined keyword (#[keyword(..)]).
            TokenKind::Ident(n) if !t.raw && keywords.contains(n) => {
                let (o, l) = span(t);
                spans.push((o, l, 2));
            }
            // A user-defined literal suffix (#[literal(number "x")]).
            TokenKind::IntLit(s) | TokenKind::FloatLit(s) => {
                if let Some(sfx) = nabic_parser::user_suffix(s) {
                    if suffixes.contains(sfx) {
                        let end = t.span.end as usize;
                        spans.push((end - sfx.len(), sfx.len(), 1));
                    }
                }
            }
            // A user-defined operator (#[infix] / #[prefix] / #[postfix]):
            // an extension operator token is user-defined by construction.
            TokenKind::ExtOp(_) => {
                let (o, l) = span(t);
                spans.push((o, l, 3));
            }
            // `#[derive(X, ..)]` / `#[attr]`: light up user-defined names.
            TokenKind::HashLBracket | TokenKind::HashBangLBracket => {
                if let Some(close) = balanced_close(&toks, i) {
                    if let Some(TokenKind::Ident(n)) = toks.get(i + 1).map(|x| &x.kind) {
                        if is_kind(nabic_parser::MetaKind::Attribute, n) {
                            let (o, l) = span(&toks[i + 1]);
                            spans.push((o, l, 1));
                        }
                    }
                    for x in &toks[i + 1..close] {
                        if let TokenKind::Ident(n) = &x.kind {
                            if is_kind(nabic_parser::MetaKind::Derive, n) {
                                let (o, l) = span(x);
                                spans.push((o, l, 1));
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    spans.sort_by_key(|s| s.0);
    spans.dedup();
    let mut data = Vec::with_capacity(spans.len() * 5);
    let (mut pl, mut pc) = (0u32, 0u32);
    for (off, len, ty) in spans {
        let (l, c) = line_col(text, off);
        let (dl, dc) = (l - pl, if l == pl { c - pc } else { c });
        let len16 = text[off..off + len]
            .chars()
            .map(|c| c.len_utf16() as u32)
            .sum();
        data.extend([dl, dc, len16, ty, 0]);
        (pl, pc) = (l, c);
    }
    data
}
