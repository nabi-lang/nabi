//! Value-based errors: `throws`, `throw`, `try` / `try?` /
//! `try!`, `catch` / `finally`.
//!
//! `throws (E) -> T` returns the carrier `HirTy::Throws { ok, err }` (MIR:
//! `is_err` plus one payload). Nothing unwinds: `try` tests
//! `is_err` and `raise`s the payload through `err_frames` innermost first,
//! running `finally` blocks inline until a `catch` that may take the type
//! parks the value and breaks to its dispatch; with no frame left the
//! function returns the error. `return` / `break` / `continue` run the
//! `finally` blocks they leave the same way.

mod adapt;
mod catch;
mod uncaught;

pub(crate) use crate::lower::throws::adapt::*;
pub(crate) use crate::lower::throws::catch::*;
pub(crate) use crate::lower::throws::uncaught::*;

use crate::lower::desugar::{optional_parts, sum_none};
#[allow(clippy::wildcard_imports)]
use crate::lower::*;
use nabic_ast::TryKind;
use nabic_diagnostic::Code;

#[derive(Clone)]
pub(crate) enum ErrFrame {
    Catch(CatchFrame),
    Finally {
        block: Block,
        /// Lowered statements run after `block` (a deferred `ref`
        /// write-back around an `await`).
        hir: Vec<HirStmt>,
        /// `try` nesting and breakable count where the `finally` sits;
        /// its block is lowered in that context at every exit.
        try_depth: u32,
        breakables: usize,
    },
}

#[derive(Clone)]
pub(crate) struct CatchFrame {
    /// The catch body runs in a block with this label; a raise breaks
    /// out of it.
    label: String,
    /// Error type each `catch` arm takes (`None`: any) and the local saying
    /// which parked error was raised; `None` for `try?` / `try!` / throwing
    /// `main`, which take all.
    arms: Option<(Vec<Option<HirTy>>, LocalId)>,
    /// Distinct error types raised into this frame, each parked in its
    /// own local.
    errs: Vec<(HirTy, LocalId)>,
    /// Some raise reached this frame.
    hit: bool,
    /// Every error type raised into this frame (`try?` checks what it
    /// drops).
    raised: Vec<HirTy>,
    /// Throwing `main`'s frame: a `str` local each raise sets to its
    /// position, for the uncaught-error report.
    site: Option<LocalId>,
}

impl CatchFrame {
    fn takes(&self, ety: &HirTy) -> bool {
        self.arms
            .as_ref()
            .is_none_or(|(filters, _)| filters.iter().any(|f| may_take(f.as_ref(), ety)))
    }
}

/// Whether a catch arm of `pat` (filter `filter`) takes every error of
/// type `ety`: `_`, a binding, or `e: T` with `T` covering `ety`.
fn takes_all(pat: &AstPattern, filter: Option<&HirTy>, ety: &HirTy) -> bool {
    match &pat.kind {
        AstPatternKind::Wildcard | AstPatternKind::Binding { sub: None, .. } => true,
        AstPatternKind::TypeCheck { .. } => {
            let Some(f) = filter else {
                return false;
            };
            let parts = |t: &HirTy| match t {
                HirTy::Sum(vs) => vs.clone(),
                t => vec![t.clone()],
            };
            let fs = parts(f);
            !matches!(ety, HirTy::AnyTrait(_)) && parts(ety).iter().all(|p| fs.contains(p))
        }
        _ => false,
    }
}

/// Can an arm filtering `filter` (`None`: any) take `ety`? Unresolved
/// filters are kept so lowering the arm reports them.
fn may_take(filter: Option<&HirTy>, ety: &HirTy) -> bool {
    let parts = |t: &HirTy| match t {
        HirTy::Sum(vs) => vs.clone(),
        t => vec![t.clone()],
    };
    match filter {
        None
        | Some(
            HirTy::Path(_)
            | HirTy::None
            | HirTy::NoThrow
            | HirTy::OpenConst(_)
            | HirTy::Var(_)
            | HirTy::Unknown,
        ) => true,
        // An untyped `throws` error (`any Error`) may hold any type.
        Some(_) if matches!(ety, HirTy::AnyTrait(_)) => true,
        Some(f) => {
            let ps = parts(ety);
            parts(f).iter().any(|x| ps.contains(x))
        }
    }
}

/// The AST return type callers see: `ret_ty` (unit when absent),
/// wrapped in `Throws` when the fn declares `throws (E)`.
pub fn sig_ret_ast(f: &FnDef) -> Ty {
    let ok = f.ret_ty.clone().unwrap_or(Ty {
        kind: AstTyKind::Tuple(Vec::new()),
        span: f.span,
    });
    let nabic_ast::ThrowsKind::Typed(errs) = &f.throws else {
        return ok;
    };
    let err = match errs.as_slice() {
        [] => return ok,
        [one] => one.clone(),
        many => Ty {
            kind: AstTyKind::Sum(many.to_vec()),
            span: f.span,
        },
    };
    Ty {
        span: ok.span,
        kind: AstTyKind::Throws {
            ok: Box::new(ok),
            err: Box::new(err),
        },
    }
}

