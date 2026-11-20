//! Core wasm modules: what the componentizer reads of them (types,
//! imports, exports, custom sections), the sections kept as bytes, and
//! the small modules it writes itself.

use crate::component::bin::{self, Reader, Res};

pub const FUNC: u8 = 0;
pub const TABLE: u8 = 1;
pub const MEMORY: u8 = 2;
pub const GLOBAL: u8 = 3;

/// A function type: parameter and result value types, one byte each.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FuncType {
    pub params: Vec<u8>,
    pub results: Vec<u8>,
}

impl FuncType {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(0x60);
        bin::usize(out, self.params.len());
        out.extend_from_slice(&self.params);
        bin::usize(out, self.results.len());
        out.extend_from_slice(&self.results);
    }
}

#[derive(Clone, Debug)]
pub enum ImportKind {
    Func(u32),
    /// Table, memory, global or tag: its kind byte.
    Other(u8),
}

#[derive(Clone, Debug)]
pub struct Import {
    pub module: String,
    pub name: String,
    pub kind: ImportKind,
}

#[derive(Clone, Debug)]
pub struct Export {
    pub name: String,
    pub kind: u8,
    pub index: u32,
}

#[derive(Debug)]
pub struct Module<'a> {
    pub bytes: &'a [u8],
    /// Every section: id, contents.
    pub sections: Vec<(u8, &'a [u8])>,
    pub types: Vec<FuncType>,
    pub imports: Vec<Import>,
    /// Type index of each function the module defines.
    pub funcs: Vec<u32>,
    pub exports: Vec<Export>,
    pub customs: Vec<(&'a str, &'a [u8])>,
}

impl<'a> Module<'a> {
    pub fn parse(bytes: &'a [u8]) -> Res<Self> {
        if bytes.len() < 8 || &bytes[..4] != b"\0asm" {
            return Err("not a wasm module".to_owned());
        }
        if bytes[4..8] != [1, 0, 0, 0] {
            return Err("not a core wasm module (a component?)".to_owned());
        }
        let mut m = Self {
            bytes,
            sections: Vec::new(),
            types: Vec::new(),
            imports: Vec::new(),
            funcs: Vec::new(),
            exports: Vec::new(),
            customs: Vec::new(),
        };
        let mut r = Reader::new(bytes);
        r.pos = 8;
        while !r.eof() {
            let id = r.u8()?;
            let n = r.u32()? as usize;
            let body = r.bytes(n)?;
            m.sections.push((id, body));
            let mut s = Reader::new(body);
            match id {
                0 => {
                    let name = s.name()?;
                    m.customs.push((name, &body[s.pos..]));
                }
                1 => {
                    for _ in 0..s.u32()? {
                        m.types.push(func_type(&mut s)?);
                    }
                }
                2 => {
                    for _ in 0..s.u32()? {
                        let module = s.name()?.to_owned();
                        let name = s.name()?.to_owned();
                        let k = s.u8()?;
                        let kind = if k == FUNC {
                            ImportKind::Func(s.u32()?)
                        } else {
                            skip_extern_type(&mut s, k)?;
                            ImportKind::Other(k)
                        };
                        m.imports.push(Import { module, name, kind });
                    }
                }
                3 => {
                    for _ in 0..s.u32()? {
                        m.funcs.push(s.u32()?);
                    }
                }
                7 => {
                    for _ in 0..s.u32()? {
                        let name = s.name()?.to_owned();
                        let kind = s.u8()?;
                        let index = s.u32()?;
                        m.exports.push(Export { name, kind, index });
                    }
                }
                _ => {}
            }
        }
        Ok(m)
    }

    /// Number of imported functions.
    pub fn func_imports(&self) -> u32 {
        self.imports
            .iter()
            .filter(|i| matches!(i.kind, ImportKind::Func(_)))
            .count() as u32
    }

    /// Type of function `f` (imports first).
    pub fn func_type(&self, f: u32) -> Option<&FuncType> {
        let ty = self
            .imports
            .iter()
            .filter_map(|i| match i.kind {
                ImportKind::Func(t) => Some(t),
                ImportKind::Other(_) => None,
            })
            .chain(self.funcs.iter().copied())
            .nth(f as usize)?;
        self.types.get(ty as usize)
    }

    pub fn export(&self, name: &str) -> Option<&Export> {
        self.exports.iter().find(|e| e.name == name)
    }

    /// The function exported as `name`.
    pub fn export_func(&self, name: &str) -> Option<u32> {
        self.export(name)
            .filter(|e| e.kind == FUNC)
            .map(|e| e.index)
    }

    /// The module without the custom sections `drop` rejects.
    pub fn without_customs(&self, drop: impl Fn(&str) -> bool) -> Vec<u8> {
        let mut out = self.bytes[..8].to_vec();
        for (id, body) in &self.sections {
            if *id == 0 {
                let mut s = Reader::new(body);
                if s.name().is_ok_and(&drop) {
                    continue;
                }
            }
            bin::section(&mut out, *id, body);
        }
        out
    }
}

