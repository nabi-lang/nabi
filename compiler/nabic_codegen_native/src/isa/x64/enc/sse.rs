//! SSE, AVX and x87 forms.

#[allow(clippy::wildcard_imports)]
use crate::isa::x64::enc::*;

/// A legacy-encoded SSE instruction: `(prefix, opcode after 0F, name,
/// memory operand size)`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Sse {
    Movss,
    Movsd,
    MovssSt,
    MovsdSt,
    Movaps,
    MovapsSt,
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
    pub(crate) const fn enc(self) -> (Option<u8>, u8, &'static str, Ptr) {
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
            Self::MovapsSt => (NP, 0x29, "movaps", Ptr::X),
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

/// SSE shifts by an immediate (`66 0F 71` / `72` / `73` `/n ib`).
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
    /// `(opcode, /n, name, VEX name)`.
    pub(crate) const fn enc(self) -> (u8, u32, &'static str, &'static str) {
        match self {
            Self::Psrlw => (0x71, 2, "psrlw", "vpsrlw"),
            Self::Psraw => (0x71, 4, "psraw", "vpsraw"),
            Self::Psllw => (0x71, 6, "psllw", "vpsllw"),
            Self::Psrld => (0x72, 2, "psrld", "vpsrld"),
            Self::Psrad => (0x72, 4, "psrad", "vpsrad"),
            Self::Pslld => (0x72, 6, "pslld", "vpslld"),
            Self::Psrlq => (0x73, 2, "psrlq", "vpsrlq"),
            Self::Psllq => (0x73, 6, "psllq", "vpsllq"),
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
    /// The mnemonic (`pd`: double, `vex`: the VEX form).
    pub(crate) const fn name(self, pd: bool, vex: bool) -> &'static str {
        const N: [&str; 16] = [
            "cmpeqps",
            "cmpeqpd",
            "vcmpeqps",
            "vcmpeqpd",
            "cmpltps",
            "cmpltpd",
            "vcmpltps",
            "vcmpltpd",
            "cmpleps",
            "cmplepd",
            "vcmpleps",
            "vcmplepd",
            "cmpneqps",
            "cmpneqpd",
            "vcmpneqps",
            "vcmpneqpd",
        ];
        let row = match self {
            Self::Eq => 0,
            Self::Lt => 1,
            Self::Le => 2,
            Self::Neq => 3,
        };
        N[4 * row + 2 * vex as usize + pd as usize]
    }
}

/// A VEX-encoded instruction: `(pp, map, W, opcode, name)`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Avx {
    Movdqu,
    MovdquSt,
    /// `vmovq xmm, r64`.
    MovqXg,
    Pmovsxbw,
    Pmovsxbd,
    Pmovsxwd,
    Pmovsxwq,
    Pmovsxdq,
    Pmovzxbw,
    Pmovzxbd,
    Pmovzxwd,
    Pmovzxwq,
    Pmovzxdq,
    Pbroadcastb,
    Pbroadcastw,
    Pbroadcastd,
    Pbroadcastq,
    Pmaskmovd,
    Pmaskmovq,
    Permq,
    Pshufd,
    Packuswb,
    Packssdw,
    Paddb,
    Paddw,
    Paddd,
    Paddq,
    Psubb,
    Psubw,
    Psubd,
    Psubq,
    Pmullw,
    Pmulld,
    Pand,
    Pandn,
    Por,
    Pxor,
    Pcmpeqb,
    Pcmpeqw,
    Pcmpeqd,
    Pcmpeqq,
    Pcmpgtb,
    Pcmpgtw,
    Pcmpgtd,
    Pcmpgtq,
    Pminub,
    Pminuw,
    Pminud,
    Pmaxub,
    Pmaxuw,
    Pmaxud,
    Pminsb,
    Pminsw,
    Pminsd,
    Pmaxsb,
    Pmaxsw,
    Pmaxsd,
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
    Pmovmskb,
    Movmskps,
    Movmskpd,
    Minps,
    Minpd,
    Maxps,
    Maxpd,
    Pmulhw,
    Pmulhuw,
    /// `dst += a * b`, rounded once (FMA).
    Vfmadd231ps,
    Vfmadd231pd,
    Vfmadd231ss,
    Vfmadd231sd,
}

