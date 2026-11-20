//! Assignment, compound assignment and index stores.

#[allow(clippy::wildcard_imports)]
use crate::lower::expr::*;

/// `target op= value;`
pub(crate) fn lower_compound_assign(
    ctx: &mut LowerCtx<'_>,
    target: &Expr,
    op: &str,
    value: &Expr,
    span: nabic_diagnostic::Span,
) -> Option<HirStmt> {
    // `.=` `.+=` ..: element-wise into the target; a slice of an array
    // is a view, so writing it writes the array.
    let view = matches!(
        target.kind,
        ExprKind::Index(..) | ExprKind::Slice { .. } | ExprKind::AxisSlice { .. }
    );
    if !(op.starts_with('.') && view || is_place(ctx, target, op, value)) {
        return None;
    }
    // The place runs once: its calls and computed indexes go to temps.
    let mut lets = Vec::new();
    let hoisted = hoist_target(ctx, target, &mut lets);
    // `.=` evaluates the value's operands first, then these.
    if op == ".=" {
        return Some(HirStmt::Expr(crate::lower::broadcast::lower_into(
            ctx, target, op, value, span, lets, &hoisted,
        )));
    }
    if !lets.is_empty() {
        lets.push(Stmt {
            span,
            kind: StmtKind::CompoundAssign {
                target: hoisted,
                op: op.to_owned(),
                value: value.clone(),
            },
        });
        let block = Expr {
            kind: ExprKind::Block(Block {
                stmts: lets,
                trailing: None,
                span,
                label: None,
            }),
            span,
        };
        return Some(HirStmt::Expr(lower_expr(ctx, &block)));
    }
    if op.starts_with('.') {
        return Some(HirStmt::Expr(crate::lower::broadcast::lower_into(
            ctx,
            target,
            op,
            value,
            span,
            Vec::new(),
            target,
        )));
    }
    if let Some(call) = assign_trait_call(ctx, target, op, value, span) {
        return Some(HirStmt::Expr(call));
    }
    // Without an `*Assign` impl, `a = a op b` (also a primitive's intrinsic
    // impl: its call is the op).
    let combined = Expr {
        span,
        kind: ExprKind::OpInfix {
            op: op.trim_end_matches('=').to_owned(),
            lhs: Box::new(target.clone()),
            rhs: Box::new(value.clone()),
        },
    };
    let a = lower_assign(ctx, target, &combined, span);
    write_through(ctx, a, span)
}

/// The target of `op=`: the field chain down to its root stays as
/// written (`is_place` has judged it); the rest is `hoist_place`.
pub(crate) fn hoist_target(ctx: &LowerCtx<'_>, e: &Expr, lets: &mut Vec<Stmt>) -> Expr {
    let kind = match &e.kind {
        ExprKind::Field(base, name) => {
            ExprKind::Field(Box::new(hoist_target(ctx, base, lets)), name.clone())
        }
        ExprKind::TupleField(base, i) => {
            ExprKind::TupleField(Box::new(hoist_target(ctx, base, lets)), *i)
        }
        ExprKind::Index(..) => return hoist_place(ctx, e, lets),
        other => other.clone(),
    };
    Expr { kind, span: e.span }
}

/// A place with each side-effecting part moved into a `let` temp, so
/// it runs once; names, `self`, literals and field chains on them stay.
pub(crate) fn hoist_place(ctx: &LowerCtx<'_>, e: &Expr, lets: &mut Vec<Stmt>) -> Expr {
    let kind = match &e.kind {
        ExprKind::Path(_) | ExprKind::SelfLower | ExprKind::SelfUpper | ExprKind::Literal(_) => {
            return e.clone();
        }
        ExprKind::Field(base, name) => {
            ExprKind::Field(Box::new(hoist_place(ctx, base, lets)), name.clone())
        }
        ExprKind::TupleField(base, i) => {
            ExprKind::TupleField(Box::new(hoist_place(ctx, base, lets)), *i)
        }
        ExprKind::Index(base, idx) => {
            let base = hoist_place(ctx, base, lets);
            // `d[k, default: v]`: the key once, as a plain index.
            let idx = match nabic_ast::as_index_default(idx) {
                Some((k, v)) => nabic_ast::index_default(hoist_place(ctx, k, lets), v.clone()),
                None => hoist_place(ctx, idx, lets),
            };
            ExprKind::Index(Box::new(base), Box::new(idx))
        }
        // The parts of `m[a:b, i]`, each once; the shapes stay so the
        // index keeps its axis arguments.
        ExprKind::Tuple(parts) => {
            ExprKind::Tuple(parts.iter().map(|p| hoist_place(ctx, p, lets)).collect())
        }
        ExprKind::AxisSlice { start, end, step } => {
            let mut once =
                |x: &Option<Box<Expr>>| x.as_ref().map(|x| Box::new(hoist_place(ctx, x, lets)));
            let (start, end, step) = (once(start), once(end), once(step));
            ExprKind::AxisSlice { start, end, step }
        }
        ExprKind::Range {
            start: None,
            end: None,
            ..
        } => return e.clone(),
        _ => {
            let t = tmp_path(ctx, "at", e.span);
            lets.push(let_stmt(&t, e.clone()));
            return t;
        }
    };
    Expr { kind, span: e.span }
}

/// Evaluating `e` has no effect and cannot fail.
fn trivial(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Literal(_) | ExprKind::Path(_) | ExprKind::SelfLower => true,
        ExprKind::Field(x, _) | ExprKind::TupleField(x, _) => trivial(x),
        ExprKind::OpPrefix { operand, .. } => matches!(operand.kind, ExprKind::Literal(_)),
        _ => false,
    }
}

/// `e` calls nothing, so it changes no variable (it can still panic).
fn inert(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Literal(_) | ExprKind::Path(_) | ExprKind::SelfLower => true,
        ExprKind::Field(x, _) | ExprKind::TupleField(x, _) | ExprKind::Cast(x, _) => inert(x),
        ExprKind::OpPrefix { operand, .. } => inert(operand),
        ExprKind::OpInfix { lhs, rhs, .. } | ExprKind::Index(lhs, rhs) => inert(lhs) && inert(rhs),
        ExprKind::Tuple(xs) | ExprKind::List(xs) => xs.iter().all(inert),
        _ => false,
    }
}

