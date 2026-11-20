//! Struct literals, update syntax and field access.

#[allow(clippy::wildcard_imports)]
use crate::lower::expr::*;

pub(crate) fn check_duplicate_fields(ctx: &LowerCtx<'_>, segs: &[&str], fields: &[(String, Expr)]) {
    let mut seen = nabic_core::HashSet::default();
    for (n, _) in fields {
        if !seen.insert(n.as_str()) {
            ctx.emit_error(
                Code::DUPLICATE_FIELD,
                format!(
                    "field `{n}` specified more than once in `{}` literal",
                    segs.join("::")
                ),
            );
        }
    }
}

/// A struct's declared `(field, type)`s; a tuple struct's are `0`, `1`, ...
pub(crate) fn struct_decl_fields(def: &AstStructDef) -> Vec<(String, &Ty)> {
    match &def.kind {
        AstStructKind::Named(fs) => fs.iter().map(|f| (f.name.clone(), &f.ty)).collect(),
        AstStructKind::Tuple(fs) => fs
            .iter()
            .enumerate()
            .map(|(i, f)| (i.to_string(), &f.ty))
            .collect(),
        AstStructKind::Unit => Vec::new(),
    }
}

/// Parameters and declared fields of generic struct or union `full`.
pub(crate) fn generic_template<'a>(
    ctx: &LowerCtx<'a>,
    full: &str,
) -> Option<(&'a nabic_ast::Generics, Vec<(String, &'a Ty)>)> {
    if let Some(d) = ctx.env.defs.generic_struct(full) {
        return Some((&d.generics, struct_decl_fields(d)));
    }
    let u = ctx.env.defs.generic_union(full)?;
    Some((
        &u.generics,
        u.fields.iter().map(|f| (f.name.clone(), &f.ty)).collect(),
    ))
}

/// A list literal for a `[T; N]` field is an array of its own length, not a
/// `List`: `V { c: [1.0, 2.0] }` binds `T` from the elements. Nested
/// literals for nested arrays likewise; open element types stay unknown.
fn array_hint(ty: &Ty, e: &Expr) -> HirTy {
    let (AstTyKind::Array(elem, _), ExprKind::List(items)) = (&ty.kind, &e.kind) else {
        return HirTy::Unknown;
    };
    let Ok(n) = u32::try_from(items.len()) else {
        return HirTy::Unknown;
    };
    let inner = items
        .first()
        .map_or(HirTy::Unknown, |x| array_hint(elem, x));
    HirTy::Array(Box::new(inner), n)
}

/// `Self` of an impl of a struct, as a struct path.
pub(crate) fn self_struct_id(ctx: &LowerCtx<'_>, segs: &[&str]) -> Option<StructId> {
    match (segs, &ctx.self_ty) {
        (["Self"], Some(HirTy::Struct(id))) => Some(*id),
        _ => None,
    }
}

/// `E::<i32>::V`: the enum instance its written arguments name (they
/// take the place of the expected type).
pub(crate) fn explicit_enum(ctx: &LowerCtx<'_>, p: &AstPath) -> Option<HirTy> {
    let [.., e, _] = p.segments.as_slice() else {
        return None;
    };
    if e.generics.is_empty() {
        return None;
    }
    let segs: Vec<&str> = p.segments.iter().map(|s| s.name.as_str()).collect();
    // `Conv::<f64>::conv(x)`: a trait's arguments, not an enum's.
    if ctx
        .canon_type(&segs[..segs.len() - 1])
        .is_some_and(|k| ctx.env.names.is_trait(&k))
    {
        return None;
    }
    ctx.enum_path(&segs)?;
    let n = p.segments.len();
    Some(ctx.resolve_ast_ty(&Ty {
        kind: AstTyKind::Path(AstPath {
            segments: p.segments[..n - 1].to_vec(),
            span: p.span,
            origin: None,
        }),
        span: p.span,
    }))
}

