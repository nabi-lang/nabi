//! The computations behind the runtime's pure `str` entries, on plain
//! bytes.
//!
//! The one implementation: the runtime's `str.rs` wraps
//! them for compiled code and the compiler's constant evaluator
//! (`nabic_consteval`) calls them directly. Uses `core` only.

/// Why `b[start..end]` is not a `str` slice; `Display` is the panic
/// message.
pub enum SliceError<'a> {
    Range,
    /// Byte `at` falls inside `ch`, which starts at byte `from`.
    Boundary {
        at: usize,
        from: usize,
        ch: &'a [u8],
    },
}

impl core::fmt::Display for SliceError<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Range => f.write_str("string slice out of range"),
            Self::Boundary { at, from, ch } => write!(
                f,
                "string slice is not on a char boundary: byte {at} is inside `{}` \
                 (bytes {from}..{})\nstring offsets count bytes; to work by character, \
                 use `chars()`, `char_indices()` or `char_slice(a, b)`\nto cut at a \
                 boundary, use `floor_char_boundary(i)`; `get(a..b)` gives `none` \
                 instead of a panic",
                core::str::from_utf8(ch).unwrap_or("?"),
                from + ch.len()
            ),
        }
    }
}

/// Whether `b[start..end]` is a `str` slice: in range, on char
/// boundaries.
#[inline]
pub fn slice_check(b: &[u8], start: usize, end: usize) -> Result<(), SliceError<'_>> {
    if start > end || end > b.len() {
        return Err(SliceError::Range);
    }
    let inside = |i: usize| i < b.len() && (b[i] & 0xC0) == 0x80;
    let Some(at) = [start, end].into_iter().find(|&i| inside(i)) else {
        return Ok(());
    };
    let from = (0..at).rev().find(|&i| !inside(i)).unwrap_or(0);
    let len = match b[from] {
        0xF0.. => 4,
        0xE0.. => 3,
        _ => 2,
    };
    let ch = &b[from..b.len().min(from + len)];
    Err(SliceError::Boundary { at, from, ch })
}

/// Byte offset of the first `n` in `h`.
#[inline]
pub fn find(h: &[u8], n: &[u8]) -> Option<usize> {
    if let [c] = n {
        return memchr(h, 0, *c).filter(|&i| i < h.len());
    }
    if n.is_empty() {
        return Some(0);
    }
    two_way(&Fwd(h), &Fwd(n))
}

/// Byte offset of the last `n` in `h`.
#[inline]
pub fn rfind(h: &[u8], n: &[u8]) -> Option<usize> {
    if n.is_empty() {
        return Some(h.len());
    }
    if n.len() > h.len() {
        return None;
    }
    // The first match in the reversed bytes is the last one.
    two_way(&Rev(h), &Rev(n)).map(|i| h.len() - n.len() - i)
}

/// Bytes as the two-way search reads them: forward or backward.
trait Bytes {
    fn len(&self) -> usize;
    fn at(&self, i: usize) -> u8;
    /// The first `i >= from` with `at(i) == c`, or `len()`.
    fn skip(&self, from: usize, c: u8) -> usize;
}

struct Fwd<'a>(&'a [u8]);
struct Rev<'a>(&'a [u8]);

impl Bytes for Fwd<'_> {
    #[inline]
    fn len(&self) -> usize {
        self.0.len()
    }
    #[inline]
    fn at(&self, i: usize) -> u8 {
        self.0[i]
    }
    #[inline]
    fn skip(&self, from: usize, c: u8) -> usize {
        memchr(self.0, from, c).unwrap_or(self.0.len())
    }
}

impl Bytes for Rev<'_> {
    #[inline]
    fn len(&self) -> usize {
        self.0.len()
    }
    #[inline]
    fn at(&self, i: usize) -> u8 {
        self.0[self.0.len() - 1 - i]
    }
    #[inline]
    fn skip(&self, from: usize, c: u8) -> usize {
        let n = self.0.len();
        if from >= n {
            return n;
        }
        self.0[..n - from]
            .iter()
            .rposition(|&b| b == c)
            .map_or(n, |i| n - 1 - i)
    }
}

const LOW: u64 = 0x0101_0101_0101_0101;
const HIGH: u64 = 0x8080_8080_8080_8080;

