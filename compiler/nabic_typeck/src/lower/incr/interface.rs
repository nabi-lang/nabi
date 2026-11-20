//! Fingerprints of the interface a lowering reads.

#[allow(clippy::wildcard_imports)]
use crate::lower::incr::*;

/// A fingerprint of interface values: positions left out, ids by their
/// build-independent names.
pub(crate) struct Fp(Enc);

impl Fp {
    pub(crate) fn new() -> Self {
        let mut e = Enc::new(false);
        e.ids = Some(Vec::new());
        e.no_ids = true;
        Self(e)
    }

    pub(crate) fn add<T: Codec>(&mut self, v: &T) -> &mut Self {
        v.enc(&mut self.0);
        self
    }

    pub(crate) fn opt<T>(&mut self, v: Option<T>, f: impl FnOnce(&mut Self, T)) -> &mut Self {
        match v {
            Some(v) => {
                self.0.u(1);
                f(self, v);
            }
            None => self.0.u(0),
        }
        self
    }

    pub(crate) fn debug(&mut self, v: &impl std::fmt::Debug) -> &mut Self {
        self.0.bytes(format!("{v:?}").as_bytes());
        self
    }

    pub(crate) fn finish(mut self, names: &Names) -> Option<u64> {
        for (k, id) in self.0.ids.take().unwrap_or_default() {
            match k {
                _ if id == NO_ID => self.0.bytes(b"!"),
                IdKind::InferVar => self.0.u(u64::from(id)),
                k => self.0.bytes(names.name_of(kind_tag(k), id)?.as_bytes()),
            }
        }
        Some(nabic_core::hash::fast(&self.0.buf))
    }
}

/// A declaration as lowering reads it.
pub(crate) fn fp_decl(f: &mut Fp, d: &MethodOrFn<'_>) {
    match d {
        MethodOrFn::Fn(def) => {
            f.add(&0u8).add(*def);
        }
        MethodOrFn::Method {
            fn_def,
            self_ty,
            module,
            key,
            subst,
            default,
        } => {
            f.add(&1u8)
                .add(&**fn_def)
                .add(self_ty)
                .add(module)
                .add(&stable_name(key))
                .add(subst)
                .add(&u8::from(*default));
        }
    }
}

pub(crate) fn fp_impl(f: &mut Fp, i: &crate::lower::Impl) {
    f.add(&i.trait_key)
        .add(&i.trait_args)
        .add(&i.assoc)
        .add(&i.generics)
        .add(&i.where_clause)
        .add(&i.self_ty)
        .add(&i.module)
        .add(&i.methods)
        .add(&i.consts)
        .add(&i.concrete);
}

/// Keys by last segment, the ones of `leaf` in order.
pub(crate) fn leaf_keys(l: &crate::lower::Leaves, leaf: &str) -> Vec<String> {
    let mut v = l.get(leaf).cloned().unwrap_or_default();
    v.sort();
    v
}

