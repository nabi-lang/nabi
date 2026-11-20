//! HIR -> MIR lowering: basic blocks over locals, block parameters at
//! joins.

pub mod niche;

mod control;
mod expr;
mod pattern;
mod place;
mod plugin;

use crate::ty::{code_ty, debug_ty, enum_payload_ty, fat_fn, lower_ty, sum_tag};
use crate::{
    BasicBlock, DebugTy, DebugTypes, LocalDecl, MirFunction, MirModule, MirStaticDef, MirTy,
    Operand, RValue, Statement, Target, Terminator,
};
use nabic_core::{HashMap, HashSet};
use nabic_hir::{
    HirBlock, HirExpr, HirExprKind, HirFunction, HirMatchArm, HirModule, HirPattern, HirStmt, HirTy,
};
use nabic_ir_common::{
    Abi, BlockId, ConstVal, FnFacts, FunctionId, Linkage, LocalId, PrimOp, PrimTy as IrPrimTy,
    PrimTy,
};

#[must_use]
pub fn lower(module: &HirModule, target: &Target) -> MirModule {
    let mut with_reprs = target.clone();
    with_reprs.reprs = std::sync::Arc::new(
        module
            .structs
            .iter()
            .filter(|s| s.repr != nabic_hir::Repr::default())
            .map(|s| (s.id.0, s.repr))
            .collect(),
    );
    // A byte-array union is as aligned as its most aligned member.
    let raised: Vec<(u32, nabic_hir::Repr)> = module
        .structs
        .iter()
        .filter_map(|s| {
            let a = crate::ty::union_align(s, module, &with_reprs);
            (a > s.repr.align).then_some((s.id.0, nabic_hir::Repr { align: a, ..s.repr }))
        })
        .collect();
    if !raised.is_empty() {
        let mut reprs = (*with_reprs.reprs).clone();
        reprs.extend(raised);
        with_reprs.reprs = std::sync::Arc::new(reprs);
    }
    with_reprs.unions = std::sync::Arc::new(
        module
            .structs
            .iter()
            .filter_map(|s| {
                let u = s.union_kind.as_ref()?;
                let ms = u
                    .aliases
                    .iter()
                    .map(|(_, t)| lower_ty(t, module, &with_reprs))
                    .collect();
                Some((s.id.0, ms))
            })
            .collect(),
    );
    let target = &with_reprs;
    // The descriptors the reached code asks for, and what they need.
    let (live, roots) = live_descs(module);
    let mut statics: Vec<MirStaticDef> = module
        .statics
        .iter()
        .map(|s| MirStaticDef {
            name: s.name.clone(),
            ty: lower_ty(&s.ty, module, target),
            init: s.init.clone(),
            is_mut: s.is_mut,
            is_extern: s.is_extern,
            dll_import: s.dll_import,
            init_fn: s.init_fn,
            c: None,
        })
        .collect();
    // A descriptor is an extern static to the code that takes its
    // address; the glue defines it (`typeinfo`).
    statics.extend(
        live.iter()
            .map(|&i| &module.type_descs[i])
            .map(|d| MirStaticDef {
                name: d.symbol.clone(),
                ty: MirTy::Prim(PrimTy::Usize),
                init: ConstVal::Unit,
                is_mut: false,
                is_extern: true,
                dll_import: false,
                init_fn: None,
                c: None,
            }),
    );
    let fnv = FnValues {
        wrappers: std::cell::RefCell::new(std::collections::BTreeMap::new()),
        closures: std::cell::RefCell::new(std::collections::BTreeMap::new()),
        coro_ctors: std::cell::RefCell::new(std::collections::BTreeMap::new()),
        next_id: std::cell::Cell::new(
            module
                .functions
                .iter()
                .map(|f| f.id.0 + 1)
                .max()
                .unwrap_or(0),
        ),
        c_externs: module
            .functions
            .iter()
            .filter(|f| f.facts.linkage == Linkage::Extern && matches!(f.facts.abi, Abi::C { .. }))
            .map(|f| f.id)
            .collect(),
    };
    // The stubs no lowered body reaches are not code, nor what the roots
    // do not reach.
    let unlowered: nabic_core::HashSet<FunctionId> = module.unlowered.iter().copied().collect();
    let reached = roots.as_ref().map(|r| reached(module, r));
    let lowered: Vec<&HirFunction> = module
        .functions
        .iter()
        .filter(|f| {
            !unlowered.contains(&f.id) && reached.as_ref().is_none_or(|r| r.contains(&f.id))
        })
        .collect();
    let ret_tys: HashMap<FunctionId, MirTy> = lowered
        .iter()
        .map(|f| (f.id, lower_ty(&f.ret_ty, module, target)))
        .collect();
    let mut functions: Vec<MirFunction> = lowered
        .iter()
        .map(|f| lower_fn(f, module, target, &ret_tys, &fnv))
        .collect();
    let ptrs = PtrTypes::new(module);
    for (f, hf) in functions.iter_mut().zip(&lowered) {
        if ptrs.in_fn(hf, f) {
            f.pins = f
                .locals
                .iter()
                .filter(|l| target.has_gc_ref(&l.ty))
                .map(|l| l.id)
                .collect();
        }
    }
    // Coroutine bodies -> resume functions + constructors.
    for (i, hf) in lowered.iter().enumerate() {
        if !(hf.is_async || hf.is_generator) || hf.body.is_none() {
            continue;
        }
        let ctor = *fnv
            .coro_ctors
            .borrow_mut()
            .entry(hf.id)
            .or_insert_with(|| fnv.fresh_id());
        // An `async` body takes the poll's `Context` on every resume, a
        // generator whether it is being closed.
        let n_resume = usize::from(hf.is_async || hf.is_generator);
        let ctor_fn = crate::coro::transform(&mut functions[i], n_resume, ctor, |t| {
            // A union away from its tag scans as ambiguous words.
            let (size, word) = (target.size_align(t).0, target.pointer_width / 8);
            if size % word == 0 {
                MirTy::Union(vec![(0, t.clone())], size / word, crate::UnionKind::Plain)
            } else {
                t.clone()
            }
        });
        functions.push(ctor_fn);
    }
    // A descriptor holds its accessors as function values.
    let renumber: HashMap<u32, u32> = live
        .iter()
        .enumerate()
        .map(|(new, &old)| (old as u32, new as u32))
        .collect();
    let type_descs: Vec<_> = live
        .iter()
        .map(|&i| &module.type_descs[i])
        .map(|d| {
            let mut m = d.map(lower_ty(&d.ty, module, target), |id| {
                *fnv.wrappers
                    .borrow_mut()
                    .entry(id)
                    .or_insert_with(|| fnv.fresh_id())
            });
            if let HirTy::Enum(id) = &d.ty {
                variant_offsets(&mut m, *id, module, target);
            }
            if module.plugin.hot.is_some() {
                sum_variants(&mut m, &d.ty, &module.type_descs, target);
            }
            let re = |i: &mut u32| *i = renumber[i];
            m.generics.iter_mut().for_each(re);
            let fields = m
                .fields
                .iter_mut()
                .chain(m.variants.iter_mut().flat_map(|v| &mut v.fields));
            for f in fields {
                re(&mut f.ty);
                f.attrs.iter_mut().for_each(|a| re(&mut a.ty));
            }
            m.attrs.iter_mut().for_each(|a| re(&mut a.ty));
            m.variants
                .iter_mut()
                .flat_map(|v| &mut v.attrs)
                .for_each(|a| re(&mut a.ty));
            m
        })
        .collect();
    // A plugin's exports and every dispatcher of a plugin or host are
    // reached as function values from the module descriptor.
    let valued = plugin::valued(module);
    for &id in &valued {
        fnv.wrappers
            .borrow_mut()
            .entry(id)
            .or_insert_with(|| fnv.fresh_id());
    }
    finish_fn_values(&mut functions, &fnv);
    if let Some(roots) = &roots {
        let mut roots = roots.clone();
        roots.extend(
            type_descs
                .iter()
                .flat_map(nabic_ir_common::typedesc::TypeDesc::functions),
        );
        let wrappers = fnv.wrappers.borrow();
        let more = valued
            .iter()
            .flat_map(|id| [Some(*id), wrappers.get(id).copied()]);
        roots.extend(more.flatten());
        retain_reachable(&mut functions, roots);
    }
    let next_id = functions.iter().map(|f| f.id.0 + 1).max().unwrap_or(0);
    let copies = crate::specialize::closures(&mut functions, next_id);
    for f in &mut functions {
        crate::rotate::rotate(f);
        crate::inplace::copy_into(f);
    }
    crate::version::version(&mut functions, target.pointer_width);
    crate::strip::strip(&mut functions);
    crate::names::stable_names(&mut functions, &nabic_hir::type_name);
    let debug = debug_types(module, &functions, &copies, target);
    let mut m = MirModule {
        debug,
        functions,
        statics,
        struct_names: module
            .structs
            .iter()
            .map(|s| {
                let fields = s.fields.iter().map(|f| f.name.clone()).collect();
                (s.id.0, (crate::ty::struct_debug_name(s, module), fields))
            })
            .collect(),
        struct_keys: module
            .structs
            .iter()
            .map(|s| {
                let key = nabic_hir::type_name(false, s.id.0).unwrap_or_else(|| s.name.clone());
                (s.id.0, key)
            })
            .collect(),
        target: target.clone(),
        link: crate::Link::default(),
        type_descs,
        plugin: crate::plugin::Plugin::default(),
    };
    let wrappers = fnv.wrappers.borrow();
    crate::plugin::finish(&mut m, module, &|id| wrappers.get(&id).copied());
    m
}

