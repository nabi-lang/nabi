//! Processes and signals.

#[cfg(unix)]
use crate::os::posix::env::{environ, split_blob};
#[cfg(unix)]
use crate::os::posix::fs::set_errno;
#[allow(clippy::wildcard_imports)]
use crate::os::posix::*;
#[cfg(all(unix, not(target_os = "android")))]
use libc::{
    posix_spawn_file_actions_adddup2, posix_spawn_file_actions_addopen,
    posix_spawn_file_actions_destroy, posix_spawn_file_actions_init, posix_spawnattr_destroy,
    posix_spawnattr_init, posix_spawnattr_setflags, posix_spawnattr_setpgroup, posix_spawnp,
};

#[cfg(any(target_vendor = "apple", target_os = "freebsd"))]
unsafe extern "C" {
    // libSystem since macOS 10.15, FreeBSD's libc since 13.1; not in the
    // `libc` crate for either.
    fn posix_spawn_file_actions_addchdir_np(
        actions: *mut libc::posix_spawn_file_actions_t,
        path: CStrPtr,
    ) -> libc::c_int;
}

// bionic's, from API 28; not in the `libc` crate for Android.
#[cfg(target_os = "android")]
unsafe extern "C" {
    fn posix_spawn_file_actions_init(fa: *mut libc::posix_spawn_file_actions_t) -> libc::c_int;
    fn posix_spawn_file_actions_destroy(fa: *mut libc::posix_spawn_file_actions_t) -> libc::c_int;
    fn posix_spawn_file_actions_addopen(
        fa: *mut libc::posix_spawn_file_actions_t,
        fd: libc::c_int,
        path: CStrPtr,
        flags: libc::c_int,
        mode: libc::mode_t,
    ) -> libc::c_int;
    fn posix_spawn_file_actions_adddup2(
        fa: *mut libc::posix_spawn_file_actions_t,
        fd: libc::c_int,
        to: libc::c_int,
    ) -> libc::c_int;
    fn posix_spawnattr_init(at: *mut libc::posix_spawnattr_t) -> libc::c_int;
    fn posix_spawnattr_destroy(at: *mut libc::posix_spawnattr_t) -> libc::c_int;
    fn posix_spawnattr_setflags(at: *mut libc::posix_spawnattr_t, f: libc::c_short) -> libc::c_int;
    fn posix_spawnattr_setpgroup(at: *mut libc::posix_spawnattr_t, g: libc::pid_t) -> libc::c_int;
    fn posix_spawnp(
        pid: *mut libc::pid_t,
        file: CStrPtr,
        fa: *const libc::posix_spawn_file_actions_t,
        at: *const libc::posix_spawnattr_t,
        argv: *const *mut libc::c_char,
        envp: *const *mut libc::c_char,
    ) -> libc::c_int;
}

/// `posix_spawn_file_actions_addchdir_np`: bionic has it from API 34,
/// so it is looked up; `ENOSYS` without it.
#[cfg(target_os = "android")]
unsafe fn posix_spawn_file_actions_addchdir_np(
    fa: *mut libc::posix_spawn_file_actions_t,
    path: CStrPtr,
) -> libc::c_int {
    type F = unsafe extern "C" fn(*mut libc::posix_spawn_file_actions_t, CStrPtr) -> libc::c_int;
    // SAFETY: a symbol lookup; the symbol has this type where it exists.
    unsafe {
        let p = libc::dlsym(
            libc::RTLD_DEFAULT,
            c"posix_spawn_file_actions_addchdir_np".as_ptr(),
        );
        if p.is_null() {
            return libc::ENOSYS;
        }
        core::mem::transmute::<*mut libc::c_void, F>(p)(fa, path)
    }
}

