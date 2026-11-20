//! ELF relocatable object (`ET_REL`) writer: ELF64 `x86_64`, arm64,
//! riscv64, powerpc64 (`ELFv2`) and s390x (big endian), ELF32 i386, 32-bit
//! ARM and riscv32.
//!
//! A section per function and data object so `--gc-sections` drops the
//! unused; their `.rela.*` (32-bit: `.rel.*`, addends in place);
//! `.eh_frame` with an FDE per function for unwinders and debuggers (32-bit
//! ARM: an `.ARM.exidx` section per function, mapping symbols, the build
//! attributes; RISC-V: RELA on ELF32 too, a label at each `auipc` a low
//! 12-bit relocation names, the attributes); debug sections; a
//! non-executable stack note. Symbols local first.

use crate::obj::{FnRange, Obj, RelKind, Sect};

const SHT_PROGBITS: u32 = 1;
const SHT_SYMTAB: u32 = 2;
const SHT_STRTAB: u32 = 3;
const SHT_RELA: u32 = 4;
const SHT_REL: u32 = 9;
const SHT_NOBITS: u32 = 8;
const SHT_SYMTAB_SHNDX: u32 = 18;
const SHT_ARM_EXIDX: u32 = 0x7000_0001;
const SHT_ARM_ATTRIBUTES: u32 = 0x7000_0003;
const SHT_RISCV_ATTRIBUTES: u32 = 0x7000_0003;
const SHN_LORESERVE: usize = 0xff00;
const SHN_XINDEX: u16 = 0xffff;
const SHF_WRITE: u64 = 1;
const SHF_ALLOC: u64 = 2;
const SHF_EXECINSTR: u64 = 4;
const SHF_INFO_LINK: u64 = 0x40;
const SHF_LINK_ORDER: u64 = 0x80;
const STB_LOCAL: u8 = 0;
const STB_GLOBAL: u8 = 1;
const STB_WEAK: u8 = 2;
const STV_HIDDEN: u8 = 2;
const STT_NOTYPE: u8 = 0;
const STT_OBJECT: u8 = 1;
const STT_FUNC: u8 = 2;
const STT_SECTION: u8 = 3;
const STT_TLS: u8 = 6;
const EM_X86_64: u16 = 62;
const EM_AARCH64: u16 = 183;
const EM_386: u16 = 3;
const EM_ARM: u16 = 40;
const EM_RISCV: u16 = 243;
const EM_PPC64: u16 = 21;
const EM_S390: u16 = 22;
/// `ELFv2`.
const EF_PPC64: u32 = 2;
/// `STO_PPC64_LOCAL` of a function whose global entry is 8 bytes long.
const STO_PPC64_LOCAL8: u8 = 3 << 5;
/// `EF_RISCV_RVC`, with `EF_RISCV_FLOAT_ABI_DOUBLE` on riscv64.
const EF_RISCV64: u32 = 0x5;
const EF_RISCV32: u32 = 0x1;
/// `EF_ARM_EABI_VER5 | EF_ARM_ABI_FLOAT_HARD`.
const EF_ARM: u32 = 0x0500_0400;

const R_X86_64_64: u32 = 1;
const R_X86_64_PC32: u32 = 2;
const R_X86_64_PLT32: u32 = 4;
const R_X86_64_32: u32 = 10;
const R_X86_64_GOTTPOFF: u32 = 22;
const R_X86_64_TLSGD: u32 = 19;
const R_X86_64_REX_GOTPCRELX: u32 = 42;
const R_386_32: u32 = 1;
const R_386_PC32: u32 = 2;
const R_386_PLT32: u32 = 4;
const R_386_GOTOFF: u32 = 9;
const R_386_GOTPC: u32 = 10;
const R_386_TLS_GOTIE: u32 = 16;
const R_386_GOT32X: u32 = 43;
const R_ARM_ABS32: u32 = 2;
const R_ARM_CALL: u32 = 28;
const R_ARM_PREL31: u32 = 42;
const R_ARM_MOVW_PREL_NC: u32 = 45;
const R_ARM_MOVT_PREL: u32 = 46;
const R_ARM_GOT_PREL: u32 = 96;
const R_ARM_TLS_IE32: u32 = 107;

/// What LLVM writes for ARMv7-A with VFPv3-D32 and NEON, hard-float
/// AAPCS.
const ARM_ATTRIBUTES: [u8; 58] = [
    0x41, 0x39, 0, 0, 0, b'a', b'e', b'a', b'b', b'i', 0, 1, 0x2f, 0, 0, 0, 0x43, b'2', b'.', b'0',
    b'9', 0, 6, 10, 7, b'A', 8, 1, 9, 2, 10, 3, 12, 1, 14, 0, 17, 1, 18, 4, 20, 1, 21, 1, 23, 3,
    24, 1, 25, 1, 28, 1, 30, 1, 34, 1, 38, 1,
];
const R_RISCV_32: u32 = 1;
const R_RISCV_64: u32 = 2;
const R_RISCV_CALL_PLT: u32 = 19;
const R_RISCV_GOT_HI20: u32 = 20;
const R_RISCV_TLS_GOT_HI20: u32 = 21;
const R_RISCV_PCREL_HI20: u32 = 23;
const R_RISCV_PCREL_LO12_I: u32 = 24;
const R_RISCV_TPREL_HI20: u32 = 29;
const R_RISCV_TPREL_LO12_I: u32 = 30;
const R_RISCV_TPREL_ADD: u32 = 32;
const R_RISCV_32_PCREL: u32 = 57;
const R_PPC64_ADDR32: u32 = 1;
const R_PPC64_REL24: u32 = 10;
const R_PPC64_GOT16_HA: u32 = 17;
const R_PPC64_REL32: u32 = 26;
const R_PPC64_ADDR64: u32 = 38;
const R_PPC64_TOC16_LO: u32 = 48;
const R_PPC64_TOC16_HA: u32 = 50;
const R_PPC64_GOT16_LO_DS: u32 = 59;
const R_PPC64_TLS: u32 = 67;
const R_PPC64_GOT_TPREL16_LO_DS: u32 = 88;
const R_PPC64_GOT_TPREL16_HA: u32 = 90;
const R_PPC64_REL16_LO: u32 = 250;
const R_PPC64_REL16_HA: u32 = 252;
const R_390_32: u32 = 4;
const R_390_PC32: u32 = 5;
const R_390_PC32DBL: u32 = 19;
const R_390_PLT32DBL: u32 = 20;
const R_390_64: u32 = 22;
const R_390_GOTENT: u32 = 26;
const R_390_TLS_IEENT: u32 = 49;