/// Unbound error parameters (`#E0` or plain `E`) -> `Unknown`, so a
/// `fn(i32) throws (E) -> i32` hint still types a closure argument.
pub(crate) fn erase_unbound_errors(ty: &mut HirTy) {
    match ty {
        HirTy::Throws { ok, err } => {
            let unbound = |t: &HirTy| {
                matches!(
                    t,
                    HirTy::Path(_)
                        | HirTy::None
                        | HirTy::NoThrow
                        | HirTy::OpenConst(_)
                        | HirTy::Var(_)
                )
            };
            if unbound(err) || matches!(&**err, HirTy::Sum(vs) if vs.iter().any(unbound)) {
                **err = HirTy::Unknown;
            }
            erase_unbound_errors(ok);
        }
        HirTy::Fn(args, ret, _) | HirTy::Closure { args, ret, .. } => {
            args.iter_mut().for_each(erase_unbound_errors);
            erase_unbound_errors(ret);
        }
        HirTy::Sum(ts) | HirTy::Tuple(ts) => ts.iter_mut().for_each(erase_unbound_errors),
        _ => {}
    }
}

fn fresh_label(ctx: &mut LowerCtx<'_>) -> String {
    ctx.next_synth += 1;
    format!("#catch{}", ctx.next_synth)
}

pub(crate) const fn unit_value(span: nabic_diagnostic::Span) -> HirExpr {
    HirExpr {
        kind: HirExprKind::Const(ConstVal::Unit),
        ty: HirTy::Unit,
        span,
    }
}

/// Placeholder for the unused payload slot of a carrier (codegen
/// materializes zero / undef).
fn undef(ty: &HirTy, span: nabic_diagnostic::Span) -> HirExpr {
    HirExpr {
        kind: HirExprKind::Const(ConstVal::Unit),
        ty: ty.clone(),
        span,
    }
}

