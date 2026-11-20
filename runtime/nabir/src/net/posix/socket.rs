//! Socket lifecycle: create, bind, listen, connect, accept, inherited listeners.

#[allow(clippy::wildcard_imports)]
use crate::net::posix::*;

#[cfg(any(unix, target_env = "p2"))]
/// A new non-blocking socket: `family` 4 / 6, `kind` 1 stream / 2
/// datagram / 3 raw, `proto` an IP protocol number (0: the default for
/// `kind`). The fd or `-errno`.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_net_socket(family: i32, kind: i32, proto: i32) -> i32 {
    let af = if family == 6 {
        libc::AF_INET6
    } else {
        libc::AF_INET
    };
    let ty = match kind {
        2 => libc::SOCK_DGRAM,
        #[cfg(unix)]
        3 => libc::SOCK_RAW,
        _ => libc::SOCK_STREAM,
    };
    // wasi-libc's preview 2 sockets take no `fcntl`: non-blocking from
    // the start.
    #[cfg(target_env = "p2")]
    let ty = ty | libc::SOCK_NONBLOCK;
    // SAFETY: plain libc call.
    let fd = unsafe { libc::socket(af, ty, proto) };
    if fd < 0 {
        return -errno();
    }
    #[cfg(unix)]
    {
        let r = prepare(fd);
        if r < 0 {
            // SAFETY: our fd.
            unsafe { libc::close(fd) };
            return r;
        }
    }
    fd
}

#[cfg(any(unix, target_env = "p2"))]
/// `bind`, nothing else (a listener sets `SO_REUSEADDR` first itself).
#[unsafe(no_mangle)]
pub extern "C" fn nabi_net_bind(fd: i32, addr: *mut u8) -> i32 {
    let (ss, len) = read_addr(addr);
    // SAFETY: a valid sockaddr.
    ret(unsafe { libc::bind(fd, (&raw const ss).cast(), len) } as isize) as i32
}

#[cfg(any(unix, target_env = "p2"))]
#[unsafe(no_mangle)]
pub extern "C" fn nabi_net_listen(fd: i32, backlog: i32) -> i32 {
    // SAFETY: plain libc call.
    ret(unsafe { libc::listen(fd, backlog) } as isize) as i32
}

#[cfg(any(unix, target_env = "p2"))]
/// Start a connect: 0 when connected, 1 while pending (wait for
/// writability, then [`nabi_net_sock_error`]), or `-errno`.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_net_connect(fd: i32, addr: *mut u8) -> i32 {
    let (ss, len) = read_addr(addr);
    loop {
        // SAFETY: valid sockaddr.
        let r = unsafe { libc::connect(fd, (&raw const ss).cast(), len) };
        if r == 0 {
            return 0;
        }
        match errno() {
            libc::EINTR => {}
            libc::EINPROGRESS => return 1,
            e => return -e,
        }
    }
}

#[cfg(any(unix, target_env = "p2"))]
/// The pending error of `fd` (`SO_ERROR`): 0 or `-errno`.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_net_sock_error(fd: i32) -> i32 {
    let mut e: libc::c_int = 0;
    let mut n = size_of::<libc::c_int>() as libc::socklen_t;
    // SAFETY: valid out-pointers.
    let r = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_ERROR,
            (&raw mut e).cast(),
            &raw mut n,
        )
    };
    if r < 0 { -errno() } else { -e }
}

#[cfg(any(unix, target_env = "p2"))]
/// Accept one connection (non-blocking: `-EAGAIN` if none is queued);
/// its peer goes to `out`.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_net_accept(fd: i32, out: *mut u8) -> i32 {
    // SAFETY: zeroed storage is a valid out-buffer.
    let mut ss: libc::sockaddr_storage = unsafe { zeroed() };
    let mut len = size_of::<libc::sockaddr_storage>() as libc::socklen_t;
    let c = loop {
        // SAFETY: valid out-pointers.
        #[cfg(unix)]
        let c = unsafe { libc::accept(fd, (&raw mut ss).cast(), &raw mut len) };
        // SAFETY: as above; no `fcntl` on preview 2 sockets.
        #[cfg(target_env = "p2")]
        let c =
            unsafe { libc::accept4(fd, (&raw mut ss).cast(), &raw mut len, libc::SOCK_NONBLOCK) };
        if c >= 0 || errno() != libc::EINTR {
            break c;
        }
    };
    if c < 0 {
        return -errno();
    }
    #[cfg(target_env = "p2")]
    let r = 0;
    #[cfg(unix)]
    let r = prepare(c);
    if r < 0 {
        // SAFETY: our fd.
        unsafe { libc::close(c) };
        return r;
    }
    write_addr(out, (&raw const ss).cast());
    c
}

#[cfg(any(unix, target_env = "p2"))]
/// The local (`peer == 0`) or peer address of `fd` into `out`.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_net_addr(fd: i32, out: *mut u8, peer: i32) -> i32 {
    // SAFETY: zeroed storage is a valid out-buffer.
    let mut ss: libc::sockaddr_storage = unsafe { zeroed() };
    let mut len = size_of::<libc::sockaddr_storage>() as libc::socklen_t;
    // SAFETY: valid out-pointers.
    let r = unsafe {
        if peer == 0 {
            libc::getsockname(fd, (&raw mut ss).cast(), &raw mut len)
        } else {
            libc::getpeername(fd, (&raw mut ss).cast(), &raw mut len)
        }
    };
    if r < 0 {
        return -errno();
    }
    write_addr(out, (&raw const ss).cast());
    0
}

