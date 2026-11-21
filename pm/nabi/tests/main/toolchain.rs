//! Toolchains: pins, proxies and updates.

#[allow(clippy::wildcard_imports)]
use crate::*;
#[cfg(unix)]
use nabi_toolchain::sign::testing::TestKey;

#[cfg(unix)]
/// The throwaway key fake releases are signed with; downloads trust it
/// with `--key` (`keyed`).
pub(crate) const TEST_KEY: TestKey = TestKey {
    seed: [0x5a; 32],
    id: *b"nabitest",
};

#[cfg(unix)]
/// `TEST_KEY`'s public key file under `root`; its path.
pub(crate) fn test_key(root: &Path) -> String {
    let f = root.join("test.pub");
    write(
        &f,
        &format!("untrusted comment: test key\n{}\n", TEST_KEY.public()),
    );
    f.display().to_string()
}

#[cfg(unix)]
/// `args` (after `toolchain`) with `--key <key>` when they download.
pub(crate) fn keyed<'a>(args: &[&'a str], key: &'a str) -> Vec<&'a str> {
    let mut v = args.to_vec();
    if matches!(args.first(), Some(&("install" | "update"))) {
        v.extend(["--key", key]);
    }
    v
}

#[cfg(unix)]
/// What a download checked with `--key key` warns first.
fn key_warning(key: &str) -> String {
    format!(
        "warning: trusting key 747365746962616E from {key} instead of the nabi release key \
         B33E6FE61BE5314C\n"
    )
}

#[cfg(unix)]
/// Sign release directory `dir` as release.yml does: `SHA256SUMS` over
/// its archives (`wrong` lists a wrong digest instead), then
/// `SHA256SUMS.minisig`.
pub(crate) fn sign_release(dir: &Path, wrong: Option<&str>) {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        // The fake releases name their archives in lower case.
        .filter(|n| {
            #[allow(clippy::case_sensitive_file_extension_comparisons)]
            let archive = n.ends_with(".tar.gz") || n.ends_with(".zip");
            archive
        })
        .collect();
    names.sort();
    let mut sums = String::new();
    for n in &names {
        let d = wrong.map_or_else(|| sha256_of(&dir.join(n)), str::to_owned);
        sums.push_str(&format!("{d}  {n}\n"));
    }
    write(&dir.join("SHA256SUMS"), &sums);
    write(
        &dir.join("SHA256SUMS.minisig"),
        &TEST_KEY.minisig(sums.as_bytes(), "nabi test SHA256SUMS"),
    );
}

/// After `nabi toolchain default`, a `nabi` earlier on PATH than the
/// proxies gets a warning naming it and its version.
#[cfg(unix)]
#[test]
fn toolchain_warns_about_path_shadow() {
    let root = scratch("toolchain-shadow");
    let home = root.join("home");
    let host = env!("NABI_HOST");
    fake_toolchain(&root.join("fake1"), "9.9.1");
    fake_toolchain(&root.join("fake2"), "9.10.0");
    for (v, dir) in [("9.9.1", "fake1"), ("9.10.0", "fake2")] {
        ok(&nabi(
            &home,
            &root,
            &["toolchain", "install", v, "--from", dir],
        ));
    }
    // The proxies first on PATH (as `nabi_cmd` puts them): no warning.
    let out = nabi(&home, &root, &["toolchain", "default", "9.10"]);
    ok(&out);
    assert!(
        !text(&out.stderr).contains("warning"),
        "{}",
        text(&out.stderr)
    );
    // An old `nabi` before them.
    let old = root.join("old/nabi");
    write(&old, "#!/bin/sh\necho 'nabi 0.0.1 (old)'\n");
    fs::set_permissions(&old, fs::Permissions::from_mode(0o755)).unwrap();
    let path = std::env::var_os("PATH").unwrap_or_default();
    let dirs = [root.join("old"), home.join("bin")]
        .into_iter()
        .chain(std::env::split_paths(&path));
    let mut c = nabi_cmd(&home, &root, &["toolchain", "default", "9.9"]);
    c.env("PATH", std::env::join_paths(dirs).unwrap());
    let out = c.output().unwrap();
    ok(&out);
    let err = text(&out.stderr);
    assert!(
        err.contains(&format!(
            "nabi: warning: your shell runs {} (nabi 0.0.1) before {}; put {} first on PATH or \
             remove the other one\n",
            old.display(),
            home.join("bin/nabi").display(),
            home.join("bin").display()
        )),
        "{err}"
    );
    // `--version -v` through the proxy names the toolchain and why.
    let mut c = Command::new(home.join("bin/nabi"));
    c.current_dir(&root)
        .env_remove("NABI_HOME")
        .env_remove("NABI_TOOLCHAIN")
        .args(["--version", "-v"]);
    let out = ok(&c.output().unwrap());
    let tc = nabic_core::real_path(home.join(format!("toolchains/9.9.1-{host}"))).unwrap();
    assert!(
        out.contains(&format!("binary:    {}\n", tc.join("bin/nabi").display())),
        "{out}"
    );
    assert!(
        out.contains(&format!("toolchain: 9.9.1-{host} (the default)\n")),
        "{out}"
    );
}

/// A fake toolchain `version` in `dir`: the real `nabi`, and a `nabic`
/// that says which toolchain it is and how it was called.
#[cfg(unix)]
pub(crate) fn fake_toolchain(dir: &Path, version: &str) {
    let host = env!("NABI_HOST");
    write(
        &dir.join("bin/nabic"),
        &format!(
            "#!/bin/sh\nif [ \"$1\" = --version ]; then echo 'nabic {version} (fake)'; echo \
             'host: {host}'; exit 0; fi\necho \"nabic {version}: $*\"\n"
        ),
    );
    fs::set_permissions(dir.join("bin/nabic"), fs::Permissions::from_mode(0o755)).unwrap();
    fs::copy(small_nabi(), dir.join("bin/nabi")).unwrap();
    write(&dir.join("bin/nabils"), "");
    write(&dir.join("lib/libnabir.a"), "");
    write(&dir.join("lib/nabi/core/src/lib.nabi"), "");
}