fn block_expr(stmts: Vec<HirStmt>, span: nabic_diagnostic::Span) -> HirExpr {
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

/// A `()` / `!` ok side has no carrier slot (`nabic_mir`'s layout).
pub(crate) const fn no_ok_slot(ok: &HirTy) -> bool {
    matches!(ok, HirTy::Unit | HirTy::Never)
}

/// Carrier field index of the error payload.
const fn err_index(ok: &HirTy) -> u32 {
    if no_ok_slot(ok) { 1 } else { 2 }
}

/// Build a carrier: `Ok(v)` -> `(false, v, _)`, `Err(e)` -> `(true, _, e)`.
pub(crate) fn carrier(
    ok: &HirTy,
    err: &HirTy,
    value: Result<HirExpr, HirExpr>,
    span: nabic_diagnostic::Span,
) -> HirExpr {
    let (is_err, ok_v, err_v) = match value {
        Ok(v) => (false, v, undef(err, span)),
        Err(e) => (true, undef(ok, span), e),
    };
    let mut lets = Vec::new();
    let mut fields = vec![HirExpr {
        kind: HirExprKind::Const(ConstVal::Bool(is_err)),
        ty: HirTy::Prim(PrimTy::Bool),
        span,
    }];
    if no_ok_slot(ok) {
        // No slot, but keep the value's side effects.
        if !matches!(ok_v.kind, HirExprKind::Const(_)) {
            lets.push(HirStmt::Expr(ok_v));
        }
    } else {
        fields.push(ok_v);
    }
    fields.push(err_v);
    let ty = HirTy::Throws {
        ok: Box::new(ok.clone()),
        err: Box::new(err.clone()),
    };
    crate::lower::expr::with_lets(
        lets,
        HirExpr {
            kind: HirExprKind::Tuple { fields },
            ty,
            span,
        },
    )
}

/// A call whose type is a `throws` carrier: `{ let t = call; if
/// t.is_err { raise(t.err) } t.ok }`. It must sit under a `try`.
pub(crate) fn unpack_call(ctx: &mut LowerCtx<'_>, call: HirExpr) -> HirExpr {
    let HirTy::Throws { ok, err } = call.ty.clone() else {
        return call;
    };
    ctx.throwing_calls += 1;
    let span = call.span;
    let carrier_ty = call.ty.clone();
    let tmp = ctx.declare(None, carrier_ty.clone());
    let field = |index: u32, ty: &HirTy| HirExpr {
        kind: HirExprKind::TupleField {
            target: Box::new(hir_local(tmp, &carrier_ty, span)),
            index,
            field_ty: ty.clone(),
        },
        ty: ty.clone(),
        span,
    };
    let mut stmts = vec![HirStmt::Let {
        local: tmp,
        init: call,
    }];
    if ctx.try_depth == 0 {
        let mut d = nabic_diagnostic::NabiDiagnostic::error(
            Code::UNHANDLED_ERROR,
            ctx.here(),
            format!(
                "this call can throw `{}` and must be marked with `try` (`try f()`; \
                 `try? f()` / `try! f()` to drop / panic on the error)",
                ctx.ty_name(&err)
            ),
        );
        if ctx.in_try_for_body > 0 {
            d = d.with_help(
                "the `try` of `try for` covers the iteration, not the loop body".to_owned(),
            );
        }
        // A call written in the source (not one lowering made up).
        if ctx.src(span).is_some_and(|s| s.ends_with(')')) {
            let at = nabic_diagnostic::Span {
                end: span.start,
                ..span
            };
            d = d.with_fix(at, "try ", "mark the call with `try`");
        }
        ctx.emit_diag(d);
    } else {
        let raised = raise(ctx, field(err_index(&ok), &err), span);
        stmts.push(HirStmt::Expr(HirExpr {
            kind: HirExprKind::If {
                cond: Box::new(field(0, &HirTy::Prim(PrimTy::Bool))),
                then_block: Box::new(HirBlock {
                    stmts: raised,
                    trailing: None,
                    label: None,
                }),
                else_branch: None,
            },
            ty: HirTy::Unit,
            span,
        }));
    }
    let trailing = (!no_ok_slot(&ok)).then(|| field(1, &ok));
    HirExpr {
        kind: HirExprKind::Block(Box::new(HirBlock {
            stmts,
            trailing,
            label: None,
        })),
        ty: *ok,
        span,
    }
}

/// Route error value `err` from the current point (see module doc).
/// The returned statements never complete normally.
pub(crate) fn raise(
    ctx: &mut LowerCtx<'_>,
    err: HirExpr,
    span: nabic_diagnostic::Span,
) -> Vec<HirStmt> {
    raise_at(ctx, err, span, None)
}

/// `raise` from a `throw` at `site` (a `str`): noted for a report when the
/// error leaves the function or reaches `try!` / throwing `main`; a
/// handler clears it.
fn raise_at(
    ctx: &mut LowerCtx<'_>,
    err: HirExpr,
    span: nabic_diagnostic::Span,
    site: Option<&HirExpr>,
) -> Vec<HirStmt> {
    let ety = err.ty.clone();
    let tmp = ctx.declare(None, ety.clone());
    let mut out = vec![HirStmt::Let {
        local: tmp,
        init: err,
    }];
    let value = hir_local(tmp, &ety, span);
    if ety == HirTy::Unknown {
        out.push(HirStmt::Return(None));
        return out;
    }
    // Under `catch` arms a sum error is raised variant by variant, so
    // each variant finds its own handler (or propagates on its own).
    if let HirTy::Sum(variants) = &ety {
        let has_arms = ctx
            .err_frames
            .iter()
            .any(|f| matches!(f, ErrFrame::Catch(c) if c.arms.is_some()));
        if has_arms {
            let arms = variants
                .iter()
                .enumerate()
                .map(|(i, v)| {
                    let b = ctx.declare(None, v.clone());
                    let body = block_expr(raise_at(ctx, hir_local(b, v, span), span, site), span);
                    HirMatchArm {
                        pat: HirPattern::SumVariant {
                            sum_ty: ety.clone(),
                            variant_index: u32::try_from(i).expect("variant overflow"),
                            binder: Some(b),
                            payload_ty: Some(v.clone()),
                        },
                        guard: None,
                        body,
                    }
                })
                .collect();
            out.push(HirStmt::Expr(HirExpr {
                kind: HirExprKind::Match {
                    scrutinee: Box::new(value),
                    arms,
                },
                ty: HirTy::Unit,
                span,
            }));
            return out;
        }
    }
    for i in (0..ctx.err_frames.len()).rev() {
        let (label, which) = match &ctx.err_frames[i] {
            ErrFrame::Finally { .. } => {
                out.push(run_finally(ctx, i));
                continue;
            }
            ErrFrame::Catch(c) if c.takes(&ety) => (c.label.clone(), c.arms.as_ref().map(|a| a.1)),
            ErrFrame::Catch(_) => continue,
        };
        if let Some(which) = which {
            let parked = match &ctx.err_frames[i] {
                ErrFrame::Catch(c) => c.errs.iter().position(|(t, _)| *t == ety),
                ErrFrame::Finally { .. } => None,
            };
            let k = parked.unwrap_or_else(|| {
                let local = ctx.declare(None, ety.clone());
                let ErrFrame::Catch(c) = &mut ctx.err_frames[i] else {
                    unreachable!("frame {i} is a catch")
                };
                c.errs.push((ety.clone(), local));
                c.errs.len() - 1
            });
            let ErrFrame::Catch(c) = &ctx.err_frames[i] else {
                unreachable!("frame {i} is a catch")
            };
            out.push(HirStmt::Assign {
                local: c.errs[k].1,
                value,
            });
            out.push(HirStmt::Assign {
                local: which,
                value: hir_int(
                    i128::try_from(k).expect("error index fits"),
                    PrimTy::I32,
                    span,
                ),
            });
            if let Some(local) = c.site {
                out.push(HirStmt::Assign {
                    local,
                    value: str_const(ctx.env.sources.location(span), span),
                });
                out.extend(site.and_then(|s| note_site(ctx, s.clone())));
            }
        }
        if let ErrFrame::Catch(c) = &mut ctx.err_frames[i] {
            c.hit = true;
            if !c.raised.contains(&ety) {
                c.raised.push(ety);
            }
        }
        out.push(HirStmt::Break {
            label: Some(label),
            value: None,
        });
        return out;
    }
    // No frame takes it: it leaves the function.
    if let Some(found) = &mut ctx.throws_probe {
        if !found.contains(&ety) {
            found.push(ety);
        }
    } else if let Some(decl) = ctx.fn_throws.clone() {
        if let Some(v) = convert_err(ctx, value, &decl) {
            let ok = carrier_ok(ctx);
            // `ref` parameters go back on the error path too.
            let c = crate::lower::mono::ref_pack(ctx, carrier(&ok, &decl, Err(v), span), span);
            out.extend(site.and_then(|s| note_site(ctx, s.clone())));
            out.extend(crate::lower::coro::frame_end(ctx, span));
            out.push(HirStmt::Return(Some(c)));
            return out;
        }
        // An untyped `throws` takes any `Error`.
        if any_error(&decl) {
            let name = ctx.ty_name(&ety);
            ctx.emit_error(
                Code::NOT_AN_ERROR,
                format!("`{name}` can't be thrown: it does not implement `Error`"),
            );
            ctx.help_last(Some(format!(
                "add `impl Error for {name} {{}}` (it needs `Display` and `Debug`), or \
                 throw a `ValueError`: `throw ValueError::new(..);`"
            )));
        } else if ctx.ty_name(&decl).contains('#') {
            // `rethrows`: the declared error is its parameters' (`#E0`).
            ctx.emit_error(
                Code::UNHANDLED_ERROR,
                format!(
                    "error `{}` is not covered: the enclosing function is `rethrows`, so it \
                     throws only what its function parameters throw",
                    ctx.ty_name(&ety)
                ),
            );
            ctx.help_last(Some(format!(
                "catch it here, or declare `throws ({})` instead of `rethrows`",
                ctx.ty_name(&ety)
            )));
        } else {
            ctx.emit_error(
                Code::UNHANDLED_ERROR,
                format!(
                    "error `{}` is not covered by the enclosing function's `throws ({})` - \
                     catch it or add it to the `throws` list",
                    ctx.ty_name(&ety),
                    ctx.ty_name(&decl)
                ),
            );
        }
    } else {
        ctx.emit_error(
            Code::UNHANDLED_ERROR,
            format!(
                "error `{}` is not handled: catch it, or declare `throws ({0})` on the \
                 enclosing function",
                ctx.ty_name(&ety)
            ),
        );
    }
    out.push(HirStmt::Return(None));
    out
}

/// Error `value` as a `to` error: itself, injected into sum `to`, or
/// re-wrapped member by member; `None` if `to` can't hold it.
fn convert_err(ctx: &mut LowerCtx<'_>, value: HirExpr, to: &HirTy) -> Option<HirExpr> {
    let from = value.ty.clone();
    if !err_fits(ctx, &from, to) {
        return None;
    }
    if from == *to {
        return Some(value);
    }
    let HirTy::Sum(vs) = &from else {
        // Into a sum, or into `any Error` (untyped `throws`) boxed.
        let boxed = box_into_any(ctx, to, value);
        return Some(wrap_into_sum(to, boxed));
    };
    // A sum: convert variant by variant.
    let span = value.span;
    let arms = vs
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let b = ctx.declare(None, v.clone());
            let body = convert_err(ctx, hir_local(b, v, span), to).expect("checked by err_fits");
            HirMatchArm {
                pat: HirPattern::SumVariant {
                    sum_ty: from.clone(),
                    variant_index: u32::try_from(i).expect("variant overflow"),
                    binder: Some(b),
                    payload_ty: Some(v.clone()),
                },
                guard: None,
                body,
            }
        })
        .collect();
    Some(HirExpr {
        kind: HirExprKind::Match {
            scrutinee: Box::new(value),
            arms,
        },
        ty: to.clone(),
        span,
    })
}

