//! Module descriptors (`nabir_abi::module`): a plugin's, which the
//! loader reads and whose tables it fills, and a host's, registered
//! before `main`. Only the glue of a plugin or of a program that reaches
//! `std::plugin` emits them.
//!
//! The linker defines what only it knows: `nabi.module.text_lo` /
//! `text_hi` around the code, `nabi.module.imports` (a plugin's import
//! table, whose length it writes into the descriptor) and
//! `nabi.module.exports` (a host's export table, likewise).

use crate::lir::{Bin, Func, Inst, K, Mem, SymId, V};
use crate::lower::{Ctx, FnLower, Sig};
use crate::obj::Sect;
use crate::typeinfo::{key_hash, real_key};
use crate::wrap;
use nabic_ir_common::{FnRole, FunctionId, Linkage, PrimTy};
use nabic_mir::plugin::{Mode, TAGS};
use nabic_mir::{Link, MirModule, MirTy};
use nabir_abi::module::{self as abi, flags, sym as kind};
use nabir_abi::typedesc::{Word, fnv64, offset_of, size_of};

/// A plugin's GC type ids, one `u32` per scan table, the loader's.
pub const GC_IDS: &str = "nabi.module.gc_ids";
const GC_TABLES: &str = "nabi.module.gc_tables";
const TAG_KEYS: &str = "nabi.module.tag_keys";
const TEXT_LO: &str = "nabi.module.text_lo";
const TEXT_HI: &str = "nabi.module.text_hi";
const IMPORTS: &str = "nabi.module.imports";
const EXPORTS: &str = "nabi.module.exports";
/// What a host's `nabi.register_types` calls first.
const HOST_REGISTER: &str = "nabi.module.register";

/// A static this module defines although it is `extern` in MIR: its
/// tag table and `Plugin::get`'s signature hashes.
#[must_use]
pub fn defines(name: &str) -> bool {
    name == TAGS || nabic_hir_sig(name)
}

fn nabic_hir_sig(name: &str) -> bool {
    name.starts_with("nabi.plugin.sig.")
}

/// Whether a plugin takes static `name` from the process: one of a
/// package it does not own or bundle (the standard library's too).
#[must_use]
pub fn imported(module: &MirModule, name: &str) -> bool {
    let p = &module.plugin;
    let foreign = module.statics.iter().any(|s| s.name == name && s.is_extern);
    if p.mode != Mode::Plugin || module.link.in_prebuilt(name) || foreign || defines(name) {
        return false;
    }
    let owns = |pkg: &String| Link::owns(pkg, name);
    // A hot package's statics too: a later version binds to the storage
    // of the first.
    p.hot.is_some() || !(module.link.lib.iter().any(owns) || p.bundle.iter().any(owns))
}

/// A plugin's copy of a static it takes from the process is
/// `<sym>.own`: code reaches `<sym>`, which the loader binds to the
/// host's, or to this one when the host has none.
#[must_use]
pub fn own(name: String, yes: bool) -> String {
    if yes { name + ".own" } else { name }
}

/// Bytes and relocations of one data object, built word by word. A
/// pointer's addend stays in place.
struct Out {
    bytes: Vec<u8>,
    relocs: Vec<(u32, SymId)>,
    pw: u32,
    be: bool,
}

