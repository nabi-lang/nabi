//! Descriptor to descriptor copies: kernel copies and the read / write loop.

#[allow(clippy::wildcard_imports)]
use crate::os::posix::*;

/// What a descriptor is, for picking the copy mechanism.
#[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum FdKind {
    File,
    Pipe,
    Sock,
    Other,
}

#[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
pub(crate) fn fd_kind(fd: i32) -> FdKind {
    // SAFETY: zeroed `stat` is a valid out-buffer.
    let mut st: libc::stat = unsafe { core::mem::zeroed() };
    // SAFETY: plain syscall.
    if unsafe { libc::fstat(fd, &raw mut st) } < 0 {
        return FdKind::Other;
    }
    match st.st_mode & libc::S_IFMT {
        libc::S_IFREG => FdKind::File,
        libc::S_IFIFO => FdKind::Pipe,
        libc::S_IFSOCK => FdKind::Sock,
        _ => FdKind::Other,
    }
}

/// Wait until `fd` is ready for `events` (after `EAGAIN`).
pub(crate) fn wait_fd(fd: i32, events: libc::c_short) {
    let mut p = libc::pollfd {
        fd,
        events,
        revents: 0,
    };
    // SAFETY: one valid pollfd; an `EINTR` only retries the copy step.
    unsafe { libc::poll(&raw mut p, 1, -1) };
}

/// After `EAGAIN` from a kernel copy: wait on `dst` if it takes no bytes
/// now, else on `src`.
#[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
pub(crate) fn wait_pair(src: i32, dst: i32) {
    let mut p = libc::pollfd {
        fd: dst,
        events: libc::POLLOUT,
        revents: 0,
    };
    // SAFETY: one valid pollfd, no wait.
    if unsafe { libc::poll(&raw mut p, 1, 0) } == 1 {
        wait_fd(src, libc::POLLIN);
    } else {
        wait_fd(dst, libc::POLLOUT);
    }
}

/// Errors with which a kernel copy declines a pair (kernel, file system,
/// `O_APPEND`, descriptor type) rather than failing it.
#[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
pub(crate) fn declines(e: i32) -> bool {
    [
        libc::EXDEV,
        libc::ENOSYS,
        libc::EINVAL,
        libc::EOPNOTSUPP,
        libc::ENOTSUP,
        libc::EPERM,
        libc::EBADF,
        libc::ENOTSOCK,
    ]
    .contains(&e)
}

/// Repeat the kernel copy `step` (bytes moved, 0 at EOF, or -1 with
/// `errno`) to EOF, counting into `total`; `None` if it declines the
/// pair before a byte moved.
#[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
pub(crate) fn pump(
    src: i32,
    dst: i32,
    total: &mut i64,
    mut step: impl FnMut() -> isize,
) -> Option<Result<i64, i32>> {
    loop {
        let n = step();
        if n > 0 {
            *total += n as i64;
            continue;
        }
        if n == 0 {
            return Some(Ok(*total));
        }
        match errno() {
            libc::EINTR => {}
            libc::EAGAIN => wait_pair(src, dst),
            e if *total == 0 && declines(e) => return None,
            e => return Some(Err(e)),
        }
    }
}

/// The in-kernel copy for this pair of descriptor kinds (Linux, Android):
/// `copy_file_range` file -> file (a reflink or server-side copy where the
/// file system can), `sendfile` file -> anything else, `splice` to or from
/// a pipe, and a socket -> file / socket through a pipe. `None` where it
/// does not apply.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub(crate) fn kernel_copy(src: i32, dst: i32, total: &mut i64) -> Option<Result<i64, i32>> {
    const CHUNK: usize = 1 << 30;
    let null = ptr::null_mut::<libc::loff_t>();
    // SAFETY (each step): null offsets use and advance the descriptors' own.
    match (fd_kind(src), fd_kind(dst)) {
        // bionic has `copy_file_range` only from API 34, and app seccomp
        // filters may refuse the system call: `sendfile` there.
        #[cfg(target_os = "linux")]
        (FdKind::File, FdKind::File) => pump(src, dst, total, || unsafe {
            libc::copy_file_range(src, null, dst, null, CHUNK, 0)
        }),
        (FdKind::File, _) => pump(src, dst, total, || unsafe {
            libc::sendfile(dst, src, ptr::null_mut(), CHUNK)
        }),
        (FdKind::Pipe, _) | (_, FdKind::Pipe) => pump(src, dst, total, || unsafe {
            libc::splice(src, null, dst, null, CHUNK, libc::SPLICE_F_MOVE)
        }),
        (FdKind::Sock, FdKind::File | FdKind::Sock) => splice_through_pipe(src, dst, total),
        _ => None,
    }
}

