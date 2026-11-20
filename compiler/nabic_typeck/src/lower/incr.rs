//! Incremental body lowering: a declaration's lowered body, with
//! everything its lowering added to the shared state (closures,
//! instances, type instances, `any` tags, warnings), is kept as a unit
//! and replayed in a later build instead of lowered again.
//!
//! A unit is keyed by `global_key` (target, flags, operators, lang slots)
//! and the declaration with its body and positions. Every read a lowering
//! makes is logged: interface reads (`Read`) with a fingerprint of the
//! answer, shared-state reads (`Lookup`) with the answer. A unit replays
//! when every read answers alike and the files its spans point into are
//! unchanged; the functions, types and inference variables it made are
//! renumbered from this build's counters. A miss lowers again.
//!
//! Only plain bodies (not generic, `const` or a trait default) make units.
//! An inferred return type stays in the interface (`interface_key`) and
//! the unit records it. A lowering that errors or touches unlogged state
//! makes no unit.

mod interface;
mod log;
mod record;
mod replay;

pub(crate) use crate::lower::incr::interface::*;
pub use crate::lower::incr::log::*;

use crate::lower::expr::AnyDispatch;
use crate::lower::{
    AliasUse, Env, FnSig, GenericMethod, MethodInfo, MethodOrFn, RefOut, TraitInfo, mangle_inst,
};
use crate::{BodyReuse, EnumId, HirEnumDef, HirFunction, HirStructDef, HirTy, StructId};
use nabic_ast::{FnDef, ImplItem, Item, Program};
use nabic_core::codec::{Codec, Dec, Enc, IdKind};
use nabic_core::{HashMap, HashSet, codec_enum, codec_struct};
use nabic_diagnostic::{NabiDiagnostic, Span};
use nabic_ir_common::FunctionId;

/// Where an embedder keeps units between builds, and the errors of
/// generic body checks (`bounds`).
pub trait BodyCache {
    fn get(&self, key: u64) -> Option<Unit>;
    fn put(&self, key: u64, unit: &Unit);
    fn get_check(&self, key: u64) -> Option<Vec<NabiDiagnostic>>;
    fn put_check(&self, key: u64, errors: &[NabiDiagnostic]);
}

/// A `BodyCache` for `HirInput::bodies`.
#[derive(Clone)]
pub struct Bodies(pub std::sync::Arc<dyn BodyCache + Send + Sync>);

impl std::fmt::Debug for Bodies {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Bodies")
    }
}

/// One declaration's lowering and what it added: what a replay checks
/// (read with the ids as they were), and the rest (read renumbered).
pub struct Unit {
    head: Head,
    body: Vec<u8>,
    /// The files `body`'s spans name (encoding), or the old -> new file
    /// ids to read it with (decoding).
    files: Vec<u32>,
}

impl Codec for Unit {
    fn enc(&self, e: &mut Enc) {
        self.head.enc(e);
        e.bytes(&self.body);
        for f in &self.files {
            if !e.files.contains(f) {
                e.files.push(*f);
            }
        }
    }
    fn dec(d: &mut Dec<'_>) -> Option<Self> {
        let head = Head::dec(d)?;
        let body = d.bytes()?.to_vec();
        Some(Self {
            head,
            body,
            files: d.files.map(<[u32]>::to_vec).unwrap_or_default(),
        })
    }
}

thread_local! {
    static REC: std::cell::RefCell<Option<Rec>> = const { std::cell::RefCell::new(None) };
}

/// The incremental state of one `lower_full`.
pub(crate) struct Incr<'a> {
    cache: &'a dyn BodyCache,
    /// `interface_key` (generic body checks).
    interface: u64,
    /// `global_key` (units).
    global: u64,
    /// Per declaration: plain (see the module doc).
    plain: Vec<bool>,
    /// The kept library stage's key and the declarations it made (the
    /// first ones): theirs are named by it.
    library: Option<(u64, usize)>,
    /// Fingerprints of the reads answered so far (by encoding).
    answers: std::cell::RefCell<HashMap<Vec<u8>, Option<u64>>>,
    /// Fingerprints of the impls by index (`Impls::all`), once taken.
    impl_fps: std::cell::RefCell<HashMap<usize, Option<u64>>>,
    /// The generic impls by method, and every impl by associated const.
    impls_by: std::cell::OnceCell<[HashMap<String, Vec<usize>>; 2]>,
    /// `fn_table` keys by `stable_name`, where it differs (`None`: two).
    raw_keys: std::cell::OnceCell<HashMap<String, Option<String>>>,
    /// Per file: each line's end and the hash of the text up to it.
    lines: std::cell::RefCell<HashMap<u32, Vec<(u32, u64)>>>,
    names: std::cell::RefCell<Names>,
    pub reuse: std::cell::Cell<BodyReuse>,
}

