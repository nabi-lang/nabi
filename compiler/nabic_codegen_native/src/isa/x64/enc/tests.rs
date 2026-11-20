//! Each form against the reference: the bytes GNU as writes for it (or,
//! for a form it would encode shorter, the bytes by hand), the text
//! `llvm-objdump -dr -M intel` prints for them (comments and `<..>`
//! annotations aside) and the relocated field.

use crate::asm::Listing;
use crate::isa::x64::enc::*;

type Case = (
    fn(&mut Asm),
    &'static [u8],
    &'static str,
    Option<(u32, RelKind)>,
);

#[rustfmt::skip]
fn cases() -> Vec<Case> {
    vec![
        (|a| a.mov(Sz::Q, Gpr(0), Rm::r(1)), &[0x48, 0x8b, 0xc1], "mov\trax, rcx", None),
        (|a| a.mov(Sz::Q, Gpr(9), Rm::r(10)), &[0x4d, 0x8b, 0xca], "mov\tr9, r10", None),
        (|a| a.mov(Sz::D, Gpr(2), Rm::r(6)), &[0x8b, 0xd6], "mov\tedx, esi", None),
        (|a| a.mov(Sz::Q, Gpr(0), Rm::m(4, 8)), &[0x48, 0x8b, 0x44, 0x24, 0x08], "mov\trax, qword ptr [rsp + 0x8]", None),
        (|a| a.mov(Sz::Q, Gpr(0), Rm::m(5, 0)), &[0x48, 0x8b, 0x45, 0x00], "mov\trax, qword ptr [rbp]", None),
        (|a| a.mov(Sz::Q, Gpr(2), Rm::m(13, -0x200)), &[0x49, 0x8b, 0x95, 0x00, 0xfe, 0xff, 0xff], "mov\trdx, qword ptr [r13 - 0x200]", None),
        (|a| a.mov(Sz::Q, Gpr(8), Rm::m(12, 0x100)), &[0x4d, 0x8b, 0x84, 0x24, 0x00, 0x01, 0x00, 0x00], "mov\tr8, qword ptr [r12 + 0x100]", None),
        (|a| a.mov(Sz::Q, Gpr(1), Rm::m(0, 0)), &[0x48, 0x8b, 0x08], "mov\trcx, qword ptr [rax]", None),
        (|a| a.mov(Sz::Q, Gpr(15), Rm::m(4, -128)), &[0x4c, 0x8b, 0x7c, 0x24, 0x80], "mov\tr15, qword ptr [rsp - 0x80]", None),
        (|a| a.mov(Sz::B, Gpr(6), Rm::m(0, 0)), &[0x40, 0x8a, 0x30], "mov\tsil, byte ptr [rax]", None),
        (|a| a.mov(Sz::B, Gpr(0), Rm::m(1, 0)), &[0x8a, 0x01], "mov\tal, byte ptr [rcx]", None),
        (|a| a.mov(Sz::W, Gpr(3), Rm::m(7, 2)), &[0x66, 0x8b, 0x5f, 0x02], "mov\tbx, word ptr [rdi + 0x2]", None),
        (|a| a.mov(Sz::D, Gpr(0), Rm::M(Addr::Rip(RelKind::X86Pc32, 0))), &[0x8b, 0x05, 0x00, 0x00, 0x00, 0x00], "mov\teax, dword ptr [rip]", Some((2, RelKind::X86Pc32))),
        (|a| a.mov(Sz::Q, Gpr(10), Rm::M(Addr::Rip(RelKind::X86GotLoad, 0))), &[0x4c, 0x8b, 0x15, 0x00, 0x00, 0x00, 0x00], "mov\tr10, qword ptr [rip]", Some((3, RelKind::X86GotLoad))),
        (|a| a.mov(Sz::Q, Gpr(1), Rm::M(Addr::Abs(Seg::Gs, 0x58))), &[0x65, 0x48, 0x8b, 0x0c, 0x25, 0x58, 0x00, 0x00, 0x00], "mov\trcx, qword ptr gs:[0x58]", None),
        (|a| a.mov(Sz::Q, Gpr(1), Rm::M(Addr::Abs(Seg::Fs, 0))), &[0x64, 0x48, 0x8b, 0x0c, 0x25, 0x00, 0x00, 0x00, 0x00], "mov\trcx, qword ptr fs:[0x0]", None),
        (|a| a.mov(Sz::Q, Gpr(1), Rm::M(Addr::Bi(Gpr(1), Gpr(0), 3))), &[0x48, 0x8b, 0x0c, 0xc1], "mov\trcx, qword ptr [rcx + 8*rax]", None),
        (|a| a.mov_st(Sz::Q, Rm::r(5), Gpr(4)), &[0x48, 0x89, 0xe5], "mov\trbp, rsp", None),
        (|a| a.mov_st(Sz::Q, Rm::r(4), Gpr(5)), &[0x48, 0x89, 0xec], "mov\trsp, rbp", None),
        (|a| a.mov_st(Sz::Q, Rm::m(4, 0x10), Gpr(15)), &[0x4c, 0x89, 0x7c, 0x24, 0x10], "mov\tqword ptr [rsp + 0x10], r15", None),
        (|a| a.mov_st(Sz::Q, Rm::m(5, -8), Gpr(3)), &[0x48, 0x89, 0x5d, 0xf8], "mov\tqword ptr [rbp - 0x8], rbx", None),
        (|a| a.mov_st(Sz::B, Rm::m(0, 0), Gpr(6)), &[0x40, 0x88, 0x30], "mov\tbyte ptr [rax], sil", None),
        (|a| a.mov_st(Sz::B, Rm::m(11, 1), Gpr(0)), &[0x41, 0x88, 0x43, 0x01], "mov\tbyte ptr [r11 + 0x1], al", None),
        (|a| a.mov_st(Sz::W, Rm::m(10, 0), Gpr(0)), &[0x66, 0x41, 0x89, 0x02], "mov\tword ptr [r10], ax", None),
        (|a| a.mov_st(Sz::D, Rm::m(11, 4), Gpr(0)), &[0x41, 0x89, 0x43, 0x04], "mov\tdword ptr [r11 + 0x4], eax", None),
        (|a| a.mov_ri32(Gpr(0), 1), &[0xb8, 0x01, 0x00, 0x00, 0x00], "mov\teax, 0x1", None),
        (|a| a.mov_ri32(Gpr(10), 0xffff_ffff), &[0x41, 0xba, 0xff, 0xff, 0xff, 0xff], "mov\tr10d, 0xffffffff", None),
        (|a| a.mov_ri_sx(Gpr(1), -1), &[0x48, 0xc7, 0xc1, 0xff, 0xff, 0xff, 0xff], "mov\trcx, -0x1", None),
        (|a| a.mov_ri_sx(Gpr(9), i32::MIN), &[0x49, 0xc7, 0xc1, 0x00, 0x00, 0x00, 0x80], "mov\tr9, -0x80000000", None),
        (|a| a.movabs(Gpr(2), 1 << 32), &[0x48, 0xba, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00], "movabs\trdx, 0x100000000", None),
        (|a| a.movabs(Gpr(11), 1 << 63), &[0x49, 0xbb, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x80], "movabs\tr11, -0x8000000000000000", None),
        (|a| a.movabs(Gpr(0), (i64::from(i32::MIN) - 1) as u64), &[0x48, 0xb8, 0xff, 0xff, 0xff, 0x7f, 0xff, 0xff, 0xff, 0xff], "movabs\trax, -0x80000001", None),
        (|a| a.movzx(Sz::D, Gpr(0), Sz::B, Rm::r(0)), &[0x0f, 0xb6, 0xc0], "movzx\teax, al", None),
        (|a| a.movzx(Sz::D, Gpr(6), Sz::B, Rm::r(0)), &[0x40, 0x0f, 0xb6, 0xf0], "movzx\tesi, al", None),
        (|a| a.movzx(Sz::D, Gpr(0), Sz::B, Rm::r(6)), &[0x40, 0x0f, 0xb6, 0xc6], "movzx\teax, sil", None),
        (|a| a.movzx(Sz::D, Gpr(2), Sz::B, Rm::m(4, 16)), &[0x0f, 0xb6, 0x54, 0x24, 0x10], "movzx\tedx, byte ptr [rsp + 0x10]", None),
        (|a| a.movzx(Sz::D, Gpr(12), Sz::W, Rm::m(10, 2)), &[0x45, 0x0f, 0xb7, 0x62, 0x02], "movzx\tr12d, word ptr [r10 + 0x2]", None),
        (|a| a.movsx(Sz::Q, Gpr(0), Sz::B, Rm::m(0, 0)), &[0x48, 0x0f, 0xbe, 0x00], "movsx\trax, byte ptr [rax]", None),
        (|a| a.movsx(Sz::Q, Gpr(7), Sz::B, Rm::r(7)), &[0x48, 0x0f, 0xbe, 0xff], "movsx\trdi, dil", None),
        (|a| a.movsx(Sz::Q, Gpr(13), Sz::W, Rm::r(1)), &[0x4c, 0x0f, 0xbf, 0xe9], "movsx\tr13, cx", None),
        (|a| a.movsx(Sz::D, Gpr(1), Sz::B, Rm::r(0)), &[0x0f, 0xbe, 0xc8], "movsx\tecx, al", None),
        (|a| a.movsxd(Gpr(0), Rm::r(1)), &[0x48, 0x63, 0xc1], "movsxd\trax, ecx", None),
        (|a| a.movsxd(Gpr(9), Rm::m(4, 12)), &[0x4c, 0x63, 0x4c, 0x24, 0x0c], "movsxd\tr9, dword ptr [rsp + 0xc]", None),
        (|a| a.lea(Gpr(0), Addr::Bd(Gpr(4), 8)), &[0x48, 0x8d, 0x44, 0x24, 0x08], "lea\trax, [rsp + 0x8]", None),
        (|a| a.lea(Gpr(5), Addr::Bd(Gpr(4), 0x200)), &[0x48, 0x8d, 0xac, 0x24, 0x00, 0x02, 0x00, 0x00], "lea\trbp, [rsp + 0x200]", None),
        (|a| a.lea(Gpr(0), Addr::Bi(Gpr(5), Gpr(1), 3)), &[0x48, 0x8d, 0x44, 0xcd, 0x00], "lea\trax, [rbp + 8*rcx]", None),
        (|a| a.lea(Gpr(9), Addr::Bi(Gpr(13), Gpr(12), 2)), &[0x4f, 0x8d, 0x4c, 0xa5, 0x00], "lea\tr9, [r13 + 4*r12]", None),
        (|a| a.lea(Gpr(2), Addr::Bi(Gpr(3), Gpr(10), 0)), &[0x4a, 0x8d, 0x14, 0x13], "lea\trdx, [rbx + r10]", None),
        (|a| a.lea(Gpr(1), Addr::Rip(RelKind::X86Pc32, 0)), &[0x48, 0x8d, 0x0d, 0x00, 0x00, 0x00, 0x00], "lea\trcx, [rip]", Some((3, RelKind::X86Pc32))),
        (|a| a.lea(Gpr(0), Addr::BdRel(Gpr(1), RelKind::X86SecRel, 0)), &[0x48, 0x8d, 0x81, 0x00, 0x00, 0x00, 0x00], "lea\trax, [rcx]", Some((3, RelKind::X86SecRel))),
        (|a| a.cmov(Cond::G, Sz::Q, Gpr(0), Rm::r(1)), &[0x48, 0x0f, 0x4f, 0xc1], "cmovg\trax, rcx", None),
        (|a| a.cmov(Cond::A, Sz::Q, Gpr(0), Rm::r(1)), &[0x48, 0x0f, 0x47, 0xc1], "cmova\trax, rcx", None),
        (|a| a.cmov(Cond::L, Sz::D, Gpr(10), Rm::r(11)), &[0x45, 0x0f, 0x4c, 0xd3], "cmovl\tr10d, r11d", None),
        (|a| a.setcc(Cond::NE, Gpr(6)), &[0x40, 0x0f, 0x95, 0xc6], "setne\tsil", None),
        (|a| a.setcc(Cond::B, Gpr(0)), &[0x0f, 0x92, 0xc0], "setb\tal", None),
        (|a| a.setcc(Cond::E, Gpr(9)), &[0x41, 0x0f, 0x94, 0xc1], "sete\tr9b", None),
        (|a| a.setcc(Cond::NP, Gpr(1)), &[0x0f, 0x9b, 0xc1], "setnp\tcl", None),
        (|a| a.setcc(Cond::LE, Gpr(4)), &[0x40, 0x0f, 0x9e, 0xc4], "setle\tspl", None),
        (|a| a.alu_rr(Alu::Add, Sz::Q, Gpr(0), Gpr(1)), &[0x48, 0x01, 0xc8], "add\trax, rcx", None),
        (|a| a.alu_rr(Alu::Sub, Sz::D, Gpr(9), Gpr(2)), &[0x41, 0x29, 0xd1], "sub\tr9d, edx", None),
        (|a| a.alu_rr(Alu::And, Sz::B, Gpr(0), Gpr(1)), &[0x20, 0xc8], "and\tal, cl", None),
        (|a| a.alu_rr(Alu::Or, Sz::B, Gpr(0), Gpr(1)), &[0x08, 0xc8], "or\tal, cl", None),
        (|a| a.alu_rr(Alu::Or, Sz::Q, Gpr(14), Gpr(3)), &[0x49, 0x09, 0xde], "or\tr14, rbx", None),
        (|a| a.alu_rr(Alu::Xor, Sz::D, Gpr(0), Gpr(0)), &[0x31, 0xc0], "xor\teax, eax", None),
        (|a| a.alu_rr(Alu::Cmp, Sz::Q, Gpr(12), Gpr(15)), &[0x4d, 0x39, 0xfc], "cmp\tr12, r15", None),
        (|a| a.alu_ri(Alu::Sub, Sz::Q, Rm::r(4), 0x1000), &[0x48, 0x81, 0xec, 0x00, 0x10, 0x00, 0x00], "sub\trsp, 0x1000", None),
        (|a| a.alu_ri(Alu::Sub, Sz::Q, Rm::r(4), 16), &[0x48, 0x81, 0xec, 0x10, 0x00, 0x00, 0x00], "sub\trsp, 0x10", None),
        (|a| a.alu_ri(Alu::Add, Sz::Q, Rm::r(0), 0x7fff_ffff), &[0x48, 0x81, 0xc0, 0xff, 0xff, 0xff, 0x7f], "add\trax, 0x7fffffff", None),
        (|a| a.alu_ri(Alu::Cmp, Sz::Q, Rm::r(10), -100_000), &[0x49, 0x81, 0xfa, 0x60, 0x79, 0xfe, 0xff], "cmp\tr10, -0x186a0", None),
        (|a| a.alu_ri(Alu::And, Sz::D, Rm::r(3), -16), &[0x81, 0xe3, 0xf0, 0xff, 0xff, 0xff], "and\tebx, 0xfffffff0", None),
        (|a| a.alu_ri(Alu::Xor, Sz::D, Rm::r(1), i32::MIN), &[0x81, 0xf1, 0x00, 0x00, 0x00, 0x80], "xor\tecx, 0x80000000", None),
        (|a| a.alu_ri8(Alu::Or, Sz::Q, Rm::m(4, 0), 0), &[0x48, 0x83, 0x0c, 0x24, 0x00], "or\tqword ptr [rsp], 0x0", None),
        (|a| a.alu_ri8(Alu::Add, Sz::Q, Rm::r(10), 8), &[0x49, 0x83, 0xc2, 0x08], "add\tr10, 0x8", None),
        (|a| a.alu_ri8(Alu::Sub, Sz::Q, Rm::r(4), -128), &[0x48, 0x83, 0xec, 0x80], "sub\trsp, -0x80", None),
        (|a| a.alu_ri8(Alu::And, Sz::D, Rm::r(0), 1), &[0x83, 0xe0, 0x01], "and\teax, 0x1", None),
        (|a| a.alu_ri8(Alu::Sub, Sz::Q, Rm::r(0), 56), &[0x48, 0x83, 0xe8, 0x38], "sub\trax, 0x38", None),
        (|a| a.alu_eax(Alu::Xor, i32::MIN), &[0x35, 0x00, 0x00, 0x00, 0x80], "xor\teax, 0x80000000", None),
        (|a| a.test_rr(Sz::Q, Gpr(0), Gpr(0)), &[0x48, 0x85, 0xc0], "test\trax, rax", None),
        (|a| a.test_rr(Sz::D, Gpr(2), Gpr(2)), &[0x85, 0xd2], "test\tedx, edx", None),
        (|a| a.test_rr(Sz::D, Gpr(13), Gpr(13)), &[0x45, 0x85, 0xed], "test\tr13d, r13d", None),
        (|a| a.test_ri(Sz::D, Rm::r(2), 0xaaaa), &[0xf7, 0xc2, 0xaa, 0xaa, 0x00, 0x00], "test\tedx, 0xaaaa", None),
        (|a| a.test_ri(Sz::D, Rm::r(2), 0xaaaa_aaaau32 as i32), &[0xf7, 0xc2, 0xaa, 0xaa, 0xaa, 0xaa], "test\tedx, 0xaaaaaaaa", None),
        (|a| a.test_ri(Sz::Q, Rm::r(12), -1), &[0x49, 0xf7, 0xc4, 0xff, 0xff, 0xff, 0xff], "test\tr12, -0x1", None),
        (|a| a.un(Un::Neg, Sz::Q, Rm::r(1)), &[0x48, 0xf7, 0xd9], "neg\trcx", None),
        (|a| a.un(Un::Not, Sz::D, Rm::r(0)), &[0xf7, 0xd0], "not\teax", None),
        (|a| a.un(Un::Mul, Sz::Q, Rm::r(1)), &[0x48, 0xf7, 0xe1], "mul\trcx", None),
        (|a| a.un(Un::Imul, Sz::Q, Rm::r(1)), &[0x48, 0xf7, 0xe9], "imul\trcx", None),
        (|a| a.un(Un::Div, Sz::D, Rm::r(1)), &[0xf7, 0xf1], "div\tecx", None),
        (|a| a.un(Un::Idiv, Sz::Q, Rm::r(1)), &[0x48, 0xf7, 0xf9], "idiv\trcx", None),
        (|a| a.un(Un::Neg, Sz::Q, Rm::r(15)), &[0x49, 0xf7, 0xdf], "neg\tr15", None),
        (|a| a.dec(Sz::D, Gpr(1)), &[0xff, 0xc9], "dec\tecx", None),
        (|a| a.dec(Sz::Q, Gpr(11)), &[0x49, 0xff, 0xcb], "dec\tr11", None),
        (|a| a.shift_ri(Shift::Shr, Sz::Q, Gpr(1), 63), &[0x48, 0xc1, 0xe9, 0x3f], "shr\trcx, 0x3f", None),
        (|a| a.shift_ri(Shift::Sar, Sz::D, Gpr(9), 31), &[0x41, 0xc1, 0xf9, 0x1f], "sar\tr9d, 0x1f", None),
        (|a| a.shift_ri(Shift::Shl, Sz::Q, Gpr(0), 4), &[0x48, 0xc1, 0xe0, 0x04], "shl\trax, 0x4", None),
        (|a| a.shift_ri(Shift::Rol, Sz::W, Gpr(0), 8), &[0x66, 0xc1, 0xc0, 0x08], "rol\tax, 0x8", None),
        (|a| a.shift_1(Shift::Shr, Sz::Q, Gpr(1)), &[0x48, 0xd1, 0xe9], "shr\trcx", None),
        (|a| a.shift_cl(Shift::Shl, Sz::Q, Gpr(0)), &[0x48, 0xd3, 0xe0], "shl\trax, cl", None),
        (|a| a.shift_cl(Shift::Sar, Sz::D, Gpr(2)), &[0xd3, 0xfa], "sar\tedx, cl", None),
        (|a| a.shift_cl(Shift::Shr, Sz::Q, Gpr(12)), &[0x49, 0xd3, 0xec], "shr\tr12, cl", None),
        (|a| a.shift_ri(Shift::Rol, Sz::Q, Gpr(0), 13), &[0x48, 0xc1, 0xc0, 0x0d], "rol\trax, 0xd", None),
        (|a| a.shift_ri(Shift::Rol, Sz::D, Gpr(9), 31), &[0x41, 0xc1, 0xc1, 0x1f], "rol\tr9d, 0x1f", None),
        (|a| a.shift_cl(Shift::Rol, Sz::Q, Gpr(12)), &[0x49, 0xd3, 0xc4], "rol\tr12, cl", None),
        (|a| a.shift_cl(Shift::Rol, Sz::D, Gpr(6)), &[0xd3, 0xc6], "rol\tesi, cl", None),
        (|a| a.imul(Sz::Q, Gpr(0), Rm::r(1)), &[0x48, 0x0f, 0xaf, 0xc1], "imul\trax, rcx", None),
        (|a| a.imul(Sz::D, Gpr(8), Rm::m(4, 4)), &[0x44, 0x0f, 0xaf, 0x44, 0x24, 0x04], "imul\tr8d, dword ptr [rsp + 0x4]", None),
        (|a| a.imul_ri(Sz::Q, Gpr(2), Rm::r(0), 1000), &[0x48, 0x69, 0xd0, 0xe8, 0x03, 0x00, 0x00], "imul\trdx, rax, 0x3e8", None),
        (|a| a.imul_ri(Sz::D, Gpr(0), Rm::r(1), -3), &[0x69, 0xc1, 0xfd, 0xff, 0xff, 0xff], "imul\teax, ecx, 0xfffffffd", None),
        (|a| a.imul_ri(Sz::D, Gpr(0), Rm::r(1), i32::MIN), &[0x69, 0xc1, 0x00, 0x00, 0x00, 0x80], "imul\teax, ecx, 0x80000000", None),
        (|a| a.cqo(Sz::Q), &[0x48, 0x99], "cqo", None),
        (|a| a.cqo(Sz::D), &[0x99], "cdq", None),
        (|a| a.bits(Bits::Bsf, Sz::Q, Gpr(0), Rm::r(0)), &[0x48, 0x0f, 0xbc, 0xc0], "bsf\trax, rax", None),
        (|a| a.bits(Bits::Bsr, Sz::Q, Gpr(0), Rm::r(0)), &[0x48, 0x0f, 0xbd, 0xc0], "bsr\trax, rax", None),
        (|a| a.bits(Bits::Popcnt, Sz::Q, Gpr(0), Rm::r(0)), &[0xf3, 0x48, 0x0f, 0xb8, 0xc0], "popcnt\trax, rax", None),
        (|a| a.bits(Bits::Lzcnt, Sz::Q, Gpr(0), Rm::r(0)), &[0xf3, 0x48, 0x0f, 0xbd, 0xc0], "lzcnt\trax, rax", None),
        (|a| a.bits(Bits::Tzcnt, Sz::Q, Gpr(0), Rm::r(0)), &[0xf3, 0x48, 0x0f, 0xbc, 0xc0], "tzcnt\trax, rax", None),
        (|a| a.bits(Bits::Popcnt, Sz::Q, Gpr(10), Rm::r(11)), &[0xf3, 0x4d, 0x0f, 0xb8, 0xd3], "popcnt\tr10, r11", None),
        (|a| a.bts(Sz::Q, Gpr(0), 32), &[0x48, 0x0f, 0xba, 0xe8, 0x20], "bts\trax, 0x20", None),
        (|a| a.bswap(Sz::D, Gpr(0)), &[0x0f, 0xc8], "bswap\teax", None),
        (|a| a.bswap(Sz::Q, Gpr(0)), &[0x48, 0x0f, 0xc8], "bswap\trax", None),
        (|a| a.bswap(Sz::Q, Gpr(9)), &[0x49, 0x0f, 0xc9], "bswap\tr9", None),
        (|a| a.lock(), &[0xf0], "lock", None),
        (|a| a.xchg(Sz::Q, Addr::Bd(Gpr(10), 0), Gpr(0)), &[0x49, 0x87, 0x02], "xchg\tqword ptr [r10], rax", None),
        (|a| a.xchg(Sz::D, Addr::Bd(Gpr(10), 0), Gpr(0)), &[0x41, 0x87, 0x02], "xchg\tdword ptr [r10], eax", None),
        (|a| a.xchg(Sz::W, Addr::Bd(Gpr(10), 0), Gpr(0)), &[0x66, 0x41, 0x87, 0x02], "xchg\tword ptr [r10], ax", None),
        (|a| a.xchg(Sz::B, Addr::Bd(Gpr(10), 0), Gpr(0)), &[0x41, 0x86, 0x02], "xchg\tbyte ptr [r10], al", None),
        (|a| a.xadd(Sz::Q, Addr::Bd(Gpr(10), 0), Gpr(1)), &[0x49, 0x0f, 0xc1, 0x0a], "xadd\tqword ptr [r10], rcx", None),
        (|a| a.xadd(Sz::B, Addr::Bd(Gpr(10), 0), Gpr(1)), &[0x41, 0x0f, 0xc0, 0x0a], "xadd\tbyte ptr [r10], cl", None),
        (|a| a.xadd(Sz::W, Addr::Bd(Gpr(10), 0), Gpr(1)), &[0x66, 0x41, 0x0f, 0xc1, 0x0a], "xadd\tword ptr [r10], cx", None),
        (|a| a.cmpxchg(Sz::D, Addr::Bd(Gpr(10), 0), Gpr(2)), &[0x41, 0x0f, 0xb1, 0x12], "cmpxchg\tdword ptr [r10], edx", None),
        (|a| a.cmpxchg(Sz::Q, Addr::Bd(Gpr(10), 0), Gpr(1)), &[0x49, 0x0f, 0xb1, 0x0a], "cmpxchg\tqword ptr [r10], rcx", None),
        (|a| a.cmpxchg(Sz::B, Addr::Bd(Gpr(10), 0), Gpr(2)), &[0x41, 0x0f, 0xb0, 0x12], "cmpxchg\tbyte ptr [r10], dl", None),
        (|a| a.cmpxchg(Sz::B, Addr::Bd(Gpr(10), 0), Gpr(6)), &[0x41, 0x0f, 0xb0, 0x32], "cmpxchg\tbyte ptr [r10], sil", None),
        (|a| { a.jcc(Cond::NE, Some(0)); }, &[0x0f, 0x85, 0xfa, 0xff, 0xff, 0xff], "jne\t0x0", None),
        (|a| { a.jcc(Cond::B, None); }, &[0x0f, 0x82, 0x00, 0x00, 0x00, 0x00], "jb\t0x6", None),
        (|a| { a.jcc(Cond::O, None); }, &[0x0f, 0x80, 0x00, 0x00, 0x00, 0x00], "jo\t0x6", None),
        (|a| { a.jcc(Cond::GE, None); }, &[0x0f, 0x8d, 0x00, 0x00, 0x00, 0x00], "jge\t0x6", None),
        (|a| { a.jmp(None); }, &[0xe9, 0x00, 0x00, 0x00, 0x00], "jmp\t0x5", None),
        (|a| { a.jmp(Some(0)); }, &[0xe9, 0xfb, 0xff, 0xff, 0xff], "jmp\t0x0", None),
        (|a| a.call(RelKind::X86Call, 0), &[0xe8, 0x00, 0x00, 0x00, 0x00], "call\t0x5", Some((1, RelKind::X86Call))),
        (|a| a.call_r(Gpr(11)), &[0x41, 0xff, 0xd3], "call\tr11", None),
        (|a| a.push_rbp(), &[0x55], "push\trbp", None),
        (|a| a.pop_rbp(), &[0x5d], "pop\trbp", None),
        (|a| a.ret(), &[0xc3], "ret", None),
        (|a| a.ud2(), &[0x0f, 0x0b], "ud2", None),
        (|a| a.fld(Sz::D, Addr::Bd(Gpr(4), -8)), &[0xd9, 0x44, 0x24, 0xf8], "fld\tdword ptr [rsp - 0x8]", None),
        (|a| a.fld(Sz::Q, Addr::Bd(Gpr(10), 16)), &[0x41, 0xdd, 0x42, 0x10], "fld\tqword ptr [r10 + 0x10]", None),
        (|a| a.fstp(Sz::D, Addr::Bd(Gpr(11), 0)), &[0x41, 0xd9, 0x1b], "fstp\tdword ptr [r11]", None),
        (|a| a.fstp(Sz::Q, Addr::Bd(Gpr(4), 0x10)), &[0xdd, 0x5c, 0x24, 0x10], "fstp\tqword ptr [rsp + 0x10]", None),
        (|a| a.sse(Sse::Movss, Xmm(1), Xrm::M(Addr::Bd(Gpr(4), 0x80))), &[0xf3, 0x0f, 0x10, 0x8c, 0x24, 0x80, 0x00, 0x00, 0x00], "movss\txmm1, dword ptr [rsp + 0x80]", None),
        (|a| a.sse(Sse::Movsd, Xmm(0), Xrm::M(Addr::Bd(Gpr(10), 0))), &[0xf2, 0x41, 0x0f, 0x10, 0x02], "movsd\txmm0, qword ptr [r10]", None),
        (|a| a.sse(Sse::Movsd, Xmm(12), Xrm::M(Addr::Bd(Gpr(4), 8))), &[0xf2, 0x44, 0x0f, 0x10, 0x64, 0x24, 0x08], "movsd\txmm12, qword ptr [rsp + 0x8]", None),
        (|a| a.sse_st(Sse::MovssSt, Addr::Bd(Gpr(4), 4), Xmm(9)), &[0xf3, 0x44, 0x0f, 0x11, 0x4c, 0x24, 0x04], "movss\tdword ptr [rsp + 0x4], xmm9", None),
        (|a| a.sse_st(Sse::MovsdSt, Addr::Bd(Gpr(11), 0), Xmm(0)), &[0xf2, 0x41, 0x0f, 0x11, 0x03], "movsd\tqword ptr [r11], xmm0", None),
        (|a| a.sse(Sse::Movaps, Xmm(4), Xrm::X(Xmm(12))), &[0x41, 0x0f, 0x28, 0xe4], "movaps\txmm4, xmm12", None),
        (|a| a.sse(Sse::Movaps, Xmm(6), Xrm::M(Addr::Bd(Gpr(5), -0x20))), &[0x0f, 0x28, 0x75, 0xe0], "movaps\txmm6, xmmword ptr [rbp - 0x20]", None),
        (|a| a.sse_st(Sse::MovapsSt, Addr::Bd(Gpr(5), -0x20), Xmm(6)), &[0x0f, 0x29, 0x75, 0xe0], "movaps\txmmword ptr [rbp - 0x20], xmm6", None),
        (|a| a.sse_st(Sse::MovapsSt, Addr::Bd(Gpr(5), -0x200), Xmm(15)), &[0x44, 0x0f, 0x29, 0xbd, 0x00, 0xfe, 0xff, 0xff], "movaps\txmmword ptr [rbp - 0x200], xmm15", None),
        (|a| a.sse(Sse::Addss, Xmm(0), Xrm::X(Xmm(1))), &[0xf3, 0x0f, 0x58, 0xc1], "addss\txmm0, xmm1", None),
        (|a| a.sse(Sse::Addsd, Xmm(8), Xrm::X(Xmm(9))), &[0xf2, 0x45, 0x0f, 0x58, 0xc1], "addsd\txmm8, xmm9", None),
        (|a| a.sse(Sse::Subss, Xmm(0), Xrm::X(Xmm(1))), &[0xf3, 0x0f, 0x5c, 0xc1], "subss\txmm0, xmm1", None),
        (|a| a.sse(Sse::Subsd, Xmm(0), Xrm::X(Xmm(1))), &[0xf2, 0x0f, 0x5c, 0xc1], "subsd\txmm0, xmm1", None),
        (|a| a.sse(Sse::Mulss, Xmm(2), Xrm::X(Xmm(3))), &[0xf3, 0x0f, 0x59, 0xd3], "mulss\txmm2, xmm3", None),
        (|a| a.sse(Sse::Mulsd, Xmm(2), Xrm::X(Xmm(3))), &[0xf2, 0x0f, 0x59, 0xd3], "mulsd\txmm2, xmm3", None),
        (|a| a.sse(Sse::Divss, Xmm(4), Xrm::X(Xmm(5))), &[0xf3, 0x0f, 0x5e, 0xe5], "divss\txmm4, xmm5", None),
        (|a| a.sse(Sse::Divsd, Xmm(4), Xrm::X(Xmm(5))), &[0xf2, 0x0f, 0x5e, 0xe5], "divsd\txmm4, xmm5", None),
        (|a| a.sse(Sse::Sqrtss, Xmm(0), Xrm::X(Xmm(0))), &[0xf3, 0x0f, 0x51, 0xc0], "sqrtss\txmm0, xmm0", None),
        (|a| a.sse(Sse::Sqrtsd, Xmm(13), Xrm::X(Xmm(14))), &[0xf2, 0x45, 0x0f, 0x51, 0xee], "sqrtsd\txmm13, xmm14", None),
        (|a| a.sse(Sse::Cvtss2sd, Xmm(0), Xrm::X(Xmm(0))), &[0xf3, 0x0f, 0x5a, 0xc0], "cvtss2sd\txmm0, xmm0", None),
        (|a| a.sse(Sse::Cvtsd2ss, Xmm(0), Xrm::X(Xmm(0))), &[0xf2, 0x0f, 0x5a, 0xc0], "cvtsd2ss\txmm0, xmm0", None),
        (|a| a.sse(Sse::Ucomiss, Xmm(0), Xrm::X(Xmm(1))), &[0x0f, 0x2e, 0xc1], "ucomiss\txmm0, xmm1", None),
        (|a| a.sse(Sse::Ucomisd, Xmm(0), Xrm::X(Xmm(2))), &[0x66, 0x0f, 0x2e, 0xc2], "ucomisd\txmm0, xmm2", None),
        (|a| a.sse(Sse::Xorps, Xmm(2), Xrm::X(Xmm(2))), &[0x0f, 0x57, 0xd2], "xorps\txmm2, xmm2", None),
        (|a| a.sse(Sse::Movdqa, Xmm(1), Xrm::X(Xmm(2))), &[0x66, 0x0f, 0x6f, 0xca], "movdqa\txmm1, xmm2", None),
        (|a| a.sse(Sse::Movdqu, Xmm(7), Xrm::M(Addr::Bi(Gpr(8), Gpr(0), 0))), &[0xf3, 0x41, 0x0f, 0x6f, 0x3c, 0x00], "movdqu\txmm7, xmmword ptr [r8 + rax]", None),
        (|a| a.sse(Sse::Movdqu, Xmm(3), Xrm::M(Addr::Bi(Gpr(6), Gpr(0), 0))), &[0xf3, 0x0f, 0x6f, 0x1c, 0x06], "movdqu\txmm3, xmmword ptr [rsi + rax]", None),
        (|a| a.sse(Sse::Movdqu, Xmm(6), Xrm::M(Addr::Bd(Gpr(4), 0x40))), &[0xf3, 0x0f, 0x6f, 0x74, 0x24, 0x40], "movdqu\txmm6, xmmword ptr [rsp + 0x40]", None),
        (|a| a.sse_st(Sse::MovdquSt, Addr::Bd(Gpr(4), 0x40), Xmm(6)), &[0xf3, 0x0f, 0x7f, 0x74, 0x24, 0x40], "movdqu\txmmword ptr [rsp + 0x40], xmm6", None),
        (|a| a.sse_st(Sse::MovdquSt, Addr::Bi(Gpr(13), Gpr(0), 1), Xmm(12)), &[0xf3, 0x45, 0x0f, 0x7f, 0x64, 0x45, 0x00], "movdqu\txmmword ptr [r13 + 2*rax], xmm12", None),
        (|a| a.sse(Sse::MovqLd, Xmm(1), Xrm::M(Addr::Bi(Gpr(6), Gpr(0), 1))), &[0xf3, 0x0f, 0x7e, 0x0c, 0x46], "movq\txmm1, qword ptr [rsi + 2*rax]", None),
        (|a| a.sse(Sse::MovdLd, Xmm(1), Xrm::M(Addr::Bi(Gpr(7), Gpr(0), 2))), &[0x66, 0x0f, 0x6e, 0x0c, 0x87], "movd\txmm1, dword ptr [rdi + 4*rax]", None),
        (|a| a.sse_st(Sse::MovqSt, Addr::Bi(Gpr(6), Gpr(0), 0), Xmm(15)), &[0x66, 0x44, 0x0f, 0xd6, 0x3c, 0x06], "movq\tqword ptr [rsi + rax], xmm15", None),
        (|a| a.sse_st(Sse::MovdSt, Addr::Bi(Gpr(6), Gpr(0), 0), Xmm(15)), &[0x66, 0x44, 0x0f, 0x7e, 0x3c, 0x06], "movd\tdword ptr [rsi + rax], xmm15", None),
        (|a| a.sse(Sse::Addps, Xmm(0), Xrm::X(Xmm(1))), &[0x0f, 0x58, 0xc1], "addps\txmm0, xmm1", None),
        (|a| a.sse(Sse::Addpd, Xmm(0), Xrm::X(Xmm(1))), &[0x66, 0x0f, 0x58, 0xc1], "addpd\txmm0, xmm1", None),
        (|a| a.sse(Sse::Subps, Xmm(0), Xrm::X(Xmm(1))), &[0x0f, 0x5c, 0xc1], "subps\txmm0, xmm1", None),
        (|a| a.sse(Sse::Subpd, Xmm(0), Xrm::X(Xmm(1))), &[0x66, 0x0f, 0x5c, 0xc1], "subpd\txmm0, xmm1", None),
        (|a| a.sse(Sse::Mulps, Xmm(0), Xrm::X(Xmm(1))), &[0x0f, 0x59, 0xc1], "mulps\txmm0, xmm1", None),
        (|a| a.sse(Sse::Mulpd, Xmm(0), Xrm::X(Xmm(1))), &[0x66, 0x0f, 0x59, 0xc1], "mulpd\txmm0, xmm1", None),
        (|a| a.sse(Sse::Divps, Xmm(0), Xrm::X(Xmm(1))), &[0x0f, 0x5e, 0xc1], "divps\txmm0, xmm1", None),
        (|a| a.sse(Sse::Divpd, Xmm(0), Xrm::X(Xmm(1))), &[0x66, 0x0f, 0x5e, 0xc1], "divpd\txmm0, xmm1", None),
        (|a| a.sse(Sse::Sqrtps, Xmm(0), Xrm::X(Xmm(1))), &[0x0f, 0x51, 0xc1], "sqrtps\txmm0, xmm1", None),
        (|a| a.sse(Sse::Sqrtpd, Xmm(0), Xrm::X(Xmm(1))), &[0x66, 0x0f, 0x51, 0xc1], "sqrtpd\txmm0, xmm1", None),
        (|a| a.sse(Sse::Punpcklbw, Xmm(3), Xrm::X(Xmm(15))), &[0x66, 0x41, 0x0f, 0x60, 0xdf], "punpcklbw\txmm3, xmm15", None),
        (|a| a.sse(Sse::Punpcklwd, Xmm(3), Xrm::X(Xmm(3))), &[0x66, 0x0f, 0x61, 0xdb], "punpcklwd\txmm3, xmm3", None),
        (|a| a.sse(Sse::Punpckldq, Xmm(3), Xrm::X(Xmm(15))), &[0x66, 0x41, 0x0f, 0x62, 0xdf], "punpckldq\txmm3, xmm15", None),
        (|a| a.sse(Sse::Punpcklqdq, Xmm(0), Xrm::X(Xmm(0))), &[0x66, 0x0f, 0x6c, 0xc0], "punpcklqdq\txmm0, xmm0", None),
        (|a| a.sse(Sse::Packsswb, Xmm(15), Xrm::X(Xmm(15))), &[0x66, 0x45, 0x0f, 0x63, 0xff], "packsswb\txmm15, xmm15", None),
        (|a| a.sse(Sse::Packuswb, Xmm(15), Xrm::X(Xmm(15))), &[0x66, 0x45, 0x0f, 0x67, 0xff], "packuswb\txmm15, xmm15", None),
        (|a| a.sse(Sse::Packssdw, Xmm(15), Xrm::X(Xmm(15))), &[0x66, 0x45, 0x0f, 0x6b, 0xff], "packssdw\txmm15, xmm15", None),
        (|a| a.sse(Sse::Paddb, Xmm(0), Xrm::X(Xmm(1))), &[0x66, 0x0f, 0xfc, 0xc1], "paddb\txmm0, xmm1", None),
        (|a| a.sse(Sse::Paddw, Xmm(0), Xrm::X(Xmm(1))), &[0x66, 0x0f, 0xfd, 0xc1], "paddw\txmm0, xmm1", None),
        (|a| a.sse(Sse::Paddd, Xmm(0), Xrm::X(Xmm(1))), &[0x66, 0x0f, 0xfe, 0xc1], "paddd\txmm0, xmm1", None),
        (|a| a.sse(Sse::Paddq, Xmm(0), Xrm::X(Xmm(1))), &[0x66, 0x0f, 0xd4, 0xc1], "paddq\txmm0, xmm1", None),
        (|a| a.sse(Sse::Psubb, Xmm(2), Xrm::X(Xmm(3))), &[0x66, 0x0f, 0xf8, 0xd3], "psubb\txmm2, xmm3", None),
        (|a| a.sse(Sse::Psubw, Xmm(2), Xrm::X(Xmm(3))), &[0x66, 0x0f, 0xf9, 0xd3], "psubw\txmm2, xmm3", None),
        (|a| a.sse(Sse::Psubd, Xmm(2), Xrm::X(Xmm(3))), &[0x66, 0x0f, 0xfa, 0xd3], "psubd\txmm2, xmm3", None),
        (|a| a.sse(Sse::Psubq, Xmm(2), Xrm::X(Xmm(3))), &[0x66, 0x0f, 0xfb, 0xd3], "psubq\txmm2, xmm3", None),
        (|a| a.sse(Sse::Pmullw, Xmm(4), Xrm::X(Xmm(5))), &[0x66, 0x0f, 0xd5, 0xe5], "pmullw\txmm4, xmm5", None),
        (|a| a.sse(Sse::Pand, Xmm(0), Xrm::X(Xmm(9))), &[0x66, 0x41, 0x0f, 0xdb, 0xc1], "pand\txmm0, xmm9", None),
        (|a| a.sse(Sse::Pandn, Xmm(1), Xrm::X(Xmm(0))), &[0x66, 0x0f, 0xdf, 0xc8], "pandn\txmm1, xmm0", None),
        (|a| a.sse(Sse::Por, Xmm(0), Xrm::X(Xmm(1))), &[0x66, 0x0f, 0xeb, 0xc1], "por\txmm0, xmm1", None),
        (|a| a.sse(Sse::Pxor, Xmm(15), Xrm::X(Xmm(15))), &[0x66, 0x45, 0x0f, 0xef, 0xff], "pxor\txmm15, xmm15", None),
        (|a| a.sse(Sse::Pcmpeqb, Xmm(2), Xrm::X(Xmm(3))), &[0x66, 0x0f, 0x74, 0xd3], "pcmpeqb\txmm2, xmm3", None),
        (|a| a.sse(Sse::Pcmpeqw, Xmm(2), Xrm::X(Xmm(3))), &[0x66, 0x0f, 0x75, 0xd3], "pcmpeqw\txmm2, xmm3", None),
        (|a| a.sse(Sse::Pcmpeqd, Xmm(2), Xrm::X(Xmm(3))), &[0x66, 0x0f, 0x76, 0xd3], "pcmpeqd\txmm2, xmm3", None),
        (|a| a.sse(Sse::Pcmpgtb, Xmm(2), Xrm::X(Xmm(3))), &[0x66, 0x0f, 0x64, 0xd3], "pcmpgtb\txmm2, xmm3", None),
        (|a| a.sse(Sse::Pcmpgtw, Xmm(2), Xrm::X(Xmm(3))), &[0x66, 0x0f, 0x65, 0xd3], "pcmpgtw\txmm2, xmm3", None),
        (|a| a.sse(Sse::Pcmpgtd, Xmm(2), Xrm::X(Xmm(3))), &[0x66, 0x0f, 0x66, 0xd3], "pcmpgtd\txmm2, xmm3", None),
        (|a| a.sse(Sse::Pminub, Xmm(2), Xrm::X(Xmm(3))), &[0x66, 0x0f, 0xda, 0xd3], "pminub\txmm2, xmm3", None),
        (|a| a.sse(Sse::Pmaxub, Xmm(2), Xrm::X(Xmm(3))), &[0x66, 0x0f, 0xde, 0xd3], "pmaxub\txmm2, xmm3", None),
        (|a| a.sse(Sse::Pminsw, Xmm(2), Xrm::X(Xmm(3))), &[0x66, 0x0f, 0xea, 0xd3], "pminsw\txmm2, xmm3", None),
        (|a| a.sse(Sse::Pmaxsw, Xmm(2), Xrm::X(Xmm(3))), &[0x66, 0x0f, 0xee, 0xd3], "pmaxsw\txmm2, xmm3", None),
        (|a| a.sse(Sse::Minps, Xmm(2), Xrm::X(Xmm(3))), &[0x0f, 0x5d, 0xd3], "minps\txmm2, xmm3", None),
        (|a| a.sse(Sse::Minpd, Xmm(2), Xrm::X(Xmm(3))), &[0x66, 0x0f, 0x5d, 0xd3], "minpd\txmm2, xmm3", None),
        (|a| a.sse(Sse::Maxps, Xmm(2), Xrm::X(Xmm(3))), &[0x0f, 0x5f, 0xd3], "maxps\txmm2, xmm3", None),
        (|a| a.sse(Sse::Maxpd, Xmm(2), Xrm::X(Xmm(3))), &[0x66, 0x0f, 0x5f, 0xd3], "maxpd\txmm2, xmm3", None),
        (|a| a.sse(Sse::Pmulhw, Xmm(2), Xrm::X(Xmm(3))), &[0x66, 0x0f, 0xe5, 0xd3], "pmulhw\txmm2, xmm3", None),
        (|a| a.sse(Sse::Pmulhuw, Xmm(2), Xrm::X(Xmm(3))), &[0x66, 0x0f, 0xe4, 0xd3], "pmulhuw\txmm2, xmm3", None),
        (|a| a.sse_ri(Sse::Pshufd, Xmm(1), Xmm(2), 8), &[0x66, 0x0f, 0x70, 0xca, 0x08], "pshufd\txmm1, xmm2, 0x8", None),
        (|a| a.sse_ri(Sse::Pshufd, Xmm(9), Xmm(9), 0xf5), &[0x66, 0x45, 0x0f, 0x70, 0xc9, 0xf5], "pshufd\txmm9, xmm9, 0xf5", None),
        (|a| a.cmpp(false, FCmp::Neq, Xmm(0), Xmm(1)), &[0x0f, 0xc2, 0xc1, 0x04], "cmpneqps\txmm0, xmm1", None),
        (|a| a.cmpp(true, FCmp::Lt, Xmm(0), Xmm(1)), &[0x66, 0x0f, 0xc2, 0xc1, 0x01], "cmpltpd\txmm0, xmm1", None),
        (|a| a.cmpp(false, FCmp::Eq, Xmm(10), Xmm(1)), &[0x44, 0x0f, 0xc2, 0xd1, 0x00], "cmpeqps\txmm10, xmm1", None),
        (|a| a.cmpp(true, FCmp::Le, Xmm(0), Xmm(11)), &[0x66, 0x41, 0x0f, 0xc2, 0xc3, 0x02], "cmplepd\txmm0, xmm11", None),
        (|a| a.pshift(PShift::Psrlw, Xmm(2), 7), &[0x66, 0x0f, 0x71, 0xd2, 0x07], "psrlw\txmm2, 0x7", None),
        (|a| a.pshift(PShift::Psraw, Xmm(2), 15), &[0x66, 0x0f, 0x71, 0xe2, 0x0f], "psraw\txmm2, 0xf", None),
        (|a| a.pshift(PShift::Psllw, Xmm(15), 8), &[0x66, 0x41, 0x0f, 0x71, 0xf7, 0x08], "psllw\txmm15, 0x8", None),
        (|a| a.pshift(PShift::Psrld, Xmm(2), 1), &[0x66, 0x0f, 0x72, 0xd2, 0x01], "psrld\txmm2, 0x1", None),
        (|a| a.pshift(PShift::Psrad, Xmm(4), 31), &[0x66, 0x0f, 0x72, 0xe4, 0x1f], "psrad\txmm4, 0x1f", None),
        (|a| a.pshift(PShift::Pslld, Xmm(4), 16), &[0x66, 0x0f, 0x72, 0xf4, 0x10], "pslld\txmm4, 0x10", None),
        (|a| a.pshift(PShift::Psrlq, Xmm(2), 3), &[0x66, 0x0f, 0x73, 0xd2, 0x03], "psrlq\txmm2, 0x3", None),
        (|a| a.pshift(PShift::Psllq, Xmm(2), 3), &[0x66, 0x0f, 0x73, 0xf2, 0x03], "psllq\txmm2, 0x3", None),
        (|a| a.movq_xg(Sz::Q, Xmm(1), Gpr(0)), &[0x66, 0x48, 0x0f, 0x6e, 0xc8], "movq\txmm1, rax", None),
        (|a| a.movq_xg(Sz::D, Xmm(1), Gpr(0)), &[0x66, 0x0f, 0x6e, 0xc8], "movd\txmm1, eax", None),
        (|a| a.movq_xg(Sz::Q, Xmm(12), Gpr(9)), &[0x66, 0x4d, 0x0f, 0x6e, 0xe1], "movq\txmm12, r9", None),
        (|a| a.movq_gx(Sz::Q, Gpr(0), Xmm(1)), &[0x66, 0x48, 0x0f, 0x7e, 0xc8], "movq\trax, xmm1", None),
        (|a| a.movq_gx(Sz::D, Gpr(0), Xmm(1)), &[0x66, 0x0f, 0x7e, 0xc8], "movd\teax, xmm1", None),
        (|a| a.movq_gx(Sz::Q, Gpr(11), Xmm(9)), &[0x66, 0x4d, 0x0f, 0x7e, 0xcb], "movq\tr11, xmm9", None),
        (|a| a.cvtsi2f(true, Xmm(0), Gpr(0)), &[0xf2, 0x48, 0x0f, 0x2a, 0xc0], "cvtsi2sd\txmm0, rax", None),
        (|a| a.cvtsi2f(false, Xmm(0), Gpr(1)), &[0xf3, 0x48, 0x0f, 0x2a, 0xc1], "cvtsi2ss\txmm0, rcx", None),
        (|a| a.cvttf2si(true, Gpr(0), Xmm(0)), &[0xf2, 0x48, 0x0f, 0x2c, 0xc0], "cvttsd2si\trax, xmm0", None),
        (|a| a.cvttf2si(false, Gpr(0), Xmm(0)), &[0xf3, 0x48, 0x0f, 0x2c, 0xc0], "cvttss2si\trax, xmm0", None),
        (|a| a.movmsk(Avx::Pmovmskb, Gpr(2), Xmm(3)), &[0x66, 0x0f, 0xd7, 0xd3], "pmovmskb\tedx, xmm3", None),
        (|a| a.movmsk(Avx::Movmskps, Gpr(2), Xmm(3)), &[0x0f, 0x50, 0xd3], "movmskps\tedx, xmm3", None),
        (|a| a.movmsk(Avx::Movmskpd, Gpr(2), Xmm(11)), &[0x66, 0x41, 0x0f, 0x50, 0xd3], "movmskpd\tedx, xmm11", None),
        (|a| a.vex_rrr(Avx::Paddd, true, Xmm(0), Xmm(1), Xmm(2)), &[0xc5, 0xf5, 0xfe, 0xc2], "vpaddd\tymm0, ymm1, ymm2", None),
        (|a| a.vex_rrr(Avx::Vfmadd231sd, false, Xmm(0), Xmm(1), Xmm(2)), &[0xc4, 0xe2, 0xf1, 0xb9, 0xc2], "vfmadd231sd\txmm0, xmm1, xmm2", None),
        (|a| a.vex_rrr(Avx::Vfmadd231ss, false, Xmm(9), Xmm(1), Xmm(12)), &[0xc4, 0x42, 0x71, 0xb9, 0xcc], "vfmadd231ss\txmm9, xmm1, xmm12", None),
        (|a| a.vex_rrr(Avx::Vfmadd231ps, true, Xmm(3), Xmm(4), Xmm(5)), &[0xc4, 0xe2, 0x5d, 0xb8, 0xdd], "vfmadd231ps\tymm3, ymm4, ymm5", None),
        (|a| a.vex_rrr(Avx::Vfmadd231pd, false, Xmm(0), Xmm(1), Xmm(2)), &[0xc4, 0xe2, 0xf1, 0xb8, 0xc2], "vfmadd231pd\txmm0, xmm1, xmm2", None),
        (|a| a.vex_rrr(Avx::Paddq, true, Xmm(0), Xmm(0), Xmm(1)), &[0xc5, 0xfd, 0xd4, 0xc1], "vpaddq\tymm0, ymm0, ymm1", None),
        (|a| a.vex_rrr(Avx::Paddb, true, Xmm(8), Xmm(9), Xmm(10)), &[0xc4, 0x41, 0x35, 0xfc, 0xc2], "vpaddb\tymm8, ymm9, ymm10", None),
        (|a| a.vex_rrr(Avx::Paddw, true, Xmm(0), Xmm(1), Xmm(2)), &[0xc5, 0xf5, 0xfd, 0xc2], "vpaddw\tymm0, ymm1, ymm2", None),
        (|a| a.vex_rrr(Avx::Psubb, true, Xmm(0), Xmm(1), Xmm(2)), &[0xc5, 0xf5, 0xf8, 0xc2], "vpsubb\tymm0, ymm1, ymm2", None),
        (|a| a.vex_rrr(Avx::Psubw, true, Xmm(0), Xmm(1), Xmm(2)), &[0xc5, 0xf5, 0xf9, 0xc2], "vpsubw\tymm0, ymm1, ymm2", None),
        (|a| a.vex_rrr(Avx::Psubd, true, Xmm(0), Xmm(1), Xmm(2)), &[0xc5, 0xf5, 0xfa, 0xc2], "vpsubd\tymm0, ymm1, ymm2", None),
        (|a| a.vex_rrr(Avx::Psubq, true, Xmm(0), Xmm(1), Xmm(2)), &[0xc5, 0xf5, 0xfb, 0xc2], "vpsubq\tymm0, ymm1, ymm2", None),
        (|a| a.vex_rrr(Avx::Pmullw, true, Xmm(0), Xmm(1), Xmm(2)), &[0xc5, 0xf5, 0xd5, 0xc2], "vpmullw\tymm0, ymm1, ymm2", None),
        (|a| a.vex_rrr(Avx::Pmulld, true, Xmm(0), Xmm(0), Xmm(1)), &[0xc4, 0xe2, 0x7d, 0x40, 0xc1], "vpmulld\tymm0, ymm0, ymm1", None),
        (|a| a.vex_rrr(Avx::Pand, true, Xmm(0), Xmm(1), Xmm(2)), &[0xc5, 0xf5, 0xdb, 0xc2], "vpand\tymm0, ymm1, ymm2", None),
        (|a| a.vex_rrr(Avx::Pandn, true, Xmm(1), Xmm(1), Xmm(0)), &[0xc5, 0xf5, 0xdf, 0xc8], "vpandn\tymm1, ymm1, ymm0", None),
        (|a| a.vex_rrr(Avx::Por, true, Xmm(0), Xmm(1), Xmm(2)), &[0xc5, 0xf5, 0xeb, 0xc2], "vpor\tymm0, ymm1, ymm2", None),
        (|a| a.vex_rrr(Avx::Pxor, true, Xmm(8), Xmm(8), Xmm(8)), &[0xc4, 0x41, 0x3d, 0xef, 0xc0], "vpxor\tymm8, ymm8, ymm8", None),
        (|a| a.vex_rrr(Avx::Pcmpeqb, true, Xmm(0), Xmm(1), Xmm(2)), &[0xc5, 0xf5, 0x74, 0xc2], "vpcmpeqb\tymm0, ymm1, ymm2", None),
        (|a| a.vex_rrr(Avx::Pcmpeqw, true, Xmm(0), Xmm(1), Xmm(2)), &[0xc5, 0xf5, 0x75, 0xc2], "vpcmpeqw\tymm0, ymm1, ymm2", None),
        (|a| a.vex_rrr(Avx::Pcmpeqd, true, Xmm(3), Xmm(3), Xmm(3)), &[0xc5, 0xe5, 0x76, 0xdb], "vpcmpeqd\tymm3, ymm3, ymm3", None),
        (|a| a.vex_rrr(Avx::Pcmpeqq, true, Xmm(0), Xmm(1), Xmm(2)), &[0xc4, 0xe2, 0x75, 0x29, 0xc2], "vpcmpeqq\tymm0, ymm1, ymm2", None),
        (|a| a.vex_rrr(Avx::Pcmpgtb, true, Xmm(4), Xmm(15), Xmm(4)), &[0xc5, 0x85, 0x64, 0xe4], "vpcmpgtb\tymm4, ymm15, ymm4", None),
        (|a| a.vex_rrr(Avx::Pcmpgtw, true, Xmm(0), Xmm(1), Xmm(2)), &[0xc5, 0xf5, 0x65, 0xc2], "vpcmpgtw\tymm0, ymm1, ymm2", None),
        (|a| a.vex_rrr(Avx::Pcmpgtd, true, Xmm(0), Xmm(1), Xmm(2)), &[0xc5, 0xf5, 0x66, 0xc2], "vpcmpgtd\tymm0, ymm1, ymm2", None),
        (|a| a.vex_rrr(Avx::Pcmpgtq, true, Xmm(2), Xmm(2), Xmm(3)), &[0xc4, 0xe2, 0x6d, 0x37, 0xd3], "vpcmpgtq\tymm2, ymm2, ymm3", None),
        (|a| a.vex_rrr(Avx::Pminub, true, Xmm(0), Xmm(1), Xmm(2)), &[0xc5, 0xf5, 0xda, 0xc2], "vpminub\tymm0, ymm1, ymm2", None),
        (|a| a.vex_rrr(Avx::Pminuw, true, Xmm(0), Xmm(1), Xmm(2)), &[0xc4, 0xe2, 0x75, 0x3a, 0xc2], "vpminuw\tymm0, ymm1, ymm2", None),
        (|a| a.vex_rrr(Avx::Pminud, true, Xmm(0), Xmm(1), Xmm(2)), &[0xc4, 0xe2, 0x75, 0x3b, 0xc2], "vpminud\tymm0, ymm1, ymm2", None),
        (|a| a.vex_rrr(Avx::Pmaxub, true, Xmm(0), Xmm(1), Xmm(2)), &[0xc5, 0xf5, 0xde, 0xc2], "vpmaxub\tymm0, ymm1, ymm2", None),
        (|a| a.vex_rrr(Avx::Pmaxuw, true, Xmm(0), Xmm(1), Xmm(2)), &[0xc4, 0xe2, 0x75, 0x3e, 0xc2], "vpmaxuw\tymm0, ymm1, ymm2", None),
        (|a| a.vex_rrr(Avx::Pmaxud, true, Xmm(0), Xmm(1), Xmm(2)), &[0xc4, 0xe2, 0x75, 0x3f, 0xc2], "vpmaxud\tymm0, ymm1, ymm2", None),
        (|a| a.vex_rrr(Avx::Pminsb, true, Xmm(0), Xmm(1), Xmm(2)), &[0xc4, 0xe2, 0x75, 0x38, 0xc2], "vpminsb\tymm0, ymm1, ymm2", None),
        (|a| a.vex_rrr(Avx::Pminsw, true, Xmm(0), Xmm(1), Xmm(2)), &[0xc5, 0xf5, 0xea, 0xc2], "vpminsw\tymm0, ymm1, ymm2", None),
        (|a| a.vex_rrr(Avx::Pminsd, true, Xmm(2), Xmm(2), Xmm(3)), &[0xc4, 0xe2, 0x6d, 0x39, 0xd3], "vpminsd\tymm2, ymm2, ymm3", None),
        (|a| a.vex_rrr(Avx::Pmaxsb, true, Xmm(0), Xmm(1), Xmm(2)), &[0xc4, 0xe2, 0x75, 0x3c, 0xc2], "vpmaxsb\tymm0, ymm1, ymm2", None),
        (|a| a.vex_rrr(Avx::Pmaxsw, true, Xmm(0), Xmm(1), Xmm(2)), &[0xc5, 0xf5, 0xee, 0xc2], "vpmaxsw\tymm0, ymm1, ymm2", None),
        (|a| a.vex_rrr(Avx::Minps, true, Xmm(0), Xmm(1), Xmm(2)), &[0xc5, 0xf4, 0x5d, 0xc2], "vminps\tymm0, ymm1, ymm2", None),
        (|a| a.vex_rrr(Avx::Minpd, true, Xmm(0), Xmm(1), Xmm(2)), &[0xc5, 0xf5, 0x5d, 0xc2], "vminpd\tymm0, ymm1, ymm2", None),
        (|a| a.vex_rrr(Avx::Maxps, true, Xmm(0), Xmm(1), Xmm(2)), &[0xc5, 0xf4, 0x5f, 0xc2], "vmaxps\tymm0, ymm1, ymm2", None),
        (|a| a.vex_rrr(Avx::Maxpd, true, Xmm(0), Xmm(1), Xmm(2)), &[0xc5, 0xf5, 0x5f, 0xc2], "vmaxpd\tymm0, ymm1, ymm2", None),
        (|a| a.vex_rrr(Avx::Pmulhw, true, Xmm(0), Xmm(1), Xmm(2)), &[0xc5, 0xf5, 0xe5, 0xc2], "vpmulhw\tymm0, ymm1, ymm2", None),
        (|a| a.vex_rrr(Avx::Pmulhuw, true, Xmm(0), Xmm(1), Xmm(2)), &[0xc5, 0xf5, 0xe4, 0xc2], "vpmulhuw\tymm0, ymm1, ymm2", None),
        (|a| a.vex_rrr(Avx::Pmaxsd, true, Xmm(0), Xmm(1), Xmm(2)), &[0xc4, 0xe2, 0x75, 0x3d, 0xc2], "vpmaxsd\tymm0, ymm1, ymm2", None),
        (|a| a.vex_rrr(Avx::Packuswb, true, Xmm(15), Xmm(15), Xmm(15)), &[0xc4, 0x41, 0x05, 0x67, 0xff], "vpackuswb\tymm15, ymm15, ymm15", None),
        (|a| a.vex_rrr(Avx::Packssdw, true, Xmm(15), Xmm(15), Xmm(15)), &[0xc4, 0x41, 0x05, 0x6b, 0xff], "vpackssdw\tymm15, ymm15, ymm15", None),
        (|a| a.vex_rrr(Avx::Addps, true, Xmm(0), Xmm(1), Xmm(2)), &[0xc5, 0xf4, 0x58, 0xc2], "vaddps\tymm0, ymm1, ymm2", None),
        (|a| a.vex_rrr(Avx::Addpd, true, Xmm(0), Xmm(1), Xmm(12)), &[0xc4, 0xc1, 0x75, 0x58, 0xc4], "vaddpd\tymm0, ymm1, ymm12", None),
        (|a| a.vex_rrr(Avx::Subps, true, Xmm(0), Xmm(1), Xmm(2)), &[0xc5, 0xf4, 0x5c, 0xc2], "vsubps\tymm0, ymm1, ymm2", None),
        (|a| a.vex_rrr(Avx::Subpd, true, Xmm(0), Xmm(1), Xmm(2)), &[0xc5, 0xf5, 0x5c, 0xc2], "vsubpd\tymm0, ymm1, ymm2", None),
        (|a| a.vex_rrr(Avx::Mulps, true, Xmm(0), Xmm(1), Xmm(2)), &[0xc5, 0xf4, 0x59, 0xc2], "vmulps\tymm0, ymm1, ymm2", None),
        (|a| a.vex_rrr(Avx::Mulpd, true, Xmm(0), Xmm(1), Xmm(2)), &[0xc5, 0xf5, 0x59, 0xc2], "vmulpd\tymm0, ymm1, ymm2", None),
        (|a| a.vex_rrr(Avx::Divps, true, Xmm(0), Xmm(1), Xmm(2)), &[0xc5, 0xf4, 0x5e, 0xc2], "vdivps\tymm0, ymm1, ymm2", None),
        (|a| a.vex_rrr(Avx::Divpd, true, Xmm(0), Xmm(1), Xmm(2)), &[0xc5, 0xf5, 0x5e, 0xc2], "vdivpd\tymm0, ymm1, ymm2", None),
        (|a| a.vex_rr(Avx::Sqrtps, true, Xmm(0), Xmm(1), false), &[0xc5, 0xfc, 0x51, 0xc1], "vsqrtps\tymm0, ymm1", None),
        (|a| a.vex_rr(Avx::Sqrtpd, true, Xmm(0), Xmm(1), false), &[0xc5, 0xfd, 0x51, 0xc1], "vsqrtpd\tymm0, ymm1", None),
        (|a| a.vex_rr(Avx::Pbroadcastb, true, Xmm(3), Xmm(3), true), &[0xc4, 0xe2, 0x7d, 0x78, 0xdb], "vpbroadcastb\tymm3, xmm3", None),
        (|a| a.vex_rr(Avx::Pbroadcastw, true, Xmm(3), Xmm(3), true), &[0xc4, 0xe2, 0x7d, 0x79, 0xdb], "vpbroadcastw\tymm3, xmm3", None),
        (|a| a.vex_rr(Avx::Pbroadcastd, true, Xmm(3), Xmm(3), true), &[0xc4, 0xe2, 0x7d, 0x58, 0xdb], "vpbroadcastd\tymm3, xmm3", None),
        (|a| a.vex_rr(Avx::Pbroadcastq, true, Xmm(9), Xmm(9), true), &[0xc4, 0x42, 0x7d, 0x59, 0xc9], "vpbroadcastq\tymm9, xmm9", None),
        (|a| a.vex_ld(Avx::Movdqu, true, Xmm(3), Addr::Bi(Gpr(6), Gpr(0), 0), Ptr::Y), &[0xc5, 0xfe, 0x6f, 0x1c, 0x06], "vmovdqu\tymm3, ymmword ptr [rsi + rax]", None),
        (|a| a.vex_ld(Avx::Movdqu, true, Xmm(15), Addr::Bi(Gpr(9), Gpr(0), 0), Ptr::Y), &[0xc4, 0x41, 0x7e, 0x6f, 0x3c, 0x01], "vmovdqu\tymm15, ymmword ptr [r9 + rax]", None),
        (|a| a.vex_ld(Avx::Movdqu, true, Xmm(1), Addr::Bi(Gpr(13), Gpr(0), 3), Ptr::Y), &[0xc4, 0xc1, 0x7e, 0x6f, 0x4c, 0xc5, 0x00], "vmovdqu\tymm1, ymmword ptr [r13 + 8*rax]", None),
        (|a| a.vex_ld(Avx::Pmovzxbd, true, Xmm(0), Addr::Bi(Gpr(6), Gpr(0), 1), Ptr::Q), &[0xc4, 0xe2, 0x7d, 0x31, 0x04, 0x46], "vpmovzxbd\tymm0, qword ptr [rsi + 2*rax]", None),
        (|a| a.vex_ld(Avx::Pmovzxbw, true, Xmm(0), Addr::Bi(Gpr(6), Gpr(0), 0), Ptr::X), &[0xc4, 0xe2, 0x7d, 0x30, 0x04, 0x06], "vpmovzxbw\tymm0, xmmword ptr [rsi + rax]", None),
        (|a| a.vex_ld(Avx::Pmovzxwd, true, Xmm(0), Addr::Bi(Gpr(6), Gpr(0), 0), Ptr::X), &[0xc4, 0xe2, 0x7d, 0x33, 0x04, 0x06], "vpmovzxwd\tymm0, xmmword ptr [rsi + rax]", None),
        (|a| a.vex_ld(Avx::Pmovzxwq, true, Xmm(0), Addr::Bi(Gpr(6), Gpr(0), 0), Ptr::Q), &[0xc4, 0xe2, 0x7d, 0x34, 0x04, 0x06], "vpmovzxwq\tymm0, qword ptr [rsi + rax]", None),
        (|a| a.vex_ld(Avx::Pmovzxdq, true, Xmm(0), Addr::Bi(Gpr(6), Gpr(0), 0), Ptr::X), &[0xc4, 0xe2, 0x7d, 0x35, 0x04, 0x06], "vpmovzxdq\tymm0, xmmword ptr [rsi + rax]", None),
        (|a| a.vex_ld(Avx::Pmovsxbw, true, Xmm(0), Addr::Bi(Gpr(6), Gpr(0), 0), Ptr::X), &[0xc4, 0xe2, 0x7d, 0x20, 0x04, 0x06], "vpmovsxbw\tymm0, xmmword ptr [rsi + rax]", None),
        (|a| a.vex_ld(Avx::Pmovsxbd, true, Xmm(0), Addr::Bi(Gpr(6), Gpr(0), 0), Ptr::Q), &[0xc4, 0xe2, 0x7d, 0x21, 0x04, 0x06], "vpmovsxbd\tymm0, qword ptr [rsi + rax]", None),
        (|a| a.vex_ld(Avx::Pmovsxwd, true, Xmm(0), Addr::Bi(Gpr(6), Gpr(0), 0), Ptr::X), &[0xc4, 0xe2, 0x7d, 0x23, 0x04, 0x06], "vpmovsxwd\tymm0, xmmword ptr [rsi + rax]", None),
        (|a| a.vex_ld(Avx::Pmovsxwq, true, Xmm(0), Addr::Bi(Gpr(6), Gpr(0), 0), Ptr::Q), &[0xc4, 0xe2, 0x7d, 0x24, 0x04, 0x06], "vpmovsxwq\tymm0, qword ptr [rsi + rax]", None),
        (|a| a.vex_ld(Avx::Pmovsxdq, true, Xmm(10), Addr::Bi(Gpr(11), Gpr(0), 2), Ptr::X), &[0xc4, 0x42, 0x7d, 0x25, 0x14, 0x83], "vpmovsxdq\tymm10, xmmword ptr [r11 + 4*rax]", None),
        (|a| a.vex_ld(Avx::Pbroadcastd, true, Xmm(0), Addr::Bd(Gpr(4), 0x10), Ptr::D), &[0xc4, 0xe2, 0x7d, 0x58, 0x44, 0x24, 0x10], "vpbroadcastd\tymm0, dword ptr [rsp + 0x10]", None),
        (|a| a.vex_ld(Avx::Pbroadcastb, true, Xmm(0), Addr::Bd(Gpr(4), 0), Ptr::B), &[0xc4, 0xe2, 0x7d, 0x78, 0x44, 0x24, 0x00], "vpbroadcastb\tymm0, byte ptr [rsp]", None),
        (|a| a.vex_ld(Avx::Pbroadcastw, true, Xmm(0), Addr::Bd(Gpr(4), 2), Ptr::W), &[0xc4, 0xe2, 0x7d, 0x79, 0x44, 0x24, 0x02], "vpbroadcastw\tymm0, word ptr [rsp + 0x2]", None),
        (|a| a.vex_ld(Avx::Pbroadcastq, true, Xmm(1), Addr::Bd(Gpr(4), 0x200), Ptr::Q), &[0xc4, 0xe2, 0x7d, 0x59, 0x8c, 0x24, 0x00, 0x02, 0x00, 0x00], "vpbroadcastq\tymm1, qword ptr [rsp + 0x200]", None),
        (|a| a.vex_st(Avx::MovdquSt, true, Addr::Bi(Gpr(7), Gpr(0), 2), Xmm(1)), &[0xc5, 0xfe, 0x7f, 0x0c, 0x87], "vmovdqu\tymmword ptr [rdi + 4*rax], ymm1", None),
        (|a| a.vex_st(Avx::MovdquSt, false, Addr::Bi(Gpr(7), Gpr(0), 2), Xmm(15)), &[0xc5, 0x7a, 0x7f, 0x3c, 0x87], "vmovdqu\txmmword ptr [rdi + 4*rax], xmm15", None),
        (|a| a.vex_st(Avx::MovdquSt, true, Addr::Bd(Gpr(4), 0x40), Xmm(2)), &[0xc5, 0xfe, 0x7f, 0x54, 0x24, 0x40], "vmovdqu\tymmword ptr [rsp + 0x40], ymm2", None),
        (|a| a.vex_st(Avx::MovdquSt, true, Addr::Bd(Gpr(4), 0), Xmm(2)), &[0xc5, 0xfe, 0x7f, 0x54, 0x24, 0x00], "vmovdqu\tymmword ptr [rsp], ymm2", None),
        (|a| a.vex_st(Avx::MovdquSt, true, Addr::Bi(Gpr(13), Gpr(0), 0), Xmm(9)), &[0xc4, 0x41, 0x7e, 0x7f, 0x4c, 0x05, 0x00], "vmovdqu\tymmword ptr [r13 + rax], ymm9", None),
        (|a| a.vex_maskst(Avx::Pmaskmovd, true, Addr::Bi(Gpr(6), Gpr(0), 0), Xmm(2), Xmm(1)), &[0xc4, 0xe2, 0x6d, 0x8e, 0x0c, 0x06], "vpmaskmovd\tymmword ptr [rsi + rax], ymm2, ymm1", None),
        (|a| a.vex_maskst(Avx::Pmaskmovq, true, Addr::Bi(Gpr(8), Gpr(0), 3), Xmm(3), Xmm(4)), &[0xc4, 0xc2, 0xe5, 0x8e, 0x24, 0xc0], "vpmaskmovq\tymmword ptr [r8 + 8*rax], ymm3, ymm4", None),
        (|a| a.vex_ri(Avx::Permq, true, Xmm(15), Xmm(15), 8), &[0xc4, 0x43, 0xfd, 0x00, 0xff, 0x08], "vpermq\tymm15, ymm15, 0x8", None),
        (|a| a.vex_ri(Avx::Pshufd, true, Xmm(15), Xmm(1), 8), &[0xc5, 0x7d, 0x70, 0xf9, 0x08], "vpshufd\tymm15, ymm1, 0x8", None),
        (|a| a.vex_cmpp(false, true, FCmp::Lt, Xmm(0), Xmm(1), Xmm(2)), &[0xc5, 0xf4, 0xc2, 0xc2, 0x01], "vcmpltps\tymm0, ymm1, ymm2", None),
        (|a| a.vex_cmpp(true, true, FCmp::Neq, Xmm(3), Xmm(4), Xmm(5)), &[0xc5, 0xdd, 0xc2, 0xdd, 0x04], "vcmpneqpd\tymm3, ymm4, ymm5", None),
        (|a| a.vex_cmpp(true, true, FCmp::Eq, Xmm(3), Xmm(4), Xmm(5)), &[0xc5, 0xdd, 0xc2, 0xdd, 0x00], "vcmpeqpd\tymm3, ymm4, ymm5", None),
        (|a| a.vex_cmpp(false, true, FCmp::Le, Xmm(3), Xmm(4), Xmm(5)), &[0xc5, 0xdc, 0xc2, 0xdd, 0x02], "vcmpleps\tymm3, ymm4, ymm5", None),
        (|a| a.vex_shift(PShift::Pslld, true, Xmm(15), Xmm(1), 16), &[0xc5, 0x85, 0x72, 0xf1, 0x10], "vpslld\tymm15, ymm1, 0x10", None),
        (|a| a.vex_shift(PShift::Psrlw, true, Xmm(15), Xmm(15), 8), &[0xc4, 0xc1, 0x05, 0x71, 0xd7, 0x08], "vpsrlw\tymm15, ymm15, 0x8", None),
        (|a| a.vex_shift(PShift::Psllw, true, Xmm(15), Xmm(1), 8), &[0xc5, 0x85, 0x71, 0xf1, 0x08], "vpsllw\tymm15, ymm1, 0x8", None),
        (|a| a.vex_shift(PShift::Psrad, true, Xmm(4), Xmm(4), 31), &[0xc5, 0xdd, 0x72, 0xe4, 0x1f], "vpsrad\tymm4, ymm4, 0x1f", None),
        (|a| a.vex_shift(PShift::Psraw, true, Xmm(4), Xmm(4), 15), &[0xc5, 0xdd, 0x71, 0xe4, 0x0f], "vpsraw\tymm4, ymm4, 0xf", None),
        (|a| a.vex_shift(PShift::Psrld, true, Xmm(4), Xmm(4), 1), &[0xc5, 0xdd, 0x72, 0xd4, 0x01], "vpsrld\tymm4, ymm4, 0x1", None),
        (|a| a.vex_shift(PShift::Psllq, true, Xmm(2), Xmm(2), 3), &[0xc5, 0xed, 0x73, 0xf2, 0x03], "vpsllq\tymm2, ymm2, 0x3", None),
        (|a| a.vex_shift(PShift::Psrlq, true, Xmm(2), Xmm(10), 3), &[0xc4, 0xc1, 0x6d, 0x73, 0xd2, 0x03], "vpsrlq\tymm2, ymm10, 0x3", None),
        (|a| a.vmovq_xg(Xmm(0), Gpr(2)), &[0xc4, 0xe1, 0xf9, 0x6e, 0xc2], "vmovq\txmm0, rdx", None),
        (|a| a.vmovq_xg(Xmm(9), Gpr(10)), &[0xc4, 0x41, 0xf9, 0x6e, 0xca], "vmovq\txmm9, r10", None),
        (|a| a.vex_movmsk(Avx::Pmovmskb, true, Gpr(2), Xmm(3)), &[0xc5, 0xfd, 0xd7, 0xd3], "vpmovmskb\tedx, ymm3", None),
        (|a| a.vex_movmsk(Avx::Movmskps, true, Gpr(2), Xmm(3)), &[0xc5, 0xfc, 0x50, 0xd3], "vmovmskps\tedx, ymm3", None),
        (|a| a.vex_movmsk(Avx::Movmskpd, true, Gpr(2), Xmm(13)), &[0xc4, 0xc1, 0x7d, 0x50, 0xd5], "vmovmskpd\tedx, ymm13", None),
        (|a| a.vzeroupper(), &[0xc5, 0xf8, 0x77], "vzeroupper", None),
        (|a| a.mov_mi(Sz::Q, Rm::m(4, 32), 0), &[0x48, 0xc7, 0x44, 0x24, 0x20, 0x00, 0x00, 0x00, 0x00], "mov\tqword ptr [rsp + 0x20], 0x0", None),
        (|a| a.mov_mi(Sz::D, Rm::m(0, 0), -1), &[0xc7, 0x00, 0xff, 0xff, 0xff, 0xff], "mov\tdword ptr [rax], 0xffffffff", None),
        (|a| a.mov_mi(Sz::Q, Rm::r(9), -2), &[0x49, 0xc7, 0xc1, 0xfe, 0xff, 0xff, 0xff], "mov\tr9, -0x2", None),
        (|a| a.alu_rax(Alu::Cmp, 0x1000), &[0x48, 0x3d, 0x00, 0x10, 0x00, 0x00], "cmp\trax, 0x1000", None),
        (|a| a.alu_rax(Alu::Sub, 0x1000), &[0x48, 0x2d, 0x00, 0x10, 0x00, 0x00], "sub\trax, 0x1000", None),
        (|a| a.test_mr(Sz::Q, Addr::Bd(Gpr(1), 0), Gpr(1)), &[0x48, 0x85, 0x09], "test\tqword ptr [rcx], rcx", None),
        (|a| a.test_mr(Sz::D, Addr::Bd(Gpr(12), 8), Gpr(9)), &[0x45, 0x85, 0x4c, 0x24, 0x08], "test\tdword ptr [r12 + 0x8], r9d", None),
        (|a| { a.jcc8(Cond::B, None); }, &[0x72, 0x00], "jb\t0x2", None),
        (|a| { a.jcc8(Cond::A, Some(0)); }, &[0x77, 0xfe], "ja\t0x0", None),
        (|a| a.push(Gpr(1)), &[0x51], "push\trcx", None),
        (|a| a.push(Gpr(12)), &[0x41, 0x54], "push\tr12", None),
        (|a| a.pop(Gpr(0)), &[0x58], "pop\trax", None),
        (|a| a.pop(Gpr(15)), &[0x41, 0x5f], "pop\tr15", None),
        (|a| a.int3(), &[0xcc], "int3", None),
    ]
}