/// This `nabi` without its debug info, made once: a debug build's is
/// ~400 MB on Linux, and each fake toolchain, its archive, the installed
/// copy and the three proxies would hold one, enough to fill a disk over
/// the run. Without `strip`, a plain copy.
#[cfg(unix)]
fn small_nabi() -> &'static Path {
    static SMALL: OnceLock<PathBuf> = OnceLock::new();
    SMALL.get_or_init(|| {
        let small = scratch("small-nabi").join("nabi");
        fs::copy(NABI, &small).unwrap();
        let stripped = Command::new("strip")
            .arg("-S")
            .arg(&small)
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if !stripped {
            fs::copy(NABI, &small).unwrap();
        }
        small
    })
}

/// Make the `nabi` of fake toolchain `dir` (`fake_toolchain`) answer
/// `completions <shell>` with `nabi <version>: completions <shell>`, so a
/// test sees which toolchain wrote a script; `None`: refuse it, as a nabi
/// too old for `completions` does.
#[cfg(unix)]
pub(crate) fn fake_completions(dir: &Path, version: Option<&str>) {
    let exe = dir.join("bin/nabi");
    let real = dir.join("bin/nabi.real");
    fs::rename(&exe, &real).unwrap();
    let answer = match version {
        Some(v) => format!("echo \"nabi {v}: completions $2\"; exit 0"),
        None => "exit 2".to_owned(),
    };
    write(
        &exe,
        &format!(
            "#!/bin/sh\nif [ \"$1\" = completions ]; then {answer}; fi\nexec \"{}\" \"$@\"\n",
            real.display()
        ),
    );
    fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(unix)]
#[test]
fn pinned_toolchains() {
    let root = scratch("toolchain");
    let home = root.join("home");
    let app = root.join("app");
    let host = env!("NABI_HOST");
    write(&app.join("src/main.nabi"), "println(\"x\");\n");
    write(&app.join("nabi.nabion"), "Package { toolchain: \"9.9\" }\n");
    fails(&nabi(&home, &app, &["build"]), "nabi toolchain install 9.9");

    fake_toolchain(&root.join("fake1"), "9.9.1");
    fake_toolchain(&root.join("fake2/nabi-9.10.0"), "9.10.0");
    let key = test_key(&root);
    let tc = |args: &[&str]| {
        nabi(
            &home,
            &root,
            &[&["toolchain"][..], &keyed(args, &key)].concat(),
        )
    };
    // Without --from, a release is downloaded from NABI_DIST.
    let empty = format!("http://127.0.0.1:{}", serve_dir(root.join("empty")));
    let mut c = nabi_cmd(&home, &root, &["toolchain", "install", "9.9.1"]);
    fails(
        &c.env("NABI_DIST", &empty).output().unwrap(),
        "/download/v9.9.1/nabi-9.9.1-",
    );
    let mut c = nabi_cmd(&home, &root, &["toolchain", "install", "9.9"]);
    fails(
        &c.env("NABI_DIST", &empty).output().unwrap(),
        "nabi toolchain install: `9.9` is not a full version; write one like `0.1.0`",
    );
    assert_eq!(
        tc(&["install", "9.9.1", "--frm", "x"]).status.code(),
        Some(2)
    );
    fails(&tc(&["install", "9.9.1", "--from", "app"]), "no bin/nabic");
    // The version is the toolchain's own (`bin/nabic --version`).
    fails(
        &tc(&["install", "9.11", "--from", "fake1"]),
        "is nabic 9.9.1, not 9.11",
    );
    ok(&tc(&["install", "9.9.1", "--from", "fake1"]));
    // A tarball with one top directory.
    let tar = Command::new("tar")
        .current_dir(root.join("fake2"))
        .args(["-czf", "../tc.tar.gz", "nabi-9.10.0"])
        .status()
        .unwrap();
    assert!(tar.success());
    ok(&tc(&["install", "9.10", "--from", "tc.tar.gz"]));
    // An archive is checked against the `.sha256` beside it, a download
    // against the one at its URL (which must exist).
    let sum = root.join("tc.tar.gz.sha256");
    write(&sum, &format!("{}  tc.tar.gz\n", "0".repeat(64)));
    fails(
        &tc(&["install", "9.10", "--from", "tc.tar.gz"]),
        "its .sha256 says 0000",
    );
    let good = format!("{}  tc.tar.gz\n", sha256_of(&root.join("tc.tar.gz")));
    write(&sum, &good);
    ok(&tc(&["install", "9.10", "--from", "tc.tar.gz"]));
    // A download is checked against the signed SHA256SUMS beside it,
    // and so is a local archive that has one beside it.
    let url = format!("http://127.0.0.1:{}/tc.tar.gz", serve_dir(root.clone()));
    fails(
        &tc(&["install", "9.10", "--from", &url]),
        "a release is installed only with its signed SHA256SUMS",
    );
    sign_release(&root, None);
    ok(&tc(&["install", "9.10", "--from", &url]));
    ok(&tc(&["install", "9.10", "--from", "tc.tar.gz"]));
    let out = tc(&["install", "9.10", "--from", &url]);
    assert!(
        text(&out.stderr).starts_with(&key_warning(&key)),
        "{}",
        text(&out.stderr)
    );
    // Not the trusted key: refused. --key is a flag, not the environment.
    let mut c = nabi_cmd(
        &home,
        &root,
        &["toolchain", "install", "9.10", "--from", &url],
    );
    fails(
        &c.env("NABI_RELEASE_KEY", TEST_KEY.public())
            .output()
            .unwrap(),
        "is signed with key 747365746962616E, not the release key B33E6FE61BE5314C",
    );
    fails(
        &nabi(
            &home,
            &root,
            &[
                "toolchain",
                "install",
                "9.10",
                "--from",
                &url,
                "--key",
                "nokey.pub",
            ],
        ),
        "--key nokey.pub: ",
    );
    assert_eq!(
        nabi(&home, &root, &["toolchain", "list", "--key", &key])
            .status
            .code(),
        Some(2)
    );
    sign_release(&root, Some(&"f".repeat(64)));
    for from in [&url[..], "tc.tar.gz"] {
        fails(
            &tc(&["install", "9.10", "--from", from]),
            "but the signed SHA256SUMS says ffff",
        );
    }
    // SHA256SUMS changed after it was signed.
    sign_release(&root, None);
    let sums = read(&root.join("SHA256SUMS"));
    write(&root.join("SHA256SUMS"), &sums.replacen('a', "b", 1));
    fails(
        &tc(&["install", "9.10", "--from", &url]),
        "SHA256SUMS.minisig does not verify",
    );
    fs::remove_file(root.join("SHA256SUMS.minisig")).unwrap();
    fails(
        &tc(&["install", "9.10", "--from", &url]),
        "SHA256SUMS.minisig",
    );
    fails(
        &tc(&["install", "9.10", "--from", "tc.tar.gz"]),
        "has no SHA256SUMS.minisig beside it",
    );
    fs::remove_file(root.join("SHA256SUMS")).unwrap();
    write(&sum, &good);
    assert!(
        home.join(format!("toolchains/9.10.0-{host}/bin/nabic"))
            .is_file()
    );
    // The first one became the default, with the proxies.
    assert_eq!(
        ok(&tc(&["list"])),
        format!("* 9.9.1-{host} (default)\n  9.10.0-{host}\n")
    );
    assert!(home.join("bin/nabi").is_file() && home.join("bin/nabic").is_file());

    // The proxies (no NABI_HOME: theirs is where they are). Precedence:
    // `+<version>` > NABI_TOOLCHAIN > nabi.nabion > the default.
    let proxy = |tool: &str, cwd: &Path, env: Option<&str>, args: &[&str]| {
        let mut c = Command::new(home.join("bin").join(tool));
        c.current_dir(cwd).env_remove("NABI_HOME").args(args);
        match env {
            Some(t) => c.env("NABI_TOOLCHAIN", t),
            None => c.env_remove("NABI_TOOLCHAIN"),
        };
        ok(&c.output().unwrap())
    };
    assert_eq!(proxy("nabic", &root, None, &["x"]), "nabic 9.9.1: x\n");
    let out = nabi(&home, &root, &["toolchain", "default", "9.10"]);
    ok(&out);
    assert_eq!(proxy("nabic", &root, None, &["x"]), "nabic 9.10.0: x\n");
    assert_eq!(proxy("nabic", &app, None, &["x"]), "nabic 9.9.1: x\n");
    assert_eq!(
        proxy("nabic", &app, Some("9.10"), &["x"]),
        "nabic 9.10.0: x\n"
    );
    assert_eq!(
        proxy("nabic", &app, Some("9.10"), &["+9.9", "x"]),
        "nabic 9.9.1: x\n"
    );
    // The proxy `nabi` runs the toolchain's `nabi`, which runs its pin's
    // `nabic` with the arguments for it.
    assert_eq!(
        proxy("nabi", &app, None, &["-q", "build", "--release"]),
        "nabic 9.9.1: -q build --release\n"
    );
    assert_eq!(
        proxy("nabi", &root, None, &["+9.9", "-q", "check", "app"]),
        "nabic 9.9.1: -q check app\n"
    );
    // `nabi toolchain` through the proxy is the proxy's own.
    assert_eq!(
        proxy("nabi", &root, None, &["toolchain", "default"]),
        "9.10\n"
    );
    // An unknown pin names where it came from.
    write(&app.join("nabi.nabion"), "Package { toolchain: \"9.8\" }\n");
    let out = Command::new(home.join("bin/nabic"))
        .current_dir(&app)
        .env_remove("NABI_TOOLCHAIN")
        .output()
        .unwrap();
    fails(&out, "toolchain 9.8 (pinned by ");
    // A field the proxy does not know (a newer nabi's) keeps the pin.
    write(
        &app.join("nabi.nabion"),
        "Package { toolchain: \"9.9\", from_a_newer_nabi: true }\n",
    );
    assert_eq!(proxy("nabic", &app, None, &["x"]), "nabic 9.9.1: x\n");

    ok(&tc(&["remove", "9.9.1"]));
    fails(&tc(&["remove", "9.9.1"]), "is not installed");
    assert_eq!(ok(&tc(&["list"])), format!("* 9.10.0-{host} (default)\n"));
    // Pinning this nabi's own version runs the nabic beside it.
    let mine: String = env!("CARGO_PKG_VERSION")
        .split('.')
        .take(2)
        .collect::<Vec<_>>()
        .join(".");
    write(
        &app.join("nabi.nabion"),
        &format!("Package {{ toolchain: \"{mine}\" }}\n"),
    );
    ok(&nabi(&home, &app, &["-q", "check"]));

    // A toolchain's nabi runs its own nabic, never another's: pinned to
    // 9.9, 9.10's nabi is the same build as 9.9's (both copies of this
    // nabi), so it keeps the command and runs its own nabic.
    write(&app.join("nabi.nabion"), "Package { toolchain: \"9.9\" }\n");
    ok(&tc(&["install", "9.9.1", "--from", "fake1"]));
    let own = home.join(format!("toolchains/9.10.0-{host}/bin/nabi"));
    let out = Command::new(&own)
        .current_dir(&app)
        .env("NABI_HOME", &home)
        .env_remove("NABI_TOOLCHAIN")
        .args(["-q", "check"])
        .output()
        .unwrap();
    assert_eq!(ok(&out), "nabic 9.10.0: -q check\n");
}

