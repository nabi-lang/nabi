//! The registry: index packages, publishing, tokens and yanks.

#[allow(clippy::wildcard_imports)]
use crate::*;

pub(crate) const ONE: &str = "pub fn one() -> i32 { return 1; }\n";

/// The index of a bare requirement.
pub(crate) const DEFAULT: &str = "https://github.com/nabi-lang/packages-index";

/// `home`'s sources.nabion fetches the default index from `index`.
pub(crate) fn mirror(home: &Path, index: &str) {
    write(
        &home.join("sources.nabion"),
        &format!(
            "Sources {{ replace: {{ {}: {} }} }}\n",
            q(DEFAULT),
            q(index)
        ),
    );
}

/// A package's repository at `dir`: `files` plus a `nabi.nabion` for `name`
/// `vers` with `deps`, committed, with remote `origin` `origin(dir)`.
pub(crate) fn package(
    dir: &Path,
    name: &str,
    vers: &str,
    deps: &str,
    files: &[(&str, &str)],
) -> String {
    package_meta(dir, name, vers, deps, "", files)
}

/// `package` with manifest fields `meta` (`license: "MIT", `).
pub(crate) fn package_meta(
    dir: &Path,
    name: &str,
    vers: &str,
    deps: &str,
    meta: &str,
    files: &[(&str, &str)],
) -> String {
    let floor = if meta.contains("nabi_version") {
        String::new()
    } else {
        format!("nabi_version: \"{}\", ", nabi_version())
    };
    let manifest = format!(
        "Package {{ name: \"{name}\", version: \"{vers}\", {floor}{meta}deps: {{ {deps} }} }}\n"
    );
    let mut all = files.to_vec();
    all.push(("nabi.nabion", &manifest));
    let c = commit(dir, &all);
    if git(dir, &["remote"]).is_empty() {
        git(dir, &["remote", "add", "origin", &origin(dir)]);
    }
    c
}

/// `name` / `vers` pairs of the registry packages in `nabi.lock`.
pub(crate) fn locked_packages(lock: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut name = "";
    for l in lock.lines().map(str::trim) {
        if let Some(n) = l.strip_prefix("name: ") {
            name = n.trim_end_matches(',');
        } else if let Some(v) = l.strip_prefix("version: \"") {
            out.push(format!(
                "{}@{}",
                name.trim_matches('"'),
                v.trim_end_matches(',').trim_matches('"')
            ));
        }
    }
    out
}

