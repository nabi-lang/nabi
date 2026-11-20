//! `VLoop` on arm64 (NEON). Integer registers: `x0` the byte offset of
//! the vector at hand, `x1` the last offset a vector may start at, `x2..`
//! the bases; `x16` scratch. Vector registers `v0..v7`, `v16..v30` by
//! number, `v31` scratch: all caller-saved, and a `VLoop` clobbers what
//! a call does.

use crate::isa::a64::enc::{
    self, AddOp, Cond, LDRB, LDRD, LDRH, LDRQ, LDRS, LDRW, LDRX, Ls, Neon3, NeonMisc, NeonShift,
    NeonWide, R, STRD, STRQ, STRS, STRX, Shift, V,
};
use crate::isa::a64::{Em, SP, V16, V17, V18, X14, X16};
use crate::lir::{
    K, Mem, V as Val, VBin, VIO_BASES, VIO_BOUND, VIO_IV, VLoop, VOp, VReg, VShift, VUn, Vx, VxOp,
};

const OFF: u32 = 0;
const LIM: u32 = 1;
const SCRATCH: u32 = 31;

/// The bytes of the narrowest element of `v`: the offset's unit.
fn emin(v: &VLoop) -> u32 {
    v.elem.iter().copied().chain([v.w]).min().unwrap_or(v.w)
}

/// The number `vr` maps to register `q` (none for `v8`..`v15`).
const fn vnum(q: u32) -> VReg {
    assert!(q < 8 || q >= 16, "a callee-saved vector register");
    (if q < 8 { q } else { q - 8 }) as VReg
}

/// The register of vector register number `r`.
const fn vr(r: VReg) -> u32 {
    let r = r as u32;
    if r < 8 { r } else { r + 8 }
}