/// A variable or a field chain on one, indexed at most once at the end
/// by a trivial index: nothing in it runs before the store.
fn plain(e: &Expr) -> bool {
    fn chain(e: &Expr) -> bool {
        match &e.kind {
            ExprKind::Path(_) | ExprKind::SelfLower => true,
            ExprKind::Field(x, _) | ExprKind::TupleField(x, _) => chain(x),
            _ => false,
        }
    }
    match &e.kind {
        ExprKind::Index(base, idx) => chain(base) && trivial(idx),
        _ => chain(e),
    }
}

/// `place = value`, and `place op= value` with both operands primitive,
/// evaluate the value first, then the place. Any other `op=` is a call
/// on the place (`add_assign`), which goes first. `None` when the order
/// cannot be observed.
#[allow(clippy::option_option)] // not taken / lowered / reported
pub(crate) fn value_first(ctx: &mut LowerCtx<'_>, stmt: &Stmt) -> Option<Option<HirStmt>> {
    let (target, op, value) = match &stmt.kind {
        StmtKind::Assign(t, v) => (t, "=", v),
        StmtKind::CompoundAssign { target, op, value } if !op.starts_with('.') => {
            (target, op.as_str(), value)
        }
        _ => return None,
    };
    let placed = unplace(ctx, target);
    let place = placed.as_ref().unwrap_or(target);
    let bare = match &place.kind {
        ExprKind::Path(p) => p.segments.len() == 1,
        ExprKind::SelfLower => true,
        _ => false,
    };
    if bare || trivial(value) || plain(place) && inert(value) {
        return None;
    }
    // The place's type is the value's expected type.
    let mark = ctx.probe_mark();
    let ty = lower_expected(ctx, place, HirTy::Unknown).ty;
    let ty = crate::lower::infer::zonk(ctx, &ty);
    let readable = ctx.probe_clean(&mark) && ty != HirTy::Unknown;
    ctx.rollback(&mark);
    if op != "=" && !(readable && matches!(ty, HirTy::Prim(_))) {
        return None;
    }
    if !is_place(ctx, place, op, value) {
        return Some(None);
    }
    let mark = ctx.probe_mark();
    ctx.push_scope();
    let v = lower_expected(ctx, value, if readable { ty } else { HirTy::Unknown });
    let vty = crate::lower::infer::zonk(ctx, &v.ty);
    // A place with no read (a write-only `IndexSet`) types an open
    // literal itself: the value goes in place.
    let open = !readable
        && (!ctx.probe_clean(&mark) || crate::lower::infer::literal_default(ctx, &vty).is_some());
    if open || op != "=" && !matches!(vty, HirTy::Prim(_)) {
        ctx.pop_scope();
        ctx.rollback(&mark);
        return None;
    }
    let tmp = tmp_path(ctx, "av", value.span);
    let first = ctx.declare_var(&path_name(&tmp), v.ty.clone(), v, stmt.span);
    let kind = match &stmt.kind {
        StmtKind::Assign(t, _) => StmtKind::Assign(t.clone(), tmp),
        StmtKind::CompoundAssign { target, op, .. } => StmtKind::CompoundAssign {
            target: target.clone(),
            op: op.clone(),
            value: tmp,
        },
        _ => unreachable!("an assignment"),
    };
    let rest = lower_stmt(
        ctx,
        &Stmt {
            kind,
            span: stmt.span,
        },
    );
    ctx.pop_scope();
    let stmts = std::iter::once(first).chain(rest).collect();
    Some(Some(HirStmt::Expr(HirExpr {
        kind: HirExprKind::Block(Box::new(HirBlock {
            stmts,
            trailing: None,
            label: None,
        })),
        ty: HirTy::Unit,
        span: stmt.span,
    })))
}

/// The target of `=` / `op=` is a variable, `self`, or a field or element
/// chain on one; an element of a shared collection is a place whatever
/// holds the collection. A call result or other temporary is reported,
/// since the write would be lost with it.
pub(crate) fn is_place(ctx: &mut LowerCtx<'_>, target: &Expr, op: &str, value: &Expr) -> bool {
    let mut root = target;
    let what = loop {
        match &root.kind {
            ExprKind::Field(inner, _) | ExprKind::TupleField(inner, _) => root = inner,
            // An array is a value: its element is a place only if the array is one.
            ExprKind::Index(base, _) => {
                let mark = ctx.probe_mark();
                let ty = lower_expected(ctx, base, HirTy::Unknown).ty;
                ctx.rollback(&mark);
                if !matches!(ty, HirTy::Array(..)) {
                    return true;
                }
                root = base;
            }
            ExprKind::Path(_) | ExprKind::SelfLower => return true,
            // A tuple target is left to `lower_assign`'s own error; `a[1:3] = v`
            // stores through the slice there.
            ExprKind::Tuple(_) | ExprKind::Slice { .. } if op == "=" => return true,
            ExprKind::Call { .. } | ExprKind::MethodCall { .. } => break "a call result",
            ExprKind::Tuple(_) => break "a tuple",
            ExprKind::Literal(_) => break "a literal",
            _ => break "a computed value",
        }
    };
    // `g.get().total += 7`: `get` hands out a copy (a guard's, a cell's);
    // the receiver's `update` / `set` store the change.
    let getter = match &root.kind {
        ExprKind::MethodCall {
            receiver,
            method,
            args,
            trailing_closure: None,
            ..
        } if method == "get" && args.is_empty() => {
            let mark = ctx.probe_mark();
            let ty = lower_expected(ctx, receiver, HirTy::Unknown).ty;
            ctx.rollback(&mark);
            Some((&**receiver, ty))
        }
        _ => None,
    };
    let src = |e: &Expr| ctx.src(e.span).map_or_else(|| snippet(e), str::to_owned);
    let text = src(target);
    let msg = if op == "=" {
        format!("cannot assign to `{text}`: {what} is not a place")
    } else {
        format!("cannot use `{op}` on `{text}`: {what} is not a place, so the change would be lost")
    };
    let v = src(value);
    let held = matches!(
        root.kind,
        ExprKind::Path(_) | ExprKind::Call { .. } | ExprKind::MethodCall { .. }
    );
    let bound = |r: &str| {
        if std::ptr::eq(root, target) {
            "v".to_owned()
        } else {
            text.replacen(r, "v", 1)
        }
    };
    let store_back = getter.and_then(|(g, ty)| {
        let has = |m: &str| matches!(pick_method(ctx, &ty, m, None), Ok(Some(_)));
        let g = src(g);
        let b = bound(&src(root));
        if op == "=" && std::ptr::eq(root, target) && has("set") {
            Some(format!("store the new value: `{g}.set({v});`"))
        } else if has("update") {
            Some(format!(
                "`{g}.get()` is a copy; change the stored value with `update`: \
                 `{g}.update(|v| {{ {b} {op} {v}; v }});`"
            ))
        } else if has("set") {
            Some(format!(
                "`{g}.get()` is a copy; change it and store it back: \
                 `let v = {g}.get(); {b} {op} {v}; {g}.set(v);`"
            ))
        } else {
            None
        }
    });
    let help = store_back.unwrap_or_else(|| match entry_write_back(root) {
        Some((dict, key, init)) => match init {
            Some(init) if op != "=" => {
                format!("index with a default: `{dict}[{key}, default: {init}] {op} {v};`")
            }
            _ if op == "=" => format!("assign the element: `{dict}[{key}] = {v};`"),
            _ => format!("update the element: `{dict}[{key}] {op} {v};`"),
        },
        // `let v = f(); v = 3;` would be no fix.
        None if held && !(op == "=" && std::ptr::eq(root, target)) => {
            let r = src(root);
            let b = bound(&r);
            format!("bind the value to a variable and change that: `let v = {r}; {b} {op} {v};`")
        }
        None => "assign to a variable, a field (`s.x = v`) or an element (`xs[i] = v`)".to_owned(),
    });
    if !matches!(root.kind, ExprKind::Path(_)) {
        let _ = lower_expected(ctx, root, HirTy::Unknown);
    }
    let _ = lower_expected(ctx, value, HirTy::Unknown);
    ctx.emit_error_help(Code::NOT_A_PLACE, target.span, msg, help);
    false
}

