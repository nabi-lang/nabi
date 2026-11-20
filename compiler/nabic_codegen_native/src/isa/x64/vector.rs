//! `VLoop` on `x86_64`: SSE2, or AVX2 (`VLoop::vb` 32: VEX forms on
//! `ymm`, `vzeroupper` once done, so code after it runs no slower for
//! the upper halves). `rax` the byte offset of the vector at hand, `rcx`
//! the last offset a vector may start at, `rdx` scratch, the bases in
//! `rsi rdi r8 .. r11` (Windows, where `rsi rdi` are callee-saved: `r8
//! .. r11`). Vector registers `xmm0 .. xmm14` (`ymm`) by number, `xmm15`
//! scratch. A `VLoop` clobbers what a call does; on Windows it keeps the
//! low halves of `xmm6 ..` it uses in `io` (`VLoop::save`).

use crate::isa::x64::enc::{
    Addr, Alu, Avx, Cond, FCmp, Gpr, PShift, Ptr, Rm, Shift, Sse, Sz, Xmm, Xrm,
};
use crate::isa::x64::{Em, R8, R9, R10, R11, RAX, RCX, RDI, RDX, RSI, RSP, addr};
use crate::lir::{
    K, Mem, V, VBin, VIO_BASES, VIO_BOUND, VIO_IV, VLoop, VOp, VReg, VShift, VUn, Vx, VxOp,
};

const OFF: u32 = RAX;
const LIM: u32 = RCX;
const TMP: u32 = RDX;
const SCRATCH: u32 = 15;
const SYSV_BASES: [u32; 6] = [RSI, RDI, R8, R9, R10, R11];
const WIN_BASES: [u32; 4] = [R8, R9, R10, R11];

