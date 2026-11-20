//! Drop glue and ownership transfer.

#[allow(clippy::wildcard_imports)]
use crate::lower::expr::*;

/// `x.drop()` is banned for `Drop::drop`; the `using` desugar's
/// `x.#drop()` requires `Drop`.
pub(crate) fn drop_method<'m>(
    ctx: &LowerCtx<'_>,
    recv_ty: &HirTy,
    method: &'m str,
    span: nabic_diagnostic::Span,
) -> Result<&'m str, HirExpr> {
    match method {
        nabic_ast::USING_DROP_METHOD => {
            if *recv_ty != HirTy::Unknown && !needs_drop(ctx, recv_ty, 0) {
                ctx.emit_diag(nabic_diagnostic::NabiDiagnostic::error(
                    Code::USING_NOT_DROP,
                    span,
                    format!(
                        "`using` needs a resource whose type implements `Drop`; `{}` does not \
                         implement `Drop` nor hold a `Drop` field",
                        ctx.ty_name(recv_ty)
                    ),
                ));
                return Err(poison(span));
            }
            Ok(method)
        }
        "drop" if is_drop_method(ctx, recv_ty) => {
            direct_drop_error(ctx, span).map_or(Ok("drop"), Err)
        }
        m => Ok(m),
    }
}

/// Nesting past which drop glue stops looking (types are finite; this
/// bounds the work on deep ones).
pub(crate) const DROP_GLUE_DEPTH: u32 = 64;

/// Does releasing a `ty` run any `Drop::drop`: its own, or one of a field
/// / tuple element / payload's?
pub(crate) fn needs_drop(ctx: &LowerCtx<'_>, ty: &HirTy, depth: u32) -> bool {
    if depth > DROP_GLUE_DEPTH {
        return false;
    }
    if ctx.has_trait(ty, ctx.lang("drop_method")) {
        return true;
    }
    let any = |ts: &[HirTy]| ts.iter().any(|t| needs_drop(ctx, t, depth + 1));
    match ty {
        HirTy::Struct(sid) => {
            let fields: Vec<HirTy> = ctx
                .struct_def(*sid)
                .fields
                .iter()
                .map(|f| f.ty.clone())
                .collect();
            any(&fields)
        }
        HirTy::Enum(eid) => {
            let tys: Vec<HirTy> = ctx
                .enum_def(*eid)
                .variants
                .iter()
                .flat_map(variant_arg_tys)
                .collect();
            any(&tys)
        }
        HirTy::Tuple(ts) | HirTy::Sum(ts) => any(ts),
        _ => false,
    }
}