impl Em<'_> {
    pub(crate) fn vloop(&mut self, v: &VLoop) {
        self.flush(None);
        self.forget();
        self.flags = None;
        self.tested = None;
        let io = i64::from(self.slot_off[v.io as usize]);
        // The offset counts elements of the narrowest kind.
        let s = emin(v).trailing_zeros();
        self.ldst(LDRX, OFF, SP, io + i64::from(VIO_IV));
        self.ldst(LDRX, LIM, SP, io + i64::from(VIO_BOUND));
        // No vector fits below `bound - min_left`.
        let lim = R(LIM);
        self.ins(enc::add_imm(AddOp::Subs, true, lim, lim, v.min_left, false));
        let skip = self.pos();
        self.ins(enc::b_cond(Cond::LO, 0));
        for r in [OFF, LIM] {
            self.ins(enc::lsl(true, R(r), R(r), s));
        }
        for (n, d) in v.bases.iter().enumerate() {
            let r = 2 + n as u32;
            self.ldst(LDRX, r, SP, io + i64::from(VIO_BASES + 8 * n as u32));
            self.add_imm(r, r, i64::from(*d));
        }
        for op in &v.pre {
            self.vop(v, op, io, &mut Vec::new());
        }
        let (off, top) = (R(OFF), self.pos());
        self.ins(enc::cmp(true, off, lim));
        let mut exits = vec![self.pos()];
        self.ins(enc::b_cond(Cond::HI, 0));
        for op in &v.body {
            self.vop(v, op, io, &mut exits);
        }
        // The bytes of the narrowest elements a vector covers.
        let step = v.vb / v.w * emin(v);
        self.ins(enc::add_imm(AddOp::Add, true, off, off, step, false));
        let back = top as i64 - self.pos() as i64;
        self.ins(enc::b(false, back));
        for at in exits {
            self.patch19(at);
        }
        self.ins(enc::shr(false, true, off, off, s));
        self.ldst(STRX, OFF, SP, io + i64::from(VIO_IV));
        for &(r, off) in &v.post {
            self.vmem(STRQ, vr(r), io + i64::from(off));
        }
        self.patch19(skip);
        self.forget();
    }

    /// Point the 19-bit branch at `at` here.
    fn patch19(&mut self, at: usize) {
        let d = self.pos() as i64 - at as i64;
        self.a.patch19(at, d);
    }

    /// `ldr` / `str` (`op`) `q<q>` at `sp + off`.
    fn vmem(&mut self, op: Ls, q: u32, off: i64) {
        if off % 16 == 0 && (0..65536).contains(&off) {
            self.a.push(enc::ldst(op, q, enc::SP, (off / 16) as u32));
        } else {
            self.add_imm(X16, SP, off);
            self.a.push(enc::ldst(op, q, R(X16), 0));
        }
    }

    /// `op`; a `Bail` adds the position of its branch to `exits`.
    fn vop(&mut self, v: &VLoop, op: &VOp, io: i64, exits: &mut Vec<usize>) {
        let size = v.w.trailing_zeros();
        match *op {
            VOp::Load { dst, base } => self.vaccess(v, base, 16, false, vr(dst)),
            VOp::Store { src, base } => self.vaccess(v, base, 16, true, vr(src)),
            VOp::LoadExt { dst, base, signed } => {
                let mut e = v.elem[base as usize];
                let d = vr(dst);
                self.vaccess(v, base, 16 * e / v.w, false, d);
                // ushll / sshll #0: each step doubles the lanes' width.
                while e < v.w {
                    let op = if signed {
                        NeonShift::Sshll
                    } else {
                        NeonShift::Ushll
                    };
                    self.a.push(enc::neon_shift(op, 8 * e, V(d), V(d), 0));
                    e *= 2;
                }
            }
            VOp::StoreTrunc { src, base } => {
                let e = v.elem[base as usize];
                let (mut w, mut a) = (v.w, vr(src));
                // xtn: each step halves the lanes' width.
                while w > e {
                    let size = (w / 2).trailing_zeros();
                    self.a
                        .push(enc::neon_misc(NeonMisc::Xtn, size, V(SCRATCH), V(a)));
                    a = SCRATCH;
                    w /= 2;
                }
                self.vaccess(v, base, 16 * e / v.w, true, SCRATCH);
            }
            VOp::MaskStore { .. } => unreachable!("NEON stores no lanes under a mask"),
            VOp::Splat { dst, off } => {
                let ld = match v.w {
                    1 => LDRB,
                    2 => LDRH,
                    4 => LDRW,
                    _ => LDRX,
                };
                self.ldst(ld, X16, SP, io + i64::from(off));
                self.dup(vr(dst), v.w);
            }
            VOp::Bail { a } => {
                let t = V(SCRATCH);
                self.a
                    .push(enc::neon_misc(NeonMisc::Cmlt0, size, t, V(vr(a))));
                self.a.push(enc::neon_misc(NeonMisc::Umaxv16, 0, t, t));
                self.a.push(enc::umov_b0(R(X16), t));
                exits.push(self.pos());
                self.a.push(enc::cbz(true, R(X16), 0));
            }
            _ => self.vcalc(v.w, op),
        }
    }

    /// `x`: an operand a register holds is used there, any other is
    /// loaded into `v16` / `v17`; the result is made in its register, or
    /// in `v18` and stored.
    pub(crate) fn vx(&mut self, x: &Vx) {
        // A lane of a vector in memory is a scalar there.
        if let (VxOp::Lane(k, n), None) = (x.op, self.dst_v(x.a)) {
            let m = x.a.at(u32::from(n) * x.w);
            if k.is_float() {
                let r = self.freg(Val::M(m), k, &[]);
                self.store_fp(r, x.dst, k);
            } else {
                self.load(X16, Val::M(m), k, false);
                self.store_ext(X16, x.dst, k, Some(false));
            }
            return;
        }
        let mut ins = [V16, V17];
        // A load goes straight to the register holding the result.
        if x.op == VxOp::Mov {
            ins[0] = self.dst_v(x.dst).unwrap_or(V16);
        }
        for (n, m) in x.srcs().enumerate() {
            ins[n] = self.vin(m, ins[n]);
        }
        let d = self.dst_v(x.dst).unwrap_or(V18);
        let w = x.w;
        let op = match x.op {
            VxOp::Mov if self.dst_v(x.dst).is_none() => {
                self.vout(ins[0], x.dst);
                return;
            }
            VxOp::Mov => {
                if ins[0] != d {
                    self.ins(enc::vmov(V(d), V(ins[0])));
                }
                None
            }
            VxOp::Bin(op) => Some(VOp::Bin {
                op,
                dst: vnum(d),
                a: vnum(ins[0]),
                b: vnum(ins[1]),
            }),
            VxOp::Un(op) => Some(VOp::Un {
                op,
                dst: vnum(d),
                a: vnum(ins[0]),
            }),
            VxOp::Const(bits) => Some(VOp::Const { dst: vnum(d), bits }),
            // A float in a register splats from there.
            VxOp::Splat(k) if k.is_float() && self.dst_v(x.a).is_some() => {
                let s = self.freg(Val::M(x.a), k, &[]);
                self.flush_reg(d);
                self.ins(enc::dup_elem(w, 0, V(d), V(s)));
                self.clobber(d);
                None
            }
            VxOp::Splat(k) => {
                let ik = match k {
                    K::F32 => K::I32,
                    K::F64 => K::I64,
                    k => k,
                };
                self.load(X16, Val::M(x.a), ik, false);
                self.flush_reg(d);
                self.dup(d, w);
                self.clobber(d);
                None
            }
            VxOp::Insert(k, n) => {
                // The scalar first: its register may be the result's.
                let s = if k.is_float() {
                    self.freg(Val::M(x.b), k, &[ins[0], d])
                } else {
                    self.reg(Val::M(x.b), k, false, &[])
                };
                if ins[0] != d {
                    self.ins(enc::vmov(V(d), V(ins[0])));
                }
                let n = u32::from(n);
                if k.is_float() {
                    self.ins(enc::ins_lane(w, n, V(d), V(s)));
                } else {
                    self.ins(enc::ins_gpr(w, n, V(d), R(s)));
                }
                None
            }
            VxOp::Sum(k) => {
                self.ins(enc::addv(w, V(V17), V(ins[0])));
                self.ins(enc::umov(w, 0, R(X16), V(V17)));
                self.store_ext(X16, x.dst, k, Some(false));
                return;
            }
            VxOp::Lane(k, n) => {
                let n = u32::from(n);
                if k.is_float() {
                    // `d`: the result's register, or a scratch one.
                    self.ins(enc::dup_lane(w, n, V(d), V(ins[0])));
                    if self.dst_v(x.dst).is_none() {
                        self.store_fp(d, x.dst, k);
                    }
                } else {
                    self.ins(enc::umov(w, n, R(X16), V(ins[0])));
                    self.store_ext(X16, x.dst, k, Some(false));
                }
                return;
            }
        };
        if let Some(op) = op {
            self.flush_reg(d);
            self.vcalc(w, &op);
            self.clobber(d);
        }
        if self.dst_v(x.dst).is_none() {
            self.vout(d, x.dst);
        }
    }

    /// The register holding vector `m`: its own, or `tmp` loaded.
    fn vin(&mut self, m: Mem, tmp: u32) -> u32 {
        if let Some(r) = self.dst_v(m) {
            return r;
        }
        let (b, o) = self.base(m, X14);
        self.ldst(LDRQ, tmp, b, o);
        tmp
    }

    /// A C vector argument or result: the 16 bytes of `m` into `q`.
    pub(crate) fn vec_load(&mut self, q: u32, m: Mem) {
        if let Mem::Frame(s, _) = m {
            self.flush(Some(s));
        }
        self.flush_reg(q);
        self.clobber(q);
        let got = self.vin(m, q);
        if got != q {
            self.ins(enc::vmov(V(q), V(got)));
        }
        self.clobber(q);
    }

    /// A C vector result or parameter: `q` into the 16 bytes of `m`.
    pub(crate) fn vec_store(&mut self, q: u32, m: Mem) {
        match self.dst_v(m) {
            Some(d) if d != q => self.ins(enc::vmov(V(d), V(q))),
            Some(_) => {}
            None => self.vout(q, m),
        }
    }

    /// Store vector register `q` to `m`.
    fn vout(&mut self, q: u32, m: Mem) {
        let (b, o) = self.base(m, X14);
        self.wrote(m, 16);
        self.ldst(STRQ, q, b, o);
    }

    /// `op` on registers only (`Const`, `Bin`, `Un`, `Shift`, `Dot`), lanes
    /// of `w` bytes.
    pub(crate) fn vcalc(&mut self, w: u32, op: &VOp) {
        let size = w.trailing_zeros();
        match *op {
            VOp::Const { dst, bits } => {
                let ones = if w == 8 {
                    u64::MAX
                } else {
                    (1u64 << (8 * w)) - 1
                };
                if bits == 0 || bits == ones {
                    self.a.push(enc::movi(bits != 0, V(vr(dst))));
                } else {
                    self.mov_imm(X16, bits);
                    self.dup(vr(dst), w);
                }
            }
            VOp::Bin {
                op: op @ (VBin::MulHiS | VBin::MulHiU),
                dst,
                a,
                b,
            } => {
                // The full products of the low and the high lanes; their
                // odd (high) halves.
                let (d, n, m) = (V(vr(dst)), V(vr(a)), V(vr(b)));
                let (lo, hi) = if op == VBin::MulHiS {
                    (NeonWide::Smull, NeonWide::Smull2)
                } else {
                    (NeonWide::Umull, NeonWide::Umull2)
                };
                let t = V(SCRATCH);
                self.a.push(enc::neon_wide(lo, size, t, n, m));
                self.a.push(enc::neon_wide(hi, size, d, n, m));
                self.a.push(enc::neon_wide(NeonWide::Uzp2, size, d, t, d));
            }
            VOp::Bin { op, dst, a, b } => {
                let (d, n, m) = (V(vr(dst)), V(vr(a)), V(vr(b)));
                let (op3, swap) = match op {
                    VBin::Add => (Neon3::Add, false),
                    VBin::Sub => (Neon3::Sub, false),
                    VBin::Mul => (Neon3::Mul, false),
                    VBin::And => (Neon3::And, false),
                    VBin::AndNot => (Neon3::Bic, false),
                    VBin::Or => (Neon3::Orr, false),
                    VBin::Xor => (Neon3::Eor, false),
                    VBin::Eq => (Neon3::Cmeq, false),
                    VBin::GtS => (Neon3::Cmgt, false),
                    VBin::GtU => (Neon3::Cmhi, false),
                    VBin::MaxS => (Neon3::Smax, false),
                    VBin::MaxU => (Neon3::Umax, false),
                    VBin::MinS => (Neon3::Smin, false),
                    VBin::MinU => (Neon3::Umin, false),
                    VBin::FAdd => (Neon3::Fadd, false),
                    VBin::FSub => (Neon3::Fsub, false),
                    VBin::FMul => (Neon3::Fmul, false),
                    VBin::FDiv => (Neon3::Fdiv, false),
                    VBin::FEq | VBin::FNe => (Neon3::Fcmeq, false),
                    // a < b: b > a; a <= b: b >= a.
                    VBin::FLt => (Neon3::Fcmgt, true),
                    VBin::FLe => (Neon3::Fcmge, true),
                    VBin::FMin => (Neon3::Fmin, false),
                    VBin::FMax => (Neon3::Fmax, false),
                    VBin::MulHiS | VBin::MulHiU => unreachable!("a high product above"),
                };
                let (n, m) = if swap { (m, n) } else { (n, m) };
                self.a.push(enc::neon3(op3, size, d, n, m));
                if op == VBin::FNe {
                    self.a.push(enc::neon_misc(NeonMisc::Not, 0, d, d));
                }
            }
            VOp::Un { op, dst, a } => {
                let (d, n) = (V(vr(dst)), V(vr(a)));
                let op = match op {
                    VUn::FSqrt => NeonMisc::Fsqrt,
                    VUn::SignMask => NeonMisc::Cmlt0,
                    VUn::Abs => NeonMisc::Abs,
                };
                self.a.push(enc::neon_misc(op, size, d, n));
            }
            VOp::Shift { op, dst, a, n } => {
                let (d, r) = (V(vr(dst)), V(vr(a)));
                let n = u32::from(n);
                if n == 0 {
                    self.a.push(enc::vmov(d, r));
                    return;
                }
                let op = match op {
                    VShift::Shl => NeonShift::Shl,
                    VShift::ShrS => NeonShift::Sshr,
                    VShift::ShrU => NeonShift::Ushr,
                };
                self.a.push(enc::neon_shift(op, 8 * w, d, r, n));
            }
            VOp::Dot { acc, a, b, signed } => {
                let op = if signed {
                    NeonWide::Sdot
                } else {
                    NeonWide::Udot
                };
                let i = enc::neon_wide(op, 0, V(vr(acc)), V(vr(a)), V(vr(b)));
                self.a.push(i);
            }
            VOp::Fma { dst, a, b, c } => {
                // fmla adds into its destination: a copy of `c`.
                let (d, a, b, c) = (vr(dst), vr(a), vr(b), vr(c));
                let t = if d == c || (d != a && d != b) {
                    d
                } else {
                    SCRATCH
                };
                if t != c {
                    self.a.push(enc::vmov(V(t), V(c)));
                }
                self.a.push(enc::neon3(Neon3::Fmla, size, V(t), V(a), V(b)));
                if t != d {
                    self.a.push(enc::vmov(V(d), V(t)));
                }
            }
            _ => unreachable!("a memory access or a bail"),
        }
    }

    /// `ldr` / `str` (`store`) of `bytes` (16, 8 or 4) between `q` and
    /// the elements of base `b` at the offset.
    fn vaccess(&mut self, v: &VLoop, b: u8, bytes: u32, store: bool, q: u32) {
        let base = 2 + u32::from(b);
        let k = (v.elem[b as usize] / emin(v)).trailing_zeros();
        let op = match (bytes, store) {
            (16, false) => LDRQ,
            (16, true) => STRQ,
            (8, false) => LDRD,
            (8, true) => STRD,
            (_, false) => LDRS,
            (_, true) => STRS,
        };
        if k == 0 {
            self.a.push(enc::ldst_reg(op, q, R(base), R(OFF), false));
        } else {
            // add x16, base, x0, lsl #k
            let x16 = R(X16);
            let add = enc::add_reg(AddOp::Add, true, x16, R(base), R(OFF), Shift::Lsl, k);
            self.a.push(add);
            self.a.push(enc::ldst(op, q, x16, 0));
        }
    }

    /// `dup v<d>.<w>, x16`.
    fn dup(&mut self, d: u32, w: u32) {
        self.a.push(enc::dup(w, V(d), R(X16)));
    }
}

