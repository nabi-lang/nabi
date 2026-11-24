//! Expressions: the dispatcher and small HIR builders.

mod args;
mod assign;
mod call;
mod control;
mod dispatch;
mod drop;
mod generic;
mod index;
mod intrinsic;
mod literal;
mod method;
mod narrow;
mod ops;
mod overload;
mod path;
mod refine;
mod stmt;
mod structs;
mod sum;
mod variant;

pub(crate) use crate::lower::expr::args::*;
pub(crate) use crate::lower::expr::assign::*;
pub use crate::lower::expr::call::*;
pub(crate) use crate::lower::expr::control::*;
pub(crate) use crate::lower::expr::dispatch::*;
pub(crate) use crate::lower::expr::drop::*;
pub(crate) use crate::lower::expr::generic::*;
pub(crate) use crate::lower::expr::index::*;
pub(crate) use crate::lower::expr::intrinsic::*;
pub(crate) use crate::lower::expr::literal::*;
pub(crate) use crate::lower::expr::method::*;
pub(crate) use crate::lower::expr::narrow::*;
pub(crate) use crate::lower::expr::ops::*;
pub(crate) use crate::lower::expr::overload::*;
pub(crate) use crate::lower::expr::path::*;
pub(crate) use crate::lower::expr::refine::*;
pub(crate) use crate::lower::expr::stmt::*;
pub(crate) use crate::lower::expr::structs::*;
pub use crate::lower::expr::sum::*;
pub(crate) use crate::lower::expr::variant::*;

use crate::lower::bignum::Big;
use crate::lower::collect::Intrinsic;
#[allow(clippy::wildcard_imports)]
use crate::lower::*;
use crate::walk::AstVisitor;
use PrimTy as P;
use nabic_diagnostic::Code;
use nabic_parser::Shape;

pub(crate) fn lower_expr(ctx: &mut LowerCtx<'_>, expr: &Expr) -> HirExpr {
    // A tuple / list literal into a sum: the member of its shape.
    if let Some(m) = ctx
        .current_expected()
        .and_then(|t| crate::lower::sum_member_for(ctx, expr, t))
    {
        return lower_expected(ctx, expr, m);
    }
    // Names resolve where the expression's head was written; its operands
    // set their own.
    let outer = ctx.set_access(head_origin(expr));
    let e = ctx.at_span(expr.span, |ctx| match expr.kind {
        ExprKind::While { .. } | ExprKind::For { .. } | ExprKind::Loop { .. } => {
            lower_loop(ctx, expr)
        }
        ExprKind::Block(ref b) if b.label.is_some() => lower_loop(ctx, expr),
        _ => lower_expr_inner(ctx, expr),
    });
    // Only a literal's open variable is left in the type.
    let mut e = e;
    if crate::lower::infer::has_var(ctx, &e.ty) {
        e.ty = crate::lower::infer::resolve(ctx, &e.ty);
    }
    ctx.access.replace(outer);
    guard_flow(ctx, expr, &e);
    if e.ty != HirTy::Never || hir_expr_diverges(&e) {
        return e;
    }
    // A call of a `-> !` function: emit it as a never-returning call
    // (MIR: call + `unreachable`) typed as the slot it sits in.
    let span = e.span;
    let ty = match ctx.current_expected() {
        Some(t) if *t != HirTy::Unknown => t.clone(),
        _ => HirTy::Never,
    };
    HirExpr {
        kind: HirExprKind::Block(Box::new(HirBlock {
            stmts: vec![HirStmt::Panic(HirExpr {
                ty: HirTy::Unit,
                ..e
            })],
            trailing: None,
            label: None,
        })),
        ty,
        span,
    }
}

/// The definition module of the name `expr` resolves first: a path, a
/// called path, a struct literal's path, a method name.
fn head_origin(expr: &Expr) -> Option<&str> {
    match &expr.kind {
        ExprKind::Path(p) | ExprKind::StructInit { path: p, .. } => p.origin.as_deref(),
        ExprKind::Call { callee, .. } => match &callee.kind {
            ExprKind::Path(p) => p.origin.as_deref(),
            _ => None,
        },
        ExprKind::MethodCall { origin, .. } => origin.as_deref(),
        _ => None,
    }
}

