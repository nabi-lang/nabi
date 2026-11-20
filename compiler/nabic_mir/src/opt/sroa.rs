//! Scalar replacement of aggregates. A struct or tuple local that is
//! built and updated field by field (`StructInit`, `WithField`, a copy)
//! and read by field becomes one local per field: what codegen would
//! keep in a frame slot, copied whole on every update, lives in
//! registers instead.
//!
//! Every assignment of the local assigns its fields; a field read reads
//! the field's local; a block parameter becomes one per field. Where the
//! whole value is still needed (a call argument, a return, a store) it
//! is built again in front (`StructInit`); a value from elsewhere (a
//! call result, a load) is taken apart after (`FieldRead`). A function
//! parameter is taken apart on entry.
//!
//! A struct's array field indexed in place (`s.shape[k]` reads the
//! struct's memory) reads the field's local instead. Left alone:
//! addressed locals, arrays written in place (memory is their identity), `any` values and sums (a tag says what the payload
//! word holds: the collector reads them together), and locals only read
//! by field from a value made elsewhere (nothing to gain).
//!
//! For `-g`, a named local split is bound to its fields' locals
//! (`Bind::parts`): no assignment of it is left.

use crate::{
    Bind, LocalDecl, MirFunction, MirTy, Operand, RValue, Scope, Statement, Target, Terminator,
};
use nabic_core::HashMap;
use nabic_ir_common::{BlockId, ConstVal, LocalId, PrimTy};

/// Wider aggregates stay whole.
const MAX_FIELDS: usize = 16;

/// Splits what it can, a few rounds deep (a field may be an aggregate
/// itself); whether anything changed.
pub fn run(f: &mut MirFunction, t: &Target) -> bool {
    if f.blocks
        .iter()
        .any(|b| matches!(b.terminator, Terminator::Suspend { .. }))
    {
        return false;
    }
    let mut changed = false;
    for _ in 0..3 {
        if !round(f, t) {
            break;
        }
        changed = true;
    }
    changed
}

/// The field types of an aggregate `ty` that may be split; of a short
/// array, its elements.
fn fields(ty: &MirTy) -> Option<Vec<MirTy>> {
    let fs = match ty {
        MirTy::Struct(id, _) if *id == crate::ANY_STRUCT => return None,
        MirTy::Struct(_, fs) | MirTy::Tuple(fs) => fs.clone(),
        MirTy::Array(e, n) if (*n as usize) <= MAX_FIELDS => vec![(**e).clone(); *n as usize],
        _ => return None,
    };
    let ok = !fs.is_empty()
        && fs.len() <= MAX_FIELDS
        && !fs.iter().any(|t| matches!(t, MirTy::Union(..)));
    ok.then_some(fs)
}

/// A vector value (`core::simd`'s shape: a struct of one array of
/// numbers, whole registers of them): its operations take it whole, in
/// a vector register where codegen has them, so it stays one.
fn vector(ty: &MirTy, t: &Target) -> bool {
    let MirTy::Struct(_, fs) = ty else {
        return false;
    };
    let [MirTy::Array(e, n)] = fs.as_slice() else {
        return false;
    };
    let MirTy::Prim(p) = **e else { return false };
    let bytes = t.size_align(e).0 * n;
    p != PrimTy::Bool && (p.is_int() || p.is_float()) && bytes > 0 && bytes % 16 == 0
}

/// Integer locals assigned once, a constant (what an unrolled `for k in
/// 0..N` leaves as indices before cleanup folds them in).
type Consts = HashMap<LocalId, i128>;

fn consts(f: &MirFunction) -> Consts {
    let mut defs: HashMap<LocalId, Option<i128>> = HashMap::default();
    let int = |o: &Operand| match o {
        Operand::Const(ConstVal::Int { value, .. }) => Some(*value),
        Operand::Const(ConstVal::UInt { value, .. }) => i128::try_from(*value).ok(),
        _ => None,
    };
    for b in &f.blocks {
        for p in &b.params {
            defs.insert(*p, None);
        }
        for s in &b.stmts {
            if let Some(d) = crate::liveness::stmt_uses(s).1 {
                let v = match s {
                    Statement::Assign {
                        rvalue: RValue::Use(o),
                        ..
                    } => int(o),
                    _ => None,
                };
                defs.entry(d).and_modify(|e| *e = None).or_insert(v);
            }
        }
    }
    for p in &f.params {
        defs.insert(*p, None);
    }
    let addr = crate::addressed(f);
    defs.into_iter()
        .filter_map(|(l, v)| Some((l, v?)))
        .filter(|(l, _)| !addr.contains(l))
        .collect()
}

