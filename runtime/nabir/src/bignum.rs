//! `int` limb kernels (`library/core/src/num/bigint.nabi`) over
//! `GcBuf<u32>` limbs (little-endian, base 2^32), computed on machine
//! words: multiplication (schoolbook, Karatsuba, Toom-3, three-prime
//! NTT, squaring variants, products by an operand transformed once,
//! products mod B^n - 1), Knuth D division, single-limb division,
//! Lehmer and half-gcd, add / sub / shifts and the base case of decimal
//! output. The library does the rest (signs, recursive division, radix
//! conversion, allocation). None of these allocate: scratch space comes from the
//! caller. The long ones run parked ([`parked`]).

// Knuth's names (q, r, u, v, m, n) read better than long ones here.
#![allow(clippy::many_single_char_names)]
// `GcBuf` elements start 8-aligned after the header.
#![allow(clippy::cast_ptr_alignment)]

mod digits;
mod div;
mod gcd;
mod mul;
mod ntt;

pub(crate) use crate::bignum::div::*;
pub(crate) use crate::bignum::mul::*;
pub(crate) use crate::bignum::ntt::*;

use crate::park;
use core::slice;
use nabir_common::header::HEADER_SIZE;

// Multiplication runs on machine-word limbs: the `u32` operands are
// packed into `L`s in the scratch buffer and the product unpacked. A
// big-endian target keeps `u32` limbs, the `u32` view of an `L` slice
// being its little-endian halves.
#[cfg(all(target_pointer_width = "64", target_endian = "little"))]
type L = u64;
#[cfg(all(target_pointer_width = "64", target_endian = "little"))]
type W = u128;
#[cfg(not(all(target_pointer_width = "64", target_endian = "little")))]
type L = u32;
#[cfg(not(all(target_pointer_width = "64", target_endian = "little")))]
type W = u64;
// Signed `L` / `W` (Lehmer cofactors and their sums).
#[cfg(all(target_pointer_width = "64", target_endian = "little"))]
type SL = i64;
#[cfg(all(target_pointer_width = "64", target_endian = "little"))]
type SW = i128;
#[cfg(not(all(target_pointer_width = "64", target_endian = "little")))]
type SL = i32;
#[cfg(not(all(target_pointer_width = "64", target_endian = "little")))]
type SW = i64;

const LB: u32 = L::BITS;
/// `u32` limbs per `L`.
const PER: usize = (L::BITS / 32) as usize;

/// `L` limbs from which products split (Karatsuba), then Toom-3, then
/// go to the NTT, and the same for squares. Measured on aarch64 (64-bit
/// limbs) in cycles; the curves are flat around these.
const KARA: usize = 32;
const TOOM3: usize = 150;
const KARA_SQR: usize = 32;
const TOOM3_SQR: usize = 200;
const NTT: usize = 700;
const NTT_SQR: usize = 1000;
const NTT_LO: usize = if NTT < NTT_SQR { NTT } else { NTT_SQR };

// `scratch_bound` relies on these (see there).
const _: () = assert!(KARA >= 18 && KARA_SQR >= KARA && TOOM3 >= KARA && TOOM3_SQR >= KARA_SQR);

/// `u32` limbs of work from which a kernel runs parked.
const PARK: usize = 2048;

/// `f()`, parked when `work` reaches `PARK` limbs ([`park::parked`]).
fn parked<R>(work: usize, f: impl FnOnce() -> R) -> R {
    park::parked(work >= PARK, f)
}

/// Limbs from which a linear kernel (add, sub, shifts, division by one
/// limb) runs parked: about 64 us of work.
const PARK_LINEAR: usize = 1 << 16;

/// The first `n` limbs of the `GcBuf<u32>` at `buf`.
///
/// # Safety
/// `buf` is a live `GcBuf<u32>` of at least `n` elements.
unsafe fn limbs<'a>(buf: *mut u8, n: usize) -> &'a mut [u32] {
    // SAFETY: caller contract; elements start after the header.
    unsafe { slice::from_raw_parts_mut(buf.add(HEADER_SIZE).cast::<u32>(), n) }
}

