//! Compile-time evaluation inside lowering.
//!
//! Consts, array lengths, const generic arguments, static initializers and
//! `const fn` literal handlers are lowered here and run by
//! `nabic_consteval`'s interpreter. Callees are lowered on demand with a
//! context (`CtxBodies`); none in the pre-body pass (`syntactic_int`).

mod block;

pub(crate) use crate::lower::consteval::block::*;
pub(crate) use nabic_consteval::*;

#[allow(clippy::wildcard_imports)]
use crate::lower::*;
use nabic_consteval::Bodies;
use nabic_core::HashMap;
use nabic_diagnostic::{Code, Span};
use std::rc::Rc;

/// `v` as a constant expression of type `ty`; `None` when it holds
/// something only a run-time value can be (a buffer, a function).
pub(crate) fn embed(ctx: &LowerCtx<'_>, v: &Value, ty: &HirTy, span: Span) -> Option<HirExpr> {
    let konst = |c: ConstVal| HirExpr {
        kind: HirExprKind::Const(c),
        ty: ty.clone(),
        span,
    };
    let kind = match v {
        Value::Unit => return Some(konst(ConstVal::Unit)),
        Value::Bool(b) => return Some(konst(ConstVal::Bool(*b))),
        Value::Int(i, p) => {
            return Some(konst(ConstVal::Int {
                value: *i,
                ty: prim_of(ty).unwrap_or(*p),
            }));
        }
        Value::Float(f, p) => {
            let p = prim_of(ty).unwrap_or(*p);
            #[allow(clippy::cast_possible_truncation)]
            let bits = if p == PrimTy::F32 {
                u64::from((*f as f32).to_bits())
            } else {
                f.to_bits()
            };
            return Some(konst(ConstVal::Float { bits, ty: p }));
        }
        Value::Char(c) => return Some(konst(ConstVal::Char(*c))),
        Value::Str(s) => return Some(konst(ConstVal::Str(s.to_string()))),
        Value::None => return Some(konst(ConstVal::None)),
        Value::Tuple(vs) => {
            let tys: Vec<HirTy> = match ty {
                HirTy::Tuple(ts) => ts.clone(),
                // A `throws` carrier: `(is_err, ok, err)`, no unit `ok`.
                HirTy::Throws { ok, err } if crate::lower::throws::no_ok_slot(ok) => {
                    vec![HirTy::Prim(PrimTy::Bool), (**err).clone()]
                }
                HirTy::Throws { ok, err } => {
                    vec![HirTy::Prim(PrimTy::Bool), (**ok).clone(), (**err).clone()]
                }
                _ => return None,
            };
            if tys.len() != vs.len() {
                return None;
            }
            let fields = vs
                .iter()
                .zip(&tys)
                .map(|(v, t)| embed(ctx, v, t, span))
                .collect::<Option<Vec<_>>>()?;
            HirExprKind::Tuple { fields }
        }
        Value::Array(vs) => {
            let HirTy::Array(elem, _) = ty else {
                return None;
            };
            let elements = vs
                .iter()
                .map(|v| embed(ctx, v, elem, span))
                .collect::<Option<Vec<_>>>()?;
            HirExprKind::ArrayInit {
                elem_ty: (**elem).clone(),
                elements,
            }
        }
        Value::Struct(id, vs) => {
            let def = ctx.struct_def(*id);
            if def.union_kind.is_some() || def.fields.len() != vs.len() {
                return None;
            }
            let fields = vs
                .iter()
                .zip(&def.fields)
                .map(|(v, f)| embed(ctx, v, &f.ty, span))
                .collect::<Option<Vec<_>>>()?;
            HirExprKind::StructInit {
                struct_id: *id,
                fields,
            }
        }
        Value::Enum(id, idx, vs) => {
            let def = ctx.enum_def(*id);
            let var = def.variants.get(*idx as usize)?;
            let tys: Vec<HirTy> = match (&var.payload_ty, var.arity) {
                (_, 0) => Vec::new(),
                (Some(t), 1) => vec![t.clone()],
                (Some(HirTy::Tuple(ts)), _) => ts.clone(),
                _ => return None,
            };
            if tys.len() != vs.len() {
                return None;
            }
            let args = vs
                .iter()
                .zip(&tys)
                .map(|(v, t)| embed(ctx, v, t, span))
                .collect::<Option<Vec<_>>>()?;
            HirExprKind::EnumVariantInit {
                enum_id: *id,
                variant_index: *idx,
                args,
            }
        }
        Value::Sum(idx, payload) => {
            let HirTy::Sum(members) = ty else {
                return None;
            };
            let member = members.get(*idx as usize)?;
            let value = match payload {
                Some(p) => Some(Box::new(embed(ctx, p, member, span)?)),
                None => None,
            };
            HirExprKind::SumWrap {
                sum_ty: ty.clone(),
                variant_index: *idx,
                value,
            }
        }
        Value::Buf(_) | Value::Fn(_) | Value::Closure(..) | Value::Union(_) => return None,
    };
    Some(HirExpr {
        kind,
        ty: ty.clone(),
        span,
    })
}