/// The in-kernel copy for this pair (macOS): `sendfile` file -> socket,
/// from the file's offset, which then moves past what was sent.
#[cfg(target_vendor = "apple")]
pub(crate) fn kernel_copy(src: i32, dst: i32, total: &mut i64) -> Option<Result<i64, i32>> {
    if fd_kind(src) != FdKind::File || fd_kind(dst) != FdKind::Sock {
        return None;
    }
    // SAFETY: plain syscall.
    let start = unsafe { libc::lseek(src, 0, libc::SEEK_CUR) };
    if start < 0 {
        return None;
    }
    let mut off = start;
    let r = pump(src, dst, total, || {
        // 0: to the file's end; on return, what was sent (also on `EAGAIN`).
        let mut len: libc::off_t = 0;
        // SAFETY: plain syscall with a valid length pointer.
        let r = unsafe { libc::sendfile(src, dst, off, &raw mut len, ptr::null_mut(), 0) };
        off += len;
        if len > 0 { len as isize } else { r as isize }
    });
    // SAFETY: plain syscall.
    unsafe { libc::lseek(src, start + *total, libc::SEEK_SET) };
    r
}

#[cfg(not(any(target_os = "linux", target_os = "android", target_vendor = "apple")))]
pub(crate) const fn kernel_copy(
    _src: i32,
    _dst: i32,
    _total: &mut i64,
) -> Option<Result<i64, i32>> {
    None
}

/// Socket -> file / socket with `splice(2)` through a pipe of our own.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub(crate) fn splice_through_pipe(src: i32, dst: i32, total: &mut i64) -> Option<Result<i64, i32>> {
    let mut p = [0i32; 2];
    // SAFETY: `p` holds two fds.
    if unsafe { libc::pipe2(p.as_mut_ptr(), libc::O_CLOEXEC) } < 0 {
        return None;
    }
    let null = ptr::null_mut::<libc::loff_t>();
    let r = loop {
        // SAFETY: plain syscall on the caller's socket and our pipe.
        let n = unsafe { libc::splice(src, null, p[1], null, 1 << 16, libc::SPLICE_F_MOVE) };
        if n == 0 {
            break Some(Ok(*total));
        }
        if n < 0 {
            match errno() {
                libc::EINTR => {}
                libc::EAGAIN => wait_fd(src, libc::POLLIN),
                e if *total == 0 && declines(e) => break None,
                e => break Some(Err(e)),
            }
            continue;
        }
        // What entered the pipe must leave it: from here on a refusal is
        // an error, not a reason to fall back.
        let (mut left, mut err) = (n as usize, 0);
        while left > 0 && err == 0 {
            // SAFETY: as above.
            let k = unsafe { libc::splice(p[0], null, dst, null, left, libc::SPLICE_F_MOVE) };
            if k > 0 {
                left -= k as usize;
                continue;
            }
            err = if k == 0 { libc::EIO } else { errno() };
            match err {
                libc::EINTR => err = 0,
                libc::EAGAIN => {
                    wait_fd(dst, libc::POLLOUT);
                    err = 0;
                }
                _ => {}
            }
        }
        if err != 0 {
            break Some(Err(err));
        }
        *total += n as i64;
    };
    // SAFETY: our pipe.
    unsafe {
        libc::close(p[0]);
        libc::close(p[1]);
    }
    r
}

/// The read / write loop: pairs no kernel copy takes.
pub(crate) fn rw_copy(src: i32, dst: i32, mut total: i64) -> Result<i64, i32> {
    let mut buf = [0u8; 16 * 1024];
    loop {
        // SAFETY: `buf` is writable for its length.
        let n = unsafe { libc::read(src, buf.as_mut_ptr().cast(), buf.len()) };
        if n == 0 {
            return Ok(total);
        }
        if n < 0 {
            match errno() {
                libc::EINTR => {}
                libc::EAGAIN => wait_fd(src, libc::POLLIN),
                e => return Err(e),
            }
            continue;
        }
        let mut off = 0;
        while off < n as usize {
            // SAFETY: `buf[off..n]` is initialized.
            let w = unsafe { libc::write(dst, buf.as_ptr().add(off).cast(), n as usize - off) };
            if w < 0 {
                match errno() {
                    libc::EINTR => {}
                    libc::EAGAIN => wait_fd(dst, libc::POLLOUT),
                    e => return Err(e),
                }
                continue;
            }
            off += w as usize;
        }
        total += n as i64;
    }
}
