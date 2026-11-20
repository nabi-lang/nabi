//! Enum variant paths and generic enum instances.

pub(crate) use crate::enum_base_name;

#[allow(clippy::wildcard_imports)]
use crate::lower::expr::*;

/// Resolve an enum variant path. A generic enum's instance comes from
/// `hint`, else from the one instance whose payload fits `arg_tys`.
pub(crate) fn resolve_enum_variant(
    ctx: &LowerCtx<'_>,
    segs: &[&str],
    hint: Option<&HirTy>,
    arg_tys: Option<&[HirTy]>,
) -> EnumRes {
    let Some((full, vname)) = ctx.enum_path(segs) else {
        return match (segs, hint) {
            ([name], Some(t)) => match target_variant(ctx, name, t) {
                Some((eid, i)) => EnumRes::Found(eid, i),
                None => EnumRes::NotEnum,
            },
            _ => EnumRes::NotEnum,
        };
    };
    let variant_of = |eid: EnumId| {
        ctx.enum_def(eid)
            .variants
            .iter()
            .position(|v| v.name == vname)
    };
    let of_full = |e: &EnumId| enum_base_name(&ctx.enum_def(*e).name) == full;
    // The expected type, or the one member of an expected sum
    // (`-> Result<T, E> | none`) that is an instance of this enum.
    let hinted: Option<EnumId> = match hint {
        Some(HirTy::Enum(e)) if of_full(e) => Some(*e),
        Some(HirTy::Sum(parts)) => {
            let mut hits = parts.iter().filter_map(|p| match p {
                HirTy::Enum(e) if of_full(e) => Some(*e),
                _ => None,
            });
            hits.next().filter(|_| hits.next().is_none())
        }
        _ => None,
    };
    let eid = if let Some(h) = hinted {
        h
    } else if let Some(eid) = ctx.lookup_enum(&full) {
        eid
    } else {
        let Some(def) = ctx.env.defs.generic_enum(&full) else {
            return EnumRes::NotEnum;
        };
        match infer_enum_instance(ctx, def, &full, &vname, arg_tys, segs) {
            Some(eid) => eid,
            None => return EnumRes::Error,
        }
    };
    if let Some(i) = variant_of(eid) {
        EnumRes::Found(eid, i)
    } else if pick_method(ctx, &HirTy::Enum(eid), &vname, None).is_ok_and(|p| p.is_some()) {
        // `Mode::default()`: an associated function, not a variant.
        EnumRes::NotEnum
    } else {
        ctx.emit_error(
            Code::UNRESOLVED_VARIANT,
            format!(
                "enum `{}` has no variant `{vname}`",
                ctx.env.names.visible_path(&full)
            ),
        );
        EnumRes::Error
    }
}

/// A bare name that ordinary lookup missed, as a variant of the expected
/// type: an enum with a variant `name`, or the one enum member of a sum
/// that has one.
pub(crate) fn target_variant(
    ctx: &LowerCtx<'_>,
    name: &str,
    want: &HirTy,
) -> Option<(EnumId, usize)> {
    let variant = |t: &HirTy| match t {
        HirTy::Enum(e) => (ctx.enum_def(*e).variants.iter())
            .position(|v| v.name == name)
            .map(|i| (*e, i)),
        _ => None,
    };
    match want {
        HirTy::Sum(parts) => {
            let mut hits = parts.iter().filter_map(variant);
            hits.next().filter(|_| hits.next().is_none())
        }
        t => variant(t),
    }
}