/// The first `i >= from` with `h[i] == c`, eight bytes a step.
#[allow(clippy::many_single_char_names)] // the algorithm's names
fn memchr(h: &[u8], from: usize, c: u8) -> Option<usize> {
    let mut i = from;
    let rep = LOW * u64::from(c);
    while i + 8 <= h.len() {
        let w = u64::from_le_bytes([
            h[i],
            h[i + 1],
            h[i + 2],
            h[i + 3],
            h[i + 4],
            h[i + 5],
            h[i + 6],
            h[i + 7],
        ]) ^ rep;
        let z = w.wrapping_sub(LOW) & !w & HIGH;
        if z != 0 {
            return Some(i + (z.trailing_zeros() / 8) as usize);
        }
        i += 8;
    }
    h.get(i..)?.iter().position(|&b| b == c).map(|k| i + k)
}

/// The start and period of the maximal suffix of `n` (by `<`, or by `>`
/// if `rev`).
#[allow(clippy::many_single_char_names)] // the algorithm's names
fn max_suffix(n: &impl Bytes, rev: bool) -> (usize, usize) {
    let (mut ms, mut j, mut k, mut p) = (usize::MAX, 0usize, 1usize, 1usize);
    while j + k < n.len() {
        let (a, b) = (n.at(j + k), n.at(ms.wrapping_add(k)));
        let (less, more) = if rev { (b < a, b > a) } else { (a < b, a > b) };
        if less {
            j += k;
            k = 1;
            p = j.wrapping_sub(ms);
        } else if more {
            ms = j;
            j += 1;
            k = 1;
            p = 1;
        } else if k == p {
            j += p;
            k = 1;
        } else {
            k += 1;
        }
    }
    (ms.wrapping_add(1), p)
}

/// Two-way string matching (Crochemore-Perrin): the first `n` in `h` in
/// time linear in both, no allocation. `n` is at least two bytes.
#[allow(clippy::many_single_char_names)] // the algorithm's names
fn two_way(h: &impl Bytes, n: &impl Bytes) -> Option<usize> {
    let m = n.len();
    if m > h.len() {
        return None;
    }
    let (s0, p0) = max_suffix(n, false);
    let (s1, p1) = max_suffix(n, true);
    let (crit, period) = if s0 >= s1 { (s0, p0) } else { (s1, p1) };
    let periodic = period + crit <= m && (0..crit).all(|i| n.at(i) == n.at(i + period));
    let (shift, mut mem) = if periodic {
        (period, 0)
    } else {
        (crit.max(m - crit) + 1, 0)
    };
    let first = n.at(crit);
    let mut pos = 0;
    while pos + m <= h.len() {
        let mut i = crit.max(mem);
        if i == crit {
            // Most windows fail on the critical byte: jump to its next
            // occurrence.
            let at = h.skip(pos + crit, first);
            if at + (m - crit) > h.len() {
                return None;
            }
            if at > pos + crit {
                pos = at - crit;
                mem = 0;
            }
        }
        while i < m && n.at(i) == h.at(pos + i) {
            i += 1;
        }
        if i < m {
            pos += i - crit + 1;
            mem = 0;
            continue;
        }
        let mut j = crit;
        while j > mem && n.at(j - 1) == h.at(pos + j - 1) {
            j -= 1;
        }
        if j <= mem {
            return Some(pos);
        }
        pos += shift;
        mem = if periodic { m - period } else { 0 };
    }
    None
}

/// [`find`] among the matches that start before `near`: the offset, -1
/// when `h` has none at all, -2 when none starts before `near` but later
/// ones may ([`find_far`] looks for them).
pub fn find_near(h: &[u8], n: &[u8], near: usize) -> isize {
    let end = near.saturating_add(n.len().saturating_sub(1));
    if end >= h.len() {
        return find(h, n).map_or(-1, |i| i as isize);
    }
    find(&h[..end], n).map_or(-2, |i| i as isize)
}

/// [`find`] among the matches that start at `near` or later.
pub fn find_far(h: &[u8], n: &[u8], near: usize) -> isize {
    h.get(near..)
        .and_then(|t| find(t, n))
        .map_or(-1, |i| (near + i) as isize)
}

/// [`rfind`] among the matches that end in the last `near` bytes: the
/// offset, -1 when `h` has none at all, -2 when none ends there but
/// earlier ones may ([`rfind_far`] looks for them).
pub fn rfind_near(h: &[u8], n: &[u8], near: usize) -> isize {
    let Some(start) = h
        .len()
        .checked_sub(near.saturating_add(n.len().saturating_sub(1)))
        .filter(|&s| s > 0)
    else {
        return rfind(h, n).map_or(-1, |i| i as isize);
    };
    rfind(&h[start..], n).map_or(-2, |i| (start + i) as isize)
}

