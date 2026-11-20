//! Calls and parameters.

#[allow(clippy::wildcard_imports)]
use crate::isa::s390x::*;

impl Em<'_> {
    pub(crate) fn param(&mut self, part: Part, dst: Mem) {
        let k = int_k(part.k);
        match part.loc {
            Loc::Int(r) => self.st(R2 + r, dst, k),
            Loc::Fp(n) => self.stf(2 * n, dst, part.k),
            Loc::Vec(_) | Loc::VecStack(_) => unreachable!("no C vectors here"),
            Loc::Stack(o) => {
                // An 8-byte slot, a narrow value right-justified; above -O0
                // through `r14`: `r4 r5` may hold parameters still.
                let at = i64::from(SAVE + o);
                let r = if self.opt { R14 } else { X };
                if part.k == K::F32 {
                    self.rxy_at(Rxy::Llgf, r, R11, at + 4);
                } else {
                    self.rxy_at(Rxy::Lg, r, R11, at);
                }
                self.st(r, dst, k);
            }
            Loc::Sret => self.st(R2, dst, K::I64),
            Loc::Area(o) => {
                let r = if self.opt { R14 } else { X };
                self.rxy_at(Rxy::Lay, r, R11, i64::from(SAVE + o));
                self.st(r, dst, K::I64);
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
        // The stack first (integers extended to 8 bytes, an `F32` in the
        // slot's low word), then the float registers, then the integer
        // ones, each loaded with itself as the scratch.
        for (p, v) in args {
            let Loc::Stack(o) = p.loc else { continue };
            let at = i64::from(SAVE + o);
            if p.k == K::F32 {
                self.ld(self.x, *v, K::I32, false);
                self.rxy_at(Rxy::Sty, self.x, R15, at + 4);
            } else {
                self.ld(self.x, *v, p.k, p.signed && !p.k.is_float());
                self.rxy_at(Rxy::Stg, self.x, R15, at);
            }
        }
        for (p, v) in args {
            if let Loc::Fp(n) = p.loc {
                self.ldf(2 * n, *v, p.k);
            }
        }
        // Above -O0 the code pointer first: an argument register may
        // hold it (or its base).
        let early = self.opt && matches!(callee, Callee::Ptr(_));
        if let (true, Callee::Ptr(m)) = (early, callee) {
            self.ld(R14, V::M(*m), K::I64, false);
        }
        for (p, v) in args {
            if let Loc::Int(r) = p.loc {
                self.ld(R2 + r, *v, p.k, p.signed && !p.k.is_float());
            }
        }
        if let Some(m) = sret {
            self.lea(R2, m);
        }
        match callee {
            Callee::Sym(s) => self.call_sym(*s),
            Callee::Ptr(m) => {
                if !early {
                    self.ld(R14, V::M(*m), K::I64, false);
                }
                self.lgr(R1, R14);
                self.a.rr(Rr::Basr, R14, R1);
            }
        }
        for (p, m) in rets {
            match p.loc {
                Loc::Int(r) => self.st(R2 + r, *m, p.k),
                Loc::Fp(n) => self.stf(2 * n, *m, p.k),
                _ => {}
            }
        }
    }
}