/// VEX `pp`.
pub(crate) const NP: u8 = 0;

pub(crate) const P66: u8 = 1;

pub(crate) const F3: u8 = 2;

/// VEX `mmmmm`.
pub(crate) const M0F: u8 = 1;

pub(crate) const M38: u8 = 2;

pub(crate) const M3A: u8 = 3;

impl Avx {
    pub(crate) const fn enc(self) -> (u8, u8, bool, u8, &'static str) {
        match self {
            Self::Movdqu => (F3, M0F, false, 0x6f, "vmovdqu"),
            Self::MovdquSt => (F3, M0F, false, 0x7f, "vmovdqu"),
            Self::MovqXg => (P66, M0F, true, 0x6e, "vmovq"),
            Self::Pmovsxbw => (P66, M38, false, 0x20, "vpmovsxbw"),
            Self::Pmovsxbd => (P66, M38, false, 0x21, "vpmovsxbd"),
            Self::Pmovsxwd => (P66, M38, false, 0x23, "vpmovsxwd"),
            Self::Pmovsxwq => (P66, M38, false, 0x24, "vpmovsxwq"),
            Self::Pmovsxdq => (P66, M38, false, 0x25, "vpmovsxdq"),
            Self::Pmovzxbw => (P66, M38, false, 0x30, "vpmovzxbw"),
            Self::Pmovzxbd => (P66, M38, false, 0x31, "vpmovzxbd"),
            Self::Pmovzxwd => (P66, M38, false, 0x33, "vpmovzxwd"),
            Self::Pmovzxwq => (P66, M38, false, 0x34, "vpmovzxwq"),
            Self::Pmovzxdq => (P66, M38, false, 0x35, "vpmovzxdq"),
            Self::Pbroadcastb => (P66, M38, false, 0x78, "vpbroadcastb"),
            Self::Pbroadcastw => (P66, M38, false, 0x79, "vpbroadcastw"),
            Self::Pbroadcastd => (P66, M38, false, 0x58, "vpbroadcastd"),
            Self::Pbroadcastq => (P66, M38, false, 0x59, "vpbroadcastq"),
            Self::Pmaskmovd => (P66, M38, false, 0x8e, "vpmaskmovd"),
            Self::Pmaskmovq => (P66, M38, true, 0x8e, "vpmaskmovq"),
            Self::Permq => (P66, M3A, true, 0x00, "vpermq"),
            Self::Pshufd => (P66, M0F, false, 0x70, "vpshufd"),
            Self::Packuswb => (P66, M0F, false, 0x67, "vpackuswb"),
            Self::Packssdw => (P66, M0F, false, 0x6b, "vpackssdw"),
            Self::Paddb => (P66, M0F, false, 0xfc, "vpaddb"),
            Self::Paddw => (P66, M0F, false, 0xfd, "vpaddw"),
            Self::Paddd => (P66, M0F, false, 0xfe, "vpaddd"),
            Self::Paddq => (P66, M0F, false, 0xd4, "vpaddq"),
            Self::Psubb => (P66, M0F, false, 0xf8, "vpsubb"),
            Self::Psubw => (P66, M0F, false, 0xf9, "vpsubw"),
            Self::Psubd => (P66, M0F, false, 0xfa, "vpsubd"),
            Self::Psubq => (P66, M0F, false, 0xfb, "vpsubq"),
            Self::Pmullw => (P66, M0F, false, 0xd5, "vpmullw"),
            Self::Pmulld => (P66, M38, false, 0x40, "vpmulld"),
            Self::Pand => (P66, M0F, false, 0xdb, "vpand"),
            Self::Pandn => (P66, M0F, false, 0xdf, "vpandn"),
            Self::Por => (P66, M0F, false, 0xeb, "vpor"),
            Self::Pxor => (P66, M0F, false, 0xef, "vpxor"),
            Self::Pcmpeqb => (P66, M0F, false, 0x74, "vpcmpeqb"),
            Self::Pcmpeqw => (P66, M0F, false, 0x75, "vpcmpeqw"),
            Self::Pcmpeqd => (P66, M0F, false, 0x76, "vpcmpeqd"),
            Self::Pcmpeqq => (P66, M38, false, 0x29, "vpcmpeqq"),
            Self::Pcmpgtb => (P66, M0F, false, 0x64, "vpcmpgtb"),
            Self::Pcmpgtw => (P66, M0F, false, 0x65, "vpcmpgtw"),
            Self::Pcmpgtd => (P66, M0F, false, 0x66, "vpcmpgtd"),
            Self::Pcmpgtq => (P66, M38, false, 0x37, "vpcmpgtq"),
            Self::Pminub => (P66, M0F, false, 0xda, "vpminub"),
            Self::Pminuw => (P66, M38, false, 0x3a, "vpminuw"),
            Self::Pminud => (P66, M38, false, 0x3b, "vpminud"),
            Self::Pmaxub => (P66, M0F, false, 0xde, "vpmaxub"),
            Self::Pmaxuw => (P66, M38, false, 0x3e, "vpmaxuw"),
            Self::Pmaxud => (P66, M38, false, 0x3f, "vpmaxud"),
            Self::Pminsb => (P66, M38, false, 0x38, "vpminsb"),
            Self::Pminsw => (P66, M0F, false, 0xea, "vpminsw"),
            Self::Pminsd => (P66, M38, false, 0x39, "vpminsd"),
            Self::Pmaxsb => (P66, M38, false, 0x3c, "vpmaxsb"),
            Self::Pmaxsw => (P66, M0F, false, 0xee, "vpmaxsw"),
            Self::Pmaxsd => (P66, M38, false, 0x3d, "vpmaxsd"),
            Self::Addps => (NP, M0F, false, 0x58, "vaddps"),
            Self::Addpd => (P66, M0F, false, 0x58, "vaddpd"),
            Self::Subps => (NP, M0F, false, 0x5c, "vsubps"),
            Self::Subpd => (P66, M0F, false, 0x5c, "vsubpd"),
            Self::Mulps => (NP, M0F, false, 0x59, "vmulps"),
            Self::Mulpd => (P66, M0F, false, 0x59, "vmulpd"),
            Self::Divps => (NP, M0F, false, 0x5e, "vdivps"),
            Self::Divpd => (P66, M0F, false, 0x5e, "vdivpd"),
            Self::Sqrtps => (NP, M0F, false, 0x51, "vsqrtps"),
            Self::Sqrtpd => (P66, M0F, false, 0x51, "vsqrtpd"),
            Self::Pmovmskb => (P66, M0F, false, 0xd7, "vpmovmskb"),
            Self::Movmskps => (NP, M0F, false, 0x50, "vmovmskps"),
            Self::Movmskpd => (P66, M0F, false, 0x50, "vmovmskpd"),
            Self::Minps => (NP, M0F, false, 0x5d, "vminps"),
            Self::Minpd => (P66, M0F, false, 0x5d, "vminpd"),
            Self::Maxps => (NP, M0F, false, 0x5f, "vmaxps"),
            Self::Maxpd => (P66, M0F, false, 0x5f, "vmaxpd"),
            Self::Pmulhw => (P66, M0F, false, 0xe5, "vpmulhw"),
            Self::Pmulhuw => (P66, M0F, false, 0xe4, "vpmulhuw"),
            Self::Vfmadd231ps => (P66, M38, false, 0xb8, "vfmadd231ps"),
            Self::Vfmadd231pd => (P66, M38, true, 0xb8, "vfmadd231pd"),
            Self::Vfmadd231ss => (P66, M38, false, 0xb9, "vfmadd231ss"),
            Self::Vfmadd231sd => (P66, M38, true, 0xb9, "vfmadd231sd"),
        }
    }
}

