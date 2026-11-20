//! The language server: position and URI helpers, then whole sessions
//! driven by a scripted JSON-RPC exchange (front end only: no code
//! generation).

mod actions;
mod completion;
mod hover;
mod lexical;
mod navigate;
mod packages;
mod signature;
mod stale;
mod tokens;

pub(crate) use crate::tests::completion::*;

use crate::serve;
use nabic_diagnostic::Json;
use nabic_tools::cursor::{ident_at, receiver_start};
use nabils_protocol::{offset, path_to_uri, position, uri_to_path};
use std::io::Cursor;
use std::path::Path;

#[test]
fn positions_round_trip_utf16() {
    let text = "ab\né😀x\n";
    let x = text.find('x').unwrap();
    assert_eq!(
        position(text, x),
        Json::obj([("line", Json::num(1)), ("character", Json::num(3))])
    );
    assert_eq!(offset(text, 1, 3), x);
    assert_eq!(offset(text, 9, 0), text.len());
    assert_eq!(ident_at("let foo = 1", 5), Some(("foo", 4)));
    assert_eq!(ident_at("a + 12", 5), None);
    let s = "x = 1 + a.b(f(c))[0].";
    assert_eq!(receiver_start(s, s.len() - 1), s.find('a').unwrap());
    let p = "/tmp/a b/é.nabi";
    assert_eq!(path_to_uri(p, false), "file:///tmp/a%20b/%C3%A9.nabi");
    assert_eq!(uri_to_path(&path_to_uri(p, false), false), p);
}

#[test]
fn windows_uris_name_drives_and_shares() {
    let p = r"C:\Users\é x\main.nabi";
    assert_eq!(
        path_to_uri(p, true),
        "file:///C:/Users/%C3%A9%20x/main.nabi"
    );
    assert_eq!(uri_to_path(&path_to_uri(p, true), true), p);
    // VS Code's spelling.
    assert_eq!(
        uri_to_path("file:///c%3A/Users/x/main.nabi", true),
        r"C:\Users\x\main.nabi"
    );
    let unc = r"\\srv\share\a.nabi";
    assert_eq!(path_to_uri(unc, true), "file://srv/share/a.nabi");
    assert_eq!(uri_to_path("file://srv/share/a.nabi", true), unc);
}

fn frame(msg: &Json) -> String {
    let body = msg.to_string();
    format!("Content-Length: {}\r\n\r\n{body}", body.len())
}

fn request(id: u32, method: &str, params: Json) -> Json {
    Json::obj([
        ("jsonrpc", Json::str("2.0")),
        ("id", Json::num(id)),
        ("method", Json::str(method)),
        ("params", params),
    ])
}

fn notify(method: &str, params: Json) -> Json {
    Json::obj([
        ("jsonrpc", Json::str("2.0")),
        ("method", Json::str(method)),
        ("params", params),
    ])
}

fn doc(uri: &str) -> Json {
    Json::obj([("uri", Json::str(uri))])
}

fn at(uri: &str, line: u32, character: u32) -> Json {
    Json::obj([
        ("textDocument", doc(uri)),
        (
            "position",
            Json::obj([
                ("line", Json::num(line)),
                ("character", Json::num(character)),
            ]),
        ),
    ])
}

fn change(uri: &str, text: &str) -> Json {
    notify(
        "textDocument/didChange",
        Json::obj([
            ("textDocument", doc(uri)),
            (
                "contentChanges",
                Json::Arr(vec![Json::obj([("text", Json::str(text))])]),
            ),
        ]),
    )
}

/// Every framed message of the server's output.
fn messages(out: &[u8]) -> Vec<Json> {
    let mut rest = std::str::from_utf8(out).unwrap();
    let mut msgs = Vec::new();
    while let Some(i) = rest.find("\r\n\r\n") {
        let len: usize = rest[..i]
            .trim_start_matches("Content-Length: ")
            .parse()
            .unwrap();
        msgs.push(Json::parse(&rest[i + 4..i + 4 + len]).unwrap());
        rest = &rest[i + 4 + len..];
    }
    msgs
}

const SRC: &str = "\
#[derive(Debug)]
struct P { x: i32; y: i32; }
impl P {
    fn sum(self) -> i32 { self.x + self.y } }
