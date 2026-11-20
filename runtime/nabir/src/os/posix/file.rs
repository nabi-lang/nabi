//! File descriptors: open, read / write, sync, mmap and locks.

#[allow(clippy::wildcard_imports)]
use crate::os::posix::*;

/// `openat` for one of the library's open functions: `how` fixes the
/// access and the disposition, the runtime maps them 1:1. Always
/// `O_CLOEXEC`. A read-only open of a directory as a file is `EISDIR`.
///
/// # Safety
/// `how` must be a live `GcBuf<u32>` of `HOW_LEN` elements.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nabi_os_open(dir: i32, path: NabiStr, how: *const u8) -> i32 {
    // SAFETY: caller contract.
    let how = unsafe { How::read(how) };
    let mut o = match how.access {
        1 => libc::O_WRONLY,
        2 => libc::O_RDWR,
        3 => libc::O_WRONLY | libc::O_APPEND,
        4 => libc::O_RDWR | libc::O_APPEND,
        _ => libc::O_RDONLY,
    };
    o |= match how.disposition {
        1 => libc::O_CREAT | libc::O_TRUNC,
        2 => libc::O_CREAT | libc::O_EXCL,
        3 => libc::O_CREAT,
        _ => 0,
    };
    if how.nofollow {
        o |= libc::O_NOFOLLOW;
    }
    if how.directory {
        o |= libc::O_DIRECTORY;
    }
    o |= how.custom as libc::c_int;
    // SAFETY: `path` is a C string; the mode is read only with O_CREAT.
    let (r, _) = native(|| unsafe {
        libc::openat(
            at(dir),
            cs(path).p(),
            o | libc::O_CLOEXEC,
            how.mode as libc::c_uint,
        )
    });
    if r >= 0 && !how.directory && how.access == 0 && is_dir_fd(r) {
        // SAFETY: ours.
        unsafe { libc::close(r) };
        return -libc::EISDIR;
    }
    ret(r as isize) as i32
}

pub(crate) fn is_dir_fd(fd: i32) -> bool {
    // SAFETY: zeroed stat is a valid out-buffer.
    let mut st: libc::stat = unsafe { core::mem::zeroed() };
    // SAFETY: valid out-buffer.
    unsafe { libc::fstat(fd, &raw mut st) == 0 && st.st_mode & libc::S_IFMT == libc::S_IFDIR }
}

/// `openat(dir, path, oflag | O_CLOEXEC, mode)`, nothing else checked
/// (`DirExt::open_at`).
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_openat_raw(dir: i32, path: NabiStr, oflag: i32, mode: u32) -> i32 {
    // SAFETY: `path` is a C string.
    let (r, _) = native(|| unsafe {
        libc::openat(
            at(dir),
            cs(path).p(),
            oflag | libc::O_CLOEXEC,
            mode as libc::c_uint,
        )
    });
    ret(r as isize) as i32
}

#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_close(fd: i32) -> i32 {
    // SAFETY: plain libc call.
    ret(unsafe { libc::close(fd) } as isize) as i32
}

/// One `read` into `buf[off..off + len]`: the count (0 = EOF) or `-errno`.
///
/// # Safety
/// `buf` must be a live `GcBuf<u8>` with at least `off + len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nabi_os_read(fd: i32, buf: *mut u8, off: usize, len: usize) -> isize {
    // SAFETY: caller contract.
    let dst = unsafe { region(buf, off) };
    if fd == 0 {
        return crate::stdio::read0(dst, len);
    }
    native(|| {
        loop {
            // SAFETY: `dst` is writable for `len` bytes.
            let n = ret(unsafe { libc::read(fd, dst.cast(), len) });
            if n != -(libc::EINTR as isize) {
                break n;
            }
        }
    })
    .0
}

