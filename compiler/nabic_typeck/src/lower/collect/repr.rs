//! Struct, enum and union definitions and their layout attributes.

#[allow(clippy::wildcard_imports)]
use crate::lower::collect::*;

pub(crate) fn lower_enum_def(
    e: &AstEnumDef,
    id: EnumId,
    defs: &TypeDefs<'_>,
    sc: Scope<'_>,
) -> HirEnumDef {
    let variants = e
        .variants
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let tag = i128::try_from(i).expect("variant index overflow");
            match &v.kind {
                // A value variant's tag is filled in once consts are known
                // (`assign_discriminants`).
                AstEnumVariantKind::Unit | AstEnumVariantKind::Value(_) => HirEnumVariant {
                    name: v.name.clone(),
                    tag,
                    payload_ty: None,
                    named_fields: Vec::new(),
                    arity: 0,
                },
                // Single-element tuple: store the payload type directly.
                AstEnumVariantKind::Tuple(tys) if tys.len() == 1 => HirEnumVariant {
                    name: v.name.clone(),
                    tag,
                    payload_ty: Some(resolve_ty_with_structs(&tys[0], defs, None, sc)),
                    named_fields: Vec::new(),
                    arity: 1,
                },
                // Multi-element tuple: pack into a tuple payload type.
                AstEnumVariantKind::Tuple(tys) => HirEnumVariant {
                    name: v.name.clone(),
                    tag,
                    payload_ty: Some(HirTy::Tuple(
                        tys.iter()
                            .map(|t| resolve_ty_with_structs(t, defs, None, sc))
                            .collect(),
                    )),
                    named_fields: Vec::new(),
                    arity: tys.len(),
                },
                // `Move { x: i32; y: i32; }`: a tuple payload in
                // declaration order, with field names for init / patterns.
                AstEnumVariantKind::Named(fields) => HirEnumVariant {
                    name: v.name.clone(),
                    tag,
                    payload_ty: Some(HirTy::Tuple(
                        fields
                            .iter()
                            .map(|f| resolve_ty_with_structs(&f.ty, defs, None, sc))
                            .collect(),
                    )),
                    named_fields: fields.iter().map(|f| f.name.clone()).collect(),
                    arity: fields.len(),
                },
            }
        })
        .collect();
    HirEnumDef {
        id,
        repr: repr_of(&e.attrs).unwrap_or_default(),
        def_id: None,
        name: e.name.clone(),
        variants,
        span: e.span,
        generic_args: Vec::new(),
    }
}

/// `#[niche(..)]` arguments: integers, `MIN` / `MAX`, `NAN` /
/// `INFINITY` / `NEG_INFINITY`, and ranges. Empty without the attribute;
/// `Err` names the first unreadable argument.
pub(crate) fn niche_args(attrs: &[nabic_ast::Attribute]) -> Result<Vec<NicheArg>, String> {
    let Some(attr) = attrs.iter().find(|a| a.name() == "niche") else {
        return Ok(Vec::new());
    };
    let AttrBody::Args(args) = &attr.body else {
        return Err("`#[niche]` takes arguments: `#[niche(0)]`".to_owned());
    };
    let val = |a: &AttrArg| -> Option<NicheVal> {
        let name = match a {
            AttrArg::Lit(Literal::Int(text)) => {
                let (body, neg) = text
                    .strip_prefix('-')
                    .map_or((text.as_str(), false), |b| (b, true));
                let (v, _) = crate::lower::expr::parse_int_literal(body)?;
                return Some(NicheVal::Int(if neg { -v } else { v }));
            }
            AttrArg::Word(w) => w.as_str(),
            AttrArg::Nested { path, body } if matches!(**body, AttrBody::Empty) => path.last()?,
            _ => return None,
        };
        Some(match name {
            "NAN" => NicheVal::Nan,
            "INFINITY" => NicheVal::Inf,
            "NEG_INFINITY" => NicheVal::NegInf,
            "MIN" => NicheVal::Min,
            "MAX" => NicheVal::Max,
            _ => {
                let (ty, end) = name.rsplit_once('_')?;
                let bits: u32 = match ty {
                    "ISIZE" | "USIZE" => 64,
                    _ => ty.get(1..)?.parse().ok()?,
                };
                let signed = match ty.as_bytes().first()? {
                    b'I' => true,
                    b'U' => false,
                    _ => return None,
                };
                if !matches!(bits, 8 | 16 | 32 | 64) {
                    return None;
                }
                NicheVal::Int(match (signed, end) {
                    (true, "MIN") => -(1i128 << (bits - 1)),
                    (true, "MAX") => (1i128 << (bits - 1)) - 1,
                    (false, "MIN") => 0,
                    (false, "MAX") => (1i128 << bits) - 1,
                    _ => return None,
                })
            }
        })
    };
    args.iter()
        .enumerate()
        .map(|(i, a)| {
            let bad = || {
                format!(
                    "`#[niche]` argument {}: expected an integer, `MIN` / `MAX`, `NAN` / \
                     `INFINITY` / `NEG_INFINITY`, or a range of them",
                    i + 1
                )
            };
            match a {
                AttrArg::Range { lo, hi, inclusive } => {
                    let lo = lo
                        .as_deref()
                        .map_or(Some(NicheVal::Min), val)
                        .ok_or_else(bad)?;
                    let hi = match (hi.as_deref().map(val), inclusive) {
                        (None, _) => NicheVal::Max,
                        (Some(Some(v)), true) => v,
                        (Some(Some(NicheVal::Int(v))), false) => NicheVal::Int(v - 1),
                        _ => return Err(bad()),
                    };
                    Ok(NicheArg::Range(lo, hi))
                }
                _ => val(a).map(NicheArg::Val).ok_or_else(bad),
            }
        })
        .collect()
}