/// Element `index` of an array of `len`, a constant in bounds: what an
/// array split reads by field.
fn const_elem(index: &Operand, len: u32, cs: &Consts) -> Option<usize> {
    let v = match index {
        Operand::Const(ConstVal::Int { value, .. }) => *value,
        Operand::Const(ConstVal::UInt { value, .. }) => i128::try_from(*value).ok()?,
        Operand::Copy(l) => *cs.get(l)?,
        Operand::Const(_) => return None,
    };
    (0..i128::from(len))
        .contains(&v)
        .then(|| usize::try_from(v).ok())
        .flatten()
}

/// `array[k]` of an array local, `k` a constant: field `k`.
const fn elem_read(array: LocalId, k: usize, elem_ty: MirTy, len: u32) -> RValue {
    RValue::ArrayIndex {
        array,
        offset: 0,
        index: Operand::Const(ConstVal::UInt {
            value: k as u128,
            ty: PrimTy::Usize,
        }),
        elem_ty,
        len,
    }
}

/// Per local, what the round learns of it.
#[derive(Clone, Copy, Default)]
struct Use {
    /// Reads of a field, updates of one.
    by_field: u32,
    /// Assignments of a whole value split here as well (a build, an
    /// update, a copy, a block parameter).
    built: u32,
    bad: bool,
}

/// The field of aggregate `ty` an in-place array read at byte `offset`
/// of `len` elements of `elem` reads whole: its index.
fn array_field(t: &Target, ty: &MirTy, offset: u32, elem: &MirTy, len: u32) -> Option<usize> {
    let fs = fields(ty).filter(|_| !matches!(ty, MirTy::Array(..)))?;
    let k = t.field_offsets(ty).iter().position(|o| *o == offset)?;
    matches!(&fs[k], MirTy::Array(e, n) if **e == *elem && *n == len).then_some(k)
}

/// The locals of `f` its memory identifies: addressed, or arrays written
/// or read at computed indices.
fn held(f: &MirFunction, t: &Target) -> nabic_core::HashSet<LocalId> {
    let cs = consts(f);
    // In-place reads of an array field count as field reads: not what
    // makes a local addressed here; nor do an array's elements at
    // constants, read or written.
    let mut pinned = crate::addressed_by(f, |s| {
        !matches!(s, Statement::Assign {
            rvalue: RValue::ArrayIndex { array, offset, elem_ty, len, .. },
            ..
        } if array_field(t, &f.locals[array.0 as usize].ty, *offset, elem_ty, *len).is_some())
    });
    for s in f.blocks.iter().flat_map(|b| &b.stmts) {
        match s {
            Statement::ArrayWrite {
                array,
                offset: 0,
                index,
                len,
                ..
            } if matches!(f.locals[array.0 as usize].ty, MirTy::Array(..))
                && const_elem(index, *len, &cs).is_some() => {}
            Statement::ArrayWrite { array, .. } => {
                pinned.insert(*array);
            }
            // A whole array's elements by a computed index: memory.
            Statement::Assign {
                rvalue:
                    RValue::ArrayIndex {
                        array,
                        offset,
                        index,
                        len,
                        ..
                    },
                ..
            } if matches!(f.locals[array.0 as usize].ty, MirTy::Array(..))
                && (*offset != 0 || const_elem(index, *len, &cs).is_none()) =>
            {
                pinned.insert(*array);
            }
            _ => {}
        }
    }
    pinned
}

