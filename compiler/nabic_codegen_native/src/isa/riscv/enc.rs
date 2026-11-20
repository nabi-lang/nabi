//! RISC-V instruction encoder (RV64GC and RV32IMAC): one function per
//! 32-bit instruction form the emitter uses (`compress` gives a word's
//! 16-bit form), the only place that knows opcodes and bit fields. Each
//! returns an `Ins`: the word and its form. `Asm` keeps the words and,
//! with text on (`--emit=asm`), each word's form; `lines` decodes them
//! into what `llvm-objdump -d` prints (its preferred aliases, hex
//! immediates). Off, only the words are kept.
//!
//! Fixed choices: `fcvt.d.s` is written with `rm` = `rne` (the field is
//! ignored, objdump shows no rounding mode for it), the other float
//! operations with `dyn` unless they name one.

#[cfg(test)]
mod tests;

use crate::asm::Listing;

/// An integer register, 0 .. 31.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct X(pub u32);

/// A float register, 0 .. 31.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct F(pub u32);

pub const ZERO: X = X(0);
pub const RA: X = X(1);

/// A branch condition (funct3 of `BRANCH`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Cond(u32);

impl Cond {
    pub const EQ: Self = Self(0);
    pub const NE: Self = Self(1);
    pub const LT: Self = Self(4);
    pub const GE: Self = Self(5);
    pub const LTU: Self = Self(6);
    pub const GEU: Self = Self(7);

    /// The opposite condition.
    #[must_use]
    pub const fn inv(self) -> Self {
        Self(self.0 ^ 1)
    }
}

// Major opcodes.
const LOAD: u32 = 0x03;
const LOAD_FP: u32 = 0x07;
const MISC_MEM: u32 = 0x0f;
const OP_IMM: u32 = 0x13;
const AUIPC: u32 = 0x17;
const OP_IMM_32: u32 = 0x1b;
const STORE: u32 = 0x23;
const STORE_FP: u32 = 0x27;
const AMO: u32 = 0x2f;
const OP: u32 = 0x33;
const LUI: u32 = 0x37;
const OP_32: u32 = 0x3b;
const OP_FP: u32 = 0x53;
const MADD: u32 = 0x43;
const BRANCH: u32 = 0x63;
const JALR: u32 = 0x67;
const JAL: u32 = 0x6f;
const SYSTEM: u32 = 0x73;

/// The form of a word, for its text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Fm {
    Op,
    OpImm,
    OpImm32,
    /// `Zbb` rotates.
    Rot,
    Lui,
    Auipc,
    Load,
    Store,
    Branch,
    Jal,
    Jalr,
    Amo,
    Fp,
    /// `fmadd` (R4).
    Fma,
    Fence,
    Unimp,
    /// V (`riscv::vector`).
    V,
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

const fn r_type(f7: u32, rs2: u32, rs1: u32, f3: u32, rd: u32, op: u32) -> u32 {
    (f7 << 25) | (rs2 << 20) | (rs1 << 15) | (f3 << 12) | (rd << 7) | op
}

const fn i_type(imm: i32, rs1: u32, f3: u32, rd: u32, op: u32) -> u32 {
    (((imm as u32) & 0xfff) << 20) | (rs1 << 15) | (f3 << 12) | (rd << 7) | op
}

const fn s_type(imm: i32, rs2: u32, rs1: u32, f3: u32, op: u32) -> u32 {
    let v = imm as u32;
    (((v >> 5) & 0x7f) << 25) | (rs2 << 20) | (rs1 << 15) | (f3 << 12) | ((v & 0x1f) << 7) | op
}

/// B-type immediate fields of byte offset `off`.
const fn b_imm(off: i32) -> u32 {
    let v = off as u32;
    (((v >> 12) & 1) << 31)
        | (((v >> 5) & 0x3f) << 25)
        | (((v >> 1) & 0xf) << 8)
        | (((v >> 11) & 1) << 7)
}

/// J-type immediate fields of byte offset `off`.
const fn j_imm(off: i32) -> u32 {
    let v = off as u32;
    (((v >> 20) & 1) << 31)
        | (((v >> 1) & 0x3ff) << 21)
        | (((v >> 11) & 1) << 20)
        | (((v >> 12) & 0xff) << 12)
}

// Integer register-register.

/// An `OP` operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Op {
    Add,
    Sub,
    Sll,
    Slt,
    Sltu,
    Xor,
    Srl,
    Sra,
    Or,
    And,
    Mul,
    Mulh,
    Mulhu,
    Div,
    Divu,
    Rem,
    Remu,
}

impl Op {
    /// `(funct7, funct3)`.
    const fn bits(self) -> (u32, u32) {
        match self {
            Self::Add => (0, 0),
            Self::Sub => (0x20, 0),
            Self::Sll => (0, 1),
            Self::Slt => (0, 2),
            Self::Sltu => (0, 3),
            Self::Xor => (0, 4),
            Self::Srl => (0, 5),
            Self::Sra => (0x20, 5),
            Self::Or => (0, 6),
            Self::And => (0, 7),
            Self::Mul => (1, 0),
            Self::Mulh => (1, 1),
            Self::Mulhu => (1, 3),
            Self::Div => (1, 4),
            Self::Divu => (1, 5),
            Self::Rem => (1, 6),
            Self::Remu => (1, 7),
        }
    }
}

/// `op rd, rs1, rs2`.
#[must_use]
pub const fn op(o: Op, rd: X, rs1: X, rs2: X) -> Ins {
    let (f7, f3) = o.bits();
    ins(r_type(f7, rs2.0, rs1.0, f3, rd.0, OP), Fm::Op)
}

// Integer register-immediate.

/// `addi rd, rs1, imm`.
#[must_use]
pub const fn addi(rd: X, rs1: X, imm: i32) -> Ins {
    ins(i_type(imm, rs1.0, 0, rd.0, OP_IMM), Fm::OpImm)
}

/// `sltiu rd, rs1, imm` (`seqz` for 1).
#[must_use]
pub const fn sltiu(rd: X, rs1: X, imm: i32) -> Ins {
    ins(i_type(imm, rs1.0, 3, rd.0, OP_IMM), Fm::OpImm)
}

/// `xori rd, rs1, imm` (`not` for -1).
#[must_use]
pub const fn xori(rd: X, rs1: X, imm: i32) -> Ins {
    ins(i_type(imm, rs1.0, 4, rd.0, OP_IMM), Fm::OpImm)
}

/// `ori rd, rs1, imm`.
#[must_use]
pub const fn ori(rd: X, rs1: X, imm: i32) -> Ins {
    ins(i_type(imm, rs1.0, 6, rd.0, OP_IMM), Fm::OpImm)
}