impl Asm {
    /// `fld` a float (`D` / `Q`).
    pub fn fld(&mut self, sz: Sz, m: Addr) {
        let at = self.pos();
        let opc = if sz == Sz::D { 0xd9 } else { 0xdd };
        self.legacy(None, false, false, &[opc], 0, Ea::M(m));
        self.rec(at, "fld", [Op::M(m, sz.ptr()), Op::No, Op::No]);
    }

    /// `fstp` a float (`D` / `Q`).
    pub fn fstp(&mut self, sz: Sz, m: Addr) {
        let at = self.pos();
        let opc = if sz == Sz::D { 0xd9 } else { 0xdd };
        self.legacy(None, false, false, &[opc], 3, Ea::M(m));
        self.rec(at, "fstp", [Op::M(m, sz.ptr()), Op::No, Op::No]);
    }

    // SSE.

    /// `op dst, src`.
    pub fn sse(&mut self, op: Sse, dst: Xmm, src: Xrm) {
        let (pre, opc, mn, ptr) = op.enc();
        let at = self.pos();
        self.legacy(pre, false, false, &[0x0f, opc], dst.0, src.into());
        self.rec(at, mn, [Op::X(dst, false), xrm_op(src, ptr), Op::No]);
    }

    /// `op m, src` (the store forms).
    pub fn sse_st(&mut self, op: Sse, m: Addr, src: Xmm) {
        let (pre, opc, mn, ptr) = op.enc();
        let at = self.pos();
        self.legacy(pre, false, false, &[0x0f, opc], src.0, Ea::M(m));
        self.rec(at, mn, [Op::M(m, ptr), Op::X(src, false), Op::No]);
    }

