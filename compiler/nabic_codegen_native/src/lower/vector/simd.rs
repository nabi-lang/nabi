//! `core::simd` calls as vector code. Where the emitter has straight-line
//! vector code (`Inst::V`), each register's worth of lanes is one
//! instruction per step, its operands frame parts a register may hold;
//! elsewhere a `VLoop` over the `N` lanes reads the operands where they
//! are (frame slots, by address) and writes the result in place. Loads,
//! stores and splats are plain copies and moves where there is neither.
//! A call the target's vector unit cannot do (the lane type or op, a
//! vector of fewer bytes than a register) stays a call of the lane loop.

use crate::cc;
use crate::lir::{Slot, Vx, VxOp};
#[allow(clippy::wildcard_imports)]
use crate::lower::vector::*;
use nabic_ir_common::SimdOp;

/// The vector bytes of `Inst::V` where the emitter for `isa` has it
/// (`x86_64`: SSE2, as nothing between them clears the upper halves).
pub(crate) const fn straight(isa: Isa) -> Option<u32> {
    match isa {
        Isa::A64 | Isa::X64 { .. } | Isa::Wasm => Some(16),
        _ => None,
    }
}

/// Whether the emitter for `isa` has `VxOp::Insert` and `VxOp::Lane` on
/// lanes of `w` bytes (SSE2: of 4 and 8).
pub(crate) const fn inserts(isa: Isa, w: u32) -> bool {
    match isa {
        Isa::X64 { .. } => w == 4 || w == 8,
        Isa::A64 | Isa::Wasm => true,
        _ => false,
    }
}

impl FnLower<'_, '_> {
    /// Lower `op` on `args` (of types `params`) into `dst`; `false`
    /// when it stays a call.
    pub(crate) fn simd_call(
        &mut self,
        op: SimdOp,
        params: &[MirTy],
        ret: &MirTy,
        args: &[Val],
        dst: Option<Mem>,
    ) -> bool {
        if matches!(op, SimdOp::Load | SimdOp::Store | SimdOp::Splat) {
            return self.simd_move(op, params, ret, args, dst);
        }
        if op == SimdOp::Sum {
            return self.simd_sum(params, args, dst);
        }
        let Some(isa) = Isa::of(self.c.t) else {
            return false;
        };
        let (Some(d), Some((p, n))) = (dst, params.first().and_then(lanes_of)) else {
            return false;
        };
        let pw = self.c.t.pointer_width;
        if p == PrimTy::Bool || !(p.is_int() || p.is_float()) {
            return false;
        }
        let w = p.bit_width(pw) / 8;
        let bytes = n * w;
        if let Some(sv) = straight(isa).filter(|sv| bytes % sv == 0 && bytes > 0) {
            let mut srcs = Vec::with_capacity(args.len());
            for a in args {
                let Val::M(m) = a else { return false };
                srcs.push(*m);
            }
            // The SSE2 forms: what `simd_body` picks for 16 bytes.
            let isa = match isa {
                Isa::X64 { win, .. } => Isa::X64 { win, avx2: false },
                i => i,
            };
            let Some((pre, body, _)) = simd_body(isa, op, p, w, args.len()) else {
                return false;
            };
            return self.simd_straight(w, sv, bytes, &pre, &body, &srcs, d);
        }
        let vb = if isa.bytes() == 32 && bytes % 32 == 0 {
            32
        } else {
            16
        };
        // A 16-byte loop on an AVX2 target is SSE code: what SSE2 has.
        let isa = match isa {
            Isa::X64 { win, avx2: true } if vb == 16 => Isa::X64 { win, avx2: false },
            i => i,
        };
        if bytes == 0 || bytes % vb != 0 || args.len() + 1 > isa.bases() {
            return false;
        }
        let mut srcs = Vec::with_capacity(args.len());
        for a in args {
            let Val::M(m) = a else { return false };
            srcs.push(*m);
        }
        let Some((pre, body, regs)) = simd_body(isa, op, p, w, args.len()) else {
            return false;
        };
        let wk = self.word();
        let nb = args.len() as u32 + 1;
        let data = (VIO_BASES + 8 * nb).next_multiple_of(32);
        let save = data + nb * bytes;
        let io_size = save
            + match isa {
                Isa::X64 { win: true, .. } => 16 * (u32::from(regs).saturating_sub(6) + 1),
                Isa::I686 => 16,
                _ => 0,
            };
        let io = self.out.slot(io_size, 16);
        let cell = |o: u32| Mem::Frame(io, o);
        // A frame operand is read in place; another is copied into the
        // block first. The result is written in place likewise.
        let mut at = Vec::with_capacity(nb as usize);
        for (k, (m, ty)) in srcs.iter().zip(params).enumerate() {
            if matches!(m, Mem::Frame(..)) {
                at.push(*m);
            } else {
                let c = cell(data + k as u32 * bytes);
                self.store(Val::M(*m), ty, c);
                at.push(c);
            }
        }
        let out = if matches!(d, Mem::Frame(..)) {
            d
        } else {
            cell(data + (nb - 1) * bytes)
        };
        at.push(out);
        for (k, m) in at.iter().enumerate() {
            self.push(Inst::Addr {
                dst: cell(VIO_BASES + 8 * k as u32),
                src: *m,
            });
        }
        self.push(Inst::Mov {
            k: wk,
            dst: cell(VIO_IV),
            src: V::I(0),
        });
        self.push(Inst::Mov {
            k: wk,
            dst: cell(VIO_BOUND),
            src: V::I(i64::from(n)),
        });
        self.push(Inst::VLoop(Box::new(VLoop {
            io,
            w,
            vb,
            min_left: vb / w,
            bases: vec![0; nb as usize],
            elem: vec![w; nb as usize],
            pre,
            body,
            post: Vec::new(),
            regs,
            save,
        })));
        if out != d {
            self.store(Val::M(out), ret, d);
        }
        true
    }
}

