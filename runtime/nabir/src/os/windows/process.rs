//! Processes, child pipes and console signals.

#[allow(clippy::wildcard_imports)]
use crate::os::windows::*;

/// A pipe for a child's output: (our read end, overlapped, so
/// [`nabi_os_poll_read`] waits on two at once; the child's write end).
pub(crate) fn output_pipe() -> Result<(HANDLE, HANDLE), i32> {
    static N: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
    // `\\.\pipe\nabi-<pid>-<n>`, NUL-terminated.
    let mut name = [0u16; 48];
    let prefix = ascii_w(br"\\.\pipe\nabi-");
    name[..prefix.len()].copy_from_slice(&prefix);
    let mut at = prefix.len();
    // SAFETY: plain call.
    let pid = unsafe { win::GetCurrentProcessId() };
    for (j, v) in [pid, N.fetch_add(1, Ordering::Relaxed)]
        .into_iter()
        .enumerate()
    {
        if j > 0 {
            name[at] = u16::from(b'-');
            at += 1;
        }
        let mut digits = [0u16; 10];
        let mut i = digits.len();
        let mut v = v;
        loop {
            i -= 1;
            digits[i] = u16::from(b'0') + (v % 10) as u16;
            v /= 10;
            if v == 0 {
                break;
            }
        }
        name[at..at + digits.len() - i].copy_from_slice(&digits[i..]);
        at += digits.len() - i;
    }
    // SAFETY: a NUL-terminated name; one inbound byte-mode instance.
    unsafe {
        let ours = win::CreateNamedPipeW(
            name.as_ptr(),
            win::PIPE_ACCESS_INBOUND
                | win::FILE_FLAG_OVERLAPPED
                | win::FILE_FLAG_FIRST_PIPE_INSTANCE,
            win::PIPE_REJECT_REMOTE_CLIENTS,
            1,
            1 << 16,
            1 << 16,
            0,
            ptr::null_mut(),
        );
        if ours == win::INVALID_HANDLE_VALUE {
            return Err(fail());
        }
        let child = win::CreateFileW(
            name.as_ptr(),
            win::GENERIC_WRITE,
            0,
            ptr::null_mut(),
            win::OPEN_EXISTING,
            0,
            0,
        );
        if child == win::INVALID_HANDLE_VALUE {
            let e = fail();
            win::CloseHandle(ours);
            return Err(e);
        }
        Ok((ours, child))
    }
}

/// Children not reaped yet: `(pid, process handle)`.
pub(crate) struct Children {
    lock: sys::Lock,
    list: UnsafeCell<MBuf<(u32, HANDLE)>>,
}

// SAFETY: `list` is only touched with `lock` held.
unsafe impl Sync for Children {}

pub(crate) static CHILDREN: Children = Children {
    lock: sys::Lock::new(),
    list: UnsafeCell::new(MBuf::new()),
};

/// With the children lock held: `f` on the list.
pub(crate) fn children<R>(f: impl FnOnce(&mut MBuf<(u32, HANDLE)>) -> R) -> R {
    CHILDREN.lock.lock();
    // SAFETY: the lock is held.
    let r = f(unsafe { &mut *CHILDREN.list.get() });
    CHILDREN.lock.unlock();
    r
}

/// The handle of child `pid`, if it has not been reaped.
pub(crate) fn child(pid: u32) -> Option<HANDLE> {
    children(|l| l.as_slice().iter().find(|c| c.0 == pid).map(|c| c.1))
}

/// The bytes of `s` for the length of a call (the caller roots `s`).
///
/// # Safety
/// The slice must not outlive `s`.
pub(crate) const unsafe fn statics(s: NabiStr) -> &'static [u8] {
    if s.len == 0 {
        return &[];
    }
    // SAFETY: caller contract.
    unsafe { core::slice::from_raw_parts(s.ptr, s.len) }
}

/// `name` with `ext` (ASCII, lower case) as its extension, any case.
pub(crate) fn has_ext(name: &[u8], ext: &[u8]) -> bool {
    name.len() >= ext.len() && name[name.len() - ext.len()..].eq_ignore_ascii_case(ext)
}

