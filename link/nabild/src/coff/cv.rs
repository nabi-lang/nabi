//! `CodeView` of the objects into the PDB's parts: type records merged
//! across objects (one copy per contents in the type and id streams, the
//! objects' indices rewritten), and per object a module of its symbols
//! (relocated, scopes linked, `_ID` procedures turned into plain ones) and
//! C13 line information (file checksums naming `/names` strings).

use crate::bytes::{u16le, u32le};
use crate::coff::Link;
use crate::coff::object::CvIn;
use crate::coff::pdb::{Contrib, Module, Strings};
use crate::coff::write::OutSec;
use crate::symtab::AtomId;
use nabic_core::HashMap;

const SIGNATURE: u32 = 4;

const DEBUG_S_SYMBOLS: u32 = 0xf1;
const DEBUG_S_LINES: u32 = 0xf2;
const DEBUG_S_STRINGTABLE: u32 = 0xf3;
const DEBUG_S_FILECHKSMS: u32 = 0xf4;
const DEBUG_S_INLINEELINES: u32 = 0xf6;

const S_END: u16 = 0x0006;
const S_THUNK32: u16 = 0x1102;
const S_BLOCK32: u16 = 0x1103;
const S_WITH32: u16 = 0x1104;
const S_CONSTANT: u16 = 0x1107;
const S_UDT: u16 = 0x1108;
const S_LDATA32: u16 = 0x110c;
const S_GDATA32: u16 = 0x110d;
const S_LPROC32: u16 = 0x110f;
const S_GPROC32: u16 = 0x1110;
const S_LTHREAD32: u16 = 0x1112;
const S_GTHREAD32: u16 = 0x1113;
const S_PROCREF: u16 = 0x1125;
const S_LPROCREF: u16 = 0x1127;
const S_SEPCODE: u16 = 0x1132;
const S_LPROC32_ID: u16 = 0x1146;
const S_GPROC32_ID: u16 = 0x1147;
const S_INLINESITE: u16 = 0x114d;
const S_INLINESITE_END: u16 = 0x114e;
const S_PROC_ID_END: u16 = 0x114f;
const S_LPROC32_DPC: u16 = 0x1155;
const S_LPROC32_DPC_ID: u16 = 0x1156;
const S_INLINESITE2: u16 = 0x115d;

const LF_TYPESERVER2: u16 = 0x1515;
const LF_PRECOMP: u16 = 0x1509;

/// Merged type and id records (each with its length prefix, padded).
#[derive(Default)]
pub struct Types {
    pub tpi: Vec<Vec<u8>>,
    pub ipi: Vec<Vec<u8>>,
    tpi_at: HashMap<Vec<u8>, u32>,
    ipi_at: HashMap<Vec<u8>, u32>,
}

/// An id record kind (the IPI stream's): `LF_FUNC_ID` .. `LF_UDT_MOD_SRC_LINE`.
const fn is_id(kind: u16) -> bool {
    matches!(kind, 0x1601..=0x1607)
}

/// The size of the numeric leaf at `at` of `b`.
fn numeric_len(b: &[u8], at: usize) -> Option<usize> {
    let v = u16::from_le_bytes([*b.get(at)?, *b.get(at + 1)?]);
    if v < 0x8000 {
        return Some(2);
    }
    let n = match v {
        0x8000 => 1,
        0x8001 | 0x8002 | 0x801c => 2,
        0x8003..=0x8005 => 4,
        0x8006 | 0x8009 | 0x800a | 0x800c | 0x801a => 8,
        0x8007 => 10,
        0x800b => 6,
        0x8008 | 0x800d | 0x8017..=0x8019 => 16,
        0x800e => 20,
        0x800f => 32,
        0x8010 => 2 + usize::from(u16::from_le_bytes([*b.get(at + 2)?, *b.get(at + 3)?])),
        0x801b => b.get(at + 2..)?.iter().position(|&c| c == 0)? + 1,
        _ => return None,
    };
    Some(2 + n)
}

/// The length of the NUL-terminated name at `at`.
fn name_len(b: &[u8], at: usize) -> Option<usize> {
    Some(b.get(at..)?.iter().position(|&c| c == 0)? + 1)
}

