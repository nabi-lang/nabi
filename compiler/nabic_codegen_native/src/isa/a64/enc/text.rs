//! Instruction text, as llvm-objdump prints it.

#[allow(clippy::wildcard_imports)]
use crate::isa::a64::enc::*;

/// `s` in the Apple syntax: the first vector operand's arrangement on
/// the mnemonic (`add.2d v0, v1, v2`, `umov.b w16, v31[0]`).
pub(crate) fn apple_neon(s: &str) -> String {
    let Some((mn, ops)) = s.split_once('\t') else {
        return s.to_owned();
    };
    let b = ops.as_bytes();
    let mut arr = None;
    let mut out = String::new();
    let mut i = 0;
    while i < b.len() {
        out.push(b[i] as char);
        let reg = b[i] == b'v' && (i == 0 || b[i - 1] == b' ');
        i += 1;
        if !reg {
            continue;
        }
        let d = b[i..].iter().take_while(|c| c.is_ascii_digit()).count();
        if d == 0 || b.get(i + d) != Some(&b'.') {
            continue;
        }
        out.push_str(&ops[i..i + d]);
        i += d + 1;
        let n = b[i..]
            .iter()
            .take_while(|c| c.is_ascii_alphanumeric())
            .count();
        arr.get_or_insert(&ops[i..i + n]);
        i += n;
    }
    // A scalar from a lane (`mov s0, v1[2]`) names no arrangement.
    let scalar = mn == "mov" && matches!(b.first(), Some(b'b' | b'h' | b's' | b'd'));
    match arr {
        Some(_) if scalar => format!("{mn}\t{out}"),
        Some(a) => format!("{mn}.{a}\t{out}"),
        None => s.to_owned(),
    }
}

pub(crate) const fn bits(w: u32, lo: u32, n: u32) -> u32 {
    (w >> lo) & ((1 << n) - 1)
}

/// A signed field.
pub(crate) fn sbits(w: u32, lo: u32, n: u32) -> i64 {
    let v = i64::from(bits(w, lo, n));
    (v << (64 - n)) >> (64 - n)
}

/// `x<r>` / `w<r>`; 31 is `sp` (`sp`) or the zero register.
pub(crate) fn reg(r: u32, x: bool, sp: bool) -> String {
    match (r, x, sp) {
        (31, true, true) => "sp".to_owned(),
        (31, false, true) => "wsp".to_owned(),
        (31, true, false) => "xzr".to_owned(),
        (31, false, false) => "wzr".to_owned(),
        (_, true, _) => format!("x{r}"),
        (_, false, _) => format!("w{r}"),
    }
}

pub(crate) fn hex(v: i64) -> String {
    if v < 0 {
        format!("#-{:#x}", v.unsigned_abs())
    } else {
        format!("#{v:#x}")
    }
}

/// `[xn{, #off}]`.
pub(crate) fn mem(rn: u32, off: i64) -> String {
    if off == 0 {
        format!("[{}]", reg(rn, true, true))
    } else {
        format!("[{}, {}]", reg(rn, true, true), hex(off))
    }
}

/// The value of logical immediate `N:immr:imms` in `x` / `w`.
pub(crate) const fn decode_bitmask(n: u32, immr: u32, imms: u32, x: bool) -> u64 {
    let len = 31 - ((n << 6) | (!imms & 0x3f)).leading_zeros();
    let size = 1u32 << len;
    let s = imms & (size - 1);
    let r = immr & (size - 1);
    let mask = if size == 64 {
        u64::MAX
    } else {
        (1u64 << size) - 1
    };
    let ones = (1u64 << (s + 1)) - 1;
    let e = if r == 0 {
        ones
    } else {
        ((ones >> r) | (ones << (size - r))) & mask
    };
    let mut v = 0u64;
    let mut i = 0;
    while i < 64 {
        v |= e << i;
        i += size;
    }
    if x { v } else { v & 0xffff_ffff }
}

