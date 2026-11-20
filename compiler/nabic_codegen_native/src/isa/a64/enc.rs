//! arm64 instruction encoder: one function per instruction form the
//! emitter uses, the only place that knows opcodes and bit fields. Each
//! returns an `Ins`: the word and its form. `Asm` keeps the words and,
//! with text on (`--emit=asm`), each word's form; `lines` decodes them
//! into what `llvm-objdump -d` prints (its preferred aliases, hex
//! immediates). Off, only the words are kept.

#[cfg(test)]
mod tests;
mod text;

pub(crate) use crate::isa::a64::enc::text::*;

use crate::asm::Listing;

/// A general-purpose register, 0 .. 30; 31 is `sp` or `xzr` by operand.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct R(pub u32);

/// A SIMD / FP register, 0 .. 31.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct V(pub u32);

pub const SP: R = R(31);
pub const ZR: R = R(31);

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

    /// The opposite condition.
    #[must_use]
    pub const fn inv(self) -> Self {
        Self(self.0 ^ 1)
    }
}

const COND: [&str; 16] = [
    "eq", "ne", "hs", "lo", "mi", "pl", "vs", "vc", "hi", "ls", "ge", "lt", "gt", "le", "al", "nv",
];

/// The form of a word, for its text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum F {
    MovWide,
    AddImm,
    AddReg,
    AddExt,
    LogReg,
    LogImm,
    Bitfield,
    Extr,
    Dp1,
    Dp2,
    Dp3,
    CondSel,
    B,
    BCond,
    Cbz,
    Blr,
    Ret,
    LdSt,
    Pair,
    Excl,
    Lse,
    Cas,
    Clrex,
    Nop,
    Mrs,
    Brk,
    Adrp,
    FmovGpr,
    Fp,
    Fp3,
    Cvtf,
    Fcvtz,
    Neon3,
    NeonMisc,
    NeonShift,
    Movi,
    Dup,
    Umov,
    NeonWide,
    Lane,
}

/// An encoded instruction.
#[derive(Clone, Copy, Debug)]
pub struct Ins {
    word: u32,
    f: F,
}

impl Ins {
    #[must_use]
    pub const fn word(self) -> u32 {
        self.word
    }

    /// The register in bits 0..5: the destination of all but stores
    /// (their data), branches and compares (no register).
    #[must_use]
    pub const fn dst(self) -> u32 {
        self.word & 31
    }
}

const fn ins(word: u32, f: F) -> Ins {
    Ins { word, f }
}

const fn sf(x: bool) -> u32 {
    (x as u32) << 31
}

// Moves.

/// `movz xd, #imm, lsl #(16 hw)`.
#[must_use]
pub const fn movz(rd: R, imm: u32, hw: u32) -> Ins {
    ins(0xd280_0000 | (hw << 21) | (imm << 5) | rd.0, F::MovWide)
}

/// `movz wd, #imm`.
#[must_use]
pub const fn movz_w(rd: R, imm: u32) -> Ins {
    ins(0x5280_0000 | (imm << 5) | rd.0, F::MovWide)
}

/// `movk xd, #imm, lsl #(16 hw)`.
#[must_use]
pub const fn movk(rd: R, imm: u32, hw: u32) -> Ins {
    ins(0xf280_0000 | (hw << 21) | (imm << 5) | rd.0, F::MovWide)
}

/// `movn xd, #imm, lsl #(16 hw)`.
#[must_use]
pub const fn movn(rd: R, imm: u32, hw: u32) -> Ins {
    ins(0x9280_0000 | (hw << 21) | (imm << 5) | rd.0, F::MovWide)
}

// Add / subtract.

/// Add / subtract, flags set or not.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AddOp {
    Add = 0,
    Adds = 1,
    Sub = 2,
    Subs = 3,
}

/// `op rd, rn, #imm{, lsl #12}` (`x`: 64-bit; `sp` for `rn`, and for
/// `rd` without flags).
#[must_use]
pub const fn add_imm(op: AddOp, x: bool, rd: R, rn: R, imm: u32, lsl12: bool) -> Ins {
    let w = 0x1100_0000 | sf(x) | ((op as u32) << 29) | ((lsl12 as u32) << 22);
    ins(w | (imm << 10) | (rn.0 << 5) | rd.0, F::AddImm)
}

/// A shift of a register operand.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Shift {
    Lsl = 0,
    Lsr = 1,
    Asr = 2,
}

/// `op rd, rn, rm{, shift #n}`.
#[must_use]
pub const fn add_reg(op: AddOp, x: bool, rd: R, rn: R, rm: R, sh: Shift, n: u32) -> Ins {
    let w = 0x0b00_0000 | sf(x) | ((op as u32) << 29) | ((sh as u32) << 22);
    ins(w | (rm.0 << 16) | (n << 10) | (rn.0 << 5) | rd.0, F::AddReg)
}