/// A fake toolchain `version` whose `nabi` is a script that says how it
/// was run: its arguments, directory and handoff guard.
#[cfg(unix)]
pub(crate) fn fake_toolchain_nabi(dir: &Path, version: &str) {
    fake_toolchain(dir, version);
    let nabi = dir.join("bin/nabi");
    fs::remove_file(&nabi).unwrap();
    write(
        &nabi,
        &format!(
            "#!/bin/sh\nif [ \"$1\" = --version ]; then echo 'nabi {version} (fake)'; exit 0; \
             fi\necho \"nabi {version}: $* in $(pwd) home=$NABI_HOME guard=$__NABI_REEXEC\"\n"
        ),
    );
    fs::set_permissions(&nabi, fs::Permissions::from_mode(0o755)).unwrap();
}

/// A nabi run directly (a dev build, nix, a toolchain's `bin/nabi`) in a
/// project pinned to another toolchain runs that toolchain's own `nabi`
/// with the same arguments, from the same directory.
#[cfg(unix)]
#[test]
fn direct_nabi_execs_pinned_toolchain() {
    let root = scratch("toolchain-handoff");
    let home = root.join("home");
    let app = root.join("app");
    let host = env!("NABI_HOST");
    write(&app.join("src/main.nabi"), "println(\"x\");\n");
    write(&app.join("nabi.nabion"), "Package { toolchain: \"9.9\" }\n");
    fake_toolchain_nabi(&root.join("fake1"), "9.9.1");
    ok(&nabi(
        &home,
        &root,
        &["toolchain", "install", "9.9.1", "--from", "fake1"],
    ));
    let tc = nabic_core::real_path(home.join(format!("toolchains/9.9.1-{host}"))).unwrap();
    let app_real = nabic_core::real_path(&app).unwrap();
    let root_real = nabic_core::real_path(&root).unwrap();
    let ran = |args: &str, cwd: &Path| {
        format!(
            "nabi 9.9.1: {args} in {} home={} guard={}\n",
            cwd.display(),
            home.display(),
            tc.display()
        )
    };
    // A command that runs nabic, and one of nabi's own.
    assert_eq!(
        ok(&nabi(&home, &app, &["-q", "build", "--release"])),
        ran("-q build --release", &app_real)
    );
    assert_eq!(ok(&nabi(&home, &app, &["deps"])), ran("deps", &app_real));
    // `--cwd` is applied again by the toolchain's nabi, from where it began.
    assert_eq!(
        ok(&nabi(&home, &root, &["--cwd", "app", "deps"])),
        ran("--cwd app deps", &root_real)
    );
    // A package named by path, `+<version>` and NABI_TOOLCHAIN (a version
    // or a directory name).
    assert_eq!(
        ok(&nabi(&home, &root, &["-q", "check", "app"])),
        ran("-q check app", &root_real)
    );
    assert_eq!(
        ok(&nabi(&home, &root, &["+9.9", "-q", "fmt"])),
        ran("+9.9 -q fmt", &root_real)
    );
    let mut c = nabi_cmd(&home, &root, &["-q", "fmt"]);
    c.env("NABI_TOOLCHAIN", format!("9.9.1-{host}"));
    assert_eq!(ok(&c.output().unwrap()), ran("-q fmt", &root_real));
    // `--version` and `toolchain` stay with this nabi.
    let out = ok(&nabi(&home, &app, &["--version"]));
    assert!(
        out.starts_with(&format!("nabi {}", env!("CARGO_PKG_VERSION"))),
        "{out}"
    );
    assert_eq!(
        ok(&nabi(&home, &app, &["toolchain", "default"])),
        format!("9.9.1-{host}\n")
    );
}

