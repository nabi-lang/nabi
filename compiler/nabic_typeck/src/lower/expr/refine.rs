//! Type tests on type parameters: `T is X`, decided per instance.
//!
//! At the definition check `T` is opaque, so the test is unknown and an
//! `if T is X` branch is checked once with `T` replaced by `X`. In an
//! instance the test is a constant and a false branch is never lowered
//! (it may be ill-typed there).

#[allow(clippy::wildcard_imports)]
use crate::lower::expr::*;

/// One `T is X` of a condition.
pub(crate) struct TypeTest {
    pub(crate) name: String,
    /// What `T` is here: a `Param` at the check, a type in an instance.
    param: HirTy,
    x: HirTy,
}

/// What a condition made of type tests says about its `then` branch.
pub(crate) enum TestShape {
    /// A `&&` chain: every test holds in the branch; `pure` when nothing
    /// else is in the chain.
    And(Vec<TypeTest>, bool),
    /// `T is A || T is B`: no single type for the branch.
    Or(String),
    /// `!(T is A)`: no type for the branch.
    Not(String, String),
}

/// The type parameter `e` names (`T`, `Self`), unless a value has that
/// name.
fn param_of(ctx: &LowerCtx<'_>, e: &Expr) -> Option<(String, HirTy)> {
    if matches!(e.kind, ExprKind::SelfUpper) {
        return Some(("Self".to_owned(), ctx.self_ty.clone()?));
    }
    let ExprKind::Path(p) = &e.kind else {
        return None;
    };
    let [seg] = p.segments.as_slice() else {
        return None;
    };
    if !seg.generics.is_empty() || ctx.lookup(&seg.name).is_some() {
        return None;
    }
    let t = if seg.name == "Self" {
        ctx.self_ty.clone()?
    } else {
        ctx.ty_subst.get(&seg.name).cloned()?
    };
    (!matches!(t, HirTy::Const(..))).then(|| (seg.name.clone(), t))
}

/// A name in `t` that is neither a type nor a parameter in scope
/// (`List<U>` with no `U`).
fn unbound_name(ctx: &LowerCtx<'_>, t: &nabic_ast::Ty) -> Option<String> {
    let rec = |t: &nabic_ast::Ty| unbound_name(ctx, t);
    match &t.kind {
        AstTyKind::Path(p) => {
            if let [seg] = p.segments.as_slice() {
                let n = seg.name.as_str();
                let known = n == "Self"
                    || ctx.ty_subst.contains_key(n)
                    || !matches!(
                        crate::ty::resolve_ty(&nabic_ast::Ty {
                            kind: AstTyKind::Path(p.clone()),
                            span: t.span,
                        }),
                        HirTy::Unknown | HirTy::Path(_)
                    )
                    || ctx.canon_type(&[n]).is_some();
                if !known && seg.generics.is_empty() {
                    return Some(seg.name.clone());
                }
            }
            p.segments.iter().flat_map(|s| &s.generics).find_map(rec)
        }
        AstTyKind::Sum(ts) | AstTyKind::Tuple(ts) => ts.iter().find_map(rec),
        AstTyKind::Array(e, _) | AstTyKind::Nest(e) => rec(e),
        _ => None,
    }
}

/// `text` with the type name `name` replaced by `with`.
fn swap_name(text: &str, name: &str, with: &str) -> String {
    let mut out = String::new();
    let mut word = String::new();
    for c in text.chars().chain(std::iter::once(' ')) {
        if c.is_alphanumeric() || c == '_' {
            word.push(c);
            continue;
        }
        out.push_str(if word == name { with } else { &word });
        word.clear();
        out.push(c);
    }
    out.pop();
    out
}