/// `d.entry(k).or_insert(v)` (`init` is `v`) or `d.get(k)`: the collection,
/// key and default as written.
pub(crate) fn entry_write_back(e: &Expr) -> Option<(String, String, Option<String>)> {
    let ExprKind::MethodCall {
        receiver,
        method,
        args,
        ..
    } = &e.kind
    else {
        return None;
    };
    let one = |args: &[CallArg]| match args {
        [CallArg::Positional(a)] => Some(snippet(a)),
        _ => None,
    };
    match method.as_str() {
        "get" => Some((snippet(receiver), one(args)?, None)),
        "or_insert" => {
            let ExprKind::MethodCall {
                receiver: dict,
                method: entry,
                args: key,
                ..
            } = &receiver.kind
            else {
                return None;
            };
            (entry == "entry").then_some(())?;
            Some((snippet(dict), one(key)?, Some(one(args)?)))
        }
        _ => None,
    }
}

/// `name = value;` for local `local`.
pub(crate) fn assign_local(
    ctx: &mut LowerCtx<'_>,
    name: &str,
    local: LocalId,
    value: &Expr,
) -> HirStmt {
    let mark = ctx.probe_mark();
    let mut v = lower_expected(ctx, value, ctx.var_ty(local));
    let mut local = local;
    // A value that fails against the narrowed type alone (`if c { none }
    // else { x + 1 }`) also goes to a wider binding.
    let clean = ctx.probe_clean(&mark);
    if ctx.narrowed.contains_key(&local) && (!clean || !coercible(ctx, &ctx.var_ty(local), &v.ty)) {
        ctx.rollback(&mark);
        if clean || wider_fits(ctx, local, value) {
            (local, v) = unnarrow(ctx, name, local, value);
        } else {
            v = lower_expected(ctx, value, ctx.var_ty(local));
        }
    }
    let slot_ty = ctx.var_ty(local);
    let value = coerce(ctx, &slot_ty, v, &|| format!("assignment to `{name}`"));
    ctx.store_local(local, value)
}

/// Does `value` lower cleanly against a binding the narrowed `local`
/// came from?
pub(crate) fn wider_fits(ctx: &mut LowerCtx<'_>, local: LocalId, value: &Expr) -> bool {
    let mut cur = local;
    while let Some((orig, orig_ty)) = ctx.narrowed.get(&cur).cloned() {
        let mark = ctx.probe_mark();
        let mut probe = ctx.clone();
        let pv = lower_expected(&mut probe, value, orig_ty.clone());
        let fits = probe.probe_clean(&mark) && coercible(ctx, &orig_ty, &pv.ty);
        ctx.rollback(&mark);
        if fits {
            return true;
        }
        cur = orig;
    }
    false
}

/// Assigning a value the narrowed `local` can't hold ends the
/// narrowing for the rest of the scope; the value goes to the innermost
/// binding that takes it (else the declared one, which reports it).
/// The value is lowered first: it reads the narrowed binding.
pub(crate) fn unnarrow(
    ctx: &mut LowerCtx<'_>,
    name: &str,
    local: LocalId,
    value: &Expr,
) -> (LocalId, HirExpr) {
    let mut chain = vec![local];
    while let Some((orig, orig_ty)) = ctx.narrowed.get(chain.last().unwrap()).cloned() {
        chain.push(orig);
        let retry = ctx.probe_mark();
        let mut probe = ctx.clone();
        let pv = lower_expected(&mut probe, value, orig_ty.clone());
        let fits = probe.probe_clean(&retry) && coercible(ctx, &orig_ty, &pv.ty);
        ctx.rollback(&retry);
        if fits {
            break;
        }
    }
    let target = chain.pop().expect("narrowed local has an origin");
    let v = lower_expected(ctx, value, ctx.var_ty(target));
    for rebound in chain {
        if let Some(i) = ctx
            .scopes
            .iter()
            .rposition(|s| s.get(name) == Some(&rebound))
        {
            end_narrowing(ctx, i, name, rebound);
            ctx.unnarrowed.push((name.to_owned(), rebound, i));
        }
    }
    (target, v)
}