/// The program to start: `prog` itself when it holds a separator or a
/// drive, else the first `<dir>\prog` (or `prog.exe` without an
/// extension) that exists along `PATH`.
pub(crate) fn find_program(prog: &[u8]) -> Result<Wide, i32> {
    if prog.iter().any(|&b| b == b'/' || b == b'\\' || b == b':') {
        return Ok(wide(prog));
    }
    let name = ascii_w(b"PATH\0");
    // SAFETY: `b` is writable for `n` units; `name` is NUL-terminated.
    let path = fill_wide(|b, n| unsafe { win::GetEnvironmentVariableW(name.as_ptr(), b, n) })
        .unwrap_or_else(|_| Wide::new());
    let has_dot = prog.contains(&b'.');
    for dir in path.as_slice().split(|&u| u == u16::from(b';')) {
        if dir.is_empty() {
            continue;
        }
        for ext in [&b""[..], b".exe"] {
            if has_dot && !ext.is_empty() {
                continue;
            }
            let mut c = Wide::new();
            c.extend(dir);
            if !dir.ends_with(&[u16::from(b'\\')]) {
                c.push(u16::from(b'\\'));
            }
            push_wide(&mut c, prog);
            push_wide(&mut c, ext);
            // SAFETY: a NUL-terminated path.
            let a = unsafe { win::GetFileAttributesW(c.ptr()) };
            if a != win::INVALID_FILE_ATTRIBUTES && a & win::FILE_ATTRIBUTE_DIRECTORY == 0 {
                return Ok(c);
            }
        }
    }
    Err(-win::ERROR_FILE_NOT_FOUND)
}

/// An inheritable copy of `hd` (0 when `hd` is 0 / invalid).
pub(crate) fn inheritable(hd: HANDLE) -> Result<HANDLE, i32> {
    if hd == 0 || hd == win::INVALID_HANDLE_VALUE {
        return Ok(0);
    }
    let mut out = 0;
    // SAFETY: duplicates within this process.
    let ok = unsafe {
        let me = win::GetCurrentProcess();
        win::DuplicateHandle(me, hd, me, &mut out, 0, 1, win::DUPLICATE_SAME_ACCESS)
    };
    if ok == 0 { Err(fail()) } else { Ok(out) }
}

/// `CreateProcessW`. `argv` / `envs` hold `argc` / `envc` NUL-terminated
/// pieces (`envc < 0`: inherit the environment); `cwd` empty = inherit;
/// `pgid` 0 starts a new process group (other values: unsupported).
/// Stdio modes: `-1` inherit, `-2` null, `-3` piped, `>= 0` that fd. The
/// pipe ends (or -1) go to `out[0..3]` (stdin write end, stdout / stderr
/// read ends). Returns the pid or `-error`.
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
    for i in 0..3 {
        // SAFETY: the library passes a live `GcBuf<i32>` of 3 elements.
        unsafe { *cell(out, i) = -1 };
    }
    if pgid > 0 {
        return -win::ERROR_NOT_SUPPORTED;
    }
    let pieces = |s: &'static [u8], n: usize| s.split(|&b| b == 0).take(n);
    // SAFETY: the library keeps the blobs alive across the call.
    let (argv, envs) = unsafe { (statics(argv), statics(envs)) };
    let Some(prog) = pieces(argv, argc).next() else {
        return -win::ERROR_INVALID_PARAMETER;
    };
    // `cmd.exe` re-parses a batch file's command line: refuse what it
    // would expand or split on.
    if (has_ext(prog, b".bat") || has_ext(prog, b".cmd"))
        && pieces(argv, argc)
            .skip(1)
            .any(|a| a.iter().any(|b| matches!(b, b'"' | b'%' | b'\r' | b'\n')))
    {
        return -win::ERROR_INVALID_PARAMETER;
    }
    let mut app = match find_program(prog) {
        Ok(a) => a,
        Err(e) => return e,
    };
    let mut cmd = Wide::new();
    for a in pieces(argv, argc) {
        push_arg(&mut cmd, wide(a).as_slice());
    }
    let mut env = Wide::new();
    if envc >= 0 {
        for kv in pieces(envs, envc as usize) {
            push_wide(&mut env, kv);
            env.push(0);
        }
        if envc == 0 {
            env.push(0);
        }
        env.push(0);
    }
    let mut dir = wide(cwd.bytes());

    // [child end, parent end] per stream; the child ends are inheritable.
    let mut ends = [[0 as HANDLE; 2]; 3];
    let r = setup_stdio([stdio_in, stdio_out, stdio_err], &mut ends);
    let r = r.and_then(|()| {
        // SAFETY: the buffers above outlive the call; the attribute list
        // is sized by the first call.
        unsafe {
            create(
                &mut app,
                &mut cmd,
                &mut env,
                envc >= 0,
                &mut dir,
                cwd.len > 0,
                pgid == 0,
                &ends,
            )
        }
    });
    for (i, e) in ends.iter().enumerate() {
        // SAFETY: our own handles.
        unsafe {
            if e[0] != 0 {
                win::CloseHandle(e[0]);
            }
            if r.is_err() && e[1] != 0 {
                win::CloseHandle(e[1]);
            }
            if r.is_ok() && e[1] != 0 {
                *cell(out, i) = e[1] as i32;
            }
        }
    }
    match r {
        Ok(pid) => pid as i32,
        Err(e) => e,
    }
}

