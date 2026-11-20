//! What `os` means without an OS: blocking calls run as they are; there
//! is no random source (`-ENOSYS`; hash keys fall back to the clock), no
//! fd to wait on (the one-thread executor never starts its I/O driver)
//! and no environment; the arguments are what the startup found. File
//! system, process and socket functions are not defined: a program that
//! uses them does not link.

pub use nabir_common::errno;

use crate::str::{NabiStr, build};

/// `-ENOSYS`: what the OS would do is not there.
const ENOSYS: i32 = 38;
const ENOENT: i32 = 2;

/// Run a blocking call parked; `errno` survives the re-entry.
pub fn native<R>(f: impl FnOnce() -> R) -> (R, i32) {
    crate::lane::block_begin();
    let r = f();
    let e = errno();
    crate::lane::block_end();
    (r, e)
}

/// No random source: `-ENOSYS`.
#[unsafe(no_mangle)]
pub const extern "C" fn nabi_os_random(_buf: *mut u8, _off: usize, _len: usize) -> i32 {
    -ENOSYS
}

/// No fds to watch: `-ENOSYS`.
#[unsafe(no_mangle)]
pub const extern "C" fn nabi_io_watch(_fd: i32, _interest: u32) -> i32 {
    -ENOSYS
}

/// No fds to wait on: `-ENOSYS`.
#[unsafe(no_mangle)]
pub const extern "C" fn nabi_net_wait(_fd: i32, _write: i32, _timeout_ns: i64) -> i32 {
    -ENOSYS
}

/// The I/O driver's wait: there is no driver thread to run it.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_io_poll(_out: *mut u8, _max: usize) -> i32 {
    nabir_common::fatal(b"fatal error: deadlock: the only thread waits for another")
}

/// The `IoErrorKind` discriminant of error code `code`: `NotFound`
/// (`ENOENT`), `Unsupported` (`ENOSYS`), else `Other` (`posix.rs`'s
/// numbering).
#[unsafe(no_mangle)]
pub const extern "C" fn nabi_os_errno_kind(code: i32) -> i32 {
    match code {
        ENOENT => 1,
        ENOSYS => 14,
        _ => 0,
    }
}

/// The message of error code `code`.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_strerror(code: i32) -> NabiStr {
    let msg: &[u8] = match code {
        ENOENT => b"No such file or directory",
        ENOSYS => b"Function not implemented",
        _ => b"Unknown error",
    };
    build(&[msg])
}

/// The arguments the startup found (`sys::args`).
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_argc() -> usize {
    nabir_common::sys::args().0
}

#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_arg(i: usize) -> NabiStr {
    let (n, v) = nabir_common::sys::args();
    if i >= n || v.is_null() {
        return build(&[]);
    }
    // SAFETY: `argv` has `argc` NUL-terminated entries.
    build(&[unsafe { core::ffi::CStr::from_ptr((*v.add(i)).cast()) }.to_bytes()])
}

/// No environment: every variable is unset.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_env_at(_i: usize) -> NabiStr {
    build(&[])
}

#[unsafe(no_mangle)]
pub const extern "C" fn nabi_os_getenv(
    _name: NabiStr,
    _buf: *mut u8,
    _off: usize,
    _len: usize,
) -> isize {
    -(ENOENT as isize)
}

/// Setting one: `-ENOSYS`; removing one: done, it was never set.
#[unsafe(no_mangle)]
pub const extern "C" fn nabi_os_setenv(_name: NabiStr, _value: NabiStr, unset: i32) -> i32 {
    if unset != 0 { 0 } else { -ENOSYS }
}

/// No kernel to move bytes between fds: `-ENOSYS`.
#[unsafe(no_mangle)]
pub const extern "C" fn nabi_os_copy_fd(_src: i32, _dst: i32) -> i64 {
    -(ENOSYS as i64)
}
