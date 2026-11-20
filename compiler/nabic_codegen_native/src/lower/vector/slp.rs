//! Straight-line vectors, above `-O0`: a vector's worth of lanes that
//! a block computes by the same tree of operations, stored to adjacent
//! elements of one buffer or built into a struct of numbers, as vector
//! code where the emitter has `Inst::V`.
//!
//! ```text
//! y0 = buf_load c[i]; m0 = Mul(a, x0); s0 = Add(y0, m0); buf_store c[i] = s0
//! y1 = buf_load c[i + 1]; m1 = Mul(a, x1); s1 = Add(y1, m1); buf_store c[i + 1] = s1
//! ```
//!
//! becomes a vector load of `c[i..]`, a splat of `a`, a build of `x0,
//! x1`, a multiply, an add and a vector store. Each lane runs its own
//! operations in its own order: nothing is reassociated or fused, so
//! floats round as they do one by one. Only operations that never trap
//! join (floats, wrapping integers, bit operations); the vector code
//! goes where the last lane's store (or the build) was, and nothing
//! between the first statement it takes and there writes memory, calls,
//! or traps, but the `+ k` of a lane's index (the lane's access is in
//! bounds, so that sum cannot overflow). Loads it takes come before any
//! store it makes. The operands it reads keep their values meanwhile.

use crate::cc;
use crate::lir::{Vx, VxOp};
use crate::lower::vector::simd::{inserts, straight};
#[allow(clippy::wildcard_imports)]
use crate::lower::vector::*;

/// A vector's lanes: a load of adjacent elements, a splat, lanes put
/// together, or an operation of two vectors.
#[derive(Clone, Debug)]
pub(crate) enum Node {
    Load { buf: Operand, index: Operand },
    Splat(Operand),
    Build(Vec<Operand>),
    Op { op: VBin, a: usize, b: usize },
}

/// Where the lanes go.
#[derive(Clone, Debug)]
pub(crate) enum Seed {
    /// Adjacent elements from `index` on.
    Store { buf: Operand, index: Operand },
    /// The struct `target`, `off` bytes in.
    Init { target: LocalId, off: u32 },
}

/// One vector of lanes, its nodes in order (each after its operands),
/// the last the result.
#[derive(Clone, Debug)]
pub(crate) struct Pack {
    pub(crate) w: u32,
    pub(crate) vb: u32,
    pub(crate) p: PrimTy,
    pub(crate) nodes: Vec<Node>,
    pub(crate) seed: Seed,
}

/// By `(block position, statement)`: the packs a statement emits, or
/// none (a statement a pack took).
pub(crate) type Plan = HashMap<(usize, usize), Vec<Pack>>;

/// The vector operation of a lane's `op` on `p`.
const fn vbin(op: PrimOp, p: PrimTy, checks: bool) -> Option<VBin> {
    if p.is_float() {
        return Some(match op {
            PrimOp::Add => VBin::FAdd,
            PrimOp::Sub => VBin::FSub,
            PrimOp::Mul => VBin::FMul,
            PrimOp::Div => VBin::FDiv,
            _ => return None,
        });
    }
    Some(match op {
        PrimOp::WrappingAdd => VBin::Add,
        PrimOp::WrappingSub => VBin::Sub,
        PrimOp::WrappingMul => VBin::Mul,
        PrimOp::Add if !checks => VBin::Add,
        PrimOp::Sub if !checks => VBin::Sub,
        PrimOp::Mul if !checks => VBin::Mul,
        PrimOp::BitAnd => VBin::And,
        PrimOp::BitOr => VBin::Or,
        PrimOp::BitXor => VBin::Xor,
        _ => return None,
    })
}

/// What the function's statements say of its locals.
struct Facts<'f> {
    /// Reads of each local.
    reads: HashMap<LocalId, u32>,
    /// The one assignment: `(block, statement, right side)`.
    def: HashMap<LocalId, (usize, usize, &'f RValue)>,
}

