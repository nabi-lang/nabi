//! Module-level passes: item collection (types, fns, impls, statics),
//! duplicate / trait-impl checks, and the final unresolved-type check.

mod assoc;
mod bodies;
mod bounds;
mod checks;
mod coherence;
mod ffi;
mod impls;
mod intrinsic;
mod items;
mod names;
mod repr;
mod stage;

pub(crate) use crate::lower::collect::assoc::*;
pub(crate) use crate::lower::collect::bodies::*;
pub(crate) use crate::lower::collect::bounds::*;
pub use crate::lower::collect::checks::*;
pub(crate) use crate::lower::collect::coherence::*;
pub(crate) use crate::lower::collect::ffi::*;
pub(crate) use crate::lower::collect::impls::*;
pub(crate) use crate::lower::collect::intrinsic::*;
pub(crate) use crate::lower::collect::items::*;
pub(crate) use crate::lower::collect::names::*;
pub(crate) use crate::lower::collect::repr::*;
pub use stage::LibraryStage;

#[allow(clippy::wildcard_imports)]
use crate::lower::*;
use crate::marks::mark_of;
use HirExprKind as K;
use nabic_ast::{AttrArg, AttrBody, Literal};
use nabic_core::{HashMap, HashSet};
use nabic_diagnostic::{Code, a_or_an};
use nabic_ir_common::{Abi, FnRole, Linkage};
use nabic_parser::{ArgKind, PatKind, Pattern};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// An `extern` block marked `#[link(.., kind = "static")]`:
/// its statics are in the image, not imported from a DLL.
fn links_static(attrs: &[nabic_ast::Attribute]) -> bool {
    attrs.iter().filter(|a| a.path == ["link"]).any(|a| {
        matches!(&a.body, AttrBody::Args(args) if args.iter().any(|x| {
            matches!(x, AttrArg::KeyValue(k, Literal::Str(v)) if k == "kind" && v == "static")
        }))
    })
}

fn is_lazy_init_fn(name: &str) -> bool {
    leaf_name(name).starts_with("__nabi_static_init_")
}

fn lazy_init_fn_name(static_name: &str) -> String {
    match static_name.rsplit_once("::") {
        Some((m, leaf)) => format!("{m}::__nabi_static_init_{leaf}"),
        None => format!("__nabi_static_init_{static_name}"),
    }
}

/// Package `pkg` is `p` or one of its tiers (`core` is in `nabi`,
/// the standard library).
fn within(pkg: &str, p: &str) -> bool {
    pkg.strip_prefix(p)
        .is_some_and(|r| r.is_empty() || r.starts_with("::"))
}

/// A `pub` / `pub(pkg)` fn writes its parameter types
/// (an error per untyped one), and without `->` returns `()`: set here
/// when the body gives no value, else marked `UNIT_RET_ATTR` so the
/// inferred type names the `-> T` to write. Trait impl methods take the
/// trait's types.
fn pub_signatures(program: &mut Program, diags: &mut Vec<nabic_diagnostic::NabiDiagnostic>) {
    let mut fix = |f: &mut FnDef| {
        if !matches!(f.vis, Visibility::Pub | Visibility::Pkg) {
            return;
        }
        let Some(body) = &f.body else {
            return;
        };
        let leaf = leaf_name(&f.name).to_owned();
        let vis = if f.vis == Visibility::Pkg {
            "pub(pkg)"
        } else {
            "pub"
        };
        for p in &f.params {
            if p.ty.is_some() || p.name == "self" || p.variadic {
                continue;
            }
            diags.push(
                nabic_diagnostic::NabiDiagnostic::error(
                    Code::PUB_SIGNATURE,
                    p.span,
                    format!("parameter `{}` of `{vis} fn {leaf}` has no type", p.name),
                )
                .with_help(format!(
                    "write the type it takes, as in `{0}: i32`, or declare `<T>` and write \
                     `{0}: T` to take any type",
                    p.name
                )),
            );
        }
        if f.ret_ty.is_some() || f.name == "main" || f.is_generator {
            return;
        }
        let coro = f
            .attrs
            .iter()
            .any(|a| a.path.first().map(String::as_str) == Some(nabic_ast::CORO_FN_ATTR));
        if !coro && !mono::gives_value(body) {
            f.ret_ty = Some(Ty {
                kind: AstTyKind::Tuple(Vec::new()),
                span: f.span,
            });
        } else if !f.attrs.iter().any(|a| a.path == [nabic_ast::UNIT_RET_ATTR]) {
            f.attrs.push(nabic_ast::Attribute {
                path: vec![nabic_ast::UNIT_RET_ATTR.to_owned()],
                body: nabic_ast::AttrBody::Empty,
                inner: false,
                span: f.span,
            });
        }
    };
    for item in &mut program.items {
        match item {
            Item::Fn(f) => fix(f),
            Item::Impl(i) if i.trait_path.is_none() => {
                for it in &mut i.items {
                    if let AstImplItem::Method(f) = it {
                        fix(f);
                    }
                }
            }
            _ => {}
        }
    }
}

