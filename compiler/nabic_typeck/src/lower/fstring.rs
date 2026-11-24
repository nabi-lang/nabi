//! `f"..."` written into one runtime text (a `core::fmt::Formatter`'s;
//! `nabi_fmt_*` return it, maybe moved):
//!
//! ```text
//! f"a {n} b {s:^5} {p:?}"   (n: i32, s: str, p: P)
//!   ==>  { let #f0 = n; let #f1 = s; let #f2 = p;
//!          let #t = nabi_fmt_new(55);
//!          #t = nabi_fmt_str(#t, "a ");
//!          #t = nabi_fmt_int(#t, <no spec>, #f0 as i64, 1, 32);
//!          #t = nabi_fmt_str(#t, " b ");
//!          let #s = nabi_fmt_len(#t); #t = nabi_fmt_str(#t, #f1);
//!          #t = nabi_fmt_pad(#t, <`^5` packed>, #s, #s, 0);
//!          #t = nabi_fmt_str(#t, " ");
//!          #t = core::fmt::fstr_debug(#t, #f2, <no spec>);
//!          nabi_fmt_finish(#t) }
//! ```
//!
//! Holes and `${..}` specs are evaluated left to right into temps, then
//! written in order: primitives by the runtime, other values through
//! `Display::fmt` / `Debug::fmt` (`fstr_display` / `fstr_debug`). Numbers
//! honour their spec; anything else is written plain (but for `#`), then
//! cut and padded.
//! A static spec is checked here with the runtime's own parser, a dynamic
//! one at run time.

use crate::lower::bignum::Big;
#[allow(clippy::wildcard_imports)]
use crate::lower::*;
use nabic_ast::{FStringPart, SpecPiece};
use nabic_diagnostic::{Code, Span, Tag};
use nabir_abi::fmt::spec::{self as fmt_spec, FmtSpec};

/// Per-hole capacity guess, beyond the literal bytes.
const HOLE_BYTES: usize = 16;

/// A hole, bound to a temp.
struct Hole {
    value: Expr,
    kind: Kind,
    spec: Spec,
    /// `?`: the Debug.
    debug: bool,
    /// Implements `Display` / `Debug` (picked at run time for a dynamic spec).
    both: bool,
    /// A struct or enum with `Display`: it takes a number's type letter.
    own: bool,
}

enum Spec {
    None,
    Static(FmtSpec),
    /// The spec text, in a temp.
    Dynamic(Expr),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Int {
        signed: bool,
        bits: u32,
        /// An `i64` already: passed to the runtime as is.
        i64: bool,
    },
    Float {
        f32: bool,
        /// An open literal's: always converted, its type may change.
        open: bool,
    },
    /// `int` / `float`.
    Big,
    Str,
    Char,
    Other,
}

impl Kind {
    const fn number(self) -> bool {
        matches!(self, Self::Int { .. } | Self::Float { .. } | Self::Big)
    }
}

fn kind_of(ctx: &LowerCtx<'_>, ty: &HirTy) -> Kind {
    // An open literal formats as the number it holds; the body's end
    // gives the runtime call its final type (`infer::Fixer`).
    let open = matches!(ty, HirTy::Var(_));
    let ty = &crate::lower::infer::zonk(ctx, ty);
    match ty {
        HirTy::Prim(p) if p.is_signed_int() || p.is_unsigned_int() => Kind::Int {
            i64: *p == PrimTy::I64 && !open,
            signed: p.is_signed_int(),
            bits: p.bit_width(crate::lower::consteval::pointer_width()),
        },
        HirTy::Prim(PrimTy::F32) => Kind::Float { f32: true, open },
        HirTy::Prim(PrimTy::F64) => Kind::Float { f32: false, open },
        HirTy::Prim(PrimTy::Str) => Kind::Str,
        HirTy::Prim(PrimTy::Char) => Kind::Char,
        _ if crate::lower::bignum::big_of(ctx, ty).is_some() => Kind::Big,
        _ => Kind::Other,
    }
}

