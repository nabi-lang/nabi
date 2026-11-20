//! 64-bit little-endian POWER (`ELFv2`) relocations (RELA) and the PLT call
//! stubs, as lld makes them for executables.
//!
//! The TOC base `.TOC.` is `.got` + 0x8000; `.got[0]` holds it. Calls to
//! shared objects go through a stub that saves r2 and jumps through the
//! function's GOT slot (`GLOB_DAT`, bound at load: no lazy binding); the
//! caller's `nop` after the `bl` reloads r2. Local calls enter at the
//! local entry point (`st_other`), past the TOC setup; calls a `bl`
//! cannot reach go through long-branch stubs between `.text` input
//! sections.
//!
//! Thread locals: variant I, r13 is 0x7000 past the start of the block
//! and DTV entries 0x8000 past it. Initial exec keeps its GOT slot;
//! general and local dynamic relax to local exec (general dynamic to a
//! shared object's thread-local: to initial exec), as lld does for
//! executables.

use crate::elf::reloc::{Dyn, Need, Site, SymKind, Values};
use crate::symtab::Reloc;

pub const NONE: u16 = 0;
pub const ADDR32: u16 = 1;
pub const ADDR24: u16 = 2;
pub const ADDR16: u16 = 3;
pub const ADDR16_LO: u16 = 4;
pub const ADDR16_HI: u16 = 5;
pub const ADDR16_HA: u16 = 6;
pub const ADDR14: u16 = 7;
pub const REL24: u16 = 10;
pub const REL14: u16 = 11;
pub const GOT16: u16 = 14;
pub const GOT16_LO: u16 = 15;
pub const GOT16_HI: u16 = 16;
pub const GOT16_HA: u16 = 17;
pub const GLOB_DAT: u32 = 20;
pub const RELATIVE: u32 = 22;
pub const REL32: u16 = 26;
pub const ADDR64: u16 = 38;
pub const ADDR16_HIGHER: u16 = 39;
pub const ADDR16_HIGHERA: u16 = 40;
pub const ADDR16_HIGHEST: u16 = 41;
pub const ADDR16_HIGHESTA: u16 = 42;
pub const REL64: u16 = 44;
pub const TOC16: u16 = 47;
pub const TOC16_LO: u16 = 48;
pub const TOC16_HI: u16 = 49;
pub const TOC16_HA: u16 = 50;
pub const TOC: u16 = 51;
pub const ADDR16_DS: u16 = 56;
pub const ADDR16_LO_DS: u16 = 57;
pub const GOT16_DS: u16 = 58;
pub const GOT16_LO_DS: u16 = 59;
pub const TOC16_DS: u16 = 63;
pub const TOC16_LO_DS: u16 = 64;
pub const TLS: u16 = 67;
pub const DTPMOD64: u32 = 68;
pub const TPREL16: u16 = 69;
pub const TPREL16_LO: u16 = 70;
pub const TPREL16_HI: u16 = 71;
pub const TPREL16_HA: u16 = 72;
pub const TPREL64: u16 = 73;
pub const DTPREL16: u16 = 74;
pub const DTPREL16_LO: u16 = 75;
pub const DTPREL16_HI: u16 = 76;
pub const DTPREL16_HA: u16 = 77;
pub const DTPREL64: u16 = 78;
pub const GOT_TLSGD16: u16 = 79;
pub const GOT_TLSGD16_LO: u16 = 80;
pub const GOT_TLSGD16_HI: u16 = 81;
pub const GOT_TLSGD16_HA: u16 = 82;
pub const GOT_TLSLD16: u16 = 83;
pub const GOT_TLSLD16_LO: u16 = 84;
pub const GOT_TLSLD16_HI: u16 = 85;
pub const GOT_TLSLD16_HA: u16 = 86;
pub const GOT_TPREL16_DS: u16 = 87;
pub const GOT_TPREL16_LO_DS: u16 = 88;
pub const GOT_TPREL16_HI: u16 = 89;
pub const GOT_TPREL16_HA: u16 = 90;
pub const GOT_DTPREL16_DS: u16 = 91;
pub const GOT_DTPREL16_LO_DS: u16 = 92;
pub const GOT_DTPREL16_HI: u16 = 93;
pub const GOT_DTPREL16_HA: u16 = 94;
pub const TPREL16_DS: u16 = 95;
pub const TPREL16_LO_DS: u16 = 96;
pub const TPREL16_HIGHER: u16 = 97;
pub const TPREL16_HIGHERA: u16 = 98;
pub const TPREL16_HIGHEST: u16 = 99;
pub const TPREL16_HIGHESTA: u16 = 100;
pub const DTPREL16_DS: u16 = 101;
pub const DTPREL16_LO_DS: u16 = 102;
pub const DTPREL16_HIGHER: u16 = 103;
pub const DTPREL16_HIGHERA: u16 = 104;
pub const DTPREL16_HIGHEST: u16 = 105;
pub const DTPREL16_HIGHESTA: u16 = 106;
pub const TLSGD: u16 = 107;
pub const TLSLD: u16 = 108;
pub const ADDR16_HIGH: u16 = 110;
pub const ADDR16_HIGHA: u16 = 111;
pub const TPREL16_HIGH: u16 = 112;
pub const TPREL16_HIGHA: u16 = 113;
pub const DTPREL16_HIGH: u16 = 114;
pub const DTPREL16_HIGHA: u16 = 115;
pub const REL16: u16 = 249;
pub const REL16_LO: u16 = 250;
pub const REL16_HI: u16 = 251;
pub const REL16_HA: u16 = 252;

