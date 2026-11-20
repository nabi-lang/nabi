//! Arithmetic, logic, unary operations and conversions.

#[allow(clippy::wildcard_imports)]
use crate::isa::x64::*;

impl Em<'_> {
    /// `op dst, imm`: above `-O0` the imm8 form where it fits.
    pub(crate) fn alu_imm(&mut self, op: Alu, sz: Sz, dst: Rm, imm: i32) {
        match i8::try_from(imm) {
            Ok(i) if self.opt => self.a.alu_ri8(op, sz, dst, i),
            _ => self.a.alu_ri(op, sz, dst, imm),
        }
    }

    /// `imul dst, src, imm`, as `alu_imm`.
    pub(crate) fn imul_imm(&mut self, sz: Sz, dst: Gpr, src: Rm, imm: i32) {
        match i8::try_from(imm) {
            Ok(i) if self.opt => self.a.imul_ri8(sz, dst, src, i),
            _ => self.a.imul_ri(sz, dst, src, imm),
        }
    }

    /// `rd = ra op imm` (`ra` is `rd` or `rd` is free) when the immediate
    /// fits 32 bits (any for 32-bit ops); not for division.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn bin_imm(
        &mut self,
        op: Bin,
        k: K,
        signed: bool,
        rd: u32,
        ra: u32,
        imm: i64,
    ) -> bool {
        let w = k == K::I64;
        let Some(i) = (if w {
            i32::try_from(imm).ok()
        } else {
            Some(imm as i32)
        }) else {
            return false;
        };
        let bits = if w { 63 } else { 31 };
        match op {
            Bin::Add | Bin::Or | Bin::And | Bin::Sub | Bin::Xor => {
                self.mov_rr(rd, ra);
                self.alu_imm(alu(op), Sz::dq(w), Rm::r(rd), i);
            }
            Bin::Mul => {
                self.w(rd, false);
                self.imul_imm(Sz::dq(w), Gpr(rd), Rm::r(ra), i);
            }
            Bin::Shl | Bin::Shr | Bin::Rotl => {
                self.mov_rr(rd, ra);
                self.a
                    .shift_ri(shift(op, signed), Sz::dq(w), Gpr(rd), (i & bits) as u8);
            }
            Bin::Div | Bin::Rem | Bin::MulHi => return false,
        }
        true
    }

    /// `rd op= rb` for all but division (variable shifts: `rb` is `rcx`).
    pub(crate) fn bin_rr(&mut self, op: Bin, k: K, signed: bool, rd: u32, rb: u32) {
        let s = Sz::dq(k == K::I64);
        match op {
            Bin::Add | Bin::Sub | Bin::And | Bin::Or | Bin::Xor => {
                self.a.alu_rr(alu(op), s, Gpr(rd), Gpr(rb));
            }
            Bin::Mul => self.a.imul(s, Gpr(rd), Rm::r(rb)),
            Bin::Shl | Bin::Shr | Bin::Rotl => self.a.shift_cl(shift(op, signed), s, Gpr(rd)),
            Bin::Div | Bin::Rem | Bin::MulHi => unreachable!("goes through rax"),
        }
    }

    pub(crate) fn bin_int(&mut self, op: Bin, k: K, signed: bool) {
        // rax = rax op rcx
        let s = Sz::dq(k == K::I64);
        match op {
            Bin::Div | Bin::Rem => {
                if signed {
                    self.a.cqo(s);
                    self.a.un(enc::Un::Idiv, s, Rm::r(RCX));
                } else {
                    self.alu(Alu::Xor, false, RDX, RDX);
                    self.a.un(enc::Un::Div, s, Rm::r(RCX));
                }
                if op == Bin::Rem {
                    self.a.mov_st(Sz::Q, Rm::r(RAX), Gpr(RDX));
                }
            }
            Bin::MulHi => {
                let op = if signed { enc::Un::Imul } else { enc::Un::Mul };
                self.a.un(op, s, Rm::r(RCX));
                self.a.mov_st(Sz::Q, Rm::r(RAX), Gpr(RDX));
            }
            _ => self.bin_rr(op, k, signed, RAX, RCX),
        }
    }

    /// `dst = a & imm` compared with zero next: `test` when only that
    /// `Cmp` reads it, else the `and` itself; the flags left for the `Cmp`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn test(
        &mut self,
        op: Bin,
        k: K,
        signed: bool,
        dst: Mem,
        a: V,
        b: V,
        next: Option<&Inst>,
    ) -> bool {
        let V::I(imm) = b else { return false };
        let Some(Inst::Cmp {
            cc: Cc::Eq | Cc::Ne,
            k: ck,
            a: V::M(t),
            b: V::I(0),
            ..
        }) = next
        else {
            return false;
        };
        if op != Bin::And
            || *t != dst
            || *ck != k
            || !matches!(k, K::I32 | K::I64)
            || i32::try_from(imm).is_err()
            || matches!(a, V::I(_))
        {
            return false;
        }
        let w = k == K::I64;
        let once = self.ra.get(dst).is_none()
            && matches!(dst, Mem::Frame(s, o) if self.waits.get(&(s, o)) == Some(&(k.size(), 1)));
        if once {
            let ra = self.reg(a, k, signed, &[]);
            self.a.test_ri(Sz::dq(w), Rm::r(ra), imm as i32);
            self.wrote(dst, k.size());
        } else {
            // The `and` sets the flags; storing leaves them.
            self.bin(op, k, signed, dst, a, b);
        }
        self.tested = Some(dst);
        true
    }

    /// Division / remainder by a power of two: shifts and a mask.
    pub(crate) fn div_pow2(&mut self, op: Bin, k: K, signed: bool, dst: Mem, a: V, b: V) -> bool {
        let V::I(imm) = b else { return false };
        let bits: u32 = if k == K::I64 { 64 } else { 32 };
        if !matches!(k, K::I32 | K::I64)
            || imm <= 1
            || !(imm as u64).is_power_of_two()
            || imm.trailing_zeros() >= bits - 1
            || matches!(a, V::I(_))
        {
            return false;
        }
        let n = imm.trailing_zeros();
        match (op, signed) {
            (Bin::Div, false) => self.bin(Bin::Shr, k, false, dst, a, V::I(i64::from(n))),
            (Bin::Rem, false) if n < 31 => self.bin(Bin::And, k, false, dst, a, V::I(imm - 1)),
            (Bin::Div, true) => {
                let w = k == K::I64;
                let ra = self.reg(a, k, signed, &[]);
                let rd = self
                    .dst_r(dst)
                    .filter(|&d| d != ra)
                    .unwrap_or_else(|| self.pick(&POOL, false, &[ra]));
                self.mov_rr(rd, ra);
                let sh = |s: &mut Self, op: Shift, by: u32| {
                    s.a.shift_ri(op, Sz::dq(w), Gpr(rd), by as u8);
                };
                if n > 1 {
                    sh(self, Shift::Sar, bits - 1);
                }
                sh(self, Shift::Shr, bits - n);
                self.alu(Alu::Add, w, rd, ra);
                sh(self, Shift::Sar, n);
                self.store_ext(rd, dst, k, (k == K::I32).then_some(false));
            }
            _ => return false,
        }
        true
    }

    pub(crate) fn bin(&mut self, op: Bin, k: K, signed: bool, dst: Mem, a: V, b: V) {
        if k.is_float() {
            let va = self.freg(a, k, &[]);
            let vb = if b == a { va } else { self.freg(b, k, &[va]) };
            // Not `vb`, unless it is `va` too: the copy of `va` comes first.
            let vd = self
                .dst_x(dst)
                .filter(|&d| d != vb || d == va)
                .unwrap_or_else(|| self.pick(&FP_POOL, true, &[vb]));
            self.w(vd, true);
            if vd != va {
                self.a.sse(Sse::Movaps, Xmm(vd), Xrm::X(Xmm(va)));
            }
            let op = match op {
                Bin::Add => Self::scalar(k, Sse::Addss, Sse::Addsd),
                Bin::Sub => Self::scalar(k, Sse::Subss, Sse::Subsd),
                Bin::Mul => Self::scalar(k, Sse::Mulss, Sse::Mulsd),
                _ => Self::scalar(k, Sse::Divss, Sse::Divsd),
            };
            self.a.sse(op, Xmm(vd), Xrm::X(Xmm(vb)));
            self.store_fp(vd, dst, k);
            return;
        }
        if matches!(op, Bin::Div | Bin::Rem | Bin::MulHi) {
            self.load(RCX, b, k, signed);
            self.load(RAX, a, k, signed);
            self.scratch();
            self.bin_int(op, k, signed);
            self.store(RAX, dst, k);
            return;
        }
        // A 32-bit operation zero-extends its result.
        let ext = (self.opt && k == K::I32).then_some(false);
        let target = self.dst_r(dst);
        if let V::I(imm) = b {
            let ra = self.reg(a, k, signed, &[]);
            let rd = target.unwrap_or_else(|| self.pick(&POOL, false, &[]));
            if self.bin_imm(op, k, signed, rd, ra, imm) {
                self.store_ext(rd, dst, k, ext);
                return;
            }
        }
        let shift = matches!(op, Bin::Shl | Bin::Shr | Bin::Rotl);
        let (ra, rb) = if shift {
            // The amount in cl.
            self.load(RCX, b, k, signed);
            (self.reg(a, k, signed, &[RCX]), RCX)
        } else {
            let ra = self.reg(a, k, signed, &[]);
            let rb = if b == a {
                ra
            } else {
                self.reg(b, k, signed, &[ra])
            };
            (ra, rb)
        };
        // Not `rb`, unless it is `ra` too: the copy of `ra` comes first.
        let rd = target
            .filter(|&d| d != rb || d == ra)
            .unwrap_or_else(|| self.pick(&POOL, false, &[rb]));
        self.mov_rr(rd, ra);
        self.bin_rr(op, k, signed, rd, rb);
        self.store_ext(rd, dst, k, ext);
    }

    /// `lea rd, [rn + rm * 2^sh]`.
    pub(crate) fn lea_sib(&mut self, rd: u32, rn: u32, rm: u32, sh: u32) {
        self.w(rd, false);
        self.a.lea(Gpr(rd), Addr::Bi(Gpr(rn), Gpr(rm), sh as u8));
    }

    /// `dst = a * b + c` rounded once: `vfmadd231` into a copy of `c`.
    pub(crate) fn fma(&mut self, k: K, dst: Mem, a: V, b: V, c: V) {
        let va = self.freg(a, k, &[]);
        let vb = self.freg(b, k, &[va]);
        let vc = self.freg(c, k, &[va, vb]);
        let vd = self
            .dst_x(dst)
            .filter(|&d| d == vc || (d != va && d != vb))
            .unwrap_or_else(|| self.pick(&FP_POOL, true, &[va, vb, vc]));
        self.w(vd, true);
        if vd != vc {
            self.a.sse(Sse::Movaps, Xmm(vd), Xrm::X(Xmm(vc)));
        }
        let op = if k == K::F64 {
            Avx::Vfmadd231sd
        } else {
            Avx::Vfmadd231ss
        };
        self.a.vex_rrr(op, false, Xmm(vd), Xmm(va), Xmm(vb));
        self.store_fp(vd, dst, k);
    }

    pub(crate) fn un(&mut self, op: Un, k: K, dst: Mem, a: V) {
        let s = Sz::dq(k == K::I64);
        let bits = k.bits();
        if op == Un::Sqrt {
            let sqrt = Self::scalar(k, Sse::Sqrtss, Sse::Sqrtsd);
            if let Some(vd) = self.dst_x(dst) {
                let va = self.freg(a, k, &[]);
                self.w(vd, true);
                self.a.sse(sqrt, Xmm(vd), Xrm::X(Xmm(va)));
                return;
            }
            self.load_fp(XMM0, a, k);
            self.scratch();
            self.a.sse(sqrt, Xmm(XMM0), Xrm::X(Xmm(XMM0)));
            self.store_fp(XMM0, dst, k);
            return;
        }
        self.load(RAX, a, k, op == Un::Neg && !k.is_float());
        self.scratch();
        match op {
            Un::Sqrt => unreachable!("sqrt is a float operation"),
            Un::Neg if k.is_float() => {
                // Flip the sign bit.
                if k == K::F64 {
                    self.mov_imm(RCX, 1 << 63);
                    self.alu(Alu::Xor, true, RAX, RCX);
                } else {
                    self.a.alu_eax(Alu::Xor, i32::MIN);
                }
            }
            Un::Neg => self.a.un(enc::Un::Neg, s, Rm::r(RAX)),
            Un::Not => self.a.un(enc::Un::Not, s, Rm::r(RAX)),
            Un::Popcnt if self.obj.popcnt => {
                // Of the zero-extended value.
                self.a.bits(Bits::Popcnt, Sz::Q, Gpr(RAX), Rm::r(RAX));
            }
            Un::Clz if self.obj.lzcnt => {
                // Less the bits above the value's.
                self.a.bits(Bits::Lzcnt, Sz::Q, Gpr(RAX), Rm::r(RAX));
                if bits < 64 {
                    let n = (64 - bits) as i8;
                    self.a.alu_ri8(Alu::Sub, Sz::Q, Rm::r(RAX), n);
                }
            }
            Un::Ctz if self.obj.bmi => {
                if bits < 64 {
                    // A zero value counts its width.
                    self.a.bts(Sz::Q, Gpr(RAX), bits as u8);
                }
                self.a.bits(Bits::Tzcnt, Sz::Q, Gpr(RAX), Rm::r(RAX));
            }
            Un::Popcnt => {
                // The classic bit-slicing count, 64-bit.
                let step = |s: &mut Self, sh: u8, mask: u64| {
                    s.a.mov_st(Sz::Q, Rm::r(RCX), Gpr(RAX));
                    s.a.shift_ri(Shift::Shr, Sz::Q, Gpr(RCX), sh);
                    s.mov_imm(RDX, mask);
                    s.alu(Alu::And, true, RCX, RDX);
                    s.alu(Alu::And, true, RAX, RDX);
                };
                // x -= (x >> 1) & 0x55..
                self.a.mov_st(Sz::Q, Rm::r(RCX), Gpr(RAX));
                self.a.shift_1(Shift::Shr, Sz::Q, Gpr(RCX));
                self.mov_imm(RDX, 0x5555_5555_5555_5555);
                self.alu(Alu::And, true, RCX, RDX);
                self.alu(Alu::Sub, true, RAX, RCX);
                // x = (x & 0x33..) + ((x >> 2) & 0x33..)
                step(self, 2, 0x3333_3333_3333_3333);
                self.alu(Alu::Add, true, RAX, RCX);
                // x = (x + (x >> 4)) & 0x0f..
                self.a.mov_st(Sz::Q, Rm::r(RCX), Gpr(RAX));
                self.a.shift_ri(Shift::Shr, Sz::Q, Gpr(RCX), 4);
                self.alu(Alu::Add, true, RAX, RCX);
                self.mov_imm(RDX, 0x0f0f_0f0f_0f0f_0f0f);
                self.alu(Alu::And, true, RAX, RDX);
                // x = (x * 0x0101..) >> 56
                self.mov_imm(RDX, 0x0101_0101_0101_0101);
                self.a.imul(Sz::Q, Gpr(RAX), Rm::r(RDX));
                self.a.shift_ri(Shift::Shr, Sz::Q, Gpr(RAX), 56);
            }
            Un::Clz | Un::Ctz => {
                self.a.test_rr(Sz::Q, Gpr(RAX), Gpr(RAX));
                let zero = self.jcc_fwd(Cond::E);
                if op == Un::Clz {
                    self.a.bits(Bits::Bsr, Sz::Q, Gpr(RAX), Rm::r(RAX));
                    self.mov_imm(RCX, u64::from(bits - 1));
                    self.alu(Alu::Sub, true, RCX, RAX);
                    self.a.mov_st(Sz::Q, Rm::r(RAX), Gpr(RCX));
                } else {
                    self.a.bits(Bits::Bsf, Sz::Q, Gpr(RAX), Rm::r(RAX));
                }
                let done = self.jmp_fwd();
                self.here(zero);
                self.mov_imm(RAX, u64::from(bits));
                self.here(done);
            }
            Un::Bswap => match k {
                K::I16 => self.a.shift_ri(Shift::Rol, Sz::W, Gpr(RAX), 8),
                K::I32 => self.a.bswap(Sz::D, Gpr(RAX)),
                K::I64 => self.a.bswap(Sz::Q, Gpr(RAX)),
                _ => {}
            },
        }
        self.store(RAX, dst, k);
    }

    pub(crate) fn conv(&mut self, conv: Conv, from: K, to: K, dst: Mem, a: V) {
        match conv {
            Conv::Int { signed } => {
                let r = self.reg(a, from, signed, &[]);
                // Widened: extended as the destination's own bits.
                let ext = (to.size() > from.size()).then_some(signed);
                self.store_ext(r, dst, to, ext);
            }
            Conv::ToBool => {
                self.load(RAX, a, from, false);
                self.scratch();
                self.a.test_rr(Sz::Q, Gpr(RAX), Gpr(RAX));
                self.setcc(Cond::NE);
                self.store(RAX, dst, K::I8);
            }
            Conv::IToF { signed } => {
                self.load(RAX, a, from, signed);
                self.scratch();
                let pd = to != K::F32;
                let cvt = |s: &mut Self, r: u32| s.a.cvtsi2f(pd, Xmm(XMM0), Gpr(r));
                if !signed && from == K::I64 {
                    // u64 >= 2^63: halve (keeping the low bit), convert,
                    // double.
                    self.a.test_rr(Sz::Q, Gpr(RAX), Gpr(RAX));
                    let big = self.jcc_fwd(Cond::S);
                    cvt(self, RAX);
                    let done = self.jmp_fwd();
                    self.here(big);
                    self.a.mov_st(Sz::Q, Rm::r(RCX), Gpr(RAX));
                    self.a.shift_1(Shift::Shr, Sz::Q, Gpr(RCX));
                    self.a.alu_ri8(Alu::And, Sz::D, Rm::r(RAX), 1);
                    self.alu(Alu::Or, true, RCX, RAX);
                    cvt(self, RCX);
                    let add = Self::scalar(to, Sse::Addss, Sse::Addsd);
                    self.a.sse(add, Xmm(XMM0), Xrm::X(Xmm(XMM0)));
                    self.here(done);
                } else {
                    cvt(self, RAX);
                }
                self.store_fp(XMM0, dst, to);
            }
            Conv::FToI { signed, bits } => self.f_to_i(from, to, dst, a, signed, bits),
            Conv::FToF => {
                self.load_fp(XMM0, a, from);
                self.scratch();
                let cvt = Self::scalar(from, Sse::Cvtss2sd, Sse::Cvtsd2ss);
                self.a.sse(cvt, Xmm(XMM0), Xrm::X(Xmm(XMM0)));
                self.store_fp(XMM0, dst, to);
            }
            Conv::Bits => {
                let r = self.reg(a, from, false, &[]);
                self.store(r, dst, to);
            }
            Conv::Half if from == K::I16 => {
                self.load(RAX, a, K::I16, false);
                self.scratch();
                self.a.movq_xg(Sz::D, Xmm(XMM0), Gpr(RAX));
                self.a.vcvtph2ps(Xmm(XMM0), Xmm(XMM0));
                self.store_fp(XMM0, dst, K::F32);
            }
            Conv::Half => {
                self.load_fp(XMM0, a, K::F32);
                self.scratch();
                self.a.vcvtps2ph(Xmm(XMM0), Xmm(XMM0));
                self.a.movq_gx(Sz::D, Gpr(RAX), Xmm(XMM0));
                self.store(RAX, dst, K::I16);
            }
        }
    }

    /// Float -> integer, saturating, NaN to 0 (`bits` of the destination are
    /// the integer's).
    pub(crate) fn f_to_i(&mut self, from: K, to: K, dst: Mem, a: V, signed: bool, bits: u32) {
        let pd = from != K::F32;
        let ucomi = Self::scalar(from, Sse::Ucomiss, Sse::Ucomisd);
        // xmm1 = 2^63 (as `from`).
        let two63 = if from == K::F64 {
            (63f64.exp2()).to_bits()
        } else {
            u64::from(63f32.exp2().to_bits())
        };
        self.load_fp(XMM0, a, from);
        self.scratch();
        self.mov_imm(RAX, two63);
        self.a.movq_xg(Sz::dq(from == K::F64), Xmm(XMM1), Gpr(RAX));
        let cvtt = |s: &mut Self| s.a.cvttf2si(pd, Gpr(RAX), Xmm(XMM0));
        let ucomi = |s: &mut Self, x: u32, y: u32| s.a.sse(ucomi, Xmm(x), Xrm::X(Xmm(y)));
        let mut ends = Vec::new();
        // NaN (and, unsigned, negative): 0.
        ucomi(self, XMM0, XMM0);
        let mut zeros = vec![self.jcc_fwd(Cond::P)];
        if signed {
            ucomi(self, XMM0, XMM1);
            let big = self.jcc_fwd(Cond::AE);
            cvtt(self); // too negative: i64::MIN
            ends.push(self.jmp_fwd());
            self.here(big);
            self.mov_imm(RAX, i64::MAX as u64);
        } else {
            // >= 2^64: max; >= 2^63: through the sign bit.
            self.a.sse(Sse::Xorps, Xmm(2), Xrm::X(Xmm(2)));
            ucomi(self, XMM0, 2);
            zeros.push(self.jcc_fwd(Cond::B));
            ucomi(self, XMM0, XMM1);
            let high = self.jcc_fwd(Cond::AE);
            cvtt(self);
            ends.push(self.jmp_fwd());
            self.here(high);
            // Less 2^63.
            let sub = Self::scalar(from, Sse::Subss, Sse::Subsd);
            self.a.sse(sub, Xmm(XMM0), Xrm::X(Xmm(XMM1)));
            ucomi(self, XMM0, XMM1);
            let huge = self.jcc_fwd(Cond::AE);
            cvtt(self);
            self.mov_imm(RCX, 1 << 63);
            self.alu(Alu::Xor, true, RAX, RCX);
            ends.push(self.jmp_fwd());
            self.here(huge);
            self.mov_imm(RAX, u64::MAX);
        }
        ends.push(self.jmp_fwd());
        for z in zeros {
            self.here(z);
        }
        self.alu(Alu::Xor, false, RAX, RAX);
        for e in ends {
            self.here(e);
        }
        if bits < 64 {
            let (max, min) = if signed {
                ((1i64 << (bits - 1)) - 1, -(1i64 << (bits - 1)))
            } else {
                ((1i64 << bits) - 1, 0)
            };
            self.mov_imm(RCX, max as u64);
            self.alu(Alu::Cmp, true, RAX, RCX);
            let cc = if signed { Cond::G } else { Cond::A };
            self.a.cmov(cc, Sz::Q, Gpr(RAX), Rm::r(RCX));
            if signed {
                self.mov_imm(RCX, min as u64);
                self.alu(Alu::Cmp, true, RAX, RCX);
                self.a.cmov(Cond::L, Sz::Q, Gpr(RAX), Rm::r(RCX));
            }
        }
        self.store(RAX, dst, to);
    }
}
