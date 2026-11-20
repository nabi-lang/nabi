//! Optimizations over finished MIR, for `-O1` and up (`-Os` / `-Oz`
//! tuned for size). `-O0` never comes here.
//!
//! Every pass keeps MIR meaning what it meant: a value, a trap (overflow,
//! division, bounds), a side effect, in the same order. What codegen
//! derives from MIR (GC roots, polls, quiet functions, deaths) it derives
//! again from the result, so a pass only has to keep MIR's own rules:
//!
//! - a local whose address is taken (`addressed`) lives in memory: no
//!   value of it is ever forwarded or its stores dropped;
//! - after `StorageDead(l)` the slot of `l` may be cleared: no read of
//!   `l` is ever moved past one;
//! - a function holding `bare::ptr` values (`pins`) is left alone;
//! - `blocks[0]` is the entry, block ids are names (not indices), and a
//!   header in `strips` keeps its id.

// Value numbering names operands and blocks tersely.
#![allow(clippy::many_single_char_names)]

mod alias;
mod cfg;
mod counted;
mod escape;
mod fill;
mod inline;
mod licm;
mod memo;
mod range;
mod rebuild;
mod simplify;
mod sroa;
mod tables;
mod tail;
mod thread;
mod unroll;

pub use crate::opt::memo::Memo;

use crate::{
    BasicBlock, LocalDecl, MirFunction, MirModule, MirTy, Operand, RValue, Statement, Terminator,
};
use nabic_core::{HashMap, HashSet};
use nabic_ir_common::{BlockId, ConstVal, FunctionId, Linkage, LocalId, PrimOp, PrimTy, Summary};

/// What to optimize for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Opt {
    /// `-O1` .. `-O3`: the level.
    Speed(u8),
    /// `-Os` / `-Oz`: no growth beyond what a call costs.
    Size,
}

/// Optimize every function of `m` with a body.
///
/// Those found to write nothing their callers read get
/// `Summary::READONLY` (`alias::readonly`), the runtime's declarations
/// their summaries (`summary::runtime`).
pub fn optimize(m: &mut MirModule, opt: Opt) {
    optimize_with(m, opt, None);
}