/// A `const fn` calls only `const fn`s and reads no `static`,
/// evaluated or not. Lifted closures are checked with their definer.
pub(crate) fn check_const_fns(env: &Env<'_>, fns: &[&HirFunction]) {
    let Some(first) = fns.first() else { return };
    let no_subst = HashMap::default();
    let ctx = LowerCtx::new(env, &no_subst, None, first.span);
    let mut bodies = CtxBodies::new(&ctx);
    let mut memo: HashMap<FunctionId, Option<String>> = HashMap::default();
    for f in fns {
        let Some(body) = &f.body else { continue };
        if let Some((at, why)) = impure(&mut bodies, body, &mut memo) {
            let why = why.trim_end_matches("");
            let d = nabic_diagnostic::NabiDiagnostic::error(
                Code::CONST_EVAL,
                at,
                format!("`const fn {}` {why}", leaf_name(&f.name)),
            );
            env.sink.diagnostics.borrow_mut().push(d);
        }
    }
}

/// The first operation in `body` a `const fn` may not perform: where,
/// and what.
fn impure(
    bodies: &mut CtxBodies<'_, '_>,
    body: &HirBlock,
    memo: &mut HashMap<FunctionId, Option<String>>,
) -> Option<(Span, String)> {
    // A panic's message is never needed: reaching the panic fails the
    // evaluation anyway.
    let mut in_panic: nabic_core::HashSet<*const HirExpr> = nabic_core::HashSet::default();
    let mark = |b: &HirBlock, in_panic: &mut nabic_core::HashSet<*const HirExpr>| {
        for s in &b.stmts {
            if let HirStmt::Panic(p) = s {
                crate::each_expr(p, &mut |x| {
                    in_panic.insert(std::ptr::from_ref(x));
                });
            }
        }
    };
    mark(body, &mut in_panic);
    let mut sites: Vec<(Span, Result<FunctionId, String>)> = Vec::new();
    crate::each_block(body, &mut |e| match &e.kind {
        _ if in_panic.contains(&std::ptr::from_ref(e)) => {}
        HirExprKind::Block(b) => mark(b, &mut in_panic),
        HirExprKind::Call { callee, .. } => sites.push((e.span, Ok(*callee))),
        HirExprKind::StaticRef(name) => {
            sites.push((
                e.span,
                Err(format!("reads the static `{}`", leaf_name(name))),
            ));
        }
        HirExprKind::RawRead { .. } | HirExprKind::RawWrite { .. } | HirExprKind::AddrOf { .. } => {
            sites.push((e.span, Err("accesses raw memory".to_owned())));
        }
        HirExprKind::BufAtomic { .. } => {
            sites.push((e.span, Err("performs an atomic operation".to_owned())));
        }
        HirExprKind::CoroNew { .. } | HirExprKind::Suspend { .. } => {
            sites.push((e.span, Err("runs a coroutine".to_owned())));
        }
        _ => {}
    });
    for (at, site) in sites {
        let why = match site {
            Ok(id) => callee_impure(bodies, id, memo),
            Err(why) => Some(why),
        };
        if let Some(why) = why {
            return Some((at, why));
        }
    }
    None
}