/// 0 when `prog` names an executable as `execvp` finds it (a path with a
/// `/`, else on `PATH`), else the errno an exec would fail with.
#[cfg(target_os = "android")]
unsafe fn find_program(prog: CStrPtr) -> i32 {
    // SAFETY: C strings from the caller and the environment; `buf` holds
    // each candidate with its NUL.
    unsafe {
        let name = core::ffi::CStr::from_ptr(prog).to_bytes();
        if name.is_empty() {
            return libc::ENOENT;
        }
        if name.contains(&b'/') {
            return if libc::access(prog, libc::X_OK) == 0 {
                0
            } else {
                errno()
            };
        }
        let path = libc::getenv(c"PATH".as_ptr());
        let path: &[u8] = if path.is_null() {
            b"/system/bin:/system/xbin"
        } else {
            core::ffi::CStr::from_ptr(path).to_bytes()
        };
        let mut buf = [0u8; PATH_MAX];
        let mut denied = false;
        for dir in path.split(|&c| c == b':') {
            let dir: &[u8] = if dir.is_empty() { b"." } else { dir };
            if dir.len() + 1 + name.len() + 1 > buf.len() {
                continue;
            }
            buf[..dir.len()].copy_from_slice(dir);
            buf[dir.len()] = b'/';
            buf[dir.len() + 1..dir.len() + 1 + name.len()].copy_from_slice(name);
            buf[dir.len() + 1 + name.len()] = 0;
            let p = buf.as_ptr().cast::<libc::c_char>();
            if libc::access(p, libc::X_OK) == 0 {
                return 0;
            }
            denied |= errno() == libc::EACCES;
        }
        if denied { libc::EACCES } else { libc::ENOENT }
    }
}

/// `nabi_os_spawn` by `fork` and `execvpe` (Android without
/// `posix_spawn_file_actions_addchdir_np`): the child sets up stdio, the
/// process group and the directory, then execs; an exec failure comes
/// back through a close-on-exec pipe. The pid, or the errno.
#[cfg(target_os = "android")]
unsafe fn fork_exec(
    av: *mut CStrPtr,
    env: *const CStrPtr,
    cwd: CStrPtr,
    modes: [i32; 3],
    ends: &[[i32; 2]; 3],
    pgid: i32,
) -> Result<libc::pid_t, i32> {
    unsafe extern "C" {
        fn execvpe(file: CStrPtr, argv: *const CStrPtr, envp: *const CStrPtr) -> libc::c_int;
    }
    let mut ep = [0i32; 2];
    // SAFETY: plain calls; the child makes only async-signal-safe ones
    // before exec or `_exit`.
    unsafe {
        if libc::pipe2(ep.as_mut_ptr(), libc::O_CLOEXEC) < 0 {
            return Err(errno());
        }
        let pid = libc::fork();
        if pid < 0 {
            let e = errno();
            libc::close(ep[0]);
            libc::close(ep[1]);
            return Err(e);
        }
        if pid == 0 {
            let mut ok = true;
            for (i, m) in modes.iter().enumerate() {
                let fd = i as i32;
                let src = match *m {
                    -1 => continue,
                    -2 => {
                        let fl = if i == 0 {
                            libc::O_RDONLY
                        } else {
                            libc::O_WRONLY
                        };
                        libc::open(c"/dev/null".as_ptr(), fl)
                    }
                    -3 => ends[i][0],
                    src => src,
                };
                ok &= src >= 0 && libc::dup2(src, fd) >= 0;
            }
            ok = ok
                && (pgid < 0 || libc::setpgid(0, pgid) == 0)
                && libc::chdir(cwd) == 0
                && execvpe(*av, av.cast_const(), env) == 0;
            let e: i32 = if ok { 0 } else { errno() };
            libc::write(ep[1], (&raw const e).cast(), 4);
            libc::_exit(127);
        }
        libc::close(ep[1]);
        let mut e = 0i32;
        let n = loop {
            let n = libc::read(ep[0], (&raw mut e).cast(), 4);
            if n >= 0 || errno() != libc::EINTR {
                break n;
            }
        };
        libc::close(ep[0]);
        if n == 4 {
            let mut st = 0;
            libc::waitpid(pid, &mut st, 0);
            return Err(e);
        }
        Ok(pid)
    }
}

