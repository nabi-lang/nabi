//! Semantic tokens.

#[allow(clippy::wildcard_imports)]
use crate::tests::*;

// The server highlights macro calls and user-defined literal suffixes via
// semantic tokens, which the static grammar cannot.
#[test]
fn semantic_tokens_macros_and_literals() {
    let dir = nabic_core::tmp::test_dir("lsp-sem");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("s.nabi");
    let uri = path_to_uri(&file.display().to_string(), cfg!(windows));
    let uri = uri.as_str();
    // `kg` is a registered user literal; `zz` is not (so it stays plain).
    let src = "#[literal(number \"kg\")] fn kg(n: Int) -> i32 { 0 }\n\
               fn f() {\n    let a = foo!(1);\n    let b = 5kg;\n    let c = 9zz;\n}\n";
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
        request(
            2,
            "textDocument/semanticTokens/full",
            Json::obj([("textDocument", doc(uri))]),
        ),
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
    // The legend is advertised.
    let caps = msgs
        .iter()
        .find(|m| m.get("id") == &Json::num(1))
        .unwrap()
        .get("result")
        .get("capabilities")
        .get("semanticTokensProvider")
        .get("legend")
        .get("tokenTypes")
        .to_string();
    assert!(
        caps.contains("macro") && caps.contains("decorator"),
        "{caps}"
    );
    // Two tokens: the `foo!` call (type 0) and the `px` suffix (type 1).
    let data = match msgs
        .iter()
        .find(|m| m.get("id") == &Json::num(2))
        .unwrap()
        .get("result")
        .get("data")
    {
        Json::Arr(v) => v.iter().filter_map(Json::as_u32).collect::<Vec<_>>(),
        other => panic!("not an array: {other}"),
    };
    assert_eq!(data.len(), 10, "{data:?}");
    assert_eq!(data[3], 0, "macro token type: {data:?}");
    assert_eq!(data[8], 1, "literal token type: {data:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

// Every user-defined construct the static grammar cannot know is lit up:
// macro call, literal suffix, keyword, operator, derive, attribute.
#[test]
fn semantic_tokens_all_user_defined() {
    let dir = nabic_core::tmp::test_dir("lsp-sem2");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("s2.nabi");
    let uri = path_to_uri(&file.display().to_string(), cfg!(windows));
    let uri = uri.as_str();
    let src = concat!(
        "#[literal(number \"kg\")] fn kg(n: i32) -> i32 { return 0; }\n",
        "#[keyword(\"mykw\")] fn myh() {}\n",
        "#[infix(\"\u{2295}\")] fn circ() {}\n",
        "#[impl_derive(Foo)] pub @derive { impl Foo for @Self {} }\n",
        "#[impl_attribute(bar)] pub @attribute { @item }\n",
        "#[derive(Foo)]\n#[bar]\nstruct S { x: i32; }\n",
        "fn g() { let a = mac!(1); let b = 5kg; let c = 1 \u{2295} 2; mykw; }\n",
    );
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
        request(
            2,
            "textDocument/semanticTokens/full",
            Json::obj([("textDocument", doc(uri))]),
        ),
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
    let data = match msgs
        .iter()
        .find(|m| m.get("id") == &Json::num(2))
        .unwrap()
        .get("result")
        .get("data")
    {
        Json::Arr(v) => v.iter().filter_map(Json::as_u32).collect::<Vec<_>>(),
        other => panic!("not an array: {other}"),
    };
    // The token type is the 4th int of each 5-tuple.
    let types: nabic_core::HashSet<u32> = data.chunks(5).map(|c| c[3]).collect();
    for ty in [0u32, 1, 2, 3] {
        assert!(
            types.contains(&ty),
            "missing token type {ty}: types={types:?} data={data:?}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// `r#mykw` is a name even where `mykw` is a keyword.
#[test]
fn semantic_tokens_raw_ident_not_keyword() {
    let dir = nabic_core::tmp::test_dir("lsp-sem-raw");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("r.nabi");
    let uri = path_to_uri(&file.display().to_string(), cfg!(windows));
    let uri = uri.as_str();
    let src = "#[keyword(\"mykw\")] fn myh() {}\nfn g() { let r#mykw = 1; mykw; }\n";
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
        request(
            2,
            "textDocument/semanticTokens/full",
            Json::obj([("textDocument", doc(uri))]),
        ),
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
    let data = match msgs
        .iter()
        .find(|m| m.get("id") == &Json::num(2))
        .unwrap()
        .get("result")
        .get("data")
    {
        Json::Arr(v) => v.iter().filter_map(Json::as_u32).collect::<Vec<_>>(),
        other => panic!("not an array: {other}"),
    };
    let keywords = data.chunks(5).filter(|c| c[3] == 2).count();
    assert_eq!(keywords, 1, "data={data:?}");
    let _ = std::fs::remove_dir_all(&dir);
}
