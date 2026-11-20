//! Streams, guard views and what escapes a guard.

#[allow(clippy::wildcard_imports)]
use crate::handoff::*;

impl<'h> Walk<'_, 'h> {
    /// A stream closed, dropped or unwrapped: the views made of it
    /// (`r.lines()`) end with it.
    pub(crate) fn stream_released(&mut self, callee: FunctionId, args: &[HirExpr]) {
        let Some(f) = self.c.fns.get(&callee) else {
            return;
        };
        let path = f
            .name
            .split(nabic_ir_common::MONO_MARK)
            .next()
            .unwrap_or("");
        if !["_drop", "_close", "_close_async", "__into_inner"]
            .iter()
            .any(|m| path.ends_with(m))
        {
            return;
        }
        let Some(g) = args
            .first()
            .and_then(place)
            .filter(|p| p.1.is_empty())
            .map(|p| p.0)
        else {
            return;
        };
        if self.is_guard(g) {
            return;
        }
        for s in self.cur.states() {
            let Some(tied) = s.tied.remove(&g) else {
                continue;
            };
            let at_tie = tied.values().next().map(|(sp, _)| *sp);
            let before: BTreeSet<Place> = s.gone.keys().cloned().collect();
            for (q, (span, tie)) in tied {
                mark_gone(s, q, Gone::Guard(span, g, tie));
            }
            if let Some(span) = at_tie {
                holders_go(s, &before, Gone::Guard(span, g, Tie::Shared));
            }
        }
    }

    /// `e` moves what it carries (stored, passed by value, captured): a
    /// stream among it ends its views, as a close would.
    pub(crate) fn stream_moved(&mut self, e: &HirExpr, at: Span) {
        let mut fl = Vec::new();
        match place(e) {
            Some(p) => fl.push((Fk::Is(Vec::new()), p)),
            None => self.flows(e, &mut fl),
        }
        for (_, q) in fl {
            let g = q.0;
            if !q.1.is_empty() || !self.stream_view(g) || self.is_guard(g) {
                continue;
            }
            let mut any = false;
            for s in self.cur.states() {
                let Some(tied) = s.tied.remove(&g) else {
                    continue;
                };
                any = true;
                let at_tie = tied.values().next().map(|(sp, _)| *sp);
                let before: BTreeSet<Place> = s.gone.keys().cloned().collect();
                for (q, (span, _)) in tied {
                    mark_gone(s, q, Gone::Guard(span, g, Tie::Moved));
                }
                if let Some(span) = at_tie {
                    holders_go(s, &before, Gone::Guard(span, g, Tie::Moved));
                }
            }
            if any {
                self.moved_at.insert(g, at);
            }
        }
    }

    /// The arguments a direct call keeps move what they carry; one it only
    /// reads (`r.capacity()`) is lent and comes back.
    pub(crate) fn args_moved(&mut self, callee: FunctionId, args: &[HirExpr]) {
        let Some(f) = self.c.fns.get(&callee) else {
            return;
        };
        for (i, (a, p)) in args.iter().zip(&f.params).enumerate() {
            let i = u32::try_from(i).unwrap_or(u32::MAX);
            if !p.by_ref && self.c.keeps.keeps(callee, i) {
                self.stream_moved(a, a.span);
            }
        }
    }

    /// `ty` is the future, generator or stream a coroutine call returns.
    pub(crate) fn is_coroutine(&self, ty: &HirTy) -> bool {
        let HirTy::Struct(id) = ty else { return false };
        let structs = self.c.env.structs();
        structs.get(id.0 as usize).is_some_and(|s| {
            let base = s.name.split("__").next().unwrap_or_default();
            let base = base.split('<').next().unwrap_or_default();
            CORO_TYPES.contains(&base)
        })
    }

    /// A tie to `g` is a view of a stream, not of what a lock protects.
    pub(crate) fn stream_view(&self, g: LocalId) -> bool {
        g != VIEW && !self.is_guard(g)
    }

    /// The help of a stream view's error: the view made at `site` of `g`.
    pub(crate) fn view_help(&self, g: LocalId, site: Span) -> String {
        let gname = self.name(g);
        let made = self
            .c
            .src(site)
            .map(|t| t.trim_start_matches("return").trim().trim_end_matches(';'))
            .filter(|t| t.len() <= 40 && !t.contains('\n') && owning_form(t).is_some())
            .map_or_else(|| format!("{gname}.lines()"), str::to_owned);
        let own = owning_form(&made).unwrap_or_else(|| format!("{gname}.into_lines()"));
        let read = if made.contains(".take(") {
            format!("`try {made}.read_to_end()`")
        } else if made.contains("_async(") {
            format!("`try await {made}.collect()`")
        } else if made.contains(".incoming(") {
            "accept the connections there".to_owned()
        } else {
            format!("`try {made}.collect()`")
        };
        format!(
            "help: `{made}` reads through `{gname}` and stays in its scope; use it there \
             ({read}), or take the owning form `{own}`, which owns `{gname}` and closes it \
             with itself"
        )
    }

    /// The error of a stream view of `g` (made at `site`) at `at`.
    pub(crate) fn view_error(
        &self,
        at: Span,
        g: LocalId,
        site: Span,
        msg: String,
    ) -> NabiDiagnostic {
        let d = NabiDiagnostic::error(Code::STREAM_VIEW_ESCAPE, at, msg);
        let d = if site == at {
            d
        } else {
            d.with_secondary(site, format!("a view of `{}` made here", self.name(g)))
        };
        d.with_note(self.view_help(g, site))
    }

    /// Handing over what a lock still guards (`tx.send(g.get())` inside
    /// `using`) shares it with the lock's other users.
    pub(crate) fn guarded_handoff(&mut self, qs: &[Place], arg: Option<&HirExpr>, what: What) {
        let Some(arg) = arg else { return };
        let Some((g, site)) = self.view_of(qs, arg) else {
            return;
        };
        let text = self
            .c
            .src(arg.span)
            .filter(|t| t.len() <= 40 && !t.contains('\n'))
            .map_or_else(|| "this value".to_owned(), |t| format!("`{t}`"));
        let to = match what {
            What::Channel => "sent through a channel",
            What::Task => "spawned as a task",
            What::TaskArg => "handed to a task",
            What::Closure => "moved into a closure",
            What::Freeze => "frozen",
            _ => "moved into a lock",
        };
        if g == VIEW {
            self.diags.push(
                NabiDiagnostic::error(
                    Code::THREAD_LOCAL_ESCAPE,
                    arg.span,
                    format!(
                        "{text} can't be {to}: it is the value `with` lends to its closure (a \
                         `ThreadLocal`'s, a locked `Mutex`'s)"
                    ),
                )
                .with_note(format!(
                    "help: hand over a separate copy (`{}.clone()`), or take a `ThreadLocal`'s \
                     value out with `replace` / `take`",
                    self.c.src(arg.span).unwrap_or("value")
                )),
            );
            return;
        }
        let gname = self.name(g);
        if self.stream_view(g) {
            let d = self.view_error(
                arg.span,
                g,
                site,
                format!("{text} can't be {to}: it is a view of `{gname}`"),
            );
            self.diags.push(d);
            return;
        }
        let ty = if self.guard_what(g) == What::RwLock {
            "RwLock"
        } else {
            "Mutex"
        };
        self.diags.push(
            NabiDiagnostic::error(
                Code::SHARED_HANDOFF,
                arg.span,
                format!("{text} can't be {to}: the `{ty}` behind `{gname}` still holds it"),
            )
            .with_secondary(site, "taken out of the lock here")
            .with_note(format!(
                "help: hand over a deep copy (`{}`), or take it out of the lock first \
                 (`{gname}.set(..)` a new value)",
                deep_copy(self.c.src(arg.span).unwrap_or("value"))
            )),
        );
    }

    /// The guard (or `VIEW`) whose protected value `arg` (carrying `qs`)
    /// is or holds, and where it was taken out.
    pub(crate) fn view_of(&self, qs: &[Place], arg: &HirExpr) -> Option<(LocalId, Span)> {
        let mut hit: Option<(LocalId, Span)> = self.got_from(arg);
        for (_, s) in &self.cur.0 {
            for (g, tied) in &s.tied {
                let mut probe = St {
                    gone: BTreeMap::new(),
                    tied: BTreeMap::new(),
                    edges: s.edges.clone(),
                    dup: BTreeSet::new(),
                    stored: BTreeMap::new(),
                };
                for q in tied.keys() {
                    mark_gone(&mut probe, q.clone(), Gone::To(Span::DUMMY, What::Mutex));
                }
                if qs.iter().any(|q| probe.gone.keys().any(|x| overlap(x, q))) {
                    hit = tied.values().next().map(|(sp, _)| (*g, *sp));
                }
            }
        }
        hit
    }

    /// A call whose callee keeps an argument that is a scoped view (what
    /// a lock protects, a `with` closure's value): the view would outlive
    /// its scope.
    pub(crate) fn kept_views(
        &mut self,
        s: &Summary,
        args: &[&HirExpr],
        call: &HirExpr,
        callee: Option<&HirExpr>,
    ) {
        if s.keeps.is_empty() || self.c.in_unsafe(call.span) {
            return;
        }
        let trusted = match &call.kind {
            HirExprKind::Call { callee, .. } => self
                .c
                .fns
                .get(callee)
                .is_some_and(|f| TRUSTED.iter().any(|m| f.name.starts_with(m))),
            _ => false,
        };
        let coroutine = match &call.kind {
            HirExprKind::Call { callee, .. } => self
                .c
                .fns
                .get(callee)
                .is_some_and(|f| self.is_coroutine(&f.ret_ty)),
            _ => false,
        };
        let params: HashSet<LocalId> = self.f.params.iter().map(|p| p.local).collect();
        for &i in &s.keeps {
            let Some(arg) = args.get(i as usize).copied() else {
                continue;
            };
            if !self.c.tracked(&arg.ty)
                || place(arg).is_some_and(|p| p.1.is_empty() && self.is_guard(p.0))
            {
                continue;
            }
            // A collection of this body keeping it is followed like the
            // view itself (`live.push(t)`, then `g.set(live)`).
            let into_local = trusted
                && args.iter().enumerate().all(|(j, a)| {
                    j == i as usize
                        || !self.c.tracked(&a.ty)
                        || place(a).is_some_and(|p| !params.contains(&p.0))
                });
            if into_local {
                continue;
            }
            let mut fl = Vec::new();
            self.flows(arg, &mut fl);
            let qs: Vec<Place> = fl.into_iter().map(|(_, q)| q).collect();
            let Some((g, site)) = self.view_of(&qs, arg) else {
                continue;
            };
            // A coroutine keeps its arguments in the frame it returns,
            // which stays tied to the stream.
            if coroutine && self.stream_view(g) {
                continue;
            }
            {
                let mut k = self.c.kept_at.borrow_mut();
                for sp in [site, call.span, arg.span] {
                    k.insert((sp.file, sp.start));
                }
            }
            if !self
                .reported
                .insert(((g, vec![u32::MAX]), arg.span.file, arg.span.start))
            {
                continue;
            }
            let text = self
                .c
                .src(arg.span)
                .filter(|t| t.len() <= 40 && !t.contains('\n'));
            let shown = text.map_or_else(|| "this value".to_owned(), |t| format!("`{t}`"));
            let named = callee.filter(|c| place(c).is_some_and(|p| !self.hidden(p.0)));
            let who = match (callee.map(|c| &c.kind), &call.kind) {
                (_, HirExprKind::Call { callee: id, .. }) | (Some(HirExprKind::FnRef(id)), _) => {
                    self.c
                        .fns
                        .get(id)
                        .map(|f| format!("`{}`", shown_fn(self.c, f)))
                }
                _ => named.map(|c| format!("`{}`", self.show(c))),
            }
            .unwrap_or_else(|| "the function called".to_owned());
            let d = if g == VIEW {
                NabiDiagnostic::error(
                    Code::THREAD_LOCAL_ESCAPE,
                    arg.span,
                    format!(
                        "{shown} is the value `with` lends to its closure, and {who} keeps it \
                         past the call"
                    ),
                )
                .with_note(format!(
                    "help: the value may not leave the closure; pass a copy (`{}`), or take a \
                     `ThreadLocal`'s value out with `replace` / `take`",
                    deep_copy(text.unwrap_or("value"))
                ))
            } else if self.stream_view(g) {
                let gname = self.name(g);
                self.view_error(
                    arg.span,
                    g,
                    site,
                    format!(
                        "{shown} is a view of `{gname}`, and {who} keeps it past the call, out \
                         of `{gname}`'s scope"
                    ),
                )
            } else {
                let gname = self.name(g);
                let ty = if self.guard_what(g) == What::RwLock {
                    "RwLock"
                } else {
                    "Mutex"
                };
                let copy = self
                    .c
                    .src(site)
                    .filter(|t| t.len() <= 40 && !t.contains('\n'))
                    .map_or_else(|| format!("{gname}.get_clone()"), deep_copy);
                let d = NabiDiagnostic::error(
                    Code::USE_AFTER_UNLOCK,
                    arg.span,
                    format!(
                        "{shown} is a view of what the `{ty}` behind `{gname}` protects, and {who} \
                         keeps it past the call"
                    ),
                );
                let d = if site == arg.span {
                    d
                } else {
                    d.with_secondary(site, "taken out of the lock here")
                };
                d.with_note(format!(
                    "help: `{gname}`'s `using` block releases the lock while {who} still holds \
                     it; pass a deep copy, `{copy}`"
                ))
            };
            self.diags.push(d);
        }
    }

    /// A `with` closure keeps its value (a capture, what other threads
    /// reach) past the call.
    pub(crate) fn view_kept(&mut self, name: &str, at: Span) {
        self.diags.push(
            NabiDiagnostic::error(
                Code::THREAD_LOCAL_ESCAPE,
                at,
                format!(
                    "this closure keeps `{name}`, the value `with` lends, past `with`: the \
                     value is only lent to the closure"
                ),
            )
            .with_note(format!(
                "help: keep a copy (`{name}.clone()`), or take a `ThreadLocal`'s value out with \
                 `replace` / `take`"
            )),
        );
    }

    /// A `using` block whose value is what its lock protects: the block's
    /// end releases the lock before that value is used.
    pub(crate) fn block_value_escapes(&mut self, site: Span, guard: LocalId, at: Span) {
        if self.reported.contains(&((guard, Vec::new()), 0, 0)) {
            return;
        }
        if !self
            .reported
            .insert(((guard, Vec::new()), site.file, site.start))
        {
            return;
        }
        let gname = self.name(guard);
        if self.stream_view(guard) {
            let d = self.view_error(
                at,
                guard,
                site,
                format!("this is a view of `{gname}`, used after `{gname}`'s scope closed it"),
            );
            self.diags.push(d);
            return;
        }
        let ty = if self.guard_what(guard) == What::RwLock {
            "RwLock"
        } else {
            "Mutex"
        };
        let copy = self
            .c
            .src(site)
            .filter(|t| t.len() <= 40 && !t.contains('\n'))
            .map_or_else(
                || format!("`{gname}.get_clone()`"),
                |t| format!("`{}`", deep_copy(t)),
            );
        self.diags.push(
            NabiDiagnostic::error(
                Code::USE_AFTER_UNLOCK,
                at,
                format!(
                    "the value of `{gname}`'s `using` block is what the `{ty}` protects, and the \
                     block releases the lock before it is used"
                ),
            )
            .with_secondary(site, "taken out of the lock here")
            .with_note(format!(
                "help: use it inside the `using` block, or make the block's value {copy}"
            )),
        );
    }

    /// A `throw` of `err`: its error leaves like a returned value, so it
    /// is checked as one. What that store itself reports is dropped; an
    /// error already at the `throw` stands alone.
    pub(crate) fn throw_escapes(&mut self, err: &'h HirExpr) {
        let at = err.span;
        if self.diags.iter().any(|d| {
            d.is_error()
                && d.primary
                    .as_ref()
                    .is_some_and(|p| p.span.file == at.file && p.span.start == at.start)
        }) {
            return;
        }
        let (cur, n, reported) = (self.cur.clone(), self.diags.len(), self.reported.clone());
        let returning = std::mem::replace(&mut self.returning, true);
        self.store(&(RET, Vec::new()), err, false);
        self.returning = returning;
        self.diags.truncate(n);
        self.reported.clone_from(&reported);
        self.escapes_guard(err, Exit::Throw);
        self.reported = reported;
        self.cur = cur;
    }

    /// The guard whose `get()` value `e` holds as a part (a field of a
    /// thrown struct, through the compiler's temporaries), and where.
    pub(crate) fn holds_get(&self, e: &HirExpr, depth: u32) -> Option<(LocalId, Span)> {
        if depth > 8 {
            return None;
        }
        if let Some(hit) = self.got_from(e) {
            return Some(hit);
        }
        match &e.kind {
            HirExprKind::StructInit { fields: xs, .. }
            | HirExprKind::Tuple { fields: xs }
            | HirExprKind::EnumVariantInit { args: xs, .. }
            | HirExprKind::ArrayInit { elements: xs, .. } => {
                xs.iter().find_map(|x| self.holds_get(x, depth + 1))
            }
            HirExprKind::SumWrap { value: Some(x), .. } => self.holds_get(x, depth + 1),
            HirExprKind::Block(b) => self.holds_get(b.trailing.as_ref()?, depth + 1),
            HirExprKind::Local(x) if self.hidden(*x) => {
                let mut init = None;
                let body = self.f.body.as_ref()?;
                let mut find = |b: &'h HirBlock| {
                    for s in &b.stmts {
                        if let HirStmt::Let { local, init: i } = s {
                            if local == x {
                                init = Some(i);
                            }
                        }
                    }
                };
                find(body);
                nabic_hir::each_block(body, &mut |e| {
                    if let HirExprKind::Block(b) = &e.kind {
                        find(b);
                    }
                });
                self.holds_get(init?, depth + 1)
            }
            _ => None,
        }
    }

    /// A `return`, `yield` or `throw` inside a `using` block hands out
    /// what the lock protects: the block's end releases the lock before the
    /// caller is done with it. So does a `with` closure's value leaving it.
    pub(crate) fn escapes_guard(&mut self, e: &HirExpr, exit: Exit) {
        let yields = exit == Exit::Yield;
        let at = e.span;
        let params: HashSet<LocalId> = self.f.params.iter().map(|p| p.local).collect();
        // A variable's use reports it; a compiler temporary's does not.
        let temp = place(e).is_none_or(|p| self.hidden(p.0));
        let mut hit: Option<(LocalId, Span)> = None;
        for (_, s) in &self.cur.0 {
            // Taken out of the lock into a compiler temporary, which the
            // block's end released (`return g.get()` inside `using`).
            for (q, why) in &s.gone {
                if let (true, true, Gone::Guard(site, g, _)) = (temp, q.0 == RET, why) {
                    hit = Some((*g, *site));
                }
            }
            for (g, tied) in &s.tied {
                if params.contains(g) {
                    continue;
                }
                let mut probe = St {
                    gone: BTreeMap::new(),
                    tied: BTreeMap::new(),
                    edges: s.edges.clone(),
                    dup: BTreeSet::new(),
                    stored: BTreeMap::new(),
                };
                for q in tied.keys() {
                    mark_gone(&mut probe, q.clone(), Gone::To(Span::DUMMY, What::Mutex));
                }
                if probe.gone.keys().any(|q| q.0 == RET) {
                    let site = tied.values().map(|(sp, _)| *sp).next().unwrap_or(at);
                    hit = Some((*g, site));
                }
            }
        }
        if exit == Exit::Throw {
            hit = hit.or_else(|| self.holds_get(e, 0));
        }
        let Some((g, site)) = hit else { return };
        // One report: not again as the block's value.
        self.reported.insert(((g, Vec::new()), 0, 0));
        if !self
            .reported
            .insert(((RET, Vec::new()), site.file, site.start))
        {
            return;
        }
        if g == VIEW {
            let how = match exit {
                Exit::Return => "returns",
                Exit::Yield => "yields",
                Exit::Throw => "throws",
            };
            let d = NabiDiagnostic::error(
                Code::THREAD_LOCAL_ESCAPE,
                at,
                format!(
                    "this {how} the value `with` lends out of its closure: the value is only \
                     lent to it"
                ),
            );
            self.diags.push(d.with_note(
                "help: return a copy (`.clone()`) or what you computed from it, or take a \
                 `ThreadLocal`'s value out with `replace` / `take`",
            ));
            return;
        }
        let gname = self.name(g);
        if self.stream_view(g) {
            let how = match exit {
                Exit::Return => "returns",
                Exit::Yield => "yields",
                Exit::Throw => "throws",
            };
            let d = self.view_error(
                at,
                g,
                site,
                format!("this {how} a view of `{gname}` out of `{gname}`'s scope"),
            );
            self.diags.push(d);
            return;
        }
        let ty = if self.guard_what(g) == What::RwLock {
            "RwLock"
        } else {
            "Mutex"
        };
        let clone_trait = self.c.env.lang("clone_trait");
        let cloneable = clone_trait.is_empty() || self.c.env.has_trait(&e.ty, clone_trait);
        // A copy of what is returned (an element, not the whole list).
        let copy = self
            .c
            .src(at)
            .map(|t| {
                t.trim_start_matches("return")
                    .trim_start_matches("yield")
                    .trim()
                    .trim_end_matches(';')
                    .trim()
            })
            .filter(|t| !t.is_empty())
            .or_else(|| self.c.src(site))
            .filter(|t| t.len() <= 40 && !t.contains('\n'))
            .map_or_else(
                || "a separate copy (`.clone()`)".to_owned(),
                |t| format!("`{}`", deep_copy(t)),
            );
        let copy = if cloneable {
            copy
        } else {
            "a value built before it was shared (keep one outside the lock, and give the lock \
             its own)"
                .to_owned()
        };
        if exit == Exit::Throw {
            let taken = self
                .c
                .src(site)
                .filter(|t| t.len() <= 40 && !t.contains('\n'))
                .map_or_else(
                    || "a deep copy (`.clone()`)".to_owned(),
                    |t| format!("a deep copy, `{}`", deep_copy(t)),
                );
            self.diags.push(
                NabiDiagnostic::error(
                    Code::USE_AFTER_UNLOCK,
                    at,
                    format!(
                        "this throws a value out of the `{ty}`, and `{gname}`'s `using` block \
                         releases the lock before the catcher uses it"
                    ),
                )
                .with_secondary(site, "taken out of the lock here")
                .with_note(format!(
                    "help: the lock stops protecting it; build the error from {taken}, or \
                     handle it inside the `using` block"
                )),
            );
            return;
        }
        self.diags.push(
            NabiDiagnostic::error(
                Code::USE_AFTER_UNLOCK,
                at,
                if yields {
                    format!(
                        "this yields a value out of the `{ty}`, and `{gname}`'s `using` block \
                         releases the lock while the caller may still use it"
                    )
                } else {
                    format!(
                        "this returns a value out of the `{ty}`, and `{gname}`'s `using` block \
                         releases the lock before the caller uses it"
                    )
                },
            )
            .with_secondary(site, "taken out of the lock here")
            .with_note(format!(
                "help: the lock stops protecting it; {} {copy}, or do the work inside the \
                 `using` block",
                if yields { "yield" } else { "return" }
            )),
        );
    }

    /// Handing over a value other threads may reach without this hand-off
    /// (a thread-local's value, a field of a `Sync` object) is an error.
    pub(crate) fn shared_handoff(&mut self, qs: &[Place], arg: Option<&HirExpr>, what: What) {
        let Some(arg) = arg else { return };
        let shared = self.cur.0.iter().any(|(_, s)| {
            let mut probe = St {
                gone: BTreeMap::new(),
                tied: BTreeMap::new(),
                edges: s.edges.clone(),
                dup: s.dup.clone(),
                stored: BTreeMap::new(),
            };
            for q in qs {
                mark_gone(&mut probe, q.clone(), Gone::To(Span::DUMMY, What::Mutex));
            }
            probe.gone.keys().any(|g| g.0 == WORLD)
        });
        if !shared {
            return;
        }
        let text = self
            .c
            .src(arg.span)
            .filter(|t| t.len() <= 40 && !t.contains('\n'))
            .map_or_else(|| "this value".to_owned(), |t| format!("`{t}`"));
        let to = match what {
            What::Channel => "sent through a channel",
            What::Task => "spawned as a task",
            What::TaskArg => "handed to a task",
            What::Closure => "moved into a closure",
            What::Freeze => "frozen",
            _ => "moved into a lock",
        };
        // The object it is read out of: a call's receiver (`TL.get()`).
        let from = match &arg.kind {
            HirExprKind::Call { args, .. } => args.first().and_then(|a| self.c.src(a.span)),
            HirExprKind::FieldRead { target, .. } => self.c.src(target.span),
            _ => None,
        }
        .filter(|t| t.len() <= 30 && !t.contains('\n'))
        .map_or_else(String::new, |t| format!(" `{t}`"));
        self.diags.push(
            NabiDiagnostic::error(
                Code::SHARED_HANDOFF,
                arg.span,
                format!(
                    "{text} can't be {to}: other code reaches it through{from} (a thread-local \
                     or a `Sync` object), so it would be used from two threads"
                ),
            )
            .with_note(format!(
                "help: hand over a separate copy: `{}.clone()`",
                self.c.src(arg.span).unwrap_or("value")
            )),
        );
    }
}