/// The RISC-V build attributes: 16-byte stack alignment, the ISA the
/// code may use.
fn riscv_attributes(rv32: bool, zbb: bool) -> Vec<u8> {
    let arch: &[u8] = if rv32 {
        b"rv32i2p1_m2p0_a2p1_c2p0"
    } else {
        b"rv64i2p1_m2p0_a2p1_f2p2_d2p2_c2p0_zicsr2p0_zifencei2p0"
    };
    // Tag_RISCV_stack_align, Tag_RISCV_arch.
    let mut attrs = vec![4, 16, 5];
    attrs.extend_from_slice(arch);
    if zbb {
        attrs.extend_from_slice(b"_zbb1p0");
    }
    attrs.push(0);
    // Tag_File and its length (the tag and the length word included).
    let mut sub = vec![1];
    put32(false, &mut sub, attrs.len() as u32 + 5);
    sub.extend_from_slice(&attrs);
    let mut b = vec![b'A'];
    put32(false, &mut b, sub.len() as u32 + 4 + 6);
    b.extend_from_slice(b"riscv\0");
    b.extend_from_slice(&sub);
    b
}

const R_AARCH64_ABS64: u32 = 257;
const R_AARCH64_ABS32: u32 = 258;
const R_AARCH64_PREL32: u32 = 261;
const R_AARCH64_ADR_PREL_PG_HI21: u32 = 275;
const R_AARCH64_ADD_ABS_LO12_NC: u32 = 277;
const R_AARCH64_CALL26: u32 = 283;
const R_AARCH64_ADR_GOT_PAGE: u32 = 311;
const R_AARCH64_LD64_GOT_LO12_NC: u32 = 312;
const R_AARCH64_TLSIE_ADR_GOTTPREL_PAGE21: u32 = 541;
const R_AARCH64_TLSIE_LD64_GOTTPREL_LO12_NC: u32 = 542;
const R_AARCH64_TLSDESC_ADR_PAGE21: u32 = 562;
const R_AARCH64_TLSDESC_LD64_LO12: u32 = 563;
const R_AARCH64_TLSDESC_ADD_LO12: u32 = 564;
const R_AARCH64_TLSDESC_CALL: u32 = 569;

/// `(offset, type, symbol table index, addend)`.
type Rela = (u64, u32, usize, i64);

struct Out {
    name: String,
    ty: u32,
    flags: u64,
    data: Vec<u8>,
    align: u64,
    link: u32,
    info: u32,
    entsize: u64,
}

fn put16(be: bool, b: &mut Vec<u8>, v: u16) {
    b.extend_from_slice(&if be { v.to_be_bytes() } else { v.to_le_bytes() });
}

fn put32(be: bool, b: &mut Vec<u8>, v: u32) {
    b.extend_from_slice(&if be { v.to_be_bytes() } else { v.to_le_bytes() });
}

fn put64(be: bool, b: &mut Vec<u8>, v: u64) {
    b.extend_from_slice(&if be { v.to_be_bytes() } else { v.to_le_bytes() });
}

/// The `n`-byte unsigned value at `b[at..]` in the object's byte order.
fn get(be: bool, b: &[u8], at: usize, n: usize) -> u64 {
    let mut v = 0u64;
    for k in 0..n {
        let byte = u64::from(b[at + if be { k } else { n - 1 - k }]);
        v = (v << 8) | byte;
    }
    v
}

/// The FDE rows of the callee-saved registers `f`'s prologue stores, past
/// code offset `cur`: the advance factored by `code`, offsets by `-align`.
fn saves(fde: &mut Vec<u8>, f: &FnRange, cur: u32, align: i32, code: u32, be: bool) {
    if f.saves.regs.is_empty() {
        return;
    }
    // DW_CFA_advance_loc4
    fde.push(0x04);
    put32(be, fde, (f.saves.at - cur) / code);
    for &(r, off) in &f.saves.regs {
        let n = (-off / align) as u64;
        if r < 64 {
            fde.push(0x80 | r as u8); // DW_CFA_offset
        } else {
            fde.push(0x05); // DW_CFA_offset_extended
            nabic_core::leb::uleb(fde, u64::from(r));
        }
        nabic_core::leb::uleb(fde, n);
    }
}

