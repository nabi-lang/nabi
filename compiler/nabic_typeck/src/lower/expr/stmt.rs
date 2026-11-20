//! Statements, `let` bindings and guard checks.

#[allow(clippy::wildcard_imports)]
use crate::lower::expr::*;

pub(crate) fn lower_block(ctx: &mut LowerCtx<'_>, block: &Block) -> HirBlock {
    ctx.push_scope();
    declare_nested_fns(ctx, block);
    let mut stmts = Vec::with_capacity(block.stmts.len());
    let owed = ctx.err_frames.len();
    let dead = ctx.dead;
    for (i, s) in block.stmts.iter().enumerate() {
        let from = stmts.len();
        match &s.kind {
            StmtKind::LetStruct {
                path,
                fields,
                rest,
                ty,
                init,
            } => ctx.at_span(s.span, |ctx| {
                let ty = lower_let_struct(ctx, path, fields, ty.as_ref(), init, s.span, &mut stmts);
                if let (false, Some(ty)) = (*rest, ty) {
                    let listed: Vec<&str> = fields.iter().map(|(f, _)| f.as_str()).collect();
                    crate::lower::pattern::missing_rest(ctx, s.span, path, &listed, &ty);
                }
            }),
            StmtKind::LetPattern { pat, ty, init } => ctx.at_span(s.span, |ctx| {
                guard_sink(ctx, init);
                let value = match ty.as_ref().map(|t| ctx.resolve_ast_ty(t)) {
                    Some(t) => {
                        let v = lower_expected(ctx, init, t.clone());
                        coerce(ctx, &t, v, &|| "the destructured value".to_owned())
                    }
                    None => lower_expected(ctx, init, HirTy::Unknown),
                };
                // The temp's `using` drops the whole value (drop glue), so
                // its guard parts need no `using` of their own.
                let tracked = ctx.guard_lets.len();
                ctx.at_span(pat.span, |ctx| {
                    crate::lower::desugar::bind_irrefutable(ctx, pat, value, &mut stmts);
                });
                if matches!(&init.kind, ExprKind::Path(p) if p.segments[0].name.starts_with("#using"))
                {
                    ctx.guard_lets.truncate(tracked);
                }
            }),
            StmtKind::LetTuple {
                names,
                rest: tuple_rest,
                ty: None,
                init,
            } => {
                let rest = &block.stmts[i + 1..];
                ctx.at_span(s.span, |ctx| {
                    crate::lower::infer::lower_let_tuple(
                        ctx,
                        names,
                        tuple_rest.as_ref(),
                        init,
                        s.span,
                        rest,
                        block,
                        &mut stmts,
                    );
                });
            }
            StmtKind::LetTuple {
                names,
                rest,
                ty,
                init,
            } => ctx.at_span(s.span, |ctx| {
                let ty = ty.as_ref().map(|t| ctx.resolve_ast_ty(t));
                lower_let_tuple(ctx, names, rest.as_ref(), ty, init, s.span, &mut stmts);
            }),
            StmtKind::Let {
                name,
                ty: None,
                init,
            } => {
                let rest = &block.stmts[i + 1..];
                stmts.extend(crate::lower::infer::lower_let(ctx, s, name, init, rest, block));
            }
            _ => {
                if let Some(stmt) = lower_stmt(ctx, s) {
                    let guard = guard_narrowings(ctx, s, &stmt);
                    // `if N == 0 { return ..; }` at `N = 0`: the rest never runs.
                    if ctx.dead == dead && always_leaves(ctx, &stmt) {
                        ctx.dead += 1;
                    }
                    stmts.push(stmt);
                    // `if x == none { return; }`: `x` is not `none` after it.
                    if !guard.is_empty() {
                        let lets = bind_narrowings(ctx, guard, s.span);
                        stmts.extend(lets);
                    }
                }
            }
        }
        crate::lower::affine::owe_lets(ctx, &mut stmts, from);
    }
    // The tail inherits the block's expected type; in a labelled block it
    // is a `break` to it and must agree with the others.
    let label_target = (ctx.breakables.len().checked_sub(1))
        .filter(|&i| block.label.is_some() && ctx.breakables[i].0 == block.label);
    let mut trailing = block.trailing.as_ref().map(|e| match label_target {
        Some(idx) => lower_break_value(ctx, idx, e),
        None => lower_expr(ctx, e),
    });
    crate::lower::affine::settle(ctx, owed, &mut stmts, &mut trailing, block.span);
    ctx.dead = dead;
    ctx.pop_scope();
    HirBlock {
        stmts,
        trailing,
        label: block.label.clone(),
    }
}

/// `if c { .. }` whose constant condition holds and whose body diverges.
fn always_leaves(ctx: &LowerCtx<'_>, s: &HirStmt) -> bool {
    let HirStmt::Expr(HirExpr {
        kind:
            HirExprKind::If {
                cond,
                then_block,
                else_branch: None,
            },
        ..
    }) = s
    else {
        return false;
    };
    const_cond(ctx, cond) == Some(true) && hir_block_diverges(then_block)
}

