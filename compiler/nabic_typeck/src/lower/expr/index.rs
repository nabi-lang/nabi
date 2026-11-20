//! Indexing, slicing and axes.

#[allow(clippy::wildcard_imports)]
use crate::lower::expr::*;

/// Array indices are integers.
pub(crate) fn check_index_ty(ctx: &LowerCtx<'_>, idx: &HirExpr) {
    let ty = crate::lower::infer::zonk(ctx, &idx.ty);
    if !matches!(&ty, HirTy::Prim(p) if p.is_int()) && ty != HirTy::Unknown {
        ctx.emit_error(
            Code::INVALID_INDEX,
            format!(
                "array index must be an integer, found `{}`",
                ctx.ty_name(&idx.ty)
            ),
        );
    }
}

/// The first segment of a hook's path (`lang_fn`): `#lang:` and its slot.
pub(crate) const LANG_FN: &str = "#lang:";

/// The lang slot a path segment calls through (`lang_fn`), if any.
pub(crate) fn lang_slot(seg: &str) -> Option<&str> {
    seg.strip_prefix(LANG_FN)
}

/// The path of hook `core::<module>::<name>`, which lowered code
/// calls: through lang slot `<name>_fn`, by path when no item fills it.
/// Not by name, so the hook need not be visible, and its `#[deprecated]`
/// warns only about calls the user wrote.
pub(crate) fn lang_fn(module: &str, name: &str, span: nabic_diagnostic::Span) -> nabic_ast::Path {
    lang_fn_in(&format!("{name}_fn"), module, name, span)
}

/// `lang_fn` through slot `slot`; `module` may be nested (`a::b`).
pub(crate) fn lang_fn_in(
    slot: &str,
    module: &str,
    name: &str,
    span: nabic_diagnostic::Span,
) -> nabic_ast::Path {
    let slot = format!("{LANG_FN}{slot}");
    nabic_ast::Path {
        segments: [slot.as_str(), "", "core"]
            .into_iter()
            .chain(module.split("::"))
            .chain([name])
            .map(|n| nabic_ast::PathSegment {
                name: n.to_owned(),
                generics: Vec::new(),
            })
            .collect(),
        span,
        origin: None,
    }
}

/// `core::index::<name>(args..)`, through its lang slot.
pub(crate) fn index_fn(name: &str, args: Vec<Expr>, span: nabic_diagnostic::Span) -> Expr {
    let path = lang_fn("index", name, span);
    Expr {
        kind: ExprKind::Call {
            callee: Box::new(Expr {
                kind: ExprKind::Path(path),
                span,
            }),
            args: args.into_iter().map(CallArg::Positional).collect(),
            trailing_closure: None,
        },
        span,
    }
}

pub(crate) fn isize_ty(span: nabic_diagnostic::Span) -> Ty {
    Ty {
        kind: AstTyKind::Path(nabic_ast::Path {
            segments: vec![nabic_ast::PathSegment {
                name: "isize".to_owned(),
                generics: Vec::new(),
            }],
            span,
            origin: None,
        }),
        span,
    }
}

/// `e as isize`.
pub(crate) fn as_isize(e: Expr, span: nabic_diagnostic::Span) -> Expr {
    Expr {
        kind: ExprKind::Cast(Box::new(e), isize_ty(span)),
        span,
    }
}

/// `core::index::index_from_end(i as isize, len)`: a signed position
/// `i` < 0 is `len + i`.
pub(crate) fn from_end(i: Expr, len: Expr, span: nabic_diagnostic::Span) -> Expr {
    index_fn("index_from_end", vec![as_isize(i, span), len], span)
}

pub(crate) fn len_of(recv: &Expr, span: nabic_diagnostic::Span) -> Expr {
    Expr {
        kind: ExprKind::MethodCall {
            receiver: Box::new(recv.clone()),
            method: "len".to_owned(),
            type_args: Vec::new(),
            args: Vec::new(),
            trailing_closure: None,
            origin: None,
            written: false,
        },
        span,
    }
}