/// `text` without objdump's `<..>` annotations.
fn bare(text: &str) -> String {
    match text.find(" <") {
        Some(i) => text[..i].to_owned(),
        None => text.to_owned(),
    }
}

#[test]
fn forms_match_the_reference_assembler() {
    for (n, (f, bytes, text, rel)) in cases().into_iter().enumerate() {
        let mut a = Asm::new(true);
        f(&mut a);
        assert_eq!(a.code, bytes, "case {n}: {text}");
        let l = Listing {
            name: "f",
            code: &a.code,
            labels: Vec::new(),
            relocs: &a.relocs,
        };
        let lines = a.lines(&l);
        assert_eq!(lines.len(), 1, "case {n}: {text}");
        assert_eq!(bare(&lines[0].1), text, "case {n}");
        let relocs: Vec<_> = a.relocs.iter().map(|r| (r.0, r.1)).collect();
        assert_eq!(
            relocs,
            rel.into_iter().collect::<Vec<_>>(),
            "case {n}: {text}"
        );
        // Text off: the same bytes, nothing recorded.
        let mut b = Asm::new(false);
        f(&mut b);
        assert_eq!(b.code, bytes);
        assert!(b.lines(&l).is_empty());
    }
}

/// A listing names labels, branch targets and relocations.
#[test]
fn listing_shows_labels_targets_and_relocations() {
    let mut obj = crate::obj::Obj::new(true, true, false);
    let f = obj.sym("f");
    let g = obj.sym("g");
    let mut a = Asm::new(true);
    a.push_rbp();
    let top = a.pos();
    a.call(RelKind::X86Call, g);
    a.jcc(Cond::NE, Some(top));
    let l = Listing {
        name: "f",
        code: &a.code,
        labels: vec![(top as u32, 3)],
        relocs: &a.relocs,
    };
    let lines = a.lines(&l);
    crate::asm::function(&mut obj, &l, &lines, crate::asm::Bytes::Each);
    let _ = f;
    let want = "
<f>:
       0: 55                           \tpush\trbp
.L3:
       1: e8 00 00 00 00               \tcall\t0x6 <f+0x6>
\t\t0000000000000002:  R_X86_64_PLT32\tg-0x4
       6: 0f 85 f5 ff ff ff            \tjne\t0x1 <.L3>
";
    assert_eq!(obj.asm.as_deref(), Some(want));
}

