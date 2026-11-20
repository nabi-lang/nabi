//! Three-prime NTT products and products mod B^n - 1.

#[allow(clippy::wildcard_imports)]
use crate::bignum::*;

// NTT multiplication. The `u32` digits of both operands are
// coefficients; their cyclic convolution is computed modulo three
// primes p < 2^30 with 2^23 | p - 1
// (Montgomery arithmetic, R = 2^32) and put back together by Garner's
// CRT. A coefficient of the product is below min(len) 2^64 <= 2^86 <
// p1 p2 p3 (2^88.2), so transform lengths up to 2^23 are exact. The
// butterflies keep values in [0, 2p) (Harvey's lazy reduction, which
// needs 4p < 2^32).

/// The primes.
pub(crate) const P1: u32 = 998_244_353;

pub(crate) const P2: u32 = 754_974_721;

pub(crate) const P3: u32 = 469_762_049;

/// The longest transform (`u32` coefficients).
pub(crate) const NTT_MAX: usize = 1 << 23;

pub(crate) const fn pow_mod(b: u64, e: u64, p: u64) -> u64 {
    let (mut b, mut e, mut r) = (b % p, e, 1u64);
    while e > 0 {
        if e & 1 == 1 {
            r = r * b % p;
        }
        b = b * b % p;
        e >>= 1;
    }
    r
}

/// Montgomery arithmetic modulo the prime `P` (R = 2^32).
pub(crate) struct Mont<const P: u32>;

impl<const P: u32> Mont<P> {
    /// -P^-1 mod 2^32 (Newton: x (2 - P x) doubles the correct bits).
    pub(crate) const PINV: u32 = {
        assert!(P < 1 << 30);
        let mut x = 1u32;
        let mut i = 0;
        while i < 5 {
            x = x.wrapping_mul(2u32.wrapping_sub(P.wrapping_mul(x)));
            i += 1;
        }
        x.wrapping_neg()
    };

    /// a b / R mod P, in [0, 2P) for `a b < 2^32 P` (`a < 4P, b < P`, or
    /// both below 2P).
    #[inline]
    pub(crate) const fn mul(a: u32, b: u32) -> u32 {
        let t = a as u64 * b as u64;
        // The low half of `t`, as 32-bit products (vectorizes).
        let m = a.wrapping_mul(b).wrapping_mul(Self::PINV);
        ((t + m as u64 * P as u64) >> 32) as u32
    }

    /// [0, 2k P) to [0, k P) (`k` = 1, 2, 4; `4 P < 2^32`): wraps unless
    /// `x >= k P`.
    #[inline]
    pub(crate) const fn fold<const K: u32>(x: u32) -> u32 {
        let y = x.wrapping_sub(K * P);
        if y < x { y } else { x }
    }

    /// `x R mod P`.
    pub(crate) const fn to_mont(x: u64) -> u32 {
        (((x % P as u64) << 32) % P as u64) as u32
    }

    /// Root tables for length `n`: `rt[h + j] = w_2h^j` for each half
    /// length `h` (Montgomery form, below P), `irt` the same for the
    /// inverses.
    pub(crate) fn roots(g: u32, n: usize, rt: &mut [u32], irt: &mut [u32]) {
        // The first `K` powers in sequence, then each from the one `K`
        // back: independent products instead of one chain of `h`.
        const K: usize = 16;
        let p = u64::from(P);
        let w = pow_mod(u64::from(g), (p - 1) / n as u64, p);
        let wi = pow_mod(w, p - 2, p);
        let h = n / 2;
        let (mut x, mut y) = (Self::to_mont(1), Self::to_mont(1));
        let (wm, wim) = (Self::to_mont(w), Self::to_mont(wi));
        for j in 0..h.min(K) {
            rt[h + j] = x;
            irt[h + j] = y;
            x = Self::fold::<1>(Self::mul(x, wm));
            y = Self::fold::<1>(Self::mul(y, wim));
        }
        for j in K..h {
            rt[h + j] = Self::fold::<1>(Self::mul(rt[h + j - K], x));
            irt[h + j] = Self::fold::<1>(Self::mul(irt[h + j - K], y));
        }
        let mut h = h / 2;
        while h >= 1 {
            for j in 0..h {
                rt[h + j] = rt[2 * h + 2 * j];
                irt[h + j] = irt[2 * h + 2 * j];
            }
            h /= 2;
        }
    }