/// Offsets of the type indices in field list `b` (its records' contents).
fn fieldlist_refs(b: &[u8]) -> Option<Vec<usize>> {
    let mut out = Vec::new();
    let mut at = 0;
    while at + 2 <= b.len() {
        if b[at] >= 0xf0 {
            at += usize::from(b[at] & 0xf);
            continue;
        }
        let kind = u16le(b, at);
        at += match kind {
            // LF_BCLASS, LF_BINTERFACE
            0x1400 | 0x151a => {
                out.push(at + 4);
                8 + numeric_len(b, at + 8)?
            }
            // LF_VBCLASS, LF_IVBCLASS
            0x1401 | 0x1402 => {
                out.extend([at + 4, at + 8]);
                let n = numeric_len(b, at + 12)?;
                12 + n + numeric_len(b, at + 12 + n)?
            }
            // LF_ENUMERATE
            0x1502 => {
                let n = numeric_len(b, at + 4)?;
                4 + n + name_len(b, at + 4 + n)?
            }
            // LF_MEMBER
            0x150d => {
                out.push(at + 4);
                let n = numeric_len(b, at + 8)?;
                8 + n + name_len(b, at + 8 + n)?
            }
            // LF_STMEMBER, LF_METHOD, LF_NESTTYPE
            0x150e..=0x1510 => {
                out.push(at + 4);
                8 + name_len(b, at + 8)?
            }
            // LF_ONEMETHOD: a virtual base offset when introducing.
            0x1511 => {
                out.push(at + 4);
                let mprop = (u16le(b, at + 2) >> 2) & 7;
                let v = if matches!(mprop, 4 | 6) { 4 } else { 0 };
                8 + v + name_len(b, at + 8 + v)?
            }
            // LF_VFUNCTAB, LF_INDEX
            0x1409 | 0x1404 => {
                out.push(at + 4);
                8
            }
            _ => return None,
        };
    }
    (at <= b.len()).then_some(out)
}

/// Offsets of the type and id indices in the contents `b` of a type
/// record of `kind`; `None`: a kind not known.
fn type_refs(kind: u16, b: &[u8]) -> Option<Vec<usize>> {
    let list = |w: usize| -> Option<Vec<usize>> {
        let n = if w == 4 {
            u32le(b, 0) as usize
        } else {
            usize::from(u16le(b, 0))
        };
        (w + 4 * n <= b.len()).then(|| (0..n).map(|i| w + 4 * i).collect())
    };
    let v = match kind {
        // LF_MODIFIER, LF_BITFIELD, LF_ALIAS, LF_STRING_ID,
        // LF_UDT_MOD_SRC_LINE
        0x1001 | 0x1205 | 0x150a | 0x1605 | 0x1607 => vec![0],
        // LF_POINTER: a pointer to member names its class.
        0x1002 => {
            let mode = (u32le(b, 4) >> 5) & 7;
            if matches!(mode, 2 | 3) {
                vec![0, 8]
            } else {
                vec![0]
            }
        }
        // LF_PROCEDURE
        0x1008 => vec![0, 8],
        // LF_MFUNCTION
        0x1009 => vec![0, 4, 8, 16],
        // LF_ARGLIST, LF_SUBSTR_LIST
        0x1201 | 0x1604 => list(4)?,
        // LF_BUILDINFO
        0x1603 => list(2)?,
        0x1203 => fieldlist_refs(b)?,
        // LF_METHODLIST
        0x1206 => {
            let mut out = Vec::new();
            let mut at = 0;
            while at + 8 <= b.len() {
                out.push(at + 4);
                let mprop = (u16le(b, at) >> 2) & 7;
                at += if matches!(mprop, 4 | 6) { 12 } else { 8 };
            }
            out
        }
        // LF_CLASS, LF_STRUCTURE, LF_INTERFACE
        0x1504 | 0x1505 | 0x1519 => vec![4, 8, 12],
        // LF_UNION
        0x1506 => vec![4],
        // LF_ENUM
        0x1507 => vec![4, 8],
        // LF_ARRAY, LF_VFTABLE, LF_FUNC_ID, LF_MFUNC_ID, LF_UDT_SRC_LINE
        0x1503 | 0x151d | 0x1601 | 0x1602 | 0x1606 => vec![0, 4],
        // LF_VTSHAPE, LF_LABEL
        0x000a | 0x000e => Vec::new(),
        _ => return None,
    };
    v.iter().all(|&o| o + 4 <= b.len()).then_some(v)
}

