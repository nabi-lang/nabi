//! REPL reuse (`nabic_codegen::repl`) on LIR: a function's code is its
//! instructions with symbols by name, so equal code is equal machine code.

use crate::lir::{Callee, Func, Inst, SymId};
use crate::obj::Obj;
use nabic_codegen::repl::Kind;
use std::fmt::Write;

/// A top-level definition of a cell (`nabic_codegen::repl::Def`).
pub struct CellDef {
    pub name: String,
    /// A candidate: its `Options::fresh_fns` entry.
    pub key: Option<String>,
    pub kind: Kind,
    pub hash: u64,
    pub refs: Vec<String>,
}

/// The definitions of a lowered cell: `funcs` (global, weak), the first
/// `keys.len()` of them the module's functions, then `obj`'s data.
pub fn defs(obj: &Obj, funcs: &[(Func, bool, bool)], keys: &[String]) -> Vec<CellDef> {
    let mut out = Vec::new();
    for (i, (f, global, _)) in funcs.iter().enumerate() {
        let name = obj.syms[f.sym as usize].name.clone();
        let kind = match (keys.get(i), global) {
            (_, false) => Kind::Other,
            (Some(_), true) => Kind::Candidate,
            (None, true) => Kind::Fresh,
        };
        let (hash, refs) = if kind == Kind::Candidate {
            let mut refs = Vec::new();
            let text = code(f, obj, &mut refs);
            refs.retain(|r| {
                *r != name && !r.starts_with("nabi.str.") && !r.starts_with("nabi.scan")
            });
            (nabic_codegen::repl::hash(&text), refs)
        } else {
            (0, Vec::new())
        };
        out.push(CellDef {
            name,
            key: keys.get(i).filter(|_| kind == Kind::Candidate).cloned(),
            kind,
            hash,
            refs,
        });
    }
    // Exported data: statics, binding slots.
    for s in &obj.syms {
        if s.def.is_some() && s.global && !s.weak {
            out.push(CellDef {
                name: s.name.clone(),
                key: None,
                kind: Kind::Fresh,
                hash: 0,
                refs: Vec::new(),
            });
        }
    }
    out
}

/// `f` as text, symbols by name (into `refs`; a function value's thunk
/// `<f>.v` stands for `f`), line info left out.
fn code(f: &Func, obj: &Obj, refs: &mut Vec<String>) -> String {
    let mut name = |s: SymId| {
        let n = obj.syms[s as usize].name.as_str();
        refs.push(n.strip_suffix(".v").unwrap_or(n).to_owned());
        n
    };
    let mut out = String::new();
    let _ = writeln!(out, "{} {:?} {}", name(f.sym), f.slots, f.labels);
    for i in &f.insts {
        let _ = match i {
            Inst::Line(..) => continue,
            Inst::Sym { dst, sym, got } => writeln!(out, "Sym {dst:?} @{} {got}", name(*sym)),
            Inst::Tls { dst, sym } => writeln!(out, "Tls {dst:?} @{}", name(*sym)),
            Inst::Call {
                callee: Callee::Sym(s),
                args,
                rets,
                sret,
                variadic,
                ..
            } => writeln!(
                out,
                "Call @{} {args:?} {rets:?} {sret:?} {variadic}",
                name(*s)
            ),
            _ => writeln!(out, "{i:?}"),
        };
    }
    out
}