/// `.eh_frame`: a CIE and one FDE per function; the relocations of the
/// FDEs' `pc_begin` (`(offset, function index)`).
fn eh_frame(fns: &[FnRange], obj: &Obj) -> (Vec<u8>, Vec<(u32, usize)>) {
    let (x86, i386, riscv) = (obj.x86, obj.c32() && !obj.riscv, obj.riscv);
    let be = obj.be;
    let mut b = Vec::new();
    let mut relocs = Vec::new();
    // CIE: "zR", code align 1, data align -8 (i386: -4), pc-relative
    // sdata4 FDEs.
    let (ra, init): (u8, &[u8]) = if obj.ppc64 {
        // CFA = r1 + 0, the return address in `lr` (65).
        (65, &[0x0c, 1, 0])
    } else if obj.s390x {
        // CFA = r15 + 160 (the caller's register save area), the return
        // address in r14.
        (14, &[0x0c, 15, 0xa0, 1])
    } else if riscv {
        // CFA = sp + 0, the return address in `ra`.
        (1, &[0x0c, 2, 0])
    } else if i386 {
        // CFA = esp + 4, return address at CFA - 4.
        (8, &[0x0c, 4, 4, 0x88, 1])
    } else if x86 {
        // CFA = rsp + 8, return address at CFA - 8.
        (16, &[0x0c, 7, 8, 0x90, 1])
    } else {
        // CFA = sp + 0.
        (30, &[0x0c, 31, 0])
    };
    let align = if i386 || obj.rv32 { 0x7c } else { 0x78 };
    // Code alignment: instructions are 4 bytes (s390x: 2, 4 or 6).
    let code = if obj.ppc64 {
        4
    } else if obj.s390x {
        2
    } else {
        1
    };
    let mut cie = vec![0, 0, 0, 0, 1, b'z', b'R', 0, code, align, ra, 1, 0x1b];
    cie.extend_from_slice(init);
    while (cie.len() + 4) % 8 != 0 {
        cie.push(0); // DW_CFA_nop
    }
    put32(be, &mut b, cie.len() as u32);
    b.extend_from_slice(&cie);
    for (i, f) in fns.iter().enumerate() {
        let start = b.len();
        let mut fde = Vec::new();
        put32(be, &mut fde, start as u32 + 4); // CIE pointer: back to offset 0
        relocs.push(((start + 8) as u32, i));
        put32(be, &mut fde, 0); // pc_begin
        put32(be, &mut fde, f.len);
        fde.push(0); // augmentation data
        if obj.ppc64 {
            // Past the global entry, mflr r0; std r31, -8(r1): r31 at
            // CFA - 8; std r0, 16(r1): lr at CFA + 16; mr r31, r1: the
            // CFA is r31.
            fde.extend_from_slice(&[0x44, 0x9f, 1, 0x41, 0x11, 65, 0x7e, 0x41, 0x0d, 31]);
            // std of the callee-saved registers used, below r31.
            saves(&mut fde, f, 24, 8, 4, be);
        } else if obj.s390x {
            // stmg r6, r15, 48(r15): rN at CFA - 160 + 48 + 8 (N - 6);
            // lgr r11, r15: the CFA is r11 + 160.
            fde.push(0x43);
            for r in 6..16u8 {
                fde.extend_from_slice(&[0x80 | r, 20 - r]);
            }
            fde.extend_from_slice(&[0x42, 0x0d, 11]);
            // std of the callee-saved float registers used.
            saves(&mut fde, f, 10, 8, 2, be);
        } else if riscv {
            // addi sp, sp, -16; s ra / s0 at CFA - XLEN / - 2 * XLEN;
            // addi s0, sp, 16 (each 2 bytes when compressed)
            let i: u8 = if f.saves.short { 2 } else { 4 };
            fde.extend_from_slice(&[
                0x40 | i,
                0x0e,
                16,
                0x40 | (2 * i),
                0x81,
                1,
                0x88,
                2,
                0x40 | i,
                0x0c,
                8,
                0,
            ]);
            // Stores of the callee-saved registers used, below s0.
            saves(
                &mut fde,
                f,
                4 * u32::from(i),
                if obj.rv32 { 4 } else { 8 },
                1,
                be,
            );
        } else if i386 {
            // push ebp; mov ebp, esp; push ebx; push esi; push edi
            fde.extend_from_slice(&[
                0x41, 0x0e, 8, 0x85, 2, 0x42, 0x0d, 5, 0x41, 0x83, 3, 0x41, 0x86, 4, 0x41, 0x87, 5,
            ]);
        } else if x86 {
            // push rbp; mov rbp, rsp
            fde.extend_from_slice(&[0x41, 0x0e, 16, 0x86, 2, 0x43, 0x0d, 6]);
            saves(&mut fde, f, 4, 8, 1, be);
        } else {
            // stp x29, x30, [sp, #-16]!; mov x29, sp
            fde.extend_from_slice(&[0x44, 0x0e, 16, 0x9e, 1, 0x9d, 2, 0x44, 0x0d, 29]);
            saves(&mut fde, f, 8, 8, 1, be);
        }
        while (fde.len() + 4) % 8 != 0 {
            fde.push(0);
        }
        put32(be, &mut b, fde.len() as u32);
        b.extend_from_slice(&fde);
    }
    put32(be, &mut b, 0); // terminator
    (b, relocs)
}