/// Offsets of the type and id indices in the contents `b` of symbol
/// record `kind`.
fn sym_refs(kind: u16, b: &[u8]) -> Vec<usize> {
    let v = match kind {
        S_GPROC32 | S_LPROC32 | S_GPROC32_ID | S_LPROC32_ID | S_LPROC32_DPC | S_LPROC32_DPC_ID => {
            vec![24]
        }
        // S_REGISTER, S_CONSTANT, S_UDT, data, thread data, S_LOCAL,
        // S_FILESTATIC, S_BUILDINFO
        0x1106 | S_CONSTANT | S_UDT | S_LDATA32 | S_GDATA32 | 0x111c | 0x111d | S_LTHREAD32
        | S_GTHREAD32 | 0x113e | 0x1153 | 0x114c => vec![0],
        // S_BPREL32, S_REGREL32
        0x110b | 0x1111 => vec![4],
        // S_CALLSITEINFO, S_HEAPALLOCSITE, inline sites
        0x1139 | 0x115e | S_INLINESITE | S_INLINESITE2 => vec![8],
        // S_CALLEES, S_CALLERS, S_INLINEES
        0x115a | 0x115b | 0x1168 => {
            let n = if b.len() >= 4 {
                u32le(b, 0) as usize
            } else {
                0
            };
            (0..n).map(|i| 4 + 4 * i).collect()
        }
        _ => Vec::new(),
    };
    v.into_iter().filter(|&o| o + 4 <= b.len()).collect()
}

/// A symbol record `kind` with contents `body`, padded to 4 bytes with
/// zeros.
fn record(kind: u16, body: &[u8]) -> Vec<u8> {
    let mut r = Vec::with_capacity(body.len() + 8);
    let len = (body.len() + 4).next_multiple_of(4) - 2;
    r.extend_from_slice(&(len as u16).to_le_bytes());
    r.extend_from_slice(&kind.to_le_bytes());
    r.extend_from_slice(body);
    r.resize(len + 2, 0);
    r
}

/// A type record: padded with `LF_PAD<n>` bytes (a field list reads zeros
/// as a member).
fn type_record(kind: u16, body: &[u8]) -> Vec<u8> {
    let mut r = record(kind, body);
    let n = r.len() - 4 - body.len();
    for (i, b) in r[4 + body.len()..].iter_mut().enumerate() {
        *b = 0xf0 | (n - i) as u8;
    }
    r
}

impl Types {
    /// Merge `.debug$T` contents `d`: the global index of each of its
    /// records (0 where unusable).
    fn merge(&mut self, d: &[u8]) -> Vec<u32> {
        let mut map = Vec::new();
        if d.len() < 4 || u32le(d, 0) != SIGNATURE {
            return map;
        }
        let mut at = 4;
        while at + 4 <= d.len() {
            let len = usize::from(u16le(d, at));
            let kind = u16le(d, at + 2);
            let end = at + 2 + len;
            if len < 2 || end > d.len() {
                break;
            }
            if kind == LF_TYPESERVER2 || kind == LF_PRECOMP {
                return Vec::new();
            }
            let mut body = d[at + 4..end].to_vec();
            // Trailing `LF_PAD` bytes go; `type_record` pads again.
            let mut pads = 0;
            while pads < 3 && body.len() > pads && body[body.len() - 1 - pads] == 0xf1 + pads as u8
            {
                pads += 1;
            }
            body.truncate(body.len() - pads);
            if let Some(refs) = type_refs(kind, &d[at + 4..end]) {
                let n = body.len();
                for o in refs.into_iter().filter(|o| o + 4 <= n) {
                    let ti = u32le(&body, o);
                    let g = if ti < 0x1000 {
                        ti
                    } else {
                        map.get((ti - 0x1000) as usize).copied().unwrap_or(0)
                    };
                    body[o..o + 4].copy_from_slice(&g.to_le_bytes());
                }
            }
            let rec = type_record(kind, &body);
            let (list, at_of) = if is_id(kind) {
                (&mut self.ipi, &mut self.ipi_at)
            } else {
                (&mut self.tpi, &mut self.tpi_at)
            };
            let g = *at_of.entry(rec.clone()).or_insert_with(|| {
                list.push(rec);
                0x1000 + list.len() as u32 - 1
            });
            map.push(g);
            at = end;
        }
        map
    }

    /// The function type of id record `id` (`LF_FUNC_ID` / `LF_MFUNC_ID`).
    fn func_type(&self, id: u32) -> u32 {
        id.checked_sub(0x1000)
            .and_then(|i| self.ipi.get(i as usize))
            .filter(|r| r.len() >= 12 && matches!(u16le(r, 2), 0x1601 | 0x1602))
            .map_or(0, |r| u32le(r, 8))
    }
}

