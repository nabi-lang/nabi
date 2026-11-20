//! Arithmetic, logic, unary operations and conversions.

#[allow(clippy::wildcard_imports)]
use crate::isa::wasm::*;

impl Em<'_> {
    pub(crate) fn bin(&mut self, op: Bin, k: K, signed: bool) {
        let o = match (vt(k), op) {
            (I32, Bin::Add) => Num::I32Add,
            (I32, Bin::Sub) => Num::I32Sub,
            (I32, Bin::Mul) => Num::I32Mul,
            (I32, Bin::Div) if signed => Num::I32DivS,
            (I32, Bin::Div) => Num::I32DivU,
            (I32, Bin::Rem) if signed => Num::I32RemS,
            (I32, Bin::Rem) => Num::I32RemU,
            (I32, Bin::And) => Num::I32And,
            (I32, Bin::Or) => Num::I32Or,
            (I32, Bin::Xor) => Num::I32Xor,
            (I32, Bin::Shl) => Num::I32Shl,
            (I32, Bin::Shr) if signed => Num::I32ShrS,
            (I32, Bin::Shr) => Num::I32ShrU,
            (I32, Bin::Rotl) => Num::I32Rotl,
            (I64, Bin::Add) => Num::I64Add,
            (I64, Bin::Sub) => Num::I64Sub,
            (I64, Bin::Mul) => Num::I64Mul,
            (I64, Bin::Div) if signed => Num::I64DivS,
            (I64, Bin::Div) => Num::I64DivU,
            (I64, Bin::Rem) if signed => Num::I64RemS,
            (I64, Bin::Rem) => Num::I64RemU,
            (I64, Bin::And) => Num::I64And,
            (I64, Bin::Or) => Num::I64Or,
            (I64, Bin::Xor) => Num::I64Xor,
            (I64, Bin::Shl) => Num::I64Shl,
            (I64, Bin::Shr) if signed => Num::I64ShrS,
            (I64, Bin::Shr) => Num::I64ShrU,
            (I64, Bin::Rotl) => Num::I64Rotl,
            (F32, Bin::Add) => Num::F32Add,
            (F32, Bin::Sub) => Num::F32Sub,
            (F32, Bin::Mul) => Num::F32Mul,
            (F32, Bin::Div) => Num::F32Div,
            (F64, Bin::Add) => Num::F64Add,
            (F64, Bin::Sub) => Num::F64Sub,
            (F64, Bin::Mul) => Num::F64Mul,
            (F64, Bin::Div) => Num::F64Div,
            _ => panic!("wasm32: no {op:?} on {k:?}"),
        };
        self.a.num(o);
    }

    pub(crate) fn un(&mut self, op: Un, k: K, a: V) {
        let t = vt(k);
        match op {
            Un::Neg if t == F32 => {
                self.val(a, k);
                self.a.num(Num::F32Neg);
            }
            Un::Neg if t == F64 => {
                self.val(a, k);
                self.a.num(Num::F64Neg);
            }
            Un::Neg => {
                self.imm(0, k);
                self.val(a, k);
                self.a.num(if t == I32 { Num::I32Sub } else { Num::I64Sub });
            }
            Un::Not => {
                self.val(a, k);
                self.imm(-1, if t == I32 { K::I32 } else { K::I64 });
                self.a.num(if t == I32 { Num::I32Xor } else { Num::I64Xor });
            }
            Un::Sqrt => {
                self.val(a, k);
                self.a
                    .num(if t == F32 { Num::F32Sqrt } else { Num::F64Sqrt });
            }
            Un::Popcnt => {
                self.val(a, k);
                self.a.num(if t == I32 {
                    Num::I32Popcnt
                } else {
                    Num::I64Popcnt
                });
            }
            Un::Clz => {
                self.val(a, k);
                if t == I64 {
                    self.a.num(Num::I64Clz);
                } else {
                    self.a.num(Num::I32Clz);
                    let pad = 32 - k.bits();
                    if pad > 0 {
                        self.i32c(pad as i32);
                        self.a.num(Num::I32Sub);
                    }
                }
            }
            Un::Ctz => {
                self.val(a, k);
                if t == I64 {
                    self.a.num(Num::I64Ctz);
                } else {
                    if k.bits() < 32 {
                        self.i32c(1 << k.bits());
                        self.a.num(Num::I32Or);
                    }
                    self.a.num(Num::I32Ctz);
                }
            }
            Un::Bswap => match k {
                K::I16 => {
                    let x = self.tmp(I32);
                    self.val(a, k);
                    self.set(x);
                    self.get(x);
                    self.i32c(0xff);
                    self.a.num(Num::I32And);
                    self.i32c(8);
                    self.a.num(Num::I32Shl);
                    self.get(x);
                    self.i32c(8);
                    self.a.num(Num::I32ShrU);
                    self.a.num(Num::I32Or);
                }
                K::I32 => {
                    let x = self.tmp(I32);
                    self.val(a, k);
                    self.set(x);
                    self.bswap32(x);
                }
                K::I64 => {
                    let x = self.tmp(I64);
                    let lo = self.tmp(I32);
                    self.val(a, k);
                    self.set(x);
                    self.get(x);
                    self.a.num(Num::I32WrapI64);
                    self.set(lo);
                    self.bswap32(lo);
                    self.a.num(Num::I64ExtendI32U);
                    self.i64c(32);
                    self.a.num(Num::I64Shl);
                    self.get(x);
                    self.i64c(32);
                    self.a.num(Num::I64ShrU);
                    self.a.num(Num::I32WrapI64);
                    self.set(lo);
                    self.bswap32(lo);
                    self.a.num(Num::I64ExtendI32U);
                    self.a.num(Num::I64Or);
                }
                _ => {
                    self.val(a, k);
                }
            },
        }
    }

    /// Push local `x` byte-swapped (`i32`).
    pub(crate) fn bswap32(&mut self, x: u32) {
        self.get(x);
        self.i32c(8);
        self.a.num(Num::I32Rotl);
        self.i32c(0x00ff_00ff);
        self.a.num(Num::I32And);
        self.get(x);
        self.i32c(8);
        self.a.num(Num::I32Rotr);
        self.i32c(0xff00_ff00_u32 as i32);
        self.a.num(Num::I32And);
        self.a.num(Num::I32Or);
    }

    pub(crate) fn conv(&mut self, c: Conv, from: K, to: K, a: V) {
        let (f, t) = (vt(from), vt(to));
        match c {
            // Lowered to the runtime call here (`FnLower::half_conv`).
            Conv::Half => unreachable!("no half conversion instruction"),
            Conv::Int { signed } => {
                let widen = to.size() > from.size();
                self.val_ext(a, from, signed && widen);
                match (f, t) {
                    (I64, I32) => self.a.num(Num::I32WrapI64),
                    (I32, I64) => self.a.num(if signed {
                        Num::I64ExtendI32S
                    } else {
                        Num::I64ExtendI32U
                    }),
                    _ => {}
                }
            }
            Conv::ToBool => {
                self.val(a, from);
                match f {
                    I64 => {
                        self.a.num(Num::I64Eqz);
                        self.a.num(Num::I32Eqz);
                    }
                    F32 => {
                        self.imm(0, K::F32);
                        self.a.num(Num::F32Ne);
                    }
                    F64 => {
                        self.imm(0, K::F64);
                        self.a.num(Num::F64Ne);
                    }
                    _ => {
                        self.a.num(Num::I32Eqz);
                        self.a.num(Num::I32Eqz);
                    }
                }
            }
            Conv::IToF { signed } => {
                self.val_ext(a, from, signed);
                let o = match (f, t, signed) {
                    (I32, F32, true) => Num::F32ConvertI32S,
                    (I32, F32, false) => Num::F32ConvertI32U,
                    (I64, F32, true) => Num::F32ConvertI64S,
                    (I64, F32, false) => Num::F32ConvertI64U,
                    (I32, F64, true) => Num::F64ConvertI32S,
                    (I32, F64, false) => Num::F64ConvertI32U,
                    (I64, F64, true) => Num::F64ConvertI64S,
                    _ => Num::F64ConvertI64U,
                };
                self.a.num(o);
            }
            Conv::FToI { signed, bits } => {
                self.val(a, from);
                let sat = match (t, f, signed) {
                    (I32, F32, true) => Sat::I32F32S,
                    (I32, F32, false) => Sat::I32F32U,
                    (I32, F64, true) => Sat::I32F64S,
                    (I32, F64, false) => Sat::I32F64U,
                    (I64, F32, true) => Sat::I64F32S,
                    (I64, F32, false) => Sat::I64F32U,
                    (I64, F64, true) => Sat::I64F64S,
                    _ => Sat::I64F64U,
                };
                self.a.sat(sat);
                let width = if t == I64 { 64 } else { 32 };
                if bits < width {
                    self.clamp(t, signed, bits);
                }
            }
            Conv::FToF => {
                self.val(a, from);
                match (f, t) {
                    (F64, F32) => self.a.num(Num::F32DemoteF64),
                    (F32, F64) => self.a.num(Num::F64PromoteF32),
                    _ => {}
                }
            }
            Conv::Bits => {
                self.val(a, from);
                self.cast(f, t);
            }
        }
    }

    /// Saturate the integer on top to `bits` bits.
    pub(crate) fn clamp(&mut self, t: u8, signed: bool, bits: u32) {
        let x = self.tmp(t);
        self.set(x);
        let c = |e: &mut Self, v: i64| {
            if t == I64 {
                e.i64c(v);
            } else {
                e.i32c(v as i32);
            }
        };
        let (lo, hi) = if signed {
            (-(1i64 << (bits - 1)), (1i64 << (bits - 1)) - 1)
        } else {
            (0, (1i64 << bits) - 1)
        };
        let (gt, lt) = match (t, signed) {
            (I64, true) => (Num::I64GtS, Num::I64LtS),
            (I64, false) => (Num::I64GtU, Num::I64LtU),
            (_, true) => (Num::I32GtS, Num::I32LtS),
            (_, false) => (Num::I32GtU, Num::I32LtU),
        };
        // hi if x > hi, else x.
        c(self, hi);
        self.get(x);
        self.get(x);
        c(self, hi);
        self.a.num(gt);
        self.a.select(t);
        if signed {
            self.set(x);
            c(self, lo);
            self.get(x);
            self.get(x);
            c(self, lo);
            self.a.num(lt);
            self.a.select(t);
        }
    }

    /// `dst = a op b` and the overflow flag in `self.ovf`.
    pub(crate) fn ovf(&mut self, op: Bin, k: K, signed: bool, dst: Mem, a: V, b: V) {
        let t = vt(k);
        let (la, lb, lr) = (self.tmp(t), self.tmp(t), self.tmp(t));
        self.val_ext(a, k, signed);
        self.set(la);
        self.val_ext(b, k, signed);
        self.set(lb);
        let narrow = k.bits() < 32;
        let (xor, and, lt_s, lt_u, add, sub) = if t == I64 {
            (
                Num::I64Xor,
                Num::I64And,
                Num::I64LtS,
                Num::I64LtU,
                Num::I64Add,
                Num::I64Sub,
            )
        } else {
            (
                Num::I32Xor,
                Num::I32And,
                Num::I32LtS,
                Num::I32LtU,
                Num::I32Add,
                Num::I32Sub,
            )
        };
        let zero = |e: &mut Self| {
            if t == I64 {
                e.i64c(0);
            } else {
                e.i32c(0);
            }
        };
        if op == Bin::Mul && k == K::I32 {
            let w = self.tmp(I64);
            self.get(la);
            self.a.num(if signed {
                Num::I64ExtendI32S
            } else {
                Num::I64ExtendI32U
            });
            self.get(lb);
            self.a.num(if signed {
                Num::I64ExtendI32S
            } else {
                Num::I64ExtendI32U
            });
            self.a.num(Num::I64Mul);
            self.a.local_tee(w);
            self.a.num(Num::I32WrapI64);
            self.set(lr);
            self.get(w);
            if signed {
                self.get(lr);
                self.a.num(Num::I64ExtendI32S);
                self.a.num(Num::I64Ne);
            } else {
                self.i64c(32);
                self.a.num(Num::I64ShrU);
                self.a.num(Num::I64Eqz);
                self.a.num(Num::I32Eqz);
            }
        } else {
            self.get(la);
            self.get(lb);
            self.a.num(match op {
                Bin::Add => add,
                Bin::Sub => sub,
                _ if t == I64 => Num::I64Mul,
                _ => Num::I32Mul,
            });
            self.set(lr);
            if narrow {
                // Out of range: the value does not survive its narrowing.
                self.get(lr);
                if signed {
                    self.ext(k, true);
                    self.get(lr);
                } else {
                    self.i32c(!((1i32 << k.bits()) - 1));
                    self.a.num(Num::I32And);
                    self.i32c(0);
                }
                self.a.num(Num::I32Ne);
            } else {
                match (op, signed) {
                    (Bin::Add, true) => {
                        self.get(la);
                        self.get(lr);
                        self.a.num(xor);
                        self.get(lb);
                        self.get(lr);
                        self.a.num(xor);
                        self.a.num(and);
                        zero(self);
                        self.a.num(lt_s);
                    }
                    (Bin::Add, false) => {
                        self.get(lr);
                        self.get(la);
                        self.a.num(lt_u);
                    }
                    (Bin::Sub, true) => {
                        self.get(la);
                        self.get(lb);
                        self.a.num(xor);
                        self.get(la);
                        self.get(lr);
                        self.a.num(xor);
                        self.a.num(and);
                        zero(self);
                        self.a.num(lt_s);
                    }
                    (Bin::Sub, false) => {
                        self.get(la);
                        self.get(lb);
                        self.a.num(lt_u);
                    }
                    (_, true) => {
                        // i64: a == -1 ? b == MIN : a != 0 && r / a != b.
                        self.get(la);
                        self.i64c(-1);
                        self.a.num(Num::I64Eq);
                        self.a.if_(I32);
                        self.get(lb);
                        self.i64c(i64::MIN);
                        self.a.num(Num::I64Eq);
                        self.a.else_();
                        self.div_check(la, lb, lr, Num::I64DivS);
                        self.a.end();
                    }
                    (_, false) => self.div_check(la, lb, lr, Num::I64DivU),
                }
            }
        }
        self.set(self.ovf);
        self.store_local(dst, k, lr);
    }

    /// i64 `a != 0 && r / a != b`.
    pub(crate) fn div_check(&mut self, la: u32, lb: u32, lr: u32, div: Num) {
        self.get(la);
        self.a.num(Num::I64Eqz);
        self.a.if_(I32);
        self.i32c(0);
        self.a.else_();
        self.get(lr);
        self.get(la);
        self.a.num(div);
        self.get(lb);
        self.a.num(Num::I64Ne);
        self.a.end();
    }
}
