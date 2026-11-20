//! 32-bit ARM instruction encoder (A32, `VFPv3`, NEON): one function per
//! instruction form the emitter uses, the only place that knows opcodes
//! and bit fields. Each returns an `Ins`: the word and its form. `Asm`
//! keeps the words and, with text on (`--emit=asm`), each word's form;
//! `lines` decodes them into what `llvm-objdump -d` prints (UAL, its
//! preferred aliases, decimal data-processing immediates, hex offsets).
//! Off, only the words are kept. No Thumb: every word is A32.

#[cfg(test)]
mod tests;
mod text;

pub(crate) use crate::isa::arm32::enc::text::*;

use crate::asm::Listing;

/// A core register, 0 .. 15.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct R(pub u32);

/// A VFP register, `s` or `d` by the instruction's precision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct V(pub u32);

/// A `d` register, 0 .. 31; a `q` operand as its first `d` (`q1` is
/// `D(2)`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct D(pub u32);

/// An `s` register, 0 .. 31.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct S(pub u32);

pub const R0: R = R(0);
pub const SP: R = R(13);
pub const LR: R = R(14);
pub const PC: R = R(15);

/// A condition code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Cond(u32);

impl Cond {
    pub const EQ: Self = Self(0);
    pub const NE: Self = Self(1);
    pub const HS: Self = Self(2);
    pub const LO: Self = Self(3);
    pub const MI: Self = Self(4);
    pub const VS: Self = Self(6);
    pub const HI: Self = Self(8);
    pub const LS: Self = Self(9);
    pub const GE: Self = Self(10);
    pub const LT: Self = Self(11);
    pub const GT: Self = Self(12);
    pub const LE: Self = Self(13);
    pub const AL: Self = Self(14);

    const fn top(self) -> u32 {
        self.0 << 28
    }

    /// The opposite condition (not of `AL`).
    #[must_use]
    pub const fn inv(self) -> Self {
        Self(self.0 ^ 1)
    }
}

const COND: [&str; 16] = [
    "eq", "ne", "hs", "lo", "mi", "pl", "vs", "vc", "hi", "ls", "ge", "lt", "gt", "le", "", "",
];

/// The form of a word, for its text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum F {
    Dp,
    DpImm,
    MovW,
    Ls,
    LsH,
    B,
    Blx,
    Word,
    Stack,
    Mul,
    Misc,
    Excl,
    Sys,
    Mrc,
    Vfp,
    VmovS,
    VmovLane,
    VmovD,
    NeonMisc,
    Vld,
    Vmovl,
    Vmovn,
    VmovImm,
    Neon3,
    NeonShift,
}

/// An encoded instruction.
#[derive(Clone, Copy, Debug)]
pub struct Ins {
    word: u32,
    f: F,
}

const fn ins(word: u32, f: F) -> Ins {
    Ins { word, f }
}

// Data processing.

/// A data-processing opcode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Op {
    And = 0,
    Eor = 1,
    Sub = 2,
    Rsb = 3,
    Add = 4,
    Adc = 5,
    Sbc = 6,
    Rsc = 7,
    Cmp = 10,
    Cmn = 11,
    Orr = 12,
    Mov = 13,
    Bic = 14,
    Mvn = 15,
}

/// A shift of a register operand.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Shift {
    Lsl = 0,
    Lsr = 1,
    Asr = 2,
    /// Not by 0 (that is `rrx`).
    Ror = 3,
}

/// A modified immediate's encoding: 8 bits rotated right by twice the
/// 4 above them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Imm(pub(crate) u32);

impl Imm {
    /// `v` (below 256) unrotated.
    #[must_use]
    pub const fn small(v: u32) -> Self {
        Self(v)
    }
}

/// `v` as a modified immediate, the least rotation that reaches it.
#[must_use]
pub fn modimm(v: u32) -> Option<Imm> {
    (0..16u32).find_map(|r| {
        let x = v.rotate_left(2 * r);
        (x <= 0xff).then_some(Imm((r << 8) | x))
    })
}

