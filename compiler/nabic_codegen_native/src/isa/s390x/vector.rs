//! `VLoop` on s390x (the vector facility of z13). `r1` the byte offset
//! of the vector at hand, `r0` the last offset a vector may start at,
//! `r6` the io block, the bases in `r2 .. r5 r9 r10`, `r14` scratch.
//! Vector registers `v0 .. v7`, `v16 .. v29` by number, `v30 v31`
//! scratch: `v8 .. v15` hold the callee-saved `f8 .. f15`. Element 0 of
//! a vector is the one at the lowest address, as in memory, and every
//! lane goes back where it came from: the byte order never shows.

use crate::isa::s390x::enc::{Ri, Ril, Rre, Rsy, Rxy, Vop};
use crate::isa::s390x::{ALWAYS, CC0, CC1, CC2, Em, R0, R1, R14, R15};
use crate::lir::{VBin, VIO_BASES, VIO_BOUND, VIO_IV, VLoop, VOp, VReg, VShift, VUn};

const OFF: u8 = R1;
const LIM: u8 = R0;
const IOB: u8 = 6;
const BASES: [u8; 6] = [2, 3, 4, 5, 9, 10];
const T1: u8 = 30;
const T2: u8 = 31;

/// The register of vector register number `r`.
const fn vr(r: VReg) -> u8 {
    if r < 8 { r } else { r + 8 }
}

/// The bytes of the narrowest element of `v`: the offset's unit.
fn emin(v: &VLoop) -> u32 {
    v.elem.iter().copied().chain([v.w]).min().unwrap_or(v.w)
}

/// log2 of `bytes`: an element size field.
const fn sz(bytes: u32) -> u8 {
    bytes.trailing_zeros() as u8
}

