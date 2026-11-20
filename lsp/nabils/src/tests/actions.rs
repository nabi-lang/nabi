//! Diagnostics and code actions.

#[allow(clippy::wildcard_imports)]
use crate::tests::*;

/// A parse error's range is never empty: it covers a token.
#[test]
fn parse_diag_has_width() {
    let msgs = std_session("lsp-diag-width", "let a = 1\nlet b = a;\n", |uri| {
        vec![request(1, "textDocument/hover", at(uri, 0, 4))]
    });
    let diags: Vec<Json> = msgs
        .iter()
        .filter(|m| m.get("method").as_str() == Some("textDocument/publishDiagnostics"))
        .flat_map(|m| match m.get("params").get("diagnostics") {
            Json::Arr(v) => v.clone(),
            _ => Vec::new(),
        })
        .collect();
    assert!(!diags.is_empty());
    for d in diags {
        let r = d.get("range");
        assert_ne!(r.get("start"), r.get("end"), "{d}");
    }
}

/// A use of a deprecated item carries `DiagnosticTag.Deprecated` (2);
/// other warnings carry no tags.
#[test]
fn deprecated_tag_sent() {
    let src =
        "#[deprecated(\"use g\")]\nfn f() -> i32 { 1 }\nfn main() { let a = 1; println(f()); }\n";
    let msgs = std_session("lsp-deprecated-tag", src, |uri| {
        vec![request(1, "textDocument/hover", at(uri, 1, 4))]
    });
    let diags: Vec<Json> = msgs
        .iter()
        .filter(|m| m.get("method").as_str() == Some("textDocument/publishDiagnostics"))
        .flat_map(|m| match m.get("params").get("diagnostics") {
            Json::Arr(v) => v.clone(),
            _ => Vec::new(),
        })
        .collect();
    let tags = |want: &str| {
        let d = diags
            .iter()
            .find(|d| d.get("message").as_str().is_some_and(|m| m.contains(want)))
            .unwrap_or_else(|| panic!("no {want:?} in {diags:?}"));
        d.get("tags").clone()
    };
    assert_eq!(
        tags("use of deprecated function `f`"),
        Json::Arr(vec![Json::num(2)])
    );
    assert_eq!(tags("unused variable `a`"), Json::Null);
}

/// `textDocument/codeAction` over `line` with no diagnostics in its
/// context: the ones published there.
pub(crate) fn code_action(id: u32, uri: &str, line: u32) -> Json {
    let pos = |c: u32| Json::obj([("line", Json::num(line)), ("character", Json::num(c))]);
    request(
        id,
        "textDocument/codeAction",
        Json::obj([
            ("textDocument", doc(uri)),
            ("range", Json::obj([("start", pos(0)), ("end", pos(200))])),
            (
                "context",
                Json::obj([("diagnostics", Json::Arr(Vec::new()))]),
            ),
        ]),
    )
}

pub(crate) fn actions(msgs: &[Json], id: u32) -> Vec<Json> {
    match reply(msgs, id) {
        Json::Arr(v) => v,
        r => panic!("code actions {id}: {r}"),
    }
}

pub(crate) fn titled<'a>(got: &'a [Json], title: &str) -> &'a Json {
    got.iter()
        .find(|a| a.get("title").as_str() == Some(title))
        .unwrap_or_else(|| panic!("no `{title}` in {got:?}"))
}

/// The one edit of a quick fix: its start and new text.
pub(crate) fn only_edit(action: &Json) -> ((u32, u32), String) {
    let Json::Obj(changes) = action.get("edit").get("changes") else {
        panic!("{action}")
    };
    let Json::Arr(edits) = &changes[0].1 else {
        panic!("{action}")
    };
    assert_eq!(edits.len(), 1, "{action}");
    let start = edits[0].get("range").get("start");
    (
        (
            start.get("line").as_u32().unwrap(),
            start.get("character").as_u32().unwrap(),
        ),
        edits[0].get("newText").as_str().unwrap().to_owned(),
    )
}

pub(crate) fn published(msgs: &[Json]) -> Vec<Json> {
    msgs.iter()
        .filter(|m| m.get("method").as_str() == Some("textDocument/publishDiagnostics"))
        .flat_map(|m| match m.get("params").get("diagnostics") {
            Json::Arr(v) => v.clone(),
            _ => Vec::new(),
        })
        .collect()
}

/// A missing `;` from its help line, asked by range and with the
/// published diagnostic in the context.
#[test]
fn code_action_adds_semicolon() {
    let src = "let a = 1\nlet b = a;\n";
    let msgs = std_session("lsp-action-semi", src, |uri| vec![code_action(1, uri, 0)]);
    let got = actions(&msgs, 1);
    let semi = titled(&got, "add `;`");
    assert_eq!(semi.get("kind").as_str(), Some("quickfix"));
    assert_eq!(only_edit(semi), ((0, 9), ";".to_owned()));
    let diag = published(&msgs).remove(0);
    let msgs = std_session("lsp-action-semi-ctx", src, |uri| {
        vec![request(
            1,
            "textDocument/codeAction",
            Json::obj([
                ("textDocument", doc(uri)),
                ("range", diag.get("range").clone()),
                (
                    "context",
                    Json::obj([("diagnostics", Json::Arr(vec![diag.clone()]))]),
                ),
            ]),
        )]
    });
    let titles: Vec<_> = actions(&msgs, 1)
        .iter()
        .filter_map(|a| a.get("title").as_str().map(str::to_owned))
        .collect();
    assert_eq!(titles, ["add `;`"]);
}

/// A numeric mismatch: the diagnostic's own fix.
#[test]
fn code_action_as_cast() {
    let src = "let x: i64 = 1;\nlet y: i32 = x;\nprintln(f\"{y}\");\n";
    let msgs = std_session("lsp-action-cast", src, |uri| vec![code_action(1, uri, 1)]);
    let got = actions(&msgs, 1);
    let cast = titled(&got, "convert with `as i32`");
    assert_eq!(only_edit(cast), ((1, 13), "x as i32".to_owned()));
}

/// An unknown name the library has: the `use` goes after the last one.
#[test]
fn code_action_adds_use() {
    let src = "\
use std::env;
let s = read_to_str(\"a\");
println(f\"{env::args().len()}\");
";
    let msgs = std_session("lsp-action-use", src, |uri| vec![code_action(1, uri, 1)]);
    let got = actions(&msgs, 1);
    let add = titled(&got, "add `use std::fs::read_to_str;`");
    assert_eq!(
        only_edit(add),
        ((1, 0), "use std::fs::read_to_str;\n".to_owned())
    );
}

/// An import of a package the script lacks: a `nabi.add` command.
#[test]
fn code_action_nabi_add_command() {
    let msgs = std_session("lsp-action-nabi-add", "use json::parse;\n", |uri| {
        vec![code_action(1, uri, 0)]
    });
    let got = actions(&msgs, 1);
    let cmd = titled(&got, "run `nabi add json`").get("command");
    assert_eq!(cmd.get("command").as_str(), Some("nabi.add"));
    let Json::Arr(args) = cmd.get("arguments") else {
        panic!("{cmd}")
    };
    let ends = |i: usize| args[i].as_str().is_some_and(|u| u.ends_with("main.nabi"));
    assert!(ends(0), "{cmd}");
    assert_eq!(args[1].as_str(), Some("json"));
    // A script: `nabi add` edits this file's `//! nabion` block.
    assert!(ends(2), "{cmd}");
}