const fn dp_word(cond: Cond, op: Op, s: bool, rd: R, rn: R) -> u32 {
    cond.top() | ((op as u32) << 21) | ((s as u32) << 20) | (rn.0 << 16) | (rd.0 << 12)
}

/// `op{s}{cond} rd, rn, rm` (`mov` / `mvn`: `rn` 0; `cmp` / `cmn`: `rd`
/// 0 and `s`).
#[must_use]
pub const fn dp(cond: Cond, op: Op, s: bool, rd: R, rn: R, rm: R) -> Ins {
    ins(dp_word(cond, op, s, rd, rn) | rm.0, F::Dp)
}

/// `op{s}{cond} rd, rn, #imm`.
#[must_use]
pub const fn dp_imm(cond: Cond, op: Op, s: bool, rd: R, rn: R, imm: Imm) -> Ins {
    ins(dp_word(cond, op, s, rd, rn) | (1 << 25) | imm.0, F::DpImm)
}

/// `op{s}{cond} rd, rn, rm, <sh> #n` (`n` taken mod 32: `lsr` / `asr`
/// of 0 would be one of 32).
#[must_use]
#[allow(clippy::too_many_arguments)]
pub const fn dp_shift(cond: Cond, op: Op, s: bool, rd: R, rn: R, rm: R, sh: Shift, n: u32) -> Ins {
    let w = dp_word(cond, op, s, rd, rn) | ((n & 31) << 7) | ((sh as u32) << 5);
    ins(w | rm.0, F::Dp)
}

/// `op{s}{cond} rd, rn, rm, <sh> rs` (a register-specified shift).
#[must_use]
#[allow(clippy::too_many_arguments)]
pub const fn dp_rsr(cond: Cond, op: Op, s: bool, rd: R, rn: R, rm: R, sh: Shift, rs: R) -> Ins {
    let w = dp_word(cond, op, s, rd, rn) | (rs.0 << 8) | ((sh as u32) << 5) | (1 << 4);
    ins(w | rm.0, F::Dp)
}

/// `mov rd, rm`.
#[must_use]
pub const fn mov(rd: R, rm: R) -> Ins {
    dp(Cond::AL, Op::Mov, false, rd, R0, rm)
}

/// `movw{cond} rd, #imm` (16 bits).
#[must_use]
pub const fn movw(cond: Cond, rd: R, imm: u32) -> Ins {
    let w = cond.top() | 0x0300_0000 | ((imm >> 12) << 16) | (rd.0 << 12) | (imm & 0xfff);
    ins(w, F::MovW)
}

/// `movt{cond} rd, #imm` (16 bits).
#[must_use]
pub const fn movt(cond: Cond, rd: R, imm: u32) -> Ins {
    let w = cond.top() | 0x0340_0000 | ((imm >> 12) << 16) | (rd.0 << 12) | (imm & 0xfff);
    ins(w, F::MovW)
}

// Loads and stores.

/// A load or store.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Acc {
    Ldr,
    Str,
    Ldrb,
    Strb,
    Ldrh,
    Strh,
    Ldrsb,
    Ldrsh,
}

impl Acc {
    /// The word with an added immediate offset of 0 and whether it is
    /// the 8-bit offset form.
    const fn enc(self) -> (u32, bool) {
        match self {
            Self::Ldr => (0xe590_0000, false),
            Self::Str => (0xe580_0000, false),
            Self::Ldrb => (0xe5d0_0000, false),
            Self::Strb => (0xe5c0_0000, false),
            Self::Ldrh => (0xe1d0_00b0, true),
            Self::Strh => (0xe1c0_00b0, true),
            Self::Ldrsb => (0xe1d0_00d0, true),
            Self::Ldrsh => (0xe1d0_00f0, true),
        }
    }

    /// The largest offset magnitude it reaches.
    #[must_use]
    pub const fn max(self) -> u32 {
        if self.enc().1 { 255 } else { 4095 }
    }
}

