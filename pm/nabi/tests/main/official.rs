//! Official packages (`official: true` in the index) come in on use; the
//! `packages` component keeps their archives for offline use.

#[allow(clippy::wildcard_imports)]
use crate::*;

/// An index mirrored as the default one, holding official `json` 0.2.0
/// (its archive served at `served`) and plain `geom` 0.1.0; the index.
pub(crate) fn official_index(root: &Path, home: &Path) -> PathBuf {
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
    mirror(home, &index.display().to_string());
    publish(root, home, &index, &served, "json", "0.2.0", true);
    publish(root, home, &index, &served, "geom", "0.1.0", false);
    index
}

/// Publish `name` `v` to `index` as the registry does: its archive in
/// `served`, the sum in the line, and `official` when it is.
pub(crate) fn publish(
    root: &Path,
    home: &Path,
    index: &Path,
    served: &Path,
    name: &str,
    v: &str,
    official: bool,
) {
    let reg = index.display().to_string();
    let dir = root.join(name);
    let c = package(&dir, name, v, "", &[("src/lib.nabi", ONE)]);
    ok(&nabi(home, &dir, &["publish", "--index-dir", &reg]));
    let dl = served.join(format!("api/v1/packages/{name}/{v}/download"));
    fs::create_dir_all(dl.parent().unwrap()).unwrap();
    let prefix = format!("--prefix={name}-{v}/");
    git(
        &dir,
        &[
            "archive",
            "--format=tar.gz",
            &prefix,
            "-o",
            dl.to_str().unwrap(),
            &c,
        ],
    );
    let sum = sha256_of(&dl);
    let file = if name.len() >= 4 {
        format!("{}/{}/{name}", &name[..2], &name[2..4])
    } else {
        format!("3/{}/{name}", &name[..1])
    };
    let text: String = read(&index.join(&file))
        .lines()
        .map(|l| {
            if !l.contains(&format!("version: \"{v}\"")) {
                return format!("{l}\n");
            }
            let l = l.replace(", yanked:", &format!(", archive: {}, yanked:", q(&sum)));
            if official {
                format!("{}, official: true }}\n", l.trim_end_matches(" }"))
            } else {
                format!("{l}\n")
            }
        })
        .collect();
    commit(index, &[(&file, &text)]);
}