impl<'f> Facts<'f> {
    fn of(f: &'f MirFunction) -> Self {
        let mut sets: HashMap<LocalId, u32> = HashMap::default();
        let mut reads: HashMap<LocalId, u32> = HashMap::default();
        let mut def = HashMap::default();
        for p in &f.params {
            *sets.entry(*p).or_default() += 1;
        }
        for (bi, b) in f.blocks.iter().enumerate() {
            for p in &b.params {
                *sets.entry(*p).or_default() += 2;
            }
            for (si, s) in b.stmts.iter().enumerate() {
                nabic_mir::stmt_reads(s, &mut |x| *reads.entry(x).or_default() += 1);
                if let Statement::Assign { target, rvalue } = s {
                    *sets.entry(*target).or_default() += 1;
                    def.insert(*target, (bi, si, rvalue));
                }
            }
            nabic_mir::term_reads(&b.terminator, &mut |x| *reads.entry(x).or_default() += 1);
        }
        def.retain(|l, _| sets.get(l) == Some(&1));
        Self { reads, def }
    }

    /// `index` as `base + c` (`base` none: a constant).
    fn split(&self, index: &Operand) -> Option<(Option<LocalId>, i128)> {
        let small = |o: &Operand| match o {
            Operand::Const(ConstVal::UInt { value, .. }) => {
                i128::try_from(*value).ok().filter(|v| *v < 64)
            }
            Operand::Const(ConstVal::Int { value, .. }) => {
                Some(*value).filter(|v| (0..64).contains(v))
            }
            _ => None,
        };
        match index {
            Operand::Const(ConstVal::UInt { value, .. }) => {
                Some((None, i128::try_from(*value).ok()?))
            }
            Operand::Const(ConstVal::Int { value, .. }) => Some((None, *value)),
            Operand::Const(_) => None,
            Operand::Copy(l) => {
                if let Some((
                    _,
                    _,
                    RValue::BinOp {
                        op: PrimOp::Add | PrimOp::WrappingAdd,
                        op_ty: MirTy::Prim(PrimTy::Usize),
                        lhs,
                        rhs,
                    },
                )) = self.def.get(l)
                {
                    if let Some(c) = small(rhs) {
                        let (b, d) = self.split(lhs)?;
                        return Some((b, d + c));
                    }
                    if let Some(c) = small(lhs) {
                        let (b, d) = self.split(rhs)?;
                        return Some((b, d + c));
                    }
                }
                Some((Some(*l), 0))
            }
        }
    }

    /// The `+ k` of an index, which cannot overflow next to an access.
    const fn index_sum(rv: &RValue) -> bool {
        matches!(rv, RValue::BinOp {
            op: PrimOp::Add | PrimOp::WrappingAdd,
            op_ty: MirTy::Prim(PrimTy::Usize),
            lhs,
            rhs,
        } if matches!(lhs, Operand::Const(_)) || matches!(rhs, Operand::Const(_)))
    }
}

/// The planning of one block.
struct Block<'a, 'f> {
    facts: &'a Facts<'f>,
    stmts: &'f [Statement],
    pos: usize,
    p: PrimTy,
    w: u32,
    isa: Isa,
    checks: bool,
    /// Statements no pack may take (other plans', or taken).
    taken: &'a HashSet<(usize, usize)>,
    /// The statements this pack takes.
    claim: Vec<usize>,
    nodes: Vec<Node>,
}

