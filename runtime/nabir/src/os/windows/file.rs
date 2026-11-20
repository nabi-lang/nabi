//! Handles as files: open, read / write, sync, mmap and locks.

#[allow(clippy::wildcard_imports)]
use crate::os::windows::*;

/// `CreateFileW` for one of the library's open functions: `how` fixes
/// the access and the disposition. Create-or-truncate is `OPEN_ALWAYS`
/// and then a truncate when the file existed, so hidden and system files
/// keep their attributes (`CREATE_ALWAYS` resets them, or fails on them).
/// A created file is read-only when `mode` has no write bit. Never
/// inherited by children.
///
/// # Safety
/// `how` must be a live `GcBuf<u32>` of the library's `OpenHow` slots.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nabi_os_open(dir: i32, path: NabiStr, how: *const u8) -> i32 {
    // SAFETY: caller contract.
    let how = unsafe { How::read(how) };
    let access = match how.access {
        1 => win::GENERIC_WRITE,
        2 => win::GENERIC_READ | win::GENERIC_WRITE,
        3 => APPEND_ACCESS,
        4 => win::GENERIC_READ | APPEND_ACCESS,
        _ => win::GENERIC_READ,
    };
    let disposition = match how.disposition {
        1 | 3 => win::OPEN_ALWAYS,
        2 => win::CREATE_NEW,
        _ => win::OPEN_EXISTING,
    };
    let mut flags = how.attrs | how.custom;
    if how.disposition != 0 && how.mode & 0o222 == 0 {
        flags |= win::FILE_ATTRIBUTE_READONLY;
    }
    if how.sqos != 0 {
        flags |= win::SECURITY_SQOS_PRESENT | how.sqos;
    }
    if how.directory {
        flags |= win::FILE_FLAG_BACKUP_SEMANTICS;
    }
    if how.nofollow {
        flags |= win::FILE_FLAG_OPEN_REPARSE_POINT;
    }
    let mut p = match path_w(dir, path) {
        Ok(p) => p,
        Err(e) => return e,
    };
    // SAFETY: a NUL-terminated path.
    let (hd, e) = native(|| unsafe {
        win::CreateFileW(
            p.ptr(),
            access,
            how.share,
            ptr::null_mut(),
            disposition,
            flags,
            0,
        )
    });
    if hd == win::INVALID_HANDLE_VALUE {
        // Without FILE_FLAG_BACKUP_SEMANTICS a directory is ACCESS_DENIED.
        // A link not followed is the link, a directory's included.
        if e == win::ERROR_ACCESS_DENIED && !how.directory {
            // SAFETY: a NUL-terminated path.
            let a = unsafe { win::GetFileAttributesW(p.ptr()) };
            if a != win::INVALID_FILE_ATTRIBUTES {
                if how.nofollow && a & win::FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                    return -win::ERROR_STOPPED_ON_SYMLINK;
                }
                if a & win::FILE_ATTRIBUTE_DIRECTORY != 0 {
                    return -win::ERROR_DIRECTORY_NOT_SUPPORTED;
                }
            }
        }
        return -e;
    }
    let hd = Owned(hd);
    if how.nofollow && is_symlink(hd.0) {
        return -win::ERROR_STOPPED_ON_SYMLINK;
    }
    if how.directory && !is_dir(hd.0) {
        return -win::ERROR_DIRECTORY;
    }
    if how.disposition == 1 && e == win::ERROR_ALREADY_EXISTS {
        if let Err(e) = truncate_existing(hd.0, how.attrs) {
            return e;
        }
    }
    let raw = hd.0;
    core::mem::forget(hd);
    raw as i32
}

/// Empty a file `OPEN_ALWAYS` found, and merge `attrs` into its
/// attributes as `CREATE_ALWAYS` would.
pub(crate) fn truncate_existing(hd: HANDLE, attrs: DWORD) -> Result<(), i32> {
    let end = 0i64;
    // SAFETY: an 8-byte `FILE_END_OF_FILE_INFO`.
    let ok = unsafe {
        win::SetFileInformationByHandle(hd, win::FileEndOfFileInfo, (&raw const end).cast(), 8)
    };
    if ok == 0 {
        return Err(fail());
    }
    if attrs == 0 {
        return Ok(());
    }
    // SAFETY: zeroed info is a valid out-buffer.
    let mut bi: win::FILE_BASIC_INFO = unsafe { zeroed() };
    let n = size_of::<win::FILE_BASIC_INFO>() as DWORD;
    // SAFETY: valid buffers of their size.
    unsafe {
        if win::GetFileInformationByHandleEx(hd, win::FileBasicInfo, (&raw mut bi).cast(), n) == 0 {
            return Err(fail());
        }
        bi.FileAttributes |= attrs;
        // Zero times mean "unchanged".
        bi.CreationTime = 0;
        bi.LastAccessTime = 0;
        bi.LastWriteTime = 0;
        bi.ChangeTime = 0;
        if win::SetFileInformationByHandle(hd, win::FileBasicInfo, (&raw const bi).cast(), n) == 0 {
            return Err(fail());
        }
    }
    Ok(())
}

