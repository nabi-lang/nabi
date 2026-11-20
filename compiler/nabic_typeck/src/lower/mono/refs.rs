//! Copy-in / copy-out of `ref` parameters.

#[allow(clippy::wildcard_imports)]
use crate::lower::mono::*;

/// An `async` method with `ref self`: its future runs the body after the
/// call returns, so the call hands back the parameter's cell, read once
/// the future completes (`ref_unpack`).
pub fn defers_refs(f: &FnDef) -> bool {
    f.params.iter().any(|p| p.by_ref)
        && f.attrs
            .iter()
            .any(|a| a.path.first().map(String::as_str) == Some(nabic_ast::CORO_FN_ATTR))
}

/// A `ref` parameter's slot in the packed return: its cell when deferred.
pub fn ref_slot_ty(deferred: bool, t: HirTy) -> HirTy {
    if deferred {
        HirTy::GcBuf(Box::new(t))
    } else {
        t
    }
}

/// A function with `ref` parameters returns `(ret, r1, ..)` (unit `ret`
/// dropped, one field unwrapped). A throwing function's whole carrier is
/// `ret`, so `ref` values come back on the error path too; the payload
/// overlays ok and error.
pub(crate) fn ref_packed(ret: &HirTy, refs: &[HirTy]) -> HirTy {
    if refs.is_empty() {
        return ret.clone();
    }
    let mut fields: Vec<HirTy> = Vec::new();
    if *ret != HirTy::Unit {
        fields.push(ret.clone());
    }
    fields.extend(refs.iter().cloned());
    if fields.len() == 1 {
        fields.pop().expect("one field")
    } else {
        HirTy::Tuple(fields)
    }
}

/// `v` packed with the current values of the `ref` parameters.
pub(crate) fn ref_pack(ctx: &LowerCtx<'_>, v: HirExpr, span: nabic_diagnostic::Span) -> HirExpr {
    if ctx.ref_params.is_empty() {
        return v;
    }
    let refs: Vec<HirExpr> = ctx
        .ref_params
        .iter()
        .map(|(l, t)| {
            if ctx.ref_cells {
                hir_local(*l, t, span)
            } else {
                ctx.read_local(*l, span)
            }
        })
        .collect();
    let tys: Vec<HirTy> = refs.iter().map(|r| r.ty.clone()).collect();
    let ty = ref_packed(&v.ty, &tys);
    let mut fields = Vec::new();
    let mut stmts = Vec::new();
    if v.ty == HirTy::Unit {
        stmts.push(HirStmt::Expr(v));
    } else {
        fields.push(v);
    }
    fields.extend(refs);
    let value = if fields.len() == 1 {
        fields.pop().expect("one field")
    } else {
        HirExpr {
            kind: HirExprKind::Tuple { fields },
            ty: ty.clone(),
            span,
        }
    };
    if stmts.is_empty() {
        return value;
    }
    HirExpr {
        ty,
        kind: HirExprKind::Block(Box::new(HirBlock {
            stmts,
            trailing: Some(value),
            label: None,
        })),
        span,
    }
}

