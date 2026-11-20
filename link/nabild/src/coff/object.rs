//! COFF relocatable objects (regular and `/bigobj`) into atoms.
//!
//! One atom per section. COMDAT sections whose leader was seen before are
//! discarded (associative ones with their parent); `.drectve` directives,
//! weak externals and commons go to the link.

use crate::bytes::{cstr, u16le, u32le};
use crate::coff::{
    Alias, DEAD, Export, Link, SCN_CNT_CODE, SCN_CNT_UNINITIALIZED_DATA, SCN_LNK_COMDAT,
    SCN_LNK_INFO, SCN_LNK_REMOVE, SCN_MEM_EXECUTE,
};
use crate::symtab::{Atom, AtomId, Def, Fetch, Reloc, SymId, Target};

const CLASS_EXTERNAL: u8 = 2;
const CLASS_STATIC: u8 = 3;
/// A symbol's type: a function.
const DTYPE_FUNCTION: u16 = 0x20;
const CLASS_LABEL: u8 = 6;
const CLASS_WEAK_EXTERNAL: u8 = 105;

const SELECT_NODUPLICATES: u8 = 1;
const SELECT_ASSOCIATIVE: u8 = 5;

const SCN_LNK_NRELOC_OVFL: u32 = 0x0100_0000;
const SCN_TYPE_NO_PAD: u32 = 0x8;

pub const BIGOBJ_CLASS_ID: [u8; 16] = [
    0xc7, 0xa1, 0xba, 0xd1, 0xee, 0xba, 0xa9, 0x4b, 0xaf, 0x20, 0xfa, 0xf6, 0x6a, 0xa4, 0xdc, 0xb8,
];

#[derive(Debug)]
pub struct InSect {
    pub name: String,
    pub chars: u32,
    /// Offset of the contents in the file's data.
    pub off: usize,
    pub size: u64,
    pub comdat: bool,
    /// Its atom, [`DEAD`] when not loaded.
    pub atom: AtomId,
    /// A COMDAT of one of nabic's string literals (`nabi.str.<hash>`,
    /// named by its content, its address meaningless).
    pub literal: bool,
    /// A COMDAT of one of nabic's function value thunks
    /// (`icf::fn_value_thunk`).
    pub thunk: bool,
}

impl InSect {
    #[must_use]
    pub const fn zerofill(&self) -> bool {
        self.chars & SCN_CNT_UNINITIALIZED_DATA != 0
    }

    #[must_use]
    pub const fn code(&self) -> bool {
        self.chars & (SCN_CNT_CODE | SCN_MEM_EXECUTE) != 0
    }
}

pub struct ObjFile {
    /// For messages: the path, or `archive(member)`.
    pub name: String,
    /// The archive it came from, else empty.
    pub archive: String,
    pub data: Vec<u8>,
    pub sects: Vec<InSect>,
    /// `/debug`: the kept `CodeView` sections.
    pub cv: Vec<CvIn>,
    /// Static functions: name, atom and offset (backtrace names).
    pub locals: Vec<(String, AtomId, u64)>,
}

/// A kept `.debug$S` or `.debug$T` section of an object.
#[derive(Debug)]
pub struct CvIn {
    /// Its index in the file.
    pub sect: usize,
    pub types: bool,
    /// The section it is associated with (a function's code), if any.
    pub parent: Option<usize>,
    pub relocs: Vec<Reloc>,
}

/// The layout of a COFF object's header.
pub struct Header {
    pub machine: u16,
    pub nsects: usize,
    pub sects_at: usize,
    pub symoff: usize,
    pub nsyms: usize,
    /// 20 for `/bigobj`, else 18.
    pub symsize: usize,
}

impl Header {
    pub const fn strtab(&self) -> usize {
        self.symoff + self.nsyms * self.symsize
    }

    /// The section number of symbol record `i`: 1.. a section, 0
    /// undefined, -1 absolute, -2 debug. A plain object's is 16 bits
    /// unsigned (sections up to 0xfeff); only 0xffff and 0xfffe are the
    /// negative ones, so section 32768 and above stay sections.
    pub fn section(&self, d: &[u8], i: usize) -> i32 {
        let e = self.symoff + i * self.symsize;
        if self.symsize == 20 {
            u32le(d, e + 12) as i32
        } else {
            match u16le(d, e + 12) {
                0xffff => -1,
                0xfffe => -2,
                n => i32::from(n),
            }
        }
    }

