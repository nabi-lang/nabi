//! Win32 helpers behind `std` (fs, env, process)
//! and `core::io`.
//! No ABI stability. The same `nabi_os_*` symbols
//! and conventions as the POSIX `os`, with these differences:
//!
//! - An "fd" is a handle value (0 / 1 / 2: the standard handles). A
//!   socket has bit 0 set ([`sock_of`]) so `close` / `read` / `write` /
//!   `dup` can tell.
//! - Errors are negated Win32 / Winsock codes.
//! - Text is WTF-8 on the nabi side, UTF-16 on the Windows side.
//! - A directory capability is a handle; paths under it resolve
//!   lexically against its final path.
//! - A child's process handle is kept until it is reaped, so its pid is
//!   not reused meanwhile.
//! - Signals are console events: Ctrl-C 2, Ctrl-Break 21, close 1.

mod crypto;
mod env;
mod file;
mod fs;
mod process;

pub use crate::os::windows::crypto::*;
pub(crate) use crate::os::windows::process::flush_on_kill;
pub use nabir_common::errno;

use crate::os::openhow::How;
use crate::str::{NabiStr, build};
use crate::winargs::{Args, parse_command_line, push_arg};
use core::cell::UnsafeCell;
use core::ffi::c_void;
use core::mem::{size_of, zeroed};
use core::ptr;
use core::sync::atomic::{AtomicPtr, AtomicU32, AtomicUsize, Ordering};
use nabir_common::header::HEADER_SIZE;
use nabir_common::sys::{self, MBuf, Wide, fill_wide, narrow, push_wide, wide};
use nabir_common::win::{self, DWORD, HANDLE};

/// Run a blocking call parked; the last error survives the re-entry.
pub fn native<R>(f: impl FnOnce() -> R) -> (R, i32) {
    crate::lane::block_begin();
    let r = f();
    let e = errno();
    crate::lane::block_end();
    (r, e)
}

/// `-GetLastError()`.
fn fail() -> i32 {
    -errno()
}

/// Whether `hd` was opened for overlapped I/O (the parent's end of a
/// child's output pipe, [`output_pipe`]).
fn overlapped(hd: HANDLE) -> bool {
    // FILE_SYNCHRONOUS_IO_ALERT | _NONALERT.
    const SYNCHRONOUS: u32 = 0x30;
    let mut mode = 0u32;
    // SAFETY: zeroed status block; a 4-byte FileModeInformation (16).
    let s = unsafe {
        let mut iosb: win::IO_STATUS_BLOCK = core::mem::zeroed();
        win::NtQueryInformationFile(hd, &mut iosb, (&raw mut mode).cast(), 4, 16)
    };
    s >= 0 && mode & SYNCHRONOUS == 0
}

/// `ReadFile` of `want` bytes into `dst` (0: until data is there or the
/// writer closed), which waits for its own request on an overlapped
/// handle. The last error is the read's.
///
/// # Safety
/// `dst` must be writable for `want` bytes.
unsafe fn read_handle(hd: HANDLE, dst: *mut u8, want: DWORD, n: &mut DWORD) -> win::BOOL {
    if !overlapped(hd) {
        // SAFETY: caller contract.
        return unsafe { win::ReadFile(hd, dst.cast(), want, n, ptr::null_mut()) };
    }
    // SAFETY: a fresh event and request, both done with before returning.
    unsafe {
        let mut ov: win::OVERLAPPED = core::mem::zeroed();
        ov.hEvent = win::CreateEventW(ptr::null_mut(), 1, 0, ptr::null());
        let mut ok = win::ReadFile(hd, dst.cast(), want, n, &mut ov);
        if ok == 0 && errno() == win::ERROR_IO_PENDING {
            ok = win::GetOverlappedResult(hd, &mut ov, n, 1);
        }
        let e = win::GetLastError();
        win::CloseHandle(ov.hEvent);
        win::SetLastError(e);
        ok
    }
}

