//! MIR analyses for the backend: where code polls and links GC
//! frames, GC object layout and scan tables, constant folding facts.

use nabic_core::{HashMap, HashSet};
use nabic_ir_common::{Abi, ConstVal, FnRole, FunctionId, Linkage, PrimOp, PrimTy};
use nabic_mir::gc_abi::{HEADER_SIZE, MIN_OBJECT_SIZE, SUM_DESC_SHIFT, SUM_PAYLOAD, TABLE_ARRAY};
use nabic_mir::{GcSlot, MirFunction, MirModule, MirTy, Operand, RValue, Statement, Target};

/// The REPL session's package hash (`"repl"`).
pub const REPL_PKG_HASH: &str = "7265706c";

/// Polling back edges by block, with their headers: all but those of short
/// loops (`nabic_mir::short_loops`) reaching no safepoint.
#[allow(clippy::implicit_hasher)]
pub fn polled_back_edges(
    f: &MirFunction,
    quiet: &HashSet<FunctionId>,
    pkgs: &crate::mangle::Pkgs,
    target: &Target,
) -> HashMap<u32, HashSet<u32>> {
    let mut edges = nabic_mir::back_edges(f);
    let unpolled: HashSet<u32> = nabic_mir::short_loops(f)
        .into_iter()
        .filter(|(_, body)| {
            !f.blocks.iter().any(|b| {
                let skip = nabic_mir::noreturn_call(b);
                body.contains(&b.id.0)
                    && b.stmts
                        .iter()
                        .enumerate()
                        .any(|(i, s)| Some(i) != skip && stmt_safepoint(s, quiet, pkgs, target))
            })
        })
        .map(|(h, _)| h)
        .collect();
    edges.retain(|_, hs| {
        hs.retain(|h| !unpolled.contains(h));
        !hs.is_empty()
    });
    edges
}

/// Can `s` reach a safepoint (a collection may run before it completes):
/// a call outside `quiet`, an allocation, a lazy static init.
#[allow(clippy::implicit_hasher)]
pub fn stmt_safepoint(
    s: &Statement,
    quiet: &HashSet<FunctionId>,
    pkgs: &crate::mangle::Pkgs,
    target: &Target,
) -> bool {
    match s {
        Statement::Assign { rvalue, .. } => match rvalue {
            RValue::Call { callee, .. } => !quiet.contains(callee),
            RValue::HeapBox { ty, .. } => !target.any_inline(ty),
            RValue::StaticLoad { name, .. } | RValue::StaticAddr(name) => {
                pkgs.lazy_statics.contains_key(name)
            }
            RValue::CallIndirect { .. } | RValue::HeapAlloc { .. } | RValue::BufAlloc { .. } => {
                true
            }
            _ => false,
        },
        Statement::StaticStore { name, .. } => pkgs.lazy_statics.contains_key(name),
        _ => false,
    }
}

/// Whether some statement of `f` can reach a safepoint (`stmt_safepoint`),
/// a never-returning call aside.
#[allow(clippy::implicit_hasher)]
pub fn body_safepoints(
    f: &MirFunction,
    quiet: &HashSet<FunctionId>,
    pkgs: &crate::mangle::Pkgs,
    target: &Target,
) -> bool {
    f.blocks.iter().any(|b| {
        let skip = nabic_mir::noreturn_call(b);
        b.stmts
            .iter()
            .enumerate()
            .any(|(i, s)| Some(i) != skip && stmt_safepoint(s, quiet, pkgs, target))
    })
}

