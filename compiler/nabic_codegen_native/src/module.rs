//! A MIR module -> one object.

use crate::cc;
use crate::lir::{Func, SymId};
use crate::lower::{Ctx, FnLower, Sig, classify};
use crate::obj::{Obj, Sect};
use crate::{Options, wrap};
use nabic_codegen::abi;
use nabic_codegen::mangle::{self, Pkgs};
use nabic_codegen::units::{self, Core};
use nabic_core::{HashMap, HashSet};
use nabic_ir_common::{Abi, ConstVal, FnRole, FunctionId, Linkage, LocalId, PrimTy};
use nabic_mir::{
    BasicBlock, Link, LocalDecl, MirModule, MirTy, Operand, RValue, Statement, Terminator,
};

/// What one module compiled to.
pub struct Compiled {
    pub obj: Obj,
    pub gc_types: Vec<Vec<u32>>,
    pub errors: Vec<(nabic_diagnostic::Span, String)>,
    /// `defs`: the cell's top-level definitions (`crate::repl`).
    pub defs: Vec<crate::repl::CellDef>,
    /// The functions that never reach a safepoint
    /// (`nabic_codegen::analysis::quiet_functions`).
    pub quiet: std::sync::Arc<HashSet<FunctionId>>,
}

/// One codegen unit of a plan (`nabic_codegen::units`).
pub struct UnitArgs<'a> {
    pub core: &'a Core,
    pub u: usize,
    pub glue: bool,
    /// The plan's GC types.
    pub tables: &'a [Vec<u32>],
}