/// A `using` exit for `value`: its `drop`, then each `Drop` field /
/// element in reverse order, recursively; a sum releases the held
/// variant's payload.
pub(crate) fn drop_glue(
    ctx: &mut LowerCtx<'_>,
    value: HirExpr,
    span: nabic_diagnostic::Span,
    depth: u32,
) -> Vec<HirStmt> {
    let mut out = Vec::new();
    if !needs_drop(ctx, &value.ty, depth) {
        return out;
    }
    let ty = value.ty.clone();
    if ctx.has_trait(&ty, ctx.lang("drop_method")) {
        out.push(HirStmt::Expr(method_call_on(
            ctx,
            value.clone(),
            "drop",
            &[],
            span,
        )));
    }
    let block = |stmts: Vec<HirStmt>| HirExpr {
        kind: HirExprKind::Block(Box::new(HirBlock {
            stmts,
            trailing: None,
            label: None,
        })),
        ty: HirTy::Unit,
        span,
    };
    let unit_arm = || HirMatchArm {
        pat: HirPattern::Wildcard,
        guard: None,
        body: block(Vec::new()),
    };
    match &ty {
        HirTy::Struct(sid) => {
            let fields: Vec<HirTy> = ctx
                .struct_def(*sid)
                .fields
                .iter()
                .map(|f| f.ty.clone())
                .collect();
            for (i, fty) in fields.iter().enumerate().rev() {
                let field = HirExpr {
                    kind: HirExprKind::FieldRead {
                        target: Box::new(value.clone()),
                        struct_id: *sid,
                        field_index: u32::try_from(i).expect("field overflow"),
                        field_ty: fty.clone(),
                    },
                    ty: fty.clone(),
                    span,
                };
                out.extend(drop_glue(ctx, field, span, depth + 1));
            }
        }
        HirTy::Tuple(ts) => {
            for (i, t) in ts.iter().enumerate().rev() {
                let elem = HirExpr {
                    kind: HirExprKind::TupleField {
                        target: Box::new(value.clone()),
                        index: u32::try_from(i).expect("tuple field overflow"),
                        field_ty: t.clone(),
                    },
                    ty: t.clone(),
                    span,
                };
                out.extend(drop_glue(ctx, elem, span, depth + 1));
            }
        }
        HirTy::Enum(eid) => {
            let variants = ctx.enum_def(*eid).variants;
            let mut arms = Vec::new();
            for (vi, v) in variants.iter().enumerate() {
                let tys = variant_arg_tys(v);
                if !tys.iter().any(|t| needs_drop(ctx, t, depth + 1)) {
                    continue;
                }
                let locals: Vec<LocalId> =
                    tys.iter().map(|t| ctx.declare(None, t.clone())).collect();
                let mut stmts = Vec::new();
                for (l, t) in locals.iter().zip(&tys).rev() {
                    stmts.extend(drop_glue(ctx, hir_local(*l, t, span), span, depth + 1));
                }
                arms.push(HirMatchArm {
                    pat: HirPattern::EnumVariant {
                        enum_id: *eid,
                        variant_index: u32::try_from(vi).expect("variant overflow"),
                        args: locals
                            .iter()
                            .zip(&tys)
                            .map(|(l, t)| HirPattern::Binding {
                                local: *l,
                                ty: t.clone(),
                            })
                            .collect(),
                    },
                    guard: None,
                    body: block(stmts),
                });
            }
            arms.push(unit_arm());
            out.push(HirStmt::Expr(HirExpr {
                kind: HirExprKind::Match {
                    scrutinee: Box::new(value),
                    arms,
                },
                ty: HirTy::Unit,
                span,
            }));
        }
        HirTy::Sum(ts) => {
            let mut arms = Vec::new();
            for (vi, t) in ts.iter().enumerate() {
                if !needs_drop(ctx, t, depth + 1) {
                    continue;
                }
                let l = ctx.declare(None, t.clone());
                let stmts = drop_glue(ctx, hir_local(l, t, span), span, depth + 1);
                arms.push(HirMatchArm {
                    pat: HirPattern::SumVariant {
                        sum_ty: ty.clone(),
                        variant_index: u32::try_from(vi).expect("variant overflow"),
                        binder: Some(l),
                        payload_ty: Some(t.clone()),
                    },
                    guard: None,
                    body: block(stmts),
                });
            }
            arms.push(unit_arm());
            out.push(HirStmt::Expr(HirExpr {
                kind: HirExprKind::Match {
                    scrutinee: Box::new(value),
                    arms,
                },
                ty: HirTy::Unit,
                span,
            }));
        }
        _ => {}
    }
    out
}

/// The standard handle types a hand-off copies through their
/// `Transfer::transfer_copy`; a coroutine future's copies what its frame
/// captured. Other types are copied part by part, or kept when `Sync` or
/// `unsafe impl Send` (they manage their own sharing: `Task`, files,
/// sockets).
pub(crate) const TRANSFER_TYPES: &[&str] = &[
    "core::collections::list::List",
    "core::collections::slice::SliceMut",
    "core::collections::dict::Dict",
    "core::collections::deque::Deque",
    "core::collections::slice::Slice",
    "core::str::StrBuf",
    "core::crypto::secret::Secret",
    "core::crypto::secret::SecretWords",
    "bare::gc::GcPtr",
    "core::future::AsyncFuture",
    "core::future::TryAsyncFuture",
    // Not copied: marked as handed on (its joiners copy the result).
    "core::task::Task",
];

pub(crate) fn transfer_type(ctx: &LowerCtx<'_>, ty: &HirTy) -> bool {
    let HirTy::Struct(sid) = ty else {
        return false;
    };
    let name = ctx.struct_def(*sid).name;
    let base = name.split("__").next().unwrap_or(&name);
    TRANSFER_TYPES.contains(&base)
}

