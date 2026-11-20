//! 32-bit ARM relocations (REL, the addends in the places), the PLT
//! entries, the interworking thunks and the exception index table, as
//! lld makes them for executables.
//!
//! A branch between A32 and T32 code: `bl` becomes `blx` (and back) by
//! bit 0 of the target, a `b` goes through a thunk. The PLT is A32 and
//! jumps through `.got` slots bound at load time (`GLOB_DAT`). Thread
//! locals: variant I, the TCB 8 bytes; general and local dynamic through
//! GOT slot pairs for `__tls_get_addr` (the executable is module 1),
//! initial exec through GOT slots, local exec; no TLS relaxations (lld
//! makes none on ARM).

use crate::elf::reloc::{Dyn, Need, Site, SymKind, Values};

pub const NONE: u16 = 0;
pub const PC24: u16 = 1;
pub const ABS32: u16 = 2;
pub const REL32: u16 = 3;
pub const ABS16: u16 = 5;
pub const ABS8: u16 = 8;
pub const THM_CALL: u16 = 10;
pub const TLS_DTPMOD32: u32 = 17;
pub const TLS_DTPOFF32: u32 = 18;
pub const TLS_TPOFF32: u32 = 19;
pub const GLOB_DAT: u32 = 21;
pub const RELATIVE: u32 = 23;
pub const GOTOFF32: u16 = 24;
pub const BASE_PREL: u16 = 25;
pub const GOT_BREL: u16 = 26;
pub const PLT32: u16 = 27;
pub const CALL: u16 = 28;
pub const JUMP24: u16 = 29;
pub const THM_JUMP24: u16 = 30;
pub const TARGET1: u16 = 38;
pub const V4BX: u16 = 40;
pub const TARGET2: u16 = 41;
pub const PREL31: u16 = 42;
pub const MOVW_ABS_NC: u16 = 43;
pub const MOVT_ABS: u16 = 44;
pub const MOVW_PREL_NC: u16 = 45;
pub const MOVT_PREL: u16 = 46;
pub const THM_MOVW_ABS_NC: u16 = 47;
pub const THM_MOVT_ABS: u16 = 48;
pub const THM_MOVW_PREL_NC: u16 = 49;
pub const THM_MOVT_PREL: u16 = 50;
pub const THM_JUMP19: u16 = 51;
pub const ABS32_NOI: u16 = 55;
pub const REL32_NOI: u16 = 56;
pub const GOT_PREL: u16 = 96;
pub const THM_JUMP11: u16 = 102;
pub const THM_JUMP8: u16 = 103;
pub const TLS_GD32: u16 = 104;
pub const TLS_LDM32: u16 = 105;
pub const TLS_LDO32: u16 = 106;
pub const TLS_IE32: u16 = 107;
pub const TLS_LE32: u16 = 108;

/// `EF_ARM_EABI_VER5 | EF_ARM_ABI_FLOAT_HARD`.
pub const EF_ARM: u32 = 0x0500_0400;
/// The program header of the exception index table.
pub const PT_ARM_EXIDX: u32 = 0x7000_0001;
/// `SHT_ARM_EXIDX`, `SHF_LINK_ORDER`.
pub const SHT_EXIDX: u32 = 0x7000_0001;
pub const SHF_LINK_ORDER: u64 = 0x80;
pub const SHT_ATTRIBUTES: u32 = 0x7000_0003;
/// An index entry for code that cannot be unwound.
pub const EXIDX_CANTUNWIND: u32 = 1;
/// A PLT entry, a thunk.
pub const PLT_ENTRY: u64 = 16;
pub const THUNK: u64 = 16;
/// Code padding: `nop`.
pub const NOP: u32 = 0xe320_f000;
/// The build attributes of the output: ARMv7-A, VFPv3-D32, NEON, the
/// hard-float AAPCS, as LLVM writes them for objects.
pub const ATTRIBUTES: [u8; 58] = [
    0x41, 0x39, 0, 0, 0, b'a', b'e', b'a', b'b', b'i', 0, 1, 0x2f, 0, 0, 0, 0x43, b'2', b'.', b'0',
    b'9', 0, 6, 10, 7, b'A', 8, 1, 9, 2, 10, 3, 12, 1, 14, 0, 17, 1, 18, 4, 20, 1, 21, 1, 23, 3,
    24, 1, 25, 1, 28, 1, 30, 1, 34, 1, 38, 1,
];