/// Why calling `id` is not allowed in a `const fn`, if it is not.
fn callee_impure(
    bodies: &mut CtxBodies<'_, '_>,
    id: FunctionId,
    memo: &mut HashMap<FunctionId, Option<String>>,
) -> Option<String> {
    if let Some(r) = memo.get(&id) {
        return r.clone();
    }
    // A `const fn` is checked on its own.
    if is_const_fn(bodies.ctx, id) {
        return None;
    }
    // Optimistic while in progress (recursion).
    memo.insert(id, None);
    let r = match bodies.callee(id) {
        Callee::Body(func, _) => func.body.as_ref().and_then(|b| {
            impure(bodies, b, memo)
                .map(|(_, why)| format!("calls `{}`, which {why}", leaf_name(&func.name)))
        }),
        Callee::Extern(name) if evaluable_extern(&name) => None,
        Callee::Extern(name) => Some(format!("calls the extern function `{name}`")),
        Callee::Refused(why) => Some(why),
        Callee::Poisoned => None,
    };
    memo.insert(id, r.clone());
    r
}

/// Bodies for an evaluation inside lowering: lowered extras as they are,
/// other declarations lowered on demand.
pub(crate) struct CtxBodies<'c, 'a> {
    ctx: &'c LowerCtx<'a>,
    cache: HashMap<FunctionId, (Rc<HirFunction>, bool)>,
}

impl<'c, 'a> CtxBodies<'c, 'a> {
    pub(crate) fn new(ctx: &'c LowerCtx<'a>) -> Self {
        Self {
            ctx,
            cache: HashMap::default(),
        }
    }
}

/// A standard library tier.
fn is_std(module: &str) -> bool {
    nabic_core::in_toolchain(module)
}

impl Bodies for CtxBodies<'_, '_> {
    fn callee(&mut self, id: FunctionId) -> Callee {
        if let Some((f, std)) = self.cache.get(&id) {
            return Callee::Body(Rc::clone(f), *std);
        }
        let env = self.ctx.env;
        // Report the source name, not a generic instance's mangled
        // `name__mono__..`; the message is user-facing.
        let name = |f: &str| {
            let leaf = leaf_name(f);
            leaf.split(nabic_ir_common::MONO_MARK)
                .next()
                .unwrap_or(leaf)
                .to_owned()
        };
        let found = env
            .sink
            .extras
            .borrow()
            .iter()
            .find(|f| f.id == id)
            .cloned();
        let (func, std) = if let Some(f) = found {
            // Lifted closures / thunks have no origin and belong to
            // their (const) definer.
            let origin = env.sink.fn_origin.borrow().get(&id).copied();
            if origin.is_some_and(|(is_const, _)| !is_const) {
                return Callee::Refused(not_const(&name(&f.name)));
            }
            (f, origin.is_some_and(|(_, std)| std))
        } else if let Some(decl) = env.decl(id) {
            let def = decl.def();
            if env.is(crate::lower::FnSet::ExternC, id)
                || env.is(crate::lower::FnSet::NabiExtern, id)
            {
                return Callee::Extern(def.name.clone());
            }
            let module = match decl {
                MethodOrFn::Fn(f) => crate::lower::fn_module(env, f),
                MethodOrFn::Method { module, .. } => module.clone(),
            };
            let is_const = def
                .attrs
                .iter()
                .any(|a| a.path == [nabic_ast::CONST_FN_ATTR]);
            if !is_const {
                return Callee::Refused(not_const(&name(&def.name)));
            }
            if def.body.is_none() {
                return Callee::Refused(format!(
                    "calls `{}`, whose body is not available",
                    name(&def.name)
                ));
            }
            if !env.sink.const_lowering.borrow_mut().insert(id) {
                return Callee::Refused(format!("`{}` depends on itself", name(&def.name)));
            }
            let f = crate::lower::collect::lower_decl(env, id.0 as usize, false);
            env.sink.const_lowering.borrow_mut().remove(&id);
            (f, is_std(&module))
        } else {
            return Callee::Refused("calls a function that is still being compiled".to_owned());
        };
        if func
            .body
            .as_ref()
            .is_some_and(crate::lower::collect::hir_block_has_unknown)
        {
            return Callee::Poisoned;
        }
        let func = Rc::new(func);
        self.cache.insert(id, (Rc::clone(&func), std));
        Callee::Body(func, std)
    }
}

fn not_const(name: &str) -> String {
    format!("calls `{name}`, which is not a `const fn`")
}

