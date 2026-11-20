//! `x86_64` relocation types, GOT relaxation and the `__tls_get_addr`
//! calls TLS relaxations drop.

use crate::elf::reloc::SymKind;
use crate::symtab::Reloc;

pub const R_64: u16 = 1;
pub const PC32: u16 = 2;
pub const GOT32: u16 = 3;
pub const PLT32: u16 = 4;
pub const GLOB_DAT: u32 = 6;
pub const RELATIVE: u32 = 8;
pub const GOTPCREL: u16 = 9;
pub const R_32: u16 = 10;
pub const R_32S: u16 = 11;
pub const R_16: u16 = 12;
pub const PC16: u16 = 13;
pub const R_8: u16 = 14;
pub const PC8: u16 = 15;
pub const DTPOFF64: u16 = 17;
pub const TPOFF64: u16 = 18;
/// Dynamic: the module of a thread-local (`R_X86_64_DTPMOD64`).
pub const DTPMOD64: u32 = 16;
pub const TLSGD: u16 = 19;
pub const TLSLD: u16 = 20;
pub const DTPOFF32: u16 = 21;
pub const GOTTPOFF: u16 = 22;
pub const TPOFF32: u16 = 23;
pub const PC64: u16 = 24;
pub const GOTOFF64: u16 = 25;
pub const GOTPC32: u16 = 26;
pub const GOT64: u16 = 27;
pub const GOTPCREL64: u16 = 28;
pub const GOTPC64: u16 = 29;
pub const SIZE32: u16 = 32;
pub const SIZE64: u16 = 33;
pub const GOTPC32_TLSDESC: u16 = 34;
pub const TLSDESC_CALL: u16 = 35;
pub const GOTPCRELX: u16 = 41;
pub const REX_GOTPCRELX: u16 = 42;

/// `x86_64`: the `__tls_get_addr` call after a general or local dynamic
/// TLS sequence, which every relaxation removes.
pub fn drop_tls_get_addr(rs: &mut Vec<Reloc>) {
    let mut k = 0;
    while k + 1 < rs.len() {
        let gap = match rs[k].kind {
            TLSGD => 8,
            TLSLD => 5,
            _ => 0,
        };
        if gap != 0 && rs[k + 1].off == rs[k].off + gap {
            rs.remove(k + 1);
        }
        k += 1;
    }
}

/// `x86_64` `mov foo@GOTPCREL(%rip), %reg` of a local symbol: relaxed to
/// `lea foo(%rip), %reg`.
#[must_use]
pub fn relax_got(b: &[u8], at: usize, ty: u16, kind: SymKind) -> bool {
    kind == SymKind::Local
        && matches!(ty, GOTPCRELX | REX_GOTPCRELX)
        && at >= 2
        && b[at - 2] == 0x8b
}
