//! Dead stripping: every atom reachable from the roots through
//! relocations and live-support edges stays.

use crate::symtab::{Atom, SymbolTable, Target};

/// Mark live atoms: `roots`, atoms flagged `root`, and what they reach.
/// With `all`, nothing is stripped.
pub fn mark(atoms: &mut [Atom], syms: &SymbolTable, roots: &[Target], all: bool) {
    let mut work: Vec<u32> = Vec::new();
    let push = |t: Target, work: &mut Vec<u32>| {
        if let Some((a, _)) = syms.resolve(t) {
            work.push(a);
        }
    };
    for (i, a) in atoms.iter().enumerate() {
        if all || a.root {
            work.push(i as u32);
        }
    }
    for &t in roots {
        push(t, &mut work);
    }
    while let Some(i) = work.pop() {
        let a = &mut atoms[i as usize];
        if a.live {
            continue;
        }
        a.live = true;
        let a = &atoms[i as usize];
        for r in &a.relocs {
            push(r.target, &mut work);
            if let Some(s) = r.sub {
                push(s, &mut work);
            }
        }
        for &k in &a.keep {
            push(k, &mut work);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::symtab::{Def, Reloc};

    const fn reloc(target: Target) -> Reloc {
        Reloc {
            off: 0,
            kind: 0,
            len: 3,
            pcrel: false,
            target,
            addend: 0,
            sub: None,
        }
    }

    #[test]
    fn keeps_what_the_roots_reach() {
        let mut syms = SymbolTable::default();
        let f = syms.intern("f");
        syms.define(f, Def::Defined { atom: 2, off: 0 }, 0, false, false)
            .unwrap();
        let mut atoms: Vec<Atom> = (0..5).map(|i| Atom::new(0, 0, i * 4, 4, 2)).collect();
        atoms[0].relocs.push(reloc(Target::Sym(f)));
        atoms[2].keep.push(Target::Atom(3));
        atoms[4].root = true;
        mark(&mut atoms, &syms, &[Target::Atom(0)], false);
        let live: Vec<bool> = atoms.iter().map(|a| a.live).collect();
        assert_eq!(live, [true, false, true, true, true]);
    }
}