/// Doubles `n`.
fn twice(n: i32) -> i32 { n * 2 }
let p = P { x: 1, y: 2 };
let s = p.sum();
println(f\"{twice(s)}\");
";

#[test]
fn scripted_session() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let bootstrap = workspace.join("library/core/src/lib.nabi");
    let dir = nabic_core::tmp::test_dir("lsp");
    std::fs::create_dir_all(&dir).unwrap();
    // A lone file: the anonymous package.
    let file = dir.join("demo.nabi");
    let uri = path_to_uri(&file.display().to_string(), cfg!(windows));
    let uri = uri.as_str();

    let completing = format!("{SRC}let t = p.\n");
    let paths = format!("{SRC}core::\n");
    let broken = SRC.replace("n * 2", "n * true");
    let unformatted = "fn f() {\nlet a = 1;\n}\n";
    let script = [
        request(
            1,
            "initialize",
            Json::obj([("capabilities", Json::obj([]))]),
        ),
        notify("initialized", Json::obj([])),
        notify(
            "textDocument/didOpen",
            Json::obj([(
                "textDocument",
                Json::obj([
                    ("uri", Json::str(uri)),
                    ("languageId", Json::str("nabi")),
                    ("version", Json::num(1)),
                    ("text", Json::str(SRC)),
                ]),
            )]),
        ),
        // `p` in `p.sum()`, `twice` in the call, `s` in `twice(s)`.
        request(2, "textDocument/hover", at(uri, 7, 8)),
        request(3, "textDocument/hover", at(uri, 8, 12)),
        request(4, "textDocument/definition", at(uri, 8, 12)),
        request(5, "textDocument/definition", at(uri, 8, 18)),
        request(
            6,
            "textDocument/documentSymbol",
            Json::obj([("textDocument", doc(uri))]),
        ),
        change(uri, &broken),
        // References check the buffer as it is now: `broken` is published.
        request(13, "textDocument/references", at(uri, 7, 8)),
        request(10, "textDocument/hover", at(uri, 7, 8)),
        change(uri, &completing),
        request(7, "textDocument/completion", at(uri, 9, 10)),
        change(uri, &paths),
        request(8, "textDocument/completion", at(uri, 9, 11)),
        change(uri, unformatted),
        request(
            9,
            "textDocument/formatting",
            Json::obj([("textDocument", doc(uri))]),
        ),
        request(11, "workspace/bogus", Json::obj([])),
        request(12, "shutdown", Json::Null),
        notify("exit", Json::Null),
    ];
    let input: String = script.iter().map(frame).collect();
    let mut out = Vec::new();
    let code = serve(
        Cursor::new(input.into_bytes()),
        &mut out,
        vec![bootstrap],
        Vec::new(),
        vec![(nabic_tools::lint::Level::Allow, "dead_code".to_owned())],
    );
    assert_eq!(code, 0);
    let msgs = messages(&out);
    let result = |id: u32| {
        msgs.iter()
            .find(|m| m.get("id") == &Json::num(id))
            .unwrap_or_else(|| panic!("no reply {id}"))
            .get("result")
            .clone()
    };
    let hover = |id| {
        result(id)
            .get("contents")
            .get("value")
            .as_str()
            .unwrap_or("")
            .to_owned()
    };

    let caps = result(1);
    assert_eq!(
        caps.get("capabilities").get("hoverProvider"),
        &Json::Bool(true)
    );
    assert!(hover(2).contains("p: P"), "{}", hover(2));
    assert!(hover(3).contains("fn twice(n: i32) -> i32"), "{}", hover(3));
    assert!(hover(3).ends_with("```\n\nDoubles `n`."), "{}", hover(3));
    let line = |r: &Json| r.get("range").get("start").get("line").clone();
    assert_eq!(line(&result(4)), Json::num(5));
    assert_eq!(result(4).get("uri").as_str(), Some(uri));
    assert_eq!(line(&result(5)), Json::num(7));

    let Json::Arr(symbols) = result(6) else {
        panic!("symbols: {}", result(6))
    };
    let names: Vec<&str> = symbols
        .iter()
        .filter_map(|s| s.get("name").as_str())
        .collect();
    assert_eq!(names, ["P", "impl P", "twice"]);
    let Json::Arr(methods) = symbols[1].get("children").clone() else {
        panic!()
    };
    assert_eq!(methods[0].get("name").as_str(), Some("sum"));

    let labels = |id| match result(id) {
        Json::Arr(items) => items
            .iter()
            .filter_map(|i| i.get("label").as_str().map(str::to_owned))
            .collect::<Vec<_>>(),
        r => panic!("completion {id}: {r}"),
    };
    let members = labels(7);
    for m in ["x", "y", "sum"] {
        assert!(members.iter().any(|l| l == m), "{members:?}");
    }
    assert!(labels(8).iter().any(|l| l == "fmt"), "{:?}", labels(8));

    let Json::Arr(edits) = result(9) else {
        panic!("formatting: {}", result(9))
    };
    assert_eq!(
        edits[0].get("newText").as_str(),
        Some("fn f() {\n    let a = 1;\n}\n")
    );

    // Hover still answers from the last good compile of a broken buffer.
    assert!(hover(10).contains("p: P"), "{}", hover(10));
    let bogus = msgs.iter().find(|m| m.get("id") == &Json::num(11)).unwrap();
    assert_eq!(bogus.get("error").get("code"), &Json::num(-32601));

    // Diagnostics: clean at first, then the type error of `broken`.
    let published: Vec<&Json> = msgs
        .iter()
        .filter(|m| m.get("method").as_str() == Some("textDocument/publishDiagnostics"))
        .map(|m| m.get("params"))
        .collect();
    assert_eq!(published[0].get("diagnostics"), &Json::Arr(Vec::new()));
    let Json::Arr(second) = published[1].get("diagnostics").clone() else {
        panic!()
    };
    let err = second
        .iter()
        .find(|d| d.get("severity") == &Json::num(1))
        .unwrap_or_else(|| panic!("no error in {second:?}"));
    assert_eq!(line(err), Json::num(5));
    // The last buffer type-checks; its lints follow the given levels.
    let Json::Arr(last) = published.last().unwrap().get("diagnostics").clone() else {
        panic!()
    };
    assert_eq!(last.len(), 1, "{last:?}");
    let msg = last[0].get("message").as_str().unwrap_or("");
    assert!(msg.starts_with("unused variable `a`"), "{msg}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A symbol's range covers the whole item; its selection range is the
/// name.
#[test]
fn document_symbol_ranges_cover_item() {
    let src = "\
pub fn dist2(a: i32, b: i32) -> i32 {
    a * a + b * b
}
struct P { x: i32; }
impl P {
    fn get(self) -> i32 { self.x }
}
";
    let msgs = std_session("lsp-symbols", src, |uri| {
        vec![request(
            1,
            "textDocument/documentSymbol",
            Json::obj([("textDocument", doc(uri))]),
        )]
    });
    let Json::Arr(syms) = reply(&msgs, 1) else {
        panic!("{}", reply(&msgs, 1))
    };
    let pos = |r: &Json, end: &str| {
        let p = r.get(end);
        (
            p.get("line").as_u32().unwrap(),
            p.get("character").as_u32().unwrap(),
        )
    };
    let f = &syms[0];
    assert_eq!(pos(f.get("range"), "start"), (0, 0));
    assert_eq!(pos(f.get("range"), "end"), (2, 1));
    assert_eq!(pos(f.get("selectionRange"), "start"), (0, 7));
    assert_eq!(pos(f.get("selectionRange"), "end"), (0, 12));
    assert_eq!(pos(syms[1].get("range"), "end"), (3, 20));
    assert_eq!(pos(syms[2].get("range"), "end"), (6, 1));
    let Json::Arr(methods) = syms[2].get("children").clone() else {
        panic!()
    };
    assert_eq!(pos(methods[0].get("range"), "start"), (5, 4));
    assert_eq!(pos(methods[0].get("range"), "end"), (5, 34));
    assert_eq!(pos(methods[0].get("selectionRange"), "start"), (5, 7));
}

#[test]
fn initialize_advertises_new_capabilities() {
    let msgs = std_session("lsp-caps", "", |_| Vec::new());
    let caps = reply(&msgs, 1000);
    let caps = caps.get("capabilities");
    assert_eq!(
        caps.get("codeActionProvider").get("codeActionKinds"),
        &Json::Arr(vec![Json::str("quickfix")])
    );
    assert_eq!(
        caps.get("signatureHelpProvider").get("triggerCharacters"),
        &Json::Arr(vec![Json::str("("), Json::str(",")])
    );
    assert_eq!(
        caps.get("renameProvider").get("prepareProvider"),
        &Json::Bool(true)
    );
    assert_eq!(
        caps.get("executeCommandProvider").get("commands"),
        &Json::Arr(vec![Json::str("nabi.add")])
    );
}
