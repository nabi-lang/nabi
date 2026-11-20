//! Layout and output: output sections from live atoms, the dynamic
//! linking sections, GOT / PLT, segments, the symbol table and the
//! section headers.

mod exidx;
mod headers;
mod islands;
mod relocate;
mod symbols;
mod synthetic;

pub(crate) use crate::elf::write::exidx::*;
pub(crate) use crate::elf::write::headers::*;
pub(crate) use crate::elf::write::islands::*;
pub(crate) use crate::elf::write::relocate::*;
pub(crate) use crate::elf::write::symbols::*;
pub(crate) use crate::elf::write::synthetic::*;

use crate::bytes::Endian;
use crate::elf::arch::{arm32, ppc64, riscv, s390x};
use crate::elf::object::{
    self, InSect, ObjFile, SHF_ALLOC, SHF_EXECINSTR, SHF_TLS, SHF_WRITE, SHT_FINI_ARRAY,
    SHT_INIT_ARRAY, SHT_NOBITS, SHT_NOTE, SHT_PREINIT_ARRAY, SHT_PROGBITS, SHT_STRTAB, SHT_SYMTAB,
    STT_FUNC, STT_GNU_IFUNC, STT_TLS,
};
use crate::elf::reloc::{self, Dyn, Site, SymKind, Values};
use crate::elf::{Arch, DEAD, Link, Options};
use crate::symtab::{Atom, AtomId, Def, SymId, Target};
use nabic_core::HashMap;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

const SHT_RELA: u32 = 4;
const SHT_REL: u32 = 9;
const SHT_DYNAMIC: u32 = 6;
const SHT_DYNSYM: u32 = 11;
const SHT_GNU_HASH: u32 = 0x6fff_fff6;
const SHT_GNU_VERNEED: u32 = 0x6fff_fffe;
const SHT_GNU_VERSYM: u32 = 0x6fff_ffff;

const PT_LOAD: u32 = 1;
const PT_DYNAMIC: u32 = 2;
const PT_INTERP: u32 = 3;
const PT_NOTE: u32 = 4;
const PT_PHDR: u32 = 6;
const PT_TLS: u32 = 7;
const PT_GNU_EH_FRAME: u32 = 0x6474_e550;
const PT_GNU_STACK: u32 = 0x6474_e551;
const PT_GNU_RELRO: u32 = 0x6474_e552;

const DT_NEEDED: u64 = 1;
const DT_PLTRELSZ: u64 = 2;
const DT_PLTGOT: u64 = 3;
const DT_STRTAB: u64 = 5;
const DT_SYMTAB: u64 = 6;
const DT_RELA: u64 = 7;
const DT_RELASZ: u64 = 8;
const DT_RELAENT: u64 = 9;
const DT_STRSZ: u64 = 10;
const DT_SYMENT: u64 = 11;
const DT_INIT: u64 = 12;
const DT_FINI: u64 = 13;
const DT_REL: u64 = 17;
const DT_RELSZ: u64 = 18;
const DT_RELENT: u64 = 19;
const DT_PLTREL: u64 = 20;
const DT_DEBUG: u64 = 21;
const DT_JMPREL: u64 = 23;
const DT_INIT_ARRAY: u64 = 25;
const DT_FINI_ARRAY: u64 = 26;
const DT_INIT_ARRAYSZ: u64 = 27;
const DT_FINI_ARRAYSZ: u64 = 28;
const DT_RUNPATH: u64 = 29;
const DT_PREINIT_ARRAY: u64 = 32;
const DT_PREINIT_ARRAYSZ: u64 = 33;
const DT_GNU_HASH: u64 = 0x6fff_fef5;
const DT_VERSYM: u64 = 0x6fff_fff0;
const DT_RELACOUNT: u64 = 0x6fff_fff9;
const DT_RELCOUNT: u64 = 0x6fff_fffa;
const DT_FLAGS_1: u64 = 0x6fff_fffb;
const DT_VERNEED: u64 = 0x6fff_fffe;
const DT_VERNEEDNUM: u64 = 0x6fff_ffff;
const DF_1_PIE: u64 = 0x0800_0000;
const DT_SONAME: u64 = 14;
const DT_FLAGS: u64 = 30;
const DF_STATIC_TLS: u64 = 0x10;

/// Tombstones and image bases.
const COMMON_PAGE: u64 = 0x1000;

/// Tentative definitions become zero-fill atoms of a `.bss` of their
/// own.
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
    if commons.is_empty() {
        return;
    }
    let fi = l.files.len() as u32;
    let mut sect = InSect {
        name: ".bss".to_owned(),
        ty: SHT_NOBITS,
        flags: SHF_ALLOC | SHF_WRITE,
        align: 1,
        off: 0,
        size: 0,
        entsize: 0,
        link: 0,
        info: 0,
        kind: object::SectKind::Atom,
        atoms: Vec::new(),
        atom_offs: Vec::new(),
    };
    for (id, size, align) in commons {
        let a = l.atoms.len() as AtomId;
        l.atoms.push(Atom::new(fi, 0, 0, size, align));
        sect.atoms.push(a);
        sect.atom_offs.push(0);
        l.syms.get_mut(id).def = Def::Defined { atom: a, off: 0 };
    }
    l.files.push(ObjFile {
        name: "<common>".to_owned(),
        data: Vec::new(),
        sects: vec![sect],
        locals: Vec::new(),
    });
}

/// PLT entries jump through lazily bound `.got.plt` slots (i386,
/// s390x), else through GOT slots bound at load.
const fn lazy_plt(arch: Arch) -> bool {
    matches!(arch, Arch::I386 | Arch::S390x)
}