impl Out {
    const fn new(pw: u32, be: bool) -> Self {
        Self {
            bytes: Vec::new(),
            relocs: Vec::new(),
            pw,
            be,
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

    fn ptr_at(&mut self, sym: Option<SymId>, addend: u64) {
        if let Some(s) = sym {
            self.relocs.push((self.bytes.len() as u32, s));
        }
        self.word(addend);
    }

    fn ptr(&mut self, sym: Option<SymId>) {
        self.ptr_at(sym, 0);
    }

    fn slice(&mut self, sym: Option<SymId>, len: usize) {
        self.ptr(sym.filter(|_| len > 0));
        self.word(len as u64);
    }

    fn text(&mut self, ctx: &mut Ctx<'_>, s: &str) {
        let sym = (!s.is_empty()).then(|| ctx.obj.bytes(s.as_bytes()));
        self.slice(sym, s.len());
    }

    /// Defined as `sym` (`global`: seen by the linker's other inputs).
    fn define(self, ctx: &mut Ctx<'_>, sym: SymId, sect: Sect, global: bool) {
        let sect = if self.relocs.is_empty() && sect == Sect::DataConst {
            Sect::Const
        } else {
            sect
        };
        let bytes = if self.bytes.is_empty() {
            vec![0; self.pw as usize]
        } else {
            self.bytes
        };
        let off = ctx
            .obj
            .define_data(sym, sect, &bytes, self.pw.max(8), global);
        for (o, s) in self.relocs {
            ctx.obj.abs64(sect, off + o, s);
        }
    }

    fn check(&self, words: &[Word], n: usize) {
        debug_assert_eq!(
            self.bytes.len(),
            size_of(words, self.pw) as usize * n,
            "descriptor layout"
        );
    }
}

/// The code word of function `id`'s value: its `.v` thunk when it
/// takes `nabi_tls`.
fn code(ctx: &mut Ctx<'_>, id: FunctionId) -> Option<SymId> {
    let sig = ctx.fns.get(&id)?;
    if !sig.tls {
        return Some(sig.sym);
    }
    let name = format!("{}.v", ctx.obj.syms[sig.sym as usize].name);
    ctx.thunks.insert(id);
    Some(ctx.sym(&name))
}

/// The function values the descriptor takes, made before the thunks are:
/// `(function, code)`.
pub fn values(module: &MirModule, ctx: &mut Ctx<'_>) -> Vec<(FunctionId, SymId)> {
    let p = &module.plugin;
    let ids = p.exports.iter().map(|e| e.value);
    ids.chain(p.dispatchers.iter().map(|d| d.value))
        .filter_map(|id| Some((id, code(ctx, id)?)))
        .collect()
}

/// One row of the export table.
struct ExportRow {
    sym: String,
    path: String,
    sig: u64,
    addr: SymId,
    kind: usize,
    /// The type `sig` hashes, for error messages.
    shown: String,
}

impl ExportRow {
    const fn new(sym: String, path: String, addr: SymId, kind: usize) -> Self {
        Self {
            sym,
            path,
            sig: 0,
            addr,
            kind,
            shown: String::new(),
        }
    }
}

/// The descriptor, after the glue's statics and GC tables (`tables`:
/// each table's id and symbol). A host's registration function, which
/// `nabi.register_types` calls, when one is made.
pub fn emit(
    module: &MirModule,
    ctx: &mut Ctx<'_>,
    values: &[(FunctionId, SymId)],
    tables: &[(u32, SymId)],
    funcs: &mut Vec<(Func, bool, bool)>,
) -> Option<SymId> {
    let p = &module.plugin;
    if p.mode == Mode::None {
        return None;
    }
    let plugin = p.mode == Mode::Plugin;
    let t = ctx.t;
    let pw = t.pointer_width / 8;
    let be = ctx.obj.be;
    let link = &module.link;
    let value_of = |id: FunctionId| values.iter().find(|v| v.0 == id).map(|v| v.1);
    for (name, key) in &p.sigs {
        let s = ctx.sym(name);
        let v = key_hash(&real_key(link, key));
        let bytes = ctx.obj.int_bytes(v, 8);
        ctx.obj.define_data(s, Sect::Const, &bytes, 8, false);
    }
    // Tags: a plugin's for the loader to fill, a host's its own.
    let n_tags = p.tags.len();
    let mut tags = Out::new(pw, be);
    let mut keys = Out::new(pw, be);
    for (tag, key) in &p.tags {
        tags.int(
            if plugin {
                u64::from(u32::MAX)
            } else {
                u64::from(*tag)
            },
            4,
        );
        let real = real_key(link, key);
        keys.text(ctx, &real);
        keys.int(fnv64(real.as_bytes()), 8);
    }
    keys.check(abi::TAG_KEY, n_tags);
    let tags_sym = ctx.sym(TAGS);
    let tag_bytes = if tags.bytes.is_empty() {
        vec![0xff; 4]
    } else {
        tags.bytes
    };
    ctx.obj
        .define_data(tags_sym, Sect::Data, &tag_bytes, 4, false);
    let keys_sym = ctx.sym(TAG_KEYS);
    keys.define(ctx, keys_sym, Sect::DataConst, false);
    // GC types: a plugin's ids are the loader's (`plugin_gc_tid`).
    let (gc_tables, gc_ids) = if plugin {
        let mut list = Out::new(pw, be);
        for &(_, s) in tables {
            list.ptr(Some(s));
        }
        let ls = ctx.sym(GC_TABLES);
        list.define(ctx, ls, Sect::DataConst, false);
        let ids = ctx.sym(GC_IDS);
        let n = tables.len().max(1) * 4;
        ctx.obj.define_data(ids, Sect::Data, &vec![0; n], 4, false);
        (Some(ls), Some(ids))
    } else {
        (None, None)
    };
    let roots = if plugin {
        roots(module, ctx, pw, be)
    } else {
        None
    };
    let exports = if plugin {
        exports(module, ctx, &value_of)
    } else {
        Vec::new()
    };
    let mut ex = Out::new(pw, be);
    for e in &exports {
        ex.text(ctx, &e.sym);
        ex.text(ctx, &e.path);
        ex.int(e.sig, 8);
        ex.ptr(Some(e.addr));
        ex.word(e.kind as u64);
        ex.text(ctx, &e.shown);
    }
    ex.check(abi::EXPORT, exports.len());
    let ex_sym = ctx.sym("nabi.module.export_list");
    ex.define(ctx, ex_sym, Sect::DataConst, false);
    // `any Tr` method bodies for the tags this module knows.
    let mut dispatch = Out::new(pw, be);
    let mut n_dispatch = 0;
    for d in &p.dispatchers {
        let Some(code) = value_of(d.value) else {
            continue;
        };
        for tag in &d.tags {
            let Some(i) = p.tag_index(*tag) else { continue };
            dispatch.int(nabic_mir::plugin::method_hash(&d.method), 8);
            dispatch.ptr(Some(code));
            dispatch.word(i as u64);
            n_dispatch += 1;
        }
    }
    dispatch.check(abi::DISPATCH, n_dispatch);
    let dispatch_sym = ctx.sym("nabi.module.dispatch");
    dispatch.define(ctx, dispatch_sym, Sect::DataConst, false);
    let pkgs = packages(module, ctx, pw, be);
    let hot = crate::hot::emit(ctx, tables.len());
    // The descriptor.
    let mut d = Out::new(pw, be);
    d.int(u64::from(abi::MAGIC), 4);
    d.int(u64::from(abi::VERSION), 2);
    let kind = if plugin { flags::PLUGIN } else { flags::HOST };
    let hot_flag = if hot.is_some() { flags::HOT } else { 0 };
    d.int(u64::from(kind | hot_flag), 2);
    d.int(p.hot.as_ref().map_or(0, |h| h.version), 8);
    let lo = ctx.sym(TEXT_LO);
    let hi = ctx.sym(TEXT_HI);
    d.ptr(Some(lo));
    d.ptr(Some(hi));
    d.text(ctx, &p.toolchain);
    d.text(ctx, &t.name);
    let name = link.lib.clone().unwrap_or_else(|| p.name.clone());
    d.text(ctx, &name);
    d.slice(pkgs.0, pkgs.1);
    d.slice(pkgs.2, pkgs.3);
    if plugin {
        // The linker writes the length.
        let imp = ctx.sym(IMPORTS);
        d.ptr(Some(imp));
        d.word(0);
        d.slice(Some(ex_sym), exports.len());
    } else {
        d.slice(None, 0);
        let exp = ctx.sym(EXPORTS);
        d.ptr(Some(exp));
        d.word(0);
    }
    d.ptr(gc_tables.filter(|_| !tables.is_empty()));
    d.ptr(gc_ids);
    d.int(if plugin { tables.len() as u64 } else { 0 }, 4);
    d.int(0, 4);
    d.ptr((n_tags > 0).then_some(keys_sym));
    d.ptr(Some(tags_sym));
    d.int(n_tags as u64, 4);
    d.int(0, 4);
    match roots {
        Some((s, n)) => d.slice(Some(s), n),
        None => d.slice(None, 0),
    }
    // A host registers its descriptors itself (`typeinfo::REGISTER`).
    let n_types = if plugin { module.type_descs.len() } else { 0 };
    let types = (n_types > 0).then(|| ctx.sym(crate::typeinfo::LIST));
    d.slice(types, n_types);
    d.slice(Some(dispatch_sym), n_dispatch);
    let (functions, desc) = hot.map_or((None, None), |(h, f)| (f, Some(h)));
    match functions {
        Some((f, n)) => d.slice(Some(f), n),
        None => d.slice(None, 0),
    }
    d.ptr(desc);
    d.word(0);
    d.check(abi::MODULE_DESC, 1);
    debug_assert_eq!(offset_of(abi::MODULE_DESC, 17, pw) % pw, 0);
    let desc = ctx.sym(if plugin {
        abi::PLUGIN_SYMBOL
    } else {
        abi::HOST_SYMBOL
    });
    d.define(ctx, desc, Sect::DataConst, true);
    if plugin {
        return None;
    }
    let reg = ctx.sym(HOST_REGISTER);
    let func = wrap::generated(
        ctx,
        HOST_REGISTER,
        reg,
        nabic_diagnostic::Span::DUMMY,
        |l| l.register_host(desc),
    );
    funcs.push((func, false, false));
    Some(reg)
}

/// `(provides, n, needs, n)`: a plugin provides its package and the ones
/// it bundles and needs the other dependencies; a host provides its
/// dependencies.
fn packages(
    module: &MirModule,
    ctx: &mut Ctx<'_>,
    pw: u32,
    be: bool,
) -> (Option<SymId>, usize, Option<SymId>, usize) {
    let link = &module.link;
    let p = &module.plugin;
    let mut provides: Vec<String> = Vec::new();
    let mut needs: Vec<String> = Vec::new();
    // The standard library is the toolchain's, which matches exactly.
    let others = link
        .packages
        .iter()
        .map(|(n, _)| n)
        .filter(|n| !n.is_empty() && !nabic_core::in_toolchain(n));
    if p.mode == Mode::Plugin {
        provides.extend(link.lib.iter().cloned());
        for d in others {
            if p.bundle.contains(d) {
                provides.push(d.clone());
            } else if link.lib.as_ref().is_none_or(|l| l != d) {
                needs.push(d.clone());
            }
        }
    } else {
        provides.extend(others.cloned());
    }
    let mut list = |names: &[String], what: &str| {
        if names.is_empty() {
            return None;
        }
        let mut o = Out::new(pw, be);
        for n in names {
            let hash = link
                .packages
                .iter()
                .find(|(p, _)| p == n)
                .map_or("", |(_, h)| h.as_str())
                .to_owned();
            o.text(ctx, n);
            let version = p
                .versions
                .iter()
                .find(|(k, _)| k == n)
                .map_or("", |(_, v)| v);
            o.text(ctx, version);
            let mut pkg8 = [b'0'; 8];
            for (d, s) in pkg8.iter_mut().zip(hash.bytes()) {
                *d = s;
            }
            o.bytes.extend_from_slice(&pkg8);
            // The full hash is not carried yet: the 8 digits, decoded.
            let mut full = [0u8; 32];
            for (i, b) in full.iter_mut().take(4).enumerate() {
                *b = hash
                    .get(2 * i..2 * i + 2)
                    .and_then(|x| u8::from_str_radix(x, 16).ok())
                    .unwrap_or(0);
            }
            o.bytes.extend_from_slice(&full);
        }
        o.check(abi::PKG_DESC, names.len());
        let s = ctx.sym(what);
        o.define(ctx, s, Sect::DataConst, false);
        Some(s)
    };
    let ps = list(&provides, "nabi.module.provides");
    let ns = list(&needs, "nabi.module.needs");
    (ps, provides.len(), ns, needs.len())
}

/// A plugin's static roots: every reference word of the statics it
/// defines (its own copies of the standard library's too).
fn roots(module: &MirModule, ctx: &mut Ctx<'_>, pw: u32, be: bool) -> Option<(SymId, usize)> {
    let t = ctx.t;
    let mut o = Out::new(pw, be);
    let mut n = 0;
    for s in &module.statics {
        if s.is_extern || ctx.pkgs.link.in_prebuilt(&s.name) {
            continue;
        }
        let mine = imported(module, &s.name);
        let sym = ctx.sym(&own(ctx.pkgs.static_sym(&s.name), mine));
        for off in t.root_offsets(&s.ty) {
            o.ptr_at(Some(sym), u64::from(off));
            o.word(0);
            n += 1;
        }
    }
    if n == 0 {
        return None;
    }
    o.check(abi::ROOT, n);
    let s = ctx.sym("nabi.module.roots");
    o.define(ctx, s, Sect::DataConst, false);
    Some((s, n))
}

/// A plugin's exports, sorted by symbol: its package's functions (and
/// their values, by path where `Plugin::get` finds them) and statics,
/// and its copies of the standard library's statics under their own
/// names.
fn exports(
    module: &MirModule,
    ctx: &mut Ctx<'_>,
    value_of: &dyn Fn(FunctionId) -> Option<SymId>,
) -> Vec<ExportRow> {
    let link = &module.link;
    let p = &module.plugin;
    let Some(lib) = link.lib.clone() else {
        return Vec::new();
    };
    let link_hash = |key: &str| key_hash(&real_key(link, key));
    let name_of = |ctx: &Ctx<'_>, s: SymId| ctx.obj.syms[s as usize].name.clone();
    let mut rows: Vec<ExportRow> = Vec::new();
    for f in &module.functions {
        if f.facts.linkage != Linkage::Defined
            || f.facts.role == FnRole::AnyDispatcher
            || !Link::owns(&lib, f.path())
        {
            continue;
        }
        let Some(sig) = ctx.fns.get(&f.id).map(|s: &Sig| s.sym) else {
            continue;
        };
        let name = name_of(ctx, sig);
        let mut row = ExportRow::new(name.clone(), String::new(), sig, kind::FN);
        // A hot build exports the body, `X.hot`, which the linker gives
        // the code of `X` before making `X` the slot's stub.
        if let Some(h) = &p.hot {
            row.addr = ctx.sym(&format!("{name}.hot"));
            if let Some((_, k)) = h.sigs.iter().find(|s| s.0 == f.id) {
                row.sig = link_hash(k);
            }
        }
        rows.push(row);
    }
    for e in &p.exports {
        let sig = link_hash(&e.sig);
        if let Some(s) = ctx.fns.get(&e.id).map(|s| s.sym) {
            let name = name_of(ctx, s);
            if let Some(r) = rows.iter_mut().find(|r| r.sym == name) {
                r.path.clone_from(&e.path);
                r.sig = sig;
                r.shown.clone_from(&e.shown);
            }
        }
        if let Some(code) = value_of(e.value) {
            let mut r = ExportRow::new(name_of(ctx, code), e.path.clone(), code, kind::FN_VALUE);
            r.sig = sig;
            r.shown.clone_from(&e.shown);
            rows.push(r);
        }
    }
    for s in &module.statics {
        if s.is_extern || ctx.pkgs.link.in_prebuilt(&s.name) {
            continue;
        }
        let shared = imported(module, &s.name);
        let sym = ctx.pkgs.static_sym(&s.name);
        let mine = Link::owns(&lib, &s.name);
        let path = if !shared || mine {
            s.name.clone()
        } else {
            String::new()
        };
        let lazy = ctx.pkgs.lazy_statics.contains_key(&s.name);
        let mut add = |ctx: &mut Ctx<'_>, name: String, path: String, k: usize| {
            let at = ctx.sym(&own(name.clone(), shared));
            rows.push(ExportRow::new(name, path, at, k));
        };
        add(ctx, sym.clone(), path, kind::DATA);
        if lazy {
            add(ctx, format!("{sym}.state"), String::new(), kind::DATA);
            add(ctx, format!("{sym}.init"), String::new(), kind::FN);
        }
        // Hot: which storage a later version may share, by the
        // descriptor's `key_hash` of its type.
        let key = p
            .hot
            .as_ref()
            .and_then(|h| h.statics.iter().find(|k| k.0 == s.name));
        if let (Some((_, k)), true) = (key, mine) {
            if let Some(r) = rows
                .iter_mut()
                .find(|r| r.sym == sym && r.kind == kind::DATA)
            {
                r.sig = link_hash(k);
            }
        }
    }
    rows.sort_by(|a, b| a.sym.cmp(&b.sym));
    rows.dedup_by(|a, b| a.sym == b.sym);
    rows
}

impl FnLower<'_, '_> {
    /// GC type id of table `i` in a plugin: the loader's range starts at
    /// `nabi.module.gc_ids[0]`.
    pub(crate) fn plugin_gc_tid(&mut self, i: usize) -> V {
        let s = self.c.sym(GC_IDS);
        let p = self.word_tmp();
        self.push(Inst::Sym {
            dst: p,
            sym: s,
            got: false,
        });
        let Mem::Frame(ps, _) = p else { unreachable!() };
        let tid = self.tmp_k(K::I32);
        self.push(Inst::Mov {
            k: K::I32,
            dst: tid,
            src: V::M(Mem::Ind(ps, 0)),
        });
        if i != 0 {
            self.push(Inst::Bin {
                op: Bin::Add,
                k: K::I32,
                signed: false,
                dst: tid,
                a: V::M(tid),
                b: V::I(i as i64),
            });
        }
        V::M(tid)
    }

    /// `nabi_module_register(&nabi_host_module)`.
    fn register_host(mut self, desc: SymId) -> Func {
        let w = MirTy::Prim(PrimTy::Usize);
        let p = self.word_tmp();
        self.push(Inst::Sym {
            dst: p,
            sym: desc,
            got: false,
        });
        let at = self.val_of(p, &w);
        let reg = self.c.sym(abi::entry::MODULE_REGISTER);
        let r = self.tmp_k(K::I32);
        self.call_sym(reg, &[w], &MirTy::Prim(PrimTy::I32), &[at], Some(r));
        self.push(Inst::Ret { vals: Vec::new() });
        self.out
    }
}
