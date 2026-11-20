//! The dev channel: lines with `nabi dev` over a unix stream socket or
//! a named pipe (`NABI_DEV`), which `nabi dev` serves.
//!
//! The program connects once, says `hello 1 <pid>`, then a thread of its
//! own reads `load <path>` lines and retries waiting changes every
//! 200 ms. Replies go out from whichever thread applied the change.

use core::sync::atomic::{AtomicBool, Ordering};
use nabir_common::sys::Lock;
use nabir_common::wtf8::MBuf;
use nabir_gc::world;

static CONNECTED: AtomicBool = AtomicBool::new(false);
/// Serializes writers (replies come from several threads).
static WRITE: Lock = Lock::new();

/// How often a waiting change is tried again.
const RETRY_MS: i32 = 200;

pub(crate) fn connected() -> bool {
    CONNECTED.load(Ordering::Acquire)
}

/// Connect to `endpoint` and start the reader thread; why not, if not.
pub(crate) fn connect(endpoint: &[u8]) -> Result<(), MBuf<u8>> {
    if connected() {
        return Ok(());
    }
    os::open(endpoint)?;
    CONNECTED.store(true, Ordering::Release);
    let mut hello: MBuf<u8> = MBuf::new();
    hello.extend(b"hello 1 ");
    push_num(&mut hello, u64::from(os::pid()));
    send(hello.as_slice());
    if !nabir_common::sys::spawn_detached(reader) {
        CONNECTED.store(false, Ordering::Release);
        let mut m = MBuf::new();
        m.extend(b"cannot start the channel thread");
        return Err(m);
    }
    Ok(())
}

/// Send one line (the `\n` is added).
pub(crate) fn send(line: &[u8]) {
    if !connected() {
        return;
    }
    let mut buf: MBuf<u8> = MBuf::new();
    buf.extend(line);
    buf.push(b'\n');
    // Senders are mutators: blocked here they count as stopped.
    world::enter_native();
    WRITE.lock();
    if !os::write(buf.as_slice()) {
        CONNECTED.store(false, Ordering::Release);
    }
    WRITE.unlock();
    world::leave_native();
}

