//! Package buffers: manifests, submodules, dependents.

#[allow(clippy::wildcard_imports)]
use crate::tests::*;

/// A `.nabion` buffer (`nabi.nabion`, the package manifest) is one NABION
/// value: only its syntax is checked, not compiled as a module.
#[test]
fn manifest_buffer() {
    let uri = "file:///nowhere/nabi.nabion";
    let open = notify(
        "textDocument/didOpen",
        Json::obj([(
            "textDocument",
            Json::obj([
                ("uri", Json::str(uri)),
                ("languageId", Json::str("nabi")),
                ("version", Json::num(1)),
                (
                    "text",
                    Json::str("Package {\n    deps: { \"a\": Dep::path(\"x\") },\n}\n"),
                ),
            ]),
        )]),
    );
    let script = [
        open,
        request(1, "textDocument/hover", at(uri, 0, 1)),
        change(uri, "Package {\n    name: \"a\"\n    version: \"1\",\n}\n"),
        request(2, "textDocument/hover", at(uri, 0, 1)),
        // Valid nabi, but not NABION data.
        change(uri, "Package {\n    name: bare,\n}\n"),
        request(4, "textDocument/hover", at(uri, 0, 1)),
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
    let published: Vec<Json> = messages(&out)
        .into_iter()
        .filter(|m| m.get("method").as_str() == Some("textDocument/publishDiagnostics"))
        .map(|m| m.get("params").get("diagnostics").clone())
        .collect();
    assert_eq!(published[0], Json::Arr(Vec::new()));
    let Json::Arr(errs) = &published[1] else {
        panic!("{published:?}")
    };
    assert!(!errs.is_empty(), "{published:?}");
    assert_eq!(errs[0].get("range").get("start").get("line"), &Json::num(2));
    let Json::Arr(errs) = &published[2] else {
        panic!("{published:?}")
    };
    assert_eq!(errs[0].get("range").get("start").get("line"), &Json::num(1));
    assert!(
        errs[0]
            .get("message")
            .as_str()
            .is_some_and(|m| m.starts_with("bare identifier `bare`")),
        "{published:?}"
    );
}

/// An open submodule buffer stands for its file: its unsaved text is
/// what the package compiles with.
#[test]
fn unsaved_submodule_buffer() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let bootstrap = workspace.join("library/core/src/lib.nabi");
    let dir = nabic_core::tmp::test_dir("lsp-sub");
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("src/main.nabi"),
        "mod m;\n\nlet x: i32 = m::v();\n",
    )
    .unwrap();
    std::fs::write(dir.join("src/m.nabi"), "pub fn v() -> i32 {\n    1\n}\n").unwrap();
    let uri = path_to_uri(&dir.join("src/m.nabi").display().to_string(), cfg!(windows));
    let uri = uri.as_str();
    let open = notify(
        "textDocument/didOpen",
        Json::obj([(
            "textDocument",
            Json::obj([
                ("uri", Json::str(uri)),
                ("languageId", Json::str("nabi")),
                ("version", Json::num(1)),
                ("text", Json::str("pub fn v() -> i32 {\n    true\n}\n")),
            ]),
        )]),
    );
    let script = [
        open,
        request(1, "textDocument/hover", at(uri, 0, 8)),
        change(uri, "pub fn v() -> i32 {\n    2\n}\n"),
        request(2, "textDocument/hover", at(uri, 0, 8)),
        request(3, "shutdown", Json::Null),
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
    let published: Vec<Json> = messages(&out)
        .into_iter()
        .filter(|m| m.get("method").as_str() == Some("textDocument/publishDiagnostics"))
        .map(|m| m.get("params").get("diagnostics").clone())
        .collect();
    let Json::Arr(errs) = &published[0] else {
        panic!("{published:?}")
    };
    assert_eq!(errs.len(), 1, "{published:?}");
    assert!(
        errs[0]
            .get("message")
            .as_str()
            .is_some_and(|m| m.contains("bool")),
        "{published:?}"
    );
    assert_eq!(published[1], Json::Arr(Vec::new()), "{published:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn submodule_changes_and_closes_refresh_dependents() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let bootstrap = workspace.join("library/core/src/lib.nabi");
    let dir = nabic_core::tmp::test_dir("lsp-dependent-buffers");
    std::fs::create_dir_all(dir.join("src")).unwrap();
    let main = "mod m;\n\nlet x = m::v() + 1;\n";
    let disk = "pub fn v() -> i32 {\n    1\n}\n";
    let edited = "pub fn v() -> i64 {\n    2\n}\n";
    std::fs::write(dir.join("src/main.nabi"), main).unwrap();
    std::fs::write(dir.join("src/m.nabi"), disk).unwrap();
    let uri = path_to_uri(
        &dir.join("src/main.nabi").display().to_string(),
        cfg!(windows),
    );
    let sub = path_to_uri(&dir.join("src/m.nabi").display().to_string(), cfg!(windows));
    let open = |uri: &str, text: &str| {
        notify(
            "textDocument/didOpen",
            Json::obj([(
                "textDocument",
                Json::obj([("uri", Json::str(uri)), ("text", Json::str(text))]),
            )]),
        )
    };
    let script = [
        open(&sub, "pub fn v() -> bool {\n    true\n}\n"),
        open(&uri, main),
        request(1, "textDocument/hover", at(&uri, 2, 4)),
        change(&sub, edited),
        request(2, "textDocument/hover", at(&uri, 2, 4)),
        notify(
            "textDocument/didClose",
            Json::obj([("textDocument", doc(&sub))]),
        ),
        request(3, "textDocument/hover", at(&uri, 2, 4)),
        open(&sub, edited),
        request(4, "textDocument/hover", at(&uri, 2, 4)),
        request(5, "shutdown", Json::Null),
        notify("exit", Json::Null),
    ];
    let input: String = script.iter().map(frame).collect();
    let mut out = Vec::new();
    assert_eq!(
        serve(
            Cursor::new(input.into_bytes()),
            &mut out,
            vec![bootstrap],
            Vec::new(),
            Vec::new()
        ),
        0
    );
    let msgs = messages(&out);
    let result = |id| {
        msgs.iter()
            .find(|m| m.get("id") == &Json::num(id))
            .unwrap()
            .get("result")
    };
    assert_eq!(result(1), &Json::Null);
    for (id, ty) in [(2, "i64"), (3, "i32"), (4, "i64")] {
        let value = result(id).get("contents").get("value").as_str().unwrap();
        assert!(value.contains(ty), "{id}: {value}");
    }
    let diagnostics: Vec<_> = msgs
        .iter()
        .filter(|m| {
            m.get("method").as_str() == Some("textDocument/publishDiagnostics")
                && m.get("params").get("uri").as_str() == Some(uri.as_str())
        })
        .map(|m| m.get("params").get("diagnostics"))
        .collect();
    let has_error = |diags: &Json| matches!(diags, Json::Arr(ds) if ds.iter().any(|d| d.get("severity") == &Json::num(1)));
    assert!(has_error(diagnostics[0]));
    assert!(!has_error(diagnostics.last().unwrap()));
}

/// A package with both root files: a file the library
/// reaches is checked as the library's (its unsaved text included); the
/// executable's own files see the library as the package's name, its
/// `pub` items only.
#[test]
fn library_and_executable_package() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let bootstrap = workspace.join("library/core/src/lib.nabi");
    let dir = nabic_core::tmp::test_dir("lsp-both").join("app");
    std::fs::create_dir_all(dir.join("src")).unwrap();
    let lib = "pub fn v() -> i32 {\n    1\n}\n\nfn hidden() -> i32 {\n    2\n}\n";
    std::fs::write(dir.join("src/lib.nabi"), lib).unwrap();
    let main = "mod cli;\n\nuse app::v;\n\nlet x: i32 = v() + app::hidden() + cli::w();\n";
    std::fs::write(dir.join("src/main.nabi"), main).unwrap();
    std::fs::write(
        dir.join("src/cli.nabi"),
        "pub fn w() -> i32 {\n    v()\n}\n",
    )
    .unwrap();
    let uri = |f: &str| path_to_uri(&dir.join(f).display().to_string(), cfg!(windows));
    let open = |f: &str, text: &str| {
        notify(
            "textDocument/didOpen",
            Json::obj([(
                "textDocument",
                Json::obj([
                    ("uri", Json::str(uri(f))),
                    ("languageId", Json::str("nabi")),
                    ("version", Json::num(1)),
                    ("text", Json::str(text)),
                ]),
            )]),
        )
    };
    let script = [
        open("src/main.nabi", main),
        request(1, "textDocument/hover", at(&uri("src/main.nabi"), 2, 9)),
        open("src/cli.nabi", "pub fn w() -> i32 {\n    v()\n}\n"),
        request(2, "textDocument/hover", at(&uri("src/cli.nabi"), 0, 7)),
        open("src/lib.nabi", &lib.replace("    1", "    true")),
        request(3, "textDocument/hover", at(&uri("src/lib.nabi"), 0, 7)),
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
    let published: Vec<(String, Vec<String>)> = messages(&out)
        .into_iter()
        .filter(|m| m.get("method").as_str() == Some("textDocument/publishDiagnostics"))
        .map(|m| {
            let p = m.get("params");
            let Json::Arr(d) = p.get("diagnostics") else {
                panic!("{p:?}")
            };
            let msgs = d
                .iter()
                .map(|d| d.get("message").as_str().unwrap_or_default().to_owned())
                .collect();
            (p.get("uri").as_str().unwrap_or_default().to_owned(), msgs)
        })
        .collect();
    let of = |f: &str| {
        published
            .iter()
            .find(|(u, _)| *u == uri(f))
            .unwrap_or_else(|| panic!("no diagnostics for {f}: {published:?}"))
            .1
            .clone()
    };
    assert_eq!(
        of("src/main.nabi"),
        [
            "function `app::hidden` is private to module `app` and cannot be used from `<root>` \
             (add `pub` to expose it)\nhelp: `src/main.nabi` uses its package's \
             library `src/lib.nabi` as the package `app`, like a dependency: it sees only the \
             library's `pub` items"
        ]
    );
    let lib_diags = of("src/lib.nabi");
    assert!(
        lib_diags.len() == 1 && lib_diags[0].contains("bool"),
        "{lib_diags:?}"
    );
    // `cli` is the executable's: `v` is not in its scope.
    let cli = of("src/cli.nabi");
    assert!(
        cli.len() == 1 && cli[0].contains("cannot find function `v`"),
        "{cli:?}"
    );
    let _ = std::fs::remove_dir_all(dir.parent().unwrap());
}

/// Each package's dependencies are resolved once, for its directory,
/// and again after its manifest or lock is saved; a buffer outside
/// every package uses the server's own.
#[test]
fn dependencies_per_package() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let bootstrap = workspace.join("library/core/src/lib.nabi");
    let dir = nabic_core::tmp::test_dir("lsp-deps-per-package");
    let mut uris = Vec::new();
    for p in ["a", "b"] {
        std::fs::create_dir_all(dir.join(p).join("src")).unwrap();
        std::fs::write(dir.join(p).join("nabi.nabion"), "Package {}\n").unwrap();
        let main = dir.join(p).join("src/main.nabi");
        std::fs::write(&main, "let x: i32 = 1;\n").unwrap();
        uris.push(path_to_uri(&main.display().to_string(), cfg!(windows)));
    }
    let lone = dir.join("lone.nabi");
    std::fs::write(&lone, "let y = 2;\n").unwrap();
    let lone = path_to_uri(&lone.display().to_string(), cfg!(windows));
    let open = |uri: &str| {
        notify(
            "textDocument/didOpen",
            Json::obj([(
                "textDocument",
                Json::obj([
                    ("uri", Json::str(uri)),
                    ("languageId", Json::str("nabi")),
                    ("version", Json::num(1)),
                    ("text", Json::str("let x: i32 = 1;\n")),
                ]),
            )]),
        )
    };
    let lock = path_to_uri(
        &dir.join("b/nabi.lock").display().to_string(),
        cfg!(windows),
    );
    let script = [
        request(
            1,
            "initialize",
            Json::obj([("capabilities", Json::obj([]))]),
        ),
        open(&uris[0]),
        open(&uris[1]),
        open(&lone),
        request(2, "textDocument/hover", at(&uris[0], 0, 4)),
        change(&uris[0], "let x: i32 = 3;\n"),
        request(3, "textDocument/hover", at(&uris[0], 0, 4)),
        request(4, "textDocument/hover", at(&uris[1], 0, 4)),
        notify(
            "textDocument/didSave",
            Json::obj([("textDocument", doc(&lock))]),
        ),
        request(5, "textDocument/hover", at(&uris[0], 0, 4)),
        request(6, "textDocument/hover", at(&uris[1], 0, 4)),
        request(7, "shutdown", Json::Null),
        notify("exit", Json::Null),
    ];
    let input: String = script.iter().map(frame).collect();
    let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = std::sync::Arc::clone(&calls);
    let deps: crate::Deps = Box::new(move |d: &Path| {
        seen.lock().unwrap().push(d.to_path_buf());
        Ok(Vec::new())
    });
    let mut out = Vec::new();
    let code = crate::serve_with(
        Cursor::new(input.into_bytes()),
        &mut out,
        vec![bootstrap],
        Vec::new(),
        Vec::new(),
        Some(deps),
    );
    assert_eq!(code, 0);
    let mut calls = calls.lock().unwrap().clone();
    calls.sort();
    let (a, b) = (dir.join("a"), dir.join("b"));
    assert_eq!(calls, [a.clone(), a, b.clone(), b], "{calls:?}");
}
