//! Which loads a store may change.
//!
//! An access names its object (the base operand), the type of the
//! object's elements and the part it touches: field `k` of element 0
//! (`HeapField`, `HeapStore`), element 0 whole (`HeapLoad`) or element
//! `i` (`BufLoad`, `BufStore`). An object is allocated with one element
//! type and every access names that type, so accesses of different
//! types touch different objects. Byte buffers and unions are the
//! exception: a `GcBuf<u8>` may be a type-erased view of any object
//! (`transmute_unchecked`), a union any of its variants.
//!
//! A fresh object (a local assigned once from an allocation and used
//! only as the base of accesses) is reached through that local alone:
//! no other base and no call touches it.
//!
//! Calls, raw memory, statics and atomics may touch anything; a call to
//! a function that writes nothing its caller can see (`readonly`, from
//! its body or its summary) touches nothing. A safepoint (poll,
//! allocation) changes no value a load reads: the collector never moves an object a root reaches, and
//! codegen roots every GC reference live across a safepoint, so a
//! reference loaded into a local keeps matching the field it came from
//! while it lives. Codegen derives element addresses per access, after
//! any safepoint.

use crate::{MirFunction, MirModule, MirTy, Operand, RValue, Statement, Terminator};
use nabic_core::{HashMap, HashSet};
use nabic_ir_common::{FnRole, FunctionId, Linkage, LocalId, PrimTy, Summary};

/// The part of an object an access touches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Part {
    /// Field `k` of element 0.
    Field(u32),
    /// Element `i`.
    Elem(Operand),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Acc {
    pub base: Operand,
    pub ty: MirTy,
    pub part: Part,
}

/// An access by reference, to compare without copying types.
#[derive(Clone, Copy)]
pub struct View<'a> {
    pub base: &'a Operand,
    pub ty: &'a MirTy,
    pub part: PartRef<'a>,
}

/// `Part` by reference, and element 0 whole.
#[derive(Clone, Copy)]
pub enum PartRef<'a> {
    Field(u32),
    Whole,
    Elem(&'a Operand),
}

impl Acc {
    pub const fn view(&self) -> View<'_> {
        View {
            base: &self.base,
            ty: &self.ty,
            part: match &self.part {
                Part::Field(k) => PartRef::Field(*k),
                Part::Elem(i) => PartRef::Elem(i),
            },
        }
    }

    pub fn reads(&self, l: LocalId) -> bool {
        let is = |o: &Operand| matches!(o, Operand::Copy(x) if *x == l);
        is(&self.base) || matches!(&self.part, Part::Elem(i) if is(i))
    }
}

/// What a statement may do to memory.
#[derive(Clone, Debug, PartialEq)]
pub enum Eff {
    None,
    Write(Acc),
    /// Anything, and it may read anything too.
    Any,
}

/// The memory `rv` reads, if a plain load.
pub const fn load(rv: &RValue) -> Option<View<'_>> {
    Some(match rv {
        RValue::HeapField { ptr, ty, index } => View {
            base: ptr,
            ty,
            part: PartRef::Field(*index),
        },
        RValue::HeapLoad { ptr, ty } => View {
            base: ptr,
            ty,
            part: PartRef::Whole,
        },
        RValue::BufLoad {
            buf,
            index,
            elem_ty,
        } => View {
            base: buf,
            ty: elem_ty,
            part: PartRef::Elem(index),
        },
        _ => return None,
    })
}

/// What `s` may write; `ro`: the `readonly` functions.
pub fn effect(s: &Statement, ro: &HashSet<FunctionId>) -> Eff {
    match s {
        Statement::Assign { rvalue, .. } => match rvalue {
            RValue::Call { callee, .. } if ro.contains(callee) => Eff::None,
            RValue::Call { .. }
            | RValue::CallIndirect { .. }
            | RValue::BufAtomic { .. }
            | RValue::StaticLoad { .. }
            | RValue::StaticAddr(_)
            | RValue::RawLoad { volatile: true, .. } => Eff::Any,
            _ => Eff::None,
        },
        Statement::BufStore {
            buf,
            index,
            elem_ty,
            ..
        } => Eff::Write(Acc {
            base: buf.clone(),
            ty: elem_ty.clone(),
            part: Part::Elem(index.clone()),
        }),
        Statement::HeapStore { ptr, ty, index, .. } => Eff::Write(Acc {
            base: ptr.clone(),
            ty: ty.clone(),
            part: Part::Field(*index),
        }),
        Statement::RawStore { .. } | Statement::StaticStore { .. } => Eff::Any,
        Statement::ArrayWrite { .. } | Statement::StorageDead(_) | Statement::Loc(..) => Eff::None,
    }
}

