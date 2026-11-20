//! ELF relocatable objects (ELF64 RELA, ELF32 REL or RELA) into atoms.
//!
//! One atom per section, per string of `SHF_MERGE | SHF_STRINGS`
//! sections, per CIE / FDE record of `.eh_frame`. COMDAT groups after the
//! first of a signature are discarded.

use crate::bytes::{Endian, cstr, u16le, u32le};
use crate::elf::arch::i386;
use crate::elf::{DEAD, Link};
use crate::symtab::{Atom, AtomId, Def, Fetch, Reloc, SymId, Target};
use nabic_core::HashMap;

pub const SHT_PROGBITS: u32 = 1;
pub const SHT_SYMTAB: u32 = 2;
pub const SHT_STRTAB: u32 = 3;
pub const SHT_RELA: u32 = 4;
pub const SHT_NOTE: u32 = 7;
pub const SHT_NOBITS: u32 = 8;
pub const SHT_REL: u32 = 9;
pub const SHT_INIT_ARRAY: u32 = 14;
pub const SHT_FINI_ARRAY: u32 = 15;
pub const SHT_PREINIT_ARRAY: u32 = 16;
pub const SHT_GROUP: u32 = 17;
pub const SHT_SYMTAB_SHNDX: u32 = 18;
pub const SHT_X86_64_UNWIND: u32 = 0x7000_0001;
/// 32-bit ARM's exception index table (the same number).
pub const SHT_ARM_EXIDX: u32 = 0x7000_0001;
const SHT_ARM_ATTRIBUTES: u32 = 0x7000_0003;
const SHT_LLVM_ADDRSIG: u32 = 0x6fff_4c03;
const SHT_LLVM_CALL_GRAPH_PROFILE: u32 = 0x6fff_4c09;

pub const SHF_WRITE: u64 = 0x1;
pub const SHF_ALLOC: u64 = 0x2;
pub const SHF_EXECINSTR: u64 = 0x4;
pub const SHF_MERGE: u64 = 0x10;
pub const SHF_STRINGS: u64 = 0x20;
pub const SHF_INFO_LINK: u64 = 0x40;
pub const SHF_GROUP: u64 = 0x200;
pub const SHF_TLS: u64 = 0x400;
const SHF_COMPRESSED: u64 = 0x800;
const SHF_GNU_RETAIN: u64 = 0x20_0000;
const SHF_EXCLUDE: u64 = 0x8000_0000;

pub const STB_LOCAL: u8 = 0;
pub const STB_GLOBAL: u8 = 1;
pub const STB_WEAK: u8 = 2;
pub const STT_NOTYPE: u8 = 0;
pub const STT_OBJECT: u8 = 1;
pub const STT_FUNC: u8 = 2;
pub const STT_SECTION: u8 = 3;
pub const STT_FILE: u8 = 4;
pub const STT_TLS: u8 = 6;
pub const STT_GNU_IFUNC: u8 = 10;

const SHN_UNDEF: u16 = 0;
const SHN_ABS: u16 = 0xfff1;
const SHN_COMMON: u16 = 0xfff2;
const SHN_XINDEX: u16 = 0xffff;
const GRP_COMDAT: u32 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SectKind {
    Skip,
    Atom,
    /// `SHF_MERGE | SHF_STRINGS`: an atom per string.
    Pieces,
    EhFrame,
}

#[derive(Debug)]
pub struct InSect {
    pub name: String,
    pub ty: u32,
    pub flags: u64,
    pub align: u64,
    pub off: u64,
    pub size: u64,
    pub entsize: u64,
    pub link: u32,
    pub info: u32,
    pub kind: SectKind,
    pub atoms: Vec<AtomId>,
    /// Section offset of each atom.
    pub atom_offs: Vec<u64>,
}

impl InSect {
    #[must_use]
    pub const fn alloc(&self) -> bool {
        self.flags & SHF_ALLOC != 0
    }

    #[must_use]
    pub const fn tls(&self) -> bool {
        self.flags & SHF_TLS != 0
    }

    #[must_use]
    pub const fn nobits(&self) -> bool {
        self.ty == SHT_NOBITS
    }

