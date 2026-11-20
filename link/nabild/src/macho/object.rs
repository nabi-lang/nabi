//! Mach-O 64 relocatable objects into atoms: sections split at symbols
//! (`MH_SUBSECTIONS_VIA_SYMBOLS`), relocations onto atoms and symbols,
//! `__compact_unwind` and `__eh_frame` into unwind records.

use crate::bytes::{cstr, name16, u16le, u32le, u64le};
use crate::macho::{Link, a64, unwind};
use crate::symtab::{Atom, AtomId, Def, Fetch, Reloc, SymId, Target};

pub const S_ZEROFILL: u32 = 0x1;
pub const S_CSTRING_LITERALS: u32 = 0x2;
pub const S_MOD_INIT_FUNC_POINTERS: u32 = 0x9;
pub const S_MOD_TERM_FUNC_POINTERS: u32 = 0xa;
pub const S_GB_ZEROFILL: u32 = 0xc;
pub const S_THREAD_LOCAL_REGULAR: u32 = 0x11;
pub const S_THREAD_LOCAL_ZEROFILL: u32 = 0x12;
pub const S_THREAD_LOCAL_VARIABLES: u32 = 0x13;
pub const S_ATTR_PURE_INSTRUCTIONS: u32 = 0x8000_0000;
pub const S_ATTR_NO_DEAD_STRIP: u32 = 0x1000_0000;
pub const S_ATTR_DEBUG: u32 = 0x0200_0000;
pub const S_ATTR_SOME_INSTRUCTIONS: u32 = 0x400;

const N_STAB: u8 = 0xe0;
const N_PEXT: u8 = 0x10;
const N_TYPE: u8 = 0x0e;
const N_EXT: u8 = 0x01;
const N_UNDF: u8 = 0x0;
const N_ABS: u8 = 0x2;
const N_SECT: u8 = 0xe;
const N_INDR: u8 = 0xa;
const N_NO_DEAD_STRIP: u16 = 0x20;
const N_WEAK_REF: u16 = 0x40;
const N_WEAK_DEF: u16 = 0x80;
const N_ALT_ENTRY: u16 = 0x200;
const REFERENCED_DYNAMICALLY: u16 = 0x10;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Kind {
    Atoms,
    CompactUnwind,
    EhFrame,
    Skip,
}

#[derive(Debug)]
pub struct InSect {
    pub seg: String,
    pub name: String,
    pub addr: u64,
    pub size: u64,
    pub offset: u32,
    pub align: u32,
    pub reloff: u32,
    pub nreloc: u32,
    pub flags: u32,
    pub kind: Kind,
    pub atoms: Vec<AtomId>,
    pub atom_offs: Vec<u64>,
}

impl InSect {
    #[must_use]
    pub const fn zerofill(&self) -> bool {
        matches!(
            self.flags & 0xff,
            S_ZEROFILL | S_GB_ZEROFILL | S_THREAD_LOCAL_ZEROFILL
        )
    }

    #[must_use]
    pub const fn code(&self) -> bool {
        self.flags & (S_ATTR_PURE_INSTRUCTIONS | S_ATTR_SOME_INSTRUCTIONS) != 0
    }
}

pub struct ObjFile {
    /// For messages: the path, or `archive(member)`.
    pub name: String,
    /// The debug map's path of the object: absolute, `archive(member)`.
    pub oso: String,
    pub data: Vec<u8>,
    pub sects: Vec<InSect>,
    /// Named local symbols: `(name, atom, offset)`.
    pub locals: Vec<(String, AtomId, u64)>,
    /// `__debug_info`'s first unit names this source file.
    pub source: Option<String>,
    pub debug: bool,
    /// `LC_LINKER_OPTION` arguments (`-lfoo`, `-framework X`).
    pub linker_options: Vec<Vec<String>>,
}

impl ObjFile {
    /// The atom and offset at object address `addr`.
    #[must_use]
    pub fn locate(&self, addr: u64) -> Option<(AtomId, u64)> {
        let s = self.sects.iter().find(|s| {
            s.kind == Kind::Atoms
                && addr >= s.addr
                && addr <= s.addr + s.size
                && !s.atoms.is_empty()
        })?;
        locate_in(s, addr - s.addr)
    }
}

/// The atom of section `s` holding section offset `off`.
#[must_use]
pub fn locate_in(s: &InSect, off: u64) -> Option<(AtomId, u64)> {
    if s.atoms.is_empty() {
        return None;
    }
    let k = match s.atom_offs.binary_search(&off) {
        Ok(k) => k,
        Err(0) => return None,
        Err(k) => k - 1,
    };
    Some((s.atoms[k], off - s.atom_offs[k]))
}