/// The variant a struct-literal path names, if any. A non-core struct
/// beats a same-named core variant from the library's `core::**`
/// (`struct Sha256` vs `HashAlgorithm::Sha256`), and any struct beats a
/// variant without named fields (`struct Sha3_256` vs
/// `HashAlgorithm::Sha3_256`): that literal could never build it.
pub(crate) fn init_variant(ctx: &LowerCtx<'_>, segs: &[&str], hint: Option<&HirTy>) -> EnumRes {
    let unnamed = |e: &str, v: &str| {
        ctx.lookup_enum(e).is_some_and(|id| {
            ctx.enum_def(id)
                .variants
                .iter()
                .any(|x| x.name == v && x.named_fields.is_empty())
        })
    };
    let shadowed = segs.len() == 1
        && ctx.canon_type(segs).is_some_and(|t| {
            ctx.enum_path(segs).is_some_and(|(e, v)| {
                (!t.starts_with("core::") && e.starts_with("core::")) || unnamed(&e, &v)
            })
        });
    if shadowed {
        return EnumRes::NotEnum;
    }
    resolve_enum_variant(ctx, segs, hint, None)
}

/// `Q { .. }` with `type Q = P<f64>`: the aliased struct's path,
/// `P::<f64>`; `type A<T> = P<T>` gives `P` with inferred arguments.
/// `None` if `path` names no alias.
fn aliased_struct(
    ctx: &LowerCtx<'_>,
    path: &AstPath,
    segs: &[&str],
) -> Option<Result<AstPath, String>> {
    // `A<f64> { .. }` resolves as a type.
    if !path.segments.last()?.generics.is_empty() {
        return None;
    }
    let full = ctx.canon_type(segs)?;
    let names = &ctx.env.names;
    let is_alias = |k: &str| names.plain_alias_ty(k).is_some() || names.generic_alias(k).is_some();
    if !is_alias(&full) {
        return None;
    }
    // The alias as the prefix of a dummy item path, which `alias_path`
    // expands; the dummy is dropped again.
    let mut probe = path.clone();
    probe.segments.push(nabic_ast::PathSegment {
        name: String::new(),
        generics: Vec::new(),
    });
    let written = segs.join("::");
    match alias_path(ctx, &probe) {
        Ok(Some(mut p)) => {
            p.segments.pop();
            // A cycle of aliases expands to an alias again (reported as one).
            let segs: Vec<&str> = p.segments.iter().map(|s| s.name.as_str()).collect();
            let open = p.segments.last().is_some_and(|s| s.generics.is_empty());
            if open && ctx.canon_type(&segs).is_some_and(|k| is_alias(&k)) {
                return None;
            }
            Some(Ok(p))
        }
        Ok(None) => None,
        Err(_) => Some(Err(format!(
            "`{written} {{ .. }}` can't infer `{written}`'s arguments: its aliased type's \
             arguments are not its distinct parameters - write `{written}<..> {{ .. }}`"
        ))),
    }
}

/// The struct a pattern `S { .. }` names, matched against `scrut`: `Self`,
/// a struct, an alias of one, or a generic struct without arguments
/// (`P { x }` takes the scrutinee's `P<f64>`).
pub(crate) fn pattern_struct(
    ctx: &LowerCtx<'_>,
    path: &AstPath,
    scrut: &HirTy,
) -> Option<StructId> {
    let segs: Vec<&str> = path.segments.iter().map(|s| s.name.as_str()).collect();
    if let Some(id) = self_struct_id(ctx, &segs) {
        return Some(id);
    }
    let path = match aliased_struct(ctx, path, &segs) {
        Some(Ok(p)) => p,
        Some(Err(_)) => return None,
        None => path.clone(),
    };
    if path.segments.last()?.generics.is_empty() {
        let segs: Vec<&str> = path.segments.iter().map(|s| s.name.as_str()).collect();
        let full = ctx.canon_type(&segs)?;
        if let Some(id) = ctx.lookup_struct(&full) {
            return Some(id);
        }
        return match scrut {
            HirTy::Struct(id) if enum_base_name(&ctx.struct_def(*id).name) == full => Some(*id),
            _ => None,
        };
    }
    let ty = Ty {
        span: path.span,
        kind: AstTyKind::Path(path),
    };
    match ctx.resolve_ast_ty(&ty) {
        HirTy::Struct(id) => Some(id),
        _ => None,
    }
}

