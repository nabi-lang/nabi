//! Intrinsics, shapes and magic calls.

#[allow(clippy::wildcard_imports)]
use crate::lower::expr::*;

/// A generic `#[intrinsic]` call; `size_of::<T>()` / `align_of::<T>()`
/// take `T` from `targs`.
pub(crate) fn lower_intrinsic(
    ctx: &mut LowerCtx<'_>,
    op: Intrinsic,
    name: &str,
    targs: &[Ty],
    args: &[CallArg],
    span: nabic_diagnostic::Span,
) -> HirExpr {
    match op {
        Intrinsic::AnyCast
        | Intrinsic::MethodTable
        | Intrinsic::FieldTable
        | Intrinsic::TypeDesc
        | Intrinsic::TypeName
        | Intrinsic::Shown => crate::lower::reflect::intrinsic(ctx, op, name, targs, args, span),
        Intrinsic::PluginGet | Intrinsic::PluginSig => {
            crate::lower::plugin::intrinsic(ctx, op, name, targs, args, span)
        }
        Intrinsic::Transfer
        | Intrinsic::TransferKeep
        | Intrinsic::TransferVisit
        | Intrinsic::TransferNeeded
        | Intrinsic::TransferPlain
        | Intrinsic::TransferFlat
        | Intrinsic::HandOver => lower_transfer(ctx, op, name, targs, args, span),
        Intrinsic::SizeOf | Intrinsic::AlignOf => {
            let (Some(t), true) = (targs.first(), args.is_empty() && targs.len() == 1) else {
                discard_args(ctx, args);
                return ctx.error_expr(
                    Code::BUILTIN_CALL,
                    span,
                    format!("`{name}` is called as `{name}::<T>()`"),
                );
            };
            HirExpr {
                kind: HirExprKind::SizeOf {
                    ty: ctx.resolve_ast_ty(t),
                    align: op == Intrinsic::AlignOf,
                },
                ty: HirTy::Prim(PrimTy::Usize),
                span,
            }
        }
        Intrinsic::PtrRead { volatile } => {
            lower_ptr_intrinsic(ctx, name, false, volatile, args, span)
        }
        Intrinsic::PtrWrite { volatile } => {
            lower_ptr_intrinsic(ctx, name, true, volatile, args, span)
        }
        Intrinsic::Atomic(atomic) => lower_atomic_intrinsic(ctx, atomic, args, span),
        Intrinsic::DropInPlace => {
            let value = lower_ptr_intrinsic(ctx, name, false, false, args, span);
            if matches!(value.ty, HirTy::Unknown) {
                return value;
            }
            let ty = value.ty.clone();
            let local = ctx.declare(None, ty.clone());
            let mut stmts = vec![HirStmt::Let { local, init: value }];
            stmts.extend(drop_glue(ctx, hir_local(local, &ty, span), span, 0));
            HirExpr {
                kind: HirExprKind::Block(Box::new(HirBlock {
                    stmts,
                    trailing: None,
                    label: None,
                })),
                ty: HirTy::Unit,
                span,
            }
        }
        // `collect::intrinsic_of` keeps primitive ops and conversions off generic
        // functions; `addr_of` is lowered at the call (`lower_addr_of`).
        Intrinsic::Prim(_) | Intrinsic::Convert | Intrinsic::AddrOf => poison(span),
        Intrinsic::BufNew
        | Intrinsic::BufGet
        | Intrinsic::BufSet
        | Intrinsic::BufSame
        | Intrinsic::BufErase
        | Intrinsic::BufCast => lower_buf_intrinsic(ctx, op, name, args, span),
    }
}