const fn sext(v: u32, bits: u32) -> i64 {
    ((v as i64) << (64 - bits)) >> (64 - bits)
}

fn r16(d: &[u8], at: usize) -> u32 {
    d.get(at..at + 2)
        .map_or(0, |b| u32::from(u16::from_le_bytes([b[0], b[1]])))
}

fn r32(d: &[u8], at: usize) -> u32 {
    d.get(at..at + 4)
        .map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn w16(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 2].copy_from_slice(&(v as u16).to_le_bytes());
}

fn w32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

/// The 16-bit immediate of an A32 `movw` / `movt`.
const fn movw_imm(w: u32) -> u32 {
    ((w >> 4) & 0xf000) | (w & 0xfff)
}

const fn set_movw(w: u32, imm: u32) -> u32 {
    (w & 0xfff0_f000) | ((imm & 0xf000) << 4) | (imm & 0xfff)
}

/// The 16-bit immediate of a T32 `movw` / `movt` (halfwords `h1 h2`).
const fn thm_movw_imm(h1: u32, h2: u32) -> u32 {
    ((h1 & 0xf) << 12) | (((h1 >> 10) & 1) << 11) | (((h2 >> 12) & 7) << 8) | (h2 & 0xff)
}

/// The offset of a T32 `bl` / `blx` / `b.w` (`S:I1:I2:imm10:imm11:0`).
const fn thm_branch(h1: u32, h2: u32) -> u32 {
    let s = (h1 >> 10) & 1;
    let i1 = !(((h2 >> 13) & 1) ^ s) & 1;
    let i2 = !(((h2 >> 11) & 1) ^ s) & 1;
    (s << 24) | (i1 << 23) | (i2 << 22) | ((h1 & 0x3ff) << 12) | ((h2 & 0x7ff) << 1)
}

/// The addend a REL relocation of type `ty` keeps at `d[at..]`.
#[must_use]
pub fn implicit_addend(d: &[u8], at: usize, ty: u16) -> i64 {
    let w = r32(d, at);
    let (h1, h2) = (r16(d, at), r16(d, at + 2));
    match ty {
        NONE | V4BX => 0,
        PREL31 => sext(w, 31),
        CALL | JUMP24 | PC24 | PLT32 => {
            // `blx` keeps bit 1 in H.
            let h = if w & 0xfe00_0000 == 0xfa00_0000 {
                ((w >> 24) & 1) << 1
            } else {
                0
            };
            sext((w & 0x00ff_ffff) << 2, 26) | i64::from(h)
        }
        THM_CALL | THM_JUMP24 => sext(thm_branch(h1, h2), 25),
        THM_JUMP19 => {
            let s = (h1 >> 10) & 1;
            let v = (s << 20)
                | (((h2 >> 11) & 1) << 19)
                | (((h2 >> 13) & 1) << 18)
                | ((h1 & 0x3f) << 12)
                | ((h2 & 0x7ff) << 1);
            sext(v, 21)
        }
        THM_JUMP11 => sext((h1 & 0x7ff) << 1, 12),
        THM_JUMP8 => sext((h1 & 0xff) << 1, 9),
        MOVW_ABS_NC | MOVT_ABS | MOVW_PREL_NC | MOVT_PREL => sext(movw_imm(w), 16),
        THM_MOVW_ABS_NC | THM_MOVT_ABS | THM_MOVW_PREL_NC | THM_MOVT_PREL => {
            sext(thm_movw_imm(h1, h2), 16)
        }
        ABS16 => sext(w & 0xffff, 16),
        ABS8 => sext(w & 0xff, 8),
        _ => sext(w, 32),
    }
}

/// A relocation that refers to a GOT or TLS slot of its symbol.
#[must_use]
pub const fn needs_symbol(ty: u16) -> bool {
    matches!(ty, GOT_BREL | GOT_PREL | TARGET2 | TLS_IE32 | TLS_GD32)
}

/// A branch (the target an undefined weak function's: the next
/// instruction).
const fn branch(ty: u16) -> bool {
    matches!(
        ty,
        CALL | JUMP24 | PC24 | PLT32 | THM_CALL | THM_JUMP24 | THM_JUMP19 | THM_JUMP11 | THM_JUMP8
    )
}