/// `#[repr(..)]` arguments, unchecked against the item, and the C type a
/// `#[lang]` struct stands for.
pub(crate) fn repr_of(attrs: &[nabic_ast::Attribute]) -> Result<crate::Repr, String> {
    let mut r = crate::Repr {
        c_kind: c_kind_of(attrs),
        ..crate::Repr::default()
    };
    for attr in attrs.iter().filter(|a| a.name() == "repr") {
        let AttrBody::Args(args) = &attr.body else {
            return Err("`#[repr]` takes arguments: `#[repr(C)]`".to_owned());
        };
        for a in args {
            match a {
                AttrArg::Word(w) => match w.as_str() {
                    "C" => r.c = true,
                    "transparent" => r.transparent = true,
                    "packed" => r.packed = true,
                    w => {
                        r.int = Some(match w {
                            "u8" => PrimTy::U8,
                            "i8" => PrimTy::I8,
                            "u16" => PrimTy::U16,
                            "i16" => PrimTy::I16,
                            "u32" => PrimTy::U32,
                            "i32" => PrimTy::I32,
                            "u64" => PrimTy::U64,
                            "i64" => PrimTy::I64,
                            _ => return Err(format!("unknown `#[repr]` argument `{w}`")),
                        });
                    }
                },
                AttrArg::Nested { path, body } if path.as_slice() == ["align"] => {
                    let n = match &**body {
                        AttrBody::Args(v) => match v.as_slice() {
                            [AttrArg::Lit(Literal::Int(t))] => {
                                crate::lower::expr::parse_int_literal(t)
                                    .and_then(|(v, _)| u32::try_from(v).ok())
                            }
                            _ => None,
                        },
                        _ => None,
                    };
                    match n {
                        Some(n) if n.is_power_of_two() && n <= 1 << 29 => r.align = n,
                        _ => {
                            return Err("`align(N)` takes a power of two up to 2^29".to_owned());
                        }
                    }
                }
                _ => return Err("unknown `#[repr]` argument".to_owned()),
            }
        }
    }
    Ok(r)
}

/// The C type of a `#[lang]` library struct.
fn c_kind_of(attrs: &[nabic_ast::Attribute]) -> crate::CKind {
    let slot = attrs
        .iter()
        .filter(|a| a.name() == "lang")
        .find_map(|a| match &a.body {
            AttrBody::Args(args) => args.iter().find_map(|x| match x {
                AttrArg::Lit(Literal::Str(s)) => Some(s.as_str()),
                _ => None,
            }),
            _ => None,
        });
    match slot {
        Some("f16_type") => crate::CKind::F16,
        Some("bf16_type") => crate::CKind::BF16,
        Some("simd_type") => crate::CKind::Simd,
        Some("mask_type") => crate::CKind::Mask,
        _ => crate::CKind::Plain,
    }
}