/// The calls preview 1 has no socket API for.
#[cfg(all(target_os = "wasi", not(target_env = "p2")))]
mod p1 {
    use crate::str::NabiStr;

    const UNSUPPORTED: i32 = -libc::ENOSYS;

    macro_rules! unsupported {
        ($($name:ident($($t:ty),*) -> $r:ty;)*) => {$(
            #[unsafe(no_mangle)]
            pub const extern "C" fn $name($(_: $t),*) -> $r {
                UNSUPPORTED as $r
            }
        )*};
    }

    unsupported! {
        nabi_net_socket(i32, i32, i32) -> i32;
        nabi_net_bind(i32, *mut u8) -> i32;
        nabi_net_listen(i32, i32) -> i32;
        nabi_net_connect(i32, *mut u8) -> i32;
        nabi_net_sock_error(i32) -> i32;
        nabi_net_addr(i32, *mut u8, i32) -> i32;
        nabi_net_send_to(i32, *mut u8, usize, usize, *mut u8) -> isize;
        nabi_net_recv_from(i32, *mut u8, usize, usize, *mut u8) -> isize;
        nabi_net_setopt(i32, i32, i64) -> i32;
        nabi_net_getopt(i32, i32, *mut u8) -> i32;
        nabi_net_multicast_v4(i32, u32, u32, i32) -> i32;
        nabi_net_resolve(NabiStr, u16, *mut u8, usize) -> i32;
    }
}

/// Accept one connection (non-blocking: `-EAGAIN` if none is queued).
/// Preview 1 cannot tell the peer: `out` gets `0.0.0.0:0`.
#[cfg(all(target_os = "wasi", not(target_env = "p2")))]
#[unsafe(no_mangle)]
pub extern "C" fn nabi_net_accept(fd: i32, out: *mut u8) -> i32 {
    // Not in the `libc` crate for preview 1; wasi-libc has it.
    unsafe extern "C" {
        fn accept(fd: i32, addr: *mut u8, len: *mut u32) -> i32;
    }
    // SAFETY: the peer is not asked for.
    let c = unsafe { accept(fd, core::ptr::null_mut(), core::ptr::null_mut()) };
    if c < 0 {
        return -errno();
    }
    let r = prepare(c);
    if r < 0 {
        // SAFETY: our fd.
        unsafe { libc::close(c) };
        return r;
    }
    if !out.is_null() {
        for i in 0..ADDR_WORDS {
            // SAFETY: a live `ADDR_WORDS` buffer.
            unsafe { *word(out, i) = 0 };
        }
        // SAFETY: as above.
        unsafe { *word(out, 0) = 4 };
    }
    c
}

/// Listening sockets the host handed in, into the `GcBuf<i32>` `out`
/// (up to `max`): WASI's pre-opened sockets (wasmtime `-S tcplisten=`),
/// systemd's socket activation (`LISTEN_FDS` for this `LISTEN_PID`,
/// from fd 3) on Unix. Each is made non-blocking. The count.
#[unsafe(no_mangle)]
#[allow(clippy::cast_ptr_alignment)] // GC payloads are 8-aligned
pub extern "C" fn nabi_net_listeners(out: *mut u8, max: usize) -> i32 {
    let mut n = 0;
    let mut fd = 3;
    while n < max {
        if !inherited(fd) {
            break;
        }
        if prepare(fd) == 0 {
            // SAFETY: the library passes a live `GcBuf<i32>` of `max`.
            unsafe { *out.add(HEADER_SIZE).cast::<i32>().add(n) = fd };
            n += 1;
        }
        fd += 1;
    }
    n as i32
}

/// WASI: pre-opens run from fd 3 without gaps; the sockets among them.
#[cfg(target_os = "wasi")]
pub(crate) fn inherited(fd: i32) -> bool {
    #[link(wasm_import_module = "wasi_snapshot_preview1")]
    unsafe extern "C" {
        fn fd_fdstat_get(fd: i32, out: *mut [u64; 3]) -> u16;
    }
    const SOCKET_STREAM: u8 = 6;
    let mut st = [0u64; 3];
    // SAFETY: a 24-byte, 8-aligned `fdstat`.
    let e = unsafe { fd_fdstat_get(fd, &raw mut st) };
    e == 0 && st[0] as u8 == SOCKET_STREAM
}

/// Unix: systemd's `LISTEN_PID` / `LISTEN_FDS`.
#[cfg(unix)]
pub(crate) fn inherited(fd: i32) -> bool {
    let num = |name: &core::ffi::CStr| -> i64 {
        // SAFETY: getenv's result is null or NUL-terminated.
        let p = unsafe { libc::getenv(name.as_ptr()) };
        if p.is_null() {
            return -1;
        }
        // SAFETY: as above.
        let s = unsafe { core::ffi::CStr::from_ptr(p) }.to_bytes();
        core::str::from_utf8(s)
            .ok()
            .and_then(|t| t.parse().ok())
            .unwrap_or(-1)
    };
    // SAFETY: plain libc call.
    let me = i64::from(unsafe { libc::getpid() });
    num(c"LISTEN_PID") == me && i64::from(fd) < 3 + num(c"LISTEN_FDS")
}
