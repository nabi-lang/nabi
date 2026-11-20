//! Value simplification: copy and constant propagation, constant
//! folding, algebraic identities, common subexpressions, comparisons
//! decided by what the branches taken so far proved, and dead code.
//!
//! Two scopes:
//!
//! - whole function, for locals assigned once (`once`): `x = c` makes
//!   every read of `x` read `c`; `x = y` (`y` assigned once before) every
//!   read `x`'s assignment dominates read `y`;
//! - extended blocks (a block with one predecessor continues its
//!   predecessor's state), for every local: the values, expressions and
//!   loads known at each statement, forgotten when an operand is
//!   assigned again, its scope ends or memory may change.
//!
//! A block with several predecessors starts from its immediate
//! dominator's state, less what the blocks between them assign or may
//! write (`alias`). Stores forward their value to later loads of the
//! same place and make an earlier store there with no read between
//! dead.
//!
//! Relations (`a < b`) come from a forward dataflow over the branches
//! (`facts`): they decide comparisons (a bounds check against a length
//! the loop test already checked) and prove `+ 1` / `- 1` of a counter
//! within range, which then needs no overflow check.

mod dead;
mod divrem;
mod facts;
mod fold;
mod number;

pub(crate) use crate::opt::simplify::dead::*;
pub use crate::opt::simplify::divrem::pair;
pub(crate) use crate::opt::simplify::facts::*;
pub(crate) use crate::opt::simplify::fold::*;
pub(crate) use crate::opt::simplify::number::*;

use crate::opt::alias::{self, Acc, Eff};
use crate::opt::{Dom, int_const, int_range, int_val, pure, removable};
use crate::{MirFunction, MirTy, Operand, RValue, Statement, Terminator};
use PrimOp::{BitAnd, BitOr, BitXor, Eq, Ge, Gt, Le, LogicalAnd, LogicalOr, Lt, Ne};
use nabic_core::{HashMap, HashSet};
use nabic_ir_common::{ConstVal, FunctionId, LocalId, PrimOp, PrimTy};
use std::cell::Cell;
use std::rc::Rc;

/// `ro`: the functions whose calls write nothing (`alias::readonly`).
pub fn run(f: &mut MirFunction, pw: u32, ro: &HashSet<FunctionId>) -> bool {
    // Neither pass changes the blocks or what is addressed.
    let idx = crate::opt::index(f);
    let preds = crate::opt::preds(f, &idx);
    let dom = Dom::new(f, &idx, &preds);
    let pinned = crate::opt::pinned(f);
    let sh = Shape {
        idx: &idx,
        preds: &preds,
        dom: &dom,
        pinned: &pinned,
    };
    let mut changed = once(f, &sh);
    changed |= blocks(f, &sh, pw, ro);
    changed |= dead(f, pw);
    changed
}

/// What the passes of a round share.
pub(crate) struct Shape<'a> {
    idx: &'a HashMap<u32, usize>,
    preds: &'a [Vec<usize>],
    dom: &'a Dom,
    pinned: &'a HashSet<LocalId>,
}

/// The single assignment of each local assigned once by an `Assign` and
/// nowhere else (no parameter, block parameter, partial write or scope
/// end), not pinned: `(block index, statement index)`, by local.
fn single_defs(f: &MirFunction, pinned: &HashSet<LocalId>) -> Vec<Option<(usize, usize)>> {
    let n = f.locals.len();
    let mut bad = vec![false; n];
    for &l in pinned.iter().chain(&f.params) {
        bad[l.0 as usize] = true;
    }
    let mut defs: Vec<Option<(usize, usize)>> = vec![None; n];
    for (bi, b) in f.blocks.iter().enumerate() {
        for p in &b.params {
            bad[p.0 as usize] = true;
        }
        for (si, s) in b.stmts.iter().enumerate() {
            match s {
                Statement::Assign { target, .. } => {
                    let t = target.0 as usize;
                    if defs[t].replace((bi, si)).is_some() {
                        bad[t] = true;
                    }
                }
                Statement::StorageDead(l) => bad[l.0 as usize] = true,
                _ => {}
            }
        }
    }
    for (d, b) in defs.iter_mut().zip(&bad) {
        if *b {
            *d = None;
        }
    }
    defs
}

