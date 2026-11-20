//! `match`, patterns and exhaustiveness.

mod exhaust;

pub(crate) use crate::lower::pattern::exhaust::*;

#[allow(clippy::wildcard_imports)]
use crate::lower::*;
use nabic_diagnostic::Code;

pub(crate) fn lower_match(
    ctx: &mut LowerCtx<'_>,
    scrutinee: &Expr,
    arms: &[AstMatchArm],
    span: nabic_diagnostic::Span,
) -> HirExpr {
    let since_match = ctx.env.sink.diagnostics.borrow().len();
    let scrut_h = lower_expected(ctx, scrutinee, HirTy::Unknown);
    let scrut_ty = scrut_h.ty.clone();
    if scrut_ty == HirTy::Unknown {
        return scrut_h;
    }
    // A known slot type (`let x: T = match ...`) unifies the arms.
    let expected = ctx
        .current_expected()
        .cloned()
        .filter(|t| *t != HirTy::Unknown);
    let mut arm_h: Vec<HirMatchArm> = Vec::with_capacity(arms.len());
    let mut ended = Vec::new();
    // A pattern that failed to lower stands in as `_`: reachability
    // over it would report errors of its own.
    let mut bad_pattern = false;
    // `if let` / `while let` / `let .. else`: `pat` then a `_` the parser
    // wrote (at the `if` / `while`, or spanning the `else` block).
    let let_form = matches!(arms, [_, w] if w.guard.is_none()
        && matches!(w.pat.kind, AstPatternKind::Wildcard)
        && ((w.pat.span.file == span.file && w.pat.span.start == span.start)
            || w.pat.span == w.body.span
            || matches!(&arms[0].pat.kind, AstPatternKind::Binding { name, .. } if name == "__letelse_v")));
    for (i, arm) in arms.iter().enumerate() {
        let mark = ctx.unnarrowed.len();
        ctx.push_scope();
        let since = ctx.env.sink.diagnostics.borrow().len();
        let pat = (let_form && i == 0)
            .then(|| some_pattern(ctx, &arm.pat, &scrut_ty))
            .flatten()
            .or_else(|| inner_layer_binding(ctx, &arm.pat, &scrut_ty, &arm_h))
            .unwrap_or_else(|| lower_pattern(ctx, &arm.pat, &scrut_ty));
        if crate::lower::mono::writable_place(ctx, &scrut_h) {
            if let HirPattern::Binding { local, .. }
            | HirPattern::At { local, .. }
            | HirPattern::SumVariant {
                binder: Some(local),
                ..
            } = &pat
            {
                ctx.arm_bound.insert(*local, scrut_h.clone());
            }
        }
        bad_pattern |= ctx.env.sink.diagnostics.borrow()[since..]
            .iter()
            .any(nabic_diagnostic::NabiDiagnostic::is_error);
        let guard = arm.guard.as_ref().map(|g| lower_cond(ctx, g));
        let cells = ctx.take_cells(arm.body.span);
        let body = crate::lower::expr::with_lets(cells, lower_expr(ctx, &arm.body));
        ctx.pop_scope();
        ended.extend(crate::lower::expr::branch_unnarrowings(ctx, mark));
        arm_h.push(HirMatchArm { pat, guard, body });
    }
    crate::lower::expr::rejoin_unnarrowings(ctx, ended);
    if bad_pattern {
        crate::lower::habits::result_match(ctx, scrutinee, arms, &scrut_ty, since_match, span);
    }
    // `let x: T = v else { ... };` is parsed as a two-arm match whose
    // second arm is the else block (parser desugar).
    let is_let_else = matches!(
        arms.first().map(|a| &a.pat.kind),
        Some(AstPatternKind::Binding { name, .. } | AstPatternKind::TypeCheck { binder: Some(name), .. })
            if name == "__letelse_v"
    );
    // Exhaustiveness and unreachable arms.
    if !is_let_else && !bad_pattern {
        // `if let` / `while let` desugar to `match { pat => .., _ => .. }`
        // with `_` at the `if` keyword; an irrefutable `pat` makes that
        // arm unreachable by construction, so don't warn (matched by where
        // it starts: no written arm starts at its `match`).
        // A `catch` dispatch reports its own unreachable arms.
        let catch_dispatch = matches!(&scrutinee.kind,
            ExprKind::Path(p) if p.segments.len() == 1 && p.segments[0].name.starts_with('#'));
        let synthetic: Vec<bool> = arms
            .iter()
            .map(|a| {
                catch_dispatch
                    || (matches!(a.pat.kind, AstPatternKind::Wildcard)
                        && a.pat.span.file == span.file
                        && a.pat.span.start == span.start)
            })
            .collect();
        check_match_arms(ctx, &scrut_ty, &arm_h, &synthetic);
    }

    // Result type: the expected slot, else the first arm that produces
    // a value; every other value-producing arm must agree.
    let inferred = expected.is_none();
    // An unsuffixed integer arm follows the others' integer type, as an
    // `if` branch does: `none => 0` beside `p => p.number`.
    let lit_arm = |i: usize| {
        let body = &arms[i].body;
        match &body.kind {
            ExprKind::Block(b) if b.stmts.is_empty() => b
                .trailing
                .as_deref()
                .is_some_and(crate::lower::expr::is_untyped_int),
            _ => crate::lower::expr::is_untyped_int(body),
        }
    };
    let result_ty = expected.unwrap_or_else(|| {
        let values = || {
            arm_h
                .iter()
                .enumerate()
                .filter(|(_, a)| !hir_expr_diverges(&a.body))
        };
        values()
            .find(|(i, _)| !lit_arm(*i))
            .or_else(|| values().next())
            .map_or(HirTy::Unit, |(_, a)| a.body.ty.clone())
    });
    // A sum one arm already is takes the others (`none` beside `T?`).
    let values: Vec<HirTy> = arm_h
        .iter()
        .filter(|a| !hir_expr_diverges(&a.body))
        .map(|a| a.body.ty.clone())
        .collect();
    let joined = branch_join(ctx, &values).filter(|_| inferred && result_ty != HirTy::Unit);
    let result_ty = joined.clone().unwrap_or(result_ty);
    let mut failed = false;
    // The arm the result type came from, for open arms to meet.
    let first_value = (inferred && crate::lower::infer::active(ctx))
        .then(|| {
            arm_h
                .iter()
                .find(|a| a.body.ty == result_ty)
                .map(|a| a.body.clone())
        })
        .flatten();
    let mut out = Vec::with_capacity(arm_h.len());
    for (i, mut a) in arm_h.into_iter().enumerate() {
        if inferred
            && lit_arm(i)
            && a.body.ty != result_ty
            && matches!(result_ty, HirTy::Prim(p) if p.is_int())
        {
            a.body = lower_expected(ctx, &arms[i].body, result_ty.clone());
        }
        // A unit-typed match (statement position) discards arm values.
        if !hir_expr_diverges(&a.body) && (result_ty != HirTy::Unit || is_let_else) {
            if is_let_else && i == 1 {
                ctx.emit_error(
                    Code::LET_ELSE_NOT_DIVERGING,
                    "the `else` block of `let ... else` must diverge (end in `return`, \
                     `break`, `continue` or `throw`)"
                        .to_owned(),
                );
            } else if inferred
                && joined.is_none()
                && a.body.ty != HirTy::Unknown
                && !first_value
                    .as_ref()
                    .is_some_and(|f| crate::lower::infer::join(ctx, f, &a.body))
                && !coercible(ctx, &result_ty, &a.body.ty)
            {
                // No sum type is inferred from differing arms.
                failed = true;
                ctx.emit_error(
                    Code::BRANCH_TYPES,
                    format!(
                        "`match` arms have different types `{}` and `{}` (arm {}) - Nabi does \
                         not infer a sum type; annotate the slot (`let v: {} = match ...`)",
                        ctx.ty_name(&result_ty),
                        ctx.ty_name(&a.body.ty),
                        i + 1,
                        ctx.sum_hint(&result_ty, &a.body.ty)
                    ),
                );
            } else {
                a.body = coerce(ctx, &result_ty, a.body, &|| format!("match arm {}", i + 1));
            }
        }
        out.push(a);
    }
    HirExpr {
        kind: HirExprKind::Match {
            scrutinee: Box::new(scrut_h),
            arms: out,
        },
        // No follow-up error for the match's value.
        ty: if failed { HirTy::Unknown } else { result_ty },
        span,
    }
}

