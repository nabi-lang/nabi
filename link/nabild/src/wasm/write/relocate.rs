//! Relocation values and their application.

#[allow(clippy::wildcard_imports)]
use crate::wasm::write::*;

/// The address of `off` in data chunk `atom`; a TLS symbol's is its
/// offset in the TLS segment.
pub(crate) fn data_va(l: &Link, out: &Out, atom: AtomId, off: u64, tls: bool) -> u64 {
    let base = l.atoms[atom as usize].addr;
    let va = match out.pieces.get(&atom) {
        Some((_, m)) => base + piece(m, off),
        None => base + off,
    };
    if tls { va - out.tls_start } else { va }
}

/// The value relocation `r` of file `fi` stores; `place` is its address
/// (data chunks), `tomb` what a dead target gets (0: the addend).
pub(crate) fn value(
    l: &Link,
    out: &Out,
    fi: u32,
    r: &Reloc,
    place: u64,
    tomb: u64,
) -> Result<u64, String> {
    let id = match r.target {
        Target::Atom(_) if r.kind == R_TYPE_INDEX_LEB => {
            let sig = l.files[fi as usize].types[r.addend as usize];
            return Ok(u64::from(out.type_of.get(&sig).copied().unwrap_or(0)));
        }
        Target::Atom(_) => return Ok(0),
        Target::Sym(id) => id,
    };
    let x = l.ext(id);
    let dead = || if tomb != 0 { tomb } else { r.addend as u64 };
    if x.kind != Some(SYM_SECTION) && !l.live(id) {
        return Ok(dead());
    }
    let def = l.syms.get(id).def;
    Ok(match r.kind {
        R_TABLE_INDEX_SLEB
        | R_TABLE_INDEX_I32
        | R_TABLE_INDEX_SLEB64
        | R_TABLE_INDEX_I64
        | R_TABLE_INDEX_REL_SLEB
        | R_TABLE_INDEX_REL_SLEB64 => {
            let s = fn_key(l, out, id)
                .and_then(|k| out.slot.get(&k))
                .copied()
                .unwrap_or(0);
            if matches!(r.kind, R_TABLE_INDEX_REL_SLEB | R_TABLE_INDEX_REL_SLEB64) {
                u64::from(s.saturating_sub(1))
            } else {
                u64::from(s)
            }
        }
        R_MEMORY_ADDR_LEB
        | R_MEMORY_ADDR_SLEB
        | R_MEMORY_ADDR_I32
        | R_MEMORY_ADDR_REL_SLEB
        | R_MEMORY_ADDR_LEB64
        | R_MEMORY_ADDR_SLEB64
        | R_MEMORY_ADDR_I64
        | R_MEMORY_ADDR_REL_SLEB64
        | R_MEMORY_ADDR_TLS_SLEB
        | R_MEMORY_ADDR_TLS_SLEB64
        | R_MEMORY_ADDR_LOCREL_I32 => {
            let v = match def {
                Def::Defined { atom, off } => {
                    data_va(l, out, atom, off, x.flags & crate::wasm::TLS != 0)
                }
                Def::Absolute(v) => v,
                _ => return Ok(0),
            };
            let v = v.wrapping_add(r.addend as u64);
            if r.kind == R_MEMORY_ADDR_LOCREL_I32 {
                v.wrapping_sub(place)
            } else {
                v
            }
        }
        R_GLOBAL_INDEX_LEB | R_GLOBAL_INDEX_I32 if x.kind != Some(SYM_GLOBAL) => {
            u64::from(got_index(out, id).unwrap_or(0))
        }
        R_FUNCTION_INDEX_LEB | R_FUNCTION_INDEX_I32 | R_GLOBAL_INDEX_LEB | R_GLOBAL_INDEX_I32
        | R_TAG_INDEX_LEB | R_TABLE_NUMBER_LEB => {
            let Some(i) = index_of(l, out, id) else {
                return Err(format!(
                    "{}: no index for `{}`",
                    l.file_name(fi),
                    l.name(id)
                ));
            };
            u64::from(i)
        }
        R_FUNCTION_OFFSET_I32 | R_FUNCTION_OFFSET_I64 => {
            let Def::Defined { atom, .. } = def else {
                return Ok(dead());
            };
            // A folded function's debug info.
            if l.moved.contains(&id) {
                return Ok(dead());
            }
            let a = &l.atoms[atom as usize];
            let lead = if a.file == u32::MAX {
                0
            } else {
                let mut at = a.off as usize;
                let _ = read_uleb(&l.files[a.file as usize].data, &mut at);
                at - a.off as usize
            };
            let base = out.code_off.get(atom as usize).copied().unwrap_or(0);
            (u64::from(base) + lead as u64).wrapping_add(r.addend as u64)
        }
        R_SECTION_OFFSET_I32 => {
            let Def::Defined { atom, .. } = def else {
                return Ok(r.addend as u64);
            };
            let base = out.custom_off.get(&atom).copied().unwrap_or(0);
            match out.pieces.get(&atom) {
                Some((_, m)) => base + piece(m, r.addend as u64),
                None => base.wrapping_add(r.addend as u64),
            }
        }
        k => return Err(format!("{}: relocation type {k}", l.file_name(fi))),
    })
}

/// Apply `relocs` of file `fi` to `b`, the chunk's bytes; `va` is its
/// address (data chunks).
pub(crate) fn relocate(
    l: &Link,
    out: &Out,
    fi: u32,
    relocs: &[Reloc],
    b: &mut [u8],
    va: u64,
    tomb: u64,
) -> Result<(), String> {
    for r in relocs {
        let v = value(l, out, fi, r, va + u64::from(r.off), tomb)?;
        let at = r.off as usize;
        let place = b
            .get_mut(at..)
            .ok_or_else(|| format!("{}: relocation past its chunk", l.file_name(fi)))?;
        match r.kind {
            R_TYPE_INDEX_LEB | R_FUNCTION_INDEX_LEB | R_GLOBAL_INDEX_LEB | R_TAG_INDEX_LEB
            | R_MEMORY_ADDR_LEB | R_TABLE_NUMBER_LEB => put_uleb5(place, v as u32),
            R_TABLE_INDEX_SLEB
            | R_TABLE_INDEX_REL_SLEB
            | R_MEMORY_ADDR_SLEB
            | R_MEMORY_ADDR_REL_SLEB
            | R_MEMORY_ADDR_TLS_SLEB => put_sleb5(place, v as i32),
            R_TABLE_INDEX_I32
            | R_MEMORY_ADDR_I32
            | R_FUNCTION_OFFSET_I32
            | R_FUNCTION_INDEX_I32
            | R_SECTION_OFFSET_I32
            | R_GLOBAL_INDEX_I32
            | R_MEMORY_ADDR_LOCREL_I32 => {
                place[..4].copy_from_slice(&(v as u32).to_le_bytes());
            }
            R_TABLE_INDEX_I64 | R_MEMORY_ADDR_I64 | R_FUNCTION_OFFSET_I64 => {
                place[..8].copy_from_slice(&v.to_le_bytes());
            }
            k => {
                return Err(format!(
                    "{}: relocation type {k} is not supported in wasm32",
                    l.file_name(fi)
                ));
            }
        }
    }
    Ok(())
}
