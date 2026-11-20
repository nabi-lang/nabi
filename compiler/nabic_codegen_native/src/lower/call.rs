//! Calls: nabi bodies, the runtime and the C ABI.

#[allow(clippy::wildcard_imports)]
use crate::lower::*;

impl FnLower<'_, '_> {
    /// A nabi-ABI call (`callee` takes `params`, returns `ret`) of `args`;
    /// the result into `dst`.
    pub(crate) fn call_sym(
        &mut self,
        sym: SymId,
        params: &[MirTy],
        ret: &MirTy,
        args: &[Val],
        dst: Option<Mem>,
    ) {
        self.call_nabi(Callee::Sym(sym), params, ret, args, dst, How::default());
    }

    /// A call of the nabi body `sig` (`call_sym`, `nabi_tls` passed if it
    /// takes it).
    pub(crate) fn call_body(&mut self, sig: &Sig, args: &[Val], dst: Option<Mem>) {
        self.call_nabi(
            Callee::Sym(sig.sym),
            &sig.params,
            &sig.ret,
            args,
            dst,
            How {
                tls: sig.tls,
                keeps_none: sig.summary.keeps_none,
                returns: sig.summary.returns,
            },
        );
    }

    /// A call of a nabi body `sym` without arguments that takes `nabi_tls`
    /// (the user's `main`, a static's initializer).
    pub(crate) fn call_tls(&mut self, sym: SymId, ret: &MirTy, dst: Option<Mem>) {
        self.call_nabi(
            Callee::Sym(sym),
            &[],
            ret,
            &[],
            dst,
            How {
                tls: true,
                ..How::default()
            },
        );
    }

    /// Call a runtime function codegen uses itself: `call_sym`, with `str`
    /// passed as a pointer to a copy on Windows x64, s390x and wasm32
    /// (`abi::classify_nabi`).
    pub(crate) fn call_runtime(
        &mut self,
        sym: SymId,
        params: &[MirTy],
        ret: &MirTy,
        args: &[Val],
        dst: Option<Mem>,
    ) {
        if !matches!(
            self.c.t.c_abi,
            nabic_mir::CAbi::X86_64Win | nabic_mir::CAbi::S390x | nabic_mir::CAbi::Wasm32
        ) {
            return self.call_sym(sym, params, ret, args, dst);
        }
        let str_ty = MirTy::Prim(PrimTy::Str);
        let word = MirTy::Prim(PrimTy::Usize);
        let mut ps = Vec::with_capacity(params.len());
        let mut vs = Vec::with_capacity(args.len());
        for (ty, v) in params.iter().zip(args) {
            if *ty == str_ty {
                let (size, align) = self.c.t.size_align(ty);
                let m = self.tmp_bytes(size, align);
                self.store(*v, ty, m);
                let p = self.word_tmp();
                self.push(Inst::Addr { dst: p, src: m });
                ps.push(word.clone());
                vs.push(Val::S(V::M(p), self.word()));
            } else {
                ps.push(ty.clone());
                vs.push(*v);
            }
        }
        self.call_sym(sym, &ps, ret, &vs, dst);
    }

    pub(crate) fn arg_parts(&self, v: Val, ty: &MirTy, parts: &[Part], out: &mut Vec<(Part, V)>) {
        let sh = cc::shape(ty, self.c.t);
        match v {
            Val::S(src, _) => {
                if let Some(p) = parts.first() {
                    out.push((*p, src));
                }
            }
            Val::M(m) => {
                let virt = match m {
                    Mem::Frame(s, base) => self.virt.get(&s).map(|v| (v, base)),
                    Mem::Ind(..) => None,
                };
                for (p, l) in parts.iter().zip(&sh.leaves) {
                    let v = virt
                        .and_then(|(v, base)| v.iter().find(|(o, _)| *o == base + l.off))
                        .map_or_else(|| V::M(m.at(l.off)), |x| x.1);
                    out.push((*p, v));
                }
            }
            Val::Unit => {}
        }
    }

