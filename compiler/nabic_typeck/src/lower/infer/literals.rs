//! Unsuffixed number literals: open types, defaults and picks.

#[allow(clippy::wildcard_imports)]
use crate::lower::infer::*;

/// An unsuffixed number literal's type: inside a body, a variable holding
/// `prim` (its default) until a use fixes it (`let c = 0; c += n_i64;`);
/// elsewhere `prim`.
pub(crate) fn literal_ty(
    ctx: &LowerCtx<'_>,
    at: Span,
    prim: PrimTy,
    open: bool,
    neg: bool,
) -> HirTy {
    // A literal meeting an open literal's variable shares it
    // (`[1, 2, 3]`: one element type for the later uses to fix).
    if let Some(t @ HirTy::Var(_)) = ctx.current_expected() {
        if active(ctx) && zonk(ctx, t) == HirTy::Prim(prim) {
            return t.clone();
        }
    }
    let sink = &ctx.env.sink;
    let all = sink.lit_open.get().is_some();
    if !(open && all) {
        return HirTy::Prim(prim);
    }
    let mut vars = sink.infer_vars.borrow_mut();
    let v = u32::try_from(vars.len()).expect("inference variable overflow");
    if all {
        sink.lit_vars.borrow_mut().push((at, v));
    }
    vars.push(Var {
        ty: Some(HirTy::Prim(prim)),
        weak: true,
        neg,
        fixed: at,
        conflict: None,
        lit: true,
        default_from: None,
    });
    HirTy::Var(v)
}

/// Is `e` made of number literals only (`0`, `-1`, `2 * (3 + 4)`)?
/// `Some(true)` for integers, `Some(false)` with a float in it.
pub(crate) fn literal_tree(e: &Expr) -> Option<bool> {
    match &e.kind {
        ExprKind::Literal(AstLit::Int(_)) => Some(true),
        ExprKind::Literal(AstLit::Float(_)) => Some(false),
        ExprKind::OpPrefix { op, operand } if op == "-" => literal_tree(operand),
        ExprKind::OpInfix { op, lhs, rhs } if expr::is_arith_op(op) => {
            Some(literal_tree(lhs)? && literal_tree(rhs)?)
        }
        _ => None,
    }
}

/// The number type an open literal variable `t` holds while its default
/// is not fixed by a use.
pub(crate) fn open_literal(ctx: &LowerCtx<'_>, t: &HirTy) -> Option<(usize, PrimTy)> {
    let HirTy::Var(v) = t else {
        return None;
    };
    let v = root(ctx, *v as usize);
    let vars = ctx.env.sink.infer_vars.borrow();
    match (&vars[v].ty, vars[v].weak) {
        (Some(HirTy::Prim(p)), true) => Some((v, *p)),
        _ => None,
    }
}

/// `f` without its effect on the inference variables: a question asked
/// of a candidate type binds nothing.
pub fn trial(ctx: &LowerCtx<'_>, f: &dyn Fn() -> bool) -> bool {
    let saved = ctx.env.sink.infer_vars.borrow().clone();
    let ok = f();
    *ctx.env.sink.infer_vars.borrow_mut() = saved;
    ok
}

/// An open literal variable fixed to its default (a method call on it
/// that no single other type decides).
pub(crate) fn force_default(ctx: &LowerCtx<'_>, t: &HirTy) -> Option<HirTy> {
    let (v, d) = open_literal(ctx, t)?;
    let mut vars = ctx.env.sink.infer_vars.borrow_mut();
    vars[v].weak = false;
    Some(HirTy::Prim(d))
}

/// The default an open literal variable `t` holds, if it is one.
pub fn literal_default(ctx: &LowerCtx<'_>, t: &HirTy) -> Option<HirTy> {
    open_literal(ctx, t).map(|(_, p)| HirTy::Prim(p))
}

/// An open literal that only one number type of its kind makes work
/// (`9.once()`, `area_of(1)`, `let s: any Shape = 7` with the trait
/// implemented for `i32` alone) takes that type; its default wins when it
/// works too. Returns the type picked.
pub(crate) fn pick_literal_type(
    ctx: &LowerCtx<'_>,
    t: &HirTy,
    works: &dyn Fn(&HirTy) -> bool,
) -> Option<HirTy> {
    if !active(ctx) {
        return None;
    }
    let (v, default) = open_literal(ctx, t)?;
    let d = HirTy::Prim(default);
    if trial(ctx, &|| works(&d)) {
        return None;
    }
    let kinds: &[PrimTy] = if default.is_float() {
        &[P::F32, P::F64]
    } else {
        &[
            P::I32,
            P::I64,
            P::U8,
            P::U16,
            P::U32,
            P::U64,
            P::Usize,
            P::Isize,
            P::I8,
            P::I16,
        ]
    };
    let hits: Vec<HirTy> = kinds
        .iter()
        .map(|p| HirTy::Prim(*p))
        .filter(|c| trial(ctx, &|| works(c)))
        .collect();
    let [one] = hits.as_slice() else {
        return None;
    };
    let mut vars = ctx.env.sink.infer_vars.borrow_mut();
    vars[v].ty = Some(one.clone());
    vars[v].weak = false;
    Some(one.clone())
}

