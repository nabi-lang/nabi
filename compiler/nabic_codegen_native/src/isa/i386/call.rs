//! Calls.

#[allow(clippy::wildcard_imports)]
use crate::isa::i386::*;

impl Em<'_> {
    /// `call sym`.
    pub(crate) fn call_sym(&mut self, sym: u32) {
        // ELF's implicit addend; COFF's displacement counts from the end.
        let addend = if self.obj.coff { 0 } else { -4 };
        self.a.call(RelKind::I386Call, sym, addend);
    }

    /// `call` a local helper.
    pub(crate) fn helper(&mut self, name: &str) {
        let s = self.obj.sym(name);
        self.call_sym(s);
    }

    pub(crate) fn call(
        &mut self,
        callee: &Callee,
        args: &[(crate::lir::Part, V)],
        rets: &[(crate::lir::Part, Mem)],
        sret: Option<Mem>,
        pop: u32,
    ) {
        for (p, v) in args {
            let Loc::Stack(o) = p.loc else { continue };
            if let Some(a) = self.ra.get_v(*v) {
                if a.fp {
                    let k = if p.k.size() == 8 { K::F64 } else { K::F32 };
                    self.a.sse_st(movs_st(k), Self::sp(o), Xmm(a.reg));
                    continue;
                }
                if !p.signed || p.k.size() == 4 {
                    // Kept zero-extended.
                    self.a.mov_st(Sz::D, Rm::M(Self::sp(o)), Gpr(a.reg));
                    continue;
                }
            }
            if wide(p.k) {
                self.ld2(EAX, EDX, *v);
                self.a.mov_st(Sz::D, Rm::M(Self::sp(o)), Gpr(EAX));
                self.a.mov_st(Sz::D, Rm::M(Self::sp(o + 4)), Gpr(EDX));
            } else {
                self.ld(EAX, *v, p.k, p.signed);
                self.a.mov_st(Sz::D, Rm::M(Self::sp(o)), Gpr(EAX));
            }
        }
        if let Some(m) = sret {
            self.lea(EAX, m);
            self.a.mov_st(Sz::D, Rm::M(Self::sp(0)), Gpr(EAX));
        }
        match callee {
            Callee::Sym(s) => self.call_sym(*s),
            Callee::Ptr(m) => {
                self.ld(EAX, V::M(*m), K::I32, false);
                self.a.call_r(Gpr(EAX));
            }
        }
        if pop != 0 {
            // The callee popped its arguments: the frame back as it was.
            self.alu_imm(Alu::Sub, gr(ESP), pop);
        }
        // The result registers still to store: no address in them.
        let mut busy: Vec<u32> = Vec::new();
        if self.opt {
            for (p, _) in rets {
                if let Loc::Int(r) = p.loc {
                    busy.push(RETS[r as usize]);
                    if wide(p.k) {
                        busy.push(RETS[r as usize + 1]);
                    }
                }
            }
        }
        for (p, m) in rets {
            if let Loc::Int(r) = p.loc {
                let r = r as usize;
                if !self.opt {
                    if wide(p.k) {
                        self.st2(RETS[r], RETS[r + 1], *m);
                    } else {
                        let m = self.mem(*m, EDI);
                        self.st_rm(RETS[r], m, p.k);
                    }
                } else if wide(p.k) {
                    self.st2_avoid(RETS[r], RETS[r + 1], *m, &busy);
                    busy.retain(|x| *x != RETS[r] && *x != RETS[r + 1]);
                } else {
                    // The part's own width: a register keeps it
                    // zero-extended.
                    self.st_avoid(RETS[r], *m, p.k, &busy);
                    busy.retain(|x| *x != RETS[r]);
                }
            }
        }
        for (p, m) in rets {
            if p.loc == Loc::Fp(0) {
                if let Some(x) = self.dst_x(*m) {
                    self.a.x87(fstp(p.k), Self::sp(0));
                    self.a.sse(movs(p.k), Xmm(x), Xrm::M(Self::sp(0)));
                    continue;
                }
                let at = self.mem(*m, if self.opt { ECX } else { EDI });
                self.a.x87(fstp(p.k), at);
            }
        }
    }
}