/// An extension of a register operand.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Ext {
    Uxtb = 0,
    Uxth = 1,
    Uxtw = 2,
    Uxtx = 3,
    Sxtb = 4,
    Sxth = 5,
    Sxtw = 6,
}

/// `op rd, rn, rm, ext` (`sp` for `rn`, and for `rd` without flags).
#[must_use]
pub const fn add_ext(op: AddOp, x: bool, rd: R, rn: R, rm: R, ext: Ext) -> Ins {
    let w = 0x0b20_0000 | sf(x) | ((op as u32) << 29) | ((ext as u32) << 13);
    ins(w | (rm.0 << 16) | (rn.0 << 5) | rd.0, F::AddExt)
}

/// `cmp rn, rm` (`subs zr, rn, rm`).
#[must_use]
pub const fn cmp(x: bool, rn: R, rm: R) -> Ins {
    add_reg(AddOp::Subs, x, ZR, rn, rm, Shift::Lsl, 0)
}

/// `cmp` / `cmn rn, #imm`: `imm` negative compares by `cmn`.
#[must_use]
pub const fn cmp_imm(x: bool, rn: R, imm: i64) -> Ins {
    let op = if imm < 0 { AddOp::Adds } else { AddOp::Subs };
    add_imm(op, x, ZR, rn, imm.unsigned_abs() as u32, false)
}

// Logical.

/// A logical operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Log {
    And = 0,
    Orr = 1,
    Eor = 2,
    Ands = 3,
}

/// `op rd, rn, rm` (`not`: `bic` / `orn` / `eon` / `bics`, `rm`
/// inverted).
#[must_use]
pub const fn log_reg(op: Log, not: bool, x: bool, rd: R, rn: R, rm: R) -> Ins {
    let w = 0x0a00_0000 | sf(x) | ((op as u32) << 29) | ((not as u32) << 21);
    ins(w | (rm.0 << 16) | (rn.0 << 5) | rd.0, F::LogReg)
}

/// `mov rd, rm` (`orr rd, zr, rm`).
#[must_use]
pub const fn mov(x: bool, rd: R, rm: R) -> Ins {
    log_reg(Log::Orr, false, x, rd, ZR, rm)
}

/// `N:immr:imms` of a logical immediate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Bitmask(u32);

/// `v` as a logical immediate of `bits` (32 or 64): a rotated run of
/// ones, repeated in 2..64-bit elements.
#[must_use]
pub fn bitmask(v: u64, bits: u32) -> Option<Bitmask> {
    let v = if bits == 32 {
        (v & 0xffff_ffff) | (v << 32)
    } else {
        v
    };
    if v == 0 || v == u64::MAX {
        return None;
    }
    let mut size = 64u32;
    while size > 2 {
        let half = size / 2;
        let m = (1u64 << half) - 1;
        if v & m != (v >> half) & m {
            break;
        }
        size = half;
    }
    let mask = if size == 64 {
        u64::MAX
    } else {
        (1u64 << size) - 1
    };
    let e = v & mask;
    let ones = e.count_ones();
    let run = (1u64 << ones) - 1;
    let rotr = |x: u64, r: u32| {
        if r == 0 {
            x
        } else {
            ((x >> r) | (x << (size - r))) & mask
        }
    };
    let r = (0..size).find(|&r| rotr(e, r) == run)?;
    let immr = (size - r) % size;
    let imms = ((!(size - 1) << 1) & 0x3f) | (ones - 1);
    Some(Bitmask((u32::from(size == 64) << 12) | (immr << 6) | imms))
}

/// `op rd, rn, #imm` (`sp` for `rd` without flags).
#[must_use]
pub const fn log_imm(op: Log, x: bool, rd: R, rn: R, imm: Bitmask) -> Ins {
    let w = 0x1200_0000 | sf(x) | ((op as u32) << 29);
    ins(w | (imm.0 << 10) | (rn.0 << 5) | rd.0, F::LogImm)
}

// Bit fields.

/// `sbfm` / `ubfm rd, rn, #immr, #imms`.
#[must_use]
pub const fn bfm(signed: bool, x: bool, rd: R, rn: R, immr: u32, imms: u32) -> Ins {
    let w = if signed { 0x1300_0000 } else { 0x5300_0000 } | sf(x) | ((x as u32) << 22);
    ins(
        w | (immr << 16) | (imms << 10) | (rn.0 << 5) | rd.0,
        F::Bitfield,
    )
}

/// `extr rd, rn, rm, #lsb`: bits `lsb` up of `rm:rn`.
#[must_use]
pub const fn extr(x: bool, rd: R, rn: R, rm: R, lsb: u32) -> Ins {
    let w = 0x1380_0000 | sf(x) | ((x as u32) << 22);
    ins(w | (rm.0 << 16) | (lsb << 10) | (rn.0 << 5) | rd.0, F::Extr)
}

