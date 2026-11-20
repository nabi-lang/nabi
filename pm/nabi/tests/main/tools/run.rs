//! `nabi run`: build, then run, the program's arguments and exit code
//! passed through.

#[allow(clippy::wildcard_imports)]
use crate::tools::*;

/// A package `nabi run` builds: `dir/app/src/main.nabi` holding `src`.
fn app(dir: &Path, src: &str) -> PathBuf {
    let app = dir.join("app");
    put(app.join("src/main.nabi"), src).unwrap();
    app
}

/// Exit codes, a signal, argument passing and progress output; `clean`.
#[test]
fn run_passes_arguments_codes_and_progress() {
    let env = setup();
    let dir = scratch("run-codes");
    let src = "use std::env;\n\
               extern \"C\" { fn raise(sig: i32) -> i32; }\n\
               fn main() -> i32 {\n\
               \x20   let a = env::args();\n\
               \x20   let last = a[a.len() - 1];\n\
               \x20   println(f\"{a.len()} {last}\");\n\
               \x20   if last == \"kill\" { unsafe { raise(9) }; }\n\
               \x20   3\n\
               }\n";
    let app = app(&dir, src);
    let out = nabi_in(Some(&env), &app, &["run", "--", "one", "two"]);
    assert_exit(&out, 3);
    assert_eq!(text(&out.stdout), "3 two\n");
    let err = text(&out.stderr);
    for verb in ["   Compiling app", "    Finished in ", "     Running `"] {
        assert!(err.contains(verb), "{verb:?}: {err}");
    }
    if cfg!(unix) {
        let out = nabi_in(Some(&env), &app, &["run", "-q", "--", "kill"]);
        assert_exit(&out, 137);
    }
    let out = nabi_in(None, &app, &["clean"]);
    assert_exit(&out, 0);
    assert!(text(&out.stderr).contains("     Removed "));
    assert!(!app.join("target").exists());
    // A library has nothing to run.
    let lib = dir.join("lib");
    put(lib.join("src/lib.nabi"), "pub fn f() {}\n").unwrap();
    let out = nabi_in(Some(&env), &lib, &["run"]);
    assert_exit(&out, 1);
    assert!(
        text(&out.stderr).contains("is a library package"),
        "{}",
        text(&out.stderr)
    );
}

/// A lone file runs from the cache as itself; `-v` names the cached
/// executable.
#[test]
fn lone_file_runs_as_itself() {
    let env = setup();
    let dir = scratch("run-lone");
    put(dir.join("hi.nabi"), "println(\"hi\");\n").unwrap();
    let out = nabi_in(Some(&env), &dir, &["run", "hi.nabi"]);
    assert_exit(&out, 0);
    assert_eq!(text(&out.stdout), "hi\n");
    let err = text(&out.stderr);
    assert!(err.contains("     Running hi.nabi\n"), "{err}");
    let out = nabi_in(Some(&env), &dir, &["run", "-v", "hi.nabi"]);
    assert!(
        text(&out.stderr).contains("     Running `"),
        "{}",
        text(&out.stderr)
    );
    // Nothing is left beside the file.
    assert!(!dir.join(exe_name("hi")).exists() && !dir.join("target").exists());
}

// A program's arguments after `--` reach it as given, any bytes.
#[cfg(unix)]
#[test]
fn non_utf8_script_arg_passes_through() {
    let env = setup();
    let dir = scratch("run-os-args");
    put(
        dir.join("args.nabi"),
        "fn main() {\n    let a = std::env::args();\n    println(f\"{a.len()}\");\n}\n",
    )
    .unwrap();
    let out = Command::new(NABI)
        .current_dir(&dir)
        .env("NABI_HOME", tool_home())
        .args(["run", "-q", "args.nabi", "--nabir"])
        .arg(&env.nabir)
        .args(["--", "a"])
        .arg(<std::ffi::OsStr as std::os::unix::ffi::OsStrExt>::from_bytes(b"b\xffc"))
        .output()
        .unwrap();
    assert_exit(&out, 0);
    assert_eq!(text(&out.stdout), "3\n");
}