/// `name` in scope `i` no longer names the narrowed `local`: its origin
/// when that was declared in the same scope (a guard's narrowing).
pub(crate) fn end_narrowing(ctx: &mut LowerCtx<'_>, i: usize, name: &str, local: LocalId) {
    let origin = ctx.narrowed.get(&local).map(|(o, _)| *o);
    match origin {
        Some(o) if ctx.scopes[..i].iter().all(|s| s.get(name) != Some(&o)) => {
            ctx.scopes[i].insert(name.to_owned(), o);
        }
        _ => {
            ctx.scopes[i].remove(name);
        }
    }
}

/// Restore narrowings ended since `mark` for the next branch;
/// `rejoin_unnarrowings` ends them again at the join.
pub(crate) fn branch_unnarrowings(
    ctx: &mut LowerCtx<'_>,
    mark: usize,
) -> Vec<(String, LocalId, usize)> {
    let ended = ctx.unnarrowed.split_off(mark);
    for (name, local, i) in &ended {
        if let Some(scope) = ctx.scopes.get_mut(*i) {
            scope.insert(name.clone(), *local);
        }
    }
    ended
}

pub(crate) fn rejoin_unnarrowings(ctx: &mut LowerCtx<'_>, ended: Vec<(String, LocalId, usize)>) {
    for (name, local, i) in ended {
        if ctx
            .scopes
            .get(i)
            .is_some_and(|s| s.get(&name) == Some(&local))
        {
            end_narrowing(ctx, i, &name, local);
            ctx.unnarrowed.push((name, local, i));
        }
    }
}