/// The `DebugTy`s of the named locals of `functions` and of struct
/// fields, where they tell more than the `MirTy`. A specialized copy's
/// locals are its original's (`copies`: `(copy, original)`).
fn debug_types(
    module: &HirModule,
    functions: &[MirFunction],
    copies: &[(FunctionId, FunctionId)],
    target: &Target,
) -> DebugTypes {
    let mut out = DebugTypes::default();
    let lowered: HashSet<FunctionId> = functions.iter().map(|f| f.id).collect();
    for f in module.functions.iter().filter(|f| lowered.contains(&f.id)) {
        for l in f.locals.iter().filter(|l| l.name.is_some()) {
            let d = match &l.ty {
                HirTy::GcBuf(t) if l.boxed => Some(DebugTy::Cell(Box::new(
                    debug_ty(t, module, target)
                        .unwrap_or_else(|| DebugTy::Mir(lower_ty(t, module, target))),
                ))),
                t => debug_ty(t, module, target),
            };
            if let Some(d) = d {
                out.locals.insert((f.id, l.id), d);
            }
        }
    }
    for &(copy, orig) in copies {
        let of: Vec<_> = out
            .locals
            .iter()
            .filter(|((f, _), _)| *f == orig)
            .map(|((_, l), d)| ((copy, *l), d.clone()))
            .collect();
        out.locals.extend(of);
    }
    out.any = crate::ty::any_variants(module, target);
    for s in module.structs.iter().filter(|s| s.union_kind.is_none()) {
        let fields: Vec<Option<DebugTy>> = s
            .fields
            .iter()
            .map(|f| debug_ty(&f.ty, module, target))
            .collect();
        if fields.iter().any(Option::is_some) {
            let fields = fields
                .into_iter()
                .zip(&s.fields)
                .map(|(d, f)| d.unwrap_or_else(|| DebugTy::Mir(lower_ty(&f.ty, module, target))))
                .collect();
            out.fields.insert(s.id.0, fields);
        }
    }
    out
}

