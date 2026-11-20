//! A linked `wasm32-wasip2` core module to a WASI preview 2 component,
//! in place of `wasm-component-ld`.
//!
//! The worlds come from the core module's `component-type:*` custom
//! sections (wasi-libc's), the adapter's and the program's WIT
//! directory; semver-compatible interfaces merge into one import under
//! the newest version. The component:
//!   - imports the interfaces whose functions and types the core
//!     modules use;
//!   - instantiates a shim of `call_indirect` stubs, the main module, the
//!     preview 1 adapter (reduced to the functions the main module
//!     imports), then fills the shim's table with what needed the main
//!     module's memory or `cabi_realloc` (lowered imports, the adapter's
//!     exports, resource destructors);
//!   - runs `_initialize` when the main module exports it;
//!   - lifts the world's exports (and, for a command, the adapter's
//!     `wasi:cli/run`), interfaces through a nested component that gives
//!     their types names.

mod bin;
mod core;
mod decode;
mod define;
mod emit;
mod encode;
mod gc;
#[cfg(test)]
mod tests;
mod types;

use self::bin::Res;
use self::core::{FuncType, ImportKind, Module};
use self::encode::{Builder, CORE_FUNC, CORE_MEMORY, CORE_TABLE, FUNC, INSTANCE, TYPE, Vt};
use self::types::{Def, Func, Groups, Model, ROOT, Tid, Val, World};
use crate::parse::Prim;
use crate::{Overlay, Request};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;

const P1: &str = "wasi_snapshot_preview1";
const MAX_FLAT_PARAMS: usize = crate::abi::MAX_FLAT_PARAMS;
const MAX_FLAT_ASYNC_PARAMS: usize = 4;

/// The component of linked core module `core`.
///
/// `core` is linked for `wasm32-wasip2`: it exports `memory`,
/// `cabi_realloc` and, for a command, `_start`. `adapter` is the
/// `wasi_snapshot_preview1` adapter (command or reactor, matching
/// `reactor`) serving its preview 1 imports; `wit` the directory
/// (`component.wit` and `deps/`) of the program's own world. A
/// `reactor` exports no `wasi:cli/run` of the adapter: the host calls
/// its exports.
pub fn componentize(
    core: &[u8],
    adapter: &[u8],
    wit: Option<&Path>,
    reactor: bool,
) -> Result<Vec<u8>, String> {
    let main = Module::parse(core).map_err(|e| format!("core module: {e}"))?;
    let mut m = Model::default();
    let unit = [m.ty(Def::Stream(None)), m.ty(Def::Future(None))];
    read_worlds(&mut m, &main, "core module")?;
    let program = wit.map(|d| load_world(&mut m, d)).transpose()?;
    if !reactor && main.export_func("_start").is_none() {
        return Err("a command's core module must export `_start`".to_owned());
    }
    let adapter_mod = if !reactor || main.imports.iter().any(|i| i.module == P1) {
        Some(Module::parse(adapter).map_err(|e| format!("adapter: {e}"))?)
    } else {
        None
    };
    let adapter_world = match &adapter_mod {
        Some(a) => read_worlds(&mut m, a, "adapter")?.into_iter().next(),
        None => None,
    };
    if !reactor && adapter_world.is_none() {
        return Err("the command adapter carries no world (no `wasi:cli/run`)".to_owned());
    }
    let groups = Groups::new(&m);

    // What is exported: the program's world, and the command adapter's.
    let mut exports: Vec<(usize, Inst)> = Vec::new();
    if let Some(w) = &program {
        exports.extend(w.exports.iter().map(|&i| (i, Inst::Main)));
    }
    if let Some(w) = adapter_world.as_ref().filter(|_| !reactor) {
        exports.extend(w.exports.iter().map(|&i| (i, Inst::Adapter)));
    }
    let mut exported = HashSet::default();
    for &(i, _) in &exports {
        if !exported.insert(groups.of[i]) {
            return Err(format!("`{}` is exported twice", m.ifaces[i].key));
        }
    }
    let gc_bytes = adapter_mod
        .as_ref()
        .map(|a| reduce_adapter(&m, &main, a, adapter, &exports))
        .transpose()?;
    let gc_mod = gc_bytes.as_deref().map(Module::parse).transpose()?;

    let root_exports = program.map(|w| w.root_exports).unwrap_or_default();
    let mut e = Enc::new(&m, &groups, unit, exported, root_exports);
    e.main_exports = main.exports.iter().map(|x| x.name.clone()).collect();
    if let Some(a) = &gc_mod {
        e.adapter_exports = a.exports.iter().map(|x| x.name.clone()).collect();
    }
    e.plan_module(&main, Inst::Main)?;
    if let Some(a) = &gc_mod {
        e.plan_module(a, Inst::Adapter)?;
    }
    e.plan_exports(&main, &exports)?;
    e.emit_imports()?;
    e.instantiate(&main, gc_mod.as_ref())?;
    for &(i, by) in &exports {
        e.export_iface(i, by)?;
    }
    for f in e.root_exports.clone() {
        let lifted = e.lift(&f, &f.name, Inst::Main)?;
        e.b.export(&f.name, FUNC, lifted, None);
    }
    Ok(e.b.finish())
}

