//! Loads, stores and register moves.

#[allow(clippy::wildcard_imports)]
use crate::isa::s390x::*;

impl Em<'_> {
    /// RXY `op` of `r` at `[base + off]`; a far `off` in `r1`, the index.
    pub(crate) fn rxy_at(&mut self, op: Rxy, r: u8, base: u8, off: i64) {
        debug_assert!(base != R0);
        if fits20(off) {
            self.a.rxy(op, r, 0, base, off as i32);
        } else {
            debug_assert!(base != R1);
            self.li(R1, off);
            self.a.rxy(op, r, R1, base, 0);
        }
    }

    /// `(base, offset)` of `m`; an indirect pointer is loaded into `tmp`.
    pub(crate) fn mem(&mut self, m: Mem, tmp: u8) -> (u8, i64) {
        match m {
            Mem::Frame(s, o) => (R15, i64::from(self.slot_off[s as usize] + o)),
            Mem::Ind(s, o) => {
                if let Some(b) = self.ra.base(m) {
                    return (b as u8, i64::from(o));
                }
                let so = i64::from(self.slot_off[s as usize]);
                self.rxy_at(Rxy::Lg, tmp, R15, so);
                (tmp, i64::from(o))
            }
        }
    }

    /// The integer register of an allocated `m`.
    pub(crate) fn dst_x(&self, m: Mem) -> Option<u8> {
        self.ra.get(m).filter(|a| !a.fp).map(|a| a.reg as u8)
    }

    /// The float register of an allocated `m`.
    pub(crate) fn dst_f(&self, m: Mem) -> Option<u8> {
        self.ra.get(m).filter(|a| a.fp).map(|a| a.reg as u8)
    }

    pub(crate) fn dst_f_v(&self, v: V) -> Option<u8> {
        self.ra.get_v(v).filter(|a| a.fp).map(|a| a.reg as u8)
    }

    /// The register holding `v` when it reads as `k` extended by `signed`
    /// (a narrow part is held zero-extended), else `v` loaded into `tmp`.
    pub(crate) fn reg(&mut self, v: V, k: K, signed: bool, tmp: u8) -> u8 {
        if let V::M(m) = v {
            if let Some(r) = self.dst_x(m).filter(|_| !signed || int_k(k).bits() >= 64) {
                return r;
            }
        }
        self.ld(tmp, v, k, signed);
        tmp
    }

    /// The float register holding `v`, else `v` loaded into `tmp`.
    pub(crate) fn freg(&mut self, v: V, k: K, tmp: u8) -> u8 {
        if let Some(f) = self.dst_f_v(v) {
            return f;
        }
        self.ldf(tmp, v, k);
        tmp
    }

    /// `r` = the allocated part `a` as `k`, extended by `signed`.
    pub(crate) fn ld_reg(&mut self, r: u8, a: R, k: K, signed: bool) {
        let bits = k.bits();
        let src = a.reg as u8;
        if a.fp {
            // An `F32` sits in the high word.
            self.a.rre(Rre::Lgdr, r, src);
            if k.size() == 4 {
                self.a
                    .rsy(if signed { Rsy::Srag } else { Rsy::Srlg }, r, r, 0, 32);
            }
            if signed && bits < 32 {
                self.ext_reg(r, r, bits, true);
            }
        } else if signed && bits < 64 {
            self.ext_reg(r, src, bits, true);
        } else {
            self.lgr(r, src);
        }
    }

    /// `r` = `v` as `k`, extended by `signed` to 64 bits.
    pub(crate) fn ld(&mut self, r: u8, v: V, k: K, signed: bool) {
        let k = int_k(k);
        match v {
            V::I(i) => self.li(r, ext(i, k, signed)),
            V::M(m) => {
                if let Some(a) = self.ra.get(m) {
                    self.ld_reg(r, a, k, signed);
                    return;
                }
                let (b, o) = self.mem(m, r);
                self.rxy_at(load_op(k, signed), r, b, o);
            }
        }
    }

    /// Store `k` of `r` at `m` (`r10` the base); an allocated integer part
    /// zero-extended, a float one moved over (`r0` scratch).
    pub(crate) fn st(&mut self, r: u8, m: Mem, k: K) {
        if let Some(a) = self.ra.get(m) {
            let k = int_k(k);
            let d = a.reg as u8;
            if !a.fp {
                self.ext_reg_mov(d, r, k.bits(), false);
            } else if k.size() == 4 {
                self.a.rsy(Rsy::Sllg, R0, r, 0, 32);
                self.a.rre(Rre::Ldgr, d, R0);
            } else {
                self.a.rre(Rre::Ldgr, d, r);
            }
            return;
        }
        let (b, o) = self.mem(m, SB);
        self.rxy_at(store_op(k), r, b, o);
    }

    /// Float register `f` = `v`: an `F32` in the high word.
    pub(crate) fn ldf(&mut self, f: u8, v: V, k: K) {
        match v {
            V::I(i) => {
                let bits = if k == K::F32 {
                    i64::from(i as u32) << 32
                } else {
                    i
                };
                self.li(R0, bits);
                self.a.rre(Rre::Ldgr, f, R0);
            }
            V::M(m) => match self.ra.get(m) {
                Some(a) if a.fp => self.ldr(f, a.reg as u8),
                Some(a) if k.size() == 4 => {
                    self.a.rsy(Rsy::Sllg, R0, a.reg as u8, 0, 32);
                    self.a.rre(Rre::Ldgr, f, R0);
                }
                Some(a) => self.a.rre(Rre::Ldgr, f, a.reg as u8),
                None => {
                    let (b, o) = self.mem(m, FB);
                    self.rxy_at(if k == K::F32 { Rxy::Ley } else { Rxy::Ldy }, f, b, o);
                }
            },
        }
    }

    pub(crate) fn ldr(&mut self, fd: u8, fs: u8) {
        if fd != fs {
            self.a.rr(Rr::Ldr, fd, fs);
        }
    }

    pub(crate) fn stf(&mut self, f: u8, m: Mem, k: K) {
        match self.ra.get(m) {
            Some(a) if a.fp => return self.ldr(a.reg as u8, f),
            Some(a) => {
                let d = a.reg as u8;
                self.a.rre(Rre::Lgdr, d, f);
                if k.size() == 4 {
                    self.a.rsy(Rsy::Srlg, d, d, 0, 32);
                }
                return;
            }
            None => {}
        }
        let (b, o) = self.mem(m, FB);
        self.rxy_at(if k == K::F32 { Rxy::Stey } else { Rxy::Stdy }, f, b, o);
    }

    /// `r` = the address of `m`.
    pub(crate) fn lea(&mut self, r: u8, m: Mem) {
        match m {
            Mem::Frame(s, o) => {
                let off = i64::from(self.slot_off[s as usize] + o);
                self.rxy_at(Rxy::Lay, r, R15, off);
            }
            Mem::Ind(s, o) => {
                if let Some(b) = self.ra.base(m) {
                    if o == 0 {
                        self.lgr(r, b as u8);
                    } else {
                        self.rxy_at(Rxy::Lay, r, b as u8, i64::from(o));
                    }
                    return;
                }
                let so = i64::from(self.slot_off[s as usize]);
                self.rxy_at(Rxy::Lg, r, R15, so);
                if o != 0 {
                    self.rxy_at(Rxy::Lay, r, r, i64::from(o));
                }
            }
        }
    }

    // Branches.
}