impl Em<'_> {
    pub(crate) fn vloop(&mut self, v: &VLoop) {
        self.scratch();
        self.flags = None;
        self.tested = None;
        let io = self.slot_off[v.io as usize] as i32;
        let at = |o: u32| Addr::Bd(Gpr(RSP), io + o as i32);
        // The offset counts elements of the narrowest kind.
        let s = emin(v).trailing_zeros() as u8;
        let bases: &[u32] = if self.obj.coff {
            &WIN_BASES
        } else {
            &SYSV_BASES
        };
        // rcx = bound - min_left; below it no vector fits.
        self.a.mov(Sz::Q, Gpr(LIM), Rm::M(at(VIO_BOUND)));
        self.a
            .alu_ri(Alu::Sub, Sz::Q, Rm::r(LIM), v.min_left as i32);
        let skip = self.jcc_fwd(Cond::B);
        self.shift_gpr(LIM, Shift::Shl, s);
        // Windows: the callee-saved ones used, the scratch one among them.
        let saved: Vec<u32> = if self.obj.coff {
            (6..u32::from(v.regs)).chain([SCRATCH]).collect()
        } else {
            Vec::new()
        };
        for (n, x) in saved.iter().enumerate() {
            self.vmov_store(*x, at(v.save + 16 * n as u32));
        }
        for op in &v.pre {
            self.vop(v, op, io, &mut Vec::new());
        }
        self.a.mov(Sz::Q, Gpr(OFF), Rm::M(at(VIO_IV)));
        self.shift_gpr(OFF, Shift::Shl, s);
        for (n, d) in v.bases.iter().enumerate() {
            let r = bases[n];
            self.a
                .mov(Sz::Q, Gpr(r), Rm::M(at(VIO_BASES + 8 * n as u32)));
            self.a.alu_ri(Alu::Add, Sz::Q, Rm::r(r), *d);
        }
        let top = self.a.pos();
        self.alu(Alu::Cmp, true, OFF, LIM);
        let mut exits = vec![self.jcc_fwd(Cond::A)];
        for op in &v.body {
            self.vop(v, op, io, &mut exits);
        }
        let step = (v.vb / v.w * emin(v)) as i8;
        self.a.alu_ri8(Alu::Add, Sz::Q, Rm::r(OFF), step);
        self.a.jmp(Some(top));
        for e in exits {
            self.here(e);
        }
        self.shift_gpr(OFF, Shift::Shr, s);
        self.a.mov_st(Sz::Q, Rm::M(at(VIO_IV)), Gpr(OFF));
        for &(r, off) in &v.post {
            if v.vb == 32 {
                self.a
                    .vex_st(Avx::MovdquSt, true, at(off), Xmm(u32::from(r)));
            } else {
                self.vmov_store(u32::from(r), at(off));
            }
        }
        if v.vb == 32 {
            self.a.vzeroupper();
        }
        for (n, x) in saved.iter().enumerate() {
            self.vmov_load(*x, at(v.save + 16 * n as u32));
        }
        self.here(skip);
        self.held.clear();
    }

    /// `op` (`shl` / `shr`) `r` by `n`.
    fn shift_gpr(&mut self, r: u32, op: Shift, n: u8) {
        if n != 0 {
            self.a.shift_ri(op, Sz::Q, Gpr(r), n);
        }
    }

    /// `movdqu [m], xmm`.
    fn vmov_store(&mut self, x: u32, m: Addr) {
        self.a.sse_st(Sse::MovdquSt, m, Xmm(x));
    }

    /// `movdqu xmm, [m]`.
    fn vmov_load(&mut self, x: u32, m: Addr) {
        self.a.sse(Sse::Movdqu, Xmm(x), Xrm::M(m));
    }

    /// `[base + rax * 2^sc]`.
    const fn indexed(base: u32, sc: u8) -> Addr {
        Addr::Bi(Gpr(base), Gpr(OFF), sc)
    }

    /// Elements of `e` bytes into lanes of twice that, `signed` or not.
    fn widen_step(&mut self, d: u32, e: u32, signed: bool) {
        let unpack = [Sse::Punpcklbw, Sse::Punpcklwd, Sse::Punpckldq][wi(e)];
        if !signed {
            self.xop(Sse::Pxor, SCRATCH, SCRATCH);
            self.xop(unpack, d, SCRATCH);
        } else if e == 4 {
            self.movdqa(SCRATCH, d);
            self.a.pshift(PShift::Psrad, Xmm(SCRATCH), 31);
            self.xop(Sse::Punpckldq, d, SCRATCH);
        } else {
            self.xop(unpack, d, d);
            let sra = if e == 1 { PShift::Psraw } else { PShift::Psrad };
            self.a.pshift(sra, Xmm(d), (8 * e) as u8);
        }
    }

    /// `xmm15` = the lanes of `src` (`w` bytes) cut to `e` bytes, in its
    /// low `16 * e / w` bytes.
    fn narrow_sse(&mut self, src: u32, w: u32, e: u32) {
        let t = SCRATCH;
        let mut w = w;
        if w == 8 {
            // The low halves first.
            self.a.sse_ri(Sse::Pshufd, Xmm(t), Xmm(src), 0b1000);
            w = 4;
        } else {
            self.movdqa(t, src);
        }
        if w == e {
            return;
        }
        // Each lane sign-extended from its low `e` bytes: the signed
        // packs keep it whole. Bytes from words pack unsigned.
        if w == 2 {
            self.a.pshift(PShift::Psllw, Xmm(t), 8);
            self.a.pshift(PShift::Psrlw, Xmm(t), 8);
            self.xop(Sse::Packuswb, t, t);
            return;
        }
        let k = (8 * (4 - e)) as u8;
        self.a.pshift(PShift::Pslld, Xmm(t), k);
        self.a.pshift(PShift::Psrad, Xmm(t), k);
        self.xop(Sse::Packssdw, t, t);
        if e == 1 {
            self.xop(Sse::Packsswb, t, t);
        }
    }

    /// An SSE instruction `xmm d, xmm a`.
    fn xop(&mut self, op: Sse, d: u32, a: u32) {
        self.a.sse(op, Xmm(d), Xrm::X(Xmm(a)));
    }

    fn movdqa(&mut self, d: u32, a: u32) {
        if d != a {
            self.xop(Sse::Movdqa, d, a);
        }
    }

    /// Every lane of `d` = the low `w` bytes of `d`'s low lane.
    fn broadcast(&mut self, d: u32, w: u32) {
        if w == 1 {
            self.xop(Sse::Punpcklbw, d, d);
        }
        if w <= 2 {
            self.xop(Sse::Punpcklwd, d, d);
        }
        if w == 8 {
            self.xop(Sse::Punpcklqdq, d, d);
        } else {
            self.a.sse_ri(Sse::Pshufd, Xmm(d), Xmm(d), 0);
        }
    }

    /// `op`; a `Bail` adds the position of its branch to `exits`.
    fn vop(&mut self, v: &VLoop, op: &VOp, io: i32, exits: &mut Vec<usize>) {
        if v.vb == 32 {
            return self.avx_op(v, op, io, exits);
        }
        let bases: &[u32] = if self.obj.coff {
            &WIN_BASES
        } else {
            &SYSV_BASES
        };
        let x = |r: VReg| u32::from(r);
        let sc = |b: u8| (v.elem[b as usize] / emin(v)).trailing_zeros() as u8;
        match *op {
            VOp::Load { dst, base } => {
                let m = Self::indexed(bases[base as usize], sc(base));
                self.vmov_load(x(dst), m);
            }
            VOp::Store { src, base } => {
                let m = Self::indexed(bases[base as usize], sc(base));
                self.vmov_store(x(src), m);
            }
            VOp::LoadExt { dst, base, signed } => {
                let e = v.elem[base as usize];
                let (m, d) = (Self::indexed(bases[base as usize], sc(base)), x(dst));
                if 16 * e / v.w == 8 {
                    self.a.sse(Sse::MovqLd, Xmm(d), Xrm::M(m));
                } else {
                    self.a.sse(Sse::MovdLd, Xmm(d), Xrm::M(m));
                }
                let mut e = e;
                while e < v.w {
                    self.widen_step(d, e, signed);
                    e *= 2;
                }
            }
            VOp::StoreTrunc { src, base } => {
                let e = v.elem[base as usize];
                self.narrow_sse(x(src), v.w, e);
                let m = Self::indexed(bases[base as usize], sc(base));
                if 16 * e / v.w == 8 {
                    self.a.sse_st(Sse::MovqSt, m, Xmm(SCRATCH));
                } else {
                    self.a.sse_st(Sse::MovdSt, m, Xmm(SCRATCH));
                }
            }
            VOp::MaskStore { .. } => unreachable!("SSE2 stores no lanes under a mask"),
            VOp::Splat { dst, off } => {
                let m = Rm::m(RSP, io + off as i32);
                let k = [K::I8, K::I16, K::I32, K::I64][wi(v.w)];
                self.sized_load(k, TMP, m);
                self.a.movq_xg(Sz::dq(v.w == 8), Xmm(x(dst)), Gpr(TMP));
                self.broadcast(x(dst), v.w);
            }
            VOp::Const { dst, bits } => {
                let d = x(dst);
                let ones = if v.w == 8 {
                    u64::MAX
                } else {
                    (1u64 << (8 * v.w)) - 1
                };
                if bits == 0 {
                    self.xop(Sse::Pxor, d, d);
                } else if bits == ones {
                    self.xop(Sse::Pcmpeqd, d, d);
                } else {
                    self.mov_imm(TMP, bits);
                    self.a.movq_xg(Sz::Q, Xmm(d), Gpr(TMP));
                    self.broadcast(d, v.w);
                }
            }
            VOp::Bin { op, dst, a, b } => self.vbin(v.w, op, x(dst), x(a), x(b), SCRATCH),
            VOp::Un { op, dst, a } => {
                let (d, a) = (x(dst), x(a));
                match op {
                    VUn::FSqrt => {
                        let op = if v.w == 8 { Sse::Sqrtpd } else { Sse::Sqrtps };
                        self.xop(op, d, a);
                    }
                    VUn::Abs => unreachable!("no lane magnitudes here"),
                    VUn::SignMask => match v.w {
                        1 => {
                            let t = if d == a { SCRATCH } else { d };
                            self.xop(Sse::Pxor, t, t);
                            self.xop(Sse::Pcmpgtb, t, a);
                            self.movdqa(d, t);
                        }
                        2 => {
                            self.movdqa(d, a);
                            self.a.pshift(PShift::Psraw, Xmm(d), 15);
                        }
                        4 => {
                            self.movdqa(d, a);
                            self.a.pshift(PShift::Psrad, Xmm(d), 31);
                        }
                        _ => {
                            self.movdqa(d, a);
                            self.a.pshift(PShift::Psrad, Xmm(d), 31);
                            self.a.sse_ri(Sse::Pshufd, Xmm(d), Xmm(d), 0xf5);
                        }
                    },
                }
            }
            VOp::Shift { op, dst, a, n } => {
                let (d, a) = (x(dst), x(a));
                self.movdqa(d, a);
                self.a.pshift(pshift(op, v.w), Xmm(d), n);
            }
            VOp::Dot { .. } => unreachable!("no dot products here"),
            VOp::Fma { dst, a, b, c } => {
                // vfmadd231 adds into its destination: a copy of `c`.
                let op = if v.w == 8 {
                    Avx::Vfmadd231pd
                } else {
                    Avx::Vfmadd231ps
                };
                let (d, a, b, c) = (x(dst), x(a), x(b), x(c));
                let t = if d == c || (d != a && d != b) {
                    d
                } else {
                    SCRATCH
                };
                self.movdqa(t, c);
                self.a.vex_rrr(op, false, Xmm(t), Xmm(a), Xmm(b));
                self.movdqa(d, t);
            }
            VOp::Bail { a } => {
                let a = x(a);
                self.a.movmsk(movmsk(v.w), Gpr(TMP), Xmm(a));
                if v.w == 2 {
                    // The high byte of each lane.
                    self.a.test_ri(Sz::D, Rm::r(TMP), 0xaaaa);
                } else {
                    self.a.test_rr(Sz::D, Gpr(TMP), Gpr(TMP));
                }
                exits.push(self.jcc_fwd(Cond::NE));
            }
        }
    }

    /// `x` (SSE2, 16 bytes): an operand a register holds is used there,
    /// any other is loaded into `xmm0` / `xmm1`; the result is made in its
    /// register, or in `xmm2` and stored; `xmm3` is scratch.
    pub(crate) fn vx(&mut self, x: &Vx) {
        // A lane of a vector in memory is a scalar there.
        if let (VxOp::Lane(k, n), None) = (x.op, self.dst_x(x.a)) {
            let m = x.a.at(u32::from(n) * x.w);
            if k.is_float() {
                let r = self.freg(V::M(m), k, &[]);
                self.store_fp(r, x.dst, k);
            } else {
                self.load(TMP, V::M(m), k, false);
                self.store_ext(TMP, x.dst, k, Some(false));
            }
            return;
        }
        let mut ins = [0, 1];
        // A load goes straight to the register holding the result.
        if x.op == VxOp::Mov {
            ins[0] = self.dst_x(x.dst).unwrap_or(0);
        }
        for (n, m) in x.srcs().enumerate() {
            ins[n] = self.vin(m, ins[n]);
        }
        let own = self.dst_x(x.dst);
        let d = own.unwrap_or(2);
        let w = x.w;
        match x.op {
            VxOp::Mov if own.is_none() => {
                self.vout(ins[0], x.dst);
                return;
            }
            VxOp::Mov => {
                self.w(d, true);
                self.movdqa(d, ins[0]);
            }
            VxOp::Bin(op) => {
                self.w(d, true);
                self.w(3, true);
                self.vbin(w, op, d, ins[0], ins[1], 3);
            }
            VxOp::Un(VUn::FSqrt) => {
                self.w(d, true);
                let op = if w == 8 { Sse::Sqrtpd } else { Sse::Sqrtps };
                self.xop(op, d, ins[0]);
            }
            VxOp::Un(op) => unreachable!("no straight-line {op:?}"),
            VxOp::Const(bits) => {
                let ones = if w == 8 {
                    u64::MAX
                } else {
                    (1u64 << (8 * w)) - 1
                };
                let bits = bits & ones;
                if bits == 0 {
                    self.w(d, true);
                    self.xop(Sse::Pxor, d, d);
                } else if bits == ones {
                    self.w(d, true);
                    self.xop(Sse::Pcmpeqd, d, d);
                } else {
                    self.mov_imm(TMP, bits);
                    self.w(d, true);
                    self.a.movq_xg(Sz::Q, Xmm(d), Gpr(TMP));
                    self.broadcast(d, w);
                }
            }
            // A float in a register splats from there.
            VxOp::Splat(k) if k.is_float() && self.dst_x(x.a).is_some() => {
                self.load_fp(d, V::M(x.a), k);
                self.w(d, true);
                self.broadcast(d, w);
            }
            VxOp::Splat(k) => {
                let ik = match k {
                    K::F32 => K::I32,
                    K::F64 => K::I64,
                    k => k,
                };
                self.load(TMP, V::M(x.a), ik, false);
                self.w(d, true);
                self.a.movq_xg(Sz::dq(w == 8), Xmm(d), Gpr(TMP));
                self.broadcast(d, w);
            }
            VxOp::Insert(k, n) => {
                // The scalar into `xmm3` first: its register may be the
                // result's.
                if k.is_float() {
                    self.load_fp(3, V::M(x.b), k);
                } else {
                    self.load(TMP, V::M(x.b), k, false);
                    self.w(3, true);
                    self.a.movq_xg(Sz::dq(w == 8), Xmm(3), Gpr(TMP));
                }
                self.w(d, true);
                self.movdqa(d, ins[0]);
                self.w(0, true);
                self.w(1, true);
                self.w(3, true);
                match (w, n) {
                    (8, 0) => self.xop(Sse::Movsd, d, 3),
                    (8, _) => self.xop(Sse::Punpcklqdq, d, 3),
                    (_, 0) => self.xop(Sse::Movss, d, 3),
                    // [d0 s d1 s1], then [d2 d3] above.
                    (_, 1) => {
                        self.movdqa(0, d);
                        self.xop(Sse::Punpckldq, 0, 3);
                        self.a.sse_ri(Sse::Pshufd, Xmm(1), Xmm(d), 0xee);
                        self.xop(Sse::Punpcklqdq, 0, 1);
                        self.movdqa(d, 0);
                    }
                    // [s d3] above [d0 d1].
                    (_, 2) => {
                        self.a.sse_ri(Sse::Pshufd, Xmm(1), Xmm(d), 3);
                        self.xop(Sse::Punpckldq, 3, 1);
                        self.xop(Sse::Punpcklqdq, d, 3);
                    }
                    // [d2 s] above [d0 d1].
                    _ => {
                        self.a.sse_ri(Sse::Pshufd, Xmm(1), Xmm(d), 2);
                        self.xop(Sse::Punpckldq, 1, 3);
                        self.xop(Sse::Punpcklqdq, d, 1);
                    }
                }
            }
            VxOp::Sum(k) => {
                let t = ins[0];
                self.w(1, true);
                self.w(3, true);
                if w == 8 {
                    self.a.sse_ri(Sse::Pshufd, Xmm(3), Xmm(t), 0xee);
                    self.xop(Sse::Paddq, 3, t);
                } else {
                    // Halves, then quarters.
                    self.a.sse_ri(Sse::Pshufd, Xmm(3), Xmm(t), 0x4e);
                    self.xop(Sse::Paddd, 3, t);
                    self.a.sse_ri(Sse::Pshufd, Xmm(1), Xmm(3), 0xb1);
                    self.xop(Sse::Paddd, 3, 1);
                }
                self.w(TMP, false);
                self.a.movq_gx(Sz::dq(w == 8), Gpr(TMP), Xmm(3));
                self.store_ext(TMP, x.dst, k, Some(false));
                return;
            }
            VxOp::Lane(k, n) => {
                let mut t = ins[0];
                if n > 0 {
                    let imm = if w == 8 { 0xee } else { n };
                    self.w(3, true);
                    self.a.sse_ri(Sse::Pshufd, Xmm(3), Xmm(t), imm);
                    t = 3;
                }
                if k.is_float() {
                    self.store_fp(t, x.dst, k);
                } else {
                    self.w(TMP, false);
                    self.a.movq_gx(Sz::dq(w == 8), Gpr(TMP), Xmm(t));
                    self.store_ext(TMP, x.dst, k, Some(false));
                }
                return;
            }
        }
        if own.is_none() {
            self.vout(d, x.dst);
        }
    }

    /// The `xmm` holding vector `m`: its own, or `tmp` loaded.
    fn vin(&mut self, m: Mem, tmp: u32) -> u32 {
        if let Some(r) = self.dst_x(m) {
            return r;
        }
        let a = addr(self.mem(m, R10));
        self.rd(m, 16);
        self.w(tmp, true);
        self.vmov_load(tmp, a);
        tmp
    }

    /// A C vector argument or result: the 16 bytes of `m` into `xmm` `q`.
    pub(crate) fn vec_load(&mut self, q: u32, m: Mem) {
        let got = self.vin(m, q);
        if got != q {
            self.w(q, true);
            self.a.sse(Sse::Movaps, Xmm(q), Xrm::X(Xmm(got)));
        }
    }

    /// A C vector result or parameter: `xmm` `q` into the 16 bytes of `m`.
    pub(crate) fn vec_store(&mut self, q: u32, m: Mem) {
        match self.dst_x(m) {
            Some(d) if d != q => {
                self.w(d, true);
                self.a.sse(Sse::Movaps, Xmm(d), Xrm::X(Xmm(q)));
            }
            Some(_) => {}
            None => self.vout(q, m),
        }
    }

    /// `xmm` `q` to 16 bytes at `a` (a stack argument).
    pub(crate) fn vec_put(&mut self, q: u32, a: Addr) {
        self.vmov_store(q, a);
    }

    /// 16 bytes at `a` (a stack parameter) into `xmm` `q`.
    pub(crate) fn vec_get(&mut self, q: u32, a: Addr) {
        self.w(q, true);
        self.vmov_load(q, a);
    }

    /// Store `xmm` `q` to `m`.
    fn vout(&mut self, q: u32, m: Mem) {
        let a = addr(self.mem(m, R10));
        self.wrote(m, 16);
        self.vmov_store(q, a);
    }

    /// `d = a op b`, `t` a scratch register.
    fn vbin(&mut self, w: u32, op: VBin, d: u32, a: u32, b: u32, t: u32) {
        let pd = w == 8;
        let fp = |ps, pd_| if pd { pd_ } else { ps };
        let i = wi(w);
        let none = || unreachable!("no SSE2 {op:?} of {w}-byte lanes");
        // (instruction or `cmpps` predicate, commutative)
        let (op, comm): (Result<Sse, FCmp>, bool) = match op {
            VBin::Add => (
                Ok([Sse::Paddb, Sse::Paddw, Sse::Paddd, Sse::Paddq][i]),
                true,
            ),
            VBin::Sub => (
                Ok([Sse::Psubb, Sse::Psubw, Sse::Psubd, Sse::Psubq][i]),
                false,
            ),
            VBin::Mul => (Ok(Sse::Pmullw), true),
            VBin::And => (Ok(Sse::Pand), true),
            VBin::Or => (Ok(Sse::Por), true),
            VBin::Xor => (Ok(Sse::Pxor), true),
            VBin::Eq | VBin::GtS if w == 8 => none(),
            VBin::Eq => (Ok([Sse::Pcmpeqb, Sse::Pcmpeqw, Sse::Pcmpeqd][i]), true),
            VBin::GtS => (Ok([Sse::Pcmpgtb, Sse::Pcmpgtw, Sse::Pcmpgtd][i]), false),
            VBin::MinU => (Ok(Sse::Pminub), true),
            VBin::MaxU => (Ok(Sse::Pmaxub), true),
            VBin::MinS => (Ok(Sse::Pminsw), true),
            VBin::MaxS => (Ok(Sse::Pmaxsw), true),
            VBin::FAdd => (Ok(fp(Sse::Addps, Sse::Addpd)), true),
            VBin::FSub => (Ok(fp(Sse::Subps, Sse::Subpd)), false),
            VBin::FMul => (Ok(fp(Sse::Mulps, Sse::Mulpd)), true),
            VBin::FDiv => (Ok(fp(Sse::Divps, Sse::Divpd)), false),
            VBin::FEq => (Err(FCmp::Eq), true),
            VBin::FNe => (Err(FCmp::Neq), true),
            VBin::FLt => (Err(FCmp::Lt), false),
            VBin::FLe => (Err(FCmp::Le), false),
            // No NaN lanes: either order.
            VBin::FMin => (Ok(fp(Sse::Minps, Sse::Minpd)), true),
            VBin::FMax => (Ok(fp(Sse::Maxps, Sse::Maxpd)), true),
            VBin::MulHiS => (Ok(Sse::Pmulhw), true),
            VBin::MulHiU => (Ok(Sse::Pmulhuw), true),
            VBin::AndNot => {
                // pandn: d = !d & src.
                if d == b {
                    self.xop(Sse::Pandn, d, a);
                } else if d == a {
                    self.movdqa(t, b);
                    self.xop(Sse::Pandn, t, a);
                    self.movdqa(d, t);
                } else {
                    self.movdqa(d, b);
                    self.xop(Sse::Pandn, d, a);
                }
                return;
            }
            VBin::GtU => unreachable!("SSE2 has no unsigned compare"),
        };
        let emit = |e: &mut Self, d: u32, src: u32| match op {
            Ok(op) => e.xop(op, d, src),
            Err(pred) => e.a.cmpp(pd, pred, Xmm(d), Xmm(src)),
        };
        if d == a {
            emit(self, d, b);
        } else if d == b && comm {
            emit(self, d, a);
        } else if d == b {
            self.movdqa(t, b);
            self.movdqa(d, a);
            emit(self, d, t);
        } else {
            self.movdqa(d, a);
            emit(self, d, b);
        }
    }
}

