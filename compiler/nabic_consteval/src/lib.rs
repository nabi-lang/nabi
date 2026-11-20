//! Compile-time evaluation: an interpreter over HIR for consts, array
//! lengths, const generic arguments, static initializers and `const fn`
//! literal handlers. Callees come from `Bodies`: lowered on demand with a
//! context (`nabic_typeck`'s `CtxBodies`), none in the pre-body pass
//! (`syntactic_int`).
//!
//! Integers are `i128` tagged with their primitive and range-checked:
//! overflow, division by zero, bad shifts and failed float-to-int casts are
//! errors (only `wrapping_*` wraps). `STEP_LIMIT` / `DEPTH_LIMIT` bound
//! loops and calls. Only `const fn`s are entered.

// `a` / `b` operands, `p` their primitive, `f` the frame, `v` a value.
#![allow(clippy::many_single_char_names)]

mod externs;
mod ops;

pub use crate::externs::*;
pub(crate) use crate::ops::{binop, cast, index_of, reinterpret, unop, zero};

use HirExprKind as K;
use PrimOp as O;
use nabic_core::HashMap;
use nabic_diagnostic::Span;
use nabic_hir::{
    EnumId, HirBlock, HirExpr, HirExprKind, HirFunction, HirPattern, HirStmt, HirTy, StructId,
};
use nabic_ir_common::{ConstVal, FunctionId, LocalId, PrimOp, PrimTy};
use nabir_abi::float;
use nabir_abi::fmt::spec::unpack;
use nabir_abi::fmt::text as fmt_text;
use nabir_abi::half;
use nabir_abi::str as str_ops;
use nabir_abi::unicode;
use nabir_abi::unicode::{norm, segment};
use std::cell::RefCell;
use std::rc::Rc;

const STEP_LIMIT: u64 = 4_000_000;
/// `core::collections::raw::buf_len`, a buffer's byte-size check.
const BUF_LEN: &str = "core::collections::raw::buf_len";
const DEPTH_LIMIT: u32 = 128;

thread_local! {
    /// Bits of `usize` / `isize` in the program being lowered
    /// (`HirInput::pointer_width`, set by `lower_full`).
    static POINTER_WIDTH: std::cell::Cell<u32> = const { std::cell::Cell::new(64) };
}

pub fn set_pointer_width(bits: u32) {
    POINTER_WIDTH.set(if bits == 0 { 64 } else { bits });
}

thread_local! {
    /// `HirInput::stdcall_system` of the program being lowered.
    static STDCALL_SYSTEM: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub fn set_stdcall_system(on: bool) {
    STDCALL_SYSTEM.set(on);
}

/// `extern "system"` is `stdcall` on the target (a type of its own).
pub fn stdcall_system() -> bool {
    STDCALL_SYSTEM.get()
}

/// Bits of `usize` / `isize` on the target.
pub fn pointer_width() -> u32 {
    POINTER_WIDTH.get()
}

#[derive(Clone, Debug)]
pub enum Value {
    Unit,
    Bool(bool),
    Int(i128, PrimTy),
    /// `f32` values are kept rounded to `f32`.
    Float(f64, PrimTy),
    /// `char` / `c_char` code point.
    Char(u32),
    Str(Rc<str>),
    None,
    Tuple(Vec<Value>),
    Array(Vec<Value>),
    Struct(StructId, Vec<Value>),
    Enum(EnumId, u32, Vec<Value>),
    Sum(u32, Option<Box<Value>>),
    /// A `GcBuf`: shared, mutable; never embedded.
    Buf(Rc<RefCell<Vec<Value>>>),
    Fn(FunctionId),
    Closure(FunctionId, Box<Value>),
    /// A union's storage: the value last written; reads reinterpret it.
    Union(Box<Value>),
}

/// Why evaluation stopped: an error, or a control transfer in flight.
pub enum Stop {
    /// Needs a run-time value or operation.
    Runtime(Span, String),
    /// The evaluation itself failed (panic, overflow).
    Fail(Span, String),
    /// Past the step or call depth limit: an error where a constant is
    /// required, the run-time call where folding is only a chance.
    Limit(Span, String),
    /// Ran into code that already has errors; nothing more to report.
    Poisoned,
    Break(Option<String>, Value),
    Continue(Option<String>),
    Return(Value),
}

pub type R<T> = Result<T, Stop>;

pub enum Callee {
    /// The body, and whether it is standard library code.
    Body(Rc<HirFunction>, bool),
    Extern(String),
    /// Not callable at compile time: the reason.
    Refused(String),
    Poisoned,
}

pub trait Bodies {
    fn callee(&mut self, id: FunctionId) -> Callee;
}

/// No bodies at all (pre-pass).
pub struct NoBodies;

impl Bodies for NoBodies {
    fn callee(&mut self, _: FunctionId) -> Callee {
        Callee::Refused("calls a function".to_owned())
    }
}

pub type Frame = HashMap<LocalId, Value>;

pub struct Interp<'b> {
    bodies: &'b mut dyn Bodies,
    steps: u64,
    depth: u32,
}