impl Em<'_> {
    pub(crate) fn vloop(&mut self, v: &VLoop) {
        self.flags = None;
        let io = i64::from(self.slot_off[v.io as usize]);
        self.rxy_at(Rxy::Lay, IOB, R15, io);
        let s = emin(v).trailing_zeros();
        self.a.rxy(Rxy::Lg, OFF, 0, IOB, VIO_IV as i32);
        self.a.rxy(Rxy::Lg, LIM, 0, IOB, VIO_BOUND as i32);
        // No vector fits below `min_left`.
        self.a.ril(Ril::Clgfi, LIM, v.min_left);
        let skip = self.a.brc_fwd(CC1);
        self.a.ri(Ri::Aghi, LIM, (-i64::from(v.min_left)) as u16);
        for r in [OFF, LIM] {
            self.a.rsy(Rsy::Sllg, r, r, 0, s as i32);
        }
        for (n, d) in v.bases.iter().enumerate() {
            let r = BASES[n];
            self.a
                .rxy(Rxy::Lg, r, 0, IOB, (VIO_BASES + 8 * n as u32) as i32);
            self.add_imm(r, i64::from(*d));
        }
        for op in &v.pre {
            self.vop(v, op, &mut Vec::new());
        }
        let top = self.a.pos();
        self.a.rre(Rre::Clgr, OFF, LIM);
        let mut exits = vec![self.a.brc_fwd(CC2)];
        for op in &v.body {
            self.vop(v, op, &mut exits);
        }
        // The bytes of the narrowest elements a vector covers.
        let step = v.vb / v.w * emin(v);
        self.a.ri(Ri::Aghi, OFF, step as u16);
        self.a.brc_back(ALWAYS, top);
        for e in exits {
            self.a.here(e);
        }
        self.a.rsy(Rsy::Srlg, OFF, OFF, 0, s as i32);
        self.a.rxy(Rxy::Stg, OFF, 0, IOB, VIO_IV as i32);
        for &(r, off) in &v.post {
            self.a.vrx(Vop::Vst, vr(r), 0, IOB, off, 0);
        }
        self.a.here(skip);
    }

    /// The index and base registers of base `b`'s elements at the offset.
    fn velem(&mut self, v: &VLoop, b: u8) -> (u8, u8) {
        let k = (v.elem[b as usize] / emin(v)).trailing_zeros();
        if k == 0 {
            return (OFF, BASES[b as usize]);
        }
        self.a.rsy(Rsy::Sllg, R14, OFF, 0, k as i32);
        (R14, BASES[b as usize])
    }

    /// `op`; a `Bail` adds the position of its branch to `exits`.
    #[allow(clippy::too_many_lines)]
    fn vop(&mut self, v: &VLoop, op: &VOp, exits: &mut Vec<usize>) {
        let m = sz(v.w);
        // A float's format: 3 long, 2 short.
        let fm = if v.w == 8 { 3 } else { 2 };
        match *op {
            VOp::Load { dst, base } => {
                let (x, b) = self.velem(v, base);
                self.a.vrx(Vop::Vl, vr(dst), x, b, 0, 0);
            }
            VOp::Store { src, base } => {
                let (x, b) = self.velem(v, base);
                self.a.vrx(Vop::Vst, vr(src), x, b, 0, 0);
            }
            VOp::LoadExt { dst, base, signed } => {
                let mut e = v.elem[base as usize];
                let d = vr(dst);
                let (x, b) = self.velem(v, base);
                // Into the leftmost elements; each unpack doubles their
                // width.
                let ld = if 16 * e / v.w == 8 {
                    Vop::Vleg
                } else {
                    Vop::Vlef
                };
                self.a.vrx(ld, d, x, b, 0, 0);
                while e < v.w {
                    let op = if signed { Vop::Vuph } else { Vop::Vuplh };
                    self.a.vrr(op, d, d, 0, sz(e), 0);
                    e *= 2;
                }
            }
            VOp::StoreTrunc { src, base } => {
                let e = v.elem[base as usize];
                let (mut w, mut a) = (v.w, vr(src));
                // Each pack halves the lanes' width, into the leftmost
                // elements.
                while w > e {
                    self.a.vrr(Vop::Vpk, T1, a, a, sz(w), 0);
                    a = T1;
                    w /= 2;
                }
                let (x, b) = self.velem(v, base);
                let st = if 16 * e / v.w == 8 {
                    Vop::Vsteg
                } else {
                    Vop::Vstef
                };
                self.a.vrx(st, T1, x, b, 0, 0);
            }
            VOp::MaskStore { .. } => {
                unreachable!("the vector facility stores no lanes under a mask")
            }
            VOp::Splat { dst, off } => {
                self.a.vrx(Vop::Vlrep, vr(dst), 0, IOB, off, m);
            }
            VOp::Const { dst, bits } => self.vconst(v.w, vr(dst), bits),
            VOp::Bin { op, dst, a, b } => {
                let (d, a, b) = (vr(dst), vr(a), vr(b));
                let (o, x, y, size) = match op {
                    VBin::Add => (Vop::Va, a, b, m),
                    VBin::Sub => (Vop::Vs, a, b, m),
                    VBin::Mul => (Vop::Vml, a, b, m),
                    VBin::MulHiS => (Vop::Vmh, a, b, m),
                    VBin::MulHiU => (Vop::Vmlh, a, b, m),
                    VBin::And => (Vop::Vn, a, b, 0),
                    VBin::AndNot => (Vop::Vnc, a, b, 0),
                    VBin::Or => (Vop::Vo, a, b, 0),
                    VBin::Xor => (Vop::Vx, a, b, 0),
                    VBin::Eq => (Vop::Vceq, a, b, m),
                    VBin::GtS => (Vop::Vch, a, b, m),
                    VBin::GtU => (Vop::Vchl, a, b, m),
                    VBin::MinS => (Vop::Vmn, a, b, m),
                    VBin::MinU => (Vop::Vmnl, a, b, m),
                    VBin::MaxS => (Vop::Vmx, a, b, m),
                    VBin::MaxU => (Vop::Vmxl, a, b, m),
                    VBin::FAdd => (Vop::Vfa, a, b, fm),
                    VBin::FSub => (Vop::Vfs, a, b, fm),
                    VBin::FMul => (Vop::Vfm, a, b, fm),
                    VBin::FDiv => (Vop::Vfd, a, b, fm),
                    VBin::FEq | VBin::FNe => (Vop::Vfce, a, b, fm),
                    // a < b: b > a; a <= b: b >= a.
                    VBin::FLt => (Vop::Vfch, b, a, fm),
                    VBin::FLe => (Vop::Vfche, b, a, fm),
                    VBin::FMin | VBin::FMax => {
                        // No NaN lanes: the one a compare picks.
                        let (x, y) = if op == VBin::FMin { (b, a) } else { (a, b) };
                        self.a.vrr(Vop::Vfch, T1, x, y, fm, 0);
                        self.a.vsel(d, a, b, T1);
                        return;
                    }
                };
                self.a.vrr(o, d, x, y, size, 0);
                if op == VBin::FNe {
                    self.a.vrr(Vop::Vno, d, d, d, 0, 0);
                }
            }
            VOp::Un { op, dst, a } => match op {
                VUn::FSqrt => self.a.vrr(Vop::Vfsq, vr(dst), vr(a), 0, fm, 0),
                VUn::Abs => unreachable!("no lane magnitudes here"),
                VUn::SignMask => {
                    let n = 8 * v.w as u16 - 1;
                    self.a.vrs(Vop::Vesra, vr(dst), vr(a), n, m);
                }
            },
            VOp::Shift { op, dst, a, n } => {
                let o = match op {
                    VShift::Shl => Vop::Vesl,
                    VShift::ShrS => Vop::Vesra,
                    VShift::ShrU => Vop::Vesrl,
                };
                self.a.vrs(o, vr(dst), vr(a), u16::from(n), m);
            }
            VOp::Dot { .. } => unreachable!("no dot products here"),
            VOp::Fma { dst, a, b, c } => self.a.vfma(vr(dst), vr(a), vr(b), vr(c), fm),
            VOp::Bail { a } => {
                // 0 > a in some lane: condition code 0 (all) or 1 (some).
                self.a.vri(Vop::Vgbm, T2, 0, 0);
                self.a.vrr(Vop::Vch, T1, T2, vr(a), m, 1);
                exits.push(self.a.brc_fwd(CC0 | CC1));
            }
        }
    }

    /// `d` = the low `w` bytes of `bits` in every lane.
    fn vconst(&mut self, w: u32, d: u8, bits: u64) {
        let bw = 8 * w;
        let ones = if w == 8 { u64::MAX } else { (1u64 << bw) - 1 };
        let x = bits & ones;
        // The lane's value sign-extended.
        let sx = ((x << (64 - bw)) as i64) >> (64 - bw);
        if x == 0 || x == ones {
            self.a.vri(Vop::Vgbm, d, if x == 0 { 0 } else { 0xffff }, 0);
        } else if (-0x8000..0x8000).contains(&sx) {
            self.a.vri(Vop::Vrepi, d, sx as u16, sz(w));
        } else {
            self.li(R14, sx);
            self.a.vrs(Vop::Vlvg, d, R14, 0, sz(w));
            self.a.vrep(d, d, 0, sz(w));
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::lir::{Func, Inst, VBin, VLoop, VOp, VReg, VShift, VUn};
    use crate::obj::{Obj, Sect};

    /// The code of a function holding one `VLoop` of lane width `w`.
    fn code(w: u32, e: u32, pre: Vec<VOp>, body: Vec<VOp>) -> Vec<u8> {
        let mut obj = Obj::new(false, true, false);
        obj.s390x = true;
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
            elem: vec![w, w, w, w, w, e],
            pre,
            body,
            post: vec![(3, 64)],
            regs: 22,
            save: 128,
        })));
        f.insts.push(Inst::Ret { vals: Vec::new() });
        super::super::emit(&mut obj, &f);
        obj.sect(Sect::Text).data.clone()
    }

    const fn bin(op: VBin, dst: VReg, a: VReg, b: VReg) -> VOp {
        VOp::Bin { op, dst, a, b }
    }

    /// Known encodings (as LLVM's assembler writes them).
    #[test]
    fn encodings() {
        let cases: Vec<(u32, VOp, &[u8])> = vec![
            // vl %v1, 0(%r1,%r2)
            (
                4,
                VOp::Load { dst: 1, base: 0 },
                &[0xe7, 0x11, 0x20, 0x00, 0x00, 0x06],
            ),
            // vst %v17, 0(%r1,%r3)
            (
                4,
                VOp::Store { src: 9, base: 1 },
                &[0xe7, 0x11, 0x30, 0x00, 0x08, 0x0e],
            ),
            // vaf %v17, %v18, %v19
            (
                4,
                bin(VBin::Add, 9, 10, 11),
                &[0xe7, 0x12, 0x30, 0x00, 0x2e, 0xf3],
            ),
            // vsh %v1, %v2, %v3
            (
                2,
                bin(VBin::Sub, 1, 2, 3),
                &[0xe7, 0x12, 0x30, 0x00, 0x10, 0xf7],
            ),
            // vmlhw / vmhh / vmlhh %v1, %v2, %v3
            (
                2,
                bin(VBin::Mul, 1, 2, 3),
                &[0xe7, 0x12, 0x30, 0x00, 0x10, 0xa2],
            ),
            (
                2,
                bin(VBin::MulHiS, 1, 2, 3),
                &[0xe7, 0x12, 0x30, 0x00, 0x10, 0xa3],
            ),
            (
                2,
                bin(VBin::MulHiU, 1, 2, 3),
                &[0xe7, 0x12, 0x30, 0x00, 0x10, 0xa1],
            ),
            (
                1,
                bin(VBin::And, 1, 2, 3),
                &[0xe7, 0x12, 0x30, 0x00, 0x00, 0x68],
            ),
            (
                1,
                bin(VBin::AndNot, 1, 2, 3),
                &[0xe7, 0x12, 0x30, 0x00, 0x00, 0x69],
            ),
            (
                1,
                bin(VBin::Or, 1, 2, 3),
                &[0xe7, 0x12, 0x30, 0x00, 0x00, 0x6a],
            ),
            (
                1,
                bin(VBin::Xor, 1, 2, 3),
                &[0xe7, 0x12, 0x30, 0x00, 0x00, 0x6d],
            ),
            (
                8,
                bin(VBin::Eq, 1, 2, 3),
                &[0xe7, 0x12, 0x30, 0x00, 0x30, 0xf8],
            ),
            (
                2,
                bin(VBin::GtS, 1, 2, 3),
                &[0xe7, 0x12, 0x30, 0x00, 0x10, 0xfb],
            ),
            (
                4,
                bin(VBin::GtU, 1, 2, 3),
                &[0xe7, 0x12, 0x30, 0x00, 0x20, 0xf9],
            ),
            (
                1,
                bin(VBin::MinS, 1, 2, 3),
                &[0xe7, 0x12, 0x30, 0x00, 0x00, 0xfe],
            ),
            (
                8,
                bin(VBin::MinU, 1, 2, 3),
                &[0xe7, 0x12, 0x30, 0x00, 0x30, 0xfc],
            ),
            (
                2,
                bin(VBin::MaxS, 1, 2, 3),
                &[0xe7, 0x12, 0x30, 0x00, 0x10, 0xff],
            ),
            (
                4,
                bin(VBin::MaxU, 1, 2, 3),
                &[0xe7, 0x12, 0x30, 0x00, 0x20, 0xfd],
            ),
            (
                8,
                bin(VBin::FAdd, 1, 2, 3),
                &[0xe7, 0x12, 0x30, 0x00, 0x30, 0xe3],
            ),
            (
                8,
                bin(VBin::FSub, 1, 2, 3),
                &[0xe7, 0x12, 0x30, 0x00, 0x30, 0xe2],
            ),
            (
                8,
                bin(VBin::FMul, 1, 2, 3),
                &[0xe7, 0x12, 0x30, 0x00, 0x30, 0xe7],
            ),
            (
                8,
                bin(VBin::FDiv, 1, 2, 3),
                &[0xe7, 0x12, 0x30, 0x00, 0x30, 0xe5],
            ),
            (
                8,
                bin(VBin::FEq, 1, 2, 3),
                &[0xe7, 0x12, 0x30, 0x00, 0x30, 0xe8],
            ),
            // vfchdb %v1, %v3, %v2
            (
                8,
                bin(VBin::FLt, 1, 2, 3),
                &[0xe7, 0x13, 0x20, 0x00, 0x30, 0xeb],
            ),
            (
                8,
                bin(VBin::FLe, 1, 2, 3),
                &[0xe7, 0x13, 0x20, 0x00, 0x30, 0xea],
            ),
            (
                4,
                bin(VBin::FAdd, 1, 2, 3),
                &[0xe7, 0x12, 0x30, 0x00, 0x20, 0xe3],
            ),
            // vfcedb then vno %v1, %v1, %v1
            (
                8,
                bin(VBin::FNe, 1, 2, 3),
                &[0xe7, 0x11, 0x10, 0x00, 0x00, 0x6b],
            ),
            // vfchdb %v30, %v3, %v2; vsel %v1, %v2, %v3, %v30
            (
                8,
                bin(VBin::FMin, 1, 2, 3),
                &[
                    0xe7, 0xe3, 0x20, 0x00, 0x38, 0xeb, 0xe7, 0x12, 0x30, 0x00, 0xe1, 0x8d,
                ],
            ),
            (
                8,
                VOp::Un {
                    op: VUn::FSqrt,
                    dst: 1,
                    a: 2,
                },
                &[0xe7, 0x12, 0x00, 0x00, 0x30, 0xce],
            ),
            // vesrag %v1, %v2, 63
            (
                8,
                VOp::Un {
                    op: VUn::SignMask,
                    dst: 1,
                    a: 2,
                },
                &[0xe7, 0x12, 0x00, 0x3f, 0x30, 0x3a],
            ),
            (
                2,
                VOp::Shift {
                    op: VShift::Shl,
                    dst: 1,
                    a: 2,
                    n: 3,
                },
                &[0xe7, 0x12, 0x00, 0x03, 0x10, 0x30],
            ),
            (
                1,
                VOp::Shift {
                    op: VShift::ShrU,
                    dst: 1,
                    a: 2,
                    n: 7,
                },
                &[0xe7, 0x12, 0x00, 0x07, 0x00, 0x38],
            ),
            // vchgs %v30, %v31, %v1
            (8, VOp::Bail { a: 1 }, &[0xe7, 0xef, 0x10, 0x10, 0x3c, 0xfb]),
            // vrepif %v21, 300
            (
                4,
                VOp::Const { dst: 13, bits: 300 },
                &[0xe7, 0x50, 0x01, 0x2c, 0x28, 0x45],
            ),
            // vrepih %v5, -1 is vone; vrepih %v5, -2
            (
                2,
                VOp::Const {
                    dst: 5,
                    bits: 0xfffe,
                },
                &[0xe7, 0x50, 0xff, 0xfe, 0x10, 0x45],
            ),
            // vone %v22
            (
                1,
                VOp::Const {
                    dst: 14,
                    bits: 0xff,
                },
                &[0xe7, 0x60, 0xff, 0xff, 0x08, 0x44],
            ),
            // vlrepf %v2, 16(%r6)
            (
                4,
                VOp::Splat { dst: 2, off: 16 },
                &[0xe7, 0x20, 0x60, 0x10, 0x20, 0x05],
            ),
            // vleg %v3, 0(%r1,%r10), 0; vuphb %v3, %v3
            (
                2,
                VOp::LoadExt {
                    dst: 3,
                    base: 5,
                    signed: true,
                },
                &[
                    0xe7, 0x31, 0xa0, 0x00, 0x00, 0x02, 0xe7, 0x33, 0x00, 0x00, 0x00, 0xd7,
                ],
            ),
            // vpkh %v30, %v3, %v3; vsteg %v30, 0(%r1,%r10), 0
            (
                2,
                VOp::StoreTrunc { src: 3, base: 5 },
                &[
                    0xe7, 0xe3, 0x30, 0x00, 0x18, 0x94, 0xe7, 0xe1, 0xa0, 0x00, 0x08, 0x0a,
                ],
            ),
        ];
        for (w, op, want) in cases {
            let e = match op {
                VOp::LoadExt { .. } | VOp::StoreTrunc { .. } => 1,
                _ => w,
            };
            let got = code(w, e, Vec::new(), vec![op]);
            assert!(
                got.windows(want.len()).any(|x| x == want),
                "{op:?}: {want:02x?} in {got:02x?}"
            );
        }
    }

    /// The loop around: the post-loop store of the results.
    #[test]
    fn frame() {
        let got = code(8, 8, vec![VOp::Const { dst: 3, bits: 0 }], Vec::new());
        // vzero %v3; vst %v3, 64(%r6)
        for want in [
            &[0xe7u8, 0x30, 0x00, 0x00, 0x00, 0x44][..],
            &[0xe7, 0x30, 0x60, 0x40, 0x00, 0x0e],
        ] {
            assert!(
                got.windows(6).any(|x| x == want),
                "{want:02x?} in {got:02x?}"
            );
        }
    }
}
