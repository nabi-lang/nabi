//! Members, definitions, references and renames.

#[allow(clippy::wildcard_imports)]
use crate::tests::*;

pub(crate) const MEMBERS: &str = "\
struct P {
    /// The x coordinate.
    x: i32;
    y: i32;
}
impl P {
    /// Sum of the coordinates.
    fn sum(self) -> i32 { self.x + self.y }
}
let p = P { x: 1, y: 2 };
let total = p.sum() + p.x;
let u = total * 2;
println(f\"{u}\");
";

/// Hover and definition of fields and methods, name ranges,
/// references and rename.
#[test]
fn members_references_rename() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let bootstrap = workspace.join("library/core/src/lib.nabi");
    let dir = nabic_core::tmp::test_dir("lsp-members");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("members.nabi");
    let uri = path_to_uri(&file.display().to_string(), cfg!(windows));
    let uri = uri.as_str();
    let rename = |id, line, col, name: &str| {
        let mut p = at(uri, line, col);
        if let Json::Obj(fields) = &mut p {
            fields.push(("newName".to_owned(), Json::str(name)));
        }
        request(id, "textDocument/rename", p)
    };
    let script = [
        request(
            1,
            "initialize",
            Json::obj([("capabilities", Json::obj([]))]),
        ),
        notify(
            "textDocument/didOpen",
            Json::obj([(
                "textDocument",
                Json::obj([
                    ("uri", Json::str(uri)),
                    ("languageId", Json::str("nabi")),
                    ("version", Json::num(1)),
                    ("text", Json::str(MEMBERS)),
                ]),
            )]),
        ),
        // `sum` and `x` in `p.sum() + p.x`, `total` in `total * 2`.
        request(2, "textDocument/hover", at(uri, 10, 15)),
        request(3, "textDocument/hover", at(uri, 10, 24)),
        request(4, "textDocument/definition", at(uri, 10, 15)),
        request(5, "textDocument/definition", at(uri, 10, 24)),
        request(6, "textDocument/definition", at(uri, 11, 9)),
        request(7, "textDocument/references", at(uri, 11, 9)),
        request(8, "textDocument/references", at(uri, 10, 24)),
        rename(9, 11, 9, "sum2"),
        rename(10, 11, 9, "fn"),
        rename(11, 12, 1, "say"),
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
        Vec::new(),
    );
    assert_eq!(code, 0);
    let msgs = messages(&out);
    let reply = |id: u32| {
        msgs.iter()
            .find(|m| m.get("id") == &Json::num(id))
            .unwrap_or_else(|| panic!("no reply {id}"))
            .clone()
    };
    let result = |id: u32| reply(id).get("result").clone();
    let hover = |id| {
        result(id)
            .get("contents")
            .get("value")
            .as_str()
            .unwrap_or("")
            .to_owned()
    };
    let start = |r: &Json| {
        let s = r.get("range").get("start");
        (s.get("line").clone(), s.get("character").clone())
    };
    let pos = |l: u32, c: u32| (Json::num(l), Json::num(c));

    assert!(hover(2).contains("fn sum(self: P) -> i32"), "{}", hover(2));
    assert!(
        hover(2).ends_with("Sum of the coordinates."),
        "{}",
        hover(2)
    );
    assert!(hover(3).contains("P.x: i32"), "{}", hover(3));
    assert!(hover(3).ends_with("The x coordinate."), "{}", hover(3));
    // Name ranges, not whole items.
    assert_eq!(start(&result(4)), pos(7, 7));
    assert_eq!(start(&result(5)), pos(2, 4));
    assert_eq!(start(&result(6)), pos(10, 4));

    let starts = |id| match result(id) {
        Json::Arr(locs) => locs.iter().map(start).collect::<Vec<_>>(),
        r => panic!("references {id}: {r}"),
    };
    assert_eq!(starts(7), [pos(10, 4), pos(11, 8)]);
    // `.x` reads, the declaration and the literal's `x:`.
    assert_eq!(starts(8), [pos(2, 4), pos(7, 31), pos(9, 12), pos(10, 24)]);

    let Json::Arr(edits) = result(9).get("changes").get(uri).clone() else {
        panic!("rename: {}", reply(9))
    };
    assert_eq!(edits.len(), 2);
    assert_eq!(edits[0].get("newText").as_str(), Some("sum2"));
    assert!(
        matches!(reply(10).get("error"), Json::Obj(_)),
        "{}",
        reply(10)
    );
    // `println` is declared in the library.
    assert!(
        matches!(reply(11).get("error"), Json::Obj(_)),
        "{}",
        reply(11)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Definition of a method goes by the receiver's type, and of a library
/// function to its source.
#[test]
fn definition_by_receiver_type() {
    let msgs = std_session("lsp-def-recv", LIST, |uri| {
        vec![
            request(1, "textDocument/definition", at(uri, 1, 4)),
            request(2, "textDocument/definition", at(uri, 2, 14)),
        ]
    });
    let uri = |id| {
        reply(&msgs, id)
            .get("uri")
            .as_str()
            .unwrap_or("")
            .to_owned()
    };
    assert!(
        uri(1).ends_with("collections/list.nabi"),
        "{}",
        reply(&msgs, 1)
    );
    assert!(
        uri(2).ends_with("collections/list.nabi"),
        "{}",
        reply(&msgs, 2)
    );
}

#[test]
fn definition_println_not_null() {
    let msgs = std_session("lsp-def-println", "println(\"hi\");\n", |uri| {
        vec![request(1, "textDocument/definition", at(uri, 0, 2))]
    });
    let got = reply(&msgs, 1);
    assert!(
        got.get("uri")
            .as_str()
            .is_some_and(|u| u.ends_with("print.nabi")),
        "{got}"
    );
}

#[test]
fn prepare_rename_local_ok_library_refused() {
    let src = "let count = 1;\nprintln(f\"{count}\");\n";
    let msgs = std_session("lsp-prepare-rename", src, |uri| {
        vec![
            request(1, "textDocument/prepareRename", at(uri, 0, 6)),
            request(2, "textDocument/prepareRename", at(uri, 1, 2)),
            request(3, "textDocument/prepareRename", at(uri, 0, 1)),
        ]
    });
    let ok = reply(&msgs, 1);
    assert_eq!(ok.get("placeholder").as_str(), Some("count"));
    assert_eq!(ok.get("range").get("start").get("character"), &Json::num(4));
    let error = |id| {
        msgs.iter()
            .find(|m| m.get("id") == &Json::num(id))
            .unwrap()
            .get("error")
            .get("message")
            .as_str()
            .unwrap_or("")
            .to_owned()
    };
    assert_eq!(
        error(2),
        "cannot rename `println`: it is defined in the standard library"
    );
    assert_eq!(error(3), "`let` is a keyword");
}

/// A redeclared name is two bindings: hover, definition, references
/// and rename keep them apart.
#[test]
fn redeclared_local_is_its_own_binding() {
    let src = "let n = \"12\";\nlet n: i32 = try! n.parse();\nprintln(f\"{n}\");\n";
    let msgs = std_session("lsp-redeclared", src, |uri| {
        let mut rename = at(uri, 1, 4);
        if let Json::Obj(fields) = &mut rename {
            fields.push(("newName".to_owned(), Json::str("m")));
        }
        vec![
            request(1, "textDocument/hover", at(uri, 0, 4)),
            request(2, "textDocument/hover", at(uri, 1, 4)),
            request(3, "textDocument/hover", at(uri, 1, 18)),
            request(4, "textDocument/definition", at(uri, 1, 18)),
            request(5, "textDocument/definition", at(uri, 2, 11)),
            request(6, "textDocument/references", at(uri, 0, 4)),
            request(7, "textDocument/rename", rename),
        ]
    });
    let hover = |id| {
        reply(&msgs, id)
            .get("contents")
            .get("value")
            .as_str()
            .unwrap_or("")
            .to_owned()
    };
    assert!(hover(1).contains("n: str"), "{}", hover(1));
    assert!(hover(2).contains("n: i32"), "{}", hover(2));
    assert!(hover(3).contains("n: str"), "{}", hover(3));
    let start = |r: &Json| {
        let s = r.get("range").get("start");
        (s.get("line").clone(), s.get("character").clone())
    };
    let pos = |l: u32, c: u32| (Json::num(l), Json::num(c));
    assert_eq!(start(&reply(&msgs, 4)), pos(0, 4));
    assert_eq!(start(&reply(&msgs, 5)), pos(1, 4));
    let Json::Arr(refs) = reply(&msgs, 6) else {
        panic!("references: {}", reply(&msgs, 6))
    };
    let refs: Vec<_> = refs.iter().map(start).collect();
    assert_eq!(refs, [pos(0, 4), pos(1, 18)]);
    let Json::Obj(changes) = reply(&msgs, 7).get("changes").clone() else {
        panic!("rename: {}", reply(&msgs, 7))
    };
    let Json::Arr(edits) = changes[0].1.clone() else {
        panic!("rename: {}", reply(&msgs, 7))
    };
    let edits: Vec<_> = edits.iter().map(start).collect();
    assert_eq!(edits, [pos(1, 4), pos(2, 11)]);
}
