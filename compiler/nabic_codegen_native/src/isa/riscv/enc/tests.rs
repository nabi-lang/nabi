//! Each form against the reference: the word `llvm-mc` writes for it
//! (`-triple=riscv64 -mattr=+m,+a,+f,+d`, and `riscv32` for the rv32
//! table) and the text `llvm-objdump -dr` prints for it, with the
//! objects' `.riscv.attributes`.

use crate::isa::riscv::enc::*;
use crate::obj::RelKind;

const fn x(n: u32) -> X {
    X(n)
}

const fn f(n: u32) -> F {
    F(n)
}

/// `(offset, form, word, text)`: branches at their offset reach `f` at 0.
type Case = (i64, Ins, u32, &'static str);

#[rustfmt::skip]
fn rv64() -> Vec<Case> {
    vec![
        (0x0, addi(x(2), x(2), -16), 0xff01_0113, "addi\tsp, sp, -0x10"),
        (0x4, addi(x(8), x(2), 16), 0x0101_0413, "addi\ts0, sp, 0x10"),
        (0x8, addi(x(0), x(0), 0), 0x0000_0013, "nop"),
        (0xc, addi(x(5), x(0), 0), 0x0000_0293, "li\tt0, 0x0"),
        (0x10, addi(x(5), x(0), -2048), 0x8000_0293, "li\tt0, -0x800"),
        (0x14, addi(x(10), x(0), 2047), 0x7ff0_0513, "li\ta0, 0x7ff"),
        (0x18, addi(x(5), x(6), 0), 0x0003_0293, "mv\tt0, t1"),
        (0x1c, addi(x(31), x(31), -1), 0xffff_8f93, "addi\tt6, t6, -0x1"),
        (0x20, addiw(x(5), x(5), 0), 0x0002_829b, "sext.w\tt0, t0"),
        (0x24, addiw(x(5), x(5), -1365), 0xaab2_829b, "addiw\tt0, t0, -0x555"),
        (0x28, addiw(x(10), x(6), 2047), 0x7ff3_051b, "addiw\ta0, t1, 0x7ff"),
        (0x2c, andi(x(28), x(29), 3), 0x003e_fe13, "andi\tt3, t4, 0x3"),
        (0x30, andi(x(29), x(29), -4), 0xffce_fe93, "andi\tt4, t4, -0x4"),
        (0x34, andi(x(11), x(11), 255), 0x0ff5_f593, "zext.b\ta1, a1"),
        (0x38, ori(x(5), x(6), -7), 0xff93_6293, "ori\tt0, t1, -0x7"),
        (0x3c, ori(x(5), x(6), 0), 0x0003_6293, "ori\tt0, t1, 0x0"),
        (0x40, xori(x(10), x(10), 1), 0x0015_4513, "xori\ta0, a0, 0x1"),
        (0x44, xori(x(5), x(6), -1), 0xfff3_4293, "not\tt0, t1"),
        (0x48, sltiu(x(10), x(10), 1), 0x0015_3513, "seqz\ta0, a0"),
        (0x4c, sltiu(x(10), x(10), 7), 0x0075_3513, "sltiu\ta0, a0, 0x7"),
        (0x54, slli(x(5), x(5), 1), 0x0012_9293, "slli\tt0, t0, 0x1"),
        (0x58, slli(x(5), x(5), 63), 0x03f2_9293, "slli\tt0, t0, 0x3f"),
        (0x5c, srli(x(10), x(5), 8), 0x0082_d513, "srli\ta0, t0, 0x8"),
        (0x60, srli(x(5), x(5), 56), 0x0382_d293, "srli\tt0, t0, 0x38"),
        (0x64, srai(x(5), x(5), 32), 0x4202_d293, "srai\tt0, t0, 0x20"),
        (0x68, srai(x(6), x(5), 31), 0x41f2_d313, "srai\tt1, t0, 0x1f"),
        (0x6c, lui(x(31), 1), 0x0000_1fb7, "lui\tt6, 0x1"),
        (0x70, lui(x(10), 1048575), 0xffff_f537, "lui\ta0, 0xfffff"),
        (0x74, lui(x(5), 524288), 0x8000_02b7, "lui\tt0, 0x80000"),
        (0x78, lui(x(5), 0), 0x0000_02b7, "lui\tt0, 0x0"),
        (0x7c, auipc(x(1), 0), 0x0000_0097, "auipc\tra, 0x0"),
        (0x80, auipc(x(5), 0), 0x0000_0297, "auipc\tt0, 0x0"),
        (0x84, auipc(x(27), 1048575), 0xffff_fd97, "auipc\ts11, 0xfffff"),
        (0x88, op(Op::Add, x(5), x(5), x(7)), 0x0072_82b3, "add\tt0, t0, t2"),
        (0x8c, op(Op::Sub, x(5), x(5), x(7)), 0x4072_82b3, "sub\tt0, t0, t2"),
        (0x90, op(Op::Sub, x(5), x(0), x(7)), 0x4070_02b3, "neg\tt0, t2"),
        (0x94, op(Op::Sll, x(6), x(6), x(7)), 0x0073_1333, "sll\tt1, t1, t2"),
        (0x98, op(Op::Slt, x(10), x(6), x(5)), 0x0053_2533, "slt\ta0, t1, t0"),
        (0x9c, op(Op::Slt, x(10), x(10), x(0)), 0x0005_2533, "sltz\ta0, a0"),
        (0xa0, op(Op::Slt, x(10), x(0), x(10)), 0x00a0_2533, "sgtz\ta0, a0"),
        (0xa4, op(Op::Sltu, x(10), x(5), x(6)), 0x0062_b533, "sltu\ta0, t0, t1"),
        (0xa8, op(Op::Sltu, x(10), x(0), x(5)), 0x0050_3533, "snez\ta0, t0"),
        (0xac, op(Op::Xor, x(14), x(6), x(28)), 0x01c3_4733, "xor\ta4, t1, t3"),
        (0xb0, op(Op::Srl, x(11), x(11), x(12)), 0x00c5_d5b3, "srl\ta1, a1, a2"),
        (0xb4, op(Op::Sra, x(5), x(6), x(10)), 0x40a3_52b3, "sra\tt0, t1, a0"),
        (0xb8, op(Op::Or, x(5), x(5), x(6)), 0x0062_e2b3, "or\tt0, t0, t1"),
        (0xbc, op(Op::And, x(14), x(14), x(15)), 0x00f7_7733, "and\ta4, a4, a5"),
        (0xc0, op(Op::Mul, x(5), x(5), x(7)), 0x0272_82b3, "mul\tt0, t0, t2"),
        (0xc4, op(Op::Mulh, x(10), x(5), x(7)), 0x0272_9533, "mulh\ta0, t0, t2"),
        (0xc8, op(Op::Mulhu, x(10), x(5), x(7)), 0x0272_b533, "mulhu\ta0, t0, t2"),
        (0xcc, op(Op::Div, x(5), x(5), x(7)), 0x0272_c2b3, "div\tt0, t0, t2"),
        (0xd0, op(Op::Divu, x(5), x(5), x(7)), 0x0272_d2b3, "divu\tt0, t0, t2"),
        (0xd4, op(Op::Rem, x(5), x(5), x(7)), 0x0272_e2b3, "rem\tt0, t0, t2"),
        (0xd8, op(Op::Remu, x(5), x(5), x(7)), 0x0272_f2b3, "remu\tt0, t0, t2"),
        (0xdc, ldst(Ls::Lb, 5, x(2), -1), 0xfff1_0283, "lb\tt0, -0x1(sp)"),
        (0xe0, ldst(Ls::Lh, 5, x(2), 2), 0x0021_1283, "lh\tt0, 0x2(sp)"),
        (0xe4, ldst(Ls::Lw, 5, x(2), 2047), 0x7ff1_2283, "lw\tt0, 0x7ff(sp)"),
        (0xe8, ldst(Ls::Ld, 1, x(2), 8), 0x0081_3083, "ld\tra, 0x8(sp)"),
        (0xec, ldst(Ls::Ld, 8, x(2), 0), 0x0001_3403, "ld\ts0, 0x0(sp)"),
        (0xf0, ldst(Ls::Lbu, 5, x(31), -2048), 0x800f_c283, "lbu\tt0, -0x800(t6)"),
        (0xf4, ldst(Ls::Lhu, 5, x(29), 0), 0x000e_d283, "lhu\tt0, 0x0(t4)"),
        (0xf8, ldst(Ls::Lwu, 5, x(29), 4), 0x004e_e283, "lwu\tt0, 0x4(t4)"),
        (0xfc, ldst(Ls::Sb, 0, x(30), 0), 0x000f_0023, "sb\tzero, 0x0(t5)"),
        (0x100, ldst(Ls::Sh, 5, x(30), -2), 0xfe5f_1f23, "sh\tt0, -0x2(t5)"),
        (0x104, ldst(Ls::Sw, 5, x(30), 2047), 0x7e5f_2fa3, "sw\tt0, 0x7ff(t5)"),
        (0x108, ldst(Ls::Sd, 1, x(2), 8), 0x0011_3423, "sd\tra, 0x8(sp)"),
        (0x10c, ldst(Ls::Sd, 9, x(8), -24), 0xfe94_3423, "sd\ts1, -0x18(s0)"),
        (0x110, ldst(Ls::Fl(false), 0, x(30), 4), 0x004f_2007, "flw\tft0, 0x4(t5)"),
        (0x114, ldst(Ls::Fl(true), 8, x(8), -32), 0xfe04_3407, "fld\tfs0, -0x20(s0)"),
        (0x118, ldst(Ls::Fs(false), 0, x(30), 0), 0x000f_2027, "fsw\tft0, 0x0(t5)"),
        (0x11c, ldst(Ls::Fs(true), 8, x(8), -32), 0xfe84_3027, "fsd\tfs0, -0x20(s0)"),
        (0x120, ldst(Ls::Fs(true), 31, x(2), 2040), 0x7ff1_3c27, "fsd\tft11, 0x7f8(sp)"),
        (0x124, jalr(x(1), x(1), 0), 0x0000_80e7, "jalr\tra"),
        (0x128, jalr(x(1), x(5), 0), 0x0002_80e7, "jalr\tt0"),
        (0x12c, jalr(x(0), x(1), 0), 0x0000_8067, "ret"),
        (0x130, jalr(x(0), x(5), 0), 0x0002_8067, "jr\tt0"),
        (0x134, amo_o(Amo::Lr, false, true, true, x(5), x(29), ZERO), 0x160e_a2af, "lr.w.aqrl\tt0, (t4)"),
        (0x138, amo_o(Amo::Lr, true, true, true, x(5), x(29), ZERO), 0x160e_b2af, "lr.d.aqrl\tt0, (t4)"),
        (0x13c, amo_o(Amo::Sc, false, true, true, x(13), x(29), x(12)), 0x1ece_a6af, "sc.w.aqrl\ta3, a2, (t4)"),
        (0x140, amo_o(Amo::Sc, true, true, true, x(13), x(29), x(6)), 0x1e6e_b6af, "sc.d.aqrl\ta3, t1, (t4)"),
        (0x144, amo_o(Amo::Swap, false, true, true, x(5), x(29), x(7)), 0x0e7e_a2af, "amoswap.w.aqrl\tt0, t2, (t4)"),
        (0x148, amo_o(Amo::Swap, true, true, true, x(5), x(29), x(7)), 0x0e7e_b2af, "amoswap.d.aqrl\tt0, t2, (t4)"),
        (0x14c, amo_o(Amo::Add, false, true, true, x(5), x(29), x(7)), 0x067e_a2af, "amoadd.w.aqrl\tt0, t2, (t4)"),
        (0x150, amo_o(Amo::Add, true, true, true, x(5), x(29), x(7)), 0x067e_b2af, "amoadd.d.aqrl\tt0, t2, (t4)"),
        (0x154, amo_o(Amo::And, true, true, true, x(5), x(29), x(7)), 0x667e_b2af, "amoand.d.aqrl\tt0, t2, (t4)"),
        (0x158, amo_o(Amo::Or, false, true, true, x(5), x(29), x(7)), 0x467e_a2af, "amoor.w.aqrl\tt0, t2, (t4)"),
        (0x15c, amo_o(Amo::Xor, true, true, true, x(5), x(29), x(7)), 0x267e_b2af, "amoxor.d.aqrl\tt0, t2, (t4)"),
        (0x160, fence(R | W, R | W), 0x0330_000f, "fence\trw, rw"),
        (0x164, fence(R, R | W), 0x0230_000f, "fence\tr, rw"),
        (0x168, fence(R | W, W), 0x0310_000f, "fence\trw, w"),
        (0x16c, unimp(), 0xc000_1073, "unimp"),
        (0x170, farith(FArith::Add, false, f(0), f(0), f(1), Rm::Dyn), 0x0010_7053, "fadd.s\tft0, ft0, ft1"),
        (0x174, farith(FArith::Add, true, f(0), f(0), f(1), Rm::Dyn), 0x0210_7053, "fadd.d\tft0, ft0, ft1"),
        (0x178, farith(FArith::Sub, true, f(27), f(3), f(31), Rm::Dyn), 0x0bf1_fdd3, "fsub.d\tfs11, ft3, ft11"),
        (0x17c, farith(FArith::Mul, false, f(0), f(0), f(1), Rm::Dyn), 0x1010_7053, "fmul.s\tft0, ft0, ft1"),
        (0x180, farith(FArith::Div, true, f(0), f(0), f(1), Rm::Dyn), 0x1a10_7053, "fdiv.d\tft0, ft0, ft1"),
        (0x184, fsqrt(false, f(0), f(0), Rm::Dyn), 0x5800_7053, "fsqrt.s\tft0, ft0"),
        (0x188, fsqrt(true, f(10), f(0), Rm::Dyn), 0x5a00_7553, "fsqrt.d\tfa0, ft0"),
        (0x18c, fsgnj(false, true, f(0), f(1), f(1)), 0x2210_8053, "fmv.d\tft0, ft1"),
        (0x190, fsgnj(false, false, f(8), f(1), f(1)), 0x2010_8453, "fmv.s\tfs0, ft1"),
        (0x194, fsgnj(true, true, f(0), f(0), f(0)), 0x2200_1053, "fneg.d\tft0, ft0"),
        (0x198, fsgnj(true, false, f(0), f(1), f(1)), 0x2010_9053, "fneg.s\tft0, ft1"),
        (0x19c, fcmp(FCmp::Eq, true, x(10), f(0), f(0)), 0xa200_2553, "feq.d\ta0, ft0, ft0"),
        (0x1a0, fcmp(FCmp::Eq, false, x(5), f(0), f(1)), 0xa010_22d3, "feq.s\tt0, ft0, ft1"),
        (0x1a4, fcmp(FCmp::Lt, true, x(5), f(1), f(0)), 0xa200_92d3, "flt.d\tt0, ft1, ft0"),
        (0x1a8, fcmp(FCmp::Le, false, x(5), f(0), f(1)), 0xa010_02d3, "fle.s\tt0, ft0, ft1"),
        (0x1ac, fcvt_to_int(IntTy::L, true, x(5), f(0), Rm::Rtz), 0xc220_12d3, "fcvt.l.d\tt0, ft0, rtz"),
        (0x1b0, fcvt_to_int(IntTy::Lu, true, x(5), f(0), Rm::Rtz), 0xc230_12d3, "fcvt.lu.d\tt0, ft0, rtz"),
        (0x1b4, fcvt_to_int(IntTy::L, false, x(5), f(0), Rm::Rtz), 0xc020_12d3, "fcvt.l.s\tt0, ft0, rtz"),
        (0x1b8, fcvt_to_int(IntTy::Lu, false, x(5), f(0), Rm::Rtz), 0xc030_12d3, "fcvt.lu.s\tt0, ft0, rtz"),
        (0x1bc, fcvt_from_int(true, IntTy::L, f(0), x(5), Rm::Dyn), 0xd222_f053, "fcvt.d.l\tft0, t0"),
        (0x1c0, fcvt_from_int(true, IntTy::Lu, f(0), x(5), Rm::Dyn), 0xd232_f053, "fcvt.d.lu\tft0, t0"),
        (0x1c4, fcvt_from_int(false, IntTy::L, f(0), x(5), Rm::Dyn), 0xd022_f053, "fcvt.s.l\tft0, t0"),
        (0x1c8, fcvt_from_int(false, IntTy::Lu, f(9), x(10), Rm::Dyn), 0xd035_74d3, "fcvt.s.lu\tfs1, a0"),
        (0x1cc, fcvt_ff(true, f(0), f(0), Rm::Rne), 0x4200_0053, "fcvt.d.s\tft0, ft0"),
        (0x1d0, fcvt_ff(true, f(0), f(0), Rm::Rne), 0x4200_0053, "fcvt.d.s\tft0, ft0"),
        (0x1d4, fcvt_ff(false, f(0), f(0), Rm::Dyn), 0x4010_7053, "fcvt.s.d\tft0, ft0"),
        (0x1d8, fmv_x(false, x(5), f(0)), 0xe000_02d3, "fmv.x.w\tt0, ft0"),
        (0x1dc, fmv_x(true, x(5), f(10)), 0xe205_02d3, "fmv.x.d\tt0, fa0"),
        (0x1e0, fmv_f(false, f(0), x(31)), 0xf00f_8053, "fmv.w.x\tft0, t6"),
        (0x1e4, fmv_f(true, f(27), x(31)), 0xf20f_8dd3, "fmv.d.x\tfs11, t6"),
        (0x1e8, branch(Cond::EQ, x(5), x(6), -488), 0xe062_8ce3, "beq\tt0, t1, 0x0 <f>"),
        (0x1ec, branch(Cond::EQ, x(5), x(0), -492), 0xe002_8ae3, "beqz\tt0, 0x0 <f>"),
        (0x1f0, branch(Cond::EQ, x(0), x(0), -496), 0xe000_08e3, "beqz\tzero, 0x0 <f>"),
        (0x1f4, branch(Cond::NE, x(5), x(0), -500), 0xe002_96e3, "bnez\tt0, 0x0 <f>"),
        (0x1f8, branch(Cond::NE, x(10), x(11), -504), 0xe0b5_14e3, "bne\ta0, a1, 0x0 <f>"),
        (0x1fc, branch(Cond::LT, x(14), x(0), -508), 0xe007_42e3, "bltz\ta4, 0x0 <f>"),
        (0x200, branch(Cond::LT, x(0), x(14), -512), 0xe0e0_40e3, "bgtz\ta4, 0x0 <f>"),
        (0x204, branch(Cond::LT, x(5), x(6), -516), 0xde62_cee3, "blt\tt0, t1, 0x0 <f>"),
        (0x208, branch(Cond::GE, x(5), x(11), -520), 0xdeb2_dce3, "bge\tt0, a1, 0x0 <f>"),
        (0x20c, branch(Cond::GE, x(0), x(11), -524), 0xdeb0_5ae3, "blez\ta1, 0x0 <f>"),
        (0x210, branch(Cond::GE, x(11), x(0), -528), 0xde05_d8e3, "bgez\ta1, 0x0 <f>"),
        (0x214, branch(Cond::LTU, x(6), x(5), -532), 0xde53_66e3, "bltu\tt1, t0, 0x0 <f>"),
        (0x218, branch(Cond::GEU, x(11), x(5), -536), 0xde55_f4e3, "bgeu\ta1, t0, 0x0 <f>"),
        (0x21c, jal(x(0), -540), 0xde5f_f06f, "j\t0x0 <f>"),
        (0x220, jal(x(1), -544), 0xde1f_f0ef, "jal\t0x0 <f>"),
        (0x224, rori(false, x(5), x(6), 51), 0x6333_5293, "rori\tt0, t1, 0x33"),
        (0x228, rori(true, x(5), x(6), 19), 0x6133_529b, "roriw\tt0, t1, 0x13"),
        (0x22c, rot(true, false, x(5), x(6), x(7)), 0x6073_12b3, "rol\tt0, t1, t2"),
        (0x230, rot(true, true, x(5), x(6), x(7)), 0x6073_12bb, "rolw\tt0, t1, t2"),
        (0x234, rot(false, false, x(5), x(6), x(7)), 0x6073_52b3, "ror\tt0, t1, t2"),
        (0x238, rot(false, true, x(5), x(6), x(7)), 0x6073_52bb, "rorw\tt0, t1, t2"),
        (0x23c, rori(false, x(10), x(11), 1), 0x6015_d513, "rori\ta0, a1, 0x1"),
        (0x240, amo_o(Amo::Lr, false, true, false, x(5), x(29), ZERO), 0x140e_a2af, "lr.w.aq\tt0, (t4)"),
        (0x244, amo_o(Amo::Sc, false, false, true, x(13), x(29), x(12)), 0x1ace_a6af, "sc.w.rl\ta3, a2, (t4)"),
        (0x248, amo_o(Amo::Add, false, false, false, x(5), x(29), x(7)), 0x007e_a2af, "amoadd.w\tt0, t2, (t4)"),
        (0x24c, amo_o(Amo::Swap, true, true, false, x(5), x(29), x(7)), 0x0c7e_b2af, "amoswap.d.aq\tt0, t2, (t4)"),
        (0x250, amo_o(Amo::Or, false, false, true, x(5), x(29), x(7)), 0x427e_a2af, "amoor.w.rl\tt0, t2, (t4)"),
        (0x254, fmadd(true, f(10), f(11), f(12), f(13), Rm::Dyn), 0x6ac5_f543, "fmadd.d\tfa0, fa1, fa2, fa3"),
        (0x258, fmadd(false, f(0), f(1), f(2), f(0), Rm::Dyn), 0x0020_f043, "fmadd.s\tft0, ft1, ft2, ft0"),
    ]
}

#[rustfmt::skip]
fn rv32() -> Vec<Case> {
    vec![
        (0x0, addi(x(2), x(2), -16), 0xff01_0113, "addi\tsp, sp, -0x10"),
        (0x4, ldst(Ls::Sw, 1, x(2), 12), 0x0011_2623, "sw\tra, 0xc(sp)"),
        (0x8, ldst(Ls::Sw, 8, x(2), 8), 0x0081_2423, "sw\ts0, 0x8(sp)"),
        (0xc, ldst(Ls::Lw, 5, x(30), -4), 0xffcf_2283, "lw\tt0, -0x4(t5)"),
        (0x10, ldst(Ls::Lbu, 5, x(29), 0), 0x000e_c283, "lbu\tt0, 0x0(t4)"),
        (0x14, slli(x(5), x(5), 31), 0x01f2_9293, "slli\tt0, t0, 0x1f"),
        (0x18, srai(x(6), x(5), 31), 0x41f2_d313, "srai\tt1, t0, 0x1f"),
        (0x1c, srli(x(10), x(5), 24), 0x0182_d513, "srli\ta0, t0, 0x18"),
        (0x20, lui(x(5), 524288), 0x8000_02b7, "lui\tt0, 0x80000"),
        (0x24, addi(x(15), x(0), -1), 0xfff0_0793, "li\ta5, -0x1"),
        (0x28, op(Op::Mul, x(5), x(5), x(7)), 0x0272_82b3, "mul\tt0, t0, t2"),
        (0x2c, op(Op::Mulhu, x(10), x(5), x(7)), 0x0272_b533, "mulhu\ta0, t0, t2"),
        (0x30, op(Op::Divu, x(5), x(5), x(7)), 0x0272_d2b3, "divu\tt0, t0, t2"),
        (0x34, amo_o(Amo::Lr, false, true, true, x(5), x(29), ZERO), 0x160e_a2af, "lr.w.aqrl\tt0, (t4)"),
        (0x38, amo_o(Amo::Sc, false, true, true, x(13), x(29), x(12)), 0x1ece_a6af, "sc.w.aqrl\ta3, a2, (t4)"),
        (0x3c, amo_o(Amo::Add, false, true, true, x(5), x(29), x(7)), 0x067e_a2af, "amoadd.w.aqrl\tt0, t2, (t4)"),
        (0x40, fence(R | W, R | W), 0x0330_000f, "fence\trw, rw"),
        (0x44, unimp(), 0xc000_1073, "unimp"),
        (0x48, jalr(x(1), x(5), 0), 0x0002_80e7, "jalr\tt0"),
        (0x4c, rori(false, x(5), x(6), 19), 0x6133_5293, "rori\tt0, t1, 0x13"),
        (0x50, rot(true, false, x(5), x(6), x(7)), 0x6073_12b3, "rol\tt0, t1, t2"),
        (0x54, rot(false, false, x(5), x(6), x(7)), 0x6073_52b3, "ror\tt0, t1, t2"),
    ]
}

#[test]
fn forms_encode_and_print_as_the_reference_tools() {
    let l = Listing {
        name: "f",
        code: &[],
        labels: Vec::new(),
        relocs: &[],
    };
    for (n, (at, i, word, want)) in rv64().into_iter().chain(rv32()).enumerate() {
        assert_eq!(i.word(), word, "case {n}: {want}");
        assert_eq!(text(i.word(), i.f, at, &l), want, "case {n}");
        // Text off: the same word, nothing recorded.
        let mut a = Asm::new(false);
        a.push(i);
        assert_eq!(a.code, [word]);
        assert!(a.lines(&l, &[]).is_empty());
    }
}

/// Patched branches show their final targets and labels; a `jalr`
/// through an `auipc` its address, by the symbol before it.
#[test]
fn listing_follows_patches_and_auipc_pairs() {
    let mut a = Asm::new(true);
    a.push(branch(Cond::NE, x(5), ZERO, 0));
    a.push(jal(ZERO, 0));
    a.push(auipc(RA, 0));
    a.push(jalr(RA, RA, 0));
    a.push(auipc(x(5), 0));
    a.push(addi(x(5), x(5), 0));
    a.push(auipc(x(6), 0));
    a.push(jalr(RA, x(6), 0));
    a.patch_branch(0, 8);
    a.patch_jal(1, -4);
    let l = Listing {
        name: "f",
        code: &[],
        labels: vec![(8, 3)],
        relocs: &[],
    };
    let syms = [(16, ".Lpcrel_hi0".to_owned())];
    let lines: Vec<String> = a.lines(&l, &syms).into_iter().map(|x| x.1).collect();
    assert_eq!(
        lines,
        [
            "bnez\tt0, 0x8 <.L3>",
            "j\t0x0 <f>",
            "auipc\tra, 0x0",
            "jalr\tra <f+0x8>",
            "auipc\tt0, 0x0",
            "mv\tt0, t0",
            "auipc\tt1, 0x0",
            "jalr\tt1 <.Lpcrel_hi0+0x8>",
        ]
    );
}

/// A listing names relocations as objdump does, `pcrel_lo` by the label
/// the ELF writer puts at its `auipc`.
#[test]
fn listing_shows_relocations() {
    let mut obj = crate::obj::Obj::new(false, true, false);
    obj.riscv = true;
    let g = obj.sym("g");
    let s = obj.sym("s");
    let mut a = Asm::new(true);
    a.push(auipc(RA, 0));
    a.push(jalr(RA, RA, 0));
    a.push(auipc(x(5), 0));
    a.push(addi(x(5), x(5), 0));
    let code: Vec<u8> = a.code.iter().flat_map(|w| w.to_le_bytes()).collect();
    let relocs = [
        (0, RelKind::RvCall, g),
        (8, RelKind::RvPcHi, s),
        (12, RelKind::RvPcLoI, s),
    ];
    let l = Listing {
        name: "f",
        code: &code,
        labels: Vec::new(),
        relocs: &relocs,
    };
    let lines = a.lines(&l, &[(8, ".Lpcrel_hi0".to_owned())]);
    crate::asm::function(&mut obj, &l, &lines, crate::asm::Bytes::Words);
    let want = "
<f>:
       0: 00000097     \tauipc\tra, 0x0
\t\t0000000000000000:  R_RISCV_CALL_PLT\tg
       4: 000080e7     \tjalr\tra <f>
       8: 00000297     \tauipc\tt0, 0x0
\t\t0000000000000008:  R_RISCV_PCREL_HI20\ts
       c: 00028293     \tmv\tt0, t0
\t\t000000000000000c:  R_RISCV_PCREL_LO12_I\t.Lpcrel_hi0
";
    assert_eq!(obj.asm.as_deref(), Some(want));
}

/// `(form, 16-bit word, objdump text, --emit=asm text)`: none for a
/// form without one.
type CCase = (Ins, Option<u16>, &'static str, &'static str);

/// Against `llvm-mc -mattr=+c` under `.option rvc` (the forms it
/// compresses, and how) and `llvm-objdump -d` of the result.
#[rustfmt::skip]
fn c64() -> Vec<CCase> {
    let st = |o, rt, b, off| ldst(o, rt, x(b), off);
    vec![
        (addi(x(10), x(10), 1), Some(0x0505), "addi\ta0, a0, 0x1", "c.addi\ta0, 0x1"),
        (addi(x(5), x(5), -32), Some(0x1281), "addi\tt0, t0, -0x20", "c.addi\tt0, -0x20"),
        (addi(x(5), x(5), 31), Some(0x02fd), "addi\tt0, t0, 0x1f", "c.addi\tt0, 0x1f"),
        (addi(x(5), x(5), 32), None, "", ""),
        (addi(x(10), x(0), 5), Some(0x4515), "li\ta0, 0x5", "c.li\ta0, 0x5"),
        (addi(x(5), x(0), -32), Some(0x5281), "li\tt0, -0x20", "c.li\tt0, -0x20"),
        (addi(x(5), x(0), 0), Some(0x4281), "li\tt0, 0x0", "c.li\tt0, 0x0"),
        (addi(x(10), x(11), 0), Some(0x852e), "mv\ta0, a1", "c.mv\ta0, a1"),
        (addi(x(2), x(2), -16), Some(0x1141), "addi\tsp, sp, -0x10", "c.addi\tsp, -0x10"),
        (addi(x(2), x(2), 496), Some(0x617d), "addi\tsp, sp, 0x1f0", "c.addi16sp\tsp, 0x1f0"),
        (addi(x(2), x(2), -512), Some(0x7101), "addi\tsp, sp, -0x200", "c.addi16sp\tsp, -0x200"),
        (addi(x(8), x(2), 16), Some(0x0800), "addi\ts0, sp, 0x10", "c.addi4spn\ts0, sp, 0x10"),
        (addi(x(15), x(2), 1020), Some(0x1ffc), "addi\ta5, sp, 0x3fc", "c.addi4spn\ta5, sp, 0x3fc"),
        (addi(x(15), x(2), 1024), None, "", ""),
        (addi(x(5), x(2), 16), None, "", ""),
        (addiw(x(10), x(10), 0), Some(0x2501), "sext.w\ta0, a0", "c.addiw\ta0, 0x0"),
        (addiw(x(5), x(5), -1), Some(0x32fd), "addiw\tt0, t0, -0x1", "c.addiw\tt0, -0x1"),
        (addiw(x(5), x(6), 0), None, "", ""),
        (lui(x(31), 1), Some(0x6f85), "lui\tt6, 0x1", "c.lui\tt6, 0x1"),
        (lui(x(10), 0xfffff), Some(0x757d), "lui\ta0, 0xfffff", "c.lui\ta0, 0xfffff"),
        (lui(x(5), 0xfffe0), Some(0x7281), "lui\tt0, 0xfffe0", "c.lui\tt0, 0xfffe0"),
        (lui(x(5), 0x20), None, "", ""),
        (lui(x(2), 1), None, "", ""),
        (slli(x(5), x(5), 3), Some(0x028e), "slli\tt0, t0, 0x3", "c.slli\tt0, 0x3"),
        (slli(x(5), x(5), 63), Some(0x12fe), "slli\tt0, t0, 0x3f", "c.slli\tt0, 0x3f"),
        (srli(x(10), x(10), 8), Some(0x8121), "srli\ta0, a0, 0x8", "c.srli\ta0, 0x8"),
        (srai(x(15), x(15), 32), Some(0x9781), "srai\ta5, a5, 0x20", "c.srai\ta5, 0x20"),
        (rori(false, x(10), x(10), 8), None, "", ""),
        (andi(x(10), x(10), -1), Some(0x997d), "andi\ta0, a0, -0x1", "c.andi\ta0, -0x1"),
        (andi(x(10), x(10), 31), Some(0x897d), "andi\ta0, a0, 0x1f", "c.andi\ta0, 0x1f"),
        (andi(x(5), x(5), 3), None, "", ""),
        (op(Op::Add, x(5), x(5), x(7)), Some(0x929e), "add\tt0, t0, t2", "c.add\tt0, t2"),
        (op(Op::Add, x(5), x(7), x(5)), Some(0x929e), "add\tt0, t0, t2", "c.add\tt0, t2"),
        (op(Op::Add, x(10), x(0), x(11)), Some(0x852e), "mv\ta0, a1", "c.mv\ta0, a1"),
        (op(Op::Add, x(10), x(11), x(0)), Some(0x852e), "mv\ta0, a1", "c.mv\ta0, a1"),
        (op(Op::Sub, x(10), x(10), x(11)), Some(0x8d0d), "sub\ta0, a0, a1", "c.sub\ta0, a1"),
        (op(Op::Xor, x(10), x(11), x(10)), Some(0x8d2d), "xor\ta0, a0, a1", "c.xor\ta0, a1"),
        (op(Op::Or, x(10), x(10), x(11)), Some(0x8d4d), "or\ta0, a0, a1", "c.or\ta0, a1"),
        (op(Op::And, x(9), x(11), x(9)), Some(0x8ced), "and\ts1, s1, a1", "c.and\ts1, a1"),
        (op(Op::Mul, x(10), x(10), x(11)), None, "", ""),
        (st(Ls::Ld, 5, 2, 8), Some(0x62a2), "ld\tt0, 0x8(sp)", "c.ldsp\tt0, 0x8(sp)"),
        (st(Ls::Sd, 5, 2, 8), Some(0xe416), "sd\tt0, 0x8(sp)", "c.sdsp\tt0, 0x8(sp)"),
        (st(Ls::Ld, 1, 2, 504), Some(0x70fe), "ld\tra, 0x1f8(sp)", "c.ldsp\tra, 0x1f8(sp)"),
        (st(Ls::Ld, 5, 2, 512), None, "", ""),
        (st(Ls::Ld, 5, 2, 4), None, "", ""),
        (st(Ls::Lw, 5, 2, 4), Some(0x4292), "lw\tt0, 0x4(sp)", "c.lwsp\tt0, 0x4(sp)"),
        (st(Ls::Sw, 5, 2, 252), Some(0xdf96), "sw\tt0, 0xfc(sp)", "c.swsp\tt0, 0xfc(sp)"),
        (st(Ls::Fl(true), 0, 2, 8), Some(0x2022), "fld\tft0, 0x8(sp)", "c.fldsp\tft0, 0x8(sp)"),
        (st(Ls::Fs(true), 0, 2, 8), Some(0xa402), "fsd\tft0, 0x8(sp)", "c.fsdsp\tft0, 0x8(sp)"),
        (st(Ls::Ld, 10, 11, 8), Some(0x6588), "ld\ta0, 0x8(a1)", "c.ld\ta0, 0x8(a1)"),
        (st(Ls::Sd, 10, 11, 248), Some(0xfde8), "sd\ta0, 0xf8(a1)", "c.sd\ta0, 0xf8(a1)"),
        (st(Ls::Lw, 10, 11, 124), Some(0x5de8), "lw\ta0, 0x7c(a1)", "c.lw\ta0, 0x7c(a1)"),
        (st(Ls::Sw, 10, 11, 4), Some(0xc1c8), "sw\ta0, 0x4(a1)", "c.sw\ta0, 0x4(a1)"),
        (st(Ls::Fl(true), 10, 11, 8), Some(0x2588), "fld\tfa0, 0x8(a1)", "c.fld\tfa0, 0x8(a1)"),
        (st(Ls::Fs(true), 10, 11, 8), Some(0xa588), "fsd\tfa0, 0x8(a1)", "c.fsd\tfa0, 0x8(a1)"),
        (st(Ls::Fl(false), 10, 11, 8), None, "", ""),
        (st(Ls::Sd, 0, 2, 0), Some(0xe002), "sd\tzero, 0x0(sp)", "c.sdsp\tzero, 0x0(sp)"),
        (st(Ls::Sw, 0, 2, 0), Some(0xc002), "sw\tzero, 0x0(sp)", "c.swsp\tzero, 0x0(sp)"),
        (st(Ls::Sd, 0, 10, 0), None, "", ""),
        (st(Ls::Lbu, 10, 11, 0), None, "", ""),
        (jalr(ZERO, RA, 0), Some(0x8082), "ret", "c.jr\tra"),
        (jalr(ZERO, x(5), 0), Some(0x8282), "jr\tt0", "c.jr\tt0"),
        (jalr(RA, x(5), 0), Some(0x9282), "jalr\tt0", "c.jalr\tt0"),
        (jalr(RA, x(5), 4), None, "", ""),
        (branch(Cond::EQ, x(10), ZERO, 16), Some(0xc901), "beqz\ta0, 0x10 <f+0x10>", "c.beqz\ta0, 0x10 <f+0x10>"),
        (branch(Cond::NE, x(15), ZERO, 14), Some(0xe799), "bnez\ta5, 0xe <f+0xe>", "c.bnez\ta5, 0xe <f+0xe>"),
        (branch(Cond::EQ, ZERO, x(10), 12), Some(0xc511), "beqz\ta0, 0xc <f+0xc>", "c.beqz\ta0, 0xc <f+0xc>"),
        (branch(Cond::EQ, x(10), ZERO, -256), Some(0xd101), "beqz\ta0, 0xffffffffffffff00 <f+0xffffffffffffff00>", "c.beqz\ta0, 0xffffffffffffff00 <f+0xffffffffffffff00>"),
        (branch(Cond::EQ, x(10), ZERO, 256), None, "", ""),
        (branch(Cond::EQ, x(5), ZERO, 16), None, "", ""),
        (branch(Cond::LT, x(10), ZERO, 16), None, "", ""),
        (jal(ZERO, 10), Some(0xa029), "j\t0xa <f+0xa>", "c.j\t0xa <f+0xa>"),
        (jal(ZERO, 2046), Some(0xaffd), "j\t0x7fe <f+0x7fe>", "c.j\t0x7fe <f+0x7fe>"),
        (jal(ZERO, -2048), Some(0xb001), "j\t0xfffffffffffff800 <f+0xfffffffffffff800>", "c.j\t0xfffffffffffff800 <f+0xfffffffffffff800>"),
        (jal(ZERO, 2048), None, "", ""),
        (jal(RA, 10), None, "", ""),
        (unimp(), Some(0x0000), "unimp", "c.unimp"),
        (addi(ZERO, ZERO, 0), Some(0x0001), "nop", "c.nop"),
        (addi(x(10), x(10), 0), Some(0x852a), "mv\ta0, a0", "c.mv\ta0, a0"),
        (addiw(x(9), x(9), 0), Some(0x2481), "sext.w\ts1, s1", "c.addiw\ts1, 0x0"),
        (auipc(x(5), 0), None, "", ""),
        (fence(R | W, R | W), None, "", ""),
    ]
}

/// The riscv32 forms that differ (`llvm-mc -triple=riscv32`): no `ld` /
/// `sd` / `fld` / `addiw`, `c.jal`.
#[rustfmt::skip]
fn c32() -> Vec<CCase> {
    vec![
        (jal(RA, 8), Some(0x2021), "jal\t0x8 <f+0x8>", "c.jal\t0x8 <f+0x8>"),
        (slli(x(5), x(5), 31), Some(0x02fe), "slli\tt0, t0, 0x1f", "c.slli\tt0, 0x1f"),
        (srai(x(15), x(15), 31), Some(0x87fd), "srai\ta5, a5, 0x1f", "c.srai\ta5, 0x1f"),
        (ldst(Ls::Lw, 10, x(11), 124), Some(0x5de8), "lw\ta0, 0x7c(a1)", "c.lw\ta0, 0x7c(a1)"),
        (ldst(Ls::Ld, 10, x(11), 8), None, "", ""),
        (ldst(Ls::Sd, 5, x(2), 8), None, "", ""),
        (addiw(x(10), x(10), 0), None, "", ""),
        (addi(x(2), x(2), -16), Some(0x1141), "addi\tsp, sp, -0x10", "c.addi\tsp, -0x10"),
    ]
}

#[test]
fn compressed_forms_as_the_reference_tools() {
    let l = Listing {
        name: "f",
        code: &[],
        labels: Vec::new(),
        relocs: &[],
    };
    for (rv64, cases) in [(true, c64()), (false, c32())] {
        for (n, (i, h, obj, src)) in cases.into_iter().enumerate() {
            assert_eq!(compress(i.word(), rv64), h, "rv64 {rv64} case {n}: {obj}");
            if h.is_none() {
                continue;
            }
            // Through `Asm`: the bytes, the listing's text and the
            // source's.
            let mut a = Asm::new(true);
            a.push(i);
            a.compact(vec![true], rv64);
            assert_eq!(a.bytes(), h.unwrap().to_le_bytes(), "case {n}");
            assert_eq!(a.lines(&l, &[]), [(0, obj.to_owned())], "case {n}");
            assert_eq!(a.lines_as(&l, &[], true), [(0, src.to_owned())], "case {n}");
        }
    }
}

/// Compacted code: offsets move with the 16-bit forms, branches are
/// repointed.
#[test]
fn compacted_offsets_and_targets() {
    let mut a = Asm::new(true);
    a.push(addi(x(10), x(10), 1));
    a.push(branch(Cond::NE, x(10), ZERO, 12));
    a.push(auipc(x(5), 0));
    a.push(addi(x(5), x(5), 0));
    assert_eq!(a.target(1), 12);
    a.repoint(1, 10);
    assert_eq!(a.target(1), 10);
    a.compact(vec![true, true, false, true], true);
    assert_eq!(
        a.bytes(),
        [0x05, 0x05, 0x09, 0xe5, 0x97, 0x02, 0x00, 0x00, 0x96, 0x82]
    );
    let lines: Vec<u32> = a
        .lines(
            &Listing {
                name: "f",
                code: &[],
                labels: Vec::new(),
                relocs: &[],
            },
            &[],
        )
        .into_iter()
        .map(|x| x.0)
        .collect();
    assert_eq!(lines, [0, 2, 4, 8]);
}
