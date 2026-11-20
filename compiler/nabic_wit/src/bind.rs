//! nabi source for WIT bindings: types as declarations, imports as nabi
//! functions that lower arguments and lift results through the canonical
//! ABI, exports as `extern "C"` entries that lift, call the user's
//! function and lower the result (a post-return frees it).
//!
//! Mapping: `record` -> `struct`, `variant` / `enum` -> `enum`, `flags` ->
//! a `struct` of `bool`s, `option<T>` -> `T?`, `result<T, E>` ->
//! `Result<T, E>` (a function returning one `throws (E) -> T`),
//! `tuple` -> tuple, `list<T>` -> `List<T>`, `list<T, N>` -> `[T; N]`,
//! `string` -> `str`, `sN` / `uN` -> `iN` / `uN`, `future<T>` ->
//! `AsyncFuture<T>`, `stream<T>` -> a channel `Receiver<T>`,
//! `error-context` -> `ErrorContext`. An imported resource is a struct
//! over its handle (`Drop` drops it); an exported one is the user's
//! struct, kept in a table while the host holds a handle to it. An
//! `async func` is an `async fn` both ways (the async canonical ABI:
//! imports lowered async, exports lifted with a callback).

mod canon;
mod export;
mod import;
mod types;

pub(crate) use crate::bind::canon::*;

use crate::abi::{self, Flat, MAX_FLAT_PARAMS, MAX_FLAT_RESULTS};
use crate::parse::Prim;
use crate::resolve::{
    FnKind, Function, IfaceId, Owner, Resolve, Type, TypeId, TypeKind, WorldEntry, WorldId,
};
use crate::{Loaded, Request};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

/// The support library the generated code calls: memory, blocks,
/// handles (`W`); the async ABI, which runs on the executor (`A`).
const W: &str = "::bare::ffi::wit";
const A: &str = "::std::wit";
const LIST: &str = "::core::collections::list::List";
const RESULT: &str = "::core::error::Result";
const FUTURE: &str = "::core::future::AsyncFuture";
const RECEIVER: &str = "::core::channel::Receiver";

/// Most flat parameters of an async-lowered import.
const MAX_FLAT_ASYNC_PARAMS: usize = 4;

/// What [`bind`] makes of the `ffi::wit` items of one module.
#[derive(Clone, Debug, Default)]
pub struct Bindings {
    /// nabi items to add to the module the `ffi::wit` items are in.
    pub source: String,
    /// Symbols the core module must export.
    pub exports: Vec<String>,
    /// The component world's `import` / `export` / `include` lines.
    pub world: String,
}

pub(crate) enum View {
    Prim(Prim),
    List(Type),
    Option(Type),
    Result(Option<Type>, Option<Type>),
    Tuple(Vec<Type>),
    FixedList(Type, u32),
    Future,
    Stream,
    ErrorContext,
    Record(TypeId, Vec<(String, Type)>),
    Variant(TypeId, Vec<(String, Option<Type>)>),
    Enum(TypeId, usize),
    Flags(TypeId, Vec<String>),
    Own(TypeId),
    Borrow(TypeId),
}

pub(crate) struct Gen<'a> {
    r: &'a Resolve,
    /// nabi path of the module the bindings go in (`pkg`, `pkg::m`).
    base: String,
    exported: BTreeSet<IfaceId>,
    /// Generated items by module path (below `base`).
    mods: BTreeMap<Vec<String>, String>,
    /// `extern "C"` declarations by module path and core module.
    externs: Externs,
    exports: Vec<String>,
    /// The function being bound: what its futures' and streams' built-ins
    /// are named after.
    cur: Option<FnCtx>,
    /// Payload ops functions made.
    ops: BTreeSet<String>,
    /// The user's exported resource types (by nabi name) that implement
    /// `Drop`.
    drops: BTreeSet<String>,
}

/// The function whose built-ins a future or stream type uses.
#[derive(Clone)]
struct FnCtx {
    /// The core module (`[export]` prefixed for an export's).
    module: String,
    /// The function's name in the canonical ABI.
    name: String,
    /// Its futures and streams, in `wit-parser`'s order.
    payloads: Vec<Type>,
}

/// nabi name of a WIT identifier: `snake_case`, a keyword escaped with
/// a trailing `_`.
fn snake(s: &str) -> String {
    let s = s.to_ascii_lowercase().replace('-', "_");
    let reserved = [
        "in", "pkg", "sup", "type", "const", "static", "extern", "throws", "catch", "where",
        "union", "rethrows", "any", "Self", "str",
    ];
    if nabic_lexer::token::keyword_kind(&s).is_some() || reserved.contains(&s.as_str()) {
        s + "_"
    } else {
        s
    }
}

/// nabi type name of a WIT identifier: `PascalCase`.
fn pascal(s: &str) -> String {
    let mut out = String::new();
    for w in s.split('-') {
        let mut cs = w.chars();
        if let Some(c) = cs.next() {
            out.push(c.to_ascii_uppercase());
            out.extend(cs.map(|c| c.to_ascii_lowercase()));
        }
    }
    out
}