/// Can an error of type `from` leave through `throws (to)`?
fn err_fits(ctx: &LowerCtx<'_>, from: &HirTy, to: &HirTy) -> bool {
    from == to
        || match from {
            HirTy::Sum(vs) => vs.iter().all(|v| err_fits(ctx, v, to)),
            _ => matches!(to, HirTy::Sum(ws) if ws.contains(from)) || coercible(ctx, to, from),
        }
}

/// Lower frame `i`'s `finally` block inline, outside that frame and
/// everything nested in it (a raise inside it goes outward).
fn run_finally(ctx: &mut LowerCtx<'_>, i: usize) -> HirStmt {
    let ErrFrame::Finally {
        block,
        hir,
        try_depth,
        ..
    } = &ctx.err_frames[i]
    else {
        unreachable!("frame {i} is a finally")
    };
    let (block, hir, try_depth) = (block.clone(), hir.clone(), *try_depth);
    let inner = ctx.err_frames.split_off(i);
    let saved = std::mem::replace(&mut ctx.try_depth, try_depth);
    ctx.push_expected(HirTy::Unit);
    let mut b = lower_block(ctx, &block);
    ctx.pop_expected();
    ctx.try_depth = saved;
    ctx.err_frames.extend(inner);
    b.stmts.extend(hir);
    HirStmt::Expr(HirExpr {
        kind: HirExprKind::Block(Box::new(b)),
        ty: HirTy::Unit,
        span: block.span,
    })
}