/// `andi rd, rs1, imm`.
#[must_use]
pub const fn andi(rd: X, rs1: X, imm: i32) -> Ins {
    ins(i_type(imm, rs1.0, 7, rd.0, OP_IMM), Fm::OpImm)
}

/// `slli rd, rs1, sh` (`sh` < XLEN).
#[must_use]
pub const fn slli(rd: X, rs1: X, sh: u32) -> Ins {
    ins(i_type(sh as i32, rs1.0, 1, rd.0, OP_IMM), Fm::OpImm)
}

/// `srli rd, rs1, sh`.
#[must_use]
pub const fn srli(rd: X, rs1: X, sh: u32) -> Ins {
    ins(i_type(sh as i32, rs1.0, 5, rd.0, OP_IMM), Fm::OpImm)
}

/// `srai rd, rs1, sh`.
#[must_use]
pub const fn srai(rd: X, rs1: X, sh: u32) -> Ins {
    ins(
        i_type((0x400 | sh) as i32, rs1.0, 5, rd.0, OP_IMM),
        Fm::OpImm,
    )
}

/// `rol` / `ror` (`left`) `rd, rs1, rs2` (`Zbb`); `w`: `rolw` /
/// `rorw` (rv64, the low word).
#[must_use]
pub const fn rot(left: bool, w: bool, rd: X, rs1: X, rs2: X) -> Ins {
    let f3 = if left { 1 } else { 5 };
    let op = if w { OP_32 } else { OP };
    ins(r_type(0x30, rs2.0, rs1.0, f3, rd.0, op), Fm::Rot)
}

/// `rori rd, rs1, sh` (`Zbb`; `sh` < XLEN); `w`: `roriw` (rv64, `sh`
/// < 32).
#[must_use]
pub const fn rori(w: bool, rd: X, rs1: X, sh: u32) -> Ins {
    let op = if w { OP_IMM_32 } else { OP_IMM };
    ins(i_type((0x600 | sh) as i32, rs1.0, 5, rd.0, op), Fm::Rot)
}

/// `v` as `lui` / `auipc` and 12-bit parts, `(hi << 12) + lo == v`
/// (`lo` sign-extended: `hi` rounds up for a negative one; `hi` wraps to
/// 20 bits).
#[must_use]
pub const fn hi_lo(v: i64) -> (u32, i32) {
    let hi = (v + 0x800) >> 12;
    (hi as u32, (v - (hi << 12)) as i32)
}

/// `addiw rd, rs1, imm` (rv64; `sext.w` for 0).
#[must_use]
pub const fn addiw(rd: X, rs1: X, imm: i32) -> Ins {
    ins(i_type(imm, rs1.0, 0, rd.0, OP_IMM_32), Fm::OpImm32)
}

/// `lui rd, imm20`.
#[must_use]
pub const fn lui(rd: X, imm20: u32) -> Ins {
    ins(((imm20 & 0xf_ffff) << 12) | (rd.0 << 7) | LUI, Fm::Lui)
}

/// `auipc rd, imm20`.
#[must_use]
pub const fn auipc(rd: X, imm20: u32) -> Ins {
    ins(((imm20 & 0xf_ffff) << 12) | (rd.0 << 7) | AUIPC, Fm::Auipc)
}

// Loads and stores.

/// A load or store and its width (`Fl` / `Fs`: float, `true` double).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Ls {
    Lb,
    Lh,
    Lw,
    Ld,
    Lbu,
    Lhu,
    Lwu,
    Sb,
    Sh,
    Sw,
    Sd,
    Fl(bool),
    Fs(bool),
}

/// `ls rt, off(base)`: `rt` an integer register, or a float one for
/// `Fl` / `Fs`; `off` a 12-bit offset.
#[must_use]
pub const fn ldst(o: Ls, rt: u32, base: X, off: i32) -> Ins {
    let b = base.0;
    let w = match o {
        Ls::Lb => i_type(off, b, 0, rt, LOAD),
        Ls::Lh => i_type(off, b, 1, rt, LOAD),
        Ls::Lw => i_type(off, b, 2, rt, LOAD),
        Ls::Ld => i_type(off, b, 3, rt, LOAD),
        Ls::Lbu => i_type(off, b, 4, rt, LOAD),
        Ls::Lhu => i_type(off, b, 5, rt, LOAD),
        Ls::Lwu => i_type(off, b, 6, rt, LOAD),
        Ls::Fl(d) => i_type(off, b, 2 + d as u32, rt, LOAD_FP),
        Ls::Sb => s_type(off, rt, b, 0, STORE),
        Ls::Sh => s_type(off, rt, b, 1, STORE),
        Ls::Sw => s_type(off, rt, b, 2, STORE),
        Ls::Sd => s_type(off, rt, b, 3, STORE),
        Ls::Fs(d) => s_type(off, rt, b, 2 + d as u32, STORE_FP),
    };
    let f = if matches!(o, Ls::Sb | Ls::Sh | Ls::Sw | Ls::Sd | Ls::Fs(_)) {
        Fm::Store
    } else {
        Fm::Load
    };
    ins(w, f)
}

// Control transfer.

/// `b<c> rs1, rs2, off` (bytes; 0 to patch).
#[must_use]
pub const fn branch(c: Cond, rs1: X, rs2: X, off: i32) -> Ins {
    ins(
        b_imm(off) | (rs2.0 << 20) | (rs1.0 << 15) | (c.0 << 12) | BRANCH,
        Fm::Branch,
    )
}

/// `jal rd, off` (bytes; 0 to patch).
#[must_use]
pub const fn jal(rd: X, off: i32) -> Ins {
    ins(j_imm(off) | (rd.0 << 7) | JAL, Fm::Jal)
}

/// `jalr rd, off(rs1)`.
#[must_use]
pub const fn jalr(rd: X, rs1: X, off: i32) -> Ins {
    ins(i_type(off, rs1.0, 0, rd.0, JALR), Fm::Jalr)
}

/// Whether byte offset `d` fits a `jal`.
#[must_use]
pub const fn reaches_jal(d: i64) -> bool {
    -(1 << 20) <= d && d < (1 << 20)
}

// Atomics and fences.

/// An `AMO` operation (`funct5`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Amo {
    Add = 0x00,
    Swap = 0x01,
    Lr = 0x02,
    Sc = 0x03,
    Xor = 0x04,
    Or = 0x08,
    And = 0x0c,
}

/// `amo<op>.w` / `.d` (`d`) `rd, rs2, (rs1)` (`lr`: no `rs2`), `.aq`
/// and `.rl` as asked.
#[must_use]
pub const fn amo_o(o: Amo, d: bool, aq: bool, rl: bool, rd: X, rs1: X, rs2: X) -> Ins {
    let f7 = ((o as u32) << 2) | ((aq as u32) << 1) | rl as u32;
    ins(r_type(f7, rs2.0, rs1.0, 2 + d as u32, rd.0, AMO), Fm::Amo)
}