/// The child's standard handles into `ends` (see [`nabi_os_spawn`]).
pub(crate) fn setup_stdio(modes: [i32; 3], ends: &mut [[HANDLE; 2]; 3]) -> Result<(), i32> {
    const STD: [DWORD; 3] = [
        win::STD_INPUT_HANDLE,
        win::STD_OUTPUT_HANDLE,
        win::STD_ERROR_HANDLE,
    ];
    for (i, &m) in modes.iter().enumerate() {
        ends[i][0] = match m {
            // SAFETY: plain query.
            -1 => inheritable(unsafe { win::GetStdHandle(STD[i]) })?,
            -2 => {
                let access = if i == 0 {
                    win::GENERIC_READ
                } else {
                    win::GENERIC_WRITE
                };
                let mut sa = win::SECURITY_ATTRIBUTES {
                    nLength: size_of::<win::SECURITY_ATTRIBUTES>() as DWORD,
                    lpSecurityDescriptor: ptr::null_mut(),
                    bInheritHandle: 1,
                };
                let nul = [u16::from(b'N'), u16::from(b'U'), u16::from(b'L'), 0];
                // SAFETY: a NUL-terminated name.
                let hd = unsafe {
                    win::CreateFileW(
                        nul.as_ptr(),
                        access,
                        win::FILE_SHARE_READ | win::FILE_SHARE_WRITE,
                        &mut sa,
                        win::OPEN_EXISTING,
                        0,
                        0,
                    )
                };
                if hd == win::INVALID_HANDLE_VALUE {
                    return Err(fail());
                }
                hd
            }
            -3 => {
                let (child, parent) = if i == 0 {
                    let (mut r, mut w) = (0, 0);
                    // SAFETY: valid out-pointers; neither end is inheritable.
                    if unsafe { win::CreatePipe(&mut r, &mut w, ptr::null_mut(), 0) } == 0 {
                        return Err(fail());
                    }
                    (r, w)
                } else {
                    let (ours, theirs) = output_pipe()?;
                    (theirs, ours)
                };
                ends[i][1] = parent;
                let c = inheritable(child);
                // SAFETY: ours; `c` is the inheritable copy.
                unsafe { win::CloseHandle(child) };
                c?
            }
            fd => inheritable(h(fd))?,
        };
    }
    Ok(())
}