/// A type whose objects other types may view.
fn erased(t: &MirTy) -> bool {
    match t {
        MirTy::Prim(PrimTy::U8 | PrimTy::I8) | MirTy::Union(..) => true,
        MirTy::Tuple(fs) | MirTy::Struct(_, fs) => fs.iter().any(erased),
        MirTy::Array(e, _) => erased(e),
        _ => false,
    }
}

fn konst(o: &Operand) -> Option<i128> {
    match o {
        Operand::Const(c) => crate::opt::int_val(c, PrimTy::Usize, 64),
        Operand::Copy(_) => None,
    }
}

/// Whether accesses `a` and `b` may overlap; `fresh`: the fresh objects
/// (`fresh`).
pub fn may_alias(a: &View<'_>, b: &View<'_>, fresh: &HashSet<LocalId>) -> bool {
    if let (Operand::Copy(x), Operand::Copy(y)) = (a.base, b.base) {
        if x != y && (fresh.contains(x) || fresh.contains(y)) {
            return false;
        }
    }
    if a.ty != b.ty {
        return erased(a.ty) || erased(b.ty);
    }
    // Same element type: the parts tell.
    let other = |i: &Operand| konst(i).is_some_and(|k| k != 0);
    match (a.part, b.part) {
        (PartRef::Field(i), PartRef::Field(j)) => i == j,
        (PartRef::Field(_) | PartRef::Whole, PartRef::Elem(i))
        | (PartRef::Elem(i), PartRef::Field(_) | PartRef::Whole) => !other(i),
        (PartRef::Elem(i), PartRef::Elem(j)) => match (konst(i), konst(j)) {
            (Some(p), Some(q)) => p == q,
            _ => true,
        },
        _ => true,
    }
}

/// Whether effect `e` may change what `a` reads.
pub fn clobbers(e: &Eff, a: &View<'_>, fresh: &HashSet<LocalId>) -> bool {
    match e {
        Eff::None => false,
        Eff::Any => !matches!(a.base, Operand::Copy(x) if fresh.contains(x)),
        Eff::Write(w) => may_alias(&w.view(), a, fresh),
    }
}

/// Fresh objects of `f`: locals assigned once, from an allocation, and
/// read only as the base of an access.
pub fn fresh(f: &MirFunction) -> HashSet<LocalId> {
    let allocs = |rv: &RValue| {
        matches!(
            rv,
            RValue::BufAlloc { .. } | RValue::HeapAlloc { .. } | RValue::HeapBox { .. }
        )
    };
    let any = f
        .blocks
        .iter()
        .flat_map(|b| &b.stmts)
        .any(|s| matches!(s, Statement::Assign { rvalue, .. } if allocs(rvalue)));
    if !any {
        return HashSet::default();
    }
    let n = f.locals.len();
    let mut alloc = vec![0u32; n];
    let mut bad = vec![false; n];
    for l in crate::addressed(f)
        .into_iter()
        .chain(f.params.iter().copied())
    {
        bad[l.0 as usize] = true;
    }
    let base = |o: &Operand, ok: &mut Vec<LocalId>| {
        if let Operand::Copy(x) = o {
            ok.push(*x);
        }
    };
    // Locals read as a base only, per statement.
    let mut ok: Vec<LocalId> = Vec::new();
    for b in &f.blocks {
        for p in &b.params {
            bad[p.0 as usize] = true;
        }
        for s in &b.stmts {
            ok.clear();
            match s {
                Statement::Assign { target, rvalue } => {
                    if allocs(rvalue) {
                        alloc[target.0 as usize] += 1;
                    } else {
                        bad[target.0 as usize] = true;
                    }
                    match rvalue {
                        RValue::HeapField { ptr, .. } | RValue::HeapLoad { ptr, .. } => {
                            base(ptr, &mut ok);
                        }
                        RValue::BufLoad { buf, .. } => base(buf, &mut ok),
                        _ => {}
                    }
                }
                Statement::BufStore { buf, .. } => base(buf, &mut ok),
                Statement::HeapStore { ptr, .. } => base(ptr, &mut ok),
                Statement::ArrayWrite { array, .. } => {
                    bad[array.0 as usize] = true;
                }
                _ => {}
            }
            // Every other read (a value, an index, an argument) escapes.
            crate::stmt_reads(s, &mut |x| {
                if let Some(i) = ok.iter().position(|y| *y == x) {
                    ok.swap_remove(i);
                } else {
                    bad[x.0 as usize] = true;
                }
            });
        }
        crate::term_reads(&b.terminator, &mut |u| bad[u.0 as usize] = true);
    }
    (0..n)
        .filter(|&i| alloc[i] == 1 && !bad[i])
        .map(|i| LocalId(i as u32))
        .collect()
}

