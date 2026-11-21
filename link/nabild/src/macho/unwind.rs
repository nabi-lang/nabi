//! Unwind information: `__unwind_info` and `__eh_frame`.
//!
//! `__compact_unwind` entries and `__eh_frame` CIEs and FDEs read from
//! objects are written out as `__unwind_info` (version 1, regular
//! second-level pages) and a `__eh_frame` of the live FDEs, which
//! DWARF-mode entries point into.

use crate::bytes::u32le;
use crate::macho::object::{ObjFile, SymRef, addr_target, inline, raw_relocs, sym_target};
use crate::macho::{Link, a64};
use crate::symtab::{AtomId, Def, Target};
use nabic_core::HashMap;
use nabic_core::leb::{read_sleb, read_uleb};

/// `UNWIND_*_MODE_MASK`.
const MODE_MASK: u32 = 0x0f00_0000;
const PERSONALITY_MASK: u32 = 0x3000_0000;
const HAS_LSDA: u32 = 0x4000_0000;
const DWARF_OFFSET: u32 = 0x00ff_ffff;
/// Entries per regular second-level page (8 + 8 * n bytes fit 4 KiB).
const PAGE_ENTRIES: usize = 500;

#[derive(Debug)]
pub struct CompactEntry {
    pub func: (AtomId, u64),
    pub enc: u32,
    pub personality: Option<Target>,
    pub lsda: Option<(Target, i64)>,
}

/// A pointer field of an `__eh_frame` record.
#[derive(Clone, Copy, Debug)]
pub struct Field {
    /// Offset in the record.
    pub at: usize,
    pub size: u8,
    pub pcrel: bool,
    pub target: (Target, i64),
}

#[derive(Debug)]
pub struct Cie {
    pub bytes: Vec<u8>,
    /// The personality: a GOT slot of the symbol (indirect), or the
    /// pointer itself.
    pub personality: Option<(usize, Target, bool)>,
}

#[derive(Debug)]
pub struct Fde {
    pub cie: u32,
    pub bytes: Vec<u8>,
    pub func: (AtomId, u64),
    pub pc: Field,
    pub lsda: Option<Field>,
}

/// Pointer encodings (`DW_EH_PE_*`): the field size, and whether pcrel.
const fn enc_size(enc: u8) -> Option<(u8, bool)> {
    let size = match enc & 0x0f {
        0x00 | 0x04 | 0x0c => 8,
        0x03 | 0x0b => 4,
        0x02 | 0x0a => 2,
        _ => return None,
    };
    match enc & 0x70 {
        0x00 => Some((size, false)),
        0x10 => Some((size, true)),
        _ => None,
    }
}

/// What a relocation at an `__eh_frame` offset says.
enum EhRel {
    /// `plus - minus + inline`.
    Diff {
        plus: u32,
        minus: u32,
    },
    Got {
        sym: u32,
    },
    Abs {
        sym: u32,
    },
}

/// The function a (resolved) target is in, if this file defines it.
fn own_atom(link: &Link, fi: u32, t: (Target, i64)) -> Option<(AtomId, u64)> {
    let (a, off) = match t.0 {
        Target::Atom(a) => (a, t.1),
        Target::Sym(id) => match link.syms.get(id).def {
            Def::Defined { atom, off } => (atom, off as i64 + t.1),
            _ => return None,
        },
    };
    (link.atoms[a as usize].file == fi && off >= 0).then_some((a, off as u64))
}

