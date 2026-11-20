//! z/Architecture relocations (RELA, big endian), the PLT and the GOT
//! header, as lld makes them for executables.
//!
//! `.got` starts with `_DYNAMIC` and two words for ld.so
//! (`_GLOBAL_OFFSET_TABLE_`, `DT_PLTGOT`); calls to shared objects go
//! through PLT entries and lazily bound `.got.plt` slots, PLT0 entering
//! ld.so's resolver. `*DBL` fields count halfwords.
//!
//! Thread locals: variant II, the thread pointer (`a0:a1`) at the end of
//! the block. Initial exec keeps its GOT slot; general and local dynamic
//! relax to local exec (general dynamic to a shared object's
//! thread-local: to initial exec), as lld does for executables.

use crate::elf::reloc::{Dyn, Need, Site, SymKind, Values};
use crate::symtab::Reloc;

pub const NONE: u16 = 0;
pub const R_8: u16 = 1;
pub const R_12: u16 = 2;
pub const R_16: u16 = 3;
pub const R_32: u16 = 4;
pub const PC32: u16 = 5;
pub const GOT12: u16 = 6;
pub const GOT32: u16 = 7;
pub const PLT32: u16 = 8;
pub const GLOB_DAT: u32 = 10;
pub const JMP_SLOT: u32 = 11;
pub const RELATIVE: u32 = 12;
pub const GOTOFF32: u16 = 13;
pub const GOTPC: u16 = 14;
pub const GOT16: u16 = 15;
pub const PC16: u16 = 16;
pub const PC16DBL: u16 = 17;
pub const PLT16DBL: u16 = 18;
pub const PC32DBL: u16 = 19;
pub const PLT32DBL: u16 = 20;
pub const GOTPCDBL: u16 = 21;
pub const R_64: u16 = 22;
pub const PC64: u16 = 23;
pub const GOT64: u16 = 24;
pub const PLT64: u16 = 25;
pub const GOTENT: u16 = 26;
pub const GOTOFF16: u16 = 27;
pub const GOTOFF64: u16 = 28;
pub const GOTPLT12: u16 = 29;
pub const GOTPLT16: u16 = 30;
pub const GOTPLT32: u16 = 31;
pub const GOTPLT64: u16 = 32;
pub const GOTPLTENT: u16 = 33;
pub const PLTOFF16: u16 = 34;
pub const PLTOFF32: u16 = 35;
pub const PLTOFF64: u16 = 36;
pub const TLS_LOAD: u16 = 37;
pub const TLS_GDCALL: u16 = 38;
pub const TLS_LDCALL: u16 = 39;
pub const TLS_GD32: u16 = 40;
pub const TLS_GD64: u16 = 41;
pub const TLS_GOTIE12: u16 = 42;
pub const TLS_GOTIE32: u16 = 43;
pub const TLS_GOTIE64: u16 = 44;
pub const TLS_LDM32: u16 = 45;
pub const TLS_LDM64: u16 = 46;
pub const TLS_IEENT: u16 = 49;
pub const TLS_LE32: u16 = 50;
pub const TLS_LE64: u16 = 51;
pub const TLS_LDO32: u16 = 52;
pub const TLS_LDO64: u16 = 53;
pub const TLS_DTPMOD: u32 = 54;
pub const TLS_DTPOFF: u32 = 55;
pub const TLS_TPOFF: u32 = 56;
pub const R_20: u16 = 57;
pub const GOT20: u16 = 58;
pub const GOTPLT20: u16 = 59;
pub const TLS_GOTIE20: u16 = 60;
pub const PC12DBL: u16 = 62;
pub const PLT12DBL: u16 = 63;
pub const PC24DBL: u16 = 64;
pub const PLT24DBL: u16 = 65;

/// PLT0 and each entry.
pub const PLT_HEADER: u64 = 32;
pub const PLT_ENTRY: u64 = 32;
/// `.got`'s header: `_DYNAMIC`, the link map, the resolver.
pub const GOT_HEADER: usize = 3;
/// A `.got.plt` slot first points this far into its PLT entry, at the
/// push of its relocation for PLT0.
pub const PLT_LAZY: u64 = 14;
/// `nopr %r7`, lld's (and GNU ld's) code padding: glibc's `.init`
/// pieces run through it.
pub const NOPR: u32 = 0x0707_0707;

