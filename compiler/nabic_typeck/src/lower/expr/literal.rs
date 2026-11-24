//! Literals: untyped values, lists, arrays and parsing.

#[allow(clippy::wildcard_imports)]
use crate::lower::expr::*;

/// An untyped literal built from integers only (`1`, `-1`, `1 + 1`).
pub(crate) fn is_untyped_int(e: &Expr) -> bool {
    fn no_float(e: &Expr) -> bool {
        match &e.kind {
            ExprKind::Literal(l) => !matches!(l, AstLit::Float(_)),
            ExprKind::OpPrefix { operand, .. } => no_float(operand),
            ExprKind::OpInfix { lhs, rhs, .. } => no_float(rhs) && no_float(lhs),
            _ => true,
        }
    }
    is_untyped_literal(e) && no_float(e)
}

/// An unsuffixed numeric literal (optionally negated): its type comes
/// from context.
pub(crate) fn is_untyped_literal(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Literal(AstLit::Int(t)) => !has_int_suffix(t),
        ExprKind::Literal(AstLit::Float(t)) => !has_float_suffix(t),
        ExprKind::OpPrefix { op, operand } if op == "-" => is_untyped_literal(operand),
        // `1 + 1`: arithmetic on untyped literals stays untyped.
        ExprKind::OpInfix { op, lhs, rhs }
            if matches!(op.as_str(), "+" | "-" | "*" | "/" | "%") =>
        {
            // The right side first: a left-leaning chain's is shallow.
            is_untyped_literal(rhs) && is_untyped_literal(lhs)
        }
        _ => false,
    }
}

/// Decode a `"..."` literal body. The lexer already rejected malformed
/// escapes, so the fallback is never taken.
pub(crate) fn decode_str_escapes(s: &str) -> String {
    nabic_lexer::escape::unescape(s, nabic_lexer::escape::EscapeMode::Str)
        .unwrap_or_else(|_| s.to_owned())
}

/// An unsuffixed number literal, `-` allowed: its text and span.
pub(crate) fn bare_num_lit(
    ctx: &LowerCtx<'_>,
    e: &Expr,
) -> Option<(String, nabic_diagnostic::Span)> {
    let lit = match &e.kind {
        ExprKind::OpPrefix { op, operand } if op == "-" => &**operand,
        _ => e,
    };
    let bare = match &lit.kind {
        ExprKind::Literal(AstLit::Int(t) | AstLit::Float(t)) => {
            !has_int_suffix(t.as_str()) && !has_float_suffix(t)
        }
        _ => false,
    };
    bare.then(|| Some((ctx.src(e.span)?.to_owned(), e.span)))
        .flatten()
}

/// `v` is a local whose type came from the literal in its `let`, and the
/// literal can take `to`: help that types the declaration instead of a
/// cast at each use. False when it does not apply.
pub(crate) fn lit_decl_help(ctx: &LowerCtx<'_>, v: &HirExpr, to: PrimTy) -> bool {
    match &v.kind {
        HirExprKind::Local(l) => lit_local_help(ctx, *l, &v.ty, to),
        _ => false,
    }
}

/// `lit_decl_help` for an operand of a mixed-number operator: the side
/// that is such a local takes the other side's type.
pub(crate) fn lit_operand_help(
    ctx: &LowerCtx<'_>,
    lhs: (&Expr, &HirTy),
    rhs: (&Expr, &HirTy),
) -> bool {
    for (side, other) in [(lhs, rhs), (rhs, lhs)] {
        let (ExprKind::Path(p), HirTy::Prim(to)) = (&side.0.kind, other.1) else {
            continue;
        };
        let local = match &p.segments[..] {
            [one] => ctx.lookup(&one.name),
            _ => None,
        };
        if local.is_some_and(|l| lit_local_help(ctx, l, side.1, *to)) {
            return true;
        }
    }
    false
}

pub(crate) fn lit_local_help(ctx: &LowerCtx<'_>, l: LocalId, ty: &HirTy, to: PrimTy) -> bool {
    if !matches!(ty, HirTy::Prim(p) if *p != to) {
        return false;
    }
    let Some((text, at)) = ctx.lit_lets.get(&l) else {
        return false;
    };
    let digits = text.trim_start_matches('-');
    let fits = match parse_int_literal(digits) {
        Some((n, _)) if !digits.contains('.') => {
            to.is_int() && int_fits(if text.starts_with('-') { -n } else { n }, to)
        }
        _ => to.is_float(),
    };
    let Some(name) = ctx.locals.get(l.0 as usize).and_then(|x| x.name.clone()) else {
        return false;
    };
    if !fits || name.starts_with('#') {
        return false;
    }
    let from = ctx.ty_name(ty);
    let t = to.name();
    ctx.help_last(Some(format!(
        "`{name}` is `{from}` from its literal; declare it as `{t}`: \
         `let {name}: {t} = {text};` or `let {name} = {text}{t};`"
    )));
    ctx.fix_last(
        *at,
        format!("{text}{t}"),
        format!("declare `{name}` as `{t}`"),
    );
    ctx.with_last(|d| {
        d.secondary.push(nabic_diagnostic::Label {
            span: *at,
            message: format!("`{name}` gets `{from}` here"),
        });
    });
    true
}

/// A float literal's suffix (`f32`, `f64`, `f16`, `bf16`) fixes its type.
pub(crate) fn has_float_suffix(text: &str) -> bool {
    nabic_lexer::lexer::FLOAT_SUFFIXES
        .iter()
        .any(|s| text.ends_with(s))
}

pub(crate) fn has_int_suffix(text: &str) -> bool {
    const SUFFIXES: &[&str] = &[
        "isize", "usize", "i64", "u64", "i32", "u32", "i16", "u16", "i8", "u8", "c_char",
    ];
    SUFFIXES.iter().any(|s| text.ends_with(s))
}

