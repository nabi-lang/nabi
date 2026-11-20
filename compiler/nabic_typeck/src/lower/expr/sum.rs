//! Sum types: structural calls, order and `min` / `max`.

#[allow(clippy::wildcard_imports)]
use crate::lower::expr::*;

/// A trait a sum has when all its members do?
pub(crate) fn is_sum_trait(ctx: &LowerCtx<'_>, key: &str) -> bool {
    [
        "clone_trait",
        "debug_trait",
        "eq_trait",
        "hash_trait",
        "cmp_trait",
        "ord_trait",
    ]
    .into_iter()
    .any(|s| ctx.lang(s) == key)
        || [COPY_TRAIT, EQ_TRAIT].contains(&key)
}

/// `PartialOrd` or `Ord`?
pub fn is_order_trait(ctx: &LowerCtx<'_>, key: &str) -> bool {
    key == ctx.lang("cmp_trait") || key == ctx.lang("ord_trait")
}

/// The terms of a sum other than `none` (a nested layer counts once).
pub fn sum_value_terms(ty: &HirTy) -> usize {
    match ty {
        HirTy::Sum(vs) => vs.iter().filter(|v| !v.is_none()).count(),
        _ => 0,
    }
}

/// A sum with two value terms has no order. `what` is the operation
/// (`<`, `min`, ..).
pub(crate) fn sum_order_error(
    ctx: &LowerCtx<'_>,
    ty: &HirTy,
    what: &str,
    span: nabic_diagnostic::Span,
) -> HirExpr {
    let msg = format!(
        "`{what}` needs an order, and `{}` has none: its terms are different types",
        ctx.ty_name(ty)
    );
    ctx.emit_diag(
        nabic_diagnostic::NabiDiagnostic::error(Code::INVALID_OPERATOR, span, msg)
            .with_help(sum_order_help().to_owned()),
    );
    poison(span)
}

/// What to write instead of ordering a sum of two value types.
pub const fn sum_order_help() -> &'static str {
    "order by a key: `xs.sort_by_key(|v| ..)`, `xs.sort_by(|a, b| ..)`, \
     `iter.min_by_key(|v| ..)`, or `match` on the terms"
}

/// Their methods, dispatched on the variant by `sum_structural_call`.
pub(crate) const SUM_METHODS: &[&str] = &[
    "clone", "fmt", "eq", "ne", "hash", "lt", "gt", "le", "ge", "cmp",
];