/// The nested fns of `block`, visible in the whole block (hoisted): the
/// ones declared here over those of enclosing blocks.
pub(crate) fn declare_nested_fns(ctx: &mut LowerCtx<'_>, block: &Block) {
    // Name -> the statement and the fn of its first definition here.
    let mut own: HashMap<&str, (nabic_diagnostic::Span, nabic_diagnostic::Span)> =
        HashMap::default();
    for inherited in [true, false] {
        if !inherited {
            // A `use` of the block hides the nested fns of the blocks around it (an
            // empty key, `resolve_any_fn`).
            for s in &block.stmts {
                let StmtKind::Item(Item::Use(u)) = &s.kind else {
                    continue;
                };
                if u.attrs
                    .iter()
                    .any(|a| a.path == [nabic_ast::INHERITED_USE_ATTR])
                {
                    continue;
                }
                let mut names = Vec::new();
                nabic_ast::rename::use_names(&u.tree, &mut names);
                if let Some(m) = ctx.nested_fns.last_mut() {
                    m.extend(names.into_iter().map(|n| (n.to_owned(), String::new())));
                }
            }
        }
        for s in &block.stmts {
            let StmtKind::Item(Item::Fn(f)) = &s.kind else {
                continue;
            };
            let Some((lifted, from_outer)) = nabic_ast::nested_marker_of(f) else {
                continue;
            };
            if from_outer != inherited {
                continue;
            }
            if let Some(&(first, first_fn)) = own.get(f.name.as_str()).filter(|_| !inherited) {
                ctx.emit_diag(nabic_diagnostic::duplicate(
                    Code::DUPLICATE_DEFINITION,
                    format!(
                        "nested function `{}` is defined twice in this block",
                        f.name
                    ),
                    &f.name,
                    s.span,
                    Some(first),
                    true,
                ));
                // Calls keep to the first, reporting no argument errors.
                ctx.env.duplicated.borrow_mut().insert(first_fn);
                continue;
            }
            if !inherited {
                own.insert(&f.name, (s.span, f.span));
            }
            let full = if ctx.current_module.is_empty() {
                lifted.to_owned()
            } else {
                format!("{}::{lifted}", ctx.current_module)
            };
            if let Some(m) = ctx.nested_fns.last_mut() {
                m.insert(f.name.clone(), full);
            }
        }
    }
    if own.is_empty() {
        return;
    }
    for s in &block.stmts {
        let names: Vec<&str> = match &s.kind {
            StmtKind::Let { name, .. } => vec![name.as_str()],
            StmtKind::LetTuple { names, .. } => names.iter().map(String::as_str).collect(),
            _ => continue,
        };
        for n in names {
            if own.contains_key(n) {
                ctx.emit_error_at(
                    Code::REDECLARED_LOCAL,
                    s.span,
                    format!("`{n}` names both a variable and a nested function in this block"),
                );
                ctx.help_last(Some(format!(
                    "a nested function is seen in its whole block, so rename one of them: \
                     `let {n}2 = ..` or `fn {n}2(..)`"
                )));
            }
        }
    }
}

/// What a block item reads from around it: one-segment value and type
/// paths it does not bind, and the head of a longer path (`T::N`).
#[derive(Default)]
pub(crate) struct Free {
    scopes: Vec<Vec<String>>,
    pub(crate) values: Vec<(String, nabic_diagnostic::Span)>,
    pub(crate) types: Vec<(String, nabic_diagnostic::Span)>,
}
impl AstVisitor for Free {
    fn expr(&mut self, e: &Expr) {
        if let ExprKind::Path(p) = &e.kind {
            match p.segments.as_slice() {
                [seg] if !self.scopes.iter().flatten().any(|n| *n == seg.name) => {
                    self.values.push((seg.name.clone(), e.span));
                }
                // `T::N`: reads the type `T`.
                [head, _, ..] => self.types.push((head.name.clone(), e.span)),
                _ => {}
            }
        }
    }
    fn ty(&mut self, t: &Ty) {
        if let nabic_ast::TyKind::Path(p) = &t.kind {
            if let [seg] = p.segments.as_slice() {
                self.types.push((seg.name.clone(), t.span));
            }
        }
    }
    fn bind(&mut self, name: &str) {
        if let Some(s) = self.scopes.last_mut() {
            s.push(name.to_owned());
        }
    }
    fn push_scope(&mut self) {
        self.scopes.push(Vec::new());
    }
    fn pop_scope(&mut self) {
        self.scopes.pop();
    }
}

/// The names `walk` reads from around it.
pub(crate) fn free_in(walk: impl FnOnce(&mut Free)) -> Free {
    let mut free = Free::default();
    free.push_scope();
    walk(&mut free);
    free
}

