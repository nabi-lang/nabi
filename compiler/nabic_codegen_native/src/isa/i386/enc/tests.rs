//! Each form against the reference: the bytes GNU as (`--32`) writes for
//! it (or, for a form it would encode otherwise, the bytes by hand), the
//! text `llvm-objdump -d -M intel` prints for them (`<..>` annotations
//! aside) and the relocated field.

use crate::asm::Listing;
use crate::isa::i386::enc::*;

type Case = (
    fn(&mut Asm),
    &'static [u8],
    &'static str,
    Option<(u32, RelKind)>,
);

const fn g(n: u32) -> Gpr {
    Gpr(n)
}

const fn r(n: u32) -> Rm {
    Rm::R(Gpr(n))
}

const fn bd(base: u32, disp: i32) -> Addr {
    Addr::Bd(Gpr(base), disp)
}

const fn m(base: u32, disp: i32) -> Rm {
    Rm::M(bd(base, disp))
}

const fn x(n: u32) -> Xmm {
    Xmm(n)
}

const fn xr(n: u32) -> Xrm {
    Xrm::X(Xmm(n))
}

const fn xm(base: u32, disp: i32) -> Xrm {
    Xrm::M(bd(base, disp))
}

#[rustfmt::skip]
fn cases() -> Vec<Case> {
    vec![
        (|a| a.mov(Sz::D, g(0), r(1)), &[0x8b, 0xc1], "mov\teax, ecx", None),
        (|a| a.mov(Sz::D, g(7), m(4, 8)), &[0x8b, 0x7c, 0x24, 0x08], "mov\tedi, dword ptr [esp + 0x8]", None),
        (|a| a.mov(Sz::D, g(0), m(5, 0)), &[0x8b, 0x45, 0x00], "mov\teax, dword ptr [ebp]", None),
        (|a| a.mov(Sz::D, g(2), m(6, -0x200)), &[0x8b, 0x96, 0x00, 0xfe, 0xff, 0xff], "mov\tedx, dword ptr [esi - 0x200]", None),
        (|a| a.mov(Sz::D, g(1), m(0, 0)), &[0x8b, 0x08], "mov\tecx, dword ptr [eax]", None),
        (|a| a.mov(Sz::D, g(3), m(4, -128)), &[0x8b, 0x5c, 0x24, 0x80], "mov\tebx, dword ptr [esp - 0x80]", None),
        (|a| a.mov(Sz::D, g(6), m(4, 0x80)), &[0x8b, 0xb4, 0x24, 0x80, 0x00, 0x00, 0x00], "mov\tesi, dword ptr [esp + 0x80]", None),
        (|a| a.mov(Sz::B, g(0), m(1, 0)), &[0x8a, 0x01], "mov\tal, byte ptr [ecx]", None),
        (|a| a.mov(Sz::W, g(1), m(4, 8)), &[0x66, 0x8b, 0x4c, 0x24, 0x08], "mov\tcx, word ptr [esp + 0x8]", None),
        (|a| a.mov(Sz::D, g(0), Rm::M(Addr::BdRel(g(3), RelKind::I386Got32X, 0))), &[0x8b, 0x83, 0x00, 0x00, 0x00, 0x00], "mov\teax, dword ptr [ebx]", Some((2, RelKind::I386Got32X))),
        (|a| a.mov(Sz::D, g(5), Rm::M(Addr::BdRel(g(4), RelKind::I386GotOff, 0))), &[0x8b, 0xac, 0x24, 0x00, 0x00, 0x00, 0x00], "mov\tebp, dword ptr [esp]", Some((3, RelKind::I386GotOff))),
        (|a| a.mov(Sz::D, g(0), Rm::M(Addr::Abs(RelKind::I386Abs32, 0))), &[0x8b, 0x05, 0x00, 0x00, 0x00, 0x00], "mov\teax, dword ptr [0x0]", Some((2, RelKind::I386Abs32))),
        (|a| a.mov(Sz::D, g(1), Rm::M(Addr::Seg(Seg::Fs, 0x2c))), &[0x64, 0x8b, 0x0d, 0x2c, 0x00, 0x00, 0x00], "mov\tecx, dword ptr fs:[0x2c]", None),
        (|a| a.mov(Sz::D, g(1), Rm::M(Addr::Bi(g(1), g(0), 2))), &[0x8b, 0x0c, 0x81], "mov\tecx, dword ptr [ecx + 4*eax]", None),
        (|a| a.mov(Sz::D, g(0), Rm::M(Addr::Bi(g(5), g(1), 3))), &[0x8b, 0x44, 0xcd, 0x00], "mov\teax, dword ptr [ebp + 8*ecx]", None),
        (|a| a.mov_st(Sz::D, r(5), g(4)), &[0x89, 0xe5], "mov\tebp, esp", None),
        (|a| a.mov_st(Sz::D, r(0), g(4)), &[0x89, 0xe0], "mov\teax, esp", None),
        (|a| a.mov_st(Sz::D, m(4, 0x10), g(7)), &[0x89, 0x7c, 0x24, 0x10], "mov\tdword ptr [esp + 0x10], edi", None),
        (|a| a.mov_st(Sz::B, m(0, 0), g(1)), &[0x88, 0x08], "mov\tbyte ptr [eax], cl", None),
        (|a| a.mov_st(Sz::W, m(7, 2), g(0)), &[0x66, 0x89, 0x47, 0x02], "mov\tword ptr [edi + 0x2], ax", None),
        (|a| a.mov_st(Sz::W, m(4, 10), g(1)), &[0x66, 0x89, 0x4c, 0x24, 0x0a], "mov\tword ptr [esp + 0xa], cx", None),
        (|a| a.mov_ri(g(0), 1), &[0xb8, 0x01, 0x00, 0x00, 0x00], "mov\teax, 0x1", None),
        (|a| a.mov_ri(g(1), 0xffff_ffff), &[0xb9, 0xff, 0xff, 0xff, 0xff], "mov\tecx, 0xffffffff", None),
        (|a| a.mov_ri(g(1), 0), &[0xb9, 0x00, 0x00, 0x00, 0x00], "mov\tecx, 0x0", None),
        (|a| a.mov_ri(g(7), 1), &[0xbf, 0x01, 0x00, 0x00, 0x00], "mov\tedi, 0x1", None),
        (|a| a.mov_ri_sym(g(2), RelKind::I386Abs32, 0), &[0xba, 0x00, 0x00, 0x00, 0x00], "mov\tedx, 0x0", Some((1, RelKind::I386Abs32))),
        (|a| a.mov_mi(m(4, 0), 0x5f80_0000), &[0xc7, 0x04, 0x24, 0x00, 0x00, 0x80, 0x5f], "mov\tdword ptr [esp], 0x5f800000", None),
        (|a| a.mov_mi(m(4, 4), 0xffff_ffff), &[0xc7, 0x44, 0x24, 0x04, 0xff, 0xff, 0xff, 0xff], "mov\tdword ptr [esp + 0x4], 0xffffffff", None),
        (|a| a.movzx(g(0), Sz::B, r(0)), &[0x0f, 0xb6, 0xc0], "movzx\teax, al", None),
        (|a| a.movzx(g(2), Sz::W, r(1)), &[0x0f, 0xb7, 0xd1], "movzx\tedx, cx", None),
        (|a| a.movzx(g(6), Sz::B, m(4, 16)), &[0x0f, 0xb6, 0x74, 0x24, 0x10], "movzx\tesi, byte ptr [esp + 0x10]", None),
        (|a| a.movzx(g(0), Sz::W, m(6, 0)), &[0x0f, 0xb7, 0x06], "movzx\teax, word ptr [esi]", None),
        (|a| a.movsx(g(1), Sz::B, r(3)), &[0x0f, 0xbe, 0xcb], "movsx\tecx, bl", None),
        (|a| a.movsx(g(0), Sz::W, m(6, 2)), &[0x0f, 0xbf, 0x46, 0x02], "movsx\teax, word ptr [esi + 0x2]", None),
        (|a| a.movsx(g(0), Sz::W, r(0)), &[0x0f, 0xbf, 0xc0], "movsx\teax, ax", None),
        (|a| a.lea(g(4), bd(5, -12)), &[0x8d, 0x65, 0xf4], "lea\tesp, [ebp - 0xc]", None),
        (|a| a.lea(g(1), bd(4, 4)), &[0x8d, 0x4c, 0x24, 0x04], "lea\tecx, [esp + 0x4]", None),
        (|a| a.lea(g(2), bd(4, 0x200)), &[0x8d, 0x94, 0x24, 0x00, 0x02, 0x00, 0x00], "lea\tedx, [esp + 0x200]", None),
        (|a| a.lea(g(0), Addr::BdRel(g(1), RelKind::X86SecRel, 0)), &[0x8d, 0x81, 0x00, 0x00, 0x00, 0x00], "lea\teax, [ecx]", Some((2, RelKind::X86SecRel))),
        (|a| a.lea(g(3), Addr::BdRel(g(3), RelKind::I386GotOff, 0)), &[0x8d, 0x9b, 0x00, 0x00, 0x00, 0x00], "lea\tebx, [ebx]", Some((2, RelKind::I386GotOff))),
        (|a| a.xchg(Sz::D, r(3), g(1)), &[0x87, 0xcb], "xchg\tecx, ebx", None),
        (|a| a.xchg(Sz::D, r(2), g(0)), &[0x87, 0xc2], "xchg\teax, edx", None),
        (|a| a.xchg(Sz::D, m(6, 0), g(0)), &[0x87, 0x06], "xchg\tdword ptr [esi], eax", None),
        (|a| a.xchg(Sz::B, m(6, 0), g(0)), &[0x86, 0x06], "xchg\tbyte ptr [esi], al", None),
        (|a| a.xchg(Sz::W, m(6, 0), g(0)), &[0x66, 0x87, 0x06], "xchg\tword ptr [esi], ax", None),
        (|a| a.xchg_eax(g(2)), &[0x92], "xchg\teax, edx", None),
        (|a| a.xchg_eax(g(4)), &[0x94], "xchg\teax, esp", None),
        (|a| a.cmov(Cond::S, g(0), r(1)), &[0x0f, 0x48, 0xc1], "cmovs\teax, ecx", None),
        (|a| a.cmov(Cond::GE, g(2), m(4, 4)), &[0x0f, 0x4d, 0x54, 0x24, 0x04], "cmovge\tedx, dword ptr [esp + 0x4]", None),
        (|a| a.setcc(Cond::NE, g(0)), &[0x0f, 0x95, 0xc0], "setne\tal", None),
        (|a| a.setcc(Cond::NP, g(1)), &[0x0f, 0x9b, 0xc1], "setnp\tcl", None),
        (|a| a.setcc(Cond::G, g(3)), &[0x0f, 0x9f, 0xc3], "setg\tbl", None),
        (|a| a.setcc(Cond::O, g(2)), &[0x0f, 0x90, 0xc2], "seto\tdl", None),
        (|a| a.setcc(Cond::B, g(0)), &[0x0f, 0x92, 0xc0], "setb\tal", None),
        (|a| a.setcc(Cond::AE, g(0)), &[0x0f, 0x93, 0xc0], "setae\tal", None),
        (|a| a.setcc(Cond::BE, g(0)), &[0x0f, 0x96, 0xc0], "setbe\tal", None),
        (|a| a.setcc(Cond::A, g(0)), &[0x0f, 0x97, 0xc0], "seta\tal", None),
        (|a| a.setcc(Cond::L, g(0)), &[0x0f, 0x9c, 0xc0], "setl\tal", None),
        (|a| a.setcc(Cond::LE, g(0)), &[0x0f, 0x9e, 0xc0], "setle\tal", None),
        (|a| a.setcc(Cond::E, g(0)), &[0x0f, 0x94, 0xc0], "sete\tal", None),
        (|a| a.setcc(Cond::P, g(0)), &[0x0f, 0x9a, 0xc0], "setp\tal", None),
        (|a| a.setcc(Cond::S, g(0)), &[0x0f, 0x98, 0xc0], "sets\tal", None),
        (|a| a.setcc(Cond::NS, g(0)), &[0x0f, 0x99, 0xc0], "setns\tal", None),
        (|a| a.setcc(Cond::GE, g(0)), &[0x0f, 0x9d, 0xc0], "setge\tal", None),
        (|a| a.push(g(5)), &[0x55], "push\tebp", None),
        (|a| a.push(g(0)), &[0x50], "push\teax", None),
        (|a| a.pop(g(7)), &[0x5f], "pop\tedi", None),
        (|a| a.pop(g(3)), &[0x5b], "pop\tebx", None),
        (|a| a.push_i8(64), &[0x6a, 0x40], "push\t0x40", None),
        (|a| a.push_i8(-1), &[0x6a, 0xff], "push\t-0x1", None),
        (|a| a.push_i8(0), &[0x6a, 0x00], "push\t0x0", None),
        (|a| a.push_sym(RelKind::I386Abs32, 0), &[0x68, 0x00, 0x00, 0x00, 0x00], "push\t0x0", Some((1, RelKind::I386Abs32))),
        (|a| a.push_m(bd(5, 8)), &[0xff, 0x75, 0x08], "push\tdword ptr [ebp + 0x8]", None),
        (|a| a.push_m(bd(5, 20)), &[0xff, 0x75, 0x14], "push\tdword ptr [ebp + 0x14]", None),
        (|a| a.alu(Alu::Add, Sz::D, r(0), g(1)), &[0x01, 0xc8], "add\teax, ecx", None),
        (|a| a.alu_ld(Alu::Add, g(3), r(2)), &[0x03, 0xda], "add\tebx, edx", None),
        (|a| a.alu_ld(Alu::Add, g(1), m(4, 4)), &[0x03, 0x4c, 0x24, 0x04], "add\tecx, dword ptr [esp + 0x4]", None),
        (|a| a.alu_ri(Alu::Add, r(2), 0x8000_0000), &[0x81, 0xc2, 0x00, 0x00, 0x00, 0x80], "add\tedx, 0x80000000", None),
        (|a| a.alu_ri8(Alu::Add, r(6), -1), &[0x83, 0xc6, 0xff], "add\tesi, -0x1", None),
        (|a| a.alu_eax(Alu::Add, 0x1000), &[0x05, 0x00, 0x10, 0x00, 0x00], "add\teax, 0x1000", None),
        (|a| a.alu(Alu::Or, Sz::D, r(0), g(1)), &[0x09, 0xc8], "or\teax, ecx", None),
        (|a| a.alu_ld(Alu::Or, g(3), r(2)), &[0x0b, 0xda], "or\tebx, edx", None),
        (|a| a.alu_ld(Alu::Or, g(1), m(4, 4)), &[0x0b, 0x4c, 0x24, 0x04], "or\tecx, dword ptr [esp + 0x4]", None),
        (|a| a.alu_ri(Alu::Or, r(2), 0x8000_0000), &[0x81, 0xca, 0x00, 0x00, 0x00, 0x80], "or\tedx, 0x80000000", None),
        (|a| a.alu_ri8(Alu::Or, r(6), -1), &[0x83, 0xce, 0xff], "or\tesi, -0x1", None),
        (|a| a.alu_eax(Alu::Or, 0x1000), &[0x0d, 0x00, 0x10, 0x00, 0x00], "or\teax, 0x1000", None),
        (|a| a.alu(Alu::Adc, Sz::D, r(0), g(1)), &[0x11, 0xc8], "adc\teax, ecx", None),
        (|a| a.alu_ld(Alu::Adc, g(3), r(2)), &[0x13, 0xda], "adc\tebx, edx", None),
        (|a| a.alu_ld(Alu::Adc, g(1), m(4, 4)), &[0x13, 0x4c, 0x24, 0x04], "adc\tecx, dword ptr [esp + 0x4]", None),
        (|a| a.alu_ri(Alu::Adc, r(2), 0x8000_0000), &[0x81, 0xd2, 0x00, 0x00, 0x00, 0x80], "adc\tedx, 0x80000000", None),
        (|a| a.alu_ri8(Alu::Adc, r(6), -1), &[0x83, 0xd6, 0xff], "adc\tesi, -0x1", None),
        (|a| a.alu_eax(Alu::Adc, 0x1000), &[0x15, 0x00, 0x10, 0x00, 0x00], "adc\teax, 0x1000", None),
        (|a| a.alu(Alu::Sbb, Sz::D, r(0), g(1)), &[0x19, 0xc8], "sbb\teax, ecx", None),
        (|a| a.alu_ld(Alu::Sbb, g(3), r(2)), &[0x1b, 0xda], "sbb\tebx, edx", None),
        (|a| a.alu_ld(Alu::Sbb, g(1), m(4, 4)), &[0x1b, 0x4c, 0x24, 0x04], "sbb\tecx, dword ptr [esp + 0x4]", None),
        (|a| a.alu_ri(Alu::Sbb, r(2), 0x8000_0000), &[0x81, 0xda, 0x00, 0x00, 0x00, 0x80], "sbb\tedx, 0x80000000", None),
        (|a| a.alu_ri8(Alu::Sbb, r(6), -1), &[0x83, 0xde, 0xff], "sbb\tesi, -0x1", None),
        (|a| a.alu_eax(Alu::Sbb, 0x1000), &[0x1d, 0x00, 0x10, 0x00, 0x00], "sbb\teax, 0x1000", None),
        (|a| a.alu(Alu::And, Sz::D, r(0), g(1)), &[0x21, 0xc8], "and\teax, ecx", None),
        (|a| a.alu_ld(Alu::And, g(3), r(2)), &[0x23, 0xda], "and\tebx, edx", None),
        (|a| a.alu_ld(Alu::And, g(1), m(4, 4)), &[0x23, 0x4c, 0x24, 0x04], "and\tecx, dword ptr [esp + 0x4]", None),
        (|a| a.alu_ri(Alu::And, r(2), 0x8000_0000), &[0x81, 0xe2, 0x00, 0x00, 0x00, 0x80], "and\tedx, 0x80000000", None),
        (|a| a.alu_ri8(Alu::And, r(6), -1), &[0x83, 0xe6, 0xff], "and\tesi, -0x1", None),
        (|a| a.alu_eax(Alu::And, 0x1000), &[0x25, 0x00, 0x10, 0x00, 0x00], "and\teax, 0x1000", None),
        (|a| a.alu(Alu::Sub, Sz::D, r(0), g(1)), &[0x29, 0xc8], "sub\teax, ecx", None),
        (|a| a.alu_ld(Alu::Sub, g(3), r(2)), &[0x2b, 0xda], "sub\tebx, edx", None),
        (|a| a.alu_ld(Alu::Sub, g(1), m(4, 4)), &[0x2b, 0x4c, 0x24, 0x04], "sub\tecx, dword ptr [esp + 0x4]", None),
        (|a| a.alu_ri(Alu::Sub, r(2), 0x8000_0000), &[0x81, 0xea, 0x00, 0x00, 0x00, 0x80], "sub\tedx, 0x80000000", None),
        (|a| a.alu_ri8(Alu::Sub, r(6), -1), &[0x83, 0xee, 0xff], "sub\tesi, -0x1", None),
        (|a| a.alu_eax(Alu::Sub, 0x1000), &[0x2d, 0x00, 0x10, 0x00, 0x00], "sub\teax, 0x1000", None),
        (|a| a.alu(Alu::Xor, Sz::D, r(0), g(1)), &[0x31, 0xc8], "xor\teax, ecx", None),
        (|a| a.alu_ld(Alu::Xor, g(3), r(2)), &[0x33, 0xda], "xor\tebx, edx", None),
        (|a| a.alu_ld(Alu::Xor, g(1), m(4, 4)), &[0x33, 0x4c, 0x24, 0x04], "xor\tecx, dword ptr [esp + 0x4]", None),
        (|a| a.alu_ri(Alu::Xor, r(2), 0x8000_0000), &[0x81, 0xf2, 0x00, 0x00, 0x00, 0x80], "xor\tedx, 0x80000000", None),
        (|a| a.alu_ri8(Alu::Xor, r(6), -1), &[0x83, 0xf6, 0xff], "xor\tesi, -0x1", None),
        (|a| a.alu_eax(Alu::Xor, 0x1000), &[0x35, 0x00, 0x10, 0x00, 0x00], "xor\teax, 0x1000", None),
        (|a| a.alu(Alu::Cmp, Sz::D, r(0), g(1)), &[0x39, 0xc8], "cmp\teax, ecx", None),
        (|a| a.alu_ld(Alu::Cmp, g(3), r(2)), &[0x3b, 0xda], "cmp\tebx, edx", None),
        (|a| a.alu_ld(Alu::Cmp, g(1), m(4, 4)), &[0x3b, 0x4c, 0x24, 0x04], "cmp\tecx, dword ptr [esp + 0x4]", None),
        (|a| a.alu_ri(Alu::Cmp, r(2), 0x8000_0000), &[0x81, 0xfa, 0x00, 0x00, 0x00, 0x80], "cmp\tedx, 0x80000000", None),
        (|a| a.alu_ri8(Alu::Cmp, r(6), -1), &[0x83, 0xfe, 0xff], "cmp\tesi, -0x1", None),
        (|a| a.alu_eax(Alu::Cmp, 0x1000), &[0x3d, 0x00, 0x10, 0x00, 0x00], "cmp\teax, 0x1000", None),
        (|a| a.alu(Alu::Xor, Sz::D, m(4, 0), g(6)), &[0x31, 0x34, 0x24], "xor\tdword ptr [esp], esi", None),
        (|a| a.alu(Alu::Add, Sz::D, m(4, 0), g(0)), &[0x01, 0x04, 0x24], "add\tdword ptr [esp], eax", None),
        (|a| a.alu(Alu::And, Sz::B, r(0), g(1)), &[0x20, 0xc8], "and\tal, cl", None),
        (|a| a.alu(Alu::Or, Sz::B, r(0), g(1)), &[0x08, 0xc8], "or\tal, cl", None),
        (|a| a.alu(Alu::Xor, Sz::D, r(2), g(2)), &[0x31, 0xd2], "xor\tedx, edx", None),
        (|a| a.alu(Alu::Cmp, Sz::D, r(7), g(6)), &[0x39, 0xf7], "cmp\tedi, esi", None),
        (|a| a.alu_ld(Alu::Add, g(0), Rm::M(Addr::Seg(Seg::Gs, 0))), &[0x65, 0x03, 0x05, 0x00, 0x00, 0x00, 0x00], "add\teax, dword ptr gs:[0x0]", None),
        (|a| a.alu_ld(Alu::Cmp, g(0), m(1, 0)), &[0x3b, 0x01], "cmp\teax, dword ptr [ecx]", None),
        (|a| a.alu_ri(Alu::Sub, r(4), 0x1000), &[0x81, 0xec, 0x00, 0x10, 0x00, 0x00], "sub\tesp, 0x1000", None),
        (|a| a.alu_ri(Alu::Sub, r(4), 12), &[0x81, 0xec, 0x0c, 0x00, 0x00, 0x00], "sub\tesp, 0xc", None),
        (|a| a.alu_ri(Alu::And, r(1), 0x5555_5555), &[0x81, 0xe1, 0x55, 0x55, 0x55, 0x55], "and\tecx, 0x55555555", None),
        (|a| a.alu_ri(Alu::Or, r(1), 0xc00), &[0x81, 0xc9, 0x00, 0x0c, 0x00, 0x00], "or\tecx, 0xc00", None),
        (|a| a.alu_ri(Alu::Cmp, r(0), 0xffff_ff80), &[0x81, 0xf8, 0x80, 0xff, 0xff, 0xff], "cmp\teax, 0xffffff80", None),
        (|a| a.alu_ri(Alu::Add, m(4, 0), 0x1000), &[0x81, 0x04, 0x24, 0x00, 0x10, 0x00, 0x00], "add\tdword ptr [esp], 0x1000", None),
        (|a| a.alu_ri_sym(Alu::Add, g(3), RelKind::I386GotPc, 0, 3), &[0x81, 0xc3, 0x03, 0x00, 0x00, 0x00], "add\tebx, 0x3", Some((2, RelKind::I386GotPc))),
        (|a| a.alu_ri8(Alu::Or, m(4, 0), 0), &[0x83, 0x0c, 0x24, 0x00], "or\tdword ptr [esp], 0x0", None),
        (|a| a.alu_ri8(Alu::Add, r(4), 4), &[0x83, 0xc4, 0x04], "add\tesp, 0x4", None),
        (|a| a.alu_ri8(Alu::Adc, r(2), 0), &[0x83, 0xd2, 0x00], "adc\tedx, 0x0", None),
        (|a| a.alu_ri8(Alu::Add, r(0), 32), &[0x83, 0xc0, 0x20], "add\teax, 0x20", None),
        (|a| a.alu_eax(Alu::And, 0xffff_f000), &[0x25, 0x00, 0xf0, 0xff, 0xff], "and\teax, 0xfffff000", None),
        (|a| a.test(Sz::D, r(0), g(0)), &[0x85, 0xc0], "test\teax, eax", None),
        (|a| a.test(Sz::D, r(7), g(7)), &[0x85, 0xff], "test\tedi, edi", None),
        (|a| a.test(Sz::B, r(0), g(0)), &[0x84, 0xc0], "test\tal, al", None),
        (|a| a.test(Sz::D, m(0, 0), g(0)), &[0x85, 0x00], "test\tdword ptr [eax], eax", None),
        (|a| a.test_ri(Sz::B, r(1), 0x20), &[0xf6, 0xc1, 0x20], "test\tcl, 0x20", None),
        (|a| a.test_ri(Sz::D, r(2), 0xaaaa), &[0xf7, 0xc2, 0xaa, 0xaa, 0x00, 0x00], "test\tedx, 0xaaaa", None),
        (|a| a.un(Un::Not, r(1)), &[0xf7, 0xd1], "not\tecx", None),
        (|a| a.un(Un::Neg, r(1)), &[0xf7, 0xd9], "neg\tecx", None),
        (|a| a.un(Un::Mul, r(1)), &[0xf7, 0xe1], "mul\tecx", None),
        (|a| a.un(Un::Imul, r(1)), &[0xf7, 0xe9], "imul\tecx", None),
        (|a| a.un(Un::Div, r(1)), &[0xf7, 0xf1], "div\tecx", None),
        (|a| a.un(Un::Idiv, r(1)), &[0xf7, 0xf9], "idiv\tecx", None),
        (|a| a.un(Un::Neg, r(0)), &[0xf7, 0xd8], "neg\teax", None),
        (|a| a.un(Un::Mul, r(6)), &[0xf7, 0xe6], "mul\tesi", None),
        (|a| a.dec(r(1)), &[0x49], "dec\tecx", None),
        (|a| a.dec(m(4, 0)), &[0xff, 0x0c, 0x24], "dec\tdword ptr [esp]", None),
        (|a| a.shift_ri(Shift::Shl, Sz::D, g(0), 3), &[0xc1, 0xe0, 0x03], "shl\teax, 0x3", None),
        (|a| a.shift_ri(Shift::Shr, Sz::D, g(1), 4), &[0xc1, 0xe9, 0x04], "shr\tecx, 0x4", None),
        (|a| a.shift_ri(Shift::Shr, Sz::D, g(0), 24), &[0xc1, 0xe8, 0x18], "shr\teax, 0x18", None),
        (|a| a.shift_ri(Shift::Sar, Sz::D, g(2), 31), &[0xc1, 0xfa, 0x1f], "sar\tedx, 0x1f", None),
        (|a| a.shift_ri(Shift::Rol, Sz::W, g(0), 8), &[0x66, 0xc1, 0xc0, 0x08], "rol\tax, 0x8", None),
        (|a| a.shift_ri(Shift::Rol, Sz::D, g(0), 7), &[0xc1, 0xc0, 0x07], "rol\teax, 0x7", None),
        (|a| a.shift_cl(Shift::Rol, g(2)), &[0xd3, 0xc2], "rol\tedx, cl", None),
        (|a| a.shift_ri(Shift::Shr, Sz::D, g(1), 1), &[0xc1, 0xe9, 0x01], "shr\tecx, 0x1", None),
        (|a| a.shift_1(Shift::Shl, g(0)), &[0xd1, 0xe0], "shl\teax", None),
        (|a| a.shift_1(Shift::Rcl, g(2)), &[0xd1, 0xd2], "rcl\tedx", None),
        (|a| a.shift_1(Shift::Rcl, g(7)), &[0xd1, 0xd7], "rcl\tedi", None),
        (|a| a.shift_cl(Shift::Shl, g(0)), &[0xd3, 0xe0], "shl\teax, cl", None),
        (|a| a.shift_cl(Shift::Sar, g(2)), &[0xd3, 0xfa], "sar\tedx, cl", None),
        (|a| a.shift_cl(Shift::Shr, g(2)), &[0xd3, 0xea], "shr\tedx, cl", None),
        (|a| a.shxd_cl(false, g(2), g(0)), &[0x0f, 0xa5, 0xc2], "shld\tedx, eax, cl", None),
        (|a| a.shxd_cl(true, g(0), g(2)), &[0x0f, 0xad, 0xd0], "shrd\teax, edx, cl", None),
        (|a| a.imul(g(7), r(1)), &[0x0f, 0xaf, 0xf9], "imul\tedi, ecx", None),
        (|a| a.imul(g(0), m(4, 8)), &[0x0f, 0xaf, 0x44, 0x24, 0x08], "imul\teax, dword ptr [esp + 0x8]", None),
        (|a| a.imul_ri(g(0), r(0), 0x0101_0101), &[0x69, 0xc0, 0x01, 0x01, 0x01, 0x01], "imul\teax, eax, 0x1010101", None),
        (|a| a.imul_ri(g(1), r(1), 0xffff_fffd), &[0x69, 0xc9, 0xfd, 0xff, 0xff, 0xff], "imul\tecx, ecx, 0xfffffffd", None),
        (|a| a.cdq(), &[0x99], "cdq", None),
        (|a| a.bsf(false, g(0), r(2)), &[0x0f, 0xbc, 0xc2], "bsf\teax, edx", None),
        (|a| a.bsf(true, g(1), r(2)), &[0x0f, 0xbd, 0xca], "bsr\tecx, edx", None),
        (|a| a.bswap(g(0)), &[0x0f, 0xc8], "bswap\teax", None),
        (|a| a.bswap(g(2)), &[0x0f, 0xca], "bswap\tedx", None),
        (|a| a.lock(), &[0xf0], "lock", None),
        (|a| a.xadd(Sz::D, bd(6, 0), g(1)), &[0x0f, 0xc1, 0x0e], "xadd\tdword ptr [esi], ecx", None),
        (|a| a.xadd(Sz::B, bd(6, 0), g(1)), &[0x0f, 0xc0, 0x0e], "xadd\tbyte ptr [esi], cl", None),
        (|a| a.xadd(Sz::W, bd(6, 0), g(1)), &[0x66, 0x0f, 0xc1, 0x0e], "xadd\tword ptr [esi], cx", None),
        (|a| a.cmpxchg(Sz::D, bd(6, 0), g(1)), &[0x0f, 0xb1, 0x0e], "cmpxchg\tdword ptr [esi], ecx", None),
        (|a| a.cmpxchg(Sz::B, bd(6, 0), g(2)), &[0x0f, 0xb0, 0x16], "cmpxchg\tbyte ptr [esi], dl", None),
        (|a| a.cmpxchg(Sz::W, bd(6, 0), g(2)), &[0x66, 0x0f, 0xb1, 0x16], "cmpxchg\tword ptr [esi], dx", None),
        (|a| a.cmpxchg8b(bd(7, 0)), &[0x0f, 0xc7, 0x0f], "cmpxchg8b\tqword ptr [edi]", None),
        (|a| a.mfence(), &[0x0f, 0xae, 0xf0], "mfence", None),
        (|a| { a.jcc(Cond::NE, None); }, &[0x0f, 0x85, 0x00, 0x00, 0x00, 0x00], "jne\t0x6", None),
        (|a| { a.jcc(Cond::E, Some(0)); }, &[0x0f, 0x84, 0xfa, 0xff, 0xff, 0xff], "je\t0x0", None),
        (|a| { a.jcc(Cond::NS, None); }, &[0x0f, 0x89, 0x00, 0x00, 0x00, 0x00], "jns\t0x6", None),
        (|a| { a.jmp(None); }, &[0xe9, 0x00, 0x00, 0x00, 0x00], "jmp\t0x5", None),
        (|a| { a.jmp(Some(0)); }, &[0xe9, 0xfb, 0xff, 0xff, 0xff], "jmp\t0x0", None),
        (|a| { a.jcc8(Cond::B, Some(0)); }, &[0x72, 0xfe], "jb\t0x0", None),
        (|a| { a.jcc8(Cond::AE, None); }, &[0x73, 0x00], "jae\t0x2", None),
        (|a| { a.jmp8(Some(0)); }, &[0xeb, 0xfe], "jmp\t0x0", None),
        (|a| a.call(RelKind::I386Call, 0, -4), &[0xe8, 0xfc, 0xff, 0xff, 0xff], "call\t0x1", Some((1, RelKind::I386Call))),
        (|a| a.call(RelKind::I386Call, 0, 0), &[0xe8, 0x00, 0x00, 0x00, 0x00], "call\t0x5", Some((1, RelKind::I386Call))),
        (|a| a.call_next(), &[0xe8, 0x00, 0x00, 0x00, 0x00], "call\t0x5", None),
        (|a| a.call_r(g(0)), &[0xff, 0xd0], "call\teax", None),
        (|a| a.ret(), &[0xc3], "ret", None),
        (|a| a.ret_n(16), &[0xc2, 0x10, 0x00], "ret\t0x10", None),
        (|a| a.ud2(), &[0x0f, 0x0b], "ud2", None),
        (|a| a.int3(), &[0xcc], "int3", None),
        (|a| a.x87(X87::Fld32, bd(4, 0)), &[0xd9, 0x04, 0x24], "fld\tdword ptr [esp]", None),
        (|a| a.x87(X87::Fld64, bd(4, 0)), &[0xdd, 0x04, 0x24], "fld\tqword ptr [esp]", None),
        (|a| a.x87(X87::Fstp32, bd(4, 0)), &[0xd9, 0x1c, 0x24], "fstp\tdword ptr [esp]", None),
        (|a| a.x87(X87::Fstp64, bd(4, 0)), &[0xdd, 0x1c, 0x24], "fstp\tqword ptr [esp]", None),
        (|a| a.x87(X87::Fild64, bd(4, 0)), &[0xdf, 0x2c, 0x24], "fild\tqword ptr [esp]", None),
        (|a| a.x87(X87::Fistp64, bd(4, 0)), &[0xdf, 0x3c, 0x24], "fistp\tqword ptr [esp]", None),
        (|a| a.x87(X87::Fadd32, bd(4, 0)), &[0xd8, 0x04, 0x24], "fadd\tdword ptr [esp]", None),
        (|a| a.x87(X87::Fnstcw, bd(4, 0)), &[0xd9, 0x3c, 0x24], "fnstcw\tword ptr [esp]", None),
        (|a| a.x87(X87::Fldcw, bd(4, 0)), &[0xd9, 0x2c, 0x24], "fldcw\tword ptr [esp]", None),
        (|a| a.x87(X87::Fstp64, bd(1, 8)), &[0xdd, 0x59, 0x08], "fstp\tqword ptr [ecx + 0x8]", None),
        (|a| a.x87(X87::Fldcw, bd(4, 10)), &[0xd9, 0x6c, 0x24, 0x0a], "fldcw\tword ptr [esp + 0xa]", None),
        (|a| a.sse(Sse::Movss, x(0), xm(4, 0)), &[0xf3, 0x0f, 0x10, 0x04, 0x24], "movss\txmm0, dword ptr [esp]", None),
        (|a| a.sse(Sse::Movsd, x(7), xm(5, 8)), &[0xf2, 0x0f, 0x10, 0x7d, 0x08], "movsd\txmm7, qword ptr [ebp + 0x8]", None),
        (|a| a.sse(Sse::Movss, x(1), xr(2)), &[0xf3, 0x0f, 0x10, 0xca], "movss\txmm1, xmm2", None),
        (|a| a.sse_st(Sse::MovssSt, bd(4, 4), x(1)), &[0xf3, 0x0f, 0x11, 0x4c, 0x24, 0x04], "movss\tdword ptr [esp + 0x4], xmm1", None),
        (|a| a.sse_st(Sse::MovsdSt, bd(1, 0), x(3)), &[0xf2, 0x0f, 0x11, 0x19], "movsd\tqword ptr [ecx], xmm3", None),
        (|a| a.sse(Sse::Movaps, x(1), xr(2)), &[0x0f, 0x28, 0xca], "movaps\txmm1, xmm2", None),
        (|a| a.sse(Sse::Movdqa, x(7), xr(0)), &[0x66, 0x0f, 0x6f, 0xf8], "movdqa\txmm7, xmm0", None),
        (|a| a.sse(Sse::Movdqu, x(3), Xrm::M(Addr::Bi(g(6), g(0), 0))), &[0xf3, 0x0f, 0x6f, 0x1c, 0x06], "movdqu\txmm3, xmmword ptr [esi + eax]", None),
        (|a| a.sse(Sse::Movdqu, x(3), Xrm::M(Addr::Bi(g(6), g(0), 1))), &[0xf3, 0x0f, 0x6f, 0x1c, 0x46], "movdqu\txmm3, xmmword ptr [esi + 2*eax]", None),
        (|a| a.sse_st(Sse::MovdquSt, Addr::Bi(g(3), g(0), 0), x(2)), &[0xf3, 0x0f, 0x7f, 0x14, 0x03], "movdqu\txmmword ptr [ebx + eax], xmm2", None),
        (|a| a.sse_st(Sse::MovdquSt, bd(4, 0x40), x(6)), &[0xf3, 0x0f, 0x7f, 0x74, 0x24, 0x40], "movdqu\txmmword ptr [esp + 0x40], xmm6", None),
        (|a| a.sse(Sse::MovqLd, x(0), xm(6, 0)), &[0xf3, 0x0f, 0x7e, 0x06], "movq\txmm0, qword ptr [esi]", None),
        (|a| a.sse(Sse::MovdLd, x(1), xm(4, 0x20)), &[0x66, 0x0f, 0x6e, 0x4c, 0x24, 0x20], "movd\txmm1, dword ptr [esp + 0x20]", None),
        (|a| a.sse(Sse::MovdLd, x(1), Xrm::M(Addr::Bi(g(7), g(0), 2))), &[0x66, 0x0f, 0x6e, 0x0c, 0x87], "movd\txmm1, dword ptr [edi + 4*eax]", None),
        (|a| a.sse_st(Sse::MovqSt, bd(6, 0), x(0)), &[0x66, 0x0f, 0xd6, 0x06], "movq\tqword ptr [esi], xmm0", None),
        (|a| a.sse_st(Sse::MovdSt, Addr::Bi(g(7), g(0), 2), x(7)), &[0x66, 0x0f, 0x7e, 0x3c, 0x87], "movd\tdword ptr [edi + 4*eax], xmm7", None),
        (|a| a.sse(Sse::Addss, x(0), xr(1)), &[0xf3, 0x0f, 0x58, 0xc1], "addss\txmm0, xmm1", None),
        (|a| a.sse(Sse::Addsd, x(0), xr(1)), &[0xf2, 0x0f, 0x58, 0xc1], "addsd\txmm0, xmm1", None),
        (|a| a.sse(Sse::Subss, x(0), xr(1)), &[0xf3, 0x0f, 0x5c, 0xc1], "subss\txmm0, xmm1", None),
        (|a| a.sse(Sse::Subsd, x(0), xr(1)), &[0xf2, 0x0f, 0x5c, 0xc1], "subsd\txmm0, xmm1", None),
        (|a| a.sse(Sse::Mulss, x(0), xr(1)), &[0xf3, 0x0f, 0x59, 0xc1], "mulss\txmm0, xmm1", None),
        (|a| a.sse(Sse::Mulsd, x(0), xr(1)), &[0xf2, 0x0f, 0x59, 0xc1], "mulsd\txmm0, xmm1", None),
        (|a| a.sse(Sse::Divss, x(0), xr(1)), &[0xf3, 0x0f, 0x5e, 0xc1], "divss\txmm0, xmm1", None),
        (|a| a.sse(Sse::Divsd, x(0), xr(1)), &[0xf2, 0x0f, 0x5e, 0xc1], "divsd\txmm0, xmm1", None),
        (|a| a.sse(Sse::Sqrtss, x(0), xr(1)), &[0xf3, 0x0f, 0x51, 0xc1], "sqrtss\txmm0, xmm1", None),
        (|a| a.sse(Sse::Sqrtsd, x(0), xr(1)), &[0xf2, 0x0f, 0x51, 0xc1], "sqrtsd\txmm0, xmm1", None),
        (|a| a.sse(Sse::Cvtss2sd, x(0), xr(1)), &[0xf3, 0x0f, 0x5a, 0xc1], "cvtss2sd\txmm0, xmm1", None),
        (|a| a.sse(Sse::Cvtsd2ss, x(0), xr(1)), &[0xf2, 0x0f, 0x5a, 0xc1], "cvtsd2ss\txmm0, xmm1", None),
        (|a| a.sse(Sse::Ucomiss, x(0), xr(1)), &[0x0f, 0x2e, 0xc1], "ucomiss\txmm0, xmm1", None),
        (|a| a.sse(Sse::Ucomisd, x(0), xr(1)), &[0x66, 0x0f, 0x2e, 0xc1], "ucomisd\txmm0, xmm1", None),
        (|a| a.sse(Sse::Addsd, x(3), xm(4, 8)), &[0xf2, 0x0f, 0x58, 0x5c, 0x24, 0x08], "addsd\txmm3, qword ptr [esp + 0x8]", None),
        (|a| a.sse(Sse::Divss, x(0), xm(1, 0)), &[0xf3, 0x0f, 0x5e, 0x01], "divss\txmm0, dword ptr [ecx]", None),
        (|a| a.sse(Sse::Ucomisd, x(0), xm(4, 0)), &[0x66, 0x0f, 0x2e, 0x04, 0x24], "ucomisd\txmm0, qword ptr [esp]", None),
        (|a| a.sse(Sse::Xorps, x(2), xr(7)), &[0x0f, 0x57, 0xd7], "xorps\txmm2, xmm7", None),
        (|a| a.sse(Sse::Addps, x(2), xr(7)), &[0x0f, 0x58, 0xd7], "addps\txmm2, xmm7", None),
        (|a| a.sse(Sse::Addpd, x(2), xr(7)), &[0x66, 0x0f, 0x58, 0xd7], "addpd\txmm2, xmm7", None),
        (|a| a.sse(Sse::Subps, x(2), xr(7)), &[0x0f, 0x5c, 0xd7], "subps\txmm2, xmm7", None),
        (|a| a.sse(Sse::Subpd, x(2), xr(7)), &[0x66, 0x0f, 0x5c, 0xd7], "subpd\txmm2, xmm7", None),
        (|a| a.sse(Sse::Mulps, x(2), xr(7)), &[0x0f, 0x59, 0xd7], "mulps\txmm2, xmm7", None),
        (|a| a.sse(Sse::Mulpd, x(2), xr(7)), &[0x66, 0x0f, 0x59, 0xd7], "mulpd\txmm2, xmm7", None),
        (|a| a.sse(Sse::Divps, x(2), xr(7)), &[0x0f, 0x5e, 0xd7], "divps\txmm2, xmm7", None),
        (|a| a.sse(Sse::Divpd, x(2), xr(7)), &[0x66, 0x0f, 0x5e, 0xd7], "divpd\txmm2, xmm7", None),
        (|a| a.sse(Sse::Sqrtps, x(2), xr(7)), &[0x0f, 0x51, 0xd7], "sqrtps\txmm2, xmm7", None),
        (|a| a.sse(Sse::Sqrtpd, x(2), xr(7)), &[0x66, 0x0f, 0x51, 0xd7], "sqrtpd\txmm2, xmm7", None),
        (|a| a.sse(Sse::Punpcklbw, x(2), xr(7)), &[0x66, 0x0f, 0x60, 0xd7], "punpcklbw\txmm2, xmm7", None),
        (|a| a.sse(Sse::Punpcklwd, x(2), xr(7)), &[0x66, 0x0f, 0x61, 0xd7], "punpcklwd\txmm2, xmm7", None),
        (|a| a.sse(Sse::Punpckldq, x(2), xr(7)), &[0x66, 0x0f, 0x62, 0xd7], "punpckldq\txmm2, xmm7", None),
        (|a| a.sse(Sse::Punpcklqdq, x(2), xr(7)), &[0x66, 0x0f, 0x6c, 0xd7], "punpcklqdq\txmm2, xmm7", None),
        (|a| a.sse(Sse::Packsswb, x(2), xr(7)), &[0x66, 0x0f, 0x63, 0xd7], "packsswb\txmm2, xmm7", None),
        (|a| a.sse(Sse::Packuswb, x(2), xr(7)), &[0x66, 0x0f, 0x67, 0xd7], "packuswb\txmm2, xmm7", None),
        (|a| a.sse(Sse::Packssdw, x(2), xr(7)), &[0x66, 0x0f, 0x6b, 0xd7], "packssdw\txmm2, xmm7", None),
        (|a| a.sse(Sse::Paddb, x(2), xr(7)), &[0x66, 0x0f, 0xfc, 0xd7], "paddb\txmm2, xmm7", None),
        (|a| a.sse(Sse::Paddw, x(2), xr(7)), &[0x66, 0x0f, 0xfd, 0xd7], "paddw\txmm2, xmm7", None),
        (|a| a.sse(Sse::Paddd, x(2), xr(7)), &[0x66, 0x0f, 0xfe, 0xd7], "paddd\txmm2, xmm7", None),
        (|a| a.sse(Sse::Paddq, x(2), xr(7)), &[0x66, 0x0f, 0xd4, 0xd7], "paddq\txmm2, xmm7", None),
        (|a| a.sse(Sse::Psubb, x(2), xr(7)), &[0x66, 0x0f, 0xf8, 0xd7], "psubb\txmm2, xmm7", None),
        (|a| a.sse(Sse::Psubw, x(2), xr(7)), &[0x66, 0x0f, 0xf9, 0xd7], "psubw\txmm2, xmm7", None),
        (|a| a.sse(Sse::Psubd, x(2), xr(7)), &[0x66, 0x0f, 0xfa, 0xd7], "psubd\txmm2, xmm7", None),
        (|a| a.sse(Sse::Psubq, x(2), xr(7)), &[0x66, 0x0f, 0xfb, 0xd7], "psubq\txmm2, xmm7", None),
        (|a| a.sse(Sse::Pmullw, x(2), xr(7)), &[0x66, 0x0f, 0xd5, 0xd7], "pmullw\txmm2, xmm7", None),
        (|a| a.sse(Sse::Pand, x(2), xr(7)), &[0x66, 0x0f, 0xdb, 0xd7], "pand\txmm2, xmm7", None),
        (|a| a.sse(Sse::Pandn, x(2), xr(7)), &[0x66, 0x0f, 0xdf, 0xd7], "pandn\txmm2, xmm7", None),
        (|a| a.sse(Sse::Por, x(2), xr(7)), &[0x66, 0x0f, 0xeb, 0xd7], "por\txmm2, xmm7", None),
        (|a| a.sse(Sse::Pxor, x(2), xr(7)), &[0x66, 0x0f, 0xef, 0xd7], "pxor\txmm2, xmm7", None),
        (|a| a.sse(Sse::Pcmpeqb, x(2), xr(7)), &[0x66, 0x0f, 0x74, 0xd7], "pcmpeqb\txmm2, xmm7", None),
        (|a| a.sse(Sse::Pcmpeqw, x(2), xr(7)), &[0x66, 0x0f, 0x75, 0xd7], "pcmpeqw\txmm2, xmm7", None),
        (|a| a.sse(Sse::Pcmpeqd, x(2), xr(7)), &[0x66, 0x0f, 0x76, 0xd7], "pcmpeqd\txmm2, xmm7", None),
        (|a| a.sse(Sse::Pcmpgtb, x(2), xr(7)), &[0x66, 0x0f, 0x64, 0xd7], "pcmpgtb\txmm2, xmm7", None),
        (|a| a.sse(Sse::Pcmpgtw, x(2), xr(7)), &[0x66, 0x0f, 0x65, 0xd7], "pcmpgtw\txmm2, xmm7", None),
        (|a| a.sse(Sse::Pcmpgtd, x(2), xr(7)), &[0x66, 0x0f, 0x66, 0xd7], "pcmpgtd\txmm2, xmm7", None),
        (|a| a.sse(Sse::Pminub, x(2), xr(7)), &[0x66, 0x0f, 0xda, 0xd7], "pminub\txmm2, xmm7", None),
        (|a| a.sse(Sse::Pmaxub, x(2), xr(7)), &[0x66, 0x0f, 0xde, 0xd7], "pmaxub\txmm2, xmm7", None),
        (|a| a.sse(Sse::Pminsw, x(2), xr(7)), &[0x66, 0x0f, 0xea, 0xd7], "pminsw\txmm2, xmm7", None),
        (|a| a.sse(Sse::Pmaxsw, x(2), xr(7)), &[0x66, 0x0f, 0xee, 0xd7], "pmaxsw\txmm2, xmm7", None),
        (|a| a.sse(Sse::Minps, x(2), xr(7)), &[0x0f, 0x5d, 0xd7], "minps\txmm2, xmm7", None),
        (|a| a.sse(Sse::Minpd, x(2), xr(7)), &[0x66, 0x0f, 0x5d, 0xd7], "minpd\txmm2, xmm7", None),
        (|a| a.sse(Sse::Maxps, x(2), xr(7)), &[0x0f, 0x5f, 0xd7], "maxps\txmm2, xmm7", None),
        (|a| a.sse(Sse::Maxpd, x(2), xr(7)), &[0x66, 0x0f, 0x5f, 0xd7], "maxpd\txmm2, xmm7", None),
        (|a| a.sse(Sse::Pmulhw, x(2), xr(7)), &[0x66, 0x0f, 0xe5, 0xd7], "pmulhw\txmm2, xmm7", None),
        (|a| a.sse(Sse::Pmulhuw, x(2), xr(7)), &[0x66, 0x0f, 0xe4, 0xd7], "pmulhuw\txmm2, xmm7", None),
        (|a| a.sse_ri(Sse::Pshufd, x(2), x(3), 0x55), &[0x66, 0x0f, 0x70, 0xd3, 0x55], "pshufd\txmm2, xmm3, 0x55", None),
        (|a| a.sse_ri(Sse::Pshufd, x(0), x(0), 0), &[0x66, 0x0f, 0x70, 0xc0, 0x00], "pshufd\txmm0, xmm0, 0x0", None),
        (|a| a.sse_ri(Sse::Pshufd, x(7), x(1), 0xf5), &[0x66, 0x0f, 0x70, 0xf9, 0xf5], "pshufd\txmm7, xmm1, 0xf5", None),
        (|a| a.cmpp(false, FCmp::Eq, x(1), x(6)), &[0x0f, 0xc2, 0xce, 0x00], "cmpeqps\txmm1, xmm6", None),
        (|a| a.cmpp(false, FCmp::Lt, x(1), x(6)), &[0x0f, 0xc2, 0xce, 0x01], "cmpltps\txmm1, xmm6", None),
        (|a| a.cmpp(false, FCmp::Le, x(1), x(6)), &[0x0f, 0xc2, 0xce, 0x02], "cmpleps\txmm1, xmm6", None),
        (|a| a.cmpp(false, FCmp::Neq, x(1), x(6)), &[0x0f, 0xc2, 0xce, 0x04], "cmpneqps\txmm1, xmm6", None),
        (|a| a.cmpp(true, FCmp::Eq, x(1), x(6)), &[0x66, 0x0f, 0xc2, 0xce, 0x00], "cmpeqpd\txmm1, xmm6", None),
        (|a| a.cmpp(true, FCmp::Lt, x(1), x(6)), &[0x66, 0x0f, 0xc2, 0xce, 0x01], "cmpltpd\txmm1, xmm6", None),
        (|a| a.cmpp(true, FCmp::Le, x(1), x(6)), &[0x66, 0x0f, 0xc2, 0xce, 0x02], "cmplepd\txmm1, xmm6", None),
        (|a| a.cmpp(true, FCmp::Neq, x(1), x(6)), &[0x66, 0x0f, 0xc2, 0xce, 0x04], "cmpneqpd\txmm1, xmm6", None),
        (|a| a.pshift(PShift::Psrlw, x(5), 2), &[0x66, 0x0f, 0x71, 0xd5, 0x02], "psrlw\txmm5, 0x2", None),
        (|a| a.pshift(PShift::Psraw, x(5), 15), &[0x66, 0x0f, 0x71, 0xe5, 0x0f], "psraw\txmm5, 0xf", None),
        (|a| a.pshift(PShift::Psllw, x(5), 8), &[0x66, 0x0f, 0x71, 0xf5, 0x08], "psllw\txmm5, 0x8", None),
        (|a| a.pshift(PShift::Psrld, x(5), 1), &[0x66, 0x0f, 0x72, 0xd5, 0x01], "psrld\txmm5, 0x1", None),
        (|a| a.pshift(PShift::Psrad, x(5), 31), &[0x66, 0x0f, 0x72, 0xe5, 0x1f], "psrad\txmm5, 0x1f", None),
        (|a| a.pshift(PShift::Pslld, x(5), 31), &[0x66, 0x0f, 0x72, 0xf5, 0x1f], "pslld\txmm5, 0x1f", None),
        (|a| a.pshift(PShift::Psrlq, x(5), 3), &[0x66, 0x0f, 0x73, 0xd5, 0x03], "psrlq\txmm5, 0x3", None),
        (|a| a.pshift(PShift::Psllq, x(5), 63), &[0x66, 0x0f, 0x73, 0xf5, 0x3f], "psllq\txmm5, 0x3f", None),
        (|a| a.movd_xg(x(1), g(0)), &[0x66, 0x0f, 0x6e, 0xc8], "movd\txmm1, eax", None),
        (|a| a.movd_xg(x(2), g(5)), &[0x66, 0x0f, 0x6e, 0xd5], "movd\txmm2, ebp", None),
        (|a| a.movd_gx(g(2), x(3)), &[0x66, 0x0f, 0x7e, 0xda], "movd\tedx, xmm3", None),
        (|a| a.movd_gx(g(0), x(0)), &[0x66, 0x0f, 0x7e, 0xc0], "movd\teax, xmm0", None),
        (|a| a.cvtsi2f(true, x(1), g(0)), &[0xf2, 0x0f, 0x2a, 0xc8], "cvtsi2sd\txmm1, eax", None),
        (|a| a.cvtsi2f(false, x(0), g(0)), &[0xf3, 0x0f, 0x2a, 0xc0], "cvtsi2ss\txmm0, eax", None),
        (|a| a.movmsk(Movmsk::Pmovmskb, g(2), x(1)), &[0x66, 0x0f, 0xd7, 0xd1], "pmovmskb\tedx, xmm1", None),
        (|a| a.movmsk(Movmsk::Movmskps, g(2), x(1)), &[0x0f, 0x50, 0xd1], "movmskps\tedx, xmm1", None),
        (|a| a.movmsk(Movmsk::Movmskpd, g(2), x(7)), &[0x66, 0x0f, 0x50, 0xd7], "movmskpd\tedx, xmm7", None),
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

/// A listing names labels, branch targets and relocations; ELF i386
/// relocations show no addend (it is in the place).
#[test]
fn listing_shows_labels_targets_and_relocations() {
    let mut obj = crate::obj::Obj::new(false, true, false);
    obj.i386 = true;
    let g = obj.sym("g");
    let mut a = Asm::new(true);
    a.push(EBP);
    let top = a.pos();
    a.call(RelKind::I386Call, g, -4);
    a.jcc(Cond::NE, Some(top));
    let back = a.jcc8(Cond::B, None);
    a.ret();
    let end = a.pos();
    a.patch_rel8(back, end);
    let l = Listing {
        name: "f",
        code: &a.code,
        labels: vec![(top as u32, 3)],
        relocs: &a.relocs,
    };
    let lines = a.lines(&l);
    crate::asm::function(&mut obj, &l, &lines, crate::asm::Bytes::Each);
    let want = "
<f>:
       0: 55                           \tpush\tebp
.L3:
       1: e8 fc ff ff ff               \tcall\t0x2 <f+0x2>
\t\t\t00000002:  R_386_PLT32\tg
       6: 0f 85 f5 ff ff ff            \tjne\t0x1 <.L3>
       c: 72 01                        \tjb\t0xf <f+0xf>
       e: c3                           \tret
";
    assert_eq!(obj.asm.as_deref(), Some(want));
}

/// COFF: C names take an `_`, relocations their `IMAGE_REL_I386_` names.
#[test]
fn coff_listing_names() {
    let mut obj = crate::obj::Obj::new(false, false, true);
    obj.i386 = true;
    let g = obj.sym("g");
    let t = obj.sym("t");
    let mut a = Asm::new(true);
    a.call(RelKind::I386Call, g, 0);
    a.mov_ri_sym(EAX, RelKind::I386Abs32, t);
    let l = Listing {
        name: "_f",
        code: &a.code,
        labels: Vec::new(),
        relocs: &a.relocs,
    };
    let lines = a.lines(&l);
    crate::asm::function(&mut obj, &l, &lines, crate::asm::Bytes::Each);
    let want = "
<_f>:
       0: e8 00 00 00 00               \tcall\t0x5 <_f+0x5>
\t\t\t00000001:  IMAGE_REL_I386_REL32\t_g
       5: b8 00 00 00 00               \tmov\teax, 0x0
\t\t\t00000006:  IMAGE_REL_I386_DIR32\t_t
";
    assert_eq!(obj.asm.as_deref(), Some(want));
}