/// The open literal variables (roots) in `t`.
pub(crate) fn open_literals_in(ctx: &LowerCtx<'_>, t: &HirTy, out: &mut Vec<usize>) {
    let mut rec = |t: &HirTy| open_literals_in(ctx, t, out);
    match t {
        HirTy::Var(_) => {
            if let Some((v, _)) = open_literal(ctx, t) {
                if !out.contains(&v) {
                    out.push(v);
                }
            }
        }
        HirTy::Struct(id) => ctx.struct_def(*id).generic_args.iter().for_each(rec),
        HirTy::Enum(id) => ctx.enum_def(*id).generic_args.iter().for_each(rec),
        HirTy::Sum(ps) | HirTy::Tuple(ps) => ps.iter().for_each(rec),
        HirTy::Array(e, _) | HirTy::GcBuf(e) => rec(e),
        HirTy::Fn(a, r, _) => {
            a.iter().for_each(&mut rec);
            rec(r);
        }
        HirTy::Closure { args, ret, env, .. } => {
            args.iter().for_each(&mut rec);
            rec(ret);
            rec(env);
        }
        HirTy::Throws { ok, err } => {
            rec(ok);
            rec(err);
        }
        _ => {}
    }
}

/// The number types a literal variable holding `p` can take.
pub(crate) const fn kind_of(p: PrimTy) -> &'static [PrimTy] {
    if p.is_float() {
        &[P::F32, P::F64]
    } else {
        &[
            P::I32,
            P::I64,
            P::U8,
            P::U16,
            P::U32,
            P::U64,
            P::Usize,
            P::Isize,
            P::I8,
            P::I16,
        ]
    }
}

/// The open literals of `t` take the one choice of number types under
/// which `works(t)` holds (`(1, 2).foo()` with `Foo` on `(i32, i32)`
/// alone); with none or several, their defaults. Either way they are
/// fixed here.
pub(crate) fn pick_literals(ctx: &LowerCtx<'_>, t: &HirTy, works: &dyn Fn(&HirTy) -> bool) {
    pick_literals_or(ctx, t, works, &|_| false, true);
}

/// `pick_literals`, falling back on `then` when `works` picks no one
/// choice (`3.wave()` with `Wave` out of scope: its impl's type, for the
/// error to name the trait); without `default`, no choice leaves them open.
pub(crate) fn pick_literals_or(
    ctx: &LowerCtx<'_>,
    t: &HirTy,
    works: &dyn Fn(&HirTy) -> bool,
    then: &dyn Fn(&HirTy) -> bool,
    default: bool,
) {
    fn go(
        i: usize,
        choice: &mut Vec<PrimTy>,
        held: &[PrimTy],
        hits: &mut Vec<Vec<PrimTy>>,
        test: &dyn Fn(&[PrimTy]) -> bool,
    ) {
        if hits.len() > 1 {
            return;
        }
        if i == held.len() {
            if test(choice) {
                hits.push(choice.clone());
            }
            return;
        }
        for p in kind_of(held[i]) {
            choice[i] = *p;
            go(i + 1, choice, held, hits, test);
        }
    }
    let mut vs = Vec::new();
    open_literals_in(ctx, t, &mut vs);
    if vs.is_empty() {
        return;
    }
    let held: Vec<PrimTy> = {
        let vars = ctx.env.sink.infer_vars.borrow();
        vs.iter()
            .map(|v| match vars[*v].ty {
                Some(HirTy::Prim(p)) => p,
                _ => PrimTy::I64,
            })
            .collect()
    };
    let set = |choice: &[PrimTy]| {
        let mut vars = ctx.env.sink.infer_vars.borrow_mut();
        for (v, p) in vs.iter().zip(choice) {
            vars[*v].ty = Some(HirTy::Prim(*p));
            vars[*v].weak = false;
        }
    };
    let unique = |works: &dyn Fn(&HirTy) -> bool| {
        let test = |c: &[PrimTy]| {
            trial(ctx, &|| {
                set(c);
                works(&zonk(ctx, t))
            })
        };
        let mut hits = Vec::new();
        // Every choice over a few variables; more keep their defaults.
        if vs.len() <= 3 {
            go(0, &mut held.clone(), &held, &mut hits, &test);
        }
        match <[Vec<PrimTy>; 1]>::try_from(hits) {
            Ok([one]) => Some(one),
            Err(_) => None,
        }
    };
    let held_works = || {
        trial(ctx, &|| {
            set(&held);
            works(&zonk(ctx, t))
        })
    };
    match unique(works).or_else(|| unique(then)) {
        Some(one) => set(&one),
        // Several choices: the defaults, as a bare literal's.
        None if default || held_works() => set(&held),
        None => {}
    }
}

