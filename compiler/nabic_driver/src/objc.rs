//! `extern "objc"` blocks: Objective-C classes and their methods, turned
//! into Nabi items before name resolution.
//!
//! A block with `#[objc::class(C)]` (or `#[objc::class(C, extends(S),
//! conforms(P))]`) declares class `C`: a marker type `C`, `IsA` for what
//! `C` is a kind of, and its methods as two traits, the instance ones on
//! every `Id<T>` and the class ones on every `T` with `T: IsA<C>`, so
//! subclasses get them wherever they are declared. `#[objc::protocol(P)]`
//! does the same for objects known by a protocol, `#[objc::category(C)]`
//! adds methods to a class declared elsewhere (`bare::ffi::objc`'s
//! `NSString`, ..). Every method is a wrapper around `objc_msgSend`
//! declared with the C signature of the message, so the C ABI of the
//! arguments applies as is; the selector and the class are
//! `sel_registerName` / `objc_getRequiredClass` calls of a literal, which
//! codegen makes static references. `ffi::objc::import` writes such
//! blocks from headers (`import`).

mod decl;
mod emit;
mod import;

use crate::objc::decl::{Decl, Kind, Method, err, parse_class_attr};
use crate::objc::emit::{Out, path_to};
use crate::{MetaCtx, ModItem, UseEntry, flatten_module_items};
use nabic_ast::{Attribute, ExternBlock, ExternItem, Item};
use nabic_diagnostic::{NabiDiagnostic, SourceMap};
use nabic_lexer::Lexer;
use nabic_parser::{ExtOps, apply_keywords, parse_program_meta};
use std::collections::{BTreeMap, BTreeSet};

/// The ABI string of the blocks this module expands.
const ABI: &str = "objc";

const OBJC: &str = "bare::ffi::objc";
const RAW: &str = "bare::ptr::CPtr<u8>";

/// Classes `bare::ffi::objc` declares, with their superclasses.
const BUILTIN: [(&str, Option<&str>); 3] = [
    ("NSObject", None),
    ("NSString", Some("NSObject")),
    ("NSError", Some("NSObject")),
];

/// Attributes only methods of an `extern "objc"` block take.
const METHOD_ATTRS: [&str; 3] = ["selector", "returns_retained", "returns_not_retained"];

/// `#[objc::class(..)]` / `#[objc::protocol(..)]` /
/// `#[objc::category(..)]` arguments; what is wrong with them, for
/// `attrs`.
pub(crate) fn class_args(a: &Attribute) -> Option<&'static str> {
    let category = a.path.last().is_some_and(|l| l == "category");
    match parse_class_attr(a) {
        Some(c) if !category || c.sup.is_none() && c.conforms.is_empty() => None,
        _ if category => Some("the class: `(NSString)`"),
        _ => Some(
            "`(NSString)`, `(NSMutableString, extends(NSString))` or \
             `(NSView, extends(NSResponder), conforms(NSCoding))`",
        ),
    }
}

/// libobjc for a program that calls into it: on Apple targets, when one
/// of its functions is the Objective-C runtime's.
pub(crate) fn link_runtime(mir: &nabic_mir::MirModule, libs: &mut Vec<(String, bool)>) {
    if mir.target.vendor != "apple" || libs.iter().any(|(n, _)| n == "objc") {
        return;
    }
    let runtime = mir.functions.iter().any(|f| {
        let name = f.facts.link_name.as_deref().unwrap_or(&f.name);
        f.facts.linkage == nabic_ir_common::Linkage::Extern
            && ["objc_", "sel_", "class_", "object_", "_Block_"]
                .iter()
                .any(|p| name.starts_with(p))
    });
    if runtime {
        libs.push(("objc".to_owned(), false));
    }
}