/// A symbol table entry's meaning for relocations.
#[derive(Clone, Copy, Debug)]
pub enum SymRef {
    Global(SymId),
    Local(AtomId, u64),
    Abs(u64),
    None,
}

/// A symbol table entry.
struct Nlist<'a> {
    name: &'a str,
    ty: u8,
    sect: u8,
    desc: u16,
    value: u64,
}

/// The `arch` slice of a universal file, or the file itself.
#[must_use]
pub fn thin_slice<'a>(d: &'a [u8], arch: &str) -> Option<&'a [u8]> {
    let be = |at: usize| {
        d.get(at..at + 4)
            .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
    };
    if be(0)? != 0xcafe_babe {
        return Some(d);
    }
    let want = crate::macho::cpu_type(arch)?;
    for k in 0..be(4)? as usize {
        let e = 8 + k * 20;
        if be(e)? == want {
            let (off, size) = (be(e + 8)? as usize, be(e + 12)? as usize);
            return d.get(off..off + size);
        }
    }
    None
}

/// Read object `data` as file `fi` of `link`: atoms, symbols, relocations
/// and unwind records. Returns the archive members its references fetch.
pub fn parse(
    link: &mut Link,
    fi: u32,
    name: String,
    oso: String,
    data: Vec<u8>,
) -> Result<Vec<Fetch>, String> {
    let d = &data;
    if u32le(d, 0) != 0xfeed_facf {
        return Err(format!("{name}: not a 64-bit Mach-O object"));
    }
    if u32le(d, 12) != 1 {
        return Err(format!("{name}: not a relocatable object"));
    }
    if u32le(d, 4) != link.cpu {
        return Err(format!(
            "{name}: built for another architecture than {}",
            link.arch
        ));
    }
    let subsections = u32le(d, 24) & 0x2000 != 0;
    let ncmds = u32le(d, 16);
    let mut sects = Vec::new();
    let (mut symoff, mut nsyms, mut stroff) = (0usize, 0usize, 0usize);
    let mut linker_options = Vec::new();
    let mut at = 32;
    for _ in 0..ncmds {
        let (cmd, size) = (u32le(d, at), u32le(d, at + 4) as usize);
        match cmd {
            // LC_SEGMENT_64
            0x19 => {
                let n = u32le(d, at + 64) as usize;
                if n.checked_mul(80)
                    .and_then(|b| b.checked_add(at + 72))
                    .is_none_or(|e| e > d.len() || e > at + size)
                {
                    return Err(format!("{name}: section headers past their load command"));
                }
                for k in 0..n {
                    let s = at + 72 + k * 80;
                    let seg = name16(d, s + 16).to_owned();
                    let sname = name16(d, s).to_owned();
                    let flags = u32le(d, s + 64);
                    let kind = if seg == "__LD" && sname == "__compact_unwind" {
                        Kind::CompactUnwind
                    } else if seg == "__TEXT" && sname == "__eh_frame" {
                        Kind::EhFrame
                    } else if seg == "__DWARF"
                        || seg == "__LLVM"
                        || seg == "__LD"
                        || flags & S_ATTR_DEBUG != 0
                    {
                        Kind::Skip
                    } else {
                        Kind::Atoms
                    };
                    sects.push(InSect {
                        seg,
                        name: sname,
                        addr: u64le(d, s + 32),
                        size: u64le(d, s + 40),
                        offset: u32le(d, s + 48),
                        align: u32le(d, s + 52),
                        reloff: u32le(d, s + 56),
                        nreloc: u32le(d, s + 60),
                        flags,
                        kind,
                        atoms: Vec::new(),
                        atom_offs: Vec::new(),
                    });
                }
            }
            // LC_BUILD_VERSION: Apple's linker refuses another platform's
            // code, which would fail at load or run.
            0x32 => {
                let p = u32le(d, at + 8);
                if p != 0 && link.platform != 0 && p != link.platform {
                    return Err(format!(
                        "{name}: built for {}, but this link is for {}; build it for the same \
                         target (`--target`, or clang's `-target {}`)",
                        platform_name(p),
                        platform_name(link.platform),
                        platform_triple(link.platform)
                    ));
                }
            }
            // LC_SYMTAB
            0x2 => {
                symoff = u32le(d, at + 8) as usize;
                nsyms = u32le(d, at + 12) as usize;
                stroff = u32le(d, at + 16) as usize;
            }
            // LC_LINKER_OPTION
            0x2d => {
                let count = u32le(d, at + 8);
                let mut p = at + 12;
                let mut opt = Vec::new();
                for _ in 0..count {
                    if p >= (at + size).min(d.len()) {
                        return Err(format!("{name}: a linker option past its load command"));
                    }
                    let s = cstr(d, p);
                    p += s.len() + 1;
                    opt.push(s.to_owned());
                }
                linker_options.push(opt);
            }
            _ => {}
        }
        if size == 0 {
            break;
        }
        at += size;
    }
    if let Some(s) = sects.iter().find(|s| {
        !s.zerofill()
            && u64::from(s.offset)
                .checked_add(s.size)
                .is_none_or(|e| e > d.len() as u64)
    }) {
        return Err(format!("{name}: {},{} runs past the end", s.seg, s.name));
    }
    if let Some(s) = sects.iter().find(|s| {
        (s.nreloc as usize)
            .checked_mul(8)
            .and_then(|b| b.checked_add(s.reloff as usize))
            .is_none_or(|e| e > d.len())
    }) {
        return Err(format!(
            "{name}: {},{}: relocations past the end",
            s.seg, s.name
        ));
    }
    if nsyms
        .checked_mul(16)
        .and_then(|b| b.checked_add(symoff))
        .is_none_or(|e| e > d.len())
    {
        return Err(format!("{name}: symbol table out of bounds"));
    }
    let syms: Vec<Nlist<'_>> = (0..nsyms)
        .map(|k| {
            let e = symoff + k * 16;
            Nlist {
                name: cstr(d, stroff + u32le(d, e) as usize),
                ty: d[e + 4],
                sect: d[e + 5],
                desc: u16le(d, e + 6),
                value: u64le(d, e + 8),
            }
        })
        .collect();

    // Atoms: each section split at its symbols.
    let debug = sects
        .iter()
        .any(|s| s.seg == "__DWARF" && s.name == "__debug_info");
    let mut cuts: Vec<Vec<u64>> = vec![vec![0]; sects.len()];
    let mut roots: Vec<Vec<u64>> = vec![Vec::new(); sects.len()];
    for s in &syms {
        if s.ty & N_STAB != 0 || s.ty & N_TYPE != N_SECT || s.sect == 0 {
            continue;
        }
        let k = s.sect as usize - 1;
        let Some(sec) = sects.get(k) else {
            return Err(format!("{name}: symbol {} in no section", s.name));
        };
        let off = s.value.wrapping_sub(sec.addr);
        if subsections && s.desc & N_ALT_ENTRY == 0 && off < sec.size {
            cuts[k].push(off);
        }
        if s.desc & (N_NO_DEAD_STRIP | REFERENCED_DYNAMICALLY) != 0 {
            roots[k].push(off);
        }
    }
    let file_root = |s: &InSect| {
        s.flags & S_ATTR_NO_DEAD_STRIP != 0
            || matches!(
                s.flags & 0xff,
                S_MOD_INIT_FUNC_POINTERS | S_MOD_TERM_FUNC_POINTERS
            )
    };
    for (k, s) in sects.iter_mut().enumerate() {
        if s.kind != Kind::Atoms {
            continue;
        }
        let c = &mut cuts[k];
        c.sort_unstable();
        c.dedup();
        let align = s.align.min(15) as u8;
        for (j, &start) in c.iter().enumerate() {
            let end = c.get(j + 1).copied().unwrap_or(s.size).max(start);
            // An atom's alignment: the section's, as far as its offset
            // keeps it.
            let a = if start == 0 {
                align
            } else {
                align.min(start.trailing_zeros() as u8)
            };
            let mut atom = Atom::new(fi, k as u32, start, end - start, a);
            atom.root = file_root(s)
                || roots[k]
                    .iter()
                    .any(|&o| o >= start && o < end.max(start + 1));
            s.atoms.push(link.atoms.len() as AtomId);
            s.atom_offs.push(start);
            link.atoms.push(atom);
        }
    }

    // Symbols.
    let mut fetch = Vec::new();
    let mut refs = Vec::with_capacity(syms.len());
    let mut locals = Vec::new();
    for s in &syms {
        if s.ty & N_STAB != 0 {
            refs.push(SymRef::None);
            continue;
        }
        let ext = s.ty & N_EXT != 0;
        let pext = s.ty & N_PEXT != 0;
        match s.ty & N_TYPE {
            N_SECT => {
                let Some(sec) = (s.sect as usize).checked_sub(1).and_then(|k| sects.get(k)) else {
                    return Err(format!(
                        "{name}: a symbol in section {}, of {}",
                        s.sect,
                        sects.len()
                    ));
                };
                if sec.kind != Kind::Atoms {
                    refs.push(SymRef::None);
                    continue;
                }
                let Some((atom, off)) = locate_in(sec, s.value.wrapping_sub(sec.addr)) else {
                    refs.push(SymRef::None);
                    continue;
                };
                if ext {
                    let id = link.syms.intern(s.name);
                    let weak = s.desc & N_WEAK_DEF != 0;
                    if let Err(prev) =
                        link.syms
                            .define(id, Def::Defined { atom, off }, fi, weak, pext)
                    {
                        return Err(format!(
                            "duplicate symbol: {}\n>>> defined in {}\n>>> defined in {name}",
                            s.name,
                            link.file_name(prev),
                        ));
                    }
                    let sym = link.syms.get_mut(id);
                    sym.no_dead_strip |= s.desc & (N_NO_DEAD_STRIP | REFERENCED_DYNAMICALLY) != 0;
                    if sec.flags & 0xff == S_THREAD_LOCAL_VARIABLES {
                        sym.tlv = true;
                    }
                    refs.push(SymRef::Global(id));
                } else {
                    if !s.name.is_empty() && !s.name.starts_with('l') && !s.name.starts_with('L') {
                        locals.push((s.name.to_owned(), atom, off));
                    }
                    refs.push(SymRef::Local(atom, off));
                }
            }
            N_UNDF if ext => {
                let id = link.syms.intern(s.name);
                if s.value != 0 {
                    let align = ((s.desc >> 8) & 0xf) as u8;
                    link.syms.common(id, s.value, align, fi, pext);
                } else if let Some(f) = link.syms.reference(id, fi, s.desc & N_WEAK_REF != 0) {
                    fetch.push(f);
                }
                refs.push(SymRef::Global(id));
            }
            N_ABS => {
                if ext {
                    let id = link.syms.intern(s.name);
                    if let Err(prev) = link
                        .syms
                        .define(id, Def::Absolute(s.value), fi, false, pext)
                    {
                        return Err(format!(
                            "duplicate symbol: {}\n>>> defined in {}\n>>> defined in {name}",
                            s.name,
                            link.file_name(prev),
                        ));
                    }
                    refs.push(SymRef::Global(id));
                } else {
                    refs.push(SymRef::Abs(s.value));
                }
            }
            N_INDR => {
                return Err(format!(
                    "{name}: indirect symbol {} (N_INDR) is not supported",
                    s.name
                ));
            }
            _ => refs.push(SymRef::None),
        }
    }

    let source = if debug { cu_source(d, &sects) } else { None };
    let file = ObjFile {
        name: name.clone(),
        oso,
        data: Vec::new(),
        sects,
        locals,
        source,
        debug,
        linker_options,
    };
    // Relocations.
    for k in 0..file.sects.len() {
        let s = &file.sects[k];
        if s.nreloc == 0 || !matches!(s.kind, Kind::Atoms) {
            continue;
        }
        let relocs = read_relocs(d, s, &refs, &file, &name)?;
        // Every extern target is in this file's symbol table, referenced
        // (and fetched) there already.
        for (atom, r) in relocs {
            link.atoms[atom as usize].relocs.push(r);
        }
    }
    let addrs: Vec<Option<u64>> = syms
        .iter()
        .map(|s| (s.ty & N_STAB == 0 && s.ty & N_TYPE == N_SECT).then_some(s.value))
        .collect();
    let mut file = file;
    for k in 0..file.sects.len() {
        match file.sects[k].kind {
            Kind::CompactUnwind => unwind::read_compact(link, fi, d, &file, k, &refs, &name)?,
            Kind::EhFrame => {
                unwind::read_eh_frame(link, fi, d, &file, k, &refs, &addrs, &name)?;
            }
            _ => {}
        }
    }
    file.data = data;
    link.files.push(file);
    Ok(fetch)
}