/// Read `__compact_unwind` (section `k`).
pub fn read_compact(
    link: &mut Link,
    fi: u32,
    d: &[u8],
    file: &ObjFile,
    k: usize,
    refs: &[SymRef],
    name: &str,
) -> Result<(), String> {
    let s = &file.sects[k];
    let rel: HashMap<u32, (bool, u32)> = raw_relocs(d, s)
        .iter()
        .filter(|r| r.ty == a64::UNSIGNED)
        .map(|r| (r.addr, (r.ext, r.sym)))
        .collect();
    let base = s.offset as usize;
    let field = |off: u32| -> Result<Option<(Target, i64)>, String> {
        let inl = inline(d, base + off as usize, 3);
        match rel.get(&off) {
            Some(&(true, sym)) => sym_target(refs, sym, inl, name).map(Some),
            Some(&(false, sect)) => addr_target(file, sect, inl as u64, name).map(Some),
            None => Ok(None),
        }
    };
    for i in 0..(s.size / 32) as u32 {
        let off = i * 32;
        let Some(func) = field(off)? else {
            continue;
        };
        let Some(func) = own_atom(link, fi, func) else {
            continue;
        };
        let enc = u32le(d, base + off as usize + 12);
        let personality = field(off + 16)?.map(|t| t.0);
        let lsda = field(off + 24)?;
        let atom = &mut link.atoms[func.0 as usize];
        if let Some(p) = personality {
            atom.keep.push(p);
        }
        if let Some((l, _)) = lsda {
            atom.keep.push(l);
        }
        link.compact.push(CompactEntry {
            func,
            enc,
            personality,
            lsda,
        });
    }
    Ok(())
}

