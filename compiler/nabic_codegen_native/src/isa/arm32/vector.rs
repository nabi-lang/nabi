//! `VLoop` on 32-bit ARM (NEON, integer lanes only: NEON flushes
//! subnormal floats, the scalar unit does not). `r0` the offset of the
//! vector at hand, `r1` the last offset a vector may start at, `r2` and
//! `ip` scratch, the bases in `r3 .. r8` (the emitter's own scratch
//! registers otherwise). Vector registers `q0 .. q3`, `q8 .. q14` by
//! number, `q15` scratch: all caller-saved, and a `VLoop` clobbers what
//! a call does.

use crate::isa::arm32::enc::{self, Cond, D, Imm, N3, NShift, Op, R, Shift};
use crate::isa::arm32::{Acc, Em, IP, R0, R1, R2, SP};
use crate::lir::{VBin, VIO_BASES, VIO_BOUND, VIO_IV, VLoop, VOp, VReg, VShift, VUn};

const OFF: u32 = R0;
const LIM: u32 = R1;
const TMP: u32 = R2;
/// `q15`.
const SCRATCH: u32 = 15;

/// The `q` register of vector register number `r`.
const fn vq(r: VReg) -> u32 {
    let r = r as u32;
    if r < 4 { r } else { r + 4 }
}

/// The bytes of the narrowest element of `v`: the offset's unit.
fn emin(v: &VLoop) -> u32 {
    v.elem.iter().copied().chain([v.w]).min().unwrap_or(v.w)
}

