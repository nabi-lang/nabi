//! The Jupyter kernel.

#[allow(clippy::wildcard_imports)]
use crate::tools::*;

/// `nabi jupyter` driven by a hand-written ZMTP client (no Jupyter
/// needed): heartbeat, `kernel_info`, cells that bind, show, print and
/// panic, completion, and `shutdown`. Every message must be signed.
#[test]
#[cfg_attr(
    not(all(
        any(
            target_os = "macos",
            all(target_os = "linux", not(target_env = "musl"))
        ),
        any(target_arch = "aarch64", target_arch = "x86_64")
    )),
    ignore = "the REPL host runs on macOS and Linux glibc, arm64 and x86_64"
)]
pub(crate) fn jupyter_kernel_speaks_the_wire_protocol() {
    let env = setup();
    let key = b"secret-key";
    let dir = scratch("jupyter");
    // The kernel loads the package it starts in.
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("src/main.nabi"),
        "fn add(a: i32, b: i32) -> i32 { a + b }\nlet base = 40i32;\nstruct Dot {}\nimpl \
         RichDisplay for Dot {\n    fn mime_bundle(self) -> Dict<str, str> {\n        let b: \
         Dict<str, str> = Dict::new();\n        b[\"image/svg+xml\"] = \"<svg/>\";\n        \
         b\n    }\n}\n",
    )
    .unwrap();
    let file = dir.join("kernel.json");
    // The kernel says `ready` on stderr once every socket accepts. A
    // probed port can be taken again before the kernel binds it (it then
    // exits instead): start over on fresh ports.
    let mut attempts = 0;
    let (kernel, ports, mut stderr) = loop {
        let listeners: Vec<_> = (0..5)
            .map(|_| TcpListener::bind("127.0.0.1:0").unwrap())
            .collect();
        let ports: Vec<u16> = listeners
            .iter()
            .map(|l| l.local_addr().unwrap().port())
            .collect();
        drop(listeners);
        std::fs::write(
            &file,
            format!(
                r#"{{"transport":"tcp","ip":"127.0.0.1","shell_port":{},"iopub_port":{},
"stdin_port":{},"control_port":{},"hb_port":{},"key":"secret-key",
"signature_scheme":"hmac-sha256","kernel_name":"nabi"}}"#,
                ports[0], ports[1], ports[2], ports[3], ports[4]
            ),
        )
        .unwrap();
        let mut kernel = Command::new(NABI)
            .args(["jupyter", "--connection-file"])
            .arg(&file)
            .args(["--nabir", &env.nabir.display().to_string()])
            .env("NABI_HOME", tool_home())
            .current_dir(&dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stderr = BufReader::new(kernel.stderr.take().unwrap());
        let mut said = String::new();
        stderr.read_line(&mut said).unwrap();
        if said == "nabi jupyter: ready\n" {
            break (kernel, ports, stderr);
        }
        kernel.wait().unwrap();
        attempts += 1;
        assert!(attempts < 5, "the kernel exits: {said}");
    };
    let connect = |port: u16, kind: &str| {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(120))).unwrap();
        handshake(&mut s, kind).unwrap();
        s
    };
    let mut iopub = connect(ports[1], "SUB");
    write_msg(&mut iopub, &[b"\x01".to_vec()]).unwrap();
    let mut shell = connect(ports[0], "DEALER");
    let mut control = connect(ports[3], "DEALER");
    let mut hb = connect(ports[4], "REQ");
    write_msg(&mut hb, &[Vec::new(), b"ping".to_vec()]).unwrap();
    assert_eq!(read_msg(&mut hb).unwrap(), [Vec::new(), b"ping".to_vec()]);

    // ZeroMQ's slow joiner: iopub gets the subscriber only after the
    // handshake, so probe with `kernel_info_request` until it answers.
    iopub
        .set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        let probe = encode(
            key,
            "test",
            &[],
            "kernel_info_request",
            &Json::obj([]),
            &Json::obj([]),
        );
        write_msg(&mut shell, &probe).unwrap();
        read_msg(&mut shell).unwrap();
        if iopub.peek(&mut [0u8]).is_ok_and(|n| n > 0) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "nothing on iopub after 60s"
        );
    }
    iopub
        .set_read_timeout(Some(Duration::from_secs(120)))
        .unwrap();
    let mut stdin = connect(ports[2], "DEALER");
    streams_and_asks(key, &mut shell, &mut iopub, &mut stdin);

    // Send a request: its reply (type, content) and the iopub messages
    // it caused, up to its `idle`.
    let mut request = |s: &mut TcpStream, kind: &str, content: Json| {
        let frames = encode(key, "test", &[], kind, &Json::obj([]), &content);
        let mine = parse_msg(key, frames.clone()).unwrap().header;
        let id = mine.get("msg_id").clone();
        write_msg(s, &frames).unwrap();
        let reply = parse_msg(key, read_msg(s).unwrap()).expect("a signed reply");
        assert!(reply.ids.is_empty());
        assert_eq!(reply.parent.get("msg_id"), &id);
        let mut published = Vec::new();
        loop {
            let m = parse_msg(key, read_msg(&mut iopub).unwrap()).expect("signed iopub");
            if m.parent.get("msg_id") != &id {
                continue;
            }
            if m.content.get("execution_state").as_str() == Some("idle") {
                break;
            }
            published.push((m.kind().to_owned(), m.content));
        }
        (reply.kind().to_owned(), reply.content, published)
    };
    let exec = |code: &str| Json::obj([("code", Json::str(code)), ("silent", Json::Bool(false))]);
    let shows = |published: &[(String, Json)], text: &str| {
        published.iter().any(|(k, c)| {
            k == "execute_result" && c.get("data").get("text/plain").as_str() == Some(text)
        })
    };

    let (kind, info, _) = request(&mut shell, "kernel_info_request", Json::obj([]));
    assert_eq!(kind, "kernel_info_reply");
    assert_eq!(info.get("language_info").get("name").as_str(), Some("nabi"));
    assert_eq!(info.get("banner").as_str(), Some(toolchain_line().as_str()));

    let (_, reply, _) = request(&mut shell, "execute_request", exec("let x = 40i32;"));
    assert_eq!(reply.get("status").as_str(), Some("ok"), "{reply}");
    let (_, reply, published) = request(&mut shell, "execute_request", exec("x + 2"));
    assert_eq!(reply.get("execution_count"), &Json::num(2));
    assert!(shows(&published, "42"), "{published:?}");
    assert!(
        published
            .iter()
            .any(|(k, c)| k == "execute_input" && c.get("code").as_str() == Some("x + 2"))
    );
    cells_follow_the_count(&mut shell, &mut request);

    let (_, _, published) = request(&mut shell, "execute_request", exec("println(f\"hi {x}\");"));
    assert!(
        published.iter().any(|(k, c)| k == "stream"
            && c.get("name").as_str() == Some("stdout")
            && c.get("text").as_str() == Some("hi 40\n")),
        "{published:?}"
    );

    let (_, reply, published) = request(&mut shell, "execute_request", exec("panic(\"boom\");"));
    assert_eq!(reply.get("status").as_str(), Some("error"), "{reply}");
    panic_streams_then_fails(&published);
    // The session goes on, as it was before the failed cell.
    let (_, _, published) = request(&mut shell, "execute_request", exec("x"));
    assert!(shows(&published, "40"), "{published:?}");
    completion_inspect_history(&mut shell, &mut request);
    // The package's items and bindings, `await` and `it`, as in the REPL.
    let (_, _, published) = request(&mut shell, "execute_request", exec("add(base, 2)"));
    assert!(shows(&published, "42"), "{published:?}");
    let code = "async fn af() -> i32 { return 3; }\nlet v = await af();\nv + it";
    let (_, _, published) = request(&mut shell, "execute_request", exec(code));
    assert!(shows(&published, "45"), "{published:?}");
    let (_, _, published) = request(&mut shell, "execute_request", exec("await af()"));
    assert!(shows(&published, "3"), "{published:?}");
    let (_, _, published) = request(&mut shell, "execute_request", exec("v * it"));
    assert!(shows(&published, "9"), "{published:?}");

    // Values and failures show as in the REPL.
    let (_, _, published) = request(&mut shell, "execute_request", exec("[1, 2, 3]"));
    assert!(shows(&published, "[1, 2, 3]"), "{published:?}");
    rich_values_display(&mut shell, &mut request);
    let (_, reply, _) = request(
        &mut shell,
        "execute_request",
        exec("std::process::exit(3);"),
    );
    assert_eq!(
        reply.get("evalue").as_str(),
        Some(
            "error: the cell called exit(3); the session is unchanged (`exit` or `:quit` ends the session)"
        ),
        "{reply}"
    );

    let (kind, _, _) = request(
        &mut control,
        "shutdown_request",
        Json::obj([("restart", Json::Bool(false))]),
    );
    assert_eq!(kind, "shutdown_reply");
    let out = kernel.wait_with_output().unwrap();
    let mut rest = String::new();
    stderr.read_to_string(&mut rest).unwrap();
    assert!(out.status.success(), "{rest}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The panic message of a failed cell streams as it is written; `error`
/// then says what became of the cell.
pub(crate) fn panic_streams_then_fails(published: &[(String, nabic_interface::Json)]) {
    let at = |kind: &str| published.iter().position(|(k, _)| k == kind);
    let (said, error) = (at("stream").expect("stream"), at("error").expect("error"));
    assert!(said < error, "{published:?}");
    assert_eq!(published[said].1.get("name").as_str(), Some("stderr"));
    assert!(
        published[said].1.to_string().contains("boom"),
        "{published:?}"
    );
    assert_eq!(
        published[error].1.get("evalue").as_str(),
        Some("error: the cell panicked; the session is unchanged")
    );
}

/// Output streamed while a cell runs, and stdin through `input_request`,
/// on the kernel of `jupyter_kernel_speaks_the_wire_protocol` (cells kept
/// out of its history).
pub(crate) fn streams_and_asks(
    key: &[u8],
    shell: &mut std::net::TcpStream,
    iopub: &mut std::net::TcpStream,
    stdin: &mut std::net::TcpStream,
) {
    let execute = |shell: &mut std::net::TcpStream, code: &str, allow_stdin: bool| {
        let content = Json::obj([
            ("code", Json::str(code)),
            ("silent", Json::Bool(false)),
            ("store_history", Json::Bool(false)),
            ("allow_stdin", Json::Bool(allow_stdin)),
        ]);
        let frames = encode(
            key,
            "test",
            &[],
            "execute_request",
            &Json::obj([]),
            &content,
        );
        let header = parse_msg(key, frames.clone()).unwrap().header;
        write_msg(shell, &frames).unwrap();
        header
    };
    // The stdout streamed for `id` until it holds `text`.
    let streamed = |iopub: &mut std::net::TcpStream, id: &Json, text: &str| {
        let mut got = String::new();
        while !got.contains(text) {
            let m = parse_msg(key, read_msg(iopub).unwrap()).expect("signed iopub");
            if m.parent.get("msg_id") == id
                && m.kind() == "stream"
                && m.content.get("name").as_str() == Some("stdout")
            {
                got += m.content.get("text").as_str().unwrap_or("");
            }
        }
        got
    };
    let reply = |shell: &mut std::net::TcpStream, id: &Json| {
        let m = parse_msg(key, read_msg(shell).unwrap()).expect("a signed reply");
        assert_eq!(m.parent.get("msg_id"), id);
        m.content
    };

    // `early` arrives while the cell still sleeps.
    let code = "use std::thread;\nprintln(\"early\");\n\
                thread::sleep(Duration::ms(1500));\nprintln(\"late\");";
    let id = execute(shell, code, true).get("msg_id").clone();
    assert_eq!(streamed(iopub, &id, "early\n"), "early\n");
    shell.set_nonblocking(true).unwrap();
    let pending = shell.peek(&mut [0u8]);
    shell.set_nonblocking(false).unwrap();
    assert!(
        pending
            .as_ref()
            .is_err_and(|e| e.kind() == std::io::ErrorKind::WouldBlock),
        "the reply came before the output: {pending:?}"
    );
    assert_eq!(reply(shell, &id).get("status").as_str(), Some("ok"));
    assert_eq!(streamed(iopub, &id, "late\n"), "late\n");

    // A read asks the client, after the prompt the cell printed.
    let code = "print(\"name? \");\n\
                let name = (try io::read_line() catch _ { \"failed\" }) ?? \"none\";\n\
                println(\"hello [\" + name + \"]\");";
    let header = execute(shell, code, true);
    let id = header.get("msg_id").clone();
    let ask = parse_msg(key, read_msg(stdin).unwrap()).expect("a signed input_request");
    assert_eq!(ask.kind(), "input_request");
    assert_eq!(ask.parent.get("msg_id"), &id);
    assert_eq!(ask.content.get("password"), &Json::Bool(false));
    assert_eq!(streamed(iopub, &id, "name? "), "name? ");
    let answer = Json::obj([("value", Json::str("nabi"))]);
    let frames = encode(key, "test", &[], "input_reply", &ask.header, &answer);
    write_msg(stdin, &frames).unwrap();
    assert_eq!(reply(shell, &id).get("status").as_str(), Some("ok"));
    assert_eq!(streamed(iopub, &id, "hello [nabi]\n"), "hello [nabi]\n");

    // Without `allow_stdin` a read is the end of input.
    let code = "let line = (try io::read_line() catch _ { \"failed\" }) ?? \"\";\n\
                println(line.len());";
    let id = execute(shell, code, false).get("msg_id").clone();
    assert_eq!(reply(shell, &id).get("status").as_str(), Some("ok"));
    assert_eq!(streamed(iopub, &id, "0\n"), "0\n");
}

/// A REPL message: (reply kind, reply content, iopub messages).
pub(crate) type Reply = (
    String,
    nabic_interface::Json,
    Vec<(String, nabic_interface::Json)>,
);

/// A value with `RichDisplay` (the package's `Dot`) goes out as
/// `display_data`, a cell's last one and `display(x)` mid-cell alike.
pub(crate) fn rich_values_display(
    shell: &mut std::net::TcpStream,
    request: &mut impl FnMut(&mut std::net::TcpStream, &str, nabic_interface::Json) -> Reply,
) {
    let exec = |code: &str| Json::obj([("code", Json::str(code)), ("silent", Json::Bool(false))]);
    let rich = "println(\"before\");\ndisplay(Dot {});\nDot {}";
    let (_, reply, published) = request(shell, "execute_request", exec(rich));
    assert_eq!(reply.get("status").as_str(), Some("ok"), "{reply}");
    let svg = |c: &Json| c.get("data").get("image/svg+xml").as_str() == Some("<svg/>");
    let shown: Vec<&str> = published
        .iter()
        .filter_map(|(k, c)| match k.as_str() {
            "display_data" if svg(c) => Some("svg"),
            "stream" => c.get("text").as_str(),
            "execute_result" => Some("result"),
            _ => None,
        })
        .collect();
    assert_eq!(shown, ["before\n", "svg", "svg"], "{published:?}");
}

/// `complete` / `is_complete` / `inspect` / `history` against the
/// session of `jupyter_kernel_speaks_the_wire_protocol` (`x: i32`, five
/// cells run).
/// Cells are numbered after the execution count (2 so far): `N.k` when
/// one execute splits; a silent execute leaves the count as it is.
pub(crate) fn cells_follow_the_count(
    shell: &mut std::net::TcpStream,
    request: &mut impl FnMut(&mut std::net::TcpStream, &str, nabic_interface::Json) -> Reply,
) {
    let exec = |code: &str| Json::obj([("code", Json::str(code)), ("silent", Json::Bool(false))]);
    let panicked_in = |published: &[(String, Json)], cell: &str| {
        published
            .iter()
            .any(|(k, c)| k == "stream" && c.to_string().contains(&format!("panicked at {cell}:")))
    };
    let code = "let y = 1;\npanic(\"two\");";
    let (_, reply, published) = request(shell, "execute_request", exec(code));
    assert_eq!(reply.get("execution_count"), &Json::num(3));
    assert!(panicked_in(&published, "<repl cell 3.2>"), "{published:?}");
    let silent = Json::obj([("code", Json::str("x;")), ("silent", Json::Bool(true))]);
    request(shell, "execute_request", silent);
    let (_, reply, published) = request(shell, "execute_request", exec("panic(\"four\");"));
    assert_eq!(reply.get("execution_count"), &Json::num(4));
    assert!(panicked_in(&published, "<repl cell 4>"), "{published:?}");
}

pub(crate) fn completion_inspect_history(
    shell: &mut std::net::TcpStream,
    request: &mut impl FnMut(&mut std::net::TcpStream, &str, nabic_interface::Json) -> Reply,
) {
    let (_, reply, _) = request(
        shell,
        "is_complete_request",
        Json::obj([("code", Json::str("fn f() {"))]),
    );
    assert_eq!(reply.get("status").as_str(), Some("incomplete"));
    let (_, reply, _) = request(
        shell,
        "complete_request",
        Json::obj([("code", Json::str("1 + x")), ("cursor_pos", Json::num(5))]),
    );
    assert_eq!(
        reply.get("matches"),
        &Json::Arr(vec![Json::str("x")]),
        "{reply}"
    );
    assert_eq!(reply.get("cursor_start"), &Json::num(4));
    let (_, reply, _) = request(
        shell,
        "complete_request",
        Json::obj([("code", Json::str("whi")), ("cursor_pos", Json::num(3))]),
    );
    assert_eq!(
        reply.get("matches"),
        &Json::Arr(vec![Json::str("while")]),
        "{reply}"
    );
    let inspect = |pos: u32| {
        Json::obj([
            ("code", Json::str("1 + x + nope")),
            ("cursor_pos", Json::num(pos)),
            ("detail_level", Json::num(0)),
        ])
    };
    let (kind, reply, _) = request(shell, "inspect_request", inspect(4));
    assert_eq!(kind, "inspect_reply");
    assert_eq!(reply.get("found"), &Json::Bool(true), "{reply}");
    assert_eq!(reply.get("data").get("text/plain").as_str(), Some("x: i32"));
    let (_, reply, _) = request(shell, "inspect_request", inspect(10));
    assert_eq!(reply.get("found"), &Json::Bool(false), "{reply}");
    let (_, reply, _) = request(
        shell,
        "history_request",
        Json::obj([
            ("hist_access_type", Json::str("tail")),
            ("n", Json::num(2)),
            ("output", Json::Bool(false)),
            ("raw", Json::Bool(true)),
        ]),
    );
    assert_eq!(
        reply.get("history"),
        &Json::Arr(vec![
            Json::Arr(vec![
                Json::num(0),
                Json::num(6),
                Json::str("panic(\"boom\");")
            ]),
            Json::Arr(vec![Json::num(0), Json::num(7), Json::str("x")]),
        ]),
        "{reply}"
    );

    // Members, paths and doc comments.
    let code = "struct P { x: i32; y: i32; }\n\
                impl P {\n    /// Sum of the coordinates.\n    fn sum(self) -> i32 { self.x + self.y }\n}";
    let exec = Json::obj([("code", Json::str(code)), ("silent", Json::Bool(false))]);
    let (_, reply, _) = request(shell, "execute_request", exec);
    assert_eq!(reply.get("status").as_str(), Some("ok"), "{reply}");
    let complete = |code: &str| {
        Json::obj([
            ("code", Json::str(code)),
            ("cursor_pos", Json::num(code.chars().count() as u32)),
        ])
    };
    let (_, reply, _) = request(
        shell,
        "complete_request",
        complete("let p = P { x: 1, y: 2 };\np.s"),
    );
    assert_eq!(
        reply.get("matches"),
        &Json::Arr(vec![Json::str("sum")]),
        "{reply}"
    );
    assert_eq!(reply.get("cursor_start"), &Json::num(28));
    let (_, reply, _) = request(
        shell,
        "complete_request",
        complete("let p = P { x: 1, y: 2 };\np."),
    );
    for m in ["x", "y", "sum"] {
        assert!(reply.get("matches").to_string().contains(m), "{reply}");
    }
    let (_, reply, _) = request(shell, "complete_request", complete("P::"));
    assert!(
        reply.get("matches").to_string().contains("\"sum\""),
        "{reply}"
    );
    let code = "let p = P { x: 1, y: 2 };\np.sum()";
    let (_, reply, _) = request(
        shell,
        "inspect_request",
        Json::obj([
            ("code", Json::str(code)),
            ("cursor_pos", Json::num(28)),
            ("detail_level", Json::num(0)),
        ]),
    );
    assert_eq!(
        reply.get("data").get("text/plain").as_str(),
        Some("fn sum(self: P) -> i32\n\nSum of the coordinates."),
        "{reply}"
    );
}