/// `b"..."`: a `[u8; N]` of its bytes; where a `Slice<u8>` is expected
/// (alone, or in a sum without an array member), a view of that array.
pub(crate) fn lower_byte_str(
    ctx: &mut LowerCtx<'_>,
    body: &str,
    span: nabic_diagnostic::Span,
) -> HirExpr {
    #[allow(clippy::cast_possible_truncation)] // the lexer allows up to `\xFF`
    let bytes: Vec<u8> =
        nabic_lexer::escape::unescape(body, nabic_lexer::escape::EscapeMode::CChar)
            .unwrap_or_default()
            .chars()
            .map(|c| c as u8)
            .collect();
    let Ok(n) = u32::try_from(bytes.len()) else {
        return ctx.error_expr(
            Code::LITERAL_OUT_OF_RANGE,
            span,
            "byte string is too long".to_owned(),
        );
    };
    let arr = HirExpr {
        ty: HirTy::Array(Box::new(HirTy::Prim(PrimTy::U8)), n),
        kind: HirExprKind::Const(ConstVal::Bytes(bytes)),
        span,
    };
    let is_byte_slice = |t: &HirTy| {
        matches!(t, HirTy::Struct(id) if {
            let d = ctx.struct_def(*id);
            enum_base_name(&d.name) == SLICE && d.generic_args == [HirTy::Prim(PrimTy::U8)]
        })
    };
    let view = match ctx.current_expected() {
        Some(t) if is_byte_slice(t) => true,
        Some(HirTy::Sum(vs)) => {
            vs.iter().any(is_byte_slice) && !vs.iter().any(|v| matches!(v, HirTy::Array(..)))
        }
        _ => false,
    };
    if !view {
        return arr;
    }
    let name = format!("#bs{}", ctx.locals.len());
    let local = ctx.declare(Some(name.clone()), arr.ty.clone());
    let slice = Expr {
        kind: ExprKind::Slice {
            target: Box::new(crate::lower::throws::path_expr(&name, span)),
            start: None,
            end: None,
            step: None,
        },
        span,
    };
    let trailing = lower_expected(ctx, &slice, HirTy::Unknown);
    HirExpr {
        ty: trailing.ty.clone(),
        kind: HirExprKind::Block(Box::new(HirBlock {
            stmts: vec![HirStmt::Let { local, init: arr }],
            trailing: Some(trailing),
            label: None,
        })),
        span,
    }
}

/// An unsuffixed numeric literal's value, read off the AST.
// Plain and negated literals only: `1 + 1` keeps its default type.
#[derive(Clone, Copy)]
pub(crate) enum LitVal {
    Int(i128),
    Float(f64),
}

pub(crate) fn untyped_lit_value(e: &Expr) -> Option<LitVal> {
    let (lit, neg) = match &e.kind {
        ExprKind::Literal(l) => (l, false),
        ExprKind::OpPrefix { op, operand } if op == "-" => match &operand.kind {
            ExprKind::Literal(l) => (l, true),
            _ => return None,
        },
        _ => return None,
    };
    if !is_untyped_literal(e) {
        return None;
    }
    match lit {
        AstLit::Int(t) => parse_int_literal(t).map(|(v, _)| LitVal::Int(if neg { -v } else { v })),
        AstLit::Float(t) => {
            let cleaned: String = t.chars().filter(|c| *c != '_').collect();
            cleaned.parse::<f64>().ok().map(LitVal::Float)
        }
        _ => None,
    }
}

/// Can a literal `v` take `ty`: an in-range integer type (or `int`), a
/// float type it stays finite in (or `float`).
pub(crate) fn literal_fits(ctx: &LowerCtx<'_>, v: LitVal, ty: &HirTy) -> bool {
    #[allow(clippy::cast_possible_truncation)]
    match (v, ty) {
        (LitVal::Int(v), HirTy::Prim(p)) => {
            p.is_int()
                && !matches!(p, PrimTy::Char | PrimTy::CChar)
                && crate::lower::pattern::int_fits(v, *p)
        }
        (LitVal::Float(f), HirTy::Prim(PrimTy::F32)) => (f as f32).is_finite(),
        (LitVal::Float(f), HirTy::Prim(PrimTy::F64)) => f.is_finite(),
        (LitVal::Float(f), _) if crate::lower::half::half_of(ctx, ty).is_some() => {
            crate::lower::half::fits(ctx, ty, f)
        }
        (LitVal::Int(_), _) => {
            crate::lower::bignum::big_of(ctx, ty) == Some(crate::lower::bignum::Big::Int)
        }
        (LitVal::Float(_), _) => {
            crate::lower::bignum::big_of(ctx, ty) == Some(crate::lower::bignum::Big::Float)
        }
    }
}

/// Which impl an unsuffixed literal takes among candidate slot types
/// `params` (`a * 2`, `T::from(2)`, `2.into()`): its default type if
/// offered, else the one it fits. `Ok(None)`: not a literal or nothing
/// fits; `Err`: several fit (named for the diagnostic).
pub(crate) fn pick_literal_impl(
    ctx: &LowerCtx<'_>,
    lit: &Expr,
    params: &[HirTy],
) -> Result<Option<usize>, String> {
    let Some(v) = untyped_lit_value(lit) else {
        return Ok(None);
    };
    let default = match v {
        LitVal::Int(_) => HirTy::Prim(DEFAULT_INT),
        LitVal::Float(_) => HirTy::Prim(PrimTy::F64),
    };
    if let Some(i) = params.iter().position(|p| *p == default) {
        if literal_fits(ctx, v, &default) {
            return Ok(Some(i));
        }
    }
    let mut fits: Vec<usize> = (0..params.len())
        .filter(|i| literal_fits(ctx, v, &params[*i]))
        .collect();
    // No integer impl: an integer literal fills a float slot it is exact in.
    if let (LitVal::Int(n), true) = (v, fits.is_empty()) {
        #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
        let exact = |p: &HirTy| match p {
            HirTy::Prim(PrimTy::F64) => (n as f64) as i128 == n,
            HirTy::Prim(PrimTy::F32) => (n as f32) as i128 == n,
            _ => false,
        };
        fits = (0..params.len()).filter(|i| exact(&params[*i])).collect();
    }
    match fits[..] {
        [] => Ok(None),
        [i] => Ok(Some(i)),
        _ => Err(fits
            .iter()
            .map(|i| format!("`{}`", ctx.ty_name(&params[*i])))
            .collect::<Vec<_>>()
            .join(", ")),
    }
}