pub fn compile(
    module: &MirModule,
    opts: &Options,
    sources: &nabic_diagnostic::SourceMap,
    unit: Option<&UnitArgs<'_>>,
    emit: &mut dyn FnMut(&mut Obj, &Func),
    defs: bool,
) -> Compiled {
    let t = &module.target;
    let conv = cc::Conv::of(t).expect("a supported target");
    let mut pkgs = Pkgs {
        link: module.link.clone(),
        ..Pkgs::default()
    };
    if let Some(a) = unit {
        a.core.unit_pkgs(&mut pkgs);
    }
    let glue = unit.is_none_or(|a| a.glue);
    let in_unit = |f: FunctionId| unit.is_none_or(|a| a.core.defines(a.u, f));
    let shared = |f: FunctionId| unit.is_some_and(|a| a.core.shared.contains(&f));
    let renamed = unit.map(|a| a.core.renamed.clone()).unwrap_or_default();
    let view: Vec<&nabic_mir::MirFunction> = unit.map_or_else(
        || module.functions.iter().collect(),
        |a| a.core.view(module, a.u),
    );
    pkgs.set_statics(&module.statics, t.os == "windows");
    if pkgs.link.packages.is_empty() {
        pkgs.link.packages = vec![(
            String::new(),
            mangle::pkg_hash(&nabic_mir::print_mir(module)),
        )];
    }
    if opts.repl.is_some() {
        for (p, h) in &mut pkgs.link.packages {
            if p.is_empty() {
                nabic_codegen::analysis::REPL_PKG_HASH.clone_into(h);
            }
        }
    }
    let plugin = module.plugin.mode;
    let mut obj = Obj::of_target(t);
    obj.i386 = t.arch == "x86";
    obj.dyn_tls = t.os == "android";
    obj.arm = t.arch == "arm";
    obj.riscv = t.arch.starts_with("riscv");
    obj.rv32 = t.arch == "riscv32";
    obj.ppc64 = t.arch == "powerpc64";
    obj.s390x = t.arch == "s390x";
    obj.be = t.endian == "big";
    if obj.x86 {
        let f = t.cpu_features();
        obj.popcnt = f.has("popcnt");
        obj.lzcnt = f.has("lzcnt");
        obj.bmi = f.has("bmi");
        obj.f16c = f.has("f16c");
    }
    if t.arch == "aarch64" {
        let f = t.cpu_features();
        obj.lse = f.has("lse");
        obj.outline = !obj.lse && t.os == "linux" && opts.opt_level != crate::OptLevel::O0;
        // `-O0` keeps every load sequentially consistent.
        obj.rcpc = f.has("rcpc") && opts.opt_level != crate::OptLevel::O0;
        obj.dotprod = f.has("dotprod");
    }
    if obj.riscv {
        obj.zbb = t.cpu_features().has("zbb");
    }
    if matches!(t.arch.as_str(), "s390x" | "riscv64") {
        let f = t.cpu_features();
        obj.vector = f.has("vector") || f.has("v");
        obj.z14 = f.has("vector-enhancements-1");
    }
    if let Some(t) = opts.asm {
        obj.asm = Some(String::new());
        if t == crate::Text::Source {
            obj.src = Some(Vec::new());
        }
    }
    if t.object_format == nabic_mir::ObjectFormat::Wasm {
        obj.elf = false;
        obj.wasm = Some(Box::new(crate::isa::wasm::Parts {
            threads: t.threads,
            ..crate::isa::wasm::Parts::default()
        }));
    }
    let wasm = obj.wasm.is_some();
    let mut errors = Vec::new();
    let defines = |name: &str| {
        opts.repl
            .as_ref()
            .is_some_and(|r| r.define.iter().any(|d| d == name))
    };
    let prebuilt_static = |name: &str| {
        !glue
            || pkgs.link.in_prebuilt(name)
            || opts
                .repl
                .as_ref()
                .is_some_and(|r| r.statics.iter().any(|s| s == name))
    };

    // Signatures.
    let mut fns: HashMap<FunctionId, Sig> = HashMap::default();
    let mut c_entries = Vec::new();
    for &f in &view {
        let c = match f.facts.abi {
            Abi::C { variadic, system } => classify(
                &mut errors,
                f.span,
                t,
                &f.param_tys,
                &f.ret_ty,
                (variadic, system),
            ),
            Abi::Nabi if f.facts.linkage == Linkage::Extern => {
                abi::classify_nabi(t, &f.param_tys, &f.ret_ty)
            }
            Abi::Nabi => None,
        };
        let mut name = units::symbol(f, &pkgs, renamed.contains(&f.id));
        let mut sig_c = None;
        let mut c_entry = None;
        // A `stdcall` symbol says how many bytes of arguments it pops.
        let at = |a: &abi::FnAbi| {
            if a.stdcall {
                format!("@{}", stdcall_bytes(t, conv, &f.param_tys, &f.ret_ty, a))
            } else {
                String::new()
            }
        };
        match c.clone() {
            Some(a) if f.facts.role == FnRole::C => {
                // The body is the C function, under its C name.
                name = f
                    .facts
                    .export_name
                    .clone()
                    .unwrap_or_else(|| f.name.clone())
                    + &at(&a);
                sig_c = Some(a);
            }
            Some(mut a) if f.facts.linkage == Linkage::Extern => {
                name.push_str(&at(&a));
                a.errno = f.facts.errno;
                a.leaf = f.facts.leaf;
                sig_c = Some(a);
            }
            Some(a) => {
                name = units::body_symbol(f, &pkgs, renamed.contains(&f.id));
                let entry = f
                    .facts
                    .export_name
                    .clone()
                    .unwrap_or_else(|| format!("{name}.c"))
                    + &at(&a);
                c_entry = Some(obj.sym(&entry));
                if in_unit(f.id) {
                    // Another unit may take its address.
                    let global = f.facts.export_name.is_some() || shared(f.id);
                    c_entries.push((f.id, a, global, f.span));
                }
            }
            None => {}
        }
        let named = obj.sym(&name);
        if let Some(w) = obj.wasm.as_mut() {
            let ty = match (&sig_c, &c) {
                (Some(a), _) => crate::isa::wasm::c_type(t, &f.param_tys, &f.ret_ty, a),
                _ => crate::isa::wasm::nabi_type(t, &f.param_tys, &f.ret_ty, f.facts.takes_tls()),
            };
            if let (Some(e), Some(a)) = (c_entry, &c) {
                w.sigs
                    .insert(e, crate::isa::wasm::c_type(t, &f.param_tys, &f.ret_ty, a));
            }
            w.sigs.insert(named, ty);
        }
        let throw_site = f.facts.linkage == Linkage::Extern
            && f.facts.abi == Abi::Nabi
            && name == "nabi_throw_site";
        let sym = obj.sym(&name);
        fns.insert(
            f.id,
            Sig {
                sym,
                params: f.param_tys.clone(),
                ret: f.ret_ty.clone(),
                c: sig_c,
                c_entry,
                local: false,
                tls: f.facts.takes_tls(),
                throw_site,
                cold: matches!(f.facts.inline, nabic_ir_common::Inline::Cold)
                    || f.facts.summary.has(nabic_ir_common::Summary::COLD),
                nosp: cfg!(debug_assertions) && f.facts.no_safepoint,
                summary: f.facts.summary,
                simd: f.facts.simd,
            },
        );
    }
    let private_copy = |f: &nabic_mir::MirFunction| match &pkgs.link.lib {
        Some(lib) => !Link::owns(lib, f.path()) && f.facts.role != FnRole::AnyDispatcher,
        None => pkgs.link.in_prebuilt(f.path()),
    };
    // A REPL cell compiles only what the session does not have.
    let compiled: HashSet<FunctionId> = module
        .functions
        .iter()
        .filter(|f| {
            f.facts.linkage == Linkage::Defined
                && in_unit(f.id)
                && !crate::hot::imported_fn(module, f)
                && (private_copy(f)
                    || opts
                        .fresh_fns
                        .as_ref()
                        .is_none_or(|s| s.contains(&mangle::symbol_for(f, &pkgs))))
        })
        .map(|f| f.id)
        .collect();
    // Defined here (a unit: in the program): reached directly, not
    // through the GOT.
    let defined: HashSet<SymId> = view
        .iter()
        .filter(|f| match unit {
            None => compiled.contains(&f.id),
            Some(_) => f.facts.linkage == Linkage::Defined,
        })
        .flat_map(|f| [Some(fns[&f.id].sym), fns[&f.id].c_entry])
        .flatten()
        .collect();
    for s in fns.values_mut() {
        s.local = defined.contains(&s.sym);
    }
    if let Some(w) = obj.wasm.as_mut() {
        for s in fns.values() {
            w.fns.extend([Some(s.sym), s.c_entry].into_iter().flatten());
        }
        for f in &view {
            if let (Some(imp), Linkage::Extern) = (&f.facts.wasm_import, f.facts.linkage) {
                w.imports.insert(fns[&f.id].sym, imp.clone());
            }
        }
    }
    let (quiet, reentrant);
    if let Some(a) = unit {
        pkgs.lazy_statics.clone_from(&a.core.lazy_statics);
        quiet = std::sync::Arc::clone(&a.core.quiet);
        reentrant = std::sync::Arc::clone(&a.core.reentrant);
    } else {
        pkgs.lazy_statics = module
            .statics
            .iter()
            .filter_map(|s| {
                let init = s
                    .init_fn
                    .and_then(|i| fns.get(&i))
                    .map(|f| obj.syms[f.sym as usize].name.clone());
                // A prebuilt library's static is initialized by its `.a`.
                let init = init.or_else(|| pkgs.link.in_prebuilt(&s.name).then(String::new));
                Some((s.name.clone(), init?))
            })
            .collect();
        quiet = std::sync::Arc::new(nabic_codegen::analysis::quiet_functions(module, &pkgs, t));
        reentrant = std::sync::Arc::new(nabic_codegen::analysis::reentrant_functions(module));
    }
    // A unit's statics are the glue's, in the same executable.
    // A plugin reaches other packages' statics in the process.
    let local_statics: HashSet<String> = module
        .statics
        .iter()
        .filter(|s| {
            (!s.is_extern || defines(&s.name) || crate::plugin::defines(&s.name))
                && (unit.is_some() || !prebuilt_static(&s.name))
                && !pkgs.link.in_prebuilt(&s.name)
                && !crate::plugin::imported(module, &s.name)
        })
        .map(|s| s.name.clone())
        .collect();

    let gc_type_base = nabic_mir::gc_abi::USER_TYPE_BASE + pkgs.link.type_offset;
    let mut gc_types: Vec<Vec<u32>> = match unit {
        Some(a) => a.tables.to_vec(),
        None => opts
            .repl
            .as_ref()
            .map(|r| r.gc_tables.clone())
            .unwrap_or_default(),
    };
    let pre_tables = if glue && unit.is_some() {
        0
    } else {
        gc_types.len()
    };
    let views: HashSet<u32> = module
        .struct_keys
        .iter()
        .filter(|(_, k)| {
            k.starts_with("core::collections::slice::Slice<")
                || k.starts_with("core::collections::slice::SliceMut<")
        })
        .map(|(id, _)| *id)
        .collect();
    let mut ctx = Ctx {
        sources,
        t,
        conv,
        obj: &mut obj,
        fns: &fns,
        pkgs: &pkgs,
        gc_types: &mut gc_types,
        gc_type_base,
        gc_pkg: unit.map(|a| a.core.placeholder.as_str()),
        overflow_checks: opts.overflow_checks,
        opt: opts.opt_level != crate::OptLevel::O0,
        level: opts.opt_level,
        debug: opts.debug.then_some(&module.debug),
        track: opts.debug && opts.opt_level != crate::OptLevel::O0,
        local_statics: &local_statics,
        quiet: &quiet,
        reentrant: &reentrant,
        errors: Vec::new(),
        thunks: std::collections::BTreeSet::new(),
        plugin,
        hot: crate::hot::State::new(module),
        views: &views,
    };
    let mut funcs: Vec<(Func, bool, bool)> = Vec::new();
    // The candidates' `fresh_fns` keys, by position in `funcs`.
    let mut keys: Vec<String> = Vec::new();

    // In `view`'s order: a unit's object is alike in every program.
    for &f in &view {
        if !compiled.contains(&f.id) {
            continue;
        }
        let sym = fns[&f.id].sym;
        let func = FnLower::new(&mut ctx, f, sym).lower();
        if defs {
            keys.push(mangle::symbol_for(f, &pkgs));
        }
        let weak = pkgs.link.lib.is_some() && f.facts.role == FnRole::AnyDispatcher;
        if f.facts.role == FnRole::C {
            // A C `static` function has no export name.
            funcs.push((func, f.facts.export_name.is_some(), false));
            continue;
        }
        let private = match &pkgs.link.lib {
            Some(lib) => !Link::owns(lib, f.path()) && f.facts.role != FnRole::AnyDispatcher,
            None => pkgs.link.in_prebuilt(f.path()),
        } && !shared(f.id);
        funcs.push((func, !private, weak));
    }
    if glue {
        crate::typeinfo::emit(module, &mut ctx, &mut funcs);
    }
    let values = if glue {
        crate::plugin::values(module, &mut ctx)
    } else {
        Vec::new()
    };
    // Thunks of the function values taken (lowering one takes none), by
    // symbol.
    let mut thunks: Vec<FunctionId> = std::mem::take(&mut ctx.thunks).into_iter().collect();
    thunks.sort_by(|a, b| {
        let name = |id: &FunctionId| ctx.obj.syms[fns[id].sym as usize].name.clone();
        name(a).cmp(&name(b))
    });
    for id in thunks {
        let sig = &fns[&id];
        let name = format!("{}.v", ctx.obj.syms[sig.sym as usize].name);
        let sym = ctx.sym(&name);
        let mut f = thunk(&name, id, sig);
        // A debugger shows a function value as the function itself.
        if let Some(g) = module.functions.iter().find(|g| g.id == id) {
            f.name.clone_from(&g.name);
            f.span = g.span;
            let at = nabic_mir::Statement::Loc(g.span, nabic_mir::Scope::default());
            f.blocks[0].stmts.insert(0, at);
        }
        funcs.push((FnLower::new(&mut ctx, &f, sym).lower(), false, false));
    }
    for (id, a, exported, span) in &c_entries {
        let sig = &fns[id];
        let Some(entry) = sig.c_entry else { continue };
        let name = ctx.obj.syms[entry as usize].name.clone();
        let wasm_export = wasm
            && view
                .iter()
                .any(|f| f.id == *id && f.facts.export_name.is_some());
        // No `setjmp` without a C library: a panic there aborts.
        let on_panic = view
            .iter()
            .find(|f| f.id == *id)
            .and_then(|f| f.facts.on_panic.clone())
            .filter(|_| !wasm && t.os != "none");
        let func = wrap::generated(&mut ctx, &name, entry, *span, |l| {
            l.c_entry(sig, a, wasm_export, on_panic.as_ref())
        });
        if wasm_export && t.reactor() {
            // A browser module exports it to JS under the same name.
            if let Some(w) = ctx.obj.wasm.as_mut() {
                w.exports.insert(entry);
            }
        }
        funcs.push((func, *exported, false));
    }
    if glue {
        glue_code(
            module,
            opts,
            unit.is_some(),
            &fns,
            &mut ctx,
            &mut funcs,
            &mut errors,
            pre_tables,
            &values,
        );
    }
    errors.append(&mut ctx.errors);
    drop(ctx);

    let defs = if defs {
        crate::repl::defs(&obj, &funcs, &keys)
    } else {
        Vec::new()
    };
    let opt = opts.opt_level != crate::OptLevel::O0;
    let track = opt && opts.debug;
    // What each callee does with the pointers it is passed (`frame`).
    let byref: Vec<_> = funcs
        .iter_mut()
        .flat_map(|f| std::mem::take(&mut f.0.byref))
        .collect();
    let caps = opt.then(|| {
        let all: Vec<&Func> = funcs.iter().map(|f| &f.0).collect();
        std::sync::Arc::new(crate::frame::caps(&all, &byref, t.pointer_width / 8))
    });
    for (f, global, weak) in &mut funcs {
        f.opt = opt;
        f.caps.clone_from(&caps);
        f.track = track && f.debug.is_some();
        if let Some(d) = &mut f.debug {
            d.opt = f.track;
        }
        emit(&mut obj, f);
        let s = &mut obj.syms[f.sym as usize];
        s.global = *global;
        s.weak = *weak;
    }
    Compiled {
        obj,
        gc_types,
        errors,
        defs,
        quiet,
    }
}