pub(crate) fn lower_fstring(ctx: &mut LowerCtx<'_>, parts: &[FStringPart], span: Span) -> HirExpr {
    ctx.push_scope();
    let mut stmts = Vec::new();
    // `Err(text)`: a literal piece.
    let mut pieces: Vec<Result<Hole, String>> = Vec::new();
    let snap = snapshots(parts);
    for (i, part) in parts.iter().enumerate() {
        match part {
            FStringPart::Lit(text) => pieces.push(Err(text.clone())),
            FStringPart::Hole { expr, spec } => {
                let at = point(expr.span);
                let Some(value) = bind(ctx, &mut stmts, expr) else {
                    continue;
                };
                let ty = ctx.local_ty(local_of(ctx, &value));
                if snap[i] && shared(ctx, &ty) {
                    let Some(h) = snapshot(ctx, &mut stmts, value, spec, expr.span) else {
                        continue;
                    };
                    pieces.push(Ok(h));
                    continue;
                }
                // The spec as text, or as f-string pieces when dynamic.
                let mut text = String::new();
                let mut dynamic = Vec::new();
                for piece in spec {
                    match piece {
                        SpecPiece::Text(t) => {
                            text.push_str(t);
                            dynamic.push(FStringPart::Lit(t.clone()));
                        }
                        SpecPiece::Expr(e) => {
                            if let Some(v) = bind(ctx, &mut stmts, e) {
                                dynamic.push(FStringPart::Hole {
                                    expr: v,
                                    spec: Vec::new(),
                                });
                            }
                        }
                    }
                }
                let is_dynamic = spec.iter().any(|p| matches!(p, SpecPiece::Expr(_)));
                let outer = ctx.at.replace(expr.span);
                let hole = if is_dynamic {
                    let spec_text = lower_fstring(ctx, &dynamic, at);
                    let spec = Spec::Dynamic(bind_hir(ctx, &mut stmts, "#fs", spec_text, at));
                    Some(dynamic_hole(ctx, value, &ty, spec, text.ends_with('?')))
                } else {
                    static_hole(ctx, value, &ty, &text)
                };
                ctx.at.set(outer);
                if let Some(h) = hole {
                    pieces.push(Ok(h));
                }
            }
        }
    }
    let tail = write_all(ctx, &mut stmts, pieces, span);
    ctx.pop_scope();
    HirExpr {
        ty: HirTy::Prim(PrimTy::Str),
        kind: HirExprKind::Block(Box::new(HirBlock {
            stmts,
            trailing: Some(tail),
            label: None,
        })),
        span,
    }
}

/// `value` formatted now with `spec` (spec values evaluated here too):
/// a `str` hole that a later hole cannot change.
fn snapshot(
    ctx: &mut LowerCtx<'_>,
    stmts: &mut Vec<HirStmt>,
    value: Expr,
    spec: &[SpecPiece],
    span: Span,
) -> Option<Hole> {
    let mut pieces = Vec::new();
    for piece in spec {
        match piece {
            SpecPiece::Text(t) => pieces.push(SpecPiece::Text(t.clone())),
            SpecPiece::Expr(e) => pieces.push(SpecPiece::Expr(bind(ctx, stmts, e)?)),
        }
    }
    let one = mk(
        ExprKind::FString(vec![FStringPart::Hole {
            expr: value,
            spec: pieces,
        }]),
        span,
    );
    ctx.push_expected(HirTy::Prim(PrimTy::Str));
    let text = lower_expr(ctx, &one);
    ctx.pop_expected();
    let text = bind_hir(ctx, stmts, "#fv", text, point(span));
    static_hole(ctx, text, &HirTy::Prim(PrimTy::Str), "")
}

/// An f-string where it is written: an `Arguments` in a
/// `core::fmt::Arguments` slot, else a `str`.
pub(crate) fn lower_fstring_expr(
    ctx: &mut LowerCtx<'_>,
    parts: &[FStringPart],
    span: Span,
) -> HirExpr {
    if wants_arguments(ctx) {
        lower_fstring_args(ctx, parts, span)
    } else {
        lower_fstring(ctx, parts, span)
    }
}

/// The slot is `core::fmt::Arguments`: the f-string is written later,
/// piece by piece (`lower_fstring_args`), not made into a `str` here.
fn wants_arguments(ctx: &LowerCtx<'_>) -> bool {
    matches!(ctx.current_expected(), Some(HirTy::Struct(id))
        if ctx.struct_def(*id).name == "core::fmt::Arguments")
}

