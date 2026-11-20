//! Plugins on Windows: `LoadLibraryExW` (the DLL's own directory first
//! for what it imports), and the text range from the PE headers.

use crate::plugin::{Fail, kind};
use core::ffi::c_void;
use nabir_common::win::{self, GetProcAddress};
use nabir_common::wtf8;

const LOAD_WITH_ALTERED_SEARCH_PATH: u32 = 8;
const ERROR_FILE_NOT_FOUND: u32 = 2;
const ERROR_PATH_NOT_FOUND: u32 = 3;
const ERROR_MOD_NOT_FOUND: u32 = 126;

unsafe extern "system" {
    fn LoadLibraryExW(name: *const u16, file: *mut c_void, flags: u32) -> *mut c_void;
    fn FreeLibrary(module: *mut c_void) -> i32;
}

pub(crate) fn open(path: &[u8]) -> Result<*mut c_void, Fail> {
    let mut w = wtf8::wide(path);
    // SAFETY: a NUL-terminated wide path.
    let h = unsafe {
        LoadLibraryExW(
            w.ptr(),
            core::ptr::null_mut(),
            LOAD_WITH_ALTERED_SEARCH_PATH,
        )
    };
    if !h.is_null() {
        return Ok(h);
    }
    // SAFETY: plain call.
    let e = unsafe { win::GetLastError() };
    // SAFETY: as above.
    let missing = unsafe { win::GetFileAttributesW(w.ptr()) } == u32::MAX;
    if missing
        && matches!(
            e,
            ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND | ERROR_MOD_NOT_FOUND
        )
    {
        return Err(Fail::new(kind::NOT_FOUND, path));
    }
    let mut fail = Fail::new(kind::OS, b"LoadLibrary failed: error ");
    let mut digits = [0u8; 10];
    let mut at = digits.len();
    let mut rest = e;
    loop {
        at -= 1;
        digits[at] = b'0' + (rest % 10) as u8;
        rest /= 10;
        if rest == 0 {
            break;
        }
    }
    fail.a.extend(&digits[at..]);
    Err(fail)
}

pub(crate) fn symbol(h: *mut c_void, name: &[u8]) -> *mut c_void {
    // SAFETY: a module from `open`, a NUL-terminated name.
    unsafe { GetProcAddress(h as win::HANDLE, name.as_ptr()) }
}

pub(crate) fn close(h: *mut c_void) {
    if !h.is_null() {
        // SAFETY: a module from `open`, freed once.
        unsafe { FreeLibrary(h) };
    }
}

/// The range of the image's executable sections, and the whole image.
pub(crate) fn ranges(h: *mut c_void, _at: *const u8) -> [(usize, usize); 2] {
    const SCN_MEM_EXECUTE: u32 = 0x2000_0000;
    let base = h as usize;
    if base == 0 {
        return [(0, 0); 2];
    }
    // SAFETY: a loaded image: its DOS and PE headers and section table
    // are mapped.
    unsafe {
        let pe = base + *((base + 0x3c) as *const u32) as usize;
        let nsect = usize::from(*((pe + 6) as *const u16));
        let opt = usize::from(*((pe + 20) as *const u16));
        let image = *((pe + 24 + 56) as *const u32) as usize;
        let mut s = pe + 24 + opt;
        let (mut lo, mut hi) = (usize::MAX, 0);
        for _ in 0..nsect {
            let size = *((s + 8) as *const u32) as usize;
            let rva = *((s + 12) as *const u32) as usize;
            let chars = *((s + 36) as *const u32);
            if chars & SCN_MEM_EXECUTE != 0 {
                lo = lo.min(base + rva);
                hi = hi.max(base + rva + size);
            }
            s += 40;
        }
        let text = if hi == 0 { (0, 0) } else { (lo, hi) };
        [text, (base, base + image)]
    }
}