/// The handoff compares toolchain directories, never the pin's text: a
/// pin naming a directory runs that toolchain's nabi once, and a nabi that
/// hands the command back is an error, not a loop.
#[cfg(unix)]
#[test]
fn reexec_guard_no_loop() {
    let root = scratch("toolchain-guard");
    let home = root.join("home");
    let host = env!("NABI_HOST");
    fake_toolchain(&root.join("fake1"), "9.8.0");
    fake_toolchain(&root.join("fake2"), "9.9.1");
    for (v, dir) in [("9.8.0", "fake1"), ("9.9.1", "fake2")] {
        ok(&nabi(
            &home,
            &root,
            &["toolchain", "install", v, "--from", dir],
        ));
    }
    write(&root.join("app/src/main.nabi"), "println(\"x\");\n");
    // The toolchain's own nabi, pinned by its directory name: no handoff.
    let own = home.join(format!("toolchains/9.8.0-{host}/bin/nabi"));
    let out = Command::new(&own)
        .current_dir(&root)
        .env("NABI_HOME", &home)
        .env("NABI_TOOLCHAIN", format!("9.8.0-{host}"))
        .args(["-q", "check", "app"])
        .output()
        .unwrap();
    assert_eq!(ok(&out), "nabic 9.8.0: -q check app\n");
    // A broken toolchain whose `bin/nabi` is another nabi: it would hand
    // the command on again.
    let nabi9 = home.join(format!("toolchains/9.9.1-{host}/bin/nabi"));
    write(
        &nabi9,
        &format!(
            "#!/bin/sh\nif [ \"$1\" = --version ]; then echo 'nabi 9.9.1 (fake)'; exit 0; \
             fi\nexec \"{NABI}\" \"$@\"\n"
        ),
    );
    fs::set_permissions(&nabi9, fs::Permissions::from_mode(0o755)).unwrap();
    let mut c = nabi_cmd(&home, &root, &["-q", "check", "app"]);
    c.env("NABI_TOOLCHAIN", format!("9.9.1-{host}"));
    let out = c.output().unwrap();
    fails(
        &out,
        &format!(
            "ran for toolchain 9.9.1-{host} (NABI_TOOLCHAIN) already and handed the command back"
        ),
    );
    assert_eq!(text(&out.stderr).matches("error").count(), 1);
}

/// Copies a toolchain's `nabi` as `bin/nabi` would be, then makes it a
/// script that runs this nabi, so the copies differ by content.
#[cfg(unix)]
pub(crate) fn proxied_toolchains(name: &str) -> (PathBuf, PathBuf) {
    let root = scratch(name);
    let home = root.join("home");
    fake_toolchain(&root.join("fake1"), "9.9.1");
    fake_toolchain(&root.join("fake2"), "9.10.0");
    // 9.9.1's nabi differs from 9.10.0's (this nabi) by content.
    let old = root.join("fake1/bin/nabi");
    fs::remove_file(&old).unwrap();
    write(&old, &format!("#!/bin/sh\nexec \"{NABI}\" \"$@\"\n"));
    fs::set_permissions(&old, fs::Permissions::from_mode(0o755)).unwrap();
    for (v, dir) in [("9.9.1", "fake1"), ("9.10.0", "fake2")] {
        ok(&nabi(
            &home,
            &root,
            &["toolchain", "install", v, "--from", dir],
        ));
    }
    (root, home)
}

#[cfg(unix)]
pub(crate) fn proxies_are(home: &Path, version: &str) {
    let want = fs::read(home.join(format!(
        "toolchains/{version}-{}/bin/nabi",
        env!("NABI_HOST")
    )))
    .unwrap();
    for t in ["nabi", "nabic", "nabils"] {
        assert!(
            fs::read(home.join("bin").join(t)).unwrap() == want,
            "bin/{t} is not {version}'s nabi"
        );
    }
}