pub fn int_range(p: PrimTy) -> (i128, i128) {
    let bits = p.bit_width(pointer_width());
    if p.is_signed_int() {
        (-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1)
    } else {
        (0, (1i128 << bits) - 1)
    }
}

/// Two's-complement wrap of `v` into `p`.
pub(crate) fn wrap(v: i128, p: PrimTy) -> i128 {
    let bits = p.bit_width(pointer_width());
    let mask = (1u128 << bits) - 1;
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_wrap)]
    let low = (v as u128) & mask;
    #[allow(clippy::cast_possible_wrap)]
    if p.is_signed_int() && low >> (bits - 1) == 1 {
        (low as i128) - (1i128 << bits)
    } else {
        low as i128
    }
}

#[allow(clippy::cast_possible_truncation)]
pub(crate) fn round(v: f64, p: PrimTy) -> f64 {
    if p == PrimTy::F32 {
        f64::from(v as f32)
    } else {
        v
    }
}

pub const fn prim_of(ty: &HirTy) -> Option<PrimTy> {
    match ty {
        HirTy::Prim(p) => Some(*p),
        _ => None,
    }
}

pub(crate) fn from_const(c: &ConstVal, ty: &HirTy) -> Value {
    match c {
        ConstVal::Unit => Value::Unit,
        ConstVal::Bool(b) => Value::Bool(*b),
        ConstVal::Int { value, ty: p } => Value::Int(*value, prim_of(ty).unwrap_or(*p)),
        #[allow(clippy::cast_possible_wrap)]
        ConstVal::UInt { value, ty: p } => Value::Int(*value as i128, prim_of(ty).unwrap_or(*p)),
        ConstVal::Float { bits, ty: p } => {
            #[allow(clippy::cast_possible_truncation)]
            let v = if *p == PrimTy::F32 {
                f64::from(f32::from_bits(*bits as u32))
            } else {
                f64::from_bits(*bits)
            };
            let p = prim_of(ty).filter(|t| t.is_float()).unwrap_or(*p);
            Value::Float(round(v, p), p)
        }
        ConstVal::Char(c) => Value::Char(*c),
        ConstVal::Str(s) => Value::Str(s.as_str().into()),
        ConstVal::Bytes(b) => Value::Array(
            b.iter()
                .map(|&x| Value::Int(i128::from(x), PrimTy::U8))
                .collect(),
        ),
        ConstVal::None => Value::None,
    }
}

pub fn describe(v: &Value) -> String {
    match v {
        Value::Str(s) => format!("{s:?}"),
        Value::Int(i, _) => i.to_string(),
        Value::Float(f, _) => f.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Char(c) => char::from_u32(*c).map_or_else(|| c.to_string(), |c| format!("{c:?}")),
        Value::Struct(_, fs) | Value::Enum(_, _, fs) | Value::Tuple(fs)
            if fs.len() == 1 && matches!(fs[0], Value::Str(_)) =>
        {
            describe(&fs[0])
        }
        Value::Sum(_, Some(p)) => describe(p),
        _ => "a value".to_owned(),
    }
}

impl<'b> Interp<'b> {
    pub fn new(bodies: &'b mut dyn Bodies) -> Self {
        Self {
            bodies,
            steps: 0,
            depth: 0,
        }
    }

