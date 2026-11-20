//! Functions codegen makes on its own: the C ABI entries of `extern "C"`
//! definitions, the C `main`, lazy static initializers and the GC type
//! registration.

use crate::cc;
use crate::lir::{Func, Inst, K, Loc, Mem, Part, SymId, V};
use crate::lower::{Ctx, FnLower, Sig};
use nabic_codegen::abi::{ArgAbi, CType, FnAbi, RetAbi};
use nabic_ir_common::{ConstVal, FunctionId, PrimTy};
use nabic_mir::{MirFunction, MirTy};

/// An empty MIR function to hang a generated body on.
pub fn shell(name: &str) -> MirFunction {
    MirFunction {
        id: FunctionId(u32::MAX),
        def_id: None,
        name: name.to_owned(),
        params: Vec::new(),
        locals: Vec::new(),
        blocks: Vec::new(),
        param_tys: Vec::new(),
        ret_ty: MirTy::Unit,
        span: nabic_diagnostic::Span::default(),
        facts: nabic_ir_common::FnFacts::default(),
        strips: Vec::new(),
        pins: Vec::new(),
        dbg: nabic_mir::FnDbg::default(),
    }
}

impl FnLower<'_, '_> {
    /// The C ABI entry of an `extern "C" fn` definition: takes the arguments as
    /// C passes them, runs the nabi body as a mutator again
    /// (`nabi_native_reenter`), returns the C way. `init_once`: first
    /// `nabi_init_once` (a wasm32 export).
    ///
    /// `on_panic`: a panic in the body returns this value to C
    /// (`nabi_catch_push`, then `_setjmp` on the record in this frame).
    pub fn c_entry(
        mut self,
        sig: &Sig,
        abi: &FnAbi,
        init_once: bool,
        on_panic: Option<&ConstVal>,
    ) -> Func {
        let t = self.c.t;
        let mut shapes = Vec::new();
        for (p, a) in sig.params.iter().zip(&abi.args) {
            shapes.push(match a {
                ArgAbi::Direct => cc::shape(p, t),
                ArgAbi::Coerce(c) => cc::coerced(c, t.pointer_width),
                ArgAbi::Indirect { byval: true } => {
                    let size = t.size_align(p).0;
                    cc::byval(size, nabic_codegen::abi::byval_align(p, t), t.pointer_width)
                }
                ArgAbi::Indirect { byval: false } => cc::coerced(&CType::PTR, t.pointer_width),
            });
        }
        let ret_shape = match &abi.ret {
            RetAbi::Direct if sig.ret != MirTy::Unit => Some(cc::shape(&sig.ret, t)),
            RetAbi::Coerce(c) => Some(cc::coerced(c, t.pointer_width)),
            _ => None,
        };
        let locs = cc::assign(
            self.c.conv,
            &shapes,
            ret_shape.as_ref(),
            abi.ret == RetAbi::Sret,
            None,
        );
        self.out.pop = if abi.stdcall { locs.stack } else { locs.pop };
        self.out.sig = Some((locs.params.concat(), locs.ret.clone()));
        let sret = (abi.ret == RetAbi::Sret).then(|| {
            let m = self.word_tmp();
            let k = self.word();
            self.push(Inst::Param {
                part: Part {
                    loc: Loc::Sret,
                    k,
                    signed: false,
                },
                dst: m,
            });
            m
        });
        // Every parameter stored first: what follows may use argument
        // registers as scratch.
        let mut raws = Vec::new();
        // A half comes in four bytes of its own.
        let mut halves = Vec::new();
        for ((p, a), (parts, sh)) in sig
            .params
            .iter()
            .zip(&abi.args)
            .zip(locs.params.iter().zip(&shapes))
        {
            let size = t.size_align(p).0;
            let raw = self.tmp_bytes(size.next_multiple_of(8).max(16), 16);
            let half = match a {
                ArgAbi::Coerce(c) => cc::half_leaves(c),
                _ => Vec::new(),
            };
            for (i, (part, l)) in parts.iter().zip(&sh.leaves).enumerate() {
                let dst = if half.get(i).copied().unwrap_or(false) {
                    let w = self.tmp_bytes(4, 4);
                    halves.push((self.half_at(w, part.loc).0, raw.at(l.off)));
                    w
                } else {
                    raw.at(l.off)
                };
                self.push(Inst::Param { part: *part, dst });
            }
            raws.push(raw);
        }
        for (w, d) in halves {
            self.push(Inst::Mov {
                k: K::I16,
                dst: d,
                src: V::M(w),
            });
        }
        let mut vals = Vec::new();
        for ((p, a), raw) in sig.params.iter().zip(&abi.args).zip(raws) {
            let size = t.size_align(p).0;
            let v = match a {
                ArgAbi::Indirect { byval: false } => {
                    let m = self.tmp(p);
                    let Mem::Frame(s, _) = raw else {
                        unreachable!()
                    };
                    if size > 0 {
                        self.push(Inst::Copy {
                            dst: m,
                            src: Mem::Ind(s, 0),
                            size,
                        });
                    }
                    m
                }
                _ => raw,
            };
            vals.push(self.val_of(v, p));
        }
        // wasm32: the host may call an export before `_start` (or without
        // it): the runtime starts on the first call.
        if init_once {
            let once = self.c.sym("nabi_init_once");
            self.call_sym(once, &[], &MirTy::Unit, &[], None);
        }
        let i32_ = MirTy::Prim(PrimTy::I32);
        let parked = self.tmp_k(K::I32);
        let reenter = self.c.sym("nabi_native_reenter");
        self.call_sym(reenter, &[], &i32_, &[], Some(parked));
        let r = (sig.ret != MirTy::Unit).then(|| self.tmp(&sig.ret));
        let catch = on_panic.map(|v| (self.catch_enter(), v));
        self.call_body(sig, &vals, r);
        if let Some(((rec, back), v)) = catch {
            let join = self.label();
            let w = self.word();
            self.call_rt("nabi_catch_pop", &[(V::M(rec), w)], None);
            self.push(Inst::Jmp(join));
            self.push(Inst::Label(back));
            self.out.cold.push(back);
            if let Some(r) = r {
                self.store_const(v, &sig.ret, r);
            }
            self.push(Inst::Jmp(join));
            self.push(Inst::Label(join));
        }
        let resume = self.c.sym("nabi_native_resume");
        let pv = self.val_of(parked, &i32_);
        self.call_sym(resume, &[i32_], &MirTy::Unit, &[pv], None);
        match (&abi.ret, r) {
            (RetAbi::Sret, Some(r)) => {
                let Some(Mem::Frame(s, _)) = sret else {
                    unreachable!()
                };
                let size = t.size_align(&sig.ret).0;
                self.push(Inst::Copy {
                    dst: Mem::Ind(s, 0),
                    src: r,
                    size,
                });
                let k = self.word();
                self.push(crate::lower::sret_ret(s, k));
            }
            (_, Some(r)) => {
                let parts = locs.ret.unwrap_or_default();
                let sh = ret_shape.unwrap_or_else(|| cc::shape(&sig.ret, t));
                let src = if matches!(abi.ret, RetAbi::Coerce(_)) {
                    let size = t.size_align(&sig.ret).0;
                    let m = self.tmp_bytes(size.max(16), 16);
                    self.push(Inst::Zero {
                        dst: m,
                        size: size.max(16),
                    });
                    self.push(Inst::Copy {
                        dst: m,
                        src: r,
                        size,
                    });
                    m
                } else {
                    r
                };
                let half = match &abi.ret {
                    RetAbi::Coerce(c) => cc::half_leaves(c),
                    _ => Vec::new(),
                };
                let mut vals = Vec::new();
                for (i, (p, l)) in parts.iter().zip(&sh.leaves).enumerate() {
                    let v = if half.get(i).copied().unwrap_or(false) {
                        self.widen_half(src.at(l.off), p.loc)
                    } else {
                        V::M(src.at(l.off))
                    };
                    vals.push((*p, v));
                }
                self.push(Inst::Ret { vals });
            }
            _ => self.push(Inst::Ret { vals: Vec::new() }),
        }
        self.out
    }

