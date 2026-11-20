//! BLAKE2b-512 (RFC 7693), unkeyed: what minisign prehashes with.

// a..d, v, m, t, x, y are the RFC's names.
#![allow(clippy::many_single_char_names)]

use crate::sign::sha512::H0 as IV;

#[rustfmt::skip]
const SIGMA: [[usize; 16]; 10] = [
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
    [14, 10, 4, 8, 9, 15, 13, 6, 1, 12, 0, 2, 11, 7, 5, 3],
    [11, 8, 12, 0, 5, 2, 15, 13, 10, 14, 3, 6, 7, 1, 9, 4],
    [7, 9, 3, 1, 13, 12, 11, 14, 2, 6, 5, 10, 4, 0, 15, 8],
    [9, 0, 5, 7, 2, 4, 10, 15, 14, 1, 11, 12, 6, 8, 3, 13],
    [2, 12, 6, 10, 0, 11, 8, 3, 4, 13, 7, 5, 15, 14, 1, 9],
    [12, 5, 1, 15, 14, 13, 4, 10, 0, 7, 6, 3, 9, 2, 8, 11],
    [13, 11, 7, 14, 12, 1, 3, 9, 5, 0, 15, 4, 8, 6, 2, 10],
    [6, 15, 14, 9, 11, 3, 0, 8, 12, 2, 13, 7, 1, 4, 10, 5],
    [10, 2, 8, 4, 7, 6, 1, 5, 15, 11, 9, 14, 3, 12, 13, 0],
];

fn g(v: &mut [u64; 16], [a, b, c, d]: [usize; 4], x: u64, y: u64) {
    v[a] = v[a].wrapping_add(v[b]).wrapping_add(x);
    v[d] = (v[d] ^ v[a]).rotate_right(32);
    v[c] = v[c].wrapping_add(v[d]);
    v[b] = (v[b] ^ v[c]).rotate_right(24);
    v[a] = v[a].wrapping_add(v[b]).wrapping_add(y);
    v[d] = (v[d] ^ v[a]).rotate_right(16);
    v[c] = v[c].wrapping_add(v[d]);
    v[b] = (v[b] ^ v[c]).rotate_right(63);
}

/// The compression function F over block `m`, `t` bytes in, `last` set
/// on the final block.
fn compress(h: &mut [u64; 8], m: &[u8; 128], t: u128, last: bool) {
    let mut w = [0u64; 16];
    for (i, word) in m.chunks_exact(8).enumerate() {
        w[i] = u64::from_le_bytes(word.try_into().unwrap_or([0; 8]));
    }
    let mut v = [0u64; 16];
    v[..8].copy_from_slice(h);
    v[8..].copy_from_slice(&IV);
    v[12] ^= t as u64;
    v[13] ^= (t >> 64) as u64;
    if last {
        v[14] = !v[14];
    }
    for r in 0..12 {
        let s = &SIGMA[r % 10];
        g(&mut v, [0, 4, 8, 12], w[s[0]], w[s[1]]);
        g(&mut v, [1, 5, 9, 13], w[s[2]], w[s[3]]);
        g(&mut v, [2, 6, 10, 14], w[s[4]], w[s[5]]);
        g(&mut v, [3, 7, 11, 15], w[s[6]], w[s[7]]);
        g(&mut v, [0, 5, 10, 15], w[s[8]], w[s[9]]);
        g(&mut v, [1, 6, 11, 12], w[s[10]], w[s[11]]);
        g(&mut v, [2, 7, 8, 13], w[s[12]], w[s[13]]);
        g(&mut v, [3, 4, 9, 14], w[s[14]], w[s[15]]);
    }
    for i in 0..8 {
        h[i] ^= v[i] ^ v[i + 8];
    }
}

/// `BLAKE2b` with a 64-byte digest and no key.
#[must_use]
pub fn blake2b512(data: &[u8]) -> [u8; 64] {
    let mut h = IV;
    // Parameter block: digest length 64, key length 0, fanout 1, depth 1.
    h[0] ^= 0x0101_0040;
    let mut t: u128 = 0;
    let mut rest = data;
    // Every full block but the last is compressed as it comes.
    while rest.len() > 128 {
        let mut m = [0u8; 128];
        m.copy_from_slice(&rest[..128]);
        t += 128;
        compress(&mut h, &m, t, false);
        rest = &rest[128..];
    }
    let mut m = [0u8; 128];
    m[..rest.len()].copy_from_slice(rest);
    t += rest.len() as u128;
    compress(&mut h, &m, t, true);
    let mut out = [0u8; 64];
    for (o, v) in out.chunks_exact_mut(8).zip(h) {
        o.copy_from_slice(&v.to_le_bytes());
    }
    out
}