    pub fn eval(&mut self, e: &HirExpr) -> R<Value> {
        self.expr(e, &mut Frame::default())
    }

    fn step(&mut self, span: Span) -> R<()> {
        self.steps += 1;
        if self.steps > STEP_LIMIT {
            return Err(Stop::Limit(
                span,
                format!("evaluation exceeds the step limit ({STEP_LIMIT})"),
            ));
        }
        Ok(())
    }

    pub fn block(&mut self, b: &HirBlock, f: &mut Frame) -> R<Value> {
        let r = self.block_inner(b, f);
        match (r, &b.label) {
            (Err(Stop::Break(Some(l), v)), Some(own)) if l == *own => Ok(v),
            (r, _) => r,
        }
    }

    fn block_inner(&mut self, b: &HirBlock, f: &mut Frame) -> R<Value> {
        for s in &b.stmts {
            self.stmt(s, f)?;
        }
        match &b.trailing {
            Some(e) => self.expr(e, f),
            None => Ok(Value::Unit),
        }
    }

    fn stmt(&mut self, s: &HirStmt, f: &mut Frame) -> R<()> {
        match s {
            HirStmt::Let { local, init } | HirStmt::Assign { local, value: init } => {
                let v = self.expr(init, f)?;
                f.insert(*local, v);
            }
            HirStmt::ArrayAssign {
                array_local,
                path,
                index,
                value,
                ..
            } => {
                let i = self.expr(index, f)?;
                let v = self.expr(value, f)?;
                let span = index.span;
                let mut at = f.get_mut(array_local);
                for k in path {
                    at = match at {
                        Some(Value::Struct(_, fs) | Value::Tuple(fs)) => fs.get_mut(*k as usize),
                        _ => None,
                    };
                }
                let Some(Value::Array(items)) = at else {
                    return Err(Stop::Runtime(span, "writes a run-time array".to_owned()));
                };
                let len = items.len();
                let slot = index_of(&i, len, span)?;
                items[slot] = v;
            }
            HirStmt::StaticAssign { name, value } => {
                return Err(Stop::Runtime(
                    value.span,
                    format!("writes the static `{}`", leaf_name(name)),
                ));
            }
            HirStmt::Expr(e) => {
                self.expr(e, f)?;
            }
            HirStmt::Return(v) => {
                let v = match v {
                    Some(e) => self.expr(e, f)?,
                    None => Value::Unit,
                };
                return Err(Stop::Return(v));
            }
            HirStmt::Break { label, value } => {
                let v = match value {
                    Some(e) => self.expr(e, f)?,
                    None => Value::Unit,
                };
                return Err(Stop::Break(label.clone(), v));
            }
            HirStmt::Continue { label } => return Err(Stop::Continue(label.clone())),
            HirStmt::Panic(e) => {
                // A message that cannot be built now still panics.
                return match self.expr(e, f) {
                    Err(Stop::Runtime(..)) | Ok(_) => {
                        Err(Stop::Fail(e.span, "panicked".to_owned()))
                    }
                    Err(stop) => Err(stop),
                };
            }
        }
        Ok(())
    }

    /// One loop iteration's outcome: `Ok(None)` to go on, `Ok(Some(v))`
    /// when a `break` left this loop.
    fn iteration(r: R<Value>, label: Option<&String>) -> R<Option<Value>> {
        let ours = |l: &Option<String>| l.is_none() || l.as_ref() == label;
        match r {
            Ok(_) => Ok(None),
            Err(Stop::Break(l, v)) if ours(&l) => Ok(Some(v)),
            Err(Stop::Continue(l)) if ours(&l) => Ok(None),
            Err(e) => Err(e),
        }
    }

