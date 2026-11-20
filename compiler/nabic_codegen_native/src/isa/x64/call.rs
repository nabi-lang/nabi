//! Calls.

#[allow(clippy::wildcard_imports)]
use crate::isa::x64::*;

impl Em<'_> {
    pub(crate) fn call(
        &mut self,
        callee: &Callee,
        args: &[(crate::lir::Part, V)],
        rets: &[(crate::lir::Part, Mem)],
        sret: Option<Mem>,
        variadic: bool,
    ) {
        // The stack arguments first: register ones are loaded last.
        for (p, v) in args {
            if let Loc::Stack(o) = p.loc {
                let r = self.reg(*v, p.k, p.signed, &[]);
                self.a.mov_st(Sz::Q, Rm::m(RSP, o as i32), Gpr(r));
            }
            if let Loc::VecStack(o) = p.loc {
                self.vec_load(XMM0, v.vector_mem());
                self.vec_put(XMM0, Addr::Bd(Gpr(RSP), o as i32));
            }
        }
        let mut fps = 0;
        for (n, (p, v)) in args.iter().enumerate() {
            match p.loc {
                Loc::Int(r) => {
                    let r = self.arg_reg(r);
                    self.keep(r, &args[n..]);
                    self.load(r, *v, p.k, p.signed);
                }
                Loc::Fp(r) => {
                    self.load_fp(u32::from(r), *v, p.k);
                    fps = fps.max(r + 1);
                    // Windows: a variadic callee reads a float from the
                    // integer register of its position.
                    if variadic && self.obj.coff {
                        let w = p.k == K::F64;
                        let g = self.arg_reg(r);
                        self.w(g, false);
                        self.a.movq_gx(Sz::dq(w), Gpr(g), Xmm(u32::from(r)));
                    }
                }
                Loc::Vec(r) => {
                    self.vec_load(u32::from(r), v.vector_mem());
                    fps = fps.max(r + 1);
                }
                _ => {}
            }
        }
        if let Some(m) = sret {
            self.lea(self.sret_reg(), m);
        }
        if variadic && !self.obj.coff {
            self.mov_imm(RAX, u64::from(fps));
        }
        if let Callee::Ptr(m) = callee {
            self.load(R11, V::M(*m), K::I64, false);
        }
        self.flush(None);
        match callee {
            Callee::Sym(s) => self.a.call(RelKind::X86Call, *s),
            Callee::Ptr(_) => self.a.call_r(Gpr(R11)),
        }
        self.held.clear();
        // x87 results (`st0`, then `st1`), popped in order.
        let x87: Vec<_> = [2, 3]
            .into_iter()
            .filter_map(|n| rets.iter().find(|(p, _)| p.loc == Loc::Fp(n)))
            .collect();
        for (p, m) in rets {
            match p.loc {
                Loc::Int(r) => self.store(RETS[r as usize], *m, p.k),
                Loc::Fp(r) if r < 2 => self.store_fp(u32::from(r), *m, p.k),
                Loc::Vec(r) => self.vec_store(u32::from(r), *m),
                _ => {}
            }
        }
        for (p, m) in x87 {
            let a = addr(self.mem(*m, R11));
            self.wrote(*m, p.k.size());
            self.a.fstp(sz(p.k), a);
        }
    }

    /// Before loading `args[0]` into `r`, move what `r` holds for a later
    /// argument to a spare (`rax`, `r11`) instead of memory.
    pub(crate) fn keep(&mut self, r: u32, args: &[(crate::lir::Part, V)]) {
        let of = |v: &V, h: &Held| {
            !h.fp
                && h.reg == r
                && matches!(v, V::M(Mem::Frame(s, o)) if h.slot == *s && h.off == *o)
        };
        let needed = |v: &V| self.held.iter().any(|h| of(v, h));
        // Already in place, or nothing later reads it.
        if needed(&args[0].1) || !args[1..].iter().any(|(_, v)| needed(v)) {
            return;
        }
        let to = self.pick(&[RAX, R11], false, &[r]);
        self.w(to, false);
        self.a.mov(Sz::Q, Gpr(to), Rm::r(r));
        for h in self.held.iter_mut().filter(|h| h.reg == r && !h.fp) {
            h.reg = to;
        }
        for p in self.pending.iter_mut().filter(|p| p.reg == r && !p.fp) {
            p.reg = to;
        }
    }
}
