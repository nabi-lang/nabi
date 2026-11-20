//! Loop-invariant code motion. A pure, non-trapping computation in a loop
//! (`opt::pure`: no load, no call, no checked arithmetic that could
//! panic) whose operands the loop never assigns, into a local assigned
//! only there, moves to a preheader in front of the loop: computed once
//! instead of every iteration.
//!
//! So does a load of a field of a GC object (a list's length, its
//! buffer) the loop cannot change: nothing in it may write the field
//! (`alias`), and the object is one the code in front of the loop or
//! the loop header already reads, so the early read is of a live
//! object. Element loads stay: a bounds check may guard them.

use crate::opt::alias::{self, Eff};
use crate::{MirFunction, Operand, RValue, Statement, Terminator};
use nabic_core::{HashMap, HashSet};
use nabic_ir_common::{FunctionId, LocalId};

/// `ro`: the functions whose calls write nothing (`alias::readonly`).
pub fn hoist(f: &mut MirFunction, pw: u32, ro: &HashSet<FunctionId>) -> bool {
    let idx = crate::opt::index(f);
    if !crate::opt::cyclic(f, &idx, &crate::opt::rpo(f, &idx)) {
        return false;
    }
    let mut changed = false;
    // Each round may make room for an outer loop's move.
    for _ in 0..2 {
        if !round(f, pw, ro, false) {
            break;
        }
        changed = true;
    }
    // Then the field loads of innermost loops (a list's length and
    // buffer), which counted and vector loops need out, however many
    // loops came first.
    for _ in 0..64 {
        if !round(f, pw, ro, true) {
            break;
        }
        changed = true;
    }
    changed
}

/// Moves what one loop may out of it (`inner`: only field loads, of an
/// innermost loop); whether it did.
fn round(f: &mut MirFunction, pw: u32, ro: &HashSet<FunctionId>, inner: bool) -> bool {
    let idx = crate::opt::index(f);
    let preds = crate::opt::preds(f, &idx);
    let dom = crate::opt::Dom::new(f, &idx, &preds);
    // Back edges `b -> h` with `h` dominating `b`, by header.
    let mut loops: HashMap<usize, Vec<usize>> = HashMap::default();
    for (b, bb) in f.blocks.iter().enumerate() {
        for s in crate::opt::succs(&bb.terminator) {
            let h = idx[&s.0];
            if dom.dominates(h, b) {
                loops.entry(h).or_default().push(b);
            }
        }
    }
    let mut headers: Vec<usize> = loops.keys().copied().collect();
    headers.sort_unstable();
    for &h in &headers {
        if h == 0 || !f.blocks[h].params.is_empty() {
            continue;
        }
        let body = body(&preds, h, &loops[&h]);
        if inner && headers.iter().any(|x| *x != h && body.contains(x)) {
            continue;
        }
        let safe = live_bases(f, &dom, h);
        if let Some(moved) = invariants(f, &body, &safe, pw, ro, inner) {
            let ids: HashSet<u32> = body.iter().map(|&b| f.blocks[b].id.0).collect();
            place(f, h, &ids, moved);
            return true;
        }
    }
    false
}

/// The blocks of the natural loop of header `h` with latches `ls`.
fn body(preds: &[Vec<usize>], h: usize, ls: &[usize]) -> HashSet<usize> {
    let mut body: HashSet<usize> = HashSet::from_iter([h]);
    let mut work: Vec<usize> = ls.to_vec();
    while let Some(b) = work.pop() {
        if body.insert(b) {
            work.extend(preds[b].iter().copied());
        }
    }
    body
}

/// The base local of a GC object access.
const fn base(s: &Statement) -> Option<LocalId> {
    match s {
        Statement::Assign {
            rvalue:
                RValue::HeapField {
                    ptr: Operand::Copy(l),
                    ..
                }
                | RValue::HeapLoad {
                    ptr: Operand::Copy(l),
                    ..
                }
                | RValue::BufLoad {
                    buf: Operand::Copy(l),
                    ..
                },
            ..
        }
        | Statement::BufStore {
            buf: Operand::Copy(l),
            ..
        }
        | Statement::HeapStore {
            ptr: Operand::Copy(l),
            ..
        } => Some(*l),
        _ => None,
    }
}

/// Locals holding a live GC object whenever loop header `h` is entered:
/// an access through one runs in `h` (before the loop could leave), or
/// in a block dominating `h` with the local assigned once.
fn live_bases(f: &MirFunction, dom: &crate::opt::Dom, h: usize) -> HashSet<LocalId> {
    let mut defs: HashMap<LocalId, usize> = HashMap::default();
    for p in &f.params {
        *defs.entry(*p).or_default() += 1;
    }
    for b in &f.blocks {
        for p in &b.params {
            *defs.entry(*p).or_default() += 2;
        }
        for s in &b.stmts {
            if let Some(l) = crate::liveness::stmt_uses(s).1 {
                *defs.entry(l).or_default() += 1;
            }
            if let Statement::ArrayWrite { array, .. } | Statement::StorageDead(array) = s {
                *defs.entry(*array).or_default() += 2;
            }
        }
    }
    let mut out = alias::fresh(f);
    for (bi, b) in f.blocks.iter().enumerate() {
        let here = bi == h;
        if !(here || dom.reachable(bi) && dom.dominates(bi, h)) {
            continue;
        }
        for s in &b.stmts {
            if let Some(l) = base(s) {
                if here || defs.get(&l) == Some(&1) {
                    out.insert(l);
                }
            }
        }
    }
    out
}

