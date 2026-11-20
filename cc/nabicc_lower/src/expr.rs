//! Expressions: values, places and their loads and stores.

use crate::R;
use crate::body::{Fb, Var};
use nabic_ir_common::{Abi, ConstVal, PrimOp, PrimTy};
use nabic_mir::{MirTy, Operand, RValue, Statement, Terminator};
use nabicc_sema::{Arith, Bits, Builtin, Cmp, TExpr, TK};

const W: MirTy = MirTy::Prim(PrimTy::Usize);

/// Constant `v` of primitive `p`.
pub(crate) fn cval(v: i128, p: PrimTy) -> ConstVal {
    match p {
        PrimTy::Bool => ConstVal::Bool(v != 0),
        PrimTy::F32 => ConstVal::Float {
            bits: u64::from((v as f32).to_bits()),
            ty: p,
        },
        PrimTy::F64 => ConstVal::Float {
            bits: (v as f64).to_bits(),
            ty: p,
        },
        _ if p.is_signed_int() => ConstVal::Int { value: v, ty: p },
        _ => {
            let bits = match p {
                PrimTy::U8 | PrimTy::CChar => 8,
                PrimTy::U16 => 16,
                PrimTy::U32 | PrimTy::Char => 32,
                _ => 64,
            };
            let m = if bits == 64 {
                u128::from(u64::MAX)
            } else {
                (1u128 << bits) - 1
            };
            ConstVal::UInt {
                value: (v as u128) & m,
                ty: p,
            }
        }
    }
}

fn fval(v: f64, p: PrimTy) -> ConstVal {
    ConstVal::Float {
        bits: if p == PrimTy::F32 {
            u64::from((v as f32).to_bits())
        } else {
            v.to_bits()
        },
        ty: p,
    }
}

/// Where an lvalue is.
#[derive(Clone, Debug)]
pub(crate) enum Place {
    Reg(nabic_ir_common::LocalId),
    Mem(Operand, Option<Bits>),
}

const fn prim(t: &MirTy) -> PrimTy {
    match t {
        MirTy::Prim(p) => *p,
        _ => PrimTy::Usize,
    }
}

const fn arith_op(a: Arith) -> PrimOp {
    match a {
        Arith::Add => PrimOp::WrappingAdd,
        Arith::Sub => PrimOp::WrappingSub,
        Arith::Mul => PrimOp::WrappingMul,
        Arith::Div => PrimOp::Div,
        Arith::Rem => PrimOp::Rem,
        Arith::Shl => PrimOp::Shl,
        Arith::Shr => PrimOp::Shr,
        Arith::And => PrimOp::BitAnd,
        Arith::Or => PrimOp::BitOr,
        Arith::Xor => PrimOp::BitXor,
    }
}

impl Fb<'_, '_> {
    fn ty(&mut self, e: &TExpr) -> R<MirTy> {
        self.lx.mir_ty(e.ty, e.loc)
    }

    /// `e` for its effects only.
    pub(crate) fn effect(&mut self, e: &TExpr) -> R<()> {
        match &e.kind {
            TK::Conv(x) if self.lx.u.types.is_void(e.ty) => self.effect(x),
            TK::Comma(a, b) => {
                self.effect(a)?;
                self.effect(b)
            }
            _ if matches!(self.ty(e)?, MirTy::Prim(_) | MirTy::Unit) || !e.is_lvalue() => {
                if matches!(self.ty(e)?, MirTy::Prim(_) | MirTy::Unit) {
                    self.val(e)?;
                } else {
                    self.agg_addr(e)?;
                }
                Ok(())
            }
            _ => {
                self.addr(e)?;
                Ok(())
            }
        }
    }

    /// A function result or argument: a scalar's value, an aggregate's
    /// value in a local of its MIR type.
    pub(crate) fn value(&mut self, e: &TExpr) -> R<Operand> {
        let ty = self.ty(e)?;
        if matches!(ty, MirTy::Prim(_) | MirTy::Unit) {
            return self.val(e);
        }
        let a = self.agg_addr(e)?;
        Ok(self.tmp(
            ty.clone(),
            RValue::RawLoad {
                addr: a,
                ty,
                volatile: false,
            },
        ))
    }