    /// Never garbage collected, as lld keeps it.
    #[must_use]
    pub fn keep(&self) -> bool {
        let n = self.name.as_str();
        matches!(
            self.ty,
            SHT_INIT_ARRAY | SHT_FINI_ARRAY | SHT_PREINIT_ARRAY | SHT_NOTE
        ) || self.flags & SHF_GNU_RETAIN != 0
            || matches!(n, ".init" | ".fini" | ".jcr")
            || n.starts_with(".ctors")
            || n.starts_with(".dtors")
            || n.starts_with(".init_array")
            || n.starts_with(".fini_array")
            || n.starts_with(".preinit_array")
    }

    /// The atom holding section offset `off`, and the offset in it.
    #[must_use]
    pub fn locate(&self, off: u64) -> Option<(AtomId, u64)> {
        if self.atoms.is_empty() {
            return None;
        }
        let k = match self.atom_offs.binary_search(&off) {
            Ok(k) => k,
            Err(0) => return None,
            Err(k) => k - 1,
        };
        Some((self.atoms[k], off - self.atom_offs[k]))
    }
}

/// A named local symbol, for the output symbol table.
#[derive(Debug)]
pub struct LocalSym {
    /// Offset of the name in the file's data.
    pub name: usize,
    pub atom: AtomId,
    pub off: u64,
    pub ty: u8,
    pub size: u64,
    /// `st_other` (POWER: the local entry point).
    pub other: u8,
}

pub struct ObjFile {
    /// For messages: the path, or `archive(member)`.
    pub name: String,
    pub data: Vec<u8>,
    pub sects: Vec<InSect>,
    pub locals: Vec<LocalSym>,
}

/// The output section of input section `name`, as lld names it.
#[must_use]
pub fn output_name(name: &str) -> &str {
    const PREFIXES: &[&str] = &[
        ".text.",
        ".rodata.",
        ".data.rel.ro.",
        ".data.",
        ".bss.rel.ro.",
        ".bss.",
        ".gcc_except_table.",
        ".init_array.",
        ".fini_array.",
        ".tbss.",
        ".tdata.",
        ".ctors.",
        ".dtors.",
        ".ldata.",
        ".lrodata.",
        ".lbss.",
        ".ARM.exidx.",
        ".ARM.extab.",
        ".sdata.",
        ".sbss.",
        ".srodata.",
    ];
    // `.data.rel.ro` itself is not `.data`.
    if name == ".data.rel.ro" {
        return name;
    }
    for p in PREFIXES {
        if name.starts_with(p) {
            let base = &p[..p.len() - 1];
            return if base == ".bss.rel.ro" {
                ".data.rel.ro"
            } else {
                base
            };
        }
    }
    if name == ".bss.rel.ro" {
        return ".data.rel.ro";
    }
    name
}

/// A symbol table entry's meaning for relocations.
#[derive(Clone, Copy, Debug)]
enum SymRef {
    Null,
    Global(SymId),
    /// Section, value, a section symbol.
    Local(u32, u64, bool),
    Abs(u64),
    Dead,
}

/// The field layout of an ELF class, and the file's byte order.
#[derive(Clone, Copy, Debug)]
pub struct Class {
    pub wide: bool,
    pub e: Endian,
}

/// A symbol table entry.
#[derive(Clone, Copy, Debug)]
pub struct Sym {
    pub name: u32,
    pub info: u8,
    pub other: u8,
    pub shndx: u16,
    pub value: u64,
    pub size: u64,
}

impl Class {
    /// The class of ELF file `d`.
    #[must_use]
    pub fn of(d: &[u8]) -> Self {
        Self {
            wide: d.get(4) != Some(&1),
            e: Endian {
                big: d.get(5) == Some(&2),
            },
        }
    }

    /// An address-sized field.
    #[must_use]
    pub fn word(self, d: &[u8], at: usize) -> u64 {
        if self.wide {
            self.e.u64(d, at)
        } else {
            u64::from(self.e.u32(d, at))
        }
    }

    #[must_use]
    pub const fn word_size(self) -> usize {
        if self.wide { 8 } else { 4 }
    }

    #[must_use]
    pub const fn sym_size(self) -> usize {
        if self.wide { 24 } else { 16 }
    }

    #[must_use]
    pub const fn shdr_size(self) -> usize {
        if self.wide { 64 } else { 40 }
    }

    /// `.dynamic` entry size.
    #[must_use]
    pub const fn dyn_size(self) -> usize {
        if self.wide { 16 } else { 8 }
    }

