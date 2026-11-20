//! Sending and receiving, shutdown and the blocking wait.

#[allow(clippy::wildcard_imports)]
use crate::net::posix::*;

/// One `send` of `buf[off..off + len]` (no SIGPIPE): the count or `-errno`.
///
/// # Safety
/// `buf` must be a live `GcBuf<u8>` with at least `off + len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nabi_net_send(fd: i32, buf: *mut u8, off: usize, len: usize) -> isize {
    // SAFETY: caller contract.
    let src = unsafe { region(buf, off) };
    // SAFETY: `src` is readable for `len` bytes.
    no_eintr(|| unsafe { libc::send(fd, src.cast(), len, SEND_FLAGS) })
}

/// One `recv` into `buf[off..off + len]`: the count (0 at the end) or
/// `-errno`. (WASI's preview 2 sockets are no files for `read`.)
///
/// # Safety
/// `buf` must be a live `GcBuf<u8>` with at least `off + len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nabi_net_recv(fd: i32, buf: *mut u8, off: usize, len: usize) -> isize {
    // SAFETY: caller contract.
    let dst = unsafe { region(buf, off) };
    let (n, _) = native(|| {
        // SAFETY: `dst` is writable for `len` bytes.
        no_eintr(|| unsafe { libc::recv(fd, dst.cast(), len, 0) })
    });
    n
}

#[cfg(any(unix, target_env = "p2"))]
/// One datagram `buf[off..off + len]` to `addr`: the count or `-errno`.
///
/// # Safety
/// `buf` must be a live `GcBuf<u8>` with at least `off + len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nabi_net_send_to(
    fd: i32,
    buf: *mut u8,
    off: usize,
    len: usize,
    addr: *mut u8,
) -> isize {
    let (ss, sl) = read_addr(addr);
    // SAFETY: caller contract.
    let src = unsafe { region(buf, off) };
    // SAFETY: valid buffer and sockaddr.
    no_eintr(|| unsafe {
        libc::sendto(fd, src.cast(), len, SEND_FLAGS, (&raw const ss).cast(), sl)
    })
}

#[cfg(any(unix, target_env = "p2"))]
/// One datagram into `buf[off..off + len]` (truncated to fit); the
/// sender goes to `out` (may be null). The count or `-errno`.
///
/// # Safety
/// `buf` must be a live `GcBuf<u8>` with at least `off + len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nabi_net_recv_from(
    fd: i32,
    buf: *mut u8,
    off: usize,
    len: usize,
    out: *mut u8,
) -> isize {
    // SAFETY: caller contract.
    let dst = unsafe { region(buf, off) };
    // SAFETY: zeroed storage is a valid out-buffer.
    let mut ss: libc::sockaddr_storage = unsafe { zeroed() };
    let mut sl = size_of::<libc::sockaddr_storage>() as libc::socklen_t;
    // SAFETY: `dst` is writable for `len` bytes.
    let n = no_eintr(|| unsafe {
        libc::recvfrom(fd, dst.cast(), len, 0, (&raw mut ss).cast(), &raw mut sl)
    });
    if n >= 0 && sl > 0 {
        write_addr(out, (&raw const ss).cast());
    }
    n
}

/// `shutdown`: `how` 0 read, 1 write, 2 both.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_net_shutdown(fd: i32, how: i32) -> i32 {
    // wasi-libc's values; preview 1's are not in the `libc` crate.
    #[cfg(all(target_os = "wasi", not(target_env = "p2")))]
    let h = match how {
        0 => 1,
        1 => 2,
        _ => 3,
    };
    #[cfg(any(unix, target_env = "p2"))]
    let h = match how {
        0 => libc::SHUT_RD,
        1 => libc::SHUT_WR,
        _ => libc::SHUT_RDWR,
    };
    // SAFETY: plain libc call.
    ret(unsafe { libc::shutdown(fd, h) } as isize) as i32
}

/// Block (parked for the GC) until `fd` is readable (`write == 0`) or
/// writable, at most `timeout_ns` (< 0: no limit). 1 ready, 0 timed out,
/// or `-errno`. Hang-ups and errors count as ready (the next call
/// reports them).
#[unsafe(no_mangle)]
pub extern "C" fn nabi_net_wait(fd: i32, write: i32, timeout_ns: i64) -> i32 {
    let mut p = libc::pollfd {
        fd,
        events: if write != 0 {
            libc::POLLOUT
        } else {
            libc::POLLIN
        },
        revents: 0,
    };
    let ms = if timeout_ns < 0 {
        -1
    } else {
        // Round up: a 1 ns timeout still waits one tick.
        (timeout_ns.saturating_add(999_999) / 1_000_000).min(i64::from(i32::MAX)) as libc::c_int
    };
    let mut poll = |ms| {
        loop {
            // SAFETY: one valid pollfd.
            let r = unsafe { libc::poll(&raw mut p, 1, ms) };
            if r >= 0 || errno() != libc::EINTR {
                break r;
            }
        }
    };
    if ms == 0 {
        let r = poll(0);
        return if r < 0 { -errno() } else { i32::from(r > 0) };
    }
    let (r, e) = native(|| poll(ms));
    if r < 0 { -e } else { i32::from(r > 0) }
}
