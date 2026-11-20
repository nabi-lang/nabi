//! Integer arithmetic, compares and operators across lanes.

#[allow(clippy::wildcard_imports)]
use crate::lower::vector::*;

impl Builder<'_, '_, '_> {
    /// `a op b` on integers, `checked` for overflow.
    pub(crate) fn int_arith(
        &mut self,
        op: PrimOp,
        p: PrimTy,
        a: VReg,
        b: VReg,
        checked: bool,
    ) -> Result<VReg, No> {
        let vop = match op {
            PrimOp::Add | PrimOp::WrappingAdd => VBin::Add,
            PrimOp::Sub | PrimOp::WrappingSub => VBin::Sub,
            PrimOp::Mul | PrimOp::WrappingMul => VBin::Mul,
            _ => return Err(()),
        };
        let r = self.bin(vop, a, b)?;
        if !checked {
            return Ok(r);
        }
        // The lanes that overflowed, as sign bits.
        let ovf = match (vop, p.is_signed_int()) {
            (VBin::Mul, signed) => {
                // The high half of the product: the low half's sign
                // extended (zero) when it fits.
                let (hi, fit) = if signed {
                    (VBin::MulHiS, self.sign_mask(r)?)
                } else {
                    (VBin::MulHiU, self.konst(0)?)
                };
                let h = self.bin(hi, a, b)?;
                let e = self.bin(VBin::Eq, h, fit)?;
                self.not(e)?
            }
            (VBin::Add, true) => {
                let x = self.bin(VBin::Xor, a, r)?;
                let y = self.bin(VBin::Xor, b, r)?;
                self.bin(VBin::And, x, y)?
            }
            (_, true) => {
                let x = self.bin(VBin::Xor, a, b)?;
                let y = self.bin(VBin::Xor, a, r)?;
                self.bin(VBin::And, x, y)?
            }
            (VBin::Add, false) => self.carry(a, b, r)?,
            (_, false) => {
                // Borrow: (!a & b) | (!(a ^ b) & r).
                let x = self.bin(VBin::AndNot, b, a)?;
                let e = self.bin(VBin::Xor, a, b)?;
                let y = self.bin(VBin::AndNot, r, e)?;
                self.bin(VBin::Or, x, y)?
            }
        };
        self.bail(ovf)?;
        Ok(r)
    }

    /// The carry out of `r = a + b` per lane, as sign bits: `(a & b) |
    /// ((a | b) & !r)`.
    pub(crate) fn carry(&mut self, a: VReg, b: VReg, r: VReg) -> Result<VReg, No> {
        let x = self.bin(VBin::And, a, b)?;
        let o = self.bin(VBin::Or, a, b)?;
        let y = self.bin(VBin::AndNot, o, r)?;
        self.bin(VBin::Or, x, y)
    }

    pub(crate) fn cmp(&mut self, op: PrimOp, p: PrimTy, a: VReg, b: VReg) -> Result<VReg, No> {
        if p.is_float() {
            let (o, x, y, not) = match op {
                PrimOp::Eq => (VBin::FEq, a, b, false),
                PrimOp::Ne => (VBin::FNe, a, b, false),
                PrimOp::Lt => (VBin::FLt, a, b, false),
                PrimOp::Le => (VBin::FLe, a, b, false),
                PrimOp::Gt => (VBin::FLt, b, a, false),
                PrimOp::Ge => (VBin::FLe, b, a, false),
                _ => return Err(()),
            };
            debug_assert!(!not);
            return self.bin(o, x, y);
        }
        let signed = p.is_signed_int();
        match op {
            PrimOp::Eq => self.bin(VBin::Eq, a, b),
            PrimOp::Ne => {
                let e = self.bin(VBin::Eq, a, b)?;
                self.not(e)
            }
            PrimOp::Gt => self.gt(a, b, signed),
            PrimOp::Lt => self.gt(b, a, signed),
            PrimOp::Le => {
                let g = self.gt(a, b, signed)?;
                self.not(g)
            }
            PrimOp::Ge => {
                let g = self.gt(b, a, signed)?;
                self.not(g)
            }
            _ => Err(()),
        }
    }

    pub(crate) fn binop(
        &mut self,
        op: PrimOp,
        p: PrimTy,
        lhs: &Operand,
        rhs: &Operand,
    ) -> Result<(VReg, Form), No> {
        let ty = MirTy::Prim(p);
        if p == PrimTy::Bool {
            let a = self.val(lhs, &ty)?;
            let b = self.val(rhs, &ty)?;
            let vop = match op {
                PrimOp::BitAnd | PrimOp::LogicalAnd => VBin::And,
                PrimOp::BitOr | PrimOp::LogicalOr => VBin::Or,
                PrimOp::BitXor | PrimOp::Ne => VBin::Xor,
                PrimOp::Eq => {
                    let (x, y) = (self.mask(a)?, self.mask(b)?);
                    let d = self.bin(VBin::Xor, x, y)?;
                    return Ok((self.not(d)?, Form::Mask));
                }
                _ => return Err(()),
            };
            if a.1 == Form::Bit && b.1 == Form::Bit {
                return Ok((self.bin(vop, a.0, b.0)?, Form::Bit));
            }
            let (x, y) = (self.mask(a)?, self.mask(b)?);
            return Ok((self.bin(vop, x, y)?, Form::Mask));
        }
        if !(p.is_int() || p.is_float()) {
            return Err(());
        }
        let (a, fa) = self.val(lhs, &ty)?;
        if matches!(op, PrimOp::Shl | PrimOp::Shr) {
            let bits = self.w * 8;
            let n = match rhs {
                Operand::Const(c) => match c {
                    ConstVal::Int { value, .. } if (0..i128::from(bits)).contains(value) => {
                        *value as u8
                    }
                    ConstVal::UInt { value, .. } if *value < u128::from(bits) => *value as u8,
                    _ => return Err(()),
                },
                Operand::Copy(_) => return Err(()),
            };
            let sop = match op {
                PrimOp::Shl => VShift::Shl,
                _ if p.is_signed_int() => VShift::ShrS,
                _ => VShift::ShrU,
            };
            if fa != Form::Num || !p.is_int() {
                return Err(());
            }
            if sop == VShift::ShrS && u32::from(n) == bits - 1 {
                return Ok((self.sign_mask(a)?, Form::Num));
            }
            if !self.isa.shift(sop, self.w) {
                return Err(());
            }
            let dst = self.reg()?;
            self.emit(VOp::Shift { op: sop, dst, a, n });
            return Ok((dst, Form::Num));
        }
        let (b, fb) = self.val(rhs, &ty)?;
        if fa != Form::Num || fb != Form::Num {
            return Err(());
        }
        if matches!(
            op,
            PrimOp::Eq | PrimOp::Ne | PrimOp::Lt | PrimOp::Le | PrimOp::Gt | PrimOp::Ge
        ) {
            return Ok((self.cmp(op, p, a, b)?, Form::Mask));
        }
        if p.is_float() {
            let vop = match op {
                PrimOp::Add => VBin::FAdd,
                PrimOp::Sub => VBin::FSub,
                PrimOp::Mul => VBin::FMul,
                PrimOp::Div => VBin::FDiv,
                _ => return Err(()),
            };
            return Ok((self.bin(vop, a, b)?, Form::Num));
        }
        let r = match op {
            PrimOp::BitAnd => self.bin(VBin::And, a, b)?,
            PrimOp::BitOr => self.bin(VBin::Or, a, b)?,
            PrimOp::BitXor => self.bin(VBin::Xor, a, b)?,
            PrimOp::Add | PrimOp::Sub | PrimOp::Mul => {
                let checked = self.checked(op, p, lhs, rhs);
                self.int_arith(op, p, a, b, checked)?
            }
            PrimOp::WrappingAdd | PrimOp::WrappingSub | PrimOp::WrappingMul => {
                self.int_arith(op, p, a, b, false)?
            }
            _ => return Err(()),
        };
        Ok((r, Form::Num))
    }

    /// Whether integer `lhs op rhs` traps on overflow (`bin_op`'s test).
    pub(crate) fn checked(&self, op: PrimOp, p: PrimTy, lhs: &Operand, rhs: &Operand) -> bool {
        let bits = p.bit_width(self.lw.c.t.pointer_width);
        self.checks
            && (p.is_signed_int() || p.is_unsigned_int())
            && !nabic_codegen::analysis::cannot_overflow(
                op,
                p,
                bits,
                (
                    nabic_codegen::analysis::const_int(lhs),
                    nabic_codegen::analysis::const_int(rhs),
                ),
            )
    }

    pub(crate) fn unop(&mut self, op: PrimOp, p: PrimTy, a: &Operand) -> Result<(VReg, Form), No> {
        let ty = MirTy::Prim(p);
        let v = self.val(a, &ty)?;
        match (op, p) {
            (PrimOp::LogicalNot | PrimOp::Not, PrimTy::Bool) => match v.1 {
                Form::Bit => {
                    let one = self.konst(1)?;
                    Ok((self.bin(VBin::Xor, v.0, one)?, Form::Bit))
                }
                _ => Ok((self.not(v.0)?, Form::Mask)),
            },
            (PrimOp::Not, p) if p.is_int() => Ok((self.not(v.0)?, Form::Num)),
            (PrimOp::Neg, p) if p.is_float() => {
                let s = self.konst(self.sign())?;
                Ok((self.bin(VBin::Xor, v.0, s)?, Form::Num))
            }
            (PrimOp::Neg, p) if p.is_signed_int() => {
                let z = self.konst(0)?;
                if self.checks && !matches!(a, Operand::Const(_)) {
                    // Only `MIN` overflows.
                    let m = self.konst(self.sign())?;
                    let e = self.bin(VBin::Eq, v.0, m)?;
                    self.bail(e)?;
                }
                Ok((self.bin(VBin::Sub, z, v.0)?, Form::Num))
            }
            (PrimOp::Sqrt, p) if p.is_float() && self.isa != Isa::Arm => {
                let dst = self.reg()?;
                self.emit(VOp::Un {
                    op: VUn::FSqrt,
                    dst,
                    a: v.0,
                });
                Ok((dst, Form::Num))
            }
            _ => Err(()),
        }
    }
}
