//! Characters and literals checked as the buffer changes: the
//! compiler's diagnostic, its quick fix, and a clean check after it.

use crate::tests::actions::{actions, code_action, published, titled};
#[allow(clippy::wildcard_imports)]
use crate::tests::*;

/// `src` with the one edit of `action` applied.
fn apply(src: &str, action: &Json) -> String {
    let Json::Obj(changes) = action.get("edit").get("changes") else {
        panic!("{action}")
    };
    let Json::Arr(edits) = &changes[0].1 else {
        panic!("{action}")
    };
    let at = |p: &Json| {
        offset(
            src,
            p.get("line").as_u32().unwrap(),
            p.get("character").as_u32().unwrap(),
        )
    };
    let r = edits[0].get("range");
    let (start, end) = (at(r.get("start")), at(r.get("end")));
    let new = edits[0].get("newText").as_str().unwrap();
    format!("{}{new}{}", &src[..start], &src[end..])
}

/// Open `src`, take the quick fix titled `title` on `line`, and check
/// that it gives `fixed` and that `fixed` checks clean once changed in:
/// the diagnostics published on the change, not on a save.
fn fix_round_trip(name: &str, src: &str, line: u32, code: &str, title: &str, fixed: &str) {
    let msgs = std_session(name, src, |uri| {
        vec![
            code_action(1, uri, line),
            change(uri, fixed),
            request(2, "textDocument/hover", at(uri, 0, 0)),
        ]
    });
    let first = published_batches(&msgs);
    let diag = first[0]
        .iter()
        .find(|d| d.get("code").as_str() == Some(code))
        .unwrap_or_else(|| panic!("no {code} in {:?}", first[0]));
    let range = diag.get("range");
    assert_eq!(
        range.get("start").get("line").as_u32(),
        Some(line),
        "{diag}"
    );
    let got = actions(&msgs, 1);
    assert_eq!(apply(src, titled(&got, title)), fixed);
    let last = first.last().unwrap();
    assert!(last.is_empty(), "{fixed:?} still has {last:?}");
}

/// The diagnostics of each `publishDiagnostics`, in order.
fn published_batches(msgs: &[Json]) -> Vec<Vec<Json>> {
    msgs.iter()
        .filter(|m| m.get("method").as_str() == Some("textDocument/publishDiagnostics"))
        .map(|m| match m.get("params").get("diagnostics") {
            Json::Arr(v) => v.clone(),
            _ => Vec::new(),
        })
        .collect()
}

#[test]
fn lookalike_minus() {
    fix_round_trip(
        "lsp-lex-minus",
        "let d = 3 \u{2212} 1;\nprintln(f\"{d}\");\n",
        0,
        "E0148",
        "write `-`",
        "let d = 3 - 1;\nprintln(f\"{d}\");\n",
    );
}

#[test]
fn curly_quotes() {
    fix_round_trip(
        "lsp-lex-quotes",
        "println(\u{201C}hi\u{201D});\n",
        0,
        "E0148",
        "write `\"hi\"`",
        "println(\"hi\");\n",
    );
}

#[test]
fn invisible_in_name() {
    fix_round_trip(
        "lsp-lex-zwsp",
        "let ab = 1;\nprintln(f\"{a\u{200B}b}\");\n",
        1,
        "E0147",
        "write `ab`",
        "let ab = 1;\nprintln(f\"{ab}\");\n",
    );
}

#[test]
fn invisible_in_string() {
    fix_round_trip(
        "lsp-lex-zwsp-str",
        "println(\"a\u{200B}b\");\n",
        0,
        "E0131",
        "write `\\u{200B}`",
        "println(\"a\\u{200B}b\");\n",
    );
}

#[test]
fn bidi_in_comment() {
    fix_round_trip(
        "lsp-lex-bidi",
        "// x \u{202E} y\nprintln(\"a\");\n",
        0,
        "E0002",
        "delete the bidirectional control character",
        "// x  y\nprintln(\"a\");\n",
    );
}

const HZ: &str = "#[literal(number \"hz\")]\nconst fn hz(n: f64) -> f64 { n }\n";

#[test]
fn suffix_case() {
    fix_round_trip(
        "lsp-lex-hz",
        &format!("{HZ}println(10Hz);\n"),
        2,
        "E0114",
        "write `10hz`",
        &format!("{HZ}println(10hz);\n"),
    );
}

#[test]
fn percent_suffix() {
    let pct = "#[literal(number \"pct\")]\nconst fn pct(n: f64) -> f64 { n / 100.0 }\n";
    fix_round_trip(
        "lsp-lex-pct",
        &format!("{pct}println(95%);\n"),
        2,
        "E0114",
        "write `95pct`",
        &format!("{pct}println(95pct);\n"),
    );
}

#[test]
fn string_prefix() {
    let date = "#[literal(\"date\" string)]\nconst fn date(s: str) -> str { s }\n";
    fix_round_trip(
        "lsp-lex-prefix",
        &format!("{date}println(dat\"2024-01-15\");\n"),
        2,
        "E0114",
        "write `date`",
        &format!("{date}println(date\"2024-01-15\");\n"),
    );
}

#[test]
fn bad_escape() {
    fix_round_trip(
        "lsp-lex-escape",
        "println(\"a\\qb\");\n",
        0,
        "E0006",
        "write `\\\\q`",
        "println(\"a\\\\qb\");\n",
    );
}

/// A symbol registered as a suffix: the error sits on the attribute
/// while it is typed.
#[test]
fn symbol_suffix_registration() {
    let src = "#[literal(number \"\u{B0}\")]\nconst fn deg(n: f64) -> f64 { n }\n";
    let msgs = std_session("lsp-lex-symbol", src, |uri| vec![code_action(1, uri, 0)]);
    let diags = published(&msgs);
    let d = diags
        .iter()
        .find(|d| d.get("code").as_str() == Some("E0114"))
        .unwrap_or_else(|| panic!("{diags:?}"));
    let msg = d.get("message").as_str().unwrap();
    assert!(msg.contains("is a symbol, not a letter"), "{msg}");
    assert!(msg.contains("`#[literal(number \"deg\")]`"), "{msg}");
    assert_eq!(d.get("range").get("start").get("line").as_u32(), Some(0));
}