/// Evaluate a lowered expression in `ctx`.
pub(crate) fn eval_in(ctx: &LowerCtx<'_>, e: &HirExpr) -> R<Value> {
    if crate::lower::collect::hir_expr_has_unknown(e) {
        return Err(Stop::Poisoned);
    }
    let mut bodies = CtxBodies::new(ctx);
    Interp::new(&mut bodies).eval(e)
}

fn report(ctx: &LowerCtx<'_>, at: Span, what: &str, op: Span, why: &str) {
    let mut d = nabic_diagnostic::NabiDiagnostic::error(
        Code::CONST_EVAL,
        at,
        format!("{what} cannot be evaluated at compile time: {why}"),
    );
    if op != at && !op.is_dummy() {
        d = d.with_secondary(op, "here");
    }
    ctx.env.sink.diagnostics.borrow_mut().push(d);
}

/// Lower const `name`'s value with `lower`, unless it is already being
/// lowered: a const defined through itself is an error (`None`).
pub(crate) fn guard(
    ctx: &mut LowerCtx<'_>,
    name: &str,
    span: Span,
    lower: impl FnOnce(&mut LowerCtx<'_>) -> HirExpr,
) -> Option<HirExpr> {
    let stack = &ctx.env.sink.const_stack;
    if stack.borrow().iter().any(|n| n == name) {
        let d = nabic_diagnostic::NabiDiagnostic::error(
            Code::CYCLIC_DEFINITION,
            span,
            format!("const `{}` is defined in terms of itself", leaf_name(name)),
        );
        ctx.env.sink.diagnostics.borrow_mut().push(d);
        return None;
    }
    stack.borrow_mut().push(name.to_owned());
    let v = crate::lower::infer::scoped(ctx, lower);
    ctx.env.sink.const_stack.borrow_mut().pop();
    Some(v)
}

/// A const item's value at a use: evaluated, and embedded
/// as a constant when it is plain data. Not evaluable is an error.
pub(crate) fn const_item(ctx: &LowerCtx<'_>, name: &str, v: HirExpr) -> HirExpr {
    constant(ctx, &format!("const `{}`", leaf_name(name)), v)
}

/// `v`, a constant expression (`what` in errors), evaluated and embedded
/// when it is plain data.
pub(crate) fn constant(ctx: &LowerCtx<'_>, what: &str, v: HirExpr) -> HirExpr {
    match eval_in(ctx, &v) {
        Ok(val) => embed(ctx, &val, &v.ty, v.span).unwrap_or(v),
        Err(Stop::Runtime(op, why) | Stop::Fail(op, why) | Stop::Limit(op, why)) => {
            // Not again when lowering `v` reported it (`200 * 2` in `u8`).
            let reported = ctx
                .env
                .sink
                .diagnostics
                .borrow()
                .iter()
                .any(|d| d.is_error() && d.primary.as_ref().is_some_and(|l| l.span == op));
            if !reported {
                report(ctx, v.span, what, op, &why);
            }
            poison(v.span)
        }
        Err(_) => v,
    }
}

/// A `#[literal]` handler call runs now when its arguments are constants;
/// a panic or `throw` inside is a compile error at the literal.
pub(crate) fn fold_literal(ctx: &LowerCtx<'_>, h: &mut HirExpr, at: Span) {
    if let HirExprKind::Call { callee, .. } = &h.kind {
        if is_const_fn(ctx, *callee) {
            fold_call(ctx, h, at);
            return;
        }
    }
    match &mut h.kind {
        HirExprKind::Block(b) => {
            for s in &mut b.stmts {
                if let HirStmt::Let { init: e, .. } | HirStmt::Expr(e) = s {
                    fold_literal(ctx, e, at);
                }
            }
            if let Some(t) = &mut b.trailing {
                fold_literal(ctx, t, at);
            }
        }
        HirExprKind::SumWrap { value: Some(e), .. }
        | HirExprKind::Cast { operand: e, .. }
        | HirExprKind::TupleField { target: e, .. } => fold_literal(ctx, e, at),
        _ => {}
    }
}

fn is_const_fn(ctx: &LowerCtx<'_>, id: FunctionId) -> bool {
    let env = ctx.env;
    if let Some(&(is_const, _)) = env.sink.fn_origin.borrow().get(&id) {
        return is_const;
    }
    env.decl(id).is_some_and(|d| {
        let def = d.def();
        def.attrs
            .iter()
            .any(|a| a.path == [nabic_ast::CONST_FN_ATTR])
    })
}

fn fold_call(ctx: &LowerCtx<'_>, h: &mut HirExpr, at: Span) {
    match eval_in(ctx, h) {
        Ok(val) => {
            // A handler that threw: its carrier says so.
            if let (HirTy::Throws { .. }, Value::Tuple(fs)) = (&h.ty, &val) {
                if let (Some(Value::Bool(true)), Some(err)) = (fs.first(), fs.last()) {
                    report(
                        ctx,
                        at,
                        "literal",
                        at,
                        &format!("the handler threw {}", describe(err)),
                    );
                    return;
                }
            }
            if let Some(c) = embed(ctx, &val, &h.ty, h.span) {
                *h = c;
            }
        }
        Err(Stop::Fail(op, why)) => report(ctx, at, "literal", op, &why),
        // Run-time arguments, or too long to run now: the call stays.
        Err(_) => {}
    }
}

/// An integer in a type position (array length, const generic argument)
/// lowered in `ctx` and evaluated; a `char` comes back tagged `char`.
pub(crate) fn eval_scalar(ctx: &LowerCtx<'_>, e: &Expr) -> Result<(i128, Option<PrimTy>), String> {
    let mut cx = ctx.clone();
    // A const parameter bound without its type reads as a `usize`.
    for (n, t) in ctx.ty_subst.iter() {
        if let Some((v, None)) = t.as_const() {
            cx.splices
                .insert(n.clone(), hir_int(v, PrimTy::Usize, e.span));
        }
    }
    let mark = cx.probe_mark();
    let h = lower_expected(&mut cx, e, HirTy::Unknown);
    let lowering_error = cx
        .env
        .sink
        .diagnostics
        .borrow()
        .get(mark.diagnostics)
        .map(|d| d.message.clone());
    let r = match lowering_error {
        Some(why) => Err(why),
        None => match eval_in(&cx, &h) {
            Ok(Value::Int(v, _)) => Ok((v, None)),
            Ok(Value::Bool(b)) => Ok((i128::from(b), Some(PrimTy::Bool))),
            Ok(Value::Char(c)) if h.ty == HirTy::Prim(PrimTy::Char) => {
                Ok((i128::from(c), Some(PrimTy::Char)))
            }
            Ok(_) => Err("not an integer".to_owned()),
            Err(Stop::Runtime(_, why) | Stop::Fail(_, why) | Stop::Limit(_, why)) => Err(why),
            Err(_) => Err("not a constant".to_owned()),
        },
    };
    cx.rollback(&mark);
    r
}

/// Evaluate every const item of the user's package once, so an unused one
/// is checked too.
pub(crate) fn check_user_consts(env: &Env<'_>) {
    let no_subst = HashMap::default();
    let mut keys: Vec<&String> = env
        .consts
        .keys()
        .filter(|k| package_of(&module_of(k)).is_empty() && !in_generic_fn(env, k))
        .collect();
    keys.sort();
    for key in keys {
        let span = env.consts[key].1.span;
        let mut ctx = LowerCtx::new(env, &no_subst, None, span);
        let module = module_of(key);
        // Impl consts (`S::N`) are checked where they are read.
        let segs: Vec<&str> = module.split("::").collect();
        if !module.is_empty() && ctx.canon_type(&segs).as_deref() == Some(module.as_str()) {
            continue;
        }
        ctx.current_module = module;
        let path = nabic_ast::Path {
            segments: vec![nabic_ast::PathSegment {
                name: leaf_name(key).to_owned(),
                generics: Vec::new(),
            }],
            span,
            origin: None,
        };
        crate::lower::expr::lower_path(&mut ctx, &path, span);
    }
    // Field defaults of the package's non-generic structs.
    let structs: Vec<HirStructDef> = env
        .defs
        .structs
        .borrow()
        .iter()
        .filter(|d| d.generic_args.is_empty() && package_of(&module_of(&d.name)).is_empty())
        .filter(|d| d.fields.iter().any(|f| f.default.is_some()))
        .cloned()
        .collect();
    for d in structs {
        for f in &d.fields {
            if let Some(value) = &f.default {
                let mut ctx = LowerCtx::new(env, &no_subst, None, value.span);
                crate::lower::expr::field_default(&mut ctx, &d.name, &f.name, &f.ty, value);
            }
        }
    }
}

/// A static's initializer function, run now: its value when it is a
/// primitive constant; `None` keeps the lazy initializer.
pub(crate) fn static_init(env: &Env<'_>, init_fn: &HirFunction) -> Option<ConstVal> {
    let no_subst = HashMap::default();
    let ctx = LowerCtx::new(env, &no_subst, None, init_fn.span);
    let body = init_fn.body.as_ref()?;
    if crate::lower::collect::hir_block_has_unknown(body) {
        return None;
    }
    let mut bodies = CtxBodies::new(&ctx);
    let mut interp = Interp::new(&mut bodies);
    let (Ok(v) | Err(Stop::Return(v))) = interp.block(body, &mut Frame::default()) else {
        return None;
    };
    if let (HirTy::Array(e, _), Value::Array(vs)) = (&init_fn.ret_ty, &v) {
        if **e == HirTy::Prim(PrimTy::U8) {
            return vs
                .iter()
                .map(|x| match x {
                    Value::Int(i, _) => u8::try_from(*i).ok(),
                    _ => None,
                })
                .collect::<Option<Vec<u8>>>()
                .map(ConstVal::Bytes);
        }
    }
    match embed(&ctx, &v, &init_fn.ret_ty, init_fn.span)?.kind {
        HirExprKind::Const(c) => Some(c),
        _ => None,
    }
}

/// `a::b::C` as written.
fn path_text(p: &nabic_ast::Path) -> String {
    p.segments
        .iter()
        .map(|s| s.name.as_str())
        .collect::<Vec<_>>()
        .join("::")
}

pub fn syntactic_int(
    c: &nabic_ast::ConstExpr,
    lookup: &dyn Fn(&str) -> Option<i128>,
) -> Option<i128> {
    fn lower(e: &Expr, lookup: &dyn Fn(&str) -> Option<i128>) -> Option<HirExpr> {
        let int = |v: i128| hir_int(v, PrimTy::I64, e.span);
        let kind = match &e.kind {
            ExprKind::Literal(AstLit::Int(s)) => return parse_int_literal(s).map(|(v, _)| int(v)),
            ExprKind::Path(p) => return lookup(&path_text(p)).map(int),
            ExprKind::OpPrefix { op, operand } if op == "-" => HirExprKind::PrimUnaryOp {
                op: PrimOp::Neg,
                operand: Box::new(lower(operand, lookup)?),
            },
            ExprKind::OpInfix { op, lhs, rhs } => HirExprKind::PrimBinOp {
                op: match op.as_str() {
                    "+" => PrimOp::Add,
                    "-" => PrimOp::Sub,
                    "*" => PrimOp::Mul,
                    "/" => PrimOp::Div,
                    "%" => PrimOp::Rem,
                    "%%" => PrimOp::Mod,
                    "<<" => PrimOp::Shl,
                    ">>" => PrimOp::Shr,
                    "&" => PrimOp::BitAnd,
                    "|" => PrimOp::BitOr,
                    "^" => PrimOp::BitXor,
                    _ => return None,
                },
                lhs: Box::new(lower(lhs, lookup)?),
                rhs: Box::new(lower(rhs, lookup)?),
            },
            _ => return None,
        };
        Some(HirExpr {
            kind,
            ty: HirTy::Prim(PrimTy::I64),
            span: e.span,
        })
    }
    let h = match c {
        nabic_ast::ConstExpr::Literal(AstLit::Int(s)) => {
            return parse_int_literal(s).map(|(v, _)| v);
        }
        nabic_ast::ConstExpr::Path(p) => return lookup(&path_text(p)),
        nabic_ast::ConstExpr::Literal(_) => return None,
        nabic_ast::ConstExpr::Expr(e) => lower(e, lookup)?,
    };
    match Interp::new(&mut NoBodies).eval(&h) {
        Ok(Value::Int(v, _)) => Some(v),
        _ => None,
    }
}