impl Incr<'_> {
    /// The fingerprint of impl `i`, taken once per build.
    pub(crate) fn impl_fp(&self, env: &Env<'_>, i: usize) -> Option<u64> {
        if let Some(h) = self.impl_fps.borrow().get(&i) {
            return *h;
        }
        let mut f = Fp::new();
        fp_impl(&mut f, &env.impls.all[i]);
        let mut names = self.names.borrow_mut();
        names.update(env);
        let h = f.finish(&names);
        drop(names);
        self.impl_fps.borrow_mut().insert(i, h);
        h
    }

    /// The fingerprints of impls `at`, in source order.
    pub(crate) fn impls_fp(&self, env: &Env<'_>, f: &mut Fp, at: &[usize]) -> Option<()> {
        for &i in at {
            f.add(&self.impl_fp(env, i)?);
        }
        Some(())
    }

    /// The impls by method (generic ones) or by associated const.
    pub(crate) fn impls_by(&self, env: &Env<'_>, consts: bool, name: &str) -> &[usize] {
        let by = self.impls_by.get_or_init(|| {
            let mut by: [HashMap<String, Vec<usize>>; 2] = Default::default();
            for (i, im) in env.impls.all.iter().enumerate() {
                if im.is_generic() {
                    for m in im.methods.keys() {
                        by[0].entry(m.clone()).or_default().push(i);
                    }
                }
                for (c, ..) in &im.consts {
                    let at = by[1].entry(c.clone()).or_default();
                    if at.last() != Some(&i) {
                        at.push(i);
                    }
                }
            }
            by
        });
        by[usize::from(consts)].get(name).map_or(&[], Vec::as_slice)
    }

    /// The fingerprint of what `r` reads now (`None`: not readable).
    pub(crate) fn answer(&self, env: &Env<'_>, r: &Read) -> Option<u64> {
        let mut e = Enc::new(true);
        r.enc(&mut e);
        self.answer_at(env, e.buf, r)
    }

    /// `answer` of `r`, encoded as `key`.
    pub(crate) fn answer_at(&self, env: &Env<'_>, key: Vec<u8>, r: &Read) -> Option<u64> {
        if let Some(a) = self.answers.borrow().get(&key) {
            return *a;
        }
        let a = self.answer_now(env, r);
        self.answers.borrow_mut().insert(key, a);
        a
    }

    #[allow(clippy::too_many_lines)] // one arm per table
    pub(crate) fn answer_now(&self, env: &Env<'_>, r: &Read) -> Option<u64> {
        let mut f = Fp::new();
        let names = &env.names;
        match r {
            Read::Sig(k) => {
                f.add(&env.fn_table.get(self.raw_key(env, k)).cloned());
            }
            Read::Generic(k) => {
                f.opt(env.generic_fns.get(k), |f, d| {
                    f.add(*d);
                });
            }
            Read::FnLeaf(k) => {
                let l = env.fn_leaves.get_or_init(|| {
                    crate::lower::leaves(env.fn_table.keys().chain(env.generic_fns.keys()))
                });
                f.add(&leaf_keys(l, k));
            }
            Read::ExternFn(k) => {
                f.add(&env.extern_fns.get(k).cloned());
            }
            Read::Methods(ty, m) => {
                let ids = crate::lower::method_target_for(ty)
                    .and_then(|t| env.method_table.get(&(t, m.clone())));
                f.add(&ids.cloned());
            }
            Read::AllMethods => {
                // Order-free: the sum of the entries' fingerprints.
                let mut sum = 0u64;
                let mut names_now = self.names.borrow_mut();
                names_now.update(env);
                for ((t, m), ids) in &env.method_table {
                    let mut g = Fp::new();
                    match t {
                        crate::lower::MethodTarget::Struct(id) => g.add(&0u8).add(id),
                        crate::lower::MethodTarget::Enum(id) => g.add(&1u8).add(id),
                        crate::lower::MethodTarget::Mangled(s) => g.add(&2u8).add(&stable_name(s)),
                    };
                    g.add(m).add(ids);
                    sum = sum.wrapping_add(g.finish(&names_now)?);
                }
                f.add(&sum);
            }
            Read::AllGenericMethods => {
                f.add(&env.generic_methods);
            }
            Read::MethodInfo(id) => {
                f.opt(env.method_info.get(id), |f, i| {
                    f.add(i);
                });
            }
            Read::GenericMethods(b, m) => {
                f.opt(env.generic_methods.get(&(b.clone(), m.clone())), |f, v| {
                    f.add(&(v.len() as u64));
                    for g in v {
                        f.add(g);
                    }
                });
            }
            Read::KeyOf(id) => {
                f.add(&env.key_of.get(id).map(|k| stable_name(k)));
            }
            Read::InferRet(id) => {
                f.add(&env.infer_ret.contains(id));
            }
            Read::MayDiverge(at) => {
                f.add(&env.may_diverge.contains(at));
            }
            Read::Intrinsic(id) => {
                f.debug(&env.intrinsics.get(id));
            }
            Read::GenericIntrinsic(k) => {
                f.debug(&env.generic_intrinsics.get(k));
            }
            Read::FnIn(set, id) => {
                f.add(&crate::lower::FnSet::of(*set)?.contains(env, *id));
            }
            Read::ItemVis(k) => {
                f.add(&env.item_visibility.get(k).copied());
            }
            Read::FieldVis(k) => {
                f.add(&env.field_vis.get(k).cloned());
            }
            Read::Const(k) => {
                f.add(&env.consts.get(k).cloned());
            }
            Read::ConstLeaf(k) => {
                let mut v = env.value_leaf(false, k);
                v.sort();
                f.add(&v);
            }
            Read::Static(k) => {
                f.add(&env.statics.get(k).cloned());
            }
            Read::StaticLeaf(k) => {
                let mut v = env.value_leaf(true, k);
                v.sort();
                f.add(&v);
            }
            Read::MutStatic(k) => {
                f.add(&env.mut_statics.contains(k));
            }
            Read::ExternStatic(k) => {
                f.add(&env.extern_statics.contains(k));
            }
            Read::TraitInfo(k) => {
                f.opt(env.trait_info.get(k), |f, t| {
                    f.add(t);
                });
            }
            Read::NegAuto(b, k) => {
                f.add(&env.neg_auto.contains(&(b.clone(), k.clone())));
            }
            Read::NonExhaustive(k) => {
                f.add(&env.non_exhaustive.contains(k));
            }
            Read::Sealed(k) => {
                f.add(&env.sealed.contains(k));
            }
            Read::FnMark(id) => {
                f.add(&env.fn_marks.get(id).cloned());
            }
            Read::Mark(k) => {
                f.add(&env.marks.get(k).cloned());
            }
            Read::BoundMethod(t, m) => {
                f.opt(env.bound_methods.get(&(t.clone(), m.clone())), |f, g| {
                    f.add(g);
                });
            }
            Read::BoundTrait(k) => {
                f.add(&env.bound_traits.get(k).cloned());
            }
            Read::Decl(id) => {
                f.opt(env.decls.get(id.0 as usize), fp_decl);
            }
            Read::CheckFailed(at) => {
                f.add(&env.sink.failed.borrow().contains(at));
            }
            Read::Uses(m) => {
                f.debug(&env.names.uses.get(m));
            }
            Read::GenericImpls => {
                for (i, _) in env.impls.iter().enumerate().filter(|(_, i)| i.is_generic()) {
                    f.add(&self.impl_fp(env, i)?);
                }
            }
            Read::GenericImplsOf(k) => {
                for &i in env.impls.generic_of.get(k).into_iter().flatten() {
                    f.add(&self.impl_fp(env, i)?);
                }
            }
            Read::GenericImplsWith(m) => {
                self.impls_fp(env, &mut f, self.impls_by(env, false, m))?;
            }
            Read::ImplConsts(n) => {
                self.impls_fp(env, &mut f, self.impls_by(env, true, n))?;
            }
            Read::ImplsOn(ty) => {
                let refs: Vec<&crate::TraitRef> = env.impls.refs_on(ty).collect();
                f.add(&(refs.len() as u64));
                for r in refs {
                    f.add(r);
                }
            }
            Read::ImplsOnOf(ty, k) => {
                let refs: Vec<&crate::TraitRef> = env.impls.refs_of(ty, k).collect();
                f.add(&(refs.len() as u64));
                for r in refs {
                    f.add(r);
                }
            }
            Read::ImplsOf(k) => {
                let tys: Vec<&HirTy> = env.impls.tys_of(k).collect();
                f.add(&(tys.len() as u64));
                for t in tys {
                    f.add(t);
                }
            }
            Read::Canon(segs, module) => {
                let segs: Vec<&str> = segs.iter().map(String::as_str).collect();
                let errors = names.diags.borrow().len();
                let full = names.canon(&segs, module, &[], Span::DUMMY);
                let failed = names.diags.borrow().len() != errors;
                names.diags.borrow_mut().truncate(errors);
                f.add(&full).add(&failed);
            }
            Read::Type(k) => {
                f.add(&names.names.contains(k));
            }
            Read::TypeLeaf(k) => {
                let l = names.leaves.get_or_init(|| {
                    crate::lower::leaves(names.names.iter().chain(names.reexports.keys()))
                });
                f.add(&leaf_keys(l, k));
            }
            Read::TypeVis(k) => {
                f.add(&names.visibility.get(k).copied());
            }
            Read::ModVis(k) => {
                f.add(&names.mod_vis.get(k).copied());
            }
            Read::Visible(k) => {
                f.add(&names.visible.get(k).cloned());
            }
            Read::Reexport(k) => {
                f.add(&names.reexports.get(k).cloned());
            }
            Read::Trait(k) => {
                f.add(&names.traits.contains(k));
            }
            Read::Traits => {
                f.add(&names.traits);
            }
            Read::Variant(k) => {
                f.add(&names.variants.contains(k));
            }
            Read::VariantLeaf(k) => {
                let l = names
                    .variant_leaves
                    .get_or_init(|| crate::lower::leaves(&names.variants));
                f.add(&leaf_keys(l, k));
            }
            Read::NameConst(k) => {
                f.add(&names.consts.get(k).cloned());
            }
            Read::NameConstLeaf(k) => {
                let l = names
                    .const_leaves
                    .get_or_init(|| crate::lower::leaves(names.consts.keys()));
                f.add(&leaf_keys(l, k));
            }
            Read::ValueReexport(k) => {
                f.add(&names.value_reexports.get(k).cloned());
            }
            Read::GenericAlias(k) => {
                f.add(&names.generic_aliases.get(k).cloned());
            }
            Read::AnyGenericAlias => {
                f.add(&!names.generic_aliases.is_empty());
            }
            Read::DefaultedLeaf(k) => {
                f.add(&env.defs.defaulted_leaves.contains(k));
            }
            Read::PlainAlias(k) => {
                f.add(&names.plain_aliases.get(k).cloned());
            }
            Read::StructDef(id) => {
                let structs = env.defs.structs.borrow();
                f.opt(structs.get(id.0 as usize), |f, d| {
                    f.add(&crate::type_name(false, id.0))
                        .add(&d.repr)
                        .add(&d.def_id)
                        .add(&d.fields)
                        .add(&d.generic_args)
                        .add(&d.union_kind)
                        .add(&d.niche);
                });
            }
            Read::EnumDef(id) => {
                let enums = env.defs.enums.borrow();
                f.opt(enums.get(id.0 as usize), |f, d| {
                    f.add(&crate::type_name(true, id.0))
                        .add(&d.repr)
                        .add(&d.def_id)
                        .add(&d.variants)
                        .add(&d.generic_args);
                });
            }
            Read::GenericStruct(k) => {
                f.opt(env.defs.generic_structs.get(k), |f, d| {
                    f.add(*d);
                });
            }
            Read::GenericEnum(k) => {
                f.opt(env.defs.generic_enums.get(k), |f, d| {
                    f.add(*d);
                });
            }
            Read::GenericUnion(k) => {
                f.opt(env.defs.generic_unions.get(k), |f, d| {
                    f.add(*d);
                });
            }
        }
        let mut names = self.names.borrow_mut();
        names.update(env);
        f.finish(&names)
    }
}

