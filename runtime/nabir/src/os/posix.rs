//! POSIX helpers behind `std` (fs, env, process)
//! and `core::io`.
//! No ABI stability. WASI through wasi-libc:
//! what preview 1 lacks (processes, signals, `mmap`, locks, `dup`)
//! fails with `-ENOSYS`.
//!
//! - `str` arguments reach libc through [`cs`] (a slice need not be
//!   NUL-terminated).
//! - Results are `>= 0` or `-errno`; resolver failures use codes from
//!   [`EAI_BASE`] up.
//! - Byte data is a region `(buf, off, len)` of the caller's
//!   `GcBuf<u8>`. Variable-length results are copied in up to `len`
//!   bytes and return their full length, so the caller can retry.
//! - Blocking calls are bracketed by `lane::block_begin` / `block_end`
//!   and allocate no GC memory meanwhile; the
//!   caller roots every GC argument.

mod copy;
mod crypto;
mod env;
mod file;
mod fs;
mod process;

pub(crate) use crate::os::posix::copy::*;
pub use crate::os::posix::crypto::*;
pub use nabir_common::errno;

use crate::os::openhow::How;
use crate::str::{NabiStr, build};
use core::mem::size_of;
use core::ptr;
#[cfg(all(
    unix,
    not(any(target_vendor = "apple", target_os = "freebsd", target_os = "android"))
))]
use libc::posix_spawn_file_actions_addchdir_np;
use nabir_common::header::HEADER_SIZE;

type CStrPtr = *const libc::c_char;

#[cfg(unix)]
const PATH_MAX: usize = libc::PATH_MAX as usize;
#[cfg(target_os = "wasi")]
const PATH_MAX: usize = 4096;

#[cfg(target_os = "wasi")]
const UNSUPPORTED: i32 = -libc::ENOSYS;

/// `r` (a libc `-1`-on-error result) as `>= 0` or `-errno`.
pub fn ret(r: isize) -> isize {
    if r < 0 { -(errno() as isize) } else { r }
}

/// Run a blocking call parked; `errno` survives the re-entry.
pub fn native<R>(f: impl FnOnce() -> R) -> (R, i32) {
    crate::lane::block_begin();
    let r = f();
    let e = errno();
    crate::lane::block_end();
    (r, e)
}

/// `s` as a C string for libc: its own bytes when a NUL follows them,
/// else a `malloc`ed copy (no GC allocation, so it works while parked)
/// freed on drop. Keep it alive across the call: `cs(s).p()` inside the
/// call expression.
pub fn cs(s: NabiStr) -> CArg {
    if s.len == 0 {
        return CArg(c"".as_ptr(), false);
    }
    // SAFETY: byte `len` of a `str` is readable (its source's next byte
    // or the NUL after it).
    if unsafe { *s.ptr.add(s.len) } == 0 {
        return CArg(s.ptr.cast(), false);
    }
    // SAFETY: malloc'd `len + 1` bytes, filled and terminated.
    unsafe {
        let p = libc::malloc(s.len + 1).cast::<u8>();
        if p.is_null() {
            nabir_common::oom();
        }
        ptr::copy_nonoverlapping(s.ptr, p, s.len);
        *p.add(s.len) = 0;
        CArg(p.cast_const().cast(), true)
    }
}

/// A NUL-terminated string from [`cs`]; `.1`: owned (malloc'd).
pub struct CArg(CStrPtr, bool);

impl CArg {
    pub const fn p(&self) -> CStrPtr {
        self.0
    }
}

impl Drop for CArg {
    fn drop(&mut self) {
        if self.1 {
            // SAFETY: our own malloc block.
            unsafe { libc::free(self.0.cast_mut().cast()) };
        }
    }
}