/// `lit.into()` / `lit.try_into()` in a slot of `expected`: the literal
/// takes the `From` / `TryFrom` impl of `expected` it fits.
pub(crate) fn literal_conv_receiver(
    ctx: &mut LowerCtx<'_>,
    receiver: &Expr,
    method: &str,
) -> Option<HirExpr> {
    let conv = match method {
        "into" => "from",
        "try_into" => "try_from",
        _ => return None,
    };
    untyped_lit_value(receiver)?;
    let expected = ctx.current_expected()?.clone();
    let ids = ctx.env.methods_on(&expected, conv)?;
    let params: Vec<HirTy> = ids
        .iter()
        .filter_map(|id| ctx.sig_by_id(*id).param_tys.first().cloned())
        .collect();
    // Ambiguous or nothing fits: the ordinary lowering reports it.
    let i = pick_literal_impl(ctx, receiver, &params).ok()??;
    Some(lower_expected(ctx, receiver, params[i].clone()))
}

/// `[a, b]` is a `List`; a fixed array only in a slot typed `[T; N]`.
pub(crate) fn lower_list_lit(
    ctx: &mut LowerCtx<'_>,
    items: &[Expr],
    span: nabic_diagnostic::Span,
) -> HirExpr {
    if let Some(HirTy::Struct(id)) = ctx.current_expected().cloned() {
        let nd = ctx.lang("ndarray_type");
        if !nd.is_empty() && enum_base_name(&ctx.struct_def(id).name) == nd {
            return lower_nd_literal(ctx, items, id, span);
        }
    }
    let Some(HirTy::Array(e, _)) = ctx.current_expected().cloned() else {
        let open = ctx
            .current_expected()
            .is_none_or(|t| matches!(t, HirTy::Unknown | HirTy::Var(_)));
        let items = open
            .then(|| float_literals(items))
            .flatten()
            .unwrap_or_else(|| items.to_vec());
        let args = vec![nabic_ast::LitArg::Many(items)];
        return lower_shape(ctx, nabic_parser::Shape::List, args, "`List`", span);
    };
    let mut elem_ty = (*e).clone();
    let mut lowered: Vec<HirExpr> = Vec::with_capacity(items.len());
    for (i, e) in items.iter().enumerate() {
        let v = lower_expected(ctx, e, elem_ty.clone());
        // An open element type (`[T; N]` slot, `T` unbound; `[[T; M]; N]`
        // likewise) is the first element's.
        if open_array_elem(&elem_ty) {
            elem_ty = v.ty.clone();
        }
        lowered.push(coerce(ctx, &elem_ty, v, &|| format!("array element {i}")));
    }
    let len = u32::try_from(lowered.len()).expect("array literal length overflow");
    HirExpr {
        ty: HirTy::Array(Box::new(elem_ty.clone()), len),
        span,
        kind: HirExprKind::ArrayInit {
            elem_ty,
            elements: lowered,
        },
    }
}

fn open_array_elem(t: &HirTy) -> bool {
    match t {
        HirTy::Unknown => true,
        HirTy::Array(e, _) => open_array_elem(e),
        _ => false,
    }
}

/// `[0, 1.5]` (nested lists too) with no type to meet: a tree of number
/// literals holding a float literal is all float literals, its integer
/// literals typed as floats (`0` is `0.0`). `None`: not such a tree.
fn float_literals(items: &[Expr]) -> Option<Vec<Expr>> {
    fn scan(e: &Expr, ints: &mut bool, floats: &mut bool) -> bool {
        match &e.kind {
            ExprKind::List(xs) => xs.iter().all(|x| scan(x, ints, floats)),
            _ => match untyped_lit_value(e) {
                Some(LitVal::Int(_)) => {
                    *ints = true;
                    true
                }
                Some(LitVal::Float(_)) => {
                    *floats = true;
                    true
                }
                None => false,
            },
        }
    }
    fn conv(e: &mut Expr) {
        match &mut e.kind {
            ExprKind::List(xs) => xs.iter_mut().for_each(conv),
            ExprKind::OpPrefix { operand, .. } => conv(operand),
            ExprKind::Literal(AstLit::Int(t)) => {
                if let Some((v, _)) = parse_int_literal(t) {
                    e.kind = ExprKind::Literal(AstLit::Float(format!("{v}.0")));
                }
            }
            _ => {}
        }
    }
    let (mut ints, mut floats) = (false, false);
    if !(items.iter().all(|e| scan(e, &mut ints, &mut floats)) && ints && floats) {
        return None;
    }
    let mut out = items.to_vec();
    out.iter_mut().for_each(conv);
    Some(out)
}

