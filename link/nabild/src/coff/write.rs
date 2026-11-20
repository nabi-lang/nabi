//! The PE32+ / PE32 image: the linker's own sections (import and export
//! tables, local import pointers, commons), output sections as lld-link
//! forms them, relocations, base relocations, headers.

use crate::bytes::{put32, u32le};
use crate::coff::{
    Arch, BSS, CODE, DATA, DEAD, Export, Link, Options, RDATA, SCN_CNT_CODE,
    SCN_CNT_INITIALIZED_DATA, SCN_CNT_UNINITIALIZED_DATA, SCN_MEM_DISCARDABLE,
};
use crate::symtab::{AtomId, Def, Reloc, SymId, Target};
use nabic_core::HashMap;
use std::collections::BTreeMap;

const SECTION_ALIGN: u64 = 0x1000;
const FILE_ALIGN: u64 = 0x200;

const AMD64_ADDR64: u16 = 1;
const AMD64_ADDR32: u16 = 2;
const AMD64_ADDR32NB: u16 = 3;
const AMD64_REL32: u16 = 4;
const AMD64_SECTION: u16 = 0xa;
const AMD64_SECREL: u16 = 0xb;

const ARM64_ADDR32: u16 = 1;
const ARM64_ADDR32NB: u16 = 2;
const ARM64_BRANCH26: u16 = 3;
const ARM64_PAGEBASE_REL21: u16 = 4;
const ARM64_REL21: u16 = 5;
const ARM64_PAGEOFFSET_12A: u16 = 6;
const ARM64_PAGEOFFSET_12L: u16 = 7;
const ARM64_SECREL: u16 = 8;
const ARM64_SECREL_LOW12A: u16 = 9;
const ARM64_SECREL_HIGH12A: u16 = 0xa;
const ARM64_SECREL_LOW12L: u16 = 0xb;
const ARM64_SECTION: u16 = 0xd;
const ARM64_ADDR64: u16 = 0xe;
const ARM64_BRANCH19: u16 = 0xf;
const ARM64_BRANCH14: u16 = 0x10;
const ARM64_REL32: u16 = 0x11;

const I386_DIR16: u16 = 1;
const I386_REL16: u16 = 2;
const I386_DIR32: u16 = 6;
const I386_DIR32NB: u16 = 7;
const I386_SECTION: u16 = 0xa;
const I386_SECREL: u16 = 0xb;
const I386_SECREL7: u16 = 0xd;
const I386_REL32: u16 = 0x14;

const BASED_HIGHLOW: u16 = 3;
const BASED_DIR64: u16 = 10;

/// lld-link's DOS program: prints that it cannot run in DOS.
const DOS_PROGRAM: [u8; 56] = [
    0x0e, 0x1f, 0xba, 0x0e, 0x00, 0xb4, 0x09, 0xcd, 0x21, 0xb8, 0x01, 0x4c, 0xcd, 0x21, 0x54, 0x68,
    0x69, 0x73, 0x20, 0x70, 0x72, 0x6f, 0x67, 0x72, 0x61, 0x6d, 0x20, 0x63, 0x61, 0x6e, 0x6e, 0x6f,
    0x74, 0x20, 0x62, 0x65, 0x20, 0x72, 0x75, 0x6e, 0x20, 0x69, 0x6e, 0x20, 0x44, 0x4f, 0x53, 0x20,
    0x6d, 0x6f, 0x64, 0x65, 0x2e, 0x24, 0x00, 0x00,
];
const DOS_STUB: u64 = 64 + DOS_PROGRAM.len() as u64;

/// lld-link's (and link.exe's) default image base of an executable.
const fn image_base(arch: Arch) -> u64 {
    if arch.is64() {
        0x1_4000_0000
    } else {
        0x40_0000
    }
}

/// `(relocation type, log2 size)` of a pointer.
const fn addr(arch: Arch) -> (u16, u8) {
    match arch {
        Arch::X86_64 => (AMD64_ADDR64, 3),
        Arch::Aarch64 => (ARM64_ADDR64, 3),
        Arch::X86 => (I386_DIR32, 2),
    }
}

const fn addr32nb(arch: Arch) -> u16 {
    match arch {
        Arch::X86_64 => AMD64_ADDR32NB,
        Arch::Aarch64 => ARM64_ADDR32NB,
        Arch::X86 => I386_DIR32NB,
    }
}

const fn reloc(off: u32, kind: u16, len: u8, target: Target, addend: i64) -> Reloc {
    Reloc {
        off,
        kind,
        len,
        pcrel: false,
        target,
        addend,
        sub: None,
    }
}

/// `__imp_<name>` of a symbol defined here rather than in a DLL: a
/// pointer to it, as lld-link makes.
pub fn local_imports(l: &mut Link) {
    let want: Vec<(SymId, SymId)> = l
        .syms
        .syms
        .iter()
        .enumerate()
        .filter(|(_, s)| s.referenced && matches!(s.def, Def::Undefined | Def::Lazy { .. }))
        .filter_map(|(i, s)| {
            let base = l.syms.find(s.name.strip_prefix("__imp_")?)?;
            let d = l.syms.get(base).def;
            matches!(d, Def::Defined { .. } | Def::Absolute(_) | Def::Synthetic)
                .then_some((i as SymId, base))
        })
        .collect();
    let (kind, len) = addr(l.arch);
    for (imp, base) in want {
        let a = l.synth(".rdata", RDATA, len, vec![0; 1 << len], 0);
        l.atoms[a as usize]
            .relocs
            .push(reloc(0, kind, len, Target::Sym(base), 0));
        l.syms.get_mut(imp).def = Def::Defined { atom: a, off: 0 };
    }
}

/// Zero fill for every tentative definition.
pub fn commons(l: &mut Link) {
    let commons: Vec<(SymId, u64, u8)> = l
        .syms
        .syms
        .iter()
        .enumerate()
        .filter_map(|(i, s)| match s.def {
            Def::Common { size, align } => Some((i as SymId, size, align)),
            _ => None,
        })
        .collect();
    for (id, size, align) in commons {
        let a = l.synth(".bss", BSS, align, Vec::new(), size);
        l.syms.get_mut(id).def = Def::Defined { atom: a, off: 0 };
    }
}