/// A block-local item at its statement: a `const` that reads the generic
/// parameters around it gets this instance's value; any other is checked.
fn local_item(ctx: &mut LowerCtx<'_>, item: &Item) {
    match item {
        Item::Const(c) if crate::lower::consteval::per_instance(ctx, c) => {
            crate::lower::consteval::instance_const(ctx, c);
        }
        _ => check_local_item(ctx, item),
    }
}

/// A block-local item sees no local or generic parameter of the code
/// around it.
pub(crate) fn check_local_item(ctx: &LowerCtx<'_>, item: &Item) {
    let free = free_in(|f| crate::walk::walk_item(f, item));
    let outer = nabic_core::local::qualified(item.local_outer().unwrap_or_default());
    let (what, own) = match item.name() {
        Some(n) => (format!("`{}`", nabic_core::local::leaf(n)), true),
        None => ("this `impl`".to_owned(), false),
    };
    let mut seen = nabic_core::HashSet::default();
    for (name, span) in &free.values {
        if ctx.lookup(name).is_none() || !seen.insert(name) {
            continue;
        }
        let fix = match item {
            Item::Static(_) => "a `static` is made once, so its value can use only items",
            Item::Const(_) => "a `const` is computed at compile time, so it can use only items",
            Item::Struct(_) | Item::Enum(_) | Item::Union(_) if own => {
                "pass it as a field or a parameter instead"
            }
            _ => "pass it as a parameter instead",
        };
        ctx.emit_error_at(
            Code::OUTER_LOCAL,
            *span,
            format!(
                "{what} is declared inside `{outer}` and can't use `{name}`, a local of \
                 `{outer}`; {fix}"
            ),
        );
        if let Item::Const(c) = item {
            ctx.help_last(Some(const_reads_help(ctx, c)));
        }
    }
    let mut generics = Vec::new();
    local_item_generics(item, &mut generics);
    for (name, span) in free.types.iter().chain(&free.values) {
        if !ctx.ty_subst.contains_key(name)
            || generics.contains(&name.as_str())
            || !seen.insert(name)
        {
            continue;
        }
        let leaf = item.name().map(nabic_core::local::leaf).unwrap_or_default();
        let keyword = match item {
            Item::Struct(_) => "struct",
            Item::Enum(_) => "enum",
            Item::Union(_) => "union",
            Item::Trait(_) => "trait",
            Item::TypeAlias(_) => "type",
            _ => "fn",
        };
        let fix = match item {
            Item::Static(_) | Item::Const(_) => {
                format!("it has one value, whatever `{outer}` is called with")
            }
            Item::Impl(_) => format!("declare it on the impl instead: `impl<{name}>`"),
            _ => format!("declare its own instead: `{keyword} {leaf}<{name}>`"),
        };
        ctx.emit_error_at(
            Code::OUTER_LOCAL,
            *span,
            format!(
                "{what} is declared inside `{outer}` and can't use `{name}`, a generic \
                 parameter of `{outer}`; {fix}"
            ),
        );
    }
}

/// What to write for a `const` that reads a value: a `let`.
pub(crate) fn const_reads_help(ctx: &LowerCtx<'_>, c: &nabic_ast::ConstDef) -> String {
    let name = nabic_core::local::leaf(&c.name).to_lowercase();
    let value = ctx.src(c.value.span).unwrap_or("..");
    format!(
        "a `const` reads only constants and const parameters; for a value write \
         `let {name} = {value};`"
    )
}

/// The generic parameters and associated types `item` declares
/// anywhere (on itself and its methods).
pub(crate) fn local_item_generics<'a>(item: &'a Item, out: &mut Vec<&'a str>) {
    let mut gs: Vec<&nabic_ast::Generics> = Vec::new();
    match item {
        Item::Fn(f) => gs.push(&f.generics),
        Item::Struct(x) => gs.push(&x.generics),
        Item::Enum(x) => gs.push(&x.generics),
        Item::Union(x) => gs.push(&x.generics),
        Item::TypeAlias(x) => gs.push(&x.generics),
        Item::Trait(t) => {
            gs.push(&t.generics);
            for ti in &t.items {
                match ti {
                    nabic_ast::TraitItem::Method(m) => gs.push(&m.generics),
                    nabic_ast::TraitItem::AssocType { name, .. } => out.push(name),
                    nabic_ast::TraitItem::Const { .. } => {}
                }
            }
        }
        Item::Impl(i) => {
            gs.push(&i.generics);
            for ii in &i.items {
                match ii {
                    nabic_ast::ImplItem::Method(m) => gs.push(&m.generics),
                    nabic_ast::ImplItem::AssocType { name, .. } => out.push(name),
                    nabic_ast::ImplItem::Const(_) => {}
                }
            }
        }
        _ => {}
    }
    out.extend(gs.iter().flat_map(|g| &g.params).map(|p| match p {
        nabic_ast::GenericParam::Type { name, .. }
        | nabic_ast::GenericParam::Const { name, .. } => name.as_str(),
    }));
}

