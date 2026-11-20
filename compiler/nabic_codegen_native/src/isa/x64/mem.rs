//! Loads, stores and register moves.

#[allow(clippy::wildcard_imports)]
use crate::isa::x64::*;

impl Em<'_> {
    /// `[base + disp]` of `m`; an indirect pointer goes into `tmp`
    /// (unless a register holds it).
    pub(crate) fn mem(&mut self, m: Mem, tmp: u32) -> Rm {
        match m {
            Mem::Frame(s, o) => Rm::M(self.sp((self.slot_off[s as usize] + o) as i32)),
            Mem::Ind(s, o) => {
                if let Some(r) = self.ra.base(m) {
                    return Rm::m(r, o);
                }
                let p = Mem::Frame(s, 0);
                if let Some(h) = self.find(p, 8, false) {
                    self.consume(p);
                    return Rm::m(h.reg, o);
                }
                self.rd(p, 8);
                self.w(tmp, false);
                let so = self.slot_off[s as usize] as i32;
                self.a.mov(Sz::Q, Gpr(tmp), Rm::M(self.sp(so)));
                self.hold(tmp, false, p, 8, None);
                Rm::m(tmp, o)
            }
        }
    }

    pub(crate) fn mov_imm(&mut self, r: u32, v: u64) {
        self.w(r, false);
        // Above `-O0`, zero by `xor` unless a branch still reads the flags.
        if v == 0 && self.opt && self.flags.is_none() && self.tested.is_none() {
            self.a.alu_rr(Alu::Xor, Sz::D, Gpr(r), Gpr(r));
        } else if u32::try_from(v).is_ok() {
            self.a.mov_ri32(Gpr(r), v as u32);
        } else if (v as i64) >= i64::from(i32::MIN) && (v as i64) < 0 {
            self.a.mov_ri_sx(Gpr(r), v as i32);
        } else {
            self.a.movabs(Gpr(r), v);
        }
    }

    /// Integer register `r` = `v` as `k`, extended by `signed` to 64 bits.
    pub(crate) fn load(&mut self, r: u32, v: V, k: K, signed: bool) {
        // A float's bits load unextended.
        let signed = signed && !k.is_float();
        if let V::M(m) = v {
            if let Some(a) = self.ra.get(m) {
                if a.fp {
                    self.w(r, false);
                    self.a.movq_gx(Sz::dq(k.size() == 8), Gpr(r), Xmm(a.reg));
                } else if k.size() == 8 || !signed {
                    // Kept zero-extended.
                    self.mov_rr(r, a.reg);
                } else {
                    self.w(r, false);
                    self.extend(r, Rm::r(a.reg), k, true);
                }
                return;
            }
            if let Some(h) = self.find(m, k.size(), false) {
                self.consume(m);
                if k.size() == 8 || h.ext == Some(signed) {
                    if h.reg != r {
                        self.w(r, false);
                        self.a.mov(Sz::Q, Gpr(r), Rm::r(h.reg));
                        self.hold(r, false, m, k.size(), h.ext);
                    }
                    return;
                }
                self.w(r, false);
                self.extend(r, Rm::r(h.reg), k, signed);
                self.hold(r, false, m, k.size(), Some(signed));
                return;
            }
            if let Some(h) = self
                .find(m, k.size(), true)
                .filter(|_| k.size() == 8 || !signed)
            {
                self.consume(m);
                self.w(r, false);
                // Zero-extends.
                self.a.movq_gx(Sz::dq(k.size() == 8), Gpr(r), Xmm(h.reg));
                self.hold(r, false, m, k.size(), Some(false));
                return;
            }
        }
        match v {
            V::I(i) => self.mov_imm(r, ext(i, k, signed) as u64),
            V::M(m) => {
                let rm = self.mem(m, R10);
                self.rd(m, k.size());
                self.w(r, false);
                self.extend(r, rm, k, signed);
                self.hold(r, false, m, k.size(), Some(signed));
            }
        }
    }

    /// `r` = the `k` at `rm`, extended by `signed` to 64 bits.
    pub(crate) fn extend(&mut self, r: u32, rm: Rm, k: K, signed: bool) {
        let r = Gpr(r);
        match (k, signed) {
            (K::I8, true) => self.a.movsx(Sz::Q, r, Sz::B, rm),
            (K::I8, false) => self.a.movzx(Sz::D, r, Sz::B, rm),
            (K::I16, true) => self.a.movsx(Sz::Q, r, Sz::W, rm),
            (K::I16, false) => self.a.movzx(Sz::D, r, Sz::W, rm),
            (K::I32, true) => self.a.movsxd(r, rm),
            (K::I32 | K::F32, _) => self.a.mov(Sz::D, r, rm),
            (K::I64 | K::F64, _) => self.a.mov(Sz::Q, r, rm),
        }
    }

    /// A register holding `v` as `k` extended by `signed`, not in `avoid`: one
    /// already holding it, else a loaded value register; never `r10` / `r11`.
    pub(crate) fn reg(&mut self, v: V, k: K, signed: bool, avoid: &[u32]) -> u32 {
        let signed = signed && !k.is_float();
        if let V::M(m) = v {
            if let Some(a) = self.ra.get(m) {
                if !a.fp && (k.size() == 8 || !signed) && !avoid.contains(&a.reg) {
                    return a.reg;
                }
            }
            if let Some(h) = self.find(m, k.size(), false) {
                if !matches!(h.reg, R10 | R11)
                    && !avoid.contains(&h.reg)
                    && (k.size() == 8 || h.ext == Some(signed))
                {
                    self.consume(m);
                    return h.reg;
                }
            }
        }
        let r = self.pick(&POOL, false, avoid);
        self.load(r, v, k, signed);
        r
    }

    /// `reg` for a float: an `xmm`.
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
        let x = self.pick(&FP_POOL, true, avoid);
        self.load_fp(x, v, k);
        x
    }

    /// `rd` = `ra` (all 64 bits), about to be written.
    pub(crate) fn mov_rr(&mut self, rd: u32, ra: u32) {
        self.w(rd, false);
        if rd != ra {
            self.a.mov(Sz::Q, Gpr(rd), Rm::r(ra));
        }
    }

    pub(crate) fn store(&mut self, r: u32, m: Mem, k: K) {
        self.store_ext(r, m, k, None);
    }

    /// Store `r`, whose upper bits are known (`ext`) or not.
    pub(crate) fn store_ext(&mut self, r: u32, m: Mem, k: K, ext: Option<bool>) {
        if let Some(a) = self.ra.get(m) {
            let d = a.reg;
            if a.fp {
                self.w(d, true);
                self.a.movq_xg(Sz::dq(k.size() == 8), Xmm(d), Gpr(r));
            } else if k.size() == 8 || (k.size() < 4 && ext == Some(false)) {
                self.mov_rr(d, r);
            } else if k.size() == 4 {
                if d != r || ext != Some(false) {
                    self.w(d, false);
                    self.a.mov(Sz::D, Gpr(d), Rm::r(r)); // zero-extends
                }
            } else {
                // Kept zero-extended.
                self.w(d, false);
                self.extend(d, Rm::r(r), k, false);
            }
            return;
        }
        self.put(r, false, m, k);
        self.hold(r, false, m, k.size(), ext);
    }

    /// Store `r` (`fp`: an `xmm`) at `m`: later, for a slot written and
    /// read once; never, for one never read.
    pub(crate) fn put(&mut self, r: u32, fp: bool, m: Mem, k: K) {
        let rm = self.mem(m, R11);
        self.wrote(m, k.size());
        if let (Mem::Frame(slot, off), Rm::M(at @ Addr::Bd(..))) = (m, rm) {
            match self.waits.get(&(slot, off)) {
                Some(&(z, 0)) if z == k.size() => return,
                Some(&(z, 1)) if z == k.size() => {
                    self.pending.push(Pend {
                        reg: r,
                        fp,
                        k,
                        slot,
                        off,
                        at,
                    });
                    return;
                }
                _ => {}
            }
        }
        match rm {
            Rm::M(a) if fp => self.a.sse_st(Self::movs_st(k), a, Xmm(r)),
            _ => self.store_raw(r, rm, k),
        }
    }

    pub(crate) fn store_raw(&mut self, r: u32, rm: Rm, k: K) {
        self.a.mov_st(sz(k), rm, Gpr(r));
    }

    /// `movss` / `movsd` load.
    pub(crate) const fn movs(k: K) -> Sse {
        if matches!(k, K::F32) {
            Sse::Movss
        } else {
            Sse::Movsd
        }
    }

    /// `movss` / `movsd` store.
    pub(crate) const fn movs_st(k: K) -> Sse {
        if matches!(k, K::F32) {
            Sse::MovssSt
        } else {
            Sse::MovsdSt
        }
    }

    /// The scalar form of `op` for `k` (`Add`..`Div`, `Sqrt`).
    pub(crate) const fn scalar(k: K, ss: Sse, sd: Sse) -> Sse {
        if matches!(k, K::F32) { ss } else { sd }
    }

    pub(crate) fn load_fp(&mut self, x: u32, v: V, k: K) {
        if let V::M(m) = v {
            if let Some(a) = self.ra.get(m) {
                if !a.fp {
                    self.w(x, true);
                    self.a.movq_xg(Sz::dq(k == K::F64), Xmm(x), Gpr(a.reg));
                } else if a.reg != x {
                    self.w(x, true);
                    self.a.sse(Sse::Movaps, Xmm(x), Xrm::X(Xmm(a.reg)));
                }
                return;
            }
            if let Some(h) = self.find(m, k.size(), true) {
                self.consume(m);
                if h.reg != x {
                    self.w(x, true);
                    self.a.sse(Sse::Movaps, Xmm(x), Xrm::X(Xmm(h.reg)));
                    self.hold(x, true, m, k.size(), None);
                }
                return;
            }
            if let Some(h) = self.find(m, k.size(), false) {
                self.consume(m);
                self.w(x, true);
                self.a.movq_xg(Sz::dq(k == K::F64), Xmm(x), Gpr(h.reg));
                self.hold(x, true, m, k.size(), None);
                return;
            }
        }
        match v {
            V::I(bits) => {
                self.mov_imm(RAX, bits as u64);
                self.w(x, true);
                self.a.movq_xg(Sz::dq(k == K::F64), Xmm(x), Gpr(RAX));
            }
            V::M(m) => {
                let a = addr(self.mem(m, R10));
                self.rd(m, k.size());
                self.w(x, true);
                self.a.sse(Self::movs(k), Xmm(x), Xrm::M(a));
                self.hold(x, true, m, k.size(), None);
            }
        }
    }

    pub(crate) fn store_fp(&mut self, x: u32, m: Mem, k: K) {
        if let Some(a) = self.ra.get(m) {
            if !a.fp {
                self.w(a.reg, false);
                self.a.movq_gx(Sz::dq(k == K::F64), Gpr(a.reg), Xmm(x));
            } else if a.reg != x {
                self.w(a.reg, true);
                self.a.sse(Sse::Movaps, Xmm(a.reg), Xrm::X(Xmm(x)));
            }
            return;
        }
        self.put(x, true, m, k);
        self.hold(x, true, m, k.size(), None);
    }

    pub(crate) fn lea(&mut self, r: u32, m: Mem) {
        if let Mem::Frame(s, _) = m {
            // Reached through the address.
            self.flush(Some(s));
        }
        let a = addr(self.mem(m, r));
        self.w(r, false);
        self.a.lea(Gpr(r), a);
    }
}