/// The index of lane width `w` (1, 2, 4, 8).
const fn wi(w: u32) -> usize {
    w.trailing_zeros() as usize
}

/// The bytes of the narrowest element of `v`: the offset's unit.
fn emin(v: &VLoop) -> u32 {
    v.elem.iter().copied().chain([v.w]).min().unwrap_or(v.w)
}

/// The shift `op` of lanes of `w` bytes.
fn pshift(op: VShift, w: u32) -> PShift {
    match (op, w) {
        (VShift::Shl, 2) => PShift::Psllw,
        (VShift::Shl, 4) => PShift::Pslld,
        (VShift::Shl, _) => PShift::Psllq,
        (VShift::ShrU, 2) => PShift::Psrlw,
        (VShift::ShrU, 4) => PShift::Psrld,
        (VShift::ShrU, _) => PShift::Psrlq,
        (VShift::ShrS, 2) => PShift::Psraw,
        (VShift::ShrS, 4) => PShift::Psrad,
        (VShift::ShrS, _) => unreachable!("no 64-bit arithmetic shift"),
    }
}

/// The sign mask of lanes of `w` bytes: `pmovmskb` (bytes), `movmskps`,
/// `movmskpd`.
const fn movmsk(w: u32) -> Avx {
    match w {
        1 | 2 => Avx::Pmovmskb,
        4 => Avx::Movmskps,
        _ => Avx::Movmskpd,
    }
}