/// `self` in a function without it: Python's method that forgot `self`.
fn self_outside_method(ctx: &LowerCtx<'_>, span: nabic_diagnostic::Span) -> HirExpr {
    let e = ctx.error_expr(
        Code::OUTSIDE_CONTEXT,
        span,
        "`self` is only available inside methods".to_owned(),
    );
    ctx.help_last(Some(
        "a method takes `self` first: `fn f(self, ..)` in an `impl` (`ref self` to change \
         its fields)"
            .to_owned(),
    ));
    e
}

fn lower_expr_inner(ctx: &mut LowerCtx<'_>, expr: &Expr) -> HirExpr {
    let span = expr.span;
    match &expr.kind {
        ExprKind::Literal(AstLit::ByteStr(body)) => lower_byte_str(ctx, body, span),
        ExprKind::Literal(lit) => crate::lower::half::literal(ctx, lit, false, span)
            .or_else(|| crate::lower::bignum::literal(ctx, lit, false, span))
            .unwrap_or_else(|| lower_literal(ctx, lit, false, span)),
        ExprKind::Path(p) => lower_path(ctx, p, span),
        ExprKind::QPath(q) => lower_qpath_value(ctx, expr, q, span),
        ExprKind::SelfLower => match ctx.lookup("self") {
            Some(local) => ctx.read_local(local, span),
            None => self_outside_method(ctx, span),
        },
        ExprKind::SelfUpper => ctx.error_expr(
            Code::WRONG_ITEM_FORM,
            span,
            "`Self` is a type, not a value".to_owned(),
        ),
        ExprKind::Unsafe(b) => {
            ctx.unsafe_depth += 1;
            let block = Expr {
                kind: ExprKind::Block(b.clone()),
                span,
            };
            let e = lower_expr_inner(ctx, &block);
            ctx.unsafe_depth -= 1;
            e
        }
        ExprKind::ConstBlock(b) => crate::lower::consteval::const_block(ctx, b, span),
        ExprKind::Block(b) => {
            let labelled = b.label.is_some();
            let idx = ctx.breakables.len();
            if labelled {
                open_break_target(ctx, idx);
                ctx.breakables.push((b.label.clone(), false));
            }
            let block = lower_block(ctx, b);
            // A labelled block's value may come from `break 'label value`.
            let break_ty = if labelled {
                ctx.breakables.pop();
                ctx.break_tys.remove(&idx).map(|(t, _)| t)
            } else {
                None
            };
            let ty = block
                .trailing
                .as_ref()
                .map_or_else(|| break_ty.clone().unwrap_or(HirTy::Unit), |e| e.ty.clone());
            HirExpr {
                kind: HirExprKind::Block(Box::new(block)),
                ty,
                span,
            }
        }
        ExprKind::OpInfix { op, .. } if crate::lower::broadcast::scalar_op(op).is_some() => {
            crate::lower::broadcast::lower(ctx, expr)
        }
        ExprKind::OpInfix { op, lhs, rhs } => eq_on_type(ctx, op, lhs, rhs, span)
            .unwrap_or_else(|| lower_infix(ctx, op, lhs, rhs, span)),
        ExprKind::OpPrefix { op, operand } => lower_prefix(ctx, op, operand, span),
        ExprKind::OpPostfix { op, operand } => {
            let operand_h = lower_expr(ctx, operand);
            if operand_h.ty == HirTy::Unknown {
                return operand_h;
            }
            dispatch_user_op(ctx, op, Fixity::Postfix, operand_h, None, span)
        }
        ExprKind::Call {
            callee,
            args,
            trailing_closure,
        } => {
            // Trailing closure becomes the last arg.
            let all_args = with_trailing_closure(args, trailing_closure.as_deref());
            if let ExprKind::Path(p) = &callee.kind {
                check_direct_drop_path(ctx, p);
            }
            if let Some(target) = crate::lower::fstring::print_target(ctx, callee) {
                return crate::lower::fstring::lower_print(
                    ctx,
                    target,
                    callee.span,
                    &all_args,
                    span,
                );
            }
            if let Some(e) = crate::lower::fstring::lower_dbg(ctx, callee, &all_args, span) {
                return e;
            }
            let call = match &callee.kind {
                ExprKind::QPath(q) => qpath_call(ctx, q, &all_args, span),
                _ => lower_call(ctx, callee, &all_args, span),
            };
            unpack_call(ctx, call)
        }
        ExprKind::MethodCall { .. } => lower_method_call(ctx, expr),
        ExprKind::If {
            cond,
            then_branch,
            else_branch,
        } => lower_if(ctx, cond, then_branch, else_branch.as_deref(), span),
        ExprKind::While { label, cond, body } => {
            let cond_h = lower_cond(ctx, cond);
            let idx = open_unit_loop(ctx, label.as_ref());
            ctx.push_expected(HirTy::Unit);
            let body_h = lower_narrowed_block(ctx, cond, body);
            ctx.pop_expected();
            close_unit_loop(ctx, idx);
            HirExpr {
                kind: HirExprKind::While {
                    label: label.clone(),
                    cond: Box::new(cond_h),
                    body: Box::new(body_h),
                },
                ty: HirTy::Unit,
                span,
            }
        }
        ExprKind::For {
            label,
            pat,
            iter,
            body,
            is_await,
            by_ref,
        } => literal_for(ctx, expr).unwrap_or_else(|| {
            lower_for(
                ctx,
                label.as_ref(),
                pat,
                iter,
                body,
                (*is_await, *by_ref),
                span,
            )
        }),
        ExprKind::Match { scrutinee, arms } => {
            guard_sink(ctx, scrutinee);
            lower_match(ctx, scrutinee, arms, span)
        }
        ExprKind::Try { kind, operand } => lower_try(ctx, *kind, operand, span),
        ExprKind::Catch {
            body,
            catches,
            finally,
        } => lower_catch(ctx, body, catches, finally.as_ref(), span),
        ExprKind::StructInit {
            path,
            fields,
            spread,
        } => lower_struct_init(ctx, path, fields, spread.as_deref(), span),
        ExprKind::Field(target, name) => lower_field(ctx, target, name, span),
        ExprKind::Tuple(items) if items.is_empty() => HirExpr {
            kind: HirExprKind::Const(ConstVal::Unit),
            ty: HirTy::Unit,
            span,
        },
        ExprKind::Tuple(items) => {
            let expected: Option<Vec<HirTy>> = match ctx.current_expected() {
                Some(HirTy::Tuple(ts)) if ts.len() == items.len() => Some(ts.clone()),
                _ => None,
            };
            let lowered: Vec<HirExpr> = items
                .iter()
                .enumerate()
                .map(|(i, e)| {
                    let slot = expected.as_ref().map_or(HirTy::Unknown, |ts| ts[i].clone());
                    let v = lower_expected(ctx, e, slot.clone());
                    wrap_into_sum(&slot, v)
                })
                .collect();
            let tys: Vec<HirTy> = lowered.iter().map(|e| e.ty.clone()).collect();
            HirExpr {
                ty: HirTy::Tuple(tys),
                span,
                kind: HirExprKind::Tuple { fields: lowered },
            }
        }
        ExprKind::TupleField(target, index) => {
            let target_h = lower_expected(ctx, target, HirTy::Unknown);
            let i = *index as usize;
            match target_h.ty.clone() {
                HirTy::Tuple(ts) => match ts.get(i) {
                    Some(field_ty) => HirExpr {
                        ty: field_ty.clone(),
                        span,
                        kind: HirExprKind::TupleField {
                            target: Box::new(target_h),
                            index: *index,
                            field_ty: field_ty.clone(),
                        },
                    },
                    None => ctx.error_expr(
                        Code::NO_FIELD,
                        span,
                        format!("tuple index {index} out of range for a {}-tuple", ts.len()),
                    ),
                },
                // Tuple structs (`struct Foo(i32);`) expose their fields as `.N`.
                HirTy::Struct(sid) => {
                    let def = ctx.struct_def(sid);
                    match def.fields.get(i) {
                        Some(f) if f.name == i.to_string() => {
                            ctx.check_field_visible(sid, &f.name);
                            HirExpr {
                                ty: f.ty.clone(),
                                span,
                                kind: HirExprKind::FieldRead {
                                    target: Box::new(target_h),
                                    struct_id: sid,
                                    field_index: *index,
                                    field_ty: f.ty.clone(),
                                },
                            }
                        }
                        _ => ctx.error_expr(
                            Code::NO_FIELD,
                            span,
                            format!("struct `{}` has no field `{index}`", def.name),
                        ),
                    }
                }
                HirTy::Unknown => target_h,
                other => ctx.error_expr(
                    Code::NO_FIELD,
                    span,
                    format!("no field `{index}` on type `{}`", ctx.ty_name(&other)),
                ),
            }
        }
        ExprKind::List(items) => lower_list_lit(ctx, items, span),
        ExprKind::ArrayRepeat { value, len, fill } => {
            lower_array_repeat(ctx, value, len, *fill, span)
        }
        // `d[k, default: v]` read: `d.get_or(k, v)`, the dict unchanged.
        ExprKind::Index(d, idx) if nabic_ast::as_index_default(idx).is_some() => {
            lower_expr(ctx, &index_default_call(d, idx, "get_or"))
        }
        // `xs[a..b]` / `xs[..]` (range spelling) is `xs[a:b]` / `xs[:]`.
        ExprKind::Index(arr, idx)
            if matches!(
                &idx.kind,
                ExprKind::Range {
                    inclusive: false,
                    ..
                }
            ) =>
        {
            let ExprKind::Range { start, end, .. } = &idx.kind else {
                unreachable!()
            };
            lower_slice(ctx, arr, start.as_deref(), end.as_deref(), None, span)
        }
        ExprKind::Index(arr, idx) => {
            let arr_h = match axis_index_read(ctx, arr, idx, span) {
                Ok(e) => return e,
                Err(arr_h) => arr_h,
            };
            // A struct / enum indexes through `Index::index`.
            if matches!(arr_h.ty, HirTy::Struct(_) | HirTy::Enum(_)) {
                return index_call(ctx, arr_h, "index", idx, &[], span);
            }
            // `GcBuf<T>`'s `Index` is the unchecked `nabi.buf_get` primitive.
            if let HirTy::GcBuf(elem) = arr_h.ty.clone() {
                return buf_access(ctx, arr_h, *elem, idx, None, span);
            }
            let idx_h = match arr_h.ty {
                HirTy::Array(_, n) => array_position(ctx, idx, n),
                _ => lower_expected(ctx, idx, HirTy::Unknown),
            };
            let elem_ty = match &arr_h.ty {
                HirTy::Array(t, _) => (**t).clone(),
                HirTy::Unknown => return arr_h,
                other => {
                    let e = ctx.error_expr(
                        Code::INVALID_INDEX,
                        span,
                        format!("cannot index into a value of type `{}`", ctx.ty_name(other)),
                    );
                    if *other == HirTy::Prim(PrimTy::Str) {
                        str_index_help(ctx, arr, idx);
                    }
                    tuple_index_help(ctx, other, arr, idx);
                    return e;
                }
            };
            HirExpr {
                ty: elem_ty.clone(),
                span,
                kind: HirExprKind::ArrayIndex {
                    array: Box::new(arr_h),
                    index: Box::new(idx_h),
                    elem_ty,
                },
            }
        }
        ExprKind::Loop { label, body } => {
            let idx = ctx.breakables.len();
            open_break_target(ctx, idx);
            ctx.breakables.push((label.clone(), true));
            let body_h = lower_loop_body(ctx, body);
            ctx.breakables.pop();
            let result_ty = ctx.break_tys.remove(&idx).map_or(HirTy::Unit, |(t, _)| t);
            // No `break` out: `loop {}` has type `!`.
            let ty = if block_breaks_out(&body_h, label.as_deref(), true) {
                result_ty.clone()
            } else {
                HirTy::Never
            };
            HirExpr {
                kind: HirExprKind::Loop {
                    label: label.clone(),
                    body: Box::new(body_h),
                    result_ty,
                },
                ty,
                span,
            }
        }
        ExprKind::Closure {
            captures,
            params,
            ret_ty,
            body,
            is_async,
            is_generator,
        } => {
            let body = crate::lower::coro::closure_body(body, *is_async, *is_generator);
            lower_closure_expr(ctx, captures, params, ret_ty.as_ref(), &body, span)
        }
        ExprKind::QuestionDot(receiver, tails) => {
            let recv_h = lower_expected(ctx, receiver, HirTy::Unknown);
            desugar_question_dot(ctx, recv_h, tails, span)
        }
        ExprKind::TryQ(inner) => {
            let since = ctx.env.sink.diagnostics.borrow().len();
            let inner_h = lower_expected(ctx, inner, HirTy::Unknown);
            lower_try_q(ctx, inner, inner_h, since, span)
        }
        ExprKind::NullishCoalesce(lhs, rhs) => {
            let lhs_h = lower_expected(ctx, lhs, HirTy::Unknown);
            desugar_nullish_coalesce(ctx, lhs_h, rhs, span)
        }
        ExprKind::Cast(operand, ty) => lower_cast(ctx, operand, ty, span),
        ExprKind::Await(inner) => crate::lower::coro::lower_await(ctx, inner, span),
        ExprKind::InModule(module, inner) => {
            let module = std::mem::replace(&mut ctx.current_module, module.clone());
            let scopes = std::mem::replace(&mut ctx.scopes, vec![HashMap::default()]);
            let e = lower_expr(ctx, inner);
            ctx.scopes = scopes;
            ctx.current_module = module;
            e
        }
        ExprKind::Yield(value) => crate::lower::coro::lower_yield(ctx, value.as_deref(), span),
        ExprKind::Range {
            start,
            end,
            inclusive,
        } => lower_range_value(ctx, start.as_deref(), end.as_deref(), *inclusive, span),
        ExprKind::Dict(pairs) => {
            let items: Vec<Expr> = pairs
                .iter()
                .map(|(k, v)| Expr {
                    kind: ExprKind::Tuple(vec![k.clone(), v.clone()]),
                    span: k.span,
                })
                .collect();
            let args = vec![nabic_ast::LitArg::Many(items)];
            lower_shape(ctx, nabic_parser::Shape::Dict, args, "`Dict`", span)
        }
        ExprKind::Set(xs) => {
            let args = vec![nabic_ast::LitArg::Many(xs.clone())];
            lower_shape(ctx, nabic_parser::Shape::Set, args, "`Set`", span)
        }
        ExprKind::MagicLiteral(cands) => lower_magic(ctx, cands, span),
        ExprKind::Is(operand, pat) => lower_is(ctx, operand, pat, span),
        ExprKind::Slice { .. } | ExprKind::AxisSlice { .. } => {
            lower_slice_form(ctx, &expr.kind, span)
        }
        ExprKind::AsyncBlock(_) | ExprKind::YieldBlock(_) | ExprKind::AsyncYieldBlock(_) => {
            crate::lower::coro::lower_coro_expr(ctx, &expr.kind, span)
        }
        ExprKind::Broadcast { .. } => crate::lower::broadcast::lower(ctx, expr),
        ExprKind::FString(parts) => crate::lower::fstring::lower_fstring_expr(ctx, parts, span),
    }
}