/// An untyped parameter is an implicit type parameter
/// (`fn id(x)` is `fn id<T#x>(x: T#x)`), checked per instance, no bound
/// synthesized. Free fns and inherent methods only (trait impl methods
/// take the trait's types).
fn implicit_generics(program: &mut Program) {
    let fix = |f: &mut FnDef| {
        if f.body.is_none() {
            return;
        }
        for p in &mut f.params {
            if p.ty.is_some() || p.name == "self" || p.variadic {
                continue;
            }
            let name = format!("T#{}", p.name);
            let path = nabic_ast::Path {
                segments: vec![nabic_ast::PathSegment {
                    name: name.clone(),
                    generics: Vec::new(),
                }],
                span: p.span,
                origin: None,
            };
            p.ty = Some(Ty {
                kind: AstTyKind::Path(path),
                span: p.span,
            });
            f.generics.params.push(nabic_ast::GenericParam::Type {
                name,
                bounds: Vec::new(),
                default: None,
            });
        }
    };
    for item in &mut program.items {
        match item {
            Item::Fn(f) => fix(f),
            Item::Impl(i) if i.trait_path.is_none() => {
                for it in &mut i.items {
                    if let AstImplItem::Method(f) = it {
                        fix(f);
                    }
                }
            }
            _ => {}
        }
    }
}

/// `x: some Tr` is an anonymous type parameter (`fn f<S#x: Tr>(x: S#x)`):
/// one static type per call, which the caller cannot name. `x: some
/// fn(A) -> R` takes the function value.
fn some_params(program: &mut Program) {
    let fix = |f: &mut FnDef| {
        for p in &mut f.params {
            let Some(Ty {
                kind: AstTyKind::Some(bounds),
                span,
            }) = &p.ty
            else {
                continue;
            };
            let span = *span;
            if let [
                one @ Ty {
                    kind: AstTyKind::Fn(..),
                    ..
                },
            ] = bounds.as_slice()
            {
                p.ty = Some(one.clone());
                continue;
            }
            let name = format!("{}{}", nabic_ast::SOME_PARAM_PREFIX, p.name);
            let bounds = bounds.clone();
            p.ty = Some(Ty {
                kind: AstTyKind::Path(nabic_ast::Path {
                    segments: vec![nabic_ast::PathSegment {
                        name: name.clone(),
                        generics: Vec::new(),
                    }],
                    span,
                    origin: None,
                }),
                span,
            });
            f.generics.params.push(nabic_ast::GenericParam::Type {
                name,
                bounds,
                default: None,
            });
        }
    };
    for item in &mut program.items {
        match item {
            Item::Fn(f) => fix(f),
            Item::Impl(i) => {
                for it in &mut i.items {
                    if let AstImplItem::Method(f) = it {
                        fix(f);
                    }
                }
            }
            Item::Trait(t) => {
                for it in &mut t.items {
                    if let nabic_ast::TraitItem::Method(f) = it {
                        fix(f);
                    }
                }
            }
            _ => {}
        }
    }
}

/// `type X = some Tr;`: the free functions of X's module that return `X`
/// define it. Each becomes `-> some Tr` (its type comes from its body);
/// `X` is that type, opaque outside the module.
fn some_types(
    program: &mut Program,
    diags: &mut Vec<nabic_diagnostic::NabiDiagnostic>,
) -> HashMap<String, crate::lower::SomeType> {
    let mut out: HashMap<String, crate::lower::SomeType> = HashMap::default();
    for item in &program.items {
        let Item::TypeAlias(a) = item else { continue };
        let AstTyKind::Some(bounds) = &a.ty.kind else {
            continue;
        };
        if !a.generics.params.is_empty() || !a.where_clause.is_empty() {
            let leaf = leaf_name(&a.name);
            diags.push(
                nabic_diagnostic::NabiDiagnostic::error(
                    Code::SOME_TYPE_DEFINITION,
                    a.span,
                    format!("named `some` type `{leaf}` cannot take type parameters"),
                )
                .with_help(format!(
                    "return the opaque type from a generic function instead: `fn \
                     {}<T>(..) -> {}`",
                    leaf.to_lowercase(),
                    nabic_ast::print::print_ty(&a.ty)
                )),
            );
            continue;
        }
        out.insert(
            a.name.clone(),
            crate::lower::SomeType {
                bounds: bounds.clone(),
                definers: Vec::new(),
                span: a.span,
            },
        );
    }
    if out.is_empty() {
        return out;
    }
    let mut generic = nabic_core::HashSet::default();
    for item in &mut program.items {
        let Item::Fn(f) = item else { continue };
        let Some(Ty {
            kind: AstTyKind::Path(p),
            span,
        }) = &f.ret_ty
        else {
            continue;
        };
        let module = module_of(&f.name);
        let full = match p.segments.as_slice() {
            [s] | [_, s] if p.segments.len() == 1 || p.segments[0].name == "self" => {
                if !s.generics.is_empty() {
                    continue;
                }
                if module.is_empty() {
                    s.name.clone()
                } else {
                    format!("{module}::{}", s.name)
                }
            }
            _ => continue,
        };
        let Some(st) = out.get_mut(&full) else {
            continue;
        };
        if !f.generics.params.is_empty() {
            generic.insert(full.clone());
            diags.push(
                nabic_diagnostic::NabiDiagnostic::error(
                    Code::SOME_TYPE_DEFINITION,
                    f.span,
                    format!(
                        "generic function `{}` cannot define `{}`",
                        leaf_name(&f.name),
                        leaf_name(&full)
                    ),
                )
                .with_help(format!(
                    "`{}` is one type, which a generic body would make per call: drop the \
                     type parameters, or write `-> {}` for a type per call",
                    leaf_name(&full),
                    nabic_ast::print::print_ty(&Ty {
                        kind: AstTyKind::Some(st.bounds.clone()),
                        span: *span,
                    })
                )),
            );
            continue;
        }
        st.definers.push(f.name.clone());
        f.ret_ty = Some(Ty {
            kind: AstTyKind::Some(st.bounds.clone()),
            span: *span,
        });
    }
    for (name, st) in &out {
        if st.definers.is_empty() && !generic.contains(name) {
            let leaf = leaf_name(name);
            diags.push(
                nabic_diagnostic::NabiDiagnostic::error(
                    Code::SOME_TYPE_DEFINITION,
                    st.span,
                    format!("`{leaf}` has no defining function"),
                )
                .with_help(format!(
                    "a function in this module that returns `{leaf}` gives its type: `fn \
                     make() -> {leaf} {{ .. }}`"
                )),
            );
        }
    }
    out
}

