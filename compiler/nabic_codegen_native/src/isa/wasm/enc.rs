//! wasm32 instruction encoder: one function per opcode or immediate form
//! the emitter uses (the `0xfc`, `0xfd` SIMD and `0xfe` atomic prefixes
//! included), the only place that knows opcodes and immediates, and the
//! LEB128 they are written in. A relocated immediate (a call's function, a
//! global, a data address, `call_indirect`'s type and table) is 5 bytes
//! with a `Reloc` the object writer fills or keeps.
//!
//! With `Asm::text` on (`--emit=asm`) each function also records its
//! instruction; `list` prints the object's final bytes (indices filled in)
//! as `llvm-objdump -d` does: its mnemonics (spaces included), hex floats,
//! memory offsets with a `p2align` that is not the natural one, and the
//! control-flow comments of its printer, whose label count and stack of
//! `block` / `loop` labels run on over the whole code section (an `end`
//! pops nothing, an `if` pushes nothing). Off, nothing is recorded.

#[cfg(test)]
mod tests;

use crate::isa::wasm::{Rel, Reloc, Ty};
use crate::lir::SymId;
use nabic_core::leb::{put_sleb, put_uleb, read_sleb, read_uleb, sleb, uleb};
use std::fmt::Write;

/// `v` in 5 bytes, as relocated operands are.
pub fn uleb5(out: &mut Vec<u8>, v: u32) {
    let at = out.len();
    out.resize(at + 5, 0);
    put_uleb(&mut out[at..], u64::from(v));
}

pub fn sleb5(out: &mut Vec<u8>, v: i32) {
    let at = out.len();
    out.resize(at + 5, 0);
    put_sleb(&mut out[at..], i64::from(v));
}

/// A LEB128 at `c[*i..]`, `*i` past it; 0 when cut short.
fn leb(c: &[u8], i: &mut usize, signed: bool) -> i64 {
    if signed {
        read_sleb(c, i).unwrap_or(0)
    } else {
        read_uleb(c, i).unwrap_or(0) as i64
    }
}