/// A plain name in `if let x = e` / `while let` / `let x = e else` on an
/// optional `e` binds the value without its `none` (one layer: `x` of a
/// `(T | none)?` is `T | none`).
fn some_pattern(ctx: &mut LowerCtx<'_>, pat: &AstPattern, scrut_ty: &HirTy) -> Option<HirPattern> {
    let (HirTy::Sum(vs), AstPatternKind::Binding { name, sub: None }) = (scrut_ty, &pat.kind)
    else {
        return None;
    };
    if name == "_" || ctx.enum_path(&[name.as_str()]).is_some() {
        return None;
    }
    let none = vs.iter().position(|v| *v == HirTy::None)?;
    let rest: Vec<usize> = (0..vs.len()).filter(|i| *i != none).collect();
    Some(narrow_to(ctx, name, scrut_ty, &rest, pat.span))
}

/// Bind `name` to the members `idxs` of sum `scrut_ty` (one: that member's
/// type; several: their sum).
pub(crate) fn narrow_to(
    ctx: &mut LowerCtx<'_>,
    name: &str,
    scrut_ty: &HirTy,
    idxs: &[usize],
    at: nabic_diagnostic::Span,
) -> HirPattern {
    let HirTy::Sum(vs) = scrut_ty else {
        unreachable!("narrowing a sum")
    };
    let index = |i: usize| u32::try_from(i).expect("variant overflow");
    if let [i] = idxs {
        let t = vs[*i].clone();
        let local = ctx.declare_bound(name, t.clone(), at);
        return HirPattern::SumVariant {
            sum_ty: scrut_ty.clone(),
            variant_index: index(*i),
            binder: Some(local),
            payload_ty: Some(t),
        };
    }
    let target = HirTy::Sum(idxs.iter().map(|i| vs[*i].clone()).collect());
    let alts = HirPattern::Or(
        idxs.iter()
            .map(|i| HirPattern::SumVariant {
                sum_ty: scrut_ty.clone(),
                variant_index: index(*i),
                binder: None,
                payload_ty: None,
            })
            .collect(),
    );
    let binder = ctx.declare_bound(name, target, at);
    let whole = ctx.declare(None, scrut_ty.clone());
    ctx.pending_resums.push((binder, whole));
    HirPattern::At {
        local: whole,
        ty: scrut_ty.clone(),
        sub: Box::new(alts),
    }
}

/// Patterns act on a nested sum's outer layer; a catch-all after arms
/// covering every outer member binds the inner one (`none => .., v => ..`
/// on `(i32 | none)?` gives `v: i32 | none`).
fn inner_layer_binding(
    ctx: &mut LowerCtx<'_>,
    pat: &AstPattern,
    scrut_ty: &HirTy,
    before: &[HirMatchArm],
) -> Option<HirPattern> {
    let (HirTy::Sum(vs), AstPatternKind::Binding { name, sub: None }) = (scrut_ty, &pat.kind)
    else {
        return None;
    };
    if name == "_" || ctx.enum_path(&[name.as_str()]).is_some() {
        return None;
    }
    let mut covered = Vec::new();
    for a in before.iter().filter(|a| a.guard.is_none()) {
        covered_variants(&a.pat, &mut covered);
    }
    let mut rest =
        (0..vs.len()).filter(|i| !covered.contains(&u32::try_from(*i).unwrap_or(u32::MAX)));
    let (i, None) = (rest.next()?, rest.next()) else {
        return None;
    };
    let inner = vs[i].clone();
    if !matches!(inner, HirTy::Sum(_)) {
        return None;
    }
    let local = ctx.declare_bound(name, inner.clone(), pat.span);
    Some(HirPattern::SumVariant {
        sum_ty: scrut_ty.clone(),
        variant_index: u32::try_from(i).expect("variant overflow"),
        binder: Some(local),
        payload_ty: Some(inner),
    })
}

/// Matches every value of its type (no refutable sub-pattern).
pub(crate) fn hir_pattern_irrefutable(p: &HirPattern) -> bool {
    p.is_irrefutable()
}

pub(crate) fn lower_pattern(
    ctx: &mut LowerCtx<'_>,
    pat: &AstPattern,
    scrut_ty: &HirTy,
) -> HirPattern {
    let typed = bare_type_patterns(ctx, pat, scrut_ty);
    let pat = typed.as_ref().unwrap_or(pat);
    let p = lower_pattern_inner(ctx, pat, scrut_ty);
    // A pattern that failed to lower still declares its names (as
    // poisoned locals) so their uses don't cascade into "cannot find".
    if matches!(p, HirPattern::Wildcard) {
        for name in pattern_bound_names(pat) {
            if ctx.lookup_in_current_scope(&name).is_none() {
                ctx.declare(Some(name), HirTy::Unknown);
            }
        }
    }
    p
}

