//! i386 instruction encoder: one function per instruction form the
//! emitter uses, the only place that knows opcodes, prefixes, `ModRM` and
//! SIB. With `Asm::text` on (`--emit=asm`) each function also records
//! its instruction; `lines` prints them in Intel syntax as `llvm-objdump
//! -d -M intel` does. Off, nothing is recorded.
//!
//! Each form has one encoding: `alu_ri` always takes an
//! imm32 and `alu_ri8` an imm8 (the emitter picks), `alu` is the `r/m, r`
//! form (`01`, `09`, ..) and `alu_ld` the `r, r/m` one (`03`, `0B`, ..),
//! `mov` reg-reg is `8B` and `mov_st` `89`, `mov_ri` is always `B8+r`
//! (never `xor`, so the flags survive), `dec` of a register the one-byte
//! `48+r`, a jump or call rel32 unless named `8`, and an absolute or
//! segment operand `ModRM` `[disp32]` (not the `A1` short forms).

#[cfg(test)]
mod tests;

use crate::asm::Listing;
use crate::lir::SymId;
use crate::obj::RelKind;

/// A general-purpose register, `eax` = 0 .. `edi` = 7.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Gpr(pub u32);

/// An `xmm` register, 0 .. 7.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Xmm(pub u32);

pub const EAX: Gpr = Gpr(0);
pub const ECX: Gpr = Gpr(1);
pub const ESP: Gpr = Gpr(4);
pub const EBP: Gpr = Gpr(5);

/// Operand size.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Sz {
    B,
    W,
    D,
}

impl Sz {
    const fn ptr(self) -> Ptr {
        match self {
            Self::B => Ptr::B,
            Self::W => Ptr::W,
            Self::D => Ptr::D,
        }
    }

    /// The operand-size prefix of a 16-bit form.
    fn pre(self) -> Option<u8> {
        (self == Self::W).then_some(0x66)
    }
}

/// A condition code (the low nibble of `jcc` / `setcc` / `cmovcc`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Cond(u8);

impl Cond {
    pub const O: Self = Self(0);
    pub const B: Self = Self(2);
    pub const AE: Self = Self(3);
    pub const E: Self = Self(4);
    pub const NE: Self = Self(5);
    pub const BE: Self = Self(6);
    pub const A: Self = Self(7);
    pub const S: Self = Self(8);
    pub const NS: Self = Self(9);
    pub const P: Self = Self(0xa);
    pub const NP: Self = Self(0xb);
    pub const L: Self = Self(0xc);
    pub const GE: Self = Self(0xd);
    pub const LE: Self = Self(0xe);
    pub const G: Self = Self(0xf);

    /// The opposite condition.
    #[must_use]
    pub const fn inv(self) -> Self {
        Self(self.0 ^ 1)
    }
}

/// A segment override.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Seg {
    Fs,
    Gs,
}

/// A memory operand.
#[derive(Clone, Copy, Debug)]
pub enum Addr {
    /// `[base + disp]`.
    Bd(Gpr, i32),
    /// `[base + index * 2^sc]`.
    Bi(Gpr, Gpr, u8),
    /// `[base + sym]`: a zero disp32 relocated by the kind.
    BdRel(Gpr, RelKind, SymId),
    /// `[sym]`: a zero disp32 relocated by the kind.
    Abs(RelKind, SymId),
    /// `seg:[disp32]`.
    Seg(Seg, i32),
}

impl Addr {
    /// `[base + disp + n]` of a `Bd`.
    #[must_use]
    pub fn plus(self, n: i32) -> Self {
        match self {
            Self::Bd(b, d) => Self::Bd(b, d + n),
            _ => unreachable!("an offset of a [base + disp] operand only"),
        }
    }
}

/// A register-or-memory operand.
#[derive(Clone, Copy, Debug)]
pub enum Rm {
    R(Gpr),
    M(Addr),
}

/// An `xmm`-or-memory operand.
#[derive(Clone, Copy, Debug)]
pub enum Xrm {
    X(Xmm),
    M(Addr),
}

/// The `ModRM.rm` side of an encoding: a register number or memory.
#[derive(Clone, Copy)]
enum Ea {
    R(u32),
    M(Addr),
}

impl From<Rm> for Ea {
    fn from(r: Rm) -> Self {
        match r {
            Rm::R(g) => Self::R(g.0),
            Rm::M(a) => Self::M(a),
        }
    }
}

impl From<Xrm> for Ea {
    fn from(r: Xrm) -> Self {
        match r {
            Xrm::X(x) => Self::R(x.0),
            Xrm::M(a) => Self::M(a),
        }
    }
}

/// Group 1 (`80`..`83`, and the `00`..`3D` register forms).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Alu {
    Add = 0,
    Or = 1,
    Adc = 2,
    Sbb = 3,
    And = 4,
    Sub = 5,
    Xor = 6,
    Cmp = 7,
}

impl Alu {
    const fn name(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Or => "or",
            Self::Adc => "adc",
            Self::Sbb => "sbb",
            Self::And => "and",
            Self::Sub => "sub",
            Self::Xor => "xor",
            Self::Cmp => "cmp",
        }
    }
}

/// Group 2 (`C1`, `D1`, `D3`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Shift {
    Rol = 0,
    Rcl = 2,
    Shl = 4,
    Shr = 5,
    Sar = 7,
}

impl Shift {
    const fn name(self) -> &'static str {
        match self {
            Self::Rol => "rol",
            Self::Rcl => "rcl",
            Self::Shl => "shl",
            Self::Shr => "shr",
            Self::Sar => "sar",
        }
    }
}

/// Group 3 (`F7`) but `test`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Un {
    Not = 2,
    Neg = 3,
    Mul = 4,
    /// One-operand `imul`: `edx:eax = eax * src`, signed.
    Imul = 5,
    Div = 6,
    Idiv = 7,
}

impl Un {
    const fn name(self) -> &'static str {
        match self {
            Self::Not => "not",
            Self::Neg => "neg",
            Self::Mul => "mul",
            Self::Imul => "imul",
            Self::Div => "div",
            Self::Idiv => "idiv",
        }
    }
}

/// An x87 memory form: `(opcode, /n, name, operand size)`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum X87 {
    /// `fld` / `fstp` a `float` / `double`.
    Fld32,
    Fld64,
    Fstp32,
    Fstp64,
    /// `fild` / `fistp` a 64-bit integer.
    Fild64,
    Fistp64,
    /// `fadd` a `float`.
    Fadd32,
    /// The control word.
    Fnstcw,
    Fldcw,
}