/// `CreateProcessW` with exactly the `ends` child handles inherited.
#[allow(clippy::too_many_arguments, clippy::fn_params_excessive_bools)]
pub(crate) unsafe fn create(
    app: &mut Wide,
    cmd: &mut Wide,
    env: &mut Wide,
    own_env: bool,
    dir: &mut Wide,
    own_dir: bool,
    new_group: bool,
    ends: &[[HANDLE; 2]; 3],
) -> Result<u32, i32> {
    let mut list = [0 as HANDLE; 3];
    let mut n = 0;
    for e in ends {
        if e[0] != 0 && !list[..n].contains(&e[0]) {
            list[n] = e[0];
            n += 1;
        }
    }
    // SAFETY: the attribute list is sized by the first call, built in a
    // malloc block, deleted and freed before returning.
    unsafe {
        let mut size = 0;
        win::InitializeProcThreadAttributeList(ptr::null_mut(), 1, 0, &mut size);
        let attrs = libc::malloc(size);
        if attrs.is_null() {
            nabir_common::oom();
        }
        let r = if win::InitializeProcThreadAttributeList(attrs, 1, 0, &mut size) == 0 {
            Err(fail())
        } else {
            let r = if n == 0
                || win::UpdateProcThreadAttribute(
                    attrs,
                    0,
                    win::PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
                    list.as_ptr().cast(),
                    n * size_of::<HANDLE>(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                ) != 0
            {
                let mut si: win::STARTUPINFOEXW = zeroed();
                si.StartupInfo.cb = size_of::<win::STARTUPINFOEXW>() as DWORD;
                si.StartupInfo.dwFlags = win::STARTF_USESTDHANDLES;
                si.StartupInfo.hStdInput = ends[0][0];
                si.StartupInfo.hStdOutput = ends[1][0];
                si.StartupInfo.hStdError = ends[2][0];
                si.lpAttributeList = if n == 0 { ptr::null_mut() } else { attrs };
                let mut flags = win::CREATE_UNICODE_ENVIRONMENT | win::EXTENDED_STARTUPINFO_PRESENT;
                if new_group {
                    flags |= win::CREATE_NEW_PROCESS_GROUP;
                }
                let mut pi: win::PROCESS_INFORMATION = zeroed();
                let cmd_p = cmd.ptr().cast_mut();
                let env_p = if own_env {
                    env.ptr().cast_mut().cast()
                } else {
                    ptr::null_mut()
                };
                let dir_p = if own_dir { dir.ptr() } else { ptr::null() };
                let app_p = app.ptr();
                let (ok, e) = native(|| {
                    win::CreateProcessW(
                        app_p,
                        cmd_p,
                        ptr::null_mut(),
                        ptr::null_mut(),
                        i32::from(n > 0),
                        flags,
                        env_p,
                        dir_p,
                        (&raw mut si).cast(),
                        &mut pi,
                    )
                });
                if ok == 0 {
                    Err(-e)
                } else {
                    win::CloseHandle(pi.hThread);
                    children(|l| l.push((pi.dwProcessId, pi.hProcess)));
                    Ok(pi.dwProcessId)
                }
            } else {
                Err(fail())
            };
            win::DeleteProcThreadAttributeList(attrs);
            r
        };
        libc::free(attrs);
        r
    }
}

/// Wait for child `pid`: the pid (its exit code in `out[0]`), 0 if
/// `nohang` and still running, or `-error`. `nohang` bit 0: do not
/// block; bit 1: only wait for the end, keep the handle (`out`
/// untouched).
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_waitpid(pid: i32, nohang: i32, out: *mut u8) -> i32 {
    let Some(hd) = child(pid as u32) else {
        return -win::ERROR_INVALID_PARAMETER;
    };
    let ms = if nohang & 1 != 0 { 0 } else { win::INFINITE };
    // SAFETY: a live process handle (only this call closes it).
    let (w, e) = native(|| unsafe { win::WaitForSingleObject(hd, ms) });
    if w == win::WAIT_TIMEOUT {
        return 0;
    }
    if w != win::WAIT_OBJECT_0 {
        return -e;
    }
    if nohang & 2 != 0 {
        return pid;
    }
    let mut code = 0;
    // SAFETY: as above.
    if unsafe { win::GetExitCodeProcess(hd, &mut code) } == 0 {
        return fail();
    }
    children(|l| {
        if let Some(i) = l.as_slice().iter().position(|c| c.0 == pid as u32) {
            let last = l.len() - 1;
            l.as_mut_slice().swap(i, last);
            l.truncate(last);
            // SAFETY: the handle leaves the list here, once.
            unsafe { win::CloseHandle(hd) };
        }
    });
    // SAFETY: a live one-element `GcBuf<i32>`.
    unsafe { *cell(out, 0) = code as i32 };
    pid
}

/// Console control events routed to the signal pipe: bit `sig` set.
pub(crate) static SIGNALS: AtomicU32 = AtomicU32::new(0);

/// Write end of the signal pipe (`nabi_os_signal_install`), or 0.
pub(crate) static SIGNAL_PIPE: AtomicUsize = AtomicUsize::new(0);

/// The signal number of console event `ev`.
pub(crate) const fn event_signal(ev: DWORD) -> i32 {
    match ev {
        win::CTRL_C_EVENT => 2,
        win::CTRL_BREAK_EVENT => 21,
        win::CTRL_CLOSE_EVENT | win::CTRL_LOGOFF_EVENT | win::CTRL_SHUTDOWN_EVENT => 1,
        _ => 0,
    }
}

/// Send `sig` down the signal pipe if it is routed there.
pub(crate) fn deliver(sig: i32) -> bool {
    let pipe = SIGNAL_PIPE.load(Ordering::Acquire) as HANDLE;
    if pipe == 0 || sig <= 0 || SIGNALS.load(Ordering::Acquire) & (1 << sig) == 0 {
        return false;
    }
    let b = sig as u8;
    let mut n = 0;
    // SAFETY: one byte from a local to our pipe.
    unsafe { win::WriteFile(pipe, (&raw const b).cast(), 1, &mut n, ptr::null_mut()) };
    true
}

/// The console control handler (on a thread the system starts): the
/// event's signal down the pipe when routed, else the default action.
/// After a close / logoff / shutdown event the system ends the process
/// once handlers return, so the nabi handler may not get to run.
pub(crate) unsafe extern "system" fn on_ctrl(ev: DWORD) -> win::BOOL {
    i32::from(deliver(event_signal(ev)))
}

/// Flush stdout on a console event, then pass it on: to a later
/// handler that routes it, else the default one, which ends the
/// process.
unsafe extern "system" fn flush_ctrl(_ev: DWORD) -> win::BOOL {
    crate::stdio::flush_in_signal();
    0
}

/// Flush stdout when Ctrl-C, Ctrl-Break or a console close ends the
/// process.
pub(crate) fn flush_on_kill() {
    // SAFETY: the handler is a plain function.
    unsafe { win::SetConsoleCtrlHandler(Some(flush_ctrl), 1) };
}

/// Route `sig` (2 `SIGINT`: Ctrl-C, 21 `SIGBREAK`: Ctrl-Break, 1
/// `SIGHUP`: console close / logoff / shutdown) to the signal pipe. The
/// first call creates the pipe; `*read_end` gets its read end on that
/// call, -1 after. 0 or `-error` (`ERROR_NOT_SUPPORTED` for other
/// numbers).
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_signal_install(sig: i32, read_end: *mut u8) -> i32 {
    // SAFETY: the library passes a live `GcBuf<i32>` of 1 element.
    unsafe { *cell(read_end, 0) = -1 };
    if !matches!(sig, 1 | 2 | 21) {
        return -win::ERROR_NOT_SUPPORTED;
    }
    if SIGNAL_PIPE.load(Ordering::Acquire) == 0 {
        let (mut r, mut w) = (0, 0);
        // SAFETY: valid out-pointers; the handler is a plain function.
        unsafe {
            if win::CreatePipe(&mut r, &mut w, ptr::null_mut(), 0) == 0 {
                return fail();
            }
            if win::SetConsoleCtrlHandler(Some(on_ctrl), 1) == 0 {
                let e = fail();
                win::CloseHandle(r);
                win::CloseHandle(w);
                return e;
            }
            *cell(read_end, 0) = r as i32;
        }
        SIGNAL_PIPE.store(w as usize, Ordering::Release);
    }
    SIGNALS.fetch_or(1 << sig, Ordering::AcqRel);
    0
}

/// `sig` to this process: down the signal pipe when routed, else the
/// CRT's `raise` (its default action ends the process).
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_raise(sig: i32) -> i32 {
    if deliver(sig) {
        return 0;
    }
    // The CRT's default actions end the process without `atexit`.
    crate::stdio::flush_in_signal();
    // SAFETY: plain CRT call.
    if unsafe { libc::raise(sig) } != 0 {
        -win::ERROR_INVALID_PARAMETER
    } else {
        0
    }
}

/// Whether `TerminateProcess` failed only because `hd` has already
/// ended: Windows refuses it with access denied then, where POSIX
/// signals a zombie without complaint.
pub(crate) fn exited(hd: HANDLE) -> bool {
    if errno() != win::ERROR_ACCESS_DENIED {
        return false;
    }
    let mut code = 0;
    // SAFETY: a live process handle. The wait catches a real exit code
    // of `STILL_ACTIVE`.
    unsafe {
        win::GetExitCodeProcess(hd, &mut code) != 0
            && (code != win::STILL_ACTIVE || win::WaitForSingleObject(hd, 0) == win::WAIT_OBJECT_0)
    }
}

/// End process `pid` (`TerminateProcess`, exit code 1) for any `sig`
/// but 0, which only checks that the process exists.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_kill(pid: i32, sig: i32) -> i32 {
    let mine = child(pid as u32);
    let hd = match mine {
        Some(hd) => hd,
        // SAFETY: plain call; closed below.
        None => unsafe {
            win::OpenProcess(
                win::PROCESS_TERMINATE | win::PROCESS_QUERY_LIMITED_INFORMATION | win::SYNCHRONIZE,
                0,
                pid as DWORD,
            )
        },
    };
    if hd == 0 {
        return fail();
    }
    // SAFETY: a live process handle.
    let r = if sig == 0 || unsafe { win::TerminateProcess(hd, 1) } != 0 || exited(hd) {
        0
    } else {
        fail()
    };
    if mine.is_none() {
        // SAFETY: ours, from `OpenProcess`.
        unsafe { win::CloseHandle(hd) };
    }
    r
}