/// `ror rd, rn, #n` (`extr rd, rn, rn, #n`).
#[must_use]
pub const fn ror(x: bool, rd: R, rn: R, n: u32) -> Ins {
    extr(x, rd, rn, rn, n)
}

/// `lsl rd, rn, #n`.
#[must_use]
pub const fn lsl(x: bool, rd: R, rn: R, n: u32) -> Ins {
    let bits = if x { 64 } else { 32 };
    bfm(false, x, rd, rn, (bits - n) % bits, bits - 1 - n)
}

/// `lsr` / `asr` (`signed`) `rd, rn, #n`.
#[must_use]
pub const fn shr(signed: bool, x: bool, rd: R, rn: R, n: u32) -> Ins {
    bfm(signed, x, rd, rn, n, if x { 63 } else { 31 })
}

/// `sxtb` / `sxth` / `sxtw xd, wn` (`bytes` 1, 2, 4).
#[must_use]
pub const fn sxt(bytes: u32, rd: R, rn: R) -> Ins {
    bfm(true, true, rd, rn, 0, 8 * bytes - 1)
}

/// `uxtb` / `uxth wd, wn` (`bytes` 1, 2).
#[must_use]
pub const fn uxt(bytes: u32, rd: R, rn: R) -> Ins {
    bfm(false, false, rd, rn, 0, 8 * bytes - 1)
}

// Data processing.

/// One-source operations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Dp1 {
    Rbit = 0,
    Rev16 = 1,
    /// `rev` of a `w` register (`rev32` of an `x`).
    Rev32 = 2,
    Rev64 = 3,
    Clz = 4,
}

/// `op rd, rn`.
#[must_use]
pub const fn dp1(op: Dp1, x: bool, rd: R, rn: R) -> Ins {
    ins(
        0x5ac0_0000 | sf(x) | ((op as u32) << 10) | (rn.0 << 5) | rd.0,
        F::Dp1,
    )
}

/// Two-source operations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Dp2 {
    Udiv = 2,
    Sdiv = 3,
    Lsl = 8,
    Lsr = 9,
    Asr = 10,
    /// `rorv`, written `ror`.
    Ror = 11,
}

/// `op rd, rn, rm`.
#[must_use]
pub const fn dp2(op: Dp2, x: bool, rd: R, rn: R, rm: R) -> Ins {
    let w = 0x1ac0_0000 | sf(x) | ((op as u32) << 10);
    ins(w | (rm.0 << 16) | (rn.0 << 5) | rd.0, F::Dp2)
}

/// Three-source operations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Dp3 {
    Madd,
    Msub,
    /// `xd = wn * wm + xa`, signed / unsigned.
    Smaddl,
    Umaddl,
    Smulh,
    Umulh,
}

/// `op rd, rn, rm, ra` (`mul` / `smull` / `umull`: `ra` = `zr`).
#[must_use]
pub const fn dp3(op: Dp3, x: bool, rd: R, rn: R, rm: R, ra: R) -> Ins {
    let w = match op {
        Dp3::Madd => 0x1b00_0000 | sf(x),
        Dp3::Msub => 0x1b00_8000 | sf(x),
        Dp3::Smaddl => 0x9b20_0000,
        Dp3::Umaddl => 0x9ba0_0000,
        Dp3::Smulh => 0x9b40_0000,
        Dp3::Umulh => 0x9bc0_0000,
    };
    ins(w | (rm.0 << 16) | (ra.0 << 10) | (rn.0 << 5) | rd.0, F::Dp3)
}

/// `mul rd, rn, rm`.
#[must_use]
pub const fn mul(x: bool, rd: R, rn: R, rm: R) -> Ins {
    dp3(Dp3::Madd, x, rd, rn, rm, ZR)
}

/// `csel rd, rn, rm, cond`.
#[must_use]
pub const fn csel(x: bool, rd: R, rn: R, rm: R, cond: Cond) -> Ins {
    let w = 0x1a80_0000 | sf(x);
    ins(
        w | (rm.0 << 16) | (cond.0 << 12) | (rn.0 << 5) | rd.0,
        F::CondSel,
    )
}

/// `csinv rd, rn, rm, cond`.
#[must_use]
pub const fn csinv(x: bool, rd: R, rn: R, rm: R, cond: Cond) -> Ins {
    let w = 0x5a80_0000 | sf(x);
    ins(
        w | (rm.0 << 16) | (cond.0 << 12) | (rn.0 << 5) | rd.0,
        F::CondSel,
    )
}

