//! Operand values: reads, constants, stores and conversions.

#[allow(clippy::wildcard_imports)]
use crate::lower::*;

impl FnLower<'_, '_> {
    /// The value of type `ty` in `m`.
    pub(crate) fn val_of(&self, m: Mem, ty: &MirTy) -> Val {
        match cc::scalar_k(ty, self.c.t) {
            Some(k) => Val::S(V::M(m), k),
            None if *ty == MirTy::Unit => Val::Unit,
            None => Val::M(m),
        }
    }

    pub(crate) const fn imm(v: i64, k: K) -> Val {
        Val::S(V::I(v), k)
    }

    /// A scalar `V` held in memory (`Ind` needs its pointer in a slot).
    pub(crate) fn in_slot(&mut self, v: V, k: K) -> Slot {
        if let V::M(Mem::Frame(s, 0)) = v {
            return s;
        }
        let m = self.tmp_k(k);
        self.push(Inst::Mov { k, dst: m, src: v });
        let Mem::Frame(s, _) = m else { unreachable!() };
        s
    }
}

impl FnLower<'_, '_> {
    /// `op` read at type `ty`.
    pub(crate) fn val(&mut self, op: &Operand, ty: &MirTy) -> Val {
        match op {
            Operand::Copy(id) => {
                let from = self.local_ty(*id);
                let Some(m) = self.local_mem(*id) else {
                    return if *ty == MirTy::Unit {
                        Val::Unit
                    } else {
                        self.zero_val(ty)
                    };
                };
                if let (MirTy::Prim(a), MirTy::Prim(b)) = (&from, ty) {
                    if prim_class(*a, self.pw()) != prim_class(*b, self.pw()) {
                        return self.cast(Val::S(V::M(m), cc::prim_k(*a, self.pw())), &from, ty);
                    }
                }
                match cc::scalar_k(&from, self.c.t) {
                    Some(k) => Val::S(V::M(m), k),
                    None => Val::M(m),
                }
            }
            Operand::Const(c) => self.const_val(c, ty),
        }
    }

    /// Zero the bytes of `size` at `d` no field of `tys` at `offs` covers,
    /// in scalar stores.
    pub(crate) fn zero_gaps(&mut self, d: Mem, size: u32, tys: &[MirTy], offs: &[u32]) {
        let t = self.c.t;
        let mut cov: Vec<(u32, u32)> = tys
            .iter()
            .zip(offs)
            .map(|(ty, &o)| (o, o + t.size_align(ty).0))
            .collect();
        cov.sort_unstable();
        let mut at = 0;
        for (a, b) in cov.into_iter().chain([(size, size)]) {
            while at < a.min(size) {
                let k = [K::I64, K::I32, K::I16, K::I8]
                    .into_iter()
                    .find(|k| at % k.size() == 0 && at + k.size() <= a)
                    .unwrap_or(K::I8);
                self.push(Inst::Mov {
                    k,
                    dst: d.at(at),
                    src: V::I(0),
                });
                at += k.size();
            }
            at = at.max(b);
        }
    }

    pub(crate) fn zero_val(&mut self, ty: &MirTy) -> Val {
        if *ty == MirTy::Unit {
            return Val::Unit;
        }
        if let Some(k) = cc::scalar_k(ty, self.c.t) {
            return Val::S(V::I(0), k);
        }
        let m = self.tmp(ty);
        let size = self.c.t.size_align(ty).0;
        self.push(Inst::Zero { dst: m, size });
        Val::M(m)
    }

