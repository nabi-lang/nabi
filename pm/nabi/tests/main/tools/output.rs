//! Closed output pipes and leftover temporaries.

#[allow(clippy::wildcard_imports)]
use crate::tools::*;

#[test]
fn check_reads_stdin() {
    let out = piped(
        NABI,
        &["-q", "check", "-"],
        "let x: i32 = 1;\nprintln(f\"{x}\");\n",
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let out = piped(NABI, &["-q", "check", "-"], "let x: i32 = true;\n");
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stderr).contains("<stdin>:1:"),
        "{}",
        text(&out.stderr)
    );
}

/// How nabic ended once its stdout (`out`) or stderr lost its reader
/// before `input` reached its stdin, and what it wrote to the other one
/// (for a failure's message).
///
/// Unix: the closed stream is a pipe made in the child with its read end
/// closed there. A pipe made here could have its read end inherited by a
/// process another test spawns meanwhile (macOS std sets `FD_CLOEXEC`
/// after `pipe`), and nabic's writes would then succeed.
pub(crate) fn closed_output(args: &[&str], out: bool, input: &str) -> Closed {
    let mut cmd = Command::new(NABI);
    cmd.args(args)
        .env("NABI_HOME", tool_home())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        let fd = if out { 1 } else { 2 };
        if out {
            cmd.stdout(Stdio::null());
        } else {
            cmd.stderr(Stdio::null());
        }
        // SAFETY: pipe / close / dup2 are async-signal-safe.
        unsafe {
            std::os::unix::process::CommandExt::pre_exec(&mut cmd, move || {
                let mut p = [0; 2];
                if libc::pipe(p.as_mut_ptr()) != 0 || libc::dup2(p[1], fd) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                libc::close(p[0]);
                libc::close(p[1]);
                Ok(())
            });
        }
    }
    let mut child = Reaped(cmd.spawn().unwrap());
    let kept = if out {
        drop(child.stdout.take());
        child.stderr.take().map(read_all)
    } else {
        drop(child.stderr.take());
        child.stdout.take().map(read_all)
    };
    // nabic may be gone before it read everything.
    drop(child.stdin.take().unwrap().write_all(input.as_bytes()));
    let status = child.wait().unwrap();
    let other = kept.map(|t| t.join().unwrap()).unwrap_or_default();
    Closed { status, other }
}

/// `closed_output`'s result; shown as the status and the other stream.
pub(crate) struct Closed {
    status: std::process::ExitStatus,
    other: String,
}

impl std::fmt::Display for Closed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}, other stream: {:?}", self.status, self.other)
    }
}

pub(crate) fn read_all(
    mut r: impl std::io::Read + Send + 'static,
) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut s = String::new();
        let _ = r.read_to_string(&mut s);
        s
    })
}

/// Whether `status` is how a program ends writing to a closed pipe:
/// killed by SIGPIPE (Windows: std drops the writes, and these `check`s
/// end with their errors' exit 1), not a panic or abort.
pub(crate) fn ended_by_closed_pipe(c: &Closed) -> bool {
    #[cfg(unix)]
    {
        std::os::unix::process::ExitStatusExt::signal(&c.status) == Some(13)
    }
    #[cfg(not(unix))]
    {
        c.status.code() == Some(1)
    }
}

/// A closed stdout / stderr ends nabic quietly (`nabic .. | head`), not
/// as an internal error (101) or an abort in the panic hook.
#[test]
fn closed_output_ends_nabic_quietly() {
    let errors = "let x: i32 = true;\n".repeat(2000);
    let st = closed_output(&["check", "-"], false, &errors);
    assert!(ended_by_closed_pipe(&st), "check, stderr closed: {st}");
    let st = closed_output(&["--message-format=json", "check", "-"], true, &errors);
    assert!(ended_by_closed_pipe(&st), "check json, stdout closed: {st}");
}