    /// Push an `#[on_panic]` recovery record in this frame and `_setjmp`
    /// on it: the record's address, and the label a recovery comes back
    /// to (the code after this goes on to the body).
    fn catch_enter(&mut self) -> (Mem, crate::lir::Label) {
        const JMP_AT: u32 = 64;
        let w = self.word();
        let buf = self.tmp_bytes(JMP_AT + 768, 16);
        let rec = self.word_tmp();
        self.push(Inst::Addr { dst: rec, src: buf });
        self.call_rt("nabi_catch_push", &[(V::M(rec), w)], None);
        let jb = self.word_tmp();
        self.push(Inst::Addr {
            dst: jb,
            src: buf.at(JMP_AT),
        });
        let got = self.tmp_k(K::I32);
        // Windows' `_setjmp` takes a frame to unwind to: none (0) makes
        // `longjmp` restore the registers only, as elsewhere. On arm64
        // ucrtbase's always unwinds, so the runtime's own pair is used.
        if self.c.t.os == "windows" && self.c.t.arch == "aarch64" {
            self.call_rt("nabi_setjmp", &[(V::M(jb), w)], Some((got, K::I32)));
        } else if self.c.t.os == "windows" && self.c.t.arch != "x86" {
            self.call_rt(
                "_setjmp",
                &[(V::M(jb), w), (V::I(0), w)],
                Some((got, K::I32)),
            );
        } else {
            self.call_rt("_setjmp", &[(V::M(jb), w)], Some((got, K::I32)));
        }
        // `Br` tests an `I8`: the `int`'s low byte is its first only on a
        // little-endian target (s390x read the high one, 0, and ran the
        // body again after a recovery).
        let again = self.tmp_k(K::I8);
        self.push(Inst::Cmp {
            cc: crate::lir::Cc::Ne,
            k: K::I32,
            signed: false,
            dst: again,
            a: V::M(got),
            b: V::I(0),
        });
        let (back, body) = (self.label(), self.label());
        self.push(Inst::Br {
            c: V::M(again),
            t: back,
            f: body,
        });
        self.push(Inst::Label(body));
        (rec, back)
    }