/// `let S { f1, f2: alias, .. } [: T] = init;` -> temp local + one field
/// read per binding; the struct type when it matched.
pub(crate) fn lower_let_struct(
    ctx: &mut LowerCtx<'_>,
    path: &AstPath,
    fields: &[(String, String)],
    ty: Option<&Ty>,
    init: &Expr,
    span: nabic_diagnostic::Span,
    stmts: &mut Vec<HirStmt>,
) -> Option<HirTy> {
    let pre_ty = ty.map(|t| ctx.resolve_ast_ty(t));
    let init_h = lower_expected(ctx, init, pre_ty.clone().unwrap_or(HirTy::Unknown));
    let init_h = match &pre_ty {
        Some(t) => coerce(ctx, t, init_h, &|| "the destructured value".to_owned()),
        None => init_h,
    };
    let init_ty = init_h.ty.clone();
    let temp_local = ctx.declare(None, init_ty.clone());
    stmts.push(HirStmt::Let {
        local: temp_local,
        init: init_h,
    });
    let segs: Vec<&str> = path.segments.iter().map(|s| s.name.as_str()).collect();
    let named = pattern_struct(ctx, path, &init_ty);
    let sid = match (&init_ty, named) {
        (HirTy::Unknown, _) => {
            // Init failed (reported); bind the names so later uses don't cascade
            // into "cannot find value".
            for (_, bind) in fields {
                ctx.declare(Some(bind.clone()), HirTy::Unknown);
            }
            return None;
        }
        (HirTy::Struct(sid), Some(n)) if *sid == n => *sid,
        // `let Q { .. } = P { x: 1.0 }` (`type Q = P<f64>`) fixes open arguments.
        (HirTy::Struct(_), Some(n))
            if crate::lower::infer::unify(ctx, &init_ty, &HirTy::Struct(n)) =>
        {
            n
        }
        (HirTy::Struct(sid), None) if path.segments.is_empty() => *sid,
        _ => {
            ctx.emit_error(
                Code::PATTERN_TYPE,
                format!(
                    "struct pattern `{}` does not match the value's type `{}`",
                    segs.join("::"),
                    ctx.ty_name(&init_ty)
                ),
            );
            for (_, bind) in fields {
                ctx.declare(Some(bind.clone()), HirTy::Unknown);
            }
            return None;
        }
    };
    let def = ctx.struct_def(sid);
    for (field_name, bind_name) in fields {
        let Some(idx) = def.fields.iter().position(|f| &f.name == field_name) else {
            ctx.no_field(sid, field_name, span);
            ctx.declare(Some(bind_name.clone()), HirTy::Unknown);
            continue;
        };
        ctx.check_field_visible(sid, field_name);
        let field_ty = def.fields[idx].ty.clone();
        let read = HirExpr {
            ty: field_ty.clone(),
            span,
            kind: HirExprKind::FieldRead {
                target: Box::new(HirExpr {
                    ty: init_ty.clone(),
                    span,
                    kind: HirExprKind::Local(temp_local),
                }),
                struct_id: sid,
                field_index: u32::try_from(idx).expect("field idx overflow"),
                field_ty: field_ty.clone(),
            },
        };
        stmts.push(ctx.declare_var(bind_name, field_ty, read, span));
    }
    Some(init_ty)
}