/// `f"a {x} b {y:>4}"` in an `Arguments` slot:
///
/// ```text
///   ==>  { let #f0 = x; let #f1 = y;
///          core::fmt::fstr_args(|#fa: Formatter| -> Formatter {
///              #fa = try fstr_put_str(#fa, "a ");
///              #fa = try fstr_put_display(#fa, #f0);
///              #fa = try fstr_put_str(#fa, " b ");
///              #fa = try fstr_put_str(#fa, f"{#f1:>4}");
///              #fa
///          }) }
/// ```
///
/// Holes and spec values are evaluated here, left to right, as for a
/// `str`; a hole with no spec goes through `Display::fmt` / `Debug::fmt`
/// straight into the formatter, one with a spec is made into its own
/// text first (padding needs it whole).
pub(crate) fn lower_fstring_args(
    ctx: &mut LowerCtx<'_>,
    parts: &[FStringPart],
    span: Span,
) -> HirExpr {
    let expected = ctx.current_expected().cloned().unwrap_or(HirTy::Unknown);
    ctx.push_scope();
    let mut stmts = Vec::new();
    let f = "#fa";
    let fp = || mk(ExprKind::Path(path(&[f], span)), span);
    let mut body: Vec<nabic_ast::Stmt> = Vec::new();
    let mut put = |piece: &str, arg: Expr| {
        let call = hook(piece, vec![fp(), arg], span);
        let tried = mk(
            ExprKind::Try {
                kind: nabic_ast::TryKind::Plain,
                operand: Box::new(call),
            },
            span,
        );
        body.push(nabic_ast::Stmt {
            kind: nabic_ast::StmtKind::Assign(fp(), tried),
            span,
        });
    };
    let snap = snapshots(parts);
    for (i, part) in parts.iter().enumerate() {
        match part {
            FStringPart::Lit(text) => put(
                "fstr_put_str",
                mk(ExprKind::Literal(AstLit::Str(text.clone())), span),
            ),
            FStringPart::Hole { expr, spec } => {
                let Some(value) = bind(ctx, &mut stmts, expr) else {
                    continue;
                };
                let ty = ctx.local_ty(local_of(ctx, &value));
                if snap[i] && shared(ctx, &ty) {
                    let Some(h) = snapshot(ctx, &mut stmts, value, spec, expr.span) else {
                        continue;
                    };
                    put("fstr_put_str", h.value);
                    continue;
                }
                let mut text = String::new();
                let mut pieces = Vec::new();
                for piece in spec {
                    match piece {
                        SpecPiece::Text(t) => {
                            text.push_str(t);
                            pieces.push(SpecPiece::Text(t.clone()));
                        }
                        SpecPiece::Expr(e) => {
                            if let Some(v) = bind(ctx, &mut stmts, e) {
                                pieces.push(SpecPiece::Expr(v));
                            }
                        }
                    }
                }
                let plain = pieces.iter().all(|p| matches!(p, SpecPiece::Text(_)));
                if plain && (text.is_empty() || text == "?") {
                    let ty = ctx.local_ty(local_of(ctx, &value));
                    let debug = text == "?";
                    let outer = ctx.at.replace(expr.span);
                    let ok = formattable(ctx, &ty, debug);
                    ctx.at.set(outer);
                    if ok {
                        put(
                            if debug {
                                "fstr_put_debug"
                            } else {
                                "fstr_put_display"
                            },
                            value,
                        );
                    }
                } else {
                    let one = mk(
                        ExprKind::FString(vec![FStringPart::Hole {
                            expr: value,
                            spec: pieces,
                        }]),
                        expr.span,
                    );
                    put("fstr_put_str", one);
                }
            }
        }
    }
    let formatter = nabic_ast::Ty {
        kind: nabic_ast::TyKind::Path(path(&["", "core", "fmt", "Formatter"], span)),
        span,
    };
    let closure = mk(
        ExprKind::Closure {
            captures: Vec::new(),
            params: vec![nabic_ast::Param {
                name: f.to_owned(),
                ty: Some(formatter.clone()),
                by_ref: false,
                default: None,
                variadic: false,
                span,
            }],
            ret_ty: Some(formatter),
            body: Box::new(mk(
                ExprKind::Block(nabic_ast::Block {
                    stmts: body,
                    trailing: Some(Box::new(fp())),
                    span,
                    label: None,
                }),
                span,
            )),
            is_async: false,
            is_generator: false,
        },
        span,
    );
    let args = lower_expected(ctx, &hook("fstr_args", vec![closure], span), expected);
    ctx.pop_scope();
    HirExpr {
        ty: args.ty.clone(),
        kind: HirExprKind::Block(Box::new(HirBlock {
            stmts,
            trailing: Some(args),
            label: None,
        })),
        span,
    }
}

/// The function `callee` names, unless a local shadows it.
fn callee_fn(ctx: &LowerCtx<'_>, callee: &Expr) -> Option<String> {
    let ExprKind::Path(p) = &callee.kind else {
        return None;
    };
    // A compiler-written call is the function itself.
    if p.origin.is_some() {
        return None;
    }
    if let [s] = p.segments.as_slice() {
        if ctx.lookup(&s.name).is_some() {
            return None;
        }
    }
    let segs: Vec<&str> = p.segments.iter().map(|s| s.name.as_str()).collect();
    ctx.resolve_fn_name(&segs)
        .or_else(|| ctx.resolve_generic_fn_name(&segs))
}

/// A direct `dbg(e)`: writes `[file:line:col] <e's source> = <e:?>` to
/// stderr; its value is `e`'s.
pub(crate) fn lower_dbg(
    ctx: &mut LowerCtx<'_>,
    callee: &Expr,
    args: &[CallArg],
    span: Span,
) -> Option<HirExpr> {
    let [CallArg::Positional(e)] = args else {
        return None;
    };
    if callee_fn(ctx, callee)? != "core::print::dbg" {
        return None;
    }
    ctx.push_scope();
    let mut stmts = Vec::new();
    let Some(v) = bind(ctx, &mut stmts, e) else {
        ctx.pop_scope();
        return Some(poison(span));
    };
    let at = point(span);
    let text = ctx.src(e.span).unwrap_or("..").to_owned();
    let parts = [
        FStringPart::Lit(format!("[{}] {text} = ", ctx.env.sources.location(span))),
        FStringPart::Hole {
            expr: v.clone(),
            spec: vec![SpecPiece::Text("?".to_owned())],
        },
    ];
    let line = lower_fstring(ctx, &parts, at);
    let t = bind_hir(ctx, &mut stmts, "#p", line, at);
    let mut callee = path(&["eprintln"], callee.span);
    callee.origin = Some("core::print".to_owned());
    let write = mk(
        ExprKind::Call {
            callee: Box::new(mk(ExprKind::Path(callee), at)),
            args: vec![CallArg::Positional(t)],
            trailing_closure: None,
        },
        at,
    );
    let w = lower_expr(ctx, &write);
    stmts.push(HirStmt::Expr(w));
    let value = lower_expr(ctx, &v);
    ctx.pop_scope();
    Some(HirExpr {
        ty: value.ty.clone(),
        kind: HirExprKind::Block(Box::new(HirBlock {
            stmts,
            trailing: Some(value),
            label: None,
        })),
        span,
    })
}

