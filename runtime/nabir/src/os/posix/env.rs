//! The executable path, arguments and the environment.

#[allow(clippy::wildcard_imports)]
use crate::os::posix::*;

#[cfg(target_vendor = "apple")]
unsafe extern "C" {
    #[link_name = "_NSGetExecutablePath"]
    fn ns_get_executable_path(buf: *mut libc::c_char, size: *mut u32) -> i32;
}

/// Absolute path of the running executable into `buf[off..off + len]`:
/// its full length or `-errno`.
#[cfg(unix)]
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_current_exe(buf: *mut u8, off: usize, len: usize) -> isize {
    let mut path = [0u8; PATH_MAX + 1];
    #[cfg(target_vendor = "apple")]
    {
        let mut raw = [0u8; PATH_MAX + 1];
        let mut size = raw.len() as u32;
        // SAFETY: `raw` is writable for `size` bytes.
        if unsafe { ns_get_executable_path(raw.as_mut_ptr().cast(), &mut size) } != 0 {
            return -(libc::ENAMETOOLONG as isize);
        }
        // SAFETY: `raw` is NUL-terminated; `path` has PATH_MAX bytes.
        if unsafe { libc::realpath(raw.as_ptr().cast(), path.as_mut_ptr().cast()) }.is_null() {
            return -(errno() as isize);
        }
    }
    // No procfs by default: the kernel's record of the image.
    #[cfg(target_os = "freebsd")]
    {
        let mib = [
            libc::CTL_KERN,
            libc::KERN_PROC,
            libc::KERN_PROC_PATHNAME,
            -1,
        ];
        let mut size = path.len() - 1;
        // SAFETY: `path` is writable for `size` bytes; `mib` names a
        // read-only node.
        let r = unsafe {
            libc::sysctl(
                mib.as_ptr(),
                mib.len() as u32,
                path.as_mut_ptr().cast(),
                &mut size,
                ptr::null(),
                0,
            )
        };
        if r != 0 {
            return -(errno() as isize);
        }
    }
    #[cfg(not(any(target_vendor = "apple", target_os = "freebsd")))]
    {
        // SAFETY: `path` is writable for its length minus the NUL.
        let n = unsafe {
            libc::readlink(
                c"/proc/self/exe".as_ptr(),
                path.as_mut_ptr().cast(),
                path.len() - 1,
            )
        };
        if n < 0 {
            return -(errno() as isize);
        }
    }
    put(buf, off, len, until_nul(&path))
}

#[cfg(target_os = "wasi")]
#[unsafe(no_mangle)]
pub const extern "C" fn nabi_os_current_exe(_buf: *mut u8, _off: usize, _len: usize) -> isize {
    UNSUPPORTED as isize
}

/// `environ` (Apple: through `_NSGetEnviron`, as executables only see it
/// there; WASI: wasi-libc's, filled on first use).
pub(crate) fn environ() -> *mut *const CStrPtr {
    #[cfg(target_vendor = "apple")]
    {
        // SAFETY: always valid in a running process.
        unsafe { libc::_NSGetEnviron().cast() }
    }
    #[cfg(all(unix, not(target_vendor = "apple")))]
    {
        unsafe extern "C" {
            static mut environ: *const CStrPtr;
        }
        &raw mut environ
    }
    #[cfg(target_os = "wasi")]
    {
        unsafe extern "C" {
            static mut __wasilibc_environ: *const CStrPtr;
            fn __wasilibc_ensure_environ();
        }
        // SAFETY: fills `__wasilibc_environ` once.
        unsafe { __wasilibc_ensure_environ() };
        &raw mut __wasilibc_environ
    }
}

/// The C `main`'s argc / argv, captured before `main` runs.
#[cfg(all(unix, not(target_vendor = "apple")))]
pub(crate) static mut ARGS: (i32, *const CStrPtr) = (0, ptr::null());

/// Captured before `main` by an `.init_array` entry. glibc passes it
/// `argc, argv, envp`. musl passes nothing: the initial stack, which the
/// kernel lays out as `argc, argv[0..argc], NULL, envp..`, is found
/// through `environ`, still the kernel's `envp` then (glibc's 32-bit Arm
/// `_start` overwrites that `argc` with a pointer).
#[cfg(all(unix, not(target_vendor = "apple")))]
#[used]
#[unsafe(link_section = ".init_array")]
pub(crate) static CAPTURE_ARGS: extern "C" fn(i32, *const CStrPtr) = capture_args;

#[cfg(all(unix, not(target_vendor = "apple"), not(target_env = "musl")))]
pub(crate) extern "C" fn capture_args(argc: i32, argv: *const CStrPtr) {
    // SAFETY: runs once, single-threaded, before `main`.
    unsafe { ARGS = (argc, argv) };
}

#[cfg(target_env = "musl")]
pub(crate) extern "C" fn capture_args(_: i32, _: *const CStrPtr) {
    // SAFETY: runs once, single-threaded, before `main`, while `environ`
    // is still the kernel's `envp`.
    unsafe {
        if let Some(a) = argv_before(*environ()) {
            ARGS = a;
        }
    }
}

