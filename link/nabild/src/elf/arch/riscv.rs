//! RISC-V relocations (RELA), the PLT entries, the `e_flags` and the
//! build attributes, as lld makes them for executables.
//!
//! No linker relaxation (as `--no-relax`): `RELAX` and `ALIGN` are
//! ignored, the assembler's padding stays.
//!
//! A `PCREL_LO12_*` names a label at its `AUIPC`, whose `*_HI20` gives the
//! value. Thread locals: variant I, the thread pointer at the start of
//! the block (no TCB offset); general dynamic through GOT slot pairs for
//! `__tls_get_addr`, the offsets biased by `-0x800` as glibc's DTV is;
//! initial exec through GOT slots; local exec. No TLS relaxations.

use crate::elf::reloc::{Dyn, Need, Site, SymKind, Values};
use nabic_core::leb::uleb;

pub const NONE: u16 = 0;
pub const R_32: u16 = 1;
pub const R_64: u16 = 2;
pub const RELATIVE: u32 = 3;
pub const JUMP_SLOT: u32 = 5;
pub const TLS_DTPMOD32: u32 = 6;
pub const TLS_DTPMOD64: u32 = 7;
pub const TLS_DTPREL32: u16 = 8;
pub const TLS_DTPREL64: u16 = 9;
pub const TLS_TPREL32: u32 = 10;
pub const TLS_TPREL64: u32 = 11;
pub const BRANCH: u16 = 16;
pub const JAL: u16 = 17;
pub const CALL: u16 = 18;
pub const CALL_PLT: u16 = 19;
pub const GOT_HI20: u16 = 20;
pub const TLS_GOT_HI20: u16 = 21;
pub const TLS_GD_HI20: u16 = 22;
pub const PCREL_HI20: u16 = 23;
pub const PCREL_LO12_I: u16 = 24;
pub const PCREL_LO12_S: u16 = 25;
pub const HI20: u16 = 26;
pub const LO12_I: u16 = 27;
pub const LO12_S: u16 = 28;
pub const TPREL_HI20: u16 = 29;
pub const TPREL_LO12_I: u16 = 30;
pub const TPREL_LO12_S: u16 = 31;
pub const TPREL_ADD: u16 = 32;
pub const ADD8: u16 = 33;
pub const ADD16: u16 = 34;
pub const ADD32: u16 = 35;
pub const ADD64: u16 = 36;
pub const SUB8: u16 = 37;
pub const SUB16: u16 = 38;
pub const SUB32: u16 = 39;
pub const SUB64: u16 = 40;
pub const ALIGN: u16 = 43;
pub const RVC_BRANCH: u16 = 44;
pub const RVC_JUMP: u16 = 45;
pub const RELAX: u16 = 51;
pub const SUB6: u16 = 52;
pub const SET6: u16 = 53;
pub const SET8: u16 = 54;
pub const SET16: u16 = 55;
pub const SET32: u16 = 56;
pub const PC32: u16 = 57;
pub const PLT32: u16 = 59;
pub const SET_ULEB128: u16 = 60;
pub const SUB_ULEB128: u16 = 61;

/// `EF_RISCV_RVC`, the float ABI field, `EF_RISCV_RVE`, `EF_RISCV_TSO`.
const EF_RVC: u32 = 0x1;
const EF_FLOAT_ABI: u32 = 0x6;
const EF_RVE: u32 = 0x8;
const EF_TSO: u32 = 0x10;
/// The program header of the build attributes.
pub const PT_ATTRIBUTES: u32 = 0x7000_0003;
pub const SHT_ATTRIBUTES: u32 = 0x7000_0003;
/// glibc's `TLS_DTV_OFFSET`: DTV entries point this far into a block.
pub const DTV_OFFSET: i64 = 0x800;
pub const PLT_ENTRY: u64 = 16;

/// Relocations that only mark code for relaxation.
#[must_use]
pub const fn ignored(ty: u32) -> bool {
    ty == ALIGN as u32 || ty == RELAX as u32
}