impl X87 {
    const fn enc(self) -> (u8, u32, &'static str, Ptr) {
        match self {
            Self::Fld32 => (0xd9, 0, "fld", Ptr::D),
            Self::Fld64 => (0xdd, 0, "fld", Ptr::Q),
            Self::Fstp32 => (0xd9, 3, "fstp", Ptr::D),
            Self::Fstp64 => (0xdd, 3, "fstp", Ptr::Q),
            Self::Fild64 => (0xdf, 5, "fild", Ptr::Q),
            Self::Fistp64 => (0xdf, 7, "fistp", Ptr::Q),
            Self::Fadd32 => (0xd8, 0, "fadd", Ptr::D),
            Self::Fnstcw => (0xd9, 7, "fnstcw", Ptr::W),
            Self::Fldcw => (0xd9, 5, "fldcw", Ptr::W),
        }
    }
}

/// An SSE / SSE2 instruction: `(prefix, opcode after 0F, name, memory
/// operand size)`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Sse {
    Movss,
    Movsd,
    MovssSt,
    MovsdSt,
    Movaps,
    Movdqa,
    Movdqu,
    MovdquSt,
    /// `movq xmm, m64` / `movd xmm, m32`.
    MovqLd,
    MovdLd,
    /// `movq m64, xmm` / `movd m32, xmm`.
    MovqSt,
    MovdSt,
    Addss,
    Addsd,
    Subss,
    Subsd,
    Mulss,
    Mulsd,
    Divss,
    Divsd,
    Sqrtss,
    Sqrtsd,
    Cvtss2sd,
    Cvtsd2ss,
    Ucomiss,
    Ucomisd,
    Xorps,
    Addps,
    Addpd,
    Subps,
    Subpd,
    Mulps,
    Mulpd,
    Divps,
    Divpd,
    Sqrtps,
    Sqrtpd,
    Punpcklbw,
    Punpcklwd,
    Punpckldq,
    Punpcklqdq,
    Packsswb,
    Packuswb,
    Packssdw,
    Pshufd,
    Paddb,
    Paddw,
    Paddd,
    Paddq,
    Psubb,
    Psubw,
    Psubd,
    Psubq,
    Pmullw,
    Pand,
    Pandn,
    Por,
    Pxor,
    Pcmpeqb,
    Pcmpeqw,
    Pcmpeqd,
    Pcmpgtb,
    Pcmpgtw,
    Pcmpgtd,
    Pminub,
    Pmaxub,
    Pminsw,
    Pmaxsw,
    Minps,
    Minpd,
    Maxps,
    Maxpd,
    Pmulhw,
    Pmulhuw,
}

impl Sse {
    const fn enc(self) -> (Option<u8>, u8, &'static str, Ptr) {
        const F3: Option<u8> = Some(0xf3);
        const F2: Option<u8> = Some(0xf2);
        const P66: Option<u8> = Some(0x66);
        const NP: Option<u8> = None;
        match self {
            Self::Movss => (F3, 0x10, "movss", Ptr::D),
            Self::Movsd => (F2, 0x10, "movsd", Ptr::Q),
            Self::MovssSt => (F3, 0x11, "movss", Ptr::D),
            Self::MovsdSt => (F2, 0x11, "movsd", Ptr::Q),
            Self::Movaps => (NP, 0x28, "movaps", Ptr::X),
            Self::Movdqa => (P66, 0x6f, "movdqa", Ptr::X),
            Self::Movdqu => (F3, 0x6f, "movdqu", Ptr::X),
            Self::MovdquSt => (F3, 0x7f, "movdqu", Ptr::X),
            Self::MovqLd => (F3, 0x7e, "movq", Ptr::Q),
            Self::MovdLd => (P66, 0x6e, "movd", Ptr::D),
            Self::MovqSt => (P66, 0xd6, "movq", Ptr::Q),
            Self::MovdSt => (P66, 0x7e, "movd", Ptr::D),
            Self::Addss => (F3, 0x58, "addss", Ptr::D),
            Self::Addsd => (F2, 0x58, "addsd", Ptr::Q),
            Self::Subss => (F3, 0x5c, "subss", Ptr::D),
            Self::Subsd => (F2, 0x5c, "subsd", Ptr::Q),
            Self::Mulss => (F3, 0x59, "mulss", Ptr::D),
            Self::Mulsd => (F2, 0x59, "mulsd", Ptr::Q),
            Self::Divss => (F3, 0x5e, "divss", Ptr::D),
            Self::Divsd => (F2, 0x5e, "divsd", Ptr::Q),
            Self::Sqrtss => (F3, 0x51, "sqrtss", Ptr::D),
            Self::Sqrtsd => (F2, 0x51, "sqrtsd", Ptr::Q),
            Self::Cvtss2sd => (F3, 0x5a, "cvtss2sd", Ptr::D),
            Self::Cvtsd2ss => (F2, 0x5a, "cvtsd2ss", Ptr::Q),
            Self::Ucomiss => (NP, 0x2e, "ucomiss", Ptr::D),
            Self::Ucomisd => (P66, 0x2e, "ucomisd", Ptr::Q),
            Self::Xorps => (NP, 0x57, "xorps", Ptr::X),
            Self::Addps => (NP, 0x58, "addps", Ptr::X),
            Self::Addpd => (P66, 0x58, "addpd", Ptr::X),
            Self::Subps => (NP, 0x5c, "subps", Ptr::X),
            Self::Subpd => (P66, 0x5c, "subpd", Ptr::X),
            Self::Mulps => (NP, 0x59, "mulps", Ptr::X),
            Self::Mulpd => (P66, 0x59, "mulpd", Ptr::X),
            Self::Divps => (NP, 0x5e, "divps", Ptr::X),
            Self::Divpd => (P66, 0x5e, "divpd", Ptr::X),
            Self::Sqrtps => (NP, 0x51, "sqrtps", Ptr::X),
            Self::Sqrtpd => (P66, 0x51, "sqrtpd", Ptr::X),
            Self::Punpcklbw => (P66, 0x60, "punpcklbw", Ptr::X),
            Self::Punpcklwd => (P66, 0x61, "punpcklwd", Ptr::X),
            Self::Punpckldq => (P66, 0x62, "punpckldq", Ptr::X),
            Self::Punpcklqdq => (P66, 0x6c, "punpcklqdq", Ptr::X),
            Self::Packsswb => (P66, 0x63, "packsswb", Ptr::X),
            Self::Packuswb => (P66, 0x67, "packuswb", Ptr::X),
            Self::Packssdw => (P66, 0x6b, "packssdw", Ptr::X),
            Self::Pshufd => (P66, 0x70, "pshufd", Ptr::X),
            Self::Paddb => (P66, 0xfc, "paddb", Ptr::X),
            Self::Paddw => (P66, 0xfd, "paddw", Ptr::X),
            Self::Paddd => (P66, 0xfe, "paddd", Ptr::X),
            Self::Paddq => (P66, 0xd4, "paddq", Ptr::X),
            Self::Psubb => (P66, 0xf8, "psubb", Ptr::X),
            Self::Psubw => (P66, 0xf9, "psubw", Ptr::X),
            Self::Psubd => (P66, 0xfa, "psubd", Ptr::X),
            Self::Psubq => (P66, 0xfb, "psubq", Ptr::X),
            Self::Pmullw => (P66, 0xd5, "pmullw", Ptr::X),
            Self::Pand => (P66, 0xdb, "pand", Ptr::X),
            Self::Pandn => (P66, 0xdf, "pandn", Ptr::X),
            Self::Por => (P66, 0xeb, "por", Ptr::X),
            Self::Pxor => (P66, 0xef, "pxor", Ptr::X),
            Self::Pcmpeqb => (P66, 0x74, "pcmpeqb", Ptr::X),
            Self::Pcmpeqw => (P66, 0x75, "pcmpeqw", Ptr::X),
            Self::Pcmpeqd => (P66, 0x76, "pcmpeqd", Ptr::X),
            Self::Pcmpgtb => (P66, 0x64, "pcmpgtb", Ptr::X),
            Self::Pcmpgtw => (P66, 0x65, "pcmpgtw", Ptr::X),
            Self::Pcmpgtd => (P66, 0x66, "pcmpgtd", Ptr::X),
            Self::Pminub => (P66, 0xda, "pminub", Ptr::X),
            Self::Pmaxub => (P66, 0xde, "pmaxub", Ptr::X),
            Self::Pminsw => (P66, 0xea, "pminsw", Ptr::X),
            Self::Pmaxsw => (P66, 0xee, "pmaxsw", Ptr::X),
            Self::Minps => (NP, 0x5d, "minps", Ptr::X),
            Self::Minpd => (P66, 0x5d, "minpd", Ptr::X),
            Self::Maxps => (NP, 0x5f, "maxps", Ptr::X),
            Self::Maxpd => (P66, 0x5f, "maxpd", Ptr::X),
            Self::Pmulhw => (P66, 0xe5, "pmulhw", Ptr::X),
            Self::Pmulhuw => (P66, 0xe4, "pmulhuw", Ptr::X),
        }
    }
}

