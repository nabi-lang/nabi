//! Inlining. A direct call to a small function (or one called from one
//! place, from a few in a loop, or marked `#[inline]`) becomes a copy of
//! its body: parameters assigned from the arguments, each `return` an
//! assignment of the result and a jump to the rest of the caller's
//! block. A call in a loop takes bigger bodies than one outside any, and
//! the calls of a body copied into a loop are weighed again as calls in
//! that loop.
//!
//! Callees go first (bottom-up over the call graph), so what is inlined
//! has its own calls inlined already, and is cleaned up (`post`) before
//! its size is taken; a call cycle is never inlined into itself.
//! `#[inline(never)]` and `#[cold]` functions, calls on a path that
//! ends in a panic, a library's `any` dispatchers (the program's
//! replaces them at link time) and non-nabi conventions stay calls. Only
//! bodies this module has are inlined: its own (from any of the
//! package's modules: a codegen unit's key covers the optimized bodies
//! it holds, so an edit of a callee recompiles every unit it went into),
//! generic instances and the inline copies of a prebuilt dependency's
//! functions.
//!
//! The copy runs in the caller's frame: its locals are the caller's, so
//! codegen roots them where a collection can see them, as any other.

use crate::opt::Opt;
use crate::{
    BasicBlock, LocalDecl, MirFunction, MirModule, MirTy, Operand, RValue, Scope, Statement,
    Terminator,
};
use nabic_core::{HashMap, HashSet};
use nabic_ir_common::{Abi, BlockId, ConstVal, FnRole, FunctionId, Inline, Linkage, LocalId};

/// Call sites a body may have and still be copied into a loop as one
/// called from one place.
const FEW_SITES: usize = 3;

/// Size limits, in `opt::size` units: at a call in a loop, and
/// (`cold`) at one outside any.
struct Limits {
    /// Inlined anywhere.
    small: usize,
    /// `#[inline]`.
    hint: usize,
    /// Called from one place only.
    single: usize,
    /// In a program, called in a loop from at most `FEW_SITES` places.
    few: usize,
    small_cold: usize,
    hint_cold: usize,
    single_cold: usize,
    /// A caller grows to at most this.
    caller: usize,
}

/// Calls in loops take bodies as `-O2` does at every speed level (what
/// makes an iterator's `next()` part of its loop); `-O1` grows the code
/// around them little, `-O3` more only in loops. `-Os` inlines tiny
/// bodies, small ones in loops.
const fn limits(opt: Opt) -> Limits {
    match opt {
        Opt::Speed(1) => Limits {
            small: 25,
            hint: 100,
            single: 60,
            few: 60,
            small_cold: 12,
            hint_cold: 40,
            single_cold: 60,
            caller: 2000,
        },
        Opt::Speed(2) => Limits {
            small: 25,
            hint: 100,
            single: 100,
            few: 200,
            small_cold: 12,
            hint_cold: 100,
            single_cold: 100,
            caller: 4000,
        },
        Opt::Speed(_) => Limits {
            small: 45,
            hint: 150,
            single: 400,
            few: 400,
            small_cold: 12,
            hint_cold: 100,
            single_cold: 200,
            caller: 6000,
        },
        // About the size of the call it replaces; in a loop a few times
        // that.
        Opt::Size => Limits {
            small: 12,
            hint: 16,
            single: 60,
            few: 60,
            small_cold: 4,
            hint_cold: 8,
            single_cold: 60,
            caller: 2000,
        },
    }
}

/// What inlining `f` adds where it is called, in `opt::size` units: a
/// way to a panic is its one call, a field of an argument or a cast
/// between integers of one width a register at most. A checked index is
/// then about the call it replaces.
fn cost(f: &MirFunction, pw: u32) -> usize {
    let free = |s: &Statement| match s {
        Statement::Assign { rvalue, .. } => match rvalue {
            RValue::Use(_) | RValue::FieldRead { .. } => true,
            RValue::Cast {
                from_ty: MirTy::Prim(a),
                to_ty: MirTy::Prim(b),
                ..
            } => a.is_int() && b.is_int() && a.bit_width(pw) == b.bit_width(pw),
            _ => false,
        },
        s => crate::opt::marker(s),
    };
    f.blocks
        .iter()
        .map(|b| {
            if matches!(b.terminator, Terminator::Unreachable) {
                1
            } else {
                1 + b.stmts.iter().filter(|s| !free(s)).count()
            }
        })
        .sum()
}