    /// The address of an aggregate value (an lvalue's, or a temporary's).
    pub(crate) fn agg_addr(&mut self, e: &TExpr) -> R<Operand> {
        if e.is_lvalue() {
            return self.addr(e);
        }
        let ty = self.ty(e)?;
        match &e.kind {
            TK::Comma(a, b) => {
                self.effect(a)?;
                self.agg_addr(b)
            }
            TK::Assign(p, v) => {
                let dst = self.addr(p)?;
                let src = self.agg_addr(v)?;
                let n = self.lx.size(e.ty);
                self.copy(dst.clone(), src, n, &ty);
                Ok(dst)
            }
            TK::Cond(c, a, b) => {
                let r = self.local(W, None);
                let (t, f, end) = (self.block(), self.block(), self.block());
                self.cond_branch(c, t, f)?;
                self.cur = Some(self.idx(t));
                let x = self.agg_addr(a)?;
                self.push(Statement::Assign {
                    target: r,
                    rvalue: RValue::Use(x),
                });
                self.goto(end);
                self.cur = Some(self.idx(f));
                let y = self.agg_addr(b)?;
                self.push(Statement::Assign {
                    target: r,
                    rvalue: RValue::Use(y),
                });
                self.goto(end);
                self.cur = Some(self.idx(end));
                Ok(Operand::Copy(r))
            }
            TK::StmtExpr(ss, last) => {
                for s in ss {
                    self.stmt(s)?;
                }
                match last {
                    Some(l) => self.agg_addr(l),
                    None => Err(self.lx.err(e.loc, "a statement expression without a value")),
                }
            }
            TK::VaArg(ap) => self.va_arg(ap, e.ty, &ty, e.loc),
            _ => {
                // A call (or another rvalue): its value into a slot.
                let v = self.val(e)?;
                let l = self.local(ty.clone(), None);
                self.push(Statement::Assign {
                    target: l,
                    rvalue: RValue::Use(v),
                });
                Ok(self.tmp(W, RValue::AddrOf(l)))
            }
        }
    }

    fn var(&self, l: u32) -> Var {
        self.vars[l as usize]
    }

    pub(crate) fn idx(&self, b: nabic_ir_common::BlockId) -> usize {
        self.f
            .blocks
            .iter()
            .position(|x| x.id == b)
            .expect("a block")
    }

    /// The address of lvalue `e`.
    pub(crate) fn addr(&mut self, e: &TExpr) -> R<Operand> {
        match self.place(e)? {
            Place::Mem(a, None) => Ok(a),
            Place::Mem(_, Some(_)) => Err(self.lx.err(e.loc, "the address of a bitfield")),
            Place::Reg(_) => Err(self.lx.err(e.loc, "the address of a register variable")),
        }
    }

    pub(crate) fn place(&mut self, e: &TExpr) -> R<Place> {
        Ok(match &e.kind {
            TK::Local(l) => match self.var(*l) {
                Var::Reg(r) => Place::Reg(r),
                Var::Mem(m) => Place::Mem(self.tmp(W, RValue::AddrOf(m)), None),
            },
            TK::Global(g) => {
                let name = self.lx.u.globals[*g].symbol.clone();
                Place::Mem(self.tmp(W, RValue::StaticAddr(name)), None)
            }
            TK::Deref(p) => Place::Mem(self.val(p)?, None),
            TK::Member(b, off, bits) => {
                let a = self.agg_addr(b)?;
                Place::Mem(self.offset(a, *off), *bits)
            }
            TK::Compound(obj, init) => {
                let a = self.addr(obj)?;
                let n = self.lx.size(obj.ty);
                self.init(a.clone(), n, init)?;
                Place::Mem(a, None)
            }
            _ => return Err(self.lx.err(e.loc, "expected an lvalue")),
        })
    }

    pub(crate) fn load(&mut self, p: &Place, ty: &MirTy) -> Operand {
        match p {
            Place::Reg(r) => Operand::Copy(*r),
            Place::Mem(a, None) => self.tmp(
                ty.clone(),
                RValue::RawLoad {
                    addr: a.clone(),
                    ty: ty.clone(),
                    volatile: false,
                },
            ),
            Place::Mem(a, Some(b)) => self.load_bits(a.clone(), *b, ty),
        }
    }

    pub(crate) fn store(&mut self, p: &Place, v: Operand, ty: &MirTy) {
        match p {
            Place::Reg(r) => self.push(Statement::Assign {
                target: *r,
                rvalue: RValue::Use(v),
            }),
            Place::Mem(a, None) => self.push(Statement::RawStore {
                addr: a.clone(),
                value: v,
                ty: ty.clone(),
                volatile: false,
            }),
            Place::Mem(a, Some(b)) => self.store_bits(a.clone(), *b, v, ty),
        }
    }

    const fn unit_ty(b: Bits) -> PrimTy {
        match b.unit {
            1 => PrimTy::U8,
            2 => PrimTy::U16,
            4 => PrimTy::U32,
            _ => PrimTy::U64,
        }
    }

    fn bin(&mut self, op: PrimOp, p: PrimTy, a: Operand, b: Operand) -> Operand {
        let rt = if matches!(
            op,
            PrimOp::Eq | PrimOp::Ne | PrimOp::Lt | PrimOp::Le | PrimOp::Gt | PrimOp::Ge
        ) {
            MirTy::Prim(PrimTy::Bool)
        } else {
            MirTy::Prim(p)
        };
        self.tmp(
            rt,
            RValue::BinOp {
                op,
                op_ty: MirTy::Prim(p),
                lhs: a,
                rhs: b,
            },
        )
    }