/// SSE2 shifts by an immediate (`66 0F 71` / `72` / `73` `/n ib`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PShift {
    Psrlw,
    Psraw,
    Psllw,
    Psrld,
    Psrad,
    Pslld,
    Psrlq,
    Psllq,
}

impl PShift {
    /// `(opcode, /n, name)`.
    const fn enc(self) -> (u8, u32, &'static str) {
        match self {
            Self::Psrlw => (0x71, 2, "psrlw"),
            Self::Psraw => (0x71, 4, "psraw"),
            Self::Psllw => (0x71, 6, "psllw"),
            Self::Psrld => (0x72, 2, "psrld"),
            Self::Psrad => (0x72, 4, "psrad"),
            Self::Pslld => (0x72, 6, "pslld"),
            Self::Psrlq => (0x73, 2, "psrlq"),
            Self::Psllq => (0x73, 6, "psllq"),
        }
    }
}

/// `cmpps` / `cmppd` predicates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FCmp {
    Eq = 0,
    Lt = 1,
    Le = 2,
    Neq = 4,
}

impl FCmp {
    /// The mnemonic (`pd`: double).
    const fn name(self, pd: bool) -> &'static str {
        match (self, pd) {
            (Self::Eq, false) => "cmpeqps",
            (Self::Eq, true) => "cmpeqpd",
            (Self::Lt, false) => "cmpltps",
            (Self::Lt, true) => "cmpltpd",
            (Self::Le, false) => "cmpleps",
            (Self::Le, true) => "cmplepd",
            (Self::Neq, false) => "cmpneqps",
            (Self::Neq, true) => "cmpneqpd",
        }
    }
}

/// A sign-bit mask of an `xmm` into a register.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Movmsk {
    Pmovmskb,
    Movmskps,
    Movmskpd,
}

/// A memory operand's size, as Intel syntax spells it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Ptr {
    /// No size (`lea`).
    None,
    B,
    W,
    D,
    Q,
    X,
}

/// A recorded operand.
#[derive(Clone, Copy, Debug)]
enum Op {
    No,
    G(Gpr, Sz),
    X(Xmm),
    M(Addr, Ptr),
    I(i64),
    /// The rel32 / rel8 ending the instruction.
    Rel,
    Rel8,
}

/// A recorded instruction.
#[derive(Clone, Copy, Debug)]
struct Line {
    at: u32,
    mn: &'static str,
    ops: [Op; 3],
}

/// The code of a function in the making.
#[derive(Default)]
pub struct Asm {
    pub code: Vec<u8>,
    /// `(offset, kind, symbol)` of each relocated field.
    pub relocs: Vec<(u32, RelKind, SymId)>,
    text: Option<Vec<Line>>,
}

impl Asm {
    /// `text`: record each instruction for `lines`.
    #[must_use]
    pub fn new(text: bool) -> Self {
        Self {
            text: text.then(Vec::new),
            ..Self::default()
        }
    }

    /// The offset of the next instruction.
    #[must_use]
    pub fn pos(&self) -> usize {
        self.code.len()
    }

    #[inline]
    fn rec(&mut self, at: usize, mn: &'static str, ops: [Op; 3]) {
        if let Some(t) = &mut self.text {
            t.push(Line {
                at: at as u32,
                mn,
                ops,
            });
        }
    }

    fn b(&mut self, x: u8) {
        self.code.push(x);
    }

    fn d32(&mut self, x: u32) {
        self.code.extend_from_slice(&x.to_le_bytes());
    }

    /// Point the rel32 at `at` to offset `target`.
    pub fn patch_rel32(&mut self, at: usize, target: usize) {
        let d = target as i64 - (at as i64 + 4);
        self.code[at..at + 4].copy_from_slice(&(d as i32).to_le_bytes());
    }

