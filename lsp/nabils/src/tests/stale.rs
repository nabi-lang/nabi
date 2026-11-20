//! Answers while a check is due: from the last good compile, through
//! the edit since; cancelled requests.

#[allow(clippy::wildcard_imports)]
use crate::tests::*;
use nabic_tools::cursor::Delta;

#[test]
fn delta_maps_outside_the_edit() {
    let d = Delta::new(
        "let a = 1;\nlet b = a;\n",
        "let a = 1;\nlet xyz = 2;\nlet b = a;\n",
    );
    assert_eq!((d.pre, d.old_end, d.new_end), (15, 15, 28));
    assert_eq!(d.to_old(4), Some(4));
    // `b` on the last line moved by the inserted 13 bytes.
    assert_eq!(d.to_old(32), Some(19));
    assert_eq!(d.to_old(20), None);
    assert_eq!(d.to_new(19), Some(32));
    // A multi-byte character is never split.
    let d = Delta::new("é", "è");
    assert_eq!((d.pre, d.old_end, d.new_end), (0, 2, 2));
}

#[test]
fn queries_answer_through_edits_and_cancel() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let bootstrap = workspace.join("library/core/src/lib.nabi");
    let dir = nabic_core::tmp::test_dir("lsp-stale");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("demo.nabi");
    let uri = path_to_uri(&file.display().to_string(), cfg!(windows));
    let uri = uri.as_str();
    // A line above shifts `p` of `p.sum()` to line 8; `twice` renamed.
    let shifted = format!("let extra = 1;\n{SRC}");
    let renamed = shifted.replace("twice", "thrice");
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
        // No compile yet: this one waits for it.
        request(2, "textDocument/hover", at(uri, 7, 8)),
        change(uri, &shifted),
        request(3, "textDocument/hover", at(uri, 8, 8)),
        request(4, "textDocument/definition", at(uri, 9, 12)),
        change(uri, &renamed),
        // On the edited name: answered from a check of the new text.
        request(5, "textDocument/hover", at(uri, 9, 12)),
        request(6, "textDocument/hover", at(uri, 8, 8)),
        notify("$/cancelRequest", Json::obj([("id", Json::num(6))])),
        request(7, "shutdown", Json::Null),
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
    };
    let hover = |id| {
        reply(id)
            .get("result")
            .get("contents")
            .get("value")
            .as_str()
            .unwrap_or("")
            .to_owned()
    };
    assert!(hover(2).contains("p: P"), "{}", hover(2));
    assert!(hover(3).contains("p: P"), "{}", hover(3));
    // The definition is reported in the text as it is now.
    let def = reply(4).get("result");
    assert_eq!(def.get("range").get("start").get("line"), &Json::num(6));
    assert!(hover(5).contains("fn thrice"), "{}", hover(5));
    // Cancelled: an error, or answered before the cancel was read.
    let six = reply(6);
    assert!(
        six.get("error").get("code") == &Json::num(-32800) || hover(6).contains("p: P"),
        "{six}"
    );
}

/// After `x.` typed where `x.method()` was, the members come from the
/// last good compile: no compile runs.
#[test]
fn member_completion_without_a_compile() {
    let bootstrap = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../library/core/src/lib.nabi");
    let dir = nabic_core::tmp::test_dir("lsp-stale-members");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("m.nabi");
    let key = path.display().to_string();
    let mut session = nabic_interface::Session::new();
    session.add_bootstrap_file(bootstrap);
    session.set_anonymous(true);
    // A loop's range: lowered, `v` has no expression of its own there.
    let src =
        format!("{SRC}fn g(v: P) -> i32 {{ let n = 0; for i in v.sum()..9 {{ n += i; }} n }}\n");
    let good =
        nabic_tools::analysis::analyze(session, &src, &path, &nabic_tools::lint::Levels::default());
    assert!(good.hir.is_some());
    for (was, typed) in [("p.sum()", "p.()"), ("v.sum()..9", "v.()..9")] {
        let typed_src = src.replace(was, typed);
        let off = typed_src.find(typed).unwrap() + typed.find('(').unwrap();
        let (_, found) = nabic_tools::complete::complete(
            &typed_src,
            &key,
            off,
            |_| panic!("compiled"),
            Some(&good),
            &[],
        );
        let labels: Vec<&str> = found.iter().map(|c| c.0.as_str()).collect();
        for m in ["x", "y", "sum"] {
            assert!(labels.contains(&m), "{typed}: {labels:?}");
        }
    }
}