/// `EF_PPC64_ABI`: 2 is `ELFv2`.
pub const EF_ABI: u32 = 3;
pub const EF_ELFV2: u32 = 2;
/// r13 is this far past the start of the TLS block.
pub const TP_OFFSET: i64 = 0x7000;
/// glibc's `TLS_DTV_OFFSET`: DTV entries point this far into a block.
pub const DTV_OFFSET: i64 = 0x8000;
/// `.TOC.` is this far past the start of `.got`.
pub const TOC_BIAS: u64 = 0x8000;
/// A PLT call stub: five instructions.
pub const PLT_ENTRY: u64 = 20;

pub const NOP: u32 = 0x6000_0000;
/// `ld r2, 24(r1)`: the TOC back after a call through a stub.
const LD_R2: u32 = 0xe841_0018;

/// The local entry point's distance from the global one, by `st_other`.
#[must_use]
pub const fn local_entry(other: u8) -> u64 {
    match other >> 5 {
        v @ 2..=6 => 1 << v,
        _ => 0,
    }
}

/// `e_flags` check of an input: only `ELFv2` (or unmarked) objects.
pub fn check_flags(f: u32) -> Result<(), String> {
    if f & EF_ABI == 1 {
        return Err("ABI version 1 (`ELFv1`) is not supported".to_owned());
    }
    Ok(())
}

/// A relocation that refers to a GOT or TLS slot of its symbol.
#[must_use]
pub const fn needs_symbol(ty: u16) -> bool {
    matches!(
        ty,
        GOT16
            | GOT16_LO
            | GOT16_HI
            | GOT16_HA
            | GOT16_DS
            | GOT16_LO_DS
            | GOT_TPREL16_DS
            | GOT_TPREL16_LO_DS
            | GOT_TPREL16_HI
            | GOT_TPREL16_HA
            | GOT_TLSGD16
            | GOT_TLSGD16_LO
            | GOT_TLSGD16_HI
            | GOT_TLSGD16_HA
    )
}

/// The `__tls_get_addr` call of a general or local dynamic sequence (a
/// `REL24` at its `TLSGD` / `TLSLD` marker), which every relaxation
/// removes.
pub fn drop_tls_get_addr(rs: &mut Vec<Reloc>) {
    let marks: Vec<u32> = rs
        .iter()
        .filter(|r| matches!(r.kind, TLSGD | TLSLD))
        .map(|r| r.off)
        .collect();
    if !marks.is_empty() {
        rs.retain(|r| r.kind != REL24 || !marks.contains(&r.off));
    }
}