/// Take the `extern "objc"` blocks out of `items`; report method
/// attributes on other extern blocks.
fn take_blocks(
    items: &mut Vec<ModItem>,
    diags: &mut Vec<NabiDiagnostic>,
) -> Vec<(String, ExternBlock)> {
    let mut blocks = Vec::new();
    let mut kept = Vec::with_capacity(items.len());
    for (module, item) in items.drain(..) {
        match item {
            Item::Extern(b) if b.abi == ABI => blocks.push((module, b)),
            Item::Extern(b) => {
                let fns = b.items.iter().filter_map(|i| match i {
                    ExternItem::Fn(f) => Some(f),
                    ExternItem::Static(_) => None,
                });
                for a in fns.flat_map(|f| &f.attrs) {
                    if let Some(name) = METHOD_ATTRS.iter().find(|n| a.path == [**n]) {
                        err(
                            diags,
                            a.span,
                            format!(
                                "`#[{name}]` is for methods of an `extern \"objc\"` block, not \
                                 an `extern \"{}\"` one",
                                b.abi
                            ),
                        );
                    }
                }
                kept.push((module, Item::Extern(b)));
            }
            other => kept.push((module, other)),
        }
    }
    *items = kept;
    blocks
}

/// A class or protocol: where it is declared, its superclass (a
/// protocol: its parent) and the protocols it conforms to.
struct Info {
    module: String,
    sup: Option<String>,
    conforms: Vec<String>,
}

/// Every class and protocol `decls` and `bare::ffi::objc` declare; one
/// declared twice is reported.
fn classes_of(decls: &[Decl], diags: &mut Vec<NabiDiagnostic>) -> BTreeMap<String, Info> {
    let mut classes: BTreeMap<String, Info> = BUILTIN
        .iter()
        .map(|(c, s)| {
            let info = Info {
                module: OBJC.to_owned(),
                sup: s.map(str::to_owned),
                conforms: Vec::new(),
            };
            ((*c).to_owned(), info)
        })
        .collect();
    for d in decls.iter().filter(|d| d.kind != Kind::Category) {
        if let Some(i) = classes.get(&d.class) {
            let place = match i.module.as_str() {
                OBJC => "`bare::ffi::objc` declares it".to_owned(),
                "" => "it is declared in the root module".to_owned(),
                m => format!("it is declared in `{m}`"),
            };
            err(
                diags,
                d.span,
                format!(
                    "class `{c}` is declared twice ({place}): add methods to it with \
                     `#[objc::category({c})]`, or import all headers with one \
                     `ffi::objc::import`",
                    c = d.class
                ),
            );
            continue;
        }
        let info = Info {
            module: d.module.clone(),
            sup: d.sup.clone(),
            conforms: d.conforms.clone(),
        };
        classes.insert(d.class.clone(), info);
    }
    classes
}

/// Whether `d` names classes and protocols that exist; reported when
/// not.
fn known(d: &Decl, classes: &BTreeMap<String, Info>, diags: &mut Vec<NabiDiagnostic>) -> bool {
    let missing = d
        .sup
        .as_ref()
        .filter(|s| !classes.contains_key(*s))
        .map(|s| format!("`extends({s})`: no class `{s}` is declared"))
        .or_else(|| {
            (d.kind == Kind::Category && !classes.contains_key(&d.class)).then(|| {
                format!(
                    "`#[objc::category({})]`: no class `{}` is declared",
                    d.class, d.class
                )
            })
        });
    if let Some(head) = missing {
        let c = d
            .sup
            .as_ref()
            .filter(|_| d.kind != Kind::Category)
            .unwrap_or(&d.class);
        err(
            diags,
            d.span,
            format!("{head}; declare it with `#[objc::class({c})]` on an `extern \"objc\"` block"),
        );
        return false;
    }
    if let Some(p) = d.conforms.iter().find(|p| !classes.contains_key(*p)) {
        err(
            diags,
            d.span,
            format!(
                "`conforms({p})`: no protocol `{p}` is declared; declare it with \
                 `#[objc::protocol({p})]` on an `extern \"objc\"` block"
            ),
        );
        return false;
    }
    true
}

