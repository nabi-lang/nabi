//! Binary and unary operators and rotates.

#[allow(clippy::wildcard_imports)]
use crate::lower::*;

impl FnLower<'_, '_> {
    pub(crate) fn bin_op(
        &mut self,
        op: PrimOp,
        op_ty: &MirTy,
        lhs: &Operand,
        rhs: &Operand,
        dst: Mem,
    ) {
        let pw = self.pw();
        let MirTy::Prim(prim) = op_ty else {
            self.err(format!("binary `{op:?}` on `{op_ty:?}`"));
            return;
        };
        let prim = *prim;
        if prim == PrimTy::Str && matches!(op, PrimOp::Eq | PrimOp::Ne) {
            let a = self.val(lhs, op_ty);
            let b = self.val(rhs, op_ty);
            let r = self.tmp_k(K::I32);
            let sym = self.c.sym("nabi_str_cmp");
            self.call_runtime(
                sym,
                &[op_ty.clone(), op_ty.clone()],
                &MirTy::Prim(PrimTy::I32),
                &[a, b],
                Some(r),
            );
            let cc = if op == PrimOp::Eq { Cc::Eq } else { Cc::Ne };
            self.push(Inst::Cmp {
                cc,
                k: K::I32,
                signed: true,
                dst,
                a: V::M(r),
                b: V::I(0),
            });
            return;
        }
        let (a, k) = self.scalar(lhs, op_ty);
        let (b, _) = self.scalar(rhs, op_ty);
        let signed = prim.is_signed_int();
        let int = prim.is_int();
        let float = prim.is_float();
        let boolean = prim == PrimTy::Bool;
        let bits = prim.bit_width(pw);
        // Checks that must panic rather than hit undefined behaviour.
        if int {
            let rc = const_int(rhs);
            match op {
                PrimOp::Div | PrimOp::Rem => {
                    let (zero_msg, ovf_msg) = if op == PrimOp::Div {
                        (
                            "attempt to divide by zero",
                            "attempt to divide with overflow",
                        )
                    } else {
                        (
                            "attempt to calculate the remainder with a divisor of zero",
                            "attempt to calculate the remainder with overflow",
                        )
                    };
                    if rc.is_none_or(|c| c == 0) {
                        let fail = self.cmp(Cc::Eq, k, signed, b, V::I(0));
                        self.check(fail, zero_msg);
                    }
                    if signed && rc.is_none_or(|c| c == -1) {
                        let min = -(1i128 << (bits - 1));
                        let is_min = self.cmp(Cc::Eq, k, true, a, V::I(min as i64));
                        let is_neg1 = self.cmp(Cc::Eq, k, true, b, V::I(-1));
                        let both = self.tmp_k(K::I8);
                        self.push(Inst::Bin {
                            op: Bin::And,
                            k: K::I8,
                            signed: false,
                            dst: both,
                            a: is_min,
                            b: is_neg1,
                        });
                        self.check(V::M(both), ovf_msg);
                    }
                }
                PrimOp::Shl | PrimOp::Shr => {
                    // A constant, or a mask, below the width.
                    let below = match rhs {
                        Operand::Copy(l) => self.masked.get(l).copied(),
                        Operand::Const(_) => rc,
                    }
                    .is_some_and(|c| (0..i128::from(bits)).contains(&c));
                    if !below {
                        let fail = self.cmp(Cc::Ge, k, false, b, V::I(i64::from(bits)));
                        let msg = if op == PrimOp::Shl {
                            "attempt to shift left with overflow"
                        } else {
                            "attempt to shift right with overflow"
                        };
                        self.check(fail, msg);
                    }
                }
                _ => {}
            }
        }
        if op == PrimOp::RotateLeft && int {
            self.rotl(k, dst, a, b);
            return;
        }
        let bin = match op {
            PrimOp::Add | PrimOp::WrappingAdd if int || float => Some(Bin::Add),
            PrimOp::Sub | PrimOp::WrappingSub if int || float => Some(Bin::Sub),
            PrimOp::Mul | PrimOp::WrappingMul if int || float => Some(Bin::Mul),
            PrimOp::Div if int || float => Some(Bin::Div),
            PrimOp::Rem if int => Some(Bin::Rem),
            PrimOp::BitAnd | PrimOp::LogicalAnd if int || boolean => Some(Bin::And),
            PrimOp::BitOr | PrimOp::LogicalOr if int || boolean => Some(Bin::Or),
            PrimOp::BitXor if int || boolean => Some(Bin::Xor),
            PrimOp::Shl if int => Some(Bin::Shl),
            PrimOp::Shr if int => Some(Bin::Shr),
            _ => None,
        };
        if op == PrimOp::Rem && float {
            // 32-bit Windows' UCRT has no `fmodf`: in `f64`, as clang does.
            let t = self.c.t;
            let narrow = prim == PrimTy::F32 && t.arch == "x86" && t.os == "windows";
            let (prim, op_ty, a, b, out) = if narrow {
                let [wa, wb, wd] = [(); 3].map(|()| self.tmp_k(K::F64));
                for (w, v) in [(wa, a), (wb, b)] {
                    self.push(Inst::Conv {
                        conv: Conv::FToF,
                        from: K::F32,
                        to: K::F64,
                        dst: w,
                        a: v,
                    });
                }
                (
                    PrimTy::F64,
                    &MirTy::Prim(PrimTy::F64),
                    V::M(wa),
                    V::M(wb),
                    wd,
                )
            } else {
                (prim, op_ty, a, b, dst)
            };
            // A float `%` is C's `fmod`.
            let name = if prim == PrimTy::F32 { "fmodf" } else { "fmod" };
            let sym = self.c.sym(name);
            let shape = cc::shape(op_ty, self.c.t);
            let locs = cc::assign(
                self.c.conv,
                &[shape.clone(), shape.clone()],
                Some(&shape),
                false,
                None,
            );
            let r = locs.ret.expect("float result in a register");
            self.push(Inst::Call {
                callee: Callee::Sym(sym),
                args: vec![(locs.params[0][0], a), (locs.params[1][0], b)],
                rets: vec![(r[0], out)],
                sret: None,
                variadic: false,
                pop: 0,
            });
            if narrow {
                self.push(Inst::Conv {
                    conv: Conv::FToF,
                    from: K::F64,
                    to: K::F32,
                    dst,
                    a: V::M(out),
                });
            }
            return;
        }
        if let Some(bop) = bin {
            let consts = (const_int(lhs), const_int(rhs));
            let checked = self.c.overflow_checks
                && int
                && matches!(op, PrimOp::Add | PrimOp::Sub | PrimOp::Mul)
                && (signed || prim.is_unsigned_int())
                && !cannot_overflow(op, prim, bits, consts);
            if checked {
                let msg = match op {
                    PrimOp::Add => "attempt to add with overflow",
                    PrimOp::Sub => "attempt to subtract with overflow",
                    _ => "attempt to multiply with overflow",
                };
                let fail = self.panic_label(msg);
                self.push(Inst::Ovf {
                    op: bop,
                    k,
                    signed,
                    dst,
                    a,
                    b,
                    fail,
                });
            } else if !(matches!(bop, Bin::Div | Bin::Rem)
                && int
                && consts
                    .1
                    .is_some_and(|d| self.div_const(bop, k, signed, bits, dst, a, d)))
            {
                self.push(Inst::Bin {
                    op: bop,
                    k,
                    signed,
                    dst,
                    a,
                    b,
                });
            }
            return;
        }
        let cc = match op {
            PrimOp::Eq => Some(Cc::Eq),
            PrimOp::Ne => Some(Cc::Ne),
            PrimOp::Lt if !boolean => Some(Cc::Lt),
            PrimOp::Le if !boolean => Some(Cc::Le),
            PrimOp::Gt if !boolean => Some(Cc::Gt),
            PrimOp::Ge if !boolean => Some(Cc::Ge),
            _ => None,
        };
        match cc {
            Some(cc) if int || float || boolean => self.push(Inst::Cmp {
                cc,
                k,
                signed,
                dst,
                a,
                b,
            }),
            _ => self.err(format!("binary `{op:?}` on `{prim:?}`")),
        }
    }

