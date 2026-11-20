//! Arithmetic, logic, unary operations and conversions.

#[allow(clippy::wildcard_imports)]
use crate::isa::riscv::*;

impl Em<'_> {
    pub(crate) fn bin(&mut self, op: Bin, k: K, signed: bool, dst: Mem, a: V, b: V) {
        if k.is_float() {
            if self.rv32 {
                let name = match (op, k) {
                    (Bin::Add, K::F32) => "__addsf3",
                    (Bin::Sub, K::F32) => "__subsf3",
                    (Bin::Mul, K::F32) => "__mulsf3",
                    (_, K::F32) => "__divsf3",
                    (Bin::Add, _) => "__adddf3",
                    (Bin::Sub, _) => "__subdf3",
                    (Bin::Mul, _) => "__muldf3",
                    _ => "__divdf3",
                };
                self.soft2(name, k, a, b);
                self.st_soft(k, dst);
                return;
            }
            let x = self.freg(a, k, F0);
            let y = self.freg(b, k, F1);
            let o = match op {
                Bin::Add => FArith::Add,
                Bin::Sub => FArith::Sub,
                Bin::Mul => FArith::Mul,
                _ => FArith::Div,
            };
            let fd = self.dst_f(dst).unwrap_or(F0);
            let (d, x, y) = (enc::F(fd), enc::F(x), enc::F(y));
            self.ins(enc::farith(o, k == K::F64, d, x, y, Rm::Dyn));
            self.stf(fd, dst, k);
            return;
        }
        if self.pair(k) {
            if matches!(op, Bin::Add | Bin::Sub | Bin::And | Bin::Or | Bin::Xor) {
                let x = self.src2(T0, T1, a);
                let y = self.src2(T2, T3, b);
                let d = self.dst2(dst);
                self.add64(op, d, x, y);
                self.put2(d, dst);
                return;
            }
            self.bin64(op, signed, a, b);
            self.st2(T0, T1, dst);
            return;
        }
        let xlen = self.xlen();
        let x = self.reg(a, k, signed, T0);
        let rd = self.dst_x(dst).unwrap_or(T0);
        match op {
            Bin::Shl | Bin::Shr => {
                let rop = match (op, signed) {
                    (Bin::Shl, _) => SLL,
                    (_, true) => SRA,
                    _ => SRL,
                };
                if let V::I(i) = b {
                    let sh = (i as u32) & (xlen - 1);
                    if sh == 0 {
                        self.mv(rd, x);
                    } else {
                        match rop {
                            SLL => self.slli(rd, x, sh),
                            SRA => self.srai(rd, x, sh),
                            _ => self.srli(rd, x, sh),
                        }
                    }
                } else {
                    let y = self.reg(b, k, false, T2);
                    self.alu(rop, rd, x, y);
                }
            }
            // `Zbb` (`FnLower::rotates`): left by `i` is right by `w - i`.
            Bin::Rotl => {
                let w32 = !self.rv32 && k == K::I32;
                let bits = if w32 { 32 } else { xlen };
                if let V::I(i) = b {
                    let sh = (bits - (i as u32 & (bits - 1))) & (bits - 1);
                    self.ins(enc::rori(w32, X(rd), X(x), sh));
                } else {
                    let y = self.reg(b, k, false, T2);
                    self.ins(enc::rot(true, w32, X(rd), X(x), X(y)));
                }
            }
            Bin::Add | Bin::Sub | Bin::And | Bin::Or | Bin::Xor
                if matches!(b, V::I(i) if {
                    let v = ext(i, k, signed);
                    fits12(if op == Bin::Sub { v.wrapping_neg() } else { v })
                }) =>
            {
                let V::I(i) = b else { unreachable!() };
                let v = ext(i, k, signed);
                match op {
                    Bin::Add => self.addi(rd, x, v as i32),
                    Bin::Sub => self.addi(rd, x, v.wrapping_neg() as i32),
                    Bin::And => self.andi(rd, x, v as i32),
                    Bin::Or => self.ins(enc::ori(X(rd), X(x), v as i32)),
                    _ => self.xori(rd, x, v as i32),
                }
            }
            _ => {
                let y = self.reg(b, k, signed, T2);
                let rop = match (op, signed) {
                    (Bin::Add, _) => ADD,
                    (Bin::Sub, _) => SUB,
                    (Bin::Mul, _) => MUL,
                    (Bin::MulHi, true) => MULH,
                    (Bin::MulHi, false) => MULHU,
                    (Bin::Div, true) => DIV,
                    (Bin::Div, false) => DIVU,
                    (Bin::Rem, true) => REM,
                    (Bin::Rem, false) => REMU,
                    (Bin::And, _) => AND,
                    (Bin::Or, _) => OR,
                    _ => XOR,
                };
                self.alu(rop, rd, x, y);
            }
        }
        // Zero-extended operands keep these zero-extended.
        let zx = !signed && matches!(op, Bin::And | Bin::Or | Bin::Xor | Bin::Shr | Bin::Rem);
        self.st_ext(rd, dst, k, zx);
    }

    /// `(t0, t1)` = `a op b`, 8 bytes on rv32; `b` in `(t2, t3)`.
    pub(crate) fn bin64(&mut self, op: Bin, signed: bool, a: V, b: V) {
        match op {
            Bin::Div | Bin::Rem => {
                self.ld2(A0, A1, a);
                self.ld2(A2, A3, b);
                self.helper(match (op, signed) {
                    (Bin::Div, true) => "__divdi3",
                    (Bin::Div, false) => "__udivdi3",
                    (_, true) => "__moddi3",
                    _ => "__umoddi3",
                });
                self.mv(T0, A0);
                self.mv(T1, A1);
                return;
            }
            Bin::Shl | Bin::Shr => {
                self.ld2(T0, T1, a);
                self.ld(T2, b, K::I32, false);
                self.shift64(op == Bin::Shl, signed);
                return;
            }
            Bin::Rotl => unreachable!("an 8-byte rotate is two shifts on rv32"),
            Bin::MulHi => unreachable!("no 8-byte high product on rv32"),
            _ => {}
        }
        self.ld2(T0, T1, a);
        self.ld2(T2, T3, b);
        if op == Bin::Mul {
            self.mul64(T0, T1, T2, T3);
        } else {
            self.add64(op, (T0, T1), (T0, T1), (T2, T3));
        }
    }

    /// `d` = `x op y` for 8-byte `+ - & | ^` (rv32; `a0` `a1` scratch).
    pub(crate) fn add64(&mut self, op: Bin, d: (u32, u32), x: (u32, u32), y: (u32, u32)) {
        match op {
            Bin::Add => {
                // The carry: the low sum below an operand it did not
                // overwrite.
                let lo = if d.0 == x.0 && d.0 == y.0 { A1 } else { d.0 };
                let other = if lo == y.0 { x.0 } else { y.0 };
                self.alu(ADD, lo, x.0, y.0);
                self.alu(SLTU, A0, lo, other);
                self.alu(ADD, d.1, x.1, y.1);
                self.alu(ADD, d.1, d.1, A0);
                self.mv(d.0, lo);
            }
            Bin::Sub => {
                self.alu(SLTU, A0, x.0, y.0);
                self.alu(SUB, d.0, x.0, y.0);
                self.alu(SUB, d.1, x.1, y.1);
                self.alu(SUB, d.1, d.1, A0);
            }
            _ => {
                let rop = match op {
                    Bin::And => AND,
                    Bin::Or => OR,
                    _ => XOR,
                };
                self.alu(rop, d.0, x.0, y.0);
                self.alu(rop, d.1, x.1, y.1);
            }
        }
    }

    /// `(al, ah)` *= `(bl, bh)`, the low 8 bytes (rv32; `a0`-`a2`
    /// scratch).
    pub(crate) fn mul64(&mut self, al: u32, ah: u32, bl: u32, bh: u32) {
        self.alu(MULHU, A0, al, bl);
        self.alu(MUL, A1, al, bh);
        self.alu(MUL, A2, ah, bl);
        self.alu(MUL, al, al, bl);
        self.alu(ADD, A0, A0, A1);
        self.alu(ADD, ah, A0, A2);
    }

    /// `(t0, t1)` shifted by `t2` (below 64), rv32.
    pub(crate) fn shift64(&mut self, left: bool, signed: bool) {
        // a0 = n - 32: from 32 on, one word moves into the other.
        self.addi(A0, T2, -32);
        let big = self.b_fwd(BGE, A0, ZERO);
        // `(x >> 1) >> (31 - n)` is `x >> (32 - n)`, 0 for n = 0.
        self.xori(A2, T2, 31);
        if left {
            self.alu(SLL, T1, T1, T2);
            self.srli(A1, T0, 1);
            self.alu(SRL, A1, A1, A2);
            self.alu(OR, T1, T1, A1);
            self.alu(SLL, T0, T0, T2);
        } else {
            self.alu(SRL, T0, T0, T2);
            self.slli(A1, T1, 1);
            self.alu(SLL, A1, A1, A2);
            self.alu(OR, T0, T0, A1);
            self.alu(if signed { SRA } else { SRL }, T1, T1, T2);
        }
        let done = self.b_fwd(BEQ, ZERO, ZERO);
        self.here(big);
        if left {
            self.alu(SLL, T1, T0, A0);
            self.mv(T0, ZERO);
        } else if signed {
            self.alu(SRA, T0, T1, A0);
            self.srai(T1, T1, 31);
        } else {
            self.alu(SRL, T0, T1, A0);
            self.mv(T1, ZERO);
        }
        self.here(done);
    }

    /// `rd` = the low `bits` of `rs` extended by `signed` to XLEN.
    pub(crate) fn ext_reg(&mut self, rd: u32, rs: u32, bits: u32, signed: bool) {
        let sh = self.xlen() - bits;
        if sh == 0 {
            self.mv(rd, rs);
            return;
        }
        self.slli(rd, rs, sh);
        if signed {
            self.srai(rd, rd, sh);
        } else {
            self.srli(rd, rd, sh);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn ovf(&mut self, op: Bin, k: K, signed: bool, dst: Mem, a: V, b: V, fail: Label) {
        if self.pair(k) {
            self.ovf64(op, signed, a, b, fail);
            self.st2(A0, A3, dst);
            return;
        }
        let xlen = self.xlen();
        let x = self.reg(a, k, signed, T0);
        let y = self.reg(b, k, signed, T2);
        let rop = match op {
            Bin::Add => ADD,
            Bin::Sub => SUB,
            _ => MUL,
        };
        self.alu(rop, T1, x, y);
        if k.bits() < xlen {
            // Exact in XLEN: the result must equal its own extension.
            self.ext_reg(A0, T1, k.bits(), signed);
            self.br(BNE, A0, T1, fail);
        } else {
            match (op, signed) {
                // The sum is below `a` exactly when `b` is negative.
                (Bin::Add, true) => {
                    self.alu(SLT, A0, T1, x);
                    self.alu(SLT, A1, y, ZERO);
                    self.br(BNE, A0, A1, fail);
                }
                (Bin::Add, false) => self.br(BLTU, T1, x, fail),
                // The difference is below `a` exactly when `b` is
                // positive.
                (Bin::Sub, true) => {
                    self.alu(SLT, A0, T1, x);
                    self.alu(SLT, A1, ZERO, y);
                    self.br(BNE, A0, A1, fail);
                }
                (Bin::Sub, false) => self.br(BLTU, x, y, fail),
                // The high half: the low one's sign (zero).
                (_, true) => {
                    self.alu(MULH, A0, x, y);
                    self.srai(A1, T1, xlen - 1);
                    self.br(BNE, A0, A1, fail);
                }
                _ => {
                    self.alu(MULHU, A0, x, y);
                    self.br(BNE, A0, ZERO, fail);
                }
            }
        }
        // A narrow unsigned result passed as its own zero extension.
        self.st_ext(T1, dst, k, !signed);
    }

    /// rv32 8-byte `+ - *` checked: the result in `(a0, a3)`.
    pub(crate) fn ovf64(&mut self, op: Bin, signed: bool, a: V, b: V, fail: Label) {
        if op != Bin::Add && op != Bin::Sub {
            self.ld2(T0, T1, a);
            self.ld2(T2, T3, b);
        }
        match op {
            Bin::Add => {
                let ((xl, xh), (yl, yh)) = (self.src2(T0, T1, a), self.src2(T2, T3, b));
                self.alu(ADD, A0, xl, yl);
                self.alu(SLTU, A1, A0, xl);
                self.alu(ADD, A2, xh, yh);
                self.alu(ADD, A3, A2, A1);
                if signed {
                    // Both operands' sign differs from the sum's.
                    self.alu(XOR, A4, xh, A3);
                    self.alu(XOR, A5, yh, A3);
                    self.alu(AND, A4, A4, A5);
                    self.br(BLT, A4, ZERO, fail);
                } else {
                    self.alu(SLTU, A4, A2, xh);
                    self.alu(SLTU, A5, A3, A2);
                    self.alu(OR, A4, A4, A5);
                    self.br(BNE, A4, ZERO, fail);
                }
            }
            Bin::Sub => {
                let ((xl, xh), (yl, yh)) = (self.src2(T0, T1, a), self.src2(T2, T3, b));
                self.alu(SLTU, A1, xl, yl);
                self.alu(SUB, A0, xl, yl);
                self.alu(SUB, A2, xh, yh);
                self.alu(SUB, A3, A2, A1);
                if signed {
                    // Operands of different signs, the result's not `a`'s.
                    self.alu(XOR, A4, xh, yh);
                    self.alu(XOR, A5, xh, A3);
                    self.alu(AND, A4, A4, A5);
                    self.br(BLT, A4, ZERO, fail);
                } else {
                    // a < b.
                    self.alu(SLTU, A4, xh, yh);
                    self.alu(XOR, A5, xh, yh);
                    self.seqz(A5, A5);
                    self.alu(AND, A5, A5, A1);
                    self.alu(OR, A4, A4, A5);
                    self.br(BNE, A4, ZERO, fail);
                }
            }
            _ => {
                if signed {
                    // |a| * |b| unsigned, then the sign.
                    self.alu(XOR, A7, T1, T3);
                    for (lo, hi) in [(T0, T1), (T2, T3)] {
                        let pos = self.b_fwd(BGE, hi, ZERO);
                        self.neg64(lo, hi);
                        self.here(pos);
                    }
                    self.mulo64(fail);
                    let pos = self.b_fwd(BGE, A7, ZERO);
                    // Negative: at most 2^63.
                    self.li(A2, 0x8000_0000);
                    self.br(BLTU, A2, A1, fail);
                    let ok = self.b_fwd(BNE, A1, A2);
                    self.br(BNE, A0, ZERO, fail);
                    self.here(ok);
                    self.neg64(A0, A1);
                    let done = self.b_fwd(BEQ, ZERO, ZERO);
                    self.here(pos);
                    self.br(BLT, A1, ZERO, fail);
                    self.here(done);
                } else {
                    self.mulo64(fail);
                }
                self.mv(A3, A1);
            }
        }
    }

    /// `(lo, hi)` = -`(lo, hi)` (rv32; `a6` scratch).
    pub(crate) fn neg64(&mut self, lo: u32, hi: u32) {
        self.snez(A6, lo);
        self.alu(SUB, lo, ZERO, lo);
        self.alu(SUB, hi, ZERO, hi);
        self.alu(SUB, hi, hi, A6);
    }

    /// `(a0, a1)` = `(t0, t1)` * `(t2, t3)` unsigned, to `fail` past 64
    /// bits (rv32; `a2 a3` scratch).
    pub(crate) fn mulo64(&mut self, fail: Label) {
        self.snez(A2, T1);
        self.snez(A3, T3);
        self.alu(AND, A2, A2, A3);
        self.br(BNE, A2, ZERO, fail);
        self.alu(MULHU, A2, T1, T2);
        self.br(BNE, A2, ZERO, fail);
        self.alu(MULHU, A3, T0, T3);
        self.br(BNE, A3, ZERO, fail);
        self.alu(MUL, A2, T1, T2);
        self.alu(MUL, A3, T0, T3);
        self.alu(MULHU, A1, T0, T2);
        self.alu(MUL, A0, T0, T2);
        self.alu(ADD, A1, A1, A2);
        self.br(BLTU, A1, A2, fail);
        self.alu(ADD, A1, A1, A3);
        self.br(BLTU, A1, A3, fail);
    }

    /// `t0` = the set bits of `t0` (all XLEN of it; `a0 a1` scratch).
    pub(crate) fn popcount(&mut self) {
        let xlen = self.xlen();
        let rep = |b: u8| -> i64 { i64::from_le_bytes([b; 8]) };
        self.srli(A0, T0, 1);
        self.li(A1, rep(0x55));
        self.alu(AND, A0, A0, A1);
        self.alu(SUB, T0, T0, A0);
        self.li(A1, rep(0x33));
        self.alu(AND, A0, T0, A1);
        self.srli(T0, T0, 2);
        self.alu(AND, T0, T0, A1);
        self.alu(ADD, T0, T0, A0);
        self.srli(A0, T0, 4);
        self.alu(ADD, T0, T0, A0);
        self.li(A1, rep(0x0f));
        self.alu(AND, T0, T0, A1);
        self.li(A1, rep(0x01));
        self.alu(MUL, T0, T0, A1);
        self.srli(T0, T0, xlen - 8);
    }

    /// `t0` = leading zeros of the `bits`-bit `t0` (zero-extended).
    pub(crate) fn clz(&mut self, bits: u32) {
        let mut s = 1;
        while s < bits {
            self.srli(A0, T0, s);
            self.alu(OR, T0, T0, A0);
            s *= 2;
        }
        self.popcount();
        self.li(A0, i64::from(bits));
        self.alu(SUB, T0, A0, T0);
    }

    /// `t0` = trailing zeros of the `bits`-bit `t0` (`bits` for 0).
    pub(crate) fn ctz(&mut self, bits: u32) {
        self.alu(SUB, A0, ZERO, T0);
        self.alu(AND, T0, T0, A0);
        self.addi(T0, T0, -1);
        if bits < self.xlen() {
            self.ext_reg(T0, T0, bits, false);
        }
        self.popcount();
    }

    /// `t0` = the `n` low bytes of `t0` reversed.
    pub(crate) fn bswap(&mut self, n: u32) {
        self.mv(A0, ZERO);
        for i in 0..n {
            if i == 0 {
                self.mv(A1, T0);
            } else {
                self.srli(A1, T0, 8 * i);
            }
            self.andi(A1, A1, 0xff);
            if n - 1 - i != 0 {
                self.slli(A1, A1, 8 * (n - 1 - i));
            }
            self.alu(OR, A0, A0, A1);
        }
        self.mv(T0, A0);
    }

    /// `dst = a * b + c` rounded once (`fmadd`; `f2` is no value's).
    pub(crate) fn fma(&mut self, k: K, dst: Mem, a: V, b: V, c: V) {
        let x = self.freg(a, k, F0);
        let y = self.freg(b, k, F1);
        let z = self.freg(c, k, 2);
        let fd = self.dst_f(dst).unwrap_or(F0);
        let (d, x, y, z) = (enc::F(fd), enc::F(x), enc::F(y), enc::F(z));
        self.ins(enc::fmadd(k == K::F64, d, x, y, z, Rm::Dyn));
        self.stf(fd, dst, k);
    }

    pub(crate) fn un(&mut self, op: Un, k: K, dst: Mem, a: V) {
        if k.is_float() {
            if self.rv32 {
                if op == Un::Sqrt {
                    // No helper: libm's.
                    self.soft1(if k == K::F32 { "sqrtf" } else { "sqrt" }, k, a);
                    self.st_soft(k, dst);
                    return;
                }
                // The sign bit flipped.
                self.mov_val(k, dst, a);
                let m = if k == K::F64 { dst.at(4) } else { dst };
                self.ld(T0, V::M(m), K::I32, false);
                self.li(A0, 0x8000_0000);
                self.alu(XOR, T0, T0, A0);
                self.st(T0, m, K::I32);
                return;
            }
            let x = self.freg(a, k, F0);
            let fd = self.dst_f(dst).unwrap_or(F0);
            let (dbl, d, x) = (k == K::F64, enc::F(fd), enc::F(x));
            if op == Un::Sqrt {
                self.ins(enc::fsqrt(dbl, d, x, Rm::Dyn));
            } else {
                self.ins(enc::fsgnj(true, dbl, d, x, x));
            }
            self.stf(fd, dst, k);
            return;
        }
        if self.pair(k) {
            self.un64(op, a);
            self.st2(T0, T1, dst);
            return;
        }
        let bits = k.bits();
        match op {
            Un::Neg | Un::Not => {
                let x = self.reg(a, k, op == Un::Neg, T0);
                let rd = self.dst_x(dst).unwrap_or(T0);
                if op == Un::Neg {
                    self.alu(SUB, rd, ZERO, x);
                } else {
                    self.xori(rd, x, -1);
                }
                self.st(rd, dst, k);
                return;
            }
            Un::Popcnt => {
                self.ld(T0, a, k, false);
                self.popcount();
            }
            Un::Clz => {
                self.ld(T0, a, k, false);
                self.clz(bits);
            }
            Un::Ctz => {
                self.ld(T0, a, k, false);
                self.ctz(bits);
            }
            Un::Bswap => {
                self.ld(T0, a, k, false);
                self.bswap(k.size());
            }
            Un::Sqrt => self.ld(T0, a, k, false),
        }
        self.st(T0, dst, k);
    }

    /// rv32 8-byte `Un` into `(t0, t1)`.
    pub(crate) fn un64(&mut self, op: Un, a: V) {
        self.ld2(T0, T1, a);
        match op {
            Un::Neg => self.neg64(T0, T1),
            Un::Not => {
                self.xori(T0, T0, -1);
                self.xori(T1, T1, -1);
            }
            Un::Popcnt => {
                self.mv(T2, T1);
                self.popcount();
                self.mv(T3, T0);
                self.mv(T0, T2);
                self.popcount();
                self.alu(ADD, T0, T0, T3);
                self.mv(T1, ZERO);
            }
            Un::Clz | Un::Ctz => {
                // The word that decides, else 32 + the other's.
                let (first, second) = if op == Un::Clz { (T1, T0) } else { (T0, T1) };
                self.mv(T2, second);
                self.mv(T3, first);
                self.mv(T0, first);
                if op == Un::Clz {
                    self.clz(32);
                } else {
                    self.ctz(32);
                }
                let done = self.b_fwd(BNE, T3, ZERO);
                self.mv(T0, T2);
                if op == Un::Clz {
                    self.clz(32);
                } else {
                    self.ctz(32);
                }
                self.addi(T0, T0, 32);
                self.here(done);
                self.mv(T1, ZERO);
            }
            Un::Bswap => {
                self.mv(T2, T1);
                self.bswap(4);
                self.mv(T3, T0);
                self.mv(T0, T2);
                self.bswap(4);
                self.mv(T1, T3);
            }
            Un::Sqrt => {}
        }
    }

    /// rv32 soft float: `a0` (`a0 a1`) = `name(a)`.
    pub(crate) fn soft1(&mut self, name: &str, k: K, a: V) {
        if k == K::F64 {
            self.ld2(A0, A1, a);
        } else {
            self.ld(A0, a, K::I32, false);
        }
        self.helper(name);
    }

    /// rv32 soft float: `a0` (`a0 a1`) = `name(a, b)`.
    pub(crate) fn soft2(&mut self, name: &str, k: K, a: V, b: V) {
        if k == K::F64 {
            self.ld2(A0, A1, a);
            self.ld2(A2, A3, b);
        } else {
            self.ld(A0, a, K::I32, false);
            self.ld(A1, b, K::I32, false);
        }
        self.helper(name);
    }

    /// Store a soft-float helper's `k` result.
    pub(crate) fn st_soft(&mut self, k: K, dst: Mem) {
        if k == K::F64 {
            self.st2(A0, A1, dst);
        } else {
            self.st(A0, dst, K::I32);
        }
    }

    pub(crate) fn conv(&mut self, conv: Conv, from: K, to: K, dst: Mem, a: V) {
        match conv {
            // Lowered to the runtime call here (`FnLower::half_conv`).
            Conv::Half => unreachable!("no half conversion instruction"),
            Conv::Int { signed } => {
                if self.pair(to) {
                    if self.pair(from) {
                        self.mov_val(K::I64, dst, a);
                        return;
                    }
                    let d = self.dst2(dst);
                    self.ld(d.0, a, from, signed);
                    if signed {
                        self.srai(d.1, d.0, 31);
                    } else {
                        self.mv(d.1, ZERO);
                    }
                    self.put2(d, dst);
                } else {
                    let rd = self.dst_x(dst).unwrap_or(T0);
                    self.ld(rd, a, from, signed);
                    let zx = !signed && from.size() <= to.size();
                    self.st_ext(rd, dst, to, zx);
                }
            }
            Conv::ToBool => {
                let rd = self.dst_x(dst).unwrap_or(T0);
                let x = if self.pair(from) {
                    self.ld2(T0, T1, a);
                    self.alu(OR, T0, T0, T1);
                    T0
                } else {
                    self.reg(a, from, false, T0)
                };
                self.snez(rd, x);
                self.st_ext(rd, dst, K::I8, true);
            }
            Conv::IToF { signed } => {
                if self.rv32 {
                    let wide = self.pair(from);
                    if wide {
                        self.ld2(A0, A1, a);
                    } else {
                        self.ld(A0, a, from, signed);
                    }
                    self.helper(match (wide, signed, to) {
                        (true, true, K::F32) => "__floatdisf",
                        (true, false, K::F32) => "__floatundisf",
                        (true, true, _) => "__floatdidf",
                        (true, false, _) => "__floatundidf",
                        (false, true, K::F32) => "__floatsisf",
                        (false, false, K::F32) => "__floatunsisf",
                        (false, true, _) => "__floatsidf",
                        (false, false, _) => "__floatunsidf",
                    });
                    self.st_soft(to, dst);
                    return;
                }
                let x = self.reg(a, from, signed, T0);
                let fd = self.dst_f(dst).unwrap_or(F0);
                let t = if signed { IntTy::L } else { IntTy::Lu };
                self.ins(enc::fcvt_from_int(
                    to == K::F64,
                    t,
                    enc::F(fd),
                    X(x),
                    Rm::Dyn,
                ));
                self.stf(fd, dst, to);
            }
            Conv::FToI { signed, bits } => {
                if self.rv32 {
                    self.f_to_i_soft(from, to, dst, a, signed, bits);
                } else {
                    self.f_to_i(from, to, dst, a, signed, bits);
                }
            }
            Conv::FToF => {
                if self.rv32 {
                    let name = if from == K::F32 {
                        "__extendsfdf2"
                    } else {
                        "__truncdfsf2"
                    };
                    self.soft1(name, from, a);
                    self.st_soft(to, dst);
                    return;
                }
                let x = self.freg(a, from, F0);
                let fd = self.dst_f(dst).unwrap_or(F0);
                // fcvt.d.s (exact, written `rne`) / fcvt.s.d
                let (d, x) = (enc::F(fd), enc::F(x));
                if from == K::F32 {
                    self.ins(enc::fcvt_ff(true, d, x, Rm::Rne));
                } else {
                    self.ins(enc::fcvt_ff(false, d, x, Rm::Dyn));
                }
                self.stf(fd, dst, to);
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

    /// rv64 float -> integer, saturating, NaN to 0: `fcvt` saturates (NaN
    /// to the maximum, masked by `feq`), then the clamp to `bits`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn f_to_i(&mut self, from: K, to: K, dst: Mem, a: V, signed: bool, bits: u32) {
        let x = self.freg(a, from, F0);
        let (dbl, f) = (from == K::F64, enc::F(x));
        let t = if signed { IntTy::L } else { IntTy::Lu };
        self.ins(enc::fcmp(FCmp::Eq, dbl, X(A0), f, f));
        self.ins(enc::fcvt_to_int(t, dbl, X(T0), f, Rm::Rtz));
        self.alu(SUB, A0, ZERO, A0);
        self.alu(AND, T0, T0, A0);
        if bits < 64 {
            let (max, min) = Self::range(signed, bits);
            let lt = if signed { BGE } else { BGEU };
            self.li(A1, max);
            let ok = self.b_fwd(lt, A1, T0);
            self.mv(T0, A1);
            self.here(ok);
            if signed {
                self.li(A1, min);
                let ok = self.b_fwd(BGE, T0, A1);
                self.mv(T0, A1);
                self.here(ok);
            }
        }
        self.st(T0, dst, to);
    }

    /// rv32 soft float -> integer: the value as an `f64` in the scratch
    /// area; NaN 0, out of range the bound, else the helper; the clamp to
    /// `bits`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn f_to_i_soft(&mut self, from: K, to: K, dst: Mem, a: V, signed: bool, bits: u32) {
        if from == K::F32 {
            self.soft1("__extendsfdf2", K::F32, a);
        } else {
            self.ld2(A0, A1, a);
        }
        self.st_word(A0, SP, 0);
        self.st_word(A1, SP, 4);
        let reload = |e: &mut Self| {
            e.ld_word(A0, SP, 0);
            e.ld_word(A1, SP, 4);
        };
        // NaN.
        self.mv(A2, A0);
        self.mv(A3, A1);
        self.helper("__unorddf2");
        let nan = self.b_fwd(BNE, A0, ZERO);
        // The bounds: 2^63 / -2^63, or 0 / 2^64.
        let (hi_bits, lo_bits) = if signed {
            (0x43e0_0000, 0xc3e0_0000_u32 as i64)
        } else {
            (0x43f0_0000, 0)
        };
        reload(self);
        self.mv(A2, ZERO);
        self.li(A3, hi_bits);
        self.helper("__gedf2");
        let max = self.b_fwd(BGE, A0, ZERO);
        reload(self);
        self.mv(A2, ZERO);
        self.li(A3, lo_bits);
        self.helper("__ltdf2");
        let min = self.b_fwd(BLT, A0, ZERO);
        reload(self);
        self.helper(if signed { "__fixdfdi" } else { "__fixunsdfdi" });
        self.mv(T0, A0);
        self.mv(T1, A1);
        let done = self.b_fwd(BEQ, ZERO, ZERO);
        self.here(max);
        self.li(T0, -1);
        self.li(T1, if signed { 0x7fff_ffff } else { -1 });
        let done2 = self.b_fwd(BEQ, ZERO, ZERO);
        self.here(min);
        if signed {
            self.mv(T0, ZERO);
            self.li(T1, 0x8000_0000);
            let done3 = self.b_fwd(BEQ, ZERO, ZERO);
            self.here(nan);
            self.mv(T0, ZERO);
            self.mv(T1, ZERO);
            self.here(done3);
        } else {
            self.here(nan);
            self.mv(T0, ZERO);
            self.mv(T1, ZERO);
        }
        self.here(done);
        self.here(done2);
        if bits < 64 {
            let (max, min) = Self::range(signed, bits);
            // max < x: max.
            self.li(T2, max);
            self.li(T3, max >> 32);
            self.lt64(signed, (T2, T3), (T0, T1), A0, (A1, A2));
            let ok = self.b_fwd(BEQ, A0, ZERO);
            self.mv(T0, T2);
            self.mv(T1, T3);
            self.here(ok);
            if signed {
                self.li(T2, min);
                self.li(T3, min >> 32);
                self.lt64(true, (T0, T1), (T2, T3), A0, (A1, A2));
                let ok = self.b_fwd(BEQ, A0, ZERO);
                self.mv(T0, T2);
                self.mv(T1, T3);
                self.here(ok);
            }
        }
        if self.pair(to) {
            self.st2(T0, T1, dst);
        } else {
            self.st(T0, dst, to);
        }
    }
}