/// Bytes of stack arguments a `stdcall` C function of this signature
/// names in its `@N` (`lower::call_c`'s shapes, the result pointer left
/// out).
fn stdcall_bytes(
    t: &nabic_mir::Target,
    conv: cc::Conv,
    params: &[MirTy],
    ret: &MirTy,
    a: &abi::FnAbi,
) -> u32 {
    let pw = t.pointer_width;
    let shapes: Vec<cc::Shape> = params
        .iter()
        .zip(&a.args)
        .map(|(p, x)| match x {
            abi::ArgAbi::Direct if *p == MirTy::Prim(PrimTy::Str) => {
                cc::coerced(&abi::CType::PTR, pw)
            }
            abi::ArgAbi::Direct => cc::shape(p, t),
            abi::ArgAbi::Coerce(c) => cc::coerced(c, pw),
            abi::ArgAbi::Indirect { byval: true } => {
                let size = t.size_align(p).0;
                cc::byval(size.max(1), abi::byval_align(p, t), pw)
            }
            abi::ArgAbi::Indirect { byval: false } => cc::coerced(&abi::CType::PTR, pw),
        })
        .collect();
    let sret = a.ret == abi::RetAbi::Sret;
    let r = match &a.ret {
        abi::RetAbi::Coerce(c) => Some(cc::coerced(c, pw)),
        abi::RetAbi::Direct if *ret != MirTy::Unit => Some(cc::shape(ret, t)),
        _ => None,
    };
    // The callee pops the hidden result pointer too, but the name, as
    // MSVC's, does not count it.
    let hidden = if sret { pw / 8 } else { 0 };
    cc::assign(conv, &shapes, r.as_ref(), sret, None).stack - hidden
}

