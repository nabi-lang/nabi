//! The REPL on a terminal: line editing, prompts, Ctrl-C.

#[allow(clippy::wildcard_imports)]
use crate::tools::*;

/// `Nabi <version> on <host>` as `nabi --version` reports them.
pub(crate) fn toolchain_line() -> String {
    let out = Command::new(NABI)
        .args(["--version", "--verbose"])
        .output()
        .unwrap();
    let out = text(&out.stdout);
    let version = out.lines().next().unwrap().strip_prefix("nabi ").unwrap();
    let host = out
        .lines()
        .find_map(|l| l.strip_prefix("host:"))
        .unwrap()
        .trim();
    format!("Nabi {version} on {host}")
}

/// A child killed and waited for when dropped: a test that panics or
/// times out leaves no process behind (one blocked on a terminal would
/// keep whatever its parent's fds hold, a lock file among them).
pub(crate) struct Reaped(pub(crate) std::process::Child);

impl Drop for Reaped {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl std::ops::Deref for Reaped {
    type Target = std::process::Child;
    fn deref(&self) -> &std::process::Child {
        &self.0
    }
}

impl std::ops::DerefMut for Reaped {
    fn deref_mut(&mut self) -> &mut std::process::Child {
        &mut self.0
    }
}

/// Wait up to `secs` for `child` to end; past that it is killed (by
/// `Reaped`) and the test fails with `what`.
pub(crate) fn wait_within(
    child: &mut Reaped,
    secs: u64,
    what: &dyn Fn() -> String,
) -> std::process::ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if let Some(st) = child.try_wait().unwrap() {
            return st;
        }
        assert!(
            Instant::now() < deadline,
            "still running after {secs}s: {}",
            what()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A pseudo-terminal: the master side, and the slave for a child's
/// stdio. Neither is inherited by other children, so the terminal hangs
/// up once this process is gone.
#[cfg(unix)]
pub(crate) fn pty() -> (std::fs::File, std::fs::File) {
    // SAFETY: plain libc calls on a pseudo-terminal the caller owns.
    unsafe {
        let m = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY);
        assert!(m >= 0 && libc::grantpt(m) == 0 && libc::unlockpt(m) == 0);
        assert!(libc::fcntl(m, libc::F_SETFD, libc::FD_CLOEXEC) == 0);
        let name = std::ffi::CStr::from_ptr(libc::ptsname(m)).to_owned();
        let master = <std::fs::File as std::os::fd::FromRawFd>::from_raw_fd(m);
        let slave = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(name.to_str().unwrap())
            .unwrap();
        (master, slave)
    }
}

/// `nabi args` on a pseudo-terminal with `NABI_HOME` at `home`: each of
/// `steps` typed at the next prompt. What it wrote; it must succeed.
#[cfg(unix)]
pub(crate) fn on_tty(args: &[&str], home: &std::path::Path, steps: &[&[u8]]) -> String {
    let (master, slave) = pty();
    let child = Command::new(NABI)
        .args(args)
        .env("NABI_HOME", home)
        .env("TERM", "xterm")
        .env_remove("NO_COLOR")
        .env_remove("CLICOLOR_FORCE")
        .env_remove("COLORTERM")
        .stdin(slave.try_clone().unwrap())
        .stdout(slave.try_clone().unwrap())
        .stderr(slave)
        .spawn()
        .unwrap();
    let mut child = Reaped(child);
    let mut keys = master.try_clone().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut master = master;
        let mut b = [0u8; 4096];
        // EIO once the child is gone.
        while let Ok(n @ 1..) = master.read(&mut b) {
            if tx.send(b[..n].to_vec()).is_err() {
                break;
            }
        }
    });
    let mut out = String::new();
    // Each prompt turns bracketed paste on: wait for the `n`th, as keys
    // typed before raw mode would meet the line discipline.
    let wait = |out: &mut String, n: usize| {
        let deadline = Instant::now() + Duration::from_secs(120);
        while out.matches("\x1b[?2004h").count() < n {
            let left = deadline.saturating_duration_since(Instant::now());
            let Ok(b) = rx.recv_timeout(left) else {
                panic!("no prompt {n}: {out:?}");
            };
            out.push_str(&String::from_utf8_lossy(&b));
        }
    };
    for (i, keys_in) in steps.iter().enumerate() {
        wait(&mut out, i + 1);
        keys.write_all(keys_in).unwrap();
    }
    let st = wait_within(&mut child, 120, &String::new);
    assert!(st.success(), "{out:?}");
    while let Ok(b) = rx.recv_timeout(Duration::from_millis(200)) {
        out.push_str(&String::from_utf8_lossy(&b));
    }
    out
}

