//! Unix sockets and fd passing.

#[allow(clippy::wildcard_imports)]
use crate::net::posix::*;

/// `path` as a `sockaddr_un` and its length; `None` when it does not fit
/// `sun_path` (with its NUL).
#[cfg(unix)]
#[allow(clippy::unnecessary_cast)] // `c_char` signedness varies by target
pub(crate) fn unix_addr(path: &[u8]) -> Option<(libc::sockaddr_un, libc::socklen_t)> {
    // SAFETY: zeroed is a valid `sockaddr_un`.
    let mut sa: libc::sockaddr_un = unsafe { zeroed() };
    if path.len() >= sa.sun_path.len() {
        return None;
    }
    sa.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (d, s) in sa.sun_path.iter_mut().zip(path) {
        *d = *s as libc::c_char;
    }
    let len = core::mem::offset_of!(libc::sockaddr_un, sun_path) + path.len() + 1;
    #[cfg(any(target_vendor = "apple", target_os = "freebsd"))]
    {
        sa.sun_len = len as u8;
    }
    Some((sa, len as libc::socklen_t))
}

/// A Unix socket (`kind` 1 stream / 2 datagram) at `path`: `op` 0 leaves
/// it unbound, 1 binds it (and listens, backlog 128, for a stream), 2
/// connects it (blocking, before it turns non-blocking). The fd or
/// `-errno` (`-ENAMETOOLONG` for a path `sun_path` cannot hold).
#[cfg(unix)]
#[unsafe(no_mangle)]
pub extern "C" fn nabi_net_unix(kind: i32, path: NabiStr, op: i32) -> i32 {
    let ty = if kind == 2 {
        libc::SOCK_DGRAM
    } else {
        libc::SOCK_STREAM
    };
    let Some((sa, len)) = unix_addr(path.bytes()) else {
        return -libc::ENAMETOOLONG;
    };
    // SAFETY: plain libc calls on our own fd and a valid sockaddr.
    unsafe {
        let fd = libc::socket(libc::AF_UNIX, ty, 0);
        if fd < 0 {
            return -errno();
        }
        let addr = (&raw const sa).cast::<libc::sockaddr>();
        let r = match op {
            1 => {
                let r = libc::bind(fd, addr, len);
                if r == 0 && ty == libc::SOCK_STREAM {
                    libc::listen(fd, 128)
                } else {
                    r
                }
            }
            2 => loop {
                let (r, e) = native(|| libc::connect(fd, addr, len));
                if r == 0 || e != libc::EINTR {
                    break r;
                }
            },
            _ => 0,
        };
        let r = if r < 0 { -errno() } else { prepare(fd) };
        if r < 0 {
            libc::close(fd);
            return r;
        }
        fd
    }
}

/// `socketpair(AF_UNIX, kind)`, both ends non-blocking + close-on-exec,
/// into the `GcBuf<i32>` `out`: 0 or `-errno`.
#[cfg(unix)]
#[unsafe(no_mangle)]
#[allow(clippy::cast_ptr_alignment)] // GC payloads are 8-aligned
pub extern "C" fn nabi_net_unix_pair(kind: i32, out: *mut u8) -> i32 {
    let ty = if kind == 2 {
        libc::SOCK_DGRAM
    } else {
        libc::SOCK_STREAM
    };
    let mut fds = [0i32; 2];
    // SAFETY: `fds` holds two fds; `out` is a live two-element buffer.
    unsafe {
        if libc::socketpair(libc::AF_UNIX, ty, 0, fds.as_mut_ptr()) < 0 {
            return -errno();
        }
        for fd in fds {
            let r = prepare(fd);
            if r < 0 {
                libc::close(fds[0]);
                libc::close(fds[1]);
                return r;
            }
        }
        let o = out.add(HEADER_SIZE).cast::<i32>();
        *o = fds[0];
        *o.add(1) = fds[1];
    }
    0
}