/// `<f>.v`, a function value of nabi body `id`: takes `sig`'s parameters
/// and passes them on, with this thread's `nabi_tls` address.
fn thunk(name: &str, id: FunctionId, sig: &Sig) -> nabic_mir::MirFunction {
    let n = u32::try_from(sig.params.len()).expect("parameter count");
    let mut f = wrap::shell(name);
    f.params = (0..n).map(LocalId).collect();
    f.param_tys.clone_from(&sig.params);
    f.ret_ty = sig.ret.clone();
    f.locals = sig
        .params
        .iter()
        .chain([&sig.ret])
        .zip(0..)
        .map(|(ty, i)| LocalDecl {
            id: LocalId(i),
            name: None,
            ty: ty.clone(),
        })
        .collect();
    let ret = (sig.ret != MirTy::Unit).then_some(Operand::Copy(LocalId(n)));
    f.blocks = vec![BasicBlock {
        id: nabic_ir_common::BlockId(0),
        params: Vec::new(),
        stmts: vec![Statement::Assign {
            target: LocalId(n),
            rvalue: RValue::Call {
                callee: id,
                args: (0..n).map(|i| Operand::Copy(LocalId(i))).collect(),
                ret_ty: sig.ret.clone(),
            },
        }],
        terminator: Terminator::Return(ret),
    }];
    f
}

