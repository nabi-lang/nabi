//! `x86_64` instruction encoder: one function per instruction form the
//! emitter uses, the only place that knows opcodes, prefixes, REX, VEX,
//! `ModRM` and SIB. With `Asm::text` on (`--emit=asm`) each function also
//! records its instruction; `lines` prints them in Intel syntax as
//! `llvm-objdump -d -M intel` does. Off, nothing is recorded.
//!
//! Each form has one encoding: `alu_ri` always takes an
//! imm32 and `alu_ri8` an imm8, `mov` reg-reg is `8B` and `mov_st` `89`, a
//! byte operand form (`Sz::B`, `movzx` / `movsx` / `setcc` of a byte)
//! has a REX whenever either register is 4..8, and a VEX memory operand
//! always has a displacement.

mod sse;
#[cfg(test)]
mod tests;
mod text;

pub(crate) use crate::isa::x64::enc::sse::*;
pub(crate) use crate::isa::x64::enc::text::*;

use crate::asm::Listing;
use crate::lir::SymId;
use crate::obj::RelKind;

/// A general-purpose register, `rax` = 0 .. `r15` = 15.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Gpr(pub u32);

/// An `xmm` (`ymm` in 256-bit forms) register, 0 .. 15.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Xmm(pub u32);

pub const RAX: Gpr = Gpr(0);
pub const RCX: Gpr = Gpr(1);
pub const RBP: Gpr = Gpr(5);

/// Operand size.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Sz {
    B,
    W,
    D,
    Q,
}

impl Sz {
    /// `Q` when `w` (REX.W), else `D`.
    #[must_use]
    pub const fn dq(w: bool) -> Self {
        if w { Self::Q } else { Self::D }
    }

    const fn w(self) -> bool {
        matches!(self, Self::Q)
    }

    const fn ptr(self) -> Ptr {
        match self {
            Self::B => Ptr::B,
            Self::W => Ptr::W,
            Self::D => Ptr::D,
            Self::Q => Ptr::Q,
        }
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
    /// `[rip + sym]`: the disp32 relocated by the kind.
    Rip(RelKind, SymId),
    /// `[base + sym]`: the disp32 relocated by the kind.
    BdRel(Gpr, RelKind, SymId),
    /// `seg:[disp32]`.
    Abs(Seg, i32),
}

/// A register-or-memory operand.
#[derive(Clone, Copy, Debug)]
pub enum Rm {
    R(Gpr),
    M(Addr),
}

impl Rm {
    /// Register `r`.
    #[must_use]
    pub const fn r(r: u32) -> Self {
        Self::R(Gpr(r))
    }

    /// `[base + disp]`.
    #[must_use]
    pub const fn m(base: u32, disp: i32) -> Self {
        Self::M(Addr::Bd(Gpr(base), disp))
    }
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
    Shl = 4,
    Shr = 5,
    Sar = 7,
}

impl Shift {
    const fn name(self) -> &'static str {
        match self {
            Self::Rol => "rol",
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
    /// One-operand `imul`: `rdx:rax = rax * src`, signed.
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

/// Bit scans and counts (`0F BC` / `BD`, `F3 0F B8` / `BC` / `BD`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Bits {
    Bsf,
    Bsr,
    Popcnt,
    Lzcnt,
    Tzcnt,
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
    Y,
}

/// A recorded operand.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Op {
    No,
    G(Gpr, Sz),
    /// `xmm` (`false`) or `ymm`.
    X(Xmm, bool),
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

