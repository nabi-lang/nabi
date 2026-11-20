//! The types a program asks about (`nabi.type_desc`), and those they are
//! made of: one `TypeDesc` each, for codegen to lay out. Built while
//! bodies lower; a body that asks makes no incremental unit, since its
//! answer is an index into this build's table.

use crate::lower::LowerCtx;
use crate::lower::reflect::{INSPECT, attrs, type_name};
use crate::{HirTy, StructId};
use nabic_core::HashMap;
use nabic_ir_common::typedesc::{FieldDesc, TypeDesc, VariantDesc};
use nabic_ir_common::{FunctionId, PrimTy};
use std::cell::{Cell, RefCell};

/// `nabir_abi::typedesc::kind`.
mod kind {
    pub(crate) const STRUCT: u16 = 1;
    pub(crate) const TUPLE_STRUCT: u16 = 2;
    pub(crate) const UNIT: u16 = 3;
    pub(crate) const ENUM: u16 = 4;
    pub(crate) const UNION: u16 = 5;
    pub(crate) const SUM: u16 = 6;
    pub(crate) const PRIM: u16 = 7;
    pub(crate) const STR: u16 = 8;
    pub(crate) const LIST: u16 = 9;
    pub(crate) const DICT: u16 = 10;
    pub(crate) const ARRAY: u16 = 11;
    pub(crate) const TUPLE: u16 = 12;
    pub(crate) const FN: u16 = 13;
    pub(crate) const ANY: u16 = 14;
    pub(crate) const OPAQUE: u16 = 15;
}

const INSPECT_FLAG: u32 = 1;
const LAYOUT_ONLY: u32 = 2;
const INSTANCE: u32 = 4;
const HAS_DEFAULT: u32 = 8;
const FIELD_PUB: u32 = 1;
const FIELD_DEFAULT: u32 = 2;
const FIELD_SKIP: u32 = 4;

const LIST: &str = "core::collections::list::List";
const DICT: &str = "core::collections::dict::Dict";

#[derive(Default)]
struct Table {
    descs: Vec<TypeDesc<HirTy>>,
    by_key: HashMap<String, u32>,
}

