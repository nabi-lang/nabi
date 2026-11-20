//! Small structs at call boundaries, above `-O0`. Scalar replacement
//! leaves a struct whole only where a call or a return takes it: built
//! (`StructInit`) right in front, taken apart (`FieldRead`) right after.
//! Stored whole, it would go through a frame slot on its way to or from
//! the registers. Instead:
//!
//! - a struct built for one call argument or the return value, and read
//!   by nothing else, is never stored: its fields go to their argument /
//!   result registers from where they are;
//! - a call result read only by field right after the call goes from the
//!   result registers to the locals reading it;
//! - a parameter read only by field at the top of the entry block goes
//!   from its argument registers to the locals reading it.
//!
//! A struct qualifies when the leaves it passes in are its fields'
//! leaves, and it is not a GC root: a root's slot never written would be
//! free for the frame to share, and a clear of it would hit another
//! root's value. Between a build and its call nothing may write the
//! fields' locals, clear them, or (for a struct holding references)
//! reach a safepoint, since only the fields' locals hold the values
//! until the call.

use crate::cc;
use nabic_core::{HashMap, HashSet};
use nabic_ir_common::{FunctionId, LocalId};
use nabic_mir::{MirFunction, MirTy, Operand, RValue, Statement, Target, Terminator};

/// Statements by `(block position, statement)`.
pub type At = (usize, usize);

#[derive(Default)]
pub struct Plan {
    /// Builds whose struct is never stored.
    pub inits: HashSet<At>,
    /// Calls whose result goes to the locals reading it: per field, the
    /// local (`None`: not read).
    pub rets: HashMap<At, Vec<Option<LocalId>>>,
    /// Parameters going to the locals reading them: per leaf, its
    /// offset, the local and the offset there.
    pub params: HashMap<LocalId, Vec<(u32, LocalId, u32)>>,
    /// Field reads done by the call or the parameter's arrival.
    pub skip: HashSet<At>,
}

/// The fields of `ty`, their types and offsets, when the leaves it
/// passes in are its fields' leaves, each in one field.
pub fn flat(ty: &MirTy, t: &Target) -> Option<Vec<(MirTy, u32)>> {
    let fs = match ty {
        MirTy::Struct(id, _) if *id == nabic_mir::ANY_STRUCT => return None,
        MirTy::Struct(_, fs) | MirTy::Tuple(fs) => fs,
        _ => return None,
    };
    if fs.is_empty() || fs.len() > MAX_FIELDS {
        return None;
    }
    let offs = t.field_offsets(ty);
    let sh = cc::shape(ty, t);
    if offs.len() != fs.len() || sh.byval {
        return None;
    }
    let mut n = 0;
    for (fty, off) in fs.iter().zip(&offs) {
        let size = t.size_align(fty).0;
        let inner: Vec<(u32, crate::lir::K)> = sh
            .leaves
            .iter()
            .filter(|l| l.off >= *off && l.off < off + size)
            .map(|l| (l.off - off, l.k))
            .collect();
        let own: Vec<(u32, crate::lir::K)> = leaves(fty, t).iter().map(|l| (l.off, l.k)).collect();
        if inner != own {
            return None;
        }
        n += own.len();
    }
    (n == sh.leaves.len()).then(|| fs.iter().cloned().zip(offs).collect())
}

/// Wider structs stay whole.
const MAX_FIELDS: usize = 16;

/// Statements a build's reader is looked for in.
const SCAN: usize = 32;

/// The leaves of a field of type `ty`, from its start.
pub fn leaves(ty: &MirTy, t: &Target) -> Vec<cc::Leaf> {
    let mut out = Vec::new();
    cc::leaves(ty, t, 0, &mut out);
    out
}

/// Per local: how often it is read and assigned.
struct Counts {
    reads: Vec<u32>,
    defs: Vec<u32>,
}

fn counts(f: &MirFunction) -> Counts {
    let n = f.locals.len();
    let mut c = Counts {
        reads: vec![0; n],
        defs: vec![0; n],
    };
    let bump = |v: &mut Vec<u32>, l: LocalId| {
        if let Some(x) = v.get_mut(l.0 as usize) {
            *x += 1;
        }
    };
    for p in &f.params {
        bump(&mut c.defs, *p);
    }
    for b in &f.blocks {
        for p in &b.params {
            bump(&mut c.defs, *p);
        }
        for s in &b.stmts {
            nabic_mir::stmt_reads(s, &mut |l| bump(&mut c.reads, l));
            if let Some(l) = written(s) {
                bump(&mut c.defs, l);
            }
        }
        nabic_mir::term_reads(&b.terminator, &mut |l| bump(&mut c.reads, l));
    }
    c
}

/// The local `s` writes, if any.
const fn written(s: &Statement) -> Option<LocalId> {
    match s {
        Statement::Assign { target, .. } => Some(*target),
        Statement::ArrayWrite { array: l, .. } | Statement::StorageDead(l) => Some(*l),
        _ => None,
    }
}