/// Index `idx` of a fixed array of length `n`; a signed one counts from
/// the end.
pub(crate) fn array_position(ctx: &mut LowerCtx<'_>, idx: &Expr, n: u32) -> HirExpr {
    let i = lower_position(ctx, idx);
    check_index_ty(ctx, &i);
    if !is_signed(ctx, &i.ty) {
        return i;
    }
    let span = i.span;
    let len = Expr {
        kind: ExprKind::Literal(AstLit::Int(format!("{n}usize"))),
        span,
    };
    let i = splice(ctx, i);
    lower_expr(ctx, &from_end(i, len, span))
}

/// Does `ty`'s `index` / `index_set` / `slice` take a position (a `List`,
/// `Slice` or `str`, not a `Dict<i64, V>`)?
pub(crate) fn takes_position(ctx: &LowerCtx<'_>, ty: &HirTy, method: &str) -> bool {
    fn text(t: &Ty) -> String {
        let AstTyKind::Path(p) = &t.kind else {
            return String::new();
        };
        let Some(seg) = p.segments.last() else {
            return String::new();
        };
        if seg.generics.is_empty() {
            return seg.name.clone();
        }
        let args: Vec<String> = seg.generics.iter().map(text).collect();
        format!("{}<{}>", seg.name, args.join(", "))
    }
    let param = match pick_method(ctx, ty, method, None) {
        Ok(Some(MethodPick::Concrete(id))) => {
            ctx.sig_by_id(id).param_tys.get(1).map(|t| ctx.ty_name(t))
        }
        Ok(Some(MethodPick::Generic(gm))) => {
            gm.def.params.get(1).and_then(|p| p.ty.as_ref()).map(text)
        }
        _ => None,
    };
    let want = if method == "slice" {
        "Range<usize>"
    } else {
        "usize"
    };
    param.is_some_and(|p| p == want || p.ends_with(&format!("::{want}")))
}

/// `recv.index(idx)` / `recv.index_set(idx, ..)`. A signed index on a
/// positional receiver counts from the end via `index_from_end`; unsigned
/// ones are plain calls.
pub(crate) fn index_call(
    ctx: &mut LowerCtx<'_>,
    recv_h: HirExpr,
    method: &str,
    idx: &Expr,
    rest: &[CallArg],
    span: nabic_diagnostic::Span,
) -> HirExpr {
    let mut args = rest.to_vec();
    if !takes_position(ctx, &recv_h.ty, method) {
        args.insert(0, CallArg::Positional(idx.clone()));
        return method_call_on(ctx, recv_h, method, &args, span);
    }
    let i = lower_position(ctx, idx);
    if !is_signed(ctx, &i.ty) {
        args.insert(0, CallArg::Positional(splice(ctx, i)));
        return method_call_on(ctx, recv_h, method, &args, span);
    }
    let t = tmp_path(ctx, "t", span);
    let i = splice(ctx, i);
    args.insert(0, CallArg::Positional(from_end(i, len_of(&t, span), span)));
    let recv = splice(ctx, recv_h);
    let call = Expr {
        kind: ExprKind::MethodCall {
            receiver: Box::new(t.clone()),
            method: method.to_owned(),
            type_args: Vec::new(),
            args,
            trailing_closure: None,
            origin: None,
            written: false,
        },
        span,
    };
    lower_expr(ctx, &block_of(vec![let_stmt(&t, recv)], call, span))
}

/// `a[i:j:k]`, or a slice part of a multi-part index on a type that is
/// not axis-indexed: a `SliceArg` value for its `Index` impl.
pub(crate) fn lower_slice_form(
    ctx: &mut LowerCtx<'_>,
    kind: &ExprKind,
    span: nabic_diagnostic::Span,
) -> HirExpr {
    match kind {
        ExprKind::Slice {
            target,
            start,
            end,
            step,
        } => lower_slice(
            ctx,
            target,
            start.as_deref(),
            end.as_deref(),
            step.as_deref(),
            span,
        ),
        ExprKind::AxisSlice { start, end, step } => {
            let e = slice_arg(ctx, start.as_deref(), end.as_deref(), step.as_deref(), span);
            lower_expr(ctx, &e)
        }
        _ => unreachable!("a slice form"),
    }
}