impl<'f> Block<'_, 'f> {
    /// The local `o` holds when one statement of this block before `at`,
    /// free to take, sets it and only one statement reads it.
    fn own(&self, o: &Operand, at: usize) -> Option<(usize, &'f RValue)> {
        let Operand::Copy(l) = o else { return None };
        let &(bi, si, rv) = self.facts.def.get(l)?;
        (bi == self.pos
            && si < at
            && self.facts.reads.get(l) == Some(&1)
            && !self.taken.contains(&(bi, si))
            && !self.claim.contains(&si))
        .then_some((si, rv))
    }

    /// The node of the lanes `ops`, read by statements before `at`.
    fn node(&mut self, ops: &[Operand], at: usize) -> Option<usize> {
        let ty = MirTy::Prim(self.p);
        let mut defs = Vec::with_capacity(ops.len());
        for o in ops {
            match self.own(o, at) {
                Some(d) => defs.push(d),
                None => break,
            }
        }
        if defs.len() == ops.len() {
            // One operation in every lane.
            let first = defs[0].1.clone();
            if let RValue::BinOp {
                op,
                op_ty,
                lhs: _,
                rhs: _,
            } = &first
            {
                let same = defs.iter().all(|(_, rv)| {
                    matches!(rv, RValue::BinOp { op: o, op_ty: t, .. } if o == op && t == op_ty)
                });
                if let (true, Some(v)) = (*op_ty == ty && same, vbin(*op, self.p, self.checks)) {
                    if self.isa.bin(v, self.w) {
                        let mark = (self.claim.len(), self.nodes.len());
                        let (ls, rs): (Vec<Operand>, Vec<Operand>) = defs
                            .iter()
                            .map(|(_, rv)| match rv {
                                RValue::BinOp { lhs, rhs, .. } => (lhs.clone(), rhs.clone()),
                                _ => unreachable!("checked above"),
                            })
                            .unzip();
                        self.claim.extend(defs.iter().map(|d| d.0));
                        if let Some(a) = self.node(&ls, at) {
                            if let Some(b) = self.node(&rs, at) {
                                self.nodes.push(Node::Op { op: v, a, b });
                                return Some(self.nodes.len() - 1);
                            }
                        }
                        self.claim.truncate(mark.0);
                        self.nodes.truncate(mark.1);
                    }
                }
            }
            // Adjacent elements of one buffer.
            if let RValue::BufLoad {
                buf: buf @ Operand::Copy(_),
                index,
                elem_ty,
            } = &first
            {
                let at0 = self.facts.split(index);
                let adjacent = at0.is_some_and(|(base, c)| {
                    defs.iter().enumerate().all(|(k, (_, rv))| {
                        matches!(rv, RValue::BufLoad { buf: b, index: i, elem_ty: e }
                            if b == buf && e == elem_ty
                                && self.facts.split(i) == Some((base, c + k as i128)))
                    })
                });
                if *elem_ty == ty && adjacent {
                    self.claim.extend(defs.iter().map(|d| d.0));
                    self.nodes.push(Node::Load {
                        buf: buf.clone(),
                        index: index.clone(),
                    });
                    return Some(self.nodes.len() - 1);
                }
            }
        }
        if ops.iter().all(|o| *o == ops[0]) {
            self.nodes.push(Node::Splat(ops[0].clone()));
        } else {
            if !inserts(self.isa, self.w) {
                return None;
            }
            self.nodes.push(Node::Build(ops.to_vec()));
        }
        Some(self.nodes.len() - 1)
    }

    /// Whether the statements from the first one taken to `seed` leave
    /// the pack's meaning alone (`stores`: the lanes' stores, taken),
    /// and whether it costs less than its statements.
    fn sound(
        &self,
        seed: usize,
        stores: &[usize],
        reads: &[LocalId],
        deaths: &[Vec<LocalId>],
    ) -> bool {
        if !self.nodes.iter().any(|n| matches!(n, Node::Op { .. })) {
            return false;
        }
        let first = self
            .claim
            .iter()
            .chain(stores)
            .copied()
            .min()
            .unwrap_or(seed)
            .min(seed);
        let first_store = stores.iter().copied().min();
        for i in first..=seed {
            let s = &self.stmts[i];
            // The seed: the build itself, or the last lane's store.
            let taken = i == seed || self.claim.contains(&i) || stores.contains(&i);
            let stored = first_store.is_some_and(|f| i > f);
            match s {
                Statement::Loc(..) => {}
                Statement::Assign { rvalue, .. } if taken => {
                    // A load past a store of the pack reads an element
                    // none of those stores wrote: another one of the
                    // same buffer.
                    if let (true, RValue::BufLoad { buf, index, .. }) = (stored, rvalue) {
                        let at = self.facts.split(index);
                        let apart = stores.iter().filter(|j| **j < i).all(|j| {
                            matches!(&self.stmts[*j], Statement::BufStore { buf: b, index: x, .. }
                            if b == buf && at.is_some() && {
                                let y = self.facts.split(x);
                                y.is_some_and(|y| at.is_some_and(|a| a.0 == y.0 && a.1 != y.1))
                            })
                        });
                        if !apart {
                            return false;
                        }
                    }
                }
                Statement::BufStore { .. } if taken => {}
                Statement::Assign { target, rvalue } => {
                    if reads.contains(target) {
                        return false;
                    }
                    let ok = match rvalue {
                        RValue::Use(_) | RValue::Bitcast { .. } | RValue::FieldRead { .. } => true,
                        RValue::BufLoad { .. } | RValue::HeapField { .. } => !stored,
                        rv if Facts::index_sum(rv) => true,
                        RValue::BinOp { op, op_ty, .. } => {
                            let float = matches!(op_ty, MirTy::Prim(p) if p.is_float());
                            float
                                || matches!(
                                    op,
                                    PrimOp::WrappingAdd
                                        | PrimOp::WrappingSub
                                        | PrimOp::WrappingMul
                                        | PrimOp::BitAnd
                                        | PrimOp::BitOr
                                        | PrimOp::BitXor
                                        | PrimOp::Eq
                                        | PrimOp::Ne
                                        | PrimOp::Lt
                                        | PrimOp::Le
                                        | PrimOp::Gt
                                        | PrimOp::Ge
                                )
                                || (!self.checks
                                    && matches!(op, PrimOp::Add | PrimOp::Sub | PrimOp::Mul))
                        }
                        _ => false,
                    };
                    if !ok {
                        return false;
                    }
                }
                _ => return false,
            }
            // An operand cleared before the pack reads it.
            if i < seed
                && deaths
                    .get(i)
                    .is_some_and(|d| d.iter().any(|l| reads.contains(l)))
            {
                return false;
            }
        }
        // Statements against instructions: a build is a lane each; an
        // address is computed either way.
        let lanes = self.claim.len() + stores.len();
        let cost: usize = self
            .nodes
            .iter()
            .map(|n| match n {
                Node::Build(v) => v.len(),
                Node::Load { .. } | Node::Splat(_) | Node::Op { .. } => 1,
            })
            .sum::<usize>()
            + 1;
        cost < lanes
    }
}

