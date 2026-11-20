//! `VLoop` on i686 (SSE2, eight `xmm` registers). `eax` the offset of
//! the vector at hand, `ecx` the last offset a vector may start at, `edx`
//! scratch, the bases in `esi edi ebx` (`esi edi` no value lives in where
//! a function has a `VLoop`, `ebx` kept in `io` at `VLoop::save`).
//! Vector registers `xmm0 .. xmm6` by number, `xmm7` scratch: all
//! caller-saved, and a `VLoop` clobbers what a call does.

use crate::isa::i386::enc::{
    Addr, Alu, Cond, FCmp, Gpr, Movmsk, PShift, Rm, Shift, Sse, Sz, Xmm, Xrm,
};
use crate::isa::i386::{EAX, EBX, ECX, EDI, EDX, ESI, Em, gr};
use crate::lir::{VBin, VIO_BASES, VIO_BOUND, VIO_IV, VLoop, VOp, VReg, VShift, VUn};

const OFF: u32 = EAX;
const LIM: u32 = ECX;
const TMP: u32 = EDX;
const SCRATCH: u32 = 7;
const BASES: [u32; 3] = [ESI, EDI, EBX];

/// The bytes of the narrowest element of `v`: the offset's unit.
fn emin(v: &VLoop) -> u32 {
    v.elem.iter().copied().chain([v.w]).min().unwrap_or(v.w)
}

/// The index of lane width `w` (1, 2, 4, 8).
const fn wi(w: u32) -> usize {
    w.trailing_zeros() as usize
}

/// The shift by an immediate of `w`-byte lanes (2, 4, else 8).
fn pshift(op: VShift, w: u32) -> PShift {
    match (op, w) {
        (VShift::Shl, 2) => PShift::Psllw,
        (VShift::ShrU, 2) => PShift::Psrlw,
        (VShift::ShrS, 2) => PShift::Psraw,
        (VShift::Shl, 4) => PShift::Pslld,
        (VShift::ShrU, 4) => PShift::Psrld,
        (VShift::ShrS, 4) => PShift::Psrad,
        (VShift::Shl, _) => PShift::Psllq,
        (VShift::ShrU, _) => PShift::Psrlq,
        (VShift::ShrS, _) => unreachable!("no 64-bit arithmetic shift"),
    }
}