thread_local! {
    static TABLE: RefCell<Table> = RefCell::new(Table::default());
    static USES: Cell<u32> = const { Cell::new(0) };
    /// `--hot`: the hot packages, whose types are described in full.
    static HOT: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// The hot packages of this build (`--hot`; empty: none).
pub(crate) fn set_hot(packages: Vec<String>) {
    HOT.with(|h| *h.borrow_mut() = packages);
}

/// Item `path` belongs to a hot package.
pub(crate) fn is_hot(path: &str) -> bool {
    HOT.with(|h| {
        h.borrow().iter().any(|p| {
            path.strip_prefix(p.as_str())
                .is_some_and(|r| r.starts_with("::"))
        })
    })
}

/// How many descriptor requests this thread has lowered (an incremental
/// unit that saw one is not kept).
pub(crate) fn uses() -> u32 {
    USES.with(Cell::get)
}

/// The table built so far, emptied.
pub(crate) fn take() -> Vec<TypeDesc<HirTy>> {
    TABLE.with(|t| std::mem::take(&mut *t.borrow_mut()).descs)
}

/// Forget an earlier lowering's table.
pub(crate) fn reset() {
    TABLE.with(|t| *t.borrow_mut() = Table::default());
}

/// The entry of `ty`, made with the entries it needs.
pub(crate) fn describe(ctx: &mut LowerCtx<'_>, ty: &HirTy, span: nabic_diagnostic::Span) -> u32 {
    USES.with(|u| u.set(u.get() + 1));
    let key = key_of(ctx, ty);
    if let Some(i) = TABLE.with(|t| t.borrow().by_key.get(&key).copied()) {
        return i;
    }
    let symbol = format!("nabi.td.{:016x}", nabic_core::hash::fnv(key.as_bytes()));
    let i = TABLE.with(|t| {
        let mut t = t.borrow_mut();
        let i = u32::try_from(t.descs.len()).expect("descriptor count");
        t.descs.push(TypeDesc {
            symbol,
            ty: ty.clone(),
            kind: kind::OPAQUE,
            flags: 0,
            key: key.clone(),
            name: String::new(),
            path: String::new(),
            tag: None,
            fields: Vec::new(),
            variants: Vec::new(),
            generics: Vec::new(),
            attrs: Vec::new(),
            ops: [None; 4],
            list: None,
            dict: None,
        });
        t.by_key.insert(key, i);
        i
    });
    let d = fill(ctx, ty, span);
    TABLE.with(|t| {
        let e = &mut t.borrow_mut().descs[i as usize];
        let symbol = std::mem::take(&mut e.symbol);
        let key = std::mem::take(&mut e.key);
        *e = TypeDesc { symbol, key, ..d };
    });
    i
}

/// The base path and type arguments of a struct or enum.
fn named(ctx: &LowerCtx<'_>, ty: &HirTy) -> Option<(String, Vec<HirTy>)> {
    let (name, args) = match ty {
        HirTy::Struct(id) => {
            let d = ctx.struct_def(*id);
            (d.name, d.generic_args)
        }
        HirTy::Enum(id) => {
            let d = ctx.enum_def(*id);
            (d.name, d.generic_args)
        }
        _ => return None,
    };
    let base = name.split("__").next().unwrap_or(&name).to_owned();
    Some((base, args))
}

/// `ty`'s key, each named type's path in braces (`{game::Player}`): the
/// text of descriptor keys, `any` tag keys and plugin signatures.
pub(crate) fn key_of(ctx: &LowerCtx<'_>, ty: &HirTy) -> String {
    let list = |ctx: &LowerCtx<'_>, ts: &[HirTy], sep: &str| {
        ts.iter()
            .map(|t| key_of(ctx, t))
            .collect::<Vec<_>>()
            .join(sep)
    };
    if let Some((base, args)) = named(ctx, ty) {
        let base = nabic_core::local::unmangle(&base).into_owned();
        let args: Vec<HirTy> = args
            .into_iter()
            .filter(|a| !matches!(a, HirTy::Const(..)))
            .collect();
        return if args.is_empty() {
            format!("{{{base}}}")
        } else {
            format!("{{{base}}}<{}>", list(ctx, &args, ","))
        };
    }
    match ty {
        HirTy::Tuple(ps) => format!("({})", list(ctx, ps, ",")),
        HirTy::Sum(ms) => list(ctx, ms, "|"),
        HirTy::Array(t, n) => format!("[{};{n}]", key_of(ctx, t)),
        HirTy::Fn(ps, r, _) => format!("fn({})->{}", list(ctx, ps, ","), key_of(ctx, r)),
        HirTy::AnyTrait(b) if b.is_empty() => "any".to_owned(),
        t => ctx.ty_path(t),
    }
}

/// Instance `path<args>` of a library generic function.
fn instance(
    ctx: &mut LowerCtx<'_>,
    path: &str,
    args: &[(&str, &HirTy)],
    span: nabic_diagnostic::Span,
) -> Option<FunctionId> {
    let def = ctx.env.generic_fn(path)?;
    let saved = std::mem::take(&mut ctx.explicit_generics);
    ctx.explicit_generics = args
        .iter()
        .map(|(n, t)| ((*n).to_owned(), (*t).clone()))
        .collect();
    let id = crate::lower::mono::monomorphize_on(ctx, def, None, None, &[], span).map(|m| m.0);
    ctx.explicit_generics = saved;
    id
}

fn field_desc(
    ctx: &mut LowerCtx<'_>,
    name: &str,
    index: usize,
    ty: &HirTy,
    flags: u32,
    found: Option<&attrs::FieldAttrs>,
    span: nabic_diagnostic::Span,
) -> FieldDesc {
    let ty = describe(ctx, ty, span);
    let (renamed_from, list) = found.map_or_else(Default::default, |f| {
        (f.renamed_from.clone().unwrap_or_default(), f.attrs.clone())
    });
    FieldDesc {
        name: name.to_owned(),
        renamed_from,
        ty,
        flags,
        index: u32::try_from(index).expect("field index"),
        offset: u64::MAX,
        attrs: attrs::resolve(ctx, &list, span),
        init_default: None,
    }
}