/// Read `__eh_frame` (section `k`); `addrs` are the symbols' object
/// addresses.
#[allow(clippy::too_many_arguments)]
pub fn read_eh_frame(
    link: &mut Link,
    fi: u32,
    d: &[u8],
    file: &ObjFile,
    k: usize,
    refs: &[SymRef],
    addrs: &[Option<u64>],
    name: &str,
) -> Result<(), String> {
    let s = &file.sects[k];
    let sec = d
        .get(s.offset as usize..(u64::from(s.offset) + s.size) as usize)
        .ok_or_else(|| format!("{name}: __eh_frame out of bounds"))?;
    let mut rels: HashMap<u32, EhRel> = HashMap::default();
    let mut minus = None;
    for r in raw_relocs(d, s) {
        match r.ty {
            a64::SUBTRACTOR => minus = Some(r.sym),
            a64::UNSIGNED if r.ext => {
                let e = match minus.take() {
                    Some(m) => EhRel::Diff {
                        plus: r.sym,
                        minus: m,
                    },
                    None => EhRel::Abs { sym: r.sym },
                };
                rels.insert(r.addr, e);
            }
            a64::POINTER_TO_GOT => {
                rels.insert(r.addr, EhRel::Got { sym: r.sym });
            }
            _ => {
                return Err(format!(
                    "{name}: relocation type {} in __eh_frame is not supported",
                    r.ty
                ));
            }
        }
    }
    let err = |what: &str, at: usize| format!("{name}: __eh_frame at {at:#x}: {what}");
    // A pointer field at section offset `q`: its target.
    let target = |q: usize, size: u8, pcrel: bool| -> Result<Option<(Target, i64)>, String> {
        let inl = inline(sec, q, if size == 8 { 3 } else { 2 });
        let field_addr = s.addr + q as u64;
        let at_addr = |a: u64| -> Result<Option<(Target, i64)>, String> {
            file.locate(a)
                .map(|(atom, off)| Some((Target::Atom(atom), off as i64)))
                .ok_or_else(|| err("pointer to no atom", q))
        };
        match rels.get(&(q as u32)) {
            Some(EhRel::Diff { plus, minus }) => {
                let m = addrs
                    .get(*minus as usize)
                    .copied()
                    .flatten()
                    .ok_or_else(|| err("difference from an undefined symbol", q))?;
                // value = plus - minus + inline; the target of a pcrel
                // field is field + value.
                let shift = if pcrel {
                    field_addr as i64 - m as i64
                } else {
                    -(m as i64)
                };
                match addrs.get(*plus as usize).copied().flatten() {
                    Some(p) => at_addr((p as i64 + inl + shift) as u64),
                    None => sym_target(refs, *plus, inl + shift, name).map(Some),
                }
            }
            Some(EhRel::Abs { sym }) => sym_target(refs, *sym, inl, name).map(Some),
            Some(EhRel::Got { .. }) => Err(err("GOT pointer outside a personality", q)),
            None if inl == 0 && !pcrel => Ok(None),
            None if pcrel => at_addr((field_addr as i64 + inl) as u64),
            None => at_addr(inl as u64),
        }
    };
    let mut cies: HashMap<usize, (u32, u8, u8, bool)> = HashMap::default();
    let mut at = 0usize;
    while at + 4 <= sec.len() {
        let len = u32le(sec, at) as usize;
        if len == 0 {
            break;
        }
        if len == 0xffff_ffff {
            return Err(err("64-bit records are not supported", at));
        }
        let end = at + 4 + len;
        if end > sec.len() {
            return Err(err("record runs past the section", at));
        }
        let rec = &sec[at..end];
        let id = u32le(sec, at + 4);
        if id == 0 {
            // CIE: version, augmentation, alignment factors, return
            // register, augmentation data.
            let mut p = at + 9;
            let aug = crate::bytes::cstr(sec, p).to_owned();
            p += aug.len() + 1;
            read_uleb(sec, &mut p);
            read_sleb(sec, &mut p);
            if sec[at + 8] == 1 {
                p += 1;
            } else {
                read_uleb(sec, &mut p);
            }
            let (mut fde_enc, mut lsda_enc, mut personality) = (0u8, 0xffu8, None);
            if aug.starts_with('z') {
                read_uleb(sec, &mut p);
                for c in aug.chars().skip(1) {
                    match c {
                        'P' => {
                            let enc = sec[p];
                            p += 1;
                            let (size, pcrel) =
                                enc_size(enc).ok_or_else(|| err("personality encoding", p))?;
                            let indirect = enc & 0x80 != 0;
                            let t = match rels.get(&(p as u32)) {
                                Some(EhRel::Got { sym }) => sym_target(refs, *sym, 0, name)?.0,
                                _ if indirect => {
                                    return Err(err(
                                        "indirect personality without a GOT relocation",
                                        p,
                                    ));
                                }
                                _ => {
                                    target(p, size, pcrel)?
                                        .ok_or_else(|| err("null personality", p))?
                                        .0
                                }
                            };
                            personality = Some((p - at, t, indirect));
                            p += size as usize;
                        }
                        'L' => {
                            lsda_enc = sec[p];
                            p += 1;
                        }
                        'R' => {
                            fde_enc = sec[p];
                            p += 1;
                        }
                        'S' | 'B' => {}
                        _ => return Err(err(&format!("augmentation {aug:?}"), at)),
                    }
                }
            }
            cies.insert(
                at,
                (
                    link.cies.len() as u32,
                    fde_enc,
                    lsda_enc,
                    aug.starts_with('z'),
                ),
            );
            link.cies.push(Cie {
                bytes: rec.to_vec(),
                personality,
            });
        } else {
            // The CIE pointer: field - CIE. Some ld -r output and the
            // assembler's across atoms spell it as a SUBTRACTOR pair
            // whose inline part is only the addend.
            let ptr = match rels.get(&((at + 4) as u32)) {
                None => i64::from(id),
                Some(EhRel::Diff { plus, minus }) => {
                    let sym = |i: u32| {
                        addrs
                            .get(i as usize)
                            .copied()
                            .flatten()
                            .ok_or_else(|| err("CIE pointer from an undefined symbol", at))
                    };
                    sym(*plus)? as i64 - sym(*minus)? as i64 + i64::from(id as i32)
                }
                Some(_) => return Err(err("CIE pointer relocation", at)),
            };
            let cie_at =
                usize::try_from((at + 4) as i64 - ptr).map_err(|_| err("CIE pointer", at))?;
            let &(cie, fde_enc, lsda_enc, z) = cies
                .get(&cie_at)
                .ok_or_else(|| err("FDE before its CIE", at))?;
            let (size, pcrel) = enc_size(fde_enc).ok_or_else(|| err("FDE pointer encoding", at))?;
            let q = at + 8;
            let Some(func) = target(q, size, pcrel)? else {
                at = end;
                continue;
            };
            let mut p = q + 2 * size as usize;
            let mut lsda = None;
            if z {
                read_uleb(sec, &mut p);
                if lsda_enc != 0xff {
                    let (lsize, lpcrel) =
                        enc_size(lsda_enc).ok_or_else(|| err("LSDA encoding", p))?;
                    if let Some(t) = target(p, lsize, lpcrel)? {
                        lsda = Some(Field {
                            at: p - at,
                            size: lsize,
                            pcrel: lpcrel,
                            target: t,
                        });
                    }
                }
            }
            // An FDE of a function another file's definition replaced.
            if let Some(f) = own_atom(link, fi, func) {
                let atom = &mut link.atoms[f.0 as usize];
                if let Some(l) = lsda {
                    atom.keep.push(l.target.0);
                }
                if let Some((_, p, _)) = link.cies[cie as usize].personality {
                    atom.keep.push(p);
                }
                link.fdes.push(Fde {
                    cie,
                    bytes: rec.to_vec(),
                    func: f,
                    pc: Field {
                        at: q - at,
                        size,
                        pcrel,
                        target: func,
                    },
                    lsda,
                });
            }
        }
        at = end;
    }
    Ok(())
}

