//! ELF (`ET_REL`, 64-bit little-endian): allocated sections; RELA sections
//! name the section they apply to. Host thread-locals as offsets from the
//! thread pointer (the host's TLS is static).

use crate::repl::link::{Kind, Obj, Place, alloc_syms, def_sym, fix, flush_code, jit_writable};
use crate::repl::link::{map_sections, sym_value, zeroed};
use crate::repl::{cstr, report, unix::RTLD_DEFAULT};
use core::ffi::c_void;
use core::ptr;

pub(crate) const SYM_PREFIX: usize = 0;

#[cfg(target_arch = "x86_64")]
const MACHINE: u16 = 62;
#[cfg(target_arch = "aarch64")]
const MACHINE: u16 = 183;

const ET_REL: u16 = 1;
const SHT_SYMTAB: u32 = 2;
const SHT_RELA: u32 = 4;
const SHT_NOBITS: u32 = 8;
const SHF_ALLOC: u64 = 2;
const SHF_EXECINSTR: u64 = 4;
const SHF_TLS: u64 = 0x400;
const SHN_UNDEF: u16 = 0;
const SHN_LORESERVE: u16 = 0xff00;
const STB_LOCAL: u8 = 0;
const STT_TLS: u8 = 6;

#[repr(C)]
struct Shdr {
    name: u32,
    kind: u32,
    flags: u64,
    addr: u64,
    offset: u64,
    size: u64,
    link: u32,
    info: u32,
    addralign: u64,
    entsize: u64,
}

#[repr(C)]
struct Sym {
    name: u32,
    info: u8,
    other: u8,
    shndx: u16,
    value: u64,
    size: u64,
}

#[repr(C)]
struct Rela {
    offset: u64,
    info: u64,
    addend: i64,
}

/// `T` at `p`, any alignment.
///
/// # Safety
/// `p` holds a `T`.
const unsafe fn rd<T>(p: *const u8) -> T {
    // SAFETY: caller contract.
    unsafe { ptr::read_unaligned(p.cast::<T>()) }
}

/// Section header `k`.
///
/// # Safety
/// `buf` is an ELF object with section `k`.
const unsafe fn section(buf: *const u8, k: u32) -> Shdr {
    // SAFETY: caller contract.
    unsafe {
        let shoff: u64 = rd(buf.add(40));
        rd(buf.add(shoff as usize + k as usize * size_of::<Shdr>()))
    }
}

/// # Safety
/// `buf` is an ELF object, `symtab` its symbol table holding symbol `i`.
const unsafe fn symbol(buf: *const u8, symtab: &Shdr, i: u32) -> Sym {
    // SAFETY: caller contract.
    unsafe { rd(buf.add(symtab.offset as usize + i as usize * size_of::<Sym>())) }
}

fn thread_pointer() -> *mut u8 {
    let tp: *mut u8;
    // SAFETY: reads the thread pointer register.
    unsafe {
        #[cfg(target_arch = "x86_64")]
        core::arch::asm!("mov {}, qword ptr fs:[0]", out(reg) tp, options(nostack, readonly, preserves_flags));
        #[cfg(target_arch = "aarch64")]
        core::arch::asm!("mrs {}, tpidr_el0", out(reg) tp, options(nomem, nostack, preserves_flags));
    }
    tp
}

/// A thread-local for a cell: its offset from the thread pointer, the same
/// in every thread (the host's TLS is static).
///
/// # Safety
/// `name` is a C string.
pub(crate) unsafe fn host_tls(name: *const u8) -> *mut c_void {
    // SAFETY: caller contract.
    let a = unsafe { libc::dlsym(RTLD_DEFAULT, name.cast()) };
    if a.is_null() {
        return ptr::null_mut();
    }
    (a as usize).wrapping_sub(thread_pointer() as usize) as *mut c_void
}

