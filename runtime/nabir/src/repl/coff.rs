//! COFF (and `/bigobj`): sections with relocations, addends in place;
//! symbols with aux records (18-byte, 20 in a big object). Each object's
//! `.pdata` becomes a function table, so exceptions pass its frames.

use crate::repl::link::{Kind, Obj, Place, alloc_syms, def_sym, fix, flush_code, jit_writable};
use crate::repl::link::{map_sections, slot, sym_value, zeroed};
use crate::repl::{report, resolve};
use core::ptr;

#[cfg(target_arch = "x86_64")]
const MACHINE: u16 = 0x8664;
#[cfg(target_arch = "x86_64")]
const PDATA_ENTRY: usize = 12;
#[cfg(target_arch = "aarch64")]
const MACHINE: u16 = 0xaa64;
#[cfg(target_arch = "aarch64")]
const PDATA_ENTRY: usize = 8;

const SCN_CODE: u32 = 0x20;
const SCN_BSS: u32 = 0x80;
const SCN_INFO: u32 = 0x200;
const SCN_REMOVE: u32 = 0x800;
const SCN_NRELOC_OVFL: u32 = 0x0100_0000;
const SCN_DISCARDABLE: u32 = 0x0200_0000;
const SCN_EXECUTE: u32 = 0x2000_0000;
const SYM_EXTERNAL: u8 = 2;
const SYM_WEAK: u8 = 105;

/// `T` at `p`, any alignment.
///
/// # Safety
/// `p` holds a `T`.
const unsafe fn rd<T>(p: *const u8) -> T {
    // SAFETY: caller contract.
    unsafe { ptr::read_unaligned(p.cast::<T>()) }
}

/// The header's fields the loader reads: `big` for a big object.
#[derive(Default)]
struct Layout {
    big: bool,
    machine: u16,
    nsects: u32,
    symoff: u32,
    nsyms: u32,
    sections: usize,
}

/// # Safety
/// `buf` holds `n` bytes.
unsafe fn layout(buf: *const u8, n: usize) -> Layout {
    let mut l = Layout::default();
    if n < 20 {
        return l;
    }
    // SAFETY: caller contract; a big header is 56 bytes.
    unsafe {
        let sig1: u16 = rd(buf);
        let sig2: u16 = rd(buf.add(2));
        let version: u16 = rd(buf.add(4));
        l.big = sig1 == 0 && sig2 == 0xffff && version >= 2 && n >= 56;
        if l.big {
            l.machine = rd(buf.add(6));
            l.nsects = rd(buf.add(44));
            l.symoff = rd(buf.add(48));
            l.nsyms = rd(buf.add(52));
            l.sections = 56;
        } else {
            l.machine = rd(buf);
            l.nsects = u32::from(rd::<u16>(buf.add(2)));
            l.symoff = rd(buf.add(8));
            l.nsyms = rd(buf.add(12));
            l.sections = 20;
            if rd::<u16>(buf.add(16)) != 0 {
                l.machine = 0;
            }
        }
    }
    l
}

/// A section header's fields.
struct Section {
    name: [u8; 8],
    size: u32,
    off: u32,
    reloff: u32,
    nrel: u16,
    flags: u32,
}

/// # Safety
/// `o.buf` is the object `map_object` checked, with section `k`.
unsafe fn section(o: &Obj, k: u32) -> Section {
    // SAFETY: caller contract.
    unsafe {
        let p = o
            .buf
            .add(layout(o.buf, usize::MAX).sections + 40 * k as usize);
        Section {
            name: rd(p),
            size: rd(p.add(16)),
            off: rd(p.add(20)),
            reloff: rd(p.add(24)),
            nrel: rd(p.add(32)),
            flags: rd(p.add(36)),
        }
    }
}

/// A symbol record's fields.
struct Symbol {
    name: *const u8,
    value: u32,
    sect: i32,
    class: u8,
    naux: u8,
    aux: *const u8,
}

