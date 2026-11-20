//! MIR -> LIR, one function at a time: layouts, GC frames, barriers,
//! allocation, polls, checks, panic messages.

mod arith;
mod call;
mod cbody;
mod combine;
mod debug;
mod fuse;
mod gc;
mod magic;
mod objc;
mod panic;
mod rvalue;
mod stmt;
mod value;
mod vector;

use crate::cc::{self, Shape};
use crate::lir::PartSrc;
use crate::lir::{
    Bin, Callee, Cap, Cc, Conv, Func, Inst, K, Label, Loc, Mem, Order, Part, Rmw, Slot, SymId, Un,
    V,
};
use crate::obj::Obj;
use nabic_codegen::abi::{self, ArgAbi, CType, FnAbi, RetAbi, nabi_by_ref};
use nabic_codegen::analysis::{
    cannot_overflow, const_bytes, const_int, gc_object_size, scan_table,
};
use nabic_core::{HashMap, HashSet};
use nabic_ir_common::{Abi, ConstVal, FunctionId, LocalId, PrimOp, PrimTy};
use nabic_mir::gc_abi::{
    CARD_SHIFT, FLAG_CARDED, FLAG_UNLOGGED, GC_FLAGS_OFFSET, GC_WORD_OFFSET, HEADER_SIZE,
    LOS_THRESHOLD, PLAIN_TYPE_ID,
};
use nabic_mir::{
    BasicBlock, MirFunction, MirTy, Operand, RValue, Statement, Target, Terminator, tls_abi,
};

/// How a call reaches a module function.
#[allow(clippy::struct_excessive_bools)]
pub struct Sig {
    pub sym: SymId,
    pub params: Vec<MirTy>,
    pub ret: MirTy,
    /// A C declaration: its C lowering.
    pub c: Option<FnAbi>,
    /// An `extern "C" fn` definition: its C ABI entry.
    pub c_entry: Option<SymId>,
    /// Defined in this object (else reached through the GOT).
    pub local: bool,
    /// The nabi body takes the caller's `nabi_tls` address last
    /// (`FnFacts::takes_tls`).
    pub tls: bool,
    /// `nabi_throw_site`: a store into this thread's slot, not a call.
    pub throw_site: bool,
    /// `#[cold]`: calls to it are rarely run.
    pub cold: bool,
    /// A `#[no_safepoint]` runtime function a debug nabic brackets with
    /// `nabi_nosp_enter` / `nabi_nosp_leave`, so the runtime can tell if
    /// one ever stops for a collection.
    pub nosp: bool,
    /// What a call does, known without the body (`FnFacts::summary`).
    pub summary: nabic_ir_common::Summary,
    /// A `core::simd` lane op (`FnFacts::simd`): vector code for a call
    /// where the target can.
    pub simd: Option<nabic_ir_common::SimdOp>,
}

/// How a nabi-ABI call passes its arguments.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct How {
    /// The caller's `nabi_tls` address goes last.
    tls: bool,
    /// The callee's `Summary::keeps_none` and `Summary::returns`.
    keeps_none: u64,
    returns: u64,
}

/// Module-wide state the functions share.
pub struct Ctx<'a> {
    pub t: &'a Target,
    /// The compile's source files (panic locations).
    pub sources: &'a nabic_diagnostic::SourceMap,
    pub conv: cc::Conv,
    pub obj: &'a mut Obj,
    pub fns: &'a HashMap<FunctionId, Sig>,
    pub pkgs: &'a nabic_codegen::mangle::Pkgs,
    /// `ScanTable` of each GC type (index + `gc_type_base`).
    pub gc_types: &'a mut Vec<Vec<u32>>,
    pub gc_type_base: u32,
    /// In a codegen unit, the user package's hash in its GC type id
    /// symbols (`Lower::gc_tid`).
    pub gc_pkg: Option<&'a str>,
    pub overflow_checks: bool,
    /// Above `-O0`.
    pub opt: bool,
    /// `-O<level>` (`-Os` / `-Oz`: no vector loops).
    pub level: crate::OptLevel,
    /// `-g`: describe functions and their locals (`MirModule::debug`).
    pub debug: Option<&'a nabic_mir::DebugTypes>,
    /// `-g` above `-O0` (`Func::track`): what optimization left of the
    /// variables (`MirFunction::dbg`) is described too.
    pub track: bool,
    /// Statics this object defines (else through the GOT).
    pub local_statics: &'a HashSet<String>,
    /// Functions that never reach a safepoint (`quiet_functions`).
    pub quiet: &'a HashSet<FunctionId>,
    /// Functions a call cycle can enter: they poll on entry
    /// (`reentrant_functions`).
    pub reentrant: &'a HashSet<FunctionId>,
    /// Unsupported constructs, at their function (dummy outside one).
    pub errors: Vec<(nabic_diagnostic::Span, String)>,
    /// Functions whose function-value thunk (`thunk`) this object needs.
    pub thunks: std::collections::BTreeSet<FunctionId>,
    /// A plugin (`--crate-type=plugin`): GC type ids and the TLS block
    /// come from the host.
    pub plugin: nabic_mir::plugin::Mode,
    /// `--hot`.
    pub hot: Option<crate::hot::State<'a>>,
    /// Struct ids of `Slice` and `SliceMut`: a buffer, a start and a
    /// length (fields 0, 1, 2), the buffer holding `start..start + len`.
    pub views: &'a HashSet<u32>,
}

impl Ctx<'_> {
    /// The index of `ty` objects' (`array`: buffers of them) GC type.
    fn gc_type(&mut self, ty: &MirTy, array: bool) -> usize {
        let key = scan_table(ty, array, self.t);
        if let Some(i) = self.hot_gc_type(ty, array, &key) {
            return i;
        }
        self.gc_types
            .iter()
            .position(|k| *k == key)
            .unwrap_or_else(|| {
                self.gc_types.push(key);
                self.gc_types.len() - 1
            })
    }

    /// The symbol of a string literal's bytes (NUL-terminated).
    pub fn str_sym(&mut self, text: &str) -> SymId {
        self.obj.bytes(text.as_bytes())
    }

    pub fn sym(&mut self, name: &str) -> SymId {
        self.obj.sym(name)
    }
}

