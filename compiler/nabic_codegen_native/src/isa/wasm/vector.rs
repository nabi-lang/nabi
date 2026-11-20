//! `VLoop` on wasm32 (SIMD128): locals for the offset of the vector at
//! hand, the last offset a vector may start at, the bases and each
//! vector register, the loop a `block` around a `loop`.

use crate::isa::wasm::enc::{MemOp, Num, Simd, V128, VLoad};
use crate::isa::wasm::{Em, I32};
use crate::lir::{
    K, Mem, V, VBin, VIO_BASES, VIO_BOUND, VIO_IV, VLoop, VOp, VShift, VUn, Vx, VxOp,
};

const SHR_S: [Simd; 4] = [
    Simd::I8x16ShrS,
    Simd::I16x8ShrS,
    Simd::I32x4ShrS,
    Simd::I64x2ShrS,
];

/// `lane` repeated over 16 bytes.
fn splat(lane: &[u8]) -> [u8; 16] {
    let mut b = [0; 16];
    for (i, x) in b.iter_mut().enumerate() {
        *x = lane[i % lane.len()];
    }
    b
}

/// The bytes of the narrowest element of `v`: the offset's unit.
fn emin(v: &VLoop) -> u32 {
    v.elem.iter().copied().chain([v.w]).min().unwrap_or(v.w)
}