/// The locals worth splitting.
fn candidates(f: &MirFunction, t: &Target) -> Vec<bool> {
    let n = f.locals.len();
    // Nothing read by field: nothing to split.
    let any = f.blocks.iter().flat_map(|b| &b.stmts).any(|s| {
        matches!(
            s,
            Statement::Assign {
                rvalue: RValue::FieldRead { .. }
                    | RValue::WithField { .. }
                    | RValue::ArrayIndex { .. },
                ..
            }
        )
    });
    if !any {
        return vec![false; n];
    }
    let pinned = held(f, t);
    let params: HashMap<u32, &[LocalId]> = f
        .blocks
        .iter()
        .map(|b| (b.id.0, b.params.as_slice()))
        .collect();
    let mut u = vec![Use::default(); n];
    for (i, d) in f.locals.iter().enumerate() {
        if fields(&d.ty).is_none() || vector(&d.ty, t) {
            u[i].bad = true;
        }
    }
    for p in &f.params {
        if matches!(f.locals[p.0 as usize].ty, MirTy::Array(..)) {
            u[p.0 as usize].built += 1;
        }
    }
    for l in pinned {
        u[l.0 as usize].bad = true;
    }
    let ty = |l: &LocalId| &f.locals[l.0 as usize].ty;
    // Fields move between values of one type only: a constant or a value
    // of another type has none to give.
    let fits = |o: &Operand, t: &MirTy| matches!(o, Operand::Copy(y) if ty(y) == t);
    for b in &f.blocks {
        for p in &b.params {
            u[p.0 as usize].built += 1;
        }
        for s in &b.stmts {
            if let Statement::ArrayWrite { array, .. } = s {
                u[array.0 as usize].by_field += 1;
                continue;
            }
            let Statement::Assign { target, rvalue } = s else {
                continue;
            };
            let t = target.0 as usize;
            let tt = &f.locals[t].ty;
            match rvalue {
                RValue::ArrayIndex { array, .. } => u[array.0 as usize].by_field += 1,
                RValue::ArrayInit { elem_ty, elements } => {
                    u[t].built += 1;
                    u[t].bad |= !matches!(tt, MirTy::Array(e, n) if **e == *elem_ty && *n as usize == elements.len());
                }
                RValue::FieldRead {
                    target: Operand::Copy(x),
                    index,
                    field_ty,
                } => {
                    let x = x.0 as usize;
                    u[x].by_field += 1;
                    let same = fields(&f.locals[x].ty)
                        .and_then(|fs| fs.get(*index as usize).cloned())
                        .is_some_and(|ft| ft == *field_ty);
                    u[x].bad |= !same;
                }
                RValue::StructInit { ty: st, .. } => {
                    u[t].built += 1;
                    u[t].bad |= st != tt;
                }
                RValue::Use(o) => {
                    u[t].built += 1;
                    u[t].bad |= !fits(o, tt);
                }
                RValue::WithField {
                    source,
                    struct_ty,
                    index,
                    ..
                } => {
                    u[t].built += 1;
                    u[t].by_field += 1;
                    u[t].bad |= !fits(source, tt)
                        || struct_ty != tt
                        || fields(tt).is_none_or(|fs| *index as usize >= fs.len());
                    if let Operand::Copy(x) = source {
                        u[x.0 as usize].by_field += 1;
                    }
                }
                _ => {}
            }
            // An array from elsewhere taken apart: its constant elements
            // are then values of their own.
            if matches!(tt, MirTy::Array(..))
                && !matches!(rvalue, RValue::ArrayInit { .. } | RValue::Use(_))
            {
                u[t].built += 1;
            }
        }
        let mut edge = |to: &BlockId, args: &[Operand]| {
            if let Some(ps) = params.get(&to.0) {
                for (p, a) in ps.iter().zip(args) {
                    if !fits(a, ty(p)) {
                        u[p.0 as usize].bad = true;
                    }
                }
            }
        };
        match &b.terminator {
            Terminator::Goto { target, args } => edge(target, args),
            Terminator::If {
                then_bb,
                then_args,
                else_bb,
                else_args,
                ..
            } => {
                edge(then_bb, then_args);
                edge(else_bb, else_args);
            }
            _ => {}
        }
    }
    u.iter()
        .map(|x| !x.bad && x.by_field > 0 && x.built > 0)
        .collect()
}

struct Rw<'a> {
    t: &'a Target,
    /// Per local: its field locals, if split.
    split: Vec<Option<Vec<LocalId>>>,
    /// Every local's type, new ones appended.
    locals: Vec<LocalDecl>,
    /// The local a split local is built again in, per split local: used
    /// right after it is assigned, one will do for every site.
    wholes: HashMap<LocalId, LocalId>,
    cs: Consts,
}

