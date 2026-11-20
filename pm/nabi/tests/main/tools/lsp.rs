//! The language server.

#[allow(clippy::wildcard_imports)]
use crate::tools::*;

pub(crate) fn frame(body: &str) -> String {
    format!("Content-Length: {}\r\n\r\n{body}", body.len())
}

#[test]
fn nabic_lsp_answers_over_stdio() {
    let input = [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"capabilities":{}}}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"shutdown"}"#,
        r#"{"jsonrpc":"2.0","method":"exit"}"#,
    ]
    .map(frame)
    .concat();
    // (`nabils` is `nabi lsp`: the `nabi` crate tests it.)
    {
        let out = piped(NABI, &["lsp"], &input);
        assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
        let stdout = text(&out.stdout);
        assert!(stdout.starts_with("Content-Length: "), "{stdout}");
        assert!(stdout.contains(r#""hoverProvider":true"#), "{stdout}");
        assert!(stdout.contains(r#""id":2,"result":null"#), "{stdout}");
    }
    // `exit` without `shutdown` is exit code 1 (LSP spec).
    let out = piped(
        NABI,
        &["lsp"],
        &frame(r#"{"jsonrpc":"2.0","method":"exit"}"#),
    );
    assert_eq!(out.status.code(), Some(1));
}

/// Input that ends before `initialize` (the server started by hand) is
/// named on stderr, not a bare exit 1.
#[test]
fn nabils_eof_before_initialize_says_so() {
    let out = piped(NABI, &["lsp"], "");
    assert_eq!(out.status.code(), Some(1));
    let err = text(&out.stderr);
    assert!(
        err.contains("input ended before the editor sent `initialize`"),
        "{err}"
    );
}

/// The `nabi.add` command of a quick fix runs `$NABI add <name>` in the
/// package directory; any other command, or a flag, is refused.
#[cfg(unix)]
#[test]
fn lsp_nabi_add_command_runs_nabi() {
    let dir = scratch("lsp-nabi-add");
    let app = dir.join("app");
    std::fs::create_dir_all(app.join("src")).unwrap();
    let main = app.join("src/main.nabi");
    std::fs::write(&main, "use json::parse;\n").unwrap();
    let log = dir.join("log");
    let stub = dir.join("nabi");
    std::fs::write(
        &stub,
        format!("#!/bin/sh\necho \"$PWD $*\" > '{}'\n", log.display()),
    )
    .unwrap();
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    let uri = nabils::path_to_uri(&main.display().to_string(), cfg!(windows));
    let run = |id: u32, command: &str, args: &str| {
        format!(
            r#"{{"jsonrpc":"2.0","id":{id},"method":"workspace/executeCommand","params":{{"command":"{command}","arguments":["{uri}"{args}]}}}}"#
        )
    };
    let input = [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"capabilities":{}}}"#.to_owned(),
        run(2, "nabi.add", r#","json""#),
        run(3, "nabi.remove", r#","json""#),
        run(4, "nabi.add", r#","--path""#),
        r#"{"jsonrpc":"2.0","id":5,"method":"shutdown"}"#.to_owned(),
        r#"{"jsonrpc":"2.0","method":"exit"}"#.to_owned(),
    ]
    .map(|m| frame(&m))
    .concat();
    let stub = stub.display().to_string();
    let out = piped_env(NABI, &["lsp"], &input, &[("__NABI", &stub)]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let stdout = text(&out.stdout);
    assert!(stdout.contains(r#""id":2,"result":null"#), "{stdout}");
    assert!(stdout.contains(r#""id":3,"error""#), "{stdout}");
    assert!(stdout.contains(r#""id":4,"error""#), "{stdout}");
    let ran = std::fs::read_to_string(&log).unwrap();
    let real = |p: &std::path::Path| std::fs::canonicalize(p).unwrap();
    let (cwd, args) = ran.trim_end().split_once(' ').unwrap();
    assert_eq!(real(cwd.as_ref()), real(&app), "{ran}");
    assert_eq!(args, "add json");
}

/// The server builds its dependencies first and answers from their NABIHIR:
/// hover shows a dependency function's doc comment, definition goes to
/// its source.
#[test]
fn lsp_sees_prebuilt_dependencies() {
    let env = setup();
    let dir = scratch("lsp-deps");
    let (util, app) = (dir.join("util"), dir.join("app"));
    std::fs::create_dir_all(util.join("src")).unwrap();
    std::fs::create_dir_all(app.join("src")).unwrap();
    std::fs::write(
        util.join("src/lib.nabi"),
        "/// Says hi.\npub fn hello() -> i32 {\n    return 1;\n}\n",
    )
    .unwrap();
    let main = app.join("src/main.nabi");
    let src = "use util::hello;\nlet x = hello();\nprintln(f\"{x}\");\n";
    std::fs::write(&main, src).unwrap();
    let uri = nabils::path_to_uri(&main.display().to_string(), cfg!(windows));
    let doc = format!(r#"{{"uri":"{uri}","languageId":"nabi","version":1,"text":{src:?}}}"#);
    let at =
        format!(r#"{{"textDocument":{{"uri":"{uri}"}},"position":{{"line":1,"character":10}}}}"#);
    let input = [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"capabilities":{}}}"#.to_owned(),
        format!(r#"{{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{{"textDocument":{doc}}}}}"#),
        format!(r#"{{"jsonrpc":"2.0","id":2,"method":"textDocument/hover","params":{at}}}"#),
        format!(r#"{{"jsonrpc":"2.0","id":3,"method":"textDocument/definition","params":{at}}}"#),
        r#"{"jsonrpc":"2.0","id":4,"method":"shutdown"}"#.to_owned(),
        r#"{"jsonrpc":"2.0","method":"exit"}"#.to_owned(),
    ]
    .map(|m| frame(&m))
    .concat();
    let ext = format!("util={}", util.display());
    let mut child = Command::new(NABI)
        .current_dir(&app)
        .env("NABI_HOME", tool_home())
        .args(["lsp", "--extern", &ext, "--nabir"])
        .arg(&env.nabir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(app.join("target/debug/deps/util.nabihir").is_file());
    let stdout = text(&out.stdout);
    let reply = |id: u32| {
        let at = stdout
            .find(&format!(r#""id":{id},"#))
            .unwrap_or_else(|| panic!("{stdout}"));
        stdout[at..]
            .split("Content-Length")
            .next()
            .unwrap()
            .to_owned()
    };
    assert!(reply(2).contains("Says hi."), "{stdout}");
    let def = reply(3);
    let want = nabils::path_to_uri(
        &util.join("src/lib.nabi").display().to_string(),
        cfg!(windows),
    );
    assert!(def.contains(&want), "{def}");
    assert!(def.contains(r#""start":{"line":1,"#), "{def}");
}

/// An internal compiler error (`NABI_TEST_ICE`, a debug nabic's) in an
/// analysis is that buffer's diagnostic; the server keeps serving.
#[test]
fn lsp_survives_a_compiler_panic() {
    let dir = scratch("lsp-ice");
    let main = dir.join("main.nabi");
    let uri = nabils::path_to_uri(&main.display().to_string(), cfg!(windows));
    let mut child = Command::new(NABI)
        .current_dir(&dir)
        .env("NABI_HOME", tool_home())
        .env("NABI_TEST_ICE", "ice-here")
        .arg("lsp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let send = |stdin: &mut std::process::ChildStdin, m: String| {
        stdin.write_all(frame(&m).as_bytes()).unwrap();
    };
    // The next message whose text contains `want`.
    let mut next = |want: &str| loop {
        let mut len = 0;
        loop {
            let mut line = String::new();
            assert_ne!(stdout.read_line(&mut line).unwrap(), 0, "server gone");
            match line.trim_end().strip_prefix("Content-Length: ") {
                Some(n) => len = n.parse().unwrap(),
                None if line.trim_end().is_empty() => break,
                None => {}
            }
        }
        let mut body = vec![0; len];
        stdout.read_exact(&mut body).unwrap();
        let body = String::from_utf8(body).unwrap();
        if body.contains(want) {
            break body;
        }
    };
    let good = "let x = 1;\nprintln(f\"{x}\");\n";
    let bad = format!("{good}// ice-here\n");
    send(
        &mut stdin,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"capabilities":{}}}"#.into(),
    );
    send(
        &mut stdin,
        format!(
            r#"{{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{{"textDocument":{{"uri":"{uri}","languageId":"nabi","version":1,"text":{bad:?}}}}}}}"#
        ),
    );
    let diags = next("publishDiagnostics");
    assert!(
        diags.contains("internal compiler error") && diags.contains("NABI_TEST_ICE"),
        "{diags}"
    );
    send(
        &mut stdin,
        format!(
            r#"{{"jsonrpc":"2.0","method":"textDocument/didChange","params":{{"textDocument":{{"uri":"{uri}","version":2}},"contentChanges":[{{"text":{good:?}}}]}}}}"#
        ),
    );
    let diags = next("publishDiagnostics");
    assert!(diags.contains(r#""diagnostics":[]"#), "{diags}");
    send(
        &mut stdin,
        format!(
            r#"{{"jsonrpc":"2.0","id":2,"method":"textDocument/hover","params":{{"textDocument":{{"uri":"{uri}"}},"position":{{"line":1,"character":11}}}}}}"#
        ),
    );
    let hover = next(r#""id":2"#);
    assert!(hover.contains("i64"), "{hover}");
    send(
        &mut stdin,
        r#"{"jsonrpc":"2.0","id":3,"method":"shutdown"}"#.into(),
    );
    send(&mut stdin, r#"{"jsonrpc":"2.0","method":"exit"}"#.into());
    drop(stdin);
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
}

/// A language server in `cwd`, driven one message at a time.
struct Client {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
}

impl Client {
    fn start(cwd: &Path, nabir: &Path, vars: &[(&str, &str)]) -> Self {
        let mut child = Command::new(NABI)
            .current_dir(cwd)
            .env("NABI_HOME", tool_home())
            .envs(vars.iter().copied())
            .args(["lsp", "--nabir"])
            .arg(nabir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            stdin,
            stdout,
        }
    }

    fn send(&mut self, m: &str) {
        self.stdin.write_all(frame(m).as_bytes()).unwrap();
        self.stdin.flush().unwrap();
    }

    /// The next message whose text contains every one of `want`.
    fn next(&mut self, want: &[&str]) -> String {
        loop {
            let mut len = 0;
            loop {
                let mut line = String::new();
                assert_ne!(self.stdout.read_line(&mut line).unwrap(), 0, "server gone");
                match line.trim_end().strip_prefix("Content-Length: ") {
                    Some(n) => len = n.parse().unwrap(),
                    None if line.trim_end().is_empty() => break,
                    None => {}
                }
            }
            let mut body = vec![0; len];
            self.stdout.read_exact(&mut body).unwrap();
            let body = String::from_utf8(body).unwrap();
            if want.iter().all(|w| body.contains(w)) {
                return body;
            }
        }
    }

    /// The diagnostics published next for `uri`.
    fn diagnostics(&mut self, uri: &str) -> String {
        let at = format!(r#""uri":"{uri}""#);
        self.next(&["textDocument/publishDiagnostics", &at])
    }

    fn open(&mut self, path: &Path) -> String {
        let uri = nabils::path_to_uri(&path.display().to_string(), cfg!(windows));
        let text = fs::read_to_string(path).unwrap();
        self.send(&format!(
            r#"{{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{{"textDocument":{{"uri":"{uri}","languageId":"nabi","version":1,"text":{text:?}}}}}}}"#
        ));
        uri
    }

    fn finish(mut self) {
        self.send(r#"{"jsonrpc":"2.0","id":99,"method":"shutdown"}"#);
        self.send(r#"{"jsonrpc":"2.0","method":"exit"}"#);
        drop(self.stdin);
        assert_eq!(self.child.wait().unwrap().code(), Some(0));
    }
}

/// A library package `json` with `parse`, under `root`.
fn json_package(root: &Path) -> PathBuf {
    let json = root.join("json");
    fs::create_dir_all(json.join("src")).unwrap();
    fs::write(json.join("nabi.nabion"), "Package { name: \"json\" }\n").unwrap();
    fs::write(
        json.join("src/lib.nabi"),
        "/// Parses.\npub fn parse() -> i32 {\n    return 1;\n}\n",
    )
    .unwrap();
    json
}

/// An application `app` under `root` that uses `json`, with `deps`.
fn app_package(root: &Path, deps: &str) -> PathBuf {
    let app = root.join("app");
    fs::create_dir_all(app.join("src")).unwrap();
    fs::write(
        app.join("nabi.nabion"),
        format!("Package {{ name: \"app\"{deps} }}\n"),
    )
    .unwrap();
    fs::write(
        app.join("src/main.nabi"),
        "use json::parse;\nlet x = parse();\nprintln(f\"{x}\");\n",
    )
    .unwrap();
    app
}

/// Started outside them, the server checks each package of a workspace
/// folder with that package's own dependencies (a path dependency here).
#[test]
fn lsp_resolves_each_package() {
    let env = setup();
    let root = scratch("lsp-workspace");
    let json = json_package(&root);
    let app = app_package(&root, ", deps: { \"json\": Dep::path(\"../json\") }");
    let folder = nabils::path_to_uri(&root.display().to_string(), cfg!(windows));
    let mut c = Client::start(&root, &env.nabir, &[]);
    c.send(&format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"capabilities":{{}},"rootUri":"{folder}","workspaceFolders":[{{"uri":"{folder}","name":"ws"}}]}}}}"#
    ));
    c.next(&[r#""id":1"#]);
    let main = c.open(&app.join("src/main.nabi"));
    let diags = c.diagnostics(&main);
    assert!(diags.contains(r#""diagnostics":[]"#), "{diags}");
    let lib = c.open(&json.join("src/lib.nabi"));
    let diags = c.diagnostics(&lib);
    assert!(diags.contains(r#""diagnostics":[]"#), "{diags}");
    c.send(&format!(
        r#"{{"jsonrpc":"2.0","id":2,"method":"textDocument/hover","params":{{"textDocument":{{"uri":"{main}"}},"position":{{"line":1,"character":9}}}}}}"#
    ));
    let hover = c.next(&[r#""id":2"#]);
    assert!(hover.contains("Parses."), "{hover}");
    c.finish();
    assert!(app.join("target/debug/deps/json.nabihir").is_file());
}

/// The `nabi.add` quick fix changes the manifest; the server resolves
/// again and the dependency is found without a restart. A manifest the
/// editor reports changed (a watched file) is read again too.
#[cfg(unix)]
#[test]
fn lsp_add_dependency_takes_effect() {
    let env = setup();
    let root = scratch("lsp-add-dep");
    let json = json_package(&root);
    let app = app_package(&root, "");
    let stub = root.join("nabi");
    fs::write(
        &stub,
        format!(
            "#!/bin/sh\nexec '{NABI}' add json --path '{}'\n",
            json.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
    let stub = stub.display().to_string();
    let mut c = Client::start(&app, &env.nabir, &[("__NABI", &stub)]);
    c.send(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"capabilities":{"workspace":{"didChangeWatchedFiles":{"dynamicRegistration":true}}}}}"#,
    );
    c.next(&[r#""id":1"#]);
    c.send(r#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#);
    let reg = c.next(&["client/registerCapability"]);
    assert!(
        reg.contains("**/nabi.nabion") && reg.contains("**/nabi.lock"),
        "{reg}"
    );
    c.send(r#"{"jsonrpc":"2.0","id":"nabils-watch","result":null}"#);
    let main = c.open(&app.join("src/main.nabi"));
    let diags = c.diagnostics(&main);
    assert!(diags.contains("json"), "{diags}");
    c.send(&format!(
        r#"{{"jsonrpc":"2.0","id":2,"method":"textDocument/codeAction","params":{{"textDocument":{{"uri":"{main}"}},"range":{{"start":{{"line":0,"character":4}},"end":{{"line":0,"character":8}}}},"context":{{"diagnostics":[]}}}}}}"#
    ));
    let actions = c.next(&[r#""id":2"#]);
    assert!(actions.contains("nabi.add"), "{actions}");
    c.send(&format!(
        r#"{{"jsonrpc":"2.0","id":3,"method":"workspace/executeCommand","params":{{"command":"nabi.add","arguments":["{main}","json"]}}}}"#
    ));
    let done = c.next(&[r#""id":3"#]);
    assert!(done.contains(r#""result":null"#), "{done}");
    let diags = c.diagnostics(&main);
    assert!(diags.contains(r#""diagnostics":[]"#), "{diags}");
    // The dependency removed by hand: the watcher's report brings the
    // error back.
    fs::write(app.join("nabi.nabion"), "Package { name: \"app\" }\n").unwrap();
    let manifest = nabils::path_to_uri(
        &app.join("nabi.nabion").display().to_string(),
        cfg!(windows),
    );
    c.send(&format!(
        r#"{{"jsonrpc":"2.0","method":"workspace/didChangeWatchedFiles","params":{{"changes":[{{"uri":"{manifest}","type":2}}]}}}}"#
    ));
    let diags = c.diagnostics(&main);
    assert!(!diags.contains(r#""diagnostics":[]"#), "{diags}");
    c.finish();
}