    #[allow(clippy::too_many_lines)]
    fn expr(&mut self, e: &HirExpr, f: &mut Frame) -> R<Value> {
        self.step(e.span)?;
        let span = e.span;
        let runtime = |what: &str| Err(Stop::Runtime(span, what.to_owned()));
        match &e.kind {
            K::Const(c) => Ok(from_const(c, &e.ty)),
            K::Local(id) => match f.get(id) {
                Some(v) => Ok(v.clone()),
                None => runtime("reads a run-time variable"),
            },
            K::Block(b) => self.block(b, f),
            K::StaticRef(name) => Err(Stop::Runtime(
                span,
                format!("reads the static `{}`", leaf_name(name)),
            )),
            K::UnresolvedPath(_) => Err(Stop::Poisoned),
            K::PrimBinOp { op, lhs, rhs } => {
                let l = self.expr(lhs, f)?;
                match (op, &l) {
                    (PrimOp::LogicalAnd, Value::Bool(false)) => return Ok(Value::Bool(false)),
                    (PrimOp::LogicalOr, Value::Bool(true)) => return Ok(Value::Bool(true)),
                    _ => {}
                }
                let r = self.expr(rhs, f)?;
                binop(*op, l, r, span)
            }
            K::PrimUnaryOp { op, operand } => {
                let v = self.expr(operand, f)?;
                unop(*op, v, span)
            }
            // A buffer holds elements, not bytes: no byte-size check, and its
            // layout argument is never evaluated.
            K::Call { callee, args } if self.is_buf_len(*callee) => match args.first() {
                Some(len) => self.expr(len, f),
                None => Err(Stop::Poisoned),
            },
            K::Call { callee, args } => {
                let args = self.exprs(args, f)?;
                self.call(*callee, args, span)
            }
            K::FnRef(id) => Ok(Value::Fn(*id)),
            K::CallIndirect { callee, args, .. }
            | K::CallClosure {
                closure: callee,
                args,
                ..
            } => {
                let c = self.expr(callee, f)?;
                let mut args = self.exprs(args, f)?;
                match c {
                    Value::Fn(id) => self.call(id, args, span),
                    Value::Closure(id, env) => {
                        args.insert(0, *env);
                        self.call(id, args, span)
                    }
                    _ => runtime("calls a run-time function value"),
                }
            }
            K::Cast {
                operand,
                from_ty,
                to_ty,
            } => {
                let v = self.expr(operand, f)?;
                cast(v, from_ty, to_ty, span)
            }
            K::ClosureValue { fn_id, env, .. } => {
                let env = self.expr(env, f)?;
                Ok(Value::Closure(*fn_id, Box::new(env)))
            }
            K::If {
                cond,
                then_block,
                else_branch,
            } => {
                if self.cond(cond, f)? {
                    self.block(then_block, f)
                } else if let Some(x) = else_branch {
                    self.expr(x, f)
                } else {
                    Ok(Value::Unit)
                }
            }
            K::While { label, cond, body } => {
                while self.cond(cond, f)? {
                    let r = self.block(body, f);
                    if Self::iteration(r, label.as_ref())?.is_some() {
                        break;
                    }
                }
                Ok(Value::Unit)
            }
            K::Loop { label, body, .. } => loop {
                let r = self.block(body, f);
                if let Some(v) = Self::iteration(r, label.as_ref())? {
                    return Ok(v);
                }
                self.step(span)?;
            },
            K::Match { scrutinee, arms } => {
                let v = self.expr(scrutinee, f)?;
                for arm in arms {
                    if !self.pattern(&arm.pat, &v, f, span)? {
                        continue;
                    }
                    if let Some(g) = &arm.guard {
                        if !self.cond(g, f)? {
                            continue;
                        }
                    }
                    return self.expr(&arm.body, f);
                }
                Err(Stop::Fail(span, "no `match` arm matched".to_owned()))
            }
            K::StructInit { struct_id, fields } => {
                Ok(Value::Struct(*struct_id, self.exprs(fields, f)?))
            }
            K::FieldRead {
                target,
                field_index,
                ..
            } => match self.expr(target, f)? {
                Value::Struct(_, mut fs) if (*field_index as usize) < fs.len() => {
                    Ok(fs.swap_remove(*field_index as usize))
                }
                _ => runtime("reads a field of a run-time value"),
            },
            K::WithField {
                target,
                field_index,
                value,
                ..
            } => {
                let t = self.expr(target, f)?;
                let v = self.expr(value, f)?;
                match t {
                    Value::Struct(id, mut fs) if (*field_index as usize) < fs.len() => {
                        fs[*field_index as usize] = v;
                        Ok(Value::Struct(id, fs))
                    }
                    _ => runtime("writes a field of a run-time value"),
                }
            }
            K::EnumVariantInit {
                enum_id,
                variant_index,
                args,
            } => Ok(Value::Enum(*enum_id, *variant_index, self.exprs(args, f)?)),
            K::SumWrap {
                variant_index,
                value,
                ..
            } => {
                let v = match value {
                    Some(x) => Some(Box::new(self.expr(x, f)?)),
                    None => None,
                };
                Ok(Value::Sum(*variant_index, v))
            }
            K::Tuple { fields } => Ok(Value::Tuple(self.exprs(fields, f)?)),
            K::TupleField { target, index, .. } => match self.expr(target, f)? {
                Value::Tuple(mut fs) if (*index as usize) < fs.len() => {
                    Ok(fs.swap_remove(*index as usize))
                }
                _ => runtime("reads a field of a run-time value"),
            },
            K::ArrayInit { elements, .. } => Ok(Value::Array(self.exprs(elements, f)?)),
            K::ArrayRepeat { value, len, .. } => {
                let v = self.expr(value, f)?;
                self.steps += u64::from(*len);
                self.step(span)?;
                Ok(Value::Array(vec![v; *len as usize]))
            }
            K::ArrayIndex { array, index, .. } => {
                let a = self.expr(array, f)?;
                let i = self.expr(index, f)?;
                match a {
                    Value::Array(mut items) => {
                        let slot = index_of(&i, items.len(), span)?;
                        Ok(items.swap_remove(slot))
                    }
                    _ => runtime("indexes a run-time array"),
                }
            }
            K::BufNew { elem, len } => {
                let n = self.expr(len, f)?;
                let Value::Int(n, _) = n else {
                    return runtime("allocates a run-time sized buffer");
                };
                let n = usize::try_from(n)
                    .map_err(|_| Stop::Fail(span, format!("buffer length {n} is out of range")))?;
                self.steps += n as u64;
                self.step(span)?;
                Ok(Value::Buf(Rc::new(RefCell::new(vec![zero(elem); n]))))
            }
            K::BufGet { buf, index, .. } => {
                let b = self.expr(buf, f)?;
                let i = self.expr(index, f)?;
                let Value::Buf(b) = b else {
                    return runtime("reads a run-time buffer");
                };
                let items = b.borrow();
                let slot = index_of(&i, items.len(), span)?;
                Ok(items[slot].clone())
            }
            K::BufSet {
                buf, index, value, ..
            } => {
                let b = self.expr(buf, f)?;
                let i = self.expr(index, f)?;
                let v = self.expr(value, f)?;
                let Value::Buf(b) = b else {
                    return runtime("writes a run-time buffer");
                };
                let mut items = b.borrow_mut();
                let slot = index_of(&i, items.len(), span)?;
                items[slot] = v;
                Ok(Value::Unit)
            }
            K::BufAtomic { .. } => runtime("performs an atomic operation"),
            K::RawRead { .. } | K::RawWrite { .. } | K::AddrOf { .. } => {
                runtime("accesses raw memory")
            }
            K::SizeOf { ty, align: false } => match plain_size(ty) {
                Some(n) => Ok(Value::Int(i128::from(n), PrimTy::Usize)),
                None => runtime("asks for a type layout"),
            },
            K::SizeOf { .. } => runtime("asks for a type layout"),
            K::TypeDesc(_) => runtime("asks for a type's description"),
            K::UnionWrite { value, .. } => Ok(Value::Union(Box::new(self.expr(value, f)?))),
            K::UnionRead {
                target, field_ty, ..
            } => match self.expr(target, f)? {
                Value::Struct(_, fs) => match fs.into_iter().next() {
                    Some(Value::Union(v)) => reinterpret(*v, field_ty)
                        .ok_or_else(|| Stop::Runtime(span, "reinterprets a union".to_owned())),
                    _ => Err(Stop::Poisoned),
                },
                _ => runtime("reads a run-time union"),
            },
            K::Suspend { .. } | K::CoroNew { .. } => runtime("runs a coroutine"),
        }
    }

