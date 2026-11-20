//! C functions (`FnRole::C`): their parameters and result the C way,
//! and `va_start`.

#[allow(clippy::wildcard_imports)]
use crate::lower::*;

/// The `va_start` of a variadic C function: where its named arguments
/// ended.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Named {
    /// Integer / float argument registers they took.
    pub gp: u32,
    pub fp: u32,
    /// Bytes of the stack argument area they took.
    pub stack: u32,
    /// Argument positions they took (Windows x64, powerpc64).
    pub pos: u32,
}

impl FnLower<'_, '_> {
    /// Lower a C function: parameters as the C ABI passes them into the
    /// locals, then the blocks.
    pub(crate) fn lower_c(mut self) -> Func {
        let t = self.c.t;
        let f = self.f;
        let (variadic, system) = match f.facts.abi {
            Abi::C { variadic, system } => (variadic, system),
            Abi::Nabi => (false, false),
        };
        let Some(abi) = classify(
            &mut self.c.errors,
            f.span,
            t,
            &f.param_tys,
            &f.ret_ty,
            (variadic, system),
        ) else {
            return self.out;
        };
        let mut shapes = Vec::new();
        for (p, a) in f.param_tys.iter().zip(&abi.args) {
            shapes.push(match a {
                ArgAbi::Direct => cc::shape(p, t),
                ArgAbi::Coerce(c) => cc::coerced(c, t.pointer_width),
                ArgAbi::Indirect { byval: true } => {
                    let (size, align) = t.size_align(p);
                    cc::byval(size, align, t.pointer_width)
                }
                ArgAbi::Indirect { byval: false } => cc::coerced(&CType::PTR, t.pointer_width),
            });
        }
        let ret_shape = match &abi.ret {
            RetAbi::Direct if f.ret_ty != MirTy::Unit => Some(cc::shape(&f.ret_ty, t)),
            RetAbi::Coerce(c) => Some(cc::coerced(c, t.pointer_width)),
            _ => None,
        };
        let sret = abi.ret == RetAbi::Sret;
        // A variadic definition's named arguments go as a variadic call's
        // fixed ones do (base AAPCS on ARM, integer registers on Windows
        // arm64).
        let from = variadic.then_some(shapes.len());
        let locs = cc::assign(self.c.conv, &shapes, ret_shape.as_ref(), sret, from);
        self.out.pop = if abi.stdcall { locs.stack } else { locs.pop };
        self.out.sig = Some((locs.params.concat(), locs.ret.clone()));
        if variadic && self.c.conv == cc::Conv::Wasm32 {
            // wasm32: the variadic buffer's address is the last parameter.
            if let Some((ps, _)) = self.out.sig.as_mut() {
                let n = ps.iter().filter(|p| matches!(p.loc, Loc::Int(_))).count();
                ps.push(Part {
                    loc: Loc::Int(u8::try_from(n).unwrap_or(u8::MAX)),
                    k: K::I32,
                    signed: false,
                });
            }
        }
        if sret {
            let s = self.out.slot(8, 8);
            let k = self.word();
            self.push(Inst::Param {
                part: Part {
                    loc: Loc::Sret,
                    k,
                    signed: false,
                },
                dst: Mem::Frame(s, 0),
            });
            self.sret = Some(s);
        }
        // Every part stored first: what follows may use argument registers
        // as scratch.
        let mut raws = Vec::new();
        for (p, (parts, sh)) in f.param_tys.iter().zip(locs.params.iter().zip(&shapes)) {
            let size = t.size_align(p).0;
            let raw = self.tmp_bytes(size.next_multiple_of(8).max(16), 16);
            for (part, l) in parts.iter().zip(&sh.leaves) {
                self.push(Inst::Param {
                    part: *part,
                    dst: raw.at(l.off),
                });
            }
            raws.push(raw);
        }
        if variadic {
            self.va = Some(self.va_save(&locs, sret));
        }
        for ((l, ty), (a, raw)) in f
            .params
            .iter()
            .zip(&f.param_tys)
            .zip(abi.args.iter().zip(raws))
        {
            let Some(m) = self.local_mem(*l) else {
                continue;
            };
            let size = t.size_align(ty).0;
            if size == 0 {
                continue;
            }
            let src = match a {
                ArgAbi::Indirect { byval: false } => {
                    let Mem::Frame(s, _) = raw else {
                        unreachable!()
                    };
                    Mem::Ind(s, 0)
                }
                _ => raw,
            };
            self.push(Inst::Copy { dst: m, src, size });
            if *ty == MirTy::Prim(PrimTy::Bool) {
                self.normalize_bool(m);
            }
        }
        self.ret_parts.clone_from(&locs.ret);
        self.c_ret = Some((abi.ret, ret_shape));
        self.vec = self.vector_loops();
        if self.c.opt {
            self.combined = combine::plan(f, t);
        }
        for (pos, b) in f.blocks.iter().enumerate() {
            self.block(pos, b);
        }
        self.emit_panics();
        if self.c.debug.is_some() && f.span.line != 0 {
            self.out.debug = Some(self.debug_facts());
        }
        self.out
    }