    /// Forward transform (decimation in frequency, length >= 4): natural
    /// order in, bit-reversed out; values in [0, 2P) both ends.
    pub(crate) fn forward(a: &mut [u32], rt: &[u32]) {
        let mut h = a.len() / 2;
        while h >= 4 {
            let w = &rt[h..2 * h];
            for blk in a.chunks_exact_mut(2 * h) {
                let (x, y) = blk.split_at_mut(h);
                for ((u, v), &w) in x.iter_mut().zip(y.iter_mut()).zip(w) {
                    let (s, d) = (*u, *v);
                    *u = Self::fold::<2>(s + d);
                    *v = Self::mul(s + 2 * P - d, w);
                }
            }
            h /= 2;
        }
        // h = 2 and 1 together; the twiddles are 1 but for w_4.
        let w4 = rt[3];
        for p in a.chunks_exact_mut(4) {
            let (y0, y2) = (
                Self::fold::<2>(p[0] + p[2]),
                Self::fold::<2>(p[0] + 2 * P - p[2]),
            );
            let (y1, y3) = (
                Self::fold::<2>(p[1] + p[3]),
                Self::mul(p[1] + 2 * P - p[3], w4),
            );
            p[0] = Self::fold::<2>(y0 + y1);
            p[1] = Self::fold::<2>(y0 + 2 * P - y1);
            p[2] = Self::fold::<2>(y2 + y3);
            p[3] = Self::fold::<2>(y2 + 2 * P - y3);
        }
    }

    /// Inverse transform without the 1/n (decimation in time, length >=
    /// 4): bit-reversed in, natural order out; values in [0, 2P) both
    /// ends.
    pub(crate) fn inverse(a: &mut [u32], irt: &[u32]) {
        let n = a.len();
        // h = 1 and 2 together; the twiddles are 1 but for w_4^-1.
        let w4 = irt[3];
        for p in a.chunks_exact_mut(4) {
            let (y0, y1) = (
                Self::fold::<2>(p[0] + p[1]),
                Self::fold::<2>(p[0] + 2 * P - p[1]),
            );
            let (y2, y3) = (
                Self::fold::<2>(p[2] + p[3]),
                Self::fold::<2>(p[2] + 2 * P - p[3]),
            );
            let t = Self::mul(y3, w4);
            p[0] = Self::fold::<2>(y0 + y2);
            p[2] = Self::fold::<2>(y0 + 2 * P - y2);
            p[1] = Self::fold::<2>(y1 + t);
            p[3] = Self::fold::<2>(y1 + 2 * P - t);
        }
        let mut h = 4;
        while h < n {
            let w = &irt[h..2 * h];
            for blk in a.chunks_exact_mut(2 * h) {
                let (x, y) = blk.split_at_mut(h);
                for ((u, v), &w) in x.iter_mut().zip(y.iter_mut()).zip(w) {
                    let (s, d) = (*u, Self::mul(*v, w));
                    *u = Self::fold::<2>(s + d);
                    *v = Self::fold::<2>(s + 2 * P - d);
                }
            }
            h *= 2;
        }
    }

    /// `f = d mod P`, zero-extended.
    pub(crate) fn load(f: &mut [u32], d: &[u32]) {
        for (x, &v) in f.iter_mut().zip(d) {
            *x = v % P;
        }
        f[d.len()..].fill(0);
    }

    /// `f = f g` pointwise (`g` empty: `f^2`).
    pub(crate) fn pointwise(f: &mut [u32], g: &[u32]) {
        if g.is_empty() {
            for x in f.iter_mut() {
                *x = Self::mul(*x, *x);
            }
        } else {
            for (x, &y) in f.iter_mut().zip(g) {
                *x = Self::mul(*x, y);
            }
        }
    }