/// `fence` ordering bits.
pub const R: u32 = 2;
pub const W: u32 = 1;

/// `fence pred, succ`.
#[must_use]
pub const fn fence(pred: u32, succ: u32) -> Ins {
    ins((pred << 24) | (succ << 20) | MISC_MEM, Fm::Fence)
}

/// `unimp` (`csrrw zero, cycle, zero`): an illegal instruction.
#[must_use]
pub const fn unimp() -> Ins {
    ins(0xc000_1000 | SYSTEM, Fm::Unimp)
}

// Floats.

/// A rounding mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Rm {
    Rne = 0,
    Rtz = 1,
    Dyn = 7,
}

const fn fp(f5: u32, d: bool, rs2: u32, rs1: u32, rm: u32, rd: u32) -> Ins {
    ins(
        r_type((f5 << 2) | d as u32, rs2, rs1, rm, rd, OP_FP),
        Fm::Fp,
    )
}

/// A float arithmetic operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FArith {
    Add = 0,
    Sub = 1,
    Mul = 2,
    Div = 3,
}

/// `f<op>.s` / `.d` (`d`) `fd, fs1, fs2`.
#[must_use]
pub const fn farith(o: FArith, d: bool, fd: F, fs1: F, fs2: F, rm: Rm) -> Ins {
    fp(o as u32, d, fs2.0, fs1.0, rm as u32, fd.0)
}

/// `fmadd.s` / `.d` (`d`) `fd, fs1, fs2, fs3`: `fs1 * fs2 + fs3`,
/// rounded once.
#[must_use]
pub const fn fmadd(d: bool, fd: F, fs1: F, fs2: F, fs3: F, rm: Rm) -> Ins {
    let w = (fs3.0 << 27) | ((d as u32) << 25) | (fs2.0 << 20) | (fs1.0 << 15);
    ins(w | ((rm as u32) << 12) | (fd.0 << 7) | MADD, Fm::Fma)
}

/// `fsqrt fd, fs`.
#[must_use]
pub const fn fsqrt(d: bool, fd: F, fs: F, rm: Rm) -> Ins {
    fp(0x0b, d, 0, fs.0, rm as u32, fd.0)
}

/// `fsgnj` (`fmv` for `fs1 == fs2`) or, `neg`, `fsgnjn` (`fneg`).
#[must_use]
pub const fn fsgnj(neg: bool, d: bool, fd: F, fs1: F, fs2: F) -> Ins {
    fp(0x04, d, fs2.0, fs1.0, neg as u32, fd.0)
}

/// A float compare.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FCmp {
    Le = 0,
    Lt = 1,
    Eq = 2,
}

/// `f<c> rd, fs1, fs2`.
#[must_use]
pub const fn fcmp(c: FCmp, d: bool, rd: X, fs1: F, fs2: F) -> Ins {
    fp(0x14, d, fs2.0, fs1.0, c as u32, rd.0)
}

/// An integer type of a conversion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IntTy {
    L = 2,
    Lu = 3,
}

/// `fcvt.<t>.s` / `.d` `rd, fs, rm`.
#[must_use]
pub const fn fcvt_to_int(t: IntTy, d: bool, rd: X, fs: F, rm: Rm) -> Ins {
    fp(0x18, d, t as u32, fs.0, rm as u32, rd.0)
}

/// `fcvt.s` / `.d.<t> fd, rs`.
#[must_use]
pub const fn fcvt_from_int(d: bool, t: IntTy, fd: F, rs: X, rm: Rm) -> Ins {
    fp(0x1a, d, t as u32, rs.0, rm as u32, fd.0)
}

/// `fcvt.d.s fd, fs` (`d`) or `fcvt.s.d`.
#[must_use]
pub const fn fcvt_ff(d: bool, fd: F, fs: F, rm: Rm) -> Ins {
    fp(0x08, d, !d as u32, fs.0, rm as u32, fd.0)
}

/// `fmv.x.w` / `fmv.x.d` `rd, fs`.
#[must_use]
pub const fn fmv_x(d: bool, rd: X, fs: F) -> Ins {
    ins(r_type(0x70 | d as u32, 0, fs.0, 0, rd.0, OP_FP), Fm::Fp)
}

/// `fmv.w.x` / `fmv.d.x` `fd, rs`.
#[must_use]
pub const fn fmv_f(d: bool, fd: F, rs: X) -> Ins {
    ins(r_type(0x78 | d as u32, 0, rs.0, 0, fd.0, OP_FP), Fm::Fp)
}

// Compressed instructions (`C`).