/// Ids of the blocks of `f` in a loop.
fn loop_blocks(f: &MirFunction) -> HashSet<u32> {
    let mut out = HashSet::default();
    let edges = crate::rotate::back_edges(f);
    if edges.is_empty() {
        return out;
    }
    let preds = crate::rotate::preds(f);
    for (latch, hs) in edges {
        for h in hs {
            out.extend(crate::rotate::body_of(&preds, h, &[latch]));
        }
    }
    out
}

/// A function whose calls may be replaced by its body. A program's
/// `any` dispatcher is the complete one; a library's is not (`library`).
fn has_body(f: &MirFunction, library: bool) -> bool {
    crate::opt::eligible(f)
        && matches!(f.facts.linkage, Linkage::Defined | Linkage::Prebuilt)
        && f.facts.abi == Abi::Nabi
        && (f.facts.role == FnRole::Plain
            || (f.facts.role == FnRole::AnyDispatcher
                && !library
                && f.facts.linkage == Linkage::Defined))
        && f.facts.wasm_import.is_none()
        && !matches!(f.facts.inline, Inline::Never | Inline::Cold)
        && !f
            .blocks
            .iter()
            .any(|b| matches!(b.terminator, Terminator::Suspend { .. }))
}

/// Whether codegen may emit a panic of its own in `f` (an overflow,
/// division or bounds check). A `#[track_caller]` function reports those
/// at its caller's position, any other at its own: such a body never
/// moves into or out of a `#[track_caller]` function. An array index the
/// ranges keep in bounds (`for k in 0..N` over `[T; N]`) checks nothing.
fn checks(f: &MirFunction, pw: u32) -> bool {
    let mut indexed = false;
    let arith = f.blocks.iter().flat_map(|b| &b.stmts).any(|s| match s {
        Statement::Assign { rvalue, .. } => match rvalue {
            RValue::BinOp { .. } | RValue::UnaryOp { .. } => !crate::opt::pure(rvalue, pw),
            RValue::ArrayIndex { index, len, .. } => {
                indexed |= !in_range(index, *len);
                false
            }
            _ => false,
        },
        Statement::ArrayWrite { index, len, .. } => {
            indexed |= !in_range(index, *len);
            false
        }
        _ => false,
    });
    arith || (indexed && !crate::opt::range::indexes_fit(f, pw))
}

/// A constant index below `len`: codegen checks nothing.
fn in_range(index: &Operand, len: u32) -> bool {
    let v = match index {
        Operand::Const(ConstVal::Int { value, .. }) => Some(*value),
        Operand::Const(ConstVal::UInt { value, .. }) => i128::try_from(*value).ok(),
        _ => None,
    };
    v.is_some_and(|v| (0..i128::from(len)).contains(&v))
}

/// The direct callees of `f`, once each.
fn callees(f: &MirFunction) -> Vec<FunctionId> {
    let mut out = Vec::new();
    let mut seen = HashSet::default();
    for s in f.blocks.iter().flat_map(|b| &b.stmts) {
        if let Statement::Assign {
            rvalue: RValue::Call { callee, .. },
            ..
        } = s
        {
            if seen.insert(*callee) {
                out.push(*callee);
            }
        }
    }
    out
}