#[must_use]
pub const fn type_name(ty: u16) -> Option<&'static str> {
    Some(match ty {
        PC24 => "R_ARM_PC24",
        ABS32 => "R_ARM_ABS32",
        REL32 => "R_ARM_REL32",
        ABS16 => "R_ARM_ABS16",
        ABS8 => "R_ARM_ABS8",
        THM_CALL => "R_ARM_THM_CALL",
        GOTOFF32 => "R_ARM_GOTOFF32",
        BASE_PREL => "R_ARM_BASE_PREL",
        GOT_BREL => "R_ARM_GOT_BREL",
        PLT32 => "R_ARM_PLT32",
        CALL => "R_ARM_CALL",
        JUMP24 => "R_ARM_JUMP24",
        THM_JUMP24 => "R_ARM_THM_JUMP24",
        TARGET1 => "R_ARM_TARGET1",
        TARGET2 => "R_ARM_TARGET2",
        PREL31 => "R_ARM_PREL31",
        MOVW_ABS_NC => "R_ARM_MOVW_ABS_NC",
        MOVT_ABS => "R_ARM_MOVT_ABS",
        MOVW_PREL_NC => "R_ARM_MOVW_PREL_NC",
        MOVT_PREL => "R_ARM_MOVT_PREL",
        THM_MOVW_ABS_NC => "R_ARM_THM_MOVW_ABS_NC",
        THM_MOVT_ABS => "R_ARM_THM_MOVT_ABS",
        THM_MOVW_PREL_NC => "R_ARM_THM_MOVW_PREL_NC",
        THM_MOVT_PREL => "R_ARM_THM_MOVT_PREL",
        THM_JUMP19 => "R_ARM_THM_JUMP19",
        ABS32_NOI => "R_ARM_ABS32_NOI",
        REL32_NOI => "R_ARM_REL32_NOI",
        GOT_PREL => "R_ARM_GOT_PREL",
        THM_JUMP11 => "R_ARM_THM_JUMP11",
        THM_JUMP8 => "R_ARM_THM_JUMP8",
        TLS_GD32 => "R_ARM_TLS_GD32",
        TLS_LDM32 => "R_ARM_TLS_LDM32",
        TLS_LDO32 => "R_ARM_TLS_LDO32",
        TLS_IE32 => "R_ARM_TLS_IE32",
        TLS_LE32 => "R_ARM_TLS_LE32",
        _ => return None,
    })
}