/// `a rt, [rn, #off]` (`off` within `a.max()`).
#[must_use]
pub const fn ls(a: Acc, rt: R, rn: R, off: i32) -> Ins {
    let (enc, short) = a.enc();
    let m = off.unsigned_abs();
    let u = if off >= 0 { 1 << 23 } else { 0 };
    let w = (enc & !(1 << 23)) | u | (rn.0 << 16) | (rt.0 << 12);
    if short {
        ins(w | ((m >> 4) << 8) | (m & 0xf), F::LsH)
    } else {
        ins(w | m, F::Ls)
    }
}

/// `ldr` / `str rt, [rn], #4`: post-incremented.
#[must_use]
pub const fn ls_post4(load: bool, rt: R, rn: R) -> Ins {
    let op = if load { 0xe490_0004 } else { 0xe480_0004 };
    ins(op | (rn.0 << 16) | (rt.0 << 12), F::Ls)
}

/// A literal word in the code (as objdump shows data: bytes, `.word`).
#[must_use]
pub const fn word(v: u32) -> Ins {
    ins(v, F::Word)
}

/// `push {list}` (`stmdb sp!`), `list` a register mask.
#[must_use]
pub const fn push(list: u32) -> Ins {
    ins(0xe92d_0000 | (list & 0xffff), F::Stack)
}

/// `pop {list}` (`ldm sp!`).
#[must_use]
pub const fn pop(list: u32) -> Ins {
    ins(0xe8bd_0000 | (list & 0xffff), F::Stack)
}

// Branches.

/// `b{cond}` with a word offset of 0, for `patch24`.
#[must_use]
pub const fn b(cond: Cond) -> Ins {
    b_to(cond, 0)
}

/// `b{cond}` to `d` words past the instruction after the next (`pc` reads
/// 8 bytes ahead).
#[must_use]
pub const fn b_to(cond: Cond, d: i64) -> Ins {
    ins(cond.top() | 0x0a00_0000 | ((d as u32) & 0x00ff_ffff), F::B)
}

/// `bl` with the implicit addend of `R_ARM_CALL`: -8 (to itself).
#[must_use]
pub const fn bl() -> Ins {
    ins(0xebff_fffe, F::B)
}

/// `blx rm`.
#[must_use]
pub const fn blx(rm: R) -> Ins {
    ins(0xe12f_ff30 | rm.0, F::Blx)
}

/// `bx rm`.
#[must_use]
pub const fn bx(rm: R) -> Ins {
    ins(0xe12f_ff10 | rm.0, F::Blx)
}

/// Point the branch word `w` `d` words on.
#[must_use]
pub const fn patch24(w: u32, d: i64) -> u32 {
    w | ((d as u32) & 0x00ff_ffff)
}

// Multiplies and the like.

/// `mul rd, rm, rs`.
#[must_use]
pub const fn mul(rd: R, rm: R, rs: R) -> Ins {
    ins(0xe000_0090 | (rd.0 << 16) | (rs.0 << 8) | rm.0, F::Mul)
}

/// `mla rd, rm, rs, ra`.
#[must_use]
pub const fn mla(rd: R, rm: R, rs: R, ra: R) -> Ins {
    ins(
        0xe020_0090 | (rd.0 << 16) | (ra.0 << 12) | (rs.0 << 8) | rm.0,
        F::Mul,
    )
}

/// `smull` / `umull lo, hi, rm, rs`.
#[must_use]
pub const fn mull(signed: bool, lo: R, hi: R, rm: R, rs: R) -> Ins {
    let op = if signed { 0xe0c0_0090 } else { 0xe080_0090 };
    ins(
        op | (hi.0 << 16) | (lo.0 << 12) | (rs.0 << 8) | rm.0,
        F::Mul,
    )
}

/// `sxtb` / `uxtb` / `sxth` / `uxth rd, rm` (`half`: 16 bits).
#[must_use]
pub const fn ext(signed: bool, half: bool, rd: R, rm: R) -> Ins {
    let op = match (half, signed) {
        (false, true) => 0xe6af_0070,
        (false, false) => 0xe6ef_0070,
        (true, true) => 0xe6bf_0070,
        (true, false) => 0xe6ff_0070,
    };
    ins(op | (rd.0 << 12) | rm.0, F::Misc)
}