impl Em<'_> {
    /// `op` in AVX2 (256 bits); a `Bail` adds its branch to `exits`.
    #[allow(clippy::too_many_lines)]
    fn avx_op(&mut self, v: &VLoop, op: &VOp, io: i32, exits: &mut Vec<usize>) {
        let bases: &[u32] = if self.obj.coff {
            &WIN_BASES
        } else {
            &SYSV_BASES
        };
        let x = |r: VReg| Xmm(u32::from(r));
        let w8 = v.w == 8;
        let at = |b: u8| {
            let sc = (v.elem[b as usize] / emin(v)).trailing_zeros() as u8;
            Self::indexed(bases[b as usize], sc)
        };
        match *op {
            VOp::Load { dst, base } => {
                self.a.vex_ld(Avx::Movdqu, true, x(dst), at(base), Ptr::Y);
            }
            VOp::Store { src, base } => {
                self.a.vex_st(Avx::MovdquSt, true, at(base), x(src));
            }
            VOp::LoadExt { dst, base, signed } => {
                // vpmovzx / vpmovsx: bw wd dq (ratio 2), bd wq (4).
                let e = v.elem[base as usize];
                let (s, z, ptr) = match (v.w / e, e) {
                    (2, 1) => (Avx::Pmovsxbw, Avx::Pmovzxbw, Ptr::X),
                    (2, 2) => (Avx::Pmovsxwd, Avx::Pmovzxwd, Ptr::X),
                    (2, _) => (Avx::Pmovsxdq, Avx::Pmovzxdq, Ptr::X),
                    (_, 1) => (Avx::Pmovsxbd, Avx::Pmovzxbd, Ptr::Q),
                    _ => (Avx::Pmovsxwq, Avx::Pmovzxwq, Ptr::Q),
                };
                let op = if signed { s } else { z };
                self.a.vex_ld(op, true, x(dst), at(base), ptr);
            }
            VOp::StoreTrunc { src, base } => {
                let (t, a) = (Xmm(SCRATCH), x(src));
                match v.w {
                    2 => {
                        self.a.vex_shift(PShift::Psllw, true, t, a, 8);
                        self.a.vex_shift(PShift::Psrlw, true, t, t, 8);
                        self.a.vex_rrr(Avx::Packuswb, true, t, t, t);
                    }
                    4 => {
                        self.a.vex_shift(PShift::Pslld, true, t, a, 16);
                        self.a.vex_shift(PShift::Psrad, true, t, t, 16);
                        self.a.vex_rrr(Avx::Packssdw, true, t, t, t);
                    }
                    _ => self.a.vex_ri(Avx::Pshufd, true, t, a, 0b1000),
                }
                // Both halves' results low.
                self.a.vex_ri(Avx::Permq, true, t, t, 0b1000);
                self.a.vex_st(Avx::MovdquSt, false, at(base), t);
            }
            VOp::MaskStore { src, mask, base } => {
                let op = if w8 { Avx::Pmaskmovq } else { Avx::Pmaskmovd };
                self.a.vex_maskst(op, true, at(base), x(mask), x(src));
            }
            VOp::Splat { dst, off } => {
                let (op, ptr) = broadcast(v.w);
                let m = Addr::Bd(Gpr(RSP), io + off as i32);
                self.a.vex_ld(op, true, x(dst), m, ptr);
            }
            VOp::Const { dst, bits } => {
                let d = x(dst);
                let ones = if w8 {
                    u64::MAX
                } else {
                    (1u64 << (8 * v.w)) - 1
                };
                if bits == 0 {
                    self.a.vex_rrr(Avx::Pxor, true, d, d, d);
                } else if bits == ones {
                    self.a.vex_rrr(Avx::Pcmpeqd, true, d, d, d);
                } else {
                    self.mov_imm(TMP, bits);
                    self.a.vmovq_xg(d, Gpr(TMP));
                    self.a.vex_rr(broadcast(v.w).0, true, d, d, true);
                }
            }
            VOp::Bin { op, dst, a, b } => self.avx_bin(v.w, op, x(dst), x(a), x(b)),
            VOp::Un { op, dst, a } => {
                let (d, a) = (x(dst), x(a));
                match op {
                    VUn::FSqrt => {
                        let op = if w8 { Avx::Sqrtpd } else { Avx::Sqrtps };
                        self.a.vex_rr(op, true, d, a, false);
                    }
                    VUn::Abs => unreachable!("no lane magnitudes here"),
                    VUn::SignMask => match v.w {
                        2 | 4 => {
                            let op = if v.w == 2 {
                                PShift::Psraw
                            } else {
                                PShift::Psrad
                            };
                            self.a.vex_shift(op, true, d, a, (8 * v.w - 1) as u8);
                        }
                        _ => {
                            // 0 > a
                            let t = Xmm(SCRATCH);
                            self.a.vex_rrr(Avx::Pxor, true, t, t, t);
                            let gt = if w8 { Avx::Pcmpgtq } else { Avx::Pcmpgtb };
                            self.a.vex_rrr(gt, true, d, t, a);
                        }
                    },
                }
            }
            VOp::Shift { op, dst, a, n } => {
                self.a.vex_shift(pshift(op, v.w), true, x(dst), x(a), n);
            }
            VOp::Dot { .. } => unreachable!("no dot products here"),
            VOp::Fma { dst, a, b, c } => {
                let op = if w8 {
                    Avx::Vfmadd231pd
                } else {
                    Avx::Vfmadd231ps
                };
                let (d, a, b, c) = (x(dst), x(a), x(b), x(c));
                let t = if d == c || (d != a && d != b) {
                    d
                } else {
                    Xmm(SCRATCH)
                };
                if t != c {
                    self.a.vex_rrr(Avx::Por, true, t, c, c);
                }
                self.a.vex_rrr(op, true, t, a, b);
                if t != d {
                    self.a.vex_rrr(Avx::Por, true, d, t, t);
                }
            }
            VOp::Bail { a } => {
                self.a.vex_movmsk(movmsk(v.w), true, Gpr(TMP), x(a));
                if v.w == 2 {
                    // The high byte of each lane.
                    self.a.test_ri(Sz::D, Rm::r(TMP), 0xaaaa_aaaau32 as i32);
                } else {
                    self.a.test_rr(Sz::D, Gpr(TMP), Gpr(TMP));
                }
                exits.push(self.jcc_fwd(Cond::NE));
            }
        }
    }

    fn avx_bin(&mut self, w: u32, op: VBin, d: Xmm, a: Xmm, b: Xmm) {
        let pd = w == 8;
        let fp = |ps, pd_| if pd { pd_ } else { ps };
        let i = wi(w);
        let none = || unreachable!("no AVX2 {op:?} of {w}-byte lanes");
        let op = match op {
            VBin::Add => [Avx::Paddb, Avx::Paddw, Avx::Paddd, Avx::Paddq][i],
            VBin::Sub => [Avx::Psubb, Avx::Psubw, Avx::Psubd, Avx::Psubq][i],
            VBin::Mul if w == 2 => Avx::Pmullw,
            VBin::Mul => Avx::Pmulld,
            VBin::And => Avx::Pand,
            VBin::Or => Avx::Por,
            VBin::Xor => Avx::Pxor,
            // vpandn d, b, a: !b & a.
            VBin::AndNot => return self.a.vex_rrr(Avx::Pandn, true, d, b, a),
            VBin::Eq => [Avx::Pcmpeqb, Avx::Pcmpeqw, Avx::Pcmpeqd, Avx::Pcmpeqq][i],
            VBin::GtS => [Avx::Pcmpgtb, Avx::Pcmpgtw, Avx::Pcmpgtd, Avx::Pcmpgtq][i],
            VBin::MinU | VBin::MaxU | VBin::MinS | VBin::MaxS => match (op, w) {
                (VBin::MinU, 1) => Avx::Pminub,
                (VBin::MinU, 2) => Avx::Pminuw,
                (VBin::MinU, 4) => Avx::Pminud,
                (VBin::MaxU, 1) => Avx::Pmaxub,
                (VBin::MaxU, 2) => Avx::Pmaxuw,
                (VBin::MaxU, 4) => Avx::Pmaxud,
                (VBin::MinS, 1) => Avx::Pminsb,
                (VBin::MinS, 2) => Avx::Pminsw,
                (VBin::MinS, 4) => Avx::Pminsd,
                (VBin::MaxS, 1) => Avx::Pmaxsb,
                (VBin::MaxS, 2) => Avx::Pmaxsw,
                (VBin::MaxS, 4) => Avx::Pmaxsd,
                _ => none(),
            },
            VBin::FAdd => fp(Avx::Addps, Avx::Addpd),
            VBin::FSub => fp(Avx::Subps, Avx::Subpd),
            VBin::FMul => fp(Avx::Mulps, Avx::Mulpd),
            VBin::FDiv => fp(Avx::Divps, Avx::Divpd),
            VBin::FMin => fp(Avx::Minps, Avx::Minpd),
            VBin::FMax => fp(Avx::Maxps, Avx::Maxpd),
            VBin::MulHiS => Avx::Pmulhw,
            VBin::MulHiU => Avx::Pmulhuw,
            VBin::FEq | VBin::FNe | VBin::FLt | VBin::FLe => {
                let pred = match op {
                    VBin::FEq => FCmp::Eq,
                    VBin::FNe => FCmp::Neq,
                    VBin::FLt => FCmp::Lt,
                    _ => FCmp::Le,
                };
                return self.a.vex_cmpp(pd, true, pred, d, a, b);
            }
            VBin::GtU => unreachable!("AVX2 has no unsigned compare"),
        };
        self.a.vex_rrr(op, true, d, a, b);
    }
}