/// The numeric instructions without immediates (`0x45`..`0xc4`), the
/// whole range though the emitter leaves some out.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
#[allow(dead_code)]
pub enum Num {
    I32Eqz = 0x45,
    I32Eq = 0x46,
    I32Ne = 0x47,
    I32LtS = 0x48,
    I32LtU = 0x49,
    I32GtS = 0x4a,
    I32GtU = 0x4b,
    I32LeS = 0x4c,
    I32LeU = 0x4d,
    I32GeS = 0x4e,
    I32GeU = 0x4f,
    I64Eqz = 0x50,
    I64Eq = 0x51,
    I64Ne = 0x52,
    I64LtS = 0x53,
    I64LtU = 0x54,
    I64GtS = 0x55,
    I64GtU = 0x56,
    I64LeS = 0x57,
    I64LeU = 0x58,
    I64GeS = 0x59,
    I64GeU = 0x5a,
    F32Eq = 0x5b,
    F32Ne = 0x5c,
    F32Lt = 0x5d,
    F32Gt = 0x5e,
    F32Le = 0x5f,
    F32Ge = 0x60,
    F64Eq = 0x61,
    F64Ne = 0x62,
    F64Lt = 0x63,
    F64Gt = 0x64,
    F64Le = 0x65,
    F64Ge = 0x66,
    I32Clz = 0x67,
    I32Ctz = 0x68,
    I32Popcnt = 0x69,
    I32Add = 0x6a,
    I32Sub = 0x6b,
    I32Mul = 0x6c,
    I32DivS = 0x6d,
    I32DivU = 0x6e,
    I32RemS = 0x6f,
    I32RemU = 0x70,
    I32And = 0x71,
    I32Or = 0x72,
    I32Xor = 0x73,
    I32Shl = 0x74,
    I32ShrS = 0x75,
    I32ShrU = 0x76,
    I32Rotl = 0x77,
    I32Rotr = 0x78,
    I64Clz = 0x79,
    I64Ctz = 0x7a,
    I64Popcnt = 0x7b,
    I64Add = 0x7c,
    I64Sub = 0x7d,
    I64Mul = 0x7e,
    I64DivS = 0x7f,
    I64DivU = 0x80,
    I64RemS = 0x81,
    I64RemU = 0x82,
    I64And = 0x83,
    I64Or = 0x84,
    I64Xor = 0x85,
    I64Shl = 0x86,
    I64ShrS = 0x87,
    I64ShrU = 0x88,
    I64Rotl = 0x89,
    I64Rotr = 0x8a,
    F32Abs = 0x8b,
    F32Neg = 0x8c,
    F32Ceil = 0x8d,
    F32Floor = 0x8e,
    F32Trunc = 0x8f,
    F32Nearest = 0x90,
    F32Sqrt = 0x91,
    F32Add = 0x92,
    F32Sub = 0x93,
    F32Mul = 0x94,
    F32Div = 0x95,
    F32Min = 0x96,
    F32Max = 0x97,
    F32Copysign = 0x98,
    F64Abs = 0x99,
    F64Neg = 0x9a,
    F64Ceil = 0x9b,
    F64Floor = 0x9c,
    F64Trunc = 0x9d,
    F64Nearest = 0x9e,
    F64Sqrt = 0x9f,
    F64Add = 0xa0,
    F64Sub = 0xa1,
    F64Mul = 0xa2,
    F64Div = 0xa3,
    F64Min = 0xa4,
    F64Max = 0xa5,
    F64Copysign = 0xa6,
    I32WrapI64 = 0xa7,
    I32TruncF32S = 0xa8,
    I32TruncF32U = 0xa9,
    I32TruncF64S = 0xaa,
    I32TruncF64U = 0xab,
    I64ExtendI32S = 0xac,
    I64ExtendI32U = 0xad,
    I64TruncF32S = 0xae,
    I64TruncF32U = 0xaf,
    I64TruncF64S = 0xb0,
    I64TruncF64U = 0xb1,
    F32ConvertI32S = 0xb2,
    F32ConvertI32U = 0xb3,
    F32ConvertI64S = 0xb4,
    F32ConvertI64U = 0xb5,
    F32DemoteF64 = 0xb6,
    F64ConvertI32S = 0xb7,
    F64ConvertI32U = 0xb8,
    F64ConvertI64S = 0xb9,
    F64ConvertI64U = 0xba,
    F64PromoteF32 = 0xbb,
    I32ReinterpretF32 = 0xbc,
    I64ReinterpretF64 = 0xbd,
    F32ReinterpretI32 = 0xbe,
    F64ReinterpretI64 = 0xbf,
    I32Extend8S = 0xc0,
    I32Extend16S = 0xc1,
    I64Extend8S = 0xc2,
    I64Extend16S = 0xc3,
    I64Extend32S = 0xc4,
}
/// `Num` as objdump spells it.
const NUM: [&str; 128] = [
    "i32.eqz",
    "i32.eq  ",
    "i32.ne  ",
    "i32.lt_s",
    "i32.lt_u",
    "i32.gt_s",
    "i32.gt_u",
    "i32.le_s",
    "i32.le_u",
    "i32.ge_s",
    "i32.ge_u",
    "i64.eqz",
    "i64.eq  ",
    "i64.ne  ",
    "i64.lt_s",
    "i64.lt_u",
    "i64.gt_s",
    "i64.gt_u",
    "i64.le_s",
    "i64.le_u",
    "i64.ge_s",
    "i64.ge_u",
    "f32.eq  ",
    "f32.ne  ",
    "f32.lt  ",
    "f32.gt  ",
    "f32.le  ",
    "f32.ge  ",
    "f64.eq  ",
    "f64.ne  ",
    "f64.lt  ",
    "f64.gt  ",
    "f64.le  ",
    "f64.ge  ",
    "i32.clz ",
    "i32.ctz ",
    "i32.popcnt",
    "i32.add ",
    "i32.sub ",
    "i32.mul ",
    "i32.div_s",
    "i32.div_u",
    "i32.rem_s",
    "i32.rem_u",
    "i32.and ",
    "i32.or  ",
    "i32.xor ",
    "i32.shl ",
    "i32.shr_s",
    "i32.shr_u",
    "i32.rotl",
    "i32.rotr",
    "i64.clz ",
    "i64.ctz ",
    "i64.popcnt",
    "i64.add ",
    "i64.sub ",
    "i64.mul ",
    "i64.div_s",
    "i64.div_u",
    "i64.rem_s",
    "i64.rem_u",
    "i64.and ",
    "i64.or  ",
    "i64.xor ",
    "i64.shl ",
    "i64.shr_s",
    "i64.shr_u",
    "i64.rotl",
    "i64.rotr",
    "f32.abs ",
    "f32.neg ",
    "f32.ceil",
    "f32.floor",
    "f32.trunc",
    "f32.nearest",
    "f32.sqrt",
    "f32.add ",
    "f32.sub ",
    "f32.mul ",
    "f32.div ",
    "f32.min ",
    "f32.max ",
    "f32.copysign",
    "f64.abs ",
    "f64.neg ",
    "f64.ceil",
    "f64.floor",
    "f64.trunc",
    "f64.nearest",
    "f64.sqrt",
    "f64.add ",
    "f64.sub ",
    "f64.mul ",
    "f64.div ",
    "f64.min ",
    "f64.max ",
    "f64.copysign",
    "i32.wrap_i64",
    "i32.trunc_f32_s",
    "i32.trunc_f32_u",
    "i32.trunc_f64_s",
    "i32.trunc_f64_u",
    "i64.extend_i32_s",
    "i64.extend_i32_u",
    "i64.trunc_f32_s",
    "i64.trunc_f32_u",
    "i64.trunc_f64_s",
    "i64.trunc_f64_u",
    "f32.convert_i32_s",
    "f32.convert_i32_u",
    "f32.convert_i64_s",
    "f32.convert_i64_u",
    "f32.demote_f64",
    "f64.convert_i32_s",
    "f64.convert_i32_u",
    "f64.convert_i64_s",
    "f64.convert_i64_u",
    "f64.promote_f32",
    "i32.reinterpret_f32",
    "i64.reinterpret_f64",
    "f32.reinterpret_i32",
    "f64.reinterpret_i64",
    "i32.extend8_s",
    "i32.extend16_s",
    "i64.extend8_s",
    "i64.extend16_s",
    "i64.extend32_s",
];

/// Plain loads and stores: the opcode, the natural `p2align`, the name.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MemOp {
    I32Load,
    I64Load,
    F32Load,
    F64Load,
    I32Load8U,
    I32Load16U,
    I32Store,
    I64Store,
    F32Store,
    F64Store,
    I32Store8,
    I32Store16,
}

impl MemOp {
    const fn enc(self) -> (u8, u8, &'static str) {
        match self {
            Self::I32Load => (0x28, 2, "i32.load"),
            Self::I64Load => (0x29, 3, "i64.load"),
            Self::F32Load => (0x2a, 2, "f32.load"),
            Self::F64Load => (0x2b, 3, "f64.load"),
            Self::I32Load8U => (0x2d, 0, "i32.load8_u"),
            Self::I32Load16U => (0x2f, 1, "i32.load16_u"),
            Self::I32Store => (0x36, 2, "i32.store"),
            Self::I64Store => (0x37, 3, "i64.store"),
            Self::F32Store => (0x38, 2, "f32.store"),
            Self::F64Store => (0x39, 3, "f64.store"),
            Self::I32Store8 => (0x3a, 0, "i32.store8"),
            Self::I32Store16 => (0x3b, 1, "i32.store16"),
        }
    }
}

/// The saturating float -> integer conversions (`0xfc 0..7`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Sat {
    I32F32S,
    I32F32U,
    I32F64S,
    I32F64U,
    I64F32S,
    I64F32U,
    I64F64S,
    I64F64U,
}

impl Sat {
    const fn name(self) -> &'static str {
        match self {
            Self::I32F32S => "i32.trunc_sat_f32_s",
            Self::I32F32U => "i32.trunc_sat_f32_u",
            Self::I32F64S => "i32.trunc_sat_f64_s",
            Self::I32F64U => "i32.trunc_sat_f64_u",
            Self::I64F32S => "i64.trunc_sat_f32_s",
            Self::I64F32U => "i64.trunc_sat_f32_u",
            Self::I64F64S => "i64.trunc_sat_f64_s",
            Self::I64F64U => "i64.trunc_sat_f64_u",
        }
    }
}