/// `clz rd, rm`.
#[must_use]
pub const fn clz(rd: R, rm: R) -> Ins {
    ins(0xe16f_0f10 | (rd.0 << 12) | rm.0, F::Misc)
}

/// `rbit rd, rm`.
#[must_use]
pub const fn rbit(rd: R, rm: R) -> Ins {
    ins(0xe6ff_0f30 | (rd.0 << 12) | rm.0, F::Misc)
}

/// `rev rd, rm`.
#[must_use]
pub const fn rev(rd: R, rm: R) -> Ins {
    ins(0xe6bf_0f30 | (rd.0 << 12) | rm.0, F::Misc)
}

/// `rev16 rd, rm`.
#[must_use]
pub const fn rev16(rd: R, rm: R) -> Ins {
    ins(0xe6bf_0fb0 | (rd.0 << 12) | rm.0, F::Misc)
}

// Exclusives and barriers.

/// `ldrex{b,h} rt, [rn]` of `bytes` (1, 2, 4).
#[must_use]
pub const fn ldrex(bytes: u32, rt: R, rn: R) -> Ins {
    let op = match bytes {
        1 => 0xe1d0_0f9f,
        2 => 0xe1f0_0f9f,
        _ => 0xe190_0f9f,
    };
    ins(op | (rn.0 << 16) | (rt.0 << 12), F::Excl)
}

/// `strex{b,h} rd, rt, [rn]` of `bytes` (1, 2, 4).
#[must_use]
pub const fn strex(bytes: u32, rd: R, rt: R, rn: R) -> Ins {
    let op = match bytes {
        1 => 0xe1c0_0f90,
        2 => 0xe1e0_0f90,
        _ => 0xe180_0f90,
    };
    ins(op | (rn.0 << 16) | (rd.0 << 12) | rt.0, F::Excl)
}

/// `ldrexd rt, rt+1, [rn]`.
#[must_use]
pub const fn ldrexd(rt: R, rn: R) -> Ins {
    ins(0xe1b0_0f9f | (rn.0 << 16) | (rt.0 << 12), F::Excl)
}

/// `strexd rd, rt, rt+1, [rn]`.
#[must_use]
pub const fn strexd(rd: R, rt: R, rn: R) -> Ins {
    ins(0xe1a0_0f90 | (rn.0 << 16) | (rd.0 << 12) | rt.0, F::Excl)
}

/// `clrex`.
#[must_use]
pub const fn clrex() -> Ins {
    ins(0xf57f_f01f, F::Sys)
}

/// `dmb ish`.
#[must_use]
pub const fn dmb_ish() -> Ins {
    ins(0xf57f_f05b, F::Sys)
}

/// `trap` (`udf #0xfdee`: the one LLVM writes for Linux).
#[must_use]
pub const fn trap() -> Ins {
    ins(0xe7ff_defe, F::Sys)
}

/// `vmrs APSR_nzcv, fpscr`.
#[must_use]
pub const fn vmrs() -> Ins {
    ins(0xeef1_fa10, F::Sys)
}

/// `mrc p15, 0, rt, c13, c0, 3`: the thread pointer (`TPIDRURO`).
#[must_use]
pub const fn mrc_tp(rt: R) -> Ins {
    ins(0xee1d_0f70 | (rt.0 << 12), F::Mrc)
}

// VFP.

/// `(Vx, x)` fields of VFP register `n`: `d` (`dbl`) or `s`.
const fn vreg(n: u32, dbl: bool) -> (u32, u32) {
    if dbl {
        (n & 15, n >> 4)
    } else {
        (n >> 1, n & 1)
    }
}