/// `vpbroadcast` of `w`-byte lanes, and its memory operand's size.
const fn broadcast(w: u32) -> (Avx, Ptr) {
    match w {
        1 => (Avx::Pbroadcastb, Ptr::B),
        2 => (Avx::Pbroadcastw, Ptr::W),
        4 => (Avx::Pbroadcastd, Ptr::D),
        _ => (Avx::Pbroadcastq, Ptr::Q),
    }
}

#[cfg(test)]
mod tests {
    use crate::lir::{Func, Inst, Mem, VBin, VLoop, VOp, VReg, VShift, VUn, Vx, VxOp};
    use crate::obj::{Obj, Sect};

    /// The code of a function holding one `VLoop` of lane width `w`.
    fn code(w: u32, coff: bool, pre: Vec<VOp>, body: Vec<VOp>) -> Vec<u8> {
        code_vb(w, 16, coff, pre, body)
    }

    /// Straight-line vector code with every operand in the frame: loads
    /// into `xmm0` / `xmm1`, the result made in `xmm2` and stored.
    #[test]
    fn straight_line_through_memory() {
        let mut obj = Obj::new(true, true, false);
        let sym = obj.sym("f");
        let mut f = Func {
            sym,
            ..Func::default()
        };
        let m: Vec<Mem> = (0..3).map(|_| Mem::Frame(f.slot(16, 16), 0)).collect();
        f.insts.push(Inst::V(Box::new(Vx {
            op: VxOp::Bin(VBin::FAdd),
            w: 4,
            vb: 16,
            dst: m[2],
            a: m[0],
            b: m[1],
        })));
        f.insts.push(Inst::Ret { vals: Vec::new() });
        super::super::emit(&mut obj, &f);
        let code = obj.sect(Sect::Text).data.clone();
        // movdqa xmm2, xmm0; addps xmm2, xmm1; movdqu [..], xmm2
        let want = [0x66, 0x0f, 0x6f, 0xd0, 0x0f, 0x58, 0xd1, 0xf3, 0x0f, 0x7f];
        assert!(code.windows(want.len()).any(|w| w == want), "{code:x?}");
    }

