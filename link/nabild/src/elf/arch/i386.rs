//! i386 relocation types, GOT relaxation and REL relocation application.

use crate::elf::reloc::{Site, SymKind, Values, put, range};
use crate::symtab::Reloc;

pub const R_32: u16 = 1;
pub const PC32: u16 = 2;
pub const GOT32: u16 = 3;
pub const PLT32: u16 = 4;
pub const GLOB_DAT: u32 = 6;
pub const JUMP_SLOT: u16 = 7;
pub const RELATIVE: u32 = 8;
pub const GOTOFF: u16 = 9;
pub const GOTPC: u16 = 10;
pub const TLS_TPOFF: u32 = 14;
pub const TLS_IE: u16 = 15;
pub const TLS_GOTIE: u16 = 16;
pub const TLS_LE: u16 = 17;
pub const TLS_GD: u16 = 18;
pub const TLS_LDM: u16 = 19;
pub const R_16: u16 = 20;
pub const PC16: u16 = 21;
pub const R_8: u16 = 22;
pub const PC8: u16 = 23;
pub const TLS_LDO_32: u16 = 32;
pub const TLS_LE_32: u16 = 34;
pub const TLS_DTPOFF32: u16 = 36;
pub const SIZE32: u16 = 38;
pub const TLS_GOTDESC: u16 = 39;
pub const TLS_DESC_CALL: u16 = 40;
pub const GOT32X: u16 = 43;

/// i386: the `___tls_get_addr` call after a general or local dynamic
/// TLS sequence (`call rel32` or `call *disp32(%reg)`), which every
/// relaxation removes.
pub fn drop_tls_get_addr(rs: &mut Vec<Reloc>) {
    let mut k = 0;
    while k + 1 < rs.len() {
        let (off, next) = (rs[k].off, rs[k + 1].off);
        if matches!(rs[k].kind, TLS_GD | TLS_LDM) && next > off && next <= off + 6 {
            rs.remove(k + 1);
        }
        k += 1;
    }
}

/// i386 `mov foo@GOT(%reg), %reg2` of a local symbol: relaxed to
/// `lea foo@GOTOFF(%reg), %reg2`, as GNU ld does.
#[must_use]
pub fn relax_got(b: &[u8], at: usize, ty: u16, kind: SymKind) -> bool {
    kind == SymKind::Local && ty == GOT32X && at >= 2 && b[at - 2] == 0x8b && !got_abs(b, at)
}

/// i386 `foo@GOT` without a base register (position-dependent code).
const fn got_abs(b: &[u8], at: usize) -> bool {
    at >= 1 && b[at - 1] & 0xc7 == 0x5
}

