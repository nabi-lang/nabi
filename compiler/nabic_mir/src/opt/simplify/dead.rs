//! Dead assignments and the values `-g` keeps for them.

#[allow(clippy::wildcard_imports)]
use crate::opt::simplify::*;

/// Drops assignments of unused values that are pure or plain loads; a
/// named local losing its assignment keeps what it held for `-g`
/// (`binds`).
pub(crate) fn dead(f: &mut MirFunction, pw: u32) -> bool {
    let pinned = crate::opt::pinned(f);
    let mut changed = false;
    for _ in 0..8 {
        let uses = crate::opt::use_counts(f);
        let mut any = false;
        // `(target, rvalue, line, scope)` of what goes, when `-g` may ask.
        let mut gone: Vec<(LocalId, RValue, u32, crate::Scope)> = Vec::new();
        let read: HashSet<LocalId> = f
            .dbg
            .binds
            .iter()
            .flat_map(|b| std::iter::once(&b.value).chain(&b.parts))
            .filter_map(|v| match v {
                Some(Operand::Copy(l)) => Some(*l),
                _ => None,
            })
            .collect();
        let locals = &f.locals;
        let wanted = |t: LocalId| {
            read.contains(&t)
                || locals[t.0 as usize]
                    .name
                    .as_deref()
                    .is_some_and(crate::debug_visible)
        };
        let mut at = (f.span.line, crate::Scope::default());
        for b in &mut f.blocks {
            b.stmts.retain(|s| match s {
                Statement::Assign { target, rvalue } => {
                    let dead = uses[target.0 as usize] == 0
                        && !pinned.contains(target)
                        && removable(rvalue, pw);
                    if dead && wanted(*target) {
                        gone.push((*target, rvalue.clone(), at.0, at.1));
                    }
                    any |= dead;
                    !dead
                }
                Statement::Loc(sp, sc) => {
                    at = (sp.line, *sc);
                    true
                }
                _ => true,
            });
        }
        if !gone.is_empty() {
            bind(f, &gone);
        }
        // Self copies.
        for b in &mut f.blocks {
            b.stmts.retain(|s| {
                let selfc = matches!(s, Statement::Assign { target, rvalue: RValue::Use(Operand::Copy(y)) } if y == target);
                any |= selfc;
                !selfc
            });
        }
        changed |= any;
        if !any {
            break;
        }
    }
    changed
}

/// Record what the dropped assignments `gone` of `f` held: a named
/// local's value, and the values of binds that read a dropped local.
///
/// A local read for one holds its value wherever the debugger shows it:
/// assigned once, or never assigned within the bound local's scope (a
/// loop's counter passed to a body inlined in the loop).
pub(crate) fn bind(f: &mut MirFunction, gone: &[(LocalId, RValue, u32, crate::Scope)]) {
    // Assignments left; the scopes each local is written in (`None`: one
    // not known).
    let mut defs = vec![0u32; f.locals.len()];
    let mut writes: Vec<Vec<Option<u32>>> = vec![Vec::new(); f.locals.len()];
    let mut parent: HashMap<u32, u32> = HashMap::default();
    for b in &f.blocks {
        let first = b.stmts.iter().find_map(|s| match s {
            Statement::Loc(_, sc) => Some(sc.id),
            _ => None,
        });
        for p in &b.params {
            writes[p.0 as usize].push(first);
        }
        let mut at = first;
        for s in &b.stmts {
            match s {
                Statement::Loc(_, sc) => {
                    at = Some(sc.id);
                    parent.insert(sc.id, sc.parent);
                }
                Statement::Assign { target, .. } => {
                    defs[target.0 as usize] += 1;
                    writes[target.0 as usize].push(at);
                }
                Statement::ArrayWrite { array: l, .. } | Statement::StorageDead(l) => {
                    defs[l.0 as usize] += 1;
                    writes[l.0 as usize].push(at);
                }
                _ => {}
            }
        }
    }
    for i in &f.dbg.inlined {
        parent.insert(i.scope, i.parent);
    }
    // Scope `s` is `root` or within it.
    let within = |mut s: u32, root: u32| {
        for _ in 0..=parent.len() {
            if s == root {
                return true;
            }
            if s == 0 {
                return false;
            }
            s = parent.get(&s).copied().unwrap_or(0);
        }
        true
    };
    // Dropped assignments per local; the rvalue of one dropped alone.
    let mut dropped = vec![0u32; f.locals.len()];
    let mut was: HashMap<LocalId, &RValue> = HashMap::default();
    for (t, rv, ..) in gone {
        dropped[t.0 as usize] += 1;
        was.insert(*t, rv);
    }
    let param = |l: LocalId| f.params.contains(&l);
    // Holds one value in scope `sc` (counting what goes now).
    let stable = |o: &Operand, sc: u32| match o {
        Operand::Const(_) => true,
        Operand::Copy(y) => {
            let y = *y;
            let n = defs[y.0 as usize] + dropped[y.0 as usize];
            let w = &writes[y.0 as usize];
            let once = if param(y) {
                n == 0
            } else {
                n == 1 && w.len() == 1
            };
            once || (sc != 0 && w.iter().all(|s| s.is_some_and(|s| !within(s, sc))))
        }
    };
    let value = |t: LocalId, rv: &RValue, sc: u32| match rv {
        RValue::Use(o)
            if dropped[t.0 as usize] == 1 && defs[t.0 as usize] == 0 && stable(o, sc) =>
        {
            Some(o.clone())
        }
        _ => None,
    };
    // An aggregate built in place: its fields that hold.
    let parts = |t: LocalId, rv: &RValue, sc: u32| match rv {
        RValue::StructInit { fields, .. }
            if dropped[t.0 as usize] == 1 && defs[t.0 as usize] == 0 =>
        {
            let p: Vec<_> = fields
                .iter()
                .map(|o| stable(o, sc).then(|| o.clone()))
                .collect();
            if p.iter().any(Option::is_some) {
                p
            } else {
                Vec::new()
            }
        }
        _ => Vec::new(),
    };
    let mut binds = std::mem::take(&mut f.dbg.binds);
    for (t, rv, line, scope) in gone {
        let named = f.locals[t.0 as usize]
            .name
            .as_deref()
            .is_some_and(crate::debug_visible);
        if named && !binds.iter().any(|b| b.local == *t) {
            binds.push(crate::Bind {
                local: *t,
                value: value(*t, rv, scope.id),
                parts: parts(*t, rv, scope.id),
                line: *line,
                scope: *scope,
            });
        }
    }
    // A value read from a local going too, or gone before: what that
    // one held.
    for k in 0..binds.len() {
        for _ in 0..8 {
            let Some(Operand::Copy(y)) = binds[k].value.clone() else {
                break;
            };
            let sc = binds[k].scope.id;
            if let Some(rv) = was.get(&y) {
                binds[k].value = value(y, rv, sc);
                binds[k].parts = parts(y, rv, sc);
                continue;
            }
            let from = binds.iter().position(|b| b.local == y);
            match from.filter(|&j| j != k && defs[y.0 as usize] == 0) {
                Some(j) => {
                    let (v, p) = (binds[j].value.clone(), binds[j].parts.clone());
                    (binds[k].value, binds[k].parts) = (v, p);
                }
                None => break,
            }
        }
    }
    for b in &mut binds {
        for p in &mut b.parts {
            for _ in 0..8 {
                let Some(Operand::Copy(y)) = &*p else {
                    break;
                };
                let Some(rv) = was.get(y) else {
                    break;
                };
                *p = value(*y, rv, b.scope.id);
            }
        }
    }
    f.dbg.binds = binds;
}