/// The text of word `w` of form `f` at byte offset `at`.
#[allow(clippy::too_many_lines)]
pub(crate) fn text(w: u32, f: F, at: i64, l: &Listing<'_>) -> String {
    let x = w >> 31 == 1;
    let (rd, rn, rm) = (bits(w, 0, 5), bits(w, 5, 5), bits(w, 16, 5));
    let r = |n: u32| reg(n, x, false);
    let target = |d: i64| l.target(at + 4 * d);
    match f {
        F::MovWide => {
            let (opc, hw, imm) = (bits(w, 29, 2), bits(w, 21, 2), u64::from(bits(w, 5, 16)));
            let alias = imm != 0 || hw == 0;
            match opc {
                0 if alias => format!("mov\t{}, {}", r(rd), hex(!(imm << (16 * hw)) as i64)),
                2 if alias => format!("mov\t{}, {}", r(rd), hex((imm << (16 * hw)) as i64)),
                _ => {
                    let mn = ["movn", "", "movz", "movk"][opc as usize];
                    let sh = if hw == 0 {
                        String::new()
                    } else {
                        format!(", lsl #{}", 16 * hw)
                    };
                    format!("{mn}\t{}, {}{sh}", r(rd), hex(imm as i64))
                }
            }
        }
        F::AddImm => {
            let (op, sh, imm) = (bits(w, 29, 2), bits(w, 22, 1), i64::from(bits(w, 10, 12)));
            let s = op & 1 == 1;
            let shs = if sh == 1 { ", lsl #12" } else { "" };
            let rdn = reg(rd, x, !s);
            let rnn = reg(rn, x, true);
            if op == 0 && sh == 0 && imm == 0 && (rd == 31 || rn == 31) {
                format!("mov\t{rdn}, {rnn}")
            } else if s && rd == 31 {
                let mn = if op == 3 { "cmp" } else { "cmn" };
                format!("{mn}\t{rnn}, {}{shs}", hex(imm))
            } else {
                let mn = ["add", "adds", "sub", "subs"][op as usize];
                format!("{mn}\t{rdn}, {rnn}, {}{shs}", hex(imm))
            }
        }
        F::AddReg => {
            let (op, sh, n) = (bits(w, 29, 2), bits(w, 22, 2), bits(w, 10, 6));
            let shs = if n == 0 && sh == 0 {
                String::new()
            } else {
                format!(", {} #{n}", ["lsl", "lsr", "asr", "ror"][sh as usize])
            };
            if op & 1 == 1 && rd == 31 {
                let mn = if op == 3 { "cmp" } else { "cmn" };
                format!("{mn}\t{}, {}{shs}", r(rn), r(rm))
            } else if op >= 2 && rn == 31 {
                let mn = if op == 3 { "negs" } else { "neg" };
                format!("{mn}\t{}, {}{shs}", r(rd), r(rm))
            } else {
                let mn = ["add", "adds", "sub", "subs"][op as usize];
                format!("{mn}\t{}, {}, {}{shs}", r(rd), r(rn), r(rm))
            }
        }
        F::AddExt => {
            let (op, opt, n) = (bits(w, 29, 2), bits(w, 13, 3), bits(w, 10, 3));
            let s = op & 1 == 1;
            let rdn = reg(rd, x, !s);
            let rnn = reg(rn, x, true);
            let rmn = reg(rm, x && opt & 3 == 3, false);
            let lsl = if x { 3 } else { 2 };
            let ext = if ((rd == 31 && !s) || rn == 31) && opt == lsl {
                if n == 0 {
                    String::new()
                } else {
                    format!(", lsl #{n}")
                }
            } else {
                let e = [
                    "uxtb", "uxth", "uxtw", "uxtx", "sxtb", "sxth", "sxtw", "sxtx",
                ];
                let a = if n == 0 {
                    String::new()
                } else {
                    format!(" #{n}")
                };
                format!(", {}{a}", e[opt as usize])
            };
            if s && rd == 31 {
                let mn = if op == 3 { "cmp" } else { "cmn" };
                format!("{mn}\t{rnn}, {rmn}{ext}")
            } else {
                let mn = ["add", "adds", "sub", "subs"][op as usize];
                format!("{mn}\t{rdn}, {rnn}, {rmn}{ext}")
            }
        }
        F::LogReg => {
            let (op, not) = (bits(w, 29, 2), bits(w, 21, 1));
            if op == 1 && rn == 31 {
                let mn = if not == 1 { "mvn" } else { "mov" };
                format!("{mn}\t{}, {}", r(rd), r(rm))
            } else if op == 3 && not == 0 && rd == 31 {
                format!("tst\t{}, {}", r(rn), r(rm))
            } else {
                let mn = [["and", "orr", "eor", "ands"], ["bic", "orn", "eon", "bics"]]
                    [not as usize][op as usize];
                format!("{mn}\t{}, {}, {}", r(rd), r(rn), r(rm))
            }
        }
        F::LogImm => {
            let op = bits(w, 29, 2);
            let v = decode_bitmask(bits(w, 22, 1), bits(w, 16, 6), bits(w, 10, 6), x);
            let imm = format!("#{v:#x}");
            if op == 3 && rd == 31 {
                format!("tst\t{}, {imm}", r(rn))
            } else {
                let mn = ["and", "orr", "eor", "ands"][op as usize];
                format!("{mn}\t{}, {}, {imm}", reg(rd, x, op != 3), r(rn))
            }
        }
        F::Bitfield => {
            let signed = bits(w, 29, 2) == 0;
            let (ir, is) = (bits(w, 16, 6), bits(w, 10, 6));
            let b = if x { 64 } else { 32 };
            let (d, n) = (r(rd), r(rn));
            if is == b - 1 {
                let mn = if signed { "asr" } else { "lsr" };
                format!("{mn}\t{d}, {n}, #{ir}")
            } else if !signed && is + 1 == ir {
                format!("lsl\t{d}, {n}, #{}", b - 1 - is)
            } else if ir == 0
                && (signed || !x)
                && (is == 7 || is == 15 || (is == 31 && signed && x))
            {
                let mn = match (signed, is) {
                    (true, 7) => "sxtb",
                    (true, 15) => "sxth",
                    (true, _) => "sxtw",
                    (false, 7) => "uxtb",
                    _ => "uxth",
                };
                format!("{mn}\t{d}, {}", reg(rn, false, false))
            } else if is < ir {
                let mn = if signed { "sbfiz" } else { "ubfiz" };
                format!("{mn}\t{d}, {n}, #{}, #{}", b - ir, is + 1)
            } else {
                let mn = if signed { "sbfx" } else { "ubfx" };
                format!("{mn}\t{d}, {n}, #{ir}, #{}", is - ir + 1)
            }
        }
        F::Extr => {
            let lsb = hex(i64::from(bits(w, 10, 6)));
            if rn == rm {
                format!("ror\t{}, {}, {lsb}", r(rd), r(rn))
            } else {
                format!("extr\t{}, {}, {}, {lsb}", r(rd), r(rn), r(rm))
            }
        }
        F::Dp1 => {
            let mn = match (bits(w, 10, 3), x) {
                (0, _) => "rbit",
                (1, _) => "rev16",
                (2, true) => "rev32",
                (2, false) | (3, _) => "rev",
                _ => "clz",
            };
            format!("{mn}\t{}, {}", r(rd), r(rn))
        }
        F::Dp2 => {
            let mn = match bits(w, 10, 6) {
                2 => "udiv",
                3 => "sdiv",
                8 => "lsl",
                9 => "lsr",
                10 => "asr",
                _ => "ror",
            };
            format!("{mn}\t{}, {}, {}", r(rd), r(rn), r(rm))
        }
        F::Dp3 => {
            let ra = bits(w, 10, 5);
            let (op, o0) = (bits(w, 21, 3), bits(w, 15, 1));
            match (op, o0) {
                (0, 0) if ra == 31 => format!("mul\t{}, {}, {}", r(rd), r(rn), r(rm)),
                (0, _) => {
                    let mn = if o0 == 0 { "madd" } else { "msub" };
                    format!("{mn}\t{}, {}, {}, {}", r(rd), r(rn), r(rm), r(ra))
                }
                (1 | 5, _) => {
                    let mn = if op == 1 { "smull" } else { "umull" };
                    let w = |n| reg(n, false, false);
                    format!("{mn}\t{}, {}, {}", r(rd), w(rn), w(rm))
                }
                _ => {
                    let mn = if op == 2 { "smulh" } else { "umulh" };
                    format!("{mn}\t{}, {}, {}", r(rd), r(rn), r(rm))
                }
            }
        }
        F::CondSel => {
            let c = bits(w, 12, 4);
            if bits(w, 10, 1) == 1 && rn == 31 && rm == 31 {
                format!("cset\t{}, {}", r(rd), COND[(c ^ 1) as usize])
            } else if bits(w, 30, 1) == 1 {
                let (rd, rn, rm) = (r(rd), r(rn), r(rm));
                format!("csinv\t{rd}, {rn}, {rm}, {}", COND[c as usize])
            } else {
                format!(
                    "csel\t{}, {}, {}, {}",
                    r(rd),
                    r(rn),
                    r(rm),
                    COND[c as usize]
                )
            }
        }
        F::B => {
            let mn = if x { "bl" } else { "b" };
            format!("{mn}\t{}", target(sbits(w, 0, 26)))
        }
        F::BCond => format!(
            "b.{}\t{}",
            COND[bits(w, 0, 4) as usize],
            target(sbits(w, 5, 19))
        ),
        F::Cbz => {
            let mn = if bits(w, 24, 1) == 1 { "cbnz" } else { "cbz" };
            let rt = reg(rd, x, false);
            format!("{mn}\t{rt}, {}", target(sbits(w, 5, 19)))
        }
        F::Blr => format!("blr\t{}", reg(rn, true, false)),
        F::Ret => "ret".to_owned(),
        F::LdSt => ldst_text(w),
        F::Pair => {
            let (op, ix, l) = (bits(w, 30, 2), bits(w, 23, 2), bits(w, 22, 1));
            let rt2 = bits(w, 10, 5);
            let (mn, t) = (
                if l == 1 { "ldp" } else { "stp" },
                if op == 2 { "x" } else { "d" },
            );
            let off = 8 * sbits(w, 15, 7);
            let b = reg(rn, true, true);
            let m = match ix {
                1 => format!("[{b}], {}", hex(off)),
                3 => format!("[{b}, {}]!", hex(off)),
                _ => mem(rn, off),
            };
            let rr = |n: u32| {
                if t == "x" {
                    reg(n, true, false)
                } else {
                    format!("d{n}")
                }
            };
            format!("{mn}\t{}, {}, {m}", rr(rd), rr(rt2))
        }
        F::Excl => {
            let size = bits(w, 30, 2);
            let sfx = ["b", "h", "", ""][size as usize];
            let rt = reg(rd, size == 3, false);
            let b = mem(rn, 0);
            if w & 0x3fff_fc00 == 0x38bf_c000 {
                return format!("ldapr{sfx}\t{rt}, {b}");
            }
            let a = if bits(w, 15, 1) == 1 { "a" } else { "" };
            let l = if bits(w, 15, 1) == 1 { "l" } else { "" };
            match bits(w, 22, 2) {
                3 => format!("ldar{sfx}\t{rt}, {b}"),
                2 => format!("stlr{sfx}\t{rt}, {b}"),
                1 => format!("ld{a}xr{sfx}\t{rt}, {b}"),
                _ => format!("st{l}xr{sfx}\t{}, {rt}, {b}", reg(rm, false, false)),
            }
        }
        F::Lse => {
            let size = bits(w, 30, 2);
            let sfx = ["b", "h", "", ""][size as usize];
            let x = size == 3;
            let mn = match bits(w, 12, 4) {
                0 => "ldadd",
                1 => "ldclr",
                2 => "ldeor",
                3 => "ldset",
                _ => "swp",
            };
            let o = ["", "l", "a", "al"][bits(w, 22, 2) as usize];
            let (s, t) = (reg(rm, x, false), reg(rd, x, false));
            format!("{mn}{o}{sfx}\t{s}, {t}, {}", mem(rn, 0))
        }
        F::Cas => {
            let size = bits(w, 30, 2);
            let sfx = ["b", "h", "", ""][size as usize];
            let x = size == 3;
            let o = match (bits(w, 22, 1), bits(w, 15, 1)) {
                (0, 0) => "",
                (0, _) => "l",
                (_, 0) => "a",
                _ => "al",
            };
            let (s, t) = (reg(rm, x, false), reg(rd, x, false));
            format!("cas{o}{sfx}\t{s}, {t}, {}", mem(rn, 0))
        }
        F::Clrex => "clrex".to_owned(),
        F::Nop => "nop".to_owned(),
        F::Mrs => format!("mrs\t{}, TPIDR_EL0", reg(rd, true, false)),
        F::Brk => format!("brk\t{}", hex(i64::from(bits(w, 5, 16)))),
        F::Adrp => {
            let imm = (sbits(w, 5, 19) << 2) | i64::from(bits(w, 29, 2));
            format!("adrp\t{}, {}", r(rd), l.target((at & !0xfff) + (imm << 12)))
        }
        F::FmovGpr => {
            let d = x;
            let (g, v) = (reg(0, d, false), if d { "d" } else { "s" });
            let g = &g[..1];
            if bits(w, 16, 1) == 0 {
                format!("fmov\t{g}{rd}, {v}{rn}")
            } else {
                format!("fmov\t{v}{rd}, {g}{rn}")
            }
        }
        F::Fp if w & !0x3ff == 0x1e23_c000 => format!("fcvt\th{rd}, s{rn}"),
        F::Fp if w & !0x3ff == 0x1ee2_4000 => format!("fcvt\ts{rd}, h{rn}"),
        F::Fp => {
            let t = if bits(w, 22, 1) == 1 { "d" } else { "s" };
            let o = if t == "d" { "s" } else { "d" };
            if bits(w, 10, 2) == 2 {
                let mn = match bits(w, 12, 4) {
                    0 => "fmul",
                    1 => "fdiv",
                    2 => "fadd",
                    _ => "fsub",
                };
                format!("{mn}\t{t}{rd}, {t}{rn}, {t}{rm}")
            } else if bits(w, 10, 4) == 8 {
                format!("fcmp\t{t}{rn}, {t}{rm}")
            } else {
                match bits(w, 15, 6) {
                    0 => format!("fmov\t{t}{rd}, {t}{rn}"),
                    2 => format!("fneg\t{t}{rd}, {t}{rn}"),
                    3 => format!("fsqrt\t{t}{rd}, {t}{rn}"),
                    _ => format!("fcvt\t{o}{rd}, {t}{rn}"),
                }
            }
        }
        F::Fp3 => {
            let t = if bits(w, 22, 1) == 1 { "d" } else { "s" };
            let ra = bits(w, 10, 5);
            format!("fmadd\t{t}{rd}, {t}{rn}, {t}{rm}, {t}{ra}")
        }
        F::Cvtf => {
            let t = if bits(w, 22, 1) == 1 { "d" } else { "s" };
            let mn = if bits(w, 16, 1) == 0 {
                "scvtf"
            } else {
                "ucvtf"
            };
            format!("{mn}\t{t}{rd}, x{rn}")
        }
        F::Fcvtz => {
            let t = if bits(w, 22, 1) == 1 { "d" } else { "s" };
            let mn = if bits(w, 16, 1) == 0 {
                "fcvtzs"
            } else {
                "fcvtzu"
            };
            format!("{mn}\t{}, {t}{rn}", reg(rd, true, false))
        }
        F::Neon3 => {
            let (u, s, opc) = (bits(w, 29, 1), bits(w, 22, 2), bits(w, 11, 5));
            let fp = opc >= 0b11000;
            let mn = match (opc, u, s) {
                (0b10000, 0, _) => "add",
                (0b10000, _, _) => "sub",
                (0b10011, _, _) => "mul",
                (0b00011, 0, 0) => "and",
                (0b00011, 0, 1) => "bic",
                (0b00011, 0, 2) if rn == rm => "mov",
                (0b00011, 0, _) => "orr",
                (0b00011, _, _) => "eor",
                (0b10001, _, _) => "cmeq",
                (0b00110, 0, _) => "cmgt",
                (0b00110, _, _) => "cmhi",
                (0b01100, 0, _) => "smax",
                (0b01100, _, _) => "umax",
                (0b01101, 0, _) => "smin",
                (0b01101, _, _) => "umin",
                (0b11010, _, 0 | 1) => "fadd",
                (0b11010, _, _) => "fsub",
                (0b11011, _, _) => "fmul",
                (0b11111, _, _) => "fdiv",
                (0b11100, 0, _) => "fcmeq",
                (0b11100, _, 0 | 1) => "fcmge",
                (0b11110, _, 0 | 1) => "fmax",
                (0b11110, _, _) => "fmin",
                (0b11001, _, _) => "fmla",
                _ => "fcmgt",
            };
            let t = if opc == 0b00011 {
                "16b"
            } else if fp {
                ["4s", "2d"][(s & 1) as usize]
            } else {
                ["16b", "8h", "4s", "2d"][s as usize]
            };
            if mn == "mov" {
                format!("mov\tv{rd}.{t}, v{rn}.{t}")
            } else {
                format!("{mn}\tv{rd}.{t}, v{rn}.{t}, v{rm}.{t}")
            }
        }
        F::NeonMisc => {
            let s = bits(w, 22, 2);
            let t = ["16b", "8h", "4s", "2d"][s as usize];
            match w & 0xff3f_fc00 {
                0x0e20_5800 => format!("cnt\tv{rd}.8b, v{rn}.8b"),
                0x0e31_b800 => format!("addv\tb{rd}, v{rn}.8b"),
                0x6e30_a800 => format!("umaxv\tb{rd}, v{rn}.16b"),
                0x6e20_5800 => format!("mvn\tv{rd}.16b, v{rn}.16b"),
                0x4e20_a800 => format!("cmlt\tv{rd}.{t}, v{rn}.{t}, #0"),
                0x4e20_b800 => format!("abs\tv{rd}.{t}, v{rn}.{t}"),
                0x0e21_2800 => {
                    let n = ["8b", "4h", "2s"][s as usize];
                    let wide = ["8h", "4s", "2d"][s as usize];
                    format!("xtn\tv{rd}.{n}, v{rn}.{wide}")
                }
                _ => {
                    let t = ["4s", "2d"][(s & 1) as usize];
                    format!("fsqrt\tv{rd}.{t}, v{rn}.{t}")
                }
            }
        }
        F::NeonShift => {
            let ih = bits(w, 16, 7);
            let lane: u32 = 1 << (31 - (ih >> 3).leading_zeros());
            let t = |l: u32| match l {
                1 => "16b",
                2 => "8h",
                4 => "4s",
                _ => "2d",
            };
            match bits(w, 11, 5) {
                0b10100 => {
                    let mn = if bits(w, 29, 1) == 1 {
                        "ushll"
                    } else {
                        "sshll"
                    };
                    let n = ["8b", "4h", "2s"][(lane.trailing_zeros()) as usize];
                    let sh = ih - 8 * lane;
                    format!(
                        "{mn}\tv{rd}.{}, v{rn}.{n}, {}",
                        t(2 * lane),
                        hex(i64::from(sh))
                    )
                }
                0b01010 => {
                    let sh = ih - 8 * lane;
                    format!(
                        "shl\tv{rd}.{}, v{rn}.{}, {}",
                        t(lane),
                        t(lane),
                        hex(i64::from(sh))
                    )
                }
                _ => {
                    let mn = if bits(w, 29, 1) == 1 { "ushr" } else { "sshr" };
                    let sh = 16 * lane - ih;
                    format!(
                        "{mn}\tv{rd}.{}, v{rn}.{}, {}",
                        t(lane),
                        t(lane),
                        hex(i64::from(sh))
                    )
                }
            }
        }
        F::Movi => {
            if bits(w, 16, 3) == 7 {
                format!("movi\tv{rd}.2d, #0xffffffffffffffff")
            } else {
                format!("movi\tv{rd}.2d, #0000000000000000")
            }
        }
        F::Dup => {
            let b = bits(w, 16, 5);
            let t = match b {
                1 => "16b",
                2 => "8h",
                4 => "4s",
                _ => "2d",
            };
            format!("dup\tv{rd}.{t}, {}", reg(rn, b == 8, false))
        }
        F::Umov => format!("umov\tw{rd}, v{rn}.b[0]"),
        F::Lane if w & 0xff3f_fc00 == 0x4e31_b800 => {
            let s = bits(w, 22, 2) as usize;
            format!(
                "addv\t{}{rd}, v{rn}.{}",
                ["b", "h", "s"][s],
                ["16b", "8h", "4s"][s]
            )
        }
        F::Lane if w & 0xffff_fc00 == 0x5ef1_b800 => format!("addp\td{rd}, v{rn}.2d"),
        F::Lane => {
            let i5 = bits(w, 16, 5);
            let sz = i5.trailing_zeros().min(3);
            let n = i5 >> (sz + 1);
            let t = ["b", "h", "s", "d"][sz as usize];
            if w & 0xffe0_8400 == 0x6e00_0400 {
                let m = bits(w, 11, 4) >> sz;
                format!("mov\tv{rd}.{t}[{n}], v{rn}.{t}[{m}]")
            } else if w & 0xffe0_fc00 == 0x4e00_1c00 {
                format!("mov\tv{rd}.{t}[{n}], {}", reg(rn, sz == 3, false))
            } else if w & 0xffe0_fc00 == 0x4e00_0400 {
                let v = ["16b", "8h", "4s", "2d"][sz as usize];
                format!("dup\tv{rd}.{v}, v{rn}.{t}[{n}]")
            } else if w & 0xffe0_fc00 == 0x5e00_0400 {
                format!("mov\t{t}{rd}, v{rn}.{t}[{n}]")
            } else if sz < 2 {
                format!("umov\tw{rd}, v{rn}.{t}[{n}]")
            } else {
                format!("mov\t{}, v{rn}.{t}[{n}]", reg(rd, sz == 3, false))
            }
        }
        F::NeonWide => {
            let s = bits(w, 22, 2) as usize;
            let t = ["16b", "8h", "4s", "2d"];
            if w & 0x9f20_fc00 == 0x0e20_c000 {
                let mn = match (bits(w, 29, 1), bits(w, 30, 1)) {
                    (0, 0) => "smull",
                    (0, _) => "smull2",
                    (_, 0) => "umull",
                    _ => "umull2",
                };
                let n = if bits(w, 30, 1) == 1 {
                    t[s]
                } else {
                    ["8b", "4h", "2s"][s]
                };
                let d = t[s + 1];
                format!("{mn}\tv{rd}.{d}, v{rn}.{n}, v{rm}.{n}")
            } else if w & 0xbf20_fc00 == 0x0e00_5800 {
                format!("uzp2\tv{rd}.{0}, v{rn}.{0}, v{rm}.{0}", t[s])
            } else {
                let mn = if bits(w, 29, 1) == 1 { "udot" } else { "sdot" };
                format!("{mn}\tv{rd}.4s, v{rn}.16b, v{rm}.16b")
            }
        }
    }
}