    /// A constant at type `ty`.
    pub(crate) fn const_val(&mut self, c: &ConstVal, ty: &MirTy) -> Val {
        let pw = self.pw();
        match c {
            ConstVal::Int { ty: p, .. } | ConstVal::UInt { ty: p, .. } => {
                let (bits, as_f64) = match c {
                    ConstVal::Int { value, .. } => (*value as i64, *value as f64),
                    ConstVal::UInt { value, .. } => (*value as i64, *value as f64),
                    _ => unreachable!(),
                };
                match ty {
                    MirTy::Prim(q) if q.is_float() => {
                        let b = if *q == PrimTy::F32 {
                            i64::from((as_f64 as f32).to_bits())
                        } else {
                            as_f64.to_bits() as i64
                        };
                        Val::S(V::I(b), cc::prim_k(*q, pw))
                    }
                    MirTy::Prim(q) if q.is_int() => Val::S(V::I(bits), cc::prim_k(*q, pw)),
                    MirTy::Any | MirTy::GcRef => Val::S(V::I(bits), self.word()),
                    _ => Val::S(V::I(bits), cc::prim_k(*p, pw)),
                }
            }
            ConstVal::Bool(b) => Val::S(V::I(i64::from(*b)), K::I8),
            ConstVal::Char(ch) => {
                let k = cc::scalar_k(ty, self.c.t).unwrap_or(K::I32);
                Val::S(V::I(i64::from(*ch)), k)
            }
            ConstVal::Float { bits, ty: p } => {
                let (q, b) = match ty {
                    MirTy::Prim(q) if q.is_float() && q != p => {
                        let v = if *p == PrimTy::F32 {
                            f64::from(f32::from_bits(*bits as u32))
                        } else {
                            f64::from_bits(*bits)
                        };
                        let b = if *q == PrimTy::F32 {
                            u64::from((v as f32).to_bits())
                        } else {
                            v.to_bits()
                        };
                        (*q, b)
                    }
                    _ => (*p, *bits),
                };
                Val::S(V::I(b as i64), cc::prim_k(q, pw))
            }
            ConstVal::Str(s) => {
                let m = self.tmp(&MirTy::Prim(PrimTy::Str));
                self.str_lit(s, m);
                Val::M(m)
            }
            ConstVal::Unit | ConstVal::None => self.zero_val(ty),
            ConstVal::Bytes(_) => unreachable!("MIR lowering expands byte arrays"),
        }
    }

    /// Write `v` (of type `ty`) to `dst`.
    pub(crate) fn store(&mut self, v: Val, ty: &MirTy, dst: Mem) {
        match v {
            Val::S(src, k) => self.push(Inst::Mov { k, dst, src }),
            Val::M(src) => {
                if src != dst {
                    let size = self.c.t.size_align(ty).0;
                    if size > 0 && !(self.c.opt && self.simd_copy(ty, dst, src)) {
                        self.push(Inst::Copy { dst, src, size });
                    }
                }
            }
            Val::Unit => {}
        }
    }

    /// `v` in memory.
    pub(crate) fn mem_of(&mut self, v: Val, ty: &MirTy) -> Mem {
        match v {
            Val::M(m) | Val::S(V::M(m), _) => m,
            Val::S(src, k) => {
                let m = self.tmp_k(k);
                self.push(Inst::Mov { k, dst: m, src });
                m
            }
            Val::Unit => self.tmp(ty),
        }
    }

    pub(crate) fn scalar(&mut self, op: &Operand, ty: &MirTy) -> (V, K) {
        match self.val(op, ty) {
            Val::S(v, k) => (v, k),
            Val::M(m) => (
                V::M(m),
                cc::scalar_k(ty, self.c.t).unwrap_or_else(|| self.word()),
            ),
            Val::Unit => (V::I(0), K::I8),
        }
    }

    pub(crate) fn op_ty(&self, op: &Operand) -> MirTy {
        match op {
            Operand::Copy(id) => self.local_ty(*id),
            Operand::Const(c) => const_ty(c),
        }
    }

    /// A value conversion.
    pub(crate) fn cast(&mut self, v: Val, from: &MirTy, to: &MirTy) -> Val {
        let pw = self.pw();
        if let (MirTy::Fn(..) | MirTy::GcRef, MirTy::Prim(_)) = (from, to) {
            return self.cast(v, &MirTy::Prim(PrimTy::Usize), to);
        }
        let (MirTy::Prim(fp), MirTy::Prim(tp)) = (from, to) else {
            if cc::scalar_k(from, self.c.t) == cc::scalar_k(to, self.c.t) {
                return v;
            }
            self.err(format!("cast from `{from:?}` to `{to:?}`"));
            return self.zero_val(to);
        };
        if prim_class(*fp, pw) == prim_class(*tp, pw) {
            return v;
        }
        let Val::S(a, fk) = v else {
            self.err(format!("cast from `{fp:?}` to `{tp:?}`"));
            return self.zero_val(to);
        };
        let tk = cc::prim_k(*tp, pw);
        let int_like = |p: &PrimTy| p.is_int() || *p == PrimTy::Bool;
        let conv = match (int_like(fp), int_like(tp), fp.is_float(), tp.is_float()) {
            (true, true, _, _) => {
                let w = |p: &PrimTy| {
                    if *p == PrimTy::Bool {
                        1
                    } else {
                        p.bit_width(pw)
                    }
                };
                if *tp == PrimTy::Bool && w(fp) > 1 {
                    Conv::ToBool
                } else {
                    Conv::Int {
                        signed: *fp != PrimTy::Bool && fp.is_signed_int(),
                    }
                }
            }
            (false, true, true, false) => Conv::FToI {
                signed: tp.is_signed_int(),
                bits: if *tp == PrimTy::Bool {
                    1
                } else {
                    tp.bit_width(pw)
                },
            },
            (true, false, false, true) => Conv::IToF {
                signed: fp.is_signed_int(),
            },
            (false, false, true, true) => Conv::FToF,
            _ => {
                self.err(format!("cast from `{fp:?}` to `{tp:?}`"));
                return self.zero_val(to);
            }
        };
        let dst = self.tmp_k(tk);
        self.push(Inst::Conv {
            conv,
            from: fk,
            to: tk,
            dst,
            a,
        });
        Val::S(V::M(dst), tk)
    }

