//! Paths and directories: clone, stat, links, listing, cwd.

#[allow(clippy::wildcard_imports)]
use crate::os::windows::*;

/// Copy `from` to a new `to`, both under `dir` (`CopyFileExW`: block
/// cloning where the volume supports it, attributes included). 0 or
/// `-error` (`to` exists, ...) - the caller then copies the bytes.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_clone(dir: i32, from: NabiStr, to: NabiStr) -> i32 {
    let (mut a, mut b) = match (path_w(dir, from), path_w(dir, to)) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(e), _) | (_, Err(e)) => return e,
    };
    // SAFETY: NUL-terminated paths; no progress routine.
    let (ok, e) = native(|| unsafe {
        win::CopyFileExW(
            a.ptr(),
            b.ptr(),
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            win::COPY_FILE_FAIL_IF_EXISTS,
        )
    });
    if ok == 0 { -e } else { 0 }
}

/// 100 ns ticks since 1601 as `(unix seconds, nanoseconds)`.
pub(crate) const fn unix_time(ticks: i64) -> (i64, i64) {
    let t = ticks - sys::FILETIME_UNIX;
    (t.div_euclid(10_000_000), t.rem_euclid(10_000_000) * 100)
}

/// Metadata of `fd` (`>= 0`) or of `path` under `dir`, filling the
/// `GcBuf<i64>` `out`: `[kind, size, mode, mtime_s, mtime_ns, atime_s,
/// ctime_s, atime_ns, ctime_ns, 1, btime_s, btime_ns]`, kind 1 file,
/// 2 dir, 3 symlink (or junction).
/// `mode` is synthesized: `r` for all, `w` unless read-only, `x` on
/// directories. 0 or `-error`.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_stat(dir: i32, path: NabiStr, follow: i32, fd: i32, out: *mut u8) -> i32 {
    let owned;
    let hd = if fd >= 0 {
        h(fd)
    } else {
        let mut p = match path_w(dir, path) {
            Ok(p) => p,
            Err(e) => return e,
        };
        owned = match native(|| open_meta(&mut p, follow != 0)).0 {
            Ok(o) => o,
            Err(e) => return e,
        };
        owned.0
    };
    // SAFETY: zeroed infos are valid out-buffers of their sizes.
    let (mut bi, mut si, mut ti): (
        win::FILE_BASIC_INFO,
        win::FILE_STANDARD_INFO,
        win::FILE_ATTRIBUTE_TAG_INFO,
    ) = unsafe { (zeroed(), zeroed(), zeroed()) };
    // SAFETY: as above.
    let ok = unsafe {
        win::GetFileInformationByHandleEx(
            hd,
            win::FileBasicInfo,
            (&raw mut bi).cast(),
            size_of::<win::FILE_BASIC_INFO>() as DWORD,
        ) != 0
            && win::GetFileInformationByHandleEx(
                hd,
                win::FileStandardInfo,
                (&raw mut si).cast(),
                size_of::<win::FILE_STANDARD_INFO>() as DWORD,
            ) != 0
            && win::GetFileInformationByHandleEx(
                hd,
                win::FileAttributeTagInfo,
                (&raw mut ti).cast(),
                size_of::<win::FILE_ATTRIBUTE_TAG_INFO>() as DWORD,
            ) != 0
    };
    if !ok {
        return fail();
    }
    let link = ti.FileAttributes & win::FILE_ATTRIBUTE_REPARSE_POINT != 0
        && (ti.ReparseTag == win::IO_REPARSE_TAG_SYMLINK
            || ti.ReparseTag == win::IO_REPARSE_TAG_MOUNT_POINT);
    let kind = if link && follow == 0 && fd < 0 {
        3
    } else if si.Directory != 0 {
        2
    } else {
        1
    };
    let mut mode = 0o444;
    if bi.FileAttributes & win::FILE_ATTRIBUTE_READONLY == 0 {
        mode |= 0o222;
    }
    if kind == 2 {
        mode |= 0o111;
    }
    let (m, mn) = unix_time(bi.LastWriteTime);
    let (a, an) = unix_time(bi.LastAccessTime);
    let (c, cn) = unix_time(bi.ChangeTime);
    let (b, bn) = unix_time(bi.CreationTime);
    let vals = [kind, si.EndOfFile, mode, m, mn, a, c, an, cn, 1, b, bn];
    // SAFETY: the library passes a live `GcBuf<i64>` of 12 elements.
    unsafe { ptr::copy_nonoverlapping(vals.as_ptr(), out.add(HEADER_SIZE).cast(), vals.len()) };
    0
}

