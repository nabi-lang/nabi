//! Instruction text, as objdump prints it.

#[allow(clippy::wildcard_imports)]
use crate::isa::ppc64::enc::*;

pub(crate) const fn bits(w: u32, lo: u32, n: u32) -> u32 {
    (w >> lo) & ((1 << n) - 1)
}

/// The signed 16-bit immediate.
pub(crate) const fn simm(w: u32) -> i64 {
    w as u16 as i16 as i64
}

/// The text of word `w` of form `f` at byte offset `at`. objdump spells
/// most instructions `mn ops`, its aliases (`mr`, `sub`, `cmpd`, `bt`,
/// ..) `mn\tops`.
#[allow(clippy::too_many_lines)]
pub(crate) fn text(w: u32, f: Fm, at: i64, l: &Listing<'_>) -> String {
    let (t, a, b) = (bits(w, 21, 5), bits(w, 16, 5), bits(w, 11, 5));
    let xo = bits(w, 1, 10);
    match f {
        Fm::Fixed => match w {
            0x6000_0000 => "nop",
            0x4e80_0020 => "blr",
            0x4e80_0421 => "bctrl",
            0x7c00_04ac => "sync",
            0x7c20_04ac => "lwsync",
            0x4c00_012c => "isync",
            _ => "trap",
        }
        .to_owned(),
        Fm::Addi => {
            let (mn, li) = if w >> 26 == 14 {
                ("addi", "li")
            } else {
                ("addis", "lis")
            };
            if a == 0 {
                format!("{li} {t}, {}", simm(w))
            } else {
                format!("{mn} {t}, {a}, {}", simm(w))
            }
        }
        Fm::LogImm => {
            let mn = match w >> 26 {
                24 => "ori",
                25 => "oris",
                26 => "xori",
                _ => "andi.",
            };
            format!("{mn} {a}, {t}, {}", w & 0xffff)
        }
        Fm::CmpImm => {
            if w >> 26 == 11 {
                format!("cmpdi\t{a}, {}", simm(w))
            } else {
                format!("cmpldi\t{a}, {}", w & 0xffff)
            }
        }
        Fm::LdSt => {
            let (mn, off) = match (w >> 26, w & 3) {
                (58, 2) => ("lwa", simm(w & !3)),
                (58, _) => ("ld", simm(w & !3)),
                (62, 1) => ("stdu", simm(w & !3)),
                (62, _) => ("std", simm(w & !3)),
                (op, _) => {
                    let mn = match op {
                        32 => "lwz",
                        34 => "lbz",
                        36 => "stw",
                        38 => "stb",
                        40 => "lhz",
                        42 => "lha",
                        44 => "sth",
                        48 => "lfs",
                        50 => "lfd",
                        52 => "stfs",
                        _ => "stfd",
                    };
                    (mn, simm(w))
                }
            };
            format!("{mn} {t}, {off}({a})")
        }
        Fm::LdStX => {
            let mn = match xo {
                87 => "lbzx",
                343 => "lhax",
                279 => "lhzx",
                341 => "lwax",
                23 => "lwzx",
                21 => "ldx",
                215 => "stbx",
                407 => "sthx",
                151 => "stwx",
                149 => "stdx",
                181 => "stdux",
                535 => "lfsx",
                599 => "lfdx",
                663 => "stfsx",
                _ => "stfdx",
            };
            format!("{mn} {t}, {a}, {b}")
        }
        Fm::Alu => match bits(w, 1, 9) {
            40 => format!("sub\t{t}, {b}, {a}"),
            104 => format!("neg {t}, {a}"),
            x => {
                let mn = match x {
                    266 => "add",
                    233 => "mulld",
                    73 => "mulhd",
                    9 => "mulhdu",
                    489 => "divd",
                    _ => "divdu",
                };
                format!("{mn} {t}, {a}, {b}")
            }
        },
        Fm::Logic => match xo {
            444 if t == b => format!("mr\t{a}, {t}"),
            124 if t == b => format!("not\t{a}, {t}"),
            954 | 922 | 986 | 58 | 506 => {
                let mn = match xo {
                    954 => "extsb",
                    922 => "extsh",
                    986 => "extsw",
                    58 => "cntlzd",
                    _ => "popcntd",
                };
                format!("{mn} {a}, {t}")
            }
            _ => {
                let mn = match xo {
                    28 => "and",
                    60 => "andc",
                    444 => "or",
                    316 => "xor",
                    124 => "nor",
                    27 => "sld",
                    539 => "srd",
                    _ => "srad",
                };
                format!("{mn} {a}, {t}, {b}")
            }
        },
        Fm::Cmp => {
            let mn = if xo == 0 { "cmpd" } else { "cmpld" };
            format!("{mn}\t{a}, {b}")
        }
        Fm::Mfcr => format!("mfcr {t}"),
        Fm::Larx | Fm::Stcx | Fm::Brx => {
            let mn = match xo {
                52 => "lbarx",
                116 => "lharx",
                20 => "lwarx",
                84 => "ldarx",
                694 => "stbcx.",
                726 => "sthcx.",
                150 => "stwcx.",
                214 => "stdcx.",
                790 => "lhbrx",
                534 => "lwbrx",
                _ => "ldbrx",
            };
            format!("{mn} {t}, {a}, {b}")
        }
        Fm::Sradi => format!("sradi {a}, {t}, {}", b | (bits(w, 1, 1) << 5)),
        Fm::Rld => {
            let sh = b | (bits(w, 1, 1) << 5);
            let m = bits(w, 6, 5) | (bits(w, 5, 1) << 5);
            match bits(w, 2, 3) {
                0 if sh == 0 => format!("clrldi\t{a}, {t}, {m}"),
                0 if m == 0 => format!("rotldi\t{a}, {t}, {sh}"),
                0 => format!("rldicl {a}, {t}, {sh}, {m}"),
                _ if m == 63 - sh => format!("sldi {a}, {t}, {sh}"),
                _ => format!("rldicr {a}, {t}, {sh}, {m}"),
            }
        }
        Fm::Rlwinm => {
            let (mb, me) = (bits(w, 6, 5), bits(w, 1, 5));
            if me == 31 && b == (32 - mb) & 31 && mb != 0 {
                format!("srwi {a}, {t}, {mb}")
            } else if me == 31 && mb == 0 && b != 0 {
                format!("rotlwi\t{a}, {t}, {b}")
            } else {
                format!("rlwinm {a}, {t}, {b}, {mb}, {me}")
            }
        }
        Fm::Rot => {
            let mn = if w >> 26 == 30 { "rotld" } else { "rotlw" };
            format!("{mn}\t{a}, {t}, {b}")
        }
        Fm::Cror => format!("cror {t}, {a}, {b}"),
        Fm::Bc => {
            let target = l.target(at + simm(w & !3));
            match t {
                12 => format!("bt\t{a}, {target}"),
                4 => format!("bf\t{a}, {target}"),
                16 if a == 0 => format!("bdnz {target}"),
                _ => format!("bc {t}, {a}, {target}"),
            }
        }
        Fm::B => {
            let li = (i64::from(w & 0x03ff_fffc) << 38) >> 38;
            let mn = if w & 1 == 1 { "bl" } else { "b" };
            format!("{mn} {}", l.target(at + li))
        }
        Fm::Spr => {
            let mn = match w & !(31 << 21) {
                0x7c08_02a6 => "mflr",
                0x7c08_03a6 => "mtlr",
                _ => "mtctr",
            };
            format!("{mn} {t}")
        }
        Fm::Fp => {
            let s = if w >> 26 == 59 { "s" } else { "" };
            match bits(w, 1, 5) {
                25 => format!("fmul{s} {t}, {a}, {}", bits(w, 6, 5)),
                29 => format!("fmadd{s} {t}, {a}, {}, {b}", bits(w, 6, 5)),
                18 | 20 | 21 => {
                    let mn = ["fdiv", "", "fsub", "fadd"][bits(w, 1, 5) as usize - 18];
                    format!("{mn}{s} {t}, {a}, {b}")
                }
                22 => format!("fsqrt{s} {t}, {b}"),
                _ => {
                    let mn = match xo {
                        12 => "frsp",
                        40 => "fneg",
                        72 => "fmr",
                        815 => "fctidz",
                        846 => "fcfid",
                        943 => "fctiduz",
                        _ => "fcfidu",
                    };
                    format!("{mn}{s} {t}, {b}")
                }
            }
        }
        Fm::Fcmpu => format!("fcmpu 0, {a}, {b}"),
        Fm::Vx => {
            let x = w & 0x7ff;
            // A compare's record bit: 0x400 also selects VX operations.
            let rc = if x & 0x400 != 0 && matches!(x & 0x3f, 6 | 7) {
                "."
            } else {
                ""
            };
            let base = if rc.is_empty() { x } else { x & !0x400 };
            match base {
                0x484 if a == b => return format!("vmr\t{t}, {a}"),
                0x504 if a == b => return format!("vnot\t{t}, {a}"),
                _ => {}
            }
            let mn = match base {
                0x000 => "vaddubm",
                0x040 => "vadduhm",
                0x080 => "vadduwm",
                0x0c0 => "vaddudm",
                0x400 => "vsububm",
                0x440 => "vsubuhm",
                0x480 => "vsubuwm",
                0x4c0 => "vsubudm",
                0x089 => "vmuluwm",
                0x404 => "vand",
                0x484 => "vor",
                0x4c4 => "vxor",
                0x444 => "vandc",
                0x504 => "vnor",
                0x006 => "vcmpequb",
                0x046 => "vcmpequh",
                0x086 => "vcmpequw",
                0x0c7 => "vcmpequd",
                0x306 => "vcmpgtsb",
                0x346 => "vcmpgtsh",
                0x386 => "vcmpgtsw",
                0x3c7 => "vcmpgtsd",
                0x206 => "vcmpgtub",
                0x246 => "vcmpgtuh",
                0x286 => "vcmpgtuw",
                0x2c7 => "vcmpgtud",
                0x302 => "vminsb",
                0x342 => "vminsh",
                0x382 => "vminsw",
                0x3c2 => "vminsd",
                0x202 => "vminub",
                0x242 => "vminuh",
                0x282 => "vminuw",
                0x2c2 => "vminud",
                0x102 => "vmaxsb",
                0x142 => "vmaxsh",
                0x182 => "vmaxsw",
                0x1c2 => "vmaxsd",
                0x002 => "vmaxub",
                0x042 => "vmaxuh",
                0x082 => "vmaxuw",
                0x0c2 => "vmaxud",
                0x104 => "vslb",
                0x144 => "vslh",
                0x184 => "vslw",
                0x5c4 => "vsld",
                0x204 => "vsrb",
                0x244 => "vsrh",
                0x284 => "vsrw",
                0x6c4 => "vsrd",
                0x304 => "vsrab",
                0x344 => "vsrah",
                0x384 => "vsraw",
                _ => "vsrad",
            };
            format!("{mn}{rc} {t}, {a}, {b}")
        }
        Fm::Va => format!("vmladduhm {t}, {a}, {b}, {}", bits(w, 6, 5)),
        Fm::Vsplt => format!("vspltisb {t}, {}", (i64::from(a) << 59) >> 59),
        Fm::Xx1 => {
            let xt = t | (bits(w, 0, 1) << 5);
            match bits(w, 1, 10) {
                0x34c => format!("lxvd2x {xt}, {a}, {b}"),
                0x3cc => format!("stxvd2x {xt}, {a}, {b}"),
                _ => format!("mtvsrd {xt}, {a}"),
            }
        }
        Fm::Xx2 => {
            let (xt, xb) = (t | (bits(w, 0, 1) << 5), b | (bits(w, 1, 1) << 5));
            let p = if bits(w, 8, 1) == 1 { "dp" } else { "sp" };
            format!("xvsqrt{p} {xt}, {xb}")
        }
        Fm::Xx3 => {
            let xt = t | (bits(w, 0, 1) << 5);
            let xa = a | (bits(w, 2, 1) << 5);
            let xb = b | (bits(w, 1, 1) << 5);
            if bits(w, 3, 8) == 0x0a {
                return format!("xxspltd\t{xt}, {xa}, 0");
            }
            let p = if bits(w, 8, 1) == 1 { "dp" } else { "sp" };
            let mn = match bits(w, 3, 8) & !0x20 {
                0x40 => "xvadd",
                0x48 => "xvsub",
                0x50 => "xvmul",
                0x58 => "xvdiv",
                0x43 => "xvcmpeq",
                0x4b => "xvcmpgt",
                0xc0 => "xvmax",
                0xc8 => "xvmin",
                0x41 => "xvmadda",
                _ => "xvcmpge",
            };
            format!("{mn}{p} {xt}, {xa}, {xb}")
        }
    }
}
