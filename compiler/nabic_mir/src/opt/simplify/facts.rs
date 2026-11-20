//! Branch facts: relations the edges taken so far prove.

#[allow(clippy::wildcard_imports)]
use crate::opt::simplify::*;

/// A relation `a op b` (an integer, `char` or `bool` comparison).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Fact {
    pub(crate) op: PrimOp,
    /// The type both sides have.
    pub(crate) ty: PrimTy,
    pub(crate) a: Operand,
    pub(crate) b: Operand,
}

impl Fact {
    pub(crate) fn mentions(&self, l: LocalId) -> bool {
        [&self.a, &self.b]
            .iter()
            .any(|o| matches!(o, Operand::Copy(x) if *x == l))
    }
}

pub const fn swap(op: PrimOp) -> PrimOp {
    match op {
        PrimOp::Lt => PrimOp::Gt,
        PrimOp::Gt => PrimOp::Lt,
        PrimOp::Le => PrimOp::Ge,
        PrimOp::Ge => PrimOp::Le,
        o => o,
    }
}

pub const fn negate(op: PrimOp) -> PrimOp {
    match op {
        PrimOp::Lt => PrimOp::Ge,
        PrimOp::Ge => PrimOp::Lt,
        PrimOp::Gt => PrimOp::Le,
        PrimOp::Le => PrimOp::Gt,
        PrimOp::Eq => PrimOp::Ne,
        PrimOp::Ne => PrimOp::Eq,
        o => o,
    }
}

pub(crate) const fn is_cmp(op: PrimOp) -> bool {
    matches!(
        op,
        PrimOp::Lt | PrimOp::Le | PrimOp::Gt | PrimOp::Ge | PrimOp::Eq | PrimOp::Ne
    )
}

/// What a known `a known b` says of `a query b`.
pub(crate) const fn implies(known: PrimOp, query: PrimOp) -> Option<bool> {
    Some(match (known, query) {
        (Lt, Lt | Le | Ne)
        | (Gt, Gt | Ge | Ne)
        | (Le, Le)
        | (Ge, Ge)
        | (Ne, Ne)
        | (Eq, Eq | Le | Ge) => true,
        (Lt, Gt | Ge | Eq)
        | (Gt, Lt | Le | Eq)
        | (Le, Gt)
        | (Ge, Lt)
        | (Ne, Eq)
        | (Eq, Ne | Lt | Gt) => false,
        _ => return None,
    })
}

/// The truth of `q` under `facts`, if they decide it: one fact, or two
/// through a middle operand (`a < m`, `m <= b`).
pub(crate) fn decide(facts: &[Fact], q: &Fact) -> Option<bool> {
    let same: Vec<&Fact> = facts.iter().filter(|k| k.ty == q.ty).collect();
    // Each fact both ways round.
    let rels = || {
        same.iter()
            .flat_map(|k| [(&k.a, k.op, &k.b), (&k.b, swap(k.op), &k.a)])
    };
    if let Some(v) = rels().find_map(|(a, op, b)| (*a == q.a && *b == q.b).then_some(op)) {
        if let Some(v) = implies(v, q.op) {
            return Some(v);
        }
    }
    let up = |op| matches!(op, PrimOp::Lt | PrimOp::Le);
    let down = |op| matches!(op, PrimOp::Gt | PrimOp::Ge);
    for (_, r1, m) in rels().filter(|(a, ..)| **a == q.a) {
        for (_, r2, _) in rels().filter(|(x, _, b)| *x == m && **b == q.b) {
            let strict =
                r1 == PrimOp::Lt || r1 == PrimOp::Gt || r2 == PrimOp::Lt || r2 == PrimOp::Gt;
            let r = if up(r1) && up(r2) {
                if strict { PrimOp::Lt } else { PrimOp::Le }
            } else if down(r1) && down(r2) {
                if strict { PrimOp::Gt } else { PrimOp::Ge }
            } else {
                continue;
            };
            if let Some(v) = implies(r, q.op) {
                return Some(v);
            }
        }
    }
    None
}

/// Whether a comparison at `ty` may become a fact (no float: a NaN
/// breaks negation).
pub(crate) fn fact_ty(ty: &MirTy) -> bool {
    matches!(ty, MirTy::Prim(p) if p.is_int() || *p == PrimTy::Bool)
}