/// `target = value;` for a local / static, a struct-field chain rooted
/// at a local, or an element of a local array.
#[allow(clippy::cognitive_complexity)] // one arm per assignable place form
pub(crate) fn lower_assign(
    ctx: &mut LowerCtx<'_>,
    target: &Expr,
    value: &Expr,
    span: nabic_diagnostic::Span,
) -> Option<HirStmt> {
    if let Some(place) = unplace(ctx, target) {
        return lower_assign(ctx, &place, value, span);
    }
    match &target.kind {
        // Bare-name assignment (local, else `static`).
        // `_ = e;` evaluates `e` and discards it.
        ExprKind::Path(p) if p.segments.len() == 1 && p.segments[0].name == "_" => {
            let v = lower_expected(ctx, value, HirTy::Unknown);
            Some(HirStmt::Expr(v))
        }
        ExprKind::Path(p) if p.segments.len() == 1 => {
            let name = &p.segments[0].name;
            if let Some(local) = ctx.lookup(name) {
                guard_sink(ctx, value);
                if ctx.value_captured.contains(&local) {
                    ctx.emit_error(
                        Code::INVALID_ASSIGNMENT,
                        format!(
                            "`{name}` is value-captured; to mutate the outer variable, add \
                             `[ref {name}]` to the closure's capture list"
                        ),
                    );
                }
                let a = assign_local(ctx, name, local, value);
                return Some(crate::lower::affine::reassign(ctx, a, span));
            }
            let segs = [name.as_str()];
            if let Some(full) = ctx.resolve_in(&segs, true) {
                let written = full.clone();
                let full = ctx.env.names.value_reexport(&full).cloned().unwrap_or(full);
                ctx.check_reached_visible(&written, &full, "static");
                item_marked(ctx, "static", &full, ctx.here());
                if !ctx.env.is_mut_static(&full) {
                    ctx.emit_error(
                        Code::INVALID_ASSIGNMENT,
                        format!(
                            "cannot assign to static `{name}`: statics are immutable; only an \
                             `extern` `static mut` can be written"
                        ),
                    );
                    let ty = ctx.ty_name(ctx.env.static_ty(&full).expect("a static"));
                    let atomic = matches!(
                        ty.as_str(),
                        "bool"
                            | "i8"
                            | "i16"
                            | "i32"
                            | "i64"
                            | "isize"
                            | "u8"
                            | "u16"
                            | "u32"
                            | "u64"
                            | "usize"
                    );
                    ctx.help_last(Some(if atomic {
                        format!(
                            "declare it `static {name}: Atomic<{ty}> = Atomic::new(..);` and \
                             write `{name}.store(..);`"
                        )
                    } else {
                        format!(
                            "declare it `static {name}: Mutex<{ty}> = Mutex::new(..);` and \
                             write `using g = {name}.lock() {{ g.set(..); }}`"
                        )
                    }));
                }
                extern_static_use(ctx, &full, true, span);
                let ty = ctx.env.static_ty(&full).expect("a static").clone();
                let v = lower_expected(ctx, value, ty.clone());
                let value = coerce(ctx, &ty, v, &|| format!("assignment to `{name}`"));
                return Some(HirStmt::StaticAssign { name: full, value });
            }
            if ctx.resolve_in(&segs, false).is_some() {
                ctx.emit_error(
                    Code::INVALID_ASSIGNMENT,
                    format!("cannot assign to const `{name}`"),
                );
                return None;
            }
            // Only `let` declares; declare it anyway so later uses don't report
            // again. A misspelt name is the likelier mistake.
            let near = nabic_diagnostic::suggest(
                name,
                ctx.scopes.iter().flat_map(|s| s.keys().map(String::as_str)),
            )
            .map(str::to_owned);
            ctx.emit_error(
                Code::INVALID_ASSIGNMENT,
                format!("`{name}` is not declared; a variable is created only by `let`"),
            );
            // `(a, b) = ..` reads each value from a parser temporary.
            let value_text = snippet(value);
            let in_tuple = value_text.starts_with("__destruct");
            // `x += 1` reads `x` too: that is the same mistake.
            let unknown = format!("cannot find value `{name}` in this scope");
            let compound = matches!(&value.kind, ExprKind::OpInfix { lhs, .. }
                if matches!(&lhs.kind, ExprKind::Path(p)
                    if matches!(p.segments.as_slice(), [s] if s.name == *name)));
            ctx.help_last(Some(match near {
                _ if compound => {
                    format!("declare `{name}` first: `let {name} = ..;` before this line")
                }
                _ if in_tuple => format!(
                    "declare `{name}` with `let` before this line, or declare every name at once \
                     with `let (..) = ..;`"
                ),
                Some(n) => format!(
                    "did you mean `{n}`? To declare a new variable, write `let {name} = {};`",
                    snippet(value)
                ),
                None => format!("write `let {name} = {};`", snippet(value)),
            }));
            let since = ctx.env.sink.diagnostics.borrow().len();
            let v = lower_expected(ctx, value, HirTy::Unknown);
            if compound {
                let mut diags = ctx.env.sink.diagnostics.borrow_mut();
                let tail = since.min(diags.len());
                if let Some(at) = diags[tail..].iter().position(|d| d.message == unknown) {
                    diags.remove(tail + at);
                }
            }
            Some(ctx.declare_var(name, v.ty.clone(), v, span))
        }
        // `ref self` names the caller's slot; `self = v` writes it (from a
        // `[ref self]` closure too, through the shared cell).
        ExprKind::SelfLower
            if ctx.lookup("self").is_some_and(|l| {
                ctx.is_boxed(l) || ctx.ref_params.iter().any(|(r, _)| *r == l)
            }) =>
        {
            let local = ctx.lookup("self")?;
            let slot_ty = ctx.var_ty(local);
            let v = lower_expected(ctx, value, slot_ty.clone());
            let value = coerce(ctx, &slot_ty, v, &|| "assignment to `self`".to_owned());
            Some(ctx.store_local(local, value))
        }
        // `arr[idx] = value`: element store on a local array.
        ExprKind::Index(arr_expr, idx_expr) => {
            let arr_h = lower_expected(ctx, arr_expr, HirTy::Unknown);
            // A struct / enum stores through `IndexSet::index_set`.
            if let HirTy::GcBuf(elem) = arr_h.ty.clone() {
                let set = buf_access(ctx, arr_h, *elem, idx_expr, Some(value), target.span);
                return Some(HirStmt::Expr(set));
            }
            if axis_indexed(ctx, &arr_h.ty) {
                let idx = axis_parts(ctx, idx_expr);
                let rest = [CallArg::Positional(value.clone())];
                let span = target.span;
                let (since, ty) = (ctx.probe_mark().diagnostics, arr_h.ty.clone());
                let set = index_call(ctx, arr_h, "index_set", &idx, &rest, span);
                no_axis_form(ctx, since, &ty, idx_expr, span);
                return Some(HirStmt::Expr(set));
            }
            if one_axis_misuse(ctx, &arr_h.ty, idx_expr, target.span) {
                return None;
            }
            if matches!(arr_h.ty, HirTy::Struct(_) | HirTy::Enum(_)) {
                let rest = [CallArg::Positional(value.clone())];
                let span = target.span;
                return Some(HirStmt::Expr(index_call(
                    ctx,
                    arr_h,
                    "index_set",
                    idx_expr,
                    &rest,
                    span,
                )));
            }
            let idx_h = match arr_h.ty {
                HirTy::Array(_, n) => array_position(ctx, idx_expr, n),
                _ => lower_expected(ctx, idx_expr, HirTy::Unknown),
            };
            let elem = match (&arr_h.kind, &arr_h.ty) {
                (HirExprKind::Local(l), HirTy::Array(elem, _)) => Some((*l, (**elem).clone())),
                (_, HirTy::Unknown) => return None,
                _ => None,
            };
            // `s.arr[i] = v` on a field chain of a local: in place.
            if let (None, HirTy::Array(elem_ty, _)) = (&elem, &arr_h.ty) {
                if let Some((array_local, path)) = local_field_path(ctx, &arr_h) {
                    let elem_ty = (**elem_ty).clone();
                    let v = lower_expected(ctx, value, elem_ty.clone());
                    let value = coerce(ctx, &elem_ty, v, &|| "the array element".to_owned());
                    return Some(HirStmt::ArrayAssign {
                        array_local,
                        path,
                        index: idx_h,
                        value,
                        elem_ty,
                    });
                }
            }
            // `xs[j][i] = v` on a value array that isn't a local: copy the array,
            // store into the copy, write it back.
            if elem.is_none()
                && matches!(arr_h.ty, HirTy::Array(..))
                && matches!(arr_expr.kind, ExprKind::Field(..) | ExprKind::Index(..))
            {
                return Some(rewrite_index_store(
                    ctx,
                    idx_expr,
                    |copy, i| {
                        let store = Expr {
                            kind: ExprKind::Index(Box::new(copy.clone()), Box::new(i.clone())),
                            span: target.span,
                        };
                        vec![
                            let_stmt(copy, (**arr_expr).clone()),
                            assign_stmt(store, value.clone()),
                            assign_stmt((**arr_expr).clone(), copy.clone()),
                        ]
                    },
                    target.span,
                ));
            }
            let Some((array_local, elem_ty)) = elem else {
                ctx.emit_error(
                    Code::INVALID_ASSIGNMENT,
                    format!(
                        "indexed assignment needs a local array variable as target \
                         (found `{}`)",
                        ctx.ty_name(&arr_h.ty)
                    ),
                );
                return None;
            };
            let v = lower_expected(ctx, value, elem_ty.clone());
            let value = coerce(ctx, &elem_ty, v, &|| "the array element".to_owned());
            Some(HirStmt::ArrayAssign {
                array_local,
                path: Vec::new(),
                index: idx_h,
                value,
                elem_ty,
            })
        }
        // `a.f1.f2 = value` (or `s.0`): peel to the root local and thread
        // `WithField` updates back out.
        ExprKind::Field(_, _) | ExprKind::TupleField(_, _) => {
            // `struct_id` is `None` for a plain tuple's element (`t.0 = x`).
            struct Step {
                ty: HirTy,
                struct_id: Option<StructId>,
                field_index: u32,
            }
            let mut chain: Vec<String> = Vec::new();
            let mut cur: &Expr = target;
            loop {
                match &cur.kind {
                    ExprKind::Field(inner, name) => {
                        chain.push(name.clone());
                        cur = inner;
                    }
                    ExprKind::TupleField(inner, i) => {
                        chain.push(i.to_string());
                        cur = inner;
                    }
                    _ => break,
                }
            }
            chain.reverse();
            // `xs[i].f = v`: the element is a value, so read it, update the copy and
            // store it back (`xs[i] = copy`).
            if let ExprKind::Index(arr, idx) = &cur.kind {
                let field_target = |elem: &Expr| {
                    chain.iter().fold(elem.clone(), |acc, name| Expr {
                        kind: ExprKind::Field(Box::new(acc), name.clone()),
                        span: target.span,
                    })
                };
                return Some(rewrite_index_store(
                    ctx,
                    idx,
                    |elem, i| {
                        let at = Expr {
                            kind: ExprKind::Index(arr.clone(), Box::new(i.clone())),
                            span: cur.span,
                        };
                        vec![
                            let_stmt(elem, at.clone()),
                            assign_stmt(field_target(elem), value.clone()),
                            assign_stmt(at, elem.clone()),
                        ]
                    },
                    target.span,
                ));
            }
            let root_h = lower_expected(ctx, cur, HirTy::Unknown);
            if root_h.ty == HirTy::Unknown {
                return None;
            }
            if crate::lower::mono::place_root(&root_h).is_none() {
                ctx.emit_error(
                    Code::INVALID_ASSIGNMENT,
                    "field assignment needs a local variable at the root of the target \
                     (`v.f = x`)"
                        .to_owned(),
                );
                return None;
            }
            if let (HirExprKind::Local(l), ExprKind::Path(p)) = (&root_h.kind, &cur.kind) {
                if ctx.value_captured.contains(l) {
                    let name = p.segments.last().map_or("", |s| s.name.as_str());
                    ctx.emit_error_help(
                        Code::INVALID_ASSIGNMENT,
                        target.span,
                        format!(
                            "cannot assign to a field of `{name}`: it is value-captured, so the \
                             write would change the closure's copy, not the outer `{name}`"
                        ),
                        format!("add `[ref {name}]` to the closure's capture list"),
                    );
                }
            }
            let root_place = root_h.clone();
            let mut steps: Vec<Step> = Vec::new();
            let mut step_ty = root_h.ty.clone();
            // Writing a union field (safe) rebuilds the union from that field; it
            // must be the last step of the target.
            let mut union_value: Option<HirExpr> = None;
            for (ci, name) in chain.iter().enumerate() {
                if let HirTy::Tuple(parts) = &step_ty {
                    if let Some(i) = name.parse::<usize>().ok().filter(|i| *i < parts.len()) {
                        let next = parts[i].clone();
                        steps.push(Step {
                            ty: step_ty.clone(),
                            struct_id: None,
                            field_index: u32::try_from(i).expect("tuple field overflow"),
                        });
                        step_ty = next;
                        continue;
                    }
                }
                let HirTy::Struct(sid) = step_ty.clone() else {
                    ctx.emit_error(
                        Code::NO_FIELD,
                        format!("no field `{name}` on type `{}`", ctx.ty_name(&step_ty)),
                    );
                    return None;
                };
                let def = ctx.struct_def(sid);
                if let Some(uk) = &def.union_kind {
                    let Some(fty) = uk
                        .aliases
                        .iter()
                        .find(|(n, _)| n == name)
                        .map(|(_, t)| t.clone())
                    else {
                        ctx.no_field(sid, name, span);
                        return None;
                    };
                    if ci + 1 != chain.len() {
                        ctx.emit_error(
                            Code::INVALID_ASSIGNMENT,
                            format!(
                                "cannot assign through union field `{name}`: read it (in \
                                 `unsafe`), change the copy, and write the field back"
                            ),
                        );
                        return None;
                    }
                    ctx.check_field_visible(sid, name);
                    let v = lower_expected(ctx, value, fty.clone());
                    let v = coerce(ctx, &fty, v, &|| format!("union field `{name}`"));
                    let storage_ty = def.fields[0].ty.clone();
                    union_value = Some(HirExpr {
                        kind: HirExprKind::StructInit {
                            struct_id: sid,
                            fields: vec![HirExpr {
                                kind: HirExprKind::UnionWrite {
                                    struct_id: sid,
                                    from_ty: v.ty.clone(),
                                    storage_ty: storage_ty.clone(),
                                    value: Box::new(v),
                                },
                                ty: storage_ty,
                                span,
                            }],
                        },
                        ty: HirTy::Struct(sid),
                        span,
                    });
                    break;
                }
                let Some(idx) = def.fields.iter().position(|f| f.name == *name) else {
                    ctx.no_field(sid, name, span);
                    return None;
                };
                ctx.check_field_visible(sid, name);
                steps.push(Step {
                    ty: step_ty.clone(),
                    struct_id: Some(sid),
                    field_index: u32::try_from(idx).expect("field index overflow"),
                });
                step_ty = def.fields[idx].ty.clone();
            }
            let last_name = chain.last().cloned().unwrap_or_default();
            let mut acc = match union_value {
                // `u.f = x` with `u` a local: replace the whole union.
                Some(u) if steps.is_empty() => {
                    return crate::lower::mono::assign_place(ctx, &root_place, u);
                }
                Some(u) => u,
                None => {
                    let v = lower_expected(ctx, value, step_ty.clone());
                    coerce(ctx, &step_ty, v, &|| format!("field `{last_name}`"))
                }
            };
            // Materialize parent values via field reads so each `WithField` has a
            // concrete target.
            let mut targets: Vec<HirExpr> = Vec::new();
            let read = |ctx: &LowerCtx<'_>, from: &HirExpr, step: &Step| {
                let (kind, ty) = match (step.struct_id, &step.ty) {
                    (Some(sid), _) => {
                        let ty = ctx.struct_def(sid).fields[step.field_index as usize]
                            .ty
                            .clone();
                        let kind = HirExprKind::FieldRead {
                            target: Box::new(from.clone()),
                            struct_id: sid,
                            field_index: step.field_index,
                            field_ty: ty.clone(),
                        };
                        (kind, ty)
                    }
                    (None, HirTy::Tuple(parts)) => {
                        let ty = parts[step.field_index as usize].clone();
                        let kind = HirExprKind::TupleField {
                            target: Box::new(from.clone()),
                            index: step.field_index,
                            field_ty: ty.clone(),
                        };
                        (kind, ty)
                    }
                    (None, _) => unreachable!("a tuple step is on a tuple"),
                };
                HirExpr {
                    kind,
                    ty,
                    span: target.span,
                }
            };
            let mut walk = root_h;
            for step in &steps[..steps.len() - 1] {
                let next = read(ctx, &walk, step);
                targets.push(walk);
                walk = next;
            }
            targets.push(walk);
            for (i, step) in steps.iter().enumerate().rev() {
                let kind = match (step.struct_id, &step.ty) {
                    (Some(sid), _) => HirExprKind::WithField {
                        target: Box::new(targets[i].clone()),
                        struct_id: sid,
                        field_index: step.field_index,
                        value: Box::new(acc),
                    },
                    // A tuple is rebuilt with the one element replaced.
                    (None, HirTy::Tuple(parts)) => {
                        let mut acc = Some(acc);
                        let fields = (0..parts.len())
                            .map(|j| {
                                let j = u32::try_from(j).expect("tuple field overflow");
                                if j == step.field_index {
                                    acc.take().expect("replaced once")
                                } else {
                                    read(
                                        ctx,
                                        &targets[i],
                                        &Step {
                                            ty: step.ty.clone(),
                                            struct_id: None,
                                            field_index: j,
                                        },
                                    )
                                }
                            })
                            .collect();
                        HirExprKind::Tuple { fields }
                    }
                    (None, _) => unreachable!("a tuple step is on a tuple"),
                };
                acc = HirExpr {
                    ty: step.ty.clone(),
                    span: target.span,
                    kind,
                };
            }
            crate::lower::mono::assign_place(ctx, &root_place, acc)
        }
        // `a[1:3] = v`: through `IndexSet<SliceArg, _>` on an axis-indexed
        // type, a same-length copy into a list.
        ExprKind::Slice {
            target: arr,
            start,
            end,
            step,
        } => {
            let span = target.span;
            let arr_h = lower_expected(ctx, arr, HirTy::Unknown);
            if axis_indexed(ctx, &arr_h.ty) {
                let sel = slice_arg(ctx, start.as_deref(), end.as_deref(), step.as_deref(), span);
                let rest = [CallArg::Positional(value.clone())];
                return Some(HirStmt::Expr(index_call(
                    ctx,
                    arr_h,
                    "index_set",
                    &sel,
                    &rest,
                    span,
                )));
            }
            if !is_list(ctx, &arr_h.ty) {
                if arr_h.ty != HirTy::Unknown {
                    ctx.emit_error_at(
                        Code::INVALID_ASSIGNMENT,
                        span,
                        format!("cannot assign to a slice of `{}`", ctx.ty_name(&arr_h.ty)),
                    );
                    ctx.help_last(Some(
                        "assign the elements one at a time (`xs[i] = v`)".to_owned(),
                    ));
                }
                return None;
            }
            if step.is_some() {
                ctx.emit_error_at(
                    Code::INVALID_ASSIGNMENT,
                    span,
                    "a stepped slice of a list cannot be assigned".to_owned(),
                );
                ctx.help_last(Some(
                    "drop the step, or store the elements one at a time; an `Array` takes \
                     `a[::2] = v`"
                        .to_owned(),
                ));
                return None;
            }
            // `{ let #t = xs; slice_mut(#t, a..b).copy_from_slice(v[:]) }`.
            let t = tmp_path(ctx, "t", span);
            let recv = splice(ctx, arr_h);
            ctx.want_view = true;
            let view = Expr {
                kind: ExprKind::Slice {
                    target: Box::new(t.clone()),
                    start: start.clone(),
                    end: end.clone(),
                    step: None,
                },
                span,
            };
            let src = Expr {
                kind: ExprKind::Slice {
                    target: Box::new(value.clone()),
                    start: None,
                    end: None,
                    step: None,
                },
                span: value.span,
            };
            let copy = Expr {
                kind: ExprKind::MethodCall {
                    receiver: Box::new(view),
                    method: "copy_from_slice".to_owned(),
                    type_args: Vec::new(),
                    args: vec![CallArg::Positional(src)],
                    trailing_closure: None,
                    origin: None,
                    written: false,
                },
                span,
            };
            let e = lower_expected(
                ctx,
                &block_of(vec![let_stmt(&t, recv)], copy, span),
                HirTy::Unit,
            );
            Some(HirStmt::Expr(e))
        }
        _ => {
            let help =
                "assign to a variable, a field (`s.x = v`) or an element (`xs[i] = v`)".to_owned();
            ctx.emit_error_at(
                Code::INVALID_ASSIGNMENT,
                target.span,
                "invalid assignment target".to_owned(),
            );
            ctx.help_last(Some(help));
            None
        }
    }
}

