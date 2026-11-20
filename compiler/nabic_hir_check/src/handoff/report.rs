//! Uses of gone places and their diagnostics.

#[allow(clippy::wildcard_imports)]
use crate::handoff::*;

impl Walk<'_, '_> {
    pub(crate) fn name(&self, l: LocalId) -> String {
        self.f
            .locals
            .iter()
            .find(|x| x.id == l)
            .and_then(|x| x.name.clone())
            .filter(|n| !n.starts_with('#'))
            .unwrap_or_else(|| "the value".to_owned())
    }

    /// `e` (a place) as written: `s.items`, `t.0`.
    pub(crate) fn show(&self, e: &HirExpr) -> String {
        match &e.kind {
            HirExprKind::Local(l) => self.name(*l),
            HirExprKind::FieldRead {
                target,
                struct_id,
                field_index,
                ..
            } => {
                let structs = self.c.env.structs();
                let f = structs
                    .get(struct_id.0 as usize)
                    .and_then(|s| s.fields.get(*field_index as usize))
                    .map_or_else(|| field_index.to_string(), |f| f.name.clone());
                drop(structs);
                format!("{}.{f}", self.show(target))
            }
            HirExprKind::TupleField { target, index, .. } => {
                format!("{}.{index}", self.show(target))
            }
            HirExprKind::SumWrap { value: Some(x), .. } | HirExprKind::Cast { operand: x, .. } => {
                self.show(x)
            }
            HirExprKind::Block(b) => b
                .trailing
                .as_ref()
                .map_or_else(String::new, |t| self.show(t)),
            _ => "the value".to_owned(),
        }
    }

    pub(crate) fn local_ty(&self, l: LocalId) -> Option<HirTy> {
        if l == RET {
            return Some(self.f.ret_ty.clone());
        }
        if (SHADOW..TEMP).contains(&l.0) {
            return self
                .f
                .params
                .get((l.0 - SHADOW) as usize)
                .map(|p| p.ty.clone());
        }
        if let Some(t) = self.temp_tys.get(&l) {
            return Some(t.clone());
        }
        self.f
            .locals
            .iter()
            .find(|x| x.id == l)
            .map(|x| x.ty.clone())
    }

    /// The fields of a value of type `t` worth following, by path step.
    pub(crate) fn fields(&self, t: &HirTy) -> Option<Vec<(u32, HirTy)>> {
        let all: Vec<(u32, HirTy)> = match t {
            HirTy::Struct(id) => {
                let structs = self.c.env.structs();
                let s = structs.get(id.0 as usize)?;
                #[allow(clippy::cast_possible_truncation)]
                s.fields
                    .iter()
                    .enumerate()
                    .map(|(i, f)| (i as u32, f.ty.clone()))
                    .collect()
            }
            #[allow(clippy::cast_possible_truncation)]
            HirTy::Tuple(ts) => ts
                .iter()
                .enumerate()
                .map(|(i, t)| (i as u32 | TUPLE, t.clone()))
                .collect(),
            HirTy::Enum(id) => {
                let enums = self.c.env.enums();
                let e = enums.get(id.0 as usize)?;
                let mut out = Vec::new();
                for (v, var) in e.variants.iter().enumerate() {
                    #[allow(clippy::cast_possible_truncation)]
                    let v = v as u32;
                    match (&var.payload_ty, var.arity) {
                        (Some(t), 1) => out.push((payload(v, 0), t.clone())),
                        (Some(HirTy::Tuple(ts)), _) => {
                            #[allow(clippy::cast_possible_truncation)]
                            out.extend(
                                ts.iter()
                                    .enumerate()
                                    .map(|(i, t)| (payload(v, i as u32), t.clone())),
                            );
                        }
                        _ => {}
                    }
                }
                out
            }
            _ => return None,
        };
        Some(all.into_iter().filter(|(_, t)| self.c.tracked(t)).collect())
    }

    pub(crate) fn place_ty(&self, p: &Place) -> Option<HirTy> {
        let mut t = self.local_ty(p.0)?;
        for step in &p.1 {
            t = self.fields(&t)?.into_iter().find(|(k, _)| k == step)?.1;
        }
        Some(t)
    }

