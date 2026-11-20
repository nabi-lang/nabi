//! `VLoop` on riscv64 with V (RVV 1.0): vectors of 128 bits, which every
//! V machine holds (`vl` the lanes of 16 bytes, at any `VLEN`; the rest
//! of a register is tail, never read). `t0` the byte offset of the
//! vector at hand, `t1` the last offset a vector may start at, `t4` the
//! io block, the bases in `a0 .. a7`, `t2 t3` scratch. Vector registers
//! `v1 .. v30` by number; `v0` holds compare and store masks, `v31` is
//! scratch. A compare's mask turns into lanes of all ones or zeros. All
//! vector registers are caller-saved, and a `VLoop` clobbers what a call
//! does.

use crate::isa::riscv::enc::{self, Cond, Ls, X};
use crate::isa::riscv::{Em, SP, T0, T1, T2, T3, T6, ZERO};
use crate::lir::{VBin, VIO_BASES, VIO_BOUND, VIO_IV, VLoop, VOp, VReg, VShift, VUn};

const OFF: u32 = T0;
const LIM: u32 = T1;
const IOB: u32 = 29;
const A: u32 = T2;
const T: u32 = T3;
const BASES: [u32; 8] = [10, 11, 12, 13, 14, 15, 16, 17];
const SCRATCH: u32 = 31;
const MASK: u32 = 0;

const OP_V: u32 = 0x57;
const LOAD_FP: u32 = 0x07;
const STORE_FP: u32 = 0x27;

// funct3 of `OP-V`.
const IVV: u32 = 0;
const FVV: u32 = 1;
const MVV: u32 = 2;
const IVI: u32 = 3;
const IVX: u32 = 4;

/// The register of vector register number `r`.
const fn vr(r: VReg) -> u32 {
    r as u32 + 1
}

/// The bytes of the narrowest element of `v`: the offset's unit.
fn emin(v: &VLoop) -> u32 {
    v.elem.iter().copied().chain([v.w]).min().unwrap_or(v.w)
}

/// An `OP-V` word: `vd, vs2, vs1` (or `rs1`, a 5-bit immediate), `vm`
/// clear for `v0.t`.
const fn opv(f6: u32, masked: bool, vs2: u32, vs1: u32, f3: u32, vd: u32) -> u32 {
    (f6 << 26)
        | ((!masked as u32) << 25)
        | (vs2 << 20)
        | ((vs1 & 31) << 15)
        | (f3 << 12)
        | (vd << 7)
        | OP_V
}

/// `vsetivli zero, avl, e<8 << sew>, <lmul>, ta, ma` (`lmul`: 0 for
/// `m1`, 7 `mf2`, 6 `mf4`).
const fn vsetivli(avl: u32, sew: u32, lmul: u32) -> u32 {
    (3 << 30) | ((0xc0 | (sew << 3) | lmul) << 20) | (avl << 15) | (7 << 12) | OP_V
}

/// `vle<eew>.v` / `vse<eew>.v` of `v` at `(rs1)`.
const fn vmem(store: bool, eew: u32, v: u32, rs1: u32, masked: bool) -> u32 {
    let width = match eew {
        8 => 0,
        16 => 5,
        32 => 6,
        _ => 7,
    };
    let op = if store { STORE_FP } else { LOAD_FP };
    ((!masked as u32) << 25) | (rs1 << 15) | (width << 12) | (v << 7) | op
}

