//! Heap objects that never leave the function. A box (`HeapBox`) whose
//! reference is only read back (`HeapLoad`, `HeapField`) or copied is
//! the value it was made of: the reads take a copy made where the box
//! was. A buffer of a few scalars (`BufAlloc` of constant length) only
//! indexed (`BufLoad`, `BufStore`) or copied is an array local, which
//! `sroa` may split. The allocation is then unused and goes as dead
//! code. A call through a function value whose code is known goes to
//! that function directly, so a closure's body may be inlined and its
//! captures box with it.
//!
//! No object moves to the stack: the values live in locals, which
//! codegen roots as any other.

use crate::{MirFunction, MirTy, Operand, RValue, Statement};
use nabic_core::{HashMap, HashSet};
use nabic_ir_common::{Abi, ConstVal, LocalId, PrimTy};

/// Elements of a buffer made an array local, at most.
const MAX_ELEMS: u128 = 32;

/// Rewrite what never escapes; whether anything changed.
pub(crate) fn run(f: &mut MirFunction, pw: u32) -> bool {
    let mut any = false;
    for s in f.blocks.iter().flat_map(|b| &b.stmts) {
        if let Statement::Assign { rvalue, .. } = s {
            any |= matches!(
                rvalue,
                RValue::HeapBox { .. }
                    | RValue::BufAlloc {
                        len: Operand::Const(_),
                        ..
                    }
                    | RValue::CallIndirect { abi: Abi::Nabi, .. }
            );
        }
    }
    if !any {
        return false;
    }
    let defs = single_defs(f);
    let mut changed = direct(f, &defs);
    let pinned = crate::opt::pinned(f);
    let mut objs: Vec<(LocalId, Obj)> = Vec::new();
    for (l, rv) in &defs {
        if pinned.contains(l) {
            continue;
        }
        let o = match rv {
            RValue::HeapBox { value, ty } => Obj::Box(value.clone(), ty.clone()),
            RValue::BufAlloc {
                elem_ty: elem_ty @ MirTy::Prim(p),
                len: Operand::Const(c),
            } if scalar(*p) => match crate::opt::int_val(c, PrimTy::Usize, pw) {
                Some(n) if n > 0 && (n as u128) <= MAX_ELEMS => Obj::Buf(elem_ty.clone(), n as u32),
                _ => continue,
            },
            _ => continue,
        };
        objs.push((*l, o));
    }
    objs.sort_by_key(|(l, _)| l.0);
    for (r, o) in objs {
        changed |= replace(f, &defs, &pinned, r, &o);
    }
    changed
}

/// What an allocation holds.
enum Obj {
    /// The value and its type.
    Box(Operand, MirTy),
    /// The element type and the length.
    Buf(MirTy, u32),
}

/// A primitive a zero constant stands for.
const fn scalar(p: PrimTy) -> bool {
    p.is_int() || p.is_float() || matches!(p, PrimTy::Bool)
}

const fn zero(p: PrimTy) -> ConstVal {
    if p.is_float() {
        ConstVal::Float { bits: 0, ty: p }
    } else if matches!(p, PrimTy::Bool) {
        ConstVal::Bool(false)
    } else {
        ConstVal::Int { value: 0, ty: p }
    }
}

/// Locals assigned exactly once, by an `Assign`, and no parameter.
fn single_defs(f: &MirFunction) -> HashMap<LocalId, RValue> {
    let mut defs: HashMap<LocalId, Option<RValue>> = HashMap::default();
    for p in &f.params {
        defs.insert(*p, None);
    }
    for b in &f.blocks {
        for p in &b.params {
            defs.insert(*p, None);
        }
        for s in &b.stmts {
            match s {
                Statement::Assign { target, rvalue } => {
                    defs.entry(*target)
                        .and_modify(|d| *d = None)
                        .or_insert_with(|| Some(rvalue.clone()));
                }
                Statement::ArrayWrite { array, .. } => {
                    defs.insert(*array, None);
                }
                _ => {}
            }
        }
    }
    defs.into_iter()
        .filter_map(|(l, d)| Some((l, d?)))
        .collect()
}

/// Calls through a function value of known code become direct calls.
fn direct(f: &mut MirFunction, defs: &HashMap<LocalId, RValue>) -> bool {
    let code = |mut l: LocalId| -> Option<(nabic_ir_common::FunctionId, &MirTy)> {
        for _ in 0..4 {
            match defs.get(&l)? {
                RValue::Use(Operand::Copy(x)) => l = *x,
                RValue::FnAddr { callee, fn_ty } => return Some((*callee, fn_ty)),
                _ => return None,
            }
        }
        None
    };
    let mut changed = false;
    for s in f.blocks.iter_mut().flat_map(|b| &mut b.stmts) {
        let Statement::Assign { rvalue, .. } = s else {
            continue;
        };
        let RValue::CallIndirect {
            callee: Operand::Copy(c),
            args,
            arg_tys,
            ret_ty,
            abi: Abi::Nabi,
        } = rvalue
        else {
            continue;
        };
        let Some((g, MirTy::Fn(ps, r))) = code(*c) else {
            continue;
        };
        if ps != arg_tys || **r != *ret_ty {
            continue;
        }
        *rvalue = RValue::Call {
            callee: g,
            args: std::mem::take(args),
            ret_ty: ret_ty.clone(),
        };
        changed = true;
    }
    changed
}