/// Functions some call cycle may pass through.
///
/// Every such cycle polls: members of a direct-call cycle, callers of what
/// this module can't see (indirect calls, C), functions called from C or
/// through a pointer, and `any` dispatchers. The rest are only entered from
/// callers that poll.
pub fn reentrant_functions(module: &MirModule) -> HashSet<FunctionId> {
    let index: HashMap<FunctionId, usize> = module
        .functions
        .iter()
        .enumerate()
        .map(|(i, f)| (f.id, i))
        .collect();
    let mut out = HashSet::default();
    let mut succs: Vec<Vec<usize>> = Vec::with_capacity(module.functions.len());
    for f in &module.functions {
        let mut next = Vec::new();
        let mut opaque = f.facts.abi != Abi::Nabi
            || f.facts.role == FnRole::AnyDispatcher
            || f.facts.export_name.is_some();
        for s in f.blocks.iter().flat_map(|b| &b.stmts) {
            let Statement::Assign { rvalue, .. } = s else {
                continue;
            };
            match rvalue {
                RValue::Call { callee, .. } => match index.get(callee) {
                    Some(&j) if module.functions[j].facts.abi != Abi::Nabi => opaque = true,
                    Some(&j) => next.push(j),
                    None => opaque = true,
                },
                RValue::CallIndirect { .. } => opaque = true,
                RValue::FnAddr { callee, .. } => {
                    out.insert(*callee);
                }
                _ => {}
            }
        }
        if opaque {
            out.insert(f.id);
        }
        succs.push(next);
    }
    // Tarjan's strongly connected components, iteratively.
    let n = succs.len();
    let (mut idx, mut low) = (vec![usize::MAX; n], vec![0; n]);
    let (mut on, mut stack, mut next_idx) = (vec![false; n], Vec::new(), 0);
    for root in 0..n {
        if idx[root] != usize::MAX {
            continue;
        }
        let mut work: Vec<(usize, usize)> = vec![(root, 0)];
        while let Some(top) = work.last_mut() {
            let (v, k) = *top;
            if k == 0 {
                idx[v] = next_idx;
                low[v] = next_idx;
                next_idx += 1;
                stack.push(v);
                on[v] = true;
            }
            if let Some(&w) = succs[v].get(k) {
                top.1 += 1;
                if idx[w] == usize::MAX {
                    work.push((w, 0));
                } else if on[w] {
                    low[v] = low[v].min(idx[w]);
                }
                continue;
            }
            work.pop();
            if let Some(&(u, _)) = work.last() {
                low[u] = low[u].min(low[v]);
            }
            if low[v] == idx[v] {
                let mut scc = Vec::new();
                while let Some(w) = stack.pop() {
                    on[w] = false;
                    scc.push(w);
                    if w == v {
                        break;
                    }
                }
                if scc.len() > 1 || succs[v].contains(&v) {
                    out.extend(scc.iter().map(|&w| module.functions[w].id));
                }
            }
        }
    }
    out
}

/// Functions entered other than by a direct nabi call: from C, through a
/// pointer, exported, or the `any` dispatcher. They poll at entry; other
/// reentrant functions poll before their first safepoint.
pub fn entered_functions(module: &MirModule) -> HashSet<FunctionId> {
    let mut out: HashSet<FunctionId> = module
        .functions
        .iter()
        .filter(|f| {
            f.facts.abi != Abi::Nabi
                || f.facts.role == FnRole::AnyDispatcher
                || f.facts.export_name.is_some()
        })
        .map(|f| f.id)
        .collect();
    for s in module
        .functions
        .iter()
        .flat_map(|f| &f.blocks)
        .flat_map(|b| &b.stmts)
    {
        if let Statement::Assign {
            rvalue: RValue::FnAddr { callee, .. },
            ..
        } = s
        {
            out.insert(*callee);
        }
    }
    out
}