#[must_use]
pub const fn type_name(ty: u16) -> Option<&'static str> {
    Some(match ty {
        ADDR32 => "R_PPC64_ADDR32",
        ADDR24 => "R_PPC64_ADDR24",
        ADDR16 => "R_PPC64_ADDR16",
        ADDR16_LO => "R_PPC64_ADDR16_LO",
        ADDR16_HI => "R_PPC64_ADDR16_HI",
        ADDR16_HA => "R_PPC64_ADDR16_HA",
        ADDR14 => "R_PPC64_ADDR14",
        REL24 => "R_PPC64_REL24",
        REL14 => "R_PPC64_REL14",
        GOT16 => "R_PPC64_GOT16",
        GOT16_LO => "R_PPC64_GOT16_LO",
        GOT16_HI => "R_PPC64_GOT16_HI",
        GOT16_HA => "R_PPC64_GOT16_HA",
        REL32 => "R_PPC64_REL32",
        ADDR64 => "R_PPC64_ADDR64",
        ADDR16_HIGHER => "R_PPC64_ADDR16_HIGHER",
        ADDR16_HIGHERA => "R_PPC64_ADDR16_HIGHERA",
        ADDR16_HIGHEST => "R_PPC64_ADDR16_HIGHEST",
        ADDR16_HIGHESTA => "R_PPC64_ADDR16_HIGHESTA",
        REL64 => "R_PPC64_REL64",
        TOC16 => "R_PPC64_TOC16",
        TOC16_LO => "R_PPC64_TOC16_LO",
        TOC16_HI => "R_PPC64_TOC16_HI",
        TOC16_HA => "R_PPC64_TOC16_HA",
        TOC => "R_PPC64_TOC",
        ADDR16_DS => "R_PPC64_ADDR16_DS",
        ADDR16_LO_DS => "R_PPC64_ADDR16_LO_DS",
        GOT16_DS => "R_PPC64_GOT16_DS",
        GOT16_LO_DS => "R_PPC64_GOT16_LO_DS",
        TOC16_DS => "R_PPC64_TOC16_DS",
        TOC16_LO_DS => "R_PPC64_TOC16_LO_DS",
        TLS => "R_PPC64_TLS",
        TPREL16 => "R_PPC64_TPREL16",
        TPREL16_LO => "R_PPC64_TPREL16_LO",
        TPREL16_HI => "R_PPC64_TPREL16_HI",
        TPREL16_HA => "R_PPC64_TPREL16_HA",
        TPREL64 => "R_PPC64_TPREL64",
        DTPREL16 => "R_PPC64_DTPREL16",
        DTPREL16_LO => "R_PPC64_DTPREL16_LO",
        DTPREL16_HI => "R_PPC64_DTPREL16_HI",
        DTPREL16_HA => "R_PPC64_DTPREL16_HA",
        DTPREL64 => "R_PPC64_DTPREL64",
        GOT_TLSGD16 => "R_PPC64_GOT_TLSGD16",
        GOT_TLSGD16_LO => "R_PPC64_GOT_TLSGD16_LO",
        GOT_TLSGD16_HI => "R_PPC64_GOT_TLSGD16_HI",
        GOT_TLSGD16_HA => "R_PPC64_GOT_TLSGD16_HA",
        GOT_TLSLD16 => "R_PPC64_GOT_TLSLD16",
        GOT_TLSLD16_LO => "R_PPC64_GOT_TLSLD16_LO",
        GOT_TLSLD16_HI => "R_PPC64_GOT_TLSLD16_HI",
        GOT_TLSLD16_HA => "R_PPC64_GOT_TLSLD16_HA",
        GOT_TPREL16_DS => "R_PPC64_GOT_TPREL16_DS",
        GOT_TPREL16_LO_DS => "R_PPC64_GOT_TPREL16_LO_DS",
        GOT_TPREL16_HI => "R_PPC64_GOT_TPREL16_HI",
        GOT_TPREL16_HA => "R_PPC64_GOT_TPREL16_HA",
        GOT_DTPREL16_DS => "R_PPC64_GOT_DTPREL16_DS",
        GOT_DTPREL16_LO_DS => "R_PPC64_GOT_DTPREL16_LO_DS",
        GOT_DTPREL16_HI => "R_PPC64_GOT_DTPREL16_HI",
        GOT_DTPREL16_HA => "R_PPC64_GOT_DTPREL16_HA",
        TPREL16_DS => "R_PPC64_TPREL16_DS",
        TPREL16_LO_DS => "R_PPC64_TPREL16_LO_DS",
        TPREL16_HIGHER => "R_PPC64_TPREL16_HIGHER",
        TPREL16_HIGHERA => "R_PPC64_TPREL16_HIGHERA",
        TPREL16_HIGHEST => "R_PPC64_TPREL16_HIGHEST",
        TPREL16_HIGHESTA => "R_PPC64_TPREL16_HIGHESTA",
        DTPREL16_DS => "R_PPC64_DTPREL16_DS",
        DTPREL16_LO_DS => "R_PPC64_DTPREL16_LO_DS",
        DTPREL16_HIGHER => "R_PPC64_DTPREL16_HIGHER",
        DTPREL16_HIGHERA => "R_PPC64_DTPREL16_HIGHERA",
        DTPREL16_HIGHEST => "R_PPC64_DTPREL16_HIGHEST",
        DTPREL16_HIGHESTA => "R_PPC64_DTPREL16_HIGHESTA",
        TLSGD => "R_PPC64_TLSGD",
        TLSLD => "R_PPC64_TLSLD",
        ADDR16_HIGH => "R_PPC64_ADDR16_HIGH",
        ADDR16_HIGHA => "R_PPC64_ADDR16_HIGHA",
        TPREL16_HIGH => "R_PPC64_TPREL16_HIGH",
        TPREL16_HIGHA => "R_PPC64_TPREL16_HIGHA",
        DTPREL16_HIGH => "R_PPC64_DTPREL16_HIGH",
        DTPREL16_HIGHA => "R_PPC64_DTPREL16_HIGHA",
        REL16 => "R_PPC64_REL16",
        REL16_LO => "R_PPC64_REL16_LO",
        REL16_HI => "R_PPC64_REL16_HI",
        REL16_HA => "R_PPC64_REL16_HA",
        _ => return None,
    })
}