/// Where a module's symbols are being written.
struct Syms<'a> {
    out: Vec<u8>,
    /// Open scopes: the offset of each opening record.
    open: Vec<usize>,
    /// Inside a dropped procedure: its depth.
    skip: Option<usize>,
    globals: &'a mut Vec<Vec<u8>>,
    procs: Vec<(u16, u32, String)>,
}

/// Opens a scope its `S_END` (or `_ID` / inline end) closes.
const fn opens(kind: u16) -> bool {
    matches!(
        kind,
        S_GPROC32
            | S_LPROC32
            | S_GPROC32_ID
            | S_LPROC32_ID
            | S_LPROC32_DPC
            | S_LPROC32_DPC_ID
            | S_THUNK32
            | S_BLOCK32
            | S_WITH32
            | S_SEPCODE
            | S_INLINESITE
            | S_INLINESITE2
    )
}

const fn closes(kind: u16) -> bool {
    matches!(kind, S_END | S_PROC_ID_END | S_INLINESITE_END)
}

/// The NUL-terminated name at `at` of `b`.
fn name_at(b: &[u8], at: usize) -> String {
    let s = b.get(at..).unwrap_or_default();
    let n = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    String::from_utf8_lossy(&s[..n]).into_owned()
}

impl Syms<'_> {
    /// Symbol record `kind` with contents `body`, whose relocations wrote
    /// no address if `dead`, at the module stream's offset.
    fn add(&mut self, mut kind: u16, mut body: Vec<u8>, dead: bool, types: &Types) {
        if let Some(depth) = self.skip {
            if opens(kind) {
                self.open.push(usize::MAX);
            } else if closes(kind) {
                self.open.pop();
                if self.open.len() == depth {
                    self.skip = None;
                }
            }
            return;
        }
        if opens(kind) && dead {
            self.skip = Some(self.open.len());
            self.open.push(usize::MAX);
            return;
        }
        // Data whose address is gone.
        if dead && !closes(kind) {
            return;
        }
        let top = self.open.is_empty();
        // `_ID` procedures name their function type instead.
        if matches!(kind, S_GPROC32_ID | S_LPROC32_ID | S_LPROC32_DPC_ID) && body.len() >= 28 {
            let ty = types.func_type(u32le(&body, 24));
            body[24..28].copy_from_slice(&ty.to_le_bytes());
            kind = match kind {
                S_GPROC32_ID => S_GPROC32,
                S_LPROC32_ID => S_LPROC32,
                _ => S_LPROC32_DPC,
            };
        }
        if kind == S_PROC_ID_END {
            kind = S_END;
        }
        // Global data, constants and types go to the globals only.
        let global_only = top && matches!(kind, S_GDATA32 | S_GTHREAD32 | S_CONSTANT | S_UDT);
        if top
            && matches!(
                kind,
                S_GDATA32 | S_GTHREAD32 | S_CONSTANT | S_UDT | S_LDATA32 | S_LTHREAD32
            )
        {
            self.globals.push(record(kind, &body));
        }
        if global_only {
            return;
        }
        // Offsets in the module's stream count its signature.
        let here = self.out.len() + 4;
        if opens(kind) {
            let parent = self.open.last().map_or(0, |&p| p as u32 + 4);
            if body.len() >= 8 {
                body[0..4].copy_from_slice(&parent.to_le_bytes());
            }
            if matches!(kind, S_GPROC32 | S_LPROC32 | S_LPROC32_DPC) && top && body.len() > 35 {
                let refk = if kind == S_GPROC32 {
                    S_PROCREF
                } else {
                    S_LPROCREF
                };
                self.procs.push((refk, here as u32, name_at(&body, 35)));
            }
            self.open.push(self.out.len());
        } else if closes(kind) {
            if let Some(o) = self.open.pop() {
                // The opener's `end` field.
                let at = o + 8;
                self.out[at..at + 4].copy_from_slice(&(here as u32).to_le_bytes());
            }
        }
        self.out.extend_from_slice(&record(kind, &body));
    }
}

/// A `.debug$S` section's bytes with its relocations applied, and the
/// offsets whose relocation found no address.
fn relocated(l: &Link, fi: usize, c: &CvIn, secs: &[OutSec]) -> (Vec<u8>, Vec<u32>) {
    let f = &l.files[fi];
    let s = &f.sects[c.sect];
    let mut b = f.data[s.off..s.off + s.size as usize].to_vec();
    let mut dead = Vec::new();
    for r in &c.relocs {
        if crate::coff::write::apply(l, &mut b, r, 0, secs).is_err() {
            dead.push(r.off);
        }
    }
    (b, dead)
}

