//! Lehmer and half-gcd.

#[allow(clippy::wildcard_imports)]
use crate::bignum::*;

/// gcd of two words (binary).
pub(crate) const fn gcd_word(mut a: L, mut b: L) -> L {
    if a == 0 {
        return b;
    }
    if b == 0 {
        return a;
    }
    let k = (a | b).trailing_zeros();
    a >>= a.trailing_zeros();
    loop {
        b >>= b.trailing_zeros();
        if a > b {
            (a, b) = (b, a);
        }
        b -= a;
        if b == 0 {
            return a << k;
        }
    }
}

/// Lehmer cofactors stay below 2^LEHMER in magnitude (see
/// `lehmer_apply`).
pub(crate) const LEHMER: u32 = LB - 2;

/// Knuth TAOCP 4.5.2 algorithm L on the top `2 LB - 2` bits of `x >= y`
/// (`n >= 2` limbs, `x[n - 1] != 0`; `y` at the same shift): cofactors
/// `(a, b, c, d)` such that `a x + b y` and `c x + d y` are two
/// consecutive remainders of Euclid's sequence; `b == 0` when not even
/// the first quotient is certain. `a`, `b` (and `c`, `d`) have opposite
/// signs. Two limbs of precision give quotients until the cofactors
/// reach about one limb, where the bound stops them. With `s > 0` no
/// remainder may fall below B^s: a remainder `r` of window value `w`
/// (the window `2^sh` apart from `x`) has `|r - w 2^sh| < 2^LEHMER
/// 2^sh` (its cofactors' bound), so `w` must reach `2^LEHMER + B^s /
/// 2^sh`.
pub(crate) fn lehmer(x: &[L], y: &[L], s: usize) -> (SL, SL, SL, SL) {
    const CAP: SW = 1 << LEHMER;
    let n = x.len();
    let z = x[n - 1].leading_zeros();
    let floor: SW = if s == 0 {
        0
    } else {
        let sh = (n as i64) * i64::from(LB) - i64::from(z) - 2 * i64::from(LB) + 2;
        let e = (s as i64) * i64::from(LB) - sh;
        if e >= 2 * i64::from(LB) - 2 {
            return (1, 0, 0, 1);
        }
        CAP + if e <= 0 { 1 } else { 1 << e }
    };
    let top = |v: &[L]| {
        let hi = (((v[n - 1] as W) << LB) | v[n - 2] as W) << z;
        let lo = if n > 2 && z > 0 {
            (v[n - 3] >> (LB - z)) as W
        } else {
            0
        };
        ((hi | lo) >> 2) as SW
    };
    let (mut xh, mut yh) = (top(x), top(y));
    let (mut a, mut b, mut c, mut d): (SW, SW, SW, SW) = (1, 0, 0, 1);
    while yh + c != 0 && yh + d != 0 {
        let q = (xh + a) / (yh + c);
        if q != (xh + b) / (yh + d) {
            break;
        }
        let next = |u: SW, v: SW| {
            q.checked_mul(v)
                .and_then(|p| u.checked_sub(p))
                .filter(|t| t.abs() < CAP)
        };
        let (Some(nc), Some(nd)) = (next(a, c), next(b, d)) else {
            break;
        };
        let r = xh - q * yh;
        if r < floor {
            break;
        }
        (a, c) = (c, nc);
        (b, d) = (d, nd);
        (xh, yh) = (yh, r);
    }
    (a as SL, b as SL, c as SL, d as SL)
}

/// `(x, y) = (a x + b y, c x + d y)` for Lehmer cofactors (both results
/// non-negative and at most `x`).
pub(crate) fn lehmer_apply(x: &mut [L], y: &mut [L], (a, b, c, d): (SL, SL, SL, SL)) {
    // |a u + b v| < 2^LEHMER 2^LB (opposite signs), so the sums fit `SW`.
    let (mut cx, mut cy): (SW, SW) = (0, 0);
    for (u, v) in x.iter_mut().zip(y.iter_mut()) {
        let (p, q) = (*u as SW, *v as SW);
        cx += a as SW * p + b as SW * q;
        cy += c as SW * p + d as SW * q;
        *u = cx as L;
        *v = cy as L;
        cx >>= LB;
        cy >>= LB;
    }
    debug_assert!(cx == 0 && cy == 0);
}