/// `let (a, b, ..rest) [: T] = init;` -> fresh tuple local plus per-name
/// index reads.
pub(crate) fn lower_let_tuple(
    ctx: &mut LowerCtx<'_>,
    names: &[String],
    rest: Option<&(usize, Option<String>)>,
    pre_ty: Option<HirTy>,
    init: &Expr,
    span: nabic_diagnostic::Span,
    stmts: &mut Vec<HirStmt>,
) {
    let init_h = lower_expected(ctx, init, pre_ty.clone().unwrap_or(HirTy::Unknown));
    let init_h = match &pre_ty {
        Some(t) => coerce(ctx, t, init_h, &|| "the destructured value".to_owned()),
        None => init_h,
    };
    let init_ty = init_h.ty.clone();
    let elem_tys: Vec<HirTy> = match &init_ty {
        HirTy::Tuple(ts) => ts.clone(),
        HirTy::Unknown => vec![HirTy::Unknown; names.len()],
        other => {
            ctx.emit_error(
                Code::PATTERN_TYPE,
                format!(
                    "tuple destructure of a non-tuple value of type `{}`",
                    ctx.ty_name(other)
                ),
            );
            vec![HirTy::Unknown; names.len()]
        }
    };
    // Length must match (no rest), or be >= `names.len()` with a `..`.
    let needed = names.len();
    let have = elem_tys.len();
    if rest.is_none() && have != needed {
        ctx.emit_error(
            Code::PATTERN_ARITY,
            format!(
                "tuple destructure length mismatch: pattern has {needed} element{}, value \
                 has {have}",
                if needed == 1 { "" } else { "s" }
            ),
        );
    } else if rest.is_some() && have < needed {
        ctx.emit_error(
            Code::PATTERN_ARITY,
            format!(
                "tuple destructure too short: pattern needs at least {needed} non-rest \
                 element{}, value has {have}",
                if needed == 1 { "" } else { "s" }
            ),
        );
    }
    let tuple_local = ctx.declare(None, init_ty.clone());
    stmts.push(HirStmt::Let {
        local: tuple_local,
        init: init_h,
    });
    // With a rest at `r` and an `N`-tuple: `names[..r]` take `init.0..r`,
    // the rest takes the middle, `names[r..]` the last ones.
    let rest_take = if rest.is_some() {
        have.saturating_sub(needed)
    } else {
        0
    };
    let rest_idx = rest.map_or(needed, |(i, _)| *i);
    let read = |index: usize, ety: HirTy| HirExpr {
        ty: ety.clone(),
        span,
        kind: HirExprKind::TupleField {
            target: Box::new(HirExpr {
                ty: init_ty.clone(),
                span,
                kind: HirExprKind::Local(tuple_local),
            }),
            index: u32::try_from(index).expect("tuple index overflow"),
            field_ty: ety,
        },
    };
    for (i, name) in names.iter().enumerate() {
        if name == "_" {
            continue;
        }
        let init_index = if i < rest_idx { i } else { i + rest_take };
        let elem_ty = elem_tys.get(init_index).cloned().unwrap_or(HirTy::Unknown);
        let init = read(init_index, elem_ty.clone());
        stmts.push(ctx.declare_var(name, elem_ty, init, span));
    }
    // Synthesize the rest-binding's sub-tuple if it was named.
    if let Some((rest_idx, Some(rest_name))) = rest {
        let sub: Vec<HirExpr> = (0..rest_take)
            .map(|k| {
                let src = rest_idx + k;
                read(src, elem_tys.get(src).cloned().unwrap_or(HirTy::Unknown))
            })
            .collect();
        let rest_ty = HirTy::Tuple(sub.iter().map(|e| e.ty.clone()).collect());
        let init = HirExpr {
            ty: rest_ty.clone(),
            span,
            kind: HirExprKind::Tuple { fields: sub },
        };
        stmts.push(ctx.declare_var(rest_name, rest_ty, init, span));
    }
}

/// The `: T` of a `let` at `stmt` whose type is at `ty`.
pub(crate) fn ann_span(
    ctx: &LowerCtx<'_>,
    stmt: nabic_diagnostic::Span,
    ty: nabic_diagnostic::Span,
) -> Option<nabic_diagnostic::Span> {
    if ty.is_dummy() || ty.file != stmt.file || ty.start < stmt.start {
        return None;
    }
    let head = nabic_diagnostic::Span {
        end: ty.end,
        ..stmt
    };
    let colon = ctx
        .src(head)?
        .get(..(ty.start - stmt.start) as usize)?
        .rfind(':')?;
    ctx.sub_span(head, colon, (ty.end - stmt.start) as usize)
}

/// `let name[: pre_ty] = init` (`ann` the `: T`): the binding's type
/// and its value.
pub(crate) fn lower_let_init(
    ctx: &mut LowerCtx<'_>,
    name: &str,
    pre_ty: Option<HirTy>,
    ann: Option<nabic_diagnostic::Span>,
    init: &Expr,
) -> (HirTy, HirExpr) {
    let since = ctx.env.sink.diagnostics.borrow().len();
    let init = lower_expected(ctx, init, pre_ty.clone().unwrap_or(HirTy::Unknown));
    // A bad annotation (`List` without arguments) is reported on its
    // own: nothing is left to infer from it.
    if matches!(pre_ty, Some(HirTy::Path(_))) {
        retain_since(ctx, since, |d| {
            d.tag != nabic_diagnostic::Tag::TypeParamInfer
        });
    }
    let Some(t) = pre_ty else {
        check_let_inferable(ctx, name, &init);
        return (init.ty.clone(), init);
    };
    ctx.let_ann.set(ann);
    let init = coerce(ctx, &t, init, &|| format!("`let {name}`"));
    (t, init)
}

pub(crate) fn lower_stmt(ctx: &mut LowerCtx<'_>, stmt: &Stmt) -> Option<HirStmt> {
    // Diagnostics raised while lowering default to this location.
    ctx.at_span(stmt.span, |ctx| lower_stmt_inner(ctx, stmt))
}