/// `bare` raw memory: `raw_read(p)`, `raw_write(p, v)` (and
/// `_volatile`) on a pointer struct whose field 0 is the address.
pub(crate) fn lower_ptr_intrinsic(
    ctx: &mut LowerCtx<'_>,
    op: &str,
    write: bool,
    volatile: bool,
    args: &[CallArg],
    span: nabic_diagnostic::Span,
) -> HirExpr {
    let usize_ty = HirTy::Prim(PrimTy::Usize);
    let pos: Vec<&Expr> = args
        .iter()
        .filter_map(|a| match a {
            CallArg::Positional(e) => Some(e),
            _ => None,
        })
        .collect();
    let want = if write { 2 } else { 1 };
    if pos.len() != want || args.len() != want {
        discard_args(ctx, args);
        return ctx.error_expr(
            Code::BUILTIN_CALL,
            span,
            format!("`{op}` takes {want} argument(s)"),
        );
    }
    let p = lower_expected(ctx, pos[0], HirTy::Unknown);
    let HirTy::Struct(sid) = p.ty else {
        discard_args(ctx, &args[1..]);
        if p.ty == HirTy::Unknown {
            return p;
        }
        return ctx.error_expr(
            Code::BUILTIN_CALL,
            span,
            format!("`{op}` needs a `Ptr<T>`, found `{}`", ctx.ty_name(&p.ty)),
        );
    };
    let def = ctx.struct_def(sid);
    let (Some(ty), Some(HirTy::Prim(PrimTy::Usize))) = (
        def.generic_args.first().cloned(),
        def.fields.first().map(|f| f.ty.clone()),
    ) else {
        return ctx.error_expr(Code::BUILTIN_CALL, span, format!("`{op}` needs a `Ptr<T>`"));
    };
    let addr = Box::new(HirExpr {
        kind: HirExprKind::FieldRead {
            target: Box::new(p),
            struct_id: sid,
            field_index: 0,
            field_ty: usize_ty.clone(),
        },
        ty: usize_ty,
        span,
    });
    if write {
        let v = lower_expected(ctx, pos[1], ty.clone());
        let v = coerce(ctx, &ty, v, &|| "the stored value".to_owned());
        HirExpr {
            kind: HirExprKind::RawWrite {
                addr,
                value: Box::new(v),
                ty,
                volatile,
            },
            ty: HirTy::Unit,
            span,
        }
    } else {
        HirExpr {
            kind: HirExprKind::RawRead {
                addr,
                ty: ty.clone(),
                volatile,
            },
            ty,
            span,
        }
    }
}

/// A standard-library method marked `#[intrinsic("nabi.addr_of")]`.
pub(crate) fn is_addr_intrinsic(def: &FnDef, module: &str) -> bool {
    nabic_core::in_toolchain(module)
        && def.attrs.iter().any(|a| {
            a.name() == "intrinsic"
                && matches!(&a.body, nabic_ast::AttrBody::Args(args) if args.iter().any(|x|
                    matches!(x, nabic_ast::AttrArg::Lit(nabic_ast::Literal::Str(s)) if s == "nabi.addr_of")))
        })
}

/// `addr_of(place)` / `many_addr_of(place)`: a pointer to the place
/// itself, not to a copy.
pub(crate) fn lower_addr_of(
    ctx: &mut LowerCtx<'_>,
    def: &FnDef,
    name: &str,
    args: &[CallArg],
    span: nabic_diagnostic::Span,
) -> HirExpr {
    let [CallArg::Positional(e)] = args else {
        discard_args(ctx, args);
        return ctx.error_expr(
            Code::BUILTIN_CALL,
            span,
            format!("`{name}` takes one place: `{name}(x)`"),
        );
    };
    let outer = std::mem::replace(&mut ctx.addr_place, true);
    let place = lower_expected(ctx, e, HirTy::Unknown);
    ctx.addr_place = outer;
    // `xs[i]` on a collection is an `Index::index` call returning a copy;
    // only fixed-array elements are places.
    let target = match &e.kind {
        ExprKind::Index(target, _) => Some(target),
        _ => None,
    };
    if let Some(target) = target.filter(|_| {
        place.ty != HirTy::Unknown && !matches!(place.kind, HirExprKind::ArrayIndex { .. })
    }) {
        let target = nabic_ast::print::print_expr(target);
        let elem = ctx.ty_name(&place.ty);
        let r = ctx.error_expr(
            Code::BUILTIN_CALL,
            span,
            format!(
                "`{name}` needs a place, but `{target}[..]` is an `Index` call that returns a \
                 copy: only an element of a fixed array `[T; N]` is a place"
            ),
        );
        ctx.help_last(Some(format!(
            "a `[..]` literal is a `List` unless its slot is a fixed array \
             (`let {target}: [{elem}; N] = [..];`)"
        )));
        return r;
    }
    let pointee = place.ty.clone();
    addr_expr(ctx, place, def, name, pointee, span)
}