/// Functions that never reach a safepoint.
///
/// Only short loops, no allocation or indirect call, and calls only to
/// such functions (never-returning ones aside). Least fixpoint, so never
/// recursive. They need no frame or entry poll: they run for bounded time
/// and no collection sees their references. A declaration is one when
/// `#[no_safepoint]` or its summary says so (`Summary::QUIET`).
pub fn quiet_functions(
    module: &MirModule,
    pkgs: &crate::mangle::Pkgs,
    target: &Target,
) -> HashSet<FunctionId> {
    let mut pending: HashMap<FunctionId, usize> = HashMap::default();
    let mut callers: HashMap<FunctionId, Vec<FunctionId>> = HashMap::default();
    let all: HashSet<FunctionId> = module.functions.iter().map(|f| f.id).collect();
    // A hot package's function may be swapped for one that stops: its
    // callers root across it.
    let hot = |f: &MirFunction| module.plugin.hot.as_ref().is_some_and(|h| h.owns(f.path()));
    let known = |f: &MirFunction| {
        !hot(f) && (f.facts.no_safepoint || f.facts.summary.has(nabic_ir_common::Summary::QUIET))
    };
    // A prebuilt function's inline copy has its body here too.
    let candidates = module.functions.iter().filter(|f| !hot(f)).filter(|f| {
        let body = f.facts.linkage == Linkage::Defined
            || (f.facts.linkage == Linkage::Prebuilt && !f.blocks.is_empty());
        body && !known(f) && f.facts.role != FnRole::AnyDispatcher && {
            let short = nabic_mir::short_loops(f);
            nabic_mir::back_edges(f)
                .values()
                .flatten()
                .all(|h| short.contains_key(h))
        }
    });
    for f in candidates {
        // Every call counts as quiet here; the callees are checked below.
        if body_safepoints(f, &all, pkgs, target) {
            continue;
        }
        let mut callees: HashSet<FunctionId> = HashSet::default();
        for b in &f.blocks {
            let skip = nabic_mir::noreturn_call(b);
            for (i, s) in b.stmts.iter().enumerate() {
                match s {
                    Statement::Assign {
                        rvalue: RValue::Call { callee, .. },
                        ..
                    } if Some(i) != skip => {
                        callees.insert(*callee);
                    }
                    _ => {}
                }
            }
        }
        pending.insert(f.id, callees.len());
        for c in callees {
            callers.entry(c).or_default().push(f.id);
        }
    }
    for f in module.functions.iter().filter(|f| known(f)) {
        pending.insert(f.id, 0);
    }
    let mut quiet = HashSet::default();
    let mut ready: Vec<FunctionId> = pending
        .iter()
        .filter(|(_, n)| **n == 0)
        .map(|(id, _)| *id)
        .collect();
    while let Some(id) = ready.pop() {
        quiet.insert(id);
        for c in callers.get(&id).into_iter().flatten() {
            if let Some(n) = pending.get_mut(c) {
                *n -= 1;
                if *n == 0 {
                    ready.push(*c);
                }
            }
        }
    }
    quiet
}

/// `(total_size, align)` of a GC object with a `ty` payload: at least
/// `MIN_OBJECT_SIZE`, as `nabi_gc_alloc_slow` rounds it (a `()` or empty
/// struct payload still gets its word).
pub fn gc_object_size(ty: &MirTy, target: &Target) -> (u32, u32) {
    let (size, align) = target.size_align(ty);
    let align = align.max(8);
    let total = HEADER_SIZE + size.next_multiple_of(align);
    (total.max(MIN_OBJECT_SIZE), align)
}

/// The `ScanTable` of `ty` objects (`array`: buffers of `ty`).
///
/// `u32` words: header `{total_size u32, version u16 = 1, num_entries
/// u16}`, `{field_offset u32, field_kind u32}` entries, then sum
/// descriptors. An array table's size is the stride, offsets
/// element-relative.
pub fn scan_table(ty: &MirTy, array: bool, target: &Target) -> Vec<u32> {
    let mut slots = Vec::new();
    let (total, version) = if array {
        target.gc_offsets(ty, 0, &mut slots);
        (target.size_align(ty).0, 1 | TABLE_ARRAY)
    } else {
        target.gc_offsets(ty, HEADER_SIZE, &mut slots);
        (gc_object_size(ty, target).0, 1)
    };
    let mut words = vec![total, version | ((slots.len() as u32) << 16)];
    words.resize(2 + 2 * slots.len(), 0);
    encode_slots(&slots, 0, 2, &mut words);
    words
}

/// Write `slots` at word `at` (reserved), offsets relative to `base`. A
/// sum's entry is its tag offset and `SUM_PAYLOAD | desc << SUM_DESC_SHIFT`;
/// the appended descriptor is `n`, then `{tag, num_entries, entries}` per
/// data variant, offsets from the tag.
fn encode_slots(slots: &[GcSlot], base: u32, at: usize, words: &mut Vec<u32>) {
    for (i, slot) in slots.iter().enumerate() {
        let (off, kind) = match slot {
            GcSlot::Word(off, kind) => (*off, *kind),
            GcSlot::Sum(off, variants) => {
                let desc = words.len();
                words.push(variants.len() as u32);
                words.resize(desc + 1 + 3 * variants.len(), 0);
                for (k, (tag, v)) in variants.iter().enumerate() {
                    let entries = words.len();
                    words.resize(entries + 2 * v.len(), 0);
                    let head = desc + 1 + 3 * k;
                    words[head..head + 3].copy_from_slice(&[*tag, v.len() as u32, entries as u32]);
                    encode_slots(v, *off, entries, words);
                }
                (*off, SUM_PAYLOAD | ((desc as u32) << SUM_DESC_SHIFT))
            }
        };
        words[at + 2 * i] = off - base;
        words[at + 2 * i + 1] = kind;
    }
}

