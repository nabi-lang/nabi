//! Paths and directories: clone, stat, links, listing, cwd.

#[allow(clippy::wildcard_imports)]
use crate::os::posix::*;

/// Reflink `from` (a regular file) to a new `to`, both under `dir`
/// (macOS `clonefileat`: same-volume APFS copies share their blocks).
/// 0, or `-errno` (`EEXIST`, cross-volume, no support) - the caller
/// then copies the bytes. Elsewhere always `-ENOTSUP` (Linux reflinks
/// through `copy_file_range` in `nabi_os_copy_fd`).
#[unsafe(no_mangle)]
#[cfg_attr(not(target_vendor = "apple"), allow(clippy::missing_const_for_fn))]
pub extern "C" fn nabi_os_clone(dir: i32, from: NabiStr, to: NabiStr) -> i32 {
    #[cfg(target_vendor = "apple")]
    {
        // SAFETY: both are C strings.
        let (r, e) =
            native(|| unsafe { libc::clonefileat(at(dir), cs(from).p(), at(dir), cs(to).p(), 0) });
        if r < 0 { -e } else { 0 }
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        let _ = (dir, from, to);
        -libc::ENOTSUP
    }
}

/// The kind `nabi_os_stat` reports for file mode `m`.
// `mode_t` is u16 on macOS, u32 elsewhere.
#[allow(clippy::unnecessary_cast)]
pub(crate) const fn kind_of(m: u32) -> i64 {
    match m & libc::S_IFMT as u32 {
        x if x == libc::S_IFREG as u32 => 1,
        x if x == libc::S_IFDIR as u32 => 2,
        x if x == libc::S_IFLNK as u32 => 3,
        _ => 0,
    }
}

/// `nabi_os_stat` through `statx` (Linux 4.11+), the only call that
/// reports creation time; `Err(errno)`.
#[cfg(target_os = "linux")]
pub(crate) fn statx(dir: i32, path: NabiStr, flags: i32, fd: i32) -> Result<[i64; 12], i32> {
    // SAFETY: zeroed `statx` is a valid out-buffer.
    let mut sx: libc::statx = unsafe { core::mem::zeroed() };
    let mask = libc::STATX_BASIC_STATS | libc::STATX_BTIME;
    // SAFETY: C strings; `sx` is writable.
    let (r, e) = native(|| unsafe {
        if fd >= 0 {
            let f = flags | libc::AT_EMPTY_PATH;
            libc::syscall(libc::SYS_statx, fd, c"".as_ptr(), f, mask, &raw mut sx)
        } else {
            libc::syscall(
                libc::SYS_statx,
                at(dir),
                cs(path).p(),
                flags,
                mask,
                &raw mut sx,
            )
        }
    });
    if r < 0 {
        return Err(e);
    }
    let ts = |t: libc::statx_timestamp| [t.tv_sec, i64::from(t.tv_nsec)];
    let [mtime, mtime_ns] = ts(sx.stx_mtime);
    let [atime, atime_ns] = ts(sx.stx_atime);
    let [ctime, ctime_ns] = ts(sx.stx_ctime);
    let [btime, btime_ns] = ts(sx.stx_btime);
    let born = i64::from(sx.stx_mask & libc::STATX_BTIME != 0);
    let mode = u32::from(sx.stx_mode);
    Ok([
        kind_of(mode),
        sx.stx_size as i64,
        i64::from(mode & 0o7777),
        mtime,
        mtime_ns,
        atime,
        ctime,
        atime_ns,
        ctime_ns,
        born,
        btime,
        btime_ns,
    ])
}