/// What lowering function values (`{code, env}`) leaves to do
/// once every body is lowered.
struct FnValues {
    /// Named fn -> its `(env, args..)` wrapper (the code of `f` as a value).
    wrappers: std::cell::RefCell<std::collections::BTreeMap<FunctionId, FunctionId>>,
    /// Lifted closure body -> its env's MIR type: the body gets the env as
    /// a `GcRef` and loads it on entry.
    closures: std::cell::RefCell<std::collections::BTreeMap<FunctionId, MirTy>>,
    next_id: std::cell::Cell<u32>,
    /// Coroutine body -> its constructor (`coro::transform`).
    coro_ctors: std::cell::RefCell<std::collections::BTreeMap<FunctionId, FunctionId>>,
    /// C `extern` declarations: their `fn` parameters are bare C pointers.
    c_externs: nabic_core::HashSet<FunctionId>,
}

impl FnValues {
    fn fresh_id(&self) -> FunctionId {
        let id = FunctionId(self.next_id.get());
        self.next_id.set(id.0 + 1);
        id
    }
}

/// The structs and enums whose values hold a `bare::ptr` pointer,
/// or whose type arguments name one (`List<ConstPtr<u8>>`).
struct PtrTypes {
    structs: HashSet<nabic_hir::StructId>,
    enums: HashSet<nabic_hir::EnumId>,
}

impl PtrTypes {
    fn new(module: &HirModule) -> Self {
        let mut p = Self {
            structs: module
                .structs
                .iter()
                .filter(|s| s.name.starts_with("bare::ptr::"))
                .map(|s| s.id)
                .collect(),
            enums: HashSet::default(),
        };
        loop {
            let n = p.structs.len() + p.enums.len();
            for s in &module.structs {
                if s.generic_args
                    .iter()
                    .chain(s.fields.iter().map(|f| &f.ty))
                    .any(|t| p.hir(t))
                {
                    p.structs.insert(s.id);
                }
            }
            for e in &module.enums {
                let payloads = e.variants.iter().filter_map(|v| v.payload_ty.as_ref());
                if e.generic_args.iter().chain(payloads).any(|t| p.hir(t)) {
                    p.enums.insert(e.id);
                }
            }
            if p.structs.len() + p.enums.len() == n {
                return p;
            }
        }
    }

