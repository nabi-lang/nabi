//! Features.

#[allow(clippy::wildcard_imports)]
use crate::*;

/// `nabi deps` as `name[features]` per package, in build order.
pub(crate) fn enabled(home: &Path, cwd: &Path, args: &[&str]) -> String {
    let mut all = vec!["deps"];
    all.extend(args);
    let out = ok(&nabi(home, cwd, &all));
    let mut parts: Vec<(String, Vec<String>)> = Vec::new();
    for l in out.lines() {
        if let Some(v) = l.strip_prefix("--extern-feature=") {
            let (n, f) = v.split_once('=').unwrap();
            let last = parts.last_mut().unwrap();
            assert_eq!(last.0, n, "{out}");
            last.1.push(f.to_owned());
        } else if let Some((n, _)) = l.split_once('=') {
            parts.push((n.to_owned(), Vec::new()));
        }
    }
    let shown: Vec<String> = parts
        .iter()
        .map(|(n, f)| format!("{n}[{}]", f.join(",")))
        .collect();
    shown.join(" ")
}

pub(crate) const TLS: &str = "#[cfg(feature = \"fast\")]\npub fn speed() -> i32 { return 2; }\n\
                   #[cfg(not(feature = \"fast\"))]\npub fn speed() -> i32 { return 1; }\n";

pub(crate) const HTTP: &str = "#[cfg(feature = \"tls\")]\npub fn port() -> i32 { return 443; }\n\
                    #[cfg(not(feature = \"tls\"))]\npub fn port() -> i32 { return 80; }\n";

pub(crate) const APP_FEATURES: &str = "Package {
    name: \"app\",
    deps: {
        \"http\": Dep::path(\"../http\"),
        \"cli\": Dep::path(\"../cli\"),
        \"extra\": Dep::path(\"../extra\", optional: true),
    },
    dev_deps: { \"tls\": Dep::path(\"../tls\", features: [\"fast\"]) },
    features: {
        \"default\": [\"web\"],
        \"web\": [\"http/tls\"],
        \"full\": [\"web\", \"http/fast\"],
        \"opt\": [\"dep:extra\"],
    },
}
";