/// `e_flags` of the output after an input's `f`: the float ABI and RVE
/// must agree, RVC and TSO accumulate.
pub fn merge_flags(acc: Option<u32>, f: u32) -> Result<u32, String> {
    let Some(a) = acc else {
        return Ok(f);
    };
    if (a ^ f) & EF_FLOAT_ABI != 0 {
        return Err(
            "cannot link object files with different floating-point ABI (e_flags)".to_owned(),
        );
    }
    if (a ^ f) & EF_RVE != 0 {
        return Err("cannot link object files with different EF_RISCV_RVE".to_owned());
    }
    Ok(a | (f & (EF_RVC | EF_TSO)))
}

/// A relocation that refers to a GOT or TLS slot of its symbol.
#[must_use]
pub const fn needs_symbol(ty: u16) -> bool {
    matches!(ty, GOT_HI20 | TLS_GOT_HI20 | TLS_GD_HI20)
}

/// A `*_HI20` a `PCREL_LO12_*` takes its value from.
#[must_use]
pub const fn pcrel_hi(ty: u16) -> bool {
    matches!(ty, PCREL_HI20 | GOT_HI20 | TLS_GOT_HI20 | TLS_GD_HI20)
}

/// A branch (an undefined weak target: the place itself, as lld does).
const fn branch(ty: u16) -> bool {
    matches!(
        ty,
        BRANCH | JAL | CALL | CALL_PLT | RVC_BRANCH | RVC_JUMP | PLT32
    )
}

#[must_use]
pub const fn type_name(ty: u16) -> Option<&'static str> {
    Some(match ty {
        R_32 => "R_RISCV_32",
        R_64 => "R_RISCV_64",
        TLS_DTPREL32 => "R_RISCV_TLS_DTPREL32",
        TLS_DTPREL64 => "R_RISCV_TLS_DTPREL64",
        BRANCH => "R_RISCV_BRANCH",
        JAL => "R_RISCV_JAL",
        CALL => "R_RISCV_CALL",
        CALL_PLT => "R_RISCV_CALL_PLT",
        GOT_HI20 => "R_RISCV_GOT_HI20",
        TLS_GOT_HI20 => "R_RISCV_TLS_GOT_HI20",
        TLS_GD_HI20 => "R_RISCV_TLS_GD_HI20",
        PCREL_HI20 => "R_RISCV_PCREL_HI20",
        PCREL_LO12_I => "R_RISCV_PCREL_LO12_I",
        PCREL_LO12_S => "R_RISCV_PCREL_LO12_S",
        HI20 => "R_RISCV_HI20",
        LO12_I => "R_RISCV_LO12_I",
        LO12_S => "R_RISCV_LO12_S",
        TPREL_HI20 => "R_RISCV_TPREL_HI20",
        TPREL_LO12_I => "R_RISCV_TPREL_LO12_I",
        TPREL_LO12_S => "R_RISCV_TPREL_LO12_S",
        TPREL_ADD => "R_RISCV_TPREL_ADD",
        ADD8 => "R_RISCV_ADD8",
        ADD16 => "R_RISCV_ADD16",
        ADD32 => "R_RISCV_ADD32",
        ADD64 => "R_RISCV_ADD64",
        SUB8 => "R_RISCV_SUB8",
        SUB16 => "R_RISCV_SUB16",
        SUB32 => "R_RISCV_SUB32",
        SUB64 => "R_RISCV_SUB64",
        RVC_BRANCH => "R_RISCV_RVC_BRANCH",
        RVC_JUMP => "R_RISCV_RVC_JUMP",
        SUB6 => "R_RISCV_SUB6",
        SET6 => "R_RISCV_SET6",
        SET8 => "R_RISCV_SET8",
        SET16 => "R_RISCV_SET16",
        SET32 => "R_RISCV_SET32",
        PC32 => "R_RISCV_32_PCREL",
        PLT32 => "R_RISCV_PLT32",
        SET_ULEB128 => "R_RISCV_SET_ULEB128",
        SUB_ULEB128 => "R_RISCV_SUB_ULEB128",
        _ => return None,
    })
}

