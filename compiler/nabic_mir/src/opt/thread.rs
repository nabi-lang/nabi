//! Jump threading. An edge on which the branch of a block further on is
//! known goes straight to the side taken: the blocks between are copied
//! for that edge, the branch in the copy a jump.
//!
//! What decides a branch is what the edge's source and the blocks after
//! it assign: constants, aggregates of them (a sum's tag built in an
//! inlined `next()`, matched by the loop around it), the arguments the
//! edges pass and comparisons of those. A local assigned a constant
//! once, where its assignment dominates the edge, counts as well.
//!
//! ```text
//! A: t = 0; goto J          A: t = 0; goto X
//! B: t = 1; goto J          B: t = 1; goto J
//! J: c = t == 1; if c X Y   J: c = t == 1; if c X Y
//! ```
//!
//! A loop header is never copied (that would give its loop a second way
//! in), nor a `strips` header; copies are bounded in size.

use crate::{BasicBlock, MirFunction, MirTy, Operand, RValue, Statement, Terminator};
use nabic_core::{HashMap, HashSet};
use nabic_ir_common::{BlockId, ConstVal, LocalId, PrimOp, PrimTy};

/// Blocks a thread passes through at most.
const DEPTH: usize = 6;

/// Statements copied for one edge at most, at every level: a thread
/// through an inlined `next()` takes apart more than it copies.
const BUDGET: usize = 16;

/// A value known on the way: a constant, or an aggregate of known and
/// unknown (`None`) fields.
#[derive(Clone, Debug, PartialEq)]
enum Val {
    C(ConstVal),
    Agg(Vec<Option<Val>>),
}

/// Locals known along one way through the blocks.
#[derive(Clone, Default)]
struct Env {
    vals: HashMap<LocalId, Val>,
}

struct Cx<'a> {
    pw: u32,
    idx: &'a HashMap<u32, usize>,
    preds: &'a [Vec<usize>],
    /// Blocks never copied.
    stop: &'a HashSet<u32>,
    /// Statements a thread may copy.
    limit: usize,
    /// Locals never tracked: addressed ones.
    pinned: &'a HashSet<LocalId>,
    /// Locals assigned a constant once, with the block of it.
    fixed: &'a HashMap<LocalId, (usize, Val)>,
    dom: &'a crate::opt::Dom,
}

impl Cx<'_> {
    fn get(&self, env: &Env, at: usize, o: &Operand) -> Option<Val> {
        match o {
            Operand::Const(c) => Some(Val::C(c.clone())),
            // Where the assignment dominates the read (in another block:
            // its own block's reads before it are walked).
            Operand::Copy(l) => env.vals.get(l).cloned().or_else(|| {
                let (d, v) = self.fixed.get(l)?;
                (*d != at && self.dom.dominates(*d, at)).then(|| v.clone())
            }),
        }
    }

    fn eval(&self, env: &Env, at: usize, rv: &RValue) -> Option<Val> {
        match rv {
            RValue::Use(o) => self.get(env, at, o),
            RValue::StructInit { fields, .. } => Some(Val::Agg(
                fields.iter().map(|o| self.get(env, at, o)).collect(),
            )),
            RValue::FieldRead { target, index, .. } => match self.get(env, at, target)? {
                Val::Agg(fs) => fs.get(*index as usize).cloned().flatten(),
                Val::C(_) => None,
            },
            RValue::WithField {
                source,
                index,
                value,
                ..
            } => match self.get(env, at, source)? {
                Val::Agg(mut fs) => {
                    *fs.get_mut(*index as usize)? = self.get(env, at, value);
                    Some(Val::Agg(fs))
                }
                Val::C(_) => None,
            },
            RValue::BinOp {
                op,
                op_ty,
                lhs,
                rhs,
            } => {
                let (Val::C(a), Val::C(b)) = (self.get(env, at, lhs)?, self.get(env, at, rhs)?)
                else {
                    return None;
                };
                binop(*op, op_ty, &a, &b, self.pw).map(Val::C)
            }
            RValue::UnaryOp {
                op: PrimOp::Not | PrimOp::LogicalNot,
                op_ty: MirTy::Prim(PrimTy::Bool),
                operand,
            } => match self.get(env, at, operand)? {
                Val::C(ConstVal::Bool(b)) => Some(Val::C(ConstVal::Bool(!b))),
                _ => None,
            },
            _ => None,
        }
    }

    /// Statement `s` of block `at` run under `env`.
    fn step(&self, env: &mut Env, at: usize, s: &Statement) {
        match s {
            Statement::Assign { target, rvalue } => {
                let v = if self.pinned.contains(target) {
                    None
                } else {
                    self.eval(env, at, rvalue)
                };
                match v {
                    Some(v) => env.vals.insert(*target, v),
                    None => env.vals.remove(target),
                };
            }
            Statement::ArrayWrite { array: l, .. } | Statement::StorageDead(l) => {
                env.vals.remove(l);
            }
            _ => {}
        }
    }

    /// Edge arguments `args` into parameters `ps` (all read first).
    fn enter(&self, env: &mut Env, at: usize, ps: &[LocalId], args: &[Operand]) {
        let vs: Vec<Option<Val>> = args.iter().map(|a| self.get(env, at, a)).collect();
        for (p, v) in ps.iter().zip(vs) {
            match v {
                Some(v) if !self.pinned.contains(p) => env.vals.insert(*p, v),
                _ => env.vals.remove(p),
            };
        }
    }
}

