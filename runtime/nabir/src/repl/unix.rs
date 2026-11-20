//! POSIX: a cell runs in the host while a forked copy from before it
//! waits for its verdict, and takes over if the cell's process dies.

use crate::repl::{
    Buf, CTL, Global, QUIESCE_CANCEL, STDIN, WHY, WHY_BUF, clear_shown, cstr, ctl, ctl_print,
    ctl_write, link, next_cell, quiesce, read_command, set_why, shown, why,
};
use core::ffi::c_void;
use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicI32, Ordering};

#[cfg(target_os = "linux")]
pub(crate) const RTLD_DEFAULT: *mut c_void = ptr::null_mut();
#[cfg(target_os = "macos")]
pub(crate) const RTLD_DEFAULT: *mut c_void = -2isize as *mut c_void;

extern "C" fn on_sigint(_: i32) {
    QUIESCE_CANCEL.store(true, Ordering::SeqCst);
}

/// The running cell's end of the verdict pipe (`await_verdict`); -1: none.
static VERDICT_FD: AtomicI32 = AtomicI32::new(-1);

/// A cell that ends early tells the copy waiting for it `!` and `why`, in
/// one write (at most `PIPE_BUF` bytes: read whole).
fn tell_why() {
    let mut msg = [0u8; 1 + WHY];
    msg[0] = b'!';
    let w = why();
    msg[1..=w.len()].copy_from_slice(w);
    let fd = VERDICT_FD.swap(-1, Ordering::SeqCst);
    if fd >= 0 {
        // SAFETY: `msg` holds the bytes written.
        unsafe { libc::write(fd, msg.as_ptr().cast(), 1 + w.len()) };
    }
}

/// The runtime is about to end the process (panic, uncaught error, `exit`).
extern "C" fn on_exit_hook(status: i32) {
    if VERDICT_FD.load(Ordering::SeqCst) < 0 {
        return;
    }
    if why().is_empty() && status == 101 {
        set_why(b"panic");
    }
    tell_why();
}

extern "C" fn on_cell_sigint(_: i32) {
    set_why(b"interrupted");
    tell_why();
    // SAFETY: ends the process.
    unsafe { libc::_exit(130) }
}

fn on_signal(f: extern "C" fn(i32)) -> libc::sighandler_t {
    f as libc::sighandler_t
}

// The parent (the `nabi` REPL), and its session directory (the host's
// own). The host ends with the parent: between cells on the channel's EOF,
// during a cell through the copy waiting for it (`await_verdict`).
// Whichever ends last removes the directory, unless the parent keeps
// temporaries.
static PARENT_PID: Global<libc::pid_t> = Global::new(0);
static SESSION_DIR: Global<Buf<4096>> = Global::new(Buf::new());

fn parent_pid() -> libc::pid_t {
    // SAFETY: set once at start.
    unsafe { *PARENT_PID.get() }
}

/// Remove `path` (NUL-terminated in `path`) and, a directory, what is in
/// it, without following links.
///
/// # Safety
/// The host's own session directory.
unsafe fn remove_tree(path: &mut Buf<8192>) {
    // SAFETY: plain libc calls on NUL-terminated paths.
    unsafe {
        let mut st: libc::stat = core::mem::zeroed();
        if libc::lstat(path.as_ptr().cast(), &raw mut st) != 0 {
            return;
        }
        if st.st_mode & libc::S_IFMT != libc::S_IFDIR {
            libc::unlink(path.as_ptr().cast());
            return;
        }
        let d = libc::opendir(path.as_ptr().cast());
        if !d.is_null() {
            let len = path.len;
            loop {
                let e = libc::readdir(d);
                if e.is_null() {
                    break;
                }
                let name = cstr((*e).d_name.as_ptr().cast());
                if name == b"." || name == b".." {
                    continue;
                }
                path.push(b"/");
                path.push(name);
                remove_tree(path);
                path.truncate(len);
            }
            libc::closedir(d);
        }
        libc::rmdir(path.as_ptr().cast());
    }
}