/// A VFP data-processing operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum Vop {
    Add = 0xee30_0a00,
    Sub = 0xee30_0a40,
    Mul = 0xee20_0a00,
    Div = 0xee80_0a00,
    /// `vfma d, n, m`: `d += n * m`, rounded once (`vfp4`).
    Fma = 0xeea0_0a00,
    Sqrt = 0xeeb1_0ac0,
    Neg = 0xeeb1_0a40,
    Cmp = 0xeeb4_0a40,
    Mov = 0xeeb0_0a40,
}

/// `op.f64` / `.f32 d, n, m` (`dbl`: `d` registers); the one-operand
/// ones take `n` 0.
#[must_use]
pub const fn vfp(op: Vop, dbl: bool, d: V, n: V, m: V) -> Ins {
    let (vd, bd) = vreg(d.0, dbl);
    let (vn, bn) = vreg(n.0, dbl);
    let (vm, bm) = vreg(m.0, dbl);
    let w = (op as u32) | ((dbl as u32) << 8) | (bd << 22) | (vn << 16) | (vd << 12);
    ins(w | (bn << 7) | (bm << 5) | vm, F::Vfp)
}

/// `vcmp.f64 d, #0`.
#[must_use]
pub const fn vcmp_zero(d: D) -> Ins {
    let (vd, bd) = vreg(d.0, true);
    ins(0xeeb5_0b40 | (bd << 22) | (vd << 12), F::Vfp)
}

/// `vcvt.f64` / `.f32 .s32` / `.u32 d, m`: integer to float.
#[must_use]
pub const fn vcvt_from_int(dbl: bool, signed: bool, d: V, m: S) -> Ins {
    let (vd, bd) = vreg(d.0, dbl);
    let (vm, bm) = vreg(m.0, false);
    let w = 0xeeb8_0a40 | ((dbl as u32) << 8) | ((signed as u32) << 7);
    ins(w | (bd << 22) | (vd << 12) | (bm << 5) | vm, F::Vfp)
}

/// `vcvt.s32` / `.u32 .f64` / `.f32 d, m`: float to integer, toward 0.
#[must_use]
pub const fn vcvt_to_int(dbl: bool, signed: bool, d: S, m: V) -> Ins {
    let (vd, bd) = vreg(d.0, false);
    let (vm, bm) = vreg(m.0, dbl);
    let w = 0xeebc_0ac0 | ((dbl as u32) << 8) | ((signed as u32) << 16);
    ins(w | (bd << 22) | (vd << 12) | (bm << 5) | vm, F::Vfp)
}

/// `vcvt.f64.f32 d, m`.
#[must_use]
pub const fn vcvt_f64_f32(d: D, m: S) -> Ins {
    let (vd, bd) = vreg(d.0, true);
    let (vm, bm) = vreg(m.0, false);
    ins(
        0xeeb7_0ac0 | (bd << 22) | (vd << 12) | (bm << 5) | vm,
        F::Vfp,
    )
}

/// `vcvt.f32.f64 d, m`.
#[must_use]
pub const fn vcvt_f32_f64(d: S, m: D) -> Ins {
    let (vd, bd) = vreg(d.0, false);
    let (vm, bm) = vreg(m.0, true);
    ins(
        0xeeb7_0bc0 | (bd << 22) | (vd << 12) | (bm << 5) | vm,
        F::Vfp,
    )
}

/// `vmov r, s` (`to_core`) / `vmov s, r`.
#[must_use]
pub const fn vmov_s(to_core: bool, s: S, r: R) -> Ins {
    let (vn, n) = vreg(s.0, false);
    let op = if to_core { 0xee10_0a10 } else { 0xee00_0a10 };
    ins(op | (vn << 16) | (r.0 << 12) | (n << 7), F::VmovS)
}

/// `vmov.32 r, d[0]` (`to_core`) / `vmov.32 d[0], r`.
#[must_use]
pub const fn vmov_lane(to_core: bool, d: D, r: R) -> Ins {
    let (vd, x) = vreg(d.0, true);
    let op = if to_core { 0xee10_0b10 } else { 0xee00_0b10 };
    ins(op | (vd << 16) | (r.0 << 12) | (x << 7), F::VmovLane)
}