/// `a op b` on constants, where the result is a `bool` or of `op_ty`.
fn binop(op: PrimOp, op_ty: &MirTy, a: &ConstVal, b: &ConstVal, pw: u32) -> Option<ConstVal> {
    let MirTy::Prim(p) = op_ty else {
        return None;
    };
    if let (ConstVal::Bool(x), ConstVal::Bool(y)) = (a, b) {
        let (x, y) = (*x, *y);
        return Some(ConstVal::Bool(match op {
            PrimOp::Eq => x == y,
            PrimOp::Ne | PrimOp::BitXor => x != y,
            PrimOp::BitAnd | PrimOp::LogicalAnd => x && y,
            PrimOp::BitOr | PrimOp::LogicalOr => x || y,
            _ => return None,
        }));
    }
    if !p.is_int() {
        return None;
    }
    let (x, y) = (
        crate::opt::int_val(a, *p, pw)?,
        crate::opt::int_val(b, *p, pw)?,
    );
    Some(ConstVal::Bool(match op {
        PrimOp::Eq => x == y,
        PrimOp::Ne => x != y,
        PrimOp::Lt => x < y,
        PrimOp::Le => x <= y,
        PrimOp::Gt => x > y,
        PrimOp::Ge => x >= y,
        _ => return None,
    }))
}

/// Statements that cost something to copy.
fn cost(b: &BasicBlock) -> usize {
    b.stmts.iter().filter(|s| !crate::opt::marker(s)).count()
}

/// Locals assigned once, a constant (or an aggregate of constants), with
/// the block index of the assignment.
fn fixed(f: &MirFunction, pinned: &HashSet<LocalId>) -> HashMap<LocalId, (usize, Val)> {
    let mut defs: HashMap<LocalId, Option<(usize, Val)>> = HashMap::default();
    for p in f
        .params
        .iter()
        .chain(f.blocks.iter().flat_map(|b| &b.params))
    {
        defs.insert(*p, None);
    }
    for (i, b) in f.blocks.iter().enumerate() {
        for s in &b.stmts {
            let (l, v) = match s {
                Statement::Assign { target, rvalue } => (*target, konst(rvalue)),
                Statement::ArrayWrite { array, .. } => (*array, None),
                _ => continue,
            };
            match defs.entry(l) {
                std::collections::hash_map::Entry::Occupied(mut e) => {
                    e.insert(None);
                }
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(v.map(|v| (i, v)));
                }
            }
        }
    }
    defs.into_iter()
        .filter_map(|(l, d)| Some((l, d?)))
        .filter(|(l, _)| !pinned.contains(l))
        .collect()
}

/// The constant value `rv` builds of constants alone.
fn konst(rv: &RValue) -> Option<Val> {
    match rv {
        RValue::Use(Operand::Const(c)) => Some(Val::C(c.clone())),
        RValue::StructInit { fields, .. } => {
            let fs: Vec<Option<Val>> = fields
                .iter()
                .map(|o| match o {
                    Operand::Const(c) => Some(Val::C(c.clone())),
                    Operand::Copy(_) => None,
                })
                .collect();
            fs.iter().any(Option::is_some).then_some(Val::Agg(fs))
        }
        _ => None,
    }
}

/// A way found: from block `from` (successor number `arm`), through
/// `path` (block indices) whose branches the walk decided, the last one
/// going on to `to` with `args`.
struct Thread {
    from: usize,
    arm: usize,
    path: Vec<usize>,
    /// Per block of `path` but the last: the edge's arguments into the
    /// next one.
    hops: Vec<Vec<Operand>>,
    first_args: Vec<Operand>,
    to: BlockId,
    args: Vec<Operand>,
}