/// A relocation that refers to a GOT or TLS slot of its symbol.
#[must_use]
pub const fn needs_symbol(ty: u16) -> bool {
    matches!(
        ty,
        GOT12
            | GOT16
            | GOT20
            | GOT32
            | GOT64
            | GOTENT
            | GOTPLT12
            | GOTPLT16
            | GOTPLT20
            | GOTPLT32
            | GOTPLT64
            | GOTPLTENT
            | TLS_GOTIE12
            | TLS_GOTIE20
            | TLS_GOTIE32
            | TLS_GOTIE64
            | TLS_IEENT
            | TLS_GD32
            | TLS_GD64
    )
}

/// The `__tls_get_offset` call of a general or local dynamic sequence
/// (the `brasl`'s `PLT32DBL` at its `TLS_GDCALL` / `TLS_LDCALL` marker),
/// which every relaxation removes.
pub fn drop_tls_get_offset(rs: &mut Vec<Reloc>) {
    let marks: Vec<u32> = rs
        .iter()
        .filter(|r| matches!(r.kind, TLS_GDCALL | TLS_LDCALL))
        .map(|r| r.off + 2)
        .collect();
    if !marks.is_empty() {
        rs.retain(|r| r.kind != PLT32DBL || !marks.contains(&r.off));
    }
}

#[must_use]
pub const fn type_name(ty: u16) -> Option<&'static str> {
    Some(match ty {
        R_8 => "R_390_8",
        R_12 => "R_390_12",
        R_16 => "R_390_16",
        R_32 => "R_390_32",
        PC32 => "R_390_PC32",
        GOT12 => "R_390_GOT12",
        GOT32 => "R_390_GOT32",
        PLT32 => "R_390_PLT32",
        GOTOFF32 => "R_390_GOTOFF",
        GOTPC => "R_390_GOTPC",
        GOT16 => "R_390_GOT16",
        PC16 => "R_390_PC16",
        PC16DBL => "R_390_PC16DBL",
        PLT16DBL => "R_390_PLT16DBL",
        PC32DBL => "R_390_PC32DBL",
        PLT32DBL => "R_390_PLT32DBL",
        GOTPCDBL => "R_390_GOTPCDBL",
        R_64 => "R_390_64",
        PC64 => "R_390_PC64",
        GOT64 => "R_390_GOT64",
        PLT64 => "R_390_PLT64",
        GOTENT => "R_390_GOTENT",
        GOTOFF16 => "R_390_GOTOFF16",
        GOTOFF64 => "R_390_GOTOFF64",
        GOTPLT12 => "R_390_GOTPLT12",
        GOTPLT16 => "R_390_GOTPLT16",
        GOTPLT32 => "R_390_GOTPLT32",
        GOTPLT64 => "R_390_GOTPLT64",
        GOTPLTENT => "R_390_GOTPLTENT",
        PLTOFF16 => "R_390_PLTOFF16",
        PLTOFF32 => "R_390_PLTOFF32",
        PLTOFF64 => "R_390_PLTOFF64",
        TLS_LOAD => "R_390_TLS_LOAD",
        TLS_GDCALL => "R_390_TLS_GDCALL",
        TLS_LDCALL => "R_390_TLS_LDCALL",
        TLS_GD32 => "R_390_TLS_GD32",
        TLS_GD64 => "R_390_TLS_GD64",
        TLS_GOTIE12 => "R_390_TLS_GOTIE12",
        TLS_GOTIE32 => "R_390_TLS_GOTIE32",
        TLS_GOTIE64 => "R_390_TLS_GOTIE64",
        TLS_LDM32 => "R_390_TLS_LDM32",
        TLS_LDM64 => "R_390_TLS_LDM64",
        TLS_IEENT => "R_390_TLS_IEENT",
        TLS_LE32 => "R_390_TLS_LE32",
        TLS_LE64 => "R_390_TLS_LE64",
        TLS_LDO32 => "R_390_TLS_LDO32",
        TLS_LDO64 => "R_390_TLS_LDO64",
        R_20 => "R_390_20",
        GOT20 => "R_390_GOT20",
        GOTPLT20 => "R_390_GOTPLT20",
        TLS_GOTIE20 => "R_390_TLS_GOTIE20",
        PC12DBL => "R_390_PC12DBL",
        PLT12DBL => "R_390_PLT12DBL",
        PC24DBL => "R_390_PC24DBL",
        PLT24DBL => "R_390_PLT24DBL",
        _ => return None,
    })
}

