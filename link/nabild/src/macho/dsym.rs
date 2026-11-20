//! `.dSYM` bundles, as dsymutil writes them: the DWARF of the objects a
//! Mach-O executable's debug map names, moved to the linked addresses,
//! in an `MH_DSYM` file that carries the executable's UUID.
//!
//! Every DWARF 2-4 unit of an object is kept as it is, its offsets into
//! the other debug sections rebased; addresses are moved by the
//! relocations that mark them, offsets from a unit's base by walking its
//! entries. Code the link dropped is at address 0, below `__TEXT`, where
//! a debugger ignores it. `.debug_aranges` is written from the
//! subprograms. Objects with DWARF 5 or 64-bit DWARF are skipped.

use crate::bytes::{cstr, name16, put32, put64, u16le, u32le, u64le};
use nabic_core::leb::{put_uleb, read_sleb, read_uleb};
use nabic_core::{HashMap, HashSet};
use std::path::{Path, PathBuf};

const MH_MAGIC_64: u32 = 0xfeed_facf;
const MH_DSYM: u32 = 0xa;
const LC_SYMTAB: u32 = 0x2;
const LC_SEGMENT_64: u32 = 0x19;
const LC_UUID: u32 = 0x1b;
const LC_BUILD_VERSION: u32 = 0x32;
const N_STAB: u8 = 0xe0;
const N_TYPE: u8 = 0x0e;
const N_SECT: u8 = 0x0e;
const N_GSYM: u8 = 0x20;
const N_FUN: u8 = 0x24;
const N_STSYM: u8 = 0x26;
const N_SO: u8 = 0x64;
const N_OSO: u8 = 0x66;
const S_ATTR_DEBUG: u32 = 0x0200_0000;
const CPU_TYPE_X86_64: u32 = 0x0100_0007;
const RELOC_UNSIGNED: u32 = 0;
const ARM64_RELOC_SUBTRACTOR: u32 = 1;
const X86_64_RELOC_SUBTRACTOR: u32 = 5;
const PAGE: u64 = 0x4000;

const DW_TAG_SUBPROGRAM: u64 = 0x2e;
const DW_TAG_NAMESPACE: u64 = 0x39;
const DW_AT_LOCATION: u64 = 0x02;
const DW_AT_STMT_LIST: u64 = 0x10;
const DW_AT_LOW_PC: u64 = 0x11;
const DW_AT_HIGH_PC: u64 = 0x12;
const DW_AT_STRING_LENGTH: u64 = 0x19;
const DW_AT_RETURN_ADDR: u64 = 0x2a;
const DW_AT_DATA_MEMBER_LOCATION: u64 = 0x38;
const DW_AT_FRAME_BASE: u64 = 0x40;
const DW_AT_SEGMENT: u64 = 0x46;
const DW_AT_STATIC_LINK: u64 = 0x48;
const DW_AT_USE_LOCATION: u64 = 0x4a;
const DW_AT_VTABLE_ELEM_LOCATION: u64 = 0x4d;
const DW_AT_RANGES: u64 = 0x55;

/// The debug sections a `.dSYM` carries, in file order.
const SECTS: [&str; 7] = [
    "__debug_abbrev",
    "__debug_info",
    "__debug_line",
    "__debug_str",
    "__debug_ranges",
    "__debug_loc",
    "__debug_aranges",
];
const ABBREV: usize = 0;
const INFO: usize = 1;
const LINE: usize = 2;
const STR: usize = 3;
const RANGES: usize = 4;
const LOC: usize = 5;
const ARANGES: usize = 6;

struct Sect {
    seg: String,
    name: String,
    addr: u64,
    size: u64,
    offset: u32,
    align: u32,
    reloff: u32,
    nreloc: u32,
    flags: u32,
}

struct Seg {
    name: String,
    vmaddr: u64,
    vmsize: u64,
    maxprot: u32,
    initprot: u32,
    flags: u32,
    /// Indices into `MachO::sects`.
    sects: Vec<usize>,
}

struct Sym {
    name: String,
    ty: u8,
    sect: u8,
    desc: u16,
    value: u64,
}

struct MachO {
    cpu: u32,
    cpusub: u32,
    segs: Vec<Seg>,
    sects: Vec<Sect>,
    syms: Vec<Sym>,
    uuid: Option<[u8; 16]>,
    build: Option<Vec<u8>>,
}

fn parse(d: &[u8]) -> Result<MachO, String> {
    if u32le(d, 0) != MH_MAGIC_64 {
        return Err("not a 64-bit Mach-O file".to_owned());
    }
    let mut m = MachO {
        cpu: u32le(d, 4),
        cpusub: u32le(d, 8),
        segs: Vec::new(),
        sects: Vec::new(),
        syms: Vec::new(),
        uuid: None,
        build: None,
    };
    let ncmds = u32le(d, 16);
    let mut at = 32;
    for _ in 0..ncmds {
        let (cmd, size) = (u32le(d, at), u32le(d, at + 4) as usize);
        if size < 8 || at + size > d.len() {
            return Err("truncated load commands".to_owned());
        }
        match cmd {
            LC_SEGMENT_64 => {
                let mut seg = Seg {
                    name: name16(d, at + 8).to_owned(),
                    vmaddr: u64le(d, at + 24),
                    vmsize: u64le(d, at + 32),
                    maxprot: u32le(d, at + 56),
                    initprot: u32le(d, at + 60),
                    flags: u32le(d, at + 68),
                    sects: Vec::new(),
                };
                for k in 0..u32le(d, at + 64) as usize {
                    let s = at + 72 + k * 80;
                    seg.sects.push(m.sects.len());
                    m.sects.push(Sect {
                        name: name16(d, s).to_owned(),
                        seg: name16(d, s + 16).to_owned(),
                        addr: u64le(d, s + 32),
                        size: u64le(d, s + 40),
                        offset: u32le(d, s + 48),
                        align: u32le(d, s + 52),
                        reloff: u32le(d, s + 56),
                        nreloc: u32le(d, s + 60),
                        flags: u32le(d, s + 64),
                    });
                }
                m.segs.push(seg);
            }
            LC_SYMTAB => {
                let (symoff, nsyms) = (u32le(d, at + 8) as usize, u32le(d, at + 12) as usize);
                let stroff = u32le(d, at + 16) as usize;
                for i in 0..nsyms {
                    let e = symoff + i * 16;
                    if e + 16 > d.len() {
                        return Err("truncated symbol table".to_owned());
                    }
                    m.syms.push(Sym {
                        name: cstr(d, stroff + u32le(d, e) as usize).to_owned(),
                        ty: d[e + 4],
                        sect: d[e + 5],
                        desc: u16le(d, e + 6),
                        value: u64le(d, e + 8),
                    });
                }
            }
            LC_UUID => {
                let mut u = [0; 16];
                u.copy_from_slice(&d[at + 8..at + 24]);
                m.uuid = Some(u);
            }
            LC_BUILD_VERSION => m.build = Some(d[at..at + size].to_vec()),
            _ => {}
        }
        at += size;
    }
    Ok(m)
}