    /// `v` (an `#[on_panic]` value) as a `ty` at `m`.
    fn store_const(&mut self, v: &ConstVal, ty: &MirTy, m: Mem) {
        let size = self.c.t.size_align(ty).0;
        self.push(Inst::Zero { dst: m, size });
        let pw = self.c.t.pointer_width;
        let (k, bits) = match (v, ty) {
            (ConstVal::Bool(b), _) => (K::I8, i64::from(*b)),
            (ConstVal::Int { value, ty: p }, MirTy::Prim(_)) => (cc::prim_k(*p, pw), *value as i64),
            (ConstVal::UInt { value, ty: p }, MirTy::Prim(_)) => {
                (cc::prim_k(*p, pw), *value as i64)
            }
            (
                ConstVal::Float {
                    bits,
                    ty: PrimTy::F32,
                },
                _,
            ) => (K::I32, *bits as i64),
            (ConstVal::Float { bits, .. }, _) => (K::I64, *bits as i64),
            // Null: the zeroed pointer.
            _ => return,
        };
        self.push(Inst::Mov {
            k,
            dst: m,
            src: V::I(bits),
        });
    }

    /// The C `main`: runtime up, types registered, the user `main`, runtime
    /// down; its result as the exit code.
    pub fn main_wrapper(mut self, user: SymId, ret: &MirTy, register: SymId) -> Func {
        if self.c.obj.wasm.is_some() {
            let once = self.c.sym("nabi_init_once");
            self.call_sym(once, &[], &MirTy::Unit, &[], None);
        } else {
            let init = self.c.sym("nabi_runtime_init");
            self.call_sym(init, &[], &MirTy::Unit, &[], None);
            self.call_sym(register, &[], &MirTy::Unit, &[], None);
        }
        let r = (*ret != MirTy::Unit).then(|| self.tmp(ret));
        self.call_tls(user, ret, r);
        let down = self.c.sym("nabi_runtime_shutdown");
        self.call_sym(down, &[], &MirTy::Unit, &[], None);
        let part = Part {
            loc: Loc::Int(0),
            k: K::I32,
            signed: true,
        };
        let code = match (ret, r) {
            (MirTy::Prim(p), Some(m)) if p.is_int() || *p == PrimTy::Bool => {
                let pw = self.c.t.pointer_width;
                let k = cc::prim_k(*p, pw);
                let signed = p.is_signed_int();
                let out = self.tmp_k(K::I32);
                self.push(Inst::Conv {
                    conv: crate::lir::Conv::Int { signed },
                    from: k,
                    to: K::I32,
                    dst: out,
                    a: V::M(m),
                });
                V::M(out)
            }
            (MirTy::Unit, _) => V::I(0),
            _ => {
                self.c.errors.push((
                    nabic_diagnostic::Span::DUMMY,
                    format!("unsupported in codegen: `main` returning `{ret:?}`"),
                ));
                V::I(0)
            }
        };
        self.push(Inst::Ret {
            vals: vec![(part, code)],
        });
        self.out
    }

