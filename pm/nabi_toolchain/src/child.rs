//! A program `nabi` runs and waits for in its place (`nabi run`'s program,
//! `nabic` or another toolchain's `nabi` on Windows): it ends with `nabi`.
//!
//! SIGTERM, SIGHUP, SIGINT and SIGQUIT sent to `nabi` go on to it (the
//! terminal's own reach it directly); should `nabi` die first, it is
//! killed (Linux, FreeBSD: by the kernel; macOS: by a watcher process;
//! Windows: by a job object).

use std::io;
use std::process::{self, Command, ExitStatus};

/// Run `c` to its end.
pub fn status(c: &mut Command) -> io::Result<ExitStatus> {
    #[cfg(unix)]
    return unix::status(c);
    #[cfg(windows)]
    return windows::status(c);
}

/// Exit as the program did: its exit code, or `128 + n` for signal `n`.
/// A signal `nabi` passes on ends `nabi` too, so a shell sees an
/// interrupted command.
pub fn exit(st: ExitStatus) -> ! {
    #[cfg(unix)]
    if let Some(sig) = std::os::unix::process::ExitStatusExt::signal(&st) {
        if unix::PASSED.contains(&sig) {
            nabic_core::tmp::remove_live();
            // SAFETY: back to the default action, then that signal.
            unsafe {
                libc::signal(sig, libc::SIG_DFL);
                libc::raise(sig);
            }
        }
        process::exit(128 + sig);
    }
    process::exit(st.code().unwrap_or(1));
}

#[cfg(unix)]
mod unix {
    use std::io;
    #[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "macos"))]
    use std::os::unix::process::CommandExt;
    use std::process::{Command, ExitStatus};
    use std::sync::Once;
    use std::sync::atomic::{AtomicI32, Ordering};

    /// The signals passed on.
    pub(crate) const PASSED: [i32; 4] = [libc::SIGHUP, libc::SIGINT, libc::SIGQUIT, libc::SIGTERM];

    /// The running program (0: none), a signal that came before it, and
    /// the controlling terminal (-1: none).
    static CHILD: AtomicI32 = AtomicI32::new(0);
    static PENDING: AtomicI32 = AtomicI32::new(0);
    static TTY: AtomicI32 = AtomicI32::new(-1);

    extern "C" fn pass(sig: i32) {
        // In the terminal's foreground group the program gets the
        // terminal's signals itself; a second copy would count twice.
        let tty = TTY.load(Ordering::SeqCst);
        // SAFETY: tcgetpgrp, getpgrp and kill are async-signal-safe.
        unsafe {
            if sig != libc::SIGTERM && tty >= 0 && libc::tcgetpgrp(tty) == libc::getpgrp() {
                return;
            }
            match CHILD.load(Ordering::SeqCst) {
                0 => PENDING.store(sig, Ordering::SeqCst),
                pid => {
                    libc::kill(pid, sig);
                }
            }
        }
    }