/// One object of the debug map: its path and its symbols' linked
/// addresses (`(name, address, size)`; data has no size).
struct MapObject {
    oso: String,
    syms: Vec<(String, u64, Option<u64>)>,
}

fn debug_map(exe: &MachO) -> Vec<MapObject> {
    let globals: HashMap<&str, u64> = exe
        .syms
        .iter()
        .filter(|s| s.ty & N_STAB == 0 && s.ty & N_TYPE == N_SECT)
        .map(|s| (s.name.as_str(), s.value))
        .collect();
    let mut out: Vec<MapObject> = Vec::new();
    let mut cur: Option<MapObject> = None;
    for s in &exe.syms {
        match s.ty {
            N_OSO => {
                out.extend(cur.take());
                cur = Some(MapObject {
                    oso: s.name.clone(),
                    syms: Vec::new(),
                });
            }
            N_SO if s.name.is_empty() => out.extend(cur.take()),
            N_FUN => {
                let Some(o) = cur.as_mut() else { continue };
                if s.name.is_empty() {
                    if let Some(last) = o.syms.last_mut() {
                        last.2 = Some(s.value);
                    }
                } else {
                    o.syms.push((s.name.clone(), s.value, None));
                }
            }
            N_STSYM | N_GSYM => {
                let Some(o) = cur.as_mut() else { continue };
                // ld64 leaves a global's address to the symbol table.
                let addr = if s.value == 0 {
                    globals.get(s.name.as_str()).copied().unwrap_or(0)
                } else {
                    s.value
                };
                o.syms.push((s.name.clone(), addr, None));
            }
            _ => {}
        }
    }
    out.extend(cur);
    out
}