/// `jump` (`return` / `break` / `continue` with `value`) after the
/// `finally` blocks it leaves: all for `return` (`depth` `None`), else
/// those inside breakable `depth`.
pub(crate) fn exit_through(
    ctx: &mut LowerCtx<'_>,
    depth: Option<usize>,
    value: Option<HirExpr>,
    jump: impl FnOnce(Option<HirExpr>) -> HirStmt,
    span: nabic_diagnostic::Span,
) -> HirStmt {
    let fins: Vec<usize> = (0..ctx.err_frames.len())
        .rev()
        .filter(|&i| {
            matches!(&ctx.err_frames[i], ErrFrame::Finally { breakables, .. }
                if depth.is_none_or(|d| *breakables > d))
        })
        .collect();
    if fins.is_empty() {
        return jump(value);
    }
    let mut stmts = Vec::new();
    let value = value.and_then(|v| {
        if v.ty == HirTy::Unit {
            stmts.push(HirStmt::Expr(v));
            return None;
        }
        let ty = v.ty.clone();
        let t = ctx.declare(None, ty.clone());
        stmts.push(HirStmt::Let { local: t, init: v });
        Some(hir_local(t, &ty, span))
    });
    for i in fins {
        stmts.push(run_finally(ctx, i));
    }
    stmts.push(jump(value));
    HirStmt::Expr(block_expr(stmts, span))
}

/// `return value` from the current point: wrapped into the ok carrier in
/// a throwing function, after the `finally` blocks it leaves.
pub(crate) fn lower_return(
    ctx: &mut LowerCtx<'_>,
    value: Option<HirExpr>,
    span: nabic_diagnostic::Span,
) -> HirStmt {
    if ctx.coro.is_some() {
        return exit_through(ctx, None, value, crate::lower::coro::coro_return, span);
    }
    let value = match ctx.fn_throws.clone() {
        Some(err) => {
            let ok = carrier_ok(ctx);
            let v = value.unwrap_or_else(|| unit_value(span));
            Some(crate::lower::mono::ref_pack(
                ctx,
                carrier(&ok, &err, Ok(v), span),
                span,
            ))
        }
        // `ref` parameters: hand their values back too.
        None if !ctx.ref_params.is_empty() => Some(crate::lower::mono::ref_pack(
            ctx,
            value.unwrap_or_else(|| unit_value(span)),
            span,
        )),
        None => value,
    };
    exit_through(ctx, None, value, HirStmt::Return, span)
}

/// Index in `ctx.breakables` of the target of a `break` / `continue`.
pub(crate) fn jump_index(ctx: &LowerCtx<'_>, label: Option<&str>, needs_loop: bool) -> usize {
    ctx.breakables
        .iter()
        .rposition(|(bl, is_loop)| match label {
            None => *is_loop,
            Some(l) => bl.as_deref() == Some(l) && (*is_loop || !needs_loop),
        })
        .unwrap_or(usize::MAX)
}

/// An untyped `throws`: `any Error`.
fn any_error(t: &HirTy) -> bool {
    matches!(t, HirTy::AnyTrait(b) if b.iter().any(|t| t.path == "core::error::Error"))
}