/// Against a sum or `any`, a bare type name (`f64 =>`) is a
/// type pattern, not a binding; elsewhere it stays a binding.
fn bare_type_patterns(
    ctx: &LowerCtx<'_>,
    pat: &AstPattern,
    scrut_ty: &HirTy,
) -> Option<AstPattern> {
    if !matches!(scrut_ty, HirTy::Sum(_) | HirTy::AnyTrait(_)) {
        return None;
    }
    let as_check = |p: &AstPattern| {
        let AstPatternKind::Binding { name, sub: None } = &p.kind else {
            return None;
        };
        if ctx.enum_path(&[name.as_str()]).is_some() {
            return None;
        }
        let ty = nabic_ast::Ty {
            kind: nabic_ast::TyKind::Path(nabic_ast::Path {
                segments: vec![nabic_ast::PathSegment {
                    name: name.clone(),
                    generics: Vec::new(),
                }],
                span: p.span,
                origin: None,
            }),
            span: p.span,
        };
        let resolved = ctx.resolve_ast_ty(&ty);
        (!matches!(resolved, HirTy::Path(_) | HirTy::None | HirTy::Unknown)).then_some(AstPattern {
            kind: AstPatternKind::TypeCheck { binder: None, ty },
            span: p.span,
        })
    };
    match &pat.kind {
        AstPatternKind::Or(alts) => {
            let new: Vec<Option<AstPattern>> = alts.iter().map(as_check).collect();
            new.iter().any(Option::is_some).then(|| AstPattern {
                kind: AstPatternKind::Or(
                    new.into_iter()
                        .zip(alts)
                        .map(|(n, a)| n.unwrap_or_else(|| a.clone()))
                        .collect(),
                ),
                span: pat.span,
            })
        }
        _ => as_check(pat),
    }
}

pub(crate) fn lower_pattern_inner(
    ctx: &mut LowerCtx<'_>,
    pat: &AstPattern,
    scrut_ty: &HirTy,
) -> HirPattern {
    // A path pattern resolves where it was written; its
    // sub-patterns set their own.
    let origin = match &pat.kind {
        AstPatternKind::Struct { path, .. } | AstPatternKind::EnumVariant { path, .. } => {
            path.origin.as_deref()
        }
        _ => None,
    };
    let outer = ctx.set_access(origin);
    let p = lower_pattern_here(ctx, pat, scrut_ty);
    ctx.access.replace(outer);
    p
}