pub(crate) fn tmp_path(ctx: &LowerCtx<'_>, what: &str, span: nabic_diagnostic::Span) -> Expr {
    let n = ctx.temps.get();
    ctx.temps.set(n + 1);
    Expr {
        kind: ExprKind::Path(nabic_ast::Path {
            segments: vec![nabic_ast::PathSegment {
                name: format!("#{what}{n}"),
                generics: Vec::new(),
            }],
            span,
            origin: None,
        }),
        span,
    }
}

pub(crate) fn path_name(e: &Expr) -> String {
    let ExprKind::Path(p) = &e.kind else {
        unreachable!("temp is a path")
    };
    p.segments[0].name.clone()
}

pub(crate) fn let_stmt(name: &Expr, init: Expr) -> Stmt {
    Stmt {
        span: init.span,
        kind: StmtKind::Let {
            name: path_name(name),
            ty: None,
            init,
        },
    }
}

pub(crate) const fn assign_stmt(target: Expr, value: Expr) -> Stmt {
    Stmt {
        span: target.span,
        kind: StmtKind::Assign(target, value),
    }
}

/// `l.f.g` (struct fields, tuple elements) of a plain local `l`: `l` and
/// the field indices outward.
pub(crate) fn local_field_path(ctx: &LowerCtx<'_>, e: &HirExpr) -> Option<(LocalId, Vec<u32>)> {
    let mut path = Vec::new();
    let mut cur = e;
    loop {
        match &cur.kind {
            HirExprKind::FieldRead {
                target,
                field_index: i,
                ..
            }
            | HirExprKind::TupleField {
                target, index: i, ..
            } => {
                path.push(*i);
                cur = target;
            }
            HirExprKind::Local(l) if !path.is_empty() && !ctx.is_boxed(*l) => {
                path.reverse();
                return Some((*l, path));
            }
            _ => return None,
        }
    }
}