pub(crate) fn lower_throw(
    ctx: &mut LowerCtx<'_>,
    value: &Expr,
    span: nabic_diagnostic::Span,
) -> HirStmt {
    let in_catch = ctx
        .err_frames
        .iter()
        .any(|f| matches!(f, ErrFrame::Catch(_)));
    let expected = if in_catch {
        HirTy::Unknown
    } else {
        ctx.fn_throws.clone().unwrap_or(HirTy::Unknown)
    };
    let v = lower_expected(ctx, value, expected);
    // `throw "msg"` throws `ValueError::new("msg")`, unless a catch or the
    // declared `throws` takes a `str` itself.
    let str_ty = HirTy::Prim(PrimTy::Str);
    let str_taken = v.ty == str_ty
        && (ctx.err_frames.iter().any(|f| {
            matches!(f, ErrFrame::Catch(CatchFrame { arms: Some((filters, _)), .. })
                if filters.iter().flatten().any(|t| err_fits(ctx, &str_ty, t)))
        }) || ctx
            .fn_throws
            .as_ref()
            .is_some_and(|d| err_fits(ctx, &str_ty, d)));
    let value_error = ctx
        .lookup_struct(VALUE_ERROR)
        .filter(|sid| v.ty == str_ty && !str_taken && ctx.struct_def(*sid).fields.len() == 1);
    let wrapped = value_error.is_some();
    let v = match value_error {
        Some(sid) => HirExpr {
            kind: HirExprKind::StructInit {
                struct_id: sid,
                fields: vec![v],
            },
            ty: HirTy::Struct(sid),
            span: value.span,
        },
        None => v,
    };
    // A `catch` passing on an error no arm takes keeps its site. The
    // library's own `throw`s are no place to point at and leave the slot
    // alone: empty, or a site the library carried over.
    let rethrow = match &value.kind {
        ExprKind::Path(p) if p.segments.len() == 1 && p.segments[0].name.starts_with("#err") => {
            ctx.rethrow_site
        }
        _ => None,
    };
    let site = match rethrow {
        Some(l) => Some(hir_local(l, &HirTy::Prim(PrimTy::Str), span)),
        None if nabic_core::in_toolchain(&ctx.current_module) => None,
        None => Some(str_const(ctx.env.sources.location(span), span)),
    };
    let since = ctx.env.sink.diagnostics.borrow().len();
    let stmts = raise_at(ctx, v, span, site.as_ref());
    if wrapped {
        let decl = ctx.fn_throws.as_ref().map(|t| ctx.ty_name(t));
        let help = match decl {
            Some(d) => format!(
                "`throw \"..\"` throws a `ValueError`: add it to the list \
                 (`throws ({d} | ValueError)`), or throw one of the listed errors"
            ),
            None => "`throw \"..\"` throws a `ValueError`: declare `throws` on the function, \
                     or catch it here"
                .to_owned(),
        };
        ctx.help_since(since, help);
    }
    HirStmt::Expr(block_expr(stmts, span))
}

const fn str_const(s: String, span: nabic_diagnostic::Span) -> HirExpr {
    HirExpr {
        kind: HirExprKind::Const(ConstVal::Str(s)),
        ty: HirTy::Prim(PrimTy::Str),
        span,
    }
}

/// `nabi_throw_site(at)`: the thread's note of where the error in flight
/// was thrown (`""`: none). Nothing without the runtime (`bare`).
fn note_site(ctx: &LowerCtx<'_>, at: HirExpr) -> Option<HirStmt> {
    let sig = ctx.env.sig("nabi_throw_site")?;
    let span = at.span;
    Some(HirStmt::Expr(HirExpr {
        kind: HirExprKind::Call {
            callee: sig.id,
            args: vec![at],
        },
        ty: HirTy::Unit,
        span,
    }))
}

/// Whether `e` calls `f`.
fn calls(e: &HirExpr, f: FunctionId) -> bool {
    let mut hit = false;
    crate::each_expr(e, &mut |x| {
        hit |= matches!(x.kind, HirExprKind::Call { callee, .. } if callee == f);
    });
    hit
}

/// A handler took the error: its site no longer applies.
fn clear_site(ctx: &LowerCtx<'_>, span: nabic_diagnostic::Span) -> Vec<HirStmt> {
    note_site(ctx, str_const(String::new(), span))
        .into_iter()
        .collect()
}

/// What `throw "msg"` throws.
const VALUE_ERROR: &str = "core::error::ValueError";

/// Push a frame that takes every error (`try?` / `try!` / throwing
/// `main`).
pub(crate) fn catch_all_frame(ctx: &mut LowerCtx<'_>) {
    let label = fresh_label(ctx);
    ctx.err_frames.push(ErrFrame::Catch(CatchFrame {
        label,
        arms: None,
        errs: Vec::new(),
        hit: false,
        raised: Vec::new(),
        site: None,
    }));
}