    /// The paths below `base` (relative to it) that cover it except
    /// `base ++ rel` (`None`: its type is not known).
    pub(crate) fn split(&self, base: &Place, rel: &[u32]) -> Option<Vec<Vec<u32>>> {
        let Some((&k0, rest)) = rel.split_first() else {
            return Some(Vec::new());
        };
        let t = self.place_ty(base)?;
        let mut out = Vec::new();
        for (k, _) in self.fields(&t)? {
            if k == k0 {
                let below = self.split(&extend(base, &[k]), rest)?;
                out.extend(below.into_iter().map(|s| [&[k][..], &s].concat()));
            } else {
                out.push(vec![k]);
            }
        }
        Some(out)
    }

    /// A use of place `p`, written `e` (`None`: the whole local `p.0`).
    #[allow(clippy::many_single_char_names)]
    pub(crate) fn use_place(&mut self, p: &Place, ty: &HirTy, written: Option<&HirExpr>, at: Span) {
        if let Some(i) = self.param_index(p.0).filter(|_| p.1.len() <= MAX_DEPTH) {
            self.uses.insert((i, p.1.clone()));
        }
        let Some((gp, g)) = self.cur.0.iter().find_map(|(_, st)| {
            st.gone
                .iter()
                .find(|(q, _)| overlap(p, q))
                .map(|(q, why)| (q.clone(), *why))
        }) else {
            return;
        };
        if !self.c.tracked(ty) || self.c.in_unsafe(at) || self.copy_out(written, at) {
            return;
        }
        // A view a callee kept was reported at the call.
        let o = g.origin();
        if self.c.kept_at.borrow().contains(&(o.file, o.start)) {
            return;
        }
        // A compiler-made local (`#..`) is not reported: what it is copied
        // into is. One holding a `using` block's value is the block's value
        // leaving the lock.
        if self.hidden(p.0) {
            // A `return`'s value: `escapes_guard` reports it.
            if let Gone::Guard(site, guard, tie) = g {
                if matches!(tie, Tie::Taken | Tie::Shared)
                    && !self.returning
                    && self.thrown != Some(p.0)
                {
                    self.block_value_escapes(site, guard, at);
                }
            }
            return;
        }
        if let Gone::Guard(site, guard, _) = g {
            if self
                .reported
                .contains(&((guard, Vec::new()), site.file, site.start))
            {
                return;
            }
        }
        // What was handed over is still among `p`'s elements.
        let in_elems = gp.0 == p.0 && gp.1.len() > p.1.len() && gp.1[p.1.len()..].contains(&ELEM);
        if in_elems && self.holder_only() {
            return;
        }
        let origin = g.origin();
        if !self
            .reported
            .insert((gp.clone(), origin.file, origin.start))
        {
            return;
        }
        let mut x = written.map_or_else(|| self.name(p.0), |e| self.show(e));
        // Only a field of it went: name the field (`b.buf`).
        if gp.0 == p.0 && gp.1.len() > p.1.len() && !gp.1.contains(&ELEM) {
            if let Some(f) = self.field_path(p, &gp.1[p.1.len()..]) {
                x = format!("{x}{f}");
            }
        }
        if let Gone::Guard(site, guard, Tie::Itself) = g {
            let gname = self.name(guard);
            let line = self.line(site, at);
            let d = if p.1.is_empty() && self.is_guard(p.0) {
                NabiDiagnostic::error(
                    Code::USE_AFTER_UNLOCK,
                    at,
                    format!(
                        "`{x}` is the guard `{gname}`, and `{gname}`'s `using` block released \
                         the lock at {line}"
                    ),
                )
                .with_secondary(site, "the lock is released here")
                .with_note(format!(
                    "help: bind a guard with `using` once, and use `{x}` itself inside its block"
                ))
            } else {
                NabiDiagnostic::error(
                    Code::USE_AFTER_UNLOCK,
                    at,
                    format!(
                        "`{x}` holds a copy of the guard `{gname}`, whose `using` block released \
                         the lock at {line}"
                    ),
                )
                .with_secondary(site, "the lock is released here")
                .with_note(format!(
                    "help: a guard is only good inside its `using` block; do the work there, or \
                     keep a deep copy of what it protects (`{gname}.get_clone()`)"
                ))
            };
            self.diags.push(d);
            return;
        }
        if let Gone::Via(site, _) = g {
            self.c.via_at.borrow_mut().insert((at.file, at.start), site);
        }
        // A spawned future that captures a guard is an error at the spawn
        // (`lower::send`); the guard's release is no second error.
        if matches!(g, Gone::To(_, What::Task)) && p.1.is_empty() && self.is_guard(p.0) {
            return;
        }
        let mut d = self.diagnostic(&x, g, written, at);
        // What was handed over, as written there (`rows[0]`).
        let handed = self
            .c
            .handed
            .borrow()
            .get(&(origin.file, origin.start))
            .cloned()
            .unwrap_or_else(|| "a value".to_owned());
        let elem = elem_of(&gp);
        let twice = in_elems
            && elem
                .as_ref()
                .is_some_and(|el| self.cur.0.iter().any(|(_, st)| is_dup(st, el)));
        if twice {
            let el = elem.unwrap_or_else(|| gp.clone());
            let mut lines: BTreeSet<u32> = BTreeSet::new();
            for (_, st) in &self.cur.0 {
                for a in elem_aliases(st, &el) {
                    for (p, sp) in &st.stored {
                        if within(p, &a) || within(&a, p) {
                            lines.extend(sp.iter().filter(|x| x.file == at.file).map(|x| x.line));
                        }
                    }
                }
            }
            let at_lines = match lines.iter().take(2).collect::<Vec<_>>()[..] {
                [a, b] => format!(" (stored at lines {a} and {b})"),
                [a] => format!(" (stored at line {a})"),
                _ => String::new(),
            };
            let given = match g {
                Gone::To(site, _) => self
                    .c
                    .direct
                    .borrow()
                    .get(&(site.file, site.start))
                    .map(|(n, _)| n.clone()),
                _ => None,
            }
            .unwrap_or_else(|| "the value".to_owned());
            let line = self.line(origin, at);
            d.message = format!(
                "`{x}` may hold one object twice{at_lines}, and one of them was handed to another \
                 owner at {line}"
            );
            d.notes = vec![format!(
                "help: store separate values (`.clone()` what you store), or hand over \
                 `{given}.clone()`"
            )];
        } else if in_elems && self.spawned_fn(g) {
            self.elem_spawned_twice(&mut d, p, &x, &handed, origin, at);
        } else if in_elems {
            let line = self.line(origin, at);
            d.message = format!(
                "`{x}` still holds {handed}, which was handed to another owner at {line} (as \
                 an element, or a value one shares)"
            );
            let take = self.take_out(p, &x);
            let copy = if handed.starts_with('`') {
                format!("`{}.clone()`", handed.trim_matches('`'))
            } else {
                "a copy (`.clone()`)".to_owned()
            };
            d.notes = vec![format!(
                "help: take it out of `{x}` first ({take}) and hand that over, or hand over \
                 {copy}"
            )];
        }
        self.diags.push(d);
    }