impl FnLower<'_, '_> {
    /// `pre` then `body` of `simd_body` as `Inst::V`, once per `vb` bytes
    /// of the `bytes` of each operand; `false` for an op it has no form of.
    #[allow(clippy::too_many_arguments)]
    fn simd_straight(
        &mut self,
        w: u32,
        vb: u32,
        bytes: u32,
        pre: &[VOp],
        body: &[VOp],
        srcs: &[Mem],
        d: Mem,
    ) -> bool {
        let ops: Vec<VOp> = pre.iter().chain(body).copied().collect();
        let mut out = None;
        for o in &ops {
            match o {
                VOp::Load { .. } | VOp::Const { .. } | VOp::Bin { .. } | VOp::Un { .. } => {}
                VOp::Store { src, .. } => out = Some(*src),
                _ => return false,
            }
        }
        let Some(out) = out else { return false };
        // The step making the result writes the destination itself.
        let last = ops.iter().rposition(|o| match o {
            VOp::Const { dst, .. } | VOp::Bin { dst, .. } | VOp::Un { dst, .. } => *dst == out,
            _ => false,
        });
        for c in 0..bytes / vb {
            let at = c * vb;
            let mut map: HashMap<VReg, Mem> = HashMap::default();
            for (i, o) in ops.iter().enumerate() {
                let tgt = |l: &mut Self| {
                    if Some(i) == last {
                        d.at(at)
                    } else {
                        Mem::Frame(l.out.slot(vb, 16), 0)
                    }
                };
                let (op, dst, a, b) = match *o {
                    VOp::Load { dst, base } => {
                        map.insert(dst, srcs[base as usize].at(at));
                        continue;
                    }
                    VOp::Store { src, .. } => {
                        let m = map[&src];
                        if m == d.at(at) {
                            continue;
                        }
                        (VxOp::Mov, d.at(at), m, m)
                    }
                    VOp::Const { dst, bits } => {
                        let t = tgt(self);
                        map.insert(dst, t);
                        (VxOp::Const(bits), t, t, t)
                    }
                    VOp::Bin { op, dst, a, b } => {
                        let (a, b) = (map[&a], map[&b]);
                        let t = tgt(self);
                        map.insert(dst, t);
                        (VxOp::Bin(op), t, a, b)
                    }
                    VOp::Un { op, dst, a } => {
                        let a = map[&a];
                        let t = tgt(self);
                        map.insert(dst, t);
                        (VxOp::Un(op), t, a, a)
                    }
                    _ => unreachable!("checked above"),
                };
                self.push(Inst::V(Box::new(Vx {
                    op,
                    w,
                    vb,
                    dst,
                    a,
                    b,
                })));
            }
        }
        true
    }