/// What site `s` needs (`wide`: RV64).
#[allow(clippy::match_same_arms)]
pub fn need(s: Site, name: &str, wide: bool) -> Result<Need, String> {
    let mut n = Need::default();
    let dso = s.kind == SymKind::Dso;
    let pic_err = |what: &str| {
        Err(format!(
            "relocation {name} cannot be used against {what}; recompile with -fPIC"
        ))
    };
    let abs_word = if wide { R_64 } else { R_32 };
    match s.ty {
        _ if s.ty == abs_word => {
            n.dynrel = s.alloc && (dso || (s.pie && s.kind == SymKind::Local));
        }
        R_32 | HI20 | LO12_I | LO12_S => {
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
        CALL | CALL_PLT | JAL | PLT32 if dso => n.plt = true,
        PCREL_HI20 | PC32 if dso => {
            if !s.func {
                return Err(format!(
                    "relocation {name} against data of a shared object needs a copy \
                     relocation, which Nabi's linker does not make; recompile with -fPIC"
                ));
            }
            n.plt = true;
        }
        BRANCH | JAL | CALL | CALL_PLT | RVC_BRANCH | RVC_JUMP | PLT32 | PCREL_HI20 | PC32
        | PCREL_LO12_I | PCREL_LO12_S => {}
        ADD8 | ADD16 | ADD32 | ADD64 | SUB6 | SUB8 | SUB16 | SUB32 | SUB64 | SET6 | SET8
        | SET16 | SET32 | SET_ULEB128 | SUB_ULEB128 | R_64 => {}
        GOT_HI20 => n.got = true,
        TLS_GOT_HI20 => n.tlsgot = true,
        TLS_GD_HI20 => n.tlsgd = true,
        TPREL_HI20 | TPREL_LO12_I | TPREL_LO12_S | TPREL_ADD if dso => {
            return Err(format!(
                "relocation {name} against a shared object's thread-local"
            ));
        }
        TPREL_HI20 | TPREL_LO12_I | TPREL_LO12_S | TPREL_ADD | TLS_DTPREL32 | TLS_DTPREL64 => {}
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

fn r16(b: &[u8], at: usize) -> u32 {
    u32::from(u16::from_le_bytes([b[at], b[at + 1]]))
}

const fn r32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn w16(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 2].copy_from_slice(&(v as u16).to_le_bytes());
}

fn w32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

fn wn(b: &mut [u8], at: usize, v: u64, n: usize) {
    b[at..at + n].copy_from_slice(&v.to_le_bytes()[..n]);
}

fn rn(b: &[u8], at: usize, n: usize) -> u64 {
    let mut x = [0u8; 8];
    x[..n].copy_from_slice(&b[at..at + n]);
    u64::from_le_bytes(x)
}

/// The upper 20 bits of `v` for a `U`-type immediate, rounded for the
/// sign-extended low 12 a following `I`/`S`-type adds.
const fn hi20(v: i64) -> u32 {
    ((v + 0x800) >> 12) as u32 & 0xf_ffff
}

/// `insn` with `U`-type immediate `v` (its upper 20 bits).
const fn set_u(insn: u32, v: i64) -> u32 {
    (insn & 0xfff) | (hi20(v) << 12)
}

/// `insn` with `I`-type immediate `v & 0xfff`.
const fn set_i(insn: u32, v: i64) -> u32 {
    (insn & 0x000f_ffff) | (((v as u32) & 0xfff) << 20)
}

/// `insn` with `S`-type immediate `v & 0xfff`.
const fn set_s(insn: u32, v: i64) -> u32 {
    let v = v as u32;
    (insn & 0x01ff_f07f) | (((v >> 5) & 0x7f) << 25) | ((v & 0x1f) << 7)
}

/// `insn` with `B`-type offset `v`.
const fn set_b(insn: u32, v: i64) -> u32 {
    let v = v as u32;
    (insn & 0x01ff_f07f)
        | (((v >> 12) & 1) << 31)
        | (((v >> 5) & 0x3f) << 25)
        | (((v >> 1) & 0xf) << 8)
        | (((v >> 11) & 1) << 7)
}

/// `insn` with `J`-type offset `v`.
const fn set_j(insn: u32, v: i64) -> u32 {
    let v = v as u32;
    (insn & 0xfff)
        | (((v >> 20) & 1) << 31)
        | (((v >> 1) & 0x3ff) << 21)
        | (((v >> 11) & 1) << 20)
        | (((v >> 12) & 0xff) << 12)
}

/// A ULEB128 at `b[at..]`: its value and length.
fn uleb_at(b: &[u8], at: usize) -> (u64, usize) {
    let (mut v, mut k) = (0u64, 0usize);
    while at + k < b.len() {
        let x = b[at + k];
        if k < 10 {
            v |= u64::from(x & 0x7f) << (7 * k);
        }
        k += 1;
        if x & 0x80 == 0 {
            break;
        }
    }
    (v, k)
}

/// `v` over the ULEB128 at `b[at..]`, in its length (padded).
pub fn put_uleb(name: &str, b: &mut [u8], at: usize, v: u64) -> Result<(), String> {
    let (_, n) = uleb_at(b, at);
    if n < 10 && v >> (7 * n) != 0 {
        return Err(format!(
            "relocation {name}: {v:#x} does not fit the {n}-byte ULEB128"
        ));
    }
    nabic_core::leb::put_uleb(&mut b[at..at + n], v);
    Ok(())
}

/// The bytes a relocation of type `ty` writes (ULEB128: at least 1).
#[must_use]
pub const fn width(ty: u16) -> usize {
    match ty {
        ADD8 | SUB8 | SUB6 | SET6 | SET8 | SET_ULEB128 | SUB_ULEB128 => 1,
        ADD16 | SUB16 | SET16 | RVC_BRANCH | RVC_JUMP => 2,
        R_64 | ADD64 | SUB64 | TLS_DTPREL64 | CALL | CALL_PLT => 8,
        _ => 4,
    }
}

/// Apply site `s` at `b[at..]` with `v`; `hi` is the value of the
/// `*_HI20` a `PCREL_LO12_*` names (its target less its place). The
/// dynamic relocation the place needs (`dynrel`).
#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
pub fn apply(
    s: Site,
    b: &mut [u8],
    at: usize,
    v: &Values,
    dynrel: Dyn,
    name: &str,
    hi: Option<i64>,
) -> Result<Dyn, String> {
    if b.len() < at + width(s.ty) {
        return Err(format!("relocation {name} runs past its section"));
    }
    let p = v.p as i64;
    let target = if s.kind == SymKind::UndefWeak && branch(s.ty) {
        p
    } else {
        v.s as i64
    };
    let sa = target.wrapping_add(v.a);
    let pc = sa.wrapping_sub(p);
    let tp = v.tp + v.a;
    let w = if b.len() >= at + 4 { r32(b, at) } else { 0 };
    match s.ty {
        R_64 => {
            let val = if s.kind == SymKind::Dso { v.a } else { sa };
            wn(b, at, val as u64, 8);
        }
        R_32 => {
            let val = if s.kind == SymKind::Dso { v.a } else { sa };
            if s.alloc {
                range(name, val, 33)?;
            }
            w32(b, at, val as u32);
        }
        PC32 | PLT32 => {
            range(name, pc, 32)?;
            w32(b, at, pc as u32);
        }
        BRANCH => {
            range(name, pc, 13)?;
            align(name, pc, 2)?;
            w32(b, at, set_b(w, pc));
        }
        JAL => {
            range(name, pc, 21)?;
            align(name, pc, 2)?;
            w32(b, at, set_j(w, pc));
        }
        CALL | CALL_PLT => {
            range(name, pc + 0x800, 32)?;
            w32(b, at, set_u(w, pc));
            let j = r32(b, at + 4);
            w32(b, at + 4, set_i(j, pc));
        }
        RVC_BRANCH => {
            range(name, pc, 9)?;
            let x = pc as u32;
            let h = r16(b, at) & 0xe383;
            w16(
                b,
                at,
                h | (((x >> 8) & 1) << 12)
                    | (((x >> 3) & 3) << 10)
                    | (((x >> 6) & 3) << 5)
                    | (((x >> 1) & 3) << 3)
                    | (((x >> 5) & 1) << 2),
            );
        }
        RVC_JUMP => {
            range(name, pc, 12)?;
            let x = pc as u32;
            let h = r16(b, at) & 0xe003;
            w16(
                b,
                at,
                h | (((x >> 11) & 1) << 12)
                    | (((x >> 4) & 1) << 11)
                    | (((x >> 8) & 3) << 9)
                    | (((x >> 10) & 1) << 8)
                    | (((x >> 6) & 1) << 7)
                    | (((x >> 7) & 1) << 6)
                    | (((x >> 1) & 7) << 3)
                    | (((x >> 5) & 1) << 2),
            );
        }
        PCREL_HI20 => {
            range(name, pc + 0x800, 32)?;
            w32(b, at, set_u(w, pc));
        }
        GOT_HI20 | TLS_GOT_HI20 | TLS_GD_HI20 => {
            let slot = if s.ty == GOT_HI20 { v.g } else { v.tg };
            let d = (slot as i64).wrapping_add(v.a).wrapping_sub(p);
            range(name, d + 0x800, 32)?;
            w32(b, at, set_u(w, d));
        }
        PCREL_LO12_I | PCREL_LO12_S => {
            let d = hi.ok_or_else(|| {
                format!("{name}: the label it names has no PCREL_HI20 / GOT_HI20 at its place")
            })?;
            w32(
                b,
                at,
                if s.ty == PCREL_LO12_I {
                    set_i(w, d)
                } else {
                    set_s(w, d)
                },
            );
        }
        HI20 => {
            range(name, sa + 0x800, 32)?;
            w32(b, at, set_u(w, sa));
        }
        LO12_I => w32(b, at, set_i(w, sa)),
        LO12_S => w32(b, at, set_s(w, sa)),
        TPREL_HI20 => {
            range(name, tp + 0x800, 32)?;
            w32(b, at, set_u(w, tp));
        }
        TPREL_LO12_I => w32(b, at, set_i(w, tp)),
        TPREL_LO12_S => w32(b, at, set_s(w, tp)),
        TPREL_ADD => {}
        TLS_DTPREL32 => w32(b, at, (v.dtp + v.a - DTV_OFFSET) as u32),
        TLS_DTPREL64 => wn(b, at, (v.dtp + v.a - DTV_OFFSET) as u64, 8),
        ADD8 | ADD16 | ADD32 | ADD64 | SUB8 | SUB16 | SUB32 | SUB64 => {
            let n = width(s.ty);
            let old = rn(b, at, n);
            let x = if s.ty >= SUB8 {
                old.wrapping_sub(sa as u64)
            } else {
                old.wrapping_add(sa as u64)
            };
            wn(b, at, x, n);
        }
        SUB6 => b[at] = (b[at] & 0xc0) | (b[at].wrapping_sub(sa as u8) & 0x3f),
        SET6 => b[at] = (b[at] & 0xc0) | (sa as u8 & 0x3f),
        SET8 => b[at] = sa as u8,
        SET16 => wn(b, at, sa as u64, 2),
        SET32 => w32(b, at, sa as u32),
        SET_ULEB128 => put_uleb(name, b, at, sa as u64)?,
        SUB_ULEB128 => {
            let (old, _) = uleb_at(b, at);
            put_uleb(name, b, at, old.wrapping_sub(sa as u64))?;
        }
        _ => return Err(format!("unsupported relocation {name}")),
    }
    Ok(dynrel)
}

/// The value of `*_HI20` site `ty` at place `p` (`PCREL_LO12_*` takes its
/// low 12 bits): the target, GOT slot or TLS slot less the place.
#[must_use]
pub const fn hi_value(ty: u16, v: &Values) -> i64 {
    let base = match ty {
        GOT_HI20 => v.g,
        TLS_GOT_HI20 | TLS_GD_HI20 => v.tg,
        _ => v.s,
    };
    (base as i64).wrapping_add(v.a).wrapping_sub(v.p as i64)
}

/// PLT entry at `at` (address `addr`) jumping through GOT slot `slot`:
/// `auipc t3, %pcrel_hi(slot); l[wd] t3, %pcrel_lo(slot)(t3); jalr t1,
/// t3; nop`, as lld's.
pub fn write_plt(b: &mut [u8], at: usize, addr: u64, slot: u64, wide: bool) {
    let d = slot.wrapping_sub(addr) as i64;
    let load = if wide { 0x000e_3e03 } else { 0x000e_2e03 };
    w32(b, at, set_u(0x0000_0e17, d));
    w32(b, at + 4, set_i(load, d));
    w32(b, at + 8, 0x000e_0367);
    w32(b, at + 12, NOP);
}

/// `addi x0, x0, 0`; `c.nop` fills a 2-byte remainder.
pub const NOP: u32 = 0x0000_0013;

/// Fill code padding `b` with `nop`s.
pub fn fill_nops(b: &mut [u8]) {
    let mut w = b.chunks_exact_mut(4);
    for c in &mut w {
        c.copy_from_slice(&NOP.to_le_bytes());
    }
    let r = w.into_remainder();
    if r.len() >= 2 {
        r[..2].copy_from_slice(&1u16.to_le_bytes());
    }
}

/// An attribute of the `riscv` vendor subsection: an integer (even tags,
/// as the psABI numbers them) or a string.
#[derive(Clone, Debug, Eq, PartialEq)]
enum Attr {
    Int(u64),
    Str(String),
}

/// The file-level attributes of one `.riscv.attributes` section.
fn parse_attributes(d: &[u8]) -> Vec<(u64, Attr)> {
    let mut out = Vec::new();
    if d.first() != Some(&b'A') {
        return out;
    }
    let mut p = 1;
    while p + 4 <= d.len() {
        let len = r32(d, p) as usize;
        if len < 4 || p + len > d.len() {
            break;
        }
        let sub = &d[p + 4..p + len];
        p += len;
        let Some(z) = sub.iter().position(|&c| c == 0) else {
            continue;
        };
        if &sub[..z] != b"riscv" {
            continue;
        }
        let mut q = z + 1;
        while q < sub.len() {
            let (tag, n) = uleb_at(sub, q);
            if q + n + 4 > sub.len() {
                break;
            }
            let size = r32(sub, q + n) as usize;
            let end = (q + size).min(sub.len());
            let mut k = q + n + 4;
            // Tag_File only; section and symbol scopes are not used.
            while tag == 1 && k < end {
                let (t, n) = uleb_at(sub, k);
                k += n;
                if t % 2 == 0 {
                    let (v, n) = uleb_at(sub, k);
                    k += n;
                    out.push((t, Attr::Int(v)));
                } else {
                    let e = sub[k..end]
                        .iter()
                        .position(|&c| c == 0)
                        .map_or(end, |e| k + e);
                    out.push((
                        t,
                        Attr::Str(String::from_utf8_lossy(&sub[k..e]).into_owned()),
                    ));
                    k = e + 1;
                }
            }
            if size == 0 {
                break;
            }
            q += size;
        }
    }
    out
}

/// Canonical order of a single-letter extension.
fn letter_rank(c: char) -> usize {
    "imafdqlcbkjtpvh".find(c).unwrap_or(26 + c as usize)
}

/// Sort key of extension `e` in an ISA string: single letters, then
/// `z*` by their first letter's rank, `s*`, `x*`.
fn ext_key(e: &str) -> (usize, usize, String) {
    let mut cs = e.chars();
    let c0 = cs.next().unwrap_or('i');
    if e.len() == 1 {
        return (0, letter_rank(c0), String::new());
    }
    match c0 {
        'z' => (1, letter_rank(cs.next().unwrap_or('z')), e.to_owned()),
        's' => (2, 0, e.to_owned()),
        'x' => (3, 0, e.to_owned()),
        _ => (4, 0, e.to_owned()),
    }
}

/// `name<major>p<minor>` split.
fn ext_version(s: &str) -> (String, u32, u32) {
    let digits = |t: &str| t.len() - t.trim_end_matches(|c: char| c.is_ascii_digit()).len();
    let nmin = digits(s);
    let rest = &s[..s.len() - nmin];
    if nmin > 0 && rest.ends_with('p') {
        let r = &rest[..rest.len() - 1];
        let nmaj = digits(r);
        if nmaj > 0 {
            let (name, maj) = r.split_at(r.len() - nmaj);
            return (
                name.to_owned(),
                maj.parse().unwrap_or(0),
                s[s.len() - nmin..].parse().unwrap_or(0),
            );
        }
    }
    (s.to_owned(), 0, 0)
}

/// The union of ISA strings (`rv64i2p1_m2p0_...`), each extension at its
/// highest version, canonically ordered.
fn merge_arch(archs: &[&str]) -> Option<String> {
    let first = archs.first()?;
    if archs.iter().all(|a| a == first) {
        return Some((*first).to_owned());
    }
    let mut xlen = "";
    let mut exts: std::collections::BTreeMap<(usize, usize, String), (String, u32, u32)> =
        std::collections::BTreeMap::new();
    for a in archs {
        let Some(body) = a.strip_prefix("rv32").or_else(|| a.strip_prefix("rv64")) else {
            continue;
        };
        xlen = &a[..4];
        for e in body.split('_').filter(|e| !e.is_empty()) {
            let (name, maj, min) = ext_version(e);
            let k = ext_key(&name);
            let slot = exts.entry(k).or_insert((name, maj, min));
            if (maj, min) > (slot.1, slot.2) {
                slot.1 = maj;
                slot.2 = min;
            }
        }
    }
    let parts: Vec<String> = exts
        .values()
        .map(|(n, a, b)| format!("{n}{a}p{b}"))
        .collect();
    Some(format!("{xlen}{}", parts.join("_")))
}

/// The output's `.riscv.attributes`: the inputs' merged as lld does
/// (the stack alignment must agree, the ISA strings are joined,
/// unaligned access is any input's, the rest kept while they agree).
pub fn merge_attributes(inputs: &[Vec<u8>]) -> Result<Vec<u8>, String> {
    let all: Vec<Vec<(u64, Attr)>> = inputs.iter().map(|d| parse_attributes(d)).collect();
    let mut tags: std::collections::BTreeMap<u64, Option<Attr>> = std::collections::BTreeMap::new();
    let mut archs: Vec<String> = Vec::new();
    for f in &all {
        for (t, a) in f {
            match (*t, a) {
                (5, Attr::Str(s)) => archs.push(s.clone()),
                (6, Attr::Int(x)) => {
                    let e = tags.entry(6).or_insert(Some(Attr::Int(0)));
                    if *x != 0 {
                        *e = Some(Attr::Int(1));
                    }
                }
                (4, Attr::Int(x)) => match tags.get(&4) {
                    Some(Some(Attr::Int(y))) if y != x => {
                        return Err(format!(
                            ".riscv.attributes: different stack alignments {y} and {x}"
                        ));
                    }
                    _ => {
                        tags.insert(4, Some(a.clone()));
                    }
                },
                _ => match tags.get(t) {
                    Some(Some(y)) if y != a => {
                        tags.insert(*t, None);
                    }
                    Some(None) => {}
                    _ => {
                        tags.insert(*t, Some(a.clone()));
                    }
                },
            }
        }
    }
    let refs: Vec<&str> = archs.iter().map(String::as_str).collect();
    if let Some(a) = merge_arch(&refs) {
        tags.insert(5, Some(Attr::Str(a)));
    }
    let mut body = Vec::new();
    for (t, a) in &tags {
        let Some(a) = a else { continue };
        uleb(&mut body, *t);
        match a {
            Attr::Int(x) => uleb(&mut body, *x),
            Attr::Str(s) => {
                body.extend_from_slice(s.as_bytes());
                body.push(0);
            }
        }
    }
    let mut file = vec![1u8];
    file.extend_from_slice(&(body.len() as u32 + 5).to_le_bytes());
    file.extend_from_slice(&body);
    let mut sub = Vec::new();
    sub.extend_from_slice(&(file.len() as u32 + 10).to_le_bytes());
    sub.extend_from_slice(b"riscv\0");
    sub.extend_from_slice(&file);
    let mut out = vec![b'A'];
    out.extend_from_slice(&sub);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn site(ty: u16) -> Site {
        Site {
            arch: crate::elf::Arch::Riscv64,
            ty,
            kind: SymKind::Local,
            func: true,
            alloc: true,
            pie: true,
            dyn_tls: false,
        }
    }

    fn vals(s: u64, p: u64) -> Values {
        Values {
            s,
            p,
            ..Values::default()
        }
    }

    /// The immediate of a `B` / `J` / `I` / `S`-type instruction.
    fn imm_b(i: u32) -> i64 {
        let v = ((i >> 31) << 12)
            | (((i >> 25) & 0x3f) << 5)
            | (((i >> 8) & 0xf) << 1)
            | (((i >> 7) & 1) << 11);
        (i64::from(v) << 51) >> 51
    }

    fn imm_j(i: u32) -> i64 {
        let v = ((i >> 31) << 20)
            | (((i >> 21) & 0x3ff) << 1)
            | (((i >> 20) & 1) << 11)
            | (((i >> 12) & 0xff) << 12);
        (i64::from(v) << 43) >> 43
    }

    #[test]
    fn branches_and_calls_reach_their_targets() {
        // beq a0, a1, 0; jal ra, 0; auipc ra, 0 + jalr ra, 0(ra)
        let mut b = 0x00b5_0063u32.to_le_bytes().to_vec();
        apply(
            site(BRANCH),
            &mut b,
            0,
            &vals(0x1000 - 0x7fe, 0x1000),
            Dyn::None,
            "",
            None,
        )
        .unwrap();
        assert_eq!(imm_b(r32(&b, 0)), -0x7fe);
        let mut b = 0x0000_00efu32.to_le_bytes().to_vec();
        apply(
            site(JAL),
            &mut b,
            0,
            &vals(0x2_1000, 0x1000),
            Dyn::None,
            "",
            None,
        )
        .unwrap();
        assert_eq!(imm_j(r32(&b, 0)), 0x2_0000);
        let mut b = Vec::new();
        b.extend_from_slice(&0x0000_0097u32.to_le_bytes());
        b.extend_from_slice(&0x0000_80e7u32.to_le_bytes());
        apply(
            site(CALL_PLT),
            &mut b,
            0,
            &vals(0x1234_5ffc, 0x1000),
            Dyn::None,
            "",
            None,
        )
        .unwrap();
        let hi = i64::from((r32(&b, 0) >> 12) as i32) << 12;
        let lo = i64::from((r32(&b, 4) as i32) >> 20);
        assert_eq!(0x1000 + ((hi << 32) >> 32) + lo, 0x1234_5ffc);
    }

    #[test]
    fn compressed_branches() {
        // c.beqz a0, 0; c.j 0
        let mut b = 0xc101u16.to_le_bytes().to_vec();
        apply(
            site(RVC_BRANCH),
            &mut b,
            0,
            &vals(0x1000 - 0x82, 0x1000),
            Dyn::None,
            "",
            None,
        )
        .unwrap();
        let h = r16(&b, 0);
        let v = (((h >> 12) & 1) << 8)
            | (((h >> 10) & 3) << 3)
            | (((h >> 5) & 3) << 6)
            | (((h >> 3) & 3) << 1)
            | (((h >> 2) & 1) << 5);
        assert_eq!((i64::from(v) << 55) >> 55, -0x82);
        let mut b = 0xa001u16.to_le_bytes().to_vec();
        apply(
            site(RVC_JUMP),
            &mut b,
            0,
            &vals(0x1000 + 0x7fe, 0x1000),
            Dyn::None,
            "",
            None,
        )
        .unwrap();
        assert_eq!(r16(&b, 0) & 0xe003, 0xa001);
    }

    #[test]
    fn uleb_pairs_keep_their_length() {
        let mut b = vec![0x80, 0x80, 0x00];
        apply(
            site(SET_ULEB128),
            &mut b,
            0,
            &vals(0x1234, 0),
            Dyn::None,
            "",
            None,
        )
        .unwrap();
        apply(
            site(SUB_ULEB128),
            &mut b,
            0,
            &vals(0x1200, 0),
            Dyn::None,
            "",
            None,
        )
        .unwrap();
        assert_eq!(b, [0xb4, 0x80, 0x00]);
    }

    #[test]
    fn isa_strings_merge() {
        let a = merge_arch(&[
            "rv64i2p1_m2p0_c2p0_zicsr2p0",
            "rv64i2p1_m2p0_a2p1_d2p2_zmmul1p0_zicsr2p0",
        ]);
        assert_eq!(
            a.as_deref(),
            Some("rv64i2p1_m2p0_a2p1_d2p2_c2p0_zicsr2p0_zmmul1p0")
        );
        assert_eq!(ext_version("zvl128b1p0"), ("zvl128b".to_owned(), 1, 0));
    }
}
