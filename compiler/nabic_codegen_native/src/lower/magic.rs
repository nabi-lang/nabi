//! Division by a constant as a multiply by its reciprocal (Granlund and
//! Montgomery): a high product, shifts and adds in place of a divide
//! that takes tens of cycles.

#[allow(clippy::wildcard_imports)]
use crate::lower::*;

/// How `x / d` of `n`-bit `x` is computed from `hi = high n bits of x * m`
/// (signed by the division's signedness).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Magic {
    /// Unsigned: `hi >> s`.
    U { m: u64, s: u32 },
    /// Unsigned, `m` one bit too wide for `n` bits: with `t = hi` of the
    /// low `n` bits of `m`, `(t + ((x - t) >> 1)) >> s`.
    UAdd { m: u64, s: u32 },
    /// Signed: `q = hi (+ x when add, - x when sub) >> s`, then plus one
    /// when negative (`q >>> (n - 1)`).
    S {
        m: i64,
        s: u32,
        add: bool,
        sub: bool,
    },
}

/// The plan for unsigned `n`-bit (`8..=64`) division by `d`, which is
/// below `2^(n-1)`, not zero, one or a power of two.
#[must_use]
pub(crate) fn unsigned_magic(d: u64, n: u32) -> Magic {
    let d128 = u128::from(d);
    let l = n - (d - 1).leading_zeros().saturating_sub(64 - n);
    // The smallest shift whose rounded-up reciprocal fits in n bits and is
    // exact for every n-bit dividend: its error at most 2^s.
    for s in 0..=l {
        if n + s >= 128 {
            break;
        }
        let p = 1u128 << (n + s);
        let m = p.div_ceil(d128);
        if m >> n == 0 && m * d128 - p <= 1u128 << s {
            return Magic::U { m: m as u64, s };
        }
    }
    // floor(2^(n + l) / d) - 2^n + 1 < 2^n, exact for all dividends.
    let m = ((1u128 << (n + l)) / d128) - (1u128 << n) + 1;
    Magic::UAdd {
        m: m as u64,
        s: l - 1,
    }
}

/// The plan for signed `n`-bit division by `d`: `2 <= |d| < 2^(n-1)`,
/// `|d|` not a power of two.
#[must_use]
pub(crate) fn signed_magic(d: i64, n: u32) -> Magic {
    let mask = if n == 128 {
        u128::MAX
    } else {
        (1u128 << n) - 1
    };
    let two = 1u128 << (n - 1);
    let ad = u128::from(d.unsigned_abs());
    let t = two + u128::from(d < 0);
    let anc = t - 1 - t % ad;
    let mut p = n - 1;
    let (mut q1, mut r1) = (two / anc, two - (two / anc) * anc);
    let (mut q2, mut r2) = (two / ad, two - (two / ad) * ad);
    loop {
        p += 1;
        q1 = (2 * q1) & mask;
        r1 = (2 * r1) & mask;
        if r1 >= anc {
            q1 = (q1 + 1) & mask;
            r1 -= anc;
        }
        q2 = (2 * q2) & mask;
        r2 = (2 * r2) & mask;
        if r2 >= ad {
            q2 = (q2 + 1) & mask;
            r2 -= ad;
        }
        let delta = ad - r2;
        if !(q1 < delta || (q1 == delta && r1 == 0)) {
            break;
        }
    }
    // The magic number as a signed n-bit value.
    let um = (q2 + 1) & mask;
    let mut m = if um >= two {
        um as i128 - (1i128 << n)
    } else {
        um as i128
    };
    if d < 0 {
        m = -m;
    }
    Magic::S {
        m: m as i64,
        s: p - n,
        add: d > 0 && m < 0,
        sub: d < 0 && m > 0,
    }
}

impl FnLower<'_, '_> {
    /// Whether the target has `Bin::MulHi` of `k`.
    pub(crate) fn mul_hi(&self, k: K) -> bool {
        match self.c.t.arch.as_str() {
            "x86_64" | "aarch64" | "powerpc64" | "riscv64" | "s390x" => k == K::I64,
            "x86" | "arm" | "riscv32" => k == K::I32,
            _ => false,
        }
    }

