//! Randomness and trust anchors (crypto, `net::tls`).

#[allow(clippy::wildcard_imports)]
use crate::os::posix::*;

/// Fill `buf[off..off + len]` from the OS random source (Linux
/// `getrandom`, else `getentropy`; WASI `random_get` underneath): 0 or
/// `-errno`.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_random(buf: *mut u8, off: usize, len: usize) -> i32 {
    let mut done = 0;
    while done < len {
        // SAFETY: the library passes a live region of `len` bytes.
        let p = unsafe { region(buf, off + done) }.cast::<libc::c_void>();
        // SAFETY: `p` is writable for `len - done` bytes.
        #[cfg(any(target_os = "linux", target_os = "android"))]
        let r = unsafe { libc::getrandom(p, len - done, 0) };
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        let r = {
            let n = (len - done).min(256);
            // SAFETY: `p` is writable for `n` bytes, getentropy's limit.
            if unsafe { libc::getentropy(p, n) } == 0 {
                n as isize
            } else {
                -1
            }
        };
        if r < 0 {
            let e = errno();
            if e != libc::EINTR {
                return -e;
            }
            continue;
        }
        done += r as usize;
    }
    0
}

/// The OS's trusted root certificates into `buf[off..off + len]` as
/// records `[u32 big-endian length][DER]`: their full length or `-errno`
/// (`-ENOSYS` where the OS has no certificate store API; the library
/// reads the bundle files there).
#[cfg(target_vendor = "apple")]
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_trust_roots(buf: *mut u8, off: usize, len: usize) -> isize {
    let (roots, _) = native(keychain::roots);
    match roots {
        Some(v) => put(buf, off, len, v.as_slice()),
        None => -(libc::ENOSYS as isize),
    }
}

#[cfg(not(target_vendor = "apple"))]
#[unsafe(no_mangle)]
pub const extern "C" fn nabi_os_trust_roots(_buf: *mut u8, _off: usize, _len: usize) -> isize {
    -(libc::ENOSYS as isize)
}

/// Security.framework, loaded on first use (programs that never ask for
/// the roots do not link it).
#[cfg(target_vendor = "apple")]
mod keychain {
    use core::ffi::{CStr, c_void};
    use core::mem::transmute_copy;
    use core::ptr;
    use nabir_common::wtf8::MBuf;

    type Ref = *const c_void;

    struct Api {
        anchors: unsafe extern "C" fn(*mut Ref) -> i32,
        domain_certs: unsafe extern "C" fn(i32, *mut Ref) -> i32,
        settings: unsafe extern "C" fn(Ref, i32, *mut Ref) -> i32,
        cert_data: unsafe extern "C" fn(Ref) -> Ref,
        count: unsafe extern "C" fn(Ref) -> isize,
        at: unsafe extern "C" fn(Ref, isize) -> Ref,
        data_len: unsafe extern "C" fn(Ref) -> isize,
        data_ptr: unsafe extern "C" fn(Ref) -> *const u8,
        release: unsafe extern "C" fn(Ref),
    }

    /// Symbol `name` of `h` as the function pointer type `F`.
    ///
    /// # Safety
    /// `F` must be a function pointer type matching the symbol.
    unsafe fn sym<F>(h: *mut c_void, name: &CStr) -> Option<F> {
        // SAFETY: `h` is a live handle, `name` NUL-terminated.
        let p = unsafe { libc::dlsym(h, name.as_ptr()) };
        // SAFETY: caller contract; `F` is pointer-sized.
        (!p.is_null()).then(|| unsafe { transmute_copy::<*mut c_void, F>(&p) })
    }

    fn api() -> Option<Api> {
        // SAFETY: plain dlopen of a system framework.
        let h = unsafe {
            libc::dlopen(
                c"/System/Library/Frameworks/Security.framework/Security".as_ptr(),
                libc::RTLD_LAZY | libc::RTLD_LOCAL,
            )
        };
        if h.is_null() {
            return None;
        }
        // SAFETY: each symbol has the C signature declared in `Api`.
        unsafe {
            Some(Api {
                anchors: sym(h, c"SecTrustCopyAnchorCertificates")?,
                domain_certs: sym(h, c"SecTrustSettingsCopyCertificates")?,
                settings: sym(h, c"SecTrustSettingsCopyTrustSettings")?,
                cert_data: sym(h, c"SecCertificateCopyData")?,
                count: sym(h, c"CFArrayGetCount")?,
                at: sym(h, c"CFArrayGetValueAtIndex")?,
                data_len: sym(h, c"CFDataGetLength")?,
                data_ptr: sym(h, c"CFDataGetBytePtr")?,
                release: sym(h, c"CFRelease")?,
            })
        }
    }

    /// Append certificate `cert`'s DER as a record.
    fn push(a: &Api, out: &mut MBuf<u8>, cert: Ref) {
        // SAFETY: `cert` is a live SecCertificateRef; the copied data
        // is released after use.
        unsafe {
            let d = (a.cert_data)(cert);
            if d.is_null() {
                return;
            }
            let n = (a.data_len)(d) as usize;
            out.extend(&(n as u32).to_be_bytes());
            out.extend(core::slice::from_raw_parts((a.data_ptr)(d), n));
            (a.release)(d);
        }
    }

    /// The system anchors, plus the user (0) and admin (1) domains'
    /// certificates trusted unconditionally (an empty trust settings
    /// array). Certificates with qualified trust settings are left out.
    pub fn roots() -> Option<MBuf<u8>> {
        let a = api()?;
        let mut out = MBuf::new();
        // SAFETY: Core Foundation calls on objects this function owns
        // (copied) or borrows from them (array elements).
        unsafe {
            let mut arr: Ref = ptr::null();
            if (a.anchors)(&mut arr) == 0 && !arr.is_null() {
                for i in 0..(a.count)(arr) {
                    push(&a, &mut out, (a.at)(arr, i));
                }
                (a.release)(arr);
            }
            for domain in [0, 1] {
                let mut arr: Ref = ptr::null();
                if (a.domain_certs)(domain, &mut arr) != 0 || arr.is_null() {
                    continue;
                }
                for i in 0..(a.count)(arr) {
                    let cert = (a.at)(arr, i);
                    let mut s: Ref = ptr::null();
                    if (a.settings)(cert, domain, &mut s) == 0 && !s.is_null() {
                        let unconditional = (a.count)(s) == 0;
                        (a.release)(s);
                        if unconditional {
                            push(&a, &mut out, cert);
                        }
                    }
                }
                (a.release)(arr);
            }
        }
        Some(out)
    }
}
