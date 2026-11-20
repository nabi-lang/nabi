//! Mach-O (`MH_OBJECT`, arm64): the segment's sections, per-section
//! relocations, addends in the instruction or a preceding
//! `ARM64_RELOC_ADDEND`. Host thread-locals through their TLV descriptors.

use crate::repl::cstr;
use crate::repl::link::{Kind, Obj, Place, alloc_syms, def_sym, fix, flush_code, jit_writable};
use crate::repl::link::{map_sections, sym_value, zeroed};
use crate::repl::report;
use core::ffi::c_void;
use core::ptr;

/// C names carry a leading `_`.
pub(crate) const SYM_PREFIX: usize = 1;

const MH_MAGIC_64: u32 = 0xfeed_facf;
const CPU_TYPE_ARM64: u32 = 0x0100_000c;
const LC_SEGMENT_64: u32 = 0x19;
const LC_SYMTAB: u32 = 0x2;
const SECTION_TYPE: u32 = 0xff;
const S_ZEROFILL: u32 = 0x1;
const S_ATTR_DEBUG: u32 = 0x0200_0000;
const S_ATTR_PURE_INSTRUCTIONS: u32 = 0x8000_0000;
const S_ATTR_SOME_INSTRUCTIONS: u32 = 0x400;
const N_STAB: u8 = 0xe0;
const N_TYPE: u8 = 0x0e;
const N_SECT: u8 = 0x0e;
const N_EXT: u8 = 0x01;

const ARM64_RELOC_UNSIGNED: u32 = 0;
const ARM64_RELOC_BRANCH26: u32 = 2;
const ARM64_RELOC_PAGE21: u32 = 3;
const ARM64_RELOC_PAGEOFF12: u32 = 4;
const ARM64_RELOC_GOT_LOAD_PAGE21: u32 = 5;
const ARM64_RELOC_GOT_LOAD_PAGEOFF12: u32 = 6;
const ARM64_RELOC_TLVP_LOAD_PAGE21: u32 = 8;
const ARM64_RELOC_TLVP_LOAD_PAGEOFF12: u32 = 9;
const ARM64_RELOC_ADDEND: u32 = 10;

/// `T` at `p`, any alignment.
///
/// # Safety
/// `p` holds a `T`.
const unsafe fn rd<T>(p: *const u8) -> T {
    // SAFETY: caller contract.
    unsafe { ptr::read_unaligned(p.cast::<T>()) }
}

/// A 16-byte name field up to its NUL.
fn name16(f: &[u8; 16]) -> &[u8] {
    let n = f.iter().position(|&c| c == 0).unwrap_or(16);
    &f[..n]
}