/// `optimize`, taking the results of unchanged bodies from `memo`.
pub fn optimize_with(m: &mut MirModule, opt: Opt, memo: Option<&dyn Memo>) {
    let pw = m.target.pointer_width;
    let t = m.target.clone();
    if matches!(opt, Opt::Speed(_)) {
        tables::run(m);
    }
    crate::summary::set_runtime(m);
    let ro = &alias::readonly(m);
    for f in &mut m.functions {
        if ro.contains(&f.id) {
            f.facts.summary.flags |= Summary::READONLY;
        }
    }
    let bounds = range::bounds(m);
    let steps = memo.map(|c| memo::Steps::new(c, m, opt));
    // Aggregates split, then cleanup: what inlining measures and
    // copies. Whether a function is at its fixed point.
    let prep = |f: &mut MirFunction| {
        let go = |f: &mut MirFunction| {
            sroa::run(f, &t);
            let done = clean(f, pw, ro);
            // Fill loops, rotated by cleanup, become literals.
            if fill::run(f) { clean(f, pw, ro) } else { done }
        };
        match &steps {
            Some(s) => s.run(b'p', f, false, &memo::facts(f, &[ro]), go),
            None => go(f),
        }
    };
    let stable: Vec<bool> = each(&mut m.functions, |_, f| eligible(f) && prep(f));
    let grown = inline::inline(m, opt, &prep);
    // Functions without a body here.
    let decls: HashSet<FunctionId> = m
        .functions
        .iter()
        .filter(|f| f.blocks.is_empty())
        .map(|f| f.id)
        .collect();
    // The runtime's calls that reach no safepoint: a loop of them may
    // be stripped.
    let nosp: HashSet<FunctionId> = m
        .functions
        .iter()
        .filter(|f| f.facts.linkage == Linkage::Extern && f.facts.no_safepoint)
        .map(|f| f.id)
        .collect();
    let local = |f: &mut MirFunction, mut stable: bool| -> bool {
        if tail::self_calls(f) {
            prep(f);
            stable = false;
        }
        // Threading leaves copies of what the edges knew: cleanup folds
        // them, and the sums an inlined `next()` built split. One round
        // where nothing else asked for cleanup: the copies need no more.
        let threaded = thread::run(f, pw);
        if threaded {
            sroa::run(f, &t);
        }
        if !stable {
            clean(f, pw, ro);
        } else if threaded {
            simplify::run(f, pw, ro);
            cfg::run(f);
        }
        if range::run(f, pw, &bounds) {
            clean(f, pw, ro);
        }
        // Loops over `[T; N]` cleanup unrolled index the arrays at
        // constants now: those split, their elements are values the
        // ranges see.
        if short_arrays(f) && sroa::run(f, &t) {
            clean(f, pw, ro);
            if range::run(f, pw, &bounds) {
                clean(f, pw, ro);
            }
        }
        // Its bounds checks gone, a loop may poll no more.
        if crate::strip::strip_quiet(f, &nosp) {
            clean(f, pw, ro);
        }
        if licm::hoist(f, pw, ro) {
            clean(f, pw, ro);
        }
        // What the ranges and hoisting folded may index short arrays at
        // constants only now.
        if short_arrays(f) && sroa::run(f, &t) {
            clean(f, pw, ro);
            if range::run(f, pw, &bounds) {
                clean(f, pw, ro);
            }
            if licm::hoist(f, pw, ro) {
                clean(f, pw, ro);
            }
        }
        if threaded && counted::run(f, &nosp) {
            clean(f, pw, ro);
        }
        // Whole strips of scalar loops, unrolled.
        if opt == Opt::Speed(3) && unroll::run(f, pw, &decls) {
            clean(f, pw, ro);
        }
        if simplify::pair(f, pw) {
            clean(f, pw, ro);
        }
        // Last: nothing takes these structs apart again.
        if rebuild::run(f, &t) {
            clean(f, pw, ro);
        }
        false
    };
    each(&mut m.functions, |i, f| {
        if !eligible(f) {
            return;
        }
        let stable = grown[i].unwrap_or(stable[i]);
        match &steps {
            Some(s) => {
                let facts = memo::facts(f, &[ro, &nosp, &decls]);
                s.run(b'l', f, stable, &facts, |f| local(f, stable))
            }
            None => local(f, stable),
        };
    });
    crate::summary::set_echoes(m);
}

/// `job(i, f)` of each function `f` (index `i`) of `fs`, on a few
/// threads: a job reads only its own function and what the closure
/// shares, so the result is the same in any order.
pub(crate) fn each<T: Send>(
    fs: &mut [MirFunction],
    job: impl Fn(usize, &mut MirFunction) -> T + Sync,
) -> Vec<T> {
    // Below this many functions threads cost more than they save.
    const MIN: usize = 64;
    const CHUNK: usize = 4;
    // More threads gain little here and cost the other jobs of a busy
    // machine.
    const THREADS: usize = 4;
    let cores = std::thread::available_parallelism()
        .map_or(1, usize::from)
        .min(THREADS);
    let n = fs.len();
    if n < MIN || cores < 2 {
        return fs.iter_mut().enumerate().map(|(i, f)| job(i, f)).collect();
    }
    // A few at a time, each taken by one thread: no lock is waited on.
    let chunks: Vec<std::sync::Mutex<&mut [MirFunction]>> =
        fs.chunks_mut(CHUNK).map(std::sync::Mutex::new).collect();
    let next = std::sync::atomic::AtomicUsize::new(0);
    let mut done: Vec<(usize, T)> = std::thread::scope(|s| {
        let mut workers = Vec::new();
        for _ in 0..cores.min(n / (MIN / 2)) {
            let w = std::thread::Builder::new()
                // Passes recurse over the MIR as the compile thread does.
                .stack_size(256 << 20)
                .spawn_scoped(s, || {
                    let mut out = Vec::new();
                    loop {
                        let c = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(chunk) = chunks.get(c) else { break };
                        let mut fs = chunk
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        for (k, f) in fs.iter_mut().enumerate() {
                            let i = c * CHUNK + k;
                            out.push((i, job(i, f)));
                        }
                    }
                    out
                })
                .expect("start an optimizer thread");
            workers.push(w);
        }
        workers
            .into_iter()
            .flat_map(|w| w.join().unwrap_or_else(|e| std::panic::resume_unwind(e)))
            .collect()
    });
    done.sort_unstable_by_key(|(i, _)| *i);
    done.into_iter().map(|(_, r)| r).collect()
}