    fn k(v: i128, p: PrimTy) -> Operand {
        Operand::Const(cval(v, p))
    }

    fn load_bits(&mut self, a: Operand, b: Bits, ty: &MirTy) -> Operand {
        let u = Self::unit_ty(b);
        let ubits = (b.unit * 8) as i128;
        let w = self.tmp(
            MirTy::Prim(u),
            RValue::RawLoad {
                addr: a,
                ty: MirTy::Prim(u),
                volatile: false,
            },
        );
        let v = if b.signed {
            // Sign-extend: up to the top, then back down arithmetically.
            let s = match u {
                PrimTy::U8 => PrimTy::I8,
                PrimTy::U16 => PrimTy::I16,
                PrimTy::U32 => PrimTy::I32,
                _ => PrimTy::I64,
            };
            let up = ubits - i128::from(b.at) - i128::from(b.width);
            let x = self.bin(PrimOp::Shl, u, w, Self::k(up, u));
            let x = self.cast(x, &MirTy::Prim(u), &MirTy::Prim(s));
            let x = self.bin(PrimOp::Shr, s, x, Self::k(ubits - i128::from(b.width), s));
            return self.cast(x, &MirTy::Prim(s), ty);
        } else {
            let x = self.bin(PrimOp::Shr, u, w, Self::k(i128::from(b.at), u));
            let mask = if b.width >= 64 {
                -1
            } else {
                (1i128 << b.width) - 1
            };
            self.bin(PrimOp::BitAnd, u, x, Self::k(mask, u))
        };
        self.cast(v, &MirTy::Prim(u), ty)
    }

    pub(crate) fn store_bits(&mut self, a: Operand, b: Bits, v: Operand, ty: &MirTy) {
        let u = Self::unit_ty(b);
        let ut = MirTy::Prim(u);
        let mask = if b.width >= 64 {
            -1i128
        } else {
            (1i128 << b.width) - 1
        };
        let old = self.tmp(
            ut.clone(),
            RValue::RawLoad {
                addr: a.clone(),
                ty: ut.clone(),
                volatile: false,
            },
        );
        let keep = !(mask << b.at);
        let old = self.bin(PrimOp::BitAnd, u, old, Self::k(keep, u));
        let nv = self.cast(v, ty, &ut);
        let nv = self.bin(PrimOp::BitAnd, u, nv, Self::k(mask, u));
        let nv = self.bin(PrimOp::Shl, u, nv, Self::k(i128::from(b.at), u));
        let x = self.bin(PrimOp::BitOr, u, old, nv);
        self.push(Statement::RawStore {
            addr: a,
            value: x,
            ty: ut,
            volatile: false,
        });
    }

    /// Copy `n` bytes of a `ty` from `src` to `dst`.
    pub(crate) fn copy(&mut self, dst: Operand, src: Operand, n: u64, ty: &MirTy) {
        if n == 0 {
            return;
        }
        let v = self.tmp(
            ty.clone(),
            RValue::RawLoad {
                addr: src,
                ty: ty.clone(),
                volatile: false,
            },
        );
        self.push(Statement::RawStore {
            addr: dst,
            value: v,
            ty: ty.clone(),
            volatile: false,
        });
    }

    /// Value `v` of `from` as a `to`.
    pub(crate) fn cast(&mut self, v: Operand, from: &MirTy, to: &MirTy) -> Operand {
        if from == to {
            return v;
        }
        if *to == MirTy::Prim(PrimTy::Bool) {
            let p = prim(from);
            let z = if p.is_float() {
                Operand::Const(fval(0.0, p))
            } else {
                Self::k(0, p)
            };
            return self.bin(PrimOp::Ne, p, v, z);
        }
        if let (Operand::Const(c), MirTy::Prim(tp)) = (&v, to) {
            let n = match c {
                ConstVal::Int { value, .. } => Some(*value),
                ConstVal::UInt { value, .. } => Some(*value as i128),
                ConstVal::Bool(b) => Some(i128::from(*b)),
                _ => None,
            };
            if let (Some(n), true) = (n, tp.is_int()) {
                let bits = tp.bit_width(self.lx.t.pointer_width);
                let m = if bits >= 128 {
                    n
                } else {
                    n & ((1i128 << bits) - 1)
                };
                let m = if tp.is_signed_int() && bits < 128 && (m >> (bits - 1)) & 1 == 1 {
                    m - (1i128 << bits)
                } else {
                    m
                };
                return Self::k(m, *tp);
            }
        }
        self.tmp(
            to.clone(),
            RValue::Cast {
                source: v,
                from_ty: from.clone(),
                to_ty: to.clone(),
            },
        )
    }