/// The stream and default `end` of a direct call of a prelude print
/// function (`print` / `println` / `eprint` / `eprintln`).
pub(crate) fn print_target(ctx: &LowerCtx<'_>, callee: &Expr) -> Option<(usize, &'static str)> {
    match callee_fn(ctx, callee)?.as_str() {
        "core::print::print" => Some((1, "")),
        "core::print::println" => Some((1, "\n")),
        "core::print::eprint" => Some((2, "")),
        "core::print::eprintln" => Some((2, "\n")),
        _ => None,
    }
}

/// A direct print call: the values, each with `Display` (`Debug` for a
/// type without it), joined by `sep` (default `" "`) and followed by
/// `end`, written in one call.
pub(crate) fn lower_print(
    ctx: &mut LowerCtx<'_>,
    (fd, nl): (usize, &str),
    callee_at: Span,
    args: &[CallArg],
    span: Span,
) -> HirExpr {
    ctx.push_scope();
    let mut stmts = Vec::new();
    let mut values = Vec::new();
    let (mut sep, mut end) = (None, None);
    for a in args {
        match a {
            CallArg::Positional(e) => values.extend(bind(ctx, &mut stmts, e)),
            CallArg::Named { name, value } if name == "sep" || name == "end" => {
                let slot = if name == "sep" { &mut sep } else { &mut end };
                if slot.is_some() {
                    ctx.emit_error(Code::NAMED_ARGUMENT, format!("`{name}:` is given twice"));
                }
                *slot = bind(ctx, &mut stmts, value);
            }
            CallArg::Named { name, .. } => ctx.emit_error(
                Code::NAMED_ARGUMENT,
                format!("print functions take values, `sep:` and `end:`; not `{name}:`"),
            ),
            CallArg::Spread(_) | CallArg::Ref(_) => ctx.emit_error(
                Code::WRONG_ARGUMENT_COUNT,
                "print functions take values, `sep:` and `end:`".to_owned(),
            ),
        }
    }
    let at = point(span);
    let hole = |e: &Expr| FStringPart::Hole {
        expr: e.clone(),
        spec: Vec::new(),
    };
    let mut parts = Vec::new();
    for (i, v) in values.iter().enumerate() {
        if i > 0 {
            parts.push(
                sep.as_ref()
                    .map_or_else(|| FStringPart::Lit(" ".to_owned()), hole),
            );
        }
        let ty = ctx.local_ty(local_of(ctx, v));
        let display = kind_of(ctx, &ty) != Kind::Other
            || ctx.has_trait(&ty, ctx.lang("display_trait"))
            || !ctx.has_trait(&ty, ctx.lang("debug_trait"));
        parts.push(FStringPart::Hole {
            expr: v.clone(),
            spec: if display {
                Vec::new()
            } else {
                vec![SpecPiece::Text("?".to_owned())]
            },
        });
    }
    // The text goes through the library function (one write); `println`
    // and `eprintln` add their own newline.
    let f = match (fd, end.as_ref()) {
        (1, None) if !nl.is_empty() => "println",
        (2, None) if !nl.is_empty() => "eprintln",
        (1, _) => "print",
        _ => "eprint",
    };
    if let Some(e) = &end {
        parts.push(hole(e));
    }
    let text = lower_fstring(ctx, &parts, at);
    let t = bind_hir(ctx, &mut stmts, "#p", text, at);
    // The user's name for it: hover and go-to find the function.
    let mut callee = path(&[f], callee_at);
    callee.origin = Some("core::print".to_owned());
    let write = mk(
        ExprKind::Call {
            callee: Box::new(mk(ExprKind::Path(callee), callee_at)),
            args: vec![CallArg::Positional(t)],
            trailing_closure: None,
        },
        span,
    );
    let w = lower_expr(ctx, &write);
    stmts.push(HirStmt::Expr(w));
    ctx.pop_scope();
    HirExpr {
        ty: HirTy::Unit,
        kind: HirExprKind::Block(Box::new(HirBlock {
            stmts,
            trailing: None,
            label: None,
        })),
        span,
    }
}

