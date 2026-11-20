//! aarch64 relocation types and instruction field encoders.

use crate::elf::reloc::range;

pub const ABS64: u16 = 257;
pub const ABS32: u16 = 258;
pub const ABS16: u16 = 259;
pub const PREL64: u16 = 260;
pub const PREL32: u16 = 261;
pub const PREL16: u16 = 262;
pub const MOVW_UABS_G0: u16 = 263;
pub const MOVW_UABS_G3: u16 = 269;
pub const LD_PREL_LO19: u16 = 273;
pub const ADR_PREL_LO21: u16 = 274;
pub const ADR_PREL_PG_HI21: u16 = 275;
pub const ADR_PREL_PG_HI21_NC: u16 = 276;
pub const ADD_ABS_LO12_NC: u16 = 277;
pub const LDST8_ABS_LO12_NC: u16 = 278;
pub const TSTBR14: u16 = 279;
pub const CONDBR19: u16 = 280;
pub const JUMP26: u16 = 282;
pub const CALL26: u16 = 283;
pub const LDST16_ABS_LO12_NC: u16 = 284;
pub const LDST32_ABS_LO12_NC: u16 = 285;
pub const LDST64_ABS_LO12_NC: u16 = 286;
pub const LDST128_ABS_LO12_NC: u16 = 299;
pub const GOT_LD_PREL19: u16 = 309;
pub const ADR_GOT_PAGE: u16 = 311;
pub const LD64_GOT_LO12_NC: u16 = 312;
pub const TLSIE_ADR_GOTTPREL_PAGE21: u16 = 541;
pub const TLSIE_LD64_GOTTPREL_LO12_NC: u16 = 542;
pub const TLSIE_LD_GOTTPREL_PREL19: u16 = 543;
pub const TLSLE_MOVW_TPREL_G2: u16 = 544;
pub const TLSLE_MOVW_TPREL_G1: u16 = 545;
pub const TLSLE_MOVW_TPREL_G1_NC: u16 = 546;
pub const TLSLE_MOVW_TPREL_G0: u16 = 547;
pub const TLSLE_MOVW_TPREL_G0_NC: u16 = 548;
pub const TLSLE_ADD_TPREL_HI12: u16 = 549;
pub const TLSLE_ADD_TPREL_LO12: u16 = 550;
pub const TLSLE_ADD_TPREL_LO12_NC: u16 = 551;
pub const TLSLE_LDST8_TPREL_LO12: u16 = 552;
pub const TLSLE_LDST64_TPREL_LO12_NC: u16 = 559;
pub const TLSDESC_ADR_PAGE21: u16 = 562;
pub const TLSDESC_LD64_LO12: u16 = 563;
pub const TLSDESC_ADD_LO12: u16 = 564;
pub const TLSDESC_CALL: u16 = 569;
pub const TLSLE_LDST128_TPREL_LO12: u16 = 570;
pub const TLSLE_LDST128_TPREL_LO12_NC: u16 = 571;
pub const GLOB_DAT: u32 = 1025;
pub const RELATIVE: u32 = 1027;
pub const TLS_DTPREL64: u16 = 1029;
pub const TLS_TPREL64: u32 = 1030;
pub const TLSDESC: u32 = 1031;

pub(crate) const fn movw(i: u32, imm: u32) -> u32 {
    (i & !(0xffff << 5)) | ((imm & 0xffff) << 5)
}

/// A `movz` / `movn` of a signed value's piece: negative values turn
/// `movz` into `movn` of the inverted piece.
pub(crate) const fn movw_signed(mut i: u32, mut imm: u32) -> u32 {
    if i & (1 << 29) == 0 {
        if imm & 0x1_0000 != 0 {
            imm ^= 0xffff;
            i &= !(1 << 30);
        } else {
            i |= 1 << 30;
        }
    }
    movw(i, imm)
}

pub(crate) fn imm19(name: &str, i: u32, d: i64) -> Result<u32, String> {
    range(name, d, 21, true)?;
    if d & 3 != 0 {
        return Err(format!("relocation {name}: {d:#x} is not 4-byte aligned"));
    }
    Ok((i & !(0x7_ffff << 5)) | ((((d >> 2) as u32) & 0x7_ffff) << 5))
}

pub(crate) const fn adr(i: u32, imm: u32) -> u32 {
    (i & 0x9f00_001f) | ((imm & 3) << 29) | (((imm >> 2) & 0x7_ffff) << 5)
}
