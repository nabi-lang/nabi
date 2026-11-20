//! Type inference inside a body.
//!
//! Every unsuffixed number literal of a body is an integer or float
//! variable holding its default (`i64` / `f64`). Lowering unifies it with
//! each use (slots, operands, bounds, impls, associated types); a use that
//! needs one type now (a method call, an `any` slot) takes the one number
//! type that works, else the default. When the body ends (`close_scope`)
//! every type in its HIR is resolved, the instances asked for over open
//! types are made for the solved ones, and literals take their types:
//! the body is lowered once.
//!
//! An unannotated `let` whose initializer leaves a type parameter open
//! (`let xs = List::new(); xs.push(1);`) is a probe: the initializer is
//! lowered with the parameters as variables, the rest of the block
//! against that open type, then everything is rolled back and the
//! initializer lowered again under the solved type. Instances over open
//! types are signatures only (`mono::var_instance`); open struct / enum
//! instances are dropped at the end (`drop_var_instances`).

mod fix;
mod literals;

pub(crate) use crate::lower::infer::fix::*;
pub use crate::lower::infer::literals::*;

#[allow(clippy::wildcard_imports)]
use crate::lower::*;
use PrimTy as P;
use nabic_diagnostic::Code;

/// An inference variable: what it is bound to, and where.
#[derive(Clone)]
pub(crate) struct Var {
    ty: Option<HirTy>,
    /// Bound from an unsuffixed literal: a later concrete use of the same
    /// number kind replaces it (`xs.push(1); let n: i64 = xs[0];`).
    weak: bool,
    /// A negative literal's: an unsigned use does not take it.
    neg: bool,
    fixed: nabic_diagnostic::Span,
    /// The first use that disagreed with the binding: where, and the type.
    conflict: Option<(nabic_diagnostic::Span, HirTy)>,
    /// An unsuffixed literal's: always holds a number type.
    lit: bool,
    /// The held type is a type parameter's declared default: where it was
    /// taken, and whose (the default of `Vec3`'s `T`). Without `lit`,
    /// a parameter's fallback, which a later use of any type replaces.
    default_from: Option<(nabic_diagnostic::Span, String)>,
}

nabic_core::codec_struct!(Var {
    ty,
    weak,
    neg,
    fixed,
    conflict,
    lit,
    default_from
});

pub fn fresh(ctx: &LowerCtx<'_>) -> HirTy {
    let mut vars = ctx.env.sink.infer_vars.borrow_mut();
    vars.push(Var {
        ty: None,
        weak: false,
        neg: false,
        fixed: ctx.here(),
        conflict: None,
        lit: false,
        default_from: None,
    });
    HirTy::Var(u32::try_from(vars.len() - 1).expect("inference variable overflow"))
}

/// A type parameter left open in a `let` initializer (`let r = zero();`)
/// holding its declared default `d` until a later use of any type
/// replaces it; the body's end fixes it to `d`.
pub fn fallback(ctx: &LowerCtx<'_>, d: HirTy, origin: String) -> HirTy {
    let here = ctx.here();
    let mut vars = ctx.env.sink.infer_vars.borrow_mut();
    vars.push(Var {
        ty: Some(d),
        weak: true,
        neg: false,
        fixed: here,
        conflict: None,
        lit: false,
        default_from: Some((here, origin)),
    });
    HirTy::Var(u32::try_from(vars.len() - 1).expect("inference variable overflow"))
}

/// `a op b` during an inference: an open side takes the other's type, and
/// both are seen through their variables. Returns the open type, for an
/// arithmetic result to carry on (`let d = c + 1; f_i64(d);`).
pub(crate) fn meet(ctx: &LowerCtx<'_>, l: &mut HirExpr, r: &mut HirExpr) -> Option<HirTy> {
    if !active(ctx) || !(has_var(ctx, &l.ty) || has_var(ctx, &r.ty)) {
        return None;
    }
    // Only numbers meet: a literal tested for membership in `s` (U+2208)
    // is left to `s.contains`.
    let number = |t: &HirTy| {
        matches!(t, HirTy::Var(_))
            || matches!(zonk(ctx, t), HirTy::Prim(p) if p.is_int() || p.is_float())
    };
    if !(number(&l.ty) && number(&r.ty)) {
        return None;
    }
    let open = if has_var(ctx, &l.ty) {
        l.ty.clone()
    } else {
        r.ty.clone()
    };
    unify_weak(ctx, &l.ty, &r.ty, is_default(ctx, l) || is_default(ctx, r));
    l.ty = zonk(ctx, &l.ty);
    r.ty = zonk(ctx, &r.ty);
    Some(open)
}

const fn var_of(t: &HirTy) -> Option<usize> {
    match t {
        HirTy::Var(v) => Some(*v as usize),
        _ => None,
    }
}

/// Is a `let` inference running (unification only then)?
pub fn active(ctx: &LowerCtx<'_>) -> bool {
    let sink = &ctx.env.sink;
    // Only once a variable exists: a `let` whose initializer opens none
    // pays nothing. A body's literals are open throughout it.
    sink.infer_depth.get() > 0
        && (sink.lit_open.get().is_some()
            || sink.infer_vars.borrow().len() > sink.infer_floor.get())
}