/// `/export-all-symbols` (mingw): every external definition of an
/// object, but import symbols, the linker's and the CRT's own.
pub fn export_all(l: &mut Link) {
    const EXCLUDED: &[&str] = &[
        "__NULL_IMPORT_DESCRIPTOR",
        "_pei386_runtime_relocator",
        "do_pseudo_reloc",
        "impure_ptr",
        "_impure_ptr",
        "_fmode",
        "environ",
        "__dso_handle",
        "_load_config_used",
        "DllMain",
        "DllEntryPoint",
        "DllMainCRTStartup",
    ];
    const PREFIXES: &[&str] = &[
        "__imp_",
        "__IMPORT_DESCRIPTOR_",
        "__nm_",
        "__rtti_",
        "__builtin_",
        ".",
        "__profc_",
        "__profd_",
        "__profvp_",
        "_head_",
        "\0",
    ];
    const SUFFIXES: &[&str] = &["_iname", "_NULL_THUNK_DATA"];
    let mut have: nabic_core::HashSet<String> = l.exports.iter().map(|e| e.name.clone()).collect();
    let mut add = Vec::new();
    for (i, s) in l.syms.syms.iter().enumerate() {
        let Def::Defined { atom, .. } = s.def else {
            continue;
        };
        let a = &l.atoms[atom as usize];
        if a.file == 0 || a.file == u32::MAX {
            continue;
        }
        let sym = s.name.as_str();
        if l.syms.find(&format!("__imp_{sym}")).is_some() {
            continue;
        }
        // x86: the C name, exported without the `_` (and matched without
        // a stdcall `@<n>`).
        let (n, c) = match sym.strip_prefix('_') {
            Some(n) if l.arch == Arch::X86 => (n, n.split_once('@').map_or(n, |(c, _)| c)),
            _ => (sym, sym),
        };
        let excluded = |n: &str, c: &str| {
            EXCLUDED.contains(&c)
                || PREFIXES.iter().any(|p| n.starts_with(p))
                || SUFFIXES.iter().any(|x| n.ends_with(x))
        };
        if excluded(sym, sym) || excluded(n, c) {
            continue;
        }
        let f = &l.files[a.file as usize];
        let file = f.name.rsplit(['/', '\\', '(']).next().unwrap_or("");
        if f.archive.is_empty()
            && matches!(
                file.trim_end_matches(')'),
                "crt0.o"
                    | "crt1.o"
                    | "crt1u.o"
                    | "crt2.o"
                    | "crt2u.o"
                    | "dllcrt1.o"
                    | "dllcrt2.o"
                    | "gcrt0.o"
                    | "gcrt1.o"
                    | "gcrt2.o"
                    | "crtbegin.o"
                    | "crtend.o"
            )
        {
            continue;
        }
        if have.insert(n.to_owned()) {
            add.push(Export {
                name: n.to_owned(),
                sym: i as SymId,
                data: !l.sect(atom).code(),
            });
        }
    }
    l.exports.extend(add);
}

/// The import directory, lookup and address tables of the live imports,
/// by DLL in the order they were first seen, each DLL's by name:
/// `(.idata$2 without its terminator, the terminator, .idata$4, .idata$5,
/// .idata$6)`.
type Idata = (Vec<AtomId>, AtomId, Vec<AtomId>, Vec<AtomId>, Vec<AtomId>);

fn import_tables(l: &mut Link) -> Option<Idata> {
    let mut order: Vec<String> = Vec::new();
    let mut by_dll: HashMap<String, Vec<usize>> = HashMap::default();
    for (k, imp) in l.imports.iter().enumerate() {
        if !l.atoms[imp.iat as usize].live {
            continue;
        }
        let key = imp.dll.to_ascii_lowercase();
        if !by_dll.contains_key(&key) {
            order.push(key.clone());
        }
        by_dll.entry(key).or_default().push(k);
    }
    if order.is_empty() {
        return None;
    }
    let rva = addr32nb(l.arch);
    let ptr = addr(l.arch).1;
    let (mut dirs, mut ilt, mut iat, mut names) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for key in &order {
        let mut ks = by_dll.remove(key).unwrap_or_default();
        ks.sort_by(|&a, &b| l.imports[a].sym.cmp(&l.imports[b].sym));
        let first = (l.imports[ks[0]].ilt, l.imports[ks[0]].iat);
        let mut dll = l.imports[ks[0]].dll.clone().into_bytes();
        dll.push(0);
        for &k in &ks {
            let imp = &l.imports[k];
            ilt.push(imp.ilt);
            iat.push(imp.iat);
            names.extend(imp.hint_name);
        }
        for (list, sect) in [(&mut ilt, ".idata$4"), (&mut iat, ".idata$5")] {
            let z = l.synth(sect, RDATA, ptr, vec![0; 1 << ptr], 0);
            l.atoms[z as usize].live = true;
            list.push(z);
        }
        let name = l.synth(".idata$7", RDATA, 1, dll, 0);
        l.atoms[name as usize].live = true;
        names.push(name);
        let d = l.synth(".idata$2", RDATA, 2, vec![0; 20], 0);
        l.atoms[d as usize].live = true;
        l.atoms[d as usize].relocs.extend([
            reloc(0, rva, 2, Target::Atom(first.0), 0),
            reloc(12, rva, 2, Target::Atom(name), 0),
            reloc(16, rva, 2, Target::Atom(first.1), 0),
        ]);
        dirs.push(d);
    }
    let null = l.synth(".idata$3", RDATA, 2, vec![0; 20], 0);
    l.atoms[null as usize].live = true;
    Some((dirs, null, ilt, iat, names))
}