/// One `__unwind_info` entry.
#[derive(Clone, Copy, Debug)]
struct Entry {
    at: (AtomId, u64),
    enc: u32,
    personality: Option<Target>,
    lsda: Option<(Target, i64)>,
}

/// What goes into `__unwind_info` and `__eh_frame`, sized before layout.
#[derive(Default)]
pub struct Plan {
    entries: Vec<Entry>,
    personalities: Vec<Target>,
    lsdas: usize,
    /// Live records, CIEs before their FDEs: `(is CIE, index, offset)`.
    eh: Vec<(bool, u32, u32)>,
    pub eh_size: u64,
    pub info_size: u64,
}

impl Plan {
    /// Symbols and atoms whose GOT slots hold personalities.
    #[must_use]
    pub fn got_targets(&self, link: &Link) -> Vec<Target> {
        let mut v = self.personalities.clone();
        for &(cie, i, _) in &self.eh {
            if cie {
                if let Some((_, t, true)) = link.cies[i as usize].personality {
                    v.push(t);
                }
            }
        }
        v
    }
}

/// Collect the unwind entries of the live `code` atoms (every function
/// gets one, so ranges end where the next function starts).
pub fn plan(link: &Link, code: &[AtomId], dwarf_mode: u32) -> Result<Plan, String> {
    let mut p = Plan::default();
    // eh_frame: live FDEs, each CIE once before its first.
    let mut cie_off: HashMap<u32, u32> = HashMap::default();
    let mut fde_of: HashMap<(AtomId, u64), u32> = HashMap::default();
    let mut size = 0u32;
    for (i, f) in link.fdes.iter().enumerate() {
        if !link.atoms[f.func.0 as usize].live {
            continue;
        }
        if let std::collections::hash_map::Entry::Vacant(e) = cie_off.entry(f.cie) {
            e.insert(size);
            p.eh.push((true, f.cie, size));
            size += link.cies[f.cie as usize].bytes.len() as u32;
        }
        fde_of.entry(f.func).or_insert(size);
        p.eh.push((false, i as u32, size));
        size += f.bytes.len() as u32;
    }
    p.eh_size = u64::from(size);
    let mut by_at: HashMap<(AtomId, u64), Entry> = HashMap::default();
    for &a in code {
        by_at.insert(
            (a, 0),
            Entry {
                at: (a, 0),
                enc: 0,
                personality: None,
                lsda: None,
            },
        );
    }
    for (&at, &off) in &fde_of {
        if let Some(e) = by_at.get_mut(&at) {
            e.enc = dwarf_mode | off;
        } else if link.atoms[at.0 as usize].live {
            by_at.insert(
                at,
                Entry {
                    at,
                    enc: dwarf_mode | off,
                    personality: None,
                    lsda: None,
                },
            );
        }
    }
    for c in &link.compact {
        if !link.atoms[c.func.0 as usize].live {
            continue;
        }
        let mut e = Entry {
            at: c.func,
            enc: c.enc,
            personality: c.personality,
            lsda: c.lsda,
        };
        if e.enc & MODE_MASK == dwarf_mode {
            // The FDE carries personality and LSDA.
            e.enc = match fde_of.get(&c.func) {
                Some(&off) if off <= DWARF_OFFSET => dwarf_mode | off,
                Some(_) => return Err("__eh_frame is too large for compact unwind".to_owned()),
                None => 0,
            };
            e.personality = None;
            e.lsda = None;
        }
        by_at.insert(c.func, e);
    }
    let mut entries: Vec<Entry> = by_at.into_values().collect();
    entries.sort_by_key(|e| (e.at.0, e.at.1));
    for e in &mut entries {
        if let Some(t) = e.personality {
            let k = if let Some(k) = p.personalities.iter().position(|&x| x == t) {
                k
            } else {
                if p.personalities.len() == 3 {
                    return Err("more than three personality routines".to_owned());
                }
                p.personalities.push(t);
                p.personalities.len() - 1
            };
            e.enc = (e.enc & !PERSONALITY_MASK) | ((k as u32 + 1) << 28);
        }
        if e.lsda.is_some() {
            e.enc |= HAS_LSDA;
            p.lsdas += 1;
        } else {
            e.enc &= !HAS_LSDA;
        }
    }
    let pages = entries.len().div_ceil(PAGE_ENTRIES);
    p.info_size = (28
        + 4 * p.personalities.len()
        + 12 * (pages + 1)
        + 8 * p.lsdas
        + pages * 8
        + entries.len() * 8) as u64;
    p.entries = entries;
    Ok(p)
}