    fn hir(&self, t: &HirTy) -> bool {
        match t {
            HirTy::Struct(id) => self.structs.contains(id),
            HirTy::Enum(id) => self.enums.contains(id),
            HirTy::Sum(ts) | HirTy::Tuple(ts) => ts.iter().any(|t| self.hir(t)),
            HirTy::Array(t, _) => self.hir(t),
            HirTy::Fn(ps, r, _) => ps.iter().any(|t| self.hir(t)) || self.hir(r),
            _ => false,
        }
    }

    fn mir(&self, t: &MirTy) -> bool {
        match t {
            MirTy::Struct(id, ts) => self.structs.contains(id) || ts.iter().any(|t| self.mir(t)),
            MirTy::Tuple(ts) => ts.iter().any(|t| self.mir(t)),
            MirTy::Union(vs, _, _) => vs.iter().any(|(_, t)| self.mir(t)),
            MirTy::Array(t, _) => self.mir(t),
            MirTy::Fn(ps, r) => ps.iter().any(|t| self.mir(t)) || self.mir(r),
            _ => false,
        }
    }

    /// Whether a pointer can appear in `f` (a local or temporary's type
    /// names one).
    fn in_fn(&self, hf: &HirFunction, f: &MirFunction) -> bool {
        hf.locals.iter().any(|l| self.hir(&l.ty)) || f.locals.iter().any(|l| self.mir(&l.ty))
    }
}

fn finish_fn_values(functions: &mut Vec<MirFunction>, fnv: &FnValues) {
    let at: HashMap<FunctionId, usize> = functions
        .iter()
        .enumerate()
        .map(|(i, f)| (f.id, i))
        .collect();
    // Closure bodies: param 0 becomes the boxed env, unpacked on entry.
    for (id, env_ty) in fnv.closures.borrow().iter() {
        let Some(f) = at.get(id).map(|&i| &mut functions[i]) else {
            continue;
        };
        let (Some(&env_local), false) = (f.params.first(), f.blocks.is_empty()) else {
            continue;
        };
        let boxed = LocalId(u32::try_from(f.locals.len()).expect("too many locals"));
        f.locals.push(LocalDecl {
            id: boxed,
            name: Some("#env".to_owned()),
            ty: MirTy::GcRef,
        });
        f.params[0] = boxed;
        f.param_tys[0] = MirTy::GcRef;
        f.blocks[0].stmts.insert(
            0,
            Statement::Assign {
                target: env_local,
                rvalue: RValue::HeapLoad {
                    ptr: Operand::Copy(boxed),
                    ty: env_ty.clone(),
                },
            },
        );
    }
    // Named fns used as values: `fn #w(env, args..) { return f(args..); }`.
    for (target, wrapper) in fnv.wrappers.borrow().iter() {
        let Some(f) = at.get(target).map(|&i| &functions[i]) else {
            continue;
        };
        let mut locals = vec![LocalDecl {
            id: LocalId(0),
            name: Some("#env".to_owned()),
            ty: MirTy::GcRef,
        }];
        let mut params = vec![LocalId(0)];
        for t in &f.param_tys {
            let id = LocalId(u32::try_from(locals.len()).expect("too many locals"));
            locals.push(LocalDecl {
                id,
                name: None,
                ty: t.clone(),
            });
            params.push(id);
        }
        let ret = LocalId(u32::try_from(locals.len()).expect("too many locals"));
        locals.push(LocalDecl {
            id: ret,
            name: None,
            ty: f.ret_ty.clone(),
        });
        let unit = matches!(f.ret_ty, MirTy::Unit);
        let call = Statement::Assign {
            target: ret,
            rvalue: RValue::Call {
                callee: f.id,
                args: params[1..].iter().map(|l| Operand::Copy(*l)).collect(),
                ret_ty: f.ret_ty.clone(),
            },
        };
        let mut param_tys = vec![MirTy::GcRef];
        param_tys.extend(f.param_tys.iter().cloned());
        let w = MirFunction {
            id: *wrapper,
            def_id: None,
            name: format!("{}__fnval", f.name),
            params,
            locals,
            blocks: vec![BasicBlock {
                id: BlockId(0),
                params: Vec::new(),
                stmts: vec![call],
                terminator: Terminator::Return((!unit).then_some(Operand::Copy(ret))),
            }],
            param_tys,
            ret_ty: f.ret_ty.clone(),
            span: f.span,
            facts: FnFacts::default(),
            strips: Vec::new(),
            pins: Vec::new(),
            dbg: crate::FnDbg::default(),
        };
        functions.push(w);
    }
}