/// A threads-proposal atomic access (`0xfe`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Atomic {
    Load,
    Store,
    Add,
    Sub,
    And,
    Or,
    Xor,
    Xchg,
    Cmpxchg,
}

/// An atomic access's width: an `i32` / `i64` one, or an `i32` one of
/// the low byte / halfword.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Width {
    W32,
    W64,
    W8,
    W16,
}

impl Atomic {
    /// The opcode, the natural `p2align` and the name of `self` at `w`.
    const fn enc(self, w: Width) -> (u8, u8, &'static str) {
        const NAMES: [[&str; 9]; 4] = [
            [
                "i32.atomic.load",
                "i32.atomic.store",
                "i32.atomic.rmw.add",
                "i32.atomic.rmw.sub",
                "i32.atomic.rmw.and",
                "i32.atomic.rmw.or",
                "i32.atomic.rmw.xor",
                "i32.atomic.rmw.xchg",
                "i32.atomic.rmw.cmpxchg",
            ],
            [
                "i64.atomic.load",
                "i64.atomic.store",
                "i64.atomic.rmw.add",
                "i64.atomic.rmw.sub",
                "i64.atomic.rmw.and",
                "i64.atomic.rmw.or",
                "i64.atomic.rmw.xor",
                "i64.atomic.rmw.xchg",
                "i64.atomic.rmw.cmpxchg",
            ],
            [
                "i32.atomic.load8_u",
                "i32.atomic.store8",
                "i32.atomic.rmw8.add_u",
                "i32.atomic.rmw8.sub_u",
                "i32.atomic.rmw8.and_u",
                "i32.atomic.rmw8.or_u",
                "i32.atomic.rmw8.xor_u",
                "i32.atomic.rmw8.xchg_u",
                "i32.atomic.rmw8.cmpxchg_u",
            ],
            [
                "i32.atomic.load16_u",
                "i32.atomic.store16",
                "i32.atomic.rmw16.add_u",
                "i32.atomic.rmw16.sub_u",
                "i32.atomic.rmw16.and_u",
                "i32.atomic.rmw16.or_u",
                "i32.atomic.rmw16.xor_u",
                "i32.atomic.rmw16.xchg_u",
                "i32.atomic.rmw16.cmpxchg_u",
            ],
        ];
        let base = match self {
            Self::Load => 0x10,
            Self::Store => 0x17,
            Self::Add => 0x1e,
            Self::Sub => 0x25,
            Self::And => 0x2c,
            Self::Or => 0x33,
            Self::Xor => 0x3a,
            Self::Xchg => 0x41,
            Self::Cmpxchg => 0x48,
        };
        let (d, align, row) = match w {
            Width::W32 => (0, 2, 0),
            Width::W64 => (1, 3, 1),
            Width::W8 => (2, 0, 2),
            Width::W16 => (3, 1, 3),
        };
        (base + d, align, NAMES[row][self as usize])
    }
}

/// SIMD128 instructions without immediates (`0xfd` and a sub-opcode).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Simd {
    I8x16Splat,
    I16x8Splat,
    I32x4Splat,
    I64x2Splat,
    I8x16Eq,
    I16x8Eq,
    I32x4Eq,
    I64x2Eq,
    I8x16GtS,
    I16x8GtS,
    I32x4GtS,
    I64x2GtS,
    I8x16GtU,
    I16x8GtU,
    I32x4GtU,
    I8x16MinS,
    I8x16MinU,
    I8x16MaxS,
    I8x16MaxU,
    I16x8MinS,
    I16x8MinU,
    I16x8MaxS,
    I16x8MaxU,
    I32x4MinS,
    I32x4MinU,
    I32x4MaxS,
    I32x4MaxU,
    I8x16Add,
    I16x8Add,
    I32x4Add,
    I64x2Add,
    I8x16Sub,
    I16x8Sub,
    I32x4Sub,
    I64x2Sub,
    I16x8Mul,
    I32x4Mul,
    I64x2Mul,
    V128And,
    V128Andnot,
    V128Or,
    V128Xor,
    F32x4Add,
    F32x4Sub,
    F32x4Mul,
    F32x4Div,
    F64x2Add,
    F64x2Sub,
    F64x2Mul,
    F64x2Div,
    F32x4Eq,
    F32x4Ne,
    F32x4Lt,
    F32x4Le,
    F64x2Eq,
    F64x2Ne,
    F64x2Lt,
    F64x2Le,
    F32x4Sqrt,
    F64x2Sqrt,
    I8x16Shl,
    I16x8Shl,
    I32x4Shl,
    I64x2Shl,
    I8x16ShrS,
    I16x8ShrS,
    I32x4ShrS,
    I64x2ShrS,
    I8x16ShrU,
    I16x8ShrU,
    I32x4ShrU,
    I64x2ShrU,
    I8x16Bitmask,
    I16x8Bitmask,
    I32x4Bitmask,
    I64x2Bitmask,
    I8x16NarrowI16x8U,
    I16x8NarrowI32x4U,
    I16x8ExtendLowI8x16S,
    I16x8ExtendLowI8x16U,
    I32x4ExtendLowI16x8S,
    I32x4ExtendLowI16x8U,
    I64x2ExtendLowI32x4S,
    I64x2ExtendLowI32x4U,
    F32x4Min,
    F32x4Max,
    F64x2Min,
    F64x2Max,
    I32x4ExtmulLowI16x8S,
    I32x4ExtmulHighI16x8S,
    I32x4ExtmulLowI16x8U,
    I32x4ExtmulHighI16x8U,
    I64x2ExtmulLowI32x4S,
    I64x2ExtmulHighI32x4S,
    I64x2ExtmulLowI32x4U,
    I64x2ExtmulHighI32x4U,
}