    /// Point the rel8 at `at` to offset `target` (within reach).
    pub fn patch_rel8(&mut self, at: usize, target: usize) {
        let d = target as i64 - (at as i64 + 1);
        debug_assert!((-128..128).contains(&d), "a short jump out of reach");
        self.code[at] = d as u8;
    }

    /// A legacy encoding: prefix, segment override, opcode, `ModRM` of
    /// `reg` and `rm`.
    fn legacy(&mut self, pre: Option<u8>, opc: &[u8], reg: u32, rm: Ea) {
        if let Some(p) = pre {
            self.b(p);
        }
        if let Ea::M(Addr::Seg(seg, _)) = rm {
            self.b(if seg == Seg::Fs { 0x64 } else { 0x65 });
        }
        self.code.extend_from_slice(opc);
        self.modrm(reg, rm);
    }

    /// `ModRM` (SIB, displacement, relocation) of `reg` and `rm`.
    fn modrm(&mut self, reg: u32, rm: Ea) {
        let r = ((reg & 7) << 3) as u8;
        match rm {
            Ea::R(x) => self.b(0xc0 | r | (x & 7) as u8),
            Ea::M(Addr::Bd(Gpr(base), disp)) => {
                let lo = (base & 7) as u8;
                let (md, small) = if disp == 0 && lo != 5 {
                    (0u8, None)
                } else if (-128..128).contains(&disp) {
                    (0x40, Some(true))
                } else {
                    (0x80, Some(false))
                };
                self.b(md | r | lo);
                if lo == 4 {
                    self.b(0x24); // SIB: base only
                }
                match small {
                    Some(true) => self.b(disp as u8),
                    Some(false) => self.d32(disp as u32),
                    None => {}
                }
            }
            Ea::M(Addr::Bi(Gpr(base), Gpr(index), sc)) => {
                // `ebp` as a base needs a displacement.
                let lo = (base & 7) as u8;
                let md = if lo == 5 { 0x40 } else { 0 };
                self.b(md | r | 4);
                self.b((sc << 6) | (((index & 7) << 3) as u8) | lo);
                if lo == 5 {
                    self.b(0);
                }
            }
            Ea::M(Addr::BdRel(Gpr(base), kind, sym)) => {
                let lo = (base & 7) as u8;
                self.b(0x80 | r | lo);
                if lo == 4 {
                    self.b(0x24);
                }
                self.reloc32(kind, sym, 0);
            }
            Ea::M(Addr::Abs(kind, sym)) => {
                self.b(r | 5);
                self.reloc32(kind, sym, 0);
            }
            Ea::M(Addr::Seg(_, disp)) => {
                self.b(r | 5);
                self.d32(disp as u32);
            }
        }
    }

    /// A 32-bit field relocated by `kind` against `sym`, holding `addend`
    /// (ELF i386 relocations keep it in the place).
    fn reloc32(&mut self, kind: RelKind, sym: SymId, addend: i32) {
        self.relocs.push((self.code.len() as u32, kind, sym));
        self.d32(addend as u32);
    }

    // Moves.

    /// `mov dst, src` (`8B` / `8A`).
    pub fn mov(&mut self, sz: Sz, dst: Gpr, src: Rm) {
        let at = self.pos();
        let opc = if sz == Sz::B { 0x8a } else { 0x8b };
        self.legacy(sz.pre(), &[opc], dst.0, src.into());
        self.rec(at, "mov", [Op::G(dst, sz), rm_op(src, sz), Op::No]);
    }

    /// `mov dst, src` (`89` / `88`).
    pub fn mov_st(&mut self, sz: Sz, dst: Rm, src: Gpr) {
        let at = self.pos();
        let opc = if sz == Sz::B { 0x88 } else { 0x89 };
        self.legacy(sz.pre(), &[opc], src.0, dst.into());
        self.rec(at, "mov", [rm_op(dst, sz), Op::G(src, sz), Op::No]);
    }

    /// `mov r32, imm32` (`B8+r`).
    pub fn mov_ri(&mut self, dst: Gpr, imm: u32) {
        let at = self.pos();
        self.b(0xb8 + dst.0 as u8);
        self.d32(imm);
        self.rec(
            at,
            "mov",
            [Op::G(dst, Sz::D), Op::I(i64::from(imm)), Op::No],
        );
    }

    /// `mov r32, sym` (`B8+r`, the imm32 relocated by `kind`).
    pub fn mov_ri_sym(&mut self, dst: Gpr, kind: RelKind, sym: SymId) {
        let at = self.pos();
        self.b(0xb8 + dst.0 as u8);
        self.reloc32(kind, sym, 0);
        self.rec(at, "mov", [Op::G(dst, Sz::D), Op::I(0), Op::No]);
    }

    /// `mov dword dst, imm32` (`C7 /0`).
    pub fn mov_mi(&mut self, dst: Rm, imm: u32) {
        let at = self.pos();
        self.legacy(None, &[0xc7], 0, dst.into());
        self.d32(imm);
        self.rec(
            at,
            "mov",
            [rm_op(dst, Sz::D), Op::I(i64::from(imm)), Op::No],
        );
    }

    /// `movzx r32, src` (`from`: `B` / `W`).
    pub fn movzx(&mut self, dst: Gpr, from: Sz, src: Rm) {
        let at = self.pos();
        let opc = if from == Sz::B { 0xb6 } else { 0xb7 };
        self.legacy(None, &[0x0f, opc], dst.0, src.into());
        self.rec(at, "movzx", [Op::G(dst, Sz::D), rm_op(src, from), Op::No]);
    }

    /// `movsx r32, src` (`from`: `B` / `W`).
    pub fn movsx(&mut self, dst: Gpr, from: Sz, src: Rm) {
        let at = self.pos();
        let opc = if from == Sz::B { 0xbe } else { 0xbf };
        self.legacy(None, &[0x0f, opc], dst.0, src.into());
        self.rec(at, "movsx", [Op::G(dst, Sz::D), rm_op(src, from), Op::No]);
    }

    /// `lea r32, m`.
    pub fn lea(&mut self, dst: Gpr, m: Addr) {
        let at = self.pos();
        self.legacy(None, &[0x8d], dst.0, Ea::M(m));
        self.rec(at, "lea", [Op::G(dst, Sz::D), Op::M(m, Ptr::None), Op::No]);
    }

