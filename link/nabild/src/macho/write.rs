//! The `MH_EXECUTE` image: output sections and segments, relocations
//! applied, stubs and GOT, dyld rebase / bind opcodes, the export trie,
//! symbol table with the debug map, and the code signature.

use crate::bytes::{put32, put64};
use crate::layout::{OutSection, Segment, assign, place_atoms};
use crate::macho::object::{
    InSect, Kind, ObjFile, S_ATTR_PURE_INSTRUCTIONS, S_ATTR_SOME_INSTRUCTIONS,
    S_THREAD_LOCAL_REGULAR, S_THREAD_LOCAL_VARIABLES, S_THREAD_LOCAL_ZEROFILL, S_ZEROFILL,
};
use crate::macho::{Link, Options, a64, sign, trie, unwind};
use crate::symtab::{Atom, AtomId, Def, SymId, Target};
use nabic_core::HashMap;
use nabic_core::leb::{sleb, uleb};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;

const BASE: u64 = 0x1_0000_0000;
const PAGE: u64 = 0x4000;
const HEADER_PAD: u64 = 32;

const MH_MAGIC_64: u32 = 0xfeed_facf;
const MH_EXECUTE: u32 = 2;
const MH_DYLIB: u32 = 6;
const MH_NO_REEXPORTED_DYLIBS: u32 = 0x10_0000;
const LC_ID_DYLIB: u32 = 0xd;
const S_MOD_INIT_FUNC_POINTERS: u32 = 0x9;
const MH_NOUNDEFS: u32 = 0x1;
const MH_DYLDLINK: u32 = 0x4;
const MH_TWOLEVEL: u32 = 0x80;
const MH_PIE: u32 = 0x20_0000;
const MH_HAS_TLV_DESCRIPTORS: u32 = 0x80_0000;

const LC_SEGMENT_64: u32 = 0x19;
const LC_SYMTAB: u32 = 0x2;
const LC_DYSYMTAB: u32 = 0xb;
const LC_LOAD_DYLIB: u32 = 0xc;
const LC_LOAD_DYLINKER: u32 = 0xe;
const LC_UUID: u32 = 0x1b;
const LC_CODE_SIGNATURE: u32 = 0x1d;
const LC_RPATH: u32 = 0x8000_001c;
const LC_DYLD_INFO_ONLY: u32 = 0x8000_0022;
const LC_FUNCTION_STARTS: u32 = 0x26;
const LC_DATA_IN_CODE: u32 = 0x29;
const LC_SOURCE_VERSION: u32 = 0x2a;
const LC_MAIN: u32 = 0x8000_0028;
const LC_BUILD_VERSION: u32 = 0x32;

const S_NON_LAZY_SYMBOL_POINTERS: u32 = 0x6;
const S_SYMBOL_STUBS: u32 = 0x8;
const SG_READ_ONLY: u32 = 0x10;
const INDIRECT_SYMBOL_LOCAL: u32 = 0x8000_0000;
const INDIRECT_SYMBOL_ABS: u32 = 0x4000_0000;

const N_SO: u8 = 0x64;
const N_OSO: u8 = 0x66;
const N_FUN: u8 = 0x24;
const N_STSYM: u8 = 0x26;
const N_GSYM: u8 = 0x20;
const N_SECT: u8 = 0xe;
const N_ABS: u8 = 0x2;
const N_EXT: u8 = 0x1;
const N_PEXT: u8 = 0x10;

const DYLD_PATH: &str = "/usr/lib/dyld";

/// Tentative definitions left after resolution become zero-fill atoms of
/// a file of their own.
pub fn commons(l: &mut Link) {
    let fi = l.files.len() as u32;
    let mut sect = InSect {
        seg: "__DATA".to_owned(),
        name: "__common".to_owned(),
        addr: 0,
        size: 0,
        offset: 0,
        align: 0,
        reloff: 0,
        nreloc: 0,
        flags: S_ZEROFILL,
        kind: Kind::Atoms,
        atoms: Vec::new(),
        atom_offs: Vec::new(),
    };
    for id in 0..l.syms.syms.len() as SymId {
        if let Def::Common { size, align } = l.syms.get(id).def {
            let atom = l.atoms.len() as AtomId;
            l.atoms.push(Atom::new(fi, 0, sect.size, size, align));
            sect.atoms.push(atom);
            sect.atom_offs.push(sect.size);
            sect.size += size;
            l.syms.get_mut(id).def = Def::Defined { atom, off: 0 };
        }
    }
    l.files.push(ObjFile {
        name: "<common symbols>".to_owned(),
        oso: String::new(),
        data: Vec::new(),
        sects: vec![sect],
        locals: Vec::new(),
        source: None,
        debug: false,
        linker_options: Vec::new(),
    });
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Synth {
    None,
    Stubs,
    Got,
    UnwindInfo,
    EhFrame,
    /// The function table of panic backtraces (`trace`).
    Trace,
    /// A dylib's `-init`: its pointer.
    Init,
}

/// Input sections that ld64 moves to `__DATA_CONST` (read-only after
/// fixups).
fn out_name<'a>(seg: &'a str, name: &'a str) -> (&'a str, &'a str) {
    match (seg, name) {
        (
            "__DATA",
            "__const" | "__cfstring" | "__mod_init_func" | "__mod_term_func" | "__nl_symbol_ptr"
            | "__objc_classlist" | "__objc_nlclslist" | "__objc_catlist" | "__objc_nlcatlist"
            | "__objc_protolist" | "__objc_imageinfo",
        ) => ("__DATA_CONST", name),
        _ => (seg, name),
    }
}

fn seg_rank(seg: &str) -> u32 {
    match seg {
        "__TEXT" => 0,
        "__DATA_CONST" => 1,
        "__DATA" => 2,
        _ => 3,
    }
}

fn sect_rank(seg: &str, name: &str, flags: u32) -> u32 {
    let ty = flags & 0xff;
    let code = flags & (S_ATTR_PURE_INSTRUCTIONS | S_ATTR_SOME_INSTRUCTIONS) != 0;
    match seg {
        "__TEXT" => match name {
            "__text" => 0,
            _ if code => 1,
            "__stubs" => 2,
            "__unwind_info" => 8,
            "__eh_frame" => 9,
            "__nabi_symbols" => 10,
            _ => 3,
        },
        "__DATA_CONST" if name == "__got" => 0,
        _ => match ty {
            S_THREAD_LOCAL_VARIABLES => 2,
            S_THREAD_LOCAL_REGULAR => 3,
            S_THREAD_LOCAL_ZEROFILL => 4,
            S_ZEROFILL | 0xc => 5,
            _ => 1,
        },
    }
}

const fn zerofill_type(flags: u32) -> bool {
    matches!(flags & 0xff, S_ZEROFILL | 0xc | S_THREAD_LOCAL_ZEROFILL)
}

/// Pointer fixups dyld applies.
#[derive(Default)]
struct Fixups {
    rebases: Vec<u64>,
    /// `(address, symbol, addend)`.
    binds: Vec<(u64, SymId, i64)>,
}