    /// A call of `thread::*` or `par::*`: what runs the `fn` values it is
    /// given, for the errors that name it.
    pub(crate) fn note_runner(&self, e: &HirExpr) {
        let HirExprKind::Call { callee, .. } = &e.kind else {
            return;
        };
        let Some(f) = self.c.fns.get(callee) else {
            return;
        };
        let path = f.facts.path(&f.name);
        let runner = if path.starts_with("std::thread::") {
            Runner::Thread
        } else if path.starts_with("core::par::") {
            Runner::Par
        } else {
            return;
        };
        self.c
            .runners
            .borrow_mut()
            .insert((e.span.file, e.span.start), runner);
    }

    /// The help for `who`, handed to a task (or a thread, a `par` job) as
    /// `written`; `fn_owner`: a `fn` value that may own moved values.
    pub(crate) fn task_help(
        &self,
        what: What,
        runner: Runner,
        fn_owner: bool,
        written: Option<&HirExpr>,
        who: &str,
        keep: &str,
    ) -> String {
        let future = what == What::Task
            && written.is_some_and(|e| match &e.ty {
                HirTy::Struct(id) => {
                    let name = self.c.env.struct_def(*id).name;
                    name.starts_with("core::future::AsyncFuture")
                        || name.starts_with("core::future::TryAsyncFuture")
                }
                _ => false,
            });
        let async_fn = written.is_some_and(|e| self.async_closure(e));
        if (fn_owner && async_fn)
            || (future && written.is_some_and(|e| self.reaches_moved_closure(e)))
        {
            "help: the futures of one `async [move ..]` closure share what it moved in; await \
             them here one after another, or make another closure for the other task (moving \
             in a `.clone()` of what it needs)"
                .to_owned()
        } else if fn_owner {
            format!(
                "help: {} owns the closure and what it moved in; make another closure here \
                 (moving in a `.clone()` of what it needs), or, for a `fn` value that moves \
                 nothing in, write `+ Send + Sync` on its type",
                runner.took().1
            )
        } else if future {
            format!(
                "help: the task runs it now; call the async fn again for a second future, or \
                 give `{who}` a new value before using it again"
            )
        } else {
            format!(
                "help: the task works on its own copy, so changes it makes are not seen here: \
                 return them from the task (`await`), {keep}, or share a collection it only \
                 reads with no copy: `.freeze()` it"
            )
        }
    }