/// The entry of `ty` (its `symbol` and `key` are the caller's).
#[allow(clippy::too_many_lines)]
fn fill(ctx: &mut LowerCtx<'_>, ty: &HirTy, span: nabic_diagnostic::Span) -> TypeDesc<HirTy> {
    let name = type_name(ctx, ty);
    let mut d = TypeDesc {
        symbol: String::new(),
        ty: ty.clone(),
        kind: kind::OPAQUE,
        flags: 0,
        key: String::new(),
        name,
        path: String::new(),
        tag: (!matches!(ty, HirTy::Unit | HirTy::Never | HirTy::Unknown)).then(|| ctx.any_tag(ty)),
        fields: Vec::new(),
        variants: Vec::new(),
        generics: Vec::new(),
        attrs: Vec::new(),
        ops: [None; 4],
        list: None,
        dict: None,
    };
    let inspect = matches!(ty, HirTy::Struct(_) | HirTy::Enum(_)) && ctx.has_trait(ty, INSPECT);
    if let Some((base, args)) = named(ctx, ty) {
        d.path = nabic_core::local::unmangle(&base).into_owned();
        // A hot package's types carry their layout: fields, variants,
        // defaults (hot reload migrates their values).
        let hot = !inspect && is_hot(&d.path) && base != LIST && base != DICT;
        if hot {
            d.flags |= LAYOUT_ONLY;
        }
        let full = inspect || hot;
        let args: Vec<HirTy> = args
            .into_iter()
            .filter(|a| !matches!(a, HirTy::Const(..)))
            .collect();
        if !args.is_empty() {
            d.flags |= INSTANCE;
        }
        for a in &args {
            let g = describe(ctx, a, span);
            d.generics.push(g);
        }
        if base == LIST && args.len() == 1 {
            d.kind = kind::LIST;
            let t = &args[0];
            let f = |ctx: &mut LowerCtx<'_>, n: &str| {
                instance(ctx, &format!("core::reflect::ops::{n}"), &[("T", t)], span)
            };
            d.list = Some([
                f(ctx, "list_len"),
                f(ctx, "list_get"),
                f(ctx, "list_items"),
                f(ctx, "list_from"),
            ]);
            return d;
        }
        if base == DICT && args.len() >= 2 {
            d.kind = kind::DICT;
            let (k, v) = (&args[0], &args[1]);
            let f = |ctx: &mut LowerCtx<'_>, n: &str| {
                instance(
                    ctx,
                    &format!("core::reflect::ops::{n}"),
                    &[("K", k), ("V", v)],
                    span,
                )
            };
            d.dict = Some([
                f(ctx, "dict_len"),
                f(ctx, "dict_items"),
                f(ctx, "dict_from"),
            ]);
            d.generics.truncate(2);
            return d;
        }
        let found = attrs::of(&base);
        if let HirTy::Struct(id) = ty {
            d.kind = struct_kind(ctx, *id);
            if full && d.kind != kind::UNION {
                struct_fields(ctx, &mut d, *id, &base, found.as_ref(), span);
            }
        } else if let HirTy::Enum(id) = ty {
            d.kind = kind::ENUM;
            if full {
                let def = ctx.enum_def(*id);
                for (vi, v) in def.variants.iter().enumerate() {
                    let n = v.arity.max(v.named_fields.len());
                    let parts: Vec<HirTy> = match (&v.payload_ty, n) {
                        (_, 0) | (None, _) => Vec::new(),
                        (Some(HirTy::Tuple(ps)), n) if ps.len() == n => ps.clone(),
                        (Some(t), _) => vec![t.clone()],
                    };
                    let va = found.as_ref().and_then(|a| a.variants.get(vi));
                    let mut fields = Vec::new();
                    for (i, t) in parts.iter().enumerate() {
                        let fname = v
                            .named_fields
                            .get(i)
                            .cloned()
                            .unwrap_or_else(|| i.to_string());
                        let fa = va.and_then(|a| a.fields.get(i));
                        fields.push(field_desc(ctx, &fname, i, t, FIELD_PUB, fa, span));
                    }
                    let vattrs = va.map(|a| a.attrs.clone()).unwrap_or_default();
                    let mut vd = VariantDesc {
                        name: v.name.clone(),
                        tag: u32::try_from(v.tag & 0xffff_ffff).unwrap_or(0),
                        fields,
                        attrs: Vec::new(),
                    };
                    vd.attrs = attrs::resolve(ctx, &vattrs, span);
                    d.variants.push(vd);
                }
            }
        }
        if inspect {
            d.flags |= INSPECT_FLAG;
            if let Some(a) = &found {
                d.attrs = attrs::resolve(ctx, &a.attrs, span);
            }
            let f = |ctx: &mut LowerCtx<'_>, n: &str| {
                instance(ctx, &format!("core::reflect::ops::{n}"), &[("T", ty)], span)
            };
            d.ops = [f(ctx, "get"), f(ctx, "set"), f(ctx, "build"), None];
        }
        return d;
    }
    match ty {
        HirTy::Prim(PrimTy::Str) => d.kind = kind::STR,
        HirTy::Prim(_) => d.kind = kind::PRIM,
        HirTy::Sum(ms) => {
            d.kind = kind::SUM;
            for m in ms {
                let g = describe(ctx, m, span);
                d.generics.push(g);
            }
        }
        HirTy::Tuple(ps) => {
            d.kind = kind::TUPLE;
            for p in ps {
                let g = describe(ctx, p, span);
                d.generics.push(g);
            }
        }
        HirTy::Array(t, _) => {
            d.kind = kind::ARRAY;
            let g = describe(ctx, t, span);
            d.generics.push(g);
        }
        HirTy::Fn(..) | HirTy::Closure { .. } => d.kind = kind::FN,
        HirTy::AnyTrait(_) => d.kind = kind::ANY,
        HirTy::None | HirTy::Unit => d.kind = kind::UNIT,
        _ => {}
    }
    d
}