/// `cset wd, cond` (`csinc wd, wzr, wzr, !cond`).
#[must_use]
pub const fn cset(rd: R, cond: Cond) -> Ins {
    ins(0x1a9f_07e0 | (cond.inv().0 << 12) | rd.0, F::CondSel)
}

// Branches; offsets in words.

/// `b` / `bl` (`link`) `.+4d`.
#[must_use]
pub const fn b(link: bool, d: i64) -> Ins {
    let w = if link { 0x9400_0000 } else { 0x1400_0000 };
    ins(w | ((d as u32) & 0x03ff_ffff), F::B)
}

/// `b.cond .+4d`.
#[must_use]
pub const fn b_cond(cond: Cond, d: i64) -> Ins {
    ins(
        0x5400_0000 | (((d as u32) & 0x7ffff) << 5) | cond.0,
        F::BCond,
    )
}

/// `cbz` / `cbnz` (`nz`) `wt, .+4d`.
#[must_use]
pub const fn cbz(nz: bool, rt: R, d: i64) -> Ins {
    let w = if nz { 0x3500_0000 } else { 0x3400_0000 };
    ins(w | (((d as u32) & 0x7ffff) << 5) | rt.0, F::Cbz)
}

/// `blr xn`.
#[must_use]
pub const fn blr(rn: R) -> Ins {
    ins(0xd63f_0000 | (rn.0 << 5), F::Blr)
}

#[must_use]
pub const fn ret() -> Ins {
    ins(0xd65f_03c0, F::Ret)
}

/// Whether a 19-bit branch reaches `d` words.
#[must_use]
pub const fn reaches19(d: i64) -> bool {
    -(1 << 18) <= d && d < 1 << 18
}

// Loads and stores.

/// A load / store: its unsigned-offset form and log2 of the access size.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Ls(u32, u32);

pub const STRB: Ls = Ls(0x3900_0000, 0);
pub const LDRB: Ls = Ls(0x3940_0000, 0);
pub const LDRSB: Ls = Ls(0x3980_0000, 0);
pub const STRH: Ls = Ls(0x7900_0000, 1);
pub const LDRH: Ls = Ls(0x7940_0000, 1);
pub const LDRSH: Ls = Ls(0x7980_0000, 1);
pub const STRW: Ls = Ls(0xb900_0000, 2);
pub const LDRW: Ls = Ls(0xb940_0000, 2);
pub const LDRSW: Ls = Ls(0xb980_0000, 2);
pub const STRX: Ls = Ls(0xf900_0000, 3);
pub const LDRX: Ls = Ls(0xf940_0000, 3);
pub const STRS: Ls = Ls(0xbd00_0000, 2);
pub const LDRS: Ls = Ls(0xbd40_0000, 2);
pub const STRD: Ls = Ls(0xfd00_0000, 3);
pub const LDRD: Ls = Ls(0xfd40_0000, 3);
pub const STRQ: Ls = Ls(0x3d80_0000, 4);
pub const LDRQ: Ls = Ls(0x3dc0_0000, 4);

impl Ls {
    /// log2 of the access size.
    #[must_use]
    pub const fn size(self) -> u32 {
        self.1
    }

    /// A store (writes no register).
    #[must_use]
    pub const fn store(self) -> bool {
        if self.0 & (1 << 26) != 0 {
            self.0 & (1 << 22) == 0
        } else {
            self.0 & (3 << 22) == 0
        }
    }
}

/// `op rt, [rn, #(idx << size)]`: the scaled 12-bit form.
#[must_use]
pub const fn ldst(op: Ls, rt: u32, rn: R, idx: u32) -> Ins {
    ins(op.0 | (idx << 10) | (rn.0 << 5) | rt, F::LdSt)
}

/// `ldur` / `stur rt, [rn, #off]`: the unscaled 9-bit form.
#[must_use]
pub const fn ldst_unscaled(op: Ls, rt: u32, rn: R, off: i64) -> Ins {
    let w = op.0 & !0x0100_0000;
    ins(
        w | (((off as u32) & 0x1ff) << 12) | (rn.0 << 5) | rt,
        F::LdSt,
    )
}

/// `op rt, [rn], #off`: post-indexed.
#[must_use]
pub const fn ldst_post(op: Ls, rt: u32, rn: R, off: i64) -> Ins {
    let w = (op.0 & !0x0100_0000) | 0x400;
    ins(
        w | (((off as u32) & 0x1ff) << 12) | (rn.0 << 5) | rt,
        F::LdSt,
    )
}

/// `op rt, [rn, xm{, lsl #size}]` (`scaled`).
#[must_use]
pub const fn ldst_reg(op: Ls, rt: u32, rn: R, rm: R, scaled: bool) -> Ins {
    let w = (op.0 & !0x0100_0000) | 0x0020_6800 | ((scaled as u32) << 12);
    ins(w | (rm.0 << 16) | (rn.0 << 5) | rt, F::LdSt)
}