#[repr(C)]
struct Section {
    sectname: [u8; 16],
    segname: [u8; 16],
    addr: u64,
    size: u64,
    offset: u32,
    align: u32,
    reloff: u32,
    nreloc: u32,
    flags: u32,
    reserved: [u32; 3],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Reloc {
    address: i32,
    info: u32,
}

#[repr(C)]
struct Symtab {
    cmd: u32,
    cmdsize: u32,
    symoff: u32,
    nsyms: u32,
    stroff: u32,
    strsize: u32,
}

#[repr(C)]
struct Nlist {
    strx: u32,
    kind: u8,
    sect: u8,
    desc: u16,
    value: u64,
}

const NLIST: usize = size_of::<Nlist>();
const SECTION: usize = size_of::<Section>();
/// `segment_command_64` up to its sections.
const SEGMENT: usize = 72;

/// The load commands of the image or object at `mh`: its sections (and
/// their count) and symbol table, as offsets from `mh`, and the
/// `__LINKEDIT` segment's `vmaddr - fileoff`.
struct Commands {
    sects: usize,
    nsects: u32,
    symtab: Option<Symtab>,
    linkedit: Option<u64>,
}

/// # Safety
/// `mh` is a Mach-O header with its load commands.
unsafe fn commands(mh: *const u8) -> Commands {
    let mut c = Commands {
        sects: 0,
        nsects: 0,
        symtab: None,
        linkedit: None,
    };
    // SAFETY: caller contract.
    unsafe {
        let ncmds: u32 = rd(mh.add(16));
        let mut lc = 32;
        for _ in 0..ncmds {
            let cmd: u32 = rd(mh.add(lc));
            let size: u32 = rd(mh.add(lc + 4));
            if cmd == LC_SEGMENT_64 {
                let name: [u8; 16] = rd(mh.add(lc + 8));
                if name16(&name) == b"__LINKEDIT" {
                    let vmaddr: u64 = rd(mh.add(lc + 24));
                    let fileoff: u64 = rd(mh.add(lc + 40));
                    c.linkedit = Some(vmaddr.wrapping_sub(fileoff));
                }
                c.sects = lc + SEGMENT;
                c.nsects = rd(mh.add(lc + 64));
            }
            if cmd == LC_SYMTAB {
                c.symtab = Some(rd(mh.add(lc)));
            }
            lc += size as usize;
        }
    }
    c
}

unsafe extern "C" {
    #[link_name = "_dyld_get_image_header"]
    fn dyld_get_image_header(index: u32) -> *const c_void;
    #[link_name = "_dyld_get_image_vmaddr_slide"]
    fn dyld_get_image_vmaddr_slide(index: u32) -> isize;
}

/// Look a host symbol up in its symbol table, not `dlsym`: a thread-local
/// needs its TLV descriptor, not this thread's instance.
///
/// # Safety
/// `name` is a C string.
pub(crate) unsafe fn host_symbol(name: *const u8) -> *mut c_void {
    // SAFETY: the main image's own headers and tables.
    unsafe {
        let mh = dyld_get_image_header(0).cast::<u8>();
        let slide = dyld_get_image_vmaddr_slide(0) as u64;
        let c = commands(mh);
        let (Some(base), Some(st)) = (c.linkedit, c.symtab) else {
            return ptr::null_mut();
        };
        let base = base.wrapping_add(slide) as *const u8;
        let nl = base.add(st.symoff as usize);
        let strs = base.add(st.stroff as usize);
        let want = cstr(name);
        // Linear scan: runs once per thread-local the session uses.
        for i in 0..st.nsyms as usize {
            let s: Nlist = rd(nl.add(i * NLIST));
            if s.kind & N_STAB == 0
                && s.kind & N_TYPE == N_SECT
                && cstr(strs.add(s.strx as usize)) == want
            {
                return s.value.wrapping_add(slide) as *mut c_void;
            }
        }
        ptr::null_mut()
    }
}

/// A thread-local's value for a cell: its TLV descriptor.
///
/// # Safety
/// As `host_symbol`.
pub(crate) unsafe fn host_tls(name: *const u8) -> *mut c_void {
    // SAFETY: caller contract.
    unsafe { host_symbol(name) }
}

/// # Safety
/// `buf` holds `n` bytes of object.
pub(crate) unsafe fn map_object(o: &mut Obj, buf: *const u8, n: usize) -> bool {
    o.buf = buf;
    // SAFETY: the header is checked first; the rest is the object's.
    unsafe {
        if n < 32 || rd::<u32>(buf) != MH_MAGIC_64 || rd::<u32>(buf.add(4)) != CPU_TYPE_ARM64 {
            report!("error: {}: not a Mach-O object of this machine\n", o.what());
            return false;
        }
        let c = commands(buf);
        o.nsects = c.nsects;
        // What runs is mapped: not debug info, unwind tables or the like.
        let p = zeroed::<Place>(o.nsects as usize);
        o.at = zeroed(o.nsects as usize);
        let mut nrel = 0;
        for k in 0..o.nsects as usize {
            let s: Section = rd(buf.add(c.sects + k * SECTION));
            *o.at.add(k) = s.addr;
            *p.add(k) = Place::NONE;
            if s.flags & S_ATTR_DEBUG != 0
                || name16(&s.segname) == b"__LD"
                || name16(&s.sectname) == b"__eh_frame"
            {
                continue;
            }
            *p.add(k) = Place {
                size: s.size,
                align: 1 << s.align,
                code: s.flags & (S_ATTR_PURE_INSTRUCTIONS | S_ATTR_SOME_INSTRUCTIONS) != 0,
                src: if s.flags & SECTION_TYPE == S_ZEROFILL {
                    ptr::null()
                } else {
                    buf.add(s.offset as usize)
                },
            };
            nrel += s.nreloc as usize;
        }
        let ok = map_sections(o, core::slice::from_raw_parts(p, o.nsects as usize), nrel);
        libc::free(p.cast());
        if !ok {
            return false;
        }
        let Some(st) = c.symtab else {
            alloc_syms(o, 0);
            return true;
        };
        let nl = buf.add(st.symoff as usize);
        let strs = buf.add(st.stroff as usize);
        alloc_syms(o, st.nsyms);
        for i in 0..st.nsyms {
            let s: Nlist = rd(nl.add(i as usize * NLIST));
            if s.kind & N_STAB != 0 || s.kind & N_TYPE != N_SECT {
                continue;
            }
            let k = u32::from(s.sect).wrapping_sub(1);
            if k >= o.nsects || o.base(k).is_null() {
                continue;
            }
            let at = o
                .base(k)
                .wrapping_add(s.value.wrapping_sub(*o.at.add(k as usize)) as usize);
            def_sym(o, i, strs.add(s.strx as usize), at, s.kind & N_EXT != 0);
        }
        true
    }
}

pub(crate) fn link_object(o: &mut Obj) -> bool {
    let mut ok = true;
    jit_writable(true);
    // SAFETY: the object `map_object` accepted; relocations stay within
    // their sections.
    unsafe {
        let c = commands(o.buf);
        let (nl, strs) = match &c.symtab {
            Some(st) => (o.buf.add(st.symoff as usize), o.buf.add(st.stroff as usize)),
            None => (ptr::null(), c"".as_ptr().cast()),
        };
        'sections: for k in 0..o.nsects {
            if o.base(k).is_null() {
                continue;
            }
            let s: Section = rd(o.buf.add(c.sects + k as usize * SECTION));
            let rel = o.buf.add(s.reloff as usize);
            let mut r = 0;
            while r < s.nreloc {
                let mut ri: Reloc = rd(rel.add(r as usize * 8));
                let mut a: i64 = 0;
                if ri.info >> 28 == ARM64_RELOC_ADDEND {
                    // The addend: a 24-bit signed `r_symbolnum`.
                    a = (i64::from(ri.info & 0xff_ffff) << 40) >> 40;
                    r += 1;
                    ri = rd(rel.add(r as usize * 8));
                }
                r += 1;
                let Reloc { address, info } = ri;
                let symbolnum = info & 0xff_ffff;
                let length = (info >> 25) & 3;
                let external = (info >> 27) & 1 != 0;
                let kind = info >> 28;
                let p = o.base(k).wrapping_add(address as usize);
                let mut tls = false;
                let kind = match kind {
                    ARM64_RELOC_UNSIGNED if length == 3 => Kind::Abs64,
                    ARM64_RELOC_UNSIGNED => Kind::Abs32,
                    ARM64_RELOC_BRANCH26 => Kind::B26,
                    ARM64_RELOC_PAGE21 => Kind::Page21,
                    ARM64_RELOC_PAGEOFF12 => Kind::Lo12,
                    ARM64_RELOC_GOT_LOAD_PAGE21 => Kind::GotPage21,
                    ARM64_RELOC_GOT_LOAD_PAGEOFF12 => Kind::GotLo12,
                    ARM64_RELOC_TLVP_LOAD_PAGE21 => {
                        tls = true;
                        Kind::TlsPage21
                    }
                    ARM64_RELOC_TLVP_LOAD_PAGEOFF12 => {
                        tls = true;
                        Kind::TlsLo12
                    }
                    t => {
                        report!(
                            "error: {}: relocation type {t} is not supported\n",
                            o.what()
                        );
                        ok = false;
                        break 'sections;
                    }
                };
                // The bytes hold the addend (section-relative: the object
                // address).
                if kind == Kind::Abs64 {
                    a = a.wrapping_add(rd::<i64>(p));
                } else if kind == Kind::Abs32 {
                    a = a.wrapping_add(i64::from(rd::<i32>(p)));
                }
                let (sym, v) = if external {
                    let name =
                        strs.add(rd::<Nlist>(nl.add(symbolnum as usize * NLIST)).strx as usize);
                    let v = sym_value(o, symbolnum, name, tls);
                    if v == 0 {
                        ok = false;
                        break 'sections;
                    }
                    (i64::from(symbolnum), v)
                } else {
                    let i = symbolnum.wrapping_sub(1);
                    if i >= o.nsects || o.base(i).is_null() {
                        continue;
                    }
                    // Section-relative: the target's object address moved
                    // by its section's.
                    (-1, (o.base(i) as u64).wrapping_sub(*o.at.add(i as usize)))
                };
                if !fix(o, p, kind, sym, v, a) {
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
