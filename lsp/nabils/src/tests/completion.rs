//! Completion.

#[allow(clippy::wildcard_imports)]
use crate::tests::*;

pub(crate) const LOCAL: &str = "\
fn f() -> i32 {
    struct L { v: i32; }
    impl L {
        fn get(self) -> i32 { self.v }
    }
    let l = L { v: 1 };
    l.get()
}
println(f\"{f()}\");
";

/// Hover and definition of an item declared in a function body
/// go by the name as written.
#[test]
fn block_local_items() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let bootstrap = workspace.join("library/core/src/lib.nabi");
    let dir = nabic_core::tmp::test_dir("lsp-local");
    std::fs::create_dir_all(&dir).unwrap();
    let uri = path_to_uri(&dir.join("local.nabi").display().to_string(), cfg!(windows));
    let uri = uri.as_str();
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
                    ("text", Json::str(LOCAL)),
                ]),
            )]),
        ),
        // `l` and `get` in `l.get()`, `L` in `L { v: 1 }`.
        request(2, "textDocument/hover", at(uri, 6, 4)),
        request(3, "textDocument/hover", at(uri, 6, 7)),
        request(4, "textDocument/definition", at(uri, 6, 7)),
        request(5, "textDocument/definition", at(uri, 5, 12)),
        request(6, "shutdown", Json::Null),
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
    let line = |id| result(id).get("range").get("start").get("line").clone();
    assert!(hover(2).contains("l: L"), "{}", hover(2));
    assert!(hover(3).contains("fn get(self: L) -> i32"), "{}", hover(3));
    assert_eq!(line(4), Json::num(3), "{}", result(4));
    assert_eq!(line(5), Json::num(1), "{}", result(5));
    let _ = std::fs::remove_dir_all(&dir);
}

pub(crate) const HIDDEN: &str = "\
pub mod m {
    pub fn shown() -> i32 { 1 }
    #[doc(hidden)]
    pub fn hook() -> i32 { 2 }
}
struct P { x: i32; }
impl P {
    fn get(self) -> i32 { self.x }
    #[doc(hidden)]
    fn raw(self) -> i32 { self.x } }
let p = P { x: m::shown() + m::hook() };
println(f\"{p.get() + p.raw()}\");
";

/// Completion leaves out `#[doc(hidden)]` items, and the library's
/// `pub(pkg)` compiler hooks.
#[test]
fn completion_skips_doc_hidden() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let bootstrap = workspace.join("library/core/src/lib.nabi");
    let dir = nabic_core::tmp::test_dir("lsp-hidden");
    std::fs::create_dir_all(&dir).unwrap();
    let uri = path_to_uri(
        &dir.join("hidden.nabi").display().to_string(),
        cfg!(windows),
    );
    let uri = uri.as_str();
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
                    ("text", Json::str(HIDDEN)),
                ]),
            )]),
        ),
        // `p` in `let p`: the good compile path completion reads.
        request(6, "textDocument/hover", at(uri, 10, 4)),
        change(uri, &format!("{HIDDEN}m::\n")),
        request(2, "textDocument/completion", at(uri, 12, 3)),
        change(uri, &format!("{HIDDEN}let t = p.\n")),
        request(3, "textDocument/completion", at(uri, 12, 10)),
        change(uri, &format!("{HIDDEN}core::index::\n")),
        request(4, "textDocument/completion", at(uri, 12, 19)),
        request(5, "shutdown", Json::Null),
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
    let labels = |id: u32| match msgs
        .iter()
        .find(|m| m.get("id") == &Json::num(id))
        .unwrap_or_else(|| panic!("no reply {id}"))
        .get("result")
    {
        Json::Arr(items) => items
            .iter()
            .filter_map(|i| i.get("label").as_str().map(str::to_owned))
            .collect::<Vec<_>>(),
        r => panic!("completion {id}: {r}"),
    };
    let has = |id: u32, name: &str| labels(id).iter().any(|l| l == name);
    assert!(has(2, "shown") && !has(2, "hook"), "{:?}", labels(2));
    assert!(has(3, "get") && !has(3, "raw"), "{:?}", labels(3));
    assert!(has(4, "Index"), "{:?}", labels(4));
    for hook in [
        "index_oob",
        "spread_misfit",
        "spread_len",
        "index_from_end",
        "slice_range",
    ] {
        assert!(!has(4, hook), "{hook} in {:?}", labels(4));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Completion leaves out the library's `#[internal]` items.
#[test]
fn internal_hidden_from_completion() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let bootstrap = workspace.join("library/core/src/lib.nabi");
    let dir = nabic_core::tmp::test_dir("lsp-internal");
    std::fs::create_dir_all(&dir).unwrap();
    let uri = path_to_uri(
        &dir.join("internal.nabi").display().to_string(),
        cfg!(windows),
    );
    let uri = uri.as_str();
    let base = "let n = 1;\n";
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
                    ("text", Json::str(base)),
                ]),
            )]),
        ),
        change(uri, &format!("{base}core::task::\n")),
        request(2, "textDocument/completion", at(uri, 1, 18)),
        change(uri, &format!("{base}core::marker::\n")),
        request(3, "textDocument/completion", at(uri, 1, 20)),
        request(4, "shutdown", Json::Null),
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
    let labels = |id: u32| match msgs
        .iter()
        .find(|m| m.get("id") == &Json::num(id))
        .unwrap_or_else(|| panic!("no reply {id}"))
        .get("result")
    {
        Json::Arr(items) => items
            .iter()
            .filter_map(|i| i.get("label").as_str().map(str::to_owned))
            .collect::<Vec<_>>(),
        r => panic!("completion {id}: {r}"),
    };
    let has = |id: u32, name: &str| labels(id).iter().any(|l| l == name);
    assert!(has(2, "block_on"), "{:?}", labels(2));
    for name in ["wake_io", "host_enter", "host_run", "host_leave"] {
        assert!(!has(2, name), "{name} in {:?}", labels(2));
    }
    assert!(!has(3, "hand_over"), "{:?}", labels(3));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Open `text` as a lone file with core and std, then send `steps(uri)`:
/// the server's messages.
pub(crate) fn std_session(
    name: &str,
    text: &str,
    steps: impl FnOnce(&str) -> Vec<Json>,
) -> Vec<Json> {
    let lib = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../library");
    let bootstrap = nabic_interface::bootstrap_tiers(&[
        lib.join("core/src/lib.nabi"),
        lib.join("std/src/lib.nabi"),
    ]);
    let dir = nabic_core::tmp::test_dir(name);
    std::fs::create_dir_all(&dir).unwrap();
    let uri = path_to_uri(&dir.join("main.nabi").display().to_string(), cfg!(windows));
    let mut script = vec![
        request(
            1000,
            "initialize",
            Json::obj([("capabilities", Json::obj([]))]),
        ),
        notify(
            "textDocument/didOpen",
            Json::obj([(
                "textDocument",
                Json::obj([
                    ("uri", Json::str(&uri)),
                    ("languageId", Json::str("nabi")),
                    ("version", Json::num(1)),
                    ("text", Json::str(text)),
                ]),
            )]),
        ),
    ];
    script.extend(steps(&uri));
    script.push(request(1001, "shutdown", Json::Null));
    script.push(notify("exit", Json::Null));
    let input: String = script.iter().map(frame).collect();
    let mut out = Vec::new();
    let code = serve(
        Cursor::new(input.into_bytes()),
        &mut out,
        bootstrap,
        Vec::new(),
        vec![(nabic_tools::lint::Level::Allow, "dead_code".to_owned())],
    );
    assert_eq!(code, 0);
    let _ = std::fs::remove_dir_all(&dir);
    messages(&out)
}

pub(crate) fn reply(msgs: &[Json], id: u32) -> Json {
    msgs.iter()
        .find(|m| m.get("id") == &Json::num(id))
        .unwrap_or_else(|| panic!("no reply {id}"))
        .get("result")
        .clone()
}

pub(crate) fn labels(msgs: &[Json], id: u32) -> Vec<String> {
    match reply(msgs, id) {
        Json::Arr(items) => items
            .iter()
            .filter_map(|i| i.get("label").as_str().map(str::to_owned))
            .collect(),
        r => panic!("completion {id}: {r}"),
    }
}