    /// The factor that undoes the 1/R of the pointwise products and the
    /// factor `n` of the transforms (applied in the CRT pass).
    pub(crate) const fn scale(n: usize) -> u32 {
        let p = P as u64;
        let r2 = (1u64 << 32) % p;
        (r2 * r2 % p * pow_mod(n as u64, p - 2, p) % p) as u32
    }
}

/// Primitive roots of P1, P2, P3.
pub(crate) const G: [u32; 3] = [3, 11, 3];

/// The `u32` limbs of `res` (three residue vectors of `n`, in [0, 2p),
/// each times its `s`) put back together (Garner: x = x1 + p1 t2 + p1
/// p2 t3) and carried into `out`; the carry out of its top. `signed`:
/// coefficients past p1 p2 p3 / 2 are negative, `out` is then two's
/// complement and the carry its sign. `res` is overwritten.
pub(crate) fn garner(
    out: &mut [u32],
    res: &mut [u32],
    n: usize,
    s: [u32; 3],
    signed: bool,
) -> i128 {
    let (r1, rest) = res.split_at_mut(n);
    let (r2, r3) = rest.split_at_mut(n);
    let (p1, p2, p3) = (u64::from(P1), u64::from(P2), u64::from(P3));
    let i12 = const { pow_mod(P1 as u64, P2 as u64 - 2, P2 as u64) };
    let i123 = const { pow_mod(P1 as u64 * P2 as u64 % P3 as u64, P3 as u64 - 2, P3 as u64) };
    // t2 = (x2 - x1) i12, t3 = (x3 - x1 - p1 t2) i123 as Montgomery
    // products, the scales folded into the constants.
    let c2a = (u64::from(s[1]) * i12 % p2) as u32;
    let c2b = Mont::<P2>::to_mont(i12);
    let c3a = (u64::from(s[2]) * i123 % p3) as u32;
    let c3b = Mont::<P3>::to_mont(i123);
    let c3c = Mont::<P3>::to_mont(p1 % p3 * i123 % p3);
    let len = out.len();
    for ((a, b), c) in r1[..len].iter_mut().zip(&mut r2[..len]).zip(&mut r3[..len]) {
        let x1 = Mont::<P1>::fold::<1>(Mont::<P1>::mul(*a, s[0]));
        let u2 = Mont::<P2>::mul(*b, c2a) + 2 * P2 - Mont::<P2>::mul(x1, c2b);
        let t2 = Mont::<P2>::fold::<1>(Mont::<P2>::fold::<2>(u2));
        let u3 =
            Mont::<P3>::mul(*c, c3a) + 4 * P3 - Mont::<P3>::mul(x1, c3b) - Mont::<P3>::mul(t2, c3c);
        let t3 = Mont::<P3>::fold::<1>(Mont::<P3>::fold::<2>(Mont::<P3>::fold::<4>(u3)));
        (*a, *b, *c) = (x1, t2, t3);
    }
    let pp = u128::from(p1 * p2) * u128::from(p3);
    let mut carry = 0i128;
    for (i, o) in out.iter_mut().enumerate() {
        let x12 = u64::from(r1[i]) + p1 * u64::from(r2[i]);
        let x = x12 as u128 + (p1 * p2) as u128 * u128::from(r3[i]);
        let x = if signed && x > pp / 2 {
            x as i128 - pp as i128
        } else {
            x as i128
        };
        let v = carry + x;
        *o = v as u32;
        carry = v >> 32;
    }
    carry
}

/// `r += c` mod `B^r.len() - 1` (`B = 2^32`; a carry off the top comes
/// back in at the bottom), with `B^r.len() - 1` itself made 0.
pub(crate) fn wrap_in(r: &mut [u32], mut c: u128) {
    while c != 0 {
        for x in r.iter_mut() {
            let v = u128::from(*x) + c;
            *x = v as u32;
            c = v >> 32;
            if c == 0 {
                break;
            }
        }
    }
    if r.iter().all(|&x| x == u32::MAX) {
        r.fill(0);
    }
}

