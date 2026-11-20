//! Ed25519 signature verification (RFC 8032), verify only.
//!
//! The field and group code follows `TweetNaCl`: GF(2^255-19) elements as
//! sixteen 16-bit limbs in i64, points in extended coordinates. Every
//! input here is public (a release signature, a public key, a message),
//! so nothing needs to be constant time. A signature is accepted only
//! when S < L and the encoding of [S]B - [k]A equals R byte for byte, so
//! a non-canonical R never matches.

// p, q, a..h, x, y, z are the formulas' names.
#![allow(clippy::many_single_char_names)]

use crate::sign::sha512::sha512;

type Gf = [i64; 16];

const GF0: Gf = [0; 16];
const GF1: Gf = [1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
/// d = -121665/121666.
#[rustfmt::skip]
const D: Gf = [
    0x78a3, 0x1359, 0x4dca, 0x75eb, 0xd8ab, 0x4141, 0x0a4d, 0x0070,
    0xe898, 0x7779, 0x4079, 0x8cc7, 0xfe73, 0x2b6f, 0x6cee, 0x5203,
];
/// 2d.
#[rustfmt::skip]
const D2: Gf = [
    0xf159, 0x26b2, 0x9b94, 0xebd6, 0xb156, 0x8283, 0x149a, 0x00e0,
    0xd130, 0xeef3, 0x80f2, 0x198e, 0xfce7, 0x56df, 0xd9dc, 0x2406,
];
/// The base point's x.
#[rustfmt::skip]
const X: Gf = [
    0xd51a, 0x8f25, 0x2d60, 0xc956, 0xa7b2, 0x9525, 0xc760, 0x692c,
    0xdc5c, 0xfdd6, 0xe231, 0xc0a4, 0x53fe, 0xcd6e, 0x36d3, 0x2169,
];
/// The base point's y = 4/5.
#[rustfmt::skip]
const Y: Gf = [
    0x6658, 0x6666, 0x6666, 0x6666, 0x6666, 0x6666, 0x6666, 0x6666,
    0x6666, 0x6666, 0x6666, 0x6666, 0x6666, 0x6666, 0x6666, 0x6666,
];
/// sqrt(-1).
#[rustfmt::skip]
const I: Gf = [
    0xa0b0, 0x4a0e, 0x1b27, 0xc4ee, 0xe478, 0xad2f, 0x1806, 0x2f43,
    0xd7a7, 0x3dfb, 0x0099, 0x2b4d, 0xdf0b, 0x4fc1, 0x2480, 0x2b83,
];
/// The group order L = 2^252 + 27742317777372353535851937790883648493,
/// little endian.
#[rustfmt::skip]
const L: [i64; 32] = [
    0xed, 0xd3, 0xf5, 0x5c, 0x1a, 0x63, 0x12, 0x58, 0xd6, 0x9c, 0xf7, 0xa2, 0xde, 0xf9, 0xde, 0x14,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x10,
];

fn car(o: &mut Gf) {
    for i in 0..16 {
        o[i] += 1 << 16;
        let c = o[i] >> 16;
        if i < 15 {
            o[i + 1] += c - 1;
        } else {
            o[0] += 38 * (c - 1);
        }
        o[i] -= c << 16;
    }
}

/// Swap `p` and `q` when `b` is 1.
fn sel(p: &mut Gf, q: &mut Gf, b: i64) {
    let c = !(b - 1);
    for i in 0..16 {
        let t = c & (p[i] ^ q[i]);
        p[i] ^= t;
        q[i] ^= t;
    }
}

/// The canonical 32-byte encoding of `n`.
fn pack(n: &Gf) -> [u8; 32] {
    let mut t = *n;
    car(&mut t);
    car(&mut t);
    car(&mut t);
    let mut m = GF0;
    for _ in 0..2 {
        m[0] = t[0] - 0xffed;
        for i in 1..15 {
            m[i] = t[i] - 0xffff - ((m[i - 1] >> 16) & 1);
            m[i - 1] &= 0xffff;
        }
        m[15] = t[15] - 0x7fff - ((m[14] >> 16) & 1);
        let b = (m[15] >> 16) & 1;
        m[14] &= 0xffff;
        sel(&mut t, &mut m, 1 - b);
    }
    let mut o = [0u8; 32];
    for i in 0..16 {
        o[2 * i] = (t[i] & 0xff) as u8;
        o[2 * i + 1] = (t[i] >> 8) as u8;
    }
    o
}

fn neq(a: &Gf, b: &Gf) -> bool {
    pack(a) != pack(b)
}

fn parity(a: &Gf) -> u8 {
    pack(a)[0] & 1
}

/// The low 255 bits of `n`.
fn unpack(n: &[u8; 32]) -> Gf {
    let mut o = GF0;
    for i in 0..16 {
        o[i] = i64::from(n[2 * i]) + (i64::from(n[2 * i + 1]) << 8);
    }
    o[15] &= 0x7fff;
    o
}

fn add(a: &Gf, b: &Gf) -> Gf {
    let mut o = GF0;
    for i in 0..16 {
        o[i] = a[i] + b[i];
    }
    o
}

fn sub(a: &Gf, b: &Gf) -> Gf {
    let mut o = GF0;
    for i in 0..16 {
        o[i] = a[i] - b[i];
    }
    o
}

fn mul(a: &Gf, b: &Gf) -> Gf {
    let mut t = [0i64; 31];
    for i in 0..16 {
        for j in 0..16 {
            t[i + j] += a[i] * b[j];
        }
    }
    for i in 0..15 {
        t[i] += 38 * t[i + 16];
    }
    let mut o = GF0;
    o.copy_from_slice(&t[..16]);
    car(&mut o);
    car(&mut o);
    o
}

fn sq(a: &Gf) -> Gf {
    mul(a, a)
}

/// a^(p-2).
fn inv(a: &Gf) -> Gf {
    let mut c = *a;
    for i in (0..=253).rev() {
        c = sq(&c);
        if i != 2 && i != 4 {
            c = mul(&c, a);
        }
    }
    c
}

/// a^((p-5)/8).
fn pow2523(a: &Gf) -> Gf {
    let mut c = *a;
    for i in (0..=250).rev() {
        c = sq(&c);
        if i != 1 {
            c = mul(&c, a);
        }
    }
    c
}

type Point = [Gf; 4];

/// p += q.
fn padd(p: &mut Point, q: &Point) {
    let a = mul(&sub(&p[1], &p[0]), &sub(&q[1], &q[0]));
    let b = mul(&add(&p[0], &p[1]), &add(&q[0], &q[1]));
    let c = mul(&mul(&p[3], &q[3]), &D2);
    let d = mul(&p[2], &q[2]);
    let d = add(&d, &d);
    let e = sub(&b, &a);
    let f = sub(&d, &c);
    let g = add(&d, &c);
    let h = add(&b, &a);
    p[0] = mul(&e, &f);
    p[1] = mul(&h, &g);
    p[2] = mul(&g, &f);
    p[3] = mul(&e, &h);
}

fn cswap(p: &mut Point, q: &mut Point, b: i64) {
    for i in 0..4 {
        sel(&mut p[i], &mut q[i], b);
    }
}

fn encode(p: &Point) -> [u8; 32] {
    let zi = inv(&p[2]);
    let tx = mul(&p[0], &zi);
    let ty = mul(&p[1], &zi);
    let mut r = pack(&ty);
    r[31] ^= parity(&tx) << 7;
    r
}

/// [s]q, s a 32-byte little-endian scalar.
fn scalarmult(q: &Point, s: &[u8; 32]) -> Point {
    let mut p = [GF0, GF1, GF1, GF0];
    let mut q = *q;
    for i in (0..256).rev() {
        let b = i64::from((s[i / 8] >> (i & 7)) & 1);
        cswap(&mut p, &mut q, b);
        let pp = p;
        padd(&mut q, &pp);
        padd(&mut p, &pp);
        cswap(&mut p, &mut q, b);
    }
    p
}

fn scalarbase(s: &[u8; 32]) -> Point {
    scalarmult(&[X, Y, GF1, mul(&X, &Y)], s)
}

/// -A from its encoding; `None` when it is not a point.
fn decode_neg(p: &[u8; 32]) -> Option<Point> {
    let z = GF1;
    let y = unpack(p);
    // y >= p is not an encoding.
    let mut canon = *p;
    canon[31] &= 0x7f;
    if pack(&y) != canon {
        return None;
    }
    let num = sq(&y);
    let den = mul(&num, &D);
    let num = sub(&num, &z);
    let den = add(&z, &den);
    let den2 = sq(&den);
    let den4 = sq(&den2);
    let den6 = mul(&den4, &den2);
    let mut t = mul(&mul(&den6, &num), &den);
    t = pow2523(&t);
    t = mul(&mul(&mul(&t, &num), &den), &den);
    let mut x = mul(&t, &den);
    if neq(&mul(&sq(&x), &den), &num) {
        x = mul(&x, &I);
    }
    if neq(&mul(&sq(&x), &den), &num) {
        return None;
    }
    if parity(&x) == p[31] >> 7 {
        x = sub(&GF0, &x);
    }
    Some([x, y, z, mul(&x, &y)])
}

/// `x` mod L into 32 bytes.
fn mod_l(x: &mut [i64; 64]) -> [u8; 32] {
    for i in (32..64).rev() {
        let mut carry = 0;
        let mut j = i - 32;
        while j < i - 12 {
            x[j] += carry - 16 * x[i] * L[j - (i - 32)];
            carry = (x[j] + 128) >> 8;
            x[j] -= carry << 8;
            j += 1;
        }
        x[j] += carry;
        x[i] = 0;
    }
    let mut carry = 0;
    for j in 0..32 {
        x[j] += carry - (x[31] >> 4) * L[j];
        carry = x[j] >> 8;
        x[j] &= 255;
    }
    for j in 0..32 {
        x[j] -= carry * L[j];
    }
    let mut r = [0u8; 32];
    for i in 0..32 {
        x[i + 1] += x[i] >> 8;
        r[i] = (x[i] & 255) as u8;
    }
    r
}

/// A 64-byte hash reduced mod L.
fn reduce(h: &[u8; 64]) -> [u8; 32] {
    let mut x = [0i64; 64];
    for (x, b) in x.iter_mut().zip(h) {
        *x = i64::from(*b);
    }
    mod_l(&mut x)
}

/// Whether the little-endian scalar `s` is below L.
fn canonical(s: &[u8; 32]) -> bool {
    for i in (0..32).rev() {
        let l = L[i] as u8;
        if s[i] != l {
            return s[i] < l;
        }
    }
    false
}

/// Whether `sig` is `pk`'s Ed25519 signature of `msg`.
#[must_use]
pub fn verify(pk: &[u8; 32], msg: &[u8], sig: &[u8; 64]) -> bool {
    let mut r = [0u8; 32];
    let mut s = [0u8; 32];
    r.copy_from_slice(&sig[..32]);
    s.copy_from_slice(&sig[32..]);
    if !canonical(&s) {
        return false;
    }
    let Some(neg_a) = decode_neg(pk) else {
        return false;
    };
    let k = reduce(&sha512(&[&r, pk, msg]));
    let mut p = scalarmult(&neg_a, &k);
    padd(&mut p, &scalarbase(&s));
    encode(&p) == r
}

/// The public key and signature of `msg` for secret seed `seed`; for
/// tests only (`sign::testing`), never built into a release.
#[cfg(any(test, feature = "test-sign"))]
pub fn sign(seed: &[u8; 32], msg: &[u8]) -> ([u8; 32], [u8; 64]) {
    let mut d = [0u8; 32];
    let h = sha512(&[seed]);
    d.copy_from_slice(&h[..32]);
    d[0] &= 248;
    d[31] &= 127;
    d[31] |= 64;
    let pk = encode(&scalarbase(&d));
    let r = reduce(&sha512(&[&h[32..], msg]));
    let big_r = encode(&scalarbase(&r));
    let k = reduce(&sha512(&[&big_r, &pk, msg]));
    let mut x = [0i64; 64];
    for (x, r) in x.iter_mut().zip(r) {
        *x = i64::from(r);
    }
    for i in 0..32 {
        for j in 0..32 {
            x[i + j] += i64::from(k[i]) * i64::from(d[j]);
        }
    }
    let s = mod_l(&mut x);
    let mut sig = [0u8; 64];
    sig[..32].copy_from_slice(&big_r);
    sig[32..].copy_from_slice(&s);
    (pk, sig)
}

#[cfg(test)]
mod tests {
    #[test]
    fn constants() {
        // d = -121665/121666, I^2 = -1, the base point's y = 4/5.
        let n = |v: i64| {
            let mut g = super::GF0;
            g[0] = v & 0xffff;
            g[1] = v >> 16;
            g
        };
        let d = super::mul(
            &super::sub(&super::GF0, &n(121_665)),
            &super::inv(&n(121_666)),
        );
        assert!(!super::neq(&d, &super::D));
        assert!(!super::neq(&super::add(&d, &d), &super::D2));
        let m1 = super::sq(&super::I);
        assert!(!super::neq(&m1, &super::sub(&super::GF0, &super::GF1)));
        let y = super::mul(&n(4), &super::inv(&n(5)));
        assert!(!super::neq(&y, &super::Y));
    }

    /// The signer the tests use makes RFC 8032's test 2.
    #[test]
    fn sign_rfc8032() {
        let seed: [u8; 32] = core::array::from_fn(|i| {
            u8::from_str_radix(
                &"4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb"
                    [2 * i..2 * i + 2],
                16,
            )
            .unwrap()
        });
        let (pk, sig) = super::sign(&seed, &[0x72]);
        assert_eq!(
            nabic_core::hash::hex(&pk),
            "3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c"
        );
        assert_eq!(
            nabic_core::hash::hex(&sig),
            "92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da\
             085ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c00"
        );
        assert!(super::verify(&pk, &[0x72], &sig));
    }

    #[test]
    fn rejects_s_at_or_above_l() {
        let mut l = [0u8; 32];
        for (b, v) in l.iter_mut().zip(super::L) {
            *b = v as u8;
        }
        assert!(!super::canonical(&l));
        l[0] -= 1;
        assert!(super::canonical(&l));
    }
}