pub(crate) const LIST: &str = "\
let xs: List<i32> = [1, 2];
xs.push(3);
println(f\"{xs.len()}\");
";

/// `xs.` offers the public methods of `List`, not its private field,
/// helpers, operator hooks or a generic instance's name.
#[test]
fn completion_list_methods_not_internals() {
    let msgs = std_session("lsp-list-members", LIST, |uri| {
        vec![
            request(1, "textDocument/hover", at(uri, 0, 4)),
            change(uri, &format!("{LIST}let n = xs.\n")),
            request(2, "textDocument/completion", at(uri, 3, 11)),
        ]
    });
    let got = labels(&msgs, 2);
    for m in ["push", "len", "pop", "iter", "clone"] {
        assert!(got.iter().any(|l| l == m), "{m} not in {got:?}");
    }
    for m in [
        "h",
        "Prim(I32)",
        "push_grown",
        "dot_add",
        "index",
        "transfer_copy",
    ] {
        assert!(!got.iter().any(|l| l == m), "{m} in {got:?}");
    }
}

#[test]
fn completion_partial_method() {
    let msgs = std_session("lsp-partial-member", LIST, |uri| {
        vec![
            request(1, "textDocument/hover", at(uri, 0, 4)),
            change(uri, &format!("{LIST}xs.pu\n")),
            request(2, "textDocument/completion", at(uri, 3, 5)),
        ]
    });
    assert_eq!(labels(&msgs, 2), ["push"]);
}

/// `h.count|` on a complete member: `h`'s members, not `h.count`'s,
/// also when the buffer without the member does not check.
#[test]
fn completion_member_of_receiver() {
    let src = "\
struct H { count: i64; name: str; }
let h = H { count: 1, name: \"a\" };
println(h.count);
";
    let msgs = std_session("lsp-member-receiver", src, |uri| {
        vec![
            request(1, "textDocument/completion", at(uri, 2, 15)),
            request(2, "textDocument/completion", at(uri, 2, 10)),
        ]
    });
    assert_eq!(labels(&msgs, 1), ["count"]);
    assert_eq!(labels(&msgs, 2), ["count", "name"]);
}

#[test]
fn completion_bare_prelude() {
    let msgs = std_session("lsp-bare", "pri\n", |uri| {
        vec![request(1, "textDocument/completion", at(uri, 0, 3))]
    });
    let got = labels(&msgs, 1);
    assert!(got.iter().any(|l| l == "print"), "{got:?}");
    assert!(got.iter().any(|l| l == "println"), "{got:?}");
    assert!(got.iter().all(|l| l.starts_with("pri")), "{got:?}");
}

/// A bare word: the locals in scope at the cursor, the buffer's items
/// and keywords; not another function's locals.
#[test]
fn completion_local() {
    let src = "\
fn helper() -> i32 {
    let inner = 1;
    inner
}
let count = helper();
let total = co
";
    let msgs = std_session("lsp-local", src, |uri| {
        vec![
            request(1, "textDocument/completion", at(uri, 5, 14)),
            request(2, "textDocument/completion", at(uri, 5, 12)),
        ]
    });
    let got = labels(&msgs, 1);
    assert!(got.iter().any(|l| l == "count"), "{got:?}");
    assert!(got.iter().any(|l| l == "const"), "{got:?}");
    let all = labels(&msgs, 2);
    assert!(all.iter().any(|l| l == "helper"), "{all:?}");
    assert!(!all.iter().any(|l| l == "inner"), "{all:?}");
}

/// `use a::b::` lists the modules below, even before the buffer has
/// ever checked.
#[test]
fn completion_use_path() {
    let msgs = std_session("lsp-use-path", "use std::\n", |uri| {
        vec![
            request(1, "textDocument/completion", at(uri, 0, 15)),
            change(uri, "use std::fs::\n"),
            request(2, "textDocument/completion", at(uri, 0, 19)),
        ]
    });
    let got = labels(&msgs, 1);
    for m in ["fs", "env", "process"] {
        assert!(got.iter().any(|l| l == m), "{m} not in {got:?}");
    }
    let fs = labels(&msgs, 2);
    assert!(fs.iter().any(|l| l == "read_to_str"), "{fs:?}");
    assert!(!fs.iter().any(|l| l == "stat_at"), "{fs:?}");
}
