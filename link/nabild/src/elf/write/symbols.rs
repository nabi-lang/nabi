//! String tables, hashes and symbol tables.

#[allow(clippy::wildcard_imports)]
use crate::elf::write::*;

/// A string table under construction.
#[derive(Default)]
pub(crate) struct StrTab {
    pub(crate) data: Vec<u8>,
    pub(crate) at: HashMap<String, u32>,
}

impl StrTab {
    pub(crate) fn new() -> Self {
        Self {
            data: vec![0],
            at: HashMap::default(),
        }
    }

    pub(crate) fn add(&mut self, s: &str) -> u32 {
        if s.is_empty() {
            return 0;
        }
        if let Some(&o) = self.at.get(s) {
            return o;
        }
        let o = self.data.len() as u32;
        self.data.extend_from_slice(s.as_bytes());
        self.data.push(0);
        self.at.insert(s.to_owned(), o);
        o
    }
}

pub(crate) fn gnu_hash(s: &str) -> u32 {
    s.bytes().fold(5381u32, |h, c| {
        h.wrapping_mul(33).wrapping_add(u32::from(c))
    })
}

pub(crate) fn elf_hash(s: &str) -> u32 {
    let mut h = 0u32;
    for c in s.bytes() {
        h = (h << 4).wrapping_add(u32::from(c));
        let g = h & 0xf000_0000;
        if g != 0 {
            h ^= g >> 24;
        }
        h &= !g;
    }
    h
}

/// A dynamic symbol.
pub(crate) struct DynSym {
    pub(crate) id: SymId,
    pub(crate) name: u32,
    /// `.gnu.version` entry.
    pub(crate) ver: u16,
    pub(crate) hash: u32,
}

/// `.symtab` entries (null first) and the first global.
#[allow(clippy::match_same_arms)]
pub(crate) fn symbol_table(out: &Out<'_>, secs: &[OSec], strs: &mut StrTab) -> (Vec<u8>, u32) {
    let en = out.l.arch.endian();
    let l = out.l;
    let mut sec_of: HashMap<(u32, u32), u32> = HashMap::default();
    for (i, s) in secs.iter().enumerate() {
        for &a in &s.atoms {
            let at = &l.atoms[a as usize];
            sec_of.entry((at.file, at.sect)).or_insert(i as u32 + 1);
        }
    }
    let shndx_of = |a: AtomId| {
        let a = l.alias.get(&a).copied().unwrap_or(a);
        let at = &l.atoms[a as usize];
        sec_of.get(&(at.file, at.sect)).copied().unwrap_or(0xfff1) as u16
    };
    let tls_start = out.tls.0;
    let value = |a: AtomId, off: u64, tls: bool| {
        let v = out.atom_addr(a) + off;
        if tls { v - tls_start } else { v }
    };
    let wide = l.arch.elf64();
    let esz = if wide { 24 } else { 16 };
    // POWER keeps the local entry points.
    let entry = |other: u8| {
        if l.arch == Arch::Ppc64 {
            other & 0xe0
        } else {
            0
        }
    };
    let mut v = vec![0u8; esz];
    let ent = |v: &mut Vec<u8>, name: u32, info: u8, other: u8, shndx: u16, val: u64, size: u64| {
        let at = v.len();
        v.resize(at + esz, 0);
        write_sym(en, &mut v[at..], wide, name, info, other, shndx, val, size);
    };
    for f in &l.files {
        for s in &f.locals {
            if !l.atoms[s.atom as usize].live {
                continue;
            }
            let tls = s.ty == STT_TLS;
            let name = strs.add(crate::bytes::cstr(&f.data, s.name));
            ent(
                &mut v,
                name,
                s.ty,
                entry(s.other),
                shndx_of(s.atom),
                value(s.atom, s.off, tls),
                s.size,
            );
        }
    }
    let mut globals = Vec::new();
    for (i, s) in l.syms.syms.iter().enumerate() {
        if s.name.starts_with('\0') || !s.referenced && s.def == Def::Undefined {
            continue;
        }
        let x = l.ext(i as SymId);
        let (shndx, val, live) = match s.def {
            Def::Defined { atom, off } => (
                shndx_of(atom),
                value(atom, off, x.ty == STT_TLS),
                l.atoms[atom as usize].live,
            ),
            Def::Absolute(a) => (0xfff1, a, true),
            Def::Synthetic => {
                let v = out.synth.get(&(i as u32)).copied().unwrap_or(0);
                (
                    0xfff1,
                    if x.ty == STT_TLS { v - tls_start } else { v },
                    true,
                )
            }
            Def::Dylib { .. } if s.referenced => (0, 0, true),
            Def::Undefined if s.referenced => (0, 0, true),
            _ => continue,
        };
        if !live {
            continue;
        }
        let bind = if s.weak_def || (shndx == 0 && s.weak_ref) {
            2
        } else {
            1
        };
        if s.private_extern && shndx != 0 {
            let name = strs.add(&s.name);
            ent(&mut v, name, x.ty, 2 | entry(x.other), shndx, val, x.size);
        } else {
            globals.push((i, bind, shndx, val, x));
        }
    }
    let first = (v.len() / esz) as u32;
    for (i, bind, shndx, val, x) in globals {
        let name = strs.add(&l.syms.syms[i].name);
        let size = if shndx == 0 { 0 } else { x.size };
        let ty = if shndx == 0 && x.ty == STT_TLS {
            STT_TLS
        } else {
            x.ty
        };
        let other = if shndx == 0 { 0 } else { entry(x.other) };
        ent(&mut v, name, (bind << 4) | ty, other, shndx, val, size);
    }
    (v, first)
}

/// A symbol table entry at `b[0..]`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn write_sym(
    en: Endian,
    b: &mut [u8],
    wide: bool,
    name: u32,
    info: u8,
    other: u8,
    shndx: u16,
    value: u64,
    size: u64,
) {
    en.put32(b, 0, name);
    if wide {
        b[4] = info;
        b[5] = other;
        en.put16(b, 6, shndx);
        en.put64(b, 8, value);
        en.put64(b, 16, size);
    } else {
        en.put32(b, 4, value as u32);
        en.put32(b, 8, size as u32);
        b[12] = info;
        b[13] = other;
        en.put16(b, 14, shndx);
    }
}