impl Rw<'_> {
    fn fresh(&mut self, ty: MirTy) -> LocalId {
        let id = LocalId(self.locals.len() as u32);
        self.locals.push(LocalDecl { id, name: None, ty });
        id
    }

    fn ty(&self, l: LocalId) -> &MirTy {
        &self.locals[l.0 as usize].ty
    }

    fn parts(&self, l: LocalId) -> Option<&Vec<LocalId>> {
        self.split[l.0 as usize].as_ref()
    }

    /// `o` whole: a split local built again in front (`pre`).
    fn whole(&mut self, o: &mut Operand, pre: &mut Vec<Statement>) {
        let Operand::Copy(l) = o else { return };
        let Some(ps) = self.parts(*l).cloned() else {
            return;
        };
        let ty = self.ty(*l).clone();
        let m = if let Some(&m) = self.wholes.get(l) {
            m
        } else {
            let m = self.fresh(ty.clone());
            self.wholes.insert(*l, m);
            m
        };
        let parts = ps.into_iter().map(Operand::Copy).collect();
        let rvalue = match ty {
            MirTy::Array(e, _) => RValue::ArrayInit {
                elem_ty: *e,
                elements: parts,
            },
            ty => RValue::StructInit { ty, fields: parts },
        };
        pre.push(Statement::Assign { target: m, rvalue });
        *o = Operand::Copy(m);
    }

    /// The read of field `i` of the whole aggregate `l`.
    fn part(&self, l: LocalId, i: usize) -> RValue {
        let ty = self.ty(l);
        let fty = fields(ty).expect("aggregate")[i].clone();
        match ty {
            MirTy::Array(_, n) => elem_read(l, i, fty, *n),
            _ => RValue::FieldRead {
                target: Operand::Copy(l),
                index: i as u32,
                field_ty: fty,
            },
        }
    }

    /// Field `i` of aggregate `o` (never a constant: `candidates`).
    fn field(&mut self, o: &Operand, i: usize, pre: &mut Vec<Statement>) -> Operand {
        let Operand::Copy(l) = o else {
            unreachable!("constant aggregate split")
        };
        if let Some(ps) = self.parts(*l) {
            return Operand::Copy(ps[i]);
        }
        let fty = fields(self.ty(*l)).expect("aggregate")[i].clone();
        let x = self.fresh(fty);
        pre.push(Statement::Assign {
            target: x,
            rvalue: self.part(*l, i),
        });
        Operand::Copy(x)
    }

    /// The statements `s` becomes.
    fn stmt(&mut self, s: Statement, out: &mut Vec<Statement>) {
        let mut pre = Vec::new();
        match s {
            Statement::Assign { target, rvalue } => self.assign(target, rvalue, out),
            Statement::ArrayWrite {
                array,
                index,
                mut value,
                len,
                ..
            } if self.parts(array).is_some() => {
                let k = const_elem(&index, len, &self.cs).expect("a constant element");
                self.whole(&mut value, &mut pre);
                out.append(&mut pre);
                let l = self.parts(array).expect("split")[k];
                out.push(Statement::Assign {
                    target: l,
                    rvalue: RValue::Use(value),
                });
            }
            Statement::StorageDead(l) => match self.parts(l).cloned() {
                Some(ps) => {
                    for p in ps {
                        if self.t.has_gc_ref(self.ty(p)) {
                            out.push(Statement::StorageDead(p));
                        }
                    }
                }
                None => out.push(Statement::StorageDead(l)),
            },
            mut s => {
                crate::opt::stmt_ops_mut(&mut s, &mut |o| self.whole(o, &mut pre));
                out.append(&mut pre);
                out.push(s);
            }
        }
    }

    fn assign(&mut self, target: LocalId, mut rvalue: RValue, out: &mut Vec<Statement>) {
        let mut pre = Vec::new();
        // A field of a split local is that field's local.
        if let RValue::FieldRead {
            target: Operand::Copy(x),
            index,
            ..
        } = &rvalue
        {
            if let Some(ps) = self.parts(*x) {
                rvalue = RValue::Use(Operand::Copy(ps[*index as usize]));
            }
        }
        if let RValue::ArrayIndex {
            array,
            offset,
            index,
            elem_ty,
            len,
        } = &mut rvalue
        {
            if let Some(ps) = self.parts(*array).cloned() {
                if matches!(self.ty(*array), MirTy::Array(..)) {
                    let k = const_elem(index, *len, &self.cs).expect("a constant element");
                    rvalue = RValue::Use(Operand::Copy(ps[k]));
                } else {
                    let k = array_field(self.t, self.ty(*array), *offset, elem_ty, *len)
                        .expect("an array field read in place");
                    *array = ps[k];
                    *offset = 0;
                }
            }
        }
        let Some(ts) = self.parts(target).cloned() else {
            crate::opt::rvalue_ops_mut(&mut rvalue, &mut |o| self.whole(o, &mut pre));
            out.append(&mut pre);
            out.push(Statement::Assign { target, rvalue });
            return;
        };
        let set = |out: &mut Vec<Statement>, l: LocalId, o: Operand| {
            out.push(Statement::Assign {
                target: l,
                rvalue: RValue::Use(o),
            });
        };
        match rvalue {
            RValue::StructInit { mut fields, .. }
            | RValue::ArrayInit {
                elements: mut fields,
                ..
            } => {
                for o in &mut fields {
                    self.whole(o, &mut pre);
                }
                out.append(&mut pre);
                for (l, o) in ts.into_iter().zip(fields) {
                    set(out, l, o);
                }
            }
            RValue::Use(o) => {
                if o == Operand::Copy(target) {
                    return;
                }
                let vs: Vec<Operand> = (0..ts.len()).map(|i| self.field(&o, i, &mut pre)).collect();
                out.append(&mut pre);
                for (l, v) in ts.into_iter().zip(vs) {
                    set(out, l, v);
                }
            }
            RValue::WithField {
                source,
                index,
                mut value,
                ..
            } => {
                self.whole(&mut value, &mut pre);
                let k = index as usize;
                let same = source == Operand::Copy(target);
                let mut vs = Vec::new();
                for i in 0..ts.len() {
                    if i != k && !same {
                        vs.push((i, self.field(&source, i, &mut pre)));
                    }
                }
                out.append(&mut pre);
                for (i, v) in vs {
                    set(out, ts[i], v);
                }
                set(out, ts[k], value);
            }
            mut rv => {
                crate::opt::rvalue_ops_mut(&mut rv, &mut |o| self.whole(o, &mut pre));
                out.append(&mut pre);
                let ty = self.ty(target).clone();
                let tmp = self.fresh(ty);
                out.push(Statement::Assign {
                    target: tmp,
                    rvalue: rv,
                });
                for (i, l) in ts.into_iter().enumerate() {
                    out.push(Statement::Assign {
                        target: l,
                        rvalue: self.part(tmp, i),
                    });
                }
            }
        }
    }

    /// Arguments `args` to a block of parameters `params` (as they were).
    fn args(&mut self, params: &[LocalId], args: &mut Vec<Operand>, pre: &mut Vec<Statement>) {
        let old = std::mem::take(args);
        for (p, mut a) in params.iter().zip(old) {
            if let Some(n) = self.parts(*p).map(Vec::len) {
                for i in 0..n {
                    let v = self.field(&a, i, pre);
                    args.push(v);
                }
            } else {
                self.whole(&mut a, pre);
                args.push(a);
            }
        }
    }
}