/// The functions `roots` reach through the HIR's calls, `fn` values,
/// closures and coroutines: a superset of what `retain_reachable` keeps.
fn reached(module: &HirModule, roots: &[FunctionId]) -> HashSet<FunctionId> {
    let index: HashMap<FunctionId, &HirFunction> =
        module.functions.iter().map(|f| (f.id, f)).collect();
    let mut live = HashSet::default();
    let mut work = roots.to_vec();
    while let Some(id) = work.pop() {
        if !live.insert(id) {
            continue;
        }
        let Some(body) = index.get(&id).and_then(|f| f.body.as_ref()) else {
            continue;
        };
        nabic_hir::each_block(body, &mut |e| {
            if let HirExprKind::Call { callee: id, .. }
            | HirExprKind::FnRef(id)
            | HirExprKind::ClosureValue { fn_id: id, .. }
            | HirExprKind::CoroNew { body_fn: id, .. } = &e.kind
            {
                work.push(*id);
            }
        });
    }
    live
}

/// The descriptors code reached from the roots asks for (with the ones
/// they name), in table order, and the roots with their accessors. A
/// descriptor some unreached body asked for is not emitted.
fn live_descs(module: &HirModule) -> (Vec<usize>, Option<Vec<FunctionId>>) {
    let descs = &module.type_descs;
    let Some(base) = &module.roots else {
        let all = (0..descs.len()).collect();
        let roots = None;
        return (all, roots);
    };
    let mut roots = base.clone();
    let mut live = vec![false; descs.len()];
    // A hot build describes every hot type it uses.
    let hot: Vec<u32> = module
        .plugin
        .hot
        .iter()
        .flat_map(|h| h.types.iter().map(|t| t.desc))
        .collect();
    loop {
        let fns = reached(module, &roots);
        let mut work: Vec<u32> = hot.clone();
        for f in module.functions.iter().filter(|f| fns.contains(&f.id)) {
            if let Some(b) = &f.body {
                nabic_hir::each_block(b, &mut |e| {
                    if let HirExprKind::TypeDesc(n) = e.kind {
                        work.push(n);
                    }
                });
            }
        }
        let before = roots.len();
        while let Some(n) = work.pop() {
            let i = n as usize;
            if std::mem::replace(&mut live[i], true) {
                continue;
            }
            let d = &descs[i];
            roots.extend(d.functions());
            work.extend(&d.generics);
            let fields = d
                .fields
                .iter()
                .chain(d.variants.iter().flat_map(|v| &v.fields));
            for f in fields {
                work.push(f.ty);
                work.extend(f.attrs.iter().map(|a| a.ty));
            }
            work.extend(d.attrs.iter().map(|a| a.ty));
            work.extend(d.variants.iter().flat_map(|v| v.attrs.iter().map(|a| a.ty)));
        }
        roots.sort_unstable();
        roots.dedup();
        if roots.len() == before {
            break;
        }
    }
    let kept = (0..descs.len()).filter(|&i| live[i]).collect();
    (kept, Some(roots))
}

/// The offsets of an enum descriptor's variant fields: the payload slot
/// after the `i32` tag, then the field within the variant's payload.
fn variant_offsets(
    d: &mut nabic_ir_common::typedesc::TypeDesc<MirTy>,
    id: nabic_hir::EnumId,
    module: &HirModule,
    target: &Target,
) {
    let Some(slot) = crate::ty::enum_payload_ty(id, module, target) else {
        return;
    };
    let at = target
        .size_align(&MirTy::Prim(PrimTy::I32))
        .0
        .next_multiple_of(target.size_align(&slot).1);
    let variants = &module.enums[id.0 as usize].variants;
    for v in &mut d.variants {
        let Some(p) = variants
            .iter()
            .find(|hv| hv.name == v.name)
            .and_then(|hv| hv.payload_ty.as_ref())
        else {
            continue;
        };
        let p = lower_ty(p, module, target);
        let offs = match &p {
            MirTy::Tuple(_) | MirTy::Struct(..) => target.field_offsets(&p),
            _ => vec![0],
        };
        for f in &mut v.fields {
            if let Some(o) = offs.get(f.index as usize) {
                f.offset = u64::from(at + o);
            }
        }
    }
}