    fn is_buf_len(&mut self, id: FunctionId) -> bool {
        matches!(self.bodies.callee(id), Callee::Body(func, _) if func.name == BUF_LEN)
    }

    fn exprs(&mut self, es: &[HirExpr], f: &mut Frame) -> R<Vec<Value>> {
        es.iter().map(|e| self.expr(e, f)).collect()
    }

    fn cond(&mut self, e: &HirExpr, f: &mut Frame) -> R<bool> {
        match self.expr(e, f)? {
            Value::Bool(b) => Ok(b),
            _ => Err(Stop::Poisoned),
        }
    }

    fn call(&mut self, id: FunctionId, args: Vec<Value>, span: Span) -> R<Value> {
        let (func, std) = match self.bodies.callee(id) {
            Callee::Body(func, std) => (func, std),
            Callee::Extern(name) if panics(&name) => {
                let msg = args.first().map_or_else(String::new, |a| match a {
                    Value::Str(s) if name == "nabi_try_failed" => format!(": try! failed: {s}"),
                    Value::Str(s) => format!(": {s}"),
                    _ => String::new(),
                });
                return Err(Stop::Fail(span, format!("panicked{msg}")));
            }
            Callee::Extern(name) if evaluable_extern(&name) => {
                return run_extern(&name, &args).map_err(|e| match e {
                    Ext::Panic(m) => Stop::Fail(span, format!("panicked: {m}")),
                    Ext::No => Stop::Runtime(
                        span,
                        format!("calls `{name}` with arguments it cannot take at compile time"),
                    ),
                });
            }
            Callee::Extern(name) => {
                return Err(Stop::Runtime(
                    span,
                    format!("calls the extern function `{name}`"),
                ));
            }
            Callee::Refused(why) => return Err(Stop::Runtime(span, why)),
            Callee::Poisoned => return Err(Stop::Poisoned),
        };
        // A hand-off's copy of a value built at compile time: nothing else
        // reaches it, so it is the value itself.
        let base = func.name.split("__").next().unwrap_or(&func.name);
        if std && base == "core::marker::hand_over_copy" && args.len() == 1 {
            return Ok(args.into_iter().next().expect("one argument"));
        }
        if std && base == "core::marker::hand_over_into_copy" && args.len() == 2 {
            return Ok(args.into_iter().nth(1).expect("two arguments"));
        }
        let Some(body) = &func.body else {
            return Err(Stop::Runtime(
                span,
                format!("calls `{}`, whose body is not available", func.name),
            ));
        };
        if self.depth >= DEPTH_LIMIT {
            return Err(Stop::Limit(
                span,
                format!("evaluation exceeds the call depth limit ({DEPTH_LIMIT})"),
            ));
        }
        if func.params.len() != args.len() {
            return Err(Stop::Poisoned);
        }
        let mut frame: Frame = func.params.iter().map(|p| p.local).zip(args).collect();
        self.depth += 1;
        let r = self.block(body, &mut frame);
        self.depth -= 1;
        match r {
            Ok(v) | Err(Stop::Return(v)) => Ok(v),
            // Inside a body every operation must be evaluable. A failure
            // inside the library is shown at the call that reached it.
            Err(Stop::Runtime(s, m) | Stop::Fail(s, m)) => {
                Err(Stop::Fail(if std { span } else { s }, m))
            }
            Err(Stop::Break(..) | Stop::Continue(_)) => Err(Stop::Poisoned),
            Err(e) => Err(e),
        }
    }