struct Image<'a> {
    l: &'a Link,
    sects: Vec<OutSection>,
    segs: Vec<Segment>,
    stubs: Vec<SymId>,
    stub_of: HashMap<SymId, usize>,
    got: Vec<Target>,
    got_of: HashMap<Target, usize>,
    stubs_sect: Option<usize>,
    got_sect: Option<usize>,
    /// Start and end of the thread-local template.
    tlv: Option<(u64, u64)>,
    /// The image's address: an executable's above `__PAGEZERO`, a
    /// dylib's 0.
    base: u64,
}

impl Image<'_> {
    fn sym_addr(&self, id: SymId) -> Option<u64> {
        match self.l.syms.get(id).def {
            Def::Defined { atom, off } => Some(self.l.atoms[atom as usize].addr + off),
            Def::Absolute(v) => Some(v),
            Def::Synthetic => Some(self.base),
            _ => None,
        }
    }

    fn addr(&self, t: Target) -> Option<u64> {
        match t {
            Target::Atom(a) => Some(self.l.atoms[a as usize].addr),
            Target::Sym(id) => self.sym_addr(id),
        }
    }

    fn dylib_sym(&self, t: Target) -> Option<SymId> {
        match t {
            Target::Sym(id) if matches!(self.l.syms.get(id).def, Def::Dylib { .. }) => Some(id),
            _ => None,
        }
    }

    fn got_addr(&self, t: Target) -> Option<u64> {
        let k = *self.got_of.get(&t)?;
        Some(self.sects[self.got_sect?].addr + 8 * k as u64)
    }

    fn stub_addr(&self, id: SymId) -> Option<u64> {
        let k = *self.stub_of.get(&id)?;
        Some(self.sects[self.stubs_sect?].addr + a64::STUB_SIZE * k as u64)
    }

    fn in_tlv(&self, a: u64) -> bool {
        self.tlv.is_some_and(|(s, e)| a >= s && a <= e)
    }

    fn name(&self, t: Target) -> String {
        match t {
            Target::Sym(id) => self.l.syms.get(id).name.clone(),
            Target::Atom(a) => {
                let a = &self.l.atoms[a as usize];
                let f = &self.l.files[a.file as usize];
                let s = &f.sects[a.sect as usize];
                format!("{},{}+{:#x} of {}", s.seg, s.name, a.off, f.name)
            }
        }
    }
}

fn patch32(
    buf: &mut [u8],
    at: usize,
    f: impl FnOnce(u32) -> Result<u32, String>,
) -> Result<(), String> {
    let v = u32::from_le_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]]);
    buf[at..at + 4].copy_from_slice(&f(v)?.to_le_bytes());
    Ok(())
}

/// Apply atom `ai`'s relocations in `buf` (the atom's bytes at `at`).
fn relocate(
    img: &Image<'_>,
    ai: AtomId,
    buf: &mut [u8],
    at: usize,
    fx: &mut Fixups,
) -> Result<(), String> {
    let a = &img.l.atoms[ai as usize];
    let file = &img.l.files[a.file as usize];
    let insect = &file.sects[a.sect as usize];
    let out = &img.sects[a.out as usize];
    let in_tlv_vars = insect.flags & 0xff == S_THREAD_LOCAL_VARIABLES;
    for r in &a.relocs {
        let p = a.addr + u64::from(r.off);
        let o = at + r.off as usize;
        let ctx = |e: String| format!("{}: {e}", file.name);
        let dylib = img.dylib_sym(r.target);
        let target = || -> Result<u64, String> {
            img.addr(r.target)
                .map(|v| (v as i64 + r.addend) as u64)
                .ok_or_else(|| format!("{}: {} has no address", file.name, img.name(r.target)))
        };
        match r.kind {
            a64::BRANCH26 => {
                let t = match dylib {
                    Some(id) => img.stub_addr(id).expect("stub") as i64 + r.addend,
                    None => target()? as i64,
                };
                patch32(buf, o, |i| a64::branch26(i, p, t as u64)).map_err(ctx)?;
            }
            a64::PAGE21 | a64::PAGEOFF12 => {
                if dylib.is_some() {
                    return Err(format!(
                        "{}: direct reference to {}, which is in a dylib (it needs a GOT access)",
                        file.name,
                        img.name(r.target)
                    ));
                }
                let t = target()?;
                if r.kind == a64::PAGE21 {
                    patch32(buf, o, |i| a64::page21(i, p, t)).map_err(ctx)?;
                } else {
                    patch32(buf, o, |i| a64::pageoff12(i, t)).map_err(ctx)?;
                }
            }
            a64::GOT_LOAD_PAGE21 | a64::GOT_LOAD_PAGEOFF12 => {
                let slot = img.got_addr(r.target);
                match slot {
                    Some(g) => {
                        let g = (g as i64 + r.addend) as u64;
                        if r.kind == a64::GOT_LOAD_PAGE21 {
                            patch32(buf, o, |i| a64::page21(i, p, g)).map_err(ctx)?;
                        } else {
                            patch32(buf, o, |i| a64::pageoff12(i, g)).map_err(ctx)?;
                        }
                    }
                    // Defined here: the address itself, no slot.
                    None => {
                        let t = target()?;
                        if r.kind == a64::GOT_LOAD_PAGE21 {
                            patch32(buf, o, |i| a64::page21(i, p, t)).map_err(ctx)?;
                        } else {
                            patch32(buf, o, |i| a64::pageoff12(a64::ldr_to_add(i)?, t))
                                .map_err(ctx)?;
                        }
                    }
                }
            }
            a64::TLVP_LOAD_PAGE21 | a64::TLVP_LOAD_PAGEOFF12 => {
                if dylib.is_some() {
                    return Err(format!(
                        "{}: thread-local {} from a dylib is not supported",
                        file.name,
                        img.name(r.target)
                    ));
                }
                let t = target()?;
                if r.kind == a64::TLVP_LOAD_PAGE21 {
                    patch32(buf, o, |i| a64::page21(i, p, t)).map_err(ctx)?;
                } else {
                    patch32(buf, o, |i| a64::pageoff12(a64::ldr_to_add(i)?, t)).map_err(ctx)?;
                }
            }
            a64::POINTER_TO_GOT => {
                let g = img.got_addr(r.target).ok_or_else(|| {
                    format!("{}: no GOT slot for {}", file.name, img.name(r.target))
                })?;
                if r.len == 2 && r.pcrel {
                    let d = g as i64 - p as i64 + r.addend;
                    let d =
                        i32::try_from(d).map_err(|_| ctx("GOT offset out of range".to_owned()))?;
                    buf[o..o + 4].copy_from_slice(&d.to_le_bytes());
                } else if r.len == 3 && !r.pcrel {
                    buf[o..o + 8].copy_from_slice(&g.to_le_bytes());
                    fx.rebases.push(p);
                } else {
                    return Err(ctx("POINTER_TO_GOT of an unsupported width".to_owned()));
                }
            }
            a64::UNSIGNED => {
                if let Some(s) = r.sub {
                    let t = target()? as i64;
                    let s = img
                        .addr(s)
                        .ok_or_else(|| format!("{}: {} has no address", file.name, img.name(s)))?
                        as i64;
                    let v = t - s;
                    match r.len {
                        3 => buf[o..o + 8].copy_from_slice(&v.to_le_bytes()),
                        2 => buf[o..o + 4].copy_from_slice(&(v as i32).to_le_bytes()),
                        _ => return Err(ctx("narrow SUBTRACTOR".to_owned())),
                    }
                    continue;
                }
                if r.len != 3 {
                    let abs = matches!(r.target, Target::Sym(id) if matches!(img.l.syms.get(id).def, Def::Absolute(_)));
                    if !abs {
                        return Err(format!(
                            "{}: 32-bit absolute address of {} in a position-independent executable",
                            file.name,
                            img.name(r.target)
                        ));
                    }
                    let t = target()?;
                    buf[o..o + 4].copy_from_slice(&(t as u32).to_le_bytes());
                    continue;
                }
                let seg_ro = img.segs[out.seg].name == "__TEXT";
                if let Some(id) = dylib {
                    if seg_ro {
                        return Err(format!(
                            "{}: pointer to {} in read-only {},{}",
                            file.name,
                            img.name(r.target),
                            insect.seg,
                            insect.name
                        ));
                    }
                    buf[o..o + 8].copy_from_slice(&r.addend.to_le_bytes());
                    fx.binds.push((p, id, r.addend));
                    continue;
                }
                let t = target()?;
                let absolute = matches!(r.target, Target::Sym(id) if matches!(img.l.syms.get(id).def, Def::Absolute(_)));
                if in_tlv_vars && img.in_tlv(t) {
                    // A descriptor's offset into the thread-local template.
                    let base = img.tlv.expect("tlv").0;
                    buf[o..o + 8].copy_from_slice(&(t - base).to_le_bytes());
                } else if absolute {
                    buf[o..o + 8].copy_from_slice(&t.to_le_bytes());
                } else {
                    if seg_ro {
                        return Err(format!(
                            "{}: absolute pointer to {} in read-only {},{} (text relocation)",
                            file.name,
                            img.name(r.target),
                            insect.seg,
                            insect.name
                        ));
                    }
                    buf[o..o + 8].copy_from_slice(&t.to_le_bytes());
                    fx.rebases.push(p);
                }
            }
            k => {
                return Err(format!(
                    "{}: relocation type {k} is not supported",
                    file.name
                ));
            }
        }
    }
    Ok(())
}