    /// Bit-preserving move into `dst`.
    pub(crate) fn bitcast_into(&mut self, v: Val, from: &MirTy, to: &MirTy, dst: Mem) {
        let t = self.c.t;
        if *from == MirTy::Unit || *to == MirTy::Unit {
            let size = t.size_align(to).0;
            if size > 0 {
                self.push(Inst::Zero { dst, size });
            }
            return;
        }
        if let (
            MirTy::Prim(
                fp @ (PrimTy::I8
                | PrimTy::I16
                | PrimTy::I32
                | PrimTy::I64
                | PrimTy::Isize
                | PrimTy::U8
                | PrimTy::U16
                | PrimTy::U32
                | PrimTy::U64
                | PrimTy::Usize
                | PrimTy::F32
                | PrimTy::F64
                | PrimTy::Bool
                | PrimTy::CChar
                | PrimTy::Char),
            ),
            MirTy::Prim(
                tp @ (PrimTy::I8
                | PrimTy::I16
                | PrimTy::I32
                | PrimTy::I64
                | PrimTy::Isize
                | PrimTy::U8
                | PrimTy::U16
                | PrimTy::U32
                | PrimTy::U64
                | PrimTy::Usize
                | PrimTy::F32
                | PrimTy::F64
                | PrimTy::Bool
                | PrimTy::CChar
                | PrimTy::Char),
            ),
            Val::S(a, fk),
        ) = (from, to, v)
        {
            let _ = fp;
            let pw = self.pw();
            let tk = cc::prim_k(*tp, pw);
            let ik = |k: K| match k {
                K::F32 => K::I32,
                K::F64 => K::I64,
                k => k,
            };
            let mut cur = if fk.is_float() {
                let m = self.tmp_k(ik(fk));
                self.push(Inst::Conv {
                    conv: Conv::Bits,
                    from: fk,
                    to: ik(fk),
                    dst: m,
                    a,
                });
                (V::M(m), ik(fk))
            } else {
                (a, fk)
            };
            if *tp == PrimTy::Bool {
                // `trunci` to `i1`: bit 0.
                if cur.1 == K::I8 {
                    self.push(Inst::Bin {
                        op: Bin::And,
                        k: K::I8,
                        signed: false,
                        dst,
                        a: cur.0,
                        b: V::I(1),
                    });
                } else {
                    let m = self.tmp_k(cur.1);
                    self.push(Inst::Bin {
                        op: Bin::And,
                        k: cur.1,
                        signed: false,
                        dst: m,
                        a: cur.0,
                        b: V::I(1),
                    });
                    self.push(Inst::Conv {
                        conv: Conv::Int { signed: false },
                        from: cur.1,
                        to: K::I8,
                        dst,
                        a: V::M(m),
                    });
                }
                return;
            }
            if ik(tk) != cur.1 {
                let m = self.tmp_k(ik(tk));
                self.push(Inst::Conv {
                    conv: Conv::Int { signed: false },
                    from: cur.1,
                    to: ik(tk),
                    dst: m,
                    a: cur.0,
                });
                cur = (V::M(m), ik(tk));
            }
            if tk.is_float() {
                self.push(Inst::Conv {
                    conv: Conv::Bits,
                    from: cur.1,
                    to: tk,
                    dst,
                    a: cur.0,
                });
            } else {
                self.push(Inst::Mov {
                    k: tk,
                    dst,
                    src: cur.0,
                });
            }
            return;
        }
        // Through memory: the common bytes, the rest zeroed.
        let src = self.mem_of(v, from);
        let (fs, ts) = (t.size_align(from).0, t.size_align(to).0);
        let n = fs.min(ts);
        if n > 0 && src != dst {
            self.push(Inst::Copy { dst, src, size: n });
        }
        if ts > n {
            self.push(Inst::Zero {
                dst: dst.at(n),
                size: ts - n,
            });
        }
    }
}