const fn plt_entry(arch: Arch) -> u64 {
    match arch {
        Arch::Ppc64 => ppc64::PLT_ENTRY,
        Arch::S390x => s390x::PLT_ENTRY,
        _ => 16,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Synth {
    Input,
    Interp,
    BuildId,
    Dynsym,
    Versym,
    Verneed,
    GnuHash,
    Dynstr,
    RelaDyn,
    /// i386: `.rel.plt`, the PLT's `JUMP_SLOT`s.
    RelPlt,
    EhFrameHdr,
    /// The function table of panic backtraces (`trace`).
    Trace,
    Plt,
    Got,
    /// i386: `.got.plt`, lazily bound PLT slots.
    GotPlt,
    /// 32-bit ARM: interworking thunks.
    Thunks,
    /// 32-bit ARM: the sorted exception index table, gaps filled.
    Exidx,
    /// 32-bit ARM: the build attributes.
    Attributes,
    Dynamic,
    Symtab,
    Strtab,
    Shstrtab,
}

#[derive(Debug)]
pub(crate) struct OSec {
    name: String,
    ty: u32,
    flags: u64,
    align: u64,
    atoms: Vec<AtomId>,
    size: u64,
    addr: u64,
    off: u64,
    synth: Synth,
    entsize: u64,
    link: u32,
    info: u32,
    /// Section header index.
    shndx: u32,
    /// Order of appearance, for a stable sort.
    first: usize,
}

impl OSec {
    fn new(name: &str, ty: u32, flags: u64, align: u64, synth: Synth) -> Self {
        Self {
            name: name.to_owned(),
            ty,
            flags,
            align,
            atoms: Vec::new(),
            size: 0,
            addr: 0,
            off: 0,
            synth,
            entsize: 0,
            link: 0,
            info: 0,
            shndx: 0,
            first: usize::MAX,
        }
    }

    const fn alloc(&self) -> bool {
        self.flags & SHF_ALLOC != 0
    }

    const fn nobits(&self) -> bool {
        self.ty == SHT_NOBITS
    }

    const fn tls(&self) -> bool {
        self.flags & SHF_TLS != 0
    }

    fn relro(&self) -> bool {
        self.flags & SHF_WRITE != 0
            && (self.tls()
                || matches!(self.ty, SHT_INIT_ARRAY | SHT_FINI_ARRAY | SHT_PREINIT_ARRAY)
                || matches!(self.synth, Synth::Dynamic | Synth::Got)
                || matches!(self.name.as_str(), ".data.rel.ro" | ".jcr" | ".toc")
                || self.name.starts_with(".ctors")
                || self.name.starts_with(".dtors"))
    }

    /// Segment group (read-only, code, RELRO, data, not loaded) and rank.
    fn rank(&self) -> (u8, u8) {
        if !self.alloc() {
            return (
                4,
                match self.synth {
                    Synth::Symtab => 1,
                    Synth::Strtab => 2,
                    Synth::Shstrtab => 3,
                    _ => 0,
                },
            );
        }
        if self.flags & SHF_EXECINSTR != 0 {
            return (
                1,
                match self.name.as_str() {
                    _ if self.synth == Synth::Plt => 4,
                    _ if self.synth == Synth::Thunks => 5,
                    ".text" => 0,
                    ".init" => 1,
                    ".fini" => 2,
                    _ => 3,
                },
            );
        }
        if self.flags & SHF_WRITE == 0 {
            let r = match self.synth {
                Synth::Interp => 0,
                Synth::BuildId => 1,
                _ if self.ty == SHT_NOTE => 2,
                Synth::Dynsym => 3,
                Synth::Versym => 4,
                Synth::Verneed => 5,
                Synth::GnuHash => 6,
                Synth::Dynstr => 7,
                Synth::RelaDyn => 8,
                Synth::RelPlt => 9,
                Synth::EhFrameHdr => 12,
                Synth::Trace => 14,
                _ if self.name == ".rodata" => 10,
                _ if self.name == ".eh_frame" => 13,
                _ => 11,
            };
            return (0, r);
        }
        if self.relro() {
            let r = match self.synth {
                Synth::Dynamic => 7,
                Synth::Got => 8,
                // POWER: `.toc` beside `.got`, in reach of r2.
                _ if self.name == ".toc" => 9,
                _ if self.tls() && !self.nobits() => 0,
                _ if self.tls() => 1,
                _ if self.ty == SHT_PREINIT_ARRAY => 2,
                _ if self.ty == SHT_INIT_ARRAY => 3,
                _ if self.ty == SHT_FINI_ARRAY => 4,
                _ if self.name == ".data.rel.ro" => 5,
                _ => 6,
            };
            return (2, r);
        }
        let r = match self.name.as_str() {
            ".data" => 0,
            _ if self.synth == Synth::GotPlt => 1,
            ".sdata" => 3,
            _ if !self.nobits() => 2,
            ".sbss" => 4,
            ".bss" => 5,
            _ => 6,
        };
        (3, r)
    }
}

/// Output state shared by layout and writing.
pub(crate) struct Out<'a> {
    l: &'a Link,
    o: &'a Options,
    secs: Vec<OSec>,
    /// GOT slots: symbol and kind (`GOT_*`).
    got: Vec<(SymId, u8)>,
    got_at: HashMap<(SymId, u8), usize>,
    plt: Vec<SymId>,
    plt_at: HashMap<SymId, usize>,
    dynsyms: Vec<DynSym>,
    dyn_index: HashMap<SymId, u32>,
    /// First exported dynamic symbol.
    exports_from: usize,
    gnu_buckets: u32,
    dynstr: StrTab,
    needed: Vec<u32>,
    /// `(dso, version name, index)` of the version needs.
    verneed: Vec<(u32, String, u16)>,
    nrela: usize,
    fdes: Vec<AtomId>,
    synth: HashMap<SymId, u64>,
    tls: (u64, u64, u64),
    /// Atom addresses.
    addrs: Vec<u64>,
    /// Output section index of each input section.
    shndx_of: HashMap<(u32, u32), u16>,
    /// 32-bit ARM thunks: the target and whether from T32 code.
    thunks: Vec<(SymId, bool)>,
    thunk_at: HashMap<(SymId, bool), usize>,
    /// 32-bit ARM index entries, in address order.
    exidx: Vec<ExEnt>,
    /// POWER: long-branch stub islands in `.text`.
    islands: Vec<Island>,
    /// POWER: the island and stub of each far call, by atom and
    /// relocation index.
    far: HashMap<(AtomId, usize), (usize, usize)>,
}

/// POWER: long-branch stubs at an offset of `.text`, each to a target
/// and addend (a local call's: past the TOC setup).
#[derive(Debug, Default)]
struct Island {
    off: u64,
    stubs: Vec<(Target, i64)>,
}

/// GOT slot kinds: the address, the offset from the thread pointer, a
/// general dynamic pair (module, offset), a local dynamic one (module,
/// 0).
const GOT_ADDR: u8 = 0;
const GOT_TP: u8 = 1;
const GOT_GD: u8 = 2;
const GOT_GD_OFF: u8 = 3;
const GOT_LD: u8 = 4;
const GOT_LD_ZERO: u8 = 5;
/// RISC-V, s390x: `.got`'s first word, `_DYNAMIC`, as lld writes it.
const GOT_DYNAMIC: u8 = 6;
/// s390x: the rest of `.got`'s header, ld.so's.
const GOT_RESERVED: u8 = 7;
/// POWER: `.got`'s first word, the TOC base.
const GOT_TOC: u8 = 8;

impl Out<'_> {
    fn sec(&self, s: Synth) -> Option<&OSec> {
        self.secs.iter().find(|x| x.synth == s)
    }

    const fn ptr(&self) -> u64 {
        self.l.arch.ptr()
    }

    /// i386's and s390x's PLT start with the lazy binding entry.
    const fn plt_header(&self) -> u64 {
        match self.l.arch {
            Arch::I386 => 16,
            Arch::S390x => s390x::PLT_HEADER,
            _ => 0,
        }
    }

    const fn plt_entry(&self) -> u64 {
        plt_entry(self.l.arch)
    }

    /// What `_GLOBAL_OFFSET_TABLE_` stands for: `.got.plt` on i386, else
    /// `.got`; POWER's TOC base.
    fn got_base(&self) -> u64 {
        let s = if self.l.arch == Arch::I386 {
            Synth::GotPlt
        } else {
            Synth::Got
        };
        let a = self.sec(s).map_or(0, |s| s.addr);
        if self.l.arch == Arch::Ppc64 {
            a + ppc64::TOC_BIAS
        } else {
            a
        }
    }

    fn atom_addr(&self, a: AtomId) -> u64 {
        let a = self.l.alias.get(&a).copied().unwrap_or(a);
        self.addrs[a as usize]
    }

    fn kind(&self, t: Target) -> SymKind {
        match t {
            Target::Atom(DEAD) => SymKind::UndefWeak,
            Target::Atom(_) => SymKind::Local,
            Target::Sym(id) => match self.l.syms.get(id).def {
                Def::Defined { .. } | Def::Synthetic | Def::Common { .. } => SymKind::Local,
                Def::Absolute(_) => SymKind::Abs,
                Def::Dylib { .. } => SymKind::Dso,
                Def::Undefined | Def::Lazy { .. } => SymKind::UndefWeak,
            },
        }
    }

    fn func(&self, t: Target) -> bool {
        match t {
            Target::Atom(_) => false,
            Target::Sym(id) => matches!(self.l.ext(id).ty, STT_FUNC | STT_GNU_IFUNC),
        }
    }

    /// The address of target `t` (a shared object's function: its PLT
    /// entry).
    fn addr(&self, t: Target) -> u64 {
        match t {
            Target::Atom(DEAD) => 0,
            Target::Atom(a) => self.atom_addr(a),
            Target::Sym(id) => match self.l.syms.get(id).def {
                Def::Defined { atom, off } => self.atom_addr(atom) + off,
                Def::Absolute(v) => v,
                Def::Synthetic => self.synth.get(&id).copied().unwrap_or(0),
                Def::Dylib { .. } => self.plt_at.get(&id).map_or(0, |&k| {
                    self.sec(Synth::Plt).map_or(0, |p| p.addr)
                        + self.plt_header()
                        + self.plt_entry() * k as u64
                }),
                _ => 0,
            },
        }
    }

    fn got_slot(&self, id: SymId, kind: u8) -> u64 {
        self.got_at.get(&(id, kind)).map_or(0, |&k| {
            self.sec(Synth::Got).map_or(0, |g| g.addr) + self.ptr() * k as u64
        })
    }

    /// Offset of TLS address `a` from the thread pointer.
    fn tpoff(&self, a: u64) -> i64 {
        let (start, memsz, align) = self.tls;
        let off = a.wrapping_sub(start) as i64;
        match self.l.arch {
            Arch::X86_64 | Arch::I386 | Arch::S390x => {
                off - memsz.next_multiple_of(align.max(1)) as i64
            }
            Arch::Aarch64 => off + 16u64.next_multiple_of(align.max(1)) as i64,
            // The TCB: 8 bytes.
            Arch::Arm => off + 8u64.next_multiple_of(align.max(1)) as i64,
            // The thread pointer at the block.
            Arch::Riscv64 | Arch::Riscv32 => off,
            Arch::Ppc64 => off - ppc64::TP_OFFSET,
        }
    }

    /// The thunk a `b` from the other instruction set to `id` takes
    /// (32-bit ARM): `Some(from T32)`.
    fn arm_thunk(&self, ty: u16, id: SymId) -> Option<bool> {
        if self.l.arch != Arch::Arm {
            return None;
        }
        let (thumb, plt) = match self.l.syms.get(id).def {
            Def::Defined { off, .. } => (off & 1 == 1, false),
            Def::Dylib { .. } => (false, true),
            _ => return None,
        };
        arm32::thunk(ty, self.func(Target::Sym(id)), thumb, plt)
    }

    fn thunk_addr(&self, id: SymId, from_thumb: bool) -> u64 {
        let k = self.thunk_at.get(&(id, from_thumb)).copied().unwrap_or(0);
        self.sec(Synth::Thunks).map_or(0, |t| t.addr) + arm32::THUNK * k as u64
    }

    /// The output section index of atom `a`, `SHN_ABS` if none.
    fn shndx(&self, a: AtomId) -> u16 {
        let a = self.l.alias.get(&a).copied().unwrap_or(a);
        let at = &self.l.atoms[a as usize];
        self.shndx_of
            .get(&(at.file, at.sect))
            .copied()
            .unwrap_or(0xfff1)
    }

    fn sect_of(&self, a: &Atom) -> &InSect {
        &self.l.files[a.file as usize].sects[a.sect as usize]
    }
}