fn pad8(b: &mut Vec<u8>) {
    while b.len() % 8 != 0 {
        b.push(0);
    }
}

fn name16(b: &mut Vec<u8>, s: &str) {
    let mut n = [0u8; 16];
    let k = s.len().min(16);
    n[..k].copy_from_slice(&s.as_bytes()[..k]);
    b.extend_from_slice(&n);
}

/// `(segment index in load commands, offset)` of an address.
/// The segment index (`segs` from `first`: 1 past an executable's
/// `__PAGEZERO`) and offset of `addr`.
fn seg_off(segs: &[Segment], first: usize, addr: u64) -> Result<(u8, u64), String> {
    for (k, s) in segs.iter().enumerate() {
        if addr >= s.addr && addr < s.addr + s.vmsize {
            return Ok(((k + first) as u8, addr - s.addr));
        }
    }
    Err(format!("fixup at {addr:#x} outside every segment"))
}

fn rebase_opcodes(segs: &[Segment], first: usize, mut addrs: Vec<u64>) -> Result<Vec<u8>, String> {
    addrs.sort_unstable();
    addrs.dedup();
    let mut b = Vec::new();
    if addrs.is_empty() {
        return Ok(b);
    }
    // REBASE_OPCODE_SET_TYPE_IMM | REBASE_TYPE_POINTER
    b.push(0x11);
    let mut i = 0;
    while i < addrs.len() {
        let (seg, off) = seg_off(segs, first, addrs[i])?;
        // A run of consecutive pointers.
        let mut n = 1;
        while i + n < addrs.len() && addrs[i + n] == addrs[i] + 8 * n as u64 {
            n += 1;
        }
        b.push(0x20 | seg);
        uleb(&mut b, off);
        if n < 16 {
            b.push(0x50 | n as u8);
        } else {
            b.push(0x60);
            uleb(&mut b, n as u64);
        }
        i += n;
    }
    b.push(0);
    pad8(&mut b);
    Ok(b)
}

fn bind_opcodes(
    segs: &[Segment],
    first: usize,
    l: &Link,
    mut binds: Vec<(u64, SymId, i64)>,
    ordinal: &dyn Fn(SymId) -> u32,
) -> Result<Vec<u8>, String> {
    binds.sort_by_key(|&(a, id, _)| (id, a));
    let mut b = Vec::new();
    if binds.is_empty() {
        return Ok(b);
    }
    // BIND_OPCODE_SET_TYPE_IMM | BIND_TYPE_POINTER
    b.push(0x51);
    let (mut cur_ord, mut cur_sym, mut cur_add) = (u32::MAX, u32::MAX, 0i64);
    for (addr, id, addend) in binds {
        let ord = ordinal(id);
        if ord != cur_ord {
            if ord < 16 {
                b.push(0x10 | ord as u8);
            } else {
                b.push(0x20);
                uleb(&mut b, u64::from(ord));
            }
            cur_ord = ord;
        }
        if id != cur_sym {
            let s = l.syms.get(id);
            // BIND_SYMBOL_FLAGS_WEAK_IMPORT
            b.push(0x40 | u8::from(s.weak_ref));
            b.extend_from_slice(s.name.as_bytes());
            b.push(0);
            cur_sym = id;
        }
        if addend != cur_add {
            b.push(0x60);
            sleb(&mut b, addend);
            cur_add = addend;
        }
        let (seg, off) = seg_off(segs, first, addr)?;
        b.push(0x70 | seg);
        uleb(&mut b, off);
        b.push(0x90);
    }
    b.push(0);
    pad8(&mut b);
    Ok(b)
}

struct Nlist {
    strx: u32,
    ty: u8,
    sect: u8,
    desc: u16,
    value: u64,
}

struct StrTab {
    b: Vec<u8>,
    seen: HashMap<String, u32>,
}

impl StrTab {
    fn add(&mut self, s: &str) -> u32 {
        if s.is_empty() {
            return 1;
        }
        if let Some(&x) = self.seen.get(s) {
            return x;
        }
        let x = self.b.len() as u32;
        self.b.extend_from_slice(s.as_bytes());
        self.b.push(0);
        self.seen.insert(s.to_owned(), x);
        x
    }
}