    /// Whether the target rotates a `k` in one instruction.
    pub(crate) fn rotates(&self, k: K) -> bool {
        let zbb = self.c.obj.zbb;
        matches!(k, K::I32 | K::I64)
            && match self.c.t.arch.as_str() {
                "x86_64" | "aarch64" | "powerpc64" | "s390x" | "wasm32" => true,
                "x86" | "arm" => k == K::I32,
                "riscv64" => zbb,
                "riscv32" => zbb && k == K::I32,
                _ => false,
            }
    }

    /// `dst = a` rotated left by `b` modulo the width: one `Rotl`, or
    /// two shifts where the target has no rotate.
    pub(crate) fn rotl(&mut self, k: K, dst: Mem, a: V, b: V) {
        let bits = i64::from(k.bits());
        let b = match b {
            V::I(c) => V::I(c.rem_euclid(bits)),
            v @ V::M(_) => v,
        };
        if b == V::I(0) {
            self.push(Inst::Mov { k, dst, src: a });
            return;
        }
        if self.rotates(k) {
            self.push(Inst::Bin {
                op: Bin::Rotl,
                k,
                signed: false,
                dst,
                a,
                b,
            });
            return;
        }
        let bin = |s: &mut Self, op, x: V, y: V| {
            let d = s.tmp_k(k);
            s.push(Inst::Bin {
                op,
                k,
                signed: false,
                dst: d,
                a: x,
                b: y,
            });
            V::M(d)
        };
        // `n & (w - 1)` and `-n & (w - 1)`: both shifts below the width,
        // and `x | x` at 0.
        let (l, r) = match b {
            V::I(c) => (V::I(c), V::I(bits - c)),
            n @ V::M(_) => {
                let neg = bin(self, Bin::Sub, V::I(0), n);
                (
                    bin(self, Bin::And, n, V::I(bits - 1)),
                    bin(self, Bin::And, neg, V::I(bits - 1)),
                )
            }
        };
        let hi = bin(self, Bin::Shl, a, l);
        let lo = bin(self, Bin::Shr, a, r);
        self.push(Inst::Bin {
            op: Bin::Or,
            k,
            signed: false,
            dst,
            a: hi,
            b: lo,
        });
    }

