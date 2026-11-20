//! Builder basics: registers, constants, masks, lane values and block entry and exit.

#[allow(clippy::wildcard_imports)]
use crate::lower::vector::*;

impl Builder<'_, '_, '_> {
    pub(crate) fn reg(&mut self) -> Result<VReg, No> {
        let r = self.next;
        self.next = self.next.checked_add(1).ok_or(())?;
        Ok(r)
    }

    pub(crate) const fn lanes_k(&self) -> K {
        lane_k(self.w)
    }

    /// The lane width of `p`, or none (`bool` fits any).
    pub(crate) fn width(&self, p: PrimTy) -> Option<u32> {
        if p == PrimTy::Bool || p == PrimTy::Str {
            return None;
        }
        Some(p.bit_width(self.lw.c.t.pointer_width) / 8)
    }

    /// A register holding `bits` in every lane.
    pub(crate) fn konst(&mut self, bits: u64) -> Result<VReg, No> {
        let mask = if self.w == 8 {
            u64::MAX
        } else {
            (1u64 << (self.w * 8)) - 1
        };
        let bits = bits & mask;
        if let Some(r) = self.consts.get(&bits) {
            return Ok(*r);
        }
        let dst = self.reg()?;
        self.pre.push(VOp::Const { dst, bits });
        self.pinned.push(dst);
        self.consts.insert(bits, dst);
        Ok(dst)
    }

    pub(crate) const fn ones() -> u64 {
        u64::MAX
    }

    pub(crate) const fn sign(&self) -> u64 {
        1u64 << (self.w * 8 - 1)
    }

    pub(crate) fn emit(&mut self, op: VOp) {
        self.body.push(op);
    }

    pub(crate) fn bin(&mut self, op: VBin, a: VReg, b: VReg) -> Result<VReg, No> {
        if !self.isa.bin(op, self.w) {
            return Err(());
        }
        let dst = self.reg()?;
        self.emit(VOp::Bin { op, dst, a, b });
        Ok(dst)
    }

    /// `a * b + c` rounded once, where the vector unit fuses it.
    pub(crate) fn fma(&mut self, a: VReg, b: VReg, c: VReg) -> Result<VReg, No> {
        let k = if self.w == 8 { K::F64 } else { K::F32 };
        if !self.isa.fma(self.w) || !self.lw.fuses(k) {
            return Err(());
        }
        let dst = self.reg()?;
        self.emit(VOp::Fma { dst, a, b, c });
        Ok(dst)
    }

    pub(crate) fn not(&mut self, a: VReg) -> Result<VReg, No> {
        let o = self.konst(Self::ones())?;
        let r = self.bin(VBin::Xor, a, o)?;
        if let Some(&(x, y, s, n)) = self.gts.get(&a) {
            self.gts.insert(r, (x, y, s, !n));
        }
        if let Some(&(x, y, n)) = self.flts.get(&a) {
            self.flts.insert(r, (x, y, !n));
        }
        Ok(r)
    }

    /// `a > b` as a mask, `signed` or not.
    pub(crate) fn gt(&mut self, a: VReg, b: VReg, signed: bool) -> Result<VReg, No> {
        let r = if signed || self.isa.bin(VBin::GtU, self.w) {
            self.bin(if signed { VBin::GtS } else { VBin::GtU }, a, b)?
        } else {
            let s = self.konst(self.sign())?;
            let x = self.bin(VBin::Xor, a, s)?;
            let y = self.bin(VBin::Xor, b, s)?;
            self.bin(VBin::GtS, x, y)?
        };
        self.gts.insert(r, (a, b, signed, false));
        Ok(r)
    }

    /// Whether every way from the header to block `b` passes one of
    /// blocks `s`.
    pub(crate) fn covered(&self, s: &[usize], b: usize) -> bool {
        let mut seen: HashSet<usize> = HashSet::default();
        let mut work = vec![self.top];
        while let Some(x) = work.pop() {
            if s.contains(&x) || !seen.insert(x) {
                continue;
            }
            if x == b {
                return false;
            }
            if let Some(n) = self.succ.get(&x) {
                work.extend(n.iter().copied());
            }
        }
        true
    }

    /// Whether block `b` follows block `a` on some way through the loop.
    pub(crate) fn after(&self, a: usize, b: usize) -> bool {
        let mut seen: HashSet<usize> = HashSet::default();
        let mut work: Vec<usize> = self.succ.get(&a).cloned().unwrap_or_default();
        while let Some(x) = work.pop() {
            if x == b {
                return true;
            }
            if seen.insert(x) {
                if let Some(n) = self.succ.get(&x) {
                    work.extend(n.iter().copied());
                }
            }
        }
        false
    }

    /// `t = v` in the lanes of the block at hand.
    pub(crate) fn set(&mut self, t: LocalId, v: (VReg, Form)) -> Result<(), No> {
        let Some(p) = self.pred else {
            self.vals.insert(t, v);
            self.defs.remove(&t);
            return Ok(());
        };
        let Some(old) = self.vals.get(&t).copied() else {
            self.vals.insert(t, v);
            self.defs.insert(t, vec![self.cur]);
            return Ok(());
        };
        // The lanes of other ways keep what they hold.
        let merged = match (v.1, old.1) {
            (Form::Num, Form::Num) => (self.select(p, v.0, old.0)?, Form::Num),
            (Form::Num, _) | (_, Form::Num) => return Err(()),
            _ => {
                let (a, b) = (self.mask(v)?, self.mask(old)?);
                (self.select(p, a, b)?, Form::Mask)
            }
        };
        self.vals.insert(t, merged);
        if let Some(d) = self.defs.get_mut(&t) {
            d.push(self.cur);
        }
        Ok(())
    }

    pub(crate) fn mask(&mut self, (r, f): (VReg, Form)) -> Result<VReg, No> {
        match f {
            Form::Mask => Ok(r),
            Form::Bit => {
                let z = self.konst(0)?;
                self.bin(VBin::Sub, z, r)
            }
            Form::Num => Err(()),
        }
    }

    pub(crate) fn bit(&mut self, (r, f): (VReg, Form)) -> Result<VReg, No> {
        match f {
            Form::Bit => Ok(r),
            Form::Mask => {
                let one = self.konst(1)?;
                self.bin(VBin::And, r, one)
            }
            Form::Num => Err(()),
        }
    }

    pub(crate) fn sign_mask(&mut self, a: VReg) -> Result<VReg, No> {
        let dst = self.reg()?;
        self.emit(VOp::Un {
            op: VUn::SignMask,
            dst,
            a,
        });
        Ok(dst)
    }

    /// The vector value of operand `o` read as a `ty`.
    pub(crate) fn val(&mut self, o: &Operand, ty: &MirTy) -> Result<(VReg, Form), No> {
        let MirTy::Prim(p) = ty else { return Err(()) };
        if *p != PrimTy::Bool && self.width(*p) != Some(self.w) {
            // Narrower values live in wider lanes only as elements.
            return Err(());
        }
        let form = if *p == PrimTy::Bool {
            Form::Bit
        } else {
            Form::Num
        };
        match o {
            Operand::Const(c) => {
                let bits = match self.lw.const_val(c, ty) {
                    Val::S(V::I(b), _) => b as u64,
                    _ => return Err(()),
                };
                if *p == PrimTy::Bool {
                    let r = self.konst(if bits & 1 == 1 { Self::ones() } else { 0 })?;
                    return Ok((r, Form::Mask));
                }
                Ok((self.konst(bits)?, Form::Num))
            }
            Operand::Copy(l) => {
                if *l == self.iv || self.ivoff.contains_key(l) {
                    return Err(());
                }
                let lt = self.lw.local_ty(*l);
                let same = lt == *ty
                    || matches!((&lt, p), (MirTy::Prim(q), p)
                        if q.is_int() && p.is_int() && self.width(*q) == self.width(*p));
                if !same {
                    return Err(());
                }
                if self.defs.get(l).is_some_and(|d| !self.covered(d, self.cur)) {
                    // Assigned on other ways through the loop only.
                    return Err(());
                }
                if let Some(v) = self.vals.get(l) {
                    return Ok(*v);
                }
                if let Some(v) = self.inv.get(l) {
                    return Ok(*v);
                }
                if self.written.contains(l) {
                    return Err(());
                }
                // Read, never written here: the same in every lane.
                let dst = self.reg()?;
                let off = self.splats.len() as u32;
                self.splats.push((o.clone(), ty.clone()));
                self.pre.push(VOp::Splat { dst, off });
                self.pinned.push(dst);
                self.inv.insert(*l, (dst, form));
                Ok((dst, form))
            }
        }
    }

    /// The base of buffer `b` at index offset `off`.
    pub(crate) fn base(
        &mut self,
        b: &Operand,
        index: &Operand,
        store: bool,
        e: u32,
    ) -> Result<u8, No> {
        let Operand::Copy(buf) = b else {
            return Err(());
        };
        let buf = &self.canon(*buf);
        if self.vals.contains_key(buf) || self.lw.local_ty(*buf) != MirTy::GcRef {
            return Err(());
        }
        let idx = match index {
            Operand::Copy(l) if *l == self.iv => Idx::IV,
            Operand::Copy(l) => *self.ivoff.get(l).ok_or(())?,
            Operand::Const(_) => return Err(()),
        };
        self.accesses.push(Access {
            base: *buf,
            inv: idx.inv,
            off: idx.c,
            e,
            store,
        });
        let key = (*buf, idx, e);
        if let Some(i) = self.bases.iter().position(|b| *b == key) {
            return Ok(i as u8);
        }
        if self.bases.len() >= self.isa.bases() {
            return Err(());
        }
        self.bases.push(key);
        Ok(self.bases.len() as u8 - 1)
    }

    /// The local holding the value `l` holds, loaded in front.
    pub(crate) fn canon(&self, l: LocalId) -> LocalId {
        self.same.get(&l).copied().unwrap_or(l)
    }

    /// Whether every lane may read `buf[index]`.
    pub(crate) fn safe_at(&self, buf: &Operand, index: &Operand) -> bool {
        let Operand::Copy(b) = buf else { return false };
        let b = &self.canon(*b);
        let idx = match index {
            Operand::Copy(l) if *l == self.iv => Idx::IV,
            Operand::Copy(l) => match self.ivoff.get(l) {
                Some(x) => *x,
                None => return false,
            },
            Operand::Const(_) => return false,
        };
        self.safe
            .iter()
            .any(|(x, inv, c)| x == b && *inv == idx.inv && *c >= idx.c)
    }

    /// Bail out of the vector before this iteration when a lane of `a`
    /// has its sign bit set: only before any store.
    pub(crate) fn bail(&mut self, a: VReg) -> Result<(), No> {
        if self.stored {
            return Err(());
        }
        let a = match self.pred {
            Some(p) => self.bin(VBin::And, a, p)?,
            None => a,
        };
        self.emit(VOp::Bail { a });
        Ok(())
    }

    /// `m ? a : b` per lane (`m` a mask).
    pub(crate) fn select(&mut self, m: VReg, a: VReg, b: VReg) -> Result<VReg, No> {
        if a == b {
            return Ok(a);
        }
        if let Some(&(x, y, signed, neg)) = self.gts.get(&m) {
            // `x > y ? x : y` is `max(x, y)`, the other way `min`.
            let max = match (a, b) {
                _ if (a, b) == (x, y) => Some(!neg),
                _ if (a, b) == (y, x) => Some(neg),
                _ => None,
            };
            let op = match (max, signed) {
                (Some(true), true) => Some(VBin::MaxS),
                (Some(true), false) => Some(VBin::MaxU),
                (Some(false), true) => Some(VBin::MinS),
                (Some(false), false) => Some(VBin::MinU),
                (None, _) => None,
            };
            if let Some(op) = op.filter(|op| self.isa.bin(*op, self.w)) {
                return self.bin(op, a, b);
            }
        }
        if let Some(&(x, y, neg)) = self.flts.get(&m) {
            // Of floats neither a NaN nor zero, `x < y ? x : y` is
            // `min(x, y)`, the other way `max`.
            let max = match (a, b) {
                _ if (a, b) == (x, y) => Some(neg),
                _ if (a, b) == (y, x) => Some(!neg),
                _ => None,
            };
            if let Some(max) = max {
                return self.bin(if max { VBin::FMax } else { VBin::FMin }, a, b);
            }
        }
        let x = self.bin(VBin::And, a, m)?;
        let y = self.bin(VBin::AndNot, b, m)?;
        self.bin(VBin::Or, x, y)
    }

    /// Into block `bi` (`full`: every staying lane runs it): its lanes
    /// and its parameters from the edges in.
    pub(crate) fn start(&mut self, bi: usize, full: bool) -> Result<(), No> {
        self.cur = bi;
        let ins = self.inbox.remove(&bi).unwrap_or_default();
        if ins.is_empty() {
            // The header.
            self.pred = None;
            return Ok(());
        }
        self.pred = None;
        if !full {
            let mut p: Option<VReg> = None;
            for (e, _) in &ins {
                let Some(e) = *e else {
                    p = None;
                    break;
                };
                p = Some(match p {
                    None => e,
                    Some(q) => self.bin(VBin::Or, q, e)?,
                });
            }
            self.pred = p;
        }
        let params = self.lw.f.blocks[bi].params.clone();
        for (k, x) in params.iter().enumerate() {
            let ty = self.lw.local_ty(*x);
            let boolean = ty == MirTy::Prim(PrimTy::Bool);
            match ty {
                MirTy::Prim(p) if boolean || self.width(p) == Some(self.w) => {}
                _ => return Err(()),
            }
            let mut acc: Option<(VReg, Form)> = None;
            for (e, args) in ins.iter().rev() {
                let mut v = *args.get(k).ok_or(())?;
                if boolean {
                    v = (self.mask(v)?, Form::Mask);
                } else if v.1 != Form::Num {
                    return Err(());
                }
                acc = Some(match (acc, e) {
                    (None, _) => v,
                    (Some(a), Some(e)) => (self.select(*e, v.0, a.0)?, v.1),
                    (Some(_), None) => return Err(()),
                });
            }
            self.vals.insert(*x, acc.ok_or(())?);
            if self.pred.is_some() {
                self.defs.insert(*x, vec![bi]);
            } else {
                self.defs.remove(x);
            }
        }
        self.started.push((bi, self.pred));
        if self.pred.is_none() {
            self.flush(bi)?;
        }
        Ok(())
    }

    /// Into block `bi`, which every lane runs: the stores some lanes did
    /// before. Every way here stored: in all lanes, merged. Else only
    /// where the target writes lanes under a mask.
    pub(crate) fn flush(&mut self, bi: usize) -> Result<(), No> {
        for p in std::mem::take(&mut self.pending) {
            let base = self.base(&p.buf, &p.index, true, self.w)?;
            self.stored = true;
            if self.covered(&p.blocks, bi) {
                self.emit(VOp::Store { src: p.src, base });
            } else if self.isa.masked(self.w) {
                self.emit(VOp::MaskStore {
                    src: p.src,
                    mask: p.mask,
                    base,
                });
            } else {
                return Err(());
            }
        }
        Ok(())
    }

    /// Out of block `bi` (not the latch): its side exit leaves the
    /// vector, its edges pass lanes and arguments on. `inside`: the
    /// loop's block ids; `consumed`: tests the loop's bound covers.
    pub(crate) fn end(
        &mut self,
        bi: usize,
        inside: &HashSet<u32>,
        consumed: &HashSet<LocalId>,
    ) -> Result<(), No> {
        let f = self.lw.f;
        let at = |id: BlockId| f.blocks.iter().position(|b| b.id == id);
        let pass =
            |s: &mut Self, to: BlockId, args: &[Operand], e: Option<VReg>| -> Result<(), No> {
                let ti = at(to).ok_or(())?;
                let params = &f.blocks[ti].params;
                let mut vs = Vec::new();
                for (a, x) in args.iter().zip(params) {
                    let ty = s.lw.local_ty(*x);
                    vs.push(s.val(a, &ty)?);
                }
                s.inbox.entry(ti).or_default().push((e, vs));
                Ok(())
            };
        match &f.blocks[bi].terminator {
            Terminator::Goto { target, args } => {
                if !inside.contains(&target.0) {
                    // Every lane here leaves.
                    let p = self.pred.ok_or(())?;
                    self.pred = None;
                    return self.bail(p);
                }
                let p = self.pred;
                pass(self, *target, args, p)
            }
            Terminator::If {
                cond: Operand::Copy(c),
                then_bb,
                then_args,
                else_bb,
                else_args,
            } => {
                let (tin, ein) = (inside.contains(&then_bb.0), inside.contains(&else_bb.0));
                if tin && ein {
                    let v = self.val(&Operand::Copy(*c), &MirTy::Prim(PrimTy::Bool))?;
                    let m = self.mask(v)?;
                    let (tp, ep) = match self.pred {
                        Some(p) => (self.bin(VBin::And, p, m)?, self.bin(VBin::AndNot, p, m)?),
                        None => (m, self.not(m)?),
                    };
                    pass(self, *then_bb, then_args, Some(tp))?;
                    return pass(self, *else_bb, else_args, Some(ep));
                }
                let on_true = !tin;
                if !consumed.contains(c) {
                    self.exit(*c, on_true)?;
                }
                // The lanes that stay are those that came.
                let p = self.pred;
                if tin {
                    pass(self, *then_bb, then_args, p)
                } else {
                    pass(self, *else_bb, else_args, p)
                }
            }
            _ => Err(()),
        }
    }
}