/// `vmov lo, hi, d` (`to_core`) / `vmov d, lo, hi`.
#[must_use]
pub const fn vmov_d(to_core: bool, d: D, lo: R, hi: R) -> Ins {
    let (vm, m) = vreg(d.0, true);
    let op = if to_core { 0xec50_0b10 } else { 0xec40_0b10 };
    ins(op | (hi.0 << 16) | (lo.0 << 12) | (m << 5) | vm, F::VmovD)
}

// NEON.

/// `Vd` (bits 15:12, `D` bit 22) of `d`.
const fn fd(d: D) -> u32 {
    ((d.0 & 15) << 12) | ((d.0 >> 4) << 22)
}

/// `Vn` (bits 19:16, `N` bit 7).
const fn fnn(d: D) -> u32 {
    ((d.0 & 15) << 16) | ((d.0 >> 4) << 7)
}

/// `Vm` (bits 3:0, `M` bit 5).
const fn fm(d: D) -> u32 {
    (d.0 & 15) | ((d.0 >> 4) << 5)
}

/// `vcnt.8 d, m`.
#[must_use]
pub const fn vcnt8(d: D, m: D) -> Ins {
    ins(0xf3b0_0500 | fd(d) | fm(m), F::NeonMisc)
}

/// `vpaddl.u<8 << size> d, m`.
#[must_use]
pub const fn vpaddl_u(size: u32, d: D, m: D) -> Ins {
    ins(0xf3b0_0280 | (size << 18) | fd(d) | fm(m), F::NeonMisc)
}

/// The register `rn` of a `vld1` / `vst1` (no write-back).
const fn vls(op: u32, rn: R, d: D) -> u32 {
    op | (rn.0 << 16) | fd(d)
}

/// `vld1.8` (`load`) / `vst1.8 {d, d+1}, [rn]`: a whole `q`.
#[must_use]
pub const fn vls_q(load: bool, d: D, rn: R) -> Ins {
    let op = if load { 0xf420_0a0f } else { 0xf400_0a0f };
    ins(vls(op, rn, d), F::Vld)
}

/// `vld1.8` / `vst1.8 {d}, [rn]`.
#[must_use]
pub const fn vls_d(load: bool, d: D, rn: R) -> Ins {
    let op = if load { 0xf420_070f } else { 0xf400_070f };
    ins(vls(op, rn, d), F::Vld)
}

/// `vld1.32` / `vst1.32 {d[0]}, [rn]`.
#[must_use]
pub const fn vls_lane32(load: bool, d: D, rn: R) -> Ins {
    let op = if load { 0xf4a0_080f } else { 0xf480_080f };
    ins(vls(op, rn, d), F::Vld)
}

/// `vld1.64 {d}, [rn]`.
#[must_use]
pub const fn vld1_64(d: D, rn: R) -> Ins {
    ins(vls(0xf420_07cf, rn, d), F::Vld)
}

/// `vld1.<8 << size> {d[], d+1[]}, [rn]`: one element to every lane.
#[must_use]
pub const fn vld1_dup(size: u32, d: D, rn: R) -> Ins {
    ins(vls(0xf4a0_0c2f | (size << 6), rn, d), F::Vld)
}

/// `vmovl.s` / `.u<bits> q, d`: each lane widened.
#[must_use]
pub const fn vmovl(signed: bool, bits: u32, d: D, m: D) -> Ins {
    let u = if signed { 0xf280_0a10 } else { 0xf380_0a10 };
    ins(u | (bits << 16) | fd(d) | fm(m), F::Vmovl)
}

/// `vmovn.i<16 << size> d, q`: each lane narrowed to `8 << size` bits.
#[must_use]
pub const fn vmovn(size: u32, d: D, m: D) -> Ins {
    ins(0xf3b2_0200 | (size << 18) | fd(d) | fm(m), F::Vmovn)
}

/// `vmov.i32 q, #0`.
#[must_use]
pub const fn vmov_zero(d: D) -> Ins {
    ins(0xf280_0050 | fd(d), F::VmovImm)
}

