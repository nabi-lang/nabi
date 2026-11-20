//! ELF relocations: the dispatch over `crate::elf::arch`, and the
//! `x86_64`, aarch64 and i386 cases, which share one body.
//!
//! What each needs from the link (GOT and TLS GOT slots, PLT entries,
//! dynamic relocations) and how it is applied, with the GOT and TLS
//! relaxations lld does for executables.

use crate::elf::Arch;
use crate::elf::arch::a64::{adr, imm19, movw, movw_signed};
use crate::elf::arch::{a64, i386, x64};
use crate::macho::a64::{branch26, page21, pageoff12};

/// The dynamic relocation types of `arch`: `(RELATIVE, GLOB_DAT,
/// symbolic 64-bit, TPOFF64)`.
#[must_use]
pub const fn dyn_types(arch: Arch) -> (u32, u32, u32, u32) {
    match arch {
        Arch::X86_64 => (x64::RELATIVE, x64::GLOB_DAT, 1, 18),
        Arch::Aarch64 => (a64::RELATIVE, a64::GLOB_DAT, 257, a64::TLS_TPREL64),
        Arch::I386 => (i386::RELATIVE, i386::GLOB_DAT, 1, i386::TLS_TPOFF),
        Arch::Arm => (
            crate::elf::arch::arm32::RELATIVE,
            crate::elf::arch::arm32::GLOB_DAT,
            2,
            crate::elf::arch::arm32::TLS_TPOFF32,
        ),
        Arch::Riscv64 => (
            crate::elf::arch::riscv::RELATIVE,
            2,
            2,
            crate::elf::arch::riscv::TLS_TPREL64,
        ),
        Arch::Riscv32 => (
            crate::elf::arch::riscv::RELATIVE,
            1,
            1,
            crate::elf::arch::riscv::TLS_TPREL32,
        ),
        Arch::Ppc64 => (
            crate::elf::arch::ppc64::RELATIVE,
            crate::elf::arch::ppc64::GLOB_DAT,
            crate::elf::arch::ppc64::ADDR64 as u32,
            crate::elf::arch::ppc64::TPREL64 as u32,
        ),
        Arch::S390x => (
            crate::elf::arch::s390x::RELATIVE,
            crate::elf::arch::s390x::GLOB_DAT,
            crate::elf::arch::s390x::R_64 as u32,
            crate::elf::arch::s390x::TLS_TPOFF,
        ),
    }
}

/// A relocation that refers to a GOT or TLS slot of its symbol: a
/// local target gets a linker-local symbol to key the slot.
#[must_use]
pub const fn needs_symbol(arch: Arch, ty: u16) -> bool {
    match arch {
        Arch::X86_64 => matches!(
            ty,
            x64::GOT32
                | x64::GOTPCREL
                | x64::GOTPCRELX
                | x64::REX_GOTPCRELX
                | x64::GOT64
                | x64::GOTPCREL64
                | x64::GOTTPOFF
                | x64::TLSGD
                | x64::GOTPC32_TLSDESC
        ),
        Arch::Aarch64 => matches!(ty, 309..=313 | 512..=543 | 560..=569),
        Arch::Arm => crate::elf::arch::arm32::needs_symbol(ty),
        Arch::Riscv64 | Arch::Riscv32 => crate::elf::arch::riscv::needs_symbol(ty),
        Arch::Ppc64 => crate::elf::arch::ppc64::needs_symbol(ty),
        Arch::S390x => crate::elf::arch::s390x::needs_symbol(ty),
        Arch::I386 => matches!(
            ty,
            i386::GOT32
                | i386::GOT32X
                | i386::TLS_GD
                | i386::TLS_IE
                | i386::TLS_GOTIE
                | i386::TLS_GOTDESC
        ),
    }
}