/// One round: split today's candidates.
fn round(f: &mut MirFunction, t: &Target) -> bool {
    let cand = candidates(f, t);
    if !cand.contains(&true) {
        return false;
    }
    let cs = consts(f);
    let mut rw = Rw {
        t,
        split: vec![None; f.locals.len()],
        locals: std::mem::take(&mut f.locals),
        wholes: HashMap::default(),
        cs,
    };
    for (i, c) in cand.iter().enumerate() {
        if !c {
            continue;
        }
        let fts = fields(&rw.locals[i].ty).expect("aggregate");
        let ps = fts.into_iter().map(|ty| rw.fresh(ty)).collect();
        rw.split[i] = Some(ps);
    }
    rw.split.resize(rw.locals.len(), None);
    bind(f, &rw);
    let params: HashMap<u32, Vec<LocalId>> = f
        .blocks
        .iter()
        .map(|b| (b.id.0, b.params.clone()))
        .collect();
    for b in &mut f.blocks {
        let old = std::mem::take(&mut b.stmts);
        let mut stmts = Vec::with_capacity(old.len());
        for s in old {
            rw.stmt(s, &mut stmts);
        }
        let mut pre = Vec::new();
        match &mut b.terminator {
            Terminator::Goto { target, args } => rw.args(&params[&target.0], args, &mut pre),
            Terminator::If {
                then_bb,
                then_args,
                else_bb,
                else_args,
                ..
            } => {
                rw.args(&params[&then_bb.0], then_args, &mut pre);
                rw.args(&params[&else_bb.0], else_args, &mut pre);
            }
            t => crate::opt::term_ops_mut(t, &mut |o| rw.whole(o, &mut pre)),
        }
        stmts.append(&mut pre);
        b.stmts = stmts;
        b.params = std::mem::take(&mut b.params)
            .into_iter()
            .flat_map(|p| rw.parts(p).cloned().unwrap_or_else(|| vec![p]))
            .collect();
    }
    // Parameters arrive whole: taken apart on entry.
    let mut entry = Vec::new();
    for &p in &f.params {
        let Some(ps) = rw.parts(p).cloned() else {
            continue;
        };
        for (i, l) in ps.into_iter().enumerate() {
            entry.push(Statement::Assign {
                target: l,
                rvalue: rw.part(p, i),
            });
        }
    }
    f.locals = rw.locals;
    if !entry.is_empty() {
        let has_preds = f
            .blocks
            .iter()
            .any(|b| crate::opt::succs(&b.terminator).contains(&f.blocks[0].id));
        if has_preds {
            let id = crate::opt::fresh_block(f);
            let to = f.blocks[0].id;
            f.blocks.insert(
                0,
                crate::opt::empty_block(
                    id,
                    Terminator::Goto {
                        target: to,
                        args: Vec::new(),
                    },
                ),
            );
        }
        let b = &mut f.blocks[0];
        entry.append(&mut b.stmts);
        b.stmts = entry;
    }
    true
}