/// Features: the root's default and `--features` / `--all-features` /
/// `--no-default-features`, optional dependencies enabled by `dep:` and
/// `<dep>/<f>`, `<dep>?/<f>`, one set per package for the whole graph,
/// dev dependencies' only in a test build, errors, `cfg(feature)` in the
/// code, and rebuilds when a package's features change.
#[test]
fn features_pick_packages_and_cfg() {
    let root = scratch("features");
    let home = root.join("home");
    let pkg = |name: &str, manifest: &str, lib: &str| {
        write(&root.join(name).join("nabi.nabion"), manifest);
        write(&root.join(name).join("src/lib.nabi"), lib);
    };
    pkg(
        "tls",
        "Package { name: \"tls\", features: { \"default\": [\"ring\"], \"ring\": [], \"fast\": [] } }\n",
        TLS,
    );
    pkg(
        "json",
        "Package { name: \"json\", features: { \"pretty\": [], \"compact\": [] } }\n",
        ONE,
    );
    pkg(
        "http",
        "Package {\n    name: \"http\",\n    deps: {\n        \"tls\": Dep::path(\"../tls\", \
         optional: true),\n        \"json\": Dep::path(\"../json\"),\n    },\n    features: { \
         \"tls\": [\"dep:tls\"], \"fast\": [\"tls?/fast\"], \"pretty\": [\"json/pretty\"] },\n}\n",
        HTTP,
    );
    let cli = "Package {\n    name: \"cli\",\n    deps: {\n        \"http\": \
               Dep::path(\"../http\", features: [\"pretty\"]),\n        \"json\": \
               Dep::path(\"../json\", features: [\"compact\"]),\n    },\n}\n";
    pkg("cli", cli, ONE);
    pkg("extra", "Package { name: \"extra\" }\n", ONE);
    let app = root.join("app");
    write(&app.join("nabi.nabion"), APP_FEATURES);
    write(
        &app.join("src/main.nabi"),
        "let web = cfg!(feature = \"web\");\nprintln(f\"{http::port()} {web}\");\n",
    );
    let e = |args: &[&str]| enabled(&home, &app, args);
    // `default` is `web`: `http/tls` enables the optional `tls` (with its
    // own default). `json` has what `http` and `cli` ask, once.
    assert_eq!(
        e(&[]),
        "tls[default,ring] json[compact,pretty] http[pretty,tls] cli[]"
    );
    assert_eq!(
        e(&["--no-default-features"]),
        "json[compact,pretty] http[pretty] cli[]"
    );
    // `tls?/fast` waits for something else to enable `tls`.
    assert_eq!(
        e(&["--no-default-features", "--features", "http/fast"]),
        "json[compact,pretty] http[fast,pretty] cli[]"
    );
    assert_eq!(
        e(&["--features=full"]),
        "tls[default,fast,ring] json[compact,pretty] http[fast,pretty,tls] cli[]"
    );
    // `dep:extra`.
    assert_eq!(
        e(&["--no-default-features", "--features", "opt,http/tls"]),
        "tls[default,ring] json[compact,pretty] http[pretty,tls] cli[] extra[]"
    );
    assert_eq!(
        e(&["--all-features"]),
        "tls[default,fast,ring] json[compact,pretty] http[fast,pretty,tls] cli[] extra[]"
    );
    // A dev dependency's features count in a test build only.
    assert_eq!(
        e(&["--dev", "--no-default-features"]),
        "tls[default,fast,ring] json[compact,pretty] http[pretty] cli[]"
    );
    fails(
        &nabi(&home, &app, &["deps", "--features", "nope"]),
        "the root package has no feature `nope` (required by --features)",
    );
    fails(
        &nabi(&home, &app, &["deps", "--features", "http/nope"]),
        "package `http` has no feature `nope` (required by --features)",
    );
    fails(
        &nabi(&home, &app, &["deps", "--features", "zip/x"]),
        "the root package has no dependency `zip`",
    );
    write(
        &root.join("cli/nabi.nabion"),
        &cli.replace("[\"compact\"]", "[\"bogus\"]"),
    );
    fails(
        &nabi(&home, &app, &["deps"]),
        "package `json` has no feature `bogus` (required by package `cli`)",
    );
    write(&root.join("cli/nabi.nabion"), cli);
    write(
        &app.join("nabi.nabion"),
        &APP_FEATURES
            .replace("\"web\", \"http/fast\"", "\"web\", \"loop\"")
            .replace("\"opt\": [", "\"loop\": [\"full\"],\n        \"opt\": ["),
    );
    let out = nabi(&home, &app, &["deps"]);
    fails(&out, "feature cycle: full -> loop -> full");
    assert!(
        text(&out.stderr).contains("nabi.nabion:13:"),
        "{}",
        text(&out.stderr)
    );
    write(&app.join("nabi.nabion"), APP_FEATURES);

    // `nabi add --optional`: the dependency and a feature enabling it.
    ok(&nabi(
        &home,
        &app,
        &[
            "add",
            "extra2",
            "--path",
            "../extra",
            "--optional",
            "--features",
            "a,b",
            "--no-default-features",
        ],
    ));
    let m = read(&app.join("nabi.nabion"));
    assert!(
        m.contains(
            "\"extra2\": Dep::path(\n            \"../extra\",\n            optional: true,\n            \
             features: [\"a\", \"b\"],\n            default_features: false,\n        ),"
        ) && m.contains("\"extra2\": [\"dep:extra2\"],"),
        "{m}"
    );
    write(&app.join("nabi.nabion"), APP_FEATURES);

    // The code sees its package's features; a change rebuilds, the same
    // features again do not.
    let run = |args: &[&str]| {
        let out = nabi(&home, &app, &[&["run"], args].concat());
        (ok(&out), text(&out.stderr))
    };
    let (out, err) = run(&[]);
    assert_eq!(out, "443 true\n", "{err}");
    let (out, err) = run(&["--no-default-features"]);
    assert_eq!(out, "80 false\n");
    assert!(err.contains("Compiling http "), "{err}");
    let (out, err) = run(&["--no-default-features"]);
    assert_eq!(out, "80 false\n");
    assert!(!err.contains("Compiling"), "{err}");
}