/// Whether the open handle `hd` is a symbolic link (opened with
/// `FILE_FLAG_OPEN_REPARSE_POINT`).
pub(crate) fn is_symlink(hd: HANDLE) -> bool {
    // SAFETY: zeroed info is a valid out-buffer.
    let mut ti: win::FILE_ATTRIBUTE_TAG_INFO = unsafe { zeroed() };
    // SAFETY: valid out-buffer of its size.
    let ok = unsafe {
        win::GetFileInformationByHandleEx(
            hd,
            win::FileAttributeTagInfo,
            (&raw mut ti).cast(),
            size_of::<win::FILE_ATTRIBUTE_TAG_INFO>() as DWORD,
        )
    };
    ok != 0
        && ti.FileAttributes & win::FILE_ATTRIBUTE_REPARSE_POINT != 0
        && ti.ReparseTag == win::IO_REPARSE_TAG_SYMLINK
}

/// `CreateFile2` with the caller's parameters as given
/// (`DirExt::create_file`): `p` is `[access, disposition, share,
/// attributes, flags, sqos]`. Never inherited by children.
///
/// # Safety
/// `p` must be a live `GcBuf<u32>` of 6 elements.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nabi_os_create_file2(dir: i32, path: NabiStr, p: *const u8) -> i32 {
    let mut v = [0u32; 6];
    // SAFETY: caller contract.
    unsafe { ptr::copy_nonoverlapping(p.add(HEADER_SIZE).cast(), v.as_mut_ptr(), 6) };
    let mut w = match path_w(dir, path) {
        Ok(w) => w,
        Err(e) => return e,
    };
    let ext = win::CREATEFILE2_EXTENDED_PARAMETERS {
        dwSize: size_of::<win::CREATEFILE2_EXTENDED_PARAMETERS>() as DWORD,
        dwFileAttributes: v[3],
        dwFileFlags: v[4],
        dwSecurityQosFlags: if v[5] != 0 {
            win::SECURITY_SQOS_PRESENT | v[5]
        } else {
            0
        },
        lpSecurityAttributes: ptr::null_mut(),
        hTemplateFile: 0,
    };
    // SAFETY: a NUL-terminated path and a filled-in parameter block.
    let (hd, e) = native(|| unsafe { win::CreateFile2(w.ptr(), v[0], v[2], v[1], &raw const ext) });
    if hd == win::INVALID_HANDLE_VALUE {
        -e
    } else {
        hd as i32
    }
}

/// Whether the open handle `hd` is a directory.
pub(crate) fn is_dir(hd: HANDLE) -> bool {
    // SAFETY: zeroed info is a valid out-buffer.
    let mut si: win::FILE_STANDARD_INFO = unsafe { zeroed() };
    // SAFETY: valid out-buffer of its size.
    let ok = unsafe {
        win::GetFileInformationByHandleEx(
            hd,
            win::FileStandardInfo,
            (&raw mut si).cast(),
            size_of::<win::FILE_STANDARD_INFO>() as DWORD,
        )
    };
    ok != 0 && si.Directory != 0
}

#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_close(fd: i32) -> i32 {
    if let Some(s) = sock_of(fd) {
        return crate::net::close_socket(s);
    }
    // SAFETY: closing a handle the library owns.
    if unsafe { win::CloseHandle(h(fd)) } == 0 {
        fail()
    } else {
        0
    }
}

/// One read into `buf[off..off + len]`: the count (0 = EOF, including a
/// pipe whose writer closed) or `-error`.
///
/// # Safety
/// `buf` must be a live `GcBuf<u8>` with at least `off + len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nabi_os_read(fd: i32, buf: *mut u8, off: usize, len: usize) -> isize {
    // SAFETY: caller contract.
    let dst = unsafe { region(buf, off) };
    if let Some(s) = sock_of(fd) {
        return crate::net::recv(s, dst, len);
    }
    if fd == 0 {
        return crate::stdio::read0(dst, len);
    }
    let mut n = 0;
    let want = len.min(1 << 30) as DWORD;
    // SAFETY: `dst` is writable for `want` bytes.
    let (ok, e) = native(|| unsafe { read_handle(h(fd), dst, want, &mut n) });
    match (ok, e) {
        (0, win::ERROR_BROKEN_PIPE) => 0,
        (0, e) => -(e as isize),
        _ => n as isize,
    }
}