/// `r += a` over `r` (`a.len() <= r.len()`); the carry out.
fn add_in(r: &mut [L], a: &[L]) -> bool {
    let (lo, hi) = r.split_at_mut(a.len());
    let mut c = 0 as W;
    for (x, &y) in lo.iter_mut().zip(a) {
        let s = *x as W + y as W + c;
        *x = s as L;
        c = s >> LB;
    }
    if c == 0 {
        return false;
    }
    for x in hi {
        *x = x.wrapping_add(1);
        if *x != 0 {
            return false;
        }
    }
    true
}

/// `r -= a` over `r` (`a.len() <= r.len()`); the borrow out.
fn sub_in(r: &mut [L], a: &[L]) -> bool {
    let (lo, hi) = r.split_at_mut(a.len());
    let mut b = false;
    for (x, &y) in lo.iter_mut().zip(a) {
        let (d, b1) = x.overflowing_sub(y);
        let (d, b2) = d.overflowing_sub(L::from(b));
        *x = d;
        b = b1 | b2;
    }
    if !b {
        return false;
    }
    for x in hi {
        let old = *x;
        *x = old.wrapping_sub(1);
        if old != 0 {
            return false;
        }
    }
    true
}

/// `r = -r` (two's complement over `r.len()` limbs).
fn neg_in(r: &mut [L]) {
    let mut c = true;
    for x in r {
        let (v, o) = (!*x).overflowing_add(L::from(c));
        *x = v;
        c = o;
    }
}

/// `r <<= 1`; the top bit must be clear.
fn shl1_in(r: &mut [L]) {
    let mut top = 0;
    for x in r {
        let v = *x;
        *x = (v << 1) | top;
        top = v >> (LB - 1);
    }
    debug_assert!(top == 0);
}

/// `r >>= 1`, arithmetic (two's complement).
fn sar1_in(r: &mut [L]) {
    let n = r.len();
    for i in 0..n - 1 {
        r[i] = (r[i] >> 1) | (r[i + 1] << (LB - 1));
    }
    r[n - 1] = (r[n - 1] >> 1) | (r[n - 1] & (1 << (LB - 1)));
}

/// `r /= 3`, exact (Hensel division, so two's complement values too).
fn divexact3_in(r: &mut [L]) {
    const INV3: L = L::MAX / 3 * 2 + 1;
    let mut c: L = 0;
    for x in r {
        let (s, b) = x.overflowing_sub(c);
        let q = s.wrapping_mul(INV3);
        *x = q;
        c = ((q as W * 3) >> LB) as L + L::from(b);
    }
}

/// `r[0..an] = a + b` (`an >= bn`); the carry.
///
/// # Safety
/// Live `GcBuf<u32>`s of at least those lengths; `r` distinct from `a`
/// and `b`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nabi_nat_add(
    r: *mut u8,
    a: *mut u8,
    an: usize,
    b: *mut u8,
    bn: usize,
) -> u32 {
    // SAFETY: caller contract; `a` / `b` only read.
    let (r, a, b) = unsafe { (limbs(r, an), &*limbs(a, an), &*limbs(b, bn)) };
    park::parked(an >= PARK_LINEAR, || {
        let (lo, hi) = r.split_at_mut(bn);
        let mut c = 0u64;
        for ((x, &p), &q) in lo.iter_mut().zip(a).zip(b) {
            let s = u64::from(p) + u64::from(q) + c;
            *x = s as u32;
            c = s >> 32;
        }
        for (x, &p) in hi.iter_mut().zip(&a[bn..]) {
            let s = u64::from(p) + c;
            *x = s as u32;
            c = s >> 32;
        }
        c as u32
    })
}