/// As source: local labels, relocations as expressions, a long branch
/// forced, a form no text gives as its bytes.
#[test]
fn source_shows_labels_and_relocations() {
    let mut obj = crate::obj::Obj::new(true, true, false);
    obj.asm = Some(String::new());
    obj.src = Some(Vec::new());
    let g = obj.sym("g");
    let h = obj.sym("h");
    let mut a = Asm::new(true);
    a.push_rbp();
    let top = a.pos();
    a.call(RelKind::X86Call, g);
    a.mov(Sz::Q, RAX, Rm::M(Addr::Rip(RelKind::X86GotLoad, h)));
    a.alu_ri(Alu::Sub, Sz::Q, Rm::r(4), 0x10);
    a.jcc(Cond::NE, Some(top));
    let l = Listing {
        name: "f",
        code: &a.code,
        labels: vec![(top as u32, 3)],
        relocs: &a.relocs,
    };
    let lines = a.lines(&l);
    crate::asm::function(&mut obj, &l, &lines, crate::asm::Bytes::Each);
    let want = "\tpush\trbp
.LBB0_3:
\tcall\tg@PLT
\tmov\trax, qword ptr [rip + h@GOTPCREL]
\t.byte\t0x48, 0x81, 0xec, 0x10, 0x00, 0x00, 0x00\t# sub rsp, 0x10
\t{disp32} jne\t.LBB0_3
";
    let src = obj.src.as_deref().unwrap_or_default();
    assert_eq!(src.len(), 1);
    assert_eq!(
        (src[0].name.as_str(), src[0].len),
        ("f", a.code.len() as u32)
    );
    assert_eq!(src[0].text, want);
    assert_eq!(obj.asm.as_deref(), Some(""));
}