/// A nabi identifier for the pair `(a, b)`: readable, made unique by a
/// hash of the exact strings.
fn ident(prefix: &str, a: &str, b: &str) -> String {
    let mut h: u32 = 0x811c_9dc5;
    for byte in a.bytes().chain([0]).chain(b.bytes()) {
        h = (h ^ u32::from(byte)).wrapping_mul(0x0100_0193);
    }
    let clean: String = format!("{a}_{b}")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("{prefix}_{clean}_{h:08x}")
}

const fn zero(f: Flat) -> &'static str {
    match f {
        Flat::I32 => "0i32",
        Flat::I64 => "0i64",
        Flat::F32 => "0.0f32",
        Flat::F64 => "0.0f64",
    }
}

/// `v` (of flat type `from`) in a variant slot of type `to`.
fn coerce(from: Flat, to: Flat, v: &str) -> String {
    match (from, to) {
        (a, b) if a == b => v.to_owned(),
        (Flat::F32, Flat::I32) => format!("{W}::f32_bits({v})"),
        (Flat::F32, Flat::I64) => format!("({W}::f32_bits({v}) as u32) as i64"),
        (Flat::I32, Flat::I64) => format!("({v} as u32) as i64"),
        _ => format!("{W}::f64_bits({v})"),
    }
}

/// A slot `v` of type `from` back as the case's flat type `to`.
fn uncoerce(from: Flat, to: Flat, v: &str) -> String {
    match (from, to) {
        (a, b) if a == b => v.to_owned(),
        (Flat::I32, Flat::F32) => format!("{W}::f32_from({v})"),
        (Flat::I64, Flat::I32) => format!("{v} as i32"),
        (Flat::I64, Flat::F32) => format!("{W}::f32_from({v} as i32)"),
        _ => format!("{W}::f64_from({v})"),
    }
}

/// Bindings of `requests` for the module at nabi path `base`, in `load`
/// order (so `loaded.roots` lines up); `drops`: exported resources that
/// implement `Drop`.
pub fn bind(
    loaded: &Loaded,
    requests: &[Request],
    base: &str,
    drops: &BTreeSet<String>,
) -> Result<Bindings, String> {
    let r = &loaded.resolve;
    let (imports, exports, funcs, includes) = targets(loaded, requests)?;
    let mut g = Gen {
        r,
        base: base.to_owned(),
        exported: exports.clone(),
        mods: BTreeMap::new(),
        externs: BTreeMap::new(),
        exports: Vec::new(),
        cur: None,
        ops: BTreeSet::new(),
        drops: drops.clone(),
    };
    generate(&mut g, &imports, &exports, &funcs)?;
    Ok(finish(g, &imports, &exports, includes))
}

type Targets = (
    BTreeSet<IfaceId>,
    BTreeSet<IfaceId>,
    Vec<(bool, WorldId, Function)>,
    Vec<WorldId>,
);

/// What `requests` bind: interfaces imported, exported, world functions
/// (`true`: imported) and the worlds the component includes.
fn targets(loaded: &Loaded, requests: &[Request]) -> Result<Targets, String> {
    let r = &loaded.resolve;
    let mut imports: BTreeSet<IfaceId> = BTreeSet::new();
    let mut exports: BTreeSet<IfaceId> = BTreeSet::new();
    let mut funcs: Vec<(bool, WorldId, Function)> = Vec::new();
    let mut includes: Vec<WorldId> = Vec::new();
    let mut root = loaded.roots.iter();
    for req in requests {
        for _ in &req.paths {
            let files = root.next().cloned().unwrap_or_default();
            let ifaces: Vec<IfaceId> = (0..r.interfaces.len())
                .filter(|i| {
                    r.interfaces[*i].inline.is_none() && files.contains(&r.interfaces[*i].file)
                })
                .collect();
            let worlds: Vec<WorldId> = (0..r.worlds.len())
                .filter(|w| files.contains(&r.worlds[*w].file))
                .collect();
            // A world is the component; without one, the interfaces are
            // imported or exported themselves.
            if worlds.is_empty() && req.export {
                exports.extend(ifaces);
            } else if worlds.is_empty() {
                imports.extend(ifaces);
            } else {
                for w in worlds {
                    includes.push(w);
                    for (import, list) in
                        [(true, &r.worlds[w].imports), (false, &r.worlds[w].exports)]
                    {
                        for e in list {
                            match e {
                                WorldEntry::Iface(i) if import => {
                                    imports.insert(*i);
                                }
                                WorldEntry::Iface(i) => {
                                    exports.insert(*i);
                                }
                                WorldEntry::Func(f) => funcs.push((import, w, f.clone())),
                            }
                        }
                    }
                }
            }
        }
    }
    // An interface whose types a bound one `use`s is imported too
    // (unless exported), as `wit-parser` elaborates a world.
    let mut todo: Vec<IfaceId> = imports.iter().chain(&exports).copied().collect();
    while let Some(i) = todo.pop() {
        for (_, t) in &r.interfaces[i].types {
            if let TypeKind::Alias(Type::Id(target)) = r.types[*t].kind {
                if let Owner::Iface(dep) = r.types[target].owner {
                    if dep != i && exports.contains(&dep) && imports.contains(&i) {
                        return Err(format!(
                            "imported interface `{}` uses types of `{}`, which this component exports: an import cannot name the component's own exported types (component model)",
                            r.iface_key(i),
                            r.iface_key(dep)
                        ));
                    }
                    if dep != i && !exports.contains(&dep) && imports.insert(dep) {
                        todo.push(dep);
                    }
                }
            }
        }
    }
    if let Some(i) = imports.intersection(&exports).next() {
        return Err(format!(
            "interface `{}` is both imported and exported",
            r.iface_key(*i)
        ));
    }
    Ok((imports, exports, funcs, includes))
}

