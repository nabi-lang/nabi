//! powerpc64 instruction encoder: one function per instruction form the
//! emitter uses, the only place that knows opcodes and bit fields. Each
//! returns an `Ins`: the word and its form. `Asm` keeps the words and,
//! with text on (`--emit=asm`), each word's form; `lines` decodes them
//! into what `llvm-objdump -d` prints (its extended mnemonics, decimal
//! operands, register numbers). Off, only the words are kept.
//!
//! VSX instructions name `vs32 + n` (the VMX register `vn`) throughout.
//!
//! Fixed choices: a conditional jump is
//! the inverted short `bc` over a `b`; a vector loop branches back by
//! `bc 20, 0` (always) and scales its offsets by `sldi r, r, 0` on byte
//! lanes; `fneg` and `frsp` take the double form, the `f32` conversions
//! from integers `fcfids` / `fcfidus`; a shift count reaches every lane by
//! `vspltisb` whatever the lane width; 16-bit lanes multiply by
//! `vmladduhm` with a zeroed `v19`.

#[cfg(test)]
mod tests;
mod text;

pub(crate) use crate::isa::ppc64::enc::text::*;

use crate::asm::Listing;

/// A general-purpose register, 0 .. 31. In the `ra` field of a load,
/// store or `addi`, 0 reads as zero.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct R(pub u32);

/// A float register, 0 .. 31.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct F(pub u32);

/// A VMX vector register, 0 .. 31 (`vs32 + n` to VSX).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct V(pub u32);

// Bits of `cr0` (and `cr6` for the vector compares).
pub const LT: u32 = 0;
pub const GT: u32 = 1;
pub const EQ: u32 = 2;
pub const UN: u32 = 3;
/// The `eq` bit of `cr6`: set where a recording vector compare is false
/// in every lane.
pub const CR6_EQ: u32 = 26;

/// A branch condition: `bc`'s BO and BI.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Cond {
    bo: u32,
    bi: u32,
}

impl Cond {
    /// Decrement `ctr`, branch while non-zero (`bdnz`).
    pub const DNZ: Self = Self { bo: 16, bi: 0 };
    /// Branch always (BO 20; the vector loop's back branch).
    pub const ALWAYS: Self = Self { bo: 20, bi: 0 };

    /// Condition register bit `bi` set (`bt`).
    #[must_use]
    pub const fn t(bi: u32) -> Self {
        Self { bo: 12, bi }
    }

    /// Condition register bit `bi` clear (`bf`).
    #[must_use]
    pub const fn f(bi: u32) -> Self {
        Self { bo: 4, bi }
    }

    /// The opposite condition (of a `t` / `f` one).
    #[must_use]
    pub const fn inv(self) -> Self {
        Self {
            bo: self.bo ^ 8,
            bi: self.bi,
        }
    }
}

/// The form of a word, for its text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Fm {
    Addi,
    LogImm,
    CmpImm,
    LdSt,
    LdStX,
    Alu,
    Logic,
    Cmp,
    Mfcr,
    Larx,
    Stcx,
    Brx,
    Sradi,
    Rld,
    Rlwinm,
    Rot,
    Cror,
    Bc,
    B,
    Spr,
    Fixed,
    Fp,
    Fcmpu,
    Vx,
    Va,
    Vsplt,
    Xx1,
    Xx2,
    Xx3,
}

/// An encoded instruction.
#[derive(Clone, Copy, Debug)]
pub struct Ins {
    word: u32,
    f: Fm,
}

impl Ins {
    #[cfg(test)]
    #[must_use]
    pub const fn word(self) -> u32 {
        self.word
    }
}

const fn ins(word: u32, f: Fm) -> Ins {
    Ins { word, f }
}

/// D-form `op rt, ra, imm`.
const fn d(op: u32, rt: u32, ra: u32, imm: i64) -> u32 {
    (op << 26) | (rt << 21) | (ra << 16) | (imm as u32 & 0xffff)
}

/// X-form (primary 31) `xo rt, ra, rb`.
const fn x(xo: u32, rt: u32, ra: u32, rb: u32) -> u32 {
    (31 << 26) | (rt << 21) | (ra << 16) | (rb << 11) | (xo << 1)
}

// Fixed words.