/// `+ - *` whose constant operands rule out overflow: both constant and
/// fitting, or an identity / absorbing constant (`x + 0`, `x * 1`, `x * 0`).
pub fn cannot_overflow(
    op: PrimOp,
    prim: PrimTy,
    bits: u32,
    (l, r): (Option<i128>, Option<i128>),
) -> bool {
    if let (Some(a), Some(b)) = (l, r) {
        let v = match op {
            PrimOp::Add => a.checked_add(b),
            PrimOp::Sub => a.checked_sub(b),
            PrimOp::Mul => a.checked_mul(b),
            _ => None,
        };
        let (lo, hi) = if prim.is_signed_int() {
            (-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1)
        } else {
            (
                0,
                if bits >= 127 {
                    i128::MAX
                } else {
                    (1i128 << bits) - 1
                },
            )
        };
        return v.is_some_and(|v| (lo..=hi).contains(&v));
    }
    match op {
        PrimOp::Add => l == Some(0) || r == Some(0),
        PrimOp::Sub => r == Some(0),
        PrimOp::Mul => matches!(l, Some(0 | 1)) || matches!(r, Some(0 | 1)),
        _ => false,
    }
}

/// Integer value of a constant operand (`Int` / in-range `UInt`).
pub fn const_int(op: &Operand) -> Option<i128> {
    match op {
        Operand::Const(ConstVal::Int { value, .. }) => Some(*value),
        Operand::Const(ConstVal::UInt { value, .. }) => i128::try_from(*value).ok(),
        _ => None,
    }
}

/// Bytes of an array of scalar `elem_ty` constants as stored on `target`
/// (in its byte order); `None` unless all are such constants.
#[must_use]
pub fn const_bytes(elements: &[Operand], elem_ty: &MirTy, target: &Target) -> Option<Vec<u8>> {
    let MirTy::Prim(p) = elem_ty else { return None };
    let size = target.size_align(elem_ty).0 as usize;
    let mut out = Vec::with_capacity(elements.len() * size);
    for e in elements {
        let Operand::Const(c) = e else { return None };
        let v: u128 = match c {
            ConstVal::Int { value, .. } if p.is_int() => *value as u128,
            ConstVal::UInt { value, .. } if p.is_int() => *value,
            ConstVal::Char(c) if p.is_int() => u128::from(*c),
            ConstVal::Bool(b) if *p == PrimTy::Bool => u128::from(*b),
            ConstVal::Float { bits, ty } if *ty == *p && p.is_float() => u128::from(*bits),
            _ => return None,
        };
        if target.endian == "big" {
            out.extend_from_slice(&v.to_be_bytes()[16 - size..]);
        } else {
            out.extend_from_slice(&v.to_le_bytes()[..size]);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gc_objects_have_at_least_the_minimum_size() {
        let t = Target::builtin("x86_64-unknown-linux-gnu");
        assert_eq!(gc_object_size(&MirTy::Unit, &t), (MIN_OBJECT_SIZE, 8));
        assert_eq!(scan_table(&MirTy::Unit, false, &t)[0], MIN_OBJECT_SIZE);
        let i64_ = MirTy::Prim(PrimTy::I64);
        assert_eq!(gc_object_size(&i64_, &t), (24, 8));
    }

    #[test]
    fn constant_operands_skip_the_check() {
        let i8_ = |op, l, r| cannot_overflow(op, PrimTy::I8, 8, (l, r));
        assert!(i8_(PrimOp::Add, Some(100), Some(27)));
        assert!(!i8_(PrimOp::Add, Some(100), Some(28)));
        assert!(i8_(PrimOp::Mul, None, Some(1)));
        assert!(i8_(PrimOp::Sub, None, Some(0)));
        assert!(!i8_(PrimOp::Sub, Some(0), None));
        assert!(!i8_(PrimOp::Add, None, Some(1)));
        assert!(!cannot_overflow(
            PrimOp::Sub,
            PrimTy::U8,
            8,
            (Some(1), Some(2))
        ));
        assert!(cannot_overflow(
            PrimOp::Mul,
            PrimTy::U64,
            64,
            (Some(1 << 40), Some(2))
        ));
    }
}