/// `posix_spawnp`. `argv` / `envs` hold `argc` / `envc`
/// NUL-terminated pieces (`envc < 0`: inherit the environment; the
/// program is looked up on this process's `PATH` either way); `cwd`
/// empty = inherit; `pgid >= 0` joins that process group (0: a new one
/// led by the child). Stdio modes: `-1` inherit, `-2` null, `-3` piped,
/// `>= 0` that fd. The pipe ends (or -1) go to `out[0..3]` (stdin write
/// end, stdout / stderr read ends). Returns the pid or `-errno` (exec
/// failures included).
#[cfg(unix)]
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn nabi_os_spawn(
    argv: NabiStr,
    argc: usize,
    envs: NabiStr,
    envc: isize,
    cwd: NabiStr,
    stdio_in: i32,
    stdio_out: i32,
    stdio_err: i32,
    pgid: i32,
    out: *mut u8,
) -> i32 {
    let av = split_blob(argv, argc);
    let ev = if envc >= 0 {
        split_blob(envs, envc as usize)
    } else {
        ptr::null_mut()
    };
    let cwd_c = cs(cwd);
    let modes = [stdio_in, stdio_out, stdio_err];
    // [child end, parent end] per stream.
    let mut ends = [[-1i32; 2]; 3];
    let r = native(|| {
        // SAFETY: pipe / fcntl / posix_spawn on locals; the actions and
        // attributes are initialized before use and destroyed after.
        unsafe {
            for (i, m) in modes.iter().enumerate() {
                if *m == -3 {
                    let mut p = [0i32; 2];
                    if libc::pipe(p.as_mut_ptr()) < 0 {
                        return -errno();
                    }
                    // `dup2` onto 0..3 clears it on the child's copy.
                    libc::fcntl(p[0], libc::F_SETFD, libc::FD_CLOEXEC);
                    libc::fcntl(p[1], libc::F_SETFD, libc::FD_CLOEXEC);
                    ends[i] = if i == 0 { [p[0], p[1]] } else { [p[1], p[0]] };
                }
            }
            let mut fa: libc::posix_spawn_file_actions_t = core::mem::zeroed();
            let mut at: libc::posix_spawnattr_t = core::mem::zeroed();
            posix_spawn_file_actions_init(&mut fa);
            posix_spawnattr_init(&mut at);
            let mut rc = 0;
            for (i, m) in modes.iter().enumerate() {
                let fd = i as i32;
                rc = match *m {
                    -1 => 0,
                    -2 => {
                        let fl = if i == 0 {
                            libc::O_RDONLY
                        } else {
                            libc::O_WRONLY
                        };
                        posix_spawn_file_actions_addopen(&mut fa, fd, c"/dev/null".as_ptr(), fl, 0)
                    }
                    -3 => posix_spawn_file_actions_adddup2(&mut fa, ends[i][0], fd),
                    src => posix_spawn_file_actions_adddup2(&mut fa, src, fd),
                };
                if rc != 0 {
                    break;
                }
            }
            // bionic before API 34 cannot change directory in
            // posix_spawn: fork and exec there.
            #[cfg(target_os = "android")]
            let mut forked = false;
            if rc == 0 && cwd.len > 0 {
                rc = posix_spawn_file_actions_addchdir_np(&mut fa, cwd_c.p());
                #[cfg(target_os = "android")]
                if rc == libc::ENOSYS {
                    (rc, forked) = (0, true);
                }
            }
            if rc == 0 && pgid >= 0 {
                rc = posix_spawnattr_setflags(&mut at, libc::POSIX_SPAWN_SETPGROUP as _);
                if rc == 0 {
                    rc = posix_spawnattr_setpgroup(&mut at, pgid);
                }
            }
            let mut pid: libc::pid_t = 0;
            // bionic's posix_spawnp reports a program it cannot exec only
            // as the child's exit (127): look it up first.
            #[cfg(target_os = "android")]
            if rc == 0 {
                rc = find_program(*av);
            }
            if rc == 0 {
                let env = if ev.is_null() {
                    *environ()
                } else {
                    ev.cast_const()
                };
                #[cfg(target_os = "android")]
                if forked {
                    match fork_exec(av, env, cwd_c.p(), modes, &ends, pgid) {
                        Ok(p) => pid = p,
                        Err(e) => rc = e,
                    }
                } else {
                    rc = posix_spawnp(&mut pid, *av, &fa, &at, av.cast(), env.cast());
                }
                #[cfg(not(target_os = "android"))]
                {
                    rc = posix_spawnp(&mut pid, *av, &fa, &at, av.cast(), env.cast());
                }
            }
            posix_spawn_file_actions_destroy(&mut fa);
            posix_spawnattr_destroy(&mut at);
            for e in &ends {
                if e[0] >= 0 {
                    libc::close(e[0]);
                }
            }
            if rc != 0 { -rc } else { pid }
        }
    })
    .0;
    // SAFETY: malloc blocks from `split_blob`.
    unsafe {
        libc::free(av.cast());
        libc::free(ev.cast());
    }
    for (i, e) in ends.iter().enumerate() {
        if r < 0 && e[1] >= 0 {
            // SAFETY: our own pipe end.
            unsafe { libc::close(e[1]) };
        }
        // SAFETY: the library passes a live `GcBuf<i32>` of 3 elements.
        unsafe { *cell(out, i) = if r < 0 { -1 } else { e[1] } };
    }
    r
}