/// # Safety
/// `o.buf` is the object `map_object` checked; `i` is at most its count.
unsafe fn symbol(o: &Obj, i: u32) -> Symbol {
    // SAFETY: caller contract.
    unsafe {
        let l = layout(o.buf, usize::MAX);
        let size = if l.big { 20 } else { 18 };
        let p = o.buf.add(l.symoff as usize + i as usize * size);
        Symbol {
            name: p,
            value: rd(p.add(8)),
            sect: if l.big {
                rd(p.add(12))
            } else {
                i32::from(rd::<i16>(p.add(12)))
            },
            class: *p.add(size - 2),
            naux: *p.add(size - 1),
            aux: p.add(size),
        }
    }
}

/// # Safety
/// As `symbol`.
unsafe fn strings(o: &Obj) -> *const u8 {
    // SAFETY: caller contract.
    unsafe { symbol(o, layout(o.buf, usize::MAX).nsyms).name }
}

/// A symbol's or section's name as a C string: inline (up to 8 bytes,
/// copied to `buf`) or in the string table (symbols: 4 zero bytes and an
/// offset; sections: `/<offset>`).
///
/// # Safety
/// As `symbol`; `raw` is 8 bytes.
unsafe fn name(o: &Obj, raw: *const u8, section: bool, buf: &mut [u8; 9]) -> *const u8 {
    // SAFETY: caller contract.
    unsafe {
        let zero: u32 = rd(raw);
        let off: u32 = rd(raw.add(4));
        if !section && zero == 0 {
            return strings(o).add(off as usize);
        }
        ptr::copy_nonoverlapping(raw, buf.as_mut_ptr(), 8);
        buf[8] = 0;
        if section && buf[0] == b'/' {
            let mut n = 0usize;
            for &c in buf[1..].iter().take_while(|c| c.is_ascii_digit()) {
                n = n * 10 + usize::from(c - b'0');
            }
            return strings(o).add(n);
        }
        buf.as_ptr()
    }
}

/// Section `k`'s relocations (10 bytes each) and their count.
///
/// # Safety
/// As `section`.
unsafe fn relocs(o: &Obj, k: u32) -> (*const u8, u32) {
    // SAFETY: caller contract.
    unsafe {
        let s = section(o, k);
        let mut r = o.buf.add(s.reloff as usize);
        let mut n = u32::from(s.nrel);
        // More than 0xffff: the count (this entry included) in the first.
        if s.flags & SCN_NRELOC_OVFL != 0 && s.nrel == 0xffff {
            n = rd::<u32>(r).wrapping_sub(1);
            r = r.add(10);
        }
        (r, n)
    }
}

