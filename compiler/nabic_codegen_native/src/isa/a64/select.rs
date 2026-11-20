//! Instruction selection, branches and atomics.

#[allow(clippy::wildcard_imports)]
use crate::isa::a64::*;

impl Em<'_> {
    pub(crate) fn b(&mut self, l: Label) {
        self.flush(None);
        let at = self.pos();
        self.ins(enc::b(false, 0));
        self.fixes.push((at, Fix::B(l)));
    }

    pub(crate) fn bcond(&mut self, cond: Cond, l: Label) {
        self.flush(None);
        if self.short {
            let at = self.pos();
            self.ins(enc::b_cond(cond, 0));
            self.fixes.push((at, Fix::C(l)));
            return;
        }
        // Inverted around an unconditional branch: any distance.
        self.ins(enc::b_cond(cond.inv(), 2));
        self.b(l);
    }

    /// `cbz` / `cbnz` (`nz`) `w<r>` to `l`.
    pub(crate) fn cbz(&mut self, nz: bool, r: u32, l: Label) {
        self.flush(None);
        let at = self.pos();
        self.ins(enc::cbz(nz, R(r), 0));
        self.fixes.push((at, Fix::C(l)));
    }

    pub(crate) fn cset(&mut self, rd: u32, cond: Cond) {
        self.ins(enc::cset(R(rd), cond));
    }

    /// `cmp xn, xm` (`sf` from `k`).
    pub(crate) fn cmp_rr(&mut self, k: K, rn: u32, rm: u32) {
        self.ins(enc::cmp(sf(k), R(rn), R(rm)));
    }

    pub(crate) fn inst(&mut self, i: &Inst, next: Option<&Inst>) {
        match i {
            Inst::Label(l) => {
                self.flush(None);
                if self.heads.get(*l as usize) == Some(&true) {
                    while (self.start as usize / 4 + self.pos()) % 4 != 0 {
                        self.a.push(enc::nop());
                    }
                }
                self.labels[*l as usize] = Some(self.pos());
                self.forget();
            }
            Inst::Jmp(l) => {
                if !matches!(next, Some(Inst::Label(n)) if n == l) {
                    self.b(*l);
                }
            }
            Inst::Br { c, t, f } => {
                if let V::I(v) = c {
                    self.b(if *v & 0xff != 0 { *t } else { *f });
                    return;
                }
                let to_t = matches!(next, Some(Inst::Label(n)) if n == t);
                let to_f = matches!(next, Some(Inst::Label(n)) if n == f);
                if self.short {
                    // Straight to the target; `t` next: inverted to `f`.
                    let flags = self.flags.take();
                    let r = flags.is_none().then(|| self.reg(*c, K::I8, false, &[]));
                    match (flags, r) {
                        (Some(cond), _) if to_t => self.bcond(cond.inv(), *f),
                        (Some(cond), _) => self.bcond(cond, *t),
                        (_, Some(r)) if to_t => self.cbz(false, r, *f),
                        (_, r) => self.cbz(true, r.unwrap_or(ZR), *t),
                    }
                    if !to_t && !to_f {
                        self.b(*f);
                    }
                    return;
                }
                if let Some(cond) = self.flags.take() {
                    // The comparison just before: its flags.
                    self.flush(None);
                    self.ins(enc::b_cond(cond.inv(), 2));
                } else {
                    let r = self.reg(*c, K::I8, false, &[]);
                    self.flush(None);
                    self.ins(enc::cbz(false, R(r), 2));
                }
                self.b(*t);
                if !matches!(next, Some(Inst::Label(n)) if n == f) {
                    self.b(*f);
                }
            }
            Inst::Mov { k, dst, src } => match self.ra.get(*dst) {
                // Straight into the register (kept zero-extended).
                Some(a) if a.fp => self.load_fp(a.reg, *src, *k),
                Some(a) => self.load(a.reg, *src, *k, false),
                None => match self.ra.get_v(*src) {
                    // A float straight from its register.
                    Some(a) if a.fp && k.is_float() => self.store_fp(a.reg, *dst, *k),
                    // Zero from the zero register.
                    None if self.opt && *src == V::I(0) && !k.is_float() => {
                        let (b, o) = self.base(*dst, X15);
                        self.wrote(*dst, k.size());
                        self.ldst(Self::store_op(*k), ZR, b, o);
                    }
                    _ => {
                        let r = self.reg(*src, *k, false, &[]);
                        self.store(r, *dst, *k);
                    }
                },
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
                let r = self.dst_x(*dst).unwrap_or_else(|| self.pick(&POOL, &[]));
                self.addr(r, *src);
                self.store(r, *dst, K::I64);
            }
            Inst::Sym { dst, sym, got } => {
                // COFF has no GOT: everything is in the image (a DLL's
                // functions through import thunks).
                let got = *got && !self.obj.coff;
                let (p, o) = if got {
                    (RelKind::GotPage21, RelKind::GotPageOff12)
                } else {
                    (RelKind::Page21, RelKind::PageOff12)
                };
                let r = self.dst_x(*dst).unwrap_or_else(|| self.pick(&POOL, &[]));
                self.reloc(p, *sym);
                self.ins(enc::adrp(R(r)));
                self.reloc(o, *sym);
                if got {
                    self.ins(enc::ldst(LDRX, r, R(r), 0));
                } else {
                    self.ins(enc::add_imm(AddOp::Add, true, R(r), R(r), 0, false));
                }
                self.store(r, *dst, K::I64);
            }
            Inst::Tls { dst, sym } if self.obj.coff => {
                // The TEB's (x18) TLS array at `_tls_index`, plus the
                // offset in the TLS section.
                let index = self.obj.sym("_tls_index");
                self.ins(enc::ldst(LDRX, X16, R(18), 0x58 / 8));
                self.reloc(RelKind::Page21, index);
                self.ins(enc::adrp(R(X17)));
                self.reloc(RelKind::PageOff12, index);
                self.ins(enc::ldst(LDRW, X17, R(X17), 0));
                self.ins(enc::ldst_reg(LDRX, X16, R(X16), R(X17), true));
                self.reloc(RelKind::SecRelHi12, *sym);
                self.ins(enc::add_imm(AddOp::Add, true, R(0), R(X16), 0, true));
                self.reloc(RelKind::SecRelLo12, *sym);
                self.ins(enc::add_imm(AddOp::Add, true, R(0), R(0), 0, false));
                self.store(0, *dst, K::I64);
            }
            Inst::Tls { dst, sym } if self.obj.dyn_tls => {
                // A TLS descriptor (x0, x1, as linkers relax it): the
                // resolver returns the offset from the thread pointer.
                self.flush(None);
                self.reloc(RelKind::TlsDescPage21, *sym);
                self.ins(enc::adrp(R(0)));
                self.reloc(RelKind::TlsDescLd, *sym);
                self.ins(enc::ldst(LDRX, 1, R(0), 0));
                self.reloc(RelKind::TlsDescAdd, *sym);
                self.ins(enc::add_imm(AddOp::Add, true, R(0), R(0), 0, false));
                self.reloc(RelKind::TlsDescCall, *sym);
                self.ins(enc::blr(R(1)));
                self.forget();
                self.ins(enc::mrs_tpidr(R(X16)));
                let add = enc::add_reg(AddOp::Add, true, R(0), R(X16), R(0), Shift::Lsl, 0);
                self.ins(add);
                self.store(0, *dst, K::I64);
            }
            Inst::Tls { dst, sym } => {
                self.reloc(RelKind::TlvPage21, *sym);
                self.ins(enc::adrp(R(0)));
                self.reloc(RelKind::TlvPageOff12, *sym);
                self.ins(enc::ldst(LDRX, 0, R(0), 0));
                if self.obj.elf {
                    // Initial exec: the thread pointer plus the offset.
                    self.ins(enc::mrs_tpidr(R(X16)));
                    let add = enc::add_reg(AddOp::Add, true, R(0), R(X16), R(0), Shift::Lsl, 0);
                    self.ins(add);
                } else {
                    // Mach-O: the descriptor's getter.
                    self.ins(enc::ldst(LDRX, X16, R(0), 0));
                    self.flush(None);
                    self.ins(enc::blr(R(X16)));
                    self.forget();
                }
                self.store(0, *dst, K::I64);
            }
            Inst::Bin {
                op,
                k,
                signed,
                dst,
                a,
                b,
            } => {
                if k.is_float() {
                    let va = self.freg(*a, *k, &[]);
                    let vb = if b == a { va } else { self.freg(*b, *k, &[va]) };
                    let vd = self.dst_v(*dst).unwrap_or_else(|| self.pick(&FP_POOL, &[]));
                    let op = match op {
                        Bin::Add => Fp::Fadd,
                        Bin::Sub => Fp::Fsub,
                        Bin::Mul => Fp::Fmul,
                        _ => Fp::Fdiv,
                    };
                    self.ins(enc::fp(op, *k == K::F64, Vr(vd), Vr(va), Vr(vb)));
                    self.store_fp(vd, *dst, *k);
                    return;
                }
                if self.opt {
                    if let Some(s) = crate::lir::scaled(i, next, &self.waits, 63) {
                        // The add that reads it scales the index.
                        self.scaled = Some(s);
                        return;
                    }
                    if let Some((t, idx, sh)) = self.scaled.take() {
                        let other = if *a == V::M(t) { *b } else { *a };
                        let rn = self.reg(other, K::I64, false, &[]);
                        let rm = self.reg(idx, K::I64, false, &[rn]);
                        let rd = self.dst_x(*dst).unwrap_or_else(|| self.pick(&POOL, &[]));
                        let add =
                            enc::add_reg(AddOp::Add, true, R(rd), R(rn), R(rm), Shift::Lsl, sh);
                        self.ins(add);
                        self.store(rd, *dst, K::I64);
                        return;
                    }
                }
                if self.opt && self.test(*op, *k, *signed, *dst, *a, *b, next) {
                    return;
                }
                // `rem` computes the quotient into x12 first, a rotate
                // the negated amount.
                let fixed: &[u32] = if matches!(op, Bin::Rem | Bin::Rotl) {
                    &[X12]
                } else {
                    &[]
                };
                let ra = self.reg(*a, *k, *signed, fixed);
                // A 32-bit operation zero-extends its result.
                let ext = (self.opt && *k == K::I32).then_some(false);
                let target = self.dst_x(*dst);
                if let V::I(imm) = b {
                    let rd = target.unwrap_or_else(|| self.pick(&POOL, &[]));
                    if self.bin_imm(*op, *k, *signed, rd, ra, *imm) {
                        self.store_ext(rd, *dst, *k, ext);
                        return;
                    }
                }
                let rb = if b == a {
                    ra
                } else {
                    self.reg(*b, *k, *signed, &[&[ra], fixed].concat())
                };
                let rd = target.unwrap_or_else(|| self.pick(&POOL, fixed));
                self.bin_int(*op, *k, *signed, rd, ra, rb);
                self.store_ext(rd, *dst, *k, ext);
            }
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
            Inst::Fma { k, dst, a, b, c } => {
                let va = self.freg(*a, *k, &[]);
                let vb = self.freg(*b, *k, &[va]);
                let vc = self.freg(*c, *k, &[va, vb]);
                let vd = self.dst_v(*dst).unwrap_or_else(|| self.pick(&FP_POOL, &[]));
                self.ins(enc::fmadd(*k == K::F64, Vr(vd), Vr(va), Vr(vb), Vr(vc)));
                self.store_fp(vd, *dst, *k);
            }
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
                let cond = if k.is_float() {
                    let va = self.freg(*a, *k, &[]);
                    let vb = if b == a { va } else { self.freg(*b, *k, &[va]) };
                    self.ins(enc::fp(Fp::Fcmp, *k == K::F64, Vr(0), Vr(va), Vr(vb)));
                    match cc {
                        Cc::Eq => Cond::EQ,
                        Cc::Ne => Cond::NE,
                        Cc::Lt => Cond::MI,
                        Cc::Le => Cond::LS,
                        Cc::Gt => Cond::GT,
                        Cc::Ge => Cond::GE,
                    }
                } else if self.tested.take().is_some_and(|t| *a == V::M(t))
                    && *b == V::I(0)
                    && matches!(cc, Cc::Eq | Cc::Ne)
                {
                    // `test` set the flags.
                    if *cc == Cc::Eq { Cond::EQ } else { Cond::NE }
                } else {
                    let ra = self.reg(*a, *k, *signed, &[]);
                    let imm = matches!(b, V::I(v) if self.cmp_imm(*k, *signed, ra, *v));
                    if !imm {
                        let rb = if b == a {
                            ra
                        } else {
                            self.reg(*b, *k, *signed, &[ra])
                        };
                        self.cmp_rr(*k, ra, rb);
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
                // A branch right after on it takes the flags; when it is
                // the only reader, nothing else is left to do.
                if matches!(next, Some(Inst::Br { c: V::M(m), .. }) if m == dst) {
                    self.flags = Some(cond);
                    if let Mem::Frame(s, o) = dst {
                        if self.waits.get(&(*s, *o)) == Some(&(1, 1)) {
                            return;
                        }
                    }
                }
                let rd = self.dst_x(*dst).unwrap_or_else(|| self.pick(&POOL, &[]));
                self.cset(rd, cond);
                self.store_ext(rd, *dst, K::I8, Some(false));
            }
            Inst::Conv {
                conv,
                from,
                to,
                dst,
                a,
            } => self.conv(*conv, *from, *to, *dst, *a),
            Inst::AtomicLoad { k, dst, ptr, ord } => {
                self.addr_of_ptr(X14, *ptr);
                let sz = k.size().trailing_zeros();
                match ord {
                    Order::Relaxed => self.ldst(Self::load_op(*k, false), X9, X14, 0),
                    // RCpc: an acquire may pass an earlier release.
                    Order::Acquire | Order::AcqRel if self.obj.rcpc => {
                        self.ins(enc::excl(Excl::Ldapr, sz, R(X9), R(X14)));
                    }
                    _ => self.ins(enc::excl(Excl::Ldar, sz, R(X9), R(X14))),
                }
                self.store(X9, *dst, *k);
            }
            Inst::AtomicStore { k, ptr, src, ord } => {
                self.load(X9, *src, *k, false);
                self.addr_of_ptr(X14, *ptr);
                let sz = k.size().trailing_zeros();
                if ord.rel() {
                    self.ins(enc::excl(Excl::Stlr, sz, R(X9), R(X14)));
                } else {
                    self.ldst(Self::store_op(*k), X9, X14, 0);
                }
                self.wrote(*ptr, k.size());
            }
            Inst::AtomicRmw {
                op,
                k,
                dst,
                ptr,
                val,
                ord,
            } => {
                self.load(X10, *val, *k, false);
                self.addr_of_ptr(X14, *ptr);
                let sz = k.size().trailing_zeros();
                if self.obj.lse {
                    self.lse_rmw(*op, sz, *ord);
                } else if self.obj.outline {
                    self.llsc_enter();
                    let skip = self.lse_test();
                    self.lse_rmw(*op, sz, *ord);
                    let out = self.lse_join(skip);
                    self.llsc_rmw(*op, sz, *ord);
                    self.lse_done(out);
                } else {
                    self.llsc_rmw(*op, sz, *ord);
                }
                self.wrote(*ptr, k.size());
                self.store(X9, *dst, *k);
            }
            Inst::Cas {
                k,
                dst,
                ptr,
                expected,
                new,
                ord,
            } => {
                self.load(X9, *expected, *k, false);
                self.load(X10, *new, *k, false);
                self.addr_of_ptr(X14, *ptr);
                let sz = k.size().trailing_zeros();
                let (a, l) = (ord.acq(), ord.rel());
                if self.obj.lse {
                    // `cas` writes `rs`.
                    self.flush_reg(X9);
                    self.ins(enc::cas(sz, a, l, R(X9), R(X10), R(X14)));
                    self.wrote(*ptr, k.size());
                    // `cas` writes `rs`.
                    self.clobber(X9);
                    self.store(X9, *dst, *k);
                } else if self.obj.outline {
                    self.llsc_enter();
                    let skip = self.lse_test();
                    self.a.push(enc::cas(sz, a, l, R(X9), R(X10), R(X14)));
                    self.a.push(enc::mov(true, R(X11), R(X9)));
                    let out = self.lse_join(skip);
                    self.llsc_cas(sz, *ord);
                    self.lse_done(out);
                    self.wrote(*ptr, k.size());
                    self.store(X11, *dst, *k);
                } else {
                    self.llsc_cas(sz, *ord);
                    self.wrote(*ptr, k.size());
                    self.store(X11, *dst, *k);
                }
            }
            Inst::Call {
                callee,
                args,
                rets,
                sret,
                ..
            } => {
                for (p, v) in args {
                    match p.loc {
                        Loc::Stack(o) => {
                            if p.k.is_float() {
                                self.load(X9, *v, p.k, false);
                            } else {
                                self.load(X9, *v, p.k, p.signed);
                            }
                            self.ldst(Self::store_op(p.k), X9, SP, i64::from(o));
                        }
                        Loc::Int(r) => self.load(u32::from(r), *v, p.k, p.signed),
                        Loc::Fp(r) => self.load_fp(u32::from(r), *v, p.k),
                        Loc::Vec(r) => self.vec_load(u32::from(r), v.vector_mem()),
                        Loc::VecStack(o) => {
                            self.vec_load(V16, v.vector_mem());
                            self.ldst(enc::STRQ, V16, SP, i64::from(o));
                        }
                        Loc::Sret | Loc::Area(_) => {}
                    }
                }
                if let Some(m) = sret {
                    self.addr(8, *m);
                }
                if let Callee::Ptr(m) = callee {
                    self.load(X16, V::M(*m), K::I64, false);
                }
                self.flush(None);
                match callee {
                    Callee::Sym(s) => {
                        self.reloc(RelKind::Branch26, *s);
                        self.ins(enc::b(true, 0));
                    }
                    Callee::Ptr(_) => self.ins(enc::blr(R(X16))),
                }
                self.forget();
                for (p, m) in rets {
                    match p.loc {
                        Loc::Int(r) => self.store(u32::from(r), *m, p.k),
                        Loc::Fp(r) => self.store_fp(u32::from(r), *m, p.k),
                        Loc::Vec(r) => self.vec_store(u32::from(r), *m),
                        _ => {}
                    }
                }
            }
            Inst::Param { part, dst } => match part.loc {
                Loc::Int(r) => self.store(u32::from(r), *dst, part.k),
                Loc::Fp(r) => self.store_fp(u32::from(r), *dst, part.k),
                Loc::Vec(r) => self.vec_store(u32::from(r), *dst),
                Loc::Stack(o) => {
                    self.ldst(Self::load_op(part.k, false), X9, FP, 16 + i64::from(o));
                    self.store(X9, *dst, part.k);
                }
                Loc::VecStack(o) => {
                    self.ldst(enc::LDRQ, V16, FP, 16 + i64::from(o));
                    self.vec_store(V16, *dst);
                }
                Loc::Sret => self.store(8, *dst, K::I64),
                Loc::Area(o) => {
                    self.add_imm(X9, FP, 16 + i64::from(o));
                    self.store(X9, *dst, K::I64);
                }
            },
            Inst::Ret { vals } => {
                for (p, v) in vals {
                    match p.loc {
                        Loc::Int(r) => self.load(u32::from(r), *v, p.k, p.signed),
                        Loc::Fp(r) => self.load_fp(u32::from(r), *v, p.k),
                        Loc::Vec(r) => self.vec_load(u32::from(r), v.vector_mem()),
                        _ => {}
                    }
                }
                // The frame dies.
                self.pending.clear();
                self.epilogue();
                self.forget();
            }
            Inst::Trap => {
                self.pending.clear();
                self.ins(enc::brk(1));
            }
            Inst::VLoop(v) => self.vloop(v),
            Inst::V(x) => self.vx(x),
            Inst::Line(line, col, scope) => {
                let at = 4 * self.a.pos() as u32;
                self.lines.push((at, *line, *col, *scope));
            }
        }
    }

    /// Before an exclusive pair: the stores waiting on the loop's
    /// registers go out, so no other access falls between `ldaxr` and
    /// `stlxr` (one may clear the monitor every time round).
    pub(crate) fn llsc_enter(&mut self) {
        for r in [X9, X10, X11, X12, X14, ZR] {
            self.flush_reg(r);
        }
    }

    /// `x9` = `[x14]`, `[x14]` = `x9 op x10`, ordered `ord`, with LSE.
    pub(crate) fn lse_rmw(&mut self, op: Rmw, sz: u32, ord: Order) {
        let x10 = R(X10);
        let op = match op {
            Rmw::Add => Lse::Ldadd,
            Rmw::Sub => {
                self.ins(enc::add_reg(
                    AddOp::Sub,
                    true,
                    x10,
                    enc::ZR,
                    x10,
                    Shift::Lsl,
                    0,
                ));
                Lse::Ldadd
            }
            Rmw::And => {
                self.ins(enc::log_reg(Log::Orr, true, true, x10, enc::ZR, x10));
                Lse::Ldclr
            }
            Rmw::Xor => Lse::Ldeor,
            Rmw::Or => Lse::Ldset,
            Rmw::Xchg => Lse::Swp,
        };
        self.ins(enc::lse_o(op, sz, ord.acq(), ord.rel(), x10, R(X9), R(X14)));
    }

    /// Without LSE in the baseline (Linux): branch past what follows to
    /// the `ldxr` / `stxr` loop unless the runtime found LSE
    /// (`nabi_cpu_lse`, set from `AT_HWCAP` at start). The position of
    /// the branch, for `lse_join`.
    pub(crate) fn lse_test(&mut self) -> usize {
        let s = self.obj.sym("nabi_cpu_lse");
        self.reloc(RelKind::Page21, s);
        self.ins(enc::adrp(R(X12)));
        self.reloc(RelKind::PageOff12, s);
        self.ins(enc::add_imm(AddOp::Add, true, R(X12), R(X12), 0, false));
        self.ins(enc::ldst(LDRB, X12, R(X12), 0));
        let at = self.a.pos();
        self.a.push(enc::cbz(false, R(X12), 0));
        at
    }

    /// The end of the LSE way: a branch past the loop that follows.
    pub(crate) fn lse_join(&mut self, skip: usize) -> usize {
        let out = self.a.pos();
        self.a.push(enc::b(false, 0));
        self.a.patch19(skip, (self.a.pos() - skip) as i64);
        out
    }

    /// Both ways met: what either wrote is unknown.
    pub(crate) fn lse_done(&mut self, out: usize) {
        self.a.patch26(out, (self.a.pos() - out) as i64);
        for r in [X9, X10, X11, X12] {
            self.clobber(r);
        }
    }

    /// `x9` = `[x14]`, `[x14]` = `x9 op x10`, ordered `ord`, without
    /// LSE: an `ldxr` / `stxr` loop (`ldaxr` to acquire, `stlxr` to
    /// release; `x11` the new value, `w12` the status).
    pub(crate) fn llsc_rmw(&mut self, op: Rmw, sz: u32, ord: Order) {
        self.llsc_enter();
        let ld = if ord.acq() { Excl::Ldaxr } else { Excl::Ldxr };
        self.ins(enc::excl(ld, sz, R(X9), R(X14)));
        let (x9, x10, x11) = (R(X9), R(X10), R(X11));
        let w = match op {
            Rmw::Xchg => None,
            Rmw::Add => Some(enc::add_reg(AddOp::Add, true, x11, x9, x10, Shift::Lsl, 0)),
            Rmw::Sub => Some(enc::add_reg(AddOp::Sub, true, x11, x9, x10, Shift::Lsl, 0)),
            Rmw::And => Some(enc::log_reg(Log::And, false, true, x11, x9, x10)),
            Rmw::Or => Some(enc::log_reg(Log::Orr, false, true, x11, x9, x10)),
            Rmw::Xor => Some(enc::log_reg(Log::Eor, false, true, x11, x9, x10)),
        };
        let src = if let Some(w) = w {
            self.ins(w);
            X11
        } else {
            X10
        };
        // stlxr w12, src, [x14]; cbnz w12, the ldaxr
        self.ins(enc::stxr(sz, ord.rel(), R(X12), R(src), R(X14)));
        let back: i64 = if src == X10 { 2 } else { 3 };
        self.ins(enc::cbz(true, R(X12), -back));
        self.clobber(X12);
    }

    /// `x11` = `[x14]`, `[x14]` = `x10` when that was `x9`, ordered
    /// `ord`, without LSE: an `ldxr` / `stxr` loop as `llsc_rmw`'s; a
    /// failed compare drops the reservation (`clrex`).
    pub(crate) fn llsc_cas(&mut self, sz: u32, ord: Order) {
        self.llsc_enter();
        let ld = if ord.acq() { Excl::Ldaxr } else { Excl::Ldxr };
        self.ins(enc::excl(ld, sz, R(X11), R(X14)));
        // cmp w11, w9, uxtb / uxth; cmp w11, w9; cmp x11, x9: the loaded
        // value is zero-extended, the expected one may not be.
        let (x9, x11) = (R(X9), R(X11));
        let cmp = match sz {
            0 => enc::add_ext(AddOp::Subs, false, enc::ZR, x11, x9, enc::Ext::Uxtb),
            1 => enc::add_ext(AddOp::Subs, false, enc::ZR, x11, x9, enc::Ext::Uxth),
            _ => enc::cmp(sz == 3, x11, x9),
        };
        self.ins(cmp);
        // b.ne the clrex (bits 0..5 name no register: no `ins`)
        self.a.push(enc::b_cond(Cond::NE, 4));
        // stlxr w12, x10, [x14]; cbnz w12, the ldaxr
        self.ins(enc::stxr(sz, ord.rel(), R(X12), R(X10), R(X14)));
        self.ins(enc::cbz(true, R(X12), -4));
        self.clobber(X12);
        // b past the clrex
        self.a.push(enc::b(false, 2));
        self.ins(enc::clrex());
    }

    /// The address a `ptr` operand of an atomic names, in `rd`.
    pub(crate) fn addr_of_ptr(&mut self, rd: u32, m: Mem) {
        self.addr(rd, m);
    }
}