/// Limbs of `a` without leading zeros.
pub(crate) fn significant(a: &[L]) -> usize {
    a.iter().rposition(|&v| v != 0).map_or(0, |i| i + 1)
}

/// `gcd(x, y)` (`x >= y`, both `x.len()` limbs, zero-extended); the
/// result is left in `x` or `y`: which, and its limbs. Lehmer steps
/// replace runs of single-limb quotients by one linear pass; a step
/// that finds no quotient divides (Knuth D, scratch `t` of `3 x.len() +
/// 1`).
pub(crate) fn gcd_lehmer<'a>(
    mut x: &'a mut [L],
    mut y: &'a mut [L],
    t: &mut [L],
) -> (&'a [L], usize) {
    let mut n = significant(x);
    loop {
        let yn = significant(&y[..n]);
        if yn == 0 {
            return (x, n);
        }
        if n == 1 {
            x[0] = gcd_word(x[0], y[0]);
            return (x, 1);
        }
        let f = lehmer(&x[..n], &y[..n], 0);
        if f.1 != 0 {
            lehmer_apply(&mut x[..n], &mut y[..n], f);
            n = significant(&x[..n]);
            continue;
        }
        // x = x mod y, then swap.
        let (q, t) = t.split_at_mut(n - yn + 1);
        divrem(q, &mut x[..n], &y[..yn], t);
        core::mem::swap(&mut x, &mut y);
        n = yn;
    }
}

/// `q = x / y`, `x = x % y` (`x.len() >= y.len()`, `y`'s top limb
/// nonzero; `q` of `x.len() - y.len() + 1`); `t` of `x.len() + y.len() +
/// 1` limbs.
pub(crate) fn divrem(q: &mut [L], x: &mut [L], y: &[L], t: &mut [L]) {
    let (n, yn) = (x.len(), y.len());
    let (u, rest) = t.split_at_mut(n + 1);
    let v = &mut rest[..yn];
    let sh = y[yn - 1].leading_zeros();
    v.copy_from_slice(y);
    shl_in(v, sh);
    u[..n].copy_from_slice(x);
    u[n] = shl_in(&mut u[..n], sh);
    knuth_d(q, u, v);
    shr_in(&mut u[..yn], sh);
    x[..yn].copy_from_slice(&u[..yn]);
    x[yn..].fill(0);
}

/// Limbs from which `hgcd` recurses, and from which `gcd` reduces by it.
pub(crate) const HGCD: usize = 100;

pub(crate) const GCD_DC: usize = 1200;

/// A 2x2 matrix of non-negative entries, determinant -1 when `neg`
/// (else 1): the cofactors of an `hgcd` reduction, the original pair
/// being `M` times the current one. Entries are zero-extended to their
/// common capacity.
pub(crate) struct Mat<'a> {
    pub(crate) e: [&'a mut [L]; 4],
    /// Entries are zero from limb `len` on.
    pub(crate) len: usize,
    neg: bool,
}

impl<'a> Mat<'a> {
    /// The identity with entries of `cap` limbs, carved from `t`.
    pub(crate) fn new(t: &'a mut [L], cap: usize) -> (Self, &'a mut [L]) {
        let (e0, t) = t.split_at_mut(cap);
        let (e1, t) = t.split_at_mut(cap);
        let (e2, t) = t.split_at_mut(cap);
        let (e3, t) = t.split_at_mut(cap);
        for e in [&mut *e0, &mut *e1, &mut *e2, &mut *e3] {
            e.fill(0);
        }
        e0[0] = 1;
        e3[0] = 1;
        let e = [e0, e1, e2, e3];
        (
            Mat {
                e,
                len: 1,
                neg: false,
            },
            t,
        )
    }

    /// Entry `i` up to `len`.
    pub(crate) fn at(&self, i: usize) -> &[L] {
        &self.e[i][..self.len]
    }