fn add_lazy_static_inits(program: &mut Program) {
    let inits: Vec<Item> = program
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Static(s) => s.value.as_ref().map(|v| {
                Item::Fn(nabic_ast::FnDef {
                    vis: Visibility::Private,
                    attrs: Vec::new(),
                    name: lazy_init_fn_name(&s.name),
                    generics: nabic_ast::Generics::default(),
                    params: Vec::new(),
                    ret_ty: Some(s.ty.clone()),
                    // `return v;` (not a trailing `v`): the return type
                    // is the expected type that infers `Atomic::new(0)`.
                    body: Some(
                        nabic_ast::Block {
                            stmts: vec![nabic_ast::Stmt {
                                kind: nabic_ast::StmtKind::Return(Some(v.clone())),
                                span: s.span,
                            }],
                            trailing: None,
                            span: s.span,
                            label: None,
                        }
                        .into(),
                    ),
                    is_async: false,
                    is_generator: false,
                    is_unsafe: false,
                    throws: nabic_ast::ThrowsKind::None,
                    where_clause: Vec::new(),
                    span: s.span,
                })
            }),
            _ => None,
        })
        .collect();
    program.items.extend(inits);
}

/// What collecting a stage leaves for the next one and for lowering.
/// The library is collected before the package, so its ids don't depend on
/// the package.
#[derive(Clone)]
pub(crate) struct Collected<'a> {
    t: Tables<'a>,
    /// Ahead of lowering's own diagnostics and not merged with them.
    early: Vec<nabic_diagnostic::NabiDiagnostic>,
    /// Diagnostics of the passes before bodies.
    pre_diags: Vec<nabic_diagnostic::NabiDiagnostic>,
    /// Every `pub use` re-export (globs expanded).
    reexports: Vec<crate::uses::ReExport>,
    /// The declared (not generic) enums and structs, by id.
    enum_decls: Vec<(EnumId, &'a AstEnumDef)>,
    struct_decls: Vec<(StructId, &'a AstStructDef)>,
    /// Inherent associated consts (`S::N`).
    inherent_keys: nabic_core::HashSet<String>,
    static_defs: Vec<HirStaticDef>,
    /// Per static: its type as written, module and initializer.
    static_asts: Vec<(&'a Ty, String)>,
    /// Per decl: its body is library code (a tier / a dependency),
    /// lowered only when reached (`lower_bodies`).
    lib_decl: Vec<bool>,
    /// Decl index -> symbol path of a dependency's method.
    link_names: HashMap<usize, String>,
    /// Generic impls with method templates by tag (`generic_trait_impl`).
    templated: HashMap<String, usize>,
    system_fns: nabic_core::HashSet<FunctionId>,
    no_safepoint: nabic_core::HashSet<FunctionId>,
    /// C declarations' `FFI_*` bits.
    ffi: HashMap<FunctionId, u8>,
    wasm_imports: HashMap<FunctionId, (String, String)>,
    extern_symbols: HashMap<FunctionId, String>,
    trait_defs: HashMap<String, &'a nabic_ast::TraitDef>,
    exports: Vec<(FunctionId, String)>,
    prebuilt: nabic_core::HashSet<FunctionId>,
    c_defs: Vec<FunctionId>,
    /// Leaf names of the functions that diverge (`may_diverge`).
    diverging: nabic_core::HashSet<String>,
}

impl Collected<'_> {
    fn empty() -> Self {
        Self {
            t: Tables::empty(),
            early: Vec::new(),
            pre_diags: Vec::new(),
            reexports: Vec::new(),
            enum_decls: Vec::new(),
            struct_decls: Vec::new(),
            inherent_keys: nabic_core::HashSet::default(),
            static_defs: Vec::new(),
            static_asts: Vec::new(),
            lib_decl: Vec::new(),
            link_names: HashMap::default(),
            templated: HashMap::default(),
            system_fns: nabic_core::HashSet::default(),
            no_safepoint: nabic_core::HashSet::default(),
            ffi: HashMap::default(),
            wasm_imports: HashMap::default(),
            extern_symbols: HashMap::default(),
            trait_defs: HashMap::default(),
            exports: Vec::new(),
            prebuilt: nabic_core::HashSet::default(),
            c_defs: Vec::new(),
            diverging: nabic_core::HashSet::default(),
        }
    }
}

/// Lower `program`: collect the library's items, then the package's on top
/// (`collect`), then the bodies (`finish`).
#[must_use]
pub fn lower_full(
    program: Program,
    prec: &PrecTable,
    raw_uses: &[(String, Visibility, nabic_ast::UseTree)],
    uses: &crate::uses::UseTable,
    reexports: &[crate::uses::ReExport],
    input: &crate::HirInput,
    sources: &nabic_diagnostic::SourceMap,
) -> HirModule {
    let collect_started = nabic_diagnostic::time_passes().then(std::time::Instant::now);
    resolve::set_packages(input.packages.clone());
    resolve::set_scopes(input.scopes.clone());
    resolve::set_own_library(input.own_library.clone());
    resolve::set_no_implicit(input.no_implicit_nabi.clone());
    resolve::set_shadows(input.shadows.clone());
    crate::lower::consteval::set_pointer_width(input.pointer_width);
    crate::lower::consteval::set_stdcall_system(input.stdcall_system);
    crate::lower::objc::set_deployment(input.apple);
    crate::clear_type_names();
    crate::lower::reflect::desc::reset();
    crate::lower::reflect::desc::set_hot(input.hot.clone().unwrap_or_default());
    let mut dropped = crate::cfg_dropped(
        input
            .item_modules
            .iter()
            .map(String::as_str)
            .zip(&program.items),
    );
    dropped.extend(input.cfg_dropped.iter().cloned());
    let (mut lib, mut user) = (Program::default(), Program::default());
    let (mut lib_modules, mut user_modules) = (Vec::new(), Vec::new());
    for (item, module) in program.items.into_iter().zip(&input.item_modules) {
        if package_of(module).is_empty() {
            user.items.push(item);
            user_modules.push(module.clone());
        } else {
            lib.items.push(item);
            lib_modules.push(module.clone());
        }
    }
    let lib_uses: Vec<_> = raw_uses
        .iter()
        .filter(|(m, _, _)| !package_of(m).is_empty())
        .cloned()
        .collect();
    let user_uses: Vec<_> = raw_uses
        .iter()
        .filter(|(m, _, _)| package_of(m).is_empty())
        .cloned()
        .collect();
    let lib_reexports: Vec<_> = reexports
        .iter()
        .filter(|r| !package_of(&r.module_path).is_empty())
        .cloned()
        .collect();
    let user_reexports: Vec<_> = reexports
        .iter()
        .filter(|r| package_of(&r.module_path).is_empty())
        .cloned()
        .collect();
    let at = Stage {
        prec,
        uses,
        input,
        sources,
    };
    // The library's stage: kept for the next compile with the same
    // library (`HirInput::library`), its items leaked to live as long.
    let key = input.library;
    let kept = key.and(input.stage.as_ref()).map(|l| {
        let l = stage(l);
        (l.c.clone(), l.lib, l.type_names.clone())
    });
    let mut lib_items: Program;
    let (base, lib): (Collected<'_>, &Program) = if let Some((mut c, lib, type_names)) = kept {
        crate::set_type_names(type_names);
        // The stage collected the library under another compile's table.
        c.t.names.uses.clone_from(uses);
        (c, lib)
    } else if let Some(k) = key {
        let lib: &'static mut Program = Box::leak(Box::new(lib));
        let (c, lib) = match collect(
            lib,
            &lib_modules,
            &[],
            Collected::empty(),
            &lib_uses,
            &lib_reexports,
            &at,
        ) {
            Ok(c) => c,
            Err(m) => return *m,
        };
        let kept = LibraryStage {
            c: c.clone(),
            lib,
            type_names: crate::type_names(),
        };
        keep_library(k, kept);
        (c, lib)
    } else {
        lib_items = lib;
        match collect(
            &mut lib_items,
            &lib_modules,
            &[],
            Collected::empty(),
            &lib_uses,
            &lib_reexports,
            &at,
        ) {
            Ok(c) => c,
            Err(m) => return *m,
        }
    };
    let (c, user) = match collect(
        &mut user,
        &user_modules,
        &lib.items,
        base,
        &user_uses,
        &user_reexports,
        &at,
    ) {
        Ok(c) => c,
        Err(m) => return *m,
    };
    if let Some(t) = collect_started {
        eprintln!(
            "time: {:>9.1}ms  hir collect",
            t.elapsed().as_secs_f64() * 1e3
        );
    }
    let mut module = finish(c, [lib, user], raw_uses, &at);
    module
        .diagnostics
        .extend(nabic_hir_check::lostwrite::check(&user.items));
    crate::note_dropped(&mut module.diagnostics, &dropped);
    module
}

/// The last library stage, by key. A compile holds its own copy
/// (`HirInput::stage`); another compile may replace this one meanwhile.
static LIBRARY: Mutex<Option<(u64, KeptLibrary)>> = Mutex::new(None);

/// A kept library stage, shared by the compiles that take it.
pub type KeptLibrary = Arc<Mutex<LibraryStage>>;

/// The stage of `kept`, locked.
pub fn stage(kept: &KeptLibrary) -> MutexGuard<'_, LibraryStage> {
    kept.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The library's stage of key `library`, if it is kept.
#[must_use]
pub fn kept_library(library: u64) -> Option<KeptLibrary> {
    LIBRARY
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
        .filter(|(k, _)| *k == library)
        .map(|(_, s)| Arc::clone(s))
}

/// Keep `stage` as the library's stage of key `library`.
pub fn keep_library(library: u64, stage: LibraryStage) -> KeptLibrary {
    let stage = Arc::new(Mutex::new(stage));
    *LIBRARY.lock().unwrap_or_else(PoisonError::into_inner) = Some((library, Arc::clone(&stage)));
    stage
}

/// What every stage of a compile shares.
pub(crate) struct Stage<'s> {
    prec: &'s PrecTable,
    uses: &'s crate::uses::UseTable,
    input: &'s crate::HirInput,
    sources: &'s nabic_diagnostic::SourceMap,
}