/// `S { a: v, .. }`, or with `spread` `S { a: v, ..base }`:
/// unwritten fields come from `base`, evaluated after the written ones.
#[allow(clippy::cognitive_complexity)] // struct, enum variant, `Self` and generic forms
pub(crate) fn lower_struct_init(
    ctx: &mut LowerCtx<'_>,
    path: &AstPath,
    fields: &[(String, Expr)],
    spread: Option<&Expr>,
    span: nabic_diagnostic::Span,
) -> HirExpr {
    let segs: Vec<&str> = path.segments.iter().map(|s| s.name.as_str()).collect();
    match aliased_struct(ctx, path, &segs) {
        Some(Ok(p)) => return lower_struct_init(ctx, &p, fields, spread, span),
        Some(Err(msg)) => {
            for (_, e) in fields {
                lower_expected(ctx, e, HirTy::Unknown);
            }
            return ctx.error_expr(Code::GENERIC_ARGUMENTS, span, msg);
        }
        None => {}
    }
    check_duplicate_fields(ctx, &segs, fields);
    // `Enum::Variant { x: .., y: .. }`: fields packed into the tuple payload
    // in declaration order.
    let mut hint = explicit_enum(ctx, path).or_else(|| ctx.current_expected().cloned());
    match init_variant(ctx, &segs, hint.as_ref()) {
        EnumRes::Error => return poison(span),
        EnumRes::Found(..) if spread.is_some() => {
            return ctx.error_expr(
                Code::WRONG_ITEM_FORM,
                span,
                format!(
                    "`..base` needs a struct; `{}` is an enum variant",
                    segs.join("::")
                ),
            );
        }
        EnumRes::Found(enum_id, idx) => {
            let variant = ctx.enum_def(enum_id).variants[idx].clone();
            if variant.named_fields.is_empty() {
                return ctx.error_expr(
                    Code::WRONG_ITEM_FORM,
                    span,
                    format!("`{}` is not a struct-like enum variant", segs.join("::")),
                );
            }
            let field_tys = variant_arg_tys(&variant);
            let mut payload_args: Vec<Option<HirExpr>> = vec![None; variant.named_fields.len()];
            for (n, e) in fields {
                match variant.named_fields.iter().position(|f| f == n) {
                    Some(i) => {
                        let v = lower_expected(ctx, e, field_tys[i].clone());
                        payload_args[i] =
                            Some(coerce(ctx, &field_tys[i], v, &|| format!("field `{n}`")));
                    }
                    None => {
                        ctx.emit_error(
                            Code::NO_FIELD,
                            format!("enum variant `{}` has no field `{n}`", segs.join("::")),
                        );
                    }
                }
            }
            let mut args = Vec::with_capacity(payload_args.len());
            for (i, a) in payload_args.into_iter().enumerate() {
                if let Some(a) = a {
                    args.push(a);
                } else {
                    ctx.emit_error(
                        Code::MISSING_FIELD,
                        format!(
                            "enum variant `{}` is missing field `{}`",
                            segs.join("::"),
                            variant.named_fields[i]
                        ),
                    );
                    args.push(poison(span));
                }
            }
            return HirExpr {
                kind: HirExprKind::EnumVariantInit {
                    enum_id,
                    variant_index: u32::try_from(idx).expect("variant overflow"),
                    args,
                },
                ty: HirTy::Enum(enum_id),
                span,
            };
        }
        EnumRes::NotEnum => {}
    }

    let base_h = spread.map(|b| lower_update_base(ctx, b, &mut hint));
    let self_struct = self_struct_id(ctx, &segs);
    let full =
        self_struct.map_or_else(|| ctx.canon_type(&segs), |id| Some(ctx.struct_def(id).name));
    let Some(full) = full else {
        for (_, e) in fields {
            lower_expected(ctx, e, HirTy::Unknown);
        }
        // `run { }` / `zero { n }` after a function's name is a struct literal;
        // the closure needs the parentheses.
        let written = segs.join("::");
        if ctx.resolve_any_fn(&segs).is_some() {
            return ctx.unknown_name(
                Code::UNRESOLVED_TYPE,
                span,
                format!("`{written} {{ .. }}` is a struct literal, but `{written}` is a function"),
                Some(format!(
                    "write the closure in parentheses: `{written}(|| {{ .. }})`"
                )),
            );
        }
        let help = ctx.type_help(&segs);
        return ctx.unknown_name(
            Code::UNRESOLVED_TYPE,
            span,
            format!("cannot find struct `{written}` in this scope"),
            help,
        );
    };
    // Values lowered before the struct is known (generic inference);
    // reused below so each field expression is evaluated exactly once.
    let mut pre: Vec<Option<HirExpr>> = vec![None; fields.len()];
    let explicit_generics = path.segments.last().is_some_and(|s| !s.generics.is_empty());
    let struct_id = if let Some(id) = self_struct {
        id
    } else if explicit_generics {
        let synth = Ty {
            kind: AstTyKind::Path(path.clone()),
            span: path.span,
        };
        let errors = ctx.env.sink.diagnostics.borrow().len();
        match ctx.resolve_ast_ty(&synth) {
            HirTy::Struct(id) => id,
            // Resolving reported why.
            _ if ctx.env.sink.diagnostics.borrow().len() > errors => return poison(span),
            _ => {
                return ctx.error_expr(
                    Code::GENERIC_ARGUMENTS,
                    span,
                    format!(
                        "cannot instantiate `{}` with these type arguments",
                        segs.join("::")
                    ),
                );
            }
        }
    } else if let Some(id) = ctx.lookup_struct(&full) {
        id
    } else if let Some((generics, decl_fields)) = generic_template(ctx, &full) {
        // `Box { v: 5 }`: the expected type picks the instantiation;
        // otherwise infer the type arguments from the field values.
        match &hint {
            Some(HirTy::Struct(h))
                if enum_base_name(&ctx.struct_def(*h).name) == full
                    && !ctx.struct_def(*h).generic_args.is_empty() =>
            {
                *h
            }
            _ => {
                let generic_names: nabic_core::HashSet<String> = generics
                    .params
                    .iter()
                    .map(|gp| match gp {
                        nabic_ast::GenericParam::Type { name, .. }
                        | nabic_ast::GenericParam::Const { name, .. } => name.clone(),
                    })
                    .collect();
                let mut subst: HashMap<String, HirTy> = HashMap::default();
                for (i, (n, e)) in fields.iter().enumerate() {
                    let decl = decl_fields.iter().find(|(name, _)| name == n).map(|d| d.1);
                    let v =
                        lower_expected(ctx, e, decl.map_or(HirTy::Unknown, |t| array_hint(t, e)));
                    if let Some(ty) = decl {
                        unify_param_ty_with(ty, &v.ty, &generic_names, &mut subst, ctx);
                    }
                    pre[i] = Some(v);
                }
                let mut args = Vec::new();
                for gp in &generics.params {
                    if let nabic_ast::GenericParam::Const { name, ty } = gp {
                        let c = subst
                            .get(name)
                            .and_then(HirTy::as_const)
                            .and_then(|v| crate::lower::mono::typed_const(ctx, name, ty, v));
                        let Some(c) = c else {
                            // An annotation whose arguments were wrong, reported there.
                            if matches!(&hint, Some(HirTy::Path(p)) if *p == [full.clone()]) {
                                return poison(span);
                            }
                            ctx.emit_uninferred(format!(
                                "cannot infer const parameter `{name}` of `{}` - \
                                 annotate the binding (`let v: {}<..> = ..`)",
                                ctx.display_path(&full),
                                segs.join("::")
                            ));
                            return poison(span);
                        };
                        args.push(c);
                    }
                    if let nabic_ast::GenericParam::Type { name, default, .. } = gp {
                        if let (Some(t), Some(d)) = (subst.get(name), default) {
                            let d = ctx.resolve_ast_ty(d);
                            crate::lower::infer::adopt_default(ctx, t, &d, &|| {
                                format!("the default of `{}`'s `{name}`", ctx.display_path(&full))
                            });
                        }
                        match subst
                            .get(name)
                            .cloned()
                            .or_else(|| default.as_ref().map(|d| ctx.resolve_ast_ty(d)))
                        {
                            Some(t) if t != HirTy::Unknown => args.push(t),
                            Some(_) => return poison(span),
                            None if ctx.infer_vars => args.push(crate::lower::infer::fresh(ctx)),
                            None => {
                                ctx.emit_uninferred(format!(
                                    "cannot infer type parameter `{name}` of `{}` - \
                                     write `{}<...> {{ ... }}`",
                                    ctx.display_path(&full),
                                    segs.join("::")
                                ));
                                ctx.tag_last(nabic_diagnostic::Tag::TypeParamInfer);
                                return poison(span);
                            }
                        }
                    }
                }
                match ctx.instantiate_struct(&full, args, ctx.here()) {
                    Some(id) => id,
                    None => return poison(span),
                }
            }
        }
    } else {
        return ctx.error_expr(
            Code::WRONG_ITEM_FORM,
            span,
            format!("`{}` is not a struct", ctx.env.names.visible_path(&full)),
        );
    };
    let def = ctx.struct_def(struct_id);
    let ty_display = ctx.ty_name(&HirTy::Struct(struct_id));
    // `union { f: v }`: exactly one field, cast/bitcast into the synthetic
    // `__data` slot.
    if let Some(union_kind) = def.union_kind.clone() {
        if spread.is_some() {
            return ctx.error_expr(
                Code::INVALID_UNION,
                span,
                format!("`..base` cannot build union `{ty_display}`"),
            );
        }
        if fields.len() != 1 {
            return ctx.error_expr(
                Code::INVALID_UNION,
                span,
                format!(
                    "union `{ty_display}` initializer must list exactly one field, found {}",
                    fields.len()
                ),
            );
        }
        let (fname, value) = &fields[0];
        let Some(field_ty) = union_kind
            .aliases
            .iter()
            .find(|(n, _)| n == fname)
            .map(|(_, t)| t.clone())
        else {
            return ctx.no_field(struct_id, fname, span);
        };
        ctx.check_field_visible(struct_id, fname);
        let lowered = lower_expected(ctx, value, field_ty.clone());
        let lowered = coerce(ctx, &field_ty, lowered, &|| format!("field `{fname}`"));
        let storage_ty = def.fields[0].ty.clone();
        let casted = HirExpr {
            kind: HirExprKind::UnionWrite {
                struct_id,
                from_ty: lowered.ty.clone(),
                storage_ty: storage_ty.clone(),
                value: Box::new(lowered),
            },
            ty: storage_ty,
            span,
        };
        return HirExpr {
            kind: HirExprKind::StructInit {
                struct_id,
                fields: vec![casted],
            },
            ty: HirTy::Struct(struct_id),
            span,
        };
    }
    let mut ordered: Vec<Option<HirExpr>> = vec![None; def.fields.len()];
    for (i, (name, value)) in fields.iter().enumerate() {
        let Some(idx) = def.fields.iter().position(|f| f.name == *name) else {
            if pre[i].is_none() {
                lower_expected(ctx, value, HirTy::Unknown);
            }
            ctx.no_field(struct_id, name, span);
            continue;
        };
        let at = if value.span.is_dummy() {
            span
        } else {
            value.span
        };
        ctx.check_field_visible_at(struct_id, name, at);
        let target_ty = def.fields[idx].ty.clone();
        let lowered = match pre[i].take() {
            Some(v) => v,
            None => lower_expected(ctx, value, target_ty.clone()),
        };
        ordered[idx] = Some(coerce(ctx, &target_ty, lowered, &|| {
            format!("field `{name}`")
        }));
    }
    if let Some(base_h) = base_h {
        return struct_update(ctx, struct_id, ordered, base_h, span);
    }
    // A missing field takes its declared default, else it is an error.
    let mut out = Vec::with_capacity(ordered.len());
    for (f, v) in def.fields.iter().zip(ordered) {
        match (v, &f.default) {
            (Some(v), _) => out.push(v),
            (None, Some(default_expr)) => {
                out.push(field_default(ctx, &def.name, &f.name, &f.ty, default_expr));
            }
            // A field this code cannot see is omitted only when it has a default.
            (None, None) if ctx.field_hidden(struct_id, &f.name).is_some() => {
                ctx.emit_error(
                    Code::PRIVATE_ITEM,
                    format!(
                        "struct `{ty_display}` has private field `{}` with no default, so a \
                         literal here cannot leave it out: build the value with a function \
                         of its module instead (a constructor such as `new`)",
                        f.name
                    ),
                );
                out.push(poison(span));
            }
            (None, None) => {
                ctx.emit_error(
                    Code::MISSING_FIELD,
                    format!(
                        "struct `{ty_display}` is missing field `{}` and no default is \
                         declared",
                        f.name
                    ),
                );
                out.push(poison(span));
            }
        }
    }
    HirExpr {
        kind: HirExprKind::StructInit {
            struct_id,
            fields: out,
        },
        ty: HirTy::Struct(struct_id),
        span,
    }
}

