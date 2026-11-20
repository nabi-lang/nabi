//! A relocatable object in the making: sections, symbols, relocations.
//! The container format (`macho`, ..) only serializes it.

pub(crate) mod coff;
pub(crate) mod elf;
pub(crate) mod macho;
pub(crate) mod objc;

use crate::lir::SymId;
use nabic_core::HashMap;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Sect {
    Text,
    /// Read-only data without relocations.
    Const,
    /// Read-only data with relocations (pointers).
    DataConst,
    Data,
    /// Mach-O Objective-C metadata (`objc`).
    Objc(objc::Part),
}

pub const SECTS: [Sect; 8] = [
    Sect::Text,
    Sect::Const,
    Sect::DataConst,
    Sect::Data,
    Sect::Objc(objc::Part::Methname),
    Sect::Objc(objc::Part::Selrefs),
    Sect::Objc(objc::Part::Classrefs),
    Sect::Objc(objc::Part::Imageinfo),
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RelKind {
    /// `bl` / `b` to a symbol.
    Branch26,
    /// `adrp` of a symbol's page / the low 12 bits (`add` or a scaled
    /// load / store offset).
    Page21,
    PageOff12,
    /// `adrp` / `ldr` of the symbol's GOT slot.
    GotPage21,
    GotPageOff12,
    /// `adrp` / `ldr` of a thread-local's descriptor.
    TlvPage21,
    TlvPageOff12,
    /// ELF aarch64 TLS descriptor sequence: `adrp`, `ldr`, `add`, `blr`
    /// (general dynamic; a linker relaxes it in an executable).
    TlsDescPage21,
    TlsDescLd,
    TlsDescAdd,
    TlsDescCall,
    /// A pointer-sized absolute address (data; 32-bit on i386).
    Abs64,
    /// `x86_64` `call rel32` to a function.
    X86Call,
    /// `x86_64` `[rip + rel32]` of a symbol.
    X86Pc32,
    /// `x86_64` `mov reg, [rip + rel32]` of the symbol's GOT slot.
    X86GotLoad,
    /// `x86_64` `mov reg, [rip + rel32]` of a thread-local's
    /// thread-pointer offset (ELF initial exec).
    X86Tls,
    /// `x86_64` `data16 lea rdi, [rip + rel32]` of a thread-local's GOT
    /// pair for `__tls_get_addr` (ELF general dynamic).
    X86TlsGd,
    /// COFF: a thread-local's 32-bit offset in the TLS section (`x86_64`
    /// displacement).
    X86SecRel,
    /// COFF arm64: `add` of bits 12..24 / 0..12 of a thread-local's TLS
    /// section offset.
    SecRelHi12,
    SecRelLo12,
    /// i386 `call rel32` to a function (ELF: through the PLT).
    I386Call,
    /// i386 `add ebx, imm32` making `ebx` the GOT (`_GLOBAL_OFFSET_TABLE_`
    /// from the `pop`, 3 bytes back).
    I386GotPc,
    /// i386 `[ebx + disp32]` of a symbol / of its GOT slot / of the GOT
    /// slot of a thread-local's thread-pointer offset.
    I386GotOff,
    I386Got32X,
    I386TlsGotIe,
    /// i386 absolute 32-bit address in code (COFF: relocated by the image).
    I386Abs32,
    /// 32-bit ARM `bl` to a function (through the PLT, `blx` to Thumb).
    ArmCall,
    /// 32-bit ARM `movw` / `movt` of a symbol's distance from the place
    /// (the addend in the instruction).
    ArmMovwPrel,
    ArmMovtPrel,
    /// 32-bit ARM literal word: the distance to the symbol's GOT slot /
    /// to the GOT slot of its thread-pointer offset.
    ArmGotPrel,
    ArmTlsIe,
    /// RISC-V `auipc ra` / `jalr ra` pair calling a function (through the
    /// PLT).
    RvCall,
    /// RISC-V `auipc` of a symbol's distance / of its GOT slot's / of the
    /// GOT slot of its thread-pointer offset (high 20 bits).
    RvPcHi,
    RvGotHi,
    RvTlsIeHi,
    /// RISC-V low 12 bits of the distance the `auipc` just before computed
    /// (I-type: `addi`, loads).
    RvPcLoI,
    /// RISC-V local exec: `lui` of a thread-local's thread-pointer offset,
    /// the `add` of `tp` (a marker), the low 12 bits (`addi`).
    RvTpHi,
    RvTpAdd,
    RvTpLo,
    /// powerpc64 `bl` to a function (through a PLT stub, the `nop`
    /// after it the TOC restore).
    PpcCall,
    /// powerpc64 global entry: `addis r2, r12` / `addi r2, r2` of the
    /// distance to `.TOC.` (`sym`) from the function's start.
    PpcEntryHa,
    PpcEntryLo,
    /// powerpc64 `addis rX, r2` / `addi rX, rX` of a symbol's TOC offset.
    PpcTocHa,
    PpcTocLo,
    /// powerpc64 `addis rX, r2` / `ld rX, rX` of the symbol's GOT slot /
    /// of the GOT slot of its thread-pointer offset; the `add` of `r13`
    /// (a marker).
    PpcGotHa,
    PpcGotLo,
    PpcTlsIeHa,
    PpcTlsIeLo,
    PpcTls,
    /// s390x `brasl` to a function (through the PLT), the halfword
    /// distance in bytes 2..6 of the instruction.
    S390Call,
    /// s390x `larl` of a symbol / `lgrl` of its GOT slot / of the GOT
    /// slot of its thread-pointer offset.
    S390Pc,
    S390Got,
    S390TlsIe,
    /// DWARF `sec_offset` into debug section `sym` (an `Obj::extra` index):
    /// relocated on ELF (sections join at link), kept as is on Mach-O.
    SecOff32,
}

#[derive(Clone, Copy, Debug)]
pub struct Reloc {
    pub off: u32,
    pub kind: RelKind,
    pub sym: SymId,
}

#[derive(Clone, Debug, Default)]
pub struct Section {
    pub data: Vec<u8>,
    pub relocs: Vec<Reloc>,
    /// log2 of the alignment.
    pub align: u32,
}

impl Section {
    /// Align the end to `align` bytes; the offset there.
    pub fn align_to(&mut self, align: u32) -> u32 {
        let a = align.max(1);
        while self.data.len() % a as usize != 0 {
            self.data.push(0);
        }
        self.align = self.align.max(a.trailing_zeros());
        self.data.len() as u32
    }
}

#[derive(Clone, Debug)]
pub struct Sym {
    pub name: String,
    /// `(section, offset, size)` when defined here.
    pub def: Option<(Sect, u32, u32)>,
    pub global: bool,
    pub weak: bool,
    /// Global within the link only (Mach-O private extern, ELF hidden;
    /// a COFF image exports nothing unasked).
    pub hidden: bool,
}

/// A code range of a function (unwind info, debug info).
#[derive(Clone, Debug)]
pub struct FnRange {
    pub sym: SymId,
    pub start: u32,
    pub len: u32,
    /// `(code offset, line, column, scope)` rows.
    pub lines: Vec<(u32, u32, u32, u32)>,
    /// `-g`: the function's facts, and each variable's offset from the
    /// frame pointer.
    pub debug: Option<(crate::lir::FnDebug, Vec<i32>)>,
    /// `-g` above `-O0` (`FnDebug::opt`): where each variable is, by code
    /// range (`locs`).
    pub locs: Vec<crate::debug::locs::Spans>,
    /// Callee-saved registers the prologue stores.
    pub saves: Saves,
}

/// Where a prologue left the callee-saved registers it stores.
#[derive(Clone, Debug, Default)]
pub struct Saves {
    /// Code offset from which they are saved.
    pub at: u32,
    /// `(DWARF register, offset from the CFA)`.
    pub regs: Vec<(u16, i32)>,
    /// Mach-O arm64: the compact unwind register-pair bits.
    pub compact: u32,
    /// Windows, a prologue saving registers: x64 its `UNWIND_INFO`, arm64
    /// its prolog's unwind codes (`end` included); empty for `PROLOGUE`.
    pub win: Vec<u8>,
    /// RISC-V: the frame's four instructions in their 16-bit forms.
    pub short: bool,
}

/// A section's bytes from one definition to the next: its own section in
/// ELF and COFF, so the linker drops unused ones.
pub struct Piece {
    pub sect: Sect,
    pub start: u32,
    pub end: u32,
    /// log2 of the alignment.
    pub align: u32,
    /// The symbols defined at `start`, the first one names the piece.
    pub syms: Vec<SymId>,
    /// Relocations, offsets from `start`.
    pub relocs: Vec<Reloc>,
}

#[derive(Default)]
#[allow(clippy::struct_excessive_bools)]
pub struct Obj {
    pub syms: Vec<Sym>,
    by_name: HashMap<String, SymId>,
    pub sects: HashMap<Sect, Section>,
    strings: HashMap<Vec<u8>, SymId>,
    /// `str_table`s by their literals and lengths.
    str_tables: HashMap<Vec<(SymId, u64)>, SymId>,
    pub fns: Vec<FnRange>,
    /// More sections: `(segment, section, flags, contents)` (debug info).
    pub extra: Vec<(&'static str, &'static str, u32, Section)>,
    /// `x86_64` code (else arm64).
    pub x86: bool,
    /// An ELF object (else Mach-O, or COFF).
    pub elf: bool,
    /// A COFF object (Windows: no GOT, TLS through `_tls_index`).
    pub coff: bool,
    /// Thread-locals by the dynamic model (Android: bionic refuses
    /// initial exec in a `dlopen`ed library), relaxed in an executable.
    pub dyn_tls: bool,
    /// i386 code (`x86` is false): 32-bit ELF / COFF, in-place addends.
    pub i386: bool,
    /// 32-bit ARM code (`x86` is false): ELF32, in-place addends.
    pub arm: bool,
    /// RISC-V code (`x86` is false; `rv32`: ELF32, else ELF64), RELA
    /// relocations.
    pub riscv: bool,
    pub rv32: bool,
    /// powerpc64 `ELFv2` code (little endian, RELA).
    pub ppc64: bool,
    /// s390x code (big endian, RELA).
    pub s390x: bool,
    /// `x86_64` instructions past the baseline the code may use
    /// (`-C target-cpu` / `-C target-feature`): `popcnt`, `lzcnt`,
    /// `tzcnt` (BMI1).
    pub popcnt: bool,
    pub lzcnt: bool,
    pub bmi: bool,
    /// x86-64 F16C: `f16` <-> `f32` conversions inline.
    pub f16c: bool,
    /// arm64 LSE atomics (ARMv8.1 `ldadd`, `swp`, `cas`); without them
    /// read-modify-writes are `ldaxr` / `stlxr` loops.
    pub lse: bool,
    /// arm64 Linux without LSE in the baseline, above `-O0`: each
    /// read-modify-write tests `nabi_cpu_lse` and takes LSE when the CPU
    /// has it, else the loop.
    pub outline: bool,
    /// RISC-V `Zbb` (rotates); without it a rotate is two shifts.
    pub zbb: bool,
    /// Vector loops on s390x (the z13 vector facility; `z14`: its
    /// enhancements) or RISC-V (V): what `--emit=asm` asks the
    /// assembler for.
    pub vector: bool,
    pub z14: bool,
    /// arm64 `ldapr` (`rcpc` acquire loads) and `sdot` / `udot`.
    pub rcpc: bool,
    pub dotprod: bool,
    /// Big-endian data.
    pub be: bool,
    /// 32-bit ARM: `(start, end)` text offsets of data in code (literal
    /// words), for the `$d` / `$a` mapping symbols.
    pub data_in_code: Vec<(u32, u32)>,
    /// A wasm32 object: its functions apart from the sections.
    pub wasm: Option<Box<crate::isa::wasm::Parts>>,
    /// `--emit=listing`: the functions' listings so far (`asm`); `None`:
    /// no encoder records text.
    pub asm: Option<String>,
    /// `--emit=asm` (`asm` set too): the functions as source so far.
    pub src: Option<Vec<crate::asm::code::FnSrc>>,
    /// A hot build's shadow-frame functions, by symbol, with their
    /// `FrameInfo` when they have one (`roots::frame`).
    pub hot_frames: HashMap<SymId, Option<SymId>>,
    /// Mach-O `LC_BUILD_VERSION`: platform and packed minimum OS;
    /// `None` is macOS 11.0.
    pub build_version: Option<(u32, u32)>,
}

impl Obj {
    #[must_use]
    pub fn new(x86: bool, elf: bool, coff: bool) -> Self {
        let mut o = Self {
            x86,
            elf,
            coff,
            ..Self::default()
        };
        for s in SECTS {
            o.sects.insert(s, Section::default());
        }
        o
    }

    /// An empty object in `t`'s format.
    #[must_use]
    pub fn of_target(t: &nabic_mir::TargetSpec) -> Self {
        let coff = t.object_format == nabic_mir::ObjectFormat::Coff;
        let macho = t.object_format == nabic_mir::ObjectFormat::MachO;
        let mut o = Self::new(t.arch == "x86_64", !coff && !macho, coff);
        o.build_version = nabic_target::apple::Platform::of(t).map(|p| {
            (
                p.id(),
                nabic_target::apple::pack_version(t.deployment_target()),
            )
        });
        o
    }

    /// Drop undefined symbols no relocation uses (a REPL cell declares
    /// every earlier function).
    pub fn drop_unreferenced(&mut self) {
        let refs = |r: &Reloc| r.kind != RelKind::SecOff32;
        let mut keep: Vec<bool> = self.syms.iter().map(|s| s.def.is_some()).collect();
        for s in self.sects.values().chain(self.extra.iter().map(|e| &e.3)) {
            for r in s.relocs.iter().filter(|r| refs(r)) {
                keep[r.sym as usize] = true;
            }
        }
        let mut map = vec![SymId::MAX; self.syms.len()];
        let mut n = 0;
        for (i, k) in keep.iter().enumerate() {
            if *k {
                map[i] = n;
                n += 1;
            }
        }
        if n as usize == self.syms.len() {
            return;
        }
        let mut i = 0;
        self.syms.retain(|_| {
            i += 1;
            keep[i - 1]
        });
        let sects = self.sects.values_mut();
        for s in sects.chain(self.extra.iter_mut().map(|e| &mut e.3)) {
            for r in s.relocs.iter_mut().filter(|r| refs(r)) {
                r.sym = map[r.sym as usize];
            }
        }
        for f in &mut self.fns {
            f.sym = map[f.sym as usize];
        }
        self.by_name.retain(|_, s| keep[*s as usize]);
        for s in self.by_name.values_mut().chain(self.strings.values_mut()) {
            *s = map[*s as usize];
        }
        self.str_tables = std::mem::take(&mut self.str_tables)
            .into_iter()
            .map(|(k, s)| {
                let k = k.into_iter().map(|(l, n)| (map[l as usize], n)).collect();
                (k, map[s as usize])
            })
            .collect();
    }

    /// The low `n` bytes of `v` in the target's byte order.
    #[must_use]
    pub fn int_bytes(&self, v: u64, n: usize) -> Vec<u8> {
        if self.be {
            v.to_be_bytes()[8 - n..].to_vec()
        } else {
            v.to_le_bytes()[..n].to_vec()
        }
    }

    /// A 32-bit object (pointers and addresses in 4 bytes).
    #[must_use]
    pub const fn c32(&self) -> bool {
        self.i386 || self.arm || self.rv32 || self.wasm.is_some()
    }

    /// Some code so far refers to `name` (or defines it).
    #[must_use]
    pub fn named(&self, name: &str) -> bool {
        self.by_name.contains_key(name)
    }

    pub fn sym(&mut self, name: &str) -> SymId {
        if let Some(s) = self.by_name.get(name) {
            return *s;
        }
        self.syms.push(Sym {
            name: name.to_owned(),
            def: None,
            global: true,
            weak: false,
            hidden: false,
        });
        let id = self.syms.len() as SymId - 1;
        self.by_name.insert(name.to_owned(), id);
        id
    }

    /// Every non-empty section split at its symbol definitions, and the
    /// piece of each defined symbol.
    #[must_use]
    pub fn pieces(&self) -> (Vec<Piece>, Vec<Option<usize>>) {
        let mut pieces: Vec<Piece> = Vec::new();
        let mut of = vec![None; self.syms.len()];
        for s in SECTS {
            let sec = &self.sects[&s];
            if sec.data.is_empty() {
                continue;
            }
            let mut defs: Vec<(u32, SymId)> = self
                .syms
                .iter()
                .enumerate()
                .filter_map(|(i, y)| y.def.filter(|d| d.0 == s).map(|d| (d.1, i as SymId)))
                .collect();
            defs.sort_unstable();
            let first = pieces.len();
            let mut prev = None;
            for (off, sym) in defs {
                if prev == Some(off) {
                    pieces.last_mut().expect("a piece").syms.push(sym);
                } else {
                    pieces.push(Piece {
                        sect: s,
                        // Bytes before the first definition go with it.
                        start: if prev.is_none() { 0 } else { off },
                        end: 0,
                        align: 0,
                        syms: vec![sym],
                        relocs: Vec::new(),
                    });
                }
                prev = Some(off);
                of[sym as usize] = Some(pieces.len() - 1);
            }
            let ps = &mut pieces[first..];
            for k in 0..ps.len() {
                ps[k].end = ps.get(k + 1).map_or(sec.data.len() as u32, |n| n.start);
                ps[k].align = sec.align.min(ps[k].start.trailing_zeros());
            }
            for r in &sec.relocs {
                let k = ps.partition_point(|p| p.start <= r.off) - 1;
                ps[k].relocs.push(Reloc {
                    off: r.off - ps[k].start,
                    ..*r
                });
            }
        }
        (pieces, of)
    }

    pub fn sect(&mut self, s: Sect) -> &mut Section {
        self.sects.get_mut(&s).expect("section")
    }

    /// Define `sym` at the end of `sect` with `bytes` (aligned to
    /// `align`); returns its offset.
    pub fn define_data(
        &mut self,
        sym: SymId,
        sect: Sect,
        bytes: &[u8],
        align: u32,
        global: bool,
    ) -> u32 {
        // s390x `larl` reaches even addresses only.
        let align = if self.s390x { align.max(2) } else { align };
        let s = self.sect(sect);
        let off = s.align_to(align);
        s.data.extend_from_slice(bytes);
        let sy = &mut self.syms[sym as usize];
        sy.def = Some((sect, off, bytes.len() as u32));
        sy.global = global;
        off
    }

    /// A pointer-sized relocated word at `off` of `sect`.
    pub fn abs64(&mut self, sect: Sect, off: u32, target: SymId) {
        self.sect(sect).relocs.push(Reloc {
            off,
            kind: RelKind::Abs64,
            sym: target,
        });
    }

    /// A read-only table of `str`s (`pw`-byte words): each the address
    /// of its literal in `lits` and its length, one per content.
    pub fn str_table(&mut self, lits: &[SymId], lens: &[u64], pw: u32) -> SymId {
        let key: Vec<(SymId, u64)> = lits.iter().copied().zip(lens.iter().copied()).collect();
        if let Some(s) = self.str_tables.get(&key) {
            return *s;
        }
        let mut names = Vec::new();
        let mut bytes = Vec::with_capacity(lits.len() * 2 * pw as usize);
        for (l, n) in &key {
            names.extend_from_slice(self.syms[*l as usize].name.as_bytes());
            names.push(0);
            names.extend_from_slice(&n.to_le_bytes());
            bytes.extend(std::iter::repeat_n(0, pw as usize));
            bytes.extend(self.int_bytes(*n, pw as usize));
        }
        let name = nabic_codegen::mangle::str_global(&names).replacen("nabi.str.", "nabi.strs.", 1);
        let sym = self.sym(&name);
        let off = self.define_data(sym, Sect::DataConst, &bytes, pw, false);
        for (i, (l, _)) in key.iter().enumerate() {
            self.abs64(Sect::DataConst, off + i as u32 * 2 * pw, *l);
        }
        self.str_tables.insert(key, sym);
        sym
    }

    /// Constant bytes and a NUL (a string literal, a constant array),
    /// one per content.
    pub fn bytes(&mut self, content: &[u8]) -> SymId {
        if let Some(s) = self.strings.get(content) {
            return *s;
        }
        let name = nabic_codegen::mangle::str_global(content);
        let sym = self.sym(&name);
        let mut bytes = content.to_vec();
        bytes.push(0);
        self.define_data(sym, Sect::Const, &bytes, 1, false);
        self.strings.insert(content.to_vec(), sym);
        sym
    }
}