impl Simd {
    const fn enc(self) -> (u32, &'static str) {
        match self {
            Self::I8x16Splat => (0x0f, "i8x16.splat"),
            Self::I16x8Splat => (0x10, "i16x8.splat"),
            Self::I32x4Splat => (0x11, "i32x4.splat"),
            Self::I64x2Splat => (0x12, "i64x2.splat"),
            Self::I8x16Eq => (0x23, "i8x16.eq"),
            Self::I16x8Eq => (0x2d, "i16x8.eq"),
            Self::I32x4Eq => (0x37, "i32x4.eq"),
            Self::I64x2Eq => (0xd6, "i64x2.eq"),
            Self::I8x16GtS => (0x27, "i8x16.gt_s"),
            Self::I16x8GtS => (0x31, "i16x8.gt_s"),
            Self::I32x4GtS => (0x3b, "i32x4.gt_s"),
            Self::I64x2GtS => (0xd9, "i64x2.gt_s"),
            Self::I8x16GtU => (0x28, "i8x16.gt_u"),
            Self::I16x8GtU => (0x32, "i16x8.gt_u"),
            Self::I32x4GtU => (0x3c, "i32x4.gt_u"),
            Self::I8x16MinS => (0x76, "i8x16.min_s"),
            Self::I8x16MinU => (0x77, "i8x16.min_u"),
            Self::I8x16MaxS => (0x78, "i8x16.max_s"),
            Self::I8x16MaxU => (0x79, "i8x16.max_u"),
            Self::I16x8MinS => (0x96, "i16x8.min_s"),
            Self::I16x8MinU => (0x97, "i16x8.min_u"),
            Self::I16x8MaxS => (0x98, "i16x8.max_s"),
            Self::I16x8MaxU => (0x99, "i16x8.max_u"),
            Self::I32x4MinS => (0xb6, "i32x4.min_s"),
            Self::I32x4MinU => (0xb7, "i32x4.min_u"),
            Self::I32x4MaxS => (0xb8, "i32x4.max_s"),
            Self::I32x4MaxU => (0xb9, "i32x4.max_u"),
            Self::I8x16Add => (0x6e, "i8x16.add"),
            Self::I16x8Add => (0x8e, "i16x8.add"),
            Self::I32x4Add => (0xae, "i32x4.add"),
            Self::I64x2Add => (0xce, "i64x2.add"),
            Self::I8x16Sub => (0x71, "i8x16.sub"),
            Self::I16x8Sub => (0x91, "i16x8.sub"),
            Self::I32x4Sub => (0xb1, "i32x4.sub"),
            Self::I64x2Sub => (0xd1, "i64x2.sub"),
            Self::I16x8Mul => (0x95, "i16x8.mul"),
            Self::I32x4Mul => (0xb5, "i32x4.mul"),
            Self::I64x2Mul => (0xd5, "i64x2.mul"),
            Self::V128And => (0x4e, "v128.and"),
            Self::V128Andnot => (0x4f, "v128.andnot"),
            Self::V128Or => (0x50, "v128.or"),
            Self::V128Xor => (0x51, "v128.xor"),
            Self::F32x4Add => (0xe4, "f32x4.add"),
            Self::F32x4Sub => (0xe5, "f32x4.sub"),
            Self::F32x4Mul => (0xe6, "f32x4.mul"),
            Self::F32x4Div => (0xe7, "f32x4.div"),
            Self::F64x2Add => (0xf0, "f64x2.add"),
            Self::F64x2Sub => (0xf1, "f64x2.sub"),
            Self::F64x2Mul => (0xf2, "f64x2.mul"),
            Self::F64x2Div => (0xf3, "f64x2.div"),
            Self::F32x4Eq => (0x41, "f32x4.eq"),
            Self::F32x4Ne => (0x42, "f32x4.ne"),
            Self::F32x4Lt => (0x43, "f32x4.lt"),
            Self::F32x4Le => (0x45, "f32x4.le"),
            Self::F64x2Eq => (0x47, "f64x2.eq"),
            Self::F64x2Ne => (0x48, "f64x2.ne"),
            Self::F64x2Lt => (0x49, "f64x2.lt"),
            Self::F64x2Le => (0x4b, "f64x2.le"),
            Self::F32x4Sqrt => (0xe3, "f32x4.sqrt"),
            Self::F64x2Sqrt => (0xef, "f64x2.sqrt"),
            Self::I8x16Shl => (0x6b, "i8x16.shl"),
            Self::I16x8Shl => (0x8b, "i16x8.shl"),
            Self::I32x4Shl => (0xab, "i32x4.shl"),
            Self::I64x2Shl => (0xcb, "i64x2.shl"),
            Self::I8x16ShrS => (0x6c, "i8x16.shr_s"),
            Self::I16x8ShrS => (0x8c, "i16x8.shr_s"),
            Self::I32x4ShrS => (0xac, "i32x4.shr_s"),
            Self::I64x2ShrS => (0xcc, "i64x2.shr_s"),
            Self::I8x16ShrU => (0x6d, "i8x16.shr_u"),
            Self::I16x8ShrU => (0x8d, "i16x8.shr_u"),
            Self::I32x4ShrU => (0xad, "i32x4.shr_u"),
            Self::I64x2ShrU => (0xcd, "i64x2.shr_u"),
            Self::I8x16Bitmask => (0x64, "i8x16.bitmask"),
            Self::I16x8Bitmask => (0x84, "i16x8.bitmask"),
            Self::I32x4Bitmask => (0xa4, "i32x4.bitmask"),
            Self::I64x2Bitmask => (0xc4, "i64x2.bitmask"),
            Self::I8x16NarrowI16x8U => (0x66, "i8x16.narrow_i16x8_u"),
            Self::I16x8NarrowI32x4U => (0x86, "i16x8.narrow_i32x4_u"),
            Self::I16x8ExtendLowI8x16S => (0x87, "i16x8.extend_low_i8x16_s"),
            Self::I16x8ExtendLowI8x16U => (0x89, "i16x8.extend_low_i8x16_u"),
            Self::I32x4ExtendLowI16x8S => (0xa7, "i32x4.extend_low_i16x8_s"),
            Self::I32x4ExtendLowI16x8U => (0xa9, "i32x4.extend_low_i16x8_u"),
            Self::I64x2ExtendLowI32x4S => (0xc7, "i64x2.extend_low_i32x4_s"),
            Self::I64x2ExtendLowI32x4U => (0xc9, "i64x2.extend_low_i32x4_u"),
            Self::F32x4Min => (0xe8, "f32x4.min"),
            Self::F32x4Max => (0xe9, "f32x4.max"),
            Self::F64x2Min => (0xf4, "f64x2.min"),
            Self::F64x2Max => (0xf5, "f64x2.max"),
            Self::I32x4ExtmulLowI16x8S => (0xbc, "i32x4.extmul_low_i16x8_s"),
            Self::I32x4ExtmulHighI16x8S => (0xbd, "i32x4.extmul_high_i16x8_s"),
            Self::I32x4ExtmulLowI16x8U => (0xbe, "i32x4.extmul_low_i16x8_u"),
            Self::I32x4ExtmulHighI16x8U => (0xbf, "i32x4.extmul_high_i16x8_u"),
            Self::I64x2ExtmulLowI32x4S => (0xdc, "i64x2.extmul_low_i32x4_s"),
            Self::I64x2ExtmulHighI32x4S => (0xdd, "i64x2.extmul_high_i32x4_s"),
            Self::I64x2ExtmulLowI32x4U => (0xde, "i64x2.extmul_low_i32x4_u"),
            Self::I64x2ExtmulHighI32x4U => (0xdf, "i64x2.extmul_high_i32x4_u"),
        }
    }
}