/// What every lowering sees alike: the build's target and flags, the
/// operators, the lang slots, the literal handlers.
pub(crate) fn global_key(env: &Env<'_>, input: &crate::HirInput) -> u64 {
    let mut e = Enc::new(false);
    input.fingerprint.enc(&mut e);
    e.bytes(
        format!(
            "{:?} {} {:?} {} {} {:?}",
            (&input.packages, &input.lib, &input.prebuilt),
            input.check_library,
            input.root_main,
            input.pointer_width,
            input.stdcall_system,
            input.no_implicit_nabi,
        )
        .as_bytes(),
    );
    let mut ops: Vec<String> = env.prec.entries().map(|o| format!("{o:?}")).collect();
    ops.sort();
    let mut lang: Vec<String> = env.names.lang.entries().map(|l| format!("{l:?}")).collect();
    lang.sort();
    let shapes = env
        .literal_shapes
        .iter()
        .map(|h| format!("{:?} {} {}", h.shape, h.name, h.priority));
    for s in ops.into_iter().chain(lang).chain(shapes) {
        e.bytes(s.as_bytes());
    }
    nabic_core::hash::fast(&e.buf)
}

/// Declaration `i` is plain (see the module doc).
pub(crate) fn is_plain(env: &Env<'_>, i: usize) -> bool {
    let (def, borrowed) = match &env.decls[i] {
        MethodOrFn::Fn(f) => (*f, true),
        MethodOrFn::Method { fn_def, subst, .. } => (
            &**fn_def,
            matches!(fn_def, std::borrow::Cow::Borrowed(_)) && subst.is_empty(),
        ),
    };
    borrowed
        && def.body.is_some()
        && def.generics.params.is_empty()
        && !def
            .attrs
            .iter()
            .any(|a| a.path == [nabic_ast::CONST_FN_ATTR])
}