/// A `bare::ptr` pointer struct `def` to `place` over `pointee`; the
/// place's indices are evaluated once, first.
pub(crate) fn addr_expr(
    ctx: &mut LowerCtx<'_>,
    place: HirExpr,
    def: &FnDef,
    name: &str,
    pointee: HirTy,
    span: nabic_diagnostic::Span,
) -> HirExpr {
    if place.ty == HirTy::Unknown {
        return place;
    }
    let mut lets = Vec::new();
    let Some(place) = addr_place(ctx, place, &mut lets) else {
        return ctx.error_expr(
            Code::BUILTIN_CALL,
            span,
            format!(
                "`{name}` needs a place: a local variable or `static`, or a field or array \
                 element of one"
            ),
        );
    };
    let ptr = def.ret_ty.as_ref().and_then(|t| match &t.kind {
        AstTyKind::Path(p) => p.segments.last().map(|s| s.name.clone()),
        _ => None,
    });
    let Some(sid) =
        ptr.and_then(|p| ctx.instantiate_struct(&format!("bare::ptr::{p}"), vec![pointee], span))
    else {
        return ctx.error_expr(
            Code::BUILTIN_CALL,
            span,
            "`nabi.addr_of` returns a `bare::ptr` pointer".to_owned(),
        );
    };
    let addr = HirExpr {
        kind: HirExprKind::AddrOf {
            place: Box::new(place),
        },
        ty: HirTy::Prim(PrimTy::Usize),
        span,
    };
    with_lets(
        lets,
        HirExpr {
            kind: HirExprKind::StructInit {
                struct_id: sid,
                fields: vec![addr],
            },
            ty: HirTy::Struct(sid),
            span,
        },
    )
}

/// `e` as a place rooted at a local or `static`, its non-trivial indices
/// moved into `lets`; `None` if it is not one.
pub(crate) fn addr_place(
    ctx: &mut LowerCtx<'_>,
    e: HirExpr,
    lets: &mut Vec<HirStmt>,
) -> Option<HirExpr> {
    let HirExpr { kind, ty, span } = e;
    let kind = match kind {
        HirExprKind::Local(_) | HirExprKind::StaticRef(_) => kind,
        HirExprKind::FieldRead {
            target,
            struct_id,
            field_index,
            field_ty,
        } if ctx.struct_def(struct_id).union_kind.is_none() => HirExprKind::FieldRead {
            target: Box::new(addr_place(ctx, *target, lets)?),
            struct_id,
            field_index,
            field_ty,
        },
        HirExprKind::TupleField {
            target,
            index,
            field_ty,
        } => HirExprKind::TupleField {
            target: Box::new(addr_place(ctx, *target, lets)?),
            index,
            field_ty,
        },
        HirExprKind::ArrayIndex {
            array,
            index,
            elem_ty,
        } => {
            let array = Box::new(addr_place(ctx, *array, lets)?);
            let index = if matches!(index.kind, HirExprKind::Local(_) | HirExprKind::Const(_)) {
                index
            } else {
                let ity = index.ty.clone();
                let local = ctx.declare(None, ity.clone());
                lets.push(HirStmt::Let {
                    local,
                    init: *index,
                });
                Box::new(hir_local(local, &ity, span))
            };
            HirExprKind::ArrayIndex {
                array,
                index,
                elem_ty,
            }
        }
        _ => return None,
    };
    Some(HirExpr { kind, ty, span })
}

