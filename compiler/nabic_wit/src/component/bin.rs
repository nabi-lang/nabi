//! Byte-level reading and writing of the wasm binary format: LEB128
//! integers, names, sections.

pub type Res<T> = Result<T, String>;

/// A cursor over wasm bytes.
#[derive(Clone)]
pub struct Reader<'a> {
    pub b: &'a [u8],
    pub pos: usize,
}

impl<'a> Reader<'a> {
    pub const fn new(b: &'a [u8]) -> Self {
        Self { b, pos: 0 }
    }

    pub const fn eof(&self) -> bool {
        self.pos >= self.b.len()
    }

    fn short() -> String {
        "unexpected end of wasm data".to_owned()
    }

    pub fn u8(&mut self) -> Res<u8> {
        let x = *self.b.get(self.pos).ok_or_else(Self::short)?;
        self.pos += 1;
        Ok(x)
    }

    pub fn peek(&self) -> Res<u8> {
        self.b.get(self.pos).copied().ok_or_else(Self::short)
    }

    pub fn bytes(&mut self, n: usize) -> Res<&'a [u8]> {
        let end = self.pos.checked_add(n).ok_or_else(Self::short)?;
        let s = self.b.get(self.pos..end).ok_or_else(Self::short)?;
        self.pos = end;
        Ok(s)
    }

    /// An unsigned LEB128 of at most `bits` bits.
    fn uleb(&mut self, bits: u32) -> Res<u64> {
        let mut r: u64 = 0;
        let mut shift = 0;
        loop {
            let x = self.u8()?;
            if shift >= bits {
                return Err("integer too large in wasm data".to_owned());
            }
            r |= u64::from(x & 0x7f) << shift;
            shift += 7;
            if x & 0x80 == 0 {
                return Ok(r);
            }
        }
    }

    fn sleb(&mut self, bits: u32) -> Res<i64> {
        let mut r: i64 = 0;
        let mut shift = 0;
        loop {
            let x = self.u8()?;
            if shift >= bits {
                return Err("integer too large in wasm data".to_owned());
            }
            r |= i64::from(x & 0x7f) << shift;
            shift += 7;
            if x & 0x80 == 0 {
                if shift < 64 && x & 0x40 != 0 {
                    r |= -1i64 << shift;
                }
                return Ok(r);
            }
        }
    }

    pub fn u32(&mut self) -> Res<u32> {
        self.uleb(32).map(|v| v as u32)
    }

    pub fn u64(&mut self) -> Res<u64> {
        self.uleb(64)
    }

    pub fn s33(&mut self) -> Res<i64> {
        self.sleb(33)
    }

    pub fn s64(&mut self) -> Res<i64> {
        self.sleb(64)
    }

    pub fn name(&mut self) -> Res<&'a str> {
        let n = self.u32()? as usize;
        std::str::from_utf8(self.bytes(n)?)
            .map_err(|_| "invalid UTF-8 name in wasm data".to_owned())
    }

    /// The bytes from `start` to the cursor.
    pub fn since(&self, start: usize) -> &'a [u8] {
        &self.b[start..self.pos]
    }
}

pub fn u32(out: &mut Vec<u8>, mut v: u32) {
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

pub fn s64(out: &mut Vec<u8>, mut v: i64) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if (v == 0 && b & 0x40 == 0) || (v == -1 && b & 0x40 != 0) {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

pub fn usize(out: &mut Vec<u8>, v: usize) {
    u32(out, u32::try_from(v).expect("wasm vector length"));
}

pub fn name(out: &mut Vec<u8>, s: &str) {
    usize(out, s.len());
    out.extend_from_slice(s.as_bytes());
}

/// Section `id` holding `body`.
pub fn section(out: &mut Vec<u8>, id: u8, body: &[u8]) {
    out.push(id);
    usize(out, body.len());
    out.extend_from_slice(body);
}

/// A section that is a vector of `n` items already encoded in `items`.
pub fn vec_section(out: &mut Vec<u8>, id: u8, n: usize, items: &[u8]) {
    let mut body = Vec::with_capacity(items.len() + 5);
    usize(&mut body, n);
    body.extend_from_slice(items);
    section(out, id, &body);
}