/// The names of the functions and types made so far, both ways (a name
/// given twice names neither).
#[derive(Default)]
pub(crate) struct Names {
    /// Functions, structs, enums looked at so far.
    done: [usize; 3],
    decls: bool,
    by_id: HashMap<(u8, u32), String>,
    by_name: HashMap<(u8, String), Option<u32>>,
}

/// `name` with each type id `ty_mangle` writes in the target of a
/// method key (`Struct12` in `Struct12__len`, `Enum3`) replaced by
/// `f(enum, id)` (`None`: no name).
fn map_type_ids(name: &str, f: impl Fn(bool, u32) -> Option<String>) -> Option<String> {
    let Some(head) = name.find("__") else {
        return Some(name.to_owned());
    };
    let mut out = String::with_capacity(name.len());
    let mut rest = &name[..head];
    while let Some(i) = rest
        .find("Struct")
        .into_iter()
        .chain(rest.find("Enum"))
        .min()
    {
        let enum_ = rest[i..].starts_with("Enum");
        let tag = if enum_ { "Enum" } else { "Struct" };
        let digits = rest[i + tag.len()..]
            .bytes()
            .take_while(u8::is_ascii_digit)
            .count();
        let end = i + tag.len() + digits;
        out.push_str(&rest[..i]);
        match rest[i + tag.len()..end].parse::<u32>() {
            Ok(id) => out.push_str(&f(enum_, id)?),
            Err(_) => out.push_str(&rest[i..end]),
        }
        rest = &rest[end..];
    }
    out.push_str(rest);
    out.push_str(&name[head..]);
    Some(out)
}

/// `name` with the types its method key names by id named by their own
/// names: alike in every build.
fn stable_name(name: &str) -> String {
    map_type_ids(name, |enum_, id| {
        Some(crate::type_name(enum_, id).map_or_else(
            || format!("{}{id}", if enum_ { "Enum" } else { "Struct" }),
            |t| format!("{{{t}}}"),
        ))
    })
    .unwrap_or_default()
}

impl Names {
    fn update(&mut self, env: &Env<'_>) {
        let mut add = |k: u8, id: u32, name: String| {
            let name = if k == 0 { stable_name(&name) } else { name };
            self.by_name
                .entry((k, name.clone()))
                .and_modify(|v| *v = None)
                .or_insert(Some(id));
            self.by_id.insert((k, id), name);
        };
        // The declarations, by `fn_table` key.
        if !self.decls {
            self.decls = true;
            let mut keys: Vec<(&FunctionId, &String)> = env.key_of.iter().collect();
            keys.sort();
            for (id, k) in keys {
                add(0, id.0, k.clone());
            }
        }
        let extras = env.sink.extras.borrow();
        for f in extras.iter().skip(self.done[0]) {
            add(0, f.id.0, f.name.clone());
        }
        self.done[0] = extras.len();
        for (k, len) in [
            (1u8, env.defs.structs.borrow().len()),
            (2, env.defs.enums.borrow().len()),
        ] {
            let from = self.done[usize::from(k)];
            for id in from..len {
                let id = u32::try_from(id).unwrap_or(u32::MAX);
                if let Some(name) = crate::type_name(k == 2, id) {
                    add(k, id, name);
                }
            }
            self.done[usize::from(k)] = len;
        }
    }

    fn name_of(&self, k: u8, id: u32) -> Option<&str> {
        self.by_id.get(&(k, id)).map(String::as_str)
    }

    fn id_of(&self, k: u8, name: &str) -> Option<u32> {
        self.by_name.get(&(k, name.to_owned())).copied().flatten()
    }
}