/// The writes, in order, against one formatter state; the block's
/// value is its text.
fn write_all(
    ctx: &mut LowerCtx<'_>,
    stmts: &mut Vec<HirStmt>,
    mut pieces: Vec<Result<Hole, String>>,
    span: Span,
) -> HirExpr {
    let holes = pieces.iter().filter(|p| p.is_ok()).count();
    if holes == 0 {
        let text: String = pieces.into_iter().filter_map(Result::err).collect();
        return lower_expr(ctx, &mk(ExprKind::Literal(AstLit::Str(text)), span));
    }
    // `f"{s}"` of a `str` is `s`.
    if let [Ok(h)] = pieces.as_slice() {
        if h.kind == Kind::Str && !h.debug && matches!(h.spec, Spec::None) {
            let Some(Ok(h)) = pieces.pop() else {
                unreachable!("one hole")
            };
            return lower_expr(ctx, &h.value);
        }
    }
    let lit_bytes: usize = pieces
        .iter()
        .filter_map(|p| p.as_ref().err())
        .map(String::len)
        .sum();
    let new = rt(
        "nabi_fmt_new",
        vec![int_lit(lit_bytes + HOLE_BYTES * holes, span)],
        span,
    );
    let init = lower_expr(ctx, &new);
    let st = bind_hir(ctx, stmts, "#t", init, span);
    for piece in pieces {
        match piece {
            Err(text) => {
                let lit = mk(ExprKind::Literal(AstLit::Str(text)), span);
                write(
                    ctx,
                    stmts,
                    &st,
                    &rt("nabi_fmt_str", vec![st.clone(), lit], span),
                );
            }
            Ok(h) => write_hole(ctx, stmts, &st, h),
        }
    }
    lower_expr(ctx, &rt("nabi_fmt_finish", vec![st], span))
}

/// One hole's writes.
fn write_hole(ctx: &mut LowerCtx<'_>, stmts: &mut Vec<HirStmt>, t: &Expr, h: Hole) {
    let at = h.value.span;
    let plain = || words(fmt_spec::PLAIN, at);
    let (flags, counts) = match &h.spec {
        // A number's `{x:?}` is its rendering under type `?`.
        Spec::None if h.debug && h.kind.number() => words(
            (
                fmt_spec::PLAIN.0 | (u64::from(b'?') << 48),
                fmt_spec::PLAIN.1,
            ),
            at,
        ),
        Spec::None => plain(),
        Spec::Static(s) => words(fmt_spec::pack(s), at),
        Spec::Dynamic(_) if !h.kind.number() && h.both => {
            let Spec::Dynamic(text) = h.spec else {
                unreachable!("dynamic")
            };
            let c = hook("fstr_text_any", vec![t.clone(), h.value, text], at);
            write(ctx, stmts, t, &c);
            return;
        }
        Spec::Dynamic(text) => {
            // A value's own `Display` may take a number's type letter.
            let own = h.own && !h.debug;
            let text_only = mk(
                ExprKind::Literal(AstLit::Bool(!h.kind.number() && !own)),
                at,
            );
            let packed = lower_expr(ctx, &hook("fstr_pack", vec![text.clone(), text_only], at));
            let p = bind_hir(ctx, stmts, "#sp", packed, at);
            let field = |i| mk(ExprKind::TupleField(Box::new(p.clone()), i), at);
            (field(0), field(1))
        }
    };
    if h.kind.number() || matches!(h.spec, Spec::None) {
        // A number applies the spec in its own rendering.
        write(
            ctx,
            stmts,
            t,
            &plain_write(t, h.value, h.kind, h.debug, flags, counts),
        );
        return;
    }
    // Written plain, then cut and padded as text. `#` (`{x:#?}`) reaches
    // the value's own `fmt`.
    let len = lower_expr(ctx, &rt("nabi_fmt_len", vec![t.clone()], at));
    let start = bind_hir(ctx, stmts, "#s", len, at);
    let (pf, pc) = plain();
    let lit = |v: u64| mk(ExprKind::Literal(AstLit::Int(v.to_string())), at);
    let op = |o: &str, a: Expr, b: Expr| {
        mk(
            ExprKind::OpInfix {
                op: o.to_owned(),
                lhs: Box::new(a),
                rhs: Box::new(b),
            },
            at,
        )
    };
    let pf = match &h.spec {
        Spec::Static(s) if s.alt => lit(fmt_spec::PLAIN.0 | fmt_spec::ALT),
        Spec::Dynamic(_) => op("|", pf, op("&", flags.clone(), lit(fmt_spec::ALT))),
        _ => pf,
    };
    // A value's own `Display` takes the precision (`{c:.2}` of a
    // `Complex` hands it to the parts); only the width pads the text.
    if matches!(h.kind, Kind::Other) && !h.debug {
        const HI: u64 = 0xFFFF_FFFF_0000_0000;
        let (plain_f, plain_c) = fmt_spec::PLAIN;
        let keep = |mask: u64| {
            op(
                "|",
                op("&", counts.clone(), lit(mask)),
                lit(plain_c & !mask),
            )
        };
        // Sign, type letter, `#` and grouping too; fill, align, `0` and
        // the width pad the whole text.
        let pass = |f: Expr| op("|", op("&", f, lit(OWN_FLAGS)), lit(plain_f));
        let pf = match &h.spec {
            Spec::Static(s) if h.own => lit(fmt_spec::pack(s).0 & OWN_FLAGS | plain_f),
            Spec::Dynamic(_) if h.own => pass(flags.clone()),
            _ => pf,
        };
        let value = hook("fstr_num", vec![t.clone(), h.value, pf, keep(HI)], at);
        write(ctx, stmts, t, &value);
        let pad = rt(
            "nabi_fmt_pad",
            vec![
                t.clone(),
                flags,
                keep(!HI),
                start.clone(),
                start,
                int_lit(0, at),
            ],
            at,
        );
        write(ctx, stmts, t, &pad);
        return;
    }
    write(
        ctx,
        stmts,
        t,
        &plain_write(t, h.value, h.kind, h.debug, pf, pc),
    );
    let pad = rt(
        "nabi_fmt_pad",
        vec![
            t.clone(),
            flags,
            counts,
            start.clone(),
            start,
            int_lit(0, at),
        ],
        at,
    );
    write(ctx, stmts, t, &pad);
}