fn pop_catch(ctx: &mut LowerCtx<'_>) -> CatchFrame {
    match ctx.err_frames.pop() {
        Some(ErrFrame::Catch(c)) => c,
        _ => unreachable!("frames are pushed and popped in pairs"),
    }
}

/// `'done: { '<frame>: { break 'done body }; handler }`: a raise breaks
/// into the handler, normal completion skips it.
fn shell(
    ctx: &mut LowerCtx<'_>,
    frame: &CatchFrame,
    body: HirExpr,
    result_ty: HirTy,
    handler: (Vec<HirStmt>, Option<HirExpr>),
    span: nabic_diagnostic::Span,
) -> HirExpr {
    let done = fresh_label(ctx);
    let inner_stmts = if hir_expr_diverges(&body) {
        vec![HirStmt::Expr(body)]
    } else if result_ty == HirTy::Unit {
        vec![
            HirStmt::Expr(body),
            HirStmt::Break {
                label: Some(done.clone()),
                value: None,
            },
        ]
    } else {
        vec![HirStmt::Break {
            label: Some(done.clone()),
            value: Some(body),
        }]
    };
    let inner = HirExpr {
        kind: HirExprKind::Block(Box::new(HirBlock {
            stmts: inner_stmts,
            trailing: None,
            label: Some(frame.label.clone()),
        })),
        ty: HirTy::Unit,
        span,
    };
    let (handler_stmts, trailing) = handler;
    let mut stmts = vec![HirStmt::Expr(inner)];
    stmts.extend(handler_stmts);
    HirExpr {
        kind: HirExprKind::Block(Box::new(HirBlock {
            stmts,
            trailing,
            label: Some(done),
        })),
        ty: result_ty,
        span,
    }
}

/// `msg` to stderr through never-returning runtime hook `hook(msg, len)`.
pub(crate) fn hook_call(
    ctx: &LowerCtx<'_>,
    hook: &str,
    msg: &str,
    span: nabic_diagnostic::Span,
) -> HirExpr {
    let Some(sig) = ctx.env.sig(hook) else {
        return ctx.error_expr(
            Code::INTERNAL,
            span,
            format!("internal: runtime hook `{hook}` is not declared"),
        );
    };
    HirExpr {
        kind: HirExprKind::Call {
            callee: sig.id,
            args: vec![HirExpr {
                kind: HirExprKind::Const(ConstVal::Str(msg.to_owned())),
                ty: HirTy::Prim(PrimTy::Str),
                span,
            }],
        },
        ty: HirTy::Unit,
        span,
    }
}