/// Root tables for transforms up to length `n`: per prime `rt` then
/// `irt`, `n` each. `rt[h + j]` depends on `h` only, so a table serves
/// every shorter transform.
pub(crate) fn ntt_roots(w: &mut [u32], n: usize) {
    let (w1, rest) = w.split_at_mut(2 * n);
    let (w2, w3) = rest.split_at_mut(2 * n);
    let (a, b) = w1.split_at_mut(n);
    Mont::<P1>::roots(G[0], n, a, b);
    let (a, b) = w2.split_at_mut(n);
    Mont::<P2>::roots(G[1], n, a, b);
    let (a, b) = w3[..2 * n].split_at_mut(n);
    Mont::<P3>::roots(G[2], n, a, b);
}

/// The forward transforms of `d` into `f` (`n` per prime), roots `w`
/// (`ntt_roots` for at least `n`, of `wn`).
pub(crate) fn ntt_forward(f: &mut [u32], n: usize, d: &[u32], w: &[u32], wn: usize) {
    let (f1, rest) = f.split_at_mut(n);
    let (f2, f3) = rest.split_at_mut(n);
    Mont::<P1>::load(f1, d);
    Mont::<P1>::forward(f1, &w[..wn]);
    Mont::<P2>::load(f2, d);
    Mont::<P2>::forward(f2, &w[2 * wn..3 * wn]);
    Mont::<P3>::load(&mut f3[..n], d);
    Mont::<P3>::forward(&mut f3[..n], &w[4 * wn..5 * wn]);
}

/// `res = ntt_forward(d) * g` pointwise, inverted (`g` the transforms of
/// the other operand, empty to square `d`); the CRT factors.
pub(crate) fn ntt_finish(res: &mut [u32], n: usize, g: &[u32], w: &[u32], wn: usize) -> [u32; 3] {
    let (f1, rest) = res.split_at_mut(n);
    let (f2, f3) = rest.split_at_mut(n);
    let f3 = &mut f3[..n];
    let gi = |i: usize| {
        if g.is_empty() {
            g
        } else {
            &g[i * n..(i + 1) * n]
        }
    };
    Mont::<P1>::pointwise(f1, gi(0));
    Mont::<P1>::inverse(f1, &w[wn..2 * wn]);
    Mont::<P2>::pointwise(f2, gi(1));
    Mont::<P2>::inverse(f2, &w[3 * wn..4 * wn]);
    Mont::<P3>::pointwise(f3, gi(2));
    Mont::<P3>::inverse(f3, &w[5 * wn..6 * wn]);
    [
        Mont::<P1>::scale(n),
        Mont::<P2>::scale(n),
        Mont::<P3>::scale(n),
    ]
}

/// The `u32` digits of `a`.
pub(crate) const fn digits32(a: &[L]) -> &[u32] {
    // SAFETY: `L` is `PER` little-endian `u32`s (see `pack`).
    unsafe { slice::from_raw_parts(a.as_ptr().cast::<u32>(), a.len() * PER) }
}

/// Transform length (`u32` coefficients) for an `n` x `m` product.
pub(crate) const fn ntt_len(n: usize, m: usize) -> usize {
    ((n + m) * PER).next_power_of_two()
}

/// Scratch `L`s `ntt_mul` uses.
pub(crate) const fn ntt_scratch(n: usize, m: usize) -> usize {
    6 * ntt_len(n, m) / PER
}

/// `f` = the cyclic convolution of `da` and `db` (`db` empty: `da` with
/// itself) mod P, in [0, 2P); `fb`, `rt`, `irt` scratch of `f.len()`.
pub(crate) fn convolve<const P: u32>(
    g: u32,
    f: &mut [u32],
    fb: &mut [u32],
    rt: &mut [u32],
    irt: &mut [u32],
    da: &[u32],
    db: &[u32],
) {
    Mont::<P>::roots(g, f.len(), rt, irt);
    Mont::<P>::load(f, da);
    Mont::<P>::forward(f, rt);
    if db.is_empty() {
        Mont::<P>::pointwise(f, &[]);
    } else {
        Mont::<P>::load(fb, db);
        Mont::<P>::forward(fb, rt);
        Mont::<P>::pointwise(f, fb);
    }
    Mont::<P>::inverse(f, irt);
}

/// A product sum `x[a] x[b] + x[c] x[d]` (`-` when `neg`).
pub(crate) type Term = (usize, usize, usize, usize, bool);