    fn d32(&mut self, x: i32) {
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

    /// A legacy encoding: prefix, REX (`w`; `byte`: an empty one for
    /// `spl`..`dil`), opcode, `ModRM` of `reg` and `rm`.
    fn legacy(&mut self, pre: Option<u8>, w: bool, byte: bool, opc: &[u8], reg: u32, rm: Ea) {
        if let Some(p) = pre {
            self.b(p);
        }
        let (base, index) = match rm {
            Ea::R(r) | Ea::M(Addr::Bd(Gpr(r), _) | Addr::BdRel(Gpr(r), ..)) => (r, 0),
            Ea::M(Addr::Bi(Gpr(b), Gpr(i), _)) => (b, i),
            Ea::M(Addr::Rip(..)) => (0, 0),
            Ea::M(Addr::Abs(seg, _)) => {
                self.b(if seg == Seg::Fs { 0x64 } else { 0x65 });
                (0, 0)
            }
        };
        let mut rex = 0x40u8;
        if w {
            rex |= 8;
        }
        if reg >= 8 {
            rex |= 4;
        }
        if index >= 8 {
            rex |= 2;
        }
        if base >= 8 {
            rex |= 1;
        }
        let force =
            byte && ((4..8).contains(&reg) || matches!(rm, Ea::R(r) if (4..8).contains(&r)));
        if rex != 0x40 || force {
            self.b(rex);
        }
        self.code.extend_from_slice(opc);
        self.modrm(reg, rm, false);
    }

    /// `ModRM` (SIB, displacement, relocation) of `reg` and `rm`; `vex`:
    /// `[base + disp]` always has a displacement.
    fn modrm(&mut self, reg: u32, rm: Ea, vex: bool) {
        let r = ((reg & 7) << 3) as u8;
        match rm {
            Ea::R(x) => self.b(0xc0 | r | (x & 7) as u8),
            Ea::M(Addr::Bd(Gpr(base), disp)) => {
                let lo = (base & 7) as u8;
                let (md, small) = if disp == 0 && lo != 5 && !vex {
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
                    Some(false) => self.d32(disp),
                    None => {}
                }
            }
            Ea::M(Addr::Bi(Gpr(base), Gpr(index), sc)) => {
                // `rbp` / `r13` as a base need a displacement.
                let lo = (base & 7) as u8;
                let md = if lo == 5 { 0x40 } else { 0 };
                self.b(md | r | 4);
                self.b((sc << 6) | (((index & 7) << 3) as u8) | lo);
                if lo == 5 {
                    self.b(0);
                }
            }
            Ea::M(Addr::Rip(kind, sym)) => {
                self.b(r | 5);
                self.reloc32(kind, sym);
            }
            Ea::M(Addr::BdRel(Gpr(base), kind, sym)) => {
                let lo = (base & 7) as u8;
                self.b(0x80 | r | lo);
                if lo == 4 {
                    self.b(0x24);
                }
                self.reloc32(kind, sym);
            }
            Ea::M(Addr::Abs(_, disp)) => {
                self.b(r | 4);
                self.b(0x25); // SIB: no base, no index
                self.d32(disp);
            }
        }
    }

    /// A zero 32-bit field relocated by `kind` against `sym`.
    fn reloc32(&mut self, kind: RelKind, sym: SymId) {
        self.relocs.push((self.code.len() as u32, kind, sym));
        self.d32(0);
    }

    /// A VEX encoding: `reg`, `vvvv` (the extra source), `rm`; `l`: 256
    /// bits.
    #[allow(clippy::too_many_arguments)]
    fn vex(&mut self, pp: u8, map: u8, w: bool, l: bool, opc: u8, reg: u32, vvvv: u32, rm: Ea) {
        let (b, x) = match rm {
            Ea::R(r) | Ea::M(Addr::Bd(Gpr(r), _) | Addr::BdRel(Gpr(r), ..)) => (r, 0),
            Ea::M(Addr::Bi(Gpr(b), Gpr(i), _)) => (b, i),
            Ea::M(Addr::Rip(..) | Addr::Abs(..)) => (0, 0),
        };
        let r = u8::from(reg < 8) << 7;
        let tail = (u8::from(w) << 7) | ((!vvvv as u8 & 15) << 3) | (u8::from(l) << 2) | pp;
        if map == M0F && !w && b < 8 && x < 8 {
            self.b(0xc5);
            self.b(r | (tail & 0x7f));
        } else {
            self.b(0xc4);
            self.b(r | (u8::from(x < 8) << 6) | (u8::from(b < 8) << 5) | map);
            self.b(tail);
        }
        self.b(opc);
        self.modrm(reg, rm, true);
    }

    // Moves.

    /// `mov dst, src` (`8B` / `8A`).
    pub fn mov(&mut self, sz: Sz, dst: Gpr, src: Rm) {
        let at = self.pos();
        match sz {
            Sz::B => self.legacy(None, false, true, &[0x8a], dst.0, src.into()),
            Sz::W => self.legacy(Some(0x66), false, false, &[0x8b], dst.0, src.into()),
            _ => self.legacy(None, sz.w(), false, &[0x8b], dst.0, src.into()),
        }
        self.rec(at, "mov", [Op::G(dst, sz), rm_op(src, sz), Op::No]);
    }

    /// `mov dst, src` (`89` / `88`).
    pub fn mov_st(&mut self, sz: Sz, dst: Rm, src: Gpr) {
        let at = self.pos();
        match sz {
            Sz::B => self.legacy(None, false, true, &[0x88], src.0, dst.into()),
            Sz::W => self.legacy(Some(0x66), false, false, &[0x89], src.0, dst.into()),
            _ => self.legacy(None, sz.w(), false, &[0x89], src.0, dst.into()),
        }
        self.rec(at, "mov", [rm_op(dst, sz), Op::G(src, sz), Op::No]);
    }

    /// `mov r32, imm32` (zero-extends).
    pub fn mov_ri32(&mut self, dst: Gpr, imm: u32) {
        let at = self.pos();
        if dst.0 >= 8 {
            self.b(0x41);
        }
        self.b(0xb8 + (dst.0 & 7) as u8);
        self.d32(imm as i32);
        self.rec(
            at,
            "mov",
            [Op::G(dst, Sz::D), Op::I(i64::from(imm)), Op::No],
        );
    }

    /// `mov r64, simm32`.
    pub fn mov_ri_sx(&mut self, dst: Gpr, imm: i32) {
        let at = self.pos();
        self.legacy(None, true, false, &[0xc7], 0, Ea::R(dst.0));
        self.d32(imm);
        self.rec(
            at,
            "mov",
            [Op::G(dst, Sz::Q), Op::I(i64::from(imm)), Op::No],
        );
    }

    /// `mov dst, simm32` (`C7 /0`; `Q`: sign-extended).
    pub fn mov_mi(&mut self, sz: Sz, dst: Rm, imm: i32) {
        let at = self.pos();
        self.legacy(None, sz.w(), false, &[0xc7], 0, dst.into());
        self.d32(imm);
        self.rec(at, "mov", [rm_op(dst, sz), imm32(sz, imm), Op::No]);
    }

    /// `movabs r64, imm64`.
    pub fn movabs(&mut self, dst: Gpr, imm: u64) {
        let at = self.pos();
        self.b(0x48 | u8::from(dst.0 >= 8));
        self.b(0xb8 + (dst.0 & 7) as u8);
        self.code.extend_from_slice(&imm.to_le_bytes());
        self.rec(at, "movabs", [Op::G(dst, Sz::Q), Op::I(imm as i64), Op::No]);
    }

    /// `movzx dst, src` (`from`: `B` / `W`).
    pub fn movzx(&mut self, sz: Sz, dst: Gpr, from: Sz, src: Rm) {
        let at = self.pos();
        let byte = from == Sz::B;
        let opc = if byte { 0xb6 } else { 0xb7 };
        self.legacy(None, sz.w(), byte, &[0x0f, opc], dst.0, src.into());
        self.rec(at, "movzx", [Op::G(dst, sz), rm_op(src, from), Op::No]);
    }

    /// `movsx dst, src` (`from`: `B` / `W`).
    pub fn movsx(&mut self, sz: Sz, dst: Gpr, from: Sz, src: Rm) {
        let at = self.pos();
        let byte = from == Sz::B;
        let opc = if byte { 0xbe } else { 0xbf };
        self.legacy(None, sz.w(), byte, &[0x0f, opc], dst.0, src.into());
        self.rec(at, "movsx", [Op::G(dst, sz), rm_op(src, from), Op::No]);
    }

    /// `movsxd r64, r/m32`.
    pub fn movsxd(&mut self, dst: Gpr, src: Rm) {
        let at = self.pos();
        self.legacy(None, true, false, &[0x63], dst.0, src.into());
        self.rec(at, "movsxd", [Op::G(dst, Sz::Q), rm_op(src, Sz::D), Op::No]);
    }

    /// `lea r64, m`.
    pub fn lea(&mut self, dst: Gpr, m: Addr) {
        let at = self.pos();
        self.legacy(None, true, false, &[0x8d], dst.0, Ea::M(m));
        self.rec(at, "lea", [Op::G(dst, Sz::Q), Op::M(m, Ptr::None), Op::No]);
    }

    /// `cmovcc dst, src`.
    pub fn cmov(&mut self, cc: Cond, sz: Sz, dst: Gpr, src: Rm) {
        const N: [&str; 16] = [
            "cmovo", "cmovno", "cmovb", "cmovae", "cmove", "cmovne", "cmovbe", "cmova", "cmovs",
            "cmovns", "cmovp", "cmovnp", "cmovl", "cmovge", "cmovle", "cmovg",
        ];
        let at = self.pos();
        self.legacy(None, sz.w(), false, &[0x0f, 0x40 + cc.0], dst.0, src.into());
        self.rec(
            at,
            N[cc.0 as usize],
            [Op::G(dst, sz), rm_op(src, sz), Op::No],
        );
    }

    /// `setcc r8`.
    pub fn setcc(&mut self, cc: Cond, dst: Gpr) {
        const N: [&str; 16] = [
            "seto", "setno", "setb", "setae", "sete", "setne", "setbe", "seta", "sets", "setns",
            "setp", "setnp", "setl", "setge", "setle", "setg",
        ];
        let at = self.pos();
        self.legacy(None, false, true, &[0x0f, 0x90 + cc.0], 0, Ea::R(dst.0));
        self.rec(at, N[cc.0 as usize], [Op::G(dst, Sz::B), Op::No, Op::No]);
    }

    // Arithmetic.

    /// `op dst, src` (register forms `01`, `09`, .. / byte `00`, ..).
    pub fn alu_rr(&mut self, op: Alu, sz: Sz, dst: Gpr, src: Gpr) {
        let at = self.pos();
        let opc = 8 * op as u8 + u8::from(sz != Sz::B);
        let byte = sz == Sz::B;
        self.legacy(None, sz.w(), byte, &[opc], src.0, Ea::R(dst.0));
        self.rec(at, op.name(), [Op::G(dst, sz), Op::G(src, sz), Op::No]);
    }

    /// `op dst, imm32` (`81 /n`).
    pub fn alu_ri(&mut self, op: Alu, sz: Sz, dst: Rm, imm: i32) {
        let at = self.pos();
        self.legacy(None, sz.w(), false, &[0x81], op as u32, dst.into());
        self.d32(imm);
        self.rec(at, op.name(), [rm_op(dst, sz), imm32(sz, imm), Op::No]);
    }

    /// `op dst, imm8` (`83 /n`, sign-extended).
    pub fn alu_ri8(&mut self, op: Alu, sz: Sz, dst: Rm, imm: i8) {
        let at = self.pos();
        self.legacy(None, sz.w(), false, &[0x83], op as u32, dst.into());
        self.b(imm as u8);
        self.rec(
            at,
            op.name(),
            [rm_op(dst, sz), Op::I(i64::from(imm)), Op::No],
        );
    }

    /// `op eax, imm32` (the short `05`, `0D`, .. form).
    pub fn alu_eax(&mut self, op: Alu, imm: i32) {
        let at = self.pos();
        self.b(8 * op as u8 + 5);
        self.d32(imm);
        self.rec(
            at,
            op.name(),
            [Op::G(RAX, Sz::D), imm32(Sz::D, imm), Op::No],
        );
    }

    /// `op rax, imm32` (`REX.W 05`, `2D`, .., sign-extended).
    pub fn alu_rax(&mut self, op: Alu, imm: i32) {
        let at = self.pos();
        self.b(0x48);
        self.b(8 * op as u8 + 5);
        self.d32(imm);
        self.rec(
            at,
            op.name(),
            [Op::G(RAX, Sz::Q), imm32(Sz::Q, imm), Op::No],
        );
    }

    /// `test m, r` (`85`).
    pub fn test_mr(&mut self, sz: Sz, m: Addr, r: Gpr) {
        let at = self.pos();
        self.legacy(None, sz.w(), false, &[0x85], r.0, Ea::M(m));
        self.rec(at, "test", [Op::M(m, sz.ptr()), Op::G(r, sz), Op::No]);
    }

    /// `test a, b` (`85`).
    pub fn test_rr(&mut self, sz: Sz, a: Gpr, b: Gpr) {
        let at = self.pos();
        self.legacy(None, sz.w(), false, &[0x85], b.0, Ea::R(a.0));
        self.rec(at, "test", [Op::G(a, sz), Op::G(b, sz), Op::No]);
    }

    /// `test dst, imm32` (`F7 /0`).
    pub fn test_ri(&mut self, sz: Sz, dst: Rm, imm: i32) {
        let at = self.pos();
        self.legacy(None, sz.w(), false, &[0xf7], 0, dst.into());
        self.d32(imm);
        self.rec(at, "test", [rm_op(dst, sz), imm32(sz, imm), Op::No]);
    }

    /// `not` / `neg` / `mul` / `div` / `idiv` (`F7 /n`).
    pub fn un(&mut self, op: Un, sz: Sz, dst: Rm) {
        let at = self.pos();
        self.legacy(None, sz.w(), false, &[0xf7], op as u32, dst.into());
        self.rec(at, op.name(), [rm_op(dst, sz), Op::No, Op::No]);
    }

    /// `dec dst` (`FF /1`).
    pub fn dec(&mut self, sz: Sz, dst: Gpr) {
        let at = self.pos();
        self.legacy(None, sz.w(), false, &[0xff], 1, Ea::R(dst.0));
        self.rec(at, "dec", [Op::G(dst, sz), Op::No, Op::No]);
    }

    /// `op dst, n` (`C1 /n ib`; `W`: `66`).
    pub fn shift_ri(&mut self, op: Shift, sz: Sz, dst: Gpr, n: u8) {
        let at = self.pos();
        let pre = (sz == Sz::W).then_some(0x66);
        self.legacy(pre, sz.w(), false, &[0xc1], op as u32, Ea::R(dst.0));
        self.b(n);
        self.rec(at, op.name(), [Op::G(dst, sz), Op::I(i64::from(n)), Op::No]);
    }

    /// `op dst, 1` (`D1 /n`).
    pub fn shift_1(&mut self, op: Shift, sz: Sz, dst: Gpr) {
        let at = self.pos();
        self.legacy(None, sz.w(), false, &[0xd1], op as u32, Ea::R(dst.0));
        self.rec(at, op.name(), [Op::G(dst, sz), Op::No, Op::No]);
    }

    /// `op dst, cl` (`D3 /n`).
    pub fn shift_cl(&mut self, op: Shift, sz: Sz, dst: Gpr) {
        let at = self.pos();
        self.legacy(None, sz.w(), false, &[0xd3], op as u32, Ea::R(dst.0));
        self.rec(at, op.name(), [Op::G(dst, sz), Op::G(RCX, Sz::B), Op::No]);
    }

    /// `imul dst, src` (`0F AF`).
    pub fn imul(&mut self, sz: Sz, dst: Gpr, src: Rm) {
        let at = self.pos();
        self.legacy(None, sz.w(), false, &[0x0f, 0xaf], dst.0, src.into());
        self.rec(at, "imul", [Op::G(dst, sz), rm_op(src, sz), Op::No]);
    }

    /// `imul dst, src, imm32` (`69`).
    pub fn imul_ri(&mut self, sz: Sz, dst: Gpr, src: Rm, imm: i32) {
        let at = self.pos();
        self.legacy(None, sz.w(), false, &[0x69], dst.0, src.into());
        self.d32(imm);
        self.rec(at, "imul", [Op::G(dst, sz), rm_op(src, sz), imm32(sz, imm)]);
    }

    /// `imul dst, src, imm8` (`6B /r ib`, sign-extended).
    pub fn imul_ri8(&mut self, sz: Sz, dst: Gpr, src: Rm, imm: i8) {
        let at = self.pos();
        self.legacy(None, sz.w(), false, &[0x6b], dst.0, src.into());
        self.b(imm as u8);
        self.rec(
            at,
            "imul",
            [Op::G(dst, sz), rm_op(src, sz), Op::I(i64::from(imm))],
        );
    }

    /// `cqo` (`Q`) / `cdq`: sign-extend `rax` into `rdx`.
    pub fn cqo(&mut self, sz: Sz) {
        let at = self.pos();
        if sz.w() {
            self.b(0x48);
        }
        self.b(0x99);
        self.rec(at, if sz.w() { "cqo" } else { "cdq" }, [Op::No; 3]);
    }

    /// A bit scan or count (`bsf`, `bsr`, `popcnt`, `lzcnt`, `tzcnt`).
    pub fn bits(&mut self, op: Bits, sz: Sz, dst: Gpr, src: Rm) {
        let (pre, opc, mn) = match op {
            Bits::Bsf => (None, 0xbc, "bsf"),
            Bits::Bsr => (None, 0xbd, "bsr"),
            Bits::Popcnt => (Some(0xf3), 0xb8, "popcnt"),
            Bits::Lzcnt => (Some(0xf3), 0xbd, "lzcnt"),
            Bits::Tzcnt => (Some(0xf3), 0xbc, "tzcnt"),
        };
        let at = self.pos();
        self.legacy(pre, sz.w(), false, &[0x0f, opc], dst.0, src.into());
        self.rec(at, mn, [Op::G(dst, sz), rm_op(src, sz), Op::No]);
    }

    /// `bts dst, n` (`0F BA /5 ib`).
    pub fn bts(&mut self, sz: Sz, dst: Gpr, n: u8) {
        let at = self.pos();
        self.legacy(None, sz.w(), false, &[0x0f, 0xba], 5, Ea::R(dst.0));
        self.b(n);
        self.rec(at, "bts", [Op::G(dst, sz), Op::I(i64::from(n)), Op::No]);
    }

    /// `bswap r` (`0F C8+r`).
    pub fn bswap(&mut self, sz: Sz, r: Gpr) {
        let at = self.pos();
        let rex = 0x40 | (u8::from(sz.w()) << 3) | u8::from(r.0 >= 8);
        if rex != 0x40 {
            self.b(rex);
        }
        self.code.extend_from_slice(&[0x0f, 0xc8 + (r.0 & 7) as u8]);
        self.rec(at, "bswap", [Op::G(r, sz), Op::No, Op::No]);
    }

    // Atomics.

    /// The `lock` prefix (objdump shows it as its own line).
    pub fn lock(&mut self) {
        let at = self.pos();
        self.b(0xf0);
        self.rec(at, "lock", [Op::No; 3]);
    }

    /// An `xchg` / `xadd` / `cmpxchg` of `m` and `r`.
    fn rmw(&mut self, opc: &[u8], byte_opc: &[u8], mn: &'static str, sz: Sz, m: Addr, r: Gpr) {
        let at = self.pos();
        match sz {
            Sz::B => self.legacy(None, false, true, byte_opc, r.0, Ea::M(m)),
            Sz::W => self.legacy(Some(0x66), false, false, opc, r.0, Ea::M(m)),
            _ => self.legacy(None, sz.w(), false, opc, r.0, Ea::M(m)),
        }
        self.rec(at, mn, [Op::M(m, sz.ptr()), Op::G(r, sz), Op::No]);
    }

    /// `xchg m, r` (`87` / `86`).
    pub fn xchg(&mut self, sz: Sz, m: Addr, r: Gpr) {
        self.rmw(&[0x87], &[0x86], "xchg", sz, m, r);
    }

    /// `xadd m, r` (`0F C1` / `C0`).
    pub fn xadd(&mut self, sz: Sz, m: Addr, r: Gpr) {
        self.rmw(&[0x0f, 0xc1], &[0x0f, 0xc0], "xadd", sz, m, r);
    }

    /// `cmpxchg m, r` (`0F B1` / `B0`).
    pub fn cmpxchg(&mut self, sz: Sz, m: Addr, r: Gpr) {
        self.rmw(&[0x0f, 0xb1], &[0x0f, 0xb0], "cmpxchg", sz, m, r);
    }

    // Control flow.

    /// `jcc rel32` to offset `target` if known, else 0 for `patch_rel32`:
    /// the rel32's offset.
    pub fn jcc(&mut self, cc: Cond, target: Option<usize>) -> usize {
        const N: [&str; 16] = [
            "jo", "jno", "jb", "jae", "je", "jne", "jbe", "ja", "js", "jns", "jp", "jnp", "jl",
            "jge", "jle", "jg",
        ];
        let at = self.pos();
        self.code.extend_from_slice(&[0x0f, 0x80 + cc.0]);
        self.rel32(at, N[cc.0 as usize], target)
    }

    /// `jmp rel32`, as `jcc`.
    pub fn jmp(&mut self, target: Option<usize>) -> usize {
        let at = self.pos();
        self.b(0xe9);
        self.rel32(at, "jmp", target)
    }

    /// `jmp rel8` (`EB`), as `jcc8`.
    pub fn jmp8(&mut self, target: Option<usize>) -> usize {
        let at = self.pos();
        self.b(0xeb);
        let field = self.pos();
        self.b(0);
        if let Some(t) = target {
            self.patch_rel8(field, t);
        }
        self.rec(at, "jmp", [Op::Rel8, Op::No, Op::No]);
        field
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

    /// `call rel32` to `sym` (relocated by `kind`).
    pub fn call(&mut self, kind: RelKind, sym: SymId) {
        let at = self.pos();
        self.b(0xe8);
        self.reloc32(kind, sym);
        self.rec(at, "call", [Op::Rel, Op::No, Op::No]);
    }

    /// `data16 lea rdi, [rip + sym@tlsgd]; data16 data16 rex64 call
    /// get@plt`: the general dynamic sequence, byte for byte as linkers
    /// relax it.
    pub fn tls_gd(&mut self, sym: SymId, get: SymId) {
        let at = self.pos();
        self.code.extend_from_slice(&[0x66, 0x48, 0x8d, 0x3d]);
        self.reloc32(RelKind::X86TlsGd, sym);
        let m = Addr::Rip(RelKind::X86TlsGd, sym);
        self.rec(
            at,
            "data16 lea",
            [Op::G(Gpr(7), Sz::Q), Op::M(m, Ptr::None), Op::No],
        );
        let at = self.pos();
        self.code.extend_from_slice(&[0x66, 0x66, 0x48, 0xe8]);
        self.reloc32(RelKind::X86Call, get);
        self.rec(at, "data16 data16 rex64 call", [Op::Rel, Op::No, Op::No]);
    }

    /// `call r64` (`FF /2`).
    pub fn call_r(&mut self, r: Gpr) {
        let at = self.pos();
        self.legacy(None, false, false, &[0xff], 2, Ea::R(r.0));
        self.rec(at, "call", [Op::G(r, Sz::Q), Op::No, Op::No]);
    }

    /// `jcc rel8` (`70+cc`), as `jcc`, for `patch_rel8`.
    pub fn jcc8(&mut self, cc: Cond, target: Option<usize>) -> usize {
        const N: [&str; 16] = [
            "jo", "jno", "jb", "jae", "je", "jne", "jbe", "ja", "js", "jns", "jp", "jnp", "jl",
            "jge", "jle", "jg",
        ];
        let at = self.pos();
        self.b(0x70 + cc.0);
        let field = self.pos();
        self.b(0);
        if let Some(t) = target {
            self.patch_rel8(field, t);
        }
        self.rec(at, N[cc.0 as usize], [Op::Rel8, Op::No, Op::No]);
        field
    }

    /// `push r64` (`50+r`).
    pub fn push(&mut self, r: Gpr) {
        let at = self.pos();
        if r.0 >= 8 {
            self.b(0x41);
        }
        self.b(0x50 + (r.0 & 7) as u8);
        self.rec(at, "push", [Op::G(r, Sz::Q), Op::No, Op::No]);
    }

    /// `pop r64` (`58+r`).
    pub fn pop(&mut self, r: Gpr) {
        let at = self.pos();
        if r.0 >= 8 {
            self.b(0x41);
        }
        self.b(0x58 + (r.0 & 7) as u8);
        self.rec(at, "pop", [Op::G(r, Sz::Q), Op::No, Op::No]);
    }

    pub fn push_rbp(&mut self) {
        let at = self.pos();
        self.b(0x55);
        self.rec(at, "push", [Op::G(RBP, Sz::Q), Op::No, Op::No]);
    }

    pub fn pop_rbp(&mut self) {
        let at = self.pos();
        self.b(0x5d);
        self.rec(at, "pop", [Op::G(RBP, Sz::Q), Op::No, Op::No]);
    }

    pub fn ret(&mut self) {
        let at = self.pos();
        self.b(0xc3);
        self.rec(at, "ret", [Op::No; 3]);
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