    /// `xchg dst, src` (`87` / `86`; objdump shows a register `dst`
    /// second).
    pub fn xchg(&mut self, sz: Sz, dst: Rm, src: Gpr) {
        let at = self.pos();
        self.rmw(&[0x87], &[0x86], "xchg", sz, dst, src);
        if let (Rm::R(d), Some(t)) = (dst, &mut self.text) {
            t.pop();
            t.push(Line {
                at: at as u32,
                mn: "xchg",
                ops: [Op::G(src, sz), Op::G(d, sz), Op::No],
            });
        }
    }

    /// `xchg eax, r` (`90+r`).
    pub fn xchg_eax(&mut self, r: Gpr) {
        let at = self.pos();
        self.b(0x90 + r.0 as u8);
        self.rec(at, "xchg", [Op::G(EAX, Sz::D), Op::G(r, Sz::D), Op::No]);
    }

    /// `cmovcc dst, src`.
    pub fn cmov(&mut self, cc: Cond, dst: Gpr, src: Rm) {
        const N: [&str; 16] = [
            "cmovo", "cmovno", "cmovb", "cmovae", "cmove", "cmovne", "cmovbe", "cmova", "cmovs",
            "cmovns", "cmovp", "cmovnp", "cmovl", "cmovge", "cmovle", "cmovg",
        ];
        let at = self.pos();
        self.legacy(None, &[0x0f, 0x40 + cc.0], dst.0, src.into());
        self.rec(
            at,
            N[cc.0 as usize],
            [Op::G(dst, Sz::D), rm_op(src, Sz::D), Op::No],
        );
    }

    /// `setcc r8` (`al`..`bl`).
    pub fn setcc(&mut self, cc: Cond, dst: Gpr) {
        const N: [&str; 16] = [
            "seto", "setno", "setb", "setae", "sete", "setne", "setbe", "seta", "sets", "setns",
            "setp", "setnp", "setl", "setge", "setle", "setg",
        ];
        let at = self.pos();
        self.legacy(None, &[0x0f, 0x90 + cc.0], 0, Ea::R(dst.0));
        self.rec(at, N[cc.0 as usize], [Op::G(dst, Sz::B), Op::No, Op::No]);
    }

    /// `push r32` (`50+r`).
    pub fn push(&mut self, r: Gpr) {
        let at = self.pos();
        self.b(0x50 + r.0 as u8);
        self.rec(at, "push", [Op::G(r, Sz::D), Op::No, Op::No]);
    }

    /// `pop r32` (`58+r`).
    pub fn pop(&mut self, r: Gpr) {
        let at = self.pos();
        self.b(0x58 + r.0 as u8);
        self.rec(at, "pop", [Op::G(r, Sz::D), Op::No, Op::No]);
    }

    /// `push imm8` (`6A`, sign-extended).
    pub fn push_i8(&mut self, imm: i8) {
        let at = self.pos();
        self.code.extend_from_slice(&[0x6a, imm as u8]);
        self.rec(at, "push", [Op::I(i64::from(imm)), Op::No, Op::No]);
    }

    /// `push sym` (`68`, the imm32 relocated by `kind`).
    pub fn push_sym(&mut self, kind: RelKind, sym: SymId) {
        let at = self.pos();
        self.b(0x68);
        self.reloc32(kind, sym, 0);
        self.rec(at, "push", [Op::I(0), Op::No, Op::No]);
    }

    /// `push dword m` (`FF /6`).
    pub fn push_m(&mut self, m: Addr) {
        let at = self.pos();
        self.legacy(None, &[0xff], 6, Ea::M(m));
        self.rec(at, "push", [Op::M(m, Ptr::D), Op::No, Op::No]);
    }

    // Arithmetic.

    /// `op dst, src` (the `r/m, r` forms `01`, `09`, .. / byte `00`, ..).
    pub fn alu(&mut self, op: Alu, sz: Sz, dst: Rm, src: Gpr) {
        let at = self.pos();
        let opc = 8 * op as u8 + u8::from(sz != Sz::B);
        self.legacy(sz.pre(), &[opc], src.0, dst.into());
        self.rec(at, op.name(), [rm_op(dst, sz), Op::G(src, sz), Op::No]);
    }

    /// `op dst, src` (the `r, r/m` forms `03`, `0B`, ..).
    pub fn alu_ld(&mut self, op: Alu, dst: Gpr, src: Rm) {
        let at = self.pos();
        self.legacy(None, &[8 * op as u8 + 3], dst.0, src.into());
        self.rec(
            at,
            op.name(),
            [Op::G(dst, Sz::D), rm_op(src, Sz::D), Op::No],
        );
    }

    /// `op dword dst, imm32` (`81 /n`).
    pub fn alu_ri(&mut self, op: Alu, dst: Rm, imm: u32) {
        let at = self.pos();
        self.legacy(None, &[0x81], op as u32, dst.into());
        self.d32(imm);
        self.rec(
            at,
            op.name(),
            [rm_op(dst, Sz::D), Op::I(i64::from(imm)), Op::No],
        );
    }

    /// `op r32, sym + addend` (`81 /n`, the imm32 relocated by `kind`).
    pub fn alu_ri_sym(&mut self, op: Alu, dst: Gpr, kind: RelKind, sym: SymId, addend: i32) {
        let at = self.pos();
        self.legacy(None, &[0x81], op as u32, Ea::R(dst.0));
        self.reloc32(kind, sym, addend);
        self.rec(
            at,
            op.name(),
            [Op::G(dst, Sz::D), Op::I(i64::from(addend as u32)), Op::No],
        );
    }

    /// `op dword dst, imm8` (`83 /n`, sign-extended).
    pub fn alu_ri8(&mut self, op: Alu, dst: Rm, imm: i8) {
        let at = self.pos();
        self.legacy(None, &[0x83], op as u32, dst.into());
        self.b(imm as u8);
        self.rec(
            at,
            op.name(),
            [rm_op(dst, Sz::D), Op::I(i64::from(imm)), Op::No],
        );
    }

    /// `op eax, imm32` (the short `05`, `0D`, .. form).
    pub fn alu_eax(&mut self, op: Alu, imm: u32) {
        let at = self.pos();
        self.b(8 * op as u8 + 5);
        self.d32(imm);
        self.rec(
            at,
            op.name(),
            [Op::G(EAX, Sz::D), Op::I(i64::from(imm)), Op::No],
        );
    }

    /// `test a, b` (`85` / `84`).
    pub fn test(&mut self, sz: Sz, a: Rm, b: Gpr) {
        let at = self.pos();
        let opc = if sz == Sz::B { 0x84 } else { 0x85 };
        self.legacy(None, &[opc], b.0, a.into());
        self.rec(at, "test", [rm_op(a, sz), Op::G(b, sz), Op::No]);
    }