/// The worlds of `md`'s `component-type:*` sections, into `m`.
fn read_worlds(m: &mut Model, md: &Module<'_>, what: &str) -> Res<Vec<World>> {
    let mut out = Vec::new();
    for (name, body) in &md.customs {
        if name.starts_with("component-type") {
            out.extend(decode::worlds(m, body).map_err(|e| format!("{what} `{name}`: {e}"))?);
        }
    }
    Ok(out)
}

/// The adapter with only the exports the main module imports and those
/// it lifts (`wasi:cli/run` for a command).
fn reduce_adapter(
    m: &Model,
    main: &Module<'_>,
    a: &Module<'_>,
    bytes: &[u8],
    exports: &[(usize, Inst)],
) -> Res<Vec<u8>> {
    let mut keep: BTreeSet<String> = main
        .imports
        .iter()
        .filter(|i| i.module == P1)
        .map(|i| i.name.clone())
        .collect();
    for &(i, by) in exports {
        if by == Inst::Adapter {
            for f in &m.ifaces[i].funcs {
                keep.insert(format!("{}#{}", m.ifaces[i].key, f.name));
            }
        }
    }
    for r in ["cabi_import_realloc", "cabi_export_realloc"] {
        if a.export_func(r).is_some() {
            keep.insert(r.to_owned());
        }
    }
    gc::run(bytes, &keep).map_err(|e| format!("adapter: {e}"))
}