/// Once per program: the C `main`, statics, `nabi.register_types` (GC types
/// from `pre_tables` on, static roots). A unit build's glue exports its
/// statics.
#[allow(clippy::too_many_arguments)]
fn glue_code(
    module: &MirModule,
    opts: &Options,
    units: bool,
    fns: &HashMap<FunctionId, Sig>,
    ctx: &mut Ctx<'_>,
    funcs: &mut Vec<(Func, bool, bool)>,
    errors: &mut Vec<(nabic_diagnostic::Span, String)>,
    pre_tables: usize,
    values: &[(FunctionId, SymId)],
) {
    let t = &module.target;
    let plugin = ctx.plugin == nabic_mir::plugin::Mode::Plugin;
    let pkgs = ctx.pkgs;
    if pkgs.link.c {
        c_statics(module, fns, ctx);
        return;
    }
    let defines = |name: &str| {
        opts.repl
            .as_ref()
            .is_some_and(|r| r.define.iter().any(|d| d == name))
    };
    let prebuilt_static = |name: &str| {
        pkgs.link.in_prebuilt(name)
            || opts
                .repl
                .as_ref()
                .is_some_and(|r| r.statics.iter().any(|s| s == name))
    };
    let gc_type_base = ctx.gc_type_base;
    // `nabi.register_types` (the dependencies' first), called from `main`.
    let register_name = match &pkgs.link.lib {
        Some(lib) => format!("nabi.register_types.{lib}"),
        None => "nabi.register_types".to_owned(),
    };
    let register = ctx.sym(&register_name);
    if let Some(m) = module
        .functions
        .iter()
        .find(|f| f.facts.role == FnRole::Main)
    {
        let user = fns[&m.id].sym;
        if opts.repl.is_some() {
            let s = ctx.sym("nabi_repl_cell");
            let func = wrap::generated(ctx, "nabi_repl_cell", s, m.span, |l| {
                l.repl_cell(user, &m.ret_ty, register)
            });
            funcs.push((func, true, false));
        } else if ctx.obj.wasm.is_some() {
            // wasi-libc's `_start` calls `__main_void`; a browser module
            // exports `main` instead.
            let name = if t.reactor() { "main" } else { "__main_void" };
            let s = ctx.sym(name);
            let mut func = wrap::generated(ctx, name, s, m.span, |l| {
                l.main_wrapper(user, &m.ret_ty, register)
            });
            if let Some(d) = func.debug.as_mut().filter(|_| t.reactor()) {
                "nabi_main".clone_into(&mut d.name);
            }
            funcs.push((func, true, false));
            if let Some(w) = ctx.obj.wasm.as_mut().filter(|_| t.reactor()) {
                w.exports.insert(s);
            }
            let inited = ctx.sym("nabi.inited");
            ctx.obj.define_data(inited, Sect::Data, &[0], 1, false);
            let s = ctx.sym("nabi_init_once");
            let func = wrap::generated(ctx, "nabi_init_once", s, m.span, |l| {
                l.init_once(inited, register)
            });
            funcs.push((func, true, false));
        } else {
            // The C entry: `main` where the C library's startup calls it,
            // `nabi_main` under nabi's own Windows startup. Debuggers show
            // it as `nabi_main`, so only the program's own is `main`.
            let name = if ctx.obj.coff { "nabi_main" } else { "main" };
            let s = ctx.sym(name);
            let mut func = wrap::generated(ctx, name, s, m.span, |l| {
                l.main_wrapper(user, &m.ret_ty, register)
            });
            if let Some(d) = func.debug.as_mut() {
                "nabi_main".clone_into(&mut d.name);
            }
            funcs.push((func, true, false));
        }
    }
    // Statics.
    for s in &module.statics {
        // A prebuilt package's statics are in its `.a`; others defined
        // outside get a symbol only if used (DLL ones via the import
        // address).
        if pkgs.link.in_prebuilt(&s.name) || (s.is_extern && !defines(&s.name)) {
            continue;
        }
        let name = pkgs.static_sym(&s.name);
        // A plugin's own copy of a static it takes from the process
        // (`plugin::own`).
        let mine = crate::plugin::imported(module, &s.name);
        let own = |n: String| crate::plugin::own(n, mine);
        let sym = ctx.sym(&own(name.clone()));
        let (size, align) = t.size_align(&s.ty);
        if s.is_extern && defines(&s.name) {
            // A zero-sized slot still takes a byte: ELF drops an empty
            // section and the symbol with it, leaving the slot undefined.
            let size = size.max(1);
            ctx.obj
                .define_data(sym, Sect::Data, &vec![0; size as usize], align, true);
            continue;
        }
        let lazy = pkgs.lazy_statics.get(&s.name).cloned();
        if prebuilt_static(&s.name) {
            // An earlier REPL cell's `.init` stays private, made only if
            // this object reads the static.
            if let Some(init) = lazy.filter(|_| ctx.obj.named(&format!("{name}.init"))) {
                let state = ctx.sym(&format!("{name}.state"));
                let init = ctx.sym(&init);
                let f_sym = ctx.sym(&format!("{name}.init"));
                let iname = format!("{name}.init");
                let func =
                    wrap::generated(ctx, &iname, f_sym, nabic_diagnostic::Span::DUMMY, |l| {
                        l.lazy_init(state, sym, true, init, &s.ty)
                    });
                funcs.push((func, false, false));
            }
            continue;
        }
        let shared = pkgs.link.shared_static(&s.name) && !mine;
        // A host lists its statics for plugins to bind to.
        let host = module.plugin.mode == nabic_mir::plugin::Mode::Host;
        let global = host
            || opts.repl.is_some()
            || units
            || shared
            || pkgs
                .link
                .lib
                .as_deref()
                .is_some_and(|l| Link::owns(l, &s.name));
        if let Some(init) = lazy {
            ctx.obj
                .define_data(sym, Sect::Data, &vec![0; size as usize], align, global);
            let state = ctx.sym(&own(format!("{name}.state")));
            ctx.obj.define_data(state, Sect::Data, &[0], 1, global);
            if shared {
                ctx.obj.syms[sym as usize].weak = true;
                ctx.obj.syms[state as usize].weak = true;
            }
            let init = ctx.sym(&init);
            let iname = own(format!("{name}.init"));
            let f_sym = ctx.sym(&iname);
            let func = wrap::generated(ctx, &iname, f_sym, nabic_diagnostic::Span::DUMMY, |l| {
                l.lazy_init(state, sym, false, init, &s.ty)
            });
            // Exported by a prebuilt library that owns the static (and to
            // the other units).
            let export = host
                || units
                || shared
                || pkgs
                    .link
                    .lib
                    .as_deref()
                    .is_some_and(|l| Link::owns(l, &s.name));
            funcs.push((func, export, shared));
            continue;
        }
        if let ConstVal::Str(text) = &s.init {
            let lit = ctx.obj.bytes(text.as_bytes());
            let pw = t.pointer_width / 8;
            let mut bytes = vec![0u8; 2 * pw as usize];
            let len = ctx.obj.int_bytes(text.len() as u64, pw as usize);
            bytes[pw as usize..].copy_from_slice(&len);
            let sect = if s.is_mut {
                Sect::Data
            } else {
                Sect::DataConst
            };
            let off = ctx.obj.define_data(sym, sect, &bytes, pw, global);
            ctx.obj.syms[sym as usize].weak = shared;
            ctx.obj.abs64(sect, off, lit);
            continue;
        }
        match const_bytes(&s.init, &s.ty, t) {
            Some(bytes) => {
                let sect = if s.is_mut { Sect::Data } else { Sect::Const };
                ctx.obj.define_data(sym, sect, &bytes, align, global);
                ctx.obj.syms[sym as usize].weak = shared;
            }
            None => errors.push((
                nabic_diagnostic::Span::DUMMY,
                format!("unsupported in codegen: initializer of static `{}`", s.name),
            )),
        }
    }
    // GC support: the ScanTables and the static roots.
    let mut tables = Vec::new();
    for i in pre_tables..ctx.gc_types.len() {
        let words = ctx.gc_types[i].clone();
        let bytes: Vec<u8> = words
            .iter()
            .flat_map(|w| ctx.obj.int_bytes(u64::from(*w), 4))
            .collect();
        let sym = ctx.sym(&nabic_codegen::units::scan_symbol(&words));
        ctx.obj.define_data(sym, Sect::Const, &bytes, 4, true);
        let s = &mut ctx.obj.syms[sym as usize];
        s.weak = true;
        s.hidden = true;
        let id = gc_type_base + i as u32;
        tables.push((id, sym));
        // A unit build's glue: the id units read (`Lower::gc_tid`).
        if let Some(pkg) = ctx.gc_pkg {
            let s = ctx.sym(&nabic_codegen::units::gc_symbol(pkg, &words));
            let id = ctx.obj.int_bytes(u64::from(id), 4);
            ctx.obj.define_data(s, Sect::Const, &id, 4, true);
        }
    }
    let mut roots = Vec::new();
    for s in &module.statics {
        if !((!s.is_extern || defines(&s.name)) && !prebuilt_static(&s.name)) {
            continue;
        }
        let sym = ctx.sym(&pkgs.static_sym(&s.name));
        for off in t.root_offsets(&s.ty) {
            roots.push((sym, true, off));
        }
    }
    let mut deps: Vec<SymId> = pkgs
        .link
        .prebuilt
        .iter()
        .map(|d| ctx.sym(&format!("nabi.register_types.{d}")))
        .collect();
    deps.extend(crate::typeinfo::register_sym(ctx));
    deps.extend(crate::plugin::emit(module, ctx, values, &tables, funcs));
    // A plugin's tables are the loader's.
    if plugin {
        return;
    }
    // Above `-O0` the tables (consecutive ids) as an array, registered
    // by one call.
    let consecutive = tables.windows(2).all(|p| p[1].0 == p[0].0 + 1);
    let list = (ctx.opt && tables.len() >= 4 && consecutive).then(|| {
        let pw = t.pointer_width / 8;
        let arr = ctx.sym(&format!("{register_name}.tables"));
        let bytes = vec![0u8; (pw as usize) * tables.len()];
        let off = ctx.obj.define_data(arr, Sect::DataConst, &bytes, pw, false);
        for (i, &(_, sym)) in tables.iter().enumerate() {
            ctx.obj.abs64(Sect::DataConst, off + pw * i as u32, sym);
        }
        (tables[0].0, arr, tables.len() as u32)
    });
    let func = wrap::generated(
        ctx,
        &register_name,
        register,
        nabic_diagnostic::Span::DUMMY,
        |l| l.register_types(&tables, list, &roots, &deps),
    );
    funcs.push((func, pkgs.link.lib.is_some(), false));
}