pub fn has_var(ctx: &LowerCtx<'_>, t: &HirTy) -> bool {
    let rec = |t: &HirTy| has_var(ctx, t);
    match t {
        HirTy::Var(_) => true,
        HirTy::Struct(id) => ctx.struct_def(*id).generic_args.iter().any(rec),
        HirTy::Enum(id) => ctx.enum_def(*id).generic_args.iter().any(rec),
        HirTy::Sum(ps) | HirTy::Tuple(ps) => ps.iter().any(rec),
        HirTy::Array(e, _) | HirTy::GcBuf(e) => rec(e),
        HirTy::Fn(a, r, _) => a.iter().any(rec) || rec(r),
        HirTy::Closure { args, ret, env, .. } => args.iter().any(rec) || rec(ret) || rec(env),
        HirTy::Throws { ok, err } => rec(ok) || rec(err),
        _ => false,
    }
}

/// `t` with every bound variable replaced by its type (struct / enum
/// instances re-instantiated).
pub fn zonk(ctx: &LowerCtx<'_>, t: &HirTy) -> HirTy {
    if !has_var(ctx, t) {
        return t.clone();
    }
    let rec = |t: &HirTy| zonk(ctx, t);
    let all = |ts: &[HirTy]| ts.iter().map(rec).collect::<Vec<_>>();
    match t {
        HirTy::Var(_) => {
            let bound = var_of(t).and_then(|v| ctx.env.sink.infer_vars.borrow()[v].ty.clone());
            bound.map_or_else(|| t.clone(), |b| rec(&b))
        }
        HirTy::Struct(id) => {
            let d = ctx.struct_def(*id);
            let base = d.name.split("__").next().unwrap_or_default();
            ctx.instantiate_struct(base, all(&d.generic_args), d.span)
                .map_or_else(|| t.clone(), HirTy::Struct)
        }
        HirTy::Enum(id) => {
            let d = ctx.enum_def(*id);
            let base = d.name.split("__").next().unwrap_or_default();
            ctx.instantiate_enum(base, all(&d.generic_args), d.span)
                .map_or_else(|| t.clone(), HirTy::Enum)
        }
        HirTy::Sum(ps) => HirTy::sum(all(ps)),
        HirTy::Tuple(ps) => HirTy::Tuple(all(ps)),
        HirTy::Array(e, n) => HirTy::Array(Box::new(rec(e)), *n),
        HirTy::GcBuf(e) => HirTy::GcBuf(Box::new(rec(e))),
        HirTy::Fn(a, r, b) => HirTy::Fn(all(a), Box::new(rec(r)), b.clone()),
        HirTy::Closure {
            args,
            ret,
            env,
            shares,
            moved,
        } => HirTy::Closure {
            args: all(args),
            ret: Box::new(rec(ret)),
            env: Box::new(rec(env)),
            shares: *shares,
            moved: *moved,
        },
        HirTy::Throws { ok, err } => HirTy::Throws {
            ok: Box::new(rec(ok)),
            err: Box::new(rec(err)),
        },
        _ => t.clone(),
    }
}

/// Does `t` hold a variable nothing bound (a literal's always holds a
/// number type)?
pub(crate) fn unsolved(ctx: &LowerCtx<'_>, t: &HirTy) -> bool {
    let rec = |t: &HirTy| unsolved(ctx, t);
    match t {
        HirTy::Var(v) => {
            let bound = ctx.env.sink.infer_vars.borrow()[root(ctx, *v as usize)]
                .ty
                .clone();
            bound.is_none_or(|b| rec(&b))
        }
        HirTy::Struct(id) => ctx.struct_def(*id).generic_args.iter().any(rec),
        HirTy::Enum(id) => ctx.enum_def(*id).generic_args.iter().any(rec),
        HirTy::Sum(ps) | HirTy::Tuple(ps) => ps.iter().any(rec),
        HirTy::Array(e, _) | HirTy::GcBuf(e) => rec(e),
        HirTy::Fn(a, r, _) => a.iter().any(rec) || rec(r),
        HirTy::Closure { args, ret, env, .. } => args.iter().any(rec) || rec(ret) || rec(env),
        HirTy::Throws { ok, err } => rec(ok) || rec(err),
        _ => false,
    }
}

/// `t` with its bound variables replaced, keeping an open literal's
/// variable (a later use may still fix it).
pub fn resolve(ctx: &LowerCtx<'_>, t: &HirTy) -> HirTy {
    if !has_var(ctx, t) {
        return t.clone();
    }
    let rec = |t: &HirTy| resolve(ctx, t);
    let all = |ts: &[HirTy]| ts.iter().map(rec).collect::<Vec<_>>();
    match t {
        HirTy::Var(v) => {
            let r = root(ctx, *v as usize);
            let (bound, weak) = {
                let vars = ctx.env.sink.infer_vars.borrow();
                (
                    vars[r].ty.clone(),
                    vars[r].weak && (vars[r].lit || vars[r].default_from.is_some()),
                )
            };
            match bound {
                Some(_) if weak => {
                    HirTy::Var(u32::try_from(r).expect("inference variable overflow"))
                }
                Some(b) => rec(&b),
                None => t.clone(),
            }
        }
        HirTy::Struct(id) => {
            let d = ctx.struct_def(*id);
            let base = d.name.split("__").next().unwrap_or_default();
            ctx.instantiate_struct(base, all(&d.generic_args), d.span)
                .map_or_else(|| t.clone(), HirTy::Struct)
        }
        HirTy::Enum(id) => {
            let d = ctx.enum_def(*id);
            let base = d.name.split("__").next().unwrap_or_default();
            ctx.instantiate_enum(base, all(&d.generic_args), d.span)
                .map_or_else(|| t.clone(), HirTy::Enum)
        }
        HirTy::Sum(ps) => HirTy::sum(all(ps)),
        HirTy::Tuple(ps) => HirTy::Tuple(all(ps)),
        HirTy::Array(e, n) => HirTy::Array(Box::new(rec(e)), *n),
        HirTy::GcBuf(e) => HirTy::GcBuf(Box::new(rec(e))),
        HirTy::Fn(a, r, b) => HirTy::Fn(all(a), Box::new(rec(r)), b.clone()),
        HirTy::Closure {
            args,
            ret,
            env,
            shares,
            moved,
        } => HirTy::Closure {
            args: all(args),
            ret: Box::new(rec(ret)),
            env: Box::new(rec(env)),
            shares: *shares,
            moved: *moved,
        },
        HirTy::Throws { ok, err } => HirTy::Throws {
            ok: Box::new(rec(ok)),
            err: Box::new(rec(err)),
        },
        _ => t.clone(),
    }
}