/// Strongly connected components of the call graph (Tarjan), in the
/// order they complete: callees before callers. Component per function.
fn components(m: &MirModule, pos: &HashMap<FunctionId, usize>) -> (Vec<Vec<usize>>, Vec<usize>) {
    let n = m.functions.len();
    let succ: Vec<Vec<usize>> = m
        .functions
        .iter()
        .map(|f| {
            callees(f)
                .into_iter()
                .filter_map(|c| pos.get(&c).copied())
                .collect()
        })
        .collect();
    let (mut index, mut low) = (vec![usize::MAX; n], vec![0; n]);
    let (mut on, mut stack, mut next) = (vec![false; n], Vec::new(), 0);
    let mut comps: Vec<Vec<usize>> = Vec::new();
    let mut comp = vec![0; n];
    for root in 0..n {
        if index[root] != usize::MAX {
            continue;
        }
        let mut work: Vec<(usize, usize)> = vec![(root, 0)];
        while let Some(top) = work.last_mut() {
            let (v, k) = *top;
            if k == 0 {
                index[v] = next;
                low[v] = next;
                next += 1;
                stack.push(v);
                on[v] = true;
            }
            if let Some(&w) = succ[v].get(k) {
                top.1 += 1;
                if index[w] == usize::MAX {
                    work.push((w, 0));
                } else if on[w] {
                    low[v] = low[v].min(index[w]);
                }
                continue;
            }
            work.pop();
            if let Some(&(u, _)) = work.last() {
                low[u] = low[u].min(low[v]);
            }
            if low[v] == index[v] {
                let mut c = Vec::new();
                while let Some(w) = stack.pop() {
                    on[w] = false;
                    comp[w] = comps.len();
                    c.push(w);
                    if w == v {
                        break;
                    }
                }
                comps.push(c);
            }
        }
    }
    (comps, comp)
}