/// Does a hand-off of a `ty` copy anything: does it hold, by value, a
/// handle `TRANSFER_TYPES` lists outside `Sync` and `unsafe impl Send`
/// parts?
pub(crate) fn transfer_needs(ctx: &LowerCtx<'_>, ty: &HirTy, depth: u32) -> bool {
    if depth > DROP_GLUE_DEPTH {
        return false;
    }
    if transfer_type(ctx, ty) {
        return true;
    }
    let rec = |t: &HirTy| transfer_needs(ctx, t, depth + 1);
    let shared = |ty: &HirTy| {
        ctx.has_trait(ty, ctx.lang("sync_trait"))
            || ctx.has_impl(ty, &TraitRef::plain(ctx.lang("send_trait")), 0)
    };
    match ty {
        HirTy::Struct(sid) => {
            let def = ctx.struct_def(*sid);
            def.union_kind.is_none() && !shared(ty) && def.fields.iter().any(|f| rec(&f.ty))
        }
        HirTy::Enum(eid) => {
            !shared(ty)
                && ctx
                    .enum_def(*eid)
                    .variants
                    .iter()
                    .flat_map(variant_arg_tys)
                    .any(|t| rec(&t))
        }
        HirTy::Tuple(ts) | HirTy::Sum(ts) => !shared(ty) && ts.iter().any(rec),
        HirTy::Array(e, _) => rec(e),
        _ => false,
    }
}

/// Is `ty` one listed handle whose elements need no copy?
pub(crate) fn transfer_flat(ctx: &LowerCtx<'_>, ty: &HirTy) -> bool {
    let HirTy::Struct(sid) = ty else {
        return false;
    };
    transfer_type(ctx, ty)
        && ctx
            .struct_def(*sid)
            .generic_args
            .iter()
            .all(|t| !transfer_needs(ctx, t, 1))
}

/// Does a `ty` hold no GC reference, so its bytes copy as they are?
pub(crate) fn transfer_plain(ctx: &LowerCtx<'_>, ty: &HirTy, depth: u32) -> bool {
    if depth > DROP_GLUE_DEPTH {
        return false;
    }
    let rec = |t: &HirTy| transfer_plain(ctx, t, depth + 1);
    match ty {
        HirTy::Prim(p) => *p != PrimTy::Str,
        HirTy::Unit | HirTy::None => true,
        HirTy::Array(e, _) => rec(e),
        HirTy::Tuple(ts) | HirTy::Sum(ts) => ts.iter().all(rec),
        HirTy::Struct(sid) => {
            let def = ctx.struct_def(*sid);
            match &def.union_kind {
                Some(u) => u.aliases.iter().all(|(_, t)| rec(t)),
                None => def.fields.iter().all(|f| rec(&f.ty)),
            }
        }
        HirTy::Enum(eid) => ctx
            .enum_def(*eid)
            .variants
            .iter()
            .flat_map(variant_arg_tys)
            .all(|t| rec(&t)),
        _ => false,
    }
}

/// The generic functions `nabi.hand_over` calls when it copies (with one
/// argument, and with the value the slot keeps first).
pub(crate) const HAND_OVER_COPY: &str = "core::marker::hand_over_copy";

pub(crate) const HAND_OVER_INTO_COPY: &str = "core::marker::hand_over_into_copy";

