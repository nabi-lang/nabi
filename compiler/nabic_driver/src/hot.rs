//! Hot-reload builds (`--hot`).
//!
//! The package hash pinned for a dev session, the version number, the
//! `.layout` dump written beside each version, and the check of a new
//! version's types against the live one's (`--hot-base`), whose
//! refusals are compile errors.

use nabic_core::HashMap;
use nabic_diagnostic::{Code, NabiDiagnostic, Span};
use nabic_ir_common::typedesc::{FieldDesc, TypeDesc};
use nabic_mir::{MirModule, MirTy};
use nabic_nabion::{Decoder, Doc, Kind, Value};
use std::path::{Path, PathBuf};

/// `nabir_abi::typedesc::kind` of the types the dump keeps.
const KINDS: [(u16, &str); 5] = [
    (1, "struct"),
    (2, "tuple_struct"),
    (3, "unit"),
    (4, "enum"),
    (5, "union"),
];
/// `FieldDesc::flags`: a `= expr` default.
const HAS_DEFAULT: u32 = 2;

/// What `--hot` asks for.
#[derive(Clone, Debug, Default)]
pub struct Hot {
    /// `--hot-base`: the live version's `.layout`.
    pub base: Option<PathBuf>,
    /// `--hot-dep`: dependencies that are hot packages too.
    pub deps: Vec<String>,
}

/// The package hash of hot package `name` for every build of a dev
/// session: its name's, so edits keep the symbols and type keys.
#[must_use]
pub fn pin(name: &str) -> String {
    nabic_codegen::pkg_hash(&format!("nabi-hot\0{name}"))
}

/// The dump of a version: `<out stem>.layout`.
#[must_use]
pub fn layout_path(out: &Path) -> PathBuf {
    out.with_extension("layout")
}

/// One field as the dump keeps it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Field {
    name: String,
    /// The real type key.
    ty: String,
    default: bool,
    renamed_from: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Variant {
    name: String,
    tag: u32,
    fields: Vec<Field>,
}

/// One type of a hot package.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Ty {
    key: String,
    path: String,
    kind: String,
    generics: Vec<String>,
    fields: Vec<Field>,
    variants: Vec<Variant>,
}

/// A version's layout.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Layout {
    version: u64,
    package: String,
    pkg8: String,
    target: String,
    types: Vec<Ty>,
    statics: Vec<(String, String)>,
}