/// Field default of `owner`: a constant expression resolved in the
/// struct's module, evaluated and embedded like a const.
pub(crate) fn field_default(
    ctx: &mut LowerCtx<'_>,
    owner: &str,
    field: &str,
    ty: &HirTy,
    value: &Expr,
) -> HirExpr {
    let module = module_of(enum_base_name(owner));
    let saved_module = std::mem::replace(&mut ctx.current_module, module);
    let saved_uses = std::mem::replace(&mut ctx.local_uses, vec![Vec::new()]);
    let saved_scopes = std::mem::replace(&mut ctx.scopes, vec![HashMap::default()]);
    let what = format!("the default of field `{field}`");
    let v = crate::lower::infer::scoped(ctx, |ctx| {
        let v = lower_expected(ctx, value, ty.clone());
        coerce(ctx, ty, v, &|| what.clone())
    });
    ctx.current_module = saved_module;
    ctx.local_uses = saved_uses;
    ctx.scopes = saved_scopes;
    crate::lower::consteval::constant(ctx, &what, v)
}

/// The `..base` of a struct update, lowered against the expected
/// struct; its type is the hint for a generic struct's arguments.
pub(crate) fn lower_update_base(
    ctx: &mut LowerCtx<'_>,
    base: &Expr,
    hint: &mut Option<HirTy>,
) -> HirExpr {
    let want = match hint {
        Some(t @ HirTy::Struct(_)) => t.clone(),
        _ => HirTy::Unknown,
    };
    let h = lower_expected(ctx, base, want);
    if matches!(h.ty, HirTy::Struct(_)) && !matches!(hint, Some(HirTy::Struct(_))) {
        *hint = Some(h.ty.clone());
    }
    h
}