/// The export directory of `l.exports` in one atom, the DLL named
/// `dll`.
fn export_table(l: &mut Link, dll: &str) -> Option<AtomId> {
    if l.exports.is_empty() {
        return None;
    }
    let mut ex = l.exports.clone();
    ex.sort_by(|a, b| a.name.cmp(&b.name));
    ex.dedup_by(|a, b| a.name == b.name);
    let n = ex.len();
    let addrs = 40;
    let ptrs = addrs + 4 * n;
    let ords = ptrs + 4 * n;
    let dll_at = ords + 2 * n;
    let mut b = vec![0u8; dll_at];
    b.extend_from_slice(dll.as_bytes());
    b.push(0);
    let mut name_at = Vec::with_capacity(n);
    for e in &ex {
        name_at.push(b.len());
        b.extend_from_slice(e.name.as_bytes());
        b.push(0);
    }
    b[16..20].copy_from_slice(&1u32.to_le_bytes());
    b[20..24].copy_from_slice(&(n as u32).to_le_bytes());
    b[24..28].copy_from_slice(&(n as u32).to_le_bytes());
    for k in 0..n {
        let o = ords + 2 * k;
        b[o..o + 2].copy_from_slice(&(k as u16).to_le_bytes());
    }
    let a = l.synth(".edata", RDATA, 2, b, 0);
    let rva = addr32nb(l.arch);
    let me = Target::Atom(a);
    let mut rs = vec![
        reloc(12, rva, 2, me, dll_at as i64),
        reloc(28, rva, 2, me, addrs as i64),
        reloc(32, rva, 2, me, ptrs as i64),
        reloc(36, rva, 2, me, ords as i64),
    ];
    for (k, e) in ex.iter().enumerate() {
        rs.push(reloc((addrs + 4 * k) as u32, rva, 2, Target::Sym(e.sym), 0));
        rs.push(reloc((ptrs + 4 * k) as u32, rva, 2, me, name_at[k] as i64));
    }
    l.atoms[a as usize].relocs = rs;
    l.atoms[a as usize].live = true;
    Some(a)
}

/// An output section under construction.
#[derive(Debug, Default)]
pub struct OutSec {
    name: String,
    pub chars: u32,
    atoms: Vec<AtomId>,
    pub rva: u64,
    size: u64,
    /// Initialized bytes (the rest is zero fill).
    init: u64,
    file_off: u64,
}

/// `.text$mn` goes to `.text`.
fn base_name(n: &str) -> &str {
    n.split('$').next().unwrap_or(n)
}

const PERM_MASK: u32 = 0xfe00_0000;
const TYPE_MASK: u32 = SCN_CNT_CODE | SCN_CNT_INITIALIZED_DATA | SCN_CNT_UNINITIALIZED_DATA;

/// Output sections in lld-link's order: its built-in ones, then the rest
/// by input section name; `$` suffixes order the pieces of a section.
fn sections(l: &Link, o: &Options, idata: Option<&Idata>) -> Vec<OutSec> {
    let mut partials: BTreeMap<(String, u32), Vec<AtomId>> = BTreeMap::new();
    if let Some((dirs, _, ilt, iat, names)) = idata {
        partials.insert((".idata$2".to_owned(), RDATA), dirs.clone());
        partials.insert((".idata$4".to_owned(), RDATA), ilt.clone());
        partials.insert((".idata$5".to_owned(), RDATA), iat.clone());
        partials.insert((".idata$6".to_owned(), RDATA), names.clone());
    }
    let ours: nabic_core::HashSet<AtomId> = idata
        .map(|(d, n, a, b, c)| {
            d.iter()
                .chain(a)
                .chain(b)
                .chain(c)
                .copied()
                .chain([*n])
                .collect()
        })
        .unwrap_or_default();
    for (k, a) in l.atoms.iter().enumerate() {
        let k = k as AtomId;
        if !a.live || ours.contains(&k) {
            continue;
        }
        let s = l.sect(k);
        let chars = s.chars & (PERM_MASK | TYPE_MASK);
        partials.entry((s.name.clone(), chars)).or_default().push(k);
    }
    if let Some((_, null, ..)) = idata {
        partials
            .entry((".idata$3".to_owned(), RDATA))
            .or_default()
            .push(*null);
    }
    let mut out: Vec<OutSec> = [
        (".text", CODE),
        (".bss", BSS),
        (".rdata", RDATA),
        (".data", DATA),
        (".pdata", RDATA),
        (".idata", RDATA),
        (".edata", RDATA),
        (".didat", RDATA),
        (".rsrc", RDATA),
        (".ctors", DATA),
        (".dtors", DATA),
    ]
    .iter()
    .map(|&(n, c)| OutSec {
        name: n.to_owned(),
        chars: c,
        ..OutSec::default()
    })
    .collect();
    for ((name, chars), atoms) in partials {
        let base = base_name(&name);
        let chars = if base == ".CRT" { RDATA } else { chars };
        let k = out
            .iter()
            .position(|s| s.name == base && s.chars == chars)
            .unwrap_or_else(|| {
                out.push(OutSec {
                    name: base.to_owned(),
                    chars,
                    ..OutSec::default()
                });
                out.len() - 1
            });
        out[k].atoms.extend(atoms);
    }
    // Merges: the objects' and lld-link's, in name order, `.bss` last.
    let mut rules: BTreeMap<String, String> = BTreeMap::new();
    for (f, t) in &l.merges {
        rules.entry(f.clone()).or_insert_with(|| t.clone());
    }
    for (f, t) in [
        (".idata", ".rdata"),
        (".didat", ".rdata"),
        (".edata", ".rdata"),
        (".xdata", ".rdata"),
        (".00cfg", ".rdata"),
        (".bss", ".data"),
    ] {
        rules.entry(f.to_owned()).or_insert_with(|| t.to_owned());
    }
    if o.mingw {
        for f in [".ctors", ".dtors", ".CRT"] {
            rules
                .entry(f.to_owned())
                .or_insert_with(|| ".rdata".to_owned());
        }
    }
    let bss = rules.remove(".bss");
    let order: Vec<(String, String)> = rules
        .iter()
        .map(|(f, t)| (f.clone(), t.clone()))
        .chain(bss.map(|t| (".bss".to_owned(), t)))
        .collect();
    for (from, mut to) in order {
        for _ in 0..8 {
            match rules.get(&to) {
                Some(n) if *n != to => to.clone_from(n),
                _ => break,
            }
        }
        if from == to {
            continue;
        }
        let Some(fi) = out.iter().position(|s| s.name == from) else {
            continue;
        };
        if let Some(ti) = out.iter().position(|s| s.name == to) {
            let atoms = std::mem::take(&mut out[fi].atoms);
            let code = out[fi].chars & SCN_CNT_CODE != 0;
            let t = &mut out[ti];
            t.atoms.extend(atoms);
            if code {
                t.chars |= SCN_CNT_CODE;
                t.chars &= !(SCN_CNT_INITIALIZED_DATA | SCN_CNT_UNINITIALIZED_DATA);
            }
            out.remove(fi);
        } else {
            out[fi].name = to;
        }
    }
    out.retain(|s| !s.atoms.is_empty());
    out.sort_by_key(|s| s.chars & SCN_MEM_DISCARDABLE != 0);
    out
}