const fn plt_branch(ty: u16) -> bool {
    matches!(
        ty,
        PLT12DBL | PLT16DBL | PLT24DBL | PLT32DBL | PLT32 | PLT64
    )
}

const fn pc_relative(ty: u16) -> bool {
    matches!(
        ty,
        PC12DBL | PC16DBL | PC24DBL | PC32DBL | PC16 | PC32 | PC64
    )
}

/// A GOT slot by its offset from the GOT base.
const fn got_off(ty: u16) -> bool {
    matches!(
        ty,
        GOT12
            | GOT16
            | GOT20
            | GOT32
            | GOT64
            | GOTPLT12
            | GOTPLT16
            | GOTPLT20
            | GOTPLT32
            | GOTPLT64
    )
}

/// What site `s` needs.
#[allow(clippy::match_same_arms)]
pub fn need(s: Site, name: &str) -> Result<Need, String> {
    let mut n = Need::default();
    let dso = s.kind == SymKind::Dso;
    let local = s.kind == SymKind::Local;
    let pic_err = |what: &str| {
        Err(format!(
            "relocation {name} cannot be used against {what}; recompile with -fPIC"
        ))
    };
    let tls_dso = || {
        Err(format!(
            "relocation {name} against a shared object's thread-local"
        ))
    };
    match s.ty {
        R_64 => n.dynrel = s.alloc && (dso || (s.pie && local)),
        R_8 | R_12 | R_16 | R_20 | R_32 => {
            if !s.alloc {
                return Ok(n);
            }
            if dso {
                return pic_err("a shared object's symbol");
            }
            if s.pie && local {
                return pic_err("a local symbol in a PIE");
            }
        }
        _ if plt_branch(s.ty) => n.plt = dso,
        _ if pc_relative(s.ty) && dso => {
            if !s.func {
                return Err(format!(
                    "relocation {name} against data of a shared object needs a copy \
                     relocation, which Nabi's linker does not make; recompile with -fPIC"
                ));
            }
            n.plt = true;
        }
        _ if pc_relative(s.ty) => {}
        GOTENT | GOTPLTENT => n.got = true,
        _ if got_off(s.ty) => {
            n.got = true;
            n.got_base = true;
        }
        GOTPC | GOTPCDBL => n.got_base = true,
        GOTOFF16 | GOTOFF32 | GOTOFF64 if dso => return pic_err("a shared object's symbol"),
        GOTOFF16 | GOTOFF32 | GOTOFF64 => n.got_base = true,
        PLTOFF16 | PLTOFF32 | PLTOFF64 => {
            n.plt = dso;
            n.got_base = true;
        }
        TLS_IEENT => n.tlsgot = true,
        TLS_GOTIE12 | TLS_GOTIE20 | TLS_GOTIE32 | TLS_GOTIE64 => {
            n.tlsgot = true;
            n.got_base = true;
        }
        TLS_GD32 | TLS_GD64 => {
            n.tlsgot = dso;
            n.got_base = dso;
        }
        TLS_LE32 | TLS_LE64 | TLS_LDM32 | TLS_LDM64 | TLS_LDO32 | TLS_LDO64 | TLS_LDCALL if dso => {
            return tls_dso();
        }
        TLS_LE32 | TLS_LE64 | TLS_LDM32 | TLS_LDM64 | TLS_LDO32 | TLS_LDO64 | TLS_LDCALL
        | TLS_GDCALL | TLS_LOAD => {}
        _ => return Err(format!("unsupported relocation {name}")),
    }
    Ok(n)
}

fn range(name: &str, v: i64, bits: u32) -> Result<(), String> {
    if v >= -(1i64 << (bits - 1)) && v < (1i64 << (bits - 1)) {
        Ok(())
    } else {
        Err(format!(
            "relocation {name} out of range: {v:#x} does not fit {bits} bits"
        ))
    }
}