#[allow(clippy::cognitive_complexity)] // one arm per pattern kind
fn lower_pattern_here(ctx: &mut LowerCtx<'_>, pat: &AstPattern, scrut_ty: &HirTy) -> HirPattern {
    let mismatch = |ctx: &LowerCtx<'_>, what: String| {
        if *scrut_ty != HirTy::Unknown {
            ctx.emit_error(
                Code::PATTERN_TYPE,
                format!(
                    "{what} can't match a value of type `{}`",
                    ctx.ty_name(scrut_ty)
                ),
            );
        }
        HirPattern::Wildcard
    };
    // `MyError { code }` / `E::A(x)` on an `any` value: narrow to that
    // type, then match its payload (`catch` of an untyped `throws`).
    if let (
        HirTy::AnyTrait(bounds),
        AstPatternKind::Struct { .. } | AstPatternKind::EnumVariant { .. },
    ) = (scrut_ty, &pat.kind)
    {
        if let Some(t) = crate::lower::throws::arm_filter(ctx, pat)
            .filter(|t| bounds.iter().all(|b| ctx.implements_trait(t, b, 0)))
        {
            return HirPattern::SumMember {
                sum_ty: scrut_ty.clone(),
                variant_index: ctx.any_tag(&t),
                sub: Box::new(lower_pattern_inner(ctx, pat, &t)),
                payload_ty: t,
            };
        }
    }
    match &pat.kind {
        AstPatternKind::Wildcard => HirPattern::Wildcard,
        AstPatternKind::Binding { name, sub: None } if name == "_" => HirPattern::Wildcard,
        // A bare name that is an imported variant (`use E::*` then
        // `A =>`) matches the variant rather than binding.
        AstPatternKind::Binding { name, sub: None }
            if ctx.enum_path(&[name.as_str()]).is_some() =>
        {
            let segments = vec![nabic_ast::PathSegment {
                name: name.clone(),
                generics: Vec::new(),
            }];
            let path = nabic_ast::Path {
                segments,
                span: pat.span,
                origin: None,
            };
            let variant = AstPattern {
                kind: AstPatternKind::EnumVariant {
                    path,
                    args: Vec::new(),
                },
                span: pat.span,
            };
            lower_pattern(ctx, &variant, scrut_ty)
        }
        // A bare name that is a variant of the scrutinee's enum is that
        // variant.
        AstPatternKind::Binding { name, sub: None }
            if variant_named(ctx, scrut_ty, name).is_some() =>
        {
            let (enum_id, idx, base) = variant_named(ctx, scrut_ty, name).expect("guard");
            let args = ctx.enum_def(enum_id).variants[idx].arity;
            if args != 0 {
                ctx.emit_error_at(
                    Code::PATTERN_ARITY,
                    pat.span,
                    format!("variant `{base}::{name}` has a payload: match it as `{name}(..)`"),
                );
            }
            HirPattern::EnumVariant {
                enum_id,
                variant_index: u32::try_from(idx).expect("variant overflow"),
                args: vec![HirPattern::Wildcard; args],
            }
        }
        AstPatternKind::Binding { name, sub } => {
            // `name @ sub`: bind the whole value, match like `sub`.
            let at = |ctx: &mut LowerCtx<'_>, local: LocalId| match sub {
                None => HirPattern::Binding {
                    local,
                    ty: scrut_ty.clone(),
                },
                Some(p) => HirPattern::At {
                    local,
                    ty: scrut_ty.clone(),
                    sub: Box::new(lower_pattern(ctx, p, scrut_ty)),
                },
            };
            binds_const(ctx, name, scrut_ty, pat.span);
            let local = ctx.declare_bound(name, scrut_ty.clone(), pat.span);
            at(ctx, local)
        }
        AstPatternKind::Literal(AstLit::None) => {
            // `none` against a sum scrutinee -> match the none variant.
            if let HirTy::Sum(variants) = scrut_ty {
                if let Some(idx) = variants.iter().position(HirTy::is_none) {
                    return HirPattern::SumVariant {
                        sum_ty: scrut_ty.clone(),
                        variant_index: u32::try_from(idx).expect("variant overflow"),
                        binder: None,
                        payload_ty: None,
                    };
                }
            }
            mismatch(ctx, "a `none` pattern".to_owned())
        }
        // A literal / range on a sum tests the one member it can match
        // (`'a'` on `char | none`).
        AstPatternKind::Literal(_) | AstPatternKind::Range { .. }
            if matches!(scrut_ty, HirTy::Sum(_)) =>
        {
            let HirTy::Sum(vs) = scrut_ty else {
                unreachable!()
            };
            let fits = |v: &HirTy| match &pat.kind {
                AstPatternKind::Literal(lit) => literal_to_const(ctx, lit, v).is_some(),
                AstPatternKind::Range { start, end, .. } => {
                    matches!(v, HirTy::Prim(p) if p.is_int())
                        && [start, end].iter().all(|b| {
                            b.as_ref()
                                .is_none_or(|l| literal_to_const(ctx, l, v).is_some())
                        })
                }
                _ => false,
            };
            let Some(i) = vs.iter().position(fits) else {
                return mismatch(ctx, "this literal pattern".to_owned());
            };
            let member = vs[i].clone();
            HirPattern::SumMember {
                sum_ty: scrut_ty.clone(),
                variant_index: u32::try_from(i).expect("variant overflow"),
                sub: Box::new(lower_pattern_inner(ctx, pat, &member)),
                payload_ty: member,
            }
        }
        // A tuple pattern on a sum tests its one tuple member of that
        // arity (`let (a, b) = pair() else { .. }` on `(i32, str) | none`).
        AstPatternKind::Tuple(ps) if matches!(scrut_ty, HirTy::Sum(_)) => {
            let HirTy::Sum(vs) = scrut_ty else {
                unreachable!()
            };
            let has_rest = ps.iter().any(|p| matches!(p.kind, AstPatternKind::Rest(_)));
            let fits = |v: &HirTy| {
                matches!(v, HirTy::Tuple(ts)
                    if ts.len() == ps.len() || (has_rest && ts.len() + 1 >= ps.len()))
            };
            let mut hits = vs.iter().enumerate().filter(|(_, v)| fits(v));
            let (Some((i, member)), None) = (hits.next(), hits.next()) else {
                return mismatch(ctx, format!("a {}-tuple pattern", ps.len()));
            };
            let member = member.clone();
            HirPattern::SumMember {
                sum_ty: scrut_ty.clone(),
                variant_index: u32::try_from(i).expect("variant overflow"),
                sub: Box::new(lower_pattern_inner(ctx, pat, &member)),
                payload_ty: member,
            }
        }
        // On an open literal, as the type it holds; the body's end
        // retypes it with the scrutinee (`infer::Fixer`).
        AstPatternKind::Literal(lit) => {
            match literal_to_const(ctx, lit, &crate::lower::infer::zonk(ctx, scrut_ty)) {
                Some((value, prim)) => HirPattern::Literal { value, prim },
                None if crate::lower::half::half_of(ctx, scrut_ty).is_some() => {
                    let p = mismatch(ctx, "this literal pattern".to_owned());
                    let text = ctx.src(pat.span).unwrap_or("1.0").to_owned();
                    ctx.help_last(Some(format!(
                        "a 16-bit float matches no literal; compare in a guard: `x if x == {text} =>`"
                    )));
                    p
                }
                None => mismatch(ctx, "this literal pattern".to_owned()),
            }
        }
        AstPatternKind::TypeCheck { binder, ty } => {
            // `name: T`: narrow a sum scrutinee to variant `T` (or, on a non-sum
            // of exactly type `T`, bind it).
            let target_ty = ctx.resolve_ast_ty(ty);
            // An unknown name, said as such (not as a mismatch).
            let mut unknown = Vec::new();
            crate::lower::collect::collect_unknown_paths(&target_ty, &mut unknown);
            if let Some(name) = unknown.iter().find(|n| !n.starts_with(['_', '#'])) {
                let segs: Vec<&str> = name.split("::").collect();
                ctx.emit_error_at(
                    Code::UNRESOLVED_TYPE,
                    ty.span,
                    format!("cannot find type `{name}` in this scope"),
                );
                ctx.help_last(ctx.type_help(&segs));
                if let Some(name) = binder {
                    ctx.declare_bound(name, HirTy::Unknown, pat.span);
                }
                return HirPattern::Wildcard;
            }
            let bind = |ctx: &mut LowerCtx<'_>| {
                binder
                    .as_ref()
                    .map(|name| ctx.declare_bound(name, target_ty.clone(), pat.span))
            };
            // `x: MutexGuard<i32>` on an open `MutexGuard<#var> | none`.
            if let HirTy::Sum(vs) = scrut_ty {
                let open: Vec<&HirTy> = vs
                    .iter()
                    .filter(|v| crate::lower::infer::has_var(ctx, v))
                    .collect();
                if let [one] = open.as_slice() {
                    crate::lower::infer::unify_member(ctx, std::slice::from_ref(&target_ty), one);
                }
            }
            // On a plain value the type is needed now: an open literal
            // takes its default (`let v = 3; match v { n: i64 => .. }`).
            let scrut_ty = &if matches!(scrut_ty, HirTy::Sum(_)) {
                crate::lower::infer::resolve(ctx, scrut_ty)
            } else {
                crate::lower::infer::fix_literals(ctx, scrut_ty)
            };
            if let HirTy::Sum(variants) = scrut_ty {
                if let Some(idx) = sum_variant_index(variants, &target_ty) {
                    let bind_local = bind(ctx);
                    return HirPattern::SumVariant {
                        sum_ty: scrut_ty.clone(),
                        variant_index: u32::try_from(idx).expect("variant overflow"),
                        binder: bind_local,
                        payload_ty: Some(target_ty),
                    };
                }
                // `v: A | B` against `A | B | C` (a generic `T` bound to a
                // sum): any of those tags, rebound as `A | B`.
                if let HirTy::Sum(ws) = &target_ty {
                    let idxs: Vec<usize> = ws
                        .iter()
                        .filter_map(|w| sum_variant_index(variants, w))
                        .collect();
                    if idxs.len() == ws.len() {
                        let alts = idxs
                            .into_iter()
                            .map(|i| HirPattern::SumVariant {
                                sum_ty: scrut_ty.clone(),
                                variant_index: u32::try_from(i).expect("variant overflow"),
                                binder: None,
                                payload_ty: None,
                            })
                            .collect();
                        let alts = HirPattern::Or(alts);
                        let Some(binder) = bind(ctx) else {
                            return alts;
                        };
                        let whole = ctx.declare(None, scrut_ty.clone());
                        ctx.pending_resums.push((binder, whole));
                        return HirPattern::At {
                            local: whole,
                            ty: scrut_ty.clone(),
                            sub: Box::new(alts),
                        };
                    }
                }
            } else if let (HirTy::AnyTrait(bounds), HirTy::Sum(ws)) = (scrut_ty, &target_ty) {
                // `x: A | B` on `any`: the tag of a stored `A | B` value
                // or of either member; the binder is the value as `A | B`.
                if ws
                    .iter()
                    .all(|w| bounds.iter().all(|b| ctx.implements_trait(w, b, 0)))
                {
                    let alts = HirPattern::Or(
                        std::iter::once(&target_ty)
                            .chain(ws)
                            .map(|w| HirPattern::SumVariant {
                                sum_ty: scrut_ty.clone(),
                                variant_index: ctx.any_tag(w),
                                binder: None,
                                payload_ty: None,
                            })
                            .collect(),
                    );
                    let Some(binder) = bind(ctx) else {
                        return alts;
                    };
                    let whole = ctx.declare(None, scrut_ty.clone());
                    ctx.pending_resums.push((binder, whole));
                    return HirPattern::At {
                        local: whole,
                        ty: scrut_ty.clone(),
                        sub: Box::new(alts),
                    };
                }
            } else if let (HirTy::AnyTrait(bounds), false) =
                (scrut_ty, matches!(target_ty, HirTy::AnyTrait(_)))
            {
                // Narrow `any Tr` to a concrete implementor (tag test).
                if bounds
                    .iter()
                    .all(|b| ctx.implements_trait(&target_ty, b, 0))
                {
                    let bind_local = bind(ctx);
                    return HirPattern::SumVariant {
                        sum_ty: scrut_ty.clone(),
                        variant_index: ctx.any_tag(&target_ty),
                        binder: bind_local,
                        payload_ty: Some(target_ty),
                    };
                }
            } else if *scrut_ty == target_ty {
                return match bind(ctx) {
                    Some(local) => HirPattern::Binding {
                        local,
                        ty: target_ty,
                    },
                    None => HirPattern::Wildcard,
                };
            } else if matches!(&target_ty, HirTy::Sum(ws) if ws.contains(scrut_ty)) {
                // `v: A | B` against an `A`: always matches; the binder
                // is the value wrapped as `A | B`.
                let Some(binder) = bind(ctx) else {
                    return HirPattern::Wildcard;
                };
                let whole = ctx.declare(None, scrut_ty.clone());
                ctx.pending_resums.push((binder, whole));
                return HirPattern::Binding {
                    local: whole,
                    ty: scrut_ty.clone(),
                };
            }
            // Every type supports `is`. A test on a type parameter is opaque at
            // the definition check and decided per instance.
            if crate::lower::bounds::has_param(ctx, scrut_ty)
                || crate::lower::bounds::has_param(ctx, &target_ty)
            {
                let binder = bind(ctx);
                return HirPattern::SumVariant {
                    sum_ty: scrut_ty.clone(),
                    variant_index: ctx.any_tag(&target_ty),
                    binder,
                    payload_ty: Some(target_ty),
                };
            }
            if in_instance(ctx) {
                bind(ctx);
                return HirPattern::Or(Vec::new());
            }
            mismatch(ctx, format!("type pattern `{}`", ctx.ty_name(&target_ty)))
        }
        AstPatternKind::Or(alts) => {
            // Every alternative binds the same names: one shared local per name,
            // typed by the first alternative.
            // A bare name that is a variant (`A | B`) binds nothing, as alone.
            let variant = |n: &String| {
                ctx.enum_path(&[n.as_str()]).is_some() || variant_named(ctx, scrut_ty, n).is_some()
            };
            let name_sets: Vec<std::collections::BTreeSet<String>> = alts
                .iter()
                .map(|a| {
                    let mut set = pattern_bound_names(a);
                    if matches!(a.kind, AstPatternKind::Binding { sub: None, .. }) {
                        set.retain(|n| !variant(n));
                    }
                    set
                })
                .collect();
            let bound_names = name_sets.first().cloned().unwrap_or_default();
            for (i, set) in name_sets.iter().enumerate().skip(1) {
                if *set != bound_names {
                    let names = |v: Vec<&String>| {
                        v.iter()
                            .map(|n| format!("`{n}`"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    };
                    let missing = names(bound_names.difference(set).collect());
                    let extra = names(set.difference(&bound_names).collect());
                    let mut detail = String::new();
                    if !missing.is_empty() {
                        detail.push_str(&format!(" missing {missing}"));
                    }
                    if !extra.is_empty() {
                        detail.push_str(&format!(" extra {extra}"));
                    }
                    ctx.emit_error_at(
                        Code::OR_PATTERN_BINDINGS,
                        alts[i].span,
                        format!(
                            "or-pattern alternatives bind different name sets \
                             (alternative {i}:{detail})"
                        ),
                    );
                }
            }
            let mut shared: HashMap<String, LocalId> = HashMap::default();
            for name in &bound_names {
                let local = ctx.declare(Some(name.clone()), HirTy::Unknown);
                shared.insert(name.clone(), local);
            }
            ctx.or_pattern_binders.push(shared);
            let lowered: Vec<HirPattern> = alts
                .iter()
                .map(|a| lower_pattern(ctx, a, scrut_ty))
                .collect();
            ctx.or_pattern_binders.pop();
            HirPattern::Or(lowered)
        }
        // `Color::Red` or `Result::Ok(n)` patterns.
        AstPatternKind::EnumVariant { path, args } => {
            let segs: Vec<&str> = path.segments.iter().map(|s| s.name.as_str()).collect();
            let (enum_id, idx) = match resolve_enum_variant(ctx, &segs, Some(scrut_ty), None) {
                EnumRes::Found(e, i) => (e, i),
                EnumRes::Error => return HirPattern::Wildcard,
                EnumRes::NotEnum => {
                    return ctx.at_span(pat.span, |ctx| {
                        tuple_struct_pattern(ctx, &segs, args, scrut_ty)
                    });
                }
            };
            if *scrut_ty != HirTy::Enum(enum_id) {
                return mismatch(ctx, format!("pattern `{}`", segs.join("::")));
            }
            let variant = ctx.enum_def(enum_id).variants[idx].clone();
            let slots = variant_arg_tys(&variant);
            // `V(a, ..)` / `V(.., z)` / `V(..)`: one `..` stands for the
            // fields not written, each matching anything.
            let rest_at: Vec<usize> = args
                .iter()
                .enumerate()
                .filter(|(_, a)| matches!(a.kind, AstPatternKind::Rest(_)))
                .map(|(i, _)| i)
                .collect();
            if let [at] = rest_at.as_slice() {
                let (before, after) = (&args[..*at], &args[at + 1..]);
                if before.len() + after.len() <= slots.len() {
                    let fill = slots.len() - before.len() - after.len();
                    let mut lowered = Vec::with_capacity(slots.len());
                    for (p, t) in before.iter().zip(&slots) {
                        lowered.push(lower_pattern(ctx, p, t));
                    }
                    lowered.extend(std::iter::repeat_n(HirPattern::Wildcard, fill));
                    for (p, t) in after.iter().zip(&slots[before.len() + fill..]) {
                        lowered.push(lower_pattern(ctx, p, t));
                    }
                    return HirPattern::EnumVariant {
                        enum_id,
                        variant_index: u32::try_from(idx).expect("variant overflow"),
                        args: lowered,
                    };
                }
            } else if rest_at.len() > 1 {
                ctx.emit_error(
                    Code::REST_PATTERN,
                    format!("pattern `{}` has more than one `..`", segs.join("::")),
                );
                return HirPattern::Wildcard;
            }
            if args.len() != slots.len() {
                ctx.emit_error(
                    Code::PATTERN_ARITY,
                    format!(
                        "pattern `{}` has {} field{}, but the variant has {}",
                        segs.join("::"),
                        args.len(),
                        if args.len() == 1 { "" } else { "s" },
                        slots.len()
                    ),
                );
                return HirPattern::Wildcard;
            }
            let lowered_args: Vec<HirPattern> = args
                .iter()
                .zip(&slots)
                .map(|(p, t)| lower_pattern(ctx, p, t))
                .collect();
            HirPattern::EnumVariant {
                enum_id,
                variant_index: u32::try_from(idx).expect("variant overflow"),
                args: lowered_args,
            }
        }
        AstPatternKind::Tuple(pats) => match scrut_ty {
            HirTy::Tuple(elem_tys) if pats.len() == elem_tys.len() => HirPattern::Tuple(
                pats.iter()
                    .zip(elem_tys.iter())
                    .map(|(p, t)| lower_pattern(ctx, p, t))
                    .collect(),
            ),
            // `()` matches the one unit value.
            HirTy::Unit if pats.is_empty() => HirPattern::Wildcard,
            _ => mismatch(ctx, format!("a {}-tuple pattern", pats.len())),
        },
        AstPatternKind::Range {
            start,
            end,
            inclusive,
        } => {
            let scrut_ty = &crate::lower::infer::zonk(ctx, scrut_ty);
            let prim = match scrut_ty {
                HirTy::Prim(p) if p.is_int() => *p,
                _ => return mismatch(ctx, "a range pattern".to_owned()),
            };
            let bound = |ctx: &LowerCtx<'_>, lit: &Option<AstLit>| match lit {
                None => Some(None),
                Some(l) => literal_to_const(ctx, l, scrut_ty).map(|(v, _)| Some(v)),
            };
            match (bound(ctx, start), bound(ctx, end)) {
                (Some(s), Some(e)) => HirPattern::Range {
                    start: s,
                    end: e,
                    inclusive: *inclusive,
                    prim,
                },
                _ => mismatch(ctx, "this range pattern".to_owned()),
            }
        }
        AstPatternKind::Struct { path, fields, .. } => {
            let segs: Vec<&str> = path.segments.iter().map(|s| s.name.as_str()).collect();
            // `Enum::Variant { x, y }`: field names map to positions in
            // the variant's tuple payload.
            match resolve_enum_variant(ctx, &segs, Some(scrut_ty), None) {
                EnumRes::Error => return HirPattern::Wildcard,
                EnumRes::Found(enum_id, idx) => {
                    if *scrut_ty != HirTy::Enum(enum_id) {
                        return mismatch(ctx, format!("pattern `{}`", segs.join("::")));
                    }
                    let variant = ctx.enum_def(enum_id).variants[idx].clone();
                    let field_tys = variant_arg_tys(&variant);
                    let mut args = vec![HirPattern::Wildcard; variant.named_fields.len()];
                    for fp in fields {
                        let Some(i) = variant.named_fields.iter().position(|n| *n == fp.name)
                        else {
                            ctx.emit_error(
                                Code::NO_FIELD,
                                format!(
                                    "enum variant `{}` has no field `{}`",
                                    segs.join("::"),
                                    fp.name
                                ),
                            );
                            continue;
                        };
                        let ty = field_tys[i].clone();
                        args[i] = if let Some(p) = &fp.pat {
                            lower_pattern(ctx, p, &ty)
                        } else {
                            let local = ctx.declare_bound(&fp.name, ty.clone(), pat.span);
                            HirPattern::Binding { local, ty }
                        };
                    }
                    return HirPattern::EnumVariant {
                        enum_id,
                        variant_index: u32::try_from(idx).expect("variant overflow"),
                        args,
                    };
                }
                EnumRes::NotEnum => {}
            }
            let struct_id = crate::lower::expr::pattern_struct(ctx, path, scrut_ty);
            let Some(struct_id) = struct_id else {
                // A struct, but of another type than the scrutinee's.
                if ctx.canon_type(&segs).is_some() {
                    return mismatch(ctx, format!("struct pattern `{}`", segs.join("::")));
                }
                let help = ctx.type_help(&segs);
                ctx.unknown_name(
                    Code::UNRESOLVED_TYPE,
                    pat.span,
                    format!("cannot find struct `{}` in this scope", segs.join("::")),
                    help,
                );
                return HirPattern::Wildcard;
            };
            // `Q { .. }` (`type Q = P<f64>`) on a `P` of open arguments fixes them.
            let want = HirTy::Struct(struct_id);
            if *scrut_ty != want && !crate::lower::infer::unify(ctx, scrut_ty, &want) {
                return mismatch(ctx, format!("struct pattern `{}`", segs.join("::")));
            }
            struct_pattern_rest(ctx, pat, scrut_ty);
            let def = ctx.struct_def(struct_id);
            let mut lowered: Vec<(u32, HirPattern)> = Vec::new();
            for fp in fields {
                let Some(idx) = def.fields.iter().position(|f| f.name == fp.name) else {
                    ctx.no_field(struct_id, &fp.name, pat.span);
                    continue;
                };
                ctx.check_field_visible(struct_id, &fp.name);
                let field_ty = def.fields[idx].ty.clone();
                let sub = match &fp.pat {
                    Some(p) => lower_pattern(ctx, p, &field_ty),
                    // Shorthand `Foo { x }`: bind a local of the field's name.
                    None => {
                        let local = ctx.declare_bound(&fp.name, field_ty.clone(), pat.span);
                        HirPattern::Binding {
                            local,
                            ty: field_ty,
                        }
                    }
                };
                lowered.push((u32::try_from(idx).expect("field index overflow"), sub));
            }
            HirPattern::Struct {
                struct_id,
                fields: lowered,
            }
        }
        AstPatternKind::Slice(items) => {
            let scrut_ty = &crate::lower::infer::zonk(ctx, scrut_ty);
            if let HirTy::Sum(vs) = scrut_ty {
                let mut seqs = vs
                    .iter()
                    .enumerate()
                    .filter(|(_, v)| seq_len(ctx, v).is_some());
                if let (Some((i, member)), None) = (seqs.next(), seqs.next()) {
                    let member = member.clone();
                    return HirPattern::SumMember {
                        sum_ty: scrut_ty.clone(),
                        variant_index: u32::try_from(i).expect("variant overflow"),
                        sub: Box::new(lower_pattern_inner(ctx, pat, &member)),
                        payload_ty: member,
                    };
                }
            }
            match seq_len(ctx, scrut_ty) {
                Some(len) => slice_pattern(ctx, items, scrut_ty, len.fixed(), pat.span),
                None => mismatch(ctx, "a slice pattern `[..]`".to_owned()),
            }
        }
        AstPatternKind::Rest(_) => {
            ctx.emit_error(
                Code::REST_PATTERN,
                "a rest pattern `..` is only allowed in a slice pattern `[a, ..rest]` or a \
                 tuple destructuring `let`"
                    .to_owned(),
            );
            HirPattern::Wildcard
        }
    }
}