    /// `len` lowered past zero top limbs.
    pub(crate) fn trim(&mut self) {
        while self.len > 1 && self.e.iter().all(|e| e[self.len - 1] == 0) {
            self.len -= 1;
        }
    }

    /// Row `r`'s two entries, `w` limbs of them.
    pub(crate) fn row(&mut self, r: usize, w: usize) -> (&mut [L], &mut [L]) {
        let (lo, hi) = self.e.split_at_mut(2 * r + 1);
        (&mut lo[2 * r][..w], &mut hi[0][..w])
    }

    /// `M = M [[c0, c1], [c2, c3]]` for single limbs `c` below
    /// 2^LEHMER (determinant -1 when `neg`).
    pub(crate) fn mul1(&mut self, c: [L; 4], neg: bool) {
        let w = (self.len + 1).min(self.e[0].len());
        for r in 0..2 {
            let (x, y) = self.row(r, w);
            let (mut cx, mut cy) = (0 as W, 0 as W);
            for (u, v) in x.iter_mut().zip(y.iter_mut()) {
                let (p, q) = (*u as W, *v as W);
                // < 2 (2^LB - 1) 2^LEHMER + carry < 2^2LB.
                cx += p * c[0] as W + q * c[2] as W;
                cy += p * c[1] as W + q * c[3] as W;
                *u = cx as L;
                *v = cy as L;
                cx >>= LB;
                cy >>= LB;
            }
            debug_assert!(cx == 0 && cy == 0);
        }
        self.len = w;
        self.trim();
        self.neg ^= neg;
    }

    /// `M = M [[q, 1], [1, 0]]` (one Euclid step of quotient `q`).
    pub(crate) fn mul_q(&mut self, q: &[L], t: &mut [L]) {
        let cap = self.e[0].len();
        let w = (significant(q) + self.len + 1).min(cap);
        let (p, t) = t.split_at_mut(significant(q) + self.len + 1);
        for r in 0..2 {
            let (x, y) = self.row(r, w);
            mul_into(p, q, x, t);
            add_in(p, y);
            y.copy_from_slice(x);
            debug_assert!(significant(p) <= w);
            x.copy_from_slice(&p[..w]);
        }
        self.len = w;
        self.trim();
        self.neg = !self.neg;
    }

    /// Columns swapped (`M [[0, 1], [1, 0]]`).
    pub(crate) fn swap_cols(&mut self) {
        let w = self.len;
        for r in 0..2 {
            let (x, y) = self.row(r, w);
            x.swap_with_slice(y);
        }
        self.neg = !self.neg;
    }

    /// `M = M N`.
    pub(crate) fn mul(&mut self, n: &Mat<'_>, t: &mut [L]) {
        // A sum of two products: one limb past them.
        let k = self.len + n.len + 1;
        let w = k.min(self.e[0].len());
        let (p0, t) = t.split_at_mut(k);
        let (p1, t) = t.split_at_mut(k);
        let (p2, t) = t.split_at_mut(k);
        let (p3, t) = t.split_at_mut(k);
        let len = self.len;
        if let Some(nt) = dot_len(len.min(n.len), k) {
            // One transform per entry: row r of M N is (x n0 + y n2, x
            // n1 + y n3) for M's row (x, y).
            let x: [&[u32]; 8] = [
                digits32(self.at(0)),
                digits32(self.at(1)),
                digits32(self.at(2)),
                digits32(self.at(3)),
                digits32(n.at(0)),
                digits32(n.at(1)),
                digits32(n.at(2)),
                digits32(n.at(3)),
            ];
            let terms = [
                (0, 4, 1, 6, false),
                (0, 5, 1, 7, false),
                (2, 4, 3, 6, false),
                (2, 5, 3, 7, false),
            ];
            let mut out = [
                digits32_mut(p0),
                digits32_mut(p1),
                digits32_mut(p2),
                digits32_mut(p3),
            ];
            ntt_dot(&mut out, &x, &terms, nt, digits32_mut(t));
            for (i, q) in [&*p0, &*p1, &*p2, &*p3].into_iter().enumerate() {
                debug_assert!(significant(q) <= w);
                self.e[i][..w].copy_from_slice(&q[..w]);
            }
        } else {
            for r in 0..2 {
                let (x, y) = self.row(r, w);
                mul_into(p0, &x[..len], n.at(0), t);
                mul_into(p1, &y[..len], n.at(2), t);
                mul_into(p2, &x[..len], n.at(1), t);
                mul_into(p3, &y[..len], n.at(3), t);
                let c = add_in(p0, p1) | add_in(p2, p3);
                debug_assert!(!c && significant(p0) <= w && significant(p2) <= w);
                x.copy_from_slice(&p0[..w]);
                y.copy_from_slice(&p2[..w]);
            }
        }
        self.len = w;
        self.trim();
        self.neg ^= n.neg;
    }
}

