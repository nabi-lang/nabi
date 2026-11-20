//! Unwind tables as the directives an assembler builds them from: ELF
//! `.eh_frame` as `.cfi_*`, Mach-O arm64 compact unwind as the `.cfi_*`
//! it is derived from, COFF x64 `.pdata` / `.xdata` as `.seh_*`, 32-bit
//! ARM `.ARM.exidx` as `.fnstart` / `.save` / `.setfp` / `.fnend`. Each
//! table is decoded; one this cannot express (or an assembler would
//! write differently) leaves all of the object's tables as data. COFF
//! arm64 keeps its `.xdata` as data: an assembler packs unwind codes
//! (`.pdata` packed entries, epilogs sharing the prolog's codes) where
//! `nabic` writes them out.

use crate::asm::code::{FnSrc, Isa};
use crate::asm::read::{Format, Object, Rel};
use nabic_core::HashMap;
use nabic_core::leb::{read_sleb, read_uleb};

/// `(code offset, directive)` of each function (as the object names it),
/// in order; the sections they replace.
pub type Directives = (HashMap<String, Vec<(u32, String)>>, Vec<bool>);

/// A format's tables decoded: each function's directives, the sections
/// they come from.
type Tables = (HashMap<String, Vec<(u32, String)>>, Vec<usize>);

pub fn directives(o: &Object, isa: Isa, be: bool, fns: &HashMap<&str, &FnSrc>) -> Directives {
    let none = (HashMap::default(), vec![false; o.sections.len()]);
    let got = match (o.format, isa) {
        (Format::Elf, Isa::Arm) => exidx(o),
        (Format::Elf, _) => eh_frame(o, isa, be),
        (Format::MachO, Isa::A64) => compact(o),
        (Format::Coff, Isa::X64) => seh(o),
        _ => None,
    };
    let Some((mut dirs, hide)) = got else {
        return none;
    };
    // Each directive must fall on an instruction of its function.
    for (name, ds) in &dirs {
        let Some(f) = fns.get(name.as_str()) else {
            return none;
        };
        if !ds.iter().all(|d| f.marks.iter().any(|m| m.0 == d.0)) {
            return none;
        }
    }
    for ds in dirs.values_mut() {
        ds.sort_by_key(|d| d.0);
    }
    let mut hidden = vec![false; o.sections.len()];
    for k in hide {
        hidden[k] = true;
    }
    (dirs, hidden)
}

fn u32_at(b: &[u8], at: usize, be: bool) -> Option<u32> {
    let x: [u8; 4] = b.get(at..at + 4)?.try_into().ok()?;
    Some(if be {
        u32::from_be_bytes(x)
    } else {
        u32::from_le_bytes(x)
    })
}

/// The function a relocation at `r` (place value `place`) points to the
/// start of, as the object names it.
fn target(o: &Object, r: &Rel, place: i64) -> Option<String> {
    let at_start = |sect: u32, value: i64| -> Option<String> {
        o.syms
            .iter()
            .find(|y| {
                y.sect == sect
                    && i64::try_from(y.value).ok() == Some(value)
                    && !y.name.is_empty()
                    && !y.section_sym
                    && match o.format {
                        Format::Elf => y.info & 0xf == 2,
                        _ => true,
                    }
            })
            .map(|y| y.name.clone())
    };
    match o.format {
        Format::Elf => {
            let y = o.syms.get(r.sym as usize)?;
            let add = if o.rela { r.addend } else { place };
            if y.info & 0xf == 3 {
                at_start(y.sect, add)
            } else {
                (add == 0).then(|| y.name.clone())
            }
        }
        Format::MachO if r.ext => o
            .syms
            .get(r.sym as usize)
            .filter(|_| place == 0)
            .map(|y| y.name.clone()),
        Format::MachO => at_start(r.sym, place),
        Format::Coff => {
            let y = o.syms.get(r.sym as usize)?;
            if y.section_sym {
                at_start(y.sect, place)
            } else {
                (place == 0).then(|| y.name.clone())
            }
        }
    }
}

/// The CIE an assembler writes for `isa`: data alignment, return
/// address register, initial rules (`DW_CFA_*` bytes).
const fn cie(isa: Isa) -> (i64, u64, &'static [u8]) {
    match isa {
        Isa::X64 => (-8, 16, &[0x0c, 7, 8, 0x90, 1]),
        Isa::I386 => (-4, 8, &[0x0c, 4, 4, 0x88, 1]),
        Isa::Ppc64 => (-8, 65, &[0x0c, 1, 0]),
        Isa::S390x => (-8, 14, &[0x0c, 15, 0xa0, 1]),
        Isa::Riscv => (0, 1, &[0x0c, 2, 0]),
        Isa::A64 | Isa::Arm => (-8, 30, &[0x0c, 31, 0]),
    }
}