/// `{ let #f.. = written values; let #b = base; S { .., f: #b.f } }`.
pub(crate) fn struct_update(
    ctx: &mut LowerCtx<'_>,
    struct_id: StructId,
    written: Vec<Option<HirExpr>>,
    base_h: HirExpr,
    span: nabic_diagnostic::Span,
) -> HirExpr {
    let ty = HirTy::Struct(struct_id);
    let base_h = coerce(ctx, &ty, base_h, &|| "the `..` base".to_owned());
    let def = ctx.struct_def(struct_id);
    let mut lets = Vec::new();
    let mut vals = Vec::with_capacity(written.len());
    for v in written {
        vals.push(v.map(|v| {
            let vty = v.ty.clone();
            let local = ctx.declare(None, vty.clone());
            lets.push(HirStmt::Let { local, init: v });
            hir_local(local, &vty, span)
        }));
    }
    let base = ctx.declare(None, ty.clone());
    lets.push(HirStmt::Let {
        local: base,
        init: base_h,
    });
    let out = def
        .fields
        .iter()
        .zip(vals)
        .map(|(f, v)| {
            v.unwrap_or_else(|| field_access(ctx, hir_local(base, &ty, span), &f.name, span))
        })
        .collect();
    with_lets(
        lets,
        HirExpr {
            kind: HirExprKind::StructInit {
                struct_id,
                fields: out,
            },
            ty,
            span,
        },
    )
}