/// Takes the invariant statements out of the loop `body`, in order;
/// loads only through `safe` bases, and only loads with `loads`.
fn invariants(
    f: &mut MirFunction,
    body: &HashSet<usize>,
    safe: &HashSet<LocalId>,
    pw: u32,
    ro: &HashSet<FunctionId>,
    loads: bool,
) -> Option<Vec<Statement>> {
    let pinned = crate::opt::pinned(f);
    let fresh = alias::fresh(f);
    // What the loop may write; `None`: anything.
    let mut effs: Option<Vec<Eff>> = Some(Vec::new());
    for &bi in body {
        for s in &f.blocks[bi].stmts {
            match (alias::effect(s, ro), &mut effs) {
                (Eff::None, _) | (_, None) => {}
                (Eff::Any, e) => *e = None,
                (e, Some(v)) => v.push(e),
            }
        }
    }
    let invariant_load = |rv: &RValue| {
        let Some(a) = alias::load(rv) else {
            return false;
        };
        matches!(rv, RValue::HeapField { .. } | RValue::HeapLoad { .. })
            && matches!(&a.base, Operand::Copy(x) if safe.contains(x))
            && effs
                .as_ref()
                .is_some_and(|v| !v.iter().any(|e| alias::clobbers(e, &a, &fresh)))
    };
    // Locals assigned in the loop, and how often in the function.
    let mut set_in: HashSet<LocalId> = HashSet::default();
    let mut defs: HashMap<LocalId, usize> = HashMap::default();
    let mut ended: HashSet<LocalId> = HashSet::default();
    for (bi, b) in f.blocks.iter().enumerate() {
        let inside = body.contains(&bi);
        for p in &b.params {
            *defs.entry(*p).or_default() += 2;
            if inside {
                set_in.insert(*p);
            }
        }
        for s in &b.stmts {
            let l = match s {
                Statement::Assign { target, .. } => *target,
                Statement::ArrayWrite { array, .. } => *array,
                Statement::StorageDead(l) => {
                    ended.insert(*l);
                    *l
                }
                _ => continue,
            };
            *defs.entry(l).or_default() += 1;
            if inside {
                set_in.insert(l);
            }
        }
    }
    for p in &f.params {
        *defs.entry(*p).or_default() += 2;
    }
    let mut blocks: Vec<usize> = body.iter().copied().collect();
    blocks.sort_unstable();
    let mut moved = Vec::new();
    loop {
        let mut more = false;
        for &bi in &blocks {
            let mut i = 0;
            while i < f.blocks[bi].stmts.len() {
                let s = &f.blocks[bi].stmts[i];
                let Statement::Assign { target, rvalue } = s else {
                    i += 1;
                    continue;
                };
                let t = *target;
                let mut ops = Vec::new();
                crate::coro::rvalue_operands(rvalue, &mut ops);
                let fits = !matches!(rvalue, RValue::Use(_))
                    && ((!loads && crate::opt::pure(rvalue, pw)) || invariant_load(rvalue))
                    && defs.get(&t) == Some(&1)
                    && !pinned.contains(&t)
                    && !ended.contains(&t)
                    && ops.iter().all(|o| match o {
                        Operand::Const(_) => true,
                        Operand::Copy(l) => !set_in.contains(l) && !pinned.contains(l),
                    });
                if fits {
                    moved.push(f.blocks[bi].stmts.remove(i));
                    set_in.remove(&t);
                    more = true;
                } else {
                    i += 1;
                }
            }
        }
        if !more {
            break;
        }
    }
    (!moved.is_empty()).then_some(moved)
}

/// Puts `moved` in front of loop header `h` (body ids `ids`): at the end
/// of its only outside predecessor when that one only jumps to it, else
/// in a new block the outside edges go through.
fn place(f: &mut MirFunction, h: usize, ids: &HashSet<u32>, moved: Vec<Statement>) {
    let hid = f.blocks[h].id;
    let outside: Vec<usize> = f
        .blocks
        .iter()
        .enumerate()
        .filter(|(_, b)| !ids.contains(&b.id.0) && crate::opt::succs(&b.terminator).contains(&hid))
        .map(|(i, _)| i)
        .collect();
    if let [p] = outside[..] {
        if matches!(&f.blocks[p].terminator, Terminator::Goto { target, args } if *target == hid && args.is_empty())
        {
            f.blocks[p].stmts.extend(moved);
            return;
        }
    }
    let pre = crate::opt::fresh_block(f);
    for &p in &outside {
        for s in crate::opt::succs_mut(&mut f.blocks[p].terminator) {
            if *s == hid {
                *s = pre;
            }
        }
    }
    let mut b = crate::opt::empty_block(
        pre,
        Terminator::Goto {
            target: hid,
            args: Vec::new(),
        },
    );
    b.stmts = moved;
    f.blocks.insert(h, b);
}