    /// `(value, type, class, aux count)` of symbol record `i`.
    pub fn fields(&self, d: &[u8], i: usize) -> (u32, u16, u8, usize) {
        let e = self.symoff + i * self.symsize;
        let t = if self.symsize == 20 { e + 16 } else { e + 14 };
        (
            u32le(d, e + 8),
            u16le(d, t),
            d.get(t + 2).copied().unwrap_or(0),
            d.get(t + 3).copied().unwrap_or(0) as usize,
        )
    }

    pub fn name<'a>(&self, d: &'a [u8], i: usize) -> &'a str {
        let e = self.symoff + i * self.symsize;
        if u32le(d, e) == 0 {
            cstr(d, self.strtab() + u32le(d, e + 4) as usize)
        } else {
            let raw = d.get(e..e + 8).unwrap_or_default();
            let n = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
            std::str::from_utf8(&raw[..n]).unwrap_or("")
        }
    }

    /// The aux record after symbol record `i`.
    pub fn aux<'a>(&self, d: &'a [u8], i: usize) -> &'a [u8] {
        let e = self.symoff + (i + 1) * self.symsize;
        d.get(e..e + self.symsize).unwrap_or_default()
    }
}

/// The header of COFF object `d`, regular or `/bigobj`.
pub fn header(d: &[u8]) -> Result<Header, String> {
    if d.len() < 20 {
        return Err("not a COFF object".to_owned());
    }
    let h = if u16le(d, 0) == 0 && u16le(d, 2) == 0xffff {
        if u16le(d, 4) < 2 || d.get(12..28) != Some(&BIGOBJ_CLASS_ID[..]) {
            return Err("not a COFF object".to_owned());
        }
        Header {
            machine: u16le(d, 6),
            nsects: u32le(d, 44) as usize,
            sects_at: 56,
            symoff: u32le(d, 48) as usize,
            nsyms: u32le(d, 52) as usize,
            symsize: 20,
        }
    } else {
        Header {
            machine: u16le(d, 0),
            nsects: usize::from(u16le(d, 2)),
            sects_at: 20 + usize::from(u16le(d, 16)),
            symoff: u32le(d, 8) as usize,
            nsyms: u32le(d, 12) as usize,
            symsize: 18,
        }
    };
    if h.sects_at + h.nsects * 40 > d.len() || h.strtab() > d.len() {
        return Err("truncated COFF object".to_owned());
    }
    Ok(h)
}

/// The name of section header `s`: inline, or `/<offset>` (decimal) or
/// `//<offset>` (base64) into the string table.
fn sect_name<'a>(d: &'a [u8], h: &Header, s: usize) -> &'a str {
    let raw = &d[s..s + 8];
    let n = raw.iter().position(|&c| c == 0).unwrap_or(8);
    let name = std::str::from_utf8(&raw[..n]).unwrap_or("");
    let off = if let Some(b) = name.strip_prefix("//") {
        let mut v = 0u64;
        for c in b.bytes() {
            let x = match c {
                b'A'..=b'Z' => c - b'A',
                b'a'..=b'z' => c - b'a' + 26,
                b'0'..=b'9' => c - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                _ => return name,
            };
            v = v * 64 + u64::from(x);
        }
        v as usize
    } else if let Some(n) = name.strip_prefix('/') {
        match n.parse::<usize>() {
            Ok(v) => v,
            Err(_) => return name,
        }
    } else {
        return name;
    };
    cstr(d, h.strtab() + off)
}

/// log2 of a section's alignment (`IMAGE_SCN_ALIGN_*`, 16 by default).
const fn align_of(chars: u32) -> u8 {
    if chars & SCN_TYPE_NO_PAD != 0 {
        return 0;
    }
    let a = (chars >> 20) & 0xf;
    if a == 0 { 4 } else { (a - 1) as u8 }
}

/// Not part of the image: directives, address significance tables,
/// `CodeView` and DWARF, guard tables, `/safeseh` tables.
fn skipped(name: &str, chars: u32) -> bool {
    chars & (SCN_LNK_REMOVE | SCN_LNK_INFO) != 0
        || name.starts_with(".debug")
        || matches!(
            name,
            ".drectve"
                | ".llvm_addrsig"
                | ".llvm.call-graph-profile"
                | ".gfids$y"
                | ".giats$y"
                | ".gljmp$y"
                | ".gehcont$y"
                | ".sxdata"
        )
}