/// After both stages: the checks over every impl, the roots, the
/// bodies, and the module.
#[allow(clippy::too_many_lines)] // linear sequence of lowering passes
fn finish<'a>(
    c: Collected<'a>,
    programs: [&'a Program; 2],
    raw_uses: &[(String, Visibility, nabic_ast::UseTree)],
    at: &Stage<'a>,
) -> HirModule {
    let Stage {
        prec,
        input,
        sources,
        ..
    } = *at;
    let lib_pkg: Option<String> = input.lib.clone();
    let check_library = input.check_library;
    let Collected {
        t,
        early,
        pre_diags,
        mut static_defs,
        lib_decl,
        link_names,
        system_fns,
        no_safepoint,
        ffi,
        wasm_imports,
        extern_symbols,
        mut exports,
        prebuilt,
        c_defs,
        ..
    } = c;
    let env = Env { sources, prec, t };
    crate::lower::reflect::attrs::collect(&env, &programs);
    // The files of the package's declarations, and of the library's.
    let decl_files = {
        let files = |lib: bool| -> nabic_core::HashSet<u32> {
            (env.decls.iter().zip(&lib_decl))
                .filter(|(_, l)| **l == lib)
                .map(|(d, _)| d.def().span.file)
                .collect()
        };
        (files(false), files(true))
    };
    // Coherence and impl rules for the package's impls (a dependency's
    // when it is built), every library's with `--check-library`.
    let own = |module: &str| {
        let pkg = package_of(module);
        pkg.is_empty() || check_library || lib_pkg.as_ref().is_some_and(|p| within(pkg, p))
    };
    let checked: &[&Program] = if check_library || lib_pkg.is_some() {
        &programs
    } else {
        &programs[1..]
    };
    nabic_diagnostic::time("hir impl overlap", || check_impl_overlap(&env, &own));
    check_impl_rules(&env, checked, &programs, &own);
    check_internal_marks(&env, checked, &own);
    check_templates(&env, &input.templates);
    let failed_derives: Vec<Span> = checked
        .iter()
        .flat_map(|p| check_derives(&env, p))
        .collect();
    let on_panic = if lib_pkg.as_deref().is_some_and(nabic_core::in_toolchain) {
        HashMap::default()
    } else {
        check_extern_ffi(&env, &programs[1..]);
        on_panic_values(&env, &programs[1..])
    };
    let main = env
        .fn_table
        .get("main")
        .map(|s| s.id)
        .filter(|id| !env.extern_fn_ids.contains(id) && !env.nabi_externs.contains(id));
    let exported: nabic_core::HashSet<FunctionId> = exports.iter().map(|(id, _)| *id).collect();
    let decl_roots: Vec<bool> = env
        .decls
        .iter()
        .zip(&lib_decl)
        .enumerate()
        .map(|(i, (d, &lib))| {
            let f = d.def();
            let module = || match d {
                MethodOrFn::Fn(f) => fn_module(&env, f),
                MethodOrFn::Method { module, .. } => module.clone(),
            };
            !lib || check_library
                || exported.contains(&FunctionId(u32::try_from(i).expect("FunctionId overflow")))
                || is_lazy_init_fn(&f.name)
                || lib_pkg
                    .as_ref()
                    .is_some_and(|p| within(package_of(&module()), p))
        })
        .collect();
    let extern_fn_ids = env.extern_fn_ids.clone();
    let nabi_externs = env.nabi_externs.clone();
    let c_variadic = env.c_variadic.clone();
    nabic_diagnostic::time("hir consts", || {
        crate::lower::consteval::check_user_consts(&env);
    });
    let incr = input.bodies.as_ref().map(|b| {
        nabic_diagnostic::time("hir interface", || {
            let lib_decls = lib_decl.iter().take_while(|&&l| l).count();
            crate::lower::incr::Incr::new(&env, &*b.0, &programs, raw_uses, input, lib_decls)
        })
    });
    // The package's own generic bodies (a dependency's when
    // it is built prebuilt); every library's with `--check-library`.
    nabic_diagnostic::time("hir generic bodies", || {
        crate::lower::bounds::check_all(&env, incr.as_ref(), |module| {
            let pkg = package_of(module);
            pkg.is_empty() || check_library || lib_pkg.as_ref().is_some_and(|p| within(pkg, p))
        });
    });
    let (mut functions, unlowered) = nabic_diagnostic::time("hir bodies", || {
        lower_bodies(&env, &decl_roots, &prebuilt, incr.as_ref())
    });
    let reuse = incr.map(|i| i.reuse.get()).unwrap_or_default();
    nabic_diagnostic::time("hir const fns", || {
        let origin = env.sink.fn_origin.borrow();
        let user_const = |f: &&HirFunction| match origin.get(&f.id) {
            Some(&(is_const, std)) => is_const && !std,
            None => env.decls.get(f.id.0 as usize).is_some_and(|d| {
                d.def()
                    .attrs
                    .iter()
                    .any(|a| a.path == [nabic_ast::CONST_FN_ATTR])
                    && package_of(&match d {
                        MethodOrFn::Fn(f) => fn_module(&env, f),
                        MethodOrFn::Method { module, .. } => module.clone(),
                    })
                    .is_empty()
            }),
        };
        // Checking may lower more bodies (into `extras`): a copy of the
        // ones it checks.
        let extras: Vec<HirFunction> = env
            .sink
            .extras
            .borrow()
            .iter()
            .filter(user_const)
            .cloned()
            .collect();
        let fns: Vec<&HirFunction> = functions.iter().filter(user_const).chain(&extras).collect();
        drop(origin);
        crate::lower::consteval::check_const_fns(&env, &fns);
    });

    // An initializer evaluated now needs no lazy init.
    let folded_statics: HashMap<String, ConstVal> = static_defs
        .iter()
        .filter_map(|s| {
            let init = lazy_init_fn_name(&s.name);
            let f = functions.iter().find(|f| f.name == init)?;
            Some((
                s.name.clone(),
                crate::lower::consteval::static_init(&env, f)?,
            ))
        })
        .collect();
    for (i, name) in link_names {
        functions[i].name = name;
    }
    check_type_bounds(&env);
    check_statics_sync(&env, checked);
    let no_subst = HashMap::default();
    let facts = || crate::lower::checks::EnvFacts::new(&env, &no_subst);
    nabic_diagnostic::time("hir send check", || {
        *env.auto_memo.borrow_mut() = Some(HashMap::default());
        nabic_hir_check::send::check(&facts(), &functions);
        *env.auto_memo.borrow_mut() = None;
    });
    nabic_hir_check::frameptr::check(&facts(), &functions);
    let diags = nabic_hir_check::views::check(&facts(), &functions);
    env.sink.diagnostics.borrow_mut().extend(diags);
    // What each call keeps of its arguments, which both checks follow.
    let keeps = nabic_diagnostic::time("hir affine summaries", || {
        *env.auto_memo.borrow_mut() = Some(HashMap::default());
        let k = crate::lower::affine::summaries(&env, &functions, &input.summaries);
        *env.auto_memo.borrow_mut() = None;
        k
    });
    let summaries = nabic_diagnostic::time("hir handoff check", || {
        *env.auto_memo.borrow_mut() = Some(HashMap::default());
        let (diags, s) = nabic_hir_check::handoff::check(
            &facts(),
            &functions,
            &input.summaries,
            &keeps,
            lib_pkg.is_some(),
        );
        env.sink.diagnostics.borrow_mut().extend(diags);
        *env.auto_memo.borrow_mut() = None;
        s
    });
    let mut summaries = summaries;
    nabic_diagnostic::time("hir affine check", || {
        *env.auto_memo.borrow_mut() = Some(HashMap::default());
        summaries.extend(crate::lower::affine::check(
            &env,
            &mut functions,
            keeps,
            lib_pkg.is_some(),
        ));
        *env.auto_memo.borrow_mut() = None;
    });
    let (any_seeds, any_exports) = crate::lower::expr::any_link(&env, lib_pkg.as_deref());
    let any_types = crate::lower::expr::any_types(&env);
    let mut plugin = crate::lower::plugin::info(&env, input.plugin, lib_pkg.as_deref(), &functions);
    plugin.hot = crate::lower::hot::run(
        &env,
        input.hot.as_deref(),
        &functions,
        &static_defs,
        &mut plugin.exports,
    );
    if lib_pkg.is_some() || !prebuilt.is_empty() {
        exports.extend(any_exports);
    }
    let Tables {
        defs, sink, names, ..
    } = env.t;
    // Append every closure body / monomorphization lifted to a top-level
    // function during expression lowering.
    functions.extend(sink.extras.into_inner());
    let mut structs = defs.structs.into_inner();
    let mut enums = defs.enums.into_inner();
    crate::lower::infer::drop_var_instances(&mut structs, &mut enums);
    for s in &mut static_defs {
        if let Some(c) = folded_statics.get(&s.name) {
            s.init = c.clone();
            continue;
        }
        let init = lazy_init_fn_name(&s.name);
        s.init_fn = functions.iter().find(|f| f.name == init).map(|f| f.id);
    }
    // Per-function facts for MIR / codegen.
    let index: HashMap<FunctionId, usize> = functions
        .iter()
        .enumerate()
        .map(|(i, f)| (f.id, i))
        .collect();
    let mut set = |id: &FunctionId, f: &dyn Fn(&mut nabic_ir_common::FnFacts)| {
        if let Some(&i) = index.get(id) {
            f(&mut functions[i].facts);
        }
    };
    for id in &extern_fn_ids {
        let variadic = c_variadic.contains(id);
        set(id, &|f| {
            f.linkage = Linkage::Extern;
            f.abi = Abi::C {
                variadic,
                system: system_fns.contains(id),
            };
        });
    }
    for id in &nabi_externs {
        set(id, &|f| f.linkage = Linkage::Extern);
    }
    for id in &no_safepoint {
        set(id, &|f| f.no_safepoint = true);
    }
    for (id, bits) in &ffi {
        set(id, &|f| {
            f.errno = bits & FFI_ERRNO != 0;
            f.leaf = bits & FFI_LEAF != 0;
        });
    }
    for (id, v) in &on_panic {
        set(id, &|f| f.on_panic = Some(v.clone()));
    }
    for (id, w) in &wasm_imports {
        set(id, &|f| f.wasm_import = Some(w.clone()));
    }
    for (id, n) in &extern_symbols {
        set(id, &|f| f.link_name = Some(n.clone()));
    }
    for id in &prebuilt {
        set(id, &|f| f.linkage = Linkage::Prebuilt);
    }
    for id in &c_defs {
        let system = system_fns.contains(id);
        set(id, &|f| {
            f.abi = Abi::C {
                variadic: false,
                system,
            };
        });
    }
    for (id, sym) in &exports {
        set(id, &|f| f.export_name = Some(sym.clone()));
    }
    if let Some(id) = &main {
        set(id, &|f| f.role = FnRole::Main);
    }
    crate::lower::caller::lower(&mut functions, &sink.track_caller.borrow(), sources);
    // A prebuilt package's statics are initialized by its `.a`.
    let lazy_inits = static_defs
        .iter()
        .filter(|s| {
            let module = module_of(&s.name);
            let pkg = package_of(&module);
            !input.prebuilt.iter().any(|p| within(pkg, p))
        })
        .filter_map(|s| s.init_fn);
    let roots = if lib_pkg.is_some() {
        let package = (0..decl_roots.len())
            .filter(|&i| decl_roots[i] && lib_decl[i])
            .map(|i| FunctionId(i as u32));
        let dispatchers = functions
            .iter()
            .filter(|f| f.facts.role == FnRole::AnyDispatcher)
            .map(|f| f.id);
        Some(
            package
                .chain(dispatchers)
                .chain(exports.iter().map(|(id, _)| *id))
                .chain(lazy_inits)
                .collect(),
        )
    } else {
        // `--check-library` compiles every body it lowered.
        let every = (0..decl_roots.len())
            .filter(|&i| check_library && decl_roots[i])
            .map(|i| FunctionId(i as u32));
        main.map(|m| {
            std::iter::once(m)
                .chain(exports.iter().map(|(id, _)| *id))
                .chain(lazy_inits)
                .chain(every)
                .collect()
        })
    };
    // The package's functions ahead of the library's (their ids are the
    // other way round): its first one names the debug info's unit.
    functions.sort_by_key(|f| lib_decl.get(f.id.0 as usize).copied().unwrap_or(false));
    let mut module = HirModule {
        functions,
        structs,
        enums,
        statics: static_defs,
        diagnostics: pre_diags,
        roots,
        unlowered,
        any_seeds,
        any_types,
        any_tags: sink.any_tags.take(),
        type_visibility: names.visibility.clone(),
        reuse,
        summaries,
        type_descs: crate::lower::reflect::desc::take(),
        plugin,
    };
    module.diagnostics.extend(names.diags.take());
    module.diagnostics.extend(sink.diagnostics.into_inner());
    module
        .diagnostics
        .extend(sink.instance_warns.into_inner().agreed());
    let bare = names.bare_generics.take();
    let generics_of = |full: &str| -> Option<(nabic_ast::Generics, nabic_diagnostic::Span)> {
        let at = *bare.get(full)?;
        defs.generic_structs
            .get(full)
            .map(|d| &d.generics)
            .or_else(|| defs.generic_unions.get(full).map(|d| &d.generics))
            .or_else(|| defs.generic_enums.get(full).map(|d| &d.generics))
            .or_else(|| names.generic_aliases.get(full).map(|d| &d.generics))
            .filter(|g| !g.params.is_empty())
            .map(|g| (g.clone(), at))
    };
    check_unresolved_types(
        &mut module,
        &names.unknown_spans.take(),
        &generics_of,
        sources,
        &names,
    );
    check_static_cycles(&mut module);
    check_value_types(&mut module);
    check_packed(&mut module);
    // One cause can be reached from several passes; report each message
    // once per place.
    let mut seen = nabic_core::HashSet::default();
    module
        .diagnostics
        .retain(|d| seen.insert((d.primary.as_ref().map(|l| l.span), d.message.clone())));
    // A library error alongside a package error follows from it: drop it.
    if !check_library && lib_pkg.is_none() {
        let (own, lib) = &decl_files;
        let file_of =
            |d: &nabic_diagnostic::NabiDiagnostic| d.primary.as_ref().map(|l| l.span.file);
        let in_own =
            |d: &nabic_diagnostic::NabiDiagnostic| file_of(d).is_some_and(|f| own.contains(&f));
        if module.diagnostics.iter().any(|d| d.is_error() && in_own(d)) {
            module.diagnostics.retain(|d| {
                !d.is_error() || in_own(d) || !file_of(d).is_some_and(|f| lib.contains(&f))
            });
        }
    }
    // A derive that failed its field check leaves its generated impl's
    // errors (located at the attribute) as a cascade: drop them.
    module.diagnostics.retain(|d| {
        d.kind() == nabic_diagnostic::ErrorKind::Meta
            || !d
                .primary
                .as_ref()
                .is_some_and(|l| failed_derives.contains(&l.span))
    });
    // A block-local item's capture error says why its name
    // is not found there.
    let captured: nabic_core::HashSet<_> = (module.diagnostics.iter())
        .filter(|d| d.message.contains(" and can't use `"))
        .filter_map(|d| d.primary.as_ref().map(|l| l.span))
        .collect();
    module.diagnostics.retain(|d| {
        !(d.message.starts_with("cannot find ")
            && d.primary
                .as_ref()
                .is_some_and(|l| captured.contains(&l.span)))
    });
    for p in programs {
        show_local_names(p, &mut module.diagnostics);
    }
    module.diagnostics.splice(0..0, early);
    module
}

