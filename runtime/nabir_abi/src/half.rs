//! IEEE 754 binary16 (`f16`) and bfloat16 (`bf16`) held as 16 bits.
//!
//! Conversions round to nearest, ties to even, never through a wider
//! format (no double rounding). A NaN converts to a quiet NaN of the
//! same sign; its payload keeps the high bits that fit. Also the
//! decimal text of a value.

#![allow(clippy::many_single_char_names)]

use core::cmp::Ordering;
use core::fmt::Write;

/// A 16-bit binary format: `man` stored significand bits, `exp`
/// exponent bits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Format {
    man: u32,
    exp: u32,
}

pub const F16: Format = Format { man: 10, exp: 5 };
pub const BF16: Format = Format { man: 7, exp: 8 };

impl Format {
    const fn bias(self) -> i64 {
        (1 << (self.exp - 1)) - 1
    }

    const fn inf(self) -> u16 {
        (((1u32 << self.exp) - 1) << self.man) as u16
    }

    const fn quiet(self) -> u16 {
        1 << (self.man - 1)
    }

    /// The quiet NaN of `neg`'s sign carrying `payload`'s high bits
    /// (`payload` left-aligned in 64 bits).
    const fn nan(self, neg: bool, payload: u64) -> u16 {
        let p = (payload >> (64 - self.man)) as u16;
        sign(neg) | self.inf() | self.quiet() | p
    }
}

const fn sign(neg: bool) -> u16 {
    if neg { 0x8000 } else { 0 }
}

/// The value of `f` nearest `m * 2^e` (`m != 0`) with `neg`'s sign;
/// `rest` says where the true value lies against `m * 2^e`, below one
/// unit of `m`, and decides a tie.
fn round(f: Format, neg: bool, m: u64, e: i64, rest: Ordering) -> u16 {
    let top = 63 - i64::from(m.leading_zeros());
    let lead = top + e;
    let emin = 1 - f.bias();
    if lead > f.bias() {
        return sign(neg) | f.inf();
    }
    // Bits of `m` below the result's last place.
    let shift = if lead >= emin {
        top - i64::from(f.man)
    } else {
        emin - i64::from(f.man) - e
    };
    let q = if shift <= 0 {
        m << (-shift)
    } else if shift > 64 {
        0
    } else {
        let (q, r) = if shift == 64 {
            (0, m)
        } else {
            (m >> shift, m & ((1 << shift) - 1))
        };
        let half = 1u64 << (shift - 1);
        let up = match r.cmp(&half) {
            Ordering::Greater => true,
            Ordering::Less => false,
            Ordering::Equal => match rest {
                Ordering::Greater => true,
                Ordering::Less => false,
                Ordering::Equal => q & 1 == 1,
            },
        };
        q + u64::from(up)
    };
    // A normal `q` holds the hidden bit, which adds one to the exponent
    // field; a carry out of the significand adds another.
    let bits = if lead >= emin {
        ((lead + f.bias() - 1) as u64) << f.man
    } else {
        0
    } + q;
    if bits >= u64::from(f.inf()) {
        return sign(neg) | f.inf();
    }
    sign(neg) | bits as u16
}

/// `x` in `f`.
#[must_use]
pub fn from_f64(f: Format, x: f64) -> u16 {
    let b = x.to_bits();
    let neg = b >> 63 != 0;
    let ex = ((b >> 52) & 0x7ff) as i64;
    let frac = b & ((1 << 52) - 1);
    match ex {
        0x7ff if frac != 0 => f.nan(neg, frac << 12),
        0x7ff => sign(neg) | f.inf(),
        0 if frac == 0 => sign(neg),
        0 => round(f, neg, frac, -1074, Ordering::Equal),
        _ => round(f, neg, frac | (1 << 52), ex - 1075, Ordering::Equal),
    }
}

/// `x` in `f`.
#[must_use]
pub fn from_f32(f: Format, x: f32) -> u16 {
    let b = x.to_bits();
    let neg = b >> 31 != 0;
    let ex = i64::from((b >> 23) & 0xff);
    let frac = u64::from(b & ((1 << 23) - 1));
    match ex {
        0xff if frac != 0 => f.nan(neg, frac << 41),
        0xff => sign(neg) | f.inf(),
        0 if frac == 0 => sign(neg),
        0 => round(f, neg, frac, -149, Ordering::Equal),
        _ => round(f, neg, frac | (1 << 23), ex - 150, Ordering::Equal),
    }
}