#[must_use]
pub const fn nop() -> Ins {
    ins(0x6000_0000, Fm::Fixed)
}

#[must_use]
pub const fn blr() -> Ins {
    ins(0x4e80_0020, Fm::Fixed)
}

#[must_use]
pub const fn bctrl() -> Ins {
    ins(0x4e80_0421, Fm::Fixed)
}

#[must_use]
pub const fn sync() -> Ins {
    ins(0x7c00_04ac, Fm::Fixed)
}

#[must_use]
pub const fn lwsync() -> Ins {
    ins(0x7c20_04ac, Fm::Fixed)
}

#[must_use]
pub const fn isync() -> Ins {
    ins(0x4c00_012c, Fm::Fixed)
}

/// `trap` (`tw 31, 0, 0`).
#[must_use]
pub const fn trap() -> Ins {
    ins(0x7fe0_0008, Fm::Fixed)
}

// Immediates.

/// `addi rt, ra, imm` (`li` with `ra` 0).
#[must_use]
pub const fn addi(rt: R, ra: R, imm: i64) -> Ins {
    ins(d(14, rt.0, ra.0, imm), Fm::Addi)
}

/// `addis rt, ra, imm` (`lis` with `ra` 0; `imm`'s low 16 bits).
#[must_use]
pub const fn addis(rt: R, ra: R, imm: i64) -> Ins {
    ins(d(15, rt.0, ra.0, imm), Fm::Addi)
}

/// A logical operation on an unsigned immediate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LogImm {
    Ori = 24,
    Oris = 25,
    Xori = 26,
    /// `andi.` (sets `cr0`).
    Andi = 28,
}

/// `op ra, rs, ui`.
#[must_use]
pub const fn log_imm(op: LogImm, ra: R, rs: R, ui: i64) -> Ins {
    ins(d(op as u32, rs.0, ra.0, ui), Fm::LogImm)
}

/// `cmpdi ra, imm` (`cr0`).
#[must_use]
pub const fn cmpdi(ra: R, imm: i64) -> Ins {
    ins(d(11, 1, ra.0, imm), Fm::CmpImm)
}

/// `cmpldi ra, imm` (`cr0`).
#[must_use]
pub const fn cmpldi(ra: R, imm: i64) -> Ins {
    ins(d(10, 1, ra.0, imm), Fm::CmpImm)
}

// Loads and stores.

/// A load or store: its D / DS-form and X-form encodings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Ls {
    Lbz,
    Lha,
    Lhz,
    Lwa,
    Lwz,
    Ld,
    Stb,
    Sth,
    Stw,
    Std,
    Lfs,
    Lfd,
    Stfs,
    Stfd,
}

impl Ls {
    /// (D / DS-form opcode, DS-form `xo` if DS, X-form `xo`).
    const fn enc(self) -> (u32, Option<u32>, u32) {
        match self {
            Self::Lbz => (34, None, 87),
            Self::Lha => (42, None, 343),
            Self::Lhz => (40, None, 279),
            Self::Lwa => (58, Some(2), 341),
            Self::Lwz => (32, None, 23),
            Self::Ld => (58, Some(0), 21),
            Self::Stb => (38, None, 215),
            Self::Sth => (44, None, 407),
            Self::Stw => (36, None, 151),
            Self::Std => (62, Some(0), 149),
            Self::Lfs => (48, None, 535),
            Self::Lfd => (50, None, 599),
            Self::Stfs => (52, None, 663),
            Self::Stfd => (54, None, 727),
        }
    }

    /// DS-form: the offset a multiple of 4.
    #[must_use]
    pub const fn ds(self) -> bool {
        self.enc().1.is_some()
    }
}

/// `op rt, off(ra)`; `rt` a float register for the float ops. A DS-form
/// `off` drops its low two bits.
#[must_use]
pub const fn ls(op: Ls, rt: u32, ra: R, off: i64) -> Ins {
    let (o, ds, _) = op.enc();
    let w = match ds {
        Some(xo) => (o << 26) | (rt << 21) | (ra.0 << 16) | (off as u32 & 0xfffc) | xo,
        None => d(o, rt, ra.0, off),
    };
    ins(w, Fm::LdSt)
}