/// The length of a value a slice pattern matches.
#[derive(Clone, Copy)]
pub(crate) enum SeqLen {
    /// A `List`, `Slice` or `SliceMut`: known at run time.
    Dynamic,
    /// A `[T; n]`.
    Fixed(u32),
}

impl SeqLen {
    pub(crate) const fn fixed(self) -> Option<u32> {
        match self {
            Self::Dynamic => None,
            Self::Fixed(n) => Some(n),
        }
    }
}

/// What a slice pattern can match, if `ty` is one.
pub(crate) fn seq_len(ctx: &LowerCtx<'_>, ty: &HirTy) -> Option<SeqLen> {
    let seq = [
        crate::lower::expr::LIST,
        crate::lower::expr::SLICE,
        crate::lower::expr::SLICE_MUT,
    ];
    match ty {
        HirTy::Array(_, n) => Some(SeqLen::Fixed(*n)),
        HirTy::Struct(id) => seq
            .contains(&enum_base_name(&ctx.struct_def(*id).name))
            .then_some(SeqLen::Dynamic),
        _ => None,
    }
}

/// `[a, ..rest, z]` on `ty`: the items read through `whole[i]`, the rest
/// through `whole[a:b]`, so they are what indexing and slicing give.
fn slice_pattern(
    ctx: &mut LowerCtx<'_>,
    items: &[AstPattern],
    ty: &HirTy,
    fixed: Option<u32>,
    span: nabic_diagnostic::Span,
) -> HirPattern {
    let rests: Vec<usize> = items
        .iter()
        .enumerate()
        .filter(|(_, p)| matches!(p.kind, AstPatternKind::Rest(_)))
        .map(|(i, _)| i)
        .collect();
    if rests.len() > 1 {
        ctx.emit_error_at(
            Code::REST_PATTERN,
            items[rests[1]].span,
            "a slice pattern has at most one `..`: `[first, .., last]`".to_owned(),
        );
        return HirPattern::Wildcard;
    }
    let split = rests.first().copied();
    let (head, tail) = match split {
        Some(r) => (&items[..r], &items[r + 1..]),
        None => (items, &items[items.len()..]),
    };
    let n = head.len() + tail.len();
    if let Some(len) = fixed {
        let fits = if split.is_some() {
            n <= len as usize
        } else {
            n == len as usize
        };
        if !fits {
            ctx.emit_error_at(
                Code::PATTERN_ARITY,
                span,
                format!(
                    "this slice pattern has {n} element(s), but the array has {len}; \
                     write {len} patterns, or add `..` for the rest"
                ),
            );
            return HirPattern::Wildcard;
        }
    }
    let usize_ty = HirTy::Prim(PrimTy::Usize);
    let whole = ctx.declare(None, ty.clone());
    let len = ctx.declare(None, usize_ty.clone());
    let at = |v: usize| HirExpr {
        kind: HirExprKind::Const(ConstVal::UInt {
            value: v as u128,
            ty: PrimTy::Usize,
        }),
        ty: usize_ty.clone(),
        span,
    };
    let from_end = |v: usize| HirExpr {
        kind: HirExprKind::PrimBinOp {
            op: PrimOp::Sub,
            lhs: Box::new(hir_local(len, &usize_ty, span)),
            rhs: Box::new(at(v)),
        },
        ty: usize_ty.clone(),
        span,
    };
    let len_expr = match fixed {
        Some(k) => at(k as usize),
        None => {
            crate::lower::expr::method_call_on(ctx, hir_local(whole, ty, span), "len", &[], span)
        }
    };
    let read = |ctx: &mut LowerCtx<'_>, i: HirExpr, p: &AstPattern| {
        let target = crate::lower::expr::splice(ctx, hir_local(whole, ty, span));
        let i = crate::lower::expr::splice(ctx, i);
        let e = lower_expr(
            ctx,
            &Expr {
                kind: ExprKind::Index(Box::new(target), Box::new(i)),
                span,
            },
        );
        let sub = lower_pattern(ctx, p, &e.ty);
        (e, sub)
    };
    let head: Vec<(HirExpr, HirPattern)> = head
        .iter()
        .enumerate()
        .map(|(i, p)| read(ctx, at(i), p))
        .collect();
    let rest = split.map(|r| {
        let AstPatternKind::Rest(Some(name)) = &items[r].kind else {
            return None;
        };
        let target = crate::lower::expr::splice(ctx, hir_local(whole, ty, span));
        let start = crate::lower::expr::splice(ctx, at(head.len()));
        let end = crate::lower::expr::splice(ctx, from_end(tail.len()));
        let view = lower_expr(
            ctx,
            &Expr {
                kind: ExprKind::Slice {
                    target: Box::new(target),
                    start: Some(Box::new(start)),
                    end: Some(Box::new(end)),
                    step: None,
                },
                span,
            },
        );
        let local = ctx.declare_bound(name, view.ty.clone(), items[r].span);
        let ty = view.ty.clone();
        Some((view, HirPattern::Binding { local, ty }))
    });
    let tail: Vec<(HirExpr, HirPattern)> = tail
        .iter()
        .enumerate()
        .map(|(j, p)| read(ctx, from_end(tail.len() - j), p))
        .collect();
    HirPattern::Slice(Box::new(crate::SlicePattern {
        whole,
        len,
        len_expr,
        head,
        rest,
        tail,
        fixed,
    }))
}