/// The integer `mag` (negated when `neg`) in `f`.
#[must_use]
pub fn from_int(f: Format, neg: bool, mag: u64) -> u16 {
    if mag == 0 {
        return 0;
    }
    round(f, neg, mag, 0, Ordering::Equal)
}

/// `h` of `f` as an `f32`, exactly.
#[must_use]
pub fn to_f32(f: Format, h: u16) -> f32 {
    let nan = h & 0x7fff > f.inf();
    let q = if nan { 1 << 22 } else { 0 };
    if f == BF16 {
        // `f32`'s exponent range: the high half of its bits.
        return f32::from_bits((u32::from(h) << 16) | q);
    }
    let neg = u32::from(h >> 15) << 31;
    let ex = u32::from(h & 0x7fff) >> f.man;
    let frac = u32::from(h) & ((1 << f.man) - 1);
    let up = 23 - f.man;
    let bias = f.bias() as i32;
    let bits = if ex == (1 << f.exp) - 1 {
        0x7f80_0000 | (frac << up) | q
    } else if ex != 0 {
        (((ex as i32 - bias + 127) as u32) << 23) | (frac << up)
    } else if frac == 0 {
        0
    } else {
        // Subnormal: normal in `f32`.
        let t = 31 - frac.leading_zeros();
        let e = t as i32 + 1 - bias - f.man as i32 + 127;
        ((e as u32) << 23) | ((frac ^ (1 << t)) << (23 - t))
    };
    f32::from_bits(neg | bits)
}

/// `h` of `f` as an `f64`, exactly.
#[must_use]
pub fn to_f64(f: Format, h: u16) -> f64 {
    f64::from(to_f32(f, h))
}

/// A decimal number `[+-]digits[.digits][(e|E)[+-]digits]` (or what
/// `f64` parses: `inf`, `NaN`) in `f`, rounded once from its exact value.
#[must_use]
pub fn parse(f: Format, s: &str) -> Option<u16> {
    let v: f64 = s.parse().ok()?;
    let h = from_f64(f, v);
    if !v.is_finite() || v == 0.0 {
        return Some(h);
    }
    // `v` is the decimal rounded once; when it is a tie of `f`, the
    // decimal itself breaks it.
    let b = v.to_bits();
    let frac = b & ((1 << 52) - 1);
    let ex = ((b >> 52) & 0x7ff) as i64;
    let (m, e) = if ex == 0 {
        (frac, -1074)
    } else {
        (frac | (1 << 52), ex - 1075)
    };
    let neg = v < 0.0;
    let (lo, hi) = (
        round(f, neg, m, e, Ordering::Less),
        round(f, neg, m, e, Ordering::Greater),
    );
    if lo == hi {
        return Some(h);
    }
    let rest = cmp_decimal(s, v).unwrap_or(Ordering::Equal);
    Some(round(f, neg, m, e, rest))
}

/// The decimal digits of `s` against `v` (same sign), when `s` is a
/// plain decimal and `v` is exact in 128 digits.
fn cmp_decimal(s: &str, v: f64) -> Option<Ordering> {
    let a = Decimal::read(s)?;
    let b = Decimal::of(v.abs())?;
    Some(a.cmp(&b))
}

/// `0.d1 d2 .. dn * 10^point`, no leading or trailing zero digit.
struct Decimal {
    digits: [u8; 128],
    len: usize,
    point: i64,
    /// More nonzero digits past `digits` (a long input).
    more: bool,
}

impl Decimal {
    fn read(s: &str) -> Option<Self> {
        let s = s.strip_prefix(['+', '-']).unwrap_or(s);
        let (mant, exp) = match s.find(['e', 'E']) {
            Some(i) => (&s[..i], s[i + 1..].parse::<i64>().ok()?),
            None => (s, 0),
        };
        let (int, frac) = mant.split_once('.').unwrap_or((mant, ""));
        if !int.bytes().chain(frac.bytes()).all(|c| c.is_ascii_digit()) {
            return None;
        }
        let mut d = Self {
            digits: [0; 128],
            len: 0,
            point: exp.saturating_add(i64::try_from(int.len()).ok()?),
            more: false,
        };
        let mut lead = true;
        for c in int.bytes().chain(frac.bytes()) {
            if lead && c == b'0' {
                d.point -= 1;
                continue;
            }
            lead = false;
            if d.len < d.digits.len() {
                d.digits[d.len] = c - b'0';
                d.len += 1;
            } else if c != b'0' {
                d.more = true;
            }
        }
        d.trim();
        Some(d)
    }