/// Branches of an `if` / `match` meeting during an inference: an open
/// one takes the other's type (`true`: they agree).
pub(crate) fn join(ctx: &LowerCtx<'_>, a: &HirExpr, b: &HirExpr) -> bool {
    let weak = is_default(ctx, a) || is_default(ctx, b);
    agree(ctx, &a.ty, &b.ty, weak)
}

/// Two values that must have one type (branches, `break` values), one of
/// them open: unified (`true`: they agree). A `none` and a value make no
/// sum here.
pub(crate) fn agree(ctx: &LowerCtx<'_>, a: &HirTy, b: &HirTy, weak: bool) -> bool {
    let plain = |t: &HirTy| {
        !matches!(
            zonk(ctx, t),
            HirTy::Sum(_) | HirTy::None | HirTy::Unknown | HirTy::Never
        )
    };
    active(ctx)
        && (has_var(ctx, a) || has_var(ctx, b))
        && plain(a)
        && plain(b)
        && unify_weak(ctx, a, b, weak)
}

/// A branch value typed by a literal's default (nothing expected it), as
/// opposed to one its slot or a typed operand fixed.
fn is_default(ctx: &LowerCtx<'_>, e: &HirExpr) -> bool {
    match &e.kind {
        HirExprKind::Const(_) => ctx.env.sink.default_lits.borrow().contains(&e.span),
        HirExprKind::Block(b) => b.trailing.as_ref().is_some_and(|t| is_default(ctx, t)),
        _ => false,
    }
}

/// `coerce` during an inference: a value meeting a slot where either
/// side is open unifies them (`true`: they agree as they are). A value
/// the slot converts (into a sum, an `any`) has its type fixed first.
pub(crate) fn unify_value(ctx: &LowerCtx<'_>, slot: &HirTy, value: &mut HirExpr) -> bool {
    if !active(ctx) || !(has_var(ctx, slot) || has_var(ctx, &value.ty)) {
        return false;
    }
    match slot {
        // `x = i` into an `i32 | none`: the one number member of its kind.
        HirTy::Sum(ms) => {
            if let Some(d) = literal_default(ctx, &value.ty) {
                let mut hits = ms.iter().filter(|m| same_kind(m, &d));
                match (hits.next(), hits.next()) {
                    (Some(m), None) => {
                        unify_weak(ctx, m, &value.ty, false);
                    }
                    _ => {
                        force_default(ctx, &value.ty);
                    }
                }
            } else if let HirTy::Sum(vs) = &value.ty {
                // `(0..0).min()` into an `i32 | none`: member by member.
                if vs.len() == ms.len() && unify_weak(ctx, slot, &value.ty, false) {
                    return true;
                }
            } else {
                unify_member(ctx, ms, &value.ty);
            }
        }
        // `let s: any Shape = 7`: the number type that has the bounds.
        HirTy::AnyTrait(bounds) => {
            pick_literals(ctx, &value.ty, &|t| {
                bounds.iter().all(|b| ctx.implements_trait(t, b, 0))
            });
        }
        HirTy::Any => pick_literals(ctx, &value.ty, &|_| false),
        _ => {
            // A typed slot is a use that fixes the literal. Agreeing types need
            // nothing more; a closure into a `fn` slot still meets the slot's
            // bounds (`coerce`).
            let weak = is_default(ctx, value) && has_var(ctx, slot);
            if unify_weak(ctx, slot, &value.ty, weak) {
                value.ty = resolve(ctx, &value.ty);
                return resolve(ctx, slot) == value.ty;
            }
        }
    }
    value.ty = zonk(ctx, &value.ty);
    false
}

/// An open literal `t` goes into sum `ms` (`unify_value`): as its one
/// number member of the literal's kind, else as its default.
pub(crate) fn literal_member(ctx: &LowerCtx<'_>, ms: &[HirTy], t: &HirTy) -> bool {
    literal_default(ctx, t)
        .is_some_and(|d| ms.iter().filter(|m| same_kind(m, &d)).count() == 1 || ms.contains(&d))
}