/// `fstatat` (`fd < 0`) or `fstat(fd)`, filling the `GcBuf<i64>` `out`:
/// `[kind, size, mode, mtime_s, mtime_ns, atime_s, ctime_s, atime_ns,
/// ctime_ns, has birth time, btime_s, btime_ns]`, kind 1 file, 2 dir,
/// 3 symlink, 0 other. 0 or `-errno`.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_stat(dir: i32, path: NabiStr, follow: i32, fd: i32, out: *mut u8) -> i32 {
    let flags = if follow != 0 {
        0
    } else {
        libc::AT_SYMLINK_NOFOLLOW
    };
    #[cfg(target_os = "linux")]
    match statx(dir, path, flags, fd) {
        Ok(vals) => {
            // SAFETY: the library passes a live `GcBuf<i64>` of 12 elements.
            unsafe { ptr::copy_nonoverlapping(vals.as_ptr(), out.add(HEADER_SIZE).cast(), 12) };
            return 0;
        }
        // No statx (old kernel, or a sandbox that refuses it).
        Err(libc::ENOSYS | libc::EPERM) => {}
        Err(e) => return -e,
    }
    // SAFETY: zeroed `stat` is a valid out-buffer.
    let mut st: libc::stat = unsafe { core::mem::zeroed() };
    // SAFETY: `path` is a C string; `st` is writable.
    let (r, e) = native(|| unsafe {
        if fd >= 0 {
            libc::fstat(fd, &mut st)
        } else {
            libc::fstatat(at(dir), cs(path).p(), &mut st, flags)
        }
    });
    if r < 0 {
        return -e;
    }
    #[allow(clippy::unnecessary_cast)]
    let kind = kind_of(st.st_mode as u32);
    // macOS keeps the creation time in `stat`.
    #[cfg(target_vendor = "apple")]
    let born = [1, st.st_birthtime, st.st_birthtime_nsec];
    #[cfg(not(target_vendor = "apple"))]
    let born = [0, 0, 0];
    // The fields are i32 or i64 by target.
    #[cfg(unix)]
    #[allow(clippy::unnecessary_cast)]
    let vals = [
        kind,
        st.st_size as i64,
        i64::from(st.st_mode & 0o7777),
        st.st_mtime as i64,
        st.st_mtime_nsec as i64,
        st.st_atime as i64,
        st.st_ctime as i64,
        st.st_atime_nsec as i64,
        st.st_ctime_nsec as i64,
        born[0],
        born[1],
        born[2],
    ];
    #[cfg(target_os = "wasi")]
    let vals = [
        kind,
        st.st_size as i64,
        i64::from(st.st_mode & 0o7777),
        st.st_mtim.tv_sec,
        st.st_mtim.tv_nsec as i64,
        st.st_atim.tv_sec,
        st.st_ctim.tv_sec,
        st.st_atim.tv_nsec as i64,
        st.st_ctim.tv_nsec as i64,
        born[0],
        born[1],
        born[2],
    ];
    // SAFETY: the library passes a live `GcBuf<i64>` of 12 elements.
    unsafe { ptr::copy_nonoverlapping(vals.as_ptr(), out.add(HEADER_SIZE).cast(), vals.len()) };
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_mkdir(dir: i32, path: NabiStr, mode: u32) -> i32 {
    // SAFETY: `path` is a C string.
    let (r, e) = native(|| unsafe { libc::mkdirat(at(dir), cs(path).p(), mode as libc::mode_t) });
    if r < 0 { -e } else { 0 }
}

/// `unlinkat`; `is_dir != 0` removes an empty directory.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_unlink(dir: i32, path: NabiStr, is_dir: i32) -> i32 {
    let flags = if is_dir != 0 { libc::AT_REMOVEDIR } else { 0 };
    // SAFETY: `path` is a C string.
    let (r, e) = native(|| unsafe { libc::unlinkat(at(dir), cs(path).p(), flags) });
    if r < 0 { -e } else { 0 }
}

#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_rename(dir: i32, from: NabiStr, to_dir: i32, to: NabiStr) -> i32 {
    // SAFETY: both are C strings.
    let (r, e) =
        native(|| unsafe { libc::renameat(at(dir), cs(from).p(), at(to_dir), cs(to).p()) });
    if r < 0 { -e } else { 0 }
}