    /// `v > 0` exactly.
    fn of(v: f64) -> Option<Self> {
        let b = v.to_bits();
        let frac = b & ((1 << 52) - 1);
        let ex = ((b >> 52) & 0x7ff) as i64;
        let (mut m, mut e) = if ex == 0 {
            (frac, -1074)
        } else {
            (frac | (1 << 52), ex - 1075)
        };
        let tz = m.trailing_zeros();
        m >>= tz;
        e += i64::from(tz);
        // Little-endian digits of `m * 2^e` as `n * 10^min(e, 0)`.
        let mut le = [0u8; 128];
        let mut n = 0;
        while m > 0 {
            le[n] = (m % 10) as u8;
            m /= 10;
            n += 1;
        }
        let k = if e < 0 { 5 } else { 2 };
        for _ in 0..e.unsigned_abs() {
            let mut carry = 0;
            for x in &mut le[..n] {
                let y = *x * k + carry;
                *x = y % 10;
                carry = y / 10;
            }
            if carry > 0 {
                if n == le.len() {
                    return None;
                }
                le[n] = carry;
                n += 1;
            }
        }
        let mut d = Self {
            digits: [0; 128],
            len: n,
            point: i64::try_from(n).ok()? + e.min(0),
            more: false,
        };
        for (i, x) in le[..n].iter().rev().enumerate() {
            d.digits[i] = *x;
        }
        d.trim();
        Some(d)
    }

    fn trim(&mut self) {
        while self.len > 0 && self.digits[self.len - 1] == 0 {
            self.len -= 1;
        }
    }

    fn cmp(&self, o: &Self) -> Ordering {
        self.point.cmp(&o.point).then_with(|| {
            let n = self.len.max(o.len);
            (0..n)
                .map(|i| {
                    let a = if i < self.len { self.digits[i] } else { 0 };
                    let b = if i < o.len { o.digits[i] } else { 0 };
                    a.cmp(&b)
                })
                .find(|c| c.is_ne())
                .unwrap_or_else(|| self.more.cmp(&o.more))
        })
    }
}

/// A `d.ddde<x>` text in a fixed buffer.
struct Buf {
    b: [u8; 40],
    n: usize,
}

impl Write for Buf {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let end = self.n + s.len();
        if end > self.b.len() {
            return Err(core::fmt::Error);
        }
        self.b[self.n..end].copy_from_slice(s.as_bytes());
        self.n = end;
        Ok(())
    }
}

/// The `f64` of the fewest significant decimal digits that reads back
/// as `h` of `f` (finite): its shortest text is `h`'s.
#[must_use]
pub fn shortest(f: Format, h: u16) -> f64 {
    let v = to_f64(f, h);
    if !v.is_finite() || v == 0.0 {
        return v;
    }
    for p in 0..17 {
        let mut buf = Buf { b: [0; 40], n: 0 };
        if write!(buf, "{v:.p$e}").is_err() {
            break;
        }
        let Ok(text) = core::str::from_utf8(&buf.b[..buf.n]) else {
            break;
        };
        if parse(f, text) == Some(h) {
            return text.parse().unwrap_or(v);
        }
    }
    v
}

#[cfg(test)]
#[allow(clippy::float_cmp)] // exact bits are the point
mod tests {
    use super::*;

    #[test]
    fn f16_round_trip() {
        for h in 0..=u16::MAX {
            let v = to_f32(F16, h);
            if v.is_nan() {
                assert_eq!(from_f32(F16, v), h | F16.quiet());
                continue;
            }
            assert_eq!(from_f32(F16, v), h, "{h:#x}");
            assert_eq!(from_f64(F16, f64::from(v)), h, "{h:#x}");
            if v.is_finite() {
                assert_eq!(from_f64(F16, shortest(F16, h)), h, "{h:#x}");
            }
        }
    }

    #[test]
    fn bf16_round_trip() {
        for h in 0..=u16::MAX {
            let v = to_f32(BF16, h);
            if v.is_nan() {
                continue;
            }
            assert_eq!(v.to_bits() >> 16, u32::from(h));
            assert_eq!(from_f32(BF16, v), h, "{h:#x}");
            if v.is_finite() {
                assert_eq!(from_f64(BF16, shortest(BF16, h)), h, "{h:#x}");
            }
        }
    }

