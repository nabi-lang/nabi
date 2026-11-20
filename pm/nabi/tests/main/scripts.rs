//! Scripts: running, caching and their nabion blocks.

#[allow(clippy::wildcard_imports)]
use crate::*;

#[test]
fn script_dependencies() {
    let root = scratch("script");
    let home = root.join("home");
    write(&root.join("lib1/src/lib.nabi"), LIB_V1);
    let script = "//! ```nabion\n//! Package {\n//!     deps: { \"greet\": Dep::path(\"lib1\") },\n//! }\n//! ```\n\
                  use greet::greet;\nprintln(f\"{greet()}\");\n";
    write(&root.join("s.nabi"), script);
    ok(&nabi(&home, &root, &["-q", "check", "s.nabi"]));
    // Its lock lives in the cache, not next to the script.
    assert!(!root.join("nabi.lock").exists());
    assert!(home.join("cache/scripts").is_dir());
    write(
        &root.join("plain.nabi"),
        "use greet::greet;\nprintln(f\"{greet()}\");\n",
    );
    assert_eq!(
        nabi(&home, &root, &["-q", "check", "plain.nabi"])
            .status
            .code(),
        Some(1)
    );
    write(
        &root.join("bad.nabi"),
        "//! ```nabion\n//! Package {\n//!     deps: { \"g\": 1 },\n//! }\n//! ```\n",
    );
    fails(
        &nabi(&home, &root, &["check", "bad.nabi"]),
        "bad.nabi (//! nabion block):2:18",
    );

    let out = nabi(&home, &root, &["-q", "run", "s.nabi"]);
    assert_eq!(ok(&out), "1\n");
    // A dependency's change is a new build of the script.
    write(&root.join("lib1/src/lib.nabi"), LIB_V2);
    let out = nabi(&home, &root, &["-q", "run", "s.nabi"]);
    assert_eq!(ok(&out), "2\n");
}

pub(crate) const ECHO: &str = "#!/usr/bin/env nabi\nuse std::env;\n\
                    use std::process;\n\
                    let line = (try! io::read_line()) ?? \"\";\n\
                    println(f\"{env::args()[1..]:?} {line.trim()}\");\n\
                    eprintln(\"err\");\nprocess::exit(3);\n";

/// The cached executable `nabi run -v` ran, and whether it compiled.
pub(crate) fn ran(out: &Output) -> (String, bool) {
    let err = text(&out.stderr);
    let exe = err
        .lines()
        .find_map(|l| l.trim().strip_prefix("Running `"))
        .unwrap_or_else(|| panic!("no Running line: {err}"));
    (
        exe.trim_end_matches('`').to_owned(),
        err.contains("Compiling"),
    )
}

#[test]
fn scripts_run_directly() {
    let root = scratch("direct");
    let home = root.join("home");
    write(&root.join("echo.nabi"), ECHO);
    // `nabi <file> args..`: every argument after the file is the
    // program's, stdin / stdout / stderr are its own, and so is the
    // exit code.
    for (file, args) in [("echo.nabi", ["-q", "--x"]), ("./echo.nabi", ["--", "a b"])] {
        let mut child = nabi_cmd(&home, &root, &[file, args[0], args[1]])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(b"in\n").unwrap();
        let out = child.wait_with_output().unwrap();
        assert_eq!(out.status.code(), Some(3), "{}", text(&out.stderr));
        assert_eq!(
            text(&out.stdout),
            format!("[{:?}, {:?}] in\n", args[0], args[1])
        );
        assert_eq!(text(&out.stderr), "err\n");
    }
    // A file named like a command is a command unless it is a path.
    write(&root.join("version"), "println(\"no\");\n");
    assert!(ok(&nabi(&home, &root, &["version"])).starts_with("nabi "));
    // Compile errors are reported as by `nabi run`.
    write(&root.join("bad.nabi"), "let x: i32 = \"s\";\n");
    fails(&nabi(&home, &root, &["bad.nabi"]), "error[E");
    // Through the shebang line.
    #[cfg(unix)]
    {
        let file = root.join("echo.nabi");
        fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).unwrap();
        let bin = Path::new(NABI).parent().unwrap();
        let path = std::env::join_paths(std::iter::once(bin.to_path_buf()).chain(
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
        ))
        .unwrap();
        let out = Command::new(&file)
            .arg("z")
            .current_dir(&root)
            .env("PATH", path)
            .env("NABI_HOME", &home)
            .env_remove("NABI_ENV")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(3), "{}", text(&out.stderr));
        assert_eq!(text(&out.stdout), "[\"z\"] \n");
    }
}