impl Em<'_> {
    fn v(&mut self, w: u32) {
        self.ins(enc::vector(w));
    }

    pub(crate) fn vloop(&mut self, v: &VLoop) {
        self.flags = None;
        let io = i64::from(self.slot_off[v.io as usize]);
        self.add_imm(IOB, SP, io, T6);
        let s = emin(v).trailing_zeros();
        self.ld_word(OFF, IOB, i64::from(VIO_IV));
        self.ld_word(LIM, IOB, i64::from(VIO_BOUND));
        // No vector fits below `min_left`.
        self.li(T, i64::from(v.min_left));
        let skip = self.b_fwd(Cond::LTU, LIM, T);
        self.alu(crate::isa::riscv::SUB, LIM, LIM, T);
        self.slli(OFF, OFF, s);
        self.slli(LIM, LIM, s);
        for (n, d) in v.bases.iter().enumerate() {
            let r = BASES[n];
            self.ld_word(r, IOB, i64::from(VIO_BASES + 8 * n as u32));
            self.add_imm(r, r, i64::from(*d), T6);
        }
        self.vtype(v, v.w, 0);
        for op in &v.pre {
            self.vop(v, op, &mut Vec::new());
        }
        let top = self.a.pos();
        let mut exits = vec![self.b_fwd(Cond::LTU, LIM, OFF)];
        for op in &v.body {
            self.vop(v, op, &mut exits);
        }
        // The bytes of the narrowest elements a vector covers.
        let step = v.vb / v.w * emin(v);
        self.addi(OFF, OFF, step as i32);
        let back = (top as i64 - self.a.pos() as i64) * 4;
        self.ins(enc::jal(X(ZERO), back as i32));
        for e in exits {
            self.here(e);
        }
        self.srli(OFF, OFF, s);
        self.st_word(OFF, IOB, i64::from(VIO_IV));
        for &(r, off) in &v.post {
            self.addi(A, IOB, off as i32);
            self.v(vmem(true, 8 * v.w, vr(r), A, false));
        }
        self.here(skip);
    }

    /// `vl` the lanes of a vector, of `bytes` each (`lmul` as
    /// `vsetivli`'s).
    fn vtype(&mut self, v: &VLoop, bytes: u32, lmul: u32) {
        self.v(vsetivli(v.vb / v.w, bytes.trailing_zeros(), lmul));
    }

    /// `A` = the address of base `b`'s elements at the offset.
    fn vaddr(&mut self, v: &VLoop, b: u8) {
        let base = BASES[b as usize];
        let k = (v.elem[b as usize] / emin(v)).trailing_zeros();
        if k == 0 {
            self.alu(crate::isa::riscv::ADD, A, base, OFF);
        } else {
            self.slli(A, OFF, k);
            self.alu(crate::isa::riscv::ADD, A, A, base);
        }
    }

    /// `d` = all ones where the mask in `v0` is set, else zero.
    fn mask_lanes(&mut self, d: u32) {
        self.v(opv(0x17, false, 0, 0, IVI, d));
        self.v(opv(0x17, true, d, 0x1f, IVI, d));
    }

    /// `op`; a `Bail` adds the position of its branch to `exits`.
    #[allow(clippy::too_many_lines)]
    fn vop(&mut self, v: &VLoop, op: &VOp, exits: &mut Vec<usize>) {
        let eew = 8 * v.w;
        match *op {
            VOp::Load { dst, base } => {
                self.vaddr(v, base);
                self.v(vmem(false, eew, vr(dst), A, false));
            }
            VOp::Store { src, base } => {
                self.vaddr(v, base);
                self.v(vmem(true, eew, vr(src), A, false));
            }
            VOp::MaskStore { src, mask, base } => {
                // vmsne.vi v0, mask, 0
                self.v(opv(0x19, false, vr(mask), 0, IVI, MASK));
                self.vaddr(v, base);
                self.v(vmem(true, eew, vr(src), A, true));
            }
            VOp::LoadExt { dst, base, signed } => {
                let e = v.elem[base as usize];
                self.vaddr(v, base);
                self.v(vmem(false, 8 * e, SCRATCH, A, false));
                // vsext / vzext .vf2 / .vf4: no overlap with the source.
                let f = match (v.w / e, signed) {
                    (2, false) => 6,
                    (2, true) => 7,
                    (4, false) => 4,
                    (4, true) => 5,
                    (_, false) => 2,
                    (_, true) => 3,
                };
                self.v(opv(0x12, false, SCRATCH, f, MVV, vr(dst)));
            }
            VOp::StoreTrunc { src, base } => {
                let e = v.elem[base as usize];
                // vnsrl.wi of half the width at a time, into `v0` then
                // `v31`; the lanes keep their count.
                let (mut w, mut a, mut lmul) = (v.w, vr(src), 7);
                let mut to = MASK;
                while w > e {
                    w /= 2;
                    self.vtype(v, w, lmul);
                    self.v(opv(0x2c, false, a, 0, IVI, to));
                    a = to;
                    to = SCRATCH;
                    lmul -= 1;
                }
                self.vaddr(v, base);
                self.v(vmem(true, 8 * e, a, A, false));
                self.vtype(v, v.w, 0);
            }
            VOp::Splat { dst, off } => {
                let ld = match v.w {
                    1 => Ls::Lb,
                    2 => Ls::Lh,
                    4 => Ls::Lw,
                    _ => Ls::Ld,
                };
                self.ls(ld, T, IOB, i64::from(off));
                self.v(opv(0x17, false, 0, T, IVX, vr(dst)));
            }
            VOp::Const { dst, bits } => {
                let bw = 8 * v.w;
                let x = if v.w == 8 {
                    bits as i64
                } else {
                    ((bits << (64 - bw)) as i64) >> (64 - bw)
                };
                if (-16..16).contains(&x) {
                    self.v(opv(0x17, false, 0, x as u32, IVI, vr(dst)));
                } else {
                    self.li(T, x);
                    self.v(opv(0x17, false, 0, T, IVX, vr(dst)));
                }
            }
            VOp::Bin { op, dst, a, b } => {
                let (d, a, b) = (vr(dst), vr(a), vr(b));
                // (funct6, funct3, vs2, vs1); a compare's result is a
                // mask.
                let (f6, f3, x, y, mask) = match op {
                    VBin::Add => (0x00, IVV, a, b, false),
                    VBin::Sub => (0x02, IVV, a, b, false),
                    VBin::MinU => (0x04, IVV, a, b, false),
                    VBin::MinS => (0x05, IVV, a, b, false),
                    VBin::MaxU => (0x06, IVV, a, b, false),
                    VBin::MaxS => (0x07, IVV, a, b, false),
                    VBin::And => (0x09, IVV, a, b, false),
                    VBin::Or => (0x0a, IVV, a, b, false),
                    VBin::Xor => (0x0b, IVV, a, b, false),
                    VBin::AndNot => {
                        // vnot.v v31, b; vand.vv d, a, v31
                        self.v(opv(0x0b, false, b, 0x1f, IVI, SCRATCH));
                        (0x09, IVV, a, SCRATCH, false)
                    }
                    VBin::Eq => (0x18, IVV, a, b, true),
                    // a > b: b < a.
                    VBin::GtU => (0x1a, IVV, b, a, true),
                    VBin::GtS => (0x1b, IVV, b, a, true),
                    VBin::Mul => (0x25, MVV, a, b, false),
                    VBin::MulHiU => (0x24, MVV, a, b, false),
                    VBin::MulHiS => (0x27, MVV, a, b, false),
                    VBin::FAdd => (0x00, FVV, a, b, false),
                    VBin::FSub => (0x02, FVV, a, b, false),
                    VBin::FMin => (0x04, FVV, a, b, false),
                    VBin::FMax => (0x06, FVV, a, b, false),
                    VBin::FDiv => (0x20, FVV, a, b, false),
                    VBin::FMul => (0x24, FVV, a, b, false),
                    VBin::FEq => (0x18, FVV, a, b, true),
                    VBin::FLe => (0x19, FVV, a, b, true),
                    VBin::FLt => (0x1b, FVV, a, b, true),
                    VBin::FNe => (0x1c, FVV, a, b, true),
                };
                if mask {
                    self.v(opv(f6, false, x, y, f3, MASK));
                    self.mask_lanes(d);
                } else {
                    self.v(opv(f6, false, x, y, f3, d));
                }
            }
            VOp::Un { op, dst, a } => match op {
                // vfsqrt.v
                VUn::FSqrt => self.v(opv(0x13, false, vr(a), 0, FVV, vr(dst))),
                VUn::Abs => unreachable!("no lane magnitudes here"),
                VUn::SignMask => self.vshift(0x29, vr(dst), vr(a), 8 * v.w - 1),
            },
            VOp::Shift { op, dst, a, n } => {
                let f6 = match op {
                    VShift::Shl => 0x25,
                    VShift::ShrU => 0x28,
                    VShift::ShrS => 0x29,
                };
                self.vshift(f6, vr(dst), vr(a), u32::from(n));
            }
            VOp::Dot { .. } => unreachable!("no dot products here"),
            VOp::Fma { dst, a, b, c } => {
                // vfmacc.vv adds into its destination: a copy of `c`.
                let (d, a, b, c) = (vr(dst), vr(a), vr(b), vr(c));
                let t = if d == c || (d != a && d != b) {
                    d
                } else {
                    SCRATCH
                };
                if t != c {
                    self.v(opv(0x17, false, 0, c, IVV, t));
                }
                self.v(opv(0x2c, false, b, a, FVV, t));
                if t != d {
                    self.v(opv(0x17, false, 0, t, IVV, d));
                }
            }
            VOp::Bail { a } => {
                // vmslt.vx v0, a, zero; vcpop.m t3, v0
                self.v(opv(0x1b, false, vr(a), ZERO, IVX, MASK));
                self.v(opv(0x10, false, MASK, 0x10, MVV, T));
                exits.push(self.b_fwd(Cond::NE, T, ZERO));
            }
        }
    }

    /// The shift `f6` of `a` by `n`: an immediate below 32, else `t3`.
    fn vshift(&mut self, f6: u32, d: u32, a: u32, n: u32) {
        if n < 32 {
            self.v(opv(f6, false, a, n, IVI, d));
        } else {
            self.li(T, i64::from(n));
            self.v(opv(f6, false, a, T, IVX, d));
        }
    }
}