/// A hot build's sum descriptor: a variant per member, named by its
/// key, holding it as field `0` after the `i32` tag; none for a sum laid
/// out otherwise (a niche).
fn sum_variants(
    d: &mut nabic_ir_common::typedesc::TypeDesc<MirTy>,
    ty: &HirTy,
    descs: &[nabic_ir_common::typedesc::TypeDesc<HirTy>],
    target: &Target,
) {
    let HirTy::Sum(parts) = ty else {
        return;
    };
    let MirTy::Tuple(t) = &d.ty else {
        return;
    };
    if t.len() != 2 || t[0] != MirTy::Prim(PrimTy::I32) || d.generics.len() != parts.len() {
        return;
    }
    let at = target.field_offsets(&d.ty).get(1).copied().unwrap_or(0);
    let gens = d.generics.clone();
    d.variants = (0u32..)
        .zip(gens)
        .map(|(i, g)| nabic_ir_common::typedesc::VariantDesc {
            name: descs[g as usize].key.clone(),
            tag: crate::ty::sum_tag(ty, i),
            fields: vec![nabic_ir_common::typedesc::FieldDesc {
                name: "0".to_owned(),
                renamed_from: String::new(),
                ty: g,
                flags: 1,
                index: 0,
                offset: u64::from(at),
                attrs: Vec::new(),
                init_default: None,
            }],
            attrs: Vec::new(),
        })
        .collect();
}

/// Keep the functions `roots` reach through calls and `fn` values.
fn retain_reachable(functions: &mut Vec<MirFunction>, roots: Vec<FunctionId>) {
    let index: HashMap<FunctionId, usize> = functions
        .iter()
        .enumerate()
        .map(|(i, f)| (f.id, i))
        .collect();
    let mut live: nabic_core::HashSet<FunctionId> = nabic_core::HashSet::default();
    let mut work = roots;
    while let Some(id) = work.pop() {
        if !live.insert(id) {
            continue;
        }
        let Some(&i) = index.get(&id) else { continue };
        for b in &functions[i].blocks {
            for s in &b.stmts {
                if let Statement::Assign {
                    rvalue: RValue::Call { callee, .. } | RValue::FnAddr { callee, .. },
                    ..
                } = s
                {
                    work.push(*callee);
                }
            }
        }
    }
    functions.retain(|f| live.contains(&f.id));
}

fn lower_fn(
    f: &HirFunction,
    module: &HirModule,
    target: &Target,
    ret_tys: &HashMap<FunctionId, MirTy>,
    fnv: &FnValues,
) -> MirFunction {
    let mut builder = FnBuilder::new(module, target, ret_tys, fnv);

    // Mirror HIR locals so HIR LocalId == MIR LocalId.
    for l in &f.locals {
        builder.locals.push(LocalDecl {
            id: l.id,
            name: l.name.clone(),
            ty: builder.ty(&l.ty),
        });
    }

    // An extern's `fn` parameter is a C function pointer (thin); the
    // runtime's `nabi_thread_spawn` takes the fat `{code, env}` value.
    let param_tys: Vec<MirTy> = f
        .params
        .iter()
        .map(|p| match (&p.ty, fnv.c_externs.contains(&f.id)) {
            (HirTy::Fn(args, ret, _), true) => MirTy::Fn(
                args.iter().map(|a| builder.ty(a)).collect(),
                Box::new(builder.ty(ret)),
            ),
            (t, _) => builder.ty(t),
        })
        .collect();
    let params: Vec<LocalId> = f.params.iter().map(|p| p.local).collect();

    if f.body.is_some() {
        let entry = builder.new_block();
        builder.current = entry;
        let returned = builder.lower_block(f.body.as_ref().unwrap(), true);
        if !returned {
            let term = if matches!(builder.ty(&f.ret_ty), MirTy::Unit) {
                Terminator::Return(None)
            } else {
                Terminator::Unreachable
            };
            builder.terminate(term);
        }
    }
    // No body -> no blocks -> emit declaration form in codegen.

    MirFunction {
        id: f.id,
        def_id: f.def_id,
        name: f.name.clone(),
        params,
        locals: builder.locals,
        blocks: reachable_blocks(builder.blocks),
        param_tys,
        ret_ty: lower_ty(&f.ret_ty, module, target),
        span: f.span,
        facts: f.facts.clone(),
        strips: Vec::new(),
        pins: Vec::new(),
        dbg: crate::FnDbg::default(),
    }
}