/// Run `f(path)` on `path` under `dir`, parked: 0 or `-error`.
pub(crate) fn on_path(dir: i32, path: NabiStr, f: impl FnOnce(*const u16) -> win::BOOL) -> i32 {
    let mut p = match path_w(dir, path) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let (ok, e) = native(|| f(p.ptr()));
    if ok == 0 { -e } else { 0 }
}

#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_mkdir(dir: i32, path: NabiStr, _mode: u32) -> i32 {
    // SAFETY: a NUL-terminated path.
    on_path(dir, path, |p| unsafe {
        win::CreateDirectoryW(p, ptr::null_mut())
    })
}

/// Remove a file (`is_dir == 0`; a symlink to a directory included) or
/// an empty directory.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_unlink(dir: i32, path: NabiStr, is_dir: i32) -> i32 {
    // SAFETY: a NUL-terminated path.
    on_path(dir, path, |p| unsafe {
        if is_dir != 0 {
            return win::RemoveDirectoryW(p);
        }
        let ok = win::DeleteFileW(p);
        if ok != 0 || win::GetLastError() as i32 != win::ERROR_ACCESS_DENIED {
            return ok;
        }
        let a = win::GetFileAttributesW(p);
        let link_dir = win::FILE_ATTRIBUTE_DIRECTORY | win::FILE_ATTRIBUTE_REPARSE_POINT;
        if a != win::INVALID_FILE_ATTRIBUTES && a & link_dir == link_dir {
            return win::RemoveDirectoryW(p);
        }
        win::SetLastError(win::ERROR_ACCESS_DENIED as DWORD);
        0
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_rename(dir: i32, from: NabiStr, to_dir: i32, to: NabiStr) -> i32 {
    let mut b = match path_w(to_dir, to) {
        Ok(b) => b,
        Err(e) => return e,
    };
    let mut a = match path_w(dir, from) {
        Ok(a) => a,
        Err(e) => return e,
    };
    let (r, _) = native(|| rename_posix(&mut a, &mut b));
    match r {
        Err(
            win::ERROR_INVALID_PARAMETER | win::ERROR_NOT_SUPPORTED | win::ERROR_INVALID_FUNCTION,
        ) => {
            // SAFETY: NUL-terminated paths.
            let (ok, e) = native(|| unsafe {
                win::MoveFileExW(a.ptr(), b.ptr(), win::MOVEFILE_REPLACE_EXISTING)
            });
            if ok == 0 { -e } else { 0 }
        }
        Err(e) => -e,
        Ok(()) => 0,
    }
}

/// Rename `from` over `to` with POSIX semantics: `to` is replaced even
/// while another handle has it open (`MoveFileExW` fails there). A file
/// system without them fails with `ERROR_INVALID_PARAMETER` or
/// `ERROR_NOT_SUPPORTED`.
pub(crate) fn rename_posix(from: &mut Wide, to: &mut Wide) -> Result<(), i32> {
    let mut full = fill_wide(|buf, n| {
        // SAFETY: `buf` is writable for `n` units; `to` is NUL-terminated.
        unsafe { win::GetFullPathNameW(to.ptr(), n, buf, ptr::null_mut()) }
    })?;
    // SAFETY: a NUL-terminated path; a link is renamed, not its target.
    let hd = unsafe {
        win::CreateFileW(
            from.ptr(),
            win::DELETE,
            win::FILE_SHARE_READ | win::FILE_SHARE_WRITE | win::FILE_SHARE_DELETE,
            ptr::null_mut(),
            win::OPEN_EXISTING,
            win::FILE_FLAG_BACKUP_SEMANTICS | win::FILE_FLAG_OPEN_REPARSE_POINT,
            0,
        )
    };
    if hd == win::INVALID_HANDLE_VALUE {
        return Err(errno());
    }
    let hd = Owned(hd);
    let name = full.as_slice().len() * 2;
    let at = core::mem::offset_of!(win::FILE_RENAME_INFO, FileName);
    let n = at + name + 2;
    let mut buf: MBuf<u64> = MBuf::new();
    buf.reserve(n.div_ceil(8));
    // SAFETY: `buf` has room for `n` bytes, 8-aligned; every field and
    // the NUL-terminated name are written before the call reads them.
    let ok = unsafe {
        let info = buf.as_mut_ptr().cast::<win::FILE_RENAME_INFO>();
        (*info).Flags =
            win::FILE_RENAME_FLAG_REPLACE_IF_EXISTS | win::FILE_RENAME_FLAG_POSIX_SEMANTICS;
        (*info).RootDirectory = 0;
        (*info).FileNameLength = name as DWORD;
        let dst = (&raw mut (*info).FileName).cast::<u16>();
        ptr::copy_nonoverlapping(full.ptr(), dst, name / 2 + 1);
        win::SetFileInformationByHandle(hd.0, win::FileRenameInfoEx, info.cast(), n as DWORD)
    };
    if ok == 0 { Err(errno()) } else { Ok(()) }
}

/// `hard == 0`: symlink `link` -> `target` (`target` kept as given,
/// separators as `\`; a directory symlink if it names a directory);
/// else a hard link to `target` under `dir`.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_link(dir: i32, target: NabiStr, link: NabiStr, hard: i32) -> i32 {
    let mut l = match path_w(dir, link) {
        Ok(l) => l,
        Err(e) => return e,
    };
    if hard != 0 {
        let mut t = match path_w(dir, target) {
            Ok(t) => t,
            Err(e) => return e,
        };
        // SAFETY: NUL-terminated paths.
        let (ok, e) = native(|| unsafe { win::CreateHardLinkW(l.ptr(), t.ptr(), ptr::null_mut()) });
        return if ok == 0 { -e } else { 0 };
    }
    let mut t = wide(target.bytes());
    for u in t.as_mut_slice() {
        if *u == u16::from(b'/') {
            *u = u16::from(b'\\');
        }
    }
    // Where `target` points, to tell a directory link.
    let t_bytes = target.bytes();
    let absolute =
        t_bytes.starts_with(b"/") || t_bytes.starts_with(b"\\") || t_bytes.get(1) == Some(&b':');
    let mut at = Wide::new();
    if !absolute {
        let ls = l.as_slice();
        let cut = ls
            .iter()
            .rposition(|&u| u == u16::from(b'\\') || u == u16::from(b'/'));
        if let Some(i) = cut {
            at.extend(&ls[..=i]);
        }
    }
    at.extend(t.as_slice());
    // SAFETY: NUL-terminated paths.
    let (ok, e) = native(|| unsafe {
        let a = win::GetFileAttributesW(at.ptr());
        let mut flags = win::SYMBOLIC_LINK_FLAG_ALLOW_UNPRIVILEGED_CREATE;
        if a != win::INVALID_FILE_ATTRIBUTES && a & win::FILE_ATTRIBUTE_DIRECTORY != 0 {
            flags |= win::SYMBOLIC_LINK_FLAG_DIRECTORY;
        }
        win::CreateSymbolicLinkW(l.ptr(), t.ptr(), flags)
    });
    if ok == 0 { -e } else { 0 }
}

/// A directory listing in progress (`nabi_os_opendir`'s handle).
pub(crate) struct Listing {
    find: HANDLE,
    data: win::WIN32_FIND_DATAW,
    /// `data` holds an entry not handed out yet.
    pending: bool,
}

/// Open `path` under `dir` for listing: a listing (its address, zero
/// extended: i686's may have the top bit set) or `-error`.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_opendir(dir: i32, path: NabiStr) -> i64 {
    let mut p = match path_w(dir, path) {
        Ok(p) => p,
        Err(e) => return i64::from(e),
    };
    if !p
        .as_slice()
        .last()
        .is_some_and(|&u| u == u16::from(b'\\') || u == u16::from(b'/'))
    {
        p.push(u16::from(b'\\'));
    }
    p.push(u16::from(b'*'));
    // SAFETY: a malloc'd listing, freed by `nabi_os_closedir` (or here).
    unsafe {
        let l = libc::malloc(size_of::<Listing>()).cast::<Listing>();
        if l.is_null() {
            nabir_common::oom();
        }
        let (find, e) = native(|| win::FindFirstFileW(p.ptr(), &raw mut (*l).data));
        if find == win::INVALID_HANDLE_VALUE {
            libc::free(l.cast());
            return -i64::from(e);
        }
        (*l).find = find;
        (*l).pending = true;
        l as usize as i64
    }
}

