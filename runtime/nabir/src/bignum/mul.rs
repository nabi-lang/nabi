//! Multiplication: schoolbook, Karatsuba, Toom-3 and the packed entry points.

#[allow(clippy::wildcard_imports)]
use crate::bignum::*;

/// `r[..a.len()] += a * x`; the carry limb.
pub(crate) fn addmul_1(r: &mut [L], a: &[L], x: L) -> L {
    let mut c: L = 0;
    for (y, &v) in r.iter_mut().zip(a) {
        // (2^b - 1)^2 + 2 (2^b - 1) < 2^2b.
        let t = v as W * x as W + *y as W + c as W;
        *y = t as L;
        c = (t >> LB) as L;
    }
    c
}

/// `dst = src` zero-extended.
pub(crate) fn copy_ext(dst: &mut [L], src: &[L]) {
    let (lo, hi) = dst.split_at_mut(src.len());
    lo.copy_from_slice(src);
    hi.fill(0);
}

/// `r[off..] += v` where the sum fits `r` (limbs of `v` past the end of
/// `r` are zero).
pub(crate) fn add_at(r: &mut [L], off: usize, v: &[L]) {
    let n = v.len().min(r.len() - off);
    debug_assert!(v[n..].iter().all(|&x| x == 0));
    let c = add_in(&mut r[off..], &v[..n]);
    debug_assert!(!c);
}

/// `r[..a.len()] = a * x`; the carry limb.
pub(crate) fn mul_1(r: &mut [L], a: &[L], x: L) -> L {
    let mut c: L = 0;
    for (y, &v) in r.iter_mut().zip(a) {
        let t = v as W * x as W + c as W;
        *y = t as L;
        c = (t >> LB) as L;
    }
    c
}

/// `r[..a.len()] += a * (x0 + x1 B)`: two rows per pass over `r`; the
/// limbs for `r[a.len()]` and `r[a.len() + 1]`.
pub(crate) fn addmul_2(r: &mut [L], a: &[L], x0: L, x1: L) -> (L, L) {
    // Pending carries into the next two positions.
    let (mut c0, mut c1): (L, L) = (0, 0);
    for (y, &v) in r.iter_mut().zip(a) {
        let t = *y as W + v as W * x0 as W + c0 as W;
        *y = t as L;
        let u = v as W * x1 as W + c1 as W + (t >> LB);
        c0 = u as L;
        c1 = (u >> LB) as L;
    }
    (c0, c1)
}

/// `r = a * b`, `a.len() >= b.len() >= 1`. Row `i` writes `r[i + n]`,
/// so nothing needs clearing first.
pub(crate) fn mul_basecase(r: &mut [L], a: &[L], b: &[L]) {
    let n = a.len();
    r[n] = mul_1(&mut r[..n], a, b[0]);
    let mut i = 1;
    while i + 1 < b.len() {
        (r[i + n], r[i + n + 1]) = addmul_2(&mut r[i..i + n], a, b[i], b[i + 1]);
        i += 2;
    }
    if i < b.len() {
        r[i + n] = addmul_1(&mut r[i..i + n], a, b[i]);
    }
}

/// `r = a^2`: the cross products once, then doubled plus the diagonal
/// in one pass.
pub(crate) fn sqr_basecase(r: &mut [L], a: &[L]) {
    let n = a.len();
    r[0] = 0;
    r[2 * n - 1] = 0;
    if n > 1 {
        r[n] = mul_1(&mut r[1..n], &a[1..], a[0]);
        for i in 1..n - 1 {
            r[i + n] = addmul_1(&mut r[2 * i + 1..i + n], &a[i + 1..], a[i]);
        }
    }
    let mut top: L = 0;
    let mut c = 0 as W;
    for (p, &x) in r.chunks_exact_mut(2).zip(a) {
        let (lo, hi) = (p[0], p[1]);
        let sq = x as W * x as W;
        let s0 = ((lo << 1) | top) as W + (sq as L) as W + c;
        let s1 = ((hi << 1) | (lo >> (LB - 1))) as W + (sq >> LB) + (s0 >> LB);
        top = hi >> (LB - 1);
        p[0] = s0 as L;
        p[1] = s1 as L;
        c = s1 >> LB;
    }
    debug_assert!(c == 0 && top == 0);
}

