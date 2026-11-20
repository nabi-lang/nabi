//! The code as text. `--emit=listing`: each function's instructions as
//! `llvm-objdump -dr` prints them (Intel syntax on `x86_64` and i386):
//! offsets from the function's start, branch targets by the emitter's
//! labels, each relocation under its instruction. `--emit=asm`: the whole
//! object as assembly source (`source`). An encoder records an
//! instruction (`x64::enc`, `a64::enc`, `i386::enc`, `arm32::enc`,
//! `riscv::enc`, `ppc64::enc`, `s390x::enc`) only when `Obj::asm` is set;
//! its text is made here, once the function's code is final. wasm32 lists
//! its own (`wasm::enc`), once the object writer has filled in the
//! indices.

pub mod code;
pub mod source;

mod read;
mod unwind;

use crate::lir::SymId;
use crate::obj::{Obj, RelKind};
use std::fmt::Write;

/// A function's code as its listing needs it.
pub struct Listing<'a> {
    /// The function's symbol as the object spells it (`sym_name`).
    pub name: &'a str,
    pub code: &'a [u8],
    /// `(offset, label)` of each bound label.
    pub labels: Vec<(u32, u32)>,
    /// `(offset, kind, symbol)`, offsets from the function's start.
    pub relocs: &'a [(u32, RelKind, SymId)],
}

impl Listing<'_> {
    /// Branch target `t` as objdump shows it: the offset, then the label
    /// there or the function plus an offset.
    #[must_use]
    pub fn target(&self, t: i64) -> String {
        let mut s = format!("{t:#x}");
        if let Some((_, l)) = self.labels.iter().find(|(at, _)| i64::from(*at) == t) {
            let _ = write!(s, " <.L{l}>");
        } else if t == 0 {
            let _ = write!(s, " <{}>", self.name);
        } else {
            let _ = write!(s, " <{}+{t:#x}>", self.name);
        }
        s
    }
}

/// How an ISA's objdump shows the code bytes.
#[derive(Clone, Copy)]
pub enum Bytes {
    /// Each byte, as on `x86_64`.
    Each,
    /// Little-endian 32-bit words, as on arm64.
    Words,
    /// Each byte of 32-bit instructions, in a word's column, as on
    /// powerpc64le.
    EachWord,
    /// Each byte, the text at column 24 (past a long instruction, the
    /// next tab stop), as on s390x and wasm32.
    Short,
}

/// Append `l` to `obj.asm` (as source to `obj.src`): `lines` are `(offset, text)` in order, the
/// text `mnemonic\toperands`.
pub fn function(obj: &mut Obj, l: &Listing<'_>, lines: &[(u32, String)], bytes: Bytes) {
    if obj.src.is_some() {
        code::function(obj, l, lines);
        return;
    }
    let mut out = String::new();
    let _ = writeln!(out, "\n<{}>:", l.name);
    let mut relocs = l.relocs.to_vec();
    relocs.sort_by_key(|r| r.0);
    let mut r = 0;
    for (n, (at, text)) in lines.iter().enumerate() {
        let end = lines.get(n + 1).map_or(l.code.len() as u32, |x| x.0);
        for (_, lab) in l.labels.iter().filter(|(o, _)| o == at) {
            let _ = writeln!(out, ".L{lab}:");
        }
        let code = &l.code[*at as usize..end as usize];
        let col = match bytes {
            Bytes::Each => {
                let b: Vec<String> = code.iter().map(|b| format!("{b:02x}")).collect();
                format!("{:<29}", b.join(" "))
            }
            // Data in code (`.word`): its bytes, as objdump shows data.
            Bytes::Words if text.starts_with('.') => {
                let b: Vec<String> = code.iter().map(|b| format!("{b:02x}")).collect();
                format!("{:<13}", b.join(" "))
            }
            Bytes::Words => {
                let w: Vec<String> = code
                    .chunks(4)
                    .map(|c| {
                        // RISC-V: a 16-bit instruction in 4 digits.
                        if c.len() == 2 && code.len() == 2 {
                            return format!("{:04x}", u16::from_le_bytes([c[0], c[1]]));
                        }
                        let mut a = [0u8; 4];
                        a[..c.len()].copy_from_slice(c);
                        format!("{:08x}", u32::from_le_bytes(a))
                    })
                    .collect();
                format!("{:<13}", w.join(" "))
            }
            Bytes::EachWord => {
                let b: Vec<String> = code.iter().map(|b| format!("{b:02x}")).collect();
                format!("{:<13}", b.join(" "))
            }
            Bytes::Short => {
                let b: Vec<String> = code.iter().map(|b| format!("{b:02x}")).collect();
                short(&b.join(" "))
            }
        };
        let _ = writeln!(out, "{at:>8x}: {col}\t{text}");
        while let Some(&(off, kind, sym)) = relocs.get(r).filter(|x| x.0 < end) {
            let name = if kind == RelKind::RvPcLoI {
                // The ELF writer's label at the `auipc`, numbered in the
                // text section's order.
                let before = obj.sects.get(&crate::obj::Sect::Text).map_or(0, |t| {
                    t.relocs
                        .iter()
                        .filter(|x| x.kind == RelKind::RvPcLoI)
                        .count()
                });
                let here = relocs[..r]
                    .iter()
                    .filter(|x| x.1 == RelKind::RvPcLoI)
                    .count();
                format!(".Lpcrel_hi{}", before + here)
            } else {
                sym_name(obj, &obj.syms[sym as usize].name)
            };
            let (ty, addend) = reloc_name(obj, kind, code);
            let add = match addend {
                0 => String::new(),
                a if a < 0 => format!("-{:#x}", -a),
                a => format!("+{a:#x}"),
            };
            // A 32-bit object's offsets in 8 digits, a tab further in.
            if obj.c32() {
                let _ = writeln!(out, "\t\t\t{off:08x}:  {ty}\t{name}{add}");
            } else {
                let _ = writeln!(out, "\t\t{off:016x}:  {ty}\t{name}{add}");
            }
            r += 1;
        }
    }
    // Labels past the last instruction.
    let len = l.code.len() as u32;
    for (_, lab) in l.labels.iter().filter(|(o, _)| *o == len) {
        let _ = writeln!(out, ".L{lab}:");
    }
    obj.asm.get_or_insert_with(String::new).push_str(&out);
}