/// `Pair(x, 4)`: a tuple struct's positional fields.
fn tuple_struct_pattern(
    ctx: &mut LowerCtx<'_>,
    segs: &[&str],
    args: &[AstPattern],
    scrut_ty: &HirTy,
) -> HirPattern {
    let full = ctx.canon_type(segs);
    let HirTy::Struct(sid) = scrut_ty else {
        ctx.emit_error(
            Code::UNRESOLVED_VARIANT,
            format!("cannot find enum variant `{}`", segs.join("::")),
        );
        ctx.help_last(ctx.value_help(segs));
        // Rust's `Some(x)` on a `T | none`: a type-test pattern.
        if let (["Some"], [arg], HirTy::Sum(vs)) = (segs, args, scrut_ty) {
            let inner: Vec<String> = vs
                .iter()
                .filter(|v| **v != HirTy::None)
                .map(|v| ctx.ty_name(v))
                .collect();
            if let (Some(x), true) = (ctx.src(arg.span), vs.contains(&HirTy::None)) {
                ctx.help_last(Some(format!(
                    "Nabi has no `Some`: match on the type; write `{x}: {} => ..` and `none => ..`",
                    inner.join(" | ")
                )));
            }
        }
        return HirPattern::Wildcard;
    };
    let def = ctx.struct_def(*sid);
    let tuple_form = def
        .fields
        .iter()
        .enumerate()
        .all(|(i, f)| f.name == i.to_string());
    if full.as_deref() != Some(enum_base_name(&def.name)) || !tuple_form {
        ctx.emit_error(
            Code::PATTERN_TYPE,
            format!(
                "pattern `{}(..)` can't match a value of type `{}`",
                segs.join("::"),
                ctx.ty_name(scrut_ty)
            ),
        );
        return HirPattern::Wildcard;
    }
    if args.len() != def.fields.len() {
        ctx.emit_error(
            Code::PATTERN_ARITY,
            format!(
                "pattern `{}` has {} field(s), but the struct has {}",
                segs.join("::"),
                args.len(),
                def.fields.len()
            ),
        );
        return HirPattern::Wildcard;
    }
    let fields = args
        .iter()
        .zip(&def.fields)
        .enumerate()
        .map(|(i, (p, f))| {
            // `_` reads nothing; any other sub-pattern needs the field visible.
            if !matches!(p.kind, AstPatternKind::Wildcard) {
                ctx.check_field_visible(*sid, &f.name);
            }
            (
                u32::try_from(i).expect("field index overflow"),
                lower_pattern(ctx, p, &f.ty),
            )
        })
        .collect();
    HirPattern::Struct {
        struct_id: *sid,
        fields,
    }
}

