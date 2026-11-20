//! Arithmetic, logic, unary operations and conversions.

#[allow(clippy::wildcard_imports)]
use crate::isa::arm32::*;

impl Em<'_> {
    pub(crate) fn bin(&mut self, op: Bin, k: K, signed: bool, dst: Mem, a: V, b: V) {
        if k.is_float() {
            let x = self.fsrc(a, k, 0);
            let y = self.fsrc(b, k, 1);
            let opc = match op {
                Bin::Add => Vop::Add,
                Bin::Sub => Vop::Sub,
                Bin::Mul => Vop::Mul,
                _ => Vop::Div,
            };
            if let Some(n) = self.dst_d(dst, k) {
                self.vfp(opc, k, n, x, y);
            } else {
                let r = Self::fr(0, k);
                self.vfp(opc, k, r, x, y);
                self.stf(r, dst, k);
            }
            return;
        }
        if wide(k) {
            if matches!(op, Bin::Add | Bin::Sub | Bin::And | Bin::Or | Bin::Xor) {
                let x = self.src2(self.x, self.xh, a);
                let y = self.src2(self.y, self.yh, b);
                let d = self.dst2(dst);
                let (lo, hi, s) = match op {
                    Bin::Add => (Op::Add, Op::Adc, true),
                    Bin::Sub => (Op::Sub, Op::Sbc, true),
                    _ => (Self::alu(op), Self::alu(op), false),
                };
                self.dp(Cond::AL, lo, s, d.0, x.0, y.0);
                self.dp(Cond::AL, hi, false, d.1, x.1, y.1);
                self.put2(d, dst);
                return;
            }
            self.bin64(op, signed, a, b);
            self.st2(self.x, self.xh, dst);
            return;
        }
        match op {
            Bin::Div | Bin::Rem => {
                self.ld(R0, a, k, signed);
                self.ld(R1, b, k, signed);
                let name = match (op, signed) {
                    (Bin::Div, true) => "__aeabi_idiv",
                    (Bin::Div, false) => "__aeabi_uidiv",
                    (_, true) => "__aeabi_idivmod",
                    _ => "__aeabi_uidivmod",
                };
                self.helper(name);
                let r = if op == Bin::Rem { R1 } else { R0 };
                self.st(r, dst, k);
            }
            Bin::Shl | Bin::Shr | Bin::Rotl => {
                let ty = match (op, signed) {
                    (Bin::Shl, _) => Shift::Lsl,
                    (Bin::Rotl, _) => Shift::Ror,
                    (_, true) => Shift::Asr,
                    _ => Shift::Lsr,
                };
                let x = self.src(self.x, a, k, signed);
                let d = self.dst_r(dst);
                if let V::I(i) = b {
                    // Left by `i` is right by `32 - i`.
                    let n = if op == Bin::Rotl {
                        (32 - (i & 31)) & 31
                    } else {
                        i & 31
                    };
                    if n != 0 {
                        self.dpsi(Cond::AL, Op::Mov, false, d, 0, x, ty, n as u32);
                    } else {
                        self.mov(d, x);
                    }
                } else {
                    let mut y = self.src(self.y, b, k, false);
                    if op == Bin::Rotl {
                        // Right by `-y`: `ror` takes its low byte, by 32
                        // or none at a multiple of 32.
                        self.dpi(Cond::AL, Op::Rsb, false, IP, y, Imm::small(0));
                        y = IP;
                    }
                    self.dps(Op::Mov, false, d, 0, x, ty, y);
                }
                self.put(d, dst, k);
            }
            Bin::Mul => {
                let x = self.src(self.x, a, k, signed);
                let y = self.src(self.y, b, k, signed);
                let d = self.dst_r(dst);
                self.ins(enc::mul(R(d), R(x), R(y)));
                self.put(d, dst, k);
            }
            Bin::MulHi => {
                let x = self.src(self.x, a, k, signed);
                let y = self.src(self.y, b, k, signed);
                let d = self.dst_r(dst);
                self.ins(enc::mull(signed, R(IP), R(d), R(x), R(y)));
                self.put(d, dst, k);
            }
            _ => {
                let x = self.src(self.x, a, k, signed);
                let d = self.dst_r(dst);
                let opc = Self::alu(op);
                if let V::I(i) = b {
                    let imm = ext32(i, k, signed);
                    let alt = match op {
                        Bin::Add => Some((Op::Sub, imm.wrapping_neg())),
                        Bin::Sub => Some((Op::Add, imm.wrapping_neg())),
                        Bin::And => Some((Op::Bic, !imm)),
                        _ => None,
                    };
                    if let Some(e) = modimm(imm) {
                        self.dpi(Cond::AL, opc, false, d, x, e);
                        self.put(d, dst, k);
                        return;
                    }
                    if let Some((o2, e)) = alt.and_then(|(o2, v)| modimm(v).map(|e| (o2, e))) {
                        self.dpi(Cond::AL, o2, false, d, x, e);
                        self.put(d, dst, k);
                        return;
                    }
                }
                let y = self.src(self.y, b, k, signed);
                self.dp(Cond::AL, opc, false, d, x, y);
                self.put(d, dst, k);
            }
        }
    }

    /// The data-processing opcode of `op`.
    pub(crate) const fn alu(op: Bin) -> Op {
        match op {
            Bin::Add => Op::Add,
            Bin::Sub => Op::Sub,
            Bin::And => Op::And,
            Bin::Or => Op::Orr,
            _ => Op::Eor,
        }
    }

    /// `(r4, r5)` = `a op b`, 8 bytes; `b` in `(r6, r7)`.
    pub(crate) fn bin64(&mut self, op: Bin, signed: bool, a: V, b: V) {
        match op {
            Bin::Div | Bin::Rem => {
                self.ld2(R0, R1, a);
                self.ld2(R2, R3, b);
                self.helper(if signed {
                    "__aeabi_ldivmod"
                } else {
                    "__aeabi_uldivmod"
                });
                // The quotient in r0 r1, the remainder in r2 r3.
                if op == Bin::Rem {
                    self.mov(self.x, R2);
                    self.mov(self.xh, R3);
                } else {
                    self.mov(self.x, R0);
                    self.mov(self.xh, R1);
                }
                return;
            }
            Bin::Shl | Bin::Shr => {
                self.ld2(self.x, self.xh, a);
                self.ld(self.y, b, K::I32, false);
                self.shift64(op == Bin::Shl, signed);
                return;
            }
            Bin::Rotl => unreachable!("an 8-byte rotate is two shifts here"),
            Bin::MulHi => unreachable!("no 8-byte high product here"),
            _ => {}
        }
        self.ld2(self.x, self.xh, a);
        self.ld2(self.y, self.yh, b);
        match op {
            Bin::Add => {
                self.dp(Cond::AL, Op::Add, true, self.x, self.x, self.y);
                self.dp(Cond::AL, Op::Adc, false, self.xh, self.xh, self.yh);
            }
            Bin::Sub => {
                self.dp(Cond::AL, Op::Sub, true, self.x, self.x, self.y);
                self.dp(Cond::AL, Op::Sbc, false, self.xh, self.xh, self.yh);
            }
            Bin::Mul => self.mul64(),
            _ => {
                let opc = Self::alu(op);
                self.dp(Cond::AL, opc, false, self.x, self.x, self.y);
                self.dp(Cond::AL, opc, false, self.xh, self.xh, self.yh);
            }
        }
    }

    /// `(r4, r5)` *= `(r6, r7)`, the low 8 bytes.
    pub(crate) fn mul64(&mut self) {
        // umull lo, hi, x, y; mla hi, x, yh, hi; mla hi, xh, y, hi (`lo hi`:
        // `r0 r1`, or `ip lr` where those are `x xh`)
        let (lo, hi) = if self.opt { (IP, LR) } else { (R0, R1) };
        self.ins(enc::mull(false, R(lo), R(hi), R(self.x), R(self.y)));
        self.ins(enc::mla(R(hi), R(self.x), R(self.yh), R(hi)));
        self.ins(enc::mla(R(hi), R(self.xh), R(self.y), R(hi)));
        self.mov(self.x, lo);
        self.mov(self.xh, hi);
    }

    /// `(r4, r5)` shifted by `r6` (below 64). A register shift of 32 or
    /// more yields 0 (`asr`: the sign), and so does one by a negative
    /// amount (its low byte is 224 or more).
    pub(crate) fn shift64(&mut self, left: bool, signed: bool) {
        if left {
            self.dps(Op::Mov, false, self.xh, 0, self.xh, Shift::Lsl, self.y);
            self.dpi(Cond::AL, Op::Rsb, false, IP, self.y, Imm::small(32));
            self.dps(Op::Orr, false, self.xh, self.xh, self.x, Shift::Lsr, IP);
            self.dpi(Cond::AL, Op::Sub, false, IP, self.y, Imm::small(32));
            self.dps(Op::Orr, false, self.xh, self.xh, self.x, Shift::Lsl, IP);
            self.dps(Op::Mov, false, self.x, 0, self.x, Shift::Lsl, self.y);
            return;
        }
        self.dps(Op::Mov, false, self.x, 0, self.x, Shift::Lsr, self.y);
        self.dpi(Cond::AL, Op::Rsb, false, IP, self.y, Imm::small(32));
        self.dps(Op::Orr, false, self.x, self.x, self.xh, Shift::Lsl, IP);
        if signed {
            // The high word's sign only from 32 on.
            self.dpi(Cond::AL, Op::Sub, true, IP, self.y, Imm::small(32));
            let i = enc::dp_rsr(
                Cond::GE,
                Op::Orr,
                false,
                R(self.x),
                R(self.x),
                R(self.xh),
                Shift::Asr,
                R(IP),
            );
            self.ins(i);
            self.dps(Op::Mov, false, self.xh, 0, self.xh, Shift::Asr, self.y);
        } else {
            self.dpi(Cond::AL, Op::Sub, false, IP, self.y, Imm::small(32));
            self.dps(Op::Orr, false, self.x, self.x, self.xh, Shift::Lsr, IP);
            self.dps(Op::Mov, false, self.xh, 0, self.xh, Shift::Lsr, self.y);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn ovf(&mut self, op: Bin, k: K, signed: bool, dst: Mem, a: V, b: V, fail: Label) {
        match k {
            K::I64 => {
                if op == Bin::Mul {
                    self.ld2(self.x, self.xh, a);
                    self.ld2(self.y, self.yh, b);
                    if signed {
                        self.mulo64_signed(fail);
                    } else {
                        self.mulo64(fail);
                    }
                    self.st2(R0, R1, dst);
                    return;
                }
                let x = self.src2(self.x, self.xh, a);
                let y = self.src2(self.y, self.yh, b);
                // Into scratch: on overflow the destination keeps its value.
                let (lo, hi) = if op == Bin::Add {
                    (Op::Add, Op::Adc)
                } else {
                    (Op::Sub, Op::Sbc)
                };
                self.dp(Cond::AL, lo, true, self.x, x.0, y.0);
                self.dp(Cond::AL, hi, true, self.xh, x.1, y.1);
                let cond = match (op, signed) {
                    (_, true) => Cond::VS,
                    (Bin::Add, false) => Cond::HS,
                    _ => Cond::LO,
                };
                self.b(cond, fail);
                self.st2(self.x, self.xh, dst);
            }
            K::I32 => {
                let x = self.src(self.x, a, k, signed);
                let y = self.src(self.y, b, k, signed);
                if op == Bin::Mul {
                    // smull / umull r0, r1, x, y: the high word is the
                    // low one's sign (zero).
                    self.ins(enc::mull(signed, R(R0), R(R1), R(x), R(y)));
                    if signed {
                        self.dpsi(Cond::AL, Op::Cmp, true, 0, R1, R0, Shift::Asr, 31);
                    } else {
                        self.dpi(Cond::AL, Op::Cmp, true, 0, R1, Imm::small(0));
                    }
                    self.b(Cond::NE, fail);
                    self.mov(self.x, R0);
                } else {
                    let opc = if op == Bin::Add { Op::Add } else { Op::Sub };
                    self.dp(Cond::AL, opc, true, self.x, x, y);
                    let cond = match (op, signed) {
                        (_, true) => Cond::VS,
                        (Bin::Add, false) => Cond::HS,
                        _ => Cond::LO,
                    };
                    self.b(cond, fail);
                }
                self.st(self.x, dst, k);
            }
            _ => {
                // 8 / 16 bits in 32: the result must equal its own
                // extension.
                let x = self.src(self.x, a, k, signed);
                let y = self.src(self.y, b, k, signed);
                match op {
                    Bin::Add => self.dp(Cond::AL, Op::Add, false, self.x, x, y),
                    Bin::Sub => self.dp(Cond::AL, Op::Sub, false, self.x, x, y),
                    _ => self.ins(enc::mul(R(self.x), R(x), R(y))),
                }
                let t = if self.opt { IP } else { R0 };
                self.extend_reg(t, self.x, k, signed);
                self.dp(Cond::AL, Op::Cmp, true, 0, t, self.x);
                self.b(Cond::NE, fail);
                self.st(self.x, dst, k);
            }
        }
    }

    /// `rd` = the low `k` (8 / 16 bits) of `rm`, extended by `signed`.
    pub(crate) fn extend_reg(&mut self, rd: u32, rm: u32, k: K, signed: bool) {
        self.ins(enc::ext(signed, k != K::I8, R(rd), R(rm)));
    }

    /// `(r0, r1)` = `(x, xh)` * `(y, yh)` unsigned, to `fail` past 64
    /// bits.
    pub(crate) fn mulo64(&mut self, fail: Label) {
        // Both high words non-zero: too big.
        self.dpi(Cond::AL, Op::Cmp, true, 0, self.xh, Imm::small(0));
        self.dpi(Cond::NE, Op::Cmp, true, 0, self.yh, Imm::small(0));
        self.b(Cond::NE, fail);
        if self.opt {
            // `x xh y yh` are `r0`-`r3`: umull ip, lr, r1, r2 and umull
            // r1, lr, r0, r3 (one of them 0, so no carry adding the low
            // words), then umull r0, r1, r0, r2.
            self.ins(enc::mull(false, R(IP), R(LR), R(R1), R(R2)));
            self.dpi(Cond::AL, Op::Cmp, true, 0, LR, Imm::small(0));
            self.b(Cond::NE, fail);
            self.ins(enc::mull(false, R(R1), R(LR), R(R0), R(R3)));
            self.dpi(Cond::AL, Op::Cmp, true, 0, LR, Imm::small(0));
            self.b(Cond::NE, fail);
            self.dp(Cond::AL, Op::Add, false, IP, IP, R1);
            self.ins(enc::mull(false, R(R0), R(R1), R(R0), R(R2)));
            self.dp(Cond::AL, Op::Add, true, R1, R1, IP);
            self.b(Cond::HS, fail);
            return;
        }
        // umull r0, r1, r4, r6; umull r2, r3, r5, r6; umull r9, r10, r4, r7
        let (x, y) = (R9, R10);
        self.ins(enc::mull(false, R(R0), R(R1), R(self.x), R(self.y)));
        self.ins(enc::mull(false, R(R2), R(R3), R(self.xh), R(self.y)));
        self.ins(enc::mull(false, R(x), R(y), R(self.x), R(self.yh)));
        self.dp(Cond::AL, Op::Orr, true, R3, R3, y);
        self.b(Cond::NE, fail);
        self.dp(Cond::AL, Op::Add, true, R1, R1, R2);
        self.b(Cond::HS, fail);
        self.dp(Cond::AL, Op::Add, true, R1, R1, x);
        self.b(Cond::HS, fail);
    }

    /// `(r0, r1)` = `(x, xh)` * `(y, yh)` signed through `__mulodi4`,
    /// to `fail` on overflow (its flag in the scratch area).
    pub(crate) fn mulo64_signed(&mut self, fail: Label) {
        self.mov(R0, self.x);
        self.mov(R1, self.xh);
        self.mov(R2, self.y);
        self.mov(R3, self.yh);
        self.add_imm(IP, SP, 8);
        self.ls(Acc::Str, IP, SP, 0, R8);
        self.helper("__mulodi4");
        self.ls(Acc::Ldr, IP, SP, 8, IP);
        self.dpi(Cond::AL, Op::Cmp, true, 0, IP, Imm::small(0));
        self.b(Cond::NE, fail);
    }

    /// `dst = a * b + c` rounded once: `vfma` into a copy of `c` in
    /// scratch 2.
    pub(crate) fn fma(&mut self, k: K, dst: Mem, a: V, b: V, c: V) {
        let x = self.fsrc(a, k, 0);
        let y = self.fsrc(b, k, 1);
        let acc = Self::fr(2, k);
        let z = self.fsrc(c, k, 2);
        if z != acc {
            self.vfp(Vop::Mov, k, acc, 0, z);
        }
        self.vfp(Vop::Fma, k, acc, x, y);
        if let Some(n) = self.dst_d(dst, k) {
            self.vfp(Vop::Mov, k, n, 0, acc);
        } else {
            self.stf(acc, dst, k);
        }
    }

    pub(crate) fn un(&mut self, op: Un, k: K, dst: Mem, a: V) {
        if k.is_float() {
            let x = self.fsrc(a, k, 0);
            let opc = if op == Un::Sqrt { Vop::Sqrt } else { Vop::Neg };
            if let Some(n) = self.dst_d(dst, k) {
                self.vfp(opc, k, n, 0, x);
            } else {
                let r = Self::fr(0, k);
                self.vfp(opc, k, r, 0, x);
                self.stf(r, dst, k);
            }
            return;
        }
        if wide(k) {
            self.ld2(self.x, self.xh, a);
            match op {
                Un::Neg => {
                    self.dpi(Cond::AL, Op::Rsb, true, self.x, self.x, Imm::small(0));
                    self.dpi(Cond::AL, Op::Rsc, false, self.xh, self.xh, Imm::small(0));
                }
                Un::Not => {
                    self.dp(Cond::AL, Op::Mvn, false, self.x, 0, self.x);
                    self.dp(Cond::AL, Op::Mvn, false, self.xh, 0, self.xh);
                }
                Un::Popcnt => {
                    self.vmov_dr(0, self.x, self.xh, false);
                    self.popcnt_d0();
                    self.vmov_sr(0, self.x, true);
                    self.mov_imm(self.xh, 0);
                }
                Un::Clz => {
                    self.clz(self.y, self.xh);
                    self.clz(self.x, self.x);
                    self.dpi(Cond::AL, Op::Cmp, true, 0, self.xh, Imm::small(0));
                    self.dpi(Cond::EQ, Op::Add, false, self.y, self.x, Imm::small(32));
                    self.mov(self.x, self.y);
                    self.mov_imm(self.xh, 0);
                }
                Un::Ctz => {
                    self.ctz(self.y, self.x);
                    self.ctz(self.yh, self.xh);
                    self.dpi(Cond::AL, Op::Cmp, true, 0, self.x, Imm::small(0));
                    self.dpi(Cond::EQ, Op::Add, false, self.y, self.yh, Imm::small(32));
                    self.mov(self.x, self.y);
                    self.mov_imm(self.xh, 0);
                }
                Un::Bswap => {
                    self.rev(self.y, self.x);
                    self.rev(self.x, self.xh);
                    self.mov(self.xh, self.y);
                }
                Un::Sqrt => {}
            }
            self.st2(self.x, self.xh, dst);
            return;
        }
        let bits = k.bits();
        match op {
            Un::Neg => {
                self.ld(self.x, a, k, true);
                self.dpi(Cond::AL, Op::Rsb, false, self.x, self.x, Imm::small(0));
            }
            Un::Not => {
                self.ld(self.x, a, k, false);
                self.dp(Cond::AL, Op::Mvn, false, self.x, 0, self.x);
            }
            Un::Popcnt => {
                self.ld(self.x, a, k, false);
                self.mov_imm(self.xh, 0);
                self.vmov_dr(0, self.x, self.xh, false);
                self.popcnt_d0();
                self.vmov_sr(0, self.x, true);
            }
            Un::Clz => {
                self.ld(self.x, a, k, false);
                self.clz(self.x, self.x);
                if bits < 32 {
                    self.dpi(
                        Cond::AL,
                        Op::Sub,
                        false,
                        self.x,
                        self.x,
                        Imm::small(32 - bits),
                    );
                }
            }
            Un::Ctz => {
                self.ld(self.x, a, k, false);
                if bits < 32 {
                    let e = modimm(1 << bits).unwrap_or(Imm::small(0));
                    self.dpi(Cond::AL, Op::Orr, false, self.x, self.x, e);
                }
                self.ctz(self.x, self.x);
            }
            Un::Bswap => {
                self.ld(self.x, a, k, false);
                match k {
                    K::I16 => self.ins(enc::rev16(R(self.x), R(self.x))),
                    K::I32 => self.rev(self.x, self.x),
                    _ => {}
                }
            }
            Un::Sqrt => self.ld(self.x, a, k, false),
        }
        self.st(self.x, dst, k);
    }

    pub(crate) fn clz(&mut self, rd: u32, rm: u32) {
        self.ins(enc::clz(R(rd), R(rm)));
    }

    /// `rd` = trailing zeros of `rm` (`rbit`, `clz`).
    pub(crate) fn ctz(&mut self, rd: u32, rm: u32) {
        self.ins(enc::rbit(R(rd), R(rm)));
        self.clz(rd, rd);
    }

    pub(crate) fn rev(&mut self, rd: u32, rm: u32) {
        self.ins(enc::rev(R(rd), R(rm)));
    }

    /// `s0` = the bit count of `d0`: `vcnt.8`, then pairwise sums.
    pub(crate) fn popcnt_d0(&mut self) {
        self.ins(enc::vcnt8(D(0), D(0)));
        for size in 0..3 {
            self.ins(enc::vpaddl_u(size, D(0), D(0)));
        }
    }

    pub(crate) fn conv(&mut self, conv: Conv, from: K, to: K, dst: Mem, a: V) {
        match conv {
            // Lowered to the runtime call here (`FnLower::half_conv`).
            Conv::Half => unreachable!("no half conversion instruction"),
            Conv::Int { signed } => {
                if wide(to) {
                    if wide(from) {
                        self.mov_val(K::I64, dst, a);
                        return;
                    }
                    let d = self.dst2(dst);
                    self.ld(d.0, a, from, signed);
                    if signed {
                        self.dpsi(Cond::AL, Op::Mov, false, d.1, 0, d.0, Shift::Asr, 31);
                    } else {
                        self.mov_imm(d.1, 0);
                    }
                    self.put2(d, dst);
                } else if let Some(d) = self.ireg(dst) {
                    self.ld(d, a, from, signed);
                    if to.bits() < 32 && (signed || from.bits() > to.bits()) {
                        self.extend_reg(d, d, to, false);
                    }
                } else {
                    self.ld(self.x, a, from, signed);
                    self.st(self.x, dst, to);
                }
            }
            Conv::ToBool => {
                if wide(from) {
                    self.ld2(self.x, self.xh, a);
                    self.dp(Cond::AL, Op::Orr, true, self.x, self.x, self.xh);
                } else {
                    self.ld(self.x, a, from, false);
                    self.dpi(Cond::AL, Op::Cmp, true, 0, self.x, Imm::small(0));
                }
                self.mov_imm_cc(Cond::NE, self.x, 1);
                self.st(self.x, dst, K::I8);
            }
            Conv::IToF { signed } => {
                if wide(from) {
                    // Run-time ABI helpers: the base standard, results in
                    // r0 (r1).
                    self.ld2(R0, R1, a);
                    let name = match (to, signed) {
                        (K::F64, true) => "__aeabi_l2d",
                        (K::F64, false) => "__aeabi_ul2d",
                        (_, true) => "__aeabi_l2f",
                        _ => "__aeabi_ul2f",
                    };
                    self.helper(name);
                    if to == K::F64 {
                        self.st2(R0, R1, dst);
                    } else {
                        self.st(R0, dst, K::I32);
                    }
                    return;
                }
                let r = self.src(self.x, a, from, signed);
                self.vmov_sr(0, r, false);
                let i = enc::vcvt_from_int(to == K::F64, signed, enc::V(0), S(0));
                self.ins(i);
                self.stf(0, dst, to);
            }
            Conv::FToI { signed, bits } => self.f_to_i(from, to, dst, a, signed, bits),
            Conv::FToF => {
                let x = self.fsrc(a, from, 0);
                if from == K::F32 {
                    self.ins(enc::vcvt_f64_f32(D(0), S(0)));
                } else {
                    self.ins(enc::vcvt_f32_f64(S(0), D(x)));
                }
                self.stf(Self::fr(0, to), dst, to);
            }
            Conv::Bits => self.mov_val(to, dst, a),
        }
    }

    /// Float -> integer, saturating, NaN to 0 (`bits` of the destination
    /// are the integer's). `vcvt` to 32 bits saturates and takes NaN to 0
    /// itself; 64 bits go through the run-time ABI helpers, the
    /// operand's range checked first.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn f_to_i(&mut self, from: K, to: K, dst: Mem, a: V, signed: bool, bits: u32) {
        if bits <= 32 {
            let x = self.fsrc(a, from, 0);
            let i = enc::vcvt_to_int(from == K::F64, signed, S(0), enc::V(x));
            self.ins(i);
            self.vmov_sr(0, self.x, true);
            if bits < 32 {
                let (max, min) = if signed {
                    ((1i64 << (bits - 1)) - 1, -(1i64 << (bits - 1)))
                } else {
                    ((1i64 << bits) - 1, 0)
                };
                self.mov_imm(self.y, max as u32);
                self.dp(Cond::AL, Op::Cmp, true, 0, self.x, self.y);
                let c = if signed { Cond::GT } else { Cond::HI };
                self.dp(c, Op::Mov, false, self.x, 0, self.y);
                if signed {
                    self.mov_imm(self.y, min as u32);
                    self.dp(Cond::AL, Op::Cmp, true, 0, self.x, self.y);
                    self.dp(Cond::LT, Op::Mov, false, self.x, 0, self.y);
                }
            }
            if wide(to) {
                if signed {
                    self.dpsi(Cond::AL, Op::Mov, false, self.xh, 0, self.x, Shift::Asr, 31);
                } else {
                    self.mov_imm(self.xh, 0);
                }
                self.st2(self.x, self.xh, dst);
            } else {
                self.st(self.x, dst, to);
            }
            return;
        }
        self.ldf(Self::fr(0, from), a, from);
        if from == K::F32 {
            self.ins(enc::vcvt_f64_f32(D(0), S(0)));
        }
        // NaN: 0.
        self.vfp(Vop::Cmp, K::F64, 0, 0, 0);
        self.ins(enc::vmrs());
        let nan = self.b_fwd(Cond::VS);
        let mut zero = Vec::new();
        let mut max_at = Vec::new();
        if signed {
            // At least 2^63: the maximum; below -2^63: the minimum.
            self.mov_imm(IP, 0);
            self.mov_imm(LR, 0x43e0_0000);
            self.vmov_dr(1, IP, LR, false);
            self.vfp(Vop::Cmp, K::F64, 0, 0, 1);
            self.ins(enc::vmrs());
            max_at.push(self.b_fwd(Cond::GE));
            self.vfp(Vop::Neg, K::F64, 1, 0, 1);
            self.vfp(Vop::Cmp, K::F64, 0, 0, 1);
            self.ins(enc::vmrs());
            let min = self.b_fwd(Cond::MI);
            self.vmov_dr(0, R0, R1, true);
            self.helper("__aeabi_d2lz");
            let done = self.b_fwd(Cond::AL);
            self.here(min);
            self.mov_imm(R0, 0);
            self.mov_imm(R1, 0x8000_0000);
            let done2 = self.b_fwd(Cond::AL);
            for at in max_at {
                self.here(at);
            }
            self.mov_imm(R0, u32::MAX);
            self.mov_imm(R1, 0x7fff_ffff);
            let done3 = self.b_fwd(Cond::AL);
            self.here(nan);
            self.mov_imm(R0, 0);
            self.mov_imm(R1, 0);
            for at in [done, done2, done3] {
                self.here(at);
            }
        } else {
            // Below 0: 0; at least 2^64: the maximum.
            self.ins(enc::vcmp_zero(D(0)));
            self.ins(enc::vmrs());
            zero.push(self.b_fwd(Cond::MI));
            self.mov_imm(IP, 0);
            self.mov_imm(LR, 0x43f0_0000);
            self.vmov_dr(1, IP, LR, false);
            self.vfp(Vop::Cmp, K::F64, 0, 0, 1);
            self.ins(enc::vmrs());
            let max = self.b_fwd(Cond::GE);
            self.vmov_dr(0, R0, R1, true);
            self.helper("__aeabi_d2ulz");
            let done = self.b_fwd(Cond::AL);
            self.here(max);
            self.mov_imm(R0, u32::MAX);
            self.mov_imm(R1, u32::MAX);
            let done2 = self.b_fwd(Cond::AL);
            self.here(nan);
            for at in zero {
                self.here(at);
            }
            self.mov_imm(R0, 0);
            self.mov_imm(R1, 0);
            self.here(done);
            self.here(done2);
        }
        if bits < 64 {
            self.clamp64(signed, bits);
        }
        self.st2(R0, R1, dst);
    }

    /// `(r0, r1)` clamped to `bits` (33..63) signed or unsigned bits.
    pub(crate) fn clamp64(&mut self, signed: bool, bits: u32) {
        let (max, min) = if signed {
            ((1i64 << (bits - 1)) - 1, -(1i64 << (bits - 1)))
        } else {
            ((1i64 << bits) - 1, 0)
        };
        // max < x: max.
        self.mov_imm(self.y, max as u32);
        self.mov_imm(self.yh, (max >> 32) as u32);
        self.dp(Cond::AL, Op::Cmp, true, 0, self.y, R0);
        self.dp(Cond::AL, Op::Sbc, true, IP, self.yh, R1);
        let c = if signed { Cond::LT } else { Cond::LO };
        self.dp(c, Op::Mov, false, R0, 0, self.y);
        self.dp(c, Op::Mov, false, R1, 0, self.yh);
        if signed {
            // x < min: min.
            self.mov_imm(self.y, min as u32);
            self.mov_imm(self.yh, (min >> 32) as u32);
            self.dp(Cond::AL, Op::Cmp, true, 0, R0, self.y);
            self.dp(Cond::AL, Op::Sbc, true, IP, R1, self.yh);
            self.dp(Cond::LT, Op::Mov, false, R0, 0, self.y);
            self.dp(Cond::LT, Op::Mov, false, R1, 0, self.yh);
        }
    }
}