/// SIMD128 loads with a memory operand.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VLoad {
    V128,
    I16x8Load8x8S,
    I16x8Load8x8U,
    I32x4Load16x4S,
    I32x4Load16x4U,
    I64x2Load32x2S,
    I64x2Load32x2U,
    Load32Zero,
}

impl VLoad {
    const fn enc(self) -> (u32, u8, &'static str) {
        match self {
            Self::V128 => (0x00, 4, "v128.load"),
            Self::I16x8Load8x8S => (0x01, 3, "i16x8.load8x8_s"),
            Self::I16x8Load8x8U => (0x02, 3, "i16x8.load8x8_u"),
            Self::I32x4Load16x4S => (0x03, 3, "i32x4.load16x4_s"),
            Self::I32x4Load16x4U => (0x04, 3, "i32x4.load16x4_u"),
            Self::I64x2Load32x2S => (0x05, 3, "i64x2.load32x2_s"),
            Self::I64x2Load32x2U => (0x06, 3, "i64x2.load32x2_u"),
            Self::Load32Zero => (0x5c, 2, "v128.load32_zero"),
        }
    }
}

/// How a recorded instruction's immediates print.
#[derive(Clone, Copy, Debug)]
enum Imm {
    None,
    /// The untyped `select`: its operands' type (source names it).
    Select(u8),
    /// An unsigned LEB: a local, global, function or lane index.
    Idx,
    /// A branch depth (`br`, `br_if`).
    Depth,
    I32,
    I64,
    F32,
    F64,
    /// A memory operand, its natural `p2align`.
    Mem(u8),
    /// An atomic's memory operand.
    Atomic(u8),
    /// A memory operand and a lane.
    Lane(u8),
    Block,
    Loop,
    If,
    BrTable,
    CallIndirect,
    V128,
    Shuffle,
    MemCopy,
    MemFill,
}

/// A recorded instruction.
#[derive(Clone, Copy, Debug)]
pub struct Line {
    at: u32,
    mn: &'static str,
    imm: Imm,
}

/// Value types.
pub const I32: u8 = 0x7f;
pub const I64: u8 = 0x7e;
pub const F32: u8 = 0x7d;
pub const F64: u8 = 0x7c;
pub const V128: u8 = 0x7b;
/// A block type: none, or one result.
pub const EMPTY: u8 = 0x40;

/// A function body's locals declaration: runs of one type.
#[must_use]
pub fn locals(types: &[u8]) -> Vec<u8> {
    let mut groups: Vec<(u32, u8)> = Vec::new();
    for &t in types {
        match groups.last_mut() {
            Some(g) if g.1 == t => g.0 += 1,
            _ => groups.push((1, t)),
        }
    }
    let mut out = Vec::new();
    uleb(&mut out, groups.len() as u64);
    for (n, t) in groups {
        uleb(&mut out, u64::from(n));
        out.push(t);
    }
    out
}

/// The code of a function in the making.
#[derive(Default)]
pub struct Asm {
    pub code: Vec<u8>,
    pub relocs: Vec<Reloc>,
    text: Option<Vec<Line>>,
}

impl Asm {
    /// `text`: record each instruction for `list`.
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

    /// The recorded instructions, their offsets moved by `shift` (the
    /// locals' declarations put before the code).
    #[must_use]
    pub fn take_text(&mut self, shift: u32) -> Option<Vec<Line>> {
        let mut t = self.text.take()?;
        for l in &mut t {
            l.at += shift;
        }
        Some(t)
    }

    #[inline]
    fn rec(&mut self, at: usize, mn: &'static str, imm: Imm) {
        if let Some(t) = &mut self.text {
            t.push(Line {
                at: at as u32,
                mn,
                imm,
            });
        }
    }

    fn b(&mut self, x: u8) {
        self.code.push(x);
    }

    fn uleb(&mut self, v: u64) {
        uleb(&mut self.code, v);
    }

    fn reloc(&mut self, rel: Rel) {
        let off = self.code.len() as u32;
        self.relocs.push(Reloc { off, rel });
    }

    fn simple(&mut self, op: u8, mn: &'static str) {
        let at = self.pos();
        self.b(op);
        self.rec(at, mn, Imm::None);
    }

    // Control.

    pub fn unreachable(&mut self) {
        self.simple(0x00, "unreachable");
    }

    /// `block` without a result.
    pub fn block(&mut self) {
        let at = self.pos();
        self.code.extend([0x02, EMPTY]);
        self.rec(at, "block   ", Imm::Block);
    }

    /// `loop` without a result.
    pub fn loop_(&mut self) {
        let at = self.pos();
        self.code.extend([0x03, EMPTY]);
        self.rec(at, "loop    ", Imm::Loop);
    }

    /// `if` of block type `ty` (`EMPTY` or a value type).
    pub fn if_(&mut self, ty: u8) {
        let at = self.pos();
        self.code.extend([0x04, ty]);
        self.rec(at, "if    ", Imm::If);
    }

    pub fn else_(&mut self) {
        self.simple(0x05, "else");
    }

    pub fn end(&mut self) {
        self.simple(0x0b, "end");
    }