/// Names bound by a pattern (sorted, for or-pattern consistency).
pub(crate) fn pattern_bound_names(pat: &AstPattern) -> std::collections::BTreeSet<String> {
    struct Names(std::collections::BTreeSet<String>);
    impl crate::walk::AstVisitor for Names {
        fn bind(&mut self, name: &str) {
            self.0.insert(name.to_owned());
        }
    }
    let mut n = Names(std::collections::BTreeSet::new());
    match &pat.kind {
        // Alternatives bind the same names; the first one
        // stands for the whole or-pattern.
        AstPatternKind::Or(alts) => {
            if let Some(first) = alts.first() {
                crate::walk::walk_pat(&mut n, first);
            }
        }
        _ => crate::walk::walk_pat(&mut n, pat),
    }
    n.0
}

/// A pattern literal as a `ConstVal` and its comparison type; `None` if it
/// doesn't fit the scrutinee (the caller reports).
pub(crate) fn literal_to_const(
    ctx: &LowerCtx<'_>,
    lit: &AstLit,
    scrut_ty: &HirTy,
) -> Option<(ConstVal, PrimTy)> {
    let HirTy::Prim(sp) = scrut_ty else {
        return None;
    };
    match lit {
        AstLit::Int(text) => {
            // Bare literals take the scrutinee's type.
            let (value, mut ty) = parse_int_literal(text)?;
            if !has_int_suffix(text) {
                ty = *sp;
            }
            // A `char` is no integer: `0` on `i32 | char` is the `i32`.
            if ty != *sp || !sp.is_int() || *sp == PrimTy::Char {
                return None;
            }
            if !int_fits(value, ty) {
                ctx.emit_error(
                    Code::LITERAL_OUT_OF_RANGE,
                    format!("literal `{text}` out of range for `{}`", ty.name()),
                );
            }
            Some((ConstVal::Int { value, ty }, ty))
        }
        AstLit::Bool(b) if *sp == PrimTy::Bool => Some((ConstVal::Bool(*b), PrimTy::Bool)),
        AstLit::Char(s) if *sp == PrimTy::Char => {
            parse_char_literal(s).map(|c| (ConstVal::Char(c), PrimTy::Char))
        }
        // An ASCII `'a'` against a `c_char` scrutinee takes that type.
        AstLit::Char(s) if *sp == PrimTy::CChar => parse_char_literal(s)
            .filter(|c| *c < 0x80)
            .map(|c| (ConstVal::Char(c), PrimTy::CChar)),
        AstLit::CChar(s) if *sp == PrimTy::CChar => {
            parse_char_literal(s).map(|c| (ConstVal::Char(c), PrimTy::CChar))
        }
        // String patterns compare contents (codegen calls `nabi_str_cmp`).
        AstLit::Str(s) if *sp == PrimTy::Str => Some((
            ConstVal::Str(crate::lower::expr::decode_str_escapes(s)),
            PrimTy::Str,
        )),
        AstLit::RawStr(s) if *sp == PrimTy::Str => Some((ConstVal::Str(s.clone()), PrimTy::Str)),
        // Matching is `==`: `0.0` matches `-0.0`, nothing
        // matches NaN. Bare literals take the scrutinee's type.
        AstLit::Float(text) if sp.is_float() => {
            let suffixed = crate::lower::expr::has_float_suffix(text);
            let text = if suffixed {
                text.clone()
            } else {
                format!("{text}{}", sp.name())
            };
            let (bits, ty) = crate::lower::expr::parse_float_literal(&text)?;
            (ty == *sp).then_some((ConstVal::Float { bits, ty }, ty))
        }
        _ => None,
    }
}

