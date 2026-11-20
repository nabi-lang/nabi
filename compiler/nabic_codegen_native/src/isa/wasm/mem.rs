//! Loads, stores, locals access and block copies.

#[allow(clippy::wildcard_imports)]
use crate::isa::wasm::*;

impl Em<'_> {
    /// Push the low `k` of immediate `x`.
    pub(crate) fn imm(&mut self, x: i64, k: K) {
        match k {
            K::I8 => self.i32c((x & 0xff) as i32),
            K::I16 => self.i32c((x & 0xffff) as i32),
            K::I32 => self.i32c(x as i32),
            K::I64 => self.i64c(x),
            K::F32 => self.a.f32_const(x as u32),
            K::F64 => self.a.f64_const(x as u64),
        }
    }

    /// Push the base address of memory `m`; the offset to add.
    pub(crate) fn base(&mut self, m: Mem) -> u32 {
        match m {
            Mem::Frame(s, o) => {
                self.get(self.fp);
                self.slot_off[s as usize] + o
            }
            Mem::Ind(s, o) => {
                self.load(Mem::Frame(s, 0), K::I32);
                if o >= 0 {
                    o as u32
                } else {
                    self.i32c(o);
                    self.a.num(Num::I32Add);
                    0
                }
            }
        }
    }

    /// Push the address of `m`.
    pub(crate) fn addr(&mut self, m: Mem) {
        let off = self.base(m);
        if off != 0 {
            self.i32c(off as i32);
            self.a.num(Num::I32Add);
        }
    }

    pub(crate) fn reg(&self, m: Mem) -> Option<Reg> {
        match m {
            Mem::Frame(s, o) => self.regs.get(&(s, o)).copied(),
            Mem::Ind(..) => None,
        }
    }

    /// The value of local type `from` as `to` (same size).
    pub(crate) fn cast(&mut self, from: u8, to: u8) {
        match (from, to) {
            (I32, F32) => self.a.num(Num::F32ReinterpretI32),
            (I64, F64) => self.a.num(Num::F64ReinterpretI64),
            (F32, I32) => self.a.num(Num::I32ReinterpretF32),
            (F64, I64) => self.a.num(Num::I64ReinterpretF64),
            _ => {}
        }
    }

    /// Push `k` from `m`.
    pub(crate) fn load(&mut self, m: Mem, k: K) {
        if let Some(r) = self.reg(m) {
            self.get(r.local);
            self.cast(r.ty, vt(k));
            return;
        }
        let off = self.base(m);
        let (op, align) = match k {
            K::I8 => (MemOp::I32Load8U, 0),
            K::I16 => (MemOp::I32Load16U, 1),
            K::I32 => (MemOp::I32Load, 2),
            K::I64 => (MemOp::I64Load, 3),
            K::F32 => (MemOp::F32Load, 2),
            K::F64 => (MemOp::F64Load, 3),
        };
        self.a.mem(op, align, off);
    }

    /// Push `k` of `v`.
    pub(crate) fn val(&mut self, v: V, k: K) {
        match v {
            V::I(x) => self.imm(x, k),
            V::M(m) => self.load(m, k),
        }
    }

    /// Sign-extend the narrow `i32` on top when `signed`.
    pub(crate) fn ext(&mut self, k: K, signed: bool) {
        if signed {
            match k {
                K::I8 => self.a.num(Num::I32Extend8S),
                K::I16 => self.a.num(Num::I32Extend16S),
                _ => {}
            }
        }
    }

    /// Push `k` of `v`, sign-extended within `i32` when `signed`.
    pub(crate) fn val_ext(&mut self, v: V, k: K, signed: bool) {
        self.val(v, k);
        self.ext(k, signed);
    }

    /// Begin a store of `k` to `m` (the address first); push the value,
    /// then `st_end`.
    pub(crate) fn st_begin(&mut self, m: Mem, k: K) -> St {
        if let Some(r) = self.reg(m) {
            return St::Local(r, k);
        }
        let off = self.base(m);
        St::Mem(off, k)
    }

    pub(crate) fn st_end(&mut self, s: St) {
        match s {
            St::Local(r, k) => {
                match k {
                    K::I8 => {
                        self.i32c(0xff);
                        self.a.num(Num::I32And);
                    }
                    K::I16 => {
                        self.i32c(0xffff);
                        self.a.num(Num::I32And);
                    }
                    _ => {}
                }
                self.cast(vt(k), r.ty);
                self.set(r.local);
            }
            St::Mem(off, k) => {
                let (op, align) = match k {
                    K::I8 => (MemOp::I32Store8, 0),
                    K::I16 => (MemOp::I32Store16, 1),
                    K::I32 => (MemOp::I32Store, 2),
                    K::I64 => (MemOp::I64Store, 3),
                    K::F32 => (MemOp::F32Store, 2),
                    K::F64 => (MemOp::F64Store, 3),
                };
                self.a.mem(op, align, off);
            }
        }
    }

    /// Store local `l` (`k`) to `m`.
    pub(crate) fn store_local(&mut self, m: Mem, k: K, l: u32) {
        let s = self.st_begin(m, k);
        self.get(l);
        self.st_end(s);
    }

    /// Copy `size` bytes from `src` (`None`: zeros) to `dst`.
    pub(crate) fn copy(&mut self, dst: Mem, src: Option<Mem>, size: u32) {
        if size == 0 {
            return;
        }
        if size > 64 {
            self.addr(dst);
            if let Some(s) = src {
                self.addr(s);
                self.i32c(size as i32);
                self.a.memory_copy();
            } else {
                self.i32c(0);
                self.i32c(size as i32);
                self.a.memory_fill();
            }
            return;
        }
        // Both bases in locals, then word by word.
        let d = self.tmp(I32);
        let doff = self.base(dst);
        self.set(d);
        let s = src.map(|m| {
            let l = self.tmp(I32);
            let off = self.base(m);
            self.set(l);
            (l, off)
        });
        let mut at = 0;
        for k in [K::I64, K::I32, K::I16, K::I8] {
            while size - at >= k.size() {
                self.get(d);
                match s {
                    Some((l, off)) => {
                        self.get(l);
                        let op = match k {
                            K::I8 => MemOp::I32Load8U,
                            K::I16 => MemOp::I32Load16U,
                            K::I32 => MemOp::I32Load,
                            _ => MemOp::I64Load,
                        };
                        self.a.mem(op, 0, off + at);
                    }
                    None => self.imm(0, k),
                }
                let op = match k {
                    K::I8 => MemOp::I32Store8,
                    K::I16 => MemOp::I32Store16,
                    K::I32 => MemOp::I32Store,
                    _ => MemOp::I64Store,
                };
                self.a.mem(op, 0, doff + at);
                at += k.size();
            }
        }
    }
}