/// The C objects of a C translation unit, their addresses in place
/// (relocations with in-place addends).
fn c_statics(module: &MirModule, fns: &HashMap<FunctionId, Sig>, ctx: &mut Ctx<'_>) {
    let t = &module.target;
    let pw = t.pointer_width / 8;
    for s in &module.statics {
        let Some(c) = &s.c else { continue };
        if s.is_extern {
            continue;
        }
        let sym = ctx.sym(&s.name);
        let align = t.size_align(&s.ty).1;
        let mut bytes = c.bytes.clone();
        for r in &c.relocs {
            let at = r.at as usize;
            let v = ctx.obj.int_bytes(r.addend as u64, pw as usize);
            bytes[at..at + pw as usize].copy_from_slice(&v);
        }
        let sect = if s.is_mut {
            Sect::Data
        } else if c.relocs.is_empty() {
            Sect::Const
        } else {
            Sect::DataConst
        };
        let off = ctx.obj.define_data(sym, sect, &bytes, align, c.global);
        for r in &c.relocs {
            let to = match &r.to {
                nabic_mir::CRef::Static(n) => ctx.sym(n),
                nabic_mir::CRef::Fn(id) => fns[id].sym,
            };
            ctx.obj.abs64(sect, off + r.at, to);
        }
    }
}

