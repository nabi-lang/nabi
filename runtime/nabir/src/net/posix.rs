//! Sockets behind `std::net` and the readiness poller
//! of the I/O driver: kqueue on BSD / macOS, epoll on Linux, `poll`
//! (`poll_oneoff` / `wasi:io/poll`) on WASI. No ABI
//! stability. Conventions as in `os`: results are `>= 0` or
//! `-errno`; byte data is a `GcBuf<u8>` region.
//!
//! WASI preview 2 has the BSD API through wasi-libc. Preview 1
//! (`wasm32-wasip1-threads`, the browser) only uses sockets the host hands
//! in ([`socket::nabi_net_listeners`]); the rest fails with `ENOSYS`.
//!
//! Sockets are non-blocking; blocking calls wait in [`transfer::nabi_net_wait`].
//!
//! Addresses cross as [`ADDR_WORDS`] words of a `GcBuf<i64>`:
//! `[family (4 / 6), port, flow_info, scope_id, byte0 .. byte15]`.

#[cfg(any(unix, target_env = "p2"))]
mod options;
mod poll;
mod socket;
mod transfer;
#[cfg(unix)]
mod unix;
mod zerocopy;

#[cfg(not(target_os = "wasi"))]
pub use crate::net::posix::poll::reset_poller;

#[cfg(any(unix, target_env = "p2"))]
use crate::os::{EAI_BASE, cs};
use crate::os::{errno, native, region, ret};
#[cfg(any(unix, target_env = "p2"))]
use crate::str::NabiStr;
#[cfg(any(unix, target_env = "p2"))]
use core::mem::{size_of, zeroed};
#[cfg(any(target_os = "linux", target_os = "android"))]
use core::ptr;
#[cfg(not(target_os = "wasi"))]
use core::sync::atomic::{AtomicI32, Ordering};
use nabir_common::header::HEADER_SIZE;

const ADDR_WORDS: usize = 20;

/// Word `i` of the `GcBuf<i64>` `buf`.
///
/// # Safety
/// `buf` must be a live `GcBuf<i64>` with more than `i` elements.
#[allow(clippy::cast_ptr_alignment)] // GC payloads are 8-aligned
const unsafe fn word(buf: *mut u8, i: usize) -> *mut i64 {
    // SAFETY: caller contract.
    unsafe { buf.add(HEADER_SIZE).cast::<i64>().add(i) }
}

#[cfg(any(unix, target_env = "p2"))]
/// The address in `buf` as a `sockaddr_storage` and its length.
fn read_addr(buf: *mut u8) -> (libc::sockaddr_storage, libc::socklen_t) {
    // SAFETY: the library passes a live `ADDR_WORDS` buffer; the storage
    // is large enough for either family.
    unsafe {
        let w = |i| *word(buf, i);
        let mut ss: libc::sockaddr_storage = zeroed();
        let port = (w(1) as u16).to_be();
        if w(0) == 6 {
            let a = (&raw mut ss).cast::<libc::sockaddr_in6>();
            (*a).sin6_family = libc::AF_INET6 as libc::sa_family_t;
            (*a).sin6_port = port;
            (*a).sin6_flowinfo = w(2) as u32;
            (*a).sin6_scope_id = w(3) as u32;
            for i in 0..16 {
                (*a).sin6_addr.s6_addr[i] = w(4 + i) as u8;
            }
            set_len(&raw mut ss, size_of::<libc::sockaddr_in6>());
            (ss, size_of::<libc::sockaddr_in6>() as libc::socklen_t)
        } else {
            let a = (&raw mut ss).cast::<libc::sockaddr_in>();
            (*a).sin_family = libc::AF_INET as libc::sa_family_t;
            (*a).sin_port = port;
            let b = [w(4) as u8, w(5) as u8, w(6) as u8, w(7) as u8];
            (*a).sin_addr.s_addr = u32::from_ne_bytes(b);
            set_len(&raw mut ss, size_of::<libc::sockaddr_in>());
            (ss, size_of::<libc::sockaddr_in>() as libc::socklen_t)
        }
    }
}

#[cfg(any(unix, target_env = "p2"))]
/// BSD sockaddrs carry their length.
#[allow(clippy::missing_const_for_fn)]
unsafe fn set_len(ss: *mut libc::sockaddr_storage, n: usize) {
    #[cfg(any(target_vendor = "apple", target_os = "freebsd"))]
    // SAFETY: caller passes a valid storage.
    unsafe {
        (*ss).ss_len = n as u8;
    }
    let _ = (ss, n);
}