#[cfg(test)]
mod tests {
    use crate::lir::{Func, Inst, Mem, VBin, VLoop, VOp, VReg, VShift, VUn, Vx, VxOp};
    use crate::obj::{Obj, Sect};

    /// The words of a function holding one `VLoop` of lane width `w`.
    fn code(w: u32, pre: Vec<VOp>, body: Vec<VOp>) -> Vec<u32> {
        let mut obj = Obj::new(false, true, false);
        let sym = obj.sym("f");
        let mut f = Func {
            sym,
            ..Func::default()
        };
        let io = f.slot(64, 16);
        f.insts.push(Inst::VLoop(Box::new(VLoop {
            io,
            w,
            vb: 16,
            min_left: 16 / w,
            bases: vec![16; 4],
            elem: vec![w; 4],
            pre,
            body,
            post: Vec::new(),
            regs: 23,
            save: 64,
        })));
        f.insts.push(Inst::Ret { vals: Vec::new() });
        super::super::emit(&mut obj, &f);
        obj.sect(Sect::Text)
            .data
            .chunks(4)
            .map(|c| u32::from_le_bytes(c.try_into().expect("a word")))
            .collect()
    }

    /// Straight-line vector code with every operand in the frame: loads
    /// into `v16` / `v17`, the result made in `v18` and stored.
    #[test]
    fn straight_line_through_memory() {
        let mut obj = Obj::new(false, true, false);
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
        let words: Vec<u32> = obj
            .sect(Sect::Text)
            .data
            .chunks(4)
            .map(|c| u32::from_le_bytes(c.try_into().expect("a word")))
            .collect();
        // fadd v18.4s, v16.4s, v17.4s between two q loads and a q store.
        let at = words.iter().position(|&w| w == 0x4e31_d612).expect("fadd");
        let ldq = |w: u32| w & 0xffc0_001f == 0x3dc0_0010 || w & 0xffc0_001f == 0x3dc0_0011;
        assert!(ldq(words[at - 2]) && ldq(words[at - 1]), "{words:x?}");
        assert_eq!(words[at + 1] & 0xffc0_001f, 0x3d80_0012, "{words:x?}");
    }