/// The parameters of `f` read only by field at the top of its entry
/// block; with an entry poll, only those holding no reference. A root
/// among the locals reading them is not cleared on entry: it holds its
/// part by then.
pub fn params(
    f: &MirFunction,
    t: &Target,
    addressed: &HashSet<LocalId>,
    by_ref: impl Fn(&MirTy) -> bool,
    entry_poll: bool,
) -> Plan {
    let mut out = Plan::default();
    let Some(entry) = f.blocks.first() else {
        return out;
    };
    let id = entry.id;
    if f.blocks.iter().any(|b| nabic_mir::succs(b).contains(&id.0)) {
        return out;
    }
    let ty = |l: LocalId| &f.locals[l.0 as usize].ty;
    let shapes: HashMap<LocalId, Vec<(MirTy, u32)>> = f
        .params
        .iter()
        .filter(|p| !addressed.contains(p) && !by_ref(ty(**p)))
        // One holding references is a root from the entry: it is written
        // too (`FnLower::lower`), and an entry poll would collect while
        // only it is seen.
        .filter(|p| !entry_poll || t.root_offsets(ty(**p)).is_empty())
        .filter_map(|p| Some((*p, flat(ty(*p), t)?)))
        .collect();
    if shapes.is_empty() {
        return out;
    }
    let c = counts(f);
    let mut reads: Vec<Read> = Vec::new();
    let mut inner: HashMap<LocalId, (LocalId, u32)> = HashMap::default();
    let mut seen = HashSet::default();
    for (i, s) in entry.stmts.iter().enumerate() {
        let Statement::Assign {
            target,
            rvalue:
                RValue::FieldRead {
                    target: Operand::Copy(src),
                    index,
                    ..
                },
        } = s
        else {
            if matches!(s, Statement::Loc(..)) {
                continue;
            }
            break;
        };
        let (p, base, fs) = if let Some(fs) = shapes.get(src) {
            (*src, 0, fs.clone())
        } else if let Some(&(p, base)) = inner.get(src) {
            let Some(fs) = flat(ty(*src), t) else { break };
            (p, base, fs)
        } else {
            break;
        };
        let x = *target;
        let Some((fty, foff)) = fs.get(*index as usize) else {
            break;
        };
        if fty != ty(x) || !seen.insert(x) || f.params.contains(&x) || addressed.contains(&x) {
            break;
        }
        if c.defs[x.0 as usize] == 1 && flat(fty, t).is_some() {
            inner.insert(x, (p, base + foff));
        }
        reads.push(Read {
            at: i,
            from: *src,
            x,
            p,
            off: base + foff,
        });
    }
    let n_from = |l: LocalId| reads.iter().filter(|r| r.from == l).count();
    // A struct read whose fields are all read here passes them on: its
    // own slot is never written.
    let through =
        |x: LocalId| inner.contains_key(&x) && c.reads[x.0 as usize] as usize == n_from(x);
    for &p in shapes.keys() {
        if c.defs[p.0 as usize] != 1
            || c.reads[p.0 as usize] as usize != n_from(p)
            || n_from(p) == 0
        {
            continue;
        }
        // The reads of `p`'s parts: from `p`, or from a struct passing
        // its fields on.
        // In order: a struct's own read comes before those of its fields.
        let mut srcs = vec![p];
        let mut to = Vec::new();
        for r in reads.iter().filter(|r| r.p == p) {
            if !srcs.contains(&r.from) {
                continue;
            }
            out.skip.insert((0, r.at));
            if through(r.x) {
                srcs.push(r.x);
                continue;
            }
            for l in leaves(ty(r.x), t) {
                to.push((r.off + l.off, r.x, l.off));
            }
        }
        out.params.insert(p, to);
    }
    out
}

/// A field read at the top of the entry block: its statement, the
/// struct read, the local reading it, and the parameter and offset
/// there it comes from.
struct Read {
    at: usize,
    from: LocalId,
    x: LocalId,
    p: LocalId,
    off: u32,
}

/// What the plan needs of the function being lowered.
pub struct Facts<'x> {
    pub t: &'x Target,
    pub addressed: &'x HashSet<LocalId>,
    pub rooted: &'x HashSet<u32>,
    pub deaths: &'x nabic_mir::Deaths,
    /// Whether argument `n` of a call of the function goes in registers
    /// as its parts (a nabi-ABI call, by value).
    pub arg_parts: &'x dyn Fn(FunctionId, usize) -> bool,
    /// Whether a call of the function returns its result in registers.
    pub ret_parts: &'x dyn Fn(FunctionId) -> bool,
    /// Whether `f` returns its result in registers.
    pub own_ret: bool,
    pub safepoint: &'x dyn Fn(&Statement) -> bool,
}