/// `.eh_frame`: a CIE as the assembler's own, FDEs of `DW_CFA_*` rules the
/// directives say.
fn eh_frame(o: &Object, isa: Isa, be: bool) -> Option<Tables> {
    let k = o.sections.iter().position(|s| s.name == ".eh_frame")?;
    let s = &o.sections[k];
    let b = &s.data;
    let mut dirs: HashMap<String, Vec<(u32, String)>> = HashMap::default();
    let mut at = 0usize;
    let (mut code_align, mut data_align) = (1u64, 1i64);
    while at + 4 <= b.len() {
        let len = u32_at(b, at, be)? as usize;
        if len == 0 {
            break;
        }
        let body = at + 4;
        let end = body + len;
        let id = u32_at(b, body, be)?;
        if id == 0 {
            // The CIE: "zR", pc-relative sdata4 pointers.
            let mut i = body + 4;
            if b.get(i) != Some(&1) {
                return None;
            }
            i += 1;
            let aug_end = i + b.get(i..)?.iter().position(|&c| c == 0)?;
            if &b[i..aug_end] != b"zR" {
                return None;
            }
            i = aug_end + 1;
            code_align = read_uleb(b, &mut i)?;
            data_align = read_sleb(b, &mut i)?;
            let ra = u64::from(*b.get(i)?);
            i += 1;
            if read_uleb(b, &mut i)? != 1 || b.get(i) != Some(&0x1b) {
                return None;
            }
            i += 1;
            let init = b.get(i..end)?;
            let (da, r, want) = cie(isa);
            let rv32 = isa == Isa::Riscv && data_align == -4;
            if (da != 0 && da != data_align) || (isa == Isa::Riscv && !rv32 && data_align != -8) {
                return None;
            }
            // The rules, then `DW_CFA_nop` padding.
            let same = init.starts_with(want) && init[want.len()..].iter().all(|&c| c == 0);
            if r != ra || !same {
                return None;
            }
        } else {
            let pc = body + 4;
            let r = s.relocs.iter().find(|r| r.off == pc as u64)?;
            let place = i64::from(u32_at(b, pc, be)? as i32);
            let name = target(o, r, place)?;
            let range = u32_at(b, pc + 4, be)?;
            let mut i = pc + 8;
            if read_uleb(b, &mut i)? != 0 {
                return None;
            }
            let mut out = vec![(0, ".cfi_startproc".to_owned())];
            let mut loc = 0u64;
            while i < end {
                let op = b[i];
                i += 1;
                let d = match op >> 6 {
                    1 => {
                        loc += u64::from(op & 0x3f) * code_align;
                        continue;
                    }
                    2 => {
                        let n = read_uleb(b, &mut i)? as i64 * data_align;
                        format!(".cfi_offset {}, {n}", op & 0x3f)
                    }
                    3 => format!(".cfi_restore {}", op & 0x3f),
                    _ => match op {
                        0 => continue,
                        2 => {
                            loc += u64::from(*b.get(i)?) * code_align;
                            i += 1;
                            continue;
                        }
                        3 => {
                            let x: [u8; 2] = b.get(i..i + 2)?.try_into().ok()?;
                            let x = if be {
                                u16::from_be_bytes(x)
                            } else {
                                u16::from_le_bytes(x)
                            };
                            loc += u64::from(x) * code_align;
                            i += 2;
                            continue;
                        }
                        4 => {
                            loc += u64::from(u32_at(b, i, be)?) * code_align;
                            i += 4;
                            continue;
                        }
                        5 => {
                            let r = read_uleb(b, &mut i)?;
                            let n = read_uleb(b, &mut i)? as i64 * data_align;
                            format!(".cfi_offset {r}, {n}")
                        }
                        0x11 => {
                            let r = read_uleb(b, &mut i)?;
                            let n = read_sleb(b, &mut i)? * data_align;
                            format!(".cfi_offset {r}, {n}")
                        }
                        0x0a => ".cfi_remember_state".to_owned(),
                        0x0b => ".cfi_restore_state".to_owned(),
                        0x0c => {
                            let r = read_uleb(b, &mut i)?;
                            let n = read_uleb(b, &mut i)?;
                            format!(".cfi_def_cfa {r}, {n}")
                        }
                        0x0d => format!(".cfi_def_cfa_register {}", read_uleb(b, &mut i)?),
                        0x0e => format!(".cfi_def_cfa_offset {}", read_uleb(b, &mut i)?),
                        _ => return None,
                    },
                };
                if loc > u64::from(range) {
                    return None;
                }
                out.push((loc as u32, d));
            }
            out.push((range, ".cfi_endproc".to_owned()));
            if dirs.insert(name, out).is_some() {
                return None;
            }
        }
        at = end;
    }
    Some((dirs, vec![k]))
}