    /// `who` used after `call` (at `site`) handed what it holds over inside
    /// the standard library, whose names are not the user's to see.
    pub(crate) fn lib_handoff(
        &self,
        who: &str,
        call: &str,
        site: Span,
        at: Span,
    ) -> NabiDiagnostic {
        NabiDiagnostic::error(
            Code::USE_AFTER_HANDOFF,
            at,
            format!(
                "`{who}` is used after {call} at {}, which gave what `{who}` holds to another \
                 owner",
                self.line(site, at)
            ),
        )
        .with_secondary(site, format!("`{who}` used up here"))
        .with_note(format!(
            "help: a call that takes `{who}` by value works on its own copy, so `{who}` here is \
             left as it was; do the work with `{who}` in one function, or return it from the \
             function you pass it to and use what it returns"
        ))
    }

    /// What runs the `fn` values the call at `site` is given.
    pub(crate) fn runner(&self, site: Span) -> Runner {
        self.c
            .runners
            .borrow()
            .get(&(site.file, site.start))
            .copied()
            .unwrap_or(Runner::Task)
    }

    /// `e` is an `async` closure: each call makes a future of its body.
    pub(crate) fn async_closure(&self, e: &HirExpr) -> bool {
        let ret = match &e.ty {
            HirTy::Fn(_, r, _) => &**r,
            HirTy::Closure { ret, .. } => &**ret,
            _ => return false,
        };
        matches!(ret, HirTy::Struct(id) if {
            let name = self.c.env.struct_def(*id).name;
            name.starts_with("core::future::AsyncFuture")
                || name.starts_with("core::future::TryAsyncFuture")
        })
    }

    /// `g` is a spawn of a `fn` value that may own moved values.
    pub(crate) fn spawned_fn(&self, g: Gone) -> bool {
        matches!(g, Gone::To(site, What::Task)
            if self.c.fn_owners.borrow().contains(&(site.file, site.start)))
    }

    /// A closure that owns moved values, spawned out of a collection: the
    /// element moves once.
    pub(crate) fn elem_spawned_twice(
        &self,
        d: &mut NabiDiagnostic,
        p: &Place,
        x: &str,
        handed: &str,
        origin: Span,
        at: Span,
    ) {
        let line = self.line(origin, at);
        let (took, _) = self.runner(origin).took();
        d.code = Code::USE_AFTER_MOVE;
        d.message = format!(
            "{handed} was {took} at {line}: it may own values moved into it \
             (`[move ..]`), so it can be handed over only once"
        );
        let take = self.take_out(p, x);
        d.notes = vec![format!(
            "help: take it out of `{x}` ({take}) and hand that over once, or make another \
             closure for the second use (moving in a `.clone()` of what it needs)"
        )];
    }

    /// The innermost call is a collection method that neither reads nor
    /// returns its elements (`len`, `push`, `clear`): a use of the
    /// collection whose elements were handed over is fine there.
    pub(crate) fn holder_only(&self) -> bool {
        const SAFE: &[&str] = &[
            "core::collections::list::List__len",
            "core::collections::list::List__is_empty",
            "core::collections::list::List__capacity",
            "core::collections::list::List__clear",
            "core::collections::list::List__truncate",
            "core::collections::list::List__reserve",
            "core::collections::list::List__shrink_to_fit",
            "core::collections::list::List__push",
            "core::collections::list::List__insert",
            "core::collections::deque::Deque__len",
            "core::collections::deque::Deque__is_empty",
            "core::collections::deque::Deque__clear",
            "core::collections::deque::Deque__push_back",
            "core::collections::deque::Deque__push_front",
            "core::collections::dict::Dict__len",
            "core::collections::dict::Dict__is_empty",
            "core::collections::dict::Dict__clear",
        ];
        let Some(HirExprKind::Call { callee, .. }) = self.calls.last().map(|c| &c.kind) else {
            return false;
        };
        self.c
            .fns
            .get(callee)
            .is_some_and(|f| SAFE.iter().any(|n| is_fn(&f.name, n)))
    }