/// `opx rt, ra, rb`; `rt` a float register for the float ops.
#[must_use]
pub const fn ls_x(op: Ls, rt: u32, ra: R, rb: R) -> Ins {
    ins(x(op.enc().2, rt, ra.0, rb.0), Fm::LdStX)
}

/// `stdu rs, off(ra)`.
#[must_use]
pub const fn stdu(rs: R, ra: R, off: i64) -> Ins {
    ins(
        (62 << 26) | (rs.0 << 21) | (ra.0 << 16) | (off as u32 & 0xfffc) | 1,
        Fm::LdSt,
    )
}

/// `stdux rs, ra, rb`.
#[cfg(test)]
#[must_use]
pub const fn stdux(rs: R, ra: R, rb: R) -> Ins {
    ins(x(181, rs.0, ra.0, rb.0), Fm::LdStX)
}

/// `lbarx` / `lharx` / `lwarx` / `ldarx` `rt, 0, rb` of `bytes`.
#[must_use]
pub const fn larx(bytes: u32, rt: R, rb: R) -> Ins {
    let xo = match bytes {
        1 => 52,
        2 => 116,
        4 => 20,
        _ => 84,
    };
    ins(x(xo, rt.0, 0, rb.0), Fm::Larx)
}

/// `stbcx.` / `sthcx.` / `stwcx.` / `stdcx.` `rs, 0, rb` of `bytes`.
#[must_use]
pub const fn stcx(bytes: u32, rs: R, rb: R) -> Ins {
    let xo = match bytes {
        1 => 694,
        2 => 726,
        4 => 150,
        _ => 214,
    };
    ins(x(xo, rs.0, 0, rb.0) | 1, Fm::Stcx)
}

/// `lhbrx` / `lwbrx` / `ldbrx` `rt, 0, rb` of `bytes` (2, 4, 8).
#[must_use]
pub const fn lbrx(bytes: u32, rt: R, rb: R) -> Ins {
    let xo = match bytes {
        2 => 790,
        4 => 534,
        _ => 532,
    };
    ins(x(xo, rt.0, 0, rb.0), Fm::Brx)
}

// Arithmetic and logic.

/// An XO-form operation `rt = ra op rb`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Alu {
    Add = 266,
    /// `rb - ra`.
    Subf = 40,
    Mulld = 233,
    Mulhd = 73,
    Mulhdu = 9,
    Divd = 489,
    Divdu = 457,
    /// `-ra` (`rb` 0).
    Neg = 104,
}

/// `op rt, ra, rb`.
#[must_use]
pub const fn alu(op: Alu, rt: R, ra: R, rb: R) -> Ins {
    ins(x(op as u32, rt.0, ra.0, rb.0), Fm::Alu)
}

/// An X-form operation `ra = rs op rb` (the source in the `rt` field);
/// the one-operand ones take `rb` 0.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Logic {
    And = 28,
    Andc = 60,
    Or = 444,
    Xor = 316,
    Nor = 124,
    Sld = 27,
    Srd = 539,
    Srad = 794,
    Extsb = 954,
    Extsh = 922,
    Extsw = 986,
    Cntlzd = 58,
    Popcntd = 506,
}

/// `op ra, rs, rb`.
#[must_use]
pub const fn logic(op: Logic, ra: R, rs: R, rb: R) -> Ins {
    ins(x(op as u32, rs.0, ra.0, rb.0), Fm::Logic)
}

/// `mr ra, rs` (`or ra, rs, rs`).
#[must_use]
pub const fn mr(ra: R, rs: R) -> Ins {
    logic(Logic::Or, ra, rs, rs)
}

/// `cmpd` / `cmpld` `ra, rb` (`cr0`).
#[must_use]
pub const fn cmp(signed: bool, ra: R, rb: R) -> Ins {
    ins(x(if signed { 0 } else { 32 }, 1, ra.0, rb.0), Fm::Cmp)
}

/// `mfcr rt`.
#[must_use]
pub const fn mfcr(rt: R) -> Ins {
    ins(x(19, rt.0, 0, 0), Fm::Mfcr)
}

/// `sradi ra, rs, n` (XS-form).
#[must_use]
pub const fn sradi(ra: R, rs: R, n: u32) -> Ins {
    let w = (31 << 26) | (rs.0 << 21) | (ra.0 << 16) | ((n & 31) << 11) | (413 << 2);
    ins(w | ((n >> 5) << 1), Fm::Sradi)
}