/// The register pairs of a compact unwind frame encoding, in the order
/// compact unwind saves them below the frame record.
const PAIRS: [(u32, &str, &str); 9] = [
    (0x1, "x19", "x20"),
    (0x2, "x21", "x22"),
    (0x4, "x23", "x24"),
    (0x8, "x25", "x26"),
    (0x10, "x27", "x28"),
    (0x100, "d8", "d9"),
    (0x200, "d10", "d11"),
    (0x400, "d12", "d13"),
    (0x800, "d14", "d15"),
];
const MODE_FRAME: u32 = 0x0400_0000;

/// Mach-O arm64 `__compact_unwind`: frame-based entries, which an
/// assembler derives from the frame record and register pairs the
/// `.cfi_*` directives describe (after `stp x29, x30, [sp, #-16]!; mov
/// x29, sp`).
fn compact(o: &Object) -> Option<Tables> {
    let k = o
        .sections
        .iter()
        .position(|s| s.seg == "__LD" && s.name == "__compact_unwind")?;
    let s = &o.sections[k];
    let b = &s.data;
    let mut dirs: HashMap<String, Vec<(u32, String)>> = HashMap::default();
    for (n, e) in b.chunks(32).enumerate() {
        if e.len() != 32 || e[16..].iter().any(|&c| c != 0) {
            return None;
        }
        let off = (n * 32) as u64;
        let r = s.relocs.iter().find(|r| r.off == off)?;
        let place = i64::from_le_bytes(e[..8].try_into().ok()?);
        let name = target(o, r, place)?;
        let len = u32::from_le_bytes(e[8..12].try_into().ok()?);
        let enc = u32::from_le_bytes(e[12..16].try_into().ok()?);
        if enc & !(0x1f | 0xf00) != MODE_FRAME || len < 12 {
            return None;
        }
        let mut out = vec![(0, ".cfi_startproc".to_owned())];
        out.push((8, ".cfi_def_cfa w29, 16".to_owned()));
        out.push((8, ".cfi_offset w30, -8".to_owned()));
        out.push((8, ".cfi_offset w29, -16".to_owned()));
        let mut cur = -16;
        for (bit, a, c) in PAIRS {
            if enc & bit != 0 {
                out.push((8, format!(".cfi_offset {a}, {}", cur - 8)));
                out.push((8, format!(".cfi_offset {c}, {}", cur - 16)));
                cur -= 16;
            }
        }
        out.push((len, ".cfi_endproc".to_owned()));
        if dirs.insert(name, out).is_some() {
            return None;
        }
    }
    Some((dirs, vec![k]))
}

const X64_REGS: [&str; 16] = [
    "rax", "rcx", "rdx", "rbx", "rsp", "rbp", "rsi", "rdi", "r8", "r9", "r10", "r11", "r12", "r13",
    "r14", "r15",
];