/// A typed operand: a scalar value, or the memory of an aggregate (`str`
/// included).
#[derive(Clone, Copy, Debug)]
pub enum Val {
    S(V, K),
    M(Mem),
    Unit,
}

/// A panic site: message, location (a literal, or the `#[track_caller]`
/// parameter).
#[derive(Clone, PartialEq, Eq, Hash)]
struct Panic {
    msg: &'static str,
    at: String,
}

pub struct FnLower<'c, 'a> {
    pub(crate) c: &'c mut Ctx<'a>,
    f: &'c MirFunction,
    pub out: Func,
    slots: Vec<Option<Slot>>,
    block_label: HashMap<u32, Label>,
    block_params: HashMap<u32, Vec<LocalId>>,
    /// This thread's `nabi_tls` block address (filled in the prologue).
    tls: Option<Slot>,
    /// In `lower` (not a generated function): the prologue fills `tls`.
    lowering: bool,
    /// The shadow-stack frame: its slot and the TLS slot of the top.
    gc_frame: Option<(Slot, Mem)>,
    rooted: HashSet<u32>,
    deaths: nabic_mir::Deaths,
    back_edges: HashSet<u32>,
    panics: Vec<(Panic, Label)>,
    at: nabic_diagnostic::Span,
    /// The hidden result pointer's slot.
    sret: Option<Slot>,
    ret_parts: Option<Vec<Part>>,
    /// `nabic_mir::addressed`.
    addressed: HashSet<LocalId>,
    /// `ref` write-backs of only the fields the callee may change
    /// (`writebacks`): the statement, the call's result, the result's
    /// field and the changed fields' mask.
    wb: HashMap<(usize, usize), (LocalId, u32, u64)>,
    /// Copies and builds of wide aggregates nothing reads (`dead_copies`),
    /// and the loads and builds `field_stores` leaves out.
    dead: HashSet<(usize, usize)>,
    /// Buffer element stores of a build changing only some fields of
    /// the element (`field_stores`): those fields and their values.
    fstores: HashMap<(usize, usize), Vec<(u32, Operand)>>,
    /// By-address parameters read where the caller has them
    /// (`in_place`), and results built where the caller wants them
    /// (`in_result`, `result_parts`, `result_param`): the slot of the
    /// pointer once stored, the offset from it, and whether it is the
    /// result's pointer.
    in_place: HashMap<LocalId, (Option<Slot>, u32, bool)>,
    /// Locals that are a field of an in-place parameter (`field_views`):
    /// the local and the parameter.
    views: Vec<(LocalId, LocalId, u32)>,
    /// Loops that get a vector loop in front (`vector`).
    vec: vector::Loops,
    /// Byte loads combined into wider ones (`combine`).
    combined: combine::Plan,
    /// Integer locals assigned once, by a mask or an unsigned remainder
    /// by a constant: the most each holds (`masks`).
    masked: HashMap<LocalId, i128>,
    /// Straight-line lanes as vector code (`vector::slp`).
    slp: vector::SlpPlan,
    /// Small structs passed in registers from / to their fields' locals
    /// (`fuse`).
    fuse: fuse::Plan,
    /// The slots of structs never stored (`fuse::Plan::inits`): where
    /// each field's value is, by offset.
    virt: HashMap<Slot, Vec<(u32, V)>>,
    /// The result parts of the next call of a symbol, by offset: where
    /// each goes (`fuse::Plan::rets`). Keyed by the callee: a call made
    /// on the way (the TLS block's address) leaves it.
    ret_to: Option<(SymId, Vec<(u32, Mem)>)>,
    /// Arrays read in place from read-only memory (`read_only`), not
    /// copied into their slots.
    ro: HashMap<LocalId, Ro>,
    /// A C function (`FnRole::C`): C conventions, no runtime state, no
    /// checks.
    pub(crate) cfn: bool,
    /// A C function's return: how, and the result's shape in registers.
    c_ret: Option<(RetAbi, Option<Shape>)>,
    /// A variadic C function: where its named arguments ended, and the
    /// registers it saved for `va_start`.
    va: Option<(cbody::Named, Option<Mem>)>,
}

/// The integer locals of `f` assigned once (no block parameter, no
/// address taken) by `x & c` or unsigned `x % c` of a constant `c`: the
/// most each holds. A shift by one below its width needs no check.
fn masks(f: &MirFunction) -> HashMap<LocalId, i128> {
    let addressed = nabic_mir::addressed(f);
    let mut defs: HashMap<LocalId, Option<i128>> = HashMap::default();
    let bound = |rv: &RValue| match rv {
        RValue::BinOp {
            op: PrimOp::BitAnd,
            op_ty: MirTy::Prim(p),
            lhs,
            rhs,
        } if p.is_signed_int() || p.is_unsigned_int() => [lhs, rhs]
            .iter()
            .find_map(|o| const_int(o).filter(|c| *c >= 0)),
        RValue::BinOp {
            op: PrimOp::Rem,
            op_ty: MirTy::Prim(p),
            rhs,
            ..
        } if p.is_unsigned_int() => const_int(rhs).filter(|c| *c > 0).map(|c| c - 1),
        _ => None,
    };
    for b in &f.blocks {
        for p in &b.params {
            defs.insert(*p, None);
        }
        for s in &b.stmts {
            match s {
                Statement::Assign { target, rvalue } => {
                    let v = bound(rvalue);
                    defs.entry(*target).and_modify(|d| *d = None).or_insert(v);
                }
                Statement::ArrayWrite { array: l, .. } | Statement::StorageDead(l) => {
                    defs.insert(*l, None);
                }
                _ => {}
            }
        }
    }
    for p in &f.params {
        defs.insert(*p, None);
    }
    defs.into_iter()
        .filter(|(l, _)| !addressed.contains(l))
        .filter_map(|(l, v)| Some((l, v?)))
        .collect()
}

/// Where a read-only array local's elements are.
#[derive(Clone)]
pub(crate) enum Ro {
    /// A static nothing writes.
    Static(String),
    /// Constants, in data of their own.
    Data(Vec<u8>),
    /// String literals: a table of `str`s pointing at them, relocated
    /// once at load.
    Strs(Vec<String>),
}