/// The local (`peer == 0`) or peer path of Unix socket `fd` into
/// `buf[off..off + len]`: its length (0: unnamed) or `-errno`.
///
/// # Safety
/// `buf` must be a live `GcBuf<u8>` with at least `off + len` bytes.
#[cfg(unix)]
#[unsafe(no_mangle)]
#[allow(clippy::unnecessary_cast)] // `c_char` signedness varies by target
pub unsafe extern "C" fn nabi_net_unix_path(
    fd: i32,
    peer: i32,
    buf: *mut u8,
    off: usize,
    len: usize,
) -> isize {
    // SAFETY: zeroed storage is a valid out-buffer; caller contract on
    // `buf`.
    unsafe {
        let mut sa: libc::sockaddr_un = zeroed();
        let mut n = size_of::<libc::sockaddr_un>() as libc::socklen_t;
        let p = (&raw mut sa).cast::<libc::sockaddr>();
        let r = if peer != 0 {
            libc::getpeername(fd, p, &raw mut n)
        } else {
            libc::getsockname(fd, p, &raw mut n)
        };
        if r < 0 {
            let e = errno();
            // A peer that already hung up has no name to report (macOS
            // says EINVAL, Linux ENOTCONN): unnamed.
            if peer != 0 && (e == libc::EINVAL || e == libc::ENOTCONN) {
                return 0;
            }
            return -(e as isize);
        }
        let start = core::mem::offset_of!(libc::sockaddr_un, sun_path);
        let room = (n as usize).saturating_sub(start).min(sa.sun_path.len());
        let path = &sa.sun_path[..room];
        let k = path.iter().position(|c| *c == 0).unwrap_or(room).min(len);
        let dst = region(buf, off);
        for (i, c) in path[..k].iter().enumerate() {
            *dst.add(i) = *c as u8;
        }
        k as isize
    }
}

/// Room for one fd in a control message.
#[cfg(unix)]
#[repr(C)]
pub(crate) union FdCmsg {
    buf: [u8; 64],
    _align: libc::cmsghdr,
}

/// `buf[off..off + len]` with fd `pass` as `SCM_RIGHTS`: the count or
/// `-errno`.
///
/// # Safety
/// `buf` must be a live `GcBuf<u8>` with at least `off + len` bytes.
#[cfg(unix)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nabi_net_send_fd(
    fd: i32,
    buf: *mut u8,
    off: usize,
    len: usize,
    pass: i32,
) -> isize {
    // SAFETY: the control buffer has room for one fd's cmsg; caller
    // contract on `buf`.
    unsafe {
        let mut iov = libc::iovec {
            iov_base: region(buf, off).cast(),
            iov_len: len,
        };
        let mut ctl = FdCmsg { buf: [0; 64] };
        let mut msg: libc::msghdr = zeroed();
        msg.msg_iov = &raw mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = (&raw mut ctl).cast();
        msg.msg_controllen = libc::CMSG_SPACE(size_of::<i32>() as u32) as _;
        let c = libc::CMSG_FIRSTHDR(&raw const msg);
        (*c).cmsg_level = libc::SOL_SOCKET;
        (*c).cmsg_type = libc::SCM_RIGHTS;
        (*c).cmsg_len = libc::CMSG_LEN(size_of::<i32>() as u32) as _;
        libc::CMSG_DATA(c).cast::<i32>().write_unaligned(pass);
        no_eintr(|| libc::sendmsg(fd, &raw const msg, SEND_FLAGS))
    }
}

/// One receive into `buf[off..off + len]`; a passed fd (made
/// close-on-exec) goes to `out[0]`, else -1. The count or `-errno`.
///
/// # Safety
/// `buf` must be a live `GcBuf<u8>` with at least `off + len` bytes and
/// `out` a live one-element `GcBuf<i32>`.
#[cfg(unix)]
#[unsafe(no_mangle)]
#[allow(clippy::cast_ptr_alignment)] // GC payloads are 8-aligned
pub unsafe extern "C" fn nabi_net_recv_fd(
    fd: i32,
    buf: *mut u8,
    off: usize,
    len: usize,
    out: *mut u8,
) -> isize {
    // SAFETY: as for `nabi_net_send_fd`; caller contract on `out`.
    unsafe {
        let got = out.add(HEADER_SIZE).cast::<i32>();
        *got = -1;
        let mut iov = libc::iovec {
            iov_base: region(buf, off).cast(),
            iov_len: len,
        };
        let mut ctl = FdCmsg { buf: [0; 64] };
        let mut msg: libc::msghdr = zeroed();
        msg.msg_iov = &raw mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = (&raw mut ctl).cast();
        msg.msg_controllen = size_of::<FdCmsg>() as _;
        #[cfg(any(target_os = "linux", target_os = "android"))]
        let flags = libc::MSG_CMSG_CLOEXEC;
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        let flags = 0;
        let n = no_eintr(|| libc::recvmsg(fd, &raw mut msg, flags));
        if n < 0 {
            return n;
        }
        let mut c = libc::CMSG_FIRSTHDR(&raw const msg);
        while !c.is_null() {
            if (*c).cmsg_level == libc::SOL_SOCKET && (*c).cmsg_type == libc::SCM_RIGHTS {
                let f = libc::CMSG_DATA(c).cast::<i32>().read_unaligned();
                #[cfg(not(any(target_os = "linux", target_os = "android")))]
                libc::fcntl(f, libc::F_SETFD, libc::FD_CLOEXEC);
                *got = f;
                break;
            }
            c = libc::CMSG_NXTHDR(&raw const msg, c);
        }
        n
    }
}