    /// `dst = a / d` (`op` `Div`) or `a % d` (`Rem`) of `bits`-bit `k`
    /// integers by a multiply, above `-O0`; `false` leaves the divide.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn div_const(
        &mut self,
        op: Bin,
        k: K,
        signed: bool,
        bits: u32,
        dst: Mem,
        a: V,
        d: i128,
    ) -> bool {
        if !self.c.opt || bits > 64 || k.is_float() || matches!(a, V::I(_)) {
            return false;
        }
        // Wide: the dividend extended to `I64`, a 64-bit product of 32-bit
        // halves (64-bit targets, wasm32). Else the high product of the
        // dividend's own width (or of `I32` for narrower ones).
        let i64_ok = self.pw() == 64 || self.c.t.arch == "wasm32";
        let (wk, n, wide) = if bits <= 32 && i64_ok {
            (K::I64, 32, true)
        } else if bits <= 32 && self.mul_hi(K::I32) {
            (K::I32, 32, false)
        } else if bits == 64 && self.mul_hi(K::I64) {
            (K::I64, 64, false)
        } else {
            return false;
        };
        let mask = (1u128 << bits) - 1;
        let du = (d as u128) & mask;
        let ds = if signed {
            let sh = 128 - bits;
            ((du << sh) as i128) >> sh
        } else {
            du as i128
        };
        let ad = ds.unsigned_abs();
        if ad < 2 || ad.is_power_of_two() || (signed && ad >= 1u128 << (bits - 1)) {
            return false;
        }
        // A fast divider (arm64) beats the longer forms: only an
        // unsigned quotient by a multiply and one shift is worth it there.
        let fast = self.c.t.arch == "aarch64";
        let half = !signed && du >= 1u128 << (n - 1);
        if fast
            && !half
            && (signed
                || op == Bin::Rem
                || matches!(unsigned_magic(du as u64, n), Magic::UAdd { .. }))
        {
            return false;
        }
        let bin = |s: &mut Self, op: Bin, signed: bool, a: V, b: V| {
            let t = s.tmp_k(wk);
            s.push(Inst::Bin {
                op,
                k: wk,
                signed,
                dst: t,
                a,
                b,
            });
            V::M(t)
        };
        let x = if wk == k {
            a
        } else {
            let t = self.tmp_k(wk);
            self.push(Inst::Conv {
                conv: Conv::Int { signed },
                from: k,
                to: wk,
                dst: t,
                a,
            });
            V::M(t)
        };
        // An `I32` immediate as its sign-extended bits.
        let imm = |v: i128| {
            if wk == K::I32 {
                V::I(i64::from(v as u32 as i32))
            } else {
                V::I(v as u64 as i64)
            }
        };
        let q = if half {
            // At least half the range: the quotient is 0 or 1.
            let c = self.tmp_k(K::I8);
            self.push(Inst::Cmp {
                cc: Cc::Ge,
                k: wk,
                signed: false,
                dst: c,
                a: x,
                b: imm(ds),
            });
            let t = self.tmp_k(wk);
            self.push(Inst::Conv {
                conv: Conv::Int { signed: false },
                from: K::I8,
                to: wk,
                dst: t,
                a: V::M(c),
            });
            V::M(t)
        } else if signed {
            let Magic::S { m, s, add, sub } = signed_magic(ds as i64, n) else {
                unreachable!()
            };
            let mut q = if wide && !add && !sub {
                let p = bin(self, Bin::Mul, true, x, V::I(m));
                bin(self, Bin::Shr, true, p, V::I(i64::from(32 + s)))
            } else {
                let mut h = if wide {
                    let p = bin(self, Bin::Mul, true, x, V::I(m));
                    bin(self, Bin::Shr, true, p, V::I(32))
                } else {
                    bin(self, Bin::MulHi, true, x, imm(i128::from(m)))
                };
                if add {
                    h = bin(self, Bin::Add, true, h, x);
                }
                if sub {
                    h = bin(self, Bin::Sub, true, h, x);
                }
                if s > 0 {
                    h = bin(self, Bin::Shr, true, h, V::I(i64::from(s)));
                }
                h
            };
            let top = i64::from(wk.bits() - 1);
            let neg = bin(self, Bin::Shr, false, q, V::I(top));
            q = bin(self, Bin::Add, true, q, neg);
            q
        } else {
            match unsigned_magic(du as u64, n) {
                Magic::U { m, s } if wide => {
                    let p = bin(self, Bin::Mul, false, x, V::I(m as i64));
                    bin(self, Bin::Shr, false, p, V::I(i64::from(32 + s)))
                }
                Magic::U { m, s } => {
                    let h = bin(self, Bin::MulHi, false, x, imm(i128::from(m)));
                    if s > 0 {
                        bin(self, Bin::Shr, false, h, V::I(i64::from(s)))
                    } else {
                        h
                    }
                }
                Magic::UAdd { m, s } => {
                    let t = if wide {
                        let p = bin(self, Bin::Mul, false, x, V::I(m as i64));
                        bin(self, Bin::Shr, false, p, V::I(32))
                    } else {
                        bin(self, Bin::MulHi, false, x, imm(i128::from(m)))
                    };
                    let u = bin(self, Bin::Sub, false, x, t);
                    let u = bin(self, Bin::Shr, false, u, V::I(1));
                    let u = bin(self, Bin::Add, false, u, t);
                    if s > 0 {
                        bin(self, Bin::Shr, false, u, V::I(i64::from(s)))
                    } else {
                        u
                    }
                }
                Magic::S { .. } => unreachable!(),
            }
        };
        let r = if op == Bin::Rem {
            let p = bin(self, Bin::Mul, signed, q, imm(ds));
            bin(self, Bin::Sub, signed, x, p)
        } else {
            q
        };
        if wk == k {
            self.push(Inst::Mov { k, dst, src: r });
        } else {
            self.push(Inst::Conv {
                conv: Conv::Int { signed },
                from: wk,
                to: k,
                dst,
                a: r,
            });
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The quotient the plan computes, in exact arithmetic.
    fn run(p: Magic, x: i128, n: u32) -> i128 {
        match p {
            Magic::U { m, s } => ((x as u128 * u128::from(m)) >> n >> s) as i128,
            Magic::UAdd { m, s } => {
                let t = (x as u128 * u128::from(m)) >> n;
                ((t + ((x as u128 - t) >> 1)) >> s) as i128
            }
            Magic::S { m, s, add, sub } => {
                let mut q = (x * i128::from(m)) >> n;
                if add {
                    q += x;
                }
                if sub {
                    q -= x;
                }
                q >>= s;
                q + i128::from(q < 0)
            }
        }
    }

    fn divisors(n: u32) -> Vec<i128> {
        let mut v: Vec<i128> = (2..300).collect();
        for k in 2..n {
            for e in [-1i128, 0, 1] {
                v.push((1i128 << k) + e);
            }
        }
        v.extend([7, 10, 100, 1000, 641, 6_700_417, 274_177, 1_000_000_007]);
        v.retain(|&d| d < (1i128 << (n - 1)) && d > 1);
        v
    }

    fn xs(n: u32, signed: bool) -> Vec<i128> {
        let (lo, hi) = if signed {
            (-(1i128 << (n - 1)), (1i128 << (n - 1)) - 1)
        } else {
            (0, (1i128 << n) - 1)
        };
        let mut v = vec![lo, lo + 1, hi, hi - 1, 0, 1, -1, 2, -2];
        let mut r: u64 = 0x9e37_79b9_7f4a_7c15;
        for _ in 0..2000 {
            r ^= r << 13;
            r ^= r >> 7;
            r ^= r << 17;
            let x = i128::from(r) % (hi - lo + 1) + lo;
            v.push(x.clamp(lo, hi));
        }
        v.retain(|&x| (lo..=hi).contains(&x));
        v
    }

    #[test]
    fn exhaustive_small() {
        for n in [8, 16] {
            let ds: Vec<u64> = if n == 8 {
                (2..128).collect()
            } else {
                divisors(16).iter().map(|&d| d as u64).collect()
            };
            for d in ds {
                if !d.is_power_of_two() {
                    let p = unsigned_magic(d, n);
                    for x in 0..(1i128 << n) {
                        assert_eq!(run(p, x, n), x / i128::from(d), "u{n} {x}/{d}");
                    }
                }
                let ds = d as i64;
                if d < 1 << (n - 1) && !d.is_power_of_two() {
                    for sd in [ds, -ds] {
                        let p = signed_magic(sd, n);
                        for x in -(1i128 << (n - 1))..(1i128 << (n - 1)) {
                            assert_eq!(run(p, x, n), x / i128::from(sd), "i{n} {x}/{sd}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn wide() {
        for n in [32, 64] {
            for d in divisors(n) {
                let ud = d as u64;
                if !ud.is_power_of_two() {
                    let p = unsigned_magic(ud, n);
                    for x in xs(n, false) {
                        assert_eq!(run(p, x, n), x / d, "u{n} {x}/{d}");
                    }
                }
                if d < (1i128 << (n - 1)) && !ud.is_power_of_two() {
                    for sd in [d as i64, -(d as i64)] {
                        let p = signed_magic(sd, n);
                        for x in xs(n, true) {
                            assert_eq!(run(p, x, n), x / i128::from(sd), "i{n} {x}/{sd}");
                        }
                    }
                }
            }
        }
    }
}