    /// Entry `i` of the symbol table at `off`; past the file, zeros.
    #[must_use]
    pub fn sym(self, d: &[u8], off: usize, i: usize) -> Sym {
        let byte = |d: &[u8], at: usize| d.get(at).copied().unwrap_or(0);
        let e = i
            .saturating_mul(self.sym_size())
            .saturating_add(off)
            .min(d.len());
        if self.wide {
            Sym {
                name: self.e.u32(d, e),
                info: byte(d, e + 4),
                other: byte(d, e + 5),
                shndx: self.e.u16(d, e + 6),
                value: self.e.u64(d, e + 8),
                size: self.e.u64(d, e + 16),
            }
        } else {
            Sym {
                name: self.e.u32(d, e),
                value: u64::from(self.e.u32(d, e + 4)),
                size: u64::from(self.e.u32(d, e + 8)),
                info: byte(d, e + 12),
                other: byte(d, e + 13),
                shndx: self.e.u16(d, e + 14),
            }
        }
    }

    /// `(e_shoff, e_shentsize, e_shnum, e_shstrndx)`.
    #[must_use]
    pub fn sh_table(self, d: &[u8]) -> (usize, usize, usize, usize) {
        if self.wide {
            (
                self.e.u64(d, 0x28) as usize,
                self.e.u16(d, 0x3a) as usize,
                self.e.u16(d, 0x3c) as usize,
                self.e.u16(d, 0x3e) as usize,
            )
        } else {
            (
                self.e.u32(d, 0x20) as usize,
                self.e.u16(d, 0x2e) as usize,
                self.e.u16(d, 0x30) as usize,
                self.e.u16(d, 0x32) as usize,
            )
        }
    }
}

pub struct Shdr {
    name: u32,
    ty: u32,
    flags: u64,
    off: u64,
    size: u64,
    link: u32,
    info: u32,
    align: u64,
    entsize: u64,
}

/// The section headers and the section name table's index.
pub fn shdrs(d: &[u8]) -> Result<(Vec<Shdr>, usize), String> {
    let c = Class::of(d);
    let (shoff, entsize, mut shnum, mut shstrndx) = c.sh_table(d);
    let size = c.shdr_size();
    if shoff == 0 {
        return Ok((Vec::new(), 0));
    }
    if entsize != size || shoff.checked_add(size).is_none_or(|e| e > d.len()) {
        return Err("bad section header table".to_owned());
    }
    // Field offsets: flags, offset, size, link, info, alignment, entry
    // size.
    let f: [usize; 7] = if c.wide {
        [8, 24, 32, 40, 44, 48, 56]
    } else {
        [8, 16, 20, 24, 28, 32, 36]
    };
    if shnum == 0 {
        shnum = c.word(d, shoff + f[2]) as usize;
    }
    if shstrndx == usize::from(SHN_XINDEX) {
        shstrndx = c.e.u32(d, shoff + f[3]) as usize;
    }
    if shnum
        .checked_mul(size)
        .and_then(|n| n.checked_add(shoff))
        .is_none_or(|e| e > d.len())
    {
        return Err("section header table runs past the end".to_owned());
    }
    let v: Vec<Shdr> = (0..shnum)
        .map(|i| {
            let h = shoff + i * size;
            Shdr {
                name: c.e.u32(d, h),
                ty: c.e.u32(d, h + 4),
                flags: c.word(d, h + f[0]),
                off: c.word(d, h + f[1]),
                size: c.word(d, h + f[2]),
                link: c.e.u32(d, h + f[3]),
                info: c.e.u32(d, h + f[4]),
                align: c.word(d, h + f[5]).max(1),
                entsize: c.word(d, h + f[6]),
            }
        })
        .collect::<Vec<Shdr>>();
    // Every section with contents lies in the file (`SHT_NOBITS` has none).
    if let Some(i) = v.iter().position(|h| {
        h.ty != SHT_NOBITS
            && h.ty != 0
            && h.off.checked_add(h.size).is_none_or(|e| e > d.len() as u64)
    }) {
        return Err(format!("section {i} runs past the end"));
    }
    Ok((v, shstrndx))
}

impl Shdr {
    #[must_use]
    pub const fn ty(&self) -> u32 {
        self.ty
    }