/// `(argc, argv)` laid out before `envp`: `envp - 1` is argv's NULL
/// terminator and `argc` sits right before `argv[0]` (argv entries are
/// addresses, never equal to their count).
///
/// # Safety
/// `envp` is null or the `envp` of the process's initial stack layout.
#[cfg(unix)]
#[cfg_attr(not(target_env = "musl"), allow(dead_code))]
pub(crate) unsafe fn argv_before(envp: *const CStrPtr) -> Option<(i32, *const CStrPtr)> {
    if envp.is_null() {
        return None;
    }
    // SAFETY: the words before `envp` are argv's, then argc (caller).
    unsafe {
        let end = envp.sub(1);
        (0..1usize << 20).find_map(|n| {
            let slot = end.sub(n + 1);
            (*slot as usize == n).then(|| (i32::try_from(n).unwrap_or(0), slot.add(1)))
        })
    }
}

pub(crate) fn args() -> (usize, *const CStrPtr) {
    #[cfg(target_vendor = "apple")]
    {
        // SAFETY: set by dyld before `main`.
        unsafe { (*libc::_NSGetArgc() as usize, (*libc::_NSGetArgv()).cast()) }
    }
    #[cfg(all(unix, not(target_vendor = "apple")))]
    {
        // SAFETY: written once before `main`.
        let (c, v) = unsafe { ARGS };
        (c.max(0) as usize, v)
    }
    #[cfg(target_os = "wasi")]
    wasi_args()
}

#[cfg(target_os = "wasi")]
#[link(wasm_import_module = "wasi_snapshot_preview1")]
unsafe extern "C" {
    fn args_sizes_get(argc: *mut usize, size: *mut usize) -> u16;
    fn args_get(argv: *mut CStrPtr, buf: *mut u8) -> u16;
}

/// `args_get` once into a malloc block (never freed).
#[cfg(target_os = "wasi")]
pub(crate) fn wasi_args() -> (usize, *const CStrPtr) {
    static mut ARGS: (usize, *const CStrPtr) = (0, ptr::null());
    // SAFETY: one thread; the block holds `argc` pointers, then the
    // strings they point into.
    unsafe {
        if ARGS.1.is_null() {
            let (mut n, mut size) = (0, 0);
            if args_sizes_get(&mut n, &mut size) != 0 {
                return (0, ptr::null());
            }
            let v = libc::malloc((n + 1) * size_of::<CStrPtr>() + size).cast::<CStrPtr>();
            if v.is_null() {
                nabir_common::oom();
            }
            if args_get(v, v.add(n + 1).cast()) != 0 {
                return (0, ptr::null());
            }
            ARGS = (n, v);
        }
        ARGS
    }
}

/// C string `p` as a GC `str` (null -> `""`).
pub(crate) fn from_c(p: CStrPtr) -> NabiStr {
    if p.is_null() {
        return build(&[]);
    }
    // SAFETY: a NUL-terminated string from libc.
    build(&[unsafe { core::ffi::CStr::from_ptr(p) }.to_bytes()])
}

#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_argc() -> usize {
    args().0
}

#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_arg(i: usize) -> NabiStr {
    let (n, v) = args();
    if i >= n || v.is_null() {
        return build(&[]);
    }
    // SAFETY: `argv` has `argc` entries.
    from_c(unsafe { *v.add(i) })
}

/// Entry `i` of `environ` (`"KEY=VALUE"`), `""` past the end.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_env_at(i: usize) -> NabiStr {
    // SAFETY: `environ` is a null-terminated array; walk up to `i`.
    unsafe {
        let env = *environ();
        if env.is_null() {
            return build(&[]);
        }
        for k in 0..=i {
            if (*env.add(k)).is_null() {
                return build(&[]);
            }
        }
        from_c(*env.add(i))
    }
}

/// `getenv(name)` into `buf[off..off + len]`: its full length, or
/// `-ENOENT` when unset.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_getenv(name: NabiStr, buf: *mut u8, off: usize, len: usize) -> isize {
    // SAFETY: `name` is a C string.
    let p = unsafe { libc::getenv(cs(name).p()) };
    if p.is_null() {
        return -(libc::ENOENT as isize);
    }
    // SAFETY: a NUL-terminated string from libc.
    put(
        buf,
        off,
        len,
        unsafe { core::ffi::CStr::from_ptr(p) }.to_bytes(),
    )
}

/// `unset == 0`: `setenv`, else `unsetenv`. 0 or `-errno`.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_setenv(name: NabiStr, value: NabiStr, unset: i32) -> i32 {
    // SAFETY: C strings. std's env lock serializes Nabi's own callers.
    let r = unsafe {
        if unset != 0 {
            libc::unsetenv(cs(name).p())
        } else {
            libc::setenv(cs(name).p(), cs(value).p(), 1)
        }
    };
    ret(r as isize) as i32
}

/// Pointers to the NUL-separated pieces of `blob` (`count` of them),
/// null-terminated, in a malloc block.
#[cfg(unix)]
pub(crate) fn split_blob(blob: NabiStr, count: usize) -> *mut CStrPtr {
    // SAFETY: malloc of `count + 1` pointers.
    let v = unsafe { libc::malloc((count + 1) * size_of::<CStrPtr>()) }.cast::<CStrPtr>();
    if v.is_null() {
        nabir_common::oom();
    }
    let mut p = blob.ptr;
    for i in 0..count {
        // SAFETY: the blob holds `count` NUL-terminated pieces.
        unsafe {
            *v.add(i) = p.cast();
            p = p.add(libc::strlen(p.cast()) + 1);
        }
    }
    // SAFETY: slot `count` exists.
    unsafe { *v.add(count) = ptr::null() };
    v
}