/// On a terminal: the session starts with the toolchain line, Tab
/// completes core names, a paste is one cell, Up and `C-r` recall whole
/// cells, the input is coloured, and the history file keeps the cells.
#[test]
#[cfg_attr(
    not(all(
        any(
            target_os = "macos",
            all(target_os = "linux", not(target_env = "musl"))
        ),
        any(target_arch = "aarch64", target_arch = "x86_64")
    )),
    ignore = "a pty test: the REPL host on macOS and Linux glibc (arm64, x86_64)"
)]
#[cfg(unix)]
pub(crate) fn repl_edits_lines_on_a_terminal() {
    let env = setup();
    let nabir = env.nabir.display().to_string();
    let home = scratch("repl-line-edit");
    let steps: [&[u8]; 7] = [
        b"let x: i32 = 4\r",
        b"\x1b[200~fn f(a: i32) -> i32 {\r\n    a * 2\r\n}\r\n\x1b[201~\r",
        b"f(x)\r",
        b"prin\tln(f(1))\r",
        b"\x1b[A\r",
        b"\x12f(x\r",
        b":exit\r",
    ];
    let out = on_tty(&["repl", "--nabir", &nabir], &home, &steps);
    let banner = format!(
        "{}\r\nType :help for help, :exit to quit.\r\n",
        toolchain_line()
    );
    assert!(out.starts_with(&banner), "{out:?}");
    let lines: Vec<&str> = out.split("\r\n").collect();
    let shown = |v: &str| {
        let lines = lines
            .iter()
            .map(|l| l.strip_prefix("\x1b[?2004l").unwrap_or(l));
        lines.filter(|l| *l == v).count()
    };
    assert_eq!((shown("8"), shown("2")), (2, 2), "{out:?}");
    // The pasted cell is shown as its lines, coloured.
    assert!(out.contains("\n... }\x1b[K"), "{out:?}");
    assert!(out.contains("\x1b[95mfn\x1b[0m"), "{out:?}");
    let history = std::fs::read_to_string(home.join("repl_history")).unwrap();
    assert_eq!(
        history,
        "#nabi repl_history 1\nlet x: i32 = 4\nfn f(a: i32) -> i32 {\\n    a * 2\\n}\nf(x)\n\
         println(f(1))\nf(x)\n:exit\n"
    );
}

/// `-q` leaves the toolchain line and the hint out.
#[test]
#[cfg_attr(
    not(all(
        any(
            target_os = "macos",
            all(target_os = "linux", not(target_env = "musl"))
        ),
        any(target_arch = "aarch64", target_arch = "x86_64")
    )),
    ignore = "a pty test: the REPL host on macOS and Linux glibc (arm64, x86_64)"
)]
#[cfg(unix)]
pub(crate) fn repl_quiet_starts_without_a_banner() {
    let env = setup();
    let nabir = env.nabir.display().to_string();
    let home = scratch("repl-quiet");
    let out = on_tty(&["repl", "-q", "--nabir", &nabir], &home, &[b"quit\r"]);
    assert!(!out.contains("Nabi ") && !out.contains(":help"), "{out:?}");
}

/// After output that does not end its line the prompt starts a fresh
/// line, with no marker drawn.
#[test]
#[cfg_attr(
    not(all(
        any(
            target_os = "macos",
            all(target_os = "linux", not(target_env = "musl"))
        ),
        any(target_arch = "aarch64", target_arch = "x86_64")
    )),
    ignore = "a pty test: the REPL host on macOS and Linux glibc (arm64, x86_64)"
)]
#[cfg(unix)]
pub(crate) fn repl_prompt_after_unended_output_has_no_marker() {
    let env = setup();
    let nabir = env.nabir.display().to_string();
    let home = scratch("repl-fresh-line");
    let steps: [&[u8]; 2] = [b"print(\"ab\")\r", b"quit\r"];
    let out = on_tty(&["repl", "-q", "--nabir", &nabir], &home, &steps);
    assert!(out.contains("ab "), "{out:?}");
    assert!(!out.contains('%') && !out.contains("\x1b[7m"), "{out:?}");
}