/// Before codegen: pin the hot packages' hashes, number the version,
/// and check it against `--hot-base`. The errors refuse the version.
pub(crate) fn prepare(mir: &mut MirModule, hot: &Hot) -> Result<(), Vec<NabiDiagnostic>> {
    let Some(own) = mir.link.lib.clone() else {
        return Ok(());
    };
    for (prefix, hash) in &mut mir.link.packages {
        if prefix.is_empty() || *prefix == own {
            *hash = pin(&own);
        } else if hot.deps.contains(prefix) {
            *hash = pin(prefix);
        }
    }
    let base = match &hot.base {
        Some(p) => Some(read(p).map_err(|e| {
            vec![
                NabiDiagnostic::error(Code::HOT_RELOAD, Span::DUMMY, e).with_help(
                    "pass the `.layout` file written beside the version the program runs, \
                     or leave out `--hot-base` for a first version",
                ),
            ]
        })?),
        None => None,
    };
    let now = of(mir);
    let mut errors = Vec::new();
    if let Some(b) = &base {
        check(b, &now, mir, &mut errors);
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    if let Some(h) = &mut mir.plugin.hot {
        h.base_version = base.as_ref().map_or(0, |b| b.version);
        h.version = h.base_version + 1;
    }
    Ok(())
}

/// The `.layout` text of `mir` (after `prepare`).
#[must_use]
pub fn dump(mir: &MirModule) -> String {
    write(&of(mir))
}

/// The text of `l`.
fn write(l: &Layout) -> String {
    let text = |s: &str| Value::str(s);
    let fields = |fs: &[Field]| {
        Value::new(Kind::List(
            fs.iter()
                .map(|f| {
                    let mut v = vec![
                        ("name", text(&f.name)),
                        ("ty", text(&f.ty)),
                        ("default", Value::new(Kind::Bool(f.default))),
                    ];
                    if !f.renamed_from.is_empty() {
                        v.push(("renamed_from", text(&f.renamed_from)));
                    }
                    Value::record(Some("Field"), v)
                })
                .collect(),
        ))
    };
    let types = l
        .types
        .iter()
        .map(|t| {
            Value::record(
                Some("Type"),
                vec![
                    ("key", text(&t.key)),
                    ("path", text(&t.path)),
                    ("kind", text(&t.kind)),
                    (
                        "generics",
                        Value::new(Kind::List(t.generics.iter().map(|g| text(g)).collect())),
                    ),
                    ("fields", fields(&t.fields)),
                    (
                        "variants",
                        Value::new(Kind::List(
                            t.variants
                                .iter()
                                .map(|v| {
                                    Value::record(
                                        Some("Variant"),
                                        vec![
                                            ("name", text(&v.name)),
                                            ("tag", Value::new(Kind::Int(i128::from(v.tag)))),
                                            ("fields", fields(&v.fields)),
                                        ],
                                    )
                                })
                                .collect(),
                        )),
                    ),
                ],
            )
        })
        .collect();
    let statics = l
        .statics
        .iter()
        .map(|(p, t)| Value::record(Some("Static"), vec![("path", text(p)), ("ty", text(t))]))
        .collect();
    let root = Value::record(
        Some("Layout"),
        vec![
            ("version", Value::new(Kind::Int(i128::from(l.version)))),
            ("package", text(&l.package)),
            ("pkg8", text(&l.pkg8)),
            ("target", text(&l.target)),
            ("types", Value::new(Kind::List(types))),
            ("statics", Value::new(Kind::List(statics))),
        ],
    );
    nabic_nabion::to_string(&root) + "\n"
}

/// The layout of `mir`'s hot-package types.
fn of(mir: &MirModule) -> Layout {
    let Some(hot) = &mir.plugin.hot else {
        return Layout::default();
    };
    let link = &mir.link;
    let descs = &mir.type_descs;
    let key = |i: u32| nabic_codegen_native::hot::real_key(link, &descs[i as usize].key);
    let fields = |fs: &[FieldDesc]| {
        fs.iter()
            .map(|f| Field {
                name: f.name.clone(),
                ty: key(f.ty),
                default: f.flags & HAS_DEFAULT != 0,
                renamed_from: f.renamed_from.clone(),
            })
            .collect()
    };
    let mut types: Vec<Ty> = descs
        .iter()
        .filter(|d| hot.owns(&d.path))
        .filter_map(|d: &TypeDesc<MirTy>| {
            let kind = KINDS.iter().find(|k| k.0 == d.kind)?.1;
            Some(Ty {
                key: nabic_codegen_native::hot::real_key(link, &d.key),
                path: d.path.clone(),
                kind: kind.to_owned(),
                generics: d.generics.iter().map(|&g| key(g)).collect(),
                fields: fields(&d.fields),
                variants: d
                    .variants
                    .iter()
                    .map(|v| Variant {
                        name: v.name.clone(),
                        tag: v.tag,
                        fields: fields(&v.fields),
                    })
                    .collect(),
            })
        })
        .collect();
    types.sort_by(|a, b| a.key.cmp(&b.key));
    let own = link.lib.clone().unwrap_or_default();
    let mut statics: Vec<(String, String)> = mir
        .statics
        .iter()
        .filter(|s| !s.is_extern && hot.owns(&s.name))
        .map(|s| {
            let key = hot.statics.iter().find(|k| k.0 == s.name);
            let ty = key.map_or_else(
                || nabic_codegen_native::hot::mir_key(mir, &s.ty),
                |k| nabic_codegen_native::hot::real_key(link, &k.1),
            );
            (s.name.clone(), ty)
        })
        .collect();
    statics.sort();
    Layout {
        version: hot.version,
        pkg8: link.hash_of(&format!("{own}::")).to_owned(),
        package: own,
        target: mir.target.spec.name.clone(),
        types,
        statics,
    }
}

/// The layout in file `p`.
fn read(p: &Path) -> Result<Layout, String> {
    let shown = p.display().to_string();
    let text =
        std::fs::read_to_string(p).map_err(|e| format!("cannot read `--hot-base {shown}`: {e}"))?;
    let doc = Doc::parse(&text, &shown)?;
    let root = doc.root().record("Layout")?;
    let word = |d: Decoder<'_>| d.string();
    let fields = |d: Decoder<'_>| -> Result<Vec<Field>, String> {
        d.list()?
            .into_iter()
            .map(|f| {
                let f = f.record("Field")?;
                Ok(Field {
                    name: word(f.req("name")?)?,
                    ty: word(f.req("ty")?)?,
                    default: f.req("default")?.bool()?,
                    renamed_from: f
                        .opt("renamed_from")
                        .map(word)
                        .transpose()?
                        .unwrap_or_default(),
                })
            })
            .collect()
    };
    let mut types = Vec::new();
    for item in root.req("types")?.list()? {
        let ty = item.record("Type")?;
        let mut variants = Vec::new();
        for v in ty.req("variants")?.list()? {
            let v = v.record("Variant")?;
            variants.push(Variant {
                name: word(v.req("name")?)?,
                tag: v.req("tag")?.int()?,
                fields: fields(v.req("fields")?)?,
            });
        }
        types.push(Ty {
            key: word(ty.req("key")?)?,
            path: word(ty.req("path")?)?,
            kind: word(ty.req("kind")?)?,
            generics: ty
                .req("generics")?
                .list()?
                .into_iter()
                .map(word)
                .collect::<Result<_, _>>()?,
            fields: fields(ty.req("fields")?)?,
            variants,
        });
    }
    let mut statics = Vec::new();
    let listed = root.opt("statics").map(|d| d.list()).transpose()?;
    for item in listed.unwrap_or_default() {
        let one = item.record("Static")?;
        statics.push((word(one.req("path")?)?, word(one.req("ty")?)?));
    }
    Ok(Layout {
        version: root.req("version")?.int()?,
        package: word(root.req("package")?)?,
        pkg8: word(root.req("pkg8")?)?,
        target: word(root.req("target")?)?,
        types,
        statics,
    })
}