/// Where a declaration is.
fn decl_span(d: &MethodOrFn<'_>) -> Span {
    match d {
        MethodOrFn::Fn(f) => f.span,
        MethodOrFn::Method { fn_def, .. } => fn_def.span,
    }
}

/// A second `what` named `name`; against a const or static of that
/// name, both kinds.
fn value_dup(what: &str, name: &str, values: &HashMap<String, (Span, &str)>) -> String {
    match values.get(name) {
        Some((_, other)) if *other != what => {
            format!(
                "`{name}` is defined more than once, as {} {other} and as {} {what}",
                a_or_an(other),
                a_or_an(what)
            )
        }
        _ => format!("{what} `{name}` is defined more than once"),
    }
}

fn method_key(
    fn_table: &HashMap<String, std::sync::Arc<FnSig>>,
    method_info: &HashMap<FunctionId, MethodInfo>,
    target: &HirTy,
    leaf: &str,
    trait_name: Option<&str>,
    trait_args: &str,
) -> Option<String> {
    let key = format!("{}__{leaf}", ty_mangle(target));
    let Some(prev) = fn_table.get(&key) else {
        return Some(key);
    };
    let prev_trait = method_info
        .get(&prev.id)
        .and_then(|i| i.trait_name.as_deref());
    // The same trait again is a duplicate unless its arguments differ
    // (checked per impl before this).
    if prev_trait == trait_name && trait_args.is_empty() {
        return None;
    }
    let alt = format!(
        "{key}__by_{}{trait_args}",
        trait_name.map_or("inherent".to_owned(), |t| t.replace("::", "_"))
    );
    (!fn_table.contains_key(&alt)).then_some(alt)
}