/// `atomic_OP(buf, i, operands..)` over a `GcBuf<T>` of an integer or
/// `bool` `T`; yields the element's previous value.
pub(crate) fn lower_atomic_intrinsic(
    ctx: &mut LowerCtx<'_>,
    op: &'static str,
    args: &[CallArg],
    span: nabic_diagnostic::Span,
) -> HirExpr {
    let pos: Vec<&Expr> = args
        .iter()
        .filter_map(|a| match a {
            CallArg::Positional(e) => Some(e),
            _ => None,
        })
        .collect();
    // Value operands, then the ordering(s) (`cas`: success, failure).
    let (values, orders) = match op {
        "load" => (0, 1),
        "cas" => (2, 2),
        _ => (1, 1),
    };
    let want = 2 + values + orders;
    if pos.len() != want || args.len() != want {
        discard_args(ctx, args);
        return ctx.error_expr(
            Code::BUILTIN_CALL,
            span,
            format!("`atomic_{op}` takes {want} argument(s)"),
        );
    }
    // The orderings must be literal ordering variants: each instruction
    // fixes its own.
    let mut order = ["seq_cst"; 2];
    for (k, e) in pos[2 + values..].iter().enumerate() {
        let name = match &e.kind {
            ExprKind::Path(p) => p.segments.last().map(|s| s.name.as_str()),
            _ => None,
        };
        let ord = match name {
            Some("Relaxed") => "relaxed",
            Some("Acquire") => "acquire",
            Some("Release") => "release",
            Some("AcqRel") => "acq_rel",
            Some("SeqCst") => "seq_cst",
            _ => {
                discard_args(ctx, args);
                return ctx.error_expr(
                    Code::BUILTIN_CALL,
                    span,
                    format!(
                        "`atomic_{op}`: the memory ordering must be a constant ordering \
                         variant (`Relaxed`, `Acquire`, `Release`, `AcqRel`, `SeqCst`)"
                    ),
                );
            }
        };
        // A load cannot release, a store cannot acquire; a failed `cas` is
        // a load.
        let bad = if op == "store" {
            matches!(ord, "acquire" | "acq_rel")
        } else {
            (op == "load" || k == 1) && matches!(ord, "release" | "acq_rel")
        };
        if bad {
            discard_args(ctx, args);
            let what = match op {
                "load" => "a load",
                "store" => "a store",
                _ => "a failed compare-exchange",
            };
            return ctx.error_expr(
                Code::BUILTIN_CALL,
                span,
                format!(
                    "`atomic_{op}`: `{}` is not a valid ordering for {what}",
                    name.unwrap_or("")
                ),
            );
        }
        order[k] = ord;
    }
    let buf = lower_expected(ctx, pos[0], HirTy::Unknown);
    if let HirTy::GcBuf(e) = &buf.ty {
        if let Some((id, word)) = transparent_word(ctx, e) {
            if !matches!(op, "load" | "store" | "xchg" | "cas") {
                discard_args(ctx, &args[1..]);
                return ctx.error_expr(
                    Code::BUILTIN_CALL,
                    span,
                    format!("`atomic_{op}` needs an integer, found `{}`", ctx.ty_name(e)),
                );
            }
            return atomic_on_word(ctx, (op, order), buf, id, word, &pos[1..2 + values], span);
        }
    }
    let elem = match &buf.ty {
        HirTy::GcBuf(e) if matches!(**e, HirTy::Prim(p) if p.is_int() || p == PrimTy::Bool) => {
            (**e).clone()
        }
        // A type parameter: checked per instance.
        HirTy::GcBuf(e) if matches!(**e, HirTy::Param(_)) => (**e).clone(),
        HirTy::Unknown => {
            discard_args(ctx, &args[1..]);
            return buf;
        }
        other => {
            let msg = format!(
                "`atomic_{op}` needs a `GcBuf` of an integer or `bool`, found `{}`",
                ctx.ty_name(other)
            );
            discard_args(ctx, &args[1..]);
            return ctx.error_expr(Code::BUILTIN_CALL, span, msg);
        }
    };
    if elem == HirTy::Prim(PrimTy::Bool) && matches!(op, "add" | "sub") {
        discard_args(ctx, &args[1..]);
        return ctx.error_expr(
            Code::BUILTIN_CALL,
            span,
            format!("`atomic_{op}` needs an integer, found `bool`"),
        );
    }
    let usize_ty = HirTy::Prim(PrimTy::Usize);
    let i = lower_expected(ctx, pos[1], usize_ty.clone());
    let i = coerce(ctx, &usize_ty, i, &|| "the buffer index".to_owned());
    let operands = pos[2..2 + values]
        .iter()
        .map(|e| {
            let v = lower_expected(ctx, e, elem.clone());
            coerce(ctx, &elem, v, &|| "the atomic operand".to_owned())
        })
        .collect();
    HirExpr {
        ty: elem.clone(),
        kind: HirExprKind::BufAtomic {
            op,
            buf: Box::new(buf),
            index: Box::new(i),
            args: operands,
            elem,
            order,
        },
        span,
    }
}