    /// A condition: jump to `t` if `e` is non-zero, else to `f`.
    pub(crate) fn cond_branch(
        &mut self,
        e: &TExpr,
        t: nabic_ir_common::BlockId,
        f: nabic_ir_common::BlockId,
    ) -> R<()> {
        match &e.kind {
            TK::And(a, b) => {
                let mid = self.block();
                self.cond_branch(a, mid, f)?;
                self.cur = Some(self.idx(mid));
                self.cond_branch(b, t, f)
            }
            TK::Or(a, b) => {
                let mid = self.block();
                self.cond_branch(a, t, mid)?;
                self.cur = Some(self.idx(mid));
                self.cond_branch(b, t, f)
            }
            TK::Not(a) => self.cond_branch(a, f, t),
            TK::Int(v) => {
                self.goto(if *v != 0 { t } else { f });
                Ok(())
            }
            _ => {
                let c = self.truth(e)?;
                self.branch(c, t, f);
                Ok(())
            }
        }
    }

    /// `e != 0` as a `bool`.
    fn truth(&mut self, e: &TExpr) -> R<Operand> {
        if let TK::Cmp(c, a, b) = &e.kind {
            return self.compare(*c, a, b);
        }
        let ty = self.ty(e)?;
        let v = self.val(e)?;
        Ok(self.cast(v, &ty, &MirTy::Prim(PrimTy::Bool)))
    }

    fn compare(&mut self, c: Cmp, a: &TExpr, b: &TExpr) -> R<Operand> {
        let ty = self.ty(a)?;
        let x = self.val(a)?;
        let y = self.val(b)?;
        let op = match c {
            Cmp::Eq => PrimOp::Eq,
            Cmp::Ne => PrimOp::Ne,
            Cmp::Lt => PrimOp::Lt,
            Cmp::Le => PrimOp::Le,
            Cmp::Gt => PrimOp::Gt,
            Cmp::Ge => PrimOp::Ge,
        };
        Ok(self.bin(op, prim(&ty), x, y))
    }

    /// A `bool` as the C `int` it is.
    fn bool_int(&mut self, b: Operand, ty: &MirTy) -> Operand {
        self.cast(b, &MirTy::Prim(PrimTy::Bool), ty)
    }

