//! Instruction text, as llvm-objdump prints it.

#[allow(clippy::wildcard_imports)]
use crate::isa::arm32::enc::*;

pub(crate) const fn bits(w: u32, lo: u32, n: u32) -> u32 {
    (w >> lo) & ((1 << n) - 1)
}

/// A signed field.
pub(crate) fn sbits(w: u32, lo: u32, n: u32) -> i64 {
    let v = i64::from(bits(w, lo, n));
    (v << (64 - n)) >> (64 - n)
}

pub(crate) fn reg(r: u32) -> String {
    match r {
        13 => "sp".to_owned(),
        14 => "lr".to_owned(),
        15 => "pc".to_owned(),
        _ => format!("r{r}"),
    }
}

/// `#off` in hex (`#-0x4`).
pub(crate) fn hex(v: i64) -> String {
    if v < 0 {
        format!("#-{:#x}", v.unsigned_abs())
    } else {
        format!("#{v:#x}")
    }
}

/// `[rn{, #off}]` with `off` as `f` shows it (`U` clear: negative).
pub(crate) fn addr(rn: u32, m: u32, up: bool, f: fn(i64) -> String) -> String {
    if m == 0 && up {
        format!("[{}]", reg(rn))
    } else {
        let v = i64::from(m);
        format!("[{}, {}]", reg(rn), f(if up { v } else { -v }))
    }
}

pub(crate) fn dec(v: i64) -> String {
    format!("#{v}")
}

/// LLVM's rotation for modified immediate `v` (`getSOImmValRotate`).
pub(crate) const fn so_rot(v: u32) -> u32 {
    if v & !255 == 0 {
        return 0;
    }
    let rot = v.trailing_zeros() & !1;
    if v.rotate_right(rot) & !255 == 0 {
        return (32 - rot) & 31;
    }
    if v & 63 != 0 {
        let rot2 = (v & !63).trailing_zeros() & !1;
        if v.rotate_right(rot2) & !255 == 0 {
            return (32 - rot2) & 31;
        }
    }
    // No single rotation covers it: LLVM's fallback.
    (32 - rot) & 31
}

/// A modified immediate as objdump shows it: its value signed, or
/// `#bits, rot` when the encoding is not the one LLVM would pick.
pub(crate) fn mod_imm(e: u32) -> String {
    let (b, rot) = (e & 0xff, (e >> 7) & 0x1e);
    let v = b.rotate_right(rot);
    let r = so_rot(v);
    let canon = v.rotate_left(r) | ((r >> 1) << 8);
    if v.rotate_left(r) & !255 == 0 && canon == e {
        format!("#{}", v as i32)
    } else {
        format!("#{b}, {rot}")
    }
}

/// A register operand with its shift (`rm, lsl #2`, `rm, asr r12`).
pub(crate) fn op2(w: u32) -> String {
    let rm = reg(bits(w, 0, 4));
    let sh = ["lsl", "lsr", "asr", "ror"][bits(w, 5, 2) as usize];
    if bits(w, 4, 1) == 1 {
        format!("{rm}, {sh} {}", reg(bits(w, 8, 4)))
    } else {
        let n = bits(w, 7, 5);
        match (bits(w, 5, 2), n) {
            (0, 0) => rm,
            (3, 0) => format!("{rm}, rrx"),
            (1 | 2, 0) => format!("{rm}, {sh} #32"),
            _ => format!("{rm}, {sh} #{n}"),
        }
    }
}

/// `{r4, r5, lr}`.
pub(crate) fn reglist(m: u32) -> String {
    let r: Vec<String> = (0..16).filter(|r| (m >> r) & 1 == 1).map(reg).collect();
    format!("{{{}}}", r.join(", "))
}

/// `d`-register number of the fields at `lo` (4 bits) and `hi` (1 bit).
pub(crate) const fn dreg(w: u32, lo: u32, hi: u32) -> u32 {
    bits(w, lo, 4) | (bits(w, hi, 1) << 4)
}

/// `s`-register number of the fields at `lo` (4 bits) and `lsb`.
pub(crate) const fn sreg(w: u32, lo: u32, lsb: u32) -> u32 {
    (bits(w, lo, 4) << 1) | bits(w, lsb, 1)
}

/// A NEON register: `q` (`q` set) or `d`.
pub(crate) fn nreg(d: u32, q: bool) -> String {
    if q {
        format!("q{}", d >> 1)
    } else {
        format!("d{d}")
    }
}

