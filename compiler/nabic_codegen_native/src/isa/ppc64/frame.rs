//! Prologue, epilogue and block copies.

#[allow(clippy::wildcard_imports)]
use crate::isa::ppc64::*;

impl Em<'_> {
    /// `stdu r1, d(r1)`: also stores the back chain, touching the page.
    pub(crate) fn stdu(&mut self, d: i64) {
        self.ins(enc::stdu(R(SP), R(SP), d));
    }

    pub(crate) fn prologue(&mut self) {
        let toc = self.obj.sym(".TOC.");
        self.reloc(RelKind::PpcEntryHa, toc);
        self.ins(enc::addis(R(TOC), R(R12), 0));
        self.reloc(RelKind::PpcEntryLo, toc);
        self.addi(TOC, TOC, 0);
        self.ins(enc::mflr(R(R0)));
        self.st_word(FP, SP, -8);
        self.st_word(R0, SP, 16);
        self.mr(FP, SP);
        let n = self.frame;
        let pages = n / PAGE;
        if pages > 8 {
            self.li(R0, i64::from(pages));
            self.ins(enc::mtctr(R(R0)));
            let top = self.pos();
            self.stdu(-i64::from(PAGE));
            self.b_back(Cond::DNZ, top);
        } else {
            for _ in 0..pages {
                self.stdu(-i64::from(PAGE));
            }
        }
        let rest = n % PAGE;
        if rest != 0 {
            self.stdu(-i64::from(rest));
        }
        if pages > 0 {
            // The back chain: the caller's `r1`.
            self.st_word(FP, SP, 0);
        }
        for (i, &(r, fp)) in self.saved.clone().iter().enumerate() {
            let off = -16 - 8 * i as i64;
            self.ls(false, K::F64, false, fp, r, FP, off);
            let dw = if fp { 32 + r } else { r };
            self.saves.regs.push((dw as u16, off as i32));
        }
        if !self.saved.is_empty() {
            self.saves.at = 4 * self.pos() as u32;
        }
    }

    pub(crate) fn epilogue(&mut self) {
        for (i, &(r, fp)) in self.saved.clone().iter().enumerate() {
            self.ls(true, K::F64, false, fp, r, FP, -16 - 8 * i as i64);
        }
        self.ld_word(R0, FP, 16);
        self.ins(enc::mtlr(R(R0)));
        self.mr(SP, FP);
        self.ld_word(FP, SP, -8);
        self.ins(enc::blr());
    }

    pub(crate) fn copy(&mut self, dst: Mem, src: Mem, size: u32, zero: bool) {
        if !zero {
            self.lea(R9, src);
        }
        self.lea(R10, dst);
        let v = if zero {
            self.li(R0, 0);
            R0
        } else {
            R11
        };
        let mut rest = if size > 128 {
            // A doubleword loop over moving addresses.
            self.li(R8, i64::from(size / 8));
            self.ins(enc::mtctr(R(R8)));
            let top = self.pos();
            if !zero {
                self.ld_word(R11, R9, 0);
                self.addi(R9, R9, 8);
            }
            self.st_word(v, R10, 0);
            self.addi(R10, R10, 8);
            self.b_back(Cond::DNZ, top);
            size % 8
        } else {
            size
        };
        let mut off = 0i64;
        for k in [K::I64, K::I32, K::I16, K::I8] {
            let n = k.size();
            while rest >= n {
                if !zero {
                    self.ls(true, k, false, false, R11, R9, off);
                }
                self.ls(false, k, false, false, v, R10, off);
                rest -= n;
                off += i64::from(n);
            }
        }
    }
}