/// The modules of every object with `CodeView`, the merged types, and the
/// global symbols (records).
pub fn collect(
    l: &Link,
    secs: &[OutSec],
    strings: &mut Strings,
) -> (Vec<Module>, Types, Vec<Vec<u8>>) {
    let mut types = Types::default();
    let mut globals = Vec::new();
    let mut modules = Vec::new();
    let live = |a: AtomId| a != crate::coff::DEAD && l.atoms[a as usize].live;
    for (fi, f) in l.files.iter().enumerate().skip(1) {
        // Types first: symbols name them.
        let mut map = Vec::new();
        for c in f.cv.iter().filter(|c| c.types) {
            let s = &f.sects[c.sect];
            map = types.merge(&f.data[s.off..s.off + s.size as usize]);
        }
        let remap = |ti: u32| -> u32 {
            if ti < 0x1000 {
                ti
            } else {
                map.get((ti - 0x1000) as usize).copied().unwrap_or(0)
            }
        };
        let mut syms = Syms {
            out: Vec::new(),
            open: Vec::new(),
            skip: None,
            globals: &mut globals,
            procs: Vec::new(),
        };
        let mut c13 = Vec::new();
        // The plain inlinee entries (id, file, line) of all sections.
        let mut inl: Vec<u8> = Vec::new();
        let mut checks: Option<Vec<u8>> = None;
        let mut strtab: Vec<u8> = Vec::new();
        for c in f.cv.iter().filter(|c| !c.types) {
            // A function's symbols whose code is gone.
            if c.parent.is_some_and(|p| !live(f.sects[p].atom)) {
                continue;
            }
            let (b, dead) = relocated(l, fi, c, secs);
            if b.len() < 4 || u32le(&b, 0) != SIGNATURE {
                continue;
            }
            let mut at = 4;
            while at + 8 <= b.len() {
                let kind = u32le(&b, at);
                let len = u32le(&b, at + 4) as usize;
                let start = at + 8;
                let end = start + len;
                if end > b.len() {
                    break;
                }
                let data = &b[start..end];
                let gone =
                    |lo: usize, hi: usize| dead.iter().any(|&o| (lo..hi).contains(&(o as usize)));
                match kind {
                    DEBUG_S_SYMBOLS => {
                        let mut p = 0;
                        while p + 4 <= data.len() {
                            let rlen = usize::from(u16le(data, p));
                            let rkind = u16le(data, p + 2);
                            if rlen < 2 || p + 2 + rlen > data.len() {
                                break;
                            }
                            let mut body = data[p + 4..p + 2 + rlen].to_vec();
                            for o in sym_refs(rkind, &body) {
                                let ti = remap(u32le(&body, o));
                                body[o..o + 4].copy_from_slice(&ti.to_le_bytes());
                            }
                            let d = gone(start + p, start + p + 2 + rlen);
                            syms.add(rkind, body, d, &types);
                            p += 2 + rlen;
                        }
                    }
                    DEBUG_S_LINES if !gone(start, start + 6) => {
                        sub(&mut c13, DEBUG_S_LINES, data);
                    }
                    DEBUG_S_INLINEELINES => {
                        let mut d = data.to_vec();
                        if d.len() >= 4 {
                            let extra = u32le(&d, 0) == 1;
                            let mut p = 4;
                            while p + 12 <= d.len() {
                                let id = remap(u32le(&d, p));
                                d[p..p + 4].copy_from_slice(&id.to_le_bytes());
                                let n = if extra { u32le(&d, p + 12) as usize } else { 0 };
                                let next = p + 12 + if extra { 4 + 4 * n } else { 0 };
                                // Plain entries go in the module's one
                                // subsection (dbghelp reads the first only).
                                if !extra && !inl.chunks(12).any(|e| e[..4] == d[p..p + 4]) {
                                    inl.extend_from_slice(&d[p..p + 12]);
                                }
                                p = next;
                            }
                            if extra {
                                sub(&mut c13, DEBUG_S_INLINEELINES, &d);
                            }
                        }
                    }
                    DEBUG_S_FILECHKSMS if checks.is_none() => checks = Some(data.to_vec()),
                    DEBUG_S_STRINGTABLE if strtab.is_empty() => strtab = data.to_vec(),
                    _ => {}
                }
                at = end.next_multiple_of(4);
            }
        }
        if !inl.is_empty() {
            let mut d = 0u32.to_le_bytes().to_vec();
            d.extend_from_slice(&inl);
            sub(&mut c13, DEBUG_S_INLINEELINES, &d);
        }
        // Checksum entries name `/names` strings; the module's files.
        let mut files = Vec::new();
        if let Some(mut ck) = checks {
            let mut p = 0;
            while p + 6 <= ck.len() {
                let name = name_at(&strtab, u32le(&ck, p) as usize);
                let off = strings.add(&name);
                ck[p..p + 4].copy_from_slice(&off.to_le_bytes());
                files.push(name);
                p = (p + 6 + usize::from(ck[p + 4])).next_multiple_of(4);
            }
            let mut head = Vec::new();
            sub(&mut head, DEBUG_S_FILECHKSMS, &ck);
            head.extend_from_slice(&c13);
            c13 = head;
        }
        let contribs = contribs(l, fi as u32, secs);
        if syms.out.is_empty() && c13.is_empty() && contribs.is_empty() {
            continue;
        }
        let procs = std::mem::take(&mut syms.procs);
        let (name, obj) = if f.archive.is_empty() {
            (f.name.clone(), f.name.clone())
        } else {
            let member = f
                .name
                .strip_prefix(&f.archive)
                .and_then(|m| m.strip_prefix('('))
                .and_then(|m| m.strip_suffix(')'))
                .unwrap_or(&f.name)
                .to_owned();
            (member, f.archive.clone())
        };
        modules.push(Module {
            name,
            obj,
            syms: syms.out,
            c13,
            files,
            contribs,
            procs,
        });
    }
    (modules, types, globals)
}