    /// Shapes of a nabi signature: by-address arguments are pointer words; a
    /// by-address result comes through a leading pointer (`true`).
    pub(crate) fn nabi_shapes(
        &self,
        params: &[MirTy],
        ret: &MirTy,
        tls: bool,
    ) -> (Vec<Shape>, Option<Shape>, bool) {
        let t = self.c.t;
        let lead = nabi_by_ref(ret, t);
        let mut shapes = Vec::new();
        if lead {
            shapes.push(cc::coerced(&CType::PTR, t.pointer_width));
        }
        shapes.extend(params.iter().map(|p| {
            if nabi_by_ref(p, t) {
                cc::coerced(&CType::PTR, t.pointer_width)
            } else {
                cc::shape(p, t)
            }
        }));
        if tls {
            shapes.push(cc::coerced(&CType::PTR, t.pointer_width));
        }
        let rs = (*ret != MirTy::Unit && !lead).then(|| cc::shape(ret, t));
        (shapes, rs, lead)
    }

    /// The address of `v` (a `ty` value) as a pointer argument.
    pub(crate) fn addr_arg(&mut self, v: Val, ty: &MirTy) -> Val {
        let m = match v {
            Val::M(m) => m,
            v => {
                let m = self.tmp(ty);
                self.store(v, ty, m);
                m
            }
        };
        let p = self.word_tmp();
        self.push(Inst::Addr { dst: p, src: m });
        Val::S(V::M(p), self.word())
    }

    pub(crate) fn call_nabi(
        &mut self,
        callee: Callee,
        params: &[MirTy],
        ret: &MirTy,
        args: &[Val],
        dst: Option<Mem>,
        how: How,
    ) {
        let How {
            tls,
            keeps_none,
            returns,
        } = how;
        let t = self.c.t;
        let (shapes, rs, lead) = self.nabi_shapes(params, ret, tls);
        let locs = cc::assign(self.c.conv, &shapes, rs.as_ref(), false, None);
        let dst = match (dst, ret) {
            (_, MirTy::Unit) => None,
            (Some(d), _) => Some(d),
            (None, r) => Some(self.tmp(r)),
        };
        let word = MirTy::Prim(PrimTy::Usize);
        let mut vals: Vec<(Val, &MirTy)> = Vec::new();
        // The by-address ones: the callee copies an argument in and
        // writes the result through, keeping neither pointer.
        let mut byref = Vec::new();
        if let (true, Some(d)) = (lead, dst) {
            let p = self.word_tmp();
            self.push(Inst::Addr { dst: p, src: d });
            byref.push((vals.len(), Cap::Ret));
            vals.push((Val::S(V::M(p), self.word()), &word));
        }
        for (i, (v, ty)) in args.iter().zip(params).enumerate() {
            if nabi_by_ref(ty, t) {
                // The callee may read the argument in place while it
                // writes the result: never the same memory.
                let v = match (*v, dst) {
                    (Val::M(m), Some(d)) if lead && same_base(m, d) => {
                        let c = self.tmp(ty);
                        self.store(Val::M(m), ty, c);
                        Val::M(c)
                    }
                    (v, _) => v,
                };
                let p = self.addr_arg(v, ty);
                byref.push((vals.len(), Cap::No));
                vals.push((p, &word));
            } else {
                // A pointer in an argument the callee's summary says it
                // does not keep.
                if i < 64 && (keeps_none >> i) & 1 != 0 {
                    let cap = if (returns >> i) & 1 != 0 {
                        Cap::Ret
                    } else {
                        Cap::No
                    };
                    byref.push((vals.len(), cap));
                }
                vals.push((*v, ty));
            }
        }
        if tls {
            let v = self.tls_val();
            vals.push((v, &word));
        }
        if let Callee::Sym(s) = &callee {
            let s = *s;
            for (n, cap) in byref {
                if let Some(p) = locs.params.get(n).and_then(|p| p.first()) {
                    self.out.byref.push((s, p.loc, cap));
                }
            }
        }
        let mut a = Vec::new();
        for ((v, ty), parts) in vals.into_iter().zip(&locs.params) {
            self.arg_parts(v, ty, parts, &mut a);
        }
        let dst = if lead { None } else { dst };
        let (rets, sret) = match (&locs.ret, dst, &rs) {
            (Some(parts), Some(d), Some(sh)) => {
                let to = match (&callee, self.ret_to.take()) {
                    (Callee::Sym(s), Some((r, to))) if *s == r => to,
                    (_, keep) => {
                        self.ret_to = keep;
                        Vec::new()
                    }
                };
                let at = |off: u32| to.iter().find(|(o, _)| *o == off).map(|x| x.1);
                (
                    parts
                        .iter()
                        .zip(&sh.leaves)
                        .map(|(p, l)| (*p, at(l.off).unwrap_or_else(|| d.at(l.off))))
                        .collect(),
                    None,
                )
            }
            (None, Some(d), _) => (Vec::new(), Some(d)),
            _ => (Vec::new(), None),
        };
        self.push(Inst::Call {
            callee,
            args: a,
            rets,
            sret,
            variadic: false,
            pop: locs.pop,
        });
        if let (MirTy::Prim(PrimTy::Bool), Some(d)) = (ret, dst) {
            self.normalize_bool(d);
        }
    }