/// Publish `num` 1.0.0, 1.1.0, 2.0.0 and `geom` 0.3.0 (needing `num`
/// ^1.0) to the index checkout `index`, checking `publish` on the way;
/// `num` 1.0.0's tree.
pub(crate) fn publish_packages(root: &Path, home: &Path, index: &Path) -> String {
    let reg = index.display().to_string();
    let index_log = || git(index, &["log", "--format=%s"]);
    let one = ONE;
    // `num` 1.0.0: a dry run prints the line and writes nothing.
    let num = root.join("num");
    let c1 = package(&num, "num", "1.0.0", "", &[("src/lib.nabi", one)]);
    let tree1 = git(&num, &["rev-parse", "HEAD^{tree}"]);
    let line = format!(
        "Package {{ name: \"num\", version: \"1.0.0\", deps: [], source: Dep::git({}, rev: \
         \"{c1}\"), tree: \"{tree1}\", yanked: false, nabi_version: \"{}\" }}\n",
        q(origin(&num)),
        nabi_version()
    );
    let out = nabi(home, &num, &["publish", "--dry-run"]);
    assert_eq!(ok(&out), line);
    let note = |o: &Output| text(&o.stderr).trim().to_owned();
    assert_eq!(
        note(&out),
        format!(
            "note: dry run; `nabi publish` sends it to the registry API of {DEFAULT} (needs \
             `nabi login`), which adds it as 3/n/num"
        )
    );
    let out = nabi(home, &num, &["publish", "--index-dir", &reg, "--dry-run"]);
    assert_eq!(ok(&out), line);
    assert_eq!(
        note(&out),
        format!("note: dry run; `nabi publish` commits it to {reg} as 3/n/num (not pushed)")
    );
    fails(
        &nabi(
            home,
            &num,
            &["publish", "--index-dir", &reg, "--registry", &reg],
        ),
        "`--index-dir` and `--registry` go apart",
    );
    assert!(!index.join("3/n/num").exists());
    // Without --index-dir, it goes through the registry API: a token first.
    fails(&nabi(home, &num, &["publish"]), "run `nabi login` first");
    ok(&nabi(home, &num, &["publish", "--index-dir", &reg]));
    assert_eq!(read(&index.join("3/n/num")), line);
    assert!(
        index_log().starts_with("Publish num 1.0.0\n"),
        "{}",
        index_log()
    );
    fails(
        &nabi(home, &num, &["publish", "--index-dir", &reg]),
        "num 1.0.0 is already in the index",
    );
    write(&num.join("src/lib.nabi"), "dirty");
    fails(
        &nabi(home, &num, &["publish", "--dry-run"]),
        "uncommitted changes",
    );

    // More versions, and `geom` 0.3.0 needing `num` ^1.0.
    for v in ["1.1.0", "2.0.0"] {
        package(&num, "num", v, "", &[("src/lib.nabi", one)]);
        ok(&nabi(home, &num, &["publish", "--index-dir", &reg]));
    }
    let geom = root.join("geom");
    let two = "use num::one;\npub fn two() -> i32 { return one() + one(); }\n";
    // Manifest metadata goes to the registry API, not the entry.
    package_meta(
        &geom,
        "geom",
        "0.3.0",
        "\"num\": \"^1.0\"",
        "description: \"Shapes\", license: \"MIT\", repository: \"https://example.com/geom\", ",
        &[("src/lib.nabi", two)],
    );
    ok(&nabi(home, &geom, &["publish", "--index-dir", &reg]));
    let entry = read(&index.join("ge/om/geom"));
    assert!(
        entry.contains("deps: [{ name: \"num\", req: \"^1.0\", kind: \"normal\" }]"),
        "{entry}"
    );
    assert!(
        !entry.contains("Shapes") && !entry.contains("license"),
        "{entry}"
    );
    // A path dependency is published only with a version requirement.
    let bad = root.join("bad");
    package(
        &bad,
        "bad",
        "0.1.0",
        "\"x\": Dep::path(\"..\")",
        &[("src/lib.nabi", one)],
    );
    fails(
        &nabi(home, &bad, &["publish", "--dry-run"]),
        "dependency `x` is a path: a registry package depends only on registry packages; add \
         the version it needs, published from that path: `Dep::path(\"..\", version: \"^0.1\")`",
    );
    let both = root.join("both");
    package(
        &both,
        "both",
        "0.1.0",
        "\"num\": Dep::path(\"../num\", version: \"^2.0\")",
        &[("src/lib.nabi", two)],
    );
    let line = ok(&nabi(home, &both, &["publish", "--dry-run"]));
    assert!(line.contains("name: \"num\", req: \"^2.0\""), "{line}");
    let old = root.join("old");
    package(
        &old,
        "old",
        "0.1.0",
        "\"num\": Dep::path(\"../num\", version: \"^1.0\")",
        &[("src/lib.nabi", two)],
    );
    fails(
        &nabi(home, &old, &["check"]),
        "is version 2.0.0, which its `version: \"^1.0\"` does not accept",
    );
    // The source is `origin` as configured, before the publisher's own
    // `insteadOf` rewrites, and must be https.
    let ssh = root.join("ssh");
    package(&ssh, "ssh", "0.1.0", "", &[("src/lib.nabi", one)]);
    git(
        &ssh,
        &[
            "config",
            "url.git@github.com:.insteadOf",
            "https://github.com/",
        ],
    );
    git(
        &ssh,
        &["remote", "set-url", "origin", "https://github.com/o/ssh"],
    );
    let line = ok(&nabi(home, &ssh, &["publish", "--dry-run"]));
    assert!(
        line.contains("source: Dep::git(\"https://github.com/o/ssh\""),
        "{line}"
    );
    git(
        &ssh,
        &["remote", "set-url", "origin", "git@github.com:o/ssh.git"],
    );
    fails(
        &nabi(home, &ssh, &["publish", "--dry-run"]),
        "a registry package's source must be an https:// URL; run `git remote set-url origin \
         https://github.com/o/ssh.git`",
    );
    git(&ssh, &["remote", "remove", "origin"]);
    fails(
        &nabi(home, &ssh, &["publish", "--dry-run"]),
        "add one with `git remote add origin https://<host>/<owner>/<repo>`",
    );

    tree1
}

