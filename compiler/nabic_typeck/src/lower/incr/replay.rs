//! Replaying a unit: renumbering and checking its reads.

#[allow(clippy::wildcard_imports)]
use crate::lower::incr::*;

impl Incr<'_> {
    /// Replay `u` when it holds for this build (see the module doc).
    pub(crate) fn replay(&self, env: &Env<'_>, decl: usize, u: &Unit) -> Option<HirFunction> {
        let sink = &env.sink;
        let head = &u.head;
        for (path, at, hash) in &head.deps {
            let id = env.sources.id_of(path)?;
            if self.prefix(env, id, *at, true) != Some((*at, *hash)) {
                return None;
            }
        }
        let ids = Ids::new(head, counts(env));
        {
            let mut names = self.names.borrow_mut();
            names.update(env);
            let mut learned = ids.learned.borrow_mut();
            for (k, old, name) in &head.named {
                if let Some(now) = names.id_of(*k, name) {
                    learned.insert((*k, *old), now);
                }
            }
        }
        if !head.lookups.iter().all(|l| answers(env, &ids, l)) {
            return None;
        }
        for (r, h) in &head.reads {
            // A read naming no id is asked as it is.
            let mut e = Enc::new(true);
            e.ids = Some(Vec::new());
            r.enc(&mut e);
            let renumbered;
            let r = if e.ids.take().is_some_and(|v| v.is_empty()) {
                r
            } else {
                renumbered = ids.read(r)?;
                e = Enc::new(true);
                renumbered.enc(&mut e);
                &renumbered
            };
            if self.answer_at(env, e.buf, r) != Some(*h) {
                return None;
            }
        }
        let map = |kind, id| ids.map(kind, id);
        let mut d = Dec::new(&u.body);
        d.files = Some(&u.files);
        d.ids = Some(&map);
        let b = Body::dec(&mut d)?;
        // A new tag must still be free (`any_tag` probes past a taken one).
        {
            let tags = sink.any_tags.borrow();
            if b.any_tags
                .iter()
                .any(|(_, t)| tags.iter().any(|(_, old)| old == t))
            {
                return None;
            }
        }
        let now = ids.now;
        // Names made of this build's ids follow them.
        let renamed = |mut f: HirFunction| {
            let old = f.id.0 - now.next_id + head.start.next_id;
            for p in ["__closure_", "__coro_", "__thunk_"] {
                if f.name == format!("{p}{old}") {
                    f.name = format!("{p}{}", f.id.0);
                }
            }
            f.name = map_type_ids(&f.name, |enum_, id| {
                let k = if enum_ { IdKind::Enum } else { IdKind::Struct };
                let tag = if enum_ { "Enum" } else { "Struct" };
                Some(format!("{tag}{}", ids.map(k, id)?))
            })?;
            Some(f)
        };
        let extras = b
            .extras
            .into_iter()
            .map(renamed)
            .collect::<Option<Vec<_>>>()?;
        sink.extras.borrow_mut().extend(extras);
        let (struct_bases, enum_bases) = head.bases.split_at(b.structs.len());
        {
            let mut table = env.defs.struct_table.borrow_mut();
            let mut structs = env.defs.structs.borrow_mut();
            for (mut s, base) in b.structs.into_iter().zip(struct_bases) {
                s.name = mangle_inst(base, &s.generic_args);
                crate::name_type(false, s.id.0, base, &s.generic_args);
                table.insert(s.name.clone(), s.id);
                structs.push(s);
            }
        }
        {
            let mut table = env.defs.enum_table.borrow_mut();
            let mut enums = env.defs.enums.borrow_mut();
            for (mut e, base) in b.enums.into_iter().zip(enum_bases) {
                e.name = mangle_inst(base, &e.generic_args);
                crate::name_type(true, e.id.0, base, &e.generic_args);
                table.insert(e.name.clone(), e.id);
                enums.push(e);
            }
        }
        for e in b.entries {
            set(&mut sink.ref_out.borrow_mut(), e.id, e.ref_out);
            set(&mut sink.mono_sigs.borrow_mut(), e.id, e.mono_sig);
            set(&mut sink.fn_origin.borrow_mut(), e.id, e.fn_origin);
            set(&mut sink.inferred_ret.borrow_mut(), e.id, e.inferred_ret);
            set(
                &mut sink.inferred_fns.borrow_mut(),
                e.id,
                e.inferred_fn.map(|(at, mut f)| {
                    if let Some(n) = decl_name(env, f.id) {
                        f.name = n;
                    }
                    (now.next_id + at, f)
                }),
            );
            if e.track_caller {
                sink.track_caller.borrow_mut().insert(e.id);
            } else {
                sink.track_caller.borrow_mut().remove(&e.id);
            }
        }
        for (k, v) in b.keyed {
            match k {
                Key::Mono(name, args) => {
                    set(
                        &mut sink.mono_cache.borrow_mut(),
                        Key::mono(&name, &args),
                        v,
                    );
                }
                Key::Thunk(kind, target, slot, callee) => set(
                    &mut sink.thunks.borrow_mut(),
                    crate::lower::throws::thunk_key(kind, target, &slot, &callee),
                    v,
                ),
                Key::Unknown(_) => {}
            }
        }
        {
            let mut unknown = env.names.unknown_spans.borrow_mut();
            for (path, at) in b.unknown_spans {
                unknown.entry(path).or_insert(at);
            }
        }
        sink.any_tags.borrow_mut().extend(b.any_tags);
        sink.any_dispatch.borrow_mut().extend(b.any_dispatch);
        sink.infer_vars.borrow_mut().extend(b.infer_vars);
        {
            let mut iw = sink.instance_warns.borrow_mut();
            for w in b.warns {
                match w {
                    WarnEvent::Instance(t, id) => iw.instance(t, id),
                    WarnEvent::Warn(t, d, id) => iw.warn(t, d, id),
                }
            }
        }
        sink.diagnostics.borrow_mut().extend(b.diagnostics);
        env.names.diags.borrow_mut().extend(b.name_diags);
        env.names.alias_uses.borrow_mut().extend(b.alias_uses);
        sink.next_id
            .set(head.end.next_id - head.start.next_id + now.next_id);
        let mut func = b.func;
        func.name = decl_name(env, FunctionId(u32::try_from(decl).ok()?))?;
        Some(func)
    }

    /// The `fn_table` key whose `stable_name` is `k` (`k` when none).
    pub(crate) fn raw_key<'k>(&'k self, env: &'k Env<'_>, k: &'k str) -> &'k str {
        let keys = self.raw_keys.get_or_init(|| {
            let mut m: HashMap<String, Option<String>> = HashMap::default();
            for raw in env.fn_table.keys() {
                let s = stable_name(raw);
                if s != *raw {
                    m.entry(s)
                        .and_modify(|v| *v = None)
                        .or_insert_with(|| Some(raw.clone()));
                }
            }
            m
        });
        match keys.get(k) {
            Some(Some(raw)) => raw,
            Some(None) => "",
            None => k,
        }
    }
}

