//! What a call does, for callees whose bodies a build does not see.
//!
//! A library build exports its functions' (`nabic_ir_common::Summary`,
//! `export`), a dependent's build sets them on its declarations, and
//! the runtime's are written down here (`runtime`).

mod echo;

pub use crate::summary::echo::{echo, set_echoes};

use crate::{Link, MirFunction, MirModule, MirTy, Operand, RValue, Statement, Terminator};
use nabic_core::{HashMap, HashSet};
use nabic_ir_common::{Abi, FnRole, FunctionId, Inline, Linkage, LocalId, PrimTy, Summary};

/// Runtime functions that write no memory nabi code reads, each checked
/// against its body in `runtime/nabir`:
///
/// - `str` reads: a `str` is an immutable `(ptr, len)` value; these only
///   read its bytes or words (`str.rs`, `fmt.rs`'s `nabi_fmt_len`), and
///   `nabi_str_trim` returns a slice of its argument;
/// - `nabi_buf_utf8_error` reads a buffer's bytes;
/// - character properties, case mapping and segmentation: table lookups
///   (`unicode`); the normalization check reads its argument and `malloc`
///   scratch;
/// - libm and `core` float functions: pure on their arguments (`math.rs`;
///   C `errno` is no memory nabi code loads);
/// - `f16` / `bf16` conversions: bit arithmetic (`math.rs`);
/// - bignum scratch and length sizes: arithmetic (`bignum.rs`);
/// - the process's argument count, CPU count, thread id, error kinds and
///   clocks: reads of the system or of this thread's slots.
const READONLY: &[&str] = &[
    "nabi_str_len",
    "nabi_str_addr",
    "nabi_str_cmp",
    "nabi_str_cmp_parked",
    "nabi_str_eq",
    "nabi_str_eq_parked",
    "nabi_str_eq_at",
    "nabi_str_eq_at_parked",
    "nabi_str_find_near",
    "nabi_str_find_parked",
    "nabi_str_rfind_near",
    "nabi_str_rfind_parked",
    "nabi_str_char_count",
    "nabi_str_char_count_parked",
    "nabi_str_grapheme_end",
    "nabi_str_grapheme_start",
    "nabi_str_grapheme_count",
    "nabi_str_grapheme_count_parked",
    "nabi_str_word_end",
    "nabi_str_is_normalized",
    "nabi_str_byte",
    "nabi_str_bytes_buf",
    "nabi_str_trim",
    "nabi_fmt_len",
    "nabi_buf_utf8_error",
    "nabi_buf_utf8_error_parked",
    "nabi_char_props",
    "nabi_char_to_case",
    "nabi_f16_from_f32",
    "nabi_f16_to_f32",
    "nabi_f16_from_f64",
    "nabi_bf16_from_f64",
    "nabi_bf16_from_i64",
    "nabi_bf16_from_u64",
    "nabi_nat_mul_scratch",
    "nabi_nat_divmod_scratch",
    "nabi_nat_mulmod_scratch",
    "nabi_nat_gcd_scratch",
    "nabi_nat_ntt_len",
    "nabi_nat_mulmod_len",
    "nabi_os_errno_kind",
    "nabi_os_argc",
    "nabi_cpu_count",
    "nabi_thread_id",
    "nabi_time_now_ns",
    "nabi_time_realtime_ns",
    "nabi_time_boot_ns",
];

/// `f32` / `f64` methods of `math.rs`, `nabi_<ty>_<name>`.
const MATH: &[&str] = &[
    "floor", "ceil", "round", "trunc", "abs", "cbrt", "exp", "exp2", "exp_m1", "ln", "ln_1p",
    "log2", "log10", "sin", "cos", "tan", "asin", "acos", "atan", "sinh", "cosh", "tanh", "asinh",
    "acosh", "atanh", "pow", "powi", "atan2", "hypot", "copysign", "min", "max", "mul_add",
];