/// The program's interface: everything lowering a body can see of it,
/// but the plain bodies (`bodies`) and source positions.
pub(crate) fn interface_key(
    env: &Env<'_>,
    bodies: &HashSet<(&str, Span)>,
    programs: &[&Program],
    raw_uses: &[(String, nabic_core::Visibility, nabic_ast::UseTree)],
    input: &crate::HirInput,
) -> u64 {
    let mut e = Enc::new(false);
    input.fingerprint.enc(&mut e);
    e.bytes(format!("{:?}", (&input.packages, &input.lib, &input.prebuilt)).as_bytes());
    e.bytes(
        format!(
            "{} {:?} {} {}",
            input.check_library, input.root_main, input.pointer_width, input.stdcall_system
        )
        .as_bytes(),
    );
    input.item_modules.enc(&mut e);
    e.u(raw_uses.len() as u64);
    for u in raw_uses {
        u.enc(&mut e);
    }
    let mut infer: Vec<u32> = env.infer_ret.iter().map(|id| id.0).collect();
    infer.sort_unstable();
    e.u(infer.len() as u64);
    for id in infer {
        e.u(u64::from(id));
    }
    // Files by id (spans name them by id).
    for (p, _) in env.sources.files() {
        e.bytes(p.as_bytes());
    }
    let fn_def = |f: &FnDef, e: &mut Enc| {
        // A plain declaration's body and parameters and return type are
        // read through `Lookup::Sig` (its callers) and its unit's key
        // (itself).
        if bodies.contains(&(f.name.as_str(), f.span)) {
            e.buf.push(1);
            f.vis.enc(e);
            f.attrs.enc(e);
            f.name.enc(e);
            f.generics.enc(e);
            // The parameters but a method's receiver are the signature's.
            f.params
                .first()
                .filter(|p| p.name == "self")
                .map(|p| p.by_ref)
                .enc(e);
            f.ret_ty.is_some().enc(e);
            (f.is_async, f.is_generator, f.is_unsafe).enc(e);
            match &f.throws {
                nabic_ast::ThrowsKind::None => e.u(0),
                nabic_ast::ThrowsKind::Untyped => e.u(1),
                nabic_ast::ThrowsKind::Typed(t) => e.u(2 + t.len() as u64),
            }
            f.where_clause.enc(e);
        } else {
            e.buf.push(0);
            f.enc(e);
        }
    };
    // A kept library stage's items (the first program's) are those of its
    // key.
    let programs = match input.library {
        Some(k) => {
            e.u(k);
            &programs[1..]
        }
        None => programs,
    };
    let items = programs.iter().flat_map(|p| &p.items);
    e.u(items.clone().count() as u64);
    for item in items {
        match item {
            Item::Fn(f) => {
                e.buf.push(0);
                fn_def(f, &mut e);
            }
            Item::Impl(imp) => {
                e.buf.push(1);
                imp.vis.enc(&mut e);
                imp.attrs.enc(&mut e);
                imp.generics.enc(&mut e);
                imp.trait_path.enc(&mut e);
                (imp.is_unsafe, imp.negative).enc(&mut e);
                imp.self_ty.enc(&mut e);
                imp.where_clause.enc(&mut e);
                imp.module.enc(&mut e);
                e.u(imp.items.len() as u64);
                for i in &imp.items {
                    match i {
                        ImplItem::Method(f) => fn_def(f, &mut e),
                        other => other.enc(&mut e),
                    }
                }
            }
            other => {
                e.buf.push(2);
                other.enc(&mut e);
            }
        }
    }
    nabic_core::hash::fast(&e.buf)
}