/// Raw `relocation_info` of section `s`, in file order.
pub struct RawReloc {
    pub addr: u32,
    pub sym: u32,
    pub pcrel: bool,
    pub len: u8,
    pub ext: bool,
    pub ty: u16,
}

pub fn raw_relocs(d: &[u8], s: &InSect) -> Vec<RawReloc> {
    (0..s.nreloc as usize)
        .map(|k| {
            let e = s.reloff as usize + k * 8;
            let info = u32le(d, e + 4);
            RawReloc {
                addr: u32le(d, e),
                sym: info & 0x00ff_ffff,
                pcrel: info & (1 << 24) != 0,
                len: ((info >> 25) & 3) as u8,
                ext: info & (1 << 27) != 0,
                ty: (info >> 28) as u16,
            }
        })
        .collect()
}

/// The inline value of a data field, sign-extended.
pub fn inline(d: &[u8], at: usize, len: u8) -> i64 {
    match len {
        3 => u64le(d, at) as i64,
        2 => i64::from(u32le(d, at) as i32),
        1 => i64::from(u16le(d, at) as i16),
        _ => i64::from(d.get(at).copied().unwrap_or(0) as i8),
    }
}

/// The target of an extern relocation's symbol `sym`, with `addend`.
pub fn sym_target(
    refs: &[SymRef],
    sym: u32,
    addend: i64,
    name: &str,
) -> Result<(Target, i64), String> {
    match refs.get(sym as usize) {
        Some(SymRef::Global(id)) => Ok((Target::Sym(*id), addend)),
        Some(SymRef::Local(a, off)) => Ok((Target::Atom(*a), addend + *off as i64)),
        _ => Err(format!("{name}: relocation against unusable symbol #{sym}")),
    }
}