/// `r = a b` zero-extended (`r` of at least their significant limbs).
pub(crate) fn mul_into(r: &mut [L], a: &[L], b: &[L], t: &mut [L]) {
    let (an, bn) = (significant(a), significant(b));
    if an == 0 || bn == 0 {
        r.fill(0);
        return;
    }
    let (lo, hi) = r.split_at_mut(an + bn);
    mul(lo, &a[..an], &b[..bn], t);
    hi.fill(0);
}

/// `x < y` (same length).
pub(crate) fn less(x: &[L], y: &[L]) -> bool {
    x.iter().rev().cmp(y.iter().rev()) == core::cmp::Ordering::Less
}

/// `(a, b) = M^-1 (a, b)` for `a = alpha B^p + a_lo`, `b = beta B^p + b_lo`
/// where `(alpha, beta)` already is `M^-1` of the old high parts, ordered so
/// that `a >= b` (`M`'s columns swap when needed). With `M^-1 = det
/// [[m11, -m01], [-m10, m00]]`: `a = alpha B^p + det (m11 a_lo - m01
/// b_lo)`, the same for `b`. Both stay positive when alpha, beta are at least
/// B^s and the entries below B^(k - s) for high parts of `k <= 2s`
/// limbs (what `hgcd` leaves): the low terms are below B^(k - s + p).
pub(crate) fn adjust(a: &mut [L], b: &mut [L], p: usize, m: &mut Mat<'_>, t: &mut [L]) {
    let w = p + m.len;
    let (u, t) = t.split_at_mut(w);
    let (v, t) = t.split_at_mut(w);
    let (x, t) = t.split_at_mut(w);
    let (y, t) = t.split_at_mut(w);
    // (U, V) = (m11 a_lo - m01 b_lo, m00 b_lo - m10 a_lo), as magnitudes
    // in `u` and `x` with their signs.
    let signs = if let Some(nt) = dot_len(m.len.min(p), w) {
        let ops: [&[u32]; 6] = [
            digits32(&a[..p]),
            digits32(&b[..p]),
            digits32(m.at(0)),
            digits32(m.at(1)),
            digits32(m.at(2)),
            digits32(m.at(3)),
        ];
        let terms = [(5, 0, 3, 1, true), (2, 1, 4, 0, true)];
        let mut out = [digits32_mut(u), digits32_mut(x)];
        let neg = ntt_dot(&mut out, &ops, &terms, nt, digits32_mut(t));
        for (d, &ng) in [&mut *u, &mut *x].into_iter().zip(&neg) {
            if ng {
                neg_in(d);
            }
        }
        [neg[0], neg[1]]
    } else {
        mul_into(u, m.at(3), &a[..p], t);
        mul_into(y, m.at(2), &a[..p], t);
        mul_into(v, m.at(1), &b[..p], t);
        mul_into(x, m.at(0), &b[..p], t);
        let mut signs = [false; 2];
        for ((pos, neg), sg) in [(&mut *u, &mut *v), (&mut *x, &mut *y)]
            .into_iter()
            .zip(&mut signs)
        {
            *sg = less(pos, neg);
            if *sg {
                sub_in(neg, pos);
                pos.copy_from_slice(neg);
            } else {
                sub_in(pos, neg);
            }
        }
        signs
    };
    a[..p].fill(0);
    b[..p].fill(0);
    for ((r, d), sg) in [(&mut *a, &*u), (&mut *b, &*x)].into_iter().zip(signs) {
        // r += det (U or V).
        let minus = sg != m.neg;
        let dn = significant(d);
        let o = if minus {
            sub_in(r, &d[..dn])
        } else {
            add_in(r, &d[..dn])
        };
        debug_assert!(!o);
    }
    if less(a, b) {
        a.swap_with_slice(b);
        m.swap_cols();
    }
}