/// `expr.field` on a struct.
pub(crate) fn lower_field(
    ctx: &mut LowerCtx<'_>,
    target: &Expr,
    name: &str,
    span: nabic_diagnostic::Span,
) -> HirExpr {
    let target_h = lower_expected(ctx, target, HirTy::Unknown);
    let has_field = match &target_h.ty {
        HirTy::Struct(id) => {
            let def = ctx.struct_def(*id);
            def.fields.iter().any(|f| f.name == name)
                || def
                    .union_kind
                    .as_ref()
                    .is_some_and(|u| u.aliases.iter().any(|(n, _)| n == name))
        }
        HirTy::Unknown => true,
        _ => false,
    };
    // `obj.method` is not a function value.
    if !has_field && matches!(pick_method(ctx, &target_h.ty, name, None), Ok(Some(_))) {
        let recv = nabic_ast::print::print_expr(target);
        let r = ctx.error_expr(
            Code::WRONG_ITEM_FORM,
            span,
            format!(
                "`{recv}.{name}` names a method without calling it; `.method` is not a \
                 function value"
            ),
        );
        ctx.help_last(Some(format!(
            "call it (`{recv}.{name}(..)`), wrap it in a closure (`|..| {recv}.{name}(..)`), \
             or use the method path `{}::{name}`",
            ctx.ty_name(&target_h.ty)
        )));
        return r;
    }
    if !has_field && name == "await" {
        let recv = nabic_ast::print::print_expr(target);
        let r = ctx.error_expr(
            Code::FOREIGN_SYNTAX,
            span,
            "`await` goes before the expression, not after it".to_owned(),
        );
        ctx.help_last(Some(format!("write `await {recv}`")));
        return r;
    }
    field_access(ctx, target_h, name, span)
}

