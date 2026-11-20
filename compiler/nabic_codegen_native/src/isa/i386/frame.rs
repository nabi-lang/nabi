//! Prologue, epilogue, GOT, stack probes and block copies.

#[allow(clippy::wildcard_imports)]
use crate::isa::i386::*;

impl Em<'_> {
    /// `sub esp, 4096; or dword [esp], 0`: a page down, touched.
    pub(crate) fn probe_page(&mut self) {
        self.a.alu_ri(Alu::Sub, gr(ESP), PAGE);
        self.a.alu_ri8(Alu::Or, Rm::M(Self::sp(0)), 0);
    }

    pub(crate) fn prologue(&mut self) {
        prologue_regs(&mut self.a);
        let n = self.frame;
        let pages = n / PAGE;
        if pages > 8 {
            self.mov_imm(ECX, pages);
            let top = self.a.pos();
            self.probe_page();
            self.a.dec(gr(ECX));
            self.a.jcc(Cond::NE, Some(top));
        } else {
            for _ in 0..pages {
                self.probe_page();
            }
        }
        let rest = n % PAGE;
        if rest != 0 {
            self.alu_opt(Alu::Sub, gr(ESP), rest);
            // More than a page below the caller's last touch.
            if pages == 0 && n + SAVED >= PAGE {
                self.a.alu_ri8(Alu::Or, Rm::M(Self::sp(0)), 0);
            }
        }
        if self.got {
            // call 1f; 1: pop ebx; add ebx, _GLOBAL_OFFSET_TABLE_ - 1b
            self.a.call_next();
            self.a.pop(Gpr(EBX));
            let g = self.obj.sym("_GLOBAL_OFFSET_TABLE_");
            self.a
                .alu_ri_sym(Alu::Add, Gpr(EBX), RelKind::I386GotPc, g, 3);
            if self.got_lost.is_some() {
                self.a.mov_st(Sz::D, Rm::M(bd(EBP, -16)), Gpr(EBX));
            }
        }
    }

    /// `dst` = the address of a shadow-stack frame descriptor (ELF): from
    /// the GOT found in `eax`, as the frame's link is in no place the
    /// allocator kept `ebx` for the GOT.
    pub(crate) fn desc_sym(&mut self, dst: Mem, sym: crate::lir::SymId) {
        // call 1f; 1: pop eax; add eax, _GLOBAL_OFFSET_TABLE_ - 1b
        self.a.call_next();
        self.a.pop(Gpr(EAX));
        let g = self.obj.sym("_GLOBAL_OFFSET_TABLE_");
        self.a
            .alu_ri_sym(Alu::Add, Gpr(EAX), RelKind::I386GotPc, g, 3);
        let m = Addr::BdRel(Gpr(EAX), RelKind::I386GotOff, sym);
        self.a.lea(Gpr(EAX), m);
        self.st(EAX, dst, K::I32);
    }

    /// Before `i`: the GOT back in `ebx` where `i` reads it and a value
    /// may have taken `ebx`.
    pub(crate) fn got_at(&mut self, i: &Inst) {
        let Some(lost) = self.got_lost else { return };
        if matches!(i, Inst::Label(_)) {
            // From anywhere.
            self.got_lost = Some(true);
        } else if lost && reads_got(self.obj, i) {
            self.a.mov(Sz::D, Gpr(EBX), Rm::M(bd(EBP, -16)));
            self.got_lost = Some(false);
        }
    }

    /// After `i`: whether it wrote a value into `ebx`.
    pub(crate) fn got_after(&mut self, i: &Inst) {
        if self.got_lost != Some(false) {
            return;
        }
        let mut ms = Vec::new();
        crate::regalloc::written(i, 4, &mut ms);
        let lost = ms.iter().any(|&m| {
            self.ra.get(m).is_some_and(|r| !r.fp && r.reg == EBX)
                || self.ra.pair(m).is_some_and(|(l, h)| l == EBX || h == EBX)
        });
        self.got_lost = Some(lost);
    }

    pub(crate) fn epilogue(&mut self) {
        self.a.lea(Gpr(ESP), bd(EBP, -12));
        for r in [EDI, ESI, EBX, EBP] {
            self.a.pop(Gpr(r));
        }
        if self.pop == 0 {
            self.a.ret();
        } else {
            self.a.ret_n(self.pop as u16);
        }
    }

    pub(crate) fn copy(&mut self, dst: Mem, src: Mem, size: u32, zero: bool) {
        if self.opt && size <= 128 {
            // Straight through the operands' addresses: `edx` / `ecx` the
            // pointers, `eax` the data.
            let s = if zero { None } else { Some(self.mem(src, EDX)) };
            let d = self.mem(dst, ECX);
            if zero {
                self.mov_imm(EAX, 0);
            }
            let mut off = 0i32;
            let mut rest = size;
            for (n, k) in [(4u32, K::I32), (2, K::I16), (1, K::I8)] {
                while rest >= n {
                    if let Some(s) = s {
                        self.extend(EAX, Rm::M(s.plus(off)), k, false);
                    }
                    self.st_rm(EAX, d.plus(off), k);
                    rest -= n;
                    off += n as i32;
                }
            }
            return;
        }
        if self.opt {
            // `esi edi` aside in the scratch area while the copy takes them;
            // both addresses formed before either changes.
            let keep: Vec<u32> = [ESI, EDI]
                .into_iter()
                .filter(|r| self.ra.saved.contains(r))
                .collect();
            for (n, r) in keep.iter().enumerate() {
                let at = Self::sp(8 + 4 * n as u32);
                self.a.mov_st(Sz::D, Rm::M(at), Gpr(*r));
            }
            if !zero {
                self.lea(EDX, src);
            }
            self.lea(EDI, dst);
            if !zero {
                self.mov(ESI, EDX);
            }
            self.copy_body(size, zero);
            for (n, r) in keep.iter().enumerate() {
                let at = Self::sp(8 + 4 * n as u32);
                self.a.mov(Sz::D, Gpr(*r), Rm::M(at));
            }
            return;
        }
        if !zero {
            self.lea(ESI, src);
        }
        self.lea(EDI, dst);
        self.copy_body(size, zero);
    }

    /// The copy of `copy`, `esi` / `edi` the addresses.
    pub(crate) fn copy_body(&mut self, size: u32, zero: bool) {
        if zero {
            self.mov_imm(EAX, 0);
        }
        if size > 128 {
            // A dword loop, not `rep movsb` / `rep stosb`: Rosetta runs
            // those a byte at a time.
            self.mov_imm(ECX, size / 4);
            let top = self.a.pos();
            if !zero {
                self.a.mov(Sz::D, Gpr(EAX), Rm::M(bd(ESI, 0)));
                self.a.alu_ri8(Alu::Add, gr(ESI), 4);
            }
            self.a.mov_st(Sz::D, Rm::M(bd(EDI, 0)), Gpr(EAX));
            self.a.alu_ri8(Alu::Add, gr(EDI), 4);
            self.a.dec(gr(ECX));
            self.a.jcc(Cond::NE, Some(top));
            self.copy_words(size % 4, zero);
            return;
        }
        self.copy_words(size, zero);
    }

    /// `[edi + i] = [esi + i]` (or zero) for `size` bytes.
    pub(crate) fn copy_words(&mut self, size: u32, zero: bool) {
        let mut off = 0i32;
        let mut rest = size;
        for (n, k) in [(4u32, K::I32), (2, K::I16), (1, K::I8)] {
            while rest >= n {
                if !zero {
                    self.extend(EAX, Rm::M(bd(ESI, off)), k, false);
                }
                self.st_rm(EAX, bd(EDI, off), k);
                rest -= n;
                off += n as i32;
            }
        }
    }
}
