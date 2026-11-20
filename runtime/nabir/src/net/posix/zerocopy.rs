//! File to socket transfers: sendfile, zero-copy send, splice.

#[allow(clippy::wildcard_imports)]
#[cfg_attr(
    not(any(target_os = "linux", target_os = "android")),
    allow(unused_imports)
)]
use crate::net::posix::*;
#[cfg(target_os = "linux")]
use libc::{MSG_ZEROCOPY, SO_ZEROCOPY};

// Linux's values, which the `libc` crate leaves out for Android.
#[cfg(target_os = "android")]
const SO_ZEROCOPY: libc::c_int = 60;
#[cfg(target_os = "android")]
const MSG_ZEROCOPY: libc::c_int = 0x0400_0000;
/// The kernel's own `ENOTSUPP`, which older kernels return to user space
/// for `SO_ZEROCOPY` on a socket that is not TCP.
#[cfg(any(target_os = "linux", target_os = "android"))]
const ENOTSUPP: libc::c_int = 524;

/// One `sendfile(2)` of `file[offset..offset + count]` to socket `sock`
/// (Linux): the count (0 at the file's end) or `-errno`; `-ENOSYS`
/// elsewhere.
#[unsafe(no_mangle)]
#[cfg_attr(
    not(any(target_os = "linux", target_os = "android")),
    allow(clippy::missing_const_for_fn)
)]
pub extern "C" fn nabi_net_sendfile(sock: i32, file: i32, offset: i64, count: usize) -> isize {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        let mut off = offset as libc::off_t;
        // SAFETY: plain syscall with a valid offset pointer.
        no_eintr(|| unsafe { libc::sendfile(sock, file, &raw mut off, count) })
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        let _ = (sock, file, offset, count);
        -(libc::ENOSYS as isize)
    }
}

/// One `send` of `buf[off..off + len]` with `MSG_ZEROCOPY` (Linux 4.14+):
/// the count or `-errno`; `-ENOSYS` elsewhere. The kernel reads the bytes
/// in place, so the buffer stays pinned until the socket's error queue
/// reports the send complete (it is a list's own storage, which nothing
/// else pins). Where the kernel cannot pin the pages (`ENOBUFS`: the
/// memlock or optmem limit), has no `SO_ZEROCOPY` or refuses it on a
/// connected socket (`EBUSY`: older kernels, Android 10's 4.14), the
/// bytes go out with a plain copying `send`, as the kernel's
/// `msg_zerocopy` notes advise.
///
/// # Safety
/// `buf` must be a live `GcBuf<u8>` with at least `off + len` bytes.
#[unsafe(no_mangle)]
#[cfg_attr(
    not(any(target_os = "linux", target_os = "android")),
    allow(clippy::missing_const_for_fn)
)]
pub unsafe extern "C" fn nabi_net_send_zerocopy(
    fd: i32,
    buf: *mut u8,
    off: usize,
    len: usize,
) -> isize {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        const SO_EE_ORIGIN_ZEROCOPY: u8 = 5;
        let one: libc::c_int = 1;
        // SAFETY: plain syscall on the caller's socket.
        let r = unsafe {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                SO_ZEROCOPY,
                (&raw const one).cast(),
                size_of::<libc::c_int>() as libc::socklen_t,
            )
        };
        // SAFETY: caller contract.
        let src = unsafe { region(buf, off) };
        // SAFETY: `src` is readable for `len` bytes.
        let copy = || no_eintr(|| unsafe { libc::send(fd, src.cast(), len, SEND_FLAGS) });
        if r < 0 {
            let e = errno();
            // EBUSY: an older kernel takes SO_ZEROCOPY only before connect.
            if matches!(
                e,
                libc::ENOPROTOOPT | libc::EOPNOTSUPP | libc::EBUSY | ENOTSUPP
            ) {
                return copy();
            }
            return -(e as isize);
        }
        // SAFETY: a live GC object, unpinned below.
        unsafe { nabir_gc::pin(buf) };
        // SAFETY: `src` is readable for `len` bytes.
        let n = no_eintr(|| unsafe { libc::send(fd, src.cast(), len, SEND_FLAGS | MSG_ZEROCOPY) });
        if n > 0 {
            // Wait for the completion notice (parked: the buffer is pinned).
            native(|| {
                loop {
                    let mut ctrl = [0u64; 16];
                    // SAFETY: a zeroed header pointing at our control buffer.
                    let mut msg: libc::msghdr = unsafe { core::mem::zeroed() };
                    msg.msg_control = ctrl.as_mut_ptr().cast();
                    msg.msg_controllen = size_of_val(&ctrl) as _;
                    // SAFETY: plain syscall with a valid header.
                    let got = unsafe { libc::recvmsg(fd, &raw mut msg, libc::MSG_ERRQUEUE) };
                    if got >= 0 {
                        // SAFETY: walking the control messages the kernel wrote.
                        let done = unsafe {
                            let mut cm = libc::CMSG_FIRSTHDR(&raw const msg);
                            let mut done = false;
                            while !cm.is_null() {
                                let ext = libc::CMSG_DATA(cm)
                                    .cast::<libc::sock_extended_err>()
                                    .read_unaligned();
                                done |= ext.ee_origin == SO_EE_ORIGIN_ZEROCOPY;
                                cm = libc::CMSG_NXTHDR(&raw const msg, cm);
                            }
                            done
                        };
                        if done {
                            break;
                        }
                        continue;
                    }
                    match errno() {
                        libc::EINTR => {}
                        libc::EAGAIN => {
                            let mut pfd = libc::pollfd {
                                fd,
                                events: 0,
                                revents: 0,
                            };
                            // SAFETY: plain syscall; POLLERR is always reported.
                            unsafe { libc::poll(&raw mut pfd, 1, -1) };
                        }
                        _ => break,
                    }
                }
            });
        }
        // SAFETY: pinned above.
        unsafe { nabir_gc::unpin(buf) };
        if n == -(libc::ENOBUFS as isize) {
            return copy();
        }
        n
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        let _ = (fd, buf, off, len);
        -(libc::ENOSYS as isize)
    }
}