/// `out[k] = terms[k]` over operands `x` (`u32` digits): each operand
/// transformed once per prime (length `n`, at least the longest
/// result), the sums taken pointwise; the signs of the results (a
/// negative one is left as two's complement). `t` holds `(x.len() + 3
/// terms.len() + 2) n` `u32`s.
pub(crate) fn ntt_dot(
    out: &mut [&mut [u32]],
    x: &[&[u32]],
    terms: &[Term],
    n: usize,
    t: &mut [u32],
) -> [bool; 4] {
    let (f, t) = t.split_at_mut(x.len() * n);
    let (res, t) = t.split_at_mut(3 * terms.len() * n);
    let (rt, irt) = t[..2 * n].split_at_mut(n);
    dot_prime::<P1>(G[0], f, res, 0, rt, irt, x, terms, n);
    dot_prime::<P2>(G[1], f, res, 1, rt, irt, x, terms, n);
    dot_prime::<P3>(G[2], f, res, 2, rt, irt, x, terms, n);
    let s = [
        Mont::<P1>::scale(n),
        Mont::<P2>::scale(n),
        Mont::<P3>::scale(n),
    ];
    let mut neg = [false; 4];
    for (k, (o, r)) in out.iter_mut().zip(res.chunks_exact_mut(3 * n)).enumerate() {
        let c = garner(o, r, n, s, terms[k].4);
        debug_assert!(c == 0 || c == -1);
        neg[k] = c < 0;
    }
    neg
}

/// `ntt_dot` modulo `P`: residues `which` of each term in `res`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn dot_prime<const P: u32>(
    g: u32,
    f: &mut [u32],
    res: &mut [u32],
    which: usize,
    rt: &mut [u32],
    irt: &mut [u32],
    x: &[&[u32]],
    terms: &[Term],
    n: usize,
) {
    Mont::<P>::roots(g, n, rt, irt);
    for (fi, xi) in f.chunks_exact_mut(n).zip(x) {
        Mont::<P>::load(fi, xi);
        Mont::<P>::forward(fi, rt);
    }
    for (k, &(a, b, c, d, minus)) in terms.iter().enumerate() {
        let o = &mut res[(3 * k + which) * n..(3 * k + which + 1) * n];
        let fi = |i: usize| &f[i * n..(i + 1) * n];
        let (fa, fb, fc, fd) = (fi(a), fi(b), fi(c), fi(d));
        for ((v, (&xa, &xb)), (&xc, &xd)) in
            o.iter_mut().zip(fa.iter().zip(fb)).zip(fc.iter().zip(fd))
        {
            let u = Mont::<P>::mul(xa, xb);
            let w = Mont::<P>::mul(xc, xd);
            *v = Mont::<P>::fold::<2>(if minus { u + 2 * P - w } else { u + w });
        }
        Mont::<P>::inverse(o, irt);
    }
}

/// The transform length `ntt_dot` takes for results of `total` limbs
/// from operands of at least `lo`, or none.
pub(crate) const fn dot_len(lo: usize, total: usize) -> Option<usize> {
    let n = (total * PER).next_power_of_two();
    if lo >= NTT / 2 && n <= NTT_MAX {
        Some(n)
    } else {
        None
    }
}

/// `L`s as `u32` digits, mutable.
pub(crate) fn digits32_mut(a: &mut [L]) -> &mut [u32] {
    // SAFETY: as in `digits32`.
    unsafe { slice::from_raw_parts_mut(a.as_mut_ptr().cast::<u32>(), a.len() * PER) }
}