/// MD-form rotate `xo` (`rldicl` 0, `rldicr` 1) `ra, rs, sh, m`.
const fn md(xo: u32, ra: R, rs: R, sh: u32, m: u32) -> Ins {
    let w = (30 << 26) | (rs.0 << 21) | (ra.0 << 16) | ((sh & 31) << 11) | ((m & 31) << 6);
    ins(w | ((m >> 5) << 5) | (xo << 2) | ((sh >> 5) << 1), Fm::Rld)
}

/// `rldicl ra, rs, sh, mb`: rotate left, clear the high `mb` bits.
#[must_use]
pub const fn rldicl(ra: R, rs: R, sh: u32, mb: u32) -> Ins {
    md(0, ra, rs, sh, mb)
}

/// `rldicr ra, rs, sh, me`: rotate left, clear the bits after `me`.
#[must_use]
pub const fn rldicr(ra: R, rs: R, sh: u32, me: u32) -> Ins {
    md(1, ra, rs, sh, me)
}

/// `rotld ra, rs, rb` (`rldcl ra, rs, rb, 0`).
#[must_use]
pub const fn rotld(ra: R, rs: R, rb: R) -> Ins {
    let w = (30 << 26) | (rs.0 << 21) | (ra.0 << 16) | (rb.0 << 11);
    ins(w | (8 << 1), Fm::Rot)
}

/// `rotlw ra, rs, rb` (`rlwnm ra, rs, rb, 0, 31`).
#[must_use]
pub const fn rotlw(ra: R, rs: R, rb: R) -> Ins {
    let w = (23 << 26) | (rs.0 << 21) | (ra.0 << 16) | (rb.0 << 11);
    ins(w | (31 << 1), Fm::Rot)
}

/// `rlwinm ra, rs, sh, mb, me`.
#[must_use]
pub const fn rlwinm(ra: R, rs: R, sh: u32, mb: u32, me: u32) -> Ins {
    let w = (21 << 26) | (rs.0 << 21) | (ra.0 << 16) | ((sh & 31) << 11);
    ins(w | (mb << 6) | (me << 1), Fm::Rlwinm)
}

/// `cror bt, ba, bb`.
#[must_use]
pub const fn cror(bt: u32, ba: u32, bb: u32) -> Ins {
    ins(
        (19 << 26) | (bt << 21) | (ba << 16) | (bb << 11) | (449 << 1),
        Fm::Cror,
    )
}

// Branches.

/// `bc` on `c`, `off` bytes on (short: within 32 KiB).
#[must_use]
pub const fn bc(c: Cond, off: i64) -> Ins {
    ins(
        (16 << 26) | (c.bo << 21) | (c.bi << 16) | (off as u32 & 0xfffc),
        Fm::Bc,
    )
}

/// `b` `off` bytes on.
#[must_use]
pub const fn b(off: i64) -> Ins {
    ins((18 << 26) | (off as u32 & 0x03ff_fffc), Fm::B)
}

/// `bl` to itself (relocated).
#[must_use]
pub const fn bl() -> Ins {
    ins((18 << 26) | 1, Fm::B)
}

/// Whether `d` bytes fit a `b`.
#[must_use]
pub const fn reaches_b(d: i64) -> bool {
    -(1 << 25) <= d && d < (1 << 25)
}

// Special registers.

/// `mflr rt`.
#[must_use]
pub const fn mflr(rt: R) -> Ins {
    ins(0x7c08_02a6 | (rt.0 << 21), Fm::Spr)
}

/// `mtlr rs`.
#[must_use]
pub const fn mtlr(rs: R) -> Ins {
    ins(0x7c08_03a6 | (rs.0 << 21), Fm::Spr)
}

/// `mtctr rs`.
#[must_use]
pub const fn mtctr(rs: R) -> Ins {
    ins(0x7c09_03a6 | (rs.0 << 21), Fm::Spr)
}

// Floats.