/// Does `v` fit in primitive integer type `p` on the target?
pub(crate) fn int_fits(v: i128, p: PrimTy) -> bool {
    let bits = p.bit_width(crate::lower::consteval::pointer_width());
    if bits == 0 || bits >= 128 {
        return true;
    }
    if p.is_signed_int() {
        let lim = 1i128 << (bits - 1);
        (-lim..lim).contains(&v)
    } else {
        (0..(1i128 << bits)).contains(&v)
    }
}

/// Reject binding pattern `name` when it looks meant as a value: it
/// names a const or static (any case), or is capitalised and close to a
/// variant of the scrutinee's enum. Either way it would bind every value.
fn binds_const(ctx: &LowerCtx<'_>, name: &str, scrut_ty: &HirTy, at: nabic_diagnostic::Span) {
    let segs = [name];
    let what = if ctx.resolve_in(&segs, false).is_some() {
        "const"
    } else if ctx.resolve_in(&segs, true).is_some() {
        "static"
    } else {
        if !name.starts_with(|c: char| c.is_uppercase()) {
            return;
        }
        let HirTy::Enum(id) = scrut_ty else {
            return;
        };
        let def = ctx.enum_def(*id);
        let Some(v) = def
            .variants
            .iter()
            .map(|v| (nabic_core::suggest::edit_distance(&v.name, name), v))
            .filter(|(d, v)| *d <= 2 && *d < v.name.chars().count())
            .min_by_key(|(d, _)| *d)
            .map(|(_, v)| v)
        else {
            return;
        };
        let base = enum_base_name(&def.name);
        let rest = if v.arity == 0 { "" } else { "(..)" };
        ctx.emit_error_help(
            Code::BINDING_SHADOWS_VARIANT,
            at,
            format!("`{name}` here would be a new variable that matches every value"),
            format!("did you mean the variant `{base}::{}{rest}`?", v.name),
        );
        // Only a unit variant is a drop-in replacement for the name.
        if v.arity == 0 && ctx.src(at) == Some(name) {
            let text = format!("{base}::{}", v.name);
            ctx.fix_last(at, text.clone(), format!("write `{text}`"));
        }
        return;
    };
    ctx.emit_error_help(
        Code::BINDING_SHADOWS_VARIANT,
        at,
        format!(
            "`{name}` here would be a new variable that matches every value, not the {what} \
             `{name}`"
        ),
        format!("to compare with the {what}, write `x if x == {name}`"),
    );
}

/// The variant of enum `scrut_ty` named `name`: its enum, index, and the
/// enum's name as a pattern writes it.
fn variant_named(
    ctx: &LowerCtx<'_>,
    scrut_ty: &HirTy,
    name: &str,
) -> Option<(EnumId, usize, String)> {
    let HirTy::Enum(id) = scrut_ty else {
        return None;
    };
    let (id, idx) = crate::lower::expr::target_variant(ctx, name, &HirTy::Enum(*id))?;
    Some((id, idx, enum_base_name(&ctx.enum_def(id).name).to_owned()))
}