    /// `op dst, src, imm8` (`pshufd`).
    pub fn sse_ri(&mut self, op: Sse, dst: Xmm, src: Xmm, imm: u8) {
        let (pre, opc, mn, _) = op.enc();
        let at = self.pos();
        self.legacy(pre, false, false, &[0x0f, opc], dst.0, Ea::R(src.0));
        self.b(imm);
        self.rec(
            at,
            mn,
            [Op::X(dst, false), Op::X(src, false), Op::I(i64::from(imm))],
        );
    }

    /// `cmp<pred>ps` / `pd dst, src` (`0F C2 ib`).
    pub fn cmpp(&mut self, pd: bool, pred: FCmp, dst: Xmm, src: Xmm) {
        let at = self.pos();
        let pre = pd.then_some(0x66);
        self.legacy(pre, false, false, &[0x0f, 0xc2], dst.0, Ea::R(src.0));
        self.b(pred as u8);
        self.rec(
            at,
            pred.name(pd, false),
            [Op::X(dst, false), Op::X(src, false), Op::No],
        );
    }

    /// A shift of `dst` by `n` (`66 0F 71..73 /n ib`).
    pub fn pshift(&mut self, op: PShift, dst: Xmm, n: u8) {
        let (opc, ext, mn, _) = op.enc();
        let at = self.pos();
        self.legacy(Some(0x66), false, false, &[0x0f, opc], ext, Ea::R(dst.0));
        self.b(n);
        self.rec(at, mn, [Op::X(dst, false), Op::I(i64::from(n)), Op::No]);
    }

    /// `movq` (`Q`) / `movd xmm, r` (`66 0F 6E`).
    pub fn movq_xg(&mut self, sz: Sz, dst: Xmm, src: Gpr) {
        let at = self.pos();
        self.legacy(
            Some(0x66),
            sz.w(),
            false,
            &[0x0f, 0x6e],
            dst.0,
            Ea::R(src.0),
        );
        let mn = if sz.w() { "movq" } else { "movd" };
        self.rec(at, mn, [Op::X(dst, false), Op::G(src, sz), Op::No]);
    }

    /// `movq` (`Q`) / `movd r, xmm` (`66 0F 7E`).
    pub fn movq_gx(&mut self, sz: Sz, dst: Gpr, src: Xmm) {
        let at = self.pos();
        self.legacy(
            Some(0x66),
            sz.w(),
            false,
            &[0x0f, 0x7e],
            src.0,
            Ea::R(dst.0),
        );
        let mn = if sz.w() { "movq" } else { "movd" };
        self.rec(at, mn, [Op::G(dst, sz), Op::X(src, false), Op::No]);
    }