pub(crate) fn rewrite_index_store(
    ctx: &mut LowerCtx<'_>,
    idx: &Expr,
    body: impl FnOnce(&Expr, &Expr) -> Vec<Stmt>,
    span: nabic_diagnostic::Span,
) -> HirStmt {
    let tmp = tmp_path(ctx, "tmp", span);
    // A literal / name index is read as is (and keeps its expected `usize`
    // type); anything else goes through a temp.
    let mut stmts = Vec::new();
    let i = if matches!(idx.kind, ExprKind::Literal(_) | ExprKind::Path(_)) {
        idx.clone()
    } else {
        let i = tmp_path(ctx, "idx", span);
        stmts.push(let_stmt(&i, idx.clone()));
        i
    };
    stmts.extend(body(&tmp, &i));
    let block = Expr {
        kind: ExprKind::Block(Block {
            stmts,
            trailing: None,
            span,
            label: None,
        }),
        span,
    };
    HirStmt::Expr(lower_expr(ctx, &block))
}

/// `d.<method>(k, v)` for `d[k, default: v]`.
pub(crate) fn index_default_call(d: &Expr, idx: &Expr, method: &str) -> Expr {
    let (k, v) = nabic_ast::as_index_default(idx).expect("a default index");
    Expr {
        span: idx.span,
        kind: ExprKind::MethodCall {
            receiver: Box::new(d.clone()),
            method: method.to_owned(),
            type_args: Vec::new(),
            args: vec![
                CallArg::Positional(k.clone()),
                CallArg::Positional(v.clone()),
            ],
            trailing_closure: None,
            origin: None,
            written: false,
        },
    }
}