#[cfg(target_os = "wasi")]
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn nabi_os_spawn(
    _argv: NabiStr,
    _argc: usize,
    _envs: NabiStr,
    _envc: isize,
    _cwd: NabiStr,
    _stdio_in: i32,
    _stdio_out: i32,
    _stdio_err: i32,
    _pgid: i32,
    out: *mut u8,
) -> i32 {
    for i in 0..3 {
        // SAFETY: the library passes a live `GcBuf<i32>` of 3 elements.
        unsafe { *cell(out, i) = -1 };
    }
    UNSUPPORTED
}

/// `waitpid`: the pid (status in `out[0]`), 0 if `nohang` and still
/// running, or `-errno`. `nohang` bit 0: do not block; bit 1: only wait
/// for the end, leave the child to reap (`out` untouched).
#[cfg(unix)]
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_waitpid(pid: i32, nohang: i32, out: *mut u8) -> i32 {
    if nohang & 2 != 0 {
        return wait_ended(pid, nohang & 1 != 0);
    }
    let mut st = 0;
    let flags = if nohang != 0 { libc::WNOHANG } else { 0 };
    let (r, e) = native(|| {
        loop {
            // SAFETY: plain libc call.
            let r = unsafe { libc::waitpid(pid, &mut st, flags) };
            if r >= 0 || errno() != libc::EINTR {
                break r;
            }
        }
    });
    if r < 0 {
        return -e;
    }
    // SAFETY: a live one-element `GcBuf<i32>`.
    unsafe { *cell(out, 0) = st };
    r
}

/// `waitid(WNOWAIT)`: the pid once it has ended, still a zombie, 0 if
/// `nohang` and running, or `-errno`.
#[cfg(unix)]
pub(crate) fn wait_ended(pid: i32, nohang: bool) -> i32 {
    let flags = libc::WEXITED | libc::WNOWAIT | if nohang { libc::WNOHANG } else { 0 };
    let (r, e) = native(|| {
        loop {
            // SAFETY: a zeroed `siginfo_t` is valid; `waitid` fills it.
            let mut info: libc::siginfo_t = unsafe { core::mem::zeroed() };
            // SAFETY: plain libc call into `info`.
            let r = unsafe { libc::waitid(libc::P_PID, pid as libc::id_t, &raw mut info, flags) };
            if r >= 0 {
                // SAFETY: filled by `waitid`, or still zeroed (WNOHANG).
                break unsafe { info.si_pid() };
            }
            if errno() != libc::EINTR {
                break r;
            }
        }
    });
    if r < 0 { -e } else { r }
}

#[cfg(target_os = "wasi")]
#[unsafe(no_mangle)]
pub const extern "C" fn nabi_os_waitpid(_pid: i32, _nohang: i32, _out: *mut u8) -> i32 {
    UNSUPPORTED
}

/// Write end of the signal pipe (`nabi_os_signal_install`), or -1.
#[cfg(unix)]
pub(crate) static SIGNAL_PIPE: core::sync::atomic::AtomicI32 =
    core::sync::atomic::AtomicI32::new(-1);

/// The C handler: the signal number as one byte down the pipe. Only
/// async-signal-safe calls; a full pipe drops the signal.
#[cfg(unix)]
pub(crate) extern "C" fn on_signal(sig: libc::c_int) {
    let fd = SIGNAL_PIPE.load(core::sync::atomic::Ordering::Relaxed);
    if fd >= 0 {
        let saved = errno();
        let b = sig as u8;
        // SAFETY: one byte from a local; `write` is async-signal-safe.
        unsafe {
            libc::write(fd, (&raw const b).cast(), 1);
            set_errno(saved);
        }
    }
}

