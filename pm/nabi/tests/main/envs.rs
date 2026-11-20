//! Named environments and what is installed in them.

#[allow(clippy::wildcard_imports)]
use crate::*;

#[test]
fn named_environments() {
    let root = scratch("envs");
    let home = root.join("home");
    let lib = root.join("lib1");
    write(&lib.join("src/lib.nabi"), LIB_V1);
    let elsewhere = root.join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();

    ok(&nabi(&home, &root, &["env", "create", "work"]));
    // `nabi --env work jupyter install`: a kernelspec that runs the
    // kernel in that environment.
    let jupyter = root.join("jupyter");
    ok(
        &nabi_cmd(&home, &root, &["--env", "work", "jupyter", "install"])
            .env("JUPYTER_DATA_DIR", &jupyter)
            .output()
            .unwrap(),
    );
    let spec = read(&jupyter.join("kernels/nabi-work/kernel.json"));
    assert!(
        spec.contains(r#""--env","work","jupyter","--connection-file","{connection_file}"],"display_name":"nabi (work)""#),
        "{spec}"
    );
    // A kernel started from a GUI still finds the tools.
    assert!(spec.contains(r#""env":{"NABI_HOME":"#) && spec.contains(r#""PATH":"#));
    for logo in ["logo-32x32.png", "logo-64x64.png"] {
        let png = fs::read(jupyter.join("kernels/nabi-work").join(logo)).unwrap();
        assert!(png.starts_with(b"\x89PNG"), "{logo}");
    }
    let svg = fs::read_to_string(jupyter.join("kernels/nabi-work/logo-svg.svg")).unwrap();
    assert!(svg.contains("<svg"), "{svg}");
    fails(
        &nabi(&home, &root, &["env", "create", "work"]),
        "already exists",
    );
    ok(&nabi(
        &home,
        &root,
        &["install", "--env", "work", "greet", "--path", "lib1"],
    ));
    // `nabi install` is the one way into an environment.
    fails(
        &nabi(
            &home,
            &root,
            &["add", "--env", "work", "x", "--path", "lib1"],
        ),
        "`nabi add` edits a package's nabi.nabion or a script's block; for the libraries the REPL \
         / Jupyter see in environment `work`, run `nabi install --env work <name>`",
    );
    fails(
        &nabi(&home, &elsewhere, &["add", "x", "--path", "../lib1"]),
        "for a script, name it: `nabi add <name> <file.nabi>`",
    );
    // In an environment, path dependencies are absolute.
    let manifest = read(&home.join("envs/work/nabi.nabion"));
    let want = q(lib.display().to_string().replace('\\', "/"));
    assert!(
        manifest.contains(&format!("\"greet\": Dep::path({want}),")),
        "{manifest}"
    );
    assert_eq!(deps(&home, &elsewhere, &["--env", "work"])[0].0, "greet");
    // Outside a project: `default` (empty), then what `env use` chose,
    // and $NABI_ENV over both.
    assert!(deps(&home, &elsewhere, &[]).is_empty());
    ok(&nabi(&home, &root, &["env", "use", "work"]));
    assert_eq!(deps(&home, &elsewhere, &[])[0].0, "greet");
    ok(&nabi(
        &home,
        &root,
        &["install", "--env", "default", "d", "--path", "lib1"],
    ));
    let out = nabi_cmd(&home, &elsewhere, &["deps"])
        .env("NABI_ENV", "default")
        .output()
        .unwrap();
    assert_eq!(ok(&out).split('=').next(), Some("d"));
    let list = ok(&nabi(&home, &root, &["env", "list"]));
    assert_eq!(list, "  default\n* work (active)\n");
    // The choice is in `.current`.
    assert_eq!(read(&home.join("envs/.current")), "work\n");

    // Export, import elsewhere.
    let export = root.join("export");
    ok(&nabi(&home, &root, &["env", "export", "work", "export"]));
    assert!(export.join("nabi.nabion").is_file());
    ok(&nabi(&home, &root, &["env", "import", "copy", "export"]));
    assert_eq!(deps(&home, &elsewhere, &["--env", "copy"])[0].0, "greet");
    // Import from a project: relative paths become absolute.
    let proj = root.join("proj");
    write(&proj.join("src/main.nabi"), "println(\"p\");\n");
    write(
        &proj.join("nabi.nabion"),
        "Package {\n    deps: {\n        \"g\": Dep::path(\"../lib1\"), // here\n    },\n}\n",
    );
    ok(&nabi(&home, &root, &["env", "import", "fromproj", "proj"]));
    assert!(
        read(&home.join("envs/fromproj/nabi.nabion"))
            .contains(&format!("\"g\": Dep::path({}), // here", q(lib.display())))
    );

    // `env shell`: $SHELL with NABI_ENV and nabi on PATH.
    let mut child = nabi_cmd(&home, &root, &["env", "shell", "work"])
        .env("SHELL", "/bin/sh")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    // Windows: %COMSPEC%, whatever SHELL says.
    let script: &[u8] = if cfg!(windows) {
        b"echo env=%NABI_ENV%\r\nwhere nabi\r\n"
    } else {
        b"echo \"env=$NABI_ENV\"; command -v nabi\n"
    };
    child.stdin.take().unwrap().write_all(script).unwrap();
    let out = child.wait_with_output().unwrap();
    let stdout = ok(&out);
    assert!(stdout.contains("env=work"), "{stdout}");
    assert!(stdout.contains(NABI), "{stdout}");

    ok(&nabi(&home, &root, &["env", "remove", "work"]));
    fails(
        &nabi(&home, &root, &["env", "remove", "work"]),
        "no environment",
    );
    fails(
        &nabi(&home, &elsewhere, &["--env", "work", "deps"]),
        "no environment `work`",
    );
    // The choice went with it: back to `default`.
    assert_eq!(deps(&home, &elsewhere, &[])[0].0, "d");
}

/// Every source of an environment name is checked: never a path, never
/// a device or the choice file, one environment per spelling whatever
/// the case.
#[test]
fn environment_names_are_checked() {
    let root = scratch("env-names");
    let home = root.join("home");
    let elsewhere = root.join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    let jupyter = root.join("jupyter");
    ok(&nabi(&home, &root, &["env", "create", "work"]));
    for (name, why) in [
        (".current", "starts with `.`"),
        ("CON", "`CON` is a device name on Windows"),
        ("nul.txt", "starts with a Windows device name (`nul`)"),
        (
            "Work",
            "environment `work` already exists, and Windows and macOS see `Work` as the same name",
        ),
        ("a/b", "contains '/'"),
    ] {
        fails(&nabi(&home, &root, &["env", "create", name]), why);
    }
    for (var, flag) in [("../../p1", false), ("../../p1", true), ("..", true)] {
        let out = if flag {
            nabi(&home, &elsewhere, &["--env", var, "deps"])
        } else {
            nabi_cmd(&home, &elsewhere, &["deps"])
                .env("NABI_ENV", var)
                .output()
                .unwrap()
        };
        fails(&out, &format!("`{var}` is a path, not an environment name"));
    }
    write(&home.join("envs/.current"), "../../p1\n");
    fails(
        &nabi(&home, &elsewhere, &["deps"]),
        "`../../p1` is a path, not an environment name",
    );
    // One made by hand still works, with a warning.
    fs::create_dir_all(home.join("envs/a b")).unwrap();
    let out = nabi(&home, &elsewhere, &["--env", "a b", "deps"]);
    ok(&out);
    assert!(
        text(&out.stderr).contains("warning: --env: `a b` is not a valid environment name"),
        "{}",
        text(&out.stderr)
    );
    if !cfg!(windows) {
        fs::create_dir_all(home.join("envs/CON")).unwrap();
        let out = nabi(&home, &elsewhere, &["--env", "CON", "deps"]);
        ok(&out);
        assert!(
            text(&out.stderr).contains("warning: --env: `CON` is a device name on Windows"),
            "{}",
            text(&out.stderr)
        );
    }
    fails(
        &nabi_cmd(&home, &root, &["--env", "nosuch", "jupyter", "install"])
            .env("JUPYTER_DATA_DIR", &jupyter)
            .output()
            .unwrap(),
        "no environment `nosuch` (create it: nabi env create nosuch)",
    );
    assert!(!jupyter.join("kernels/nabi-nosuch").exists());
}

/// `nabi <args>` in `cwd`; panics if it runs past a minute.
pub(crate) fn nabi_within(home: &Path, cwd: &Path, args: &[&str]) -> Output {
    let mut child = nabi_cmd(home, cwd, args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let start = std::time::Instant::now();
    while child.try_wait().unwrap().is_none() {
        if start.elapsed() > std::time::Duration::from_secs(60) {
            let _ = child.kill();
            panic!(
                "nabi {args:?} in {} still running after 60 s",
                cwd.display()
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    child.wait_with_output().unwrap()
}

/// Outside a package `nabi update` is an error that says what to write,
/// unless the ambient environment has libraries to update.
#[test]
fn update_outside_package() {
    let root = scratch("update-outside");
    let home = root.join("home");
    let user = home.join("user");
    let empty = root.join("empty");
    fs::create_dir_all(&user).unwrap();
    fs::create_dir_all(&empty).unwrap();
    for cwd in [&user, &empty] {
        let out = nabi_within(&home, cwd, &["update"]);
        fails(
            &out,
            "not inside a package (no src/main.nabi, src/lib.nabi or nabi.nabion here or above): \
             make one with `nabi init`; for an environment's libraries: `nabi update --env <name>`",
        );
        assert_eq!(out.status.code(), Some(1));
    }
    // An environment with nothing in it.
    let out = nabi_within(&home, &empty, &["--env", "default", "update"]);
    ok(&out);
    assert_eq!(
        text(&out.stderr),
        "        note: no dependencies in environment `default` to update; add one: \
         `nabi install <name>`\n"
    );
    // The ambient environment with a library is updated.
    let lib = root.join("lib1");
    write(&lib.join("src/lib.nabi"), "pub fn one() -> i32 { 1 }\n");
    write(
        &home.join("envs/default/nabi.nabion"),
        &format!(
            "Package {{\n    deps: {{ \"lib1\": Dep::path({}) }},\n}}\n",
            q(lib.display())
        ),
    );
    let out = nabi_within(&home, &empty, &["update"]);
    ok(&out);
    assert_eq!(
        text(&out.stderr),
        "        note: all dependencies in environment `default` are up to date\n"
    );
}

/// `nabi update` in a package says when there is nothing to do.
#[test]
fn update_nothing_to_do() {
    let root = scratch("update-nothing");
    let home = root.join("home");
    ok(&nabi(&home, &root, &["new", "app"]));
    let out = nabi_within(&home, &root.join("app/src"), &["update"]);
    ok(&out);
    assert_eq!(
        text(&out.stderr),
        "        note: no dependencies to update; add one: `nabi add <name>`\n"
    );
    assert_eq!(
        text(&nabi(&home, &root.join("app"), &["-q", "update"]).stderr),
        ""
    );
}

#[test]
fn env_list_empty() {
    let root = scratch("env-list-empty");
    let home = root.join("home");
    let out = nabi_within(&home, &root, &["env", "list"]);
    assert_eq!(ok(&out), "");
    assert_eq!(
        text(&out.stderr),
        "        note: no environments; make one with `nabi env create <name>`\n"
    );
    ok(&nabi(&home, &root, &["env", "create", "ml"]));
    assert_eq!(ok(&nabi(&home, &root, &["env", "list"])), "  ml\n");
}

/// `nabi install`: a library goes into the active environment, for the
/// REPL; neither scripts nor packages see it. An executable package or a
/// script file is built into `$NABI_HOME/bin`; `uninstall` undoes either.
#[test]
fn install_libraries_and_executables() {
    let root = scratch("install");
    let home = root.join("home");
    let greet = root.join("greet");
    write(&greet.join("nabi.nabion"), "Package { name: \"greet\" }\n");
    write(&greet.join("src/lib.nabi"), LIB_V1);
    let script = "use greet::greet;\nprintln(f\"{greet()}\");\n";
    write(&root.join("s.nabi"), script);

    // A library: the package's own name, absolute in the environment,
    // prebuilt into its target/.
    ok(&nabi(&home, &root, &["install", "--path", "greet"]));
    let manifest = read(&home.join("envs/default/nabi.nabion"));
    let src = q(greet.display().to_string().replace('\\', "/"));
    assert!(
        manifest.contains(&format!("\"greet\": Dep::path({src})")),
        "{manifest}"
    );
    assert!(
        home.join("envs/default/target/debug/deps/libgreet.a")
            .is_file()
    );
    // The REPL's dependencies (`nabi deps` outside a package).
    assert_eq!(deps(&home, &root, &[])[0].0, "greet");
    // A script does not see it: the help is the command that adds it,
    // and a note says why the installed one does not count.
    let out = nabi(&home, &root, &["-q", "s.nabi"]);
    fails(&out, "= help: run `nabi add greet s.nabi`");
    fails(
        &out,
        "= note: `greet` is installed in environment `default`, but a script uses only the \
         libraries of its own `//! nabion` block (installed libraries are for `nabi repl` / `nabi \
         jupyter`)",
    );
    // `import greet`: the library's `nabi add`.
    write(&root.join("i.nabi"), "import greet\n");
    fails(
        &nabi(&home, &root, &["check", "i.nabi"]),
        "= help: to use library `greet`, run `nabi add greet i.nabi`, then write `use greet::f;`",
    );

    // A library that does not build is not installed.
    write(
        &root.join("broken/src/lib.nabi"),
        "pub fn f() -> i32 { return \"s\"; }\n",
    );
    fails(
        &nabi(&home, &root, &["install", "--path", "broken"]),
        "`broken` did not build in environment `default`; nothing was installed",
    );
    assert_eq!(read(&home.join("envs/default/nabi.nabion")), manifest);

    // A package does not see it, with a manifest or without.
    for (dir, files) in [
        ("app", &[("nabi.nabion", "Package { name: \"app\" }\n")][..]),
        ("bare", &[][..]),
    ] {
        let app = root.join(dir);
        write(&app.join("src/main.nabi"), script);
        for (f, t) in files {
            write(&app.join(f), t);
        }
        fails(
            &nabi(&home, &app, &["-q", "build"]),
            "= help: `greet` is not a dependency of this package: add it to nabi.nabion (`nabi add \
             greet`)",
        );
    }
    fails(
        &nabi(&home, &root.join("app"), &["--env", "default", "build"]),
        "`--env default` is for scripts and the REPL: a package builds with the dependencies in \
         its nabi.nabion only (add one: `nabi add <name>`)",
    );

    // Executables: a package with src/main.nabi, from a path and from git
    // (built from a copy: the cached checkout stays as it was), and a
    // script file.
    let hello = root.join("hello");
    write(&hello.join("nabi.nabion"), "Package { name: \"hello\" }\n");
    write(&hello.join("src/main.nabi"), "println(\"hello\");\n");
    ok(&nabi(&home, &root, &["install", "--path", "hello"]));
    let tool = root.join("tool");
    let commit = commit(&tool, &[("src/main.nabi", "println(\"tool\");\n")]);
    ok(&nabi(
        &home,
        &root,
        &["install", "--git", &tool.display().to_string()],
    ));
    write(&root.join("hi.nabi"), "println(\"hi\");\n");
    ok(&nabi(&home, &root, &["install", "--path", "hi.nabi"]));
    for (name, want) in [("hello", "hello\n"), ("tool", "tool\n"), ("hi", "hi\n")] {
        let out = Command::new(home.join("bin").join(exe(name)))
            .output()
            .unwrap();
        assert_eq!(ok(&out), want);
    }
    assert!(hello.join("target/release").is_dir());
    assert!(
        !home
            .join("cache/git/checkouts")
            .join(&commit)
            .join("target")
            .exists()
    );
    let list = ok(&nabi(&home, &root, &["install", "--list"]));
    let want = format!(
        "libraries (environment `default`):\n  greet Dep::path({src})\nexecutables ({}):\n  hello \
         Dep::path({})\n  tool Dep::git({}, rev: \"{commit}\")\n  hi Dep::path({})\n",
        home.join("bin").display(),
        q(hello.display().to_string().replace('\\', "/")),
        q(tool.display()),
        q(root
            .join("hi.nabi")
            .display()
            .to_string()
            .replace('\\', "/")),
    );
    assert_eq!(list, want);
    // The same commit again is kept; --force or a new commit rebuilds; a
    // path always does.
    let url = tool.display().to_string();
    let at = |c: &str| format!("{url} at {}", &c[..12]);
    let again = nabi(&home, &root, &["install", "--git", &url]);
    assert!(
        text(&again.stderr).contains(&format!(
            "`tool` {} is installed already ({}); to rebuild it, run `nabi install --git {url} \
             --force`",
            at(&commit),
            home.join("bin").join(exe("tool")).display()
        )),
        "{}",
        text(&again.stderr)
    );
    let forced = nabi(&home, &root, &["install", "--git", &url, "--force"]);
    assert!(
        text(&forced.stderr).contains(&format!(" Reinstalled tool {}", at(&commit))),
        "{}",
        text(&forced.stderr)
    );
    let commit2 = crate::commit(&tool, &[("src/main.nabi", "println(\"tool 2\");\n")]);
    let replaced = nabi(&home, &root, &["install", "--git", &url]);
    assert!(
        text(&replaced.stderr).contains(&format!(
            "    Replaced tool {} with {}",
            at(&commit),
            at(&commit2)
        )),
        "{}",
        text(&replaced.stderr)
    );
    let out = Command::new(home.join("bin").join(exe("tool")))
        .output()
        .unwrap();
    assert_eq!(ok(&out), "tool 2\n");
    let rebuilt = nabi(&home, &root, &["install", "--path", "hello"]);
    assert!(
        text(&rebuilt.stderr).contains(" Reinstalled hello "),
        "{}",
        text(&rebuilt.stderr)
    );
    fails(
        &nabi(&home, &root, &["install", "nabic", "--path", "hello"]),
        "`nabic` is the toolchain's own executable",
    );
    write(&home.join("bin").join(exe("mine")), "");
    fails(
        &nabi(&home, &root, &["install", "mine", "--path", "hello"]),
        "and `nabi install` did not put it there; remove it first",
    );

    // `uninstall`: executables, then the library.
    ok(&nabi(&home, &root, &["uninstall", "hello", "hi"]));
    assert!(!home.join("bin").join(exe("hello")).exists());
    assert!(home.join("bin").join(exe("tool")).is_file());
    let list = ok(&nabi(&home, &root, &["install", "--list"]));
    assert!(
        !list.contains("hello") && list.contains("  tool "),
        "{list}"
    );
    ok(&nabi(&home, &root, &["uninstall", "greet"]));
    let out = nabi(&home, &root, &["-q", "s.nabi"]);
    fails(&out, "= help: run `nabi add greet s.nabi`");
    assert!(
        !text(&out.stderr).contains("note:"),
        "{}",
        text(&out.stderr)
    );
    fails(
        &nabi(&home, &root, &["uninstall", "greet"]),
        "`greet` is not installed: environment `default` has no library `greet`",
    );
    fails(
        &nabi(&home, &root, &["uninstall", "mine"]),
        "`mine` is not installed",
    );
}

/// `nabi install <package>` from a local index: into the environment, pinned
/// in its lock (a registry package with a library).
#[test]
fn install_registry() {
    let root = scratch("install-registry");
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
    package(&num, "num", "1.0.0", "", &[("src/lib.nabi", ONE)]);
    ok(&nabi(&home, &num, &["publish", "--index-dir", &reg]));

    ok(&nabi(&home, &root, &["install", "num"]));
    let env = home.join("envs/default");
    assert!(read(&env.join("nabi.nabion")).contains("\"num\": Dep::version(\"^1.0.0\")"));
    assert_eq!(
        locked_packages(&read(&env.join("nabi.lock"))),
        ["num@1.0.0"]
    );
    ok(&nabi(&home, &root, &["uninstall", "num"]));
    assert!(locked_packages(&read(&env.join("nabi.lock"))).is_empty());
    // A script gets a registry package into its block, pinned as a package's is.
    let file = root.join("s.nabi");
    write(&file, "use num::one;\nprintln(f\"{one()}\");\n");
    ok(&nabi(&home, &root, &["add", "num", "s.nabi"]));
    assert!(read(&file).contains("//!         \"num\": Dep::version(\"^1.0.0\"),\n"));
    assert_eq!(script_out(&home, &root, &["s.nabi"]), "1\n");
    // `import num`: a library name.
    write(&root.join("i.nabi"), "import num\n");
    fails(
        &nabi(&home, &root, &["check", "i.nabi"]),
        "= help: to use library `num`, run `nabi add num i.nabi`, then write `use num::f;`",
    );
}