/// A float A / X-form operation; `fneg`, `fmr` and the conversions to /
/// from integers but `fcfid` take no single precision form.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Fp {
    Frsp = 12,
    Fdiv = 18,
    Fsub = 20,
    Fadd = 21,
    Fsqrt = 22,
    Fneg = 40,
    Fmr = 72,
    Fctidz = 815,
    Fcfid = 846,
    Fctiduz = 943,
    Fcfidu = 974,
}

/// `op(s) frt, fra, frb` (`single`: primary 59, single precision).
#[must_use]
pub const fn fp(op: Fp, single: bool, frt: F, fra: F, frb: F) -> Ins {
    let p = if single { 59 } else { 63 };
    ins(
        (p << 26) | (frt.0 << 21) | (fra.0 << 16) | (frb.0 << 11) | ((op as u32) << 1),
        Fm::Fp,
    )
}

/// `fmul(s) frt, fra, frc`.
#[must_use]
pub const fn fmul(single: bool, frt: F, fra: F, frc: F) -> Ins {
    let p = if single { 59 } else { 63 };
    ins(
        (p << 26) | (frt.0 << 21) | (fra.0 << 16) | (frc.0 << 6) | (25 << 1),
        Fm::Fp,
    )
}

/// `fmadd(s) frt, fra, frc, frb`: `fra * frc + frb`, rounded once.
#[must_use]
pub const fn fmadd(single: bool, frt: F, fra: F, frc: F, frb: F) -> Ins {
    let p = if single { 59 } else { 63 };
    ins(
        (p << 26) | (frt.0 << 21) | (fra.0 << 16) | (frb.0 << 11) | (frc.0 << 6) | (29 << 1),
        Fm::Fp,
    )
}

/// `fcmpu cr0, fra, frb`.
#[must_use]
pub const fn fcmpu(fra: F, frb: F) -> Ins {
    ins((63 << 26) | (fra.0 << 16) | (frb.0 << 11), Fm::Fcmpu)
}

// Vectors (VMX).

/// A VX / VC-form operation on lanes of a width.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Vop {
    Add,
    Sub,
    /// 32-bit lanes only (`vmuluwm`).
    Mul,
    And,
    Or,
    Xor,
    Andc,
    Nor,
    Eq,
    GtS,
    GtU,
    MinS,
    MinU,
    MaxS,
    MaxU,
    Sl,
    Sr,
    Sra,
}

/// The `xo` of `op` on lanes of `w` bytes (1, 2, 4, 8).
const fn vxo(op: Vop, w: u32) -> u32 {
    let i = w.trailing_zeros() as usize;
    let xs = match op {
        Vop::Add => [0x000, 0x040, 0x080, 0x0c0],
        Vop::Sub => [0x400, 0x440, 0x480, 0x4c0],
        Vop::Mul => [0x089; 4],
        Vop::And => [0x404; 4],
        Vop::Or => [0x484; 4],
        Vop::Xor => [0x4c4; 4],
        Vop::Andc => [0x444; 4],
        Vop::Nor => [0x504; 4],
        Vop::Eq => [0x006, 0x046, 0x086, 0x0c7],
        Vop::GtS => [0x306, 0x346, 0x386, 0x3c7],
        Vop::GtU => [0x206, 0x246, 0x286, 0x2c7],
        Vop::MinS => [0x302, 0x342, 0x382, 0x3c2],
        Vop::MinU => [0x202, 0x242, 0x282, 0x2c2],
        Vop::MaxS => [0x102, 0x142, 0x182, 0x1c2],
        Vop::MaxU => [0x002, 0x042, 0x082, 0x0c2],
        Vop::Sl => [0x104, 0x144, 0x184, 0x5c4],
        Vop::Sr => [0x204, 0x244, 0x284, 0x6c4],
        Vop::Sra => [0x304, 0x344, 0x384, 0x3c4],
    };
    xs[i]
}

/// `op vd, va, vb` on lanes of `w` bytes.
#[must_use]
pub const fn vx(op: Vop, w: u32, vd: V, va: V, vb: V) -> Ins {
    ins(
        (4 << 26) | (vd.0 << 21) | (va.0 << 16) | (vb.0 << 11) | vxo(op, w),
        Fm::Vx,
    )
}

/// The recording form (`.`, sets `cr6`) of compare `op`.
#[must_use]
pub const fn vx_rc(op: Vop, w: u32, vd: V, va: V, vb: V) -> Ins {
    let i = vx(op, w, vd, va, vb);
    ins(i.word | 0x400, Fm::Vx)
}

