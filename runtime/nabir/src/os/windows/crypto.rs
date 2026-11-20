//! Randomness and trust anchors (crypto, `net::tls`).

#[allow(clippy::wildcard_imports)]
use crate::os::windows::*;

/// Fill `buf[off..off + len]` from the system RNG (`BCryptGenRandom`): 0
/// or `-error`.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_random(buf: *mut u8, off: usize, len: usize) -> i32 {
    let mut done = 0;
    while done < len {
        let n = (len - done).min(1 << 30);
        // SAFETY: the library passes a live region of `len` bytes.
        let s = unsafe {
            win::BCryptGenRandom(
                ptr::null_mut(),
                region(buf, off + done),
                n as u32,
                win::BCRYPT_USE_SYSTEM_PREFERRED_RNG,
            )
        };
        if s < 0 {
            return -win::ERROR_NOT_SUPPORTED;
        }
        done += n;
    }
    0
}

/// `CERT_CONTEXT` (wincrypt.h).
#[repr(C)]
pub(crate) struct CertContext {
    encoding: DWORD,
    der: *const u8,
    der_len: DWORD,
    info: *mut c_void,
    store: *mut c_void,
}

/// The `ROOT` system store's certificates (crypt32, loaded on first use;
/// it is not in the import set) as records `[u32 big-endian length][DER]`.
pub(crate) fn root_store() -> Option<MBuf<u8>> {
    type Open = unsafe extern "system" fn(usize, *const u16) -> *mut c_void;
    type Enum = unsafe extern "system" fn(*mut c_void, *const CertContext) -> *const CertContext;
    type Close = unsafe extern "system" fn(*mut c_void, DWORD) -> win::BOOL;
    // SAFETY: plain module load; each export has the declared signature.
    unsafe {
        let m = win::LoadLibraryW(wide(b"crypt32.dll").ptr());
        if m == 0 {
            return None;
        }
        let proc = |name: &[u8]| {
            let p = win::GetProcAddress(m, name.as_ptr());
            (!p.is_null()).then_some(p)
        };
        let open: Open = core::mem::transmute(proc(b"CertOpenSystemStoreW\0")?);
        let next: Enum = core::mem::transmute(proc(b"CertEnumCertificatesInStore\0")?);
        let close: Close = core::mem::transmute(proc(b"CertCloseStore\0")?);
        let store = open(0, wide(b"ROOT").ptr());
        if store.is_null() {
            return None;
        }
        let mut out = MBuf::new();
        let mut c: *const CertContext = ptr::null();
        loop {
            c = next(store, c);
            if c.is_null() {
                break;
            }
            let n = (*c).der_len as usize;
            out.extend(&(n as u32).to_be_bytes());
            out.extend(core::slice::from_raw_parts((*c).der, n));
        }
        close(store, 0);
        Some(out)
    }
}

/// The OS's trusted root certificates into `buf[off..off + len]` as
/// records `[u32 big-endian length][DER]`: their full length or `-error`.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_os_trust_roots(buf: *mut u8, off: usize, len: usize) -> isize {
    let (roots, _) = native(root_store);
    match roots {
        Some(v) => put(buf, off, len, v.as_slice()),
        None => -(win::ERROR_NOT_SUPPORTED as isize),
    }
}