/// The absolute forms: a PIE cannot have them in code.
const fn absolute(ty: u16) -> bool {
    matches!(
        ty,
        ADDR32
            | ADDR24
            | ADDR16
            | ADDR16_LO
            | ADDR16_HI
            | ADDR16_HA
            | ADDR14
            | ADDR16_HIGHER
            | ADDR16_HIGHERA
            | ADDR16_HIGHEST
            | ADDR16_HIGHESTA
            | ADDR16_DS
            | ADDR16_LO_DS
            | ADDR16_HIGH
            | ADDR16_HIGHA
    )
}

const fn pc_relative(ty: u16) -> bool {
    matches!(
        ty,
        REL32 | REL64 | REL16 | REL16_LO | REL16_HI | REL16_HA | REL14
    )
}

const fn toc_relative(ty: u16) -> bool {
    matches!(
        ty,
        TOC16 | TOC16_LO | TOC16_HI | TOC16_HA | TOC16_DS | TOC16_LO_DS
    )
}

const fn got(ty: u16) -> bool {
    matches!(
        ty,
        GOT16 | GOT16_LO | GOT16_HI | GOT16_HA | GOT16_DS | GOT16_LO_DS
    )
}

const fn got_tprel(ty: u16) -> bool {
    matches!(
        ty,
        GOT_TPREL16_DS | GOT_TPREL16_LO_DS | GOT_TPREL16_HI | GOT_TPREL16_HA
    )
}

const fn tprel(ty: u16) -> bool {
    matches!(
        ty,
        TPREL16
            | TPREL16_LO
            | TPREL16_HI
            | TPREL16_HA
            | TPREL16_DS
            | TPREL16_LO_DS
            | TPREL16_HIGH
            | TPREL16_HIGHA
            | TPREL16_HIGHER
            | TPREL16_HIGHERA
            | TPREL16_HIGHEST
            | TPREL16_HIGHESTA
            | TPREL64
    )
}

const fn dtprel(ty: u16) -> bool {
    matches!(
        ty,
        DTPREL16
            | DTPREL16_LO
            | DTPREL16_HI
            | DTPREL16_HA
            | DTPREL16_DS
            | DTPREL16_LO_DS
            | DTPREL16_HIGH
            | DTPREL16_HIGHA
            | DTPREL16_HIGHER
            | DTPREL16_HIGHERA
            | DTPREL16_HIGHEST
            | DTPREL16_HIGHESTA
            | DTPREL64
    )
}

const fn tls_gd(ty: u16) -> bool {
    matches!(
        ty,
        GOT_TLSGD16 | GOT_TLSGD16_LO | GOT_TLSGD16_HI | GOT_TLSGD16_HA | TLSGD
    )
}

const fn tls_ld(ty: u16) -> bool {
    matches!(
        ty,
        GOT_TLSLD16 | GOT_TLSLD16_LO | GOT_TLSLD16_HI | GOT_TLSLD16_HA | TLSLD
    )
}

