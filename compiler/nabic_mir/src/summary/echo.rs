//! Results that hand a parameter back (a `ref` parameter's value): of
//! that parameter, the fields the body may change. The rest come back
//! as they went in, so a caller writes back only the changed ones.
//!
//! A flow-insensitive fixed point over what each local may hold: a
//! parameter but for some fields, a field of a parameter as passed, a
//! tuple of these, or anything. A local's value joins every assignment
//! of it, so a field read is the parameter's own only when no
//! assignment of the local changed that field.

use crate::{MirFunction, MirModule, MirTy, Operand, RValue, Statement, Terminator};
use nabic_core::HashMap;
use nabic_ir_common::{Abi, BlockId, Echo, LocalId};

/// What a local may hold.
#[derive(Clone, Debug, PartialEq, Eq)]
enum V {
    /// Parameter `j` but for the fields in the mask (`bit`).
    Param(u32, u64),
    /// Field `i` of parameter `j` as passed.
    Field(u32, u32),
    /// A tuple of these.
    Pack(Vec<V>),
    /// Anything else.
    Top,
}

/// Field `i`'s bit in a mask: fields from 63 on share the last.
const fn bit(i: u32) -> u64 {
    1 << if i < 63 { i } else { 63 }
}

fn join(x: &V, y: &V) -> V {
    match (x, y) {
        (V::Param(at, m), V::Param(other, n)) if at == other => V::Param(*at, m | n),
        (V::Field(..), V::Field(..)) if x == y => x.clone(),
        (V::Pack(xs), V::Pack(ys)) if xs.len() == ys.len() => {
            V::Pack(xs.iter().zip(ys).map(|(a, b)| join(a, b)).collect())
        }
        _ => V::Top,
    }
}

fn aggregate(t: &MirTy) -> bool {
    matches!(t, MirTy::Struct(id, _) if *id != crate::ANY_STRUCT) || matches!(t, MirTy::Tuple(_))
}

struct Run<'a> {
    f: &'a MirFunction,
    /// Block position by id.
    at: HashMap<u32, usize>,
    /// Absent: nothing assigned yet.
    vals: HashMap<LocalId, V>,
    changed: bool,
    /// An assignment read a local nothing assigned yet.
    pending: bool,
    /// Last round: such an assignment makes its local `Top`.
    settle: bool,
}

impl Run<'_> {
    fn op(&self, o: &Operand) -> Option<V> {
        match o {
            Operand::Copy(l) => self.vals.get(l).cloned(),
            Operand::Const(_) => Some(V::Top),
        }
    }

    fn eval(&self, rv: &RValue) -> Option<V> {
        let f = self.f;
        Some(match rv {
            RValue::Use(o) => self.op(o)?,
            RValue::FieldRead { target, index, .. } => match self.op(target)? {
                V::Param(j, s) if *index < 63 && s & bit(*index) == 0 => V::Field(j, *index),
                V::Pack(v) => v.get(*index as usize).cloned().unwrap_or(V::Top),
                _ => V::Top,
            },
            RValue::WithField {
                source,
                index,
                value,
                ..
            } => match self.op(source)? {
                V::Param(j, s) => {
                    let same = self.op(value)? == V::Field(j, *index);
                    V::Param(j, if same { s } else { s | bit(*index) })
                }
                _ => V::Top,
            },
            RValue::StructInit { ty, fields } if *ty == f.ret_ty => {
                V::Pack(fields.iter().map(|o| self.op(o)).collect::<Option<_>>()?)
            }
            RValue::StructInit { ty, fields } => {
                let Some(j) = f.param_tys.iter().position(|p| p == ty) else {
                    return Some(V::Top);
                };
                let j = j as u32;
                let mut s = 0;
                for (i, o) in fields.iter().enumerate() {
                    if self.op(o)? != V::Field(j, i as u32) {
                        s |= bit(i as u32);
                    }
                }
                V::Param(j, s)
            }
            _ => V::Top,
        })
    }

    fn set(&mut self, l: LocalId, v: Option<V>) {
        let v = match v {
            Some(v) => v,
            None if self.settle => V::Top,
            None => {
                self.pending = true;
                return;
            }
        };
        let new = match self.vals.get(&l) {
            Some(old) => join(old, &v),
            None => v,
        };
        if self.vals.get(&l) != Some(&new) {
            self.vals.insert(l, new);
            self.changed = true;
        }
    }

    fn edge(&mut self, to: BlockId, args: &[Operand]) {
        let f = self.f;
        let Some(b) = self.at.get(&to.0).map(|&i| &f.blocks[i]) else {
            return;
        };
        for (p, a) in b.params.iter().zip(args) {
            let v = self.op(a);
            self.set(*p, v);
        }
    }

    fn round(&mut self) {
        let f = self.f;
        for b in &f.blocks {
            for s in &b.stmts {
                match s {
                    Statement::Assign { target, rvalue } => {
                        let v = self.eval(rvalue);
                        self.set(*target, v);
                    }
                    Statement::ArrayWrite { array, .. } => self.set(*array, Some(V::Top)),
                    _ => {}
                }
            }
            match &b.terminator {
                Terminator::Goto { target, args } => self.edge(*target, args),
                Terminator::If {
                    then_bb,
                    then_args,
                    else_bb,
                    else_args,
                    ..
                } => {
                    self.edge(*then_bb, then_args);
                    self.edge(*else_bb, else_args);
                }
                _ => {}
            }
        }
    }
}

