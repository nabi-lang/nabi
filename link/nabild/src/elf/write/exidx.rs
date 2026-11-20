//! 32-bit ARM index table, thunks and build attributes.

#[allow(clippy::wildcard_imports)]
use crate::elf::write::*;

/// A 32-bit ARM index table entry.
#[derive(Clone, Copy, Debug)]
pub(crate) enum ExEnt {
    /// The entry at this offset of an input `.ARM.exidx` atom.
    Copy(AtomId, u32),
    /// `EXIDX_CANTUNWIND` from the start of a code atom / of an output
    /// section (by index) / from the end of the code.
    CantAtom(AtomId),
    CantSec(usize),
    End,
}

/// The 32-bit ARM index table for the code of `secs` in address order:
/// each code atom's entries, or `EXIDX_CANTUNWIND` from it (merged with
/// one before), and one from the end of the code.
pub(crate) fn exidx_entries(l: &Link, secs: &[OSec]) -> Vec<ExEnt> {
    let mut of: HashMap<AtomId, Vec<AtomId>> = HashMap::default();
    for &(code, x) in &l.exidx {
        if l.atoms[x as usize].live {
            of.entry(code).or_default().push(x);
        }
    }
    let mut v: Vec<ExEnt> = Vec::new();
    let cant = |v: &mut Vec<ExEnt>, e: ExEnt| {
        if !matches!(
            v.last(),
            Some(ExEnt::CantAtom(_) | ExEnt::CantSec(_) | ExEnt::End)
        ) {
            v.push(e);
        }
    };
    let code = secs
        .iter()
        .enumerate()
        .filter(|(_, s)| s.alloc() && s.flags & SHF_EXECINSTR != 0);
    for (i, s) in code {
        if s.synth != Synth::Input {
            cant(&mut v, ExEnt::CantSec(i));
            continue;
        }
        for &a in &s.atoms {
            if l.atoms[a as usize].size == 0 {
                continue;
            }
            match of.get(&a) {
                Some(xs) => {
                    for &x in xs {
                        for k in 0..l.atoms[x as usize].size / 8 {
                            v.push(ExEnt::Copy(x, 8 * k as u32));
                        }
                    }
                }
                None => cant(&mut v, ExEnt::CantAtom(a)),
            }
        }
    }
    if !v.is_empty() {
        cant(&mut v, ExEnt::End);
    }
    v
}

/// The 32-bit ARM index table, the thunks and the build attributes.
pub(crate) fn write_arm(out: &Out<'_>, buf: &mut [u8]) {
    let en = out.l.arch.endian();
    let l = out.l;
    if let Some(t) = out.sec(Synth::Thunks) {
        for (k, &(id, from_thumb)) in out.thunks.iter().enumerate() {
            let at = t.off as usize + (arm32::THUNK as usize) * k;
            let addr = t.addr + arm32::THUNK * k as u64;
            arm32::write_thunk(buf, at, addr, out.addr(Target::Sym(id)), from_thumb);
        }
    }
    if let Some(a) = out.sec(Synth::Attributes) {
        let at = a.off as usize;
        buf[at..at + arm32::ATTRIBUTES.len()].copy_from_slice(&arm32::ATTRIBUTES);
    }
    let Some(x) = out.sec(Synth::Exidx) else {
        return;
    };
    let code_end = out
        .secs
        .iter()
        .filter(|s| s.alloc() && s.flags & SHF_EXECINSTR != 0)
        .map(|s| s.addr + s.size)
        .max()
        .unwrap_or(0);
    let word = |d: &[u8], at: usize| u32::from_le_bytes([d[at], d[at + 1], d[at + 2], d[at + 3]]);
    for (i, e) in out.exidx.iter().enumerate() {
        let at = x.off as usize + 8 * i;
        let addr = x.addr + 8 * i as u64;
        let cant = |to: u64| (arm32::prel31(0, addr, to), arm32::EXIDX_CANTUNWIND);
        let (w0, w1) = match *e {
            ExEnt::Copy(a, off) => {
                let atom = &l.atoms[a as usize];
                let f = &l.files[atom.file as usize];
                let src = (f.sects[atom.sect as usize].off + atom.off) as usize + off as usize;
                let rel = |o: u32| atom.relocs.iter().find(|r| r.off == o);
                let to = |r: &crate::symtab::Reloc| (out.addr(r.target) as i64 + r.addend) as u64;
                let w0 =
                    rel(off).map_or_else(|| word(&f.data, src), |r| arm32::prel31(0, addr, to(r)));
                let w1 = rel(off + 4).map_or_else(
                    || word(&f.data, src + 4),
                    |r| arm32::prel31(0, addr + 4, to(r)),
                );
                (w0, w1)
            }
            ExEnt::CantAtom(a) => cant(out.atom_addr(a)),
            ExEnt::CantSec(k) => cant(out.secs[k].addr),
            ExEnt::End => cant(code_end),
        };
        en.put32(buf, at, w0);
        en.put32(buf, at + 4, w1);
    }
}