/// A register pair.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Pair {
    StpX,
    LdpX,
    StpD,
    LdpD,
}

/// The addressing of a pair.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Index {
    Post = 1,
    Offset = 2,
    Pre = 3,
}

/// `op rt, rt2, [rn, #8 imm]` (`Pre`: `!`; `Post`: `[rn], #8 imm`).
#[must_use]
pub const fn pair(op: Pair, ix: Index, rt: u32, rt2: u32, rn: R, imm: i32) -> Ins {
    let w = match op {
        Pair::StpX => 0xa800_0000,
        Pair::LdpX => 0xa840_0000,
        Pair::StpD => 0x6c00_0000,
        Pair::LdpD => 0x6c40_0000,
    } | ((ix as u32) << 23);
    let imm = (imm as u32) & 0x7f;
    ins(w | (imm << 15) | (rt2 << 10) | (rn.0 << 5) | rt, F::Pair)
}

// Atomics; `size` is log2 of the access size.

/// Ordered and exclusive accesses.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Excl {
    Ldar,
    Stlr,
    Ldaxr,
    Ldxr,
    /// Release-consistent, processor-consistent (`rcpc`).
    Ldapr,
}

/// `op rt, [rn]`.
#[must_use]
pub const fn excl(op: Excl, size: u32, rt: R, rn: R) -> Ins {
    let w = match op {
        Excl::Ldar => 0x08df_fc00,
        Excl::Stlr => 0x089f_fc00,
        Excl::Ldaxr => 0x085f_fc00,
        Excl::Ldxr => 0x085f_7c00,
        Excl::Ldapr => 0x38bf_c000,
    };
    ins((size << 30) | w | (rn.0 << 5) | rt.0, F::Excl)
}

/// `stxr ws, rt, [rn]`, or `stlxr` (`rel`).
#[must_use]
pub const fn stxr(size: u32, rel: bool, rs: R, rt: R, rn: R) -> Ins {
    ins(
        (size << 30) | 0x0800_7c00 | ((rel as u32) << 15) | (rs.0 << 16) | (rn.0 << 5) | rt.0,
        F::Excl,
    )
}

/// LSE read-modify-writes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Lse {
    Ldadd = 0,
    Ldclr = 1,
    Ldeor = 2,
    Ldset = 3,
    Swp = 8,
}

/// `op{a}{l} rs, rt, [rn]`: acquire (`a`), release (`l`).
#[must_use]
pub const fn lse_o(op: Lse, size: u32, a: bool, l: bool, rs: R, rt: R, rn: R) -> Ins {
    let w = (size << 30) | 0x3820_0000 | ((a as u32) << 23) | ((l as u32) << 22);
    ins(
        w | ((op as u32) << 12) | (rs.0 << 16) | (rn.0 << 5) | rt.0,
        F::Lse,
    )
}

/// `cas{a}{l} rs, rt, [rn]`: acquire (`a`), release (`l`).
#[must_use]
pub const fn cas(size: u32, a: bool, l: bool, rs: R, rt: R, rn: R) -> Ins {
    let w = (size << 30) | 0x08a0_7c00 | ((a as u32) << 22) | ((l as u32) << 15);
    ins(w | (rs.0 << 16) | (rn.0 << 5) | rt.0, F::Cas)
}

#[must_use]
pub const fn clrex() -> Ins {
    ins(0xd503_3f5f, F::Clrex)
}

#[must_use]
pub const fn nop() -> Ins {
    ins(0xd503_201f, F::Nop)
}

/// `mrs xt, tpidr_el0`.
#[must_use]
pub const fn mrs_tpidr(rt: R) -> Ins {
    ins(0xd53b_d040 | rt.0, F::Mrs)
}

/// `brk #imm`.
#[must_use]
pub const fn brk(imm: u32) -> Ins {
    ins(0xd420_0000 | (imm << 5), F::Brk)
}

/// `adrp xd, 0` (relocated).
#[must_use]
pub const fn adrp(rd: R) -> Ins {
    ins(0x9000_0000 | rd.0, F::Adrp)
}

// Floating point; `d`: double, else single.

/// `fmov` from `vn` to `rd` (`to_gpr`) or from `rn` to `vd`: `x` / `d`
/// (`d`) or `w` / `s`.
#[must_use]
pub const fn fmov_gpr(to_gpr: bool, d: bool, rd: u32, rn: u32) -> Ins {
    let w = match (to_gpr, d) {
        (true, true) => 0x9e66_0000,
        (true, false) => 0x1e26_0000,
        (false, true) => 0x9e67_0000,
        (false, false) => 0x1e27_0000,
    };
    ins(w | (rn << 5) | rd, F::FmovGpr)
}