/// Element `i` of the `GcBuf<i32>` `buf` (header pointer).
///
/// # Safety
/// `buf` must be a live `GcBuf<i32>` with more than `i` elements.
#[allow(clippy::cast_ptr_alignment)] // GC payloads are 8-aligned
const unsafe fn cell(buf: *mut u8, i: usize) -> *mut i32 {
    // SAFETY: caller contract; elements follow the header.
    unsafe { buf.add(HEADER_SIZE).cast::<i32>().add(i) }
}

/// The writable region `off..off + len` of the `GcBuf<u8>` `buf`.
///
/// # Safety
/// `buf` must be a live `GcBuf<u8>` with at least `off + len` bytes.
pub const unsafe fn region(buf: *mut u8, off: usize) -> *mut u8 {
    // SAFETY: caller contract.
    unsafe { buf.add(HEADER_SIZE + off) }
}

/// Copy up to `len` bytes of `src` to `buf[off..]`; `src`'s full length.
fn put(buf: *mut u8, off: usize, len: usize, src: &[u8]) -> isize {
    // SAFETY: the library passes a live region of `len` bytes.
    unsafe { ptr::copy_nonoverlapping(src.as_ptr(), region(buf, off), src.len().min(len)) };
    src.len() as isize
}

/// Bytes of the NUL-terminated `buf` before its NUL.
fn until_nul(buf: &[u8]) -> &[u8] {
    &buf[..buf.iter().position(|&b| b == 0).unwrap_or(buf.len())]
}

/// First resolver code: `getaddrinfo` failure `e` is `EAI_BASE + |e|`.
pub const EAI_BASE: i32 = 100_000;

/// `getaddrinfo`'s "name does not resolve"; the libc crate lacks it for
/// WASI, where wasi-libc numbers it as musl does.
#[cfg(unix)]
const EAI_NONAME: i32 = libc::EAI_NONAME;
#[cfg(not(unix))]
const EAI_NONAME: i32 = -2;

/// `IoErrorKind` discriminants (library/core/src/io.nabi declares the same).
mod kind {
    pub const OTHER: i32 = 0;
    pub const NOT_FOUND: i32 = 1;
    pub const PERMISSION_DENIED: i32 = 2;
    pub const ALREADY_EXISTS: i32 = 3;
    pub const NOT_A_DIRECTORY: i32 = 4;
    pub const IS_A_DIRECTORY: i32 = 5;
    pub const DIRECTORY_NOT_EMPTY: i32 = 6;
    pub const INVALID_INPUT: i32 = 7;
    pub const BROKEN_PIPE: i32 = 8;
    pub const INTERRUPTED: i32 = 9;
    pub const WOULD_BLOCK: i32 = 10;
    pub const TIMED_OUT: i32 = 11;
    pub const STORAGE_FULL: i32 = 12;
    pub const CROSSES_DEVICES: i32 = 13;
    pub const UNSUPPORTED: i32 = 14;
    pub const FILESYSTEM_LOOP: i32 = 18;
    pub const RESOURCE_BUSY: i32 = 19;
}

/// The `IoErrorKind` discriminant of error code `code`.
#[unsafe(no_mangle)]
pub const extern "C" fn nabi_os_errno_kind(code: i32) -> i32 {
    match code {
        libc::ENOENT => kind::NOT_FOUND,
        // A name that does not resolve; other resolver failures are Other.
        c if c == EAI_BASE + EAI_NONAME.abs() => kind::NOT_FOUND,
        libc::EACCES | libc::EPERM => kind::PERMISSION_DENIED,
        libc::EEXIST => kind::ALREADY_EXISTS,
        libc::ENOTDIR => kind::NOT_A_DIRECTORY,
        libc::EISDIR => kind::IS_A_DIRECTORY,
        libc::ENOTEMPTY => kind::DIRECTORY_NOT_EMPTY,
        libc::EINVAL => kind::INVALID_INPUT,
        libc::EPIPE => kind::BROKEN_PIPE,
        libc::EINTR => kind::INTERRUPTED,
        libc::EAGAIN => kind::WOULD_BLOCK,
        libc::ETIMEDOUT => kind::TIMED_OUT,
        libc::ENOSPC => kind::STORAGE_FULL,
        libc::EXDEV => kind::CROSSES_DEVICES,
        libc::ENOSYS | libc::ENOTSUP => kind::UNSUPPORTED,
        libc::ELOOP => kind::FILESYSTEM_LOOP,
        libc::EBUSY | libc::ETXTBSY => kind::RESOURCE_BUSY,
        _ => kind::OTHER,
    }
}