/// Drop blocks no path from the entry reaches (e.g. `match` arms after
/// a catch-all): no code for them.
fn reachable_blocks(blocks: Vec<BasicBlock>) -> Vec<BasicBlock> {
    let mut seen = vec![false; blocks.len()];
    let mut work = if blocks.is_empty() { vec![] } else { vec![0] };
    while let Some(i) = work.pop() {
        if std::mem::replace(&mut seen[i], true) {
            continue;
        }
        match &blocks[i].terminator {
            Terminator::Goto { target, .. } => work.push(target.0 as usize),
            Terminator::If {
                then_bb, else_bb, ..
            } => work.extend([then_bb.0 as usize, else_bb.0 as usize]),
            Terminator::Suspend { resume, .. } => work.push(resume.0 as usize),
            Terminator::Return(_) | Terminator::Unreachable => {}
        }
    }
    blocks
        .into_iter()
        .filter(|b| seen[b.id.0 as usize])
        .collect()
}

struct LoopFrame {
    /// Where `continue` jumps to. For `while`, the header (cond check);
    /// for `loop`, the start of the body.
    continue_bb: BlockId,
    /// Where `break` jumps to.
    break_bb: BlockId,
    /// Block param of `break_bb` carrying the loop's value, if any.
    break_param: Option<LocalId>,
    label: Option<String>,
    /// `false` for a labelled block: only `break 'label` targets it, an
    /// unlabelled `break` / `continue` goes to the enclosing loop.
    is_loop: bool,
}

struct FnBuilder<'a> {
    locals: Vec<LocalDecl>,
    blocks: Vec<BasicBlock>,
    current: BlockId,
    module: &'a HirModule,
    target: &'a Target,
    fn_ret_tys: &'a HashMap<FunctionId, MirTy>,
    fnv: &'a FnValues,
    loop_stack: Vec<LoopFrame>,
    /// Set of block ids that have been explicitly terminated. Once a
    /// block is terminated further `push_stmt` / `terminate` calls are
    /// dropped - the rest of the source path is dead code.
    terminated: nabic_core::HashSet<u32>,
    /// Lexical scope being lowered (`Statement::Loc`), and the last id.
    scope: crate::Scope,
    scopes: u32,
}

impl<'a> FnBuilder<'a> {
    fn new(
        module: &'a HirModule,
        target: &'a Target,
        fn_ret_tys: &'a HashMap<FunctionId, MirTy>,
        fnv: &'a FnValues,
    ) -> Self {
        Self {
            locals: Vec::new(),
            blocks: Vec::new(),
            current: BlockId(0),
            module,
            target,
            fn_ret_tys,
            fnv,
            loop_stack: Vec::new(),
            terminated: nabic_core::HashSet::default(),
            scope: crate::Scope::default(),
            scopes: 0,
        }
    }

    fn ty(&self, t: &HirTy) -> MirTy {
        lower_ty(t, self.module, self.target)
    }

    fn new_block(&mut self) -> BlockId {
        let id = BlockId(u32::try_from(self.blocks.len()).expect("too many blocks"));
        self.blocks.push(BasicBlock {
            id,
            params: Vec::new(),
            stmts: Vec::new(),
            terminator: Terminator::Unreachable,
        });
        id
    }

    fn add_block_param(&mut self, block: BlockId, ty: MirTy) -> LocalId {
        let local = self.fresh_local(ty);
        self.blocks[block.0 as usize].params.push(local);
        local
    }

    fn current_block(&mut self) -> &mut BasicBlock {
        &mut self.blocks[self.current.0 as usize]
    }

    fn block_is_live(&self) -> bool {
        !self.terminated.contains(&self.current.0)
    }

    fn push_stmt(&mut self, s: Statement) {
        if !self.block_is_live() {
            return;
        }
        self.current_block().stmts.push(s);
    }

    fn terminate(&mut self, t: Terminator) {
        if !self.block_is_live() {
            return;
        }
        self.current_block().terminator = t;
        self.terminated.insert(self.current.0);
    }

    fn fresh_local(&mut self, ty: MirTy) -> LocalId {
        let id = LocalId(u32::try_from(self.locals.len()).expect("LocalId overflow"));
        self.locals.push(LocalDecl { id, name: None, ty });
        id
    }

    /// Lower a block at function-body level. The trailing expression
    /// becomes the implicit return value. Returns `true` if the block
    /// ended in a terminating statement.
    fn lower_block(&mut self, b: &HirBlock, _is_fn_body: bool) -> bool {
        for s in &b.stmts {
            if self.lower_stmt(s) {
                return true;
            }
        }
        if let Some(t) = &b.trailing {
            self.push_loc(t.span);
            let op = self.lower_expr_to_operand(t);
            // A unit trailing expression returns no value.
            let term = if matches!(self.ty(&t.ty), MirTy::Unit) {
                Terminator::Return(None)
            } else {
                Terminator::Return(Some(op))
            };
            self.terminate(term);
            return true;
        }
        false
    }