    /// Sampled `f32`s against the definition: no neighbour in `f` is
    /// nearer, and an even one wins a tie; every tie of `f` exactly.
    #[test]
    fn f32_rounds_to_nearest_even() {
        for f in [F16, BF16] {
            let check = |w: f32| {
                let h = from_f32(f, w);
                let w = f64::from(w);
                if h == f.inf() {
                    let max = to_f64(f, f.inf() - 1);
                    let thr = max + (max - to_f64(f, f.inf() - 2)) / 2.0;
                    assert!(w >= thr, "{w}");
                    return;
                }
                let d = |k: u16| (to_f64(f, k) - w).abs();
                for k in [h.wrapping_sub(1), h + 1] {
                    if k < f.inf() {
                        assert!(d(h) < d(k) || (d(h) == d(k) && h & 1 == 0), "{w}");
                    }
                }
            };
            let mut x = 0u32;
            while x < 0x7f80_0000 {
                check(f32::from_bits(x));
                x += 0x10 + (x >> 12);
            }
            for h in 0..f.inf() - 1 {
                let mid = f64::midpoint(to_f64(f, h), to_f64(f, h + 1));
                check(mid as f32);
            }
        }
    }

    #[test]
    fn specials() {
        assert_eq!(from_f64(F16, 65504.0), 0x7bff);
        assert_eq!(from_f64(F16, 65519.99), 0x7bff);
        assert_eq!(from_f64(F16, 65520.0), 0x7c00);
        assert_eq!(from_f64(F16, -0.0), 0x8000);
        assert_eq!(from_f64(F16, 2f64.powi(-24)), 1);
        assert_eq!(from_f64(F16, 2f64.powi(-25)), 0);
        assert_eq!(from_f64(F16, 2f64.powi(-25) * 1.000_001), 1);
        assert_eq!(from_f64(F16, f64::NAN) & 0x7e00, 0x7e00);
        assert_eq!(from_f64(BF16, 1.0), 0x3f80);
        assert_eq!(from_f64(BF16, f64::from(f32::MAX)), 0x7f80);
        assert_eq!(from_int(BF16, false, 257), 0x4380);
        assert_eq!(from_int(BF16, false, 259), 0x4382);
        assert_eq!(from_int(F16, true, 2049), 0xe800);
        // 2^63 + 2^55 + 1 is just above a bf16 tie; `f64` would round it
        // onto the tie.
        assert_eq!(from_int(BF16, false, (1 << 63) + (1 << 55) + 1), 0x5f01);
        assert_eq!(to_f32(F16, 1), 2f32.powi(-24));
        assert_eq!(to_f32(F16, 0x3c00), 1.0);
        assert_eq!(to_f32(F16, 0xfc00), f32::NEG_INFINITY);
    }

    #[test]
    fn decimal_ties() {
        // 1 + 2^-11 is the tie between 1 and the next f16.
        assert_eq!(parse(F16, "1.00048828125"), Some(0x3c00));
        assert_eq!(parse(F16, "1.000488281250000000000000000001"), Some(0x3c01));
        assert_eq!(parse(F16, "1.000488281249999999999999999999"), Some(0x3c00));
        assert_eq!(
            parse(F16, "-1.000488281250000000000000000001"),
            Some(0xbc01)
        );
        assert_eq!(parse(F16, "0.1"), Some(0x2e66));
        assert_eq!(parse(F16, "65504"), Some(0x7bff));
        assert_eq!(parse(F16, "1e10"), Some(0x7c00));
        assert_eq!(parse(BF16, "1.00390625"), Some(0x3f80));
        assert_eq!(parse(BF16, "1.0039062500000000000000000001"), Some(0x3f81));
        assert_eq!(parse(F16, "x"), None);
    }

    #[test]
    fn shortest_digits() {
        assert_eq!(shortest(F16, parse(F16, "0.1").unwrap()), 0.1);
        assert_eq!(shortest(F16, 0x3c00), 1.0);
        assert_eq!(shortest(F16, 0x7bff), 65500.0);
        assert_eq!(shortest(BF16, parse(BF16, "2.71").unwrap()), 2.7);
    }
}