/// One reduction of `a >= b` keeping both at least B^s, into `M`: a
/// Lehmer step, else one Euclid step; whether there was one.
pub(crate) fn hgcd_step(a: &mut [L], b: &mut [L], s: usize, m: &mut Mat<'_>, t: &mut [L]) -> bool {
    let n = significant(a);
    let bn = significant(b);
    if bn <= s {
        return false;
    }
    let f = lehmer(&a[..n], &b[..n], s);
    if f.1 != 0 {
        lehmer_apply(&mut a[..n], &mut b[..n], f);
        // M (a', b') with (a', b') = E (a, b): M E^-1, E^-1 = det E [[d,
        // -b], [-c, a]] (no negative entries).
        let (ca, cb, cc, cd) = f;
        let det = SW::from(ca) * SW::from(cd) - SW::from(cb) * SW::from(cc);
        let u = |v: SL| v.unsigned_abs() as L;
        m.mul1([u(cd), u(cb), u(cc), u(ca)], det < 0);
        return true;
    }
    // (a, b) = (b, a mod b) unless that falls below B^s.
    let (q, t) = t.split_at_mut(n - bn + 1);
    let (r, t) = t.split_at_mut(n);
    r.copy_from_slice(&a[..n]);
    divrem(q, r, &b[..bn], t);
    if significant(r) <= s {
        return false;
    }
    m.mul_q(q, t);
    a[..n].copy_from_slice(&b[..n]);
    b[..n].copy_from_slice(r);
    true
}

/// Half gcd (Moller, "On Schonhage's algorithm and subquadratic integer
/// gcd computation", 2008): reduces `a >= b` (`n` significant limbs) by
/// Euclid steps while both stay at least B^s, `s = n/2 + 1`, into `M`
/// (the identity on entry, entries of at least `n - s + 1` limbs);
/// whether it did. From `HGCD` limbs on, the top half first reduces
/// itself recursively and its matrix is applied to the rest
/// (`adjust`), then the top of what is left once more, so the steps
/// in between and at the end are few.
pub(crate) fn hgcd(a: &mut [L], b: &mut [L], m: &mut Mat<'_>, t: &mut [L]) -> bool {
    let n = significant(a);
    let s = n / 2 + 1;
    if significant(b) <= s {
        return false;
    }
    let mut done = false;
    if n >= HGCD {
        let p = n / 2;
        if hgcd(&mut a[p..n], &mut b[p..n], m, t) {
            adjust(&mut a[..n], &mut b[..n], p, m, t);
            done = true;
        }
        while significant(a) > 3 * n / 4 + 1 {
            if !hgcd_step(a, b, s, m, t) {
                return done;
            }
            done = true;
        }
        let k = significant(a);
        if k > s + 2 {
            // The top `2 (k - s) - 1` limbs, whose own floor lands at
            // about B^(s + 1) for the whole.
            let p = 2 * s - k + 1;
            let (mut m1, t) = Mat::new(t, (k - p) / 2 + 3);
            if hgcd(&mut a[p..k], &mut b[p..k], &mut m1, t) {
                adjust(&mut a[..k], &mut b[..k], p, &mut m1, t);
                m.mul(&m1, t);
                done = true;
            }
        }
    }
    while hgcd_step(a, b, s, m, t) {
        done = true;
    }
    done
}

