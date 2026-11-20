//! REPL cells compile only what is new.
//!
//! A function whose code equals an earlier cell's definition of its
//! symbol, with all callees likewise, becomes a declaration of it, so
//! library code compiles once per session.
//!
//! Equal code is equal behaviour: symbols are stable across cells, and a
//! function referring to anything this cell redefines is redefined too.
//! The host renames redefined symbols so later cells get the newest
//! definition. The backend hashes its own output per definition.

use nabic_core::{HashMap, HashSet};
use std::hash::{Hash, Hasher};

/// A function this cell defines anew: symbol, code hash, and referenced
/// symbols (its `known` entry goes when one is redefined).
#[derive(Clone, Debug)]
pub struct ReplFn {
    pub name: String,
    pub hash: u64,
    pub refs: Vec<String>,
}

/// One REPL cell: code the host links into a process whose runtime is
/// up. Instead of a C `main`, `nabi_repl_cell()` registers its GC types and
/// roots and runs the user `main`.
#[derive(Clone, Debug, Default)]
pub struct ReplCell {
    /// The `ScanTable`s earlier cells registered (type id `USER_TYPE_BASE + i`):
    /// the same type keeps its id, new ones follow.
    pub gc_tables: Vec<Vec<u32>>,
    /// Functions the session has (symbol -> code hash, `reuse`):
    /// declared, not defined, when this cell's code is the same.
    pub known: HashMap<String, u64>,
    /// `extern` statics this cell defines (zeroed, exported, GC roots):
    /// the slots of the session's new bindings.
    pub define: Vec<String>,
    /// Statics (MIR names) an earlier cell defined: declared, not
    /// defined, so their state carries over.
    pub statics: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// An exported function with a body: may be an earlier cell's.
    Candidate,
    /// An exported definition always made anew (statics, slots,
    /// `nabi_repl_cell`, `nabi.register_types`).
    Fresh,
    /// Internal, or a declaration.
    Other,
}

/// One top-level definition of a cell: its symbol, kind, code hash (of a
/// candidate) and the symbols it refers to.
pub struct Def<'a> {
    pub name: &'a str,
    pub kind: Kind,
    pub hash: u64,
    pub refs: Vec<&'a str>,
}

/// The hash of a definition's code.
#[must_use]
pub fn hash(code: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    code.hash(&mut h);
    h.finish()
}

/// The candidates of `defs` that `known` already has (to declare only),
/// plus the exported symbols and functions the cell defines anew.
#[allow(clippy::implicit_hasher)]
pub fn reuse<'a>(
    defs: &[Def<'a>],
    known: &HashMap<String, u64>,
) -> (HashSet<&'a str>, Vec<String>, Vec<ReplFn>) {
    let fresh: HashSet<&str> = defs
        .iter()
        .filter(|o| o.kind == Kind::Fresh)
        .map(|o| o.name)
        .collect();
    let mut reused: HashSet<&str> = defs
        .iter()
        .filter(|o| o.kind == Kind::Candidate && known.get(o.name) == Some(&o.hash))
        .map(|o| o.name)
        .collect();
    let candidates: HashSet<&str> = defs
        .iter()
        .filter(|o| o.kind == Kind::Candidate)
        .map(|o| o.name)
        .collect();
    // A fixpoint over all definitions: cells have a few hundred.
    loop {
        let drop: Vec<&str> = defs
            .iter()
            .filter(|o| reused.contains(o.name))
            .filter(|o| {
                o.refs
                    .iter()
                    .any(|r| fresh.contains(r) || (candidates.contains(r) && !reused.contains(r)))
            })
            .map(|o| o.name)
            .collect();
        if drop.is_empty() {
            break;
        }
        for d in drop {
            reused.remove(d);
        }
    }
    let mut fns = Vec::new();
    for o in defs {
        if o.kind == Kind::Candidate && !reused.contains(o.name) {
            let mut refs: Vec<String> = o.refs.iter().map(|r| (*r).to_owned()).collect();
            refs.sort();
            refs.dedup();
            fns.push(ReplFn {
                name: o.name.to_owned(),
                hash: o.hash,
                refs,
            });
        }
    }
    let mut fresh: Vec<String> = fresh.into_iter().map(str::to_owned).collect();
    fresh.extend(fns.iter().map(|f| f.name.clone()));
    (reused, fresh, fns)
}
