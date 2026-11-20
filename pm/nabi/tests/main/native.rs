//! Native packages.

#[allow(clippy::wildcard_imports)]
use crate::*;

/// Native packages end to end on a local index: `yy` (a shared library)
/// and `zz` (a static one calling into `yy`) made from C with the
/// system `cc`, published, added, locked, linked and run; a missing
/// target, a changed archive, the offline cache and a `Dep::native(url:)`.
#[cfg(unix)]
#[test]
#[cfg_attr(
    target_env = "musl",
    ignore = "a static musl toolchain links no shared library (`yy`)"
)]
fn native_packages() {
    if Command::new("cc").arg("--version").output().is_err() {
        return;
    }
    let host = env!("NABI_HOST");
    let apple = nabic_target::TargetSpec::host().object_format == nabic_target::ObjectFormat::MachO;
    let root = scratch("native");
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

    // The archives: `lib/` and a license, packed by `tar`.
    let dist = root.join("dist");
    fs::create_dir_all(&dist).unwrap();
    let pack = |name: &str, c: &str, shared: bool| {
        let src = root.join(name);
        write(&src.join(format!("{name}.c")), c);
        write(&src.join("pkg/LICENSE"), "MIT\n");
        fs::create_dir_all(src.join("pkg/lib")).unwrap();
        let c_file = format!("{name}.c");
        if !shared {
            sh(&src, "cc", &["-c", "-fPIC", &c_file, "-o", "x.o"]);
            sh(&src, "ar", &["rcs", &format!("pkg/lib/lib{name}.a"), "x.o"]);
        } else if apple {
            let lib = format!("pkg/lib/lib{name}.dylib");
            let id = format!("@rpath/lib{name}.dylib");
            sh(
                &src,
                "cc",
                &["-dynamiclib", "-install_name", &id, "-o", &lib, &c_file],
            );
        } else {
            let lib = format!("pkg/lib/lib{name}.so");
            let so = format!("-Wl,-soname,lib{name}.so");
            sh(&src, "cc", &["-shared", "-fPIC", &so, "-o", &lib, &c_file]);
        }
        let tgz = dist.join(format!("{name}-1.0.0.tar.gz"));
        let tgz_arg = tgz.display().to_string();
        sh(&src, "tar", &["-czf", &tgz_arg, "-C", "pkg", "."]);
        sha256_of(&tgz)
    };
    let yy_sum = pack("yy", "int yy_mul(int a, int b) { return a * b; }\n", true);
    let zz_sum = pack(
        "zz",
        "int yy_mul(int, int);\nint zz_muladd(int a, int b, int c) { return yy_mul(a, b) + c; }\n",
        false,
    );
    let ext = if apple { "dylib" } else { "so" };
    let yy_meta = format!(
        "Native {{ name: \"yy\", version: \"1.0.0\", targets: {{ \"{host}\": Artifact {{ url: \
         \"file://{}/yy-1.0.0.tar.gz\", sha256: \"{yy_sum}\", links: {{ \"yy\": \"dylib\" }}, \
         runtime: [\"lib/libyy.{ext}\"] }} }}, license: \"MIT\" }}\n",
        dist.display()
    );
    let zz_meta = format!(
        "Native {{ name: \"zz\", version: \"1.0.0\", deps: {{ \"yy\": \"^1.0\" }}, targets: {{ \
         \"{host}\": Artifact {{ url: \"file://{}/zz-1.0.0.tar.gz\", sha256: \"{zz_sum}\", links: \
         {{ \"zz\": \"static\" }} }} }} }}\n",
        dist.display()
    );
    write(&root.join("yy.nabion"), &yy_meta);
    write(&root.join("zz.nabion"), &zz_meta);

    // `publish --native`: a dry run, then committed; one namespace per kind.
    let line = ok(&nabi(
        &home,
        &root,
        &["publish", "--native", "yy.nabion", "--dry-run"],
    ));
    assert!(
        line.starts_with("Native { name: \"yy\", version: \"1.0.0\", deps: {}, ")
            && !line.contains("license"),
        "{line}"
    );
    assert!(!index.join("native/2/yy").exists());
    for f in ["yy.nabion", "zz.nabion"] {
        let args = ["publish", "--native", f, "--index-dir", &reg];
        ok(&nabi(&home, &root, &args));
    }
    assert_eq!(read(&index.join("native/2/yy")), line);
    assert!(git(&index, &["log", "--format=%s"]).starts_with("Publish native zz 1.0.0\n"));
    fails(
        &nabi(
            &home,
            &root,
            &["publish", "--native", "yy.nabion", "--index-dir", &reg],
        ),
        "native package yy 1.0.0 is already in the index",
    );
    assert_eq!(
        ok(&nabi(&home, &root, &["search", "--native", "y"])),
        "yy 1.0.0\n"
    );
    assert_eq!(ok(&nabi(&home, &root, &["search", "y"])), "");
    native_yank(&home, &root, &index, &line);
    // No license, no publish.
    fs::remove_file(root.join("yy/pkg/LICENSE")).unwrap();
    let bad = dist.join("yy-2.0.0.tar.gz");
    let bad_arg = bad.display().to_string();
    sh(
        &root.join("yy"),
        "tar",
        &["-czf", &bad_arg, "-C", "pkg", "."],
    );
    let bad_meta = yy_meta
        .replace("1.0.0", "2.0.0")
        .replace(&yy_sum, &sha256_of(&bad));
    write(&root.join("bad.nabion"), &bad_meta);
    fails(
        &nabi(
            &home,
            &root,
            &["publish", "--native", "bad.nabion", "--dry-run"],
        ),
        "the archive has no license file",
    );

    // An app calling both through `extern "C"` (no `#[link]`: the
    // packages name their libraries).
    let app = root.join("app");
    ok(&nabi(&home, &root, &["new", "app"]));
    write(
        &app.join("src/main.nabi"),
        "extern \"C\" {\n    fn zz_muladd(a: i32, b: i32, c: i32) -> i32;\n    fn yy_mul(a: i32, b: \
         i32) -> i32;\n}\nlet a = unsafe { zz_muladd(6, 7, 1) };\nlet b = unsafe { yy_mul(2, 3) \
         };\nprintln(f\"{a} {b}\");\n",
    );
    ok(&nabi(&home, &app, &["add", "--native", "zz"]));
    let manifest = read(&app.join("nabi.nabion"));
    assert!(
        manifest.contains("\"zz\": Dep::native(\"^1.0.0\")"),
        "{manifest}"
    );
    let lock = read(&app.join("nabi.lock"));
    for n in ["yy", "zz"] {
        assert!(
            lock.contains(&format!(
                "LockedNative {{\n            name: \"{n}\",\n            version: \"1.0.0\",\n"
            )),
            "{lock}"
        );
    }
    assert_eq!(ok(&nabi(&home, &app, &["-q", "run"])), "43 6\n");
    // The shared library sits beside the executable, which finds it there
    // from anywhere.
    assert!(app.join(format!("target/debug/libyy.{ext}")).is_file());
    let run = Command::new(app.join("target/debug/app"))
        .current_dir(&root)
        .output()
        .unwrap();
    assert_eq!(ok(&run), "43 6\n");
    native_in_tools(&home, &root, &app, ext);

    // A target the packages have no build for.
    let other = if host == "x86_64-unknown-linux-gnu" {
        "aarch64-unknown-linux-gnu"
    } else {
        "x86_64-unknown-linux-gnu"
    };
    fails(
        &nabi(&home, &app, &["build", "--target", other]),
        &format!("native package `zz` 1.0.0 has no build for {other} (it has: {host})"),
    );
    // A file named by url and hash; the artifact url relative to it.
    let vend = root.join("vend");
    fs::create_dir_all(&vend).unwrap();
    fs::copy(dist.join("yy-1.0.0.tar.gz"), vend.join("yy.tgz")).unwrap();
    let from = format!("file://{}/yy-1.0.0.tar.gz", dist.display());
    write(&vend.join("yy.nabion"), &yy_meta.replace(&from, "yy.tgz"));
    let direct = root.join("direct");
    write(
        &direct.join("src/main.nabi"),
        "extern \"C\" { fn yy_mul(a: i32, b: i32) -> i32; }\nlet x = unsafe { yy_mul(4, 5) \
         };\nprintln(f\"{x}\");\n",
    );
    let manifest = |sum: &str| {
        format!(
            "Package {{ native: {{ \"yy\": Dep::native(url: \"../vend/yy.nabion\", sha256: \
             \"{sum}\") }} }}\n"
        )
    };
    write(&direct.join("nabi.nabion"), &manifest(&"0".repeat(64)));
    fails(&nabi(&home, &direct, &["-q", "run"]), "but 0000");
    let sum = sha256_of(&vend.join("yy.nabion"));
    write(&direct.join("nabi.nabion"), &manifest(&sum));
    assert_eq!(ok(&nabi(&home, &direct, &["-q", "run"])), "20\n");

    // Offline: what is cached builds without its sources.
    for f in ["yy-1.0.0.tar.gz", "zz-1.0.0.tar.gz"] {
        fs::remove_file(dist.join(f)).unwrap();
    }
    ok(&nabi(&home, &app, &["-q", "clean"]));
    assert_eq!(
        ok(&nabi(&home, &app, &["--offline", "-q", "run"])),
        "43 6\n"
    );

    // A changed archive is refused (a fresh cache fetches it again).
    write(&dist.join("yy-1.0.0.tar.gz"), "not the archive");
    write(&dist.join("zz-1.0.0.tar.gz"), "not the archive");
    let home2 = root.join("home2");
    mirror(&home2, &reg);
    let out = nabi(&home2, &app, &["-q", "build"]);
    fails(
        &out,
        &format!("native package `zz` 1.0.0 ({host}): file://"),
    );
    fails(&out, &format!("but {zz_sum} is expected"));

    // `remove --native` takes it out of nabi.nabion and nabi.lock.
    ok(&nabi(&home, &app, &["remove", "--native", "zz"]));
    assert!(!read(&app.join("nabi.lock")).contains("LockedNative"));
}