/// `nabi.transfer::<T>(x, m)` (`x` with every collection it reaches copied,
/// `m` the hand-off's memo), `nabi.transfer_keep` (each noted as its own
/// copy), `nabi.transfer_needed::<T>()`, `nabi.transfer_plain::<T>()` and
/// `nabi.hand_over::<T>(x)` / `(kept, x)` (no call at all when nothing is
/// copied).
pub(crate) fn lower_transfer(
    ctx: &mut LowerCtx<'_>,
    op: Intrinsic,
    name: &str,
    targs: &[Ty],
    args: &[CallArg],
    span: nabic_diagnostic::Span,
) -> HirExpr {
    let Some(t) = targs.first().filter(|_| targs.len() == 1) else {
        discard_args(ctx, args);
        return ctx.error_expr(
            Code::BUILTIN_CALL,
            span,
            format!("`{name}` is called as `{name}::<T>(..)`"),
        );
    };
    let t = ctx.resolve_ast_ty(t);
    if op == Intrinsic::HandOver {
        let (x, copy) = match args {
            [CallArg::Positional(x)] => (x, HAND_OVER_COPY),
            [CallArg::Positional(kept), CallArg::Positional(x)] => {
                if !transfer_needs(ctx, &t, 0) {
                    let kept = lower_expected(ctx, kept, t.clone());
                    let x = lower_expected(ctx, x, t.clone());
                    let x = coerce(ctx, &t, x, &|| format!("the value of `{name}`"));
                    let local = ctx.declare(None, t.clone());
                    return HirExpr {
                        kind: HirExprKind::Block(Box::new(HirBlock {
                            stmts: vec![HirStmt::Expr(kept), HirStmt::Let { local, init: x }],
                            trailing: Some(hir_local(local, &t, span)),
                            label: None,
                        })),
                        ty: t,
                        span,
                    };
                }
                (x, HAND_OVER_INTO_COPY)
            }
            _ => {
                discard_args(ctx, args);
                return ctx.error_expr(
                    Code::BUILTIN_CALL,
                    span,
                    format!("`{name}` takes one value (or the kept one first)"),
                );
            }
        };
        if transfer_needs(ctx, &t, 0) {
            if let Some(def) = ctx.env.generic_fn(copy) {
                return call_generic(ctx, def, None, None, args, copy, span);
            }
        }
        let x = lower_expected(ctx, x, t.clone());
        return coerce(ctx, &t, x, &|| format!("the value of `{name}`"));
    }
    if matches!(
        op,
        Intrinsic::TransferNeeded | Intrinsic::TransferPlain | Intrinsic::TransferFlat
    ) {
        discard_args(ctx, args);
        let yes = match op {
            Intrinsic::TransferNeeded => transfer_needs(ctx, &t, 0),
            Intrinsic::TransferPlain => transfer_plain(ctx, &t, 0),
            _ => transfer_flat(ctx, &t),
        };
        return HirExpr {
            kind: HirExprKind::Const(ConstVal::Bool(yes)),
            ty: HirTy::Prim(PrimTy::Bool),
            span,
        };
    }
    let [CallArg::Positional(x), CallArg::Positional(memo)] = args else {
        discard_args(ctx, args);
        return ctx.error_expr(
            Code::BUILTIN_CALL,
            span,
            format!("`{name}` takes a value and a memo"),
        );
    };
    let x = lower_expected(ctx, x, t.clone());
    let x = coerce(ctx, &t, x, &|| format!("the value of `{name}`"));
    let method = match op {
        Intrinsic::TransferKeep => "transfer_keep",
        Intrinsic::TransferVisit => "transfer_visit",
        _ => "transfer_copy",
    };
    transfer_glue(ctx, x, memo, method, span, 0)
}

