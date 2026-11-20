//! Hot-reload builds (`--hot`): what the hot runtime reads besides the
//! plugin descriptor (`nabir_abi::module::HotDesc`).
//!
//! - GC tables are nominal: a table whose objects hold a hot-package type
//!   by value is never shared with another type's, and each says which
//!   type it holds (`GcKey`).
//! - A function holding a hot type by value gets a `FrameInfo`, whose
//!   address sits in a root word of its frame and in the word before
//!   its shadow-frame descriptor (`roots::frame`); the frame is linked
//!   even when it roots nothing else.
//! - `sigs`: the signature of every hot-package function, by symbol.
//!
//! Hot builds are `-O0` only: every local lives in its slot, which is
//! its root word, so a migration that rewrites roots leaves no stale
//! copy (8.4); and a hot function never counts as quiet.

pub use crate::typeinfo::real_key;

use crate::lir::SymId;
use crate::lower::Ctx;
use crate::obj::Sect;
use crate::typeinfo::key_hash;
use nabic_mir::plugin::{Hot, HotTy};
use nabic_mir::{MirFunction, MirModule, MirTy, UnionKind};
use nabir_abi::module::{self as abi, gc_kind};
use nabir_abi::typedesc::size_of;

/// A hot build's codegen state.
pub struct State<'a> {
    pub hot: &'a Hot,
    pub module: &'a MirModule,
    /// Each GC table holding a hot type: `(index, payload, array)`.
    pub tables: Vec<(usize, MirTy, bool)>,
    /// Every `FrameInfo` made, in order.
    pub frames: Vec<SymId>,
}

impl<'a> State<'a> {
    #[must_use]
    pub fn new(module: &'a MirModule) -> Option<Self> {
        Some(Self {
            hot: module.plugin.hot.as_ref()?,
            module,
            tables: Vec::new(),
            frames: Vec::new(),
        })
    }
}

/// `ty` as text that names its structs by their build-independent
/// names: equal in every version of a dev session while the type is.
#[must_use]
pub fn mir_key(module: &MirModule, ty: &MirTy) -> String {
    let list = |ts: &[MirTy]| {
        ts.iter()
            .map(|t| mir_key(module, t))
            .collect::<Vec<_>>()
            .join(",")
    };
    match ty {
        MirTy::Unit => "()".to_owned(),
        MirTy::Prim(p) => format!("{p:?}").to_lowercase(),
        MirTy::Any => "any".to_owned(),
        MirTy::GcRef => "ref".to_owned(),
        MirTy::Tuple(ts) => format!("({})", list(ts)),
        MirTy::Struct(id, ts) => {
            let name = module
                .struct_keys
                .get(&id.0)
                .or_else(|| module.struct_names.get(&id.0).map(|n| &n.0))
                .map_or("?", String::as_str);
            format!("{name}{{{}}}", list(ts))
        }
        MirTy::Array(t, n) => format!("[{};{n}]", mir_key(module, t)),
        MirTy::Fn(ps, r) => format!("fn({})->{}", list(ps), mir_key(module, r)),
        MirTy::Union(vs, words, kind) => {
            let parts: Vec<String> = vs
                .iter()
                .map(|(t, v)| format!("{t}:{}", mir_key(module, v)))
                .collect();
            let k = if *kind == UnionKind::Sum {
                "sum"
            } else {
                "union"
            };
            format!("{k}{words}{{{}}}", parts.join(","))
        }
    }
}

/// Whether `f` is another hot package's own function, which this
/// module calls through a hot slot bound to that package's plugin
/// instead of compiling a copy. Instances and lifted bodies are copies.
#[must_use]
pub fn imported_fn(module: &MirModule, f: &MirFunction) -> bool {
    let Some(h) = &module.plugin.hot else {
        return false;
    };
    let path = f.path();
    f.def_id.is_some()
        && f.facts.mono_args.is_none()
        && f.facts.role == nabic_ir_common::FnRole::Plain
        && f.facts.export_name.is_none()
        && h.packages[1..]
            .iter()
            .any(|p| nabic_mir::Link::owns(p, path))
}

/// Whether hot type `h` can be told apart by its lowered type: a
/// payload-less enum is a bare integer, like any integer.
const fn tracked(h: &HotTy) -> bool {
    !(h.is_enum && matches!(h.mir, MirTy::Prim(_) | MirTy::Unit))
}

/// The hot types `ty` holds by value (through tuples, arrays, sums and
/// value structs), each once.
fn held(st: &State<'_>, ty: &MirTy, out: &mut Vec<usize>) {
    for (i, h) in st.hot.types.iter().enumerate() {
        if tracked(h) && h.mir == *ty && !out.contains(&i) {
            out.push(i);
        }
    }
    match ty {
        MirTy::Tuple(ts) | MirTy::Struct(_, ts) => ts.iter().for_each(|t| held(st, t, out)),
        MirTy::Array(t, _) => held(st, t, out),
        MirTy::Union(vs, ..) => vs.iter().for_each(|(_, t)| held(st, t, out)),
        _ => {}
    }
}