#[cfg(any(unix, target_env = "p2"))]
/// Store the address at `sa` into `buf` (no-op for a null `buf`).
#[allow(clippy::cast_ptr_alignment)] // a sockaddr of its own family
fn write_addr(buf: *mut u8, sa: *const libc::sockaddr) {
    if buf.is_null() {
        return;
    }
    // SAFETY: `sa` points at a sockaddr of its own family; `buf` is a
    // live `ADDR_WORDS` buffer.
    unsafe {
        let put = |i, v: i64| *word(buf, i) = v;
        for i in 0..ADDR_WORDS {
            put(i, 0);
        }
        if i32::from((*sa).sa_family) == libc::AF_INET6 {
            let a = sa.cast::<libc::sockaddr_in6>();
            put(0, 6);
            put(1, i64::from(u16::from_be((*a).sin6_port)));
            put(2, i64::from((*a).sin6_flowinfo));
            put(3, i64::from((*a).sin6_scope_id));
            for i in 0..16 {
                put(4 + i, i64::from((*a).sin6_addr.s6_addr[i]));
            }
        } else {
            let a = sa.cast::<libc::sockaddr_in>();
            put(0, 4);
            put(1, i64::from(u16::from_be((*a).sin_port)));
            for (i, b) in (*a).sin_addr.s_addr.to_ne_bytes().iter().enumerate() {
                put(4 + i, i64::from(*b));
            }
        }
    }
}

/// Non-blocking + close-on-exec.
fn prepare(fd: i32) -> i32 {
    // SAFETY: plain fcntl calls on an fd we own.
    unsafe {
        let fl = libc::fcntl(fd, libc::F_GETFL);
        if fl < 0 || libc::fcntl(fd, libc::F_SETFL, fl | libc::O_NONBLOCK) < 0 {
            return -errno();
        }
        #[cfg(unix)]
        if libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) < 0 {
            return -errno();
        }
        #[cfg(target_vendor = "apple")]
        {
            let on: libc::c_int = 1;
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_NOSIGPIPE,
                (&raw const on).cast(),
                size_of::<libc::c_int>() as libc::socklen_t,
            );
        }
    }
    0
}

/// `send` flags: no SIGPIPE where the flag exists.
#[cfg(not(any(target_vendor = "apple", target_os = "wasi")))]
const SEND_FLAGS: libc::c_int = libc::MSG_NOSIGNAL;
#[cfg(any(target_vendor = "apple", target_os = "wasi"))]
const SEND_FLAGS: libc::c_int = 0;

/// Retry `f` (a libc `-1`-on-error call) across `EINTR`.
fn no_eintr(mut f: impl FnMut() -> isize) -> isize {
    loop {
        let n = ret(f());
        if n != -(libc::EINTR as isize) {
            return n;
        }
    }
}

#[cfg(any(unix, target_env = "p2"))]
/// Resolve `host` (a name or numeric address) with `port`: up to `max`
/// addresses into `out` (`max * ADDR_WORDS` words), stream sockets only.
/// The count or `-code` (an errno, or a resolver code from `EAI_BASE`).
#[unsafe(no_mangle)]
pub extern "C" fn nabi_net_resolve(host: NabiStr, port: u16, out: *mut u8, max: usize) -> i32 {
    // SAFETY: zeroed hints are valid; `cs` NUL-terminates `host`.
    let mut hints: libc::addrinfo = unsafe { zeroed() };
    hints.ai_socktype = libc::SOCK_STREAM;
    let mut res: *mut libc::addrinfo = core::ptr::null_mut();
    let (r, e) = native(|| unsafe {
        libc::getaddrinfo(
            cs(host).p(),
            core::ptr::null(),
            &raw const hints,
            &raw mut res,
        )
    });
    if r != 0 {
        if r == libc::EAI_SYSTEM {
            return -e;
        }
        return -(EAI_BASE + r.abs());
    }
    let mut n = 0;
    let mut p = res;
    while !p.is_null() && n < max {
        // SAFETY: a list from getaddrinfo; `out` has room for `max`.
        unsafe {
            let fam = (*p).ai_family;
            if fam == libc::AF_INET || fam == libc::AF_INET6 {
                let slot = out.add(n * ADDR_WORDS * 8);
                write_addr(slot, (*p).ai_addr);
                *word(slot, 1) = i64::from(port);
                n += 1;
            }
            p = (*p).ai_next;
        }
    }
    // SAFETY: from getaddrinfo.
    unsafe { libc::freeaddrinfo(res) };
    n as i32
}