impl Em<'_> {
    pub(crate) fn vloop(&mut self, v: &VLoop) {
        let io = self.slot_off[v.io as usize];
        // The offset counts elements of the narrowest kind.
        let s = emin(v).trailing_zeros() as i32;
        let off = self.new_local(I32);
        let lim = self.new_local(I32);
        let bases: Vec<u32> = v.bases.iter().map(|_| self.new_local(I32)).collect();
        // The registers, then a scratch one.
        let regs: Vec<u32> = (0..=v.regs).map(|_| self.new_local(V128)).collect();
        self.a.block();
        // No vector fits below `min_left`.
        self.io_word(io + VIO_BOUND);
        self.i32c(v.min_left as i32);
        self.a.num(Num::I32LtU);
        self.a.br_if(0);
        self.io_word(io + VIO_BOUND);
        self.i32c(v.min_left as i32);
        self.a.num(Num::I32Sub);
        self.i32c(s);
        self.a.num(Num::I32Shl);
        self.set(lim);
        self.io_word(io + VIO_IV);
        self.i32c(s);
        self.a.num(Num::I32Shl);
        self.set(off);
        for (n, d) in v.bases.iter().enumerate() {
            self.io_word(io + VIO_BASES + 8 * n as u32);
            self.i32c(*d);
            self.a.num(Num::I32Add);
            self.set(bases[n]);
        }
        for op in &v.pre {
            self.vop(v, op, io, off, &bases, &regs);
        }
        self.a.block();
        self.a.loop_();
        self.get(off);
        self.get(lim);
        self.a.num(Num::I32GtU);
        self.a.br_if(1);
        for op in &v.body {
            self.vop(v, op, io, off, &bases, &regs);
        }
        self.get(off);
        self.i32c((v.vb / v.w * emin(v)) as i32);
        self.a.num(Num::I32Add);
        self.set(off);
        self.a.br(0);
        self.a.end();
        self.a.end();
        self.get(self.fp);
        self.get(off);
        self.i32c(s);
        self.a.num(Num::I32ShrU);
        self.a.mem(MemOp::I32Store, 2, io + VIO_IV);
        for &(r, o) in &v.post {
            self.get(self.fp);
            self.get(regs[r as usize]);
            self.a.v128_store(0, io + o);
        }
        self.a.end();
    }

    /// Push the `i32` word at frame offset `at`.
    fn io_word(&mut self, at: u32) {
        self.get(self.fp);
        self.a.mem(MemOp::I32Load, 2, at);
    }

    /// Push the address of base `b`'s elements at the offset.
    fn vaddr(&mut self, v: &VLoop, b: u8, off: u32, bases: &[u32]) {
        self.get(bases[b as usize]);
        self.get(off);
        let k = (v.elem[b as usize] / emin(v)).trailing_zeros();
        if k != 0 {
            self.i32c(k as i32);
            self.a.num(Num::I32Shl);
        }
        self.a.num(Num::I32Add);
    }

    fn vop(&mut self, v: &VLoop, op: &VOp, io: u32, off: u32, bases: &[u32], regs: &[u32]) {
        let wi = v.w.trailing_zeros() as usize;
        let float = v.w == 8;
        let tmp = regs[v.regs as usize];
        match *op {
            VOp::Load { dst, base } => {
                self.vaddr(v, base, off, bases);
                self.a.vload(VLoad::V128, 0, 0);
                self.set(regs[dst as usize]);
            }
            VOp::Store { src, base } => {
                self.vaddr(v, base, off, bases);
                self.get(regs[src as usize]);
                self.a.v128_store(0, 0);
            }
            VOp::LoadExt { dst, base, signed } => {
                let mut e = v.elem[base as usize];
                self.vaddr(v, base, off, bases);
                let u = usize::from(!signed);
                if v.w == 2 * e {
                    const EXT: [[VLoad; 2]; 3] = [
                        [VLoad::I16x8Load8x8S, VLoad::I16x8Load8x8U],
                        [VLoad::I32x4Load16x4S, VLoad::I32x4Load16x4U],
                        [VLoad::I64x2Load32x2S, VLoad::I64x2Load32x2U],
                    ];
                    self.a.vload(EXT[e.trailing_zeros() as usize][u], 0, 0);
                } else {
                    self.a.vload(VLoad::Load32Zero, 0, 0);
                    while e < v.w {
                        const LOW: [[Simd; 2]; 3] = [
                            [Simd::I16x8ExtendLowI8x16S, Simd::I16x8ExtendLowI8x16U],
                            [Simd::I32x4ExtendLowI16x8S, Simd::I32x4ExtendLowI16x8U],
                            [Simd::I64x2ExtendLowI32x4S, Simd::I64x2ExtendLowI32x4U],
                        ];
                        self.a.simd(LOW[e.trailing_zeros() as usize][u]);
                        e *= 2;
                    }
                }
                self.set(regs[dst as usize]);
            }
            VOp::StoreTrunc { src, base } => {
                // The low halves of the lanes into the low 8 bytes (only
                // `w` twice `e`).
                self.vaddr(v, base, off, bases);
                self.get(regs[src as usize]);
                if v.w == 8 {
                    self.get(regs[src as usize]);
                    self.a
                        .shuffle(&[0, 1, 2, 3, 8, 9, 10, 11, 0, 1, 2, 3, 8, 9, 10, 11]);
                } else {
                    // Masked to the low half, the unsigned narrowing is
                    // exact.
                    let m: u64 = if v.w == 2 { 0x00ff } else { 0xffff };
                    self.a.v128_const(&splat(&m.to_le_bytes()[..v.w as usize]));
                    self.a.simd(Simd::V128And);
                    self.a.local_tee(tmp);
                    self.get(tmp);
                    self.a.simd(if v.w == 2 {
                        Simd::I8x16NarrowI16x8U
                    } else {
                        Simd::I16x8NarrowI32x4U
                    });
                }
                self.a.store64_lane(0, 0, 0);
            }
            VOp::MaskStore { .. } => unreachable!("SIMD128 stores no lanes under a mask"),
            VOp::Splat { dst, off: at } => {
                self.get(self.fp);
                let (ld, align) = [
                    (MemOp::I32Load8U, 0),
                    (MemOp::I32Load16U, 1),
                    (MemOp::I32Load, 2),
                    (MemOp::I64Load, 3),
                ][wi];
                self.a.mem(ld, align, io + at);
                self.a.simd(
                    [
                        Simd::I8x16Splat,
                        Simd::I16x8Splat,
                        Simd::I32x4Splat,
                        Simd::I64x2Splat,
                    ][wi],
                );
                self.set(regs[dst as usize]);
            }
            VOp::Const { dst, bits } => {
                self.a
                    .v128_const(&splat(&bits.to_le_bytes()[..v.w as usize]));
                self.set(regs[dst as usize]);
            }
            VOp::Bin {
                op: op @ (VBin::MulHiS | VBin::MulHiU),
                dst,
                a,
                b,
            } => {
                // The products of the low and the high lanes, widened;
                // their high halves.
                let signed = op == VBin::MulHiS;
                let (lo, hi) = match (v.w, signed) {
                    (2, true) => (Simd::I32x4ExtmulLowI16x8S, Simd::I32x4ExtmulHighI16x8S),
                    (2, false) => (Simd::I32x4ExtmulLowI16x8U, Simd::I32x4ExtmulHighI16x8U),
                    (4, true) => (Simd::I64x2ExtmulLowI32x4S, Simd::I64x2ExtmulHighI32x4S),
                    (4, false) => (Simd::I64x2ExtmulLowI32x4U, Simd::I64x2ExtmulHighI32x4U),
                    _ => unreachable!("no SIMD128 high product of {}-byte lanes", v.w),
                };
                for half in <[Simd; 2]>::from((lo, hi)) {
                    self.get(regs[a as usize]);
                    self.get(regs[b as usize]);
                    self.a.simd(half);
                }
                let w = v.w as u8;
                let mut lanes = [0u8; 16];
                for (k, l) in lanes.iter_mut().enumerate() {
                    let (lane, byte) = (k as u8 / w, k as u8 % w);
                    *l = 2 * w * lane + w + byte;
                }
                self.a.shuffle(&lanes);
                self.set(regs[dst as usize]);
            }
            VOp::Bin { op, dst, a, b } => {
                self.get(regs[a as usize]);
                self.get(regs[b as usize]);
                self.vbin(v.w, op);
                self.set(regs[dst as usize]);
            }
            VOp::Un { op, dst, a } => {
                self.get(regs[a as usize]);
                match op {
                    VUn::FSqrt => self.a.simd(if float {
                        Simd::F64x2Sqrt
                    } else {
                        Simd::F32x4Sqrt
                    }),
                    VUn::Abs => unreachable!("no lane magnitudes here"),
                    VUn::SignMask => {
                        self.i32c(8 * v.w as i32 - 1);
                        self.a.simd(SHR_S[wi]);
                    }
                }
                self.set(regs[dst as usize]);
            }
            VOp::Shift { op, dst, a, n } => {
                self.get(regs[a as usize]);
                self.i32c(i32::from(n));
                self.a.simd(match op {
                    VShift::Shl => [
                        Simd::I8x16Shl,
                        Simd::I16x8Shl,
                        Simd::I32x4Shl,
                        Simd::I64x2Shl,
                    ][wi],
                    VShift::ShrS => SHR_S[wi],
                    VShift::ShrU => [
                        Simd::I8x16ShrU,
                        Simd::I16x8ShrU,
                        Simd::I32x4ShrU,
                        Simd::I64x2ShrU,
                    ][wi],
                });
                self.set(regs[dst as usize]);
            }
            VOp::Dot { .. } => unreachable!("no dot products here"),
            VOp::Fma { .. } => unreachable!("no fused multiply-add here"),
            VOp::Bail { a } => {
                self.get(regs[a as usize]);
                self.a.simd(
                    [
                        Simd::I8x16Bitmask,
                        Simd::I16x8Bitmask,
                        Simd::I32x4Bitmask,
                        Simd::I64x2Bitmask,
                    ][wi],
                );
                self.a.br_if(1);
            }
        }
    }
    /// `a op b` of the two vectors on the stack (not a high product).
    fn vbin(&mut self, lw: u32, op: VBin) {
        let wi = lw.trailing_zeros() as usize;
        let float = lw == 8;
        let f = |x: Simd, y: Simd| if float { y } else { x };
        // A lane width the vectorizer never asks of an operation
        // has no instruction.
        let w = |l: [Option<Simd>; 4]| l[wi].expect("a SIMD128 lane operation");
        self.a.simd(match op {
            VBin::Add => [
                Simd::I8x16Add,
                Simd::I16x8Add,
                Simd::I32x4Add,
                Simd::I64x2Add,
            ][wi],
            VBin::Sub => [
                Simd::I8x16Sub,
                Simd::I16x8Sub,
                Simd::I32x4Sub,
                Simd::I64x2Sub,
            ][wi],
            VBin::Mul => w([
                None,
                Some(Simd::I16x8Mul),
                Some(Simd::I32x4Mul),
                Some(Simd::I64x2Mul),
            ]),
            VBin::And => Simd::V128And,
            VBin::AndNot => Simd::V128Andnot,
            VBin::Or => Simd::V128Or,
            VBin::Xor => Simd::V128Xor,
            VBin::Eq => [Simd::I8x16Eq, Simd::I16x8Eq, Simd::I32x4Eq, Simd::I64x2Eq][wi],
            VBin::GtS => [
                Simd::I8x16GtS,
                Simd::I16x8GtS,
                Simd::I32x4GtS,
                Simd::I64x2GtS,
            ][wi],
            VBin::GtU => w([
                Some(Simd::I8x16GtU),
                Some(Simd::I16x8GtU),
                Some(Simd::I32x4GtU),
                None,
            ]),
            VBin::MinS => w([
                Some(Simd::I8x16MinS),
                Some(Simd::I16x8MinS),
                Some(Simd::I32x4MinS),
                None,
            ]),
            VBin::MinU => w([
                Some(Simd::I8x16MinU),
                Some(Simd::I16x8MinU),
                Some(Simd::I32x4MinU),
                None,
            ]),
            VBin::MaxS => w([
                Some(Simd::I8x16MaxS),
                Some(Simd::I16x8MaxS),
                Some(Simd::I32x4MaxS),
                None,
            ]),
            VBin::MaxU => w([
                Some(Simd::I8x16MaxU),
                Some(Simd::I16x8MaxU),
                Some(Simd::I32x4MaxU),
                None,
            ]),
            VBin::FAdd => f(Simd::F32x4Add, Simd::F64x2Add),
            VBin::FSub => f(Simd::F32x4Sub, Simd::F64x2Sub),
            VBin::FMul => f(Simd::F32x4Mul, Simd::F64x2Mul),
            VBin::FDiv => f(Simd::F32x4Div, Simd::F64x2Div),
            VBin::FEq => f(Simd::F32x4Eq, Simd::F64x2Eq),
            VBin::FNe => f(Simd::F32x4Ne, Simd::F64x2Ne),
            VBin::FLt => f(Simd::F32x4Lt, Simd::F64x2Lt),
            VBin::FLe => f(Simd::F32x4Le, Simd::F64x2Le),
            VBin::FMin => f(Simd::F32x4Min, Simd::F64x2Min),
            VBin::FMax => f(Simd::F32x4Max, Simd::F64x2Max),
            VBin::MulHiS | VBin::MulHiU => unreachable!("a high product above"),
        });
    }

    /// `x`: through memory, the operands loaded and the result stored.
    pub(crate) fn vx(&mut self, x: &Vx) {
        let wi = x.w.trailing_zeros() as usize;
        match x.op {
            // A lane is a scalar in memory.
            VxOp::Lane(k, n) => {
                let s = self.st_begin(x.dst, k);
                self.load(x.a.at(u32::from(n) * x.w), k);
                self.st_end(s);
                return;
            }
            VxOp::Insert(k, n) => {
                if x.a != x.dst {
                    let off = self.base(x.dst);
                    self.vget(x.a);
                    self.a.v128_store(0, off);
                }
                let s = self.st_begin(x.dst.at(u32::from(n) * x.w), k);
                self.val(V::M(x.b), k);
                self.st_end(s);
                return;
            }
            _ => {}
        }
        let off = self.base(x.dst);
        match x.op {
            VxOp::Mov => self.vget(x.a),
            VxOp::Bin(op) => {
                self.vget(x.a);
                self.vget(x.b);
                self.vbin(x.w, op);
            }
            VxOp::Un(VUn::FSqrt) => {
                self.vget(x.a);
                self.a.simd(if x.w == 8 {
                    Simd::F64x2Sqrt
                } else {
                    Simd::F32x4Sqrt
                });
            }
            VxOp::Un(op) => unreachable!("no straight-line {op:?}"),
            VxOp::Const(bits) => {
                self.a
                    .v128_const(&splat(&bits.to_le_bytes()[..x.w as usize]));
            }
            VxOp::Splat(k) => {
                let ik = match k {
                    K::F32 => K::I32,
                    K::F64 => K::I64,
                    k => k,
                };
                self.load(x.a, ik);
                self.a.simd(
                    [
                        Simd::I8x16Splat,
                        Simd::I16x8Splat,
                        Simd::I32x4Splat,
                        Simd::I64x2Splat,
                    ][wi],
                );
            }
            VxOp::Insert(..) | VxOp::Lane(..) => unreachable!("scalar stores above"),
            VxOp::Sum(_) => unreachable!("sums are lanes here"),
        }
        self.a.v128_store(0, off);
    }

    /// Push vector `m`.
    fn vget(&mut self, m: Mem) {
        let off = self.base(m);
        self.a.vload(VLoad::V128, 0, off);
    }
}