    /// `test dst, imm` (`F7 /0 imm32` / `F6 /0 imm8`).
    pub fn test_ri(&mut self, sz: Sz, dst: Rm, imm: u32) {
        let at = self.pos();
        if sz == Sz::B {
            self.legacy(None, &[0xf6], 0, dst.into());
            self.b(imm as u8);
        } else {
            self.legacy(None, &[0xf7], 0, dst.into());
            self.d32(imm);
        }
        self.rec(at, "test", [rm_op(dst, sz), Op::I(i64::from(imm)), Op::No]);
    }

    /// `not` / `neg` / `mul` / `div` / `idiv` (`F7 /n`).
    pub fn un(&mut self, op: Un, dst: Rm) {
        let at = self.pos();
        self.legacy(None, &[0xf7], op as u32, dst.into());
        self.rec(at, op.name(), [rm_op(dst, Sz::D), Op::No, Op::No]);
    }

    /// `dec dst` (a register: `48+r`; memory: `FF /1`).
    pub fn dec(&mut self, dst: Rm) {
        let at = self.pos();
        match dst {
            Rm::R(r) => self.b(0x48 + r.0 as u8),
            Rm::M(m) => self.legacy(None, &[0xff], 1, Ea::M(m)),
        }
        self.rec(at, "dec", [rm_op(dst, Sz::D), Op::No, Op::No]);
    }

    /// `op dst, n` (`C1 /n ib`; `W`: `66`).
    pub fn shift_ri(&mut self, op: Shift, sz: Sz, dst: Gpr, n: u8) {
        let at = self.pos();
        self.legacy(sz.pre(), &[0xc1], op as u32, Ea::R(dst.0));
        self.b(n);
        self.rec(at, op.name(), [Op::G(dst, sz), Op::I(i64::from(n)), Op::No]);
    }

    /// `op dst, 1` (`D1 /n`).
    pub fn shift_1(&mut self, op: Shift, dst: Gpr) {
        let at = self.pos();
        self.legacy(None, &[0xd1], op as u32, Ea::R(dst.0));
        self.rec(at, op.name(), [Op::G(dst, Sz::D), Op::No, Op::No]);
    }

    /// `op dst, cl` (`D3 /n`).
    pub fn shift_cl(&mut self, op: Shift, dst: Gpr) {
        let at = self.pos();
        self.legacy(None, &[0xd3], op as u32, Ea::R(dst.0));
        self.rec(
            at,
            op.name(),
            [Op::G(dst, Sz::D), Op::G(ECX, Sz::B), Op::No],
        );
    }

    /// `shld dst, src, cl` (`0F A5`) / `shrd` (`0F AD`, `right`).
    pub fn shxd_cl(&mut self, right: bool, dst: Gpr, src: Gpr) {
        let at = self.pos();
        let opc = if right { 0xad } else { 0xa5 };
        self.legacy(None, &[0x0f, opc], src.0, Ea::R(dst.0));
        let mn = if right { "shrd" } else { "shld" };
        self.rec(
            at,
            mn,
            [Op::G(dst, Sz::D), Op::G(src, Sz::D), Op::G(ECX, Sz::B)],
        );
    }

    /// `imul dst, src` (`0F AF`).
    pub fn imul(&mut self, dst: Gpr, src: Rm) {
        let at = self.pos();
        self.legacy(None, &[0x0f, 0xaf], dst.0, src.into());
        self.rec(at, "imul", [Op::G(dst, Sz::D), rm_op(src, Sz::D), Op::No]);
    }

    /// `imul dst, src, imm32` (`69`).
    pub fn imul_ri(&mut self, dst: Gpr, src: Rm, imm: u32) {
        let at = self.pos();
        self.legacy(None, &[0x69], dst.0, src.into());
        self.d32(imm);
        self.rec(
            at,
            "imul",
            [Op::G(dst, Sz::D), rm_op(src, Sz::D), Op::I(i64::from(imm))],
        );
    }

    /// `cdq`: sign-extend `eax` into `edx`.
    pub fn cdq(&mut self) {
        let at = self.pos();
        self.b(0x99);
        self.rec(at, "cdq", [Op::No; 3]);
    }

    /// `bsf` (`0F BC`) / `bsr` (`0F BD`, `rev`) `dst, src`.
    pub fn bsf(&mut self, rev: bool, dst: Gpr, src: Rm) {
        let at = self.pos();
        let opc = if rev { 0xbd } else { 0xbc };
        self.legacy(None, &[0x0f, opc], dst.0, src.into());
        let mn = if rev { "bsr" } else { "bsf" };
        self.rec(at, mn, [Op::G(dst, Sz::D), rm_op(src, Sz::D), Op::No]);
    }

    /// `bswap r` (`0F C8+r`).
    pub fn bswap(&mut self, r: Gpr) {
        let at = self.pos();
        self.code.extend_from_slice(&[0x0f, 0xc8 + r.0 as u8]);
        self.rec(at, "bswap", [Op::G(r, Sz::D), Op::No, Op::No]);
    }

    // Atomics.

    /// The `lock` prefix (objdump shows it as its own line).
    pub fn lock(&mut self) {
        let at = self.pos();
        self.b(0xf0);
        self.rec(at, "lock", [Op::No; 3]);
    }

    /// An `xchg` / `xadd` / `cmpxchg` of `dst` and `r`.
    fn rmw(&mut self, opc: &[u8], byte_opc: &[u8], mn: &'static str, sz: Sz, dst: Rm, r: Gpr) {
        let at = self.pos();
        let opc = if sz == Sz::B { byte_opc } else { opc };
        self.legacy(sz.pre(), opc, r.0, dst.into());
        self.rec(at, mn, [rm_op(dst, sz), Op::G(r, sz), Op::No]);
    }

    /// `xadd m, r` (`0F C1` / `C0`).
    pub fn xadd(&mut self, sz: Sz, m: Addr, r: Gpr) {
        self.rmw(&[0x0f, 0xc1], &[0x0f, 0xc0], "xadd", sz, Rm::M(m), r);
    }

    /// `cmpxchg m, r` (`0F B1` / `B0`).
    pub fn cmpxchg(&mut self, sz: Sz, m: Addr, r: Gpr) {
        self.rmw(&[0x0f, 0xb1], &[0x0f, 0xb0], "cmpxchg", sz, Rm::M(m), r);
    }

