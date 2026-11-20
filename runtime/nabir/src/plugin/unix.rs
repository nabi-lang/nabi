//! Plugins on POSIX: `dlopen`, and the text range from the image's
//! headers (Mach-O load commands, ELF program headers).

use crate::plugin::{Fail, kind};
use core::ffi::c_void;
use nabir_common::wtf8::MBuf;

/// An executable segment (not in the `libc` crate for Android).
#[cfg(not(target_vendor = "apple"))]
const PF_X: u32 = 1;

pub(crate) fn open(path: &[u8]) -> Result<*mut c_void, Fail> {
    let mut p: MBuf<u8> = MBuf::new();
    p.extend(path);
    p.push(0);
    // SAFETY: a NUL-terminated path.
    let h = unsafe {
        libc::dlopen(
            p.as_slice().as_ptr().cast(),
            libc::RTLD_NOW | libc::RTLD_LOCAL,
        )
    };
    if !h.is_null() {
        return Ok(h);
    }
    // SAFETY: as above.
    if unsafe { libc::access(p.as_slice().as_ptr().cast(), libc::F_OK) } != 0 {
        return Err(Fail::new(kind::NOT_FOUND, path));
    }
    // SAFETY: the message of the failed `dlopen`.
    let msg = unsafe {
        let e = libc::dlerror();
        if e.is_null() {
            &b"dlopen failed"[..]
        } else {
            core::ffi::CStr::from_ptr(e).to_bytes()
        }
    };
    Err(Fail::new(kind::OS, msg))
}

pub(crate) fn symbol(h: *mut c_void, name: &[u8]) -> *mut c_void {
    // SAFETY: a handle from `open`, a NUL-terminated name.
    unsafe { libc::dlsym(h, name.as_ptr().cast()) }
}

pub(crate) fn close(h: *mut c_void) {
    if !h.is_null() {
        // SAFETY: a handle from `open`, closed once.
        unsafe { libc::dlclose(h) };
    }
}

/// The code range and the whole range of the image holding `at`.
#[cfg(target_vendor = "apple")]
pub(crate) fn ranges(_h: *mut c_void, at: *const u8) -> [(usize, usize); 2] {
    const LC_SEGMENT_64: u32 = 0x19;
    // SAFETY: `dladdr` on an address of a loaded image; its Mach-O header
    // and load commands are mapped with it.
    unsafe {
        let mut info: libc::Dl_info = core::mem::zeroed();
        if libc::dladdr(at.cast(), &raw mut info) == 0 || info.dli_fbase.is_null() {
            return [(0, 0); 2];
        }
        let base = info.dli_fbase as usize;
        let ncmds = *((base + 16) as *const u32);
        let mut cmd = base + 32;
        let mut slide = None;
        let mut segs: (usize, usize) = (usize::MAX, 0);
        for _ in 0..ncmds {
            let (kind, size) = (*(cmd as *const u32), *((cmd + 4) as *const u32));
            if kind == LC_SEGMENT_64 {
                let name = core::slice::from_raw_parts((cmd + 8) as *const u8, 16);
                let vmaddr = *((cmd + 24) as *const u64) as usize;
                let vmsize = *((cmd + 32) as *const u64) as usize;
                if name.starts_with(b"__TEXT\0") {
                    let s = base.wrapping_sub(vmaddr);
                    slide = Some((s, vmaddr + s, vmaddr + s + vmsize));
                }
                if vmsize != 0 {
                    segs = (segs.0.min(vmaddr), segs.1.max(vmaddr + vmsize));
                }
            }
            cmd += size as usize;
        }
        slide.map_or([(0, 0); 2], |(s, lo, hi)| {
            [(lo, hi), (segs.0 + s, segs.1 + s)]
        })
    }
}

/// The code range and the whole range of the image holding `at`.
#[cfg(not(target_vendor = "apple"))]
pub(crate) fn ranges(_h: *mut c_void, at: *const u8) -> [(usize, usize); 2] {
    struct Want {
        at: usize,
        text: [(usize, usize); 2],
    }
    unsafe extern "C" fn each(info: *mut libc::dl_phdr_info, _: usize, data: *mut c_void) -> i32 {
        // SAFETY: the loader's description of one image; `data` is ours.
        unsafe {
            let w = &mut *data.cast::<Want>();
            let base = (*info).dlpi_addr as usize;
            let ph =
                core::slice::from_raw_parts((*info).dlpi_phdr, usize::from((*info).dlpi_phnum));
            let mut mine = false;
            let mut text = [(0, 0), (usize::MAX, 0)];
            for p in ph.iter().filter(|p| p.p_type == libc::PT_LOAD) {
                let lo = base + p.p_vaddr as usize;
                let hi = lo + p.p_memsz as usize;
                mine |= w.at >= lo && w.at < hi;
                if p.p_flags & PF_X != 0 {
                    text[0] = (lo, hi);
                }
                text[1] = (text[1].0.min(lo), text[1].1.max(hi));
            }
            if mine {
                w.text = text;
                return 1;
            }
            0
        }
    }
    let mut w = Want {
        at: at as usize,
        text: [(0, 0); 2],
    };
    // SAFETY: `each` only reads the loader's records.
    unsafe { libc::dl_iterate_phdr(Some(each), (&raw mut w).cast()) };
    w.text
}