/// Address of an atom-relative target.
fn addr_of(link: &Link, t: (Target, i64)) -> Option<u64> {
    let (a, off) = link.syms.resolve(t.0)?;
    Some((link.atoms[a as usize].addr as i64 + off as i64 + t.1) as u64)
}

/// `__eh_frame` at `base`: the planned records with their pointers
/// rewritten. `got` gives a symbol's GOT slot.
pub fn write_eh_frame(
    p: &Plan,
    link: &Link,
    base: u64,
    got: &dyn Fn(Target) -> Option<u64>,
) -> Result<(Vec<u8>, Vec<u64>), String> {
    let mut out = vec![0u8; p.eh_size as usize];
    let mut rebases = Vec::new();
    let mut cie_at: HashMap<u32, u32> = HashMap::default();
    let put = |out: &mut [u8], at: usize, size: u8, v: i64| {
        if size == 8 {
            out[at..at + 8].copy_from_slice(&v.to_le_bytes());
        } else if size == 4 {
            out[at..at + 4].copy_from_slice(&(v as i32).to_le_bytes());
        } else {
            out[at..at + 2].copy_from_slice(&(v as i16).to_le_bytes());
        }
    };
    for &(is_cie, i, off) in &p.eh {
        let o = off as usize;
        if is_cie {
            let c = &link.cies[i as usize];
            out[o..o + c.bytes.len()].copy_from_slice(&c.bytes);
            cie_at.insert(i, off);
            if let Some((at, t, indirect)) = c.personality {
                let field = base + (o + at) as u64;
                let to = if indirect {
                    got(t).ok_or("personality without a GOT slot")?
                } else {
                    addr_of(link, (t, 0)).ok_or("personality is not defined")?
                };
                put(&mut out, o + at, 4, to as i64 - field as i64);
            }
        } else {
            let f = &link.fdes[i as usize];
            out[o..o + f.bytes.len()].copy_from_slice(&f.bytes);
            let cie = cie_at[&f.cie];
            out[o + 4..o + 8].copy_from_slice(&(off + 4 - cie).to_le_bytes());
            for fld in std::iter::once(&f.pc).chain(f.lsda.as_ref()) {
                let field = base + (o + fld.at) as u64;
                let to = addr_of(link, fld.target).ok_or("eh_frame pointer to nothing")?;
                if fld.pcrel {
                    put(&mut out, o + fld.at, fld.size, to as i64 - field as i64);
                } else {
                    if fld.size != 8 {
                        return Err("absolute eh_frame pointers must be 8 bytes".to_owned());
                    }
                    put(&mut out, o + fld.at, 8, to as i64);
                    rebases.push(field);
                }
            }
        }
    }
    Ok((out, rebases))
}

