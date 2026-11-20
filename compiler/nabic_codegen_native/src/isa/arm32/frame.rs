//! Prologue, epilogue and block copies.

#[allow(clippy::wildcard_imports)]
use crate::isa::arm32::*;

impl Em<'_> {
    pub(crate) fn prologue(&mut self) {
        if self.va > 0 {
            // `push {r0-r3}`: below the stack arguments, a `va_list`
            // walks on from them into those.
            self.ins(enc::push(0x000f));
        }
        for i in PROLOGUE {
            self.ins(i);
        }
        let n = self.frame;
        let pages = n / PAGE;
        // Every page touched in turn (`ldr lr, [sp]`: `lr` is saved).
        if pages > 8 {
            self.mov_imm(IP, pages);
            let top = self.pos();
            self.dpi(
                Cond::AL,
                Op::Sub,
                false,
                SP,
                SP,
                modimm(PAGE).unwrap_or(Imm::small(0)),
            );
            self.ins(enc::ls(Acc::Ldr, enc::LR, enc::SP, 0));
            self.dpi(Cond::AL, Op::Sub, true, IP, IP, Imm::small(1));
            self.b_back(Cond::NE, top);
        } else {
            for _ in 0..pages {
                self.dpi(
                    Cond::AL,
                    Op::Sub,
                    false,
                    SP,
                    SP,
                    modimm(PAGE).unwrap_or(Imm::small(0)),
                );
                self.ins(enc::ls(Acc::Ldr, enc::LR, enc::SP, 0));
            }
        }
        let rest = n % PAGE;
        if rest != 0 {
            self.add_imm(SP, SP, -(rest as i32));
            // More than a page below the caller's last touch.
            if pages == 0 && n + SAVED >= PAGE {
                self.ins(enc::ls(Acc::Ldr, enc::LR, enc::SP, 0));
            }
        }
    }

    pub(crate) fn epilogue(&mut self) {
        // mov sp, r11; pop {r4-r11, pc}
        self.ins(enc::mov(enc::SP, R(FP)));
        if self.va > 0 {
            // pop {r4-r11, lr}; add sp, sp, #16; bx lr
            self.ins(enc::pop(0x4ff0));
            self.add_imm(SP, SP, self.va as i32);
            self.ins(enc::bx(enc::LR));
            return;
        }
        self.ins(enc::pop(0x8ff0));
    }

    pub(crate) fn copy(&mut self, dst: Mem, src: Mem, size: u32, zero: bool) {
        // r9 r10 are the allocator's above -O0.
        let (s, d) = if self.opt { (R2, R3) } else { (R9, R10) };
        if !zero {
            self.lea(s, src);
        }
        self.lea(d, dst);
        if zero {
            self.mov_imm(self.x, 0);
        }
        let mut rest = if size > 128 {
            // A word loop with post-incremented addresses.
            self.mov_imm(IP, size / 4);
            let top = self.pos();
            if !zero {
                self.ins(enc::ls_post4(true, R(self.x), R(s)));
            }
            self.ins(enc::ls_post4(false, R(self.x), R(d)));
            self.dpi(Cond::AL, Op::Sub, true, IP, IP, Imm::small(1));
            self.b_back(Cond::NE, top);
            size % 4
        } else {
            size
        };
        let mut off = 0i32;
        for k in [K::I32, K::I16, K::I8] {
            let n = k.size();
            while rest >= n {
                if !zero {
                    self.ls(Acc::load(k, false), self.x, s, off, s);
                }
                self.ls(Acc::store(k), self.x, d, off, d);
                rest -= n;
                off += n as i32;
            }
        }
    }
}