    /// The value of scalar expression `e` (an aggregate: in a local of
    /// its type).
    pub(crate) fn val(&mut self, e: &TExpr) -> R<Operand> {
        let ty = self.ty(e)?;
        Ok(match &e.kind {
            TK::Int(v) => match ty {
                MirTy::Prim(p) if p.is_float() => Operand::Const(fval(*v as f64, p)),
                MirTy::Prim(p) => Self::k(*v, p),
                MirTy::Unit => Operand::Const(ConstVal::Unit),
                _ => Self::k(*v, PrimTy::Usize),
            },
            TK::Float(v) => Operand::Const(fval(*v, prim(&ty))),
            TK::Local(_) | TK::Global(_) | TK::Deref(_) | TK::Member(..) | TK::Compound(..) => {
                if !matches!(ty, MirTy::Prim(_)) {
                    return self.value(e);
                }
                let p = self.place(e)?;
                self.load(&p, &ty)
            }
            TK::Func(f) => {
                let id = self.lx.fn_ids[f];
                self.tmp(
                    W,
                    RValue::FnAddr {
                        callee: id,
                        fn_ty: MirTy::Fn(Vec::new(), Box::new(MirTy::Unit)),
                    },
                )
            }
            TK::AddrOf(x) => {
                if let TK::Func(f) = x.kind {
                    let id = self.lx.fn_ids[&f];
                    return Ok(self.tmp(
                        W,
                        RValue::FnAddr {
                            callee: id,
                            fn_ty: MirTy::Fn(Vec::new(), Box::new(MirTy::Unit)),
                        },
                    ));
                }
                self.addr(x)?
            }
            TK::Decay(x) => self.agg_addr(x)?,
            TK::Conv(x) => {
                if ty == MirTy::Unit {
                    self.effect(x)?;
                    return Ok(Operand::Const(ConstVal::Unit));
                }
                let from = self.ty(x)?;
                let v = self.val(x)?;
                self.cast(v, &from, &ty)
            }
            TK::Neg(x) => {
                let v = self.val(x)?;
                let p = prim(&ty);
                if p.is_float() {
                    self.tmp(
                        ty.clone(),
                        RValue::UnaryOp {
                            op: PrimOp::Neg,
                            op_ty: ty,
                            operand: v,
                        },
                    )
                } else {
                    self.bin(PrimOp::WrappingSub, p, Self::k(0, p), v)
                }
            }
            TK::BitNot(x) => {
                let v = self.val(x)?;
                self.tmp(
                    ty.clone(),
                    RValue::UnaryOp {
                        op: PrimOp::Not,
                        op_ty: ty,
                        operand: v,
                    },
                )
            }
            TK::Not(x) => {
                let b = self.truth(x)?;
                let n = self.tmp(
                    MirTy::Prim(PrimTy::Bool),
                    RValue::UnaryOp {
                        op: PrimOp::LogicalNot,
                        op_ty: MirTy::Prim(PrimTy::Bool),
                        operand: b,
                    },
                );
                self.bool_int(n, &ty)
            }
            TK::Bin(op, a, b) => {
                let x = self.val(a)?;
                let mut y = self.val(b)?;
                if matches!(op, Arith::Shl | Arith::Shr) {
                    let bt = self.ty(b)?;
                    y = self.cast(y, &bt, &ty);
                }
                self.bin(arith_op(*op), prim(&ty), x, y)
            }
            TK::Cmp(c, a, b) => {
                let r = self.compare(*c, a, b)?;
                self.bool_int(r, &ty)
            }
            TK::PtrAdd(p, i, step) => {
                let pv = self.val(p)?;
                let it = self.ty(i)?;
                let iv = self.val(i)?;
                let off = self.scale(iv, &it, *step);
                self.bin(PrimOp::WrappingAdd, PrimTy::Usize, pv, off)
            }
            TK::PtrDiff(a, b, step) => {
                let x = self.val(a)?;
                let y = self.val(b)?;
                let d = self.bin(PrimOp::WrappingSub, PrimTy::Usize, x, y);
                let p = prim(&ty);
                let d = self.cast(d, &W, &ty);
                if *step == 1 {
                    d
                } else {
                    self.bin(PrimOp::Div, p, d, Self::k(i128::from(*step), p))
                }
            }
            TK::And(..) | TK::Or(..) => {
                let r = self.local(MirTy::Prim(PrimTy::Bool), None);
                let (t, f, end) = (self.block(), self.block(), self.block());
                self.cond_branch(e, t, f)?;
                for (b, v) in [(t, true), (f, false)] {
                    self.cur = Some(self.idx(b));
                    self.push(Statement::Assign {
                        target: r,
                        rvalue: RValue::Use(Operand::Const(ConstVal::Bool(v))),
                    });
                    self.goto(end);
                }
                self.cur = Some(self.idx(end));
                self.bool_int(Operand::Copy(r), &ty)
            }
            TK::Cond(c, a, b) => {
                if !matches!(ty, MirTy::Prim(_)) && ty != MirTy::Unit {
                    return self.value(e);
                }
                let r = self.local(ty.clone(), None);
                let (t, f, end) = (self.block(), self.block(), self.block());
                self.cond_branch(c, t, f)?;
                for (bb, x) in [(t, a), (f, b)] {
                    self.cur = Some(self.idx(bb));
                    if ty == MirTy::Unit {
                        self.effect(x)?;
                    } else {
                        let v = self.val(x)?;
                        self.push(Statement::Assign {
                            target: r,
                            rvalue: RValue::Use(v),
                        });
                    }
                    self.goto(end);
                }
                self.cur = Some(self.idx(end));
                Operand::Copy(r)
            }
            TK::Comma(a, b) => {
                self.effect(a)?;
                self.val(b)?
            }
            TK::Assign(p, v) => {
                if !matches!(ty, MirTy::Prim(_)) {
                    let a = self.agg_addr(e)?;
                    return Ok(self.tmp(
                        ty.clone(),
                        RValue::RawLoad {
                            addr: a,
                            ty,
                            volatile: false,
                        },
                    ));
                }
                let x = self.val(v)?;
                let pl = self.place(p)?;
                // A value read twice must not change between: keep it.
                let x = match x {
                    Operand::Copy(l) if matches!(pl, Place::Reg(r) if r == l) => x,
                    _ => x,
                };
                self.store(&pl, x.clone(), &ty);
                x
            }
            TK::OpAssign(op, p, v, op_ty) => {
                let ot = self.lx.mir_ty(*op_ty, e.loc)?;
                let pl = self.place(p)?;
                let old = self.load(&pl, &ty);
                let old = self.cast(old, &ty, &ot);
                let mut y = self.val(v)?;
                if matches!(op, Arith::Shl | Arith::Shr) {
                    let vt = self.ty(v)?;
                    y = self.cast(y, &vt, &ot);
                }
                let r = self.bin(arith_op(*op), prim(&ot), old, y);
                let r = self.cast(r, &ot, &ty);
                self.store(&pl, r.clone(), &ty);
                r
            }
            TK::PtrAssign(p, i, step, sub) => {
                let pl = self.place(p)?;
                let old = self.load(&pl, &ty);
                let it = self.ty(i)?;
                let iv = self.val(i)?;
                let off = self.scale(iv, &it, *step);
                let op = if *sub {
                    PrimOp::WrappingSub
                } else {
                    PrimOp::WrappingAdd
                };
                let r = self.bin(op, PrimTy::Usize, old, off);
                self.store(&pl, r.clone(), &ty);
                r
            }
            TK::IncDec(p, pre, inc, step) => {
                let pl = self.place(p)?;
                let old = self.load(&pl, &ty);
                // The old value kept: a store to a register place changes it.
                let old = match old {
                    Operand::Copy(_) => self.tmp(ty.clone(), RValue::Use(old)),
                    c @ Operand::Const(_) => c,
                };
                let pt = prim(&ty);
                let one = if pt.is_float() {
                    Operand::Const(fval(1.0, pt))
                } else if pt == PrimTy::Bool {
                    Operand::Const(ConstVal::Bool(true))
                } else {
                    Self::k(i128::from(*step), pt)
                };
                let r = if pt == PrimTy::Bool {
                    // `++` on a `_Bool` sets it; `--` flips it.
                    if *inc {
                        one
                    } else {
                        self.tmp(
                            ty.clone(),
                            RValue::UnaryOp {
                                op: PrimOp::LogicalNot,
                                op_ty: ty.clone(),
                                operand: old.clone(),
                            },
                        )
                    }
                } else {
                    let op = match (inc, pt.is_float()) {
                        (true, true) => PrimOp::Add,
                        (false, true) => PrimOp::Sub,
                        (true, false) => PrimOp::WrappingAdd,
                        (false, false) => PrimOp::WrappingSub,
                    };
                    self.bin(op, pt, old.clone(), one)
                };
                self.store(&pl, r.clone(), &ty);
                if *pre { r } else { old }
            }
            TK::Call(f, args, fty) => self.call(f, args, *fty, &ty, e.loc)?,
            TK::StmtExpr(ss, last) => {
                for s in ss {
                    self.stmt(s)?;
                }
                match last {
                    Some(l) => self.val(l)?,
                    None => Operand::Const(ConstVal::Unit),
                }
            }
            TK::VaStart(ap) => {
                let a = self.val(ap)?;
                let f = self
                    .lx
                    .helper("__builtin_va_start", vec![W], MirTy::Unit, false);
                self.tmp(
                    MirTy::Unit,
                    RValue::Call {
                        callee: f,
                        args: vec![a],
                        ret_ty: MirTy::Unit,
                    },
                );
                Operand::Const(ConstVal::Unit)
            }
            TK::VaEnd(ap) => {
                self.val(ap)?;
                Operand::Const(ConstVal::Unit)
            }
            TK::VaCopy(d, s) => {
                let dv = self.val(d)?;
                let sv = self.val(s)?;
                let vt = self.va_list_ty();
                let n = u64::from(self.lx.t.size_align(&vt).0);
                self.copy(dv, sv, n, &vt);
                Operand::Const(ConstVal::Unit)
            }
            TK::VaArg(ap) => {
                let a = self.va_arg(ap, e.ty, &ty, e.loc)?;
                self.tmp(
                    ty.clone(),
                    RValue::RawLoad {
                        addr: a,
                        ty,
                        volatile: false,
                    },
                )
            }
            TK::Builtin(b, args) => self.builtin(*b, args, &ty, e.loc)?,
        })
    }