/// `(width log2, pc-relative)` of relocation type `ty`.
const fn reloc_shape(machine: u16, ty: u16) -> (u8, bool) {
    if machine == crate::coff::MACHINE_AMD64 {
        match ty {
            1 => (3, false),
            4..=9 => (2, true),
            0xa => (1, false),
            _ => (2, false),
        }
    } else if machine == crate::coff::MACHINE_I386 {
        match ty {
            1 | 0xa => (1, false),
            2 => (1, true),
            0xd => (0, false),
            0x14 => (2, true),
            _ => (2, false),
        }
    } else {
        match ty {
            0xe => (3, false),
            0xd => (1, false),
            3..=5 | 0xf | 0x10 | 0x11 => (2, true),
            _ => (2, false),
        }
    }
}

/// A symbol record's meaning for relocations.
#[derive(Clone, Copy, Debug)]
enum SymRef {
    None,
    Global(SymId),
    Local(AtomId, u64),
}

/// Read object `data` as file `fi` into `l`; the archive members its
/// references fetch.
#[allow(clippy::too_many_lines)]
pub fn parse(
    l: &mut Link,
    fi: u32,
    name: String,
    archive: String,
    data: Vec<u8>,
) -> Result<Vec<Fetch>, String> {
    let h = header(&data).map_err(|e| format!("{name}: {e}"))?;
    if h.machine != l.arch.machine() && h.machine != 0 {
        return Err(format!(
            "{name}: machine type {:#x} conflicts with {}",
            h.machine,
            l.arch.name()
        ));
    }
    let d = &data;
    let mut sects = Vec::with_capacity(h.nsects);
    for i in 0..h.nsects {
        let s = h.sects_at + i * 40;
        let chars = u32le(d, s + 36);
        let size = u64::from(u32le(d, s + 16));
        let off = u32le(d, s + 20) as usize;
        if chars & SCN_CNT_UNINITIALIZED_DATA == 0 && off + size as usize > d.len() {
            return Err(format!("{name}: section {} runs past the end", i + 1));
        }
        sects.push(InSect {
            name: sect_name(d, &h, s).to_owned(),
            chars,
            off,
            size,
            comdat: chars & SCN_LNK_COMDAT != 0,
            atom: DEAD,
            literal: false,
            thunk: false,
        });
    }

    // COMDATs: the section definition record (selection, associated
    // section), then the leader, the next symbol of that section.
    let mut selection = vec![0u8; h.nsects];
    let mut assoc = vec![0usize; h.nsects];
    let mut leader: Vec<Option<(String, bool)>> = vec![None; h.nsects];
    let mut have_def = vec![false; h.nsects];
    let mut i = 0;
    while i < h.nsyms {
        let sn = h.section(d, i);
        let (value, _, class, naux) = h.fields(d, i);
        if sn > 0 && (sn as usize) <= h.nsects {
            let k = sn as usize - 1;
            if sects[k].comdat {
                if !have_def[k] && class == CLASS_STATIC && naux > 0 && value == 0 {
                    let aux = h.aux(d, i);
                    selection[k] = aux.get(14).copied().unwrap_or(0);
                    let mut num = usize::from(u16le(aux, 12));
                    if h.symsize == 20 {
                        num |= usize::from(u16le(aux, 16)) << 16;
                    }
                    assoc[k] = num;
                    have_def[k] = true;
                } else if have_def[k] && leader[k].is_none() && selection[k] != SELECT_ASSOCIATIVE {
                    leader[k] = Some((h.name(d, i).to_owned(), class == CLASS_EXTERNAL));
                }
            }
        }
        i += 1 + naux;
    }
    for (s, lead) in sects.iter_mut().zip(&leader) {
        s.literal = lead
            .as_ref()
            .is_some_and(|(n, ext)| !ext && n.trim_start_matches('_').starts_with("nabi.str."));
        s.thunk = lead
            .as_ref()
            .is_some_and(|(n, ext)| !ext && crate::icf::fn_value_thunk(n.trim_start_matches('_')));
    }
    // Keep or discard each COMDAT; associative ones follow their parent.
    let mut keep = vec![true; h.nsects];
    for k in 0..h.nsects {
        if !sects[k].comdat || selection[k] == SELECT_ASSOCIATIVE {
            continue;
        }
        let Some((lead, external)) = &leader[k] else {
            continue;
        };
        if !external {
            continue;
        }
        if l.comdats.contains_key(lead) {
            if selection[k] == SELECT_NODUPLICATES {
                l.errors.push(format!(
                    "duplicate symbol: {lead}\n>>> defined at {name}\n>>> and in another object"
                ));
            }
            keep[k] = false;
        } else {
            l.comdats.insert(lead.clone(), sects[k].size);
        }
    }
    for _ in 0..8 {
        let mut changed = false;
        for k in 0..h.nsects {
            if sects[k].comdat && selection[k] == SELECT_ASSOCIATIVE && keep[k] {
                let p = assoc[k];
                if p == 0 || p > h.nsects || !keep[p - 1] {
                    keep[k] = false;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }

    // Atoms.
    let mut directives = String::new();
    #[allow(clippy::needless_range_loop)]
    for k in 0..h.nsects {
        let s = &sects[k];
        if s.name == ".drectve" {
            let bytes = &d[s.off..s.off + s.size as usize];
            directives.push_str(&String::from_utf8_lossy(bytes));
            directives.push(' ');
            continue;
        }
        if !keep[k] || skipped(&s.name, s.chars) {
            continue;
        }
        let id = l.atoms.len() as AtomId;
        let mut a = Atom::new(fi, k as u32, 0, s.size, align_of(s.chars));
        a.root = !s.comdat;
        l.atoms.push(a);
        sects[k].atom = id;
    }
    // Associative children live with their parents.
    for k in 0..h.nsects {
        if sects[k].atom != DEAD && sects[k].comdat && selection[k] == SELECT_ASSOCIATIVE {
            let p = sects[assoc[k] - 1].atom;
            if p != DEAD {
                l.atoms[p as usize].keep.push(Target::Atom(sects[k].atom));
            }
        }
    }

    // Symbols.
    let mut refs = vec![SymRef::None; h.nsyms];
    let mut locals = Vec::new();
    let mut fetch = Vec::new();
    let mut i = 0;
    while i < h.nsyms {
        let sn = h.section(d, i);
        let (value, ty, class, naux) = h.fields(d, i);
        let sname = h.name(d, i);
        let in_sect = |k: i32| -> Option<AtomId> {
            (k > 0 && (k as usize) <= h.nsects).then(|| sects[k as usize - 1].atom)
        };
        match class {
            CLASS_EXTERNAL => {
                let id = l.syms.intern(sname);
                refs[i] = SymRef::Global(id);
                if sn > 0 {
                    let atom = in_sect(sn).unwrap_or(DEAD);
                    if atom != DEAD {
                        let def = Def::Defined {
                            atom,
                            off: u64::from(value),
                        };
                        if let Err(prev) = l.syms.define(id, def, fi, false, false) {
                            l.errors.push(format!(
                                "duplicate symbol: {sname}\n>>> defined at {}\n>>> defined at {name}",
                                l.file_name(prev)
                            ));
                        }
                    }
                    // Else in a discarded COMDAT: the kept copy defines it.
                } else if sn == -1 {
                    let _ = l
                        .syms
                        .define(id, Def::Absolute(u64::from(value)), fi, false, false);
                } else if sn == 0 && value > 0 {
                    let align = (u64::from(value).min(32))
                        .next_power_of_two()
                        .trailing_zeros();
                    l.syms.common(id, u64::from(value), align as u8, fi, false);
                } else if sn == 0 {
                    if let Some(f) = l.syms.reference(id, fi, false) {
                        fetch.push(f);
                    }
                }
            }
            CLASS_WEAK_EXTERNAL if sn == 0 && naux > 0 => {
                let id = l.syms.intern(sname);
                refs[i] = SymRef::Global(id);
                let aux = h.aux(d, i);
                let tag = u32le(aux, 0) as usize;
                if tag < h.nsyms {
                    let tsn = h.section(d, tag);
                    let (tv, _, tclass, _) = h.fields(d, tag);
                    let target = if tclass == CLASS_EXTERNAL || tclass == CLASS_WEAK_EXTERNAL {
                        Some((Target::Sym(l.syms.intern(h.name(d, tag))), 0))
                    } else {
                        in_sect(tsn)
                            .filter(|&a| a != DEAD)
                            .map(|a| (Target::Atom(a), u64::from(tv)))
                    };
                    if let Some((target, off)) = target {
                        l.aliases.entry(id).or_insert(Alias { target, off });
                    }
                }
            }
            CLASS_STATIC | CLASS_LABEL => {
                if sn > 0 {
                    let atom = in_sect(sn).unwrap_or(DEAD);
                    refs[i] = SymRef::Local(atom, u64::from(value));
                    if ty == DTYPE_FUNCTION && atom != DEAD {
                        locals.push((sname.to_owned(), atom, u64::from(value)));
                    }
                } else if sn == -1 {
                    refs[i] = SymRef::Global(l.absolute(u64::from(value)));
                }
            }
            _ => {}
        }
        i += 1 + naux;
    }

    // Relocations.
    for (k, sect) in sects.iter().enumerate() {
        let atom = sect.atom;
        if atom == DEAD {
            continue;
        }
        l.atoms[atom as usize].relocs = relocs_of(d, &h, &name, k, sect.chars, &refs)?;
    }
    // `/debug`: `CodeView` the PDB takes.
    let mut cv = Vec::new();
    for (k, sect) in sects.iter().enumerate() {
        let types = sect.name == ".debug$T";
        if !l.debug || !keep[k] || !(types || sect.name == ".debug$S") {
            continue;
        }
        let parent = (sect.comdat && selection[k] == SELECT_ASSOCIATIVE).then(|| assoc[k] - 1);
        let relocs = relocs_of(d, &h, &name, k, sect.chars, &refs)?;
        cv.push(CvIn {
            sect: k,
            types,
            parent,
            relocs,
        });
    }

    l.files.push(ObjFile {
        name: name.clone(),
        archive,
        data,
        sects,
        cv,
        locals,
    });
    directives_into(l, fi, &name, &directives, &mut fetch)?;
    Ok(fetch)
}

/// The relocations of section `k` of object `d` (`name`), whose symbol
/// records mean `refs`.
fn relocs_of(
    d: &[u8],
    h: &Header,
    name: &str,
    k: usize,
    chars: u32,
    refs: &[SymRef],
) -> Result<Vec<Reloc>, String> {
    let s = h.sects_at + k * 40;
    let vaddr = u32le(d, s + 12);
    let relptr = u32le(d, s + 24) as usize;
    let mut nrel = usize::from(u16le(d, s + 32));
    let mut at = relptr;
    if chars & SCN_LNK_NRELOC_OVFL != 0 && nrel == 0xffff {
        nrel = (u32le(d, relptr) as usize).saturating_sub(1);
        at += 10;
    }
    if nrel
        .checked_mul(10)
        .and_then(|n| n.checked_add(at))
        .is_none_or(|end| end > d.len())
    {
        return Err(format!(
            "{name}: relocations of section {} run past the end",
            k + 1
        ));
    }
    let mut relocs = Vec::with_capacity(nrel);
    for r in 0..nrel {
        let e = at + r * 10;
        let off = u32le(d, e).wrapping_sub(vaddr);
        let si = u32le(d, e + 4) as usize;
        let ty = u16le(d, e + 8);
        // `IMAGE_REL_*_ABSOLUTE`: nothing to do.
        if ty == 0 {
            continue;
        }
        let (target, addend) = match refs.get(si).copied().unwrap_or(SymRef::None) {
            SymRef::Global(id) => (Target::Sym(id), 0),
            SymRef::Local(a, v) => (Target::Atom(a), v as i64),
            SymRef::None => {
                return Err(format!(
                    "{name}: relocation against symbol {si}, which is no section or external symbol"
                ));
            }
        };
        let (len, pcrel) = reloc_shape(h.machine, ty);
        relocs.push(Reloc {
            off,
            kind: ty,
            len,
            pcrel,
            target,
            addend,
            sub: None,
        });
    }
    Ok(relocs)
}

/// Split a directive string as the command line is: on blanks, with
/// double quotes grouping.
fn tokens(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut any = false;
    for c in s.trim_start_matches('\u{feff}').chars() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            c if c.is_whitespace() && !quoted => {
                if any {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            '\0' => {}
            c => {
                cur.push(c);
                any = true;
            }
        }
    }
    if any {
        out.push(cur);
    }
    out
}

/// `/EXPORT:name[=internal][,@ordinal[,NONAME]][,DATA][,PRIVATE]`. x86:
/// C names, the symbols `_`-prefixed.
fn export_directive(l: &mut Link, v: &str) -> Export {
    let mut parts = v.split(',');
    let first = parts.next().unwrap_or("");
    let (name, internal) = first.split_once('=').unwrap_or((first, first));
    let data = parts.any(|p| p.eq_ignore_ascii_case("data"));
    let sym = if internal.starts_with(['?', '@']) {
        l.syms.intern(internal)
    } else {
        l.syms.intern(&l.arch.mangle(internal))
    };
    Export {
        name: name.to_owned(),
        sym,
        data,
    }
}

/// Apply object `fi`'s `.drectve`.
fn directives_into(
    l: &mut Link,
    fi: u32,
    name: &str,
    text: &str,
    fetch: &mut Vec<Fetch>,
) -> Result<(), String> {
    for t in tokens(text) {
        let flag = t.strip_prefix('/').or_else(|| t.strip_prefix('-'));
        let Some(flag) = flag else {
            return Err(format!("{name}: unknown directive: {t}"));
        };
        let (k, v) = flag.split_once(':').unwrap_or((flag, ""));
        match k.to_ascii_lowercase().as_str() {
            "defaultlib" => l.default_libs.push(v.to_owned()),
            "include" => {
                let id = l.syms.intern(v);
                if let Some(f) = l.syms.reference(id, fi, false) {
                    fetch.push(f);
                }
                l.includes.push(id);
            }
            "export" => {
                let e = export_directive(l, v);
                if let Some(f) = l.syms.reference(e.sym, fi, false) {
                    fetch.push(f);
                }
                l.exports.push(e);
            }
            "alternatename" => {
                let (from, to) = v
                    .split_once('=')
                    .ok_or_else(|| format!("{name}: /alternatename:{v}: expected from=to"))?;
                let from = l.syms.intern(from);
                let to = l.syms.intern(to);
                l.aliases.entry(from).or_insert(Alias {
                    target: Target::Sym(to),
                    off: 0,
                });
            }
            "merge" => {
                let (from, to) = v
                    .split_once('=')
                    .ok_or_else(|| format!("{name}: /merge:{v}: expected from=to"))?;
                l.merges.push((from.to_owned(), to.to_owned()));
            }
            "failifmismatch" => {
                let (key, val) = v
                    .split_once('=')
                    .ok_or_else(|| format!("{name}: /failifmismatch:{v}: expected key=value"))?;
                match l.mismatch.get(key) {
                    Some((old, by)) if old != val => {
                        return Err(format!(
                            "/failifmismatch: mismatch detected for '{key}':\n>>> {by} has value {old}\n>>> {name} has value {val}"
                        ));
                    }
                    Some(_) => {}
                    None => {
                        l.mismatch
                            .insert(key.to_owned(), (val.to_owned(), name.to_owned()));
                    }
                }
            }
            // Nothing for an executable without a manifest, guard tables
            // or incremental linking.
            "nodefaultlib" | "manifestdependency" | "guardsym" | "editandcontinue"
            | "throwingnew" | "disallowlib" | "aligncomm" => {}
            _ => return Err(format!("{name}: unsupported directive: {t}")),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directives_split_like_a_command_line() {
        assert_eq!(
            tokens("\u{feff} /DEFAULTLIB:\"msvcrt\" /alternatename:a=b  /EXPORT:\"x y\",DATA\0"),
            [
                "/DEFAULTLIB:msvcrt",
                "/alternatename:a=b",
                "/EXPORT:x y,DATA"
            ]
        );
    }

    #[test]
    fn section_numbers_above_32767_are_sections() {
        let h = Header {
            machine: 0xaa64,
            nsects: 0xfeff,
            sects_at: 0,
            symoff: 0,
            nsyms: 4,
            symsize: 18,
        };
        let mut d = vec![0u8; 4 * 18];
        for (i, n) in [0x8000u16, 0xfeff, 0xffff, 0xfffe].into_iter().enumerate() {
            d[i * 18 + 12..i * 18 + 14].copy_from_slice(&n.to_le_bytes());
        }
        let got: Vec<i32> = (0..4).map(|i| h.section(&d, i)).collect();
        assert_eq!(got, [0x8000, 0xfeff, -1, -2]);
    }

    #[test]
    fn section_alignment_defaults_to_16() {
        assert_eq!(align_of(0x6050_0020), 4);
        assert_eq!(align_of(0x4010_0040), 0);
        assert_eq!(align_of(0x4000_0040), 4);
        assert_eq!(align_of(0x40d0_0040), 12);
    }
}