/// `__unwind_info` for an image at `image`.
pub fn write_unwind_info(
    p: &Plan,
    link: &Link,
    image: u64,
    got: &dyn Fn(Target) -> Option<u64>,
) -> Result<Vec<u8>, String> {
    let rel = |a: u64| -> Result<u32, String> {
        u32::try_from(a - image).map_err(|_| "image larger than 4 GiB".to_owned())
    };
    let mut ents: Vec<(u32, u32, Option<u32>)> = Vec::with_capacity(p.entries.len());
    for e in &p.entries {
        let f = link.atoms[e.at.0 as usize].addr + e.at.1;
        let lsda = match e.lsda {
            Some(t) => Some(rel(addr_of(link, t).ok_or("LSDA is not defined")?)?),
            None => None,
        };
        ents.push((rel(f)?, e.enc, lsda));
    }
    ents.sort_by_key(|e| e.0);
    let end = p
        .entries
        .iter()
        .map(|e| {
            let a = &link.atoms[e.at.0 as usize];
            a.addr + a.size
        })
        .max()
        .unwrap_or(image);
    let pages: Vec<&[(u32, u32, Option<u32>)]> = ents.chunks(PAGE_ENTRIES).collect();
    let pers_off = 28u32;
    let index_off = pers_off + 4 * p.personalities.len() as u32;
    let lsda_off = index_off + 12 * (pages.len() as u32 + 1);
    let pages_off = lsda_off + 8 * p.lsdas as u32;
    let mut b = Vec::with_capacity(p.info_size as usize);
    for v in [
        1,
        pers_off,
        0,
        pers_off,
        p.personalities.len() as u32,
        index_off,
        pages.len() as u32 + 1,
    ] {
        b.extend_from_slice(&v.to_le_bytes());
    }
    for &t in &p.personalities {
        let slot = got(t).ok_or("personality without a GOT slot")?;
        b.extend_from_slice(&rel(slot)?.to_le_bytes());
    }
    // Index: one entry per page, then the end sentinel.
    let (mut page_at, mut lsda_at) = (pages_off, lsda_off);
    for pg in &pages {
        for v in [pg[0].0, page_at, lsda_at] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        page_at += 8 + 8 * pg.len() as u32;
        lsda_at += 8 * pg.iter().filter(|e| e.2.is_some()).count() as u32;
    }
    for v in [rel(end)?, 0, lsda_at] {
        b.extend_from_slice(&v.to_le_bytes());
    }
    for e in &ents {
        if let Some(l) = e.2 {
            b.extend_from_slice(&e.0.to_le_bytes());
            b.extend_from_slice(&l.to_le_bytes());
        }
    }
    for pg in &pages {
        b.extend_from_slice(&2u32.to_le_bytes());
        b.extend_from_slice(&8u16.to_le_bytes());
        b.extend_from_slice(&(pg.len() as u16).to_le_bytes());
        for e in *pg {
            b.extend_from_slice(&e.0.to_le_bytes());
            b.extend_from_slice(&e.1.to_le_bytes());
        }
    }
    debug_assert_eq!(b.len() as u64, p.info_size);
    Ok(b)
}