/// The next entry name (skipping `.` / `..`) into `buf[off..off + len]`:
/// its length, 0 at the end, or `-error`.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_readdir(d: isize, buf: *mut u8, off: usize, len: usize) -> isize {
    let l = d as *mut Listing;
    loop {
        // SAFETY: `d` is a live listing from `nabi_os_opendir`.
        unsafe {
            if !(*l).pending && win::FindNextFileW((*l).find, &raw mut (*l).data) == 0 {
                let e = errno();
                return if e == win::ERROR_NO_MORE_FILES {
                    0
                } else {
                    -(e as isize)
                };
            }
            (*l).pending = false;
            let name = &(*l).data.cFileName;
            let n = name.iter().position(|&u| u == 0).unwrap_or(name.len());
            let name = narrow(&name[..n]);
            let name = name.as_slice();
            if name == b"." || name == b".." {
                continue;
            }
            if name.len() > len {
                return -(win::ERROR_FILENAME_EXCED_RANGE as isize);
            }
            return put(buf, off, len, name);
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_closedir(d: isize) {
    let l = d as *mut Listing;
    // SAFETY: a live listing from `nabi_os_opendir`, closed once.
    unsafe {
        win::FindClose((*l).find);
        libc::free(l.cast());
    }
}

/// The working directory into `buf[off..off + len]`: its full length or
/// `-error`.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_getcwd(buf: *mut u8, off: usize, len: usize) -> isize {
    // SAFETY: `b` is writable for `n` units.
    match fill_wide(|b, n| unsafe { win::GetCurrentDirectoryW(n, b) }) {
        Ok(w) => put_wide(buf, off, len, w.as_slice()),
        Err(e) => -(e as isize),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_chdir(path: NabiStr) -> i32 {
    // SAFETY: a NUL-terminated path.
    on_path(-1, path, |p| unsafe { win::SetCurrentDirectoryW(p) })
}

/// The absolute, link-resolved form of `path` into `buf[off..off +
/// len]` (`C:\..` / `\\server\..`, no `\\?\`): its full length or
/// `-error`.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_realpath(path: NabiStr, buf: *mut u8, off: usize, len: usize) -> isize {
    let mut p = wide(path.bytes());
    let r = native(|| open_meta(&mut p, true).and_then(|o| final_path(o.0))).0;
    match r {
        Ok(w) => put_wide(buf, off, len, friendly(w.as_slice()).as_slice()),
        Err(e) => e as isize,
    }
}
