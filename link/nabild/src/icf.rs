//! Identical code folding, safe mode.
//!
//! Of atoms with the same bytes and relocations to the same places, one
//! copy stays and the others take its address. Candidates are code whose
//! address nothing takes (only calls and jumps reach it), so no
//! comparison of function addresses can tell the copies apart, and
//! nabic's function value thunks, whose addresses only function values
//! hold, which have no comparison (`fn_value_thunk`). Atoms
//! referring to each other compare by the copy they belong to, refined
//! until nothing changes, as lld does, so identical recursive and
//! mutually recursive functions fold too.

use crate::symtab::{Atom, AtomId, Reloc, SymbolTable, Target};
use nabic_core::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};

/// Where a relocation points, for comparing two atoms.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Dest {
    /// An offset in an atom; `tag`: what of the symbol the relocation
    /// depends on beyond its address (an ELF type, a local entry point).
    In { atom: AtomId, off: u64, tag: u32 },
    /// Anything else (an import, an absolute or linker-made symbol):
    /// equal only to itself.
    Other(u64),
}

/// The format's view of the atoms.
pub trait View {
    /// The bytes of candidate `a`.
    fn bytes(&self, a: AtomId) -> &[u8];
    /// What else must match: section kind, unwind encoding, the symbols
    /// defined in it.
    fn extra(&self, a: AtomId) -> u64;
    /// Where `t` points; atoms that are copies of each other (merged
    /// strings) given as one.
    fn dest(&self, t: Target) -> Dest;
    /// The addend of relocation `r` of `a` as the link sees it.
    fn addend(&self, _a: AtomId, r: &Reloc) -> i64 {
        r.addend
    }
}

/// Whether `name` (a symbol, leading `_`s aside) is a function value
/// thunk nabic makes, `<symbol>.v`.
///
/// Its address is only ever a function value's code, and no program can
/// compare those, so its copies may fold although that address is taken.
#[must_use]
pub fn fn_value_thunk(name: &str) -> bool {
    name.strip_suffix(".v").is_some_and(|s| !s.is_empty())
}

/// Atoms whose address is taken: the targets of every relocation of a
/// `scan` atom that `call` does not call or jump through.
pub fn address_taken(
    atoms: &[Atom],
    syms: &SymbolTable,
    scan: impl Fn(AtomId) -> bool,
    call: impl Fn(AtomId, &Reloc) -> bool,
) -> Vec<bool> {
    let mut taken = vec![false; atoms.len()];
    for (i, a) in atoms.iter().enumerate() {
        if !a.live || !scan(i as AtomId) {
            continue;
        }
        for r in &a.relocs {
            let direct = call(i as AtomId, r);
            for (t, sub) in std::iter::once((r.target, false)).chain(r.sub.map(|s| (s, true))) {
                if direct && !sub {
                    continue;
                }
                if let Some((t, _)) = syms.resolve(t) {
                    taken[t as usize] = true;
                }
            }
        }
    }
    taken
}

/// Whether the rel32 field at `off` of x86 code is the operand of a
/// `call`, `jmp` or `jcc` (a RIP-relative operand follows a `ModRM` byte,
/// never one of these opcodes).
#[must_use]
pub fn x86_branch(code: &[u8], off: u32) -> bool {
    let off = off as usize;
    match off.checked_sub(1).and_then(|k| code.get(k)) {
        Some(0xe8 | 0xe9) => true,
        Some(b) if b & 0xf0 == 0x80 => off >= 2 && code[off - 2] == 0x0f,
        _ => false,
    }
}

/// The constant part of relocation `r` of `a`: all but its targets.
fn reloc_key(v: &dyn View, a: AtomId, r: &Reloc) -> (u32, u16, u8, bool, i64, bool) {
    (
        r.off,
        r.kind,
        r.len,
        r.pcrel,
        v.addend(a, r),
        r.sub.is_some(),
    )
}