pub(crate) fn lower_buf_intrinsic(
    ctx: &mut LowerCtx<'_>,
    op: Intrinsic,
    name: &str,
    args: &[CallArg],
    span: nabic_diagnostic::Span,
) -> HirExpr {
    let usize_ty = HirTy::Prim(PrimTy::Usize);
    let pos: Vec<&Expr> = args
        .iter()
        .filter_map(|a| match a {
            CallArg::Positional(e) => Some(e),
            _ => None,
        })
        .collect();
    let want = match op {
        Intrinsic::BufNew | Intrinsic::BufErase | Intrinsic::BufCast => 1,
        Intrinsic::BufGet | Intrinsic::BufSame => 2,
        _ => 3,
    };
    if pos.len() != want || args.len() != want {
        discard_args(ctx, args);
        return ctx.error_expr(
            Code::BUILTIN_CALL,
            span,
            format!("`{name}` takes {want} positional argument(s)"),
        );
    }
    let index = |ctx: &mut LowerCtx<'_>, e: &Expr| {
        let i = lower_expected(ctx, e, usize_ty.clone());
        coerce(ctx, &usize_ty, i, &|| "the buffer index".to_owned())
    };
    if op == Intrinsic::BufNew {
        let Some(HirTy::GcBuf(elem)) = ctx.current_expected().cloned() else {
            discard_args(ctx, args);
            return ctx.error_expr(
                Code::TYPE_ANNOTATIONS_NEEDED,
                span,
                format!("`{name}` needs its `GcBuf<T>` type from the context - annotate the slot"),
            );
        };
        let len = index(ctx, pos[0]);
        return HirExpr {
            ty: HirTy::GcBuf(elem.clone()),
            kind: HirExprKind::BufNew {
                elem: *elem,
                len: Box::new(len),
            },
            span,
        };
    }
    let buf = lower_expected(ctx, pos[0], HirTy::Unknown);
    let HirTy::GcBuf(elem) = buf.ty.clone() else {
        discard_args(ctx, &args[1..]);
        if buf.ty == HirTy::Unknown {
            return buf;
        }
        return ctx.error_expr(
            Code::BUILTIN_CALL,
            span,
            format!(
                "`{name}` needs a `GcBuf<T>`, found `{}`",
                ctx.ty_name(&buf.ty)
            ),
        );
    };
    let u8_buf = HirTy::GcBuf(Box::new(HirTy::Prim(PrimTy::U8)));
    if op == Intrinsic::BufCast {
        let Some(to @ HirTy::GcBuf(_)) = ctx.current_expected().cloned() else {
            return ctx.error_expr(
                Code::TYPE_ANNOTATIONS_NEEDED,
                span,
                format!("`{name}` needs its `GcBuf<U>` type from the context - annotate the slot"),
            );
        };
        return HirExpr {
            ty: to.clone(),
            kind: HirExprKind::Cast {
                from_ty: buf.ty.clone(),
                to_ty: to,
                operand: Box::new(buf),
            },
            span,
        };
    }
    match op {
        Intrinsic::BufSame => buf_same(ctx, buf, pos[1], span),
        // The same reference as `GcBuf<u8>`, the type of a runtime parameter
        // that takes any GC object.
        Intrinsic::BufErase => HirExpr {
            ty: u8_buf.clone(),
            kind: HirExprKind::Cast {
                from_ty: buf.ty.clone(),
                to_ty: u8_buf,
                operand: Box::new(buf),
            },
            span,
        },
        _ => buf_access(ctx, buf, *elem, pos[1], pos.get(2).copied(), span),
    }
}

/// A `#[repr(transparent)]` struct around one integer (`Ptr<T>`): its id
/// and that integer type, which atomics operate on.
pub(crate) fn transparent_word(ctx: &LowerCtx<'_>, ty: &HirTy) -> Option<(StructId, HirTy)> {
    let HirTy::Struct(id) = ty else { return None };
    let def = ctx.struct_def(*id);
    match def.fields.as_slice() {
        [f] if def.repr.transparent && matches!(f.ty, HirTy::Prim(p) if p.is_int()) => {
            Some((*id, f.ty.clone()))
        }
        _ => None,
    }
}

/// `atomic_OP` on a `GcBuf` of a transparent word struct: operate on it as
/// `GcBuf<word>`, unwrapping and rewrapping.
pub(crate) fn atomic_on_word(
    ctx: &mut LowerCtx<'_>,
    (op, order): (&'static str, [&'static str; 2]),
    buf: HirExpr,
    id: StructId,
    word: HirTy,
    rest: &[&Expr],
    span: nabic_diagnostic::Span,
) -> HirExpr {
    let elem = HirTy::Struct(id);
    let word_buf = HirTy::GcBuf(Box::new(word.clone()));
    let buf = HirExpr {
        ty: word_buf.clone(),
        kind: HirExprKind::Cast {
            from_ty: buf.ty.clone(),
            to_ty: word_buf,
            operand: Box::new(buf),
        },
        span,
    };
    let usize_ty = HirTy::Prim(PrimTy::Usize);
    let i = lower_expected(ctx, rest[0], usize_ty.clone());
    let i = coerce(ctx, &usize_ty, i, &|| "the buffer index".to_owned());
    let operands = rest[1..]
        .iter()
        .map(|e| {
            let v = lower_expected(ctx, e, elem.clone());
            let v = coerce(ctx, &elem, v, &|| "the atomic operand".to_owned());
            HirExpr {
                ty: word.clone(),
                kind: HirExprKind::FieldRead {
                    target: Box::new(v),
                    struct_id: id,
                    field_index: 0,
                    field_ty: word.clone(),
                },
                span,
            }
        })
        .collect();
    let raw = HirExpr {
        ty: word.clone(),
        kind: HirExprKind::BufAtomic {
            op,
            buf: Box::new(buf),
            index: Box::new(i),
            args: operands,
            elem: word,
            order,
        },
        span,
    };
    HirExpr {
        ty: elem,
        kind: HirExprKind::StructInit {
            struct_id: id,
            fields: vec![raw],
        },
        span,
    }
}