    /// Index `i` (a `ty`) times `step`, as an address offset.
    fn scale(&mut self, i: Operand, ty: &MirTy, step: u64) -> Operand {
        let iw = self.cast(i, ty, &MirTy::Prim(PrimTy::Isize));
        let iw = self.cast(iw, &MirTy::Prim(PrimTy::Isize), &W);
        if step == 1 {
            return iw;
        }
        self.bin(
            PrimOp::WrappingMul,
            PrimTy::Usize,
            iw,
            Self::k(i128::from(step), PrimTy::Usize),
        )
    }

    fn call(
        &mut self,
        f: &TExpr,
        args: &[TExpr],
        fty: nabicc_parse::ty::TypeId,
        ty: &MirTy,
        loc: nabicc_lex::Loc,
    ) -> R<Operand> {
        let Some(ft) = self.lx.func_type(fty) else {
            return Err(self.lx.err(loc, "a call of a non-function"));
        };
        let mut ops = Vec::new();
        let mut tys = Vec::new();
        for a in args {
            tys.push(self.ty(a)?);
            ops.push(self.value(a)?);
        }
        // Windows' `_setjmp` takes the frame to unwind to second (MSVC
        // passes it unseen): none, so `longjmp` restores the registers
        // without unwinding, which C frames need not.
        if let TK::Func(id) = &f.kind {
            let name = &self.lx.u.funcs[*id].symbol;
            if self.lx.t.os == "windows"
                && self.lx.t.arch != "x86"
                && name == "_setjmp"
                && ops.len() == 1
            {
                ops.push(Operand::Const(cval(0, PrimTy::Usize)));
            }
        }
        let rv = if let TK::Func(id) = &f.kind {
            RValue::Call {
                callee: self.lx.fn_ids[id],
                args: ops,
                ret_ty: ty.clone(),
            }
        } else {
            let c = self.val(f)?;
            RValue::CallIndirect {
                callee: c,
                args: ops,
                arg_tys: tys,
                ret_ty: ty.clone(),
                abi: Abi::C {
                    variadic: ft.variadic,
                    system: matches!(ft.cc, nabicc_parse::ty::CallConv::Stdcall),
                },
            }
        };
        Ok(self.tmp(ty.clone(), rv))
    }