/// The proxies are the newest toolchain's nabi: making an older one the
/// default keeps them.
#[cfg(unix)]
#[test]
fn default_to_older_keeps_newest_proxies() {
    let (root, home) = proxied_toolchains("toolchain-proxies-default");
    // Installing the newer one replaced the first one's proxies.
    proxies_are(&home, "9.10.0");
    ok(&nabi(&home, &root, &["toolchain", "default", "9.10"]));
    ok(&nabi(&home, &root, &["toolchain", "default", "9.9"]));
    proxies_are(&home, "9.10.0");
    // They run the default's tools.
    let out = Command::new(home.join("bin/nabic"))
        .current_dir(&root)
        .env_remove("NABI_HOME")
        .env_remove("NABI_TOOLCHAIN")
        .arg("x")
        .output()
        .unwrap();
    assert_eq!(ok(&out), "nabic 9.9.1: x\n");
    // A missing proxy comes back.
    fs::remove_file(home.join("bin/nabils")).unwrap();
    ok(&nabi(&home, &root, &["toolchain", "default", "9.9"]));
    proxies_are(&home, "9.10.0");
}

/// Removing the newest toolchain makes the proxies the next newest's.
#[cfg(unix)]
#[test]
fn remove_newest_rewrites_proxies() {
    let (root, home) = proxied_toolchains("toolchain-proxies-remove");
    proxies_are(&home, "9.10.0");
    ok(&nabi(&home, &root, &["toolchain", "remove", "9.10.0"]));
    proxies_are(&home, "9.9.1");
}

/// `nabi toolchain default` rewrites the completion scripts the
/// installers wrote from the new default, never makes one, and says
/// which under `-v`; a toolchain without `completions` leaves them.
#[cfg(unix)]
#[test]
fn toolchain_default_refreshes_completions() {
    let root = scratch("toolchain-completions");
    let home = root.join("home");
    let user = home.join("user");
    fake_toolchain(&root.join("fake1"), "9.9.1");
    fake_toolchain(&root.join("fake2"), "9.10.0");
    fake_toolchain(&root.join("fake3"), "9.11.0");
    fake_completions(&root.join("fake1"), Some("9.9.1"));
    fake_completions(&root.join("fake2"), Some("9.10.0"));
    // Too old for `completions`.
    fake_completions(&root.join("fake3"), None);
    let tc = |args: &[&str]| nabi(&home, &root, &[&["toolchain"][..], args].concat());
    for (v, dir) in [("9.9.1", "fake1"), ("9.10.0", "fake2"), ("9.11.0", "fake3")] {
        ok(&tc(&["install", v, "--from", dir]));
    }
    let bash = user.join(".local/share/bash-completion/completions/nabi");
    let zsh = home.join("zsh/_nabi");
    let ps1 = home.join("completions/nabi.ps1");
    for f in [&bash, &zsh, &ps1] {
        write(f, "old\n");
    }
    let absent = [
        user.join(".local/share/bash-completion/completions/nabic"),
        home.join("zsh/_nabic"),
        user.join(".config/fish/completions/nabi.fish"),
        user.join(".config/fish/completions/nabic.fish"),
    ];

    let out = tc(&["default", "9.10"]);
    ok(&out);
    assert!(
        !text(&out.stderr).contains("note:"),
        "{}",
        text(&out.stderr)
    );
    assert_eq!(read(&bash), "nabi 9.10.0: completions bash\n");
    assert_eq!(read(&zsh), "nabi 9.10.0: completions zsh\n");
    assert_eq!(read(&ps1), "nabi 9.10.0: completions powershell\n");
    for f in &absent {
        assert!(!f.exists(), "{}", f.display());
    }

    let out = tc(&["default", "9.9", "-v"]);
    ok(&out);
    let err = text(&out.stderr);
    for want in [
        "        note: refreshed bash completions for nabi in ~/.local/share/bash-completion/completions/nabi\n".to_owned(),
        format!("        note: refreshed zsh completions for nabi in {}\n", zsh.display()),
        format!(
            "        note: refreshed powershell completions for nabi in {}\n",
            ps1.display()
        ),
    ] {
        assert!(err.contains(&want), "lacks {want:?}:\n{err}");
    }
    assert_eq!(err.matches("note:").count(), 3, "{err}");
    assert_eq!(read(&zsh), "nabi 9.9.1: completions zsh\n");

    // `-q` wins over `-v`.
    let out = nabi(&home, &root, &["-q", "-v", "toolchain", "default", "9.10"]);
    assert_eq!(text(&out.stderr), "");
    assert_eq!(read(&zsh), "nabi 9.10.0: completions zsh\n");

    let before: Vec<String> = [&bash, &zsh, &ps1].iter().map(|f| read(f)).collect();
    let out = nabi(&home, &root, &["-v", "toolchain", "default", "9.11"]);
    ok(&out);
    assert!(
        !text(&out.stderr).contains("note:"),
        "{}",
        text(&out.stderr)
    );
    let after: Vec<String> = [&bash, &zsh, &ps1].iter().map(|f| read(f)).collect();
    assert_eq!(before, after);
    for f in &absent {
        assert!(!f.exists(), "{}", f.display());
    }
}