/// The locals holding `r`'s reference: `r` and its copies.
fn group(
    defs: &HashMap<LocalId, RValue>,
    pinned: &HashSet<LocalId>,
    r: LocalId,
) -> HashSet<LocalId> {
    let mut g: HashSet<LocalId> = HashSet::default();
    g.insert(r);
    loop {
        let more: Vec<LocalId> = defs
            .iter()
            .filter(|(l, rv)| {
                !g.contains(*l)
                    && !pinned.contains(*l)
                    && matches!(rv, RValue::Use(Operand::Copy(x)) if g.contains(x))
            })
            .map(|(l, _)| *l)
            .collect();
        if more.is_empty() {
            return g;
        }
        g.extend(more);
    }
}

/// Replace the reads of allocation `r` when nothing else reads it.
fn replace(
    f: &mut MirFunction,
    defs: &HashMap<LocalId, RValue>,
    pinned: &HashSet<LocalId>,
    r: LocalId,
    o: &Obj,
) -> bool {
    let g = group(defs, pinned, r);
    let of = |op: &Operand| matches!(op, Operand::Copy(x) if g.contains(x));
    // Every read of a member must be one of the allowed ones.
    let mut reads = 0usize;
    let mut fine = 0usize;
    let mut site = None;
    for (bi, b) in f.blocks.iter().enumerate() {
        for (si, s) in b.stmts.iter().enumerate() {
            crate::stmt_reads(s, &mut |u| reads += usize::from(g.contains(&u)));
            fine += match (s, o) {
                (
                    Statement::Assign {
                        target,
                        rvalue: RValue::Use(x),
                    },
                    _,
                ) if g.contains(target) && of(x) => 1,
                (
                    Statement::Assign {
                        rvalue: RValue::HeapLoad { ptr, ty } | RValue::HeapField { ptr, ty, .. },
                        ..
                    },
                    Obj::Box(_, bt),
                ) if of(ptr) && ty == bt => 1,
                (
                    Statement::Assign {
                        rvalue: RValue::BufLoad { buf, elem_ty, .. },
                        ..
                    }
                    | Statement::BufStore { buf, elem_ty, .. },
                    Obj::Buf(et, _),
                ) if of(buf) && elem_ty == et => 1,
                _ => 0,
            };
            if matches!(s, Statement::Assign { target, .. } if *target == r) {
                site = Some((bi, si));
            }
        }
        crate::term_reads(&b.terminator, &mut |u| reads += usize::from(g.contains(&u)));
    }
    let Some((bi, si)) = site else {
        return false;
    };
    if reads != fine || reads == 0 {
        return false;
    }
    let (held, init) = match o {
        Obj::Box(Operand::Const(c), _) => (Operand::Const(c.clone()), None),
        Obj::Box(v, ty) => {
            let h = crate::opt::fresh_local(f, ty.clone());
            (Operand::Copy(h), Some((h, RValue::Use(v.clone()))))
        }
        Obj::Buf(et, n) => {
            let MirTy::Prim(p) = et else { return false };
            let a = crate::opt::fresh_local(f, MirTy::Array(Box::new(et.clone()), *n));
            let elements = vec![Operand::Const(zero(*p)); *n as usize];
            (
                Operand::Copy(a),
                Some((
                    a,
                    RValue::ArrayInit {
                        elem_ty: et.clone(),
                        elements,
                    },
                )),
            )
        }
    };
    let tys: Vec<MirTy> = f.locals.iter().map(|d| d.ty.clone()).collect();
    for b in &mut f.blocks {
        for s in &mut b.stmts {
            match s {
                Statement::Assign { target, rvalue } => {
                    let new = match (&*rvalue, o) {
                        (RValue::HeapLoad { ptr, .. }, Obj::Box(..)) if of(ptr) => {
                            RValue::Use(held.clone())
                        }
                        (RValue::HeapField { ptr, index, .. }, Obj::Box(..)) if of(ptr) => {
                            RValue::FieldRead {
                                target: held.clone(),
                                index: *index,
                                field_ty: tys[target.0 as usize].clone(),
                            }
                        }
                        (
                            RValue::BufLoad {
                                buf,
                                index,
                                elem_ty,
                            },
                            Obj::Buf(_, n),
                        ) if of(buf) => {
                            let Operand::Copy(a) = held else {
                                unreachable!()
                            };
                            RValue::ArrayIndex {
                                array: a,
                                offset: 0,
                                index: index.clone(),
                                elem_ty: elem_ty.clone(),
                                len: *n,
                            }
                        }
                        _ => continue,
                    };
                    *rvalue = new;
                }
                Statement::BufStore {
                    buf,
                    index,
                    value,
                    elem_ty,
                } if of(buf) => {
                    let (Operand::Copy(a), Obj::Buf(_, n)) = (&held, o) else {
                        continue;
                    };
                    *s = Statement::ArrayWrite {
                        array: *a,
                        offset: 0,
                        index: index.clone(),
                        value: value.clone(),
                        elem_ty: elem_ty.clone(),
                        len: *n,
                    };
                }
                _ => {}
            }
        }
    }
    if let Some((h, rv)) = init {
        f.blocks[bi].stmts.insert(
            si,
            Statement::Assign {
                target: h,
                rvalue: rv,
            },
        );
    }
    true
}