/// A scalar static's initial bytes.
fn const_bytes(v: &ConstVal, ty: &MirTy, t: &nabic_mir::Target) -> Option<Vec<u8>> {
    if let (ConstVal::Bytes(b), MirTy::Array(..)) = (v, ty) {
        return Some(b.clone());
    }
    let MirTy::Prim(p) = ty else { return None };
    let size = t.size_align(ty).0 as usize;
    let be = t.endian == "big";
    let int = |x: i128| {
        Some(if be {
            x.to_be_bytes()[16 - size..].to_vec()
        } else {
            x.to_le_bytes()[..size].to_vec()
        })
    };
    let f32b = |v: f32| int(i128::from(v.to_bits()));
    let f64b = |v: f64| int(i128::from(v.to_bits()));
    match v {
        ConstVal::Int { value, .. } if p.is_int() => int(*value),
        ConstVal::UInt { value, .. } if p.is_int() => int(*value as i128),
        ConstVal::Char(c) if p.is_int() => int(i128::from(*c)),
        ConstVal::Bool(b) if *p == PrimTy::Bool => Some(vec![u8::from(*b)]),
        ConstVal::Float { bits, ty: fp } if p.is_float() => {
            let v = if *fp == PrimTy::F32 {
                f64::from(f32::from_bits(*bits as u32))
            } else {
                f64::from_bits(*bits)
            };
            if *p == PrimTy::F32 {
                f32b(v as f32)
            } else {
                f64b(v)
            }
        }
        ConstVal::Int { value, .. } if p.is_float() => {
            let v = *value as f64;
            if *p == PrimTy::F32 {
                f32b(v as f32)
            } else {
                f64b(v)
            }
        }
        _ => None,
    }
}
