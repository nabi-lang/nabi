//! Each form against the reference: its bytes as the spec encodes them,
//! the text `llvm-objdump -d` prints for them (its comments aside), and
//! a whole object's listing as objdump prints it.

use crate::isa::wasm::enc::*;
use crate::isa::wasm::{Code, I32, I64, Parts};
use crate::obj::Obj;

type Case = (fn(&mut Asm), &'static [u8], &'static str);

#[rustfmt::skip]
fn cases() -> Vec<Case> {
    vec![
        (|a| a.unreachable(), &[0x00], "unreachable"),
        (|a| a.block(), &[0x02, 0x40], "block   \t"),
        (|a| a.loop_(), &[0x03, 0x40], "loop    \t"),
        (|a| a.if_(EMPTY), &[0x04, 0x40], "if    \t"),
        (|a| a.if_(0x7f), &[0x04, 0x7f], "if    \ti32"),
        (|a| a.else_(), &[0x05], "else"),
        (|a| a.end(), &[0x0b], "end"),
        (|a| a.br(0), &[0x0c, 0x00], "br      \t0"),
        (|a| a.br(200), &[0x0c, 0xc8, 0x01], "br      \t200"),
        (|a| a.br_if(1), &[0x0d, 0x01], "br_if   \t1"),
        (|a| a.br_table(0..3, 3), &[0x0e, 0x03, 0x00, 0x01, 0x02, 0x03], "br_table \t{0, 1, 2, 3}"),
        (|a| a.br_table(0..0, 0), &[0x0e, 0x00, 0x00], "br_table \t{0}"),
        (|a| a.return_(), &[0x0f], "return"),
        (|a| a.call(0), &[0x10, 0x80, 0x80, 0x80, 0x80, 0x00], "call\t0"),
        (|a| a.call_indirect((Vec::new(), Vec::new()), 0), &[0x11, 0x80, 0x80, 0x80, 0x80, 0x00, 0x80, 0x80, 0x80, 0x80, 0x00], "call_indirect\t 0"),
        (|a| a.select(0x7f), &[0x1b], "f32.select"),
        (|a| a.local_get(0), &[0x20, 0x00], "local.get\t0"),
        (|a| a.local_get(127), &[0x20, 0x7f], "local.get\t127"),
        (|a| a.local_get(128), &[0x20, 0x80, 0x01], "local.get\t128"),
        (|a| a.local_get(70000), &[0x20, 0xf0, 0xa2, 0x04], "local.get\t70000"),
        (|a| a.local_set(0), &[0x21, 0x00], "local.set\t0"),
        (|a| a.local_set(127), &[0x21, 0x7f], "local.set\t127"),
        (|a| a.local_set(128), &[0x21, 0x80, 0x01], "local.set\t128"),
        (|a| a.local_set(70000), &[0x21, 0xf0, 0xa2, 0x04], "local.set\t70000"),
        (|a| a.local_tee(0), &[0x22, 0x00], "local.tee\t0"),
        (|a| a.local_tee(127), &[0x22, 0x7f], "local.tee\t127"),
        (|a| a.local_tee(128), &[0x22, 0x80, 0x01], "local.tee\t128"),
        (|a| a.local_tee(70000), &[0x22, 0xf0, 0xa2, 0x04], "local.tee\t70000"),
        (|a| a.global_get(0), &[0x23, 0x80, 0x80, 0x80, 0x80, 0x00], "global.get\t0"),
        (|a| a.global_set(0), &[0x24, 0x80, 0x80, 0x80, 0x80, 0x00], "global.set\t0"),
        (|a| a.mem(MemOp::I32Load, 2, 16), &[0x28, 0x02, 0x10], "i32.load\t16"),
        (|a| a.mem(MemOp::I32Load, 0, 300), &[0x28, 0x00, 0xac, 0x02], "i32.load\t300:p2align=0"),
        (|a| a.mem(MemOp::I64Load, 3, 16), &[0x29, 0x03, 0x10], "i64.load\t16"),
        (|a| a.mem(MemOp::I64Load, 0, 300), &[0x29, 0x00, 0xac, 0x02], "i64.load\t300:p2align=0"),
        (|a| a.mem(MemOp::F32Load, 2, 16), &[0x2a, 0x02, 0x10], "f32.load\t16"),
        (|a| a.mem(MemOp::F32Load, 0, 300), &[0x2a, 0x00, 0xac, 0x02], "f32.load\t300:p2align=0"),
        (|a| a.mem(MemOp::F64Load, 3, 16), &[0x2b, 0x03, 0x10], "f64.load\t16"),
        (|a| a.mem(MemOp::F64Load, 0, 300), &[0x2b, 0x00, 0xac, 0x02], "f64.load\t300:p2align=0"),
        (|a| a.mem(MemOp::I32Load8U, 0, 16), &[0x2d, 0x00, 0x10], "i32.load8_u\t16"),
        (|a| a.mem(MemOp::I32Load8U, 0, 300), &[0x2d, 0x00, 0xac, 0x02], "i32.load8_u\t300"),
        (|a| a.mem(MemOp::I32Load16U, 1, 16), &[0x2f, 0x01, 0x10], "i32.load16_u\t16"),
        (|a| a.mem(MemOp::I32Load16U, 0, 300), &[0x2f, 0x00, 0xac, 0x02], "i32.load16_u\t300:p2align=0"),
        (|a| a.mem(MemOp::I32Store, 2, 16), &[0x36, 0x02, 0x10], "i32.store\t16"),
        (|a| a.mem(MemOp::I32Store, 0, 300), &[0x36, 0x00, 0xac, 0x02], "i32.store\t300:p2align=0"),
        (|a| a.mem(MemOp::I64Store, 3, 16), &[0x37, 0x03, 0x10], "i64.store\t16"),
        (|a| a.mem(MemOp::I64Store, 0, 300), &[0x37, 0x00, 0xac, 0x02], "i64.store\t300:p2align=0"),
        (|a| a.mem(MemOp::F32Store, 2, 16), &[0x38, 0x02, 0x10], "f32.store\t16"),
        (|a| a.mem(MemOp::F32Store, 0, 300), &[0x38, 0x00, 0xac, 0x02], "f32.store\t300:p2align=0"),
        (|a| a.mem(MemOp::F64Store, 3, 16), &[0x39, 0x03, 0x10], "f64.store\t16"),
        (|a| a.mem(MemOp::F64Store, 0, 300), &[0x39, 0x00, 0xac, 0x02], "f64.store\t300:p2align=0"),
        (|a| a.mem(MemOp::I32Store8, 0, 16), &[0x3a, 0x00, 0x10], "i32.store8\t16"),
        (|a| a.mem(MemOp::I32Store8, 0, 300), &[0x3a, 0x00, 0xac, 0x02], "i32.store8\t300"),
        (|a| a.mem(MemOp::I32Store16, 1, 16), &[0x3b, 0x01, 0x10], "i32.store16\t16"),
        (|a| a.mem(MemOp::I32Store16, 0, 300), &[0x3b, 0x00, 0xac, 0x02], "i32.store16\t300:p2align=0"),
        (|a| a.memory_copy(), &[0xfc, 0x0a, 0x00, 0x00], "memory.copy\t0, 0"),
        (|a| a.memory_fill(), &[0xfc, 0x0b, 0x00], "memory.fill\t0"),
        (|a| a.i32_const(0), &[0x41, 0x00], "i32.const\t0"),
        (|a| a.i32_const(-1), &[0x41, 0x7f], "i32.const\t-1"),
        (|a| a.i32_const(63), &[0x41, 0x3f], "i32.const\t63"),
        (|a| a.i32_const(64), &[0x41, 0xc0, 0x00], "i32.const\t64"),
        (|a| a.i32_const(-64), &[0x41, 0x40], "i32.const\t-64"),
        (|a| a.i32_const(-65), &[0x41, 0xbf, 0x7f], "i32.const\t-65"),
        (|a| a.i32_const(2147483647), &[0x41, 0xff, 0xff, 0xff, 0xff, 0x07], "i32.const\t2147483647"),
        (|a| a.i32_const(-2147483648), &[0x41, 0x80, 0x80, 0x80, 0x80, 0x78], "i32.const\t-2147483648"),
        (|a| a.i32_const(255), &[0x41, 0xff, 0x01], "i32.const\t255"),
        (|a| a.i32_const_rel(Rel::Addr(0)), &[0x41, 0x80, 0x80, 0x80, 0x80, 0x00], "i32.const\t0"),
        (|a| a.i64_const(0_i64), &[0x42, 0x00], "i64.const\t0"),
        (|a| a.i64_const(-1_i64), &[0x42, 0x7f], "i64.const\t-1"),
        (|a| a.i64_const(1099511627776_i64), &[0x42, 0x80, 0x80, 0x80, 0x80, 0x80, 0x20], "i64.const\t1099511627776"),
        (|a| a.i64_const(i64::MIN), &[0x42, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x7f], "i64.const\t-9223372036854775808"),
        (|a| a.i64_const(9223372036854775807_i64), &[0x42, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00], "i64.const\t9223372036854775807"),
        (|a| a.f32_const(0x0), &[0x43, 0x00, 0x00, 0x00, 0x00], "f32.const\t0x0p0"),
        (|a| a.f32_const(0x80000000), &[0x43, 0x00, 0x00, 0x00, 0x80], "f32.const\t-0x0p0"),
        (|a| a.f32_const(0x3f800000), &[0x43, 0x00, 0x00, 0x80, 0x3f], "f32.const\t0x1p0"),
        (|a| a.f32_const(0x3fc00000), &[0x43, 0x00, 0x00, 0xc0, 0x3f], "f32.const\t0x1.8p0"),
        (|a| a.f32_const(0x3dcccccd), &[0x43, 0xcd, 0xcc, 0xcc, 0x3d], "f32.const\t0x1.99999ap-4"),
        (|a| a.f32_const(0xc0200000), &[0x43, 0x00, 0x00, 0x20, 0xc0], "f32.const\t-0x1.4p1"),
        (|a| a.f32_const(0x7f800000), &[0x43, 0x00, 0x00, 0x80, 0x7f], "f32.const\tinfinity"),
        (|a| a.f32_const(0xff800000), &[0x43, 0x00, 0x00, 0x80, 0xff], "f32.const\t-infinity"),
        (|a| a.f32_const(0x7fc00000), &[0x43, 0x00, 0x00, 0xc0, 0x7f], "f32.const\tnan"),
        (|a| a.f32_const(0xffc00000), &[0x43, 0x00, 0x00, 0xc0, 0xff], "f32.const\t-nan"),
        (|a| a.f32_const(0x7f800001), &[0x43, 0x01, 0x00, 0x80, 0x7f], "f32.const\tnan:0x8000020000000"),
        (|a| a.f32_const(0x7fa00000), &[0x43, 0x00, 0x00, 0xa0, 0x7f], "f32.const\tnan:0xc000000000000"),
        (|a| a.f32_const(0x1), &[0x43, 0x01, 0x00, 0x00, 0x00], "f32.const\t0x1p-149"),
        (|a| a.f32_const(0x400000), &[0x43, 0x00, 0x00, 0x40, 0x00], "f32.const\t0x1p-127"),
        (|a| a.f32_const(0x80000003), &[0x43, 0x03, 0x00, 0x00, 0x80], "f32.const\t-0x1.8p-148"),
        (|a| a.f32_const(0x7f7fffff), &[0x43, 0xff, 0xff, 0x7f, 0x7f], "f32.const\t0x1.fffffep127"),
        (|a| a.f64_const(0x0), &[0x44, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00], "f64.const\t0x0p0"),
        (|a| a.f64_const(0x8000000000000000), &[0x44, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x80], "f64.const\t-0x0p0"),
        (|a| a.f64_const(0x3ff0000000000000), &[0x44, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xf0, 0x3f], "f64.const\t0x1p0"),
        (|a| a.f64_const(0x3fb999999999999a), &[0x44, 0x9a, 0x99, 0x99, 0x99, 0x99, 0x99, 0xb9, 0x3f], "f64.const\t0x1.999999999999ap-4"),
        (|a| a.f64_const(0xfe37e43c8800759c), &[0x44, 0x9c, 0x75, 0x00, 0x88, 0x3c, 0xe4, 0x37, 0xfe], "f64.const\t-0x1.7e43c8800759cp996"),
        (|a| a.f64_const(0x10000000000000), &[0x44, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00], "f64.const\t0x1p-1022"),
        (|a| a.f64_const(0x7fefffffffffffff), &[0x44, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xef, 0x7f], "f64.const\t0x1.fffffffffffffp1023"),
        (|a| a.f64_const(0x7ff8000000000000), &[0x44, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xf8, 0x7f], "f64.const\tnan"),
        (|a| a.f64_const(0xfff8000000000000), &[0x44, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xf8, 0xff], "f64.const\t-nan"),
        (|a| a.f64_const(0x7ff0000000000001), &[0x44, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0xf0, 0x7f], "f64.const\tnan:0x1"),
        (|a| a.f64_const(0xfff0000000000001), &[0x44, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0xf0, 0xff], "f64.const\t-nan:0x1"),
        (|a| a.f64_const(0x1), &[0x44, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00], "f64.const\t0x0.0000000000001p-1022"),
        (|a| a.f64_const(0x8000000000000), &[0x44, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x08, 0x00], "f64.const\t0x0.8p-1022"),
        (|a| a.f64_const(0x7ff0000000000000), &[0x44, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xf0, 0x7f], "f64.const\tinfinity"),
        (|a| a.num(Num::I32Eqz), &[0x45], "i32.eqz"),
        (|a| a.num(Num::I32Eq), &[0x46], "i32.eq  "),
        (|a| a.num(Num::I32Ne), &[0x47], "i32.ne  "),
        (|a| a.num(Num::I32LtS), &[0x48], "i32.lt_s"),
        (|a| a.num(Num::I32LtU), &[0x49], "i32.lt_u"),
        (|a| a.num(Num::I32GtS), &[0x4a], "i32.gt_s"),
        (|a| a.num(Num::I32GtU), &[0x4b], "i32.gt_u"),
        (|a| a.num(Num::I32LeS), &[0x4c], "i32.le_s"),
        (|a| a.num(Num::I32LeU), &[0x4d], "i32.le_u"),
        (|a| a.num(Num::I32GeS), &[0x4e], "i32.ge_s"),
        (|a| a.num(Num::I32GeU), &[0x4f], "i32.ge_u"),
        (|a| a.num(Num::I64Eqz), &[0x50], "i64.eqz"),
        (|a| a.num(Num::I64Eq), &[0x51], "i64.eq  "),
        (|a| a.num(Num::I64Ne), &[0x52], "i64.ne  "),
        (|a| a.num(Num::I64LtS), &[0x53], "i64.lt_s"),
        (|a| a.num(Num::I64LtU), &[0x54], "i64.lt_u"),
        (|a| a.num(Num::I64GtS), &[0x55], "i64.gt_s"),
        (|a| a.num(Num::I64GtU), &[0x56], "i64.gt_u"),
        (|a| a.num(Num::I64LeS), &[0x57], "i64.le_s"),
        (|a| a.num(Num::I64LeU), &[0x58], "i64.le_u"),
        (|a| a.num(Num::I64GeS), &[0x59], "i64.ge_s"),
        (|a| a.num(Num::I64GeU), &[0x5a], "i64.ge_u"),
        (|a| a.num(Num::F32Eq), &[0x5b], "f32.eq  "),
        (|a| a.num(Num::F32Ne), &[0x5c], "f32.ne  "),
        (|a| a.num(Num::F32Lt), &[0x5d], "f32.lt  "),
        (|a| a.num(Num::F32Gt), &[0x5e], "f32.gt  "),
        (|a| a.num(Num::F32Le), &[0x5f], "f32.le  "),
        (|a| a.num(Num::F32Ge), &[0x60], "f32.ge  "),
        (|a| a.num(Num::F64Eq), &[0x61], "f64.eq  "),
        (|a| a.num(Num::F64Ne), &[0x62], "f64.ne  "),
        (|a| a.num(Num::F64Lt), &[0x63], "f64.lt  "),
        (|a| a.num(Num::F64Gt), &[0x64], "f64.gt  "),
        (|a| a.num(Num::F64Le), &[0x65], "f64.le  "),
        (|a| a.num(Num::F64Ge), &[0x66], "f64.ge  "),
        (|a| a.num(Num::I32Clz), &[0x67], "i32.clz "),
        (|a| a.num(Num::I32Ctz), &[0x68], "i32.ctz "),
        (|a| a.num(Num::I32Popcnt), &[0x69], "i32.popcnt"),
        (|a| a.num(Num::I32Add), &[0x6a], "i32.add "),
        (|a| a.num(Num::I32Sub), &[0x6b], "i32.sub "),
        (|a| a.num(Num::I32Mul), &[0x6c], "i32.mul "),
        (|a| a.num(Num::I32DivS), &[0x6d], "i32.div_s"),
        (|a| a.num(Num::I32DivU), &[0x6e], "i32.div_u"),
        (|a| a.num(Num::I32RemS), &[0x6f], "i32.rem_s"),
        (|a| a.num(Num::I32RemU), &[0x70], "i32.rem_u"),
        (|a| a.num(Num::I32And), &[0x71], "i32.and "),
        (|a| a.num(Num::I32Or), &[0x72], "i32.or  "),
        (|a| a.num(Num::I32Xor), &[0x73], "i32.xor "),
        (|a| a.num(Num::I32Shl), &[0x74], "i32.shl "),
        (|a| a.num(Num::I32ShrS), &[0x75], "i32.shr_s"),
        (|a| a.num(Num::I32ShrU), &[0x76], "i32.shr_u"),
        (|a| a.num(Num::I32Rotl), &[0x77], "i32.rotl"),
        (|a| a.num(Num::I32Rotr), &[0x78], "i32.rotr"),
        (|a| a.num(Num::I64Clz), &[0x79], "i64.clz "),
        (|a| a.num(Num::I64Ctz), &[0x7a], "i64.ctz "),
        (|a| a.num(Num::I64Popcnt), &[0x7b], "i64.popcnt"),
        (|a| a.num(Num::I64Add), &[0x7c], "i64.add "),
        (|a| a.num(Num::I64Sub), &[0x7d], "i64.sub "),
        (|a| a.num(Num::I64Mul), &[0x7e], "i64.mul "),
        (|a| a.num(Num::I64DivS), &[0x7f], "i64.div_s"),
        (|a| a.num(Num::I64DivU), &[0x80], "i64.div_u"),
        (|a| a.num(Num::I64RemS), &[0x81], "i64.rem_s"),
        (|a| a.num(Num::I64RemU), &[0x82], "i64.rem_u"),
        (|a| a.num(Num::I64And), &[0x83], "i64.and "),
        (|a| a.num(Num::I64Or), &[0x84], "i64.or  "),
        (|a| a.num(Num::I64Xor), &[0x85], "i64.xor "),
        (|a| a.num(Num::I64Shl), &[0x86], "i64.shl "),
        (|a| a.num(Num::I64ShrS), &[0x87], "i64.shr_s"),
        (|a| a.num(Num::I64ShrU), &[0x88], "i64.shr_u"),
        (|a| a.num(Num::I64Rotl), &[0x89], "i64.rotl"),
        (|a| a.num(Num::I64Rotr), &[0x8a], "i64.rotr"),
        (|a| a.num(Num::F32Abs), &[0x8b], "f32.abs "),
        (|a| a.num(Num::F32Neg), &[0x8c], "f32.neg "),
        (|a| a.num(Num::F32Ceil), &[0x8d], "f32.ceil"),
        (|a| a.num(Num::F32Floor), &[0x8e], "f32.floor"),
        (|a| a.num(Num::F32Trunc), &[0x8f], "f32.trunc"),
        (|a| a.num(Num::F32Nearest), &[0x90], "f32.nearest"),
        (|a| a.num(Num::F32Sqrt), &[0x91], "f32.sqrt"),
        (|a| a.num(Num::F32Add), &[0x92], "f32.add "),
        (|a| a.num(Num::F32Sub), &[0x93], "f32.sub "),
        (|a| a.num(Num::F32Mul), &[0x94], "f32.mul "),
        (|a| a.num(Num::F32Div), &[0x95], "f32.div "),
        (|a| a.num(Num::F32Min), &[0x96], "f32.min "),
        (|a| a.num(Num::F32Max), &[0x97], "f32.max "),
        (|a| a.num(Num::F32Copysign), &[0x98], "f32.copysign"),
        (|a| a.num(Num::F64Abs), &[0x99], "f64.abs "),
        (|a| a.num(Num::F64Neg), &[0x9a], "f64.neg "),
        (|a| a.num(Num::F64Ceil), &[0x9b], "f64.ceil"),
        (|a| a.num(Num::F64Floor), &[0x9c], "f64.floor"),
        (|a| a.num(Num::F64Trunc), &[0x9d], "f64.trunc"),
        (|a| a.num(Num::F64Nearest), &[0x9e], "f64.nearest"),
        (|a| a.num(Num::F64Sqrt), &[0x9f], "f64.sqrt"),
        (|a| a.num(Num::F64Add), &[0xa0], "f64.add "),
        (|a| a.num(Num::F64Sub), &[0xa1], "f64.sub "),
        (|a| a.num(Num::F64Mul), &[0xa2], "f64.mul "),
        (|a| a.num(Num::F64Div), &[0xa3], "f64.div "),
        (|a| a.num(Num::F64Min), &[0xa4], "f64.min "),
        (|a| a.num(Num::F64Max), &[0xa5], "f64.max "),
        (|a| a.num(Num::F64Copysign), &[0xa6], "f64.copysign"),
        (|a| a.num(Num::I32WrapI64), &[0xa7], "i32.wrap_i64"),
        (|a| a.num(Num::I32TruncF32S), &[0xa8], "i32.trunc_f32_s"),
        (|a| a.num(Num::I32TruncF32U), &[0xa9], "i32.trunc_f32_u"),
        (|a| a.num(Num::I32TruncF64S), &[0xaa], "i32.trunc_f64_s"),
        (|a| a.num(Num::I32TruncF64U), &[0xab], "i32.trunc_f64_u"),
        (|a| a.num(Num::I64ExtendI32S), &[0xac], "i64.extend_i32_s"),
        (|a| a.num(Num::I64ExtendI32U), &[0xad], "i64.extend_i32_u"),
        (|a| a.num(Num::I64TruncF32S), &[0xae], "i64.trunc_f32_s"),
        (|a| a.num(Num::I64TruncF32U), &[0xaf], "i64.trunc_f32_u"),
        (|a| a.num(Num::I64TruncF64S), &[0xb0], "i64.trunc_f64_s"),
        (|a| a.num(Num::I64TruncF64U), &[0xb1], "i64.trunc_f64_u"),
        (|a| a.num(Num::F32ConvertI32S), &[0xb2], "f32.convert_i32_s"),
        (|a| a.num(Num::F32ConvertI32U), &[0xb3], "f32.convert_i32_u"),
        (|a| a.num(Num::F32ConvertI64S), &[0xb4], "f32.convert_i64_s"),
        (|a| a.num(Num::F32ConvertI64U), &[0xb5], "f32.convert_i64_u"),
        (|a| a.num(Num::F32DemoteF64), &[0xb6], "f32.demote_f64"),
        (|a| a.num(Num::F64ConvertI32S), &[0xb7], "f64.convert_i32_s"),
        (|a| a.num(Num::F64ConvertI32U), &[0xb8], "f64.convert_i32_u"),
        (|a| a.num(Num::F64ConvertI64S), &[0xb9], "f64.convert_i64_s"),
        (|a| a.num(Num::F64ConvertI64U), &[0xba], "f64.convert_i64_u"),
        (|a| a.num(Num::F64PromoteF32), &[0xbb], "f64.promote_f32"),
        (|a| a.num(Num::I32ReinterpretF32), &[0xbc], "i32.reinterpret_f32"),
        (|a| a.num(Num::I64ReinterpretF64), &[0xbd], "i64.reinterpret_f64"),
        (|a| a.num(Num::F32ReinterpretI32), &[0xbe], "f32.reinterpret_i32"),
        (|a| a.num(Num::F64ReinterpretI64), &[0xbf], "f64.reinterpret_i64"),
        (|a| a.num(Num::I32Extend8S), &[0xc0], "i32.extend8_s"),
        (|a| a.num(Num::I32Extend16S), &[0xc1], "i32.extend16_s"),
        (|a| a.num(Num::I64Extend8S), &[0xc2], "i64.extend8_s"),
        (|a| a.num(Num::I64Extend16S), &[0xc3], "i64.extend16_s"),
        (|a| a.num(Num::I64Extend32S), &[0xc4], "i64.extend32_s"),
        (|a| a.sat(Sat::I32F32S), &[0xfc, 0x00], "i32.trunc_sat_f32_s"),
        (|a| a.sat(Sat::I32F32U), &[0xfc, 0x01], "i32.trunc_sat_f32_u"),
        (|a| a.sat(Sat::I32F64S), &[0xfc, 0x02], "i32.trunc_sat_f64_s"),
        (|a| a.sat(Sat::I32F64U), &[0xfc, 0x03], "i32.trunc_sat_f64_u"),
        (|a| a.sat(Sat::I64F32S), &[0xfc, 0x04], "i64.trunc_sat_f32_s"),
        (|a| a.sat(Sat::I64F32U), &[0xfc, 0x05], "i64.trunc_sat_f32_u"),
        (|a| a.sat(Sat::I64F64S), &[0xfc, 0x06], "i64.trunc_sat_f64_s"),
        (|a| a.sat(Sat::I64F64U), &[0xfc, 0x07], "i64.trunc_sat_f64_u"),
        (|a| a.atomic(Atomic::Load, Width::W32), &[0xfe, 0x10, 0x02, 0x00], "i32.atomic.load\t 0"),
        (|a| a.atomic(Atomic::Load, Width::W64), &[0xfe, 0x11, 0x03, 0x00], "i64.atomic.load\t 0"),
        (|a| a.atomic(Atomic::Load, Width::W8), &[0xfe, 0x12, 0x00, 0x00], "i32.atomic.load8_u\t 0"),
        (|a| a.atomic(Atomic::Load, Width::W16), &[0xfe, 0x13, 0x01, 0x00], "i32.atomic.load16_u\t 0"),
        (|a| a.atomic(Atomic::Store, Width::W32), &[0xfe, 0x17, 0x02, 0x00], "i32.atomic.store\t 0"),
        (|a| a.atomic(Atomic::Store, Width::W64), &[0xfe, 0x18, 0x03, 0x00], "i64.atomic.store\t 0"),
        (|a| a.atomic(Atomic::Store, Width::W8), &[0xfe, 0x19, 0x00, 0x00], "i32.atomic.store8\t 0"),
        (|a| a.atomic(Atomic::Store, Width::W16), &[0xfe, 0x1a, 0x01, 0x00], "i32.atomic.store16\t 0"),
        (|a| a.atomic(Atomic::Add, Width::W32), &[0xfe, 0x1e, 0x02, 0x00], "i32.atomic.rmw.add\t 0"),
        (|a| a.atomic(Atomic::Add, Width::W64), &[0xfe, 0x1f, 0x03, 0x00], "i64.atomic.rmw.add\t 0"),
        (|a| a.atomic(Atomic::Add, Width::W8), &[0xfe, 0x20, 0x00, 0x00], "i32.atomic.rmw8.add_u\t 0"),
        (|a| a.atomic(Atomic::Add, Width::W16), &[0xfe, 0x21, 0x01, 0x00], "i32.atomic.rmw16.add_u\t 0"),
        (|a| a.atomic(Atomic::Sub, Width::W32), &[0xfe, 0x25, 0x02, 0x00], "i32.atomic.rmw.sub\t 0"),
        (|a| a.atomic(Atomic::Sub, Width::W64), &[0xfe, 0x26, 0x03, 0x00], "i64.atomic.rmw.sub\t 0"),
        (|a| a.atomic(Atomic::Sub, Width::W8), &[0xfe, 0x27, 0x00, 0x00], "i32.atomic.rmw8.sub_u\t 0"),
        (|a| a.atomic(Atomic::Sub, Width::W16), &[0xfe, 0x28, 0x01, 0x00], "i32.atomic.rmw16.sub_u\t 0"),
        (|a| a.atomic(Atomic::And, Width::W32), &[0xfe, 0x2c, 0x02, 0x00], "i32.atomic.rmw.and\t 0"),
        (|a| a.atomic(Atomic::And, Width::W64), &[0xfe, 0x2d, 0x03, 0x00], "i64.atomic.rmw.and\t 0"),
        (|a| a.atomic(Atomic::And, Width::W8), &[0xfe, 0x2e, 0x00, 0x00], "i32.atomic.rmw8.and_u\t 0"),
        (|a| a.atomic(Atomic::And, Width::W16), &[0xfe, 0x2f, 0x01, 0x00], "i32.atomic.rmw16.and_u\t 0"),
        (|a| a.atomic(Atomic::Or, Width::W32), &[0xfe, 0x33, 0x02, 0x00], "i32.atomic.rmw.or\t 0"),
        (|a| a.atomic(Atomic::Or, Width::W64), &[0xfe, 0x34, 0x03, 0x00], "i64.atomic.rmw.or\t 0"),
        (|a| a.atomic(Atomic::Or, Width::W8), &[0xfe, 0x35, 0x00, 0x00], "i32.atomic.rmw8.or_u\t 0"),
        (|a| a.atomic(Atomic::Or, Width::W16), &[0xfe, 0x36, 0x01, 0x00], "i32.atomic.rmw16.or_u\t 0"),
        (|a| a.atomic(Atomic::Xor, Width::W32), &[0xfe, 0x3a, 0x02, 0x00], "i32.atomic.rmw.xor\t 0"),
        (|a| a.atomic(Atomic::Xor, Width::W64), &[0xfe, 0x3b, 0x03, 0x00], "i64.atomic.rmw.xor\t 0"),
        (|a| a.atomic(Atomic::Xor, Width::W8), &[0xfe, 0x3c, 0x00, 0x00], "i32.atomic.rmw8.xor_u\t 0"),
        (|a| a.atomic(Atomic::Xor, Width::W16), &[0xfe, 0x3d, 0x01, 0x00], "i32.atomic.rmw16.xor_u\t 0"),
        (|a| a.atomic(Atomic::Xchg, Width::W32), &[0xfe, 0x41, 0x02, 0x00], "i32.atomic.rmw.xchg\t 0"),
        (|a| a.atomic(Atomic::Xchg, Width::W64), &[0xfe, 0x42, 0x03, 0x00], "i64.atomic.rmw.xchg\t 0"),
        (|a| a.atomic(Atomic::Xchg, Width::W8), &[0xfe, 0x43, 0x00, 0x00], "i32.atomic.rmw8.xchg_u\t 0"),
        (|a| a.atomic(Atomic::Xchg, Width::W16), &[0xfe, 0x44, 0x01, 0x00], "i32.atomic.rmw16.xchg_u\t 0"),
        (|a| a.atomic(Atomic::Cmpxchg, Width::W32), &[0xfe, 0x48, 0x02, 0x00], "i32.atomic.rmw.cmpxchg\t 0"),
        (|a| a.atomic(Atomic::Cmpxchg, Width::W64), &[0xfe, 0x49, 0x03, 0x00], "i64.atomic.rmw.cmpxchg\t 0"),
        (|a| a.atomic(Atomic::Cmpxchg, Width::W8), &[0xfe, 0x4a, 0x00, 0x00], "i32.atomic.rmw8.cmpxchg_u\t 0"),
        (|a| a.atomic(Atomic::Cmpxchg, Width::W16), &[0xfe, 0x4b, 0x01, 0x00], "i32.atomic.rmw16.cmpxchg_u\t 0"),
        (|a| a.simd(Simd::I8x16Splat), &[0xfd, 0x0f], "i8x16.splat"),
        (|a| a.simd(Simd::I16x8Splat), &[0xfd, 0x10], "i16x8.splat"),
        (|a| a.simd(Simd::I32x4Splat), &[0xfd, 0x11], "i32x4.splat"),
        (|a| a.simd(Simd::I64x2Splat), &[0xfd, 0x12], "i64x2.splat"),
        (|a| a.simd(Simd::I8x16Eq), &[0xfd, 0x23], "i8x16.eq"),
        (|a| a.simd(Simd::I16x8Eq), &[0xfd, 0x2d], "i16x8.eq"),
        (|a| a.simd(Simd::I32x4Eq), &[0xfd, 0x37], "i32x4.eq"),
        (|a| a.simd(Simd::I64x2Eq), &[0xfd, 0xd6, 0x01], "i64x2.eq"),
        (|a| a.simd(Simd::I8x16GtS), &[0xfd, 0x27], "i8x16.gt_s"),
        (|a| a.simd(Simd::I16x8GtS), &[0xfd, 0x31], "i16x8.gt_s"),
        (|a| a.simd(Simd::I32x4GtS), &[0xfd, 0x3b], "i32x4.gt_s"),
        (|a| a.simd(Simd::I64x2GtS), &[0xfd, 0xd9, 0x01], "i64x2.gt_s"),
        (|a| a.simd(Simd::I8x16GtU), &[0xfd, 0x28], "i8x16.gt_u"),
        (|a| a.simd(Simd::I16x8GtU), &[0xfd, 0x32], "i16x8.gt_u"),
        (|a| a.simd(Simd::I32x4GtU), &[0xfd, 0x3c], "i32x4.gt_u"),
        (|a| a.simd(Simd::I8x16MinS), &[0xfd, 0x76], "i8x16.min_s"),
        (|a| a.simd(Simd::I8x16MinU), &[0xfd, 0x77], "i8x16.min_u"),
        (|a| a.simd(Simd::I8x16MaxS), &[0xfd, 0x78], "i8x16.max_s"),
        (|a| a.simd(Simd::I8x16MaxU), &[0xfd, 0x79], "i8x16.max_u"),
        (|a| a.simd(Simd::I16x8MinS), &[0xfd, 0x96, 0x01], "i16x8.min_s"),
        (|a| a.simd(Simd::I16x8MinU), &[0xfd, 0x97, 0x01], "i16x8.min_u"),
        (|a| a.simd(Simd::I16x8MaxS), &[0xfd, 0x98, 0x01], "i16x8.max_s"),
        (|a| a.simd(Simd::I16x8MaxU), &[0xfd, 0x99, 0x01], "i16x8.max_u"),
        (|a| a.simd(Simd::I32x4MinS), &[0xfd, 0xb6, 0x01], "i32x4.min_s"),
        (|a| a.simd(Simd::I32x4MinU), &[0xfd, 0xb7, 0x01], "i32x4.min_u"),
        (|a| a.simd(Simd::I32x4MaxS), &[0xfd, 0xb8, 0x01], "i32x4.max_s"),
        (|a| a.simd(Simd::I32x4MaxU), &[0xfd, 0xb9, 0x01], "i32x4.max_u"),
        (|a| a.simd(Simd::I8x16Add), &[0xfd, 0x6e], "i8x16.add"),
        (|a| a.simd(Simd::I16x8Add), &[0xfd, 0x8e, 0x01], "i16x8.add"),
        (|a| a.simd(Simd::I32x4Add), &[0xfd, 0xae, 0x01], "i32x4.add"),
        (|a| a.simd(Simd::I64x2Add), &[0xfd, 0xce, 0x01], "i64x2.add"),
        (|a| a.simd(Simd::I8x16Sub), &[0xfd, 0x71], "i8x16.sub"),
        (|a| a.simd(Simd::I16x8Sub), &[0xfd, 0x91, 0x01], "i16x8.sub"),
        (|a| a.simd(Simd::I32x4Sub), &[0xfd, 0xb1, 0x01], "i32x4.sub"),
        (|a| a.simd(Simd::I64x2Sub), &[0xfd, 0xd1, 0x01], "i64x2.sub"),
        (|a| a.simd(Simd::I16x8Mul), &[0xfd, 0x95, 0x01], "i16x8.mul"),
        (|a| a.simd(Simd::I32x4Mul), &[0xfd, 0xb5, 0x01], "i32x4.mul"),
        (|a| a.simd(Simd::I64x2Mul), &[0xfd, 0xd5, 0x01], "i64x2.mul"),
        (|a| a.simd(Simd::V128And), &[0xfd, 0x4e], "v128.and"),
        (|a| a.simd(Simd::V128Andnot), &[0xfd, 0x4f], "v128.andnot"),
        (|a| a.simd(Simd::V128Or), &[0xfd, 0x50], "v128.or"),
        (|a| a.simd(Simd::V128Xor), &[0xfd, 0x51], "v128.xor"),
        (|a| a.simd(Simd::F32x4Add), &[0xfd, 0xe4, 0x01], "f32x4.add"),
        (|a| a.simd(Simd::F32x4Sub), &[0xfd, 0xe5, 0x01], "f32x4.sub"),
        (|a| a.simd(Simd::F32x4Mul), &[0xfd, 0xe6, 0x01], "f32x4.mul"),
        (|a| a.simd(Simd::F32x4Div), &[0xfd, 0xe7, 0x01], "f32x4.div"),
        (|a| a.simd(Simd::F64x2Add), &[0xfd, 0xf0, 0x01], "f64x2.add"),
        (|a| a.simd(Simd::F64x2Sub), &[0xfd, 0xf1, 0x01], "f64x2.sub"),
        (|a| a.simd(Simd::F64x2Mul), &[0xfd, 0xf2, 0x01], "f64x2.mul"),
        (|a| a.simd(Simd::F64x2Div), &[0xfd, 0xf3, 0x01], "f64x2.div"),
        (|a| a.simd(Simd::F32x4Eq), &[0xfd, 0x41], "f32x4.eq"),
        (|a| a.simd(Simd::F32x4Ne), &[0xfd, 0x42], "f32x4.ne"),
        (|a| a.simd(Simd::F32x4Lt), &[0xfd, 0x43], "f32x4.lt"),
        (|a| a.simd(Simd::F32x4Le), &[0xfd, 0x45], "f32x4.le"),
        (|a| a.simd(Simd::F64x2Eq), &[0xfd, 0x47], "f64x2.eq"),
        (|a| a.simd(Simd::F64x2Ne), &[0xfd, 0x48], "f64x2.ne"),
        (|a| a.simd(Simd::F64x2Lt), &[0xfd, 0x49], "f64x2.lt"),
        (|a| a.simd(Simd::F64x2Le), &[0xfd, 0x4b], "f64x2.le"),
        (|a| a.simd(Simd::F32x4Min), &[0xfd, 0xe8, 0x01], "f32x4.min"),
        (|a| a.simd(Simd::F32x4Max), &[0xfd, 0xe9, 0x01], "f32x4.max"),
        (|a| a.simd(Simd::F64x2Min), &[0xfd, 0xf4, 0x01], "f64x2.min"),
        (|a| a.simd(Simd::F64x2Max), &[0xfd, 0xf5, 0x01], "f64x2.max"),
        (|a| a.simd(Simd::I32x4ExtmulLowI16x8S), &[0xfd, 0xbc, 0x01], "i32x4.extmul_low_i16x8_s"),
        (|a| a.simd(Simd::I32x4ExtmulHighI16x8S), &[0xfd, 0xbd, 0x01], "i32x4.extmul_high_i16x8_s"),
        (|a| a.simd(Simd::I32x4ExtmulLowI16x8U), &[0xfd, 0xbe, 0x01], "i32x4.extmul_low_i16x8_u"),
        (|a| a.simd(Simd::I32x4ExtmulHighI16x8U), &[0xfd, 0xbf, 0x01], "i32x4.extmul_high_i16x8_u"),
        (|a| a.simd(Simd::I64x2ExtmulLowI32x4S), &[0xfd, 0xdc, 0x01], "i64x2.extmul_low_i32x4_s"),
        (|a| a.simd(Simd::I64x2ExtmulHighI32x4S), &[0xfd, 0xdd, 0x01], "i64x2.extmul_high_i32x4_s"),
        (|a| a.simd(Simd::I64x2ExtmulLowI32x4U), &[0xfd, 0xde, 0x01], "i64x2.extmul_low_i32x4_u"),
        (|a| a.simd(Simd::I64x2ExtmulHighI32x4U), &[0xfd, 0xdf, 0x01], "i64x2.extmul_high_i32x4_u"),
        (|a| a.simd(Simd::F32x4Sqrt), &[0xfd, 0xe3, 0x01], "f32x4.sqrt"),
        (|a| a.simd(Simd::F64x2Sqrt), &[0xfd, 0xef, 0x01], "f64x2.sqrt"),
        (|a| a.simd(Simd::I8x16Shl), &[0xfd, 0x6b], "i8x16.shl"),
        (|a| a.simd(Simd::I16x8Shl), &[0xfd, 0x8b, 0x01], "i16x8.shl"),
        (|a| a.simd(Simd::I32x4Shl), &[0xfd, 0xab, 0x01], "i32x4.shl"),
        (|a| a.simd(Simd::I64x2Shl), &[0xfd, 0xcb, 0x01], "i64x2.shl"),
        (|a| a.simd(Simd::I8x16ShrS), &[0xfd, 0x6c], "i8x16.shr_s"),
        (|a| a.simd(Simd::I16x8ShrS), &[0xfd, 0x8c, 0x01], "i16x8.shr_s"),
        (|a| a.simd(Simd::I32x4ShrS), &[0xfd, 0xac, 0x01], "i32x4.shr_s"),
        (|a| a.simd(Simd::I64x2ShrS), &[0xfd, 0xcc, 0x01], "i64x2.shr_s"),
        (|a| a.simd(Simd::I8x16ShrU), &[0xfd, 0x6d], "i8x16.shr_u"),
        (|a| a.simd(Simd::I16x8ShrU), &[0xfd, 0x8d, 0x01], "i16x8.shr_u"),
        (|a| a.simd(Simd::I32x4ShrU), &[0xfd, 0xad, 0x01], "i32x4.shr_u"),
        (|a| a.simd(Simd::I64x2ShrU), &[0xfd, 0xcd, 0x01], "i64x2.shr_u"),
        (|a| a.simd(Simd::I8x16Bitmask), &[0xfd, 0x64], "i8x16.bitmask"),
        (|a| a.simd(Simd::I16x8Bitmask), &[0xfd, 0x84, 0x01], "i16x8.bitmask"),
        (|a| a.simd(Simd::I32x4Bitmask), &[0xfd, 0xa4, 0x01], "i32x4.bitmask"),
        (|a| a.simd(Simd::I64x2Bitmask), &[0xfd, 0xc4, 0x01], "i64x2.bitmask"),
        (|a| a.simd(Simd::I8x16NarrowI16x8U), &[0xfd, 0x66], "i8x16.narrow_i16x8_u"),
        (|a| a.simd(Simd::I16x8NarrowI32x4U), &[0xfd, 0x86, 0x01], "i16x8.narrow_i32x4_u"),
        (|a| a.simd(Simd::I16x8ExtendLowI8x16S), &[0xfd, 0x87, 0x01], "i16x8.extend_low_i8x16_s"),
        (|a| a.simd(Simd::I16x8ExtendLowI8x16U), &[0xfd, 0x89, 0x01], "i16x8.extend_low_i8x16_u"),
        (|a| a.simd(Simd::I32x4ExtendLowI16x8S), &[0xfd, 0xa7, 0x01], "i32x4.extend_low_i16x8_s"),
        (|a| a.simd(Simd::I32x4ExtendLowI16x8U), &[0xfd, 0xa9, 0x01], "i32x4.extend_low_i16x8_u"),
        (|a| a.simd(Simd::I64x2ExtendLowI32x4S), &[0xfd, 0xc7, 0x01], "i64x2.extend_low_i32x4_s"),
        (|a| a.simd(Simd::I64x2ExtendLowI32x4U), &[0xfd, 0xc9, 0x01], "i64x2.extend_low_i32x4_u"),
        (|a| a.vload(VLoad::V128, 0, 0), &[0xfd, 0x00, 0x00, 0x00], "v128.load\t0:p2align=0"),
        (|a| a.vload(VLoad::V128, 4, 32), &[0xfd, 0x00, 0x04, 0x20], "v128.load\t32"),
        (|a| a.vload(VLoad::I16x8Load8x8S, 0, 0), &[0xfd, 0x01, 0x00, 0x00], "i16x8.load8x8_s\t0:p2align=0"),
        (|a| a.vload(VLoad::I16x8Load8x8S, 3, 32), &[0xfd, 0x01, 0x03, 0x20], "i16x8.load8x8_s\t32"),
        (|a| a.vload(VLoad::I16x8Load8x8U, 0, 0), &[0xfd, 0x02, 0x00, 0x00], "i16x8.load8x8_u\t0:p2align=0"),
        (|a| a.vload(VLoad::I16x8Load8x8U, 3, 32), &[0xfd, 0x02, 0x03, 0x20], "i16x8.load8x8_u\t32"),
        (|a| a.vload(VLoad::I32x4Load16x4S, 0, 0), &[0xfd, 0x03, 0x00, 0x00], "i32x4.load16x4_s\t0:p2align=0"),
        (|a| a.vload(VLoad::I32x4Load16x4S, 3, 32), &[0xfd, 0x03, 0x03, 0x20], "i32x4.load16x4_s\t32"),
        (|a| a.vload(VLoad::I32x4Load16x4U, 0, 0), &[0xfd, 0x04, 0x00, 0x00], "i32x4.load16x4_u\t0:p2align=0"),
        (|a| a.vload(VLoad::I32x4Load16x4U, 3, 32), &[0xfd, 0x04, 0x03, 0x20], "i32x4.load16x4_u\t32"),
        (|a| a.vload(VLoad::I64x2Load32x2S, 0, 0), &[0xfd, 0x05, 0x00, 0x00], "i64x2.load32x2_s\t0:p2align=0"),
        (|a| a.vload(VLoad::I64x2Load32x2S, 3, 32), &[0xfd, 0x05, 0x03, 0x20], "i64x2.load32x2_s\t32"),
        (|a| a.vload(VLoad::I64x2Load32x2U, 0, 0), &[0xfd, 0x06, 0x00, 0x00], "i64x2.load32x2_u\t0:p2align=0"),
        (|a| a.vload(VLoad::I64x2Load32x2U, 3, 32), &[0xfd, 0x06, 0x03, 0x20], "i64x2.load32x2_u\t32"),
        (|a| a.vload(VLoad::Load32Zero, 0, 0), &[0xfd, 0x5c, 0x00, 0x00], "v128.load32_zero\t0:p2align=0"),
        (|a| a.vload(VLoad::Load32Zero, 2, 32), &[0xfd, 0x5c, 0x02, 0x20], "v128.load32_zero\t32"),
        (|a| a.v128_store(0, 0), &[0xfd, 0x0b, 0x00, 0x00], "v128.store\t0:p2align=0"),
        (|a| a.v128_store(4, 200), &[0xfd, 0x0b, 0x04, 0xc8, 0x01], "v128.store\t200"),
        (|a| a.store64_lane(0, 0, 0), &[0xfd, 0x5b, 0x00, 0x00, 0x00], "v128.store64_lane\t0:p2align=0, 0"),
        (|a| a.store64_lane(3, 8, 1), &[0xfd, 0x5b, 0x03, 0x08, 0x01], "v128.store64_lane\t8, 1"),
        (|a| a.v128_const(&[0xff; 16]), &[0xfd, 0x0c, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff], "v128.const\t4294967295, 4294967295, 4294967295, 4294967295"),
        (|a| a.v128_const(&[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]), &[0xfd, 0x0c, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f], "v128.const\t50462976, 117835012, 185207048, 252579084"),
        (|a| a.shuffle(&[0, 1, 2, 3, 8, 9, 10, 11, 0, 1, 2, 3, 8, 9, 10, 11]), &[0xfd, 0x0d, 0x00, 0x01, 0x02, 0x03, 0x08, 0x09, 0x0a, 0x0b, 0x00, 0x01, 0x02, 0x03, 0x08, 0x09, 0x0a, 0x0b], "i8x16.shuffle\t0, 1, 2, 3, 8, 9, 10, 11, 0, 1, 2, 3, 8, 9, 10, 11"),
    ]
}

