//! Instruction selection, comparisons and atomics.

#[allow(clippy::wildcard_imports)]
use crate::isa::wasm::*;

impl Em<'_> {
    pub(crate) fn inst(&mut self, i: &Inst) {
        self.used = [0; 4];
        match i {
            Inst::Label(_) | Inst::Jmp(_) | Inst::Br { .. } => {}
            Inst::Line(line, col, scope) => {
                let at = self.a.pos() as u32;
                self.lines.push((at, *line, *col, *scope));
            }
            Inst::Mov { k, dst, src } => {
                let s = self.st_begin(*dst, *k);
                self.val(*src, *k);
                self.st_end(s);
            }
            Inst::Copy { dst, src, size } => match scalar(*size) {
                Some(k) => {
                    // A float local keeps its kind across the copy.
                    let k = match (self.reg(*src), self.reg(*dst)) {
                        (Some(r), _) | (None, Some(r)) if r.ty == F64 && k == K::I64 => K::F64,
                        (Some(r), _) | (None, Some(r)) if r.ty == F32 && k == K::I32 => K::F32,
                        _ => k,
                    };
                    let s = self.st_begin(*dst, k);
                    self.load(*src, k);
                    self.st_end(s);
                }
                None => self.copy(*dst, Some(*src), *size),
            },
            Inst::Zero { dst, size } => self.copy(*dst, None, *size),
            Inst::Addr { dst, src } => {
                let s = self.st_begin(*dst, K::I32);
                self.addr(*src);
                self.st_end(s);
            }
            Inst::Sym { dst, sym, .. } => {
                let s = self.st_begin(*dst, K::I32);
                self.a.i32_const_rel(Rel::Addr(*sym));
                self.st_end(s);
            }
            Inst::Tls { dst, sym } => {
                let s = self.st_begin(*dst, K::I32);
                if self.threads() {
                    self.a.global_get(self.tls_base);
                    self.a.i32_const_rel(Rel::Tls(*sym));
                    self.a.num(Num::I32Add);
                } else {
                    self.a.i32_const_rel(Rel::Addr(*sym));
                }
                self.st_end(s);
            }
            Inst::Bin {
                op,
                k,
                signed,
                dst,
                a,
                b,
            } => {
                let s = self.st_begin(*dst, *k);
                let ext = *signed && matches!(op, Bin::Div | Bin::Rem | Bin::Shr);
                self.val_ext(*a, *k, ext);
                self.val_ext(*b, *k, ext);
                self.bin(*op, *k, *signed);
                self.st_end(s);
            }
            Inst::Ovf {
                op,
                k,
                signed,
                dst,
                a,
                b,
                ..
            } => self.ovf(*op, *k, *signed, *dst, *a, *b),
            Inst::Fma { .. } => unreachable!("no fused multiply-add in wasm"),
            Inst::Un { op, k, dst, a } => {
                let s = self.st_begin(*dst, *k);
                self.un(*op, *k, *a);
                self.st_end(s);
            }
            Inst::Cmp {
                cc,
                k,
                signed,
                dst,
                a,
                b,
            } => {
                let s = self.st_begin(*dst, K::I8);
                self.val_ext(*a, *k, *signed);
                self.val_ext(*b, *k, *signed);
                self.cmp(*cc, *k, *signed);
                self.st_end(s);
            }
            Inst::Conv {
                conv,
                from,
                to,
                dst,
                a,
            } => {
                let s = self.st_begin(*dst, *to);
                self.conv(*conv, *from, *to, *a);
                self.st_end(s);
            }
            Inst::AtomicLoad { k, dst, ptr, .. } => {
                let s = self.st_begin(*dst, *k);
                let ik = int_k(*k);
                if self.threads() {
                    self.addr(*ptr);
                    self.atomic(Atomic::Load, ik);
                } else {
                    self.addr(*ptr);
                    self.plain_load(ik);
                }
                self.cast(vt(ik), vt(*k));
                self.st_end(s);
            }
            Inst::AtomicStore { k, ptr, src, .. } => {
                let ik = int_k(*k);
                self.addr(*ptr);
                self.val(*src, *k);
                self.cast(vt(*k), vt(ik));
                if self.threads() {
                    self.atomic(Atomic::Store, ik);
                } else {
                    self.plain_store(ik);
                }
            }
            Inst::AtomicRmw {
                op,
                k,
                dst,
                ptr,
                val,
                ..
            } => self.rmw(*op, int_k(*k), *dst, *ptr, *val),
            Inst::Cas {
                k,
                dst,
                ptr,
                expected,
                new,
                ..
            } => self.cas(int_k(*k), *dst, *ptr, *expected, *new),
            Inst::Call {
                callee,
                args,
                rets,
                sret,
                variadic,
                ..
            } => self.call(callee, args, rets, *sret, *variadic),
            Inst::Param { part, dst } => {
                let idx = match part.loc {
                    Loc::Sret => 0,
                    Loc::Int(n) => u32::from(n) + u32::from(self.sret),
                    _ => unreachable!("wasm32 parameters are positions"),
                };
                let s = self.st_begin(*dst, part.k);
                self.get(idx);
                self.st_end(s);
            }
            Inst::Ret { vals } => self.ret(vals),
            Inst::Trap => self.a.unreachable(),
            Inst::VLoop(v) => self.vloop(v),
            Inst::V(x) => self.vx(x),
        }
    }

    pub(crate) fn cmp(&mut self, cc: Cc, k: K, signed: bool) {
        let o = match (vt(k), cc, signed) {
            (I32, Cc::Eq, _) => Num::I32Eq,
            (I32, Cc::Ne, _) => Num::I32Ne,
            (I32, Cc::Lt, true) => Num::I32LtS,
            (I32, Cc::Lt, false) => Num::I32LtU,
            (I32, Cc::Gt, true) => Num::I32GtS,
            (I32, Cc::Gt, false) => Num::I32GtU,
            (I32, Cc::Le, true) => Num::I32LeS,
            (I32, Cc::Le, false) => Num::I32LeU,
            (I32, Cc::Ge, true) => Num::I32GeS,
            (I32, Cc::Ge, false) => Num::I32GeU,
            (I64, Cc::Eq, _) => Num::I64Eq,
            (I64, Cc::Ne, _) => Num::I64Ne,
            (I64, Cc::Lt, true) => Num::I64LtS,
            (I64, Cc::Lt, false) => Num::I64LtU,
            (I64, Cc::Gt, true) => Num::I64GtS,
            (I64, Cc::Gt, false) => Num::I64GtU,
            (I64, Cc::Le, true) => Num::I64LeS,
            (I64, Cc::Le, false) => Num::I64LeU,
            (I64, Cc::Ge, true) => Num::I64GeS,
            (I64, Cc::Ge, false) => Num::I64GeU,
            (F32, Cc::Eq, _) => Num::F32Eq,
            (F32, Cc::Ne, _) => Num::F32Ne,
            (F32, Cc::Lt, _) => Num::F32Lt,
            (F32, Cc::Gt, _) => Num::F32Gt,
            (F32, Cc::Le, _) => Num::F32Le,
            (F32, Cc::Ge, _) => Num::F32Ge,
            (_, Cc::Eq, _) => Num::F64Eq,
            (_, Cc::Ne, _) => Num::F64Ne,
            (_, Cc::Lt, _) => Num::F64Lt,
            (_, Cc::Gt, _) => Num::F64Gt,
            (_, Cc::Le, _) => Num::F64Le,
            (_, Cc::Ge, _) => Num::F64Ge,
        };
        self.a.num(o);
    }

    /// An atomic access of `k` (integer).
    pub(crate) fn atomic(&mut self, op: Atomic, k: K) {
        let w = match k {
            K::I8 => Width::W8,
            K::I16 => Width::W16,
            K::I64 => Width::W64,
            _ => Width::W32,
        };
        self.a.atomic(op, w);
    }

    pub(crate) fn plain_load(&mut self, k: K) {
        let (op, align) = match k {
            K::I8 => (MemOp::I32Load8U, 0),
            K::I16 => (MemOp::I32Load16U, 1),
            K::I64 => (MemOp::I64Load, 3),
            _ => (MemOp::I32Load, 2),
        };
        self.a.mem(op, align, 0);
    }

    pub(crate) fn plain_store(&mut self, k: K) {
        let (op, align) = match k {
            K::I8 => (MemOp::I32Store8, 0),
            K::I16 => (MemOp::I32Store16, 1),
            K::I64 => (MemOp::I64Store, 3),
            _ => (MemOp::I32Store, 2),
        };
        self.a.mem(op, align, 0);
    }

    pub(crate) fn rmw(&mut self, op: Rmw, k: K, dst: Mem, ptr: Mem, val: V) {
        let t = vt(k);
        if self.threads() {
            let r = self.tmp(t);
            self.addr(ptr);
            self.val(val, k);
            let a = match op {
                Rmw::Add => Atomic::Add,
                Rmw::Sub => Atomic::Sub,
                Rmw::And => Atomic::And,
                Rmw::Or => Atomic::Or,
                Rmw::Xor => Atomic::Xor,
                Rmw::Xchg => Atomic::Xchg,
            };
            self.atomic(a, k);
            self.set(r);
            self.store_local(dst, k, r);
            return;
        }
        let (p, old) = (self.tmp(I32), self.tmp(t));
        self.addr(ptr);
        self.a.local_tee(p);
        self.plain_load(k);
        self.set(old);
        self.get(p);
        if op == Rmw::Xchg {
            self.val(val, k);
        } else {
            self.get(old);
            self.val(val, k);
            self.bin(
                match op {
                    Rmw::Add => Bin::Add,
                    Rmw::Sub => Bin::Sub,
                    Rmw::And => Bin::And,
                    Rmw::Or => Bin::Or,
                    _ => Bin::Xor,
                },
                k,
                false,
            );
        }
        self.plain_store(k);
        self.store_local(dst, k, old);
    }

    pub(crate) fn cas(&mut self, k: K, dst: Mem, ptr: Mem, expected: V, new: V) {
        let t = vt(k);
        let old = self.tmp(t);
        if self.threads() {
            self.addr(ptr);
            self.val(expected, k);
            self.val(new, k);
            self.atomic(Atomic::Cmpxchg, k);
            self.set(old);
            self.store_local(dst, k, old);
            return;
        }
        let p = self.tmp(I32);
        self.addr(ptr);
        self.a.local_tee(p);
        self.plain_load(k);
        self.set(old);
        self.get(old);
        self.val(expected, k);
        self.cmp(Cc::Eq, k, false);
        self.a.if_(EMPTY);
        self.get(p);
        self.val(new, k);
        self.plain_store(k);
        self.a.end();
        self.store_local(dst, k, old);
    }
}