/// Live atoms into output sections, in input order.
fn input_sections(l: &mut Link) -> Vec<OSec> {
    let mut secs: Vec<OSec> = Vec::new();
    let mut by_name: HashMap<String, usize> = HashMap::default();
    let mut order = 0;
    for f in &l.files {
        for s in &f.sects {
            if s.kind == object::SectKind::Skip {
                continue;
            }
            let live: Vec<AtomId> = s
                .atoms
                .iter()
                .copied()
                .filter(|&a| l.atoms[a as usize].live)
                .collect();
            if live.is_empty() {
                continue;
            }
            let name = object::output_name(&s.name);
            let k = *by_name.entry(name.to_owned()).or_insert_with(|| {
                let ty = match s.ty {
                    crate::elf::object::SHT_X86_64_UNWIND => SHT_PROGBITS,
                    t => t,
                };
                secs.push(OSec::new(name, ty, s.flags, 1, Synth::Input));
                secs.len() - 1
            });
            let o = &mut secs[k];
            if o.first == usize::MAX {
                o.first = order;
                order += 1;
            }
            o.flags |= s.flags & (SHF_ALLOC | SHF_WRITE | SHF_EXECINSTR | SHF_TLS);
            if o.ty != s.ty && !(s.ty == SHT_NOBITS && o.ty == SHT_NOBITS) {
                // Mixed: zero-fill pieces take file space.
                if o.ty == SHT_NOBITS || s.ty == SHT_NOBITS {
                    o.ty = SHT_PROGBITS;
                }
            }
            o.align = o.align.max(s.align);
            if s.entsize != 0 && o.atoms.is_empty() {
                o.entsize = s.entsize;
            }
            o.atoms.extend(live);
        }
    }
    // Init and fini arrays by priority, unprioritized last.
    for o in &mut secs {
        if matches!(o.ty, SHT_INIT_ARRAY | SHT_FINI_ARRAY) {
            let prio = |a: &AtomId| {
                let at = &l.atoms[*a as usize];
                let n = &l.files[at.file as usize].sects[at.sect as usize].name;
                n.rsplit_once('.')
                    .and_then(|(_, p)| p.parse::<u32>().ok())
                    .unwrap_or(65536)
            };
            o.atoms.sort_by_key(prio);
        }
    }
    // Identical strings share one copy.
    for o in &mut secs {
        let mut seen: HashMap<&[u8], AtomId> = HashMap::default();
        o.atoms.retain(|&a| {
            let at = &l.atoms[a as usize];
            let f = &l.files[at.file as usize];
            let s = &f.sects[at.sect as usize];
            if s.kind != object::SectKind::Pieces {
                return true;
            }
            let start = (s.off + at.off) as usize;
            let bytes = &f.data[start..start + at.size as usize];
            if let Some(&c) = seen.get(bytes) {
                l.alias.insert(a, c);
                false
            } else {
                seen.insert(bytes, a);
                true
            }
        });
    }
    secs
}

