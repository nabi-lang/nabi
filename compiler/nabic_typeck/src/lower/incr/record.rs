//! Recording a lowering as a unit.

#[allow(clippy::wildcard_imports)]
use crate::lower::incr::*;

impl Incr<'_> {
    /// The unit of declaration `i` lowered as `f` from `start`, unless
    /// the lowering did more than it logged.
    pub(crate) fn record(
        &self,
        env: &Env<'_>,
        i: usize,
        f: &HirFunction,
        start: Counts,
        before: &Sizes,
        rec: Rec,
    ) -> Option<Unit> {
        let sink = &env.sink;
        let end = counts(env);
        let own = |id: &FunctionId| id.0 >= start.next_id && id.0 < end.next_id;
        let diagnostics = sink.diagnostics.borrow()[before.diagnostics..].to_vec();
        let name_diags = env.names.diags.borrow()[before.name_diags..].to_vec();
        if diagnostics
            .iter()
            .chain(&name_diags)
            .any(NabiDiagnostic::is_error)
            || sink.uninferred.get() != before.uninferred
            // A descriptor's index is this build's.
            || crate::lower::reflect::desc::uses() != before.type_descs
            || !sink.tentative_ret.borrow().is_empty()
            || !sink.recursive_ret.borrow().is_empty()
            // Calls of a duplicated item lower without their checks.
            || !env.duplicated.borrow().is_empty()
        {
            return None;
        }
        // Every table entry it may have set, as it is now.
        let mut touched = rec.touched;
        touched.push(FunctionId(u32::try_from(i).ok()?));
        touched.sort_unstable();
        touched.dedup();
        let entries: Vec<Entries> = touched
            .into_iter()
            .map(|id| {
                let inferred_fn = match sink.inferred_fns.borrow().get(&id) {
                    Some((at, f)) => Some((at.checked_sub(start.next_id)?, f.clone())),
                    None => None,
                };
                Some(Entries {
                    id,
                    ref_out: sink.ref_out.borrow().get(&id).cloned(),
                    mono_sig: sink.mono_sigs.borrow().get(&id).cloned(),
                    fn_origin: sink.fn_origin.borrow().get(&id).copied(),
                    track_caller: sink.track_caller.borrow().contains(&id),
                    inferred_ret: sink.inferred_ret.borrow().get(&id).cloned(),
                    inferred_fn,
                })
            })
            .collect::<Option<_>>()?;
        let mut keyed: Vec<(Key, Option<FunctionId>)> = Vec::new();
        let mut unknown_spans: Vec<(String, Span)> = Vec::new();
        for k in rec.keys {
            if let Key::Unknown(path) = &k {
                if !unknown_spans.iter().any(|(p, _)| p == path) {
                    let at = env.names.unknown_spans.borrow().get(path).copied()?;
                    unknown_spans.push((path.clone(), at));
                }
                continue;
            }
            if keyed.iter().any(|(old, _)| *old == k) {
                continue;
            }
            let v = match &k {
                Key::Mono(name, args) => sink
                    .mono_cache
                    .borrow()
                    .get(&Key::mono(name, args))
                    .copied(),
                Key::Thunk(kind, target, slot, callee) => sink
                    .thunks
                    .borrow()
                    .get(&crate::lower::throws::thunk_key(
                        kind, *target, slot, callee,
                    ))
                    .copied(),
                Key::Unknown(_) => None,
            };
            keyed.push((k, v));
        }
        // Tables that grew past what it touched: something it did is
        // not accounted for.
        let grew = |now: usize, then: usize, mine: usize| now > then + mine;
        let count = |pick: &dyn Fn(&Entries) -> bool| entries.iter().filter(|e| pick(e)).count();
        let monos = keyed
            .iter()
            .filter(|(k, _)| matches!(k, Key::Mono(..)))
            .count();
        if grew(sink.mono_cache.borrow().len(), before.mono_cache, monos)
            || grew(
                sink.thunks.borrow().len(),
                before.thunks,
                keyed.len() - monos,
            )
            || grew(
                sink.ref_out.borrow().len(),
                before.ref_out,
                count(&|e| e.ref_out.is_some()),
            )
            || grew(
                sink.mono_sigs.borrow().len(),
                before.mono_sigs,
                count(&|e| e.mono_sig.is_some()),
            )
            || grew(
                sink.fn_origin.borrow().len(),
                before.fn_origin,
                count(&|e| e.fn_origin.is_some()),
            )
            || grew(
                sink.track_caller.borrow().len(),
                before.track_caller,
                count(&|e| e.track_caller),
            )
            || grew(
                sink.inferred_ret.borrow().len(),
                before.inferred_ret,
                count(&|e| e.inferred_ret.is_some()),
            )
            || grew(
                sink.inferred_fns.borrow().len(),
                before.inferred_fns,
                count(&|e| e.inferred_fn.is_some()),
            )
            || grew(
                env.names.unknown_spans.borrow().len(),
                before.unknown_spans,
                unknown_spans.len(),
            )
        {
            return None;
        }
        // The functions it made are its own, numbered from `start`.
        let extras = sink.extras.borrow()[start.extras as usize..].to_vec();
        if extras.iter().any(|f| !own(&f.id))
            || sink.any_dispatch.borrow()[start.any_dispatch as usize..]
                .iter()
                .any(|d| !own(&d.id))
        {
            return None;
        }
        let structs = env.defs.structs.borrow()[start.structs as usize..].to_vec();
        let enums = env.defs.enums.borrow()[start.enums as usize..].to_vec();
        // A call of a `-> some Tr` fn checks the body again with opaque
        // parameters (`mono::check_opaque_uses`): the parameters and the
        // instances over them stay, and a replay could remake neither.
        let none = HashMap::default();
        let ctx = crate::lower::LowerCtx::new(env, &none, None, Span::DUMMY);
        let opaque = |args: &[HirTy]| {
            args.iter()
                .any(|t| crate::lower::bounds::has_param(&ctx, t))
        };
        if sink.params.borrow().len() != before.params
            || structs.iter().any(|s| opaque(&s.generic_args))
            || enums.iter().any(|e| opaque(&e.generic_args))
        {
            return None;
        }
        // Each new type is an instance `base__args` (`mangle_inst`).
        let base = |name: &str, args: &[HirTy]| {
            let tail = mangle_inst("", args);
            name.strip_suffix(&tail).map(str::to_owned)
        };
        let bases: Vec<String> = structs
            .iter()
            .map(|s| base(&s.name, &s.generic_args))
            .chain(enums.iter().map(|e| base(&e.name, &e.generic_args)))
            .collect::<Option<_>>()?;
        let body = Body {
            func: f.clone(),
            extras,
            structs,
            enums,
            entries,
            keyed,
            unknown_spans,
            any_tags: sink.any_tags.borrow()[start.any_tags as usize..].to_vec(),
            any_dispatch: sink.any_dispatch.borrow()[start.any_dispatch as usize..].to_vec(),
            infer_vars: sink.infer_vars.borrow()[start.infer_vars as usize..].to_vec(),
            warns: rec.warns,
            diagnostics,
            name_diags,
            alias_uses: env.names.alias_uses.borrow()[before.alias_uses..].to_vec(),
        };
        // The files its spans point into outside its body.
        let span = env.decls[i].def().span;
        let mut e = Enc::new(true);
        e.home = Some((span.file, span.start, span.end));
        e.ids = Some(Vec::new());
        body.enc(&mut e);
        // Reads of what it made itself follow from its other reads.
        let made = |r: &Read| {
            let mut e = Enc::new(false);
            e.ids = Some(Vec::new());
            r.enc(&mut e);
            e.ids.into_iter().flatten().any(|(k, id)| match k {
                IdKind::Function => (start.next_id..end.next_id).contains(&id),
                IdKind::Struct => (start.structs..end.structs).contains(&id),
                IdKind::Enum => (start.enums..end.enums).contains(&id),
                IdKind::InferVar => true,
            })
        };
        let reads = rec
            .reads
            .into_iter()
            .filter(|r| !made(r))
            .map(|r| match r {
                Read::Sig(k) => Read::Sig(stable_name(&k)),
                r => r,
            })
            .map(|r| Some((self.answer(env, &r)?, r)))
            .map(|a| a.map(|(h, r)| (r, h)))
            .collect::<Option<Vec<_>>>()?;
        let mut head = Head {
            start,
            end,
            lookups: rec.lookups,
            reads,
            deps: Vec::new(),
            bases,
            named: Vec::new(),
        };
        let mut h = Enc::new(true);
        h.home = e.home;
        h.ids = Some(Vec::new());
        head.enc(&mut h);
        let written = e.ids.take().into_iter().chain(h.ids.take()).flatten();
        head.named = self.named(env, start, end, written)?;
        let mut foreign: Vec<(u32, u32)> = Vec::new();
        for (f, end) in e.foreign.into_iter().chain(h.foreign) {
            match foreign.iter_mut().find(|(g, _)| *g == f) {
                Some((_, e)) => *e = (*e).max(end),
                None => foreign.push((f, end)),
            }
        }
        foreign.sort_unstable();
        head.deps = foreign
            .iter()
            .map(|&(id, end)| {
                let (at, hash) = self.prefix(env, id, end, false)?;
                Some((env.sources.path(id)?.to_owned(), at, hash))
            })
            .collect::<Option<_>>()?;
        Some(Unit {
            head,
            body: e.buf,
            files: e.files,
        })
    }

    /// The ids of what was made before `start` that `encodings` (of a
    /// unit made from `start` to `end`) name, with their names.
    pub(crate) fn named(
        &self,
        env: &Env<'_>,
        start: Counts,
        end: Counts,
        written: impl Iterator<Item = (IdKind, u32)>,
    ) -> Option<Vec<(u8, u32, String)>> {
        let mut seen = HashSet::default();
        let mut names = self.names.borrow_mut();
        names.update(env);
        let mut out = Vec::new();
        for (kind, id) in written {
            let (from, to) = match kind {
                IdKind::Function => (start.next_id, end.next_id),
                IdKind::Struct => (start.structs, end.structs),
                IdKind::Enum => (start.enums, end.enums),
                IdKind::InferVar => (start.infer_vars, end.infer_vars),
            };
            let k = (kind_tag(kind), id);
            if id != NO_ID && !(from..to).contains(&id) && seen.insert(k) {
                out.push((k.0, id, names.name_of(k.0, id)?.to_owned()));
            }
        }
        Some(out)
    }
}