impl Em<'_> {
    pub(crate) fn vloop(&mut self, v: &VLoop) {
        let io = self.slot_off[v.io as usize];
        let at = |o: u32| Rm::M(Self::sp(io + o));
        // The offset counts elements of the narrowest kind.
        let s = emin(v).trailing_zeros() as u8;
        // ecx = bound - min_left; below it no vector fits.
        self.a.mov(Sz::D, Gpr(LIM), at(VIO_BOUND));
        self.a.alu_ri(Alu::Sub, gr(LIM), v.min_left);
        let skip = self.jcc_fwd(Cond::B);
        self.shift_gpr(LIM, Shift::Shl, s);
        let ebx = v.bases.len() > 2;
        if ebx {
            self.a.mov_st(Sz::D, at(v.save), Gpr(EBX));
        }
        for op in &v.pre {
            self.vop(v, op, io, &mut Vec::new());
        }
        self.a.mov(Sz::D, Gpr(OFF), at(VIO_IV));
        self.shift_gpr(OFF, Shift::Shl, s);
        for (n, d) in v.bases.iter().enumerate() {
            let r = BASES[n];
            self.a.mov(Sz::D, Gpr(r), at(VIO_BASES + 8 * n as u32));
            self.a.alu_ri(Alu::Add, gr(r), *d as u32);
        }
        let top = self.a.pos();
        self.alu(Alu::Cmp, OFF, LIM);
        let mut exits = vec![self.jcc_fwd(Cond::A)];
        for op in &v.body {
            self.vop(v, op, io, &mut exits);
        }
        self.a.alu_ri(Alu::Add, gr(OFF), v.vb / v.w * emin(v));
        self.a.jmp(Some(top));
        for e in exits {
            self.here(e);
        }
        self.shift_gpr(OFF, Shift::Shr, s);
        self.a.mov_st(Sz::D, at(VIO_IV), Gpr(OFF));
        for &(r, off) in &v.post {
            let Rm::M(m) = at(off) else { unreachable!() };
            self.a.sse_st(Sse::MovdquSt, m, Xmm(u32::from(r)));
        }
        if ebx {
            self.a.mov(Sz::D, Gpr(EBX), at(v.save));
        }
        self.here(skip);
    }

    /// `shl` / `shr` `r` by `n`.
    fn shift_gpr(&mut self, r: u32, op: Shift, n: u8) {
        if n != 0 {
            self.a.shift_ri(op, Sz::D, Gpr(r), n);
        }
    }

    /// An SSE instruction `xmm d, xmm a`.
    fn xop(&mut self, op: Sse, d: u32, a: u32) {
        self.a.sse(op, Xmm(d), Xrm::X(Xmm(a)));
    }

    fn movdqa(&mut self, d: u32, a: u32) {
        if d != a {
            self.xop(Sse::Movdqa, d, a);
        }
    }

    /// `[base + eax * 2^sc]`.
    const fn indexed(base: u32, sc: u8) -> Addr {
        Addr::Bi(Gpr(base), Gpr(OFF), sc)
    }

    /// A shift of `d` by `n`.
    fn shift_imm(&mut self, op: PShift, d: u32, n: u8) {
        self.a.pshift(op, Xmm(d), n);
    }

    /// Every lane of `d` = the low `w` bytes of `d`'s low lane.
    fn broadcast(&mut self, d: u32, w: u32) {
        if w == 1 {
            self.xop(Sse::Punpcklbw, d, d);
        }
        if w <= 2 {
            self.xop(Sse::Punpcklwd, d, d);
        }
        if w == 8 {
            self.xop(Sse::Punpcklqdq, d, d);
        } else {
            self.a.sse_ri(Sse::Pshufd, Xmm(d), Xmm(d), 0);
        }
    }

    /// Elements of `e` bytes into lanes of twice that, `signed` or not.
    fn widen_step(&mut self, d: u32, e: u32, signed: bool) {
        let unpack = [Sse::Punpcklbw, Sse::Punpcklwd, Sse::Punpckldq][wi(e)];
        if !signed {
            self.xop(Sse::Pxor, SCRATCH, SCRATCH);
            self.xop(unpack, d, SCRATCH);
        } else if e == 4 {
            self.movdqa(SCRATCH, d);
            self.shift_imm(PShift::Psrad, SCRATCH, 31);
            self.xop(Sse::Punpckldq, d, SCRATCH);
        } else {
            self.xop(unpack, d, d);
            let sra = if e == 1 { PShift::Psraw } else { PShift::Psrad };
            self.shift_imm(sra, d, (8 * e) as u8);
        }
    }

    /// `xmm7` = the lanes of `src` (`w` bytes) cut to `e` bytes, in its
    /// low `16 * e / w` bytes.
    fn narrow_sse(&mut self, src: u32, w: u32, e: u32) {
        let t = SCRATCH;
        let mut w = w;
        if w == 8 {
            // The low halves first.
            self.a.sse_ri(Sse::Pshufd, Xmm(t), Xmm(src), 0x08);
            w = 4;
        } else {
            self.movdqa(t, src);
        }
        if w == e {
            return;
        }
        if w == 2 {
            self.shift_imm(PShift::Psllw, t, 8);
            self.shift_imm(PShift::Psrlw, t, 8);
            self.xop(Sse::Packuswb, t, t);
            return;
        }
        let k = (8 * (4 - e)) as u8;
        self.shift_imm(PShift::Pslld, t, k);
        self.shift_imm(PShift::Psrad, t, k);
        self.xop(Sse::Packssdw, t, t);
        if e == 1 {
            self.xop(Sse::Packsswb, t, t);
        }
    }

    /// `op`; a `Bail` adds the position of its branch to `exits`.
    #[allow(clippy::too_many_lines)]
    fn vop(&mut self, v: &VLoop, op: &VOp, io: u32, exits: &mut Vec<usize>) {
        let x = |r: VReg| u32::from(r);
        let sc = |b: u8| (v.elem[b as usize] / emin(v)).trailing_zeros() as u8;
        let base = |b: u8| BASES[b as usize];
        match *op {
            VOp::Load { dst, base: b } => {
                let m = Self::indexed(base(b), sc(b));
                self.a.sse(Sse::Movdqu, Xmm(x(dst)), Xrm::M(m));
            }
            VOp::Store { src, base: b } => {
                let m = Self::indexed(base(b), sc(b));
                self.a.sse_st(Sse::MovdquSt, m, Xmm(x(src)));
            }
            VOp::LoadExt {
                dst,
                base: b,
                signed,
            } => {
                let mut e = v.elem[b as usize];
                let d = x(dst);
                let m = Xrm::M(Self::indexed(base(b), sc(b)));
                if 16 * e / v.w == 8 {
                    self.a.sse(Sse::MovqLd, Xmm(d), m);
                } else {
                    self.a.sse(Sse::MovdLd, Xmm(d), m);
                }
                while e < v.w {
                    self.widen_step(d, e, signed);
                    e *= 2;
                }
            }
            VOp::StoreTrunc { src, base: b } => {
                let e = v.elem[b as usize];
                self.narrow_sse(x(src), v.w, e);
                let m = Self::indexed(base(b), sc(b));
                if 16 * e / v.w == 8 {
                    self.a.sse_st(Sse::MovqSt, m, Xmm(SCRATCH));
                } else {
                    self.a.sse_st(Sse::MovdSt, m, Xmm(SCRATCH));
                }
            }
            VOp::MaskStore { .. } => unreachable!("SSE2 stores no lanes under a mask"),
            VOp::Splat { dst, off } => {
                let m = Xrm::M(Self::sp(io + off));
                if v.w == 8 {
                    self.a.sse(Sse::MovqLd, Xmm(x(dst)), m);
                } else {
                    self.a.sse(Sse::MovdLd, Xmm(x(dst)), m);
                }
                self.broadcast(x(dst), v.w);
            }
            VOp::Const { dst, bits } => {
                let d = x(dst);
                let ones = if v.w == 8 {
                    u64::MAX
                } else {
                    (1u64 << (8 * v.w)) - 1
                };
                if bits == 0 {
                    self.xop(Sse::Pxor, d, d);
                } else if bits == ones {
                    self.xop(Sse::Pcmpeqd, d, d);
                } else {
                    // Through the scratch words at `esp`.
                    self.a.mov_mi(Rm::M(Self::sp(0)), bits as u32);
                    self.a.mov_mi(Rm::M(Self::sp(4)), (bits >> 32) as u32);
                    self.a.sse(Sse::MovqLd, Xmm(d), Xrm::M(Self::sp(0)));
                    self.broadcast(d, v.w);
                }
            }
            VOp::Bin { op, dst, a, b } => self.vbin(v.w, op, x(dst), x(a), x(b)),
            VOp::Un { op, dst, a } => {
                let (d, a) = (x(dst), x(a));
                match op {
                    VUn::FSqrt => {
                        let op = if v.w == 8 { Sse::Sqrtpd } else { Sse::Sqrtps };
                        self.xop(op, d, a);
                    }
                    VUn::Abs => unreachable!("no lane magnitudes here"),
                    VUn::SignMask => match v.w {
                        1 => {
                            let t = if d == a { SCRATCH } else { d };
                            self.xop(Sse::Pxor, t, t);
                            self.xop(Sse::Pcmpgtb, t, a);
                            self.movdqa(d, t);
                        }
                        2 => {
                            self.movdqa(d, a);
                            self.shift_imm(PShift::Psraw, d, 15);
                        }
                        4 => {
                            self.movdqa(d, a);
                            self.shift_imm(PShift::Psrad, d, 31);
                        }
                        _ => {
                            self.movdqa(d, a);
                            self.shift_imm(PShift::Psrad, d, 31);
                            self.a.sse_ri(Sse::Pshufd, Xmm(d), Xmm(d), 0xf5);
                        }
                    },
                }
            }
            VOp::Shift { op, dst, a, n } => {
                let (d, a) = (x(dst), x(a));
                self.movdqa(d, a);
                self.shift_imm(pshift(op, v.w), d, n);
            }
            VOp::Dot { .. } => unreachable!("no dot products here"),
            VOp::Fma { .. } => unreachable!("no fused multiply-add here"),
            VOp::Bail { a } => {
                let a = Xmm(x(a));
                let op = match v.w {
                    1 | 2 => Movmsk::Pmovmskb,
                    4 => Movmsk::Movmskps,
                    _ => Movmsk::Movmskpd,
                };
                self.a.movmsk(op, Gpr(TMP), a);
                if v.w == 2 {
                    // The high byte of each lane.
                    self.a.test_ri(Sz::D, gr(TMP), 0xaaaa);
                } else {
                    self.a.test(Sz::D, gr(TMP), Gpr(TMP));
                }
                exits.push(self.jcc_fwd(Cond::NE));
            }
        }
    }

    fn vbin(&mut self, w: u32, op: VBin, d: u32, a: u32, b: u32) {
        let pd = w == 8;
        let f = |ps: Sse, pd_: Sse| if pd { pd_ } else { ps };
        // (instruction, commutative, cmpps predicate)
        let (sse, comm, pred): (Sse, bool, Option<FCmp>) = match op {
            VBin::Add => (
                [Sse::Paddb, Sse::Paddw, Sse::Paddd, Sse::Paddq][wi(w)],
                true,
                None,
            ),
            VBin::Sub => (
                [Sse::Psubb, Sse::Psubw, Sse::Psubd, Sse::Psubq][wi(w)],
                false,
                None,
            ),
            VBin::Mul => (Sse::Pmullw, true, None),
            VBin::And => (Sse::Pand, true, None),
            VBin::Or => (Sse::Por, true, None),
            VBin::Xor => (Sse::Pxor, true, None),
            VBin::Eq => (
                [Sse::Pcmpeqb, Sse::Pcmpeqw, Sse::Pcmpeqd][wi(w)],
                true,
                None,
            ),
            VBin::GtS => (
                [Sse::Pcmpgtb, Sse::Pcmpgtw, Sse::Pcmpgtd][wi(w)],
                false,
                None,
            ),
            VBin::MinU => (Sse::Pminub, true, None),
            VBin::MaxU => (Sse::Pmaxub, true, None),
            VBin::MinS => (Sse::Pminsw, true, None),
            VBin::MaxS => (Sse::Pmaxsw, true, None),
            VBin::FAdd => (f(Sse::Addps, Sse::Addpd), true, None),
            VBin::FSub => (f(Sse::Subps, Sse::Subpd), false, None),
            VBin::FMul => (f(Sse::Mulps, Sse::Mulpd), true, None),
            VBin::FDiv => (f(Sse::Divps, Sse::Divpd), false, None),
            VBin::FEq => (Sse::Xorps, true, Some(FCmp::Eq)),
            VBin::FNe => (Sse::Xorps, true, Some(FCmp::Neq)),
            VBin::FLt => (Sse::Xorps, false, Some(FCmp::Lt)),
            VBin::FLe => (Sse::Xorps, false, Some(FCmp::Le)),
            // No NaN lanes: either order.
            VBin::FMin => (f(Sse::Minps, Sse::Minpd), true, None),
            VBin::FMax => (f(Sse::Maxps, Sse::Maxpd), true, None),
            VBin::MulHiS => (Sse::Pmulhw, true, None),
            VBin::MulHiU => (Sse::Pmulhuw, true, None),
            VBin::AndNot => {
                // pandn: d = !d & src.
                if d == b {
                    self.xop(Sse::Pandn, d, a);
                } else if d == a {
                    self.movdqa(SCRATCH, b);
                    self.xop(Sse::Pandn, SCRATCH, a);
                    self.movdqa(d, SCRATCH);
                } else {
                    self.movdqa(d, b);
                    self.xop(Sse::Pandn, d, a);
                }
                return;
            }
            VBin::GtU => unreachable!("SSE2 has no unsigned compare"),
        };
        // A compare is `cmpps` / `cmppd` (`sse` unused).
        let emit = |e: &mut Self, d: u32, src: u32| match pred {
            Some(p) => e.a.cmpp(pd, p, Xmm(d), Xmm(src)),
            None => e.xop(sse, d, src),
        };
        if d == a {
            emit(self, d, b);
        } else if d == b && comm {
            emit(self, d, a);
        } else if d == b {
            self.movdqa(SCRATCH, b);
            self.movdqa(d, a);
            emit(self, d, SCRATCH);
        } else {
            self.movdqa(d, a);
            emit(self, d, b);
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::lir::{Func, Inst, VBin, VLoop, VOp};
    use crate::obj::{Obj, Sect};

    /// The code of a function holding one `VLoop` of lane width `w`.
    fn code(w: u32, bases: usize, body: Vec<VOp>) -> Vec<u8> {
        let mut obj = Obj::new(false, true, false);
        obj.i386 = true;
        let sym = obj.sym("f");
        let mut f = Func {
            sym,
            ..Func::default()
        };
        let io = f.slot(256, 16);
        f.insts.push(Inst::VLoop(Box::new(VLoop {
            io,
            w,
            vb: 16,
            min_left: 16 / w,
            bases: vec![16; bases],
            elem: vec![w; bases],
            pre: Vec::new(),
            body,
            post: Vec::new(),
            regs: 7,
            save: 64,
        })));
        f.insts.push(Inst::Ret { vals: Vec::new() });
        super::super::emit(&mut obj, &f);
        obj.sect(Sect::Text).data.clone()
    }

    /// Known encodings (as an assembler writes them); `ebx` kept around
    /// a third base.
    #[test]
    fn encodings() {
        // movdqu xmm3, [esi + eax]
        let got = code(4, 1, vec![VOp::Load { dst: 3, base: 0 }]);
        assert!(got.windows(5).any(|x| x == [0xf3, 0x0f, 0x6f, 0x1c, 0x06]));
        // paddd xmm0, xmm1
        let got = code(
            4,
            1,
            vec![VOp::Bin {
                op: VBin::Add,
                dst: 0,
                a: 0,
                b: 1,
            }],
        );
        assert!(got.windows(4).any(|x| x == [0x66, 0x0f, 0xfe, 0xc1]));
        // movdqu [ebx + eax], xmm2; ebx stored and loaded at io + 64.
        let got = code(4, 3, vec![VOp::Store { src: 2, base: 2 }]);
        assert!(got.windows(5).any(|x| x == [0xf3, 0x0f, 0x7f, 0x14, 0x03]));
        let ebx = |op: u8| got.windows(2).any(|x| x[0] == op && x[1] & 0x3f == 0x1c);
        assert!(ebx(0x89) && ebx(0x8b));
    }
}