/// Functions whose calls write no memory a caller reads: they store only
/// into objects they allocated and call only such functions. What runs
/// on a way to a panic (a block ending `unreachable`) is left out: the
/// caller never goes on from there. A function another body may replace
/// (an `any` dispatcher of a prebuilt library) is never one, one with no
/// body here only when its summary says so. The greatest fixed point: a
/// cycle of such functions is.
pub fn readonly(m: &MirModule) -> HashSet<FunctionId> {
    let lazy: HashSet<&str> = m
        .statics
        .iter()
        .filter(|s| s.init_fn.is_some())
        .map(|s| s.name.as_str())
        .collect();
    // Per candidate: the functions it calls; `None`: it writes.
    let mut calls: HashMap<FunctionId, Vec<FunctionId>> = HashMap::default();
    for f in &m.functions {
        let body = !f.blocks.is_empty()
            && matches!(f.facts.linkage, Linkage::Defined | Linkage::Prebuilt)
            && f.facts.role != FnRole::AnyDispatcher;
        let cs = body.then(|| local_writes(f, &lazy)).flatten().or_else(|| {
            (f.facts.summary.has(Summary::READONLY) && f.facts.role != FnRole::AnyDispatcher)
                .then(Vec::new)
        });
        if let Some(cs) = cs {
            calls.insert(f.id, cs);
        }
    }
    loop {
        let gone: Vec<FunctionId> = calls
            .iter()
            .filter(|(_, cs)| cs.iter().any(|c| !calls.contains_key(c)))
            .map(|(f, _)| *f)
            .collect();
        if gone.is_empty() {
            break;
        }
        for f in gone {
            calls.remove(&f);
        }
    }
    calls.into_keys().collect()
}

/// The functions `f` calls if it writes nothing else a caller reads
/// (`readonly`), else `None`.
fn local_writes(f: &MirFunction, lazy: &HashSet<&str>) -> Option<Vec<FunctionId>> {
    // Objects allocated here: locals assigned once, from an allocation.
    let mut defs: HashMap<LocalId, (u32, bool)> = HashMap::default();
    for p in &f.params {
        defs.insert(*p, (2, false));
    }
    for b in &f.blocks {
        for p in &b.params {
            defs.insert(*p, (2, false));
        }
        for s in &b.stmts {
            if let Statement::Assign { target, rvalue } = s {
                let alloc = matches!(
                    rvalue,
                    RValue::BufAlloc { .. } | RValue::HeapAlloc { .. } | RValue::HeapBox { .. }
                );
                let d = defs.entry(*target).or_insert((0, true));
                d.0 += 1;
                d.1 &= alloc;
            }
        }
    }
    let own = |o: &Operand| match o {
        Operand::Copy(l) => defs.get(l).is_some_and(|&(n, a)| n == 1 && a),
        Operand::Const(_) => false,
    };
    let mut calls = Vec::new();
    for b in &f.blocks {
        match b.terminator {
            Terminator::Unreachable => continue,
            Terminator::Suspend { .. } => return None,
            _ => {}
        }
        for s in &b.stmts {
            let ok = match s {
                Statement::Assign { rvalue, .. } => match rvalue {
                    RValue::Call { callee, .. } => {
                        calls.push(*callee);
                        true
                    }
                    RValue::StaticLoad { name, .. } => !lazy.contains(name.as_str()),
                    RValue::CallIndirect { .. }
                    | RValue::BufAtomic { .. }
                    | RValue::StaticAddr(_)
                    | RValue::RawLoad { volatile: true, .. } => false,
                    _ => true,
                },
                Statement::BufStore { buf: p, .. } | Statement::HeapStore { ptr: p, .. } => own(p),
                Statement::RawStore { .. } | Statement::StaticStore { .. } => false,
                Statement::ArrayWrite { .. } | Statement::StorageDead(_) | Statement::Loc(..) => {
                    true
                }
            };
            if !ok {
                return None;
            }
        }
    }
    Some(calls)
}