/// Scratch `L`s that `mul(r, a, b, t)` / `sqr` may use, for operands of
/// `n` and `m` limbs. By induction over the dispatch: a balanced step
/// on `N = max(n, m)` limbs uses at most `4k + 4` (Karatsuba, `k =
/// ceil(N/2)`) or `8k + 8` (Toom-3, `k = ceil(N/3)`) limbs plus its
/// recursion on `k + 1`, which stays within `6N + 40 bits(N)` once
/// `N >= 18`; an unbalanced one uses `2m` plus a balanced `m` product.
pub(crate) const fn scratch_bound(n: usize, m: usize) -> usize {
    let (lo, hi) = if n < m { (n, m) } else { (m, n) };
    if lo < KARA {
        return 0;
    }
    let bits = (usize::BITS - hi.leading_zeros()) as usize;
    // NTT steps anywhere below use at most this on top (the largest
    // transform bounds it for huge operands).
    let ntt = if hi < NTT_LO {
        0
    } else if ntt_len(hi, hi) <= NTT_MAX {
        ntt_scratch(hi, hi)
    } else {
        6 * NTT_MAX / PER
    };
    6 * hi + 40 * bits + ntt
}

/// `r = a * b` (`r.len() == a.len() + b.len()`), scratch `t`.
pub(crate) fn mul(r: &mut [L], a: &[L], b: &[L], t: &mut [L]) {
    let (a, b) = if a.len() < b.len() { (b, a) } else { (a, b) };
    let (n, m) = (a.len(), b.len());
    if m == 0 {
        r.fill(0);
    } else if m < KARA {
        mul_basecase(r, a, b);
    } else if m >= NTT && ntt_len(n, m) <= NTT_MAX {
        ntt_mul(r, a, b, t);
    } else if 2 * m <= n {
        // Unbalanced: `m`-limb pieces of `a`, each against all of `b`.
        r.fill(0);
        let (p, t) = t.split_at_mut(2 * m);
        for (i, piece) in a.chunks(m).enumerate() {
            let p = &mut p[..piece.len() + m];
            mul(p, piece, b, t);
            let c = add_in(&mut r[i * m..], p);
            debug_assert!(!c);
        }
    } else if m < TOOM3 || m <= 2 * n.div_ceil(3) {
        karatsuba(r, a, b, t);
    } else {
        toom3(r, a, b, t);
    }
}

/// `r = a^2` (`r.len() == 2 a.len()`), scratch `t`.
pub(crate) fn sqr(r: &mut [L], a: &[L], t: &mut [L]) {
    let n = a.len();
    if n == 0 {
        r.fill(0);
    } else if n < KARA_SQR {
        sqr_basecase(r, a);
    } else if n < TOOM3_SQR {
        karatsuba_sqr(r, a, t);
    } else if n >= NTT_SQR && ntt_len(n, n) <= NTT_MAX {
        ntt_mul(r, a, &[], t);
    } else {
        toom3_sqr(r, a, t);
    }
}

/// a = a1 B^k + a0, b = b1 B^k + b0 (`n >= m > n / 2`):
/// ab = a1 b1 B^2k + ((a0 + a1)(b0 + b1) - a0 b0 - a1 b1) B^k + a0 b0.
pub(crate) fn karatsuba(r: &mut [L], a: &[L], b: &[L], t: &mut [L]) {
    let k = a.len().div_ceil(2);
    let (a0, a1) = a.split_at(k);
    let (b0, b1) = b.split_at(k);
    {
        let (lo, hi) = r.split_at_mut(2 * k);
        mul(lo, a0, b0, t);
        mul(hi, a1, b1, t);
    }
    let (sa, t) = t.split_at_mut(k + 1);
    let (sb, t) = t.split_at_mut(k + 1);
    let (z, t) = t.split_at_mut(2 * k + 2);
    copy_ext(sa, a0);
    add_in(sa, a1);
    copy_ext(sb, b0);
    add_in(sb, b1);
    mul(z, sa, sb, t);
    sub_in(z, &r[..2 * k]);
    sub_in(z, &r[2 * k..]);
    add_at(r, k, z);
}

pub(crate) fn karatsuba_sqr(r: &mut [L], a: &[L], t: &mut [L]) {
    let k = a.len().div_ceil(2);
    let (a0, a1) = a.split_at(k);
    {
        let (lo, hi) = r.split_at_mut(2 * k);
        sqr(lo, a0, t);
        sqr(hi, a1, t);
    }
    let (sa, t) = t.split_at_mut(k + 1);
    let (z, t) = t.split_at_mut(2 * k + 2);
    copy_ext(sa, a0);
    add_in(sa, a1);
    sqr(z, sa, t);
    sub_in(z, &r[..2 * k]);
    sub_in(z, &r[2 * k..]);
    add_at(r, k, z);
}