    /// The target's `va_list` as a MIR type.
    pub(crate) fn va_list_ty(&mut self) -> MirTy {
        let t = &self.lx.u.types;
        let td = &t.typedefs[t.va_list as usize];
        let q = td.ty.ty;
        self.lx.mir_ty(q, nabicc_lex::Loc::default()).unwrap_or(W)
    }

    fn builtin(
        &mut self,
        b: Builtin,
        args: &[TExpr],
        ty: &MirTy,
        loc: nabicc_lex::Loc,
    ) -> R<Operand> {
        let un = |fb: &mut Self, op: PrimOp, a: &TExpr| -> R<Operand> {
            let at = fb.ty(a)?;
            let v = fb.val(a)?;
            let r = fb.tmp(
                at.clone(),
                RValue::UnaryOp {
                    op,
                    op_ty: at.clone(),
                    operand: v,
                },
            );
            Ok(fb.cast(r, &at, ty))
        };
        Ok(match b {
            Builtin::Clz => un(self, PrimOp::LeadingZeros, &args[0])?,
            Builtin::Ctz => un(self, PrimOp::TrailingZeros, &args[0])?,
            Builtin::Popcount => un(self, PrimOp::CountOnes, &args[0])?,
            Builtin::Bswap => un(self, PrimOp::SwapBytes, &args[0])?,
            Builtin::Unreachable | Builtin::Trap => {
                self.term(Terminator::Unreachable);
                Operand::Const(ConstVal::Unit)
            }
            Builtin::Fence => Operand::Const(ConstVal::Unit),
            Builtin::AddOverflow | Builtin::SubOverflow | Builtin::MulOverflow => {
                let at = self.ty(&args[0])?;
                let p = prim(&at);
                let x = self.val(&args[0])?;
                let y = self.val(&args[1])?;
                let dst = self.val(&args[2])?;
                let op = match b {
                    Builtin::AddOverflow => PrimOp::WrappingAdd,
                    Builtin::SubOverflow => PrimOp::WrappingSub,
                    _ => PrimOp::WrappingMul,
                };
                let r = self.bin(op, p, x.clone(), y.clone());
                self.push(Statement::RawStore {
                    addr: dst,
                    value: r.clone(),
                    ty: at,
                    volatile: false,
                });
                let o = self.overflowed(b, p, x, y, r);
                self.cast(o, &MirTy::Prim(PrimTy::Bool), ty)
            }
            Builtin::AtomicLoad => {
                let a = self.val(&args[0])?;
                self.tmp(
                    ty.clone(),
                    RValue::RawLoad {
                        addr: a,
                        ty: ty.clone(),
                        volatile: true,
                    },
                )
            }
            Builtin::AtomicStore => {
                let a = self.val(&args[0])?;
                let vt = self.ty(&args[1])?;
                let v = self.val(&args[1])?;
                self.push(Statement::RawStore {
                    addr: a,
                    value: v,
                    ty: vt,
                    volatile: true,
                });
                Operand::Const(ConstVal::Unit)
            }
            Builtin::AtomicExchange => {
                let a = self.val(&args[0])?;
                let v = self.val(&args[1])?;
                let old = self.tmp(
                    ty.clone(),
                    RValue::RawLoad {
                        addr: a.clone(),
                        ty: ty.clone(),
                        volatile: true,
                    },
                );
                self.push(Statement::RawStore {
                    addr: a,
                    value: v,
                    ty: ty.clone(),
                    volatile: true,
                });
                old
            }
            Builtin::AtomicFetch(op) | Builtin::AtomicOpFetch(op) => {
                let a = self.val(&args[0])?;
                let v = self.val(&args[1])?;
                let old = self.tmp(
                    ty.clone(),
                    RValue::RawLoad {
                        addr: a.clone(),
                        ty: ty.clone(),
                        volatile: true,
                    },
                );
                let r = self.bin(arith_op(op), prim(ty), old.clone(), v);
                self.push(Statement::RawStore {
                    addr: a,
                    value: r.clone(),
                    ty: ty.clone(),
                    volatile: true,
                });
                if matches!(b, Builtin::AtomicFetch(_)) {
                    old
                } else {
                    r
                }
            }
            Builtin::AtomicFetchNand => {
                let a = self.val(&args[0])?;
                let v = self.val(&args[1])?;
                let old = self.tmp(
                    ty.clone(),
                    RValue::RawLoad {
                        addr: a.clone(),
                        ty: ty.clone(),
                        volatile: true,
                    },
                );
                let r = self.bin(PrimOp::BitAnd, prim(ty), old.clone(), v);
                let r = self.tmp(
                    ty.clone(),
                    RValue::UnaryOp {
                        op: PrimOp::Not,
                        op_ty: ty.clone(),
                        operand: r,
                    },
                );
                self.push(Statement::RawStore {
                    addr: a,
                    value: r,
                    ty: ty.clone(),
                    volatile: true,
                });
                old
            }
            Builtin::AtomicCas | Builtin::AtomicCasVal => {
                let et = self.ty(&args[2])?;
                let p = self.val(&args[0])?;
                let cur = self.tmp(
                    et.clone(),
                    RValue::RawLoad {
                        addr: p.clone(),
                        ty: et.clone(),
                        volatile: true,
                    },
                );
                let expect = if b == Builtin::AtomicCas {
                    let ep = self.val(&args[1])?;
                    (
                        self.tmp(
                            et.clone(),
                            RValue::RawLoad {
                                addr: ep.clone(),
                                ty: et.clone(),
                                volatile: false,
                            },
                        ),
                        Some(ep),
                    )
                } else {
                    (self.val(&args[1])?, None)
                };
                let des = self.val(&args[2])?;
                let eq = self.bin(PrimOp::Eq, prim(&et), cur.clone(), expect.0);
                let (yes, no, end) = (self.block(), self.block(), self.block());
                self.branch(eq.clone(), yes, no);
                self.cur = Some(self.idx(yes));
                self.push(Statement::RawStore {
                    addr: p,
                    value: des,
                    ty: et.clone(),
                    volatile: true,
                });
                self.goto(end);
                self.cur = Some(self.idx(no));
                if let Some(ep) = expect.1 {
                    self.push(Statement::RawStore {
                        addr: ep,
                        value: cur.clone(),
                        ty: et,
                        volatile: false,
                    });
                }
                self.goto(end);
                self.cur = Some(self.idx(end));
                if b == Builtin::AtomicCas {
                    self.cast(eq, &MirTy::Prim(PrimTy::Bool), ty)
                } else {
                    cur
                }
            }
            Builtin::Alloca | Builtin::FrameAddress => {
                return Err(self
                    .lx
                    .err(loc, "this builtin is not supported by `nabi cc`"));
            }
        })
    }