/// Read field `name` of an already-lowered struct / union value.
pub(crate) fn field_access(
    ctx: &LowerCtx<'_>,
    target_h: HirExpr,
    name: &str,
    span: nabic_diagnostic::Span,
) -> HirExpr {
    let struct_id = match &target_h.ty {
        HirTy::Struct(id) => *id,
        HirTy::Unknown => return target_h,
        // An unknown type name: reported where it is written.
        HirTy::Path(_) => return poison(span),
        other => {
            return ctx.error_expr(
                Code::NO_FIELD,
                span,
                format!("no field `{name}` on type `{}`", ctx.ty_name(other)),
            );
        }
    };
    let def = ctx.struct_def(struct_id);
    // A union field read is a `UnionRead`: a cast/bitcast of the one `__data`
    // slot.
    if let Some(union_kind) = def.union_kind.clone() {
        // Reinterpreting the bits can make an invalid value (a bad `bool`, a
        // forged GC reference).
        if ctx.unsafe_depth == 0 {
            ctx.emit_error(
                Code::UNSAFE_REQUIRED,
                format!(
                    "reading field `{name}` of union `{}` needs an `unsafe` block \
                     (the bits may not be a valid value of the field's type)",
                    ctx.ty_name(&HirTy::Struct(struct_id))
                ),
            );
        }
        let storage_ty = def.fields[0].ty.clone();
        let Some(field_ty) = union_kind
            .aliases
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, t)| t.clone())
        else {
            return ctx.no_field(struct_id, name, span);
        };
        ctx.check_field_visible(struct_id, name);
        return HirExpr {
            kind: HirExprKind::UnionRead {
                struct_id,
                target: Box::new(target_h),
                field_name: name.to_owned(),
                storage_ty,
                field_ty: field_ty.clone(),
            },
            ty: field_ty,
            span,
        };
    }
    let Some(idx) = def.fields.iter().position(|f| f.name == name) else {
        return ctx.no_field(struct_id, name, span);
    };
    ctx.check_field_visible(struct_id, name);
    let field_ty = def.fields[idx].ty.clone();
    HirExpr {
        kind: HirExprKind::FieldRead {
            target: Box::new(target_h),
            struct_id,
            field_index: u32::try_from(idx).expect("field index overflow"),
            field_ty: field_ty.clone(),
        },
        ty: field_ty,
        span,
    }
}

/// The instances of generic struct `base` made so far.
pub(crate) fn struct_instantiations(ctx: &LowerCtx<'_>, base: &str) -> Vec<StructId> {
    let ids = LowerCtx::instantiations(&ctx.env.defs.struct_table.borrow(), base);
    crate::lower::incr::log(
        || format!("is|{base}"),
        || crate::lower::incr::Lookup::StructInsts(base.to_owned(), ids.clone()),
    );
    ids
}