/// `f` has a local of a short array type, which `sroa` may split.
fn short_arrays(f: &MirFunction) -> bool {
    f.locals
        .iter()
        .any(|d| matches!(d.ty, MirTy::Array(_, n) if n <= 16))
}

/// A function the passes may change.
fn eligible(f: &MirFunction) -> bool {
    !f.blocks.is_empty() && f.pins.is_empty()
}

/// Value and CFG cleanup to a fixed point (a few rounds at most);
/// whether it got there. `ro`: `alias::readonly`.
fn clean(f: &mut MirFunction, pw: u32, ro: &HashSet<FunctionId>) -> bool {
    for _ in 0..4 {
        let a = escape::run(f, pw) | simplify::run(f, pw, ro);
        let b = cfg::run(f);
        if !a && !b {
            return true;
        }
    }
    false
}

/// Each operand `r` reads.
pub fn rvalue_ops_mut(r: &mut RValue, f: &mut impl FnMut(&mut Operand)) {
    match r {
        RValue::Use(o)
        | RValue::UnaryOp { operand: o, .. }
        | RValue::FieldRead { target: o, .. }
        | RValue::Cast { source: o, .. }
        | RValue::Bitcast { source: o, .. }
        | RValue::HeapBox { value: o, .. }
        | RValue::BufAlloc { len: o, .. }
        | RValue::RawLoad { addr: o, .. }
        | RValue::HeapLoad { ptr: o, .. }
        | RValue::HeapField { ptr: o, .. } => f(o),
        RValue::BinOp { lhs, rhs, .. } => {
            f(lhs);
            f(rhs);
        }
        RValue::Call { args, .. }
        | RValue::StructInit { fields: args, .. }
        | RValue::ArrayInit { elements: args, .. } => args.iter_mut().for_each(f),
        RValue::WithField { source, value, .. } => {
            f(source);
            f(value);
        }
        RValue::ArrayIndex { index, .. } => f(index),
        RValue::BufLoad { buf, index, .. } => {
            f(buf);
            f(index);
        }
        RValue::BufAtomic {
            buf, index, args, ..
        } => {
            f(buf);
            f(index);
            args.iter_mut().for_each(f);
        }
        RValue::CallIndirect { callee, args, .. } => {
            f(callee);
            args.iter_mut().for_each(f);
        }
        RValue::AddrOf(_)
        | RValue::FnAddr { .. }
        | RValue::StaticLoad { .. }
        | RValue::StaticAddr(_)
        | RValue::SizeOf { .. }
        | RValue::HeapAlloc { .. } => {}
    }
}