/// Serialize `obj`.
#[must_use]
pub fn write(obj: &Obj) -> Vec<u8> {
    let x86 = obj.x86;
    let c32 = obj.c32();
    let arm = obj.arm;
    let riscv = obj.riscv;
    let be = obj.be;
    // In-place addends (REL).
    let rel = c32 && !riscv;
    let (pieces, piece_of) = obj.pieces();
    let mut outs: Vec<Out> = vec![Out {
        name: String::new(),
        ty: 0,
        flags: 0,
        data: Vec::new(),
        align: 0,
        link: 0,
        info: 0,
        entsize: 0,
    }];
    // Symbols: null, section symbols, locals, globals. Unwind and debug
    // info refer to code by section symbol, so they drop with the section.
    let nsect_syms = pieces.len() + obj.extra.len();
    // 32-bit ARM mapping symbols: `(piece, offset, name)`, A32 code at
    // each text piece and after its literal words.
    let mut maps: Vec<(usize, u32, &str)> = Vec::new();
    if arm {
        for (k, p) in pieces
            .iter()
            .enumerate()
            .filter(|(_, p)| p.sect == Sect::Text)
        {
            maps.push((k, 0, "$a"));
            for &(a, b) in obj
                .data_in_code
                .iter()
                .filter(|d| (p.start..p.end).contains(&d.0))
            {
                maps.push((k, a - p.start, "$d"));
                if b < p.end {
                    maps.push((k, b - p.start, "$a"));
                }
            }
        }
        maps.sort_by_key(|m| (m.0, m.1));
    }
    // RISC-V: the `auipc` before each low 12-bit relocation, `(piece,
    // offset)`, a local label.
    let mut labels: Vec<(usize, u32)> = Vec::new();
    if riscv {
        for (k, p) in pieces.iter().enumerate() {
            for r in &p.relocs {
                if r.kind == RelKind::RvPcLoI {
                    labels.push((k, r.off - 4));
                }
            }
        }
        labels.sort_unstable();
        labels.dedup();
    }
    let label_index = |k: usize, off: u32| {
        1 + nsect_syms + maps.len() + labels.partition_point(|&l| l < (k, off))
    };
    let mut order: Vec<usize> = (0..obj.syms.len()).collect();
    let local = |i: usize| obj.syms[i].def.is_some() && !obj.syms[i].global;
    order.sort_by_key(|&i| (!local(i), i));
    let mut index = vec![0usize; obj.syms.len()];
    for (k, &i) in order.iter().enumerate() {
        index[i] = 1 + nsect_syms + maps.len() + labels.len() + k;
    }
    // A thread-local (the runtime's, undefined here) is STT_TLS: GNU ld
    // rejects a TLS definition for a reference of another type.
    let mut tls = vec![false; obj.syms.len()];
    for r in pieces.iter().flat_map(|p| &p.relocs) {
        if matches!(
            r.kind,
            RelKind::X86Tls
                | RelKind::X86TlsGd
                | RelKind::TlvPage21
                | RelKind::TlvPageOff12
                | RelKind::TlsDescPage21
                | RelKind::TlsDescLd
                | RelKind::TlsDescAdd
                | RelKind::TlsDescCall
                | RelKind::I386TlsGotIe
                | RelKind::ArmTlsIe
                | RelKind::RvTlsIeHi
                | RelKind::RvTpHi
                | RelKind::RvTpAdd
                | RelKind::RvTpLo
                | RelKind::PpcTlsIeHa
                | RelKind::PpcTlsIeLo
                | RelKind::PpcTls
                | RelKind::S390TlsIe
        ) {
            tls[r.sym as usize] = true;
        }
    }
    // The section symbol of `sym`'s piece and the object address `at`
    // as an offset in it.
    let in_piece = |sym: u32, at: u64| match piece_of.get(sym as usize).copied().flatten() {
        Some(k) => (1 + k, at - u64::from(pieces[k].start)),
        None => (index[sym as usize], at),
    };
    // The section index of each section symbol.
    let mut sect_syms: Vec<usize> = Vec::new();
    // The relocations of each section by its index.
    let mut relas: Vec<(usize, Vec<Rela>)> = Vec::new();
    for (k, p) in pieces.iter().enumerate() {
        let data = &obj.sects[&p.sect].data[p.start as usize..p.end as usize];
        let bss = p.sect == Sect::Data && p.relocs.is_empty() && data.iter().all(|&b| b == 0);
        let (prefix, flags) = match p.sect {
            Sect::Text => (".text", SHF_ALLOC | SHF_EXECINSTR),
            Sect::Const => (".rodata", SHF_ALLOC),
            Sect::DataConst => (".data.rel.ro", SHF_ALLOC | SHF_WRITE),
            Sect::Data if bss => (".bss", SHF_ALLOC | SHF_WRITE),
            Sect::Data => (".data", SHF_ALLOC | SHF_WRITE),
            Sect::Objc(_) => unreachable!("Objective-C metadata is Mach-O only"),
        };
        sect_syms.push(outs.len());
        // A data pointer's addend is written in place; RELA carries it
        // (`rela_data` clears the place).
        let rela = !(arm || c32 || riscv || obj.ppc64 || obj.s390x);
        let in_place = |r: &crate::obj::Reloc| -> i64 {
            if !rela || r.kind != RelKind::Abs64 || p.sect == Sect::Text {
                return 0;
            }
            let at = r.off as usize;
            let w: [u8; 8] = data[at..at + 8].try_into().expect("a word");
            (if obj.be {
                u64::from_be_bytes(w)
            } else {
                u64::from_le_bytes(w)
            }) as i64
        };
        let rs = p
            .relocs
            .iter()
            .map(|r| {
                if riscv {
                    let ty = reloc_riscv(r.kind, obj.rv32);
                    let sym = if r.kind == RelKind::RvPcLoI {
                        label_index(k, r.off - 4)
                    } else {
                        index[r.sym as usize]
                    };
                    return (u64::from(r.off), ty, sym, 0);
                }
                if obj.ppc64 || obj.s390x {
                    let (ty, addend) = if obj.ppc64 {
                        reloc_ppc64(r.kind)
                    } else {
                        reloc_s390x(r.kind)
                    };
                    return (u64::from(r.off), ty, index[r.sym as usize], addend);
                }
                let (ty, addend) = if arm {
                    (reloc_arm(r.kind), 0)
                } else if c32 {
                    (reloc386(r.kind), 0)
                } else {
                    reloc(r.kind, x86)
                };
                (
                    u64::from(r.off),
                    ty,
                    index[r.sym as usize],
                    addend + in_place(r),
                )
            })
            .collect();
        relas.push((outs.len(), rs));
        outs.push(Out {
            name: format!("{prefix}.{}", obj.syms[p.syms[0] as usize].name),
            ty: if bss { SHT_NOBITS } else { SHT_PROGBITS },
            flags,
            data: rela_data(data, &p.relocs, rela && p.sect != Sect::Text),
            align: 1 << p.align,
            link: 0,
            info: 0,
            entsize: 0,
        });
    }
    // Unwind tables: 32-bit ARM's index, a section per function linked to
    // its code (the place holds the function's offset in it).
    if arm {
        for f in &obj.fns {
            let Some(k) = piece_of.get(f.sym as usize).copied().flatten() else {
                continue;
            };
            let (s, at) = in_piece(f.sym, u64::from(f.start));
            let text = sect_syms[k];
            let mut data = Vec::new();
            put32(be, &mut data, 0);
            put32(be, &mut data, crate::isa::arm32::EXIDX);
            relas.push((outs.len(), vec![(0, R_ARM_PREL31, s, at as i64)]));
            outs.push(Out {
                name: format!(".ARM.exidx{}", outs[text].name),
                ty: SHT_ARM_EXIDX,
                flags: SHF_ALLOC | SHF_LINK_ORDER,
                data,
                align: 4,
                link: text as u32,
                info: 0,
                entsize: 0,
            });
        }
    } else if !obj.fns.is_empty() {
        let (data, rs) = eh_frame(&obj.fns, obj);
        let ty = if obj.ppc64 {
            R_PPC64_REL32
        } else if obj.s390x {
            R_390_PC32
        } else if riscv {
            R_RISCV_32_PCREL
        } else if c32 {
            R_386_PC32
        } else if x86 {
            R_X86_64_PC32
        } else {
            R_AARCH64_PREL32
        };
        relas.push((
            outs.len(),
            rs.into_iter()
                .map(|(off, f)| {
                    let f = &obj.fns[f];
                    let (s, at) = in_piece(f.sym, u64::from(f.start));
                    (u64::from(off), ty, s, at as i64)
                })
                .collect(),
        ));
        outs.push(Out {
            name: ".eh_frame".to_owned(),
            ty: SHT_PROGBITS,
            flags: SHF_ALLOC,
            data,
            align: 8,
            link: 0,
            info: 0,
            entsize: 0,
        });
    }
    // Debug sections: the Mach-O contents (the target's object address,
    // an offset into a debug section) become explicit addends.
    let extra_sym = 1 + pieces.len();
    for (_, name, _, sec) in &obj.extra {
        let mut data = sec.data.clone();
        let mut rs = Vec::new();
        for r in &sec.relocs {
            let at = r.off as usize;
            if r.kind == RelKind::SecOff32 {
                let v = get(be, &data, at, 4) as u32;
                data[at..at + 4].fill(0);
                let ty = if obj.ppc64 {
                    R_PPC64_ADDR32
                } else if obj.s390x {
                    R_390_32
                } else if riscv {
                    R_RISCV_32
                } else if arm {
                    R_ARM_ABS32
                } else if c32 {
                    R_386_32
                } else if x86 {
                    R_X86_64_32
                } else {
                    R_AARCH64_ABS32
                };
                rs.push((
                    u64::from(r.off),
                    ty,
                    extra_sym + r.sym as usize,
                    i64::from(v),
                ));
                continue;
            }
            let w = if c32 { 4 } else { 8 };
            let v = get(be, &data, at, w);
            data[at..at + w].fill(0);
            let ty = if obj.ppc64 {
                R_PPC64_ADDR64
            } else if obj.s390x {
                R_390_64
            } else if riscv && c32 {
                R_RISCV_32
            } else if riscv {
                R_RISCV_64
            } else if arm {
                R_ARM_ABS32
            } else if c32 {
                R_386_32
            } else if x86 {
                R_X86_64_64
            } else {
                R_AARCH64_ABS64
            };
            let (s, addend) = in_piece(r.sym, v);
            rs.push((u64::from(r.off), ty, s, addend as i64));
        }
        sect_syms.push(outs.len());
        relas.push((outs.len(), rs));
        outs.push(Out {
            name: format!(".{}", name.trim_start_matches('_')),
            ty: SHT_PROGBITS,
            flags: 0,
            data,
            align: 1,
            link: 0,
            info: 0,
            entsize: 0,
        });
    }
    if arm {
        outs.push(Out {
            name: ".ARM.attributes".to_owned(),
            ty: SHT_ARM_ATTRIBUTES,
            flags: 0,
            data: ARM_ATTRIBUTES.to_vec(),
            align: 1,
            link: 0,
            info: 0,
            entsize: 0,
        });
    }
    if riscv {
        outs.push(Out {
            name: ".riscv.attributes".to_owned(),
            ty: SHT_RISCV_ATTRIBUTES,
            flags: 0,
            data: riscv_attributes(obj.rv32, obj.zbb),
            align: 1,
            link: 0,
            info: 0,
            entsize: 0,
        });
    }
    outs.push(Out {
        name: ".note.GNU-stack".to_owned(),
        ty: SHT_PROGBITS,
        flags: 0,
        data: Vec::new(),
        align: 1,
        link: 0,
        info: 0,
        entsize: 0,
    });

    // Section indexes from `SHN_LORESERVE` on are in `.symtab_shndx`.
    let xindex = outs.len() > SHN_LORESERVE;
    let mut shndx_tab = Vec::new();
    if xindex {
        put32(be, &mut shndx_tab, 0);
    }
    let mut strtab = vec![0u8];
    let mut symtab = vec![0u8; if c32 { 16 } else { 24 }];
    let mut entry =
        |symtab: &mut Vec<u8>, name: u32, info: u8, other, shndx: usize, value: u64, size: u64| {
            let shndx16 = if shndx >= SHN_LORESERVE {
                SHN_XINDEX
            } else {
                shndx as u16
            };
            put32(be, symtab, name);
            if c32 {
                put32(be, symtab, value as u32);
                put32(be, symtab, size as u32);
            }
            symtab.push(info);
            symtab.push(other);
            put16(be, symtab, shndx16);
            if !c32 {
                put64(be, symtab, value);
                put64(be, symtab, size);
            }
            if xindex {
                put32(be, &mut shndx_tab, shndx as u32);
            }
        };
    for shndx in &sect_syms {
        entry(&mut symtab, 0, STT_SECTION, 0, *shndx, 0, 0);
    }
    for &(k, off, name) in &maps {
        let at = strtab.len() as u32;
        strtab.extend_from_slice(name.as_bytes());
        strtab.push(0);
        entry(
            &mut symtab,
            at,
            STT_NOTYPE,
            0,
            sect_syms[k],
            u64::from(off),
            0,
        );
    }
    for (n, &(k, off)) in labels.iter().enumerate() {
        let at = strtab.len() as u32;
        strtab.extend_from_slice(format!(".Lpcrel_hi{n}").as_bytes());
        strtab.push(0);
        entry(
            &mut symtab,
            at,
            STT_NOTYPE,
            0,
            sect_syms[k],
            u64::from(off),
            0,
        );
    }
    let first_global =
        1 + nsect_syms + maps.len() + labels.len() + order.iter().filter(|&&i| local(i)).count();
    for &i in &order {
        let s = &obj.syms[i];
        let name = strtab.len() as u32;
        strtab.extend_from_slice(s.name.as_bytes());
        strtab.push(0);
        let (shndx, value, size, ty) = match (s.def, piece_of[i]) {
            (Some((sec, off, size)), Some(k)) => (
                sect_syms[k],
                u64::from(off - pieces[k].start),
                u64::from(size),
                if sec == Sect::Text {
                    STT_FUNC
                } else {
                    STT_OBJECT
                },
            ),
            _ => (0, 0, 0, if tls[i] { STT_TLS } else { STT_NOTYPE }),
        };
        let bind = if local(i) {
            STB_LOCAL
        } else if s.weak {
            STB_WEAK
        } else {
            STB_GLOBAL
        };
        let mut other = if s.hidden { STV_HIDDEN } else { 0 };
        // Every function has the 8-byte global entry.
        if obj.ppc64 && ty == STT_FUNC {
            other |= STO_PPC64_LOCAL8;
        }
        entry(
            &mut symtab,
            name,
            (bind << 4) | ty,
            other,
            shndx,
            value,
            size,
        );
    }
    let symtab_index = outs.len() + relas.iter().filter(|r| !r.1.is_empty()).count();
    for (target, rs) in &relas {
        if rs.is_empty() {
            continue;
        }
        let mut data = Vec::new();
        // A data word's addend may be in place (C initializers): RELA
        // takes it out.
        let abs = if obj.x86 {
            R_X86_64_64
        } else if riscv {
            if obj.rv32 { R_RISCV_32 } else { R_RISCV_64 }
        } else if obj.ppc64 {
            R_PPC64_ADDR64
        } else if obj.s390x {
            R_390_64
        } else {
            R_AARCH64_ABS64
        };
        for &(off, ty, sym, mut addend) in rs {
            if !rel && ty == abs && outs[*target].ty != SHT_NOBITS {
                let n = if c32 { 4 } else { 8 };
                let at = off as usize;
                if let Some(place) = outs[*target].data.get_mut(at..at + n) {
                    let mut w = [0u8; 8];
                    if be {
                        w[8 - n..].copy_from_slice(place);
                        addend += u64::from_be_bytes(w) as i64;
                    } else {
                        w[..n].copy_from_slice(place);
                        addend += if c32 {
                            i64::from(u32::from_le_bytes([w[0], w[1], w[2], w[3]]) as i32)
                        } else {
                            u64::from_le_bytes(w) as i64
                        };
                    }
                    place.fill(0);
                }
            }
            if c32 && riscv {
                put32(be, &mut data, off as u32);
                put32(be, &mut data, ((sym as u32) << 8) | ty);
                put32(be, &mut data, addend as u32);
            } else if c32 {
                // The addend in the place.
                let at = off as usize;
                let place = &mut outs[*target].data[at..at + 4];
                let v = u32::from_le_bytes(place.try_into().unwrap_or([0; 4]));
                place.copy_from_slice(&v.wrapping_add(addend as u32).to_le_bytes());
                put32(be, &mut data, off as u32);
                put32(be, &mut data, ((sym as u32) << 8) | ty);
            } else {
                put64(be, &mut data, off);
                put64(be, &mut data, ((sym as u64) << 32) | u64::from(ty));
                put64(be, &mut data, addend as u64);
            }
        }
        let (prefix, ty, entsize) = if rel {
            (".rel", SHT_REL, 8)
        } else if c32 {
            (".rela", SHT_RELA, 12)
        } else {
            (".rela", SHT_RELA, 24)
        };
        outs.push(Out {
            name: format!("{prefix}{}", outs[*target].name),
            ty,
            flags: SHF_INFO_LINK,
            data,
            align: if c32 { 4 } else { 8 },
            link: symtab_index as u32,
            info: *target as u32,
            entsize,
        });
    }
    let strtab_index = symtab_index + 1 + usize::from(xindex);
    outs.push(Out {
        name: ".symtab".to_owned(),
        ty: SHT_SYMTAB,
        flags: 0,
        data: symtab,
        align: if c32 { 4 } else { 8 },
        link: strtab_index as u32,
        info: first_global as u32,
        entsize: if c32 { 16 } else { 24 },
    });
    if xindex {
        outs.push(Out {
            name: ".symtab_shndx".to_owned(),
            ty: SHT_SYMTAB_SHNDX,
            flags: 0,
            data: shndx_tab,
            align: 4,
            link: symtab_index as u32,
            info: 0,
            entsize: 4,
        });
    }
    outs.push(Out {
        name: ".strtab".to_owned(),
        ty: SHT_STRTAB,
        flags: 0,
        data: strtab,
        align: 1,
        link: 0,
        info: 0,
        entsize: 0,
    });
    let mut shstrtab = vec![0u8];
    let mut names = Vec::new();
    for o in &outs {
        names.push(shstrtab.len() as u32);
        shstrtab.extend_from_slice(o.name.as_bytes());
        shstrtab.push(0);
    }
    names.push(shstrtab.len() as u32);
    shstrtab.extend_from_slice(b".shstrtab\0");
    outs.push(Out {
        name: ".shstrtab".to_owned(),
        ty: SHT_STRTAB,
        flags: 0,
        data: shstrtab,
        align: 1,
        link: 0,
        info: 0,
        entsize: 0,
    });
    // A count or index from `SHN_LORESERVE` on is in section 0's header.
    let (n, shstrndx) = (outs.len(), outs.len() - 1);
    if shstrndx >= SHN_LORESERVE {
        outs[0].link = shstrndx as u32;
    }

    if c32 {
        let (machine, flags) = if riscv {
            (EM_RISCV, EF_RISCV32)
        } else if arm {
            (EM_ARM, EF_ARM)
        } else {
            (EM_386, 0)
        };
        return layout32(&outs, &names, machine, flags);
    }
    // Layout: header, section contents, section headers.
    let mut b = vec![0u8; 64];
    let mut offs = Vec::new();
    for o in &outs {
        let a = o.align.max(1) as usize;
        b.resize(b.len().next_multiple_of(a), 0);
        offs.push(b.len() as u64);
        if o.ty != SHT_NOBITS {
            b.extend_from_slice(&o.data);
        }
    }
    b.resize(b.len().next_multiple_of(8), 0);
    let shoff = b.len() as u64;
    for (k, o) in outs.iter().enumerate() {
        put32(be, &mut b, names[k]);
        put32(be, &mut b, o.ty);
        put64(be, &mut b, o.flags);
        put64(be, &mut b, 0);
        put64(be, &mut b, if k == 0 { 0 } else { offs[k] });
        let size = if k == 0 && n >= SHN_LORESERVE {
            n
        } else {
            o.data.len()
        };
        put64(be, &mut b, size as u64);
        put32(be, &mut b, o.link);
        put32(be, &mut b, o.info);
        put64(be, &mut b, o.align);
        put64(be, &mut b, o.entsize);
    }
    let mut h = Vec::new();
    h.extend_from_slice(&[0x7f, b'E', b'L', b'F', 2, if be { 2 } else { 1 }, 1, 0]);
    h.resize(16, 0);
    put16(be, &mut h, 1); // ET_REL
    put16(
        be,
        &mut h,
        if x86 {
            EM_X86_64
        } else if riscv {
            EM_RISCV
        } else if obj.ppc64 {
            EM_PPC64
        } else if obj.s390x {
            EM_S390
        } else {
            EM_AARCH64
        },
    );
    put32(be, &mut h, 1);
    put64(be, &mut h, 0);
    put64(be, &mut h, 0);
    put64(be, &mut h, shoff);
    let flags = if riscv {
        EF_RISCV64
    } else if obj.ppc64 {
        EF_PPC64
    } else {
        0
    };
    put32(be, &mut h, flags);
    put16(be, &mut h, 64);
    put16(be, &mut h, 0);
    put16(be, &mut h, 0);
    put16(be, &mut h, 64);
    put16(be, &mut h, if n >= SHN_LORESERVE { 0 } else { n as u16 });
    put16(
        be,
        &mut h,
        if shstrndx >= SHN_LORESERVE {
            SHN_XINDEX
        } else {
            shstrndx as u16
        },
    );
    b[..64].copy_from_slice(&h);
    b
}