    /// `x = t.k` right after `t = call f(.., x, ..)` where `f` hands `x`
    /// back in field `k` changing only some fields (`Summary::echo`):
    /// those fields are written back, the rest stay. `x` keeps its
    /// value through the call (no death clear); nothing between may
    /// write either local or reach a safepoint.
    pub(crate) fn writebacks(&mut self) {
        // A hot build may swap the callee for one changing more.
        if self.c.hot.is_some() {
            return;
        }
        let f = self.f;
        let t = self.c.t;
        let (quiet, pkgs) = (self.c.quiet, self.c.pkgs);
        for (pos, b) in f.blocks.iter().enumerate() {
            for (i, s) in b.stmts.iter().enumerate() {
                let Statement::Assign {
                    target: tl,
                    rvalue: RValue::Call { callee, args, .. },
                } = s
                else {
                    continue;
                };
                let Some(e) = self.c.fns.get(callee).map(|s| s.summary.echo) else {
                    continue;
                };
                let Some(Operand::Copy(a)) = args.get(e.param as usize).filter(|_| e.field != 0)
                else {
                    continue;
                };
                let (a, k) = (*a, e.field - 1);
                let aty = self.local_ty(a);
                if !nabi_by_ref(&aty, t)
                    || a == *tl
                    || self.addressed.contains(&a)
                    || self.in_place.contains_key(&a)
                    || self.ro.contains_key(&a)
                    || !matches!(self.local_ty(*tl), MirTy::Tuple(ref fs) if fs.get(k as usize) == Some(&aty))
                {
                    continue;
                }
                for (j, s2) in b.stmts.iter().enumerate().skip(i + 1) {
                    if let Statement::Assign {
                        target,
                        rvalue:
                            RValue::FieldRead {
                                target: Operand::Copy(x),
                                index,
                                ..
                            },
                    } = s2
                    {
                        if *target == a && x == tl && *index == k {
                            if !self.fuse.skip.contains(&(pos, j)) {
                                self.wb.insert((pos, j), (*tl, k, e.changed));
                                let ds = self.deaths.after_stmt.get_mut(pos);
                                for d in ds.into_iter().flat_map(|d| &mut d[i..j]) {
                                    d.retain(|l| *l != a);
                                }
                            }
                            break;
                        }
                    }
                    let def = match s2 {
                        Statement::Assign { target, .. } => Some(*target),
                        Statement::ArrayWrite { array, .. } | Statement::StorageDead(array) => {
                            Some(*array)
                        }
                        _ => None,
                    };
                    if def == Some(a)
                        || def == Some(*tl)
                        || nabic_codegen::analysis::stmt_safepoint(s2, quiet, pkgs, t)
                    {
                        break;
                    }
                }
            }
        }
    }