/// # Safety
/// `name` is a C string.
pub(crate) unsafe fn host_symbol(name: *const u8) -> *mut c_void {
    // SAFETY: caller contract.
    unsafe { libc::dlsym(RTLD_DEFAULT, name.cast()) }
}

/// # Safety
/// `buf` holds `n` bytes of object.
pub(crate) unsafe fn map_object(o: &mut Obj, buf: *const u8, n: usize) -> bool {
    o.buf = buf;
    // SAFETY: the header is checked first; the rest is the object's.
    unsafe {
        if n < 64
            || core::slice::from_raw_parts(buf, 4) != b"\x7fELF"
            || *buf.add(4) != 2
            || rd::<u16>(buf.add(16)) != ET_REL
            || rd::<u16>(buf.add(18)) != MACHINE
        {
            report!("error: {}: not an ELF object of this machine\n", o.what());
            return false;
        }
        o.nsects = u32::from(rd::<u16>(buf.add(60)));
        let shstrndx = u32::from(rd::<u16>(buf.add(62)));
        let names = buf.add(section(buf, shstrndx).offset as usize);
        let p = zeroed::<Place>(o.nsects as usize);
        o.at = zeroed(o.nsects as usize);
        let mut nrel = 0;
        let mut symtab = None;
        for k in 0..o.nsects {
            let s = section(buf, k);
            *p.add(k as usize) = Place::NONE;
            if s.kind == SHT_SYMTAB {
                symtab = Some(k);
            }
            if s.kind == SHT_RELA {
                nrel += s.size as usize / size_of::<Rela>();
            }
            // What runs is mapped: not unwind tables, nor thread-locals (a
            // cell uses the host's).
            if s.flags & SHF_ALLOC == 0
                || s.flags & SHF_TLS != 0
                || cstr(names.add(s.name as usize)) == b".eh_frame"
            {
                continue;
            }
            *p.add(k as usize) = Place {
                size: s.size,
                align: if s.addralign == 0 { 1 } else { s.addralign },
                code: s.flags & SHF_EXECINSTR != 0,
                src: if s.kind == SHT_NOBITS {
                    ptr::null()
                } else {
                    buf.add(s.offset as usize)
                },
            };
        }
        let ok = map_sections(o, core::slice::from_raw_parts(p, o.nsects as usize), nrel);
        libc::free(p.cast());
        if !ok {
            return false;
        }
        let Some(symtab) = symtab else {
            alloc_syms(o, 0);
            return true;
        };
        let st = section(buf, symtab);
        let strs = buf.add(section(buf, st.link).offset as usize);
        alloc_syms(o, (st.size / size_of::<Sym>() as u64) as u32);
        for i in 1..o.nsyms {
            let s = symbol(buf, &st, i);
            if s.shndx == SHN_UNDEF
                || s.shndx >= SHN_LORESERVE
                || u32::from(s.shndx) >= o.nsects
                || o.base(u32::from(s.shndx)).is_null()
                || s.info & 15 == STT_TLS
            {
                continue;
            }
            let global = s.info >> 4 != STB_LOCAL && s.name != 0;
            let at = o.base(u32::from(s.shndx)).wrapping_add(s.value as usize);
            def_sym(o, i, strs.add(s.name as usize), at, global);
        }
        true
    }
}