    fn pattern(&mut self, p: &HirPattern, v: &Value, f: &mut Frame, span: Span) -> R<bool> {
        Ok(match (p, v) {
            (HirPattern::Wildcard, _) => true,
            (HirPattern::Binding { local, .. }, _) => {
                f.insert(*local, v.clone());
                true
            }
            (HirPattern::At { local, sub, .. }, _) => {
                let hit = self.pattern(sub, v, f, span)?;
                if hit {
                    f.insert(*local, v.clone());
                }
                hit
            }
            (HirPattern::Literal { value, .. }, _) => {
                let c = from_const(value, &HirTy::Unknown);
                matches!(binop(PrimOp::Eq, v.clone(), c, span)?, Value::Bool(true))
            }
            (HirPattern::Or(alts), _) => {
                for a in alts {
                    if self.pattern(a, v, f, span)? {
                        return Ok(true);
                    }
                }
                false
            }
            (HirPattern::Tuple(ps), Value::Tuple(vs)) => self.all(ps.iter().zip(vs), f, span)?,
            (HirPattern::Struct { fields, .. }, Value::Struct(_, vs)) => {
                let pairs: Option<Vec<_>> = fields
                    .iter()
                    .map(|(i, p)| vs.get(*i as usize).map(|v| (p, v)))
                    .collect();
                let Some(pairs) = pairs else {
                    return Err(Stop::Poisoned);
                };
                self.all(pairs.into_iter(), f, span)?
            }
            (
                HirPattern::Range {
                    start,
                    end,
                    inclusive,
                    ..
                },
                _,
            ) => {
                let le = |a: Value, b: Value| -> R<bool> {
                    Ok(matches!(binop(PrimOp::Le, a, b, span)?, Value::Bool(true)))
                };
                let lt = |a: Value, b: Value| -> R<bool> {
                    Ok(matches!(binop(PrimOp::Lt, a, b, span)?, Value::Bool(true)))
                };
                let lo = match start {
                    Some(s) => le(from_const(s, &HirTy::Unknown), v.clone())?,
                    None => true,
                };
                let hi = match end {
                    Some(e) if *inclusive => le(v.clone(), from_const(e, &HirTy::Unknown))?,
                    Some(e) => lt(v.clone(), from_const(e, &HirTy::Unknown))?,
                    None => true,
                };
                lo && hi
            }
            (
                HirPattern::EnumVariant {
                    variant_index,
                    args,
                    ..
                },
                Value::Enum(_, idx, vs),
            ) => *variant_index == *idx && self.all(args.iter().zip(vs), f, span)?,
            (
                HirPattern::SumVariant {
                    variant_index,
                    binder,
                    ..
                },
                Value::Sum(idx, payload),
            ) => {
                if *variant_index != *idx {
                    return Ok(false);
                }
                if let Some(b) = binder {
                    f.insert(*b, payload.as_deref().cloned().unwrap_or(Value::None));
                }
                true
            }
            (
                HirPattern::SumMember {
                    variant_index, sub, ..
                },
                Value::Sum(idx, Some(payload)),
            ) => *variant_index == *idx && self.pattern(sub, payload, f, span)?,
            (HirPattern::Slice(s), _) => {
                f.insert(s.whole, v.clone());
                let n = self.expr(&s.len_expr, f)?;
                f.insert(s.len, n.clone());
                let usize_ty = HirTy::Prim(PrimTy::Usize);
                let want = ConstVal::UInt {
                    value: u128::from(s.min_len()),
                    ty: PrimTy::Usize,
                };
                let op = if s.rest.is_some() {
                    PrimOp::Ge
                } else {
                    PrimOp::Eq
                };
                if !matches!(
                    binop(op, n, from_const(&want, &usize_ty), span)?,
                    Value::Bool(true)
                ) {
                    return Ok(false);
                }
                for (e, p) in s.items() {
                    let x = self.expr(e, f)?;
                    if !self.pattern(p, &x, f, span)? {
                        return Ok(false);
                    }
                }
                true
            }
            _ => false,
        })
    }

    fn all<'p>(
        &mut self,
        pairs: impl Iterator<Item = (&'p HirPattern, &'p Value)>,
        f: &mut Frame,
        span: Span,
    ) -> R<bool> {
        for (p, v) in pairs {
            if !self.pattern(p, v, f, span)? {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

pub(crate) fn bad_spec(spec: &str, why: &str) -> Ext {
    let mut m = String::new();
    fmt_text::bad_spec(&mut m, spec, why);
    Ext::Panic(m)
}

/// `a::b::c`'s last segment.
fn leaf_name(name: &str) -> &str {
    name.rsplit_once("::").map_or(name, |(_, l)| l)
}
