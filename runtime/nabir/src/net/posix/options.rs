//! Socket options and multicast.

#[allow(clippy::wildcard_imports)]
use crate::net::posix::*;

#[cfg(any(unix, target_env = "p2"))]
pub(crate) fn setopt(fd: i32, level: libc::c_int, name: libc::c_int, v: libc::c_int) -> i32 {
    // SAFETY: valid option pointer.
    let r = unsafe {
        libc::setsockopt(
            fd,
            level,
            name,
            (&raw const v).cast(),
            size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    ret(r as isize) as i32
}

#[cfg(any(unix, target_env = "p2"))]
pub(crate) fn getopt(fd: i32, level: libc::c_int, name: libc::c_int) -> Result<libc::c_int, i32> {
    let mut v: libc::c_int = 0;
    let mut n = size_of::<libc::c_int>() as libc::socklen_t;
    // SAFETY: valid out-pointers.
    let r = unsafe { libc::getsockopt(fd, level, name, (&raw mut v).cast(), &raw mut n) };
    if r < 0 { Err(-errno()) } else { Ok(v) }
}

/// `SO_LINGER` in seconds (plain `SO_LINGER` counts ticks on Apple).
#[cfg(target_vendor = "apple")]
pub(crate) const LINGER: libc::c_int = libc::SO_LINGER_SEC;

#[cfg(all(unix, not(target_vendor = "apple")))]
pub(crate) const LINGER: libc::c_int = libc::SO_LINGER;

#[cfg(unix)]
pub(crate) fn set_linger(fd: i32, secs: libc::c_int) -> i32 {
    let l = libc::linger {
        l_onoff: libc::c_int::from(secs >= 0),
        l_linger: secs.max(0),
    };
    // SAFETY: a valid option.
    let r = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            LINGER,
            (&raw const l).cast(),
            size_of::<libc::linger>() as libc::socklen_t,
        )
    };
    ret(r as isize) as i32
}

#[cfg(any(unix, target_env = "p2"))]
/// Socket options by library code: 1 `TCP_NODELAY`, 2 keepalive (`val`
/// idle seconds, < 0 off), 3 `SO_BROADCAST`, 4 `IP_MULTICAST_LOOP`, 5
/// `IP_MULTICAST_TTL`, 6 `UDP_SEGMENT` (Linux only), 7 `SO_REUSEADDR`, 8
/// `SO_REUSEPORT`, 9 `SO_RCVBUF`, 10 `SO_SNDBUF`, 11 `IP_TTL`, 12
/// `IPV6_V6ONLY`, 13 `SO_LINGER` (`val` seconds, < 0 off).
#[unsafe(no_mangle)]
pub extern "C" fn nabi_net_setopt(fd: i32, opt: i32, val: i64) -> i32 {
    let v = val as libc::c_int;
    match opt {
        1 => setopt(fd, libc::IPPROTO_TCP, libc::TCP_NODELAY, v),
        2 => {
            let r = setopt(fd, libc::SOL_SOCKET, libc::SO_KEEPALIVE, i32::from(v >= 0));
            if r < 0 || v < 0 {
                return r;
            }
            #[cfg(target_vendor = "apple")]
            let idle = libc::TCP_KEEPALIVE;
            #[cfg(not(target_vendor = "apple"))]
            let idle = libc::TCP_KEEPIDLE;
            setopt(fd, libc::IPPROTO_TCP, idle, v.max(1))
        }
        3 => setopt(fd, libc::SOL_SOCKET, libc::SO_BROADCAST, v),
        4 => {
            // SAFETY: a one-byte option on BSD.
            #[cfg(target_vendor = "apple")]
            unsafe {
                let b = u8::from(v != 0);
                ret(libc::setsockopt(
                    fd,
                    libc::IPPROTO_IP,
                    libc::IP_MULTICAST_LOOP,
                    (&raw const b).cast(),
                    1,
                ) as isize) as i32
            }
            #[cfg(not(target_vendor = "apple"))]
            setopt(fd, libc::IPPROTO_IP, libc::IP_MULTICAST_LOOP, v)
        }
        5 => setopt(fd, libc::IPPROTO_IP, libc::IP_MULTICAST_TTL, v),
        #[cfg(any(target_os = "linux", target_os = "android"))]
        6 => setopt(fd, libc::SOL_UDP, libc::UDP_SEGMENT, v),
        7 => setopt(fd, libc::SOL_SOCKET, libc::SO_REUSEADDR, v),
        #[cfg(unix)]
        8 => setopt(fd, libc::SOL_SOCKET, libc::SO_REUSEPORT, v),
        #[cfg(unix)]
        9 => setopt(fd, libc::SOL_SOCKET, libc::SO_RCVBUF, v),
        #[cfg(unix)]
        10 => setopt(fd, libc::SOL_SOCKET, libc::SO_SNDBUF, v),
        #[cfg(unix)]
        11 => setopt(fd, libc::IPPROTO_IP, libc::IP_TTL, v),
        #[cfg(unix)]
        12 => setopt(fd, libc::IPPROTO_IPV6, libc::IPV6_V6ONLY, v),
        #[cfg(unix)]
        13 => set_linger(fd, v),
        _ => -libc::ENOTSUP,
    }
}

