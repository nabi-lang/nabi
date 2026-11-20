//! Constant tables passed by value. A call passing an array built of
//! constants (`ratio(P, Q, r)` with `const P: [f64; 8]`) to a parameter
//! its callee only reads goes to a copy of the callee with that array
//! built in its entry instead: the call no longer copies the table into
//! the argument registers and the callee's frame, and the copy reads it
//! in place, as it reads any constant table.

use crate::{MirFunction, MirTy, Operand, RValue, Statement};
use nabic_core::hash::{hex, sha256};
use nabic_core::{HashMap, HashSet};
use nabic_ir_common::{Abi, FnRole, FunctionId, Linkage, LocalId, MONO_MARK};
use std::fmt::Write;

/// Copies of one function at most: bounds the code growth.
const MAX_COPIES: usize = 4;

/// Bodies bigger than this (`opt::size` units) are not copied.
const MAX_SIZE: usize = 400;

/// Arrays of fewer elements pass in a few registers: not worth a copy.
const MIN_LEN: u32 = 4;

/// The parameters of `f` that are arrays it never writes, nor takes the
/// address of.
fn read_only_arrays(f: &MirFunction) -> Vec<usize> {
    let mut written: HashSet<LocalId> = crate::liveness::addressed(f);
    for b in &f.blocks {
        written.extend(b.params.iter().copied());
        for s in &b.stmts {
            match s {
                Statement::Assign { target, .. } => {
                    written.insert(*target);
                }
                Statement::ArrayWrite { array, .. } => {
                    written.insert(*array);
                }
                _ => {}
            }
        }
    }
    f.params
        .iter()
        .enumerate()
        .filter(|(i, p)| {
            matches!(f.param_tys[*i], MirTy::Array(_, n) if n >= MIN_LEN) && !written.contains(p)
        })
        .map(|(i, _)| i)
        .collect()
}

/// A function whose calls all are in this module and may go to a copy.
fn copyable(f: &MirFunction, taken: &HashSet<FunctionId>) -> bool {
    crate::opt::eligible(f)
        && f.facts.linkage == Linkage::Defined
        && f.facts.export_name.is_none()
        && f.facts.abi == Abi::Nabi
        && f.facts.role == FnRole::Plain
        && f.facts.wasm_import.is_none()
        && !taken.contains(&f.id)
        && crate::opt::size(f) <= MAX_SIZE
}

/// Locals of `f` assigned once, from an array of constants.
fn const_arrays(f: &MirFunction) -> HashMap<LocalId, &RValue> {
    let mut defs: HashMap<LocalId, Option<&RValue>> = HashMap::default();
    for b in &f.blocks {
        for p in &b.params {
            defs.insert(*p, None);
        }
        for s in &b.stmts {
            let def = match s {
                Statement::Assign { target, rvalue } => Some((*target, Some(rvalue))),
                Statement::ArrayWrite { array, .. } => Some((*array, None)),
                _ => None,
            };
            if let Some((l, r)) = def {
                defs.entry(l).and_modify(|d| *d = None).or_insert(r);
            }
        }
    }
    let addressed = crate::liveness::addressed(f);
    defs.into_iter()
        .filter(|(l, _)| !addressed.contains(l))
        .filter_map(|(l, d)| match d? {
            r @ RValue::ArrayInit { elements, .. }
                if elements.iter().all(|e| matches!(e, Operand::Const(_))) =>
            {
                Some((l, r))
            }
            _ => None,
        })
        .collect()
}

/// A copy's key: the callee and its constant arguments, by position.
type Key = (FunctionId, Vec<(usize, String)>);

/// A copy to build: its key, id and arrays by parameter.
struct Made {
    key: Key,
    id: FunctionId,
    arrays: Vec<(usize, RValue)>,
}