/// One write of `buf[off..off + len]`: the count or `-error`.
///
/// # Safety
/// `buf` must be a live `GcBuf<u8>` with at least `off + len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nabi_os_write(fd: i32, buf: *mut u8, off: usize, len: usize) -> isize {
    // SAFETY: caller contract.
    let src = unsafe { region(buf, off) };
    if let Some(s) = sock_of(fd) {
        return crate::net::send(s, src, len);
    }
    let hd = h(fd);
    if sys::is_console(hd) {
        // SAFETY: `src` is readable for `len` bytes.
        let bytes = unsafe { core::slice::from_raw_parts(src, len) };
        let r = native(|| sys::write_all(fd, &[bytes])).0;
        return if r < 0 { r as isize } else { len as isize };
    }
    let mut n = 0;
    let want = len.min(1 << 30) as DWORD;
    // SAFETY: `src` is readable for `want` bytes.
    let (ok, e) =
        native(|| unsafe { win::WriteFile(hd, src.cast(), want, &mut n, ptr::null_mut()) });
    if ok == 0 { -(e as isize) } else { n as isize }
}

/// Wait until pipe `fd1` or `fd2` (negative: ignored) has data or its
/// writer closed: bit 0 / bit 1 set for each ready one, or `-error`.
///
/// A child's output pipes are overlapped ([`output_pipe`]): a zero-byte
/// read on each completes when data arrives or the writer closes, and
/// the wait takes whichever comes first. A handle that is not an
/// overlapped pipe counts as ready (a read just blocks).
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_poll_read(fd1: i32, fd2: i32) -> i32 {
    let fds = [fd1, fd2];
    // Data or the writer gone, or a handle this cannot wait on.
    let ready = |fd: i32| {
        if fd < 0 {
            return false;
        }
        let mut avail = 0;
        // SAFETY: valid out-pointer; no data is read.
        let ok = unsafe {
            win::PeekNamedPipe(
                h(fd),
                ptr::null_mut(),
                0,
                ptr::null_mut(),
                &mut avail,
                ptr::null_mut(),
            )
        };
        ok == 0 || avail > 0 || !overlapped(h(fd))
    };
    native(|| {
        loop {
            let r = i32::from(ready(fd1)) | (i32::from(ready(fd2)) << 1);
            if r != 0 || (fd1 < 0 && fd2 < 0) {
                break r;
            }
            // SAFETY: zeroed requests; each is finished (waited for or
            // cancelled and waited for) before its event closes.
            unsafe {
                let mut ovs: [win::OVERLAPPED; 2] = core::mem::zeroed();
                let mut events = [0 as HANDLE; 2];
                let mut pending = [false; 2];
                let mut k = 0;
                let mut dummy = 0u8;
                for (i, &fd) in fds.iter().enumerate() {
                    if fd < 0 {
                        continue;
                    }
                    ovs[i].hEvent = win::CreateEventW(ptr::null_mut(), 1, 0, ptr::null());
                    let mut n = 0;
                    let ok = win::ReadFile(h(fd), (&raw mut dummy).cast(), 0, &mut n, &mut ovs[i]);
                    if ok == 0 && errno() == win::ERROR_IO_PENDING {
                        pending[i] = true;
                        events[k] = ovs[i].hEvent;
                        k += 1;
                    }
                }
                if pending.iter().filter(|&&p| p).count() == fds.iter().filter(|&&f| f >= 0).count()
                {
                    win::WaitForMultipleObjects(k as DWORD, events.as_ptr(), 0, win::INFINITE);
                }
                for (i, &fd) in fds.iter().enumerate() {
                    if fd < 0 {
                        continue;
                    }
                    if pending[i] {
                        let mut n = 0;
                        win::CancelIoEx(h(fd), &mut ovs[i]);
                        win::GetOverlappedResult(h(fd), &mut ovs[i], &mut n, 1);
                    }
                    win::CloseHandle(ovs[i].hEvent);
                }
            }
        }
    })
    .0
}