impl Em<'_> {
    pub(crate) fn vloop(&mut self, v: &VLoop) {
        let io = self.slot_off[v.io as usize] as i32;
        // The offset counts elements of the narrowest kind.
        let s = emin(v).trailing_zeros();
        self.ls(Acc::Ldr, OFF, SP, io + VIO_IV as i32, IP);
        self.ls(Acc::Ldr, LIM, SP, io + VIO_BOUND as i32, IP);
        // subs r1, r1, #min_left; no vector fits below it.
        self.dpi(Cond::AL, Op::Sub, true, LIM, LIM, Imm::small(v.min_left));
        let skip = self.b_fwd(Cond::LO);
        if s != 0 {
            for r in [OFF, LIM] {
                self.dpsi(Cond::AL, Op::Mov, false, r, 0, r, Shift::Lsl, s);
            }
        }
        for (n, d) in v.bases.iter().enumerate() {
            let r = 3 + n as u32;
            self.ls(Acc::Ldr, r, SP, io + (VIO_BASES + 8 * n as u32) as i32, IP);
            self.add_imm(r, r, *d);
        }
        for op in &v.pre {
            self.vop(v, op, io, &mut Vec::new());
        }
        let top = self.pos();
        self.dp(Cond::AL, Op::Cmp, true, 0, OFF, LIM);
        let mut exits = vec![self.b_fwd(Cond::HI)];
        for op in &v.body {
            self.vop(v, op, io, &mut exits);
        }
        self.add_imm(OFF, OFF, (v.vb / v.w * emin(v)) as i32);
        self.b_back(Cond::AL, top);
        for e in exits {
            self.here(e);
        }
        // An immediate `lsr` of 0 is one of 32.
        if s != 0 {
            self.dpsi(Cond::AL, Op::Mov, false, OFF, 0, OFF, Shift::Lsr, s);
        }
        self.ls(Acc::Str, OFF, SP, io + VIO_IV as i32, IP);
        for &(r, off) in &v.post {
            self.add_imm(IP, SP, io + off as i32);
            self.ins(enc::vls_q(false, D(2 * vq(r)), R(IP)));
        }
        self.here(skip);
    }

    /// `ip` = the address of base `b`'s elements at the offset.
    fn vaddr(&mut self, v: &VLoop, b: u8) {
        let k = (v.elem[b as usize] / emin(v)).trailing_zeros();
        self.dpsi(
            Cond::AL,
            Op::Add,
            false,
            IP,
            3 + u32::from(b),
            OFF,
            Shift::Lsl,
            k,
        );
    }

    /// `op`; a `Bail` adds the position of its branch to `exits`.
    #[allow(clippy::too_many_lines)]
    fn vop(&mut self, v: &VLoop, op: &VOp, io: i32, exits: &mut Vec<usize>) {
        // `d` registers of the `q` ones.
        let q = |r: VReg| D(2 * vq(r));
        let ip = R(IP);
        let size = v.w.trailing_zeros();
        match *op {
            VOp::Load { dst, base } => {
                self.vaddr(v, base);
                self.ins(enc::vls_q(true, q(dst), ip));
            }
            VOp::Store { src, base } => {
                self.vaddr(v, base);
                self.ins(enc::vls_q(false, q(src), ip));
            }
            VOp::LoadExt { dst, base, signed } => {
                let mut e = v.elem[base as usize];
                let d = q(dst);
                self.vaddr(v, base);
                if 16 * e / v.w == 8 {
                    self.ins(enc::vls_d(true, d, ip));
                } else {
                    self.ins(enc::vls_lane32(true, d, ip));
                }
                // vmovl: each step doubles the lanes' width.
                while e < v.w {
                    self.ins(enc::vmovl(signed, 8 * e, d, d));
                    e *= 2;
                }
            }
            VOp::StoreTrunc { src, base } => {
                let e = v.elem[base as usize];
                let (mut w, mut a) = (v.w, q(src));
                let t = D(2 * SCRATCH);
                // vmovn: each step halves the lanes' width.
                while w > e {
                    let sz = (w / 2).trailing_zeros();
                    self.ins(enc::vmovn(sz, t, a));
                    a = t;
                    w /= 2;
                }
                self.vaddr(v, base);
                if 16 * e / v.w == 8 {
                    self.ins(enc::vls_d(false, t, ip));
                } else {
                    self.ins(enc::vls_lane32(false, t, ip));
                }
            }
            VOp::MaskStore { .. } => unreachable!("NEON stores no lanes under a mask"),
            VOp::Splat { dst, off } => {
                self.add_imm(IP, SP, io + off as i32);
                let d = q(dst);
                if v.w == 8 {
                    self.ins(enc::vld1_64(d, ip));
                    self.ins(enc::vorr_d(D(d.0 + 1), d, d));
                } else {
                    self.ins(enc::vld1_dup(size, d, ip));
                }
            }
            VOp::Const { dst, bits } => {
                let d = q(dst);
                let ones = if v.w == 8 {
                    u64::MAX
                } else {
                    (1u64 << (8 * v.w)) - 1
                };
                if bits == 0 {
                    self.ins(enc::vmov_zero(d));
                } else if bits == ones {
                    self.ins(enc::vmov_ones(d));
                } else {
                    // The lane repeated across 64 bits, in both halves.
                    let mut x = bits & ones;
                    let mut n = 8 * v.w;
                    while n < 64 {
                        x |= x << n;
                        n *= 2;
                    }
                    self.mov_imm(TMP, x as u32);
                    self.mov_imm(IP, (x >> 32) as u32);
                    self.ins(enc::vmov_d(false, d, R(TMP), ip));
                    self.ins(enc::vorr_d(D(d.0 + 1), d, d));
                }
            }
            VOp::Bin { op, dst, a, b } => {
                let (d, n, m) = (q(dst), q(a), q(b));
                let op = match op {
                    VBin::Add => N3::Add,
                    VBin::Sub => N3::Sub,
                    VBin::Mul => N3::Mul,
                    VBin::And => N3::And,
                    VBin::Or => N3::Orr,
                    VBin::Xor => N3::Eor,
                    // vbic d, a, b: a & !b.
                    VBin::AndNot => N3::Bic,
                    VBin::Eq => N3::Ceq,
                    VBin::GtS => N3::CgtS,
                    VBin::GtU => N3::CgtU,
                    VBin::MinS => N3::MinS,
                    VBin::MinU => N3::MinU,
                    VBin::MaxS => N3::MaxS,
                    VBin::MaxU => N3::MaxU,
                    _ => unreachable!("NEON here computes on integers only"),
                };
                self.ins(enc::neon3(op, size, d, n, m));
            }
            VOp::Un { op, dst, a } => match op {
                VUn::Abs => unreachable!("no lane magnitudes here"),
                VUn::SignMask => self.vshift(v.w, VShift::ShrS, q(dst), q(a), 8 * v.w - 1),
                VUn::FSqrt => unreachable!("NEON here computes on integers only"),
            },
            VOp::Shift { op, dst, a, n } => self.vshift(v.w, op, q(dst), q(a), u32::from(n)),
            VOp::Dot { .. } => unreachable!("no dot products here"),
            VOp::Fma { .. } => unreachable!("no fused multiply-add here"),
            VOp::Bail { a } => {
                // Lanes all ones where negative, folded to 64 bits, then
                // to the core registers.
                let t = D(2 * SCRATCH);
                self.vshift(v.w, VShift::ShrS, t, q(a), 8 * v.w - 1);
                self.ins(enc::vorr_d(t, t, D(t.0 + 1)));
                self.ins(enc::vmov_d(true, t, R(TMP), ip));
                self.dp(Cond::AL, Op::Orr, true, TMP, TMP, IP);
                exits.push(self.b_fwd(Cond::NE));
            }
        }
    }

    /// `vshl` / `vshr` by `n` of lanes of `w` bytes, `q` registers
    /// `d`, `a` (as their first `d` registers).
    fn vshift(&mut self, w: u32, op: VShift, d: D, a: D, n: u32) {
        if op != VShift::Shl && n == 0 {
            // A shift right by nothing has no encoding: a copy.
            self.ins(enc::neon3(N3::Orr, 0, d, a, a));
            return;
        }
        let op = match op {
            VShift::Shl => NShift::Shl,
            VShift::ShrS => NShift::ShrS,
            VShift::ShrU => NShift::ShrU,
        };
        self.ins(enc::vshift(op, w, d, a, n));
    }
}