/// `value` with every collection it reaches copied: a listed handle by
/// its `method` (`transfer_copy` / `transfer_keep`), a struct, tuple,
/// array, enum or sum part by part. Recursion follows the type, never the
/// data: a handle's elements wait on the memo's queue.
pub(crate) fn transfer_glue(
    ctx: &mut LowerCtx<'_>,
    value: HirExpr,
    memo: &Expr,
    method: &str,
    span: nabic_diagnostic::Span,
    depth: u32,
) -> HirExpr {
    let ty = value.ty.clone();
    if !transfer_needs(ctx, &ty, depth) {
        return value;
    }
    if transfer_type(ctx, &ty) {
        return method_call_on(
            ctx,
            value,
            method,
            &[CallArg::Positional(memo.clone())],
            span,
        );
    }
    let at = |i: usize| u32::try_from(i).expect("index overflow");
    let local = ctx.declare(None, ty.clone());
    let v = hir_local(local, &ty, span);
    let part =
        |ctx: &mut LowerCtx<'_>, e: HirExpr| transfer_glue(ctx, e, memo, method, span, depth + 1);
    if let HirTy::Array(e, n) = &ty {
        return transfer_array(ctx, value, local, e, *n, &part, span);
    }
    if !matches!(
        ty,
        HirTy::Struct(_) | HirTy::Tuple(_) | HirTy::Enum(_) | HirTy::Sum(_)
    ) {
        return value;
    }
    let mut stmts = vec![HirStmt::Let { local, init: value }];
    let copy = match &ty {
        HirTy::Struct(sid) => {
            let fields: Vec<HirTy> = ctx
                .struct_def(*sid)
                .fields
                .iter()
                .map(|f| f.ty.clone())
                .collect();
            let read = |i: usize, fty: &HirTy| HirExpr {
                kind: HirExprKind::FieldRead {
                    target: Box::new(v.clone()),
                    struct_id: *sid,
                    field_index: at(i),
                    field_ty: fty.clone(),
                },
                ty: fty.clone(),
                span,
            };
            // Taking an affine part moves a value with its own `Drop` as a whole:
            // read the plain parts into temporaries first.
            let affine_part: Vec<bool> = fields
                .iter()
                .map(|f| crate::lower::affine::affine(ctx, f).is_some())
                .collect();
            let mut parts: Vec<Option<HirExpr>> = vec![None; fields.len()];
            if affine_part.iter().any(|&a| a) {
                for later in [false, true] {
                    for (i, fty) in fields.iter().enumerate() {
                        if affine_part[i] != later {
                            continue;
                        }
                        let e = part(ctx, read(i, fty));
                        let t = ctx.declare(None, fty.clone());
                        stmts.push(HirStmt::Let { local: t, init: e });
                        parts[i] = Some(hir_local(t, fty, span));
                    }
                }
            }
            let fields = fields
                .iter()
                .enumerate()
                .map(|(i, fty)| match parts[i].take() {
                    Some(e) => e,
                    None => part(ctx, read(i, fty)),
                })
                .collect();
            HirExprKind::StructInit {
                struct_id: *sid,
                fields,
            }
        }
        HirTy::Tuple(ts) => HirExprKind::Tuple {
            fields: ts
                .iter()
                .enumerate()
                .map(|(i, t)| {
                    let read = HirExpr {
                        kind: HirExprKind::TupleField {
                            target: Box::new(v.clone()),
                            index: at(i),
                            field_ty: t.clone(),
                        },
                        ty: t.clone(),
                        span,
                    };
                    part(ctx, read)
                })
                .collect(),
        },
        HirTy::Enum(eid) => {
            let variants = ctx.enum_def(*eid).variants;
            let mut arms = Vec::new();
            for (vi, var) in variants.iter().enumerate() {
                let tys = variant_arg_tys(var);
                if !tys.iter().any(|t| transfer_needs(ctx, t, depth + 1)) {
                    continue;
                }
                let locals: Vec<LocalId> =
                    tys.iter().map(|t| ctx.declare(None, t.clone())).collect();
                let args = locals
                    .iter()
                    .zip(&tys)
                    .map(|(l, t)| part(ctx, hir_local(*l, t, span)))
                    .collect();
                arms.push(HirMatchArm {
                    pat: HirPattern::EnumVariant {
                        enum_id: *eid,
                        variant_index: at(vi),
                        args: locals
                            .iter()
                            .zip(&tys)
                            .map(|(l, t)| HirPattern::Binding {
                                local: *l,
                                ty: t.clone(),
                            })
                            .collect(),
                    },
                    guard: None,
                    body: HirExpr {
                        kind: HirExprKind::EnumVariantInit {
                            enum_id: *eid,
                            variant_index: at(vi),
                            args,
                        },
                        ty: ty.clone(),
                        span,
                    },
                });
            }
            arms.push(HirMatchArm {
                pat: HirPattern::Wildcard,
                guard: None,
                body: v.clone(),
            });
            HirExprKind::Match {
                scrutinee: Box::new(v),
                arms,
            }
        }
        HirTy::Sum(ts) => {
            let mut arms = Vec::new();
            for (vi, t) in ts.iter().enumerate() {
                if !transfer_needs(ctx, t, depth + 1) {
                    continue;
                }
                let l = ctx.declare(None, t.clone());
                let inner = part(ctx, hir_local(l, t, span));
                arms.push(HirMatchArm {
                    pat: HirPattern::SumVariant {
                        sum_ty: ty.clone(),
                        variant_index: at(vi),
                        binder: Some(l),
                        payload_ty: Some(t.clone()),
                    },
                    guard: None,
                    body: HirExpr {
                        kind: HirExprKind::SumWrap {
                            sum_ty: ty.clone(),
                            variant_index: at(vi),
                            value: Some(Box::new(inner)),
                        },
                        ty: ty.clone(),
                        span,
                    },
                });
            }
            arms.push(HirMatchArm {
                pat: HirPattern::Wildcard,
                guard: None,
                body: v.clone(),
            });
            HirExprKind::Match {
                scrutinee: Box::new(v),
                arms,
            }
        }
        _ => unreachable!("checked above"),
    };
    HirExpr {
        kind: HirExprKind::Block(Box::new(HirBlock {
            stmts,
            trailing: Some(HirExpr {
                kind: copy,
                ty: ty.clone(),
                span,
            }),
            label: None,
        })),
        ty,
        span,
    }
}