    const fn bin(op: VBin, dst: VReg, a: VReg, b: VReg) -> VOp {
        VOp::Bin { op, dst, a, b }
    }

    /// Known encodings (as an assembler writes them).
    #[test]
    fn encodings() {
        let cases: Vec<(u32, VOp, &[u32])> = vec![
            (8, bin(VBin::Add, 0, 1, 2), &[0x4ee2_8420]),
            (1, bin(VBin::Sub, 3, 4, 5), &[0x6e25_8483]),
            (2, bin(VBin::Mul, 6, 7, 8), &[0x4e70_9ce6]),
            (1, bin(VBin::And, 0, 1, 2), &[0x4e22_1c20]),
            (1, bin(VBin::AndNot, 0, 1, 2), &[0x4e62_1c20]),
            (1, bin(VBin::Or, 0, 1, 2), &[0x4ea2_1c20]),
            (1, bin(VBin::Xor, 0, 1, 2), &[0x6e22_1c20]),
            (4, bin(VBin::Eq, 0, 1, 2), &[0x6ea2_8c20]),
            (2, bin(VBin::GtS, 0, 1, 2), &[0x4e62_3420]),
            (8, bin(VBin::GtU, 0, 1, 2), &[0x6ee2_3420]),
            (4, bin(VBin::FAdd, 0, 1, 2), &[0x4e22_d420]),
            (8, bin(VBin::FSub, 0, 1, 2), &[0x4ee2_d420]),
            (8, bin(VBin::FMul, 0, 1, 2), &[0x6e62_dc20]),
            (4, bin(VBin::FDiv, 0, 1, 2), &[0x6e22_fc20]),
            (8, bin(VBin::FEq, 0, 1, 2), &[0x4e62_e420]),
            (8, bin(VBin::FNe, 0, 1, 2), &[0x4e62_e420, 0x6e20_5800]),
            (8, bin(VBin::FLt, 0, 1, 2), &[0x6ee1_e440]),
            (4, bin(VBin::FLe, 0, 1, 2), &[0x6e21_e440]),
            (
                8,
                VOp::Un {
                    op: VUn::FSqrt,
                    dst: 0,
                    a: 1,
                },
                &[0x6ee1_f820],
            ),
            (
                4,
                VOp::Un {
                    op: VUn::FSqrt,
                    dst: 0,
                    a: 1,
                },
                &[0x6ea1_f820],
            ),
            (
                2,
                VOp::Un {
                    op: VUn::SignMask,
                    dst: 0,
                    a: 1,
                },
                &[0x4e60_a820],
            ),
            (
                4,
                VOp::Shift {
                    op: VShift::Shl,
                    dst: 0,
                    a: 1,
                    n: 3,
                },
                &[0x4f23_5420],
            ),
            (
                2,
                VOp::Shift {
                    op: VShift::ShrS,
                    dst: 0,
                    a: 1,
                    n: 5,
                },
                &[0x4f1b_0420],
            ),
            (
                8,
                VOp::Shift {
                    op: VShift::ShrU,
                    dst: 0,
                    a: 1,
                    n: 63,
                },
                &[0x6f41_0420],
            ),
            (4, VOp::Load { dst: 3, base: 0 }, &[0x3ce0_6843]),
            (4, VOp::Store { src: 3, base: 3 }, &[0x3ca0_68a3]),
        ];
        for (w, op, want) in cases {
            let got = code(w, Vec::new(), vec![op]);
            assert!(
                got.windows(want.len()).any(|x| x == want),
                "{op:?}: {want:x?} in {got:x?}"
            );
        }
        let got = code(
            1,
            vec![
                VOp::Const { dst: 0, bits: 0 },
                VOp::Const { dst: 1, bits: 0xff },
            ],
            Vec::new(),
        );
        assert!(got.contains(&0x6f00_e400) && got.contains(&0x6f07_e7e1));
        let got = code(8, vec![VOp::Splat { dst: 0, off: 16 }], Vec::new());
        assert!(got.contains(&0x4e08_0e00));
    }
}