impl Ctx<'_> {
    /// In a hot build: `ty` holds a hot type by value, so its buffers
    /// get a table even without references.
    pub(crate) fn hot_holds(&self, ty: &MirTy) -> bool {
        self.hot.as_ref().is_some_and(|st| {
            let mut h = Vec::new();
            held(st, ty, &mut h);
            !h.is_empty()
        })
    }

    /// In a hot build, the GC table of `ty` objects (`array`: buffers of
    /// them) with scan words `key`: one of their own when they hold a hot
    /// type, never one of those otherwise.
    pub(crate) fn hot_gc_type(&mut self, ty: &MirTy, array: bool, key: &[u32]) -> Option<usize> {
        let st = self.hot.as_mut()?;
        let mut h = Vec::new();
        held(st, ty, &mut h);
        if !h.is_empty() {
            if let Some(t) = st.tables.iter().find(|t| t.1 == *ty && t.2 == array) {
                return Some(t.0);
            }
            self.gc_types.push(key.to_vec());
            let i = self.gc_types.len() - 1;
            st.tables.push((i, ty.clone(), array));
            return Some(i);
        }
        let i = (0..self.gc_types.len())
            .find(|&i| self.gc_types[i] == key && !st.tables.iter().any(|t| t.0 == i))
            .unwrap_or_else(|| {
                self.gc_types.push(key.to_vec());
                self.gc_types.len() - 1
            });
        Some(i)
    }
}

/// `Str` words of `text` (in rodata), into `out` with its relocations.
fn text(ctx: &mut Ctx<'_>, out: &mut Words, s: &str) {
    let sym = (!s.is_empty()).then(|| ctx.obj.bytes(s.as_bytes()));
    out.ptr(sym);
    out.word(s.len() as u64);
}

/// Bytes and relocations of one data object.
struct Words {
    bytes: Vec<u8>,
    relocs: Vec<(u32, SymId)>,
    pw: u32,
    be: bool,
}

impl Words {
    fn new(ctx: &Ctx<'_>) -> Self {
        Self {
            bytes: Vec::new(),
            relocs: Vec::new(),
            pw: ctx.t.pointer_width / 8,
            be: ctx.obj.be,
        }
    }

    fn int(&mut self, v: u64, n: usize) {
        if self.be {
            self.bytes.extend_from_slice(&v.to_be_bytes()[8 - n..]);
        } else {
            self.bytes.extend_from_slice(&v.to_le_bytes()[..n]);
        }
    }

    fn word(&mut self, v: u64) {
        self.int(v, self.pw as usize);
    }

    fn ptr(&mut self, sym: Option<SymId>) {
        if let Some(s) = sym {
            self.relocs.push((self.bytes.len() as u32, s));
        }
        self.word(0);
    }

    fn slice(&mut self, sym: Option<SymId>, len: usize) {
        self.ptr(sym.filter(|_| len > 0));
        self.word(len as u64);
    }

    fn define(self, ctx: &mut Ctx<'_>, sym: SymId) {
        let sect = if self.relocs.is_empty() {
            Sect::Const
        } else {
            Sect::DataConst
        };
        let bytes = if self.bytes.is_empty() {
            vec![0; self.pw as usize]
        } else {
            self.bytes
        };
        let off = ctx.obj.define_data(sym, sect, &bytes, 8, false);
        for (o, s) in self.relocs {
            ctx.obj.abs64(sect, off + o, s);
        }
    }
}

/// The key hash of hot type `i`.
fn hash_of(ctx: &Ctx<'_>, i: usize) -> u64 {
    let h = &ctx.hot.as_ref().expect("a hot build").hot.types[i];
    key_hash(&real_key(&ctx.pkgs.link, &h.key))
}

/// Function `f`'s `FrameInfo` (`me`: its symbol) when it holds a hot
/// type by value: then its shadow frame is linked, also with no other
/// roots.
pub(crate) fn frame_info(c: &mut Ctx<'_>, f: &MirFunction, me: SymId) -> Option<SymId> {
    let st = c.hot.as_ref()?;
    let mut deps = Vec::new();
    for l in &f.locals {
        held(st, &l.ty, &mut deps);
    }
    for t in f.param_tys.iter().chain([&f.ret_ty]) {
        held(st, t, &mut deps);
    }
    if deps.is_empty() {
        return None;
    }
    let span = f.span;
    let at = c.sources.location(span);
    let file = at.rsplitn(3, ':').nth(2).unwrap_or("").to_owned();
    let name = f.path().to_owned();
    let me = c.obj.syms[me as usize].name.clone();
    // Generated code (`.v` thunks) has no slot: its symbol is the body.
    let own = st.hot.owns(f.path()) && f.id.0 != u32::MAX;
    let names: Vec<String> = deps.iter().map(|&i| st.hot.types[i].name.clone()).collect();
    let hashes: Vec<u64> = deps.iter().map(|&i| hash_of(c, i)).collect();
    let mut held = Words::new(c);
    for (n, k) in names.iter().zip(&hashes) {
        held.int(*k, 8);
        held.int(u64::from(span.line), 4);
        held.int(0, 4);
        text(c, &mut held, n);
    }
    debug_assert_eq!(
        held.bytes.len(),
        size_of(abi::HELD, held.pw) as usize * names.len()
    );
    let held_sym = c.sym(&format!("{me}.held"));
    held.define(c, held_sym);
    // The body: `X.hot` once the linker made `X` the slot's stub.
    let code = if own {
        c.sym(&format!("{me}.hot"))
    } else {
        c.sym(&me)
    };
    let mut fi = Words::new(c);
    fi.int(u64::from(abi::FRAME_MAGIC), 4);
    fi.int(0, 4);
    fi.ptr(Some(code));
    fi.word(u64::from(span.line));
    text(c, &mut fi, &name);
    text(c, &mut fi, &file);
    fi.slice(Some(held_sym), names.len());
    debug_assert_eq!(fi.bytes.len(), size_of(abi::FRAME_INFO, fi.pw) as usize);
    let sym = c.sym(&format!("{me}.frame"));
    fi.define(c, sym);
    if let Some(st) = c.hot.as_mut() {
        st.frames.push(sym);
    }
    Some(sym)
}