pub(crate) fn push_num(out: &mut MBuf<u8>, mut n: u64) {
    let mut d = [0u8; 20];
    let mut i = d.len();
    loop {
        i -= 1;
        d[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    out.extend(&d[i..]);
}

/// The channel thread: a mutator that blocks in reads, outside the
/// world, and hands each line to the swap.
extern "C" fn reader() {
    nabir_common::tls::thread_init();
    world::attach_current();
    crate::stack::thread_start();
    let mut pending: MBuf<u8> = MBuf::new();
    let mut chunk = [0u8; 512];
    loop {
        world::enter_native();
        let n = os::read(&mut chunk, RETRY_MS);
        world::leave_native();
        let Some(n) = n else {
            // The other end is gone: `nabi dev` ended.
            CONNECTED.store(false, Ordering::Release);
            world::detach();
            return;
        };
        pending.extend(&chunk[..n]);
        while let Some(at) = pending.as_slice().iter().position(|&c| c == b'\n') {
            let mut line: MBuf<u8> = MBuf::new();
            line.extend(&pending.as_slice()[..at]);
            let rest: MBuf<u8> = {
                let mut r = MBuf::new();
                r.extend(&pending.as_slice()[at + 1..]);
                r
            };
            pending = rest;
            let l = line.as_slice();
            let l = l.strip_suffix(b"\r").unwrap_or(l);
            if !l.is_empty() {
                crate::hot::swap::request(l);
            }
        }
        crate::hot::swap::tick();
    }
}

#[cfg(unix)]
mod os {
    use core::sync::atomic::{AtomicI32, Ordering};
    use nabir_common::wtf8::MBuf;

    static FD: AtomicI32 = AtomicI32::new(-1);

    fn os_error(what: &[u8]) -> MBuf<u8> {
        let mut m = MBuf::new();
        m.extend(what);
        // SAFETY: `strerror` gives a NUL-terminated message.
        let s = unsafe {
            let p = libc::strerror(nabir_common::sys::errno());
            if p.is_null() {
                &b""[..]
            } else {
                core::ffi::CStr::from_ptr(p).to_bytes()
            }
        };
        m.extend(s);
        m
    }

    pub(super) fn open(path: &[u8]) -> Result<(), MBuf<u8>> {
        // SAFETY: plain socket calls on a zeroed address.
        unsafe {
            let mut addr: libc::sockaddr_un = core::mem::zeroed();
            if path.is_empty() || path.len() >= addr.sun_path.len() || path[0] != b'/' {
                let mut m = MBuf::new();
                m.extend(b"NABI_DEV must be an absolute socket path shorter than ");
                crate::hot::channel::push_num(&mut m, addr.sun_path.len() as u64);
                m.extend(b" bytes");
                return Err(m);
            }
            addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
            for (d, &s) in addr.sun_path.iter_mut().zip(path) {
                *d = s as libc::c_char;
            }
            let fd = libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0);
            if fd < 0 {
                return Err(os_error(b""));
            }
            let _ = libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
            #[cfg(target_vendor = "apple")]
            {
                let one: libc::c_int = 1;
                libc::setsockopt(
                    fd,
                    libc::SOL_SOCKET,
                    libc::SO_NOSIGPIPE,
                    (&raw const one).cast(),
                    size_of::<libc::c_int>() as libc::socklen_t,
                );
            }
            let len = size_of::<libc::sockaddr_un>() as libc::socklen_t;
            if libc::connect(fd, (&raw const addr).cast(), len) != 0 {
                let e = os_error(b"");
                libc::close(fd);
                return Err(e);
            }
            FD.store(fd, Ordering::Release);
        }
        Ok(())
    }

    pub(super) fn pid() -> u32 {
        // SAFETY: no preconditions.
        unsafe { libc::getpid() as u32 }
    }

    pub(super) fn write(mut b: &[u8]) -> bool {
        let fd = FD.load(Ordering::Acquire);
        #[cfg(target_vendor = "apple")]
        let flags = 0;
        #[cfg(not(target_vendor = "apple"))]
        let flags = libc::MSG_NOSIGNAL;
        while !b.is_empty() {
            // SAFETY: `b` is readable for its length.
            let n = unsafe { libc::send(fd, b.as_ptr().cast(), b.len(), flags) };
            if n < 0 {
                if nabir_common::sys::errno() == libc::EINTR {
                    continue;
                }
                return false;
            }
            b = &b[n as usize..];
        }
        true
    }

    /// Bytes read into `buf` within `ms` (0 when none came); `None` at
    /// the end of the stream.
    pub(super) fn read(buf: &mut [u8], ms: i32) -> Option<usize> {
        let fd = FD.load(Ordering::Acquire);
        let mut p = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one pollfd.
        let r = unsafe { libc::poll(&raw mut p, 1, ms) };
        if r <= 0 {
            return Some(0);
        }
        // SAFETY: `buf` is writable for its length.
        let n = unsafe { libc::recv(fd, buf.as_mut_ptr().cast(), buf.len(), 0) };
        if n < 0 && nabir_common::sys::errno() == libc::EINTR {
            return Some(0);
        }
        (n > 0).then_some(n as usize)
    }
}

#[cfg(windows)]
mod os {
    use core::ffi::c_void;
    use core::sync::atomic::{AtomicIsize, Ordering};
    use nabir_common::win::{self, DWORD, HANDLE, INVALID_HANDLE_VALUE};
    use nabir_common::wtf8::MBuf;

    static PIPE: AtomicIsize = AtomicIsize::new(INVALID_HANDLE_VALUE);

    const GENERIC_READ: DWORD = 0x8000_0000;
    const GENERIC_WRITE: DWORD = 0x4000_0000;
    const OPEN_EXISTING: DWORD = 3;
    const ERROR_PIPE_BUSY: DWORD = 231;

    unsafe extern "system" {
        fn WaitNamedPipeW(name: *const u16, ms: DWORD) -> i32;
        fn PeekNamedPipe(
            h: HANDLE,
            buf: *mut c_void,
            size: DWORD,
            read: *mut DWORD,
            avail: *mut DWORD,
            left: *mut DWORD,
        ) -> i32;
    }

    fn os_error(what: &[u8], code: DWORD) -> MBuf<u8> {
        let mut m = MBuf::new();
        m.extend(what);
        m.extend(b"system error ");
        crate::hot::channel::push_num(&mut m, u64::from(code));
        m
    }

    pub(super) fn open(name: &[u8]) -> Result<(), MBuf<u8>> {
        if !name.starts_with(b"\\\\.\\pipe\\") {
            let mut m = MBuf::new();
            m.extend(b"NABI_DEV must be a pipe name starting with \\\\.\\pipe\\");
            return Err(m);
        }
        let mut w = nabir_common::wtf8::wide(name);
        let p = w.ptr();
        for _ in 0..2 {
            // SAFETY: a NUL-terminated wide name.
            let h = unsafe {
                win::CreateFileW(
                    p,
                    GENERIC_READ | GENERIC_WRITE,
                    0,
                    core::ptr::null_mut(),
                    OPEN_EXISTING,
                    0,
                    0,
                )
            };
            if h != INVALID_HANDLE_VALUE {
                PIPE.store(h, Ordering::Release);
                return Ok(());
            }
            // SAFETY: no preconditions.
            let e = unsafe { win::GetLastError() };
            if e != ERROR_PIPE_BUSY {
                return Err(os_error(b"", e));
            }
            // SAFETY: as above.
            unsafe { WaitNamedPipeW(p, 2000) };
        }
        Err(os_error(b"the pipe stayed busy: ", ERROR_PIPE_BUSY))
    }

    pub(super) fn pid() -> u32 {
        // SAFETY: no preconditions.
        unsafe { win::GetCurrentProcessId() }
    }

    pub(super) fn write(mut b: &[u8]) -> bool {
        let h = PIPE.load(Ordering::Acquire);
        while !b.is_empty() {
            let mut n: DWORD = 0;
            // SAFETY: `b` is readable for its length.
            let ok = unsafe {
                win::WriteFile(
                    h,
                    b.as_ptr().cast(),
                    b.len().min(1 << 20) as DWORD,
                    &raw mut n,
                    core::ptr::null_mut(),
                )
            };
            if ok == 0 {
                return false;
            }
            b = &b[n as usize..];
        }
        true
    }

    /// As on unix; the pipe is polled every 20 ms.
    pub(super) fn read(buf: &mut [u8], ms: i32) -> Option<usize> {
        let h = PIPE.load(Ordering::Acquire);
        let mut waited = 0;
        loop {
            let mut avail: DWORD = 0;
            // SAFETY: a pipe handle; only the count is asked for.
            let ok = unsafe {
                PeekNamedPipe(
                    h,
                    core::ptr::null_mut(),
                    0,
                    core::ptr::null_mut(),
                    &raw mut avail,
                    core::ptr::null_mut(),
                )
            };
            if ok == 0 {
                return None;
            }
            if avail > 0 {
                break;
            }
            if waited >= ms {
                return Some(0);
            }
            // SAFETY: no preconditions.
            unsafe { win::Sleep(20) };
            waited += 20;
        }
        let mut n: DWORD = 0;
        // SAFETY: `buf` is writable for its length.
        let ok = unsafe {
            win::ReadFile(
                h,
                buf.as_mut_ptr().cast(),
                buf.len() as DWORD,
                &raw mut n,
                core::ptr::null_mut(),
            )
        };
        (ok != 0 && n > 0).then_some(n as usize)
    }
}
