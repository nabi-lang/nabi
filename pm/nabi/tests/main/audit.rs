//! `nabi audit`: a locked app against a stand-in registry's advisories.

#[allow(clippy::wildcard_imports)]
use crate::*;
use std::sync::atomic::{AtomicUsize, Ordering};

/// `num` 1.0.0 up to 1.2.0 (fixed in 2.0.0 too), withdrawn for `geom`,
/// and `geom` 0.3.0 listed without a fix.
const ADVISORIES: &str = r#"{"advisories":[
{"id":"NABI-2026-0003","summary":"geom panics on empty input","aliases":["CVE-2026-3"],
 "affected":[{"package":{"ecosystem":"Nabi","name":"geom"},"versions":["0.3.0"],
 "database_specific":{"severity":"LOW"}}]},
{"id":"NABI-2026-0002","summary":"withdrawn","withdrawn":"2026-11-01T00:00:00Z",
 "affected":[{"package":{"ecosystem":"Nabi","name":"geom"},
 "ranges":[{"type":"SEMVER","events":[{"introduced":"0"}]}]}]},
{"id":"NABI-2026-0001","summary":"num overflows","database_specific":{"severity":"HIGH"},
 "affected":[{"package":{"ecosystem":"Nabi","name":"NUM"},
 "ranges":[{"type":"SEMVER","events":[{"introduced":"1.0.0"},{"fixed":"1.2.0"}]}]}]}
]}"#;

/// The advisory list on `http://127.0.0.1:<port>` (the port); `hits`
/// counts its requests, other paths are 404.
fn fake_advisories(hits: Arc<AtomicUsize>) -> u16 {
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
            let path = start.split(' ').nth(1).unwrap_or("/");
            let (code, body) = if path == "/api/v1/advisories?format=json" {
                hits.fetch_add(1, Ordering::SeqCst);
                (200, ADVISORIES)
            } else {
                (404, "")
            };
            let _ = write!(
                conn,
                "HTTP/1.1 {code} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    port
}

/// The JSON records of `nabi audit --json`.
fn records(out: &str) -> Vec<Json> {
    out.lines().map(|l| Json::parse(l).unwrap()).collect()
}

#[test]
fn audit_lock() {
    let root = scratch("audit");
    let home = root.join("home");
    let hits = Arc::new(AtomicUsize::new(0));
    let api = format!("http://127.0.0.1:{}", fake_advisories(hits.clone()));
    let index = root.join("index");
    commit(
        &index,
        &[("config.nabion", &format!("Config {{ api: \"{api}\" }}\n"))],
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
    for v in ["1.0.0", "1.1.0"] {
        package(&num, "num", v, "", &[("src/lib.nabi", ONE)]);
        ok(&nabi(&home, &num, &["publish", "--index-dir", &reg]));
    }
    let geom = root.join("geom");
    package(
        &geom,
        "geom",
        "0.3.0",
        "\"num\": \"^1.0\"",
        &[(
            "src/lib.nabi",
            "use num::one;\npub fn two() -> i32 { return one() + one(); }\n",
        )],
    );
    ok(&nabi(&home, &geom, &["publish", "--index-dir", &reg]));
    let local = root.join("local");
    write(
        &local.join("nabi.nabion"),
        "Package { name: \"local\", version: \"0.1.0\" }\n",
    );
    write(&local.join("src/lib.nabi"), ONE);

    let app = root.join("app");
    write(&app.join("src/main.nabi"), "println(\"hi\");\n");
    write(
        &app.join("nabi.nabion"),
        "Package { name: \"app\", deps: { \"geom\": \"^0.3\", \"local\": Dep::path(\"../local\") } }\n",
    );
    // No nabi.lock yet: audit resolves first.
    let out = nabi(&home, &app, &["audit"]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    assert!(app.join("nabi.lock").is_file());
    let stdout = text(&out.stdout);
    assert_eq!(
        stdout,
        "geom 0.3.0 is affected by NABI-2026-0003 (severity LOW): geom panics on empty input\n  \
         help: no fixed version yet: drop it (`nabi remove geom`) or, once you judge it safe \
         here, `nabi audit --ignore NABI-2026-0003`\n\
         num 1.1.0 is affected by NABI-2026-0001 (severity HIGH): num overflows\n  \
         help: upgrade to 1.2.0 or later: `nabi update num` (if a requirement holds it back: \
         `nabi add num@^1.2.0`)\n"
    );
    let stderr = text(&out.stderr);
    for want in [
        "local not checked: a path dependency",
        "error: 2 advisories apply in 2 locked packages",
    ] {
        assert!(stderr.contains(want), "{stderr}");
    }
    assert!(!stdout.contains("withdrawn"));

    // Cached for an hour: one request so far.
    let ignored = nabi(
        &home,
        &app,
        &[
            "audit",
            "--ignore",
            "nabi-2026-0001",
            "--ignore",
            "CVE-2026-3",
        ],
    );
    assert_eq!(ok(&ignored), "");
    assert!(
        text(&ignored.stderr).contains("no known advisories"),
        "{}",
        text(&ignored.stderr)
    );
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    let all = records(&ok_or_one(&nabi(&home, &app, &["audit", "--json"])));
    let summary = all.last().unwrap();
    assert_eq!(summary.get("type").as_str(), Some("summary"));
    assert_eq!(summary.get("affected").as_u32(), Some(2));
    assert_eq!(summary.get("checked").as_u32(), Some(2));
    let num_rec = all
        .iter()
        .find(|r| r.get("name").as_str() == Some("num"))
        .unwrap();
    assert_eq!(num_rec.get("fixed").as_str(), Some("1.2.0"));
    assert_eq!(num_rec.get("severity").as_str(), Some("HIGH"));
    assert!(
        all.iter().any(|r| r.get("type").as_str() == Some("skipped")
            && r.get("name").as_str() == Some("local"))
    );

    // Fixed: nothing applies but geom's.
    package(&num, "num", "1.2.0", "", &[("src/lib.nabi", ONE)]);
    ok(&nabi(&home, &num, &["publish", "--index-dir", &reg]));
    ok(&nabi(&home, &app, &["update", "num"]));
    let out = nabi(&home, &app, &["--offline", "audit"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(!text(&out.stdout).contains("num"), "{}", text(&out.stdout));

    // Offline with nothing cached, or no registry to ask: not checked.
    let fresh = root.join("fresh");
    mirror(&fresh, &reg);
    fails(
        &nabi(&fresh, &app, &["audit", "--offline"]),
        "no advisories of https://github.com/nabi-lang/packages-index are cached, and --offline \
         is set: run `nabi audit` online once",
    );
    assert_eq!(
        nabi(&fresh, &app, &["audit", "--offline"]).status.code(),
        Some(2)
    );
    commit(
        &index,
        &[("config.nabion", "Config { api: \"http://127.0.0.1:1\" }\n")],
    );
    let out = nabi(&fresh, &app, &["audit"]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("run `nabi audit` again"),
        "{}",
        text(&out.stderr)
    );

    fails(
        &nabi(&home, &app, &["audit", "x"]),
        "unexpected argument `x`",
    );
}

/// The stdout of an audit that found something (exit 1).
#[track_caller]
fn ok_or_one(out: &Output) -> String {
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    text(&out.stdout)
}