/// `#[repr(..)]` arguments and placement.
pub(crate) fn check_repr_attrs(
    program: &nabic_ast::Program,
    diags: &mut Vec<nabic_diagnostic::NabiDiagnostic>,
) {
    for item in &program.items {
        let attrs = match item {
            Item::Struct(s) => &s.attrs,
            Item::Enum(e) => &e.attrs,
            Item::Union(u) => &u.attrs,
            _ => continue,
        };
        let Some(attr) = attrs.iter().find(|a| a.name() == "repr") else {
            continue;
        };
        let msg = match (repr_of(attrs), item) {
            (Err(e), _) => Some(e),
            (Ok(r), Item::Struct(s)) => {
                let n = match &s.kind {
                    AstStructKind::Named(fs) => fs.len(),
                    AstStructKind::Tuple(fs) => fs.len(),
                    AstStructKind::Unit => 0,
                };
                if r.int.is_some() {
                    Some("an integer `#[repr]` goes on an enum, not a struct".to_owned())
                } else if r.packed && r.align != 0 {
                    Some("a struct cannot be both `packed` and `align(N)`".to_owned())
                } else if r.transparent && (r.c || r.packed || r.align != 0) {
                    Some("`#[repr(transparent)]` takes no other argument".to_owned())
                } else if r.transparent && n != 1 {
                    Some(format!(
                        "`#[repr(transparent)]` needs exactly one field, not {n}"
                    ))
                } else {
                    None
                }
            }
            (Ok(r), Item::Enum(e)) => {
                let data = e.variants.iter().any(|v| {
                    !matches!(
                        v.kind,
                        AstEnumVariantKind::Unit | AstEnumVariantKind::Value(_)
                    )
                });
                if r.packed || r.transparent || r.align != 0 {
                    Some("an enum takes only `C` or an integer `#[repr]`".to_owned())
                } else if data
                    && r.int
                        .is_some_and(|p| !matches!(p, PrimTy::I32 | PrimTy::U32))
                {
                    Some(
                        "a data-carrying enum's tag is 32 bits: only `#[repr(i32)]` / \
                         `#[repr(u32)]`"
                            .to_owned(),
                    )
                } else {
                    None
                }
            }
            (Ok(r), _) => (r.packed || r.transparent || r.align != 0 || r.int.is_some())
                .then(|| "a union takes only `#[repr(C)]`".to_owned()),
        };
        if let Some(msg) = msg {
            diags.push(nabic_diagnostic::NabiDiagnostic::error(
                Code::INVALID_REPR,
                attr.span,
                msg,
            ));
        }
    }
}

/// Value variants: all or none; each value a const integer
/// that fits the tag type and is unique.
pub(crate) fn assign_discriminants(env: &Env<'_>, decls: &[(EnumId, &AstEnumDef)]) {
    let no_subst = HashMap::default();
    for &(id, decl) in decls {
        let i = id.0 as usize;
        let values = decl
            .variants
            .iter()
            .filter(|v| matches!(v.kind, AstEnumVariantKind::Value(_)))
            .count();
        if values == 0 {
            continue;
        }
        let mut ctx = LowerCtx::new(env, &no_subst, None, decl.span);
        ctx.current_module = module_of(&decl.name);
        let err = |msg: String| {
            ctx.emit_error(Code::INVALID_ENUM_VALUE, msg);
        };
        if values != decl.variants.len() {
            err(format!(
                "enum `{}` mixes value variants (`V = n`) with other variants; \
                 a value enum has only value variants",
                decl.name
            ));
            ctx.help_last(Some(
                "give every variant a value (`A = 0, B = 5, C = 6`), or none".to_owned(),
            ));
            continue;
        }
        let tag_ty = ctx.enum_def(id).repr.int.unwrap_or(PrimTy::I32);
        let mut seen: HashMap<i128, &str> = HashMap::default();
        let mut tags = Vec::new();
        for v in &decl.variants {
            let AstEnumVariantKind::Value(e) = &v.kind else {
                continue;
            };
            let c = nabic_ast::ConstExpr::Expr(Box::new(e.clone()));
            let n = match ctx.eval_const_msg(&c) {
                Ok(n) => n,
                Err(why) => {
                    err(format!(
                        "the value of `{}::{}` is not a const integer: {why}",
                        decl.name, v.name
                    ));
                    tags.push(None);
                    continue;
                }
            };
            let (lo, hi) = crate::lower::consteval::int_range(tag_ty);
            if !(lo..=hi).contains(&n) {
                err(format!(
                    "the value {n} of `{}::{}` does not fit the tag type `{}`",
                    decl.name,
                    v.name,
                    tag_ty.name()
                ));
                tags.push(None);
                continue;
            }
            if let Some(other) = seen.insert(n, &v.name) {
                err(format!(
                    "`{}::{}` and `{}::{other}` have the same value {n}",
                    decl.name, v.name, decl.name
                ));
            }
            tags.push(Some(n));
        }
        let mut enums = env.defs.enums.borrow_mut();
        for (hv, t) in enums[i].variants.iter_mut().zip(tags) {
            if let Some(n) = t {
                hv.tag = n;
            }
        }
    }
}

