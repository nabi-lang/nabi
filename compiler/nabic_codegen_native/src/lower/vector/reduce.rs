//! Reductions, counts, dot products and side exits.

#[allow(clippy::wildcard_imports)]
use crate::lower::vector::*;

impl Builder<'_, '_, '_> {
    /// `acc = acc op x` for reduction `acc` (a `p`), `x` the other
    /// operand: one of its updates, done at the end of the iteration.
    pub(crate) fn reduce(
        &mut self,
        acc: LocalId,
        op: PrimOp,
        p: PrimTy,
        x: &Operand,
    ) -> Result<(), No> {
        if !(p.is_signed_int() || p.is_unsigned_int()) || self.accs.iter().any(|a| a.local == acc) {
            return Err(());
        }
        let ty = MirTy::Prim(p);
        let c = nabic_codegen::analysis::const_int(x);
        let (x, f) = self.val(x, &ty)?;
        if f != Form::Num {
            return Err(());
        }
        let up = (op, x, c, self.pred);
        match self.ups.iter_mut().find(|u| u.0 == acc) {
            Some(u) => u.2.push(up),
            None => self.ups.push((acc, p, vec![up])),
        }
        Ok(())
    }

    /// The reductions' updates, merged into one value per iteration, and
    /// their accumulators.
    #[allow(clippy::too_many_lines)]
    pub(crate) fn finish(&mut self) -> Result<(), No> {
        if !self.pending.is_empty() {
            return Err(());
        }
        for (m, max, signed, r) in self.minmax.clone() {
            let v = self.vals.get(&m).ok_or(())?;
            if v.1 != Form::Num {
                return Err(());
            }
            let v = v.0;
            if v != r {
                self.tail.push(VOp::Bin {
                    op: VBin::Or,
                    dst: r,
                    a: v,
                    b: v,
                });
            }
            let float = self.fmm.contains(&m);
            let k = match (float, self.w) {
                (true, 8) => K::F64,
                (true, _) => K::F32,
                _ => self.lanes_k(),
            };
            self.accs.push(Acc {
                local: m,
                k,
                lane: k,
                red: Red::MinMax { max, signed },
                regs: vec![r],
            });
        }
        for (acc, p, ups) in std::mem::take(&mut self.ups) {
            let kind = |op: PrimOp| match op {
                PrimOp::Add | PrimOp::WrappingAdd => 0,
                PrimOp::Sub | PrimOp::WrappingSub => 1,
                PrimOp::BitAnd => 2,
                PrimOp::BitOr => 3,
                PrimOp::BitXor => 4,
                _ => 5,
            };
            let me = Operand::Copy(acc);
            let unit = Operand::Const(ConstVal::Unit);
            let checked = |s: &Self, op: PrimOp| {
                matches!(op, PrimOp::Add | PrimOp::Sub) && s.checked(op, p, &me, &unit)
            };
            let first = ups[0].0;
            let bit = kind(first) >= 2;
            if kind(first) == 5
                || ups
                    .iter()
                    .any(|u| (kind(u.0) >= 2 || bit) && kind(u.0) != kind(first))
            {
                return Err(());
            }
            let any_checked = ups.iter().any(|u| checked(self, u.0));
            if any_checked && ups.iter().any(|u| !checked(self, u.0)) {
                return Err(());
            }
            let all_sub = ups.iter().all(|u| kind(u.0) == 1);
            // A checked sum bounds the partial sums of the scalar loop by
            // those of its parts: one update on every way through.
            if any_checked && ups.len() > 1 {
                let blocks: Vec<usize> = ups
                    .iter()
                    .map(|u| self.pred_block(u.3))
                    .collect::<Option<_>>()
                    .ok_or(())?;
                for (i, a) in blocks.iter().enumerate() {
                    for b in &blocks[i + 1..] {
                        if a == b || self.after(*a, *b) || self.after(*b, *a) {
                            return Err(());
                        }
                    }
                }
            }
            // What is added (taken off, for `all_sub`) this iteration.
            let mut delta: Option<VReg> = None;
            for (op, x, c, m) in ups.iter().copied() {
                let x = if kind(op) == 1 && !all_sub {
                    if any_checked {
                        // `acc - c` as `acc + (-c)`: a constant, not MIN.
                        let c = c.ok_or(())?;
                        let bits = p.bit_width(self.lw.c.t.pointer_width);
                        if !p.is_signed_int() || c == -(1i128 << (bits - 1)) {
                            return Err(());
                        }
                        self.konst((-c) as u64)?
                    } else {
                        let z = self.konst(0)?;
                        self.bin(VBin::Sub, z, x)?
                    }
                } else {
                    x
                };
                // Lanes that skip the update add what changes nothing.
                let x = match (m, op) {
                    (None, _) => x,
                    (Some(m), PrimOp::BitAnd) => {
                        let n = self.not(m)?;
                        self.bin(VBin::Or, x, n)?
                    }
                    (Some(m), _) => self.bin(VBin::And, x, m)?,
                };
                let vop = match op {
                    PrimOp::BitAnd => VBin::And,
                    PrimOp::BitOr => VBin::Or,
                    PrimOp::BitXor => VBin::Xor,
                    _ => VBin::Add,
                };
                delta = Some(match delta {
                    None => x,
                    Some(d) => self.bin(vop, d, x)?,
                });
            }
            let x = delta.ok_or(())?;
            let red = match first {
                PrimOp::BitAnd => Red::Wrap(Bin::And),
                PrimOp::BitOr => Red::Wrap(Bin::Or),
                PrimOp::BitXor => Red::Wrap(Bin::Xor),
                _ if any_checked => Red::Checked {
                    sub: all_sub,
                    signed: p.is_signed_int(),
                },
                _ if all_sub => Red::WrapSub,
                _ => Red::Wrap(Bin::Add),
            };
            self.reduction(acc, red, x)?;
        }
        Ok(())
    }

    /// The block whose lanes mask `m` is: one the builder started with
    /// it.
    pub(crate) fn pred_block(&self, m: Option<VReg>) -> Option<usize> {
        let m = m?;
        self.started
            .iter()
            .find(|(_, p)| *p == Some(m))
            .map(|b| b.0)
    }

    /// The accumulator of reduction `red` of `acc`, adding `x` each
    /// iteration.
    pub(crate) fn reduction(&mut self, acc: LocalId, red: Red, x: VReg) -> Result<(), No> {
        let k = self.lanes_k();
        let acc_reg = |s: &mut Self, init: u64| -> Result<VReg, No> {
            let dst = s.reg()?;
            s.pre.push(VOp::Const { dst, bits: init });
            s.pinned.push(dst);
            Ok(dst)
        };
        let tail = |s: &mut Self, op, dst, a, b| s.tail.push(VOp::Bin { op, dst, a, b });
        let regs = match red {
            Red::Wrap(b) => {
                let (vop, init) = match b {
                    Bin::And => (VBin::And, u64::MAX),
                    Bin::Or => (VBin::Or, 0),
                    Bin::Xor => (VBin::Xor, 0),
                    _ => (VBin::Add, 0),
                };
                let a = acc_reg(self, init)?;
                tail(self, vop, a, a, x);
                vec![a]
            }
            Red::WrapSub => {
                let a = acc_reg(self, 0)?;
                tail(self, VBin::Add, a, a, x);
                vec![a]
            }
            Red::Checked { signed: true, .. } => {
                // The non-negative elements summed, and the negative ones
                // as `!x` (`-x - 1`, never negative) with their count:
                // sums of non-negatives overflow into the sign bit. The
                // count fits a lane: an entry runs `MAX_TRIPS` iterations
                // at most (a short loop's fewer).
                let pos = acc_reg(self, 0)?;
                let neg = acc_reg(self, 0)?;
                let cnt = acc_reg(self, 0)?;
                let ovf = acc_reg(self, 0)?;
                let (m, p, q) = (self.reg()?, self.reg()?, self.reg()?);
                self.tail.push(VOp::Un {
                    op: VUn::SignMask,
                    dst: m,
                    a: x,
                });
                tail(self, VBin::AndNot, p, x, m);
                tail(self, VBin::AndNot, q, m, x);
                tail(self, VBin::Add, pos, pos, p);
                tail(self, VBin::Or, ovf, ovf, pos);
                tail(self, VBin::Add, neg, neg, q);
                tail(self, VBin::Or, ovf, ovf, neg);
                tail(self, VBin::Add, cnt, cnt, m);
                vec![pos, neg, cnt, ovf]
            }
            Red::Count { .. } | Red::MinMax { .. } | Red::Dot { .. } => return Err(()),
            Red::Checked { signed: false, .. } => {
                let pos = acc_reg(self, 0)?;
                let ovf = acc_reg(self, 0)?;
                let (r, x1, o, y) = (self.reg()?, self.reg()?, self.reg()?, self.reg()?);
                tail(self, VBin::Add, r, pos, x);
                tail(self, VBin::And, x1, pos, x);
                tail(self, VBin::Or, o, pos, x);
                tail(self, VBin::AndNot, y, o, r);
                tail(self, VBin::Or, x1, x1, y);
                tail(self, VBin::Or, ovf, ovf, x1);
                tail(self, VBin::Or, pos, r, r);
                vec![pos, ovf]
            }
        };
        self.accs.push(Acc {
            local: acc,
            k,
            lane: k,
            red,
            regs,
        });
        Ok(())
    }

    /// `acc = acc + a * b` (a `p`), `a` `b` lanes of bytes widened
    /// (`signed`): four products into each 4-byte lane; `checked`, with
    /// the sum of the magnitudes as the partial sums' bound.
    pub(crate) fn dot_acc(
        &mut self,
        acc: LocalId,
        p: PrimTy,
        (a, b, signed): (VReg, VReg, bool),
        checked: bool,
    ) -> Result<(), No> {
        if self.pred.is_some() || self.accs.iter().any(|x| x.local == acc) {
            return Err(());
        }
        let zero = |s: &mut Self| -> Result<VReg, No> {
            let dst = s.reg()?;
            s.pre.push(VOp::Const { dst, bits: 0 });
            s.pinned.push(dst);
            Ok(dst)
        };
        let d = zero(self)?;
        self.tail.push(VOp::Dot {
            acc: d,
            a,
            b,
            signed,
        });
        let mut regs = vec![d];
        if checked && signed {
            let m = zero(self)?;
            let (x, y) = (self.reg()?, self.reg()?);
            self.tail.push(VOp::Un {
                op: VUn::Abs,
                dst: x,
                a,
            });
            self.tail.push(VOp::Un {
                op: VUn::Abs,
                dst: y,
                a: b,
            });
            self.tail.push(VOp::Dot {
                acc: m,
                a: x,
                b: y,
                signed: false,
            });
            regs.push(m);
        }
        let k = crate::cc::prim_k(p, self.lw.c.t.pointer_width);
        if k != K::I32 {
            return Err(());
        }
        self.accs.push(Acc {
            local: acc,
            k,
            lane: k,
            red: Red::Dot {
                checked,
                signed,
                acc_signed: p.is_signed_int(),
            },
            regs,
        });
        Ok(())
    }

    /// `acc = acc + 1` (a `p`) in the lanes of mask `m` (and the block's).
    pub(crate) fn count(
        &mut self,
        acc: LocalId,
        p: PrimTy,
        m: VReg,
        checked: bool,
    ) -> Result<(), No> {
        if !(p.is_signed_int() || p.is_unsigned_int())
            || self.accs.iter().any(|a| a.local == acc)
            || self.ups.iter().any(|u| u.0 == acc)
        {
            return Err(());
        }
        let m = match self.pred {
            Some(q) => self.bin(VBin::And, m, q)?,
            None => m,
        };
        // A lane counts no more than an entry's iterations, `MAX_TRIPS`
        // at most.
        let dst = self.reg()?;
        self.pre.push(VOp::Const { dst, bits: 0 });
        self.pinned.push(dst);
        self.tail.push(VOp::Bin {
            op: VBin::Sub,
            dst,
            a: dst,
            b: m,
        });
        let pw = self.lw.c.t.pointer_width;
        self.accs.push(Acc {
            local: acc,
            k: crate::cc::prim_k(p, pw),
            lane: self.lanes_k(),
            red: Red::Count {
                checked,
                signed: p.is_signed_int(),
            },
            regs: vec![dst],
        });
        Ok(())
    }

    /// A side exit, taken where `c` is `on_true`: no vector goes past
    /// a lane that takes it.
    pub(crate) fn exit(&mut self, c: LocalId, on_true: bool) -> Result<(), No> {
        let v = self.val(&Operand::Copy(c), &MirTy::Prim(PrimTy::Bool))?;
        let m = self.mask(v)?;
        let m = if on_true { m } else { self.not(m)? };
        self.bail(m)
    }

    /// Whether `o` is a float min / max accumulator.
    pub(crate) fn facc(&self, o: &Operand) -> bool {
        matches!(o, Operand::Copy(l) if self.fmm.contains(l))
    }

    /// Bail out where a lane of `o` (a `p`), an element a float minimum
    /// or maximum takes, is a NaN or a zero: lanes would order a NaN, and
    /// zeros of both signs, as the scalar loop does not.
    pub(crate) fn float_wild(&mut self, o: &Operand, p: PrimTy) -> Result<(), No> {
        let (x, f) = self.val(o, &MirTy::Prim(p))?;
        if f != Form::Num {
            return Err(());
        }
        let z = self.konst(0)?;
        let n = self.bin(VBin::FNe, x, x)?;
        let e = self.bin(VBin::FEq, x, z)?;
        let bad = self.bin(VBin::Or, n, e)?;
        self.bail(bad)
    }
}
