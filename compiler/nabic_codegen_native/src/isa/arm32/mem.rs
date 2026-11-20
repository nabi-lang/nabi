//! Loads, stores and register moves.

#[allow(clippy::wildcard_imports)]
use crate::isa::arm32::*;

impl Em<'_> {
    /// `a rt, [rn, #off]`; an offset out of reach goes into `tmp` (not
    /// `rt` for a store) with the base first.
    pub(crate) fn ls(&mut self, a: Acc, rt: u32, rn: u32, off: i32, tmp: u32) {
        let (rn, off) = if off.unsigned_abs() > a.max() {
            self.add_imm(tmp, rn, off);
            (tmp, 0)
        } else {
            (rn, off)
        };
        self.ins(enc::ls(a, R(rt), R(rn), off));
    }

    /// `(base, offset)` of `m`; an indirect pointer is loaded into `tmp`.
    pub(crate) fn mem(&mut self, m: Mem, tmp: u32) -> (u32, i32) {
        debug_assert!(!self.ra.held(m), "a register part in memory");
        match m {
            Mem::Frame(s, o) => (SP, (self.slot_off[s as usize] + o) as i32),
            Mem::Ind(s, o) => {
                if let Some(r) = self.ra.base(m) {
                    return (r, o);
                }
                let so = self.slot_off[s as usize] as i32;
                self.ls(Acc::Ldr, tmp, SP, so, tmp);
                (tmp, o)
            }
        }
    }

    /// The integer register holding `m`, if allocated.
    pub(crate) fn ireg(&self, m: Mem) -> Option<u32> {
        self.ra.get(m).filter(|a| !a.fp).map(|a| a.reg)
    }

    /// The `d` register holding `m`, if allocated.
    pub(crate) fn dreg(&self, m: Mem) -> Option<u32> {
        self.ra.get(m).filter(|a| a.fp).map(|a| a.reg)
    }

    pub(crate) fn ireg_v(&self, v: V) -> Option<u32> {
        self.ra.get_v(v).filter(|a| !a.fp).map(|a| a.reg)
    }

    pub(crate) fn dreg_v(&self, v: V) -> Option<u32> {
        self.ra.get_v(v).filter(|a| a.fp).map(|a| a.reg)
    }

    /// A register holding `v` as `k` extended by `signed`: its own where
    /// that is already so, else `r` loaded.
    pub(crate) fn src(&mut self, r: u32, v: V, k: K, signed: bool) -> u32 {
        if let Some(d) = self.ireg_v(v).filter(|_| !(signed && k.bits() < 32)) {
            return d;
        }
        self.ld(r, v, k, signed);
        r
    }

    /// Where an integer result for `m` is computed: its register or `r4`.
    pub(crate) fn dst_r(&self, m: Mem) -> u32 {
        self.ireg(m).unwrap_or(self.x)
    }

    /// Complete a result computed by `dst_r`: store it, or zero-extend a
    /// narrow one in its register.
    pub(crate) fn put(&mut self, d: u32, m: Mem, k: K) {
        if self.ireg(m).is_some() {
            if k.bits() < 32 {
                self.extend_reg(d, d, k, false);
            }
        } else {
            self.st(d, m, k);
        }
    }

    /// `vmov.32 r, d[0]` / `vmov.32 d[0], r`.
    pub(crate) fn vmov_lane(&mut self, d: u32, r: u32, to_core: bool) {
        self.ins(enc::vmov_lane(to_core, D(d), R(r)));
    }

    /// `vmov.f64 d, m`.
    pub(crate) fn vmov_d(&mut self, d: u32, m: u32) {
        if d != m {
            self.vfp(Vop::Mov, K::F64, d, 0, m);
        }
    }

    /// A VFP register holding `v` (`k`): its own `d` for an allocated
    /// `f64`, else scratch `i` loaded.
    pub(crate) fn fsrc(&mut self, v: V, k: K, i: u32) -> u32 {
        if let Some(n) = self.dreg_v(v) {
            if k == K::F64 {
                return n;
            }
            self.vmov_d(i, n);
            return Self::fr(i, k);
        }
        let x = Self::fr(i, k);
        self.ldf(x, v, k);
        x
    }

    /// The `d` register an `f64` result for `m` can be computed into.
    pub(crate) fn dst_d(&self, m: Mem, k: K) -> Option<u32> {
        if k == K::F64 { self.dreg(m) } else { None }
    }

    /// `(base, offset)` of `m` with `off + 4` in a word access's reach.
    pub(crate) fn mem2(&mut self, m: Mem, tmp: u32) -> (u32, i32) {
        let (b, o) = self.mem(m, tmp);
        if o.unsigned_abs() + 4 > 4095 {
            self.add_imm(tmp, b, o);
            return (tmp, 0);
        }
        (b, o)
    }

    /// `r` = `v` as `k` (the low word of an 8-byte one), extended by
    /// `signed`.
    pub(crate) fn ld(&mut self, r: u32, v: V, k: K, signed: bool) {
        let signed = signed && !k.is_float();
        match v {
            V::I(i) => self.mov_imm(r, ext32(i, k, signed)),
            V::M(m) => {
                if let Some((lo, _)) = self.ra.pair(m) {
                    if signed && k.bits() < 32 {
                        self.extend_reg(r, lo, k, true);
                    } else {
                        self.mov(r, lo);
                    }
                    return;
                }
                if let Some(a) = self.ra.get(m) {
                    if a.fp {
                        self.vmov_lane(a.reg, r, true);
                    } else if signed && k.bits() < 32 {
                        self.extend_reg(r, a.reg, k, true);
                    } else {
                        self.mov(r, a.reg);
                    }
                    return;
                }
                let (b, o) = self.mem(m, r);
                self.ls(Acc::load(k, signed), r, b, o, r);
            }
        }
    }

    /// `(lo, hi)` = the 8 bytes of `v`.
    pub(crate) fn ld2(&mut self, lo: u32, hi: u32, v: V) {
        match v {
            V::I(i) => {
                self.mov_imm(lo, i as u32);
                self.mov_imm(hi, (i >> 32) as u32);
            }
            V::M(m) => {
                if let Some((l, h)) = self.ra.pair(m) {
                    self.mov2(lo, hi, l, h);
                    return;
                }
                if let Some(n) = self.dreg(m) {
                    self.vmov_dr(n, lo, hi, true);
                    return;
                }
                let (b, o) = self.mem2(m, hi);
                self.ls(Acc::Ldr, lo, b, o, hi);
                self.ls(Acc::Ldr, hi, b, o + 4, hi);
            }
        }
    }

    /// Registers holding the 8 bytes of `v`: its pair, else `(lo, hi)`
    /// loaded.
    pub(crate) fn src2(&mut self, lo: u32, hi: u32, v: V) -> (u32, u32) {
        if let Some(p) = self.ra.pair_v(v) {
            return p;
        }
        self.ld2(lo, hi, v);
        (lo, hi)
    }

    /// Where an 8-byte result for `m` is computed: its pair or `r4 r5`.
    pub(crate) fn dst2(&self, m: Mem) -> (u32, u32) {
        self.ra.pair(m).unwrap_or((self.x, self.xh))
    }

    /// Complete a result computed by `dst2`.
    pub(crate) fn put2(&mut self, d: (u32, u32), m: Mem) {
        if self.ra.pair(m).is_none() {
            self.st2(d.0, d.1, m);
        }
    }

    /// Store `k` (1 / 2 / 4 bytes) of `r` (never `r8`) at `m`.
    pub(crate) fn st(&mut self, r: u32, m: Mem, k: K) {
        if let Some(a) = self.ra.get(m) {
            if a.fp {
                self.vmov_lane(a.reg, r, false);
            } else if k.bits() < 32 {
                self.extend_reg(a.reg, r, k, false);
            } else {
                self.mov(a.reg, r);
            }
            return;
        }
        let (b, o) = self.mem(m, R8);
        self.ls(Acc::store(k), r, b, o, R8);
    }

    /// Store the pair `(lo, hi)` (neither `r8`) at `m`.
    pub(crate) fn st2(&mut self, lo: u32, hi: u32, m: Mem) {
        if let Some((l, h)) = self.ra.pair(m) {
            self.mov2(l, h, lo, hi);
            return;
        }
        if let Some(n) = self.dreg(m) {
            self.vmov_dr(n, lo, hi, false);
            return;
        }
        let t = R8;
        let (b, o) = self.mem2(m, t);
        self.ls(Acc::Str, lo, b, o, t);
        self.ls(Acc::Str, hi, b, o + 4, t);
    }

    /// `vmov s, r` / `vmov r, s`.
    pub(crate) fn vmov_sr(&mut self, s: u32, r: u32, to_core: bool) {
        self.ins(enc::vmov_s(to_core, S(s), R(r)));
    }

    /// `vmov d, lo, hi` / `vmov lo, hi, d`.
    pub(crate) fn vmov_dr(&mut self, d: u32, lo: u32, hi: u32, to_core: bool) {
        self.ins(enc::vmov_d(to_core, D(d), R(lo), R(hi)));
    }

    /// VFP register `f` (`s` for an `f32`, `d` for an `f64`) = `v`.
    pub(crate) fn ldf(&mut self, f: u32, v: V, k: K) {
        if let Some(n) = self.dreg_v(v) {
            if k == K::F64 {
                self.vmov_d(f, n);
            } else {
                self.vmov_lane(n, IP, true);
                self.vmov_sr(f, IP, false);
            }
            return;
        }
        if k == K::F32 {
            self.ld(IP, v, K::I32, false);
            self.vmov_sr(f, IP, false);
        } else {
            self.ld2(IP, LR, v);
            self.vmov_dr(f, IP, LR, false);
        }
    }

    /// Store VFP register `f` (`k`) at `m`.
    pub(crate) fn stf(&mut self, f: u32, m: Mem, k: K) {
        if let Some(n) = self.dreg(m) {
            if k == K::F64 {
                self.vmov_d(n, f);
            } else if f % 2 == 0 {
                self.vmov_d(n, f / 2);
            } else {
                self.vmov_sr(f, IP, true);
                self.vmov_lane(n, IP, false);
            }
            return;
        }
        if k == K::F32 {
            self.vmov_sr(f, IP, true);
            self.st(IP, m, K::I32);
        } else {
            self.vmov_dr(f, IP, LR, true);
            self.st2(IP, LR, m);
        }
    }

    /// A VFP data-processing instruction `op` (single-precision encoding)
    /// on `k` registers `d`, `n`, `m`.
    pub(crate) fn vfp(&mut self, op: Vop, k: K, d: u32, n: u32, m: u32) {
        let v = enc::V;
        self.ins(enc::vfp(op, k == K::F64, v(d), v(n), v(m)));
    }

    /// Scratch VFP register `i` (0..3) for `k`: `s0 s2 s4` / `d0 d1 d2`.
    pub(crate) const fn fr(i: u32, k: K) -> u32 {
        if matches!(k, K::F32) { 2 * i } else { i }
    }

    /// `r` = the address of `m`.
    pub(crate) fn lea(&mut self, r: u32, m: Mem) {
        match m {
            Mem::Frame(s, o) => {
                let off = (self.slot_off[s as usize] + o) as i32;
                self.add_imm(r, SP, off);
            }
            Mem::Ind(s, o) => {
                if let Some(b) = self.ra.base(m) {
                    self.add_imm(r, b, o);
                    return;
                }
                let so = self.slot_off[s as usize] as i32;
                self.ls(Acc::Ldr, r, SP, so, r);
                self.add_imm(r, r, o);
            }
        }
    }
}