/// The summary of `extern "nabi"` runtime function `name`.
#[must_use]
pub fn runtime(name: &str) -> Option<Summary> {
    let math = name
        .strip_prefix("nabi_f64_")
        .or_else(|| name.strip_prefix("nabi_f32_"))
        .is_some_and(|m| MATH.contains(&m));
    (math || READONLY.contains(&name)).then_some(Summary {
        flags: Summary::READONLY,
        ..Summary::default()
    })
}

/// Set the runtime's summaries (`runtime`) on `m`'s declarations of them.
pub fn set_runtime(m: &mut MirModule) {
    for f in &mut m.functions {
        if f.facts.linkage == Linkage::Extern
            && f.facts.abi == Abi::Nabi
            && f.facts.link_name.is_none()
            && f.facts.wasm_import.is_none()
        {
            if let Some(s) = runtime(&f.name) {
                f.facts.summary.flags |= s.flags;
            }
        }
    }
}

/// Whether `f`'s calls may take its summary: its body is the one a
/// dependent links (not a dispatcher the program replaces, not an
/// instance a dependent makes its own).
fn exported(f: &MirFunction) -> bool {
    f.facts.linkage == Linkage::Defined
        && !f.blocks.is_empty()
        && f.facts.role == FnRole::Plain
        && f.facts.mono_args.is_none()
}

