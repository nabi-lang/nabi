//! Arithmetic, logic, unary operations and conversions.

#[allow(clippy::wildcard_imports)]
use crate::isa::ppc64::*;

impl Em<'_> {
    pub(crate) fn bin(&mut self, op: Bin, k: K, signed: bool, dst: Mem, a: V, b: V) {
        if k.is_float() {
            let fa = self.fsrc(a, k, FA);
            let fb = self.fsrc(b, k, FB);
            let d = self.dst_f(dst).unwrap_or(FA);
            match op {
                Bin::Add => self.fop(k, Fp::Fadd, d, fa, fb),
                Bin::Sub => self.fop(k, Fp::Fsub, d, fa, fb),
                Bin::Mul => self.fmul(k, d, fa, fb),
                _ => self.fop(k, Fp::Fdiv, d, fa, fb),
            }
            self.stf(d, dst, k);
            return;
        }
        let ra = self.src(a, k, signed, R11);
        let d = self.dst_r(dst).unwrap_or(R11);
        // Zero-extended operands keep these zero-extended.
        let mut clean = !signed
            && matches!(
                op,
                Bin::Shr | Bin::Div | Bin::Rem | Bin::And | Bin::Or | Bin::Xor
            );
        match op {
            Bin::Shl | Bin::Shr => {
                if let V::I(i) = b {
                    let sh = (i as u32) & 63;
                    if sh == 0 {
                        self.mr(d, ra);
                    } else {
                        match (op, signed) {
                            (Bin::Shl, _) => self.sldi(d, ra, sh),
                            (_, true) => self.sradi(d, ra, sh),
                            _ => self.srdi(d, ra, sh),
                        }
                    }
                } else {
                    let rb = self.src(b, k, false, R12);
                    let xo = match (op, signed) {
                        (Bin::Shl, _) => Logic::Sld,
                        (_, true) => Logic::Srad,
                        _ => Logic::Srd,
                    };
                    self.logic(xo, d, ra, rb);
                }
            }
            Bin::Rotl => {
                let w = k == K::I64;
                if let V::I(i) = b {
                    let sh = (i as u32) & if w { 63 } else { 31 };
                    if w {
                        self.ins(enc::rldicl(R(d), R(ra), sh, 0));
                    } else {
                        self.ins(enc::rlwinm(R(d), R(ra), sh, 0, 31));
                    }
                } else {
                    let rb = self.src(b, k, false, R12);
                    let (d, ra, rb) = (R(d), R(ra), R(rb));
                    self.ins(if w {
                        enc::rotld(d, ra, rb)
                    } else {
                        enc::rotlw(d, ra, rb)
                    });
                }
            }
            Bin::Add | Bin::Sub
                if matches!(b, V::I(i) if {
                    let v = ext(i, k, signed);
                    fits16(if op == Bin::Sub { v.wrapping_neg() } else { v })
                }) =>
            {
                let V::I(i) = b else { unreachable!() };
                let v = ext(i, k, signed);
                self.addi(d, ra, if op == Bin::Sub { v.wrapping_neg() } else { v });
            }
            Bin::And | Bin::Or | Bin::Xor if matches!(b, V::I(i) if (0..0x1_0000).contains(&ext(i, k, signed))) =>
            {
                let V::I(i) = b else { unreachable!() };
                let o = match op {
                    Bin::And => LogImm::Andi,
                    Bin::Or => LogImm::Ori,
                    _ => LogImm::Xori,
                };
                clean |= op == Bin::And;
                self.logic_imm(o, d, ra, ext(i, k, signed));
            }
            _ => {
                let rb = self.src(b, k, signed, R12);
                match op {
                    Bin::Add => self.alu(Alu::Add, d, ra, rb),
                    Bin::Sub => self.subf(d, rb, ra),
                    Bin::Mul => self.alu(Alu::Mulld, d, ra, rb),
                    Bin::MulHi => {
                        self.alu(if signed { Alu::Mulhd } else { Alu::Mulhdu }, d, ra, rb);
                    }
                    Bin::Div => self.alu(if signed { Alu::Divd } else { Alu::Divdu }, d, ra, rb),
                    Bin::Rem => {
                        self.alu(if signed { Alu::Divd } else { Alu::Divdu }, R10, ra, rb);
                        self.alu(Alu::Mulld, R10, R10, rb);
                        self.subf(d, R10, ra);
                    }
                    Bin::And => self.logic(Logic::And, d, ra, rb),
                    Bin::Or => self.logic(Logic::Or, d, ra, rb),
                    _ => self.logic(Logic::Xor, d, ra, rb),
                }
            }
        }
        self.put(d, dst, k, clean);
    }

    /// `rd` = the low `bits` of `rs` extended by `signed` to 64 bits.
    pub(crate) fn ext_reg(&mut self, rd: u32, rs: u32, bits: u32, signed: bool) {
        match (bits, signed) {
            (64, _) => self.mr(rd, rs),
            (8, true) => self.logic(Logic::Extsb, rd, rs, 0),
            (16, true) => self.logic(Logic::Extsh, rd, rs, 0),
            (32, true) => self.logic(Logic::Extsw, rd, rs, 0),
            _ => self.clrldi(rd, rs, bits),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn ovf(&mut self, op: Bin, k: K, signed: bool, dst: Mem, a: V, b: V, fail: Label) {
        let (ra, rb) = (self.src(a, k, signed, R11), self.src(b, k, signed, R12));
        match op {
            Bin::Add => self.alu(Alu::Add, R10, ra, rb),
            Bin::Sub => self.subf(R10, rb, ra),
            _ => self.alu(Alu::Mulld, R10, ra, rb),
        }
        if k.bits() < 64 {
            // Exact in 64 bits: the result must equal its own extension.
            self.ext_reg(R9, R10, k.bits(), signed);
            self.cmp_reg(true, R9, R10);
            self.br(Cond::f(EQ), fail);
        } else {
            match (op, signed) {
                // Both operands' sign differs from the sum's.
                (Bin::Add, true) => {
                    self.logic(Logic::Xor, R8, ra, R10);
                    self.logic(Logic::Xor, R9, rb, R10);
                    self.logic(Logic::And, R8, R8, R9);
                    self.cmp_imm(R8, 0);
                    self.br(Cond::t(LT), fail);
                }
                (Bin::Add, false) => {
                    self.cmp_reg(false, R10, ra);
                    self.br(Cond::t(LT), fail);
                }
                // Operands of different signs, the result's not `a`'s.
                (Bin::Sub, true) => {
                    self.logic(Logic::Xor, R8, ra, rb);
                    self.logic(Logic::Xor, R9, ra, R10);
                    self.logic(Logic::And, R8, R8, R9);
                    self.cmp_imm(R8, 0);
                    self.br(Cond::t(LT), fail);
                }
                (Bin::Sub, false) => {
                    self.cmp_reg(false, ra, rb);
                    self.br(Cond::t(LT), fail);
                }
                // The high half: the low one's sign (zero).
                (_, true) => {
                    self.alu(Alu::Mulhd, R8, ra, rb);
                    self.sradi(R9, R10, 63);
                    self.cmp_reg(true, R8, R9);
                    self.br(Cond::f(EQ), fail);
                }
                _ => {
                    self.alu(Alu::Mulhdu, R8, ra, rb);
                    self.cmp_imm(R8, 0);
                    self.br(Cond::f(EQ), fail);
                }
            }
        }
        self.st(R10, dst, k);
    }

    /// `dst = a * b + c` rounded once (`fmadd`). With `a`, `b` in the
    /// scratch registers and neither `c` nor `dst` in one, `c` goes in
    /// `f0`, kept below the stack pointer meanwhile.
    pub(crate) fn fma(&mut self, k: K, dst: Mem, a: V, b: V, c: V) {
        let fa = self.fsrc(a, k, FA);
        let fb = self.fsrc(b, k, FB);
        let d = self.dst_f(dst);
        let free = [FA, FB].into_iter().find(|&t| t != fa && t != fb);
        let fc = match self.ra.get_v(c) {
            Some(r) if r.fp => Some(r.reg),
            _ => free.or_else(|| d.filter(|&d| d != fa && d != fb)),
        };
        if let Some(fc) = fc {
            if self.ra.get_v(c).is_none_or(|r| !r.fp || r.reg != fc) {
                self.ldf(fc, c, k);
            }
            let t = d.unwrap_or(FA);
            self.ins(enc::fmadd(k == K::F32, F(t), F(fa), F(fb), F(fc)));
            self.stf(t, dst, k);
            return;
        }
        self.ls(false, K::F64, false, true, 0, SP, -8);
        self.ldf(0, c, k);
        self.ins(enc::fmadd(k == K::F32, F(FA), F(fa), F(fb), F(0)));
        self.ls(true, K::F64, false, true, 0, SP, -8);
        self.stf(FA, dst, k);
    }

    pub(crate) fn un(&mut self, op: Un, k: K, dst: Mem, a: V) {
        if k.is_float() {
            let fa = self.fsrc(a, k, FA);
            let d = self.dst_f(dst).unwrap_or(FA);
            if op == Un::Sqrt {
                self.fop(k, Fp::Fsqrt, d, 0, fa);
            } else {
                self.fop(K::F64, Fp::Fneg, d, 0, fa);
            }
            self.stf(d, dst, k);
            return;
        }
        let bits = k.bits();
        let d = self.dst_r(dst).unwrap_or(R11);
        let clean = match op {
            Un::Neg => {
                let ra = self.src(a, k, true, R11);
                self.alu(Alu::Neg, d, ra, 0);
                false
            }
            Un::Not => {
                let ra = self.src(a, k, false, R11);
                self.logic(Logic::Nor, d, ra, ra);
                false
            }
            Un::Popcnt => {
                let ra = self.src(a, k, false, R11);
                self.logic(Logic::Popcntd, d, ra, 0);
                true
            }
            Un::Clz => {
                let ra = self.src(a, k, false, R11);
                self.logic(Logic::Cntlzd, d, ra, 0);
                if bits < 64 {
                    self.addi(d, d, -i64::from(64 - bits));
                }
                true
            }
            Un::Ctz => {
                // The bits below the lowest set one: `(x - 1) & !x`.
                let ra = self.src(a, k, false, R11);
                self.addi(R12, ra, -1);
                self.logic(Logic::Andc, d, R12, ra);
                if bits < 64 {
                    self.clrldi(d, d, bits);
                }
                self.logic(Logic::Popcntd, d, d, 0);
                true
            }
            Un::Bswap => match a {
                V::I(i) => {
                    let n = k.size() as usize;
                    let b = (i as u64).to_le_bytes();
                    let mut r = [0u8; 8];
                    for j in 0..n {
                        r[j] = b[n - 1 - j];
                    }
                    self.li(d, i64::from_le_bytes(r));
                    true
                }
                V::M(m) => {
                    // A byte-reversed load; a register's through the
                    // scratch.
                    if let Some(a) = self.ra.get(m) {
                        let s = self.scratch;
                        let kk = if a.fp { float_k(k) } else { int_k(k) };
                        self.ls(false, kk, false, a.fp, a.reg, SP, s);
                        self.addi(R9, SP, s);
                    } else {
                        self.lea(R9, m);
                    }
                    match k.size() {
                        1 => self.ls(true, K::I8, false, false, d, R9, 0),
                        n => self.ins(enc::lbrx(n, R(d), R(R9))),
                    }
                    true
                }
            },
            Un::Sqrt => {
                self.ld(d, a, k, false);
                true
            }
        };
        self.put(d, dst, k, clean);
    }

    pub(crate) fn conv(&mut self, conv: Conv, from: K, to: K, dst: Mem, a: V) {
        match conv {
            // Lowered to the runtime call here (`FnLower::half_conv`).
            Conv::Half => unreachable!("no half conversion instruction"),
            Conv::Int { signed } => {
                let d = self.dst_r(dst).unwrap_or(R11);
                self.ld(d, a, from, signed);
                self.put(d, dst, to, !signed && from.bits() <= to.bits());
            }
            Conv::ToBool => {
                let r = self.src(a, from, false, R11);
                self.cmp_imm(r, 0);
                let d = self.dst_r(dst).unwrap_or(R11);
                self.cr_bit(d, EQ);
                self.logic_imm(LogImm::Xori, d, d, 1);
                self.put(d, dst, K::I8, true);
            }
            Conv::IToF { signed } => {
                let r = self.src(a, from, signed, R11);
                let d = self.dst_f(dst).unwrap_or(FA);
                self.gpr_to_fpr(d, r);
                // fcfid(u)(s)
                self.fop(to, if signed { Fp::Fcfid } else { Fp::Fcfidu }, d, 0, d);
                self.stf(d, dst, to);
            }
            Conv::FToI { signed, bits } => self.f_to_i(from, to, dst, a, signed, bits),
            Conv::FToF => {
                let fa = self.fsrc(a, from, FA);
                let d = self.dst_f(dst).unwrap_or(FA);
                if to == K::F32 {
                    self.fop(K::F64, Fp::Frsp, d, 0, fa);
                } else {
                    self.fmr(d, fa);
                }
                self.stf(d, dst, to);
            }
            Conv::Bits => self.mov_val(to, dst, a),
        }
    }

    /// `(max, min)` of `bits` signed / unsigned bits.
    pub(crate) const fn range(signed: bool, bits: u32) -> (i64, i64) {
        if signed {
            ((1i64 << (bits - 1)) - 1, -(1i64 << (bits - 1)))
        } else {
            ((1i64 << bits) - 1, 0)
        }
    }

    /// Float -> integer, saturating, NaN to 0: `fctidz` / `fctiduz`
    /// saturate (the signed one makes NaN the minimum, masked by an
    /// unordered compare), then the clamp to `bits`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn f_to_i(&mut self, from: K, to: K, dst: Mem, a: V, signed: bool, bits: u32) {
        let fa = self.fsrc(a, from, FA);
        let d = self.dst_r(dst).unwrap_or(R11);
        self.fop(
            K::F64,
            if signed { Fp::Fctidz } else { Fp::Fctiduz },
            FB,
            0,
            fa,
        );
        self.fpr_to_gpr(d, FB);
        if signed {
            self.fcmpu(fa, fa);
            let ordered = self.b_fwd(Cond::f(UN));
            self.li(d, 0);
            self.here(ordered);
        }
        if bits < 64 {
            let (max, min) = Self::range(signed, bits);
            self.li(R12, max);
            self.cmp_reg(signed, d, R12);
            let ok = self.b_fwd(Cond::f(GT));
            self.mr(d, R12);
            self.here(ok);
            if signed {
                self.li(R12, min);
                self.cmp_reg(true, d, R12);
                let ok = self.b_fwd(Cond::f(LT));
                self.mr(d, R12);
                self.here(ok);
            }
        }
        self.put(d, dst, to, !signed && bits < 64);
    }
}