/// The writable region `off..off + len` of the `GcBuf<u8>` `buf`.
///
/// # Safety
/// `buf` must be a live `GcBuf<u8>` with at least `off + len` bytes.
pub const unsafe fn region(buf: *mut u8, off: usize) -> *mut u8 {
    // SAFETY: caller contract.
    unsafe { buf.add(HEADER_SIZE + off) }
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

/// Copy up to `len` bytes of `src` to `buf[off..]`; `src`'s full length.
fn put(buf: *mut u8, off: usize, len: usize, src: &[u8]) -> isize {
    // SAFETY: the library passes a live region of `len` bytes.
    unsafe { ptr::copy_nonoverlapping(src.as_ptr(), region(buf, off), src.len().min(len)) };
    src.len() as isize
}

/// [`put`] for a UTF-16 result.
fn put_wide(buf: *mut u8, off: usize, len: usize, w: &[u16]) -> isize {
    put(buf, off, len, narrow(w).as_slice())
}

/// The handle behind `fd`.
fn h(fd: i32) -> HANDLE {
    sys::handle(fd)
}

/// The socket behind `fd`, if it is one (see the module notes).
pub const fn sock_of(fd: i32) -> Option<win::SOCKET> {
    if fd != 1 && fd & 3 == 1 {
        Some((fd & !3) as u32 as win::SOCKET)
    } else {
        None
    }
}

/// The fd for socket `s`.
pub const fn fd_of(s: win::SOCKET) -> i32 {
    s as i32 | 1
}

// The fd encoding stays inside the runtime: `os::windows::io` hands out
// and takes the full HANDLE / SOCKET. Kernel handles carry 32
// significant bits (sign-extended), so the fd holds them whole.

/// The HANDLE behind fd `fd` (`INVALID_HANDLE_VALUE` for a closed fd or
/// a socket).
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_raw_handle(fd: i32) -> isize {
    if fd < 0 || sock_of(fd).is_some() {
        return win::INVALID_HANDLE_VALUE;
    }
    h(fd)
}

/// The SOCKET behind fd `fd` (`INVALID_SOCKET` for anything else).
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_raw_socket(fd: i32) -> u64 {
    sock_of(fd).unwrap_or(win::INVALID_SOCKET) as u64
}

/// The fd for HANDLE `hd` (`-1`: not a kernel handle the fd can hold).
#[unsafe(no_mangle)]
pub const extern "C" fn nabi_os_handle_fd(hd: isize) -> i32 {
    let fd = hd as i32;
    if fd as isize != hd || fd <= 0 || fd & 3 != 0 {
        return -1;
    }
    fd
}

/// The fd for SOCKET `s` (`-1`: not one the fd can hold).
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_socket_fd(s: u64) -> i32 {
    if s > u64::from(u32::MAX) || s & 3 != 0 {
        return -1;
    }
    fd_of(s as win::SOCKET)
}

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

/// The `IoErrorKind` discriminant of Win32 / Winsock error `code`.
#[unsafe(no_mangle)]
pub const extern "C" fn nabi_os_errno_kind(code: i32) -> i32 {
    match code {
        win::ERROR_FILE_NOT_FOUND
        | win::ERROR_PATH_NOT_FOUND
        | win::ERROR_INVALID_DRIVE
        | win::ERROR_ENVVAR_NOT_FOUND
        | win::WSAHOST_NOT_FOUND
        | win::WSANO_DATA => kind::NOT_FOUND,
        win::ERROR_ACCESS_DENIED
        | win::ERROR_WRITE_PROTECT
        | win::ERROR_PRIVILEGE_NOT_HELD
        | win::WSAEACCES => kind::PERMISSION_DENIED,
        // Another handle's share mode denies this access.
        win::ERROR_SHARING_VIOLATION => kind::RESOURCE_BUSY,
        win::ERROR_FILE_EXISTS | win::ERROR_ALREADY_EXISTS => kind::ALREADY_EXISTS,
        win::ERROR_DIRECTORY => kind::NOT_A_DIRECTORY,
        win::ERROR_DIRECTORY_NOT_SUPPORTED => kind::IS_A_DIRECTORY,
        win::ERROR_DIR_NOT_EMPTY => kind::DIRECTORY_NOT_EMPTY,
        win::ERROR_INVALID_PARAMETER | win::ERROR_INVALID_NAME | win::WSAEINVAL => {
            kind::INVALID_INPUT
        }
        win::ERROR_BROKEN_PIPE | win::ERROR_NO_DATA | win::WSAESHUTDOWN => kind::BROKEN_PIPE,
        win::WSAEINTR => kind::INTERRUPTED,
        // A byte range another handle locked: what EAGAIN is for a lock.
        win::WSAEWOULDBLOCK | win::ERROR_LOCK_VIOLATION => kind::WOULD_BLOCK,
        win::ERROR_TIMEOUT | win::WSAETIMEDOUT => kind::TIMED_OUT,
        win::ERROR_HANDLE_DISK_FULL | win::ERROR_DISK_FULL => kind::STORAGE_FULL,
        win::ERROR_NOT_SAME_DEVICE => kind::CROSSES_DEVICES,
        win::ERROR_NOT_SUPPORTED | win::ERROR_CALL_NOT_IMPLEMENTED | win::WSAEOPNOTSUPP => {
            kind::UNSUPPORTED
        }
        win::ERROR_STOPPED_ON_SYMLINK | win::ERROR_CANT_RESOLVE_FILENAME => kind::FILESYSTEM_LOOP,
        _ => kind::OTHER,
    }
}

/// The system message of error `code` (`FormatMessageW`).
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_strerror(code: i32) -> NabiStr {
    let mut buf = [0u16; 512];
    // SAFETY: `buf` is writable for its length; no inserts.
    let n = unsafe {
        win::FormatMessageW(
            win::FORMAT_MESSAGE_FROM_SYSTEM | win::FORMAT_MESSAGE_IGNORE_INSERTS,
            ptr::null(),
            code as DWORD,
            0,
            buf.as_mut_ptr(),
            buf.len() as DWORD,
            ptr::null_mut(),
        )
    } as usize;
    let mut msg = &buf[..n];
    while let [rest @ .., last] = msg {
        if !matches!(u8::try_from(*last), Ok(b'\r' | b'\n' | b' ' | b'.')) {
            break;
        }
        msg = rest;
    }
    if msg.is_empty() {
        let mut d = [0u8; 12];
        return build(&[b"os error ", decimal(code, &mut d)]);
    }
    build(&[narrow(msg).as_slice()])
}

/// `v` in decimal.
fn decimal(v: i32, out: &mut [u8; 12]) -> &[u8] {
    let mut n = v.unsigned_abs();
    let mut i = out.len();
    loop {
        i -= 1;
        out[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    if v < 0 {
        i -= 1;
        out[i] = b'-';
    }
    &out[i..]
}

/// `path` under directory capability `dir` (negative: the process's
/// own resolution, relative to the working directory) as a Win32 path.
fn path_w(dir: i32, path: NabiStr) -> Result<Wide, i32> {
    if dir < 0 {
        return Ok(wide(path.bytes()));
    }
    let mut w = final_path(h(dir))?;
    for part in path.bytes().split(|b| *b == b'/' || *b == b'\\') {
        match part {
            b"" | b"." => {}
            b".." => {
                // Never above the capability's own path.
                if let Some(i) = w.as_slice().iter().rposition(|&u| u == u16::from(b'\\')) {
                    w.truncate(i);
                }
            }
            _ => {
                w.push(u16::from(b'\\'));
                push_wide(&mut w, part);
            }
        }
    }
    Ok(w)
}

/// The `\\?\` final path of the open handle `h`.
fn final_path(h: HANDLE) -> Result<Wide, i32> {
    let mut w = fill_wide(|b, n| {
        // SAFETY: `b` is writable for `n` units.
        unsafe { win::GetFinalPathNameByHandleW(h, b, n, win::FILE_NAME_NORMALIZED) }
    })?;
    if w.as_slice().last() == Some(&u16::from(b'\\')) {
        let n = w.len() - 1;
        w.truncate(n);
    }
    Ok(w)
}

/// A `\\?\` path as the user would write it: `\\?\C:\x` -> `C:\x`,
/// `\\?\UNC\srv\x` -> `\\srv\x`.
fn friendly(w: &[u16]) -> MBuf<u16> {
    let mut out = MBuf::new();
    let unc = ascii_w(br"\\?\UNC\");
    let verbatim = ascii_w(br"\\?\");
    if w.starts_with(&unc) {
        out.extend(&[u16::from(b'\\'); 2]);
        out.extend(&w[unc.len()..]);
    } else if w.starts_with(&verbatim)
        && w.len() >= 6
        && w[5] == u16::from(b':')
        && u8::try_from(w[4]).is_ok_and(|c| c.is_ascii_alphabetic())
    {
        out.extend(&w[verbatim.len()..]);
    } else {
        out.extend(w);
    }
    out
}

/// The UTF-16 of an ASCII literal.
pub const fn ascii_w<const N: usize>(s: &[u8; N]) -> [u16; N] {
    let mut a = [0u16; N];
    let mut i = 0;
    while i < N {
        a[i] = s[i] as u16;
        i += 1;
    }
    a
}

/// An open handle closed on drop.
struct Owned(HANDLE);

impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: our own handle.
        unsafe { win::CloseHandle(self.0) };
    }
}

/// Open `path` for metadata only (`follow == false`: a symlink itself).
fn open_meta(path: &mut Wide, follow: bool) -> Result<Owned, i32> {
    let flags = win::FILE_FLAG_BACKUP_SEMANTICS
        | if follow {
            0
        } else {
            win::FILE_FLAG_OPEN_REPARSE_POINT
        };
    // SAFETY: a NUL-terminated path.
    let hd = unsafe {
        win::CreateFileW(
            path.ptr(),
            win::FILE_READ_ATTRIBUTES,
            win::FILE_SHARE_READ | win::FILE_SHARE_WRITE | win::FILE_SHARE_DELETE,
            ptr::null_mut(),
            win::OPEN_EXISTING,
            flags,
            0,
        )
    };
    if hd == win::INVALID_HANDLE_VALUE {
        Err(fail())
    } else {
        Ok(Owned(hd))
    }
}

/// `FILE_GENERIC_WRITE` without `FILE_WRITE_DATA`: appends only.
const APPEND_ACCESS: DWORD = 0x0012_0114;
