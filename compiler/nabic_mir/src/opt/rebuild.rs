//! Wide structs built again from a parameter's fields. Scalar
//! replacement takes a parameter apart on entry and builds it again
//! where the whole value is needed (`S { _0.0, _0.1, y, _0.3 }`): each
//! field copied out and back in. A build whose fields are mostly the
//! parameter's own, at their places, becomes the parameter with the
//! others replaced (`_0 with .2 = y`): one copy, and the field reads
//! that only fed the build die.
//!
//! Only parameters never assigned: a field read of one holds that
//! field of the value the caller passed, wherever it is read.

use crate::{MirFunction, MirTy, Operand, RValue, Statement, Target};
use nabic_core::HashMap;
use nabic_ir_common::LocalId;

/// Structs up to this size stay builds: their fields live in registers.
const MIN_BYTES: u32 = 64;

pub fn run(f: &mut MirFunction, t: &Target) -> bool {
    if f.params.is_empty() || !f.pins.is_empty() {
        return false;
    }
    let addressed = crate::addressed(f);
    let mut defs = vec![0u32; f.locals.len()];
    for b in &f.blocks {
        for p in &b.params {
            defs[p.0 as usize] += 2;
        }
        for s in &b.stmts {
            match s {
                Statement::Assign { target, .. } => defs[target.0 as usize] += 1,
                Statement::ArrayWrite { array, .. } => defs[array.0 as usize] += 2,
                _ => {}
            }
        }
    }
    let stable =
        |l: LocalId| f.params.contains(&l) && defs[l.0 as usize] == 0 && !addressed.contains(&l);
    // Locals assigned once, by a field read of a stable parameter.
    let mut from: HashMap<LocalId, (LocalId, u32)> = HashMap::default();
    for b in &f.blocks {
        for s in &b.stmts {
            if let Statement::Assign {
                target,
                rvalue:
                    RValue::FieldRead {
                        target: Operand::Copy(p),
                        index,
                        ..
                    },
            } = s
            {
                if defs[target.0 as usize] == 1 && stable(*p) {
                    from.insert(*target, (*p, *index));
                }
            }
        }
    }
    if from.is_empty() {
        return false;
    }
    let ty_of = |l: LocalId| &f.locals[l.0 as usize].ty;
    let mut changed = false;
    for b in &mut f.blocks {
        let mut out = Vec::with_capacity(b.stmts.len());
        for s in std::mem::take(&mut b.stmts) {
            let Statement::Assign {
                target,
                rvalue: RValue::StructInit { ty, fields },
            } = &s
            else {
                out.push(s);
                continue;
            };
            if !matches!(ty, MirTy::Struct(..) | MirTy::Tuple(_)) || t.size_align(ty).0 <= MIN_BYTES
            {
                out.push(s);
                continue;
            }
            // The parameter giving the most fields at their places.
            let mut count: HashMap<LocalId, usize> = HashMap::default();
            for (i, o) in fields.iter().enumerate() {
                if let Operand::Copy(l) = o {
                    if let Some(&(p, j)) = from.get(l) {
                        if j as usize == i && ty_of(p) == ty && p != *target {
                            *count.entry(p).or_default() += 1;
                        }
                    }
                }
            }
            let Some((p, _)) = count
                .into_iter()
                .max_by_key(|&(p, n)| (n, std::cmp::Reverse(p.0)))
            else {
                out.push(s);
                continue;
            };
            let own = |i: usize, o: &Operand| matches!(o, Operand::Copy(l) if from.get(l) == Some(&(p, i as u32)));
            let mut src = Operand::Copy(p);
            let mut any = false;
            for (i, o) in fields.iter().enumerate() {
                if own(i, o) {
                    continue;
                }
                out.push(Statement::Assign {
                    target: *target,
                    rvalue: RValue::WithField {
                        source: src,
                        struct_ty: ty.clone(),
                        index: i as u32,
                        value: o.clone(),
                    },
                });
                src = Operand::Copy(*target);
                any = true;
            }
            if !any {
                out.push(Statement::Assign {
                    target: *target,
                    rvalue: RValue::Use(src),
                });
            }
            changed = true;
        }
        b.stmts = out;
    }
    changed
}