fn remove_session() {
    // SAFETY: set once at start.
    let dir = unsafe { &*SESSION_DIR.get() };
    if dir.len == 0 {
        return;
    }
    let mut p = Buf::<8192>::new();
    p.push(dir.bytes());
    p.push(b"/.nabi-keep");
    // SAFETY: a NUL-terminated path.
    if unsafe { libc::access(p.as_ptr().cast(), libc::F_OK) } == 0 {
        return;
    }
    p.truncate(dir.len);
    // SAFETY: the session directory.
    unsafe { remove_tree(&mut p) };
}

/// `exe`'s directory, if it is a compiler temporary (`nabic-<pid>-...`).
fn set_session_dir(exe: &[u8]) {
    let Some(slash) = exe.iter().rposition(|&c| c == b'/') else {
        return;
    };
    let dir = &exe[..slash];
    if dir.len() >= 4096 {
        return;
    }
    let base = dir.iter().rposition(|&c| c == b'/').map(|i| &dir[i + 1..]);
    if base.is_some_and(|b| b.starts_with(b"nabic")) {
        // SAFETY: once, at start.
        unsafe { (*SESSION_DIR.get()).push(dir) };
    }
}

/// A descriptor readable once the parent exits; -1 if it has, -2 if it can't be
/// watched (then polled).
fn watch_parent() -> i32 {
    // SAFETY: plain OS calls.
    unsafe {
        #[cfg(target_os = "macos")]
        {
            let kq = libc::kqueue();
            if kq < 0 {
                return -2;
            }
            let ev = libc::kevent {
                ident: parent_pid() as usize,
                filter: libc::EVFILT_PROC,
                flags: libc::EV_ADD | libc::EV_ONESHOT,
                fflags: libc::NOTE_EXIT,
                data: 0,
                udata: ptr::null_mut(),
            };
            if libc::kevent(kq, &raw const ev, 1, ptr::null_mut(), 0, ptr::null()) == 0 {
                return kq;
            }
            let gone = nabir_common::errno() == libc::ESRCH;
            libc::close(kq);
            if gone { -1 } else { -2 }
        }
        #[cfg(target_os = "linux")]
        {
            // Linux 5.3+.
            const SYS_PIDFD_OPEN: libc::c_long = 434;
            let fd = libc::syscall(SYS_PIDFD_OPEN, parent_pid(), 0);
            if fd >= 0 {
                return fd as i32;
            }
            if nabir_common::errno() == libc::ESRCH {
                -1
            } else {
                -2
            }
        }
    }
}