/// The packed flags a value's own `Display` takes: sign, type, `#`, `,`
/// and `_` (see `fmt_spec::pack`).
const OWN_FLAGS: u64 = (0xFFFF << 40) | (1 << 56) | (1 << 58) | (1 << 59);

/// The packed spec words as literals.
fn words((flags, counts): (u64, u64), at: Span) -> (Expr, Expr) {
    let lit = |v: u64| mk(ExprKind::Literal(AstLit::Int(v.to_string())), at);
    (lit(flags), lit(counts))
}

/// `value` written under the spec words: primitives by the runtime,
/// anything else through its `Display::fmt` (`Debug::fmt` for `debug`).
fn plain_write(t: &Expr, value: Expr, kind: Kind, debug: bool, flags: Expr, counts: Expr) -> Expr {
    let at = value.span;
    // Only a real conversion: an `i64` / `f64` cast to itself would carry
    // the hole's span, which a user's own cast there starts with too, and
    // `redundant_cast` would see it as one.
    let cast = |v: Expr, to: &str, already: bool| {
        if already {
            return v;
        }
        mk(
            ExprKind::Cast(
                Box::new(v),
                nabic_ast::Ty {
                    kind: nabic_ast::TyKind::Path(path(&[to], at)),
                    span: at,
                },
            ),
            at,
        )
    };
    let flag = |b: bool| int_lit(usize::from(b), at);
    let fmt = |f: &str, args: Vec<Expr>| hook(f, args, at);
    match (kind, debug) {
        (Kind::Int { signed, bits, i64 }, _) => rt(
            "nabi_fmt_int",
            vec![
                t.clone(),
                flags,
                counts,
                cast(value, "i64", i64),
                flag(signed),
                int_lit(bits as usize, at),
            ],
            at,
        ),
        (Kind::Float { f32, open }, _) => rt(
            "nabi_fmt_f64",
            vec![
                t.clone(),
                flags,
                counts,
                cast(value, "f64", !f32 && !open),
                flag(f32),
            ],
            at,
        ),
        (Kind::Big, _) => fmt("fstr_num", vec![t.clone(), value, flags, counts]),
        (Kind::Str, false) => rt("nabi_fmt_str", vec![t.clone(), value], at),
        (Kind::Str, true) => rt(
            "nabi_fmt_escaped",
            vec![t.clone(), value, int_lit(usize::from(b'"'), at)],
            at,
        ),
        (Kind::Char, false) => rt(
            "nabi_fmt_char",
            vec![t.clone(), cast(value, "u32", false)],
            at,
        ),
        (_, true) => fmt("fstr_debug", vec![t.clone(), value, flags]),
        (_, false) => fmt("fstr_display", vec![t.clone(), value, flags]),
    }
}

/// `e` may change a value an earlier hole holds: anything but a name, a
/// literal or a field of one.
fn effectful(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Path(_) | ExprKind::Literal(_) => false,
        ExprKind::Field(x, _) | ExprKind::TupleField(x, _) => effectful(x),
        _ => true,
    }
}

/// For each part: a later hole or spec value may change what this hole
/// holds (`f"{xs:?} {xs.pop():?}"`), so it is formatted when evaluated.
fn snapshots(parts: &[FStringPart]) -> Vec<bool> {
    let mut later = false;
    let mut out = vec![false; parts.len()];
    for (i, part) in parts.iter().enumerate().rev() {
        out[i] = later;
        if let FStringPart::Hole { expr, spec } = part {
            later |= effectful(expr)
                || spec
                    .iter()
                    .any(|p| matches!(p, SpecPiece::Expr(e) if effectful(e)));
        }
    }
    out
}

/// A value whose text a later hole could change: not a number, `char`
/// or `str` (copies, or immutable).
fn shared(ctx: &LowerCtx<'_>, ty: &HirTy) -> bool {
    !matches!(kind_of(ctx, ty), Kind::Str | Kind::Char)
        && !kind_of(ctx, ty).number()
        && !matches!(crate::lower::infer::zonk(ctx, ty), HirTy::Prim(_))
}