/// `clone` / `Debug::fmt` / `eq` / `ne` / `hash` / `lt`.. / `cmp` on a sum:
/// a `match` calling the variant's method. Across variants `eq` is false
/// and `none` orders first; `none` clones, prints, compares and hashes as
/// itself. `lowered_rhs`: an operator's already lowered right operand.
pub(crate) fn sum_structural_call(
    ctx: &mut LowerCtx<'_>,
    recv: HirExpr,
    method: &str,
    args: &[CallArg],
    lowered_rhs: Option<HirExpr>,
    span: nabic_diagnostic::Span,
) -> HirExpr {
    let HirTy::Sum(variants) = recv.ty.clone() else {
        return recv;
    };
    let sum_ty = recv.ty.clone();
    let binary = matches!(method, "eq" | "ne" | "lt" | "gt" | "le" | "ge" | "cmp");
    let bool_c = |b: bool| HirExpr {
        kind: HirExprKind::Const(ConstVal::Bool(b)),
        ty: HirTy::Prim(PrimTy::Bool),
        span,
    };
    let ordering = |ctx: &mut LowerCtx<'_>, variant: &str| {
        let segments = ["", "core", "cmp", "Ordering", variant]
            .iter()
            .map(|n| nabic_ast::PathSegment {
                name: (*n).to_owned(),
                generics: Vec::new(),
            })
            .collect();
        lower_expr(
            ctx,
            &Expr {
                kind: ExprKind::Path(AstPath {
                    segments,
                    span,
                    origin: None,
                }),
                span,
            },
        )
    };
    let result_ty = match method {
        "clone" => sum_ty.clone(),
        "fmt" | "hash" => HirTy::Unit,
        "cmp" => ordering(ctx, "Equal").ty,
        _ => HirTy::Prim(PrimTy::Bool),
    };
    // The result when the two sides are different variants (`less`: the
    // left one comes first) or both `none`.
    let across = |ctx: &mut LowerCtx<'_>, less: bool| match method {
        "cmp" => ordering(ctx, if less { "Less" } else { "Greater" }),
        "lt" | "le" => bool_c(less),
        "gt" | "ge" => bool_c(!less),
        _ => bool_c(method == "ne"),
    };
    let same_none = |ctx: &mut LowerCtx<'_>| match method {
        "cmp" => ordering(ctx, "Equal"),
        _ => bool_c(matches!(method, "eq" | "le" | "ge")),
    };
    let mut stmts = Vec::new();
    let s = ctx.declare(None, sum_ty.clone());
    stmts.push(HirStmt::Let {
        local: s,
        init: recv,
    });
    let rhs = if binary {
        let v = if let Some(v) = lowered_rhs {
            v
        } else {
            let [CallArg::Positional(e)] = args else {
                discard_args(ctx, args);
                return ctx.error_expr(
                    Code::WRONG_ARGUMENT_COUNT,
                    span,
                    format!("`{method}` takes one argument"),
                );
            };
            lower_expected(ctx, e, sum_ty.clone())
        };
        let v = coerce(ctx, &sum_ty, v, &|| format!("the argument of `{method}`"));
        let r = ctx.declare(None, sum_ty.clone());
        stmts.push(HirStmt::Let { local: r, init: v });
        Some(r)
    } else {
        if method == "hash" && !matches!(args, [CallArg::Ref(_) | CallArg::Positional(_)]) {
            discard_args(ctx, args);
            return ctx.error_expr(
                Code::REF_ARGUMENT,
                span,
                "`hash` takes the hasher: `x.hash(ref h)`".to_owned(),
            );
        }
        if !matches!(method, "fmt" | "hash") {
            discard_args(ctx, args);
        }
        None
    };
    let path = |name: &str| Expr {
        kind: ExprKind::Path(AstPath {
            segments: vec![nabic_ast::PathSegment {
                name: name.to_owned(),
                generics: Vec::new(),
            }],
            span,
            origin: None,
        }),
        span,
    };
    let call = |m: &str, arg: Option<&str>| Expr {
        kind: ExprKind::MethodCall {
            receiver: Box::new(path("#a")),
            method: m.to_owned(),
            type_args: Vec::new(),
            args: arg
                .map(|a| CallArg::Positional(path(a)))
                .into_iter()
                .collect(),
            trailing_closure: None,
            origin: None,
            written: false,
        },
        span,
    };
    let arm_pat = |ctx: &mut LowerCtx<'_>, i: usize, name: &str| {
        let vt = &variants[i];
        let none = vt.is_none();
        let binder = (!none).then(|| ctx.declare(Some(name.to_owned()), vt.clone()));
        HirPattern::SumVariant {
            sum_ty: sum_ty.clone(),
            variant_index: u32::try_from(i).expect("variant overflow"),
            binder,
            payload_ty: (!none).then(|| vt.clone()),
        }
    };
    let mut arms = Vec::new();
    for (i, vt) in variants.iter().enumerate() {
        ctx.push_scope();
        let pat = arm_pat(ctx, i, "#a");
        let none = vt.is_none();
        let body = match (method, rhs) {
            ("clone", _) if none => hir_local(s, &sum_ty, span),
            ("clone", _) => {
                let v = lower_expr(ctx, &call("clone", None));
                wrap_into_sum(&sum_ty, v)
            }
            ("fmt", _) => {
                // `Debug::fmt(#a, ref f)`, or `none` written.
                let fmt_path = |names: &[&str]| Expr {
                    kind: ExprKind::Path(AstPath {
                        segments: names
                            .iter()
                            .map(|n| nabic_ast::PathSegment {
                                name: (*n).to_owned(),
                                generics: Vec::new(),
                            })
                            .collect(),
                        span,
                        origin: None,
                    }),
                    span,
                };
                let debug_fmt = format!("{}::fmt", ctx.lang("debug_trait"));
                let debug_fmt: Vec<&str> = debug_fmt.split("::").collect();
                let (callee, first) = if none {
                    let lit = Expr {
                        kind: ExprKind::Literal(AstLit::Str("none".to_owned())),
                        span,
                    };
                    (&["", "core", "fmt", "Formatter", "write_str"][..], lit)
                } else {
                    (&debug_fmt[..], path("#a"))
                };
                let mut call_args = args.to_vec();
                if none {
                    call_args.push(CallArg::Positional(first));
                } else {
                    call_args.insert(0, CallArg::Positional(first));
                }
                let c = Expr {
                    kind: ExprKind::Call {
                        callee: Box::new(fmt_path(callee)),
                        args: call_args,
                        trailing_closure: None,
                    },
                    span,
                };
                lower_expr(ctx, &c)
            }
            ("hash", _) => {
                // `h.write_usize(i)`, then the member's `hash(ref h)`.
                let [CallArg::Ref(h) | CallArg::Positional(h)] = args else {
                    unreachable!("checked above")
                };
                let tag = Expr {
                    kind: ExprKind::MethodCall {
                        receiver: Box::new(h.clone()),
                        method: "write_usize".to_owned(),
                        type_args: Vec::new(),
                        args: vec![CallArg::Positional(Expr {
                            kind: ExprKind::Literal(AstLit::Int(format!("{i}usize"))),
                            span,
                        })],
                        trailing_closure: None,
                        // `Hasher` is in scope where the trait lives.
                        origin: Some("core::hash".to_owned()),
                        written: false,
                    },
                    span,
                };
                let tag = lower_expr(ctx, &tag);
                let member = (!none).then(|| {
                    lower_expr(
                        ctx,
                        &Expr {
                            kind: ExprKind::MethodCall {
                                receiver: Box::new(path("#a")),
                                method: "hash".to_owned(),
                                type_args: Vec::new(),
                                args: args.to_vec(),
                                trailing_closure: None,
                                origin: None,
                                written: false,
                            },
                            span,
                        },
                    )
                });
                HirExpr {
                    kind: HirExprKind::Block(Box::new(HirBlock {
                        stmts: vec![HirStmt::Expr(tag)],
                        trailing: member,
                        label: None,
                    })),
                    ty: HirTy::Unit,
                    span,
                }
            }
            (_, Some(r)) => {
                // Same variant: compare payloads; else `none` first, then the variant
                // order (only `T | none` reaches here).
                let eq_like = matches!(method, "eq" | "ne");
                let mut inner = Vec::new();
                for (j, vj) in variants.iter().enumerate() {
                    if j != i && eq_like {
                        continue;
                    }
                    ctx.push_scope();
                    let inner_pat = arm_pat(ctx, j, "#b");
                    let body = if j != i {
                        across(ctx, none || (i < j && !vj.is_none()))
                    } else if none {
                        same_none(ctx)
                    } else {
                        lower_expr(ctx, &call(method, Some("#b")))
                    };
                    ctx.pop_scope();
                    inner.push(HirMatchArm {
                        pat: inner_pat,
                        guard: None,
                        body,
                    });
                }
                if eq_like {
                    inner.push(HirMatchArm {
                        pat: HirPattern::Wildcard,
                        guard: None,
                        body: across(ctx, false),
                    });
                }
                HirExpr {
                    kind: HirExprKind::Match {
                        scrutinee: Box::new(hir_local(r, &sum_ty, span)),
                        arms: inner,
                    },
                    ty: result_ty.clone(),
                    span,
                }
            }
            _ => lower_expr(ctx, &call(method, None)),
        };
        ctx.pop_scope();
        let body = coerce(ctx, &result_ty, body, &|| {
            format!("`{method}` of a sum value")
        });
        arms.push(HirMatchArm {
            pat,
            guard: None,
            body,
        });
    }
    HirExpr {
        kind: HirExprKind::Block(Box::new(HirBlock {
            stmts,
            trailing: Some(HirExpr {
                kind: HirExprKind::Match {
                    scrutinee: Box::new(hir_local(s, &sum_ty, span)),
                    arms,
                },
                ty: result_ty.clone(),
                span,
            }),
            label: None,
        })),
        ty: result_ty,
        span,
    }
}