/// A nested list literal in an `Array<T, N>` slot: the nesting gives the
/// shape (checked here, so a ragged literal is a compile error), the
/// leaves are the elements in row-major order:
/// `array_literal([rows, cols], e00, e01, ..)`.
pub(crate) fn lower_nd_literal(
    ctx: &mut LowerCtx<'_>,
    items: &[Expr],
    id: StructId,
    span: nabic_diagnostic::Span,
) -> HirExpr {
    let rank = ctx
        .struct_def(id)
        .fields
        .iter()
        .find(|f| f.name == "shape")
        .and_then(|f| match &f.ty {
            HirTy::Array(_, n) => Some(*n as usize),
            _ => None,
        })
        .unwrap_or(1);
    let mut shape: Vec<Option<usize>> = vec![None; rank];
    let mut flat: Vec<Expr> = Vec::new();
    // (span of the row, what is wrong, what to write).
    let mut bad: Option<(nabic_diagnostic::Span, String, String)> = None;
    let mut stack: Vec<(&[Expr], usize, nabic_diagnostic::Span)> = vec![(items, 0, span)];
    while let Some((row, depth, at)) = stack.pop() {
        match shape.get(depth).copied().flatten() {
            Some(n) if n != row.len() => {
                bad = Some((
                    at,
                    format!(
                        "this row has {} elements, the first row at its depth has {n}",
                        row.len()
                    ),
                    "give every row the same length, or keep ragged rows in a \
                     `List<List<T>>`"
                        .to_owned(),
                ));
                break;
            }
            _ => {
                if let Some(s) = shape.get_mut(depth) {
                    *s = Some(row.len());
                }
            }
        }
        if depth + 1 == rank {
            flat.extend(row.iter().cloned());
            continue;
        }
        // Children pushed in reverse so they come off in order.
        for e in row.iter().rev() {
            if let ExprKind::List(inner) = &e.kind {
                stack.push((inner, depth + 1, e.span));
            } else {
                bad = Some((
                    e.span,
                    format!(
                        "an `Array` of rank {rank} is written with {rank} levels of `[..]`, and this element is not a list"
                    ),
                    "write the row as a `[..]` literal, or build the array with \
                         `Array::from_list(xs, shape)`"
                        .to_owned(),
                ));
                break;
            }
        }
        if bad.is_some() {
            break;
        }
    }
    if let Some((at, msg, help)) = bad {
        ctx.emit_error_help(Code::RAGGED_ARRAY_LITERAL, at, msg, help);
        for e in items {
            let _ = lower_expected(ctx, e, HirTy::Unknown);
        }
        return poison(span);
    }
    let usize_lit = |n: usize| Expr {
        kind: ExprKind::Literal(AstLit::Int(format!("{n}usize"))),
        span,
    };
    let dims: Vec<Expr> = shape.iter().map(|n| usize_lit(n.unwrap_or(0))).collect();
    let mut args = vec![CallArg::Positional(Expr {
        kind: ExprKind::List(dims),
        span,
    })];
    // Each element typed by the slot's `T`, so `[[1, 2]]` in an
    // `Array<i64, 2>` slot is `i64`s.
    let elem = ctx
        .struct_def(id)
        .fields
        .iter()
        .find(|f| f.name == "buf")
        .and_then(|f| match &f.ty {
            HirTy::GcBuf(t) => Some((**t).clone()),
            _ => None,
        })
        .unwrap_or(HirTy::Unknown);
    for (i, e) in flat.iter().enumerate() {
        let h = lower_expected(ctx, e, elem.clone());
        let h = if elem == HirTy::Unknown {
            h
        } else {
            coerce(ctx, &elem, h, &|| format!("array element {i}"))
        };
        args.push(CallArg::Positional(splice(ctx, h)));
    }
    let call = Expr {
        kind: ExprKind::Call {
            callee: Box::new(Expr {
                kind: ExprKind::Path(lang_fn("array", "array_literal", span)),
                span,
            }),
            args,
            trailing_closure: None,
        },
        span,
    };
    // The slot fixes `T` and `N`, also for an empty literal.
    let expected = HirTy::Struct(id);
    let name = ctx.lang("array_literal_fn").to_owned();
    if let Some(def) = ctx.env.generic_fn(&name) {
        if let Some(ret) = def.ret_ty.as_ref() {
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
    }
    lower_expected(ctx, &call, expected)
}

/// How to make a list of `len` elements `value`: `List::filled` copies a
/// plain value; a list or other shared value gets a new one each time,
/// which only a loop makes.
fn repeat_list_help(
    ctx: &LowerCtx<'_>,
    value: &Expr,
    len: &nabic_ast::ConstExpr,
) -> Option<String> {
    let v = ctx.src(value.span)?;
    let n = match len {
        nabic_ast::ConstExpr::Expr(x) => ctx.src(x.span)?.to_owned(),
        nabic_ast::ConstExpr::Literal(AstLit::Int(t)) => t.clone(),
        nabic_ast::ConstExpr::Path(p) => p
            .segments
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
            .join("::"),
        nabic_ast::ConstExpr::Literal(_) => return None,
    };
    let shared = matches!(
        value.kind,
        ExprKind::List(_)
            | ExprKind::Dict(_)
            | ExprKind::Set(_)
            | ExprKind::Call { .. }
            | ExprKind::MethodCall { .. }
            | ExprKind::StructInit { .. }
    );
    Some(if shared {
        format!(
            "make a list with a new `{v}` in each place: `let xs = []; for _ in 0..{n} {{ \
             xs.push({v}); }}` (`List::filled` would share one)"
        )
    } else {
        format!("make a list: `List::filled({n}, {v})`")
    })
}

/// `[v; N]`: `v` evaluated once, then copied into each element (one node
/// per element up to `REPEAT_UNROLL`, a fill loop beyond). An element a
/// bitwise copy does not duplicate gets `v.clone()` in elements `0..N-1`
/// and `v` itself in the last.
pub(crate) fn lower_array_repeat(
    ctx: &mut LowerCtx<'_>,
    value: &Expr,
    len: &nabic_ast::ConstExpr,
    fill: bool,
    span: nabic_diagnostic::Span,
) -> HirExpr {
    let elem_hint = match ctx.current_expected() {
        Some(HirTy::Array(e, _)) => (**e).clone(),
        _ => HirTy::Unknown,
    };
    // `let xs: List<T> = [v; 3];`: `[v; N]` is a fixed array.
    if let Some(want @ HirTy::Struct(_)) = ctx.current_expected().cloned() {
        if ctx.ty_name(&want).starts_with("List<") {
            let e = ctx.error_expr(
                Code::MISMATCHED_TYPES,
                span,
                format!("`[v; N]` is a fixed array, not a `{}`", ctx.ty_name(&want)),
            );
            ctx.help_last(repeat_list_help(ctx, value, len));
            return e;
        }
    }
    let v = lower_expected(ctx, value, elem_hint.clone());
    let v = if elem_hint == HirTy::Unknown {
        v
    } else {
        coerce(ctx, &elem_hint, v, &|| "array repeat value".to_owned())
    };
    let n = ctx.eval_const_msg(len);
    let Some(n) = n.as_ref().ok().and_then(|&n| u32::try_from(n).ok()) else {
        let panicked = n.as_ref().err().and_then(|w| w.strip_prefix("panicked: "));
        let msg = match panicked {
            Some(m) => format!("evaluating the length of `[v; N]` panicked: {m}"),
            None => "the length of `[v; N]` must be a non-negative compile-time integer".to_owned(),
        };
        let e = ctx.error_expr(Code::CONST_EVAL, span, msg);
        if panicked.is_some() {
            ctx.help_last(Some(
                "the length runs at compile time: pass a value its `const fn` accepts, or a \
                 literal"
                    .to_owned(),
            ));
        } else if let nabic_ast::ConstExpr::Expr(x) = len {
            // A length known at run time is a list's.
            let help = crate::lower::consteval::layout_help(ctx, x).or_else(|| {
                let n = ctx.src(x.span)?;
                let make = repeat_list_help(ctx, value, len)?;
                Some(format!(
                    "an array's length is fixed when it compiles; for `{n}` known at run time, \
                     {make}"
                ))
            });
            ctx.help_last(help);
        }
        return e;
    };
    let elem_ty = v.ty.clone();
    if !fill {
        let copies = repeat_copies(ctx, &elem_ty, 0);
        if let Some(err) = repeat_clone_error(ctx, &elem_ty, copies, len, n, span) {
            return err;
        }
        if !copies {
            return repeat_cloned(ctx, v, n, span);
        }
    }
    let tmp = ctx.declare(None, elem_ty.clone());
    let ty = HirTy::Array(Box::new(elem_ty.clone()), n);
    HirExpr {
        kind: HirExprKind::Block(Box::new(HirBlock {
            stmts: vec![HirStmt::Let {
                local: tmp,
                init: v,
            }],
            trailing: Some(HirExpr {
                kind: if n > REPEAT_UNROLL {
                    HirExprKind::ArrayRepeat {
                        value: Box::new(hir_local(tmp, &elem_ty, span)),
                        elem_ty,
                        len: n,
                    }
                } else {
                    HirExprKind::ArrayInit {
                        elements: (0..n).map(|_| hir_local(tmp, &elem_ty, span)).collect(),
                        elem_ty,
                    }
                },
                ty: ty.clone(),
                span,
            }),
            label: None,
        })),
        ty,
        span,
    }
}

/// Does a bitwise copy of a `ty` make an independent value: `Copy`, or
/// nothing in it reaches shared mutable storage? An open type counts as
/// one (an untyped literal).
pub(crate) fn repeat_copies(ctx: &LowerCtx<'_>, ty: &HirTy, depth: u32) -> bool {
    if depth > DROP_GLUE_DEPTH {
        return false;
    }
    let rec = |t: &HirTy| repeat_copies(ctx, t, depth + 1);
    let ty = crate::lower::infer::resolve(ctx, ty);
    match &ty {
        HirTy::Unknown
        | HirTy::Any
        | HirTy::Var(_)
        | HirTy::Prim(_)
        | HirTy::Unit
        | HirTy::None
        | HirTy::NoThrow
        | HirTy::Fn(..)
        | HirTy::Closure { .. } => true,
        HirTy::Array(e, _) => rec(e),
        HirTy::Tuple(ts) | HirTy::Sum(ts) => ts.iter().all(rec),
        _ if ctx.has_trait(&ty, COPY_TRAIT) => true,
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

/// `[v; N]` of an element `clone()` cannot copy: an owned resource (one
/// owner per value), or a type without `Clone`.
pub(crate) fn repeat_clone_error(
    ctx: &LowerCtx<'_>,
    elem_ty: &HirTy,
    copies: bool,
    len: &nabic_ast::ConstExpr,
    n: u32,
    span: nabic_diagnostic::Span,
) -> Option<HirExpr> {
    let mut inner = elem_ty;
    while let HirTy::Array(e, _) = inner {
        inner = e;
    }
    let name = ctx.ty_name(elem_ty);
    let msg = if crate::lower::affine::affine(ctx, inner).is_some() {
        format!(
            "`[v; N]` copies `v` into each element, but {} has one owner",
            nabic_diagnostic::article(&ctx.ty_name(inner))
        )
    } else {
        let clone = ctx.lang("clone_trait");
        if copies || clone.is_empty() || ctx.has_trait(elem_ty, clone) {
            return None;
        }
        format!("`[v; N]` copies `v` into each element with `clone()`, but `{name}` is not `Clone`")
    };
    let len = match len {
        nabic_ast::ConstExpr::Literal(_) => n.to_string(),
        nabic_ast::ConstExpr::Path(p) => {
            let names: Vec<&str> = p.segments.iter().map(|s| s.name.as_str()).collect();
            names.join("::")
        }
        nabic_ast::ConstExpr::Expr(e) => nabic_ast::print::print_expr(e),
    };
    let from_fn =
        format!("build each element on its own: `array::from_fn::<{name}, {len}>(|_| ..)`");
    let help = match elem_ty {
        HirTy::Param(_) => format!("add a `{name}: Clone` bound, or {from_fn}"),
        _ => from_fn,
    };
    let err = ctx.error_expr(Code::UNSATISFIED_BOUND, span, msg);
    ctx.help_last(Some(help));
    Some(err)
}

/// `{ let #r = v; let #a = [#r; n] (a fill); for #i in 0..n - 1 {
/// #a[#i] = #r.clone(); } #a }`.
pub(crate) fn repeat_cloned(
    ctx: &mut LowerCtx<'_>,
    v: HirExpr,
    n: u32,
    span: nabic_diagnostic::Span,
) -> HirExpr {
    let rep = tmp_path(ctx, "rep", span);
    let arr = tmp_path(ctx, "repa", span);
    let idx = tmp_path(ctx, "repi", span);
    let ast = |kind: ExprKind| Expr { kind, span };
    let usize_lit = |n: u32| ast(ExprKind::Literal(AstLit::Int(format!("{n}usize"))));
    let init = splice(ctx, v);
    let clone = ast(ExprKind::MethodCall {
        receiver: Box::new(rep.clone()),
        method: "clone".to_owned(),
        type_args: Vec::new(),
        args: Vec::new(),
        trailing_closure: None,
        origin: None,
        written: false,
    });
    let store = Stmt {
        kind: StmtKind::Assign(spread_item_at(&path_name(&arr), &idx, span), clone),
        span,
    };
    let fill = ast(ExprKind::ArrayRepeat {
        value: Box::new(rep.clone()),
        len: nabic_ast::ConstExpr::Literal(AstLit::Int(n.to_string())),
        fill: true,
    });
    let each = ast(ExprKind::For {
        label: None,
        pat: nabic_ast::Pattern {
            kind: nabic_ast::PatternKind::Binding {
                name: path_name(&idx),
                sub: None,
            },
            span,
        },
        iter: Box::new(ast(ExprKind::Range {
            start: Some(Box::new(usize_lit(0))),
            end: Some(Box::new(usize_lit(n.saturating_sub(1)))),
            inclusive: false,
        })),
        body: nabic_ast::Block {
            stmts: vec![store],
            trailing: None,
            span,
            label: None,
        },
        is_await: false,
        by_ref: false,
    });
    let stmts = vec![
        let_stmt(&rep, init),
        let_stmt(&arr, fill),
        Stmt {
            kind: StmtKind::Expr(each),
            span,
        },
    ];
    lower_expr(ctx, &block_of(stmts, arr, span))
}

/// Longest `[v; N]` lowered element by element.
pub(crate) const REPEAT_UNROLL: u32 = 16;

/// Integer type an unsuffixed literal takes from its expected slot.
pub(crate) fn expected_prim(ctx: &LowerCtx<'_>, want: fn(PrimTy) -> bool) -> Option<PrimTy> {
    match &crate::lower::infer::zonk(ctx, ctx.current_expected()?) {
        HirTy::Prim(p) if want(*p) => Some(*p),
        // `let x: i64 | none = 5`: the sum's single matching variant, in a nested
        // layer too (`(i32 | none)?`).
        HirTy::Sum(vs) => {
            fn prims(vs: &[HirTy], want: fn(PrimTy) -> bool, out: &mut Vec<PrimTy>) {
                for v in vs {
                    match v {
                        HirTy::Prim(p) if want(*p) => out.push(*p),
                        HirTy::Sum(ws) => prims(ws, want, out),
                        _ => {}
                    }
                }
            }
            let mut hits = Vec::new();
            prims(vs, want, &mut hits);
            match hits[..] {
                [one] => Some(one),
                _ => None,
            }
        }
        _ => None,
    }
}

/// An unsuffixed integer into a sum with several integer members and the
/// float member `fty`: an error, the literal names no member.
pub(crate) fn ambiguous_int(
    ctx: &LowerCtx<'_>,
    text: &str,
    fty: PrimTy,
    span: nabic_diagnostic::Span,
) {
    let Some(HirTy::Sum(vs)) = ctx.current_expected() else {
        return;
    };
    let ints: Vec<&str> = vs
        .iter()
        .filter_map(|v| match v {
            HirTy::Prim(p) if p.is_int() && !matches!(p, PrimTy::CChar | PrimTy::Char) => {
                Some(p.name())
            }
            _ => None,
        })
        .collect();
    if ints.len() < 2 {
        return;
    }
    let sum = ctx.ty_name(&HirTy::Sum(vs.clone()));
    let picks: Vec<String> = ints.iter().map(|t| format!("`{text}{t}`")).collect();
    ctx.emit_error_help(
        Code::AMBIGUOUS_LITERAL,
        span,
        format!(
            "ambiguous literal: integer literal `{text}` into `{sum}`, which has more than one \
             integer member"
        ),
        format!(
            "write {}, or `{text}.0` for the `{}`",
            picks.join(" or "),
            fty.name()
        ),
    );
}

/// A literal (`negate`: under a unary minus). Unsuffixed numbers take the
/// expected type (else `i32` / `f64`); integers are range-checked.
pub(crate) fn lower_literal(
    ctx: &LowerCtx<'_>,
    lit: &AstLit,
    negate: bool,
    span: nabic_diagnostic::Span,
) -> HirExpr {
    let konst = |kind: ConstVal, ty: HirTy| HirExpr {
        kind: HirExprKind::Const(kind),
        ty,
        span,
    };
    match lit {
        AstLit::Int(text) => {
            if let Some((digits, wide, narrow)) = ["i128", "u128"]
                .into_iter()
                .find_map(|w| Some((text.strip_suffix(w)?, w, crate::lower::wide_int_narrow(w)?)))
            {
                // Offer the 64-bit suffix only where the value fits it.
                let max = if narrow == "u64" {
                    u64::MAX.into()
                } else {
                    i128::from(i64::MAX)
                };
                let fits = parse_int_literal(digits)
                    .is_some_and(|(v, _)| v <= max + i128::from(negate && narrow == "i64"));
                let narrow = if fits {
                    format!("`{digits}{narrow}`, or ")
                } else {
                    String::new()
                };
                return ctx.error_expr(
                    Code::LITERAL_OUT_OF_RANGE,
                    span,
                    format!(
                        "integer literal `{text}`: Nabi has no `{wide}`; write {narrow}`{digits}` \
                         where an `int` is expected (any size)"
                    ),
                );
            }
            let Some((value, suffix_ty)) = parse_int_literal(text) else {
                return ctx.error_expr(
                    Code::LITERAL_OUT_OF_RANGE,
                    span,
                    format!("integer literal `{text}` is too large (an `int` holds any size)"),
                );
            };
            let value = if negate { -value } else { value };
            // An unsuffixed integer in a float slot is that float if exact
            // (`let x: f64 = 1`); an integer member of an expected sum wins
            // (`i32 | f64`). `char` takes no integer literal.
            let int_slot = |p: PrimTy| p.is_int() && !matches!(p, PrimTy::CChar | PrimTy::Char);
            if let Some(fty) = (!has_int_suffix(text) && expected_prim(ctx, int_slot).is_none())
                .then(|| expected_prim(ctx, PrimTy::is_float))
                .flatten()
            {
                #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
                let (bits, exact) = if fty == PrimTy::F32 {
                    let f = value as f32;
                    (u64::from(f.to_bits()), f as i128 == value)
                } else {
                    let f = value as f64;
                    (f.to_bits(), f as i128 == value)
                };
                if !exact {
                    return ctx.error_expr(
                        Code::LITERAL_OUT_OF_RANGE,
                        span,
                        format!(
                            "integer literal `{text}` is not exactly representable as `{}`",
                            fty.name()
                        ),
                    );
                }
                ambiguous_int(ctx, text, fty, span);
                return konst(ConstVal::Float { bits, ty: fty }, HirTy::Prim(fty));
            }
            let ty = if has_int_suffix(text) {
                suffix_ty
            } else {
                expected_prim(ctx, int_slot).unwrap_or(DEFAULT_INT)
            };
            if negate && !ty.is_signed_int() {
                return ctx.error_expr(
                    Code::LITERAL_OUT_OF_RANGE,
                    span,
                    format!(
                        "cannot negate unsigned literal `{text}` of type `{}`",
                        ty.name()
                    ),
                );
            }
            if !int_fits(value, ty) {
                let shown = if negate {
                    format!("-{text}")
                } else {
                    text.clone()
                };
                return ctx.error_expr(
                    Code::LITERAL_OUT_OF_RANGE,
                    span,
                    format!("integer literal `{shown}` out of range for `{}`", ty.name()),
                );
            }
            let open = !has_int_suffix(text) && expected_prim(ctx, int_slot).is_none();
            if open {
                ctx.env.sink.default_lits.borrow_mut().insert(span);
            }
            konst(
                ConstVal::Int { value, ty },
                crate::lower::infer::literal_ty(ctx, span, ty, open, negate),
            )
        }
        AstLit::Float(text) => {
            let suffixed = has_float_suffix(text);
            let ty = if suffixed {
                parse_float_literal(text).map_or(PrimTy::F64, |(_, t)| t)
            } else {
                expected_prim(ctx, PrimTy::is_float).unwrap_or(PrimTy::F64)
            };
            let digits = text.trim_end_matches("f32").trim_end_matches("f64");
            let cleaned: String = digits.chars().filter(|c| *c != '_').collect();
            let Ok(v) = cleaned.parse::<f64>() else {
                return ctx.error_expr(
                    Code::MALFORMED_NUMBER,
                    span,
                    format!("malformed float literal `{text}`"),
                );
            };
            // Once from the decimal, not through `f64`.
            let narrow = cleaned.parse::<f32>().unwrap_or(f32::INFINITY);
            if v.is_infinite() || (ty == PrimTy::F32 && narrow.is_infinite()) {
                return ctx.error_expr(
                    Code::LITERAL_OUT_OF_RANGE,
                    span,
                    format!("float literal out of range for `{}`", ty.name()),
                );
            }
            let v = if negate { -v } else { v };
            let bits = if ty == PrimTy::F32 {
                u64::from((if negate { -narrow } else { narrow }).to_bits())
            } else {
                v.to_bits()
            };
            let open = !suffixed && expected_prim(ctx, PrimTy::is_float).is_none();
            if open {
                ctx.env.sink.default_lits.borrow_mut().insert(span);
            }
            konst(
                ConstVal::Float { bits, ty },
                crate::lower::infer::literal_ty(ctx, span, ty, open, negate),
            )
        }
        AstLit::Bool(b) => konst(ConstVal::Bool(*b), HirTy::Prim(PrimTy::Bool)),
        AstLit::Str(s) => konst(
            ConstVal::Str(decode_str_escapes(s)),
            HirTy::Prim(PrimTy::Str),
        ),
        AstLit::RawStr(s) => konst(ConstVal::Str(s.clone()), HirTy::Prim(PrimTy::Str)),
        AstLit::None => konst(ConstVal::None, HirTy::None),
        AstLit::Char(s) => match parse_char_literal(s) {
            // An ASCII `'a'` flowing into a `c_char` slot takes that type.
            Some(c) if c < 0x80 && expected_prim(ctx, |p| p == PrimTy::CChar).is_some() => {
                konst(ConstVal::Char(c), HirTy::Prim(PrimTy::CChar))
            }
            Some(c) => konst(ConstVal::Char(c), HirTy::Prim(PrimTy::Char)),
            None => ctx.error_expr(
                Code::CHAR_LITERAL_LENGTH,
                span,
                format!("unsupported character literal `'{s}'`"),
            ),
        },
        AstLit::CChar(s) => match parse_char_literal(s) {
            Some(c) => konst(ConstVal::Char(c), HirTy::Prim(PrimTy::CChar)),
            None => ctx.error_expr(
                Code::CHAR_LITERAL_LENGTH,
                span,
                format!("unsupported character literal `c'{s}'`"),
            ),
        },
        // Reached only under a unary minus (`lower_byte_str` otherwise).
        AstLit::ByteStr(_) => ctx.error_expr(
            Code::INVALID_OPERATOR,
            span,
            "cannot negate a byte string".to_owned(),
        ),
    }
}

/// `'A'` / `c'A'` payload is the body as written (without quotes); decode
/// it with the shared escape table.
pub(crate) fn parse_char_literal(text: &str) -> Option<u32> {
    let decoded =
        nabic_lexer::escape::unescape(text, nabic_lexer::escape::EscapeMode::CChar).ok()?;
    let mut chars = decoded.chars();
    let c = chars.next()?;
    chars.next().is_none().then_some(c as u32)
}

/// A literal typed by default meeting a number slot of its kind (a
/// generic call's argument once the parameter is known) takes the slot's
/// type when its value fits.
pub(crate) fn retype_default_literal(ctx: &LowerCtx<'_>, slot: &HirTy, mut v: HirExpr) -> HirExpr {
    let HirTy::Prim(to) = *slot else {
        return v;
    };
    if v.ty == *slot || !ctx.env.sink.default_lits.borrow().contains(&v.span) {
        return v;
    }
    match &mut v.kind {
        HirExprKind::Const(ConstVal::Int { value, ty })
            if to.is_int()
                && !matches!(to, PrimTy::Char | PrimTy::CChar)
                && crate::lower::pattern::int_fits(*value, to) =>
        {
            *ty = to;
            v.ty = slot.clone();
        }
        HirExprKind::Const(ConstVal::Float { bits, ty })
            if to == PrimTy::F32 && *ty == PrimTy::F64 =>
        {
            #[allow(clippy::cast_possible_truncation)]
            let cast = f64::from_bits(*bits) as f32;
            // Once from the decimal where the source still says it (the
            // two differ by an ulp at most, on a tie of `f32`).
            let once = ctx
                .src(v.span)
                .map(|t| t.replace('_', ""))
                .and_then(|t| t.parse::<f32>().ok())
                .filter(|f| f.to_bits().abs_diff(cast.to_bits()) <= 1);
            let f = once.unwrap_or(cast);
            *bits = u64::from(f.to_bits());
            *ty = to;
            v.ty = slot.clone();
        }
        _ => {}
    }
    v
}

/// The type of an unsuffixed integer literal nothing fixes.
pub(crate) const DEFAULT_INT: PrimTy = PrimTy::I64;

/// Parse an integer literal; an unsuffixed one is `DEFAULT_INT`.
pub(crate) fn parse_int_literal(text: &str) -> Option<(i128, PrimTy)> {
    const SUFFIXES: &[(&str, PrimTy)] = &[
        ("isize", PrimTy::Isize),
        ("usize", PrimTy::Usize),
        ("i64", PrimTy::I64),
        ("u64", PrimTy::U64),
        ("i32", PrimTy::I32),
        ("u32", PrimTy::U32),
        ("i16", PrimTy::I16),
        ("u16", PrimTy::U16),
        ("i8", PrimTy::I8),
        ("u8", PrimTy::U8),
        ("c_char", PrimTy::CChar),
    ];

    let (digits, ty) = SUFFIXES
        .iter()
        .find_map(|(suf, ty)| text.strip_suffix(suf).map(|d| (d, *ty)))
        .unwrap_or((text, DEFAULT_INT));

    let cleaned: String = digits.chars().filter(|c| *c != '_').collect();
    let (radix, body) = if let Some(rest) = cleaned
        .strip_prefix("0x")
        .or_else(|| cleaned.strip_prefix("0X"))
    {
        (16u32, rest.to_owned())
    } else if let Some(rest) = cleaned
        .strip_prefix("0b")
        .or_else(|| cleaned.strip_prefix("0B"))
    {
        (2u32, rest.to_owned())
    } else if let Some(rest) = cleaned
        .strip_prefix("0o")
        .or_else(|| cleaned.strip_prefix("0O"))
    {
        (8u32, rest.to_owned())
    } else {
        (10u32, cleaned)
    };

    let value = i128::from_str_radix(&body, radix).ok()?;
    Some((value, ty))
}

pub(crate) fn parse_float_literal(text: &str) -> Option<(u64, PrimTy)> {
    const SUFFIXES: &[(&str, PrimTy)] = &[("f32", PrimTy::F32), ("f64", PrimTy::F64)];
    let (digits, ty) = SUFFIXES
        .iter()
        .find_map(|(suf, ty)| text.strip_suffix(suf).map(|d| (d, *ty)))
        .unwrap_or((text, PrimTy::F64));

    let cleaned: String = digits.chars().filter(|c| *c != '_').collect();
    let bits = match ty {
        PrimTy::F32 => u64::from(cleaned.parse::<f32>().ok()?.to_bits()),
        _ => cleaned.parse::<f64>().ok()?.to_bits(),
    };
    Some((bits, ty))
}