/// What site `s` needs.
pub fn need(s: Site, name: &str) -> Result<Need, String> {
    let mut n = Need::default();
    let dso = s.kind == SymKind::Dso;
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
        ADDR64 => n.dynrel = s.alloc && (dso || (s.pie && s.kind == SymKind::Local)),
        _ if absolute(s.ty) => {
            if !s.alloc {
                return Ok(n);
            }
            if dso {
                return pic_err("a shared object's symbol");
            }
            if s.pie && s.kind == SymKind::Local {
                return pic_err("a local symbol in a PIE");
            }
        }
        REL24 if dso => n.plt = true,
        REL14 if dso => return pic_err("a shared object's function (a 16-bit branch)"),
        REL24 => {}
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
        _ if toc_relative(s.ty) => {
            if dso {
                return pic_err("a shared object's symbol");
            }
            n.got_base = true;
        }
        TOC => {
            n.got_base = true;
            n.dynrel = s.alloc && s.pie;
        }
        _ if got(s.ty) => {
            n.got = true;
            n.got_base = true;
        }
        _ if got_tprel(s.ty) => {
            n.tlsgot = true;
            n.got_base = true;
        }
        TLS => {}
        _ if tls_gd(s.ty) => {
            n.tlsgot = dso;
            n.got_base = dso;
        }
        _ if (tls_ld(s.ty) || tprel(s.ty)) && dso => return tls_dso(),
        _ if tls_ld(s.ty) || tprel(s.ty) || dtprel(s.ty) => {}
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

fn align(name: &str, v: i64, n: i64) -> Result<(), String> {
    if v % n == 0 {
        Ok(())
    } else {
        Err(format!(
            "relocation {name}: improper alignment: {v:#x} is not a multiple of {n}"
        ))
    }
}

const fn r16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

const fn r32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn w16(b: &mut [u8], at: usize, v: u16) {
    b[at..at + 2].copy_from_slice(&v.to_le_bytes());
}

fn w32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

fn w64(b: &mut [u8], at: usize, v: u64) {
    b[at..at + 8].copy_from_slice(&v.to_le_bytes());
}

const fn lo(v: i64) -> u16 {
    v as u16
}

const fn hi(v: i64) -> u16 {
    (v >> 16) as u16
}

const fn ha(v: i64) -> u16 {
    ((v + 0x8000) >> 16) as u16
}

/// The 16-bit field of a `_HA` / `_HI` / `_LO` / `_DS` relocation (of
/// `ty`'s suffix) for `v`, checked as lld checks it.
#[allow(clippy::match_same_arms)]
fn half(name: &str, b: &mut [u8], at: usize, ty: u16, v: i64) -> Result<(), String> {
    match ty {
        ADDR16_LO | TOC16_LO | GOT16_LO | REL16_LO | TPREL16_LO | DTPREL16_LO | GOT_TLSGD16_LO
        | GOT_TLSLD16_LO => w16(b, at, lo(v)),
        ADDR16_HI | TOC16_HI | GOT16_HI | REL16_HI | TPREL16_HI | DTPREL16_HI | GOT_TPREL16_HI
        | GOT_DTPREL16_HI | GOT_TLSGD16_HI | GOT_TLSLD16_HI => {
            range(name, v, 32)?;
            w16(b, at, hi(v));
        }
        ADDR16_HIGH | TPREL16_HIGH | DTPREL16_HIGH => w16(b, at, hi(v)),
        ADDR16_HA | TOC16_HA | GOT16_HA | REL16_HA | TPREL16_HA | DTPREL16_HA | GOT_TPREL16_HA
        | GOT_DTPREL16_HA | GOT_TLSGD16_HA | GOT_TLSLD16_HA => {
            range(name, v + 0x8000, 32)?;
            w16(b, at, ha(v));
        }
        ADDR16_HIGHA | TPREL16_HIGHA | DTPREL16_HIGHA => w16(b, at, ha(v)),
        ADDR16_HIGHER | TPREL16_HIGHER | DTPREL16_HIGHER => w16(b, at, (v >> 32) as u16),
        ADDR16_HIGHERA | TPREL16_HIGHERA | DTPREL16_HIGHERA => {
            w16(b, at, ((v + 0x8000) >> 32) as u16);
        }
        ADDR16_HIGHEST | TPREL16_HIGHEST | DTPREL16_HIGHEST => w16(b, at, (v >> 48) as u16),
        ADDR16_HIGHESTA | TPREL16_HIGHESTA | DTPREL16_HIGHESTA => {
            w16(b, at, ((v + 0x8000) >> 48) as u16);
        }
        ADDR16_LO_DS | TOC16_LO_DS | GOT16_LO_DS | TPREL16_LO_DS | DTPREL16_LO_DS
        | GOT_TPREL16_LO_DS | GOT_DTPREL16_LO_DS => {
            align(name, v, 4)?;
            w16(b, at, (r16(b, at) & 3) | (lo(v) & 0xfffc));
        }
        ADDR16_DS | TOC16_DS | GOT16_DS | TPREL16_DS | DTPREL16_DS | GOT_TPREL16_DS
        | GOT_DTPREL16_DS => {
            range(name, v, 16)?;
            align(name, v, 4)?;
            w16(b, at, (r16(b, at) & 3) | (lo(v) & 0xfffc));
        }
        _ => {
            range(name, v, 16)?;
            w16(b, at, lo(v));
        }
    }
    Ok(())
}

/// The instruction word holding the 16-bit field at `at`.
const fn insn_at(at: usize) -> usize {
    at & !3
}

/// Apply site `s` at `b[at..]` with `v` (`v.got_base`: the TOC base;
/// a local call's target: its local entry point). The dynamic relocation
/// the place needs (`dynrel`).
#[allow(clippy::too_many_lines)]
pub fn apply(
    s: Site,
    b: &mut [u8],
    at: usize,
    v: &Values,
    dynrel: Dyn,
    name: &str,
) -> Result<Dyn, String> {
    let width = match s.ty {
        ADDR64 | REL64 | TOC | TPREL64 | DTPREL64 | TLSGD | TLSLD => 8,
        REL24 | REL14 | ADDR32 | ADDR24 | ADDR14 | REL32 => 4,
        TLS => 0,
        _ => 2,
    };
    if b.len() < at + width {
        return Err(format!("relocation {name} runs past its section"));
    }
    let p = v.p as i64;
    let undef = s.kind == SymKind::UndefWeak;
    let sa = (v.s as i64).wrapping_add(v.a);
    let pc = sa.wrapping_sub(p);
    let toc = v.got_base as i64;
    let tp = v.tp + v.a;
    let dtp = v.dtp + v.a - DTV_OFFSET;
    let dso = s.kind == SymKind::Dso;
    match s.ty {
        ADDR64 => {
            let val = if dso { v.a } else { sa };
            w64(b, at, val as u64);
        }
        ADDR32 => {
            if s.alloc {
                range(name, sa, 33)?;
            }
            w32(b, at, sa as u32);
        }
        ADDR24 => {
            range(name, sa, 26)?;
            w32(
                b,
                at,
                (r32(b, at) & !0x03ff_fffc) | (sa as u32 & 0x03ff_fffc),
            );
        }
        ADDR14 => {
            range(name, sa, 16)?;
            w32(b, at, (r32(b, at) & !0xfffc) | (sa as u32 & 0xfffc));
        }
        REL24 => {
            // An undefined weak function: the place itself.
            let d = if undef { 0 } else { pc };
            range(name, d, 26)?;
            align(name, d, 4)?;
            let i = r32(b, at);
            w32(b, at, (i & !0x03ff_fffc) | (d as u32 & 0x03ff_fffc));
            if dso && i & 1 == 1 {
                // `bl` through a stub: the TOC comes back in the `nop`.
                if b.len() < at + 8 || r32(b, at + 4) != NOP {
                    return Err(format!(
                        "{name}: a call to a shared object's function lacks the nop \
                         after it, so the TOC cannot be restored; recompile with -fPIC"
                    ));
                }
                w32(b, at + 4, LD_R2);
            }
        }
        REL14 => {
            let d = if undef { 0 } else { pc };
            range(name, d, 16)?;
            align(name, d, 4)?;
            w32(b, at, (r32(b, at) & !0xfffc) | (d as u32 & 0xfffc));
        }
        REL32 => {
            range(name, pc, 32)?;
            w32(b, at, pc as u32);
        }
        REL64 => w64(b, at, pc as u64),
        TOC => {
            w64(b, at, toc as u64);
            if dynrel != Dyn::None {
                return Ok(Dyn::Relative(toc as u64));
            }
        }
        TLS => {}
        REL16 | REL16_LO | REL16_HI | REL16_HA => half(name, b, at, s.ty, pc)?,
        _ if absolute(s.ty) => half(name, b, at, s.ty, sa)?,
        _ if toc_relative(s.ty) => half(name, b, at, s.ty, sa.wrapping_sub(toc))?,
        _ if got(s.ty) => {
            let d = (v.g as i64).wrapping_add(v.a).wrapping_sub(toc);
            half(name, b, at, s.ty, d)?;
        }
        _ if got_tprel(s.ty) => {
            let d = (v.tg as i64).wrapping_add(v.a).wrapping_sub(toc);
            half(name, b, at, s.ty, d)?;
        }
        TPREL64 => w64(b, at, tp as u64),
        DTPREL64 => w64(b, at, dtp as u64),
        _ if tprel(s.ty) => half(name, b, at, s.ty, tp)?,
        _ if dtprel(s.ty) => half(name, b, at, s.ty, dtp)?,
        GOT_TLSGD16_HA | GOT_TLSGD16_LO | GOT_TLSGD16 | TLSGD if dso => {
            gd_to_ie(name, b, at, s.ty, (v.tg as i64).wrapping_sub(toc))?;
        }
        GOT_TLSGD16_HA | GOT_TLSGD16_LO | GOT_TLSGD16 | TLSGD => {
            gd_to_le(name, b, at, s.ty, tp)?;
        }
        GOT_TLSLD16_HA | GOT_TLSLD16_LO | GOT_TLSLD16 | TLSLD => ld_to_le(b, at, s.ty),
        _ => return Err(format!("unsupported relocation {name}")),
    }
    Ok(dynrel)
}

/// `addis r3, r2, x@got@tlsgd@ha; addi r3, r3, x@got@tlsgd@l;
/// bl __tls_get_addr(x@tlsgd); nop` to `nop; addis r3, r13, x@tprel@ha;
/// nop; addi r3, r3, x@tprel@l`.
fn gd_to_le(name: &str, b: &mut [u8], at: usize, ty: u16, tp: i64) -> Result<(), String> {
    match ty {
        GOT_TLSGD16_HA => w32(b, insn_at(at), NOP),
        TLSGD => {
            w32(b, at, NOP);
            w32(b, at + 4, 0x3863_0000);
            half(name, b, at + 4, TPREL16_LO, tp)?;
        }
        _ => {
            w32(b, insn_at(at), 0x3c6d_0000);
            half(name, b, at, TPREL16_HA, tp)?;
        }
    }
    Ok(())
}

/// The same sequence to `addis r3, r2, x@got@tprel@ha; ld r3,
/// x@got@tprel@l(r3); nop; add r3, r3, r13`; `d`: the TLS GOT slot less
/// the TOC base.
fn gd_to_ie(name: &str, b: &mut [u8], at: usize, ty: u16, d: i64) -> Result<(), String> {
    match ty {
        GOT_TLSGD16_HA => half(name, b, at, GOT_TPREL16_HA, d)?,
        TLSGD => {
            w32(b, at, NOP);
            w32(b, at + 4, 0x7c63_6a14);
        }
        _ => {
            let ra = r32(b, insn_at(at)) & (0x1f << 16);
            w32(b, insn_at(at), 0xe860_0000 | ra);
            half(name, b, at, GOT_TPREL16_LO_DS, d)?;
        }
    }
    Ok(())
}

/// `addis r3, r2, x@got@tlsld@ha; addi r3, r3, x@got@tlsld@l;
/// bl __tls_get_addr(x@tlsld); nop` to `nop; addis r3, r13, 0; nop;
/// addi r3, r3, 4096`: r3 is the block's DTV pointer, 0x8000 past it.
fn ld_to_le(b: &mut [u8], at: usize, ty: u16) {
    match ty {
        GOT_TLSLD16_HA => w32(b, insn_at(at), NOP),
        TLSLD => {
            w32(b, at, NOP);
            w32(b, at + 4, 0x3863_1000);
        }
        _ => w32(b, insn_at(at), 0x3c6d_0000),
    }
}

/// PLT call stub at `at` (address `addr`) through GOT slot `slot`:
/// `std r2, 24(r1); addis r12, r2, slot@toc@ha; ld r12, slot@toc@l(r12);
/// mtctr r12; bctr`, as lld's.
pub fn write_plt(b: &mut [u8], at: usize, slot: u64, toc: u64) {
    let d = slot.wrapping_sub(toc) as i64;
    w32(b, at, 0xf841_0018);
    w32(b, at + 4, 0x3d82_0000 | u32::from(ha(d)));
    w32(b, at + 8, 0xe98c_0000 | u32::from(lo(d) & 0xfffc));
    w32(b, at + 12, 0x7d89_03a6);
    w32(b, at + 16, 0x4e80_0420);
}

/// A long-branch stub: four instructions.
pub const LONG_BRANCH: u64 = 16;

/// Long-branch stub at `at` to `to`: `addis r12, r2, to@toc@ha; addi r12,
/// r12, to@toc@l; mtctr r12; bctr`. r2 is already the TOC: a local target
/// is entered past its TOC setup.
pub fn write_long_branch(b: &mut [u8], at: usize, to: u64, toc: u64) {
    let d = to.wrapping_sub(toc) as i64;
    w32(b, at, 0x3d82_0000 | u32::from(ha(d)));
    w32(b, at + 4, 0x398c_0000 | u32::from(lo(d)));
    w32(b, at + 8, 0x7d89_03a6);
    w32(b, at + 12, 0x4e80_0420);
}

/// Fill code padding `b` with `nop`s.
pub fn fill_nops(b: &mut [u8]) {
    for w in b.chunks_exact_mut(4) {
        w.copy_from_slice(&NOP.to_le_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn site(ty: u16, kind: SymKind) -> Site {
        Site {
            arch: crate::elf::Arch::Ppc64,
            ty,
            kind,
            func: true,
            alloc: true,
            pie: true,
            dyn_tls: false,
        }
    }

    #[test]
    fn local_entry_offsets() {
        assert_eq!(local_entry(0), 0);
        assert_eq!(local_entry(0x20), 0);
        assert_eq!(local_entry(0x60), 8);
        assert_eq!(local_entry(0x80 | 2), 16);
    }

    #[test]
    fn toc_setup_and_toc_relative_loads() {
        // addis 2, 12, 0; addi 2, 2, 0 at 0x10000; .TOC. at 0x28000
        let mut b = Vec::new();
        b.extend_from_slice(&0x3c4c_0000u32.to_le_bytes());
        b.extend_from_slice(&0x3842_0000u32.to_le_bytes());
        let v = |p: u64, a: i64| Values {
            s: 0x2_8000,
            p,
            a,
            ..Values::default()
        };
        apply(
            site(REL16_HA, SymKind::Local),
            &mut b,
            0,
            &v(0x1_0000, 0),
            Dyn::None,
            "",
        )
        .unwrap();
        apply(
            site(REL16_LO, SymKind::Local),
            &mut b,
            4,
            &v(0x1_0004, 4),
            Dyn::None,
            "",
        )
        .unwrap();
        let hi = i64::from(r32(&b, 0) as u16 as i16) << 16;
        let lo = i64::from(r32(&b, 4) as u16 as i16);
        assert_eq!(0x1_0000 + hi + lo, 0x2_8000);
        // ld 3, 0(3) of a TOC entry 0x7ff8 past the base
        let mut b = 0xe863_0000u32.to_le_bytes().to_vec();
        let v = Values {
            s: 0x2_8000 + 0x7ff8,
            got_base: 0x2_8000,
            ..Values::default()
        };
        apply(
            site(TOC16_LO_DS, SymKind::Local),
            &mut b,
            0,
            &v,
            Dyn::None,
            "",
        )
        .unwrap();
        assert_eq!(r32(&b, 0), 0xe863_7ff8);
    }

    #[test]
    fn calls_through_a_stub_restore_the_toc() {
        let mut b = Vec::new();
        b.extend_from_slice(&0x4800_0001u32.to_le_bytes());
        b.extend_from_slice(&NOP.to_le_bytes());
        let v = Values {
            s: 0x1_0100,
            p: 0x1_0000,
            ..Values::default()
        };
        apply(site(REL24, SymKind::Dso), &mut b, 0, &v, Dyn::None, "").unwrap();
        assert_eq!(r32(&b, 0), 0x4800_0101);
        assert_eq!(r32(&b, 4), LD_R2);
        // A local call keeps its nop.
        b[4..].copy_from_slice(&NOP.to_le_bytes());
        apply(site(REL24, SymKind::Local), &mut b, 0, &v, Dyn::None, "").unwrap();
        assert_eq!(r32(&b, 4), NOP);
    }

    #[test]
    fn general_dynamic_relaxes_to_local_exec() {
        let mut b = Vec::new();
        for w in [0x3c62_0000u32, 0x3863_0000, 0x4800_0001, NOP] {
            b.extend_from_slice(&w.to_le_bytes());
        }
        let v = Values {
            tp: -0x7000 + 0x10,
            ..Values::default()
        };
        let s = |ty| site(ty, SymKind::Local);
        apply(s(GOT_TLSGD16_HA), &mut b, 0, &v, Dyn::None, "").unwrap();
        apply(s(GOT_TLSGD16_LO), &mut b, 4, &v, Dyn::None, "").unwrap();
        apply(s(TLSGD), &mut b, 8, &v, Dyn::None, "").unwrap();
        // nop; addis 3, 13, 0; nop; addi 3, 3, -0x6ff0
        assert_eq!(r32(&b, 0), NOP);
        assert_eq!(r32(&b, 4), 0x3c6d_0000);
        assert_eq!(r32(&b, 8), NOP);
        assert_eq!(r32(&b, 12), 0x3863_9010);
    }
}