/// Each operand `r` reads, by reference (not `rvalue_ops_mut`'s locals
/// named by id).
pub fn rvalue_ops(r: &RValue, f: &mut impl FnMut(&Operand)) {
    match r {
        RValue::Use(o)
        | RValue::UnaryOp { operand: o, .. }
        | RValue::FieldRead { target: o, .. }
        | RValue::Cast { source: o, .. }
        | RValue::Bitcast { source: o, .. }
        | RValue::HeapBox { value: o, .. }
        | RValue::BufAlloc { len: o, .. }
        | RValue::RawLoad { addr: o, .. }
        | RValue::HeapLoad { ptr: o, .. }
        | RValue::HeapField { ptr: o, .. }
        | RValue::ArrayIndex { index: o, .. } => f(o),
        RValue::BinOp { lhs: a, rhs: b, .. }
        | RValue::WithField {
            source: a,
            value: b,
            ..
        }
        | RValue::BufLoad {
            buf: a, index: b, ..
        } => {
            f(a);
            f(b);
        }
        RValue::Call { args, .. }
        | RValue::StructInit { fields: args, .. }
        | RValue::ArrayInit { elements: args, .. } => args.iter().for_each(f),
        RValue::BufAtomic {
            buf, index, args, ..
        } => {
            f(buf);
            f(index);
            args.iter().for_each(f);
        }
        RValue::CallIndirect { callee, args, .. } => {
            f(callee);
            args.iter().for_each(f);
        }
        RValue::AddrOf(_)
        | RValue::FnAddr { .. }
        | RValue::StaticLoad { .. }
        | RValue::StaticAddr(_)
        | RValue::SizeOf { .. }
        | RValue::HeapAlloc { .. } => {}
    }
}

/// Each operand `s` reads (not the array local of an `ArrayWrite`).
pub fn stmt_ops_mut(s: &mut Statement, f: &mut impl FnMut(&mut Operand)) {
    match s {
        Statement::Assign { rvalue, .. } => rvalue_ops_mut(rvalue, f),
        Statement::ArrayWrite { index, value, .. } => {
            f(index);
            f(value);
        }
        Statement::BufStore {
            buf, index, value, ..
        } => {
            f(buf);
            f(index);
            f(value);
        }
        Statement::RawStore { addr, value, .. } => {
            f(addr);
            f(value);
        }
        Statement::HeapStore { ptr, value, .. } => {
            f(ptr);
            f(value);
        }
        Statement::StaticStore { value, .. } => f(value),
        Statement::StorageDead(_) | Statement::Loc(..) => {}
    }
}

/// Each operand `t` reads.
pub fn term_ops_mut(t: &mut Terminator, f: &mut impl FnMut(&mut Operand)) {
    match t {
        Terminator::Goto { args, .. } => args.iter_mut().for_each(f),
        Terminator::If {
            cond,
            then_args,
            else_args,
            ..
        } => {
            f(cond);
            then_args.iter_mut().for_each(&mut *f);
            else_args.iter_mut().for_each(f);
        }
        Terminator::Return(Some(o)) | Terminator::Suspend { value: Some(o), .. } => f(o),
        Terminator::Return(None)
        | Terminator::Suspend { value: None, .. }
        | Terminator::Unreachable => {}
    }
}

/// The successor ids of `t`, one per edge.
pub fn succs(t: &Terminator) -> Vec<BlockId> {
    match t {
        Terminator::Goto { target, .. } => vec![*target],
        Terminator::If {
            then_bb, else_bb, ..
        } => vec![*then_bb, *else_bb],
        Terminator::Suspend { resume, .. } => vec![*resume],
        Terminator::Return(_) | Terminator::Unreachable => Vec::new(),
    }
}

pub fn succs_mut(t: &mut Terminator) -> Vec<&mut BlockId> {
    match t {
        Terminator::Goto { target, .. } => vec![target],
        Terminator::If {
            then_bb, else_bb, ..
        } => vec![then_bb, else_bb],
        Terminator::Suspend { resume, .. } => vec![resume],
        Terminator::Return(_) | Terminator::Unreachable => Vec::new(),
    }
}

/// Block index by id.
pub fn index(f: &MirFunction) -> HashMap<u32, usize> {
    f.blocks
        .iter()
        .enumerate()
        .map(|(i, b)| (b.id.0, i))
        .collect()
}

/// Incoming edges per block index (an `If` to one block twice counts
/// twice).
pub fn preds(f: &MirFunction, idx: &HashMap<u32, usize>) -> Vec<Vec<usize>> {
    let mut p = vec![Vec::new(); f.blocks.len()];
    for (i, b) in f.blocks.iter().enumerate() {
        for s in succs(&b.terminator) {
            if let Some(&j) = idx.get(&s.0) {
                p[j].push(i);
            }
        }
    }
    p
}