/// A run of stores: the buffer, the lanes' type, the index's base.
type Key = (LocalId, PrimTy, Option<LocalId>);

/// The packs of one seed: the statement they go at, the lanes' type,
/// per vector its lanes' values and place, the lanes' stores.
struct Group {
    at: usize,
    p: PrimTy,
    w: u32,
    parts: Vec<(Vec<Operand>, Seed)>,
    stores: Vec<usize>,
}

/// The locals a pack's nodes read at its seed.
fn node_reads(nodes: &[Node], seed: &Seed) -> Vec<LocalId> {
    let mut out = Vec::new();
    let mut op = |o: &Operand| {
        if let Operand::Copy(l) = o {
            out.push(*l);
        }
    };
    for n in nodes {
        match n {
            Node::Load { buf, index } => {
                op(buf);
                op(index);
            }
            Node::Splat(o) => op(o),
            Node::Build(v) => v.iter().for_each(&mut op),
            Node::Op { .. } => {}
        }
    }
    if let Seed::Store { buf, index } = seed {
        op(buf);
        op(index);
    }
    out
}

impl FnLower<'_, '_> {
    /// The straight-line packs of the function, none of the statements
    /// another plan (`combine`, `fuse`) takes.
    pub(crate) fn slp_plan(&self) -> Plan {
        let mut out = Plan::default();
        let Some(isa) = Isa::of(self.c.t) else {
            return out;
        };
        let Some(vb) = straight(isa) else {
            return out;
        };
        // The SSE2 forms, as `simd_call`.
        let isa = match isa {
            Isa::X64 { win, .. } => Isa::X64 { win, avx2: false },
            i => i,
        };
        let f = self.f;
        let t = self.c.t;
        let facts = Facts::of(f);
        let mut taken: HashSet<(usize, usize)> = HashSet::default();
        taken.extend(self.combined.keys().copied());
        taken.extend(self.fuse.inits.iter().copied());
        taken.extend(self.fuse.skip.iter().copied());
        taken.extend(self.fuse.rets.keys().copied());
        taken.extend(self.dead.iter().copied());
        taken.extend(self.wb.keys().copied());
        taken.extend(self.fstores.keys().copied());
        let num = |ty: &MirTy| match ty {
            MirTy::Prim(p) if *p != PrimTy::Bool && (p.is_int() || p.is_float()) => {
                cc::scalar_k(ty, t).map(|k| (*p, k.size()))
            }
            _ => None,
        };
        for (pos, b) in f.blocks.iter().enumerate() {
            let deaths: &[Vec<LocalId>] =
                self.deaths.after_stmt.get(pos).map_or(&[], Vec::as_slice);
            // Seeds: struct builds of numbers of one type (all their
            // vectors or none), then runs of stores to adjacent elements.
            let mut seeds: Vec<Group> = Vec::new();
            for (i, s) in b.stmts.iter().enumerate() {
                let Statement::Assign {
                    target,
                    rvalue: RValue::StructInit { ty, fields },
                } = s
                else {
                    continue;
                };
                let MirTy::Struct(_, ftys) = ty else { continue };
                let Some((p, w)) = ftys.first().and_then(num) else {
                    continue;
                };
                let offs = t.field_offsets(ty);
                let lanes = (vb / w) as usize;
                if ftys.iter().any(|x| x != &ftys[0])
                    || offs.iter().enumerate().any(|(k, o)| *o != k as u32 * w)
                    || t.size_align(ty).0 != w * ftys.len() as u32
                    || ftys.len() % lanes != 0
                    || self.addressed.contains(target)
                    || !matches!(self.local_mem(*target), Some(Mem::Frame(..)))
                    || taken.contains(&(pos, i))
                {
                    continue;
                }
                let parts = fields
                    .chunks(lanes)
                    .enumerate()
                    .map(|(c, ops)| {
                        let seed = Seed::Init {
                            target: *target,
                            off: c as u32 * vb,
                        };
                        (ops.to_vec(), seed)
                    })
                    .collect();
                seeds.push(Group {
                    at: i,
                    p,
                    w,
                    parts,
                    stores: Vec::new(),
                });
            }
            let mut runs: HashMap<Key, Vec<(i128, usize)>> = HashMap::default();
            let mut bad: HashSet<Key> = HashSet::default();
            for (i, s) in b.stmts.iter().enumerate() {
                if let Statement::BufStore {
                    buf: Operand::Copy(bl),
                    index,
                    elem_ty,
                    ..
                } = s
                {
                    let Some((p, _)) = num(elem_ty) else { continue };
                    let Some((base, c)) = facts.split(index) else {
                        continue;
                    };
                    let key = (*bl, p, base);
                    let v = runs.entry(key).or_default();
                    if v.iter().any(|x| x.0 == c) || taken.contains(&(pos, i)) {
                        bad.insert(key);
                    }
                    v.push((c, i));
                }
            }
            let mut keys: Vec<Key> = runs.keys().filter(|k| !bad.contains(*k)).copied().collect();
            keys.sort_by_key(|k| runs[k].iter().map(|x| x.1).min());
            for key in keys {
                let mut v = runs[&key].clone();
                v.sort_unstable();
                let w = cc::scalar_k(&MirTy::Prim(key.1), t).map_or(8, K::size);
                let lanes = (vb / w) as usize;
                let mut at = 0;
                while at + lanes <= v.len() {
                    let run = &v[at..at + lanes];
                    if run
                        .iter()
                        .enumerate()
                        .any(|(k, x)| x.0 != run[0].0 + k as i128)
                    {
                        at += 1;
                        continue;
                    }
                    let stores: Vec<usize> = run.iter().map(|x| x.1).collect();
                    let ops: Vec<Operand> = stores
                        .iter()
                        .map(|&i| match &b.stmts[i] {
                            Statement::BufStore { value, .. } => value.clone(),
                            _ => unreachable!("a store"),
                        })
                        .collect();
                    let (buf, index) = match &b.stmts[run[0].1] {
                        Statement::BufStore { buf, index, .. } => (buf.clone(), index.clone()),
                        _ => unreachable!("a store"),
                    };
                    seeds.push(Group {
                        at: *stores.iter().max().expect("lanes"),
                        p: key.1,
                        w,
                        parts: vec![(ops, Seed::Store { buf, index })],
                        stores,
                    });
                    at += lanes;
                }
            }
            seeds.sort_by_key(|g| g.at);
            for g in seeds {
                if g.stores.iter().any(|i| taken.contains(&(pos, *i))) {
                    continue;
                }
                let mut held = taken.clone();
                let mut packs = Vec::new();
                let mut claims = Vec::new();
                for (ops, seed) in &g.parts {
                    let mut bl = Block {
                        facts: &facts,
                        stmts: &b.stmts,
                        pos,
                        p: g.p,
                        w: g.w,
                        isa,
                        checks: self.c.overflow_checks,
                        taken: &held,
                        claim: Vec::new(),
                        nodes: Vec::new(),
                    };
                    if bl.node(ops, g.at).is_none() {
                        break;
                    }
                    let reads = node_reads(&bl.nodes, seed);
                    if !bl.sound(g.at, &g.stores, &reads, deaths) {
                        break;
                    }
                    let (claim, nodes) = (bl.claim, bl.nodes);
                    held.extend(claim.iter().map(|i| (pos, *i)));
                    claims.extend(claim);
                    packs.push(Pack {
                        w: g.w,
                        vb,
                        p: g.p,
                        nodes,
                        seed: seed.clone(),
                    });
                }
                if packs.len() != g.parts.len() {
                    continue;
                }
                for i in claims.iter().chain(&g.stores) {
                    taken.insert((pos, *i));
                    if *i != g.at {
                        out.entry((pos, *i)).or_default();
                    }
                }
                taken.insert((pos, g.at));
                out.entry((pos, g.at)).or_default().extend(packs);
            }
        }
        out
    }