/// Scalar floating-point operations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Fp {
    Fmov,
    Fneg,
    Fsqrt,
    Fadd,
    Fsub,
    Fmul,
    Fdiv,
    Fcmp,
    /// `fcvt` to the other precision (`d`: from double).
    Fcvt,
    /// `fcvt hd, sn`.
    FcvtToHalf,
    /// `fcvt sd, hn`.
    FcvtFromHalf,
}

/// `op vd, vn{, vm}` (`fcmp vn, vm`: `vd` unused).
#[must_use]
pub const fn fp(op: Fp, d: bool, vd: V, vn: V, vm: V) -> Ins {
    let ty = (d as u32) << 22;
    let w = match op {
        Fp::Fmov => 0x1e20_4000 | ty,
        Fp::Fneg => 0x1e21_4000 | ty,
        Fp::Fsqrt => 0x1e21_c000 | ty,
        Fp::Fadd => 0x1e20_2800 | ty | (vm.0 << 16),
        Fp::Fsub => 0x1e20_3800 | ty | (vm.0 << 16),
        Fp::Fmul => 0x1e20_0800 | ty | (vm.0 << 16),
        Fp::Fdiv => 0x1e20_1800 | ty | (vm.0 << 16),
        Fp::Fcmp => 0x1e20_2000 | ty | (vm.0 << 16),
        Fp::Fcvt if d => 0x1e62_4000,
        Fp::Fcvt => 0x1e22_c000,
        Fp::FcvtToHalf => 0x1e23_c000,
        Fp::FcvtFromHalf => 0x1ee2_4000,
    };
    let rd = if matches!(op, Fp::Fcmp) { 0 } else { vd.0 };
    ins(w | (vn.0 << 5) | rd, F::Fp)
}

/// `fmadd vd, vn, vm, va`: `vn * vm + va`, rounded once.
#[must_use]
pub const fn fmadd(d: bool, vd: V, vn: V, vm: V, va: V) -> Ins {
    let w = 0x1f00_0000 | ((d as u32) << 22) | (vm.0 << 16) | (va.0 << 10);
    ins(w | (vn.0 << 5) | vd.0, F::Fp3)
}

/// `scvtf` / `ucvtf vd, xn`.
#[must_use]
pub const fn cvtf(signed: bool, d: bool, vd: V, rn: R) -> Ins {
    let w = match (d, signed) {
        (false, true) => 0x9e22_0000,
        (false, false) => 0x9e23_0000,
        (true, true) => 0x9e62_0000,
        (true, false) => 0x9e63_0000,
    };
    ins(w | (rn.0 << 5) | vd.0, F::Cvtf)
}

/// `fcvtzs` / `fcvtzu xd, vn`.
#[must_use]
pub const fn fcvtz(signed: bool, d: bool, rd: R, vn: V) -> Ins {
    let w = if signed { 0x9e38_0000 } else { 0x9e39_0000 } | ((d as u32) << 22);
    ins(w | (vn.0 << 5) | rd.0, F::Fcvtz)
}

// NEON; `size` is log2 of the lane bytes.

/// Three-register operations on 128 bits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Neon3 {
    Add,
    Sub,
    Mul,
    And,
    Bic,
    Orr,
    Eor,
    Cmeq,
    Cmgt,
    Cmhi,
    Smax,
    Umax,
    Smin,
    Umin,
    Fadd,
    Fsub,
    Fmul,
    Fdiv,
    Fcmeq,
    Fcmge,
    Fcmgt,
    Fmax,
    Fmin,
    /// `vd += vn * vm`, rounded once.
    Fmla,
}

/// `op vd.T, vn.T, vm.T`.
#[must_use]
pub const fn neon3(op: Neon3, size: u32, vd: V, vn: V, vm: V) -> Ins {
    let sz = (size == 3) as u32;
    let (u, s, opc) = match op {
        Neon3::Add => (0, size, 0b10000),
        Neon3::Sub => (1, size, 0b10000),
        Neon3::Mul => (0, size, 0b10011),
        Neon3::And => (0, 0, 0b00011),
        Neon3::Bic => (0, 1, 0b00011),
        Neon3::Orr => (0, 2, 0b00011),
        Neon3::Eor => (1, 0, 0b00011),
        Neon3::Cmeq => (1, size, 0b10001),
        Neon3::Cmgt => (0, size, 0b00110),
        Neon3::Cmhi => (1, size, 0b00110),
        Neon3::Smax => (0, size, 0b01100),
        Neon3::Umax => (1, size, 0b01100),
        Neon3::Smin => (0, size, 0b01101),
        Neon3::Umin => (1, size, 0b01101),
        Neon3::Fadd => (0, sz, 0b11010),
        Neon3::Fsub => (0, 2 | sz, 0b11010),
        Neon3::Fmul => (1, sz, 0b11011),
        Neon3::Fdiv => (1, sz, 0b11111),
        Neon3::Fcmeq => (0, sz, 0b11100),
        Neon3::Fcmge => (1, sz, 0b11100),
        Neon3::Fcmgt => (1, 2 | sz, 0b11100),
        Neon3::Fmax => (0, sz, 0b11110),
        Neon3::Fmin => (0, 2 | sz, 0b11110),
        Neon3::Fmla => (0, sz, 0b11001),
    };
    let w = 0x4e20_0400 | (u << 29) | (s << 22) | (opc << 11);
    ins(w | (vm.0 << 16) | (vn.0 << 5) | vd.0, F::Neon3)
}