/// One `write` of `buf[off..off + len]`: the count or `-errno`.
///
/// # Safety
/// `buf` must be a live `GcBuf<u8>` with at least `off + len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nabi_os_write(fd: i32, buf: *mut u8, off: usize, len: usize) -> isize {
    // SAFETY: caller contract.
    let src = unsafe { region(buf, off) };
    native(|| {
        loop {
            // SAFETY: `src` is readable for `len` bytes.
            let n = ret(unsafe { libc::write(fd, src.cast(), len) });
            if n != -(libc::EINTR as isize) {
                break n;
            }
        }
    })
    .0
}

/// Wait until `fd1` or `fd2` (negative: ignored) is readable or hung
/// up: bit 0 / bit 1 set for each ready one, or `-errno`.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_poll_read(fd1: i32, fd2: i32) -> i32 {
    let mut fds = [fd1, fd2].map(|fd| libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    });
    native(|| {
        loop {
            // SAFETY: two entries; negative fds are ignored.
            if unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) } >= 0 {
                break i32::from(fds[0].revents != 0) | (i32::from(fds[1].revents != 0) << 1);
            }
            let e = errno();
            if e != libc::EINTR {
                break -e;
            }
        }
    })
    .0
}

/// `fsync` (`data_only == 0`) or `fdatasync`.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_fsync(fd: i32, data_only: i32) -> i32 {
    // SAFETY: plain libc calls.
    let (r, e) = native(|| unsafe {
        #[cfg(not(target_vendor = "apple"))]
        if data_only != 0 {
            return libc::fdatasync(fd);
        }
        let _ = data_only;
        libc::fsync(fd)
    });
    if r < 0 { -e } else { 0 }
}

#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_ftruncate(fd: i32, len: u64) -> i32 {
    // SAFETY: plain libc call.
    ret(unsafe { libc::ftruncate(fd, len as libc::off_t) } as isize) as i32
}

/// `lseek`; `whence` 0 start, 1 current, 2 end. New offset or `-errno`.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_lseek(fd: i32, off: i64, whence: i32) -> i64 {
    let w = match whence {
        1 => libc::SEEK_CUR,
        2 => libc::SEEK_END,
        _ => libc::SEEK_SET,
    };
    // SAFETY: plain libc call.
    let r = unsafe { libc::lseek(fd, off as libc::off_t, w) };
    if r < 0 { -i64::from(errno()) } else { r as i64 }
}

#[cfg(unix)]
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_dup(fd: i32) -> i32 {
    // SAFETY: plain libc call.
    ret(unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) } as isize) as i32
}

#[cfg(target_os = "wasi")]
#[unsafe(no_mangle)]
pub const extern "C" fn nabi_os_dup(_fd: i32) -> i32 {
    UNSUPPORTED
}

#[cfg(unix)]
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_fchmod(fd: i32, mode: u32) -> i32 {
    // SAFETY: plain libc call.
    ret(unsafe { libc::fchmod(fd, mode as libc::mode_t) } as isize) as i32
}

/// WASI files have no permission bits (wasi-libc makes `st_mode` up):
/// nothing to set.
#[cfg(target_os = "wasi")]
#[unsafe(no_mangle)]
pub const extern "C" fn nabi_os_fchmod(_fd: i32, _mode: u32) -> i32 {
    0
}

/// `fchmodat`: the permission bits of `path` under `dir` (a final
/// symlink is followed).
#[cfg(unix)]
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_chmod(dir: i32, path: NabiStr, mode: u32) -> i32 {
    // SAFETY: `path` is a C string.
    let (r, e) =
        native(|| unsafe { libc::fchmodat(at(dir), cs(path).p(), mode as libc::mode_t, 0) });
    if r < 0 { -e } else { 0 }
}

/// WASI: no permission bits; `path` must exist.
#[cfg(target_os = "wasi")]
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_chmod(dir: i32, path: NabiStr, _mode: u32) -> i32 {
    // SAFETY: zeroed stat is a valid out-buffer; `path` is a C string.
    let (r, e) = native(|| unsafe {
        let mut st: libc::stat = core::mem::zeroed();
        libc::fstatat(at(dir), cs(path).p(), &raw mut st, 0)
    });
    if r < 0 { -e } else { 0 }
}