impl<'a> Incr<'a> {
    pub(crate) fn new(
        env: &Env<'_>,
        cache: &'a dyn BodyCache,
        programs: &[&Program],
        raw_uses: &[(String, nabic_core::Visibility, nabic_ast::UseTree)],
        input: &crate::HirInput,
        lib_decls: usize,
    ) -> Self {
        let plain: Vec<bool> = (0..env.decls.len()).map(|i| is_plain(env, i)).collect();
        // An inferred return is part of the interface (callers and
        // generic bodies read it): its body stays in the key.
        let bodies: HashSet<(&str, Span)> = env
            .decls
            .iter()
            .zip(&plain)
            .enumerate()
            .filter(|(i, (_, p))| {
                **p && !env
                    .infer_ret
                    .contains(&FunctionId(u32::try_from(*i).unwrap_or(u32::MAX)))
            })
            .map(|(_, (d, _))| (d.def().name.as_str(), d.def().span))
            .collect();
        let interface = interface_key(env, &bodies, programs, raw_uses, input);
        Self {
            cache,
            interface,
            global: global_key(env, input),
            plain,
            library: input.library.map(|k| (k, lib_decls)),
            answers: std::cell::RefCell::new(HashMap::default()),
            impl_fps: std::cell::RefCell::new(HashMap::default()),
            impls_by: std::cell::OnceCell::new(),
            raw_keys: std::cell::OnceCell::new(),
            lines: std::cell::RefCell::new(HashMap::default()),
            names: std::cell::RefCell::new(Names::default()),
            reuse: std::cell::Cell::new(BodyReuse::default()),
        }
    }

    /// The end of the line of file `id` that offset `end` is on (`exact`:
    /// `end` must be one), and the hash of the text up to it.
    fn prefix(&self, env: &Env<'_>, id: u32, end: u32, exact: bool) -> Option<(u32, u64)> {
        let mut lines = self.lines.borrow_mut();
        let table = lines.entry(id).or_insert_with(|| {
            let Some((_, text)) = env.sources.file(id) else {
                return Vec::new();
            };
            let (mut h, mut at) = (0u64, 0usize);
            let mut out = Vec::new();
            for line in text.split_inclusive('\n') {
                h = nabic_core::hash::fast_add(h, line.as_bytes());
                at += line.len();
                out.push((u32::try_from(at).unwrap_or(u32::MAX), h));
            }
            out
        });
        let k = table.partition_point(|(at, _)| *at < end);
        let found = *table.get(k)?;
        (!exact || found.0 == end).then_some(found)
    }

    /// The errors of checking generic body `f` (`owner`: its impl's
    /// target or its trait), from the store or from `check`. They follow
    /// from the interface and the body (positions in), so they are kept
    /// unless one points outside the body.
    pub(crate) fn checked(
        &self,
        env: &Env<'_>,
        f: &FnDef,
        owner: &[u8],
        check: impl FnOnce() -> Vec<NabiDiagnostic>,
    ) -> Vec<NabiDiagnostic> {
        let mut e = Enc::new(true);
        f.enc(&mut e);
        let path = env.sources.path(f.span.file).unwrap_or_default();
        let key = [b"check".as_slice(), path.as_bytes(), owner, &e.buf]
            .iter()
            .fold(self.interface, |h, part| {
                nabic_core::hash::fast_add(h, part)
            });
        if let Some(errors) = self.cache.get_check(key) {
            return errors;
        }
        let errors = check();
        let inside =
            |s: &Span| s.file == f.span.file && s.start >= f.span.start && s.end <= f.span.end;
        let local = errors.iter().all(|d| {
            d.primary
                .iter()
                .map(|l| &l.span)
                .chain(d.secondary.iter().map(|l| &l.span))
                .all(inside)
        });
        if local {
            self.cache.put_check(key, &errors);
        }
        errors
    }