/// Code bytes `b` padded as objdump pads them before text at column 24:
/// past it (the line starts 10 columns in), to the next tab stop but one
/// column.
#[must_use]
pub fn short(b: &str) -> String {
    let col = 10 + b.len();
    let pad = if col < 23 { 23 - col } else { 7 - col % 8 };
    format!("{b}{}", " ".repeat(pad))
}

/// `name` as the object's symbol table spells it (i386 COFF: C names
/// take an `_`, as `coff` writes them).
pub fn sym_name(obj: &Obj, name: &str) -> String {
    if obj.coff && obj.i386 {
        match name.strip_prefix("__imp_") {
            Some(n) => format!("__imp__{n}"),
            None => format!("_{name}"),
        }
    } else if obj.elf || obj.coff {
        name.to_owned()
    } else {
        format!("_{name}")
    }
}

/// The object format's name of relocation `kind` in instruction `insn`,
/// and the addend objdump shows (ELF `x86_64` PC-relative ones count
/// from the field's end; i386 keeps its addends in the place).
fn reloc_name(obj: &Obj, kind: RelKind, insn: &[u8]) -> (&'static str, i64) {
    if obj.riscv {
        return match kind {
            RelKind::RvCall => ("R_RISCV_CALL_PLT", 0),
            RelKind::RvPcHi => ("R_RISCV_PCREL_HI20", 0),
            RelKind::RvGotHi => ("R_RISCV_GOT_HI20", 0),
            RelKind::RvTlsIeHi => ("R_RISCV_TLS_GOT_HI20", 0),
            RelKind::RvPcLoI => ("R_RISCV_PCREL_LO12_I", 0),
            RelKind::RvTpHi => ("R_RISCV_TPREL_HI20", 0),
            RelKind::RvTpAdd => ("R_RISCV_TPREL_ADD", 0),
            RelKind::RvTpLo => ("R_RISCV_TPREL_LO12_I", 0),
            _ if obj.rv32 => ("R_RISCV_32", 0),
            _ => ("R_RISCV_64", 0),
        };
    }
    if obj.i386 {
        let name = if obj.coff {
            match kind {
                RelKind::I386Call => "IMAGE_REL_I386_REL32",
                RelKind::X86SecRel => "IMAGE_REL_I386_SECREL",
                _ => "IMAGE_REL_I386_DIR32",
            }
        } else {
            match kind {
                RelKind::I386Call => "R_386_PLT32",
                RelKind::I386GotPc => "R_386_GOTPC",
                RelKind::I386GotOff => "R_386_GOTOFF",
                RelKind::I386Got32X => "R_386_GOT32X",
                RelKind::I386TlsGotIe => "R_386_TLS_GOTIE",
                _ => "R_386_32",
            }
        };
        return (name, 0);
    }
    if obj.x86 {
        if obj.coff {
            return match kind {
                RelKind::X86SecRel => ("IMAGE_REL_AMD64_SECREL", 0),
                RelKind::Abs64 => ("IMAGE_REL_AMD64_ADDR64", 0),
                _ => ("IMAGE_REL_AMD64_REL32", 0),
            };
        }
        return match kind {
            RelKind::X86Call => ("R_X86_64_PLT32", -4),
            RelKind::X86GotLoad => ("R_X86_64_REX_GOTPCRELX", -4),
            RelKind::X86Tls => ("R_X86_64_GOTTPOFF", -4),
            RelKind::X86TlsGd => ("R_X86_64_TLSGD", -4),
            RelKind::Abs64 => ("R_X86_64_64", 0),
            _ => ("R_X86_64_PC32", -4),
        };
    }
    if obj.arm {
        // REL: the addends are in the code, objdump shows none.
        return match kind {
            RelKind::ArmCall => ("R_ARM_CALL", 0),
            RelKind::ArmMovwPrel => ("R_ARM_MOVW_PREL_NC", 0),
            RelKind::ArmMovtPrel => ("R_ARM_MOVT_PREL", 0),
            RelKind::ArmGotPrel => ("R_ARM_GOT_PREL", 0),
            RelKind::ArmTlsIe => ("R_ARM_TLS_IE32", 0),
            _ => ("R_ARM_ABS32", 0),
        };
    }
    if obj.ppc64 {
        return match kind {
            RelKind::PpcCall => ("R_PPC64_REL24", 0),
            RelKind::PpcEntryHa => ("R_PPC64_REL16_HA", 0),
            // The entry's `addi` is 4 bytes past the start, as `elf` says.
            RelKind::PpcEntryLo => ("R_PPC64_REL16_LO", 4),
            RelKind::PpcTocHa => ("R_PPC64_TOC16_HA", 0),
            RelKind::PpcTocLo => ("R_PPC64_TOC16_LO", 0),
            RelKind::PpcGotHa => ("R_PPC64_GOT16_HA", 0),
            RelKind::PpcGotLo => ("R_PPC64_GOT16_LO_DS", 0),
            RelKind::PpcTlsIeHa => ("R_PPC64_GOT_TPREL16_HA", 0),
            RelKind::PpcTlsIeLo => ("R_PPC64_GOT_TPREL16_LO_DS", 0),
            RelKind::PpcTls => ("R_PPC64_TLS", 0),
            _ => ("R_PPC64_ADDR64", 0),
        };
    }
    if obj.s390x {
        // The field is 2 bytes into its instruction.
        return match kind {
            RelKind::S390Call => ("R_390_PLT32DBL", 2),
            RelKind::S390Got => ("R_390_GOTENT", 2),
            RelKind::S390TlsIe => ("R_390_TLS_IEENT", 2),
            _ => ("R_390_PC32DBL", 2),
        };
    }
    if obj.coff {
        return match kind {
            RelKind::Branch26 => ("IMAGE_REL_ARM64_BRANCH26", 0),
            RelKind::Page21 => ("IMAGE_REL_ARM64_PAGEBASE_REL21", 0),
            // A load / store scales the offset (as `coff` decides).
            RelKind::PageOff12 if crate::isa::a64::enc::scaled_ldst(insn) => {
                ("IMAGE_REL_ARM64_PAGEOFFSET_12L", 0)
            }
            RelKind::PageOff12 => ("IMAGE_REL_ARM64_PAGEOFFSET_12A", 0),
            RelKind::SecRelHi12 => ("IMAGE_REL_ARM64_SECREL_HIGH12A", 0),
            RelKind::SecRelLo12 => ("IMAGE_REL_ARM64_SECREL_LOW12A", 0),
            _ => ("IMAGE_REL_ARM64_ADDR64", 0),
        };
    }
    if obj.elf {
        return match kind {
            RelKind::Branch26 => ("R_AARCH64_CALL26", 0),
            RelKind::Page21 => ("R_AARCH64_ADR_PREL_PG_HI21", 0),
            RelKind::PageOff12 => ("R_AARCH64_ADD_ABS_LO12_NC", 0),
            RelKind::GotPage21 => ("R_AARCH64_ADR_GOT_PAGE", 0),
            RelKind::GotPageOff12 => ("R_AARCH64_LD64_GOT_LO12_NC", 0),
            RelKind::TlvPage21 => ("R_AARCH64_TLSIE_ADR_GOTTPREL_PAGE21", 0),
            RelKind::TlvPageOff12 => ("R_AARCH64_TLSIE_LD64_GOTTPREL_LO12_NC", 0),
            RelKind::TlsDescPage21 => ("R_AARCH64_TLSDESC_ADR_PAGE21", 0),
            RelKind::TlsDescLd => ("R_AARCH64_TLSDESC_LD64_LO12", 0),
            RelKind::TlsDescAdd => ("R_AARCH64_TLSDESC_ADD_LO12", 0),
            RelKind::TlsDescCall => ("R_AARCH64_TLSDESC_CALL", 0),
            _ => ("R_AARCH64_ABS64", 0),
        };
    }
    match kind {
        RelKind::Branch26 => ("ARM64_RELOC_BRANCH26", 0),
        RelKind::Page21 => ("ARM64_RELOC_PAGE21", 0),
        RelKind::PageOff12 => ("ARM64_RELOC_PAGEOFF12", 0),
        RelKind::GotPage21 => ("ARM64_RELOC_GOT_LOAD_PAGE21", 0),
        RelKind::GotPageOff12 => ("ARM64_RELOC_GOT_LOAD_PAGEOFF12", 0),
        RelKind::TlvPage21 => ("ARM64_RELOC_TLVP_LOAD_PAGE21", 0),
        RelKind::TlvPageOff12 => ("ARM64_RELOC_TLVP_LOAD_PAGEOFF12", 0),
        _ => ("ARM64_RELOC_UNSIGNED", 0),
    }
}