    /// `cmpxchg8b m` (`0F C7 /1`).
    pub fn cmpxchg8b(&mut self, m: Addr) {
        let at = self.pos();
        self.legacy(None, &[0x0f, 0xc7], 1, Ea::M(m));
        self.rec(at, "cmpxchg8b", [Op::M(m, Ptr::Q), Op::No, Op::No]);
    }

    pub fn mfence(&mut self) {
        let at = self.pos();
        self.code.extend_from_slice(&[0x0f, 0xae, 0xf0]);
        self.rec(at, "mfence", [Op::No; 3]);
    }

    // Control flow.

    /// `jcc rel32` to offset `target` if known, else 0 for `patch_rel32`:
    /// the rel32's offset.
    pub fn jcc(&mut self, cc: Cond, target: Option<usize>) -> usize {
        let at = self.pos();
        self.code.extend_from_slice(&[0x0f, 0x80 + cc.0]);
        self.rel32(at, JCC[cc.0 as usize], target)
    }

    /// `jmp rel32`, as `jcc`.
    pub fn jmp(&mut self, target: Option<usize>) -> usize {
        let at = self.pos();
        self.b(0xe9);
        self.rel32(at, "jmp", target)
    }

    fn rel32(&mut self, at: usize, mn: &'static str, target: Option<usize>) -> usize {
        let field = self.pos();
        self.d32(0);
        if let Some(t) = target {
            self.patch_rel32(field, t);
        }
        self.rec(at, mn, [Op::Rel, Op::No, Op::No]);
        field
    }

    /// `jcc rel8` (`70+cc`), as `jcc`, for `patch_rel8`.
    pub fn jcc8(&mut self, cc: Cond, target: Option<usize>) -> usize {
        let at = self.pos();
        self.b(0x70 + cc.0);
        self.rel8(at, JCC[cc.0 as usize], target)
    }

    /// `jmp rel8` (`EB`), as `jcc8`.
    pub fn jmp8(&mut self, target: Option<usize>) -> usize {
        let at = self.pos();
        self.b(0xeb);
        self.rel8(at, "jmp", target)
    }

    fn rel8(&mut self, at: usize, mn: &'static str, target: Option<usize>) -> usize {
        let field = self.pos();
        self.b(0);
        if let Some(t) = target {
            self.patch_rel8(field, t);
        }
        self.rec(at, mn, [Op::Rel8, Op::No, Op::No]);
        field
    }

    /// `call rel32` to `sym` (relocated by `kind`, the field `addend`).
    pub fn call(&mut self, kind: RelKind, sym: SymId, addend: i32) {
        let at = self.pos();
        self.b(0xe8);
        self.reloc32(kind, sym, addend);
        self.rec(at, "call", [Op::Rel, Op::No, Op::No]);
    }

    /// `call` the next instruction (`E8 00000000`): its address pushed.
    pub fn call_next(&mut self) {
        let at = self.pos();
        self.b(0xe8);
        self.d32(0);
        self.rec(at, "call", [Op::Rel, Op::No, Op::No]);
    }

    /// `call r32` (`FF /2`).
    pub fn call_r(&mut self, r: Gpr) {
        let at = self.pos();
        self.legacy(None, &[0xff], 2, Ea::R(r.0));
        self.rec(at, "call", [Op::G(r, Sz::D), Op::No, Op::No]);
    }

    pub fn ret(&mut self) {
        let at = self.pos();
        self.b(0xc3);
        self.rec(at, "ret", [Op::No; 3]);
    }

    /// `ret n` (`C2 iw`): `n` bytes of arguments popped.
    pub fn ret_n(&mut self, n: u16) {
        let at = self.pos();
        self.b(0xc2);
        self.code.extend_from_slice(&n.to_le_bytes());
        self.rec(at, "ret", [Op::I(i64::from(n)), Op::No, Op::No]);
    }

    pub fn ud2(&mut self) {
        let at = self.pos();
        self.code.extend_from_slice(&[0x0f, 0x0b]);
        self.rec(at, "ud2", [Op::No; 3]);
    }

    pub fn int3(&mut self) {
        let at = self.pos();
        self.b(0xcc);
        self.rec(at, "int3", [Op::No; 3]);
    }

    // x87.

    /// An x87 load, store or control-word access of `m`.
    pub fn x87(&mut self, op: X87, m: Addr) {
        let (opc, ext, mn, ptr) = op.enc();
        let at = self.pos();
        self.legacy(None, &[opc], ext, Ea::M(m));
        self.rec(at, mn, [Op::M(m, ptr), Op::No, Op::No]);
    }

    // SSE.

    /// `op dst, src`.
    pub fn sse(&mut self, op: Sse, dst: Xmm, src: Xrm) {
        let (pre, opc, mn, ptr) = op.enc();
        let at = self.pos();
        self.legacy(pre, &[0x0f, opc], dst.0, src.into());
        self.rec(at, mn, [Op::X(dst), xrm_op(src, ptr), Op::No]);
    }

    /// `op m, src` (the store forms).
    pub fn sse_st(&mut self, op: Sse, m: Addr, src: Xmm) {
        let (pre, opc, mn, ptr) = op.enc();
        let at = self.pos();
        self.legacy(pre, &[0x0f, opc], src.0, Ea::M(m));
        self.rec(at, mn, [Op::M(m, ptr), Op::X(src), Op::No]);
    }

    /// `op dst, src, imm8` (`pshufd`).
    pub fn sse_ri(&mut self, op: Sse, dst: Xmm, src: Xmm, imm: u8) {
        let (pre, opc, mn, _) = op.enc();
        let at = self.pos();
        self.legacy(pre, &[0x0f, opc], dst.0, Ea::R(src.0));
        self.b(imm);
        self.rec(at, mn, [Op::X(dst), Op::X(src), Op::I(i64::from(imm))]);
    }

    /// `cmp<pred>ps` / `pd dst, src` (`0F C2 ib`).
    pub fn cmpp(&mut self, pd: bool, pred: FCmp, dst: Xmm, src: Xmm) {
        let at = self.pos();
        let pre = pd.then_some(0x66);
        self.legacy(pre, &[0x0f, 0xc2], dst.0, Ea::R(src.0));
        self.b(pred as u8);
        self.rec(at, pred.name(pd), [Op::X(dst), Op::X(src), Op::No]);
    }

