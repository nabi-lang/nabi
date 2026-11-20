//! The I/O driver's poller: kqueue / epoll, WASI watch sets.

#[allow(clippy::wildcard_imports)]
use crate::net::posix::*;

#[cfg(not(target_os = "wasi"))]
pub(crate) static POLLER: AtomicI32 = AtomicI32::new(-1);

/// The process's kqueue / epoll fd, created on first use.
#[cfg(not(target_os = "wasi"))]
#[unsafe(no_mangle)]
pub extern "C" fn nabi_io_poller() -> i32 {
    let p = POLLER.load(Ordering::Acquire);
    if p >= 0 {
        return p;
    }
    // SAFETY: plain libc calls.
    #[cfg(any(target_vendor = "apple", target_os = "freebsd"))]
    let fd = unsafe {
        let fd = libc::kqueue();
        if fd >= 0 {
            libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
        }
        fd
    };
    // SAFETY: plain libc call.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    let fd = unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) };
    if fd < 0 {
        nabir_common::fatal(b"nabi: cannot create the I/O poller");
    }
    match POLLER.compare_exchange(-1, fd, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => fd,
        Err(cur) => {
            // SAFETY: our own extra fd.
            unsafe { libc::close(fd) };
            cur
        }
    }
}

/// After a failed REPL cell (`session`): a new poller on next use (a
/// forked copy has no kqueue; an epoll set would be shared with the
/// process that died). Waiters watch their fds again when woken.
#[cfg(not(target_os = "wasi"))]
pub fn reset_poller() {
    let p = POLLER.swap(-1, Ordering::AcqRel);
    if p >= 0 {
        // SAFETY: our own fd, no longer published.
        unsafe { libc::close(p) };
    }
}

#[cfg(not(target_os = "wasi"))]
/// Watch `fd` for readability and writability, edge-triggered (an
/// event per change, so a waiter must re-check after it parks). Watching
/// a watched fd again is harmless. `interest` (1 read, 2 write): what the
/// caller waits for; kqueue / epoll watch both. 0 or `-errno`.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_io_watch(fd: i32, _interest: u32) -> i32 {
    let p = nabi_io_poller();
    #[cfg(any(target_vendor = "apple", target_os = "freebsd"))]
    {
        // SAFETY: zeroed kevents are valid; filled below.
        let mut ch: [libc::kevent; 2] = unsafe { zeroed() };
        for (k, filter) in ch.iter_mut().zip([libc::EVFILT_READ, libc::EVFILT_WRITE]) {
            k.ident = fd as libc::uintptr_t;
            k.filter = filter;
            k.flags = libc::EV_ADD | libc::EV_CLEAR;
        }
        // SAFETY: two valid changes, no event buffer.
        let r = unsafe {
            libc::kevent(
                p,
                ch.as_ptr(),
                2,
                core::ptr::null_mut(),
                0,
                core::ptr::null(),
            )
        };
        ret(r as isize) as i32
    }
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        let mut ev = libc::epoll_event {
            events: (libc::EPOLLIN | libc::EPOLLOUT | libc::EPOLLRDHUP | libc::EPOLLET) as u32,
            u64: fd as u64,
        };
        // SAFETY: a valid event.
        let r = unsafe { libc::epoll_ctl(p, libc::EPOLL_CTL_ADD, fd, &raw mut ev) };
        if r < 0 && errno() == libc::EEXIST {
            return 0;
        }
        ret(r as isize) as i32
    }
}

