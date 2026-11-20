//! Single-limb and Knuth D division.

#[allow(clippy::wildcard_imports)]
use crate::bignum::*;

/// `q[0..n] = a / v`; the remainder.
///
/// # Safety
/// Live `GcBuf<u32>`s of at least `n` elements; `v != 0`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nabi_nat_divmod_small(q: *mut u8, a: *mut u8, n: usize, v: u32) -> u32 {
    // SAFETY: caller contract (`q` may be `a`: each limb is read first).
    let (q, a) = unsafe { (limbs(q, n), limbs(a, n)) };
    park::parked(n >= PARK_LINEAR, || {
        let d = u64::from(v);
        let mut rem = 0u64;
        for i in (0..n).rev() {
            let cur = (rem << 32) | u64::from(a[i]);
            q[i] = (cur / d) as u32;
            rem = cur % d;
        }
        rem as u32
    })
}

/// floor((B^2 - 1) / d) - B for a normalized `d` (top bit set): the
/// reciprocal of Moller and Granlund, "Improved division by invariant
/// integers" (2011).
pub(crate) const fn reciprocal(d: L) -> L {
    (((((!d) as W) << LB) | L::MAX as W) / d as W) as L
}

/// `(u1 B + u0) / d` for `u1 < d`, `d` normalized, `v = reciprocal(d)`:
/// quotient and remainder (algorithm 4 of the paper).
pub(crate) const fn div2by1(u1: L, u0: L, d: L, v: L) -> (L, L) {
    let p = v as W * u1 as W + (((u1 as W) << LB) | u0 as W);
    let mut q1 = ((p >> LB) as L).wrapping_add(1);
    let q0 = p as L;
    let mut r = u0.wrapping_sub(q1.wrapping_mul(d));
    if r > q0 {
        q1 = q1.wrapping_sub(1);
        r = r.wrapping_add(d);
    }
    if r >= d {
        q1 += 1;
        r -= d;
    }
    (q1, r)
}

/// `r[..a.len()] -= a * x`; the borrow limb.
pub(crate) fn submul_1(r: &mut [L], a: &[L], x: L) -> L {
    let mut c: L = 0;
    for (y, &v) in r.iter_mut().zip(a) {
        let p = v as W * x as W + c as W;
        let (d, b) = y.overflowing_sub(p as L);
        *y = d;
        c = (p >> LB) as L + L::from(b);
    }
    c
}

/// `r <<= s` (`s < LB`); the bits shifted out.
pub(crate) fn shl_in(r: &mut [L], s: u32) -> L {
    if s == 0 {
        return 0;
    }
    let mut top = 0;
    for x in r {
        let v = *x;
        *x = (v << s) | top;
        top = v >> (LB - s);
    }
    top
}

/// `r >>= s` (`s < LB`), zero filled.
pub(crate) fn shr_in(r: &mut [L], s: u32) {
    if s == 0 {
        return;
    }
    let n = r.len();
    for i in 0..n - 1 {
        r[i] = (r[i] >> s) | (r[i + 1] << (LB - s));
    }
    r[n - 1] >>= s;
}

/// Knuth TAOCP 4.3.1 algorithm D: `u` (`m + n + 1` limbs, `u[m + n] <
/// v[n - 1]`) / `v` (`n` limbs, top bit set) -> `q[0..=m]`, the
/// remainder left in `u[..n]`.
pub(crate) fn knuth_d(q: &mut [L], u: &mut [L], v: &[L]) {
    let n = v.len();
    let top = v[n - 1];
    let inv = reciprocal(top);
    if n == 1 {
        let mut rem = u[u.len() - 1];
        for i in (0..u.len() - 1).rev() {
            (q[i], rem) = div2by1(rem, u[i], top, inv);
            u[i] = 0;
        }
        u[0] = rem;
        return;
    }
    let next = v[n - 2];
    for j in (0..q.len()).rev() {
        let (u1, u0, u2) = (u[j + n], u[j + n - 1], u[j + n - 2]);
        let (mut qhat, mut rhat, mut big) = if u1 >= top {
            // qhat = B - 1; rhat = u1 B + u0 - (B - 1) top = u0 + top.
            let (r, o) = u0.overflowing_add(top);
            (L::MAX, r, o)
        } else {
            let (q, r) = div2by1(u1, u0, top, inv);
            (q, r, false)
        };
        // At most two steps make qhat exact or one too big.
        while !big && qhat as W * next as W > (((rhat as W) << LB) | u2 as W) {
            qhat -= 1;
            (rhat, big) = rhat.overflowing_add(top);
        }
        let c = submul_1(&mut u[j..j + n], v, qhat);
        let (t, b) = u[j + n].overflowing_sub(c);
        u[j + n] = t;
        if b {
            // One too big: add the divisor back.
            qhat -= 1;
            let c = add_in(&mut u[j..j + n], v);
            u[j + n] = u[j + n].wrapping_add(L::from(c));
        }
        q[j] = qhat;
    }
}

/// `u32` limbs of scratch `nabi_nat_divmod` needs.
#[unsafe(no_mangle)]
pub const extern "C" fn nabi_nat_divmod_scratch(an: usize, bn: usize) -> usize {
    let (na, nb) = (packed(an), packed(bn));
    (2 * na + nb + 2) * PER
}

/// `q[0..=an - bn] = a / b`, `r[0..bn] = a % b` for `an >= bn >= 2`, the
/// top limb of `b` nonzero; `t` holds `nabi_nat_divmod_scratch(an, bn)`.
///
/// # Safety
/// Live `GcBuf<u32>`s of at least those lengths; `q`, `r` and `t`
/// distinct from each other and from `a` / `b`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nabi_nat_divmod(
    q: *mut u8,
    r: *mut u8,
    a: *mut u8,
    an: usize,
    b: *mut u8,
    bn: usize,
    t: *mut u8,
) {
    let (na, nb) = (packed(an), packed(bn));
    let tn = nabi_nat_divmod_scratch(an, bn) / PER;
    // SAFETY: caller contract; `a` / `b` only read.
    let (a, b, q, r) = unsafe {
        (
            &*limbs(a, an),
            &*limbs(b, bn),
            limbs(q, an - bn + 1),
            limbs(r, bn),
        )
    };
    // SAFETY: as above; `t` is its own buffer of `tn * PER` limbs.
    let t = unsafe { slice::from_raw_parts_mut(t.add(HEADER_SIZE).cast::<L>(), tn) };
    let (u, t) = t.split_at_mut(na + 1);
    let (v, t) = t.split_at_mut(nb);
    let qs = &mut t[..=na - nb];
    parked(an, || {
        pack(&mut u[..na], a);
        u[na] = 0;
        pack(v, b);
        let s = v[nb - 1].leading_zeros();
        shl_in(v, s);
        u[na] = shl_in(&mut u[..na], s);
        knuth_d(qs, u, v);
        shr_in(&mut u[..nb], s);
        unpack(q, qs);
        unpack(r, &u[..nb]);
    });
}