/// Inlines calls; per function into which any call was inlined, what
/// `post` (run on it once its callees are in, before its callers
/// measure it) returned.
pub fn inline(
    m: &mut MirModule,
    opt: Opt,
    post: &(dyn Fn(&mut MirFunction) -> bool + Sync),
) -> Vec<Option<bool>> {
    let lim = limits(opt);
    let grown = vec![None; m.functions.len()];
    let pos: HashMap<FunctionId, usize> = m
        .functions
        .iter()
        .enumerate()
        .map(|(i, f)| (f.id, i))
        .collect();
    // Direct call sites and address takings, before any inlining.
    let mut sites: HashMap<FunctionId, usize> = HashMap::default();
    let mut taken: HashSet<FunctionId> = HashSet::default();
    for s in m
        .functions
        .iter()
        .flat_map(|f| &f.blocks)
        .flat_map(|b| &b.stmts)
    {
        match s {
            Statement::Assign {
                rvalue: RValue::Call { callee, .. },
                ..
            } => *sites.entry(*callee).or_default() += 1,
            Statement::Assign {
                rvalue: RValue::FnAddr { callee, .. },
                ..
            } => {
                taken.insert(*callee);
            }
            _ => {}
        }
    }
    let library = m.link.lib.is_some();
    // A prebuilt library keeps the plain rules: its build is every
    // program's. A program built for speed, with the instances it makes,
    // takes the wider ones (bodies with a few call sites, calls weighed
    // again).
    let wide = !library && opt != Opt::Size;
    let pw = m.target.pointer_width;
    let (comps, comp) = components(m, &pos);
    let mut cyclic = vec![false; m.functions.len()];
    for c in &comps {
        let rec = c.len() > 1 || callees(&m.functions[c[0]]).contains(&m.functions[c[0]].id);
        for &i in c {
            cyclic[i] = rec;
        }
    }
    // Callees are done before their callers: `checks` holds once known.
    let checked: std::sync::Mutex<HashMap<FunctionId, bool>> = std::sync::Mutex::default();
    let grown = std::sync::Mutex::new(grown);
    // One function: the copies of its callees in it, and the `-g` types
    // of their locals.
    let one = |m: &MirModule,
               fi: usize,
               caller: &mut MirFunction|
     -> Vec<((FunctionId, LocalId), crate::DebugTy)> {
        let mut types = Vec::new();
        let recursive = cyclic[fi];
        let tracked = caller.facts.track_caller;
        let decide = |callee: &MirFunction, caller_size: usize, hot: bool| -> bool {
            if !has_body(callee, library) {
                return false;
            }
            let ci = pos[&callee.id];
            if comp[ci] == comp[fi] {
                return false;
            }
            if (tracked || callee.facts.track_caller) && {
                let known = checked
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get(&callee.id)
                    .copied();
                known.unwrap_or_else(|| {
                    let c = checks(callee, pw);
                    checked
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .insert(callee.id, c);
                    c
                })
            } {
                return false;
            }
            let size = cost(callee, pw);
            if caller_size + size > lim.caller {
                return callee.facts.inline == Inline::Always && size <= lim.single.max(lim.hint);
            }
            let cap = match callee.facts.inline {
                Inline::Always => usize::MAX,
                Inline::Hint if hot => lim.hint,
                Inline::Hint => lim.hint_cold,
                _ if hot => lim.small,
                _ => lim.small_cold,
            };
            if size <= cap {
                return true;
            }
            // A recursive caller keeps a small frame. In a loop a
            // body called from a few places counts as one called from
            // one: the copies are few, and the call would be paid on
            // every iteration (a sampler's draw in its loop).
            let size_lib = library && opt == Opt::Size;
            let calls = sites.get(&callee.id).copied().unwrap_or(0);
            let few = !(recursive
                || size_lib
                || calls == 0
                || calls > if hot && wide { FEW_SITES } else { 1 }
                || taken.contains(&callee.id))
                && callee.facts.export_name.is_none();
            let cap = match (hot, wide) {
                (true, true) => lim.few,
                (true, false) => lim.single,
                (false, _) => lim.single_cold,
            };
            few && size <= cap
        };
        let mut inlined = false;
        let mut size = crate::opt::size(caller);
        let mut hot = loop_blocks(caller);
        let mut bi = 0;
        let mut si = 0;
        while bi < caller.blocks.len() {
            let b = &caller.blocks[bi];
            // A call on the way to a panic stays a call.
            if si >= b.stmts.len() || matches!(b.terminator, Terminator::Unreachable) {
                bi += 1;
                si = 0;
                continue;
            }
            let Statement::Assign {
                rvalue: RValue::Call { callee, args, .. },
                ..
            } = &b.stmts[si]
            else {
                si += 1;
                continue;
            };
            let Some(&ci) = pos.get(callee) else {
                si += 1;
                continue;
            };
            let callee = &m.functions[ci];
            let in_loop = hot.contains(&b.id.0);
            if callee.params.len() != args.len() || !decide(callee, size, in_loop) {
                si += 1;
                continue;
            }
            size += crate::opt::size(callee);
            let n = callee.blocks.len();
            let base = caller.locals.len() as u32;
            splice(caller, bi, si, callee);
            if in_loop {
                hot.extend(caller.blocks[bi + 1..=bi + n + 1].iter().map(|b| b.id.0));
            }
            // The copies' `-g` types.
            for d in &callee.locals {
                if let Some(t) = m.debug.locals.get(&(callee.id, d.id)) {
                    types.push(((caller.id, LocalId(base + d.id.0)), t.clone()));
                }
            }
            inlined = true;
            // A small body copied into a loop has its calls weighed
            // again: one cold in the callee may be hot here
            // (`next_u64` of a `uniform` in a sampling loop). Else go
            // on after the copy, in the rest of the block.
            let again = wide && in_loop && !cyclic[ci] && crate::opt::size(callee) <= lim.small;
            bi += if again { 1 } else { n + 1 };
            si = 0;
        }
        if inlined {
            let done = post(caller);
            grown
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)[fi] = Some(done);
        }
        types
    };
    // A component's callees are in components before it: those of one
    // level (the longest chain of callee components below) are apart,
    // done side by side.
    let mut level = vec![0usize; comps.len()];
    let mut levels: Vec<Vec<usize>> = Vec::new();
    for (ci, c) in comps.iter().enumerate() {
        let l = c
            .iter()
            .flat_map(|&fi| callees(&m.functions[fi]))
            .filter_map(|id| pos.get(&id))
            .map(|&g| comp[g])
            .filter(|&g| g != ci)
            .map(|g| level[g] + 1)
            .max()
            .unwrap_or(0);
        level[ci] = l;
        if levels.len() <= l {
            levels.resize_with(l + 1, Vec::new);
        }
        levels[l].extend(
            c.iter()
                .copied()
                .filter(|&fi| crate::opt::eligible(&m.functions[fi])),
        );
    }
    for fis in levels {
        let mut callers: Vec<MirFunction> = fis
            .iter()
            .map(|&fi| std::mem::replace(&mut m.functions[fi], placeholder()))
            .collect();
        let types = crate::opt::each(&mut callers, |k, caller| one(m, fis[k], caller));
        for (&fi, caller) in fis.iter().zip(callers) {
            m.functions[fi] = caller;
        }
        m.debug.locals.extend(types.into_iter().flatten());
    }
    grown
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn placeholder() -> MirFunction {
    MirFunction {
        id: FunctionId(u32::MAX),
        def_id: None,
        name: String::new(),
        params: Vec::new(),
        locals: Vec::new(),
        blocks: Vec::new(),
        param_tys: Vec::new(),
        ret_ty: MirTy::Unit,
        span: nabic_diagnostic::Span::DUMMY,
        facts: nabic_ir_common::FnFacts::default(),
        strips: Vec::new(),
        pins: Vec::new(),
        dbg: crate::FnDbg::default(),
    }
}