/// A 16-bit form: the mnemonic `--emit=asm` writes, by its operands.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Cm {
    Nop,
    Unimp,
    /// `rd, imm`.
    Imm(&'static str),
    Lui,
    Addi16sp,
    Addi4spn,
    /// `rd, rs2`.
    Reg(&'static str),
    /// `rt, off(base)`.
    Mem(&'static str),
    /// `rs1`.
    Jr(&'static str),
    /// `target`.
    J(&'static str),
    /// `rs1, target`.
    B(&'static str),
}

/// A compressible register (`x8`-`x15`).
const fn cr(r: u32) -> bool {
    r >= 8 && r < 16
}

const fn fits6(v: i32) -> bool {
    -32 <= v && v < 32
}

/// `b` of `v` at `at`.
const fn bit(v: u32, b: u32, at: u32) -> u32 {
    ((v >> b) & 1) << at
}

/// CI: `funct3 imm[5] rd imm[4:0] op`.
const fn ci(f3: u32, imm: i32, rd: u32, op: u32) -> u32 {
    let v = imm as u32;
    (f3 << 13) | bit(v, 5, 12) | (rd << 7) | ((v & 0x1f) << 2) | op
}

/// CA: `100 f6 rd' f2 rs2' 01`.
const fn ca(f6lo: u32, rd: u32, f2: u32, rs2: u32) -> u32 {
    (0x23 << 10) | (f6lo << 12) | ((rd - 8) << 7) | (f2 << 5) | ((rs2 - 8) << 2) | 1
}

/// CL / CS: `funct3 uimm[5:3] rs1' uimm[2|6] or [7:6] rd' 00`, `d`: the
/// 8-byte offset form.
const fn cl(f3: u32, off: u32, rs1: u32, rd: u32, d: bool) -> u32 {
    let lo = if d {
        ((off >> 6) & 3) << 5
    } else {
        bit(off, 2, 6) | bit(off, 6, 5)
    };
    (f3 << 13) | (((off >> 3) & 7) << 10) | ((rs1 - 8) << 7) | lo | ((rd - 8) << 2)
}

/// `c.j` / `c.jal` offset fields.
const fn cj_imm(off: i32) -> u32 {
    let v = off as u32;
    bit(v, 11, 12)
        | bit(v, 4, 11)
        | (((v >> 8) & 3) << 9)
        | bit(v, 10, 8)
        | bit(v, 6, 7)
        | bit(v, 7, 6)
        | (((v >> 1) & 7) << 3)
        | bit(v, 5, 2)
}

/// `c.beqz` / `c.bnez` offset fields.
const fn cb_imm(off: i32) -> u32 {
    let v = off as u32;
    bit(v, 8, 12)
        | (((v >> 3) & 3) << 10)
        | (((v >> 6) & 3) << 5)
        | (((v >> 1) & 3) << 3)
        | bit(v, 5, 2)
}

/// The offset of a branch word.
const fn b_off(w: u32) -> i32 {
    (((w as i32) >> 31) << 12)
        | ((bit(w, 7, 11) | (((w >> 25) & 0x3f) << 5) | (((w >> 8) & 0xf) << 1)) as i32)
}

/// The offset of a `jal` word.
const fn j_off(w: u32) -> i32 {
    (((w as i32) >> 31) << 20)
        | (((((w >> 12) & 0xff) << 12) | bit(w, 20, 11) | (((w >> 21) & 0x3ff) << 1)) as i32)
}

/// The 16-bit form of word `w` (rv64: `rv64`), its 32-bit equivalent as
/// objdump shows it (and its form), and its mnemonic; none if `w` has
/// none (a branch or jump: its offset out of reach). As `llvm-mc` with
/// `C` compresses: `c.addi` before `c.addi16sp`, commuted `add` / `xor`
/// / `or` / `and` / `beq` / `bne`.
#[allow(clippy::too_many_lines)]
fn cform(w: u32, rv64: bool) -> Option<(u16, u32, Fm, Cm)> {
    let (rd, f3, rs1, rs2, f7) = (
        bits(w, 7, 5),
        bits(w, 12, 3),
        bits(w, 15, 5),
        bits(w, 20, 5),
        bits(w, 25, 7),
    );
    let imm = (w as i32) >> 20;
    let s_imm = (((w as i32) >> 25) << 5) | bits(w, 7, 5) as i32;
    let sp = 2;
    let r = |h: u32, c: u32, f: Fm, m: Cm| Some((h as u16, c, f, m));
    match w & 0x7f {
        OP_IMM => match f3 {
            0 if rd == 0 => (rs1 == 0 && imm == 0).then_some((1, w, Fm::OpImm, Cm::Nop)),
            0 if rs1 == 0 && fits6(imm) => r(ci(2, imm, rd, 1), w, Fm::OpImm, Cm::Imm("c.li")),
            0 if rd == rs1 && imm != 0 && fits6(imm) => {
                r(ci(0, imm, rd, 1), w, Fm::OpImm, Cm::Imm("c.addi"))
            }
            0 if rd == sp
                && rs1 == sp
                && imm != 0
                && imm % 16 == 0
                && (-512..512).contains(&imm) =>
            {
                let v = imm as u32;
                let h = (3 << 13)
                    | bit(v, 9, 12)
                    | (sp << 7)
                    | bit(v, 4, 6)
                    | bit(v, 6, 5)
                    | (((v >> 7) & 3) << 3)
                    | bit(v, 5, 2)
                    | 1;
                r(h, w, Fm::OpImm, Cm::Addi16sp)
            }
            0 if imm == 0 && rs1 != 0 => r(
                (0x8 << 12) | (rd << 7) | (rs1 << 2) | 2,
                w,
                Fm::OpImm,
                Cm::Reg("c.mv"),
            ),
            0 if rs1 == sp && cr(rd) && imm > 0 && imm % 4 == 0 && imm < 1024 => {
                let v = imm as u32;
                let h = (((v >> 4) & 3) << 11)
                    | (((v >> 6) & 0xf) << 7)
                    | bit(v, 2, 6)
                    | bit(v, 3, 5)
                    | ((rd - 8) << 2);
                r(h, w, Fm::OpImm, Cm::Addi4spn)
            }
            1 if rd == rs1 && rd != 0 && imm & 0x3f != 0 && imm >> 6 == 0 => {
                r(ci(0, imm, rd, 2), w, Fm::OpImm, Cm::Imm("c.slli"))
            }
            5 if rd == rs1 && cr(rd) && imm & 0x3f != 0 && (imm >> 6 == 0 || imm >> 6 == 0x10) => {
                let (f2, mn) = if imm >> 6 == 0 {
                    (0, "c.srli")
                } else {
                    (1, "c.srai")
                };
                let h = ci(4, imm & 0x3f, rd - 8, 1) | (f2 << 10);
                r(h, w, Fm::OpImm, Cm::Imm(mn))
            }
            7 if rd == rs1 && cr(rd) && fits6(imm) => r(
                ci(4, imm, rd - 8, 1) | (2 << 10),
                w,
                Fm::OpImm,
                Cm::Imm("c.andi"),
            ),
            _ => None,
        },
        OP_IMM_32 if rv64 && f3 == 0 && rd == rs1 && rd != 0 && fits6(imm) => {
            r(ci(1, imm, rd, 1), w, Fm::OpImm32, Cm::Imm("c.addiw"))
        }
        LUI => {
            let v = w >> 12;
            let ok = rd != 0 && rd != sp && v != 0 && !(32..0xf_ffe0).contains(&v);
            ok.then(|| {
                (
                    (ci(3, (v & 0x3f) as i32, rd, 1)) as u16,
                    w,
                    Fm::Lui,
                    Cm::Lui,
                )
            })
        }
        OP => {
            let canon = |a: u32, b: u32| r_type(f7, b, a, f3, rd, OP);
            match (f7, f3) {
                (0, 0) if rd == 0 => None,
                (0, 0) if rs1 == 0 && rs2 != 0 => r(
                    (0x8 << 12) | (rd << 7) | (rs2 << 2) | 2,
                    i_type(0, rs2, 0, rd, OP_IMM),
                    Fm::OpImm,
                    Cm::Reg("c.mv"),
                ),
                (0, 0) if rs2 == 0 && rs1 != 0 => r(
                    (0x8 << 12) | (rd << 7) | (rs1 << 2) | 2,
                    i_type(0, rs1, 0, rd, OP_IMM),
                    Fm::OpImm,
                    Cm::Reg("c.mv"),
                ),
                (0, 0) if rd == rs1 && rs2 != 0 => r(
                    (0x9 << 12) | (rd << 7) | (rs2 << 2) | 2,
                    w,
                    Fm::Op,
                    Cm::Reg("c.add"),
                ),
                (0, 0) if rd == rs2 && rs1 != 0 => r(
                    (0x9 << 12) | (rd << 7) | (rs1 << 2) | 2,
                    canon(rd, rs1),
                    Fm::Op,
                    Cm::Reg("c.add"),
                ),
                (0x20, 0) if rd == rs1 && cr(rd) && cr(rs2) => {
                    r(ca(0, rd, 0, rs2), w, Fm::Op, Cm::Reg("c.sub"))
                }
                (0, 4 | 6 | 7) if cr(rd) && cr(rs1) && cr(rs2) && (rd == rs1 || rd == rs2) => {
                    let o = if rd == rs1 { rs2 } else { rs1 };
                    let (f2, mn) = match f3 {
                        4 => (1, "c.xor"),
                        6 => (2, "c.or"),
                        _ => (3, "c.and"),
                    };
                    r(ca(0, rd, f2, o), canon(rd, o), Fm::Op, Cm::Reg(mn))
                }
                _ => None,
            }
        }
        LOAD | LOAD_FP => {
            let fp = w & 0x7f == LOAD_FP;
            // `ld` / `fld` (8 bytes), `lw` (4; rv32 has no float).
            let (d, f3c, mn, mnsp) = match (f3, fp) {
                (2, false) => (false, 2, "c.lw", "c.lwsp"),
                (3, false) if rv64 => (true, 3, "c.ld", "c.ldsp"),
                (3, true) if rv64 => (true, 1, "c.fld", "c.fldsp"),
                _ => return None,
            };
            let a = if d { 8 } else { 4 };
            if imm < 0 || imm % a != 0 {
                return None;
            }
            let v = imm as u32;
            if rs1 == sp && (rd != 0 || fp) && v < 64 * a as u32 {
                let lo = if d {
                    (((v >> 3) & 3) << 5) | (((v >> 6) & 7) << 2)
                } else {
                    (((v >> 2) & 7) << 4) | (((v >> 6) & 3) << 2)
                };
                return r(
                    (f3c << 13) | bit(v, 5, 12) | (rd << 7) | lo | 2,
                    w,
                    Fm::Load,
                    Cm::Mem(mnsp),
                );
            }
            if cr(rs1) && cr(rd) && v < 32 * a as u32 {
                return r(cl(f3c, v, rs1, rd, d), w, Fm::Load, Cm::Mem(mn));
            }
            None
        }
        STORE | STORE_FP => {
            let fp = w & 0x7f == STORE_FP;
            let (d, f3c, mn, mnsp) = match (f3, fp) {
                (2, false) => (false, 6, "c.sw", "c.swsp"),
                (3, false) if rv64 => (true, 7, "c.sd", "c.sdsp"),
                (3, true) if rv64 => (true, 5, "c.fsd", "c.fsdsp"),
                _ => return None,
            };
            let a = if d { 8 } else { 4 };
            if s_imm < 0 || s_imm % a != 0 {
                return None;
            }
            let v = s_imm as u32;
            if rs1 == sp && v < 64 * a as u32 {
                let f = if d {
                    (((v >> 3) & 7) << 10) | (((v >> 6) & 7) << 7)
                } else {
                    (((v >> 2) & 0xf) << 9) | (((v >> 6) & 3) << 7)
                };
                return r(
                    (f3c << 13) | f | (rs2 << 2) | 2,
                    w,
                    Fm::Store,
                    Cm::Mem(mnsp),
                );
            }
            if cr(rs1) && cr(rs2) && v < 32 * a as u32 {
                return r(cl(f3c, v, rs1, rs2, d), w, Fm::Store, Cm::Mem(mn));
            }
            None
        }
        JALR if f3 == 0 && imm == 0 && rs1 != 0 && rd < 2 => {
            let (h, mn) = if rd == 0 {
                (0x8 << 12, "c.jr")
            } else {
                (0x9 << 12, "c.jalr")
            };
            r(h | (rs1 << 7) | 2, w, Fm::Jalr, Cm::Jr(mn))
        }
        JAL if rd == 0 || (rd == 1 && !rv64) => {
            let off = j_off(w);
            if !(-2048..2048).contains(&off) {
                return None;
            }
            let (f3c, mn) = if rd == 0 { (5, "c.j") } else { (1, "c.jal") };
            r((f3c << 13) | cj_imm(off) | 1, w, Fm::Jal, Cm::J(mn))
        }
        BRANCH if f3 < 2 && ((rs2 == 0 && cr(rs1)) || (rs1 == 0 && cr(rs2))) => {
            let off = b_off(w);
            if !(-256..256).contains(&off) {
                return None;
            }
            let reg = if rs2 == 0 { rs1 } else { rs2 };
            let (f3c, mn) = if f3 == 0 {
                (6, "c.beqz")
            } else {
                (7, "c.bnez")
            };
            let canon = b_imm(off) | (reg << 15) | (f3 << 12) | BRANCH;
            r(
                (f3c << 13) | cb_imm(off) | ((reg - 8) << 7) | 1,
                canon,
                Fm::Branch,
                Cm::B(mn),
            )
        }
        SYSTEM if w == unimp().word => r(0, w, Fm::Unimp, Cm::Unimp),
        _ => None,
    }
}

/// The 16-bit form of word `w` (`C`; rv64: `rv64`), if it has one.
#[must_use]
pub fn compress(w: u32, rv64: bool) -> Option<u16> {
    cform(w, rv64).map(|c| c.0)
}

/// Whether a branch from a word can take a 16-bit form to some target
/// (`c.beqz` / `c.bnez`: `beq` / `bne` with `zero` and `x8`-`x15`), and
/// its reach in bytes `[lo, hi)`; a `jal` (`c.j`, rv32 `c.jal`) likewise.
#[must_use]
pub const fn c_reach(w: u32, rv64: bool) -> Option<(i32, i32)> {
    let (rd, f3, rs1, rs2) = (
        bits(w, 7, 5),
        bits(w, 12, 3),
        bits(w, 15, 5),
        bits(w, 20, 5),
    );
    match w & 0x7f {
        BRANCH if f3 < 2 && ((rs2 == 0 && cr(rs1)) || (rs1 == 0 && cr(rs2))) => Some((-256, 256)),
        JAL if rd == 0 || (rd == 1 && !rv64) => Some((-2048, 2048)),
        _ => None,
    }
}

/// A V word (`riscv::vector` encodes it).
#[must_use]
pub const fn vector(w: u32) -> Ins {
    ins(w, Fm::V)
}

/// The name of integer register `r`.
#[must_use]
pub const fn xreg(r: u32) -> &'static str {
    XREG[r as usize]
}

/// The code of a function in the making.
#[derive(Default)]
pub struct Asm {
    pub code: Vec<u32>,
    /// Each word's form, with text on.
    text: Option<Vec<Fm>>,
    /// Which words go out in 16 bits (`compact`); empty: none.
    small: Vec<bool>,
    rv64: bool,
}

impl Asm {
    /// `text`: record each word's form for `lines`.
    #[must_use]
    pub fn new(text: bool) -> Self {
        Self {
            code: Vec::new(),
            text: text.then(Vec::new),
            small: Vec::new(),
            rv64: false,
        }
    }

    /// Put out the words `small` says in their 16-bit forms (each must
    /// have one), shown as their 32-bit equivalents.
    pub fn compact(&mut self, small: Vec<bool>, rv64: bool) {
        for (n, _) in small.iter().enumerate().filter(|x| *x.1) {
            let (_, w, f, _) = cform(self.code[n], rv64).expect("a 16-bit form");
            self.code[n] = w;
            if let Some(t) = &mut self.text {
                t[n] = f;
            }
        }
        self.small = small;
        self.rv64 = rv64;
    }

    /// Whether word `n` goes out in 16 bits.
    #[must_use]
    pub fn is_small(&self, n: usize) -> bool {
        self.small.get(n).copied().unwrap_or(false)
    }

    /// The code's bytes.
    #[must_use]
    pub fn bytes(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(4 * self.code.len());
        for (n, w) in self.code.iter().enumerate() {
            if self.is_small(n) {
                let h = cform(*w, self.rv64).expect("a 16-bit form").0;
                b.extend_from_slice(&h.to_le_bytes());
            } else {
                b.extend_from_slice(&w.to_le_bytes());
            }
        }
        b
    }

    /// Point the branch or `jal` at `at` `d` bytes on, whatever it
    /// pointed at.
    pub fn repoint(&mut self, at: usize, d: i32) {
        let w = self.code[at];
        self.code[at] = if w & 0x7f == JAL {
            (w & 0xfff) | j_imm(d)
        } else {
            (w & 0x01ff_f07f) | b_imm(d)
        };
    }

    /// Where the branch or `jal` at `at` points, in bytes from it.
    #[must_use]
    pub fn target(&self, at: usize) -> i32 {
        let w = self.code[at];
        if w & 0x7f == JAL { j_off(w) } else { b_off(w) }
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

    /// Point the `jal` at `at` `d` bytes on.
    pub fn patch_jal(&mut self, at: usize, d: i32) {
        self.code[at] |= j_imm(d);
    }

    /// Point the branch at `at` `d` bytes on.
    pub fn patch_branch(&mut self, at: usize, d: i32) {
        self.code[at] |= b_imm(d);
    }

    /// Each recorded word's byte offset and text. `syms`: the local
    /// symbols in the function, `(offset, name)` in order (the ELF
    /// writer's `.Lpcrel_hi` labels). As objdump, a `jalr` through a
    /// register an `auipc` set shows the address it reaches, by the
    /// symbol before it; branches, calls and symbols forget the
    /// registers.
    #[cfg(test)]
    #[must_use]
    pub fn lines(&self, l: &Listing<'_>, syms: &[(u32, String)]) -> Vec<(u32, String)> {
        self.lines_as(l, syms, false)
    }

    /// `lines`, `src`: each 16-bit form as `--emit=asm` writes it (its
    /// `c.` mnemonic).
    #[must_use]
    pub fn lines_as(
        &self,
        l: &Listing<'_>,
        syms: &[(u32, String)],
        src: bool,
    ) -> Vec<(u32, String)> {
        let Some(t) = &self.text else {
            return Vec::new();
        };
        let mut known = [None::<i64>; 32];
        let mut out = Vec::with_capacity(t.len());
        let mut next = 0i64;
        for (n, (f, w)) in t.iter().zip(&self.code).enumerate() {
            let at = next;
            next += if self.is_small(n) { 2 } else { 4 };
            if syms.iter().any(|s| i64::from(s.0) == at) {
                known = [None; 32];
            }
            let mut s = if src && self.is_small(n) {
                c_text(*w, self.rv64, at, l)
            } else {
                text(*w, *f, at, l)
            };
            let rd = bits(*w, 7, 5) as usize;
            match f {
                Fm::Jalr => {
                    if let Some(base) = known[bits(*w, 15, 5) as usize] {
                        let t = base + i64::from((*w as i32) >> 20);
                        let (name, from) = syms
                            .iter()
                            .rev()
                            .find(|s| i64::from(s.0) <= t)
                            .map_or((l.name, 0), |s| (s.1.as_str(), i64::from(s.0)));
                        s = if t == from {
                            format!("{s} <{name}>")
                        } else {
                            format!("{s} <{name}+{:#x}>", t - from)
                        };
                    }
                    known = [None; 32];
                }
                Fm::Branch | Fm::Jal => known = [None; 32],
                Fm::Auipc => known[rd] = Some(at + i64::from((*w & 0xffff_f000) as i32)),
                Fm::Op | Fm::OpImm | Fm::OpImm32 | Fm::Rot | Fm::Lui | Fm::Amo | Fm::V => {
                    known[rd] = None;
                }
                Fm::Load if *w & 0x7f == LOAD => known[rd] = None,
                Fm::Fp if matches!(bits(*w, 27, 5), 0x14 | 0x18 | 0x1c) => known[rd] = None,
                _ => {}
            }
            out.push((at as u32, s));
        }
        out
    }
}

// Text.

const XREG: [&str; 32] = [
    "zero", "ra", "sp", "gp", "tp", "t0", "t1", "t2", "s0", "s1", "a0", "a1", "a2", "a3", "a4",
    "a5", "a6", "a7", "s2", "s3", "s4", "s5", "s6", "s7", "s8", "s9", "s10", "s11", "t3", "t4",
    "t5", "t6",
];

const FREG: [&str; 32] = [
    "ft0", "ft1", "ft2", "ft3", "ft4", "ft5", "ft6", "ft7", "fs0", "fs1", "fa0", "fa1", "fa2",
    "fa3", "fa4", "fa5", "fa6", "fa7", "fs2", "fs3", "fs4", "fs5", "fs6", "fs7", "fs8", "fs9",
    "fs10", "fs11", "ft8", "ft9", "ft10", "ft11",
];

const fn bits(w: u32, lo: u32, n: u32) -> u32 {
    (w >> lo) & ((1 << n) - 1)
}

fn hex(v: i64) -> String {
    if v < 0 {
        format!("-{:#x}", v.unsigned_abs())
    } else {
        format!("{v:#x}")
    }
}

/// The text of word `w` of form `f` at byte offset `at`.
#[allow(clippy::too_many_lines)]
fn text(w: u32, f: Fm, at: i64, l: &Listing<'_>) -> String {
    let (rd, f3, rs1, rs2, f7) = (
        bits(w, 7, 5),
        bits(w, 12, 3),
        bits(w, 15, 5),
        bits(w, 20, 5),
        bits(w, 25, 7),
    );
    let x = |r: u32| XREG[r as usize];
    let fr = |r: u32| FREG[r as usize];
    let imm_i = i64::from((w as i32) >> 20);
    match f {
        Fm::OpImm => match f3 {
            0 if rd == 0 && rs1 == 0 && imm_i == 0 => "nop".to_owned(),
            0 if rs1 == 0 => format!("li\t{}, {}", x(rd), hex(imm_i)),
            0 if imm_i == 0 => format!("mv\t{}, {}", x(rd), x(rs1)),
            3 if imm_i == 1 => format!("seqz\t{}, {}", x(rd), x(rs1)),
            4 if imm_i == -1 => format!("not\t{}, {}", x(rd), x(rs1)),
            7 if imm_i == 255 => format!("zext.b\t{}, {}", x(rd), x(rs1)),
            1 | 5 => {
                let mn = match (f3, (w >> 30) & 1) {
                    (1, _) => "slli",
                    (_, 0) => "srli",
                    _ => "srai",
                };
                format!(
                    "{mn}\t{}, {}, {}",
                    x(rd),
                    x(rs1),
                    hex(i64::from(bits(w, 20, 6)))
                )
            }
            _ => {
                let mn = ["addi", "", "slti", "sltiu", "xori", "", "ori", "andi"][f3 as usize];
                format!("{mn}\t{}, {}, {}", x(rd), x(rs1), hex(imm_i))
            }
        },
        Fm::Rot => {
            let w32 = matches!(w & 0x7f, OP_32 | OP_IMM_32);
            let sfx = if w32 { "w" } else { "" };
            if matches!(w & 0x7f, OP_IMM | OP_IMM_32) {
                let sh = hex(i64::from(bits(w, 20, 6)));
                format!("rori{sfx}\t{}, {}, {sh}", x(rd), x(rs1))
            } else {
                let mn = if f3 == 1 { "rol" } else { "ror" };
                format!("{mn}{sfx}\t{}, {}, {}", x(rd), x(rs1), x(rs2))
            }
        }
        Fm::OpImm32 if imm_i == 0 => format!("sext.w\t{}, {}", x(rd), x(rs1)),
        Fm::OpImm32 => format!("addiw\t{}, {}, {}", x(rd), x(rs1), hex(imm_i)),
        Fm::Lui | Fm::Auipc => {
            let mn = if f == Fm::Lui { "lui" } else { "auipc" };
            format!("{mn}\t{}, {:#x}", x(rd), w >> 12)
        }
        Fm::Op => {
            let mn = match (f7, f3) {
                (0x20, 0) if rs1 == 0 => return format!("neg\t{}, {}", x(rd), x(rs2)),
                (0, 3) if rs1 == 0 => return format!("snez\t{}, {}", x(rd), x(rs2)),
                (0, 2) if rs2 == 0 => return format!("sltz\t{}, {}", x(rd), x(rs1)),
                (0, 2) if rs1 == 0 => return format!("sgtz\t{}, {}", x(rd), x(rs2)),
                (0x20, 0) => "sub",
                (0x20, _) => "sra",
                (1, _) => [
                    "mul", "mulh", "mulhsu", "mulhu", "div", "divu", "rem", "remu",
                ][f3 as usize],
                _ => ["add", "sll", "slt", "sltu", "xor", "srl", "or", "and"][f3 as usize],
            };
            format!("{mn}\t{}, {}, {}", x(rd), x(rs1), x(rs2))
        }
        Fm::Load => {
            let fpl = w & 0x7f == LOAD_FP;
            let (mn, rt) = if fpl {
                (if f3 == 3 { "fld" } else { "flw" }, fr(rd))
            } else {
                (
                    ["lb", "lh", "lw", "ld", "lbu", "lhu", "lwu", ""][f3 as usize],
                    x(rd),
                )
            };
            format!("{mn}\t{rt}, {}({})", hex(imm_i), x(rs1))
        }
        Fm::Store => {
            let off = i64::from((((w as i32) >> 25) << 5) | bits(w, 7, 5) as i32);
            let (mn, rt) = if w & 0x7f == STORE_FP {
                (if f3 == 3 { "fsd" } else { "fsw" }, fr(rs2))
            } else {
                (["sb", "sh", "sw", "sd"][f3 as usize & 3], x(rs2))
            };
            format!("{mn}\t{rt}, {}({})", hex(off), x(rs1))
        }
        Fm::Branch => {
            let off = i64::from(
                (((w as i32) >> 31) << 12)
                    | ((bits(w, 7, 1) << 11) | (bits(w, 25, 6) << 5) | (bits(w, 8, 4) << 1)) as i32,
            );
            let t = l.target(at + off);
            match (f3, rs1, rs2) {
                (0, _, 0) => format!("beqz\t{}, {t}", x(rs1)),
                (1, _, 0) => format!("bnez\t{}, {t}", x(rs1)),
                (4, _, 0) => format!("bltz\t{}, {t}", x(rs1)),
                (5, _, 0) => format!("bgez\t{}, {t}", x(rs1)),
                (4, 0, _) => format!("bgtz\t{}, {t}", x(rs2)),
                (5, 0, _) => format!("blez\t{}, {t}", x(rs2)),
                _ => {
                    let mn = ["beq", "bne", "", "", "blt", "bge", "bltu", "bgeu"][f3 as usize];
                    format!("{mn}\t{}, {}, {t}", x(rs1), x(rs2))
                }
            }
        }
        Fm::Jal => {
            let off = i64::from(
                (((w as i32) >> 31) << 20)
                    | ((bits(w, 12, 8) << 12) | (bits(w, 20, 1) << 11) | (bits(w, 21, 10) << 1))
                        as i32,
            );
            let t = l.target(at + off);
            match rd {
                0 => format!("j\t{t}"),
                1 => format!("jal\t{t}"),
                _ => format!("jal\t{}, {t}", x(rd)),
            }
        }
        Fm::Jalr => match (rd, rs1, imm_i) {
            (0, 1, 0) => "ret".to_owned(),
            (0, _, 0) => format!("jr\t{}", x(rs1)),
            (1, _, 0) => format!("jalr\t{}", x(rs1)),
            _ => format!("jalr\t{}, {}({})", x(rd), hex(imm_i), x(rs1)),
        },
        Fm::Amo => {
            let sz = if f3 == 3 { "d" } else { "w" };
            let ord = ["", ".rl", ".aq", ".aqrl"][(f7 & 3) as usize];
            let mn = match f7 >> 2 {
                0x00 => "amoadd",
                0x01 => "amoswap",
                0x02 => return format!("lr.{sz}{ord}\t{}, ({})", x(rd), x(rs1)),
                0x03 => "sc",
                0x04 => "amoxor",
                0x08 => "amoor",
                _ => "amoand",
            };
            format!("{mn}.{sz}{ord}\t{}, {}, ({})", x(rd), x(rs2), x(rs1))
        }
        Fm::Fence => {
            let set = |b: u32| match b {
                1 => "w",
                2 => "r",
                _ => "rw",
            };
            format!("fence\t{}, {}", set(bits(w, 24, 4)), set(bits(w, 20, 4)))
        }
        Fm::Unimp => "unimp".to_owned(),
        Fm::V => crate::isa::riscv::vector::text(w),
        Fm::Fp => fp_text(w),
        Fm::Fma => {
            let fr = |r: u32| FREG[r as usize];
            let s = if bits(w, 25, 1) == 1 { "d" } else { "s" };
            let rm = if bits(w, 12, 3) == 7 { "" } else { ", rne" };
            let (rd, rs1, rs2, rs3) = (
                bits(w, 7, 5),
                bits(w, 15, 5),
                bits(w, 20, 5),
                bits(w, 27, 5),
            );
            format!(
                "fmadd.{s}\t{}, {}, {}, {}{rm}",
                fr(rd),
                fr(rs1),
                fr(rs2),
                fr(rs3)
            )
        }
    }
}

/// The `c.` text of the 16-bit form of word `w` at byte offset `at`.
fn c_text(w: u32, rv64: bool, at: i64, l: &Listing<'_>) -> String {
    let (_, w, _, m) = cform(w, rv64).expect("a 16-bit form");
    let (rd, rs1, rs2) = (bits(w, 7, 5), bits(w, 15, 5), bits(w, 20, 5));
    let x = |r: u32| XREG[r as usize];
    let imm = i64::from((w as i32) >> 20);
    match m {
        Cm::Nop => "c.nop".to_owned(),
        Cm::Unimp => "c.unimp".to_owned(),
        Cm::Imm(mn) => {
            // A shift's amount: the low 6 bits (`srai`'s carry `0x400`).
            let v = if matches!(mn, "c.slli" | "c.srli" | "c.srai") {
                imm & 0x3f
            } else {
                imm
            };
            format!("{mn}\t{}, {}", x(rd), hex(v))
        }
        Cm::Lui => format!("c.lui\t{}, {:#x}", x(rd), w >> 12),
        Cm::Addi16sp => format!("c.addi16sp\tsp, {}", hex(imm)),
        Cm::Addi4spn => format!("c.addi4spn\t{}, sp, {}", x(rd), hex(imm)),
        Cm::Reg(mn) => {
            // The 32-bit equivalent: `addi rd, rs, 0` or `op rd, rd, rs2`.
            let src = if mn == "c.mv" { rs1 } else { rs2 };
            format!("{mn}\t{}, {}", x(rd), x(src))
        }
        Cm::Mem(mn) => {
            let store = matches!(w & 0x7f, STORE | STORE_FP);
            let (rt, off) = if store {
                (
                    rs2,
                    i64::from((((w as i32) >> 25) << 5) | bits(w, 7, 5) as i32),
                )
            } else {
                (rd, imm)
            };
            let rt = if matches!(w & 0x7f, LOAD_FP | STORE_FP) {
                FREG[rt as usize]
            } else {
                x(rt)
            };
            format!("{mn}\t{rt}, {}({})", hex(off), x(rs1))
        }
        Cm::Jr(mn) => format!("{mn}\t{}", x(rs1)),
        Cm::J(mn) => format!("{mn}\t{}", l.target(at + i64::from(j_off(w)))),
        Cm::B(mn) => format!("{mn}\t{}, {}", x(rs1), l.target(at + i64::from(b_off(w)))),
    }
}

/// The text of an `OP-FP` word.
fn fp_text(w: u32) -> String {
    let (rd, rm, rs1, rs2) = (
        bits(w, 7, 5),
        bits(w, 12, 3),
        bits(w, 15, 5),
        bits(w, 20, 5),
    );
    let (f5, d) = (bits(w, 27, 5), bits(w, 25, 1) == 1);
    let x = |r: u32| XREG[r as usize];
    let fr = |r: u32| FREG[r as usize];
    let s = if d { "d" } else { "s" };
    let rms = |dflt: u32| match rm {
        r if r == dflt => String::new(),
        7 => ", dyn".to_owned(),
        0 => ", rne".to_owned(),
        1 => ", rtz".to_owned(),
        2 => ", rdn".to_owned(),
        3 => ", rup".to_owned(),
        _ => ", rmm".to_owned(),
    };
    let it = |t: u32| ["w", "wu", "l", "lu"][t as usize & 3];
    match f5 {
        0..=3 => {
            let mn = ["fadd", "fsub", "fmul", "fdiv"][f5 as usize];
            format!("{mn}.{s}\t{}, {}, {}{}", fr(rd), fr(rs1), fr(rs2), rms(7))
        }
        0x0b => format!("fsqrt.{s}\t{}, {}{}", fr(rd), fr(rs1), rms(7)),
        0x04 if rs1 == rs2 => {
            let mn = ["fmv", "fneg", "fabs"][rm as usize % 3];
            format!("{mn}.{s}\t{}, {}", fr(rd), fr(rs1))
        }
        0x04 => {
            let mn = ["fsgnj", "fsgnjn", "fsgnjx"][rm as usize % 3];
            format!("{mn}.{s}\t{}, {}, {}", fr(rd), fr(rs1), fr(rs2))
        }
        0x14 => {
            let mn = ["fle", "flt", "feq"][rm as usize % 3];
            format!("{mn}.{s}\t{}, {}, {}", x(rd), fr(rs1), fr(rs2))
        }
        0x18 => format!("fcvt.{}.{s}\t{}, {}{}", it(rs2), x(rd), fr(rs1), rms(7)),
        0x1a => format!("fcvt.{s}.{}\t{}, {}{}", it(rs2), fr(rd), x(rs1), rms(7)),
        // Widening: exact, `rne` its default.
        0x08 if d => format!("fcvt.d.s\t{}, {}{}", fr(rd), fr(rs1), rms(0)),
        0x08 => format!("fcvt.s.d\t{}, {}{}", fr(rd), fr(rs1), rms(7)),
        0x1c => format!(
            "fmv.x.{}\t{}, {}",
            if d { "d" } else { "w" },
            x(rd),
            fr(rs1)
        ),
        _ => format!(
            "fmv.{}.x\t{}, {}",
            if d { "d" } else { "w" },
            fr(rd),
            x(rs1)
        ),
    }
}