#[cfg(any(unix, target_env = "p2"))]
/// The value of option `opt` (as in [`nabi_net_setopt`]) into word 0 of
/// the `GcBuf<i64>` `out`: 0 / 1 for a flag, a size, a TTL, or the linger
/// seconds (-1: off). 0 or `-errno`.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_net_getopt(fd: i32, opt: i32, out: *mut u8) -> i32 {
    let v = match opt {
        1 => getopt(fd, libc::IPPROTO_TCP, libc::TCP_NODELAY),
        3 => getopt(fd, libc::SOL_SOCKET, libc::SO_BROADCAST),
        7 => getopt(fd, libc::SOL_SOCKET, libc::SO_REUSEADDR),
        #[cfg(unix)]
        8 => getopt(fd, libc::SOL_SOCKET, libc::SO_REUSEPORT),
        #[cfg(unix)]
        9 => getopt(fd, libc::SOL_SOCKET, libc::SO_RCVBUF),
        #[cfg(unix)]
        10 => getopt(fd, libc::SOL_SOCKET, libc::SO_SNDBUF),
        #[cfg(unix)]
        11 => getopt(fd, libc::IPPROTO_IP, libc::IP_TTL),
        #[cfg(unix)]
        12 => getopt(fd, libc::IPPROTO_IPV6, libc::IPV6_V6ONLY),
        #[cfg(unix)]
        13 => {
            // SAFETY: zeroed is a valid linger.
            let mut l: libc::linger = unsafe { zeroed() };
            let mut n = size_of::<libc::linger>() as libc::socklen_t;
            // SAFETY: valid out-pointers.
            let r = unsafe {
                libc::getsockopt(
                    fd,
                    libc::SOL_SOCKET,
                    LINGER,
                    (&raw mut l).cast(),
                    &raw mut n,
                )
            };
            if r < 0 {
                Err(-errno())
            } else if l.l_onoff == 0 {
                Ok(-1)
            } else {
                Ok(l.l_linger)
            }
        }
        _ => Err(-libc::ENOTSUP),
    };
    match v {
        Ok(v) => {
            // SAFETY: the library passes a live `GcBuf<i64>` of one word.
            unsafe { *word(out, 0) = i64::from(v) };
            0
        }
        Err(e) => e,
    }
}

#[cfg(any(unix, target_env = "p2"))]
/// Join (`join != 0`) or leave IPv4 multicast `group` on `iface` (both
/// as `a.b.c.d` octets in a big-endian `u32`).
#[unsafe(no_mangle)]
pub extern "C" fn nabi_net_multicast_v4(fd: i32, group: u32, iface: u32, join: i32) -> i32 {
    let m = libc::ip_mreq {
        imr_multiaddr: libc::in_addr {
            s_addr: group.to_be(),
        },
        imr_interface: libc::in_addr {
            s_addr: iface.to_be(),
        },
    };
    let name = if join != 0 {
        libc::IP_ADD_MEMBERSHIP
    } else {
        libc::IP_DROP_MEMBERSHIP
    };
    // SAFETY: valid option pointer.
    let r = unsafe {
        libc::setsockopt(
            fd,
            libc::IPPROTO_IP,
            name,
            (&raw const m).cast(),
            size_of::<libc::ip_mreq>() as libc::socklen_t,
        )
    };
    ret(r as isize) as i32
}