#[test]
fn forms_match_the_reference_disassembler() {
    for (n, (f, bytes, want)) in cases().into_iter().enumerate() {
        let mut a = Asm::new(true);
        f(&mut a);
        assert_eq!(a.code, bytes, "case {n}: {want}");
        let t = a.take_text(0).expect("recorded");
        assert_eq!(t.len(), 1, "case {n}: {want}");
        let (got, _) = text(&a.code, &t[0], &mut Printer::default());
        assert_eq!(got, want, "case {n}");
        // Text off: the same bytes, nothing recorded.
        let mut b = Asm::new(false);
        f(&mut b);
        assert_eq!(b.code, bytes);
        assert!(b.take_text(0).is_none());
    }
}

/// An object's listing: locals, filled-in indices, relocations, and the
/// branch comments counting labels on across functions.
#[test]
fn listing_matches_objdump() {
    let mut o = Obj::new(false, false, false);
    o.asm = Some(String::new());
    o.wasm = Some(Box::default());
    let f = o.sym("f");
    let g = o.sym("g");
    let h = o.sym("h");
    let sp = o.sym("__stack_pointer");
    let table = o.sym("__indirect_function_table");
    let data = o.sym("d");
    o.define_data(data, crate::obj::Sect::Data, &[1, 2, 3, 4], 4, false);
    let mut code = Vec::new();
    for (sym, n) in [(f, 0), (g, 1)] {
        let mut a = Asm::new(true);
        a.global_get(sp);
        a.local_set(1);
        a.loop_();
        a.block();
        a.local_get(0);
        a.br_if(0);
        a.local_get(1);
        a.br_table(0..2, 1);
        a.end();
        a.i32_const_rel(Rel::Addr(data));
        a.call(h);
        a.i32_const(n);
        a.local_get(0);
        a.call_indirect((vec![I32], vec![I32]), table);
        a.br(1);
        a.end();
        a.unreachable();
        a.end();
        let mut body = vec![2, 1, I32, 1, I64];
        let shift = body.len() as u32;
        body.extend_from_slice(&a.code);
        let relocs = a
            .relocs
            .iter()
            .map(|r| Reloc {
                off: r.off + shift,
                rel: r.rel.clone(),
            })
            .collect();
        code.push(Code {
            sym,
            ty: (vec![I32], vec![I32]),
            body,
            relocs,
            text: a.take_text(shift).map(|t| (vec![I32, I64], t)),
        });
        o.syms[sym as usize].def = Some((crate::obj::Sect::Text, n as u32, 0));
    }
    let w: &mut Parts = o.wasm.as_mut().expect("wasm");
    w.code = code;
    w.sigs.insert(h, (Vec::new(), Vec::new()));
    let (_, asm) = crate::isa::wasm::object::write_listed(&o);
    assert_eq!(asm.as_deref(), Some(WANT));
}