/// `c` and everything it is a kind of: its superclasses, the protocols
/// they conform to and those protocols' parents; `c` first.
fn ancestors(c: &str, classes: &BTreeMap<String, Info>) -> Vec<String> {
    let mut out = vec![c.to_owned()];
    let mut i = 0;
    while i < out.len() {
        if let Some(info) = classes.get(&out[i]) {
            for n in info.sup.iter().chain(&info.conforms) {
                if classes.contains_key(n) && !out.contains(n) {
                    out.push(n.clone());
                }
            }
        }
        i += 1;
    }
    out
}

/// The path to class `c` from `module`, `None` across packages.
fn class_path(
    c: &str,
    module: &str,
    classes: &BTreeMap<String, Info>,
    packages: &[String],
) -> Option<String> {
    match classes.get(c) {
        Some(i) if i.module == OBJC => Some(format!("{OBJC}::{c}")),
        Some(i) => path_to(packages, &i.module, module, c),
        None => None,
    }
}

/// The methods of `d` its class's ancestors do not declare already
/// (by name, instance and class methods apart): theirs reach it, and a
/// second trait with the name would make calls ambiguous.
fn own_methods<'a>(
    d: &'a Decl,
    decls: &[Decl],
    classes: &BTreeMap<String, Info>,
) -> Vec<&'a Method> {
    let up = ancestors(&d.class, classes);
    let taken: BTreeSet<(bool, &str)> = decls
        .iter()
        .filter(|x| x.class != d.class && up.contains(&x.class))
        .flat_map(|x| x.methods.iter().map(|m| (m.instance, m.def.name.as_str())))
        .collect();
    d.methods
        .iter()
        .filter(|m| !taken.contains(&(m.instance, m.def.name.as_str())))
        .collect()
}

/// Mark the methods of the impls among `items` as the compiler's: the
/// thousands of wrappers an import makes are checked as they are used.
fn glue(items: &mut [Item]) {
    for item in items {
        if let Item::Impl(i) = item {
            for m in &mut i.items {
                if let nabic_ast::ImplItem::Method(f) = m {
                    f.attrs.push(Attribute {
                        path: vec![nabic_ast::GLUE_ATTR.to_owned()],
                        body: nabic_ast::AttrBody::Empty,
                        inner: false,
                        span: f.span,
                    });
                }
            }
        }
    }
}

/// Parse `source`, the bindings of `module` (kept in file `file`), into
/// `items`.
#[allow(clippy::too_many_arguments)] // the parse context
fn parse_into(
    module: &str,
    source: &str,
    file: Option<&str>,
    items: &mut Vec<ModItem>,
    uses: &mut Vec<UseEntry>,
    ext: &ExtOps,
    metas: &MetaCtx,
    diags: &mut Vec<NabiDiagnostic>,
    files: &mut SourceMap,
) {
    let name = file.map_or_else(
        || {
            format!(
                "<Objective-C bindings of `{}`>",
                if module.is_empty() { "pkg" } else { module }
            )
        },
        str::to_owned,
    );
    let file_id = files.add(name.clone(), source.to_owned());
    let (mut tokens, lex) = Lexer::new(source, name.clone())
        .with_ext_ops(ext.symbols())
        .lex_all();
    for t in &mut tokens {
        t.span.file = file_id;
    }
    diags.extend(lex);
    apply_keywords(&mut tokens, &metas.keywords, diags);
    let (program, parse_diags) = parse_program_meta(
        &tokens,
        name,
        &metas.package,
        ext,
        &metas.table,
        &metas.literals,
        Some(&metas.cfg),
    );
    diags.extend(parse_diags);
    let mut program = program;
    glue(&mut program.items);
    let path: Vec<String> = if module.is_empty() {
        Vec::new()
    } else {
        module.split("::").map(str::to_owned).collect()
    };
    flatten_module_items(
        program.items,
        &path,
        None,
        ext,
        metas,
        diags,
        items,
        uses,
        None,
        files,
    );
}