    /// `cvtsi2ss` / `cvtsi2sd xmm, r64` (`pd`: double).
    pub fn cvtsi2f(&mut self, pd: bool, dst: Xmm, src: Gpr) {
        let at = self.pos();
        let pre = if pd { 0xf2 } else { 0xf3 };
        self.legacy(Some(pre), true, false, &[0x0f, 0x2a], dst.0, Ea::R(src.0));
        let mn = if pd { "cvtsi2sd" } else { "cvtsi2ss" };
        self.rec(at, mn, [Op::X(dst, false), Op::G(src, Sz::Q), Op::No]);
    }

    /// `cvttss2si` / `cvttsd2si r64, xmm`.
    pub fn cvttf2si(&mut self, pd: bool, dst: Gpr, src: Xmm) {
        let at = self.pos();
        let pre = if pd { 0xf2 } else { 0xf3 };
        self.legacy(Some(pre), true, false, &[0x0f, 0x2c], dst.0, Ea::R(src.0));
        let mn = if pd { "cvttsd2si" } else { "cvttss2si" };
        self.rec(at, mn, [Op::G(dst, Sz::Q), Op::X(src, false), Op::No]);
    }

    /// `pmovmskb` / `movmskps` / `movmskpd r32, xmm`.
    pub fn movmsk(&mut self, op: Avx, dst: Gpr, src: Xmm) {
        let (pp, _, _, opc, mn) = op.enc();
        let mn = &mn[1..];
        let pre = (pp == P66).then_some(0x66);
        let at = self.pos();
        self.legacy(pre, false, false, &[0x0f, opc], dst.0, Ea::R(src.0));
        self.rec(at, mn, [Op::G(dst, Sz::D), Op::X(src, false), Op::No]);
    }

    // AVX.

    /// `vop dst, a, b` (`l`: `ymm`).
    pub fn vex_rrr(&mut self, op: Avx, l: bool, dst: Xmm, a: Xmm, b: Xmm) {
        let (pp, map, w, opc, mn) = op.enc();
        let at = self.pos();
        self.vex(pp, map, w, l, opc, dst.0, a.0, Ea::R(b.0));
        self.rec(at, mn, [Op::X(dst, l), Op::X(a, l), Op::X(b, l)]);
    }

    /// `vop dst, src` (no `vvvv`); `xs`: the source an `xmm`.
    pub fn vex_rr(&mut self, op: Avx, l: bool, dst: Xmm, src: Xmm, xs: bool) {
        let (pp, map, w, opc, mn) = op.enc();
        let at = self.pos();
        self.vex(pp, map, w, l, opc, dst.0, 0, Ea::R(src.0));
        self.rec(at, mn, [Op::X(dst, l), Op::X(src, l && !xs), Op::No]);
    }

    /// `vop dst, m` (`ptr`: the memory operand's size).
    pub fn vex_ld(&mut self, op: Avx, l: bool, dst: Xmm, m: Addr, ptr: Ptr) {
        let (pp, map, w, opc, mn) = op.enc();
        let at = self.pos();
        self.vex(pp, map, w, l, opc, dst.0, 0, Ea::M(m));
        self.rec(at, mn, [Op::X(dst, l), Op::M(m, ptr), Op::No]);
    }

    /// `vop m, src` (a store).
    pub fn vex_st(&mut self, op: Avx, l: bool, m: Addr, src: Xmm) {
        let (pp, map, w, opc, mn) = op.enc();
        let at = self.pos();
        self.vex(pp, map, w, l, opc, src.0, 0, Ea::M(m));
        let ptr = if l { Ptr::Y } else { Ptr::X };
        self.rec(at, mn, [Op::M(m, ptr), Op::X(src, l), Op::No]);
    }

    /// `vpmaskmov m, mask, src`.
    pub fn vex_maskst(&mut self, op: Avx, l: bool, m: Addr, mask: Xmm, src: Xmm) {
        let (pp, map, w, opc, mn) = op.enc();
        let at = self.pos();
        self.vex(pp, map, w, l, opc, src.0, mask.0, Ea::M(m));
        let ptr = if l { Ptr::Y } else { Ptr::X };
        self.rec(at, mn, [Op::M(m, ptr), Op::X(mask, l), Op::X(src, l)]);
    }

