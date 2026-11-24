//! Desugarings: closures, `?` / `??` / `?.`, and `for` loops.

mod closure;
mod loops;

pub(crate) use crate::lower::desugar::closure::*;
pub(crate) use crate::lower::desugar::loops::*;

#[allow(clippy::wildcard_imports)]
use crate::lower::*;
use nabic_diagnostic::Code;

/// `T | none`-shaped sum: index of `none` plus the non-none variants.
pub(crate) fn optional_parts(ty: &HirTy) -> Option<(usize, Vec<(usize, HirTy)>)> {
    let HirTy::Sum(variants) = ty else {
        return None;
    };
    let none_idx = variants.iter().position(HirTy::is_none)?;
    let some: Vec<(usize, HirTy)> = variants
        .iter()
        .enumerate()
        .filter(|(_, v)| !v.is_none())
        .map(|(i, v)| (i, v.clone()))
        .collect();
    Some((none_idx, some))
}

/// The value type left after removing `none`: the single payload, or a
/// sum of the remaining variants.
pub(crate) fn without_none(some: &[(usize, HirTy)]) -> HirTy {
    match some {
        [(_, t)] => t.clone(),
        many => HirTy::sum(many.iter().map(|(_, t)| t.clone())),
    }
}

/// `match scrut { v: T_k => wrap(v), ..., none => none_body }` with one
/// arm per non-none variant; each payload is re-wrapped into `result_ty`.
#[allow(clippy::too_many_arguments)] // independent match pieces
pub(crate) fn optional_match(
    ctx: &mut LowerCtx<'_>,
    scrut: HirExpr,
    none_idx: usize,
    some: &[(usize, HirTy)],
    some_body: &mut dyn FnMut(&mut LowerCtx<'_>, HirExpr) -> HirExpr,
    none_body: HirExpr,
    result_ty: HirTy,
    span: nabic_diagnostic::Span,
) -> HirExpr {
    let sum_ty = scrut.ty.clone();
    let mut arms = Vec::with_capacity(some.len() + 1);
    for (idx, payload_ty) in some {
        let bind_local = ctx.declare(None, payload_ty.clone());
        let value = HirExpr {
            kind: HirExprKind::Local(bind_local),
            ty: payload_ty.clone(),
            span,
        };
        arms.push(HirMatchArm {
            pat: HirPattern::SumVariant {
                sum_ty: sum_ty.clone(),
                variant_index: u32::try_from(*idx).expect("variant overflow"),
                binder: Some(bind_local),
                payload_ty: Some(payload_ty.clone()),
            },
            guard: None,
            body: some_body(ctx, value),
        });
    }
    arms.push(HirMatchArm {
        pat: HirPattern::SumVariant {
            sum_ty,
            variant_index: u32::try_from(none_idx).expect("variant overflow"),
            binder: None,
            payload_ty: None,
        },
        guard: None,
        body: none_body,
    });
    HirExpr {
        kind: HirExprKind::Match {
            scrutinee: Box::new(scrut),
            arms,
        },
        ty: result_ty,
        span,
    }
}

pub(crate) fn sum_none(sum_ty: &HirTy, span: nabic_diagnostic::Span) -> HirExpr {
    wrap_into_sum(
        sum_ty,
        HirExpr {
            kind: HirExprKind::Const(ConstVal::None),
            ty: HirTy::None,
            span,
        },
    )
}

/// `x?.tail` -> `match x { v: T => v.tail, none => none }`, typed
/// `tail_ty | none` (`?.` chains compose).
pub(crate) fn desugar_question_dot(
    ctx: &mut LowerCtx<'_>,
    recv: HirExpr,
    tails: &[nabic_ast::AccessTail],
    span: nabic_diagnostic::Span,
) -> HirExpr {
    if recv.ty == HirTy::Unknown {
        return recv;
    }
    let Some((none_idx, some)) = optional_parts(&recv.ty) else {
        // `?.` on a non-optional receiver is an error.
        return ctx.error_expr(
            Code::OPTIONAL_OPERATOR,
            span,
            format!(
                "`?.` needs an optional (`T | none`) receiver, found `{}` - it is not \
                 optional, so use `.`",
                ctx.ty_name(&recv.ty)
            ),
        );
    };
    if some.len() != 1 {
        return ctx.error_expr(
            Code::OPTIONAL_OPERATOR,
            span,
            format!(
                "`?.` needs a `T | none` receiver with a single payload type, found `{}`",
                ctx.ty_name(&recv.ty)
            ),
        );
    }
    // Lower the tail once against a placeholder local to learn its type.
    let payload_ty = some[0].1.clone();
    let bind_local = ctx.declare(None, payload_ty.clone());
    let unwrapped = HirExpr {
        kind: HirExprKind::Local(bind_local),
        ty: payload_ty.clone(),
        span,
    };
    let mut inner = unwrapped;
    for tail in tails {
        inner = apply_access_tail(ctx, inner, tail, span);
        if inner.ty == HirTy::Unknown {
            return inner;
        }
    }
    let inner_optional = optional_parts(&inner.ty).is_some();
    let result_ty = if inner_optional {
        inner.ty.clone()
    } else {
        HirTy::sum([inner.ty.clone(), HirTy::None])
    };
    let some_body = if inner_optional {
        inner
    } else {
        wrap_into_sum(&result_ty, inner)
    };
    let none_body = sum_none(&result_ty, span);
    HirExpr {
        kind: HirExprKind::Match {
            scrutinee: Box::new(recv.clone()),
            arms: vec![
                HirMatchArm {
                    pat: HirPattern::SumVariant {
                        sum_ty: recv.ty.clone(),
                        variant_index: u32::try_from(some[0].0).expect("variant overflow"),
                        binder: Some(bind_local),
                        payload_ty: Some(payload_ty),
                    },
                    guard: None,
                    body: some_body,
                },
                HirMatchArm {
                    pat: HirPattern::SumVariant {
                        sum_ty: recv.ty,
                        variant_index: u32::try_from(none_idx).expect("variant overflow"),
                        binder: None,
                        payload_ty: None,
                    },
                    guard: None,
                    body: none_body,
                },
            ],
        },
        ty: result_ty,
        span,
    }
}

/// Apply an `AccessTail` to an already-lowered HIR receiver. Supports
/// the same forms `?.` does: field, tuple field, and method call.
pub(crate) fn apply_access_tail(
    ctx: &mut LowerCtx<'_>,
    recv: HirExpr,
    tail: &nabic_ast::AccessTail,
    span: nabic_diagnostic::Span,
) -> HirExpr {
    match tail {
        nabic_ast::AccessTail::Field(name) => field_access(ctx, recv, name, span),
        nabic_ast::AccessTail::TupleField(idx) => match recv.ty.clone() {
            HirTy::Tuple(parts) => match parts.get(*idx as usize) {
                Some(field_ty) => HirExpr {
                    kind: HirExprKind::TupleField {
                        target: Box::new(recv),
                        index: *idx,
                        field_ty: field_ty.clone(),
                    },
                    ty: field_ty.clone(),
                    span,
                },
                None => ctx.error_expr(
                    Code::NO_FIELD,
                    span,
                    format!("tuple index {idx} out of range for a {}-tuple", parts.len()),
                ),
            },
            other => ctx.error_expr(
                Code::NO_FIELD,
                span,
                format!("no field `{idx}` on type `{}`", ctx.ty_name(&other)),
            ),
        },
        nabic_ast::AccessTail::Method { method, args, .. } => {
            let call = method_call_on(ctx, recv, method, args, span);
            unpack_call(ctx, call)
        }
        nabic_ast::AccessTail::Index(i) => {
            let target = crate::lower::expr::splice(ctx, recv);
            let e = nabic_ast::Expr {
                kind: ExprKind::Index(Box::new(target), i.clone()),
                span,
            };
            lower_expr(ctx, &e)
        }
    }
}

/// `expr?`: unwrap `T | none` or return `none` from the enclosing
/// function (whose return must hold `none`). Several payload members keep
/// the rest of the sum.
pub(crate) fn desugar_try_q(
    ctx: &mut LowerCtx<'_>,
    inner: HirExpr,
    span: nabic_diagnostic::Span,
) -> HirExpr {
    if inner.ty == HirTy::Unknown {
        return inner;
    }
    let Some((none_idx, some)) = optional_parts(&inner.ty) else {
        return ctx.error_expr(
            Code::OPTIONAL_OPERATOR,
            span,
            format!(
                "`?` only works on `T | none`, found `{}` - to propagate errors use `throws` \
                 and `try`",
                ctx.ty_name(&inner.ty)
            ),
        );
    };
    let ret_ty = ctx.fn_ret_ty.clone();
    if optional_parts(&ret_ty).is_none() {
        let e = ctx.error_expr(
            Code::OPTIONAL_OPERATOR,
            span,
            format!(
                "`?` propagates `none`, but the enclosing function returns `{}` - its \
                 return type must include `none`",
                ctx.ty_name(&ret_ty)
            ),
        );
        // A script's top level (or `main`) cannot return `none`: unwrap here.
        let x = crate::lower::suggest::short_src(ctx, inner.span).unwrap_or("x");
        ctx.help_last(Some(format!(
            "or handle `none` here: `let v = {x} else {{ return; }};`, or `{x} ?? default`"
        )));
        return e;
    }
    let value_ty = without_none(&some);
    let ret = lower_return(ctx, Some(sum_none(&ret_ty, span)), span);
    let none_body = HirExpr {
        kind: HirExprKind::Block(Box::new(HirBlock {
            stmts: vec![ret],
            trailing: None,
            label: None,
        })),
        ty: value_ty.clone(),
        span,
    };
    let vt = value_ty.clone();
    optional_match(
        ctx,
        inner,
        none_idx,
        &some,
        &mut |_, v| wrap_into_sum(&vt, v),
        none_body,
        value_ty,
        span,
    )
}

/// Desugar `lhs ?? rhs` to `match lhs { v: T => v, none => rhs }`; the
/// default has the payload type, or both go into the receiving slot.
pub(crate) fn desugar_nullish_coalesce(
    ctx: &mut LowerCtx<'_>,
    lhs: HirExpr,
    rhs: &Expr,
    span: nabic_diagnostic::Span,
) -> HirExpr {
    let Some((none_idx, some)) = optional_parts(&lhs.ty) else {
        // An optional local narrowed by `x != none` / `x is none` keeps
        // taking `??`, as before narrowing: it is the value itself.
        if let HirExprKind::Local(l) = lhs.kind {
            if ctx
                .narrowed
                .get(&l)
                .is_some_and(|(_, t)| optional_parts(t).is_some())
            {
                let ty = lhs.ty.clone();
                let rhs_h = lower_expected(ctx, rhs, ty.clone());
                let _ = coerce(ctx, &ty, rhs_h, &|| "the `??` default".to_owned());
                return lhs;
            }
        }
        let rhs_h = lower_expected(ctx, rhs, HirTy::Unknown);
        if lhs.ty == HirTy::Unknown || rhs_h.ty == HirTy::Unknown {
            return poison(span);
        }
        return ctx.error_expr(
            Code::OPTIONAL_OPERATOR,
            span,
            format!(
                "`??` needs an optional left operand (`T | none`), found `{}`",
                ctx.ty_name(&lhs.ty)
            ),
        );
    };
    let mut value_ty = without_none(&some);
    let rhs_h = lower_expected(ctx, rhs, value_ty.clone());
    // `a ?? b` with `b: T | none` too is a fallback chain typed `T | none`.
    // Only that case; other sums still need `T`.
    if rhs_h.ty == lhs.ty {
        value_ty = lhs.ty.clone();
    } else if rhs_h.ty != value_ty && rhs_h.ty != HirTy::Unknown {
        // Branches of different types take the receiving slot's type,
        // never a widened sum (`let r: i32 | str = a ?? "d"`).
        if let Some(slot) = ctx
            .current_expected()
            .filter(|t| matches!(t, HirTy::Sum(_)))
        {
            value_ty = slot.clone();
        }
    }
    let rhs_h = coerce(ctx, &value_ty, rhs_h, &|| "the `??` default".to_owned());
    let vt = value_ty.clone();
    optional_match(
        ctx,
        lhs,
        none_idx,
        &some,
        &mut |_, v| wrap_into_sum(&vt, v),
        rhs_h,
        value_ty,
        span,
    )
}

/// Bind an irrefutable `for` / destructuring pattern to `value`:
/// a name, `_`, `name @ pat`, or (nested) tuples / structs of those.
pub(crate) fn bind_irrefutable(
    ctx: &mut LowerCtx<'_>,
    pat: &AstPattern,
    value: HirExpr,
    stmts: &mut Vec<HirStmt>,
) {
    let span = value.span;
    match (&pat.kind, value.ty.clone()) {
        (AstPatternKind::Binding { name, sub: None }, ty) => {
            if name == "_" {
                return;
            }
            stmts.push(ctx.declare_var(name, ty, value, pat.span));
        }
        // `whole @ (a, b)`: bind the whole value, then destructure it.
        (
            AstPatternKind::Binding {
                name,
                sub: Some(sub),
            },
            ty,
        ) => {
            stmts.push(ctx.declare_var(name, ty, value, pat.span));
            if let Some(local) = ctx.lookup(name) {
                let read = ctx.read_local(local, span);
                bind_irrefutable(ctx, sub, read, stmts);
            }
        }
        (AstPatternKind::Tuple(pats), HirTy::Tuple(tys)) if pats.len() == tys.len() => {
            let tmp = ctx.declare(None, value.ty.clone());
            let tuple_ty = value.ty.clone();
            stmts.push(HirStmt::Let {
                local: tmp,
                init: value,
            });
            for (i, (p, t)) in pats.iter().zip(tys).enumerate() {
                let read = HirExpr {
                    kind: HirExprKind::TupleField {
                        target: Box::new(hir_local(tmp, &tuple_ty, span)),
                        index: u32::try_from(i).expect("tuple index overflow"),
                        field_ty: t.clone(),
                    },
                    ty: t.clone(),
                    span,
                };
                bind_irrefutable(ctx, p, read, stmts);
            }
        }
        // `Point { x, y: py, .. }` / tuple struct `Pair(a, b)`.
        (
            AstPatternKind::Struct { path, .. } | AstPatternKind::EnumVariant { path, .. },
            HirTy::Struct(sid),
        ) if ctx
            .canon_type(
                &path
                    .segments
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>(),
            )
            .as_deref()
            == Some(enum_base_name(&ctx.struct_def(sid).name)) =>
        {
            crate::lower::pattern::struct_pattern_rest(ctx, pat, &value.ty);
            let expanded;
            let parts: Vec<(String, Option<&AstPattern>)> = match &pat.kind {
                AstPatternKind::Struct { fields, .. } => fields
                    .iter()
                    .map(|f| (f.name.clone(), f.pat.as_ref()))
                    .collect(),
                AstPatternKind::EnumVariant { args, .. } => {
                    expanded =
                        crate::lower::pattern::expand_rest(args, ctx.struct_def(sid).fields.len());
                    expanded
                        .as_deref()
                        .unwrap_or(args)
                        .iter()
                        .enumerate()
                        .map(|(i, a)| (i.to_string(), Some(a)))
                        .collect()
                }
                _ => Vec::new(),
            };
            let tmp = ctx.declare(None, value.ty.clone());
            let struct_ty = value.ty.clone();
            stmts.push(HirStmt::Let {
                local: tmp,
                init: value,
            });
            let def = ctx.struct_def(sid);
            for (fname, fpat) in parts {
                let Some(idx) = def.fields.iter().position(|f| f.name == fname) else {
                    ctx.no_field(sid, &fname, span);
                    continue;
                };
                if !fpat.is_some_and(|p| matches!(p.kind, AstPatternKind::Wildcard)) {
                    ctx.check_field_visible(sid, &fname);
                }
                let field_ty = def.fields[idx].ty.clone();
                let read = HirExpr {
                    ty: field_ty.clone(),
                    span,
                    kind: HirExprKind::FieldRead {
                        target: Box::new(hir_local(tmp, &struct_ty, span)),
                        struct_id: sid,
                        field_index: u32::try_from(idx).expect("field idx overflow"),
                        field_ty: field_ty.clone(),
                    },
                };
                match fpat {
                    Some(p) => bind_irrefutable(ctx, p, read, stmts),
                    None => stmts.push(ctx.declare_var(&fname, field_ty, read, pat.span)),
                }
            }
        }
        // `let [a, b, ..rest] = arr;`: a fixed array has one length.
        (AstPatternKind::Slice(items), HirTy::Array(_, n)) => {
            let rest = items
                .iter()
                .position(|p| matches!(p.kind, AstPatternKind::Rest(_)));
            let fixed = items.len() - usize::from(rest.is_some());
            let fits = if rest.is_some() {
                fixed <= n as usize
            } else {
                fixed == n as usize
            };
            if !fits {
                ctx.emit_error_at(
                    Code::PATTERN_ARITY,
                    pat.span,
                    format!(
                        "this slice pattern has {fixed} element(s), but the array has {n}; \
                         write {n} patterns, or add `..` for the rest"
                    ),
                );
                for name in crate::lower::pattern::pattern_bound_names(pat) {
                    ctx.declare(Some(name), HirTy::Unknown);
                }
                return;
            }
            let arr_ty = value.ty.clone();
            let tmp = ctx.declare(None, arr_ty.clone());
            stmts.push(HirStmt::Let {
                local: tmp,
                init: value,
            });
            let tail = rest.map_or(0, |r| items.len() - r - 1);
            let at = |ctx: &mut LowerCtx<'_>, v: usize| {
                let c = HirExpr {
                    kind: HirExprKind::Const(ConstVal::UInt {
                        value: v as u128,
                        ty: PrimTy::Usize,
                    }),
                    ty: HirTy::Prim(PrimTy::Usize),
                    span,
                };
                crate::lower::expr::splice(ctx, c)
            };
            for (i, p) in items.iter().enumerate() {
                let target = crate::lower::expr::splice(ctx, hir_local(tmp, &arr_ty, span));
                let read = match (&p.kind, rest) {
                    (AstPatternKind::Rest(None), _) => continue,
                    (AstPatternKind::Rest(Some(name)), _) => {
                        let (a, b) = (at(ctx, i), at(ctx, n as usize - tail));
                        let view = lower_expr(
                            ctx,
                            &nabic_ast::Expr {
                                kind: ExprKind::Slice {
                                    target: Box::new(target),
                                    start: Some(Box::new(a)),
                                    end: Some(Box::new(b)),
                                    step: None,
                                },
                                span,
                            },
                        );
                        stmts.push(ctx.declare_var(name, view.ty.clone(), view, p.span));
                        continue;
                    }
                    (_, Some(r)) if i > r => n as usize - (items.len() - i),
                    _ => i,
                };
                let i = at(ctx, read);
                let e = lower_expr(
                    ctx,
                    &nabic_ast::Expr {
                        kind: ExprKind::Index(Box::new(target), Box::new(i)),
                        span,
                    },
                );
                bind_irrefutable(ctx, p, e, stmts);
            }
        }
        (AstPatternKind::Slice(_), ty) if crate::lower::pattern::seq_len(ctx, &ty).is_some() => {
            ctx.emit_error(
                Code::PATTERN_TYPE,
                format!(
                    "a slice pattern can't destructure a `{}` where it must always match: its \
                     length is known only at run time",
                    ctx.ty_name(&ty)
                ),
            );
            ctx.help_last(Some(
                "write `let [first, ..rest] = xs else { return; };`, or `match` on the lengths"
                    .to_owned(),
            ));
            // Its names, poisoned, so their uses report nothing more.
            for name in crate::lower::pattern::pattern_bound_names(pat) {
                ctx.declare(Some(name), HirTy::Unknown);
            }
        }
        (AstPatternKind::Wildcard, _) | (_, HirTy::Unknown) => {}
        // `let (a: i32, b: str) = ..`: an annotated element is a slot of
        // that type, not a runtime test.
        (AstPatternKind::TypeCheck { binder, ty }, have) => {
            let want = ctx.resolve_ast_ty(ty);
            let name = binder.clone().unwrap_or_else(|| "_".to_owned());
            // `for x: i32 in xs` over `i32 | none`: a test, which a pattern
            // that must always match cannot be.
            if matches!(&have, HirTy::Sum(vs) if vs.contains(&want)) {
                let (t, h) = (ctx.ty_name(&want), ctx.ty_name(&have));
                ctx.emit_error_at(
                    Code::PATTERN_TYPE,
                    pat.span,
                    format!(
                        "the pattern `{name}: {t}` can't always match a `{h}`, and a `for` or \
                         `let` pattern must"
                    ),
                );
                ctx.help_last(Some(format!(
                    "test each value instead: `for v in xs {{ if let {name}: {t} = v {{ .. }} }}`, \
                     or `let {name}: {t} = v else {{ .. }};`"
                )));
                if binder.is_some() {
                    ctx.declare(Some(name), HirTy::Unknown);
                }
                return;
            }
            let value = coerce(ctx, &want, value, &|| format!("the pattern `{name}`"));
            if binder.is_some() {
                stmts.push(ctx.declare_var(&name, want, value, pat.span));
            }
        }
        (_, ty) => {
            ctx.emit_error(
                Code::PATTERN_TYPE,
                format!(
                    "this pattern can't destructure a value of type `{}` here - it must be \
                     irrefutable: a name, `_`, `name @ pat`, or a tuple / struct of those",
                    ctx.ty_name(&ty)
                ),
            );
            // `let A(x) = e;`: a test, which `let .. else` makes (spec 3.16).
            let whole = nabic_diagnostic::Span {
                end: span.start,
                ..pat.span
            };
            let p = ctx
                .src(whole)
                .filter(|_| whole.file == span.file && whole.start < span.start)
                .map(|t| t.trim_end().trim_end_matches('=').trim_end());
            if let (Some(p), Some(v)) = (p, ctx.src(span)) {
                ctx.help_last(Some(format!(
                    "to test it, say what happens otherwise: `let {p} = {v} else {{ .. }};`, or \
                     `match`"
                )));
            }
        }
    }
}
