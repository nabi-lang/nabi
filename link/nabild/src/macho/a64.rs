//! arm64 Mach-O relocation types and instruction patching.

pub const UNSIGNED: u16 = 0;
pub const SUBTRACTOR: u16 = 1;
pub const BRANCH26: u16 = 2;
pub const PAGE21: u16 = 3;
pub const PAGEOFF12: u16 = 4;
pub const GOT_LOAD_PAGE21: u16 = 5;
pub const GOT_LOAD_PAGEOFF12: u16 = 6;
pub const POINTER_TO_GOT: u16 = 7;
pub const TLVP_LOAD_PAGE21: u16 = 8;
pub const TLVP_LOAD_PAGEOFF12: u16 = 9;
pub const ADDEND: u16 = 10;

/// `UNWIND_ARM64_MODE_DWARF`.
pub const UNWIND_DWARF: u32 = 0x0300_0000;
pub const STUB_SIZE: u64 = 12;

/// `b` / `bl` at `p` to `t`.
pub fn branch26(insn: u32, p: u64, t: u64) -> Result<u32, String> {
    let d = t.wrapping_sub(p) as i64;
    if d & 3 != 0 || !(-(1 << 27)..(1 << 27)).contains(&d) {
        return Err(format!("branch from {p:#x} to {t:#x} is out of range"));
    }
    Ok((insn & 0xfc00_0000) | ((d >> 2) as u32 & 0x03ff_ffff))
}

/// `adrp` at `p` of `t`'s page.
pub fn page21(insn: u32, p: u64, t: u64) -> Result<u32, String> {
    let d = ((t & !0xfff) as i64).wrapping_sub((p & !0xfff) as i64) >> 12;
    if !(-(1 << 20)..(1 << 20)).contains(&d) {
        return Err(format!("adrp from {p:#x} to {t:#x} is out of range"));
    }
    let imm = d as u32 & 0x1f_ffff;
    Ok((insn & 0x9f00_001f) | ((imm & 3) << 29) | ((imm >> 2) << 5))
}

/// The low 12 bits of `t` into an `add` or a load / store, scaled by its
/// access size.
pub fn pageoff12(insn: u32, t: u64) -> Result<u32, String> {
    let lo = (t & 0xfff) as u32;
    let scale = if insn & 0x3b00_0000 == 0x3900_0000 {
        let size = insn >> 30;
        // A 128-bit vector load / store.
        if size == 0 && insn & 0x0480_0000 == 0x0480_0000 {
            4
        } else {
            size
        }
    } else {
        0
    };
    if lo & ((1 << scale) - 1) != 0 {
        return Err(format!(
            "{t:#x} is not aligned for a {}-byte access",
            1 << scale
        ));
    }
    Ok((insn & 0xffc0_03ff) | ((lo >> scale) << 10))
}

/// `ldr xT, [xN, #off]` relaxed to `add xT, xN, #off` (the address
/// itself instead of a GOT or TLV slot's contents).
pub fn ldr_to_add(insn: u32) -> Result<u32, String> {
    if insn & 0xffc0_0000 != 0xf940_0000 {
        return Err(format!(
            "expected a 64-bit ldr to relax, found {insn:#010x}"
        ));
    }
    Ok(0x9100_0000 | (insn & 0x3ff))
}

/// A stub at `p` jumping through the pointer at `slot`:
/// `adrp x16, slot@page; ldr x16, [x16, slot@pageoff]; br x16`.
pub fn stub(p: u64, slot: u64) -> Result<[u8; 12], String> {
    let adrp = page21(0x9000_0010, p, slot)?;
    let ldr = pageoff12(0xf940_0210, slot)?;
    let mut b = [0u8; 12];
    b[..4].copy_from_slice(&adrp.to_le_bytes());
    b[4..8].copy_from_slice(&ldr.to_le_bytes());
    b[8..].copy_from_slice(&0xd61f_0200u32.to_le_bytes());
    Ok(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patches_match_the_assembler() {
        // bl +0x100
        assert_eq!(branch26(0x9400_0000, 0x1000, 0x1100).unwrap(), 0x9400_0040);
        // bl -4
        assert_eq!(branch26(0x9400_0000, 0x1000, 0xffc).unwrap(), 0x97ff_ffff);
        assert!(branch26(0x9400_0000, 0, 1 << 28).is_err());
        // adrp x0, +2 pages
        assert_eq!(
            page21(0x9000_0000, 0x1_0000_0ffc, 0x1_0000_2010).unwrap(),
            0xd000_0000
        );
        // ldr x1, [x0, #0x18]
        assert_eq!(pageoff12(0xf940_0001, 0x2018).unwrap(), 0xf940_0c01);
        assert!(pageoff12(0xf940_0001, 0x2014).is_err());
        // ldr q0, [x0, #0x20]
        assert_eq!(pageoff12(0x3dc0_0000, 0x20).unwrap(), 0x3dc0_0800);
        // add x0, x0, #0x123
        assert_eq!(pageoff12(0x9100_0000, 0x5123).unwrap(), 0x9104_8c00);
        assert_eq!(ldr_to_add(0xf940_0c01).unwrap(), 0x9100_0001);
    }
}