/// `nabi toolchain update` against a local release server: the newest
/// release (`latest/download/nabi-version`) or a given one, checked
/// against its signed `SHA256SUMS`, becomes the default with new proxies; a
/// toolchain whose proxies do not run is rolled back.
#[cfg(unix)]
#[test]
fn update_toolchain() {
    let root = scratch("update-toolchain");
    let home = root.join("home");
    let host = env!("NABI_HOST");
    let dist = root.join("dist");
    let release = |v: &str, sum: Option<&str>, broken_nabi: bool| {
        let stage = root.join("stage").join(v);
        let top = stage.join(format!("nabi-{v}"));
        fake_toolchain(&top, v);
        if broken_nabi {
            let nabi = top.join("bin/nabi");
            write(&nabi, "#!/bin/sh\nexit 1\n");
            fs::set_permissions(&nabi, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let name = format!("nabi-{v}-{host}.tar.gz");
        let to = dist.join(format!("download/v{v}"));
        fs::create_dir_all(&to).unwrap();
        let tar = Command::new("tar")
            .current_dir(&stage)
            .arg("-czf")
            .arg(to.join(&name))
            .arg(format!("nabi-{v}"))
            .status()
            .unwrap();
        assert!(tar.success());
        sign_release(&to, sum);
        write(
            &dist.join("latest/download/nabi-version"),
            &format!("{v}\n"),
        );
    };
    let url = format!("http://127.0.0.1:{}", serve_dir(dist.clone()));
    let key = test_key(&root);
    let warn = key_warning(&key);
    let update = |args: &[&str]| {
        let args = keyed(&[&["update"][..], args].concat(), &key);
        let mut c = nabi_cmd(&home, &root, &[&["toolchain"][..], &args].concat());
        c.env("NABI_DIST", &url);
        c.output().unwrap()
    };
    let proxy = |args: &[&str]| {
        let mut c = Command::new(home.join("bin/nabic"));
        c.current_dir(&root)
            .env_remove("NABI_HOME")
            .env_remove("NABI_TOOLCHAIN")
            .args(args);
        ok(&c.output().unwrap())
    };
    let list = || ok(&nabi(&home, &root, &["toolchain", "list"]));

    fails(&update(&[]), "latest/download/nabi-version");
    release("9.9.1", None, false);
    let out = update(&[]);
    ok(&out);
    let err = text(&out.stderr);
    assert!(
        err.starts_with(&format!(
            " Downloading nabi 9.9.1 ({host})\n{warn}   Installed nabi 9.9.1 to "
        )),
        "{err}"
    );
    assert_eq!(list(), format!("* 9.9.1-{host} (default)\n"));
    assert_eq!(proxy(&["x"]), "nabic 9.9.1: x\n");
    let out = update(&[]);
    assert_eq!(text(&out.stderr), "nabi 9.9.1 is up to date\n");

    // Through the proxy, which updates itself and, quietly, the
    // completion scripts there are.
    release("9.10.0", None, false);
    let zsh = home.join("zsh/_nabi");
    write(&zsh, "old\n");
    let mut c = Command::new(home.join("bin/nabi"));
    user_env(&mut c, &home.join("user"));
    proxies_first(&mut c, &home);
    c.current_dir(&root)
        .env_remove("NABI_HOME")
        .env_remove("NABI_TOOLCHAIN")
        .env("NABI_DIST", &url)
        .args(["toolchain", "update", "--key", &key]);
    let out = c.output().unwrap();
    ok(&out);
    assert_eq!(
        text(&out.stderr),
        format!(
            " Downloading nabi 9.10.0 ({host})\n{warn}     Updated nabi 9.9.1 -> 9.10.0\n        note: \
             9.9.1-{host} stays installed (`nabi toolchain remove 9.9.1-{host}`)\n"
        )
    );
    assert_eq!(
        list(),
        format!("  9.9.1-{host}\n* 9.10.0-{host} (default)\n")
    );
    assert_eq!(proxy(&["x"]), "nabic 9.10.0: x\n");
    assert!(read(&zsh).starts_with("#compdef nabi"), "{}", read(&zsh));

    // A damaged download is refused before anything changes.
    release("9.11.0", Some(&"0".repeat(64)), false);
    fails(&update(&[]), "but the signed SHA256SUMS says 0000");
    // Proxies that do not run the new toolchain are rolled back.
    release("9.12.0", None, true);
    fails(
        &update(&[]),
        "rolled back: the default toolchain and the proxies are as they were",
    );
    assert_eq!(
        list(),
        format!("  9.9.1-{host}\n* 9.10.0-{host} (default)\n")
    );
    assert_eq!(proxy(&["x"]), "nabic 9.10.0: x\n");
    let leftovers: Vec<_> = fs::read_dir(home.join("bin"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .filter(|n| n.to_string_lossy().starts_with('.'))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");

    // A given version: the default already, or installed (no download).
    let out = update(&["9.10.0"]);
    assert_eq!(text(&out.stderr), "nabi 9.10.0 is up to date\n");
    fails(
        &update(&["greet"]),
        "nabi toolchain update: `greet` is not a full version",
    );
    fs::remove_dir_all(dist.join("download/v9.9.1")).unwrap();
    write(&zsh, "old\n");
    let out = update(&["9.9.1", "-v"]);
    ok(&out);
    let err = text(&out.stderr);
    assert!(
        err.starts_with("     Updated nabi 9.10.0 -> 9.9.1\n"),
        "{err}"
    );
    assert!(
        err.ends_with(&format!(
            "        note: refreshed zsh completions for nabi in {}\n",
            zsh.display()
        )),
        "{err}"
    );
    assert!(read(&zsh).starts_with("#compdef nabi"), "{}", read(&zsh));
    assert_eq!(
        list(),
        format!("* 9.9.1-{host} (default)\n  9.10.0-{host}\n")
    );
    assert_eq!(proxy(&["x"]), "nabic 9.9.1: x\n");
    assert_eq!(update(&["9.9.1", "x"]).status.code(), Some(2));
    assert_eq!(update(&["--from", "x"]).status.code(), Some(2));
    // `clean` removes every installed toolchain but the default.
    assert_eq!(
        list(),
        format!("* 9.9.1-{host} (default)\n  9.10.0-{host}\n")
    );
    ok(&nabi(&home, &root, &["toolchain", "clean"]));
    assert_eq!(list(), format!("* 9.9.1-{host} (default)\n"));
    assert_eq!(proxy(&["x"]), "nabic 9.9.1: x\n");
    // Idempotent: nothing left to remove.
    let out = nabi_cmd(&home, &root, &["toolchain", "clean"])
        .output()
        .unwrap();
    ok(&out);
    assert!(
        text(&out.stderr).contains("nothing to clean"),
        "{}",
        text(&out.stderr)
    );
    for gone in ["self", "upgrade"] {
        fails(
            &nabi(&home, &root, &[gone]),
            &format!("unknown command `{gone}`"),
        );
    }
}

/// A release server in `root/dist` with fake releases `versions` for
/// this host; its URL.
#[cfg(unix)]
pub(crate) fn fake_dist(root: &Path, versions: &[&str]) -> String {
    let host = env!("NABI_HOST");
    let dist = root.join("dist");
    for v in versions {
        let stage = root.join("stage").join(v);
        fake_toolchain(&stage.join(format!("nabi-{v}")), v);
        let name = format!("nabi-{v}-{host}.tar.gz");
        let to = dist.join(format!("download/v{v}"));
        fs::create_dir_all(&to).unwrap();
        let tar = Command::new("tar")
            .current_dir(&stage)
            .arg("-czf")
            .arg(to.join(&name))
            .arg(format!("nabi-{v}"))
            .status()
            .unwrap();
        assert!(tar.success());
        sign_release(&to, None);
    }
    format!("http://127.0.0.1:{}", serve_dir(dist))
}

/// A bare update installs the newest release, also from another series,
/// and says the old series gets no more fixes. An argument is an exact version;
/// a series or a requirement is an error that says to write one.
#[cfg(unix)]
#[test]
fn update_newest_and_exact() {
    let root = scratch("update-exact");
    let home = root.join("home");
    let host = env!("NABI_HOST");
    let url = fake_dist(&root, &["0.90.1", "0.90.2", "0.91.0"]);
    let dist = root.join("dist");
    let key = test_key(&root);
    let tc = |args: &[&str]| {
        let mut c = nabi_cmd(
            &home,
            &root,
            &[&["toolchain"][..], &keyed(args, &key)].concat(),
        );
        c.env("NABI_DIST", &url);
        c.output().unwrap()
    };
    let default = || {
        let list = ok(&nabi(&home, &root, &["toolchain", "list"]));
        list.lines()
            .find_map(|l| l.strip_prefix("* ")?.strip_suffix(" (default)"))
            .map(str::to_owned)
    };
    write(&dist.join("latest/download/nabi-version"), "0.91.0\n");

    // An exact version.
    ok(&tc(&["update", "0.90.1"]));
    assert_eq!(default(), Some(format!("0.90.1-{host}")));
    let err = text(&tc(&["update", "0.90.2"]).stderr);
    assert!(err.contains("Updated nabi 0.90.1 -> 0.90.2\n"), "{err}");
    assert!(
        err.ends_with(&format!(
            "note: 0.90.1-{host} stays installed (`nabi toolchain remove 0.90.1-{host}`)\n"
        )),
        "{err}"
    );
    assert_eq!(default(), Some(format!("0.90.2-{host}")));

    // A series, a requirement or a short version is not a release.
    for bad in ["0.90", "^0.90.2", "0", "latest"] {
        fails(
            &tc(&["update", bad]),
            &format!(
                "nabi toolchain update: `{bad}` is not a full version; write one like `0.1.0`, \
                 or nothing for the newest release"
            ),
        );
    }
    assert_eq!(default(), Some(format!("0.90.2-{host}")));

    // A bare update crosses into the newest series, keeping the old one.
    let out = tc(&["update"]);
    ok(&out);
    let err = text(&out.stderr);
    assert!(err.contains("Updated nabi 0.90.2 -> 0.91.0\n"), "{err}");
    assert!(
        err.ends_with(
            "note: nabi 0.90.2 stays installed, but 0.90 gets no more fixes; move projects to \
             0.91.0. Until a project moves it can pin 0.90 (`toolchain: \"0.90\"` in \
             nabi.nabion or a script's `//! nabion` block) or run `nabi +0.90 ..`\n"
        ),
        "{err}"
    );
    assert_eq!(default(), Some(format!("0.91.0-{host}")));
    assert!(home.join(format!("toolchains/0.90.2-{host}")).is_dir());
    assert_eq!(text(&tc(&["update"]).stderr), "nabi 0.91.0 is up to date\n");
    // A project pinned to the old series keeps it.
    let app = root.join("app");
    write(
        &app.join("nabi.nabion"),
        "Package { toolchain: \"0.90\" }\n",
    );
    let mut c = Command::new(home.join("bin/nabic"));
    c.current_dir(&app)
        .env_remove("NABI_HOME")
        .env_remove("NABI_TOOLCHAIN")
        .arg("x");
    assert_eq!(ok(&c.output().unwrap()), "nabic 0.90.2: x\n");
}

/// `nabi toolchain install <version>` without `--from` downloads a
/// release by exact version and keeps the default.
#[cfg(unix)]
#[test]
fn install_by_version_without_from() {
    let root = scratch("install-version");
    let home = root.join("home");
    let host = env!("NABI_HOST");
    let url = fake_dist(&root, &["0.90.1", "0.90.2"]);
    let key = test_key(&root);
    let tc = |args: &[&str]| {
        let mut c = nabi_cmd(
            &home,
            &root,
            &[&["toolchain"][..], &keyed(args, &key)].concat(),
        );
        c.env("NABI_DIST", &url);
        c.output().unwrap()
    };
    let out = tc(&["install", "0.90.1"]);
    ok(&out);
    assert!(
        text(&out.stderr).contains(&format!("Installed toolchain 0.90.1-{host}\n")),
        "{}",
        text(&out.stderr)
    );
    // The first one became the default.
    ok(&tc(&["install", "0.90.2"]));
    assert_eq!(
        ok(&tc(&["list"])),
        format!("* 0.90.1-{host} (default)\n  0.90.2-{host}\n")
    );
    assert_eq!(
        text(&tc(&["install", "0.90.2"]).stderr),
        format!("toolchain 0.90.2-{host} is already installed\n")
    );
    // The proxies are the newest's; the default still runs.
    let mut c = Command::new(home.join("bin/nabic"));
    c.current_dir(&root)
        .env_remove("NABI_HOME")
        .env_remove("NABI_TOOLCHAIN")
        .arg("x");
    assert_eq!(ok(&c.output().unwrap()), "nabic 0.90.1: x\n");
    fails(
        &tc(&["install", "0.92"]),
        "nabi toolchain install: `0.92` is not a full version; write one like `0.1.0`",
    );
}

/// A release candidate is installed only by name: a bare update takes
/// latest/'s final release, `update X.Y.Z-rc.N` and `install` the rc.
#[cfg(unix)]
#[test]
fn release_candidate_by_name_only() {
    let root = scratch("toolchain-rc");
    let home = root.join("home");
    let host = env!("NABI_HOST");
    let url = fake_dist(&root, &["0.90.1", "0.91.0-rc.1"]);
    write(&root.join("dist/latest/download/nabi-version"), "0.90.1\n");
    let key = test_key(&root);
    let tc = |args: &[&str]| {
        let mut c = nabi_cmd(
            &home,
            &root,
            &[&["toolchain"][..], &keyed(args, &key)].concat(),
        );
        c.env("NABI_DIST", &url);
        c.output().unwrap()
    };
    ok(&tc(&["update"]));
    assert_eq!(ok(&tc(&["list"])), format!("* 0.90.1-{host} (default)\n"));
    let err = text(&tc(&["update"]).stderr);
    assert!(err.ends_with("nabi 0.90.1 is up to date\n"), "{err}");
    ok(&tc(&["install", "0.91.0-rc.1"]));
    assert_eq!(
        ok(&tc(&["list"])),
        format!("* 0.90.1-{host} (default)\n  0.91.0-rc.1-{host}\n")
    );
    ok(&tc(&["update", "0.91.0-rc.1"]));
    assert_eq!(
        ok(&tc(&["list"])),
        format!("  0.90.1-{host}\n* 0.91.0-rc.1-{host} (default)\n")
    );
    fails(
        &tc(&["install", "0.91.0-rc.2"]),
        "/download/v0.91.0-rc.2/nabi-0.91.0-rc.2-",
    );
}

/// A pin is a version prefix, a requirement (`^`, `~`, `=`) or a
/// directory name; the newest installed match wins. A pin that is none
/// of them warns, and says what to write.
#[cfg(unix)]
#[test]
fn pin_requirement_caret() {
    let root = scratch("pin-req");
    let home = root.join("home");
    let host = env!("NABI_HOST");
    let app = root.join("app");
    write(&app.join("src/main.nabi"), "println(\"x\");\n");
    fake_toolchain(&root.join("a"), "9.9.1");
    fake_toolchain(&root.join("b"), "9.9.4");
    fake_toolchain(&root.join("c"), "9.10.0");
    for (v, from) in [("9.9.1", "a"), ("9.9.4", "b"), ("9.10.0", "c")] {
        ok(&nabi(
            &home,
            &root,
            &["toolchain", "install", v, "--from", from],
        ));
    }
    let nabic = |pin: &str| {
        write(
            &app.join("nabi.nabion"),
            &format!("Package {{ toolchain: \"{pin}\" }}\n"),
        );
        let mut c = Command::new(home.join("bin/nabic"));
        c.current_dir(&app)
            .env_remove("NABI_HOME")
            .env_remove("NABI_TOOLCHAIN")
            .arg("x");
        c.output().unwrap()
    };
    for (pin, runs) in [
        ("9.9", "9.9.4"),
        ("^9.9.1", "9.10.0"),
        ("~9.9.1", "9.9.4"),
        ("=9.9.1", "9.9.1"),
        (&format!("9.9.1-{host}"), "9.9.1"),
    ] {
        assert_eq!(ok(&nabic(pin)), format!("nabic {runs}: x\n"), "{pin}");
    }
    fails(&nabic("^9.11"), "toolchain ^9.11 (pinned by ");
    fails(
        &nabic("^9.11"),
        "install a release it accepts by its full version, `nabi toolchain install <version>` (or `nabi toolchain install ^9.11 --from",
    );
}

#[test]
fn bad_pin_warns() {
    let root = scratch("bad-pin");
    let home = root.join("home");
    write(&root.join("src/main.nabi"), "println(\"x\");\n");
    write(
        &root.join("nabi.nabion"),
        "Package { name: \"app\", toolchain: \"banana\", deps: {} }\n",
    );
    // nabi's own commands go on, with a warning.
    let out = nabi(&home, &root, &["deps"]);
    ok(&out);
    let err = text(&out.stderr);
    assert!(
        err.contains(
            "nabi.nabion:1:35: `toolchain: \"banana\"`: write a version (`0.1`), a requirement \
             (`^0.1.0`) or a toolchain directory name"
        ),
        "{err}"
    );
    // A build needs the toolchain it names.
    fails(
        &nabi(&home, &root, &["check"]),
        "toolchain banana (pinned by ",
    );
    fails(
        &nabi(&home, &root, &["check"]),
        "`banana` is not a toolchain pin; write a version (`0.1`)",
    );
}

/// A script's `//! ```nabion` block may pin a toolchain: the proxy and a
/// nabi run directly both run the pinned toolchain's nabi. Only the script
/// that is run pins; a module's block may not.
#[cfg(unix)]
#[test]
fn script_block_pins_toolchain() {
    let root = scratch("script-pin");
    let home = root.join("home");
    let host = env!("NABI_HOST");
    // 9.10.0 (a copy of this nabi, so the proxies too) is the default.
    fake_toolchain(&root.join("new"), "9.10.0");
    fake_toolchain_nabi(&root.join("old"), "9.9.1");
    for (v, from) in [("9.10.0", "new"), ("9.9.1", "old")] {
        ok(&nabi(
            &home,
            &root,
            &["toolchain", "install", v, "--from", from],
        ));
    }
    let dir = root.join("s");
    let block = "//! ```nabion\n//! { toolchain: \"9.9\" }\n//! ```\n";
    write(&dir.join("s.nabi"), &format!("{block}println(\"x\");\n"));
    let real = nabic_core::real_path(&dir).unwrap();
    let tc = nabic_core::real_path(home.join(format!("toolchains/9.9.1-{host}"))).unwrap();
    let mut c = Command::new(home.join("bin/nabi"));
    c.current_dir(&dir)
        .env_remove("NABI_HOME")
        .env_remove("NABI_TOOLCHAIN");
    for args in [
        &["run", "s.nabi"][..],
        &["s.nabi"],
        &["-q", "check", "s.nabi"],
    ] {
        let out = ok(&Command::new(home.join("bin/nabi"))
            .current_dir(&dir)
            .env_remove("NABI_HOME")
            .env_remove("NABI_TOOLCHAIN")
            .args(args)
            .output()
            .unwrap());
        assert!(
            out.starts_with(&format!(
                "nabi 9.9.1: {} in {}",
                args.join(" "),
                real.display()
            )),
            "{args:?}: {out}"
        );
    }
    // Run directly, this nabi hands the script to the pinned one.
    assert_eq!(
        ok(&nabi(&home, &dir, &["s.nabi"])),
        format!(
            "nabi 9.9.1: s.nabi in {} home={} guard={}\n",
            real.display(),
            home.display(),
            tc.display()
        )
    );
    // `+<version>` still wins.
    let out = ok(&c.args(["+9.10", "-q", "check", "s.nabi"]).output().unwrap());
    assert!(out.starts_with("nabic 9.10.0: "), "{out}");
    // A module's block may not pin.
    let mine = series();
    write(&dir.join("t.nabi"), "mod m;\nprintln(\"x\");\n");
    write(
        &dir.join("m.nabi"),
        &format!("//! ```nabion\n//! {{ toolchain: \"{mine}\" }}\n//! ```\npub fn f() {{}}\n"),
    );
    fails(
        &nabi(&home, &dir, &["check", "t.nabi"]),
        "m.nabi (//! nabion block): `toolchain` goes in the block of t.nabi, the script that is run; \
         remove it here",
    );
    // Other fields are still refused.
    write(
        &dir.join("u.nabi"),
        "//! ```nabion\n//! { name: \"u\" }\n//! ```\n",
    );
    fails(
        &nabi(&home, &dir, &["check", "u.nabi"]),
        "a script block holds only `deps`, `native` and `toolchain`",
    );
}