#[test]
#[cfg_attr(
    not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(
            target_os = "linux",
            not(target_env = "musl"),
            any(target_arch = "aarch64", target_arch = "x86_64")
        ),
        windows
    )),
    ignore = "the REPL host runs on arm64 macOS, Linux glibc and Windows (arm64, x86_64)"
)]
pub(crate) fn repl_closed_output_ends_quietly() {
    let env = setup();
    let nabir = env.nabir.display().to_string();
    let args = ["repl", "--nabir", &nabir];
    let st = closed_output(&args, false, "let a = 1; let b: str = 2;\n");
    assert!(ended_by_closed_pipe(&st), "stderr closed: {st}");
    let st = closed_output(&args, true, "1 + 2\n");
    assert!(ended_by_closed_pipe(&st), "stdout closed: {st}");
}

/// Entries in the temp directory `tmp` other than the lone-file cache.
pub(crate) fn leftovers(tmp: &std::path::Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(tmp)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !n.starts_with("nabic-target"))
        .collect();
    v.sort();
    v
}

/// Processes whose executable or command line names `dir`.
pub(crate) fn processes_in(dir: &std::path::Path) -> Vec<String> {
    let dir = dir.display().to_string();
    let out = if cfg!(windows) {
        Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                "Get-CimInstance Win32_Process | ForEach-Object { $_.ExecutablePath }",
            ])
            .output()
    } else {
        Command::new("ps").args(["-axo", "command"]).output()
    };
    text(&out.unwrap().stdout)
        .lines()
        .filter(|l| l.contains(&dir))
        .map(str::to_owned)
        .collect()
}

#[test]
#[cfg_attr(
    not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(
            target_os = "linux",
            not(target_env = "musl"),
            any(target_arch = "aarch64", target_arch = "x86_64")
        ),
        windows
    )),
    ignore = "the REPL host runs on arm64 macOS, Linux glibc and Windows (arm64, x86_64)"
)]
pub(crate) fn nabi_leaves_no_temporaries() {
    let env = setup();
    let nabir = env.nabir.display().to_string();
    let root = scratch("tool-no-temporaries");
    let tmp = root.join("tmp");
    std::fs::create_dir_all(&tmp).unwrap();
    let nabi = |args: &[&str]| {
        let mut c = Command::new(NABI);
        c.args(args).arg("--nabir").arg(&nabir).current_dir(&root);
        for v in ["TMPDIR", "TMP", "TEMP"] {
            c.env(v, &tmp);
        }
        c.env("NABI_HOME", tool_home());
        c
    };
    std::fs::write(root.join("hi.nabi"), "println(\"hi\");\n").unwrap();
    std::fs::write(root.join("bad.nabi"), "let x: i32 = \"no\";\n").unwrap();
    let out = nabi(&["run", "-q", "hi.nabi"]).output().unwrap();
    assert_eq!(text(&out.stdout), "hi\n", "{}", text(&out.stderr));
    let out = nabi(&["build", "-q", "bad.nabi"]).output().unwrap();
    assert!(!out.status.success());
    assert_eq!(leftovers(&tmp), Vec::<String>::new());

    // A REPL whose stdout lost its reader exits without running drops,
    // its host still alive: the host ends before the session goes.
    let mut child = Reaped(
        nabi(&["repl"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    drop(child.stdout.take());
    drop(child.stdin.take().unwrap().write_all(b"1 + 2\n"));
    child.wait().unwrap();
    assert_eq!(leftovers(&tmp), Vec::<String>::new());

    // nabi killed while a cell runs: its host ends too, and nothing stays.
    let mark = root.join("started");
    let cell = format!(
        "use std::fs;\ntry! fs::write(\"{}\", \"\"); loop {{}}\n",
        mark.display().to_string().replace('\\', "/")
    );
    let mut child = Reaped(
        nabi(&["repl"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(cell.as_bytes())
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    while !mark.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(mark.exists(), "the cell did not start");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        let (left, procs) = (leftovers(&tmp), processes_in(&tmp));
        // Windows: the job ends the host; its directory, still holding the
        // host's image as it ends, is left to the sweep.
        if procs.is_empty() && (left.is_empty() || cfg!(windows)) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "left behind: {left:?}, running: {procs:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}