/// The array locals of `f` assigned once, a static nothing writes or an
/// array of constants, and only ever indexed: their elements are read
/// where they are, with no copy per call (above `-O0`).
fn read_only(
    f: &MirFunction,
    pkgs: &nabic_codegen::mangle::Pkgs,
    t: &Target,
) -> HashMap<LocalId, Ro> {
    let mut defs: HashMap<LocalId, Option<Ro>> = HashMap::default();
    let ty = |l: LocalId| &f.locals[l.0 as usize].ty;
    let plain = |l: LocalId| matches!(ty(l), MirTy::Array(e, _) if !t.has_gc_ref(e));
    // A static's elements are read where a copy would read them; the
    // static roots what they refer to.
    let array = |l: LocalId| matches!(ty(l), MirTy::Array(..));
    let str_ty = MirTy::Prim(PrimTy::Str);
    let strs = |es: &[Operand]| -> Option<Vec<String>> {
        es.iter()
            .map(|e| match e {
                Operand::Const(ConstVal::Str(s)) => Some(s.clone()),
                _ => None,
            })
            .collect()
    };
    for b in &f.blocks {
        for p in &b.params {
            defs.insert(*p, None);
        }
        for s in &b.stmts {
            let Some(d) = nabic_mir::stmt_uses(s).1 else {
                continue;
            };
            let ro = match s {
                Statement::Assign {
                    rvalue: RValue::StaticLoad { name, .. },
                    ..
                } if array(d)
                    && pkgs.ro_statics.contains(name)
                    && !pkgs.dll_statics.contains(name) =>
                {
                    Some(Ro::Static(name.clone()))
                }
                Statement::Assign {
                    rvalue: RValue::ArrayInit { elem_ty, elements },
                    ..
                } if plain(d) && nabi_by_ref(ty(d), t) => {
                    const_bytes(elements, elem_ty, t).map(Ro::Data)
                }
                Statement::Assign {
                    rvalue: RValue::ArrayInit { elem_ty, elements },
                    ..
                } if *elem_ty == str_ty && nabi_by_ref(ty(d), t) => strs(elements).map(Ro::Strs),
                _ => None,
            };
            defs.entry(d).and_modify(|e| *e = None).or_insert(ro);
        }
    }
    for p in &f.params {
        defs.insert(*p, None);
    }
    // Read as a whole anywhere: a copy after all.
    let mut whole: HashSet<LocalId> = nabic_mir::addressed(f);
    for b in &f.blocks {
        for s in &b.stmts {
            match s {
                Statement::Assign {
                    rvalue:
                        RValue::ArrayIndex {
                            index: Operand::Copy(i),
                            ..
                        },
                    ..
                } => {
                    whole.insert(*i);
                }
                Statement::Assign {
                    rvalue: RValue::ArrayIndex { .. },
                    ..
                } => {}
                s => nabic_mir::stmt_reads(s, &mut |l| {
                    whole.insert(l);
                }),
            }
        }
        nabic_mir::term_reads(&b.terminator, &mut |l| {
            whole.insert(l);
        });
    }
    defs.into_iter()
        .filter(|(l, _)| !whole.contains(l))
        .filter_map(|(l, r)| Some((l, r?)))
        .collect()
}