/// `hard == 0`: symlink `link` -> `target`; else hard link.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_link(dir: i32, target: NabiStr, link: NabiStr, hard: i32) -> i32 {
    // SAFETY: both are C strings.
    let (r, e) = native(|| unsafe {
        if hard != 0 {
            libc::linkat(at(dir), cs(target).p(), at(dir), cs(link).p(), 0)
        } else {
            libc::symlinkat(cs(target).p(), at(dir), cs(link).p())
        }
    });
    if r < 0 { -e } else { 0 }
}

/// Open `path` under `dir` for listing: a `DIR*` (its address, zero
/// extended: a 32-bit one may have the top bit set) or `-errno`. The
/// address may read as negative too: Android tags the top byte of heap
/// pointers, so callers take only small negatives as errors.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_opendir(dir: i32, path: NabiStr) -> i64 {
    // SAFETY: `path` is a C string; `fdopendir` owns `fd` on success.
    let (d, e) = native(|| unsafe {
        let fd = libc::openat(
            at(dir),
            cs(path).p(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        );
        if fd < 0 {
            return ptr::null_mut();
        }
        let d = libc::fdopendir(fd);
        if d.is_null() {
            libc::close(fd);
        }
        d
    });
    if d.is_null() {
        -i64::from(e)
    } else {
        d as usize as i64
    }
}

/// The next entry name (skipping `.` / `..`) into `buf[off..off + len]`
/// (room for any name: 1024 bytes): its length, 0 at the end, or `-errno`.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_readdir(d: isize, buf: *mut u8, off: usize, len: usize) -> isize {
    loop {
        // SAFETY: errno reset distinguishes end from failure.
        unsafe { set_errno(0) };
        // SAFETY: `d` is a live `DIR*` from `nabi_os_opendir`.
        let ent = unsafe { libc::readdir(d as *mut libc::DIR) };
        if ent.is_null() {
            return -(errno() as isize);
        }
        // SAFETY: `d_name` is NUL-terminated.
        let name = unsafe { core::ffi::CStr::from_ptr((*ent).d_name.as_ptr()) }.to_bytes();
        if name.len() > len {
            return -(libc::ENAMETOOLONG as isize);
        }
        if name != b"." && name != b".." {
            return put(buf, off, len, name);
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_closedir(d: isize) {
    // SAFETY: `d` is a live `DIR*`.
    unsafe { libc::closedir(d as *mut libc::DIR) };
}

pub(crate) unsafe fn set_errno(v: i32) {
    // SAFETY: errno is this thread's.
    #[cfg(any(target_vendor = "apple", target_os = "freebsd"))]
    unsafe {
        *libc::__error() = v;
    }
    // SAFETY: as above.
    #[cfg(target_os = "android")]
    unsafe {
        *libc::__errno() = v;
    }
    // SAFETY: as above.
    #[cfg(not(any(target_vendor = "apple", target_os = "freebsd", target_os = "android")))]
    unsafe {
        *libc::__errno_location() = v;
    }
}

/// The working directory into `buf[off..off + len]`: its full length or `-errno`.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_getcwd(buf: *mut u8, off: usize, len: usize) -> isize {
    let mut cwd = [0u8; PATH_MAX + 1];
    // SAFETY: `cwd` is writable for its length.
    if unsafe { libc::getcwd(cwd.as_mut_ptr().cast(), cwd.len()) }.is_null() {
        return -(errno() as isize);
    }
    put(buf, off, len, until_nul(&cwd))
}

#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_chdir(path: NabiStr) -> i32 {
    // SAFETY: `path` is a C string.
    ret(unsafe { libc::chdir(cs(path).p()) } as isize) as i32
}

/// `realpath(path)` into `buf[off..off + len]`: its full length or `-errno`.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_realpath(path: NabiStr, buf: *mut u8, off: usize, len: usize) -> isize {
    let mut real = [0u8; PATH_MAX + 1];
    // SAFETY: `real` has PATH_MAX bytes.
    let (p, e) = native(|| unsafe { libc::realpath(cs(path).p(), real.as_mut_ptr().cast()) });
    if p.is_null() {
        return -(e as isize);
    }
    put(buf, off, len, until_nul(&real))
}