    /// Lower declaration `i` (not a stub): replayed from its unit, or
    /// lowered and kept as one.
    pub(crate) fn lower(&self, env: &Env<'_>, i: usize) -> HirFunction {
        if !self.plain[i] {
            return crate::lower::collect::lower_decl(env, i, false);
        }
        let def = env.decls[i].def();
        let mut e = Enc::new(true);
        match self.library {
            // The stage's key names its declarations.
            Some((k, n)) if i < n => {
                e.u(k);
                e.u(i as u64);
            }
            _ => def.enc(&mut e),
        }
        let path = env.sources.path(def.span.file).unwrap_or_default();
        let id = FunctionId(u32::try_from(i).unwrap_or(u32::MAX));
        let name = env
            .key_of
            .get(&id)
            .map(|k| stable_name(k))
            .unwrap_or_default();
        let key = [name.as_bytes(), path.as_bytes(), &e.buf]
            .iter()
            .fold(self.global, |h, part| nabic_core::hash::fast_add(h, part));
        let mut r = self.reuse.get();
        r.lowered += 1;
        if let Some(f) = self.cache.get(key).and_then(|u| self.replay(env, i, &u)) {
            r.replayed += 1;
            self.reuse.set(r);
            return f;
        }
        self.reuse.set(r);
        let start = counts(env);
        let before = Sizes::of(env);
        REC.with(|r| {
            *r.borrow_mut() = Some(Rec {
                lookups: Vec::new(),
                seen: HashSet::default(),
                reads: Vec::new(),
                read_seen: HashSet::default(),
                warns: Vec::new(),
                touched: Vec::new(),
                keys: Vec::new(),
            });
        });
        let f = crate::lower::collect::lower_decl(env, i, false);
        let rec = REC.with(|r| r.borrow_mut().take());
        let unit = rec.and_then(|rec| self.record(env, i, &f, start, &before, rec));
        if let Some(unit) = unit {
            self.cache.put(key, &unit);
        }
        f
    }
}

/// The name declaration `id` is lowered under: a method's key may name
/// types by id (`Struct12__len`), so a replayed one takes this build's.
fn decl_name(env: &Env<'_>, id: FunctionId) -> Option<String> {
    Some(match env.decls.get(id.0 as usize)? {
        MethodOrFn::Fn(f) => f.name.clone(),
        MethodOrFn::Method { key, .. } => key.clone(),
    })
}

/// The id of no function (a signature made up for a call).
const NO_ID: u32 = u32::MAX;

const fn kind_tag(k: IdKind) -> u8 {
    match k {
        IdKind::Function => 0,
        IdKind::Struct => 1,
        IdKind::Enum => 2,
        IdKind::InferVar => 3,
    }
}

/// Set or remove the entry at `k`.
fn set<K: std::hash::Hash + Eq, V>(map: &mut HashMap<K, V>, k: K, v: Option<V>) {
    match v {
        Some(v) => {
            map.insert(k, v);
        }
        None => {
            map.remove(&k);
        }
    }
}

fn counts(env: &Env<'_>) -> Counts {
    let n = |x: usize| u32::try_from(x).unwrap_or(u32::MAX);
    Counts {
        next_id: env.sink.next_id.get(),
        structs: n(env.defs.structs.borrow().len()),
        enums: n(env.defs.enums.borrow().len()),
        any_tags: n(env.sink.any_tags.borrow().len()),
        any_dispatch: n(env.sink.any_dispatch.borrow().len()),
        extras: n(env.sink.extras.borrow().len()),
        infer_vars: n(env.sink.infer_vars.borrow().len()),
    }
}

/// Sizes of the shared tables before a lowering.
pub(crate) struct Sizes {
    diagnostics: usize,
    name_diags: usize,
    alias_uses: usize,
    unknown_spans: usize,
    uninferred: u32,
    inferred_fns: usize,
    mono_cache: usize,
    mono_sigs: usize,
    ref_out: usize,
    thunks: usize,
    fn_origin: usize,
    track_caller: usize,
    inferred_ret: usize,
    type_descs: u32,
    params: usize,
}

impl Sizes {
    fn of(env: &Env<'_>) -> Self {
        let sink = &env.sink;
        Self {
            diagnostics: sink.diagnostics.borrow().len(),
            name_diags: env.names.diags.borrow().len(),
            alias_uses: env.names.alias_uses.borrow().len(),
            unknown_spans: env.names.unknown_spans.borrow().len(),
            uninferred: sink.uninferred.get(),
            inferred_fns: sink.inferred_fns.borrow().len(),
            mono_cache: sink.mono_cache.borrow().len(),
            mono_sigs: sink.mono_sigs.borrow().len(),
            ref_out: sink.ref_out.borrow().len(),
            thunks: sink.thunks.borrow().len(),
            fn_origin: sink.fn_origin.borrow().len(),
            track_caller: sink.track_caller.borrow().len(),
            inferred_ret: sink.inferred_ret.borrow().len(),
            type_descs: crate::lower::reflect::desc::uses(),
            params: sink.params.borrow().len(),
        }
    }
}