/// # Safety
/// `buf` holds `n` bytes of object.
pub(crate) unsafe fn map_object(o: &mut Obj, buf: *const u8, n: usize) -> bool {
    o.buf = buf;
    // SAFETY: the header is checked first; the rest is the object's.
    unsafe {
        let h = layout(buf, n);
        if h.machine != MACHINE {
            report!("error: {}: not a COFF object of this machine\n", o.what());
            return false;
        }
        o.nsects = h.nsects;
        let p = zeroed::<Place>(o.nsects as usize);
        o.at = zeroed(o.nsects as usize);
        let mut nrel = 0;
        for k in 0..o.nsects {
            let s = section(o, k);
            nrel += relocs(o, k).1 as usize;
            *p.add(k as usize) = Place::NONE;
            // What runs is mapped: not debug info or linker directives, nor
            // thread-locals (a cell uses the host's).
            if s.flags & (SCN_INFO | SCN_REMOVE | SCN_DISCARDABLE) != 0
                || s.name.starts_with(b".tls")
            {
                continue;
            }
            let align = (s.flags >> 20) & 15;
            *p.add(k as usize) = Place {
                size: u64::from(s.size),
                align: if align == 0 { 16 } else { 1 << (align - 1) },
                code: s.flags & (SCN_CODE | SCN_EXECUTE) != 0,
                src: if s.flags & SCN_BSS != 0 {
                    ptr::null()
                } else {
                    buf.add(s.off as usize)
                },
            };
        }
        let ok = map_sections(o, core::slice::from_raw_parts(p, o.nsects as usize), nrel);
        libc::free(p.cast());
        if !ok {
            return false;
        }
        alloc_syms(o, h.nsyms);
        let mut buf9 = [0u8; 9];
        let mut i = 0;
        while i < o.nsyms {
            let s = symbol(o, i);
            let sect = s.sect;
            if sect > 0 && sect as u32 <= o.nsects && !o.base(sect as u32 - 1).is_null() {
                let at = o.base(sect as u32 - 1).wrapping_add(s.value as usize);
                def_sym(
                    o,
                    i,
                    name(o, s.name, false, &mut buf9),
                    at,
                    s.class == SYM_EXTERNAL,
                );
            }
            i += 1 + u32::from(s.naux);
        }
        // A weak definition: a weak external whose default (its auxiliary
        // record's first word) is defined here.
        i = 0;
        while i < o.nsyms {
            let s = symbol(o, i);
            if s.class == SYM_WEAK && s.naux != 0 {
                let tag: u32 = rd(s.aux);
                if tag < o.nsyms && !o.addr(tag).is_null() {
                    let at = o.addr(tag);
                    def_sym(o, i, name(o, s.name, false, &mut buf9), at, true);
                }
            }
            i += 1 + u32::from(s.naux);
        }
        true
    }
}

/// What relocation `t` at `p` computes, its addend and whether of a
/// thread-local; None: nothing; Err: not supported.
///
/// # Safety
/// `p` is the relocation's place in a mapped section, `at` its offset.
unsafe fn kind(t: u16, p: *const u8, at: u32) -> Result<Option<(Kind, i64, bool)>, ()> {
    // SAFETY: caller contract.
    unsafe {
        let field: i32 = rd(p);
        #[cfg(target_arch = "x86_64")]
        let r = match t {
            0 => return Ok(None), // ABSOLUTE
            1 => (Kind::Abs64, rd::<i64>(p), false),
            2 => (Kind::Abs32, i64::from(field as u32), false),
            3 => (Kind::Rva32, i64::from(field as u32), false),
            4..=9 => {
                // REL32_<n>: n bytes of immediate after the field; a
                // `call` / `jmp` reaches far through a stub.
                let extra = i64::from(t - 4);
                let branch = extra == 0 && at != 0 && matches!(*p.sub(1), 0xe8 | 0xe9);
                let k = if branch { Kind::Call32 } else { Kind::Pc32 };
                (k, i64::from(field) - 4 - extra, false)
            }
            0xb => (Kind::Abs32, i64::from(field), true), // SECREL
            _ => return Err(()),
        };
        #[cfg(target_arch = "aarch64")]
        let r = {
            let _ = at;
            let ins = field as u32;
            let imm12 = i64::from((ins >> 10) & 0xfff);
            // A load / store scales its offset by the access size.
            let scale = if ins & 0x0480_0000 == 0x0480_0000 {
                4
            } else {
                ins >> 30
            };
            match t {
                0 => return Ok(None), // ABSOLUTE
                1 => (Kind::Abs32, i64::from(field as u32), false),
                2 => (Kind::Rva32, i64::from(field as u32), false),
                3 => (Kind::B26, i64::from(((ins << 6) as i32) >> 6) * 4, false),
                // PAGEBASE_REL21
                4 => {
                    let imm = i64::from(((ins >> 29) & 3) | ((ins >> 3) & 0x1f_fffc));
                    (Kind::Page21, (imm << 43) >> 43, false)
                }
                6 => (Kind::Lo12, imm12, false), // PAGEOFFSET_12A
                7 => (Kind::Lo12, imm12 << scale, false), // PAGEOFFSET_12L
                8 => (Kind::Abs32, i64::from(field as u32), true), // SECREL
                9 => (Kind::Lo12, imm12, true),  // SECREL_LOW12A
                0xa => (Kind::Hi12, imm12 << 12, true), // SECREL_HIGH12A
                0xb => (Kind::Lo12, imm12 << scale, true), // SECREL_LOW12L
                0xe => (Kind::Abs64, rd::<i64>(p), false),
                0x11 => (Kind::Pc32, i64::from(field) - 4, false), // REL32: from the next byte
                _ => return Err(()),
            }
        };
        Ok(Some(r))
    }
}