    /// The write-back `writebacks` planned for statement `s`.
    pub(crate) fn writeback(&mut self, s: &Statement, pos: usize, i: usize) {
        let (Some(&(tl, k, changed)), Statement::Assign { target, .. }) =
            (self.wb.get(&(pos, i)), s)
        else {
            return self.stmt(s);
        };
        let t = self.c.t;
        let ty = self.local_ty(*target);
        let (Some(src), Some(dst)) = (self.local_mem(tl), self.local_mem(*target)) else {
            return self.stmt(s);
        };
        let src = src.at(t.field_offsets(&self.local_ty(tl))[k as usize]);
        let offs = t.field_offsets(&ty);
        for (n, (fty, off)) in struct_fields(&ty).iter().zip(offs).enumerate() {
            let n = n as u32;
            if changed & (1 << n.min(63)) != 0 {
                let v = self.val_of(src.at(off), fty);
                self.store(v, fty, dst.at(off));
            }
        }
    }

    /// Keep bit 0 of a `bool` another compiler handed over (only that bit
    /// is defined).
    pub(crate) fn normalize_bool(&mut self, m: Mem) {
        self.push(Inst::Bin {
            op: Bin::And,
            k: K::I8,
            signed: false,
            dst: m,
            a: V::M(m),
            b: V::I(1),
        });
    }

    /// The two bytes of a half at `m` as the `f32` bits that carry it
    /// to `at`: the low half of a float register's four, NaN-boxed (the
    /// rest all ones) on RISC-V; right-justified in an s390x stack slot.
    pub(crate) fn widen_half(&mut self, m: Mem, at: Loc) -> V {
        let w = self.tmp_bytes(4, 4);
        let (half, rest) = self.half_at(w, at);
        self.push(Inst::Mov {
            k: K::I16,
            dst: half,
            src: V::M(m),
        });
        let fill = if matches!(self.c.conv, cc::Conv::Riscv64 | cc::Conv::Riscv32) {
            0xffff
        } else {
            0
        };
        self.push(Inst::Mov {
            k: K::I16,
            dst: rest,
            src: V::I(fill),
        });
        V::M(w)
    }

    /// Where a half and the rest are in the four bytes `w` going to or
    /// coming from `at`.
    pub(crate) fn half_at(&self, w: Mem, at: Loc) -> (Mem, Mem) {
        if self.c.conv == cc::Conv::S390x && matches!(at, Loc::Stack(_)) {
            (w.at(2), w)
        } else {
            (w, w.at(2))
        }
    }