/// The text of word `w` of form `f` at byte offset `at`.
#[allow(clippy::too_many_lines)]
pub(crate) fn text(w: u32, f: F, at: i64, l: &Listing<'_>) -> String {
    let c = COND[bits(w, 28, 4) as usize];
    let (rd, rn) = (bits(w, 12, 4), bits(w, 16, 4));
    match f {
        F::Dp | F::DpImm => {
            let op = bits(w, 21, 4);
            let s = if bits(w, 20, 1) == 1 { "s" } else { "" };
            let src = if f == F::DpImm {
                mod_imm(bits(w, 0, 12))
            } else {
                op2(w)
            };
            match op {
                10 | 11 => {
                    let mn = if op == 10 { "cmp" } else { "cmn" };
                    format!("{mn}{c}\t{}, {src}", reg(rn))
                }
                13 if f == F::Dp && bits(w, 4, 8) != 0 => {
                    // `mov` of a shifted register: the shift's alias.
                    let sh = ["lsl", "lsr", "asr", "ror"][bits(w, 5, 2) as usize];
                    let by = if bits(w, 4, 1) == 1 {
                        reg(bits(w, 8, 4))
                    } else {
                        match bits(w, 7, 5) {
                            0 => "#32".to_owned(),
                            n => format!("#{n}"),
                        }
                    };
                    format!("{sh}{s}{c}\t{}, {}, {by}", reg(rd), reg(bits(w, 0, 4)))
                }
                13 | 15 => {
                    let mn = if op == 13 { "mov" } else { "mvn" };
                    format!("{mn}{s}{c}\t{}, {src}", reg(rd))
                }
                _ => {
                    let mn = [
                        "and", "eor", "sub", "rsb", "add", "adc", "sbc", "rsc", "", "", "", "",
                        "orr", "", "bic", "",
                    ][op as usize];
                    format!("{mn}{s}{c}\t{}, {}, {src}", reg(rd), reg(rn))
                }
            }
        }
        F::MovW => {
            let mn = if bits(w, 22, 1) == 1 { "movt" } else { "movw" };
            let imm = (bits(w, 16, 4) << 12) | bits(w, 0, 12);
            format!("{mn}{c}\t{}, #{imm:#x}", reg(rd))
        }
        F::Ls => {
            let load = bits(w, 20, 1) == 1;
            let byte = if bits(w, 22, 1) == 1 { "b" } else { "" };
            let mn = if load { "ldr" } else { "str" };
            let (m, up) = (bits(w, 0, 12), bits(w, 23, 1) == 1);
            if bits(w, 24, 1) == 0 {
                // Post-indexed: the offset in decimal.
                let v = if up { i64::from(m) } else { -i64::from(m) };
                format!("{mn}{byte}{c}\t{}, [{}], #{v}", reg(rd), reg(rn))
            } else {
                format!("{mn}{byte}{c}\t{}, {}", reg(rd), addr(rn, m, up, hex))
            }
        }
        F::LsH => {
            let mn = match (bits(w, 20, 1), bits(w, 5, 2)) {
                (1, 1) => "ldrh",
                (0, 1) => "strh",
                (1, 2) => "ldrsb",
                _ => "ldrsh",
            };
            let m = (bits(w, 8, 4) << 4) | bits(w, 0, 4);
            let up = bits(w, 23, 1) == 1;
            format!("{mn}{c}\t{}, {}", reg(rd), addr(rn, m, up, dec))
        }
        F::B => {
            let mn = if bits(w, 24, 1) == 1 { "bl" } else { "b" };
            let t = at + 8 + 4 * sbits(w, 0, 24);
            format!("{mn}{c}\t{}", l.target(t))
        }
        F::Blx if bits(w, 4, 4) == 1 => format!("bx{c}\t{}", reg(bits(w, 0, 4))),
        F::Blx => format!("blx{c}\t{}", reg(bits(w, 0, 4))),
        F::Word => format!(".word\t{w:#010x}"),
        F::Stack => {
            let mn = if bits(w, 20, 1) == 1 { "pop" } else { "push" };
            format!("{mn}{c}\t{}", reglist(bits(w, 0, 16)))
        }
        F::Mul => {
            let (hi, lo, rs, rm) = (rn, rd, bits(w, 8, 4), bits(w, 0, 4));
            match bits(w, 21, 3) {
                0 => format!("mul{c}\t{}, {}, {}", reg(hi), reg(rm), reg(rs)),
                1 => format!("mla{c}\t{}, {}, {}, {}", reg(hi), reg(rm), reg(rs), reg(lo)),
                op => {
                    let mn = if op == 6 { "smull" } else { "umull" };
                    format!(
                        "{mn}{c}\t{}, {}, {}, {}",
                        reg(lo),
                        reg(hi),
                        reg(rm),
                        reg(rs)
                    )
                }
            }
        }
        F::Misc => {
            let mn = match w & 0x0ff0_00f0 {
                0x06a0_0070 => "sxtb",
                0x06e0_0070 => "uxtb",
                0x06b0_0070 => "sxth",
                0x06f0_0070 => "uxth",
                0x0160_0010 => "clz",
                0x06f0_0030 => "rbit",
                0x06b0_0030 => "rev",
                _ => "rev16",
            };
            format!("{mn}{c}\t{}, {}", reg(rd), reg(bits(w, 0, 4)))
        }
        F::Excl => {
            let sz = ["", "d", "b", "h"][bits(w, 21, 2) as usize];
            let base = reg(rn);
            if bits(w, 20, 1) == 1 {
                if sz == "d" {
                    format!("ldrexd{c}\t{}, {}, [{base}]", reg(rd), reg(rd + 1))
                } else {
                    format!("ldrex{sz}{c}\t{}, [{base}]", reg(rd))
                }
            } else {
                let rt = bits(w, 0, 4);
                if sz == "d" {
                    let (a, b) = (reg(rt), reg(rt + 1));
                    format!("strexd{c}\t{}, {a}, {b}, [{base}]", reg(rd))
                } else {
                    format!("strex{sz}{c}\t{}, {}, [{base}]", reg(rd), reg(rt))
                }
            }
        }
        F::Sys => match w {
            0xf57f_f01f => "clrex".to_owned(),
            0xf57f_f05b => "dmb\tish".to_owned(),
            0xeef1_fa10 => "vmrs\tAPSR_nzcv, fpscr".to_owned(),
            _ => "trap".to_owned(),
        },
        F::Mrc => format!("mrc{c}\tp15, #0x0, {}, c13, c0, #0x3", reg(rd)),
        F::Vfp => vfp_text(w),
        F::VmovS => {
            let s = format!("s{}", sreg(w, 16, 7));
            if bits(w, 20, 1) == 1 {
                format!("vmov{c}\t{}, {s}", reg(rd))
            } else {
                format!("vmov{c}\t{s}, {}", reg(rd))
            }
        }
        F::VmovLane => {
            let d = format!("d{}[0]", dreg(w, 16, 7));
            if bits(w, 20, 1) == 1 {
                format!("vmov{c}.32\t{}, {d}", reg(rd))
            } else {
                format!("vmov{c}.32\t{d}, {}", reg(rd))
            }
        }
        F::VmovD => {
            let d = format!("d{}", dreg(w, 0, 5));
            if bits(w, 20, 1) == 1 {
                format!("vmov{c}\t{}, {}, {d}", reg(rd), reg(rn))
            } else {
                format!("vmov{c}\t{d}, {}, {}", reg(rd), reg(rn))
            }
        }
        F::NeonMisc => {
            let (d, m) = (dreg(w, 12, 22), dreg(w, 0, 5));
            if bits(w, 8, 4) == 5 {
                format!("vcnt.8\td{d}, d{m}")
            } else {
                format!("vpaddl.u{}\td{d}, d{m}", 8 << bits(w, 18, 2))
            }
        }
        F::Vld => {
            let mn = if bits(w, 21, 1) == 1 { "vld1" } else { "vst1" };
            let d = dreg(w, 12, 22);
            let base = reg(rn);
            let (sz, list) = if bits(w, 23, 1) == 0 {
                let n = if bits(w, 8, 4) == 0xa { 2 } else { 1 };
                let r: Vec<String> = (d..d + n).map(|x| format!("d{x}")).collect();
                (8 << bits(w, 6, 2), r.join(", "))
            } else if bits(w, 8, 4) == 0xc {
                let n = 1 + bits(w, 5, 1);
                let r: Vec<String> = (d..d + n).map(|x| format!("d{x}[]")).collect();
                (8 << bits(w, 6, 2), r.join(", "))
            } else {
                (8 << bits(w, 10, 2), format!("d{d}[0]"))
            };
            format!("{mn}.{sz}\t{{{list}}}, [{base}]")
        }
        F::Vmovl => {
            let s = if bits(w, 24, 1) == 1 { "u" } else { "s" };
            let b = 8 << (bits(w, 19, 3).trailing_zeros());
            let (d, m) = (dreg(w, 12, 22), dreg(w, 0, 5));
            format!("vmovl.{s}{b}\t{}, d{m}", nreg(d, true))
        }
        F::Vmovn => {
            let (d, m) = (dreg(w, 12, 22), dreg(w, 0, 5));
            format!("vmovn.i{}\td{d}, {}", 16 << bits(w, 18, 2), nreg(m, true))
        }
        F::VmovImm => {
            let d = nreg(dreg(w, 12, 22), bits(w, 6, 1) == 1);
            let imm = (bits(w, 24, 1) << 7) | (bits(w, 16, 3) << 4) | bits(w, 0, 4);
            let ty = if bits(w, 8, 4) == 0xe { "i8" } else { "i32" };
            format!("vmov.{ty}\t{d}, #{imm:#x}")
        }
        F::Neon3 => {
            let q = bits(w, 6, 1) == 1;
            let (d, n, m) = (dreg(w, 12, 22), dreg(w, 16, 7), dreg(w, 0, 5));
            let u = bits(w, 24, 1) == 1;
            let size = 8 << bits(w, 20, 2);
            let su = if u { "u" } else { "s" };
            let mn = match (bits(w, 8, 4), bits(w, 4, 1)) {
                (8, 0) => format!("{}.i{size}", if u { "vsub" } else { "vadd" }),
                (8, _) => format!("vceq.i{size}"),
                (9, _) => format!("vmul.i{size}"),
                (3, _) => format!("vcgt.{su}{size}"),
                (6, 1) => format!("vmin.{su}{size}"),
                (6, _) => format!("vmax.{su}{size}"),
                _ => match (u, bits(w, 20, 2)) {
                    (false, 0) => "vand",
                    (false, 1) => "vbic",
                    (false, 2) => "vorr",
                    (false, _) => "vorn",
                    _ => "veor",
                }
                .to_owned(),
            };
            format!("{mn}\t{}, {}, {}", nreg(d, q), nreg(n, q), nreg(m, q))
        }
        F::NeonShift => {
            let q = bits(w, 6, 1) == 1;
            let (d, m) = (dreg(w, 12, 22), dreg(w, 0, 5));
            let imm6 = bits(w, 16, 6);
            let esize = if bits(w, 7, 1) == 1 {
                64
            } else {
                1 << (31 - imm6.leading_zeros())
            };
            let (mn, n) = if bits(w, 8, 4) == 5 {
                ("vshl.i", imm6 % esize)
            } else if bits(w, 24, 1) == 1 {
                (
                    "vshr.u",
                    2 * esize - imm6 - if esize == 64 { 64 } else { 0 },
                )
            } else {
                (
                    "vshr.s",
                    2 * esize - imm6 - if esize == 64 { 64 } else { 0 },
                )
            };
            format!("{mn}{esize}\t{}, {}, #{n:#x}", nreg(d, q), nreg(m, q))
        }
    }
}