    /// Lower a block whose value is needed. Returns `(operand, diverged)`
    /// where `diverged` is true when the block already terminated the
    /// current basic block (e.g. via `return`, `break`, `continue`). The
    /// caller must not append another terminator in that case.
    fn lower_block_value(&mut self, b: &HirBlock) -> (Operand, bool) {
        let outer = self.scope;
        self.scopes += 1;
        self.scope = crate::Scope {
            id: self.scopes,
            parent: outer.id,
        };
        let r = self.lower_scope(b);
        self.scope = outer;
        r
    }

    fn lower_scope(&mut self, b: &HirBlock) -> (Operand, bool) {
        let first_temp = self.locals.len();
        for s in &b.stmts {
            if self.lower_stmt(s) {
                return (Operand::Const(ConstVal::Unit), true);
            }
        }
        let mut value = match &b.trailing {
            Some(t) => {
                self.push_loc(t.span);
                self.lower_expr_to_operand(t)
            }
            None => Operand::Const(ConstVal::Unit),
        };
        // The block's `let`s and the temporaries made while lowering it go
        // out of scope (only the block value escapes, copied below): tell
        // codegen so their shadow-stack roots stop keeping objects alive.
        let lets = b.stmts.iter().filter_map(|s| match s {
            HirStmt::Let { local, .. } => Some(*local),
            _ => None,
        });
        let temps = (first_temp..self.locals.len()).map(|i| self.locals[i].id);
        let mut dead: Vec<LocalId> = lets
            .chain(temps)
            .filter(|l| self.target.has_gc_ref(&self.locals[l.0 as usize].ty))
            .collect();
        dead.dedup();
        let escaping = match &value {
            Operand::Copy(l) => dead.contains(l).then_some(*l),
            Operand::Const(_) => None,
        };
        if let Some(l) = escaping {
            let tmp = self.fresh_local(self.locals[l.0 as usize].ty.clone());
            self.push_stmt(Statement::Assign {
                target: tmp,
                rvalue: RValue::Use(value),
            });
            value = Operand::Copy(tmp);
        }
        for l in dead {
            self.push_stmt(Statement::StorageDead(l));
        }
        (value, false)
    }

    /// Mark the source position of what follows (debug line info);
    /// a repeat of the same line is dropped.
    fn push_loc(&mut self, span: nabic_diagnostic::Span) {
        if span.line == 0 || !self.block_is_live() {
            return;
        }
        let scope = self.scope;
        let stmts = &mut self.current_block().stmts;
        // Another scope keeps its own `Loc` (it names that scope).
        if let Some(Statement::Loc(prev, s)) = stmts.last_mut() {
            if *s == scope {
                *prev = span;
                return;
            }
        }
        stmts.push(Statement::Loc(span, scope));
    }
}

/// The value of a condition made of `bool` constants, `!` and `&&` /
/// `||` (an `if` with constant arms).
fn const_bool(e: &HirExpr) -> Option<bool> {
    match &e.kind {
        HirExprKind::Const(ConstVal::Bool(b)) => Some(*b),
        HirExprKind::PrimUnaryOp {
            op: PrimOp::LogicalNot,
            operand,
        } => const_bool(operand).map(|b| !b),
        HirExprKind::Block(b) if b.stmts.is_empty() => const_bool(b.trailing.as_ref()?),
        HirExprKind::If {
            cond,
            then_block,
            else_branch: Some(else_e),
        } if then_block.stmts.is_empty() => {
            if const_bool(cond)? {
                const_bool(then_block.trailing.as_ref()?)
            } else {
                const_bool(else_e)
            }
        }
        _ => None,
    }
}

// A zero ConstVal of the given primitive shape; used as a dummy
// payload for unit variants of payload-bearing enums.
const fn zero_const(ty: &MirTy) -> ConstVal {
    match ty {
        MirTy::Prim(p) if p.is_int() => ConstVal::Int { value: 0, ty: *p },
        MirTy::Prim(IrPrimTy::Bool) => ConstVal::Bool(false),
        MirTy::Prim(p) if p.is_float() => ConstVal::Float { bits: 0, ty: *p },
        _ => ConstVal::Unit,
    }
}

/// The type of the tag a variant pattern compares: `i32`, or the
/// `#[repr]` integer a fieldless enum is.
const fn variant_tag_ty(pat: &HirPattern, scrut_ty: &MirTy) -> IrPrimTy {
    match (pat, scrut_ty) {
        (HirPattern::EnumVariant { .. }, MirTy::Prim(p)) => *p,
        _ => IrPrimTy::I32,
    }
}