/// `llvm-objdump -dr` of the object, offsets from each function's symbol.
const WANT: &str = "
<f>:
        .local i32, i64
       6: 23 80 80 80 80 00    \tglobal.get\t0
\t\t\t00000007:  R_WASM_GLOBAL_INDEX_LEB\t__stack_pointer+0
       c: 21 01        \tlocal.set\t1
       e: 03 40        \tloop    \t                        # label0:
      10: 02 40        \tblock   \t
      12: 20 00        \tlocal.get\t0
      14: 0d 00        \tbr_if   \t0                       # 0: down to label1
      16: 20 01        \tlocal.get\t1
      18: 0e 02 00 01 01       \tbr_table \t{0, 1, 1} # 1: up to label0
      1d: 0b           \tend
      1e: 41 80 80 80 80 00    \ti32.const\t0
\t\t\t0000001f:  R_WASM_MEMORY_ADDR_SLEB\td+0
      24: 10 80 80 80 80 00    \tcall\t0
\t\t\t00000025:  R_WASM_FUNCTION_INDEX_LEB\th+0
      2a: 41 00        \ti32.const\t0
      2c: 20 00        \tlocal.get\t0
      2e: 11 81 80 80 80 00 80 80 80 80 00     \tcall_indirect\t 1
\t\t\t0000002f:  R_WASM_TYPE_INDEX_LEB\t1+0
\t\t\t00000034:  R_WASM_TABLE_NUMBER_LEB\t__indirect_function_table+0
      39: 0c 01        \tbr      \t1                       # 1: up to label0
      3b: 0b           \tend
      3c: 00           \tunreachable
      3d: 0b           \tend