pub(crate) fn lower_stmt_inner(ctx: &mut LowerCtx<'_>, stmt: &Stmt) -> Option<HirStmt> {
    if let Some(s) = value_first(ctx, stmt) {
        return s;
    }
    match &stmt.kind {
        StmtKind::Let { name, ty, init } => {
            // The declared type (if any) is the init's expected type, so
            // literals and generic-enum leaves (`Box::None`) pick it up.
            let pre_ty = ty.as_ref().map(|t| ctx.resolve_ast_ty(t));
            let ast_init = init;
            guard_sink(ctx, init);
            // `using (a, b) = (..)` drops the whole tuple.
            if let (true, ExprKind::Tuple(items)) = (name.starts_with("#using"), &init.kind) {
                for i in items {
                    guard_sink(ctx, i);
                }
            }
            let ann = ty.as_ref().and_then(|t| ann_span(ctx, stmt.span, t.span));
            let lit = ty.is_none().then(|| bare_num_lit(ctx, ast_init)).flatten();
            let (declared_ty, init) = lower_let_init(ctx, name, pre_ty, ann, init);
            let ty = declared_ty.clone();
            let fresh = ctx.sole_names.contains(name.as_str()) && fresh_future(ctx, ast_init);
            let s = ctx.declare_var(name, declared_ty, init, stmt.span);
            if let HirStmt::Let { local, .. } = s {
                if let Some(lit) = lit {
                    ctx.lit_lets.insert(local, lit);
                }
                ctx.track_guard(local, &ty, stmt.span, name);
                if fresh {
                    ctx.sole_fresh.insert(local, ctx.breakables.len());
                }
            }
            Some(s)
        }
        // A bare `return;` ends a script (exit code 0).
        StmtKind::Return(Some(v)) if ctx.implicit_main => {
            let code = ctx.src(v.span).unwrap_or("code").to_owned();
            ctx.emit_error(
                Code::TOP_LEVEL_RETURN,
                "top-level `return` takes no value: the program's statements return nothing"
                    .to_owned(),
            );
            ctx.help_last(Some(format!(
                "write `return;` to end the program, or `process::exit({code});` for an \
                 exit code (`use std::process;`)"
            )));
            None
        }
        StmtKind::Return(value) => {
            let ret = ctx.fn_ret_ty.clone();
            let value = if let Some(e) = value {
                guard_sink(ctx, e);
                let lowered = lower_expected(ctx, e, ret.clone());
                // A closure without a declared return type takes it from its first
                // `return`.
                if ret == HirTy::Unknown && ctx.infer_ret {
                    ctx.fn_ret_ty = lowered.ty.clone();
                    // A poisoned value leaves the type unknown, not unset.
                    ctx.infer_ret = lowered.ty != HirTy::Unknown;
                    Some(lowered)
                } else if let Some(tys) = ctx.ret_tys.as_mut() {
                    tys.push(lowered.ty.clone());
                    Some(lowered)
                } else {
                    Some(coerce(ctx, &ret, lowered, &|| {
                        "the return value".to_owned()
                    }))
                }
            } else {
                if let Some(tys) = ctx.ret_tys.as_mut() {
                    tys.push(HirTy::Unit);
                }
                if !matches!(ret, HirTy::Unit | HirTy::Unknown) {
                    ctx.emit_error(
                        Code::MISMATCHED_TYPES,
                        format!(
                            "`return;` without a value in a function returning `{}`",
                            ctx.ty_name(&ret)
                        ),
                    );
                }
                None
            };
            Some(lower_return(ctx, value, stmt.span))
        }
        // Statement position: the value is discarded (expected `()`), so
        // `if` / `match` arms needn't agree on a type.
        StmtKind::Expr(expr) => {
            guard_sink(ctx, expr);
            let h = lower_expected(ctx, expr, HirTy::Unit);
            if let Some(g) = lock_guard(ctx, &h.ty) {
                ctx.emit_error_at(
                    Code::GUARD_NOT_RELEASED,
                    stmt.span,
                    format!(
                        "this `{g}` is discarded without being released: only `using` \
                         releases a lock guard (`using g = .. {{ .. }}`)"
                    ),
                );
            }
            unused_result(ctx, expr, &h.ty);
            Some(HirStmt::Expr(h))
        }
        // A block-like statement without `;` before more statements
        // (`if c { 1 } else { 2 }  next();`) must not produce a value.
        StmtKind::Trailing(expr) => {
            let h = lower_expected(ctx, expr, HirTy::Unit);
            if let Some(t) = arm_value_ty(&h) {
                ctx.emit_error_at(
                    Code::BLOCK_VALUE_DISCARDED,
                    stmt.span,
                    format!(
                        "this block-like statement leaves a value of type `{}` without `;`: a \
                         statement must be `()` (add `;` to discard it, or make it the \
                         block's last expression)",
                        ctx.ty_name(&t)
                    ),
                );
            }
            Some(HirStmt::Expr(h))
        }
        StmtKind::Item(nabic_ast::Item::Use(u)) => {
            // Function-local `use`: visible in the current scope only.
            let mut entries = Vec::new();
            crate::uses::flatten_tree_for_local(&u.tree, &ctx.current_module, &mut entries);
            for e in entries {
                ctx.add_local_use(e);
            }
            None
        }
        StmtKind::Item(Item::Fn(f)) if nabic_ast::nested_marker_of(f).is_some() => None,
        StmtKind::Item(item) if item.is_local() => {
            local_item(ctx, item);
            None
        }
        // The parser keeps no other item in a block.
        StmtKind::Item(_) => unreachable!("a block item other than `fn` / `const` / `use`"),
        StmtKind::Assign(target, _) | StmtKind::CompoundAssign { target, .. }
            if default_index_of(target).is_some() =>
        {
            Some(index_default_store(ctx, stmt, target))
        }
        StmtKind::Assign(target, value) => {
            if !is_place(ctx, target, "=", value) {
                return None;
            }
            let a = lower_assign(ctx, target, value, stmt.span);
            write_through(ctx, a, stmt.span)
        }
        StmtKind::CompoundAssign { target, op, value } => {
            let placed = unplace(ctx, target);
            let target = placed.as_ref().unwrap_or(target);
            lower_compound_assign(ctx, target, op, value, stmt.span)
        }
        StmtKind::LetTuple { .. } | StmtKind::LetStruct { .. } | StmtKind::LetPattern { .. } => {
            unreachable!("destructuring lets are expanded by lower_block")
        }
        StmtKind::Break { label, value } => {
            check_jump_target(ctx, "break", label.as_deref(), false);
            let depth = jump_index(ctx, label.as_deref(), false);
            let value = if let Some(e) = value.as_ref().filter(|_| ctx.unit_loops.contains(&depth))
            {
                let _ = lower_expected(ctx, e, HirTy::Unknown);
                ctx.emit_error(
                    Code::OUTSIDE_CONTEXT,
                    "`break` with a value inside a `while` or `for` loop, whose value is always \
                     `()`"
                        .to_owned(),
                );
                ctx.help_last(Some(
                    "write `loop { ... break value; ... }` to leave with a value, or drop the \
                     value: `break;`"
                        .to_owned(),
                ));
                None
            } else if let Some(e) = value {
                Some(lower_break_value(ctx, depth, e))
            } else {
                match ctx.break_tys.get(&depth).cloned() {
                    None => {
                        ctx.break_tys.insert(depth, (HirTy::Unit, true));
                    }
                    Some((t, _)) if t != HirTy::Unit => {
                        let t = ctx.ty_name(&t);
                        ctx.emit_error(
                            Code::BRANCH_TYPES,
                            format!(
                                "`break` without a value, but this loop's `break` values are \
                                 `{t}`"
                            ),
                        );
                        ctx.help_last(Some(format!(
                            "write `break <value>;` with {} value",
                            nabic_diagnostic::article(&t)
                        )));
                    }
                    Some(_) => {}
                }
                None
            };
            let label = label.clone();
            Some(exit_through(
                ctx,
                Some(depth),
                value,
                |value| HirStmt::Break { label, value },
                stmt.span,
            ))
        }
        StmtKind::Continue { label } => {
            check_jump_target(ctx, "continue", label.as_deref(), true);
            let depth = jump_index(ctx, label.as_deref(), true);
            let label = label.clone();
            Some(exit_through(
                ctx,
                Some(depth),
                None,
                |_| HirStmt::Continue { label },
                stmt.span,
            ))
        }
        StmtKind::Throw(value) => Some(lower_throw(ctx, value, stmt.span)),
    }
}