/// `let #fN = e;`: the temp's path, or `None` when `e` didn't type.
fn bind(ctx: &mut LowerCtx<'_>, stmts: &mut Vec<HirStmt>, e: &Expr) -> Option<Expr> {
    // A hole has no expected type: the f-string's own `str` slot is not it.
    ctx.push_expected(HirTy::Unknown);
    let init = lower_expr(ctx, e);
    ctx.pop_expected();
    if init.ty == HirTy::Unknown {
        // Dropped without a word, the hole would print nothing. After any
        // error it is that error's echo (a name it left untyped).
        let quiet = !ctx
            .env
            .sink
            .diagnostics
            .borrow()
            .iter()
            .any(nabic_diagnostic::NabiDiagnostic::is_error);
        if quiet {
            ctx.emit_error_at(
                Code::TYPE_ANNOTATIONS_NEEDED,
                e.span,
                "cannot infer the type of this value".to_owned(),
            );
            ctx.help_last(Some(
                "bind it to a typed `let` first (`let v: i64 = ..;`) and format `v`, or give \
                 its literals a type (`4i64`)"
                    .to_owned(),
            ));
        }
        return None;
    }
    Some(bind_hir(ctx, stmts, "#f", init, point(e.span)))
}

/// `let <prefix>N = init;`: the temp's path.
fn bind_hir(
    ctx: &mut LowerCtx<'_>,
    stmts: &mut Vec<HirStmt>,
    prefix: &str,
    init: HirExpr,
    at: Span,
) -> Expr {
    let name = format!("{prefix}{}", ctx.locals.len());
    let local = ctx.declare(Some(name.clone()), init.ty.clone());
    stmts.push(HirStmt::Let { local, init });
    mk(ExprKind::Path(path(&[&name], at)), at)
}

/// `st = e;`: a write returns the state, maybe moved.
fn write(ctx: &mut LowerCtx<'_>, stmts: &mut Vec<HirStmt>, st: &Expr, e: &Expr) {
    let local = local_of(ctx, st);
    let value = lower_expr(ctx, e);
    stmts.push(HirStmt::Assign { local, value });
}

/// The empty span at the start of `s`: diagnostics about the code made
/// for a hole land on the hole, but it covers no source (editor hover
/// finds the user's expression).
const fn point(s: Span) -> Span {
    Span { end: s.start, ..s }
}

fn local_of(ctx: &LowerCtx<'_>, e: &Expr) -> LocalId {
    let ExprKind::Path(p) = &e.kind else {
        unreachable!("a bound temp")
    };
    ctx.lookup(&p.segments[0].name).expect("bound temp")
}

fn hole(ctx: &LowerCtx<'_>, value: Expr, ty: &HirTy, spec: Spec, debug: bool) -> Hole {
    let both =
        ctx.has_trait(ty, ctx.lang("display_trait")) && ctx.has_trait(ty, ctx.lang("debug_trait"));
    let held = crate::lower::infer::zonk(ctx, ty);
    let own = matches!(held, HirTy::Struct(_) | HirTy::Enum(_))
        && ctx.has_trait(&held, ctx.lang("display_trait"));
    Hole {
        value,
        kind: kind_of(ctx, ty),
        spec,
        debug,
        both,
        own,
    }
}

/// `{:?}` and `{}` as written in an f-string.
const DEBUG_HOLE: &str = concat!("{", ":?}");
const PLAIN_HOLE: &str = concat!("{", "}");

/// A value the runtime does not format itself needs `Display` (`Debug`
/// for `{:?}`); report a missing impl here, at the hole, rather than
/// inside the library's formatting function.
fn formattable(ctx: &LowerCtx<'_>, ty: &HirTy, debug: bool) -> bool {
    let (slot, name) = if debug {
        ("debug_trait", "Debug")
    } else {
        ("display_trait", "Display")
    };
    let tr = ctx.lang(slot);
    if kind_of(ctx, ty) != Kind::Other || ctx.has_trait(ty, tr) {
        return true;
    }
    // An unknown type name is reported on its own.
    if matches!(ty, HirTy::Path(_)) {
        return false;
    }
    let shown = ctx.ty_name(ty);
    let culprit = match ty {
        HirTy::Sum(vs) => vs
            .iter()
            .find(|v| !ctx.has_trait(v, tr))
            .map(|v| format!(": member `{}` does not implement `{name}`", ctx.ty_name(v))),
        _ => None,
    }
    .unwrap_or_else(|| format!(": it does not implement `{name}`"));
    let hole = if debug { DEBUG_HOLE } else { PLAIN_HOLE };
    ctx.emit_error(
        Code::INVALID_FORMAT,
        format!("`{shown}` cannot be formatted with `{hole}`{culprit}"),
    );
    let (tag, help) = if debug {
        (
            Tag::NoDebug,
            "add `#[derive(Debug)]` to the type, or implement `Debug` for it".to_owned(),
        )
    } else if ctx.has_trait(ty, ctx.lang("debug_trait")) {
        (
            Tag::NoDisplayHasDebug,
            format!("format it with `{DEBUG_HOLE}`, or implement `Display` for it"),
        )
    } else {
        (
            Tag::NoDisplay,
            format!(
                "implement `Display` for it, or derive `Debug` and format it with `{DEBUG_HOLE}`"
            ),
        )
    };
    ctx.help_last(Some(help));
    ctx.tag_last(tag);
    false
}