/// No pidfds on Windows: `-ERROR_NOT_SUPPORTED` (`Child` falls back to
/// its pid).
#[unsafe(no_mangle)]
pub const extern "C" fn nabi_os_pidfd_open(_pid: i32) -> i32 {
    -win::ERROR_NOT_SUPPORTED
}

#[unsafe(no_mangle)]
pub const extern "C" fn nabi_os_pidfd_signal(_fd: i32, _sig: i32) -> i32 {
    -win::ERROR_NOT_SUPPORTED
}

#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_getpid() -> u32 {
    // SAFETY: no preconditions.
    unsafe { win::GetCurrentProcessId() }
}

/// The parent's pid (0 if unknown).
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_getppid() -> u32 {
    // SAFETY: zeroed info is a valid out-buffer; class 0 is
    // `ProcessBasicInformation`.
    unsafe {
        let mut pbi: win::PROCESS_BASIC_INFORMATION = zeroed();
        let s = win::NtQueryInformationProcess(
            win::GetCurrentProcess(),
            0,
            (&raw mut pbi).cast(),
            size_of::<win::PROCESS_BASIC_INFORMATION>() as u32,
            ptr::null_mut(),
        );
        if s < 0 {
            0
        } else {
            pbi.InheritedFromUniqueProcessId as u32
        }
    }
}

/// Abnormal termination, no cleanup.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_abort() -> ! {
    // SAFETY: never returns.
    unsafe { libc::abort() }
}