/// The three pieces of `a` at `k` limbs (`a2` may be shorter).
pub(crate) const fn split3(a: &[L], k: usize) -> (&[L], &[L], &[L]) {
    let (a0, rest) = a.split_at(k);
    let (a1, a2) = rest.split_at(k);
    (a0, a1, a2)
}

/// `e = a(1)`.
pub(crate) fn eval_1(e: &mut [L], a: (&[L], &[L], &[L])) {
    copy_ext(e, a.0);
    add_in(e, a.1);
    add_in(e, a.2);
}

/// `e = |a(-1)|` (`p` scratch); whether `a(-1) < 0`.
pub(crate) fn eval_m1(e: &mut [L], p: &mut [L], a: (&[L], &[L], &[L])) -> bool {
    copy_ext(p, a.0);
    add_in(p, a.2);
    copy_ext(e, a.1);
    let lt = {
        // p < a1?
        let mut lt = false;
        for (x, y) in p.iter().zip(e.iter()).rev() {
            if x != y {
                lt = x < y;
                break;
            }
        }
        lt
    };
    if lt {
        sub_in(e, p);
    } else {
        neg_in(e);
        add_in(e, p);
    }
    lt
}

/// `e = a(2) = a0 + 2 a1 + 4 a2`.
pub(crate) fn eval_2(e: &mut [L], a: (&[L], &[L], &[L])) {
    copy_ext(e, a.2);
    shl1_in(e);
    add_in(e, a.1);
    shl1_in(e);
    add_in(e, a.0);
}

/// Toom-3 interpolation from the point values at 0 (`r[..2k]`), 1
/// (`v1`), -1 (`vm1`), 2 (`v2`) and infinity (`r[4k..]`; the middle of
/// `r` zero), each `2k + 2`-limb two's complement; adds the inner
/// coefficients c1..c3 into `r`.
pub(crate) fn toom3_interpolate(r: &mut [L], k: usize, v1: &mut [L], vm1: &mut [L], v2: &mut [L]) {
    let (v0, rest) = r.split_at(2 * k);
    let vinf = &rest[2 * k..];
    // v2 = (v2 - vm1) / 3 = c1 + c2 + 3 c3 + 5 c4
    sub_in(v2, vm1);
    divexact3_in(v2);
    // vm1 = (v1 - vm1) / 2 = c1 + c3
    neg_in(vm1);
    add_in(vm1, v1);
    sar1_in(vm1);
    // v1 = v1 - v0 = c1 + c2 + c3 + c4
    sub_in(v1, v0);
    // v2 = (v2 - v1) / 2 - 2 c4 = c3
    sub_in(v2, v1);
    sar1_in(v2);
    sub_in(v2, vinf);
    sub_in(v2, vinf);
    // v1 = v1 - vm1 - c4 = c2
    sub_in(v1, vm1);
    sub_in(v1, vinf);
    // vm1 = vm1 - c3 = c1
    sub_in(vm1, v2);
    add_at(r, k, vm1);
    add_at(r, 2 * k, v1);
    add_at(r, 3 * k, v2);
}

/// Toom-3 on pieces of `k = ceil(n/3)` limbs (`m > 2k`).
pub(crate) fn toom3(r: &mut [L], a: &[L], b: &[L], t: &mut [L]) {
    let k = a.len().div_ceil(3);
    let a3 = split3(a, k);
    let b3 = split3(b, k);
    {
        let (lo, rest) = r.split_at_mut(2 * k);
        let (mid, hi) = rest.split_at_mut(2 * k);
        mul(lo, a3.0, b3.0, t);
        mul(hi, a3.2, b3.2, t);
        mid.fill(0);
    }
    let w = 2 * k + 2;
    let (v1, t) = t.split_at_mut(w);
    let (vm1, t) = t.split_at_mut(w);
    let (v2, t) = t.split_at_mut(w);
    let (p, t) = t.split_at_mut(k + 1);
    let (ea, t) = t.split_at_mut(k + 1);
    let (eb, t) = t.split_at_mut(k + 1);
    eval_1(ea, a3);
    eval_1(eb, b3);
    mul(v1, ea, eb, t);
    let na = eval_m1(ea, p, a3);
    let nb = eval_m1(eb, p, b3);
    mul(vm1, ea, eb, t);
    if na != nb {
        neg_in(vm1);
    }
    eval_2(ea, a3);
    eval_2(eb, b3);
    mul(v2, ea, eb, t);
    toom3_interpolate(r, k, v1, vm1, v2);
}