/// Fold `cands`: each copy and the atom it becomes, the first of its
/// kind in `cands` order (which keeps the output stable).
pub fn fold(atoms: &[Atom], cands: &[AtomId], v: &dyn View) -> Vec<(AtomId, AtomId)> {
    let n = cands.len();
    if n < 2 {
        return Vec::new();
    }
    let mut index = vec![u32::MAX; atoms.len()];
    for (k, &a) in cands.iter().enumerate() {
        index[a as usize] = k as u32;
    }
    // Each candidate's relocation targets, the subtrahends after.
    let dests: Vec<Vec<Dest>> = cands
        .iter()
        .map(|&a| {
            let rs = &atoms[a as usize].relocs;
            rs.iter()
                .map(|r| v.dest(r.target))
                .chain(rs.iter().filter_map(|r| r.sub.map(|s| v.dest(s))))
                .collect()
        })
        .collect();
    // A target among the candidates matters by its class, found later.
    let cand = |d: &Dest| match *d {
        Dest::In { atom, .. } => index[atom as usize] != u32::MAX,
        Dest::Other(_) => false,
    };
    let fixed = |d: &Dest| match *d {
        Dest::In { off, tag, .. } if cand(d) => Dest::In {
            atom: AtomId::MAX,
            off,
            tag,
        },
        d => d,
    };
    let same = |x: usize, y: usize| {
        let (a, b) = (&atoms[cands[x] as usize], &atoms[cands[y] as usize]);
        a.size == b.size
            && a.relocs.len() == b.relocs.len()
            && v.extra(cands[x]) == v.extra(cands[y])
            && v.bytes(cands[x]) == v.bytes(cands[y])
            && a.relocs
                .iter()
                .zip(&b.relocs)
                .all(|(p, q)| reloc_key(v, cands[x], p) == reloc_key(v, cands[y], q))
            && dests[x].iter().map(fixed).eq(dests[y].iter().map(fixed))
    };
    let hashes: Vec<u64> = (0..n)
        .map(|k| {
            let a = &atoms[cands[k] as usize];
            let mut h = DefaultHasher::new();
            a.size.hash(&mut h);
            v.extra(cands[k]).hash(&mut h);
            v.bytes(cands[k]).hash(&mut h);
            for r in &a.relocs {
                reloc_key(v, cands[k], r).hash(&mut h);
            }
            for d in &dests[k] {
                fixed(d).hash(&mut h);
            }
            h.finish()
        })
        .collect();
    // Classes by constant content: equal hashes, then equal contents.
    let mut by_hash: Vec<usize> = (0..n).collect();
    by_hash.sort_by_key(|&k| (hashes[k], k));
    let mut class = vec![0u32; n];
    let mut classes = 0u32;
    let mut i = 0;
    while i < n {
        let mut j = i;
        while j < n && hashes[by_hash[j]] == hashes[by_hash[i]] {
            j += 1;
        }
        let mut reps: Vec<(usize, u32)> = Vec::new();
        for &k in &by_hash[i..j] {
            if let Some(&(_, c)) = reps.iter().find(|&&(r, _)| same(r, k)) {
                class[k] = c;
            } else {
                reps.push((k, classes));
                class[k] = classes;
                classes += 1;
            }
        }
        i = j;
    }
    // Split classes whose members point at different classes until
    // none does.
    loop {
        let mut next: HashMap<(u32, Vec<u32>), u32> = HashMap::default();
        let mut new = vec![0u32; n];
        for k in 0..n {
            let key: Vec<u32> = dests[k]
                .iter()
                .filter_map(|d| match *d {
                    Dest::In { atom, .. } if cand(d) => Some(class[index[atom as usize] as usize]),
                    _ => None,
                })
                .collect();
            let fresh = next.len() as u32;
            new[k] = *next.entry((class[k], key)).or_insert(fresh);
        }
        let done = next.len() as u32 == classes;
        classes = next.len() as u32;
        class = new;
        if done {
            break;
        }
    }
    let mut leader = vec![u32::MAX; classes as usize];
    let mut folds = Vec::new();
    for k in 0..n {
        let c = class[k] as usize;
        if leader[c] == u32::MAX {
            leader[c] = cands[k];
        } else {
            folds.push((cands[k], leader[c]));
        }
    }
    folds
}