/// Up to `count` bytes from `file`'s offset to socket `sock` through a
/// pipe with `splice(2)` (Linux): the count (0 at the file's end) or
/// `-errno`; `-ENOSYS` elsewhere. What reaches the pipe is drained to
/// the socket before returning, waiting for it to take more.
#[unsafe(no_mangle)]
#[cfg_attr(
    not(any(target_os = "linux", target_os = "android")),
    allow(clippy::missing_const_for_fn)
)]
pub extern "C" fn nabi_net_splice(sock: i32, file: i32, count: usize) -> isize {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        let mut p = [0i32; 2];
        // SAFETY: `p` holds two fds.
        if unsafe { libc::pipe2(p.as_mut_ptr(), libc::O_CLOEXEC) } < 0 {
            return -(errno() as isize);
        }
        let (n, e) = native(|| {
            // SAFETY: plain syscalls on fds the caller and we own.
            unsafe {
                let null = ptr::null_mut::<libc::loff_t>();
                let n = loop {
                    let n = libc::splice(file, null, p[1], null, count, libc::SPLICE_F_MOVE);
                    if n >= 0 || errno() != libc::EINTR {
                        break n;
                    }
                };
                let mut left = n.max(0) as usize;
                while left > 0 {
                    let k = libc::splice(p[0], null, sock, null, left, libc::SPLICE_F_MOVE);
                    if k > 0 {
                        left -= k as usize;
                        continue;
                    }
                    match errno() {
                        libc::EINTR => {}
                        libc::EAGAIN => {
                            let mut pfd = libc::pollfd {
                                fd: sock,
                                events: libc::POLLOUT,
                                revents: 0,
                            };
                            libc::poll(&raw mut pfd, 1, -1);
                        }
                        _ => return -1,
                    }
                }
                n
            }
        });
        // SAFETY: our pipe.
        unsafe {
            libc::close(p[0]);
            libc::close(p[1]);
        }
        if n < 0 { -(e as isize) } else { n }
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        let _ = (sock, file, count);
        -(libc::ENOSYS as isize)
    }
}