/// A generic type instantiated over open literals (`B { v: 3 }`): its
/// bounds fix them where one number type meets them.
pub(crate) fn unify_def_bounds(ctx: &LowerCtx<'_>, base: &str, args: &[HirTy]) {
    if !active(ctx) || !args.iter().any(|a| has_var(ctx, a)) {
        return;
    }
    let defs = &ctx.env.defs;
    let Some((generics, where_clause)) = defs
        .generic_struct(base)
        .map(|d| (&d.generics, &d.where_clause))
        .or_else(|| {
            defs.generic_union(base)
                .map(|u| (&u.generics, &u.where_clause))
        })
        .or_else(|| {
            defs.generic_enum(base)
                .map(|e| (&e.generics, &e.where_clause))
        })
    else {
        return;
    };
    let subst = crate::lower::mono::generic_subst(generics, args);
    let mut rcx = LowerCtx::new(ctx.env, &subst, None, ctx.here());
    rcx.current_module = module_of(base);
    let mut goals: Vec<(HirTy, &Vec<Ty>)> = Vec::new();
    for gp in &generics.params {
        if let nabic_ast::GenericParam::Type { name, bounds, .. } = gp {
            if let Some(t) = subst.get(name).filter(|t| has_var(ctx, t)) {
                goals.push((t.clone(), bounds));
            }
        }
    }
    for w in where_clause {
        let t = rcx.resolve_ast_ty(&w.ty);
        if has_var(ctx, &t) {
            goals.push((t, &w.bounds));
        }
    }
    for (t, bounds) in goals {
        for b in bounds {
            if let Some(want) = rcx.bound_ref(b, &rcx.current_module, |x| rcx.resolve_ast_ty(x)) {
                let _ = rcx.implements_trait(&t, &want, 0);
            }
        }
    }
}

/// An open value meeting a sum (`reader = spawn(..)` into a
/// `Task<i32> | none`): the one member of its shape takes it.
pub(crate) fn unify_member(ctx: &LowerCtx<'_>, ms: &[HirTy], t: &HirTy) -> bool {
    if !active(ctx) || !has_var(ctx, t) || matches!(t, HirTy::Var(_)) {
        return false;
    }
    let base = |t: &HirTy| match t {
        HirTy::Struct(id) => ctx
            .struct_def(*id)
            .name
            .split("__")
            .next()
            .map(str::to_owned),
        HirTy::Enum(id) => ctx.enum_def(*id).name.split("__").next().map(str::to_owned),
        HirTy::Tuple(ts) => Some(format!("({})", ts.len())),
        _ => None,
    };
    let Some(want) = base(t) else {
        return false;
    };
    let mut hits = ms
        .iter()
        .filter(|m| base(m).as_deref() == Some(want.as_str()));
    match (hits.next(), hits.next()) {
        (Some(m), None) => unify_weak(ctx, m, t, false),
        _ => false,
    }
}

/// `a` and `b` agree, binding open variables.
pub fn unify(ctx: &LowerCtx<'_>, a: &HirTy, b: &HirTy) -> bool {
    unify_weak(ctx, a, b, false)
}

/// A `fn` / closure type's parameters and result.
fn fn_sig(ty: &HirTy) -> Option<(&[HirTy], &HirTy)> {
    match ty {
        HirTy::Fn(params, ret, _)
        | HirTy::Closure {
            args: params, ret, ..
        } => Some((params, ret)),
        _ => None,
    }
}

fn unify_weak(ctx: &LowerCtx<'_>, lhs: &HirTy, rhs: &HirTy, weak: bool) -> bool {
    let rec = |l: &HirTy, r: &HirTy| unify_weak(ctx, l, r, weak);
    let all = |ls: &[HirTy], rs: &[HirTy]| {
        ls.len() == rs.len() && ls.iter().zip(rs).all(|(l, r)| rec(l, r))
    };
    match (lhs, rhs) {
        _ if lhs == rhs => true,
        (HirTy::Unknown | HirTy::Never | HirTy::Any, _)
        | (_, HirTy::Unknown | HirTy::Never | HirTy::Any) => true,
        (HirTy::Var(_), _) => bind(ctx, var_of(lhs).unwrap_or(0), rhs, weak),
        (_, HirTy::Var(_)) => bind(ctx, var_of(rhs).unwrap_or(0), lhs, weak),
        (HirTy::Struct(l), HirTy::Struct(r)) => {
            let (l, r) = (ctx.struct_def(*l), ctx.struct_def(*r));
            l.name.split("__").next() == r.name.split("__").next()
                && all(&l.generic_args, &r.generic_args)
        }
        (HirTy::Enum(l), HirTy::Enum(r)) => {
            let (l, r) = (ctx.enum_def(*l), ctx.enum_def(*r));
            l.name.split("__").next() == r.name.split("__").next()
                && all(&l.generic_args, &r.generic_args)
        }
        (HirTy::Tuple(ls), HirTy::Tuple(rs)) | (HirTy::Sum(ls), HirTy::Sum(rs)) => all(ls, rs),
        // A member of an open sum: the one open member takes it.
        (HirTy::Sum(ps), other) | (other, HirTy::Sum(ps)) => {
            if ps.contains(other) {
                return true;
            }
            let open: Vec<&HirTy> = ps.iter().filter(|p| has_var(ctx, p)).collect();
            matches!(open.as_slice(), [one] if rec(one, other))
        }
        (HirTy::Array(le, ln), HirTy::Array(re, rn)) => ln == rn && rec(le, re),
        (HirTy::GcBuf(le), HirTy::GcBuf(re)) => rec(le, re),
        // One const argument, with or without its declared type known.
        (HirTy::Const(l, lp), HirTy::Const(r, rp)) => l == r && (lp.is_none() || rp.is_none()),
        (HirTy::Closure { env: le, .. }, HirTy::Closure { env: re, .. }) if !rec(le, re) => false,
        (HirTy::Throws { ok: lo, err: le }, HirTy::Throws { ok: ro, err: re }) => {
            rec(lo, ro) && rec(le, re)
        }
        // A function that throws nothing fits one that may throw.
        _ => match (fn_sig(lhs), fn_sig(rhs)) {
            (Some((lp, lr)), Some((rp, rr))) => {
                all(lp, rp)
                    && match (lr, rr) {
                        (HirTy::Throws { ok, .. }, r) if !matches!(r, HirTy::Throws { .. }) => {
                            rec(ok, r)
                        }
                        (l, HirTy::Throws { ok, .. }) if !matches!(l, HirTy::Throws { .. }) => {
                            rec(l, ok)
                        }
                        _ => rec(lr, rr),
                    }
            }
            _ => false,
        },
    }
}