/// Prefix `try e` / `try? e` / `try! e`.
pub(crate) fn lower_try(
    ctx: &mut LowerCtx<'_>,
    kind: nabic_ast::TryKind,
    operand: &Expr,
    span: nabic_diagnostic::Span,
) -> HirExpr {
    let expected = ctx.current_expected().cloned().unwrap_or(HirTy::Unknown);
    let calls = ctx.throwing_calls;
    let mark = ctx.env.sink.diagnostics.borrow().len();
    ctx.try_for = matches!(operand.kind, ExprKind::For { .. }).then_some(ctx.try_depth);
    // An operand that failed to lower (an error, or a poisoned value) may
    // hide its throwing call. In an instance of a library template the
    // call is the user's impl, which may throw less than its trait; the
    // `try` there is not the user's to drop.
    let warn_if_idle = |ctx: &LowerCtx<'_>, e: &HirExpr| {
        if crate::lower::pattern::in_instance(ctx) && nabic_core::in_toolchain(&ctx.current_module)
        {
            return;
        }
        let failed = ctx
            .env
            .sink
            .diagnostics
            .borrow()
            .iter()
            .skip(mark)
            .any(|d| d.severity == nabic_diagnostic::Severity::Error);
        if ctx.throwing_calls == calls && !failed && e.ty != HirTy::Unknown {
            ctx.emit_warning(
                Code::UNNEEDED_TRY,
                "this `try` covers no throwing call".to_owned(),
            );
        }
    };
    // `try! xs.pop()`: no throwing call, an optional value.
    let optional = |ctx: &LowerCtx<'_>, e: &HirExpr| {
        if ctx.throwing_calls != calls {
            return None;
        }
        let some = crate::lower::suggest::some_part(&e.ty)?;
        let op = crate::lower::suggest::short_src(ctx, operand.span).unwrap_or("value");
        let t = ctx.ty_name(&e.ty);
        let d = crate::lower::suggest::default_lit(&some).unwrap_or("default");
        let word = if kind == TryKind::Force {
            "try!"
        } else {
            "try"
        };
        let fix = format!("{op} ?? {d}");
        ctx.emit_diag(
            nabic_diagnostic::NabiDiagnostic::error(
                Code::OPTIONAL_OPERATOR,
                span,
                format!("`{word}` is for calls that throw; `{op}` gives `{t}`, which may be none"),
            )
            .with_help(format!("write `{fix}`, or `if let x = {op} {{ .. }}`"))
            .with_fix(span, fix.clone(), format!("use `{fix}`")),
        );
        Some(poison(span))
    };
    if kind == TryKind::Plain {
        ctx.try_depth += 1;
        let e = lower_expected(ctx, operand, expected);
        ctx.try_depth -= 1;
        if let Some(p) = optional(ctx, &e) {
            return p;
        }
        warn_if_idle(ctx, &e);
        return e;
    }
    // `let x: i32 | none = try? f()` - the operand's slot is the payload.
    let inner_expected = match (kind, optional_parts(&expected)) {
        (TryKind::Optional, Some((_, some))) => crate::lower::desugar::without_none(&some),
        (TryKind::Optional, None) if expected != HirTy::Unit => HirTy::Unknown,
        _ => expected.clone(),
    };
    if kind == TryKind::Force {
        main_frame(ctx);
    } else {
        catch_all_frame(ctx);
    }
    ctx.try_depth += 1;
    let body = lower_expected(ctx, operand, inner_expected);
    ctx.try_depth -= 1;
    if kind == TryKind::Force {
        if let Some(p) = optional(ctx, &body) {
            pop_catch(ctx);
            return p;
        }
    }
    warn_if_idle(ctx, &body);
    let frame = pop_catch(ctx);
    if kind == TryKind::Force {
        if !frame.hit {
            return body;
        }
        let report = force_report(ctx, &frame, span);
        let ty = body.ty.clone();
        return shell(ctx, &frame, body, ty, (report, None), span);
    }
    dropped_resources(ctx, &frame, span);
    // `try?` in statement position drops the error; elsewhere a `()`
    // operand gives `() | none`.
    if expected == HirTy::Unit {
        if !frame.hit {
            return body;
        }
        let clear = clear_site(ctx, span);
        return shell(ctx, &frame, body, HirTy::Unit, (clear, None), span);
    }
    // `T?` with the operand as `T`: an optional operand
    // nests, so a thrown error and a returned `none` stay apart.
    let result_ty = if body.ty == HirTy::Unknown {
        body.ty.clone()
    } else {
        HirTy::sum([body.ty.clone(), HirTy::None])
    };
    let body = wrap_into_sum(&result_ty, body);
    if !frame.hit {
        return body;
    }
    let none = sum_none(&result_ty, span);
    let clear = clear_site(ctx, span);
    shell(ctx, &frame, body, result_ty, (clear, Some(none)), span)
}

/// `try?` drops the error it catches: one that can hold a value whose
/// close can fail would leave it open.
fn dropped_resources(ctx: &LowerCtx<'_>, frame: &CatchFrame, span: nabic_diagnostic::Span) {
    for ety in &frame.raised {
        if crate::lower::affine::affine(ctx, ety) != Some(crate::lower::affine::Kind::Close) {
            continue;
        }
        let t = ctx.ty_name(ety);
        ctx.emit_diag(
            nabic_diagnostic::NabiDiagnostic::error(
                Code::UNCLOSED_RESOURCE,
                span,
                format!(
                    "`try?` drops the `{t}` it catches, which can hold a value that must be \
                     closed"
                ),
            )
            .with_help(format!(
                "catch it instead: `try .. catch e {{ .. }}`, take the value out of the `{t}` \
                 and close it (`try x.close();`)"
            )),
        );
    }
}

/// The ok side of the current function's carrier.
fn carrier_ok(ctx: &LowerCtx<'_>) -> HirTy {
    // A coroutine body's errors ride in its step carrier.
    if let Some(c) = &ctx.coro {
        return c.step_ty(ctx, &ctx.fn_ret_ty);
    }
    ctx.fn_ret_ty.clone()
}

/// A throwing function's body yields the ok carrier.
pub(crate) fn finish_throwing_body(ctx: &LowerCtx<'_>, block: &mut HirBlock) {
    let Some(err) = ctx.fn_throws.clone() else {
        return;
    };
    let ok = carrier_ok(ctx);
    match block.trailing.take() {
        Some(t) if hir_expr_diverges(&t) => block.trailing = Some(t),
        Some(t) => {
            let span = t.span;
            block.trailing = Some(carrier(&ok, &err, Ok(t), span));
        }
        None if hir_block_diverges(block) => {}
        None => {
            let span = nabic_diagnostic::Span::DUMMY;
            block.trailing = Some(carrier(&ok, &err, Ok(unit_value(span)), span));
        }
    }
}
