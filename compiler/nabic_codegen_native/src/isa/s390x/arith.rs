//! Arithmetic, logic, unary operations and conversions.

#[allow(clippy::wildcard_imports)]
use crate::isa::s390x::*;

impl Em<'_> {
    /// The even / odd pair of multiply, divide and `flogr`: `r12 r13`
    /// at -O0, else `r2 r3` (r12 r13 hold values).
    pub(crate) const fn pair(&self) -> (u8, u8) {
        if self.opt { (R2, R3) } else { (R12, R13) }
    }

    pub(crate) fn bin(&mut self, op: Bin, k: K, signed: bool, dst: Mem, a: V, b: V) {
        if k.is_float() {
            let fop = match op {
                Bin::Add => [Rre::Aebr, Rre::Adbr],
                Bin::Sub => [Rre::Sebr, Rre::Sdbr],
                Bin::Mul => [Rre::Meebr, Rre::Mdbr],
                _ => [Rre::Debr, Rre::Ddbr],
            };
            let fop = fop[usize::from(k == K::F64)];
            if self.opt {
                // Straight into the destination's register unless it is
                // `b`'s.
                let fb = self.freg(b, k, F3);
                let d = self.dst_f(dst).filter(|&d| d != fb).unwrap_or(F1);
                self.ldf(d, a, k);
                self.a.rre(fop, d, fb);
                self.stf(d, dst, k);
                return;
            }
            self.ldf(F1, a, k);
            self.ldf(F3, b, k);
            self.a.rre(fop, F1, F3);
            self.stf(F1, dst, k);
            return;
        }
        if self.opt && self.bin_opt(op, k, signed, dst, a, b) {
            return;
        }
        // Narrow values computed in 64 bits, extended by `signed`: the low
        // bits are right.
        self.ld(self.x, a, k, signed);
        match op {
            Bin::Shl | Bin::Shr | Bin::Rotl => {
                let sop = match (op, signed) {
                    (Bin::Shl, _) => Rsy::Sllg,
                    (Bin::Rotl, _) => rotl(k),
                    (_, true) => Rsy::Srag,
                    _ => Rsy::Srlg,
                };
                if let V::I(i) = b {
                    self.a.rsy(sop, self.x, self.x, 0, (i & 63) as i32);
                } else {
                    self.ld(self.y, b, k, false);
                    self.a.rsy(sop, self.x, self.x, self.y, 0);
                }
            }
            Bin::Add | Bin::Sub
                if matches!(b, V::I(i) if {
                    let v = ext(i, k, signed);
                    fits32(if op == Bin::Sub { v.wrapping_neg() } else { v })
                }) =>
            {
                let V::I(i) = b else { unreachable!() };
                let v = ext(i, k, signed);
                self.add_imm(self.x, if op == Bin::Sub { v.wrapping_neg() } else { v });
            }
            Bin::Div | Bin::Rem => {
                // The dividend in the odd register (unsigned: the pair),
                // the remainder in the even one, the quotient in the odd.
                let (hi, lo) = self.pair();
                self.ld(self.y, b, k, signed);
                self.lgr(lo, self.x);
                if signed {
                    self.a.rre(Rre::Dsgr, hi, self.y);
                } else {
                    self.li(hi, 0);
                    self.a.rre(Rre::Dlgr, hi, self.y);
                }
                self.lgr(self.x, if op == Bin::Div { lo } else { hi });
            }
            Bin::MulHi => {
                // The unsigned high half; signed, less `b` when `a` is
                // negative and `a` when `b` is.
                let (hi, lo) = self.pair();
                self.ld(self.y, b, k, signed);
                self.lgr(lo, self.x);
                self.a.rre(Rre::Mlgr, hi, self.y);
                if signed {
                    for (s, o) in [(self.x, self.y), (self.y, self.x)] {
                        self.a.rsy(Rsy::Srag, R0, s, 0, 63);
                        self.a.rre(Rre::Ngr, R0, o);
                        self.a.rre(Rre::Sgr, hi, R0);
                    }
                }
                self.lgr(self.x, hi);
            }
            _ => {
                self.ld(self.y, b, k, signed);
                let rop = match op {
                    Bin::Add => Rre::Agr,
                    Bin::Sub => Rre::Sgr,
                    Bin::Mul => Rre::Msgr,
                    Bin::And => Rre::Ngr,
                    Bin::Or => Rre::Ogr,
                    _ => Rre::Xgr,
                };
                self.a.rre(rop, self.x, self.y);
            }
        }
        self.st(self.x, dst, k);
    }

    /// `bin` above -O0 of the operations whose low bits do not depend on
    /// the operands' extension, and of shifts: in the destination's
    /// register when it is not `b`'s.
    pub(crate) fn bin_opt(&mut self, op: Bin, k: K, signed: bool, dst: Mem, a: V, b: V) -> bool {
        let rop = match op {
            Bin::Add => Err(Rre::Agr),
            Bin::Sub => Err(Rre::Sgr),
            Bin::Mul => Err(Rre::Msgr),
            Bin::And => Err(Rre::Ngr),
            Bin::Or => Err(Rre::Ogr),
            Bin::Xor => Err(Rre::Xgr),
            Bin::Shl => Ok(Rsy::Sllg),
            Bin::Shr if signed => Ok(Rsy::Srag),
            Bin::Shr => Ok(Rsy::Srlg),
            Bin::Rotl => Ok(rotl(k)),
            _ => return false,
        };
        let shift = matches!(op, Bin::Shl | Bin::Shr | Bin::Rotl);
        let imm = match b {
            V::I(i) if shift => Some(i & 63),
            V::I(i) if matches!(op, Bin::Add | Bin::Sub) => {
                let v = ext(i, k, signed);
                let v = if op == Bin::Sub { v.wrapping_neg() } else { v };
                fits32(v).then_some(v)
            }
            _ => None,
        };
        let y = match imm {
            Some(_) => R0,
            None => self.reg(b, k, false, self.y),
        };
        let d = self.dst_x(dst).filter(|&d| d != y).unwrap_or(self.x);
        // Only a right shift reads the extension.
        self.ld(d, a, k, signed && op == Bin::Shr);
        match (imm, rop) {
            (Some(i), Ok(s)) => self.a.rsy(s, d, d, 0, i as i32),
            (Some(i), Err(_)) => self.add_imm(d, i),
            (None, Ok(s)) => self.a.rsy(s, d, d, y, 0),
            (None, Err(r)) => self.a.rre(r, d, y),
        }
        self.st(d, dst, k);
        true
    }

    /// `rd` = the low `bits` of `rs` extended by `signed` to 64.
    pub(crate) fn ext_reg(&mut self, rd: u8, rs: u8, bits: u32, signed: bool) {
        let op = match (bits, signed) {
            (8, true) => Rre::Lgbr,
            (16, true) => Rre::Lghr,
            (32, true) => Rre::Lgfr,
            (8, false) => Rre::Llgcr,
            (16, false) => Rre::Llghr,
            (32, false) => Rre::Llgfr,
            _ => Rre::Lgr,
        };
        self.a.rre(op, rd, rs);
    }

    /// `rd` = `rs` zero-extended from `bits` (a move when 64).
    pub(crate) fn ext_reg_mov(&mut self, rd: u8, rs: u8, bits: u32, signed: bool) {
        if bits >= 64 {
            self.lgr(rd, rs);
        } else {
            self.ext_reg(rd, rs, bits, signed);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn ovf(&mut self, op: Bin, k: K, signed: bool, dst: Mem, a: V, b: V, fail: Label) {
        self.ld(self.x, a, k, signed);
        self.ld(self.y, b, k, signed);
        let rop = match op {
            Bin::Add => Rre::Agr,
            Bin::Sub => Rre::Sgr,
            _ => Rre::Msgr,
        };
        if k.bits() < 64 {
            // Exact in 64 bits: the result must equal its own extension.
            self.a.rre(rop, self.x, self.y);
            self.ext_reg(R0, self.x, k.bits(), signed);
            self.a.rre(Rre::Cgr, R0, self.x);
            self.jcc(NE, fail);
        } else {
            match (op, signed) {
                (Bin::Add | Bin::Sub, true) => {
                    self.a.rre(rop, self.x, self.y);
                    self.jcc(CC3, fail);
                }
                // A carry.
                (Bin::Add, false) => {
                    self.a.rre(Rre::Algr, self.x, self.y);
                    self.jcc(CC2 | CC3, fail);
                }
                // A borrow.
                (Bin::Sub, false) => {
                    self.a.rre(Rre::Slgr, self.x, self.y);
                    self.jcc(CC0 | CC1, fail);
                }
                (_, false) => {
                    let (hi, lo) = self.pair();
                    self.lgr(lo, self.x);
                    self.a.rre(Rre::Mlgr, hi, self.y);
                    self.a.rre(Rre::Ltgr, hi, hi);
                    self.jcc(NE, fail);
                    self.lgr(self.x, lo);
                }
                _ => {
                    // The unsigned product's high half, less `b` when `a`
                    // is negative and `a` when `b` is: the signed one,
                    // which must be the low half's sign.
                    let (hi, lo) = self.pair();
                    self.lgr(lo, self.x);
                    self.a.rre(Rre::Mlgr, hi, self.y);
                    for (s, o) in [(self.x, self.y), (self.y, self.x)] {
                        self.a.rsy(Rsy::Srag, R0, s, 0, 63);
                        self.a.rre(Rre::Ngr, R0, o);
                        self.a.rre(Rre::Sgr, hi, R0);
                    }
                    self.a.rsy(Rsy::Srag, R0, lo, 0, 63);
                    self.a.rre(Rre::Cgr, hi, R0);
                    self.jcc(NE, fail);
                    self.lgr(self.x, lo);
                }
            }
        }
        self.st(self.x, dst, k);
    }

    /// `r7` = the set bits of `r7` (`r0` and the pair's even register
    /// scratch).
    pub(crate) fn popcount(&mut self) {
        let rep = |b: u8| -> i64 { i64::from_le_bytes([b; 8]) };
        let t = self.pair().0;
        self.a.rsy(Rsy::Srlg, R0, self.x, 0, 1);
        self.li(t, rep(0x55));
        self.a.rre(Rre::Ngr, R0, t);
        self.a.rre(Rre::Sgr, self.x, R0);
        self.li(t, rep(0x33));
        self.lgr(R0, self.x);
        self.a.rre(Rre::Ngr, R0, t);
        self.a.rsy(Rsy::Srlg, self.x, self.x, 0, 2);
        self.a.rre(Rre::Ngr, self.x, t);
        self.a.rre(Rre::Agr, self.x, R0);
        self.a.rsy(Rsy::Srlg, R0, self.x, 0, 4);
        self.a.rre(Rre::Agr, self.x, R0);
        self.li(t, rep(0x0f));
        self.a.rre(Rre::Ngr, self.x, t);
        self.li(t, rep(0x01));
        self.a.rre(Rre::Msgr, self.x, t);
        self.a.rsy(Rsy::Srlg, self.x, self.x, 0, 56);
    }

    /// `dst = a * b + c` rounded once: `madbr` into `c`'s copy, in the
    /// destination's register or `f1`. With `f3` taken and `b` in no
    /// register, `f5` holds it, kept in the save area meanwhile.
    pub(crate) fn fma(&mut self, k: K, dst: Mem, a: V, b: V, c: V) {
        let reg = |s: &Self, v: V| match v {
            V::M(m) => s.dst_f(m),
            V::I(_) => None,
        };
        // The one in a register first: the product's order is free.
        let (a, b) = if reg(self, a).is_none() && reg(self, b).is_some() {
            (b, a)
        } else {
            (a, b)
        };
        let (ra, rb) = (reg(self, a), reg(self, b));
        let acc = self
            .dst_f(dst)
            .filter(|&d| Some(d) != ra && Some(d) != rb)
            .unwrap_or(F1);
        let fa = self.freg(a, k, F3);
        let dbl = k == K::F64;
        if let Some(fb) = rb {
            self.ldf(acc, c, k);
            self.a.madbr(dbl, acc, fa, fb);
        } else if acc != F1 && fa != F1 {
            self.ldf(F1, b, k);
            self.ldf(acc, c, k);
            self.a.madbr(dbl, acc, fa, F1);
        } else {
            self.rxy_at(Rxy::Stdy, 5, R15, 152);
            self.ldf(5, b, k);
            self.ldf(acc, c, k);
            self.a.madbr(dbl, acc, fa, 5);
            self.rxy_at(Rxy::Ldy, 5, R15, 152);
        }
        self.stf(acc, dst, k);
    }

    pub(crate) fn un(&mut self, op: Un, k: K, dst: Mem, a: V) {
        if k.is_float() {
            let fop = if op == Un::Sqrt {
                [Rre::Sqebr, Rre::Sqdbr]
            } else {
                // The sign flipped, NaNs too.
                [Rre::Lcebr, Rre::Lcdbr]
            };
            let fop = fop[usize::from(k == K::F64)];
            if self.opt {
                let fa = self.freg(a, k, F1);
                let d = self.dst_f(dst).unwrap_or(F1);
                self.a.rre(fop, d, fa);
                self.stf(d, dst, k);
                return;
            }
            self.ldf(F1, a, k);
            self.a.rre(fop, F1, F1);
            self.stf(F1, dst, k);
            return;
        }
        let bits = k.bits();
        match op {
            Un::Neg => {
                self.ld(self.x, a, k, true);
                self.a.rre(Rre::Lcgr, self.x, self.x);
            }
            Un::Not => {
                self.ld(self.x, a, k, false);
                self.li(R0, -1);
                self.a.rre(Rre::Xgr, self.x, R0);
            }
            Un::Popcnt => {
                self.ld(self.x, a, k, false);
                self.popcount();
            }
            Un::Clz => {
                // `flogr`: 64 for 0.
                let hi = self.pair().0;
                self.ld(self.x, a, k, false);
                self.a.rre(Rre::Flogr, hi, self.x);
                self.add_imm(hi, -i64::from(64 - bits));
                self.lgr(self.x, hi);
            }
            Un::Ctz => {
                // 64 - clz(~x & (x - 1)); a narrow value with bit `bits`
                // set gives `bits` for 0.
                self.ld(self.x, a, k, false);
                if bits < 64 {
                    self.li(R0, 1i64 << bits);
                    self.a.rre(Rre::Ogr, self.x, R0);
                }
                let hi = self.pair().0;
                self.lgr(R0, self.x);
                self.add_imm(R0, -1);
                self.li(hi, -1);
                self.a.rre(Rre::Xgr, self.x, hi);
                self.a.rre(Rre::Ngr, self.x, R0);
                self.a.rre(Rre::Flogr, hi, self.x);
                self.li(self.x, 64);
                self.a.rre(Rre::Sgr, self.x, hi);
            }
            Un::Bswap => {
                self.ld(self.x, a, k, false);
                match bits {
                    64 => self.a.rre(Rre::Lrvgr, self.x, self.x),
                    32 => self.a.rre(Rre::Lrvr, self.x, self.x),
                    16 => {
                        self.a.rre(Rre::Lrvr, self.x, self.x);
                        self.a.rsy(Rsy::Srlg, self.x, self.x, 0, 16);
                    }
                    _ => {}
                }
            }
            Un::Sqrt => self.ld(self.x, a, k, false),
        }
        self.st(self.x, dst, k);
    }

    pub(crate) fn conv(&mut self, conv: Conv, from: K, to: K, dst: Mem, a: V) {
        match conv {
            // Lowered to the runtime call here (`FnLower::half_conv`).
            Conv::Half => unreachable!("no half conversion instruction"),
            Conv::Int { signed } => {
                let r = self.dst_x(dst).unwrap_or(self.x);
                self.ld(r, a, from, signed);
                self.st(r, dst, to);
            }
            Conv::ToBool => {
                // `(x | -x) >> 63`.
                self.ld(self.x, a, from, false);
                self.a.rre(Rre::Lcgr, R0, self.x);
                self.a.rre(Rre::Ogr, R0, self.x);
                self.a.rsy(Rsy::Srlg, self.x, R0, 0, 63);
                self.st(self.x, dst, K::I8);
            }
            Conv::IToF { signed } => {
                self.ld(self.x, a, from, signed);
                let cvt = if to == K::F64 { Rre::Cdgbr } else { Rre::Cegbr };
                if !signed && from.size() == 8 {
                    // From 2^63 on: half of it, the low bit kept (rounding
                    // to odd), converted and doubled.
                    self.a.rre(Rre::Ltgr, self.x, self.x);
                    let neg = self.a.brc_fwd(CC1);
                    self.a.rre(cvt, F1, self.x);
                    let done = self.a.brc_fwd(ALWAYS);
                    self.a.here(neg);
                    self.li(R0, 1);
                    self.a.rre(Rre::Ngr, R0, self.x);
                    self.a.rsy(Rsy::Srlg, self.x, self.x, 0, 1);
                    self.a.rre(Rre::Ogr, self.x, R0);
                    self.a.rre(cvt, F1, self.x);
                    self.a
                        .rre(if to == K::F64 { Rre::Adbr } else { Rre::Aebr }, F1, F1);
                    self.a.here(done);
                } else if self.opt {
                    let d = self.dst_f(dst).unwrap_or(F1);
                    self.a.rre(cvt, d, self.x);
                    return self.stf(d, dst, to);
                } else {
                    self.a.rre(cvt, F1, self.x);
                }
                self.stf(F1, dst, to);
            }
            Conv::FToI { signed, bits } => self.f_to_i(from, to, dst, a, signed, bits),
            Conv::FToF => {
                let op = if from == K::F32 {
                    Rre::Ldebr
                } else {
                    Rre::Ledbr
                };
                if self.opt {
                    let fa = self.freg(a, from, F1);
                    let d = self.dst_f(dst).unwrap_or(F1);
                    self.a.rre(op, d, fa);
                    return self.stf(d, dst, to);
                }
                self.ldf(F1, a, from);
                self.a.rre(op, F1, F1);
                self.stf(F1, dst, to);
            }
            Conv::Bits => self.mov_val(to, dst, a),
        }
    }

    /// Float -> integer, saturating, NaN to 0: NaN and the values at or
    /// past the bounds of `bits` by comparisons, the rest by the signed
    /// conversion toward zero (unsigned 64-bit from 2^63: of `x - 2^63`,
    /// the top bit set).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn f_to_i(&mut self, from: K, to: K, dst: Mem, a: V, signed: bool, bits: u32) {
        let d = from == K::F64;
        let fcmp = if d { Rre::Cdbr } else { Rre::Cebr };
        let cvt = if d { Rrf::Cgdbr } else { Rrf::Cgebr };
        let fbits = |x: f64| -> i64 {
            if d {
                x.to_bits() as i64
            } else {
                i64::from((x as f32).to_bits())
            }
        };
        let pow2 = |n: u32| f64::from_bits(u64::from(1023 + n) << 52);
        let (lo, hi) = if signed {
            (-pow2(bits - 1), pow2(bits - 1))
        } else {
            (0.0, pow2(bits))
        };
        let (min, max) = if signed {
            (
                (1i64 << (bits - 1)).wrapping_neg(),
                ((1u64 << (bits - 1)) - 1) as i64,
            )
        } else if bits == 64 {
            (0, -1)
        } else {
            (0, (1i64 << bits) - 1)
        };
        self.ldf(F1, a, from);
        self.a.rre(fcmp, F1, F1);
        let nan = self.a.brc_fwd(CC3);
        self.ldf(F3, V::I(fbits(lo)), from);
        self.a.rre(fcmp, F1, F3);
        let low = self.a.brc_fwd(CC0 | CC1);
        self.ldf(F3, V::I(fbits(hi)), from);
        self.a.rre(fcmp, F1, F3);
        let high = self.a.brc_fwd(CC0 | CC2);
        let mut done = Vec::new();
        if !signed && bits == 64 {
            self.ldf(F3, V::I(fbits(pow2(63))), from);
            self.a.rre(fcmp, F1, F3);
            let small = self.a.brc_fwd(CC1);
            self.a.rre(if d { Rre::Sdbr } else { Rre::Sebr }, F1, F3);
            self.a.rrf(cvt, RTZ, self.x, F1);
            self.li(R0, i64::MIN);
            self.a.rre(Rre::Ogr, self.x, R0);
            done.push(self.a.brc_fwd(ALWAYS));
            self.a.here(small);
        }
        self.a.rrf(cvt, RTZ, self.x, F1);
        done.push(self.a.brc_fwd(ALWAYS));
        self.a.here(nan);
        self.li(self.x, 0);
        done.push(self.a.brc_fwd(ALWAYS));
        self.a.here(low);
        self.li(self.x, min);
        done.push(self.a.brc_fwd(ALWAYS));
        self.a.here(high);
        self.li(self.x, max);
        for at in done {
            self.a.here(at);
        }
        self.st(self.x, dst, to);
    }
}