/// `Ord`'s `min` / `max` / `clamp` on a sum, inlined from the trait's
/// default bodies (its `Ord` is compiler-dispatched).
pub(crate) fn sum_min_max(
    ctx: &mut LowerCtx<'_>,
    recv: HirExpr,
    method: &str,
    args: &[CallArg],
    span: nabic_diagnostic::Span,
) -> HirExpr {
    let ty = recv.ty.clone();
    let want = if method == "clamp" { 2 } else { 1 };
    if args.len() != want || args.iter().any(|a| !matches!(a, CallArg::Positional(_))) {
        discard_args(ctx, args);
        let (msg, _) = crate::lower::arity_msg(method, want, args.len(), None);
        return ctx.error_expr(Code::WRONG_ARGUMENT_COUNT, span, msg);
    }
    let mut stmts = Vec::new();
    let mut local = |ctx: &mut LowerCtx<'_>, init: HirExpr| {
        let l = ctx.declare(None, ty.clone());
        stmts.push(HirStmt::Let { local: l, init });
        l
    };
    let a = local(ctx, recv);
    let mut ls = Vec::new();
    for arg in args {
        let CallArg::Positional(e) = arg else {
            unreachable!("checked above")
        };
        let v = lower_expected(ctx, e, ty.clone());
        let v = coerce(ctx, &ty, v, &|| format!("the argument of `{method}`"));
        ls.push(local(ctx, v));
    }
    let get = |l: LocalId| hir_local(l, &ty, span);
    let test = |ctx: &mut LowerCtx<'_>, op: &str, l: LocalId, r: LocalId| {
        sum_structural_call(ctx, get(l), op, &[], Some(get(r)), span)
    };
    let pick = |cond: HirExpr, then: HirExpr, other: HirExpr| HirExpr {
        kind: HirExprKind::If {
            cond: Box::new(cond),
            then_block: Box::new(HirBlock {
                stmts: Vec::new(),
                trailing: Some(then),
                label: None,
            }),
            else_branch: Some(Box::new(other)),
        },
        ty: ty.clone(),
        span,
    };
    let value = match (method, ls.as_slice()) {
        ("min", [b]) => pick(test(ctx, "le", a, *b), get(a), get(*b)),
        ("max", [b]) => pick(test(ctx, "ge", a, *b), get(a), get(*b)),
        (_, [lo, hi]) => {
            let above = pick(test(ctx, "gt", a, *hi), get(*hi), get(a));
            pick(test(ctx, "lt", a, *lo), get(*lo), above)
        }
        _ => unreachable!("arity checked above"),
    };
    with_lets(stmts, value)
}