/// The way on from block `bi`'s terminator under `env`: the successor
/// taken (number and edge) if known.
fn decide(cx: &Cx<'_>, env: &Env, bi: usize, t: &Terminator) -> Option<(BlockId, Vec<Operand>)> {
    match t {
        Terminator::Goto { target, args } => Some((*target, args.clone())),
        Terminator::If {
            cond,
            then_bb,
            then_args,
            else_bb,
            else_args,
        } => match cx.get(env, bi, cond)? {
            Val::C(ConstVal::Bool(true)) => Some((*then_bb, then_args.clone())),
            Val::C(ConstVal::Bool(false)) => Some((*else_bb, else_args.clone())),
            _ => None,
        },
        _ => None,
    }
}

/// Jump threading over `f`; whether anything changed.
pub fn run(f: &mut MirFunction, pw: u32) -> bool {
    if f.blocks
        .iter()
        .any(|b| matches!(b.terminator, Terminator::Suspend { .. }))
    {
        return false;
    }
    // One pass: a walk already goes through as many branches as it can
    // decide, so a second finds little.
    round(f, pw)
}

fn round(f: &mut MirFunction, pw: u32) -> bool {
    let idx = crate::opt::index(f);
    let preds = crate::opt::preds(f, &idx);
    // Nothing joins: nothing to thread.
    if !preds.iter().any(|p| p.len() > 1) {
        return false;
    }
    let mut headers: Vec<usize> = Vec::new();
    let mut hot = vec![false; f.blocks.len()];
    // Per block, the last loop that walked it.
    let mut seen = vec![usize::MAX; f.blocks.len()];
    // Only loops run a branch often enough to pay for a copy.
    for (l, hs) in crate::rotate::back_edges(f) {
        for h in hs {
            let h = idx[&h];
            let k = headers.len();
            headers.push(h);
            hot[h] = true;
            seen[h] = k;
            let mut work = vec![idx[&l]];
            while let Some(b) = work.pop() {
                if seen[b] != k {
                    seen[b] = k;
                    hot[b] = true;
                    work.extend(&preds[b]);
                }
            }
        }
    }
    let mut stop: HashSet<u32> = f.strips.iter().copied().collect();
    stop.insert(f.blocks[0].id.0);
    stop.extend(headers.iter().map(|&h| f.blocks[h].id.0));
    // The edges worth a walk: into a join, through jumps alone.
    let joins = |x: usize| -> bool {
        let mut cur = x;
        for _ in 0..DEPTH {
            let b = &f.blocks[cur];
            if stop.contains(&b.id.0) {
                return false;
            }
            if preds[cur].len() > 1 {
                return true;
            }
            match &b.terminator {
                Terminator::Goto { target, .. } => match idx.get(&target.0) {
                    Some(&n) => cur = n,
                    None => return false,
                },
                _ => return false,
            }
        }
        false
    };
    let mut edges: Vec<(usize, usize, usize)> = Vec::new();
    for (p, b) in f.blocks.iter().enumerate() {
        if !hot[p] {
            continue;
        }
        for (arm, t) in crate::opt::succs(&b.terminator).into_iter().enumerate() {
            if let Some(&x) = idx.get(&t.0) {
                if joins(x) {
                    edges.push((p, arm, x));
                }
            }
        }
    }
    if edges.is_empty() {
        return false;
    }
    let dom = crate::opt::Dom::new(f, &idx, &preds);
    let pinned = crate::addressed(f);
    let fixed = fixed(f, &pinned);
    let cx = Cx {
        pw,
        idx: &idx,
        preds: &preds,
        stop: &stop,
        limit: BUDGET,
        pinned: &pinned,
        fixed: &fixed,
        dom: &dom,
    };
    let mut found: Vec<Thread> = Vec::new();
    // One thread per edge source a round: the copies change the rest.
    let mut used: HashSet<usize> = HashSet::default();
    let mut base: Option<(usize, Env)> = None;
    for (p, arm, x) in edges {
        if used.contains(&p) || !dom.reachable(p) {
            continue;
        }
        let pb = &f.blocks[p];
        let (args, cond): (&[Operand], Option<(LocalId, bool)>) = match &pb.terminator {
            Terminator::Goto { args, .. } => (args, None),
            Terminator::If {
                cond,
                then_args,
                else_args,
                ..
            } => {
                let c = match cond {
                    Operand::Copy(l) if !pinned.contains(l) => Some((*l, arm == 0)),
                    _ => None,
                };
                (if arm == 0 { then_args } else { else_args }, c)
            }
            _ => continue,
        };
        if base.as_ref().is_none_or(|(b, _)| *b != p) {
            let mut env = Env::default();
            for s in &pb.stmts {
                cx.step(&mut env, p, s);
            }
            base = Some((p, env));
        }
        let mut env = base.as_ref().expect("set above").1.clone();
        if let Some((c, v)) = cond {
            env.vals.insert(c, Val::C(ConstVal::Bool(v)));
        }
        if let Some(t) = walk(&cx, f, env, (p, arm), x, args) {
            used.insert(p);
            found.push(t);
        }
    }
    if found.is_empty() {
        return false;
    }
    for t in found {
        apply(f, &t);
    }
    true
}