    /// Whether `x op y` (`r`, wrapped) overflowed `p`.
    fn overflowed(&mut self, b: Builtin, p: PrimTy, x: Operand, y: Operand, r: Operand) -> Operand {
        let signed = p.is_signed_int();
        match (b, signed) {
            (Builtin::AddOverflow, false) => self.bin(PrimOp::Lt, p, r, x),
            (Builtin::SubOverflow, false) => self.bin(PrimOp::Lt, p, x, y),
            (Builtin::AddOverflow, true) => {
                let a = self.bin(PrimOp::BitXor, p, x, r.clone());
                let c = self.bin(PrimOp::BitXor, p, y, r);
                let d = self.bin(PrimOp::BitAnd, p, a, c);
                self.bin(PrimOp::Lt, p, d, Self::k(0, p))
            }
            (Builtin::SubOverflow, true) => {
                let a = self.bin(PrimOp::BitXor, p, x.clone(), y);
                let c = self.bin(PrimOp::BitXor, p, x, r);
                let d = self.bin(PrimOp::BitAnd, p, a, c);
                self.bin(PrimOp::Lt, p, d, Self::k(0, p))
            }
            _ => {
                // `x != 0 && r / x != y`, and MIN * -1 when signed.
                let res = self.local(MirTy::Prim(PrimTy::Bool), None);
                self.push(Statement::Assign {
                    target: res,
                    rvalue: RValue::Use(Operand::Const(ConstVal::Bool(false))),
                });
                let nz = self.bin(PrimOp::Ne, p, x.clone(), Self::k(0, p));
                let (div, end) = (self.block(), self.block());
                self.branch(nz, div, end);
                self.cur = Some(self.idx(div));
                if signed {
                    let m1 = self.bin(PrimOp::Eq, p, x.clone(), Self::k(-1, p));
                    let (minus, plain) = (self.block(), self.block());
                    self.branch(m1, minus, plain);
                    self.cur = Some(self.idx(minus));
                    let bits = p.bit_width(self.lx.t.pointer_width);
                    let min = -(1i128 << (bits - 1));
                    let is_min = self.bin(PrimOp::Eq, p, y.clone(), Self::k(min, p));
                    self.push(Statement::Assign {
                        target: res,
                        rvalue: RValue::Use(is_min),
                    });
                    self.goto(end);
                    self.cur = Some(self.idx(plain));
                }
                let q = self.bin(PrimOp::Div, p, r, x);
                let ne = self.bin(PrimOp::Ne, p, q, y);
                self.push(Statement::Assign {
                    target: res,
                    rvalue: RValue::Use(ne),
                });
                self.goto(end);
                self.cur = Some(self.idx(end));
                Operand::Copy(res)
            }
        }
    }
}