    /// A copy of a vector (a struct of one array of numbers, or the
    /// array) as vector moves, which leave it a part a register may hold; `false` when
    /// the emitter has none for it.
    pub(crate) fn simd_copy(&mut self, ty: &MirTy, dst: Mem, src: Mem) -> bool {
        let t = self.c.t;
        let Some(vb) = Isa::of(t).and_then(straight) else {
            return false;
        };
        let lanes = match ty {
            MirTy::Array(e, n) => match **e {
                MirTy::Prim(p) => Some((p, *n)),
                _ => None,
            },
            ty => lanes_of(ty),
        };
        let Some((p, n)) = lanes else {
            return false;
        };
        let Some(k) = cc::scalar_k(&MirTy::Prim(p), t) else {
            return false;
        };
        let bytes = n * k.size();
        if p == PrimTy::Bool || bytes == 0 || bytes % vb != 0 {
            return false;
        }
        for c in 0..bytes / vb {
            self.push(Inst::V(Box::new(Vx {
                op: VxOp::Mov,
                w: k.size(),
                vb,
                dst: dst.at(c * vb),
                a: src.at(c * vb),
                b: src.at(c * vb),
            })));
        }
        true
    }

    /// `a = [e0, e1, ..]` feeding a vector (`Simd { lanes: a }`) as the
    /// first element splatted and the rest put in their lanes, a part a
    /// register may hold; `false` to store the elements one by one.
    pub(crate) fn simd_build(
        &mut self,
        a: LocalId,
        elem_ty: &MirTy,
        elements: &[Operand],
        dst: Mem,
    ) -> bool {
        let t = self.c.t;
        let (Some(isa), MirTy::Prim(p)) = (Isa::of(t), elem_ty) else {
            return false;
        };
        let Some(vb) = straight(isa) else {
            return false;
        };
        let Some(k) = cc::scalar_k(elem_ty, t) else {
            return false;
        };
        let w = k.size();
        let bytes = w * elements.len() as u32;
        if *p == PrimTy::Bool
            || !(p.is_int() || p.is_float())
            || bytes == 0
            || bytes % vb != 0
            || !inserts(isa, w)
            || !matches!(dst, Mem::Frame(..))
            || elements.iter().all(|e| matches!(e, Operand::Const(_)))
        {
            return false;
        }
        // Only an array a vector is made of.
        let me = Operand::Copy(a);
        let feeds = self.f.blocks.iter().flat_map(|b| &b.stmts).any(|s| {
            matches!(s, Statement::Assign {
                rvalue: RValue::StructInit { ty, fields },
                ..
            } if lanes_of(ty).is_some() && fields.as_slice() == [me.clone()])
        });
        if !feeds {
            return false;
        }
        let lanes = (vb / w) as usize;
        for (c, chunk) in elements.chunks(lanes).enumerate() {
            let at = dst.at(c as u32 * vb);
            for (n, e) in chunk.iter().enumerate() {
                let m = match self.val(e, elem_ty) {
                    Val::S(V::M(m), _) => m,
                    Val::S(v, _) => {
                        let m = self.tmp_k(k);
                        self.push(Inst::Mov { k, dst: m, src: v });
                        m
                    }
                    _ => return false,
                };
                if n > 0 && chunk[..n].iter().all(|x| x == e) {
                    continue;
                }
                let op = if n == 0 {
                    VxOp::Splat(k)
                } else {
                    VxOp::Insert(k, n as u8)
                };
                self.push(Inst::V(Box::new(Vx {
                    op,
                    w,
                    vb,
                    dst: at,
                    a: if n == 0 { m } else { at },
                    b: m,
                })));
            }
        }
        true
    }