/// The atom at object address `addr` (a non-extern relocation's target).
pub fn addr_target(
    file: &ObjFile,
    sect: u32,
    addr: u64,
    name: &str,
) -> Result<(Target, i64), String> {
    let s = file
        .sects
        .get(sect.wrapping_sub(1) as usize)
        .ok_or_else(|| format!("{name}: relocation against section {sect}"))?;
    if s.kind != Kind::Atoms {
        return Err(format!(
            "{name}: relocation into {},{} is not supported",
            s.seg, s.name
        ));
    }
    let (a, off) = locate_in(s, addr.wrapping_sub(s.addr)).ok_or_else(|| {
        format!(
            "{name}: relocation target {addr:#x} outside {},{}",
            s.seg, s.name
        )
    })?;
    Ok((Target::Atom(a), off as i64))
}

fn read_relocs(
    d: &[u8],
    s: &InSect,
    refs: &[SymRef],
    file: &ObjFile,
    name: &str,
) -> Result<Vec<(AtomId, Reloc)>, String> {
    let raw = raw_relocs(d, s);
    let mut out = Vec::with_capacity(raw.len());
    let mut addend: i64 = 0;
    let mut sub: Option<(Target, i64)> = None;
    for r in &raw {
        if r.addr & 0x8000_0000 != 0 {
            return Err(format!("{name}: scattered relocations are not supported"));
        }
        match r.ty {
            a64::ADDEND => {
                // 24-bit signed.
                addend = i64::from(((r.sym << 8) as i32) >> 8);
                continue;
            }
            a64::SUBTRACTOR => {
                if !r.ext {
                    return Err(format!(
                        "{name}: section-relative SUBTRACTOR is not supported"
                    ));
                }
                sub = Some(sym_target(refs, r.sym, 0, name)?);
                continue;
            }
            _ => {}
        }
        let Some((atom, at)) = locate_in(s, u64::from(r.addr)) else {
            return Err(format!(
                "{name}: relocation at {:#x} in {},{} outside every atom",
                r.addr, s.seg, s.name
            ));
        };
        let is_data = r.ty == a64::UNSIGNED;
        let inl = if is_data {
            inline(d, s.offset as usize + r.addr as usize, r.len)
        } else {
            0
        };
        let (target, mut add) = if r.ext {
            sym_target(refs, r.sym, inl + addend, name)?
        } else if is_data && sub.is_none() {
            addr_target(file, r.sym, inl as u64, name)?
        } else {
            return Err(format!(
                "{name}: section-relative relocation type {} is not supported",
                r.ty
            ));
        };
        // `target - sub + addend`: the subtrahend's offset folds in.
        let sub_t = sub.take().map(|(t, o)| {
            add -= o;
            t
        });
        out.push((
            atom,
            Reloc {
                off: at as u32,
                kind: r.ty,
                len: r.len,
                pcrel: r.pcrel,
                target,
                addend: add,
                sub: sub_t,
            },
        ));
        addend = 0;
    }
    Ok(out)
}