/// The world of WIT directory `dir`: the one its own package defines
/// (`component` when several).
fn load_world(m: &mut Model, dir: &Path) -> Res<World> {
    let req = Request {
        export: false,
        paths: vec![dir.to_owned()],
    };
    let loaded = crate::load(&[req], &Overlay::default()).map_err(|e| e.to_string())?;
    let r = &loaded.resolve;
    let files = loaded.roots.first().cloned().unwrap_or_default();
    let mine: Vec<usize> = (0..r.worlds.len())
        .filter(|&w| files.contains(&r.worlds[w].file))
        .collect();
    let w = mine
        .iter()
        .copied()
        .find(|&w| r.worlds[w].name == "component")
        .or_else(|| (mine.len() == 1).then(|| mine[0]))
        .ok_or_else(|| format!("{}: no world (or several)", dir.display()))?;
    types::from_resolve(m, r, w)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Inst {
    Main,
    Adapter,
}

/// Canonical options, the realloc being the importer's: independent
/// flags.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy, Debug, Default)]
struct Opts {
    memory: bool,
    realloc: bool,
    utf8: bool,
    async_: bool,
}

#[derive(Clone, Debug)]
enum Op {
    /// `canon lower` of function `name` of group `g` (the world's own
    /// when `g` is the root group).
    Lower {
        g: usize,
        name: String,
        opts: Opts,
    },
    ResDrop(Tid),
    ResNew(Tid),
    ResRep(Tid),
    TaskReturn(Option<Val>, Opts),
    /// A `canon` built-in of no type and no options.
    Simple(Vec<u8>),
    /// One followed by the memory index (`waitable-set.wait`).
    WithMem(Vec<u8>),
    WithOpts(Vec<u8>, Opts),
    /// A future or stream built-in: opcode, type, options, async flag.
    Payload {
        code: u8,
        ty: Tid,
        opts: Option<Opts>,
        flag: Option<bool>,
    },
    /// A core export of an instance.
    Export(Inst, String),
}

impl Op {
    /// Needs what the main module or the adapter defines.
    const fn late(&self) -> bool {
        match self {
            Self::Lower { opts, .. } | Self::TaskReturn(_, opts) => opts.memory || opts.realloc,
            Self::Payload { opts, .. } => opts.is_some(),
            Self::WithMem(_) | Self::WithOpts(..) | Self::Export(..) => true,
            Self::ResDrop(_) | Self::ResNew(_) | Self::ResRep(_) | Self::Simple(_) => false,
        }
    }
}

enum Special {
    Memory,
    Main(String),
}

/// One import of a core module and what satisfies it.
struct Bind {
    module: String,
    name: String,
    by: Inst,
    op: Op,
    slot: Option<usize>,
    special: Option<Special>,
}

impl Bind {
    const fn special(module: String, name: String, s: Special) -> Self {
        Self {
            module,
            name,
            by: Inst::Adapter,
            op: Op::Simple(Vec::new()),
            slot: None,
            special: Some(s),
        }
    }
}

type Key = (usize, String);

/// A type index space being written.
enum Space {
    Outer,
    /// An imported interface's instance type.
    Inst(InstSp),
    /// The component giving an exported interface's types names.
    Nest(Box<Nest>),
}

struct InstSp {
    group: usize,
    decls: Vec<u8>,
    n: usize,
    types: u32,
    named: HashMap<Key, u32>,
    anon: HashMap<Vec<u8>, u32>,
}

struct Nest {
    group: usize,
    b: Builder,
    /// Exporting: the interface's own types resolve to their exports.
    exporting: bool,
    imports: HashMap<Key, u32>,
    exports: HashMap<Key, u32>,
    anon: HashMap<Vec<u8>, u32>,
    names: HashSet<String>,
    /// Instantiation arguments: name, sort, outer index.
    args: Vec<(String, u8, u32)>,
}

impl Nest {
    fn uniq(&mut self, base: &str) -> String {
        let mut n = base.to_owned();
        let mut i = 0;
        while !self.names.insert(n.clone()) {
            n = format!("{base}{i}");
            i += 1;
        }
        n
    }
}

struct Enc<'a> {
    m: &'a Model,
    g: &'a Groups,
    root: Option<usize>,
    /// `stream` and `future` without payload.
    unit: [Tid; 2],
    exported: HashSet<usize>,
    root_exports: Vec<Func>,
    live_funcs: BTreeMap<usize, BTreeSet<String>>,
    live_types: BTreeMap<usize, BTreeSet<String>>,
    b: Builder,
    outer_named: HashMap<Key, u32>,
    outer_anon: HashMap<Vec<u8>, u32>,
    imported: HashMap<usize, u32>,
    importing: HashSet<usize>,
    root_funcs: HashMap<String, u32>,
    /// Shim slot of each exported resource's destructor.
    dtors: HashMap<Key, usize>,
    shim: Option<u32>,
    main: Option<u32>,
    adapter: Option<u32>,
    mem: Option<u32>,
    reallocs: HashMap<(Inst, &'static str), u32>,
    main_exports: HashSet<String>,
    adapter_exports: HashSet<String>,
    binds: Vec<Bind>,
    /// Shim slots: core type, what fills it, for whom.
    slots: Vec<(FuncType, Op, Inst)>,
}

const fn prim_byte(p: Prim) -> u8 {
    match p {
        Prim::Bool => 0x7f,
        Prim::S8 => 0x7e,
        Prim::U8 => 0x7d,
        Prim::S16 => 0x7c,
        Prim::U16 => 0x7b,
        Prim::S32 => 0x7a,
        Prim::U32 => 0x79,
        Prim::S64 => 0x78,
        Prim::U64 => 0x77,
        Prim::F32 => 0x76,
        Prim::F64 => 0x75,
        Prim::Char => 0x74,
        Prim::String => 0x73,
    }
}

/// A type import or export's bound: `eq` to type `bound`, else a
/// resource.
fn type_bound(bound: Option<u32>) -> Vec<u8> {
    let mut d = vec![0x03];
    match bound {
        Some(i) => {
            d.push(0);
            bin::u32(&mut d, i);
        }
        None => d.push(1),
    }
    d
}

/// Name of the import of function `wit` in an exported interface's
/// component (`wit-component`'s).
fn import_func_name(wit: &str) -> String {
    let kebab = |s: &str| s.replace('.', "-");
    if let Some(r) = wit.strip_prefix("[constructor]") {
        format!("import-constructor-{r}")
    } else if let Some(r) = wit.strip_prefix("[method]") {
        format!("import-method-{}", kebab(r))
    } else if let Some(r) = wit.strip_prefix("[static]") {
        format!("import-static-{}", kebab(r))
    } else {
        format!("import-func-{wit}")
    }
}

/// `[stream-read-0]f` and the like: kind, operation, payload index
/// (`None`: `-unit`), function.
fn payload_name(name: &str) -> Option<(bool, &str, Option<usize>, &str)> {
    let rest = name.strip_prefix('[')?;
    let (inner, func) = rest.split_once(']')?;
    let (future, op) = if let Some(op) = inner.strip_prefix("future-") {
        (true, op)
    } else {
        (false, inner.strip_prefix("stream-")?)
    };
    let (op, idx) = op.rsplit_once('-')?;
    let idx = if idx == "unit" {
        None
    } else {
        Some(idx.parse().ok()?)
    };
    Some((future, op, idx, func))
}

/// A `$root` built-in of the async ABI.
fn intrinsic(name: &str) -> Option<Res<Op>> {
    let simple = |b: &[u8]| Some(Ok(Op::Simple(b.to_vec())));
    let ec = |b: u8, realloc| {
        Some(Ok(Op::WithOpts(
            vec![b],
            Opts {
                memory: true,
                realloc,
                utf8: true,
                async_: false,
            },
        )))
    };
    match name {
        "[waitable-set-new]" => simple(&[0x1f]),
        "[waitable-set-wait]" => Some(Ok(Op::WithMem(vec![0x20, 0]))),
        "[cancellable][waitable-set-wait]" => Some(Ok(Op::WithMem(vec![0x20, 1]))),
        "[waitable-set-poll]" => Some(Ok(Op::WithMem(vec![0x21, 0]))),
        "[cancellable][waitable-set-poll]" => Some(Ok(Op::WithMem(vec![0x21, 1]))),
        "[waitable-set-drop]" => simple(&[0x22]),
        "[waitable-join]" => simple(&[0x23]),
        "[subtask-drop]" => simple(&[0x0d]),
        "[subtask-cancel]" => simple(&[0x06, 0]),
        "[async-lower][subtask-cancel]" => simple(&[0x06, 1]),
        "[backpressure-set]" => simple(&[0x08]),
        "[backpressure-inc]" => simple(&[0x24]),
        "[backpressure-dec]" => simple(&[0x25]),
        "[thread-yield]" => simple(&[0x0c, 0]),
        "[cancellable][thread-yield]" => simple(&[0x0c, 1]),
        "[error-context-new-utf8]" => ec(0x1c, false),
        "[error-context-debug-message-utf8]" => ec(0x1d, true),
        "[error-context-drop]" => simple(&[0x1e]),
        _ => {
            let n = name
                .strip_prefix("[context-get-")
                .map(|n| (0x0a, n))
                .or_else(|| name.strip_prefix("[context-set-").map(|n| (0x0b, n)))?;
            let i = n.1.strip_suffix(']')?.parse::<u32>().ok()?;
            let mut b = vec![n.0, 0x7f];
            bin::u32(&mut b, i);
            Some(Ok(Op::Simple(b)))
        }
    }
}

impl<'a> Enc<'a> {
    fn new(
        m: &'a Model,
        g: &'a Groups,
        unit: [Tid; 2],
        exported: HashSet<usize>,
        root_exports: Vec<Func>,
    ) -> Self {
        Self {
            m,
            g,
            root: g.find(ROOT),
            unit,
            exported,
            root_exports,
            live_funcs: BTreeMap::new(),
            live_types: BTreeMap::new(),
            b: Builder::default(),
            outer_named: HashMap::default(),
            outer_anon: HashMap::default(),
            imported: HashMap::default(),
            importing: HashSet::default(),
            root_funcs: HashMap::default(),
            dtors: HashMap::default(),
            shim: None,
            main: None,
            adapter: None,
            mem: None,
            reallocs: HashMap::default(),
            main_exports: HashSet::default(),
            adapter_exports: HashSet::default(),
            binds: Vec::new(),
            slots: Vec::new(),
        }
    }

    /// Plan every import of `md`: bound before the main module exists,
    /// or through a shim slot filled after.
    fn plan_module(&mut self, md: &Module<'_>, by: Inst) -> Res<()> {
        for imp in &md.imports {
            let (module, name) = (imp.module.clone(), imp.name.clone());
            let ty = match imp.kind {
                ImportKind::Func(t) => {
                    md.types.get(t as usize).cloned().ok_or("bad import type")?
                }
                ImportKind::Other(core::MEMORY)
                    if by == Inst::Adapter && module == "env" && name == "memory" =>
                {
                    self.binds
                        .push(Bind::special(module, name, Special::Memory));
                    continue;
                }
                ImportKind::Other(_) => {
                    return Err(format!("core import `{module}` `{name}` is not a function"));
                }
            };
            if by == Inst::Adapter && module == "__main_module__" {
                if !self.main_exports.contains(&name) {
                    return Err(format!(
                        "the adapter needs the main module to export `{name}`"
                    ));
                }
                let s = Special::Main(name.clone());
                self.binds.push(Bind::special(module, name, s));
                continue;
            }
            let op = self
                .plan(&module, &name, by)
                .map_err(|err| format!("core import `{module}` `{name}`: {err}"))?;
            let slot = if op.late() {
                self.slots.push((ty, op.clone(), by));
                Some(self.slots.len() - 1)
            } else {
                None
            };
            self.binds.push(Bind {
                module,
                name,
                by,
                op,
                slot,
                special: None,
            });
        }
        Ok(())
    }

    /// Mark what the exports use; a shim slot for each exported
    /// resource's destructor.
    fn plan_exports(&mut self, main: &Module<'_>, exports: &[(usize, Inst)]) -> Res<()> {
        let m = self.m;
        for &(i, by) in exports {
            for f in &m.ifaces[i].funcs {
                self.mark_func(f);
            }
            for (name, t) in &m.ifaces[i].types {
                self.mark(&Val::Id(*t));
                let dtor = format!("{}#[dtor]{name}", m.ifaces[i].key);
                let resource = matches!(m.tys[*t].def, Def::Resource);
                match main.export_func(&dtor) {
                    Some(f) if resource && by == Inst::Main => {
                        let ty = main.func_type(f).cloned().ok_or("bad export")?;
                        self.slots
                            .push((ty, Op::Export(Inst::Main, dtor), Inst::Main));
                        self.dtors
                            .insert((self.g.of[i], name.clone()), self.slots.len() - 1);
                    }
                    _ => {}
                }
            }
        }
        for f in self.root_exports.clone() {
            self.mark_func(&f);
        }
        Ok(())
    }

    /// The imports: every interface (and world function and type) used.
    fn emit_imports(&mut self) -> Res<()> {
        let groups = self.g;
        for g in 0..groups.groups.len() {
            let used = self.live_funcs.get(&g).is_some_and(|s| !s.is_empty())
                || self.live_types.get(&g).is_some_and(|s| !s.is_empty());
            if used && Some(g) != self.root && !self.exported.contains(&g) {
                self.import_group(g)?;
            }
        }
        if let Some(r) = self.root {
            for name in self.live_types.get(&r).cloned().unwrap_or_default() {
                let t = groups.ty(self.m, r, &name).ok_or("lost world type")?;
                self.named(&mut Space::Outer, t)?;
            }
            for name in self.live_funcs.get(&r).cloned().unwrap_or_default() {
                self.root_func(&name)?;
            }
        }
        Ok(())
    }

    /// The shim, the main module, the adapter, the shim's table filled,
    /// and `_initialize` run.
    fn instantiate(&mut self, main: &Module<'_>, adapter: Option<&Module<'_>>) -> Res<()> {
        let tys: Vec<FuncType> = self.slots.iter().map(|(t, ..)| t.clone()).collect();
        if !tys.is_empty() {
            let sm = self.b.module(&core::shim(&tys));
            self.shim = Some(self.b.core_instantiate(sm, &[]));
        }
        let args = self.bags(Inst::Main)?;
        let mm = self
            .b
            .module(&main.without_customs(|n| n.starts_with("component-type")));
        let mi = self.b.core_instantiate(mm, &args);
        self.main = Some(mi);
        if self.main_exports.contains("memory") {
            self.mem = Some(self.b.alias_core(mi, "memory", CORE_MEMORY));
        }
        if let Some(a) = adapter {
            let args = self.bags(Inst::Adapter)?;
            let am = self.b.module(a.bytes);
            self.adapter = Some(self.b.core_instantiate(am, &args));
        }
        if let Some(shim) = self.shim {
            let table = self.b.alias_core(shim, "$imports", CORE_TABLE);
            let mut items = vec![("$imports".to_owned(), CORE_TABLE, table)];
            for (i, (_, op, by)) in self.slots.clone().iter().enumerate() {
                let f = self.emit_op(op, *by)?;
                items.push((i.to_string(), CORE_FUNC, f));
            }
            let fm = self.b.module(&core::fixup(&tys));
            let bag = self.b.core_bag(&items);
            self.b.core_instantiate(fm, &[(String::new(), bag)]);
        }
        // A reactor's constructors, once all they may call is in place.
        if main.export_func("_initialize").is_some() {
            let f = self.b.alias_core(mi, "_initialize", CORE_FUNC);
            let sm = self.b.module(&core::starter());
            let bag = self.b.core_bag(&[(String::new(), CORE_FUNC, f)]);
            self.b.core_instantiate(sm, &[(String::new(), bag)]);
        }
        Ok(())
    }

    fn mark(&mut self, v: &Val) {
        let m = self.m;
        let Val::Id(t) = v else { return };
        let mut t = *t;
        if m.tys[t].name.is_some() {
            t = self.g.canon(m, t);
            let Some((g, n)) = self.g.named(m, t) else {
                return;
            };
            if !self.live_types.entry(g).or_default().insert(n) {
                return;
            }
        }
        let one = |e: &mut Self, v: &Val| e.mark(v);
        match &m.tys[t].def {
            Def::Alias(a) | Def::List(a) | Def::FixedList(a, _) | Def::Option(a) => one(self, a),
            Def::Record(fs) => fs.iter().for_each(|(_, t)| one(self, t)),
            Def::Tuple(ts) => ts.iter().for_each(|t| one(self, t)),
            Def::Variant(cs) => cs
                .iter()
                .filter_map(|(_, t)| t.as_ref())
                .for_each(|t| one(self, t)),
            Def::Result(a, b) => a.iter().chain(b).for_each(|t| one(self, t)),
            Def::Own(r) | Def::Borrow(r) => one(self, &Val::Id(*r)),
            Def::Future(p) | Def::Stream(p) => p.iter().for_each(|t| one(self, t)),
            Def::Enum(_) | Def::Flags(_) | Def::Resource => {}
        }
    }

    fn mark_func(&mut self, f: &Func) {
        for (_, t) in &f.params {
            self.mark(t);
        }
        if let Some(t) = &f.result {
            self.mark(t);
        }
    }

    /// What satisfies core import `module` `name` of `by`.
    fn plan(&mut self, module: &str, name: &str, by: Inst) -> Res<Op> {
        let m = self.m;
        if module == P1 {
            return if by == Inst::Main {
                Ok(Op::Export(Inst::Adapter, name.to_owned()))
            } else {
                Err("the adapter imports preview 1".to_owned())
            };
        }
        if let Some(key) = module.strip_prefix("[export]") {
            // Built-ins of the component's own exports.
            if name == "[task-cancel]" {
                return Ok(Op::Simple(vec![0x05]));
            }
            if key == ROOT {
                let funcs = self.root_exports.clone();
                let find = |f: &str| funcs.iter().find(|x| x.name == f).cloned();
                return self.export_builtin(name, &find, None);
            }
            let g = self
                .g
                .find(key)
                .filter(|g| self.exported.contains(g))
                .ok_or_else(|| format!("`{key}` is not an export of the world"))?;
            let groups = self.g;
            let find = |f: &str| groups.func(m, g, f).cloned();
            return self.export_builtin(name, &find, Some(g));
        }
        if module == ROOT {
            if let Some(op) = intrinsic(name) {
                return op;
            }
        }
        let g = self
            .g
            .find(module)
            .ok_or_else(|| "no world imports this interface".to_owned())?;
        if self.exported.contains(&g) {
            return Err("the interface is exported, not imported".to_owned());
        }
        let groups = self.g;
        let find = |f: &str| groups.func(m, g, f).cloned();
        if let Some(r) = name.strip_prefix("[resource-drop]") {
            let t = self.resource(g, r)?;
            self.mark(&Val::Id(t));
            return Ok(Op::ResDrop(t));
        }
        if let Some(op) = self.payload(name, &find)? {
            return Ok(op);
        }
        let (async_, fname) = match name.strip_prefix("[async-lower]") {
            Some(f) => (true, f),
            None => (false, name),
        };
        let f = find(fname).ok_or("no such function")?;
        if f.is_async != async_ {
            return Err(if async_ {
                "an `[async-lower]` import of a sync function".to_owned()
            } else {
                "an `async func` imported without `[async-lower]`".to_owned()
            });
        }
        self.live_funcs.entry(g).or_default().insert(f.name.clone());
        self.mark_func(&f);
        Ok(Op::Lower {
            g,
            name: f.name.clone(),
            opts: self.lower_opts(&f),
        })
    }

    /// Resource `r` of group `g`.
    fn resource(&self, g: usize, r: &str) -> Res<Tid> {
        let m = self.m;
        self.g
            .ty(m, g, r)
            .filter(|t| matches!(m.tys[*t].def, Def::Resource))
            .ok_or_else(|| format!("no resource `{r}`"))
    }

    fn export_builtin(
        &mut self,
        name: &str,
        find: &dyn Fn(&str) -> Option<Func>,
        g: Option<usize>,
    ) -> Res<Op> {
        if let Some(f) = name.strip_prefix("[task-return]") {
            let f = find(f).ok_or("no such exported function")?;
            if let Some(t) = &f.result {
                self.mark(t);
            }
            let m = self.m;
            let opts = Opts {
                memory: f
                    .result
                    .as_ref()
                    .is_some_and(|t| m.flat(t) > MAX_FLAT_PARAMS || m.has_ptr(t)),
                utf8: f.result.as_ref().is_some_and(|t| m.has_string(t)),
                ..Opts::default()
            };
            return Ok(Op::TaskReturn(f.result, opts));
        }
        if let Some(g) = g {
            for (pre, mk) in [
                ("[resource-new]", Op::ResNew as fn(Tid) -> Op),
                ("[resource-rep]", Op::ResRep),
                ("[resource-drop]", Op::ResDrop),
            ] {
                if let Some(r) = name.strip_prefix(pre) {
                    return Ok(mk(self.resource(g, r)?));
                }
            }
        }
        self.payload(name, find)?
            .ok_or_else(|| "unknown export built-in".to_owned())
    }

    /// A future or stream built-in, if `name` is one.
    fn payload(&mut self, name: &str, find: &dyn Fn(&str) -> Option<Func>) -> Res<Option<Op>> {
        let m = self.m;
        let (async_, rest) = match name.strip_prefix("[async-lower]") {
            Some(r) => (true, r),
            None => (false, name),
        };
        let Some((future, op, idx, fname)) = payload_name(rest) else {
            return Ok(None);
        };
        let ty = match idx {
            None => self.unit[usize::from(future)],
            Some(i) => {
                let f = find(fname).ok_or("no such function")?;
                *m.payloads(&f)
                    .get(i)
                    .ok_or("no future or stream of that index")?
            }
        };
        let elem = match &m.tys[ty].def {
            Def::Future(e) if future => e.clone(),
            Def::Stream(e) if !future => e.clone(),
            _ => return Err("a future and a stream mixed up".to_owned()),
        };
        self.mark(&Val::Id(ty));
        let base = if future { 0x15 } else { 0x0e };
        let rw = |read: bool| Opts {
            memory: true,
            realloc: read && elem.as_ref().is_some_and(|t| m.has_ptr(t)),
            utf8: elem.as_ref().is_some_and(|t| m.has_string(t)),
            async_,
        };
        let (code, opts, flag) = match op {
            "new" => (base, None, None),
            "read" => (base + 1, Some(rw(true)), None),
            "write" => (base + 2, Some(rw(false)), None),
            "cancel-read" => (base + 3, None, Some(async_)),
            "cancel-write" => (base + 4, None, Some(async_)),
            "drop-readable" => (base + 5, None, None),
            "drop-writable" => (base + 6, None, None),
            _ => return Err("unknown future or stream built-in".to_owned()),
        };
        Ok(Some(Op::Payload {
            code,
            ty,
            opts,
            flag,
        }))
    }

    fn lower_opts(&self, f: &Func) -> Opts {
        let m = self.m;
        let pflat = m.flat_all(f.params.iter().map(|(_, t)| t));
        let ptr = f.params.iter().any(|(_, t)| m.has_ptr(t))
            || f.result.as_ref().is_some_and(|t| m.has_ptr(t));
        let spill = if f.is_async {
            pflat > MAX_FLAT_ASYNC_PARAMS || f.result.is_some()
        } else {
            pflat > MAX_FLAT_PARAMS || f.result.as_ref().is_some_and(|t| m.flat(t) > 1)
        };
        Opts {
            memory: ptr || spill,
            realloc: f.result.as_ref().is_some_and(|t| m.has_ptr(t)),
            utf8: self.has_string(f),
            async_: f.is_async,
        }
    }

    fn has_string(&self, f: &Func) -> bool {
        let m = self.m;
        f.params.iter().any(|(_, t)| m.has_string(t))
            || f.result.as_ref().is_some_and(|t| m.has_string(t))
    }
}