/// Follows the edge `(p, arm)` (successor number `arm` of block `p`)
/// into block `x` under `env` while the way on is known; a thread if it
/// decided a branch.
fn walk(
    cx: &Cx<'_>,
    f: &MirFunction,
    mut env: Env,
    (p, arm): (usize, usize),
    x: usize,
    args: &[Operand],
) -> Option<Thread> {
    let mut path: Vec<usize> = Vec::new();
    let mut hops: Vec<Vec<Operand>> = Vec::new();
    let mut spent = 0;
    // Past a join, every block is a copy.
    let mut joined = false;
    let mut cur = x;
    let mut in_args = args.to_vec();
    let mut best: Option<(usize, BlockId, Vec<Operand>)> = None;
    loop {
        let b = &f.blocks[cur];
        joined |= cx.preds[cur].len() > 1;
        if joined {
            spent += cost(b);
        }
        if spent > cx.limit || path.contains(&cur) {
            break;
        }
        cx.enter(&mut env, cur, &b.params, &in_args);
        for s in &b.stmts {
            cx.step(&mut env, cur, s);
        }
        path.push(cur);
        let Some((to, out)) = decide(cx, &env, cur, &b.terminator) else {
            break;
        };
        if matches!(b.terminator, Terminator::If { .. }) && joined {
            best = Some((path.len(), to, out.clone()));
        }
        let Some(&next) = cx.idx.get(&to.0) else {
            break;
        };
        if path.len() >= DEPTH || cx.stop.contains(&to.0) {
            break;
        }
        hops.push(out.clone());
        in_args = out;
        cur = next;
    }
    let (n, to, out) = best?;
    path.truncate(n);
    hops.truncate(n - 1);
    Some(Thread {
        from: p,
        arm,
        path,
        hops,
        first_args: args.to_vec(),
        to,
        args: out,
    })
}

/// `ps = args` as statements, through temporaries where an argument
/// reads a parameter assigned before it.
fn bind(f: &mut MirFunction, ps: &[LocalId], args: &[Operand]) -> Vec<Statement> {
    let clash = args
        .iter()
        .any(|a| matches!(a, Operand::Copy(l) if ps.contains(l)));
    let mut out = Vec::new();
    if clash {
        let tmps: Vec<LocalId> = ps
            .iter()
            .map(|p| {
                let ty = f.locals[p.0 as usize].ty.clone();
                crate::opt::fresh_local(f, ty)
            })
            .collect();
        for (t, a) in tmps.iter().zip(args) {
            out.push(Statement::Assign {
                target: *t,
                rvalue: RValue::Use(a.clone()),
            });
        }
        for (p, t) in ps.iter().zip(&tmps) {
            out.push(Statement::Assign {
                target: *p,
                rvalue: RValue::Use(Operand::Copy(*t)),
            });
        }
    } else {
        for (p, a) in ps.iter().zip(args) {
            out.push(Statement::Assign {
                target: *p,
                rvalue: RValue::Use(a.clone()),
            });
        }
    }
    out
}