/// Every value of `t` passes `v is x`: `t` is `x`, a member of sum `x`, or
/// a sub-sum of it. `None` while `t` is opaque.
fn decide(ctx: &LowerCtx<'_>, t: &HirTy, x: &HirTy) -> Option<bool> {
    if t == x {
        return Some(true);
    }
    if crate::lower::bounds::has_param(ctx, t) || crate::lower::bounds::has_param(ctx, x) {
        return None;
    }
    Some(match (t, x) {
        (HirTy::Sum(ts), HirTy::Sum(xs)) => ts.iter().all(|m| xs.contains(m)),
        (_, HirTy::Sum(xs)) => xs.contains(t),
        _ => false,
    })
}

/// `T is X` of `operand is pat`, if `operand` names a type parameter;
/// errors are reported here.
fn test_of(
    ctx: &LowerCtx<'_>,
    operand: &Expr,
    pat: &nabic_ast::Pattern,
    report: bool,
) -> Option<Result<TypeTest, ()>> {
    let (name, param) = param_of(ctx, operand)?;
    let AstPatternKind::TypeCheck { binder, ty } = &pat.kind else {
        return None;
    };
    if let Some(b) = binder {
        if report {
            ctx.emit_error_at(
                Code::MISMATCHED_TYPES,
                pat.span,
                format!("`{name} is {b}: ..` binds no value: `{name}` is a type"),
            );
            ctx.help_last(Some(format!(
                "test the type alone: `if {name} is {}`",
                ctx.src(ty.span).unwrap_or("X")
            )));
        }
        return Some(Err(()));
    }
    if let Some(u) = unbound_name(ctx, ty) {
        if report {
            ctx.emit_error_at(
                Code::UNRESOLVED_TYPE,
                ty.span,
                format!("`{u}` is not a parameter here: the type after `is` can't introduce one"),
            );
            let base = swap_name(&nabic_ast::print::print_ty(ty), &u, "f32");
            ctx.help_last(Some(format!(
                "add `{u}` to the function's parameters, or test a concrete type: \
                 `{name} is {base}`"
            )));
        }
        return Some(Err(()));
    }
    let x = ctx.resolve_ast_ty(ty);
    if x == HirTy::Unknown {
        return Some(Err(()));
    }
    Some(Ok(TypeTest { name, param, x }))
}

/// `T is X` as a value: a constant in an instance, unknown at the check.
pub(crate) fn lower_test(
    ctx: &LowerCtx<'_>,
    operand: &Expr,
    pat: &nabic_ast::Pattern,
    span: Span,
) -> Option<HirExpr> {
    let Ok(t) = test_of(ctx, operand, pat, true)? else {
        return Some(crate::lower::poison(span));
    };
    let bool_ty = HirTy::Prim(PrimTy::Bool);
    let lit = |b: bool| HirExpr {
        kind: HirExprKind::Const(ConstVal::Bool(b)),
        ty: bool_ty.clone(),
        span,
    };
    Some(match decide(ctx, &t.param, &t.x) {
        Some(b) => lit(b),
        // Not a constant to `const_cond`: the check sees both branches.
        None => HirExpr {
            kind: HirExprKind::Block(Box::new(HirBlock {
                stmts: vec![HirStmt::Expr(lit(false))],
                trailing: Some(lit(false)),
                label: None,
            })),
            ty: bool_ty,
            span,
        },
    })
}