pub(crate) fn link_object(o: &mut Obj) -> bool {
    let mut ok = true;
    let mut buf9 = [0u8; 9];
    jit_writable(true);
    // SAFETY: the object `map_object` accepted; relocations stay within
    // their sections.
    unsafe {
        'sections: for k in 0..o.nsects {
            if o.base(k).is_null() {
                continue;
            }
            let (rel, nrel) = relocs(o, k);
            for r in 0..nrel as usize {
                let at: u32 = rd(rel.add(10 * r));
                let i: u32 = rd(rel.add(10 * r + 4));
                let t: u16 = rd(rel.add(10 * r + 8));
                let p = o.base(k).wrapping_add(at as usize);
                let (kind, a, tls) = match kind(t, p, at) {
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
                let s = symbol(o, i);
                let (sym, v) = if s.class == SYM_EXTERNAL || s.class == SYM_WEAK {
                    let n = name(o, s.name, false, &mut buf9);
                    // An undefined `__imp_<name>` (dllimport): an import slot
                    // of this object holding `<name>`.
                    let imp = if !tls
                        && o.addr(i).is_null()
                        && crate::repl::cstr(n).starts_with(b"__imp_")
                        && resolve(n, false).is_null()
                    {
                        resolve(n.add(6), false)
                    } else {
                        ptr::null_mut()
                    };
                    let v = if imp.is_null() {
                        sym_value(o, i, n, tls)
                    } else {
                        slot(o, i64::from(i), imp as u64) as u64
                    };
                    if v == 0 {
                        ok = false;
                        break 'sections;
                    }
                    (i64::from(i), v)
                } else {
                    // A section or local symbol of this object.
                    if o.addr(i).is_null() {
                        continue;
                    }
                    (-1, o.addr(i) as u64)
                };
                if !fix(o, p, kind, sym, v, a) {
                    ok = false;
                    break 'sections;
                }
            }
        }
        // One unwind table from every `.pdata` section (an entry per
        // function), image-relative to the data mapping and sorted, so
        // exceptions pass through the object's frames.
        let pdata = |o: &Obj, k: u32, buf9: &mut [u8; 9]| {
            let s = section(o, k);
            let n = crate::repl::cstr(name(o, s.name.as_ptr(), true, buf9));
            (!o.base(k).is_null() && n.starts_with(b".pdata"))
                .then_some(s.size as usize / PDATA_ENTRY)
        };
        let mut entries = 0;
        for k in 0..o.nsects {
            entries += pdata(o, k, &mut buf9).unwrap_or(0);
        }
        if ok && entries != 0 {
            let table = libc::malloc(entries * PDATA_ENTRY).cast::<u8>();
            if !table.is_null() {
                let mut at = 0;
                for k in 0..o.nsects {
                    if let Some(n) = pdata(o, k, &mut buf9) {
                        ptr::copy_nonoverlapping(o.base(k), table.add(at), n * PDATA_ENTRY);
                        at += n * PDATA_ENTRY;
                    }
                }
                let t = core::slice::from_raw_parts_mut(table.cast::<[u8; PDATA_ENTRY]>(), entries);
                t.sort_unstable_by_key(|e| u32::from_le_bytes([e[0], e[1], e[2], e[3]]));
                crate::repl::windows::RtlAddFunctionTable(
                    table.cast(),
                    entries as u32,
                    o.data as u64,
                );
            }
        }
    }
    jit_writable(false);
    flush_code(o.code, o.code_cap);
    ok
}