/// `r = a * b` by the NTT (`b` empty squares `a`), scratch `t`: one
/// prime at a time.
pub(crate) fn ntt_mul(r: &mut [L], a: &[L], b: &[L], t: &mut [L]) {
    let n = ntt_len(a.len(), if b.is_empty() { a.len() } else { b.len() });
    let (da, db) = (digits32(a), digits32(b));
    // SAFETY: `t` is `L`s; any bit pattern is a `u32`.
    let t = unsafe { slice::from_raw_parts_mut(t.as_mut_ptr().cast::<u32>(), t.len() * PER) };
    let (res, t) = t.split_at_mut(3 * n);
    let (fb, t) = t.split_at_mut(n);
    let (rt, t) = t.split_at_mut(n);
    let irt = &mut t[..n];
    let (r1, rest) = res.split_at_mut(n);
    let (r2, r3) = rest.split_at_mut(n);
    convolve::<P1>(G[0], r1, fb, rt, irt, da, db);
    convolve::<P2>(G[1], r2, fb, rt, irt, da, db);
    convolve::<P3>(G[2], r3, fb, rt, irt, da, db);
    // SAFETY: as `t` above.
    let out = unsafe { slice::from_raw_parts_mut(r.as_mut_ptr().cast::<u32>(), r.len() * PER) };
    let s = [
        Mont::<P1>::scale(n),
        Mont::<P2>::scale(n),
        Mont::<P3>::scale(n),
    ];
    let c = garner(out, res, n, s, false);
    debug_assert!(c == 0);
}

/// The transform length `nabi_nat_mul` takes for an `an` x `bn` product
/// (`u32` limbs), 0 when it does not use the NTT.
#[unsafe(no_mangle)]
pub const extern "C" fn nabi_nat_ntt_len(an: usize, bn: usize) -> usize {
    let (na, nb) = (packed(an), packed(bn));
    let lo = if na < nb { na } else { nb };
    let n = ntt_len(na, nb);
    if lo >= NTT && n <= NTT_MAX { n } else { 0 }
}

/// `w[0..6 n]` = root tables for transforms up to length `n` (a power of
/// two, at most what `nabi_nat_ntt_len` returns).
///
/// # Safety
/// A live `GcBuf<u32>` of at least `6 n` elements.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nabi_nat_ntt_roots(w: *mut u8, n: usize) {
    assert!(n.is_power_of_two() && n <= NTT_MAX);
    // SAFETY: caller contract.
    let w = unsafe { limbs(w, 6 * n) };
    parked(n, || ntt_roots(w, n));
}

/// `f[0..3 n]` = the transforms of `b[0..bn]` for products of up to `n`
/// limbs, roots `w` from `nabi_nat_ntt_roots(w, wn)`, `wn >= n`.
///
/// # Safety
/// Live `GcBuf<u32>`s of at least those lengths; `f` distinct from the
/// others.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nabi_nat_ntt_prep(
    f: *mut u8,
    b: *mut u8,
    bn: usize,
    n: usize,
    w: *mut u8,
    wn: usize,
) {
    assert!(n.is_power_of_two() && bn <= n && n <= wn);
    // SAFETY: caller contract; `b` and `w` only read.
    let (f, b, w) = unsafe { (limbs(f, 3 * n), &*limbs(b, bn), &*limbs(w, 6 * wn)) };
    parked(n, || ntt_forward(f, n, b, w, wn));
}

/// `r = a * b mod (B^n - 1)` in `min(an + bn, n)` limbs (`B = 2^32`;
/// the exact product when `an + bn <= n`) for `b` prepared in `f`
/// (`nabi_nat_ntt_prep`, length `n >= an, bn`, roots `w` of `wn`); `t`
/// holds `3 n` limbs.
///
/// # Safety
/// Live `GcBuf<u32>`s of at least those lengths; `r` and `t` distinct
/// from each other and from the rest.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nabi_nat_ntt_mul(
    r: *mut u8,
    a: *mut u8,
    an: usize,
    f: *mut u8,
    bn: usize,
    n: usize,
    w: *mut u8,
    wn: usize,
    t: *mut u8,
) {
    assert!(an <= n && bn <= n && n <= wn);
    let wrap = an + bn > n;
    // SAFETY: caller contract; `a`, `f` and `w` only read.
    let (r, a, f, w, t) = unsafe {
        (
            limbs(r, if wrap { n } else { an + bn }),
            &*limbs(a, an),
            &*limbs(f, 3 * n),
            &*limbs(w, 6 * wn),
            limbs(t, 3 * n),
        )
    };
    parked(n, || {
        ntt_forward(t, n, a, w, wn);
        let s = ntt_finish(t, n, f, w, wn);
        let c = garner(r, t, n, s, false);
        if wrap {
            wrap_in(r, c as u128);
        }
        debug_assert!(wrap || c == 0);
    });
}