/// The fact the edge from block `b` to successor number `k` proves:
/// `if c` where `c = a op b` is computed in `b` with nothing assigned
/// after it.
pub(crate) fn edge_fact(f: &MirFunction, b: usize, k: usize) -> Option<Fact> {
    let bb = &f.blocks[b];
    let Terminator::If {
        cond: Operand::Copy(c),
        then_bb,
        else_bb,
        ..
    } = &bb.terminator
    else {
        return None;
    };
    if then_bb == else_bb {
        return None;
    }
    let (i, op, a, bo, ty) = bb
        .stmts
        .iter()
        .enumerate()
        .rev()
        .find_map(|(i, s)| match s {
            Statement::Assign {
                target,
                rvalue:
                    RValue::BinOp {
                        op,
                        op_ty,
                        lhs,
                        rhs,
                    },
            } if target == c && is_cmp(*op) && fact_ty(op_ty) => Some((i, *op, lhs, rhs, op_ty)),
            _ => None,
        })?;
    let MirTy::Prim(p) = ty else { return None };
    if [a, bo]
        .iter()
        .any(|o| !crate::opt::has_ty(&f.locals, o, ty))
    {
        return None;
    }
    let mut ls = vec![*c];
    for o in [a, bo] {
        if let Operand::Copy(l) = o {
            ls.push(*l);
        }
    }
    let later = bb.stmts[i..].iter().skip(1).any(|s| match s {
        Statement::Assign { target, .. } => ls.contains(target),
        Statement::StorageDead(l) | Statement::ArrayWrite { array: l, .. } => ls.contains(l),
        _ => false,
    });
    if later {
        return None;
    }
    let op = if k == 0 { op } else { negate(op) };
    Some(Fact {
        op,
        ty: *p,
        a: a.clone(),
        b: bo.clone(),
    })
}

/// Facts holding on entry to each block: a forward must-dataflow over
/// the edges, facts dropped where an operand is assigned.
pub(crate) fn entry_facts(
    f: &MirFunction,
    idx: &HashMap<u32, usize>,
    pinned: &HashSet<LocalId>,
    pw: u32,
) -> Vec<Vec<Fact>> {
    let n = f.blocks.len();
    let usable = |x: &Fact| {
        ![&x.a, &x.b]
            .iter()
            .any(|o| matches!(o, Operand::Copy(l) if pinned.contains(l)))
    };
    // Facts the steps of each block leave true at its end.
    let made: Vec<Vec<Fact>> = f
        .blocks
        .iter()
        .map(|b| {
            let mut v: Vec<Fact> = Vec::new();
            for s in &b.stmts {
                let l = match s {
                    Statement::Assign { target, .. } => *target,
                    Statement::StorageDead(l) | Statement::ArrayWrite { array: l, .. } => *l,
                    _ => continue,
                };
                if !v.is_empty() {
                    v.retain(|x| !x.mentions(l));
                }
                if let Statement::Assign { target, rvalue } = s {
                    if !pinned.contains(target) && !rv_reads(rvalue, *target) {
                        v.extend(step_of(&f.locals, pinned, pw, *target, rvalue));
                    }
                }
            }
            v
        })
        .collect();
    // What each edge proves: the branch's relation, and the same of the
    // locals holding its constant side.
    let edges: Vec<Vec<EdgeFacts>> = (0..n)
        .map(|b| {
            (0..crate::opt::succs(&f.blocks[b].terminator).len())
                .map(|k| {
                    let x = edge_fact(f, b, k).filter(usable)?;
                    let ys = named(f, b, &x).into_iter().filter(usable).collect();
                    Some((ys, x))
                })
                .collect()
        })
        .collect();
    if made.iter().all(Vec::is_empty) && edges.iter().flatten().all(Option::is_none) {
        return vec![Vec::new(); n];
    }
    let rpo = crate::opt::rpo(f, idx);
    // Without a cycle each block's predecessors come first: one pass.
    let loops = crate::opt::cyclic(f, idx, &rpo);
    // `None`: not reached yet (every fact).
    let mut entry: Vec<Option<Vec<Fact>>> = vec![None; n];
    entry[0] = Some(Vec::new());
    let killed: Vec<Vec<LocalId>> = f
        .blocks
        .iter()
        .map(|b| {
            let mut k: Vec<LocalId> = b.params.clone();
            for s in &b.stmts {
                match s {
                    Statement::Assign { target, .. } => k.push(*target),
                    Statement::StorageDead(l) | Statement::ArrayWrite { array: l, .. } => {
                        k.push(*l);
                    }
                    _ => {}
                }
            }
            k.sort_unstable();
            k.dedup();
            k
        })
        .collect();
    // To the fixed point: facts only shrink, and short of it some
    // would be unproven.
    // Blocks whose entry changed since their last visit.
    let mut dirty = vec![true; n];
    loop {
        let mut changed = false;
        for &b in &rpo {
            if !std::mem::take(&mut dirty[b]) {
                continue;
            }
            let Some(inn) = entry[b].clone() else {
                continue;
            };
            let mut out: Vec<Fact> = inn;
            for l in &killed[b] {
                if out.is_empty() {
                    break;
                }
                forget(&mut out, *l, pw);
            }
            for x in &made[b] {
                if !out.contains(x) {
                    out.push(x.clone());
                }
            }
            for (k, s) in crate::opt::succs(&f.blocks[b].terminator)
                .iter()
                .enumerate()
            {
                let Some(&j) = idx.get(&s.0) else { continue };
                let mut edge = out.clone();
                if let Some((ys, x)) = &edges[b][k] {
                    for y in ys {
                        if !edge.contains(y) {
                            edge.push(y.clone());
                        }
                    }
                    edge.push(x.clone());
                }
                let new = match &entry[j] {
                    None => edge,
                    Some(old) => old.iter().filter(|x| edge.contains(x)).cloned().collect(),
                };
                if entry[j].as_ref() != Some(&new) {
                    entry[j] = Some(new);
                    dirty[j] = true;
                    changed = true;
                }
            }
        }
        if !changed || !loops {
            break;
        }
    }
    entry.into_iter().map(Option::unwrap_or_default).collect()
}