    pub fn br(&mut self, depth: u32) {
        let at = self.pos();
        self.b(0x0c);
        self.uleb(u64::from(depth));
        self.rec(at, "br      ", Imm::Depth);
    }

    pub fn br_if(&mut self, depth: u32) {
        let at = self.pos();
        self.b(0x0d);
        self.uleb(u64::from(depth));
        self.rec(at, "br_if   ", Imm::Depth);
    }

    /// `br_table` of depths `targets`, then `default`.
    pub fn br_table(&mut self, targets: impl ExactSizeIterator<Item = u32>, default: u32) {
        let at = self.pos();
        self.b(0x0e);
        self.uleb(targets.len() as u64);
        for t in targets {
            self.uleb(u64::from(t));
        }
        self.uleb(u64::from(default));
        self.rec(at, "br_table ", Imm::BrTable);
    }

    pub fn return_(&mut self) {
        self.simple(0x0f, "return");
    }

    /// `call` of function `sym`.
    pub fn call(&mut self, sym: SymId) {
        let at = self.pos();
        self.b(0x10);
        self.reloc(Rel::Call(sym));
        uleb5(&mut self.code, 0);
        self.rec(at, "call", Imm::Idx);
    }

    /// `call_indirect` of type `ty` through table `table`.
    pub fn call_indirect(&mut self, ty: Ty, table: SymId) {
        let at = self.pos();
        self.b(0x11);
        self.reloc(Rel::Type(ty));
        uleb5(&mut self.code, 0);
        self.reloc(Rel::Table(table));
        uleb5(&mut self.code, 0);
        self.rec(at, "call_indirect", Imm::CallIndirect);
    }

    /// The untyped `select` of two `ty`s.
    pub fn select(&mut self, ty: u8) {
        let at = self.pos();
        self.b(0x1b);
        // objdump decodes the untyped form as its first match.
        self.rec(at, "f32.select", Imm::Select(ty));
    }

    // Variables.

    pub fn local_get(&mut self, l: u32) {
        self.idx(0x20, "local.get", l);
    }

    pub fn local_set(&mut self, l: u32) {
        self.idx(0x21, "local.set", l);
    }

    pub fn local_tee(&mut self, l: u32) {
        self.idx(0x22, "local.tee", l);
    }

    fn idx(&mut self, op: u8, mn: &'static str, i: u32) {
        let at = self.pos();
        self.b(op);
        self.uleb(u64::from(i));
        self.rec(at, mn, Imm::Idx);
    }

    /// `global.get` of global `sym`.
    pub fn global_get(&mut self, sym: SymId) {
        self.global(0x23, "global.get", sym);
    }

    pub fn global_set(&mut self, sym: SymId) {
        self.global(0x24, "global.set", sym);
    }

    fn global(&mut self, op: u8, mn: &'static str, sym: SymId) {
        let at = self.pos();
        self.b(op);
        self.reloc(Rel::Global(sym));
        uleb5(&mut self.code, 0);
        self.rec(at, mn, Imm::Idx);
    }

    // Memory.

    /// A load or store: `align` the `p2align`, `off` the offset.
    pub fn mem(&mut self, op: MemOp, align: u32, off: u32) {
        let (o, natural, mn) = op.enc();
        let at = self.pos();
        self.b(o);
        self.memarg(align, off);
        self.rec(at, mn, Imm::Mem(natural));
    }

    fn memarg(&mut self, align: u32, off: u32) {
        self.uleb(u64::from(align));
        self.uleb(u64::from(off));
    }

    /// `memory.copy` within memory 0.
    pub fn memory_copy(&mut self) {
        let at = self.pos();
        self.code.extend([0xfc, 10, 0, 0]);
        self.rec(at, "memory.copy", Imm::MemCopy);
    }

    /// `memory.fill` of memory 0.
    pub fn memory_fill(&mut self) {
        let at = self.pos();
        self.code.extend([0xfc, 11, 0]);
        self.rec(at, "memory.fill", Imm::MemFill);
    }

    // Constants.

    pub fn i32_const(&mut self, v: i32) {
        let at = self.pos();
        self.b(0x41);
        sleb(&mut self.code, i64::from(v));
        self.rec(at, "i32.const", Imm::I32);
    }

    /// `i32.const` of a relocated address (`Rel::Addr` / `Rel::Tls`).
    pub fn i32_const_rel(&mut self, rel: Rel) {
        let at = self.pos();
        self.b(0x41);
        self.reloc(rel);
        sleb5(&mut self.code, 0);
        self.rec(at, "i32.const", Imm::I32);
    }

    pub fn i64_const(&mut self, v: i64) {
        let at = self.pos();
        self.b(0x42);
        sleb(&mut self.code, v);
        self.rec(at, "i64.const", Imm::I64);
    }

    /// `f32.const` of the bits `v`.
    pub fn f32_const(&mut self, v: u32) {
        let at = self.pos();
        self.b(0x43);
        self.code.extend_from_slice(&v.to_le_bytes());
        self.rec(at, "f32.const", Imm::F32);
    }

    /// `f64.const` of the bits `v`.
    pub fn f64_const(&mut self, v: u64) {
        let at = self.pos();
        self.b(0x44);
        self.code.extend_from_slice(&v.to_le_bytes());
        self.rec(at, "f64.const", Imm::F64);
    }

    // Numeric.

    pub fn num(&mut self, op: Num) {
        let at = self.pos();
        self.b(op as u8);
        self.rec(at, NUM[op as usize - 0x45], Imm::None);
    }

    /// A saturating conversion.
    pub fn sat(&mut self, op: Sat) {
        let at = self.pos();
        self.b(0xfc);
        self.uleb(op as u64);
        self.rec(at, op.name(), Imm::None);
    }

    /// An atomic access at offset 0, aligned naturally.
    pub fn atomic(&mut self, op: Atomic, w: Width) {
        let (o, align, mn) = op.enc(w);
        let at = self.pos();
        self.b(0xfe);
        self.uleb(u64::from(o));
        self.memarg(u32::from(align), 0);
        self.rec(at, mn, Imm::Atomic(align));
    }

    // SIMD128.

    pub fn simd(&mut self, op: Simd) {
        let (o, mn) = op.enc();
        let at = self.pos();
        self.b(0xfd);
        self.uleb(u64::from(o));
        self.rec(at, mn, Imm::None);
    }

