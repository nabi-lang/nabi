//! Statements of a loop body in vector form.

#[allow(clippy::wildcard_imports)]
use crate::lower::vector::*;

impl Builder<'_, '_, '_> {
    /// The statement of a body, in order.
    pub(crate) fn stmt(&mut self, s: &Statement, accs: &HashSet<LocalId>) -> Result<(), No> {
        match s {
            Statement::Loc(..) => Ok(()),
            Statement::BufStore {
                buf,
                index,
                value,
                elem_ty: MirTy::Prim(p),
            } => {
                let e = self.width(*p);
                if let Some(e) = e.filter(|e| *e < self.w) {
                    // Narrowed in its lanes, stored in every lane.
                    let r = match value {
                        Operand::Copy(l) => self.narrow.get(l).copied(),
                        Operand::Const(_) => None,
                    };
                    let Some((src, n)) = r.filter(|r| r.1 == e) else {
                        return Err(());
                    };
                    let _ = n;
                    if self.pred.is_some()
                        || !self.pending.is_empty()
                        || !self.isa.narrow(self.w, e)
                    {
                        return Err(());
                    }
                    let base = self.base(buf, index, true, e)?;
                    self.emit(VOp::StoreTrunc { src, base });
                    self.stored = true;
                    return Ok(());
                }
                if e.is_some_and(|w| w != self.w) || (*p == PrimTy::Bool && self.w != 1) {
                    return Err(());
                }
                let ty = MirTy::Prim(*p);
                let v = self.val(value, &ty)?;
                let src = if *p == PrimTy::Bool {
                    self.bit(v)?
                } else if v.1 == Form::Num {
                    v.0
                } else {
                    return Err(());
                };
                let Some(m) = self.pred else {
                    if !self.pending.is_empty() {
                        return Err(());
                    }
                    let base = self.base(buf, index, true, self.w)?;
                    self.emit(VOp::Store { src, base });
                    self.stored = true;
                    return Ok(());
                };
                // Some lanes: merged with the other ways' stores at the
                // element, done where they meet.
                let key = |s: &Self, o: &Operand| match o {
                    Operand::Copy(l) => Operand::Copy(s.canon(*l)),
                    c @ Operand::Const(_) => c.clone(),
                };
                let (kb, ki) = (key(self, buf), key(self, index));
                let at = self
                    .pending
                    .iter()
                    .position(|q| key(self, &q.buf) == kb && key(self, &q.index) == ki);
                let Some(at) = at else {
                    if !self.pending.is_empty() {
                        return Err(());
                    }
                    self.pending.push(Pend {
                        buf: buf.clone(),
                        index: index.clone(),
                        src,
                        mask: m,
                        blocks: vec![self.cur],
                    });
                    return Ok(());
                };
                let (old, om) = (self.pending[at].src, self.pending[at].mask);
                let src = self.select(m, src, old)?;
                let mask = self.bin(VBin::Or, om, m)?;
                let q = &mut self.pending[at];
                q.src = src;
                q.mask = mask;
                q.blocks.push(self.cur);
                Ok(())
            }
            Statement::Assign { target, rvalue } => self.assign(*target, rvalue, accs),
            _ => Err(()),
        }
    }

    pub(crate) fn assign(
        &mut self,
        t: LocalId,
        rv: &RValue,
        accs: &HashSet<LocalId>,
    ) -> Result<(), No> {
        let tty = self.lw.local_ty(t);
        if accs.contains(&t) {
            let RValue::BinOp {
                op,
                op_ty: MirTy::Prim(p),
                lhs,
                rhs,
            } = rv
            else {
                return Err(());
            };
            if tty != MirTy::Prim(*p) {
                return Err(());
            }
            let me = Operand::Copy(t);
            if self.dot {
                let x = match (op, lhs, rhs) {
                    (PrimOp::Add | PrimOp::WrappingAdd, l, Operand::Copy(x)) if *l == me => x,
                    (PrimOp::Add | PrimOp::WrappingAdd, Operand::Copy(x), r) if *r == me => x,
                    _ => return Err(()),
                };
                let &(a, b, signed) = self.dotmul.get(x).ok_or(())?;
                let checked = *op == PrimOp::Add && self.checked(*op, *p, &me, &me);
                return self.dot_acc(t, *p, (a, b, signed), checked);
            }
            if self.width(*p) != Some(self.w) {
                // A count: `acc + 1` where a test holds, or `acc + (b
                // as T)`.
                let x = match op {
                    PrimOp::Add | PrimOp::WrappingAdd if *lhs == me => rhs,
                    PrimOp::Add | PrimOp::WrappingAdd if *rhs == me => lhs,
                    _ => return Err(()),
                };
                let m = match x {
                    Operand::Const(c)
                        if nabic_codegen::analysis::const_int(&Operand::Const(c.clone()))
                            == Some(1) =>
                    {
                        self.konst(Self::ones())?
                    }
                    Operand::Copy(b) if self.counts.contains(b) => self.vals[b].0,
                    _ => return Err(()),
                };
                let checked = *op == PrimOp::Add && self.checked(*op, *p, &me, &me);
                return self.count(t, *p, m, checked);
            }
            let x = match op {
                _ if *lhs == me && *rhs != me => rhs,
                PrimOp::Add
                | PrimOp::WrappingAdd
                | PrimOp::BitAnd
                | PrimOp::BitOr
                | PrimOp::BitXor
                    if *rhs == me && *lhs != me =>
                {
                    lhs
                }
                _ => return Err(()),
            };
            return self.reduce(t, *op, *p, x);
        }
        if self.dot {
            match rv {
                RValue::Cast {
                    source,
                    from_ty: MirTy::Prim(a),
                    to_ty: MirTy::Prim(_),
                } if self.pred.is_none() => {
                    // A byte widened for a product: its lane as it is.
                    let (r, f) = self.val(source, &MirTy::Prim(*a))?;
                    if f != Form::Num {
                        return Err(());
                    }
                    self.dotcast.insert(t, (r, a.is_signed_int()));
                    return Ok(());
                }
                RValue::BinOp {
                    op: PrimOp::Mul | PrimOp::WrappingMul,
                    lhs: Operand::Copy(x),
                    rhs: Operand::Copy(y),
                    ..
                } => {
                    let (&(a, sa), &(b, sb)) = (
                        self.dotcast.get(x).ok_or(())?,
                        self.dotcast.get(y).ok_or(())?,
                    );
                    if sa != sb {
                        return Err(());
                    }
                    self.dotmul.insert(t, (a, b, sa));
                    return Ok(());
                }
                _ => {}
            }
        }
        let v = match rv {
            RValue::Use(o) => self.val(o, &tty)?,
            RValue::BufLoad {
                buf,
                index,
                elem_ty: MirTy::Prim(p),
            } => {
                let e = self.width(*p).unwrap_or(1);
                if tty != MirTy::Prim(*p)
                    || (*p == PrimTy::Bool && self.w != 1)
                    || !self.pending.is_empty()
                {
                    return Err(());
                }
                if self.pred.is_some() && !self.safe_at(buf, index) {
                    // Lanes that skip it may index past the buffer.
                    return Err(());
                }
                if e < self.w {
                    // Widened in its lanes: a cast to the lane type reads
                    // it.
                    if !p.is_int() || !self.isa.widen(self.w, e) {
                        return Err(());
                    }
                    let base = self.base(buf, index, false, e)?;
                    let dst = self.reg()?;
                    let signed = p.is_signed_int();
                    self.emit(VOp::LoadExt { dst, base, signed });
                    self.widened.insert(t, (dst, signed));
                    return Ok(());
                }
                let base = self.base(buf, index, false, self.w)?;
                let dst = self.reg()?;
                self.emit(VOp::Load { dst, base });
                (
                    dst,
                    if *p == PrimTy::Bool {
                        Form::Bit
                    } else {
                        Form::Num
                    },
                )
            }
            RValue::BinOp {
                op,
                op_ty: MirTy::Prim(p),
                lhs,
                rhs,
            } => {
                let cmp = matches!(
                    op,
                    PrimOp::Eq | PrimOp::Ne | PrimOp::Lt | PrimOp::Le | PrimOp::Gt | PrimOp::Ge
                );
                let want = if cmp {
                    MirTy::Prim(PrimTy::Bool)
                } else {
                    MirTy::Prim(*p)
                };
                if tty != want {
                    return Err(());
                }
                if cmp && p.is_float() && self.facc(lhs) != self.facc(rhs) {
                    // A float minimum's test: its element is no NaN or
                    // zero; equal elements are equal bits.
                    self.float_wild(if self.facc(lhs) { rhs } else { lhs }, *p)?;
                    let r = self.binop(*op, *p, lhs, rhs)?;
                    let x = self.val(lhs, &MirTy::Prim(*p))?.0;
                    let y = self.val(rhs, &MirTy::Prim(*p))?.0;
                    let lt = match op {
                        PrimOp::Lt | PrimOp::Le => Some((x, y, false)),
                        PrimOp::Gt | PrimOp::Ge => Some((y, x, false)),
                        _ => None,
                    };
                    if let Some(lt) = lt {
                        self.flts.insert(r.0, lt);
                    }
                    r
                } else {
                    self.binop(*op, *p, lhs, rhs)?
                }
            }
            RValue::Call { args, .. } if self.lw.float_fma(rv) => {
                let MirTy::Prim(p) = tty else { return Err(()) };
                let [a, b, c] = args.as_slice() else {
                    return Err(());
                };
                if self.width(p) != Some(self.w) || args.iter().any(|o| self.facc(o)) {
                    return Err(());
                }
                let (x, fx) = self.val(a, &tty)?;
                let (y, fy) = self.val(b, &tty)?;
                let (z, fz) = self.val(c, &tty)?;
                if [fx, fy, fz].iter().any(|f| *f != Form::Num) {
                    return Err(());
                }
                (self.fma(x, y, z)?, Form::Num)
            }
            RValue::Call { args, .. } => {
                // The runtime's float `min` / `max`: on values neither of
                // which is a NaN, and that are not two zeros, any order
                // of the operands gives its result.
                let max = self.lw.float_extreme(rv).ok_or(())?;
                let MirTy::Prim(p) = tty else { return Err(()) };
                let [a, b] = args.as_slice() else {
                    return Err(());
                };
                if !p.is_float() || self.width(p) != Some(self.w) {
                    return Err(());
                }
                match (self.facc(a), self.facc(b)) {
                    (true, false) => self.float_wild(b, p)?,
                    (false, true) => self.float_wild(a, p)?,
                    (false, false) => {
                        let (x, y) = (self.val(a, &tty)?.0, self.val(b, &tty)?.0);
                        let z = self.konst(0)?;
                        let nx = self.bin(VBin::FNe, x, x)?;
                        let ny = self.bin(VBin::FNe, y, y)?;
                        let zx = self.bin(VBin::FEq, x, z)?;
                        let zy = self.bin(VBin::FEq, y, z)?;
                        let n = self.bin(VBin::Or, nx, ny)?;
                        let zz = self.bin(VBin::And, zx, zy)?;
                        let bad = self.bin(VBin::Or, n, zz)?;
                        self.bail(bad)?;
                    }
                    (true, true) => return Err(()),
                }
                let (x, fx) = self.val(a, &tty)?;
                let (y, fy) = self.val(b, &tty)?;
                if fx != Form::Num || fy != Form::Num {
                    return Err(());
                }
                let op = if max { VBin::FMax } else { VBin::FMin };
                (self.bin(op, x, y)?, Form::Num)
            }
            RValue::UnaryOp {
                op,
                op_ty: MirTy::Prim(p),
                operand,
            } => {
                if tty != MirTy::Prim(*p) {
                    return Err(());
                }
                self.unop(*op, *p, operand)?
            }
            RValue::Cast {
                source,
                from_ty: MirTy::Prim(a),
                to_ty: MirTy::Prim(b),
            } if tty == MirTy::Prim(*b) => {
                let (wa, wb) = (self.width(*a), self.width(*b));
                if a.is_int() && b.is_int() && wa < Some(self.w) && wb == Some(self.w) {
                    // A widened element, extended as the cast does.
                    let Operand::Copy(l) = source else {
                        return Err(());
                    };
                    let &(r, signed) = self.widened.get(l).ok_or(())?;
                    if signed != a.is_signed_int() || self.lw.local_ty(*l) != MirTy::Prim(*a) {
                        return Err(());
                    }
                    (r, Form::Num)
                } else if a.is_int() && b.is_int() && wa == Some(self.w) && wb < Some(self.w) {
                    // Narrowed only to be stored.
                    let (r, f) = self.val(source, &MirTy::Prim(*a))?;
                    if f != Form::Num || self.pred.is_some() {
                        return Err(());
                    }
                    self.narrow.insert(t, (r, wb.ok_or(())?));
                    return Ok(());
                } else if *a == PrimTy::Bool && b.is_int() && self.width(*b) == Some(self.w) {
                    let v = self.val(source, &MirTy::Prim(*a))?;
                    (self.bit(v)?, Form::Num)
                } else if *a == PrimTy::Bool && b.is_int() {
                    // Only a count reads it.
                    let v = self.val(source, &MirTy::Prim(*a))?;
                    self.counts.insert(t);
                    (self.mask(v)?, Form::Mask)
                } else if a.is_int() && b.is_int() && self.width(*a) == self.width(*b) {
                    self.val(source, &MirTy::Prim(*a))?
                } else {
                    return Err(());
                }
            }
            _ => return Err(()),
        };
        self.set(t, v)
    }
}