/// `arr[idx]` on an axis-indexed type, or the misuse error on a one-axis
/// one; else the lowered `arr` back for the other forms.
pub(crate) fn axis_index_read(
    ctx: &mut LowerCtx<'_>,
    arr: &Expr,
    idx: &Expr,
    span: nabic_diagnostic::Span,
) -> Result<HirExpr, HirExpr> {
    let arr_h = lower_expected(ctx, arr, HirTy::Unknown);
    if axis_indexed(ctx, &arr_h.ty) {
        let parts = axis_parts(ctx, idx);
        let (since, ty) = (ctx.probe_mark().diagnostics, arr_h.ty.clone());
        let read = index_call(ctx, arr_h, "index", &parts, &[], span);
        no_axis_form(ctx, since, &ty, idx, span);
        return Ok(read);
    }
    if one_axis_misuse(ctx, &arr_h.ty, idx, span) {
        return Ok(poison(span));
    }
    Err(arr_h)
}

/// `ty` implements `AxisIndexed`: its index parts are axis arguments.
pub(crate) fn axis_indexed(ctx: &LowerCtx<'_>, ty: &HirTy) -> bool {
    let key = ctx.lang("axis_indexed_trait");
    !key.is_empty() && matches!(ty, HirTy::Struct(_) | HirTy::Enum(_)) && ctx.has_trait(ty, key)
}

/// The item `leaf` beside the one in lang slot `slot` (`SliceArg` is in
/// `core::index`), as a path written by the compiler.
pub(crate) fn lang_item_path(
    ctx: &LowerCtx<'_>,
    slot: &str,
    leaf: Option<&str>,
    span: nabic_diagnostic::Span,
) -> Expr {
    let full = ctx.lang(slot).to_owned();
    if let Some(leaf) = leaf {
        return crate::lower::bignum::path_expr(&full, leaf, span);
    }
    let (module, name) = full.rsplit_once("::").unwrap_or(("", &full));
    crate::lower::bignum::path_expr(module, name, span)
}

/// An integer axis position or slice bound as an `isize` (a literal is
/// one; a wider or unsigned value is checked on the way).
pub(crate) fn axis_int(ctx: &mut LowerCtx<'_>, e: &Expr) -> Expr {
    let span = e.span;
    let h = lower_expected(ctx, e, HirTy::Prim(PrimTy::Isize));
    // A variable still open holds its literal's default (`for i in 0..4`).
    let HirTy::Prim(p) = crate::lower::infer::zonk(ctx, &h.ty) else {
        return splice(ctx, h);
    };
    let cast = |e: Expr, to: &str| Expr {
        kind: ExprKind::Cast(
            Box::new(e),
            Ty {
                kind: AstTyKind::Path(nabic_ast::Path {
                    segments: vec![nabic_ast::PathSegment {
                        name: to.to_owned(),
                        generics: Vec::new(),
                    }],
                    span,
                    origin: None,
                }),
                span,
            },
        ),
        span,
    };
    let v = splice(ctx, h);
    match p {
        PrimTy::Isize => v,
        PrimTy::I8 | PrimTy::I16 | PrimTy::I32 | PrimTy::U8 | PrimTy::U16 => as_isize(v, span),
        _ if p.is_signed_int() => index_fn("axis_from_i64", vec![cast(v, "i64")], span),
        _ if p.is_unsigned_int() => index_fn("axis_from_u64", vec![cast(v, "u64")], span),
        _ => v,
    }
}

