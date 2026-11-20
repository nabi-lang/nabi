//! Instruction selection, branches and labels.

#[allow(clippy::wildcard_imports)]
use crate::isa::i386::*;

impl Em<'_> {
    /// Whether the next label branch is rel8 (`relax::relax`).
    pub(crate) fn short(&self) -> bool {
        self.plan
            .as_ref()
            .is_some_and(|p| p.get(self.sites.len()) == Some(&true))
    }

    pub(crate) fn branched(&mut self, start: usize, at: usize, l: Label, short: bool) {
        self.sites.push((start, (self.a.pos() - start) as u8, l));
        self.fixes.push((at, l, short));
    }

    pub(crate) fn jmp(&mut self, l: Label) {
        let (start, short) = (self.a.pos(), self.short());
        let at = if short {
            self.a.jmp8(None)
        } else {
            self.a.jmp(None)
        };
        self.branched(start, at, l, short);
    }

    pub(crate) fn jcc(&mut self, cc: Cond, l: Label) {
        let (start, short) = (self.a.pos(), self.short());
        let at = if short {
            self.a.jcc8(cc, None)
        } else {
            self.a.jcc(cc, None)
        };
        self.branched(start, at, l, short);
    }

    /// A forward `jcc` to a spot not known yet: its rel32's position.
    pub(crate) fn jcc_fwd(&mut self, cc: Cond) -> usize {
        self.a.jcc(cc, None)
    }

    pub(crate) fn jmp_fwd(&mut self) -> usize {
        self.a.jmp(None)
    }

    /// Point the rel32 at `at` here.
    pub(crate) fn here(&mut self, at: usize) {
        let to = self.a.pos();
        self.a.patch_rel32(at, to);
    }

    /// `eax` = `cc` as 0 / 1.
    pub(crate) fn setcc(&mut self, cc: Cond) {
        self.a.setcc(cc, Gpr(EAX));
        self.a.movzx(Gpr(EAX), Sz::B, gr(EAX));
    }

    pub(crate) fn inst(&mut self, i: &Inst, next: Option<&Inst>) {
        self.got_at(i);
        self.inst_body(i, next);
        self.got_after(i);
    }

    pub(crate) fn inst_body(&mut self, i: &Inst, next: Option<&Inst>) {
        match i {
            Inst::Label(l) => self.labels[*l as usize] = Some(self.a.pos()),
            Inst::Jmp(l) => {
                if !matches!(next, Some(Inst::Label(n)) if n == l) {
                    self.jmp(*l);
                }
            }
            Inst::Br { c, t, f } => {
                if let V::I(v) = c {
                    self.jmp(if *v & 0xff != 0 { *t } else { *f });
                    return;
                }
                if let Some(cc) = self.flags.take() {
                    // The comparison just before: its flags.
                    if matches!(next, Some(Inst::Label(n)) if n == t) {
                        self.jcc(cc.inv(), *f);
                    } else {
                        self.jcc(cc, *t);
                        if !matches!(next, Some(Inst::Label(n)) if n == f) {
                            self.jmp(*f);
                        }
                    }
                    return;
                }
                if let Some(r) = self.ra.get_v(*c).filter(|a| !a.fp) {
                    // Zero-extended: the byte is the register.
                    self.a.test(Sz::D, gr(r.reg), Gpr(r.reg));
                } else {
                    self.ld(EAX, *c, K::I8, false);
                    self.a.test(Sz::B, gr(EAX), Gpr(EAX));
                }
                self.jcc(Cond::NE, *t);
                if !matches!(next, Some(Inst::Label(n)) if n == f) {
                    self.jmp(*f);
                }
            }
            Inst::Mov { k, dst, src } => self.mov_val(*k, *dst, *src),
            Inst::Copy { dst, src, size } => match scalar(*size) {
                Some(k) => self.mov_val(k, *dst, V::M(*src)),
                None => self.copy(*dst, *src, *size, false),
            },
            Inst::Zero { dst, size } => self.copy(*dst, *dst, *size, true),
            Inst::Addr { dst, src } => {
                let r = self.dst_r(*dst).unwrap_or(EAX);
                self.lea(r, *src);
                self.st(r, *dst, K::I32);
            }
            Inst::Sym { dst, sym, got } => {
                let r = self.dst_r(*dst).unwrap_or(EAX);
                if self.obj.coff {
                    // The image relocates it.
                    self.a.mov_ri_sym(Gpr(r), RelKind::I386Abs32, *sym);
                } else if *got {
                    let m = Addr::BdRel(Gpr(EBX), RelKind::I386Got32X, *sym);
                    self.a.mov(Sz::D, Gpr(r), Rm::M(m));
                } else {
                    let m = Addr::BdRel(Gpr(EBX), RelKind::I386GotOff, *sym);
                    self.a.lea(Gpr(r), m);
                }
                self.st(r, *dst, K::I32);
            }
            Inst::Tls { dst, sym } => {
                if self.obj.coff {
                    // The TEB's TLS array at `_tls_index`, plus the offset
                    // in the TLS section.
                    let index = self.obj.sym("_tls_index");
                    let m = Addr::Abs(RelKind::I386Abs32, index);
                    self.a.mov(Sz::D, Gpr(EAX), Rm::M(m));
                    self.a.mov(Sz::D, Gpr(ECX), Rm::M(Addr::Seg(Seg::Fs, 0x2c)));
                    let m = Addr::Bi(Gpr(ECX), Gpr(EAX), 2);
                    self.a.mov(Sz::D, Gpr(ECX), Rm::M(m));
                    let m = Addr::BdRel(Gpr(ECX), RelKind::X86SecRel, *sym);
                    self.a.lea(Gpr(EAX), m);
                } else {
                    // Initial exec: the offset from the thread pointer.
                    let m = Addr::BdRel(Gpr(EBX), RelKind::I386TlsGotIe, *sym);
                    self.a.mov(Sz::D, Gpr(EAX), Rm::M(m));
                    let tp = Addr::Seg(Seg::Gs, 0);
                    self.a.alu_ld(Alu::Add, Gpr(EAX), Rm::M(tp));
                }
                self.st(EAX, *dst, K::I32);
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
            Inst::Fma { .. } => unreachable!("no fused multiply-add on i686"),
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
            Inst::AtomicLoad { k, dst, ptr, .. } => {
                self.lea(ESI, *ptr);
                if wide(*k) {
                    self.a.sse(Sse::MovqLd, Xmm(XMM0), Xrm::M(bd(ESI, 0)));
                    self.stf(XMM0, *dst, K::F64);
                } else {
                    self.extend(EAX, Rm::M(bd(ESI, 0)), *k, false);
                    self.st(EAX, *dst, *k);
                }
            }
            Inst::AtomicStore { k, ptr, src, ord } => {
                // x86 stores release as they are; only seq_cst fences.
                let sc = *ord == Order::SeqCst;
                if wide(*k) {
                    self.ldf(XMM0, *src, K::F64);
                    self.lea(ESI, *ptr);
                    self.a.sse_st(Sse::MovqSt, bd(ESI, 0), Xmm(XMM0));
                    if sc {
                        self.a.mfence();
                    }
                } else {
                    self.ld(EAX, *src, *k, false);
                    self.lea(ESI, *ptr);
                    if sc {
                        self.a.xchg(size(*k), Rm::M(bd(ESI, 0)), Gpr(EAX));
                    } else {
                        self.a.mov_st(size(*k), Rm::M(bd(ESI, 0)), Gpr(EAX));
                    }
                }
            }
            Inst::AtomicRmw {
                op,
                k,
                dst,
                ptr,
                val,
                ..
            } => {
                if wide(*k) {
                    self.rmw64(*op, *dst, *ptr, *val);
                } else {
                    self.rmw(*op, *k, *dst, *ptr, *val);
                }
            }
            Inst::Cas {
                k,
                dst,
                ptr,
                expected,
                new,
                ..
            } => {
                if wide(*k) {
                    self.save_ebx();
                    self.ld2(ECX, ESI, *new);
                    self.mov(EBX, ECX);
                    self.mov(ECX, ESI);
                    self.ld2(EAX, EDX, *expected);
                    self.lea(EDI, *ptr);
                    self.a.lock();
                    self.a.cmpxchg8b(bd(EDI, 0));
                    self.restore_ebx();
                    self.st2(EAX, EDX, *dst);
                } else {
                    self.ld(EAX, *expected, *k, false);
                    self.ld(ECX, *new, *k, false);
                    self.lea(ESI, *ptr);
                    self.a.lock();
                    self.a.cmpxchg(size(*k), bd(ESI, 0), Gpr(ECX));
                    self.st(EAX, *dst, *k);
                }
            }
            Inst::Call {
                callee,
                args,
                rets,
                sret,
                pop,
                ..
            } => self.call(callee, args, rets, *sret, *pop),
            Inst::Param { part, dst } => {
                let o = match part.loc {
                    Loc::Stack(o) => o,
                    Loc::Area(o) => {
                        self.a.lea(Gpr(EAX), bd(EBP, 8 + o as i32));
                        self.st(EAX, *dst, K::I32);
                        return;
                    }
                    _ => 0,
                };
                let at = bd(EBP, 8 + o as i32);
                if let Some(x) = self.dst_x(*dst) {
                    let k = if part.k.size() == 8 { K::F64 } else { K::F32 };
                    self.a.sse(movs(k), Xmm(x), Xrm::M(at));
                } else if let Some(d) = self.dst_r(*dst) {
                    // Kept zero-extended.
                    self.extend(d, Rm::M(at), part.k, false);
                } else if wide(part.k) {
                    self.a.mov(Sz::D, Gpr(EAX), Rm::M(at));
                    self.a.mov(Sz::D, Gpr(EDX), Rm::M(at.plus(4)));
                    self.st2(EAX, EDX, *dst);
                } else {
                    self.extend(EAX, Rm::M(at), part.k, part.signed);
                    self.st(EAX, *dst, part.k);
                }
            }
            Inst::Ret { vals } => {
                if self.opt {
                    // The float first: its address may take any register.
                    for (p, v) in vals {
                        if p.loc == Loc::Fp(0) {
                            self.fld(*v, p.k);
                        }
                    }
                }
                for (p, v) in vals {
                    if let Loc::Int(r) = p.loc {
                        let r = r as usize;
                        if wide(p.k) {
                            self.ld2(RETS[r], RETS[r + 1], *v);
                        } else {
                            self.ld(RETS[r], *v, p.k, p.signed);
                        }
                    }
                }
                for (p, v) in vals {
                    if p.loc == Loc::Fp(0) && !self.opt {
                        self.fld(*v, p.k);
                    }
                }
                self.epilogue();
            }
            Inst::VLoop(v) => self.vloop(v),
            Inst::V(_) => unreachable!("no straight-line vector code here"),
            Inst::Trap => self.a.ud2(),
            Inst::Line(line, col, scope) => {
                let at = self.a.pos() as u32;
                self.lines.push((at, *line, *col, *scope));
            }
        }
    }

    pub(crate) fn mov_val(&mut self, k: K, dst: Mem, src: V) {
        if self.opt && self.mov_opt(k, dst, src) {
            return;
        }
        if wide(k) {
            self.ld2(EAX, EDX, src);
            self.st2(EAX, EDX, dst);
        } else {
            self.ld(EAX, src, k, false);
            self.st(EAX, dst, k);
        }
    }

    /// `mov_val` through registers where an operand has one: false where
    /// the plain way applies. (8 bytes in memory stay two words: an `xmm`
    /// load of two word stores would stall store forwarding.)
    pub(crate) fn mov_opt(&mut self, k: K, dst: Mem, src: V) -> bool {
        if let Some((l, h)) = self.ra.pair(dst) {
            self.ld2(l, h, src);
            return true;
        }
        if let Some((l, h)) = self.ra.pair_v(src) {
            self.st2(l, h, dst);
            return true;
        }
        // The float kind of the size, for an untyped copy.
        let fk = if k.size() == 8 { K::F64 } else { K::F32 };
        let s = self.ra.get_v(src);
        if let Some(d) = self.ra.get(dst) {
            if d.fp {
                self.ldf(d.reg, src, fk);
            } else {
                self.ld(d.reg, src, k, false);
            }
            return true;
        }
        match s {
            Some(a) if a.fp => self.stf(a.reg, dst, fk),
            Some(a) if a.reg < 4 || k != K::I8 => {
                let m = self.mem(dst, ECX);
                self.st_rm(a.reg, m, k);
            }
            Some(a) => {
                self.mov(EAX, a.reg);
                self.st(EAX, dst, k);
            }
            None => return false,
        }
        true
    }

    /// x87 `st0` = the float `v`.
    pub(crate) fn fld(&mut self, v: V, k: K) {
        if let Some(x) = self.dst_x_v(v) {
            self.a.sse_st(movs_st(k), Self::sp(0), Xmm(x));
            self.a.x87(fld(k), Self::sp(0));
            return;
        }
        let m = match v {
            V::M(m) => self.mem(m, if self.opt { EAX } else { ESI }),
            V::I(bits) => {
                self.a.mov_mi(Rm::M(Self::sp(0)), bits as u32);
                self.a.mov_mi(Rm::M(Self::sp(4)), (bits >> 32) as u32);
                Self::sp(0)
            }
        };
        self.a.x87(fld(k), m);
    }

    /// `ebx` aside (it may hold the GOT) in the scratch area.
    pub(crate) fn save_ebx(&mut self) {
        self.a.mov_st(Sz::D, Rm::M(Self::sp(12)), Gpr(EBX));
    }

    pub(crate) fn restore_ebx(&mut self) {
        self.a.mov(Sz::D, Gpr(EBX), Rm::M(Self::sp(12)));
    }

    pub(crate) fn rmw(&mut self, op: Rmw, k: K, dst: Mem, ptr: Mem, val: V) {
        self.ld(ECX, val, k, false);
        self.lea(ESI, ptr);
        let at = bd(ESI, 0);
        let sz = size(k);
        match op {
            Rmw::Xchg => {
                self.mov(EAX, ECX);
                self.a.xchg(sz, Rm::M(at), Gpr(EAX));
            }
            Rmw::Add | Rmw::Sub => {
                if op == Rmw::Sub {
                    self.a.un(enc::Un::Neg, gr(ECX));
                }
                self.a.lock();
                self.a.xadd(sz, at, Gpr(ECX));
                self.mov(EAX, ECX);
            }
            Rmw::And | Rmw::Or | Rmw::Xor => {
                // A compare-and-swap loop: eax = old, edx = new.
                self.extend(EAX, Rm::M(at), k, false);
                let top = self.a.pos();
                self.mov(EDX, EAX);
                let op = match op {
                    Rmw::And => Alu::And,
                    Rmw::Or => Alu::Or,
                    _ => Alu::Xor,
                };
                self.alu(op, EDX, ECX);
                self.a.lock();
                self.a.cmpxchg(sz, at, Gpr(EDX));
                self.a.jcc(Cond::NE, Some(top));
            }
        }
        self.st(EAX, dst, k);
    }

    /// An 8-byte read-modify-write: a `cmpxchg8b` loop, the operand in the
    /// scratch area.
    pub(crate) fn rmw64(&mut self, op: Rmw, dst: Mem, ptr: Mem, val: V) {
        self.save_ebx();
        self.ld2(ECX, ESI, val);
        self.a.mov_st(Sz::D, Rm::M(Self::sp(0)), Gpr(ECX));
        self.a.mov_st(Sz::D, Rm::M(Self::sp(4)), Gpr(ESI));
        self.lea(EDI, ptr);
        self.a.mov(Sz::D, Gpr(EAX), Rm::M(bd(EDI, 0)));
        self.a.mov(Sz::D, Gpr(EDX), Rm::M(bd(EDI, 4)));
        let top = self.a.pos();
        if op == Rmw::Xchg {
            self.a.mov(Sz::D, Gpr(EBX), Rm::M(Self::sp(0)));
            self.a.mov(Sz::D, Gpr(ECX), Rm::M(Self::sp(4)));
        } else {
            self.mov(EBX, EAX);
            self.mov(ECX, EDX);
            // `r, r/m` forms: low word, then high (with the carry).
            let (lo, hi) = match op {
                Rmw::Add => (Alu::Add, Alu::Adc),
                Rmw::Sub => (Alu::Sub, Alu::Sbb),
                Rmw::And => (Alu::And, Alu::And),
                Rmw::Or => (Alu::Or, Alu::Or),
                _ => (Alu::Xor, Alu::Xor),
            };
            self.a.alu_ld(lo, Gpr(EBX), Rm::M(Self::sp(0)));
            self.a.alu_ld(hi, Gpr(ECX), Rm::M(Self::sp(4)));
        }
        self.a.lock();
        self.a.cmpxchg8b(bd(EDI, 0));
        self.a.jcc(Cond::NE, Some(top));
        self.restore_ebx();
        self.st2(EAX, EDX, dst);
    }

    #[allow(clippy::too_many_arguments)]
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
        if k.is_float() {
            let (xa, xb) = if self.opt {
                (self.freg(a, k, XMM0), self.freg(b, k, XMM1))
            } else {
                self.ldf(XMM0, a, k);
                self.ldf(XMM1, b, k);
                (XMM0, XMM1)
            };
            let ucomi = |s: &mut Self, x: u32, y: u32| {
                let (x, y) = (
                    if x == XMM0 { xa } else { xb },
                    if y == XMM0 { xa } else { xb },
                );
                s.a.sse(ucomis(k), Xmm(x), xr(y));
            };
            match cc {
                Cc::Eq | Cc::Ne => {
                    ucomi(self, XMM0, XMM1);
                    let (c1, c2, comb) = if cc == Cc::Eq {
                        (Cond::E, Cond::NP, Alu::And)
                    } else {
                        (Cond::NE, Cond::P, Alu::Or)
                    };
                    self.a.setcc(c1, Gpr(EAX));
                    self.a.setcc(c2, Gpr(ECX));
                    self.a.alu(comb, Sz::B, gr(EAX), Gpr(ECX));
                    self.a.movzx(Gpr(EAX), Sz::B, gr(EAX));
                }
                Cc::Lt | Cc::Le | Cc::Gt | Cc::Ge => {
                    if matches!(cc, Cc::Lt | Cc::Le) {
                        ucomi(self, XMM1, XMM0);
                    } else {
                        ucomi(self, XMM0, XMM1);
                    }
                    let c = if matches!(cc, Cc::Lt | Cc::Gt) {
                        Cond::A
                    } else {
                        Cond::AE
                    };
                    if self.fuse(c, dst, next) {
                        return;
                    }
                    self.setcc(c);
                }
            }
            self.st(EAX, dst, K::I8);
            return;
        }
        let pick = |s: bool, signed_cc: Cond, unsigned_cc: Cond| {
            if s { signed_cc } else { unsigned_cc }
        };
        if wide(k) && self.opt {
            let c = match cc {
                Cc::Eq | Cc::Ne => {
                    self.ld2(EAX, EDX, a);
                    self.op64(Alu::Xor, Alu::Xor, b);
                    self.alu(Alu::Or, EAX, EDX);
                    if cc == Cc::Eq { Cond::E } else { Cond::NE }
                }
                _ => {
                    // The flags of a - b, or of b - a for > and <=.
                    let (x, y) = if matches!(cc, Cc::Gt | Cc::Le) {
                        (b, a)
                    } else {
                        (a, b)
                    };
                    self.ld2(EAX, EDX, x);
                    self.op64(Alu::Cmp, Alu::Sbb, y);
                    if matches!(cc, Cc::Lt | Cc::Gt) {
                        pick(signed, Cond::L, Cond::B)
                    } else {
                        pick(signed, Cond::GE, Cond::AE)
                    }
                }
            };
            if self.fuse(c, dst, next) {
                return;
            }
            self.setcc(c);
            self.st(EAX, dst, K::I8);
            return;
        }
        if wide(k) {
            self.ld2(EAX, EDX, a);
            self.ld2(ECX, ESI, b);
            let c = match cc {
                Cc::Eq | Cc::Ne => {
                    self.alu(Alu::Xor, EAX, ECX);
                    self.alu(Alu::Xor, EDX, ESI);
                    self.alu(Alu::Or, EAX, EDX);
                    if cc == Cc::Eq { Cond::E } else { Cond::NE }
                }
                Cc::Lt | Cc::Ge => {
                    // The flags of a - b.
                    self.alu(Alu::Cmp, EAX, ECX);
                    self.alu(Alu::Sbb, EDX, ESI);
                    if cc == Cc::Lt {
                        pick(signed, Cond::L, Cond::B)
                    } else {
                        pick(signed, Cond::GE, Cond::AE)
                    }
                }
                Cc::Gt | Cc::Le => {
                    // Of b - a.
                    self.alu(Alu::Cmp, ECX, EAX);
                    self.alu(Alu::Sbb, ESI, EDX);
                    if cc == Cc::Gt {
                        pick(signed, Cond::L, Cond::B)
                    } else {
                        pick(signed, Cond::GE, Cond::AE)
                    }
                }
            };
            if self.fuse(c, dst, next) {
                return;
            }
            self.setcc(c);
            self.st(EAX, dst, K::I8);
            return;
        }
        // A register's zero extension serves 4 bytes or unsigned.
        let direct = k.size() == 4 || !signed;
        let reg_of = |s: &Self, v: V| {
            s.ra.get_v(v)
                .filter(|x| !x.fp && s.opt && direct)
                .map(|x| x.reg)
        };
        let ra_ = reg_of(self, a);
        let rb = reg_of(self, b);
        let x = if let Some(r) = ra_ {
            r
        } else {
            self.ld(EAX, a, k, signed);
            EAX
        };
        if let V::I(i) = b {
            self.alu_imm(Alu::Cmp, gr(x), ext32(i, k, signed));
        } else if let Some(r) = rb {
            self.alu(Alu::Cmp, x, r);
        } else if let (true, V::M(m @ Mem::Ind(..)), 4) = (self.opt, b, k.size()) {
            let at = self.mem(m, ECX);
            self.a.alu_ld(Alu::Cmp, Gpr(x), Rm::M(at));
        } else {
            self.ld(ECX, b, k, signed);
            self.alu(Alu::Cmp, x, ECX);
        }
        let c = match cc {
            Cc::Eq => Cond::E,
            Cc::Ne => Cond::NE,
            Cc::Lt => pick(signed, Cond::L, Cond::B),
            Cc::Le => pick(signed, Cond::LE, Cond::BE),
            Cc::Gt => pick(signed, Cond::G, Cond::A),
            Cc::Ge => pick(signed, Cond::GE, Cond::AE),
        };
        if self.fuse(c, dst, next) {
            return;
        }
        if let Some(d) = self.dst_r(dst) {
            self.a.setcc(c, Gpr(EAX));
            self.a.movzx(Gpr(d), Sz::B, gr(EAX));
            return;
        }
        self.setcc(c);
        self.st(EAX, dst, K::I8);
    }
}