/// The parameter `f`'s result hands back, and its fields `f` may change.
#[must_use]
pub fn echo(f: &MirFunction) -> Echo {
    let none = Echo::default();
    let MirTy::Tuple(rets) = &f.ret_ty else {
        return none;
    };
    if f.facts.abi != Abi::Nabi
        || f.blocks.is_empty()
        || !f.pins.is_empty()
        || !rets.iter().any(aggregate)
        || !f.param_tys.iter().any(aggregate)
        || !crate::addressed(f).is_empty()
    {
        return none;
    }
    let mut run = Run {
        f,
        at: f
            .blocks
            .iter()
            .enumerate()
            .map(|(i, b)| (b.id.0, i))
            .collect(),
        vals: HashMap::default(),
        changed: false,
        pending: false,
        settle: false,
    };
    for (j, (p, t)) in f.params.iter().zip(&f.param_tys).enumerate() {
        let v = if aggregate(t) {
            V::Param(j as u32, 0)
        } else {
            V::Top
        };
        run.vals.insert(*p, v);
    }
    // Rounds until nothing changes; then one where a value still
    // unknown is anything, until nothing changes again.
    for _ in 0..64 {
        run.changed = false;
        run.pending = false;
        run.round();
        if !run.changed {
            if !run.pending || run.settle {
                break;
            }
            run.settle = true;
        }
    }
    if run.changed {
        return none;
    }
    let mut got: Option<Vec<V>> = None;
    for b in &f.blocks {
        match &b.terminator {
            Terminator::Return(Some(Operand::Copy(l))) => match (run.vals.get(l), &got) {
                (Some(V::Pack(v)), None) => got = Some(v.clone()),
                (Some(V::Pack(v)), Some(g)) if v.len() == g.len() => {
                    got = Some(g.iter().zip(v).map(|(a, b)| join(a, b)).collect());
                }
                _ => return none,
            },
            Terminator::Return(_) => return none,
            _ => {}
        }
    }
    for (k, v) in got.iter().flatten().enumerate() {
        if let V::Param(j, s) = v {
            if f.param_tys.get(*j as usize) == rets.get(k) {
                return Echo {
                    field: k as u32 + 1,
                    param: *j,
                    changed: *s,
                };
            }
        }
    }
    none
}

/// `echo` of every function of `m` with a body.
pub fn set_echoes(m: &mut MirModule) {
    for f in &mut m.functions {
        f.facts.summary.echo = echo(f);
    }
}