/// `-g` of the locals `rw` splits: a named one is its fields' locals, from
/// where it was first assigned (a parameter: from the entry, where it is
/// taken apart); a bind to one is to its fields, a part that is one is
/// gone (parts are not nested).
fn bind(f: &mut MirFunction, rw: &Rw<'_>) {
    let parts = |l: LocalId| -> Vec<Option<Operand>> {
        rw.parts(l)
            .map(|ps| ps.iter().map(|p| Some(Operand::Copy(*p))).collect())
            .unwrap_or_default()
    };
    for b in &mut f.dbg.binds {
        if let Some(Operand::Copy(l)) = b.value {
            if rw.parts(l).is_some() {
                b.value = None;
                b.parts = parts(l);
            }
        }
        for p in &mut b.parts {
            if matches!(p, Some(Operand::Copy(l)) if rw.parts(*l).is_some()) {
                *p = None;
            }
        }
    }
    let named = |l: LocalId| {
        rw.parts(l).is_some()
            && rw.locals[l.0 as usize]
                .name
                .as_deref()
                .is_some_and(crate::debug_visible)
    };
    let mut at = (f.span.line, Scope::default());
    let mut first: HashMap<LocalId, (u32, Scope)> = HashMap::default();
    for &p in f.params.iter().filter(|p| named(**p)) {
        first.insert(p, at);
    }
    for b in &f.blocks {
        for p in b.params.iter().filter(|p| named(**p)) {
            first.entry(*p).or_insert(at);
        }
        for s in &b.stmts {
            match s {
                Statement::Loc(sp, sc) => at = (sp.line, *sc),
                Statement::Assign { target, .. } if named(*target) => {
                    first.entry(*target).or_insert(at);
                }
                _ => {}
            }
        }
    }
    let mut ls: Vec<(LocalId, (u32, Scope))> = first.into_iter().collect();
    ls.sort_unstable_by_key(|(l, _)| l.0);
    for (l, (line, scope)) in ls {
        f.dbg.binds.retain(|b| b.local != l);
        f.dbg.binds.push(Bind {
            local: l,
            value: None,
            parts: parts(l),
            line,
            scope,
        });
    }
}