/// `x` with a constant side read as a local block `b` leaves holding it
/// (`i = 1; if 1 < n`: `i < n` too).
pub(crate) fn named(f: &MirFunction, b: usize, x: &Fact) -> Vec<Fact> {
    let bb = &f.blocks[b];
    let ca = match &x.a {
        Operand::Const(c) => Some(c),
        Operand::Copy(_) => None,
    };
    let cb = match &x.b {
        Operand::Const(c) => Some(c),
        Operand::Copy(_) => None,
    };
    if ca.is_none() && cb.is_none() {
        return Vec::new();
    }
    // Locals assigned a constant and nothing after, in statement order.
    let mut held: Vec<(LocalId, &ConstVal)> = Vec::new();
    let mut later: HashSet<LocalId> = HashSet::default();
    for s in bb.stmts.iter().rev() {
        let l = match s {
            Statement::Assign { target, .. } => *target,
            Statement::StorageDead(l) | Statement::ArrayWrite { array: l, .. } => *l,
            _ => continue,
        };
        if let Statement::Assign {
            target,
            rvalue: RValue::Use(Operand::Const(v)),
        } = s
        {
            if !later.contains(target) && f.locals[target.0 as usize].ty == MirTy::Prim(x.ty) {
                held.push((*target, v));
            }
        }
        later.insert(l);
    }
    held.reverse();
    let mut out = Vec::new();
    if let Some(c) = ca {
        for &(l, _) in held.iter().filter(|(_, v)| *v == c) {
            out.push(Fact {
                a: Operand::Copy(l),
                ..x.clone()
            });
        }
    }
    if let Some(c) = cb {
        for &(l, _) in held.iter().filter(|(_, v)| *v == c) {
            out.push(Fact {
                b: Operand::Copy(l),
                ..x.clone()
            });
        }
    }
    out
}

/// Drops the facts about `l`, which changes. What they said of the other
/// side stays: `l < y` leaves `y` above the type's minimum (so `y - 1`
/// cannot overflow), `y < l` leaves it below the maximum.
pub(crate) fn forget(facts: &mut Vec<Fact>, l: LocalId, pw: u32) {
    let mut weak = Vec::new();
    for x in facts.iter().filter(|x| x.mentions(l)) {
        let Some((lo, hi)) = int_range(x.ty, pw) else {
            continue;
        };
        let (y, above) = match x.op {
            PrimOp::Lt if reads(&x.a, l) => (&x.b, true),
            PrimOp::Lt if reads(&x.b, l) => (&x.a, false),
            PrimOp::Gt if reads(&x.a, l) => (&x.b, false),
            PrimOp::Gt if reads(&x.b, l) => (&x.a, true),
            _ => continue,
        };
        if !matches!(y, Operand::Copy(v) if *v != l) {
            continue;
        }
        weak.push(if above {
            Fact {
                op: PrimOp::Gt,
                ty: x.ty,
                a: y.clone(),
                b: Operand::Const(int_const(lo, x.ty)),
            }
        } else {
            Fact {
                op: PrimOp::Lt,
                ty: x.ty,
                a: y.clone(),
                b: Operand::Const(int_const(hi, x.ty)),
            }
        });
    }
    facts.retain(|x| !x.mentions(l));
    for w in weak {
        if !facts.contains(&w) {
            facts.push(w);
        }
    }
}