/// `FlushFileBuffers` (data and metadata either way).
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_fsync(fd: i32, _data_only: i32) -> i32 {
    // SAFETY: plain call.
    let (ok, e) = native(|| unsafe { win::FlushFileBuffers(h(fd)) });
    if ok == 0 { -e } else { 0 }
}

#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_ftruncate(fd: i32, len: u64) -> i32 {
    let end = len as i64;
    // SAFETY: an 8-byte `FILE_END_OF_FILE_INFO`.
    let ok = unsafe {
        win::SetFileInformationByHandle(h(fd), win::FileEndOfFileInfo, (&raw const end).cast(), 8)
    };
    if ok == 0 { fail() } else { 0 }
}

/// `SetFilePointerEx`; `whence` 0 start, 1 current, 2 end (the Win32
/// numbering too). New offset or `-error`.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_lseek(fd: i32, off: i64, whence: i32) -> i64 {
    let mut at = 0;
    // SAFETY: valid out-pointer.
    if unsafe { win::SetFilePointerEx(h(fd), off, &mut at, whence.clamp(0, 2) as DWORD) } == 0 {
        return i64::from(fail());
    }
    at
}

/// A second handle to the same file (or socket), not inherited.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_dup(fd: i32) -> i32 {
    if let Some(s) = sock_of(fd) {
        return crate::net::dup_socket(s);
    }
    let mut out = 0;
    // SAFETY: duplicates within this process.
    let ok = unsafe {
        let me = win::GetCurrentProcess();
        win::DuplicateHandle(me, h(fd), me, &mut out, 0, 0, win::DUPLICATE_SAME_ACCESS)
    };
    if ok == 0 { fail() } else { out as i32 }
}

/// Permissions as far as Windows has them: no write bit for anyone =
/// the read-only attribute. Goes through a second handle with
/// attribute-write access, so `fd`'s own access does not matter.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_fchmod(fd: i32, mode: u32) -> i32 {
    // SAFETY: a fresh handle to the same file, closed by `Owned`.
    let hd = unsafe {
        win::ReOpenFile(
            h(fd),
            win::FILE_READ_ATTRIBUTES | win::FILE_WRITE_ATTRIBUTES,
            win::FILE_SHARE_READ | win::FILE_SHARE_WRITE | win::FILE_SHARE_DELETE,
            0,
        )
    };
    if hd == win::INVALID_HANDLE_VALUE {
        return fail();
    }
    let hd = Owned(hd);
    // SAFETY: zeroed info is a valid out-buffer.
    let mut bi: win::FILE_BASIC_INFO = unsafe { zeroed() };
    let n = size_of::<win::FILE_BASIC_INFO>() as DWORD;
    // SAFETY: valid buffers of their size.
    unsafe {
        if win::GetFileInformationByHandleEx(hd.0, win::FileBasicInfo, (&raw mut bi).cast(), n) == 0
        {
            return fail();
        }
        if mode & 0o222 == 0 {
            bi.FileAttributes |= win::FILE_ATTRIBUTE_READONLY;
        } else {
            bi.FileAttributes &= !win::FILE_ATTRIBUTE_READONLY;
        }
        // Zero times mean "unchanged".
        bi.CreationTime = 0;
        bi.LastAccessTime = 0;
        bi.LastWriteTime = 0;
        bi.ChangeTime = 0;
        if win::SetFileInformationByHandle(hd.0, win::FileBasicInfo, (&raw const bi).cast(), n) == 0
        {
            return fail();
        }
    }
    0
}

/// `nabi_os_fchmod` by path: the read-only attribute of `path` under
/// `dir` (a final symlink is followed).
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_chmod(dir: i32, path: NabiStr, mode: u32) -> i32 {
    let mut p = match path_w(dir, path) {
        Ok(p) => p,
        Err(e) => return e,
    };
    // SAFETY: a NUL-terminated path.
    let hd = unsafe {
        win::CreateFileW(
            p.ptr(),
            win::FILE_READ_ATTRIBUTES | win::FILE_WRITE_ATTRIBUTES,
            win::FILE_SHARE_READ | win::FILE_SHARE_WRITE | win::FILE_SHARE_DELETE,
            ptr::null_mut(),
            win::OPEN_EXISTING,
            win::FILE_FLAG_BACKUP_SEMANTICS,
            0,
        )
    };
    if hd == win::INVALID_HANDLE_VALUE {
        return fail();
    }
    let hd = Owned(hd);
    nabi_os_fchmod(hd.0 as i32, mode)
}