// A script's arguments may be any bytes; nabi's own must be UTF-8.
#[cfg(unix)]
#[test]
fn scripts_take_non_utf8_args() {
    let root = scratch("direct-bytes");
    let home = root.join("home");
    write(
        &root.join("n.nabi"),
        "println(f\"{std::env::args().len()}\");\n",
    );
    let bad = std::ffi::OsStr::from_bytes(b"b\xffc");
    for args in [&["./n.nabi", "a"][..], &["run", "n.nabi", "--", "a"]] {
        let out = nabi_cmd(&home, &root, args)
            .arg(bad)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert_eq!(ok(&out), "3\n", "{args:?}");
    }
    let out = nabi_cmd(&home, &root, &["--cwd"])
        .arg(bad)
        .args(["run", "n.nabi"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    fails(
        &out,
        "nabi: argument 2 (`b\u{fffd}c`) is not valid UTF-8; only a program's arguments, after \
         `--`, may be other bytes",
    );
}

#[test]
fn script_executables_are_cached_by_content() {
    let root = scratch("script-cache");
    let home = root.join("home");
    let file = root.join("t.nabi");
    write(&file, "println(\"one\");\n");
    let run = |args: &[&str]| {
        let mut all = vec!["run", "-v", "t.nabi"];
        all.splice(1..1, args.iter().copied());
        let out = nabi(&home, &root, &all);
        ok(&out);
        (text(&out.stdout), ran(&out))
    };
    let (out, (one, compiled)) = run(&[]);
    assert_eq!((out.as_str(), compiled), ("one\n", true));
    let entry = Path::new(&one).parent().unwrap().to_path_buf();
    // A script's own directory, in the cache (`target/<profile>/scripts/`).
    let scripts = entry.parent().unwrap();
    assert!(scripts.starts_with(home.join("cache/scripts")), "{one}");
    assert!(scripts.ends_with("target/debug/scripts"), "{one}");
    // The second run compiles nothing.
    assert_eq!(run(&[]), ("one\n".to_owned(), (one.clone(), false)));
    // An edit is a new key; the old entry stays and serves the old text.
    write(&file, "println(\"two\");\n");
    let (out, (two, compiled)) = run(&[]);
    assert_eq!((out.as_str(), compiled), ("two\n", true));
    assert_ne!(one, two);
    write(&file, "println(\"one\");\n");
    assert_eq!(run(&[]), ("one\n".to_owned(), (one, false)));
    // Options before the file, and their own key.
    let (out, (rel, compiled)) = run(&["--release"]);
    assert_eq!((out.as_str(), compiled), ("one\n", true));
    assert!(rel.contains("release"), "{rel}");
    // Unused for a day, an entry goes when another is built.
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 24 * 3600);
    fs::File::options()
        .write(true)
        .open(entry.join("used"))
        .unwrap()
        .set_modified(old)
        .unwrap();
    write(&file, "println(\"three\");\n");
    assert_eq!(run(&[]).0, "three\n");
    assert!(!entry.exists());
}

#[test]
fn concurrent_script_runs_share_one_entry() {
    let root = scratch("script-race");
    let home = root.join("home");
    write(&root.join("t.nabi"), "println(\"ok\");\n");
    // The script's cache directory first: the runs race on the script.
    ok(&nabi(&home, &root, &["-q", "check", "t.nabi"]));
    let runs: Vec<_> = (0..6)
        .map(|_| {
            nabi_cmd(&home, &root, &["t.nabi"])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    for r in runs {
        let out = r.wait_with_output().unwrap();
        assert_eq!(ok(&out), "ok\n", "{}", text(&out.stderr));
    }
    let cache: Vec<_> = fs::read_dir(home.join("cache/scripts"))
        .unwrap()
        .flatten()
        .collect();
    assert_eq!(cache.len(), 1);
    let scripts = cache[0].path().join("target/debug/scripts");
    let entries: Vec<_> = fs::read_dir(&scripts).unwrap().flatten().collect();
    assert_eq!(entries.len(), 1);
    let left: Vec<String> = fs::read_dir(entries[0].path())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !n.ends_with(".dSYM"))
        .collect();
    let mut left = left;
    left.sort();
    // Windows: the PDB of the scratch name the link wrote, which the
    // executable names.
    let mut want = vec![format!(".{}.fp", exe("t")), exe("t"), "used".to_owned()];
    if cfg!(windows) {
        want.push(format!(".{}.pdb", exe("t")));
    }
    want.sort();
    assert_eq!(left, want);
}

/// `//! nabion` block lines for `Package { deps: { <deps> } }`, then `body`.
pub(crate) fn block(deps: &str, body: &str) -> String {
    format!("//! ```nabion\n//! Package {{ deps: {{ {deps} }} }}\n//! ```\n{body}")
}

/// A script uses only the `//! nabion` blocks of the files it reaches (its
/// own and those its `mod`s load, each path relative to its file),
/// merged; never the environment's libraries. `nabi init --script` makes
/// it a package.
#[test]
fn scripts_use_their_blocks() {
    let root = scratch("script-blocks");
    let home = root.join("home");
    for (dir, lib) in [("greet", LIB_V1), ("greet2", LIB_V2)] {
        write(
            &root.join(dir).join("nabi.nabion"),
            "Package { name: \"greet\" }\n",
        );
        write(&root.join(dir).join("src/lib.nabi"), lib);
    }
    write(
        &root.join("util/src/lib.nabi"),
        "pub fn ten() -> i32 { return 10; }\n",
    );
    // The environment's `greet` is not the script's.
    ok(&nabi(&home, &root, &["install", "--path", "greet"]));
    let uses = "use greet::greet;\nprintln(f\"{greet()}\");\n";
    write(
        &root.join("s.nabi"),
        &block("\"greet\": Dep::path(\"greet2\")", uses),
    );
    assert_eq!(script_out(&home, &root, &["s.nabi"]), "2\n");
    assert_eq!(
        script_out(&home, &root, &["--env", "default", "s.nabi"]),
        "2\n"
    );

    // `mod helper;` loads app/helper.nabi; its `mod deep;` app/helper/deep.nabi,
    // whose block's path is relative to it.
    let app = root.join("app");
    write(
        &app.join("hi.nabi"),
        &block(
            "\"greet\": Dep::path(\"../greet\")",
            "mod helper;\nuse greet::greet;\nprintln(f\"{greet() + helper::ten()}\");\n",
        ),
    );
    let helper = "mod deep;\npub fn ten() -> i32 { return deep::ten(); }\n";
    write(&app.join("helper.nabi"), helper);
    let deep = |n: i32| {
        block(
            "\"util\": Dep::path(\"../../util\")",
            &format!("use util::ten as t;\npub fn ten() -> i32 {{ return t() * {n}; }}\n"),
        )
    };
    write(&app.join("helper/deep.nabi"), &deep(1));
    assert_eq!(script_out(&home, &root, &["app/hi.nabi"]), "11\n");
    let names = |file: &str| -> Vec<String> {
        ok(&nabi(&home, &root, &["deps", file]))
            .lines()
            .filter_map(|l| Some(l.split_once('=')?.0.to_owned()))
            .collect()
    };
    let mut both = names("app/hi.nabi");
    both.sort();
    assert_eq!(both, ["greet", "util"]);
    // Run on its own, a file has only the blocks it reaches.
    assert_eq!(names("app/helper/deep.nabi"), ["util"]);
    // A helper's change is a new build.
    write(&app.join("helper/deep.nabi"), &deep(2));
    assert_eq!(script_out(&home, &root, &["app/hi.nabi"]), "21\n");
    // Every block is part of the cache key.
    let count = || fs::read_dir(home.join("cache/scripts")).unwrap().count();
    let before = count();
    write(
        &app.join("helper.nabi"),
        &block("\"greet\": Dep::path(\"../greet\")", helper),
    );
    assert_eq!(script_out(&home, &root, &["app/hi.nabi"]), "21\n");
    assert_eq!(count(), before + 1);
    // Two blocks, two requirements of one library.
    write(
        &app.join("helper.nabi"),
        &block("\"greet\": Dep::path(\"../greet2\")", helper),
    );
    let sep = std::path::MAIN_SEPARATOR;
    fails(
        &nabi(&home, &root, &["app/hi.nabi"]),
        &format!(
            "`greet` is required as Dep::path(\"../greet\") in app{sep}hi.nabi and as \
             Dep::path(\"../greet2\") in app{sep}helper.nabi: use one version, the same entry in \
             both blocks (or keep it in one of them)"
        ),
    );
    write(&app.join("helper.nabi"), helper);

    // To a package: nabi.nabion from every block, the files under src/
    // without theirs.
    ok(&nabi(&home, &root, &["init", "--script", "app/hi.nabi"]));
    assert_eq!(
        read(&app.join("nabi.nabion")),
        "Package {\n    name: \"hi\",\n    version: \"0.1.0\",\n    deps: { \"greet\": \
         Dep::path(\"../greet\"), \"util\": Dep::path(\"../util\") },\n}\n"
    );
    assert!(!app.join("hi.nabi").exists() && !app.join("helper").exists());
    assert_eq!(read(&app.join("src/helper.nabi")), helper);
    assert!(read(&app.join("src/main.nabi")).starts_with("mod helper;\n"));
    assert!(read(&app.join("src/helper/deep.nabi")).starts_with("use util::ten as t;\n"));
    assert_eq!(script_out(&home, &app, &["run"]), "21\n");
    fails(
        &nabi(&home, &root, &["init", "--script", "s.nabi", "x"]),
        "usage: nabi init --script <file.nabi>",
    );
}

/// `nabi add` / `remove` with a script edit its `//! nabion` block: made
/// after the shebang and doc lines when missing, removed when empty, a
/// path relative to the script. `import x` says what to write.
#[test]
fn script_add_and_remove() {
    let root = scratch("script-add");
    let home = root.join("home");
    write(&root.join("lib1/src/lib.nabi"), LIB_V1);
    write(&root.join("util/src/lib.nabi"), LIB_V2);
    fs::create_dir_all(root.join("elsewhere")).unwrap();
    let plain = "#!/usr/bin/env nabi\n//! Greets.\nuse greet::greet;\nprintln(f\"{greet()}\");\n";
    let file = root.join("s.nabi");
    write(&file, plain);
    ok(&nabi(
        &home,
        &root,
        &["add", "greet", "--path", "lib1", "s.nabi"],
    ));
    let one = "#!/usr/bin/env nabi\n//! Greets.\n//! ```nabion\n//! Package {\n//!     deps: {\n\
               //!         \"greet\": Dep::path(\"lib1\"),\n//!     },\n//! }\n//! ```\n\
               use greet::greet;\nprintln(f\"{greet()}\");\n";
    assert_eq!(read(&file), one);
    assert_eq!(script_out(&home, &root, &["s.nabi"]), "1\n");
    // Again: the same text.
    ok(&nabi(
        &home,
        &root,
        &["add", "greet", "--path", "lib1", "s.nabi"],
    ));
    assert_eq!(read(&file), one);
    // From elsewhere: the path is relative to the script.
    ok(&nabi(
        &home,
        &root.join("elsewhere"),
        &["add", "util", "--path", "../util", "../s.nabi"],
    ));
    assert!(read(&file).contains("//!         \"util\": Dep::path(\"util\"),\n"));
    // Removing both gives the file back.
    ok(&nabi(&home, &root, &["remove", "util", "s.nabi"]));
    assert_eq!(read(&file), one);
    ok(&nabi(&home, &root, &["remove", "greet", "s.nabi"]));
    assert_eq!(read(&file), plain);
    fails(
        &nabi(&home, &root, &["remove", "greet", "s.nabi"]),
        "`greet` is not a dependency in the block of s.nabi",
    );
    // A block of the user's keeps its layout and comments.
    let mine = "//! ```nabion\n//! Package { deps: { \"greet\": Dep::path(\"lib1\") } } // mine\n\
                //! ```\nprintln(1);\n";
    write(&file, mine);
    ok(&nabi(
        &home,
        &root,
        &["add", "util", "--path", "util", "s.nabi"],
    ));
    let text = read(&file);
    assert!(
        text.contains("// mine") && text.contains("\"util\": Dep::path(\"util\")"),
        "{text}"
    );
    // What does not resolve is not written; a block holds only `deps`.
    write(&file, plain);
    fails(
        &nabi(&home, &root, &["add", "x", "--path", "elsewhere", "s.nabi"]),
        "is not a library package: no src/lib.nabi",
    );
    assert_eq!(read(&file), plain);
    fails(
        &nabi(
            &home,
            &root,
            &["add", "greet", "--dev", "--path", "lib1", "s.nabi"],
        ),
        "a script's block holds only `deps`: no --dev",
    );
    fails(
        &nabi(
            &home,
            &root,
            &["add", "greet", "--path", "lib1", "nope.nabi"],
        ),
        "`nope.nabi` does not exist",
    );

    // `import x` (or `include` / `require`): `mod` for x.nabi beside the
    // file; the `use` hint for a name the file knows; else `nabi add`.
    write(&root.join("helper.nabi"), "pub fn f() {}\n");
    write(&root.join("i.nabi"), "import helper\n");
    fails(
        &nabi(&home, &root, &["check", "i.nabi"]),
        "= help: to load helper.nabi, write `mod helper;` then call `helper::f()` or `use helper::f;`",
    );
    for known in ["import std\n", "mod geo { pub fn f() {} }\ninclude geo\n"] {
        write(&root.join("i.nabi"), known);
        fails(
            &nabi(&home, &root, &["check", "i.nabi"]),
            "= help: did you mean `use`?",
        );
    }
    write(&root.join("i.nabi"), "require json\n");
    fails(
        &nabi(&home, &root, &["check", "i.nabi"]),
        "= help: to use library `json`, run `nabi add json i.nabi`, then write `use json::f;`",
    );
}