/// `vmov.i8 q, #0xff`.
#[must_use]
pub const fn vmov_ones(d: D) -> Ins {
    ins(0xf387_0e5f | fd(d), F::VmovImm)
}

/// A NEON three-register operation (on `q` registers but `vorr_d`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum N3 {
    Add = 0xf200_0840,
    Sub = 0xf300_0840,
    Mul = 0xf200_0950,
    And = 0xf200_0150,
    Orr = 0xf220_0150,
    Eor = 0xf300_0150,
    /// `vbic d, n, m`: `n & !m`.
    Bic = 0xf210_0150,
    Ceq = 0xf300_0850,
    CgtS = 0xf200_0340,
    CgtU = 0xf300_0340,
    MinS = 0xf200_0650,
    MinU = 0xf300_0650,
    MaxS = 0xf200_0640,
    MaxU = 0xf300_0640,
}

impl N3 {
    /// Whether the lane size is a field (not of the bitwise ones).
    const fn sized(self) -> bool {
        !matches!(self, Self::And | Self::Orr | Self::Eor | Self::Bic)
    }
}

/// `op.<lanes of 8 << size> q d, n, m`.
#[must_use]
pub const fn neon3(op: N3, size: u32, d: D, n: D, m: D) -> Ins {
    let sz = if op.sized() { size << 20 } else { 0 };
    ins((op as u32) | sz | fd(d) | fnn(n) | fm(m), F::Neon3)
}

/// `vorr d, n, m` on `d` registers.
#[must_use]
pub const fn vorr_d(d: D, n: D, m: D) -> Ins {
    ins(0xf220_0110 | fd(d) | fnn(n) | fm(m), F::Neon3)
}

/// A NEON shift by an immediate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum NShift {
    Shl = 0xf280_0550,
    ShrS = 0xf280_0050,
    ShrU = 0xf380_0050,
}

/// `vshl.i` / `vshr.s` / `.u<8 w> q d, a, #n` on lanes of `w` bytes
/// (`n` 1 .. 8 w for a right shift).
#[must_use]
pub const fn vshift(op: NShift, w: u32, d: D, a: D, n: u32) -> Ins {
    let bits = 8 * w;
    // imm6 and L: the lane size and the shift.
    let imm6 = match op {
        NShift::Shl if w == 8 => n,
        NShift::Shl => bits + n,
        _ if w == 8 => 64 - n,
        _ => 2 * bits - n,
    };
    let w = (op as u32) | ((imm6 & 63) << 16) | (((w == 8) as u32) << 7);
    ins(w | fd(d) | fm(a), F::NeonShift)
}

/// The code of a function in the making.
#[derive(Default)]
pub struct Asm {
    pub code: Vec<u32>,
    /// Each word's form, with text on.
    text: Option<Vec<F>>,
}

impl Asm {
    /// `text`: record each word's form for `lines`.
    #[must_use]
    pub fn new(text: bool) -> Self {
        Self {
            code: Vec::new(),
            text: text.then(Vec::new),
        }
    }

    /// The index of the next word.
    #[must_use]
    pub fn pos(&self) -> usize {
        self.code.len()
    }

    #[inline]
    pub fn push(&mut self, i: Ins) {
        if let Some(t) = &mut self.text {
            t.push(i.f);
        }
        self.code.push(i.word);
    }

    /// Point the branch at `at` `d` words past the one after it.
    pub fn patch24(&mut self, at: usize, d: i64) {
        self.code[at] = patch24(self.code[at], d);
    }

    /// Each recorded word's byte offset and text.
    #[must_use]
    pub fn lines(&self, l: &Listing<'_>) -> Vec<(u32, String)> {
        let Some(t) = &self.text else {
            return Vec::new();
        };
        t.iter()
            .zip(&self.code)
            .enumerate()
            .map(|(n, (f, w))| {
                let at = 4 * n as i64;
                (at as u32, text(*w, *f, at, l))
            })
            .collect()
    }
}