/// The ELF32 image of `outs` (`names`: their offsets in the last,
/// `.shstrtab`) for `machine` with `flags`: header, section contents,
/// section headers.
fn layout32(outs: &[Out], names: &[u32], machine: u16, flags: u32) -> Vec<u8> {
    // Every 32-bit target is little endian.
    let be = false;
    let (n, shstrndx) = (outs.len(), outs.len() - 1);
    let mut b = vec![0u8; 52];
    let mut offs = Vec::new();
    for o in outs {
        let a = o.align.max(1) as usize;
        b.resize(b.len().next_multiple_of(a), 0);
        offs.push(b.len() as u32);
        if o.ty != SHT_NOBITS {
            b.extend_from_slice(&o.data);
        }
    }
    b.resize(b.len().next_multiple_of(4), 0);
    let shoff = b.len() as u32;
    for (k, o) in outs.iter().enumerate() {
        put32(be, &mut b, names[k]);
        put32(be, &mut b, o.ty);
        put32(be, &mut b, o.flags as u32);
        put32(be, &mut b, 0);
        put32(be, &mut b, if k == 0 { 0 } else { offs[k] });
        let size = if k == 0 && n >= SHN_LORESERVE {
            n
        } else {
            o.data.len()
        };
        put32(be, &mut b, size as u32);
        put32(be, &mut b, o.link);
        put32(be, &mut b, o.info);
        put32(be, &mut b, o.align as u32);
        put32(be, &mut b, o.entsize as u32);
    }
    let mut h = Vec::new();
    h.extend_from_slice(&[0x7f, b'E', b'L', b'F', 1, 1, 1, 0]);
    h.resize(16, 0);
    put16(be, &mut h, 1); // ET_REL
    put16(be, &mut h, machine);
    put32(be, &mut h, 1);
    put32(be, &mut h, 0);
    put32(be, &mut h, 0);
    put32(be, &mut h, shoff);
    put32(be, &mut h, flags);
    put16(be, &mut h, 52);
    put16(be, &mut h, 0);
    put16(be, &mut h, 0);
    put16(be, &mut h, 40);
    put16(be, &mut h, if n >= SHN_LORESERVE { 0 } else { n as u16 });
    put16(
        be,
        &mut h,
        if shstrndx >= SHN_LORESERVE {
            SHN_XINDEX
        } else {
            shstrndx as u16
        },
    );
    b[..52].copy_from_slice(&h);
    b
}