/// The first compile unit's source file: `DW_AT_name` under
/// `DW_AT_comp_dir`.
fn cu_source(d: &[u8], sects: &[InSect]) -> Option<String> {
    let sect = |n: &str| {
        sects
            .iter()
            .find(|s| s.seg == "__DWARF" && s.name == n)
            .and_then(|s| d.get(s.offset as usize..(s.offset as u64 + s.size) as usize))
    };
    let info = sect("__debug_info")?;
    let abbrev = sect("__debug_abbrev")?;
    let strs = sect("__debug_str").unwrap_or_default();
    let line_strs = sect("__debug_line_str").unwrap_or_default();
    let version = u16le(info, 4);
    let (abbrev_off, addr_size, mut at) = if version >= 5 {
        (u32le(info, 8) as usize, info[7], 12)
    } else {
        (u32le(info, 6) as usize, *info.get(10)?, 11)
    };
    let code = nabic_core::leb::read_uleb(info, &mut at)?;
    // The abbreviation.
    let mut p = abbrev_off;
    let attrs = loop {
        let c = nabic_core::leb::read_uleb(abbrev, &mut p)?;
        if c == 0 {
            return None;
        }
        nabic_core::leb::read_uleb(abbrev, &mut p)?;
        p += 1;
        let mut list = Vec::new();
        loop {
            let a = nabic_core::leb::read_uleb(abbrev, &mut p)?;
            let f = nabic_core::leb::read_uleb(abbrev, &mut p)?;
            if f == 0x21 {
                nabic_core::leb::read_sleb(abbrev, &mut p)?;
            }
            if a == 0 && f == 0 {
                break;
            }
            list.push((a, f));
        }
        if c == code {
            break list;
        }
    };
    let (mut name, mut dir) = (None, None);
    for (a, f) in attrs {
        let val: Option<String> = match f {
            0x08 => {
                let s = cstr(info, at).to_owned();
                at += s.len() + 1;
                Some(s)
            }
            0x0e => {
                let o = u32le(info, at) as usize;
                at += 4;
                Some(cstr(strs, o).to_owned())
            }
            0x1f => {
                let o = u32le(info, at) as usize;
                at += 4;
                Some(cstr(line_strs, o).to_owned())
            }
            _ => {
                at += form_size(f, info, at, addr_size)?;
                None
            }
        };
        match a {
            0x03 => name = val,
            0x1b => dir = val,
            _ => {}
        }
    }
    let name = name?;
    if name.starts_with('/') {
        return Some(name);
    }
    Some(match dir {
        Some(d) => format!("{}/{name}", d.trim_end_matches('/')),
        None => name,
    })
}