/// Rewrites the calls of `m` as the module doc says, the copies' locals
/// typed for `-g` as the originals'. Whether anything changed.
pub fn run(m: &mut crate::MirModule) -> bool {
    let crate::MirModule {
        functions, debug, ..
    } = m;
    let mut taken: HashSet<FunctionId> = HashSet::default();
    for s in functions
        .iter()
        .flat_map(|f| &f.blocks)
        .flat_map(|b| &b.stmts)
    {
        if let Statement::Assign {
            rvalue: RValue::FnAddr { callee, .. },
            ..
        } = s
        {
            taken.insert(*callee);
        }
    }
    let fixed: HashMap<FunctionId, Vec<usize>> = functions
        .iter()
        .filter(|f| copyable(f, &taken))
        .map(|f| (f.id, read_only_arrays(f)))
        .filter(|(_, ps)| !ps.is_empty())
        .collect();
    if fixed.is_empty() {
        return false;
    }
    let index: HashMap<FunctionId, usize> = functions
        .iter()
        .enumerate()
        .map(|(i, f)| (f.id, i))
        .collect();
    let mut next_id = functions.iter().map(|f| f.id.0 + 1).max().unwrap_or(0);
    // Key -> copy; the copies to build, with their arrays by parameter.
    let mut copies: HashMap<Key, FunctionId> = HashMap::default();
    let mut order: Vec<Made> = Vec::new();
    let mut count: HashMap<FunctionId, usize> = HashMap::default();
    for f in functions.iter_mut() {
        let mut rewrites: Vec<(usize, usize, FunctionId, Vec<usize>)> = Vec::new();
        {
            let consts = const_arrays(f);
            for (bi, b) in f.blocks.iter().enumerate() {
                for (si, s) in b.stmts.iter().enumerate() {
                    let Statement::Assign {
                        rvalue: RValue::Call { callee, args, .. },
                        ..
                    } = s
                    else {
                        continue;
                    };
                    // A call of the function itself keeps its arguments.
                    let Some(ps) = fixed.get(callee).filter(|_| *callee != f.id) else {
                        continue;
                    };
                    let hits: Vec<(usize, &RValue)> = ps
                        .iter()
                        .filter_map(|&p| match args.get(p)? {
                            Operand::Copy(l) => Some((p, *consts.get(l)?)),
                            Operand::Const(_) => None,
                        })
                        .collect();
                    if hits.is_empty() {
                        continue;
                    }
                    let key: Key = (
                        *callee,
                        hits.iter().map(|(p, r)| (*p, format!("{r:?}"))).collect(),
                    );
                    let id = if let Some(id) = copies.get(&key) {
                        *id
                    } else {
                        let n = count.entry(*callee).or_default();
                        if *n >= MAX_COPIES {
                            continue;
                        }
                        *n += 1;
                        let id = FunctionId(next_id);
                        next_id += 1;
                        copies.insert(key.clone(), id);
                        let arrays = hits.iter().map(|(p, r)| (*p, (*r).clone())).collect();
                        order.push(Made { key, id, arrays });
                        id
                    };
                    rewrites.push((bi, si, id, hits.iter().map(|(p, _)| *p).collect()));
                }
            }
        }
        for (bi, si, id, dropped) in rewrites {
            if let Statement::Assign {
                rvalue: RValue::Call { callee, args, .. },
                ..
            } = &mut f.blocks[bi].stmts[si]
            {
                *callee = id;
                let mut i = 0;
                args.retain(|_| {
                    i += 1;
                    !dropped.contains(&(i - 1))
                });
            }
        }
    }
    let made = !order.is_empty();
    for Made {
        key: (orig, consts),
        id,
        arrays,
    } in order
    {
        let mut copy = functions[index[&orig]].clone();
        copy.id = id;
        let text = consts.iter().fold(String::new(), |mut t, (p, r)| {
            let _ = write!(t, "{p}={r};");
            t
        });
        let tag = format!(
            "{}|tables={}",
            copy.facts.mono_args.take().unwrap_or_default(),
            &hex(&sha256(text.as_bytes()))[..16]
        );
        copy.name = format!("{}{MONO_MARK}{tag}", copy.path());
        copy.facts.mono_args = Some(tag);
        // The arrays built at entry, in parameter order; the parameters go.
        let mut init: Vec<Statement> = Vec::new();
        for (p, r) in &arrays {
            init.push(Statement::Assign {
                target: copy.params[*p],
                rvalue: r.clone(),
            });
        }
        copy.blocks[0].stmts.splice(0..0, init);
        let gone: Vec<usize> = arrays.iter().map(|(p, _)| *p).collect();
        let keep = |i: &usize| !gone.contains(i);
        copy.params = (0..copy.params.len())
            .filter(keep)
            .map(|i| copy.params[i])
            .collect();
        copy.param_tys = (0..copy.param_tys.len())
            .filter(keep)
            .map(|i| copy.param_tys[i].clone())
            .collect();
        for d in &copy.locals {
            if let Some(t) = debug.locals.get(&(orig, d.id)).cloned() {
                debug.locals.insert((id, d.id), t);
            }
        }
        functions.push(copy);
    }
    made
}