/// The declarations and functions of the targets, into `g`.
fn generate(
    g: &mut Gen<'_>,
    imports: &BTreeSet<IfaceId>,
    exports: &BTreeSet<IfaceId>,
    funcs: &[(bool, WorldId, Function)],
) -> Result<(), String> {
    let r = g.r;
    // Types: those of the interfaces bound and what they refer to.
    let mut types = BTreeSet::new();
    for &i in imports.iter().chain(exports) {
        for (_, t) in &r.interfaces[i].types {
            g.reach(&Type::Id(*t), &mut types);
        }
        for f in &r.interfaces[i].funcs {
            for (_, t) in &f.params {
                g.reach(t, &mut types);
            }
            if let Some(t) = &f.result {
                g.reach(t, &mut types);
            }
        }
    }
    for (_, _, f) in funcs {
        for (_, t) in &f.params {
            g.reach(t, &mut types);
        }
        if let Some(t) = &f.result {
            g.reach(t, &mut types);
        }
    }
    for t in types {
        g.type_decl(t)?;
    }
    for &i in imports {
        g.import_iface(i)?;
    }
    for &i in exports {
        g.export_iface(i)?;
    }
    for (import, w, f) in funcs {
        let m = g.owner_mod(Owner::World(*w));
        if *import {
            let text = g.import_fn(&m, "$root", f)?;
            g.push(&m, &text);
        } else {
            let call = g.user_fn(f);
            g.export_fn(&f.name, "[export]$root", f, &call)?;
        }
    }
    Ok(())
}

fn finish(
    g: Gen<'_>,
    imports: &BTreeSet<IfaceId>,
    exports: &BTreeSet<IfaceId>,
    includes: Vec<WorldId>,
) -> Bindings {
    let r = g.r;
    let mut source = String::new();
    print_mods(&g.mods, &g.externs, &mut source);
    let mut world = String::new();
    for &i in imports {
        if r.interfaces[i].inline.is_none() {
            let _ = writeln!(world, "    import {};", r.iface_key(i));
        }
    }
    for &i in exports {
        if r.interfaces[i].inline.is_none() {
            let _ = writeln!(world, "    export {};", r.iface_key(i));
        }
    }
    for w in includes {
        let p = &r.packages[r.worlds[w].pkg].name;
        let v = p
            .version
            .as_ref()
            .map(|v| format!("@{v}"))
            .unwrap_or_default();
        let _ = writeln!(
            world,
            "    include {}:{}/{}{v};",
            p.ns, p.name, r.worlds[w].name
        );
    }
    let mut exports = g.exports;
    exports.push("cabi_realloc".into());
    Bindings {
        source,
        exports,
        world,
    }
}

type Externs = BTreeMap<Vec<String>, BTreeMap<String, BTreeSet<String>>>;

/// The modules as nested `pub mod` items.
fn print_mods(mods: &BTreeMap<Vec<String>, String>, externs: &Externs, out: &mut String) {
    let mut paths: BTreeSet<Vec<String>> = mods.keys().cloned().collect();
    paths.extend(externs.keys().cloned());
    // Every prefix is a module too.
    for p in paths.clone() {
        for n in 1..p.len() {
            paths.insert(p[..n].to_vec());
        }
    }
    print_mod(&[], &paths, mods, externs, out);
}

/// Module `at` and the modules below it.
fn print_mod(
    at: &[String],
    paths: &BTreeSet<Vec<String>>,
    mods: &BTreeMap<Vec<String>, String>,
    externs: &Externs,
    out: &mut String,
) {
    if let Some(name) = at.last() {
        let _ = writeln!(out, "pub mod {name} {{");
    }
    for (module, decls) in externs.get(at).into_iter().flatten() {
        let _ = writeln!(
            out,
            "#[link(wasm_import_module = \"{module}\")]\nextern \"C\" {{"
        );
        for d in decls {
            out.push_str(d);
        }
        out.push_str("}\n");
    }
    if let Some(text) = mods.get(at) {
        out.push_str(text);
    }
    for p in paths
        .iter()
        .filter(|p| p.len() == at.len() + 1 && p.starts_with(at))
    {
        print_mod(p, paths, mods, externs, out);
    }
    if !at.is_empty() {
        out.push_str("}\n");
    }
}