<g>:
        .local i32, i64
       6: 23 80 80 80 80 00    \tglobal.get\t0
\t\t\t00000007:  R_WASM_GLOBAL_INDEX_LEB\t__stack_pointer+0
       c: 21 01        \tlocal.set\t1
       e: 03 40        \tloop    \t                        # label2:
      10: 02 40        \tblock   \t
      12: 20 00        \tlocal.get\t0
      14: 0d 00        \tbr_if   \t0                       # 0: down to label3
      16: 20 01        \tlocal.get\t1
      18: 0e 02 00 01 01       \tbr_table \t{0, 1, 1} # 1: up to label2
      1d: 0b           \tend
      1e: 41 80 80 80 80 00    \ti32.const\t0
\t\t\t0000001f:  R_WASM_MEMORY_ADDR_SLEB\td+0
      24: 10 80 80 80 80 00    \tcall\t0
\t\t\t00000025:  R_WASM_FUNCTION_INDEX_LEB\th+0
      2a: 41 01        \ti32.const\t1
      2c: 20 00        \tlocal.get\t0
      2e: 11 81 80 80 80 00 80 80 80 80 00     \tcall_indirect\t 1
\t\t\t0000002f:  R_WASM_TYPE_INDEX_LEB\t1+0
\t\t\t00000034:  R_WASM_TABLE_NUMBER_LEB\t__indirect_function_table+0
      39: 0c 01        \tbr      \t1                       # 1: up to label2
      3b: 0b           \tend
      3c: 00           \tunreachable
      3d: 0b           \tend