/// The by-address parameters of `f` it never writes, whose address it
/// never takes: read through the caller's pointer, not copied in (above
/// `-O0`). The caller keeps the value in place and rooted for the call:
/// it reads it as an argument, and a call of a body is no point after
/// which the slot is cleared. Not in a function that never returns: a
/// caller whose only safepoint is such a call links no frame
/// (`body_safepoints`), so nothing roots its copy.
fn in_place(f: &MirFunction, t: &Target, addressed: &HashSet<LocalId>) -> Vec<LocalId> {
    let returns = f
        .blocks
        .iter()
        .any(|b| matches!(b.terminator, Terminator::Return(_)));
    if !returns {
        return Vec::new();
    }
    let mut written: HashSet<LocalId> = addressed.clone();
    written.extend(&f.pins);
    for b in &f.blocks {
        written.extend(&b.params);
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
        .zip(&f.param_tys)
        .filter(|(p, ty)| nabi_by_ref(ty, t) && !written.contains(p))
        .map(|(p, _)| *p)
        .collect()
}

/// The locals `f` returns, when the result goes by address and
/// nothing else reads them: built right in the caller's memory, not
/// copied there (above `-O0`). Several share it when each is assigned
/// only in the blocks returning it. The caller's memory is no root of
/// the callee, so a local holding references must not be live across a
/// safepoint.
fn in_result(
    f: &MirFunction,
    c: &Ctx<'_>,
    addressed: &HashSet<LocalId>,
    back_edges: &HashSet<u32>,
) -> Vec<LocalId> {
    let t = c.t;
    if !nabi_by_ref(&f.ret_ty, t) {
        return Vec::new();
    }
    // Each returned local, and the blocks returning it.
    let mut rs: Vec<(LocalId, Vec<usize>)> = Vec::new();
    for (bi, b) in f.blocks.iter().enumerate() {
        match &b.terminator {
            Terminator::Return(Some(Operand::Copy(l))) => match rs.iter_mut().find(|r| r.0 == *l) {
                Some(r) => r.1.push(bi),
                None => rs.push((*l, vec![bi])),
            },
            Terminator::Return(_) => return Vec::new(),
            _ => {}
        }
    }
    let ok = |r: LocalId| !(f.params.contains(&r) || addressed.contains(&r) || f.pins.contains(&r));
    if rs.iter().any(|r| !ok(r.0)) {
        return Vec::new();
    }
    let many = rs.len() > 1;
    for (bi, b) in f.blocks.iter().enumerate() {
        let is_r = |l: LocalId| rs.iter().any(|r| r.0 == l);
        let mut bad = b.params.iter().any(|l| is_r(*l));
        for s in &b.stmts {
            nabic_mir::stmt_reads(s, &mut |l| bad |= is_r(l));
            let def = match s {
                Statement::Assign { target, .. } => Some(*target),
                Statement::ArrayWrite { array, .. } => Some(*array),
                _ => None,
            };
            // Written elsewhere than where it is returned.
            if let Some(d) = def.filter(|_| many) {
                bad |= rs.iter().any(|r| r.0 == d && !r.1.contains(&bi));
            }
        }
        if !matches!(b.terminator, Terminator::Return(_)) {
            nabic_mir::term_reads(&b.terminator, &mut |l| bad |= is_r(l));
        }
        if bad {
            return Vec::new();
        }
    }
    if !t.root_offsets(&f.ret_ty).is_empty() {
        let (quiet, pkgs) = (c.quiet, c.pkgs);
        let live = nabic_mir::live_across(
            f,
            |l| rs.iter().any(|r| r.0 == l),
            |s| nabic_codegen::analysis::stmt_safepoint(s, quiet, pkgs, t).then_some(true),
            |b| back_edges.contains(&b.0),
            true,
        );
        if !live.is_empty() {
            return Vec::new();
        }
    }
    rs.into_iter().map(|r| r.0).collect()
}

/// The statements copying or building a by-address aggregate that is
/// dead right after: what scalar replacement leaves on paths that do
/// not read its copy.
fn dead_copies(f: &MirFunction, t: &Target) -> HashSet<(usize, usize)> {
    let deaths = nabic_mir::deaths(f, |l| nabi_by_ref(&f.locals[l.0 as usize].ty, t));
    let mut out = HashSet::default();
    let mut defs = vec![(0u32, 0u32); f.locals.len()];
    for (pos, b) in f.blocks.iter().enumerate() {
        for (i, s) in b.stmts.iter().enumerate() {
            let Statement::Assign { target, rvalue } = s else {
                continue;
            };
            let d = &mut defs[target.0 as usize];
            d.0 += 1;
            if matches!(
                rvalue,
                RValue::Use(_)
                    | RValue::FieldRead { .. }
                    | RValue::StructInit { .. }
                    | RValue::WithField { .. }
            ) && deaths.after_stmt[pos][i].contains(target)
            {
                d.1 += 1;
                out.insert((pos, i));
            }
        }
    }
    // A local some copy still writes: a slot never written is one the
    // frame may share, and a root's clear there would hit another root.
    out.retain(|&(pos, i)| {
        let Statement::Assign { target, .. } = &f.blocks[pos].stmts[i] else {
            return false;
        };
        let (all, dead) = defs[target.0 as usize];
        dead < all
    });
    out
}

/// Locals the results `rs` (`in_result`) are built of, each built at
/// its field's place in the caller's memory and offset there: a wide
/// aggregate read once, as field `k` of a build of a result, and
/// assigned only before that build in its block, with neither a
/// result's assignment nor a safepoint between.
fn result_parts(
    f: &MirFunction,
    c: &Ctx<'_>,
    rs: &[LocalId],
    addressed: &HashSet<LocalId>,
    dead: &HashSet<(usize, usize)>,
) -> Vec<(LocalId, u32)> {
    let t = c.t;
    if rs.is_empty() {
        return Vec::new();
    }
    let mut reads = vec![0u32; f.locals.len()];
    let mut blocked: HashSet<LocalId> = addressed.clone();
    blocked.extend(&f.pins);
    blocked.extend(&f.params);
    blocked.extend(rs);
    for b in &f.blocks {
        blocked.extend(&b.params);
        for s in &b.stmts {
            nabic_mir::stmt_reads(s, &mut |l| reads[l.0 as usize] += 1);
        }
        nabic_mir::term_reads(&b.terminator, &mut |l| reads[l.0 as usize] += 1);
    }
    let (quiet, pkgs) = (c.quiet, c.pkgs);
    let mut out = Vec::new();
    for (bi, b) in f.blocks.iter().enumerate() {
        for (at, s) in b.stmts.iter().enumerate() {
            let Statement::Assign {
                target,
                rvalue: RValue::StructInit { ty, fields },
            } = s
            else {
                continue;
            };
            if !rs.contains(target) {
                continue;
            }
            let offs = t.field_offsets(ty);
            for (k, o) in fields.iter().enumerate() {
                let Operand::Copy(y) = o else { continue };
                let y = *y;
                let yty = &f.locals[y.0 as usize].ty;
                if blocked.contains(&y)
                    || reads[y.0 as usize] != 1
                    || !nabi_by_ref(yty, t)
                    || out.iter().any(|(l, _)| *l == y)
                {
                    continue;
                }
                // Its assignments: here, before the build; nothing else
                // writes the result between the first and the build.
                let def = |s: &Statement| match s {
                    Statement::Assign { target, .. } => Some(*target),
                    Statement::ArrayWrite { array, .. } => Some(*array),
                    _ => None,
                };
                // A dead copy is never made (`dead_copies`).
                let elsewhere = f.blocks.iter().enumerate().any(|(oi, ob)| {
                    oi != bi
                        && (ob.stmts.iter().enumerate())
                            .any(|(j, s)| def(s) == Some(y) && !dead.contains(&(oi, j)))
                }) || b.stmts[at..].iter().any(|s| def(s) == Some(y));
                let Some(first) = b.stmts[..at].iter().position(|s| def(s) == Some(y)) else {
                    continue;
                };
                let between = &b.stmts[first + 1..at];
                let clobbers = between.iter().any(|s| {
                    def(s).is_some_and(|d| rs.contains(&d))
                        || nabic_codegen::analysis::stmt_safepoint(s, quiet, pkgs, t)
                });
                if !elsewhere && !clobbers {
                    out.push((y, offs[k]));
                }
            }
        }
    }
    out
}

/// A parameter every build of the results `rs` takes as field `k`, the
/// results built nowhere else: copied in right at that field's place in
/// the caller's memory, so the build leaves it where it is (`ref self`
/// handed back). It must not hold references across a safepoint there.
fn result_param(
    f: &MirFunction,
    c: &Ctx<'_>,
    rs: &[LocalId],
    in_place: &[LocalId],
    addressed: &HashSet<LocalId>,
    back_edges: &HashSet<u32>,
) -> Option<(LocalId, u32)> {
    let t = c.t;
    let mut at: Option<(LocalId, usize)> = None;
    let mut builds = 0;
    for b in &f.blocks {
        for s in &b.stmts {
            let d = match s {
                Statement::Assign { target, rvalue } => Some((*target, Some(rvalue))),
                Statement::ArrayWrite { array, .. } => Some((*array, None)),
                _ => None,
            };
            let Some((d, rv)) = d.filter(|d| rs.contains(&d.0)) else {
                continue;
            };
            let Some(RValue::StructInit { fields, .. }) = rv else {
                return None;
            };
            // The parameter at the same field of every build.
            let here = fields.iter().enumerate().find_map(|(k, o)| match o {
                Operand::Copy(y) if f.params.contains(y) && at.is_none_or(|a| a == (*y, k)) => {
                    Some((*y, k))
                }
                _ => None,
            });
            let _ = d;
            at = Some(here?);
            builds += 1;
        }
    }
    let (y, k) = at?;
    let ty = &f.locals[y.0 as usize].ty;
    if builds == 0
        || in_place.contains(&y)
        || addressed.contains(&y)
        || f.pins.contains(&y)
        || !nabi_by_ref(ty, t)
    {
        return None;
    }
    if !t.root_offsets(ty).is_empty() {
        let (quiet, pkgs) = (c.quiet, c.pkgs);
        let live = nabic_mir::live_across(
            f,
            |l| l == y,
            |s| nabic_codegen::analysis::stmt_safepoint(s, quiet, pkgs, t).then_some(true),
            |b| back_edges.contains(&b.0),
            // A parameter is live on entry: only an entry poll matters.
            c.reentrant.contains(&f.id),
        );
        if live.contains(&y) {
            return None;
        }
    }
    Some((y, t.field_offsets(&f.ret_ty)[k]))
}

/// Locals assigned once, by a read of a by-address field of an in-place
/// parameter (`in_place`): read right in the parameter, not copied out.
/// The parameter never changes, so neither does the field.
fn field_views(
    f: &MirFunction,
    t: &Target,
    ps: &[LocalId],
    addressed: &HashSet<LocalId>,
) -> Vec<(LocalId, LocalId, u32)> {
    if ps.is_empty() {
        return Vec::new();
    }
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
    let mut out = Vec::new();
    for b in &f.blocks {
        for s in &b.stmts {
            let Statement::Assign {
                target,
                rvalue:
                    RValue::FieldRead {
                        target: Operand::Copy(p),
                        index,
                        ..
                    },
            } = s
            else {
                continue;
            };
            let ty = &f.locals[target.0 as usize].ty;
            if ps.contains(p)
                && defs[target.0 as usize] == 1
                && nabi_by_ref(ty, t)
                && !addressed.contains(target)
                && !f.pins.contains(target)
                && !f.params.contains(target)
            {
                let off = t.field_offsets(&f.locals[p.0 as usize].ty)[*index as usize];
                out.push((*target, *p, off));
            }
        }
    }
    out
}

/// Where a barriered store lands in its object.
#[derive(Clone, Copy)]
pub(crate) enum Card {
    /// At this byte offset.
    At(u32),
    /// In an element of a buffer of elements of this size.
    Elem(u32),
}

const fn const_ty(c: &ConstVal) -> MirTy {
    match c {
        ConstVal::Int { ty, .. } | ConstVal::UInt { ty, .. } | ConstVal::Float { ty, .. } => {
            MirTy::Prim(*ty)
        }
        ConstVal::Bool(_) => MirTy::Prim(PrimTy::Bool),
        ConstVal::Str(_) => MirTy::Prim(PrimTy::Str),
        ConstVal::Char(_) => MirTy::Prim(PrimTy::Char),
        ConstVal::Unit | ConstVal::None | ConstVal::Bytes(_) => MirTy::Unit,
    }
}

/// The register class of a primitive (`pw`-bit words): equal classes
/// need no cast.
const fn prim_class(p: PrimTy, pw: u32) -> u8 {
    match p {
        PrimTy::I8 | PrimTy::U8 | PrimTy::CChar => 1,
        PrimTy::I16 | PrimTy::U16 => 2,
        PrimTy::I32 | PrimTy::U32 | PrimTy::Char => 3,
        PrimTy::Isize | PrimTy::Usize if pw == 32 => 3,
        PrimTy::I64 | PrimTy::U64 | PrimTy::Isize | PrimTy::Usize => 4,
        PrimTy::F32 => 5,
        PrimTy::F64 => 6,
        PrimTy::Bool => 7,
        PrimTy::Str => 8,
    }
}

fn struct_fields(ty: &MirTy) -> &[MirTy] {
    match ty {
        MirTy::Struct(_, fs) | MirTy::Tuple(fs) => fs,
        _ => &[],
    }
}

/// `(size, align)` of a slot of `ty`.
fn slot_layout(ty: &MirTy, t: &Target) -> (u32, u32) {
    let (s, a) = t.size_align(ty);
    (s.max(1), a.max(1))
}

impl<'c, 'a> FnLower<'c, 'a> {
    pub fn new(c: &'c mut Ctx<'a>, f: &'c MirFunction, sym: SymId) -> Self {
        let mut out = Func {
            sym,
            ..Func::default()
        };
        let ro = if c.opt {
            read_only(f, c.pkgs, c.t)
        } else {
            HashMap::default()
        };
        // A C function never polls.
        let back_edges: HashSet<u32> = if f.facts.role == nabic_ir_common::FnRole::C {
            HashSet::default()
        } else {
            nabic_codegen::analysis::polled_back_edges(f, c.quiet, c.pkgs, c.t)
                .into_keys()
                .collect()
        };
        let addressed = nabic_mir::addressed(f);
        let dead = if c.opt {
            dead_copies(f, c.t)
        } else {
            HashSet::default()
        };
        let mut views = Vec::new();
        let in_place: HashMap<LocalId, (Option<Slot>, u32, bool)> =
            if c.opt && c.hot.is_none() && f.facts.role != nabic_ir_common::FnRole::C {
                let rs = in_result(f, c, &addressed, &back_edges);
                let mut parts = result_parts(f, c, &rs, &addressed, &dead);
                let ps = in_place(f, c.t, &addressed);
                parts.extend(result_param(f, c, &rs, &ps, &addressed, &back_edges));
                views = field_views(f, c.t, &ps, &addressed);
                views.retain(|v| !parts.iter().any(|p| p.0 == v.0));
                let vs: Vec<_> = views
                    .iter()
                    .map(|&(x, _, o)| (x, (None, o, false)))
                    .collect();
                let ps = ps.into_iter().map(|l| (l, (None, 0, false))).chain(vs);
                let rs = rs.into_iter().map(|l| (l, 0)).chain(parts);
                ps.chain(rs.map(|(l, o)| (l, (None, o, true)))).collect()
            } else {
                HashMap::default()
            };
        // A read-only array's elements are elsewhere, and so is an
        // in-place parameter: the slot only names it.
        let slots = f
            .locals
            .iter()
            .map(|l| {
                (l.ty != MirTy::Unit).then(|| {
                    let (s, a) = if ro.contains_key(&l.id) || in_place.contains_key(&l.id) {
                        (1, 1)
                    } else {
                        slot_layout(&l.ty, c.t)
                    };
                    out.slot(s, a)
                })
            })
            .collect();
        let mut block_label = HashMap::default();
        let mut block_params = HashMap::default();
        for b in &f.blocks {
            block_label.insert(b.id.0, out.label());
            block_params.insert(b.id.0, b.params.clone());
        }
        Self {
            c,
            f,
            out,
            slots,
            block_label,
            block_params,
            tls: None,
            lowering: false,
            gc_frame: None,
            rooted: HashSet::default(),
            deaths: nabic_mir::Deaths::default(),
            back_edges,
            panics: Vec::new(),
            at: f.span,
            sret: None,
            ret_parts: None,
            addressed,
            views,
            wb: HashMap::default(),
            dead,
            fstores: HashMap::default(),
            in_place,
            vec: vector::Loops::default(),
            combined: combine::Plan::default(),
            masked: HashMap::default(),
            slp: vector::SlpPlan::default(),
            fuse: fuse::Plan::default(),
            virt: HashMap::default(),
            ret_to: None,
            ro,
            cfn: f.facts.role == nabic_ir_common::FnRole::C,
            c_ret: None,
            va: None,
        }
    }

    fn err(&mut self, what: String) {
        let msg = format!("in `{}`: unsupported in codegen: {what}", self.f.name);
        self.c.errors.push((self.f.span, msg));
    }

    fn pw(&self) -> u32 {
        self.c.t.pointer_width
    }

    pub(crate) fn word(&self) -> K {
        if self.pw() == 32 { K::I32 } else { K::I64 }
    }

    fn local_ty(&self, l: LocalId) -> MirTy {
        self.f
            .locals
            .get(l.0 as usize)
            .map_or(MirTy::Any, |d| d.ty.clone())
    }

    fn local_mem(&self, l: LocalId) -> Option<Mem> {
        if let Some(&(p, off, _)) = self.in_place.get(&l) {
            return p.map(|s| Mem::Ind(s, off as i32));
        }
        self.slots
            .get(l.0 as usize)
            .copied()
            .flatten()
            .map(|s| Mem::Frame(s, 0))
    }

    /// Where array local `l`'s elements are: its slot, or the read-only
    /// memory it stands for (`read_only`).
    pub(crate) fn array_base(&mut self, l: LocalId) -> Option<Mem> {
        match self.ro.get(&l).cloned() {
            Some(Ro::Static(name)) => Some(Mem::Ind(self.static_addr(&name), 0)),
            Some(Ro::Data(bytes)) => {
                let sym = self.c.obj.bytes(&bytes);
                let p = self.word_tmp();
                self.push(Inst::Sym {
                    dst: p,
                    sym,
                    got: false,
                });
                let Mem::Frame(s, _) = p else { unreachable!() };
                Some(Mem::Ind(s, 0))
            }
            Some(Ro::Strs(texts)) => {
                let lits: Vec<SymId> = texts.iter().map(|x| self.c.str_sym(x)).collect();
                let lens: Vec<u64> = texts.iter().map(|x| x.len() as u64).collect();
                let pw = self.pw() / 8;
                let sym = self.c.obj.str_table(&lits, &lens, pw);
                let p = self.word_tmp();
                self.push(Inst::Sym {
                    dst: p,
                    sym,
                    got: false,
                });
                let Mem::Frame(s, _) = p else { unreachable!() };
                Some(Mem::Ind(s, 0))
            }
            None => self.local_mem(l),
        }
    }

    pub(crate) fn tmp(&mut self, ty: &MirTy) -> Mem {
        let (s, a) = slot_layout(ty, self.c.t);
        Mem::Frame(self.out.slot(s, a), 0)
    }

    pub(crate) fn tmp_k(&mut self, k: K) -> Mem {
        Mem::Frame(self.out.slot(k.size(), k.size()), 0)
    }

    pub(crate) fn tmp_bytes(&mut self, size: u32, align: u32) -> Mem {
        Mem::Frame(self.out.slot(size, align), 0)
    }

    pub(crate) fn push(&mut self, i: Inst) {
        self.out.push(i);
    }

    pub(crate) fn label(&mut self) -> Label {
        self.out.label()
    }

    /// `o` above `-O0`; `-O0` keeps every atomic sequentially consistent.
    pub(crate) fn order(&self, o: Order) -> Order {
        if self.c.opt { o } else { Order::SeqCst }
    }

    pub(crate) fn word_tmp(&mut self) -> Mem {
        let k = self.word();
        self.tmp_k(k)
    }

    /// Lower the whole function.
    pub fn lower(mut self) -> Func {
        let t = self.c.t;
        let f = self.f;
        self.lowering = true;
        if self.cfn {
            return self.lower_c();
        }
        // Incoming parameters, the caller's `nabi_tls` address last.
        let tls = self.c.fns.get(&f.id).is_some_and(|s| s.tls);
        let (shapes, rs, lead) = self.nabi_shapes(&f.param_tys, &f.ret_ty, tls);
        let locs = cc::assign(self.c.conv, &shapes, rs.as_ref(), false, None);
        self.out.sig = Some((locs.params.concat(), locs.ret.clone()));
        if tls {
            let s = self.out.slot(8, 8);
            self.tls = Some(s);
            let part = locs.params.last().expect("the nabi_tls address")[0];
            self.push(Inst::Param {
                part,
                dst: Mem::Frame(s, 0),
            });
        }
        let (mut param_locs, mut param_shapes) = (locs.params.iter(), shapes.iter());
        if lead {
            // The result's address: returned through like a hidden one.
            let s = self.out.slot(8, 8);
            let (parts, _) = (param_locs.next(), param_shapes.next());
            self.push(Inst::Param {
                part: parts.expect("result pointer")[0],
                dst: Mem::Frame(s, 0),
            });
            self.sret = Some(s);
            // The result built where the caller wants it.
            for at in self.in_place.values_mut().filter(|at| at.2) {
                at.0 = Some(s);
            }
        } else if locs.ret.is_none() {
            let s = self.out.slot(8, 8);
            let k = self.word();
            self.push(Inst::Param {
                part: Part {
                    loc: Loc::Sret,
                    k,
                    signed: false,
                },
                dst: Mem::Frame(s, 0),
            });
            self.sret = Some(s);
        }
        self.out.pop = locs.pop;
        self.ret_parts.clone_from(&locs.ret);
        if self.c.opt {
            let entry_poll = self.c.reentrant.contains(&f.id) && self.reaches_safepoint();
            self.fuse = fuse::params(f, t, &self.addressed, |ty| nabi_by_ref(ty, t), entry_poll);
        }
        // A by-address argument: its pointer first, the copy once every
        // parameter is stored.
        let mut copies = Vec::new();
        let mut held = Vec::new();
        for (((p, ty), parts), sh) in f
            .params
            .iter()
            .zip(&f.param_tys)
            .zip(param_locs)
            .zip(param_shapes)
        {
            if nabi_by_ref(ty, t) {
                let s = self.out.slot(8, 8);
                self.push(Inst::Param {
                    part: parts[0],
                    dst: Mem::Frame(s, 0),
                });
                if let Some(at) = self.in_place.get_mut(p).filter(|at| !at.2) {
                    at.0 = Some(s);
                } else if let Some(m) = self.local_mem(*p) {
                    copies.push((m, s, t.size_align(ty).0));
                }
                continue;
            }
            let Some(m) = self.local_mem(*p) else {
                continue;
            };
            let to = self.fuse_param(*p);
            for (part, l) in parts.iter().zip(&sh.leaves) {
                let at = to.iter().find(|(o, _)| *o == l.off).map(|x| x.1);
                self.push(Inst::Param {
                    part: *part,
                    dst: at.unwrap_or_else(|| m.at(l.off)),
                });
            }
            // A root (from the entry) holds its parts too: the frame
            // shares only slots it sees written.
            if !to.is_empty() && !t.root_offsets(ty).is_empty() {
                for l in &sh.leaves {
                    if let Some((_, x)) = to.iter().find(|(o, _)| *o == l.off) {
                        held.push((l.k, m.at(l.off), *x));
                    }
                }
            }
        }
        for (k, dst, src) in held {
            self.push(Inst::Mov {
                k,
                dst,
                src: V::M(src),
            });
        }
        for &(x, p, _) in &self.views {
            let at = self.in_place.get(&p).and_then(|a| a.0);
            if let Some(v) = self.in_place.get_mut(&x) {
                v.0 = at;
            }
        }
        for (dst, s, size) in copies {
            self.push(Inst::Copy {
                dst,
                src: Mem::Ind(s, 0),
                size,
            });
        }
        // After every parameter is stored: an instruction's scratch
        // registers may be argument registers.
        for p in &f.params {
            if let (MirTy::Prim(PrimTy::Bool), Some(m)) = (self.local_ty(*p), self.local_mem(*p)) {
                self.normalize_bool(m);
            }
        }
        let head = self.out.insts.len();
        self.gc_frame();
        // Entry poll: only where a call cycle can come back in without
        // a loop's poll, and a collection can run at all.
        if self.c.reentrant.contains(&f.id) && self.reaches_safepoint() {
            self.poll();
        }
        self.vec = self.vector_loops();
        if self.c.opt {
            self.combined = combine::plan(f, t);
            self.masked = masks(f);
            self.fuse_plan();
            self.writebacks();
            self.field_stores();
            // After the plans above: it leaves their statements alone.
            self.slp = self.slp_plan();
        }
        for (pos, b) in f.blocks.iter().enumerate() {
            self.block(pos, b);
        }
        self.emit_panics();
        // The TLS block's address, right after the parameters.
        if let Some(s) = self.tls.filter(|_| !tls) {
            let at = self.tls_addr(Mem::Frame(s, 0));
            self.out.insts.insert(head, at);
        }
        if self.c.debug.is_some() && f.span.line != 0 {
            self.out.debug = Some(self.debug_facts());
        }
        self.out
    }

    fn terminator(&mut self, term: &Terminator) {
        let f = self.f;
        match term {
            Terminator::Return(None) => {
                self.gc_frame_pop();
                self.push(Inst::Ret { vals: Vec::new() });
            }
            Terminator::Return(Some(_)) if f.ret_ty == MirTy::Unit => {
                self.gc_frame_pop();
                self.push(Inst::Ret { vals: Vec::new() });
            }
            Terminator::Return(Some(op)) if self.c_ret.is_some() => {
                let ty = f.ret_ty.clone();
                let v = self.val(op, &ty);
                self.c_return(v, &ty);
            }
            Terminator::Return(Some(op)) => {
                let ty = f.ret_ty.clone();
                let v = self.val(op, &ty);
                self.gc_frame_pop();
                if let Some(s) = self.sret {
                    self.store(v, &ty, Mem::Ind(s, 0));
                    let k = self.word();
                    self.push(sret_ret(s, k));
                    return;
                }
                let parts = self.ret_parts.clone().unwrap_or_default();
                let mut vals = Vec::new();
                self.arg_parts(v, &ty, &parts, &mut vals);
                self.push(Inst::Ret { vals });
            }
            Terminator::Goto { target, args } => self.edge(target.0, args),
            Terminator::If {
                cond,
                then_bb,
                then_args,
                else_bb,
                else_args,
            } => {
                let (c, _) = self.scalar(cond, &MirTy::Prim(PrimTy::Bool));
                let tl = if then_args.is_empty() {
                    self.block_label[&then_bb.0]
                } else {
                    self.label()
                };
                let el = if else_args.is_empty() {
                    self.block_label[&else_bb.0]
                } else {
                    self.label()
                };
                self.push(Inst::Br { c, t: tl, f: el });
                if !then_args.is_empty() {
                    self.push(Inst::Label(tl));
                    self.edge(then_bb.0, then_args);
                }
                if !else_args.is_empty() {
                    self.push(Inst::Label(el));
                    self.edge(else_bb.0, else_args);
                }
            }
            Terminator::Unreachable => self.push(Inst::Trap),
            Terminator::Suspend { .. } => {
                let msg = "internal: a coroutine suspend point survived MIR".to_owned();
                self.c.errors.push((self.f.span, msg));
                self.push(Inst::Trap);
            }
        }
    }

    /// Pass `args` to block `target`'s parameters and jump there.
    fn edge(&mut self, target: u32, args: &[Operand]) {
        let params = self.block_params.get(&target).cloned().unwrap_or_default();
        // A parallel copy: through temporaries when an argument reads a
        // parameter of the target.
        let reads_param = args
            .iter()
            .any(|a| matches!(a, Operand::Copy(l) if params.contains(l)));
        let mut staged = Vec::new();
        for (a, p) in args.iter().zip(&params) {
            let ty = self.local_ty(*p);
            let Some(d) = self.local_mem(*p) else {
                continue;
            };
            // A constant takes the parameter's type.
            let aty = match a {
                Operand::Copy(_) => self.op_ty(a),
                Operand::Const(_) => ty.clone(),
            };
            let v = self.val(a, &aty);
            let v = match (&aty, &ty) {
                (MirTy::Prim(x), MirTy::Prim(y))
                    if prim_class(*x, self.pw()) != prim_class(*y, self.pw()) =>
                {
                    self.cast(v, &aty, &ty)
                }
                _ => v,
            };
            if reads_param {
                let m = self.tmp(&ty);
                self.store(v, &ty, m);
                staged.push((Val::M(m), ty, d));
            } else {
                self.store(v, &ty, d);
            }
        }
        for (v, ty, d) in staged {
            self.store(v, &ty, d);
        }
        let l = self.block_label[&target];
        self.push(Inst::Jmp(l));
    }
}

/// `abi::classify` with a `#[repr(transparent)]` scalar passed as its
/// field, `stdcall` for `extern "system"` (`system`) where it is;
/// `None` (and an error) where the target cannot pass it.
pub fn classify(
    errors: &mut Vec<(nabic_diagnostic::Span, String)>,
    at: nabic_diagnostic::Span,
    t: &Target,
    params: &[MirTy],
    ret: &MirTy,
    (variadic, system): (bool, bool),
) -> Option<FnAbi> {
    if let Some((why, help)) = abi::c_refusal(t, params, ret) {
        errors.push((at, why));
        errors.push((at, help));
        return None;
    }
    let mut fa = abi::classify(t, params, ret, variadic);
    if let Some(a) = fa.as_mut() {
        a.stdcall = system && t.stdcall_system();
        // MSVC passes an over-aligned aggregate by reference to cdecl
        // functions only.
        // There it is 16-aligned, which an i386 frame is not on Windows.
        let by_ref = |x: &ArgAbi, p: &MirTy| {
            matches!(x, ArgAbi::Indirect { byval: false })
                && abi::is_aggregate(p)
                && t.c_abi == nabic_mir::CAbi::X86Win
        };
        if !a.stdcall && a.args.iter().zip(params).any(|(x, p)| by_ref(x, p)) {
            errors.push((
                at,
                "a struct with `#[repr(align(N))]` past 4 cannot be passed by value to a cdecl C \
                 function on i686 Windows"
                    .to_owned(),
            ));
            errors.push((
                at,
                "help: pass a pointer to it (`ConstPtr<T>`), or declare the function \
                 `extern \"system\"` if C declares it `__stdcall`"
                    .to_owned(),
            ));
            return None;
        }
        if a.stdcall {
            for (x, p) in a.args.iter_mut().zip(params) {
                if matches!(x, ArgAbi::Indirect { byval: false }) && abi::is_aggregate(p) {
                    *x = ArgAbi::Indirect { byval: true };
                }
            }
        }
    }
    let coerce = |ty: &MirTy| {
        let inner = abi::transparent(ty, t);
        (inner != *ty && !abi::is_aggregate(&inner) && inner != MirTy::Unit).then(|| {
            match cc::scalar_k(&inner, t) {
                Some(K::F32) => abi::CType::Scalar(abi::CScalar::F32),
                Some(K::F64) => abi::CType::Scalar(abi::CScalar::F64),
                Some(k) => abi::CType::int(k.bits()),
                None => abi::CType::PTR,
            }
        })
    };
    if let Some(a) = fa.as_mut() {
        for (x, p) in a.args.iter_mut().zip(params) {
            if let (ArgAbi::Direct, Some(c)) = (&x, coerce(p)) {
                *x = ArgAbi::Coerce(c);
            }
        }
        if let (RetAbi::Direct, Some(c)) = (&a.ret, coerce(ret)) {
            a.ret = RetAbi::Coerce(c);
        }
    }
    if fa.is_none() {
        errors.push((
            at,
            "unsupported in codegen: a `struct` by value across the C ABI on this target"
                .to_owned(),
        ));
    }
    fa
}

/// Return through the hidden result pointer in `slot` (a `k` word),
/// which goes back in the first result register (`x86_64` System V,
/// i386; harmless elsewhere).
pub fn sret_ret(slot: Slot, k: K) -> Inst {
    Inst::Ret {
        vals: vec![(
            Part {
                loc: Loc::Int(0),
                k,
                signed: false,
            },
            V::M(Mem::Frame(slot, 0)),
        )],
    }
}

/// A scalar constant's bits, as the debugger reads the value.
fn const_bits(c: &nabic_ir_common::ConstVal) -> Option<u64> {
    Some(match c {
        ConstVal::Bool(b) => u64::from(*b),
        ConstVal::Int { value, .. } => *value as u64,
        ConstVal::UInt { value, .. } => *value as u64,
        ConstVal::Float { bits, .. } => *bits,
        ConstVal::Char(c) => u64::from(*c),
        _ => return None,
    })
}