/// `buf_same(a, b)`: the addresses are compared where both are loaded, so
/// nothing can move in between and nothing is pinned.
pub(crate) fn buf_same(
    ctx: &mut LowerCtx<'_>,
    a: HirExpr,
    b: &Expr,
    span: nabic_diagnostic::Span,
) -> HirExpr {
    let b = lower_expected(ctx, b, a.ty.clone());
    let b = coerce(ctx, &a.ty, b, &|| "the compared buffer".to_owned());
    let usize_ty = HirTy::Prim(PrimTy::Usize);
    let addr = |e: HirExpr| HirExpr {
        ty: usize_ty.clone(),
        kind: HirExprKind::Cast {
            from_ty: e.ty.clone(),
            to_ty: usize_ty.clone(),
            operand: Box::new(e),
        },
        span,
    };
    HirExpr {
        ty: HirTy::Prim(PrimTy::Bool),
        kind: HirExprKind::PrimBinOp {
            op: PrimOp::Eq,
            lhs: Box::new(addr(a)),
            rhs: Box::new(addr(b)),
        },
        span,
    }
}

/// `buf[i]` / `buf[i] = value` on a `GcBuf<T>`: the `nabi.buf_get` /
/// `nabi.buf_set` primitive, for the intrinsics and `Index` / `IndexSet`.
pub(crate) fn buf_access(
    ctx: &mut LowerCtx<'_>,
    buf: HirExpr,
    elem: HirTy,
    idx: &Expr,
    value: Option<&Expr>,
    span: nabic_diagnostic::Span,
) -> HirExpr {
    // No bounds check: an index past the allocation reads / writes arbitrary
    // memory.
    if ctx.unsafe_depth == 0 {
        ctx.emit_error(
            Code::UNSAFE_REQUIRED,
            "indexing a `GcBuf` is unchecked and needs an `unsafe` block or `unsafe fn`".to_owned(),
        );
    }
    let usize_ty = HirTy::Prim(PrimTy::Usize);
    let i = lower_expected(ctx, idx, usize_ty.clone());
    let i = coerce(ctx, &usize_ty, i, &|| "the buffer index".to_owned());
    let Some(value) = value else {
        return HirExpr {
            ty: elem.clone(),
            kind: HirExprKind::BufGet {
                buf: Box::new(buf),
                index: Box::new(i),
                elem,
            },
            span,
        };
    };
    let v = lower_expected(ctx, value, elem.clone());
    let v = coerce(ctx, &elem, v, &|| "the stored element".to_owned());
    HirExpr {
        ty: HirTy::Unit,
        kind: HirExprKind::BufSet {
            buf: Box::new(buf),
            index: Box::new(i),
            value: Box::new(v),
            elem,
        },
        span,
    }
}

/// Lower call arguments only for their diagnostics (the call itself
/// failed to resolve).
pub(crate) fn discard_args(ctx: &mut LowerCtx<'_>, args: &[CallArg]) {
    for a in args {
        let (CallArg::Positional(e)
        | CallArg::Named { value: e, .. }
        | CallArg::Spread(e)
        | CallArg::Ref(e)) = a;
        lower_expected(ctx, e, HirTy::Unknown);
    }
}