/// ELF i386 relocation type of `kind` (the addend is in the place).
const fn reloc386(kind: RelKind) -> u32 {
    match kind {
        RelKind::I386Call => R_386_PLT32,
        RelKind::I386GotPc => R_386_GOTPC,
        RelKind::I386GotOff => R_386_GOTOFF,
        RelKind::I386Got32X => R_386_GOT32X,
        RelKind::I386TlsGotIe => R_386_TLS_GOTIE,
        _ => R_386_32,
    }
}

/// ELF 32-bit ARM relocation type of `kind` (the addend is in the place).
const fn reloc_arm(kind: RelKind) -> u32 {
    match kind {
        RelKind::ArmCall => R_ARM_CALL,
        RelKind::ArmMovwPrel => R_ARM_MOVW_PREL_NC,
        RelKind::ArmMovtPrel => R_ARM_MOVT_PREL,
        RelKind::ArmGotPrel => R_ARM_GOT_PREL,
        RelKind::ArmTlsIe => R_ARM_TLS_IE32,
        _ => R_ARM_ABS32,
    }
}

/// ELF RISC-V relocation type of `kind` (addends are 0).
const fn reloc_riscv(kind: RelKind, rv32: bool) -> u32 {
    match kind {
        RelKind::RvCall => R_RISCV_CALL_PLT,
        RelKind::RvPcHi => R_RISCV_PCREL_HI20,
        RelKind::RvGotHi => R_RISCV_GOT_HI20,
        RelKind::RvTlsIeHi => R_RISCV_TLS_GOT_HI20,
        RelKind::RvPcLoI => R_RISCV_PCREL_LO12_I,
        RelKind::RvTpHi => R_RISCV_TPREL_HI20,
        RelKind::RvTpAdd => R_RISCV_TPREL_ADD,
        RelKind::RvTpLo => R_RISCV_TPREL_LO12_I,
        _ if rv32 => R_RISCV_32,
        _ => R_RISCV_64,
    }
}