/// Ctrl-C ends a running cell; the session goes on as before it.
#[test]
#[cfg_attr(
    not(all(
        any(
            target_os = "macos",
            all(target_os = "linux", not(target_env = "musl"))
        ),
        any(target_arch = "aarch64", target_arch = "x86_64")
    )),
    ignore = "the REPL host runs on macOS and Linux glibc, arm64 and x86_64"
)]
pub(crate) fn repl_ctrl_c_interrupts_the_cell() {
    let env = setup();
    let nabir = env.nabir.display().to_string();
    let child = Command::new(NABI)
        .args(["repl", "--nabir", &nabir])
        .env("NABI_HOME", tool_home())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut child = Reaped(child);
    let mut stdin = child.stdin.take().unwrap();
    let mut stderr = BufReader::new(child.stderr.take().unwrap());
    stdin
        .write_all(b"let n = 1\neprintln(\"spinning\"); n += 1; while true {}\n")
        .unwrap();
    let mut line = String::new();
    stderr.read_line(&mut line).unwrap();
    assert_eq!(line, "spinning\n");
    // The session host is the REPL's child; the cell runs in it.
    let host = Command::new("pgrep")
        .args(["-P", &child.id().to_string()])
        .output()
        .unwrap();
    let host = text(&host.stdout);
    let host = host.lines().next().unwrap_or_default();
    assert!(
        Command::new("kill")
            .args(["-INT", host])
            .status()
            .unwrap()
            .success()
    );
    stdin.write_all(b"n\n").unwrap();
    drop(stdin);
    let mut rest = String::new();
    stderr.read_to_string(&mut rest).unwrap();
    let mut stdout = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut stdout)
        .unwrap();
    assert!(wait_within(&mut child, 120, &String::new).success());
    assert_eq!(
        rest,
        "error: interrupted (Ctrl-C); the session is unchanged\n"
    );
    assert_eq!(stdout, "1\n");
}

/// Piped input is the REPL's own: a cell reads the end of input, and
/// the lines after it are still cells.
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
pub(crate) fn repl_piped_cells_read_no_input() {
    let env = setup();
    let nabir = env.nabir.display().to_string();
    let input = "((try io::read_line() catch _ { \"failed\" }) ?? \"\").len()\n\
                 40 + 2\n";
    let out = piped(NABI, &["repl", "--nabir", &nabir], input);
    assert_eq!(text(&out.stdout), "0\n42\n", "{}", text(&out.stderr));
}

/// On a terminal a cell reads the terminal, and the line editor has it
/// back after the cell.
#[cfg(unix)]
#[test]
#[cfg_attr(
    not(all(
        any(
            target_os = "macos",
            all(target_os = "linux", not(target_env = "musl"))
        ),
        any(target_arch = "aarch64", target_arch = "x86_64")
    )),
    ignore = "a pseudo-terminal: the REPL host on macOS and Linux glibc, arm64 and x86_64"
)]
pub(crate) fn repl_cells_read_the_terminal() {
    let env = setup();
    let nabir = env.nabir.display().to_string();
    let (mut master, slave) = pty();
    // Its own home: the line editor keeps its history there.
    let home = scratch("repl-tty");
    let mut cmd = Command::new(NABI);
    cmd.args(["repl", "--nabir", &nabir])
        .env("NABI_HOME", &home)
        .stdin(slave.try_clone().unwrap())
        .stdout(slave.try_clone().unwrap())
        .stderr(slave);
    // SAFETY: async-signal-safe calls only: the terminal becomes the
    // child's controlling one (`/dev/tty`).
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = Reaped(cmd.spawn().unwrap());
    drop(cmd);
    let seen = Arc::new(Mutex::new(String::new()));
    let mut reader = master.try_clone().unwrap();
    let s = seen.clone();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            s.lock()
                .unwrap()
                .push_str(&String::from_utf8_lossy(&buf[..n]));
        }
    });
    // Until `what` has been written `times` times.
    let wait_for = |what: &str, times: usize| {
        let deadline = Instant::now() + Duration::from_secs(120);
        while seen.lock().unwrap().matches(what).count() < times {
            assert!(
                Instant::now() < deadline,
                "no {what:?} in {:?}",
                seen.lock().unwrap()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    wait_for(">>> ", 1);
    master
        .write_all(b"println(\"name?\"); let name = (try io::read_line() catch _ { \"failed\" }) ?? \"none\";\r")
        .unwrap();
    wait_for("name?\r\n", 1);
    master.write_all(b"nabi\r").unwrap();
    wait_for(">>> ", 2);
    master.write_all(b"\"[\" + name + \"]\"\r").unwrap();
    wait_for("[nabi]", 1);
    master.write_all(b":quit\r").unwrap();
    wait_within(&mut child, 60, &|| seen.lock().unwrap().clone());
    // The cell read `nabi`, not the line editor.
    let history = std::fs::read_to_string(home.join("repl_history")).unwrap();
    assert!(
        history.ends_with("\"[\" + name + \"]\"\n:quit\n"),
        "{history}"
    );
    assert!(!history.lines().any(|l| l == "nabi"), "{history}");
    let _ = std::fs::remove_dir_all(&home);
}