/// Features in a registry index: `publish` writes them, resolution reads
/// them; `nabi add --features --no-default-features` writes the
/// dependency so; the lock holds optional dependencies whatever is enabled.
#[test]
fn index_features_round_trip() {
    let root = scratch("index-features");
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
    let num = root.join("num");
    package_meta(
        &num,
        "num",
        "1.0.0",
        "",
        "features: { \"big\": [] }, ",
        &[("src/lib.nabi", ONE)],
    );
    ok(&nabi(&home, &num, &["publish", "--index-dir", &reg]));
    let codec = root.join("codec");
    package_meta(
        &codec,
        "codec",
        "1.0.0",
        "\"num\": Dep::version(\"^1.0\", optional: true, features: [\"big\"], default_features: \
         false)",
        "features: { \"default\": [\"std\"], \"std\": [], \"num\": [\"dep:num\"] }, ",
        &[("src/lib.nabi", ONE)],
    );
    let line = ok(&nabi(&home, &codec, &["publish", "--index-dir", &reg]));
    assert!(
        line.contains(
            "deps: [{ name: \"num\", req: \"^1.0\", kind: \"normal\", optional: true, features: \
             [\"big\"], default_features: false }], features: { \"default\": [\"std\"], \"std\": \
             [], \"num\": [\"dep:num\"] }, source: Dep::git("
        ),
        "{line}"
    );
    assert_eq!(read(&index.join("co/de/codec")), line);

    let app = root.join("app");
    write(&app.join("src/main.nabi"), "println(1);\n");
    write(&app.join("nabi.nabion"), APP0);
    ok(&nabi(
        &home,
        &app,
        &["add", "codec", "--features", "num", "--no-default-features"],
    ));
    let m = read(&app.join("nabi.nabion"));
    assert!(
        m.contains(
            "\"codec\": Dep::version(\"^1.0.0\", features: [\"num\"], default_features: false),"
        ),
        "{m}"
    );
    assert_eq!(enabled(&home, &app, &[]), "num[big] codec[num]");
    let lock = read(&app.join("nabi.lock"));
    assert_eq!(locked_packages(&lock), ["codec@1.0.0", "num@1.0.0"]);
    write(
        &app.join("nabi.nabion"),
        &APP0.replace("// deps go here", "\"codec\": \"^1.0\","),
    );
    assert_eq!(enabled(&home, &app, &[]), "codec[default,std]");
    // `num` stays pinned: the lock covers every optional dependency.
    assert_eq!(read(&app.join("nabi.lock")), lock);

    // Index features are checked against the package's nabi.nabion at its
    // commit, in any order.
    let entry = read(&index.join("3/n/num"));
    let forged = entry.replace(
        "features: { \"big\": [] }",
        "features: { \"big\": [], \"fast\": [] }",
    );
    assert_ne!(forged, entry);
    commit(&index, &[("3/n/num", &forged)]);
    let home2 = root.join("home2");
    mirror(&home2, &reg);
    let app2 = root.join("app2");
    write(&app2.join("src/main.nabi"), "println(1);\n");
    write(
        &app2.join("nabi.nabion"),
        &APP0.replace("// deps go here", "\"num\": \"^1.0\","),
    );
    fails(
        &nabi(&home2, &app2, &["deps"]),
        "package `num` 1.0.0: the index's features differ from its nabi.nabion's at commit",
    );
}
