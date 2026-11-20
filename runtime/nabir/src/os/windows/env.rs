//! The executable path, arguments and the environment.

#[allow(clippy::wildcard_imports)]
use crate::os::windows::*;

/// Absolute path of the running executable into `buf[off..off + len]`:
/// its full length or `-error`.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_current_exe(buf: *mut u8, off: usize, len: usize) -> isize {
    // SAFETY: `b` is writable for `n` units.
    match fill_wide(|b, n| unsafe { win::GetModuleFileNameW(0, b, n) }) {
        Ok(w) => put_wide(buf, off, len, w.as_slice()),
        Err(e) => -(e as isize),
    }
}

pub(crate) fn args() -> &'static Args {
    static ARGS: AtomicPtr<Args> = AtomicPtr::new(ptr::null_mut());
    let a = ARGS.load(Ordering::Acquire);
    if !a.is_null() {
        // SAFETY: published once, never freed.
        return unsafe { &*a };
    }
    // SAFETY: the process command line lives as long as the process.
    let line = unsafe { sys::wide_cstr(win::GetCommandLineW()) };
    let parsed = parse_command_line(line);
    // SAFETY: plain allocation, never freed.
    let p = unsafe { libc::malloc(size_of::<Args>()) }.cast::<Args>();
    if p.is_null() {
        nabir_common::oom();
    }
    // SAFETY: fresh allocation.
    unsafe { p.write(parsed) };
    match ARGS.compare_exchange(ptr::null_mut(), p, Ordering::AcqRel, Ordering::Acquire) {
        // SAFETY: just published.
        Ok(_) => unsafe { &*p },
        Err(won) => {
            // SAFETY: ours, never published; `won` lives forever.
            unsafe {
                ptr::drop_in_place(p);
                libc::free(p.cast());
                &*won
            }
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_argc() -> usize {
    args().count
}

#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_arg(i: usize) -> NabiStr {
    let a = args();
    a.bytes
        .as_slice()
        .split(|&b| b == 0)
        .nth(i)
        .filter(|_| i < a.count)
        .map_or_else(|| build(&[]), |s| build(&[s]))
}

/// Entry `i` of the environment block (`"KEY=VALUE"`), `""` past the end.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_env_at(i: usize) -> NabiStr {
    // SAFETY: a double-NUL-terminated block, freed below.
    unsafe {
        let env = win::GetEnvironmentStringsW();
        if env.is_null() {
            return build(&[]);
        }
        let mut p = env.cast_const();
        let mut k = 0;
        let mut found = MBuf::new();
        while *p != 0 {
            let entry = sys::wide_cstr(p);
            if k == i {
                found = narrow(entry);
                break;
            }
            p = p.add(entry.len() + 1);
            k += 1;
        }
        win::FreeEnvironmentStringsW(env);
        build(&[found.as_slice()])
    }
}

/// Variable `name` into `buf[off..off + len]`: its full length, or
/// `-ERROR_ENVVAR_NOT_FOUND` when unset.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_getenv(name: NabiStr, buf: *mut u8, off: usize, len: usize) -> isize {
    let mut n = wide(name.bytes());
    let np = n.ptr();
    // SAFETY: `b` is writable for `k` units; `np` is NUL-terminated.
    match fill_wide(|b, k| unsafe { win::GetEnvironmentVariableW(np, b, k) }) {
        Ok(w) => put_wide(buf, off, len, w.as_slice()),
        Err(e) => -(e as isize),
    }
}

/// `unset == 0`: set the variable, else remove it. 0 or `-error`;
/// removing an absent one is 0, as `unsetenv`.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_setenv(name: NabiStr, value: NabiStr, unset: i32) -> i32 {
    let mut n = wide(name.bytes());
    let mut v = wide(value.bytes());
    // SAFETY: NUL-terminated strings (or null to remove).
    let ok = unsafe {
        win::SetEnvironmentVariableW(n.ptr(), if unset != 0 { ptr::null() } else { v.ptr() })
    };
    if ok != 0 {
        return 0;
    }
    let e = fail();
    if unset != 0 && e == -win::ERROR_ENVVAR_NOT_FOUND {
        0
    } else {
        e
    }
}