/// Signed or unsigned: what lld's `checkIntUInt` takes.
fn range_any(name: &str, v: i64, bits: u32) -> Result<(), String> {
    if v >= -(1i64 << (bits - 1)) && (v < 0 || (v as u64) < (1u64 << bits)) {
        Ok(())
    } else {
        Err(format!(
            "relocation {name} out of range: {v:#x} does not fit {bits} bits"
        ))
    }
}

fn even(name: &str, v: i64) -> Result<(), String> {
    if v & 1 == 0 {
        Ok(())
    } else {
        Err(format!(
            "relocation {name}: improper alignment: {v:#x} is odd"
        ))
    }
}

const fn r16(b: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([b[at], b[at + 1]])
}

const fn r32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn w16(b: &mut [u8], at: usize, v: u16) {
    b[at..at + 2].copy_from_slice(&v.to_be_bytes());
}

fn w32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_be_bytes());
}

fn w64(b: &mut [u8], at: usize, v: u64) {
    b[at..at + 8].copy_from_slice(&v.to_be_bytes());
}

/// The bytes a relocation of type `ty` writes.
const fn width(ty: u16) -> usize {
    match ty {
        R_8 => 1,
        R_12 | R_16 | PC16 | PC16DBL | PLT16DBL | PC12DBL | PLT12DBL | GOT12 | GOT16 | GOTPLT12
        | GOTPLT16 | GOTOFF16 | PLTOFF16 | TLS_GOTIE12 => 2,
        R_64 | PC64 | GOT64 | PLT64 | GOTOFF64 | GOTPLT64 | PLTOFF64 | TLS_GD64 | TLS_GOTIE64
        | TLS_LDM64 | TLS_LE64 | TLS_LDO64 => 8,
        TLS_GDCALL | TLS_LDCALL => 6,
        TLS_LOAD => 0,
        _ => 4,
    }
}

/// `v` into the field of a relocation of `ty`'s width and form.
fn field(name: &str, b: &mut [u8], at: usize, ty: u16, v: i64) -> Result<(), String> {
    match ty {
        R_12 | GOT12 | GOTPLT12 | TLS_GOTIE12 => {
            if !(0..0x1000).contains(&v) {
                return Err(format!(
                    "relocation {name} out of range: {v:#x} does not fit 12 bits"
                ));
            }
            w16(b, at, (r16(b, at) & 0xf000) | (v as u16 & 0xfff));
        }
        R_20 | GOT20 | GOTPLT20 | TLS_GOTIE20 => {
            range(name, v, 20)?;
            let x = v as u32;
            w32(
                b,
                at,
                (r32(b, at) & 0xf000_00ff) | ((x & 0xfff) << 16) | ((x & 0xf_f000) >> 4),
            );
        }
        _ => match width(ty) {
            1 => {
                range_any(name, v, 8)?;
                b[at] = v as u8;
            }
            2 => {
                range_any(name, v, 16)?;
                w16(b, at, v as u16);
            }
            4 => {
                range_any(name, v, 32)?;
                w32(b, at, v as u32);
            }
            _ => w64(b, at, v as u64),
        },
    }
    Ok(())
}

/// A halfword-counted PC-relative field.
fn dbl(name: &str, b: &mut [u8], at: usize, ty: u16, d: i64) -> Result<(), String> {
    even(name, d)?;
    let h = d >> 1;
    match ty {
        PC12DBL | PLT12DBL => {
            range(name, h, 12)?;
            w16(b, at, (r16(b, at) & 0xf000) | (h as u16 & 0xfff));
        }
        PC16DBL | PLT16DBL => {
            range(name, h, 16)?;
            w16(b, at, h as u16);
        }
        PC24DBL | PLT24DBL => {
            range(name, h, 24)?;
            w32(b, at, (r32(b, at) & 0xff00_0000) | (h as u32 & 0xff_ffff));
        }
        _ => {
            range(name, h, 32)?;
            w32(b, at, h as u32);
        }
    }
    Ok(())
}