    /// Return `v` (a `ty`) the C way.
    pub(crate) fn c_return(&mut self, v: Val, ty: &MirTy) {
        let t = self.c.t;
        let Some((ret, shape)) = self.c_ret.clone() else {
            return;
        };
        if let Some(s) = self.sret {
            self.store(v, ty, Mem::Ind(s, 0));
            let k = self.word();
            self.push(sret_ret(s, k));
            return;
        }
        let parts = self.ret_parts.clone().unwrap_or_default();
        let sh = shape.unwrap_or_else(|| cc::shape(ty, t));
        let src = if matches!(ret, RetAbi::Coerce(_)) {
            let size = t.size_align(ty).0;
            let m = self.tmp_bytes(size.max(16), 16);
            self.push(Inst::Zero {
                dst: m,
                size: size.max(16),
            });
            self.store(v, ty, m);
            m
        } else {
            self.mem_of(v, ty)
        };
        let vals = parts
            .iter()
            .zip(&sh.leaves)
            .map(|(p, l)| (*p, V::M(src.at(l.off))))
            .collect();
        self.push(Inst::Ret { vals });
    }

    /// Where the named arguments of `locs` ended.
    fn named(locs: &cc::SigLocs, sret: bool, conv: cc::Conv) -> Named {
        let mut n = Named::default();
        if sret {
            n.pos = 1;
            // The result pointer takes the first integer register but on
            // AAPCS64 (`x8`).
            let x8 = matches!(
                conv,
                cc::Conv::Darwin64 | cc::Conv::Aapcs64 | cc::Conv::WinArm64
            );
            n.gp = u32::from(!x8);
        }
        for parts in &locs.params {
            for p in parts {
                match p.loc {
                    Loc::Int(r) => {
                        n.gp = n.gp.max(u32::from(r) + 1);
                        n.pos = n.pos.max(u32::from(r) + 1);
                    }
                    Loc::Fp(r) | Loc::Vec(r) => {
                        n.fp = n.fp.max(u32::from(r) + 1);
                        n.pos = n.pos.max(u32::from(r) + 1);
                    }
                    Loc::VecStack(o) => {
                        n.stack = n.stack.max(o + 16);
                        n.pos = n.pos.max(o / 8 + 2);
                    }
                    Loc::Stack(o) => {
                        n.stack = n.stack.max(o + p.k.size());
                        n.pos = n.pos.max(o / 8 + 1);
                    }
                    Loc::Sret | Loc::Area(_) => {}
                }
            }
        }
        n
    }