    /// `sum`: the lanes added in order to zero, each taken from its
    /// register where the emitter has that, else read from memory.
    fn simd_sum(&mut self, params: &[MirTy], args: &[Val], dst: Option<Mem>) -> bool {
        let t = self.c.t;
        let (Some((p, n)), [Val::M(v)]) = (params.first().and_then(lanes_of), args) else {
            return false;
        };
        let Some(k) = cc::scalar_k(&MirTy::Prim(p), t) else {
            return false;
        };
        if p == PrimTy::Bool || !(p.is_int() || p.is_float()) {
            return false;
        }
        let Some(d) = dst else { return true };
        let w = k.size();
        let mut v = *v;
        let isa = Isa::of(t);
        if let Some(vb) = isa.and_then(straight) {
            let bytes = n * w;
            let lanes = vb / w;
            let lane = self.c.opt && isa.is_some_and(|i| inserts(i, w));
            // Integers wrap: their lanes add in any order, the vectors
            // first, then across (`addv`, shuffles and adds).
            let across = match isa {
                Some(Isa::A64) => true,
                Some(Isa::X64 { .. }) => w == 4 || w == 8,
                _ => false,
            };
            if lane && across && !p.is_float() && bytes % vb == 0 && matches!(v, Mem::Frame(..)) {
                let mut acc = v;
                for c in 1..bytes / vb {
                    let t = Mem::Frame(self.out.slot(vb, 16), 0);
                    self.push(Inst::V(Box::new(Vx {
                        op: VxOp::Bin(VBin::Add),
                        w,
                        vb,
                        dst: t,
                        a: acc,
                        b: v.at(c * vb),
                    })));
                    acc = t;
                }
                let s = self.tmp_k(k);
                self.push(Inst::V(Box::new(Vx {
                    op: VxOp::Sum(k),
                    w,
                    vb,
                    dst: s,
                    a: acc,
                    b: acc,
                })));
                self.push(Inst::Mov {
                    k,
                    dst: d,
                    src: V::M(s),
                });
                return true;
            }
            if lane && bytes % vb == 0 && matches!(v, Mem::Frame(..)) {
                let s = self.tmp_k(k);
                let x = self.tmp_k(k);
                // Integers wrap: the first lane is the start.
                let int = !p.is_float();
                if !int {
                    self.push(Inst::Mov {
                        k,
                        dst: s,
                        src: V::I(0),
                    });
                }
                for i in 0..n {
                    let into = if int && i == 0 { s } else { x };
                    self.push(Inst::V(Box::new(Vx {
                        op: VxOp::Lane(k, (i % lanes) as u8),
                        w,
                        vb,
                        dst: into,
                        a: v.at(i / lanes * vb),
                        b: v.at(i / lanes * vb),
                    })));
                    if into == s {
                        continue;
                    }
                    self.push(Inst::Bin {
                        op: Bin::Add,
                        k,
                        signed: p.is_signed_int(),
                        dst: s,
                        a: V::M(s),
                        b: V::M(x),
                    });
                }
                self.push(Inst::Mov {
                    k,
                    dst: d,
                    src: V::M(s),
                });
                return true;
            }
            // A vector a register may hold goes to memory first.
            if bytes % vb == 0 && matches!(v, Mem::Frame(..)) {
                let m = Mem::Frame(self.out.slot(bytes, 16), 0);
                for c in 0..bytes / vb {
                    self.push(Inst::V(Box::new(Vx {
                        op: VxOp::Mov,
                        w,
                        vb,
                        dst: m.at(c * vb),
                        a: v.at(c * vb),
                        b: v.at(c * vb),
                    })));
                }
                v = m;
            }
        }
        let s = self.tmp_k(k);
        self.push(Inst::Mov {
            k,
            dst: s,
            src: V::I(0),
        });
        for i in 0..n {
            self.push(Inst::Bin {
                op: Bin::Add,
                k,
                signed: p.is_signed_int(),
                dst: s,
                a: V::M(s),
                b: V::M(v.at(i * w)),
            });
        }
        self.push(Inst::Mov {
            k,
            dst: d,
            src: V::M(s),
        });
        true
    }

