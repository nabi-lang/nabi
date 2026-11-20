//! Lone files: anonymous packages and the executable cache.

#[allow(clippy::wildcard_imports)]
use crate::cli::*;

// The day's eviction of the shared lone-file cache runs inside a build:
// it must not take the directory that build just made, and its stamp
// must move (Windows keeps an empty file's time when it is rewritten).
#[test]
fn lone_file_survives_the_cache_eviction() {
    let env = setup();
    let dir = scratch("anon_evict");
    let tmp = dir.join("tmp");
    let stamp = lone_cache(&tmp).join(".evicted");
    put(&stamp, "").unwrap();
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3 * 86_400);
    fs::File::options()
        .write(true)
        .open(&stamp)
        .unwrap()
        .set_modified(old)
        .unwrap();
    put(dir.join("hello.nabi"), "println(\"hi\");\n").unwrap();
    for _ in 0..2 {
        let out = Command::new(NABIC)
            .current_dir(&dir)
            .env("NABI_HOME", workspace())
            .env("TMPDIR", &tmp)
            .env("TMP", &tmp)
            .env("TEMP", &tmp)
            .args(["build", "hello.nabi", "--nabir"])
            .arg(&env.nabir)
            .output()
            .unwrap();
        assert_exit(&out, 0);
        let ran = Command::new(dir.join(exe_name("hello"))).output().unwrap();
        assert_eq!(text(&ran.stdout), "hi\n");
    }
    let age = fs::metadata(&stamp)
        .unwrap()
        .modified()
        .unwrap()
        .elapsed()
        .unwrap();
    assert!(age.as_secs() < 3600, "stamp not renewed: {age:?}");
}

/// The lone-file cache under temp directory `tmp`, made as nabic makes it.
pub(crate) fn lone_cache(tmp: &Path) -> PathBuf {
    fs::create_dir_all(tmp).unwrap();
    #[cfg(unix)]
    {
        let uid = fs::metadata(tmp).unwrap().uid();
        let d = tmp.join(format!("nabic-target-{uid}"));
        fs::DirBuilder::new().mode(0o700).create(&d).unwrap();
        d
    }
    #[cfg(not(unix))]
    {
        let d = tmp.join("nabic-target");
        fs::create_dir_all(&d).unwrap();
        d
    }
}

/// `nabic <args>` in `dir` with `tmp` as the temp directory.
#[cfg(unix)]
pub(crate) fn nabic_in_tmp(env: &Env, dir: &Path, tmp: &Path, args: &[&str]) -> Output {
    Command::new(NABIC)
        .current_dir(dir)
        .env("NABI_HOME", workspace())
        .env_remove("NABI_TARGET_DIR")
        .env("TMPDIR", tmp)
        .env("TMP", tmp)
        .env("TEMP", tmp)
        .args(args)
        .arg("--nabir")
        .arg(&env.nabir)
        .output()
        .unwrap()
}

// The lone-file cache runs what it finds: it is the user's alone.
#[cfg(unix)]
#[test]
fn lone_cache_is_private() {
    let env = setup();
    let dir = scratch("anon_private");
    let tmp = dir.join("tmp");
    fs::create_dir_all(&tmp).unwrap();
    put(dir.join("hello.nabi"), "println(\"hi\");\n").unwrap();
    let out = nabic_in_tmp(&env, &dir, &tmp, &["build", "-q", "hello.nabi"]);
    assert_exit(&out, 0);
    let uid = fs::metadata(&dir).unwrap().uid();
    let m = fs::symlink_metadata(tmp.join(format!("nabic-target-{uid}"))).unwrap();
    assert!(m.is_dir());
    assert_eq!(m.uid(), uid);
    assert_eq!(m.permissions().mode() & 0o777, 0o700);
}

