//! `VLoop` on powerpc64 (POWER8: VMX and VSX). `r3` the byte offset of
//! the vector at hand, `r4` the last offset a vector may start at, `r11
//! r12 r0` scratch, the bases in `r5 .. r10`. Vector registers `v0 ..
//! v17` by number (VSX `vs32 ..`), `v18 v19` scratch, all volatile; a
//! `VLoop` clobbers what a call does. `lxvd2x` / `stxvd2x` keep the
//! doublewords of a vector in the other order in little-endian mode:
//! every lane is computed alike and goes back where it came from, so the
//! order never shows (no operation moves elements between lanes).

use crate::isa::ppc64::enc::{self, CR6_EQ, Cond, GT, LT, R, V, Vop, Xv};
use crate::isa::ppc64::{Alu, Em, R11, R12};
use crate::lir::{K, Mem, VBin, VIO_BASES, VIO_BOUND, VIO_IV, VLoop, VOp, VReg, VShift, VUn};

const OFF: u32 = 3;
const LIM: u32 = 4;
const T1: u32 = 18;
const T2: u32 = 19;
impl Em<'_> {
    pub(crate) fn vloop(&mut self, v: &VLoop) {
        let (sp, io) = self.mem(Mem::Frame(v.io, 0), R11);
        let s = v.w.trailing_zeros();
        self.ld_word(OFF, sp, io + i64::from(VIO_IV));
        self.ld_word(LIM, sp, io + i64::from(VIO_BOUND));
        // No vector fits below `min_left`.
        self.cmpl_imm(LIM, i64::from(v.min_left));
        let skip = self.b_fwd(Cond::t(LT));
        self.addi(LIM, LIM, -i64::from(v.min_left));
        self.sldi(OFF, OFF, s);
        self.sldi(LIM, LIM, s);
        for (n, d) in v.bases.iter().enumerate() {
            let r = 5 + n as u32;
            self.ld_word(r, sp, io + i64::from(VIO_BASES + 8 * n as u32));
            self.add_imm(r, r, i64::from(*d));
        }
        for op in &v.pre {
            self.vop(v, op, sp, io, &mut Vec::new());
        }
        let top = self.pos();
        self.ins(enc::cmp(false, R(OFF), R(LIM)));
        let mut exits = vec![self.b_fwd(Cond::t(GT))];
        for op in &v.body {
            self.vop(v, op, sp, io, &mut exits);
        }
        self.addi(OFF, OFF, i64::from(v.vb));
        self.b_back(Cond::ALWAYS, top);
        for e in exits {
            self.here(e);
        }
        self.srdi(OFF, OFF, s);
        self.st_word(OFF, sp, io + i64::from(VIO_IV));
        for &(r, off) in &v.post {
            self.add_imm(R12, sp, io + i64::from(off));
            self.ins(enc::stxvd2x(V(u32::from(r)), R(0), R(R12)));
        }
        self.here(skip);
    }

    /// `vs32 + t` = the doubleword in `r12` in both halves.
    fn splat_r12(&mut self, t: u32) {
        self.ins(enc::mtvsrd(V(t), R(R12)));
        self.ins(enc::xxspltd0(V(t), V(t)));
    }

    /// `vt` = the 64 bits `x` in both halves.
    fn vconst(&mut self, t: u32, x: u64) {
        if x == 0 {
            self.ins(enc::vx(Vop::Xor, 1, V(t), V(t), V(t)));
        } else if x == u64::MAX {
            self.ins(enc::vspltisb(V(t), -1));
        } else {
            self.li(R12, x as i64);
            self.splat_r12(t);
        }
    }

    /// `op`; a `Bail` adds the position of its branch to `exits`.
    #[allow(clippy::too_many_lines)]
    fn vop(&mut self, v: &VLoop, op: &VOp, sp: u32, io: i64, exits: &mut Vec<usize>) {
        let r = |x: VReg| V(u32::from(x));
        let wi = v.w.trailing_zeros() as usize;
        let dp = v.w == 8;
        match *op {
            VOp::Load { dst, base } => {
                self.ins(enc::lxvd2x(r(dst), R(5 + u32::from(base)), R(OFF)));
            }
            VOp::Store { src, base } => {
                self.ins(enc::stxvd2x(r(src), R(5 + u32::from(base)), R(OFF)));
            }
            VOp::LoadExt { .. } | VOp::StoreTrunc { .. } => {
                unreachable!("powerpc64 vector loops keep one element width")
            }
            VOp::MaskStore { .. } => unreachable!("VMX stores no lanes under a mask"),
            VOp::Splat { dst, off } => {
                let k = [K::I8, K::I16, K::I32, K::I64][wi];
                self.ls(true, k, false, false, R12, sp, io + i64::from(off));
                if v.w < 8 {
                    // Each lane a copy: times 0x0101.., 0x0001_0001.., ..
                    let rep = [
                        0x0101_0101_0101_0101u64,
                        0x0001_0001_0001_0001,
                        0x0000_0001_0000_0001,
                    ][wi];
                    self.li(R11, rep as i64);
                    self.alu(Alu::Mulld, R12, R12, R11);
                }
                self.splat_r12(u32::from(dst));
            }
            VOp::Const { dst, bits } => {
                let ones = if dp {
                    u64::MAX
                } else {
                    (1u64 << (8 * v.w)) - 1
                };
                let mut x = bits & ones;
                let mut n = 8 * v.w;
                while n < 64 {
                    x |= x << n;
                    n *= 2;
                }
                self.vconst(u32::from(dst), x);
            }
            VOp::Bin { op, dst, a, b } => {
                let (d, a, b) = (r(dst), r(a), r(b));
                let w = v.w;
                let i = match op {
                    VBin::Add => enc::vx(Vop::Add, w, d, a, b),
                    VBin::Sub => enc::vx(Vop::Sub, w, d, a, b),
                    VBin::Mul if w == 2 => {
                        // vmladduhm d, a, b, 0
                        self.ins(enc::vx(Vop::Xor, 1, V(T2), V(T2), V(T2)));
                        enc::vmladduhm(d, a, b, V(T2))
                    }
                    VBin::Mul => enc::vx(Vop::Mul, w, d, a, b),
                    VBin::And => enc::vx(Vop::And, w, d, a, b),
                    VBin::Or => enc::vx(Vop::Or, w, d, a, b),
                    VBin::Xor => enc::vx(Vop::Xor, w, d, a, b),
                    VBin::AndNot => enc::vx(Vop::Andc, w, d, a, b),
                    VBin::Eq => enc::vx(Vop::Eq, w, d, a, b),
                    VBin::GtS => enc::vx(Vop::GtS, w, d, a, b),
                    VBin::GtU => enc::vx(Vop::GtU, w, d, a, b),
                    VBin::MinS => enc::vx(Vop::MinS, w, d, a, b),
                    VBin::MinU => enc::vx(Vop::MinU, w, d, a, b),
                    VBin::MaxS => enc::vx(Vop::MaxS, w, d, a, b),
                    VBin::MaxU => enc::vx(Vop::MaxU, w, d, a, b),
                    VBin::FAdd => enc::xv(Xv::Add, dp, d, a, b),
                    VBin::FSub => enc::xv(Xv::Sub, dp, d, a, b),
                    VBin::FMul => enc::xv(Xv::Mul, dp, d, a, b),
                    VBin::FDiv => enc::xv(Xv::Div, dp, d, a, b),
                    VBin::FEq | VBin::FNe => enc::xv(Xv::CmpEq, dp, d, a, b),
                    // a < b: b > a; a <= b: b >= a.
                    VBin::FLt => enc::xv(Xv::CmpGt, dp, d, b, a),
                    VBin::FLe => enc::xv(Xv::CmpGe, dp, d, b, a),
                    VBin::FMin => enc::xv(Xv::Min, dp, d, a, b),
                    VBin::FMax => enc::xv(Xv::Max, dp, d, a, b),
                    VBin::MulHiS | VBin::MulHiU => unreachable!("no VMX high product"),
                };
                self.ins(i);
                if op == VBin::FNe {
                    self.ins(enc::vx(Vop::Nor, 1, d, d, d));
                }
            }
            VOp::Un { op, dst, a } => {
                let (d, a) = (r(dst), r(a));
                match op {
                    VUn::FSqrt => self.ins(enc::xvsqrt(dp, d, a)),
                    VUn::Abs => unreachable!("no lane magnitudes here"),
                    VUn::SignMask => {
                        // Shifted right by all ones: by the lane's bits - 1.
                        self.ins(enc::vspltisb(V(T1), -1));
                        self.ins(enc::vx(Vop::Sra, v.w, d, a, V(T1)));
                    }
                }
            }
            VOp::Shift { op, dst, a, n } => {
                // The count in every lane (only its low bits count).
                let n = u32::from(n);
                if v.w == 8 {
                    self.vconst(T1, u64::from(n));
                } else {
                    let s = if n < 16 { n as i32 } else { n as i32 - 32 };
                    self.ins(enc::vspltisb(V(T1), s));
                }
                let o = match op {
                    VShift::Shl => Vop::Sl,
                    VShift::ShrU => Vop::Sr,
                    VShift::ShrS => Vop::Sra,
                };
                self.ins(enc::vx(o, v.w, r(dst), r(a), V(T1)));
            }
            VOp::Dot { .. } => unreachable!("no dot products here"),
            VOp::Fma { dst, a, b, c } => {
                // xvmadda adds into its target: a copy of `c`.
                let (d, a, b, c) = (r(dst), r(a), r(b), r(c));
                let t = if d == c || (d != a && d != b) {
                    d
                } else {
                    V(T1)
                };
                if t != c {
                    self.ins(enc::vx(Vop::Or, 1, t, c, c));
                }
                self.ins(enc::xv(Xv::Madd, dp, t, a, b));
                if t != d {
                    self.ins(enc::vx(Vop::Or, 1, d, t, t));
                }
            }
            VOp::Bail { a } => {
                // 0 > a in some lane: cr6's `eq` clear.
                self.ins(enc::vx(Vop::Xor, 1, V(T2), V(T2), V(T2)));
                self.ins(enc::vx_rc(Vop::GtS, v.w, V(T1), V(T2), r(a)));
                exits.push(self.b_fwd(Cond::f(CR6_EQ)));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::lir::{Func, Inst, VBin, VLoop, VOp, VUn};
    use crate::obj::{Obj, Sect};

    /// The code of a function holding one `VLoop` of lane width `w`.
    fn code(w: u32, body: Vec<VOp>) -> Vec<u32> {
        let mut obj = Obj::new(false, true, false);
        obj.ppc64 = true;
        let sym = obj.sym("f");
        let mut f = Func {
            sym,
            ..Func::default()
        };
        let io = f.slot(256, 16);
        f.insts.push(Inst::VLoop(Box::new(VLoop {
            io,
            w,
            vb: 16,
            min_left: 16 / w,
            bases: vec![16; 6],
            elem: vec![w; 6],
            pre: Vec::new(),
            body,
            post: Vec::new(),
            regs: 18,
            save: 64,
        })));
        f.insts.push(Inst::Ret { vals: Vec::new() });
        super::super::emit(&mut obj, &f);
        obj.sect(Sect::Text)
            .data
            .chunks(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect()
    }

    /// Known encodings (as an assembler writes them).
    #[test]
    fn encodings() {
        let bin = |op, dst, a, b| VOp::Bin { op, dst, a, b };
        let cases: Vec<(u32, VOp, u32)> = vec![
            // lxvd2x vs32, r5, r3
            (1, VOp::Load { dst: 0, base: 0 }, 0x7c05_1e99),
            (1, bin(VBin::Add, 2, 3, 4), 0x1043_2000),
            (8, bin(VBin::GtS, 2, 3, 4), 0x1043_23c7),
            (4, bin(VBin::FAdd, 2, 3, 4), 0xf043_2207),
            (8, bin(VBin::FLt, 2, 3, 4), 0xf044_1b5f),
            (
                8,
                VOp::Un {
                    op: VUn::FSqrt,
                    dst: 2,
                    a: 3,
                },
                0xf040_1b2f,
            ),
            // vcmpgtsb. v18, v19, v2
            (1, VOp::Bail { a: 2 }, 0x1253_1706),
        ];
        for (w, op, want) in cases {
            let got = code(w, vec![op]);
            assert!(got.contains(&want), "{op:?}: {want:08x} in {got:08x?}");
        }
    }
}