/// `Y1a2b3c4d:game::Vec2` as source writes it: `Vec2`.
fn shown(key: &str) -> String {
    let b = key.as_bytes();
    let ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let mut out = String::new();
    let mut i = 0;
    while let Some(c) = key[i..].chars().next() {
        let hashed = c == 'Y'
            && (i == 0 || !ident(b[i - 1]))
            && b.len() > i + 10
            && b[i + 9] == b':'
            && b[i + 1..i + 9].iter().all(u8::is_ascii_hexdigit);
        if !hashed {
            out.push(c);
            i += c.len_utf8();
            continue;
        }
        let path = &key[i + 10..];
        let end = path
            .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == ':'))
            .unwrap_or(path.len());
        let p = &path[..end];
        out.push_str(p.rsplit("::").next().unwrap_or(p));
        i += 10 + end;
    }
    out
}

/// A value of type `key` to write as an example default.
fn example(key: &str) -> &'static str {
    match key {
        "bool" => "false",
        "str" => "\"\"",
        "f32" | "f64" | "f16" => "0.0",
        "char" => "'a'",
        k if k.starts_with('i') || k.starts_with('u') || k == "int" => "0",
        _ => "..",
    }
}

/// The 5.4 rules: what may change while the program runs.
fn check(base: &Layout, now: &Layout, mir: &MirModule, errors: &mut Vec<NabiDiagnostic>) {
    let span_of = |path: &str| {
        mir.plugin
            .hot
            .as_ref()
            .and_then(|h| h.types.iter().find(|t| t.path == path))
            .map_or(Span::DUMMY, |t| t.span)
    };
    let mut err = |path: &str, msg: String, help: String| {
        errors.push(NabiDiagnostic::error(Code::HOT_RELOAD, span_of(path), msg).with_help(help));
    };
    let old: HashMap<&str, &Ty> = base.types.iter().map(|t| (t.key.as_str(), t)).collect();
    for t in &now.types {
        let name = t.path.rsplit("::").next().unwrap_or(&t.path);
        // Type parameters changed: no instance keeps its key.
        if !old.contains_key(t.key.as_str()) {
            let before = base.types.iter().find(|b| b.path == t.path);
            if let Some(b) = before.filter(|b| b.generics.len() != t.generics.len()) {
                err(
                    &t.path,
                    format!(
                        "`{name}` changed its type parameters ({} before, {} now) while the \
                         program runs",
                        b.generics.len(),
                        t.generics.len()
                    ),
                    "restart with `r` to change a type's parameters".to_owned(),
                );
            }
            continue;
        }
        let b = old[t.key.as_str()];
        let was_enum = b.kind == "enum";
        if (t.kind == "enum") != was_enum {
            let what = |e: bool| if e { "an enum" } else { "a struct" };
            err(
                &t.path,
                format!(
                    "`{name}` changed from {} to {} while the program runs",
                    what(was_enum),
                    what(!was_enum)
                ),
                "restart with `r` to change what kind of type it is".to_owned(),
            );
            continue;
        }
        if t.kind == "enum" {
            for v in &t.variants {
                if let Some(bv) = b.variants.iter().find(|x| x.name == v.name) {
                    fields(
                        &format!("variant `{name}::{}`", v.name),
                        &t.path,
                        &bv.fields,
                        &v.fields,
                        true,
                        &mut err,
                    );
                }
            }
        } else {
            fields(
                &format!("`{name}`"),
                &t.path,
                &b.fields,
                &t.fields,
                false,
                &mut err,
            );
        }
    }
}

