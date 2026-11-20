//! Calls and parameters.

#[allow(clippy::wildcard_imports)]
use crate::isa::arm32::*;

impl Em<'_> {
    /// `bl sym`.
    pub(crate) fn call_sym(&mut self, sym: u32) {
        self.reloc(RelKind::ArmCall, sym);
        self.ins(enc::bl());
    }

    /// `bl` a run-time ABI helper.
    pub(crate) fn helper(&mut self, name: &str) {
        let s = self.obj.sym(name);
        self.call_sym(s);
    }

    /// `rd` = a word at a literal skipped over (relocated by `kind`
    /// against `sym`), plus the place: `ldr rd, [pc, #4]; add rd, pc,
    /// rd; b 1f; .word ...; 1:`.
    pub(crate) fn literal(&mut self, rd: u32, kind: RelKind, sym: u32) {
        self.ins(enc::ls(Acc::Ldr, R(rd), enc::PC, 4));
        self.dp(Cond::AL, Op::Add, false, rd, PC, rd);
        self.ins(enc::b(Cond::AL));
        self.pools.push(4 * self.pos() as u32);
        self.reloc(kind, sym);
        self.ins(enc::word(0));
    }

    pub(crate) fn param(&mut self, part: Part, dst: Mem) {
        match part.loc {
            Loc::Int(r) => {
                let r = u32::from(r);
                if wide(part.k) {
                    self.st2(r, r + 1, dst);
                } else {
                    self.st(r, dst, part.k);
                }
            }
            Loc::Fp(n) => self.stf(u32::from(n), dst, part.k),
            Loc::Vec(_) | Loc::VecStack(_) => unreachable!("no C vectors here"),
            Loc::Stack(o) => {
                // Above the saved registers.
                // `ip lr` above -O0: `r0`-`r3` may hold parameters still.
                let at = (SAVED + self.va + o) as i32;
                let (lo, hi) = if self.opt { (IP, LR) } else { (R4, R5) };
                if wide(part.k) {
                    self.ls(Acc::Ldr, lo, FP, at, lo);
                    self.ls(Acc::Ldr, hi, FP, at + 4, hi);
                    self.st2(lo, hi, dst);
                } else {
                    self.ls(Acc::load(part.k, part.signed), lo, FP, at, lo);
                    self.st(lo, dst, part.k);
                }
            }
            Loc::Sret => self.st(R0, dst, K::I32),
            Loc::Area(o) => {
                let (lo, _) = if self.opt { (IP, LR) } else { (R4, R5) };
                self.add_imm(lo, FP, (SAVED + o) as i32);
                self.st(lo, dst, K::I32);
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
        // The stack first, then the VFP registers (through `ip lr`), then
        // the integer ones, each loaded with itself as the scratch.
        for (p, v) in args {
            let Loc::Stack(o) = p.loc else { continue };
            if wide(p.k) {
                self.ld2(self.x, self.xh, *v);
                self.ls(Acc::Str, self.x, SP, o as i32, R8);
                self.ls(Acc::Str, self.xh, SP, o as i32 + 4, R8);
            } else {
                self.ld(self.x, *v, p.k, p.signed);
                self.ls(Acc::Str, self.x, SP, o as i32, R8);
            }
        }
        for (p, v) in args {
            if let Loc::Fp(n) = p.loc {
                self.ldf(u32::from(n), *v, p.k);
            }
        }
        for (p, v) in args {
            if let Loc::Int(r) = p.loc {
                let r = u32::from(r);
                if wide(p.k) {
                    self.ld2(r, r + 1, *v);
                } else {
                    self.ld(r, *v, p.k, p.signed);
                }
            }
        }
        if let Some(m) = sret {
            self.lea(R0, m);
        }
        match callee {
            Callee::Sym(s) => self.call_sym(*s),
            Callee::Ptr(m) => {
                self.ld(IP, V::M(*m), K::I32, false);
                self.ins(enc::blx(R(IP)));
            }
        }
        for (p, m) in rets {
            match p.loc {
                Loc::Int(r) => {
                    let r = u32::from(r);
                    if wide(p.k) {
                        self.st2(r, r + 1, *m);
                    } else {
                        self.st(r, *m, p.k);
                    }
                }
                Loc::Fp(n) => self.stf(u32::from(n), *m, p.k),
                _ => {}
            }
        }
    }
}