/// Drop each copy of `folds`, its atom aligned for every copy.
pub fn apply(atoms: &mut [Atom], folds: &[(AtomId, AtomId)]) {
    for &(dup, keep) in folds {
        let align = atoms[dup as usize].align;
        let k = &mut atoms[keep as usize];
        k.align = k.align.max(align);
        atoms[dup as usize].live = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::symtab::Def;

    #[test]
    fn function_value_thunks_by_name() {
        assert!(fn_value_thunk("_Yf2cfd549__twice_0_0fnval_.v"));
        assert!(fn_value_thunk("f.v"));
        for n in [".v", "v", "f.vv", "f.v.1", "_Yf2cfd549__twice_"] {
            assert!(!fn_value_thunk(n), "{n}");
        }
    }

    struct T {
        data: Vec<Vec<u8>>,
        syms: SymbolTable,
    }

    impl View for T {
        fn bytes(&self, a: AtomId) -> &[u8] {
            &self.data[a as usize]
        }
        fn extra(&self, _: AtomId) -> u64 {
            0
        }
        fn dest(&self, t: Target) -> Dest {
            match self.syms.resolve(t) {
                Some((atom, off)) => Dest::In { atom, off, tag: 0 },
                None => Dest::Other(0),
            }
        }
    }

    fn atom(i: u64, relocs: &[AtomId]) -> Atom {
        let mut a = Atom::new(0, 0, i, 4, 2);
        a.live = true;
        a.relocs = relocs
            .iter()
            .map(|&t| Reloc {
                off: 0,
                kind: 4,
                len: 2,
                pcrel: true,
                target: Target::Atom(t),
                addend: 0,
                sub: None,
            })
            .collect();
        a
    }

    #[test]
    fn folds_to_a_fixpoint() {
        // 0 and 1 call themselves, 2 and 3 each other, 4 calls 0 and 5
        // calls 1: each calls a copy of itself, so all fold; 6 differs in
        // its bytes.
        let atoms = vec![
            atom(0, &[0]),
            atom(1, &[1]),
            atom(2, &[3]),
            atom(3, &[2]),
            atom(4, &[0]),
            atom(5, &[1]),
            atom(6, &[0]),
        ];
        let mut data = vec![vec![1, 2, 3, 4]; 7];
        data[6] = vec![9, 9, 9, 9];
        let t = T {
            data,
            syms: SymbolTable::default(),
        };
        let folds = fold(&atoms, &[0, 1, 2, 3, 4, 5, 6], &t);
        assert_eq!(folds, [(1, 0), (2, 0), (3, 0), (4, 0), (5, 0)]);
    }

    #[test]
    fn different_callees_keep_both() {
        // 0 and 1 call 2 and 3, whose bytes differ.
        let atoms = vec![atom(0, &[2]), atom(1, &[3]), atom(2, &[]), atom(3, &[])];
        let mut data = vec![vec![7; 4]; 4];
        data[3] = vec![8; 4];
        let t = T {
            data,
            syms: SymbolTable::default(),
        };
        assert!(fold(&atoms, &[0, 1, 2, 3], &t).is_empty());
    }

    #[test]
    fn taken_addresses() {
        let mut syms = SymbolTable::default();
        let f = syms.intern("f");
        syms.define(f, Def::Defined { atom: 1, off: 0 }, 0, false, false)
            .unwrap();
        let mut atoms = vec![atom(0, &[2]), atom(1, &[]), atom(2, &[])];
        atoms[0].relocs[0].kind = 9;
        let r = Reloc {
            target: Target::Sym(f),
            kind: 1,
            ..atoms[0].relocs[0]
        };
        atoms[0].relocs.push(r);
        let taken = address_taken(&atoms, &syms, |_| true, |_, r| r.kind == 9);
        assert_eq!(taken, [false, true, false]);
        assert!(x86_branch(&[0xe8, 0, 0, 0, 0], 1));
        assert!(x86_branch(&[0x0f, 0x84, 0, 0, 0, 0], 2));
        assert!(!x86_branch(&[0x48, 0x8d, 0x05, 0, 0, 0, 0], 3));
    }
}