    fn catch() {
        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            // SAFETY: plain open of a constant path.
            let tty = unsafe {
                libc::open(
                    c"/dev/tty".as_ptr(),
                    libc::O_RDONLY | libc::O_NOCTTY | libc::O_CLOEXEC,
                )
            };
            TTY.store(tty, Ordering::SeqCst);
            for sig in PASSED {
                // SAFETY: a zeroed sigaction with a handler that only
                // calls async-signal-safe functions.
                unsafe {
                    let mut sa: libc::sigaction = std::mem::zeroed();
                    sa.sa_sigaction = pass as *const () as usize;
                    sa.sa_flags = libc::SA_RESTART;
                    libc::sigemptyset(&raw mut sa.sa_mask);
                    libc::sigaction(sig, &raw const sa, std::ptr::null_mut());
                }
            }
        });
    }

    pub(crate) fn status(c: &mut Command) -> io::Result<ExitStatus> {
        catch();
        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        {
            let parent = std::process::id() as libc::pid_t;
            // SAFETY: async-signal-safe calls only, between fork and exec.
            unsafe {
                c.pre_exec(move || {
                    die_with_parent()?;
                    // The parent may have died before that.
                    if libc::getppid() != parent {
                        libc::raise(libc::SIGKILL);
                    }
                    Ok(())
                })
            };
        }
        // The watcher first, then the program, which tells it its pid:
        // no moment where `nabi` can die unwatched.
        #[cfg(target_os = "macos")]
        let (watcher, tell) = watch();
        #[cfg(target_os = "macos")]
        if tell >= 0 {
            // SAFETY: getpid and a write of at most PIPE_BUF bytes, both
            // async-signal-safe, between fork and exec.
            unsafe {
                c.pre_exec(move || {
                    let me = libc::getpid();
                    let n = std::mem::size_of_val(&me);
                    libc::write(tell, (&raw const me).cast(), n);
                    Ok(())
                })
            };
        }
        let spawned = c.spawn();
        #[cfg(target_os = "macos")]
        // SAFETY: our end of the watcher's pipe and our own child.
        unsafe {
            if tell >= 0 {
                libc::close(tell);
            }
            if spawned.is_err() && watcher > 0 {
                libc::kill(watcher, libc::SIGKILL);
                libc::waitpid(watcher, std::ptr::null_mut(), 0);
            }
        }
        let mut child = spawned?;
        let pid = child.id() as libc::pid_t;
        CHILD.store(pid, Ordering::SeqCst);
        let sig = PENDING.swap(0, Ordering::SeqCst);
        if sig != 0 {
            // SAFETY: our unreaped child.
            unsafe { libc::kill(pid, sig) };
        }
        // Wait without reaping: the pid stays ours while CHILD names it.
        // SAFETY: a zeroed siginfo for waitid to fill.
        unsafe {
            let mut info: libc::siginfo_t = std::mem::zeroed();
            while libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &raw mut info,
                libc::WEXITED | libc::WNOWAIT,
            ) != 0
                && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted
            {}
        }
        CHILD.store(0, Ordering::SeqCst);
        #[cfg(target_os = "macos")]
        if watcher > 0 {
            // SAFETY: our own child, reaped here.
            unsafe {
                libc::kill(watcher, libc::SIGKILL);
                libc::waitpid(watcher, std::ptr::null_mut(), 0);
            }
        }
        child.wait()
    }

    /// Have the kernel kill this process when its parent dies.
    #[cfg(target_os = "linux")]
    fn die_with_parent() -> io::Result<()> {
        // SAFETY: plain prctl.
        if unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    #[cfg(target_os = "freebsd")]
    fn die_with_parent() -> io::Result<()> {
        let mut sig = libc::SIGKILL;
        // SAFETY: procctl on this process with a live int.
        let r = unsafe {
            libc::procctl(
                libc::P_PID,
                0,
                libc::PROC_PDEATHSIG_CTL,
                (&raw mut sig).cast(),
            )
        };
        if r != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// A forked watcher that kills the program should this process die
    /// first: its pid and the end of a pipe the program writes its own
    /// pid to before exec (close-on-exec), or -1s. The watcher leaves
    /// when the pipe closes with no pid: no program ran.
    #[cfg(target_os = "macos")]
    fn watch() -> (libc::pid_t, libc::c_int) {
        // SAFETY: the forked side makes async-signal-safe calls only and
        // leaves by `_exit`.
        unsafe {
            let mut fds = [-1; 2];
            if libc::pipe(fds.as_mut_ptr()) != 0 {
                return (-1, -1);
            }
            for fd in fds {
                libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
            }
            let parent = libc::getpid();
            let pid = libc::fork();
            if pid != 0 {
                libc::close(fds[0]);
                if pid < 0 {
                    libc::close(fds[1]);
                    return (-1, -1);
                }
                return (pid, fds[1]);
            }
            // Only the program's end or ours ends the watcher.
            for sig in [
                libc::SIGHUP,
                libc::SIGINT,
                libc::SIGQUIT,
                libc::SIGTERM,
                libc::SIGTSTP,
            ] {
                libc::signal(sig, libc::SIG_IGN);
            }
            // Hold none of the parent's files: pipes a reader waits to
            // close, locks.
            for fd in 0..libc::getdtablesize().min(1 << 16) {
                if fd != fds[0] {
                    libc::close(fd);
                }
            }
            let mut child: libc::pid_t = 0;
            let size = std::mem::size_of_val(&child);
            let mut got = 0;
            while got < size {
                let n = libc::read(
                    fds[0],
                    (&raw mut child).cast::<u8>().add(got).cast(),
                    size - got,
                );
                if n > 0 {
                    got += n as usize;
                } else if n == 0 || *libc::__error() != libc::EINTR {
                    libc::_exit(0);
                }
            }
            libc::close(fds[0]);
            let kq = libc::kqueue();
            let ev = |pid: libc::pid_t| libc::kevent {
                ident: pid as usize,
                filter: libc::EVFILT_PROC,
                flags: libc::EV_ADD | libc::EV_ONESHOT,
                fflags: libc::NOTE_EXIT,
                data: 0,
                udata: std::ptr::null_mut(),
            };
            let add = |pid| {
                let e = ev(pid);
                libc::kevent(
                    kq,
                    &raw const e,
                    1,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null(),
                ) == 0
            };
            if kq < 0 || !add(child) {
                libc::_exit(0);
            }
            let watching = add(parent);
            if !watching && *libc::__error() != libc::ESRCH {
                libc::_exit(0);
            }
            if watching && libc::getppid() == parent {
                let mut out = [ev(0), ev(0)];
                loop {
                    let n = libc::kevent(
                        kq,
                        std::ptr::null(),
                        0,
                        out.as_mut_ptr(),
                        2,
                        std::ptr::null(),
                    );
                    if n > 0 {
                        if out[..n as usize].iter().any(|e| e.ident == child as usize) {
                            libc::_exit(0);
                        }
                        break;
                    }
                }
            }
            libc::kill(child, libc::SIGKILL);
            libc::_exit(0)
        }
    }
}

#[cfg(windows)]
mod windows {
    use std::io;
    use std::os::windows::io::AsRawHandle;
    use std::process::{Command, ExitStatus};

    type Handle = *mut std::ffi::c_void;

    unsafe extern "system" {
        fn CreateJobObjectW(security: *mut std::ffi::c_void, name: *const u16) -> Handle;
        fn SetInformationJobObject(job: Handle, class: i32, info: *const u8, len: u32) -> i32;
        fn AssignProcessToJobObject(job: Handle, process: Handle) -> i32;
        fn SetConsoleCtrlHandler(handler: Option<extern "system" fn(u32) -> i32>, add: i32) -> i32;
    }

    /// `JOBOBJECT_EXTENDED_LIMIT_INFORMATION`.
    #[repr(C)]
    #[derive(Default)]
    struct Limits {
        per_process_time: i64,
        per_job_time: i64,
        flags: u32,
        min_working_set: usize,
        max_working_set: usize,
        active_processes: u32,
        affinity: usize,
        priority_class: u32,
        scheduling_class: u32,
        io: [u64; 6],
        process_memory: usize,
        job_memory: usize,
        peak_process_memory: usize,
        peak_job_memory: usize,
    }

    const EXTENDED_LIMIT_INFORMATION: i32 = 9;
    const KILL_ON_JOB_CLOSE: u32 = 0x2000;
    const SILENT_BREAKAWAY_OK: u32 = 0x1000;

    const extern "system" fn ignore(_: u32) -> i32 {
        1
    }

    pub(crate) fn status(c: &mut Command) -> io::Result<ExitStatus> {
        // Ctrl-C / Ctrl-Break reach every process on the console: let the
        // child decide and wait. A handler, unlike `NULL`, isn't inherited.
        // SAFETY: registers a handler that only returns.
        unsafe { SetConsoleCtrlHandler(Some(ignore), 1) };
        let mut child = c.spawn()?;
        // The job's one handle is ours and never closed: it closes when
        // this process ends, however it ends, and that kills the child.
        // The processes the child starts stay out of the job, as on unix.
        // SAFETY: an unnamed job with default security; `limits` has the
        // layout the class names; both handles are live.
        unsafe {
            let job = CreateJobObjectW(std::ptr::null_mut(), std::ptr::null());
            if !job.is_null() {
                let limits = Limits {
                    flags: KILL_ON_JOB_CLOSE | SILENT_BREAKAWAY_OK,
                    ..Limits::default()
                };
                SetInformationJobObject(
                    job,
                    EXTENDED_LIMIT_INFORMATION,
                    (&raw const limits).cast(),
                    size_of::<Limits>() as u32,
                );
                AssignProcessToJobObject(job, child.as_raw_handle());
            }
        }
        child.wait()
    }
}