    fn code_vb(w: u32, vb: u32, coff: bool, pre: Vec<VOp>, body: Vec<VOp>) -> Vec<u8> {
        let mut obj = Obj::new(true, !coff, coff);
        let sym = obj.sym("f");
        let mut f = Func {
            sym,
            ..Func::default()
        };
        let io = f.slot(256, 16);
        f.insts.push(Inst::VLoop(Box::new(VLoop {
            io,
            w,
            vb,
            min_left: vb / w,
            bases: vec![16; 4],
            elem: vec![w; 4],
            pre,
            body,
            post: Vec::new(),
            regs: 15,
            save: 64,
        })));
        f.insts.push(Inst::Ret { vals: Vec::new() });
        super::super::emit(&mut obj, &f);
        obj.sect(Sect::Text).data.clone()
    }

    const fn bin(op: VBin, dst: VReg, a: VReg, b: VReg) -> VOp {
        VOp::Bin { op, dst, a, b }
    }

    /// Known encodings (as an assembler writes them).
    #[test]
    fn encodings() {
        let cases: Vec<(u32, VOp, &[u8])> = vec![
            (
                8,
                VOp::Load { dst: 3, base: 0 },
                &[0xf3, 0x0f, 0x6f, 0x1c, 0x06],
            ),
            (
                8,
                VOp::Store { src: 12, base: 3 },
                &[0xf3, 0x45, 0x0f, 0x7f, 0x24, 0x01],
            ),
            (8, bin(VBin::Add, 0, 0, 1), &[0x66, 0x0f, 0xd4, 0xc1]),
            (1, bin(VBin::Sub, 2, 2, 3), &[0x66, 0x0f, 0xf8, 0xd3]),
            (2, bin(VBin::Mul, 4, 4, 5), &[0x66, 0x0f, 0xd5, 0xe5]),
            (1, bin(VBin::And, 0, 0, 9), &[0x66, 0x41, 0x0f, 0xdb, 0xc1]),
            (1, bin(VBin::AndNot, 1, 0, 1), &[0x66, 0x0f, 0xdf, 0xc8]),
            (4, bin(VBin::Eq, 2, 2, 3), &[0x66, 0x0f, 0x76, 0xd3]),
            (2, bin(VBin::GtS, 2, 2, 3), &[0x66, 0x0f, 0x65, 0xd3]),
            (8, bin(VBin::FAdd, 0, 0, 1), &[0x66, 0x0f, 0x58, 0xc1]),
            (4, bin(VBin::FSub, 0, 0, 1), &[0x0f, 0x5c, 0xc1]),
            (8, bin(VBin::FMul, 0, 0, 1), &[0x66, 0x0f, 0x59, 0xc1]),
            (4, bin(VBin::FDiv, 0, 0, 1), &[0x0f, 0x5e, 0xc1]),
            (8, bin(VBin::FLt, 0, 0, 1), &[0x66, 0x0f, 0xc2, 0xc1, 0x01]),
            (4, bin(VBin::FNe, 0, 0, 1), &[0x0f, 0xc2, 0xc1, 0x04]),
            (
                8,
                VOp::Un {
                    op: VUn::FSqrt,
                    dst: 0,
                    a: 1,
                },
                &[0x66, 0x0f, 0x51, 0xc1],
            ),
            (
                8,
                VOp::Un {
                    op: VUn::SignMask,
                    dst: 4,
                    a: 4,
                },
                &[0x66, 0x0f, 0x72, 0xe4, 0x1f, 0x66, 0x0f, 0x70, 0xe4, 0xf5],
            ),
            (
                8,
                VOp::Shift {
                    op: VShift::Shl,
                    dst: 2,
                    a: 2,
                    n: 3,
                },
                &[0x66, 0x0f, 0x73, 0xf2, 0x03],
            ),
            (
                2,
                VOp::Shift {
                    op: VShift::ShrU,
                    dst: 2,
                    a: 2,
                    n: 7,
                },
                &[0x66, 0x0f, 0x71, 0xd2, 0x07],
            ),
            (1, VOp::Bail { a: 3 }, &[0x66, 0x0f, 0xd7, 0xd3, 0x85, 0xd2]),
            (8, VOp::Bail { a: 3 }, &[0x66, 0x0f, 0x50, 0xd3]),
        ];
        for (w, op, want) in cases {
            let got = code(w, false, Vec::new(), vec![op]);
            assert!(
                got.windows(want.len()).any(|x| x == want),
                "{op:?}: {want:x?}"
            );
        }
        let splat = code(8, false, vec![VOp::Splat { dst: 0, off: 16 }], Vec::new());
        let want = [0x66, 0x48, 0x0f, 0x6e, 0xc2, 0x66, 0x0f, 0x6c, 0xc0];
        assert!(splat.windows(want.len()).any(|x| x == want));
    }