/// The copy-out shape of a function with `ref` parameters.
pub(crate) fn ref_out_of(env: &Env<'_>, sig: &FnSig) -> Option<RefOut> {
    let deferred = env.decl(sig.id).is_some_and(|d| mono::defers_refs(d.def()));
    let refs: Vec<(usize, HirTy)> = sig
        .param_refs
        .iter()
        .enumerate()
        .filter(|(_, r)| **r)
        .map(|(i, _)| (i, mono::ref_slot_ty(deferred, sig.param_tys[i].clone())))
        .collect();
    (!refs.is_empty()).then(|| RefOut {
        ret: sig.ret_ty.clone(),
        refs,
        deferred,
    })
}

/// Every `#[literal]` function's signature against its
/// pattern's captures; the built-in shapes' handlers.
fn literal_handlers(
    program: &Program,
    diags: &mut Vec<nabic_diagnostic::NabiDiagnostic>,
) -> Vec<crate::lower::ShapeHandler> {
    let mut out = Vec::new();
    // (pattern, priority, return type leaf) -> first handler; a second is
    // ambiguous wherever the pattern matches.
    let mut seen: HashMap<(String, i64, String), (String, nabic_diagnostic::Span)> =
        HashMap::default();
    for item in &program.items {
        let Item::Fn(f) = item else { continue };
        let Some(attr) = f.attrs.iter().find(|a| a.name() == "literal") else {
            continue;
        };
        let nabic_ast::AttrBody::Args(args) = &attr.body else {
            continue;
        };
        let Some(nabic_ast::AttrArg::Lit(nabic_ast::Literal::Str(text))) = args.first() else {
            continue;
        };
        let Some(pat) = Pattern::from_text(text) else {
            continue;
        };
        let priority = args
            .iter()
            .find_map(|a| match a {
                nabic_ast::AttrArg::KeyValue(k, nabic_ast::Literal::Int(v)) if k == "priority" => {
                    v.parse().ok()
                }
                _ => None,
            })
            .unwrap_or(0);
        let want = pat.args();
        let fits = want.len() == f.params.len()
            && want
                .iter()
                .zip(&f.params)
                .all(|(k, p)| !p.variadic || *k == ArgKind::Many);
        if !fits {
            let shape: Vec<&str> = want
                .iter()
                .map(|k| match k {
                    ArgKind::One => "a value",
                    ArgKind::Many => "a repetition (`...T`, an array or a List)",
                })
                .collect();
            diags.push(
                nabic_diagnostic::NabiDiagnostic::error(
                    Code::INVALID_LITERAL_HANDLER,
                    attr.span,
                    format!(
                        "`#[literal({text})]` handler `{}` must take {} parameter(s): {}",
                        f.name,
                        want.len(),
                        if shape.is_empty() {
                            "none".to_owned()
                        } else {
                            shape.join(", ")
                        }
                    ),
                )
                .with_secondary(f.span, "handler declared here"),
            );
            continue;
        }
        let ret_leaf = match f.ret_ty.as_ref().map(|t| &t.kind) {
            Some(AstTyKind::Path(p)) => p.segments.last().map(|s| s.name.clone()),
            _ => None,
        };
        let key = (pat.render(), priority, ret_leaf.unwrap_or_default());
        if let Some((first, first_span)) = seen.get(&key) {
            diags.push(
                nabic_diagnostic::NabiDiagnostic::error(
                    Code::DUPLICATE_REGISTRATION,
                    attr.span,
                    format!(
                        "`#[literal({text})]` is already registered by `{first}` with the same \
                         priority and return type: every use would be ambiguous - drop one, or \
                         give one a `priority = N`"
                    ),
                )
                .with_secondary(*first_span, "first registered here"),
            );
            continue;
        }
        seen.insert(key, (f.name.clone(), attr.span));
        if let Ok(PatKind::Shape(shape)) = pat.kind() {
            out.push(crate::lower::ShapeHandler {
                shape,
                name: f.name.clone(),
                priority,
                span: attr.span,
            });
        }
    }
    out
}

