//! LEB128 numbers, as DWARF, wasm and Mach-O write them.

/// Append `v` as an unsigned LEB128.
pub fn uleb(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

/// Append `v` as a signed LEB128.
pub fn sleb(out: &mut Vec<u8>, mut v: i64) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        let done = (v == 0 && b & 0x40 == 0) || (v == -1 && b & 0x40 != 0);
        if done {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

/// The bytes `uleb` writes for `v`.
#[must_use]
pub const fn uleb_len(mut v: u64) -> usize {
    let mut n = 1;
    while v >= 0x80 {
        v >>= 7;
        n += 1;
    }
    n
}

/// `v` as an unsigned LEB128 of exactly `b.len()` bytes (padded), the
/// form relocated fields take. High bits that do not fit are dropped.
pub fn put_uleb(b: &mut [u8], v: u64) {
    let n = b.len();
    for (k, x) in b.iter_mut().enumerate() {
        let more = if k + 1 < n { 0x80 } else { 0 };
        *x = ((v.checked_shr(7 * k as u32).unwrap_or(0)) & 0x7f) as u8 | more;
    }
}

/// `v` as a signed LEB128 of exactly `b.len()` bytes (padded).
pub fn put_sleb(b: &mut [u8], v: i64) {
    let n = b.len();
    let mut x = v;
    for (k, byte) in b.iter_mut().enumerate() {
        let more = if k + 1 < n { 0x80 } else { 0 };
        *byte = (x & 0x7f) as u8 | more;
        x >>= 7;
    }
}

/// Decode an unsigned LEB128 at `*at`, advancing it; `None` past the end.
pub fn read_uleb(b: &[u8], at: &mut usize) -> Option<u64> {
    let (mut v, mut shift) = (0u64, 0);
    loop {
        let byte = *b.get(*at)?;
        *at += 1;
        if shift < 64 {
            v |= u64::from(byte & 0x7f) << shift;
        }
        shift += 7;
        if byte & 0x80 == 0 {
            return Some(v);
        }
    }
}

/// Decode a signed LEB128 at `*at`, advancing it; `None` past the end.
pub fn read_sleb(b: &[u8], at: &mut usize) -> Option<i64> {
    let (mut v, mut shift) = (0i64, 0);
    loop {
        let byte = *b.get(*at)?;
        *at += 1;
        if shift < 64 {
            v |= i64::from(byte & 0x7f) << shift;
        }
        shift += 7;
        if byte & 0x80 == 0 {
            if shift < 64 && byte & 0x40 != 0 {
                v |= -1 << shift;
            }
            return Some(v);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leb_round_trips() {
        for v in [0u64, 1, 127, 128, 300, 0x3fff, 0x4000, u64::MAX] {
            let mut b = Vec::new();
            uleb(&mut b, v);
            assert_eq!(b.len(), uleb_len(v));
            assert_eq!(read_uleb(&b, &mut 0), Some(v));
        }
        for v in [0i64, 1, -1, 63, 64, -64, -65, i64::MIN, i64::MAX] {
            let mut b = Vec::new();
            sleb(&mut b, v);
            assert_eq!(read_sleb(&b, &mut 0), Some(v));
        }
    }

    #[test]
    fn padded_leb_reads_back() {
        for v in [0u64, 1, 127, 128, 0xffff_ffff] {
            let mut b = [0u8; 5];
            put_uleb(&mut b, v);
            assert_eq!(read_uleb(&b, &mut 0), Some(v));
        }
        for v in [0i64, -1, 63, -64, i64::from(i32::MIN), i64::from(i32::MAX)] {
            let mut b = [0u8; 5];
            put_sleb(&mut b, v);
            assert_eq!(read_sleb(&b, &mut 0), Some(v));
        }
        assert_eq!(read_uleb(&[0x80], &mut 0), None);
    }
}