/// Subsection `kind` of `data`, padded to 4 bytes.
fn sub(out: &mut Vec<u8>, kind: u32, data: &[u8]) {
    out.extend_from_slice(&kind.to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(data);
    out.resize(out.len().next_multiple_of(4), 0);
}

/// The section contributions of file `fi`'s atoms in the image.
pub fn contribs(l: &Link, fi: u32, secs: &[OutSec]) -> Vec<Contrib> {
    let f = &l.files[fi as usize];
    let mut v = Vec::new();
    for s in &f.sects {
        let a = s.atom;
        if a == crate::coff::DEAD {
            continue;
        }
        let atom = &l.atoms[a as usize];
        if !atom.live || atom.out == u32::MAX || atom.file != fi {
            continue;
        }
        let sec = &secs[atom.out as usize];
        v.push(Contrib {
            sect: atom.out as u16 + 1,
            off: (atom.addr - sec.rva) as u32,
            size: atom.size as u32,
            chars: s.chars,
        });
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `.debug$T` of `recs` (kind, contents).
    fn debug_t(recs: &[(u16, Vec<u8>)]) -> Vec<u8> {
        let mut d = SIGNATURE.to_le_bytes().to_vec();
        for (k, b) in recs {
            d.extend_from_slice(&type_record(*k, b));
        }
        d
    }

    /// Two objects with the same argument list and procedure: one copy of
    /// each, the second object's indices mapped onto the first's; a field
    /// list padded with `LF_PAD` bytes, not zeros.
    #[test]
    fn types_merge_by_contents() {
        let args = |ti: u32| {
            let mut b = 1u32.to_le_bytes().to_vec();
            b.extend_from_slice(&ti.to_le_bytes());
            b
        };
        let procedure = |ret: u32, list: u32| {
            let mut b = ret.to_le_bytes().to_vec();
            b.extend_from_slice(&[0, 0, 1, 0]);
            b.extend_from_slice(&list.to_le_bytes());
            b
        };
        let mut t = Types::default();
        let a = t.merge(&debug_t(&[
            (0x1201, args(0x74)),
            (0x1008, procedure(0x74, 0x1000)),
        ]));
        // Another order: the list second.
        let mut member = vec![0x0d, 0x15, 3, 0];
        member.extend_from_slice(&0x74u32.to_le_bytes());
        member.extend_from_slice(&[0, 0, b'x', b'y', 0]);
        let b = t.merge(&debug_t(&[
            (0x1203, member),
            (0x1201, args(0x74)),
            (0x1008, procedure(0x74, 0x1001)),
        ]));
        assert_eq!(a, [0x1000, 0x1001]);
        assert_eq!(b, [0x1002, 0x1000, 0x1001]);
        assert_eq!(t.tpi.len(), 3);
        let fl = &t.tpi[2];
        assert_eq!(&fl[17..], &[0xf3, 0xf2, 0xf1]);
    }
}