/// Whether `nabi_nat_mulmod` takes an `an` x `bn` product mod `B^m - 1`
/// as one cyclic transform of length `m`.
pub(crate) const fn wrap_ntt(an: usize, bn: usize, m: usize) -> bool {
    let lo = if an < bn { an } else { bn };
    m.is_power_of_two() && m <= NTT_MAX && an <= m && bn <= m && packed(lo) >= NTT / 2
}

/// The modulus length `m >= n` (`B^m - 1`) `nabi_nat_mulmod` serves best
/// for an `an` x `bn` product: a transform length, or `n` itself.
#[unsafe(no_mangle)]
pub const extern "C" fn nabi_nat_mulmod_len(an: usize, bn: usize, n: usize) -> usize {
    let m = n.next_power_of_two();
    if wrap_ntt(an, bn, m) { m } else { n }
}

/// `u32` limbs of scratch `nabi_nat_mulmod` needs.
#[unsafe(no_mangle)]
pub const extern "C" fn nabi_nat_mulmod_scratch(an: usize, bn: usize, m: usize) -> usize {
    if wrap_ntt(an, bn, m) {
        6 * m
    } else {
        packed(an + bn) * PER + nabi_nat_mul_scratch(an, bn)
    }
}

/// `r[0..m] = a * b mod (B^m - 1)` (`B = 2^32`): a cyclic convolution
/// when `m` is from `nabi_nat_mulmod_len`, else the product folded; `t`
/// holds `nabi_nat_mulmod_scratch(an, bn, m)` limbs. `a` and `b` may be
/// the same buffer (`an == bn` squares).
///
/// # Safety
/// Live `GcBuf<u32>`s of at least those lengths; `r` and `t` distinct
/// from each other and from `a` / `b`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nabi_nat_mulmod(
    r: *mut u8,
    a: *mut u8,
    an: usize,
    b: *mut u8,
    bn: usize,
    m: usize,
    t: *mut u8,
) {
    let square = a == b && an == bn;
    let tn = nabi_nat_mulmod_scratch(an, bn, m);
    // SAFETY: caller contract; `a` / `b` only read; `t` is 8-aligned.
    let (a, b, r, t) = unsafe { (&*limbs(a, an), &*limbs(b, bn), limbs(r, m), limbs(t, tn)) };
    parked(an + bn, || {
        if wrap_ntt(an, bn, m) {
            let (res, rest) = t.split_at_mut(3 * m);
            let (fb, rest) = rest.split_at_mut(m);
            let (rt, irt) = rest.split_at_mut(m);
            let db = if square { &[][..] } else { b };
            let (r1, rest) = res.split_at_mut(m);
            let (r2, r3) = rest.split_at_mut(m);
            convolve::<P1>(G[0], r1, fb, rt, irt, a, db);
            convolve::<P2>(G[1], r2, fb, rt, irt, a, db);
            convolve::<P3>(G[2], r3, fb, rt, irt, a, db);
            let s = [
                Mont::<P1>::scale(m),
                Mont::<P2>::scale(m),
                Mont::<P3>::scale(m),
            ];
            let c = garner(r, res, m, s, false);
            wrap_in(r, c as u128);
        } else {
            let (p, t) = t.split_at_mut(packed(an + bn) * PER);
            let p = &mut p[..an + bn];
            // SAFETY: `t` starts on an `L` boundary.
            let t = unsafe { slice::from_raw_parts_mut(t.as_mut_ptr().cast::<L>(), t.len() / PER) };
            nat_mul(p, a, b, square, t);
            r.fill(0);
            for chunk in p.chunks(m) {
                let mut c = 0u64;
                for (x, &v) in r.iter_mut().zip(chunk) {
                    let s = u64::from(*x) + u64::from(v) + c;
                    *x = s as u32;
                    c = s >> 32;
                }
                for x in &mut r[chunk.len()..] {
                    if c == 0 {
                        break;
                    }
                    let s = u64::from(*x) + c;
                    *x = s as u32;
                    c = s >> 32;
                }
                wrap_in(r, u128::from(c));
            }
            wrap_in(r, 0);
        }
    });
}