/// The fields of struct `id` (`base`: its path) into `d`; a hot type's
/// skipped fields too, and the functions giving its defaults.
fn struct_fields(
    ctx: &mut LowerCtx<'_>,
    d: &mut TypeDesc<HirTy>,
    id: StructId,
    base: &str,
    found: Option<&attrs::TypeAttrs>,
    span: nabic_diagnostic::Span,
) {
    let def = ctx.struct_def(id);
    let vis = ctx.env.fields_vis(base).cloned();
    let hot = is_hot(&d.path);
    let mut defaults = 0;
    for (i, f) in def.fields.iter().enumerate() {
        let fa = found.and_then(|a| a.fields.get(i));
        let mut flags = 0;
        if fa.is_some_and(|a| a.skip) {
            if !hot {
                continue;
            }
            flags |= FIELD_SKIP;
        }
        if vis.as_ref().and_then(|v| v.get(&f.name)) == Some(&nabic_core::Visibility::Pub) {
            flags |= FIELD_PUB;
        }
        if f.default.is_some() {
            flags |= FIELD_DEFAULT;
            defaults += 1;
        }
        let mut fd = field_desc(ctx, &f.name, i, &f.ty, flags, fa, span);
        if let (true, Some(e)) = (hot, &f.default) {
            fd.init_default = Some(crate::lower::hot::default_fn(
                ctx, &def.name, &f.name, &f.ty, e,
            ));
        }
        d.fields.push(fd);
    }
    if hot && defaults == def.fields.len() {
        d.flags |= HAS_DEFAULT;
    }
}

fn struct_kind(ctx: &LowerCtx<'_>, id: StructId) -> u16 {
    let def = ctx.struct_def(id);
    if def.union_kind.is_some() {
        kind::UNION
    } else if def.fields.is_empty() {
        kind::UNIT
    } else if def.fields[0].name == "0" {
        kind::TUPLE_STRUCT
    } else {
        kind::STRUCT
    }
}