/// Call a function with `ref` parameters: write each final `ref` value back
/// into its argument place (`places` by parameter index: a local, a field
/// chain, a `[ref]` cell) and yield the value. A throwing callee's values
/// are written back before its error moves on, as with real references.
pub(crate) fn ref_unpack(
    ctx: &mut LowerCtx<'_>,
    call: HirExpr,
    out: &RefOut,
    places: &[Option<HirExpr>],
) -> HirExpr {
    let span = call.span;
    let call_ty = call.ty.clone();
    let tmp = ctx.declare(None, call_ty.clone());
    let mut stmts = vec![HirStmt::Let {
        local: tmp,
        init: call,
    }];
    let whole = hir_local(tmp, &call_ty, span);
    let lead = usize::from(out.ret != HirTy::Unit);
    let n = lead + out.refs.len();
    let field = |i: usize| -> HirExpr {
        if n == 1 {
            return whole.clone();
        }
        let ty = match &call_ty {
            HirTy::Tuple(ts) => ts[i].clone(),
            t => t.clone(),
        };
        HirExpr {
            kind: HirExprKind::TupleField {
                target: Box::new(whole.clone()),
                index: u32::try_from(i).expect("field index overflow"),
                field_ty: ty.clone(),
            },
            ty,
            span,
        }
    };
    let mut later = Vec::new();
    for (k, (pi, ty)) in out.refs.iter().enumerate() {
        let place = places.get(*pi).and_then(Option::as_ref);
        let value = match ty {
            // The cell the future writes: read when it completes.
            HirTy::GcBuf(elem) if out.deferred => HirExpr {
                kind: HirExprKind::BufGet {
                    buf: Box::new(field(lead + k)),
                    index: Box::new(hir_int(0, PrimTy::Usize, span)),
                    elem: (**elem).clone(),
                },
                ty: (**elem).clone(),
                span,
            },
            _ => field(lead + k),
        };
        if let Some(st) = place.and_then(|p| assign_place(ctx, p, value)) {
            if out.deferred {
                later.push((place.and_then(place_root), st));
            } else {
                stmts.push(st);
            }
        }
    }
    if !later.is_empty() {
        if ctx.awaited == Some(span) {
            ctx.awaited = None;
            ctx.await_write_back = later.into_iter().map(|(_, st)| st).collect();
        } else if let Some(l) = later.iter().filter_map(|(l, _)| *l).find(|l| {
            ctx.locals[l.0 as usize]
                .name
                .as_deref()
                .is_some_and(|n| !n.starts_with('#'))
        }) {
            // The future would hold the place until it completes, with
            // nothing to write it back then.
            let name = ctx.locals[l.0 as usize].name.clone().unwrap_or_default();
            let text = ctx.src(span).filter(|t| t.len() <= 60 && !t.contains('\n'));
            ctx.emit_error_at(
                Code::COROUTINE,
                span,
                format!(
                    "this `async` call takes `{name}` as `ref self`: its future holds `{name}` \
                     until it completes, so it must be awaited where it is made"
                ),
            );
            ctx.help_last(Some(match text {
                Some(t) => format!("write `await {t}`; `{name}` gets the new value then"),
                None => {
                    format!("`await` the call where it is made; `{name}` gets the new value then")
                }
            }));
        }
    }
    let value = (lead == 1).then(|| field(0));
    // A carrier: route a thrown error on.
    let (trailing, ty) = match value {
        Some(c) if matches!(out.ret, HirTy::Throws { .. }) => {
            let v = unpack_call(ctx, c);
            let ty = v.ty.clone();
            (Some(v), ty)
        }
        v => (v, out.ret.clone()),
    };
    HirExpr {
        kind: HirExprKind::Block(Box::new(HirBlock {
            stmts,
            trailing,
            label: None,
        })),
        ty,
        span,
    }
}

/// The local at the root of place `e` (a local, a field chain on one,
/// or a `[ref]` cell); `None`: not a place a `ref` argument can name.
pub(crate) fn place_root(e: &HirExpr) -> Option<LocalId> {
    match &e.kind {
        HirExprKind::Local(l) => Some(*l),
        HirExprKind::FieldRead { target, .. } => place_root(target),
        HirExprKind::BufGet { buf, .. } => match buf.kind {
            HirExprKind::Local(l) => Some(l),
            _ => None,
        },
        _ => None,
    }
}

/// Whether [`assign_place`] can write `place` back.
pub(crate) fn writable_place(ctx: &LowerCtx<'_>, place: &HirExpr) -> bool {
    match &place.kind {
        HirExprKind::Local(_) => true,
        HirExprKind::BufGet { buf, .. } => {
            matches!(buf.kind, HirExprKind::Local(l) if ctx.is_boxed(l))
        }
        HirExprKind::FieldRead { target, .. } => writable_place(ctx, target),
        _ => false,
    }
}

/// `place = value` for a local, a field chain on a local, or a cell. A
/// narrowed local writes through to the slot it narrows, as `=` does.
pub(crate) fn assign_place(
    ctx: &mut LowerCtx<'_>,
    place: &HirExpr,
    value: HirExpr,
) -> Option<HirStmt> {
    match &place.kind {
        HirExprKind::Local(l) => {
            let span = value.span;
            let st = HirStmt::Assign { local: *l, value };
            crate::lower::expr::write_through(ctx, Some(st), span)
        }
        HirExprKind::BufGet { buf, .. } if matches!(buf.kind, HirExprKind::Local(l) if ctx.is_boxed(l)) => {
            Some(HirStmt::Expr(buf_store((**buf).clone(), value)))
        }
        HirExprKind::FieldRead {
            target,
            struct_id,
            field_index,
            ..
        } => {
            let span = value.span;
            let rebuilt = HirExpr {
                kind: HirExprKind::WithField {
                    target: target.clone(),
                    struct_id: *struct_id,
                    field_index: *field_index,
                    value: Box::new(value),
                },
                ty: target.ty.clone(),
                span,
            };
            assign_place(ctx, target, rebuilt)
        }
        _ => None,
    }
}
