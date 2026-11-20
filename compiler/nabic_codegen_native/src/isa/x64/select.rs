//! Instruction selection, branches and labels.

#[allow(clippy::wildcard_imports)]
use crate::isa::x64::*;

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
        self.flush(None);
        let (start, short) = (self.a.pos(), self.short());
        let at = if short {
            self.a.jmp8(None)
        } else {
            self.a.jmp(None)
        };
        self.branched(start, at, l, short);
    }

    pub(crate) fn jcc(&mut self, cc: Cond, l: Label) {
        self.flush(None);
        let (start, short) = (self.a.pos(), self.short());
        let at = if short {
            self.a.jcc8(cc, None)
        } else {
            self.a.jcc(cc, None)
        };
        self.branched(start, at, l, short);
    }

    /// A forward `jcc` to a not yet known spot: its rel32's position.
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

    /// `setcc al; movzx eax, al`.
    pub(crate) fn setcc(&mut self, cc: Cond) {
        self.a.setcc(cc, Gpr(RAX));
        self.a.movzx(Sz::D, Gpr(RAX), Sz::B, Rm::r(RAX));
    }

    pub(crate) fn inst(&mut self, i: &Inst, next: Option<&Inst>) {
        match i {
            Inst::Label(l) => {
                self.flush(None);
                self.labels[*l as usize] = Some(self.a.pos());
                self.held.clear();
            }
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
                let cc = if let Some(cc) = self.flags.take() {
                    // The comparison just before: its flags.
                    cc
                } else {
                    let r = self.reg(*c, K::I8, false, &[]);
                    self.a.test_rr(Sz::D, Gpr(r), Gpr(r));
                    Cond::NE
                };
                if self.opt && matches!(next, Some(Inst::Label(n)) if n == t) {
                    // `t` next: the inverse to `f`.
                    self.jcc(cc.inv(), *f);
                    return;
                }
                self.jcc(cc, *t);
                if !matches!(next, Some(Inst::Label(n)) if n == f) {
                    self.jmp(*f);
                }
            }
            Inst::Mov { k, dst, src } => match self.ra.get(*dst) {
                // Straight into the register (kept zero-extended).
                Some(a) if a.fp => self.load_fp(a.reg, *src, *k),
                Some(a) => self.load(a.reg, *src, *k, false),
                None => {
                    let r = self.reg(*src, *k, false, &[]);
                    self.store(r, *dst, *k);
                }
            },
            Inst::Copy { dst, src, size } => {
                if let Some(k) = scalar(*size) {
                    // One scalar's bytes: a move.
                    let f = if k == K::I64 { K::F64 } else { K::F32 };
                    match (self.ra.get(*dst), self.ra.get(*src)) {
                        (Some(a), _) if a.fp => self.load_fp(a.reg, V::M(*src), f),
                        (Some(a), _) => self.load(a.reg, V::M(*src), k, false),
                        (None, Some(a)) if a.fp => self.store_fp(a.reg, *dst, f),
                        _ => {
                            let r = self.reg(V::M(*src), k, false, &[]);
                            self.store(r, *dst, k);
                        }
                    }
                } else {
                    self.copy(*dst, *src, *size, false);
                }
            }
            Inst::Zero { dst, size } => self.copy(*dst, *dst, *size, true),
            Inst::Addr { dst, src } => {
                let r = self
                    .dst_r(*dst)
                    .unwrap_or_else(|| self.pick(&POOL, false, &[]));
                self.lea(r, *src);
                self.store(r, *dst, K::I64);
            }
            Inst::Sym { dst, sym, got } => {
                // COFF has no GOT: everything is in the image (a DLL's
                // functions through import thunks).
                let r = self
                    .dst_r(*dst)
                    .unwrap_or_else(|| self.pick(&POOL, false, &[]));
                self.w(r, false);
                if *got && !self.obj.coff {
                    let m = Addr::Rip(RelKind::X86GotLoad, *sym);
                    self.a.mov(Sz::Q, Gpr(r), Rm::M(m));
                } else {
                    self.a.lea(Gpr(r), Addr::Rip(RelKind::X86Pc32, *sym));
                }
                self.store(r, *dst, K::I64);
            }
            Inst::Tls { dst, sym } => {
                self.scratch();
                if self.obj.coff {
                    // The TEB's TLS array at `_tls_index`, plus the
                    // offset in the TLS section.
                    let index = self.obj.sym("_tls_index");
                    let m = Addr::Rip(RelKind::X86Pc32, index);
                    self.a.mov(Sz::D, Gpr(RAX), Rm::M(m));
                    let teb = Rm::M(Addr::Abs(Seg::Gs, 0x58));
                    self.a.mov(Sz::Q, Gpr(RCX), teb);
                    let slot = Addr::Bi(Gpr(RCX), Gpr(RAX), 3);
                    self.a.mov(Sz::Q, Gpr(RCX), Rm::M(slot));
                    let var = Addr::BdRel(Gpr(RCX), RelKind::X86SecRel, *sym);
                    self.a.lea(Gpr(RAX), var);
                } else if self.obj.dyn_tls {
                    // ELF general dynamic, as linkers relax it: the
                    // address from `__tls_get_addr`.
                    let get = self.obj.sym("__tls_get_addr");
                    self.a.tls_gd(*sym, get);
                } else {
                    // ELF initial exec: the offset from the thread pointer.
                    let m = Addr::Rip(RelKind::X86Tls, *sym);
                    self.a.mov(Sz::Q, Gpr(RAX), Rm::M(m));
                    let tp = Rm::M(Addr::Abs(Seg::Fs, 0));
                    self.a.mov(Sz::Q, Gpr(RCX), tp);
                    self.alu(Alu::Add, true, RAX, RCX);
                }
                self.store(RAX, *dst, K::I64);
            }
            Inst::Bin {
                op,
                k,
                signed,
                dst,
                a,
                b,
            } => {
                if self.opt {
                    if let Some(s) = crate::lir::scaled(i, next, &self.waits, 3) {
                        // The add that reads it scales the index.
                        self.scaled = Some(s);
                        return;
                    }
                    if let Some((t, idx, sh)) = self.scaled.take() {
                        let other = if *a == V::M(t) { *b } else { *a };
                        let rn = self.reg(other, K::I64, false, &[]);
                        let rm = self.reg(idx, K::I64, false, &[rn]);
                        let rd = self
                            .dst_r(*dst)
                            .unwrap_or_else(|| self.pick(&POOL, false, &[rn, rm]));
                        self.lea_sib(rd, rn, rm, sh);
                        self.store(rd, *dst, K::I64);
                        return;
                    }
                }
                if self.opt && self.test(*op, *k, *signed, *dst, *a, *b, next) {
                    return;
                }
                if self.opt && self.div_pow2(*op, *k, *signed, *dst, *a, *b) {
                    return;
                }
                self.bin(*op, *k, *signed, *dst, *a, *b);
            }
            Inst::Ovf {
                op,
                k: k @ (K::I64 | K::I32),
                signed,
                dst,
                a,
                b,
                fail,
            } if self.opt
                && (matches!(op, Bin::Add | Bin::Sub) || (*op == Bin::Mul && *signed)) =>
            {
                // In place: the fail path, a panic, reads none of these.
                let w = *k == K::I64;
                let imm = match (op, b) {
                    (Bin::Add | Bin::Sub, V::I(i)) if w => i32::try_from(*i).ok(),
                    (Bin::Add | Bin::Sub, V::I(i)) => Some(*i as i32),
                    _ => None,
                };
                let ra = self.reg(*a, *k, *signed, &[]);
                let rb = match imm {
                    Some(_) => None,
                    None if b == a => Some(ra),
                    None => Some(self.reg(*b, *k, *signed, &[ra])),
                };
                let rd = self
                    .dst_r(*dst)
                    .filter(|&d| Some(d) != rb || d == ra)
                    .unwrap_or_else(|| self.pick(&POOL, false, &[ra, rb.unwrap_or(ra)]));
                self.mov_rr(rd, ra);
                match (imm, rb) {
                    (Some(i), _) => self.alu_imm(alu(*op), Sz::dq(w), Rm::r(rd), i),
                    (None, Some(rb)) => match op {
                        Bin::Add | Bin::Sub => self.alu(alu(*op), w, rd, rb),
                        _ => self.a.imul(Sz::dq(w), Gpr(rd), Rm::r(rb)),
                    },
                    (None, None) => {}
                }
                let cc = if *signed || *op == Bin::Mul {
                    Cond::O
                } else {
                    Cond::B
                };
                self.jcc(cc, *fail);
                self.store_ext(rd, *dst, *k, (!w).then_some(false));
            }
            Inst::Ovf {
                op,
                k,
                signed,
                dst,
                a,
                b,
                fail,
            } => {
                let imm = match (k, op, b) {
                    (K::I64, Bin::Add | Bin::Sub, V::I(i)) => i32::try_from(*i).ok(),
                    (K::I32, Bin::Add | Bin::Sub, V::I(i)) => Some(*i as i32),
                    _ => None,
                };
                if imm.is_none() {
                    self.load(RCX, *b, *k, *signed);
                }
                self.load(RAX, *a, *k, *signed);
                self.scratch();
                match k {
                    K::I64 | K::I32 => {
                        let w = *k == K::I64;
                        let s = Sz::dq(w);
                        match op {
                            // add / sub rax, imm32
                            _ if imm.is_some() => {
                                let i = imm.unwrap_or(0);
                                self.alu_imm(alu(*op), s, Rm::r(RAX), i);
                            }
                            Bin::Add | Bin::Sub => self.alu(alu(*op), w, RAX, RCX),
                            _ if *signed => self.a.imul(s, Gpr(RAX), Rm::r(RCX)),
                            // mul rcx: rdx:rax; OF / CF when the high half is used.
                            _ => self.a.un(enc::Un::Mul, s, Rm::r(RCX)),
                        }
                        let cc = match (op, signed) {
                            (_, true) | (Bin::Mul, false) => Cond::O,
                            _ => Cond::B,
                        };
                        self.jcc(cc, *fail);
                    }
                    _ => {
                        // 8 / 16 bits in 32: the result must equal its
                        // own extension.
                        self.bin_int(*op, K::I32, *signed);
                        let from = if *k == K::I8 { Sz::B } else { Sz::W };
                        if *signed {
                            self.a.movsx(Sz::D, Gpr(RCX), from, Rm::r(RAX));
                        } else {
                            self.a.movzx(Sz::D, Gpr(RCX), from, Rm::r(RAX));
                        }
                        self.alu(Alu::Cmp, false, RAX, RCX);
                        self.jcc(Cond::NE, *fail);
                    }
                }
                self.store(RAX, *dst, *k);
            }
            Inst::Un { op, k, dst, a } => self.un(*op, *k, *dst, *a),
            Inst::Fma { k, dst, a, b, c } => self.fma(*k, *dst, *a, *b, *c),
            Inst::Cmp {
                cc,
                k,
                signed,
                dst,
                a,
                b,
            } => {
                // An immediate first compares second, the predicate swapped.
                let (cc, a, b) = if self.opt && matches!((a, b), (V::I(_), V::M(_))) {
                    (&cc.swap(), b, a)
                } else {
                    (cc, a, b)
                };
                if k.is_float() {
                    let (x0, x1) = if self.opt {
                        let va = self.freg(*a, *k, &[]);
                        (va, self.freg(*b, *k, &[va]))
                    } else {
                        self.load_fp(XMM0, *a, *k);
                        self.load_fp(XMM1, *b, *k);
                        (XMM0, XMM1)
                    };
                    self.scratch();
                    let ucomi = Self::scalar(*k, Sse::Ucomiss, Sse::Ucomisd);
                    let ucomi = |s: &mut Self, x: u32, y: u32| {
                        s.a.sse(ucomi, Xmm(x), Xrm::X(Xmm(y)));
                    };
                    match cc {
                        Cc::Eq | Cc::Ne => {
                            ucomi(self, x0, x1);
                            let (c1, c2, comb) = if *cc == Cc::Eq {
                                (Cond::E, Cond::NP, Alu::And)
                            } else {
                                (Cond::NE, Cond::P, Alu::Or)
                            };
                            self.a.setcc(c1, Gpr(RAX));
                            self.a.setcc(c2, Gpr(RCX));
                            self.a.alu_rr(comb, Sz::B, Gpr(RAX), Gpr(RCX));
                            self.a.movzx(Sz::D, Gpr(RAX), Sz::B, Rm::r(RAX));
                        }
                        Cc::Lt | Cc::Le | Cc::Gt | Cc::Ge => {
                            if matches!(cc, Cc::Lt | Cc::Le) {
                                ucomi(self, x1, x0);
                            } else {
                                ucomi(self, x0, x1);
                            }
                            let c = if matches!(cc, Cc::Lt | Cc::Gt) {
                                Cond::A
                            } else {
                                Cond::AE
                            };
                            self.setcc(c);
                            if matches!(next, Some(Inst::Br { c: V::M(m), .. }) if m == dst) {
                                self.flags = Some(c);
                            }
                        }
                    }
                    self.store_ext(RAX, *dst, K::I8, Some(false));
                    return;
                }
                let tested = self.tested.take().is_some_and(|t| *a == V::M(t))
                    && *b == V::I(0)
                    && matches!(cc, Cc::Eq | Cc::Ne);
                let ra = if tested {
                    0
                } else {
                    self.reg(*a, *k, *signed, &[])
                };
                match b {
                    // The `and` / `test` before set the flags.
                    _ if tested => {}
                    // cmp ra, simm32 (the immediate extended as `load` would)
                    V::I(i) if i32::try_from(ext(*i, *k, *signed)).is_ok() => {
                        let i = ext(*i, *k, *signed) as i32;
                        self.alu_imm(Alu::Cmp, Sz::Q, Rm::r(ra), i);
                    }
                    _ => {
                        let rb = if b == a {
                            ra
                        } else {
                            self.reg(*b, *k, *signed, &[ra])
                        };
                        self.alu(Alu::Cmp, true, ra, rb);
                    }
                }
                let c = match (cc, signed) {
                    (Cc::Eq, _) => Cond::E,
                    (Cc::Ne, _) => Cond::NE,
                    (Cc::Lt, true) => Cond::L,
                    (Cc::Lt, false) => Cond::B,
                    (Cc::Le, true) => Cond::LE,
                    (Cc::Le, false) => Cond::BE,
                    (Cc::Gt, true) => Cond::G,
                    (Cc::Gt, false) => Cond::A,
                    (Cc::Ge, true) => Cond::GE,
                    (Cc::Ge, false) => Cond::AE,
                };
                // A branch right after on it takes the flags; when it is
                // the only reader, nothing else is left to do.
                if matches!(next, Some(Inst::Br { c: V::M(m), .. }) if m == dst) {
                    self.flags = Some(c);
                    if let Mem::Frame(s, o) = dst {
                        if self.waits.get(&(*s, *o)) == Some(&(1, 1)) {
                            return;
                        }
                    }
                }
                // Stores going out leave the flags alone.
                let rd = self
                    .dst_r(*dst)
                    .unwrap_or_else(|| self.pick(&POOL, false, &[]));
                self.w(rd, false);
                self.a.setcc(c, Gpr(rd));
                self.a.movzx(Sz::D, Gpr(rd), Sz::B, Rm::r(rd));
                self.store_ext(rd, *dst, K::I8, Some(false));
            }
            Inst::Conv {
                conv,
                from,
                to,
                dst,
                a,
            } => self.conv(*conv, *from, *to, *dst, *a),
            Inst::AtomicLoad { k, dst, ptr, .. } => {
                self.lea(R10, *ptr);
                self.scratch();
                self.sized_load(*k, RAX, Rm::m(R10, 0));
                self.store(RAX, *dst, *k);
            }
            Inst::AtomicStore { k, ptr, src, ord } => {
                self.load(RAX, *src, *k, false);
                self.lea(R10, *ptr);
                self.scratch();
                // x86 stores release as they are; only seq_cst needs xchg.
                if *ord == Order::SeqCst {
                    self.xchg(*k);
                } else {
                    self.a.mov_st(sz(*k), Rm::m(R10, 0), Gpr(RAX));
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
                self.load(RCX, *val, *k, false);
                self.lea(R10, *ptr);
                self.scratch();
                let at_r10 = Addr::Bd(Gpr(R10), 0);
                match op {
                    Rmw::Xchg => {
                        self.a.mov_st(Sz::Q, Rm::r(RAX), Gpr(RCX));
                        self.xchg(*k);
                    }
                    Rmw::Add | Rmw::Sub => {
                        if *op == Rmw::Sub {
                            self.a.un(enc::Un::Neg, Sz::Q, Rm::r(RCX));
                        }
                        self.a.lock();
                        self.a.xadd(sz(*k), at_r10, Gpr(RCX));
                        self.a.mov_st(Sz::Q, Rm::r(RAX), Gpr(RCX));
                    }
                    Rmw::And | Rmw::Or | Rmw::Xor => {
                        // A compare-and-swap loop: rax = old, rdx = new.
                        self.sized_load(*k, RAX, Rm::M(at_r10));
                        let top = self.a.pos();
                        self.a.mov_st(Sz::Q, Rm::r(RDX), Gpr(RAX));
                        let op = match op {
                            Rmw::And => Alu::And,
                            Rmw::Or => Alu::Or,
                            _ => Alu::Xor,
                        };
                        self.alu(op, true, RDX, RCX);
                        self.a.lock();
                        self.a.cmpxchg(sz(*k), at_r10, Gpr(RDX));
                        self.a.jcc(Cond::NE, Some(top));
                    }
                }
                self.store(RAX, *dst, *k);
            }
            Inst::Cas {
                k,
                dst,
                ptr,
                expected,
                new,
                ..
            } => {
                self.load(RAX, *expected, *k, false);
                self.load(RCX, *new, *k, false);
                self.lea(R10, *ptr);
                self.scratch();
                self.a.lock();
                self.a.cmpxchg(sz(*k), Addr::Bd(Gpr(R10), 0), Gpr(RCX));
                self.store(RAX, *dst, *k);
            }
            Inst::Call {
                callee,
                args,
                rets,
                sret,
                variadic,
                ..
            } => self.call(callee, args, rets, *sret, *variadic),
            Inst::Param { part, dst } => match part.loc {
                Loc::Int(r) => self.store(self.arg_reg(r), *dst, part.k),
                Loc::Fp(r) => self.store_fp(u32::from(r), *dst, part.k),
                Loc::Stack(o) => {
                    // rax: no parameter comes in it.
                    self.w(RAX, false);
                    self.a.mov(Sz::Q, Gpr(RAX), Rm::m(RBP, 16 + o as i32));
                    self.store(RAX, *dst, part.k);
                }
                Loc::Vec(r) => self.vec_store(u32::from(r), *dst),
                Loc::VecStack(o) => {
                    self.vec_get(XMM0, Addr::Bd(Gpr(RBP), 16 + o as i32));
                    self.vec_store(XMM0, *dst);
                }
                Loc::Sret => self.store(self.sret_reg(), *dst, K::I64),
                Loc::Area(o) => {
                    self.w(RAX, false);
                    self.a.lea(Gpr(RAX), Addr::Bd(Gpr(RBP), 16 + o as i32));
                    self.store(RAX, *dst, K::I64);
                }
            },
            Inst::Ret { vals } => {
                // x87 results: the last pushed first, so `st0` is the first.
                let x87 = [3, 2]
                    .into_iter()
                    .filter_map(|n| vals.iter().find(|(p, _)| p.loc == Loc::Fp(n)));
                for (p, v) in x87 {
                    let m = match v {
                        V::M(m) => {
                            // From memory: what a register holds goes there.
                            self.flush(None);
                            addr(self.mem(*m, R10))
                        }
                        V::I(bits) => {
                            self.mov_imm(RAX, *bits as u64);
                            self.a.mov_st(Sz::Q, Rm::m(RSP, -8), Gpr(RAX));
                            Addr::Bd(Gpr(RSP), -8)
                        }
                    };
                    self.a.fld(sz(p.k), m);
                }
                // Floats first: a float constant passes through rax.
                for (p, v) in vals {
                    if let Loc::Fp(r @ 0..2) = p.loc {
                        self.load_fp(u32::from(r), *v, p.k);
                    }
                    if let Loc::Vec(r) = p.loc {
                        self.vec_load(u32::from(r), v.vector_mem());
                    }
                }
                for (p, v) in vals {
                    if let Loc::Int(r) = p.loc {
                        self.load(RETS[r as usize], *v, p.k, p.signed);
                    }
                }
                // The frame dies.
                self.pending.clear();
                self.epilogue();
                self.held.clear();
            }
            Inst::VLoop(v) => self.vloop(v),
            Inst::V(x) => self.vx(x),
            Inst::Trap => {
                self.pending.clear();
                self.a.ud2();
            }
            Inst::Line(line, col, scope) => {
                let at = self.a.pos() as u32;
                self.lines.push((at, *line, *col, *scope));
            }
        }
    }
}