/// [`rfind`] among the matches that end before the last `near` bytes.
pub fn rfind_far(h: &[u8], n: &[u8], near: usize) -> isize {
    h.get(..h.len().saturating_sub(near))
        .and_then(|t| rfind(t, n))
        .map_or(-1, |i| i as isize)
}

/// Whether `h[at..at + n.len()]` is `n`.
#[inline]
pub fn eq_at(h: &[u8], at: usize, n: &[u8]) -> bool {
    at.checked_add(n.len()).and_then(|end| h.get(at..end)) == Some(n)
}

/// `h` with every non-overlapping `f` replaced by `t`, piece by piece
/// into `put`. An empty `f` matches at every char boundary: "ab" ->
/// "-a-b-".
pub fn replace(h: &[u8], f: &[u8], t: &[u8], mut put: impl FnMut(&[u8])) {
    if f.is_empty() {
        put(t);
        for (i, b) in h.iter().enumerate() {
            put(core::slice::from_ref(b));
            if h.get(i + 1).is_none_or(|n| (n & 0xC0) != 0x80) {
                put(t);
            }
        }
        return;
    }
    let (mut i, mut run) = (0, 0);
    while i + f.len() <= h.len() {
        if &h[i..i + f.len()] == f {
            put(&h[run..i]);
            put(t);
            i += f.len();
            run = i;
        } else {
            i += 1;
        }
    }
    put(&h[run..]);
}

/// `s` as an integer of `bits` bits in base `radix` (2..=36), `signed`
/// or not.
///
/// An optional sign, then digits of either case. The one integer
/// grammar (`FromStr` and `Int::parse_radix`). The value's bits, or the
/// status: 1 empty, 2 invalid digit, 3 out of range.
#[inline]
pub fn parse_int(s: &[u8], radix: u32, signed: bool, bits: u32) -> Result<u64, i32> {
    let (neg, digits) = match s {
        [b'-', rest @ ..] => (true, rest),
        [b'+', rest @ ..] => (false, rest),
        all => (false, all),
    };
    if digits.is_empty() {
        return Err(1);
    }
    if !(2..=36).contains(&radix) || !digits.iter().all(|&c| char::from(c).is_digit(radix)) {
        return Err(2);
    }
    let mag = digits.iter().try_fold(0u64, |m, &c| {
        let d = char::from(c).to_digit(radix).unwrap_or(0);
        m.checked_mul(radix.into())?.checked_add(d.into())
    });
    let bits = bits.clamp(1, 64);
    let limit = match (signed, neg) {
        (true, true) => 1u64 << (bits - 1),
        (true, false) => (1u64 << (bits - 1)) - 1,
        (false, false) => u64::MAX >> (64 - bits),
        (false, true) => 0,
    };
    let Some(mag) = mag.filter(|&m| (signed || !neg) && m <= limit) else {
        return Err(3);
    };
    Ok(if neg { mag.wrapping_neg() } else { mag })
}

/// `t` as an `f64` (`inf`, `NaN` and exponents included), or the status:
/// 2 invalid, 3 out of range.
#[inline]
pub fn parse_float(t: &str) -> Result<f64, i32> {
    let Ok(v) = t.parse::<f64>() else {
        return Err(2);
    };
    // Digits that round past f64's range (`inf` itself has none).
    if v.is_infinite() && t.bytes().any(|b| b.is_ascii_digit()) {
        return Err(3);
    }
    Ok(v)
}

/// `t` as the bits of a narrower float, rounded once from the decimal:
/// `kind` 0 an `f32`, 1 an `f16`, 2 a `bf16`. Errors as `parse_float`.
#[inline]
pub fn parse_narrow(t: &str, kind: u32) -> Result<u32, i32> {
    let (bits, inf) = if kind == 0 {
        let v = t.parse::<f32>().map_err(|_| 2)?;
        (v.to_bits(), v.is_infinite())
    } else {
        let f = if kind == 1 {
            crate::half::F16
        } else {
            crate::half::BF16
        };
        let h = crate::half::parse(f, t).ok_or(2)?;
        (u32::from(h), crate::half::to_f32(f, h).is_infinite())
    };
    if inf && t.bytes().any(|b| b.is_ascii_digit()) {
        return Err(3);
    }
    Ok(bits)
}

/// Offset of the first byte of `b` that is not UTF-8, or -1.
#[inline]
pub fn utf8_error(b: &[u8]) -> isize {
    core::str::from_utf8(b).map_or_else(|e| e.valid_up_to() as isize, |_| -1)
}