/// Widening, permuting and dot-product operations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NeonWide {
    /// `smull` / `umull` of the low halves (`2`: the high ones): lanes
    /// of twice `size`.
    Smull,
    Smull2,
    Umull,
    Umull2,
    /// `uzp2`: the odd lanes of `vn` then `vm`.
    Uzp2,
    /// `sdot` / `udot vd.4s, vn.16b, vm.16b` (`size` unused).
    Sdot,
    Udot,
}

/// `op vd, vn, vm` (`size`: log2 of the narrow lanes' bytes).
#[must_use]
pub const fn neon_wide(op: NeonWide, size: u32, vd: V, vn: V, vm: V) -> Ins {
    let w = match op {
        NeonWide::Smull => 0x0e20_c000 | (size << 22),
        NeonWide::Smull2 => 0x4e20_c000 | (size << 22),
        NeonWide::Umull => 0x2e20_c000 | (size << 22),
        NeonWide::Umull2 => 0x6e20_c000 | (size << 22),
        NeonWide::Uzp2 => 0x4e00_5800 | (size << 22),
        NeonWide::Sdot => 0x4e80_9400,
        NeonWide::Udot => 0x6e80_9400,
    };
    ins(w | (vm.0 << 16) | (vn.0 << 5) | vd.0, F::NeonWide)
}

/// `mov vd.16b, vn.16b` (`orr vd, vn, vn`).
#[must_use]
pub const fn vmov(vd: V, vn: V) -> Ins {
    neon3(Neon3::Orr, 0, vd, vn, vn)
}

/// Two-register operations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NeonMisc {
    /// `cnt vd.8b, vn.8b`.
    Cnt8,
    /// `addv bd, vn.8b`.
    Addv8,
    /// `umaxv bd, vn.16b`.
    Umaxv16,
    /// `mvn vd.16b, vn.16b`.
    Not,
    /// `cmlt vd.T, vn.T, #0`.
    Cmlt0,
    /// `fsqrt vd.T, vn.T` (`size` 2 / 3).
    Fsqrt,
    /// `xtn vd.Tn, vn.T`: lanes of `size` from twice that.
    Xtn,
    /// `abs vd.T, vn.T`.
    Abs,
}

/// `op vd, vn` (`size`: the lanes', where it has them).
#[must_use]
pub const fn neon_misc(op: NeonMisc, size: u32, vd: V, vn: V) -> Ins {
    let w = match op {
        NeonMisc::Cnt8 => 0x0e20_5800,
        NeonMisc::Addv8 => 0x0e31_b800,
        NeonMisc::Umaxv16 => 0x6e30_a800,
        NeonMisc::Not => 0x6e20_5800,
        NeonMisc::Cmlt0 => 0x4e20_a800 | (size << 22),
        NeonMisc::Fsqrt => 0x6ea1_f800 | (((size == 3) as u32) << 22),
        NeonMisc::Xtn => 0x0e21_2800 | (size << 22),
        NeonMisc::Abs => 0x4e20_b800 | (size << 22),
    };
    ins(w | (vn.0 << 5) | vd.0, F::NeonMisc)
}

/// Shifts by an immediate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NeonShift {
    Shl,
    Sshr,
    Ushr,
    /// `sshll` / `ushll #0`: lanes of twice `bits` from the low half.
    Sshll,
    Ushll,
}

/// `op vd.T, vn.T, #n` on lanes of `bits`.
#[must_use]
pub const fn neon_shift(op: NeonShift, bits: u32, vd: V, vn: V, n: u32) -> Ins {
    let w = match op {
        NeonShift::Shl => 0x4f00_5400 | ((bits + n) << 16),
        NeonShift::Sshr => 0x4f00_0400 | ((2 * bits - n) << 16),
        NeonShift::Ushr => 0x6f00_0400 | ((2 * bits - n) << 16),
        NeonShift::Sshll => 0x0f00_a400 | ((bits + n) << 16),
        NeonShift::Ushll => 0x2f00_a400 | ((bits + n) << 16),
    };
    ins(w | (vn.0 << 5) | vd.0, F::NeonShift)
}