/// A hole with a static spec: checked against the value's type here.
fn static_hole(ctx: &LowerCtx<'_>, value: Expr, ty: &HirTy, text: &str) -> Option<Hole> {
    if text.is_empty() || text == "?" {
        return formattable(ctx, ty, text == "?")
            .then(|| hole(ctx, value, ty, Spec::None, text == "?"));
    }
    let spec = match fmt_spec::parse(text) {
        Ok(s) => s,
        Err(why) => {
            ctx.emit_error(
                Code::INVALID_FORMAT,
                format!("invalid format spec `{text}`: {why}"),
            );
            return None;
        }
    };
    let held = &crate::lower::infer::zonk(ctx, ty);
    let (int, float) = match (held, crate::lower::bignum::big_of(ctx, held)) {
        (HirTy::Prim(p), _) => (p.is_int(), p.is_float()),
        (_, Some(b)) => (b == Big::Int, b == Big::Float),
        _ => (false, false),
    };
    // A value's own `Display` gets the type letter, sign and grouping
    // with the precision, as a number wrapper hands them to its parts.
    let own = matches!(held, HirTy::Struct(_) | HirTy::Enum(_))
        && spec.ty != b'?'
        && ctx.has_trait(held, ctx.lang("display_trait"));
    let needs = match spec.ty {
        _ if own => None,
        b'b' | b'o' | b'x' | b'X' if !int => Some("an integer"),
        b'e' | b'E' | b'f' | b'g' | b'%' if !int && !float => Some("a number"),
        _ => None,
    };
    if spec.grouping != 0 && !int && !float && !own {
        ctx.emit_error(
            Code::INVALID_FORMAT,
            format!(
                "`{}` groups the digits of a number, found `{}`",
                char::from(spec.grouping),
                ctx.ty_name(ty)
            ),
        );
        return None;
    }
    if spec.grouping == b',' && matches!(spec.ty, b'b' | b'o' | b'x' | b'X') {
        ctx.emit_error(
            Code::INVALID_FORMAT,
            format!(
                "`,` groups decimal digits; group `{}` digits by 4 with `_`",
                char::from(spec.ty)
            ),
        );
        return None;
    }
    if let Some(needs) = needs {
        ctx.emit_error(
            Code::INVALID_FORMAT,
            format!(
                "format type `{}` needs {needs}, found `{}`",
                char::from(spec.ty),
                ctx.ty_name(ty)
            ),
        );
        return None;
    }
    let number = kind_of(ctx, ty).number();
    let debug = spec.ty == b'?';
    // Nothing to apply: the plain rendering (a non-number's sign / `0` /
    // `#` flags do nothing).
    let nothing = matches!(spec.ty, 0 | b's' | b'd')
        && spec.width.is_none()
        && spec.precision.is_none()
        && spec.grouping == 0
        && ((!number && !own) || (spec.sign != b'+' && spec.sign != b' '));
    let spec = if nothing {
        Spec::None
    } else {
        Spec::Static(spec)
    };
    formattable(ctx, ty, debug && !number).then(|| hole(ctx, value, ty, spec, debug && !number))
}

/// A hole with a dynamic spec: a value with only one of `Display` /
/// `Debug` takes that one; `?` known from the text picks the Debug.
fn dynamic_hole(ctx: &LowerCtx<'_>, value: Expr, ty: &HirTy, spec: Spec, q: bool) -> Hole {
    let mut h = hole(ctx, value, ty, spec, q);
    if !h.kind.number() && !q {
        let display = ctx.has_trait(ty, ctx.lang("display_trait"));
        h.debug = !display;
        h.both &= display;
    }
    h.both &= !q;
    h
}

const fn mk(kind: ExprKind, span: Span) -> Expr {
    Expr { kind, span }
}

fn call(names: &[&str], args: Vec<Expr>, at: Span) -> Expr {
    mk(
        ExprKind::Call {
            callee: Box::new(mk(ExprKind::Path(path(names, at)), at)),
            args: args.into_iter().map(CallArg::Positional).collect(),
            trailing_closure: None,
        },
        at,
    )
}

/// `core::fmt::<f>(args..)`, through its lang slot.
fn hook(f: &str, args: Vec<Expr>, at: Span) -> Expr {
    mk(
        ExprKind::Call {
            callee: Box::new(mk(
                ExprKind::Path(crate::lower::expr::lang_fn("fmt", f, at)),
                at,
            )),
            args: args.into_iter().map(CallArg::Positional).collect(),
            trailing_closure: None,
        },
        at,
    )
}

/// `unsafe { name(args) }`: a runtime entry (`core::rt`).
fn rt(name: &str, args: Vec<Expr>, at: Span) -> Expr {
    mk(
        ExprKind::Unsafe(nabic_ast::Block {
            stmts: Vec::new(),
            trailing: Some(Box::new(call(&[name], args, at))),
            span: at,
            label: None,
        }),
        at,
    )
}

fn path(names: &[&str], span: Span) -> AstPath {
    AstPath {
        segments: names
            .iter()
            .map(|n| nabic_ast::PathSegment {
                name: (*n).to_owned(),
                generics: Vec::new(),
            })
            .collect(),
        span,
        origin: None,
    }
}

fn int_lit(v: usize, at: Span) -> Expr {
    mk(ExprKind::Literal(AstLit::Int(v.to_string())), at)
}