/// The variable `v` is linked to (`a = b` of two literals), or `v`.
fn root(ctx: &LowerCtx<'_>, mut v: usize) -> usize {
    let vars = ctx.env.sink.infer_vars.borrow();
    while let Some(HirTy::Var(w)) = vars[v].ty {
        v = w as usize;
    }
    v
}

/// Both integers / both floats.
fn same_kind(a: &HirTy, b: &HirTy) -> bool {
    let kind = |t: &HirTy| match t {
        HirTy::Prim(p) if p.is_int() => Some(true),
        HirTy::Prim(p) if p.is_float() => Some(false),
        _ => None,
    };
    kind(a).is_some() && kind(a) == kind(b)
}

/// Two variables meet: one holding a literal's default follows the
/// other, linked, so a later fix of either reaches both.
fn bind_vars(ctx: &LowerCtx<'_>, v: usize, w: usize, weak: bool) -> Option<bool> {
    if v == w {
        return Some(true);
    }
    let (bv, wv, bw, ww) = {
        let vars = ctx.env.sink.infer_vars.borrow();
        (
            vars[v].ty.clone()?,
            vars[v].weak,
            vars[w].ty.clone()?,
            vars[w].weak,
        )
    };
    if !same_kind(&bv, &bw) {
        return Some(unify_weak(ctx, &bv, &bw, weak));
    }
    // A declared default one side took goes with the link.
    let link = |from: usize, to: usize| {
        let mut vars = ctx.env.sink.infer_vars.borrow_mut();
        if let (Some(d), None, true) = (
            vars[from].default_from.clone(),
            &vars[to].default_from,
            vars[to].weak,
        ) {
            let held = vars[from].ty.clone();
            vars[to].ty = held;
            vars[to].default_from = Some(d);
        }
        vars[from].ty = Some(HirTy::Var(to as u32));
        true
    };
    Some(match (wv, ww) {
        (true, _) => link(v, w),
        (false, true) => link(w, v),
        (false, false) => bv == bw,
    })
}

fn bind(ctx: &LowerCtx<'_>, v: usize, t: &HirTy, weak: bool) -> bool {
    let v = root(ctx, v);
    if let HirTy::Var(w) = t {
        let w = root(ctx, *w as usize);
        if let Some(ok) = bind_vars(ctx, v, w, weak) {
            return ok;
        }
        if w == v {
            return true;
        }
        // An open `w` takes a bound `v`; an open `v` links to `w` below.
        let (v_open, w_open) = {
            let vars = ctx.env.sink.infer_vars.borrow();
            (vars[v].ty.is_none(), vars[w].ty.is_none())
        };
        if w_open && !v_open {
            return bind(ctx, w, &HirTy::Var(v as u32), weak);
        }
    }
    let here = ctx.here();
    let bound = ctx.env.sink.infer_vars.borrow()[v].ty.clone();
    let ok = match bound {
        None => {
            // Occurs check: `xs.push(xs)`.
            if zonk_has(ctx, t, v) {
                false
            } else {
                let mut vars = ctx.env.sink.infer_vars.borrow_mut();
                vars[v].ty = Some(t.clone());
                vars[v].weak = weak;
                vars[v].fixed = here;
                true
            }
        }
        Some(b) => {
            let was_weak = ctx.env.sink.infer_vars.borrow()[v].weak;
            let same_kind = same_kind(&b, t);
            let unsigned = matches!(t, HirTy::Prim(p) if p.is_int() && !p.is_signed_int());
            if was_weak && ctx.env.sink.infer_vars.borrow()[v].neg && unsigned {
                // `let i = -1; xs[i]`: the slot decides (an index from the end).
                true
            } else if was_weak && !weak && (same_kind || is_fallback(ctx, v)) {
                let mut vars = ctx.env.sink.infer_vars.borrow_mut();
                vars[v].default_from = None;
                vars[v].ty = Some(t.clone());
                vars[v].weak = false;
                vars[v].fixed = here;
                true
            } else {
                unify_weak(ctx, &b, t, weak) || (weak && same_kind)
            }
        }
    };
    if !ok {
        let mut vars = ctx.env.sink.infer_vars.borrow_mut();
        if vars[v].conflict.is_none() {
            vars[v].conflict = Some((here, t.clone()));
        }
    }
    ok
}

/// Is `v` a type parameter's fallback (not a literal's)?
fn is_fallback(ctx: &LowerCtx<'_>, v: usize) -> bool {
    let vars = ctx.env.sink.infer_vars.borrow();
    !vars[v].lit && vars[v].default_from.is_some()
}