/// `movi vd.2d, #0` or (`ones`) `#-1`.
#[must_use]
pub const fn movi(ones: bool, vd: V) -> Ins {
    ins(if ones { 0x6f07_e7e0 } else { 0x6f00_e400 } | vd.0, F::Movi)
}

/// `dup vd.T, rn` on lanes of `bytes`.
#[must_use]
pub const fn dup(bytes: u32, vd: V, rn: R) -> Ins {
    ins(0x4e00_0c00 | (bytes << 16) | (rn.0 << 5) | vd.0, F::Dup)
}

/// `umov wd, vn.b[0]`.
#[must_use]
pub const fn umov_b0(rd: R, vn: V) -> Ins {
    ins(0x0e01_3c00 | (vn.0 << 5) | rd.0, F::Umov)
}

/// The `imm5` of lane `n` of `bytes`.
const fn imm5(bytes: u32, n: u32) -> u32 {
    ((n << 1) | 1) << bytes.trailing_zeros()
}

/// `mov vd.T[n], vn.T[0]` (`ins`) on lanes of `bytes`.
#[must_use]
pub const fn ins_lane(bytes: u32, n: u32, vd: V, vn: V) -> Ins {
    ins(
        0x6e00_0400 | (imm5(bytes, n) << 16) | (vn.0 << 5) | vd.0,
        F::Lane,
    )
}

/// `mov vd.T[n], rn` (`ins`) on lanes of `bytes`.
#[must_use]
pub const fn ins_gpr(bytes: u32, n: u32, vd: V, rn: R) -> Ins {
    ins(
        0x4e00_1c00 | (imm5(bytes, n) << 16) | (rn.0 << 5) | vd.0,
        F::Lane,
    )
}

/// The sum of the lanes of `bytes` of `vn` (wrapping) into `vd`'s low
/// lane: `addv` (`addp` of two 8-byte lanes).
#[must_use]
pub const fn addv(bytes: u32, vd: V, vn: V) -> Ins {
    let w = match bytes {
        1 => 0x4e31_b800,
        2 => 0x4e71_b800,
        4 => 0x4eb1_b800,
        _ => 0x5ef1_b800,
    };
    ins(w | (vn.0 << 5) | vd.0, F::Lane)
}

/// `dup vd.T, vn.T[n]` on lanes of `bytes`.
#[must_use]
pub const fn dup_elem(bytes: u32, n: u32, vd: V, vn: V) -> Ins {
    ins(
        0x4e00_0400 | (imm5(bytes, n) << 16) | (vn.0 << 5) | vd.0,
        F::Lane,
    )
}

/// `mov <b|h|s|d>d, vn.T[n]` (`dup`): lane `n` of `bytes`, the rest zero.
#[must_use]
pub const fn dup_lane(bytes: u32, n: u32, vd: V, vn: V) -> Ins {
    ins(
        0x5e00_0400 | (imm5(bytes, n) << 16) | (vn.0 << 5) | vd.0,
        F::Lane,
    )
}

/// `umov wd, vn.T[n]` (`xd` of 8 bytes): lane `n`, zero-extended.
#[must_use]
pub const fn umov(bytes: u32, n: u32, rd: R, vn: V) -> Ins {
    let q = ((bytes == 8) as u32) << 30;
    ins(
        0x0e00_3c00 | q | (imm5(bytes, n) << 16) | (vn.0 << 5) | rd.0,
        F::Lane,
    )
}

/// Whether `insn` is a load / store with a scaled 12-bit offset (its
/// `PageOff12` scales).
#[must_use]
pub fn scaled_ldst(insn: &[u8]) -> bool {
    let w = u32::from_le_bytes(insn[..4].try_into().unwrap_or([0; 4]));
    w & 0x3b00_0000 == 0x3900_0000
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

    /// Point the `b` at `at` `d` words on.
    pub fn patch26(&mut self, at: usize, d: i64) {
        self.code[at] |= (d as u32) & 0x03ff_ffff;
    }

    /// Point the `b.cond` / `cbz` at `at` `d` words on.
    pub fn patch19(&mut self, at: usize, d: i64) {
        self.code[at] |= ((d as u32) & 0x7ffff) << 5;
    }

    /// Each recorded word's byte offset and text (`apple`: NEON in the
    /// Apple syntax, as objdump shows a Mach-O object).
    #[must_use]
    pub fn lines(&self, l: &Listing<'_>, apple: bool) -> Vec<(u32, String)> {
        let Some(t) = &self.text else {
            return Vec::new();
        };
        t.iter()
            .zip(&self.code)
            .enumerate()
            .map(|(n, (f, w))| {
                let at = 4 * n as i64;
                let s = text(*w, *f, at, l);
                (at as u32, if apple { apple_neon(&s) } else { s })
            })
            .collect()
    }
}