/// `t` with its open literals fixed to their defaults (a use that needs
/// the type now).
pub(crate) fn fix_literals(ctx: &LowerCtx<'_>, t: &HirTy) -> HirTy {
    if !has_var(ctx, t) {
        return t.clone();
    }
    pick_literals(ctx, t, &|_| false);
    resolve(ctx, t)
}

/// An open literal filling a type parameter that declares the number
/// default `d` (`Vec3::new(1.0, ..)` with `Vec3<T = f32>`) takes `d` as
/// its default type: it still yields to a written or later use. A float
/// default takes either kind of literal, an integer one integers only.
/// Two different declared defaults for one literal are an error.
pub fn adopt_default(ctx: &LowerCtx<'_>, t: &HirTy, d: &HirTy, origin: &dyn Fn() -> String) {
    let HirTy::Prim(q) = *d else {
        return;
    };
    if !active(ctx) || !(q.is_int() || q.is_float()) {
        return;
    }
    let Some((v, held)) = open_literal(ctx, t) else {
        return;
    };
    let lit = ctx.env.sink.infer_vars.borrow()[v].lit;
    if !lit || !(q.is_float() || held.is_int()) {
        return;
    }
    let prior = ctx.env.sink.infer_vars.borrow()[v].default_from.clone();
    match prior {
        Some(_) if held == q => {}
        Some((at, first)) => default_conflict(ctx, v, (held, &first, at), (q, &origin())),
        None => {
            let mut vars = ctx.env.sink.infer_vars.borrow_mut();
            vars[v].ty = Some(HirTy::Prim(q));
            vars[v].default_from = Some((ctx.here(), origin()));
        }
    }
}

/// One literal reaching two declared defaults: `s` would take a type by
/// statement order, so it is an error naming both.
fn default_conflict(
    ctx: &LowerCtx<'_>,
    v: usize,
    (had, first, at): (PrimTy, &str, Span),
    (now, second): (PrimTy, &str),
) {
    let local = ctx.lit_lets.iter().find_map(|(l, (src, decl))| {
        let HirTy::Var(w) = ctx.var_ty(*l) else {
            return None;
        };
        let name = ctx.locals.get(l.0 as usize)?.name.clone()?;
        (root(ctx, w as usize) == v).then(|| (name, src.clone(), *decl))
    });
    let (what, help) = match &local {
        Some((name, src, _)) => (
            format!("`{name}`"),
            format!(
                "say the type at the declaration: `let {name}: {} = {src}` (or `{}`)",
                had.name(),
                now.name()
            ),
        ),
        None => (
            "the literal".to_owned(),
            format!(
                "write its type: a `{}` or `{}` suffix",
                had.name(),
                now.name()
            ),
        ),
    };
    let mut d = nabic_diagnostic::NabiDiagnostic::error(
        Code::MISMATCHED_TYPES,
        ctx.here(),
        format!(
            "conflicting uses of {what}: `{}` ({second}) here, but `{}` ({first}) at {}:{}",
            now.name(),
            had.name(),
            at.line,
            at.col
        ),
    )
    .with_secondary(at, format!("`{}` taken here", had.name()))
    .with_help(help);
    if let Some((name, _, decl)) = local {
        d = d.with_secondary(decl, format!("`{name}` declared here"));
    }
    ctx.env.sink.diagnostics.borrow_mut().push(d);
    // Reported once: the first default stays.
    let mut vars = ctx.env.sink.infer_vars.borrow_mut();
    vars[v].weak = false;
}

/// An open literal set to `p` for good, whatever its kind (after an error
/// that said what to write, so nothing reports it again).
pub fn force_literal(ctx: &LowerCtx<'_>, t: &HirTy, p: PrimTy) {
    if let Some((v, _)) = open_literal(ctx, t) {
        let mut vars = ctx.env.sink.infer_vars.borrow_mut();
        vars[v].ty = Some(HirTy::Prim(p));
        vars[v].weak = false;
    }
}