/// Apply site `s` at `b[at..]` with `v` (`v.got_base`: `.got`). The
/// dynamic relocation the place needs (`dynrel`).
#[allow(clippy::too_many_lines)]
pub fn apply(
    s: Site,
    b: &mut [u8],
    at: usize,
    v: &Values,
    dynrel: Dyn,
    name: &str,
) -> Result<Dyn, String> {
    if b.len() < at + width(s.ty) {
        return Err(format!("relocation {name} runs past its section"));
    }
    let p = v.p as i64;
    let dso = s.kind == SymKind::Dso;
    // An undefined weak function: the place itself, as for a branch.
    let target = if s.kind == SymKind::UndefWeak && plt_branch(s.ty) {
        p
    } else {
        v.s as i64
    };
    let sa = target.wrapping_add(v.a);
    let pc = sa.wrapping_sub(p);
    let gb = v.got_base as i64;
    let g = (v.g as i64).wrapping_add(v.a);
    let tg = (v.tg as i64).wrapping_add(v.a);
    let tp = v.tp + v.a;
    match s.ty {
        R_64 => w64(b, at, if dso { v.a } else { sa } as u64),
        R_8 | R_12 | R_16 | R_20 | R_32 => field(name, b, at, s.ty, sa)?,
        PC12DBL | PLT12DBL | PC16DBL | PLT16DBL | PC24DBL | PLT24DBL | PC32DBL | PLT32DBL => {
            dbl(name, b, at, s.ty, pc)?;
        }
        PC16 | PC32 | PLT32 => {
            if s.ty == PC16 {
                range(name, pc, 16)?;
            }
            field(name, b, at, s.ty, pc)?;
        }
        PC64 | PLT64 => w64(b, at, pc as u64),
        GOTENT | GOTPLTENT => dbl(name, b, at, PC32DBL, g.wrapping_sub(p))?,
        _ if got_off(s.ty) => field(name, b, at, s.ty, g.wrapping_sub(gb))?,
        GOTPC => field(name, b, at, s.ty, gb.wrapping_add(v.a).wrapping_sub(p))?,
        GOTPCDBL => dbl(name, b, at, PC32DBL, gb.wrapping_add(v.a).wrapping_sub(p))?,
        GOTOFF16 | GOTOFF32 | GOTOFF64 | PLTOFF16 | PLTOFF32 | PLTOFF64 => {
            field(name, b, at, s.ty, sa.wrapping_sub(gb))?;
        }
        TLS_IEENT => dbl(name, b, at, PC32DBL, tg.wrapping_sub(p))?,
        TLS_GOTIE12 | TLS_GOTIE20 | TLS_GOTIE32 | TLS_GOTIE64 => {
            field(name, b, at, s.ty, tg.wrapping_sub(gb))?;
        }
        TLS_LE32 | TLS_LE64 => field(name, b, at, s.ty, tp)?,
        // The literal of a general dynamic sequence: the offset from
        // the thread pointer, else the TLS GOT slot's from the GOT.
        TLS_GD32 | TLS_GD64 if dso => field(name, b, at, s.ty, tg.wrapping_sub(gb))?,
        TLS_GD32 | TLS_GD64 => field(name, b, at, s.ty, tp)?,
        // Local dynamic: the module's offset is 0, each variable's its
        // offset from the thread pointer; in debug info, from the block.
        TLS_LDM32 | TLS_LDM64 => field(name, b, at, s.ty, 0)?,
        TLS_LDO32 | TLS_LDO64 if s.alloc => field(name, b, at, s.ty, tp)?,
        TLS_LDO32 | TLS_LDO64 => field(name, b, at, s.ty, v.dtp + v.a)?,
        TLS_GDCALL if dso => {
            // brasl %r14, __tls_get_offset@plt -> lg %r2, 0(%r2, %r12)
            b[at..at + 6].copy_from_slice(&[0xe3, 0x22, 0xc0, 0x00, 0x00, 0x04]);
        }
        TLS_GDCALL | TLS_LDCALL => {
            // brasl %r14, __tls_get_offset@plt -> brcl 0, .
            b[at..at + 6].copy_from_slice(&[0xc0, 0x04, 0, 0, 0, 0]);
        }
        TLS_LOAD => {}
        _ => return Err(format!("unsupported relocation {name}")),
    }
    Ok(dynrel)
}

