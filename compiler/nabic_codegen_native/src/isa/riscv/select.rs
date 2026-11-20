//! Instruction selection, branches, comparisons and atomics.

#[allow(clippy::wildcard_imports)]
use crate::isa::riscv::*;

impl Em<'_> {
    pub(crate) fn branch(&mut self, c: Cond, rs1: u32, rs2: u32, off: i32) {
        self.ins(enc::branch(c, X(rs1), X(rs2), off));
    }

    /// `jal zero, l`.
    pub(crate) fn j(&mut self, l: Label) {
        self.fixes.push((self.a.pos(), l, false));
        self.ins(enc::jal(X(ZERO), 0));
    }

    /// To `l` when `rs1 <c> rs2`: one branch where `relax::relax` says it
    /// reaches, else the inverted branch over a `jal`.
    pub(crate) fn br(&mut self, c: Cond, rs1: u32, rs2: u32, l: Label) {
        let at = self.a.pos();
        let short = self
            .plan
            .as_ref()
            .is_some_and(|p| p.get(self.sites.len()) == Some(&true));
        self.sites.push((at, 2, l));
        if short {
            self.fixes.push((at, l, true));
            self.branch(c, rs1, rs2, 0);
        } else {
            self.branch(c.inv(), rs1, rs2, 8);
            self.j(l);
        }
    }

    /// A short forward branch to a spot not known yet: its index.
    pub(crate) fn b_fwd(&mut self, c: Cond, rs1: u32, rs2: u32) -> usize {
        self.branch(c, rs1, rs2, 0);
        self.a.pos() - 1
    }

    /// Point the short branch at `at` here.
    pub(crate) fn here(&mut self, at: usize) {
        let d = ((self.a.pos() - at) * 4) as i32;
        self.a.patch_branch(at, d);
    }

    /// A short branch back to index `top`.
    pub(crate) fn b_back(&mut self, c: Cond, rs1: u32, rs2: u32, top: usize) {
        let d = (top as i64 - self.a.pos() as i64) * 4;
        self.branch(c, rs1, rs2, d as i32);
    }

    pub(crate) fn inst(&mut self, i: &Inst, next: Option<&Inst>) {
        match i {
            Inst::Label(l) => self.labels[*l as usize] = Some(self.a.pos()),
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
                if let Some((cc, x, y)) = self.flags.take() {
                    // The comparison just before, as the branch.
                    if matches!(next, Some(Inst::Label(n)) if n == t) {
                        self.br(cc.inv(), x, y, *f);
                    } else {
                        self.br(cc, x, y, *t);
                        if !matches!(next, Some(Inst::Label(n)) if n == f) {
                            self.j(*f);
                        }
                    }
                    return;
                }
                let r = self.reg(*c, K::I8, false, T0);
                self.br(BNE, r, ZERO, *t);
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
                let rd = self.dst_x(*dst).unwrap_or(T0);
                self.lea(rd, *src);
                self.st_ext(rd, *dst, self.word(), true);
            }
            Inst::Sym { dst, sym, got } => {
                let rd = self.dst_x(*dst).unwrap_or(T0);
                // rv32 (bare metal) is one image.
                if *got && !self.rv32 {
                    self.reloc(RelKind::RvGotHi, *sym);
                    self.ins(enc::auipc(X(rd), 0));
                    self.reloc(RelKind::RvPcLoI, *sym);
                    self.ld_word(rd, rd, 0);
                } else {
                    self.reloc(RelKind::RvPcHi, *sym);
                    self.ins(enc::auipc(X(rd), 0));
                    self.reloc(RelKind::RvPcLoI, *sym);
                    self.addi(rd, rd, 0);
                }
                self.st_ext(rd, *dst, self.word(), true);
            }
            Inst::Tls { dst, sym } => {
                let rd = self.dst_x(*dst).unwrap_or(T0);
                if self.rv32 {
                    // Local exec: the executable's own block.
                    self.reloc(RelKind::RvTpHi, *sym);
                    self.lui(rd, 0);
                    self.reloc(RelKind::RvTpAdd, *sym);
                    self.alu(ADD, rd, rd, TP);
                    self.reloc(RelKind::RvTpLo, *sym);
                    self.addi(rd, rd, 0);
                } else {
                    // Initial exec: the offset from the thread pointer in
                    // a GOT slot.
                    self.reloc(RelKind::RvTlsIeHi, *sym);
                    self.ins(enc::auipc(X(rd), 0));
                    self.reloc(RelKind::RvPcLoI, *sym);
                    self.ld_word(rd, rd, 0);
                    self.alu(ADD, rd, rd, TP);
                }
                self.st_ext(rd, *dst, self.word(), true);
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
                if self.pair(*k) {
                    self.lea(A0, *ptr);
                    self.li(A1, SEQ_CST);
                    self.helper("__atomic_load_8");
                    self.st2(A0, A1, *dst);
                    return;
                }
                self.lea(T4, *ptr);
                if *ord == Order::SeqCst {
                    self.ins(enc::fence(enc::R | enc::W, enc::R | enc::W));
                }
                let f3 = self.load_f3(*k, false);
                self.ls(f3, T0, T4, 0);
                if ord.acq() {
                    self.ins(enc::fence(enc::R, enc::R | enc::W));
                }
                self.st(T0, *dst, *k);
            }
            Inst::AtomicStore { k, ptr, src, ord } => {
                if self.pair(*k) {
                    // `(ptr, value, order)`: the value in `a1 a2`.
                    self.ld2(A1, A2, *src);
                    self.lea(A0, *ptr);
                    self.li(A3, SEQ_CST);
                    self.helper("__atomic_store_8");
                    return;
                }
                self.ld(T2, *src, *k, false);
                self.lea(T4, *ptr);
                if ord.rel() {
                    self.ins(enc::fence(enc::R | enc::W, enc::W));
                }
                self.ls(Self::store_f3(*k), T2, T4, 0);
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
                // Above -O0 in order: a value may sit in a later result's
                // register.
                for (p, v) in vals {
                    if let Loc::Int(r) = p.loc {
                        self.ld_arg(A0 + u32::from(r), *p, *v, None);
                    } else if let (Loc::Fp(n), true) = (p.loc, self.opt) {
                        self.ldf(FA0 + u32::from(n), *v, p.k);
                    }
                }
                for (p, v) in vals {
                    if let (Loc::Fp(n), false) = (p.loc, self.opt) {
                        self.ldf(FA0 + u32::from(n), *v, p.k);
                    }
                }
                self.epilogue();
            }
            Inst::VLoop(v) => self.vloop(v),
            Inst::V(_) => unreachable!("no straight-line vector code here"),
            Inst::Trap => self.ins(enc::unimp()),
            Inst::Line(line, col, scope) => {
                let at = 4 * self.a.pos() as u32;
                self.lines.push((at, *line, *col, *scope));
            }
        }
    }

    pub(crate) fn mov_val(&mut self, k: K, dst: Mem, src: V) {
        if self.pair(k) {
            if let Some((l, h)) = self.ra.pair(dst) {
                self.ld2(l, h, src);
            } else {
                let (l, h) = self.src2(T0, T1, src);
                self.st2(l, h, dst);
            }
        } else if let Some(a) = self.ra.get(dst) {
            if a.fp {
                let fk = if k.size() == 8 { K::F64 } else { K::F32 };
                self.ldf(a.reg, src, fk);
            } else {
                // Loaded zero-extended.
                self.ld(a.reg, src, k, false);
            }
        } else {
            self.ld(T0, src, k, false);
            self.st(T0, dst, k);
        }
    }

    /// `out` = `x < y` for the 8-byte pairs `x`, `y` (rv32; `t` scratch).
    pub(crate) fn lt64(
        &mut self,
        signed: bool,
        x: (u32, u32),
        y: (u32, u32),
        out: u32,
        t: (u32, u32),
    ) {
        self.alu(if signed { SLT } else { SLTU }, out, x.1, y.1);
        self.alu(XOR, t.0, x.1, y.1);
        self.seqz(t.0, t.0);
        self.alu(SLTU, t.1, x.0, y.0);
        self.alu(AND, t.0, t.0, t.1);
        self.alu(OR, out, out, t.0);
    }

    /// Whether the branch `next` reads `dst`, the comparison's result,
    /// alone: it then branches on the comparison itself.
    pub(crate) fn fusable(&self, dst: Mem, next: Option<&Inst>) -> bool {
        let Mem::Frame(s, o) = dst else { return false };
        matches!(next, Some(Inst::Br { c: V::M(m), .. }) if *m == dst)
            && self.waits.get(&(s, o)) == Some(&(1, 1))
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
            self.fcmp(cc, k, dst, a, b);
            return;
        }
        if self.pair(k) {
            let (x, y) = (self.src2(T0, T1, a), self.src2(T2, T3, b));
            match cc {
                Cc::Eq | Cc::Ne => {
                    self.alu(XOR, T0, x.0, y.0);
                    self.alu(XOR, T1, x.1, y.1);
                    self.alu(OR, T0, T0, T1);
                    if cc == Cc::Eq {
                        self.seqz(T0, T0);
                    } else {
                        self.snez(T0, T0);
                    }
                }
                Cc::Lt | Cc::Ge => {
                    self.lt64(signed, x, y, A0, (A1, A2));
                    self.mv(T0, A0);
                    if cc == Cc::Ge {
                        self.xori(T0, T0, 1);
                    }
                }
                _ => {
                    self.lt64(signed, y, x, A0, (A1, A2));
                    self.mv(T0, A0);
                    if cc == Cc::Le {
                        self.xori(T0, T0, 1);
                    }
                }
            }
            self.st(T0, dst, K::I8);
            return;
        }
        let x = self.reg(a, k, signed, T0);
        let y = self.reg(b, k, signed, T2);
        if self.fusable(dst, next) {
            let (lt, ge) = if signed { (BLT, BGE) } else { (BLTU, BGEU) };
            self.flags = Some(match cc {
                Cc::Eq => (BEQ, x, y),
                Cc::Ne => (BNE, x, y),
                Cc::Lt => (lt, x, y),
                Cc::Gt => (lt, y, x),
                Cc::Le => (ge, y, x),
                Cc::Ge => (ge, x, y),
            });
            if let Mem::Frame(s, o) = dst {
                self.skipped.insert((s, o), (1, 1));
            }
            return;
        }
        let rd = self.dst_x(dst).unwrap_or(T0);
        let lt = if signed { SLT } else { SLTU };
        match cc {
            Cc::Eq => {
                self.alu(XOR, T0, x, y);
                self.seqz(rd, T0);
            }
            Cc::Ne => {
                self.alu(XOR, T0, x, y);
                self.snez(rd, T0);
            }
            Cc::Lt => self.alu(lt, rd, x, y),
            Cc::Gt => self.alu(lt, rd, y, x),
            Cc::Le => {
                self.alu(lt, rd, y, x);
                self.xori(rd, rd, 1);
            }
            Cc::Ge => {
                self.alu(lt, rd, x, y);
                self.xori(rd, rd, 1);
            }
        }
        self.st_ext(rd, dst, K::I8, true);
    }

    pub(crate) fn fcmp(&mut self, cc: Cc, k: K, dst: Mem, a: V, b: V) {
        if self.rv32 {
            let d = k == K::F64;
            let name = match (cc, d) {
                (Cc::Eq, false) => "__eqsf2",
                (Cc::Ne, false) => "__nesf2",
                (Cc::Lt, false) => "__ltsf2",
                (Cc::Le, false) => "__lesf2",
                (Cc::Gt, false) => "__gtsf2",
                (Cc::Ge, false) => "__gesf2",
                (Cc::Eq, true) => "__eqdf2",
                (Cc::Ne, true) => "__nedf2",
                (Cc::Lt, true) => "__ltdf2",
                (Cc::Le, true) => "__ledf2",
                (Cc::Gt, true) => "__gtdf2",
                (Cc::Ge, true) => "__gedf2",
            };
            self.soft2(name, k, a, b);
            // The helper's sign (unordered: the false one).
            match cc {
                Cc::Eq => self.seqz(A0, A0),
                Cc::Ne => self.snez(A0, A0),
                Cc::Lt => self.alu(SLT, A0, A0, ZERO),
                Cc::Gt => self.alu(SLT, A0, ZERO, A0),
                Cc::Le => {
                    self.alu(SLT, A0, ZERO, A0);
                    self.xori(A0, A0, 1);
                }
                Cc::Ge => {
                    self.alu(SLT, A0, A0, ZERO);
                    self.xori(A0, A0, 1);
                }
            }
            self.st(A0, dst, K::I8);
            return;
        }
        let fa = self.freg(a, k, F0);
        let fb = self.freg(b, k, F1);
        // Operands swapped for > / >=.
        let (c, x, y) = match cc {
            Cc::Eq | Cc::Ne => (FCmp::Eq, fa, fb),
            Cc::Lt => (FCmp::Lt, fa, fb),
            Cc::Le => (FCmp::Le, fa, fb),
            Cc::Gt => (FCmp::Lt, fb, fa),
            Cc::Ge => (FCmp::Le, fb, fa),
        };
        let rd = self.dst_x(dst).unwrap_or(T0);
        self.ins(enc::fcmp(c, k == K::F64, X(rd), enc::F(x), enc::F(y)));
        if cc == Cc::Ne {
            self.xori(rd, rd, 1);
        }
        self.st_ext(rd, dst, K::I8, true);
    }

    /// `amo` / `lr` / `sc` `o`, 8 bytes wide (`d`) or 4, ordered `ord`:
    /// an `amo` takes `aq` / `rl` as it asks, an `lr` only `aq` and an
    /// `sc` only `rl`; `seq_cst` sets both on each.
    pub(crate) fn amo(&mut self, o: Amo, d: bool, rd: u32, addr: u32, rs2: u32, ord: Order) {
        let sc = ord == Order::SeqCst;
        let (aq, rl) = match o {
            Amo::Lr => (ord.acq(), sc),
            Amo::Sc => (sc, ord.rel()),
            _ => (ord.acq(), ord.rel()),
        };
        self.ins(enc::amo_o(o, d, aq, rl, X(rd), X(addr), X(rs2)));
    }

    pub(crate) fn rmw(&mut self, op: Rmw, k: K, dst: Mem, ptr: Mem, val: V, ord: Order) {
        if self.pair(k) {
            self.ld2(A1, A2, val);
            self.lea(A0, ptr);
            self.li(A3, SEQ_CST);
            self.helper(match op {
                Rmw::Xchg => "__atomic_exchange_8",
                Rmw::Add => "__atomic_fetch_add_8",
                Rmw::Sub => "__atomic_fetch_sub_8",
                Rmw::And => "__atomic_fetch_and_8",
                Rmw::Or => "__atomic_fetch_or_8",
                Rmw::Xor => "__atomic_fetch_xor_8",
            });
            self.st2(A0, A1, dst);
            return;
        }
        self.ld(T2, val, k, false);
        self.lea(T4, ptr);
        if k.size() >= 4 {
            let w = k.size() == 8;
            let o = match op {
                Rmw::Xchg => Amo::Swap,
                Rmw::Add | Rmw::Sub => Amo::Add,
                Rmw::And => Amo::And,
                Rmw::Or => Amo::Or,
                Rmw::Xor => Amo::Xor,
            };
            if op == Rmw::Sub {
                self.alu(SUB, T2, ZERO, T2);
            }
            self.amo(o, w, T0, T4, T2, ord);
            self.st(T0, dst, k);
            return;
        }
        // A byte / halfword: an `lr.w` / `sc.w` loop on its aligned word,
        // `t3` the shift, `a0` the mask.
        self.masked(k);
        let top = self.a.pos();
        self.amo(Amo::Lr, false, T0, T4, ZERO, ord);
        self.alu(SRL, A1, T0, T3);
        match op {
            Rmw::Xchg => self.mv(A1, T2),
            Rmw::Add => self.alu(ADD, A1, A1, T2),
            Rmw::Sub => self.alu(SUB, A1, A1, T2),
            Rmw::And => self.alu(AND, A1, A1, T2),
            Rmw::Or => self.alu(OR, A1, A1, T2),
            Rmw::Xor => self.alu(XOR, A1, A1, T2),
        }
        self.merge();
        self.amo(Amo::Sc, false, A3, T4, A2, ord);
        self.b_back(BNE, A3, ZERO, top);
        self.alu(SRL, T0, T0, T3);
        self.st(T0, dst, k);
    }

    /// Byte / halfword atomics on the word at `t4 & -4`: `t4` that word,
    /// `t3` the bit shift, `a0` the field's mask.
    pub(crate) fn masked(&mut self, k: K) {
        self.andi(T3, T4, 3);
        self.slli(T3, T3, 3);
        self.andi(T4, T4, -4);
        self.li(A0, (1i64 << k.bits()) - 1);
        self.alu(SLL, A0, A0, T3);
    }

    /// `a2` = word `t0` with its field replaced by `a1` (`masked`).
    pub(crate) fn merge(&mut self) {
        self.alu(SLL, A1, A1, T3);
        self.alu(AND, A1, A1, A0);
        self.xori(A2, A0, -1);
        self.alu(AND, A2, A2, T0);
        self.alu(OR, A2, A2, A1);
    }

    pub(crate) fn cas(&mut self, k: K, dst: Mem, ptr: Mem, expected: V, new: V, ord: Order) {
        if self.pair(k) {
            // `__atomic_compare_exchange_8(ptr, &expected, new, ..)`: the
            // old value comes back in `expected`, the scratch area.
            self.ld2(T0, T1, expected);
            self.st_word(T0, SP, 0);
            self.st_word(T1, SP, 4);
            self.ld2(A2, A3, new);
            self.lea(A0, ptr);
            self.mv(A1, SP);
            self.li(A4, SEQ_CST);
            self.li(A5, SEQ_CST);
            self.helper("__atomic_compare_exchange_8");
            self.ld_word(T0, SP, 0);
            self.ld_word(T1, SP, 4);
            self.st2(T0, T1, dst);
            return;
        }
        if k.size() >= 4 {
            // `lr.w` sign-extends: so is `expected`.
            let w = k.size() == 8;
            self.ld(T2, expected, k, true);
            self.ld(T1, new, k, false);
            self.lea(T4, ptr);
            let top = self.a.pos();
            self.amo(Amo::Lr, w, T0, T4, ZERO, ord);
            let out = self.b_fwd(BNE, T0, T2);
            self.amo(Amo::Sc, w, A3, T4, T1, ord);
            self.b_back(BNE, A3, ZERO, top);
            self.here(out);
            self.st(T0, dst, k);
            return;
        }
        self.ld(T2, expected, k, false);
        self.ld(A4, new, k, false);
        self.lea(T4, ptr);
        self.masked(k);
        self.li(A5, (1i64 << k.bits()) - 1);
        let top = self.a.pos();
        self.amo(Amo::Lr, false, T0, T4, ZERO, ord);
        self.alu(SRL, A1, T0, T3);
        self.alu(AND, A1, A1, A5);
        let out = self.b_fwd(BNE, A1, T2);
        self.mv(A1, A4);
        self.merge();
        self.amo(Amo::Sc, false, A3, T4, A2, ord);
        self.b_back(BNE, A3, ZERO, top);
        self.here(out);
        self.alu(SRL, T0, T0, T3);
        self.st(T0, dst, k);
    }
}