    /// A vector load: `align` the `p2align`, `off` the offset.
    pub fn vload(&mut self, op: VLoad, align: u32, off: u32) {
        let (o, natural, mn) = op.enc();
        let at = self.pos();
        self.b(0xfd);
        self.uleb(u64::from(o));
        self.memarg(align, off);
        self.rec(at, mn, Imm::Mem(natural));
    }

    /// `v128.store`.
    pub fn v128_store(&mut self, align: u32, off: u32) {
        let at = self.pos();
        self.code.extend([0xfd, 0x0b]);
        self.memarg(align, off);
        self.rec(at, "v128.store", Imm::Mem(4));
    }

    /// `v128.store64_lane` of lane `lane`.
    pub fn store64_lane(&mut self, align: u32, off: u32, lane: u8) {
        let at = self.pos();
        self.code.extend([0xfd, 0x5b]);
        self.memarg(align, off);
        self.b(lane);
        self.rec(at, "v128.store64_lane", Imm::Lane(3));
    }

    /// `v128.const` of `bytes`.
    pub fn v128_const(&mut self, bytes: &[u8; 16]) {
        let at = self.pos();
        self.code.extend([0xfd, 0x0c]);
        self.code.extend_from_slice(bytes);
        self.rec(at, "v128.const", Imm::V128);
    }

    /// `i8x16.shuffle` picking `lanes`.
    pub fn shuffle(&mut self, lanes: &[u8; 16]) {
        let at = self.pos();
        self.code.extend([0xfd, 0x0d]);
        self.code.extend_from_slice(lanes);
        self.rec(at, "i8x16.shuffle", Imm::Shuffle);
    }
}

/// llvm-objdump's control-flow state: the next label, and the label and
/// kind (a `loop`: branches go up) of every `block` / `loop` so far.
#[derive(Default)]
pub struct Printer {
    next: u32,
    stack: Vec<(u32, bool)>,
}

impl Printer {
    fn depth(&self, d: i64) -> String {
        match usize::try_from(d).ok().filter(|&d| d < self.stack.len()) {
            Some(d) => {
                let (l, up) = self.stack[self.stack.len() - 1 - d];
                format!("{d}: {} to label{l}", if up { "up" } else { "down" })
            }
            None => "Invalid depth argument!".to_owned(),
        }
    }
}

/// A function as `list` prints it.
pub struct Func<'a> {
    /// The symbol, as the object names it.
    pub name: &'a str,
    /// Each local's type, past the parameters.
    pub locals: &'a [u8],
    /// From the symbol: the body's size, the locals' declarations, the
    /// instructions, indices filled in.
    pub code: &'a [u8],
    /// Where `lines`' offsets count from in `code`.
    pub start: u32,
    pub lines: &'a [Line],
    /// `(offset in code, type, target)` of each relocation.
    pub relocs: &'a [(u32, &'static str, String)],
}

const fn ty_name(t: u8) -> &'static str {
    match t {
        I32 => "i32",
        I64 => "i64",
        F32 => "f32",
        F64 => "f64",
        _ => "v128",
    }
}

/// Append the listing of `f` to `out`.
pub fn list(out: &mut String, f: &Func<'_>, p: &mut Printer) {
    let _ = write!(out, "\n<{}>:\n", f.name);
    if !f.locals.is_empty() {
        let l: Vec<&str> = f.locals.iter().map(|&t| ty_name(t)).collect();
        let _ = write!(out, "        .local {}", l.join(", "));
    }
    out.push('\n');
    let mut r = 0;
    for (n, line) in f.lines.iter().enumerate() {
        let at = (f.start + line.at) as usize;
        let end = f
            .lines
            .get(n + 1)
            .map_or(f.code.len(), |x| (f.start + x.at) as usize);
        let code = &f.code[at..end];
        let (text, notes) = text(code, line, p);
        let b: Vec<String> = code.iter().map(|b| format!("{b:02x}")).collect();
        let b = crate::asm::short(&b.join(" "));
        let _ = write!(out, "{at:>8x}: {b}\t{text}");
        // objdump puts a `br_table`'s comments at column 56 (the rest on
        // lines of their own), the others' at 64.
        let to: usize = if matches!(line.imm, Imm::BrTable) {
            56
        } else {
            64
        };
        let mut col = 10 + b.len();
        for ch in format!("\t{text}").chars() {
            col = if ch == '\t' {
                (col / 8 + 1) * 8
            } else {
                col + 1
            };
        }
        for (k, c) in notes.iter().enumerate() {
            let pad = if k == 0 {
                to.saturating_sub(col).max(1)
            } else {
                56
            };
            if k > 0 {
                out.push('\n');
            }
            let _ = write!(out, "{}# {c}", " ".repeat(pad));
        }
        out.push('\n');
        while let Some((off, ty, target)) = f.relocs.get(r).filter(|x| (x.0 as usize) < end) {
            let _ = writeln!(out, "\t\t\t{off:08x}:  {ty}\t{target}+0");
            r += 1;
        }
    }
}

/// A function as `source` writes it.
pub struct SrcFunc<'a> {
    /// The body: the locals' declarations, the instructions (indices
    /// filled in).
    pub code: &'a [u8],
    pub lines: &'a [Line],
    /// `(offset in code, operand)` of each relocated field, in order:
    /// the expression that makes the assembler emit its relocation.
    pub relocs: &'a [(u32, String)],
}