/// Build and write the image.
#[allow(clippy::too_many_lines)]
/// The backtrace table's functions and the runtime's anchor atom, when
/// the runtime is linked in.
fn trace_plan(l: &Link) -> Option<(crate::trace::Plan, (AtomId, u64))> {
    let anchor = match l
        .syms
        .get(l.syms.find(&format!("_{}", crate::trace::ANCHOR))?)
        .def
    {
        Def::Defined { atom, off } if l.atoms[atom as usize].live => (atom, off),
        _ => return None,
    };
    let kept: HashMap<AtomId, AtomId> = l.folded.iter().copied().collect();
    let canon = |a: AtomId| {
        let c = kept.get(&a).copied().unwrap_or(a);
        let x = &l.atoms[c as usize];
        let code = l.files[x.file as usize].sects[x.sect as usize].code();
        (x.live && code).then_some((c, x.size))
    };
    let locals = l
        .files
        .iter()
        .flat_map(|f| f.locals.iter().map(|(n, a, off)| (n.as_str(), *a, *off)));
    let p = crate::trace::Plan::new(crate::trace::globals(&l.syms).chain(locals), "_", canon);
    (!p.is_empty()).then_some((p, anchor))
}

pub fn write(l: &mut Link, o: &Options) -> Result<(), String> {
    // Output sections of the live atoms, in input order.
    let mut sects: Vec<OutSection> = Vec::new();
    let mut synth: Vec<Synth> = Vec::new();
    let mut seg_names: Vec<String> = Vec::new();
    let mut by_key: HashMap<(String, String), usize> = HashMap::default();
    let mut code_atoms: Vec<AtomId> = Vec::new();
    let add_sect = |sects: &mut Vec<OutSection>,
                    synth: &mut Vec<Synth>,
                    seg_names: &mut Vec<String>,
                    seg: &str,
                    name: &str,
                    flags: u32,
                    s: Synth|
     -> usize {
        let si = seg_names.iter().position(|n| n == seg).unwrap_or_else(|| {
            seg_names.push(seg.to_owned());
            seg_names.len() - 1
        });
        sects.push(OutSection {
            seg: si,
            name: name.to_owned(),
            flags,
            zerofill: zerofill_type(flags),
            ..OutSection::default()
        });
        synth.push(s);
        sects.len() - 1
    };
    for ai in 0..l.atoms.len() {
        let a = &l.atoms[ai];
        if !a.live {
            continue;
        }
        let f = &l.files[a.file as usize];
        let s = &f.sects[a.sect as usize];
        let (seg, name) = out_name(&s.seg, &s.name);
        let key = (seg.to_owned(), name.to_owned());
        let k = if let Some(&k) = by_key.get(&key) {
            k
        } else {
            let k = add_sect(
                &mut sects,
                &mut synth,
                &mut seg_names,
                seg,
                name,
                s.flags,
                Synth::None,
            );
            by_key.insert(key, k);
            k
        };
        if seg == "__TEXT" && s.code() {
            code_atoms.push(ai as AtomId);
        }
        // Each object using Objective-C has one; the image keeps the first.
        if name == "__objc_imageinfo" && !sects[k].atoms.is_empty() {
            l.atoms[ai].live = false;
            continue;
        }
        sects[k].atoms.push(ai as AtomId);
        sects[k].flags |= s.flags & 0xffff_ff00;
    }

    // Stubs and GOT slots.
    let mut stubs: Vec<SymId> = Vec::new();
    let mut stub_of: HashMap<SymId, usize> = HashMap::default();
    let mut got: Vec<Target> = Vec::new();
    let mut got_of: HashMap<Target, usize> = HashMap::default();
    let want_got = |t: Target, got: &mut Vec<Target>, got_of: &mut HashMap<Target, usize>| {
        if let std::collections::hash_map::Entry::Vacant(e) = got_of.entry(t) {
            e.insert(got.len());
            got.push(t);
        }
    };
    let is_dylib =
        |t: Target| matches!(t, Target::Sym(id) if matches!(l.syms.get(id).def, Def::Dylib { .. }));
    let is_abs =
        |t: Target| matches!(t, Target::Sym(id) if matches!(l.syms.get(id).def, Def::Absolute(_)));
    for a in l.atoms.iter().filter(|a| a.live) {
        for r in &a.relocs {
            match r.kind {
                a64::BRANCH26 if is_dylib(r.target) => {
                    if let Target::Sym(id) = r.target {
                        if let std::collections::hash_map::Entry::Vacant(e) = stub_of.entry(id) {
                            e.insert(stubs.len());
                            stubs.push(id);
                        }
                    }
                    want_got(r.target, &mut got, &mut got_of);
                }
                a64::GOT_LOAD_PAGE21 | a64::GOT_LOAD_PAGEOFF12
                    if is_dylib(r.target) || is_abs(r.target) =>
                {
                    want_got(r.target, &mut got, &mut got_of);
                }
                a64::POINTER_TO_GOT => want_got(r.target, &mut got, &mut got_of),
                _ => {}
            }
        }
    }
    let plan = unwind::plan(l, &code_atoms, a64::UNWIND_DWARF)?;
    for t in plan.got_targets(l) {
        want_got(t, &mut got, &mut got_of);
    }

    // Synthetic sections.
    let text_code = S_ATTR_PURE_INSTRUCTIONS | S_ATTR_SOME_INSTRUCTIONS;
    let stubs_sect = (!stubs.is_empty()).then(|| {
        let k = add_sect(
            &mut sects,
            &mut synth,
            &mut seg_names,
            "__TEXT",
            "__stubs",
            S_SYMBOL_STUBS | text_code,
            Synth::Stubs,
        );
        sects[k].size = a64::STUB_SIZE * stubs.len() as u64;
        sects[k].align = 2;
        k
    });
    let got_sect = (!got.is_empty()).then(|| {
        let k = add_sect(
            &mut sects,
            &mut synth,
            &mut seg_names,
            "__DATA_CONST",
            "__got",
            S_NON_LAZY_SYMBOL_POINTERS,
            Synth::Got,
        );
        sects[k].size = 8 * got.len() as u64;
        sects[k].align = 3;
        k
    });
    if plan.info_size > 28 + 12 {
        let k = add_sect(
            &mut sects,
            &mut synth,
            &mut seg_names,
            "__TEXT",
            "__unwind_info",
            0,
            Synth::UnwindInfo,
        );
        sects[k].size = plan.info_size;
        sects[k].align = 2;
    }
    if plan.eh_size > 0 {
        let k = add_sect(
            &mut sects,
            &mut synth,
            &mut seg_names,
            "__TEXT",
            "__eh_frame",
            0x6800_000b,
            Synth::EhFrame,
        );
        sects[k].size = plan.eh_size;
        sects[k].align = 3;
    }
    if o.dylib && o.init.is_some() {
        let k = add_sect(
            &mut sects,
            &mut synth,
            &mut seg_names,
            "__DATA_CONST",
            "__mod_init_func",
            S_MOD_INIT_FUNC_POINTERS,
            Synth::Init,
        );
        sects[k].size = 8;
        sects[k].align = 3;
    }
    let trace = trace_plan(l);
    if let Some((p, _)) = &trace {
        let k = add_sect(
            &mut sects,
            &mut synth,
            &mut seg_names,
            "__TEXT",
            "__nabi_symbols",
            0,
            Synth::Trace,
        );
        sects[k].size = p.size();
        sects[k].align = 2;
    }
    if !seg_names.iter().any(|s| s == "__TEXT") {
        seg_names.push("__TEXT".to_owned());
    }

    // Segment and section order; sections renumbered.
    let mut seg_order: Vec<usize> = (0..seg_names.len()).collect();
    seg_order.sort_by_key(|&k| (seg_rank(&seg_names[k]), k));
    let mut order: Vec<usize> = (0..sects.len()).collect();
    let seg_pos = |k: usize| seg_order.iter().position(|&x| x == k).unwrap();
    order.sort_by_key(|&k| {
        let s = &sects[k];
        (
            seg_pos(s.seg),
            sect_rank(&seg_names[s.seg], &s.name, s.flags),
            k,
        )
    });
    let mut renum = vec![0usize; sects.len()];
    for (new, &old) in order.iter().enumerate() {
        renum[old] = new;
    }
    let mut sects: Vec<OutSection> = {
        let mut v: Vec<Option<OutSection>> = sects.into_iter().map(Some).collect();
        order.iter().map(|&k| v[k].take().expect("once")).collect()
    };
    let synth: Vec<Synth> = order.iter().map(|&k| synth[k]).collect();
    let stubs_sect = stubs_sect.map(|k| renum[k]);
    let got_sect = got_sect.map(|k| renum[k]);
    let mut segs: Vec<Segment> = seg_order
        .iter()
        .map(|&k| {
            let name = seg_names[k].clone();
            let prot = if name == "__TEXT" { 5 } else { 3 };
            Segment {
                name,
                prot,
                ..Segment::default()
            }
        })
        .collect();
    for (i, s) in sects.iter_mut().enumerate() {
        s.seg = seg_pos(s.seg);
        segs[s.seg].sects.push(i);
    }
    for (i, s) in sects.iter().enumerate() {
        for &a in &s.atoms {
            l.atoms[a as usize].out = i as u32;
        }
    }
    let section_ordinals = sects.len();
    if section_ordinals > 255 {
        return Err("more than 255 output sections".to_owned());
    }

    // Load commands, to know where sections start.
    let ident = o
        .out
        .file_name()
        .map_or_else(|| "a.out".to_owned(), |n| n.to_string_lossy().into_owned());
    let dylib_cmd = |name: &str| (24 + name.len() + 1).next_multiple_of(8);
    let rpath_cmd = |p: &str| (12 + p.len() + 1).next_multiple_of(8);
    let install_name = o
        .install_name
        .clone()
        .unwrap_or_else(|| format!("@rpath/{ident}"));
    // An executable: `__PAGEZERO`, `LC_LOAD_DYLINKER`, `LC_MAIN`; a
    // dylib: `LC_ID_DYLIB`.
    let own = if o.dylib {
        dylib_cmd(&install_name)
    } else {
        72 + (12 + DYLD_PATH.len() + 1).next_multiple_of(8) + 24
    };
    let mut sizeofcmds = own
        + segs.iter().map(|s| 72 + 80 * s.sects.len()).sum::<usize>()
        + 72 // __LINKEDIT
        + 48 + 24 + 80
        + 24 + 32 + 16
        + l.dylibs.iter().map(|d| dylib_cmd(&d.install_name)).sum::<usize>()
        + o.rpaths.iter().map(|p| rpath_cmd(p)).sum::<usize>()
        + 16 + 16 + 16;
    let ncmds =
        segs.len() + 1 + 3 + 3 + if o.dylib { 1 } else { 3 } + l.dylibs.len() + o.rpaths.len() + 3;
    let base = if o.dylib { 0 } else { BASE };
    sizeofcmds = sizeofcmds.next_multiple_of(8);
    let header = 32 + sizeofcmds as u64 + HEADER_PAD;

    for s in &mut sects {
        place_atoms(s, &mut l.atoms);
    }
    assign(&mut segs, &mut sects, &mut l.atoms, base, header, PAGE);
    // A folded atom is at its copy.
    for &(dup, keep) in &l.folded {
        let k = &l.atoms[keep as usize];
        let (addr, out) = (k.addr, k.out);
        let d = &mut l.atoms[dup as usize];
        d.addr = addr;
        d.out = out;
    }

    let tlv = {
        let t: Vec<&OutSection> = sects
            .iter()
            .filter(|s| {
                matches!(
                    s.flags & 0xff,
                    S_THREAD_LOCAL_REGULAR | S_THREAD_LOCAL_ZEROFILL
                )
            })
            .collect();
        match (t.first(), t.last()) {
            (Some(a), Some(b)) => Some((a.addr, b.addr + b.size)),
            _ => None,
        }
    };
    let img = Image {
        l,
        sects,
        segs,
        stubs,
        stub_of,
        got,
        got_of,
        stubs_sect,
        got_sect,
        tlv,
        base,
    };
    let linkedit_off = img.segs.last().map_or(0, |s| s.offset + s.filesize);
    let linkedit_addr = img.segs.last().map_or(base, |s| s.addr + s.vmsize);
    let mut buf = vec![0u8; linkedit_off as usize];
    let mut fx = Fixups::default();

    // Atom contents and relocations.
    for s in &img.sects {
        if s.zerofill {
            continue;
        }
        for &ai in &s.atoms {
            let a = &img.l.atoms[ai as usize];
            let f = &img.l.files[a.file as usize];
            let ins = &f.sects[a.sect as usize];
            let at = (s.offset + (a.addr - s.addr)) as usize;
            if !ins.zerofill() && a.size > 0 {
                let src = ins.offset as usize + a.off as usize;
                let data = f
                    .data
                    .get(src..src + a.size as usize)
                    .ok_or_else(|| format!("{}: {},{} out of bounds", f.name, ins.seg, ins.name))?;
                buf[at..at + a.size as usize].copy_from_slice(data);
            }
            relocate(&img, ai, &mut buf, at, &mut fx)?;
        }
    }

    // Synthetic sections' contents.
    let got_fn = |t: Target| img.got_addr(t);
    for (k, s) in img.sects.iter().enumerate() {
        let at = s.offset as usize;
        match synth.get(k).copied().unwrap_or(Synth::None) {
            Synth::None => {}
            Synth::Stubs => {
                for (i, &id) in img.stubs.iter().enumerate() {
                    let p = s.addr + a64::STUB_SIZE * i as u64;
                    let slot = img.got_addr(Target::Sym(id)).expect("stub slot");
                    let st = a64::stub(p, slot)?;
                    let o = at + 12 * i;
                    buf[o..o + 12].copy_from_slice(&st);
                }
            }
            Synth::Got => {
                for (i, &t) in img.got.iter().enumerate() {
                    let p = s.addr + 8 * i as u64;
                    let o = at + 8 * i;
                    if let Some(id) = img.dylib_sym(t) {
                        fx.binds.push((p, id, 0));
                    } else {
                        let v = img
                            .addr(t)
                            .ok_or_else(|| format!("GOT slot of undefined {}", img.name(t)))?;
                        buf[o..o + 8].copy_from_slice(&v.to_le_bytes());
                        if !is_abs(t) {
                            fx.rebases.push(p);
                        }
                    }
                }
            }
            Synth::UnwindInfo => {
                let b = unwind::write_unwind_info(&plan, img.l, base, &got_fn)?;
                buf[at..at + b.len()].copy_from_slice(&b);
            }
            Synth::EhFrame => {
                let (b, rebases) = unwind::write_eh_frame(&plan, img.l, s.addr, &got_fn)?;
                buf[at..at + b.len()].copy_from_slice(&b);
                fx.rebases.extend(rebases);
            }
            Synth::Init => {
                let f = o
                    .init
                    .as_ref()
                    .and_then(|n| img.l.syms.find(n))
                    .and_then(|id| img.sym_addr(id))
                    .ok_or("the -init symbol is not defined")?;
                buf[at..at + 8].copy_from_slice(&f.to_le_bytes());
                fx.rebases.push(s.addr);
            }
            Synth::Trace => {
                let Some((p, (atom, off))) = &trace else {
                    continue;
                };
                let atoms = &img.l.atoms;
                let t = p.table(s.addr, false, |a| {
                    (atoms[a as usize].addr, atoms[a as usize].size)
                });
                buf[at..at + t.len()].copy_from_slice(&t);
                let anchor = atoms[*atom as usize].addr + off;
                if let Some(d) = img
                    .sects
                    .iter()
                    .find(|d| !d.zerofill && anchor >= d.addr && anchor + 8 <= d.addr + d.size)
                {
                    let pos = (d.offset + anchor - d.addr) as usize;
                    buf[pos..pos + 8]
                        .copy_from_slice(&crate::trace::anchor(s.addr, anchor, 8, false));
                }
            }
        }
    }

    for (a, t) in &o.anchors {
        let addr = |n: &str| {
            img.l
                .syms
                .find(n)
                .and_then(|id| img.sym_addr(id))
                .ok_or_else(|| format!("-nabi_anchor: {n} is not defined"))
        };
        let (at, to) = (addr(a)?, addr(t)?);
        let d = img
            .sects
            .iter()
            .find(|d| !d.zerofill && at >= d.addr && at + 8 <= d.addr + d.size)
            .ok_or_else(|| format!("-nabi_anchor: {a} is not in initialized data"))?;
        let pos = (d.offset + at - d.addr) as usize;
        buf[pos..pos + 8].copy_from_slice(&crate::trace::anchor(to, at, 8, false));
    }

    // Dylib ordinals and the symbols dyld binds.
    let ordinal = |id: SymId| match img.l.syms.get(id).def {
        Def::Dylib { lib } => lib + 1,
        _ => 0,
    };
    let mut imports: Vec<SymId> = fx.binds.iter().map(|b| b.1).collect();
    imports.sort_unstable();
    imports.dedup();
    let first = usize::from(!o.dylib);
    let rebase = rebase_opcodes(&img.segs, first, fx.rebases.clone())?;
    let bind = bind_opcodes(&img.segs, first, img.l, fx.binds.clone(), &ordinal)?;

    // Exports.
    let syms = &img.l.syms;
    let live_def = |id: SymId| match syms.get(id).def {
        Def::Defined { atom, .. } => img.l.atoms[atom as usize].live,
        Def::Absolute(_) | Def::Synthetic => true,
        _ => false,
    };
    // `-exported_symbol` lists the only exports.
    let unlisted = |id: SymId| !o.exported.is_empty() && !o.exported.contains(&syms.get(id).name);
    let exports: Vec<(String, u64, u64)> = if o.no_exports {
        Vec::new()
    } else {
        (0..syms.syms.len() as SymId)
            .filter(|&id| !syms.get(id).private_extern && !unlisted(id) && live_def(id))
            .filter_map(|id| {
                let s = syms.get(id);
                let addr = img.sym_addr(id)?;
                let flags = match s.def {
                    Def::Absolute(_) => 2,
                    _ if s.tlv => 1,
                    _ => 0,
                };
                let off = if flags == 2 { addr } else { addr - base };
                Some((s.name.clone(), flags, off))
            })
            .collect()
    };
    let export_trie = trie::build(exports);

    // Function starts.
    let mut starts: Vec<u64> = code_atoms
        .iter()
        .map(|&a| img.l.atoms[a as usize].addr)
        .collect();
    starts.sort_unstable();
    starts.dedup();
    let mut fstarts = Vec::new();
    let mut prev = base;
    for s in starts {
        if s > prev || fstarts.is_empty() {
            uleb(&mut fstarts, s - prev);
            prev = s;
        }
    }
    if !fstarts.is_empty() {
        fstarts.push(0);
    }
    pad8(&mut fstarts);

    // Symbol table: stabs and locals, external definitions, imports.
    let mut strtab = StrTab {
        b: vec![b' ', 0],
        seen: HashMap::default(),
    };
    let sect_no = |a: AtomId| (img.l.atoms[a as usize].out + 1) as u8;
    let code_out = |a: AtomId| {
        let s = &img.sects[img.l.atoms[a as usize].out as usize];
        s.flags & text_code != 0
    };
    let mut locals: Vec<Nlist> = Vec::new();
    // Per file: (name, atom, off, external).
    let mut by_file: Vec<Vec<(String, AtomId, u64, bool)>> = vec![Vec::new(); img.l.files.len()];
    if !o.strip_locals {
        for (fi, f) in img.l.files.iter().enumerate() {
            for (n, a, off) in &f.locals {
                if img.l.atoms[*a as usize].live {
                    by_file[fi].push((n.clone(), *a, *off, false));
                }
            }
        }
    }
    let mut extdefs: Vec<Nlist> = Vec::new();
    let mut ext_ids: Vec<SymId> = (0..syms.syms.len() as SymId)
        .filter(|&id| live_def(id))
        .collect();
    ext_ids.sort_by(|a, b| syms.get(*a).name.cmp(&syms.get(*b).name));
    // `-no_exported_symbols` hides every global, as lld does: with `-x`
    // the symbol table keeps only imports.
    let hidden = |id: SymId| syms.get(id).private_extern || o.no_exports || unlisted(id);
    for &id in &ext_ids {
        let s = syms.get(id);
        if let Def::Defined { atom, off } = s.def {
            if hidden(id) {
                if !o.strip_locals {
                    by_file[s.file as usize].push((s.name.clone(), atom, off, false));
                }
                continue;
            }
            by_file[s.file as usize].push((s.name.clone(), atom, off, true));
        }
    }
    let mut stabs: Vec<Nlist> = Vec::new();
    for (fi, list) in by_file.iter_mut().enumerate() {
        list.sort_by_key(|e| img.l.atoms[e.1 as usize].addr + e.2);
        let f = &img.l.files[fi];
        if f.debug && !list.is_empty() {
            let src = f.source.clone().unwrap_or_else(|| f.oso.clone());
            stabs.push(Nlist {
                strx: strtab.add(&src),
                ty: N_SO,
                sect: 0,
                desc: 0,
                value: 0,
            });
            stabs.push(Nlist {
                strx: strtab.add(&f.oso),
                ty: N_OSO,
                sect: 0,
                desc: 1,
                // No timestamp, as lld: clean builds stay byte-identical
                // and dsymutil skips its staleness check.
                value: 0,
            });
            for (n, a, off, ext) in list.iter() {
                let addr = img.l.atoms[*a as usize].addr + off;
                let strx = strtab.add(n);
                if code_out(*a) {
                    stabs.push(Nlist {
                        strx,
                        ty: N_FUN,
                        sect: sect_no(*a),
                        desc: 0,
                        value: addr,
                    });
                    let size = img.l.atoms[*a as usize].size.saturating_sub(*off);
                    stabs.push(Nlist {
                        strx: 1,
                        ty: N_FUN,
                        sect: 0,
                        desc: 0,
                        value: size,
                    });
                } else {
                    let ty = if *ext { N_GSYM } else { N_STSYM };
                    stabs.push(Nlist {
                        strx,
                        ty,
                        sect: sect_no(*a),
                        desc: 0,
                        value: addr,
                    });
                }
            }
            stabs.push(Nlist {
                strx: 1,
                ty: N_SO,
                sect: 1,
                desc: 0,
                value: 0,
            });
        }
        for (n, a, off, ext) in list.iter() {
            if *ext {
                continue;
            }
            let pext = syms
                .find(n)
                .is_some_and(|id| hidden(id) && syms.get(id).file == fi as u32);
            locals.push(Nlist {
                strx: strtab.add(n),
                ty: N_SECT | if pext { N_PEXT } else { 0 },
                sect: sect_no(*a),
                desc: 0,
                value: img.l.atoms[*a as usize].addr + off,
            });
        }
    }
    for &id in &ext_ids {
        let s = syms.get(id);
        if hidden(id) {
            continue;
        }
        let (ty, sect, value) = match s.def {
            Def::Defined { atom, off } => (
                N_SECT | N_EXT,
                sect_no(atom),
                img.l.atoms[atom as usize].addr + off,
            ),
            Def::Absolute(v) => (N_ABS | N_EXT, 0, v),
            Def::Synthetic => (N_SECT | N_EXT, 1, base),
            _ => continue,
        };
        let mut desc = 0u16;
        if s.weak_def {
            desc |= 0x80;
        }
        if s.def == Def::Synthetic {
            // REFERENCED_DYNAMICALLY
            desc |= 0x10;
        }
        extdefs.push(Nlist {
            strx: strtab.add(&s.name),
            ty,
            sect,
            desc,
            value,
        });
    }
    // Imports: what dyld binds, sorted.
    let mut undef_ids: Vec<SymId> = imports.clone();
    undef_ids.extend(img.stubs.iter().copied());
    undef_ids.sort_by(|a, b| syms.get(*a).name.cmp(&syms.get(*b).name));
    undef_ids.dedup();
    let nlocal = stabs.len() + locals.len();
    let iundef = nlocal + extdefs.len();
    let mut undef_index: HashMap<SymId, u32> = HashMap::default();
    let mut undefs: Vec<Nlist> = Vec::new();
    for (k, &id) in undef_ids.iter().enumerate() {
        let s = syms.get(id);
        undef_index.insert(id, (iundef + k) as u32);
        let desc = ((ordinal(id) as u16) << 8) | if s.weak_ref { 0x40 } else { 0 };
        undefs.push(Nlist {
            strx: strtab.add(&s.name),
            ty: N_EXT,
            sect: 0,
            desc,
            value: 0,
        });
    }
    let mut indirect: Vec<u32> = Vec::new();
    for &id in &img.stubs {
        indirect.push(undef_index[&id]);
    }
    for &t in &img.got {
        indirect.push(match img.dylib_sym(t) {
            Some(id) => undef_index[&id],
            None if is_abs(t) => INDIRECT_SYMBOL_LOCAL | INDIRECT_SYMBOL_ABS,
            None => INDIRECT_SYMBOL_LOCAL,
        });
    }
    let mut symtab = Vec::new();
    for n in stabs.iter().chain(&locals).chain(&extdefs).chain(&undefs) {
        put32(&mut symtab, n.strx);
        symtab.push(n.ty);
        symtab.push(n.sect);
        symtab.extend_from_slice(&n.desc.to_le_bytes());
        put64(&mut symtab, n.value);
    }
    let nsyms = stabs.len() + locals.len() + extdefs.len() + undefs.len();
    let mut indirect_b = Vec::new();
    for v in &indirect {
        put32(&mut indirect_b, *v);
    }
    pad8(&mut indirect_b);
    let mut strs = strtab.b;
    pad8(&mut strs);

    // __LINKEDIT layout.
    let mut le = Vec::new();
    let place = |le: &mut Vec<u8>, b: &[u8]| -> (u32, u32) {
        let off = linkedit_off + le.len() as u64;
        le.extend_from_slice(b);
        (if b.is_empty() { 0 } else { off as u32 }, b.len() as u32)
    };
    let rebase_at = place(&mut le, &rebase);
    let bind_at = place(&mut le, &bind);
    let export_at = place(&mut le, &export_trie);
    let fstarts_at = place(&mut le, &fstarts);
    let dic_at = (linkedit_off as u32 + le.len() as u32, 0u32);
    let symtab_at = place(&mut le, &symtab);
    let indirect_at = place(&mut le, &indirect_b);
    let strtab_at = place(&mut le, &strs);
    while le.len() % 16 != 0 {
        le.push(0);
    }
    let sig_off = linkedit_off + le.len() as u64;
    let sig_size = sign::size(sig_off, &ident);
    let file_size = sig_off + sig_size;
    let linkedit_size = file_size - linkedit_off;

    // Header and load commands.
    let mut h: Vec<u8> = Vec::with_capacity(header as usize);
    let has_tlv = img.tlv.is_some()
        || img
            .sects
            .iter()
            .any(|s| s.flags & 0xff == S_THREAD_LOCAL_VARIABLES);
    let flags = MH_NOUNDEFS
        | MH_DYLDLINK
        | MH_TWOLEVEL
        | if o.dylib {
            MH_NO_REEXPORTED_DYLIBS
        } else {
            MH_PIE
        }
        | if has_tlv { MH_HAS_TLV_DESCRIPTORS } else { 0 };
    for v in [
        MH_MAGIC_64,
        img.l.cpu,
        0,
        if o.dylib { MH_DYLIB } else { MH_EXECUTE },
        ncmds as u32,
        sizeofcmds as u32,
        flags,
        0,
    ] {
        put32(&mut h, v);
    }
    let segment = |h: &mut Vec<u8>,
                   name: &str,
                   addr: u64,
                   vmsize: u64,
                   off: u64,
                   filesize: u64,
                   prot: u32,
                   nsects: usize,
                   sflags: u32| {
        put32(h, LC_SEGMENT_64);
        put32(h, (72 + 80 * nsects) as u32);
        name16(h, name);
        put64(h, addr);
        put64(h, vmsize);
        put64(h, off);
        put64(h, filesize);
        put32(h, prot);
        put32(h, prot);
        put32(h, nsects as u32);
        put32(h, sflags);
    };
    if !o.dylib {
        segment(&mut h, "__PAGEZERO", 0, BASE, 0, 0, 0, 0, 0);
    }
    for seg in &img.segs {
        let sflags = if seg.name == "__DATA_CONST" {
            SG_READ_ONLY
        } else {
            0
        };
        segment(
            &mut h,
            &seg.name,
            seg.addr,
            seg.vmsize,
            seg.offset,
            seg.filesize,
            seg.prot,
            seg.sects.len(),
            sflags,
        );
        for &si in &seg.sects {
            let s = &img.sects[si];
            name16(&mut h, &s.name);
            name16(&mut h, &seg.name);
            put64(&mut h, s.addr);
            put64(&mut h, s.size);
            put32(&mut h, s.offset as u32);
            put32(&mut h, u32::from(s.align));
            put32(&mut h, 0);
            put32(&mut h, 0);
            put32(&mut h, s.flags);
            let (r1, r2) = match synth.get(si).copied().unwrap_or(Synth::None) {
                Synth::Stubs => (0, a64::STUB_SIZE as u32),
                Synth::Got => (img.stubs.len() as u32, 0),
                _ => (0, 0),
            };
            put32(&mut h, r1);
            put32(&mut h, r2);
            put32(&mut h, 0);
        }
    }
    segment(
        &mut h,
        "__LINKEDIT",
        linkedit_addr,
        linkedit_size.next_multiple_of(PAGE),
        linkedit_off,
        linkedit_size,
        1,
        0,
        0,
    );
    put32(&mut h, LC_DYLD_INFO_ONLY);
    put32(&mut h, 48);
    for v in [
        rebase_at.0,
        rebase_at.1,
        bind_at.0,
        bind_at.1,
        0,
        0,
        0,
        0,
        export_at.0,
        export_at.1,
    ] {
        put32(&mut h, v);
    }
    put32(&mut h, LC_SYMTAB);
    put32(&mut h, 24);
    for v in [symtab_at.0, nsyms as u32, strtab_at.0, strtab_at.1] {
        put32(&mut h, v);
    }
    put32(&mut h, LC_DYSYMTAB);
    put32(&mut h, 80);
    for v in [
        0,
        nlocal as u32,
        nlocal as u32,
        extdefs.len() as u32,
        iundef as u32,
        undefs.len() as u32,
        0,
        0,
        0,
        0,
        0,
        0,
        if indirect.is_empty() {
            0
        } else {
            indirect_at.0
        },
        indirect.len() as u32,
        0,
        0,
        0,
        0,
    ] {
        put32(&mut h, v);
    }
    let lc_str = |h: &mut Vec<u8>, cmd: u32, fixed: &[u32], s: &str| {
        let head = 8 + 4 * fixed.len();
        let size = (head + 4 + s.len() + 1).next_multiple_of(8);
        let dylib = matches!(cmd, LC_LOAD_DYLIB | LC_ID_DYLIB);
        let size = if dylib {
            (24 + s.len() + 1).next_multiple_of(8)
        } else {
            size
        };
        put32(h, cmd);
        put32(h, size as u32);
        let start = h.len() - 8;
        if dylib {
            put32(h, 24);
            for v in fixed {
                put32(h, *v);
            }
        } else {
            put32(h, 12);
        }
        h.extend_from_slice(s.as_bytes());
        h.resize(start + size, 0);
    };
    if o.dylib {
        lc_str(
            &mut h,
            LC_ID_DYLIB,
            &[1, o.current_version, o.compat_version],
            &install_name,
        );
    } else {
        lc_str(&mut h, LC_LOAD_DYLINKER, &[], DYLD_PATH);
    }
    let uuid_at = h.len() + 8;
    put32(&mut h, LC_UUID);
    put32(&mut h, 24);
    h.extend_from_slice(&[0; 16]);
    put32(&mut h, LC_BUILD_VERSION);
    put32(&mut h, 32);
    for v in [o.platform, o.min_os, o.sdk, 1, 3, 0] {
        put32(&mut h, v);
    }
    put32(&mut h, LC_SOURCE_VERSION);
    put32(&mut h, 16);
    put64(&mut h, 0);
    if !o.dylib {
        let entry = syms
            .find(&o.entry)
            .and_then(|id| img.sym_addr(id))
            .ok_or_else(|| format!("entry symbol {} is not defined", o.entry))?;
        put32(&mut h, LC_MAIN);
        put32(&mut h, 24);
        put64(&mut h, entry - BASE);
        put64(&mut h, 0);
    }
    for d in &img.l.dylibs {
        lc_str(
            &mut h,
            LC_LOAD_DYLIB,
            &[2, d.current, d.compat],
            &d.install_name,
        );
    }
    for p in &o.rpaths {
        lc_str(&mut h, LC_RPATH, &[], p);
    }
    for (cmd, at) in [
        (LC_FUNCTION_STARTS, fstarts_at),
        (LC_DATA_IN_CODE, dic_at),
        (LC_CODE_SIGNATURE, (sig_off as u32, sig_size as u32)),
    ] {
        put32(&mut h, cmd);
        put32(&mut h, 16);
        put32(&mut h, at.0);
        put32(&mut h, at.1);
    }
    debug_assert_eq!(h.len(), 32 + sizeofcmds);
    if h.len() as u64 > header {
        return Err("load commands overflow the header".to_owned());
    }
    buf[..h.len()].copy_from_slice(&h);
    buf.extend_from_slice(&le);
    buf.resize(file_size as usize, 0);

    // UUID: a digest of the page hashes without it; then its page is
    // hashed again for the signature.
    let code = sig_off as usize;
    let mut hashes = sign::page_hashes(&buf[..code]);
    let digest = nabic_core::hash::sha256(hashes.as_flattened());
    let mut uuid = [0u8; 16];
    uuid.copy_from_slice(&digest[..16]);
    uuid[6] = (uuid[6] & 0x0f) | 0x30;
    uuid[8] = (uuid[8] & 0x3f) | 0x80;
    buf[uuid_at..uuid_at + 16].copy_from_slice(&uuid);
    for k in uuid_at / 4096..=(uuid_at + 15) / 4096 {
        sign::rehash(&buf[..code], &mut hashes, k);
    }
    let text = img
        .segs
        .iter()
        .find(|s| s.name == "__TEXT")
        .map_or((0, 0), |s| (s.offset, s.filesize));
    sign::sign(&mut buf, sig_off, &hashes, &ident, text, !o.dylib);

    // A new file: the kernel caches signatures by vnode.
    let _ = std::fs::remove_file(&o.out);
    std::fs::write(&o.out, &buf).map_err(|e| format!("cannot write {}: {e}", o.out.display()))?;
    #[cfg(unix)]
    {
        let _ = std::fs::set_permissions(&o.out, std::fs::Permissions::from_mode(0o755));
    }
    Ok(())
}
