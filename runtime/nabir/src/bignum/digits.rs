//! Decimal (radix) output of small numbers.

#[allow(clippy::wildcard_imports)]
use crate::bignum::*;

/// Writes the digits of `a[0..n]` (`n <= 64`) in base `radix` ending at
/// `out[end]` (exclusive), zero-padded to `width`; the start. Chunks of
/// the largest power of `radix` in an `L`, by single-limb division with
/// a reciprocal.
///
/// # Safety
/// Live `GcBuf`s: `out` (`u8`) of at least `end` bytes with room for the
/// digits, `a` (`u32`) of at least `n` limbs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nabi_nat_put_digits(
    out: *mut u8,
    end: usize,
    a: *mut u8,
    n: usize,
    width: usize,
    radix: u32,
) -> usize {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    // SAFETY: caller contract.
    let (out, a) = unsafe {
        (
            slice::from_raw_parts_mut(out.add(HEADER_SIZE), end),
            &*limbs(a, n),
        )
    };
    let mut buf = [0 as L; 64 / PER];
    let mut wn = packed(n);
    if wn > 0 {
        pack(&mut buf[..wn], a);
    }
    let rx = L::from(radix);
    let (mut chunk, mut per) = (rx, 1usize);
    while let Some(c) = chunk.checked_mul(rx) {
        chunk = c;
        per += 1;
    }
    let s = chunk.leading_zeros();
    let dn = chunk << s;
    let inv = reciprocal(dn);
    let mut at = end;
    while wn > 0 && buf[wn - 1] == 0 {
        wn -= 1;
    }
    while wn > 0 {
        let mut rem: L = 0;
        for x in buf[..wn].iter_mut().rev() {
            // (rem B + x) / chunk as (that << s) / (chunk << s).
            let n1 = if s == 0 {
                rem
            } else {
                (rem << s) | (*x >> (LB - s))
            };
            let (q, r) = div2by1(n1, *x << s, dn, inv);
            *x = q;
            rem = r >> s;
        }
        while wn > 0 && buf[wn - 1] == 0 {
            wn -= 1;
        }
        // A full chunk unless it is the leading one.
        let mut k = 0;
        while k < per && (wn > 0 || rem != 0) {
            at -= 1;
            out[at] = DIGITS[(rem % rx) as usize];
            rem /= rx;
            k += 1;
        }
    }
    while end - at < width {
        at -= 1;
        out[at] = b'0';
    }
    at
}