#[test]
fn official_packages_come_in_on_use() {
    let root = scratch("official");
    let home = root.join("home");
    let index = official_index(&root, &home);
    let app = root.join("app");
    write(
        &app.join("src/main.nabi"),
        "use json::one;\nprintln(f\"{one()}\");\n",
    );
    write(
        &app.join("nabi.nabion"),
        "Package { name: \"app\", deps: {} }\n",
    );
    // --locked and CI never edit the manifest.
    fails(
        &nabi(&home, &app, &["--locked", "build"]),
        "`json` is an official package the code uses, but it is not in nabi.nabion, and \
         --locked never changes it; add it: `nabi add json`",
    );
    fails(
        &nabi(&home, &app, &["--frozen", "build"]),
        "--frozen never changes it; add it: `nabi add json`",
    );
    let out = nabi_cmd(&home, &app, &["build"])
        .env("CI", "1")
        .output()
        .unwrap();
    fails(
        &out,
        "CI (`CI` is set) never changes it; add it: `nabi add json`",
    );
    assert!(!read(&app.join("nabi.nabion")).contains("json"));
    // Otherwise it is added, locked, and the command goes on.
    let out = nabi(&home, &app, &["run"]);
    assert_eq!(ok(&out), "1\n");
    assert!(
        text(&out.stderr).contains("Added json ^0.2 (official)"),
        "{}",
        text(&out.stderr)
    );
    assert!(read(&app.join("nabi.nabion")).contains("\"json\": \"^0.2\""));
    assert_eq!(
        locked_packages(&read(&app.join("nabi.lock"))),
        ["json@0.2.0"]
    );
    // Next time nothing is added.
    let out = nabi(&home, &app, &["run"]);
    assert!(
        !text(&out.stderr).contains("Added"),
        "{}",
        text(&out.stderr)
    );
    // A package that is not official keeps the compiler's error.
    write(
        &app.join("src/main.nabi"),
        "use geom::one;\nprintln(f\"{one()}\");\n",
    );
    fails(
        &nabi(&home, &app, &["build"]),
        "`geom` is not a dependency of this package: add it to nabi.nabion (`nabi add geom`)",
    );
    assert!(!read(&app.join("nabi.nabion")).contains("geom"));
    // A local module of that name is no package.
    write(
        &app.join("src/main.nabi"),
        "mod toml;\nprintln(f\"{toml::two()}\");\n",
    );
    write(
        &app.join("src/toml.nabi"),
        "pub fn two() -> i32 { return 2; }\n",
    );
    assert_eq!(ok(&nabi(&home, &app, &["-q", "run"])), "2\n");
    // A script: its own block.
    let s = root.join("s.nabi");
    write(&s, "use json::one;\nprintln(f\"{one() + 1}\");\n");
    let out = nabi(&home, &root, &["run", "s.nabi"]);
    assert_eq!(ok(&out), "2\n", "{}", text(&out.stderr));
    assert!(read(&s).starts_with("//! ```nabion\n"), "{}", read(&s));
    assert!(read(&s).contains("\"json\": \"^0.2\""), "{}", read(&s));
    // The REPL adds it for the cell that uses it (where the REPL runs:
    // not FreeBSD or a static musl nabic).
    if nabic_interface::repl_host() {
        let lib = root.join("replapp");
        write(&lib.join("src/main.nabi"), "println(0);\n");
        write(
            &lib.join("nabi.nabion"),
            "Package { name: \"replapp\", deps: {} }\n",
        );
        let mut child = nabi_cmd(&home, &lib, &["repl"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"json::one() + 41\n")
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(ok(&out).contains("42"), "{}", text(&out.stderr));
        assert!(
            text(&out.stderr).contains("Added json ^0.2 (official)"),
            "{}",
            text(&out.stderr)
        );
        assert!(read(&lib.join("nabi.nabion")).contains("json"));
    }
    // Offline, with the index cached but not the package: what to run.
    let home2 = root.join("home2");
    mirror(&home2, &root.join("index").display().to_string());
    ok(&nabi(&home2, &root, &["search", "json"]));
    let app2 = root.join("app2");
    write(
        &app2.join("src/main.nabi"),
        "use json::one;\nprintln(f\"{one()}\");\n",
    );
    write(
        &app2.join("nabi.nabion"),
        "Package { name: \"app2\", deps: {} }\n",
    );
    fails(
        &nabi(&home2, &app2, &["--offline", "build"]),
        "it is not in the cache and --offline is set: run `nabi add json` online, or keep the \
         official packages for offline use with `nabi component add packages`",
    );
    assert!(!read(&app2.join("nabi.nabion")).contains("json"));
    // In a workspace it goes to the member's manifest.
    let ws = root.join("ws");
    write(&ws.join("nabi.nabion"), "Workspace { members: [\"m\"] }\n");
    write(
        &ws.join("m/nabi.nabion"),
        "Package { name: \"m\", deps: {} }\n",
    );
    write(
        &ws.join("m/src/main.nabi"),
        "println(f\"{json::one()}\");\n",
    );
    ok(&nabi(&home, &ws, &["-q", "build"]));
    assert!(read(&ws.join("m/nabi.nabion")).contains("\"json\": \"^0.2\""));
    assert!(ws.join("nabi.lock").is_file() && !ws.join("m/nabi.lock").exists());
    // A code name with `_` is the package with `-`.
    publish(
        &root,
        &home,
        &index,
        &root.join("served"),
        "two-words",
        "0.1.0",
        true,
    );
    ok(&nabi(&home, &root, &["search", "two"]));
    let app3 = root.join("app3");
    write(
        &app3.join("src/main.nabi"),
        "println(f\"{two_words::one()}\");\n",
    );
    write(
        &app3.join("nabi.nabion"),
        "Package { name: \"app3\", deps: {} }\n",
    );
    let out = nabi(&home, &app3, &["run"]);
    assert_eq!(ok(&out), "1\n", "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("Added two-words ^0.1 (official)"),
        "{}",
        text(&out.stderr)
    );
    assert!(read(&app3.join("nabi.nabion")).contains("\"two-words\": \"^0.1\""));
}

/// An official package that only a dependency has is not the root's: the
/// root's use of it adds it to its own manifest, and the build goes on.
#[test]
fn official_package_of_a_dependency_comes_in_on_use() {
    let root = scratch("official-dep");
    let home = root.join("home");
    official_index(&root, &home);
    let mid = root.join("mid");
    write(
        &mid.join("src/lib.nabi"),
        "pub fn two() -> i32 { return json::one() + 1; }\n",
    );
    write(
        &mid.join("nabi.nabion"),
        "Package { name: \"mid\", deps: { \"json\": \"^0.2\" } }\n",
    );
    let app = root.join("app");
    write(
        &app.join("src/main.nabi"),
        "println(f\"{mid::two()} {json::one()}\");\n",
    );
    write(
        &app.join("nabi.nabion"),
        "Package { name: \"app\", deps: { \"mid\": Dep::path(\"../mid\") } }\n",
    );
    fails(
        &nabi(&home, &app, &["--locked", "build"]),
        "`json` is an official package the code uses, but it is not in nabi.nabion",
    );
    let out = nabi(&home, &app, &["run"]);
    assert_eq!(ok(&out), "2 1\n");
    assert!(
        text(&out.stderr).contains("Added json ^0.2 (official)"),
        "{}",
        text(&out.stderr)
    );
    assert!(read(&app.join("nabi.nabion")).contains("\"json\": \"^0.2\""));
}

/// `nabi component add packages` keeps the newest official archives, so
/// official packages resolve and come in on use offline; `update` fetches
/// newer ones, `remove` deletes them.
#[test]
fn packages_component_for_offline_use() {
    let root = scratch("official-component");
    let home = root.join("home");
    let index = official_index(&root, &home);
    let served = root.join("served");
    assert!(ok(&nabi(&home, &root, &["component", "list"])).contains("packages not added"));
    fails(
        &nabi(&home, &root, &["--offline", "component", "add", "packages"]),
        "run it without --offline",
    );
    fails(
        &nabi(&home, &root, &["component", "add", "pkgs"]),
        "there is no component `pkgs` (components: packages)",
    );
    let out = nabi(&home, &root, &["component", "add", "packages"]);
    ok(&out);
    let err = text(&out.stderr);
    assert!(err.contains("Downloading json 0.2.0"), "{err}");
    assert!(!err.contains("geom"), "{err}");
    assert!(
        err.contains("Added component packages (1 official packages)"),
        "{err}"
    );
    assert!(read(&home.join("components.nabion")).contains("\"json@0.2.0\""));
    assert!(ok(&nabi(&home, &root, &["component", "list"])).contains("added: 1 package(s)"));
    // Offline from here: auto-add and the build use the kept archive.
    let app = root.join("app");
    write(
        &app.join("src/main.nabi"),
        "use json::one;\nprintln(f\"{one()}\");\n",
    );
    write(
        &app.join("nabi.nabion"),
        "Package { name: \"app\", deps: {} }\n",
    );
    let out = nabi(&home, &app, &["--offline", "run"]);
    assert_eq!(ok(&out), "1\n", "{}", text(&out.stderr));
    assert!(!home.join("cache/git/db").exists());
    // A version the component does not hold: what to run.
    publish(&root, &home, &index, &served, "json", "0.2.5", true);
    ok(&nabi(&home, &root, &["search", "json"]));
    write(
        &app.join("nabi.nabion"),
        "Package { name: \"app\", deps: { \"json\": \"=0.2.5\" } }\n",
    );
    fails(
        &nabi(&home, &app, &["--offline", "run"]),
        "package `json` 0.2.5 is not in the cache, and --offline is set: run the command once \
         without --offline; `nabi component add packages` (or `update`) keeps the official \
         packages for offline use",
    );
    let out = nabi(&home, &root, &["component", "update", "packages"]);
    assert!(
        text(&out.stderr).contains("Updated component packages"),
        "{}",
        text(&out.stderr)
    );
    assert!(read(&home.join("components.nabion")).contains("\"json@0.2.5\""));
    assert_eq!(ok(&nabi(&home, &app, &["--offline", "-q", "run"])), "1\n");
    ok(&nabi(&home, &root, &["component", "remove", "packages"]));
    assert!(!home.join("components.nabion").exists());
    let src = fs::read_dir(home.join("cache/registry/src"))
        .unwrap()
        .count();
    assert_eq!(
        src, 1,
        "the 0.2.0 archive stays: the component no longer held it"
    );
    fails(
        &nabi(&home, &root, &["component", "remove", "packages"]),
        "the `packages` component is not added",
    );
}

/// `exe` (a toolchain's `nabi`) with the environment `nabi` runs with.
fn tool(exe: &Path, home: &Path, cwd: &Path, args: &[&str]) -> Output {
    let base = nabi_cmd(home, cwd, args);
    let mut c = Command::new(exe);
    c.current_dir(cwd).args(args);
    for (k, v) in base.get_envs() {
        match v {
            Some(v) => c.env(k, v),
            None => c.env_remove(k),
        };
    }
    c.output().unwrap()
}

/// A release toolchain carries the official packages: they resolve and
/// come in on use with no index and no network, a version it carries
/// goes first, `nabi update` moves past it, and `nabi component remove
/// packages` deletes them.
#[test]
fn toolchain_carries_official_packages() {
    let root = scratch("official-bundle");
    let home = root.join("home");
    let index = official_index(&root, &home);
    let served = root.join("served");
    let tc = root.join("tc");
    crate::toolchain::fake_toolchain(&tc, "9.9.7");
    let exe = tc.join("bin/nabi");
    let packages = tc.join("lib/nabi/packages");
    let out = nabi(
        &home,
        &root,
        &[
            "component",
            "add",
            "packages",
            "--bundle",
            packages.to_str().unwrap(),
        ],
    );
    ok(&out);
    assert!(
        text(&out.stderr).contains("Bundled 1 official packages into"),
        "{}",
        text(&out.stderr)
    );
    let line = read(&packages.join("index"));
    assert!(
        line.contains("name: \"json\"") && line.contains("official: true"),
        "{line}"
    );
    publish(&root, &home, &index, &served, "json", "0.2.5", true);
    // A home with no index and nothing cached.
    let fresh = root.join("fresh");
    mirror(&fresh, &index.display().to_string());
    let app = root.join("app");
    write(&app.join("src/main.nabi"), "println(\"x\");\n");
    write(
        &app.join("nabi.nabion"),
        "Package { name: \"app\", deps: { \"json\": \"^0.2\" } }\n",
    );
    let deps = ok(&tool(&exe, &fresh, &app, &["--offline", "deps"]));
    assert!(
        deps.starts_with(&format!("json={}", packages.join("src").display())),
        "{deps}"
    );
    assert!(read(&app.join("nabi.lock")).contains("version: \"0.2.0\""));
    assert!(!fresh.join("cache/registry").exists());
    // Online too, the version it carries goes first.
    fs::remove_file(app.join("nabi.lock")).unwrap();
    ok(&tool(&exe, &fresh, &app, &["deps"]));
    assert!(read(&app.join("nabi.lock")).contains("version: \"0.2.0\""));
    // `update` takes the newest.
    ok(&tool(&exe, &fresh, &app, &["update"]));
    assert!(read(&app.join("nabi.lock")).contains("version: \"0.2.5\""));
    // Used without a dependency, it comes in from the toolchain offline.
    let used = root.join("used");
    write(
        &used.join("src/main.nabi"),
        "use json::one;\nprintln(f\"{one()}\");\n",
    );
    write(
        &used.join("nabi.nabion"),
        "Package { name: \"used\", deps: {} }\n",
    );
    let bare = root.join("bare");
    let out = tool(&exe, &bare, &used, &["--offline", "check"]);
    ok(&out);
    assert!(
        text(&out.stderr).contains("Added json ^0.2 (official)"),
        "{}",
        text(&out.stderr)
    );
    assert!(read(&used.join("nabi.lock")).contains("version: \"0.2.0\""));
    let list = ok(&tool(&exe, &bare, &root, &["component", "list"]));
    assert!(
        list.contains("added: 1 package(s) with the toolchain"),
        "{list}"
    );
    ok(&tool(
        &exe,
        &bare,
        &root,
        &["component", "remove", "packages"],
    ));
    assert!(!packages.exists());
    let list = ok(&tool(&exe, &bare, &root, &["component", "list"]));
    assert!(list.contains("packages not added"), "{list}");
    fails(
        &tool(&exe, &bare, &root, &["component", "remove", "packages"]),
        "the `packages` component is not added",
    );
}

/// The packages an archive carries stay installed; `install.sh
/// --no-packages` (or `NABI_NO_PACKAGES=1`) removes them.
#[test]
fn install_sh_no_packages() {
    let root = scratch("official-install");
    let top = root.join("stage/nabi-9.9.8");
    crate::toolchain::fake_toolchain(&top, "9.9.8");
    write(&top.join("lib/nabi/packages/index"), "");
    let tar = Command::new("tar")
        .current_dir(root.join("stage"))
        .args(["-czf", "../nabi.tar.gz", "nabi-9.9.8"])
        .status()
        .unwrap();
    assert!(tar.success());
    let host = env!("NABI_HOST");
    for (i, how) in ["default", "flag", "env"].into_iter().enumerate() {
        let user = root.join(format!("user{i}"));
        let mut c = crate::install::install_sh(&root, &user, "/bin/sh");
        c.args(["--archive", "nabi.tar.gz", "--no-modify-path"])
            .env_remove("NABI_NO_PACKAGES");
        match how {
            "flag" => {
                c.arg("--no-packages");
            }
            "env" => {
                c.env("NABI_NO_PACKAGES", "1");
            }
            _ => {}
        }
        ok(&c.output().unwrap());
        let kept = user
            .join(format!(".nabi/toolchains/9.9.8-{host}/lib/nabi/packages"))
            .exists();
        assert_eq!(kept, how == "default", "{how}");
    }
}
