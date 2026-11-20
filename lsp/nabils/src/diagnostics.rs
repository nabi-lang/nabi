//! Diagnostics as LSP `Diagnostic`s.

#[allow(clippy::wildcard_imports)]
use crate::*;

/// LSP `Diagnostic` of `d`, whose span is in `text`.
pub(crate) fn diag_json(d: &NabiDiagnostic, uri: &str, text: &str) -> Json {
    let span = d.primary.as_ref().map(|l| l.span).unwrap_or_default();
    let mut message = d.message.clone();
    for n in &d.notes {
        message.push('\n');
        if !n.starts_with("help: ") {
            message.push_str("note: ");
        }
        message.push_str(n);
    }
    // Labels in the same file (`first defined here`, an unclosed
    // delimiter) as related locations.
    let related: Vec<Json> = d
        .secondary
        .iter()
        .filter(|l| l.span.file == span.file && !l.span.is_dummy())
        .map(|l| {
            Json::obj([
                (
                    "location",
                    location_at(uri, text, l.span.start as usize, l.span.end as usize),
                ),
                ("message", Json::str(&l.message)),
            ])
        })
        .collect();
    let (start, end) = (span.start as usize, span.end as usize);
    // Some editors hide an empty range.
    let (start, end) = if start == end && !span.is_dummy() {
        widen(text, start)
    } else {
        (start, end)
    };
    let mut d_json = Json::obj([
        ("range", range(text, start, end)),
        ("severity", Json::num(if d.is_error() { 1 } else { 2 })),
        ("code", Json::str(d.code.to_string())),
        ("source", Json::str("nabic")),
        ("message", Json::str(message)),
        ("relatedInformation", Json::Arr(related)),
    ]);
    // `DiagnosticTag.Deprecated`: editors strike the use through.
    if let (Json::Obj(fields), nabic_diagnostic::Tag::Deprecated) = (&mut d_json, d.tag) {
        fields.push(("tags".to_owned(), Json::Arr(vec![Json::num(2)])));
    }
    d_json
}

/// The token a zero-width range at `at` points at: the next one on its
/// line, else the one before it on the line, else one character.
pub(crate) fn widen(text: &str, at: usize) -> (usize, usize) {
    let at = at.min(text.len());
    let line_start = text[..at].rfind('\n').map_or(0, |i| i + 1);
    let line_end = text[at..].find('\n').map_or(text.len(), |i| at + i);
    let (toks, _) = nabic_lexer::Lexer::new(text, String::new()).lex_all();
    let spans = toks
        .iter()
        .map(|t| (t.span.start as usize, t.span.end as usize))
        .filter(|&(s, e)| s < e && e <= text.len() && !text[s..e].trim().is_empty());
    let mut before = None;
    for (s, e) in spans {
        if s >= at && s < line_end {
            return (s, e);
        }
        if e <= at && s >= line_start {
            before = Some((s, e));
        }
    }
    before.unwrap_or_else(|| match text[at..].chars().next() {
        Some(c) if c != '\n' => (at, at + c.len_utf8()),
        _ => (at, at),
    })
}