    /// File offset, size, link and info.
    #[must_use]
    pub const fn place(&self) -> (usize, usize, u32, u32) {
        (self.off as usize, self.size as usize, self.link, self.info)
    }
}

/// The ELF header of a file for `arch`, in its byte order.
pub fn check_header(d: &[u8], arch: crate::elf::Arch) -> Result<(), String> {
    let (machine, wide) = (arch.machine(), arch.elf64());
    if d.len() < if wide { 64 } else { 52 } || &d[..4] != b"\x7fELF" {
        return Err("not an ELF file".to_owned());
    }
    let e = arch.endian();
    if d[5] != if e.big { 2 } else { 1 } {
        let m = Class::of(d).e.u16(d, 18);
        if m == machine {
            return Err(format!(
                "is a {}-endian ELF file, not {}-endian",
                if e.big { "little" } else { "big" },
                if e.big { "big" } else { "little" }
            ));
        }
        return Err(format!(
            "is for machine {m}, not {machine} (a different architecture)"
        ));
    }
    if d[4] != if wide { 2 } else { 1 } {
        let m = e.u16(d, 18);
        if m == machine {
            return Err(format!(
                "is ELF{}, not ELF{}",
                if wide { 32 } else { 64 },
                if wide { 64 } else { 32 }
            ));
        }
        return Err(format!(
            "is for machine {m}, not {machine} (a different architecture)"
        ));
    }
    let m = e.u16(d, 18);
    if m != machine {
        return Err(format!(
            "is for machine {m}, not {machine} (a different architecture)"
        ));
    }
    Ok(())
}