    /// A variadic C function's entry: the registers a `va_list` reads
    /// saved where it reads them. The `Named` facts and the save area
    /// `va_start` builds the list from.
    fn va_save(&mut self, locs: &cc::SigLocs, sret: bool) -> (Named, Option<Mem>) {
        let n = Self::named(locs, sret, self.c.conv);
        let w = self.word();
        let (gp, fp, gsize, fsize) = match self.c.conv {
            cc::Conv::SysV64 => (6, 8, 8, 16),
            cc::Conv::Aapcs64 => (8, 8, 8, 16),
            cc::Conv::S390x => (5, 4, 8, 8),
            cc::Conv::Win64 => (4, 0, 8, 0),
            cc::Conv::Ppc64 => (8, 0, 8, 0),
            cc::Conv::Riscv64 | cc::Conv::Riscv32 | cc::Conv::ArmHf => {
                // The prologue stores the argument registers below the
                // stack arguments.
                let regs = if self.c.conv == cc::Conv::ArmHf { 4 } else { 8 };
                self.out.va_save = regs * self.pw() / 8;
                return (n, None);
            }
            cc::Conv::Wasm32 => {
                // The buffer's address: the parameter after the named
                // ones, read at the entry.
                let v = self.word_tmp();
                self.push(Inst::Param {
                    part: Part {
                        loc: Loc::Int(u8::try_from(n.gp).unwrap_or(u8::MAX)),
                        k: K::I32,
                        signed: false,
                    },
                    dst: v,
                });
                return (n, Some(v));
            }
            _ => return (n, None),
        };
        // s390x lays the area out as its register save area: r2 at 16,
        // f0 at 128.
        let s390 = self.c.conv == cc::Conv::S390x;
        let (gbase, fbase) = if s390 { (16, 128) } else { (0, gp * gsize) };
        let size = if s390 { 160 } else { gp * gsize + fp * fsize };
        let area = self.tmp_bytes(size, 16);
        // Windows x64 and powerpc64: the caller's home / save area holds
        // the registers, contiguous with the stack arguments.
        let home = matches!(self.c.conv, cc::Conv::Win64 | cc::Conv::Ppc64);
        for r in n.gp.min(gp)..gp {
            let dst = if home {
                let a = self.word_tmp();
                self.push(Inst::Param {
                    part: Part {
                        loc: Loc::Area(r * 8),
                        k: w,
                        signed: false,
                    },
                    dst: a,
                });
                let v = self.word_tmp();
                self.push(Inst::Param {
                    part: Part {
                        loc: Loc::Int(r as u8),
                        k: w,
                        signed: false,
                    },
                    dst: v,
                });
                let Mem::Frame(s, _) = a else { unreachable!() };
                self.push(Inst::Mov {
                    k: w,
                    dst: Mem::Ind(s, 0),
                    src: V::M(v),
                });
                continue;
            } else {
                area.at(gbase + r * gsize)
            };
            self.push(Inst::Param {
                part: Part {
                    loc: Loc::Int(r as u8),
                    k: w,
                    signed: false,
                },
                dst,
            });
        }
        for r in n.fp.min(fp)..fp {
            self.push(Inst::Param {
                part: Part {
                    loc: Loc::Fp(r as u8),
                    k: K::F64,
                    signed: false,
                },
                dst: area.at(fbase + r * fsize),
            });
        }
        (n, (!home).then_some(area))
    }