/// A replay's renumbering: what the unit made moves to where this
/// build's counters stand; what it found made before is what the reads
/// answer now (`learn`); the program's own declarations and types keep
/// their ids.
pub(crate) struct Ids {
    start: Counts,
    end: Counts,
    now: Counts,
    learned: std::cell::RefCell<HashMap<(u8, u32), u32>>,
}

impl Ids {
    pub(crate) fn new(head: &Head, now: Counts) -> Self {
        Self {
            start: head.start,
            end: head.end,
            now,
            learned: std::cell::RefCell::new(HashMap::default()),
        }
    }

    /// The id `id` of `kind` of the unit is in this build.
    pub(crate) fn map(&self, kind: IdKind, id: u32) -> Option<u32> {
        let (from, to, now) = match kind {
            IdKind::Function => (self.start.next_id, self.end.next_id, self.now.next_id),
            IdKind::Struct => (self.start.structs, self.end.structs, self.now.structs),
            IdKind::Enum => (self.start.enums, self.end.enums, self.now.enums),
            IdKind::InferVar => (
                self.start.infer_vars,
                self.end.infer_vars,
                self.now.infer_vars,
            ),
        };
        if id == NO_ID {
            Some(id)
        } else if (from..to).contains(&id) {
            Some(id - from + now)
        } else {
            self.learned.borrow().get(&(kind_tag(kind), id)).copied()
        }
    }

    /// Whether `old` (a read's answer then) is `new` (its answer now):
    /// an id made before the unit and not known yet is learned.
    pub(crate) fn same(&self, kind: IdKind, old: Option<u32>, new: Option<u32>) -> bool {
        match (old, new) {
            (None, None) => true,
            (Some(o), Some(n)) => {
                if let Some(m) = self.map(kind, o) {
                    return m == n;
                }
                self.learned.borrow_mut().insert((kind_tag(kind), o), n);
                true
            }
            _ => false,
        }
    }

    /// `v` with its ids as this build numbers them.
    pub(crate) fn renumber<T: Codec>(&self, v: &T) -> Option<T> {
        let mut e = Enc::new(true);
        v.enc(&mut e);
        let map = |kind, id| self.map(kind, id);
        let mut d = Dec::new(&e.buf);
        d.ids = Some(&map);
        T::dec(&mut d)
    }

    pub(crate) fn ty(&self, ty: &HirTy) -> Option<HirTy> {
        self.renumber(ty)
    }