";

/// `--emit=asm`: each `end` by what it closes, `select` by its type,
/// relocated operands by their symbols, the rest as objdump.
#[test]
fn source_names_ends_and_operands() {
    let mut a = Asm::new(true);
    a.block();
    a.loop_();
    a.local_get(0);
    a.br_if(1);
    a.end();
    a.end();
    a.local_get(0);
    a.if_(0x7e);
    a.i64_const(1);
    a.else_();
    a.i64_const(2);
    a.end();
    a.local_get(1);
    a.local_get(1);
    a.local_get(0);
    a.select(0x7c);
    a.call(7);
    a.mem(MemOp::I32Load, 0, 4);
    a.end();
    let lines = a.take_text(0).unwrap();
    let call = a.relocs[0].off;
    let relocs = [(call, "f".to_owned())];
    let f = SrcFunc {
        code: &a.code,
        lines: &lines,
        relocs: &relocs,
    };
    let mut out = String::new();
    source(&mut out, &f);
    let want = "\tblock\n\tloop\n\tlocal.get\t0\n\tbr_if\t1\n\tend_loop\n\tend_block\n\t\
        local.get\t0\n\tif\ti64\n\ti64.const\t1\n\telse\n\ti64.const\t2\n\tend_if\n\t\
        local.get\t1\n\tlocal.get\t1\n\tlocal.get\t0\n\tf64.select\n\tcall\tf\n\t\
        i32.load\t4:p2align=0\n\tend_function\n";
    assert_eq!(out, want);
}