/// The type tests `cond` is made of, if any (read after `cond` was
/// lowered, so nothing is reported twice).
pub(crate) fn test_shape(ctx: &LowerCtx<'_>, cond: &Expr) -> Option<TestShape> {
    fn chain(ctx: &LowerCtx<'_>, e: &Expr, out: &mut Vec<TypeTest>) -> bool {
        match &e.kind {
            ExprKind::OpInfix { op, lhs, rhs } if op == "&&" => {
                let l = chain(ctx, lhs, out);
                chain(ctx, rhs, out) && l
            }
            ExprKind::Is(operand, pat) => match test_of(ctx, operand, pat, false) {
                Some(Ok(t)) => {
                    out.push(t);
                    true
                }
                _ => false,
            },
            _ => false,
        }
    }
    let first_test = |e: &Expr| match &e.kind {
        ExprKind::Is(operand, pat) => match test_of(ctx, operand, pat, false) {
            Some(Ok(t)) => Some(t),
            _ => None,
        },
        _ => None,
    };
    match &cond.kind {
        ExprKind::OpInfix { op, lhs, rhs } if op == "||" => {
            let t = first_test(lhs).or_else(|| first_test(rhs))?;
            Some(TestShape::Or(t.name))
        }
        ExprKind::OpPrefix { op, operand } if op == "!" => {
            let t = first_test(operand)?;
            let x = ctx.ty_name(&t.x);
            Some(TestShape::Not(t.name, x))
        }
        _ => {
            let mut tests = Vec::new();
            let pure = chain(ctx, cond, &mut tests);
            (!tests.is_empty()).then_some(TestShape::And(tests, pure))
        }
    }
}

/// Does a test of the chain fail in this instance (its branch is dead)?
pub(crate) fn tests_fail(ctx: &LowerCtx<'_>, tests: &[TypeTest]) -> bool {
    tests
        .iter()
        .any(|t| decide(ctx, &t.param, &t.x) == Some(false))
}

/// Do all tests of a pure chain hold in this instance?
pub(crate) fn tests_hold(ctx: &LowerCtx<'_>, tests: &[TypeTest]) -> bool {
    tests
        .iter()
        .all(|t| decide(ctx, &t.param, &t.x) == Some(true))
}

