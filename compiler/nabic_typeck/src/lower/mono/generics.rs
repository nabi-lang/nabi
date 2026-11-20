//! Generic arguments: defaults, arity checks and instance field lists.

#[allow(clippy::wildcard_imports)]
use crate::lower::mono::*;

/// `{T -> arg}` for a generic item's type parameters.
pub(crate) fn generic_subst(
    generics: &nabic_ast::Generics,
    args: &[HirTy],
) -> HashMap<String, HirTy> {
    let mut subst = HashMap::default();
    for (i, gp) in generics.params.iter().enumerate() {
        let (nabic_ast::GenericParam::Type { name, .. }
        | nabic_ast::GenericParam::Const { name, .. }) = gp;
        if let Some(arg) = args.get(i) {
            subst.insert(name.clone(), arg.clone());
        }
    }
    subst
}

/// Every parameter has a default.
pub(crate) fn all_defaulted(generics: &nabic_ast::Generics) -> bool {
    generics.params.iter().all(|p| {
        matches!(
            p,
            nabic_ast::GenericParam::Type {
                default: Some(_),
                ..
            }
        )
    })
}

/// Omitted trailing type arguments take their defaults. Then
/// `args` must fit `generics`: too long is cut, too short padded with
/// `Unknown` (so uses add no errors), and the error comes back.
pub(crate) fn fit_args(
    name: &str,
    generics: &nabic_ast::Generics,
    args: &mut Vec<HirTy>,
    res: impl Fn(&HashMap<String, HirTy>, &Ty) -> HirTy,
) -> Option<(String, String)> {
    let given = args.len();
    while let Some(nabic_ast::GenericParam::Type {
        default: Some(d), ..
    }) = generics.params.get(args.len())
    {
        let t = res(&generic_subst(generics, args), d);
        args.push(t);
    }
    if args.len() == generics.params.len() {
        return None;
    }
    let e = misfit(name, generics, given);
    args.resize(generics.params.len(), HirTy::Unknown);
    Some(e)
}

/// The error (message, help) for `given` arguments to `name<generics>`,
/// a count that does not fit.
pub(crate) fn misfit(name: &str, generics: &nabic_ast::Generics, given: usize) -> (String, String) {
    let max = generics.params.len();
    let min = generics
        .params
        .iter()
        .take_while(|p| {
            !matches!(
                p,
                nabic_ast::GenericParam::Type {
                    default: Some(_),
                    ..
                }
            )
        })
        .count();
    let fix = match given.checked_sub(max) {
        Some(1) => "remove the last argument".to_owned(),
        Some(n) => format!("remove the last {n} arguments"),
        None => {
            let missing: Vec<String> = generics.params[given..min]
                .iter()
                .map(|p| format!("`{}`", param_name(p)))
                .collect();
            format!("add one for {}", missing.join(", "))
        }
    };
    (
        arity_msg(name, min, max, given),
        format!("`{name}` is declared `{}`; {fix}", declared(name, generics)),
    )
}

/// The error (message, help) for `name<generics>` written with no
/// arguments where some have no default.
pub(crate) fn bare_generic(name: &str, generics: &nabic_ast::Generics) -> (String, String) {
    let example: Vec<&str> = generics
        .params
        .iter()
        .map_while(|p| match p {
            nabic_ast::GenericParam::Type { default: None, .. } => Some("i32"),
            nabic_ast::GenericParam::Type { .. } => None,
            nabic_ast::GenericParam::Const { .. } => Some("1"),
        })
        .collect();
    (
        format!("`{name}` needs generic arguments"),
        format!(
            "`{name}` is declared `{}`; write it with them, e.g. `{name}<{}>`",
            declared(name, generics),
            example.join(", ")
        ),
    )
}

/// "`S` takes 1 generic argument but 2 were given" (`min..=max` taken).
pub(crate) fn arity_msg(name: &str, min: usize, max: usize, given: usize) -> String {
    let takes = match (min, max) {
        (0, 0) => "no generic arguments".to_owned(),
        (1, 1) => "1 generic argument".to_owned(),
        (a, b) if a == b => format!("{b} generic arguments"),
        (0, 1) => "at most 1 generic argument".to_owned(),
        (0, b) => format!("at most {b} generic arguments"),
        (a, b) => format!("{a} to {b} generic arguments"),
    };
    let was = if given == 1 { "was" } else { "were" };
    format!("`{name}` takes {takes} but {given} {was} given")
}

/// `S<T, U = i32>` as declared.
pub(crate) fn declared(name: &str, generics: &nabic_ast::Generics) -> String {
    let params: Vec<String> = generics
        .params
        .iter()
        .map(|p| match p {
            nabic_ast::GenericParam::Type {
                name,
                default: Some(d),
                ..
            } => format!("{name} = {}", nabic_ast::print::print_ty(d)),
            nabic_ast::GenericParam::Type { name, .. } => name.clone(),
            nabic_ast::GenericParam::Const { name, ty } => {
                format!("const {name}: {}", nabic_ast::print::print_ty(ty))
            }
        })
        .collect();
    format!("{name}<{}>", params.join(", "))
}