/// What relocation `t` computes, and whether of a thread-local; None:
/// nothing; Err: not supported.
const fn kind(t: u32) -> Result<Option<(Kind, bool)>, ()> {
    let k = match t {
        #[cfg(target_arch = "x86_64")]
        0 => return Ok(None),
        #[cfg(target_arch = "x86_64")]
        1 => Kind::Abs64,
        #[cfg(target_arch = "x86_64")]
        10 | 11 => Kind::Abs32,
        #[cfg(target_arch = "x86_64")]
        2 => Kind::Pc32,
        #[cfg(target_arch = "x86_64")]
        4 => Kind::Call32,
        // GOTPCREL, GOTPCRELX, REX_GOTPCRELX.
        #[cfg(target_arch = "x86_64")]
        9 | 41 | 42 => Kind::GotPc32,
        // GOTTPOFF.
        #[cfg(target_arch = "x86_64")]
        22 => return Ok(Some((Kind::TlsPc32, true))),
        #[cfg(target_arch = "aarch64")]
        0 => return Ok(None),
        #[cfg(target_arch = "aarch64")]
        257 => Kind::Abs64,
        #[cfg(target_arch = "aarch64")]
        258 => Kind::Abs32,
        #[cfg(target_arch = "aarch64")]
        261 => Kind::Pc32,
        // CALL26, JUMP26.
        #[cfg(target_arch = "aarch64")]
        283 | 282 => Kind::B26,
        #[cfg(target_arch = "aarch64")]
        275 => Kind::Page21,
        // ADD_ABS_LO12_NC, LDST{8,16,32,64,128}_ABS_LO12_NC.
        #[cfg(target_arch = "aarch64")]
        277 | 278 | 284 | 285 | 286 | 299 => Kind::Lo12,
        #[cfg(target_arch = "aarch64")]
        311 => Kind::GotPage21,
        #[cfg(target_arch = "aarch64")]
        312 => Kind::GotLo12,
        // TLSIE_ADR_GOTTPREL_PAGE21, TLSIE_LD64_GOTTPREL_LO12_NC.
        #[cfg(target_arch = "aarch64")]
        541 => return Ok(Some((Kind::TlsPage21, true))),
        #[cfg(target_arch = "aarch64")]
        542 => return Ok(Some((Kind::TlsLo12, true))),
        _ => return Err(()),
    };
    Ok(Some((k, false)))
}

pub(crate) fn link_object(o: &mut Obj) -> bool {
    let mut ok = true;
    jit_writable(true);
    // SAFETY: the object `map_object` accepted; relocations stay within
    // their sections.
    unsafe {
        let buf = o.buf;
        let shnum = u32::from(rd::<u16>(buf.add(60)));
        'sections: for k in 0..o.nsects {
            let rs = section(buf, k);
            if rs.kind != SHT_RELA || rs.info >= shnum || o.base(rs.info).is_null() {
                continue;
            }
            let st = section(buf, rs.link);
            let strs = buf.add(section(buf, st.link).offset as usize);
            let rel = buf.add(rs.offset as usize);
            for r in 0..rs.size as usize / size_of::<Rela>() {
                let rela: Rela = rd(rel.add(r * size_of::<Rela>()));
                let t = rela.info as u32;
                let i = (rela.info >> 32) as u32;
                let p = o.base(rs.info).wrapping_add(rela.offset as usize);
                let (kind, tls) = match kind(t) {
                    Ok(Some(k)) => k,
                    Ok(None) => continue,
                    Err(()) => {
                        report!(
                            "error: {}: relocation type {t} is not supported\n",
                            o.what()
                        );
                        ok = false;
                        break 'sections;
                    }
                };
                let s = symbol(buf, &st, i);
                let (sym, v) = if s.info >> 4 == STB_LOCAL && s.info & 15 != STT_TLS {
                    // A section or local symbol of this object.
                    if u32::from(s.shndx) >= o.nsects || o.base(u32::from(s.shndx)).is_null() {
                        continue;
                    }
                    (
                        -1,
                        (o.base(u32::from(s.shndx)) as u64).wrapping_add(s.value),
                    )
                } else {
                    let v = sym_value(o, i, strs.add(s.name as usize), tls);
                    if v == 0 {
                        ok = false;
                        break 'sections;
                    }
                    (i64::from(i), v)
                };
                if !fix(o, p, kind, sym, v, rela.addend) {
                    ok = false;
                    break 'sections;
                }
            }
        }
    }
    jit_writable(false);
    flush_code(o.code, o.code_cap);
    ok
}