    pub(crate) fn un_op(&mut self, op: PrimOp, op_ty: &MirTy, operand: &Operand, dst: Mem) {
        let pw = self.pw();
        let MirTy::Prim(p) = op_ty else {
            self.err(format!("unary `{op:?}` on `{op_ty:?}`"));
            return;
        };
        let p = *p;
        let (a, k) = self.scalar(operand, op_ty);
        let un = |s: &mut Self, op| s.push(Inst::Un { op, k, dst, a });
        match op {
            PrimOp::Neg if p.is_signed_int() => {
                let bits = p.bit_width(pw);
                let min = -(1i128 << (bits - 1));
                if self.c.overflow_checks && const_int(operand).is_none_or(|c| c == min) {
                    let fail = self.cmp(Cc::Eq, k, true, a, V::I(min as i64));
                    self.check(fail, "attempt to negate with overflow");
                }
                un(self, Un::Neg);
            }
            PrimOp::Neg if p.is_float() => un(self, Un::Neg),
            PrimOp::Sqrt if p.is_float() => un(self, Un::Sqrt),
            PrimOp::CountOnes if p.is_int() => un(self, Un::Popcnt),
            PrimOp::SwapBytes if p.is_int() => un(self, Un::Bswap),
            PrimOp::LeadingZeros if p.is_int() => un(self, Un::Clz),
            PrimOp::TrailingZeros if p.is_int() => un(self, Un::Ctz),
            PrimOp::Not if p.is_int() => un(self, Un::Not),
            PrimOp::LogicalNot if p == PrimTy::Bool => self.push(Inst::Bin {
                op: Bin::Xor,
                k: K::I8,
                signed: false,
                dst,
                a,
                b: V::I(1),
            }),
            _ => self.err(format!("unary `{op:?}` on `{p:?}`")),
        }
    }
}

impl FnLower<'_, '_> {
    /// Whether the target fuses a multiply and an add of `k` in one
    /// instruction.
    pub(crate) fn fuses(&self, k: K) -> bool {
        let t = self.c.t;
        match t.arch.as_str() {
            "aarch64" | "powerpc64" | "s390x" => true,
            "x86_64" => t.cpu_features().has("fma"),
            "riscv64" => t.cpu_features().has(if k == K::F64 { "d" } else { "f" }),
            "arm" => t.cpu_features().has("vfp4"),
            _ => false,
        }
    }

    /// A call of the runtime's `nabi_f64_mul_add` / `nabi_f32_mul_add`
    /// above `-O0` as the CPU's fused multiply-add, where it has one.
    pub(crate) fn fma(&mut self, sym: SymId, args: &[Operand], dst: Option<Mem>) -> bool {
        let (k, p) = match self.c.obj.syms[sym as usize].name.as_str() {
            "nabi_f64_mul_add" => (K::F64, PrimTy::F64),
            "nabi_f32_mul_add" => (K::F32, PrimTy::F32),
            _ => return false,
        };
        if !self.c.opt || args.len() != 3 || !self.fuses(k) {
            return false;
        }
        let Some(dst) = dst else {
            return true;
        };
        let ty = MirTy::Prim(p);
        let (Val::S(a, _), Val::S(b, _), Val::S(c, _)) = (
            self.val(&args[0], &ty),
            self.val(&args[1], &ty),
            self.val(&args[2], &ty),
        ) else {
            return false;
        };
        self.push(Inst::Fma { k, dst, a, b, c });
        true
    }

    /// A call of the runtime's `nabi_f16_from_f32` / `nabi_f16_to_f32`
    /// as the CPU's conversion: x86-64 with F16C, arm64 (base `fcvt`).
    pub(crate) fn half_conv(&mut self, sym: SymId, args: &[Operand], dst: Option<Mem>) -> bool {
        let name = self.c.obj.syms[sym as usize].name.as_str();
        let (from, to, from_ty) = match name {
            "nabi_f16_from_f32" => (K::F32, K::I16, PrimTy::F32),
            "nabi_f16_to_f32" => (K::I16, K::F32, PrimTy::U16),
            _ => return false,
        };
        let t = self.c.t;
        if !(t.arch == "aarch64" || self.c.obj.f16c) || args.len() != 1 {
            return false;
        }
        let Some(dst) = dst else {
            return true;
        };
        let Val::S(a, _) = self.val(&args[0], &MirTy::Prim(from_ty)) else {
            return false;
        };
        self.push(Inst::Conv {
            conv: Conv::Half,
            from,
            to,
            dst,
            a,
        });
        true
    }
}