pub fn func_type(s: &mut Reader<'_>) -> Res<FuncType> {
    if s.u8()? != 0x60 {
        return Err("unsupported core type (only function types are)".to_owned());
    }
    let n = s.u32()? as usize;
    let params = s.bytes(n)?.to_vec();
    let n = s.u32()? as usize;
    let results = s.bytes(n)?.to_vec();
    Ok(FuncType { params, results })
}

pub fn skip_limits(s: &mut Reader<'_>) -> Res<()> {
    let flags = s.u8()?;
    if flags & 4 != 0 {
        s.u64()?;
        if flags & 1 != 0 {
            s.u64()?;
        }
    } else {
        s.u32()?;
        if flags & 1 != 0 {
            s.u32()?;
        }
    }
    if flags & 8 != 0 {
        s.u32()?;
    }
    Ok(())
}

fn skip_extern_type(s: &mut Reader<'_>, kind: u8) -> Res<()> {
    match kind {
        TABLE => {
            s.u8()?;
            skip_limits(s)
        }
        MEMORY => skip_limits(s),
        GLOBAL => {
            s.u8()?;
            s.u8()?;
            Ok(())
        }
        4 => {
            s.u8()?;
            s.u32()?;
            Ok(())
        }
        _ => Err(format!("unknown import kind {kind}")),
    }
}

/// A module of `n` functions of `types[i]` each forwarding through
/// table `$imports`, filled later (`fixup`): imports that need what
/// another instance exports, before it exists.
pub fn shim(types: &[FuncType]) -> Vec<u8> {
    let (tys, of) = dedup(types);
    let mut out = b"\0asm\x01\0\0\0".to_vec();
    let mut b = Vec::new();
    for t in &tys {
        t.encode(&mut b);
    }
    bin::vec_section(&mut out, 1, tys.len(), &b);
    b.clear();
    for &t in &of {
        bin::u32(&mut b, t);
    }
    bin::vec_section(&mut out, 3, of.len(), &b);
    b = vec![0x70, 1];
    bin::usize(&mut b, types.len());
    bin::usize(&mut b, types.len());
    bin::vec_section(&mut out, 4, 1, &b);
    b.clear();
    for i in 0..types.len() {
        bin::name(&mut b, &i.to_string());
        b.push(FUNC);
        bin::usize(&mut b, i);
    }
    bin::name(&mut b, "$imports");
    b.push(TABLE);
    b.push(0);
    bin::vec_section(&mut out, 7, types.len() + 1, &b);
    b.clear();
    for (i, t) in types.iter().enumerate() {
        let mut body = vec![0];
        for p in 0..t.params.len() {
            body.push(0x20);
            bin::usize(&mut body, p);
        }
        body.push(0x41);
        bin::s64(&mut body, i as i64);
        body.push(0x11);
        bin::u32(&mut body, of[i]);
        body.push(0);
        body.push(0x0b);
        bin::usize(&mut b, body.len());
        b.extend_from_slice(&body);
    }
    bin::vec_section(&mut out, 10, types.len(), &b);
    out
}

/// The module filling a shim's table with the functions it imports.
pub fn fixup(types: &[FuncType]) -> Vec<u8> {
    let (tys, of) = dedup(types);
    let mut out = b"\0asm\x01\0\0\0".to_vec();
    let mut b = Vec::new();
    for t in &tys {
        t.encode(&mut b);
    }
    bin::vec_section(&mut out, 1, tys.len(), &b);
    b.clear();
    for (i, &t) in of.iter().enumerate() {
        bin::name(&mut b, "");
        bin::name(&mut b, &i.to_string());
        b.push(FUNC);
        bin::u32(&mut b, t);
    }
    bin::name(&mut b, "");
    bin::name(&mut b, "$imports");
    b.extend_from_slice(&[TABLE, 0x70, 1]);
    bin::usize(&mut b, types.len());
    bin::usize(&mut b, types.len());
    bin::vec_section(&mut out, 2, types.len() + 1, &b);
    b = vec![0, 0x41, 0, 0x0b];
    bin::usize(&mut b, types.len());
    for i in 0..types.len() {
        bin::usize(&mut b, i);
    }
    bin::vec_section(&mut out, 9, 1, &b);
    out
}

/// A module whose start function is its one import: runs `_initialize`
/// once everything it may call is in place.
pub fn starter() -> Vec<u8> {
    let mut out = b"\0asm\x01\0\0\0".to_vec();
    bin::vec_section(&mut out, 1, 1, &[0x60, 0, 0]);
    bin::vec_section(&mut out, 2, 1, &[0, 0, FUNC, 0]);
    bin::section(&mut out, 8, &[0]);
    out
}

/// Distinct types of `types`, and each one's index among them.
fn dedup(types: &[FuncType]) -> (Vec<FuncType>, Vec<u32>) {
    let mut tys: Vec<FuncType> = Vec::new();
    let of = types
        .iter()
        .map(|t| {
            let i = tys.iter().position(|u| u == t).unwrap_or_else(|| {
                tys.push(t.clone());
                tys.len() - 1
            });
            i as u32
        })
        .collect();
    (tys, of)
}