/// The instance of generic enum `def` built by `vname(arg_tys)`: each type
/// argument from the payload (`Pair::Two(1, "a")`: `Pair<i32, str>`), else
/// its default, else a `let` inference variable; `None` (reported) if one
/// stays open.
pub(crate) fn infer_enum_instance(
    ctx: &LowerCtx<'_>,
    def: &nabic_ast::EnumDef,
    full: &str,
    vname: &str,
    arg_tys: Option<&[HirTy]>,
    segs: &[&str],
) -> Option<EnumId> {
    let names: nabic_core::HashSet<String> = def
        .generics
        .params
        .iter()
        .map(|gp| match gp {
            nabic_ast::GenericParam::Type { name, .. }
            | nabic_ast::GenericParam::Const { name, .. } => name.clone(),
        })
        .collect();
    let Some(variant) = def.variants.iter().find(|v| v.name == vname) else {
        ctx.emit_error(
            Code::UNRESOLVED_VARIANT,
            format!(
                "enum `{}` has no variant `{vname}`",
                ctx.env.names.visible_path(full)
            ),
        );
        return None;
    };
    let payload: Vec<&Ty> = match &variant.kind {
        nabic_ast::EnumVariantKind::Tuple(ts) => ts.iter().collect(),
        nabic_ast::EnumVariantKind::Named(fs) => fs.iter().map(|f| &f.ty).collect(),
        nabic_ast::EnumVariantKind::Unit | nabic_ast::EnumVariantKind::Value(_) => Vec::new(),
    };
    let mut subst: HashMap<String, HirTy> = HashMap::default();
    for (t, a) in payload.iter().zip(arg_tys.unwrap_or_default()) {
        if *a != HirTy::Unknown {
            unify_param_ty_with(t, a, &names, &mut subst, ctx);
        }
    }
    let mut args = Vec::new();
    for gp in &def.generics.params {
        let arg = match gp {
            nabic_ast::GenericParam::Type { name, default, .. } => {
                if let (Some(t), Some(d)) = (subst.get(name), default) {
                    let d = ctx.resolve_ast_ty(d);
                    crate::lower::infer::adopt_default(ctx, t, &d, &|| {
                        format!("the default of `{}`'s `{name}`", ctx.display_path(full))
                    });
                }
                subst
                    .get(name)
                    .cloned()
                    .filter(|t| *t != HirTy::Unknown)
                    .or_else(|| default.as_ref().map(|d| ctx.resolve_ast_ty(d)))
            }
            nabic_ast::GenericParam::Const { name, ty } => subst
                .get(name)
                .and_then(HirTy::as_const)
                .and_then(|v| crate::lower::mono::typed_const(ctx, name, ty, v)),
        };
        match arg {
            Some(t) => args.push(t),
            None if ctx.infer_vars => args.push(crate::lower::infer::fresh(ctx)),
            None => {
                ctx.emit_uninferred(format!(
                    "cannot infer the type arguments of `{}` for `{}` - annotate the \
                     binding with the concrete type",
                    ctx.display_path(full),
                    segs.join("::")
                ));
                return None;
            }
        }
    }
    ctx.instantiate_enum(full, args, ctx.here())
}

/// `segs` names an enum (or generic enum base) that has no variant of that
/// name, so `E::f(..)` is an associated function call.
pub(crate) fn enum_lacks_variant(ctx: &LowerCtx<'_>, segs: &[&str]) -> bool {
    let Some((full, vname)) = ctx.enum_path(segs) else {
        return false;
    };
    if let Some(e) = ctx.lookup_enum(&full) {
        return ctx.enum_def(e).variants.iter().all(|v| v.name != vname);
    }
    ctx.env
        .defs
        .generic_enum(&full)
        .is_some_and(|d| d.variants.iter().all(|v| v.name != vname))
}

/// Payload slot types of a variant, one per constructor argument.
pub(crate) fn variant_arg_tys(v: &HirEnumVariant) -> Vec<HirTy> {
    match (&v.payload_ty, v.arity) {
        (None, _) => Vec::new(),
        // A named-field variant's payload is always the tuple of fields.
        (Some(HirTy::Tuple(ps)), _) if !v.named_fields.is_empty() => ps.clone(),
        (Some(t), 1) => vec![t.clone()],
        (Some(HirTy::Tuple(ps)), _) => ps.clone(),
        (Some(t), _) => vec![t.clone()],
    }
}