    pub(crate) fn read(&self, r: &Read) -> Option<Read> {
        self.renumber(r)
    }

    pub(crate) fn tys(&self, tys: &[HirTy]) -> Option<Vec<HirTy>> {
        tys.iter().map(|t| self.ty(t)).collect()
    }
}

/// Whether the shared state answers `l` (read under `ids`) as it did
/// when it was logged.
pub(crate) fn answers(env: &Env<'_>, ids: &Ids, l: &Lookup) -> bool {
    let sink = &env.sink;
    let raw = |id: Option<FunctionId>| id.map(|i| i.0);
    let fun = |old: &Option<FunctionId>, new: Option<FunctionId>| {
        ids.same(IdKind::Function, raw(*old), raw(new))
    };
    let list = |kind, old: Vec<u32>, new: Vec<u32>| {
        old.len() == new.len()
            && old
                .into_iter()
                .zip(new)
                .all(|(o, n)| ids.same(kind, Some(o), Some(n)))
    };
    match l {
        Lookup::Mono(name, args, r) => {
            let Some(args) = ids.tys(args) else {
                return false;
            };
            let now = sink
                .mono_cache
                .borrow()
                .get(&Key::mono(name, &args))
                .copied();
            fun(r, now)
        }
        Lookup::Struct(k, r) => {
            let now = env.defs.struct_table.borrow().get(k).copied();
            ids.same(IdKind::Struct, r.map(|i| i.0), now.map(|i| i.0))
        }
        Lookup::Enum(k, r) => {
            let now = env.defs.enum_table.borrow().get(k).copied();
            ids.same(IdKind::Enum, r.map(|i| i.0), now.map(|i| i.0))
        }
        Lookup::StructInst(base, args, r) => {
            let Some(args) = ids.tys(args) else {
                return false;
            };
            let now = env
                .defs
                .struct_table
                .borrow()
                .get(&mangle_inst(base, &args))
                .copied();
            ids.same(IdKind::Struct, r.map(|i| i.0), now.map(|i| i.0))
        }
        Lookup::EnumInst(base, args, r) => {
            let Some(args) = ids.tys(args) else {
                return false;
            };
            let now = env
                .defs
                .enum_table
                .borrow()
                .get(&mangle_inst(base, &args))
                .copied();
            ids.same(IdKind::Enum, r.map(|i| i.0), now.map(|i| i.0))
        }
        Lookup::StructInsts(base, r) => {
            let now = crate::lower::LowerCtx::instantiations(&env.defs.struct_table.borrow(), base);
            list(
                IdKind::Struct,
                r.iter().map(|i| i.0).collect(),
                now.iter().map(|i| i.0).collect(),
            )
        }
        Lookup::EnumInsts(base, r) => {
            let now = crate::lower::LowerCtx::instantiations(&env.defs.enum_table.borrow(), base);
            list(
                IdKind::Enum,
                r.iter().map(|i| i.0).collect(),
                now.iter().map(|i| i.0).collect(),
            )
        }
        Lookup::Thunk(Key::Thunk(kind, target, slot, callee), r) => {
            let target = match target {
                Some(t) => match ids.map(IdKind::Function, t.0) {
                    Some(t) => Some(FunctionId(t)),
                    None => return false,
                },
                None => None,
            };
            let (Some(slot), Some(callee)) = (ids.ty(slot), ids.ty(callee)) else {
                return false;
            };
            let key = crate::lower::throws::thunk_key(kind, target, &slot, &callee);
            fun(r, sink.thunks.borrow().get(&key).copied())
        }
        Lookup::Thunk(..) => false,
        Lookup::AnyTag(ty, r) => {
            let Some(ty) = ids.ty(ty) else {
                return false;
            };
            sink.any_tags
                .borrow()
                .iter()
                .find(|(t, _)| *t == ty)
                .map(|(_, tag)| *tag)
                == *r
        }
        Lookup::AnyDispatch(recv, trait_key, method, shape, r) => {
            let Some(recv) = ids.ty(recv) else {
                return false;
            };
            let Some(shape) = shape.iter().map(|t| ids.ty(t)).collect::<Option<Vec<_>>>() else {
                return false;
            };
            let now = sink
                .any_dispatch
                .borrow()
                .iter()
                .find(|d| d.is(&recv, trait_key, method, &shape))
                .map(|d| d.id);
            fun(r, now)
        }
        Lookup::InferredRet(id, r) => {
            let Some(id) = ids.map(IdKind::Function, id.0) else {
                return false;
            };
            let now = sink.inferred_ret.borrow().get(&FunctionId(id)).cloned();
            match (r, now) {
                (None, None) | (Some(None), Some(None)) => true,
                (Some(Some(t)), Some(Some(n))) => ids.ty(t).is_some_and(|t| t == n),
                _ => false,
            }
        }
    }
}