/// PLT0 at `at` (address `plt`): it saves the entry's relocation offset,
/// passes the link map (`.got[1]`) and enters the resolver (`.got[2]`).
pub fn write_plt0(b: &mut [u8], at: usize, plt: u64, got: u64) {
    b[at..at + 32].copy_from_slice(&[
        0xe3, 0x10, 0xf0, 0x38, 0x00, 0x24, // stg %r1, 56(%r15)
        0xc0, 0x10, 0, 0, 0, 0, // larl %r1, _GLOBAL_OFFSET_TABLE_
        0xd2, 0x07, 0xf0, 0x30, 0x10, 0x08, // mvc 48(8, %r15), 8(%r1)
        0xe3, 0x10, 0x10, 0x10, 0x00, 0x04, // lg %r1, 16(%r1)
        0x07, 0xf1, // br %r1
        0x07, 0x00, 0x07, 0x00, 0x07, 0x00, // nopr
    ]);
    w32(b, at + 8, (got.wrapping_sub(plt + 6) >> 1) as u32);
}

/// PLT entry at `at` (address `addr`): a jump through `.got.plt` slot
/// `slot`, which first points back at its `basr`, the push of `.rela.plt`
/// offset `rel` for PLT0 at `plt`.
pub fn write_plt(b: &mut [u8], at: usize, addr: u64, slot: u64, plt: u64, rel: u32) {
    b[at..at + 32].copy_from_slice(&[
        0xc0, 0x10, 0, 0, 0, 0, // larl %r1, slot
        0xe3, 0x10, 0x10, 0x00, 0x00, 0x04, // lg %r1, 0(%r1)
        0x07, 0xf1, // br %r1
        0x0d, 0x10, // basr %r1, %r0
        0xe3, 0x10, 0x10, 0x0c, 0x00, 0x14, // lgf %r1, 12(%r1)
        0xc0, 0xf4, 0, 0, 0, 0, // jg plt0
        0, 0, 0, 0, // the relocation's offset
    ]);
    w32(b, at + 2, (slot.wrapping_sub(addr) >> 1) as u32);
    w32(b, at + 24, (plt.wrapping_sub(addr + 22) >> 1) as u32);
    w32(b, at + 28, rel);
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn site(ty: u16, kind: SymKind) -> Site {
        Site {
            arch: crate::elf::Arch::S390x,
            ty,
            kind,
            func: true,
            alloc: true,
            pie: true,
            dyn_tls: false,
        }
    }

    #[test]
    fn halfword_relative_fields() {
        // brasl %r14, f
        let mut b = vec![0xc0, 0xe5, 0, 0, 0, 0];
        let v = Values {
            s: 0x1000,
            a: 2,
            p: 0x2002,
            ..Values::default()
        };
        apply(site(PLT32DBL, SymKind::Local), &mut b, 2, &v, Dyn::None, "").unwrap();
        assert_eq!(b, [0xc0, 0xe5, 0xff, 0xff, 0xf8, 0x00]);
        let v = Values {
            s: 0x1001,
            p: 0x2002,
            ..Values::default()
        };
        assert!(apply(site(PC32DBL, SymKind::Local), &mut b, 2, &v, Dyn::None, "").is_err());
    }

    #[test]
    fn twenty_bit_displacements_split() {
        // lg %r1, 0(%r1): DL in the low 12 bits of bytes 2-3, DH in byte 4.
        let mut b = vec![0xe3, 0x10, 0x10, 0x00, 0x00, 0x04];
        let v = Values {
            s: 0x1_2345,
            ..Values::default()
        };
        apply(site(R_20, SymKind::Abs), &mut b, 2, &v, Dyn::None, "").unwrap();
        assert_eq!(b, [0xe3, 0x10, 0x13, 0x45, 0x12, 0x04]);
    }

    #[test]
    fn general_dynamic_call_relaxes() {
        let mut b = vec![0xc0, 0xe5, 0, 0, 0, 0];
        apply(
            site(TLS_GDCALL, SymKind::Local),
            &mut b,
            0,
            &Values::default(),
            Dyn::None,
            "",
        )
        .unwrap();
        assert_eq!(b, [0xc0, 0x04, 0, 0, 0, 0]);
    }
}
