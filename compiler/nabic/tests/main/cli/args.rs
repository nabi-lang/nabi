//! Program arguments and environment variables.

#[allow(clippy::wildcard_imports)]
use crate::cli::*;

// `env::args` / `env::vars` replace bytes that are not UTF-8 with U+FFFD;
// `args_os` keeps them.
#[cfg(unix)]
#[test]
fn non_utf8_args_and_vars_are_lossy() {
    let env = setup();
    let dir = scratch("anon_lossy_args");
    let case = workspace().join("tests/cases/std_args_lossy.nabi");
    fs::copy(case, dir.join("lossy.nabi")).unwrap();
    let out = Command::new(NABIC)
        .current_dir(&dir)
        .env("NABI_HOME", workspace())
        .args(["build", "-q", "lossy.nabi", "--nabir"])
        .arg(&env.nabir)
        .output()
        .unwrap();
    assert_exit(&out, 0);
    let out = Command::new(dir.join(exe_name("lossy")))
        .env("NABI_LOSSY", OsStr::from_bytes(b"v\xffw"))
        .arg("a")
        .arg(OsStr::from_bytes(b"b\xff\xfec"))
        .arg(OsStr::from_bytes(b"\xe2\x82"))
        .output()
        .unwrap();
    assert_exit(&out, 0);
    assert_eq!(
        text(&out.stdout),
        "args: 4\n\
         1: a [61] 1 bytes, lossy true\n\
         2: b\u{fffd}\u{fffd}c [62 fffd fffd 63] 4 bytes, lossy true\n\
         3: \u{fffd} [fffd] 2 bytes, lossy true\n\
         var: v\u{fffd}w\n"
    );
}

// Windows: a lone surrogate (WTF-8 in `args_os`) is one U+FFFD.
#[cfg(windows)]
#[test]
fn lone_surrogate_args_and_vars_are_lossy() {
    let env = setup();
    let dir = scratch("anon_lossy_args");
    let case = workspace().join("tests/cases/std_args_lossy.nabi");
    fs::copy(case, dir.join("lossy.nabi")).unwrap();
    let out = Command::new(NABIC)
        .current_dir(&dir)
        .env("NABI_HOME", workspace())
        .args(["build", "-q", "lossy.nabi", "--nabir"])
        .arg(&env.nabir)
        .output()
        .unwrap();
    assert_exit(&out, 0);
    let out = Command::new(dir.join(exe_name("lossy")))
        .env("NABI_LOSSY", OsString::from_wide(&[0x76, 0xDC00, 0x77]))
        .arg("a")
        .arg(OsString::from_wide(&[0x62, 0xD800, 0x63]))
        .output()
        .unwrap();
    assert_exit(&out, 0);
    assert_eq!(
        text(&out.stdout),
        "args: 3\n\
         1: a [61] 1 bytes, lossy true\n\
         2: b\u{fffd}c [62 fffd 63] 5 bytes, lossy true\n\
         var: v\u{fffd}w\n"
    );
}

// Options, commands and paths are UTF-8; the error says where bytes may go.
#[test]
fn non_utf8_option_is_usage_error() {
    let dir = scratch("anon_os_opt");
    #[cfg(unix)]
    let bad = std::ffi::OsString::from_vec(b"b\xffc".to_vec());
    #[cfg(windows)]
    let bad = std::ffi::OsString::from_wide(&[0x62, 0xD800, 0x63]);
    let out = Command::new(NABIC)
        .current_dir(&dir)
        .env("NABI_HOME", workspace())
        .args(["build", "-q"])
        .arg(&bad)
        .output()
        .unwrap();
    assert_exit(&out, 2);
    let err = text(&out.stderr);
    assert!(
        err.contains("argument 3 (`b\u{fffd}c`) is not valid UTF-8; only a program's arguments, after `--`, may be other bytes"),
        "{err}"
    );
}

/// `nabic build` progress output, the diagnostic knobs and `emit`.
#[test]
fn build_progress_and_knobs() {
    let env = setup();
    let dir = scratch("build-progress");
    let app = new_package(&dir, "app", false);
    let out = Command::new(NABIC)
        .current_dir(&app)
        .env("NABI_HOME", workspace())
        .env("NABI_VERBOSE", "1")
        .env("NABI_TIME_PASSES", "1")
        .args(["build", "--nabir"])
        .arg(&env.nabir)
        .output()
        .unwrap();
    assert_exit(&out, 0);
    let err = text(&out.stderr);
    for want in [
        "   Compiling app",
        "    Building ",
        "   Stage: front end (",
        "time: ",
        " parse\n",
        "    Finished in ",
    ] {
        assert!(err.contains(want), "{want:?}: {err}");
    }

    let out = nabic(None, &app, &["emit"]);
    assert_exit(&out, 0);
    assert!(text(&out.stderr).contains("       Wrote "));
    assert!(app.join("target/debug/build/app.mir").is_file());
    assert_exit(
        &nabic(None, &app, &["emit", "--emit=hir,mir", "-o", "x"]),
        2,
    );
    // An unknown kind (`llvmir`): the error says what there is.
    let out = nabic(None, &app, &["emit", "--emit=llvmir"]);
    assert_exit(&out, 2);
    assert!(text(&out.stderr).contains("unknown emit kind `llvmir`"));
    let out = nabic(None, &app, &["help", "build"]);
    assert_exit(&out, 0);
    assert!(text(&out.stdout).contains("build [path]"));

    put(dir.join("x.no"), "println(\"no\");\n").unwrap();
    let out = nabic(None, &dir, &["build", "x.no"]);
    assert_exit(&out, 1);
    assert!(
        text(&out.stderr).contains("is not a Nabi source file"),
        "{}",
        text(&out.stderr)
    );
}

/// A compiler knob set to a value outside its accepted
/// ones warns once on stderr and keeps its default.
#[test]
fn env_knob_bad_value_warns() {
    let env = setup();
    let dir = scratch("env-knob");
    let lib = new_package(&dir, "knobs", true);
    let out = Command::new(NABIC)
        .current_dir(&lib)
        .env("NABI_HOME", workspace())
        .env("NABI_VERBOSE", "yes")
        .env("NABI_TIME_PASSES", "on")
        .env("NABI_INCREMENTAL_VERIFY", "true")
        .args(["test", "-q", "--nabir"])
        .arg(&env.nabir)
        .output()
        .unwrap();
    assert_exit(&out, 0);
    let err = text(&out.stderr);
    let warnings: Vec<&str> = err
        .lines()
        .filter(|l| l.starts_with("nabic: warning: "))
        .collect();
    assert_eq!(
        warnings,
        [
            "nabic: warning: ignoring NABI_VERBOSE=yes (expected 0|1)",
            "nabic: warning: ignoring NABI_TIME_PASSES=on (expected 0|1)",
            "nabic: warning: ignoring NABI_INCREMENTAL_VERIFY=true (expected 0|1)",
        ]
    );
}