/// Does `t` (resolved) mention variable `v`?
fn zonk_has(ctx: &LowerCtx<'_>, t: &HirTy, v: usize) -> bool {
    fn walk(ctx: &LowerCtx<'_>, t: &HirTy, v: usize) -> bool {
        let rec = |t: &HirTy| walk(ctx, t, v);
        match t {
            HirTy::Var(_) => match var_of(t) {
                Some(w) if w == v => true,
                Some(w) => ctx.env.sink.infer_vars.borrow()[w]
                    .ty
                    .clone()
                    .is_some_and(|b| rec(&b)),
                None => false,
            },
            HirTy::Struct(id) => ctx.struct_def(*id).generic_args.iter().any(rec),
            HirTy::Enum(id) => ctx.enum_def(*id).generic_args.iter().any(rec),
            HirTy::Sum(ps) | HirTy::Tuple(ps) => ps.iter().any(rec),
            HirTy::Array(e, _) | HirTy::GcBuf(e) => rec(e),
            HirTy::Fn(a, r, _) => a.iter().any(rec) || rec(r),
            HirTy::Closure { args, ret, env, .. } => args.iter().any(rec) || rec(ret) || rec(env),
            HirTy::Throws { ok, err } => rec(ok) || rec(err),
            _ => false,
        }
    }
    has_var(ctx, t) && walk(ctx, t, v)
}

/// Lower what follows a `let` in a throwaway context, unifying as it goes.
fn probe_tail(ctx: &LowerCtx<'_>, rest: &[Stmt], block: &Block) {
    let mut probe = ctx.clone();
    probe.infer_vars = true;
    let tail = Block {
        stmts: rest.to_vec(),
        trailing: block.trailing.clone(),
        span: block.span,
        label: None,
    };
    let hint = ctx.current_expected().cloned().unwrap_or(HirTy::Unknown);
    probe.expected_ty_stack.push(hint.clone());
    let b = expr::lower_block(&mut probe, &tail);
    // The tail meets the block's slot (a body's result).
    if let Some(t) = b.trailing.as_ref().filter(|_| hint != HirTy::Unknown) {
        let mut t = t.clone();
        unify_value(&probe, &hint, &mut t);
    }
}

/// `let (a, b) = init;` without annotation, as `lower_let`:
/// `let (tx, rx) = channel(); tx.send(1);`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn lower_let_tuple(
    ctx: &mut LowerCtx<'_>,
    names: &[String],
    tuple_rest: Option<&(usize, Option<String>)>,
    init: &Expr,
    span: Span,
    rest: &[Stmt],
    block: &Block,
    out: &mut Vec<HirStmt>,
) {
    let sink = &ctx.env.sink;
    let mark = ctx.probe_mark();
    let first = sink.infer_vars.borrow().len();
    let outer = std::mem::replace(&mut ctx.infer_vars, true);
    if sink.infer_depth.get() == 0 {
        sink.infer_floor.set(first);
    }
    sink.infer_depth.set(sink.infer_depth.get() + 1);
    let mut stmts = Vec::new();
    expr::lower_let_tuple(ctx, names, tuple_rest, None, init, span, &mut stmts);
    ctx.infer_vars = outer;
    let own = first..sink.infer_vars.borrow().len();
    let untyped = !outer
        && ctx.probe_clean(&mark)
        && matches!(stmts.first(), Some(HirStmt::Let { init, .. }) if collect::hir_expr_has_unknown(init));
    if !untyped
        && (own.is_empty() || outer || !own.clone().any(|v| unsolved(ctx, &HirTy::Var(v as u32))))
    {
        sink.infer_depth.set(sink.infer_depth.get() - 1);
        out.extend(stmts);
        return;
    }
    let open = match stmts.first() {
        Some(HirStmt::Let { init, .. }) => init.ty.clone(),
        _ => HirTy::Unknown,
    };
    let solved = has_var(ctx, &open)
        .then(|| {
            probe_tail(ctx, rest, block);
            resolve(ctx, &open)
        })
        .or_else(|| (untyped && open != HirTy::Unknown).then(|| zonk(ctx, &open)));
    sink.infer_depth.set(sink.infer_depth.get() - 1);
    let conflict = own.into_iter().find_map(|v| {
        let r = root(ctx, v);
        let vars = sink.infer_vars.borrow();
        let (at, used) = vars[v]
            .conflict
            .clone()
            .or_else(|| vars[r].conflict.clone())?;
        let have = vars[r].ty.clone()?;
        Some((at, used, vars[v].fixed, have))
    });
    let conflict = conflict.map(|(at, used, fixed, have)| {
        let (used, have) = (
            ctx.ty_name(&zonk(ctx, &used)),
            ctx.ty_name(&zonk(ctx, &have)),
        );
        nabic_diagnostic::NabiDiagnostic::error(
            Code::MISMATCHED_TYPES,
            at,
            format!(
                "conflicting uses of the destructured value: `{used}` here, but `{have}` fixed \
                 at {}:{}",
                fixed.line, fixed.col
            ),
        )
        .with_secondary(fixed, "its type fixed here".to_owned())
    });
    ctx.rollback(&mark);
    let outer_span = ctx.at.replace(span);
    match (solved, conflict) {
        (Some(_), Some(d)) => {
            ctx.env.sink.diagnostics.borrow_mut().push(d);
            let _ = lower_expected(ctx, init, HirTy::Unknown);
            for n in names {
                ctx.declare(Some(n.clone()), HirTy::Unknown);
            }
        }
        (Some(t), None) if !unsolved(ctx, &t) => {
            expr::lower_let_tuple(ctx, names, tuple_rest, Some(t), init, span, out);
        }
        _ => expr::lower_let_tuple(ctx, names, tuple_rest, None, init, span, out),
    }
    ctx.at.set(outer_span);
}