pub(crate) fn toom3_sqr(r: &mut [L], a: &[L], t: &mut [L]) {
    let k = a.len().div_ceil(3);
    let a3 = split3(a, k);
    {
        let (lo, rest) = r.split_at_mut(2 * k);
        let (mid, hi) = rest.split_at_mut(2 * k);
        sqr(lo, a3.0, t);
        sqr(hi, a3.2, t);
        mid.fill(0);
    }
    let w = 2 * k + 2;
    let (v1, t) = t.split_at_mut(w);
    let (vm1, t) = t.split_at_mut(w);
    let (v2, t) = t.split_at_mut(w);
    let (p, t) = t.split_at_mut(k + 1);
    let (ea, t) = t.split_at_mut(k + 1);
    eval_1(ea, a3);
    sqr(v1, ea, t);
    eval_m1(ea, p, a3);
    sqr(vm1, ea, t);
    eval_2(ea, a3);
    sqr(v2, ea, t);
    toom3_interpolate(r, k, v1, vm1, v2);
}

/// `L`s holding `n` `u32` limbs.
pub(crate) const fn packed(n: usize) -> usize {
    n.div_ceil(PER)
}

// `PER` consecutive `u32` limbs are one `L` in memory.
pub(crate) const _: () = assert!(PER == 1 || cfg!(target_endian = "little"));

/// `dst = src` (`dst.len() == packed(src.len())`).
pub(crate) fn pack(dst: &mut [L], src: &[u32]) {
    dst[dst.len() - 1] = 0;
    // SAFETY: `dst` holds at least `src.len()` `u32`s; distinct buffers.
    unsafe {
        core::ptr::copy_nonoverlapping(src.as_ptr(), dst.as_mut_ptr().cast::<u32>(), src.len());
    };
}

/// `dst = src` truncated to `dst.len()` `u32`s.
pub(crate) fn unpack(dst: &mut [u32], src: &[L]) {
    assert!(dst.len() <= src.len() * PER);
    // SAFETY: as in `pack`.
    unsafe {
        core::ptr::copy_nonoverlapping(src.as_ptr().cast::<u32>(), dst.as_mut_ptr(), dst.len());
    };
}

/// `u32` limbs of scratch `nabi_nat_mul` needs for an `an` x `bn` product.
#[unsafe(no_mangle)]
pub const extern "C" fn nabi_nat_mul_scratch(an: usize, bn: usize) -> usize {
    let (na, nb) = (packed(an), packed(bn));
    (2 * (na + nb) + scratch_bound(na, nb)) * PER
}

/// `r[0..an + bn] = a * b`; `t` holds `nabi_nat_mul_scratch(an, bn)`
/// limbs. `a` and `b` may be the same buffer (`an == bn` squares).
///
/// # Safety
/// Live `GcBuf<u32>`s of at least those lengths; `r` and `t` distinct
/// from each other and from `a` / `b`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nabi_nat_mul(
    r: *mut u8,
    a: *mut u8,
    an: usize,
    b: *mut u8,
    bn: usize,
    t: *mut u8,
) {
    let square = a == b && an == bn;
    let tn = nabi_nat_mul_scratch(an, bn) / PER;
    // SAFETY: caller contract; `a` / `b` only read; elements are 8-aligned.
    let (a, b, r) = unsafe { (&*limbs(a, an), &*limbs(b, bn), limbs(r, an + bn)) };
    // SAFETY: as above; `t` is its own buffer of `tn * PER` limbs.
    let t = unsafe { slice::from_raw_parts_mut(t.add(HEADER_SIZE).cast::<L>(), tn) };
    parked(an + bn, || nat_mul(r, a, b, square, t));
}

/// `r = a * b` (`a` squared when `square`) on packed copies in `t`
/// (`nabi_nat_mul_scratch` `u32`s).
pub(crate) fn nat_mul(r: &mut [u32], a: &[u32], b: &[u32], square: bool, t: &mut [L]) {
    let (na, nb) = (packed(a.len()), packed(b.len()));
    let (pa, t) = t.split_at_mut(na);
    let (pb, t) = t.split_at_mut(nb);
    let (pr, t) = t.split_at_mut(na + nb);
    pack(pa, a);
    if square {
        sqr(pr, pa, t);
    } else {
        pack(pb, b);
        mul(pr, pa, pb, t);
    }
    unpack(r, pr);
}