/// Replaces the call at statement `si` of block `bi` of `caller` by a copy
/// of `callee`, placed right after the block; the rest of the block moves
/// to a new block after the copy. The copy's positions are in scopes of
/// their own under the call's (`Inlined`), its locals keep their names.
fn splice(caller: &mut MirFunction, bi: usize, si: usize, callee: &MirFunction) {
    let base = caller.locals.len() as u32;
    let local = |l: LocalId| LocalId(base + l.0);
    for d in &callee.locals {
        caller.locals.push(LocalDecl {
            id: local(d.id),
            name: d.name.clone(),
            ty: d.ty.clone(),
        });
    }
    let first = crate::opt::fresh_block(caller).0;
    let block = |b: BlockId| BlockId(first + b.0);
    let rest_id = BlockId(first + callee.blocks.iter().map(|b| b.id.0 + 1).max().unwrap_or(0));
    // The caller's scope at the call: the copy's positions are in it.
    let scope_at = caller.blocks[bi].stmts[..si]
        .iter()
        .rev()
        .find_map(|s| match s {
            Statement::Loc(sp, sc) => Some((*sp, *sc)),
            _ => None,
        });
    let at = scope_at.map_or_else(Scope::default, |(_, sc)| sc);
    // Callee scope `k` is `top + k` here, its body (0) the inline site.
    let site = callee.span.line != 0;
    let top = max_scope(caller) + 1;
    let scope = |sc: Scope| -> Scope {
        if !site {
            at
        } else if sc.id == 0 {
            Scope {
                id: top,
                parent: at.id,
            }
        } else {
            Scope {
                id: top + sc.id,
                parent: top + sc.parent,
            }
        }
    };
    if site {
        caller.dbg.inlined.push(crate::Inlined {
            scope: top,
            parent: at.id,
            name: callee.path().to_owned(),
            decl: callee.span,
            call: scope_at.map_or(caller.span, |(sp, _)| sp),
            params: callee.params.iter().map(|p| local(*p)).collect(),
        });
        for i in &callee.dbg.inlined {
            let mut i = i.clone();
            i.scope += top;
            i.parent += top;
            i.params = i.params.iter().map(|p| local(*p)).collect();
            caller.dbg.inlined.push(i);
        }
    }
    for &(l, line, sc) in &callee.dbg.decls {
        caller.dbg.decls.push((local(l), line, scope(sc)));
    }
    for b in &callee.dbg.binds {
        let mut b = b.clone();
        b.local = local(b.local);
        for v in std::iter::once(&mut b.value).chain(&mut b.parts).flatten() {
            if let Operand::Copy(l) = v {
                *l = local(*l);
            }
        }
        b.scope = scope(b.scope);
        caller.dbg.binds.push(b);
    }
    let b = &mut caller.blocks[bi];
    let mut tail: Vec<Statement> = b.stmts.drain(si..).collect();
    let Statement::Assign {
        target,
        rvalue: RValue::Call { args, .. },
    } = tail.remove(0)
    else {
        unreachable!()
    };
    let unit_result = caller.locals[target.0 as usize].ty == MirTy::Unit;
    if let (Some((sp, sc)), false) = (scope_at, unit_result) {
        caller.dbg.decls.push((target, sp.line, sc));
    }
    let entry = block(callee.blocks[0].id);
    let b = &mut caller.blocks[bi];
    if site {
        b.stmts
            .push(Statement::Loc(callee.span, scope(Scope::default())));
    }
    for (p, a) in callee.params.iter().zip(args) {
        b.stmts.push(Statement::Assign {
            target: local(*p),
            rvalue: RValue::Use(a),
        });
    }
    let term = std::mem::replace(
        &mut b.terminator,
        Terminator::Goto {
            target: entry,
            args: Vec::new(),
        },
    );
    // Back at the caller's position after the copy.
    if let Some((sp, sc)) = scope_at {
        tail.insert(0, Statement::Loc(sp, sc));
    }
    let rest = BasicBlock {
        id: rest_id,
        params: Vec::new(),
        stmts: tail,
        terminator: term,
    };
    let op = |o: &Operand| match o {
        Operand::Copy(l) => Operand::Copy(local(*l)),
        Operand::Const(_) => o.clone(),
    };
    let mut copy: Vec<BasicBlock> = Vec::with_capacity(callee.blocks.len() + 1);
    for cb in &callee.blocks {
        let mut stmts: Vec<Statement> = cb
            .stmts
            .iter()
            .map(|s| remap_stmt(s, &local, &scope))
            .collect();
        let terminator = match &cb.terminator {
            Terminator::Return(v) => {
                if let (Some(v), false) = (v, unit_result) {
                    stmts.push(Statement::Assign {
                        target,
                        rvalue: RValue::Use(op(v)),
                    });
                }
                Terminator::Goto {
                    target: rest_id,
                    args: Vec::new(),
                }
            }
            t => {
                let mut t = t.clone();
                crate::opt::term_ops_mut(&mut t, &mut |o| *o = op(o));
                for s in crate::opt::succs_mut(&mut t) {
                    *s = block(*s);
                }
                t
            }
        };
        copy.push(BasicBlock {
            id: block(cb.id),
            params: cb.params.iter().map(|p| local(*p)).collect(),
            stmts,
            terminator,
        });
    }
    copy.push(rest);
    caller
        .strips
        .extend(callee.strips.iter().map(|h| first + h));
    caller.blocks.splice((bi + 1)..=bi, copy);
}