/// `native_packages`' app beyond `run`: a deleted runtime copy, `deps`,
/// checks and the REPL.
#[cfg(unix)]
pub(crate) fn native_in_tools(home: &Path, root: &Path, app: &Path, ext: &str) {
    // A deleted copy comes back on the next build.
    let copy = app.join(format!("target/debug/libyy.{ext}"));
    fs::remove_file(&copy).unwrap();
    ok(&nabi(home, app, &["-q", "build"]));
    assert!(copy.is_file());

    // `deps` lists them: nabic's arguments under a `# native` line each.
    let deps = ok(&nabi(home, app, &["deps"]));
    for want in [
        "# native zz 1.0.0\n-L",
        "-lstatic=zz\n# native yy 1.0.0\n-L",
        "-ldylib=yy\n--native-runtime=",
    ] {
        assert!(deps.contains(want), "{want:?} in {deps}");
    }
    // Checks link nothing.
    for c in ["check", "lint", "doc"] {
        ok(&nabi(home, app, &["-q", c]));
    }
    // The REPL loads both: the shared library, then the static one. In
    // an environment, `:add --native` adds them to a running session
    // (where the REPL runs: not FreeBSD or a static musl nabic).
    if !nabic_interface::repl_host() {
        return;
    }
    let repl = |args: &[&str], input: &str| {
        let mut child = nabi_cmd(home, app, args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(input.as_bytes()).unwrap();
        drop(stdin);
        let out = child.wait_with_output().unwrap();
        let stdout = ok(&out);
        assert!(
            stdout.contains("43\n6\n"),
            "{stdout}\n{}",
            text(&out.stderr)
        );
    };
    let cells = "extern \"C\" {\n    fn zz_muladd(a: i32, b: i32, c: i32) -> i32;\n    fn \
                 yy_mul(a: i32, b: i32) -> i32;\n}\nunsafe { zz_muladd(6, 7, 1) }\nunsafe { \
                 yy_mul(2, 3) }\n";
    repl(&["-q", "repl"], cells);
    ok(&nabi(home, root, &["env", "create", "n"]));
    repl(&["--env", "n"], &format!(":add --native zz\n{cells}"));
}

/// `yank --native` hides a version from resolution; `--undo` brings it
/// back (to `line`).
#[cfg(unix)]
pub(crate) fn native_yank(home: &Path, root: &Path, index: &Path, line: &str) {
    let reg = index.display().to_string();
    let yank = ["yank", "--native", "yy@1.0.0", "--index-dir", &reg];
    ok(&nabi(home, root, &yank));
    assert!(read(&index.join("native/2/yy")).contains("yanked: true"));
    assert!(git(index, &["log", "--format=%s"]).starts_with("Yank native yy 1.0.0\n"));
    assert_eq!(ok(&nabi(home, root, &["search", "--native", "y"])), "");
    ok(&nabi(home, root, &[&yank[..], &["--undo"]].concat()));
    assert_eq!(read(&index.join("native/2/yy")), line);
}