/// Block indices in reverse post-order from the entry (reachable only).
pub fn rpo(f: &MirFunction, idx: &HashMap<u32, usize>) -> Vec<usize> {
    let n = f.blocks.len();
    let mut seen = vec![false; n];
    let mut post = Vec::with_capacity(n);
    if n == 0 {
        return post;
    }
    let mut stack: Vec<(usize, usize)> = vec![(0, 0)];
    seen[0] = true;
    while let Some(top) = stack.last_mut() {
        let (b, k) = *top;
        let ss = succs(&f.blocks[b].terminator);
        if let Some(s) = ss.get(k) {
            top.1 += 1;
            if let Some(&j) = idx.get(&s.0) {
                if !seen[j] {
                    seen[j] = true;
                    stack.push((j, 0));
                }
            }
            continue;
        }
        post.push(b);
        stack.pop();
    }
    post.reverse();
    post
}

/// Whether some edge goes back to a block no later in reverse
/// post-order (`order`): the reachable part has a cycle.
pub fn cyclic(f: &MirFunction, idx: &HashMap<u32, usize>, order: &[usize]) -> bool {
    let mut at = vec![usize::MAX; f.blocks.len()];
    for (k, &b) in order.iter().enumerate() {
        at[b] = k;
    }
    order.iter().any(|&b| {
        succs(&f.blocks[b].terminator)
            .iter()
            .any(|s| idx.get(&s.0).is_some_and(|&j| at[j] <= at[b]))
    })
}

/// Immediate dominators by block index (`usize::MAX`: unreachable; the
/// entry is its own).
pub struct Dom {
    idom: Vec<usize>,
    order: Vec<usize>,
}

impl Dom {
    pub fn new(f: &MirFunction, idx: &HashMap<u32, usize>, preds: &[Vec<usize>]) -> Self {
        let n = f.blocks.len();
        let rpo = rpo(f, idx);
        let mut order = vec![usize::MAX; n];
        for (k, &b) in rpo.iter().enumerate() {
            order[b] = k;
        }
        let mut idom = vec![usize::MAX; n];
        if n == 0 {
            return Self { idom, order };
        }
        idom[0] = 0;
        let mut changed = true;
        while changed {
            changed = false;
            for &b in rpo.iter().skip(1) {
                let mut new = usize::MAX;
                for &p in &preds[b] {
                    if idom[p] == usize::MAX {
                        continue;
                    }
                    new = if new == usize::MAX {
                        p
                    } else {
                        let (mut x, mut y) = (p, new);
                        while x != y {
                            while order[x] > order[y] {
                                x = idom[x];
                            }
                            while order[y] > order[x] {
                                y = idom[y];
                            }
                        }
                        x
                    };
                }
                if new != usize::MAX && idom[b] != new {
                    idom[b] = new;
                    changed = true;
                }
            }
        }
        Self { idom, order }
    }

    /// The immediate dominator of reachable block `b`.
    pub fn idom(&self, b: usize) -> usize {
        self.idom[b]
    }

    pub fn reachable(&self, b: usize) -> bool {
        self.idom[b] != usize::MAX
    }

    /// Block `a` dominates block `b` (both reachable).
    pub fn dominates(&self, a: usize, mut b: usize) -> bool {
        if !self.reachable(a) || !self.reachable(b) {
            return false;
        }
        loop {
            if a == b {
                return true;
            }
            if b == 0 || self.order[b] < self.order[a] {
                return false;
            }
            b = self.idom[b];
        }
    }
}

/// Reads of each local (`liveness::stmt_uses` / `term_uses`).
pub fn use_counts(f: &MirFunction) -> Vec<u32> {
    let mut uses = vec![0u32; f.locals.len()];
    for b in &f.blocks {
        for s in &b.stmts {
            crate::stmt_reads(s, &mut |u| uses[u.0 as usize] += 1);
        }
        crate::term_reads(&b.terminator, &mut |u| uses[u.0 as usize] += 1);
    }
    uses
}