#[cfg(not(target_os = "wasi"))]
/// Block (parked for the GC) until watched fds change; up to `max`
/// events go to the `GcBuf<i64>` `out` as `fd * 2 + (0 read | 1 write)`.
/// The count, or `-errno`.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_io_poll(out: *mut u8, max: usize) -> i32 {
    const N: usize = 64;
    let p = nabi_io_poller();
    let mut keys = [0i64; 2 * N];
    let (n, e) = native(|| {
        let mut n = 0usize;
        #[cfg(any(target_vendor = "apple", target_os = "freebsd"))]
        {
            // SAFETY: zeroed kevents are a valid out-buffer.
            let mut evs: [libc::kevent; N] = unsafe { zeroed() };
            // SAFETY: valid buffer; no timeout.
            let r = unsafe {
                libc::kevent(
                    p,
                    core::ptr::null(),
                    0,
                    evs.as_mut_ptr(),
                    N as libc::c_int,
                    core::ptr::null(),
                )
            };
            if r < 0 {
                return -1;
            }
            for ev in &evs[..r as usize] {
                let dir = i64::from(ev.filter == libc::EVFILT_WRITE);
                keys[n] = ev.ident as i64 * 2 + dir;
                n += 1;
            }
        }
        #[cfg(any(target_os = "linux", target_os = "android"))]
        {
            // SAFETY: zeroed events are a valid out-buffer.
            let mut evs: [libc::epoll_event; N] = unsafe { zeroed() };
            // SAFETY: valid buffer.
            let r = unsafe { libc::epoll_wait(p, evs.as_mut_ptr(), N as libc::c_int, -1) };
            if r < 0 {
                return -1;
            }
            for ev in &evs[..r as usize] {
                let fd = ev.u64 as i64;
                let bits = ev.events;
                let err = (libc::EPOLLERR | libc::EPOLLHUP) as u32;
                if bits & (libc::EPOLLIN as u32 | libc::EPOLLRDHUP as u32 | err) != 0 {
                    keys[n] = fd * 2;
                    n += 1;
                }
                if bits & (libc::EPOLLOUT as u32 | err) != 0 {
                    keys[n] = fd * 2 + 1;
                    n += 1;
                }
            }
        }
        n as isize
    });
    if n < 0 {
        return if e == libc::EINTR { 0 } else { -e };
    }
    let n = (n as usize).min(max);
    for (i, k) in keys[..n].iter().enumerate() {
        // SAFETY: the library passes `max` words.
        unsafe { *word(out, i) = *k };
    }
    n as i32
}

/// WASI: no kqueue / epoll. The watched fds and the directions still
/// armed (bit 0 read, 1 write); `poll` (`poll_oneoff`) over them reports
/// each armed direction once, like `EPOLLET`, until `nabi_io_watch`
/// re-arms it.
#[cfg(target_os = "wasi")]
mod watch {
    use core::cell::UnsafeCell;
    use nabir_common::sys::Lock;

    /// Highest watchable fd + 1.
    pub const MAX: usize = 1024;

    pub struct Set {
        pub lock: Lock,
        pub armed: UnsafeCell<[u8; MAX]>,
    }

    // SAFETY: `armed` only under `lock`.
    unsafe impl Sync for Set {}

    pub static SET: Set = Set {
        lock: Lock::new(),
        armed: UnsafeCell::new([0; MAX]),
    };
}

/// No poller fd on WASI: the watch set is the runtime's.
#[cfg(target_os = "wasi")]
#[unsafe(no_mangle)]
pub const extern "C" fn nabi_io_poller() -> i32 {
    0
}

/// Arm `fd` for the directions of `interest` (1 read, 2 write; only
/// those: wasi-libc rejects a `poll` for writing on a listening socket).
/// 0 or `-errno` (`-EMFILE` past the set).
#[cfg(target_os = "wasi")]
#[unsafe(no_mangle)]
pub extern "C" fn nabi_io_watch(fd: i32, interest: u32) -> i32 {
    let Ok(i) = usize::try_from(fd) else {
        return -libc::EBADF;
    };
    if i >= watch::MAX {
        return -libc::EMFILE;
    }
    watch::SET.lock.lock();
    // SAFETY: under the lock.
    unsafe { (*watch::SET.armed.get())[i] |= interest as u8 & 3 };
    watch::SET.lock.unlock();
    0
}