/// `SliceArg::new(a, b, k)` for `a:b:k`, a missing part `none`.
pub(crate) fn slice_arg(
    ctx: &mut LowerCtx<'_>,
    start: Option<&Expr>,
    end: Option<&Expr>,
    step: Option<&Expr>,
    span: nabic_diagnostic::Span,
) -> Expr {
    let mut part = |x: Option<&Expr>| match x {
        Some(e) => axis_int(ctx, e),
        None => Expr {
            kind: ExprKind::Literal(AstLit::None),
            span,
        },
    };
    let args = vec![part(start), part(end), part(step)];
    Expr {
        kind: ExprKind::Call {
            callee: Box::new(lang_item_path(ctx, "slice_arg_type", Some("new"), span)),
            args: args.into_iter().map(CallArg::Positional).collect(),
            trailing_closure: None,
        },
        span,
    }
}

/// An axis index whose form no `Index` / `IndexSet` impl of `ty` takes
/// fails as an ambiguous call (the impls differ only in that form): say
/// what it is instead.
pub(crate) fn no_axis_form(ctx: &LowerCtx<'_>, since: usize, ty: &HirTy, idx: &Expr, span: Span) {
    let part = |p: &Expr| match &p.kind {
        ExprKind::AxisSlice { .. } => "slice",
        ExprKind::Literal(AstLit::None) => "none",
        ExprKind::Range {
            start: None,
            end: None,
            inclusive: false,
        } => "..",
        _ => "position",
    };
    let form = match &idx.kind {
        ExprKind::Tuple(parts) if parts.len() >= 2 => {
            parts.iter().map(part).collect::<Vec<_>>().join(", ")
        }
        _ => part(idx).to_owned(),
    };
    let name = ctx.ty_name(ty);
    let mut diags = ctx.env.sink.diagnostics.borrow_mut();
    for d in diags[since..].iter_mut().filter(|d| {
        d.code == Code::AMBIGUOUS_ITEM
            && d.primary
                .as_ref()
                .is_some_and(|l| l.span.start == span.start)
    }) {
        d.message = format!("`{name}` takes no index of the form `[{form}]`");
        d.notes = vec![
            "help: index in two brackets, each a form it takes (`a[:, none][.., 0]`)".to_owned(),
        ];
    }
}

/// The index of an `AxisIndexed` value as axis arguments: integers as
/// `isize`, `a:b:k` as `SliceArg`, `none` as `NewAxis`, `..` beside
/// other parts as `Rest`; anything else (a mask) as written.
pub(crate) fn axis_parts(ctx: &mut LowerCtx<'_>, idx: &Expr) -> Expr {
    let one = |ctx: &mut LowerCtx<'_>, p: &Expr, many: bool| -> Expr {
        let span = p.span;
        match &p.kind {
            ExprKind::AxisSlice { start, end, step } => {
                slice_arg(ctx, start.as_deref(), end.as_deref(), step.as_deref(), span)
            }
            ExprKind::Literal(AstLit::None) => lang_item_path(ctx, "new_axis_type", None, span),
            ExprKind::Range {
                start: None,
                end: None,
                inclusive: false,
            } if many => lang_item_path(ctx, "rest_axis_type", None, span),
            _ => axis_int(ctx, p),
        }
    };
    match &idx.kind {
        ExprKind::Tuple(parts) if parts.len() >= 2 => Expr {
            kind: ExprKind::Tuple(parts.iter().map(|p| one(ctx, p, true)).collect()),
            span: idx.span,
        },
        _ => one(ctx, idx, false),
    }
}

/// Parts of a multi-part index that only an axis-indexed type takes,
/// on a type indexed along one axis: an error that says what to write.
pub(crate) fn one_axis_misuse(
    ctx: &LowerCtx<'_>,
    ty: &HirTy,
    idx: &Expr,
    span: nabic_diagnostic::Span,
) -> bool {
    let ExprKind::Tuple(parts) = &idx.kind else {
        return false;
    };
    if !parts
        .iter()
        .any(|p| matches!(p.kind, ExprKind::AxisSlice { .. }))
    {
        return false;
    }
    let one_axis = matches!(ty, HirTy::Array(..) | HirTy::Prim(_))
        || takes_position(ctx, ty, "index")
        || is_list(ctx, ty);
    if !one_axis {
        return false;
    }
    ctx.emit_error_at(
        Code::ONE_AXIS_INDEX,
        span,
        format!(
            "`{}` is indexed along one axis, but this index has {} parts with a slice among them",
            ctx.ty_name(ty),
            parts.len()
        ),
    );
    ctx.help_last(Some(
        "slice and index in separate brackets (`m[a:b][c]`), or keep the data in an \
         `Array` (`core::array::Array`), which takes `m[a:b, c]`"
            .to_owned(),
    ));
    true
}