/// The backtrace table's functions and the runtime's anchor atom, when
/// the runtime is linked in.
fn trace_plan(l: &Link) -> Option<(crate::trace::Plan, (AtomId, u64))> {
    let anchor = match l.syms.get(l.syms.find(crate::trace::ANCHOR)?).def {
        Def::Defined { atom, off } if l.atoms[atom as usize].live => (atom, off),
        _ => return None,
    };
    let canon = |a: AtomId| {
        let c = l.alias.get(&a).copied().unwrap_or(a);
        let x = &l.atoms[c as usize];
        x.live.then_some((c, x.size))
    };
    let globals = crate::trace::globals(&l.syms)
        .filter(|&(n, _, _)| l.syms.find(n).is_some_and(|id| l.ext(id).ty == STT_FUNC));
    // Local functions only in a link that keeps its symbols.
    let locals = l.files.iter().filter(|_| l.keep_debug).flat_map(|f| {
        f.locals
            .iter()
            .filter(|s| s.ty == STT_FUNC)
            .map(|s| (crate::bytes::cstr(&f.data, s.name), s.atom, s.off))
    });
    let p = crate::trace::Plan::new(globals.chain(locals), "", canon);
    (!p.is_empty()).then_some((p, anchor))
}

/// The backtrace table into section `s`, and its place into the
/// runtime's anchor word.
fn write_trace(
    out: &Out<'_>,
    buf: &mut [u8],
    p: &crate::trace::Plan,
    (atom, off): (AtomId, u64),
    s: &OSec,
) {
    let l = out.l;
    let big = l.arch.endian().big;
    let t = p.table(s.addr, big, |a| {
        (out.atom_addr(a), l.atoms[a as usize].size)
    });
    buf[s.off as usize..s.off as usize + t.len()].copy_from_slice(&t);
    let at = out.atom_addr(atom) + off;
    let _ = write_word(out, buf, at, s.addr.wrapping_sub(at));
}

/// Pointer-sized `v` at address `at` of the image, in a section with
/// file contents; `None` when there is none.
fn write_word(out: &Out<'_>, buf: &mut [u8], at: u64, v: u64) -> Option<()> {
    let l = out.l;
    let width = l.arch.ptr() as usize;
    let d = out.secs.iter().find(|x| {
        x.alloc() && !x.nobits() && at >= x.addr && at + width as u64 <= x.addr + x.size
    })?;
    let pos = (d.off + at - d.addr) as usize;
    let b = crate::trace::anchor(v, 0, width, l.arch.endian().big);
    buf[pos..pos + width].copy_from_slice(&b);
    Some(())
}

