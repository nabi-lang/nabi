//! Instruction selection, branches, comparisons and atomics.

#[allow(clippy::wildcard_imports)]
use crate::isa::s390x::*;

impl Em<'_> {
    /// `brc mask, l` (`short`), else `brcl`.
    pub(crate) fn jcc(&mut self, mask: u8, l: Label) {
        let at = if self.short {
            self.a.brc_fwd(mask)
        } else {
            self.a.brcl(mask)
        };
        self.fixes.push((at, l));
    }

    pub(crate) fn j(&mut self, l: Label) {
        self.jcc(ALWAYS, l);
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
                if let Some(mask) = self.flags.take() {
                    // The comparison just before: its condition code.
                    if matches!(next, Some(Inst::Label(n)) if n == t) {
                        self.jcc(mask ^ ALWAYS, *f);
                    } else {
                        self.jcc(mask, *t);
                        if !matches!(next, Some(Inst::Label(n)) if n == f) {
                            self.j(*f);
                        }
                    }
                    return;
                }
                let r = self.reg(*c, K::I8, false, self.x);
                self.a.rre(Rre::Ltgr, self.x, r);
                self.jcc(NE, *t);
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
                let r = self.dst_x(*dst).unwrap_or(self.x);
                self.lea(r, *src);
                self.st(r, *dst, K::I64);
            }
            Inst::Sym { dst, sym, got } => {
                let r = self.dst_x(*dst).unwrap_or(self.x);
                if *got {
                    self.a.pcrel(Pcrel::Lgrl, r, RelKind::S390Got, *sym);
                } else {
                    self.a.pcrel(Pcrel::Larl, r, RelKind::S390Pc, *sym);
                }
                self.st(r, *dst, K::I64);
            }
            Inst::Tls { dst, sym } => {
                // Initial exec: the offset from the thread pointer in a GOT
                // slot; the thread pointer in `a0:a1`.
                self.a.pcrel(Pcrel::Lgrl, self.x, RelKind::S390TlsIe, *sym);
                self.a.rre(Rre::Ear, R0, 0);
                self.a.rsy(Rsy::Sllg, R0, R0, 0, 32);
                self.a.rre(Rre::Ear, R0, 1);
                self.a.rre(Rre::Agr, self.x, R0);
                self.st(self.x, *dst, K::I64);
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
            Inst::AtomicLoad { k, dst, ptr, .. } => {
                // Loads are ordered as they are.
                self.lea(P, *ptr);
                self.a.rxy(load_op(*k, false), X, 0, P, 0);
                self.st(X, *dst, *k);
            }
            Inst::AtomicStore { k, ptr, src, ord } => {
                self.ld(Y, *src, *k, false);
                self.lea(P, *ptr);
                self.a.rxy(store_op(*k), Y, 0, P, 0);
                // Serialize: no later load passes a seq_cst store.
                if *ord == Order::SeqCst {
                    self.a.br(R0);
                }
            }
            Inst::AtomicRmw {
                op,
                k,
                dst,
                ptr,
                val,
                ..
            } => self.rmw(*op, *k, *dst, *ptr, *val),
            Inst::Cas {
                k,
                dst,
                ptr,
                expected,
                new,
                ..
            } => self.cas(*k, *dst, *ptr, *expected, *new),
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
                        self.ld(R2 + r, *v, p.k, p.signed && !p.k.is_float());
                    }
                }
                for (p, v) in vals {
                    if let Loc::Fp(n) = p.loc {
                        self.ldf(2 * n, *v, p.k);
                    }
                }
                self.epilogue();
            }
            // An illegal instruction.
            Inst::VLoop(v) => self.vloop(v),
            Inst::V(_) => unreachable!("no straight-line vector code here"),
            Inst::Trap => self.a.trap(),
            Inst::Line(line, col, scope) => {
                let at = self.a.pos() as u32;
                self.lines.push((at, *line, *col, *scope));
            }
        }
    }

    pub(crate) fn mov_val(&mut self, k: K, dst: Mem, src: V) {
        let k = int_k(k);
        // A load zero-extends: straight into an integer destination.
        if let Some(d) = self.dst_x(dst) {
            return self.ld(d, src, k, false);
        }
        let fk = if k.size() == 4 { K::F32 } else { K::F64 };
        if let Some(d) = self.dst_f(dst) {
            return self.ldf(d, src, fk);
        }
        if let Some(f) = self.dst_f_v(src) {
            return self.stf(f, dst, fk);
        }
        // An allocated value is stored as it is.
        let r = self
            .ra
            .get_v(src)
            .filter(|a| !a.fp)
            .map_or(self.x, |a| a.reg as u8);
        self.ld(r, src, k, false);
        self.st(r, dst, k);
    }

    /// `r` = 1 when the condition code is in `mask`, else 0.
    pub(crate) fn set_cc(&mut self, r: u8, mask: u8) {
        self.li(r, 1);
        let at = self.a.brc_fwd(mask);
        self.li(r, 0);
        self.a.here(at);
    }

    /// Whether the branch `next` takes condition-code mask `mask` in place
    /// of `dst`, the comparison's result, read by it alone: nothing more
    /// to do then.
    pub(crate) fn fuse(&mut self, mask: u8, dst: Mem, next: Option<&Inst>) -> bool {
        let Mem::Frame(s, o) = dst else { return false };
        if !matches!(next, Some(Inst::Br { c: V::M(m), .. }) if *m == dst)
            || self.waits.get(&(s, o)) != Some(&(1, 1))
        {
            return false;
        }
        self.flags = Some(mask);
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
        if self.opt {
            if k.is_float() {
                let fa = self.freg(a, k, F1);
                let fb = self.freg(b, k, F3);
                self.a
                    .rre(if k == K::F64 { Rre::Cdbr } else { Rre::Cebr }, fa, fb);
            } else {
                let x = self.reg(a, k, signed, self.x);
                match b {
                    V::I(i) if signed && fits32(ext(i, k, true)) => {
                        self.a.ril(Ril::Cgfi, x, ext(i, k, true) as u32);
                    }
                    V::I(i) if !signed && u32::try_from(ext(i, k, false)).is_ok() => {
                        self.a.ril(Ril::Clgfi, x, ext(i, k, false) as u32);
                    }
                    _ => {
                        let y = self.reg(b, k, signed, self.y);
                        self.a.rre(if signed { Rre::Cgr } else { Rre::Clgr }, x, y);
                    }
                }
            }
            let mask = cc_mask(cc);
            if self.fuse(mask, dst, next) {
                return;
            }
            match self.dst_x(dst) {
                // 0 or 1: already zero-extended.
                Some(d) => self.set_cc(d, mask),
                None => {
                    self.set_cc(self.x, mask);
                    self.st(self.x, dst, K::I8);
                }
            }
            return;
        }
        if k.is_float() {
            self.ldf(F1, a, k);
            self.ldf(F3, b, k);
            self.a
                .rre(if k == K::F64 { Rre::Cdbr } else { Rre::Cebr }, F1, F3);
        } else {
            self.ld(self.x, a, k, signed);
            self.ld(self.y, b, k, signed);
            self.a
                .rre(if signed { Rre::Cgr } else { Rre::Clgr }, self.x, self.y);
        }
        self.set_cc(self.x, cc_mask(cc));
        self.st(self.x, dst, K::I8);
    }

    /// `r` = `r op r8` (`r` holds the old value).
    pub(crate) fn rmw_op(&mut self, op: Rmw, r: u8) {
        match op {
            Rmw::Xchg => self.lgr(r, Y),
            Rmw::Add => self.a.rre(Rre::Agr, r, Y),
            Rmw::Sub => self.a.rre(Rre::Sgr, r, Y),
            Rmw::And => self.a.rre(Rre::Ngr, r, Y),
            Rmw::Or => self.a.rre(Rre::Ogr, r, Y),
            Rmw::Xor => self.a.rre(Rre::Xgr, r, Y),
        }
    }

    /// `cs` / `csg` of `r1` (the expected value; on a mismatch, the
    /// current one) and `r3` at `[r9]`.
    pub(crate) fn cs(&mut self, k: K, r1: u8, r3: u8) {
        if k.size() == 8 {
            self.a.rsy(Rsy::Csg, r1, r3, P, 0);
        } else {
            self.a.cs(r1, r3, P, 0);
        }
    }

    pub(crate) fn rmw(&mut self, op: Rmw, k: K, dst: Mem, ptr: Mem, val: V) {
        self.ld(Y, val, k, false);
        self.lea(P, ptr);
        if k.size() >= 4 {
            self.a.rxy(load_op(k, false), X, 0, P, 0);
            let top = self.a.pos();
            self.lgr(R5, X);
            self.rmw_op(op, R5);
            self.cs(k, X, R5);
            self.a.brc_back(CC1, top);
            self.st(X, dst, k);
            return;
        }
        // A byte / halfword: a `cs` loop on its aligned word.
        self.masked(k);
        self.a.rxy(Rxy::Llgf, X, 0, P, 0);
        let top = self.a.pos();
        self.a.rsy(Rsy::Srlg, R4, X, R2, 0);
        self.rmw_op(op, R4);
        self.a.rsy(Rsy::Sllg, R4, R4, R2, 0);
        self.a.rre(Rre::Ngr, R4, R3);
        self.others();
        self.lgr(R5, R0);
        self.a.rre(Rre::Ogr, R5, R4);
        self.cs(K::I32, X, R5);
        self.a.brc_back(CC1, top);
        self.a.rsy(Rsy::Srlg, X, X, R2, 0);
        self.st(X, dst, k);
    }

    /// Byte / halfword atomics on the word at `r9 & -4`: `r9` that word,
    /// `r2` the field's bit shift (big endian: from the word's low end,
    /// `(4 - size - (r9 & 3)) * 8`), `r3` its mask.
    pub(crate) fn masked(&mut self, k: K) {
        self.li(R2, i64::from(4 - k.size()));
        self.li(R0, 3);
        self.a.rre(Rre::Ngr, R0, P);
        self.a.rre(Rre::Sgr, R2, R0);
        self.a.rsy(Rsy::Sllg, R2, R2, 0, 3);
        self.li(R0, -4);
        self.a.rre(Rre::Ngr, P, R0);
        self.li(R3, (1i64 << k.bits()) - 1);
        self.a.rsy(Rsy::Sllg, R3, R3, R2, 0);
    }

    /// `r0` = word `r7` without the field (`masked`).
    pub(crate) fn others(&mut self) {
        self.li(R0, -1);
        self.a.rre(Rre::Xgr, R0, R3);
        self.a.rre(Rre::Ngr, R0, X);
    }

    pub(crate) fn cas(&mut self, k: K, dst: Mem, ptr: Mem, expected: V, new: V) {
        if k.size() >= 4 {
            self.ld(X, expected, k, false);
            self.ld(R5, new, k, false);
            self.lea(P, ptr);
            self.cs(k, X, R5);
            self.st(X, dst, k);
            return;
        }
        // The field compared, the word swapped: again while only the rest
        // of the word changed. Above -O0 `r14 r1 r10` stand for `r6 r13
        // r12`, which hold values.
        let (nw, mask, word) = if self.opt {
            (R14, R1, SB)
        } else {
            (R6, R13, R12)
        };
        self.ld(Y, expected, k, false);
        self.ld(nw, new, k, false);
        self.lea(P, ptr);
        self.masked(k);
        self.li(mask, (1i64 << k.bits()) - 1);
        self.a.rxy(Rxy::Llgf, X, 0, P, 0);
        let top = self.a.pos();
        self.a.rsy(Rsy::Srlg, R4, X, R2, 0);
        self.a.rre(Rre::Ngr, R4, mask);
        self.a.rre(Rre::Cgr, R4, Y);
        let out = self.a.brc_fwd(NE);
        self.others();
        self.a.rsy(Rsy::Sllg, word, Y, R2, 0);
        self.a.rre(Rre::Ogr, word, R0);
        self.a.rsy(Rsy::Sllg, R5, nw, R2, 0);
        self.a.rre(Rre::Ogr, R5, R0);
        self.cs(K::I32, word, R5);
        let ok = self.a.brc_fwd(CC0);
        self.a.rre(Rre::Llgfr, X, word);
        self.a.brc_back(ALWAYS, top);
        self.a.here(out);
        self.a.here(ok);
        self.st(R4, dst, k);
    }
}