/// A VFP data-processing word's text.
pub(crate) fn vfp_text(w: u32) -> String {
    let dbl = bits(w, 8, 1) == 1;
    let ty = if dbl { "f64" } else { "f32" };
    let r = |lo: u32, hi: u32, d: bool| {
        if d {
            format!("d{}", dreg(w, lo, hi))
        } else {
            format!("s{}", sreg(w, lo, hi))
        }
    };
    let (d, n, m) = (r(12, 22, dbl), r(16, 7, dbl), r(0, 5, dbl));
    let opc2 = bits(w, 16, 4);
    match (bits(w, 20, 4) & 0xb, bits(w, 6, 1)) {
        (3, 0) => format!("vadd.{ty}\t{d}, {n}, {m}"),
        (3, _) => format!("vsub.{ty}\t{d}, {n}, {m}"),
        (2, _) => format!("vmul.{ty}\t{d}, {n}, {m}"),
        (8, _) => format!("vdiv.{ty}\t{d}, {n}, {m}"),
        (10, 0) => format!("vfma.{ty}\t{d}, {n}, {m}"),
        _ => match opc2 {
            0 => format!("vmov.{ty}\t{d}, {m}"),
            1 if bits(w, 7, 1) == 1 => format!("vsqrt.{ty}\t{d}, {m}"),
            1 => format!("vneg.{ty}\t{d}, {m}"),
            4 => format!("vcmp.{ty}\t{d}, {m}"),
            5 => format!("vcmp.{ty}\t{d}, #0"),
            7 => {
                // To the other precision.
                let (to, dd) = if dbl {
                    ("f32", r(12, 22, false))
                } else {
                    ("f64", r(12, 22, true))
                };
                format!("vcvt.{to}.{ty}\t{dd}, {m}")
            }
            8 => {
                let s = if bits(w, 7, 1) == 1 { "s32" } else { "u32" };
                format!("vcvt.{ty}.{s}\t{d}, {}", r(0, 5, false))
            }
            _ => {
                let s = if bits(w, 16, 1) == 1 { "s32" } else { "u32" };
                format!("vcvt.{s}.{ty}\t{}, {m}", r(12, 22, false))
            }
        },
    }
}