/// A general dynamic or descriptor access.
///
/// Relaxed to local exec for an executable's own thread-local, which a
/// shared object cannot do (its block's place is the loader's to say):
/// there it takes the initial exec form, as for another object's.
#[must_use]
pub const fn dynamic_tls(arch: Arch, ty: u16) -> bool {
    match arch {
        Arch::X86_64 => matches!(ty, x64::TLSGD | x64::GOTPC32_TLSDESC | x64::TLSDESC_CALL),
        Arch::Aarch64 => matches!(
            ty,
            a64::TLSDESC_ADR_PAGE21
                | a64::TLSDESC_LD64_LO12
                | a64::TLSDESC_ADD_LO12
                | a64::TLSDESC_CALL
        ),
        Arch::I386 => matches!(ty, i386::TLS_GD | i386::TLS_GOTDESC | i386::TLS_DESC_CALL),
        _ => false,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SymKind {
    /// Defined in the executable.
    Local,
    /// An absolute value.
    Abs,
    /// Defined by a shared object.
    Dso,
    /// Undefined weak: zero.
    UndefWeak,
}

#[derive(Clone, Copy, Debug, Default)]
#[allow(clippy::struct_excessive_bools)]
pub struct Need {
    pub got: bool,
    pub tlsgot: bool,
    /// GOT slot pairs for `__tls_get_addr`: module and offset of the
    /// symbol (general dynamic) / of the module's block (local dynamic).
    pub tlsgd: bool,
    pub tlsld: bool,
    pub plt: bool,
    /// A `RELATIVE` (local) or symbolic (shared object) dynamic
    /// relocation at the place.
    pub dynrel: bool,
    /// `_GLOBAL_OFFSET_TABLE_` must exist.
    pub got_base: bool,
}

/// What the link must provide for a relocation; the context.
#[derive(Clone, Copy, Debug)]
#[allow(clippy::struct_excessive_bools)]
pub struct Site {
    pub arch: Arch,
    pub ty: u16,
    pub kind: SymKind,
    /// The target is a function (a PLT entry stands for it).
    pub func: bool,
    /// The place is in an allocated section.
    pub alloc: bool,
    pub pie: bool,
    /// A shared object's own thread-local, reached through the loader
    /// (`--nabi-dynamic-tls`): the sequence stays as it is, its GOT pair
    /// filled at load.
    pub dyn_tls: bool,
}

#[must_use]
pub fn type_name(arch: Arch, ty: u16) -> String {
    let n = match (arch, ty) {
        (Arch::X86_64, x64::R_64) => "R_X86_64_64",
        (Arch::X86_64, x64::PC32) => "R_X86_64_PC32",
        (Arch::X86_64, x64::PLT32) => "R_X86_64_PLT32",
        (Arch::X86_64, x64::R_32) => "R_X86_64_32",
        (Arch::X86_64, x64::R_32S) => "R_X86_64_32S",
        (Arch::X86_64, x64::GOTPCREL) => "R_X86_64_GOTPCREL",
        (Arch::X86_64, x64::GOTPCRELX) => "R_X86_64_GOTPCRELX",
        (Arch::X86_64, x64::REX_GOTPCRELX) => "R_X86_64_REX_GOTPCRELX",
        (Arch::X86_64, x64::TPOFF32) => "R_X86_64_TPOFF32",
        (Arch::X86_64, x64::GOTTPOFF) => "R_X86_64_GOTTPOFF",
        (Arch::X86_64, x64::TLSGD) => "R_X86_64_TLSGD",
        (Arch::X86_64, x64::TLSLD) => "R_X86_64_TLSLD",
        (Arch::Aarch64, a64::ABS64) => "R_AARCH64_ABS64",
        (Arch::Aarch64, a64::ABS32) => "R_AARCH64_ABS32",
        (Arch::Aarch64, a64::PREL32) => "R_AARCH64_PREL32",
        (Arch::Aarch64, a64::CALL26) => "R_AARCH64_CALL26",
        (Arch::Aarch64, a64::JUMP26) => "R_AARCH64_JUMP26",
        (Arch::Aarch64, a64::ADR_PREL_PG_HI21) => "R_AARCH64_ADR_PREL_PG_HI21",
        (Arch::Aarch64, a64::ADD_ABS_LO12_NC) => "R_AARCH64_ADD_ABS_LO12_NC",
        (Arch::Aarch64, a64::ADR_GOT_PAGE) => "R_AARCH64_ADR_GOT_PAGE",
        (Arch::Aarch64, a64::LD64_GOT_LO12_NC) => "R_AARCH64_LD64_GOT_LO12_NC",
        (Arch::I386, i386::R_32) => "R_386_32",
        (Arch::I386, i386::PC32) => "R_386_PC32",
        (Arch::I386, i386::GOT32) => "R_386_GOT32",
        (Arch::I386, i386::GOT32X) => "R_386_GOT32X",
        (Arch::I386, i386::PLT32) => "R_386_PLT32",
        (Arch::I386, i386::GOTOFF) => "R_386_GOTOFF",
        (Arch::I386, i386::GOTPC) => "R_386_GOTPC",
        (Arch::I386, i386::R_16) => "R_386_16",
        (Arch::I386, i386::PC16) => "R_386_PC16",
        (Arch::I386, i386::R_8) => "R_386_8",
        (Arch::I386, i386::PC8) => "R_386_PC8",
        (Arch::I386, i386::TLS_IE) => "R_386_TLS_IE",
        (Arch::I386, i386::TLS_GOTIE) => "R_386_TLS_GOTIE",
        (Arch::I386, i386::TLS_LE) => "R_386_TLS_LE",
        (Arch::I386, i386::TLS_LE_32) => "R_386_TLS_LE_32",
        (Arch::I386, i386::TLS_GD) => "R_386_TLS_GD",
        (Arch::I386, i386::TLS_LDM) => "R_386_TLS_LDM",
        (Arch::I386, i386::TLS_LDO_32) => "R_386_TLS_LDO_32",
        (Arch::I386, i386::TLS_DTPOFF32) => "R_386_TLS_DTPOFF32",
        (Arch::I386, i386::TLS_GOTDESC) => "R_386_TLS_GOTDESC",
        (Arch::I386, i386::TLS_DESC_CALL) => "R_386_TLS_DESC_CALL",
        (Arch::I386, i386::SIZE32) => "R_386_SIZE32",
        (Arch::Arm, _) => match crate::elf::arch::arm32::type_name(ty) {
            Some(n) => n,
            None => return format!("type {ty}"),
        },
        (Arch::Riscv64 | Arch::Riscv32, _) => match crate::elf::arch::riscv::type_name(ty) {
            Some(n) => n,
            None => return format!("type {ty}"),
        },
        (Arch::Ppc64, _) => match crate::elf::arch::ppc64::type_name(ty) {
            Some(n) => n,
            None => return format!("type {ty}"),
        },
        (Arch::S390x, _) => match crate::elf::arch::s390x::type_name(ty) {
            Some(n) => n,
            None => return format!("type {ty}"),
        },
        _ => return format!("type {ty}"),
    };
    n.to_owned()
}

/// [`x64::relax_got`] or [`i386::relax_got`] for `arch`.
#[must_use]
pub fn relax_got(arch: Arch, b: &[u8], at: usize, ty: u16, kind: SymKind) -> bool {
    match arch {
        Arch::X86_64 => x64::relax_got(b, at, ty, kind),
        Arch::I386 => i386::relax_got(b, at, ty, kind),
        _ => false,
    }
}

/// What site `s` needs; `relax` is [`x64::relax_got`]'s answer. An error
/// for relocations the output cannot express.
#[allow(clippy::match_same_arms)]
pub fn need(s: Site, relax: bool) -> Result<Need, String> {
    let mut n = Need::default();
    let name = || type_name(s.arch, s.ty);
    let dso = s.kind == SymKind::Dso;
    let pic_err = |what: &str| {
        Err(format!(
            "relocation {} cannot be used against {what}; recompile with -fPIC",
            name()
        ))
    };
    let pc_to_dso = |n: &mut Need| {
        if s.func {
            n.plt = true;
            Ok(())
        } else {
            Err(format!(
                "relocation {} against data of a shared object needs a copy relocation, \
                 which Nabi's linker does not make; recompile with -fPIC",
                name()
            ))
        }
    };
    match s.arch {
        Arch::Arm => return crate::elf::arch::arm32::need(s, &name()),
        Arch::Riscv64 | Arch::Riscv32 => {
            return crate::elf::arch::riscv::need(s, &name(), s.arch == Arch::Riscv64);
        }
        Arch::Ppc64 => return crate::elf::arch::ppc64::need(s, &name()),
        Arch::S390x => return crate::elf::arch::s390x::need(s, &name()),
        Arch::X86_64 => match s.ty {
            x64::R_64 => {
                n.dynrel = s.alloc && (dso || (s.pie && s.kind == SymKind::Local));
            }
            x64::R_32 | x64::R_32S | x64::R_16 | x64::R_8 => {
                if dso {
                    return pic_err("a shared object's symbol");
                }
                if s.alloc && s.pie && s.kind == SymKind::Local {
                    return pic_err("a local symbol in a PIE");
                }
            }
            x64::PC32 | x64::PC64 | x64::PC16 | x64::PC8 if dso => pc_to_dso(&mut n)?,
            x64::PLT32 if dso => n.plt = true,
            x64::PC32 | x64::PC64 | x64::PC16 | x64::PC8 | x64::PLT32 => {}
            x64::GOTPCRELX | x64::REX_GOTPCRELX if relax => {}
            x64::GOTPCREL | x64::GOTPCRELX | x64::REX_GOTPCRELX => n.got = true,
            x64::GOTPC32 | x64::GOTPC64 | x64::GOTOFF64 => n.got_base = true,
            x64::SIZE32 | x64::SIZE64 | x64::DTPOFF64 => {}
            x64::TPOFF32 | x64::TPOFF64 | x64::DTPOFF32 | x64::TLSLD => {
                if dso {
                    return Err(format!(
                        "relocation {} against a shared object's thread-local",
                        name()
                    ));
                }
            }
            x64::GOTTPOFF => n.tlsgot = true,
            x64::TLSGD if s.dyn_tls => n.tlsgd = true,
            x64::TLSGD | x64::GOTPC32_TLSDESC => n.tlsgot = dso,
            x64::TLSDESC_CALL => {}
            _ => return Err(format!("unsupported relocation {}", name())),
        },
        Arch::Aarch64 => match s.ty {
            a64::ABS64 => {
                n.dynrel = s.alloc && (dso || (s.pie && s.kind == SymKind::Local));
            }
            a64::ABS32 | a64::ABS16 | a64::MOVW_UABS_G0..=a64::MOVW_UABS_G3 | 270..=272 => {
                if dso {
                    return pic_err("a shared object's symbol");
                }
                if s.alloc && s.pie && s.kind == SymKind::Local {
                    return pic_err("a local symbol in a PIE");
                }
            }
            a64::CALL26 | a64::JUMP26 if dso => n.plt = true,
            a64::PREL64
            | a64::PREL32
            | a64::PREL16
            | a64::LD_PREL_LO19
            | a64::ADR_PREL_LO21
            | a64::ADR_PREL_PG_HI21
            | a64::ADR_PREL_PG_HI21_NC
            | a64::ADD_ABS_LO12_NC
            | a64::LDST8_ABS_LO12_NC
            | a64::LDST16_ABS_LO12_NC
            | a64::LDST32_ABS_LO12_NC
            | a64::LDST64_ABS_LO12_NC
            | a64::LDST128_ABS_LO12_NC
                if dso =>
            {
                pc_to_dso(&mut n)?;
            }
            a64::PREL64
            | a64::PREL32
            | a64::PREL16
            | a64::LD_PREL_LO19
            | a64::ADR_PREL_LO21
            | a64::ADR_PREL_PG_HI21
            | a64::ADR_PREL_PG_HI21_NC
            | a64::ADD_ABS_LO12_NC
            | a64::LDST8_ABS_LO12_NC
            | a64::LDST16_ABS_LO12_NC
            | a64::LDST32_ABS_LO12_NC
            | a64::LDST64_ABS_LO12_NC
            | a64::LDST128_ABS_LO12_NC
            | a64::TSTBR14
            | a64::CONDBR19
            | a64::CALL26
            | a64::JUMP26 => {}
            a64::GOT_LD_PREL19 | a64::ADR_GOT_PAGE | a64::LD64_GOT_LO12_NC => n.got = true,
            a64::TLSIE_ADR_GOTTPREL_PAGE21
            | a64::TLSIE_LD64_GOTTPREL_LO12_NC
            | a64::TLSIE_LD_GOTTPREL_PREL19 => n.tlsgot = true,
            a64::TLSLE_MOVW_TPREL_G2..=a64::TLSLE_LDST64_TPREL_LO12_NC
            | a64::TLSLE_LDST128_TPREL_LO12
            | a64::TLSLE_LDST128_TPREL_LO12_NC => {
                if dso {
                    return Err(format!(
                        "relocation {} against a shared object's thread-local",
                        name()
                    ));
                }
            }
            a64::TLSDESC_ADR_PAGE21 | a64::TLSDESC_LD64_LO12 | a64::TLSDESC_ADD_LO12
                if s.dyn_tls =>
            {
                n.tlsgd = true;
            }
            a64::TLSDESC_ADR_PAGE21
            | a64::TLSDESC_LD64_LO12
            | a64::TLSDESC_ADD_LO12
            | a64::TLSDESC_CALL => n.tlsgot = dso,
            a64::TLS_DTPREL64 => {}
            _ => return Err(format!("unsupported relocation {}", name())),
        },
        Arch::I386 => match s.ty {
            i386::R_32 => {
                n.dynrel = s.alloc && (dso || (s.pie && s.kind == SymKind::Local));
            }
            i386::R_16 | i386::R_8 => {
                if dso {
                    return pic_err("a shared object's symbol");
                }
                if s.alloc && s.pie && s.kind == SymKind::Local {
                    return pic_err("a local symbol in a PIE");
                }
            }
            i386::PC32 | i386::PC16 | i386::PC8 if dso => pc_to_dso(&mut n)?,
            i386::PLT32 if dso => n.plt = true,
            i386::PC32 | i386::PC16 | i386::PC8 | i386::PLT32 | i386::SIZE32 => {}
            i386::GOT32 | i386::GOT32X if relax => n.got_base = true,
            i386::GOT32 | i386::GOT32X => {
                n.got = true;
                n.got_base = true;
            }
            i386::GOTOFF if dso => return pic_err("a shared object's symbol"),
            i386::GOTOFF | i386::GOTPC => n.got_base = true,
            i386::TLS_LE
            | i386::TLS_LE_32
            | i386::TLS_LDO_32
            | i386::TLS_DTPOFF32
            | i386::TLS_LDM
                if dso =>
            {
                return Err(format!(
                    "relocation {} against a shared object's thread-local",
                    name()
                ));
            }
            i386::TLS_LE | i386::TLS_LE_32 | i386::TLS_LDO_32 | i386::TLS_DTPOFF32 => {}
            i386::TLS_IE if dso => return pic_err("a shared object's thread-local"),
            i386::TLS_IE | i386::TLS_DESC_CALL => {}
            i386::TLS_GD | i386::TLS_LDM | i386::TLS_GOTIE | i386::TLS_GOTDESC => {
                n.tlsgot = dso;
                n.got_base = true;
            }
            _ => return Err(format!("unsupported relocation {}", name())),
        },
    }
    Ok(n)
}

/// Addresses a relocation resolves with.
#[derive(Clone, Copy, Debug, Default)]
pub struct Values {
    /// S: the target (a shared object's function: its PLT entry).
    pub s: u64,
    pub a: i64,
    /// P: the place.
    pub p: u64,
    /// The GOT slot, the TLS GOT slot, `_GLOBAL_OFFSET_TABLE_`.
    pub g: u64,
    pub tg: u64,
    pub got_base: u64,
    /// S's offset from the thread pointer, and in its TLS block.
    pub tp: i64,
    pub dtp: i64,
    /// The symbol's size.
    pub size: u64,
}

/// The dynamic relocation [`apply`] asks for at the place.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Dyn {
    None,
    /// `RELATIVE` with this addend.
    Relative(u64),
    /// Symbolic 64-bit with this addend.
    Symbolic(i64),
}

pub(crate) fn range(name: &str, v: i64, bits: u32, signed: bool) -> Result<(), String> {
    let ok = if bits >= 64 {
        true
    } else if signed {
        v >= -(1i64 << (bits - 1)) && v < (1i64 << (bits - 1))
    } else {
        // Unsigned fields also take negative values that fit signed.
        v >= -(1i64 << (bits - 1)) && (v < 0 || (v as u64) < (1u64 << bits))
    };
    if ok {
        Ok(())
    } else {
        Err(format!(
            "relocation {name} out of range: {v:#x} does not fit {bits} bits"
        ))
    }
}

pub(crate) fn put(b: &mut [u8], at: usize, v: u64, n: usize) {
    b[at..at + n].copy_from_slice(&v.to_le_bytes()[..n]);
}

const fn insn(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn set(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

/// Apply site `s` at `b[at..]` with `v`; the dynamic relocation the
/// place needs.
pub fn apply(s: Site, b: &mut [u8], at: usize, v: &Values) -> Result<Dyn, String> {
    apply_hi(s, b, at, v, None)
}

/// [`apply`], `hi` the value a RISC-V `PCREL_LO12_*` takes.
#[allow(clippy::too_many_lines)]
pub fn apply_hi(
    s: Site,
    b: &mut [u8],
    at: usize,
    v: &Values,
    hi: Option<i64>,
) -> Result<Dyn, String> {
    let relax = relax_got(s.arch, b, at, s.ty, s.kind);
    let n = need(s, relax)?;
    let name = type_name(s.arch, s.ty);
    if matches!(
        s.arch,
        Arch::Riscv64 | Arch::Riscv32 | Arch::Ppc64 | Arch::S390x
    ) {
        let sa = (v.s as i64).wrapping_add(v.a);
        let dynrel = if !n.dynrel {
            Dyn::None
        } else if s.kind == SymKind::Dso {
            Dyn::Symbolic(v.a)
        } else {
            Dyn::Relative(sa as u64)
        };
        return match s.arch {
            Arch::Ppc64 => crate::elf::arch::ppc64::apply(s, b, at, v, dynrel, &name),
            Arch::S390x => crate::elf::arch::s390x::apply(s, b, at, v, dynrel, &name),
            _ => crate::elf::arch::riscv::apply(s, b, at, v, dynrel, &name, hi),
        };
    }
    let sa = (v.s as i64).wrapping_add(v.a);
    let pc = sa.wrapping_sub(v.p as i64);
    let width = match (s.arch, s.ty) {
        (Arch::X86_64, x64::R_8 | x64::PC8) | (Arch::I386, i386::R_8 | i386::PC8) => 1,
        (Arch::X86_64, x64::R_16 | x64::PC16)
        | (Arch::Aarch64, a64::ABS16 | a64::PREL16)
        | (Arch::I386, i386::R_16 | i386::PC16) => 2,
        (
            Arch::X86_64,
            x64::R_64
            | x64::PC64
            | x64::GOTPC64
            | x64::GOTOFF64
            | x64::SIZE64
            | x64::TPOFF64
            | x64::DTPOFF64,
        )
        | (Arch::Aarch64, a64::ABS64 | a64::PREL64 | a64::TLS_DTPREL64) => 8,
        _ => 4,
    };
    if b.len() < at + width {
        return Err(format!("relocation {name} runs past its section"));
    }
    let dynrel = if !n.dynrel {
        Dyn::None
    } else if s.kind == SymKind::Dso {
        Dyn::Symbolic(v.a)
    } else {
        Dyn::Relative(sa as u64)
    };
    match s.arch {
        Arch::X86_64 => match s.ty {
            x64::R_64 => put(b, at, sa as u64, 8),
            x64::R_32 => {
                range(&name, sa, 32, false)?;
                put(b, at, sa as u64, 4);
            }
            x64::R_32S => {
                range(&name, sa, 32, true)?;
                put(b, at, sa as u64, 4);
            }
            x64::R_16 => {
                range(&name, sa, 16, false)?;
                put(b, at, sa as u64, 2);
            }
            x64::R_8 => {
                range(&name, sa, 8, false)?;
                put(b, at, sa as u64, 1);
            }
            x64::PC32 | x64::PLT32 => {
                range(&name, pc, 32, true)?;
                put(b, at, pc as u64, 4);
            }
            x64::PC64 => put(b, at, pc as u64, 8),
            x64::PC16 => {
                range(&name, pc, 16, true)?;
                put(b, at, pc as u64, 2);
            }
            x64::PC8 => {
                range(&name, pc, 8, true)?;
                put(b, at, pc as u64, 1);
            }
            x64::GOTPCRELX | x64::REX_GOTPCRELX if relax => {
                b[at - 2] = 0x8d;
                range(&name, pc, 32, true)?;
                put(b, at, pc as u64, 4);
            }
            x64::GOTPCREL | x64::GOTPCRELX | x64::REX_GOTPCRELX => {
                let d = (v.g as i64).wrapping_add(v.a).wrapping_sub(v.p as i64);
                range(&name, d, 32, true)?;
                put(b, at, d as u64, 4);
            }
            x64::GOTPC32 => {
                let d = (v.got_base as i64)
                    .wrapping_add(v.a)
                    .wrapping_sub(v.p as i64);
                range(&name, d, 32, true)?;
                put(b, at, d as u64, 4);
            }
            x64::GOTPC64 => {
                let d = (v.got_base as i64)
                    .wrapping_add(v.a)
                    .wrapping_sub(v.p as i64);
                put(b, at, d as u64, 8);
            }
            x64::GOTOFF64 => put(b, at, sa.wrapping_sub(v.got_base as i64) as u64, 8),
            x64::SIZE32 => put(b, at, (v.size as i64 + v.a) as u64, 4),
            x64::SIZE64 => put(b, at, (v.size as i64 + v.a) as u64, 8),
            x64::TPOFF32 | x64::DTPOFF32 => {
                let t = v.tp + v.a;
                range(&name, t, 32, true)?;
                put(b, at, t as u64, 4);
            }
            x64::TPOFF64 => put(b, at, (v.tp + v.a) as u64, 8),
            x64::DTPOFF64 => put(b, at, (v.dtp + v.a) as u64, 8),
            x64::GOTTPOFF => {
                let d = (v.tg as i64).wrapping_add(v.a).wrapping_sub(v.p as i64);
                range(&name, d, 32, true)?;
                put(b, at, d as u64, 4);
            }
            x64::TLSGD => {
                // data16 lea x@tlsgd(%rip), %rdi; data16 data16 rex64
                // call __tls_get_addr@plt.
                if at < 4
                    || b.len() < at + 12
                    || b[at - 4..at] != [0x66, 0x48, 0x8d, 0x3d]
                    || b[at + 4..at + 8] != [0x66, 0x66, 0x48, 0xe8]
                {
                    return Err(format!("{name}: unsupported general dynamic TLS sequence"));
                }
                if s.dyn_tls {
                    // As it is: the GOT pair's address for the call.
                    let d = (v.tg as i64).wrapping_add(v.a).wrapping_sub(v.p as i64);
                    range(&name, d, 32, true)?;
                    put(b, at, d as u64, 4);
                } else if s.kind == SymKind::Dso {
                    // mov %fs:0, %rax; add x@gottpoff(%rip), %rax
                    b[at - 4..at + 8].copy_from_slice(&[
                        0x64, 0x48, 0x8b, 0x04, 0x25, 0, 0, 0, 0, 0x48, 0x03, 0x05,
                    ]);
                    let d = (v.tg as i64) - (v.p as i64 + 12);
                    range(&name, d, 32, true)?;
                    put(b, at + 8, d as u64, 4);
                } else {
                    // mov %fs:0, %rax; lea x@tpoff(%rax), %rax
                    b[at - 4..at + 8].copy_from_slice(&[
                        0x64, 0x48, 0x8b, 0x04, 0x25, 0, 0, 0, 0, 0x48, 0x8d, 0x80,
                    ]);
                    let t = v.tp + v.a + 4;
                    range(&name, t, 32, true)?;
                    put(b, at + 8, t as u64, 4);
                }
            }
            x64::TLSLD => {
                // lea x@tlsld(%rip), %rdi; call __tls_get_addr@plt
                if at < 3
                    || b.len() < at + 9
                    || b[at - 3..at] != [0x48, 0x8d, 0x3d]
                    || b[at + 4] != 0xe8
                {
                    return Err(format!("{name}: unsupported local dynamic TLS sequence"));
                }
                // data16 data16 data16 mov %fs:0, %rax
                b[at - 3..at + 9]
                    .copy_from_slice(&[0x66, 0x66, 0x66, 0x64, 0x48, 0x8b, 0x04, 0x25, 0, 0, 0, 0]);
            }
            x64::GOTPC32_TLSDESC => {
                // lea x@tlsdesc(%rip), %reg
                if at < 3 || b[at - 2] != 0x8d {
                    return Err(format!("{name}: unsupported TLS descriptor sequence"));
                }
                if s.kind == SymKind::Dso {
                    // mov x@gottpoff(%rip), %reg
                    b[at - 2] = 0x8b;
                    let d = (v.tg as i64).wrapping_add(v.a).wrapping_sub(v.p as i64);
                    range(&name, d, 32, true)?;
                    put(b, at, d as u64, 4);
                } else {
                    // mov $x@tpoff, %reg
                    let rex = b[at - 3];
                    b[at - 3] = 0x48 | ((rex >> 2) & 1);
                    b[at - 2] = 0xc7;
                    b[at - 1] = 0xc0 | ((b[at - 1] >> 3) & 7);
                    range(&name, v.tp, 32, true)?;
                    put(b, at, v.tp as u64, 4);
                }
            }
            x64::TLSDESC_CALL => {
                // call *(%rax) -> xchg %ax, %ax
                if b.len() < at + 2 || b[at] != 0xff || b[at + 1] != 0x10 {
                    return Err(format!("{name}: unsupported TLS descriptor call"));
                }
                b[at] = 0x66;
                b[at + 1] = 0x90;
            }
            _ => return Err(format!("unsupported relocation {name}")),
        },
        Arch::Aarch64 => {
            let p = v.p;
            let i = if b.len() >= at + 4 { insn(b, at) } else { 0 };
            let sa_u = sa as u64;
            match s.ty {
                a64::ABS64 => put(b, at, sa_u, 8),
                a64::ABS32 => {
                    range(&name, sa, 32, false)?;
                    put(b, at, sa_u, 4);
                }
                a64::ABS16 => {
                    range(&name, sa, 16, false)?;
                    put(b, at, sa_u, 2);
                }
                a64::PREL64 => put(b, at, pc as u64, 8),
                a64::PREL32 => {
                    range(&name, pc, 32, false)?;
                    put(b, at, pc as u64, 4);
                }
                a64::PREL16 => {
                    range(&name, pc, 16, false)?;
                    put(b, at, pc as u64, 2);
                }
                263..=269 => {
                    let k = u32::from(s.ty - a64::MOVW_UABS_G0) / 2;
                    let nc = s.ty != a64::MOVW_UABS_G3 && (s.ty - a64::MOVW_UABS_G0) % 2 == 1;
                    if !nc && k < 3 {
                        range(&name, sa, 16 * (k + 1), false)?;
                    }
                    set(b, at, movw(i, (sa_u >> (16 * k)) as u32));
                }
                270..=272 => {
                    let k = u32::from(s.ty - 270);
                    range(&name, sa, 16 * (k + 1) + 1, true)?;
                    set(b, at, movw_signed(i, (sa >> (16 * k)) as u32));
                }
                a64::LD_PREL_LO19 => set(b, at, imm19(&name, i, pc)?),
                a64::ADR_PREL_LO21 => {
                    range(&name, pc, 21, true)?;
                    set(b, at, adr(i, pc as u32));
                }
                a64::ADR_PREL_PG_HI21 | a64::ADR_PREL_PG_HI21_NC => {
                    let t = if s.kind == SymKind::UndefWeak {
                        p
                    } else {
                        sa_u
                    };
                    set(b, at, page21(i, p, t).map_err(|e| format!("{name}: {e}"))?);
                }
                a64::ADD_ABS_LO12_NC
                | a64::LDST8_ABS_LO12_NC
                | a64::LDST16_ABS_LO12_NC
                | a64::LDST32_ABS_LO12_NC
                | a64::LDST64_ABS_LO12_NC
                | a64::LDST128_ABS_LO12_NC => {
                    set(
                        b,
                        at,
                        pageoff12(i, sa_u).map_err(|e| format!("{name}: {e}"))?,
                    );
                }
                a64::TSTBR14 | a64::CONDBR19 | a64::CALL26 | a64::JUMP26 => {
                    // An undefined weak function: the next instruction.
                    let t = if s.kind == SymKind::UndefWeak {
                        p.wrapping_add(4)
                    } else {
                        sa_u
                    };
                    let d = t.wrapping_sub(p) as i64;
                    let w = match s.ty {
                        a64::TSTBR14 => {
                            range(&name, d, 16, true)?;
                            (i & !(0x3fff << 5)) | ((((d >> 2) as u32) & 0x3fff) << 5)
                        }
                        a64::CONDBR19 => imm19(&name, i, d)?,
                        _ => branch26(i, p, t).map_err(|e| format!("{name}: {e}"))?,
                    };
                    set(b, at, w);
                }
                a64::GOT_LD_PREL19 => {
                    let d = (v.g as i64).wrapping_add(v.a).wrapping_sub(p as i64);
                    set(b, at, imm19(&name, i, d)?);
                }
                a64::ADR_GOT_PAGE => {
                    let g = (v.g as i64).wrapping_add(v.a) as u64;
                    set(b, at, page21(i, p, g).map_err(|e| format!("{name}: {e}"))?);
                }
                a64::LD64_GOT_LO12_NC => {
                    let g = (v.g as i64).wrapping_add(v.a) as u64;
                    set(b, at, pageoff12(i, g).map_err(|e| format!("{name}: {e}"))?);
                }
                a64::TLSIE_ADR_GOTTPREL_PAGE21 => {
                    let g = (v.tg as i64).wrapping_add(v.a) as u64;
                    set(b, at, page21(i, p, g).map_err(|e| format!("{name}: {e}"))?);
                }
                a64::TLSIE_LD64_GOTTPREL_LO12_NC => {
                    let g = (v.tg as i64).wrapping_add(v.a) as u64;
                    set(b, at, pageoff12(i, g).map_err(|e| format!("{name}: {e}"))?);
                }
                a64::TLSIE_LD_GOTTPREL_PREL19 => {
                    let d = (v.tg as i64).wrapping_add(v.a).wrapping_sub(p as i64);
                    set(b, at, imm19(&name, i, d)?);
                }
                a64::TLSLE_MOVW_TPREL_G2..=a64::TLSLE_LDST64_TPREL_LO12_NC
                | a64::TLSLE_LDST128_TPREL_LO12
                | a64::TLSLE_LDST128_TPREL_LO12_NC => {
                    let t = v.tp + v.a;
                    let w = match s.ty {
                        a64::TLSLE_MOVW_TPREL_G2 => {
                            range(&name, t, 49, true)?;
                            movw_signed(i, (t >> 32) as u32)
                        }
                        a64::TLSLE_MOVW_TPREL_G1 => {
                            range(&name, t, 33, true)?;
                            movw_signed(i, (t >> 16) as u32)
                        }
                        a64::TLSLE_MOVW_TPREL_G1_NC => movw(i, (t >> 16) as u32),
                        a64::TLSLE_MOVW_TPREL_G0 => {
                            range(&name, t, 17, true)?;
                            movw_signed(i, t as u32)
                        }
                        a64::TLSLE_MOVW_TPREL_G0_NC => movw(i, t as u32),
                        a64::TLSLE_ADD_TPREL_HI12 => {
                            range(&name, t, 24, false)?;
                            (i & 0xffc0_03ff) | ((((t >> 12) as u32) & 0xfff) << 10)
                        }
                        a64::TLSLE_ADD_TPREL_LO12 => {
                            range(&name, t, 12, false)?;
                            (i & 0xffc0_03ff) | (((t as u32) & 0xfff) << 10)
                        }
                        a64::TLSLE_ADD_TPREL_LO12_NC => {
                            (i & 0xffc0_03ff) | (((t as u32) & 0xfff) << 10)
                        }
                        // The LDSTn_TPREL_LO12 forms: no overflow check on
                        // the NC ones.
                        _ => {
                            let nc = matches!(s.ty, 553 | 555 | 557 | 559 | 571);
                            if !nc {
                                range(&name, t, 12, false)?;
                            }
                            pageoff12(i, t as u64).map_err(|e| format!("{name}: {e}"))?
                        }
                    };
                    set(b, at, w);
                }
                a64::TLSDESC_ADR_PAGE21 if s.dyn_tls => {
                    let w = page21(i, p, v.tg).map_err(|e| format!("{name}: {e}"))?;
                    set(b, at, w);
                }
                a64::TLSDESC_LD64_LO12 | a64::TLSDESC_ADD_LO12 if s.dyn_tls => {
                    let w = pageoff12(i, v.tg).map_err(|e| format!("{name}: {e}"))?;
                    set(b, at, w);
                }
                a64::TLSDESC_CALL if s.dyn_tls => {}
                a64::TLSDESC_ADR_PAGE21 => {
                    if s.kind == SymKind::Dso {
                        // adrp x0, :gottprel:v
                        let w = page21(0x9000_0000, p, v.tg).map_err(|e| format!("{name}: {e}"))?;
                        set(b, at, w);
                    } else {
                        range(&name, v.tp, 32, false)?;
                        // movz x0, #:tprel_g1:v
                        set(b, at, 0xd2a0_0000 | ((((v.tp >> 16) as u32) & 0xffff) << 5));
                    }
                }
                a64::TLSDESC_LD64_LO12 => {
                    if s.kind == SymKind::Dso {
                        // ldr x0, [x0, :gottprel_lo12:v]
                        let w = pageoff12(0xf940_0000, v.tg).map_err(|e| format!("{name}: {e}"))?;
                        set(b, at, w);
                    } else {
                        // movk x0, #:tprel_g0_nc:v
                        set(b, at, 0xf280_0000 | (((v.tp as u32) & 0xffff) << 5));
                    }
                }
                a64::TLSDESC_ADD_LO12 | a64::TLSDESC_CALL => set(b, at, 0xd503_201f),
                a64::TLS_DTPREL64 => put(b, at, (v.dtp + v.a) as u64, 8),
                _ => return Err(format!("unsupported relocation {name}")),
            }
        }
        Arch::I386 => i386::apply(s, b, at, v, relax, &name)?,
        Arch::Arm => return crate::elf::arch::arm32::apply(s, b, at, v, dynrel, &name),
        Arch::Riscv64 | Arch::Riscv32 | Arch::Ppc64 | Arch::S390x => {
            unreachable!("handled above")
        }
    }
    Ok(dynrel)
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn site(arch: Arch, ty: u16, kind: SymKind) -> Site {
        Site {
            arch,
            ty,
            kind,
            func: true,
            alloc: true,
            pie: true,
            dyn_tls: false,
        }
    }

    #[test]
    fn x86_general_dynamic_relaxes_to_local_exec() {
        // data16 lea x@tlsgd(%rip),%rdi; data16 data16 rex64 call
        let mut b = vec![
            0x66, 0x48, 0x8d, 0x3d, 0, 0, 0, 0, 0x66, 0x66, 0x48, 0xe8, 0, 0, 0, 0,
        ];
        let v = Values {
            a: -4,
            tp: -16,
            ..Values::default()
        };
        apply(
            site(Arch::X86_64, x64::TLSGD, SymKind::Local),
            &mut b,
            4,
            &v,
        )
        .unwrap();
        assert_eq!(
            b,
            [
                0x64, 0x48, 0x8b, 0x04, 0x25, 0, 0, 0, 0, 0x48, 0x8d, 0x80, 0xf0, 0xff, 0xff, 0xff
            ]
        );
    }

    #[test]
    fn x86_got_load_of_a_local_becomes_lea() {
        // mov foo@GOTPCREL(%rip), %rax
        let mut b = vec![0x48, 0x8b, 0x05, 0, 0, 0, 0];
        let v = Values {
            s: 0x2000,
            a: -4,
            p: 0x1003,
            ..Values::default()
        };
        let d = apply(
            site(Arch::X86_64, x64::REX_GOTPCRELX, SymKind::Local),
            &mut b,
            3,
            &v,
        );
        assert_eq!(d, Ok(Dyn::None));
        assert_eq!(b, [0x48, 0x8d, 0x05, 0xf9, 0x0f, 0, 0]);
    }

    #[test]
    fn i386_general_dynamic_relaxes_to_local_exec() {
        // leal x@tlsgd(,%ebx,1), %eax; call ___tls_get_addr@plt
        let mut b = vec![0x8d, 0x04, 0x1d, 0, 0, 0, 0, 0xe8, 0, 0, 0, 0];
        let v = Values {
            tp: -8,
            ..Values::default()
        };
        apply(
            site(Arch::I386, i386::TLS_GD, SymKind::Local),
            &mut b,
            3,
            &v,
        )
        .unwrap();
        // movl %gs:0, %eax; subl $8, %eax
        assert_eq!(b, [0x65, 0xa1, 0, 0, 0, 0, 0x81, 0xe8, 8, 0, 0, 0]);
    }

    #[test]
    fn i386_got_load_of_a_local_becomes_gotoff_lea() {
        // mov foo@GOT(%ebx), %eax
        let mut b = vec![0x8b, 0x83, 0, 0, 0, 0];
        let v = Values {
            s: 0x2010,
            got_base: 0x3000,
            ..Values::default()
        };
        let d = apply(
            site(Arch::I386, i386::GOT32X, SymKind::Local),
            &mut b,
            2,
            &v,
        );
        assert_eq!(d, Ok(Dyn::None));
        assert_eq!(b, [0x8d, 0x83, 0x10, 0xf0, 0xff, 0xff]);
    }

    #[test]
    fn i386_initial_exec_relaxes_to_an_immediate() {
        // movl x@gotntpoff(%ebx), %ecx; addl x@gotntpoff(%ebx), %edx
        let mut b = vec![0x8b, 0x8b, 0, 0, 0, 0, 0x03, 0x93, 0, 0, 0, 0];
        let v = Values {
            tp: -4,
            ..Values::default()
        };
        let s = site(Arch::I386, i386::TLS_GOTIE, SymKind::Local);
        apply(s, &mut b, 2, &v).unwrap();
        apply(s, &mut b, 8, &v).unwrap();
        // movl $-4, %ecx; leal -4(%edx), %edx
        assert_eq!(
            b,
            [
                0xc7, 0xc1, 0xfc, 0xff, 0xff, 0xff, 0x8d, 0x92, 0xfc, 0xff, 0xff, 0xff
            ]
        );
    }

    #[test]
    fn a64_tls_descriptor_relaxes_to_movz_movk() {
        let mut b = vec![0u8; 16];
        let v = Values {
            tp: 0x1_2340,
            ..Values::default()
        };
        let s = |ty| site(Arch::Aarch64, ty, SymKind::Local);
        apply(s(a64::TLSDESC_ADR_PAGE21), &mut b, 0, &v).unwrap();
        apply(s(a64::TLSDESC_LD64_LO12), &mut b, 4, &v).unwrap();
        apply(s(a64::TLSDESC_ADD_LO12), &mut b, 8, &v).unwrap();
        apply(s(a64::TLSDESC_CALL), &mut b, 12, &v).unwrap();
        // movz x0, #0x1, lsl #16; movk x0, #0x2340; nop; nop
        assert_eq!(insn(&b, 0), 0xd2a0_0020);
        assert_eq!(insn(&b, 4), 0xf284_6800);
        assert_eq!(insn(&b, 8), 0xd503_201f);
        assert_eq!(insn(&b, 12), 0xd503_201f);
    }

    #[test]
    fn a64_movw_signed_flips_to_movn() {
        // movz x0, #0, lsl #16 with -2: movn x0, #0, lsl #16... the
        // inverted piece of 0xffff_fffe >> 16.
        assert_eq!(movw_signed(0xd2a0_0000, 0xffff_ffff), 0x92a0_0000);
        assert_eq!(movw_signed(0xd2a0_0000, 1), 0xd2a0_0020);
    }
}