    /// AVX2 encodings (as an assembler writes them), and `vzeroupper`
    /// after the loop.
    #[test]
    fn avx2_encodings() {
        let cases: Vec<(u32, VOp, &[u8])> = vec![
            (
                8,
                VOp::Load { dst: 3, base: 0 },
                &[0xc5, 0xfe, 0x6f, 0x1c, 0x06],
            ),
            (8, bin(VBin::Add, 0, 0, 1), &[0xc5, 0xfd, 0xd4, 0xc1]),
            (4, bin(VBin::Mul, 0, 0, 1), &[0xc4, 0xe2, 0x7d, 0x40, 0xc1]),
            (1, bin(VBin::AndNot, 1, 0, 1), &[0xc5, 0xf5, 0xdf, 0xc8]),
            (4, bin(VBin::MinS, 2, 2, 3), &[0xc4, 0xe2, 0x6d, 0x39, 0xd3]),
            (8, bin(VBin::GtS, 2, 2, 3), &[0xc4, 0xe2, 0x6d, 0x37, 0xd3]),
            (
                4,
                VOp::MaskStore {
                    src: 1,
                    mask: 2,
                    base: 0,
                },
                &[0xc4, 0xe2, 0x6d, 0x8e, 0x0c, 0x06],
            ),
            (
                4,
                VOp::Un {
                    op: VUn::SignMask,
                    dst: 4,
                    a: 4,
                },
                &[0xc5, 0xdd, 0x72, 0xe4, 0x1f],
            ),
            (1, VOp::Bail { a: 3 }, &[0xc5, 0xfd, 0xd7, 0xd3, 0x85, 0xd2]),
        ];
        for (w, op, want) in cases {
            let got = code_vb(w, 32, false, Vec::new(), vec![op]);
            assert!(
                got.windows(want.len()).any(|x| x == want),
                "{op:?}: {want:x?} in {got:x?}"
            );
            assert!(got.windows(3).any(|x| x == [0xc5, 0xf8, 0x77]));
        }
        let splat = code_vb(
            4,
            32,
            false,
            vec![VOp::Splat { dst: 0, off: 16 }],
            Vec::new(),
        );
        // vpbroadcastd ymm0, [rsp + io + 16]
        assert!(splat.windows(4).any(|x| x == [0xc4, 0xe2, 0x7d, 0x58]));
    }

    /// Windows: `xmm6` .. used are kept in `io` around the loop, the
    /// bases are `r8` ...
    #[test]
    fn windows_keeps_callee_saved_xmm() {
        let got = code(4, true, Vec::new(), vec![VOp::Load { dst: 7, base: 0 }]);
        // movdqu [rsp + 64], xmm6 / xmm7 and back.
        let save6 = [0xf3, 0x0f, 0x7f, 0x74, 0x24, 0x40];
        let load6 = [0xf3, 0x0f, 0x6f, 0x74, 0x24, 0x40];
        assert!(got.windows(6).any(|x| x == save6));
        assert!(got.windows(6).any(|x| x == load6));
        // movdqu xmm7, [r8 + rax]
        let ld = [0xf3, 0x41, 0x0f, 0x6f, 0x3c, 0x00];
        assert!(got.windows(6).any(|x| x == ld));
    }
}
