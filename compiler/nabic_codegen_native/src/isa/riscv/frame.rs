//! Prologue, epilogue and block copies.

#[allow(clippy::wildcard_imports)]
use crate::isa::riscv::*;

impl Em<'_> {
    pub(crate) fn prologue(&mut self) {
        let w = i64::from(self.xlen() / 8);
        if self.va > 0 {
            // `a0`-`a7` right below the stack arguments: a `va_list`
            // walks on from one into the other.
            self.addi(SP, SP, -(self.va as i32));
            for r in 0..8 {
                self.st_word(A0 + r, SP, i64::from(r) * w);
            }
        }
        self.addi(SP, SP, -16);
        self.st_word(RA, SP, 16 - w);
        self.st_word(S0, SP, 16 - 2 * w);
        self.addi(S0, SP, 16);
        let n = self.frame;
        // rv32 (bare metal) has no guard page to probe.
        let pages = if self.rv32 { 0 } else { n / PAGE };
        if pages > 8 {
            self.li(T5, i64::from(pages));
            let top = self.a.pos();
            self.lui(T6, 1);
            self.alu(SUB, SP, SP, T6);
            self.st_word(ZERO, SP, 0);
            self.addi(T5, T5, -1);
            self.b_back(BNE, T5, ZERO, top);
        } else {
            for _ in 0..pages {
                self.lui(T6, 1);
                self.alu(SUB, SP, SP, T6);
                self.st_word(ZERO, SP, 0);
            }
        }
        let rest = if self.rv32 { n } else { n % PAGE };
        if rest != 0 {
            self.add_imm(SP, SP, -i64::from(rest), T6);
        }
        if self.saved.is_empty() {
            return;
        }
        for (i, &(r, fp)) in self.saved.clone().iter().enumerate() {
            let off = -16 - 8 * (i as i32 + 1);
            if fp {
                self.ls(Ls::Fs(true), r, S0, i64::from(off));
            } else {
                self.st_word(r, S0, i64::from(off));
            }
            // DWARF: `f` registers from 32.
            let dw = if fp { 32 + r } else { r };
            self.saves.regs.push((dw as u16, off));
        }
        self.saves.at = 4 * self.a.pos() as u32;
    }

    pub(crate) fn epilogue(&mut self) {
        let w = i64::from(self.xlen() / 8);
        for (i, &(r, fp)) in self.saved.clone().iter().enumerate() {
            let off = -16 - 8 * (i as i64 + 1);
            if fp {
                self.ls(Ls::Fl(true), r, S0, off);
            } else {
                self.ld_word(r, S0, off);
            }
        }
        self.addi(SP, S0, -16);
        self.ld_word(RA, SP, 16 - w);
        self.ld_word(S0, SP, 16 - 2 * w);
        self.addi(SP, SP, 16 + self.va as i32);
        self.ins(enc::jalr(enc::ZERO, enc::RA, 0));
    }

    pub(crate) fn copy(&mut self, dst: Mem, src: Mem, size: u32, zero: bool) {
        if !zero {
            self.lea(T4, src);
        }
        self.lea(T5, dst);
        let w = self.xlen() / 8;
        let wk = self.word();
        let mut rest = if size > 128 {
            // A word loop over moving addresses.
            self.li(T3, i64::from(size / w));
            let top = self.a.pos();
            if !zero {
                self.ls(self.load_f3(wk, false), T0, T4, 0);
                self.addi(T4, T4, w as i32);
            }
            let v = if zero { ZERO } else { T0 };
            self.ls(Self::store_f3(wk), v, T5, 0);
            self.addi(T5, T5, w as i32);
            self.addi(T3, T3, -1);
            self.b_back(BNE, T3, ZERO, top);
            size % w
        } else {
            size
        };
        let mut off = 0i64;
        for k in [K::I64, K::I32, K::I16, K::I8] {
            let n = k.size();
            if n > w {
                continue;
            }
            while rest >= n {
                let v = if zero { ZERO } else { T0 };
                if !zero {
                    self.ls(self.load_f3(k, false), T0, T4, off);
                }
                self.ls(Self::store_f3(k), v, T5, off);
                rest -= n;
                off += i64::from(n);
            }
        }
    }
}