/// `#[niche]` placement: a single-field struct only.
pub(crate) fn check_niche_attrs(
    program: &nabic_ast::Program,
    diags: &mut Vec<nabic_diagnostic::NabiDiagnostic>,
) {
    for item in &program.items {
        // `misplaced`: what the attribute sits on, if not one field.
        let (attrs, misplaced) = match item {
            Item::Struct(s) => {
                let n = match &s.kind {
                    AstStructKind::Named(fs) => fs.len(),
                    AstStructKind::Tuple(fs) => fs.len(),
                    AstStructKind::Unit => 0,
                };
                (
                    &s.attrs,
                    (n != 1).then(|| format!("a struct with {n} fields")),
                )
            }
            Item::Enum(e) => (&e.attrs, Some("an enum".to_owned())),
            Item::Union(u) => (&u.attrs, Some("a union".to_owned())),
            _ => continue,
        };
        let Some(attr) = attrs.iter().find(|a| a.name() == "niche") else {
            continue;
        };
        let msg = match misplaced {
            None => niche_args(attrs).err(),
            Some(w) => Some(format!("`#[niche]` goes on a single-field struct, not {w}")),
        };
        if let Some(msg) = msg {
            diags.push(nabic_diagnostic::NabiDiagnostic::error(
                Code::INVALID_REPR,
                attr.span,
                msg,
            ));
        }
    }
}

pub(crate) fn lower_struct_def(
    s: &AstStructDef,
    id: StructId,
    defs: &TypeDefs<'_>,
    sc: Scope<'_>,
) -> HirStructDef {
    let fields = match &s.kind {
        AstStructKind::Named(fs) => fs
            .iter()
            .map(|f| HirField {
                name: f.name.clone(),
                ty: resolve_ty_with_structs(&f.ty, defs, None, sc),
                // A literal omitting the field fills it at the use site.
                default: f.default.clone().map(|d| in_module(&module_of(&s.name), d)),
            })
            .collect(),
        // Tuple struct: fields named "0", "1", ... so `n.0` reads like a
        // named field.
        AstStructKind::Tuple(fs) => fs
            .iter()
            .enumerate()
            .map(|(i, f)| HirField {
                name: i.to_string(),
                ty: resolve_ty_with_structs(&f.ty, defs, None, sc),
                default: None,
            })
            .collect(),
        AstStructKind::Unit => Vec::new(),
    };
    HirStructDef {
        id,
        repr: repr_of(&s.attrs).unwrap_or_default(),
        def_id: None,
        name: s.name.clone(),
        fields,
        span: s.span,
        generic_args: Vec::new(),
        union_kind: None,
        niche: niche_args(&s.attrs).unwrap_or_default(),
    }
}