/// Builds and call results of `f` (`params` is done apart).
pub fn plan(f: &MirFunction, x: &Facts<'_>) -> Plan {
    let mut out = Plan::default();
    let whole = |l: &LocalId| {
        matches!(
            f.locals[l.0 as usize].ty,
            MirTy::Struct(..) | MirTy::Tuple(_)
        )
    };
    let any = f.blocks.iter().flat_map(|b| &b.stmts).any(|s| {
        matches!(s, Statement::Assign { target, rvalue: RValue::StructInit { .. } | RValue::Call { .. } } if whole(target))
    });
    if !any {
        return out;
    }
    let c = counts(f);
    let t = x.t;
    let ty = |l: LocalId| &f.locals[l.0 as usize].ty;
    // Not a root: a slot never written is free for the frame to share,
    // and a collection would scan what another slot left there.
    let plain = |l: LocalId| {
        !x.addressed.contains(&l) && !x.rooted.contains(&l.0) && !f.params.contains(&l)
    };
    for (pos, b) in f.blocks.iter().enumerate() {
        let dead = |i: usize| -> &[LocalId] {
            x.deaths
                .after_stmt
                .get(pos)
                .and_then(|d| d.get(i))
                .map_or(&[], Vec::as_slice)
        };
        for (i, s) in b.stmts.iter().enumerate() {
            let Statement::Assign { target, rvalue } = s else {
                continue;
            };
            let l = *target;
            if !matches!(rvalue, RValue::StructInit { .. } | RValue::Call { .. }) {
                continue;
            }
            let one = c.defs[l.0 as usize] == 1 && plain(l);
            match rvalue {
                RValue::StructInit { fields, .. } if one && c.reads[l.0 as usize] == 1 => {
                    if flat(ty(l), t).is_some() && init_ok(f, x, pos, i, l, fields, &dead) {
                        out.inits.insert((pos, i));
                    }
                }
                RValue::Call { callee, .. } if one && whole(&l) && (x.ret_parts)(*callee) => {
                    let Some(fs) = flat(ty(l), t) else {
                        continue;
                    };
                    let mut call_reads = HashSet::default();
                    nabic_mir::stmt_reads(s, &mut |r| {
                        call_reads.insert(r);
                    });
                    let mut to = vec![None; fs.len()];
                    let mut skip = Vec::new();
                    let mut ok = true;
                    for (k, s2) in b.stmts.iter().enumerate().skip(i + 1) {
                        match s2 {
                            Statement::Loc(..) => {}
                            Statement::Assign {
                                target: y,
                                rvalue:
                                    RValue::FieldRead {
                                        target: Operand::Copy(src),
                                        index,
                                        ..
                                    },
                            } if *src == l => {
                                let y = *y;
                                let j = *index as usize;
                                let fits = fs.get(j).is_some_and(|(fty, _)| fty == ty(y));
                                let cleared = (i..k).any(|d| dead(d).contains(&y));
                                if !fits
                                    || y == l
                                    || x.addressed.contains(&y)
                                    || call_reads.contains(&y)
                                    || cleared
                                    || to[j].replace(y).is_some()
                                    || to.iter().flatten().filter(|z| **z == y).count() > 1
                                {
                                    ok = false;
                                    break;
                                }
                                skip.push((pos, k));
                            }
                            _ => break,
                        }
                    }
                    if ok && !skip.is_empty() && skip.len() == c.reads[l.0 as usize] as usize {
                        out.skip.extend(skip);
                        out.rets.insert((pos, i), to);
                    }
                }
                _ => {}
            }
        }
    }
    out
}

/// Whether the struct `l` built at statement `i` of block `pos` from
/// `fields` reaches its one reader, a call argument or the return,
/// with every field's local unchanged.
fn init_ok<'d>(
    f: &MirFunction,
    x: &Facts<'_>,
    pos: usize,
    i: usize,
    l: LocalId,
    fields: &[Operand],
    dead: &dyn Fn(usize) -> &'d [LocalId],
) -> bool {
    let b = &f.blocks[pos];
    let ops: Vec<LocalId> = fields
        .iter()
        .filter_map(|o| match o {
            Operand::Copy(y) => Some(*y),
            Operand::Const(_) => None,
        })
        .collect();
    if ops.iter().any(|y| x.addressed.contains(y)) {
        return false;
    }
    let refs = x.t.has_gc_ref(&f.locals[l.0 as usize].ty);
    let reads_l = |s: &Statement| {
        let mut r = false;
        nabic_mir::stmt_reads(s, &mut |y| r |= y == l);
        r
    };
    // A reader further on is not looked for.
    for k in i + 1..b.stmts.len().min(i + 1 + SCAN) {
        if dead(k - 1).iter().any(|y| ops.contains(y)) {
            return false;
        }
        let s = &b.stmts[k];
        if reads_l(s) {
            let Statement::Assign {
                rvalue: RValue::Call { callee, args, .. },
                ..
            } = s
            else {
                return false;
            };
            let at: Vec<usize> = args
                .iter()
                .enumerate()
                .filter(|(_, a)| matches!(a, Operand::Copy(y) if *y == l))
                .map(|(n, _)| n)
                .collect();
            return matches!(at.as_slice(), [n] if (x.arg_parts)(*callee, *n));
        }
        if written(s).is_some_and(|w| ops.contains(&w)) || refs && (x.safepoint)(s) {
            return false;
        }
    }
    x.own_ret
        && !dead(b.stmts.len() - 1).iter().any(|y| ops.contains(y))
        && matches!(&b.terminator, Terminator::Return(Some(Operand::Copy(y))) if *y == l)
}