/// The text of vector word `w` as `llvm-objdump -d` prints it.
pub(crate) fn text(w: u32) -> String {
    let bits = |lo: u32, n: u32| (w >> lo) & ((1 << n) - 1);
    let (vd, f3, vs1, vs2, f6) = (
        bits(7, 5),
        bits(12, 3),
        bits(15, 5),
        bits(20, 5),
        bits(26, 6),
    );
    let masked = bits(25, 1) == 0;
    let x = |r: u32| crate::isa::riscv::enc::xreg(r);
    let t = if masked { ", v0.t" } else { "" };
    let imm = || {
        let i = ((vs1 << 27) as i32) >> 27;
        if i < 0 {
            format!("-{:#x}", -i)
        } else {
            format!("{i:#x}")
        }
    };
    if w & 0x7f != OP_V {
        let eew = match f3 {
            0 => 8,
            5 => 16,
            6 => 32,
            _ => 64,
        };
        let mn = if w & 0x7f == STORE_FP { "vse" } else { "vle" };
        return format!("{mn}{eew}.v\tv{vd}, ({}){t}", x(vs1));
    }
    if f3 == 7 {
        let vt = bits(20, 10);
        let lmul = ["m1", "m2", "m4", "m8", "", "mf8", "mf4", "mf2"][(vt & 7) as usize];
        let sew = 8 << ((vt >> 3) & 7);
        return format!("vsetivli\t{}, {:#x}, e{sew}, {lmul}, ta, ma", x(vd), vs1);
    }
    let tail = |sfx: &str| -> String {
        match f3 {
            IVI => format!(
                "v{vd}, v{vs2}, {}{t}",
                if sfx == "u" {
                    format!("{vs1:#x}")
                } else {
                    imm()
                }
            ),
            IVX => format!("v{vd}, v{vs2}, {}{t}", x(vs1)),
            _ => format!("v{vd}, v{vs2}, v{vs1}{t}"),
        }
    };
    let k = match f3 {
        IVV | MVV | FVV => "vv",
        IVI => "vi",
        _ => "vx",
    };
    let name = |n: &str, sfx: &str| format!("{n}.{k}\t{}", tail(sfx));
    match (f3, f6) {
        (IVV | IVI | IVX, 0x17) if masked => format!("vmerge.vim\tv{vd}, v{vs2}, {}, v0", imm()),
        (IVV, 0x17) => format!("vmv.v.v\tv{vd}, v{vs1}"),
        (IVI, 0x17) => format!("vmv.v.i\tv{vd}, {}", imm()),
        (IVX, 0x17) => format!("vmv.v.x\tv{vd}, {}", x(vs1)),
        (IVI, 0x0b) if vs1 == 0x1f => format!("vnot.v\tv{vd}, v{vs2}{t}"),
        (MVV, 0x10) => format!("vcpop.m\t{}, v{vs2}{t}", x(vd)),
        (MVV, 0x12) => {
            let mn = [
                "",
                "",
                "vzext.vf8",
                "vsext.vf8",
                "vzext.vf4",
                "vsext.vf4",
                "vzext.vf2",
                "vsext.vf2",
            ][(vs1 & 7) as usize];
            format!("{mn}\tv{vd}, v{vs2}{t}")
        }
        (FVV, 0x13) => format!("vfsqrt.v\tv{vd}, v{vs2}{t}"),
        (FVV, 0x2c) => format!("vfmacc.vv\tv{vd}, v{vs1}, v{vs2}{t}"),
        (IVI, 0x2c) => format!("vnsrl.wi\tv{vd}, v{vs2}, {vs1:#x}{t}"),
        (FVV, f) => {
            let n = match f {
                0x00 => "vfadd",
                0x02 => "vfsub",
                0x04 => "vfmin",
                0x06 => "vfmax",
                0x18 => "vmfeq",
                0x19 => "vmfle",
                0x1b => "vmflt",
                0x1c => "vmfne",
                0x20 => "vfdiv",
                _ => "vfmul",
            };
            name(n, "")
        }
        (MVV, f) => {
            let n = match f {
                0x24 => "vmulhu",
                0x25 => "vmul",
                _ => "vmulh",
            };
            name(n, "")
        }
        (_, f) => {
            let n = match f {
                0x00 => "vadd",
                0x02 => "vsub",
                0x04 => "vminu",
                0x05 => "vmin",
                0x06 => "vmaxu",
                0x07 => "vmax",
                0x09 => "vand",
                0x0a => "vor",
                0x0b => "vxor",
                0x18 => "vmseq",
                0x19 => "vmsne",
                0x1a => "vmsltu",
                0x1b => "vmslt",
                0x25 => "vsll",
                0x28 => "vsrl",
                _ => "vsra",
            };
            // Shift amounts print unsigned.
            name(
                n,
                if matches!(f, 0x25 | 0x28 | 0x29) {
                    "u"
                } else {
                    ""
                },
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lir::{Func, Inst};
    use crate::obj::{Obj, Sect};

    /// The words of a function holding one `VLoop` of lane width `w`.
    fn code(w: u32, e: u32, body: Vec<VOp>) -> Vec<u32> {
        let mut obj = Obj::new(false, true, false);
        obj.riscv = true;
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
            bases: vec![16; 8],
            elem: vec![w, w, w, w, w, w, w, e],
            pre: Vec::new(),
            body,
            post: vec![(3, 64)],
            regs: 30,
            save: 128,
        })));
        f.insts.push(Inst::Ret { vals: Vec::new() });
        super::super::emit(&mut obj, &f);
        obj.sect(Sect::Text)
            .data
            .chunks(4)
            .map(|c| u32::from_le_bytes(c.try_into().expect("a word")))
            .collect()
    }

    /// `(word, text)`: as `llvm-mc -mattr=+v` assembles the text and
    /// `llvm-objdump -d` prints the word.
    const WANT: &[(u32, &str)] = &[
        (0xcc08_7057, "vsetivli\tzero, 0x10, e8, m1, ta, ma"),
        (0xcd02_7057, "vsetivli\tzero, 0x4, e32, m1, ta, ma"),
        (0xcd81_7057, "vsetivli\tzero, 0x2, e64, m1, ta, ma"),
        (0xccf2_7057, "vsetivli\tzero, 0x4, e16, mf2, ta, ma"),
        (0xcc62_7057, "vsetivli\tzero, 0x4, e8, mf4, ta, ma"),
        (0x0203_8087, "vle8.v\tv1, (t2)"),
        (0x0203_d107, "vle16.v\tv2, (t2)"),
        (0x0203_ef87, "vle32.v\tv31, (t2)"),
        (0x0203_ff07, "vle64.v\tv30, (t2)"),
        (0x0203_80a7, "vse8.v\tv1, (t2)"),
        (0x0203_f0a7, "vse64.v\tv1, (t2)"),
        (0x0003_e0a7, "vse32.v\tv1, (t2), v0.t"),
        (0x0221_80d7, "vadd.vv\tv1, v2, v3"),
        (0x0a21_80d7, "vsub.vv\tv1, v2, v3"),
        (0x1221_80d7, "vminu.vv\tv1, v2, v3"),
        (0x1621_80d7, "vmin.vv\tv1, v2, v3"),
        (0x1a21_80d7, "vmaxu.vv\tv1, v2, v3"),
        (0x1e21_80d7, "vmax.vv\tv1, v2, v3"),
        (0x2621_80d7, "vand.vv\tv1, v2, v3"),
        (0x2a21_80d7, "vor.vv\tv1, v2, v3"),
        (0x2e21_80d7, "vxor.vv\tv1, v2, v3"),
        (0x2e3f_bfd7, "vnot.v\tv31, v3"),
        (0x6221_8057, "vmseq.vv\tv0, v2, v3"),
        (0x6630_3057, "vmsne.vi\tv0, v3, 0x0"),
        (0x6a31_0057, "vmsltu.vv\tv0, v3, v2"),
        (0x6e31_0057, "vmslt.vv\tv0, v3, v2"),
        (0x6e30_4057, "vmslt.vx\tv0, v3, zero"),
        (0x5e00_30d7, "vmv.v.i\tv1, 0x0"),
        (0x5e08_30d7, "vmv.v.i\tv1, -0x10"),
        (0x5e0e_40d7, "vmv.v.x\tv1, t3"),
        (0x5c1f_b0d7, "vmerge.vim\tv1, v1, -0x1, v0"),
        (0x9621_b0d7, "vsll.vi\tv1, v2, 0x3"),
        (0xa22f_b0d7, "vsrl.vi\tv1, v2, 0x1f"),
        (0xa623_b0d7, "vsra.vi\tv1, v2, 0x7"),
        (0xa62e_40d7, "vsra.vx\tv1, v2, t3"),
        (0x962e_40d7, "vsll.vx\tv1, v2, t3"),
        (0xa22e_40d7, "vsrl.vx\tv1, v2, t3"),
        (0xb210_3fd7, "vnsrl.wi\tv31, v1, 0x0"),
        (0x9621_a0d7, "vmul.vv\tv1, v2, v3"),
        (0x9e21_a0d7, "vmulh.vv\tv1, v2, v3"),
        (0x9221_a0d7, "vmulhu.vv\tv1, v2, v3"),
        (0x4208_2e57, "vcpop.m\tt3, v0"),
        (0x4bf3_a0d7, "vsext.vf2\tv1, v31"),
        (0x4bf3_20d7, "vzext.vf2\tv1, v31"),
        (0x4bf2_a0d7, "vsext.vf4\tv1, v31"),
        (0x4bf2_20d7, "vzext.vf4\tv1, v31"),
        (0x0221_90d7, "vfadd.vv\tv1, v2, v3"),
        (0x0a21_90d7, "vfsub.vv\tv1, v2, v3"),
        (0x9221_90d7, "vfmul.vv\tv1, v2, v3"),
        (0x8221_90d7, "vfdiv.vv\tv1, v2, v3"),
        (0x1221_90d7, "vfmin.vv\tv1, v2, v3"),
        (0x1a21_90d7, "vfmax.vv\tv1, v2, v3"),
        (0x6221_9057, "vmfeq.vv\tv0, v2, v3"),
        (0x7221_9057, "vmfne.vv\tv0, v2, v3"),
        (0x6e21_9057, "vmflt.vv\tv0, v2, v3"),
        (0x6621_9057, "vmfle.vv\tv0, v2, v3"),
        (0x4e20_10d7, "vfsqrt.v\tv1, v2"),
        (0x5e01_00d7, "vmv.v.v\tv1, v2"),
        (0xb221_90d7, "vfmacc.vv\tv1, v3, v2"),
    ];

    #[test]
    fn text_matches_the_reference() {
        for (w, want) in WANT {
            assert_eq!(text(*w), *want, "{w:08x}");
        }
    }

    /// The emitter writes the reference words.
    #[test]
    fn encodings() {
        let bin = |op, dst, a, b| VOp::Bin { op, dst, a, b };
        let cases: Vec<(u32, VOp, &[u32])> = vec![
            // vle32.v v1, (t2)
            (4, VOp::Load { dst: 0, base: 0 }, &[0x0203_8087 | (6 << 12)]),
            (1, bin(VBin::Add, 0, 1, 2), &[0x0221_80d7]),
            (1, bin(VBin::Sub, 0, 1, 2), &[0x0a21_80d7]),
            (2, bin(VBin::MulHiS, 0, 1, 2), &[0x9e21_a0d7]),
            (2, bin(VBin::MulHiU, 0, 1, 2), &[0x9221_a0d7]),
            (8, bin(VBin::Mul, 0, 1, 2), &[0x9621_a0d7]),
            (8, bin(VBin::FMin, 0, 1, 2), &[0x1221_90d7]),
            (4, bin(VBin::FMax, 0, 1, 2), &[0x1a21_90d7]),
            // vmslt.vv v0, v3, v2; vmv.v.i v1, 0; vmerge.vim v1, v1, -1, v0
            (
                4,
                bin(VBin::GtS, 0, 1, 2),
                &[0x6e31_0057, 0x5e00_30d7, 0x5c1f_b0d7],
            ),
            (4, bin(VBin::GtU, 0, 1, 2), &[0x6a31_0057]),
            (8, bin(VBin::FLt, 0, 1, 2), &[0x6e21_9057]),
            (8, bin(VBin::FNe, 0, 1, 2), &[0x7221_9057]),
            // vnot.v v31, v3; vand.vv v1, v2, v31
            (1, bin(VBin::AndNot, 0, 1, 2), &[0x2e3f_bfd7, 0x262f_80d7]),
            (
                8,
                VOp::Un {
                    op: VUn::FSqrt,
                    dst: 0,
                    a: 1,
                },
                &[0x4e20_10d7],
            ),
            (
                4,
                VOp::Shift {
                    op: VShift::ShrS,
                    dst: 0,
                    a: 1,
                    n: 7,
                },
                &[0xa623_b0d7],
            ),
            // vmslt.vx v0, v3, zero; vcpop.m t3, v0
            (2, VOp::Bail { a: 2 }, &[0x6e30_4057, 0x4208_2e57]),
            (1, VOp::Const { dst: 0, bits: 0xf0 }, &[0x5e08_30d7]),
            // vsext.vf2 v1, v31
            (
                2,
                VOp::LoadExt {
                    dst: 0,
                    base: 7,
                    signed: true,
                },
                &[0x4bf3_a0d7],
            ),
            // vsetivli e8 mf2; vnsrl.wi v0, v1, 0; vse8.v v0, (t2)
            (
                2,
                VOp::StoreTrunc { src: 0, base: 7 },
                &[0xcc74_7057, 0xb210_3057],
            ),
            // vmsne.vi v0, v3, 0; add t2, a0, t0; vse32.v v1, (t2), v0.t
            (
                4,
                VOp::MaskStore {
                    src: 0,
                    mask: 2,
                    base: 0,
                },
                &[0x6630_3057, 0x0055_03b3, 0x0003_e0a7],
            ),
        ];
        for (w, op, want) in cases {
            let e = match op {
                VOp::LoadExt { .. } | VOp::StoreTrunc { .. } => w / 2,
                _ => w,
            };
            let got = code(w, e, vec![op]);
            assert!(
                got.windows(want.len()).any(|x| x == want),
                "{op:?}: {want:08x?} in {got:08x?}"
            );
            for x in want {
                if (x & 0x7f) == OP_V || (x & 0x7f) == LOAD_FP || (x & 0x7f) == STORE_FP {
                    assert!(!text(*x).is_empty());
                }
            }
        }
    }
}