/// A `union` as one synthetic `HirStructDef` field `__data` of the chosen
/// storage (`union_storage_strategy`); field -> declared type in
/// `union_kind.aliases`.
pub(crate) fn lower_union_def(
    u: &nabic_ast::UnionDef,
    id: StructId,
    defs: &TypeDefs<'_>,
    sc: Scope<'_>,
    pre_diags: &mut Vec<nabic_diagnostic::NabiDiagnostic>,
) -> HirStructDef {
    let res = |t: &nabic_ast::Ty| resolve_ty_with_structs(t, defs, None, sc);
    union_def(u, id, u.name.clone(), &res, Vec::new(), pre_diags)
}

/// `u` as a struct named `name` (a generic one's instance: `res`
/// resolves under its substitution, `generic_args` are its arguments).
pub(crate) fn union_def(
    u: &nabic_ast::UnionDef,
    id: StructId,
    name: String,
    res: &dyn Fn(&nabic_ast::Ty) -> HirTy,
    generic_args: Vec<HirTy>,
    pre_diags: &mut Vec<nabic_diagnostic::NabiDiagnostic>,
) -> HirStructDef {
    let aliases: Vec<(String, HirTy)> = u
        .fields
        .iter()
        .map(|f| (f.name.clone(), res(&f.ty)))
        .collect();
    let is_repr_c = u.attrs.iter().any(|a| {
        a.path.as_slice() == ["repr"]
            && matches!(&a.body, nabic_ast::AttrBody::Args(args)
                if args.iter().any(|arg| matches!(arg, nabic_ast::AttrArg::Word(w) if w == "C")))
    });
    let storage = union_storage_strategy(&aliases, is_repr_c, &u.name, u.span, pre_diags);
    let storage_ty = match &storage {
        UnionStorage::WidestPrim(p) => HirTy::Prim(*p),
        // Every field side by side; MIR overlays them (`union_storage`).
        UnionStorage::ByteArray => HirTy::Tuple(aliases.iter().map(|(_, t)| t.clone()).collect()),
    };
    HirStructDef {
        id,
        repr: repr_of(&u.attrs).unwrap_or_default(),
        def_id: None,
        name,
        fields: vec![HirField {
            name: "__data".to_owned(),
            ty: storage_ty,
            default: None,
        }],
        span: u.span,
        generic_args,
        union_kind: Some(HirUnionKind {
            storage,
            aliases,
            is_repr_c,
        }),
        niche: Vec::new(),
    }
}

/// Union storage: an all-primitive union without `#[repr(C)]` is its widest
/// primitive; any other is `[u8; size]`.
pub(crate) fn union_storage_strategy(
    aliases: &[(String, HirTy)],
    is_repr_c: bool,
    name: &str,
    span: nabic_diagnostic::Span,
    pre_diags: &mut Vec<nabic_diagnostic::NabiDiagnostic>,
) -> UnionStorage {
    if aliases.is_empty() {
        pre_diags.push(nabic_diagnostic::NabiDiagnostic::error(
            Code::INVALID_UNION,
            span,
            format!("union `{name}` declares no fields"),
        ));
        return UnionStorage::WidestPrim(PrimTy::U8);
    }
    let all_prim = aliases
        .iter()
        .all(|(_, t)| matches!(t, HirTy::Prim(p) if *p != PrimTy::Str));
    if all_prim && !is_repr_c {
        // Pick the widest primitive. Tie-break: prefer unsigned > signed
        // > float so written bits round-trip cleanly through a wider read.
        let mut best: Option<PrimTy> = None;
        let mut best_score: (u32, u8) = (0, 0);
        for (_, t) in aliases {
            if let HirTy::Prim(p) = t {
                let width = p.bit_width(64);
                let kind_pref: u8 = match p {
                    PrimTy::U8 | PrimTy::U16 | PrimTy::U32 | PrimTy::U64 | PrimTy::Usize => 3,
                    PrimTy::I8
                    | PrimTy::I16
                    | PrimTy::I32
                    | PrimTy::I64
                    | PrimTy::Isize
                    | PrimTy::CChar
                    | PrimTy::Char => 2,
                    PrimTy::F32 | PrimTy::F64 => 1,
                    PrimTy::Bool | PrimTy::Str => 0,
                };
                let score = (width, kind_pref);
                if best.is_none() || score > best_score {
                    best = Some(*p);
                    best_score = score;
                }
            }
        }
        return UnionStorage::WidestPrim(best.unwrap_or(PrimTy::U8));
    }
    UnionStorage::ByteArray
}