pub(crate) fn block_of(stmts: Vec<Stmt>, tail: Expr, span: nabic_diagnostic::Span) -> Expr {
    Expr {
        kind: ExprKind::Block(Block {
            stmts,
            trailing: Some(Box::new(tail)),
            span,
            label: None,
        }),
        span,
    }
}

/// `t[a:b:k]` with Python's semantics on a positional target (the
/// `Sliceable` impls clamp to the length):
/// - no step, unsigned bounds: `t.slice(a..b)`;
/// - no step, a signed bound counts from the end:
///   `{ let #t = t; #t.slice(slice_bound(a, #t.len())..b) }`;
/// - a step: `{ let #t = t; let #k: isize = k;
///   #t.slice(slice_range(a, b, #k, #t.len())).step(#k) }`, missing bounds
///   as `none` (their default depends on the step's sign).
pub(crate) fn lower_slice(
    ctx: &mut LowerCtx<'_>,
    target: &Expr,
    start: Option<&Expr>,
    end: Option<&Expr>,
    step: Option<&Expr>,
    span: nabic_diagnostic::Span,
) -> HirExpr {
    let want_mut = std::mem::take(&mut ctx.want_view)
        || ctx.current_expected().is_some_and(|t| is_slice_mut(ctx, t));
    let t_h = lower_expected(ctx, target, HirTy::Unknown);
    // `a[1:3]` / `a[..]` on an axis-indexed type: `Index<SliceArg>`.
    if axis_indexed(ctx, &t_h.ty) {
        let sel = slice_arg(ctx, start, end, step, span);
        return index_call(ctx, t_h, "index", &sel, &[], span);
    }
    if !takes_position(ctx, &t_h.ty, "slice") {
        let t = splice(ctx, t_h);
        return lower_expr(ctx, &slice_desugar(&t, start, end, step, false, span));
    }
    // `xs[a:b]` of a list where a `SliceMut` is expected: a writable view.
    let view = want_mut && is_list(ctx, &t_h.ty);
    if view && step.is_some() {
        return ctx.error_expr(
            Code::INVALID_INDEX,
            span,
            "a `SliceMut` views a contiguous run of a list: drop the step, or copy the \
             elements with `xs[a:b:k].to_list()`"
                .to_owned(),
        );
    }
    let a = start.map(|e| lower_position(ctx, e));
    let b = end.map(|e| lower_position(ctx, e));
    let signed = |h: &Option<HirExpr>| h.as_ref().is_some_and(|h| is_signed(ctx, &h.ty));
    if step.is_none() && !signed(&a) && !signed(&b) {
        let t = splice(ctx, t_h);
        let a = a.map(|h| splice(ctx, h));
        let b = b.map(|h| splice(ctx, h));
        return lower_expr(
            ctx,
            &slice_desugar(&t, a.as_ref(), b.as_ref(), None, view, span),
        );
    }
    let t = tmp_path(ctx, "t", span);
    let recv = splice(ctx, t_h);
    let mut bound = |h: Option<HirExpr>| h.map(|h| (is_signed(ctx, &h.ty), splice(ctx, h)));
    let (a, b) = (bound(a), bound(b));
    let Some(k) = step else {
        let pos = |x: Option<(bool, Expr)>| {
            x.map(|(neg, e)| {
                if neg {
                    index_fn(
                        "slice_bound",
                        vec![as_isize(e, span), len_of(&t, span)],
                        span,
                    )
                } else {
                    e
                }
            })
        };
        let (a, b) = (pos(a), pos(b));
        let body = slice_desugar(&t, a.as_ref(), b.as_ref(), None, view, span);
        return lower_expr(ctx, &block_of(vec![let_stmt(&t, recv)], body, span));
    };
    let kt = tmp_path(ctx, "k", span);
    let opt = |x: Option<(bool, Expr)>| {
        x.map_or(
            Expr {
                kind: ExprKind::Literal(AstLit::None),
                span,
            },
            |(_, e)| as_isize(e, span),
        )
    };
    let range = index_fn(
        "slice_range",
        vec![opt(a), opt(b), kt.clone(), len_of(&t, span)],
        span,
    );
    let call = |recv: Expr, method: &str, arg: Expr| Expr {
        kind: ExprKind::MethodCall {
            receiver: Box::new(recv),
            method: method.to_owned(),
            type_args: Vec::new(),
            args: vec![CallArg::Positional(arg)],
            trailing_closure: None,
            origin: None,
            written: false,
        },
        span,
    };
    let body = call(call(t.clone(), "slice", range), "step", kt.clone());
    // Any signed integer step widens to `isize`.
    let k_h = lower_expected(ctx, k, HirTy::Prim(PrimTy::Isize));
    let k_signed = is_signed(ctx, &k_h.ty);
    let k_e = splice(ctx, k_h);
    let let_k = Stmt {
        span,
        kind: StmtKind::Let {
            name: path_name(&kt),
            ty: Some(isize_ty(span)),
            init: if k_signed { as_isize(k_e, span) } else { k_e },
        },
    };
    lower_expr(ctx, &block_of(vec![let_stmt(&t, recv), let_k], body, span))
}

