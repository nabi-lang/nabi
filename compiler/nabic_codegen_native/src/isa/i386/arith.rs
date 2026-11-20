//! Arithmetic, logic, unary operations and conversions.

#[allow(clippy::wildcard_imports)]
use crate::isa::i386::*;

impl Em<'_> {
    pub(crate) fn bin(&mut self, op: Bin, k: K, signed: bool, dst: Mem, a: V, b: V) {
        if k.is_float() && self.opt {
            // Into the destination's register unless `b` is in it.
            let bx = self.dst_x_v(b);
            let d = self
                .dst_x(dst)
                .filter(|d| bx != Some(*d) || a == b)
                .unwrap_or(XMM0);
            self.ldf(d, a, k);
            let src = self.frm(b, k, XMM1);
            self.a.sse(farith(op, k), Xmm(d), src);
            self.stf(d, dst, k);
            return;
        }
        if k.is_float() {
            self.ldf(XMM0, a, k);
            let src = match b {
                V::M(m) => Xrm::M(self.mem(m, ECX)),
                V::I(_) => {
                    self.ldf(XMM1, b, k);
                    xr(XMM1)
                }
            };
            self.a.sse(farith(op, k), Xmm(XMM0), src);
            self.stf(XMM0, dst, k);
            return;
        }
        if wide(k) {
            // Into the destination's pair, unless `b` is in it.
            let pair = self
                .ra
                .pair(dst)
                .filter(|_| a == V::M(dst) || b != V::M(dst));
            if let (Some(d), Bin::Add | Bin::Sub | Bin::And | Bin::Or | Bin::Xor) = (pair, op) {
                self.ld2(d.0, d.1, a);
                self.op64_to(d, alu_of(op), alu_hi(op), b);
                return;
            }
            self.bin64(op, signed, a, b);
            self.st2(EAX, EDX, dst);
            return;
        }
        if self.opt && self.bin_into(op, k, signed, dst, a, b) {
            return;
        }
        match op {
            Bin::Div | Bin::Rem => {
                self.ld(ECX, b, k, signed);
                self.ld(EAX, a, k, signed);
                self.div32(signed);
                if op == Bin::Rem {
                    self.mov(EAX, EDX);
                }
            }
            Bin::MulHi => {
                self.ld(ECX, b, k, signed);
                self.ld(EAX, a, k, signed);
                let op = if signed { enc::Un::Imul } else { enc::Un::Mul };
                self.a.un(op, gr(ECX));
                self.mov(EAX, EDX);
            }
            Bin::Shl | Bin::Shr | Bin::Rotl => {
                let sh = shift_of(op, signed);
                if let V::I(i) = b {
                    self.ld(EAX, a, k, signed);
                    self.a.shift_ri(sh, Sz::D, Gpr(EAX), (i & 31) as u8);
                } else {
                    self.ld(ECX, b, k, false);
                    self.ld(EAX, a, k, signed);
                    self.a.shift_cl(sh, Gpr(EAX));
                }
            }
            _ => {
                self.ld(EAX, a, k, signed);
                if let V::I(i) = b {
                    let imm = ext32(i, k, signed);
                    match op {
                        Bin::Mul => self.a.imul_ri(Gpr(EAX), gr(EAX), imm),
                        _ => self.alu_imm(alu_of(op), gr(EAX), imm),
                    }
                } else if self.opt {
                    // The low bits do not depend on the operands' upper ones.
                    self.alu_val(op, EAX, b, k, signed);
                } else {
                    self.ld(ECX, b, k, signed);
                    match op {
                        Bin::Mul => self.a.imul(Gpr(EAX), gr(ECX)),
                        _ => self.alu(alu_of(op), EAX, ECX),
                    }
                }
            }
        }
        self.st(EAX, dst, k);
    }

    /// `r = r op v` for add, subtract, logic and multiplication: `v` from
    /// its register or (4 bytes) behind a pointer, else loaded into `ecx`.
    pub(crate) fn alu_val(&mut self, op: Bin, r: u32, v: V, k: K, signed: bool) {
        let src = match v {
            V::I(i) => {
                let imm = ext32(i, k, signed);
                if op == Bin::Mul {
                    self.a.imul_ri(Gpr(r), gr(r), imm);
                } else {
                    self.alu_imm(alu_of(op), gr(r), imm);
                }
                return;
            }
            V::M(m) => match self.ra.get(m) {
                Some(a) if !a.fp => gr(a.reg),
                None if k.size() == 4 && matches!(m, Mem::Ind(..)) => Rm::M(self.mem(m, ECX)),
                _ => {
                    self.ld(ECX, v, k, signed);
                    gr(ECX)
                }
            },
        };
        match op {
            Bin::Mul => self.a.imul(Gpr(r), src),
            // The `r, r/m` form.
            _ => self.a.alu_ld(alu_of(op), Gpr(r), src),
        }
    }

    /// A 4-byte operation computed in the destination's register: false
    /// where it does not apply.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn bin_into(&mut self, op: Bin, k: K, signed: bool, dst: Mem, a: V, b: V) -> bool {
        let Some(d) = self.dst_r(dst) else {
            return false;
        };
        if k.size() != 4 {
            return false;
        }
        let breg = self.ra.get_v(b).filter(|x| !x.fp).map(|x| x.reg);
        match op {
            Bin::Add | Bin::Sub | Bin::And | Bin::Or | Bin::Xor | Bin::Mul => {
                if breg != Some(d) || a == b {
                    self.ld(d, a, k, signed);
                    self.alu_val(op, d, b, k, signed);
                } else if op != Bin::Sub {
                    // `d` holds `b`: the other way round.
                    self.alu_val(op, d, a, k, signed);
                } else {
                    return false;
                }
            }
            Bin::Shl | Bin::Shr | Bin::Rotl => {
                let V::I(i) = b else { return false };
                self.ld(d, a, k, signed);
                self.a
                    .shift_ri(shift_of(op, signed), Sz::D, Gpr(d), (i & 31) as u8);
            }
            _ => return false,
        }
        true
    }

    /// `eax` = `eax / ecx`, `edx` = the remainder.
    pub(crate) fn div32(&mut self, signed: bool) {
        if signed {
            self.a.cdq();
            self.a.un(enc::Un::Idiv, gr(ECX));
        } else {
            self.mov_imm(EDX, 0);
            self.a.un(enc::Un::Div, gr(ECX));
        }
    }

    /// `(lo, hi)` = `-(lo, hi)`.
    pub(crate) fn neg64(&mut self, lo: u32, hi: u32) {
        self.a.un(enc::Un::Neg, gr(lo));
        self.a.alu_ri8(Alu::Adc, gr(hi), 0);
        self.a.un(enc::Un::Neg, gr(hi));
    }

    /// `(eax, edx)` = `a op b`, 8 bytes; `b` in `(ecx, esi)` if needed.
    pub(crate) fn bin64(&mut self, op: Bin, signed: bool, a: V, b: V) {
        match op {
            Bin::Add | Bin::Sub | Bin::And | Bin::Or | Bin::Xor => {
                self.ld2(EAX, EDX, a);
                let (lo, hi) = (alu_of(op), alu_hi(op));
                if let V::I(i) = b {
                    self.alu_opt(lo, gr(EAX), i as u32);
                    self.alu_opt(hi, gr(EDX), (i >> 32) as u32);
                } else if self.opt {
                    self.op64(lo, hi, b);
                } else {
                    self.ld2(ECX, ESI, b);
                    self.alu(lo, EAX, ECX);
                    self.alu(hi, EDX, ESI);
                }
            }
            Bin::Mul => {
                self.ld2(EAX, EDX, a);
                self.ld2(ECX, ESI, b);
                // The cross products into the high word of the low one's.
                self.mov(EDI, EDX);
                self.a.imul(Gpr(EDI), gr(ECX));
                self.a.imul(Gpr(ESI), gr(EAX));
                self.alu(Alu::Add, EDI, ESI);
                self.a.un(enc::Un::Mul, gr(ECX));
                self.alu(Alu::Add, EDX, EDI);
            }
            Bin::Shl | Bin::Shr => {
                let amount = match b {
                    V::I(i) => V::I(i & 63),
                    v @ V::M(_) => v,
                };
                self.ld(ECX, amount, K::I32, false);
                self.ld2(EAX, EDX, a);
                self.shift64(op == Bin::Shl, signed);
            }
            Bin::Div | Bin::Rem => {
                self.ld2(EAX, EDX, a);
                self.ld2(ECX, ESI, b);
                self.divmod64(op == Bin::Rem, signed);
            }
            Bin::Rotl => unreachable!("an 8-byte rotate is two shifts here"),
            Bin::MulHi => unreachable!("no 8-byte high product here"),
        }
    }

    /// `(eax, edx)` op= the 8-byte `v` (`lo` / `hi` the operations on the
    /// low and high words, by the `r, r/m` forms), `v` an immediate, in
    /// registers or in memory (a frame slot's words through `ecx`).
    pub(crate) fn op64(&mut self, lo: Alu, hi: Alu, v: V) {
        self.op64_to((EAX, EDX), lo, hi, v);
    }

    /// `op64` into the pair `d`.
    pub(crate) fn op64_to(&mut self, d: (u32, u32), lo: Alu, hi: Alu, v: V) {
        if let Some((l, h)) = self.ra.pair_v(v) {
            self.a.alu_ld(lo, Gpr(d.0), gr(l));
            self.a.alu_ld(hi, Gpr(d.1), gr(h));
            return;
        }
        match v {
            V::I(i) => {
                self.alu_opt(lo, gr(d.0), i as u32);
                self.alu_opt(hi, gr(d.1), (i >> 32) as u32);
            }
            V::M(m @ Mem::Frame(..)) => {
                let at = self.mem(m, ECX);
                self.a.mov(Sz::D, Gpr(ECX), Rm::M(at));
                self.a.alu_ld(lo, Gpr(d.0), gr(ECX));
                self.a.mov(Sz::D, Gpr(ECX), Rm::M(at.plus(4)));
                self.a.alu_ld(hi, Gpr(d.1), gr(ECX));
            }
            V::M(m) => {
                let at = self.mem(m, ECX);
                self.a.alu_ld(lo, Gpr(d.0), Rm::M(at));
                self.a.alu_ld(hi, Gpr(d.1), Rm::M(at.plus(4)));
            }
        }
    }

    /// `(eax, edx)` shifted by `cl` (below 64).
    pub(crate) fn shift64(&mut self, left: bool, signed: bool) {
        if left {
            self.a.shxd_cl(false, Gpr(EDX), Gpr(EAX));
            self.a.shift_cl(Shift::Shl, Gpr(EAX));
        } else {
            self.a.shxd_cl(true, Gpr(EAX), Gpr(EDX));
            let sh = if signed { Shift::Sar } else { Shift::Shr };
            self.a.shift_cl(sh, Gpr(EDX));
        }
        self.a.test_ri(Sz::B, gr(ECX), 32);
        let small = self.jcc_fwd(Cond::E);
        if left {
            self.mov(EDX, EAX);
            self.mov_imm(EAX, 0);
        } else {
            self.mov(EAX, EDX);
            if signed {
                self.a.shift_ri(Shift::Sar, Sz::D, Gpr(EDX), 31);
            } else {
                self.mov_imm(EDX, 0);
            }
        }
        self.here(small);
    }

    /// `test r, r`.
    pub(crate) fn test(&mut self, r: u32) {
        self.a.test(Sz::D, gr(r), Gpr(r));
    }

    /// `(eax, edx)` / `(ecx, esi)` (or the remainder), 8 bytes, the
    /// divisor non-zero.
    pub(crate) fn divmod64(&mut self, rem: bool, signed: bool) {
        if signed {
            // The signs aside, the magnitudes divided.
            self.a.mov_st(Sz::D, Rm::M(Self::sp(0)), Gpr(EDX));
            self.a.mov_st(Sz::D, Rm::M(Self::sp(4)), Gpr(ESI));
            self.test(EDX);
            let a = self.jcc_fwd(Cond::NS);
            self.neg64(EAX, EDX);
            self.here(a);
            self.test(ESI);
            let b = self.jcc_fwd(Cond::NS);
            self.neg64(ECX, ESI);
            self.here(b);
        }
        self.helper(UDIVMOD);
        if rem {
            self.mov(EAX, ECX);
            self.mov(EDX, ESI);
        }
        if signed {
            // A quotient negative when the signs differ, a remainder
            // when the dividend is.
            self.a.mov(Sz::D, Gpr(EDI), Rm::M(Self::sp(0)));
            if !rem {
                self.a.alu_ld(Alu::Xor, Gpr(EDI), Rm::M(Self::sp(4)));
            }
            self.test(EDI);
            let pos = self.jcc_fwd(Cond::NS);
            self.neg64(EAX, EDX);
            self.here(pos);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn ovf(&mut self, op: Bin, k: K, signed: bool, dst: Mem, a: V, b: V, fail: Label) {
        if wide(k) && self.opt && matches!(op, Bin::Add | Bin::Sub) {
            self.ld2(EAX, EDX, a);
            self.op64(alu_of(op), alu_hi(op), b);
            self.jcc(if signed { Cond::O } else { Cond::B }, fail);
            self.st2(EAX, EDX, dst);
            return;
        }
        if wide(k) {
            self.ld2(EAX, EDX, a);
            self.ld2(ECX, ESI, b);
            match op {
                Bin::Add | Bin::Sub => {
                    self.alu(alu_of(op), EAX, ECX);
                    self.alu(alu_hi(op), EDX, ESI);
                    self.jcc(if signed { Cond::O } else { Cond::B }, fail);
                }
                _ if !signed => {
                    self.helper(UMULO);
                    self.test(EDI);
                    self.jcc(Cond::NE, fail);
                }
                _ => self.mulo64_signed(fail),
            }
            self.st2(EAX, EDX, dst);
            return;
        }
        self.ld(ECX, b, k, signed);
        self.ld(EAX, a, k, signed);
        if k == K::I32 {
            match op {
                Bin::Add => self.alu(Alu::Add, EAX, ECX),
                Bin::Sub => self.alu(Alu::Sub, EAX, ECX),
                _ if signed => self.a.imul(Gpr(EAX), gr(ECX)),
                // edx:eax; OF / CF when the high half is used.
                _ => self.a.un(enc::Un::Mul, gr(ECX)),
            }
            let cc = match (op, signed) {
                (_, true) | (Bin::Mul, false) => Cond::O,
                _ => Cond::B,
            };
            self.jcc(cc, fail);
        } else {
            // 8 / 16 bits in 32: the result must equal its own extension.
            match op {
                Bin::Add => self.alu(Alu::Add, EAX, ECX),
                Bin::Sub => self.alu(Alu::Sub, EAX, ECX),
                _ => self.a.imul(Gpr(EAX), gr(ECX)),
            }
            self.extend(ECX, gr(EAX), k, signed);
            self.alu(Alu::Cmp, EAX, ECX);
            self.jcc(Cond::NE, fail);
        }
        self.st(EAX, dst, k);
    }

    /// `(eax, edx)` *= `(ecx, esi)` signed, to `fail` on overflow.
    pub(crate) fn mulo64_signed(&mut self, fail: Label) {
        // The result's sign aside, the magnitudes multiplied.
        self.a.mov_st(Sz::D, Rm::M(Self::sp(0)), Gpr(EDX));
        self.a.alu(Alu::Xor, Sz::D, Rm::M(Self::sp(0)), Gpr(ESI));
        self.test(EDX);
        let a = self.jcc_fwd(Cond::NS);
        self.neg64(EAX, EDX);
        self.here(a);
        self.test(ESI);
        let b = self.jcc_fwd(Cond::NS);
        self.neg64(ECX, ESI);
        self.here(b);
        self.helper(UMULO);
        self.test(EDI);
        self.jcc(Cond::NE, fail);
        self.a.mov(Sz::D, Gpr(ECX), Rm::M(Self::sp(0)));
        self.test(ECX);
        let neg = self.jcc_fwd(Cond::S);
        // Positive: below 2^63.
        self.test(EDX);
        self.jcc(Cond::S, fail);
        let done = self.jmp_fwd();
        self.here(neg);
        // Negative: at most 2^63, so the negation is negative or zero.
        self.neg64(EAX, EDX);
        let ok = self.jcc_fwd(Cond::S);
        self.mov(ECX, EAX);
        self.alu(Alu::Or, ECX, EDX);
        self.jcc(Cond::NE, fail);
        self.here(ok);
        self.here(done);
    }

    pub(crate) fn un(&mut self, op: Un, k: K, dst: Mem, a: V) {
        if self.opt && k.is_float() && matches!(op, Un::Sqrt | Un::Neg) {
            let d = self.dst_x(dst).unwrap_or(XMM0);
            if op == Un::Sqrt {
                let src = self.frm(a, k, d);
                self.a.sse(fsqrt(k), Xmm(d), src);
            } else {
                // The sign bit alone: all ones shifted up.
                let m = if d == XMM1 { XMM2 } else { XMM1 };
                self.a.sse(Sse::Pcmpeqd, Xmm(m), xr(m));
                if k == K::F64 {
                    self.a.pshift(enc::PShift::Psllq, Xmm(m), 63);
                } else {
                    self.a.pshift(enc::PShift::Pslld, Xmm(m), 31);
                }
                self.ldf(d, a, k);
                self.a.sse(Sse::Xorps, Xmm(d), xr(m));
            }
            self.stf(d, dst, k);
            return;
        }
        if op == Un::Sqrt {
            self.ldf(XMM0, a, k);
            self.a.sse(fsqrt(k), Xmm(XMM0), xr(XMM0));
            self.stf(XMM0, dst, k);
            return;
        }
        if wide(k) {
            self.ld2(EAX, EDX, a);
            match op {
                Un::Neg if k.is_float() => self.a.alu_ri(Alu::Xor, gr(EDX), 0x8000_0000),
                Un::Neg => self.neg64(EAX, EDX),
                Un::Not => {
                    self.a.un(enc::Un::Not, gr(EAX));
                    self.a.un(enc::Un::Not, gr(EDX));
                }
                Un::Popcnt => {
                    self.mov(ESI, EDX);
                    self.popcnt32();
                    self.mov(EDI, EAX);
                    self.mov(EAX, ESI);
                    self.popcnt32();
                    self.alu(Alu::Add, EAX, EDI);
                    self.mov_imm(EDX, 0);
                }
                Un::Clz => {
                    self.test(EDX);
                    let lo = self.jcc_fwd(Cond::E);
                    self.a.bsf(true, Gpr(ECX), gr(EDX));
                    self.mov_imm(EAX, 31);
                    self.alu(Alu::Sub, EAX, ECX);
                    let d1 = self.jmp_fwd();
                    self.here(lo);
                    self.test(EAX);
                    let zero = self.jcc_fwd(Cond::E);
                    self.a.bsf(true, Gpr(ECX), gr(EAX));
                    self.mov_imm(EAX, 63);
                    self.alu(Alu::Sub, EAX, ECX);
                    let d2 = self.jmp_fwd();
                    self.here(zero);
                    self.mov_imm(EAX, 64);
                    self.here(d1);
                    self.here(d2);
                    self.mov_imm(EDX, 0);
                }
                Un::Ctz => {
                    self.test(EAX);
                    let hi = self.jcc_fwd(Cond::E);
                    self.a.bsf(false, Gpr(EAX), gr(EAX));
                    let d1 = self.jmp_fwd();
                    self.here(hi);
                    self.test(EDX);
                    let zero = self.jcc_fwd(Cond::E);
                    self.a.bsf(false, Gpr(EAX), gr(EDX));
                    self.alu_imm(Alu::Add, gr(EAX), 32);
                    let d2 = self.jmp_fwd();
                    self.here(zero);
                    self.mov_imm(EAX, 64);
                    self.here(d1);
                    self.here(d2);
                    self.mov_imm(EDX, 0);
                }
                Un::Bswap => {
                    self.a.bswap(Gpr(EAX));
                    self.a.bswap(Gpr(EDX));
                    self.a.xchg_eax(Gpr(EDX));
                }
                Un::Sqrt => unreachable!("sqrt is a float operation"),
            }
            self.st2(EAX, EDX, dst);
            return;
        }
        let bits = k.bits();
        self.ld(EAX, a, k, op == Un::Neg && !k.is_float());
        match op {
            Un::Neg if k.is_float() => self.a.alu_ri(Alu::Xor, gr(EAX), 0x8000_0000),
            Un::Neg => self.a.un(enc::Un::Neg, gr(EAX)),
            Un::Not => self.a.un(enc::Un::Not, gr(EAX)),
            Un::Popcnt => self.popcnt32(),
            Un::Clz | Un::Ctz => {
                self.test(EAX);
                let zero = self.jcc_fwd(Cond::E);
                if op == Un::Clz {
                    self.a.bsf(true, Gpr(ECX), gr(EAX));
                    self.mov_imm(EAX, bits - 1);
                    self.alu(Alu::Sub, EAX, ECX);
                } else {
                    self.a.bsf(false, Gpr(EAX), gr(EAX));
                }
                let done = self.jmp_fwd();
                self.here(zero);
                self.mov_imm(EAX, bits);
                self.here(done);
            }
            Un::Bswap => match k {
                K::I16 => self.a.shift_ri(Shift::Rol, Sz::W, Gpr(EAX), 8),
                K::I32 => self.a.bswap(Gpr(EAX)),
                _ => {}
            },
            Un::Sqrt => unreachable!("sqrt is a float operation"),
        }
        self.st(EAX, dst, k);
    }

    /// `eax` = its bit count (`ecx` scratch).
    pub(crate) fn popcnt32(&mut self) {
        let mask = |s: &mut Self, sh: u8, m: u32| {
            s.mov(ECX, EAX);
            s.a.shift_ri(Shift::Shr, Sz::D, Gpr(ECX), sh);
            s.a.alu_ri(Alu::And, gr(ECX), m);
        };
        // x -= (x >> 1) & 0x55..
        mask(self, 1, 0x5555_5555);
        self.alu(Alu::Sub, EAX, ECX);
        // x = (x & 0x33..) + ((x >> 2) & 0x33..)
        mask(self, 2, 0x3333_3333);
        self.a.alu_ri(Alu::And, gr(EAX), 0x3333_3333);
        self.alu(Alu::Add, EAX, ECX);
        // x = (x + (x >> 4)) & 0x0f..
        self.mov(ECX, EAX);
        self.a.shift_ri(Shift::Shr, Sz::D, Gpr(ECX), 4);
        self.alu(Alu::Add, EAX, ECX);
        self.a.alu_ri(Alu::And, gr(EAX), 0x0f0f_0f0f);
        // x = (x * 0x0101..) >> 24
        self.a.imul_ri(Gpr(EAX), gr(EAX), 0x0101_0101);
        self.a.shift_ri(Shift::Shr, Sz::D, Gpr(EAX), 24);
    }

    pub(crate) fn conv(&mut self, conv: Conv, from: K, to: K, dst: Mem, a: V) {
        match conv {
            // Lowered to the runtime call here (`FnLower::half_conv`).
            Conv::Half => unreachable!("no half conversion instruction"),
            Conv::Int { signed } => {
                if wide(to) {
                    if wide(from) {
                        self.ld2(EAX, EDX, a);
                    } else {
                        self.ld(EAX, a, from, signed);
                        if signed {
                            self.a.cdq();
                        } else {
                            self.mov_imm(EDX, 0);
                        }
                    }
                    self.st2(EAX, EDX, dst);
                } else {
                    self.ld(EAX, a, from, signed);
                    self.st(EAX, dst, to);
                }
            }
            Conv::ToBool => {
                if wide(from) {
                    self.ld2(EAX, EDX, a);
                    self.alu(Alu::Or, EAX, EDX);
                } else {
                    self.ld(EAX, a, from, false);
                    self.test(EAX);
                }
                self.setcc(Cond::NE);
                self.st(EAX, dst, K::I8);
            }
            Conv::IToF { signed } => {
                if !wide(from) && (signed || from.size() < 4) {
                    self.ld(EAX, a, from, signed);
                    let d = self.dst_x(dst).unwrap_or(XMM0);
                    self.a.cvtsi2f(to != K::F32, Xmm(d), Gpr(EAX));
                    self.stf(d, dst, to);
                    return;
                }
                // Through the x87 unit: a 64-bit integer at `esp`.
                if wide(from) {
                    self.ld2(EAX, EDX, a);
                } else {
                    self.ld(EAX, a, from, false);
                    self.mov_imm(EDX, 0);
                }
                self.a.mov_st(Sz::D, Rm::M(Self::sp(0)), Gpr(EAX));
                self.a.mov_st(Sz::D, Rm::M(Self::sp(4)), Gpr(EDX));
                self.a.x87(X87::Fild64, Self::sp(0));
                if wide(from) && !signed {
                    // Read as negative: 2^64 more.
                    self.test(EDX);
                    let small = self.jcc_fwd(Cond::NS);
                    self.a.mov_mi(Rm::M(Self::sp(8)), 0x5f80_0000);
                    self.a.x87(X87::Fadd32, Self::sp(8));
                    self.here(small);
                }
                if let Some(x) = self.dst_x(dst) {
                    self.a.x87(fstp(to), Self::sp(0));
                    self.a.sse(movs(to), Xmm(x), Xrm::M(Self::sp(0)));
                    return;
                }
                let at = self.mem(dst, if self.opt { ECX } else { EDI });
                self.a.x87(fstp(to), at);
            }
            Conv::FToI { signed, bits } => self.f_to_i(from, to, dst, a, signed, bits),
            Conv::FToF if self.opt => {
                let d = self.dst_x(dst).unwrap_or(XMM0);
                let src = self.frm(a, from, d);
                self.a.sse(fcvt(from), Xmm(d), src);
                self.stf(d, dst, to);
            }
            Conv::FToF => {
                self.ldf(XMM0, a, from);
                self.a.sse(fcvt(from), Xmm(XMM0), xr(XMM0));
                self.stf(XMM0, dst, to);
            }
            Conv::Bits => {
                if wide(from) {
                    self.ld2(EAX, EDX, a);
                    self.st2(EAX, EDX, dst);
                } else {
                    self.ld(EAX, a, from, false);
                    self.st(EAX, dst, to);
                }
            }
        }
    }

    /// `xmm x` = the `k` float `2^63`.
    pub(crate) fn two63(&mut self, x: u32, k: K) {
        let bits = if k == K::F64 {
            63f64.exp2().to_bits()
        } else {
            u64::from(63f32.exp2().to_bits())
        };
        self.ldf(x, V::I(bits as i64), k);
    }

    /// `(eax, edx)` = `xmm0` truncated by the x87 unit (its control word
    /// set to round toward zero meanwhile): `i64::MIN` out of range.
    pub(crate) fn trunc64(&mut self, k: K) {
        self.a.sse_st(movs_st(k), Self::sp(0), Xmm(XMM0));
        self.a.x87(fld(k), Self::sp(0));
        self.a.x87(X87::Fnstcw, Self::sp(8));
        self.a.mov(Sz::W, Gpr(ECX), Rm::M(Self::sp(8)));
        self.a.alu_ri(Alu::Or, gr(ECX), 0x0c00);
        self.a.mov_st(Sz::W, Rm::M(Self::sp(10)), Gpr(ECX));
        self.a.x87(X87::Fldcw, Self::sp(10));
        self.a.x87(X87::Fistp64, Self::sp(0));
        self.a.x87(X87::Fldcw, Self::sp(8));
        self.a.mov(Sz::D, Gpr(EAX), Rm::M(Self::sp(0)));
        self.a.mov(Sz::D, Gpr(EDX), Rm::M(Self::sp(4)));
    }

    /// Float -> integer, saturating, NaN to 0 (`bits` of the destination are
    /// the integer's): an 8-byte result first, then clamped.
    pub(crate) fn f_to_i(&mut self, from: K, to: K, dst: Mem, a: V, signed: bool, bits: u32) {
        let ucomi = |s: &mut Self, x: u32, y: u32| {
            s.a.sse(ucomis(from), Xmm(x), xr(y));
        };
        self.ldf(XMM0, a, from);
        self.two63(XMM1, from);
        let mut ends = Vec::new();
        // NaN (and, unsigned, negative): 0.
        ucomi(self, XMM0, XMM0);
        let mut zeros = vec![self.jcc_fwd(Cond::P)];
        if signed {
            ucomi(self, XMM0, XMM1);
            let big = self.jcc_fwd(Cond::AE);
            self.trunc64(from); // too negative: i64::MIN
            ends.push(self.jmp_fwd());
            self.here(big);
            self.mov_imm(EAX, u32::MAX);
            self.mov_imm(EDX, 0x7fff_ffff);
        } else {
            // >= 2^64: max; >= 2^63: through the sign bit.
            self.a.sse(Sse::Xorps, Xmm(XMM2), xr(XMM2));
            ucomi(self, XMM0, XMM2);
            zeros.push(self.jcc_fwd(Cond::B));
            ucomi(self, XMM0, XMM1);
            let high = self.jcc_fwd(Cond::AE);
            self.trunc64(from);
            ends.push(self.jmp_fwd());
            self.here(high);
            // Less 2^63.
            self.a.sse(fsub(from), Xmm(XMM0), xr(XMM1));
            ucomi(self, XMM0, XMM1);
            let huge = self.jcc_fwd(Cond::AE);
            self.trunc64(from);
            self.a.alu_ri(Alu::Xor, gr(EDX), 0x8000_0000);
            ends.push(self.jmp_fwd());
            self.here(huge);
            self.mov_imm(EAX, u32::MAX);
            self.mov_imm(EDX, u32::MAX);
        }
        ends.push(self.jmp_fwd());
        for z in zeros {
            self.here(z);
        }
        self.mov_imm(EAX, 0);
        self.mov_imm(EDX, 0);
        for e in ends {
            self.here(e);
        }
        if bits < 64 {
            let (max, min) = if signed {
                ((1i64 << (bits - 1)) - 1, -(1i64 << (bits - 1)))
            } else {
                ((1i64 << bits) - 1, 0)
            };
            if self.opt {
                self.clamp(max, min, signed);
            } else {
                self.clamp_o0(max, min, signed);
            }
        }
        if wide(to) {
            self.st2(EAX, EDX, dst);
        } else {
            self.st(EAX, dst, to);
        }
    }

    /// `(eax, edx)` clamped to `[min, max]`, `ecx` scratch: the flags of
    /// the 8-byte subtractions only.
    pub(crate) fn clamp(&mut self, max: i64, min: i64, signed: bool) {
        // max - x < 0: max.
        self.mov_imm(ECX, max as u32);
        self.alu(Alu::Sub, ECX, EAX);
        // The flags kept: never `xor`.
        self.a.mov_ri(Gpr(ECX), (max >> 32) as u32);
        self.alu(Alu::Sbb, ECX, EDX);
        let ok = self.jcc_fwd(if signed { Cond::GE } else { Cond::AE });
        self.mov_imm(EAX, max as u32);
        self.mov_imm(EDX, (max >> 32) as u32);
        self.here(ok);
        if signed {
            // x - min < 0: min.
            self.a.alu_ri(Alu::Cmp, gr(EAX), min as u32);
            self.mov(ECX, EDX);
            self.a.alu_ri(Alu::Sbb, gr(ECX), (min >> 32) as u32);
            let ok = self.jcc_fwd(Cond::GE);
            self.mov_imm(EAX, min as u32);
            self.mov_imm(EDX, (min >> 32) as u32);
            self.here(ok);
        }
    }

    pub(crate) fn clamp_o0(&mut self, max: i64, min: i64, signed: bool) {
        // max - x < 0: max.
        self.mov_imm(ECX, max as u32);
        self.mov_imm(ESI, (max >> 32) as u32);
        self.alu(Alu::Sub, ECX, EAX);
        self.alu(Alu::Sbb, ESI, EDX);
        let ok = self.jcc_fwd(if signed { Cond::GE } else { Cond::AE });
        self.mov_imm(EAX, max as u32);
        self.mov_imm(EDX, (max >> 32) as u32);
        self.here(ok);
        if signed {
            // x - min < 0: min.
            self.mov(ECX, EAX);
            self.mov(ESI, EDX);
            self.a.alu_ri(Alu::Sub, gr(ECX), min as u32);
            self.a.alu_ri(Alu::Sbb, gr(ESI), (min >> 32) as u32);
            let ok = self.jcc_fwd(Cond::GE);
            self.mov_imm(EAX, min as u32);
            self.mov_imm(EDX, (min >> 32) as u32);
            self.here(ok);
        }
    }
}