/// Append the instructions of `f` to `out` as LLVM's wasm assembler
/// reads them: each `end` named by what it closes, the untyped `select`
/// by its operands' type, relocated operands by their symbols.
pub fn source(out: &mut String, f: &SrcFunc<'_>) {
    let mut open: Vec<&str> = Vec::new();
    let mut p = Printer::default();
    for (n, line) in f.lines.iter().enumerate() {
        let at = line.at as usize;
        let end = f.lines.get(n + 1).map_or(f.code.len(), |x| x.at as usize);
        let code = &f.code[at..end];
        let rel: Vec<&str> = f
            .relocs
            .iter()
            .filter(|r| (at..end).contains(&(r.0 as usize)))
            .map(|r| r.1.as_str())
            .collect();
        let mn = line.mn.trim_end();
        let text = match line.imm {
            Imm::Select(t) => format!("{}.select", ty_name(t)),
            Imm::None if mn == "end" => match open.pop() {
                Some(k) => format!("end_{k}"),
                None => "end_function".to_owned(),
            },
            Imm::Block | Imm::If | Imm::Loop => {
                open.push(match line.imm {
                    Imm::Block => "block",
                    Imm::If => "if",
                    _ => "loop",
                });
                match code.get(1) {
                    Some(&t) if t != EMPTY => format!("{mn}\t{}", ty_name(t)),
                    _ => mn.to_owned(),
                }
            }
            Imm::CallIndirect => match rel[..] {
                [ty, table] => format!("{mn}\t{table}, {ty}"),
                _ => text(code, line, &mut p).0,
            },
            Imm::Idx | Imm::I32 if !rel.is_empty() => format!("{mn}\t{}", rel[0]),
            Imm::Atomic(n) => {
                let mut i = 1;
                leb(code, &mut i, false);
                format!("{mn}\t{}", memarg(code, &mut i, n))
            }
            _ => {
                let t = text(code, line, &mut p).0;
                match t.split_once('\t') {
                    Some((_, ops)) => format!("{mn}\t{ops}"),
                    None => mn.to_owned(),
                }
            }
        };
        let _ = writeln!(out, "\t{text}");
    }
}

/// `bits` of an `f64` (an `f32` widened to one first, a NaN quieted) as
/// LLVM prints a float: C99 hex, shortest digits.
fn hexf(bits: u64) -> String {
    let s = if bits >> 63 != 0 { "-" } else { "" };
    let exp = ((bits >> 52) & 0x7ff) as i64;
    let frac = bits & ((1 << 52) - 1);
    if exp == 0x7ff {
        return match frac {
            0 => format!("{s}infinity"),
            f if f == 1 << 51 => format!("{s}nan"),
            f => format!("{s}nan:{f:#x}"),
        };
    }
    if exp == 0 && frac == 0 {
        return format!("{s}0x0p0");
    }
    let (lead, e) = if exp == 0 {
        (0, -1022)
    } else {
        (1, exp - 1023)
    };
    let d = format!("{frac:013x}");
    let d = d.trim_end_matches('0');
    if d.is_empty() {
        format!("{s}0x{lead}p{e}")
    } else {
        format!("{s}0x{lead}.{d}p{e}")
    }
}

/// `f32` bits as `f64` bits.
fn widen(b: u32) -> u64 {
    if (b >> 23) & 0xff == 0xff && b & 0x7f_ffff != 0 {
        let sign = u64::from(b >> 31) << 63;
        return sign | (0x7ff << 52) | (1 << 51) | (u64::from(b & 0x7f_ffff) << 29);
    }
    f64::from(f32::from_bits(b)).to_bits()
}

fn memarg(c: &[u8], i: &mut usize, natural: u8) -> String {
    let align = leb(c, i, false);
    let off = leb(c, i, false);
    if align == i64::from(natural) {
        format!("{off}")
    } else {
        format!("{off}:p2align={align}")
    }
}

/// The text of the instruction at `c` and objdump's comments on it.
fn text(c: &[u8], line: &Line, p: &mut Printer) -> (String, Vec<String>) {
    // Past the opcode (and a prefix's sub-opcode).
    let mut i = 1;
    if matches!(c[0], 0xfc..=0xfe) {
        leb(c, &mut i, false);
    }
    let mut notes = Vec::new();
    let ops = match line.imm {
        Imm::None | Imm::Select(_) => return (line.mn.to_owned(), notes),
        Imm::Idx => leb(c, &mut i, false).to_string(),
        Imm::Depth => {
            let d = leb(c, &mut i, false);
            notes.push(p.depth(d));
            d.to_string()
        }
        Imm::I32 => (leb(c, &mut i, true) as i32).to_string(),
        Imm::I64 => leb(c, &mut i, true).to_string(),
        Imm::F32 => hexf(widen(u32::from_le_bytes([c[1], c[2], c[3], c[4]]))),
        Imm::F64 => {
            let mut b = [0; 8];
            b.copy_from_slice(&c[1..9]);
            hexf(u64::from_le_bytes(b))
        }
        Imm::Mem(n) => memarg(c, &mut i, n),
        Imm::Atomic(n) => format!(" {}", memarg(c, &mut i, n)),
        Imm::Lane(n) => {
            let m = memarg(c, &mut i, n);
            format!("{m}, {}", c[i])
        }
        Imm::Block | Imm::If => {
            if line.imm_is_block() {
                p.stack.push((p.next, false));
                p.next += 1;
            }
            match c[1] {
                EMPTY => String::new(),
                t => ty_name(t).to_owned(),
            }
        }
        Imm::Loop => {
            notes.push(format!("label{}:", p.next));
            p.stack.push((p.next, true));
            p.next += 1;
            String::new()
        }
        Imm::BrTable => {
            let n = leb(c, &mut i, false);
            let ds: Vec<i64> = (0..=n).map(|_| leb(c, &mut i, false)).collect();
            let mut seen = Vec::new();
            for &d in &ds[1..] {
                if !seen.contains(&d) {
                    seen.push(d);
                    notes.push(p.depth(d));
                }
            }
            let ds: Vec<String> = ds.iter().map(ToString::to_string).collect();
            format!("{{{}}}", ds.join(", "))
        }
        Imm::CallIndirect => {
            let ty = leb(c, &mut i, false);
            match leb(c, &mut i, false) {
                0 => format!(" {ty}"),
                t => format!("{t} {ty}"),
            }
        }
        Imm::V128 => {
            let w: Vec<String> = c[2..18]
                .chunks(4)
                .map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]]).to_string())
                .collect();
            w.join(", ")
        }
        Imm::Shuffle => {
            let l: Vec<String> = c[2..18].iter().map(ToString::to_string).collect();
            l.join(", ")
        }
        Imm::MemCopy => "0, 0".to_owned(),
        Imm::MemFill => "0".to_owned(),
    };
    (format!("{}\t{ops}", line.mn), notes)
}

impl Line {
    const fn imm_is_block(&self) -> bool {
        matches!(self.imm, Imm::Block)
    }
}