/// `r[0..an] = a - b` (`a >= b`, `an >= bn`).
///
/// # Safety
/// As `nabi_nat_add`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nabi_nat_sub(r: *mut u8, a: *mut u8, an: usize, b: *mut u8, bn: usize) {
    // SAFETY: as in `nabi_nat_add`.
    let (r, a, b) = unsafe { (limbs(r, an), &*limbs(a, an), &*limbs(b, bn)) };
    park::parked(an >= PARK_LINEAR, || {
        let (lo, hi) = r.split_at_mut(bn);
        let mut borrow = 0u32;
        for ((x, &p), &q) in lo.iter_mut().zip(a).zip(b) {
            let (d, b1) = p.overflowing_sub(q);
            let (d, b2) = d.overflowing_sub(borrow);
            *x = d;
            borrow = u32::from(b1 | b2);
        }
        for (x, &p) in hi.iter_mut().zip(&a[bn..]) {
            let (d, b1) = p.overflowing_sub(borrow);
            *x = d;
            borrow = u32::from(b1);
        }
    });
}

/// `r[off..=off + n] = a[0..n] << s` (`s < 32`).
///
/// # Safety
/// Live, distinct `GcBuf<u32>`s of at least those lengths.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nabi_nat_shl(r: *mut u8, off: usize, a: *mut u8, n: usize, s: u32) {
    // SAFETY: caller contract.
    let (r, a) = unsafe { (&mut limbs(r, off + n + 1)[off..], &*limbs(a, n)) };
    park::parked(n >= PARK_LINEAR, || {
        let mut top = 0u64;
        for (x, &v) in r.iter_mut().zip(a) {
            let w = (u64::from(v) << s) | top;
            *x = w as u32;
            top = w >> 32;
        }
        r[n] = top as u32;
    });
}