/// Locals no value of which may be forwarded: taken by address, written
/// in part (`ArrayWrite`), or block / function parameters.
pub fn pinned(f: &MirFunction) -> HashSet<LocalId> {
    let mut out = crate::addressed(f);
    for s in f.blocks.iter().flat_map(|b| &b.stmts) {
        if let Statement::ArrayWrite { array, .. } = s {
            out.insert(*array);
        }
    }
    out
}

pub fn fresh_block(f: &MirFunction) -> BlockId {
    BlockId(f.blocks.iter().map(|b| b.id.0 + 1).max().unwrap_or(0))
}

pub fn fresh_local(f: &mut MirFunction, ty: MirTy) -> LocalId {
    let id = LocalId(f.locals.len() as u32);
    f.locals.push(LocalDecl { id, name: None, ty });
    id
}

pub const fn empty_block(id: BlockId, terminator: Terminator) -> BasicBlock {
    BasicBlock {
        id,
        params: Vec::new(),
        stmts: Vec::new(),
        terminator,
    }
}

/// Whether `o` is a `t` (the types of locals in `locals`).
pub fn has_ty(locals: &[LocalDecl], o: &Operand, t: &MirTy) -> bool {
    match o {
        Operand::Copy(l) => locals.get(l.0 as usize).is_some_and(|d| d.ty == *t),
        Operand::Const(c) => match c {
            ConstVal::Int { ty, .. } | ConstVal::UInt { ty, .. } | ConstVal::Float { ty, .. } => {
                *t == MirTy::Prim(*ty)
            }
            ConstVal::Bool(_) => *t == MirTy::Prim(PrimTy::Bool),
            ConstVal::Char(_) => *t == MirTy::Prim(PrimTy::Char),
            ConstVal::Str(_) => *t == MirTy::Prim(PrimTy::Str),
            ConstVal::Unit => *t == MirTy::Unit,
            ConstVal::None | ConstVal::Bytes(_) => false,
        },
    }
}

/// An integer type's value range.
pub const fn int_range(p: PrimTy, pw: u32) -> Option<(i128, i128)> {
    let bits = p.bit_width(pw);
    if p.is_signed_int() {
        Some((-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1))
    } else if p.is_unsigned_int() {
        Some((0, (1i128 << bits) - 1))
    } else {
        None
    }
}

/// The value of integer constant `c` read as a `p`.
pub fn int_val(c: &ConstVal, p: PrimTy, pw: u32) -> Option<i128> {
    let (lo, hi) = int_range(p, pw)?;
    let raw = match c {
        ConstVal::Int { value, ty } if *ty == p => *value,
        ConstVal::UInt { value, ty } if *ty == p => i128::try_from(*value).ok()?,
        _ => return None,
    };
    let bits = p.bit_width(pw);
    // Bits as stored, read back as the type says.
    let m = raw & ((1i128 << bits) - 1);
    let v = if lo < 0 && m > hi {
        m - (1i128 << bits)
    } else {
        m
    };
    (lo..=hi).contains(&v).then_some(v)
}

/// Integer constant `v` (in range) of type `p`.
pub const fn int_const(v: i128, p: PrimTy) -> ConstVal {
    if p.is_signed_int() {
        ConstVal::Int { value: v, ty: p }
    } else {
        ConstVal::UInt {
            value: v as u128,
            ty: p,
        }
    }
}

/// Integer arithmetic that traps or reads memory never runs early or
/// twice: `rv` is a value of its operands alone and cannot fail.
pub fn pure(rv: &RValue, pw: u32) -> bool {
    match rv {
        RValue::Use(_)
        | RValue::StructInit { .. }
        | RValue::FieldRead { .. }
        | RValue::Cast { .. }
        | RValue::Bitcast { .. }
        | RValue::WithField { .. }
        | RValue::ArrayInit { .. }
        | RValue::FnAddr { .. }
        | RValue::SizeOf { .. } => true,
        RValue::BinOp {
            op,
            op_ty,
            lhs,
            rhs,
        } => bin_cannot_fail(*op, op_ty, lhs, rhs, pw),
        RValue::UnaryOp { op, op_ty, operand } => match op_ty {
            MirTy::Prim(p) if *op == PrimOp::Neg && p.is_signed_int() => match operand {
                Operand::Const(c) => int_val(c, *p, pw)
                    .zip(int_range(*p, pw))
                    .is_some_and(|(v, (lo, _))| v != lo),
                Operand::Copy(_) => false,
            },
            MirTy::Prim(_) => true,
            _ => false,
        },
        _ => false,
    }
}