/// A built-in literal shape (`[..]`, `{k: v}`, `{a, b}`, ranges) calls the
/// handlers registered for it; a user package's apply to its own code.
pub(crate) fn lower_shape(
    ctx: &mut LowerCtx<'_>,
    shape: nabic_parser::Shape,
    args: Vec<nabic_ast::LitArg>,
    what: &str,
    span: nabic_diagnostic::Span,
) -> HirExpr {
    let in_lib = nabic_core::in_toolchain(&ctx.current_module);
    let cands: Vec<nabic_ast::LitCand> = ctx
        .env
        .literal_shapes
        .iter()
        .filter(|h| h.shape == shape && (!in_lib || nabic_core::in_toolchain(&h.name)))
        .map(|h| nabic_ast::LitCand {
            handler: AstPath {
                segments: h
                    .name
                    .split("::")
                    .map(|n| nabic_ast::PathSegment {
                        name: n.to_owned(),
                        generics: Vec::new(),
                    })
                    .collect(),
                span,
                // Called as if from the handler's own module.
                origin: h.name.rsplit_once("::").map(|(m, _)| {
                    let m = if m == "pkg" { "" } else { m };
                    m.strip_prefix("pkg::").unwrap_or(m).to_owned()
                }),
            },
            args: args.clone(),
            priority: h.priority,
            reg: h.span,
        })
        .collect();
    if cands.is_empty() {
        return ctx.error_expr(
            Code::INVALID_LITERAL_HANDLER,
            span,
            format!("no `#[literal]` handler is registered for {what} literals"),
        );
    }
    let empty = args
        .iter()
        .any(|a| matches!(a, nabic_ast::LitArg::Many(v) if v.is_empty()));
    if empty && !ctx.infer_vars && ctx.current_expected().is_none_or(|t| *t == HirTy::Unknown) {
        ctx.emit_uninferred(format!(
            "cannot infer the type of an empty {what} literal - annotate the slot"
        ));
        return poison(span);
    }
    lower_magic(ctx, &cands, span)
}

/// The full name of a literal handler's path (`pkg::` is the root).
pub(crate) fn handler_name(c: &nabic_ast::LitCand) -> String {
    let segs: Vec<&str> = c.handler.segments.iter().map(|s| s.name.as_str()).collect();
    // `pkg@<package>::..` (nabic_driver's `literal_tables`).
    match segs.split_first() {
        Some((root, rest)) if root.starts_with(nabic_parser::PKG_ROOT) => {
            std::iter::once(&root[nabic_parser::PKG_ROOT.len()..])
                .chain(rest.iter().copied())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("::")
        }
        _ => segs.join("::"),
    }
}

/// The leaf name of the type a handler declares it returns.
pub(crate) fn handler_ret_leaf(ctx: &LowerCtx<'_>, name: &str) -> Option<String> {
    if let Some(def) = ctx.env.generic_fn(name) {
        let nabic_ast::TyKind::Path(p) = &def.ret_ty.as_ref()?.kind else {
            return None;
        };
        return p.segments.last().map(|s| s.name.clone());
    }
    ctx.env.sig(name)?;
    match &ctx.sig_of_key(name).ret_ty {
        HirTy::Struct(id) => Some(leaf_name(enum_base_name(&ctx.struct_def(*id).name)).to_owned()),
        HirTy::Enum(id) => Some(leaf_name(enum_base_name(&ctx.enum_def(*id).name)).to_owned()),
        HirTy::Prim(p) => Some(p.name().to_owned()),
        _ => None,
    }
}