/// The RVA a relocation target resolves to (absolute symbols: their
/// address less the image base, wrapping).
fn target_rva(l: &Link, t: Target) -> Option<u64> {
    match t {
        Target::Atom(a) => Some(l.atoms[a as usize].addr),
        Target::Sym(id) => match l.syms.get(id).def {
            Def::Defined { atom, off } => Some(l.atoms[atom as usize].addr + off),
            Def::Absolute(v) => Some(v.wrapping_sub(image_base(l.arch))),
            Def::Synthetic => Some(0),
            _ => None,
        },
    }
}

/// The output section (0-based) holding target `t`.
fn target_sect(l: &Link, t: Target) -> Option<u32> {
    let a = match t {
        Target::Atom(a) => a,
        Target::Sym(id) => match l.syms.get(id).def {
            Def::Defined { atom, .. } => atom,
            _ => return None,
        },
    };
    let o = l.atoms[a as usize].out;
    (o != u32::MAX).then_some(o)
}

fn rd32(b: &[u8], at: usize) -> u32 {
    u32le(b, at)
}

fn wr32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

fn add32(b: &mut [u8], at: usize, v: u64) {
    let x = rd32(b, at).wrapping_add(v as u32);
    wr32(b, at, x);
}

fn add16(b: &mut [u8], at: usize, v: u64) {
    let x = u16::from_le_bytes([b[at], b[at + 1]]).wrapping_add(v as u16);
    b[at..at + 2].copy_from_slice(&x.to_le_bytes());
}

/// arm64 `adrp` / `adr` at `p` of `s` plus the immediate already there.
fn a64_addr(b: &mut [u8], at: usize, s: u64, p: u64, shift: u32) -> Result<(), String> {
    let orig = rd32(b, at);
    let imm = ((orig >> 29) & 3) | ((orig >> 3) & 0x1f_fffc);
    let imm = (i64::from(imm) << 43) >> 43;
    let s = s.wrapping_add(imm as u64);
    let d = ((s >> shift) as i64).wrapping_sub((p >> shift) as i64);
    if !(-(1 << 20)..(1 << 20)).contains(&d) {
        return Err(format!(
            "adrp / adr from RVA {p:#x} to {s:#x} is out of range"
        ));
    }
    let d = d as u32;
    let v = (orig & 0x9f00_001f) | ((d & 3) << 29) | (((d >> 2) & 0x7_ffff) << 5);
    wr32(b, at, v);
    Ok(())
}

/// The 12-bit immediate of an arm64 `add` / load / store plus `imm`.
fn a64_imm(b: &mut [u8], at: usize, imm: u64, range_limit: u32) {
    let orig = rd32(b, at);
    let imm = imm.wrapping_add(u64::from((orig >> 10) & 0xfff));
    let v = (orig & !(0xfff << 10)) | (((imm as u32) & (0xfff >> range_limit)) << 10);
    wr32(b, at, v);
}

/// A load / store's scaled immediate.
fn a64_ldr(b: &mut [u8], at: usize, imm: u64) -> Result<(), String> {
    let orig = rd32(b, at);
    let mut size = orig >> 30;
    if orig & 0x0480_0000 == 0x0480_0000 {
        size += 4;
    }
    if imm & ((1 << size) - 1) != 0 {
        return Err(format!("misaligned ldr / str offset {imm:#x}"));
    }
    a64_imm(b, at, imm >> size, size);
    Ok(())
}

fn a64_branch(
    b: &mut [u8],
    at: usize,
    v: i64,
    bits: u32,
    mask: u32,
    shift: u32,
) -> Result<(), String> {
    if !(-(1i64 << (bits - 1))..(1i64 << (bits - 1))).contains(&v) {
        return Err(format!("branch at {at:#x} out of range ({v:#x})"));
    }
    let orig = rd32(b, at);
    let field = if shift == 0 {
        (v as u32 & mask) >> 2
    } else {
        (v as u32 & mask) << shift
    };
    wr32(b, at, orig | field);
    Ok(())
}