    /// `vop dst, src, imm8` (`vpshufd`, `vpermq`).
    pub fn vex_ri(&mut self, op: Avx, l: bool, dst: Xmm, src: Xmm, imm: u8) {
        let (pp, map, w, opc, mn) = op.enc();
        let at = self.pos();
        self.vex(pp, map, w, l, opc, dst.0, 0, Ea::R(src.0));
        self.b(imm);
        self.rec(
            at,
            mn,
            [Op::X(dst, l), Op::X(src, l), Op::I(i64::from(imm))],
        );
    }

    /// `vcmp<pred>ps` / `pd dst, a, b`.
    pub fn vex_cmpp(&mut self, pd: bool, l: bool, pred: FCmp, dst: Xmm, a: Xmm, b: Xmm) {
        let at = self.pos();
        let pp = if pd { P66 } else { NP };
        self.vex(pp, M0F, false, l, 0xc2, dst.0, a.0, Ea::R(b.0));
        self.b(pred as u8);
        self.rec(
            at,
            pred.name(pd, true),
            [Op::X(dst, l), Op::X(a, l), Op::X(b, l)],
        );
    }

    /// `vop dst, src, n`: a shift by an immediate.
    pub fn vex_shift(&mut self, op: PShift, l: bool, dst: Xmm, src: Xmm, n: u8) {
        let (opc, ext, _, mn) = op.enc();
        let at = self.pos();
        self.vex(P66, M0F, false, l, opc, ext, dst.0, Ea::R(src.0));
        self.b(n);
        self.rec(at, mn, [Op::X(dst, l), Op::X(src, l), Op::I(i64::from(n))]);
    }

    /// `vmovq xmm, r64`.
    pub fn vmovq_xg(&mut self, dst: Xmm, src: Gpr) {
        let (pp, map, w, opc, mn) = Avx::MovqXg.enc();
        let at = self.pos();
        self.vex(pp, map, w, false, opc, dst.0, 0, Ea::R(src.0));
        self.rec(at, mn, [Op::X(dst, false), Op::G(src, Sz::Q), Op::No]);
    }

    /// `vpmovmskb` / `vmovmskps` / `vmovmskpd r32, ymm`.
    pub fn vex_movmsk(&mut self, op: Avx, l: bool, dst: Gpr, src: Xmm) {
        let (pp, map, w, opc, mn) = op.enc();
        let at = self.pos();
        self.vex(pp, map, w, l, opc, dst.0, 0, Ea::R(src.0));
        self.rec(at, mn, [Op::G(dst, Sz::D), Op::X(src, l), Op::No]);
    }

    /// `vcvtph2ps dst, src`: four `f16` to `f32` (F16C).
    pub fn vcvtph2ps(&mut self, dst: Xmm, src: Xmm) {
        let at = self.pos();
        self.vex(P66, M38, false, false, 0x13, dst.0, 0, Ea::R(src.0));
        self.rec(
            at,
            "vcvtph2ps",
            [Op::X(dst, false), Op::X(src, false), Op::No],
        );
    }

    /// `vcvtps2ph dst, src, 0`: four `f32` to `f16`, to nearest even
    /// whatever MXCSR says (F16C).
    pub fn vcvtps2ph(&mut self, dst: Xmm, src: Xmm) {
        let at = self.pos();
        self.vex(P66, M3A, false, false, 0x1d, src.0, 0, Ea::R(dst.0));
        self.b(0);
        self.rec(
            at,
            "vcvtps2ph",
            [Op::X(dst, false), Op::X(src, false), Op::I(0)],
        );
    }

    pub fn vzeroupper(&mut self) {
        let at = self.pos();
        self.code.extend_from_slice(&[0xc5, 0xf8, 0x77]);
        self.rec(at, "vzeroupper", [Op::No; 3]);
    }
}
