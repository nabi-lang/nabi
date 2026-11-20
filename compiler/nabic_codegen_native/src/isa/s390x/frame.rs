//! Prologue, epilogue and block copies.

#[allow(clippy::wildcard_imports)]
use crate::isa::s390x::*;

impl Em<'_> {
    pub(crate) fn prologue(&mut self) {
        self.a.rsy(Rsy::Stmg, R6, R15, R15, 48);
        self.a.rre(Rre::Lgr, R11, R15);
        let n = self.frame;
        let pages = n / PAGE;
        let probe = |e: &mut Self| {
            e.a.ri(Ri::Aghi, R15, (-(PAGE as i32)) as u16);
            e.a.rxy(Rxy::Stg, R0, 0, R15, 0);
        };
        if pages > 8 {
            self.li(R1, i64::from(pages));
            let top = self.a.pos();
            probe(self);
            self.a.brctg(R1, top);
        } else {
            for _ in 0..pages {
                probe(self);
            }
        }
        self.add_imm(R15, -i64::from(n % PAGE));
        // `std fN, -8 (i + 1)(r11)`: at CFA - 160 - 8 (i + 1).
        for (i, f) in self.fsaved.clone().into_iter().enumerate() {
            let off = -8 * (i as i32 + 1);
            self.a.rxy(Rxy::Stdy, f, 0, R11, off);
            self.saves.regs.push((dwarf_f(f), off - SAVE as i32));
        }
        if !self.fsaved.is_empty() {
            self.saves.at = self.a.pos() as u32;
        }
    }

    pub(crate) fn epilogue(&mut self) {
        for (i, f) in self.fsaved.clone().into_iter().enumerate() {
            self.a.rxy(Rxy::Ldy, f, 0, R11, -8 * (i as i32 + 1));
        }
        self.a.rsy(Rsy::Lmg, R6, R15, R11, 48);
        self.a.br(R14);
    }

    /// `mvc` (`zero`: `xc` of `dst` with itself) in 256-byte pieces.
    pub(crate) fn copy(&mut self, dst: Mem, src: Mem, size: u32, zero: bool) {
        let (op, sb) = if zero { (Ss::Xc, SB) } else { (Ss::Mvc, P) };
        if !zero {
            self.lea(P, src);
        }
        self.lea(SB, dst);
        let mut rest = if size > 1024 {
            self.li(R1, i64::from(size / 256));
            let top = self.a.pos();
            self.a.ss(op, 256, SB, 0, sb, 0);
            if !zero {
                self.a.la(P, 0, P, 256);
            }
            self.a.la(SB, 0, SB, 256);
            self.a.brctg(R1, top);
            size % 256
        } else {
            size
        };
        let mut off = 0;
        while rest > 0 {
            let n = rest.min(256);
            self.a.ss(op, n, SB, off, sb, off);
            off += n;
            rest -= n;
        }
    }
}