/// Two operands that share a type: an untyped literal takes the other
/// side's. Left to right, unless the left is a literal (no effects).
pub(crate) fn lower_sibling_pair(
    ctx: &mut LowerCtx<'_>,
    lhs: &Expr,
    rhs: &Expr,
    lhs_expected: HirTy,
) -> (HirExpr, HirExpr) {
    if is_untyped_literal(lhs) && !is_untyped_literal(rhs) {
        let r = lower_expected(ctx, rhs, lhs_expected);
        let l = lower_expected(ctx, lhs, r.ty.clone());
        (l, r)
    } else {
        let l = lower_expected(ctx, lhs, lhs_expected);
        let r = lower_expected(ctx, rhs, l.ty.clone());
        (l, r)
    }
}

pub(crate) fn hir_local(local: LocalId, ty: &HirTy, span: nabic_diagnostic::Span) -> HirExpr {
    HirExpr {
        kind: HirExprKind::Local(local),
        ty: ty.clone(),
        span,
    }
}

pub(crate) const fn hir_int(value: i128, prim: PrimTy, span: nabic_diagnostic::Span) -> HirExpr {
    HirExpr {
        kind: HirExprKind::Const(ConstVal::Int { value, ty: prim }),
        ty: HirTy::Prim(prim),
        span,
    }
}

pub(crate) fn hir_binop(
    op: PrimOp,
    lhs: HirExpr,
    rhs: HirExpr,
    ty: HirTy,
    span: nabic_diagnostic::Span,
) -> HirExpr {
    HirExpr {
        kind: HirExprKind::PrimBinOp {
            op,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        },
        ty,
        span,
    }
}

/// `t[0]` on a tuple: its parts are fields, `t.0`.
fn tuple_index_help(ctx: &LowerCtx<'_>, ty: &HirTy, arr: &Expr, idx: &Expr) {
    if let (HirTy::Tuple(_), ExprKind::Literal(AstLit::Int(n))) = (ty, &idx.kind) {
        let t = ctx.src(arr.span).unwrap_or("t");
        ctx.help_last(Some(format!("a tuple's parts are fields: write `{t}.{n}`")));
    }
}