/// The copy from before a cell waits for its verdict on `fd`, read into
/// `buf`: a 0 byte, else `tell_why`'s or nothing (it died). Should the
/// parent die meanwhile, nothing would end the cell: end it and the
/// session.
fn await_verdict(fd: i32, buf: &mut [u8], host: libc::pid_t) -> isize {
    let w = watch_parent();
    // SAFETY: plain OS calls on our descriptors.
    unsafe {
        loop {
            if w == -1 {
                break;
            }
            let mut p = [
                libc::pollfd {
                    fd,
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: w,
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            let n = libc::poll(
                p.as_mut_ptr(),
                if w >= 0 { 2 } else { 1 },
                if w >= 0 { -1 } else { 1000 },
            );
            if n < 0 && nabir_common::errno() != libc::EINTR {
                break;
            }
            if n > 0 && p[0].revents != 0 {
                if w >= 0 {
                    libc::close(w);
                }
                loop {
                    let r = libc::read(fd, buf.as_mut_ptr().cast(), buf.len());
                    if r >= 0 || nabir_common::errno() != libc::EINTR {
                        return r;
                    }
                }
            }
            let gone = if w >= 0 {
                n > 0
            } else {
                libc::kill(parent_pid(), 0) != 0 && nabir_common::errno() == libc::ESRCH
            };
            if gone {
                break;
            }
        }
        if libc::getppid() == host {
            libc::kill(host, libc::SIGKILL);
        }
        remove_session();
        libc::_exit(0)
    }
}

/// Set while the host exits: a failed write to the parent.
static GONE: AtomicBool = AtomicBool::new(false);

fn ctl_write_all(b: &[u8]) {
    // SAFETY: `b` is valid for its length.
    if unsafe { libc::write(ctl(), b.as_ptr().cast(), b.len()) } < 0 {
        GONE.store(true, Ordering::SeqCst);
    }
}

/// # Safety
/// Once, on the main thread, with `main`'s arguments.
pub(crate) unsafe fn main(argc: i32, argv: *const *const u8) -> i32 {
    // SAFETY: start-up, before any other thread.
    unsafe {
        *PARENT_PID.get() = libc::getppid();
        if argc > 0 {
            set_session_dir(cstr(*argv));
        }
        *CTL.get() = libc::dup(0);
        let nul = libc::open(c"/dev/null".as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC);
        (*STDIN.get()).0 = nul;
        // The cells' stdin.
        libc::dup2(nul, 0);
        // No concurrent marking: its marker thread is not in a snapshot.
        libc::setenv(c"NABIR_GC_CONCURRENT".as_ptr(), c"off".as_ptr(), 1);
        crate::nabi_runtime_init();
        crate::stdio::nabi_stdio_line_buffered();
        crate::nabi_set_exit_hook(Some(on_exit_hook));
        libc::signal(libc::SIGINT, libc::SIG_IGN);
    }
    let mut line = Buf::<8192>::new();
    loop {
        if !read_command(&mut line) {
            return 0;
        }
        let Some((ir, n)) = next_cell(&line) else {
            continue;
        };
        if ir.is_null() {
            return 1;
        }
        crate::stdio::nabi_stdio_flush();
        // A copy of the session from before the cell takes over if the
        // cell's process dies (panic, signal, exit, compile failure), else
        // is told to go. The JIT has no threads; other threads are quiesced
        // while the copy is made and forgotten when it takes over (the
        // executor restarts its own, their locks are released).
        let mut verdict = [0i32; 2];
        // SAFETY: plain OS calls; `ir` is `malloc`ed.
        unsafe {
            if libc::pipe(verdict.as_mut_ptr()) != 0 {
                return 1;
            }
            ctl_print(format_args!("pid {}\n", libc::getpid()));
            libc::signal(libc::SIGINT, on_signal(on_sigint));
            let quiet = quiesce();
            libc::signal(libc::SIGINT, libc::SIG_IGN);
            if !quiet {
                libc::free(ir.cast());
                libc::close(verdict[0]);
                libc::close(verdict[1]);
                ctl_write(b"err 11\ninterrupted");
                continue;
            }
            let host = libc::getpid();
            let snap = libc::fork();
            crate::session::nabi_session_resume();
            if snap == 0 {
                libc::close(verdict[1]);
                let mut got = [0u8; 1 + WHY];
                let r = await_verdict(verdict[0], &mut got, host);
                if r > 0 && got[0] == 0 {
                    libc::_exit(0);
                }
                libc::close(verdict[0]);
                libc::free(ir.cast());
                crate::session::nabi_session_restored(ptr::null(), usize::MAX);
                let len = if r > 1 { r as usize - 1 } else { 0 };
                ctl_print(format_args!("err {len}\n"));
                ctl_write_all(&got[1..=len]);
                if GONE.load(Ordering::SeqCst) {
                    return 1;
                }
                continue;
            }
            libc::close(verdict[0]);
            (*WHY_BUF.get()).1 = 0;
            VERDICT_FD.store(verdict[1], Ordering::SeqCst);
            libc::signal(libc::SIGINT, on_signal(on_cell_sigint));
            let cell = link::jit_cell(ir, n);
            libc::free(ir.cast());
            // The copy, from before the cell, goes on.
            let Some(cell) = cell else { libc::_exit(1) };
            clear_shown();
            cell();
            libc::signal(libc::SIGINT, libc::SIG_IGN);
            VERDICT_FD.store(-1, Ordering::SeqCst);
            crate::stdio::nabi_stdio_flush();
            libc::write(verdict[1], c"".as_ptr().cast(), 1);
            libc::close(verdict[1]);
            if snap > 0 {
                libc::waitpid(snap, ptr::null_mut(), 0);
            }
            let out = shown();
            ctl_print(format_args!("ok {}\n", out.len()));
            ctl_write_all(out);
            if GONE.load(Ordering::SeqCst) {
                return 1;
            }
        }
    }
}