    /// A call under the C ABI.
    pub(crate) fn call_c(
        &mut self,
        callee: Callee,
        params: &[MirTy],
        ret: &MirTy,
        abi: &FnAbi,
        args: &[Operand],
        dst: Option<Mem>,
    ) {
        let t = self.c.t;
        let str_ty = MirTy::Prim(PrimTy::Str);
        let mut shapes: Vec<Shape> = Vec::new();
        let mut srcs: Vec<Vec<V>> = Vec::new();
        let mut c_strs: Vec<(Mem, Mem)> = Vec::new();
        // By-address copies: the callee's own parameters, gone when it
        // returns.
        let mut copies = Vec::new();
        let mut halves = Vec::new();
        for (i, op) in args.iter().enumerate() {
            let ty = params.get(i).cloned().unwrap_or_else(|| self.op_ty(op));
            let mut v = self.val(op, &ty);
            let mut ty = ty;
            if ty == str_ty && !abi.nabi {
                let s = self.mem_of(v, &str_ty);
                let p = self.word_tmp();
                let sym = self.c.sym("nabi_str_cstr");
                self.call_runtime(
                    sym,
                    std::slice::from_ref(&str_ty),
                    &MirTy::Prim(PrimTy::Usize),
                    &[Val::M(s)],
                    Some(p),
                );
                c_strs.push((s, p));
                v = Val::S(V::M(p), self.word());
                ty = MirTy::Prim(PrimTy::Usize);
            }
            let variadic = abi.variadic && i >= params.len();
            match abi.args.get(i).unwrap_or(&ArgAbi::Direct) {
                ArgAbi::Direct => {
                    let mut sh = cc::shape(&ty, t);
                    if variadic {
                        // C default promotions: narrow integers to a word,
                        // `f32` to `f64`.
                        if let (Val::S(src, k), Some(l)) = (v, sh.leaves.first_mut()) {
                            let w = self.word();
                            let (nk, conv) = if k.is_float() {
                                (K::F64, Conv::FToF)
                            } else if k.size() >= w.size() {
                                (k, Conv::Int { signed: l.signed })
                            } else {
                                (w, Conv::Int { signed: l.signed })
                            };
                            if nk != k {
                                let m = self.tmp_k(nk);
                                self.push(Inst::Conv {
                                    conv,
                                    from: k,
                                    to: nk,
                                    dst: m,
                                    a: src,
                                });
                                v = Val::S(V::M(m), nk);
                            }
                            l.k = nk;
                        }
                    }
                    let vs = match v {
                        Val::S(src, _) => vec![src],
                        Val::M(m) => sh.leaves.iter().map(|l| V::M(m.at(l.off))).collect(),
                        Val::Unit => Vec::new(),
                    };
                    shapes.push(sh);
                    srcs.push(vs);
                }
                ArgAbi::Coerce(c) => {
                    let sh = cc::coerced(c, t.pointer_width);
                    let size = t.size_align(&ty).0.max(16);
                    let m = self.tmp_bytes(size, 16);
                    self.push(Inst::Zero { dst: m, size });
                    self.store(v, &ty, m);
                    // Halves widen once their places are known.
                    for (j, (l, h)) in sh.leaves.iter().zip(cc::half_leaves(c)).enumerate() {
                        if h {
                            halves.push((srcs.len(), j, m.at(l.off)));
                        }
                    }
                    srcs.push(sh.leaves.iter().map(|l| V::M(m.at(l.off))).collect());
                    shapes.push(sh);
                }
                ArgAbi::Indirect { byval: true } => {
                    // A copy in the argument area.
                    let (size, align) = slot_layout(&ty, t);
                    let sh = cc::byval(
                        size,
                        nabic_codegen::abi::byval_align(&ty, t),
                        t.pointer_width,
                    );
                    let m = self.tmp_bytes(size.next_multiple_of(8), align.max(8));
                    self.store(v, &ty, m);
                    srcs.push(sh.leaves.iter().map(|l| V::M(m.at(l.off))).collect());
                    shapes.push(sh);
                }
                ArgAbi::Indirect { byval: false } => {
                    let (size, align) = slot_layout(&ty, t);
                    let align = if nabic_codegen::abi::is_vector(&ty, t) {
                        16
                    } else {
                        align.max(8)
                    };
                    let m = self.tmp_bytes(size, align);
                    self.store(v, &ty, m);
                    let p = self.word_tmp();
                    self.push(Inst::Addr { dst: p, src: m });
                    shapes.push(cc::coerced(&CType::PTR, t.pointer_width));
                    copies.push(srcs.len());
                    srcs.push(vec![V::M(p)]);
                }
            }
        }
        let ret_shape = match &abi.ret {
            RetAbi::Direct if *ret != MirTy::Unit => Some(cc::shape(ret, t)),
            RetAbi::Coerce(c) => Some(cc::coerced(c, t.pointer_width)),
            _ => None,
        };
        let variadic_from = abi.variadic.then_some(params.len());
        let locs = cc::assign(
            self.c.conv,
            &shapes,
            ret_shape.as_ref(),
            abi.ret == RetAbi::Sret,
            variadic_from,
        );
        for (p, j, m) in halves {
            let at = locs.params[p][j].loc;
            srcs[p][j] = self.widen_half(m, at);
        }
        if let Callee::Sym(s) = &callee {
            for n in copies {
                if let Some(p) = locs.params.get(n).and_then(|p| p.first()) {
                    self.out.byref.push((*s, p.loc, Cap::No));
                }
            }
        }
        let mut a = Vec::new();
        for (parts, vs) in locs.params.iter().zip(&srcs) {
            for (p, v) in parts.iter().zip(vs) {
                a.push((*p, *v));
            }
        }
        let out = match (dst, ret) {
            (_, MirTy::Unit) => None,
            (Some(d), _) => Some(d),
            (None, r) => Some(self.tmp(r)),
        };
        let mut rets = Vec::new();
        let mut sret = None;
        let mut coerce_back: Option<(Mem, Mem)> = None;
        let mut half_rets = Vec::new();
        match &abi.ret {
            RetAbi::Sret => {
                let m = match out {
                    Some(m) => m,
                    None => self.tmp(ret),
                };
                sret = Some(m);
            }
            RetAbi::Coerce(c) => {
                let size = t.size_align(ret).0.max(16);
                let m = self.tmp_bytes(size, 16);
                if let (Some(parts), Some(sh)) = (&locs.ret, &ret_shape) {
                    for ((p, l), h) in parts.iter().zip(&sh.leaves).zip(cc::half_leaves(c)) {
                        if h {
                            let w = self.tmp_bytes(4, 4);
                            half_rets.push((w, m.at(l.off)));
                            rets.push((*p, w));
                        } else {
                            rets.push((*p, m.at(l.off)));
                        }
                    }
                }
                if let Some(o) = out {
                    coerce_back = Some((m, o));
                }
            }
            RetAbi::Direct => {
                if let (Some(parts), Some(sh), Some(o)) = (&locs.ret, &ret_shape, out) {
                    rets = parts
                        .iter()
                        .zip(&sh.leaves)
                        .map(|(p, l)| (*p, o.at(l.off)))
                        .collect();
                }
            }
        }
        // The thread parks for a C call, except into a `nabi_*` runtime symbol
        // declared `extern "C"` (it may allocate).
        // A `#[leaf]` call runs as a mutator: a debug nabic checks that it
        // never stops for a collection.
        let parked = !abi.nabi
            && !self.cfn
            && !abi.leaf
            && !matches!(callee, Callee::Sym(s) if self.c.obj.syms[s as usize].name.starts_with("nabi_"));
        let nosp = abi.leaf && !self.cfn && cfg!(debug_assertions);
        if parked {
            self.native_edge(true);
        }
        if nosp {
            self.call_rt("nabi_nosp_enter", &[], None);
        }
        let pop = if abi.stdcall { locs.stack } else { locs.pop };
        self.push(Inst::Call {
            callee,
            args: a,
            rets,
            sret,
            variadic: abi.variadic,
            pop,
        });
        // Before anything else runs: leaving native may wait out a
        // collection, and unpinning calls the runtime.
        if abi.errno {
            self.call_rt("nabi_errno_save", &[], None);
        }
        if nosp {
            self.call_rt("nabi_nosp_leave", &[], None);
        }
        if parked {
            self.native_edge(false);
        }
        for (s, p) in c_strs {
            let sym = self.c.sym("nabi_str_cstr_done");
            let w = MirTy::Prim(PrimTy::Usize);
            self.call_runtime(
                sym,
                &[str_ty.clone(), w],
                &MirTy::Unit,
                &[Val::M(s), Val::S(V::M(p), self.word())],
                None,
            );
        }
        for (w, d) in half_rets {
            self.push(Inst::Mov {
                k: K::I16,
                dst: d,
                src: V::M(w),
            });
        }
        if let Some((m, o)) = coerce_back {
            let size = t.size_align(ret).0;
            self.push(Inst::Copy {
                dst: o,
                src: m,
                size,
            });
        }
        if let (MirTy::Prim(PrimTy::Bool), Some(o), RetAbi::Direct) = (ret, out, &abi.ret) {
            self.normalize_bool(o);
        }
    }
}

/// Whether `a` and `b` may be the same memory: one slot, or through one
/// pointer.
const fn same_base(a: Mem, b: Mem) -> bool {
    match (a, b) {
        (Mem::Frame(x, _), Mem::Frame(y, _)) | (Mem::Ind(x, _), Mem::Ind(y, _)) => x == y,
        _ => false,
    }
}
