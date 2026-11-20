//! Prologue, epilogue, stack allocation and block copies.

#[allow(clippy::wildcard_imports)]
use crate::isa::a64::*;

impl Em<'_> {
    pub(crate) fn prologue(&mut self) {
        if self.obj.coff && !self.pairs.is_empty() {
            self.prologue_win();
            return;
        }
        for i in PRO {
            self.ins(i);
        }
        self.alloc(self.frame);
        // stp second, first, [x29, #-16 * (i + 1)]: `first` higher.
        for (i, &(a, b, fp)) in self.pairs.clone().iter().enumerate() {
            let off = -2 * (i as i32 + 1);
            let op = if fp { Pair::StpD } else { Pair::StpX };
            self.a.push(enc::pair(op, Index::Offset, b, a, R(FP), off));
            let dw = |r: u32| (if fp { 64 + r } else { r }) as u16;
            let cfa = -16 * i as i32 - 24;
            self.saves.regs.push((dw(a), cfa));
            self.saves.regs.push((dw(b), cfa - 8));
            self.saves.compact |= if fp {
                0x100 << ((a - 8) / 2)
            } else {
                1 << ((a - 19) / 2)
            };
        }
        self.saves.at = 4 * self.a.pos() as u32;
    }

    /// `sub sp, sp, n`, touching every page on the way down (a guard page
    /// cannot be skipped).
    pub(crate) fn alloc(&mut self, n: u32) {
        let pages = n / PAGE;
        if pages > 0 {
            if pages > 8 {
                self.mov_imm(X17, u64::from(pages));
                let top = self.pos();
                self.page();
                self.ins(enc::add_imm(AddOp::Subs, true, R(X17), R(X17), 1, false));
                let d = top as i64 - self.pos() as i64;
                self.ins(enc::b_cond(Cond::NE, d));
            } else {
                for _ in 0..pages {
                    self.page();
                }
            }
        }
        let rest = n % PAGE;
        if rest != 0 {
            self.add_imm(SP, SP, -i64::from(rest));
        }
    }

    /// `sub sp, sp, #1, lsl #12; str xzr, [sp]`: a page down, touched.
    pub(crate) fn page(&mut self) {
        self.ins(enc::add_imm(AddOp::Sub, true, enc::SP, enc::SP, 1, true));
        self.probe();
    }

    /// `str xzr, [sp]`.
    pub(crate) fn probe(&mut self) {
        self.ins(enc::ldst(STRX, ZR, enc::SP, 0));
    }

    /// Windows with callee-saved pairs: their area first, stored from `sp`
    /// (the lower register lower) before `x29` is set, as unwind codes
    /// describe: `stp x29, x30, [sp, #-16]!; sub sp, sp, #a; stp ..;
    /// add x29, sp, #a`, then the rest of the frame. `saves.win` holds the
    /// codes.
    pub(crate) fn prologue_win(&mut self) {
        let n = self.pairs.len() as u32;
        let a = 16 * n;
        self.ins(PRO[0]);
        self.ins(enc::add_imm(AddOp::Sub, true, enc::SP, enc::SP, a, false));
        let mut codes = vec![0xe4, 0x81, a as u8 / 16]; // end, save_fplr_x, alloc_s
        for (i, &(lo, hi, fp)) in self.pairs.clone().iter().enumerate() {
            let z = 2 * (n - 1 - i as u32);
            let op = if fp { Pair::StpD } else { Pair::StpX };
            self.ins(enc::pair(op, Index::Offset, lo, hi, enc::SP, z as i32));
            // save_fregp / save_regp, both bytes (reversed with the rest).
            let (base, first) = if fp { (8, 0xd8) } else { (19, 0xc8) };
            let x = lo - base;
            codes.push((((x & 3) << 6) | z) as u8);
            codes.push(first | (x >> 2) as u8);
            let dw = |r: u32| (if fp { 64 + r } else { r }) as u16;
            let cfa = -16 * i as i32 - 24;
            self.saves.regs.push((dw(hi), cfa));
            self.saves.regs.push((dw(lo), cfa - 8));
        }
        self.ins(enc::add_imm(AddOp::Add, true, R(FP), enc::SP, a, false));
        codes.extend_from_slice(&[(a / 8) as u8, 0xe2]); // add_fp
        codes.reverse();
        self.saves.win = codes;
        // The rest, the first page's probe as far below `x29` as before.
        if self.frame >= PAGE {
            self.add_imm(SP, SP, -i64::from(PAGE - a));
            self.probe();
            self.alloc(self.frame - PAGE);
        } else {
            self.alloc(self.frame - a);
        }
        self.saves.at = 4 * self.a.pos() as u32;
    }

    pub(crate) fn epilogue(&mut self) {
        for (i, &(a, b, fp)) in self.pairs.clone().iter().enumerate() {
            let off = -2 * (i as i32 + 1);
            let op = if fp { Pair::LdpD } else { Pair::LdpX };
            // Windows stores the lower register lower.
            let (rt, rt2) = if self.obj.coff { (a, b) } else { (b, a) };
            self.a
                .push(enc::pair(op, Index::Offset, rt, rt2, R(FP), off));
        }
        for i in EPI {
            self.ins(i);
        }
    }

    pub(crate) fn copy(&mut self, dst: Mem, src: Mem, size: u32, zero: bool) {
        let words = size / 8;
        if words <= 16 {
            // Straight through the bases; x16 carries the data (never
            // `Held` here, so neither base is it).
            self.clobber(X16);
            let (sb, soff) = if zero { (ZR, 0) } else { self.base(src, X14) };
            // The destination's pointer must not land on the source's.
            let (db, doff) = self.base(dst, if sb == X15 { X14 } else { X15 });
            let mut off = 0i64;
            // Above `-O0`: pairs while both ends reach (signed 7-bit,
            // scaled by 8).
            let pairs = |o: i64| o % 8 == 0 && (-512..=504).contains(&o);
            while self.opt
                && size as i64 - off >= 16
                && pairs(doff + off)
                && (zero || pairs(soff + off))
            {
                let d = ((doff + off) / 8) as i32;
                if zero {
                    self.a
                        .push(enc::pair(Pair::StpX, Index::Offset, ZR, ZR, R(db), d));
                } else {
                    let s = soff + off;
                    self.frame_read(sb, s, 16);
                    self.clobber(X17);
                    self.ins(enc::pair(
                        Pair::LdpX,
                        Index::Offset,
                        X16,
                        X17,
                        R(sb),
                        (s / 8) as i32,
                    ));
                    self.clobber(X17);
                    self.a
                        .push(enc::pair(Pair::StpX, Index::Offset, X16, X17, R(db), d));
                }
                off += 16;
            }
            for (n, ld, st) in [
                (8, LDRX, STRX),
                (4, LDRW, STRW),
                (2, LDRH, STRH),
                (1, LDRB, STRB),
            ] {
                while (size as i64 - off) >= n {
                    if zero {
                        self.ldst(st, ZR, db, doff + off);
                    } else {
                        self.ldst(ld, X16, sb, soff + off);
                        self.ldst(st, X16, db, doff + off);
                    }
                    off += n;
                }
            }
            self.wrote(dst, size);
            return;
        }
        // A loop: nothing may be emitted into it.
        self.flush(None);
        // Addresses into x14 (source) and x15 (destination).
        if !zero {
            self.addr(X14, src);
        }
        self.addr(X15, dst);
        let mut off = 0i64;
        // Above `-O0`: a pair of words a turn, then the odd one.
        let (step, turns) = if self.opt {
            (16, words / 2)
        } else {
            (8, words)
        };
        {
            self.mov_imm(X13, u64::from(turns));
            let top = self.pos();
            match (zero, step) {
                (true, 16) => self.ins(enc::pair(Pair::StpX, Index::Post, ZR, ZR, R(X15), 2)),
                (false, 16) => {
                    self.ins(enc::pair(Pair::LdpX, Index::Post, X9, X10, R(X14), 2));
                    self.ins(enc::pair(Pair::StpX, Index::Post, X9, X10, R(X15), 2));
                }
                (true, _) => self.ins(enc::ldst_post(STRX, ZR, R(X15), 8)),
                (false, _) => {
                    self.ins(enc::ldst_post(LDRX, X9, R(X14), 8));
                    self.ins(enc::ldst_post(STRX, X9, R(X15), 8));
                }
            }
            self.ins(enc::add_imm(AddOp::Subs, true, R(X13), R(X13), 1, false));
            let d = top as i64 - self.pos() as i64;
            self.ins(enc::b_cond(Cond::NE, d));
        }
        self.clobber(X10);
        if step == 16 && words % 2 == 1 {
            if zero {
                self.ldst(STRX, ZR, X15, 0);
            } else {
                self.ldst(LDRX, X9, X14, 0);
                self.ldst(STRX, X9, X15, 0);
            }
            off += 8;
        }
        // Post-indexed: the bases moved.
        self.clobber(X14);
        self.clobber(X15);
        self.wrote(dst, size);
        let mut rest = size % 8;
        for (n, ld, st) in [(4, LDRW, STRW), (2, LDRH, STRH), (1, LDRB, STRB)] {
            while rest >= n {
                if zero {
                    self.ldst(st, ZR, X15, off);
                } else {
                    self.ldst(ld, X9, X14, off);
                    self.ldst(st, X9, X15, off);
                }
                off += i64::from(n);
                rest -= n;
            }
        }
    }
}
