//! Loads, stores and register moves.

#[allow(clippy::wildcard_imports)]
use crate::isa::ppc64::*;

impl Em<'_> {
    /// A load (`load`) or store of `k` (`signed`: an extending integer
    /// load; `fp`: a float register), `rt` at `[base + off]`; a far or
    /// (DS-form) unaligned `off` indexed by `r0`.
    #[allow(clippy::fn_params_excessive_bools, clippy::too_many_arguments)]
    pub(crate) fn ls(
        &mut self,
        load: bool,
        k: K,
        signed: bool,
        fp: bool,
        rt: u32,
        base: u32,
        off: i64,
    ) {
        let op = match (load, fp, k.size(), signed) {
            (true, true, 4, _) => Ls::Lfs,
            (true, true, _, _) => Ls::Lfd,
            (false, true, 4, _) => Ls::Stfs,
            (false, true, _, _) => Ls::Stfd,
            (true, _, 1, _) => Ls::Lbz,
            (true, _, 2, true) => Ls::Lha,
            (true, _, 2, false) => Ls::Lhz,
            (true, _, 4, true) => Ls::Lwa,
            (true, _, 4, false) => Ls::Lwz,
            (true, _, _, _) => Ls::Ld,
            (false, _, 1, _) => Ls::Stb,
            (false, _, 2, _) => Ls::Sth,
            (false, _, 4, _) => Ls::Stw,
            (false, _, _, _) => Ls::Std,
        };
        if fits16(off) && (!op.ds() || off % 4 == 0) {
            self.ins(enc::ls(op, rt, R(base), off));
        } else {
            self.li(R0, off);
            self.ins(enc::ls_x(op, rt, R(base), R(R0)));
        }
        if load && !fp && k.size() == 1 && signed {
            self.logic(Logic::Extsb, rt, rt, 0);
        }
    }

    /// Load the doubleword at `[base + off]` into `rd`.
    pub(crate) fn ld_word(&mut self, rd: u32, base: u32, off: i64) {
        self.ls(true, K::I64, false, false, rd, base, off);
    }

    pub(crate) fn st_word(&mut self, rs: u32, base: u32, off: i64) {
        self.ls(false, K::I64, false, false, rs, base, off);
    }

    /// `(base, offset)` of `m`; an indirect pointer is loaded into `tmp`.
    pub(crate) fn mem(&mut self, m: Mem, tmp: u32) -> (u32, i64) {
        debug_assert!(
            self.ra.get(m).is_none(),
            "powerpc64: an allocated part in memory"
        );
        if let Some(r) = self.ra.base(m) {
            let Mem::Ind(_, o) = m else { unreachable!() };
            return (r, i64::from(o));
        }
        match m {
            Mem::Frame(s, o) => (SP, i64::from(self.slot_off[s as usize] + o)),
            Mem::Ind(s, o) => {
                let so = i64::from(self.slot_off[s as usize]);
                self.ld_word(tmp, SP, so);
                (tmp, i64::from(o))
            }
        }
    }

    /// The integer register `m` is allocated to, if any.
    pub(crate) fn dst_r(&self, m: Mem) -> Option<u32> {
        self.ra.get(m).filter(|a| !a.fp).map(|a| a.reg)
    }

    /// The float register `m` is allocated to, if any.
    pub(crate) fn dst_f(&self, m: Mem) -> Option<u32> {
        self.ra.get(m).filter(|a| a.fp).map(|a| a.reg)
    }

    /// The register holding `v` as `k` extended by `signed`: its own when
    /// allocated and already so, else `tmp` loaded.
    pub(crate) fn src(&mut self, v: V, k: K, signed: bool, tmp: u32) -> u32 {
        match self.ra.get_v(v) {
            Some(a) if !a.fp && (!signed || int_k(k).bits() >= 64) => return a.reg,
            _ => {}
        }
        self.ld(tmp, v, k, signed);
        tmp
    }

    /// The float register holding `v`: its own, else `tmp` loaded.
    pub(crate) fn fsrc(&mut self, v: V, k: K, tmp: u32) -> u32 {
        match self.ra.get_v(v) {
            Some(a) if a.fp => return a.reg,
            _ => {}
        }
        self.ldf(tmp, v, k);
        tmp
    }

    /// `fmr fd, fs`.
    pub(crate) fn fmr(&mut self, fd: u32, fs: u32) {
        if fd != fs {
            self.fop(K::F64, Fp::Fmr, fd, 0, fs);
        }
    }

    /// `r` = `v` as `k`, extended by `signed` to 64 bits.
    pub(crate) fn ld(&mut self, r: u32, v: V, k: K, signed: bool) {
        let k = int_k(k);
        match v {
            V::I(i) => self.li(r, ext(i, k, signed)),
            V::M(m) => match self.ra.get(m) {
                Some(a) if !a.fp => {
                    if signed {
                        self.ext_reg(r, a.reg, k.bits(), true);
                    } else {
                        self.mr(r, a.reg);
                    }
                }
                Some(a) => {
                    // The bits of a float register, through the scratch.
                    let s = self.scratch;
                    self.ls(false, float_k(k), false, true, a.reg, SP, s);
                    self.ls(true, k, signed, false, r, SP, s);
                }
                None => {
                    let (b, o) = self.mem(m, r);
                    self.ls(true, k, signed, false, r, b, o);
                }
            },
        }
    }

    /// Store `k` of `r` at `m` (`r11`, else `r12`, the base).
    pub(crate) fn st(&mut self, r: u32, m: Mem, k: K) {
        self.put(r, m, k, false);
    }

    /// `st`; `clean`: `r` already holds `k` zero-extended.
    pub(crate) fn put(&mut self, r: u32, m: Mem, k: K, clean: bool) {
        let k = int_k(k);
        match self.ra.get(m) {
            Some(a) if !a.fp => {
                if clean || k.bits() >= 64 {
                    self.mr(a.reg, r);
                } else {
                    self.clrldi(a.reg, r, k.bits());
                }
            }
            Some(a) => {
                let s = self.scratch;
                self.ls(false, k, false, false, r, SP, s);
                self.ls(true, float_k(k), false, true, a.reg, SP, s);
            }
            None => {
                let tmp = if r == R11 { R12 } else { R11 };
                let (b, o) = self.mem(m, tmp);
                self.ls(false, k, false, false, r, b, o);
            }
        }
    }

    /// Float register `f` = `v` (an `f32` as a double).
    pub(crate) fn ldf(&mut self, f: u32, v: V, k: K) {
        if let Some(a) = self.ra.get_v(v) {
            if a.fp {
                self.fmr(f, a.reg);
            } else {
                let s = self.scratch;
                self.ls(false, int_k(k), false, false, a.reg, SP, s);
                self.ls(true, k, false, true, f, SP, s);
            }
            return;
        }
        match v {
            V::I(i) => {
                // The bits through the scratch bytes.
                self.li(R11, i);
                let s = self.scratch;
                self.ls(false, int_k(k), false, false, R11, SP, s);
                self.ls(true, k, false, true, f, SP, s);
            }
            V::M(m) => {
                let (b, o) = self.mem(m, R11);
                self.ls(true, k, false, true, f, b, o);
            }
        }
    }

    pub(crate) fn stf(&mut self, f: u32, m: Mem, k: K) {
        match self.ra.get(m) {
            Some(a) if a.fp => return self.fmr(a.reg, f),
            Some(a) => {
                let s = self.scratch;
                self.ls(false, k, false, true, f, SP, s);
                self.ls(true, int_k(k), false, false, a.reg, SP, s);
                return;
            }
            None => {}
        }
        let (b, o) = self.mem(m, R11);
        self.ls(false, k, false, true, f, b, o);
    }

    /// A float A / X-form instruction `xo frt, fra, frb` of `k`'s
    /// precision.
    pub(crate) fn fop(&mut self, k: K, op: Fp, frt: u32, fra: u32, frb: u32) {
        self.ins(enc::fp(op, k == K::F32, F(frt), F(fra), F(frb)));
    }

    /// `fmul(s) frt, fra, frc`.
    pub(crate) fn fmul(&mut self, k: K, frt: u32, fra: u32, frc: u32) {
        self.ins(enc::fmul(k == K::F32, F(frt), F(fra), F(frc)));
    }

    /// `fcmpu cr0, fra, frb`.
    pub(crate) fn fcmpu(&mut self, fra: u32, frb: u32) {
        self.ins(enc::fcmpu(F(fra), F(frb)));
    }

    /// Float register `f` = the 8 bytes of `r` (through the scratch).
    pub(crate) fn gpr_to_fpr(&mut self, f: u32, r: u32) {
        let s = self.scratch;
        self.st_word(r, SP, s);
        self.ls(true, K::F64, false, true, f, SP, s);
    }

    /// `r` = the 8 bytes of float register `f`.
    pub(crate) fn fpr_to_gpr(&mut self, r: u32, f: u32) {
        let s = self.scratch;
        self.ls(false, K::F64, false, true, f, SP, s);
        self.ld_word(r, SP, s);
    }

    /// `r` = the address of `m`.
    pub(crate) fn lea(&mut self, r: u32, m: Mem) {
        if let Some(b) = self.ra.base(m) {
            let Mem::Ind(_, o) = m else { unreachable!() };
            self.add_imm(r, b, i64::from(o));
            return;
        }
        match m {
            Mem::Frame(s, o) => {
                let off = i64::from(self.slot_off[s as usize] + o);
                self.add_imm(r, SP, off);
            }
            Mem::Ind(s, o) => {
                let so = i64::from(self.slot_off[s as usize]);
                self.ld_word(r, SP, so);
                self.add_imm(r, r, i64::from(o));
            }
        }
    }

    // Comparisons (`cr0`) and branches.
}