#[cfg(test)]
mod tests {
    use crate::lir::{Func, Inst, VBin, VLoop, VOp, VShift, VUn};
    use crate::obj::{Obj, Sect};

    /// The code of a function holding one `VLoop` of lane width `w`.
    fn code(w: u32, body: Vec<VOp>) -> Vec<u32> {
        let mut obj = Obj::new(false, true, false);
        obj.arm = true;
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
            regs: 11,
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

    /// Byte lanes keep the counter as it is: no `lsr #0`, which is one
    /// of 32.
    #[test]
    fn byte_lanes_shift_nothing() {
        let got = code(1, Vec::new());
        // mov r0, r0, lsr #<n>
        assert!(!got.iter().any(|w| w & 0x0fff_0070 == 0x01a0_0020));
    }

    /// Known encodings (as an assembler writes them).
    #[test]
    fn encodings() {
        let bin = |op, dst, a, b| VOp::Bin { op, dst, a, b };
        let cases: Vec<(u32, VOp, u32)> = vec![
            // vadd.i8 q14, q13, q12 (numbers 10, 9, 8)
            (1, bin(VBin::Add, 10, 9, 8), 0xf24a_c8e8),
            (4, bin(VBin::GtU, 0, 1, 2), 0xf322_0344),
            (2, bin(VBin::AndNot, 0, 1, 2), 0xf212_0154),
            (
                8,
                VOp::Shift {
                    op: VShift::ShrS,
                    dst: 0,
                    a: 0,
                    n: 1,
                },
                0xf2bf_00d0,
            ),
            (
                1,
                VOp::Un {
                    op: VUn::SignMask,
                    dst: 0,
                    a: 0,
                },
                0xf289_0050,
            ),
            // vld1.32 {d0[], d1[]}, [ip]
            (4, VOp::Splat { dst: 0, off: 16 }, 0xf4ac_0caf),
        ];
        for (w, op, want) in cases {
            let got = code(w, vec![op]);
            assert!(got.contains(&want), "{op:?}: {want:08x} in {got:08x?}");
        }
    }
}