/// ELF powerpc64 relocation type and addend of `kind`: the entry's
/// `addi` is 4 bytes past the function's start, which its distance is
/// from.
const fn reloc_ppc64(kind: RelKind) -> (u32, i64) {
    match kind {
        RelKind::PpcCall => (R_PPC64_REL24, 0),
        RelKind::PpcEntryHa => (R_PPC64_REL16_HA, 0),
        RelKind::PpcEntryLo => (R_PPC64_REL16_LO, 4),
        RelKind::PpcTocHa => (R_PPC64_TOC16_HA, 0),
        RelKind::PpcTocLo => (R_PPC64_TOC16_LO, 0),
        RelKind::PpcGotHa => (R_PPC64_GOT16_HA, 0),
        RelKind::PpcGotLo => (R_PPC64_GOT16_LO_DS, 0),
        RelKind::PpcTlsIeHa => (R_PPC64_GOT_TPREL16_HA, 0),
        RelKind::PpcTlsIeLo => (R_PPC64_GOT_TPREL16_LO_DS, 0),
        RelKind::PpcTls => (R_PPC64_TLS, 0),
        _ => (R_PPC64_ADDR64, 0),
    }
}

/// ELF s390x relocation type and addend of `kind`: a relative-long
/// field is 2 bytes into its instruction, the distance from the
/// instruction.
const fn reloc_s390x(kind: RelKind) -> (u32, i64) {
    match kind {
        RelKind::S390Call => (R_390_PLT32DBL, 2),
        RelKind::S390Pc => (R_390_PC32DBL, 2),
        RelKind::S390Got => (R_390_GOTENT, 2),
        RelKind::S390TlsIe => (R_390_TLS_IEENT, 2),
        _ => (R_390_64, 0),
    }
}