/// `rv` may be dropped when its value is unused: pure, or a load from
/// a GC object.
pub fn removable(rv: &RValue, pw: u32) -> bool {
    pure(rv, pw)
        || matches!(
            rv,
            RValue::BufLoad { .. } | RValue::HeapField { .. } | RValue::HeapLoad { .. }
        )
        || matches!(rv, RValue::AddrOf(_))
        || alloc(rv, pw)
}

/// An allocation that cannot fail but for want of memory: unused, it
/// goes.
fn alloc(rv: &RValue, pw: u32) -> bool {
    match rv {
        RValue::HeapBox { .. } | RValue::HeapAlloc { .. } => true,
        RValue::BufAlloc {
            len: Operand::Const(c),
            ..
        } => int_val(c, PrimTy::Usize, pw).is_some_and(|n| (0..=1 << 16).contains(&n)),
        _ => false,
    }
}

/// A binary operation that cannot trap.
fn bin_cannot_fail(op: PrimOp, op_ty: &MirTy, lhs: &Operand, rhs: &Operand, pw: u32) -> bool {
    let MirTy::Prim(p) = op_ty else {
        return false;
    };
    let p = *p;
    if !p.is_int() {
        // Floats, bools, `str ==`.
        return true;
    }
    let c = |o: &Operand| match o {
        Operand::Const(c) => int_val(c, p, pw),
        Operand::Copy(_) => None,
    };
    let checked = p.is_signed_int() || p.is_unsigned_int();
    match op {
        PrimOp::Add | PrimOp::Sub | PrimOp::Mul if checked => {
            let (a, b) = (c(lhs), c(rhs));
            let Some((lo, hi)) = int_range(p, pw) else {
                return false;
            };
            match (a, b) {
                (Some(a), Some(b)) => {
                    let v = match op {
                        PrimOp::Add => a.checked_add(b),
                        PrimOp::Sub => a.checked_sub(b),
                        _ => a.checked_mul(b),
                    };
                    v.is_some_and(|v| (lo..=hi).contains(&v))
                }
                _ => match op {
                    PrimOp::Add => a == Some(0) || b == Some(0),
                    PrimOp::Sub => b == Some(0),
                    _ => matches!(a, Some(0 | 1)) || matches!(b, Some(0 | 1)),
                },
            }
        }
        PrimOp::Div | PrimOp::Rem | PrimOp::Mod => {
            c(rhs).is_some_and(|d| d != 0 && !(p.is_signed_int() && d == -1))
        }
        PrimOp::Shl | PrimOp::Shr => {
            c(rhs).is_some_and(|s| (0..i128::from(p.bit_width(pw))).contains(&s))
        }
        _ => true,
    }
}

/// Statements of no effect on what runs: debug positions and scope ends.
pub const fn marker(s: &Statement) -> bool {
    matches!(s, Statement::Loc(..) | Statement::StorageDead(_))
}

/// A function's size for inlining: statements that do something, plus
/// one per block. A copy costs nothing: a register move at most, and
/// most go once the copy is in its caller.
pub fn size(f: &MirFunction) -> usize {
    let free = |s: &Statement| {
        marker(s)
            || matches!(
                s,
                Statement::Assign {
                    rvalue: RValue::Use(_),
                    ..
                }
            )
    };
    f.blocks
        .iter()
        .map(|b| 1 + b.stmts.iter().filter(|s| !free(s)).count())
        .sum()
}