/// Read object `data` as file `fi` into `l`; the archive members its
/// references fetch.
#[allow(clippy::too_many_lines)]
pub fn parse(l: &mut Link, fi: u32, name: String, data: Vec<u8>) -> Result<Vec<Fetch>, String> {
    check_header(&data, l.arch).map_err(|e| format!("{name}: {e}"))?;
    if l.arch.riscv() {
        let at = if l.arch.elf64() { 48 } else { 36 };
        let f = u32le(&data, at);
        l.eflags = Some(
            crate::elf::arch::riscv::merge_flags(l.eflags, f)
                .map_err(|e| format!("{name}: {e}"))?,
        );
    }
    if l.arch == crate::elf::Arch::Ppc64 {
        crate::elf::arch::ppc64::check_flags(u32le(&data, 48))
            .map_err(|e| format!("{name}: {e}"))?;
    }
    let d = &data;
    let c = Class::of(d);
    let esz = c.sym_size();
    let (hs, shstrndx) = shdrs(d).map_err(|e| format!("{name}: {e}"))?;
    let shstr = hs.get(shstrndx).map_or(0, |h| h.off as usize);
    let symtab = hs.iter().position(|h| h.ty == SHT_SYMTAB);
    let (sym_off, nsyms, strtab) = symtab.map_or((0, 0, 0), |i| {
        let h = &hs[i];
        let strtab = hs.get(h.link as usize).map_or(0, |s| s.off as usize);
        (h.off as usize, h.size as usize / esz, strtab)
    });
    if nsyms
        .checked_mul(esz)
        .and_then(|n| n.checked_add(sym_off))
        .is_none_or(|e| e > d.len())
    {
        return Err(format!("{name}: the symbol table runs past the end"));
    }
    let xindex = hs
        .iter()
        .find(|h| h.ty == SHT_SYMTAB_SHNDX)
        .map(|h| h.off as usize);
    // A section index, or a reserved `SHN_*` (as `Err`).
    let sym_shndx = |i: usize| -> Result<u32, u16> {
        let s = c.sym(d, sym_off, i).shndx;
        if s == SHN_XINDEX {
            Ok(xindex.map_or(0, |x| c.e.u32(d, x + i * 4)))
        } else if s >= 0xff00 || s == SHN_UNDEF {
            Err(s)
        } else {
            Ok(u32::from(s))
        }
    };
    let sym_name = |i: usize| cstr(d, strtab + c.sym(d, sym_off, i).name as usize);

    // COMDAT groups: a signature seen before discards its members.
    let mut discarded = vec![false; hs.len()];
    for h in hs.iter().filter(|h| h.ty == SHT_GROUP) {
        let off = h.off as usize;
        if c.e.u32(d, off) & GRP_COMDAT == 0 {
            continue;
        }
        let sig = sym_name(h.info as usize).to_owned();
        if !l.comdats.insert(sig) {
            for k in 1..(h.size / 4) as usize {
                let m = c.e.u32(d, off + k * 4) as usize;
                if m < discarded.len() {
                    discarded[m] = true;
                }
            }
        }
    }

    // Compressed debug info (FreeBSD's crt objects) is not inflated: the
    // file's debug sections all go, so none refers into a missing one.
    if let Some(h) = hs
        .iter()
        .find(|h| h.flags & (SHF_COMPRESSED | SHF_ALLOC) == SHF_COMPRESSED | SHF_ALLOC)
    {
        return Err(format!(
            "{name}: {}: compressed allocated sections are not supported",
            cstr(d, shstr + h.name as usize)
        ));
    }
    let drop_debug = hs.iter().any(|h| h.flags & SHF_COMPRESSED != 0);

    let mut sects = Vec::with_capacity(hs.len());
    for (i, h) in hs.iter().enumerate() {
        let sname = cstr(d, shstr + h.name as usize).to_owned();
        if h.ty == SHT_REL && c.wide {
            return Err(format!(
                "{name}: {sname}: REL relocations are not supported (RELA only)"
            ));
        }
        let skip = i == 0
            || discarded[i]
            || matches!(
                h.ty,
                0 | SHT_SYMTAB
                    | SHT_RELA
                    | SHT_REL
                    | SHT_GROUP
                    | SHT_SYMTAB_SHNDX
                    | SHT_LLVM_ADDRSIG
                    | SHT_LLVM_CALL_GRAPH_PROFILE
            )
            || (h.ty == SHT_STRTAB && h.flags & SHF_ALLOC == 0)
            || h.flags & SHF_EXCLUDE != 0
            || sname == ".note.GNU-stack"
            || sname == ".note.gnu.property"
            // Build attributes: the output gets its own (RISC-V: merged).
            || ((l.arch == crate::elf::Arch::Arm || l.arch.riscv()) && h.ty == SHT_ARM_ATTRIBUTES)
            || (h.flags & SHF_ALLOC == 0 && (!l.keep_debug || drop_debug))
            || (h.ty != SHT_NOBITS && h.off.saturating_add(h.size) > d.len() as u64);
        if l.arch.riscv()
            && h.ty == SHT_ARM_ATTRIBUTES
            && h.off.saturating_add(h.size) <= d.len() as u64
        {
            l.attributes
                .push(d[h.off as usize..(h.off + h.size) as usize].to_vec());
        }
        let kind = if skip {
            SectKind::Skip
        } else if sname == ".eh_frame" {
            SectKind::EhFrame
        } else if h.flags & (SHF_MERGE | SHF_STRINGS) == SHF_MERGE | SHF_STRINGS
            && h.entsize == 1
            && h.size > 0
            && d[(h.off + h.size - 1) as usize] == 0
        {
            SectKind::Pieces
        } else {
            SectKind::Atom
        };
        sects.push(InSect {
            name: sname,
            ty: h.ty,
            flags: h.flags,
            align: h.align,
            off: h.off,
            size: h.size,
            entsize: h.entsize,
            link: h.link,
            info: h.info,
            kind,
            atoms: Vec::new(),
            atom_offs: Vec::new(),
        });
    }

    // Atoms.
    let mut fde_cies: Vec<(AtomId, AtomId)> = Vec::new();
    for (i, s) in sects.iter_mut().enumerate() {
        let align = s.align.trailing_zeros() as u8;
        let mut add = |s: &mut InSect, off: u64, size: u64, align: u8| {
            let id = l.atoms.len() as AtomId;
            l.atoms.push(Atom::new(fi, i as u32, off, size, align));
            s.atoms.push(id);
            s.atom_offs.push(off);
            id
        };
        match s.kind {
            SectKind::Skip => {}
            SectKind::Atom => {
                add(s, 0, s.size, align);
            }
            SectKind::Pieces => {
                let b = &d[s.off as usize..(s.off + s.size) as usize];
                let mut p = 0;
                while p < b.len() {
                    let e = p + b[p..].iter().position(|&c| c == 0).unwrap_or(b.len() - p);
                    add(s, p as u64, (e + 1 - p) as u64, align);
                    p = e + 1;
                }
            }
            SectKind::EhFrame => {
                let b = &d[s.off as usize..(s.off + s.size) as usize];
                let mut cies: HashMap<usize, AtomId> = HashMap::default();
                let mut p = 0;
                while p + 4 <= b.len() {
                    let len = c.e.u32(b, p) as usize;
                    if len == 0 {
                        break;
                    }
                    if len == 0xffff_ffff {
                        return Err(format!(
                            "{name}: 64-bit .eh_frame records are not supported"
                        ));
                    }
                    let size = 4 + len;
                    if p + size > b.len() {
                        return Err(format!("{name}: .eh_frame record runs past the end"));
                    }
                    let id = c.e.u32(b, p + 4) as usize;
                    let a = add(s, p as u64, size as u64, 0);
                    if id == 0 {
                        cies.insert(p, a);
                    } else {
                        let cie = (p + 4)
                            .checked_sub(id)
                            .and_then(|c| cies.get(&c).copied())
                            .ok_or_else(|| format!("{name}: an FDE without its CIE"))?;
                        fde_cies.push((a, cie));
                    }
                    p += size;
                }
            }
        }
    }
    for &(fde, cie) in &fde_cies {
        l.atoms[fde as usize].keep.push(Target::Atom(cie));
    }
    l.fdes.extend(fde_cies);
    // 32-bit ARM: an index table lives with the code it describes.
    if l.arch == crate::elf::Arch::Arm {
        for s in sects.iter().filter(|s| s.ty == SHT_ARM_EXIDX) {
            let (Some(&x), Some(&code)) = (
                s.atoms.first(),
                sects.get(s.link as usize).and_then(|c| c.atoms.first()),
            ) else {
                continue;
            };
            l.atoms[code as usize].keep.push(Target::Atom(x));
            l.exidx.push((code, x));
        }
    }

    // Symbols.
    let mut fetch = Vec::new();
    let mut refs = vec![SymRef::Null; nsyms];
    let mut locals = Vec::new();
    for (i, r) in refs.iter_mut().enumerate().skip(1) {
        let e = c.sym(d, sym_off, i);
        let (bind, ty) = (e.info >> 4, e.info & 0xf);
        let vis = e.other & 3;
        let shndx = sym_shndx(i);
        let (value, size) = (e.value, e.size);
        let sname = sym_name(i);
        let regular = shndx.is_ok_and(|s| s != 0 && (s as usize) < sects.len());
        let special = shndx.err().unwrap_or(0xff00);
        let shndx = shndx.unwrap_or(0);
        if bind == STB_LOCAL {
            *r = if special == SHN_ABS {
                SymRef::Abs(value)
            } else if !regular {
                SymRef::Null
            } else if sects[shndx as usize].kind == SectKind::Skip {
                SymRef::Dead
            } else {
                SymRef::Local(shndx, value, ty == STT_SECTION)
            };
            // Kept stripped too: identical code folding reads them, and
            // must fold the same with symbols as without.
            if regular
                && matches!(ty, STT_NOTYPE | STT_OBJECT | STT_FUNC | STT_TLS)
                && !sname.is_empty()
                && !sname.starts_with(".L")
                && (!sname.starts_with('$') || arm_mapping(l.arch, sname))
            {
                if let Some((atom, off)) = sects[shndx as usize].locate(value) {
                    locals.push(LocalSym {
                        name: strtab + e.name as usize,
                        atom,
                        off,
                        ty,
                        size,
                        other: e.other,
                    });
                }
            }
            continue;
        }
        let (base, ver) = match sname.split_once('@') {
            Some((b, v)) if special == SHN_UNDEF => (b, Some(v.trim_start_matches('@'))),
            Some((b, _)) => (b, None),
            None => (sname, None),
        };
        let id = l.syms.intern(base);
        *r = SymRef::Global(id);
        let weak = bind == STB_WEAK;
        let hidden = vis == 1 || vis == 2;
        let def = if special == SHN_UNDEF {
            None
        } else if special == SHN_ABS {
            Some(Def::Absolute(value))
        } else if special == SHN_COMMON {
            let align = value.max(1).trailing_zeros() as u8;
            l.syms.common(id, size, align, fi, hidden);
            if l.syms.get(id).file == fi {
                let x = l.ext_mut(id);
                x.ty = STT_OBJECT;
                x.size = size;
            }
            continue;
        } else if regular {
            sects[shndx as usize]
                .locate(value)
                .map(|(atom, off)| Def::Defined { atom, off })
        } else {
            None
        };
        match def {
            None => {
                if let Some(f) = l.syms.reference(id, fi, weak) {
                    fetch.push(f);
                }
                if let Some(v) = ver {
                    l.ext_mut(id).want_version = Some(v.to_owned());
                }
                if hidden {
                    l.syms.get_mut(id).private_extern = true;
                }
            }
            Some(def) => {
                if let Err(prev) = l.syms.define(id, def, fi, weak, hidden) {
                    let prev = if prev == u32::MAX {
                        "the linker".to_owned()
                    } else {
                        l.file_name(prev)
                    };
                    l.errors.push(format!(
                        "duplicate symbol: {base}\n>>> defined at {prev}\n>>> defined at {name}"
                    ));
                }
                if l.syms.get(id).file == fi {
                    l.syms.get_mut(id).tlv = ty == STT_TLS;
                    let x = l.ext_mut(id);
                    x.ty = ty;
                    x.size = size;
                    x.other = e.other;
                }
            }
        }
    }

    // Relocations.
    let mut aliases: HashMap<usize, SymId> = HashMap::default();
    let mut abs: HashMap<u64, SymId> = HashMap::default();
    for h in hs.iter().filter(|h| matches!(h.ty, SHT_RELA | SHT_REL)) {
        let t = h.info as usize;
        if t >= sects.len() || sects[t].kind == SectKind::Skip {
            continue;
        }
        let rela = h.ty == SHT_RELA;
        let ent = match (c.wide, rela) {
            (true, _) => 24,
            (false, true) => 12,
            (false, false) => 8,
        };
        for k in 0..h.size as usize / ent {
            let e = h.off as usize + k * ent;
            let (off, si, ty) = if c.wide {
                let info = c.e.u64(d, e + 8);
                (
                    c.e.u64(d, e),
                    (info >> 32) as usize,
                    (info & 0xffff_ffff) as u32,
                )
            } else {
                let info = c.e.u32(d, e + 4);
                (u64::from(c.e.u32(d, e)), (info >> 8) as usize, info & 0xff)
            };
            // `R_ARM_V4BX` marks a `bx` for ARMv4, nothing to do.
            // RISC-V `RELAX` / `ALIGN`: no relaxation, the padding stays.
            if ty == 0
                || ty == 256
                || (l.arch == crate::elf::Arch::Arm && ty == 40)
                || (l.arch.riscv() && crate::elf::arch::riscv::ignored(ty))
            {
                continue;
            }
            let addend = if rela {
                c.word(d, e + ent - c.word_size()) as i64
            } else {
                let s = &sects[t];
                if s.nobits() || off >= s.size {
                    0
                } else {
                    implicit_addend(l.arch, d, (s.off + off) as usize, ty)
                }
            };
            let addend = if c.wide {
                addend
            } else {
                i64::from(addend as i32)
            };
            if ty > u32::from(u16::MAX) {
                return Err(format!("{name}: relocation type {ty} is not supported"));
            }
            let sref = refs.get(si).copied().unwrap_or(SymRef::Null);
            let (target, addend) = match sref {
                SymRef::Global(id) => (Target::Sym(id), addend),
                SymRef::Dead => (Target::Atom(DEAD), addend),
                SymRef::Null | SymRef::Abs(_) => {
                    let v = if let SymRef::Abs(v) = sref { v } else { 0 };
                    let id = *abs.entry(v).or_insert_with(|| {
                        let id = l.syms.intern(&format!("\0abs.{v:x}"));
                        let _ = l.syms.define(id, Def::Absolute(v), fi, false, true);
                        id
                    });
                    (Target::Sym(id), addend)
                }
                SymRef::Local(sect, value, section) => {
                    let s = &sects[sect as usize];
                    let at = s.locate(value);
                    if let (true, Some((atom, aoff))) =
                        (crate::elf::reloc::needs_symbol(l.arch, ty as u16), at)
                    {
                        let id = *aliases.entry(si).or_insert_with(|| {
                            let id = l.syms.intern(&format!("\0{fi}.{si}"));
                            let _ = l.syms.define(
                                id,
                                Def::Defined { atom, off: aoff },
                                fi,
                                false,
                                true,
                            );
                            l.syms.get_mut(id).tlv = s.tls();
                            let x = l.ext_mut(id);
                            x.local_alias = true;
                            x.ty = if s.tls() { STT_TLS } else { STT_OBJECT };
                            id
                        });
                        (Target::Sym(id), addend)
                    } else {
                        let key = if section {
                            value.wrapping_add(addend as u64)
                        } else {
                            value
                        };
                        // POWER: a local call enters past the TOC setup.
                        let entry = if l.arch == crate::elf::Arch::Ppc64
                            && !section
                            && ty == u32::from(crate::elf::arch::ppc64::REL24)
                        {
                            crate::elf::arch::ppc64::local_entry(c.sym(d, sym_off, si).other) as i64
                        } else {
                            0
                        };
                        match s.locate(key) {
                            Some((atom, aoff)) => {
                                let post = if section { 0 } else { addend + entry };
                                (Target::Atom(atom), aoff as i64 + post)
                            }
                            // Past the end (a section end marker).
                            None if s.kind == SectKind::Atom => {
                                (Target::Atom(s.atoms[0]), value as i64 + addend)
                            }
                            None => (Target::Atom(DEAD), addend),
                        }
                    }
                }
            };
            let Some((atom, at)) = sects[t].locate(off) else {
                continue;
            };
            l.atoms[atom as usize].relocs.push(Reloc {
                off: at as u32,
                kind: ty as u16,
                len: 0,
                pcrel: false,
                target,
                addend,
                sub: None,
            });
        }
    }
    for s in &sects {
        for &a in &s.atoms {
            let rs = &mut l.atoms[a as usize].relocs;
            if rs.windows(2).any(|w| w[0].off > w[1].off) {
                rs.sort_by_key(|r| r.off);
            }
            match l.arch {
                crate::elf::Arch::X86_64 if !l.tls_calls => {
                    crate::elf::arch::x64::drop_tls_get_addr(rs);
                }
                crate::elf::Arch::I386 => crate::elf::arch::i386::drop_tls_get_addr(rs),
                crate::elf::Arch::Ppc64 => crate::elf::arch::ppc64::drop_tls_get_addr(rs),
                crate::elf::Arch::S390x => crate::elf::arch::s390x::drop_tls_get_offset(rs),
                _ => {}
            }
        }
    }
    l.files.push(ObjFile {
        name,
        data,
        sects,
        locals,
    });
    Ok(fetch)
}