/// A read-only shared mapping of `len` bytes of `fd` at `offset`: its
/// address, or `-errno`.
#[cfg(unix)]
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_mmap(fd: i32, len: usize, offset: u64) -> i64 {
    if len == 0 {
        return -i64::from(libc::EINVAL);
    }
    // SAFETY: a fresh mapping; the kernel picks the address.
    let (p, e) = native(|| unsafe {
        libc::mmap(
            core::ptr::null_mut(),
            len,
            libc::PROT_READ,
            libc::MAP_SHARED,
            fd,
            offset as libc::off_t,
        )
    });
    if p == libc::MAP_FAILED {
        -i64::from(e)
    } else {
        p as i64
    }
}

#[cfg(target_os = "wasi")]
#[unsafe(no_mangle)]
pub const extern "C" fn nabi_os_mmap(_fd: i32, _len: usize, _offset: u64) -> i64 {
    UNSUPPORTED as i64
}

/// Unmap a `nabi_os_mmap` region: 0 or `-errno`.
#[cfg(unix)]
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_munmap(addr: usize, len: usize) -> i32 {
    // SAFETY: `addr..addr + len` is a mapping from `nabi_os_mmap`.
    ret(unsafe { libc::munmap(addr as *mut libc::c_void, len) } as isize) as i32
}

#[cfg(target_os = "wasi")]
#[unsafe(no_mangle)]
pub const extern "C" fn nabi_os_munmap(_addr: usize, _len: usize) -> i32 {
    UNSUPPORTED
}

/// Copy `len` bytes at `addr + at` into `buf[off..]`.
///
/// # Safety
/// `addr + at .. + len` must be mapped; `buf` a live `GcBuf<u8>` with at
/// least `off + len` bytes.
#[unsafe(no_mangle)]
pub const unsafe extern "C" fn nabi_os_mmap_copy(
    addr: usize,
    at: usize,
    buf: *mut u8,
    off: usize,
    len: usize,
) {
    // SAFETY: caller contract.
    unsafe { core::ptr::copy_nonoverlapping((addr + at) as *const u8, region(buf, off), len) };
}

/// `flock`: `op` 1 shared, 2 exclusive, 8 unlock, | 4 non-blocking.
/// 1 = locked, 0 = would block (non-blocking), `-errno`.
#[cfg(unix)]
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_flock(fd: i32, op: i32) -> i32 {
    let mut o = 0;
    for (bit, f) in [
        (1, libc::LOCK_SH),
        (2, libc::LOCK_EX),
        (4, libc::LOCK_NB),
        (8, libc::LOCK_UN),
    ] {
        if op & bit != 0 {
            o |= f;
        }
    }
    // SAFETY: plain libc call.
    let (r, e) = native(|| unsafe { libc::flock(fd, o) });
    match (r, e) {
        (0, _) => 1,
        (_, libc::EWOULDBLOCK) if op & 4 != 0 => 0,
        _ => -e,
    }
}

#[cfg(target_os = "wasi")]
#[unsafe(no_mangle)]
pub const extern "C" fn nabi_os_flock(_fd: i32, _op: i32) -> i32 {
    UNSUPPORTED
}

/// Copy `src` -> `dst` from their current offsets to EOF: bytes or `-errno`.
/// The kernel moves the bytes where the pair of descriptor kinds allows
/// ([`kernel_copy`]); every other pair, and one the kernel or file system
/// refuses before a byte moved, takes the read / write loop. Non-blocking
/// ends are waited on with `poll`.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_copy_fd(src: i32, dst: i32) -> i64 {
    let (r, _) = native(|| {
        let mut total = 0;
        kernel_copy(src, dst, &mut total).unwrap_or_else(|| rw_copy(src, dst, total))
    });
    r.unwrap_or_else(|e| -i64::from(e))
}