/// One `poll` of the armed fds, at most `ms` (< 0: no limit): their
/// events (disarmed) into `out` as in [`nabi_io_poll`]. `None`: nothing
/// is armed.
#[cfg(target_os = "wasi")]
pub(crate) fn poll_armed(out: *mut u8, max: usize, ms: i32) -> Option<i32> {
    const N: usize = 64;
    let mut fds = [libc::pollfd {
        fd: -1,
        events: 0,
        revents: 0,
    }; N];
    let mut k = 0;
    watch::SET.lock.lock();
    // SAFETY: under the lock.
    let armed = unsafe { &*watch::SET.armed.get() };
    for (fd, a) in armed.iter().enumerate() {
        if *a != 0 && k < N {
            let mut ev = 0;
            if a & 1 != 0 {
                ev |= libc::POLLIN;
            }
            if a & 2 != 0 {
                ev |= libc::POLLOUT;
            }
            fds[k] = libc::pollfd {
                fd: fd as i32,
                events: ev,
                revents: 0,
            };
            k += 1;
        }
    }
    watch::SET.lock.unlock();
    if k == 0 {
        return None;
    }
    // SAFETY: `k` valid pollfds.
    let (r, e) = native(|| unsafe { libc::poll(fds.as_mut_ptr(), k as libc::nfds_t, ms) });
    if r < 0 && e == libc::EINTR {
        return Some(0);
    }
    let mut n = 0;
    watch::SET.lock.lock();
    for p in &fds[..k] {
        // A failed `poll` wakes every waiter: its retried call reports
        // what is wrong with its socket.
        let hup = r < 0 || p.revents & (libc::POLLERR | libc::POLLHUP) != 0;
        for (dir, bit) in [(0, libc::POLLIN), (1, libc::POLLOUT)] {
            if n < max && p.events & bit != 0 && (p.revents & bit != 0 || hup) {
                // SAFETY: under the lock; the library passes `max` words.
                unsafe {
                    (*watch::SET.armed.get())[p.fd as usize] &= !(1 << dir);
                    *word(out, n) = i64::from(p.fd) * 2 + dir;
                }
                n += 1;
            }
        }
    }
    watch::SET.lock.unlock();
    Some(n as i32)
}

/// [`nabi_io_poll`] for the driver thread (`wasm32-wasip1-threads`): a
/// watch armed meanwhile is seen within a 10 ms slice.
#[cfg(target_os = "wasi")]
#[unsafe(no_mangle)]
pub extern "C" fn nabi_io_poll(out: *mut u8, max: usize) -> i32 {
    loop {
        match poll_armed(out, max, 10) {
            Some(0) => {}
            Some(n) => return n,
            None => nabir_common::sys::sleep_ns(10_000_000),
        }
    }
}

/// Whether a watch is armed: what the one-thread executor could wait for.
#[cfg(target_os = "wasi")]
#[unsafe(no_mangle)]
pub extern "C" fn nabi_io_armed() -> i32 {
    watch::SET.lock.lock();
    // SAFETY: under the lock.
    let any = unsafe { &*watch::SET.armed.get() }.iter().any(|a| *a != 0);
    watch::SET.lock.unlock();
    i32::from(any)
}

/// The one-thread executor's wait: the watched fds' events (as
/// [`nabi_io_poll`]) or `timeout_ns` (< 0: no limit), whichever comes
/// first; the count (0: timed out). Nothing watched and no limit is a
/// deadlock: nothing could ever wake the thread.
#[cfg(target_os = "wasi")]
#[unsafe(no_mangle)]
pub extern "C" fn nabi_io_wait(out: *mut u8, max: usize, timeout_ns: i64) -> i32 {
    let ms = if timeout_ns < 0 {
        -1
    } else {
        (timeout_ns.saturating_add(999_999) / 1_000_000).min(i64::from(i32::MAX)) as i32
    };
    match poll_armed(out, max, ms) {
        Some(n) => n,
        None if timeout_ns < 0 => {
            nabir_common::fatal(b"fatal error: deadlock: the only thread waits for another")
        }
        None => {
            nabir_common::sys::sleep_ns(timeout_ns as u64);
            0
        }
    }
}