/// Apply relocation `r` of an atom at RVA `p0` whose bytes are `b`.
#[allow(clippy::too_many_arguments)]
pub fn apply(l: &Link, b: &mut [u8], r: &Reloc, p0: u64, secs: &[OutSec]) -> Result<(), String> {
    let at = r.off as usize;
    let width = 1usize << r.len;
    if at + width > b.len() {
        return Err(format!("relocation at {at:#x} runs past its section"));
    }
    let s = target_rva(l, r.target)
        .ok_or("relocation against an undefined symbol")?
        .wrapping_add(r.addend as u64);
    let base = image_base(l.arch);
    let p = p0 + u64::from(r.off);
    let secrel = || -> Result<u64, String> {
        let k = target_sect(l, r.target).ok_or("SECREL relocation against no section")?;
        Ok(s.wrapping_sub(secs[k as usize].rva))
    };
    let sect_index = || -> Result<u16, String> {
        let k = target_sect(l, r.target).ok_or("SECTION relocation against no section")?;
        Ok(k as u16 + 1)
    };
    let rel32 = |b: &mut [u8], bias: u64| -> Result<(), String> {
        let v = s.wrapping_sub(p).wrapping_sub(bias) as i64;
        let x = i64::from(rd32(b, at) as i32) + v;
        if i32::try_from(x).is_err() {
            return Err(format!(
                "relocation from RVA {p:#x} to {s:#x} is out of range"
            ));
        }
        wr32(b, at, x as u32);
        Ok(())
    };
    match l.arch {
        Arch::X86_64 => match r.kind {
            AMD64_ADDR64 => {
                let v = u64::from_le_bytes(b[at..at + 8].try_into().unwrap());
                b[at..at + 8].copy_from_slice(&v.wrapping_add(s.wrapping_add(base)).to_le_bytes());
            }
            AMD64_ADDR32 => add32(b, at, s.wrapping_add(base)),
            AMD64_ADDR32NB => add32(b, at, s),
            4..=9 => rel32(b, 4 + u64::from(r.kind - AMD64_REL32))?,
            AMD64_SECTION => add16(b, at, u64::from(sect_index()?)),
            AMD64_SECREL => add32(b, at, secrel()?),
            k => return Err(format!("unsupported relocation type {k:#x}")),
        },
        Arch::X86 => match r.kind {
            I386_DIR32 => add32(b, at, s.wrapping_add(base)),
            I386_DIR32NB => add32(b, at, s),
            I386_REL32 => rel32(b, 4)?,
            I386_SECTION => add16(b, at, u64::from(sect_index()?)),
            I386_SECREL => add32(b, at, secrel()?),
            I386_SECREL7 => {
                let v = secrel()?.wrapping_add(u64::from(b[at]));
                if v > 0x7f {
                    return Err(format!("SECREL7 offset {v:#x} is out of range"));
                }
                b[at] = v as u8;
            }
            I386_DIR16 => add16(b, at, s.wrapping_add(base)),
            I386_REL16 => add16(b, at, s.wrapping_sub(p).wrapping_sub(2)),
            k => return Err(format!("unsupported relocation type {k:#x}")),
        },
        Arch::Aarch64 => match r.kind {
            ARM64_PAGEBASE_REL21 => a64_addr(b, at, s, p, 12)?,
            ARM64_REL21 => a64_addr(b, at, s, p, 0)?,
            ARM64_PAGEOFFSET_12A => a64_imm(b, at, s & 0xfff, 0),
            ARM64_PAGEOFFSET_12L => a64_ldr(b, at, s & 0xfff)?,
            ARM64_BRANCH26 => a64_branch(b, at, s.wrapping_sub(p) as i64, 28, 0x0fff_fffc, 0)?,
            ARM64_BRANCH19 => a64_branch(b, at, s.wrapping_sub(p) as i64, 21, 0x001f_fffc, 3)?,
            ARM64_BRANCH14 => a64_branch(b, at, s.wrapping_sub(p) as i64, 16, 0x0000_fffc, 3)?,
            ARM64_ADDR32 => add32(b, at, s.wrapping_add(base)),
            ARM64_ADDR32NB => add32(b, at, s),
            ARM64_ADDR64 => {
                let v = u64::from_le_bytes(b[at..at + 8].try_into().unwrap());
                b[at..at + 8].copy_from_slice(&v.wrapping_add(s.wrapping_add(base)).to_le_bytes());
            }
            ARM64_SECREL => add32(b, at, secrel()?),
            ARM64_SECREL_LOW12A => a64_imm(b, at, secrel()? & 0xfff, 0),
            ARM64_SECREL_HIGH12A => {
                let orig = rd32(b, at);
                let imm = u64::from((orig >> 10) & 0xfff);
                let v = (secrel()?.wrapping_add(imm) >> 12) as u32;
                wr32(b, at, (orig & !(0xfff << 10)) | ((v & 0xfff) << 10));
            }
            ARM64_SECREL_LOW12L => a64_ldr(b, at, secrel()? & 0xfff)?,
            ARM64_SECTION => add16(b, at, u64::from(sect_index()?)),
            ARM64_REL32 => rel32(b, 4)?,
            k => return Err(format!("unsupported relocation type {k:#x}")),
        },
    }
    Ok(())
}

/// The base relocation type of `r`, if the loader must rebase it.
fn based(l: &Link, r: &Reloc) -> Option<u16> {
    let ty = match (l.arch, r.kind) {
        (Arch::X86_64, AMD64_ADDR64) | (Arch::Aarch64, ARM64_ADDR64) => BASED_DIR64,
        (Arch::X86_64, AMD64_ADDR32) | (Arch::Aarch64, ARM64_ADDR32) | (Arch::X86, I386_DIR32) => {
            BASED_HIGHLOW
        }
        _ => return None,
    };
    match r.target {
        Target::Sym(id) => match l.syms.get(id).def {
            Def::Absolute(_) => None,
            _ => Some(ty),
        },
        Target::Atom(_) => Some(ty),
    }
}

/// `.reloc`: blocks of a 4 KiB page each, padded to 4 bytes.
fn base_relocs(mut v: Vec<(u64, u16)>) -> Vec<u8> {
    v.sort_unstable();
    v.dedup();
    let mut out = Vec::new();
    let mut i = 0;
    while i < v.len() {
        let page = v[i].0 & !0xfff;
        let start = out.len();
        put32(&mut out, page as u32);
        put32(&mut out, 0);
        while i < v.len() && v[i].0 & !0xfff == page {
            let e = (v[i].1 << 12) | (v[i].0 & 0xfff) as u16;
            out.extend_from_slice(&e.to_le_bytes());
            i += 1;
        }
        if (out.len() - start) % 4 != 0 {
            out.extend_from_slice(&0u16.to_le_bytes());
        }
        let size = (out.len() - start) as u32;
        out[start + 4..start + 8].copy_from_slice(&size.to_le_bytes());
    }
    out
}

/// The PDB's public symbols: every external definition in the image.
fn publics(l: &Link, secs: &[OutSec]) -> Vec<crate::coff::pdb::Public> {
    let mut v = Vec::new();
    for s in &l.syms.syms {
        let Def::Defined { atom, off } = s.def else {
            continue;
        };
        let a = &l.atoms[atom as usize];
        if !a.live || a.out == u32::MAX || s.name.starts_with('\0') {
            continue;
        }
        let n = s.name.as_str();
        if ["__profd_", "__profc_", "__covrec_"]
            .iter()
            .any(|p| n.starts_with(p))
        {
            continue;
        }
        let sec = &secs[a.out as usize];
        v.push(crate::coff::pdb::Public {
            name: s.name.clone(),
            sect: a.out as u16 + 1,
            off: (a.addr + off - sec.rva) as u32,
            function: sec.chars & crate::coff::SCN_MEM_EXECUTE != 0,
        });
    }
    v
}

/// Lay out, relocate and write the image.
#[allow(clippy::too_many_lines)]
/// The backtrace table's functions and the runtime's anchor atom, when
/// the runtime is linked in.
fn trace_plan(l: &Link) -> Option<(crate::trace::Plan, (AtomId, u64))> {
    let anchor = match l
        .syms
        .get(l.syms.find(&l.arch.mangle(crate::trace::ANCHOR))?)
        .def
    {
        Def::Defined { atom, off } if l.atoms[atom as usize].live => (atom, off),
        _ => return None,
    };
    let kept: HashMap<AtomId, AtomId> = l.folded.iter().copied().collect();
    let canon = |a: AtomId| {
        let c = kept.get(&a).copied().unwrap_or(a);
        let x = &l.atoms[c as usize];
        (x.live && l.sect(c).chars & SCN_CNT_CODE != 0).then_some((c, x.size))
    };
    let locals = l
        .files
        .iter()
        .flat_map(|f| f.locals.iter().map(|(n, a, off)| (n.as_str(), *a, *off)));
    let prefix = l.arch.mangle("");
    let p = crate::trace::Plan::new(crate::trace::globals(&l.syms).chain(locals), &prefix, canon);
    (!p.is_empty()).then_some((p, anchor))
}

