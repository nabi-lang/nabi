//! Prologue, epilogue, stack allocation and block copies.

#[allow(clippy::wildcard_imports)]
use crate::isa::x64::*;

impl Em<'_> {
    pub(crate) fn prologue(&mut self) {
        if self.obj.coff && !(self.saved.is_empty() && self.saved_xmm.is_empty()) {
            self.prologue_win();
            return;
        }
        // push rbp; mov rbp, rsp: as the unwind encodings say.
        self.a.push_rbp();
        self.a.mov_st(Sz::Q, Rm::r(RBP), Gpr(RSP));
        self.alloc(self.frame);
        // mov [rbp - 8 (i + 1)], r
        for (i, r) in self.saved.clone().into_iter().enumerate() {
            let off = -8 * (i as i32 + 1);
            self.a.mov_st(Sz::Q, Rm::m(RBP, off), Gpr(r));
            // r8-r15 keep their numbers in DWARF.
            let dw = if r >= 8 {
                r as u16
            } else {
                [0, 2, 1, 3, 7, 6, 4, 5][r as usize]
            };
            self.saves.regs.push((dw, off - 16));
        }
        self.saves.at = self.a.pos() as u32;
    }

    /// `sub rsp, n`, touching every page on the way down (a guard page
    /// cannot be skipped).
    pub(crate) fn alloc(&mut self, n: u32) {
        let pages = n / PAGE;
        if pages > 8 {
            self.mov_imm(R11, u64::from(pages));
            let top = self.a.pos();
            self.sub_rsp(PAGE);
            self.probe();
            self.a.dec(Sz::Q, Gpr(R11));
            self.a.jcc(Cond::NE, Some(top));
        } else {
            for _ in 0..pages {
                self.sub_rsp(PAGE);
                self.probe();
            }
        }
        let rest = n % PAGE;
        if rest != 0 {
            self.sub_rsp(rest);
        }
    }

    /// `sub rsp, imm32`.
    pub(crate) fn sub_rsp(&mut self, n: u32) {
        self.alu_imm(Alu::Sub, Sz::Q, Rm::r(RSP), n as i32);
    }

    /// `or qword [rsp], 0`: touch the page.
    pub(crate) fn probe(&mut self) {
        self.a.alu_ri8(Alu::Or, Sz::Q, Rm::m(RSP, 0), 0);
    }

    /// Windows x64 with callee-saved registers: their area first, so the
    /// unwind codes reach it from the frame register: `push rbp; sub rsp,
    /// a; lea rbp, [rsp + a]`, the stores (`xmm` 16-byte aligned below the
    /// integer registers), then the rest of the frame. `saves.win` is the
    /// `UNWIND_INFO` saying so (`rbp` the frame register at `rsp + a`).
    pub(crate) fn prologue_win(&mut self) {
        let n = self.saved.len();
        let g = 16 * n.div_ceil(2) as u32;
        let a = win_saves(n, self.saved_xmm.len());
        // `(code offset, UNWIND_CODE op | info << 4, extra slots)`.
        let mut codes: Vec<(u8, u8, Vec<u16>)> = Vec::new();
        let at = |e: &Self| e.a.pos() as u8;
        self.a.push_rbp();
        codes.push((at(self), UWOP_PUSH_NONVOL | ((RBP as u8) << 4), Vec::new()));
        if a < 128 {
            self.a.alu_ri8(Alu::Sub, Sz::Q, Rm::r(RSP), a as i8);
        } else {
            self.sub_rsp(a);
        }
        let alloc = if a <= 128 {
            (UWOP_ALLOC_SMALL | (((a / 8 - 1) as u8) << 4), Vec::new())
        } else {
            (UWOP_ALLOC_LARGE, vec![(a / 8) as u16])
        };
        codes.push((at(self), alloc.0, alloc.1));
        self.a.lea(Gpr(RBP), Addr::Bd(Gpr(RSP), a as i32));
        codes.push((at(self), UWOP_SET_FPREG, Vec::new()));
        for (i, r) in self.saved.clone().into_iter().enumerate() {
            let off = -8 * (i as i32 + 1);
            self.a.mov_st(Sz::Q, Rm::m(RBP, off), Gpr(r));
            let slot = ((a as i32 + off) / 8) as u16;
            codes.push((at(self), UWOP_SAVE_NONVOL | ((r as u8) << 4), vec![slot]));
            // r8-r15 keep their numbers in DWARF.
            let dw = if r >= 8 {
                r as u16
            } else {
                [0, 2, 1, 3, 7, 6, 4, 5][r as usize]
            };
            self.saves.regs.push((dw, off - 16));
        }
        for (j, x) in self.saved_xmm.clone().into_iter().enumerate() {
            let off = -(g as i32) - 16 * (j as i32 + 1);
            self.a
                .sse_st(Sse::MovapsSt, Addr::Bd(Gpr(RBP), off), Xmm(x));
            let slot = ((a as i32 + off) / 16) as u16;
            codes.push((at(self), UWOP_SAVE_XMM128 | ((x as u8) << 4), vec![slot]));
            self.saves.regs.push((17 + x as u16, off - 16));
        }
        let prolog = self.a.pos() as u8;
        // The rest, the first page's probe as far below `rbp` as before.
        let rest = self.frame - a;
        if self.frame >= PAGE {
            self.sub_rsp(PAGE - a);
            self.probe();
            self.alloc(self.frame - PAGE);
        } else if rest != 0 {
            self.alloc(rest);
        }
        self.saves.at = self.a.pos() as u32;
        // UNWIND_INFO: version 1, the prolog's size, its codes last first.
        let mut list = Vec::new();
        for (off, op, extra) in codes.iter().rev() {
            list.extend_from_slice(&[*off, *op]);
            for e in extra {
                list.extend_from_slice(&e.to_le_bytes());
            }
        }
        let slots = list.len() / 2;
        let mut info = vec![
            1,
            prolog,
            slots as u8,
            (RBP as u8) | (((a / 16) as u8) << 4),
        ];
        info.extend_from_slice(&list);
        if slots % 2 == 1 {
            info.extend_from_slice(&[0, 0]);
        }
        self.saves.win = info;
    }

    pub(crate) fn epilogue(&mut self) {
        for (i, r) in self.saved.clone().into_iter().enumerate() {
            let off = -8 * (i as i32 + 1);
            self.a.mov(Sz::Q, Gpr(r), Rm::m(RBP, off));
        }
        let g = 16 * self.saved.len().div_ceil(2) as i32;
        for (j, x) in self.saved_xmm.clone().into_iter().enumerate() {
            let off = -g - 16 * (j as i32 + 1);
            self.a
                .sse(Sse::Movaps, Xmm(x), Xrm::M(Addr::Bd(Gpr(RBP), off)));
        }
        self.a.mov_st(Sz::Q, Rm::r(RSP), Gpr(RBP));
        self.a.pop_rbp();
        self.a.ret();
    }

    pub(crate) fn copy(&mut self, dst: Mem, src: Mem, size: u32, zero: bool) {
        if size > 128 {
            // A qword loop, not `rep movsb` / `rep stosb`: Rosetta runs
            // those a byte at a time.
            self.scratch();
            if !zero {
                self.lea(R10, src);
            }
            self.lea(R11, dst);
            self.mov_imm(RCX, u64::from(size / 8));
            if zero {
                self.alu(Alu::Xor, false, RAX, RAX);
            }
            let top = self.a.pos();
            if !zero {
                self.a.mov(Sz::Q, Gpr(RAX), Rm::m(R10, 0));
                self.a.alu_ri8(Alu::Add, Sz::Q, Rm::r(R10), 8);
            }
            self.a.mov_st(Sz::Q, Rm::m(R11, 0), Gpr(RAX));
            self.a.alu_ri8(Alu::Add, Sz::Q, Rm::r(R11), 8);
            self.a.dec(Sz::D, Gpr(RCX));
            self.a.jcc(Cond::NE, Some(top));
            self.copy_words(size % 8, zero);
            return;
        }
        if !zero {
            self.lea(R10, src);
        }
        self.lea(R11, dst);
        self.w(RAX, false);
        if zero {
            self.alu(Alu::Xor, false, RAX, RAX);
        }
        self.copy_words(size, zero);
        self.wrote(dst, size);
    }

    /// `[r11 + i] = [r10 + i]` (or zero) for `size` bytes.
    pub(crate) fn copy_words(&mut self, size: u32, zero: bool) {
        let mut off = 0i32;
        let mut rest = size;
        for (n, k) in [(8u32, K::I64), (4, K::I32), (2, K::I16), (1, K::I8)] {
            while rest >= n {
                if !zero {
                    self.sized_load(k, RAX, Rm::m(R10, off));
                }
                self.a.mov_st(sz(k), Rm::m(R11, off), Gpr(RAX));
                rest -= n;
                off += n as i32;
            }
        }
    }
}