/// The allocation granularity (64 KiB): map view offsets are multiples.
pub(crate) fn granularity() -> usize {
    static G: AtomicUsize = AtomicUsize::new(0);
    let g = G.load(Ordering::Relaxed);
    if g != 0 {
        return g;
    }
    // SAFETY: zeroed info is a valid out-buffer.
    let mut si: win::SYSTEM_INFO = unsafe { zeroed() };
    // SAFETY: valid out-pointer.
    unsafe { win::GetSystemInfo(&mut si) };
    let g = si.dwAllocationGranularity as usize;
    G.store(g, Ordering::Relaxed);
    g
}

/// A read-only view of `len` bytes of `fd` at `offset`: its address, or
/// `-error`. The view starts at the granularity boundary below `offset`.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_mmap(fd: i32, len: usize, offset: u64) -> i64 {
    if len == 0 {
        return -i64::from(win::ERROR_INVALID_PARAMETER);
    }
    let delta = (offset % granularity() as u64) as usize;
    let base = offset - delta as u64;
    // SAFETY: a mapping object for the file, closed once the view holds
    // its own reference.
    let (p, e) = native(|| unsafe {
        let m = win::CreateFileMappingW(
            h(fd),
            ptr::null_mut(),
            win::PAGE_READONLY,
            0,
            0,
            ptr::null(),
        );
        if m == 0 {
            return ptr::null_mut();
        }
        let p = win::MapViewOfFile(
            m,
            win::FILE_MAP_READ,
            (base >> 32) as DWORD,
            base as DWORD,
            len + delta,
        );
        let e = errno();
        win::CloseHandle(m);
        win::SetLastError(e as DWORD);
        p
    });
    if p.is_null() {
        -i64::from(e)
    } else {
        (p as usize + delta) as i64
    }
}

/// Unmap a `nabi_os_mmap` view: 0 or `-error`.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_munmap(addr: usize, _len: usize) -> i32 {
    let base = addr & !(granularity() - 1);
    // SAFETY: `base` is the view `nabi_os_mmap` mapped.
    if unsafe { win::UnmapViewOfFile(base as *const c_void) } == 0 {
        fail()
    } else {
        0
    }
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
    unsafe { ptr::copy_nonoverlapping((addr + at) as *const u8, region(buf, off), len) };
}

/// Whole-file lock: `op` 1 shared, 2 exclusive, 8 unlock, | 4
/// non-blocking. 1 = locked, 0 = would block (non-blocking), `-error`.
/// Like `flock`, changing a held lock's kind unlocks first.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_flock(fd: i32, op: i32) -> i32 {
    let hd = h(fd);
    // SAFETY: zeroed OVERLAPPED = offset 0; the handle is synchronous.
    let mut ov: win::OVERLAPPED = unsafe { zeroed() };
    // SAFETY: as above; unlocking an unlocked range just fails.
    unsafe { win::UnlockFileEx(hd, 0, u32::MAX, u32::MAX, &mut ov) };
    if op & 8 != 0 {
        return 1;
    }
    let mut flags = 0;
    if op & 2 != 0 {
        flags |= win::LOCKFILE_EXCLUSIVE_LOCK;
    }
    if op & 4 != 0 {
        flags |= win::LOCKFILE_FAIL_IMMEDIATELY;
    }
    // SAFETY: as above.
    let (ok, e) = native(|| unsafe { win::LockFileEx(hd, flags, 0, u32::MAX, u32::MAX, &mut ov) });
    match (ok, e) {
        (0, win::ERROR_LOCK_VIOLATION) if op & 4 != 0 => 0,
        (0, e) => -e,
        _ => 1,
    }
}

/// Copy `src` -> `dst` from their current offsets to EOF: bytes or `-error`.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_copy_fd(src: i32, dst: i32) -> i64 {
    let (r, e) = native(|| {
        let mut buf = [0u8; 16 * 1024];
        let mut total: i64 = 0;
        loop {
            let mut n = 0;
            // SAFETY: `buf` is writable for its length.
            let ok = unsafe { read_handle(h(src), buf.as_mut_ptr(), buf.len() as DWORD, &mut n) };
            if ok == 0 && errno() != win::ERROR_BROKEN_PIPE {
                return -1;
            }
            if ok == 0 || n == 0 {
                return total;
            }
            if sys::write_handle(h(dst), &buf[..n as usize]) < 0 {
                return -1;
            }
            total += i64::from(n);
        }
    });
    if r < 0 { -i64::from(e) } else { r }
}