/// `let name = init;` without annotation: lower `init` with open
/// parameters; if any remain, probe the rest of the block, roll back, and
/// lower `init` again under the solved type.
pub(crate) fn lower_let(
    ctx: &mut LowerCtx<'_>,
    s: &Stmt,
    name: &str,
    init: &Expr,
    rest: &[Stmt],
    block: &Block,
) -> Option<HirStmt> {
    let sink = &ctx.env.sink;
    let mark = ctx.probe_mark();
    let first = sink.infer_vars.borrow().len();
    let outer = std::mem::replace(&mut ctx.infer_vars, true);
    if sink.infer_depth.get() == 0 {
        sink.infer_floor.set(first);
    }
    sink.infer_depth.set(sink.infer_depth.get() + 1);
    let stmt = expr::lower_stmt(ctx, s);
    ctx.infer_vars = outer;
    let own = first..sink.infer_vars.borrow().len();
    // Nothing open, inside an enclosing inference (which solves it), or
    // only literals, which the body's end solves. A part left untyped
    // (`[]` beside a typed branch) is lowered again under the solved type.
    let untyped = !outer
        && ctx.probe_clean(&mark)
        && matches!(&stmt, Some(HirStmt::Let { init, .. }) if collect::hir_expr_has_unknown(init));
    if !untyped
        && (own.is_empty() || outer || !own.clone().any(|v| unsolved(ctx, &HirTy::Var(v as u32))))
    {
        sink.infer_depth.set(sink.infer_depth.get() - 1);
        return stmt;
    }
    let open = ctx.lookup(name).map_or(HirTy::Unknown, |l| ctx.var_ty(l));
    let solved = has_var(ctx, &open).then(|| {
        probe_tail(ctx, rest, block);
        // A closure's result is only what its body returns.
        match resolve(ctx, &open) {
            HirTy::Closure {
                args,
                ret,
                env,
                shares,
                moved,
            } if unsolved(ctx, &ret) => HirTy::Closure {
                args,
                ret: Box::new(HirTy::Unknown),
                env,
                shares,
                moved,
            },
            HirTy::Fn(args, ret, b) if unsolved(ctx, &ret) => {
                HirTy::Fn(args, Box::new(HirTy::Unknown), b)
            }
            t => t,
        }
    });
    let solved = solved.or_else(|| (untyped && open != HirTy::Unknown).then(|| zonk(ctx, &open)));
    sink.infer_depth.set(sink.infer_depth.get() - 1);
    let conflict = own.into_iter().find_map(|v| {
        let r = root(ctx, v);
        let vars = sink.infer_vars.borrow();
        let (at, used) = vars[v]
            .conflict
            .clone()
            .or_else(|| vars[r].conflict.clone())?;
        let have = vars[r].ty.clone()?;
        Some((at, used, vars[v].fixed, have, vars[r].weak))
    });
    let conflict = conflict.map(|(at, used, fixed, have, _)| {
        let (used, have) = (
            ctx.ty_name(&zonk(ctx, &used)),
            ctx.ty_name(&zonk(ctx, &have)),
        );
        nabic_diagnostic::NabiDiagnostic::error(
            Code::MISMATCHED_TYPES,
            at,
            format!(
                "conflicting uses of `{name}` (`{}`): `{used}` here, but `{have}` fixed at {}:{}",
                ctx.ty_name(&open),
                fixed.line,
                fixed.col
            ),
        )
        .with_secondary(fixed, format!("`{name}`'s type fixed here"))
    });
    // Undo the open attempt (its binding included).
    ctx.rollback(&mark);
    let outer_span = ctx.at.replace(s.span);
    let out = match solved {
        // The open variables are not in the binding's type
        // (`List::new().len()`): lowered again, they are errors.
        None => expr::lower_stmt(ctx, s),
        Some(t) => {
            if let Some(d) = conflict {
                ctx.env.sink.diagnostics.borrow_mut().push(d);
                // Its uses are not checked against either type again.
                lower_expected(ctx, init, t);
                ctx.declare(Some(name.to_owned()), HirTy::Unknown);
                None
            } else if unsolved(ctx, &t) {
                let shown = ctx.ty_name(&t).replace("{unknown}", "_");
                let slot = match open_slots(&shown) {
                    s if s == ".." => "T".to_owned(),
                    s => s,
                };
                ctx.emit_uninferred(format!(
                    "cannot infer the type of `{name}` (`{shown}`) from its initializer or \
                     later uses - add an annotation naming its arguments (`let {name}: {slot} \
                     = ..`)"
                ));
                ctx.declare(Some(name.to_owned()), HirTy::Unknown);
                None
            } else {
                let v = lower_expected(ctx, init, t.clone());
                // A closure takes the parameter types; its own type is
                // what its body makes of them.
                let (t, v) = if matches!(t, HirTy::Closure { .. } | HirTy::Fn(..)) {
                    (v.ty.clone(), v)
                } else {
                    let v = coerce(ctx, &t, v, &|| format!("`let {name}`"));
                    (t, v)
                };
                Some(ctx.declare_var(name, t, v, s.span))
            }
        }
    };
    ctx.at.set(outer_span);
    out
}