    /// wasm32 `nabi_init_once`: the runtime up and the types registered
    /// on the first call (from `_start` or the first export called).
    pub fn init_once(mut self, inited: SymId, register: SymId) -> Func {
        let p = self.word_tmp();
        self.push(Inst::Sym {
            dst: p,
            sym: inited,
            got: false,
        });
        let Mem::Frame(ps, _) = p else { unreachable!() };
        let z = self.tmp_k(K::I8);
        self.push(Inst::Cmp {
            cc: crate::lir::Cc::Eq,
            k: K::I8,
            signed: false,
            dst: z,
            a: V::M(Mem::Ind(ps, 0)),
            b: V::I(0),
        });
        let (run, done) = (self.label(), self.label());
        self.push(Inst::Br {
            c: V::M(z),
            t: run,
            f: done,
        });
        self.push(Inst::Label(run));
        self.push(Inst::Mov {
            k: K::I8,
            dst: Mem::Ind(ps, 0),
            src: V::I(1),
        });
        let init = self.c.sym("nabi_runtime_init");
        self.call_sym(init, &[], &MirTy::Unit, &[], None);
        self.call_sym(register, &[], &MirTy::Unit, &[], None);
        self.push(Inst::Jmp(done));
        self.push(Inst::Label(done));
        self.push(Inst::Ret { vals: Vec::new() });
        self.out
    }

    /// A REPL cell's entry: register its types and roots, run its `main`.
    pub fn repl_cell(mut self, user: SymId, ret: &MirTy, register: SymId) -> Func {
        self.call_sym(register, &[], &MirTy::Unit, &[], None);
        let r = (*ret != MirTy::Unit).then(|| self.tmp(ret));
        self.call_tls(user, ret, r);
        self.push(Inst::Ret { vals: Vec::new() });
        self.out
    }

    /// `@sym.init(at)`: runs the initializer once across threads (0 new,
    /// 1 running, 2 ready; `nabi_static_init_begin` / `_end`); `at` is the
    /// reading code's location, for the panic of a recursive one.
    pub fn lazy_init(
        mut self,
        state: SymId,
        storage: SymId,
        got: bool,
        init: SymId,
        ty: &MirTy,
    ) -> Func {
        let w = MirTy::Prim(PrimTy::Usize);
        let str_ty = MirTy::Prim(PrimTy::Str);
        let shapes = [cc::shape(&str_ty, self.c.t)];
        let locs = cc::assign(self.c.conv, &shapes, None, false, None);
        self.out.sig = Some((locs.params.concat(), locs.ret.clone()));
        let at = self.tmp_bytes(16, 16);
        for (part, l) in locs.params[0].iter().zip(&shapes[0].leaves) {
            self.push(Inst::Param {
                part: *part,
                dst: at.at(l.off),
            });
        }
        let p = self.word_tmp();
        self.push(Inst::Sym {
            dst: p,
            sym: state,
            got,
        });
        let pv = self.val_of(p, &w);
        let ap = self.val_of(at, &w);
        let al = self.val_of(at.at(self.c.t.pointer_width / 8), &w);
        let r = self.tmp_k(K::I32);
        let begin = self.c.sym("nabi_static_init_begin");
        self.call_sym(
            begin,
            &[w.clone(), w.clone(), w.clone()],
            &MirTy::Prim(PrimTy::I32),
            &[pv, ap, al],
            Some(r),
        );
        let (run, done) = (self.label(), self.label());
        let ne = self.tmp_k(K::I8);
        self.push(Inst::Cmp {
            cc: crate::lir::Cc::Ne,
            k: K::I32,
            signed: false,
            dst: ne,
            a: V::M(r),
            b: V::I(0),
        });
        self.push(Inst::Br {
            c: V::M(ne),
            t: run,
            f: done,
        });
        self.push(Inst::Label(run));
        let v = (*ty != MirTy::Unit).then(|| self.tmp(ty));
        self.call_tls(init, ty, v);
        if let Some(v) = v {
            let a = self.word_tmp();
            self.push(Inst::Sym {
                dst: a,
                sym: storage,
                got,
            });
            let Mem::Frame(s, _) = a else { unreachable!() };
            let size = self.c.t.size_align(ty).0;
            if size > 0 {
                self.push(Inst::Copy {
                    dst: Mem::Ind(s, 0),
                    src: v,
                    size,
                });
            }
        }
        let end = self.c.sym("nabi_static_init_end");
        self.call_sym(end, &[w], &MirTy::Unit, &[pv], None);
        self.push(Inst::Jmp(done));
        self.push(Inst::Label(done));
        self.push(Inst::Ret { vals: Vec::new() });
        self.out
    }