/// The `__NABI*` hand-offs stay out of the program that `run` and `test`
/// start; other variables reach it.
#[test]
fn program_env_no_nabi_handoff() {
    let env = setup();
    let dir = scratch("run-handoff");
    let body = "    for (k, _) in env::vars() {\n        if k.starts_with(\"__NABI\") || k == \
                \"NABI_KEEP\" { println(k); }\n    }\n";
    let app = app(&dir, &format!("use std::env;\n\nfn main() {{\n{body}}}\n"));
    put(
        app.join("src/lib.nabi"),
        "use std::env;\n\n#[test]\nfn sees() {\n    for (k, _) in env::vars() {\n        \
         if k.starts_with(\"__NABI\") { panic(k); }\n    }\n}\n",
    )
    .unwrap();
    let run = |args: &[&str]| {
        Command::new(NABI)
            .current_dir(&app)
            .env("NABI_HOME", tool_home())
            .env("__NABI_TEST_ONLY", "1")
            .env("NABI_KEEP", "1")
            .args(args)
            .arg("--nabir")
            .arg(&env.nabir)
            .output()
            .unwrap()
    };
    let out = run(&["run", "-q"]);
    assert_exit(&out, 0);
    assert_eq!(text(&out.stdout), "NABI_KEEP\n", "{}", text(&out.stderr));
    let out = run(&["test", "-q"]);
    assert_exit(&out, 0);
}

/// The program ends with `nabi run`: a signal sent to `nabi` goes on to
/// it, and a `nabi` killed outright takes it along.
#[test]
fn program_ends_with_nabi_run() {
    let env = setup();
    let dir = scratch("run-endless");
    put(
        dir.join("endless.nabi"),
        "use core::time::Duration;\nuse std::process;\nuse std::thread;\n\n\
         fn main() {\n    eprintln(f\"{process::id()}\");\n    \
         loop { thread::sleep(Duration::ms(50)); }\n}\n",
    )
    .unwrap();
    // `nabi run` and the program's pid, once it runs.
    let start = || {
        let mut nabi = Command::new(NABI)
            .current_dir(&dir)
            .env("NABI_HOME", tool_home())
            .args(["run", "-q", "endless.nabi", "--nabir"])
            .arg(&env.nabir)
            .stdin(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        // Its pid, after any warnings.
        let pid = BufReader::new(nabi.stderr.take().unwrap())
            .lines()
            .map_while(Result::ok)
            .find_map(|l| l.trim().parse::<u32>().ok());
        let Some(pid) = pid else {
            let _ = nabi.kill();
            let _ = nabi.wait();
            panic!("`nabi run` printed no pid");
        };
        (nabi, pid)
    };
    #[cfg(unix)]
    {
        let (mut nabi, pid) = start();
        // SAFETY: our child, not yet reaped.
        unsafe { libc::kill(nabi.id() as i32, libc::SIGTERM) };
        let st = nabi.wait().unwrap();
        let sig = std::os::unix::process::ExitStatusExt::signal(&st);
        assert_eq!(sig, Some(libc::SIGTERM));
        assert!(gone(pid), "program {pid} outlived SIGTERM to `nabi run`");
    }
    let (mut nabi, pid) = start();
    nabi.kill().unwrap();
    nabi.wait().unwrap();
    assert!(gone(pid), "program {pid} outlived a killed `nabi run`");
}

/// Process `pid` ends within 10 s.
fn gone(pid: u32) -> bool {
    #[cfg(unix)]
    return (0..200).any(|_| {
        // SAFETY: signal 0 only checks the pid.
        let ended = unsafe { libc::kill(pid as i32, 0) } != 0;
        if !ended {
            std::thread::sleep(Duration::from_millis(50));
        }
        ended
    });
    #[cfg(windows)]
    return {
        type Handle = *mut std::ffi::c_void;
        unsafe extern "system" {
            fn OpenProcess(access: u32, inherit: i32, pid: u32) -> Handle;
            fn WaitForSingleObject(h: Handle, ms: u32) -> u32;
            fn CloseHandle(h: Handle) -> i32;
        }
        const SYNCHRONIZE: u32 = 0x0010_0000;
        // SAFETY: a handle opened and closed here.
        unsafe {
            let h = OpenProcess(SYNCHRONIZE, 0, pid);
            if h.is_null() {
                return true;
            }
            let ended = WaitForSingleObject(h, 10_000) == 0;
            CloseHandle(h);
            ended
        }
    };
}