    /// A shift of `dst` by `n` (`66 0F 71..73 /n ib`).
    pub fn pshift(&mut self, op: PShift, dst: Xmm, n: u8) {
        let (opc, ext, mn) = op.enc();
        let at = self.pos();
        self.legacy(Some(0x66), &[0x0f, opc], ext, Ea::R(dst.0));
        self.b(n);
        self.rec(at, mn, [Op::X(dst), Op::I(i64::from(n)), Op::No]);
    }

    /// `movd xmm, r32` (`66 0F 6E`).
    pub fn movd_xg(&mut self, dst: Xmm, src: Gpr) {
        let at = self.pos();
        self.legacy(Some(0x66), &[0x0f, 0x6e], dst.0, Ea::R(src.0));
        self.rec(at, "movd", [Op::X(dst), Op::G(src, Sz::D), Op::No]);
    }

    /// `movd r32, xmm` (`66 0F 7E`).
    pub fn movd_gx(&mut self, dst: Gpr, src: Xmm) {
        let at = self.pos();
        self.legacy(Some(0x66), &[0x0f, 0x7e], src.0, Ea::R(dst.0));
        self.rec(at, "movd", [Op::G(dst, Sz::D), Op::X(src), Op::No]);
    }

    /// `cvtsi2ss` / `cvtsi2sd xmm, r32` (`pd`: double).
    pub fn cvtsi2f(&mut self, pd: bool, dst: Xmm, src: Gpr) {
        let at = self.pos();
        let pre = if pd { 0xf2 } else { 0xf3 };
        self.legacy(Some(pre), &[0x0f, 0x2a], dst.0, Ea::R(src.0));
        let mn = if pd { "cvtsi2sd" } else { "cvtsi2ss" };
        self.rec(at, mn, [Op::X(dst), Op::G(src, Sz::D), Op::No]);
    }

    /// `pmovmskb` / `movmskps` / `movmskpd r32, xmm`.
    pub fn movmsk(&mut self, op: Movmsk, dst: Gpr, src: Xmm) {
        let (pre, opc, mn) = match op {
            Movmsk::Pmovmskb => (Some(0x66), 0xd7, "pmovmskb"),
            Movmsk::Movmskps => (None, 0x50, "movmskps"),
            Movmsk::Movmskpd => (Some(0x66), 0x50, "movmskpd"),
        };
        let at = self.pos();
        self.legacy(pre, &[0x0f, opc], dst.0, Ea::R(src.0));
        self.rec(at, mn, [Op::G(dst, Sz::D), Op::X(src), Op::No]);
    }

    /// Each recorded instruction's offset and text.
    #[must_use]
    pub fn lines(&self, l: &Listing<'_>) -> Vec<(u32, String)> {
        let Some(t) = &self.text else {
            return Vec::new();
        };
        let mut out = Vec::with_capacity(t.len());
        for (n, line) in t.iter().enumerate() {
            let end = t.get(n + 1).map_or(self.code.len(), |x| x.at as usize);
            let ops: Vec<String> = line
                .ops
                .iter()
                .filter(|o| !matches!(o, Op::No))
                .map(|o| match *o {
                    Op::Rel => {
                        let rel = i32::from_le_bytes(
                            self.code[end - 4..end].try_into().unwrap_or([0; 4]),
                        );
                        l.target(end as i64 + i64::from(rel))
                    }
                    Op::Rel8 => {
                        let rel = self.code[end - 1] as i8;
                        l.target(end as i64 + i64::from(rel))
                    }
                    o => op_text(o),
                })
                .collect();
            let text = if ops.is_empty() {
                line.mn.to_owned()
            } else {
                format!("{}\t{}", line.mn, ops.join(", "))
            };
            out.push((line.at, text));
        }
        out
    }
}

const JCC: [&str; 16] = [
    "jo", "jno", "jb", "jae", "je", "jne", "jbe", "ja", "js", "jns", "jp", "jnp", "jl", "jge",
    "jle", "jg",
];

const fn rm_op(rm: Rm, sz: Sz) -> Op {
    match rm {
        Rm::R(g) => Op::G(g, sz),
        Rm::M(m) => Op::M(m, sz.ptr()),
    }
}

const fn xrm_op(rm: Xrm, ptr: Ptr) -> Op {
    match rm {
        Xrm::X(x) => Op::X(x),
        Xrm::M(m) => Op::M(m, ptr),
    }
}

fn hex(v: i64) -> String {
    if v < 0 {
        format!("-{:#x}", v.unsigned_abs())
    } else {
        format!("{v:#x}")
    }
}

const fn gpr_name(g: Gpr, sz: Sz) -> &'static str {
    const D: [&str; 8] = ["eax", "ecx", "edx", "ebx", "esp", "ebp", "esi", "edi"];
    const W: [&str; 8] = ["ax", "cx", "dx", "bx", "sp", "bp", "si", "di"];
    const B: [&str; 8] = ["al", "cl", "dl", "bl", "ah", "ch", "dh", "bh"];
    let n = (g.0 & 7) as usize;
    match sz {
        Sz::D => D[n],
        Sz::W => W[n],
        Sz::B => B[n],
    }
}

fn op_text(o: Op) -> String {
    match o {
        Op::G(g, sz) => gpr_name(g, sz).to_owned(),
        Op::X(x) => format!("xmm{}", x.0),
        Op::I(v) => hex(v),
        Op::M(m, ptr) => {
            let p = match ptr {
                Ptr::None => "",
                Ptr::B => "byte ptr ",
                Ptr::W => "word ptr ",
                Ptr::D => "dword ptr ",
                Ptr::Q => "qword ptr ",
                Ptr::X => "xmmword ptr ",
            };
            let r = |g| gpr_name(g, Sz::D);
            let a = match m {
                Addr::Bd(b, 0) | Addr::BdRel(b, ..) => format!("[{}]", r(b)),
                Addr::Bd(b, d) if d < 0 => {
                    format!("[{} - {:#x}]", r(b), i64::from(d).unsigned_abs())
                }
                Addr::Bd(b, d) => format!("[{} + {d:#x}]", r(b)),
                Addr::Bi(b, i, 0) => format!("[{} + {}]", r(b), r(i)),
                Addr::Bi(b, i, sc) => format!("[{} + {}*{}]", r(b), 1 << sc, r(i)),
                Addr::Abs(..) => "[0x0]".to_owned(),
                Addr::Seg(seg, d) => {
                    let s = if seg == Seg::Fs { "fs" } else { "gs" };
                    format!("{s}:[{d:#x}]")
                }
            };
            format!("{p}{a}")
        }
        Op::No | Op::Rel | Op::Rel8 => String::new(),
    }
}
