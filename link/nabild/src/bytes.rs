//! Fixed-width field helpers.

pub fn u16le(b: &[u8], at: usize) -> u16 {
    b.get(at..at + 2)
        .map_or(0, |s| u16::from_le_bytes([s[0], s[1]]))
}

pub fn u32le(b: &[u8], at: usize) -> u32 {
    b.get(at..at + 4)
        .map_or(0, |s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

pub fn u64le(b: &[u8], at: usize) -> u64 {
    b.get(at..at + 8).map_or(0, |s| {
        u64::from_le_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]])
    })
}

pub fn u16be(b: &[u8], at: usize) -> u16 {
    b.get(at..at + 2)
        .map_or(0, |s| u16::from_be_bytes([s[0], s[1]]))
}

pub fn u32be(b: &[u8], at: usize) -> u32 {
    b.get(at..at + 4)
        .map_or(0, |s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

pub fn u64be(b: &[u8], at: usize) -> u64 {
    b.get(at..at + 8).map_or(0, |s| {
        u64::from_be_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]])
    })
}

/// The byte order of a file's fields (ELF: `EI_DATA`).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Endian {
    pub big: bool,
}

impl Endian {
    pub const LITTLE: Self = Self { big: false };

    #[must_use]
    pub fn u16(self, b: &[u8], at: usize) -> u16 {
        if self.big { u16be(b, at) } else { u16le(b, at) }
    }

    #[must_use]
    pub fn u32(self, b: &[u8], at: usize) -> u32 {
        if self.big { u32be(b, at) } else { u32le(b, at) }
    }

    #[must_use]
    pub fn u64(self, b: &[u8], at: usize) -> u64 {
        if self.big { u64be(b, at) } else { u64le(b, at) }
    }

    pub fn put16(self, b: &mut [u8], at: usize, v: u16) {
        let x = if self.big {
            v.to_be_bytes()
        } else {
            v.to_le_bytes()
        };
        b[at..at + 2].copy_from_slice(&x);
    }

    pub fn put32(self, b: &mut [u8], at: usize, v: u32) {
        let x = if self.big {
            v.to_be_bytes()
        } else {
            v.to_le_bytes()
        };
        b[at..at + 4].copy_from_slice(&x);
    }

    pub fn put64(self, b: &mut [u8], at: usize, v: u64) {
        let x = if self.big {
            v.to_be_bytes()
        } else {
            v.to_le_bytes()
        };
        b[at..at + 8].copy_from_slice(&x);
    }
}

pub fn put32(b: &mut Vec<u8>, v: u32) {
    b.extend_from_slice(&v.to_le_bytes());
}

pub fn put64(b: &mut Vec<u8>, v: u64) {
    b.extend_from_slice(&v.to_le_bytes());
}

/// A NUL-terminated string at `at`, lossy.
pub fn cstr(b: &[u8], at: usize) -> &str {
    let s = b.get(at..).unwrap_or_default();
    let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    std::str::from_utf8(&s[..end]).unwrap_or("")
}

/// A fixed-width name field, NUL padded.
pub fn name16(b: &[u8], at: usize) -> &str {
    let s = b.get(at..at + 16).unwrap_or_default();
    let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    std::str::from_utf8(&s[..end]).unwrap_or("")
}