/// `let a = value; let i = 0; while i < n { a[i] = part(a[i]); i += 1 } a`.
pub(crate) fn transfer_array(
    ctx: &mut LowerCtx<'_>,
    value: HirExpr,
    a: LocalId,
    elem: &HirTy,
    n: u32,
    part: &dyn Fn(&mut LowerCtx<'_>, HirExpr) -> HirExpr,
    span: nabic_diagnostic::Span,
) -> HirExpr {
    let usize_ty = HirTy::Prim(PrimTy::Usize);
    let arr_ty = HirTy::Array(Box::new(elem.clone()), n);
    let i = ctx.declare(None, usize_ty.clone());
    let num = |v: u32| HirExpr {
        kind: HirExprKind::Const(ConstVal::Int {
            value: i128::from(v),
            ty: PrimTy::Usize,
        }),
        ty: usize_ty.clone(),
        span,
    };
    let bin = |op: PrimOp, lhs: HirExpr, rhs: HirExpr, ty: HirTy| HirExpr {
        kind: HirExprKind::PrimBinOp {
            op,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        },
        ty,
        span,
    };
    let read = HirExpr {
        kind: HirExprKind::ArrayIndex {
            array: Box::new(hir_local(a, &arr_ty, span)),
            index: Box::new(hir_local(i, &usize_ty, span)),
            elem_ty: elem.clone(),
        },
        ty: elem.clone(),
        span,
    };
    let body = HirBlock {
        stmts: vec![
            HirStmt::ArrayAssign {
                array_local: a,
                path: Vec::new(),
                index: hir_local(i, &usize_ty, span),
                value: part(ctx, read),
                elem_ty: elem.clone(),
            },
            HirStmt::Assign {
                local: i,
                value: bin(
                    PrimOp::Add,
                    hir_local(i, &usize_ty, span),
                    num(1),
                    usize_ty.clone(),
                ),
            },
        ],
        trailing: None,
        label: None,
    };
    let cond = bin(
        PrimOp::Lt,
        hir_local(i, &usize_ty, span),
        num(n),
        HirTy::Prim(PrimTy::Bool),
    );
    HirExpr {
        kind: HirExprKind::Block(Box::new(HirBlock {
            stmts: vec![
                HirStmt::Let {
                    local: a,
                    init: value,
                },
                HirStmt::Let {
                    local: i,
                    init: num(0),
                },
                HirStmt::Expr(HirExpr {
                    kind: HirExprKind::While {
                        label: None,
                        cond: Box::new(cond),
                        body: Box::new(body),
                    },
                    ty: HirTy::Unit,
                    span,
                }),
            ],
            trailing: Some(hir_local(a, &arr_ty, span)),
            label: None,
        })),
        ty: arr_ty,
        span,
    }
}

/// Does `ty`'s `drop` method come from `Drop`?
pub(crate) fn is_drop_method(ctx: &LowerCtx<'_>, ty: &HirTy) -> bool {
    let trait_name = match pick_method(ctx, ty, "drop", None) {
        Ok(Some(MethodPick::Concrete(id))) => {
            ctx.env.method_info(id).and_then(|i| i.trait_name.clone())
        }
        Ok(Some(MethodPick::Generic(gm))) => gm.info.trait_name.clone(),
        _ => None,
    };
    trait_name.as_deref() == Some(ctx.lang("drop_method"))
}

/// `Drop::drop` runs where the owner releases the value (scope end or
/// `using` exit); a direct call is an error outside `unsafe` bare-tier
/// code.
pub(crate) fn direct_drop_error(
    ctx: &LowerCtx<'_>,
    span: nabic_diagnostic::Span,
) -> Option<HirExpr> {
    if ctx.unsafe_depth > 0 && ctx.current_module.starts_with("bare") {
        return None;
    }
    Some(
        ctx.error_expr(
            Code::EXPLICIT_DROP,
            span,
            "`Drop::drop` cannot be called directly; it runs when a `using` block exits \
         - expose a method such as `close()` to release early"
                .to_owned(),
        ),
    )
}

/// `Drop::drop(x)` / `File::drop(f)` / `T::drop(x)`.
pub(crate) fn check_direct_drop_path(ctx: &LowerCtx<'_>, p: &nabic_ast::Path) {
    let [prefix @ .., last] = p.segments.as_slice() else {
        return;
    };
    if last.name != "drop" || prefix.is_empty() {
        return;
    }
    let segs: Vec<&str> = prefix.iter().map(|s| s.name.as_str()).collect();
    let is_drop = ctx
        .env
        .names
        .trait_key(&segs, &ctx.current_module, ctx.here())
        == ctx.lang("drop_method")
        || {
            let ty = ctx.resolve_ast_ty(&nabic_ast::Ty {
                kind: nabic_ast::TyKind::Path(nabic_ast::Path {
                    segments: prefix.to_vec(),
                    span: p.span,
                    origin: None,
                }),
                span: p.span,
            });
            is_drop_method(ctx, &ty)
        };
    if is_drop {
        direct_drop_error(ctx, p.span);
    }
}