    /// `load`, `store` and `splat`: vector moves where the emitter has
    /// them (whole registers), else copies and scalar moves.
    fn simd_move(
        &mut self,
        op: SimdOp,
        params: &[MirTy],
        ret: &MirTy,
        args: &[Val],
        dst: Option<Mem>,
    ) -> bool {
        let t = self.c.t;
        let vty = if op == SimdOp::Store { &params[0] } else { ret };
        let Some((p, n)) = lanes_of(vty) else {
            return false;
        };
        let Some(k) = cc::scalar_k(&MirTy::Prim(p), t) else {
            return false;
        };
        if p == PrimTy::Bool {
            return false;
        }
        let w = k.size();
        let bytes = n * w;
        let vb = Isa::of(t)
            .and_then(straight)
            .filter(|vb| bytes % vb == 0)
            .unwrap_or(0);
        let vx = |l: &mut Self, op: VxOp, dst: Mem, a: Mem| {
            for c in 0..bytes / vb {
                l.push(Inst::V(Box::new(Vx {
                    op,
                    w,
                    vb,
                    dst: dst.at(c * vb),
                    a: if matches!(op, VxOp::Splat(_)) {
                        a
                    } else {
                        a.at(c * vb)
                    },
                    b: a,
                })));
            }
        };
        match op {
            SimdOp::Splat => {
                let Some(d) = dst else { return true };
                let x = match args[0] {
                    Val::S(v, _) => v,
                    Val::M(m) => V::M(m),
                    Val::Unit => return false,
                };
                if vb == 0 {
                    for i in 0..n {
                        self.push(Inst::Mov {
                            k,
                            dst: d.at(i * w),
                            src: x,
                        });
                    }
                } else if let V::I(bits) = x {
                    vx(self, VxOp::Const(bits as u64), d, d);
                } else {
                    let V::M(m) = x else { unreachable!() };
                    vx(self, VxOp::Splat(k), d, m);
                }
            }
            SimdOp::Load | SimdOp::Store => {
                let (v, rest) = if op == SimdOp::Store {
                    let Val::M(v) = args[0] else { return false };
                    (Some(v), &args[1..])
                } else {
                    (dst, args)
                };
                let Some(v) = v else { return true };
                let at = self.simd_elem(rest, w);
                let m = Mem::Ind(at, HEADER_SIZE as i32);
                let (to, from) = if op == SimdOp::Store { (m, v) } else { (v, m) };
                if vb == 0 {
                    self.push(Inst::Copy {
                        dst: to,
                        src: from,
                        size: bytes,
                    });
                } else {
                    vx(self, VxOp::Mov, to, from);
                }
            }
            _ => unreachable!("not a move"),
        }
        true
    }

    /// A slot holding the address of element `at` of `buf` (`[buf, at]`),
    /// elements of `w` bytes, less the header.
    fn simd_elem(&mut self, args: &[Val], w: u32) -> Slot {
        let wk = self.word();
        let v = |a: &Val| match *a {
            Val::S(v, _) => v,
            Val::M(m) => V::M(m),
            Val::Unit => V::I(0),
        };
        let (buf, at) = (v(&args[0]), v(&args[1]));
        let off = if let V::I(i) = at {
            V::I(i.wrapping_mul(i64::from(w)))
        } else {
            let o = self.word_tmp();
            self.push(Inst::Bin {
                op: Bin::Mul,
                k: wk,
                signed: false,
                dst: o,
                a: at,
                b: V::I(i64::from(w)),
            });
            V::M(o)
        };
        let p = self.word_tmp();
        self.push(Inst::Bin {
            op: Bin::Add,
            k: wk,
            signed: false,
            dst: p,
            a: off,
            b: buf,
        });
        let Mem::Frame(ps, _) = p else { unreachable!() };
        ps
    }
}

/// `(T, N)` of a `Simd<T, N>` / `Mask<T, N>` (a struct of one `[T; N]`).
fn lanes_of(ty: &MirTy) -> Option<(PrimTy, u32)> {
    let MirTy::Struct(_, fields) = ty else {
        return None;
    };
    match fields.as_slice() {
        [MirTy::Array(e, n)] => match **e {
            MirTy::Prim(p) => Some((p, *n)),
            _ => None,
        },
        _ => None,
    }
}