/// Replace the `extern "objc"` blocks of `items` (the package's and its
/// dependencies', by module) with their bindings.
#[allow(clippy::too_many_arguments)] // the parse context
pub fn expand(
    items: &mut Vec<ModItem>,
    uses: &mut Vec<UseEntry>,
    packages: &[String],
    target: &nabic_mir::TargetSpec,
    ext: &ExtOps,
    metas: &MetaCtx,
    diags: &mut Vec<NabiDiagnostic>,
    files: &mut SourceMap,
) {
    import::expand(items, uses, target, ext, metas, diags, files);
    let blocks = take_blocks(items, diags);
    if blocks.is_empty() {
        return;
    }
    if target.vendor != "apple" {
        for (_, b) in &blocks {
            err(
                diags,
                b.span,
                format!(
                    "`extern \"objc\"` declares Objective-C classes, which exist only on Apple \
                     targets (macOS and iOS), not {}: put it and the code using it under \
                     `#[cfg(any(target_os = \"macos\", target_os = \"ios\"))]`",
                    target.name
                ),
            );
        }
        return;
    }
    let decls: Vec<Decl> = blocks
        .into_iter()
        .filter_map(|(m, b)| decl::decl(m, b, diags))
        .collect();
    let classes = classes_of(&decls, diags);
    let mut outs: BTreeMap<String, Out> = BTreeMap::new();
    for d in &decls {
        if !known(d, &classes, diags) {
            continue;
        }
        let out = outs.entry(d.module.clone()).or_default();
        out.links_of(d);
        if d.kind != Kind::Category {
            let sup = d
                .sup
                .as_ref()
                .map(|s| class_path(s, &d.module, &classes, packages).unwrap_or_else(|| s.clone()));
            let up = ancestors(&d.class, &classes);
            let isa: Vec<String> = up
                .iter()
                .filter_map(|a| class_path(a, &d.module, &classes, packages))
                .collect();
            let main = up.iter().any(|a| a == "NSResponder");
            out.class(d, sup.as_deref(), &isa, main);
            if d.kind == Kind::Class && up.iter().any(|a| a == "NSString") {
                out.string_text(&d.class);
            }
        }
        let ty =
            class_path(&d.class, &d.module, &classes, packages).unwrap_or_else(|| d.class.clone());
        let ms = own_methods(d, &decls, &classes);
        out.methods(&d.module, &ms, &d.class, &ty, diags);
    }
    for (module, out) in outs {
        let source = out.source();
        parse_into(
            &module, &source, None, items, uses, ext, metas, diags, files,
        );
    }
}

#[cfg(test)]
mod tests {
    use crate::objc::decl::family;
    use crate::objc::emit::{hash, path_to, ty_text};

    #[test]
    fn method_families() {
        assert_eq!(family("alloc"), "alloc");
        assert_eq!(family("initWithFrame:"), "init");
        assert_eq!(family("_init"), "init");
        assert_eq!(family("newCommandQueue"), "new");
        assert_eq!(family("mutableCopy"), "mutableCopy");
        assert_eq!(family("copyWithZone:"), "copy");
        assert_eq!(family("initialize"), "");
        assert_eq!(family("newsletter"), "");
        assert_eq!(family("count"), "");
    }

    #[test]
    fn paths_within_a_package() {
        let pkgs = vec![String::new(), "dep".to_owned()];
        assert_eq!(path_to(&pkgs, "ns", "ns", "C").as_deref(), Some("C"));
        assert_eq!(
            path_to(&pkgs, "a", "b::c", "C").as_deref(),
            Some("pkg::b::c::C")
        );
        assert_eq!(path_to(&pkgs, "a", "", "C").as_deref(), Some("pkg::C"));
        assert_eq!(
            path_to(&pkgs, "dep::x", "dep", "C").as_deref(),
            Some("pkg::C")
        );
        assert_eq!(path_to(&pkgs, "dep::x", "a", "C"), None);
    }

    #[test]
    fn self_is_the_class() {
        let t = nabic_ast::Ty {
            kind: nabic_ast::TyKind::SelfUpper,
            span: nabic_diagnostic::Span::DUMMY,
        };
        assert_eq!(ty_text(&t, "pkg::ns::NSArray"), "pkg::ns::NSArray");
        assert_ne!(hash("a|b"), hash("a|c"));
    }
}