/// `x` / `x.f.g` for a local or a field chain on one.
pub(crate) fn place_text(ctx: &LowerCtx<'_>, e: &HirExpr) -> Option<String> {
    match &e.kind {
        HirExprKind::Local(l) => ctx.locals.get(l.0 as usize)?.name.clone(),
        HirExprKind::FieldRead {
            target,
            struct_id,
            field_index,
            ..
        } => {
            let base = place_text(ctx, target)?;
            let def = ctx.struct_def(*struct_id);
            Some(format!("{base}.{}", def.fields[*field_index as usize].name))
        }
        _ => None,
    }
}

/// `method` on a sum value that only some of its cases have: name the
/// case and show the `if let` that reaches it.
pub(crate) fn sum_no_method(
    ctx: &LowerCtx<'_>,
    recv: &HirExpr,
    method: &str,
    no_args: bool,
    span: nabic_diagnostic::Span,
) -> Option<HirExpr> {
    let HirTy::Sum(cases) = &recv.ty else {
        return None;
    };
    let case = cases
        .iter()
        .find(|c| matches!(pick_method(ctx, c, method, None), Ok(Some(_))))?;
    let (ty, case) = (ctx.ty_name(&recv.ty), ctx.ty_name(case));
    let place = place_text(ctx, recv).filter(|p| !p.starts_with('#'));
    let what = place
        .as_deref()
        .map_or("this value".to_owned(), |p| format!("`{p}`"));
    let an = |t: &str| {
        if t.starts_with(['a', 'e', 'i', 'o', 'u']) {
            "an"
        } else {
            "a"
        }
    };
    let e = ctx.error_expr(
        Code::NO_METHOD,
        span,
        format!(
            "{what} is {} `{ty}`, and only {} `{case}` has the method `{method}`",
            an(&ty),
            an(&case)
        ),
    );
    let src = place.as_deref().unwrap_or("value");
    let bind = match src.rsplit_once('.') {
        Some((_, field)) => field.to_owned(),
        None => case.chars().next().unwrap_or('v').to_lowercase().collect(),
    };
    let call = if no_args { "()" } else { "(..)" };
    let fix = format!("`if let {bind}: {case} = {src} {{ {bind}.{method}{call} }}`");
    ctx.help_last(Some(if src.contains('.') {
        format!("`is` narrows local variables only, not fields; bind the field first: {fix}")
    } else {
        format!("check which case it holds first: {fix}")
    }));
    Some(e)
}