/// `t[a:b]` is `t.slice(a..b)` and `t[a:b:k]` `t.slice(a..b).step(k)`; a
/// missing `a` is `0`, a missing `b` is `t.len()` with `t` evaluated once.
/// `view`: the writable view `slice_mut(t, a..b)` instead.
pub(crate) fn slice_desugar(
    target: &Expr,
    start: Option<&Expr>,
    end: Option<&Expr>,
    step: Option<&Expr>,
    view: bool,
    span: nabic_diagnostic::Span,
) -> Expr {
    const TEMP: &str = "__nabi_slice_target";
    let call = |recv: Expr, method: &str, args: Vec<Expr>| Expr {
        kind: ExprKind::MethodCall {
            receiver: Box::new(recv),
            method: method.to_owned(),
            type_args: Vec::new(),
            args: args.into_iter().map(CallArg::Positional).collect(),
            trailing_closure: None,
            origin: None,
            written: false,
        },
        span,
    };
    let temp = end.is_none();
    let recv = if temp {
        Expr {
            kind: ExprKind::Path(nabic_ast::Path {
                segments: vec![nabic_ast::PathSegment {
                    name: TEMP.to_owned(),
                    generics: Vec::new(),
                }],
                span,
                origin: None,
            }),
            span,
        }
    } else {
        target.clone()
    };
    let start = start.cloned().unwrap_or_else(|| Expr {
        kind: ExprKind::Literal(AstLit::Int("0usize".to_owned())),
        span,
    });
    let end = end
        .cloned()
        .unwrap_or_else(|| call(recv.clone(), "len", Vec::new()));
    let range = Expr {
        kind: ExprKind::Range {
            start: Some(Box::new(start)),
            end: Some(Box::new(end)),
            inclusive: false,
        },
        span,
    };
    let mut e = if view {
        index_fn("slice_mut", vec![recv, range], span)
    } else {
        call(recv, "slice", vec![range])
    };
    if let Some(k) = step {
        e = call(e, "step", vec![k.clone()]);
    }
    if !temp {
        return e;
    }
    Expr {
        kind: ExprKind::Block(nabic_ast::Block {
            stmts: vec![nabic_ast::Stmt {
                kind: nabic_ast::StmtKind::Let {
                    name: TEMP.to_owned(),
                    ty: None,
                    init: target.clone(),
                },
                span,
            }],
            trailing: Some(Box::new(e)),
            span,
            label: None,
        }),
        span,
    }
}