/// The text of a load / store word.
pub(crate) fn ldst_text(w: u32) -> String {
    let (size, v, opc) = (bits(w, 30, 2), bits(w, 26, 1), bits(w, 22, 2));
    let (rt, rn) = (bits(w, 0, 5), bits(w, 5, 5));
    let load = if v == 1 { opc & 1 == 1 } else { opc != 0 };
    let (base, t, scale) = if v == 1 {
        let (c, s) = match (size, opc >> 1) {
            (0, 1) => ("q", 4),
            (2, _) => ("s", 2),
            _ => ("d", 3),
        };
        (if load { "ldr" } else { "str" }, format!("{c}{rt}"), s)
    } else {
        let sfx = ["b", "h", "", ""][size as usize];
        let (mn, x) = match opc {
            0 => ("str", size == 3),
            1 => ("ldr", size == 3),
            _ => ("ldrs", opc == 2),
        };
        let mn: &'static str = match (mn, sfx) {
            ("str", "b") => "strb",
            ("str", "h") => "strh",
            ("ldr", "b") => "ldrb",
            ("ldr", "h") => "ldrh",
            ("ldrs", "b") => "ldrsb",
            ("ldrs", "h") => "ldrsh",
            ("ldrs", _) => "ldrsw",
            (m, _) => m,
        };
        (mn, reg(rt, x, false), size)
    };
    let b = reg(rn, true, true);
    if bits(w, 24, 1) == 1 {
        return format!(
            "{base}\t{t}, {}",
            mem(rn, i64::from(bits(w, 10, 12)) << scale)
        );
    }
    let imm = sbits(w, 12, 9);
    match (bits(w, 21, 1), bits(w, 10, 2)) {
        (1, _) => {
            let rm = reg(bits(w, 16, 5), true, false);
            if bits(w, 12, 1) == 1 {
                format!("{base}\t{t}, [{b}, {rm}, lsl #{scale}]")
            } else {
                format!("{base}\t{t}, [{b}, {rm}]")
            }
        }
        (_, 1) => format!("{base}\t{t}, [{b}], {}", hex(imm)),
        _ => {
            let mn = format!("{}u{}", &base[..2], &base[2..]);
            format!("{mn}\t{t}, {}", mem(rn, imm))
        }
    }
}
