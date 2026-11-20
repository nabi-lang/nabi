//! Loads, stores and register moves.

#[allow(clippy::wildcard_imports)]
use crate::isa::riscv::*;

impl Em<'_> {
    /// Load or store `o` of `rt` at `[base + off]`; a far `off` through
    /// `t6`.
    pub(crate) fn ls(&mut self, o: Ls, rt: u32, base: u32, off: i64) {
        let (base, off) = if fits12(off) {
            (base, off)
        } else {
            let (hi, lo) = enc::hi_lo(off);
            self.lui(T6, hi);
            self.alu(ADD, T6, T6, base);
            (T6, i64::from(lo))
        };
        self.ins(enc::ldst(o, rt, X(base), off as i32));
    }

    /// The load of `k`, extended by `signed` (rv64 `I32`: `lw` /
    /// `lwu`).
    pub(crate) const fn load_f3(&self, k: K, signed: bool) -> Ls {
        match (int_k(k), signed) {
            (K::I8, true) => Ls::Lb,
            (K::I8, false) => Ls::Lbu,
            (K::I16, true) => Ls::Lh,
            (K::I16, false) => Ls::Lhu,
            (K::I32, false) if !self.rv32 => Ls::Lwu,
            (K::I32, _) => Ls::Lw,
            _ if self.rv32 => Ls::Lw,
            _ => Ls::Ld,
        }
    }

    pub(crate) const fn store_f3(k: K) -> Ls {
        match int_k(k) {
            K::I8 => Ls::Sb,
            K::I16 => Ls::Sh,
            K::I32 => Ls::Sw,
            _ => Ls::Sd,
        }
    }

    /// Load the word at `[base + off]` into `rd`.
    pub(crate) fn ld_word(&mut self, rd: u32, base: u32, off: i64) {
        let o = if self.rv32 { Ls::Lw } else { Ls::Ld };
        self.ls(o, rd, base, off);
    }

    pub(crate) fn st_word(&mut self, rs: u32, base: u32, off: i64) {
        let o = if self.rv32 { Ls::Sw } else { Ls::Sd };
        self.ls(o, rs, base, off);
    }

    /// `(base, offset)` of `m`; an indirect pointer is loaded into `tmp`.
    pub(crate) fn mem(&mut self, m: Mem, tmp: u32) -> (u32, i64) {
        debug_assert!(!self.ra.held(m), "a register part in memory");
        match m {
            Mem::Frame(s, o) => (SP, i64::from(self.slot_off[s as usize] + o)),
            Mem::Ind(s, o) => {
                if let Some(b) = self.ra.base(m) {
                    return (b, i64::from(o));
                }
                let so = i64::from(self.slot_off[s as usize]);
                self.ld_word(tmp, SP, so);
                (tmp, i64::from(o))
            }
        }
    }

    /// The integer register `m` is allocated to.
    pub(crate) fn dst_x(&self, m: Mem) -> Option<u32> {
        self.ra.get(m).filter(|a| !a.fp).map(|a| a.reg)
    }

    /// The float register `m` is allocated to.
    pub(crate) fn dst_f(&self, m: Mem) -> Option<u32> {
        self.ra.get(m).filter(|a| a.fp).map(|a| a.reg)
    }

    /// `fmv.x.w` / `fmv.x.d` (`to_int`), or `fmv.w.x` / `fmv.d.x`.
    pub(crate) fn fmv(&mut self, to_int: bool, size: u32, rd: u32, rs: u32) {
        self.ins(if to_int {
            enc::fmv_x(size == 8, X(rd), enc::F(rs))
        } else {
            enc::fmv_f(size == 8, enc::F(rd), X(rs))
        });
    }

    /// Float register move (`fsgnj`).
    pub(crate) fn fmov(&mut self, k: K, fd: u32, fs: u32) {
        if fd != fs {
            let (d, s) = (enc::F(fd), enc::F(fs));
            self.ins(enc::fsgnj(false, k == K::F64, d, s, s));
        }
    }

    /// Zero-extend the low `bits` of `rs` into `rd`.
    pub(crate) fn zext(&mut self, rd: u32, rs: u32, bits: u32) {
        if bits >= self.xlen() {
            self.mv(rd, rs);
        } else if bits == 8 {
            self.andi(rd, rs, 0xff);
        } else {
            self.ext_reg(rd, rs, bits, false);
        }
    }

    /// A register holding `v` as `k` extended by `signed`: its own, else
    /// `tmp` loaded.
    pub(crate) fn reg(&mut self, v: V, k: K, signed: bool, tmp: u32) -> u32 {
        if let V::M(m) = v {
            if let Some(r) = self.dst_x(m) {
                if !signed || int_k(k).bits() >= self.xlen() {
                    return r;
                }
            }
        }
        self.ld(tmp, v, k, signed);
        tmp
    }

    /// `reg` for a float (rv64).
    pub(crate) fn freg(&mut self, v: V, k: K, tmp: u32) -> u32 {
        if let V::M(m) = v {
            if let Some(r) = self.dst_f(m) {
                return r;
            }
        }
        self.ldf(tmp, v, k);
        tmp
    }

    /// `r` = `v` as `k` (rv32: the low word of an 8-byte one), extended
    /// by `signed`.
    pub(crate) fn ld(&mut self, r: u32, v: V, k: K, signed: bool) {
        let k = int_k(k);
        if let Some((lo, _)) = self.ra.pair_v(v) {
            let bits = k.bits();
            if bits >= self.xlen() || !signed {
                self.mv(r, lo);
            } else {
                self.ext_reg(r, lo, bits, true);
            }
            return;
        }
        if let Some(a) = self.ra.get_v(v) {
            let bits = k.bits();
            if a.fp {
                self.fmv(true, k.size(), r, a.reg);
                if bits < self.xlen() && !signed {
                    self.zext(r, r, bits);
                }
            } else if bits >= self.xlen() || !signed {
                self.mv(r, a.reg);
            } else if bits == 32 {
                self.ins(enc::addiw(X(r), X(a.reg), 0)); // sext.w
            } else {
                self.ext_reg(r, a.reg, bits, true);
            }
            return;
        }
        match v {
            V::I(i) => self.li(r, ext(i, k, signed)),
            V::M(m) => {
                let (b, o) = self.mem(m, r);
                let f3 = self.load_f3(k, signed);
                self.ls(f3, r, b, o);
            }
        }
    }

    /// `(lo, hi)` = the 8 bytes of `v` (rv32).
    pub(crate) fn ld2(&mut self, lo: u32, hi: u32, v: V) {
        if let Some((l, h)) = self.ra.pair_v(v) {
            self.mv2(lo, hi, l, h);
            return;
        }
        match v {
            V::I(i) => {
                self.li(lo, i);
                self.li(hi, i >> 32);
            }
            V::M(m) => {
                let (b, o) = self.mem(m, hi);
                self.ls(Ls::Lw, lo, b, o);
                self.ls(Ls::Lw, hi, b, o + 4);
            }
        }
    }

    /// `(lo, hi)` = `(l, h)`, whichever registers they share (`t6`
    /// scratch).
    pub(crate) fn mv2(&mut self, lo: u32, hi: u32, l: u32, h: u32) {
        if lo == h && hi == l {
            self.mv(T6, l);
            self.mv(lo, h);
            self.mv(hi, T6);
        } else if lo == h {
            self.mv(hi, h);
            self.mv(lo, l);
        } else {
            self.mv(lo, l);
            self.mv(hi, h);
        }
    }

    /// Registers holding the 8 bytes of `v` (rv32): its pair, else
    /// `(lo, hi)` loaded.
    pub(crate) fn src2(&mut self, lo: u32, hi: u32, v: V) -> (u32, u32) {
        if let Some(p) = self.ra.pair_v(v) {
            return p;
        }
        self.ld2(lo, hi, v);
        (lo, hi)
    }

    /// Where an 8-byte result for `m` is computed (rv32): its pair or
    /// `t0 t1`.
    pub(crate) fn dst2(&self, m: Mem) -> (u32, u32) {
        self.ra.pair(m).unwrap_or((T0, T1))
    }

    /// Complete a result computed by `dst2`.
    pub(crate) fn put2(&mut self, d: (u32, u32), m: Mem) {
        if self.ra.pair(m).is_none() {
            self.st2(d.0, d.1, m);
        }
    }

    /// Store `k` of `r` at `m` (`t5` the base).
    pub(crate) fn st(&mut self, r: u32, m: Mem, k: K) {
        self.st_ext(r, m, k, false);
    }

    /// `st` of an `r` known zero-extended from `k` (`zx`) or not.
    pub(crate) fn st_ext(&mut self, r: u32, m: Mem, k: K, zx: bool) {
        if let Some(a) = self.ra.get(m) {
            let bits = int_k(k).bits();
            if a.fp {
                self.fmv(false, k.size(), a.reg, r);
            } else if zx {
                self.mv(a.reg, r);
            } else {
                self.zext(a.reg, r, bits);
            }
            return;
        }
        let (b, o) = self.mem(m, T5);
        self.ls(Self::store_f3(k), r, b, o);
    }

    pub(crate) fn st2(&mut self, lo: u32, hi: u32, m: Mem) {
        if let Some((l, h)) = self.ra.pair(m) {
            self.mv2(l, h, lo, hi);
            return;
        }
        let (b, o) = self.mem(m, T5);
        self.ls(Ls::Sw, lo, b, o);
        self.ls(Ls::Sw, hi, b, o + 4);
    }

    /// Float register `f` = `v` (rv64).
    pub(crate) fn ldf(&mut self, f: u32, v: V, k: K) {
        let dbl = k == K::F64;
        if let Some(a) = self.ra.get_v(v) {
            if a.fp {
                self.fmov(k, f, a.reg);
            } else {
                self.fmv(false, k.size(), f, a.reg);
            }
            return;
        }
        match v {
            V::I(i) => {
                self.li(T6, i);
                self.ins(enc::fmv_f(dbl, enc::F(f), X(T6)));
            }
            V::M(m) => {
                let (b, o) = self.mem(m, T5);
                self.ls(Ls::Fl(dbl), f, b, o);
            }
        }
    }

    pub(crate) fn stf(&mut self, f: u32, m: Mem, k: K) {
        if let Some(a) = self.ra.get(m) {
            if a.fp {
                self.fmov(k, a.reg, f);
            } else {
                self.fmv(true, k.size(), a.reg, f);
                self.zext(a.reg, a.reg, k.bits());
            }
            return;
        }
        let (b, o) = self.mem(m, T5);
        self.ls(Ls::Fs(k == K::F64), f, b, o);
    }

    /// `r` = the address of `m`.
    pub(crate) fn lea(&mut self, r: u32, m: Mem) {
        match m {
            Mem::Frame(s, o) => {
                let off = i64::from(self.slot_off[s as usize] + o);
                self.add_imm(r, SP, off, T6);
            }
            Mem::Ind(s, o) => {
                if let Some(b) = self.ra.base(m) {
                    self.add_imm(r, b, i64::from(o), T6);
                    return;
                }
                let so = i64::from(self.slot_off[s as usize]);
                self.ld_word(r, SP, so);
                self.add_imm(r, r, i64::from(o), T6);
            }
        }
    }

    // Branches.
}