/// Copies the blocks of `t.path` for its edge.
fn apply(f: &mut MirFunction, t: &Thread) {
    let mut next = crate::opt::fresh_block(f).0;
    let ids: Vec<BlockId> = t
        .path
        .iter()
        .map(|_| {
            next += 1;
            BlockId(next - 1)
        })
        .collect();
    let mut copies = Vec::with_capacity(t.path.len());
    for (k, &bi) in t.path.iter().enumerate() {
        let src = f.blocks[bi].clone();
        let into = if k == 0 {
            &t.first_args
        } else {
            &t.hops[k - 1]
        };
        let mut stmts = bind(f, &src.params, into);
        stmts.extend(src.stmts);
        let terminator = if k + 1 < t.path.len() {
            Terminator::Goto {
                target: ids[k + 1],
                args: Vec::new(),
            }
        } else {
            Terminator::Goto {
                target: t.to,
                args: t.args.clone(),
            }
        };
        copies.push(BasicBlock {
            id: ids[k],
            params: Vec::new(),
            stmts,
            terminator,
        });
    }
    let first = ids[0];
    match &mut f.blocks[t.from].terminator {
        Terminator::Goto { target, args } => {
            *target = first;
            args.clear();
        }
        Terminator::If {
            then_bb,
            then_args,
            else_bb,
            else_args,
            ..
        } => {
            if t.arm == 0 {
                *then_bb = first;
                then_args.clear();
            } else {
                *else_bb = first;
                else_args.clear();
            }
        }
        _ => unreachable!("an edge source jumps"),
    }
    f.blocks.extend(copies);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LocalDecl;
    use nabic_diagnostic::Span;
    use nabic_ir_common::{FunctionId, PrimTy};

    const fn goto(t: u32) -> Terminator {
        Terminator::Goto {
            target: BlockId(t),
            args: Vec::new(),
        }
    }

    const fn branch(c: u32, then_bb: u32, else_bb: u32) -> Terminator {
        Terminator::If {
            cond: Operand::Copy(LocalId(c)),
            then_bb: BlockId(then_bb),
            then_args: Vec::new(),
            else_bb: BlockId(else_bb),
            else_args: Vec::new(),
        }
    }

    const fn block(id: u32, stmts: Vec<Statement>, terminator: Terminator) -> BasicBlock {
        BasicBlock {
            id: BlockId(id),
            params: Vec::new(),
            stmts,
            terminator,
        }
    }

    const fn tag(t: u32, v: i128) -> Statement {
        Statement::Assign {
            target: LocalId(t),
            rvalue: RValue::Use(Operand::Const(ConstVal::Int {
                value: v,
                ty: PrimTy::I32,
            })),
        }
    }

    /// `loop { t = if p { 1 } else { 0 }; if t == 1 { continue } return }`:
    /// x0 `p` (a parameter), x1 `t`, x2 `t == 1`.
    fn tagged() -> MirFunction {
        let ty = |i: u32| match i {
            1 => MirTy::Prim(PrimTy::I32),
            _ => MirTy::Prim(PrimTy::Bool),
        };
        MirFunction {
            id: FunctionId(0),
            def_id: None,
            name: "f".into(),
            facts: nabic_ir_common::FnFacts::default(),
            strips: Vec::new(),
            pins: Vec::new(),
            dbg: crate::FnDbg::default(),
            params: vec![LocalId(0)],
            locals: (0..3)
                .map(|i| LocalDecl {
                    id: LocalId(i),
                    name: None,
                    ty: ty(i),
                })
                .collect(),
            blocks: vec![
                block(0, vec![], goto(1)),
                block(1, vec![], branch(0, 2, 3)),
                block(2, vec![tag(1, 1)], goto(4)),
                block(3, vec![tag(1, 0)], goto(4)),
                block(
                    4,
                    vec![Statement::Assign {
                        target: LocalId(2),
                        rvalue: RValue::BinOp {
                            op: PrimOp::Eq,
                            op_ty: MirTy::Prim(PrimTy::I32),
                            lhs: Operand::Copy(LocalId(1)),
                            rhs: Operand::Const(ConstVal::Int {
                                value: 1,
                                ty: PrimTy::I32,
                            }),
                        },
                    }],
                    branch(2, 1, 5),
                ),
                block(5, vec![], Terminator::Return(None)),
            ],
            param_tys: vec![MirTy::Prim(PrimTy::Bool)],
            ret_ty: MirTy::Unit,
            span: Span::default(),
        }
    }

    /// Where block `b` jumps, through the copies threading made.
    fn lands(f: &MirFunction, b: u32) -> u32 {
        let mut at = b;
        for _ in 0..8 {
            let blk = f.blocks.iter().find(|x| x.id.0 == at).expect("block");
            match &blk.terminator {
                Terminator::Goto { target, .. } if target.0 > 5 || target.0 == 4 => at = target.0,
                Terminator::Goto { target, .. } => return target.0,
                _ => return at,
            }
        }
        at
    }

    #[test]
    fn a_tag_known_on_the_way_in_skips_its_test() {
        let mut f = tagged();
        assert!(run(&mut f, 64));
        // `t = 1` goes back to the loop, `t = 0` returns: no way through
        // the test of `t` is left.
        assert_eq!(lands(&f, 2), 1);
        assert_eq!(lands(&f, 3), 5);
    }

    #[test]
    fn outside_a_loop_nothing_is_copied() {
        let mut f = tagged();
        f.blocks[4].terminator = branch(2, 5, 5);
        assert!(!run(&mut f, 64));
    }
}