/// ELF relocation type and addend of `kind`.
const fn reloc(kind: RelKind, x86: bool) -> (u32, i64) {
    if x86 {
        match kind {
            RelKind::X86Call => (R_X86_64_PLT32, -4),
            RelKind::X86GotLoad => (R_X86_64_REX_GOTPCRELX, -4),
            RelKind::X86Tls => (R_X86_64_GOTTPOFF, -4),
            RelKind::X86TlsGd => (R_X86_64_TLSGD, -4),
            RelKind::Abs64 => (R_X86_64_64, 0),
            _ => (R_X86_64_PC32, -4),
        }
    } else {
        match kind {
            RelKind::Branch26 => (R_AARCH64_CALL26, 0),
            RelKind::Page21 => (R_AARCH64_ADR_PREL_PG_HI21, 0),
            RelKind::PageOff12 => (R_AARCH64_ADD_ABS_LO12_NC, 0),
            RelKind::GotPage21 => (R_AARCH64_ADR_GOT_PAGE, 0),
            RelKind::GotPageOff12 => (R_AARCH64_LD64_GOT_LO12_NC, 0),
            RelKind::TlvPage21 => (R_AARCH64_TLSIE_ADR_GOTTPREL_PAGE21, 0),
            RelKind::TlvPageOff12 => (R_AARCH64_TLSIE_LD64_GOTTPREL_LO12_NC, 0),
            RelKind::TlsDescPage21 => (R_AARCH64_TLSDESC_ADR_PAGE21, 0),
            RelKind::TlsDescLd => (R_AARCH64_TLSDESC_LD64_LO12, 0),
            RelKind::TlsDescAdd => (R_AARCH64_TLSDESC_ADD_LO12, 0),
            RelKind::TlsDescCall => (R_AARCH64_TLSDESC_CALL, 0),
            _ => (R_AARCH64_ABS64, 0),
        }
    }
}

/// A piece's bytes with RELA data pointers' places cleared.
fn rela_data(data: &[u8], relocs: &[crate::obj::Reloc], rela: bool) -> Vec<u8> {
    let mut out = data.to_vec();
    if rela {
        for r in relocs.iter().filter(|r| r.kind == RelKind::Abs64) {
            let at = r.off as usize;
            out[at..at + 8].fill(0);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Past `SHN_LORESERVE` sections: the counts in section 0, the
    /// symbols' section indexes in `.symtab_shndx`.
    #[test]
    fn many_sections_use_extended_numbering() {
        let mut o = Obj::new(true, true, false);
        for i in 0..0xff00u32 {
            let s = o.sym(&format!("d{i}"));
            o.define_data(s, Sect::Const, &[1], 1, true);
        }
        let b = write(&o);
        if let Some(p) = std::env::var_os("NABI_DUMP_XINDEX") {
            std::fs::write(p, &b).unwrap();
        }
        let u16_at = |at: usize| u16::from_le_bytes([b[at], b[at + 1]]);
        let shoff = u64::from_le_bytes(b[40..48].try_into().unwrap()) as usize;
        assert_eq!((u16_at(60), u16_at(62)), (0, SHN_XINDEX));
        let n = u64::from_le_bytes(b[shoff + 32..shoff + 40].try_into().unwrap());
        let shstrndx = u32::from_le_bytes(b[shoff + 40..shoff + 44].try_into().unwrap());
        // The pieces, the stack note, symtab, its indexes, strtab, shstrtab.
        assert_eq!((n, shstrndx), (0xff00 + 6, 0xff00 + 5));
        let shndx = shoff + 64 * (0xff00 + 3);
        assert_eq!(
            u32::from_le_bytes(b[shndx + 4..shndx + 8].try_into().unwrap()),
            SHT_SYMTAB_SHNDX
        );
    }
}