/// An unannotated `let` needs a typed initializer: not `none` alone nor
/// one that never yields (empty `[]` and mismatched branches are rejected
/// where lowered).
pub(crate) fn check_let_inferable(ctx: &LowerCtx<'_>, name: &str, init: &HirExpr) {
    let why = if init.ty.is_none() {
        format!("`none` alone has no value type - write `let {name}: T | none = none;`")
    } else if hir_expr_diverges(init) {
        "the initializer never produces a value (it always returns / breaks / loops)".to_owned()
    } else {
        return;
    };
    ctx.emit_error(
        Code::TYPE_ANNOTATIONS_NEEDED,
        format!("cannot infer the type of `{name}`: {why}"),
    );
}

/// A value a statement may drop silently: `()`, `none`, `Never`, and
/// what an error left unknown.
pub(crate) fn droppable(ty: &HirTy) -> bool {
    match ty {
        HirTy::Unit
        | HirTy::None
        | HirTy::Never
        | HirTy::Unknown
        | HirTy::Any
        | HirTy::NoThrow
        | HirTy::Var(_) => true,
        HirTy::Sum(ts) => ts.iter().all(droppable),
        _ => false,
    }
}

/// `unused_result`: expression statement `e` of type `ty` drops a value.
/// A call of a `#[discardable]` function may; `try? f();` is
/// `ignored_error`'s, and a block-like statement's value is its arms'.
pub(crate) fn unused_result(ctx: &LowerCtx<'_>, e: &Expr, ty: &HirTy) {
    // An open literal is the number it holds.
    let ty = &crate::lower::infer::zonk(ctx, ty);
    if droppable(ty) || e.span.is_dummy() || !ctx.results_apply() {
        return;
    }
    let mut inner = e;
    loop {
        inner = match &inner.kind {
            ExprKind::Try {
                kind: nabic_ast::TryKind::Plain | nabic_ast::TryKind::Force,
                operand,
            } => operand,
            ExprKind::Await(x) | ExprKind::InModule(_, x) => x,
            ExprKind::Try { .. }
            | ExprKind::Block(_)
            | ExprKind::Unsafe(_)
            | ExprKind::ConstBlock(_)
            | ExprKind::If { .. }
            | ExprKind::Match { .. }
            | ExprKind::Loop { .. }
            | ExprKind::While { .. }
            | ExprKind::For { .. }
            | ExprKind::Catch { .. }
            | ExprKind::Closure { .. }
            | ExprKind::Yield(_)
            | ExprKind::AsyncBlock(_)
            | ExprKind::YieldBlock(_)
            | ExprKind::AsyncYieldBlock(_) => return,
            _ => break,
        };
    }
    // The outermost call ends where the statement does; a call in its
    // arguments ends before.
    let discardable = ctx.discardable_calls.borrow().iter().any(|c| {
        c.file == inner.span.file && c.end == inner.span.end && c.start >= inner.span.start
    });
    if discardable {
        return;
    }
    let what = match &inner.kind {
        ExprKind::Call { callee, .. } => match &callee.kind {
            ExprKind::Path(p) => p.segments.last().map(|s| format!("`{}`", s.name)),
            _ => None,
        },
        ExprKind::MethodCall { method, .. } => Some(format!("`{method}`")),
        _ => None,
    };
    let msg = match what {
        Some(f) => format!("the `{}` result of {f} is never used", ctx.ty_name(ty)),
        None => format!("this `{}` value is never used", ctx.ty_name(ty)),
    };
    ctx.emit_lint(
        "unused_result",
        e.span,
        msg,
        format!(
            "use it, or write `_ = {};` to drop it on purpose",
            snippet(e)
        ),
    );
}