/// A 32-bit ARM mapping symbol (`$a`, `$t`, `$d`, maybe `.n` after).
fn arm_mapping(arch: crate::elf::Arch, name: &str) -> bool {
    arch == crate::elf::Arch::Arm && matches!(name.split('.').next(), Some("$a" | "$t" | "$d"))
}

/// The addend a REL relocation of type `ty` keeps at `d[at..]`.
fn implicit_addend(arch: crate::elf::Arch, d: &[u8], at: usize, ty: u32) -> i64 {
    let Ok(ty) = u16::try_from(ty) else {
        return 0;
    };
    if arch == crate::elf::Arch::Arm {
        return crate::elf::arch::arm32::implicit_addend(d, at, ty);
    }
    match ty {
        i386::R_8 | i386::PC8 => i64::from(d.get(at).copied().unwrap_or(0) as i8),
        i386::R_16 | i386::PC16 if at + 2 <= d.len() => i64::from(u16le(d, at) as i16),
        i386::JUMP_SLOT | i386::R_16 | i386::PC16 => 0,
        _ if at + 4 <= d.len() => i64::from(u32le(d, at) as i32),
        _ => 0,
    }
}

/// The function atom FDE `fde` describes (its `pc_begin` target).
#[must_use]
pub fn fde_function(l: &Link, fde: AtomId) -> Option<AtomId> {
    let r = l.atoms[fde as usize].relocs.iter().find(|r| r.off == 8)?;
    let a = match r.target {
        Target::Atom(a) => a,
        Target::Sym(id) => match l.syms.get(id).def {
            Def::Defined { atom, .. } => atom,
            _ => return None,
        },
    };
    (a != DEAD).then_some(a)
}