    /// `nabi.register_types`: the `ScanTable`s (by symbol and type id),
    /// or `list`, the type ids from `base` with an array of their
    /// tables (`(base, array, count)`); the static roots (`(symbol,
    /// local, offset)`), and the prebuilt dependencies' own
    /// registrations.
    pub fn register_types(
        mut self,
        tables: &[(u32, SymId)],
        list: Option<(u32, SymId, u32)>,
        roots: &[(SymId, bool, u32)],
        deps: &[SymId],
    ) -> Func {
        let w = MirTy::Prim(PrimTy::Usize);
        let i32_ = MirTy::Prim(PrimTy::I32);
        if let Some((base, arr, n)) = list {
            let regs = self.c.sym("nabi_gc_register_types");
            let p = self.word_tmp();
            self.push(Inst::Sym {
                dst: p,
                sym: arr,
                got: false,
            });
            let pv = self.val_of(p, &w);
            let k = self.word();
            let args = [
                Self::imm(i64::from(base), K::I32),
                pv,
                Self::imm(i64::from(n), k),
            ];
            self.call_sym(
                regs,
                &[i32_.clone(), w.clone(), w.clone()],
                &MirTy::Unit,
                &args,
                None,
            );
        }
        let tables = if list.is_some() { &[][..] } else { tables };
        let reg = self.c.sym("nabi_gc_register_type");
        for &(id, sym) in tables {
            let p = self.word_tmp();
            self.push(Inst::Sym {
                dst: p,
                sym,
                got: false,
            });
            let pv = self.val_of(p, &w);
            let idv = Self::imm(i64::from(id), K::I32);
            self.call_sym(
                reg,
                &[i32_.clone(), w.clone()],
                &MirTy::Unit,
                &[idv, pv],
                None,
            );
        }
        let add = self.c.sym("nabi_gc_add_root");
        for &(sym, local, off) in roots {
            let p = self.word_tmp();
            self.push(Inst::Sym {
                dst: p,
                sym,
                got: !local,
            });
            if off != 0 {
                let k = self.word();
                self.push(Inst::Bin {
                    op: crate::lir::Bin::Add,
                    k,
                    signed: false,
                    dst: p,
                    a: V::M(p),
                    b: V::I(i64::from(off)),
                });
            }
            let pv = self.val_of(p, &w);
            self.call_sym(add, std::slice::from_ref(&w), &MirTy::Unit, &[pv], None);
        }
        for &d in deps {
            self.call_sym(d, &[], &MirTy::Unit, &[], None);
        }
        self.push(Inst::Ret { vals: Vec::new() });
        self.out
    }
}

/// Lower generated function `name` with `build`; under `-g` a
/// subprogram at `span` (a dummy one: line 0 of the unit's file).
pub fn generated(
    c: &mut Ctx<'_>,
    name: &str,
    sym: SymId,
    span: nabic_diagnostic::Span,
    build: impl FnOnce(FnLower<'_, '_>) -> Func,
) -> Func {
    let shell = shell(name);
    let mut f = build(FnLower::new(c, &shell, sym));
    if c.debug.is_some() {
        f.debug = Some(crate::lir::FnDebug {
            name: name.to_owned(),
            file: if span.line == 0 { 0 } else { span.file },
            line: span.line,
            scopes: Vec::new(),
            vars: Vec::new(),
            ret: None,
            opt: false,
            inlined: Vec::new(),
        });
    }
    f
}
