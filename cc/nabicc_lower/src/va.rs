//! `va_arg`: the next variadic argument, read as the target's C library
//! lays out a `va_list` (codegen's `va_start` sets it up).

use crate::R;
use crate::body::Fb;
use crate::expr::cval;
use nabic_codegen::abi::{self, ArgAbi, CScalar, CType};
use nabic_ir_common::{ConstVal, PrimOp, PrimTy};
use nabic_mir::{CAbi, MirTy, Operand, RValue, Statement};
use nabicc_lex::Loc;
use nabicc_parse::ty::TypeId;
use nabicc_sema::TExpr;

const W: MirTy = MirTy::Prim(PrimTy::Usize);

impl Fb<'_, '_> {
    fn ld(&mut self, a: Operand, p: PrimTy) -> Operand {
        self.tmp(
            MirTy::Prim(p),
            RValue::RawLoad {
                addr: a,
                ty: MirTy::Prim(p),
                volatile: false,
            },
        )
    }

    fn st(&mut self, a: Operand, v: Operand, p: PrimTy) {
        self.push(Statement::RawStore {
            addr: a,
            value: v,
            ty: MirTy::Prim(p),
            volatile: false,
        });
    }

    fn op(&mut self, op: PrimOp, p: PrimTy, a: Operand, b: Operand) -> Operand {
        let rt = if matches!(
            op,
            PrimOp::Lt | PrimOp::Le | PrimOp::Gt | PrimOp::Ge | PrimOp::Eq | PrimOp::Ne
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

    fn c(v: i128, p: PrimTy) -> Operand {
        Operand::Const(cval(v, p))
    }

    /// `(a + n - 1) & -n`.
    fn align_up(&mut self, a: Operand, n: u64) -> Operand {
        if n <= 1 {
            return a;
        }
        let x = self.op(
            PrimOp::WrappingAdd,
            PrimTy::Usize,
            a,
            Self::c(i128::from(n - 1), PrimTy::Usize),
        );
        self.op(
            PrimOp::BitAnd,
            PrimTy::Usize,
            x,
            Self::c(-i128::from(n), PrimTy::Usize),
        )
    }

    /// The address of the next argument of C type `cty` (MIR `ty`) of
    /// the `va_list` at address `ap`.
    pub(crate) fn va_arg(&mut self, ap: &TExpr, cty: TypeId, ty: &MirTy, loc: Loc) -> R<Operand> {
        let apv = self.val(ap)?;
        let t = self.lx.t.clone();
        let (size, align) = t.size_align(ty);
        let (size, align) = (u64::from(size), u64::from(align));
        let pw = u64::from(t.pointer_width / 8);
        let fa = abi::classify(&t, std::slice::from_ref(ty), &MirTy::Unit, false);
        let a = fa
            .as_ref()
            .and_then(|f| f.args.first().cloned())
            .unwrap_or(ArgAbi::Direct);
        let _ = cty;
        match t.c_abi {
            CAbi::X86_64SysV => Ok(self.va_sysv(apv, ty, &a, size, align)),
            CAbi::Aarch64 if !matches!(t.os.as_str(), "macos" | "ios" | "windows") => {
                Ok(self.va_aapcs(apv, ty, &a, size, align))
            }
            CAbi::S390x => Ok(self.va_s390x(apv, ty, &a, size)),
            _ => {
                // A pointer stepping through slots.
                let (slot, al, byref) = match t.c_abi {
                    CAbi::X86_64Win => (8, 8, !matches!(size, 1 | 2 | 4 | 8)),
                    CAbi::Aarch64 => (8, if align > 8 { 16 } else { 8 }, size > 16),
                    CAbi::X86SysV | CAbi::X86Win => (4, 4, false),
                    CAbi::ArmHf => (4, align.clamp(4, 8), false),
                    CAbi::Riscv64 | CAbi::Riscv32 => {
                        (pw, if align > pw { 2 * pw } else { pw }, size > 2 * pw)
                    }
                    CAbi::Ppc64 => (8, 8, false),
                    CAbi::Wasm32 => (4, align.max(4), matches!(a, ArgAbi::Indirect { .. })),
                    _ => return Err(self.lx.err(loc, "`va_arg` on this target")),
                };
                let cur = self.ld(apv.clone(), PrimTy::Usize);
                let at = self.align_up(cur, al);
                let used = if byref {
                    pw
                } else {
                    size.div_ceil(slot) * slot
                };
                let next = self.op(
                    PrimOp::WrappingAdd,
                    PrimTy::Usize,
                    at.clone(),
                    Self::c(i128::from(used.max(slot)), PrimTy::Usize),
                );
                self.st(apv, next, PrimTy::Usize);
                if byref {
                    return Ok(self.ld(at, PrimTy::Usize));
                }
                Ok(at)
            }
        }
    }

    /// The eightbytes of a System V argument: which are SSE.
    fn sse_parts(ty: &MirTy, a: &ArgAbi) -> Vec<bool> {
        let s = |c: &CScalar| matches!(c, CScalar::F32 | CScalar::F64 | CScalar::F32x2);
        match a {
            ArgAbi::Direct => vec![matches!(ty, MirTy::Prim(p) if p.is_float())],
            ArgAbi::Coerce(CType::Scalar(c)) => vec![s(c)],
            ArgAbi::Coerce(CType::Struct(cs)) => cs.iter().map(s).collect(),
            ArgAbi::Coerce(CType::Array(n, c)) => vec![s(c); *n as usize],
            ArgAbi::Coerce(CType::At(fs)) => {
                let mut v = Vec::new();
                for (off, c) in fs {
                    let i = (*off / 8) as usize;
                    if v.len() <= i {
                        v.resize(i + 1, true);
                    }
                    v[i] &= s(c);
                }
                v
            }
            ArgAbi::Indirect { .. } => Vec::new(),
        }
    }

    /// x86-64 System V: from the register save area while the
    /// registers last, else the overflow area.
    fn va_sysv(&mut self, ap: Operand, ty: &MirTy, a: &ArgAbi, size: u64, align: u64) -> Operand {
        let parts = Self::sse_parts(ty, a);
        let res = self.local(W, None);
        let end = self.block();
        let mem = self.block();
        if parts.is_empty() {
            self.goto(mem);
        } else {
            let ngp = parts.iter().filter(|s| !**s).count() as i128;
            let nfp = parts.len() as i128 - ngp;
            let gpo_a = ap.clone();
            let fpo_a = self.offset(ap.clone(), 4);
            let gpo = self.ld(gpo_a.clone(), PrimTy::U32);
            let fpo = self.ld(fpo_a.clone(), PrimTy::U32);
            let g_ok = self.op(
                PrimOp::Le,
                PrimTy::U32,
                gpo.clone(),
                Self::c(48 - 8 * ngp, PrimTy::U32),
            );
            let f_ok = self.op(
                PrimOp::Le,
                PrimTy::U32,
                fpo.clone(),
                Self::c(176 - 16 * nfp, PrimTy::U32),
            );
            let both = self.tmp(
                MirTy::Prim(PrimTy::Bool),
                RValue::BinOp {
                    op: PrimOp::LogicalAnd,
                    op_ty: MirTy::Prim(PrimTy::Bool),
                    lhs: g_ok,
                    rhs: f_ok,
                },
            );
            let regs = self.block();
            self.branch(both, regs, mem);
            self.cur = Some(self.idx(regs));
            let rsa_a = self.offset(ap.clone(), 16);
            let rsa = self.ld(rsa_a, PrimTy::Usize);
            // Gather the eightbytes into a buffer of the value's type.
            let buf = self.local(
                MirTy::Array(Box::new(MirTy::Prim(PrimTy::U64)), parts.len() as u32),
                None,
            );
            let ba = self.tmp(W, RValue::AddrOf(buf));
            let (mut g, mut f) = (gpo, fpo);
            for (i, sse) in parts.iter().enumerate() {
                let (offv, p) = if *sse {
                    (f.clone(), PrimTy::U32)
                } else {
                    (g.clone(), PrimTy::U32)
                };
                let ow = self.tmp(
                    W,
                    RValue::Cast {
                        source: offv,
                        from_ty: MirTy::Prim(p),
                        to_ty: W,
                    },
                );
                let src = self.op(PrimOp::WrappingAdd, PrimTy::Usize, rsa.clone(), ow);
                let v = self.ld(src, PrimTy::U64);
                let dst = self.offset(ba.clone(), 8 * i as u64);
                self.st(dst, v, PrimTy::U64);
                if *sse {
                    f = self.op(
                        PrimOp::WrappingAdd,
                        PrimTy::U32,
                        f,
                        Self::c(16, PrimTy::U32),
                    );
                } else {
                    g = self.op(PrimOp::WrappingAdd, PrimTy::U32, g, Self::c(8, PrimTy::U32));
                }
            }
            self.st(gpo_a, g, PrimTy::U32);
            self.st(fpo_a, f, PrimTy::U32);
            self.push(Statement::Assign {
                target: res,
                rvalue: RValue::Use(ba),
            });
            self.goto(end);
        }
        self.cur = Some(self.idx(mem));
        let ova = self.offset(ap, 8);
        let ov = self.ld(ova.clone(), PrimTy::Usize);
        let at = self.align_up(ov, align.max(8));
        let next = self.op(
            PrimOp::WrappingAdd,
            PrimTy::Usize,
            at.clone(),
            Self::c(i128::from(size.div_ceil(8) * 8), PrimTy::Usize),
        );
        self.st(ova, next, PrimTy::Usize);
        self.push(Statement::Assign {
            target: res,
            rvalue: RValue::Use(at),
        });
        self.goto(end);
        self.cur = Some(self.idx(end));
        Operand::Copy(res)
    }

    /// AAPCS64 (not Apple, not Windows): general or vector registers
    /// while they last (`__gr_offs` / `__vr_offs` below zero), else the
    /// stack.
    fn va_aapcs(&mut self, ap: Operand, ty: &MirTy, a: &ArgAbi, size: u64, align: u64) -> Operand {
        let (fp_regs, elem) = match a {
            ArgAbi::Direct if matches!(ty, MirTy::Prim(p) if p.is_float()) => (1, size),
            ArgAbi::Coerce(CType::Array(n, CScalar::F32)) => (*n, 4),
            ArgAbi::Coerce(CType::Array(n, CScalar::F64)) => (*n, 8),
            _ => (0, 0),
        };
        let byref = size > 16 && fp_regs == 0;
        let res = self.local(W, None);
        let (regs, stack, end) = (self.block(), self.block(), self.block());
        let (offs_a, top_a, n) = if fp_regs > 0 {
            (
                self.offset(ap.clone(), 28),
                self.offset(ap.clone(), 16),
                i128::from(fp_regs) * 16,
            )
        } else {
            let used = if byref { 8 } else { size.div_ceil(8) * 8 };
            (
                self.offset(ap.clone(), 24),
                self.offset(ap.clone(), 8),
                i128::from(used),
            )
        };
        let mut offs = self.ld(offs_a.clone(), PrimTy::I32);
        let neg = self.op(
            PrimOp::Lt,
            PrimTy::I32,
            offs.clone(),
            Self::c(0, PrimTy::I32),
        );
        let try_regs = self.block();
        self.branch(neg, try_regs, stack);
        self.cur = Some(self.idx(try_regs));
        if fp_regs == 0 && align > 8 {
            // A 16-aligned value starts at an even register.
            let x = self.op(
                PrimOp::WrappingAdd,
                PrimTy::I32,
                offs,
                Self::c(15, PrimTy::I32),
            );
            offs = self.op(PrimOp::BitAnd, PrimTy::I32, x, Self::c(-16, PrimTy::I32));
        }
        let next = self.op(
            PrimOp::WrappingAdd,
            PrimTy::I32,
            offs.clone(),
            Self::c(n, PrimTy::I32),
        );
        self.st(offs_a, next.clone(), PrimTy::I32);
        let fits = self.op(PrimOp::Le, PrimTy::I32, next, Self::c(0, PrimTy::I32));
        self.branch(fits, regs, stack);
        self.cur = Some(self.idx(regs));
        let top = self.ld(top_a, PrimTy::Usize);
        let ow = self.tmp(
            MirTy::Prim(PrimTy::Isize),
            RValue::Cast {
                source: offs,
                from_ty: MirTy::Prim(PrimTy::I32),
                to_ty: MirTy::Prim(PrimTy::Isize),
            },
        );
        let ow = self.tmp(
            W,
            RValue::Cast {
                source: ow,
                from_ty: MirTy::Prim(PrimTy::Isize),
                to_ty: W,
            },
        );
        let base = self.op(PrimOp::WrappingAdd, PrimTy::Usize, top, ow);
        if fp_regs > 1 {
            // An HFA: each member in its own 16-byte register slot.
            let buf = self.local(MirTy::Array(Box::new(MirTy::Prim(PrimTy::U64)), 4), None);
            let ba = self.tmp(W, RValue::AddrOf(buf));
            let p = if elem == 4 { PrimTy::U32 } else { PrimTy::U64 };
            for i in 0..u64::from(fp_regs) {
                let src = self.offset(base.clone(), 16 * i);
                let v = self.ld(src, p);
                let dst = self.offset(ba.clone(), elem * i);
                self.st(dst, v, p);
            }
            self.push(Statement::Assign {
                target: res,
                rvalue: RValue::Use(ba),
            });
        } else if byref {
            let p = self.ld(base, PrimTy::Usize);
            self.push(Statement::Assign {
                target: res,
                rvalue: RValue::Use(p),
            });
        } else {
            self.push(Statement::Assign {
                target: res,
                rvalue: RValue::Use(base),
            });
        }
        self.goto(end);
        self.cur = Some(self.idx(stack));
        let sa = ap;
        let s = self.ld(sa.clone(), PrimTy::Usize);
        let at = self.align_up(s, if align > 8 { 16 } else { 8 });
        let used = if byref { 8 } else { size.div_ceil(8) * 8 };
        let next = self.op(
            PrimOp::WrappingAdd,
            PrimTy::Usize,
            at.clone(),
            Self::c(i128::from(used), PrimTy::Usize),
        );
        self.st(sa, next, PrimTy::Usize);
        let v = if byref {
            self.ld(at, PrimTy::Usize)
        } else {
            at
        };
        self.push(Statement::Assign {
            target: res,
            rvalue: RValue::Use(v),
        });
        self.goto(end);
        self.cur = Some(self.idx(end));
        Operand::Copy(res)
    }

    /// s390x: `__gpr` / `__fpr` counts, right-justified 8-byte slots.
    fn va_s390x(&mut self, ap: Operand, ty: &MirTy, a: &ArgAbi, size: u64) -> Operand {
        let float = matches!(ty, MirTy::Prim(p) if p.is_float())
            || matches!(
                a,
                ArgAbi::Coerce(CType::Scalar(CScalar::F32 | CScalar::F64))
            );
        let byref = !float && !matches!(size, 1 | 2 | 4 | 8);
        let n = if byref { 8 } else { size };
        let (cnt_a, max, base_off) = if float {
            (self.offset(ap.clone(), 8), 4, 128)
        } else {
            (self.offset(ap.clone(), 0), 5, 16)
        };
        let res = self.local(W, None);
        let (regs, mem, end) = (self.block(), self.block(), self.block());
        let cnt = self.ld(cnt_a.clone(), PrimTy::U64);
        let ok = self.op(
            PrimOp::Lt,
            PrimTy::U64,
            cnt.clone(),
            Self::c(max, PrimTy::U64),
        );
        self.branch(ok, regs, mem);
        self.cur = Some(self.idx(regs));
        let next = self.op(
            PrimOp::WrappingAdd,
            PrimTy::U64,
            cnt.clone(),
            Self::c(1, PrimTy::U64),
        );
        self.st(cnt_a, next, PrimTy::U64);
        let rsa_a = self.offset(ap.clone(), 24);
        let rsa = self.ld(rsa_a, PrimTy::Usize);
        let k8 = self.op(
            PrimOp::WrappingMul,
            PrimTy::U64,
            cnt,
            Self::c(8, PrimTy::U64),
        );
        let k8 = self.tmp(
            W,
            RValue::Cast {
                source: k8,
                from_ty: MirTy::Prim(PrimTy::U64),
                to_ty: W,
            },
        );
        let slot = self.op(PrimOp::WrappingAdd, PrimTy::Usize, rsa, k8);
        // A float sits in the register's high (first) word.
        let just = if float { 0 } else { 8 - n };
        let at = self.offset(slot, base_off + just);
        let v = if byref {
            self.ld(at, PrimTy::Usize)
        } else {
            at
        };
        self.push(Statement::Assign {
            target: res,
            rvalue: RValue::Use(v),
        });
        self.goto(end);
        self.cur = Some(self.idx(mem));
        let ova = self.offset(ap, 16);
        let ov = self.ld(ova.clone(), PrimTy::Usize);
        let next = self.op(
            PrimOp::WrappingAdd,
            PrimTy::Usize,
            ov.clone(),
            Self::c(8, PrimTy::Usize),
        );
        self.st(ova, next, PrimTy::Usize);
        let just = 8 - n;
        let at = self.offset(ov, just);
        let v = if byref {
            self.ld(at, PrimTy::Usize)
        } else {
            at
        };
        self.push(Statement::Assign {
            target: res,
            rvalue: RValue::Use(v),
        });
        self.goto(end);
        self.cur = Some(self.idx(end));
        let _ = ConstVal::Unit;
        Operand::Copy(res)
    }
}