/// Write the executable `o.out`.
#[allow(clippy::too_many_lines, clippy::match_same_arms)]
pub fn write(l: &mut Link, o: &Options, start_stop: &[(SymId, String)]) -> Result<(), String> {
    let mut secs = input_sections(l);
    let l: &Link = l;
    let dynamic = o.dynamic();
    let mut out = Out {
        l,
        o,
        secs: Vec::new(),
        got: Vec::new(),
        got_at: HashMap::default(),
        plt: Vec::new(),
        plt_at: HashMap::default(),
        dynsyms: Vec::new(),
        dyn_index: HashMap::default(),
        exports_from: 1,
        gnu_buckets: 1,
        dynstr: StrTab::new(),
        needed: Vec::new(),
        verneed: Vec::new(),
        nrela: 0,
        fdes: Vec::new(),
        synth: HashMap::default(),
        tls: (0, 0, 1),
        addrs: vec![0; l.atoms.len()],
        shndx_of: HashMap::default(),
        thunks: Vec::new(),
        thunk_at: HashMap::default(),
        exidx: Vec::new(),
        islands: Vec::new(),
        far: HashMap::default(),
    };

    // Scan relocations: GOT, PLT and dynamic relocation needs.
    let mut errors = Vec::new();
    let mut imports: Vec<SymId> = Vec::new();
    let mut imported: nabic_core::HashSet<SymId> = nabic_core::HashSet::default();
    let mut need_got = ["_GLOBAL_OFFSET_TABLE_", ".TOC."].iter().any(|n| {
        l.syms
            .find(n)
            .is_some_and(|id| l.syms.get(id).def == Def::Synthetic)
    });
    let mut dynrels = 0usize;
    for s in &secs {
        if !s.alloc() {
            continue;
        }
        for &a in &s.atoms {
            let at = &l.atoms[a as usize];
            let is = &l.files[at.file as usize].sects[at.sect as usize];
            let base = (is.off + at.off) as usize;
            let data = &l.files[at.file as usize].data;
            for r in &at.relocs {
                let kind = out.kind(r.target);
                if let Target::Sym(id) = r.target {
                    if l.ext(id).ty == STT_GNU_IFUNC && kind == SymKind::Local {
                        errors.push(format!(
                            "{}: {}: IFUNC symbols are not supported",
                            l.file_name(at.file),
                            l.syms.get(id).name
                        ));
                        continue;
                    }
                }
                let own_tls =
                    o.shared && kind == SymKind::Local && reloc::dynamic_tls(l.arch, r.kind);
                let site = Site {
                    arch: l.arch,
                    ty: r.kind,
                    kind: if own_tls && !o.dynamic_tls {
                        SymKind::Dso
                    } else {
                        kind
                    },
                    func: out.func(r.target),
                    alloc: true,
                    pie: o.pie,
                    dyn_tls: own_tls && o.dynamic_tls,
                };
                let relax = reloc::relax_got(l.arch, data, base + r.off as usize, r.kind, kind);
                let n = match reloc::need(site, relax) {
                    Ok(n) => n,
                    Err(e) => {
                        errors.push(format!(
                            "{e}\n>>> referenced by {}:({}+{:#x})",
                            l.file_name(at.file),
                            is.name,
                            at.off + u64::from(r.off)
                        ));
                        continue;
                    }
                };
                // The loader cannot write a read-only section (no
                // DT_TEXTREL): code built without -fPIC, such as a
                // pointer table in .rodata.
                if n.dynrel && s.flags & SHF_WRITE == 0 {
                    errors.push(format!(
                        "relocation {} in read-only section {} needs a dynamic relocation; \
                         recompile with -fPIC\n>>> referenced by {}:({}+{:#x})",
                        reloc::type_name(l.arch, r.kind),
                        is.name,
                        l.file_name(at.file),
                        is.name,
                        at.off + u64::from(r.off)
                    ));
                    continue;
                }
                let Target::Sym(id) = r.target else {
                    if n.dynrel {
                        dynrels += 1;
                    }
                    continue;
                };
                if let Some(t) = out.arm_thunk(r.kind, id) {
                    let n = out.thunks.len();
                    if *out.thunk_at.entry((id, t)).or_insert(n) == n {
                        out.thunks.push((id, t));
                    }
                }
                // A pair's second slot follows its first.
                let slot = |kind: u8, out: &mut Out<'_>| {
                    if let std::collections::hash_map::Entry::Vacant(e) =
                        out.got_at.entry((id, kind))
                    {
                        e.insert(out.got.len());
                        out.got.push((id, kind));
                        if matches!(kind, GOT_GD | GOT_LD) {
                            out.got.push((id, kind + 1));
                        }
                    }
                };
                // A lazy PLT jumps through `.got.plt`.
                if n.got || (n.plt && !lazy_plt(l.arch)) {
                    slot(GOT_ADDR, &mut out);
                }
                if n.tlsgot {
                    slot(GOT_TP, &mut out);
                }
                if n.tlsgd {
                    slot(GOT_GD, &mut out);
                }
                if n.tlsld {
                    slot(GOT_LD, &mut out);
                }
                if n.plt && !out.plt_at.contains_key(&id) {
                    out.plt_at.insert(id, out.plt.len());
                    out.plt.push(id);
                }
                if n.dynrel {
                    dynrels += 1;
                }
                need_got |= n.got_base;
                if kind == SymKind::Dso && imported.insert(id) {
                    imports.push(id);
                }
            }
        }
    }
    if !errors.is_empty() {
        errors.truncate(20);
        return Err(errors.join("\n"));
    }
    // `.got`'s header, as lld writes it.
    let header: &[u8] = match l.arch {
        _ if out.got.is_empty() && !need_got && (out.plt.is_empty() || l.arch != Arch::S390x) => {
            &[]
        }
        Arch::Riscv64 | Arch::Riscv32 => &[GOT_DYNAMIC],
        Arch::S390x => &[GOT_DYNAMIC, GOT_RESERVED, GOT_RESERVED],
        Arch::Ppc64 => &[GOT_TOC],
        _ => &[],
    };
    if !header.is_empty() {
        let mut got: Vec<(SymId, u8)> = header.iter().map(|&g| (0, g)).collect();
        got.append(&mut out.got);
        out.got = got;
        for k in out.got_at.values_mut() {
            *k += header.len();
        }
    }
    // GOT slots that take a dynamic relocation.
    for &(id, g) in &out.got {
        if matches!(g, GOT_DYNAMIC | GOT_RESERVED | GOT_TOC) {
            continue;
        }
        let k = out.kind(Target::Sym(id));
        if (k == SymKind::Dso && g <= GOT_GD_OFF)
            || (g == GOT_ADDR && o.pie && k == SymKind::Local)
            || (g == GOT_TP && o.shared && k == SymKind::Local)
            || (g == GOT_GD && o.shared && o.dynamic_tls && k == SymKind::Local)
        {
            dynrels += 1;
        }
    }
    out.nrela = if dynamic { dynrels } else { 0 };
    if !dynamic && dynrels != 0 {
        // A static executable resolves everything itself.
        out.nrela = 0;
    }

    // Live FDEs, for .eh_frame_hdr.
    if o.eh_frame_hdr {
        out.fdes = l
            .fdes
            .iter()
            .filter(|(f, _)| l.atoms[*f as usize].live)
            .map(|&(f, _)| f)
            .collect();
    }

    // Dynamic symbols: imports, then exports by hash bucket.
    if dynamic {
        let mut ver_index: HashMap<(u32, String), u16> = HashMap::default();
        let mut used = vec![false; l.dsos.len()];
        for &id in &imports {
            let s = l.syms.get(id);
            let Def::Dylib { lib } = s.def else {
                continue;
            };
            if !s.weak_ref {
                used[lib as usize] = true;
            }
            let d = &l.dsos[lib as usize];
            let x = l.ext(id);
            let ver = match d.verdefs.get(&x.dso_ver).filter(|_| x.dso_ver >= 2) {
                Some(v) => {
                    let next = 2 + ver_index.len() as u16;
                    let k = *ver_index.entry((lib, v.clone())).or_insert(next);
                    if k == next {
                        out.verneed.push((lib, v.clone(), k));
                    }
                    k
                }
                None => 1,
            };
            let name = out.dynstr.add(&s.name);
            out.dyn_index.insert(id, out.dynsyms.len() as u32 + 1);
            out.dynsyms.push(DynSym {
                id,
                name,
                ver,
                hash: 0,
            });
        }
        out.exports_from = out.dynsyms.len() + 1;
        let mut exports: Vec<DynSym> = (0..l.syms.syms.len() as SymId)
            .filter(|&id| crate::elf::exported(l, o, id))
            .filter(|&id| match l.syms.get(id).def {
                Def::Defined { atom, .. } => l.atoms[atom as usize].live,
                _ => true,
            })
            .map(|id| {
                let n = &l.syms.get(id).name;
                DynSym {
                    id,
                    name: 0,
                    ver: 1,
                    hash: gnu_hash(n),
                }
            })
            .collect();
        out.gnu_buckets = (exports.len() as u32 / 4).max(1);
        let nb = out.gnu_buckets;
        exports.sort_by_key(|e| (e.hash % nb, e.id));
        for mut e in exports {
            e.name = out.dynstr.add(&l.syms.get(e.id).name);
            out.dyn_index.insert(e.id, out.dynsyms.len() as u32 + 1);
            out.dynsyms.push(e);
        }
        for (i, d) in l.dsos.iter().enumerate() {
            if used[i] || !d.as_needed {
                out.needed.push(i as u32);
            }
        }
        for &i in &out.needed.clone() {
            out.dynstr.add(&l.dsos[i as usize].soname);
        }
        for (_, v, _) in &out.verneed.clone() {
            out.dynstr.add(v);
        }
        if !o.rpaths.is_empty() {
            out.dynstr.add(&o.rpaths.join(":"));
        }
        if let Some(n) = &o.soname {
            out.dynstr.add(n);
        }
    }

    // Synthetic sections.
    let arch = l.arch;
    let i386 = arch == Arch::I386;
    let lazy = lazy_plt(arch);
    let arm = arch == Arch::Arm;
    // REL dynamic relocations (ELF32), else RELA.
    let rel = arch.rel();
    let ptr = arch.ptr();
    let sym_size = if arch.elf64() { 24 } else { 16 };
    let dyn_size = 2 * ptr;
    // A dynamic relocation: RELA, REL on ELF32.
    let rel_size = if rel { 8 } else { 24 };
    let add = |s: OSec, secs: &mut Vec<OSec>| secs.push(s);
    if let Some(i) = o.interp.as_ref().filter(|_| dynamic) {
        let mut s = OSec::new(".interp", SHT_PROGBITS, SHF_ALLOC, 1, Synth::Interp);
        s.size = i.len() as u64 + 1;
        add(s, &mut secs);
    }
    if o.build_id {
        let mut s = OSec::new(".note.gnu.build-id", SHT_NOTE, SHF_ALLOC, 4, Synth::BuildId);
        s.size = 16 + 8;
        add(s, &mut secs);
    }
    if dynamic {
        let n = out.dynsyms.len() as u64 + 1;
        let mut s = OSec::new(".dynsym", SHT_DYNSYM, SHF_ALLOC, ptr, Synth::Dynsym);
        s.size = sym_size * n;
        s.entsize = sym_size;
        s.info = 1;
        add(s, &mut secs);
        if !out.verneed.is_empty() {
            let mut s = OSec::new(".gnu.version", SHT_GNU_VERSYM, SHF_ALLOC, 2, Synth::Versym);
            s.size = 2 * n;
            s.entsize = 2;
            add(s, &mut secs);
            let files = out
                .verneed
                .iter()
                .map(|v| v.0)
                .collect::<std::collections::BTreeSet<_>>()
                .len();
            let mut s = OSec::new(
                ".gnu.version_r",
                SHT_GNU_VERNEED,
                SHF_ALLOC,
                4,
                Synth::Verneed,
            );
            s.size = 16 * (files + out.verneed.len()) as u64;
            s.info = files as u32;
            add(s, &mut secs);
        }
        let nexp = out.dynsyms.len() + 1 - out.exports_from;
        let mut s = OSec::new(".gnu.hash", SHT_GNU_HASH, SHF_ALLOC, ptr, Synth::GnuHash);
        s.size = 16 + ptr + 4 * u64::from(out.gnu_buckets) + 4 * nexp as u64;
        add(s, &mut secs);
        let mut s = OSec::new(".dynstr", SHT_STRTAB, SHF_ALLOC, 1, Synth::Dynstr);
        s.size = out.dynstr.data.len() as u64;
        add(s, &mut secs);
        if out.nrela > 0 {
            let (name, ty) = if rel {
                (".rel.dyn", SHT_REL)
            } else {
                (".rela.dyn", SHT_RELA)
            };
            let mut s = OSec::new(name, ty, SHF_ALLOC, ptr, Synth::RelaDyn);
            s.size = rel_size * out.nrela as u64;
            s.entsize = rel_size;
            add(s, &mut secs);
        }
        if lazy && !out.plt.is_empty() {
            let (name, ty) = if rel {
                (".rel.plt", SHT_REL)
            } else {
                (".rela.plt", SHT_RELA)
            };
            let mut s = OSec::new(
                name,
                ty,
                SHF_ALLOC | object::SHF_INFO_LINK,
                ptr,
                Synth::RelPlt,
            );
            s.size = rel_size * out.plt.len() as u64;
            s.entsize = rel_size;
            add(s, &mut secs);
        }
    }
    let have_eh = secs.iter().any(|s| s.name == ".eh_frame" && s.alloc());
    if o.eh_frame_hdr && have_eh {
        let mut s = OSec::new(
            ".eh_frame_hdr",
            SHT_PROGBITS,
            SHF_ALLOC,
            4,
            Synth::EhFrameHdr,
        );
        s.size = 12 + 8 * out.fdes.len() as u64;
        add(s, &mut secs);
    }
    let trace = trace_plan(l);
    if let Some((p, _)) = &trace {
        let mut s = OSec::new(".nabi_symbols", SHT_PROGBITS, SHF_ALLOC, 4, Synth::Trace);
        s.size = p.size();
        add(s, &mut secs);
    }
    if !out.plt.is_empty() {
        let mut s = OSec::new(
            ".plt",
            SHT_PROGBITS,
            SHF_ALLOC | SHF_EXECINSTR,
            16,
            Synth::Plt,
        );
        s.size = out.plt_header() + out.plt_entry() * out.plt.len() as u64;
        if arch == Arch::Ppc64 {
            s.align = 4;
        }
        add(s, &mut secs);
    }
    if !out.thunks.is_empty() {
        let mut s = OSec::new(
            ".text.thunk",
            SHT_PROGBITS,
            SHF_ALLOC | SHF_EXECINSTR,
            4,
            Synth::Thunks,
        );
        s.size = arm32::THUNK * out.thunks.len() as u64;
        add(s, &mut secs);
    }
    if arm {
        // The input tables are rewritten as one, sized after ordering.
        secs.retain(|s| !(s.synth == Synth::Input && s.name == ".ARM.exidx"));
        let mut s = OSec::new(
            ".ARM.exidx",
            arm32::SHT_EXIDX,
            SHF_ALLOC | arm32::SHF_LINK_ORDER,
            4,
            Synth::Exidx,
        );
        s.size = 0;
        add(s, &mut secs);
        let mut s = OSec::new(
            ".ARM.attributes",
            arm32::SHT_ATTRIBUTES,
            0,
            1,
            Synth::Attributes,
        );
        s.size = arm32::ATTRIBUTES.len() as u64;
        add(s, &mut secs);
    }
    let attributes = if arch.riscv() && !l.attributes.is_empty() {
        let a = riscv::merge_attributes(&l.attributes)?;
        let mut s = OSec::new(
            ".riscv.attributes",
            riscv::SHT_ATTRIBUTES,
            0,
            1,
            Synth::Attributes,
        );
        s.size = a.len() as u64;
        add(s, &mut secs);
        a
    } else {
        Vec::new()
    };
    if !out.got.is_empty() || (need_got && !i386) {
        let mut s = OSec::new(".got", SHT_PROGBITS, SHF_ALLOC | SHF_WRITE, ptr, Synth::Got);
        s.size = ptr * out.got.len() as u64;
        add(s, &mut secs);
    }
    if (i386 && need_got) || (lazy && !out.plt.is_empty()) {
        // i386: `_DYNAMIC` and two words for ld.so, then a slot per PLT
        // entry; s390x keeps its header in `.got`.
        let mut s = OSec::new(
            ".got.plt",
            SHT_PROGBITS,
            SHF_ALLOC | SHF_WRITE,
            ptr,
            Synth::GotPlt,
        );
        s.size = ptr * (u64::from(i386) * 3 + out.plt.len() as u64);
        add(s, &mut secs);
    }
    let mut dyn_tags: Vec<(u64, u64)> = Vec::new();
    if dynamic {
        // Values are filled after layout; the count fixes the size.
        for &i in &out.needed {
            dyn_tags.push((DT_NEEDED, u64::from(i)));
        }
        if o.soname.is_some() {
            dyn_tags.push((DT_SONAME, 0));
        }
        if !o.rpaths.is_empty() {
            dyn_tags.push((DT_RUNPATH, 0));
        }
        let init = o.init.as_deref().unwrap_or("_init");
        for (n, t) in [(init, DT_INIT), ("_fini", DT_FINI)] {
            if l.syms.find(n).is_some_and(|id| {
                matches!(l.syms.get(id).def, Def::Defined { atom, .. } if l.atoms[atom as usize].live)
            }) {
                dyn_tags.push((t, 0));
            }
        }
        for (ty, a, z) in [
            (SHT_PREINIT_ARRAY, DT_PREINIT_ARRAY, DT_PREINIT_ARRAYSZ),
            (SHT_INIT_ARRAY, DT_INIT_ARRAY, DT_INIT_ARRAYSZ),
            (SHT_FINI_ARRAY, DT_FINI_ARRAY, DT_FINI_ARRAYSZ),
        ] {
            if secs.iter().any(|s| s.ty == ty) {
                dyn_tags.push((a, 0));
                dyn_tags.push((z, 0));
            }
        }
        for t in [DT_GNU_HASH, DT_STRTAB, DT_SYMTAB, DT_STRSZ, DT_SYMENT] {
            dyn_tags.push((t, 0));
        }
        if !o.shared {
            dyn_tags.push((DT_DEBUG, 0));
        }
        if lazy && !out.plt.is_empty() {
            for t in [DT_JMPREL, DT_PLTRELSZ, DT_PLTGOT, DT_PLTREL] {
                dyn_tags.push((t, 0));
            }
        }
        if out.nrela > 0 {
            let tags = if rel {
                [DT_REL, DT_RELSZ, DT_RELENT, DT_RELCOUNT]
            } else {
                [DT_RELA, DT_RELASZ, DT_RELAENT, DT_RELACOUNT]
            };
            for t in tags {
                dyn_tags.push((t, 0));
            }
        }
        if !out.verneed.is_empty() {
            for t in [DT_VERSYM, DT_VERNEED, DT_VERNEEDNUM] {
                dyn_tags.push((t, 0));
            }
        }
        if o.shared {
            // Initial-exec thread-locals: the block must be in the
            // static TLS area, also when the library is `dlopen`ed.
            let tls = !o.dynamic_tls && secs.iter().any(|s| s.alloc() && s.tls());
            dyn_tags.push((DT_FLAGS, if tls { DF_STATIC_TLS } else { 0 }));
        } else {
            dyn_tags.push((DT_FLAGS_1, DF_1_PIE));
        }
        dyn_tags.push((0, 0));
        let mut s = OSec::new(
            ".dynamic",
            SHT_DYNAMIC,
            SHF_ALLOC | SHF_WRITE,
            ptr,
            Synth::Dynamic,
        );
        s.size = dyn_size * dyn_tags.len() as u64;
        s.entsize = dyn_size;
        add(s, &mut secs);
    }

    // Order: segments, then rank, then appearance.
    for (i, s) in secs.iter_mut().enumerate() {
        if s.first == usize::MAX {
            s.first = 100_000 + i;
        }
        // Zero-fill in the RELRO segment is written out.
        if s.nobits() && !s.tls() && s.relro() {
            s.ty = SHT_PROGBITS;
        }
    }
    secs.sort_by_key(|s| (s.rank(), s.first));

    // Atom offsets within their sections.
    for s in &mut secs {
        if s.synth != Synth::Input {
            continue;
        }
        let mut at = 0u64;
        for &a in &s.atoms {
            let x = &l.atoms[a as usize];
            at = at.next_multiple_of(1 << x.align);
            out.addrs[a as usize] = at;
            at += x.size;
        }
        s.size = at;
    }
    if arch == Arch::Ppc64 {
        plan_islands(&mut out, &mut secs);
    }
    if arm {
        out.exidx = exidx_entries(l, &secs);
        let n = out.exidx.len() as u64;
        if let Some(x) = secs.iter_mut().find(|s| s.synth == Synth::Exidx) {
            x.size = 8 * n;
        }
        if n == 0 {
            secs.retain(|s| s.synth != Synth::Exidx);
        }
    }

    // Program headers.
    let groups: Vec<u8> = {
        let mut g: Vec<u8> = vec![0];
        for s in secs
            .iter()
            .filter(|s| s.alloc() && !(s.tls() && s.nobits()))
        {
            let r = s.rank().0;
            if *g.last().unwrap() != r {
                g.push(r);
            }
        }
        g
    };
    let has = |p: &dyn Fn(&OSec) -> bool| secs.iter().any(|s| s.alloc() && p(s));
    let interp = has(&|s| s.synth == Synth::Interp);
    let tls_any = has(&|s| s.tls());
    let relro_any = groups.contains(&2);
    let eh_hdr = has(&|s| s.synth == Synth::EhFrameHdr);
    let notes = has(&|s| s.ty == SHT_NOTE);
    let exidx_any = has(&|s| s.synth == Synth::Exidx);
    let attrs_any = secs
        .iter()
        .any(|s| s.ty == riscv::SHT_ATTRIBUTES && arch.riscv());
    let nph = groups.len()
        + usize::from(exidx_any)
        + usize::from(attrs_any)
        + 2 * usize::from(interp)
        // RISC-V: PT_PHDR without an interpreter too, as lld writes it.
        + usize::from(!interp && arch.riscv())
        + usize::from(tls_any)
        + usize::from(dynamic)
        + usize::from(relro_any)
        + usize::from(eh_hdr)
        + usize::from(notes)
        + 1;
    let base = if o.pie { 0 } else { arch.static_base() };
    let page = o.max_page.unwrap_or_else(|| l.arch.max_page());
    let (ehdr, phdr) = if arch.elf64() { (64, 56) } else { (52, 32) };
    let header = ehdr + phdr * nph as u64;
    let (mut off, mut addr) = (header, base + header);
    let mut group = 0u8;
    // The TLS template starts at its largest alignment.
    let tls_align = secs
        .iter()
        .filter(|s| s.alloc() && s.tls())
        .map(|s| s.align)
        .max()
        .unwrap_or(1);
    let mut first_tls = true;
    for s in secs.iter_mut().filter(|s| s.alloc()) {
        let mut a = s.align.max(1);
        if s.tls() && first_tls {
            a = a.max(tls_align);
            first_tls = false;
        }
        if a > page {
            return Err(format!("{}: alignment {a} exceeds the page size", s.name));
        }
        let g = s.rank().0;
        if g != group {
            group = g;
            off = off.next_multiple_of(a);
            addr = addr.next_multiple_of(page) + off % page;
        }
        if s.nobits() {
            s.addr = addr.next_multiple_of(a);
            s.off = off.next_multiple_of(a);
            // .tbss takes no address space of its own.
            if !s.tls() {
                addr = s.addr + s.size;
            }
            continue;
        }
        off = off.next_multiple_of(a);
        addr = addr.next_multiple_of(a);
        s.addr = addr;
        s.off = off;
        off += s.size;
        addr += s.size;
    }
    for s in &secs {
        for &a in &s.atoms {
            out.addrs[a as usize] += s.addr;
        }
    }
    for (&dup, &c) in &l.alias {
        out.addrs[dup as usize] = out.addrs[c as usize];
    }
    if tls_any {
        let t: Vec<&OSec> = secs.iter().filter(|s| s.alloc() && s.tls()).collect();
        let start = t[0].addr;
        let end = t.iter().map(|s| s.addr + s.size).max().unwrap_or(start);
        let align = t.iter().map(|s| s.align).max().unwrap_or(1);
        out.tls = (start, end - start, align);
    }

    // Linker-defined symbols.
    let find = |n: &str| secs.iter().find(|s| s.name == n && s.alloc());
    let seg_end = |g: u8, file: bool| {
        secs.iter()
            .filter(|s| s.alloc() && s.rank().0 == g && !(s.tls() && s.nobits()))
            .filter(|s| !(file && s.nobits()))
            .map(|s| s.addr + s.size)
            .max()
    };
    let data_end = seg_end(3, true)
        .or_else(|| seg_end(2, true))
        .unwrap_or(base);
    let end = seg_end(3, false)
        .or_else(|| seg_end(2, false))
        .unwrap_or(data_end);
    let text_end = seg_end(1, false).unwrap_or(base);
    for (i, sym) in l.syms.syms.iter().enumerate() {
        if sym.def != Def::Synthetic {
            continue;
        }
        let range = |ty: u32| {
            secs.iter()
                .find(|s| s.ty == ty && s.alloc())
                .map_or((base, base), |s| (s.addr, s.addr + s.size))
        };
        let v = match sym.name.as_str() {
            "__ehdr_start" | "__executable_start" | "__dso_handle" => base,
            "_GLOBAL_OFFSET_TABLE_" => {
                let g = if i386 { Synth::GotPlt } else { Synth::Got };
                secs.iter().find(|s| s.synth == g).map_or(0, |s| s.addr)
            }
            ".TOC." if arch == Arch::Ppc64 => secs
                .iter()
                .find(|s| s.synth == Synth::Got)
                .map_or(0, |s| s.addr + ppc64::TOC_BIAS),
            "_DYNAMIC" => secs
                .iter()
                .find(|s| s.synth == Synth::Dynamic)
                .map_or(0, |s| s.addr),
            "__bss_start" | "_edata" | "edata" => find(".bss").map_or(data_end, |s| s.addr),
            "_end" | "end" => end,
            "_etext" | "etext" => text_end,
            "__init_array_start" => range(SHT_INIT_ARRAY).0,
            "__init_array_end" => range(SHT_INIT_ARRAY).1,
            "__fini_array_start" => range(SHT_FINI_ARRAY).0,
            "__fini_array_end" => range(SHT_FINI_ARRAY).1,
            "__preinit_array_start" => range(SHT_PREINIT_ARRAY).0,
            "__preinit_array_end" => range(SHT_PREINIT_ARRAY).1,
            "__rela_iplt_start" | "__rela_iplt_end" => base,
            "__exidx_start" | "__exidx_end" => secs
                .iter()
                .find(|s| s.synth == Synth::Exidx)
                .map_or(base, |s| {
                    if sym.name == "__exidx_start" {
                        s.addr
                    } else {
                        s.addr + s.size
                    }
                }),
            // Its offset from the thread pointer is 0, as lld has it:
            // local dynamic TLS descriptors relax to `x@ntpoff` + 0.
            "_TLS_MODULE_BASE_" => (out.tls.0 as i64 - out.tpoff(out.tls.0)) as u64,
            // gp-relative reach of small data.
            "__global_pointer$" => find(".sdata").map_or(base, |s| s.addr) + 0x800,
            _ => start_stop
                .iter()
                .find(|(id, _)| *id == i as SymId)
                .and_then(|(_, sect)| {
                    let s = find(sect)?;
                    Some(if sym.name.starts_with("__start_") {
                        s.addr
                    } else {
                        s.addr + s.size
                    })
                })
                .unwrap_or(0),
        };
        out.synth.insert(i as SymId, v);
    }

    // The symbol table, then the non-allocated sections' offsets.
    let mut strtab = StrTab::new();
    let symtab = if o.strip_all {
        None
    } else {
        Some(symbol_table(&out, &secs, &mut strtab))
    };
    if let Some((syms, first_global)) = &symtab {
        let mut s = OSec::new(".symtab", SHT_SYMTAB, 0, ptr, Synth::Symtab);
        s.size = syms.len() as u64;
        s.entsize = sym_size;
        s.info = *first_global;
        secs.push(s);
        let mut s = OSec::new(".strtab", SHT_STRTAB, 0, 1, Synth::Strtab);
        s.size = strtab.data.len() as u64;
        secs.push(s);
    }
    let mut shstr = StrTab::new();
    for s in &secs {
        shstr.add(&s.name);
    }
    shstr.add(".shstrtab");
    let mut s = OSec::new(".shstrtab", SHT_STRTAB, 0, 1, Synth::Shstrtab);
    s.size = shstr.data.len() as u64;
    secs.push(s);
    for s in secs.iter_mut().filter(|s| !s.alloc()) {
        off = off.next_multiple_of(s.align.max(1));
        s.off = off;
        off += s.size;
    }
    for (i, s) in secs.iter_mut().enumerate() {
        s.shndx = i as u32 + 1;
    }
    let shoff = off.next_multiple_of(ptr);
    let shdr_size = if arch.elf64() { 64 } else { 40 };
    let file_size = shoff + shdr_size * (secs.len() as u64 + 1);
    let shidx = |secs: &[OSec], s: Synth| secs.iter().find(|x| x.synth == s).map_or(0, |x| x.shndx);
    for i in 0..secs.len() {
        let (link, info) = match secs[i].synth {
            Synth::Dynsym | Synth::Verneed | Synth::Dynamic => {
                (shidx(&secs, Synth::Dynstr), secs[i].info)
            }
            Synth::Versym | Synth::GnuHash | Synth::RelaDyn => {
                (shidx(&secs, Synth::Dynsym), secs[i].info)
            }
            Synth::RelPlt => (shidx(&secs, Synth::Dynsym), shidx(&secs, Synth::GotPlt)),
            Synth::Symtab => (shidx(&secs, Synth::Strtab), secs[i].info),
            Synth::Exidx => (
                secs.iter()
                    .find(|s| s.alloc() && s.flags & SHF_EXECINSTR != 0)
                    .map_or(0, |s| s.shndx),
                0,
            ),
            _ => (secs[i].link, secs[i].info),
        };
        secs[i].link = link;
        secs[i].info = info;
    }
    for s in &secs {
        for &a in &s.atoms {
            let at = &l.atoms[a as usize];
            out.shndx_of
                .entry((at.file, at.sect))
                .or_insert(s.shndx as u16);
        }
    }
    out.secs = secs;

    let mut buf = vec![0u8; file_size as usize];
    write_atoms(&out, &mut buf);
    let rela = apply_relocs(&out, &mut buf)?;
    write_arm(&out, &mut buf);
    write_islands(&out, &mut buf);
    if let Some(s) = out.sec(Synth::Attributes).filter(|_| arch.riscv()) {
        let at = s.off as usize;
        buf[at..at + attributes.len()].copy_from_slice(&attributes);
    }
    write_synthetic(
        &out,
        &mut buf,
        &dyn_tags,
        rela,
        symtab.as_ref().map(|s| &s.0),
        &strtab,
        &shstr,
    )?;
    if let (Some((p, anchor)), Some(s)) = (&trace, out.sec(Synth::Trace)) {
        write_trace(&out, &mut buf, p, *anchor, s);
    }
    for (a, t) in &o.anchors {
        let addr = |n: &str| {
            l.syms
                .find(n)
                .map(|id| out.addr(Target::Sym(id)))
                .ok_or_else(|| format!("--nabi-anchor: {n} is not defined"))
        };
        let (at, to) = (addr(a)?, addr(t)?);
        write_word(&out, &mut buf, at, to.wrapping_sub(at))
            .ok_or_else(|| format!("--nabi-anchor: {a} is not in initialized data"))?;
    }
    write_headers(&out, &mut buf, &groups, nph, base, shoff, &shstr);
    if o.build_id {
        build_id(&out, &mut buf);
    }
    let _ = std::fs::remove_file(&o.out);
    std::fs::write(&o.out, &buf).map_err(|e| format!("cannot write {}: {e}", o.out.display()))?;
    #[cfg(unix)]
    {
        let _ = std::fs::set_permissions(&o.out, std::fs::Permissions::from_mode(0o755));
    }
    Ok(())
}