/// `vmladduhm vd, va, vb, vc`.
#[must_use]
pub const fn vmladduhm(vd: V, va: V, vb: V, vc: V) -> Ins {
    ins(
        (4 << 26) | (vd.0 << 21) | (va.0 << 16) | (vb.0 << 11) | (vc.0 << 6) | 0x22,
        Fm::Va,
    )
}

/// `vspltisb vd, imm` (-16 .. 15).
#[must_use]
pub const fn vspltisb(vd: V, imm: i32) -> Ins {
    ins(
        0x1000_030c | (vd.0 << 21) | ((imm as u32 & 31) << 16),
        Fm::Vsplt,
    )
}

// Vectors (VSX, on `vs32 + n`).

/// `lxvd2x vs32 + t, ra, rb`.
#[must_use]
pub const fn lxvd2x(t: V, ra: R, rb: R) -> Ins {
    ins(
        0x7c00_0698 | (t.0 << 21) | (ra.0 << 16) | (rb.0 << 11) | 1,
        Fm::Xx1,
    )
}

/// `stxvd2x vs32 + s, ra, rb`.
#[must_use]
pub const fn stxvd2x(s: V, ra: R, rb: R) -> Ins {
    ins(
        0x7c00_0798 | (s.0 << 21) | (ra.0 << 16) | (rb.0 << 11) | 1,
        Fm::Xx1,
    )
}

/// `mtvsrd vs32 + t, ra`.
#[must_use]
pub const fn mtvsrd(t: V, ra: R) -> Ins {
    ins(0x7c00_0166 | (t.0 << 21) | (ra.0 << 16) | 1, Fm::Xx1)
}

/// An XX3-form operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Xv {
    Add,
    Sub,
    Mul,
    Div,
    CmpEq,
    CmpGt,
    CmpGe,
    Max,
    Min,
    /// `t += a * b`, rounded once (`xvmadda`).
    Madd,
}

/// `xv<op>sp` / `xv<op>dp` `vs32 + t, vs32 + a, vs32 + b`.
#[must_use]
pub const fn xv(op: Xv, dp: bool, t: V, a: V, b: V) -> Ins {
    let base = match op {
        Xv::Add => 0xf000_0200,
        Xv::Sub => 0xf000_0240,
        Xv::Mul => 0xf000_0280,
        Xv::Div => 0xf000_02c0,
        Xv::CmpEq => 0xf000_0218,
        Xv::CmpGt => 0xf000_0258,
        Xv::CmpGe => 0xf000_0298,
        Xv::Max => 0xf000_0600,
        Xv::Min => 0xf000_0640,
        Xv::Madd => 0xf000_0208,
    } | ((dp as u32) << 8);
    ins(base | (t.0 << 21) | (a.0 << 16) | (b.0 << 11) | 7, Fm::Xx3)
}

/// `xxspltd vs32 + t, vs32 + a, 0` (`xxpermdi t, a, a, 0`).
#[must_use]
pub const fn xxspltd0(t: V, a: V) -> Ins {
    ins(
        0xf000_0050 | (t.0 << 21) | (a.0 << 16) | (a.0 << 11) | 7,
        Fm::Xx3,
    )
}

/// `xvsqrtsp` / `xvsqrtdp` `vs32 + t, vs32 + b`.
#[must_use]
pub const fn xvsqrt(dp: bool, t: V, b: V) -> Ins {
    let base = if dp { 0xf000_032c } else { 0xf000_022c };
    ins(base | (t.0 << 21) | (b.0 << 11) | 3, Fm::Xx2)
}

/// The code of a function in the making.
#[derive(Default)]
pub struct Asm {
    pub code: Vec<u32>,
    /// Each word's form, with text on.
    text: Option<Vec<Fm>>,
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

    /// Point the `b` at `at` `d` bytes on.
    pub fn patch_b(&mut self, at: usize, d: i64) {
        self.code[at] |= (d as u32) & 0x03ff_fffc;
    }

    /// Point the forward `bc` at `at` `d` bytes on.
    pub fn patch_bc(&mut self, at: usize, d: u32) {
        self.code[at] |= d & 0xfffc;
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