/// Pick the handler (return type is the slot's, then highest `priority`;
/// still several is an error) and call it.
pub(crate) fn lower_magic(
    ctx: &mut LowerCtx<'_>,
    cands: &[nabic_ast::LitCand],
    span: nabic_diagnostic::Span,
) -> HirExpr {
    let expected = ctx.current_expected().cloned().unwrap_or(HirTy::Unknown);
    let slot_leaf = match &expected {
        HirTy::Struct(id) => Some(leaf_name(enum_base_name(&ctx.struct_def(*id).name)).to_owned()),
        HirTy::Enum(id) => Some(leaf_name(enum_base_name(&ctx.enum_def(*id).name)).to_owned()),
        // `let f: f64 = 5ms` picks the handler returning `f64`.
        HirTy::Prim(p) => Some(p.name().to_owned()),
        _ => None,
    };
    let mut live: Vec<&nabic_ast::LitCand> = cands.iter().collect();
    let typed: Vec<&nabic_ast::LitCand> = live
        .iter()
        .copied()
        .filter(|c| slot_leaf.is_some() && handler_ret_leaf(ctx, &handler_name(c)) == slot_leaf)
        .collect();
    if !typed.is_empty() {
        live = typed;
    }
    let top = live.iter().map(|c| c.priority).max().unwrap_or(0);
    live.retain(|c| c.priority == top);
    if live.len() > 1 {
        let names: Vec<String> = live
            .iter()
            .map(|c| format!("`{}`", handler_name(c)))
            .collect();
        let mut d = nabic_diagnostic::NabiDiagnostic::error(
            Code::AMBIGUOUS_LITERAL,
            span,
            format!(
                "ambiguous literal: handlers {} all match - give the slot a type \
                 or a handler `priority = N`",
                names.join(", ")
            ),
        );
        for c in &live {
            d = d.with_secondary(c.reg, "registered here");
        }
        ctx.env.sink.diagnostics.borrow_mut().push(d);
        return poison(span);
    }
    let Some(c) = live.first() else {
        return poison(span);
    };
    let name = handler_name(c);
    let def = ctx.env.generic_fn(&name);
    // The handler's last parameter is a variadic `...T`.
    let variadic_at = match def {
        Some(d) => d
            .params
            .last()
            .filter(|p| p.variadic)
            .map(|_| d.params.len() - 1),
        None => ctx
            .env
            .sig(&name)
            .filter(|s| s.variadic)
            .map(|s| s.param_tys.len() - 1),
    };
    let mut args: Vec<CallArg> = Vec::new();
    for (i, a) in c.args.iter().enumerate() {
        match a {
            nabic_ast::LitArg::One(e) => args.push(CallArg::Positional(e.clone())),
            nabic_ast::LitArg::Many(items) if variadic_at == Some(i) => {
                for it in items {
                    let a = list_item(ctx, it);
                    args.push(a);
                }
            }
            nabic_ast::LitArg::Many(items) => args.push(CallArg::Positional(Expr {
                kind: ExprKind::List(items.clone()),
                span,
            })),
        }
    }
    let call = Expr {
        kind: ExprKind::Call {
            callee: Box::new(Expr {
                kind: ExprKind::Path(c.handler.clone()),
                span,
            }),
            args,
            trailing_closure: None,
        },
        span,
    };
    // The slot type fixes the handler's type parameters: `[1, 2]` in a
    // `List<i64>` slot has `i64` items.
    if let (Some(def), Some(ret), true) = (
        def,
        def.and_then(|d| d.ret_ty.as_ref()),
        slot_leaf.is_some() && handler_ret_leaf(ctx, &name) == slot_leaf,
    ) {
        let names: nabic_core::HashSet<String> = def
            .generics
            .params
            .iter()
            .map(|gp| match gp {
                nabic_ast::GenericParam::Type { name, .. }
                | nabic_ast::GenericParam::Const { name, .. } => name.clone(),
            })
            .collect();
        let mut subst = HashMap::default();
        crate::lower::mono::unify_param_ty_with(ret, &expected, &names, &mut subst, ctx);
        ctx.explicit_generics = subst;
    }
    let mut h = lower_expected(ctx, &call, expected);
    crate::lower::consteval::fold_literal(ctx, &mut h, span);
    h
}

/// An AST stand-in (`#sN` path) for lowered `h`, so a desugar can place it
/// without re-lowering. Each use evaluates `h` again: use it once.
pub(crate) fn splice(ctx: &mut LowerCtx<'_>, h: HirExpr) -> Expr {
    let span = h.span;
    let name = format!("#s{}", ctx.splices.len());
    ctx.splices.insert(name.clone(), h);
    Expr {
        kind: ExprKind::Path(nabic_ast::Path {
            segments: vec![nabic_ast::PathSegment {
                name,
                generics: Vec::new(),
            }],
            span,
            origin: None,
        }),
        span,
    }
}

/// A signed index counts from the end when negative; an open literal is
/// as signed as the type it holds.
pub(crate) fn is_signed(ctx: &LowerCtx<'_>, ty: &HirTy) -> bool {
    matches!(crate::lower::infer::zonk(ctx, ty), HirTy::Prim(p) if p.is_signed_int())
}

/// An index / slice bound in a `usize` position slot: a negative
/// untyped literal (`xs[-1]`) is an `isize`.
pub(crate) fn lower_position(ctx: &mut LowerCtx<'_>, idx: &Expr) -> HirExpr {
    let neg = matches!(&idx.kind, ExprKind::OpPrefix { op, .. } if op == "-");
    let want = if neg && is_untyped_literal(idx) {
        PrimTy::Isize
    } else {
        PrimTy::Usize
    };
    lower_expected(ctx, idx, HirTy::Prim(want))
}
