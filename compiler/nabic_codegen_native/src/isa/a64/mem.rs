//! Loads, stores and register moves.

#[allow(clippy::wildcard_imports)]
use crate::isa::a64::*;

impl Em<'_> {
    /// Load / store `rt` at `base + off`.
    /// A read of `len` bytes at `[base + off]`: if from the frame, the
    /// stores waiting there go out first.
    pub(crate) fn frame_read(&mut self, base: u32, off: i64, len: i64) {
        if base == SP {
            let hit: Vec<u32> = self
                .pending
                .iter()
                .filter(|p| p.at < off + len && off < p.at + (1 << p.op.size()))
                .map(|p| p.slot)
                .collect();
            for s in hit {
                self.flush(Some(s));
            }
        }
    }

    pub(crate) fn ldst(&mut self, op: Ls, rt: u32, base: u32, off: i64) {
        let store = op.store();
        let scale = 1i64 << op.size();
        if !store {
            self.frame_read(base, off, scale);
        }
        let w = if off >= 0 && off % scale == 0 && off / scale < 4096 {
            enc::ldst(op, rt, R(base), (off / scale) as u32)
        } else if (-256..256).contains(&off) {
            enc::ldst_unscaled(op, rt, R(base), off)
        } else {
            self.add_imm(X17, base, off);
            enc::ldst(op, rt, R(X17), 0)
        };
        if store {
            // Writes no register.
            self.a.push(w);
        } else {
            self.ins(w);
        }
    }

    /// `(base register, offset)` of `m`; an indirect pointer goes into `tmp`.
    pub(crate) fn base(&mut self, m: Mem, tmp: u32) -> (u32, i64) {
        match m {
            Mem::Frame(s, o) => (SP, i64::from(self.slot_off[s as usize] + o)),
            Mem::Ind(s, o) => {
                if let Some(r) = self.ra.base(m) {
                    return (r, i64::from(o));
                }
                let p = Mem::Frame(s, 0);
                if let Some(h) = self.find(p, 8, false) {
                    self.consume(p);
                    return (h.reg, i64::from(o));
                }
                let so = i64::from(self.slot_off[s as usize]);
                self.ldst(LDRX, tmp, SP, so);
                self.hold(tmp, false, p, 8, None);
                (tmp, i64::from(o))
            }
        }
    }

    pub(crate) fn addr(&mut self, rd: u32, m: Mem) {
        if let Mem::Frame(s, _) = m {
            // Reached through the address.
            self.flush(Some(s));
        }
        let (b, o) = self.base(m, rd);
        self.add_imm(rd, b, o);
    }

    pub(crate) const fn load_op(k: K, signed: bool) -> Ls {
        match (k, signed) {
            (K::I8, true) => LDRSB,
            (K::I8, false) => LDRB,
            (K::I16, true) => LDRSH,
            (K::I16, false) => LDRH,
            (K::I32, true) => LDRSW,
            (K::I32 | K::F32, _) => LDRW,
            (K::I64 | K::F64, _) => LDRX,
        }
    }

    pub(crate) const fn store_op(k: K) -> Ls {
        match k {
            K::I8 => STRB,
            K::I16 => STRH,
            K::I32 | K::F32 => STRW,
            K::I64 | K::F64 => STRX,
        }
    }

    /// Integer register `rd` = `v` as `k`, extended by `signed`.
    pub(crate) fn load(&mut self, rd: u32, v: V, k: K, signed: bool) {
        // A float's bits load unextended.
        let signed = signed && !k.is_float();
        if let V::M(m) = v {
            if let Some(a) = self.ra.get(m) {
                let r = a.reg;
                let w = if a.fp {
                    enc::fmov_gpr(true, k.size() == 8, rd, r)
                } else if k.size() == 8 || !signed {
                    // Kept zero-extended.
                    if r == rd {
                        return;
                    }
                    enc::mov(true, R(rd), R(r))
                } else {
                    enc::sxt(k.size(), R(rd), R(r))
                };
                self.ins(w);
                return;
            }
            if let Some(h) = self.find(m, k.size(), false) {
                self.consume(m);
                let r = h.reg;
                if k.size() == 8 || h.ext == Some(signed) {
                    if r != rd {
                        self.ins(enc::mov(true, R(rd), R(r)));
                        self.hold(rd, false, m, k.size(), h.ext);
                    }
                    return;
                }
                let w = match (k.size(), signed) {
                    (n @ (1 | 2 | 4), true) => enc::sxt(n, R(rd), R(r)),
                    (n @ (1 | 2), false) => enc::uxt(n, R(rd), R(r)),
                    _ => enc::mov(false, R(rd), R(r)),
                };
                self.ins(w);
                self.hold(rd, false, m, k.size(), Some(signed));
                return;
            }
        }
        match v {
            V::I(i) => {
                let bits = k.bits();
                let x = if bits == 64 {
                    i as u64
                } else if signed {
                    ((i << (64 - bits)) >> (64 - bits)) as u64
                } else {
                    (i as u64) & ((1u64 << bits) - 1)
                };
                self.mov_imm(rd, x);
            }
            V::M(m) => {
                let (b, o) = self.base(m, X14);
                self.ldst(Self::load_op(k, signed), rd, b, o);
                self.hold(rd, false, m, k.size(), Some(signed));
            }
        }
    }

    /// A register holding `v` as `k` extended by `signed`, not in `avoid`
    /// (registers written before this is read): one already holding it,
    /// else a loaded value register; never `x14`, `x15`, `x17`.
    pub(crate) fn reg(&mut self, v: V, k: K, signed: bool, avoid: &[u32]) -> u32 {
        let signed = signed && !k.is_float();
        if let V::M(m) = v {
            if let Some(a) = self.ra.get(m) {
                if !a.fp && (k.size() == 8 || !signed) && !avoid.contains(&a.reg) {
                    return a.reg;
                }
            }
            if let Some(h) = self.find(m, k.size(), false) {
                if !matches!(h.reg, X14 | X15 | X17)
                    && !avoid.contains(&h.reg)
                    && (k.size() == 8 || h.ext == Some(signed))
                {
                    self.consume(m);
                    return h.reg;
                }
            }
        }
        let r = self.pick(&POOL, avoid);
        self.load(r, v, k, signed);
        r
    }

    /// `reg` for a float: a vector register.
    pub(crate) fn freg(&mut self, v: V, k: K, avoid: &[u32]) -> u32 {
        if let V::M(m) = v {
            if let Some(a) = self.ra.get(m) {
                if a.fp && !avoid.contains(&a.reg) {
                    return a.reg;
                }
            }
            if let Some(h) = self.find(m, k.size(), true) {
                if !avoid.contains(&h.reg) {
                    self.consume(m);
                    return h.reg;
                }
            }
        }
        let r = self.pick(&FP_POOL, avoid);
        self.load_fp(r, v, k);
        r
    }

    pub(crate) fn store(&mut self, rs: u32, m: Mem, k: K) {
        self.store_ext(rs, m, k, None);
    }

    /// Store `rs`, whose upper bits are known (`ext`) or not.
    pub(crate) fn store_ext(&mut self, rs: u32, m: Mem, k: K, ext: Option<bool>) {
        if let Some(a) = self.ra.get(m) {
            let r = a.reg;
            let w = if a.fp {
                enc::fmov_gpr(false, k.size() == 8, r, rs)
            } else if k.size() == 8 || (k.size() < 4 && ext == Some(false)) {
                if r == rs {
                    return;
                }
                enc::mov(true, R(r), R(rs))
            } else if k.size() == 4 {
                if r == rs && ext == Some(false) {
                    return;
                }
                // Zero-extends.
                enc::mov(false, R(r), R(rs))
            } else {
                // and w, #0xffff / #0xff
                let mask = (1u64 << (8 * k.size())) - 1;
                let m = enc::bitmask(mask, 32).expect("a mask");
                enc::log_imm(Log::And, false, R(r), R(rs), m)
            };
            self.ins(w);
            return;
        }
        self.put(Self::store_op(k), rs, m, k.size());
        self.hold(rs, false, m, k.size(), ext);
    }

    /// Store `rs` (`op`, `size` bytes) at `m`: later, for a `once` slot.
    pub(crate) fn put(&mut self, op: Ls, rs: u32, m: Mem, size: u32) {
        let (b, o) = self.base(m, X15);
        self.wrote(m, size);
        if let Mem::Frame(slot, off) = m {
            let fits = o % i64::from(size) == 0 && o / i64::from(size) < 4096;
            match self.waits.get(&(slot, off)) {
                Some(&(z, 0)) if z == size => return,
                Some(&(z, 1)) if z == size && fits => {
                    self.pending.push(Pend {
                        reg: rs,
                        op,
                        slot,
                        off,
                        at: o,
                    });
                    return;
                }
                _ => {}
            }
        }
        self.ldst(op, rs, b, o);
    }

    pub(crate) fn load_fp(&mut self, vd: u32, v: V, k: K) {
        if let V::M(m) = v {
            if let Some(a) = self.ra.get(m) {
                if !a.fp {
                    self.ins(enc::fmov_gpr(false, k == K::F64, vd, a.reg));
                } else if a.reg != vd {
                    self.ins(fmov(k == K::F64, vd, a.reg));
                }
                return;
            }
            if let Some(h) = self.find(m, k.size(), false) {
                self.consume(m);
                self.ins(enc::fmov_gpr(false, k == K::F64, vd, h.reg));
                self.hold(vd, true, m, k.size(), None);
                return;
            }
            if let Some(h) = self.find(m, k.size(), true) {
                self.consume(m);
                if h.reg != vd {
                    self.ins(fmov(k == K::F64, vd, h.reg));
                    self.hold(vd, true, m, k.size(), None);
                }
                return;
            }
        }
        match v {
            V::I(bits) => {
                self.mov_imm(X13, bits as u64);
                self.ins(enc::fmov_gpr(false, k != K::F32, vd, X13));
            }
            V::M(m) => {
                let (b, o) = self.base(m, X14);
                self.ldst(if k == K::F32 { LDRS } else { LDRD }, vd, b, o);
                self.hold(vd, true, m, k.size(), None);
            }
        }
    }

    pub(crate) fn store_fp(&mut self, vs: u32, m: Mem, k: K) {
        if let Some(a) = self.ra.get(m) {
            if !a.fp {
                self.ins(enc::fmov_gpr(true, k == K::F64, a.reg, vs));
            } else if a.reg != vs {
                self.ins(fmov(k == K::F64, a.reg, vs));
            }
            return;
        }
        self.put(if k == K::F32 { STRS } else { STRD }, vs, m, k.size());
        self.hold(vs, true, m, k.size(), None);
    }
}