/// The loop body of `op` over `arity` operands (bases `0 ..`) into base
/// `arity`: `(pre, body, registers)`, or `None` where the target lacks
/// an instruction for it.
fn simd_body(
    isa: Isa,
    op: SimdOp,
    p: PrimTy,
    w: u32,
    arity: usize,
) -> Option<(Vec<VOp>, Vec<VOp>, u8)> {
    let mut pre = Vec::new();
    let mut body = Vec::new();
    let mut next: VReg = 0;
    let mut reg = || {
        next += 1;
        next - 1
    };
    let ins: Vec<VReg> = (0..arity).map(|_| reg()).collect();
    for (k, r) in ins.iter().enumerate() {
        body.push(VOp::Load {
            dst: *r,
            base: k as u8,
        });
    }
    let float = p.is_float();
    let signed = p.is_signed_int();
    let sign = 1u64 << (w * 8 - 1);
    let bin = |o: VBin, a: VReg, b: VReg, d: VReg, body: &mut Vec<VOp>| -> Option<()> {
        isa.bin(o, w).then(|| {
            body.push(VOp::Bin {
                op: o,
                dst: d,
                a,
                b,
            });
        })
    };
    let konst = |bits: u64, pre: &mut Vec<VOp>, r: VReg| {
        pre.push(VOp::Const { dst: r, bits });
    };
    let out = reg();
    match op {
        SimdOp::Bin(o) => {
            let (a, b) = (ins[0], ins[1]);
            let (vop, x, y, negate) = match (o, float) {
                (PrimOp::Add, false) => (VBin::Add, a, b, false),
                (PrimOp::Sub, false) => (VBin::Sub, a, b, false),
                (PrimOp::Mul, false) => (VBin::Mul, a, b, false),
                (PrimOp::Add, true) => (VBin::FAdd, a, b, false),
                (PrimOp::Sub, true) => (VBin::FSub, a, b, false),
                (PrimOp::Mul, true) => (VBin::FMul, a, b, false),
                (PrimOp::Div, true) => (VBin::FDiv, a, b, false),
                (PrimOp::BitAnd, _) => (VBin::And, a, b, false),
                (PrimOp::BitOr, _) => (VBin::Or, a, b, false),
                (PrimOp::BitXor, _) => (VBin::Xor, a, b, false),
                (PrimOp::Eq, false) => (VBin::Eq, a, b, false),
                (PrimOp::Ne, false) => (VBin::Eq, a, b, true),
                (PrimOp::Eq, true) => (VBin::FEq, a, b, false),
                (PrimOp::Ne, true) => (VBin::FNe, a, b, false),
                (PrimOp::Lt, true) => (VBin::FLt, a, b, false),
                (PrimOp::Le, true) => (VBin::FLe, a, b, false),
                // `a < b` is `b > a`; `a <= b` is `!(a > b)`.
                (PrimOp::Lt, false) => (if signed { VBin::GtS } else { VBin::GtU }, b, a, false),
                (PrimOp::Le, false) => (if signed { VBin::GtS } else { VBin::GtU }, a, b, true),
                _ => return None,
            };
            if negate {
                let t = reg();
                let ones = reg();
                bin(vop, x, y, t, &mut body)?;
                konst(u64::MAX, &mut pre, ones);
                bin(VBin::Xor, t, ones, out, &mut body)?;
            } else {
                bin(vop, x, y, out, &mut body)?;
            }
        }
        SimdOp::Min | SimdOp::Max if !float => {
            let vop = match (op, signed) {
                (SimdOp::Min, true) => VBin::MinS,
                (SimdOp::Min, false) => VBin::MinU,
                (_, true) => VBin::MaxS,
                (_, false) => VBin::MaxU,
            };
            bin(vop, ins[0], ins[1], out, &mut body)?;
        }
        SimdOp::Un(PrimOp::Neg) => {
            let c = reg();
            if float {
                konst(sign, &mut pre, c);
                bin(VBin::Xor, ins[0], c, out, &mut body)?;
            } else {
                konst(0, &mut pre, c);
                bin(VBin::Sub, c, ins[0], out, &mut body)?;
            }
        }
        SimdOp::Un(PrimOp::Not) => {
            let c = reg();
            konst(u64::MAX, &mut pre, c);
            bin(VBin::Xor, ins[0], c, out, &mut body)?;
        }
        SimdOp::Abs if float => {
            let c = reg();
            konst(sign, &mut pre, c);
            bin(VBin::AndNot, ins[0], c, out, &mut body)?;
        }
        SimdOp::Select => {
            // `(m & a) | (b & !m)`.
            let (m, a, b) = (ins[0], ins[1], ins[2]);
            let (x, y) = (reg(), reg());
            bin(VBin::And, a, m, x, &mut body)?;
            bin(VBin::AndNot, b, m, y, &mut body)?;
            bin(VBin::Or, x, y, out, &mut body)?;
        }
        _ => return None,
    }
    body.push(VOp::Store {
        src: out,
        base: arity as u8,
    });
    (usize::from(next) <= isa.regs()).then_some((pre, body, next))
}