/// What site `s` needs.
#[allow(clippy::match_same_arms)]
pub fn need(s: Site, name: &str) -> Result<Need, String> {
    let mut n = Need::default();
    let dso = s.kind == SymKind::Dso;
    let pic_err = |what: &str| {
        Err(format!(
            "relocation {name} cannot be used against {what}; recompile with -fPIC"
        ))
    };
    match s.ty {
        ABS32 | TARGET1 | ABS32_NOI => {
            n.dynrel = s.alloc && (dso || (s.pie && s.kind == SymKind::Local));
        }
        ABS16 | ABS8 | MOVW_ABS_NC | MOVT_ABS | THM_MOVW_ABS_NC | THM_MOVT_ABS => {
            if dso {
                return pic_err("a shared object's symbol");
            }
            if s.alloc && s.pie && s.kind == SymKind::Local {
                return pic_err("a local symbol in a PIE");
            }
        }
        REL32 | REL32_NOI | PREL31 | MOVW_PREL_NC | MOVT_PREL | THM_MOVW_PREL_NC
        | THM_MOVT_PREL
            if dso =>
        {
            if !s.func {
                return Err(format!(
                    "relocation {name} against data of a shared object needs a copy \
                     relocation, which Nabi's linker does not make; recompile with -fPIC"
                ));
            }
            n.plt = true;
        }
        REL32 | REL32_NOI | PREL31 | MOVW_PREL_NC | MOVT_PREL | THM_MOVW_PREL_NC
        | THM_MOVT_PREL => {}
        _ if branch(s.ty) && dso => n.plt = true,
        _ if branch(s.ty) => {}
        GOT_BREL => {
            n.got = true;
            n.got_base = true;
        }
        GOT_PREL | TARGET2 => n.got = true,
        BASE_PREL => n.got_base = true,
        GOTOFF32 if dso => return pic_err("a shared object's symbol"),
        GOTOFF32 => n.got_base = true,
        TLS_IE32 => n.tlsgot = true,
        TLS_LE32 if dso => {
            return Err(format!(
                "relocation {name} against a shared object's thread-local"
            ));
        }
        TLS_LE32 | TLS_LDO32 => {}
        TLS_GD32 => n.tlsgd = true,
        TLS_LDM32 if dso => {
            return Err(format!(
                "relocation {name} against a shared object's thread-local"
            ));
        }
        TLS_LDM32 => n.tlsld = true,
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

/// Apply site `s` at `b[at..]` with `v`; the dynamic relocation the
/// place needs (`dynrel`).
#[allow(clippy::too_many_lines)]
pub fn apply(
    s: Site,
    b: &mut [u8],
    at: usize,
    v: &Values,
    dynrel: Dyn,
    name: &str,
) -> Result<Dyn, String> {
    let p = v.p as i64;
    // An undefined weak function: the next instruction.
    let target = if s.kind == SymKind::UndefWeak && branch(s.ty) {
        p + 4
    } else {
        v.s as i64
    };
    let sa = target.wrapping_add(v.a);
    let pc = sa.wrapping_sub(p);
    let g = (v.g as i64).wrapping_add(v.a);
    let gb = v.got_base as i64;
    let func = s.func || s.kind == SymKind::Dso;
    let put32 = |b: &mut [u8], val: i64| w32(b, at, val as u32);
    let w = r32(b, at);
    match s.ty {
        ABS32 | TARGET1 | ABS32_NOI => {
            // A symbolic dynamic relocation adds the symbol at load.
            let val = if s.kind == SymKind::Dso { v.a } else { sa };
            put32(b, val);
        }
        ABS16 => {
            range(name, sa, 17)?;
            w16(b, at, sa as u32);
        }
        ABS8 => {
            range(name, sa, 9)?;
            b[at] = sa as u8;
        }
        REL32 | REL32_NOI => put32(b, pc),
        PREL31 => {
            range(name, pc, 31)?;
            put32(b, i64::from((w & 0x8000_0000) | (pc as u32 & 0x7fff_ffff)));
        }
        GOT_BREL => put32(b, g.wrapping_sub(gb)),
        GOT_PREL | TARGET2 => put32(b, g.wrapping_sub(p)),
        BASE_PREL => put32(b, gb.wrapping_add(v.a).wrapping_sub(p)),
        GOTOFF32 => put32(b, sa.wrapping_sub(gb)),
        // `tg`: the slot (pair) the relocation names.
        TLS_IE32 | TLS_GD32 | TLS_LDM32 => {
            put32(b, (v.tg as i64).wrapping_add(v.a).wrapping_sub(p));
        }
        TLS_LE32 => put32(b, v.tp + v.a),
        TLS_LDO32 => put32(b, v.dtp + v.a),
        CALL => {
            let thumb = pc & 1 != 0;
            let blx = w & 0xfe00_0000 == 0xfa00_0000;
            range(name, pc, 26)?;
            if if func { thumb } else { blx } {
                put32(
                    b,
                    i64::from(
                        0xfa00_0000 | ((pc as u32 & 2) << 23) | ((pc as u32 >> 2) & 0x00ff_ffff),
                    ),
                );
            } else {
                // `bl`, unconditional when it was `blx`.
                let base = if blx { 0xeb00_0000 } else { w & 0xff00_0000 };
                put32(b, i64::from(base | ((pc as u32 >> 2) & 0x00ff_ffff)));
            }
        }
        JUMP24 | PC24 | PLT32 => {
            range(name, pc, 26)?;
            put32(
                b,
                i64::from((w & 0xff00_0000) | ((pc as u32 >> 2) & 0x00ff_ffff)),
            );
        }
        THM_CALL | THM_JUMP24 => {
            let (h1, h2) = (r16(b, at), r16(b, at + 2));
            let mut val = pc;
            let mut h2 = h2;
            if s.ty == THM_CALL {
                let blx = h2 & 0x1000 == 0;
                if if func { pc & 1 == 0 } else { blx } {
                    // `blx` to A32: the target 4-byte aligned from the
                    // aligned place.
                    val = (val + 3) & !3;
                    h2 &= !0x1000;
                } else {
                    h2 |= 0x1000;
                }
            }
            range(name, val, 25)?;
            let v = val as u32;
            w16(
                b,
                at,
                (h1 & 0xf800) | ((v >> 14) & 0x0400) | ((v >> 12) & 0x03ff),
            );
            w16(
                b,
                at + 2,
                (h2 & 0xd000)
                    | ((!(v >> 10) ^ (v >> 11)) & 0x2000)
                    | ((!(v >> 11) ^ (v >> 13)) & 0x0800)
                    | ((v >> 1) & 0x07ff),
            );
        }
        THM_JUMP19 => {
            range(name, pc, 21)?;
            let v = pc as u32;
            let (h1, h2) = (r16(b, at), r16(b, at + 2));
            w16(
                b,
                at,
                (h1 & 0xfbc0) | ((v >> 10) & 0x0400) | ((v >> 12) & 0x003f),
            );
            w16(
                b,
                at + 2,
                (h2 & 0xd000) | ((v >> 8) & 0x0800) | ((v >> 5) & 0x2000) | ((v >> 1) & 0x07ff),
            );
        }
        THM_JUMP11 => {
            range(name, pc, 12)?;
            w16(b, at, (r16(b, at) & 0xf800) | ((pc as u32 >> 1) & 0x07ff));
        }
        THM_JUMP8 => {
            range(name, pc, 9)?;
            w16(b, at, (r16(b, at) & 0xff00) | ((pc as u32 >> 1) & 0xff));
        }
        MOVW_ABS_NC => put32(b, i64::from(set_movw(w, sa as u32 & 0xffff))),
        MOVT_ABS => put32(b, i64::from(set_movw(w, (sa as u32) >> 16))),
        MOVW_PREL_NC => put32(b, i64::from(set_movw(w, pc as u32 & 0xffff))),
        MOVT_PREL => put32(b, i64::from(set_movw(w, (pc as u32) >> 16))),
        THM_MOVW_ABS_NC | THM_MOVT_ABS | THM_MOVW_PREL_NC | THM_MOVT_PREL => {
            let x = match s.ty {
                THM_MOVW_ABS_NC | THM_MOVT_ABS => sa,
                _ => pc,
            } as u32;
            let imm = if matches!(s.ty, THM_MOVT_ABS | THM_MOVT_PREL) {
                x >> 16
            } else {
                x & 0xffff
            };
            let (h1, h2) = (r16(b, at), r16(b, at + 2));
            w16(
                b,
                at,
                (h1 & 0xfbf0) | ((imm >> 12) & 0xf) | (((imm >> 11) & 1) << 10),
            );
            w16(
                b,
                at + 2,
                (h2 & 0x8f00) | (((imm >> 8) & 7) << 12) | (imm & 0xff),
            );
        }
        _ => return Err(format!("unsupported relocation {name}")),
    }
    Ok(dynrel)
}

/// A `b` / `b.w` between the instruction sets to a function: through a
/// thunk (`Some(from Thumb)`). `thumb`: the target is T32 code;
/// `plt`: it is a PLT entry (A32).
#[must_use]
pub const fn thunk(ty: u16, func: bool, thumb: bool, plt: bool) -> Option<bool> {
    match ty {
        JUMP24 | PC24 | PLT32 if func && thumb && !plt => Some(false),
        THM_JUMP24 if plt || (func && !thumb) => Some(true),
        _ => None,
    }
}

/// The thunk at `at` to `target` (bit 0: T32), position independent:
/// `movw ip / movt ip` of the distance, `add ip, pc`, `bx ip`; A32 or,
/// from Thumb, T32.
pub fn write_thunk(b: &mut [u8], at: usize, addr: u64, target: u64, from_thumb: bool) {
    if from_thumb {
        // pc reads 4 past the 16-bit `add` at +8.
        let d = target.wrapping_sub(addr + 12) as u32;
        let (lo, hi) = (d & 0xffff, d >> 16);
        let enc = |op: u32, imm: u32| -> [u32; 2] {
            [
                op | ((imm >> 12) & 0xf) | (((imm >> 11) & 1) << 10),
                (((imm >> 8) & 7) << 12) | (12 << 8) | (imm & 0xff),
            ]
        };
        let [a, b1] = enc(0xf240, lo);
        let [c, d1] = enc(0xf2c0, hi);
        for (k, h) in [a, b1, c, d1, 0x44fc, 0x4760, 0xbf00, 0xbf00]
            .iter()
            .enumerate()
        {
            w16(b, at + 2 * k, *h);
        }
        return;
    }
    // pc reads 8 past the `add` at +8.
    let d = target.wrapping_sub(addr + 16) as u32;
    w32(b, at, set_movw(0xe300_c000, d & 0xffff));
    w32(b, at + 4, set_movw(0xe340_c000, d >> 16));
    w32(b, at + 8, 0xe08c_c00f); // add ip, ip, pc
    w32(b, at + 12, 0xe12f_ff1c); // bx ip
}

/// PLT entry at `at` (address `addr`) jumping through GOT slot `slot`:
/// `add ip, pc, #hi; add ip, ip, #mid; ldr pc, [ip, #lo]!`, as lld's
/// short form; the long form past 2^27 bytes.
pub fn write_plt(b: &mut [u8], at: usize, addr: u64, slot: u64) {
    let off = slot.wrapping_sub(addr + 8) as u32;
    if u64::from(off) < 1 << 27 {
        w32(b, at, 0xe28f_c600 | ((off >> 20) & 0xff));
        w32(b, at + 4, 0xe28c_ca00 | ((off >> 12) & 0xff));
        w32(b, at + 8, 0xe5bc_f000 | (off & 0xfff));
        w32(b, at + 12, 0xe7ff_defe);
    } else {
        // movw ip, #lo; movt ip, #hi; add ip, ip, pc; ldr pc, [ip]
        let d = slot.wrapping_sub(addr + 16) as u32;
        w32(b, at, set_movw(0xe300_c000, d & 0xffff));
        w32(b, at + 4, set_movw(0xe340_c000, d >> 16));
        w32(b, at + 8, 0xe08c_c00f);
        w32(b, at + 12, 0xe59c_f000);
    }
}

/// A PREL31 field: `to - from`, bit 31 kept from `w`.
#[must_use]
pub const fn prel31(w: u32, from: u64, to: u64) -> u32 {
    (w & 0x8000_0000) | ((to.wrapping_sub(from) as u32) & 0x7fff_ffff)
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn site(ty: u16, func: bool) -> Site {
        Site {
            arch: crate::elf::Arch::Arm,
            ty,
            kind: SymKind::Local,
            func,
            alloc: true,
            pie: true,
            dyn_tls: false,
        }
    }

    fn vals(s: u64, a: i64, p: u64) -> Values {
        Values {
            s,
            a,
            p,
            ..Values::default()
        }
    }

    /// `bl` to T32 code becomes `blx` with H; T32 `bl` to A32 becomes
    /// `blx` aligned; both read back as their addends.
    #[test]
    fn calls_switch_instruction_sets() {
        let mut b = 0xebff_fffeu32.to_le_bytes().to_vec();
        assert_eq!(implicit_addend(&b, 0, CALL), -8);
        apply(
            site(CALL, true),
            &mut b,
            0,
            &vals(0x1007, -8, 0x1000),
            Dyn::None,
            "",
        )
        .unwrap();
        assert_eq!(r32(&b, 0), 0xfbff_ffff);
        let mut b = vec![0xff, 0xf7, 0xfe, 0xff];
        assert_eq!(implicit_addend(&b, 0, THM_CALL), -4);
        apply(
            site(THM_CALL, true),
            &mut b,
            0,
            &vals(0x2000, -4, 0x1002),
            Dyn::None,
            "",
        )
        .unwrap();
        // blx: bit 12 of the second halfword clear.
        assert_eq!(r16(&b, 2) & 0x1000, 0);
        assert_eq!((implicit_addend(&b, 0, THM_CALL) + 0x1004) & !3, 0x2000);
    }

    #[test]
    fn movw_movt_take_the_halves() {
        let mut b = 0xe30f_4ff0u32.to_le_bytes().to_vec();
        assert_eq!(implicit_addend(&b, 0, MOVW_PREL_NC), -16);
        apply(
            site(MOVW_PREL_NC, false),
            &mut b,
            0,
            &vals(0x12_3456, -16, 0x1000),
            Dyn::None,
            "",
        )
        .unwrap();
        assert_eq!(movw_imm(r32(&b, 0)), (0x12_3456 - 16 - 0x1000) & 0xffff);
        let mut b = vec![0x40, 0xf2, 0, 0];
        apply(
            site(THM_MOVW_ABS_NC, false),
            &mut b,
            0,
            &vals(0xabcd, 0, 0),
            Dyn::None,
            "",
        )
        .unwrap();
        assert_eq!(thm_movw_imm(r16(&b, 0), r16(&b, 2)), 0xabcd);
    }
}