/// COFF x64: each `.pdata` entry's `UNWIND_INFO` as `.seh_*` directives
/// at the prolog offsets its codes give.
fn seh(o: &Object) -> Option<Tables> {
    let mut dirs: HashMap<String, Vec<(u32, String)>> = HashMap::default();
    let mut hide = Vec::new();
    for (k, s) in o.sections.iter().enumerate() {
        if s.name != ".pdata" {
            continue;
        }
        hide.push(k);
        for (n, e) in s.data.chunks(12).enumerate() {
            if e.len() != 12 {
                return None;
            }
            let off = (n * 12) as u64;
            let rel = |at: u64| s.relocs.iter().find(|r| r.off == at);
            let word = |at: usize| {
                i64::from(u32::from_le_bytes(
                    e[at..at + 4].try_into().unwrap_or([0; 4]),
                ))
            };
            let name = target(o, rel(off)?, word(0))?;
            let (end_r, info_r) = (rel(off + 4)?, rel(off + 8)?);
            if end_r.sym != rel(off)?.sym {
                return None;
            }
            let len = u32::try_from(word(4) - word(0)).ok()?;
            // The `UNWIND_INFO`.
            let y = o.syms.get(info_r.sym as usize)?;
            let x = y.sect as usize - 1;
            let at = usize::try_from(if y.section_sym {
                word(8)
            } else {
                word(8) + y.value as i64
            })
            .ok()?;
            let xs = o.sections.get(x)?;
            if xs.name != ".xdata" {
                return None;
            }
            if !hide.contains(&x) {
                hide.push(x);
            }
            let u = xs.data.get(at..at + 4)?;
            if u[0] != 1 {
                return None;
            }
            let (prolog, count) = (u32::from(u[1]), usize::from(u[2]));
            let (fr, foff) = (usize::from(u[3] & 0xf), u32::from(u[3] >> 4) * 16);
            let codes = xs.data.get(at + 4..at + 4 + 2 * count)?;
            let slot = |i: usize| u32::from(u16::from_le_bytes([codes[2 * i], codes[2 * i + 1]]));
            let mut list: Vec<(u32, String)> = Vec::new();
            let mut i = 0;
            while i < count {
                let (pc, op, info) = (
                    u32::from(codes[2 * i]),
                    codes[2 * i + 1] & 0xf,
                    codes[2 * i + 1] >> 4,
                );
                let reg = X64_REGS[usize::from(info)];
                let (d, used) = match op {
                    0 => (format!(".seh_pushreg {reg}"), 1),
                    1 if info == 0 => (format!(".seh_stackalloc {}", slot(i + 1) * 8), 2),
                    1 => (
                        format!(".seh_stackalloc {}", slot(i + 1) | (slot(i + 2) << 16)),
                        3,
                    ),
                    2 => (format!(".seh_stackalloc {}", u32::from(info) * 8 + 8), 1),
                    3 => (format!(".seh_setframe {}, {foff}", X64_REGS[fr]), 1),
                    4 => (format!(".seh_savereg {reg}, {}", slot(i + 1) * 8), 2),
                    5 => (
                        format!(".seh_savereg {reg}, {}", slot(i + 1) | (slot(i + 2) << 16)),
                        3,
                    ),
                    8 => (format!(".seh_savexmm xmm{info}, {}", slot(i + 1) * 16), 2),
                    9 => (
                        format!(
                            ".seh_savexmm xmm{info}, {}",
                            slot(i + 1) | (slot(i + 2) << 16)
                        ),
                        3,
                    ),
                    _ => return None,
                };
                list.push((pc, d));
                i += used;
            }
            // The codes run backwards from the end of the prolog.
            list.reverse();
            let mut out = vec![(0, format!(".seh_proc {}", crate::asm::code::quote(&name)))];
            out.extend(list);
            out.push((prolog, ".seh_endprologue".to_owned()));
            out.push((len, ".seh_endproc".to_owned()));
            if dirs.insert(name, out).is_some() {
                return None;
            }
        }
    }
    Some((dirs, hide))
}

/// The `.ARM.exidx` entry of `arm32::PROLOGUE`.
const EXIDX: u32 = crate::isa::arm32::EXIDX;

/// 32-bit ARM: each `.ARM.exidx` entry, `arm32`'s one compact entry or
/// `EXIDX_CANTUNWIND`, as the directives an assembler makes it from.
fn exidx(o: &Object) -> Option<Tables> {
    let mut dirs: HashMap<String, Vec<(u32, String)>> = HashMap::default();
    let mut hide = Vec::new();
    for (k, s) in o.sections.iter().enumerate() {
        if s.ty != 0x7000_0001 {
            continue;
        }
        hide.push(k);
        for (n, e) in s.data.chunks(8).enumerate() {
            if e.len() != 8 {
                return None;
            }
            let off = (n * 8) as u64;
            let r = s.relocs.iter().find(|r| r.off == off && r.ty == 42)?;
            let w0 = i64::from(i32::from_le_bytes(e[..4].try_into().ok()?));
            let name = target(o, r, (w0 << 33) >> 33)?;
            let len = o
                .syms
                .iter()
                .find(|y| y.name == name)
                .map(|y| y.size as u32)?;
            let w1 = u32::from_le_bytes(e[4..].try_into().ok()?);
            let mut out = vec![(0, ".fnstart".to_owned())];
            match w1 {
                EXIDX if len >= 8 => {
                    out.push((4, ".save {r4, r5, r6, r7, r8, r9, r10, r11, lr}".to_owned()));
                    out.push((8, ".setfp r11, sp".to_owned()));
                }
                1 => out.push((0, ".cantunwind".to_owned())),
                _ => return None,
            }
            out.push((len, ".fnend".to_owned()));
            if dirs.insert(name, out).is_some() {
                return None;
            }
        }
    }
    Some((dirs, hide))
}