/// `e` as source for a diagnostic's suggested fix; `..` when long.
pub(crate) fn snippet(e: &Expr) -> String {
    let s = nabic_ast::print::print_expr(e);
    if s.len() > 40 || s.contains('\n') {
        "..".to_owned()
    } else {
        s
    }
}

/// A guard received without `using` must be bound with `using`. `e` (and
/// its branches) may hand one over unreleased: to a binding or out of the
/// function.
pub(crate) fn guard_sink(ctx: &mut LowerCtx<'_>, e: &Expr) {
    ctx.guard_sinks.insert(std::ptr::from_ref(e) as usize);
    let tail = |ctx: &mut LowerCtx<'_>, b: &Block| {
        if let Some(t) = &b.trailing {
            guard_sink(ctx, t);
        }
    };
    match &e.kind {
        ExprKind::Block(b) | ExprKind::Unsafe(b) => tail(ctx, b),
        ExprKind::If {
            then_branch,
            else_branch,
            ..
        } => {
            tail(ctx, then_branch);
            if let Some(x) = else_branch {
                guard_sink(ctx, x);
            }
        }
        ExprKind::Match { arms, .. } => {
            for a in arms {
                guard_sink(ctx, &a.body);
            }
        }
        ExprKind::Catch { body, catches, .. } => {
            guard_sink(ctx, body);
            for c in catches {
                tail(ctx, &c.body);
            }
        }
        ExprKind::Try { operand, .. } => guard_sink(ctx, operand),
        _ => {}
    }
}

/// A guard `expr` produces outside a `guard_sink` is never released; a
/// guard variable read in one is moved to its new holder.
pub(crate) fn guard_flow(ctx: &mut LowerCtx<'_>, expr: &Expr, e: &HirExpr) {
    let Some(g) = lock_guard(ctx, &e.ty) else {
        return;
    };
    let sink = ctx
        .guard_sinks
        .contains(&(std::ptr::from_ref(expr) as usize));
    match &expr.kind {
        ExprKind::Path(p) => {
            if let (true, [seg]) = (sink, p.segments.as_slice()) {
                if let Some(l) = ctx.lookup(&seg.name) {
                    ctx.guard_lets.retain(|x| x.0 != l);
                }
            }
        }
        // Reads of a place, and forms whose value comes from inner ones.
        ExprKind::SelfLower
        | ExprKind::Field(..)
        | ExprKind::TupleField(..)
        | ExprKind::Index(..)
        | ExprKind::Block(_)
        | ExprKind::Unsafe(_)
        | ExprKind::ConstBlock(_)
        | ExprKind::If { .. }
        | ExprKind::Match { .. }
        | ExprKind::Catch { .. }
        | ExprKind::Try { .. }
        | ExprKind::Loop { .. } => {}
        _ if !sink => ctx.emit_error_at(
            Code::GUARD_NOT_RELEASED,
            expr.span,
            format!(
                "this `{g}` is never released: a guard received without `using` must be \
                 bound with `using g = .. {{ .. }}`"
            ),
        ),
        _ => {}
    }
}