/// Package metadata (`authors`, `homepage`, `documentation`, `readme`,
/// `keywords`, `publish`) is checked at publish; index entries from a
/// newer nabi, with fields this one does not know, still resolve.
#[test]
fn publish_writes_toolchain() {
    let root = scratch("pkg_meta");
    let home = root.join("home");
    let index = root.join("index");
    commit(
        &index,
        &[("config.nabion", "Config { api: \"http://127.0.0.1:1\" }\n")],
    );
    let reg = index.display().to_string();
    mirror(&home, &reg);
    let meta = root.join("meta");
    let fields = "authors: [\"Ann <a@x.org>\"], homepage: \"https://x.org\", documentation: \
                  \"https://x.org/doc\", keywords: [\"geo\", \"math\"], license: \"MIT\", ";
    let files = [("src/lib.nabi", ONE), ("README.md", "# meta\n")];
    // `fields` with `extra`; `extra` keywords replace the default ones.
    let publish = |extra: &str| {
        let base = if extra.starts_with("keywords") {
            fields.replace("keywords: [\"geo\", \"math\"], ", "")
        } else {
            fields.to_owned()
        };
        package_meta(
            &meta,
            "meta",
            "0.1.0",
            "",
            &format!("{base}{extra}"),
            &files,
        );
        nabi(&home, &meta, &["publish", "--dry-run"])
    };
    // Checked, not in the entry; the entry carries `nabi_version`.
    let line = ok(&publish(""));
    assert!(
        line.ends_with(&format!(
            "yanked: false, nabi_version: \"{}\" }}\n",
            nabi_version()
        )),
        "{line}"
    );
    // A project pin stays out of the entry.
    let pinned = ok(&publish(&format!("toolchain: \"{}\", ", series())));
    assert!(!pinned.contains("toolchain:"), "{pinned}");
    // A publish needs `nabi_version`, one that admits this nabi.
    fails(
        &publish("nabi_version: \"0.0.1\", "),
        "`nabi_version: \"0.0.1\"` admits nabi 0.0.1 or a later 0.0.x",
    );
    // The optional manifest fields, and an unknown one, which warns.
    let out = publish(
        "license_file: \"LICENSE\", categories: [\"math\"], include: [\"src/**\"], links: \
         \"m\", metadata: { a: 1 }, colour: \"red\", ",
    );
    fails(
        &out,
        "`license_file: \"LICENSE\"` in nabi.nabion: not a file inside the package",
    );
    assert!(text(&out.stderr).contains("unknown field `colour` is ignored"));
    write(&meta.join("LICENSE"), "MIT\n");
    let out = publish(
        "license_file: \"LICENSE\", categories: [\"math\"], include: [\"src/**\"], links: \
         \"m\", metadata: { a: 1 }, default_run: \"meta\", ",
    );
    fails(&out, "the package's executables are []");
    let out = publish(
        "license_file: \"LICENSE\", categories: [\"math\"], include: [\"src/**\"], links: \
         \"m\", metadata: { a: 1 }, ",
    );
    assert!(ok(&out).ends_with("links: \"m\" }\n"), "{}", ok(&out));
    fails(&publish("readme: \"doc/R.md\", "), "`readme: \"doc/R.md\"`");
    fails(
        &publish("readme: \"../meta/README.md\", "),
        "is outside the package; write a path inside it",
    );
    ok(&publish("readme: \"README.md\", "));
    fails(
        &publish("keywords: [\"a\", \"b\", \"c\", \"d\", \"e\", \"f\"], "),
        "`keywords` has 6 entries; keep at most 5",
    );
    fails(
        &publish("publish: false, "),
        "`meta` has `publish: false` in nabi.nabion, so it is not published; remove that line \
         (or write `publish: true`)",
    );
    // An entry with fields this nabi does not know still resolves.
    let newer = line.replace(" }\n", ", later: [\"x\"] }\n");
    assert_ne!(newer, line);
    commit(&index, &[("me/ta/meta", &newer)]);
    let app = root.join("app");
    write(&app.join("src/main.nabi"), "println(\"x\");\n");
    write(&app.join("nabi.nabion"), APP0);
    ok(&nabi(&home, &app, &["add", "meta"]));
    assert_eq!(
        locked_packages(&read(&app.join("nabi.lock"))),
        ["meta@0.1.0"]
    );
}

/// A dependency on the package itself (here a test-only one) is an
/// error: `pkg::` is how a package names its own items.
#[test]
fn self_dependency_is_rejected() {
    let root = scratch("self_dep");
    let home = root.join("home");
    let lib = root.join("me");
    write(&lib.join("src/lib.nabi"), "pub fn f() -> i32 { 1 }\n");
    write(
        &lib.join("nabi.nabion"),
        "Package {\n    name: \"me\",\n    dev_deps: { \"me\": Dep::path(\".\") },\n}\n",
    );
    fails(
        &nabi(&home, &lib, &["check"]),
        "dependency `me` (required by the root package) is the package being built",
    );
}

/// A stand-in for the registry API on `http://127.0.0.1:<port>` (the
/// port): token `rks_ok` is account `alice`, who may publish;
/// `rks_reader` is `bob`, who may not; others get 401. Each name and
/// version publishes once. Every request goes to `log` as
/// `<method> <path> [<Authorization>] <body>`.
pub(crate) fn fake_api(log: Arc<Mutex<Vec<String>>>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let mut published = Vec::new();
        for conn in listener.incoming() {
            let mut conn = conn.unwrap();
            let mut reader = BufReader::new(conn.try_clone().unwrap());
            let mut start = String::new();
            reader.read_line(&mut start).unwrap();
            let (mut auth, mut len) = (String::new(), 0);
            loop {
                let mut header = String::new();
                reader.read_line(&mut header).unwrap();
                let Some((key, value)) = header.trim_end().split_once(": ") else {
                    break;
                };
                match key.to_ascii_lowercase().as_str() {
                    "authorization" => value.clone_into(&mut auth),
                    "content-length" => len = value.parse().unwrap(),
                    _ => {}
                }
            }
            let mut body = vec![0; len];
            reader.read_exact(&mut body).unwrap();
            let body = String::from_utf8(body).unwrap();
            let mut words = start.split(' ');
            let (method, path) = (words.next().unwrap(), words.next().unwrap());
            log.lock()
                .unwrap()
                .push(format!("{method} {path} [{auth}] {body}"));
            let field = |f: &str| {
                let rest = body.split(&format!("{f}: \"")).nth(1).unwrap_or("");
                rest.split('"').next().unwrap().to_owned()
            };
            let (code, reply) = match (auth.as_str(), path) {
                ("Bearer rks_ok", "/api/v1/me") => {
                    (200, "Me { login: \"alice\", publish: true }".to_owned())
                }
                ("Bearer rks_reader", "/api/v1/me") => {
                    (200, "Me { login: \"bob\", publish: false }".to_owned())
                }
                ("Bearer rks_ok", "/api/v1/publish") => {
                    let (n, v) = (field("name"), field("version"));
                    if published.contains(&(n.clone(), v.clone())) {
                        let m = format!("{n} {v} is already in the index");
                        (409, format!("Error {{ message: \"{m}\" }}"))
                    } else {
                        let reply = format!(
                            "Published {{ name: \"{n}\", version: \"{v}\", file: \"3/n/{n}\", \
                             warnings: [\"no README\"], later: 1 }}"
                        );
                        published.push((n, v));
                        (200, reply)
                    }
                }
                ("Bearer rks_ok", "/api/v1/yank") => (
                    200,
                    format!(
                        "Yanked {{ name: \"{}\", version: \"{}\", yanked: true }}",
                        field("name"),
                        field("version")
                    ),
                ),
                ("", "/api/v1/packages/num") => (
                    200,
                    "Package { name: \"num\", versions: [Version { version: \"1.0.0\", meta: \
                     Meta { description: \"Numbers\" }, downloads: 3 }] }"
                        .to_owned(),
                ),
                _ => (401, "Error { message: \"bad token\" }".to_owned()),
            };
            let _ = write!(
                conn,
                "HTTP/1.1 {code} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                reply.len()
            );
        }
    });
    port
}