/// Scratch `L`s `hgcd` may use for `n` limbs: at each level what
/// `hgcd_step`, `adjust` and `Mat::mul` take (their temporaries plus a
/// product's scratch, or `ntt_dot`'s: at most 44 `L`s per result limb),
/// and the second half's matrix.
pub(crate) const fn hgcd_scratch(n: usize) -> usize {
    let own = 48 * n + 64 + scratch_bound(n, n);
    if n < HGCD {
        return own;
    }
    let first = hgcd_scratch(n - n / 2);
    let inner = hgcd_scratch(n / 2);
    let second = 4 * (n / 4 + 4) + if inner > own { inner } else { own };
    let r = if first > own { first } else { own };
    if second > r { second } else { r }
}

/// `x, y` (`x >= y`, zero-extended alike) reduced by `hgcd` on their top
/// third until below `GCD_DC` limbs (one Euclid step when it finds
/// nothing); the gcd is unchanged.
pub(crate) fn gcd_dc(x: &mut [L], y: &mut [L], t: &mut [L]) {
    loop {
        let n = significant(x);
        let yn = significant(y);
        if n < GCD_DC || yn == 0 {
            return;
        }
        let p = 2 * n / 3;
        let (mut m, t) = Mat::new(t, (n - p) / 2 + 3);
        if hgcd(&mut x[p..n], &mut y[p..n], &mut m, t) {
            adjust(&mut x[..n], &mut y[..n], p, &mut m, t);
        } else {
            let (q, t) = t.split_at_mut(n - yn + 1);
            divrem(q, &mut x[..n], &y[..yn], t);
            x.swap_with_slice(y);
        }
    }
}

/// `u32` limbs of scratch `nabi_nat_gcd` needs.
#[unsafe(no_mangle)]
pub const extern "C" fn nabi_nat_gcd_scratch(an: usize, bn: usize) -> usize {
    let m = packed(if an > bn { an } else { bn });
    let dc = if m < GCD_DC {
        0
    } else {
        let (h, a) = (
            hgcd_scratch(m - 2 * m / 3),
            32 * m + 64 + scratch_bound(m, m),
        );
        4 * (m / 6 + 4) + if h > a { h } else { a }
    };
    (2 * m + if dc > 3 * m + 1 { dc } else { 3 * m + 1 }) * PER
}

/// `r = gcd(a, b)` (`an >= bn >= 1`, `a >= b`, both without leading
/// zero limbs): half-gcd reductions down to `GCD_DC` limbs, then
/// Lehmer; its limbs (at most `bn`). `t` holds
/// `nabi_nat_gcd_scratch(an, bn)` limbs.
///
/// # Safety
/// Live `GcBuf<u32>`s of at least those lengths (`r` of `bn`); `r` and
/// `t` distinct from each other and from `a` / `b`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nabi_nat_gcd(
    r: *mut u8,
    a: *mut u8,
    an: usize,
    b: *mut u8,
    bn: usize,
    t: *mut u8,
) -> usize {
    let m = packed(an);
    let tn = nabi_nat_gcd_scratch(an, bn) / PER;
    // SAFETY: caller contract; `a` / `b` only read.
    let (a, b, r) = unsafe { (&*limbs(a, an), &*limbs(b, bn), limbs(r, bn)) };
    // SAFETY: as above; `t` is its own buffer of `tn * PER` limbs.
    let t = unsafe { slice::from_raw_parts_mut(t.add(HEADER_SIZE).cast::<L>(), tn) };
    let (x, t) = t.split_at_mut(m);
    let (y, t) = t.split_at_mut(m);
    parked(an, || {
        pack(x, a);
        y.fill(0);
        pack(&mut y[..packed(bn)], b);
        gcd_dc(x, y, t);
        let (g, gn) = gcd_lehmer(x, y, t);
        // SAFETY: as in `digits32`.
        let g32 = unsafe { slice::from_raw_parts(g.as_ptr().cast::<u32>(), gn * PER) };
        let k = g32.iter().rposition(|&v| v != 0).map_or(0, |i| i + 1);
        r[..k].copy_from_slice(&g32[..k]);
        k
    })
}