/// i386: REL relocations, so a dynamic relocation's addend stays in the
/// place.
#[allow(clippy::too_many_lines)]
pub(crate) fn apply(
    s: Site,
    b: &mut [u8],
    at: usize,
    v: &Values,
    relax: bool,
    name: &str,
) -> Result<(), String> {
    let sa = (v.s as i64).wrapping_add(v.a);
    let pc = sa.wrapping_sub(v.p as i64);
    let gb = v.got_base as i64;
    let dso = s.kind == SymKind::Dso;
    let put32 = |b: &mut [u8], val: i64| -> Result<(), String> {
        range(name, val, 32, true)?;
        put(b, at, val as u64, 4);
        Ok(())
    };
    let seq_err = |what: &str| Err(format!("{name}: unsupported {what} TLS sequence"));
    match s.ty {
        R_32 => {
            let val = if dso { v.a } else { sa };
            range(name, val, 32, false)?;
            put(b, at, val as u64, 4);
        }
        R_16 => {
            range(name, sa, 16, false)?;
            put(b, at, sa as u64, 2);
        }
        R_8 => {
            range(name, sa, 8, false)?;
            put(b, at, sa as u64, 1);
        }
        PC32 | PLT32 => put32(b, pc)?,
        PC16 => {
            range(name, pc, 17, true)?;
            put(b, at, pc as u64, 2);
        }
        PC8 => {
            range(name, pc, 8, true)?;
            put(b, at, pc as u64, 1);
        }
        GOT32 | GOT32X if got_abs(b, at) => {
            if s.pie {
                return Err(format!(
                    "relocation {name} without a base register cannot be used in a PIE; \
                     recompile with -fPIC"
                ));
            }
            put32(b, (v.g as i64).wrapping_add(v.a))?;
        }
        GOT32 | GOT32X if relax => {
            b[at - 2] = 0x8d;
            put32(b, sa.wrapping_sub(gb))?;
        }
        GOT32 | GOT32X => put32(b, (v.g as i64).wrapping_add(v.a).wrapping_sub(gb))?,
        GOTOFF => put32(b, sa.wrapping_sub(gb))?,
        GOTPC => put32(b, gb.wrapping_add(v.a).wrapping_sub(v.p as i64))?,
        SIZE32 => put32(b, (v.size as i64).wrapping_add(v.a))?,
        TLS_LE => put32(b, v.tp + v.a)?,
        TLS_LE_32 => put32(b, -(v.tp + v.a))?,
        TLS_DTPOFF32 => put32(b, v.dtp + v.a)?,
        // Relaxed with its TLS_LDM in code; DWARF wants the offset in the
        // module's block.
        TLS_LDO_32 if s.alloc => put32(b, v.tp + v.a)?,
        TLS_LDO_32 => put32(b, v.dtp + v.a)?,
        TLS_GD => {
            // leal x@tlsgd(,%ebx,1), %eax; call ___tls_get_addr@plt
            // or leal x@tlsgd(%reg), %eax; call *___tls_get_addr@got(%reg)
            let sib = at >= 3 && b[at - 2] == 0x04;
            let w = if sib {
                at.wrapping_sub(3)
            } else {
                at.wrapping_sub(2)
            };
            if at < 3 || b.len() < w + 12 || b[w] != 0x8d || !matches!(b[at + 4], 0xe8 | 0xff) {
                return seq_err("general dynamic");
            }
            let (op, val) = if dso {
                // movl %gs:0, %eax; addl x@gotntpoff(%ebx), %eax
                (0x03, (v.tg as i64).wrapping_sub(gb))
            } else {
                // movl %gs:0, %eax; subl $x@ntpoff, %eax
                (0x81, -(v.tp + v.a))
            };
            let modrm = if dso { 0x83 } else { 0xe8 };
            b[w..w + 8].copy_from_slice(&[0x65, 0xa1, 0, 0, 0, 0, op, modrm]);
            range(name, val, 32, true)?;
            put(b, w + 8, val as u64, 4);
        }
        TLS_LDM => {
            // leal x@tlsldm(%ebx), %eax; call ___tls_get_addr
            if at < 2 || b.len() < at + 10 || b[at - 2] != 0x8d {
                return seq_err("local dynamic");
            }
            match b[at + 4] {
                // movl %gs:0, %eax; nop; leal 0(%esi,1), %esi
                0xe8 => b[at - 2..at + 9]
                    .copy_from_slice(&[0x65, 0xa1, 0, 0, 0, 0, 0x90, 0x8d, 0x74, 0x26, 0]),
                // movl %gs:0, %eax; leal 0(%esi), %esi
                0xff => b[at - 2..at + 10]
                    .copy_from_slice(&[0x65, 0xa1, 0, 0, 0, 0, 0x8d, 0xb6, 0, 0, 0, 0]),
                _ => return seq_err("local dynamic"),
            }
        }
        TLS_IE | TLS_GOTIE if !dso => {
            if at < 2 {
                return seq_err("initial exec");
            }
            let reg = (b[at - 1] >> 3) & 7;
            if s.ty == TLS_IE && b[at - 1] == 0xa1 {
                // movl x@indntpoff, %eax -> movl $x, %eax
                b[at - 1] = 0xb8;
            } else if b[at - 2] == 0x8b {
                // movl x@gotntpoff(%reg), %reg2 -> movl $x, %reg2
                b[at - 2] = 0xc7;
                b[at - 1] = 0xc0 | reg;
            } else if b[at - 2] == 0x03 {
                if s.ty == TLS_IE {
                    // addl x@indntpoff, %reg -> addl $x, %reg
                    b[at - 2] = 0x81;
                    b[at - 1] = 0xc0 | reg;
                } else {
                    // addl x@gotntpoff(%reg), %reg2 -> leal x(%reg2), %reg2
                    b[at - 2] = 0x8d;
                    b[at - 1] = 0x80 | (reg << 3) | reg;
                }
            } else {
                return seq_err("initial exec");
            }
            put32(b, v.tp + v.a)?;
        }
        TLS_GOTIE => put32(b, (v.tg as i64).wrapping_add(v.a).wrapping_sub(gb))?,
        TLS_GOTDESC => {
            // leal x@tlsdesc(%ebx), %eax
            if at < 2 || b[at - 2..at] != [0x8d, 0x83] {
                return seq_err("descriptor");
            }
            if dso {
                // movl x@gotntpoff(%ebx), %eax
                b[at - 2] = 0x8b;
                put32(b, (v.tg as i64).wrapping_sub(gb))?;
            } else {
                // leal x@ntpoff, %eax
                b[at - 1] = 0x05;
                put32(b, v.tp + v.a)?;
            }
        }
        TLS_DESC_CALL => {
            // call *x@tlsdesc(%eax) -> xchg %ax, %ax
            if b.len() < at + 2 || b[at] != 0xff || b[at + 1] != 0x10 {
                return seq_err("descriptor call");
            }
            b[at] = 0x66;
            b[at + 1] = 0x90;
        }
        _ => return Err(format!("unsupported relocation {name}")),
    }
    Ok(())
}
