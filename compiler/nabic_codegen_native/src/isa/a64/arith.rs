//! Arithmetic, logic, unary operations and conversions.

#[allow(clippy::wildcard_imports)]
use crate::isa::a64::*;

impl Em<'_> {
    pub(crate) fn bin_int(&mut self, op: Bin, k: K, signed: bool, rd: u32, a: u32, b: u32) {
        let s = sf(k);
        let (rd, a, b) = (R(rd), R(a), R(b));
        let div = if signed { Dp2::Sdiv } else { Dp2::Udiv };
        let w = match op {
            Bin::Add => enc::add_reg(AddOp::Add, s, rd, a, b, Shift::Lsl, 0),
            Bin::Sub => enc::add_reg(AddOp::Sub, s, rd, a, b, Shift::Lsl, 0),
            Bin::Mul => enc::mul(s, rd, a, b),
            Bin::MulHi => {
                let op = if signed { Dp3::Smulh } else { Dp3::Umulh };
                enc::dp3(op, true, rd, a, b, enc::ZR)
            }
            Bin::Div => enc::dp2(div, s, rd, a, b),
            Bin::Rem => {
                let q = R(X12);
                self.ins(enc::dp2(div, s, q, a, b));
                // msub rd, q, b, a
                enc::dp3(Dp3::Msub, s, rd, q, b, a)
            }
            Bin::And => enc::log_reg(Log::And, false, s, rd, a, b),
            Bin::Or => enc::log_reg(Log::Orr, false, s, rd, a, b),
            Bin::Xor => enc::log_reg(Log::Eor, false, s, rd, a, b),
            Bin::Shl => enc::dp2(Dp2::Lsl, s, rd, a, b),
            Bin::Shr => enc::dp2(if signed { Dp2::Asr } else { Dp2::Lsr }, s, rd, a, b),
            // Left by `b` is right by `-b`.
            Bin::Rotl => {
                let n = R(X12);
                self.ins(enc::add_reg(AddOp::Sub, s, n, enc::ZR, b, Shift::Lsl, 0));
                enc::dp2(Dp2::Ror, s, rd, a, n)
            }
        };
        self.ins(w);
    }

    /// `rd = ra <op> imm` in one instruction, when `imm` encodes: add /
    /// sub of 12 bits, shifts, a multiply by a power of two.
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
        let s = sf(k);
        let bits: i64 = if s { 64 } else { 32 };
        let (rd, ra) = (R(rd), R(ra));
        let lsl = |n: i64| enc::lsl(s, rd, ra, n as u32);
        let w = match op {
            Bin::Add | Bin::Sub if (-4095..4096).contains(&imm) => {
                let sub = (op == Bin::Sub) != (imm < 0);
                let op = if sub { AddOp::Sub } else { AddOp::Add };
                enc::add_imm(op, s, rd, ra, imm.unsigned_abs() as u32, false)
            }
            Bin::Mul
                if imm > 0
                    && (imm as u64).is_power_of_two()
                    && i64::from(imm.trailing_zeros()) < bits =>
            {
                lsl(i64::from(imm.trailing_zeros()))
            }
            Bin::Shl => lsl(imm & (bits - 1)),
            // Unsigned by a power of two: a shift, a mask.
            Bin::Div
                if self.opt
                    && !signed
                    && imm > 1
                    && (imm as u64).is_power_of_two()
                    && imm.trailing_zeros() < bits as u32 =>
            {
                enc::shr(false, s, rd, ra, imm.trailing_zeros())
            }
            Bin::Rem
                if self.opt
                    && !signed
                    && imm > 1
                    && (imm as u64).is_power_of_two()
                    && imm.trailing_zeros() < bits as u32 =>
            {
                let Some(m) = enc::bitmask((imm - 1) as u64, bits as u32) else {
                    return false;
                };
                enc::log_imm(Log::And, s, rd, ra, m)
            }
            // Signed by 2^n: round toward zero, `(a + (a < 0 ? 2^n - 1 : 0)) >> n`.
            Bin::Div
                if self.opt
                    && signed
                    && imm > 1
                    && (imm as u64).is_power_of_two()
                    && i64::from(imm.trailing_zeros()) < bits - 1 =>
            {
                let n = imm.trailing_zeros();
                let b = bits as u32;
                let add = |rm, n| enc::add_reg(AddOp::Add, s, rd, ra, rm, Shift::Lsr, n);
                if n == 1 {
                    self.ins(add(ra, b - 1));
                } else {
                    if rd == ra {
                        return false;
                    }
                    self.ins(enc::shr(true, s, rd, ra, b - 1));
                    self.ins(add(rd, b - n));
                }
                self.ins(enc::shr(true, s, rd, rd, n));
                return true;
            }
            Bin::Shr => enc::shr(signed, s, rd, ra, (imm & (bits - 1)) as u32),
            Bin::Rotl => enc::ror(s, rd, ra, ((bits - (imm & (bits - 1))) % bits) as u32),
            Bin::And | Bin::Or | Bin::Xor if self.opt => {
                let Some(m) = enc::bitmask(imm as u64, bits as u32) else {
                    return false;
                };
                let op = match op {
                    Bin::And => Log::And,
                    Bin::Or => Log::Orr,
                    _ => Log::Eor,
                };
                enc::log_imm(op, s, rd, ra, m)
            }
            _ => return false,
        };
        self.ins(w);
        true
    }

    /// `dst = a & imm` compared with zero next: `ands`, the flags left
    /// for that `Cmp`, the result not kept when only it reads it.
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
        let bits = if sf(k) { 64 } else { 32 };
        let Some(m) = enc::bitmask(imm as u64, bits) else {
            return false;
        };
        if op != Bin::And || *t != dst || *ck != k || matches!(a, V::I(_)) {
            return false;
        }
        let ra = self.reg(a, k, signed, &[]);
        let once = self.ra.get(dst).is_none()
            && matches!(dst, Mem::Frame(s, o) if self.waits.get(&(s, o)) == Some(&(k.size(), 1)));
        let rd = if once {
            ZR
        } else {
            self.dst_x(dst).unwrap_or_else(|| self.pick(&POOL, &[ra]))
        };
        self.ins(enc::log_imm(Log::Ands, sf(k), R(rd), R(ra), m));
        if once {
            self.wrote(dst, k.size());
        } else {
            self.store_ext(rd, dst, k, Some(false));
        }
        self.tested = Some(dst);
        true
    }

    /// `cmp rn, #imm` (or `cmn`) when the immediate `imm` of `k`
    /// (extended by `signed`, as `load` would) encodes.
    pub(crate) fn cmp_imm(&mut self, k: K, signed: bool, rn: u32, imm: i64) -> bool {
        let bits = k.bits();
        let x = if bits == 64 {
            imm
        } else if signed {
            (imm << (64 - bits)) >> (64 - bits)
        } else {
            imm & ((1i64 << bits) - 1)
        };
        // 8 / 16 / 32 bits compare as 32.
        let v = if sf(k) { x } else { i64::from(x as i32) };
        if !(-4095..4096).contains(&v) {
            return false;
        }
        self.ins(enc::cmp_imm(sf(k), R(rn), v));
        true
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn ovf(&mut self, op: Bin, k: K, signed: bool, dst: Mem, a: V, b: V, fail: Label) {
        // Above `-O0` operands stay where they are and an add / sub
        // writes the destination's register at once: the fail path, a
        // panic, reads none of them.
        let x9 = if self.opt {
            self.reg(a, k, signed, &[X11, X12])
        } else {
            self.load(X9, a, k, signed);
            X9
        };
        let rd = if self.opt {
            self.dst_x(dst).unwrap_or(X11)
        } else {
            X11
        };
        if let (K::I64 | K::I32, Bin::Add | Bin::Sub, V::I(imm @ 0..4096)) = (k, op, b) {
            let op_ = if op == Bin::Add {
                AddOp::Adds
            } else {
                AddOp::Subs
            };
            self.ins(enc::add_imm(op_, sf(k), R(rd), R(x9), imm as u32, false));
            let cond = ovf_cond(op, signed);
            self.bcond(cond, fail);
            self.store(rd, dst, k);
            return;
        }
        let x10 = if self.opt {
            self.reg(b, k, signed, &[x9, X11, X12])
        } else {
            self.load(X10, b, k, signed);
            X10
        };
        let mut out = X11;
        match (k, op) {
            (K::I64 | K::I32, Bin::Add | Bin::Sub) => {
                let op_ = if op == Bin::Add {
                    AddOp::Adds
                } else {
                    AddOp::Subs
                };
                let x = k == K::I64;
                self.ins(enc::add_reg(op_, x, R(rd), R(x9), R(x10), Shift::Lsl, 0));
                out = rd;
                self.bcond(ovf_cond(op, signed), fail);
            }
            (K::I64, _) => {
                // Straight into the destination's register when the high
                // half still reads both operands after it.
                if self.opt && rd != x9 && rd != x10 && rd != X12 {
                    out = rd;
                }
                let (a, b, x12) = (R(x9), R(x10), R(X12));
                self.ins(enc::mul(true, R(out), a, b));
                if signed {
                    self.ins(enc::dp3(Dp3::Smulh, true, x12, a, b, enc::ZR));
                    // cmp x12, out, asr #63
                    let c = enc::add_reg(AddOp::Subs, true, enc::ZR, x12, R(out), Shift::Asr, 63);
                    self.ins(c);
                } else {
                    self.ins(enc::dp3(Dp3::Umulh, true, x12, a, b, enc::ZR));
                    self.ins(enc::cmp(true, x12, enc::ZR));
                }
                self.bcond(Cond::NE, fail);
            }
            (K::I32, _) => {
                // smull / umull x11, w9, w10; the product must fit 32 bits.
                let (m, ext) = if signed {
                    (Dp3::Smaddl, enc::Ext::Sxtw)
                } else {
                    (Dp3::Umaddl, enc::Ext::Uxtw)
                };
                let x11 = R(X11);
                self.ins(enc::dp3(m, true, x11, R(x9), R(x10), enc::ZR));
                self.ins(enc::add_ext(AddOp::Subs, true, enc::ZR, x11, x11, ext));
                self.bcond(Cond::NE, fail);
            }
            _ => {
                // 8 / 16 bits, computed in 32 from extended operands: the
                // result must equal its own extension.
                self.bin_int(op, K::I32, signed, X11, x9, x10);
                let ext = match (k, signed) {
                    (K::I8, true) => enc::Ext::Sxtb,
                    (K::I16, true) => enc::Ext::Sxth,
                    (K::I8, false) => enc::Ext::Uxtb,
                    _ => enc::Ext::Uxth,
                };
                let x11 = R(X11);
                self.ins(enc::add_ext(AddOp::Subs, false, enc::ZR, x11, x11, ext));
                self.bcond(Cond::NE, fail);
            }
        }
        self.store(out, dst, k);
    }

    pub(crate) fn un(&mut self, op: Un, k: K, dst: Mem, a: V) {
        if k.is_float() {
            let va = self.freg(a, k, &[]);
            let vd = self
                .dst_v(dst)
                .unwrap_or_else(|| self.pick(&FP_POOL, &[va]));
            let op = if op == Un::Sqrt { Fp::Fsqrt } else { Fp::Fneg };
            self.ins(enc::fp(op, k == K::F64, Vr(vd), Vr(va), Vr(0)));
            self.store_fp(vd, dst, k);
            return;
        }
        let s = sf(k);
        let bits = k.bits();
        let x9 = R(X9);
        match op {
            Un::Neg => {
                self.load(X9, a, k, true);
                self.ins(enc::add_reg(AddOp::Sub, s, x9, enc::ZR, x9, Shift::Lsl, 0));
            }
            Un::Not => {
                self.load(X9, a, k, false);
                self.ins(enc::log_reg(Log::Orr, true, s, x9, enc::ZR, x9));
            }
            Un::Popcnt => {
                self.load(X9, a, k, false);
                let v = Vr(V16);
                self.ins(enc::fmov_gpr(false, true, V16, X9));
                self.ins(enc::neon_misc(enc::NeonMisc::Cnt8, 0, v, v));
                self.ins(enc::neon_misc(enc::NeonMisc::Addv8, 0, v, v));
                self.ins(enc::fmov_gpr(true, false, X9, V16));
            }
            Un::Clz => {
                self.load(X9, a, k, false);
                self.ins(enc::dp1(Dp1::Clz, s, x9, x9));
                if bits < 32 {
                    self.ins(enc::add_imm(AddOp::Sub, false, x9, x9, 32 - bits, false));
                }
            }
            Un::Ctz => {
                self.load(X9, a, k, false);
                if bits < 32 {
                    self.mov_imm(X10, 1 << bits);
                    self.ins(enc::log_reg(Log::Orr, false, false, x9, x9, R(X10)));
                }
                self.ins(enc::dp1(Dp1::Rbit, s, x9, x9));
                self.ins(enc::dp1(Dp1::Clz, s, x9, x9));
            }
            Un::Bswap => {
                self.load(X9, a, k, false);
                match k {
                    K::I16 => self.ins(enc::dp1(Dp1::Rev16, false, x9, x9)),
                    K::I32 => self.ins(enc::dp1(Dp1::Rev32, false, x9, x9)),
                    K::I64 => self.ins(enc::dp1(Dp1::Rev64, true, x9, x9)),
                    _ => {}
                }
            }
            Un::Sqrt => self.load(X9, a, k, false),
        }
        self.store(X9, dst, k);
    }

    pub(crate) fn conv(&mut self, conv: Conv, from: K, to: K, dst: Mem, a: V) {
        match conv {
            Conv::Int { signed } if self.opt => {
                let r = self.reg(a, from, signed, &[]);
                // Widened: extended as the destination's own bits.
                let ext = (to.size() > from.size()).then_some(signed);
                self.store_ext(r, dst, to, ext);
            }
            Conv::Int { signed } => {
                self.load(X9, a, from, signed);
                self.store(X9, dst, to);
            }
            Conv::ToBool => {
                self.load(X9, a, from, false);
                self.ins(enc::cmp_imm(true, R(X9), 0));
                self.cset(X9, Cond::NE);
                self.store(X9, dst, K::I8);
            }
            Conv::IToF { signed } => {
                let r = if self.opt {
                    self.reg(a, from, signed, &[])
                } else {
                    self.load(X9, a, from, signed);
                    X9
                };
                let vd = self.dst_v(dst).unwrap_or(V16);
                self.ins(enc::cvtf(signed, to != K::F32, Vr(vd), R(r)));
                self.store_fp(vd, dst, to);
            }
            Conv::FToI { signed, bits } => {
                self.load_fp(V16, a, from);
                self.ins(enc::fcvtz(signed, from == K::F64, R(X9), Vr(V16)));
                if bits < 64 {
                    let (max, min) = if signed {
                        ((1i64 << (bits - 1)) - 1, -(1i64 << (bits - 1)))
                    } else {
                        ((1i64 << bits) - 1, 0)
                    };
                    self.mov_imm(X10, max as u64);
                    self.cmp_rr(K::I64, X9, X10);
                    // csel x9, x10, x9, gt / hi
                    let c = if signed { Cond::GT } else { Cond::HI };
                    let (x9, x10) = (R(X9), R(X10));
                    self.ins(enc::csel(true, x9, x10, x9, c));
                    if signed {
                        self.mov_imm(X10, min as u64);
                        self.cmp_rr(K::I64, X9, X10);
                        self.ins(enc::csel(true, x9, x10, x9, Cond::LT));
                    }
                }
                self.store(X9, dst, to);
            }
            Conv::FToF => {
                let va = if self.opt {
                    self.freg(a, from, &[])
                } else {
                    self.load_fp(V16, a, from);
                    V16
                };
                let vd = self.dst_v(dst).unwrap_or(V16);
                let d = from != K::F32;
                self.ins(enc::fp(Fp::Fcvt, d, Vr(vd), Vr(va), Vr(0)));
                self.store_fp(vd, dst, to);
            }
            Conv::Bits => {
                self.load(X9, a, from, false);
                self.store(X9, dst, to);
            }
            // The half sits in the low 16 bits of `s16` / `w9`.
            Conv::Half if from == K::I16 => {
                self.load(X9, a, K::I16, false);
                self.ins(enc::fmov_gpr(false, false, V16, X9));
                self.ins(enc::fp(Fp::FcvtFromHalf, false, Vr(V16), Vr(V16), Vr(0)));
                self.store_fp(V16, dst, K::F32);
            }
            Conv::Half => {
                self.load_fp(V16, a, K::F32);
                self.ins(enc::fp(Fp::FcvtToHalf, false, Vr(V16), Vr(V16), Vr(0)));
                self.ins(enc::fmov_gpr(true, false, X9, V16));
                self.store(X9, dst, K::I16);
            }
        }
    }
}