/// Parameters never assigned in the body: their value holds everywhere.
fn fixed_params(f: &MirFunction, pinned: &HashSet<LocalId>) -> Vec<bool> {
    let mut out = vec![false; f.locals.len()];
    for p in &f.params {
        out[p.0 as usize] = true;
    }
    for l in pinned {
        out[l.0 as usize] = false;
    }
    for b in &f.blocks {
        for p in &b.params {
            out[p.0 as usize] = false;
        }
        for s in &b.stmts {
            match s {
                Statement::Assign { target: l, .. } | Statement::StorageDead(l) => {
                    out[l.0 as usize] = false;
                }
                _ => {}
            }
        }
    }
    out
}

/// What reads of a local read instead, and from where that holds.
type Repl = (Operand, Option<(usize, usize)>);

/// What an edge proves: the facts of locals holding its constant side,
/// and its relation.
type EdgeFacts = Option<(Vec<Fact>, Fact)>;

/// Whole-function propagation of locals assigned once.
fn once(f: &mut MirFunction, sh: &Shape<'_>) -> bool {
    let defs = single_defs(f, sh.pinned);
    if defs.iter().all(Option::is_none) {
        return false;
    }
    let fixed = fixed_params(f, sh.pinned);
    let dom = sh.dom;
    // `x` -> what its reads may read instead, and where that holds from.
    let mut repl: Vec<Option<Repl>> = vec![None; defs.len()];
    let mut any = false;
    for (x, &(bi, si)) in defs
        .iter()
        .enumerate()
        .filter_map(|(x, d)| d.as_ref().map(|d| (LocalId(x as u32), d)))
    {
        let Statement::Assign {
            rvalue: RValue::Use(o),
            ..
        } = &f.blocks[bi].stmts[si]
        else {
            continue;
        };
        let xt = &f.locals[x.0 as usize].ty;
        if !crate::opt::has_ty(&f.locals, o, xt) {
            continue;
        }
        match o {
            // The only value it ever holds.
            Operand::Const(_) => {
                repl[x.0 as usize] = Some((o.clone(), None));
                any = true;
            }
            Operand::Copy(y) if *y != x => {
                let held = fixed[y.0 as usize]
                    || defs[y.0 as usize].is_some_and(|(yb, ys)| {
                        (yb == bi && ys < si) || (yb != bi && dom.dominates(yb, bi))
                    });
                if held {
                    repl[x.0 as usize] = Some((o.clone(), Some((bi, si))));
                    any = true;
                }
            }
            Operand::Copy(_) => {}
        }
    }
    if !any {
        return false;
    }
    // Chains: `x = y; z = x` reads `y` where both hold; resolve one step at
    // a time, the dominance checks keep each step valid.
    let mut changed = false;
    for bi in 0..f.blocks.len() {
        if !dom.reachable(bi) {
            continue;
        }
        let n = f.blocks[bi].stmts.len();
        let sub = |o: &mut Operand, at: usize, changed: &mut bool| {
            for _ in 0..8 {
                let Operand::Copy(x) = o else { return };
                let Some((v, from)) = &repl[x.0 as usize] else {
                    return;
                };
                let ok = from.is_none_or(|(db, ds)| {
                    if db == bi {
                        ds < at
                    } else {
                        dom.dominates(db, bi)
                    }
                });
                if !ok {
                    return;
                }
                *o = v.clone();
                *changed = true;
            }
        };
        for si in 0..n {
            let s = &mut f.blocks[bi].stmts[si];
            crate::opt::stmt_ops_mut(s, &mut |o| sub(o, si, &mut changed));
        }
        let t = &mut f.blocks[bi].terminator;
        crate::opt::term_ops_mut(t, &mut |o| sub(o, n, &mut changed));
    }
    changed
}