/// The module of item `name` (`m::f`, `m::T`, or a method `m::T::f`):
/// type segments are `UpperCamelCase`, module segments are not.
fn owner_module(name: &str) -> String {
    let mut m = module_of(name);
    while leaf_name(&m).starts_with(|c: char| c.is_ascii_uppercase()) {
        m = module_of(&m);
    }
    m
}

/// `Tables::main_lets` of the implicit `main`.
fn main_lets(program: &Program) -> HashMap<String, String> {
    let main = program.items.iter().find_map(|i| match i {
        Item::Fn(f) if f.name == "main" => f.body.as_ref(),
        _ => None,
    });
    let mut out = HashMap::default();
    for s in main.iter().flat_map(|b| &b.stmts) {
        let StmtKind::Let { name, ty, init } = &s.kind else {
            continue;
        };
        if name.starts_with(['#', '_']) {
            continue;
        }
        let ty = match (ty, &init.kind) {
            (Some(t), _) => nabic_ast::print::print_ty(t),
            (None, ExprKind::Literal(nabic_ast::Literal::Int(t)))
                if t.bytes().all(|b| b.is_ascii_digit() || b == b'_') =>
            {
                "i32".to_owned()
            }
            (None, ExprKind::Literal(nabic_ast::Literal::Float(_))) => "f64".to_owned(),
            (None, ExprKind::Literal(nabic_ast::Literal::Str(_))) => "str".to_owned(),
            (None, ExprKind::Literal(nabic_ast::Literal::Bool(_))) => "bool".to_owned(),
            (None, _) => "..".to_owned(),
        };
        let konst = format!(
            "const {}: {ty} = {};",
            name.to_uppercase(),
            crate::lower::expr::snippet(init)
        );
        out.insert(name.clone(), konst);
    }
    out
}