/// `t` with `Param(p)` replaced by `x` for each `(p, x)`.
pub(crate) fn swap_params(ctx: &LowerCtx<'_>, t: &HirTy, by: &[(u32, HirTy)]) -> HirTy {
    let hit = |t: &HirTy| matches!(t, HirTy::Param(p) if by.iter().any(|(q, _)| q == p));
    if by.is_empty() || !crate::lower::bounds::mentions(ctx, t, &hit) {
        return t.clone();
    }
    let rec = |t: &HirTy| swap_params(ctx, t, by);
    let all = |ts: &[HirTy]| ts.iter().map(rec).collect::<Vec<_>>();
    match t {
        HirTy::Param(p) => by
            .iter()
            .find(|(q, _)| q == p)
            .map_or_else(|| t.clone(), |(_, x)| x.clone()),
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

/// What `refine` changed, for `unrefine`.
pub(crate) struct Refined {
    pub(crate) by: Vec<(u32, HirTy)>,
    subst: TySubst,
    self_ty: Option<HirTy>,
    ret: HirTy,
    locals: Vec<(usize, HirTy)>,
}

/// At the check, `T` is `X` in the branch: the parameter, `Self`, the
/// return type and every local in scope over `T`. A sum `X` refines
/// nothing (only a member may be used as `X`).
pub(crate) fn refine(ctx: &mut LowerCtx<'_>, tests: &[TypeTest]) -> Option<Refined> {
    let by: Vec<(u32, HirTy)> = tests
        .iter()
        .filter(|t| !matches!(t.x, HirTy::Sum(_) | HirTy::Any | HirTy::AnyTrait(_)))
        .filter_map(|t| match t.param {
            HirTy::Param(p) if decide(ctx, &t.param, &t.x).is_none() => Some((p, t.x.clone())),
            _ => None,
        })
        .collect();
    if by.is_empty() {
        return None;
    }
    let subst = ctx.ty_subst.as_ref().clone();
    for t in tests {
        if by.iter().any(|(p, _)| t.param == HirTy::Param(*p)) && t.name != "Self" {
            ctx.ty_subst.to_mut().insert(t.name.clone(), t.x.clone());
        }
    }
    let keys: Vec<String> = ctx.ty_subst.keys().cloned().collect();
    for k in keys {
        let v = swap_params(ctx, &ctx.ty_subst[&k], &by);
        ctx.ty_subst.to_mut().insert(k, v);
    }
    let self_ty = ctx.self_ty.clone();
    ctx.self_ty = self_ty.as_ref().map(|t| swap_params(ctx, t, &by));
    let ret = ctx.fn_ret_ty.clone();
    ctx.fn_ret_ty = swap_params(ctx, &ret, &by);
    let mut locals = Vec::new();
    for i in 0..ctx.locals.len() {
        let t = ctx.locals[i].ty.clone();
        let s = swap_params(ctx, &t, &by);
        if s != t {
            ctx.locals[i].ty = s;
            locals.push((i, t));
        }
    }
    Some(Refined {
        by,
        subst,
        self_ty,
        ret,
        locals,
    })
}

pub(crate) fn unrefine(ctx: &mut LowerCtx<'_>, s: Refined) {
    ctx.ty_subst = std::borrow::Cow::Owned(s.subst);
    ctx.self_ty = s.self_ty;
    ctx.fn_ret_ty = s.ret;
    for (i, t) in s.locals {
        ctx.locals[i].ty = t;
    }
}

/// Errors from `since` on get `help` (the ones about `name`).
pub(crate) fn help_since(ctx: &LowerCtx<'_>, since: usize, name: &str, help: &str) {
    let mut ds = ctx.env.sink.diagnostics.borrow_mut();
    let want = format!("`{name}`");
    for d in ds.iter_mut().skip(since) {
        if d.is_error()
            && d.code == Code::MISMATCHED_TYPES
            && (d.message.contains(&want) || name == "Self")
            && !d.notes.iter().any(|n| n.starts_with("help:"))
        {
            d.notes.push(format!("help: {help}"));
        }
    }
}

/// `T == f32`: a type compared like a value.
pub(crate) fn eq_on_type(
    ctx: &LowerCtx<'_>,
    op: &str,
    lhs: &Expr,
    rhs: &Expr,
    span: Span,
) -> Option<HirExpr> {
    if op != "==" && op != "!=" {
        return None;
    }
    let (name, other) = match (param_of(ctx, lhs), param_of(ctx, rhs)) {
        (Some((n, _)), _) => (n, rhs),
        (None, Some((n, _))) => (n, lhs),
        _ => return None,
    };
    let x = ctx.src(other.span).unwrap_or("f32").to_owned();
    let not = if op == "!=" { "!" } else { "" };
    ctx.emit_error_at(
        Code::MISMATCHED_TYPES,
        span,
        "types are compared with `is`, not `==`".to_owned(),
    );
    let test = if not.is_empty() {
        format!("{name} is {x}")
    } else {
        format!("{not}({name} is {x})")
    };
    ctx.help_last(Some(format!("write `if {test}`")));
    Some(crate::lower::poison(span))
}

/// `if` over type tests that fail in this instance: the branch is never
/// lowered (it may be ill-typed for this type); `Err` gives `cond` back.
pub(crate) fn skip_then(
    ctx: &mut LowerCtx<'_>,
    tests: Option<&TestShape>,
    cond: HirExpr,
    else_branch: Option<&Expr>,
    span: Span,
) -> Result<HirExpr, HirExpr> {
    match tests {
        Some(TestShape::And(ts, _)) if tests_fail(ctx, ts) => {
            let else_h = else_branch.map(|e| lower_expr(ctx, e));
            let ty = else_h.as_ref().map_or(HirTy::Unit, |e| e.ty.clone());
            Ok(HirExpr {
                kind: HirExprKind::Block(Box::new(HirBlock {
                    stmts: vec![HirStmt::Expr(cond)],
                    trailing: else_h,
                    label: None,
                })),
                ty,
                span,
            })
        }
        _ => Err(cond),
    }
}

/// The `then` branch of an `if`, under what its type tests say: at the
/// definition check `T` is `X` in it. Also the refinement made, for the
/// `else` (`else_under_tests`).
pub(crate) fn then_under_tests(
    ctx: &mut LowerCtx<'_>,
    cond: &Expr,
    block: &Block,
    tests: Option<&TestShape>,
    expected: Option<&HirTy>,
) -> (HirBlock, Vec<(u32, HirTy)>) {
    let since = ctx.env.sink.diagnostics.borrow().len();
    let refined = match tests {
        Some(TestShape::And(ts, _)) => refine(ctx, ts),
        _ => None,
    };
    let by = refined.as_ref().map(|r| r.by.clone()).unwrap_or_default();
    let swapped = expected
        .filter(|_| refined.is_some())
        .map(|t| swap_params(ctx, t, &by));
    if let Some(t) = swapped.clone() {
        ctx.push_expected(t);
    }
    let mut then_h = lower_narrowed_block(ctx, cond, block);
    if let Some(r) = refined {
        if swapped.is_some() {
            ctx.pop_expected();
        }
        unrefine(ctx, r);
        // The branch's `X` value is the slot's `T`.
        if let (Some(e), Some(t)) = (expected, then_h.trailing.as_mut()) {
            if swapped.as_ref() == Some(&t.ty) {
                t.ty = e.clone();
            }
        }
    }
    match tests {
        Some(TestShape::Or(name)) if ctx.env.sink.diagnostics.borrow().len() > since => {
            ctx.env.sink.diagnostics.borrow_mut().truncate(since);
            ctx.emit_error_at(
                Code::MISMATCHED_TYPES,
                cond.span,
                format!("`||` of type tests gives the branch no single type for `{name}`"),
            );
            ctx.help_last(Some(format!(
                "write one branch per type: `if {name} is f32 {{ .. }} else if {name} is f64 \
                 {{ .. }}`"
            )));
        }
        Some(TestShape::Not(name, x)) => help_since(
            ctx,
            since,
            name,
            &format!(
                "a negated test gives no type to `{name}`; put the `{x}` code in \
                 `if {name} is {x} {{ .. }}`"
            ),
        ),
        _ => {}
    }
    (then_h, by)
}

/// A pure chain of type tests that holds in this instance, with an
/// `else`: only the `then` branch is lowered.
pub(crate) fn tests_take_then(
    ctx: &LowerCtx<'_>,
    tests: Option<&TestShape>,
    has_else: bool,
) -> bool {
    matches!(tests, Some(TestShape::And(ts, true)) if has_else && tests_hold(ctx, ts))
}

/// `{ cond; then }` for `tests_take_then`.
pub(crate) fn then_only(cond: HirExpr, mut then_h: HirBlock, span: Span) -> HirExpr {
    let ty = if hir_block_diverges(&then_h) {
        HirTy::Never
    } else {
        then_h
            .trailing
            .as_ref()
            .map_or(HirTy::Unit, |e| e.ty.clone())
    };
    then_h.stmts.insert(0, HirStmt::Expr(cond));
    HirExpr {
        kind: HirExprKind::Block(Box::new(then_h)),
        ty,
        span,
    }
}

/// The `else` of a refined `if`: its errors about `T` say that only the
/// branch knows `T`, and a `then` value of type `X` is the `else`'s `T`.
pub(crate) fn else_under_tests(
    ctx: &LowerCtx<'_>,
    tests: Option<&TestShape>,
    by: &[(u32, HirTy)],
    since: usize,
    then_h: &mut HirBlock,
    else_h: &HirExpr,
) {
    let Some(TestShape::And(ts, _)) = tests.filter(|_| !by.is_empty()) else {
        return;
    };
    let name = &ts[0].name;
    help_since(
        ctx,
        since,
        name,
        &format!(
            "only the `if {name} is ..` branch knows `{name}`; return a `{name}` here, or add a \
             bound (`{name}: num::One`) and write `{name}::one()`"
        ),
    );
    if let Some(t) = then_h.trailing.as_mut() {
        if t.ty != else_h.ty && swap_params(ctx, &else_h.ty, by) == t.ty {
            t.ty = else_h.ty.clone();
        }
    }
}