/// The summaries of library `m`'s functions for its NABIHIR.
///
/// `m` is optimized (`READONLY` set by `optimize`); `quiet`: the
/// functions that never reach a safepoint (codegen's). By name, sorted;
/// functions with none are left out.
#[must_use]
pub fn export<S: std::hash::BuildHasher>(
    m: &MirModule,
    quiet: &std::collections::HashSet<FunctionId, S>,
) -> Vec<(String, Summary)> {
    let keeps = keeps(m);
    let mut out: Vec<(String, Summary)> = m
        .functions
        .iter()
        .filter(|f| exported(f))
        // Its copies of other packages' code are not its interface.
        .filter(|f| m.link.lib.as_deref().is_none_or(|l| Link::owns(l, f.path())))
        .filter_map(|f| {
            let mut s = Summary {
                flags: f.facts.summary.flags & Summary::READONLY,
                ..Summary::default()
            };
            if quiet.contains(&f.id) {
                s.flags |= Summary::QUIET;
            }
            let returns = f
                .blocks
                .iter()
                .any(|b| matches!(b.terminator, Terminator::Return(_)));
            if f.facts.inline == Inline::Cold || !returns {
                s.flags |= Summary::COLD;
            }
            if let Some(&(k, r)) = keeps.get(&f.id) {
                s.keeps_none = k;
                s.returns = r & k;
            }
            (!s.is_empty()).then(|| (f.name.clone(), s))
        })
        .collect();
    out.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Whether a value of `t` may be an address: a word, or a struct of one
/// (`bare::Ptr`). Only such parameters are followed (`keeps`): a frame
/// address is what a caller passes (`frame::caps` in codegen).
fn addressy(t: &MirTy) -> bool {
    match t {
        MirTy::Prim(PrimTy::Usize | PrimTy::Isize) => true,
        MirTy::Struct(_, fs) => matches!(fs.as_slice(), [f] if addressy(f)),
        _ => false,
    }
}

/// Per function with a body and a parameter that may be an address
/// (`addressy`), `(keeps_none, returns)` (`Summary`): the greatest fixed
/// point, a parameter kept only when shown to be.
fn keeps(m: &MirModule) -> HashMap<FunctionId, (u64, u64)> {
    let index: HashMap<FunctionId, usize> = m
        .functions
        .iter()
        .enumerate()
        .map(|(i, f)| (f.id, i))
        .collect();
    let mut state: HashMap<FunctionId, (u64, u64)> = HashMap::default();
    let mut callers: HashMap<FunctionId, Vec<FunctionId>> = HashMap::default();
    for f in m.functions.iter().filter(|f| exported(f)) {
        let wanted = (f.param_tys.iter().take(64).enumerate())
            .filter(|(_, t)| addressy(t))
            .fold(0u64, |v, (i, _)| v | (1 << i));
        if wanted == 0 {
            continue;
        }
        state.insert(f.id, (wanted, 0));
        for b in &f.blocks {
            for s in &b.stmts {
                if let Statement::Assign {
                    rvalue: RValue::Call { callee, .. },
                    ..
                } = s
                {
                    let c = callers.entry(*callee).or_default();
                    if c.last() != Some(&f.id) {
                        c.push(f.id);
                    }
                }
            }
        }
    }
    let mut work: Vec<FunctionId> = state.keys().copied().collect();
    let mut queued: HashSet<FunctionId> = work.iter().copied().collect();
    let mut sc = Scratch::default();
    while let Some(id) = work.pop() {
        queued.remove(&id);
        let f = &m.functions[index[&id]];
        let callee = |c: FunctionId| {
            state.get(&c).copied().unwrap_or_else(|| {
                let s = index
                    .get(&c)
                    .map(|&i| m.functions[i].facts.summary)
                    .unwrap_or_default();
                (s.keeps_none, s.returns)
            })
        };
        let old = state[&id];
        let (esc, ret) = sc.run(f, old.0, &callee);
        let new = (old.0 & !esc, old.1 | ret);
        if new != old {
            state.insert(id, new);
            for &c in callers.get(&id).into_iter().flatten() {
                if state.contains_key(&c) && queued.insert(c) {
                    work.push(c);
                }
            }
        }
    }
    state
}

/// Buffers `Scratch::run` reuses.
#[derive(Default)]
struct Scratch {
    /// Per local: the parameters whose value it may hold (or derive from).
    from: Vec<u64>,
    /// Block index by block id.
    at: Vec<u32>,
}

impl Scratch {
    /// Of the parameters `of` of `f`, those that may be kept past a
    /// call, and those that may be returned; `callee`: a callee's
    /// `(keeps_none, returns)`.
    fn run(
        &mut self,
        f: &MirFunction,
        of: u64,
        callee: &dyn Fn(FunctionId) -> (u64, u64),
    ) -> (u64, u64) {
        let from = &mut self.from;
        from.clear();
        from.resize(f.locals.len(), 0);
        for (i, p) in f.params.iter().take(64).enumerate() {
            from[p.0 as usize] |= of & (1 << i);
        }
        let at = &mut self.at;
        at.clear();
        for (i, b) in f.blocks.iter().enumerate() {
            let id = b.id.0 as usize;
            if at.len() <= id {
                at.resize(id + 1, u32::MAX);
            }
            at[id] = i as u32;
        }
        let get = |from: &[u64], o: &Operand| match o {
            Operand::Copy(l) => from[l.0 as usize],
            Operand::Const(_) => 0,
        };
        // Where values flow, to a fixed point (flow-insensitive).
        loop {
            let mut grew = false;
            let mut put = |from: &mut Vec<u64>, l: LocalId, v: u64| {
                let x = &mut from[l.0 as usize];
                if *x | v != *x {
                    *x |= v;
                    grew = true;
                }
            };
            for b in &f.blocks {
                for s in &b.stmts {
                    if let Statement::Assign { target, rvalue } = s {
                        let v = flow(from, rvalue, callee, &get);
                        put(from, *target, v);
                    }
                }
                let mut edge =
                    |from: &mut Vec<u64>, to: nabic_ir_common::BlockId, args: &[Operand]| {
                        let Some(&i) = at.get(to.0 as usize) else {
                            return;
                        };
                        if let Some(tb) = f.blocks.get(i as usize) {
                            for (p, a) in tb.params.iter().zip(args) {
                                let v = get(from, a);
                                put(from, *p, v);
                            }
                        }
                    };
                match &b.terminator {
                    Terminator::Goto { target, args } => edge(from, *target, args),
                    Terminator::If {
                        then_bb,
                        then_args,
                        else_bb,
                        else_args,
                        ..
                    } => {
                        edge(from, *then_bb, then_args);
                        edge(from, *else_bb, else_args);
                    }
                    _ => {}
                }
            }
            if !grew {
                break;
            }
        }
        // Where they may be kept.
        let (mut esc, mut ret) = (0u64, 0u64);
        for b in &f.blocks {
            for s in &b.stmts {
                match s {
                    Statement::Assign { rvalue, .. } => match rvalue {
                        RValue::Call {
                            callee: c, args, ..
                        } => {
                            let (k, _) = callee(*c);
                            for (j, a) in args.iter().enumerate() {
                                if j >= 64 || (k >> j) & 1 == 0 {
                                    esc |= get(from, a);
                                }
                            }
                        }
                        RValue::CallIndirect {
                            callee: c, args, ..
                        } => {
                            esc |= get(from, c);
                            for a in args {
                                esc |= get(from, a);
                            }
                        }
                        RValue::HeapBox { value, .. } => esc |= get(from, value),
                        RValue::BufAtomic { args, .. } => {
                            for a in args {
                                esc |= get(from, a);
                            }
                        }
                        RValue::AddrOf(l) => esc |= from[l.0 as usize],
                        _ => {}
                    },
                    Statement::ArrayWrite { value, .. }
                    | Statement::BufStore { value, .. }
                    | Statement::HeapStore { value, .. }
                    | Statement::RawStore { value, .. }
                    | Statement::StaticStore { value, .. } => esc |= get(from, value),
                    Statement::StorageDead(_) | Statement::Loc(..) => {}
                }
            }
            match &b.terminator {
                Terminator::Return(Some(o)) => ret |= get(from, o),
                Terminator::Suspend { value: Some(o), .. } => esc |= get(from, o),
                _ => {}
            }
        }
        (esc, ret)
    }
}

/// The parameters `rv`'s value may hold or derive from.
fn flow(
    from: &[u64],
    rv: &RValue,
    callee: &dyn Fn(FunctionId) -> (u64, u64),
    get: &dyn Fn(&[u64], &Operand) -> u64,
) -> u64 {
    match rv {
        RValue::Use(o)
        | RValue::UnaryOp { operand: o, .. }
        | RValue::FieldRead { target: o, .. }
        | RValue::Cast { source: o, .. }
        | RValue::Bitcast { source: o, .. } => get(from, o),
        RValue::BinOp { lhs, rhs, .. } => get(from, lhs) | get(from, rhs),
        RValue::WithField { source, value, .. } => get(from, source) | get(from, value),
        RValue::StructInit { fields: xs, .. } | RValue::ArrayInit { elements: xs, .. } => {
            xs.iter().fold(0, |v, x| v | get(from, x))
        }
        RValue::Call {
            callee: c, args, ..
        } => {
            let (_, r) = callee(*c);
            args.iter()
                .enumerate()
                .filter(|(j, _)| *j < 64 && (r >> j) & 1 != 0)
                .fold(0, |v, (_, a)| v | get(from, a))
        }
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each runtime summary is of a function core declares.
    #[test]
    fn runtime_summaries_name_declared_functions() {
        let rt = include_str!("../../../library/core/src/rt.nabi");
        let math = MATH
            .iter()
            .flat_map(|m| ["f32", "f64"].map(|t| format!("nabi_{t}_{m}")));
        for name in READONLY.iter().map(|n| (*n).to_owned()).chain(math) {
            assert!(rt.contains(&format!("    fn {name}(")), "{name}");
            assert!(runtime(&name).is_some_and(|s| s.has(Summary::READONLY)));
        }
        for name in [
            "nabi_buf_copy",
            "nabi_hash_keys",
            "nabi_f64_frexp",
            "nabi_throw_site",
        ] {
            assert_eq!(runtime(name), None, "{name}");
        }
    }
}
