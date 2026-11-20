//! Calls and parameters.

#[allow(clippy::wildcard_imports)]
use crate::isa::riscv::*;

impl Em<'_> {
    /// `call sym`: `auipc ra` / `jalr ra`.
    pub(crate) fn call_sym(&mut self, sym: u32) {
        self.reloc(RelKind::RvCall, sym);
        self.ins(enc::auipc(enc::RA, 0));
        self.ins(enc::jalr(enc::RA, enc::RA, 0));
    }

    pub(crate) fn helper(&mut self, name: &str) {
        let s = self.obj.sym(name);
        self.call_sym(s);
    }

    /// Integer argument register `r` = part `p` of `v`, extended as the
    /// psABI says (rv64: 32-bit values sign-extended). rv32: an 8-byte
    /// part in `r` and the next register, or (`r` is `a7`) its high word
    /// stored at `hi_at` (the outgoing area; `None`: a result).
    pub(crate) fn ld_arg(&mut self, r: u32, p: Part, v: V, hi_at: Option<u32>) {
        if self.pair(p.k) {
            if r == A7 {
                self.ld2(A7, T1, v);
                if let Some(o) = hi_at {
                    self.st_word(T1, SP, i64::from(o));
                }
            } else {
                self.ld2(r, r + 1, v);
            }
            return;
        }
        let k = int_k(p.k);
        let signed = (k == K::I32 && !self.rv32) || (p.signed && !p.k.is_float());
        self.ld(r, v, k, signed);
    }

    pub(crate) fn param(&mut self, part: Part, dst: Mem) {
        let k = int_k(part.k);
        match part.loc {
            Loc::Int(r) => {
                let r = A0 + u32::from(r);
                if self.pair(k) {
                    if r == A7 {
                        // The high word: at the incoming stack's start.
                        self.ld_word(T1, S0, 0);
                        self.st2(A7, T1, dst);
                    } else {
                        self.st2(r, r + 1, dst);
                    }
                } else {
                    self.st(r, dst, k);
                }
            }
            Loc::Fp(n) => self.stf(FA0 + u32::from(n), dst, part.k),
            Loc::Vec(_) | Loc::VecStack(_) => unreachable!("no C vectors here"),
            Loc::Stack(o) => {
                let at = i64::from(o + self.va);
                if self.pair(k) {
                    self.ls(Ls::Lw, T0, S0, at);
                    self.ls(Ls::Lw, T1, S0, at + 4);
                    self.st2(T0, T1, dst);
                } else {
                    let f3 = self.load_f3(k, part.signed);
                    self.ls(f3, T0, S0, at);
                    self.st(T0, dst, k);
                }
            }
            Loc::Sret => self.st(A0, dst, self.word()),
            Loc::Area(o) => {
                self.add_imm(T0, S0, i64::from(o), T6);
                self.st(T0, dst, k);
            }
        }
    }

    pub(crate) fn call(
        &mut self,
        callee: &Callee,
        args: &[(Part, V)],
        rets: &[(Part, Mem)],
        sret: Option<Mem>,
    ) {
        // The stack first (XLEN slots, narrow values extended), then the
        // float registers, then the integer ones, each loaded with itself
        // as the scratch.
        for (p, v) in args {
            let Loc::Stack(o) = p.loc else { continue };
            let o = i64::from(o);
            if self.pair(p.k) {
                self.ld2(T0, T1, *v);
                self.st_word(T0, SP, o);
                self.st_word(T1, SP, o + 4);
            } else {
                self.ld_arg(T0, *p, *v, None);
                self.st_word(T0, SP, o);
            }
        }
        // Above -O0 the registers in order: a value may sit in a later
        // argument's register.
        for (p, v) in args {
            if let Loc::Fp(n) = p.loc {
                self.ldf(FA0 + u32::from(n), *v, p.k);
            } else if let (Loc::Int(r), true) = (p.loc, self.opt) {
                self.ld_arg(A0 + u32::from(r), *p, *v, Some(0));
            }
        }
        for (p, v) in args {
            if let (Loc::Int(r), false) = (p.loc, self.opt) {
                self.ld_arg(A0 + u32::from(r), *p, *v, Some(0));
            }
        }
        if let Some(m) = sret {
            self.lea(A0, m);
        }
        match callee {
            Callee::Sym(s) => self.call_sym(*s),
            Callee::Ptr(m) => {
                self.ld(T0, V::M(*m), self.word(), false);
                self.ins(enc::jalr(enc::RA, X(T0), 0));
            }
        }
        // In order: a result may land in a later one's register.
        for (p, m) in rets {
            match p.loc {
                Loc::Int(r) => {
                    let r = A0 + u32::from(r);
                    if self.pair(p.k) {
                        self.st2(r, r + 1, *m);
                    } else {
                        self.st(r, *m, p.k);
                    }
                }
                Loc::Fp(n) => self.stf(FA0 + u32::from(n), *m, p.k),
                _ => {}
            }
        }
    }
}