pub(crate) fn param_name(p: &nabic_ast::GenericParam) -> &str {
    let (nabic_ast::GenericParam::Type { name, .. } | nabic_ast::GenericParam::Const { name, .. }) =
        p;
    name
}

/// Const argument `v` for `name: ty`, range-checked and
/// tagged with the parameter's type.
pub fn typed_const(
    ctx: &LowerCtx<'_>,
    name: &str,
    ty: &Ty,
    (v, tag): (i128, Option<PrimTy>),
) -> Option<HirTy> {
    let p = match ctx.resolve_ast_ty(ty) {
        HirTy::Prim(p) if p.is_int() || matches!(p, PrimTy::Bool | PrimTy::CChar) => p,
        other => {
            ctx.emit_error(
                Code::INVALID_GENERIC_PARAMS,
                format!(
                    "const parameter `{name}` has type `{}`; only integers, `bool`, `char` \
                     and `c_char` may be const parameters",
                    ctx.ty_name(&other)
                ),
            );
            return None;
        }
    };
    let is_char = tag == Some(PrimTy::Char);
    let is_bool = tag == Some(PrimTy::Bool);
    if is_char != (p == PrimTy::Char) || is_bool != (p == PrimTy::Bool) {
        let found = if is_char {
            "a `char`"
        } else if is_bool {
            "a `bool`"
        } else {
            "an integer"
        };
        ctx.emit_error(
            Code::INVALID_CONST_ARGUMENT,
            format!(
                "const argument for `{name}: {}` must be {}, found {found}",
                p.name(),
                nabic_diagnostic::article(p.name())
            ),
        );
        return None;
    }
    let fits = match p {
        PrimTy::Bool => v == 0 || v == 1,
        PrimTy::Char => u32::try_from(v).ok().and_then(char::from_u32).is_some(),
        _ => crate::lower::pattern::int_fits(v, p),
    };
    if !fits {
        ctx.emit_error(
            Code::INVALID_CONST_ARGUMENT,
            format!(
                "const argument `{v}` out of range for `{name}: {}`",
                p.name()
            ),
        );
        return None;
    }
    Some(HirTy::Const(v, Some(p)))
}

/// Field list of a generic struct instantiated with `args`.
/// `res` resolves a field type under the instantiation's substitution.
pub(crate) fn generic_struct_fields(
    def: &AstStructDef,
    res: &dyn Fn(&Ty) -> HirTy,
) -> Vec<HirField> {
    let field = |name: String, ty: &Ty, default: Option<Expr>| HirField {
        name,
        ty: res(ty),
        default,
    };
    match &def.kind {
        AstStructKind::Named(fs) => fs
            .iter()
            .map(|f| {
                let d = f
                    .default
                    .clone()
                    .map(|d| in_module(&module_of(&def.name), d));
                field(f.name.clone(), &f.ty, d)
            })
            .collect(),
        AstStructKind::Tuple(fs) => fs
            .iter()
            .enumerate()
            .map(|(i, f)| field(i.to_string(), &f.ty, None))
            .collect(),
        AstStructKind::Unit => Vec::new(),
    }
}

/// Variants of a generic enum instantiation; `res` resolves a payload
/// type under the instantiation's substitution.
pub(crate) fn generic_enum_variants(
    def: &AstEnumDef,
    res: &dyn Fn(&Ty) -> HirTy,
) -> Vec<HirEnumVariant> {
    def.variants
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let tag = u32::try_from(i).expect("variant index overflow");
            let (payload_ty, named_fields, arity) = match &v.kind {
                nabic_ast::EnumVariantKind::Unit | nabic_ast::EnumVariantKind::Value(_) => {
                    (None, Vec::new(), 0)
                }
                nabic_ast::EnumVariantKind::Tuple(tys) if tys.len() == 1 => {
                    (Some(res(&tys[0])), Vec::new(), 1)
                }
                nabic_ast::EnumVariantKind::Tuple(tys) => (
                    Some(HirTy::Tuple(tys.iter().map(res).collect())),
                    Vec::new(),
                    tys.len(),
                ),
                nabic_ast::EnumVariantKind::Named(fields) => (
                    Some(HirTy::Tuple(fields.iter().map(|f| res(&f.ty)).collect())),
                    fields.iter().map(|f| f.name.clone()).collect(),
                    fields.len(),
                ),
            };
            HirEnumVariant {
                name: v.name.clone(),
                tag: tag.into(),
                payload_ty,
                named_fields,
                arity,
            }
        })
        .collect()
}