/// The greatest scope id `f` uses.
fn max_scope(f: &MirFunction) -> u32 {
    let locs = f.blocks.iter().flat_map(|b| &b.stmts).map(|s| match s {
        Statement::Loc(_, sc) => sc.id,
        _ => 0,
    });
    let sites = f.dbg.inlined.iter().map(|i| i.scope);
    let binds = f.dbg.binds.iter().map(|b| b.scope.id);
    let decls = f.dbg.decls.iter().map(|d| d.2.id);
    locs.chain(sites)
        .chain(binds)
        .chain(decls)
        .max()
        .unwrap_or(0)
}

fn remap_stmt(
    s: &Statement,
    local: &impl Fn(LocalId) -> LocalId,
    scope: &impl Fn(Scope) -> Scope,
) -> Statement {
    let mut s = s.clone();
    match &mut s {
        Statement::Assign { target, rvalue } => {
            *target = local(*target);
            match rvalue {
                RValue::AddrOf(l) | RValue::ArrayIndex { array: l, .. } => *l = local(*l),
                _ => {}
            }
        }
        Statement::ArrayWrite { array, .. } => *array = local(*array),
        Statement::StorageDead(l) => *l = local(*l),
        Statement::Loc(_, sc) => *sc = scope(*sc),
        _ => {}
    }
    crate::opt::stmt_ops_mut(&mut s, &mut |o| {
        if let Operand::Copy(l) = o {
            *l = local(*l);
        }
    });
    s
}
