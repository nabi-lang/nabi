//! Loads, stores and register moves.

#[allow(clippy::wildcard_imports)]
use crate::isa::i386::*;

impl Em<'_> {
    /// `[esp + disp]`.
    pub(crate) const fn sp(disp: u32) -> Addr {
        bd(ESP, disp as i32)
    }

    /// Frame byte `disp` (from `esp`): above `-O0` from `ebp` (`esp + 12 +
    /// frame` past the prologue) when that encodes shorter.
    pub(crate) fn slot(&self, disp: u32) -> Addr {
        let d = disp as i32;
        let fp = d - (self.frame + SAVED - 4) as i32;
        let short = |x: i32| (-128..128).contains(&x);
        if self.opt && (short(fp) || (d != 0 && !short(d))) {
            bd(EBP, fp)
        } else {
            Self::sp(disp)
        }
    }

    /// `[base + disp]` of `m`; an indirect pointer is loaded into `base`.
    pub(crate) fn mem(&mut self, m: Mem, base: u32) -> Addr {
        debug_assert!(!self.ra.held(m), "a register part in memory");
        match m {
            Mem::Frame(s, o) => self.slot(self.slot_off[s as usize] + o),
            Mem::Ind(s, o) => {
                if let Some(r) = self.ra.base(m) {
                    return bd(r, o);
                }
                let so = self.slot_off[s as usize];
                self.a.mov(Sz::D, Gpr(base), Rm::M(self.slot(so)));
                bd(base, o)
            }
        }
    }

    /// `r` = the `k` at `rm`, extended by `signed` to 32 bits.
    pub(crate) fn extend(&mut self, r: u32, rm: Rm, k: K, signed: bool) {
        let d = Gpr(r);
        match (k, signed) {
            (K::I8, true) => self.a.movsx(d, Sz::B, rm),
            (K::I8, false) => self.a.movzx(d, Sz::B, rm),
            (K::I16, true) => self.a.movsx(d, Sz::W, rm),
            (K::I16, false) => self.a.movzx(d, Sz::W, rm),
            _ => self.a.mov(Sz::D, d, rm),
        }
    }

    /// `r` = `v` as `k` (the low word of an 8-byte one), extended by
    /// `signed`.
    pub(crate) fn ld(&mut self, r: u32, v: V, k: K, signed: bool) {
        let signed = signed && !k.is_float();
        if let Some((lo, _)) = self.ra.pair_v(v) {
            self.mov(r, lo);
            if signed && k.size() < 4 {
                self.extend(r, gr(r), k, true);
            }
            return;
        }
        if let Some(a) = self.ra.get_v(v) {
            if a.fp {
                self.movd_from(r, a.reg);
            } else {
                // Kept zero-extended.
                self.mov(r, a.reg);
                if signed && k.size() < 4 {
                    debug_assert!(r < 4 || k != K::I8, "a byte register");
                    self.extend(r, gr(r), k, true);
                }
            }
            return;
        }
        match v {
            V::I(i) => self.mov_imm(r, ext32(i, k, signed)),
            V::M(m) => {
                let a = self.mem(m, r);
                let k = if wide(k) { K::I32 } else { k };
                self.extend(r, Rm::M(a), k, signed);
            }
        }
    }

    /// `(lo, hi)` = the 8 bytes of `v`.
    pub(crate) fn ld2(&mut self, lo: u32, hi: u32, v: V) {
        if let Some((l, h)) = self.ra.pair_v(v) {
            self.mov2(lo, hi, l, h);
            return;
        }
        if let Some(a) = self.ra.get_v(v) {
            // Only floats are 8 bytes in a register.
            debug_assert!(a.fp);
            self.movd_from(lo, a.reg);
            // The high word low.
            self.a.sse_ri(Sse::Pshufd, Xmm(XMM2), Xmm(a.reg), 0x55);
            self.movd_from(hi, XMM2);
            return;
        }
        match v {
            V::I(i) => {
                self.mov_imm(lo, i as u32);
                self.mov_imm(hi, (i >> 32) as u32);
            }
            V::M(m) => {
                let a = self.mem(m, hi);
                self.a.mov(Sz::D, Gpr(lo), Rm::M(a));
                self.a.mov(Sz::D, Gpr(hi), Rm::M(a.plus(4)));
            }
        }
    }

    /// `(lo, hi)` = `(l, h)`, whichever registers they share.
    pub(crate) fn mov2(&mut self, lo: u32, hi: u32, l: u32, h: u32) {
        if lo == h && hi == l {
            self.a.xchg(Sz::D, gr(hi), Gpr(lo));
        } else if lo == h {
            self.mov(hi, h);
            self.mov(lo, l);
        } else {
            self.mov(lo, l);
            self.mov(hi, h);
        }
    }

    /// A register to address a store with, none of `avoid`.
    pub(crate) fn base(&self, avoid: &[u32]) -> u32 {
        // Above -O0 `esi edi` may hold values.
        let pool: &[u32] = if self.opt {
            &[ECX, EDX, EAX, EDI]
        } else {
            &[EDI, ESI, ECX, EDX, EAX]
        };
        pool.iter()
            .copied()
            .find(|r| !avoid.contains(r))
            .unwrap_or(EDI)
    }

    pub(crate) fn st_rm(&mut self, r: u32, m: Addr, k: K) {
        let sz = match k {
            K::I8 => {
                debug_assert!(r < 4, "a byte register");
                Sz::B
            }
            K::I16 => Sz::W,
            _ => Sz::D,
        };
        self.a.mov_st(sz, Rm::M(m), Gpr(r));
    }

    /// Store `k` (1 / 2 / 4 bytes) of `r` at `m`.
    pub(crate) fn st(&mut self, r: u32, m: Mem, k: K) {
        self.st_avoid(r, m, k, &[r]);
    }

    /// `st`, the address never in `avoid`.
    pub(crate) fn st_avoid(&mut self, r: u32, m: Mem, k: K, avoid: &[u32]) {
        if let Some(a) = self.ra.get(m) {
            if a.fp {
                self.movd_to(a.reg, r);
            } else {
                self.canon(a.reg, r, k);
            }
            return;
        }
        let a = self.mem(m, self.base(avoid));
        self.st_rm(r, a, k);
    }

    /// Register `d` = `r` as `k`, zero-extended.
    pub(crate) fn canon(&mut self, d: u32, r: u32, k: K) {
        match k {
            K::I8 => {
                debug_assert!(r < 4, "a byte register");
                self.a.movzx(Gpr(d), Sz::B, gr(r));
            }
            K::I16 => self.a.movzx(Gpr(d), Sz::W, gr(r)),
            _ => self.mov(d, r),
        }
    }

    /// `movd r, xmm x`.
    pub(crate) fn movd_from(&mut self, r: u32, x: u32) {
        self.a.movd_gx(Gpr(r), Xmm(x));
    }

    /// `movd xmm x, r`.
    pub(crate) fn movd_to(&mut self, x: u32, r: u32) {
        self.a.movd_xg(Xmm(x), Gpr(r));
    }

    /// `movaps x, y`.
    pub(crate) fn movaps(&mut self, x: u32, y: u32) {
        if x != y {
            self.a.sse(Sse::Movaps, Xmm(x), xr(y));
        }
    }

    /// The integer register allocated to `m`.
    pub(crate) fn dst_r(&self, m: Mem) -> Option<u32> {
        self.ra.get(m).filter(|a| !a.fp).map(|a| a.reg)
    }

    /// The `xmm` holding operand `v`.
    pub(crate) fn dst_x_v(&self, v: V) -> Option<u32> {
        self.ra.get_v(v).filter(|a| a.fp).map(|a| a.reg)
    }

    /// The `xmm` allocated to `m`.
    pub(crate) fn dst_x(&self, m: Mem) -> Option<u32> {
        self.ra.get(m).filter(|a| a.fp).map(|a| a.reg)
    }

    /// An `xmm` holding the float `v`: its own, else `tmp` loaded.
    pub(crate) fn freg(&mut self, v: V, k: K, tmp: u32) -> u32 {
        if let Some(x) = self.ra.get_v(v).filter(|a| a.fp) {
            return x.reg;
        }
        self.ldf(tmp, v, k);
        tmp
    }

    /// The float `v` as an SSE operand: its `xmm`, its memory, else `tmp`
    /// loaded.
    pub(crate) fn frm(&mut self, v: V, k: K, tmp: u32) -> Xrm {
        match v {
            V::M(m) if self.ra.get(m).is_none() => Xrm::M(self.mem(m, ECX)),
            _ => xr(self.freg(v, k, tmp)),
        }
    }

    /// Store the pair `(lo, hi)` at `m`.
    pub(crate) fn st2(&mut self, lo: u32, hi: u32, m: Mem) {
        self.st2_avoid(lo, hi, m, &[lo, hi]);
    }

    pub(crate) fn st2_avoid(&mut self, lo: u32, hi: u32, m: Mem, avoid: &[u32]) {
        if let Some((l, h)) = self.ra.pair(m) {
            self.mov2(l, h, lo, hi);
            return;
        }
        if let Some(x) = self.dst_x(m) {
            self.movd_to(x, lo);
            self.movd_to(XMM2, hi);
            self.a.sse(Sse::Punpckldq, Xmm(x), xr(XMM2));
            return;
        }
        let a = self.mem(m, self.base(avoid));
        self.a.mov_st(Sz::D, Rm::M(a), Gpr(lo));
        self.a.mov_st(Sz::D, Rm::M(a.plus(4)), Gpr(hi));
    }

    /// `xmm x` = the float `v`.
    pub(crate) fn ldf(&mut self, x: u32, v: V, k: K) {
        if let Some(a) = self.ra.get_v(v) {
            if a.fp {
                self.movaps(x, a.reg);
            } else {
                self.movd_to(x, a.reg);
            }
            return;
        }
        let m = match v {
            V::M(m) => self.mem(m, ECX),
            V::I(bits) => {
                self.a.mov_mi(Rm::M(Self::sp(0)), bits as u32);
                if k == K::F64 {
                    self.a.mov_mi(Rm::M(Self::sp(4)), (bits >> 32) as u32);
                }
                Self::sp(0)
            }
        };
        self.a.sse(movs(k), Xmm(x), Xrm::M(m));
    }

    /// Store `xmm x` (`k`) at `m`.
    pub(crate) fn stf(&mut self, x: u32, m: Mem, k: K) {
        if let Some(a) = self.ra.get(m) {
            if a.fp {
                self.movaps(a.reg, x);
            } else {
                self.movd_from(a.reg, x);
            }
            return;
        }
        let a = self.mem(m, if self.opt { ECX } else { EDI });
        self.a.sse_st(movs_st(k), a, Xmm(x));
    }

    /// `r` = the address of `m`.
    pub(crate) fn lea(&mut self, r: u32, m: Mem) {
        let a = self.mem(m, r);
        if !matches!(a, Addr::Bd(b, 0) if b.0 == r) {
            self.a.lea(Gpr(r), a);
        }
    }
}