pub fn write(l: &mut Link, o: &Options) -> Result<(), String> {
    let idata = import_tables(l);
    let dll = o
        .out
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let edata = export_table(l, &dll);
    // The debug directory: `/debug`'s PDB (a `CodeView` record naming
    // it), `/Brepro`'s entry saying the timestamp is a hash.
    let pdb_path = o.debug.then(|| {
        let p = o.out.with_extension("pdb");
        std::path::absolute(&p).unwrap_or(p)
    });
    let ndebug = usize::from(o.debug) + usize::from(o.repro);
    let debug_dir = (ndebug > 0).then(|| {
        let a = l.synth(".rdata", RDATA, 2, vec![0; 28 * ndebug], 0);
        l.atoms[a as usize].live = true;
        a
    });
    let cv = pdb_path.as_ref().map(|p| {
        let mut d = b"RSDS".to_vec();
        d.resize(20, 0);
        d.extend_from_slice(&1u32.to_le_bytes());
        d.extend_from_slice(p.to_string_lossy().as_bytes());
        d.push(0);
        let a = l.synth(".rdata", RDATA, 2, d, 0);
        l.atoms[a as usize].live = true;
        a
    });
    let trace = trace_plan(l).map(|(p, anchor)| {
        let a = l.synth(".rdata", RDATA, 2, vec![0; p.size() as usize], 0);
        l.atoms[a as usize].live = true;
        (p, anchor, a)
    });
    // Relocations into discarded sections.
    for a in l.atoms.iter().filter(|a| a.live) {
        if a.relocs.iter().any(|r| r.target == Target::Atom(DEAD)) {
            return Err(format!(
                "{}: relocation against a symbol in a discarded section",
                l.file_name(a.file)
            ));
        }
    }
    let mut secs = sections(l, o, idata.as_ref());
    let mut based_relocs: Vec<(AtomId, u32, u16)> = Vec::new();
    for s in &secs {
        for &a in &s.atoms {
            for r in &l.atoms[a as usize].relocs {
                if let Some(t) = based(l, r) {
                    based_relocs.push((a, r.off, t));
                }
            }
        }
    }
    let has_reloc = !based_relocs.is_empty() && o.dynamic_base;
    let nsects = secs.len() + usize::from(has_reloc);
    let is64 = l.arch.is64();
    let opt_size: u64 = if is64 { 240 } else { 224 };
    let headers_end = DOS_STUB + 4 + 20 + opt_size + 40 * nsects as u64;
    let size_of_headers = headers_end.next_multiple_of(FILE_ALIGN);
    let mut rva = size_of_headers.next_multiple_of(SECTION_ALIGN);
    let mut file_off = size_of_headers;
    for (k, s) in secs.iter_mut().enumerate() {
        s.rva = rva;
        let mut at = 0u64;
        let mut init = 0u64;
        for &a in &s.atoms {
            let atom = &mut l.atoms[a as usize];
            at = at.next_multiple_of(1 << atom.align);
            atom.addr = rva + at;
            atom.out = k as u32;
            at += atom.size;
            let f = &l.files[atom.file as usize];
            if !f.sects[atom.sect as usize].zerofill() {
                init = at;
            }
        }
        s.size = at;
        s.init = init;
        s.file_off = if init > 0 { file_off } else { 0 };
        file_off += init.next_multiple_of(FILE_ALIGN);
        rva += at.max(1).next_multiple_of(SECTION_ALIGN);
    }
    // A folded atom is at its copy.
    for &(dup, keep) in &l.folded {
        let k = &l.atoms[keep as usize];
        let (addr, out) = (k.addr, k.out);
        let d = &mut l.atoms[dup as usize];
        d.addr = addr;
        d.out = out;
    }
    // Base relocations, now that addresses are known.
    let reloc_data = if has_reloc {
        base_relocs(
            based_relocs
                .iter()
                .map(|&(a, off, t)| (l.atoms[a as usize].addr + u64::from(off), t))
                .collect(),
        )
    } else {
        Vec::new()
    };
    if has_reloc {
        let size = reloc_data.len() as u64;
        secs.push(OutSec {
            name: ".reloc".to_owned(),
            chars: RDATA | SCN_MEM_DISCARDABLE,
            rva,
            size,
            init: size,
            file_off,
            atoms: Vec::new(),
        });
        file_off += size.next_multiple_of(FILE_ALIGN);
        rva += size.next_multiple_of(SECTION_ALIGN);
    }
    let size_of_image = rva;
    let mut buf = vec![0u8; file_off as usize];

    // Contents and relocations.
    let mut errors = Vec::new();
    for s in &secs {
        for &a in &s.atoms {
            let atom = &l.atoms[a as usize];
            let bytes = l.bytes(a);
            if bytes.is_empty() {
                continue;
            }
            let at = (s.file_off + atom.addr - s.rva) as usize;
            let mut b = bytes.to_vec();
            for r in &atom.relocs {
                if let Err(e) = apply(l, &mut b, r, atom.addr, &secs) {
                    if errors.len() < 20 {
                        errors.push(format!("{}: {e}", l.file_name(atom.file)));
                    }
                }
            }
            buf[at..at + b.len()].copy_from_slice(&b);
        }
    }
    if !errors.is_empty() {
        return Err(errors.join("\n"));
    }
    if has_reloc {
        let s = secs.last().unwrap();
        let at = s.file_off as usize;
        buf[at..at + reloc_data.len()].copy_from_slice(&reloc_data);
    }
    for (a, t) in &o.anchors {
        let addr = |n: &str| match l.syms.find(n).map(|id| l.syms.get(id).def) {
            Some(Def::Defined { atom, off }) => Ok(l.atoms[atom as usize].addr + off),
            _ => Err(format!("/nabianchor: {n} is not defined")),
        };
        let (at, to) = (addr(a)?, addr(t)?);
        let width = if is64 { 8 } else { 4 };
        let s = secs
            .iter()
            .find(|s| at >= s.rva && at + width <= s.rva + s.init)
            .ok_or_else(|| format!("/nabianchor: {a} is not in initialized data"))?;
        let pos = (s.file_off + at - s.rva) as usize;
        let b = crate::trace::anchor(to, at, width as usize, false);
        buf[pos..pos + width as usize].copy_from_slice(&b);
    }
    if let Some((p, (atom, off), table)) = &trace {
        let pos = |rva: u64, n: u64| {
            secs.iter()
                .find(|s| rva >= s.rva && rva + n <= s.rva + s.init)
                .map(|s| (s.file_off + rva - s.rva) as usize)
        };
        let at = l.atoms[*table as usize].addr;
        let t = p.table(at, false, |a| {
            (l.atoms[a as usize].addr, l.atoms[a as usize].size)
        });
        let width = if is64 { 8 } else { 4 };
        let anchor = l.atoms[*atom as usize].addr + off;
        if let (Some(tp), Some(ap)) = (pos(at, t.len() as u64), pos(anchor, width as u64)) {
            buf[tp..tp + t.len()].copy_from_slice(&t);
            buf[ap..ap + width].copy_from_slice(&crate::trace::anchor(at, anchor, width, false));
        }
    }

    // The exception table, sorted by function (x86 has none).
    let pdata = secs.iter().find(|s| s.name == ".pdata" && is64);
    if let Some(s) = pdata {
        let e = if l.arch == Arch::X86_64 { 12 } else { 8 };
        if s.size % e != 0 {
            return Err(format!("unexpected .pdata size {}", s.size));
        }
        let at = s.file_off as usize;
        let mut ents: Vec<Vec<u8>> = buf[at..at + s.size as usize]
            .chunks(e as usize)
            .map(<[u8]>::to_vec)
            .collect();
        ents.sort_by_key(|x| u32le(x, 0));
        buf[at..at + s.size as usize].copy_from_slice(&ents.concat());
    }

    let rva_of = |l: &Link, n: &str| -> Option<u64> {
        let id = l.syms.find(n)?;
        match l.syms.get(id).def {
            Def::Defined { atom, off } if l.atoms[atom as usize].live => {
                Some(l.atoms[atom as usize].addr + off)
            }
            _ => None,
        }
    };
    // The TLS directory's alignment (its `Characteristics`): the largest
    // of the TLS data.
    let tls_used = l.arch.mangle("_tls_used");
    if let Some(t) = rva_of(l, &tls_used) {
        let align = l
            .atoms
            .iter()
            .enumerate()
            .filter(|(k, a)| a.live && l.sect(*k as AtomId).name.starts_with(".tls"))
            .map(|(_, a)| a.align)
            .max()
            .unwrap_or(0);
        let s = secs
            .iter()
            .find(|s| t >= s.rva && t < s.rva + s.size)
            .unwrap();
        let at = (s.file_off + t - s.rva) as usize + if is64 { 0x24 } else { 0x14 };
        let c = (rd32(&buf, at) & !0x00f0_0000) | ((u32::from(align) + 1) << 20);
        wr32(&mut buf, at, c);
    }

    // Headers.
    let mut h = Vec::with_capacity(size_of_headers as usize);
    let mut dos = [0u8; 64];
    dos[0] = b'M';
    dos[1] = b'Z';
    dos[2..4].copy_from_slice(&((DOS_STUB % 512) as u16).to_le_bytes());
    dos[4..6].copy_from_slice(&(DOS_STUB.div_ceil(512) as u16).to_le_bytes());
    dos[8..10].copy_from_slice(&4u16.to_le_bytes());
    dos[0x18..0x1a].copy_from_slice(&64u16.to_le_bytes());
    dos[0x3c..0x40].copy_from_slice(&(DOS_STUB as u32).to_le_bytes());
    h.extend_from_slice(&dos);
    h.extend_from_slice(&DOS_PROGRAM);
    h.extend_from_slice(b"PE\0\0");
    h.extend_from_slice(&l.arch.machine().to_le_bytes());
    h.extend_from_slice(&(secs.len() as u16).to_le_bytes());
    let stamp_at = h.len();
    put32(&mut h, 0);
    put32(&mut h, 0);
    put32(&mut h, 0);
    h.extend_from_slice(&(opt_size as u16).to_le_bytes());
    // Executable; large address aware; 32-bit.
    let mut chars: u16 = 0x0002;
    if o.dll {
        chars |= 0x2000;
    }
    if o.large_address_aware {
        chars |= 0x0020;
    }
    if !is64 {
        chars |= 0x0100;
    }
    if !has_reloc {
        chars |= 0x0001;
    }
    h.extend_from_slice(&chars.to_le_bytes());
    // The optional header.
    let text = secs.iter().find(|s| s.name == ".text");
    let init_data: u64 = secs
        .iter()
        .filter(|s| s.chars & SCN_CNT_INITIALIZED_DATA != 0)
        .map(|s| s.init.next_multiple_of(FILE_ALIGN))
        .sum();
    h.extend_from_slice(&(if is64 { 0x20bu16 } else { 0x10b }).to_le_bytes());
    h.extend_from_slice(&[14, 0]);
    put32(
        &mut h,
        text.map_or(0, |t| t.init.next_multiple_of(FILE_ALIGN)) as u32,
    );
    put32(&mut h, init_data as u32);
    put32(&mut h, 0);
    let entry =
        rva_of(l, &o.entry).ok_or_else(|| format!("entry point {} is not defined", o.entry))?;
    put32(&mut h, entry as u32);
    put32(&mut h, text.map_or(0, |t| t.rva) as u32);
    let base = image_base(l.arch);
    if is64 {
        h.extend_from_slice(&base.to_le_bytes());
    } else {
        // `BaseOfData`: the first section after the code.
        let data = secs
            .iter()
            .find(|s| s.chars & SCN_CNT_CODE == 0)
            .map_or(0, |s| s.rva);
        put32(&mut h, data as u32);
        put32(&mut h, base as u32);
    }
    put32(&mut h, SECTION_ALIGN as u32);
    put32(&mut h, FILE_ALIGN as u32);
    // OS 6.0, image 0.0, subsystem 6.0 (arm64: 6.2, its first).
    let sub_minor: u16 = if l.arch == Arch::Aarch64 { 2 } else { 0 };
    for v in [6u16, 0, 0, 0, 6, sub_minor] {
        h.extend_from_slice(&v.to_le_bytes());
    }
    put32(&mut h, 0);
    put32(&mut h, size_of_image as u32);
    put32(&mut h, size_of_headers as u32);
    put32(&mut h, 0);
    h.extend_from_slice(&o.subsystem.to_le_bytes());
    let mut dllc: u16 = 0x8000; // terminal server aware
    if o.dynamic_base {
        dllc |= 0x0040;
        if o.high_entropy_va {
            dllc |= 0x0020;
        }
    }
    if o.nx_compat {
        dllc |= 0x0100;
    }
    h.extend_from_slice(&dllc.to_le_bytes());
    // Stack and heap, reserve and commit.
    for v in [o.stack_reserve, o.stack_commit, 0x10_0000, 0x1000] {
        if is64 {
            h.extend_from_slice(&v.to_le_bytes());
        } else {
            put32(&mut h, v as u32);
        }
    }
    put32(&mut h, 0);
    put32(&mut h, 16);
    // Data directories.
    let mut dirs = [(0u32, 0u32); 16];
    if let Some(e) = edata {
        let a = &l.atoms[e as usize];
        dirs[0] = (a.addr as u32, a.size as u32);
    }
    let span = |atoms: &[AtomId]| -> (u32, u32) {
        let first = &l.atoms[atoms[0] as usize];
        let last = &l.atoms[atoms[atoms.len() - 1] as usize];
        (
            first.addr as u32,
            (last.addr + last.size - first.addr) as u32,
        )
    };
    if let Some((d, null, _, iat, _)) = &idata {
        let mut all = d.clone();
        all.push(*null);
        dirs[1] = span(&all);
        dirs[12] = span(iat);
    }
    if let Some(s) = pdata {
        dirs[3] = (s.rva as u32, s.size as u32);
    }
    if has_reloc {
        let s = secs.last().unwrap();
        dirs[5] = (s.rva as u32, s.size as u32);
    }
    if let Some(d) = debug_dir {
        dirs[6] = (l.atoms[d as usize].addr as u32, 28 * ndebug as u32);
    }
    if let Some(t) = rva_of(l, &tls_used) {
        dirs[9] = (t as u32, if is64 { 40 } else { 24 });
    }
    if let Some(c) = rva_of(l, &l.arch.mangle("_load_config_used")) {
        let s = secs
            .iter()
            .find(|s| c >= s.rva && c < s.rva + s.size)
            .unwrap();
        let size = rd32(&buf, (s.file_off + c - s.rva) as usize);
        dirs[10] = (c as u32, size);
    }
    for (r, s) in dirs {
        put32(&mut h, r);
        put32(&mut h, s);
    }
    for s in &secs {
        let mut name = [0u8; 8];
        let n = s.name.as_bytes();
        name[..n.len().min(8)].copy_from_slice(&n[..n.len().min(8)]);
        h.extend_from_slice(&name);
        put32(&mut h, s.size as u32);
        put32(&mut h, s.rva as u32);
        put32(&mut h, s.init.next_multiple_of(FILE_ALIGN) as u32);
        put32(&mut h, s.file_off as u32);
        put32(&mut h, 0);
        put32(&mut h, 0);
        put32(&mut h, 0);
        put32(&mut h, s.chars);
    }
    debug_assert!(h.len() as u64 <= size_of_headers);
    buf[..h.len()].copy_from_slice(&h);
    let file_at = |a: AtomId| {
        let at = &l.atoms[a as usize];
        let s = &secs[at.out as usize];
        (s.file_off + at.addr - s.rva) as usize
    };
    if let Some(d) = debug_dir {
        let at = file_at(d);
        let mut e = 0;
        if let Some(c) = cv {
            let a = &l.atoms[c as usize];
            wr32(&mut buf, at + 12, 2);
            wr32(&mut buf, at + 16, a.size as u32);
            wr32(&mut buf, at + 20, a.addr as u32);
            wr32(&mut buf, at + 24, file_at(c) as u32);
            e += 28;
        }
        if o.repro {
            wr32(&mut buf, at + e + 12, 16);
        }
    }
    // The timestamp: a hash of the image under `/Brepro`, else the time;
    // the PDB's GUID from the same hash.
    let h = nabic_core::hash::sha256(&buf);
    let stamp = if o.repro {
        u32::from_le_bytes([h[0], h[1], h[2], h[3]])
    } else {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as u32)
    };
    let mut guid = [0u8; 16];
    guid.copy_from_slice(&h[..16]);
    if !o.repro {
        guid[..4].copy_from_slice(&stamp.to_le_bytes());
    }
    wr32(&mut buf, stamp_at, stamp);
    if let Some(d) = debug_dir {
        let at = file_at(d);
        for k in 0..ndebug {
            wr32(&mut buf, at + 28 * k + 4, stamp);
        }
    }
    if let Some(c) = cv {
        let at = file_at(c) + 4;
        buf[at..at + 16].copy_from_slice(&guid);
    }
    if let Some(p) = &pdb_path {
        let first = (DOS_STUB + 4 + 20 + opt_size) as usize;
        let headers = &buf[first..first + 40 * secs.len()];
        let publics = publics(l, &secs);
        let mut strings = crate::coff::pdb::Strings::default();
        let (mut modules, types, globals) = crate::coff::cv::collect(l, &secs, &mut strings);
        modules.push(crate::coff::pdb::Module {
            name: "* Linker *".to_owned(),
            contribs: crate::coff::cv::contribs(l, 0, &secs),
            ..crate::coff::pdb::Module::default()
        });
        let debug = crate::coff::pdb::Debug {
            modules,
            tpi: types.tpi,
            ipi: types.ipi,
            globals,
            strings,
        };
        let id = crate::coff::pdb::Id {
            machine: l.arch.machine(),
            signature: stamp,
            guid,
        };
        crate::coff::pdb::write(p, &id, headers, publics, debug)?;
    }
    if let Some(p) = o.implib.as_ref().filter(|_| o.dll) {
        let mut def = format!("LIBRARY {dll}\nEXPORTS\n");
        for e in &o.exports {
            def.push_str(&format!("    {e}\n"));
        }
        let lib = crate::coff::import::import_library(l.arch.machine(), &def)?;
        let _ = std::fs::remove_file(p);
        std::fs::write(p, lib).map_err(|e| format!("cannot write {}: {e}", p.display()))?;
    }
    let _ = std::fs::remove_file(&o.out);
    std::fs::write(&o.out, &buf).map_err(|e| format!("cannot write {}: {e}", o.out.display()))
}