/// The fields of one struct or variant: `old` live, `new` built now.
fn fields(
    owner: &str,
    path: &str,
    old: &[Field],
    new: &[Field],
    variant: bool,
    err: &mut dyn FnMut(&str, String, String),
) {
    for f in new {
        let from = if f.renamed_from.is_empty() {
            &f.name
        } else {
            &f.renamed_from
        };
        let Some(o) = old.iter().find(|o| o.name == *from) else {
            if !f.renamed_from.is_empty() {
                let names: Vec<String> = old.iter().map(|o| format!("`{}`", o.name)).collect();
                err(
                    path,
                    format!(
                        "field `{}` of {owner} is `#[renamed_from(\"{}\")]`, and {owner} had \
                         no field `{}`",
                        f.name, f.renamed_from, f.renamed_from
                    ),
                    if names.is_empty() {
                        "it had no fields: drop `#[renamed_from(..)]`".to_owned()
                    } else {
                        format!("name the field as it was before: {}", names.join(", "))
                    },
                );
            } else if variant {
                err(
                    path,
                    format!(
                        "new field `{}` of {owner} while the program runs: a variant's fields \
                         have no defaults",
                        f.name
                    ),
                    "restart with `r` to change what a variant holds".to_owned(),
                );
            } else if !f.default {
                let ty = shown(&f.ty);
                err(
                    path,
                    format!(
                        "new field `{}` of {owner} needs a default while the program runs",
                        f.name
                    ),
                    format!(
                        "write `{}: {ty} = {};` (or restart with `r`)",
                        f.name,
                        example(&f.ty)
                    ),
                );
            }
            continue;
        };
        if o.ty != f.ty {
            let (was, now) = (shown(&o.ty), shown(&f.ty));
            err(
                path,
                format!(
                    "field `{}` of {owner} changed from `{was}` to `{now}` while the program runs",
                    f.name
                ),
                format!(
                    "give the new field its own name with a default (`{}_{}: {now} = {}`) to \
                     start it fresh, or restart with `r`",
                    f.name,
                    now.to_lowercase(),
                    example(&f.ty)
                ),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(name: &str, ty: &str, default: bool) -> Field {
        Field {
            name: name.into(),
            ty: ty.into(),
            default,
            renamed_from: String::new(),
        }
    }

    fn run(old: &[Field], new: &[Field]) -> Vec<(String, String)> {
        let mut out = Vec::new();
        fields(
            "`Player`",
            "game::Player",
            old,
            new,
            false,
            &mut |_, m, h| {
                out.push((m, h));
            },
        );
        out
    }

    #[test]
    fn pins_by_name() {
        assert_eq!(pin("game"), pin("game"));
        assert_ne!(pin("game"), pin("engine"));
        assert_eq!(pin("game").len(), 8);
    }

    #[test]
    fn shows_keys_as_source() {
        assert_eq!(shown("Y1a2b3c4d:game::Vec2"), "Vec2");
        assert_eq!(shown("i32"), "i32");
        assert_eq!(
            shown("Y00000000:core::collections::list::List<Y1a2b3c4d:game::Vec2>"),
            "List<Vec2>"
        );
    }

    #[test]
    fn fields_added_need_a_default() {
        let old = [field("hp", "i32", false)];
        assert!(
            run(
                &old,
                &[field("hp", "i32", false), field("mana", "i32", true)]
            )
            .is_empty()
        );
        let e = run(
            &old,
            &[field("hp", "i32", false), field("mana", "i32", false)],
        );
        assert_eq!(e.len(), 1);
        assert!(e[0].0.contains("new field `mana`"), "{e:?}");
        assert_eq!(e[0].1, "write `mana: i32 = 0;` (or restart with `r`)");
    }

    #[test]
    fn removed_and_reordered_fields_are_fine() {
        let old = [field("a", "i32", false), field("b", "str", false)];
        assert!(run(&old, &[field("b", "str", false)]).is_empty());
        assert!(run(&old, &[field("b", "str", false), field("a", "i32", false)]).is_empty());
    }

    #[test]
    fn a_changed_type_is_refused() {
        let e = run(&[field("hp", "i32", false)], &[field("hp", "f64", false)]);
        assert_eq!(e.len(), 1);
        assert!(e[0].0.contains("changed from `i32` to `f64`"), "{e:?}");
        assert!(e[0].1.contains("hp_f64: f64 = 0.0"), "{e:?}");
    }

    #[test]
    fn renamed_from_names_an_old_field() {
        let mut health = field("health", "i32", false);
        health.renamed_from = "hp".into();
        assert!(run(&[field("hp", "i32", false)], &[health.clone()]).is_empty());
        health.renamed_from = "hpp".into();
        let e = run(&[field("hp", "i32", false)], &[health]);
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].1, "name the field as it was before: `hp`");
    }

    #[test]
    fn a_dump_reads_back() {
        let l = Layout {
            version: 3,
            package: "game".into(),
            pkg8: "1a2b3c4d".into(),
            target: "aarch64-apple-darwin".into(),
            types: vec![Ty {
                key: "Y1a2b3c4d:game::Player".into(),
                path: "game::Player".into(),
                kind: "struct".into(),
                generics: vec![],
                fields: vec![field("hp", "i32", true)],
                variants: vec![],
            }],
            statics: vec![("game::COUNT".into(), "Prim(I32)".into())],
        };
        let dir = std::env::temp_dir().join(format!("nabi-hot-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("game.0.layout");
        std::fs::write(&p, write(&l)).unwrap();
        let back = read(&p);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(back.unwrap(), l);
    }
}