    pub(crate) fn local_name(&self, l: LocalId) -> Option<&str> {
        self.f
            .locals
            .iter()
            .find(|x| x.id == l)
            .and_then(|x| x.name.as_deref())
    }

    pub(crate) fn hidden(&self, l: LocalId) -> bool {
        self.f
            .locals
            .iter()
            .find(|x| x.id == l)
            .is_some_and(|x| x.name.as_ref().is_none_or(|n| n.starts_with('#')))
    }

    /// `e` is the value of a `ref` parameter going back to the caller
    /// (not written in the source): the caller sees what is gone of it.
    pub(crate) fn copy_out(&self, e: Option<&HirExpr>, at: Span) -> bool {
        let Some(HirExprKind::Local(l)) = e.map(|e| &e.kind) else {
            return false;
        };
        self.f.params.iter().any(|p| p.local == *l && p.by_ref)
            && self.c.src(at).is_some_and(|t| t != self.name(*l))
    }

    /// `at`: where the line of `s` is, said from the use at `from`.
    pub(crate) fn line(&self, s: Span, from: Span) -> String {
        if s.file == from.file {
            format!("line {}", s.line)
        } else {
            let path = self.c.file_name(s.file);
            let name = path.rsplit(['/', '\\']).next().unwrap_or(&path);
            format!("{name}:{}", s.line)
        }
    }

    /// The call `e` is an argument (or receiver) of, rewritten with
    /// `with` in place of `e`: `xs.push(i)` -> `g.get().push(i)`.
    pub(crate) fn in_call(&self, e: Option<&HirExpr>, with: &str) -> String {
        let fallback = with.to_owned();
        let Some(e) = e else { return fallback };
        let Some(call) = self.calls.last().filter(|c| {
            let (HirExprKind::Call { args, .. }
            | HirExprKind::CallIndirect { args, .. }
            | HirExprKind::CallClosure { args, .. }) = &c.kind
            else {
                return false;
            };
            args.iter().any(|a| a.span == e.span)
        }) else {
            return fallback;
        };
        let (Some(text), true) = (
            self.c.src(call.span),
            call.span.file == e.span.file
                && call.span.start <= e.span.start
                && e.span.end <= call.span.end,
        ) else {
            return fallback;
        };
        let (a, b) = (
            (e.span.start - call.span.start) as usize,
            (e.span.end - call.span.start) as usize,
        );
        if text.len() > 60 || text.contains('\n') || !text.is_char_boundary(a) {
            return fallback;
        }
        format!("{}{with}{}", &text[..a], &text[b..])
    }

