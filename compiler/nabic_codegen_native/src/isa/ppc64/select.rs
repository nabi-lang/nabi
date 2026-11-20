//! Instruction selection, branches, comparisons and atomics.

#[allow(clippy::wildcard_imports)]
use crate::isa::ppc64::*;

impl Em<'_> {
    /// `cmpd` / `cmpld` `ra, rb`.
    pub(crate) fn cmp_reg(&mut self, signed: bool, ra: u32, rb: u32) {
        self.ins(enc::cmp(signed, R(ra), R(rb)));
    }

    /// `cmpdi ra, imm`.
    pub(crate) fn cmp_imm(&mut self, ra: u32, imm: i64) {
        self.ins(enc::cmpdi(R(ra), imm));
    }

    /// `cmpldi ra, imm`.
    pub(crate) fn cmpl_imm(&mut self, ra: u32, imm: i64) {
        self.ins(enc::cmpldi(R(ra), imm));
    }

    /// `r` = bit `bit` of `cr0`.
    pub(crate) fn cr_bit(&mut self, r: u32, bit: u32) {
        self.ins(enc::mfcr(R(r)));
        self.ins(enc::rlwinm(R(r), R(r), (bit + 1) & 31, 31, 31));
    }

    /// `cror bt, ba, bb`.
    pub(crate) fn cror(&mut self, bt: u32, ba: u32, bb: u32) {
        self.ins(enc::cror(bt, ba, bb));
    }

    /// `b l`.
    pub(crate) fn j(&mut self, l: Label) {
        self.fixes.push((self.pos(), l, false));
        self.ins(enc::b(0));
    }

    /// To `l` on `c`: a `bc` (`short`), else the inverted branch over a
    /// `b`.
    pub(crate) fn br(&mut self, c: Cond, l: Label) {
        if self.short {
            self.fixes.push((self.pos(), l, true));
            self.ins(enc::bc(c, 0));
            return;
        }
        self.ins(enc::bc(c.inv(), 8));
        self.j(l);
    }

    /// A short forward branch on `c` to a spot not known yet: its index.
    pub(crate) fn b_fwd(&mut self, c: Cond) -> usize {
        self.ins(enc::bc(c, 0));
        self.pos() - 1
    }

    /// Point the short branch at `at` here.
    pub(crate) fn here(&mut self, at: usize) {
        let d = ((self.pos() - at) * 4) as u32;
        self.a.patch_bc(at, d);
    }

    /// A short branch on `c` back to index `top`.
    pub(crate) fn b_back(&mut self, c: Cond, top: usize) {
        let d = (top as i64 - self.pos() as i64) * 4;
        self.ins(enc::bc(c, d));
    }

    pub(crate) fn inst(&mut self, i: &Inst, next: Option<&Inst>) {
        match i {
            Inst::Label(l) => self.labels[*l as usize] = Some(self.pos()),
            Inst::Jmp(l) => {
                if !matches!(next, Some(Inst::Label(n)) if n == l) {
                    self.j(*l);
                }
            }
            Inst::Br { c, t, f } => {
                if let V::I(v) = c {
                    self.j(if *v & 0xff != 0 { *t } else { *f });
                    return;
                }
                if let Some(cc) = self.flags.take() {
                    // The comparison just before: its condition bit.
                    if matches!(next, Some(Inst::Label(n)) if n == t) {
                        self.br(cc.inv(), *f);
                    } else {
                        self.br(cc, *t);
                        if !matches!(next, Some(Inst::Label(n)) if n == f) {
                            self.j(*f);
                        }
                    }
                    return;
                }
                let r = self.src(*c, K::I8, false, R11);
                self.cmp_imm(r, 0);
                self.br(Cond::f(EQ), *t);
                if !matches!(next, Some(Inst::Label(n)) if n == f) {
                    self.j(*f);
                }
            }
            Inst::Mov { k, dst, src } => self.mov_val(*k, *dst, *src),
            Inst::Copy { dst, src, size } => match scalar(*size) {
                Some(k) => self.mov_val(k, *dst, V::M(*src)),
                None => self.copy(*dst, *src, *size, false),
            },
            Inst::Zero { dst, size } => self.copy(*dst, *dst, *size, true),
            Inst::Addr { dst, src } => {
                let d = self.dst_r(*dst).unwrap_or(R11);
                self.lea(d, *src);
                self.put(d, *dst, K::I64, true);
            }
            Inst::Sym { dst, sym, got } => {
                let d = self.dst_r(*dst).unwrap_or(R11);
                if *got {
                    self.reloc(RelKind::PpcGotHa, *sym);
                    self.ins(enc::addis(R(d), R(TOC), 0));
                    self.reloc(RelKind::PpcGotLo, *sym);
                    self.ld_word(d, d, 0);
                } else {
                    self.reloc(RelKind::PpcTocHa, *sym);
                    self.ins(enc::addis(R(d), R(TOC), 0));
                    self.reloc(RelKind::PpcTocLo, *sym);
                    self.addi(d, d, 0);
                }
                self.put(d, *dst, K::I64, true);
            }
            Inst::Tls { dst, sym } => {
                // Initial exec: the offset from the thread pointer in a
                // GOT slot.
                self.reloc(RelKind::PpcTlsIeHa, *sym);
                self.ins(enc::addis(R(R11), R(TOC), 0));
                self.reloc(RelKind::PpcTlsIeLo, *sym);
                self.ld_word(R11, R11, 0);
                self.reloc(RelKind::PpcTls, *sym);
                let d = self.dst_r(*dst).unwrap_or(R11);
                self.alu(Alu::Add, d, R11, TP);
                self.put(d, *dst, K::I64, true);
            }
            Inst::Bin {
                op,
                k,
                signed,
                dst,
                a,
                b,
            } => self.bin(*op, *k, *signed, *dst, *a, *b),
            Inst::Ovf {
                op,
                k,
                signed,
                dst,
                a,
                b,
                fail,
            } => self.ovf(*op, *k, *signed, *dst, *a, *b, *fail),
            Inst::Un { op, k, dst, a } => self.un(*op, *k, *dst, *a),
            Inst::Fma { k, dst, a, b, c } => self.fma(*k, *dst, *a, *b, *c),
            Inst::Cmp {
                cc,
                k,
                signed,
                dst,
                a,
                b,
            } => self.cmp(*cc, *k, *signed, *dst, *a, *b, next),
            Inst::Conv {
                conv,
                from,
                to,
                dst,
                a,
            } => self.conv(*conv, *from, *to, *dst, *a),
            Inst::AtomicLoad { k, dst, ptr, ord } => {
                let k = int_k(*k);
                self.lea(R9, *ptr);
                if *ord == Order::SeqCst {
                    self.ins(enc::sync());
                }
                let d = self.dst_r(*dst).unwrap_or(R11);
                self.ls(true, k, false, false, d, R9, 0);
                if ord.acq() {
                    self.ins(enc::lwsync());
                }
                self.put(d, *dst, k, true);
            }
            Inst::AtomicStore { k, ptr, src, ord } => {
                let k = int_k(*k);
                let r = self.src(*src, k, false, R12);
                self.lea(R9, *ptr);
                self.before(*ord);
                self.ls(false, k, false, false, r, R9, 0);
            }
            Inst::AtomicRmw {
                op,
                k,
                dst,
                ptr,
                val,
                ord,
            } => self.rmw(*op, *k, *dst, *ptr, *val, *ord),
            Inst::Cas {
                k,
                dst,
                ptr,
                expected,
                new,
                ord,
            } => self.cas(*k, *dst, *ptr, *expected, *new, *ord),
            Inst::Call {
                callee,
                args,
                rets,
                sret,
                ..
            } => self.call(callee, args, rets, *sret),
            Inst::Param { part, dst } => self.param(*part, *dst),
            Inst::Ret { vals } => {
                // In order above -O0: the allocator assumes so.
                for (p, v) in vals {
                    if let Loc::Fp(n) = p.loc {
                        self.ldf(F1 + u32::from(n), *v, p.k);
                    } else if let (Loc::Int(r), true) = (p.loc, self.opt) {
                        self.ld_arg(R3 + u32::from(r), *p, *v);
                    }
                }
                for (p, v) in vals {
                    if let (Loc::Int(r), false) = (p.loc, self.opt) {
                        self.ld_arg(R3 + u32::from(r), *p, *v);
                    }
                }
                self.epilogue();
            }
            Inst::VLoop(v) => self.vloop(v),
            Inst::V(_) => unreachable!("no straight-line vector code here"),
            Inst::Trap => self.ins(enc::trap()),
            Inst::Line(line, col, scope) => {
                let at = 4 * self.pos() as u32;
                self.lines.push((at, *line, *col, *scope));
            }
        }
    }

    pub(crate) fn mov_val(&mut self, k: K, dst: Mem, src: V) {
        match (self.ra.get(dst), self.ra.get_v(src)) {
            (Some(a), _) if a.fp => self.ldf(a.reg, src, float_k(k)),
            (Some(a), _) => self.ld(a.reg, src, k, false),
            (None, Some(a)) if a.fp => self.stf(a.reg, dst, float_k(k)),
            (None, Some(a)) => self.st(a.reg, dst, k),
            (None, None) => {
                self.ld(R11, src, k, false);
                self.st(R11, dst, k);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn cmp(
        &mut self,
        cc: Cc,
        k: K,
        signed: bool,
        dst: Mem,
        a: V,
        b: V,
        next: Option<&Inst>,
    ) {
        let (bit, neg) = if k.is_float() {
            let fa = self.fsrc(a, k, FA);
            let fb = self.fsrc(b, k, FB);
            self.fcmpu(fa, fb);
            // The ordered predicates: `<=` / `>=` join `==`.
            match cc {
                Cc::Eq => (EQ, false),
                Cc::Ne => (EQ, true),
                Cc::Lt => (LT, false),
                Cc::Gt => (GT, false),
                Cc::Le => {
                    self.cror(LT, LT, EQ);
                    (LT, false)
                }
                Cc::Ge => {
                    self.cror(GT, GT, EQ);
                    (GT, false)
                }
            }
        } else {
            let ra = self.src(a, k, signed, R11);
            match b {
                // Against an immediate the comparison takes.
                V::I(i) if self.opt && signed && fits16(ext(i, k, true)) => {
                    self.cmp_imm(ra, ext(i, k, true));
                }
                V::I(i) if self.opt && !signed && (0..0x1_0000).contains(&ext(i, k, false)) => {
                    self.cmpl_imm(ra, ext(i, k, false));
                }
                _ => {
                    let rb = self.src(b, k, signed, R12);
                    self.cmp_reg(signed, ra, rb);
                }
            }
            match cc {
                Cc::Eq => (EQ, false),
                Cc::Ne => (EQ, true),
                Cc::Lt => (LT, false),
                Cc::Ge => (LT, true),
                Cc::Gt => (GT, false),
                Cc::Le => (GT, true),
            }
        };
        // A branch right after, its only reader, tests the bit itself.
        if let Mem::Frame(s, o) = dst {
            if matches!(next, Some(Inst::Br { c: V::M(m), .. }) if *m == dst)
                && self.waits.get(&(s, o)) == Some(&(1, 1))
            {
                self.flags = Some(if neg { Cond::f(bit) } else { Cond::t(bit) });
                self.skipped.insert((s, o), (1, 1));
                return;
            }
        }
        let d = self.dst_r(dst).unwrap_or(R11);
        self.cr_bit(d, bit);
        if neg {
            self.logic_imm(LogImm::Xori, d, d, 1);
        }
        self.put(d, dst, K::I8, true);
    }

    /// The barrier in front of an access ordered `ord`: `hwsync` for
    /// `seq_cst`, `lwsync` for release.
    pub(crate) fn before(&mut self, ord: Order) {
        if ord == Order::SeqCst {
            self.ins(enc::sync());
        } else if ord.rel() {
            self.ins(enc::lwsync());
        }
    }

    /// `lbarx` / `lharx` / `lwarx` / `ldarx` `rt, 0, ra` (zero-extending).
    pub(crate) fn larx(&mut self, k: K, rt: u32, ra: u32) {
        self.ins(enc::larx(k.size(), R(rt), R(ra)));
    }

    /// `stbcx.` / `sthcx.` / `stwcx.` / `stdcx.` `rs, 0, ra`.
    pub(crate) fn stcx(&mut self, k: K, rs: u32, ra: u32) {
        self.ins(enc::stcx(k.size(), R(rs), R(ra)));
    }

    pub(crate) fn rmw(&mut self, op: Rmw, k: K, dst: Mem, ptr: Mem, val: V, ord: Order) {
        let k = int_k(k);
        let rv = self.src(val, k, false, R12);
        self.lea(R9, ptr);
        self.before(ord);
        let top = self.pos();
        self.larx(k, R11, R9);
        match op {
            Rmw::Xchg => self.mr(R10, rv),
            Rmw::Add => self.alu(Alu::Add, R10, R11, rv),
            Rmw::Sub => self.subf(R10, rv, R11),
            Rmw::And => self.logic(Logic::And, R10, R11, rv),
            Rmw::Or => self.logic(Logic::Or, R10, R11, rv),
            Rmw::Xor => self.logic(Logic::Xor, R10, R11, rv),
        }
        self.stcx(k, R10, R9);
        self.b_back(Cond::f(EQ), top);
        if ord.acq() {
            self.ins(enc::isync());
        }
        self.put(R11, dst, k, true);
    }

    pub(crate) fn cas(&mut self, k: K, dst: Mem, ptr: Mem, expected: V, new: V, ord: Order) {
        let k = int_k(k);
        // The reservation loads zero-extend: so is `expected`.
        let re = self.src(expected, k, false, R12);
        let rn = self.src(new, k, false, R10);
        self.lea(R9, ptr);
        self.before(ord);
        let top = self.pos();
        self.larx(k, R11, R9);
        self.cmp_reg(true, R11, re);
        let out = self.b_fwd(Cond::f(EQ));
        self.stcx(k, rn, R9);
        self.b_back(Cond::f(EQ), top);
        self.here(out);
        if ord.acq() {
            self.ins(enc::isync());
        }
        self.put(R11, dst, k, true);
    }
}
