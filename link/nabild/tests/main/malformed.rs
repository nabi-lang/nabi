//! Inputs the fuzzer found: the readers reject them instead of panicking
//! or asking for an impossible allocation.

/// An `x86_64` ELF relocatable object whose section table is `shdrs`
/// (`(type, offset, size)` each, after the null section).
fn elf(shdrs: &[(u32, u64, u64)]) -> Vec<u8> {
    let mut b = vec![0u8; 64];
    b[..4].copy_from_slice(b"\x7fELF");
    b[4] = 2; // ELFCLASS64
    b[5] = 1; // little endian
    b[6] = 1;
    b[16..18].copy_from_slice(&1u16.to_le_bytes()); // ET_REL
    b[18..20].copy_from_slice(&62u16.to_le_bytes()); // EM_X86_64
    b[20..24].copy_from_slice(&1u32.to_le_bytes());
    b[0x28..0x30].copy_from_slice(&64u64.to_le_bytes()); // e_shoff
    b[0x34..0x36].copy_from_slice(&64u16.to_le_bytes()); // e_ehsize
    b[0x3a..0x3c].copy_from_slice(&64u16.to_le_bytes()); // e_shentsize
    b[0x3c..0x3e].copy_from_slice(&(shdrs.len() as u16 + 1).to_le_bytes());
    b.extend_from_slice(&[0u8; 64]);
    for &(ty, off, size) in shdrs {
        let mut h = [0u8; 64];
        h[4..8].copy_from_slice(&ty.to_le_bytes());
        h[24..32].copy_from_slice(&off.to_le_bytes());
        h[32..40].copy_from_slice(&size.to_le_bytes());
        h[48..56].copy_from_slice(&8u64.to_le_bytes());
        b.extend_from_slice(&h);
    }
    b
}

#[test]
fn elf_symbol_table_past_the_end() {
    let o = elf(&[(2, 0, 0x00aa_aaab_de00_0000)]);
    assert!(nabild::read_input(&o).is_err());
}

#[test]
fn elf_section_count_past_the_end() {
    let mut o = elf(&[]);
    o[0x3c..0x3e].copy_from_slice(&0xffffu16.to_le_bytes());
    assert!(nabild::read_input(&o).is_err());
}