/// Route signal `sig` to the signal pipe (`process::on_signal`). The first
/// call creates the pipe; `*read_end` gets its read end (blocking,
/// close-on-exec) on that call, -1 after. 0 or `-errno` (`EINVAL` for
/// `SIGKILL` / `SIGSTOP` / a bad number).
#[cfg(unix)]
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_signal_install(sig: i32, read_end: *mut u8) -> i32 {
    // SAFETY: the library passes a live `GcBuf<i32>` of 1 element.
    unsafe { *cell(read_end, 0) = -1 };
    if !(1..64).contains(&sig) || sig == libc::SIGKILL || sig == libc::SIGSTOP {
        return -libc::EINVAL;
    }
    if SIGNAL_PIPE.load(core::sync::atomic::Ordering::Acquire) < 0 {
        let mut p = [0i32; 2];
        // SAFETY: `p` is writable; fcntl on our own fds.
        unsafe {
            if libc::pipe(p.as_mut_ptr()) < 0 {
                return -errno();
            }
            for fd in p {
                libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
            }
            let fl = libc::fcntl(p[1], libc::F_GETFL);
            libc::fcntl(p[1], libc::F_SETFL, fl | libc::O_NONBLOCK);
            *cell(read_end, 0) = p[0];
        }
        SIGNAL_PIPE.store(p[1], core::sync::atomic::Ordering::Release);
    }
    // SAFETY: a zeroed `sigaction` with the fields set below is valid.
    unsafe {
        let mut sa: libc::sigaction = core::mem::zeroed();
        sa.sa_sigaction = on_signal as *const () as libc::sighandler_t;
        sa.sa_flags = libc::SA_RESTART;
        libc::sigemptyset(&mut sa.sa_mask);
        if libc::sigaction(sig, &sa, ptr::null_mut()) < 0 {
            return -errno();
        }
    }
    0
}

#[cfg(target_os = "wasi")]
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_signal_install(_sig: i32, read_end: *mut u8) -> i32 {
    // SAFETY: the library passes a live `GcBuf<i32>` of 1 element.
    unsafe { *cell(read_end, 0) = -1 };
    UNSUPPORTED
}

#[cfg(unix)]
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_raise(sig: i32) -> i32 {
    // SAFETY: plain libc call.
    if unsafe { libc::raise(sig) } != 0 {
        -libc::EINVAL
    } else {
        0
    }
}

#[cfg(target_os = "wasi")]
#[unsafe(no_mangle)]
pub const extern "C" fn nabi_os_raise(_sig: i32) -> i32 {
    UNSUPPORTED
}

#[cfg(unix)]
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_kill(pid: i32, sig: i32) -> i32 {
    // SAFETY: plain libc call.
    ret(unsafe { libc::kill(pid, sig) } as isize) as i32
}

#[cfg(target_os = "wasi")]
#[unsafe(no_mangle)]
pub const extern "C" fn nabi_os_kill(_pid: i32, _sig: i32) -> i32 {
    UNSUPPORTED
}

/// `pidfd_open(pid)` (Linux 5.3+): a close-on-exec fd tracking the
/// process, or `-errno`; `-ENOSYS` on other systems.
#[unsafe(no_mangle)]
#[cfg_attr(not(target_os = "linux"), allow(clippy::missing_const_for_fn))]
pub extern "C" fn nabi_os_pidfd_open(pid: i32) -> i32 {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: plain syscall.
        ret(unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) } as isize) as i32
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        -libc::ENOSYS
    }
}

/// `pidfd_send_signal(fd, sig)`: 0 or `-errno` (`-ENOSYS` off Linux).
#[unsafe(no_mangle)]
#[cfg_attr(not(target_os = "linux"), allow(clippy::missing_const_for_fn))]
pub extern "C" fn nabi_os_pidfd_signal(fd: i32, sig: i32) -> i32 {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: plain syscall; no siginfo.
        let r =
            unsafe { libc::syscall(libc::SYS_pidfd_send_signal, fd, sig, ptr::null::<u8>(), 0) };
        ret(r as isize) as i32
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (fd, sig);
        -libc::ENOSYS
    }
}

#[cfg(unix)]
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_getpid() -> u32 {
    // SAFETY: plain libc call.
    unsafe { libc::getpid() as u32 }
}

#[cfg(target_os = "wasi")]
#[unsafe(no_mangle)]
pub const extern "C" fn nabi_os_getpid() -> u32 {
    0
}

#[cfg(unix)]
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_getppid() -> u32 {
    // SAFETY: plain libc call.
    unsafe { libc::getppid() as u32 }
}

#[cfg(target_os = "wasi")]
#[unsafe(no_mangle)]
pub const extern "C" fn nabi_os_getppid() -> u32 {
    0
}

/// Abnormal termination (`SIGABRT`), no cleanup.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_abort() -> ! {
    // SAFETY: never returns.
    unsafe { libc::abort() }
}