/// `login` / `logout`, and `publish` / `yank` without --index: through
/// the registry API named by the index's config.nabion (`fake_api`).
#[test]
fn login_publish_yank_api() {
    let root = scratch("api");
    let home = root.join("home");
    let log = Arc::new(Mutex::new(Vec::new()));
    let api = format!("http://127.0.0.1:{}", fake_api(log.clone()));
    let last = || log.lock().unwrap().last().cloned().unwrap_or_default();
    let index = root.join("index");
    commit(
        &index,
        &[("config.nabion", &format!("Config {{ api: \"{api}\" }}\n"))],
    );
    let reg = index.display().to_string();
    mirror(&home, &reg);
    let at = |args: &[&str]| nabi_cmd(&home, &root.join("num"), args);
    // Every output, checked never to hold a token.
    let done = |c: &mut Command| {
        let out = c.output().unwrap();
        for t in ["rks_ok", "rks_reader", "rks_nope"] {
            assert!(!text(&out.stdout).contains(t) && !text(&out.stderr).contains(t));
        }
        out
    };
    let run = |args: &[&str]| done(&mut at(args));
    package(
        &root.join("num"),
        "num",
        "1.0.0",
        "",
        &[("src/lib.nabi", ONE)],
    );

    let out = run(&["logout"]);
    ok(&out);
    assert!(text(&out.stderr).contains("not logged in"));
    fails(&run(&["publish"]), "run `nabi login` first");
    fails(&run(&["yank", "num@1.0.0"]), "run `nabi login` first");
    // A dry run needs no token and sends nothing.
    ok(&run(&["publish", "--dry-run"]));
    assert!(log.lock().unwrap().is_empty());

    // A bad token, and an account that may not publish: nothing saved.
    fails(
        &run(&["login", "rks_nope"]),
        &format!("{api} refused the token: create one at {api}/me"),
    );
    assert_eq!(last(), "GET /api/v1/me [Bearer rks_nope] ");
    fails(
        &run(&["login", "rks_reader"]),
        "this account cannot publish yet: publishing is open to the registry's maintainers only",
    );
    let keyed = home.join("tokens.nabion");
    assert!(!keyed.exists());

    // The token from stdin.
    let mut child = at(&["login"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    writeln!(child.stdin.take().unwrap(), "rks_ok").unwrap();
    let out = child.wait_with_output().unwrap();
    ok(&out);
    assert!(text(&out.stderr).contains("Logged in as alice"));
    assert!(!text(&out.stderr).contains("rks_ok") && !text(&out.stdout).contains("rks_ok"));
    // The token goes into tokens.nabion.
    assert!(
        read(&keyed).starts_with("Tokens {") && read(&keyed).contains(": \"rks_ok\""),
        "{}",
        read(&keyed)
    );
    #[cfg(unix)]
    {
        assert_eq!(
            fs::metadata(&keyed).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    // Publish: the body is the entry line and the metadata; the reply's
    // warnings are shown, its unknown fields skipped. A second time, the
    // API's error is relayed.
    let out = run(&["publish"]);
    let line = ok(&out);
    assert!(line.starts_with("Package { name: \"num\", version: \"1.0.0\""));
    let body = last();
    assert!(
        body.starts_with(&format!(
            "POST /api/v1/publish [Bearer rks_ok] Publish {{ entry: {}, meta: {{ ",
            line.trim_end()
        )),
        "{body}"
    );
    for want in ["has_lib: true", "bin_names: []", "size: "] {
        assert!(body.contains(want), "{body}");
    }
    assert!(text(&out.stderr).contains(&format!("Published num 1.0.0 to {api}")));
    assert!(text(&out.stderr).contains(&format!("warning: {api}: no README")));
    fails(
        &run(&["publish"]),
        &format!("{api}: num 1.0.0 is already in the index"),
    );

    // `search` reads descriptions from the API.
    commit(&index, &[("3/n/num", &line)]);
    assert_eq!(ok(&run(&["search", "nu"])), "num 1.0.0 - Numbers\n");
    assert_eq!(last(), "GET /api/v1/packages/num [] ");

    // Yank with the environment's token (the default index), no file; a
    // bad one is refused.
    ok(&run(&["logout"]));
    assert!(!read(&keyed).contains("rks_ok"));
    let out = done(
        at(&["yank", "num@1.0.0", "--message", "broken build"])
            .env("NABI_REGISTRY_TOKEN", "rks_ok"),
    );
    ok(&out);
    assert_eq!(
        last(),
        "POST /api/v1/yank [Bearer rks_ok] Yank { name: \"num\", version: \"1.0.0\", undo: \
         false, native: false, message: \"broken build\" }"
    );
    fails(
        &run(&["yank", "--undo", "num@1.0.0", "--message", "x"]),
        "`--undo` takes none",
    );
    ok(&done(
        at(&["yank", "--native", "--undo", "z@2.0.0"]).env("NABI_REGISTRY_TOKEN", "rks_ok"),
    ));
    assert_eq!(
        last(),
        "POST /api/v1/yank [Bearer rks_ok] Yank { name: \"z\", version: \"2.0.0\", undo: true, \
         native: true }"
    );
    fails(
        &done(at(&["yank", "num@1.0.0"]).env("NABI_REGISTRY_TOKEN", "rks_nope")),
        "refused the token",
    );

    // The API must be HTTPS off the loopback host.
    commit(
        &index,
        &[("config.nabion", "Config { api: \"http://example.com\" }\n")],
    );
    fails(
        &run(&["login", "rks_ok"]),
        "api `http://example.com` must be an https:// URL",
    );
}

/// A token from `nabi login --registry <url>` goes only to that index;
/// `logout --registry` and `--all` remove it.
#[test]
fn token_goes_only_to_its_index() {
    let root = scratch("api-index");
    let home = root.join("home");
    let log = Arc::new(Mutex::new(Vec::new()));
    let api = format!("http://127.0.0.1:{}", fake_api(log.clone()));
    let last = || log.lock().unwrap().last().cloned().unwrap_or_default();
    let (a, b) = (root.join("a"), root.join("b"));
    for i in [&a, &b] {
        commit(
            i,
            &[("config.nabion", &format!("Config {{ api: \"{api}\" }}\n"))],
        );
    }
    let (ra, rb) = (a.display().to_string(), b.display().to_string());
    package(
        &root.join("num"),
        "num",
        "1.0.0",
        "",
        &[("src/lib.nabi", ONE)],
    );
    mirror(&home, &ra);
    let run = |args: &[&str]| nabi_cmd(&home, &root.join("num"), args).output().unwrap();
    ok(&run(&["login", "--registry", &rb, "rks_ok"]));
    fails(&run(&["yank", "num@1.0.0"]), "run `nabi login` first");
    ok(&run(&["yank", "num@1.0.0", "--registry", &rb]));
    assert_eq!(
        last(),
        "POST /api/v1/yank [Bearer rks_ok] Yank { name: \"num\", version: \"1.0.0\", undo: \
         false, native: false }"
    );
    // `NABI_REGISTRY_TOKEN` is not sent to another index.
    let out = nabi_cmd(
        &home,
        &root.join("num"),
        &["yank", "num@1.0.0", "--registry", &ra],
    )
    .env("NABI_REGISTRY_TOKEN", "rks_ok")
    .output()
    .unwrap();
    fails(&out, &format!("run `nabi login --registry {ra}` first"));
    assert!(
        text(&out.stderr).contains(&format!("NABI_REGISTRY_TOKEN goes only to {DEFAULT}")),
        "{}",
        text(&out.stderr)
    );
    fails(&run(&["publish"]), "run `nabi login` first");
    ok(&run(&["publish", "--registry", &rb]));
    ok(&run(&["logout", "--registry", &rb]));
    fails(
        &run(&["yank", "num@1.0.0", "--registry", &rb]),
        &format!("run `nabi login --registry {rb}` first"),
    );
    ok(&run(&["login", "--registry", &rb, "rks_ok"]));
    ok(&run(&["login", "rks_ok"]));
    fails(
        &run(&["logout", "--all", "--registry", &rb]),
        "give `--all` or",
    );
    ok(&run(&["logout", "--all"]));
    for r in [&ra, &rb] {
        fails(&run(&["yank", "num@1.0.0", "--registry", r]), "first");
    }
}

/// An index entry whose `toolchain` this nabi does not meet is skipped,
/// with a note when that hides the newest match; a lone match is an
/// error that says which toolchain it needs.
#[test]
fn resolver_skips_other_series() {
    let root = scratch("registry-series");
    let home = root.join("home");
    let index = root.join("index");
    commit(
        &index,
        &[("config.nabion", "Config { api: \"http://127.0.0.1:1\" }\n")],
    );
    for kv in [
        ["user.name", "t"],
        ["user.email", "t@t"],
        ["commit.gpgsign", "false"],
    ] {
        git(&index, &["config", kv[0], kv[1]]);
    }
    let reg = index.display().to_string();
    mirror(&home, &reg);
    publish_packages(&root, &home, &index);
    // num 2.0.0 needs another series, 1.1.0 this one (as a newer nabi
    // writes them).
    let file = read(&index.join("3/n/num"));
    let mine = format!("nabi_version: \"{}\"", nabi_version());
    let pin = |l: &str, t: &str| l.replace(&mine, &format!("nabi_version: \"{t}\""));
    let other: String = file
        .lines()
        .map(|l| {
            if l.contains("version: \"2.0.0\"") {
                pin(l, "99.0.0")
            } else if l.contains("version: \"1.1.0\"") {
                pin(l, &nabi_version())
            } else {
                l.to_owned()
            }
        })
        .map(|l| l + "\n")
        .collect();
    assert_ne!(other, file);
    commit(&index, &[("3/n/num", &other)]);

    let app = root.join("app");
    write(&app.join("src/main.nabi"), "println(\"x\");\n");
    write(&app.join("nabi.nabion"), APP0);
    ok(&nabi(&home, &app, &["add", "num"]));
    assert!(read(&app.join("nabi.nabion")).contains("\"num\": Dep::version(\"^1.1.0\"),"));
    write(
        &app.join("nabi.nabion"),
        "Package { name: \"app\", deps: { \"num\": \">=1.0\" } }\n",
    );
    let _ = fs::remove_file(app.join("nabi.lock"));
    let out = nabi(&home, &app, &["deps"]);
    ok(&out);
    assert_eq!(
        locked_packages(&read(&app.join("nabi.lock"))),
        ["num@1.1.0"]
    );
    let err = text(&out.stderr);
    assert!(
        err.contains(&format!(
            "note: package `num` 2.0.0 needs nabi 99.0.0 or a later 99.x (this is nabi {}); to use \
             it, run `nabi +99.0.0 ..` or pin that toolchain (`toolchain: \"99.0.0\"` in \
             nabi.nabion); using 1.1.0",
            env!("CARGO_PKG_VERSION")
        )),
        "{err}"
    );
    write(
        &app.join("nabi.nabion"),
        "Package { name: \"app\", deps: { \"num\": \"^2\" } }\n",
    );
    fails(
        &nabi(&home, &app, &["deps"]),
        "help: package `num` 2.0.0 needs nabi 99.0.0 or a later 99.x",
    );
}

/// Registry packages end to end on a local index: publish (dry run and commit),
/// `add` / lock / `update`, conflicts, yank, cksum checks, search.
#[test]
fn registry_publish_add_update_yank() {
    let root = scratch("registry");
    let home = root.join("home");
    let index = root.join("index");
    commit(
        &index,
        &[("config.nabion", "Config { api: \"http://127.0.0.1:1\" }\n")],
    );
    for kv in [
        ["user.name", "t"],
        ["user.email", "t@t"],
        ["commit.gpgsign", "false"],
    ] {
        git(&index, &["config", kv[0], kv[1]]);
    }
    let reg = index.display().to_string();
    let qreg = q(&reg);
    mirror(&home, &reg);
    let num = root.join("num");
    let tree1 = publish_packages(&root, &home, &index);
    let index_log = || git(&index, &["log", "--format=%s"]);

    // `add`: `^` the newest version; the lock pins both packages.
    let app = root.join("app");
    write(
        &app.join("src/main.nabi"),
        "use geom::two;\nprintln(f\"{two()}\");\n",
    );
    write(&app.join("nabi.nabion"), APP0);
    ok(&nabi(&home, &app, &["add", "geom"]));
    assert!(read(&app.join("nabi.nabion")).contains("\"geom\": Dep::version(\"^0.3.0\"),"));
    let lock = read(&app.join("nabi.lock"));
    assert_eq!(locked_packages(&lock), ["geom@0.3.0", "num@1.1.0"]);
    // The lock names the index, not the mirror it came from.
    assert!(
        lock.contains(&format!("source: Dep::registry({})", q(DEFAULT))) && !lock.contains(&qreg),
        "{lock}"
    );
    let names: Vec<String> = deps(&home, &app, &[]).into_iter().map(|(n, _)| n).collect();
    assert_eq!(names, ["num", "geom"]);
    assert_eq!(ok(&nabi(&home, &app, &["-q", "run"])), "2\n");

    // A root requirement the chosen version does not meet: a conflict
    // naming both, and nabi.nabion as it was.
    let before = read(&app.join("nabi.nabion"));
    let out = nabi(&home, &app, &["add", "num@2"]);
    fails(
        &out,
        "no version of package `num` meets every requirement on it",
    );
    for want in [
        "`2` (required by the root package)",
        "`^1.0` (required by `geom` 0.3.0)",
        "help: num 1.1.0 meets the other requirements: change the root package's to it, \
         `\"num\": \"^1.1.0\"`",
    ] {
        assert!(text(&out.stderr).contains(want), "{}", text(&out.stderr));
    }
    assert_eq!(read(&app.join("nabi.nabion")), before);
    ok(&nabi(&home, &app, &["add", "num@~1.1"]));
    assert!(read(&app.join("nabi.nabion")).contains("\"num\": Dep::version(\"~1.1\"),"));
    fails(
        &nabi(&home, &app, &["add", "num@^9"]),
        "no version of package `num` matches `^9`",
    );
    fails(
        &nabi(&home, &app, &["add", "nope"]),
        "no package named `nope`",
    );
    fails(
        &nabi(&home, &app, &["add", "nope"]),
        "help: search the index: `nabi search nope`",
    );
    // A near name: the package it likely is.
    fails(
        &nabi(&home, &app, &["add", "gemo"]),
        "help: did you mean `geom`? (or search: `nabi search gemo`)",
    );
    fails(
        &nabi(&home, &app, &["add", "num@x.y"]),
        "is not a version requirement",
    );

    // A new version: builds keep the locked one until `update`.
    package(&num, "num", "1.1.5", "", &[("src/lib.nabi", ONE)]);
    ok(&nabi(&home, &num, &["publish", "--index-dir", &reg]));
    ok(&nabi(&home, &app, &["--locked", "deps"]));
    assert!(locked_packages(&read(&app.join("nabi.lock"))).contains(&"num@1.1.0".to_owned()));
    let out = nabi(&home, &app, &["update", "num"]);
    ok(&out);
    assert!(
        text(&out.stderr).contains("Updated num v1.1.0 -> v1.1.5\n"),
        "{}",
        text(&out.stderr)
    );
    assert!(locked_packages(&read(&app.join("nabi.lock"))).contains(&"num@1.1.5".to_owned()));
    ok(&nabi(&home, &app, &["--offline", "deps"]));

    // Yanked: kept while locked, skipped by `update`; `--undo` restores.
    ok(&nabi(
        &home,
        &num,
        &["yank", "num@1.1.5", "--index-dir", &reg],
    ));
    assert!(read(&index.join("3/n/num")).contains("version: \"1.1.5\""));
    assert!(index_log().starts_with("Yank num 1.1.5\n"));
    ok(&nabi(&home, &app, &["deps"]));
    assert!(locked_packages(&read(&app.join("nabi.lock"))).contains(&"num@1.1.5".to_owned()));
    ok(&nabi(&home, &app, &["update"]));
    assert!(locked_packages(&read(&app.join("nabi.lock"))).contains(&"num@1.1.0".to_owned()));
    let out = nabi(&home, &app, &["add", "num@=1.1.5"]);
    fails(&out, "1.1.5 (yanked)");
    ok(&nabi(
        &home,
        &num,
        &["yank", "--undo", "num@1.1.5", "--index-dir", &reg],
    ));
    ok(&nabi(&home, &app, &["add", "num@=1.1.5"]));
    fails(
        &nabi(&home, &num, &["yank", "num@9.9.9", "--index-dir", &reg]),
        "not in the index",
    );
    fails(
        &nabi(&home, &num, &["yank", "num", "--index-dir", &reg]),
        "`num`: give the version as `<name>@<version>`, e.g. `nabi yank num@1.2.0`",
    );

    // An entry whose tree is not its commit's is refused.
    let entry = read(&index.join("3/n/num"))
        .lines()
        .next()
        .unwrap()
        .replace("1.0.0", "1.0.1");
    let forged = format!("{}\n", entry.replace(&tree1, &"0".repeat(40)));
    write(
        &index.join("3/n/num"),
        &(read(&index.join("3/n/num")) + &forged),
    );
    git(&index, &["commit", "-qam", "forged"]);
    fails(
        &nabi(&home, &app, &["add", "num@=1.0.1"]),
        "but the index says",
    );

    // `search`, with the index from a mirror in a fresh NABI_HOME (the
    // API, which has the descriptions, does not answer).
    let home2 = root.join("home2");
    mirror(&home2, &reg);
    assert_eq!(ok(&nabi(&home2, &root, &["search", "o"])), "geom 0.3.0\n");
    assert_eq!(ok(&nabi(&home, &root, &["search", "NU"])), "num 2.0.0\n");
    // Either spelling of `-` / `_` finds a package.
    let big = root.join("big-int");
    package(&big, "big-int", "0.1.0", "", &[("src/lib.nabi", ONE)]);
    ok(&nabi(&home, &big, &["publish", "--index-dir", &reg]));
    for q in ["big_int", "G-I", "big-int"] {
        assert_eq!(ok(&nabi(&home, &root, &["search", q])), "big-int 0.1.0\n");
    }

    // A per-dependency index is an index of its own, even when it is the
    // default one's mirror: its packages come from it.
    let app2 = root.join("app2");
    write(&app2.join("src/main.nabi"), "println(1);\n");
    write(&app2.join("nabi.nabion"), APP0);
    ok(&nabi(
        &home,
        &app2,
        &["add", "geom@0.3", "--registry", &reg],
    ));
    assert!(read(&app2.join("nabi.nabion")).contains(&format!(
        "\"geom\": Dep::version(\"0.3\", registry: {qreg}),"
    )));
    let lock = read(&app2.join("nabi.lock"));
    assert!(lock.contains(&qreg) && !lock.contains(DEFAULT), "{lock}");
}

/// Registry sources from the registry's archives: checked against the
/// index's `archive`, used without the git repository, and the git
/// source when the registry keeps none.
#[cfg(unix)]
#[test]
fn registry_archives() {
    let root = scratch("registry-archive");
    let home = root.join("home");
    let index = root.join("index");
    let served = root.join("served");
    let port = serve_dir(served.clone());
    commit(
        &index,
        &[(
            "config.nabion",
            &format!("Config {{ api: \"http://127.0.0.1:{port}\" }}\n"),
        )],
    );
    for kv in [
        ["user.name", "t"],
        ["user.email", "t@t"],
        ["commit.gpgsign", "false"],
    ] {
        git(&index, &["config", kv[0], kv[1]]);
    }
    let reg = index.display().to_string();
    mirror(&home, &reg);
    let num = root.join("num");
    let mut lines = Vec::new();
    for v in ["1.0.0", "1.1.0", "1.2.0"] {
        let c = package(&num, "num", v, "", &[("src/lib.nabi", ONE)]);
        ok(&nabi(&home, &num, &["publish", "--index-dir", &reg]));
        // What the registry keeps at publish: `git archive` of the commit.
        let dl = served.join(format!("api/v1/packages/num/{v}/download"));
        fs::create_dir_all(dl.parent().unwrap()).unwrap();
        let prefix = format!("--prefix=num-{v}/");
        git(
            &num,
            &[
                "archive",
                "--format=tar.gz",
                &prefix,
                "-o",
                dl.to_str().unwrap(),
                &c,
            ],
        );
        lines.push(sha256_of(&dl));
    }
    // 1.0.0: right sum; 1.1.0: none kept (no `archive`); 1.2.0: a wrong sum.
    let file: String = read(&index.join("3/n/num"))
        .lines()
        .map(|l| {
            let sum = if l.contains("\"1.0.0\"") {
                Some(lines[0].clone())
            } else if l.contains("\"1.2.0\"") {
                Some("0".repeat(64))
            } else {
                None
            };
            match sum {
                Some(s) => l.replace(", yanked:", &format!(", archive: {}, yanked:", q(s))),
                None => l.to_owned(),
            }
        })
        .map(|l| l + "\n")
        .collect();
    commit(&index, &[("3/n/num", &file)]);
    // The repository is gone: 1.0.0 still comes, from its archive.
    let away = root.join("num-away");
    fs::rename(&num, &away).unwrap();
    let app = root.join("app");
    write(&app.join("src/main.nabi"), "println(f\"{num::one()}\");\n");
    write(
        &app.join("nabi.nabion"),
        "Package { name: \"app\", deps: { \"num\": \"=1.0.0\" } }\n",
    );
    let out = nabi(&home, &app, &["run"]);
    assert_eq!(ok(&out), "1\n");
    assert!(
        text(&out.stderr).contains("Downloading num 1.0.0"),
        "{}",
        text(&out.stderr)
    );
    assert!(
        home.join("cache/registry/src")
            .join(&lines[0])
            .join("src/lib.nabi")
            .is_file()
    );
    assert!(!home.join("cache/git/db").exists());
    ok(&nabi(&home, &app, &["--offline", "-q", "run"]));
    // A download that does not match the index is refused.
    write(
        &app.join("nabi.nabion"),
        "Package { name: \"app\", deps: { \"num\": \"=1.2.0\" } }\n",
    );
    fails(&nabi(&home, &app, &["run"]), "but the index says 0000");
    // No archive: the git source (back in place).
    fs::rename(&away, &num).unwrap();
    write(
        &app.join("nabi.nabion"),
        "Package { name: \"app\", deps: { \"num\": \"=1.1.0\" } }\n",
    );
    assert_eq!(ok(&nabi(&home, &app, &["-q", "run"])), "1\n");
    assert!(home.join("cache/git/db").exists());
}

/// `GET /<name>` of the files in `dir` on `http://127.0.0.1:<port>`
/// (the port); 404 for others.
#[cfg(unix)]
pub(crate) fn serve_dir(dir: PathBuf) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let mut conn = conn.unwrap();
            let mut reader = BufReader::new(conn.try_clone().unwrap());
            let mut start = String::new();
            reader.read_line(&mut start).unwrap();
            loop {
                let mut header = String::new();
                reader.read_line(&mut header).unwrap();
                if header.trim_end().is_empty() {
                    break;
                }
            }
            let name = start
                .split(' ')
                .nth(1)
                .unwrap_or("/")
                .trim_start_matches('/');
            let (code, body) = fs::read(dir.join(name)).map_or((404, Vec::new()), |b| (200, b));
            let _ = write!(
                conn,
                "HTTP/1.1 {code} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = conn.write_all(&body);
        }
    });
    port
}

#[cfg(unix)]
pub(crate) fn sha256_of(path: &Path) -> String {
    nabic_core::hash::hex(&nabic_core::hash::sha256(&fs::read(path).unwrap()))
}
