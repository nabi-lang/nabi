//! Calls and parameters.

#[allow(clippy::wildcard_imports)]
use crate::isa::ppc64::*;

impl Em<'_> {
    /// `bl sym; nop` (the linker's TOC restore after a PLT stub).
    pub(crate) fn call_sym(&mut self, sym: u32) {
        self.reloc(RelKind::PpcCall, sym);
        self.ins(enc::bl());
        self.ins(enc::nop());
    }

    /// Integer argument register `r` = part `p` of `v`, extended to 64
    /// bits by its signedness (a float's bits when variadic).
    pub(crate) fn ld_arg(&mut self, r: u32, p: Part, v: V) {
        self.ld(r, v, int_k(p.k), p.signed && !p.k.is_float());
    }

    pub(crate) fn param(&mut self, part: Part, dst: Mem) {
        match part.loc {
            Loc::Int(r) => self.st(R3 + u32::from(r), dst, part.k),
            Loc::Fp(n) => self.stf(F1 + u32::from(n), dst, part.k),
            Loc::Vec(_) | Loc::VecStack(_) => unreachable!("no C vectors here"),
            Loc::Stack(o) => {
                // The parameter save area of the caller's frame.
                let at = i64::from(HEADER + o);
                let k = int_k(part.k);
                self.ls(true, k, part.signed, false, R11, FP, at);
                self.st(R11, dst, k);
            }
            Loc::Sret => self.st(R3, dst, K::I64),
            Loc::Area(o) => {
                self.add_imm(R11, FP, i64::from(HEADER + o));
                self.st(R11, dst, K::I64);
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
        // The stack first (doublewords, an `f32` in its first word), then
        // the float registers, then the integer ones, each loaded with
        // itself as the scratch.
        for (p, v) in args {
            let Loc::Stack(o) = p.loc else { continue };
            let at = i64::from(HEADER + o);
            if p.k == K::F32 {
                self.ld(R11, *v, K::I32, false);
                self.ls(false, K::I32, false, false, R11, SP, at);
            } else {
                self.ld_arg(R11, *p, *v);
                self.st_word(R11, SP, at);
            }
        }
        // Above -O0 in order: the allocator assumes so.
        for (p, v) in args {
            if let Loc::Fp(n) = p.loc {
                self.ldf(F1 + u32::from(n), *v, p.k);
            } else if let (Loc::Int(r), true) = (p.loc, self.opt) {
                self.ld_arg(R3 + u32::from(r), *p, *v);
            }
        }
        for (p, v) in args {
            if let (Loc::Int(r), false) = (p.loc, self.opt) {
                self.ld_arg(R3 + u32::from(r), *p, *v);
            }
        }
        if let Some(m) = sret {
            self.lea(R3, m);
        }
        match callee {
            Callee::Sym(s) => self.call_sym(*s),
            Callee::Ptr(m) => {
                // The global entry wants its address in `r12`.
                self.ld(R12, V::M(*m), K::I64, false);
                self.ins(enc::mtctr(R(R12)));
                self.st_word(TOC, SP, TOC_SAVE);
                self.ins(enc::bctrl());
                self.ld_word(TOC, SP, TOC_SAVE);
            }
        }
        for (p, m) in rets {
            match p.loc {
                Loc::Int(r) => self.st(R3 + u32::from(r), *m, p.k),
                Loc::Fp(n) => self.stf(F1 + u32::from(n), *m, p.k),
                _ => {}
            }
        }
    }
}