    pub(crate) fn diagnostic(
        &self,
        who: &str,
        gone: Gone,
        written: Option<&HirExpr>,
        at: Span,
    ) -> NabiDiagnostic {
        let chk = self.c;
        let err = |code: Code, msg: String| NabiDiagnostic::error(code, at, msg);
        let keep = format!("pass `{who}.clone()` to keep a separate copy");
        match gone {
            Gone::To(site, what) => {
                let owner = chk
                    .owners
                    .borrow()
                    .get(&(site.file, site.start))
                    .filter(|(_, b)| self.scopes.contains(b))
                    .map(|(n, _)| n.clone());
                let (arg, arg_ty) = chk
                    .direct
                    .borrow()
                    .get(&(site.file, site.start))
                    .cloned()
                    .unzip();
                let line = self.line(site, at);
                let runner = self.runner(site);
                let (moved, here) = match what {
                    What::Mutex => ("moved into the `Mutex`", "moved into the `Mutex` here"),
                    What::RwLock => ("moved into the `RwLock`", "moved into the `RwLock` here"),
                    What::Channel => ("sent through a channel", "sent here"),
                    What::Handoff => ("handed to a `Handoff`", "handed over here"),
                    What::Task => match runner {
                        Runner::Task => ("spawned as a task", "spawned here"),
                        Runner::Thread => ("given to a thread", "given to the thread here"),
                        Runner::Par => ("given to a `par` job", "given to the `par` job here"),
                    },
                    What::TaskArg => ("handed to a task", "handed to the task here"),
                    What::Closure => ("moved into a closure", "moved into the closure here"),
                    What::Freeze => ("frozen", "frozen here"),
                };
                // The hand-off itself again, on the loop's next pass.
                if self.calls.last().is_some_and(|k| k.span == site) {
                    return err(Code::USE_AFTER_HANDOFF, format!(
                        "`{who}` was already {moved} by this call, on an earlier pass of the loop"
                    ))
                    .with_note(format!(
                        "help: hand over `{who}.clone()` so each pass hands over its own copy, or \
                         make `{who}` inside the loop"
                    ));
                }
                // Another handle to what was handed over.
                let (head, given) = match arg.as_deref().filter(|arg| *arg != who) {
                    Some(arg) => (
                        format!("`{who}` shares `{arg}`, which was {moved}"),
                        arg.to_owned(),
                    ),
                    None => (format!("`{who}` was {moved}"), who.to_owned()),
                };
                let keep = format!("hand over `{given}.clone()` to keep a separate copy");
                // A closure (or `fn` value) owning moved values moves once.
                let fn_owner = what == What::Task
                    && written
                        .is_some_and(|e| matches!(e.ty, HirTy::Fn(..) | HirTy::Closure { .. }));
                let diag = if fn_owner && written.is_some_and(|e| self.async_closure(e)) {
                    err(
                        Code::USE_AFTER_MOVE,
                        format!(
                            "a future of `{given}` was spawned as a task at {line}, and every \
                             future of an `async [move ..]` closure shares what it moved in"
                        ),
                    )
                } else if fn_owner {
                    err(
                        Code::USE_AFTER_MOVE,
                        format!(
                            "{head} at {line}: it may own values moved into it (`[move ..]`), so \
                             it can be handed over only once"
                        ),
                    )
                } else {
                    err(Code::USE_AFTER_HANDOFF, format!("{head} at {line}"))
                }
                .with_secondary(site, format!("`{given}` {here}"));
                match what {
                    What::Mutex | What::RwLock => {
                        let (ty, lock) = if what == What::Mutex {
                            ("Mutex", "lock")
                        } else {
                            ("RwLock", "write")
                        };
                        let through = match owner {
                            Some(lock_var) => {
                                let same = written
                                    .is_some_and(|e| arg_ty.as_ref() == Some(&e.ty.stable_key()));
                                let body = if same {
                                    self.in_call(written, "g.get()")
                                } else {
                                    ".. g.get() ..".to_owned()
                                };
                                format!(
                                    "use it through the lock: `using g = {lock_var}.{lock}() {{ {body} }}`"
                                )
                            }
                            None => format!(
                                "use it through the `{ty}`'s lock (`g.get()` inside its `using` \
                                 block)"
                            ),
                        };
                        diag.with_note(format!(
                            "help: it belongs to the `{ty}` now; {through}, or {keep}"
                        ))
                    }
                    What::Channel => diag.with_note(format!(
                        "help: the receiver owns it now; {keep}, or give `{who}` a new value before \
                         using it again"
                    )),
                    What::Task | What::TaskArg => {
                        diag.with_note(self.task_help(what, runner, fn_owner, written, who, &keep))
                    }
                    What::Closure => diag.with_note(format!(
                        "help: the closure owns it now; use it inside the closure, capture it \
                         without `move` if the closure stays on this thread, or move \
                         `{given}.clone()` to keep a separate copy"
                    )),
                    What::Freeze => {
                        let through = owner.map_or_else(
                            || "the value `freeze()` returned".to_owned(),
                            |own| format!("`{own}`"),
                        );
                        diag.with_note(format!(
                            "help: the frozen collection holds the elements now; read them \
                             through {through}, or freeze `{given}.clone()` to keep `{given}` \
                             as it was"
                        ))
                    }
                    What::Handoff => {
                        let getter = owner.map_or_else(
                            || "the `Handoff`'s `get()`".to_owned(),
                            |own| format!("`{own}.get()`"),
                        );
                        diag.with_note(format!(
                            "help: the `Handoff` owns it now; read it through {getter}, or {keep}"
                        ))
                    }
                }
            }
            Gone::Via(site, id) => {
                let why = chk.whys.borrow().0[id as usize].clone();
                let callee = chk.callees.borrow().get(&(site.file, site.start)).cloned();
                let text = chk.src(site).filter(|t| t.len() <= 60 && !t.contains('\n'));
                let named = text.is_some_and(|t| t.contains(who));
                let call = text
                    .map(|t| format!("`{t}`"))
                    .or_else(|| callee.map(|n| format!("this call of `{n}`")))
                    .unwrap_or_else(|| "this call".to_owned());
                let verb = match why.what {
                    What::Mutex | What::RwLock => format!("moved into {}", why.what.noun()),
                    What::Channel => "sent through a channel".to_owned(),
                    What::Handoff => "handed to a `Handoff`".to_owned(),
                    What::Task => "spawned as a task".to_owned(),
                    What::TaskArg => "handed to a task".to_owned(),
                    What::Closure => "moved into a closure".to_owned(),
                    What::Freeze => "frozen".to_owned(),
                };
                if why.lib {
                    return self.lib_handoff(who, &call, site, at);
                }
                let file = why.file.rsplit(['/', '\\']).next().unwrap_or(&why.file);
                let inner = if chk.file_name(site.file) == why.file {
                    format!("line {}", why.line)
                } else {
                    format!("{file}:{}", why.line)
                };
                let head = if named {
                    format!("`{who}` was {verb} by {call}")
                } else if why.what == What::Freeze {
                    format!("`{who}` shares what {call} froze")
                } else {
                    format!("`{who}` shares what {call} {verb}")
                };
                let fn_owner = why.what == What::Task
                    && written
                        .is_some_and(|e| matches!(e.ty, HirTy::Fn(..) | HirTy::Closure { .. }));
                if fn_owner {
                    return err(
                        Code::USE_AFTER_MOVE,
                        format!(
                            "{head} at {} (inside `{}`, {inner}): it may own values moved into it \
                             (`[move ..]`), so it can be handed over only once",
                            self.line(site, at),
                            why.func
                        ),
                    )
                    .with_secondary(site, format!("{verb} here"))
                    .with_note(
                        "help: make another closure for the second use (moving in a `.clone()` \
                         of what it needs), or, for a `fn` value that moves nothing in, write \
                         `+ Send + Sync` on its type"
                            .to_owned(),
                    );
                }
                err(Code::USE_AFTER_HANDOFF, format!(
                    "{head} at {} (inside `{}`, {inner})",
                    self.line(site, at),
                    why.func
                ))
                .with_secondary(site, format!("{verb} here"))
                .with_note(format!(
                    "help: {} now; pass a `.clone()` to keep a separate copy, or give `{who}` a new \
                     value before using it again",
                    match why.what {
                        What::Channel => "the receiver owns it".to_owned(),
                        What::Handoff => "the `Handoff` owns it".to_owned(),
                        What::Task | What::TaskArg => "the task owns it".to_owned(),
                        What::Closure => "the closure owns it".to_owned(),
                        What::Freeze => "the frozen collection holds it".to_owned(),
                        What::Mutex | What::RwLock => format!("it belongs to {}", why.what.noun()),
                    }
                ))
            }
            Gone::Guard(site, guard, Tie::Moved) if self.stream_view(guard) => {
                let gname = self.name(guard);
                let who = who.split('.').next().unwrap_or(who);
                let line = self.line(site, at);
                let moved = self.moved_at.get(&guard).copied();
                let d = NabiDiagnostic::error(
                    Code::STREAM_VIEW_ESCAPE,
                    at,
                    format!(
                        "`{who}` is a view of `{gname}` (made at {line}), used after `{gname}` \
                         was moved{}",
                        moved.map_or_else(String::new, |m| format!(" at {}", self.line(m, at)))
                    ),
                )
                .with_secondary(site, format!("a view of `{gname}` made here"));
                let d = match moved {
                    Some(m) => d.with_secondary(m, format!("`{gname}` moved here")),
                    None => d,
                };
                let made = self
                    .c
                    .src(site)
                    .filter(|t| t.len() <= 40 && !t.contains('\n') && owning_form(t).is_some())
                    .map_or_else(|| format!("{gname}.lines()"), str::to_owned);
                let own = owning_form(&made).unwrap_or_else(|| format!("{gname}.into_lines()"));
                d.with_note(format!(
                    "help: `{who}` reads through `{gname}`, so it ends where `{gname}` moves; \
                     finish with `{who}` before moving `{gname}`, or take the owning form \
                     `{own}` and move that instead"
                ))
            }
            Gone::Guard(site, guard, _) if self.stream_view(guard) => {
                let gname = self.name(guard);
                let who = who.split('.').next().unwrap_or(who);
                let line = self.line(site, at);
                self.view_error(
                    at,
                    guard,
                    site,
                    format!(
                        "`{who}` is a view of `{gname}` (made at {line}), used after `{gname}` \
                         was closed"
                    ),
                )
            }
            Gone::Guard(site, guard, tie_kind) => {
                let gname = self.name(guard);
                let what = self.guard_what(guard);
                let ty = if what == What::RwLock {
                    "RwLock"
                } else {
                    "Mutex"
                };
                let line = self.line(site, at);
                let tie = chk
                    .src(site)
                    .filter(|t| t.len() <= 24 && !t.contains('\n'))
                    .map(str::to_owned);
                if tie_kind == Tie::Given {
                    err(
                        Code::USE_AFTER_HANDOFF,
                        format!("`{who}` was given to the `{ty}` at {line}"),
                    )
                    .with_secondary(site, format!("`{who}` given to the lock here"))
                    .with_note(format!(
                        "help: `{who}` belongs to the `{ty}` now; use it through the lock \
                             (`{gname}.get()` inside a `using` block), or {keep}"
                    ))
                } else if self.returning {
                    let copy = tie.map_or_else(
                        || "a `.clone()` of it".to_owned(),
                        |t| format!("`{t}.clone()`"),
                    );
                    err(
                        Code::USE_AFTER_UNLOCK,
                        format!(
                            "this returns a value out of the `{ty}` after `{gname}`'s `using` \
                         block released the lock"
                        ),
                    )
                    .with_secondary(site, "taken out of the lock here")
                    .with_note(format!(
                        "help: the lock no longer protects it; return {copy}, or do the work \
                         inside the `using` block"
                    ))
                } else if tie_kind == Tie::Shared {
                    let from = tie
                        .as_ref()
                        .map_or_else(String::new, |t| format!(" (`{t}`)"));
                    err(
                        Code::USE_AFTER_UNLOCK,
                        format!(
                            "`{who}` shares the value the `{ty}` protects{from} at {line}, and \
                         `{gname}`'s `using` block released the lock"
                        ),
                    )
                    .with_secondary(site, "the lock's value, taken out here")
                    .with_note(format!(
                        "help: the lock no longer protects `{who}`; use it inside the `using` \
                         block, or store a copy: `.clone()` what you take out of `{}`",
                        tie.unwrap_or_else(|| format!("{gname}.get()"))
                    ))
                } else {
                    let copy = format!("keep a deep copy there with `{gname}.get_clone()`");
                    err(Code::USE_AFTER_UNLOCK, format!(
                        "`{who}` came out of the `{ty}` at {line}, and `{gname}`'s `using` block \
                         released the lock"
                    ))
                    .with_secondary(site, format!("`{who}` taken out of the lock here"))
                    .with_note(format!(
                        "help: the lock no longer protects `{who}`; use it inside the `using` \
                         block, or {copy}"
                    ))
                }
            }
        }
    }

    /// Local `l` is a lock guard.
    pub(crate) fn is_guard(&self, l: LocalId) -> bool {
        let structs = self.c.env.structs();
        match self.local_ty(l) {
            Some(HirTy::Struct(id)) => structs
                .get(id.0 as usize)
                .is_some_and(|s| GUARDS.iter().any(|(g, _)| s.name.starts_with(g))),
            _ => false,
        }
    }

    pub(crate) fn guard_what(&self, g: LocalId) -> What {
        let structs = self.c.env.structs();
        let ty = self.local_ty(g);
        let name = match &ty {
            Some(HirTy::Struct(id)) => structs.get(id.0 as usize).map(|s| s.name.clone()),
            _ => None,
        };
        drop(structs);
        name.and_then(|n| {
            GUARDS
                .iter()
                .find(|(p, _)| n.starts_with(p))
                .map(|(_, w)| *w)
        })
        .unwrap_or(What::Mutex)
    }
}
