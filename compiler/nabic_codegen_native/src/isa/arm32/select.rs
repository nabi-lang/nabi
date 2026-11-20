//! Instruction selection, branches, comparisons and atomics.

#[allow(clippy::wildcard_imports)]
use crate::isa::arm32::*;

impl Em<'_> {
    pub(crate) fn b(&mut self, cond: Cond, l: Label) {
        self.fixes.push((self.pos(), l));
        self.ins(enc::b(cond));
    }

    /// A forward branch to a spot not known yet: its index.
    pub(crate) fn b_fwd(&mut self, cond: Cond) -> usize {
        self.ins(enc::b(cond));
        self.pos() - 1
    }

    /// A branch back to index `top`.
    pub(crate) fn b_back(&mut self, cond: Cond, top: usize) {
        let d = top as i64 - (self.pos() as i64 + 2);
        self.ins(enc::b_to(cond, d));
    }

    /// Point the branch at `at` here.
    pub(crate) fn here(&mut self, at: usize) {
        let d = self.pos() as i64 - (at as i64 + 2);
        self.a.patch24(at, d);
    }

    /// `r` = 1 under `cond`, else 0.
    pub(crate) fn setcc(&mut self, r: u32, cond: Cond) {
        self.mov_imm(r, 0);
        self.mov_imm_cc(cond, r, 1);
    }

    pub(crate) fn inst(&mut self, i: &Inst, next: Option<&Inst>) {
        match i {
            Inst::Label(l) => self.labels[*l as usize] = Some(self.pos()),
            Inst::Jmp(l) => {
                if !matches!(next, Some(Inst::Label(n)) if n == l) {
                    self.b(Cond::AL, *l);
                }
            }
            Inst::Br { c, t, f } => {
                if let V::I(v) = c {
                    self.b(Cond::AL, if *v & 0xff != 0 { *t } else { *f });
                    return;
                }
                if let Some(cc) = self.flags.take() {
                    // The comparison just before: its flags.
                    if matches!(next, Some(Inst::Label(n)) if n == t) {
                        self.b(cc.inv(), *f);
                    } else {
                        self.b(cc, *t);
                        if !matches!(next, Some(Inst::Label(n)) if n == f) {
                            self.b(Cond::AL, *f);
                        }
                    }
                    return;
                }
                let r = self.src(self.x, *c, K::I8, false);
                self.dpi(Cond::AL, Op::Cmp, true, 0, r, Imm::small(0));
                self.b(Cond::NE, *t);
                if !matches!(next, Some(Inst::Label(n)) if n == f) {
                    self.b(Cond::AL, *f);
                }
            }
            Inst::Mov { k, dst, src } => self.mov_val(*k, *dst, *src),
            Inst::Copy { dst, src, size } => match scalar(*size) {
                Some(k) => self.mov_val(k, *dst, V::M(*src)),
                None => self.copy(*dst, *src, *size, false),
            },
            Inst::Zero { dst, size } => self.copy(*dst, *dst, *size, true),
            Inst::Addr { dst, src } => {
                self.lea(self.x, *src);
                self.st(self.x, *dst, K::I32);
            }
            Inst::Sym { dst, sym, got } => {
                if *got {
                    self.literal(self.x, RelKind::ArmGotPrel, *sym);
                    self.ls(Acc::Ldr, self.x, self.x, 0, self.x);
                } else {
                    // movw / movt r4 of `sym - (. + 16)`; add r4, pc, r4
                    self.reloc(RelKind::ArmMovwPrel, *sym);
                    self.ins(enc::movw(Cond::AL, R(self.x), 0xfff0));
                    self.reloc(RelKind::ArmMovtPrel, *sym);
                    self.ins(enc::movt(Cond::AL, R(self.x), 0xfff4));
                    self.dp(Cond::AL, Op::Add, false, self.x, PC, self.x);
                }
                self.st(self.x, *dst, K::I32);
            }
            Inst::Tls { dst, sym } => {
                // Initial exec: the offset from the thread pointer, in a
                // GOT slot.
                self.literal(self.x, RelKind::ArmTlsIe, *sym);
                self.ls(Acc::Ldr, self.x, self.x, 0, self.x);
                self.ins(enc::mrc_tp(R(self.xh)));
                self.dp(Cond::AL, Op::Add, false, self.x, self.x, self.xh);
                self.st(self.x, *dst, K::I32);
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
                self.lea(R8, *ptr);
                if wide(*k) {
                    self.ins(enc::ldrexd(R(R4), R(R8)));
                    self.ins(enc::clrex());
                    self.after(*ord);
                    self.st2(R4, R5, *dst);
                } else {
                    self.ls(Acc::load(*k, false), R4, R8, 0, R8);
                    self.after(*ord);
                    self.st(R4, *dst, *k);
                }
            }
            Inst::AtomicStore { k, ptr, src, ord } => {
                if wide(*k) {
                    self.ld2(R6, R7, *src);
                    self.lea(R8, *ptr);
                    self.before(*ord);
                    let top = self.pos();
                    self.ins(enc::ldrexd(R(R4), R(R8)));
                    self.ins(enc::strexd(R(IP), R(R6), R(R8)));
                    self.dpi(Cond::AL, Op::Cmp, true, 0, IP, Imm::small(0));
                    self.b_back(Cond::NE, top);
                } else {
                    self.ld(R6, *src, *k, false);
                    self.lea(R8, *ptr);
                    self.before(*ord);
                    self.ls(Acc::store(*k), R6, R8, 0, R8);
                }
                // No later load passes a seq_cst store.
                if *ord == Order::SeqCst {
                    self.ins(enc::dmb_ish());
                }
            }
            Inst::AtomicRmw {
                op,
                k,
                dst,
                ptr,
                val,
                ord,
            } => {
                if wide(*k) {
                    self.rmw64(*op, *dst, *ptr, *val, *ord);
                } else {
                    self.rmw(*op, *k, *dst, *ptr, *val, *ord);
                }
            }
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
                for (p, v) in vals {
                    if let Loc::Int(r) = p.loc {
                        let r = u32::from(r);
                        if wide(p.k) {
                            self.ld2(r, r + 1, *v);
                        } else {
                            self.ld(r, *v, p.k, p.signed);
                        }
                    }
                }
                for (p, v) in vals {
                    if let Loc::Fp(n) = p.loc {
                        self.ldf(u32::from(n), *v, p.k);
                    }
                }
                self.epilogue();
            }
            Inst::VLoop(v) => self.vloop(v),
            Inst::V(_) => unreachable!("no straight-line vector code here"),
            // The trap LLVM writes for Linux.
            Inst::Trap => self.ins(enc::trap()),
            Inst::Line(line, col, scope) => {
                let at = 4 * self.pos() as u32;
                self.lines.push((at, *line, *col, *scope));
            }
        }
    }

    pub(crate) fn mov_val(&mut self, k: K, dst: Mem, src: V) {
        if k.is_float() && (self.dreg(dst).is_some() || self.dreg_v(src).is_some()) {
            let x = self.fsrc(src, k, 0);
            self.stf(x, dst, k);
            return;
        }
        if let Some(d) = self.ireg(dst).filter(|_| !wide(k)) {
            // Zero-extended by the load itself.
            self.ld(d, src, k, false);
            return;
        }
        if wide(k) {
            if let Some((l, h)) = self.ra.pair(dst) {
                self.ld2(l, h, src);
            } else {
                let (l, h) = self.src2(self.x, self.xh, src);
                self.st2(l, h, dst);
            }
        } else {
            // An allocated value is stored as it is.
            let r = self.ireg_v(src).unwrap_or(self.x);
            self.ld(r, src, k, false);
            self.st(r, dst, k);
        }
    }

    /// Whether the branch `next` takes condition `c` as flags in place of
    /// `dst`, the comparison's result, read by it alone: nothing more to
    /// do then.
    pub(crate) fn fuse(&mut self, c: Cond, dst: Mem, next: Option<&Inst>) -> bool {
        let Mem::Frame(s, o) = dst else { return false };
        if !matches!(next, Some(Inst::Br { c: V::M(m), .. }) if *m == dst)
            || self.waits.get(&(s, o)) != Some(&(1, 1))
        {
            return false;
        }
        self.flags = Some(c);
        self.skipped.insert((s, o), (1, 1));
        true
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
        let cond = if k.is_float() {
            let x = self.fsrc(a, k, 0);
            let y = self.fsrc(b, k, 1);
            self.vfp(Vop::Cmp, k, x, 0, y);
            self.ins(enc::vmrs());
            match cc {
                Cc::Eq => Cond::EQ,
                Cc::Ne => Cond::NE,
                Cc::Lt => Cond::MI,
                Cc::Le => Cond::LS,
                Cc::Gt => Cond::GT,
                Cc::Ge => Cond::GE,
            }
        } else if wide(k) {
            let (x, y) = (self.src2(self.x, self.xh, a), self.src2(self.y, self.yh, b));
            match cc {
                Cc::Eq | Cc::Ne => {
                    self.dp(Cond::AL, Op::Cmp, true, 0, x.0, y.0);
                    self.dp(Cond::EQ, Op::Cmp, true, 0, x.1, y.1);
                    if cc == Cc::Eq { Cond::EQ } else { Cond::NE }
                }
                // The flags of a - b.
                Cc::Lt | Cc::Ge => {
                    self.dp(Cond::AL, Op::Cmp, true, 0, x.0, y.0);
                    self.dp(Cond::AL, Op::Sbc, true, IP, x.1, y.1);
                    match (cc, signed) {
                        (Cc::Lt, true) => Cond::LT,
                        (Cc::Lt, false) => Cond::LO,
                        (_, true) => Cond::GE,
                        _ => Cond::HS,
                    }
                }
                // Of b - a.
                _ => {
                    self.dp(Cond::AL, Op::Cmp, true, 0, y.0, x.0);
                    self.dp(Cond::AL, Op::Sbc, true, IP, y.1, x.1);
                    match (cc, signed) {
                        (Cc::Gt, true) => Cond::LT,
                        (Cc::Gt, false) => Cond::LO,
                        (_, true) => Cond::GE,
                        _ => Cond::HS,
                    }
                }
            }
        } else {
            let x = self.src(self.x, a, k, signed);
            let mut done = false;
            if let V::I(i) = b {
                let imm = ext32(i, k, signed);
                if let Some(e) = modimm(imm) {
                    self.dpi(Cond::AL, Op::Cmp, true, 0, x, e);
                    done = true;
                } else if let Some(e) = modimm(imm.wrapping_neg()) {
                    self.dpi(Cond::AL, Op::Cmn, true, 0, x, e);
                    done = true;
                }
            }
            if !done {
                let y = self.src(self.y, b, k, signed);
                self.dp(Cond::AL, Op::Cmp, true, 0, x, y);
            }
            match (cc, signed) {
                (Cc::Eq, _) => Cond::EQ,
                (Cc::Ne, _) => Cond::NE,
                (Cc::Lt, true) => Cond::LT,
                (Cc::Lt, false) => Cond::LO,
                (Cc::Le, true) => Cond::LE,
                (Cc::Le, false) => Cond::LS,
                (Cc::Gt, true) => Cond::GT,
                (Cc::Gt, false) => Cond::HI,
                (Cc::Ge, true) => Cond::GE,
                (Cc::Ge, false) => Cond::HS,
            }
        };
        if self.fuse(cond, dst, next) {
            return;
        }
        // 0 / 1: already zero-extended.
        let d = self.dst_r(dst);
        self.setcc(d, cond);
        if d == self.x {
            self.st(self.x, dst, K::I8);
        }
    }

    /// `ldrex` / `strex` of `k` (1 / 2 / 4 bytes): `rt` at `[r8]`, `ip`
    /// the store's status.
    /// The barrier in front of an access ordered `ord`.
    pub(crate) fn before(&mut self, ord: Order) {
        if ord.rel() {
            self.ins(enc::dmb_ish());
        }
    }

    /// The barrier after it.
    pub(crate) fn after(&mut self, ord: Order) {
        if ord.acq() {
            self.ins(enc::dmb_ish());
        }
    }

    pub(crate) fn ldrex(&mut self, k: K, rt: u32) {
        self.ins(enc::ldrex(k.size(), R(rt), R(R8)));
    }

    pub(crate) fn strex(&mut self, k: K, rt: u32) {
        self.ins(enc::strex(k.size(), R(IP), R(rt), R(R8)));
    }

    pub(crate) fn rmw(&mut self, op: Rmw, k: K, dst: Mem, ptr: Mem, val: V, ord: Order) {
        self.ld(R6, val, k, false);
        self.lea(R8, ptr);
        self.before(ord);
        let top = self.pos();
        self.ldrex(k, R4);
        match op {
            Rmw::Xchg => self.mov(R5, R6),
            Rmw::Add => self.dp(Cond::AL, Op::Add, false, R5, R4, R6),
            Rmw::Sub => self.dp(Cond::AL, Op::Sub, false, R5, R4, R6),
            Rmw::And => self.dp(Cond::AL, Op::And, false, R5, R4, R6),
            Rmw::Or => self.dp(Cond::AL, Op::Orr, false, R5, R4, R6),
            Rmw::Xor => self.dp(Cond::AL, Op::Eor, false, R5, R4, R6),
        }
        self.strex(k, R5);
        self.dpi(Cond::AL, Op::Cmp, true, 0, IP, Imm::small(0));
        self.b_back(Cond::NE, top);
        self.after(ord);
        self.st(R4, dst, k);
    }

    /// An 8-byte read-modify-write: an `ldrexd` / `strexd` loop, the new
    /// value in `r2 r3`.
    pub(crate) fn rmw64(&mut self, op: Rmw, dst: Mem, ptr: Mem, val: V, ord: Order) {
        self.ld2(R6, R7, val);
        self.lea(R8, ptr);
        self.before(ord);
        let top = self.pos();
        self.ins(enc::ldrexd(R(R4), R(R8)));
        match op {
            Rmw::Xchg => {
                self.mov(R2, R6);
                self.mov(R3, R7);
            }
            Rmw::Add => {
                self.dp(Cond::AL, Op::Add, true, R2, R4, R6);
                self.dp(Cond::AL, Op::Adc, false, R3, R5, R7);
            }
            Rmw::Sub => {
                self.dp(Cond::AL, Op::Sub, true, R2, R4, R6);
                self.dp(Cond::AL, Op::Sbc, false, R3, R5, R7);
            }
            _ => {
                let opc = match op {
                    Rmw::And => Op::And,
                    Rmw::Or => Op::Orr,
                    _ => Op::Eor,
                };
                self.dp(Cond::AL, opc, false, R2, R4, R6);
                self.dp(Cond::AL, opc, false, R3, R5, R7);
            }
        }
        self.ins(enc::strexd(R(IP), R(R2), R(R8)));
        self.dpi(Cond::AL, Op::Cmp, true, 0, IP, Imm::small(0));
        self.b_back(Cond::NE, top);
        self.after(ord);
        self.st2(R4, R5, dst);
    }

    pub(crate) fn cas(&mut self, k: K, dst: Mem, ptr: Mem, expected: V, new: V, ord: Order) {
        if wide(k) {
            self.ld2(R6, R7, expected);
            self.ld2(R2, R3, new);
            self.lea(R8, ptr);
            self.before(ord);
            let top = self.pos();
            self.ins(enc::ldrexd(R(R4), R(R8)));
            self.dp(Cond::AL, Op::Cmp, true, 0, R4, R6);
            self.dp(Cond::EQ, Op::Cmp, true, 0, R5, R7);
            let out = self.b_fwd(Cond::NE);
            self.ins(enc::strexd(R(IP), R(R2), R(R8)));
            self.dpi(Cond::AL, Op::Cmp, true, 0, IP, Imm::small(0));
            self.b_back(Cond::NE, top);
            self.here(out);
            self.after(ord);
            self.st2(R4, R5, dst);
            return;
        }
        self.ld(R6, expected, k, false);
        self.ld(R7, new, k, false);
        self.lea(R8, ptr);
        self.before(ord);
        let top = self.pos();
        self.ldrex(k, R4);
        self.dp(Cond::AL, Op::Cmp, true, 0, R4, R6);
        let out = self.b_fwd(Cond::NE);
        self.strex(k, R7);
        self.dpi(Cond::AL, Op::Cmp, true, 0, IP, Imm::small(0));
        self.b_back(Cond::NE, top);
        self.here(out);
        self.after(ord);
        self.st(R4, dst, k);
    }
}