/// The message of error code `code` (`strerror` / `gai_strerror`).
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_strerror(code: i32) -> NabiStr {
    #[cfg(any(unix, target_env = "p2"))]
    if code >= EAI_BASE {
        // glibc and wasi-libc number EAI codes negative, BSD positive.
        #[cfg(any(target_os = "linux", target_os = "wasi"))]
        let c = EAI_BASE - code;
        #[cfg(not(any(target_os = "linux", target_os = "wasi")))]
        let c = code - EAI_BASE;
        // SAFETY: gai_strerror returns a static C string.
        let s = unsafe { core::ffi::CStr::from_ptr(libc::gai_strerror(c)) };
        return build(&[s.to_bytes()]);
    }
    let mut buf = [0u8; 256];
    // SAFETY: `buf` is writable for its length.
    unsafe { libc::strerror_r(code, buf.as_mut_ptr().cast(), buf.len()) };
    build(&[until_nul(&buf)])
}

/// `AT_FDCWD` for a negative `dir` (the root / cwd capability).
const fn at(dir: i32) -> i32 {
    if dir < 0 { libc::AT_FDCWD } else { dir }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::os::posix::env::argv_before;

    #[test]
    fn errno_kinds() {
        assert_eq!(nabi_os_errno_kind(libc::ENOENT), kind::NOT_FOUND);
        assert_eq!(nabi_os_errno_kind(libc::EPERM), kind::PERMISSION_DENIED);
        assert_eq!(nabi_os_errno_kind(libc::EACCES), kind::PERMISSION_DENIED);
        assert_eq!(nabi_os_errno_kind(libc::EEXIST), kind::ALREADY_EXISTS);
        assert_eq!(nabi_os_errno_kind(libc::ENOTSUP), kind::UNSUPPORTED);
        assert_eq!(nabi_os_errno_kind(libc::EBUSY), kind::RESOURCE_BUSY);
        assert_eq!(nabi_os_errno_kind(libc::ETXTBSY), kind::RESOURCE_BUSY);
        assert_eq!(
            nabi_os_errno_kind(EAI_BASE + EAI_NONAME.abs()),
            kind::NOT_FOUND
        );
        assert_eq!(
            nabi_os_errno_kind(EAI_BASE + libc::EAI_AGAIN.abs()),
            kind::OTHER
        );
        assert_eq!(nabi_os_errno_kind(0), kind::OTHER);
        assert_eq!(nabi_os_errno_kind(-1), kind::OTHER);
    }

    /// The initial stack: `argc, argv.., NULL, envp.., NULL`.
    #[test]
    fn argv_from_the_initial_stack() {
        let (prog, flag, var) = (c"prog".as_ptr(), c"-x".as_ptr(), c"K=V".as_ptr());
        for (stack, envp_at, argc) in [
            (
                vec![2 as CStrPtr, prog, flag, ptr::null(), var, ptr::null()],
                4,
                2,
            ),
            (vec![0 as CStrPtr, ptr::null(), var, ptr::null()], 2, 0),
        ] {
            // SAFETY: `stack` has that layout.
            let (count, argv) = unsafe { argv_before(stack.as_ptr().add(envp_at)) }.unwrap();
            assert_eq!(count, argc);
            assert_eq!(argv, stack[1..].as_ptr());
        }
        // SAFETY: null is allowed.
        assert!(unsafe { argv_before(ptr::null()) }.is_none());
    }
}