/// Bytes a DWARF attribute of form `f` takes at `at`; `None` for forms
/// the source name lookup does not handle.
fn form_size(f: u64, b: &[u8], at: usize, addr_size: u8) -> Option<usize> {
    let leb = |at: usize| {
        let mut p = at;
        nabic_core::leb::read_uleb(b, &mut p).map(|_| p - at)
    };
    Some(match f {
        0x01 => addr_size as usize,
        0x0b | 0x11 | 0x0c | 0x25 | 0x29 => 1,
        0x05 | 0x12 | 0x26 | 0x2a => 2,
        0x27 | 0x2b => 3,
        0x06 | 0x13 | 0x17 | 0x10 | 0x28 | 0x2c => 4,
        0x07 | 0x14 | 0x20 => 8,
        0x1e => 16,
        0x19 | 0x21 => 0,
        0x0d | 0x0f | 0x15 | 0x1a | 0x1b | 0x22 | 0x23 => leb(at)?,
        0x18 | 0x09 => {
            let mut p = at;
            let n = nabic_core::leb::read_uleb(b, &mut p)? as usize;
            p - at + n
        }
        0x0a => 1 + *b.get(at)? as usize,
        0x03 => 2 + u16le(b, at) as usize,
        0x04 => 4 + u32le(b, at) as usize,
        _ => return None,
    })
}

/// A `PLATFORM_*` as Apple's tools name it.
fn platform_name(p: u32) -> String {
    match p {
        1 => "macOS".to_owned(),
        2 => "iOS".to_owned(),
        6 => "Mac Catalyst".to_owned(),
        7 => "the iOS simulator".to_owned(),
        _ => format!("platform {p}"),
    }
}

/// The clang triple of a `PLATFORM_*`, for the error's fix.
const fn platform_triple(p: u32) -> &'static str {
    match p {
        2 => "arm64-apple-ios13.4",
        7 => "arm64-apple-ios13.4-simulator",
        _ => "arm64-apple-macos11.0",
    }
}