#[cfg(unix)]
#[test]
fn lone_cache_refuses_foreign_dir() {
    let env = setup();
    let dir = scratch("anon_foreign");
    let tmp = dir.join("tmp");
    put(dir.join("hello.nabi"), "println(\"hi\");\n").unwrap();
    let cache = lone_cache(&tmp);
    fs::set_permissions(&cache, fs::Permissions::from_mode(0o755)).unwrap();
    let out = nabic_in_tmp(&env, &dir, &tmp, &["build", "-q", "hello.nabi"]);
    assert_exit(&out, 1);
    let uid = fs::metadata(&dir).unwrap().uid();
    let err = text(&out.stderr);
    assert!(
        err.contains(&format!(
            "{} is not private to you (owner uid {uid}, mode 0755); remove it or set \
             NABI_TARGET_DIR",
            cache.display()
        )),
        "{err}"
    );
    assert_eq!(text(&out.stdout), "");
    // A link to a directory of the user's is refused too.
    fs::remove_dir(&cache).unwrap();
    let real = lone_cache(&dir.join("elsewhere"));
    std::os::unix::fs::symlink(&real, &cache).unwrap();
    let out = nabic_in_tmp(&env, &dir, &tmp, &["build", "-q", "hello.nabi"]);
    assert_exit(&out, 1);
    assert!(
        text(&out.stderr).contains("(a symbolic link)"),
        "{}",
        text(&out.stderr)
    );
    // `NABI_TARGET_DIR` is the way out.
    let out = Command::new(NABIC)
        .current_dir(&dir)
        .env("NABI_HOME", workspace())
        .env("NABI_TARGET_DIR", dir.join("t"))
        .env("TMPDIR", &tmp)
        .args(["build", "-q", "hello.nabi", "--nabir"])
        .arg(&env.nabir)
        .output()
        .unwrap();
    assert_exit(&out, 0);
}

#[test]
fn lone_file_is_an_anonymous_package() {
    let env = setup();
    let dir = scratch("anon");
    put(dir.join("hello.nabi"), "println(\"hi\");\n").unwrap();
    let out = build_run(Some(&env), &dir, &["hello.nabi"]);
    assert_exit(&out, 0);
    assert_eq!(text(&out.stdout), "hi\n");
    // A lone file leaves no target/ behind; `build` puts the executable
    // beside it.
    assert!(!dir.join("target").exists());
    assert_exit(&nabic(Some(&env), &dir, &["build", "hello.nabi"]), 0);
    assert!(!dir.join("target").exists());
    assert!(dir.join(exe_name("hello")).is_file());

    // `mod m;` loads m.nabi beside the file, as in a
    // package's src/, and a change to it is a new build.
    put(dir.join("m.nabi"), "pub fn f() { println(\"m\"); }\n").unwrap();
    put(dir.join("sub.nabi"), "mod m;\nm::f();\n").unwrap();
    let out = build_run(Some(&env), &dir, &["sub.nabi"]);
    assert_exit(&out, 0);
    assert_eq!(text(&out.stdout), "m\n");
    put(dir.join("m.nabi"), "pub fn f() { println(\"m2\"); }\n").unwrap();
    let out = build_run(Some(&env), &dir, &["sub.nabi"]);
    assert_eq!(text(&out.stdout), "m2\n");
    put(dir.join("sub.nabi"), "mod gone;\n").unwrap();
    let out = nabic(Some(&env), &dir, &["build", "sub.nabi"]);
    assert_exit(&out, 1);
    assert!(
        text(&out.stderr).contains("module `gone` body not found"),
        "{}",
        text(&out.stderr)
    );

    // A directory without src/main.nabi / src/lib.nabi is not a package.
    assert_exit(&nabic(Some(&env), &dir, &["build"]), 1);
}

/// A lone file builds beside itself, over its own earlier build but
/// never over another file.
#[test]
fn lone_file_build_output() {
    let env = setup();
    let dir = scratch("lone-output");
    put(dir.join("hi.nabi"), "println(\"hi\");\n").unwrap();
    let hi = exe_name("hi");
    for _ in 0..2 {
        let out = nabic(Some(&env), &dir, &["build", "hi.nabi"]);
        assert_exit(&out, 0);
        let err = text(&out.stderr);
        assert!(
            err.ends_with(&format!("       Built {hi} (use -o to put it elsewhere)\n")),
            "{err}"
        );
    }
    let out = Command::new(dir.join(&hi)).output().unwrap();
    assert_eq!(text(&out.stdout), "hi\n");

    put(dir.join("ho.nabi"), "println(\"ho\");\n").unwrap();
    let ho = dir.join(exe_name("ho"));
    put(&ho, "mine").unwrap();
    let out = nabic(Some(&env), &dir, &["build", "ho.nabi"]);
    assert_exit(&out, 1);
    let err = text(&out.stderr);
    assert!(err.contains("build ho.nabi -o <file>`"), "{err}");
    assert_eq!(fs::read_to_string(&ho).unwrap(), "mine");
    let other = dir.join(exe_name("other"));
    let out = nabic(
        Some(&env),
        &dir,
        &["build", "ho.nabi", "-o", other.to_str().unwrap()],
    );
    assert_exit(&out, 0);
    assert!(other.is_file());
}
