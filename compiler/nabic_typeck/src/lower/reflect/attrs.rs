//! Runtime attributes: `#[runtime_attribute(name)] struct S` declares
//! `#[name(..)]`, whose arguments are checked like a literal of `S` and
//! kept as constants for `TypeInfo`; and `#[inspect(skip)]` /
//! `#[renamed_from("old")]` on fields.

use crate::lower::LowerCtx;
use crate::lower::env::Env;
use nabic_ast::{
    AttrArg, AttrBody, Attribute, EnumVariantKind, Item, Literal, Program, StructKind,
};
use nabic_core::HashMap;
use nabic_diagnostic::{Code, NabiDiagnostic, Span};
use nabic_ir_common::typedesc::AttrDesc;
use nabic_ir_common::{ConstVal, PrimTy};
use std::cell::RefCell;

/// One `#[name(..)]` use, checked: the attribute struct and its fields'
/// values in declaration order.
#[derive(Clone, Debug)]
pub(crate) struct Use {
    pub(crate) decl: String,
    pub(crate) values: Vec<ConstVal>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct FieldAttrs {
    pub(crate) skip: bool,
    pub(crate) renamed_from: Option<String>,
    pub(crate) attrs: Vec<Use>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct VariantAttrs {
    pub(crate) attrs: Vec<Use>,
    pub(crate) fields: Vec<FieldAttrs>,
}

/// What a struct or enum carries, by field and variant position.
#[derive(Clone, Debug, Default)]
pub(crate) struct TypeAttrs {
    pub(crate) attrs: Vec<Use>,
    pub(crate) fields: Vec<FieldAttrs>,
    pub(crate) variants: Vec<VariantAttrs>,
}

/// A declared runtime attribute.
struct Decl {
    path: String,
    fields: Vec<(String, PrimTy)>,
}

thread_local! {
    static TYPES: RefCell<HashMap<String, TypeAttrs>> = RefCell::new(HashMap::default());
}

/// The attributes of the struct or enum at `path`.
pub(crate) fn of(path: &str) -> Option<TypeAttrs> {
    TYPES.with(|t| t.borrow().get(path).cloned())
}

/// The descriptors of `uses`.
pub(crate) fn resolve(ctx: &mut LowerCtx<'_>, uses: &[Use], span: Span) -> Vec<AttrDesc> {
    let mut out = Vec::new();
    for u in uses {
        let Some(id) = ctx.lookup_struct(&u.decl) else {
            continue;
        };
        let ty = crate::lower::reflect::desc::describe(ctx, &crate::HirTy::Struct(id), span);
        out.push(AttrDesc {
            ty,
            values: u.values.clone(),
        });
    }
    out
}

fn prim_of(ty: &nabic_ast::Ty) -> Option<PrimTy> {
    let nabic_ast::TyKind::Path(p) = &ty.kind else {
        return None;
    };
    let [seg] = p.segments.as_slice() else {
        return None;
    };
    if !seg.generics.is_empty() {
        return None;
    }
    Some(match seg.name.as_str() {
        "i8" => PrimTy::I8,
        "i16" => PrimTy::I16,
        "i32" => PrimTy::I32,
        "i64" => PrimTy::I64,
        "isize" => PrimTy::Isize,
        "u8" => PrimTy::U8,
        "u16" => PrimTy::U16,
        "u32" => PrimTy::U32,
        "u64" => PrimTy::U64,
        "usize" => PrimTy::Usize,
        "f32" => PrimTy::F32,
        "f64" => PrimTy::F64,
        "bool" => PrimTy::Bool,
        "char" => PrimTy::Char,
        "str" => PrimTy::Str,
        _ => return None,
    })
}

fn ty_text(ty: &nabic_ast::Ty) -> String {
    crate::lower::collect::impl_def_display(ty)
}

/// `lit` as a value of `p`, or what to write instead.
fn value(lit: &Literal, p: PrimTy) -> Result<ConstVal, String> {
    let int = |t: &str| {
        let t: String = t.chars().filter(|c| *c != '_').collect();
        let t = t.trim_end_matches(|c: char| c.is_ascii_alphabetic() && c != 'x');
        t.parse::<i128>().ok()
    };
    match (lit, p) {
        (Literal::Int(t), p) if p.is_int() => int(t)
            .map(|value| ConstVal::Int { value, ty: p })
            .ok_or_else(|| "an integer".to_owned()),
        (Literal::Int(t), PrimTy::F32 | PrimTy::F64) => int(t)
            .map(|v| ConstVal::Float {
                bits: (v as f64).to_bits(),
                ty: PrimTy::F64,
            })
            .ok_or_else(|| "a number".to_owned()),
        (Literal::Float(t), PrimTy::F32 | PrimTy::F64) => {
            let t: String = t.chars().filter(|c| *c != '_').collect();
            t.trim_end_matches(|c: char| c.is_ascii_alphabetic())
                .parse::<f64>()
                .map(|v| ConstVal::Float {
                    bits: v.to_bits(),
                    ty: PrimTy::F64,
                })
                .map_err(|_| "a number".to_owned())
        }
        (Literal::Bool(b), PrimTy::Bool) => Ok(ConstVal::Bool(*b)),
        (Literal::Str(s) | Literal::RawStr(s), PrimTy::Str) => Ok(ConstVal::Str(s.clone())),
        (Literal::Char(c), PrimTy::Char) => c
            .chars()
            .next()
            .map(|c| ConstVal::Char(c as u32))
            .ok_or_else(|| "a character".to_owned()),
        (_, PrimTy::F32 | PrimTy::F64) => Err("a number".to_owned()),
        (_, p) if p.is_int() => Err("an integer".to_owned()),
        (_, PrimTy::Bool) => Err("`true` or `false`".to_owned()),
        (_, PrimTy::Str) => Err("a string".to_owned()),
        (_, PrimTy::Char) => Err("a character".to_owned()),
        (_, p) => Err(format!("a `{}`", p.name())),
    }
}

/// `field: type` of every field of `d`, for messages.
fn shape(d: &Decl) -> String {
    d.fields
        .iter()
        .map(|(n, p)| format!("{n}: {}", p.name()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// An example of `#[name(..)]` for `d`.
fn example(name: &str, d: &Decl) -> String {
    let args: Vec<&str> = d
        .fields
        .iter()
        .enumerate()
        .map(|(i, (_, p))| match p {
            PrimTy::Str => "\"..\"",
            PrimTy::Bool => "true",
            PrimTy::Char => "'a'",
            _ if i == 0 => "0",
            _ => "10",
        })
        .collect();
    format!("#[{name}({})]", args.join(", "))
}

struct Cx<'e> {
    decls: HashMap<String, Vec<Decl>>,
    diags: &'e mut Vec<NabiDiagnostic>,
}

impl Cx<'_> {
    fn err(&mut self, at: Span, msg: String, help: Option<String>) {
        let mut d = NabiDiagnostic::error(Code::INVALID_ATTRIBUTE, at, msg);
        if let Some(h) = help {
            d = d.with_help(h);
        }
        self.diags.push(d);
    }

    /// The runtime attributes among `attrs`, checked.
    fn uses(&mut self, owner: &str, attrs: &[Attribute]) -> Vec<Use> {
        let mut out = Vec::new();
        for a in attrs {
            let name = a.name();
            let Some(cands) = self.decls.get(name) else {
                continue;
            };
            let pkg = owner.split("::").next().unwrap_or_default();
            let own: Vec<usize> = (0..cands.len())
                .filter(|&i| cands[i].path.split("::").next() == Some(pkg))
                .collect();
            let pick = match (own.as_slice(), cands.len()) {
                ([i], _) => *i,
                (_, 1) => 0,
                _ => {
                    let paths: Vec<String> =
                        cands.iter().map(|d| format!("`{}`", d.path)).collect();
                    let msg = format!("`#[{name}]` is declared by {}", paths.join(" and "));
                    self.err(
                        a.span,
                        msg,
                        Some(
                            "give one of the `#[runtime_attribute(..)]` declarations another name"
                                .to_owned(),
                        ),
                    );
                    continue;
                }
            };
            let d = &self.decls[name][pick];
            let args: Vec<AttrArg> = match &a.body {
                AttrBody::Empty => Vec::new(),
                AttrBody::Args(xs) => xs.clone(),
                AttrBody::Value(_) => vec![AttrArg::Word(String::new())],
            };
            let mut values: Vec<Option<ConstVal>> = vec![None; d.fields.len()];
            let mut problem: Option<String> = None;
            let mut positional = 0;
            for x in &args {
                let (slot, lit) = match x {
                    AttrArg::Lit(l) => {
                        positional += 1;
                        (positional - 1, l)
                    }
                    AttrArg::KeyValue(k, l) => {
                        let Some(i) = d.fields.iter().position(|(n, _)| n == k) else {
                            problem = Some(format!("`#[{name}]` has no field `{k}`"));
                            break;
                        };
                        (i, l)
                    }
                    _ => {
                        problem = Some(format!("`#[{name}]` takes literal values"));
                        break;
                    }
                };
                let Some((fname, p)) = d.fields.get(slot) else {
                    problem = Some(format!("`#[{name}]` takes {} values", d.fields.len()));
                    break;
                };
                match value(lit, *p) {
                    Ok(v) => values[slot] = Some(v),
                    Err(want) => {
                        problem = Some(format!("`{fname}` of `#[{name}]` takes {want}"));
                        break;
                    }
                }
            }
            if problem.is_none() {
                if let Some((n, _)) = d
                    .fields
                    .iter()
                    .zip(&values)
                    .find(|(_, v)| v.is_none())
                    .map(|(f, _)| f)
                {
                    problem = Some(format!("`#[{name}]` needs `{n}`"));
                }
            }
            if let Some(msg) = problem {
                let help = format!(
                    "`#[{name}]` takes `{}`; write `{}`",
                    shape(d),
                    example(name, d)
                );
                self.err(a.span, msg, Some(help));
                continue;
            }
            out.push(Use {
                decl: d.path.clone(),
                values: values.into_iter().flatten().collect(),
            });
        }
        out
    }

    fn field(&mut self, owner: &str, attrs: &[Attribute]) -> FieldAttrs {
        let mut f = FieldAttrs {
            attrs: self.uses(owner, attrs),
            ..FieldAttrs::default()
        };
        for a in attrs {
            match (a.name(), &a.body) {
                ("inspect", _) => f.skip = true,
                ("renamed_from", AttrBody::Args(xs)) => {
                    if let [AttrArg::Lit(Literal::Str(s))] = xs.as_slice() {
                        f.renamed_from = Some(s.clone());
                    }
                }
                _ => {}
            }
        }
        f
    }
}

fn runtime_name(attrs: &[Attribute]) -> Option<String> {
    attrs.iter().find_map(|a| match (a.name(), &a.body) {
        ("runtime_attribute", AttrBody::Args(xs)) => match xs.as_slice() {
            [AttrArg::Word(w)] => Some(w.clone()),
            _ => None,
        },
        _ => None,
    })
}

/// Collect the declarations and every use in `programs`.
pub(crate) fn collect(env: &Env<'_>, programs: &[&Program]) {
    let mut diags = Vec::new();
    let mut cx = Cx {
        decls: HashMap::default(),
        diags: &mut diags,
    };
    let items = || programs.iter().flat_map(|p| p.items.iter());
    for item in items() {
        let Item::Struct(s) = item else { continue };
        let Some(name) = runtime_name(&s.attrs) else {
            continue;
        };
        let mut fields = Vec::new();
        if let StructKind::Named(fs) = &s.kind {
            for f in fs {
                match prim_of(&f.ty) {
                    Some(p) => fields.push((f.name.clone(), p)),
                    None => cx.err(
                        f.span,
                        format!(
                            "field `{}` of runtime attribute `{}` is `{}`",
                            f.name,
                            crate::lower::leaf_name(&s.name),
                            ty_text(&f.ty)
                        ),
                        Some(
                            "a runtime attribute's fields are numbers, `bool`, `char` or `str`"
                                .to_owned(),
                        ),
                    ),
                }
            }
        } else if !matches!(s.kind, StructKind::Unit) {
            cx.err(
                s.span,
                format!(
                    "runtime attribute `{}` has unnamed fields",
                    crate::lower::leaf_name(&s.name)
                ),
                Some("name them: `pub struct Range { pub min: f64; pub max: f64; }`".to_owned()),
            );
        }
        cx.decls.entry(name).or_default().push(Decl {
            path: s.name.clone(),
            fields,
        });
    }
    let mut types: HashMap<String, TypeAttrs> = HashMap::default();
    for item in items() {
        let t = match item {
            Item::Struct(s) => {
                let fields = match &s.kind {
                    StructKind::Named(fs) => {
                        fs.iter().map(|f| cx.field(&s.name, &f.attrs)).collect()
                    }
                    StructKind::Tuple(fs) => {
                        fs.iter().map(|f| cx.field(&s.name, &f.attrs)).collect()
                    }
                    StructKind::Unit => Vec::new(),
                };
                let attrs = if runtime_name(&s.attrs).is_some() {
                    Vec::new()
                } else {
                    cx.uses(&s.name, &s.attrs)
                };
                (
                    s.name.clone(),
                    TypeAttrs {
                        attrs,
                        fields,
                        variants: Vec::new(),
                    },
                )
            }
            Item::Enum(e) => {
                let variants = e
                    .variants
                    .iter()
                    .map(|v| VariantAttrs {
                        attrs: cx.uses(&e.name, &v.attrs),
                        fields: match &v.kind {
                            EnumVariantKind::Named(fs) => {
                                fs.iter().map(|f| cx.field(&e.name, &f.attrs)).collect()
                            }
                            _ => Vec::new(),
                        },
                    })
                    .collect();
                let attrs = cx.uses(&e.name, &e.attrs);
                (
                    e.name.clone(),
                    TypeAttrs {
                        attrs,
                        fields: Vec::new(),
                        variants,
                    },
                )
            }
            _ => continue,
        };
        let empty = t.1.attrs.is_empty()
            && t.1
                .fields
                .iter()
                .all(|f| f.attrs.is_empty() && !f.skip && f.renamed_from.is_none())
            && t.1
                .variants
                .iter()
                .all(|v| v.attrs.is_empty() && v.fields.iter().all(|f| f.attrs.is_empty()));
        if !empty {
            types.insert(t.0, t.1);
        }
    }
    TYPES.with(|t| *t.borrow_mut() = types);
    env.sink.diagnostics.borrow_mut().extend(diags);
}