/// The `d[k, default: v]` an assignment target starts from (`t` itself, or
/// the base of its fields / indexes).
pub(crate) fn default_index_of(t: &Expr) -> Option<&Expr> {
    match &t.kind {
        ExprKind::Index(_, idx) if nabic_ast::as_index_default(idx).is_some() => Some(t),
        ExprKind::Index(b, _) | ExprKind::Field(b, _) | ExprKind::TupleField(b, _) => {
            default_index_of(b)
        }
        _ => None,
    }
}

/// `t` with its `d[k, default: v]` replaced by `d2[k2]`.
pub(crate) fn replace_default_index(t: &Expr, d2: &Expr, k2: &Expr) -> Expr {
    let kind = match &t.kind {
        ExprKind::Index(_, idx) if nabic_ast::as_index_default(idx).is_some() => {
            ExprKind::Index(Box::new(d2.clone()), Box::new(k2.clone()))
        }
        ExprKind::Index(b, i) => {
            ExprKind::Index(Box::new(replace_default_index(b, d2, k2)), i.clone())
        }
        ExprKind::Field(b, f) => {
            ExprKind::Field(Box::new(replace_default_index(b, d2, k2)), f.clone())
        }
        ExprKind::TupleField(b, n) => {
            ExprKind::TupleField(Box::new(replace_default_index(b, d2, k2)), *n)
        }
        k => k.clone(),
    };
    Expr { kind, span: t.span }
}

/// An assignment whose target starts from `d[k, default: v]`: `d` and
/// `k` once into temps, `v` stored when `k` is absent, then the
/// assignment through `d[k]`.
pub(crate) fn index_default_store(ctx: &mut LowerCtx<'_>, stmt: &Stmt, target: &Expr) -> HirStmt {
    let span = stmt.span;
    let at = default_index_of(target).expect("a default index");
    let ExprKind::Index(d, idx) = &at.kind else {
        unreachable!("a default index")
    };
    let (k, v) = nabic_ast::as_index_default(idx).expect("a default index");
    let d2 = tmp_path(ctx, "dict", span);
    let k2 = tmp_path(ctx, "key", span);
    let insert = Expr {
        span,
        kind: ExprKind::MethodCall {
            receiver: Box::new(d2.clone()),
            method: "get_or_insert".to_owned(),
            type_args: Vec::new(),
            args: vec![
                CallArg::Positional(k2.clone()),
                CallArg::Positional(v.clone()),
            ],
            trailing_closure: None,
            origin: None,
            written: false,
        },
    };
    let new_target = replace_default_index(target, &d2, &k2);
    let kind = match &stmt.kind {
        StmtKind::Assign(_, value) => StmtKind::Assign(new_target, value.clone()),
        StmtKind::CompoundAssign { op, value, .. } => StmtKind::CompoundAssign {
            target: new_target,
            op: op.clone(),
            value: value.clone(),
        },
        _ => unreachable!("an assignment"),
    };
    let stmts = vec![
        let_stmt(&d2, (**d).clone()),
        let_stmt(&k2, k.clone()),
        Stmt {
            span,
            kind: StmtKind::Expr(insert),
        },
        Stmt { span, kind },
    ];
    let block = Expr {
        kind: ExprKind::Block(Block {
            stmts,
            trailing: None,
            span,
            label: None,
        }),
        span,
    };
    HirStmt::Expr(lower_expr(ctx, &block))
}

/// Write-through for an assignment to a narrowed binding: also store
/// the new value, converted back, into the original slot.
pub(crate) fn write_through(
    ctx: &mut LowerCtx<'_>,
    stmt: Option<HirStmt>,
    span: nabic_diagnostic::Span,
) -> Option<HirStmt> {
    let local = match &stmt {
        Some(HirStmt::Assign { local, .. }) => *local,
        Some(HirStmt::ArrayAssign { array_local, .. }) => *array_local,
        _ => return stmt,
    };
    if !ctx.narrowed.contains_key(&local) {
        return stmt;
    }
    // A narrowing of a narrowing writes back through every level.
    let mut stmts = vec![stmt?];
    let mut cur = local;
    while let Some((orig, orig_ty)) = ctx.narrowed.get(&cur).cloned() {
        let ty = ctx.local_ty(cur);
        let back = coerce(ctx, &orig_ty, hir_local(cur, &ty, span), &|| {
            "the narrowed slot".to_owned()
        });
        stmts.push(ctx.store_local(orig, back));
        cur = orig;
    }
    Some(HirStmt::Expr(HirExpr {
        kind: HirExprKind::Block(Box::new(HirBlock {
            stmts,
            trailing: None,
            label: None,
        })),
        ty: HirTy::Unit,
        span,
    }))
}