    /// `va_start(ap)`: the `va_list` at address `ap` set to the first
    /// variadic argument, as the target's C library reads it.
    pub(crate) fn va_start(&mut self, ap: V) {
        let w = self.word();
        let pw = self.pw() / 8;
        let Some((n, area)) = self.va else {
            self.err("`va_start` outside a variadic C function".to_owned());
            return;
        };
        let p = self.word_tmp();
        self.push(Inst::Mov {
            k: w,
            dst: p,
            src: ap,
        });
        let Mem::Frame(ps, _) = p else { unreachable!() };
        let at = |o: i32| Mem::Ind(ps, o);
        let stack_at = |me: &mut Self, off: u32| {
            let a = me.word_tmp();
            me.push(Inst::Param {
                part: Part {
                    loc: Loc::Area(off),
                    k: w,
                    signed: false,
                },
                dst: a,
            });
            a
        };
        let addr_of = |me: &mut Self, m: Mem| {
            let a = me.word_tmp();
            me.push(Inst::Addr { dst: a, src: m });
            a
        };
        match self.c.conv {
            cc::Conv::SysV64 => {
                // gp_offset, fp_offset, overflow_arg_area, reg_save_area.
                let st = stack_at(self, n.stack.next_multiple_of(8));
                let ra = addr_of(self, area.expect("a save area"));
                self.push(Inst::Mov {
                    k: K::I32,
                    dst: at(0),
                    src: V::I(i64::from(n.gp.min(6) * 8)),
                });
                self.push(Inst::Mov {
                    k: K::I32,
                    dst: at(4),
                    src: V::I(i64::from(48 + n.fp.min(8) * 16)),
                });
                self.push(Inst::Mov {
                    k: K::I64,
                    dst: at(8),
                    src: V::M(st),
                });
                self.push(Inst::Mov {
                    k: K::I64,
                    dst: at(16),
                    src: V::M(ra),
                });
            }
            cc::Conv::Aapcs64 => {
                // __stack, __gr_top, __vr_top, __gr_offs, __vr_offs.
                let st = stack_at(self, n.stack.next_multiple_of(8));
                let a = area.expect("a save area");
                let gr_top = addr_of(self, a.at(64));
                let vr_top = addr_of(self, a.at(64 + 128));
                self.push(Inst::Mov {
                    k: K::I64,
                    dst: at(0),
                    src: V::M(st),
                });
                self.push(Inst::Mov {
                    k: K::I64,
                    dst: at(8),
                    src: V::M(gr_top),
                });
                self.push(Inst::Mov {
                    k: K::I64,
                    dst: at(16),
                    src: V::M(vr_top),
                });
                let gr = -(i64::from(8 - n.gp.min(8)) * 8);
                let vr = -(i64::from(8 - n.fp.min(8)) * 16);
                self.push(Inst::Mov {
                    k: K::I32,
                    dst: at(24),
                    src: V::I(gr),
                });
                self.push(Inst::Mov {
                    k: K::I32,
                    dst: at(28),
                    src: V::I(vr),
                });
            }
            cc::Conv::S390x => {
                // __gpr, __fpr (counts), __overflow_arg_area, __reg_save_area
                // (laid out as the ABI's: r2 at 16, f0 at 128).
                let st = stack_at(self, n.stack.next_multiple_of(8));
                let a = area.expect("a save area");
                let base = addr_of(self, a);
                let k = K::I64;
                self.push(Inst::Mov {
                    k,
                    dst: at(0),
                    src: V::I(i64::from(n.gp.min(5))),
                });
                self.push(Inst::Mov {
                    k,
                    dst: at(8),
                    src: V::I(i64::from(n.fp.min(4))),
                });
                self.push(Inst::Mov {
                    k,
                    dst: at(16),
                    src: V::M(st),
                });
                self.push(Inst::Mov {
                    k,
                    dst: at(24),
                    src: V::M(base),
                });
            }
            cc::Conv::Win64 | cc::Conv::Ppc64 => {
                let st = stack_at(self, n.pos * 8);
                self.push(Inst::Mov {
                    k: w,
                    dst: at(0),
                    src: V::M(st),
                });
            }
            cc::Conv::Darwin64 | cc::Conv::X86 | cc::Conv::X86Win => {
                let st = stack_at(self, n.stack.next_multiple_of(pw));
                self.push(Inst::Mov {
                    k: w,
                    dst: at(0),
                    src: V::M(st),
                });
            }
            cc::Conv::Wasm32 => {
                let v = area.expect("the buffer's address");
                self.push(Inst::Mov {
                    k: w,
                    dst: at(0),
                    src: V::M(v),
                });
            }
            cc::Conv::Riscv64 | cc::Conv::Riscv32 | cc::Conv::ArmHf => {
                let regs = if self.c.conv == cc::Conv::ArmHf { 4 } else { 8 };
                let off = if n.gp < regs {
                    n.gp * pw
                } else {
                    self.out.va_save + n.stack.next_multiple_of(pw)
                };
                let st = stack_at(self, off);
                self.push(Inst::Mov {
                    k: w,
                    dst: at(0),
                    src: V::M(st),
                });
            }
            cc::Conv::WinArm64 => {
                let msg = format!(
                    "a variadic C function definition on {}: write a `va_list` version \
                     (`int vf(const char *fmt, va_list ap)`) and call that from a non-variadic \
                     wrapper",
                    self.c.t.name
                );
                self.err(msg);
            }
        }
    }
}