    /// The vector code of `pk`, at its seed.
    pub(crate) fn pack(&mut self, pk: &Pack) {
        let t = self.c.t;
        let ty = MirTy::Prim(pk.p);
        let k = cc::scalar_k(&ty, t).unwrap_or(K::I64);
        let (w, vb) = (pk.w, pk.vb);
        let last = pk.nodes.len() - 1;
        // The result's place: the struct's bytes, or the elements.
        let out = match &pk.seed {
            Seed::Init { target, off } => self.local_mem(*target).map(|m| m.at(*off)),
            Seed::Store { buf, index } => {
                let (_, p) = self.buf_elem(buf, index, &ty);
                Some(Mem::Ind(p, HEADER_SIZE as i32))
            }
        };
        let Some(out) = out else { return };
        let mut at: Vec<Mem> = Vec::with_capacity(pk.nodes.len());
        let scalar = |l: &mut Self, o: &Operand| match l.val(o, &ty) {
            Val::S(V::M(m), _) => m,
            Val::S(v, _) => {
                let m = l.tmp_k(k);
                l.push(Inst::Mov { k, dst: m, src: v });
                m
            }
            _ => l.tmp_k(k),
        };
        for (n, node) in pk.nodes.iter().enumerate() {
            let dst = if n == last && matches!(node, Node::Op { .. }) {
                out
            } else {
                Mem::Frame(self.out.slot(vb, 16), 0)
            };
            let vx = |op, a, b| {
                Inst::V(Box::new(Vx {
                    op,
                    w,
                    vb,
                    dst,
                    a,
                    b,
                }))
            };
            match node {
                Node::Load { buf, index } => {
                    let (_, p) = self.buf_elem(buf, index, &ty);
                    let a = Mem::Ind(p, HEADER_SIZE as i32);
                    self.push(vx(VxOp::Mov, a, a));
                }
                Node::Splat(o) => {
                    let m = scalar(self, o);
                    self.push(vx(VxOp::Splat(k), m, m));
                }
                Node::Build(ops) => {
                    for (i, o) in ops.iter().enumerate() {
                        let m = scalar(self, o);
                        let op = if i == 0 {
                            VxOp::Splat(k)
                        } else {
                            VxOp::Insert(k, i as u8)
                        };
                        self.push(vx(op, if i == 0 { m } else { dst }, m));
                    }
                }
                Node::Op { op, a, b } => {
                    let (a, b) = (at[*a], at[*b]);
                    self.push(vx(VxOp::Bin(*op), a, b));
                }
            }
            at.push(dst);
        }
        if !matches!(pk.nodes[last], Node::Op { .. }) {
            let r = at[last];
            self.push(Inst::V(Box::new(Vx {
                op: VxOp::Mov,
                w,
                vb,
                dst: out,
                a: r,
                b: r,
            })));
        }
    }
}