/// The bytes of `oso`: a file, or `archive(member)`.
fn load(oso: &str, cpu: u32) -> Result<Vec<u8>, String> {
    let arch = if cpu == CPU_TYPE_X86_64 {
        "x86_64"
    } else {
        "arm64"
    };
    let (path, member) = match oso.strip_suffix(')').and_then(|s| s.rsplit_once('(')) {
        Some((p, m)) if !Path::new(oso).exists() => (p, Some(m)),
        _ => (oso, None),
    };
    let raw = std::fs::read(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    let raw = crate::macho::object::thin_slice(&raw, arch)
        .ok_or_else(|| format!("{path}: no {arch} slice"))?;
    let Some(member) = member else {
        return Ok(raw.to_vec());
    };
    let a = crate::archive::parse(raw).map_err(|e| format!("{path}: {e}"))?;
    a.members
        .iter()
        .find(|m| m.name == member)
        .map(|m| m.data.to_vec())
        .ok_or_else(|| format!("{path}: no member {member}"))
}

/// Object addresses to linked ones: `(object start, object end, linked
/// start)`, by start.
struct AddrMap(Vec<(u64, u64, u64)>);

impl AddrMap {
    fn new(obj: &MachO, linked: &[(String, u64, Option<u64>)]) -> Self {
        // Each section's defined symbols by address, for the ends of
        // those the map gives no size.
        let mut by_sect: HashMap<u8, Vec<u64>> = HashMap::default();
        let mut by_name: HashMap<&str, (u8, u64)> = HashMap::default();
        for s in &obj.syms {
            if s.ty & N_STAB == 0 && s.ty & N_TYPE == N_SECT {
                by_sect.entry(s.sect).or_default().push(s.value);
                by_name.entry(s.name.as_str()).or_insert((s.sect, s.value));
            }
        }
        for v in by_sect.values_mut() {
            v.sort_unstable();
            v.dedup();
        }
        let mut v = Vec::new();
        for (name, addr, size) in linked {
            let Some(&(sect, start)) = by_name.get(name.as_str()) else {
                continue;
            };
            let end = size.map_or_else(
                || {
                    let next = by_sect[&sect].iter().find(|&&a| a > start);
                    let sect_end = obj
                        .sects
                        .get(usize::from(sect).wrapping_sub(1))
                        .map_or(start, |s| s.addr + s.size);
                    next.copied().unwrap_or(sect_end).max(start)
                },
                |n| start + n,
            );
            v.push((start, end, *addr));
        }
        v.sort_unstable();
        Self(v)
    }

    /// The linked address of object address `a`, in a symbol's range. An
    /// end address is never asked for: it follows its start.
    fn get(&self, a: u64) -> Option<u64> {
        let i = self.0.partition_point(|e| e.0 <= a);
        let first = self.0[..i].last()?.0;
        self.0[..i]
            .iter()
            .rev()
            .take_while(|e| e.0 == first)
            .find(|e| a < e.1)
            .map(|e| e.2 + (a - e.0))
    }
}

/// A debug section of an object: its bytes and, by offset, the object
/// address each relocated field holds (`width` 4 or 8).
struct DebugSect {
    data: Vec<u8>,
    relocs: HashMap<usize, (u64, usize)>,
}

impl DebugSect {
    fn read(obj: &MachO, raw: &[u8], name: &str) -> Self {
        let Some(s) = obj
            .sects
            .iter()
            .find(|s| s.seg == "__DWARF" && s.name == name)
        else {
            return Self {
                data: Vec::new(),
                relocs: HashMap::default(),
            };
        };
        let (off, size) = (s.offset as usize, s.size as usize);
        let data = raw.get(off..off + size).unwrap_or_default().to_vec();
        let mut relocs = HashMap::default();
        let mut skip = false;
        for k in 0..s.nreloc as usize {
            let r = s.reloff as usize + k * 8;
            let (addr, info) = (u32le(raw, r), u32le(raw, r + 4));
            if std::mem::take(&mut skip) || addr & 0x8000_0000 != 0 {
                continue;
            }
            let ty = info >> 28;
            let sub = if obj.cpu == CPU_TYPE_X86_64 {
                X86_64_RELOC_SUBTRACTOR
            } else {
                ARM64_RELOC_SUBTRACTOR
            };
            if ty == sub {
                // A difference within the object: it holds.
                skip = true;
                continue;
            }
            let width = match (info >> 25) & 3 {
                2 => 4,
                3 => 8,
                _ => continue,
            };
            let at = addr as usize;
            if ty != RELOC_UNSIGNED || at + width > data.len() {
                continue;
            }
            let held = if width == 4 {
                u64::from(u32le(&data, at))
            } else {
                u64le(&data, at)
            };
            let value = if info & (1 << 27) != 0 {
                match obj.syms.get((info & 0x00ff_ffff) as usize) {
                    Some(sym) if sym.ty & N_TYPE == N_SECT => sym.value.wrapping_add(held),
                    _ => continue,
                }
            } else {
                held
            };
            relocs.insert(at, (value, width));
        }
        Self { data, relocs }
    }

    /// The field of `width` bytes at `at`: the object address a relocation
    /// gives, else the bytes.
    fn value(&self, at: usize, width: usize) -> u64 {
        if let Some(&(v, _)) = self.relocs.get(&at) {
            return v;
        }
        match width {
            1 => u64::from(self.data[at]),
            2 => u64::from(u16le(&self.data, at)),
            4 => u64::from(u32le(&self.data, at)),
            _ => u64le(&self.data, at),
        }
    }
}

fn put(b: &mut [u8], at: usize, width: usize, v: u64) {
    if let Some(s) = b.get_mut(at..at + width) {
        s.copy_from_slice(&v.to_le_bytes()[..width]);
    }
}

#[derive(Clone, Copy)]
struct Attr {
    name: u64,
    form: u64,
}

struct Abbrev {
    tag: u64,
    children: bool,
    attrs: Vec<Attr>,
}

fn abbrevs(d: &[u8], mut at: usize) -> Option<HashMap<u64, Abbrev>> {
    let mut t = HashMap::default();
    loop {
        let code = read_uleb(d, &mut at)?;
        if code == 0 {
            return Some(t);
        }
        let tag = read_uleb(d, &mut at)?;
        let children = *d.get(at)? != 0;
        at += 1;
        let mut attrs = Vec::new();
        loop {
            let (name, form) = (read_uleb(d, &mut at)?, read_uleb(d, &mut at)?);
            if name == 0 && form == 0 {
                break;
            }
            if form == 0x21 {
                // DW_FORM_implicit_const (DWARF 5): its value.
                read_sleb(d, &mut at)?;
            }
            attrs.push(Attr { name, form });
        }
        t.insert(
            code,
            Abbrev {
                tag,
                children,
                attrs,
            },
        );
    }
}

type High = (usize, usize, bool, u64);

/// Where an object's sections start in the bundle's.
#[derive(Clone, Copy, Default)]
struct Bases {
    abbrev: u64,
    info: u64,
    line: u64,
    str: u64,
    ranges: u64,
    loc: u64,
}

/// A list in `.debug_ranges` / `.debug_loc` and the unit it belongs to.
struct ListUse {
    loc: bool,
    off: usize,
}

/// One unit's address fields as found: the unit's `low_pc` (offset in
/// `.debug_info`, width, object address), its `high_pc` (offset, form
/// width, an address or a length), and its lists.
#[derive(Default)]
struct Unit {
    low: Option<(usize, usize, u64)>,
    high: Option<(usize, usize, bool)>,
    lists: Vec<ListUse>,
    /// Linked `[lo, hi)` of its live subprograms.
    code: Vec<(u64, u64)>,
}

/// The object's debug sections moved: `info`, `line`, `ranges`, `loc`
/// relocated in place, offsets rebased. Its units' code for aranges.
struct Moved {
    sects: [Vec<u8>; 7],
    /// `(offset of the unit in .debug_info, its code ranges)`.
    aranges: Vec<(u64, Vec<(u64, u64)>)>,
}

fn unsupported(what: &str) -> String {
    format!("{what}: not in a .dSYM")
}

/// An attribute of an entry as found: its value is `width` bytes at
/// `pos` (0: not a fixed-size form, but `ref_udata`'s length).
#[derive(Clone, Copy)]
struct AttrAt {
    name: u64,
    form: u64,
    pos: usize,
    width: usize,
}

struct Die {
    off: usize,
    tag: u64,
    attrs: Vec<AttrAt>,
    /// Past its attributes: its children, if it has any.
    body: usize,
    /// Past its last child (or itself).
    end: usize,
}

struct UnitAt {
    start: usize,
    end: usize,
    version: u16,
    aw: usize,
    dies: Vec<Die>,
}

const FORM_ADDR: u64 = 0x01;
const FORM_DATA4: u64 = 0x06;
const FORM_DATA8: u64 = 0x07;
const FORM_STRP: u64 = 0x0e;
const FORM_REF_ADDR: u64 = 0x10;
const FORM_REF_UDATA: u64 = 0x15;
const FORM_SEC_OFFSET: u64 = 0x17;

/// The units of `.debug_info` `d` and their entries.
fn walk(d: &[u8], abbrev: &[u8]) -> Result<Vec<UnitAt>, String> {
    let bad = || "bad .debug_info".to_owned();
    let mut tables: HashMap<usize, HashMap<u64, Abbrev>> = HashMap::default();
    let mut units = Vec::new();
    let mut at = 0;
    while at + 11 <= d.len() {
        let len = u32le(d, at) as usize;
        if len >= 0xffff_fff0 {
            return Err(unsupported("64-bit DWARF"));
        }
        let end = (at + 4 + len).min(d.len());
        let version = u16le(d, at + 4);
        if !(2..=4).contains(&version) {
            return Err(unsupported(&format!("DWARF {version}")));
        }
        let aoff = u32le(d, at + 6) as usize;
        let aw = usize::from(d[at + 10]);
        if let std::collections::hash_map::Entry::Vacant(e) = tables.entry(aoff) {
            e.insert(abbrevs(abbrev, aoff).ok_or("bad .debug_abbrev")?);
        }
        let table = &tables[&aoff];
        let mut dies: Vec<Die> = Vec::new();
        let mut open: Vec<usize> = Vec::new();
        let mut p = at + 11;
        while p < end {
            let off = p;
            let code = read_uleb(d, &mut p).ok_or_else(bad)?;
            if code == 0 {
                if let Some(k) = open.pop() {
                    dies[k].end = p;
                }
                continue;
            }
            let ab = table.get(&code).ok_or("bad abbreviation code")?;
            let mut attrs = Vec::new();
            for a in &ab.attrs {
                let mut form = a.form;
                while form == 0x16 {
                    form = read_uleb(d, &mut p).ok_or_else(bad)?;
                }
                let pos = p;
                let width = match form {
                    FORM_ADDR => aw,
                    0x0b | 0x0c | 0x11 => 1,
                    0x05 | 0x12 => 2,
                    FORM_REF_ADDR if version == 2 => aw,
                    FORM_DATA4 | FORM_STRP | FORM_REF_ADDR | 0x13 | FORM_SEC_OFFSET => 4,
                    FORM_DATA8 | 0x14 | 0x20 => 8,
                    0x19 => 0,
                    0x08 => {
                        p += d[p..].iter().position(|&c| c == 0).ok_or_else(bad)? + 1;
                        0
                    }
                    0x0d => {
                        read_sleb(d, &mut p).ok_or_else(bad)?;
                        0
                    }
                    0x0f => {
                        read_uleb(d, &mut p).ok_or_else(bad)?;
                        0
                    }
                    FORM_REF_UDATA => {
                        read_uleb(d, &mut p).ok_or_else(bad)?;
                        attrs.push(AttrAt {
                            name: a.name,
                            form,
                            pos,
                            width: p - pos,
                        });
                        continue;
                    }
                    0x09 | 0x18 => {
                        let n = read_uleb(d, &mut p).ok_or_else(bad)?;
                        p += n as usize;
                        0
                    }
                    0x0a => {
                        p += 1 + usize::from(*d.get(p).ok_or_else(bad)?);
                        0
                    }
                    0x03 => {
                        p += 2 + usize::from(u16le(d, p));
                        0
                    }
                    0x04 => {
                        p += 4 + u32le(d, p) as usize;
                        0
                    }
                    f => return Err(unsupported(&format!("DW_FORM {f:#x}"))),
                };
                if p == pos {
                    p += width;
                }
                if p > end {
                    return Err(bad());
                }
                attrs.push(AttrAt {
                    name: a.name,
                    form,
                    pos,
                    width: if p - pos == width { width } else { 0 },
                });
            }
            if ab.children {
                open.push(dies.len());
            }
            dies.push(Die {
                off,
                tag: ab.tag,
                attrs,
                body: p,
                end: p,
            });
        }
        for k in open {
            dies[k].end = end;
        }
        units.push(UnitAt {
            start: at,
            end,
            version,
            aw,
            dies,
        });
        at = end;
    }
    Ok(units)
}

/// `[start, end)` byte ranges left out of `.debug_info`, sorted: where an
/// offset moves to.
struct Dropped(Vec<(usize, usize)>);

impl Dropped {
    fn contains(&self, o: usize) -> bool {
        let i = self.0.partition_point(|r| r.0 <= o);
        i > 0 && o < self.0[i - 1].1
    }

    /// Leave out `[s, e)` too.
    fn add(&mut self, s: usize, e: usize) {
        let i = self.0.partition_point(|r| r.1 < s);
        let mut j = i;
        let (mut lo, mut hi) = (s, e);
        while j < self.0.len() && self.0[j].0 <= hi {
            lo = lo.min(self.0[j].0);
            hi = hi.max(self.0[j].1);
            j += 1;
        }
        self.0.splice(i..j, [(lo, hi)]);
    }

    /// Is all of `[s, e)` left out (or empty)?
    fn covers(&self, s: usize, e: usize) -> bool {
        if s >= e {
            return true;
        }
        let i = self.0.partition_point(|r| r.0 <= s);
        i > 0 && self.0[i - 1].1 >= e
    }

    fn shift(&self, o: usize) -> usize {
        o - self
            .0
            .iter()
            .take_while(|r| r.1 <= o)
            .map(|r| r.1 - r.0)
            .sum::<usize>()
    }
}

/// Move one object's DWARF. `Ok(None)`: it has none.
///
/// Subprograms of code the link dropped go, with their children, unless
/// something refers into them.
#[allow(clippy::too_many_lines)]
fn move_object(obj: &MachO, raw: &[u8], map: &AddrMap, b: Bases) -> Result<Option<Moved>, String> {
    let read = |n: &str| DebugSect::read(obj, raw, n);
    let info = read("__debug_info");
    if info.data.is_empty() {
        return Ok(None);
    }
    let abbrev = read("__debug_abbrev");
    let line = read("__debug_line");
    let str_ = read("__debug_str");
    let ranges = read("__debug_ranges");
    let loc = read("__debug_loc");
    let units = walk(&info.data, &abbrev.data)?;

    // Dead code's entries, kept when referred to.
    let mut targets = Vec::new();
    let mut dead = Vec::new();
    for u in &units {
        for e in &u.dies {
            for a in &e.attrs {
                match a.form {
                    0x11..=0x14 => targets.push(u.start + info.value(a.pos, a.width) as usize),
                    FORM_REF_UDATA => {
                        let mut p = a.pos;
                        let v = read_uleb(&info.data, &mut p).unwrap_or(0);
                        targets.push(u.start + v as usize);
                    }
                    FORM_REF_ADDR => targets.push(info.value(a.pos, a.width) as usize),
                    FORM_ADDR if e.tag == DW_TAG_SUBPROGRAM && a.name == DW_AT_LOW_PC => {
                        if map.get(info.value(a.pos, a.width)).is_none() {
                            dead.push((e.off, e.end));
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    targets.sort_unstable();
    let unreferenced = |s: usize, e: usize| {
        let i = targets.partition_point(|&t| t < s);
        targets.get(i).is_none_or(|&t| t >= e)
    };
    let mut dropped = Dropped(Vec::new());
    for (s, e) in dead {
        if unreferenced(s, e) {
            dropped.add(s, e);
        }
    }
    // Namespaces left empty, innermost first.
    for u in &units {
        for e in u.dies.iter().rev() {
            if e.tag == DW_TAG_NAMESPACE
                && e.end > e.body
                && dropped.covers(e.body, e.end - 1)
                && unreferenced(e.off, e.end)
            {
                dropped.add(e.off, e.end);
            }
        }
    }

    // Every relocated address to its linked one, 0 for dropped code.
    let reloc = |s: &DebugSect| {
        let mut d = s.data.clone();
        for (&at, &(v, w)) in &s.relocs {
            put(&mut d, at, w, map.get(v).unwrap_or(0));
        }
        d
    };
    let mut out_info = reloc(&info);
    let mut out_ranges = reloc(&ranges);
    let mut out_loc = reloc(&loc);

    let mut lines_seen = HashSet::default();
    let mut aranges = Vec::new();
    let mut lists_done = HashSet::default();
    for u in &units {
        let aoff = u64::from(u32le(&info.data, u.start + 6));
        put(&mut out_info, u.start + 6, 4, aoff + b.abbrev);
        let len = dropped.shift(u.end) - dropped.shift(u.start) - 4;
        put(&mut out_info, u.start, 4, len as u64);
        let aw = u.aw;
        let mut unit = Unit::default();
        for (k, e) in u.dies.iter().enumerate() {
            if dropped.contains(e.off) {
                continue;
            }
            // `low_pc`: (offset, object address); `high_pc`: (offset,
            // width, an address, value).
            let mut lo: Option<(usize, u64)> = None;
            let mut hi: Option<High> = None;
            for a in &e.attrs {
                let (start, width, form) = (a.pos, a.width, a.form);
                let raw_v = || info.value(start, width);
                let offset_form = form == FORM_SEC_OFFSET
                    || (u.version < 4 && matches!(form, FORM_DATA4 | FORM_DATA8));
                match (a.name, form) {
                    (_, 0x11..=0x14) => {
                        let t = u.start + raw_v() as usize;
                        let v = dropped.shift(t) - dropped.shift(u.start);
                        put(&mut out_info, start, width, v as u64);
                    }
                    (_, FORM_REF_UDATA) => {
                        let mut p = start;
                        let t = u.start + read_uleb(&info.data, &mut p).unwrap_or(0) as usize;
                        let v = dropped.shift(t) - dropped.shift(u.start);
                        put_uleb(&mut out_info[start..start + width], v as u64);
                    }
                    (_, FORM_STRP) => put(&mut out_info, start, 4, raw_v() + b.str),
                    (_, FORM_REF_ADDR) => {
                        let v = dropped.shift(raw_v() as usize) as u64;
                        put(&mut out_info, start, width, v + b.info);
                    }
                    (DW_AT_LOW_PC, FORM_ADDR) => lo = Some((start, raw_v())),
                    (DW_AT_HIGH_PC, _) if width > 0 => {
                        hi = Some((start, width, form == FORM_ADDR, raw_v()));
                    }
                    (DW_AT_STMT_LIST, FORM_DATA4 | FORM_SEC_OFFSET) => {
                        let off = raw_v();
                        put(&mut out_info, start, 4, off + b.line);
                        let v = u16le(&line.data, off as usize + 4);
                        if lines_seen.insert(off) && v >= 5 {
                            return Err(unsupported(&format!("a DWARF {v} line table")));
                        }
                    }
                    (DW_AT_RANGES, _) if offset_form => {
                        let off = raw_v();
                        put(&mut out_info, start, width, off + b.ranges);
                        unit.lists.push(ListUse {
                            loc: false,
                            off: off as usize,
                        });
                    }
                    (
                        DW_AT_LOCATION
                        | DW_AT_FRAME_BASE
                        | DW_AT_STRING_LENGTH
                        | DW_AT_RETURN_ADDR
                        | DW_AT_DATA_MEMBER_LOCATION
                        | DW_AT_SEGMENT
                        | DW_AT_STATIC_LINK
                        | DW_AT_USE_LOCATION
                        | DW_AT_VTABLE_ELEM_LOCATION,
                        _,
                    ) if offset_form => {
                        let off = raw_v();
                        put(&mut out_info, start, width, off + b.loc);
                        unit.lists.push(ListUse {
                            loc: true,
                            off: off as usize,
                        });
                    }
                    _ => {}
                }
            }
            // An end address follows its start: code moves whole.
            let new_lo = lo.and_then(|(_, v)| map.get(v));
            if let (Some((_, obj_lo)), Some((hs, hw, true, obj_hi))) = (lo, hi) {
                let v = new_lo.map_or(0, |n| n + obj_hi.saturating_sub(obj_lo));
                put(&mut out_info, hs, hw, v);
            }
            if k == 0 {
                unit.low = lo.map(|(s, v)| (s, aw, v));
                unit.high = hi.map(|(s, w, addr, _)| (s, w, addr));
            } else if e.tag == DW_TAG_SUBPROGRAM {
                if let (Some(n), Some((_, _, addr, h))) = (new_lo, hi) {
                    let size = if addr {
                        h.saturating_sub(lo.map_or(0, |l| l.1))
                    } else {
                        h
                    };
                    unit.code.push((n, n + size));
                }
            }
        }

        // The unit spans its live code; lists are relative to its base.
        unit.code.sort_unstable();
        let obj_base = unit.low.map_or(0, |l| l.2);
        let new_base = match (unit.low, unit.code.first()) {
            (Some(_), Some(&(lo, _))) => lo,
            (Some(l), None) => map.get(l.2).unwrap_or(0),
            (None, _) => 0,
        };
        if let Some((s, w, _)) = unit.low {
            put(&mut out_info, s, w, new_base);
        }
        if let (Some((s, w, addr)), Some(&(_, last))) = (unit.high, unit.code.last()) {
            let hi = unit.code.iter().map(|c| c.1).max().unwrap_or(last);
            put(&mut out_info, s, w, if addr { hi } else { hi - new_base });
        }
        for l in &unit.lists {
            if !lists_done.insert((l.loc, l.off)) {
                continue;
            }
            let (src, dst) = if l.loc {
                (&loc, &mut out_loc)
            } else {
                (&ranges, &mut out_ranges)
            };
            move_list(src, dst, l.off, aw, l.loc, (obj_base, new_base), map);
        }
        if !unit.code.is_empty() {
            aranges.push((dropped.shift(u.start) as u64 + b.info, unit.code));
        }
    }

    // What stays of .debug_info.
    let mut kept = Vec::with_capacity(out_info.len());
    let mut from = 0;
    for &(s, e) in &dropped.0 {
        kept.extend_from_slice(&out_info[from..s]);
        from = e;
    }
    kept.extend_from_slice(&out_info[from..]);

    // Line tables need only their addresses moved.
    Ok(Some(Moved {
        sects: [
            abbrev.data,
            kept,
            reloc(&line),
            str_.data,
            out_ranges,
            out_loc,
            Vec::new(),
        ],
        aranges,
    }))
}

/// Rewrite the range or location list at `off` of `src` into `dst`:
/// each entry at its code's linked address, relative to the unit's new
/// base; an entry of dropped code empty.
fn move_list(
    src: &DebugSect,
    dst: &mut [u8],
    mut off: usize,
    aw: usize,
    loc: bool,
    (mut obj_base, mut new_base): (u64, u64),
    map: &AddrMap,
) {
    let all = if aw == 4 {
        u64::from(u32::MAX)
    } else {
        u64::MAX
    };
    let mask = all;
    while off + 2 * aw <= src.data.len() {
        let (b, e) = (src.value(off, aw), src.value(off + aw, aw));
        if b == 0 && e == 0 && !src.relocs.contains_key(&off) {
            return;
        }
        if b == all {
            // A base address selection entry.
            obj_base = e;
            new_base = map.get(e).unwrap_or(0);
            put(dst, off + aw, aw, new_base);
            off += 2 * aw;
            continue;
        }
        let at = |v: u64| {
            if src.relocs.contains_key(&off) {
                v
            } else {
                obj_base.wrapping_add(v)
            }
        };
        let (ob, oe) = (at(b), at(e));
        if let Some(n) = map.get(ob) {
            let rel = n.wrapping_sub(new_base) & mask;
            put(dst, off, aw, rel);
            put(
                dst,
                off + aw,
                aw,
                rel.wrapping_add(oe.wrapping_sub(ob)) & mask,
            );
        } else {
            put(dst, off, aw, 1);
            put(dst, off + aw, aw, 1);
        }
        off += 2 * aw;
        if loc {
            off += 2 + usize::from(u16le(&src.data, off));
        }
    }
}

fn aranges(units: &[(u64, Vec<(u64, u64)>)]) -> Vec<u8> {
    let mut b = Vec::new();
    for (info, code) in units {
        let start = b.len();
        put32(&mut b, 0);
        b.extend_from_slice(&2u16.to_le_bytes());
        put32(&mut b, *info as u32);
        b.extend_from_slice(&[8, 0]);
        b.resize(start + 16, 0);
        // Adjacent ranges merged.
        let mut merged: Vec<(u64, u64)> = Vec::new();
        for &(lo, hi) in code {
            match merged.last_mut() {
                Some(m) if lo <= m.1 => m.1 = m.1.max(hi),
                _ => merged.push((lo, hi)),
            }
        }
        for (lo, hi) in merged {
            put64(&mut b, lo);
            put64(&mut b, hi - lo);
        }
        put64(&mut b, 0);
        put64(&mut b, 0);
        let len = (b.len() - start - 4) as u32;
        b[start..start + 4].copy_from_slice(&len.to_le_bytes());
    }
    b
}

/// The DWARF of `exe`'s objects, by [`SECTS`]. Objects that cannot be
/// read or moved are reported in `warnings` and left out.
fn gather(exe: &MachO, warnings: &mut Vec<String>) -> [Vec<u8>; 7] {
    let mut out: [Vec<u8>; 7] = Default::default();
    let mut units = Vec::new();
    for m in debug_map(exe) {
        let raw = match load(&m.oso, exe.cpu) {
            Ok(r) => r,
            Err(e) => {
                warnings.push(e);
                continue;
            }
        };
        let obj = match parse(&raw) {
            Ok(o) => o,
            Err(e) => {
                warnings.push(format!("{}: {e}", m.oso));
                continue;
            }
        };
        let map = AddrMap::new(&obj, &m.syms);
        let len = |i: usize| out[i].len() as u64;
        let b = Bases {
            abbrev: len(ABBREV),
            info: len(INFO),
            line: len(LINE),
            str: len(STR),
            ranges: len(RANGES),
            loc: len(LOC),
        };
        match move_object(&obj, &raw, &map, b) {
            Ok(Some(moved)) => {
                for (o, s) in out.iter_mut().zip(moved.sects) {
                    o.extend_from_slice(&s);
                }
                units.extend(moved.aranges);
            }
            Ok(None) => {}
            Err(e) => warnings.push(format!("{}: {e}", m.oso)),
        }
    }
    out[ARANGES] = aranges(&units);
    out
}

fn segment(
    h: &mut Vec<u8>,
    seg: &Seg,
    sects: &[(&Sect, u64, u32)],
    (fileoff, filesize): (u64, u64),
) {
    put32(h, LC_SEGMENT_64);
    put32(h, 72 + 80 * sects.len() as u32);
    let mut name = [0u8; 16];
    name[..seg.name.len().min(16)].copy_from_slice(&seg.name.as_bytes()[..seg.name.len().min(16)]);
    h.extend_from_slice(&name);
    put64(h, seg.vmaddr);
    put64(h, seg.vmsize);
    put64(h, fileoff);
    put64(h, filesize);
    put32(h, seg.maxprot);
    put32(h, seg.initprot);
    put32(h, sects.len() as u32);
    put32(h, seg.flags);
    for (s, addr, offset) in sects {
        for n in [&s.name, &s.seg] {
            let mut f = [0u8; 16];
            f[..n.len().min(16)].copy_from_slice(&n.as_bytes()[..n.len().min(16)]);
            h.extend_from_slice(&f);
        }
        put64(h, *addr);
        put64(h, s.size);
        put32(h, *offset);
        put32(h, s.align);
        put32(h, 0);
        put32(h, 0);
        put32(h, s.flags);
        put32(h, 0);
        put32(h, 0);
        put32(h, 0);
    }
}

/// The `MH_DSYM` file of `exe` with DWARF `dwarf`.
fn dsym_file(exe: &MachO, dwarf: &[Vec<u8>; 7]) -> Vec<u8> {
    // The symbol table without the debug map.
    let mut strtab = vec![b' ', 0];
    let mut nlist = Vec::new();
    for s in exe.syms.iter().filter(|s| s.ty & N_STAB == 0) {
        let strx = strtab.len() as u32;
        strtab.extend_from_slice(s.name.as_bytes());
        strtab.push(0);
        put32(&mut nlist, strx);
        nlist.extend_from_slice(&[s.ty, s.sect]);
        nlist.extend_from_slice(&s.desc.to_le_bytes());
        put64(&mut nlist, s.value);
    }
    let nsyms = (nlist.len() / 16) as u32;
    while strtab.len() % 8 != 0 {
        strtab.push(0);
    }

    let kept: Vec<&Seg> = exe.segs.iter().filter(|s| s.name != "__DWARF").collect();
    let dwarf_sects: Vec<usize> = (0..SECTS.len()).filter(|&i| !dwarf[i].is_empty()).collect();
    let mut cmds = 24 + 24 + 72 + 80 * dwarf_sects.len();
    cmds += exe.build.as_ref().map_or(0, Vec::len);
    for s in &kept {
        cmds += 72 + 80 * s.sects.len();
    }
    let linkedit = (32 + cmds as u64).next_multiple_of(PAGE);
    let symoff = linkedit;
    let stroff = symoff + nlist.len() as u64;
    let dwarf_off = (stroff + strtab.len() as u64).next_multiple_of(PAGE);
    let dwarf_size: u64 = dwarf_sects.iter().map(|&i| dwarf[i].len() as u64).sum();
    let vm_end = exe
        .segs
        .iter()
        .map(|s| s.vmaddr + s.vmsize)
        .max()
        .unwrap_or(0)
        .next_multiple_of(PAGE);

    let mut h = Vec::new();
    put32(&mut h, MH_MAGIC_64);
    put32(&mut h, exe.cpu);
    put32(&mut h, exe.cpusub);
    put32(&mut h, MH_DSYM);
    put32(
        &mut h,
        3 + kept.len() as u32 + u32::from(exe.build.is_some()),
    );
    put32(&mut h, cmds as u32);
    put32(&mut h, 0);
    put32(&mut h, 0);
    put32(&mut h, LC_UUID);
    put32(&mut h, 24);
    h.extend_from_slice(&exe.uuid.unwrap_or_default());
    if let Some(b) = &exe.build {
        h.extend_from_slice(b);
    }
    put32(&mut h, LC_SYMTAB);
    put32(&mut h, 24);
    put32(&mut h, symoff as u32);
    put32(&mut h, nsyms);
    put32(&mut h, stroff as u32);
    put32(&mut h, strtab.len() as u32);
    for seg in &kept {
        let sects: Vec<(&Sect, u64, u32)> = seg
            .sects
            .iter()
            .map(|&i| (&exe.sects[i], exe.sects[i].addr, 0))
            .collect();
        let file = if seg.name == "__LINKEDIT" {
            (symoff, dwarf_off - symoff)
        } else {
            (0, 0)
        };
        segment(&mut h, seg, &sects, file);
    }
    let names: Vec<Sect> = dwarf_sects
        .iter()
        .map(|&i| Sect {
            seg: "__DWARF".to_owned(),
            name: SECTS[i].to_owned(),
            addr: 0,
            size: dwarf[i].len() as u64,
            offset: 0,
            align: 0,
            reloff: 0,
            nreloc: 0,
            flags: S_ATTR_DEBUG,
        })
        .collect();
    let mut off = dwarf_off;
    let mut placed = Vec::new();
    for s in &names {
        placed.push((s, vm_end + (off - dwarf_off), off as u32));
        off += s.size;
    }
    let dwarf_seg = Seg {
        name: "__DWARF".to_owned(),
        vmaddr: vm_end,
        vmsize: dwarf_size.next_multiple_of(PAGE),
        maxprot: 7,
        initprot: 3,
        flags: 0,
        sects: Vec::new(),
    };
    segment(&mut h, &dwarf_seg, &placed, (dwarf_off, dwarf_size));
    debug_assert_eq!(h.len(), 32 + cmds);

    h.resize(symoff as usize, 0);
    h.extend_from_slice(&nlist);
    h.extend_from_slice(&strtab);
    h.resize(dwarf_off as usize, 0);
    for &i in &dwarf_sects {
        h.extend_from_slice(&dwarf[i]);
    }
    h
}

fn info_plist(name: &str) -> String {
    let esc = name
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         \t<dict>\n\
         \t\t<key>CFBundleDevelopmentRegion</key>\n\t\t<string>English</string>\n\
         \t\t<key>CFBundleIdentifier</key>\n\t\t<string>com.apple.xcode.dsym.{esc}</string>\n\
         \t\t<key>CFBundleInfoDictionaryVersion</key>\n\t\t<string>6.0</string>\n\
         \t\t<key>CFBundlePackageType</key>\n\t\t<string>dSYM</string>\n\
         \t\t<key>CFBundleSignature</key>\n\t\t<string>????</string>\n\
         \t\t<key>CFBundleShortVersionString</key>\n\t\t<string>1.0</string>\n\
         \t\t<key>CFBundleVersion</key>\n\t\t<string>1</string>\n\
         \t</dict>\n\
         </plist>\n"
    )
}

/// Write `<exe>.dSYM` beside `exe` from the objects its debug map names,
/// which must still exist. Returns the objects left out and why.
pub fn write(exe: &Path) -> Result<Vec<String>, String> {
    let raw = std::fs::read(exe).map_err(|e| format!("cannot read {}: {e}", exe.display()))?;
    let m = parse(&raw).map_err(|e| format!("{}: {e}", exe.display()))?;
    if m.uuid.is_none() {
        return Err(format!("{}: no LC_UUID", exe.display()));
    }
    let mut warnings = Vec::new();
    let dwarf = gather(&m, &mut warnings);
    let file = dsym_file(&m, &dwarf);

    let name = exe
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let mut bundle = exe.as_os_str().to_owned();
    bundle.push(".dSYM");
    let bundle = PathBuf::from(bundle);
    let io = |p: &Path, e: std::io::Error| format!("cannot write {}: {e}", p.display());
    let _ = std::fs::remove_dir_all(&bundle);
    let dir = bundle.join("Contents/Resources/DWARF");
    std::fs::create_dir_all(&dir).map_err(|e| io(&dir, e))?;
    let plist = bundle.join("Contents/Info.plist");
    std::fs::write(&plist, info_plist(&name)).map_err(|e| io(&plist, e))?;
    let dw = dir.join(&name);
    std::fs::write(&dw, file).map_err(|e| io(&dw, e))?;
    Ok(warnings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_addresses_by_symbol() {
        let m = AddrMap(vec![(0, 0x10, 0x1000), (0x10, 0x30, 0x2000)]);
        assert_eq!(m.get(0), Some(0x1000));
        assert_eq!(m.get(0xf), Some(0x100f));
        assert_eq!(m.get(0x10), Some(0x2000));
        // Past the last function: dropped code.
        assert_eq!(m.get(0x30), None);
        assert_eq!(AddrMap(Vec::new()).get(0), None);
    }

    #[test]
    fn drops_merge_and_shift() {
        let mut d = Dropped(Vec::new());
        d.add(10, 20);
        d.add(30, 40);
        d.add(18, 31);
        assert_eq!(d.0, [(10, 40)]);
        d.add(50, 60);
        assert!(d.covers(12, 39) && !d.covers(39, 41) && d.covers(5, 5));
        assert!(d.contains(10) && !d.contains(40));
        assert_eq!((d.shift(5), d.shift(40), d.shift(70)), (5, 10, 30));
    }

    #[test]
    fn aranges_merge_adjacent_code() {
        let b = aranges(&[(0, vec![(0x100, 0x110), (0x110, 0x120), (0x200, 0x210)])]);
        // Header, two ranges, terminator.
        assert_eq!(b.len(), 16 + 2 * 16 + 16);
        assert_eq!(u64le(&b, 16), 0x100);
        assert_eq!(u64le(&b, 24), 0x20);
        assert_eq!(u64le(&b, 32), 0x200);
    }
}
