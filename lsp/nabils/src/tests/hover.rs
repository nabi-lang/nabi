//! Hover.

#[allow(clippy::wildcard_imports)]
use crate::tests::*;

// Hovering a `name!(..)` call shows its expansion.
#[test]
fn hover_expands_macro_call() {
    let dir = nabic_core::tmp::test_dir("lsp-mhover");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("h.nabi");
    let uri = path_to_uri(&file.display().to_string(), cfg!(windows));
    let uri = uri.as_str();
    let src = "#[impl_macro(dbl)] pub @macro { (@e: expr) => { (@e, @e) } }\n\
               fn f() {\n    let x = dbl!(1i32);\n}\n";
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
                    ("text", Json::str(src)),
                ]),
            )]),
        ),
        // cursor on `dbl` in `dbl!(1i32)` (line 2, inside the name)
        request(2, "textDocument/hover", at(uri, 2, 13)),
        request(3, "shutdown", Json::Null),
        notify("exit", Json::Null),
    ];
    let input: String = script.iter().map(frame).collect();
    let mut out = Vec::new();
    let code = serve(
        Cursor::new(input.into_bytes()),
        &mut out,
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );
    assert_eq!(code, 0);
    let msgs = messages(&out);
    let value = msgs
        .iter()
        .find(|m| m.get("id") == &Json::num(2))
        .unwrap()
        .get("result")
        .get("contents")
        .get("value")
        .as_str()
        .unwrap_or("")
        .to_owned();
    assert!(
        value.contains("1i32") && value.contains("```nabi"),
        "{value}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Hover on a library method and function: its signature, then its doc.
#[test]
fn hover_library_signature_and_doc() {
    let src = "let xs: List<i32> = [1];\nxs.extend([2, 3]);\nprintln(\"a\");\n";
    let msgs = std_session("lsp-hover-lib", src, |uri| {
        vec![
            request(1, "textDocument/hover", at(uri, 1, 5)),
            request(2, "textDocument/hover", at(uri, 2, 3)),
        ]
    });
    let value = |id| {
        reply(&msgs, id)
            .get("contents")
            .get("value")
            .as_str()
            .unwrap_or("")
            .to_owned()
    };
    assert!(value(1).contains("fn extend("), "{}", value(1));
    assert!(value(1).contains("Push every item"), "{}", value(1));
    assert!(value(2).contains("fn println("), "{}", value(2));
}

/// A `-> some Tr` function hovers as written, and so do the values it
/// returns: the concrete type stays hidden.
#[test]
fn hover_some_return_type() {
    let src = "\
fn evens(n: i32) -> some Iterator<Item = i32> {
    (0..n).filter(|x| x % 2 == 0)
}
let it = evens(4);
println(it.count());
";
    let msgs = std_session("lsp-hover-some", src, |uri| {
        vec![
            request(1, "textDocument/hover", at(uri, 3, 10)),
            request(2, "textDocument/hover", at(uri, 4, 9)),
        ]
    });
    let value = |id| {
        reply(&msgs, id)
            .get("contents")
            .get("value")
            .as_str()
            .unwrap_or("")
            .to_owned()
    };
    assert!(
        value(1).contains("-> some Iterator<Item = i32>"),
        "{}",
        value(1)
    );
    assert!(
        value(2).contains("it: some Iterator<Item = i32>"),
        "{}",
        value(2)
    );
}

/// A named `some` type hovers as its name: its defining function, the
/// values it gives, and the alias itself as declared.
#[test]
fn hover_named_some_type() {
    let src = "\
type Evens = some Iterator<Item = i32>;
fn evens(n: i32) -> Evens {
    (0..n).filter(|x| x % 2 == 0)
}
let it = evens(4);
println(it.count());
";
    let msgs = std_session("lsp-hover-named-some", src, |uri| {
        vec![
            request(1, "textDocument/hover", at(uri, 4, 10)),
            request(2, "textDocument/hover", at(uri, 5, 9)),
            request(3, "textDocument/hover", at(uri, 0, 6)),
        ]
    });
    let value = |id| {
        reply(&msgs, id)
            .get("contents")
            .get("value")
            .as_str()
            .unwrap_or("")
            .to_owned()
    };
    assert!(
        value(1).contains("fn evens(n: i32) -> Evens"),
        "{}",
        value(1)
    );
    assert!(value(2).contains("it: Evens"), "{}", value(2));
    assert!(
        value(3).contains("type Evens = some Iterator<Item = i32>"),
        "{}",
        value(3)
    );
}

/// A `some Tr` parameter hovers as written, at the call and the definition.
#[test]
fn hover_some_param() {
    let src = "\
fn show(x: some Display) -> str {
    f\"{x}\"
}
println(show(5));
";
    let msgs = std_session("lsp-hover-some-param", src, |uri| {
        vec![
            request(1, "textDocument/hover", at(uri, 3, 9)),
            request(2, "textDocument/hover", at(uri, 0, 4)),
        ]
    });
    let value = |id| {
        reply(&msgs, id)
            .get("contents")
            .get("value")
            .as_str()
            .unwrap_or("")
            .to_owned()
    };
    assert!(
        value(1).contains("fn show(x: some Display) -> str"),
        "{}",
        value(1)
    );
    assert!(
        value(2).contains("fn show(x: some Display) -> str"),
        "{}",
        value(2)
    );
}

/// A buffer with a type error from the start hovers from what its check
/// lowered: the names that typed.
#[test]
fn hover_with_type_error() {
    let src = "\
fn twice(n: i32) -> i32 { n * 2 }
let k = twice(3);
let bad: i32 = \"x\";
println(k);
";
    let msgs = std_session("lsp-hover-partial", src, |uri| {
        vec![
            request(1, "textDocument/hover", at(uri, 3, 8)),
            request(2, "textDocument/hover", at(uri, 1, 9)),
        ]
    });
    let value = |id| {
        reply(&msgs, id)
            .get("contents")
            .get("value")
            .as_str()
            .unwrap_or("")
            .to_owned()
    };
    assert!(value(1).contains("k: i32"), "{}", value(1));
    assert!(value(2).contains("fn twice(n: i32) -> i32"), "{}", value(2));
}