/// `r[0..n - s / 32] = a[0..n] >> s` (`s < 32 n`).
///
/// # Safety
/// Live, distinct `GcBuf<u32>`s of at least those lengths.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nabi_nat_shr(r: *mut u8, a: *mut u8, n: usize, s: u64) {
    let skip = (s / 32) as usize;
    // SAFETY: caller contract.
    let (r, a) = unsafe { (limbs(r, n - skip), &limbs(a, n)[skip..]) };
    park::parked(n >= PARK_LINEAR, || {
        for (i, x) in r.iter_mut().enumerate() {
            let next = a.get(i + 1).copied().unwrap_or(0);
            *x = (((u64::from(next) << 32) | u64::from(a[i])) >> (s % 32)) as u32;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bignum::gcd::{GCD_DC, HGCD, Mat, gcd_dc, gcd_lehmer, nabi_nat_gcd_scratch};

    /// A heap buffer laid out like a `GcBuf<u32>` (header + limbs).
    fn buf(limbs: &[u32], len: usize) -> Vec<u64> {
        let mut v = vec![0u64; (HEADER_SIZE + 4 * len.max(limbs.len())).div_ceil(8) + 1];
        let p = v.as_mut_ptr().cast::<u8>();
        for (i, &l) in limbs.iter().enumerate() {
            // SAFETY: in bounds of `v`.
            unsafe { p.add(HEADER_SIZE).cast::<u32>().add(i).write(l) };
        }
        v
    }

    fn read(v: &mut [u64], n: usize) -> Vec<u32> {
        // SAFETY: `v` came from `buf` with room for `n` limbs.
        unsafe { limbs(v.as_mut_ptr().cast(), n).to_vec() }
    }

    fn num(l: &[u32]) -> u128 {
        l.iter()
            .rev()
            .fold(0u128, |acc, &x| (acc << 32) | u128::from(x))
    }

    /// xorshift64*; all-ones limbs now and then (carry chains).
    fn rand_limbs(seed: &mut u64, n: usize) -> Vec<L> {
        (0..n)
            .map(|_| {
                *seed ^= *seed >> 12;
                *seed ^= *seed << 25;
                *seed ^= *seed >> 27;
                let v = seed.wrapping_mul(0x2545_F491_4F6C_DD1D);
                if v % 7 == 0 { L::MAX } else { v as L }
            })
            .collect()
    }

    #[test]
    fn mul_matches_schoolbook() {
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let sizes = [
            1,
            2,
            17,
            KARA - 1,
            KARA,
            KARA + 1,
            2 * KARA + 3,
            KARA_SQR,
            TOOM3 - 1,
            TOOM3,
            TOOM3 + 1,
            TOOM3_SQR,
            TOOM3_SQR + 1,
            3 * TOOM3 + 2,
            700,
            NTT - 1,
            NTT,
            NTT_SQR + 1,
            2 * NTT + 7,
        ];
        for &n in &sizes {
            for &m in &sizes {
                if m > n {
                    continue;
                }
                let a = rand_limbs(&mut seed, n);
                let b = rand_limbs(&mut seed, m);
                let mut want = vec![0; n + m];
                mul_basecase(&mut want, &a, &b);
                let mut got = vec![0; n + m];
                let mut t = vec![0; scratch_bound(n, m)];
                mul(&mut got, &a, &b, &mut t);
                assert_eq!(got, want, "{n} x {m}");
            }
            let a = rand_limbs(&mut seed, n);
            let mut want = vec![0; 2 * n];
            mul_basecase(&mut want, &a, &a);
            let mut got = vec![0; 2 * n];
            let mut t = vec![0; scratch_bound(n, n)];
            sqr(&mut got, &a, &mut t);
            assert_eq!(got, want, "{n}^2");
        }
    }

    /// Binary gcd on `L` vectors (reference).
    fn gcd_ref(mut a: Vec<L>, mut b: Vec<L>) -> Vec<L> {
        let n = a.len().max(b.len());
        a.resize(n, 0);
        b.resize(n, 0);
        let zero = |v: &[L]| v.iter().all(|&x| x == 0);
        let even = |v: &[L]| v[0] & 1 == 0;
        let less =
            |x: &[L], y: &[L]| x.iter().rev().cmp(y.iter().rev()) == core::cmp::Ordering::Less;
        if zero(&a) {
            return b;
        }
        let mut k = 0;
        while even(&a) && even(&b) && !zero(&b) {
            shr_in(&mut a, 1);
            shr_in(&mut b, 1);
            k += 1;
        }
        while !zero(&b) {
            while even(&a) {
                shr_in(&mut a, 1);
            }
            while !zero(&b) && even(&b) {
                shr_in(&mut b, 1);
            }
            if zero(&b) {
                break;
            }
            if less(&b, &a) {
                core::mem::swap(&mut a, &mut b);
            }
            sub_in(&mut b, &a);
        }
        a.push(0);
        for _ in 0..k {
            shl1_in(&mut a);
        }
        a
    }

    /// (B^k - 1)^2 = B^2k - 2 B^k + 1 (`u32` digits): every coefficient
    /// of the convolution at its maximum, the products and squares
    /// through the NTT (`k` u32 limbs).
    #[test]
    fn ntt_max_coefficients() {
        let k = 1 << 16;
        let a = vec![L::MAX; k / PER];
        let mut want = vec![0u32; 2 * k];
        want[0] = 1;
        want[k] = 0xFFFF_FFFE;
        want[k + 1..].fill(u32::MAX);
        let mut t = vec![0; ntt_scratch(a.len(), a.len())];
        for square in [false, true] {
            let mut r = vec![0; 2 * a.len()];
            ntt_mul(&mut r, &a, if square { &[] } else { &a }, &mut t);
            assert_eq!(digits32(&r), &want[..], "square {square}");
        }
    }

    #[test]
    fn ntt_prepared_matches_mul() {
        let mut seed = 0xA076_1D64_78BD_642Fu64;
        let wn = 1 << 13;
        let mut w = vec![0u32; 6 * wn];
        ntt_roots(&mut w, wn);
        for &(an, bn, n) in &[
            (1usize, 1usize, 4usize),
            (1500, 1400, 4096),
            (2000, 2096, 4096),
            (3000, 2000, 8192),
        ] {
            let a = digits32(&rand_limbs(&mut seed, an.div_ceil(PER)))[..an].to_vec();
            let b = digits32(&rand_limbs(&mut seed, bn.div_ceil(PER)))[..bn].to_vec();
            let mut f = vec![0u32; 3 * n];
            ntt_forward(&mut f, n, &b, &w, wn);
            let mut t = vec![0u32; 3 * n];
            ntt_forward(&mut t, n, &a, &w, wn);
            let s = ntt_finish(&mut t, n, &f, &w, wn);
            let mut got = vec![0u32; an + bn];
            garner(&mut got, &mut t, n, s, false);
            // Schoolbook on u32 digits.
            let mut want = vec![0u32; an + bn];
            for (i, &x) in a.iter().enumerate() {
                let mut c = 0u64;
                for (j, &y) in b.iter().enumerate() {
                    let v = u64::from(x) * u64::from(y) + u64::from(want[i + j]) + c;
                    want[i + j] = v as u32;
                    c = v >> 32;
                }
                want[i + bn] = c as u32;
            }
            assert_eq!(got, want, "{an} x {bn} in {n}");
        }
    }

    /// `a * b mod (B^m - 1)` (`u32` digits) by schoolbook and folding.
    fn mulmod_ref(a: &[u32], b: &[u32], m: usize) -> Vec<u32> {
        let mut p = vec![0u32; a.len() + b.len()];
        for (i, &x) in a.iter().enumerate() {
            let mut c = 0u64;
            for (j, &y) in b.iter().enumerate() {
                let v = u64::from(x) * u64::from(y) + u64::from(p[i + j]) + c;
                p[i + j] = v as u32;
                c = v >> 32;
            }
            p[i + b.len()] = c as u32;
        }
        let mut r = vec![0u32; m];
        for chunk in p.chunks(m) {
            let mut c = 0u128;
            for (x, &v) in r.iter_mut().zip(chunk) {
                let s = u128::from(*x) + u128::from(v) + c;
                *x = s as u32;
                c = s >> 32;
            }
            for x in &mut r[chunk.len()..] {
                let s = u128::from(*x) + c;
                *x = s as u32;
                c = s >> 32;
            }
            wrap_in(&mut r, c);
        }
        wrap_in(&mut r, 0);
        r
    }

    #[test]
    fn mulmod_matches_folded_product() {
        let mut seed = 0x3C6E_F372_FE94_F82Bu64;
        let big = NTT * PER;
        for &(an, bn, n) in &[
            (1usize, 1usize, 1usize),
            (3, 2, 3),
            (40, 30, 50),
            (200, 150, 202),
            (big, big, big + 2),
            (big + 5, big - 9, big),
            (2 * big, big, 2 * big + 1),
            (4096, 4096, 4096),
            (4096, 4000, 4096),
            (4096, 1, 4096),
        ] {
            let m = nabi_nat_mulmod_len(an, bn, n);
            assert!(m >= n);
            for square in [false, true] {
                if square && an != bn {
                    continue;
                }
                let a = digits32(&rand_limbs(&mut seed, an.div_ceil(PER)))[..an].to_vec();
                let b = if square {
                    a.clone()
                } else {
                    digits32(&rand_limbs(&mut seed, bn.div_ceil(PER)))[..bn].to_vec()
                };
                let (mut ab, mut bb, mut r) = (buf(&a, an), buf(&b, bn), buf(&[], m));
                let mut t = buf(&[], nabi_nat_mulmod_scratch(an, bn, m));
                let bp = if square {
                    ab.as_mut_ptr()
                } else {
                    bb.as_mut_ptr()
                };
                // SAFETY: test buffers sized above.
                unsafe {
                    nabi_nat_mulmod(
                        r.as_mut_ptr().cast(),
                        ab.as_mut_ptr().cast(),
                        an,
                        bp.cast(),
                        bn,
                        m,
                        t.as_mut_ptr().cast(),
                    );
                }
                assert_eq!(
                    read(&mut r, m),
                    mulmod_ref(&a, &b, m),
                    "{an} x {bn} mod {m}"
                );
            }
        }
        // All ones squared: B^m - 1 = 0 comes out as 0.
        let m = 1 << 12;
        let a = vec![u32::MAX; m];
        assert_eq!(nabi_nat_mulmod_len(m, m, m), m);
        let (mut ab, mut r) = (buf(&a, m), buf(&[], m));
        let mut t = buf(&[], nabi_nat_mulmod_scratch(m, m, m));
        // SAFETY: as above.
        unsafe {
            let p = ab.as_mut_ptr().cast();
            nabi_nat_mulmod(r.as_mut_ptr().cast(), p, m, p, m, m, t.as_mut_ptr().cast());
        }
        assert!(read(&mut r, m).iter().all(|&x| x == 0));
    }

    #[test]
    fn ntt_prepared_wraps() {
        let mut seed = 0x1B87_3593_8BD5_4F2Fu64;
        let n = 1 << 12;
        let mut w = vec![0u32; 6 * n];
        ntt_roots(&mut w, n);
        for &(an, bn) in &[(n, n), (3000, n), (n, 1500), (2100, 2100)] {
            let a = digits32(&rand_limbs(&mut seed, an.div_ceil(PER)))[..an].to_vec();
            let b = digits32(&rand_limbs(&mut seed, bn.div_ceil(PER)))[..bn].to_vec();
            let (mut fb, mut bb) = (buf(&[], 3 * n), buf(&b, bn));
            let (mut ab, mut wb) = (buf(&a, an), buf(&w, 6 * n));
            let rn = n.min(an + bn);
            let (mut r, mut t) = (buf(&[], rn), buf(&[], 3 * n));
            // SAFETY: test buffers sized above.
            unsafe {
                nabi_nat_ntt_prep(
                    fb.as_mut_ptr().cast(),
                    bb.as_mut_ptr().cast(),
                    bn,
                    n,
                    wb.as_mut_ptr().cast(),
                    n,
                );
                nabi_nat_ntt_mul(
                    r.as_mut_ptr().cast(),
                    ab.as_mut_ptr().cast(),
                    an,
                    fb.as_mut_ptr().cast(),
                    bn,
                    n,
                    wb.as_mut_ptr().cast(),
                    n,
                    t.as_mut_ptr().cast(),
                );
            }
            let mut want = mulmod_ref(&a, &b, n);
            want.truncate(rn);
            assert_eq!(read(&mut r, rn), want, "{an} x {bn} mod {n}");
        }
    }

    #[test]
    fn gcd_matches_binary_gcd() {
        let mut seed = 0x5DEE_CE66_D1CE_4E5Bu64;
        for &(n, m, gl) in &[
            (1, 1, 1),
            (2, 1, 1),
            (2, 2, 1),
            (3, 2, 2),
            (5, 5, 3),
            (9, 4, 4),
            (17, 16, 6),
            (30, 30, 1),
        ] {
            for _ in 0..30 {
                // Shared factor g, cofactors without the all-ones bias.
                let g = rand_limbs(&mut seed, gl);
                let mut a = vec![0; n + gl];
                let mut b = vec![0; m + gl];
                let (ca, cb) = (rand_limbs(&mut seed, n), rand_limbs(&mut seed, m));
                mul_basecase(&mut a, &ca, &g);
                if m >= gl {
                    mul_basecase(&mut b, &cb, &g);
                } else {
                    mul_basecase(&mut b, &g, &cb);
                }
                let mut want = gcd_ref(a.clone(), b.clone());
                let (a, b) = if gcd_ref_less(&a, &b) { (b, a) } else { (a, b) };
                let w = a.len();
                let mut x = a.clone();
                let mut y = b.clone();
                y.resize(w, 0);
                let mut t = vec![0; 3 * w + 1];
                let (gv, gn) = gcd_lehmer(&mut x, &mut y, &mut t);
                let mut got = gv[..gn].to_vec();
                got.resize(want.len().max(gn), 0);
                want.resize(got.len(), 0);
                assert_eq!(got, want, "{n} {m} {gl}");
            }
        }
    }

    /// `gcd(a, b)` (`a >= b`) through `nabi_nat_gcd`'s path and through
    /// Lehmer alone.
    fn gcd_both(a: &[L], b: &[L]) -> (Vec<L>, Vec<L>) {
        let w = a.len();
        let run = |dc: bool| {
            let (mut x, mut y) = (a.to_vec(), b.to_vec());
            y.resize(w, 0);
            let mut t = vec![0; nabi_nat_gcd_scratch(w * PER, w * PER) / PER];
            if dc {
                gcd_dc(&mut x, &mut y, &mut t);
            }
            let (g, gn) = gcd_lehmer(&mut x, &mut y, &mut t);
            g[..gn].to_vec()
        };
        (run(true), run(false))
    }

    #[test]
    fn hgcd_matches_lehmer() {
        let mut seed = 0x6A09_E667_F3BC_C909u64;
        for &(n, m, gl) in &[
            (GCD_DC, GCD_DC, 1),
            (GCD_DC + 7, GCD_DC - 3, 40),
            (GCD_DC + 2 * HGCD + 5, GCD_DC + 2 * HGCD, 3),
            (2000, 1999, 600),
            (2500, 2500, 1),
            (3000, GCD_DC, 700),
            (5000, 5000, 9),
        ] {
            // Shared factor g; all-ones limbs now and then.
            let g = rand_limbs(&mut seed, gl);
            let (ca, cb) = (rand_limbs(&mut seed, n - gl), rand_limbs(&mut seed, m - gl));
            let mut a = vec![0; n];
            let mut b = vec![0; m];
            mul(&mut a, &ca, &g, &mut vec![0; scratch_bound(n, gl)]);
            mul(&mut b, &cb, &g, &mut vec![0; scratch_bound(m, gl)]);
            let (a, b) = if gcd_ref_less(&a, &b) { (b, a) } else { (a, b) };
            let (got, want) = gcd_both(&a, &b);
            assert_eq!(got, want, "{n} {m} {gl}");
            assert!(want.len() >= gl.min(2) || gl == 1);
        }
        // Consecutive Fibonacci numbers: every quotient 1.
        let n = 2 * GCD_DC;
        let (mut x, mut y) = (vec![0 as L; n], vec![0 as L; n]);
        y[0] = 1;
        while x[n - 1] >> (LB - 2) == 0 {
            add_in(&mut x, &y);
            core::mem::swap(&mut x, &mut y);
        }
        let (got, want) = gcd_both(&y, &x);
        assert_eq!((got.as_slice(), want.as_slice()), (&[1][..], &[1][..]));
        // a = b q + 1 with a huge quotient, and b | a.
        let b = rand_limbs(&mut seed, GCD_DC + 50);
        let q = rand_limbs(&mut seed, 2 * GCD_DC);
        let mut a = vec![0; b.len() + q.len()];
        mul(
            &mut a,
            &q,
            &b,
            &mut vec![0; scratch_bound(q.len(), b.len())],
        );
        let (got, want) = gcd_both(&a, &b);
        assert_eq!(got, want);
        add_in(&mut a, &[1]);
        let (got, want) = gcd_both(&a, &b);
        assert_eq!(got, want);
    }

    /// M N for entries all B^k - 1: each a sum of two full products,
    /// 2 (B^k - 1)^2, one limb past them (both product paths).
    #[test]
    fn mat_mul_carries() {
        for k in [3, NTT / 2 + 1] {
            let cap = 2 * k + 2;
            let mut ones = vec![L::MAX; k];
            ones.resize(cap, 0);
            let (mut sm, mut sn) = (vec![0; 4 * cap], vec![0; 4 * cap]);
            let (mut m, _) = Mat::new(&mut sm, cap);
            let (mut n, _) = Mat::new(&mut sn, cap);
            for e in m.e.iter_mut().chain(n.e.iter_mut()) {
                e.copy_from_slice(&ones);
            }
            (m.len, n.len) = (k, k);
            let mut t = vec![0; 64 * cap + scratch_bound(cap, cap)];
            m.mul(&n, &mut t);
            // 2 (B^k - 1)^2 = B^2k + (B^k - 4) B^k + 2.
            let mut want = vec![0 as L; cap];
            want[0] = 2;
            want[k] = L::MAX - 3;
            want[k + 1..2 * k].fill(L::MAX);
            want[2 * k] = 1;
            for e in &m.e {
                assert_eq!(&e[..], &want[..], "k {k}");
            }
        }
    }

    fn gcd_ref_less(x: &[L], y: &[L]) -> bool {
        let n = x.len().max(y.len());
        (0..n)
            .rev()
            .map(|i| {
                (
                    x.get(i).copied().unwrap_or(0),
                    y.get(i).copied().unwrap_or(0),
                )
            })
            .find(|(p, q)| p != q)
            .is_some_and(|(p, q)| p < q)
    }

    #[test]
    fn knuth_d_recovers_quotient() {
        let mut seed = 0x0123_4567_89AB_CDEFu64;
        for &(nq, nv) in &[
            (1, 1),
            (5, 1),
            (1, 2),
            (7, 2),
            (3, 9),
            (40, 17),
            (17, 40),
            (64, 64),
        ] {
            for _ in 0..20 {
                let q = rand_limbs(&mut seed, nq);
                let mut v = rand_limbs(&mut seed, nv);
                v[nv - 1] |= 1 << (LB - 1);
                let mut r = rand_limbs(&mut seed, nv);
                r[nv - 1] &= v[nv - 1] - 1;
                // u = q v + r in nq + nv + 1 limbs.
                let mut u = vec![0; nq + nv + 1];
                mul_basecase(
                    &mut u[..nq + nv],
                    if nq >= nv { &q } else { &v },
                    if nq >= nv { &v } else { &q },
                );
                add_in(&mut u, &r);
                let mut got = vec![0; nq + 1];
                knuth_d(&mut got, &mut u, &v);
                assert_eq!(&got[..nq], &q[..], "{nq} / {nv}");
                assert_eq!(got[nq], 0);
                assert_eq!(&u[..nv], &r[..]);
            }
        }
    }

    #[test]
    fn mul_divmod_round_trip() {
        let a = [0x9ABC_DEF7u32];
        let b = [0x8000_0001u32, 0xFFFF_FFFF];
        let (mut ab, mut bb, mut r) = (buf(&a, 1), buf(&b, 2), buf(&[], 3));
        let mut t = buf(&[], nabi_nat_mul_scratch(1, 2));
        // SAFETY: test buffers sized above.
        unsafe {
            nabi_nat_mul(
                r.as_mut_ptr().cast(),
                ab.as_mut_ptr().cast(),
                1,
                bb.as_mut_ptr().cast(),
                2,
                t.as_mut_ptr().cast(),
            );
        }
        let prod = read(&mut r, 3);
        assert_eq!(num(&prod), num(&a) * num(&b));
        // (prod + 5) / b == a, remainder 5 (`b` not normalized).
        let b = [5u32, 0x0000_0FFF];
        let bb = buf(&b, 2);
        let mut rb = buf(&[], 3);
        let mut t = buf(&[], nabi_nat_mul_scratch(1, 2));
        // SAFETY: as above.
        unsafe {
            nabi_nat_mul(
                rb.as_mut_ptr().cast(),
                ab.as_mut_ptr().cast(),
                1,
                bb.as_ptr().cast_mut().cast(),
                2,
                t.as_mut_ptr().cast(),
            );
        }
        let mut prod = read(&mut rb, 3);
        prod[0] += 5;
        let mut un = buf(&prod, 3);
        let (mut q, mut rem) = (buf(&[], 2), buf(&[], 2));
        let mut t = buf(&[], nabi_nat_divmod_scratch(3, 2));
        let mut bb = bb;
        // SAFETY: as above.
        unsafe {
            nabi_nat_divmod(
                q.as_mut_ptr().cast(),
                rem.as_mut_ptr().cast(),
                un.as_mut_ptr().cast(),
                3,
                bb.as_mut_ptr().cast(),
                2,
                t.as_mut_ptr().cast(),
            );
        }
        assert_eq!(num(&read(&mut q, 2)), num(&a));
        assert_eq!(num(&read(&mut rem, 2)), 5);
        let mut sq = buf(&[], 1);
        // SAFETY: as above.
        let rem =
            unsafe { nabi_nat_divmod_small(sq.as_mut_ptr().cast(), ab.as_mut_ptr().cast(), 1, 10) };
        assert_eq!(num(&read(&mut sq, 1)) * 10 + u128::from(rem), num(&a));
    }
}