/// `W<_, i64>` as `W<.., i64>`: the known part of a type, its open
/// arguments left to fill in.
fn open_slots(shown: &str) -> String {
    let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
    let cs: Vec<char> = shown.chars().collect();
    let mut out = String::with_capacity(shown.len());
    for (i, &c) in cs.iter().enumerate() {
        let alone = !word(i.checked_sub(1).map(|j| cs[j])) && !word(cs.get(i + 1).copied());
        if c == '_' && alone {
            out.push_str("..");
        } else {
            out.push(c);
        }
    }
    out
}

/// A literal-bound `let` used as another number type than the one it
/// has: the fix goes on the declaration (`let c: i32 = 0`, `let c =
/// 0.0`), not a cast at the use. `None` for other mismatches.
pub(crate) fn let_literal_conflict(
    ctx: &LowerCtx<'_>,
    slot: &HirTy,
    value: &HirExpr,
) -> Option<nabic_diagnostic::NabiDiagnostic> {
    let HirExprKind::Local(l) = value.kind else {
        return None;
    };
    let (src, decl) = ctx.lit_lets.get(&l)?.clone();
    let name = ctx.locals.get(l.0 as usize)?.name.clone()?;
    let HirTy::Var(v) = ctx.var_ty(l) else {
        return None;
    };
    let r = root(ctx, v as usize);
    let (have, weak, fixed) = {
        let vars = ctx.env.sink.infer_vars.borrow();
        (vars[r].ty.clone()?, vars[r].weak, vars[r].fixed)
    };
    let number = |t: &HirTy| matches!(t, HirTy::Prim(p) if p.is_int() || p.is_float());
    let float = |t: &HirTy| matches!(t, HirTy::Prim(p) if p.is_float());
    let used_ty = zonk(ctx, slot);
    if !(number(&used_ty) && number(&have)) || used_ty == have {
        return None;
    }
    let (used, have_s) = (ctx.ty_name(&used_ty), ctx.ty_name(&have));
    let at = value.span;
    if !weak {
        return Some(
            nabic_diagnostic::NabiDiagnostic::error(
                Code::MISMATCHED_TYPES,
                at,
                format!(
                    "conflicting uses of `{name}`: `{used}` here, but `{have_s}` fixed at {}:{}",
                    fixed.line, fixed.col
                ),
            )
            .with_secondary(fixed, format!("`{name}`'s type fixed here"))
            .with_help(format!(
                "Nabi never converts numbers implicitly; say the type at the declaration \
                 (`let {name}: {have_s} = ..`) and convert the other use with `as`"
            )),
        );
    }
    if !float(&used_ty) || float(&have) {
        return None;
    }
    let int = !float(&have);
    let kind = if int { "an integer" } else { "a float" };
    let decimal = src
        .chars()
        .all(|c| c.is_ascii_digit() || c == '_' || c == '-');
    let help = if int && decimal {
        format!(
            "Nabi never converts numbers implicitly; make it a float at the declaration: \
             `let {name} = {src}.0;` (or `let {name}: {used} = {src};`)"
        )
    } else {
        format!(
            "Nabi never converts numbers implicitly; make it a float at the declaration: \
             `let {name}: {used} = {src};`"
        )
    };
    Some(
        nabic_diagnostic::NabiDiagnostic::error(
            Code::MISMATCHED_TYPES,
            at,
            format!(
                "`{name}` is used as `{used}` here, but it holds {kind} (`{have_s}`) from `{src}` \
                 at {}:{}",
                decl.line, decl.col
            ),
        )
        .with_secondary(decl, format!("`{name}` declared here"))
        .with_help(help),
    )
}

/// Struct / enum instances over inference variables (made by probes)
/// are never used: empty them so nothing lays them out.
pub(crate) fn drop_var_instances(structs: &mut [HirStructDef], enums: &mut [HirEnumDef]) {
    let open = |args: &[HirTy], s: &[bool], e: &[bool]| {
        fn walk(ty: &HirTy, open_s: &[bool], open_e: &[bool]) -> bool {
            let rec = |t: &HirTy| walk(t, open_s, open_e);
            match ty {
                HirTy::Var(_) => true,
                HirTy::Struct(id) => open_s.get(id.0 as usize).copied().unwrap_or(false),
                HirTy::Enum(id) => open_e.get(id.0 as usize).copied().unwrap_or(false),
                HirTy::Sum(ps) | HirTy::Tuple(ps) => ps.iter().any(rec),
                HirTy::Array(elem, _) | HirTy::GcBuf(elem) => rec(elem),
                HirTy::Fn(params, ret, _) => params.iter().any(rec) || rec(ret),
                HirTy::Closure { args, ret, env, .. } => {
                    args.iter().any(rec) || rec(ret) || rec(env)
                }
                HirTy::Throws { ok, err } => rec(ok) || rec(err),
                _ => false,
            }
        }
        args.iter().any(|a| walk(a, s, e))
    };
    let mut s = vec![false; structs.len()];
    let mut e = vec![false; enums.len()];
    loop {
        let ns: Vec<bool> = structs
            .iter()
            .map(|d| open(&d.generic_args, &s, &e))
            .collect();
        let ne: Vec<bool> = enums
            .iter()
            .map(|d| open(&d.generic_args, &s, &e))
            .collect();
        if ns == s && ne == e {
            break;
        }
        (s, e) = (ns, ne);
    }
    for (d, o) in structs.iter_mut().zip(&s) {
        if *o {
            d.fields.clear();
        }
    }
    for (d, o) in enums.iter_mut().zip(&e) {
        if *o {
            d.variants.clear();
        }
    }
}