/// The descriptor's hot parts: the `HotDesc` and the `functions` table
/// `(symbol, rows)`, after the GC tables are known.
pub fn emit(ctx: &mut Ctx<'_>, n_tables: usize) -> Option<(SymId, Option<(SymId, usize)>)> {
    let st = ctx.hot.as_ref()?;
    let base_version = st.hot.base_version;
    let tables = st.tables.clone();
    let frames = st.frames.clone();
    let hot = st.hot;
    // GC tables.
    let mut gc = Words::new(ctx);
    for i in 0..n_tables {
        let Some((_, ty, array)) = tables.iter().find(|t| t.0 == i) else {
            gc.int(0, 8);
            gc.int(u64::from(gc_kind::NONE), 4);
            gc.int(0, 4);
            gc.slice(None, 0);
            continue;
        };
        let st = ctx.hot.as_ref().expect("a hot build");
        let same: Vec<usize> = (0..hot.types.len())
            .filter(|&k| tracked(&hot.types[k]) && hot.types[k].mir == *ty)
            .collect();
        let mut inner = Vec::new();
        held(st, ty, &mut inner);
        let (key, kind, list) = match same.as_slice() {
            [k] => (
                hash_of(ctx, *k),
                if *array {
                    gc_kind::ELEMS
                } else {
                    gc_kind::VALUE
                },
                Vec::new(),
            ),
            [] => (
                0,
                gc_kind::HOLDS,
                inner.iter().map(|&k| hash_of(ctx, k)).collect(),
            ),
            many => (
                0,
                gc_kind::AMBIGUOUS,
                many.iter().map(|&k| hash_of(ctx, k)).collect(),
            ),
        };
        let list_sym = (!list.is_empty()).then(|| {
            let bytes: Vec<u8> = list.iter().flat_map(|k| ctx.obj.int_bytes(*k, 8)).collect();
            let s = ctx.sym(&format!("nabi.hot.held.{i}"));
            ctx.obj.define_data(s, Sect::Const, &bytes, 8, false);
            s
        });
        gc.int(key, 8);
        gc.int(u64::from(kind), 4);
        gc.int(0, 4);
        gc.slice(list_sym, list.len());
    }
    let gc_sym = ctx.sym("nabi.hot.gc");
    gc.define(ctx, gc_sym);
    // Signatures, by symbol.
    let mut sigs: Vec<(String, u64)> = Vec::new();
    for (id, key) in &hot.sigs {
        let Some(s) = ctx.fns.get(id) else { continue };
        let name = ctx.obj.syms[s.sym as usize].name.clone();
        sigs.push((name, key_hash(&real_key(&ctx.pkgs.link, key))));
    }
    sigs.sort();
    sigs.dedup_by(|a, b| a.0 == b.0);
    let mut sw = Words::new(ctx);
    for (name, sig) in &sigs {
        text(ctx, &mut sw, name);
        sw.int(*sig, 8);
    }
    let sigs_sym = ctx.sym("nabi.hot.sigs");
    sw.define(ctx, sigs_sym);
    let mut d = Words::new(ctx);
    d.int(u64::from(abi::HOT_MAGIC), 4);
    d.int(1, 2);
    d.int(u64::from(abi::hot_flags::FRAMES_ROOTED), 2);
    d.int(base_version, 8);
    d.slice(Some(gc_sym), n_tables);
    d.slice(Some(sigs_sym), sigs.len());
    debug_assert_eq!(d.bytes.len(), size_of(abi::HOT_DESC, d.pw) as usize);
    let desc = ctx.sym("nabi.hot.desc");
    d.define(ctx, desc);
    // `functions`: one row per FrameInfo; the code range is the
    // FrameInfo's.
    let functions = (!frames.is_empty()).then(|| {
        let mut f = Words::new(ctx);
        for s in &frames {
            f.int(0, 4);
            f.int(0, 4);
            f.ptr(Some(*s));
            f.word(0);
        }
        let sym = ctx.sym("nabi.hot.functions");
        f.define(ctx, sym);
        (sym, frames.len())
    });
    Some((desc, functions))
}
