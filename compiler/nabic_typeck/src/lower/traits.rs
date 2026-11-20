//! Trait implementation queries, auto traits and generic impls.

#[allow(clippy::wildcard_imports)]
use crate::lower::*;

impl<'a> LowerCtx<'a> {
    /// The item bound to lang slot `slot`, `""` if unbound.
    pub fn lang(&self, slot: &str) -> &'a str {
        self.env.names.lang_item(slot)
    }

    /// The trait refs of `ty`'s concrete impls.
    pub(crate) fn concrete_impls(
        &self,
        ty: &HirTy,
    ) -> impl Iterator<Item = &'a TraitRef> + use<'a> {
        let env: &'a Env<'a> = self.env;
        env.impls.refs_on(ty)
    }

    /// Does `ty` implement trait `path` (whatever its arguments)?
    pub(crate) fn has_trait(&self, ty: &HirTy, path: &str) -> bool {
        self.implements_trait(ty, &TraitRef::plain(path), 0)
    }

    /// Does `ty` meet `want`: an impl covers it (`has_impl`) or a
    /// structural rule does (auto traits, sum members, `any` bounds,
    /// `Reflect`).
    pub fn implements_trait(&self, ty: &HirTy, want: &TraitRef, depth: u32) -> bool {
        // Linked literal variables compare as their roots.
        let open = |t: &HirTy| infer::has_var(self, t);
        if infer::active(self)
            && (open(ty)
                || want
                    .args
                    .iter()
                    .chain(want.binds.iter().map(|(_, t)| t))
                    .any(open))
        {
            let want = TraitRef {
                path: want.path.clone(),
                args: want.args.iter().map(|t| infer::resolve(self, t)).collect(),
                binds: want
                    .binds
                    .iter()
                    .map(|(n, t)| (n.clone(), infer::resolve(self, t)))
                    .collect(),
            };
            return self.implements_trait_here(&infer::resolve(self, ty), &want, depth);
        }
        self.implements_trait_here(ty, want, depth)
    }

    pub(crate) fn implements_trait_here(&self, ty: &HirTy, want: &TraitRef, depth: u32) -> bool {
        let trait_key = want.path.as_str();
        if trait_key == reflect::REFLECT {
            return true;
        }
        if self.env.names.auto_traits.contains(trait_key) {
            return self.auto_trait(ty, trait_key, depth, &mut AutoSeen::default());
        }
        // A sum has the structural traits its variants have (`none`
        // has them all); `expr::sum_structural_call` dispatches them.
        match ty {
            HirTy::Sum(vs) if want.is_plain() => {
                // `T | none` defaults to `none`.
                if trait_key == DEFAULT_TRAIT {
                    return vs.iter().any(HirTy::is_none);
                }
                // No order across two value terms.
                if expr::is_order_trait(self, trait_key) && expr::sum_value_terms(ty) > 1 {
                    return false;
                }
                if expr::is_sum_trait(self, trait_key) {
                    return vs
                        .iter()
                        .all(|v| v.is_none() || self.implements_trait(v, want, depth + 1));
                }
            }
            // An `any` value formats through its bound (`expr::any_fmt`).
            HirTy::AnyTrait(b)
                if want.is_plain()
                    && (trait_key == self.lang("display_trait")
                        || trait_key == self.lang("debug_trait")) =>
            {
                return bound_covers(self, b, want);
            }
            _ => {}
        }
        self.has_impl(ty, want, depth)
    }

    /// Structural `Send` / `Sync`: `unsafe impl` opts in, `impl !Tr` out,
    /// else every component must have it. A recursive type assumes it for
    /// itself (`seen`).
    pub(crate) fn auto_trait(
        &self,
        ty: &HirTy,
        key: &str,
        depth: u32,
        seen: &mut AutoSeen,
    ) -> bool {
        let memo = self.env.auto_memo.borrow().is_some();
        if let Some(&v) = self
            .env
            .auto_memo
            .borrow()
            .as_ref()
            .and_then(|t| t.get(key)?.get(ty))
        {
            return v;
        }
        let at = seen.open.len();
        let low = std::mem::replace(&mut seen.low, usize::MAX);
        let ok = self.auto_trait_inner(ty, key, depth, seen);
        // A `true` that assumed an enclosing type holds only inside it.
        let assumed = seen.low < at;
        seen.low = seen.low.min(low);
        if memo && (!ok || !assumed) {
            if let Some(t) = self.env.auto_memo.borrow_mut().as_mut() {
                t.entry(key.to_owned()).or_default().insert(ty.clone(), ok);
            }
        }
        ok
    }

    pub(crate) fn auto_trait_inner(
        &self,
        ty: &HirTy,
        key: &str,
        depth: u32,
        seen: &mut AutoSeen,
    ) -> bool {
        if self.has_impl(ty, &TraitRef::plain(key), depth) {
            return true;
        }
        let base = match ty {
            HirTy::Struct(id) => Some(self.struct_def(*id).name),
            HirTy::Enum(id) => Some(self.enum_def(*id).name),
            _ => None,
        };
        if let Some(b) = base {
            let b = b.split("__").next().unwrap_or(&b).to_owned();
            if self.env.opts_out(&b, key) {
                return false;
            }
        }
        if let Some(i) = seen.open.iter().position(|t| t == ty) {
            seen.low = seen.low.min(i);
            return true;
        }
        seen.open.push(ty.clone());
        let mut rec = |t: &HirTy| self.auto_trait(t, key, depth, seen);
        let ok = match ty {
            HirTy::Tuple(ts) | HirTy::Sum(ts) => ts.iter().all(&mut rec),
            HirTy::Array(e, _) | HirTy::GcBuf(e) => rec(e),
            // A closure crossing threads shares its plain captures with the
            // creator, so each must be `Sync` too; a `[move x]` capture is
            // the closure's alone; a `[ref x]` cell is never either.
            HirTy::Closure {
                env, shares, moved, ..
            } => {
                let sync = self.lang("sync_trait");
                let fields = match &**env {
                    HirTy::Tuple(ts) => ts.as_slice(),
                    t => std::slice::from_ref(t),
                };
                !shares
                    && fields.iter().enumerate().all(|(i, t)| {
                        rec(t)
                            && (key == sync
                                || (i < 64 && moved & (1 << i) != 0)
                                || self.auto_trait(t, sync, depth, &mut AutoSeen::default()))
                    })
            }
            HirTy::Throws { ok, err } => rec(ok) && rec(err),
            HirTy::Struct(id) => {
                let def = self.struct_def(*id);
                // A union has what every declared field has (its storage
                // is plain bytes).
                match &def.union_kind {
                    Some(u) => u.aliases.iter().all(|(_, t)| rec(t)),
                    None => def.fields.iter().all(|f| rec(&f.ty)),
                }
            }
            HirTy::Enum(id) => self
                .enum_def(*id)
                .variants
                .iter()
                .all(|v| v.payload_ty.as_ref().is_none_or(&mut rec)),
            // A `+ Send` closure may own a `[move x]` capture that is not
            // `Sync`: only a written `+ Sync` makes a value `Sync`.
            HirTy::Fn(_, _, b) => b.auto.iter().any(|k| k == key),
            // As for a `+ Send` closure: copies of an `any` share its box, so
            // crossing threads needs `Sync` too (no collection inside).
            HirTy::AnyTrait(b) => {
                bound_covers(self, b, &TraitRef::plain(key))
                    && (key != self.lang("send_trait")
                        || bound_covers(self, b, &TraitRef::plain(self.lang("sync_trait"))))
            }
            // Unresolved paths (`none`, type-param leftovers) and error
            // types count as having it: no cascade errors.
            HirTy::Prim(_)
            | HirTy::Unit
            | HirTy::Never
            | HirTy::Any
            | HirTy::Unknown
            | HirTy::Path(_)
            | HirTy::None
            | HirTy::Const(..)
            | HirTy::OpenConst(_)
            | HirTy::NoThrow
            | HirTy::Var(_) => true,
            // Only through its bounds (`has_impl`).
            HirTy::Param(_) => false,
        };
        seen.open.pop();
        ok
    }

    /// Why `ty` lacks `key`: the path to the component that opts out, to
    /// follow "`T` is not `Sync`".
    pub fn auto_trait_cause(&self, ty: &HirTy, key: &str, depth: u32) -> String {
        self.cause_on(ty, key, depth, &[])
    }

    /// `auto_trait_cause` below the types `path` already went through: a
    /// type that contains itself is said once, not followed round again.
    pub(crate) fn cause_on(&self, ty: &HirTy, key: &str, depth: u32, path: &[HirTy]) -> String {
        let tr = leaf_name(key);
        let lacks = |t: &HirTy| !self.auto_trait(t, key, 0, &mut AutoSeen::default());
        let mut below = path.to_vec();
        below.push(ty.clone());
        let step = |what: String, t: &HirTy| {
            let name = self.ty_name(t);
            if below.contains(t) {
                return format!(
                    " because {what} has type `{name}`, and so on: `{name}` contains itself"
                );
            }
            format!(
                " because {what} has type `{name}`{}",
                self.cause_on(t, key, depth + 1, &below)
            )
        };
        // Of two causes, the one that reaches the opting-out part beats one
        // that only comes round to a type already on the path.
        let not_round = |a: String, b: String| {
            if a.ends_with("contains itself") && !b.ends_with("contains itself") {
                b
            } else {
                a
            }
        };
        if depth > 8 {
            return String::new();
        }
        match ty {
            HirTy::Struct(id) => {
                let def = self.struct_def(*id);
                let base = def.name.split("__").next().unwrap_or(&def.name);
                // A frozen one lacks it only through its elements.
                if base.starts_with("core::collections::frozen::") {
                    return def
                        .generic_args
                        .iter()
                        .find(|t| lacks(t))
                        .map(|t| step("an element".to_owned(), t))
                        .unwrap_or_default();
                }
                if base.starts_with("core::collections::slice::") {
                    return format!(
                        " (a view shares its list's buffer, so it is not `{tr}`: give each \
                         worker a view of its own as a `par` item, such as \
                         `par::for_each(xs.chunks_mut(n), ..)`)"
                    );
                }
                if base.starts_with("core::collections::") {
                    return format!(
                        " (collections are not `{tr}`: share one read-only with `.freeze()`, \
                         or through a `Mutex` to change it)"
                    );
                }
                if base == "bare::gc::Weak" && key == self.lang("send_trait") {
                    return " (a hand-off cannot copy what a `Weak` names, so it stays with its \
                            thread: send the `GcPtr`)"
                        .to_owned();
                }
                if self.env.opts_out(base, key) {
                    return String::new();
                }
                if base == "core::sync::RwLock" {
                    if let Some(arg) = def.generic_args.first() {
                        let t = self.ty_name(arg);
                        // A collection of `Sync` elements freezes: readers
                        // share it, a writer swaps in a new one.
                        let frozen = match arg {
                            HirTy::Struct(c) => {
                                let cd = self.struct_def(*c);
                                let cb = cd.name.split("__").next().unwrap_or(&cd.name);
                                [
                                    "core::collections::list::List",
                                    "core::collections::dict::Dict",
                                    "core::collections::set::Set",
                                ]
                                .contains(&cb)
                                    && cd.generic_args.iter().all(|e| !lacks(e))
                            }
                            _ => false,
                        };
                        let swap = if frozen {
                            format!(
                                ", or keep a frozen one and replace it whole: `RwLock<Frozen{t}>`"
                            )
                        } else {
                            String::new()
                        };
                        return format!(
                            ": its readers could change the `{t}` inside at once; use \
                             `Mutex<{t}>` and `.lock()` in place of `.read()` / `.write()`{swap}"
                        );
                    }
                }
                def.fields
                    .iter()
                    .filter(|f| lacks(&f.ty))
                    .map(|f| step(format!("field `{}`", f.name), &f.ty))
                    .reduce(not_round)
                    .unwrap_or_default()
            }
            HirTy::Enum(id) => self
                .enum_def(*id)
                .variants
                .iter()
                .filter_map(|v| {
                    let t = v.payload_ty.as_ref().filter(|t| lacks(t))?;
                    Some(step(format!("variant `{}`", v.name), t))
                })
                .reduce(not_round)
                .unwrap_or_default(),
            HirTy::Tuple(ts) | HirTy::Sum(ts) => ts
                .iter()
                .filter(|t| lacks(t))
                .map(|t| step("a member".to_owned(), t))
                .reduce(not_round)
                .unwrap_or_default(),
            HirTy::Array(e, _) | HirTy::GcBuf(e) => step("an element".to_owned(), e),
            HirTy::AnyTrait(_) if key == self.lang("send_trait") => " (an `any` crosses \
                 threads only when its bound has both `Send` and `Sync`, since copies share \
                 its box: give the trait `Sync` as a supertrait, or send the concrete \
                 type)"
                .to_owned(),
            _ => String::new(),
        }
    }

    /// A closure value's first `[ref x]` capture: `x` and its type.
    pub(crate) fn ref_capture(&self, value: &HirExpr) -> Option<(String, HirTy)> {
        let HirExprKind::ClosureValue { env, .. } = &value.kind else {
            return None;
        };
        let HirExprKind::Tuple { fields } = &env.kind else {
            return None;
        };
        fields.iter().find_map(|f| {
            let (HirExprKind::Local(l), HirTy::GcBuf(x)) = (&f.kind, &f.ty) else {
                return None;
            };
            let local = self.locals.get(l.0 as usize).filter(|x| x.boxed)?;
            Some((local.name.clone()?, (**x).clone()))
        })
    }

    /// For a closure that does not fit a `fn .. + Send` slot: which
    /// capture is not `Send` / `Sync`, and why; with the capture's name
    /// and whether it is `Send` (only not `Sync`).
    pub(crate) fn closure_bound_note(&self, value: &HirExpr) -> Option<(String, String, bool)> {
        let HirExprKind::ClosureValue { env, .. } = &value.kind else {
            return None;
        };
        let HirExprKind::Tuple { fields } = &env.kind else {
            return None;
        };
        let keys = [self.lang("send_trait"), self.lang("sync_trait")];
        fields.iter().find_map(|f| {
            let local = match &f.kind {
                HirExprKind::Local(l) => *l,
                HirExprKind::BufGet { buf, .. } => match buf.kind {
                    HirExprKind::Local(l) => l,
                    _ => return None,
                },
                _ => return None,
            };
            let key = keys
                .iter()
                .find(|k| !self.auto_trait(&f.ty, k, 0, &mut AutoSeen::default()))?;
            let name = self.locals.get(local.0 as usize)?.name.clone()?;
            let ty = self.ty_name(&f.ty);
            let guard = ["MutexGuard<", "RwLockReadGuard<", "RwLockWriteGuard<"]
                .iter()
                .any(|g| ty.starts_with(g));
            let read = if guard {
                format!(
                    "; a guard stays with its `using` block: read it first (`let v = \
                     {name}.get();`) and capture `v`"
                )
            } else {
                String::new()
            };
            let note = format!(
                "the closure captures `{name}` (type `{ty}`), which is not `{}`{}{read}",
                leaf_name(key),
                self.auto_trait_cause(&f.ty, key, 0)
            );
            Some((note, name, *key != keys[0] && !guard))
        })
    }

    /// What to write for a closure capture `name` of type `ty` that is
    /// not `Sync`, when it is a collection: freeze it, or split it.
    pub(crate) fn capture_help(&self, value: &HirExpr) -> Option<String> {
        let HirExprKind::ClosureValue { env, .. } = &value.kind else {
            return None;
        };
        let HirExprKind::Tuple { fields } = &env.kind else {
            return None;
        };
        let sync = self.lang("sync_trait");
        fields.iter().find_map(|f| {
            let HirExprKind::Local(l) = &f.kind else {
                return None;
            };
            let HirTy::Struct(sid) = &f.ty else {
                return None;
            };
            let def = self.struct_def(*sid);
            let base = def.name.split("__").next().unwrap_or(&def.name);
            let list = base == "core::collections::list::List";
            if !list
                && base != "core::collections::dict::Dict"
                && base != "core::collections::set::Set"
            {
                return None;
            }
            let name = self.locals.get(l.0 as usize)?.name.clone()?;
            let frozen = def
                .generic_args
                .iter()
                .all(|t| self.auto_trait(t, sync, 0, &mut AutoSeen::default()));
            let split = if list {
                format!(
                    "; to write parts of it in parallel, give each worker its own part as an \
                     item: `par::for_each({name}.chunks_mut(n), |part| ..)`"
                )
            } else {
                String::new()
            };
            Some(if frozen {
                format!(
                    "help: share `{name}` read-only, with no copy: `let {name} = {name}.freeze();` \
                     before the closure{split}"
                )
            } else {
                format!(
                    "help: put `{name}` in a `Mutex` and lock it inside the closure (its elements \
                     can change, so it cannot be frozen){split}"
                )
            })
        })
    }

    /// Does an impl give `ty` trait `want`: a concrete one, or a
    /// covering generic one (`generic_impls`)? All impl questions go
    /// through here, `concrete_impls` and `generic_impls`.
    pub(crate) fn has_impl(&self, ty: &HirTy, want: &TraitRef, depth: u32) -> bool {
        if let HirTy::Param(p) = ty {
            if bounds::has(self, *p, want) {
                return true;
            }
        }
        if bounds::assumed(self.env, ty, want) {
            return true;
        }
        self.env
            .impls
            .refs_of(ty, &want.path)
            .any(|r| r.satisfies(want))
            || self
                .generic_impls(ty, Pick::Trait(&want.path), depth)
                .any(|(gi, subst)| {
                    want.is_plain()
                        || self
                            .generic_impl_ref_for(gi, &subst, Some(want))
                            .satisfies(want)
                })
            || self.unify_impl(ty, want, depth)
    }

    /// During an inference, `List<i32>: FromIterator<#var>`: the one impl
    /// of the trait for `ty` fixes the open arguments.
    pub(crate) fn unify_impl(&self, ty: &HirTy, want: &TraitRef, depth: u32) -> bool {
        // An open literal with the trait on one number type of its kind.
        if matches!(ty, HirTy::Var(_)) {
            let works = |t: &HirTy| self.implements_trait(t, want, depth + 1);
            // Its default has it: the variable stays open.
            if infer::literal_default(self, ty).is_some_and(|d| infer::trial(self, &|| works(&d)))
                || infer::pick_literal_type(self, ty, &works).is_some()
            {
                return true;
            }
        }
        let open = |t: &HirTy| infer::has_var(self, t);
        if !infer::active(self)
            || !(open(ty) || want.args.iter().any(open) || want.binds.iter().any(|(_, t)| open(t)))
        {
            return false;
        }
        // `Box<#var>: Width` with `impl Width for Box<i32>`: the one choice
        // of number types with the impl.
        let mut lits = Vec::new();
        infer::open_literals_in(self, ty, &mut lits);
        let open_want = want.args.iter().any(open) || want.binds.iter().any(|(_, t)| open(t));
        if !lits.is_empty() && !matches!(ty, HirTy::Var(_)) && !open_want {
            let works = |t: &HirTy| self.implements_trait(t, want, depth + 1);
            if infer::trial(self, &|| works(&infer::zonk(self, ty))) {
                return true;
            }
            infer::pick_literals_or(self, ty, &works, &|_| false, false);
            let solved = infer::resolve(self, ty);
            if solved != *ty && !infer::has_var(self, &solved) {
                return works(&solved);
            }
        }
        let mut cands: Vec<TraitRef> = self.env.impls.refs_of(ty, &want.path).cloned().collect();
        cands.extend(
            self.generic_impls(ty, Pick::Trait(&want.path), depth)
                .map(|(gi, subst)| self.generic_impl_ref(gi, &subst)),
        );
        // `ListIter<#var>: Iterator<Item = i32>`: the impl's `Item`
        // meets the wanted one. Of several impls (`Add<i32>`, `Add<str>`
        // for `M + 2`), the one the open arguments can take.
        let fits = |one: &TraitRef| {
            one.args.len() == want.args.len()
                && one
                    .args
                    .iter()
                    .zip(&want.args)
                    .all(|(a, b)| infer::unify(self, a, b))
                && want.binds.iter().all(|(n, b)| {
                    one.binds
                        .iter()
                        .find(|(m, _)| m == n)
                        .is_none_or(|(_, a)| infer::unify(self, a, b))
                })
        };
        let mut open = cands.iter().filter(|c| infer::trial(self, &|| fits(c)));
        match (open.next(), open.next()) {
            (Some(one), None) => fits(one),
            _ => false,
        }
    }

    /// The trait arguments of `ty`'s impl of trait `key` (a bound's,
    /// for a parameter), the first found.
    pub(crate) fn impl_trait_args(&self, ty: &HirTy, key: &str) -> Option<Vec<HirTy>> {
        if let HirTy::Param(p) = ty {
            return bounds::of_trait(self, *p, key)
                .into_iter()
                .next()
                .map(|r| r.args);
        }
        if let Some(r) = self.env.impls.refs_of(ty, key).next() {
            return Some(r.args.clone());
        }
        let (gi, subst) = self.generic_impls(ty, Pick::Trait(key), 0).next()?;
        Some(self.generic_impl_ref(gi, &subst).args)
    }

    /// Generic impls `keep` picks (`trait_key` `""`: inherent) covering
    /// `ty`, with their bindings.
    pub(crate) fn generic_impls<'s>(
        &'s self,
        ty: &'s HirTy,
        pick: Pick<'s>,
        depth: u32,
    ) -> impl Iterator<Item = (&'a Impl, TySubst)> + 's {
        let env: &'a Env<'a> = self.env;
        env.impls
            .generic_by(pick)
            .filter_map(move |gi| Some((gi, self.generic_impl_subst(gi, ty, depth)?)))
    }

    /// The trait ref generic impl `gi` provides under `subst`: its trait
    /// arguments and associated types instantiated.
    pub(crate) fn generic_impl_ref(&self, gi: &Impl, subst: &TySubst) -> TraitRef {
        self.generic_impl_ref_for(gi, subst, None)
    }

    /// `generic_impl_ref`, the parameters only the trait arguments name
    /// (`impl<const L: i32> Div<G<L>> for f64`) bound from `want`'s
    /// arguments. A type naming a parameter still unbound is `Unknown`.
    pub(crate) fn generic_impl_ref_for(
        &self,
        gi: &Impl,
        subst: &TySubst,
        want: Option<&TraitRef>,
    ) -> TraitRef {
        let names: nabic_core::HashSet<String> = gi
            .generics
            .params
            .iter()
            .map(|gp| match gp {
                nabic_ast::GenericParam::Type { name, .. }
                | nabic_ast::GenericParam::Const { name, .. } => name.clone(),
            })
            .collect();
        let mut subst = subst.clone();
        for (a, t) in gi.trait_args.iter().zip(want.map_or(&[][..], |w| &w.args)) {
            let t = infer::resolve(self, t);
            if !infer::has_var(self, &t) {
                unify_param_ty_with(a, &t, &names, &mut subst, self);
            }
        }
        for gp in &gi.generics.params {
            if let nabic_ast::GenericParam::Const { name, ty: cty } = gp {
                if let (Some((v, _)), HirTy::Prim(p)) = (
                    subst.get(name).and_then(HirTy::as_const),
                    crate::ty::resolve_ty(cty),
                ) {
                    subst.insert(name.clone(), HirTy::Const(v, Some(p)));
                }
            }
        }
        let open = |t: &Ty| {
            names
                .iter()
                .any(|n| !subst.contains_key(n) && crate::lower::expr::ast_ty_mentions(t, n))
        };
        let mut rcx = LowerCtx::new(self.env, &subst, None, self.here());
        rcx.current_module.clone_from(&gi.module);
        rcx.set_bounds(&gi.generics, &gi.where_clause);
        let mut res = |t: &Ty| {
            if open(t) {
                HirTy::Unknown
            } else {
                rcx.resolve_ast_ty(t)
            }
        };
        let mut args: Vec<HirTy> = gi.trait_args.iter().map(&mut res).collect();
        let binds = gi.assoc.iter().map(|(n, t)| (n.clone(), res(t))).collect();
        // Omitted trait arguments take the parameter's default, `Self` the
        // impl's target (`impl<T> Add for W<T>` is `Add<W<T>>`).
        let defaults = self.env.trait_of(&gi.trait_key).map(|t| &t.generics);
        if let Some(g) = defaults.filter(|g| g.params.len() > args.len()) {
            if !open(&gi.self_ty) {
                rcx.self_ty = Some(rcx.resolve_ast_ty(&gi.self_ty));
                for gp in g.params.iter().skip(args.len()) {
                    let nabic_ast::GenericParam::Type {
                        default: Some(d), ..
                    } = gp
                    else {
                        break;
                    };
                    args.push(rcx.resolve_ast_ty(d));
                }
            }
        }
        TraitRef::new(gi.trait_key.clone(), args, binds)
    }

    /// Bound `b` (an AST trait path) as written in `module`, its type
    /// arguments resolved by `rec`.
    pub fn bound_ref(
        &self,
        b: &Ty,
        module: &str,
        rec: impl FnMut(&Ty) -> HirTy,
    ) -> Option<TraitRef> {
        let AstTyKind::Path(p) = &b.kind else {
            return None;
        };
        Some(resolve::trait_ref(
            &self.env.names,
            p,
            module,
            self.here(),
            rec,
        ))
    }

    /// The impl's type-parameter bindings when generic impl `gi` covers
    /// `ty`: `ty` matches its target and meets its bounds.
    pub(crate) fn generic_impl_subst(
        &self,
        gi: &Impl,
        ty: &HirTy,
        depth: u32,
    ) -> Option<HashMap<String, HirTy>> {
        self.generic_impl_match(gi, ty, depth).ok()
    }

    /// `generic_impl_subst` with a reason: `Err(Some((T, B)))` when the
    /// target matches but `T: B` fails, `Err(None)` when it doesn't match.
    pub(crate) fn generic_impl_match(
        &self,
        gi: &Impl,
        ty: &HirTy,
        depth: u32,
    ) -> Result<HashMap<String, HirTy>, Option<(HirTy, TraitRef)>> {
        // Depth cap instead of cycle detection (`impl<T: Tr> Tr for T`).
        if depth > 8 || matches!(ty, HirTy::Unknown) {
            return Err(None);
        }
        let params: nabic_core::HashSet<String> = gi
            .generics
            .params
            .iter()
            .map(|gp| match gp {
                nabic_ast::GenericParam::Type { name, .. }
                | nabic_ast::GenericParam::Const { name, .. } => name.clone(),
            })
            .collect();
        let mut subst = HashMap::default();
        unify_param_ty_with(&gi.self_ty, ty, &params, &mut subst, self);
        // Parameters only the trait arguments name are left to the call.
        let shown = collect::impl_def_display(&gi.self_ty);
        let mut in_self = shown
            .split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .filter(|w| params.contains(*w));
        if in_self.any(|p| !subst.contains_key(p)) {
            return Err(None);
        }
        // `impl<T, const N: usize> Tr for [T; N]`: `N` takes its type.
        for gp in &gi.generics.params {
            if let nabic_ast::GenericParam::Const { name, ty: cty } = gp {
                let prim = match crate::ty::resolve_ty(cty) {
                    HirTy::Prim(p) => Some(p),
                    _ => None,
                };
                if let Some((v, _)) = subst.get(name).and_then(HirTy::as_const) {
                    // A solved header (`W<T, { N + 1 }>` against `W<T, 0>`)
                    // only applies with a value of the parameter's type.
                    if prim.is_some_and(|p| !const_fits(v, p)) {
                        return Err(None);
                    }
                    subst.insert(name.clone(), HirTy::Const(v, prim));
                }
            }
        }
        let mut rcx = LowerCtx::new(self.env, &subst, None, self.here());
        rcx.current_module.clone_from(&gi.module);
        rcx.set_bounds(&gi.generics, &gi.where_clause);
        // Another generic struct's arguments (`List<i32>` against `Rev<I>`,
        // `T<N>` against `GcBuf<f64>`) would instantiate a bogus struct.
        if let AstTyKind::Path(p) = &gi.self_ty.kind {
            let name = match ty {
                HirTy::Struct(id) => Some(self.struct_def(*id).name),
                HirTy::Enum(id) => Some(self.enum_def(*id).name),
                HirTy::GcBuf(_) => Some(resolve::GC_BUF.to_owned()),
                _ => None,
            };
            if let Some(name) =
                name.filter(|_| p.segments.last().is_some_and(|s| !s.generics.is_empty()))
            {
                let segs: Vec<&str> = p.segments.iter().map(|s| s.name.as_str()).collect();
                if rcx.canon_type(&segs).as_deref() != Some(enum_base_name(&name)) {
                    return Err(None);
                }
            }
        }
        let target = rcx.resolve_ast_ty(&gi.self_ty);
        // `impl<const N: usize> Tr for fn(L<N>) -> f64` takes a `fn` value
        // whatever bounds its type carries (`+ Send + Sync`).
        let fits = match (&target, ty) {
            (HirTy::Fn(a, r, _), HirTy::Fn(b, q, _)) => a == b && r == q,
            _ => target == *ty,
        };
        if !fits {
            return Err(None);
        }
        let mut goals: Vec<(HirTy, &Vec<Ty>)> = Vec::new();
        for gp in &gi.generics.params {
            if let nabic_ast::GenericParam::Type { name, bounds, .. } = gp {
                if let Some(t) = subst.get(name) {
                    goals.push((t.clone(), bounds));
                }
            }
        }
        for w in &gi.where_clause {
            goals.push((rcx.resolve_ast_ty(&w.ty), &w.bounds));
        }
        for (bty, bounds) in goals {
            // An open variable is checked once solved; an open literal
            // takes the one number type that meets the bound.
            let bty = infer::resolve(self, &bty);
            if infer::unsolved(self, &bty) {
                continue;
            }
            for b in bounds {
                let Some(want) = self.bound_ref(b, &gi.module, |t| rcx.resolve_ast_ty(t)) else {
                    continue;
                };
                if !self.implements_trait(&bty, &want, depth + 1) {
                    return Err(Some((bty, want)));
                }
            }
        }
        Ok(subst)
    }

    /// `any` tag of `ty`: a 31-bit hash of its name, so separately compiled
    /// packages agree. A collision probes to the next free tag, which is
    /// then stable only within this build.
    pub(crate) fn any_tag(&self, ty: &HirTy) -> u32 {
        let mut tags = self.env.sink.any_tags.borrow_mut();
        let found = tags.iter().find(|(t, _)| t == ty).map(|(_, tag)| *tag);
        incr::log(
            || format!("a|{ty:?}"),
            || incr::Lookup::AnyTag(ty.clone(), found),
        );
        if let Some(tag) = found {
            return tag;
        }
        let mut tag = self.ty_name(ty).bytes().fold(0x811c_9dc5_u32, |h, b| {
            (h ^ u32::from(b)).wrapping_mul(0x0100_0193)
        }) & 0x7fff_ffff;
        while tags.iter().any(|(_, t)| *t == tag) {
            tag = (tag + 1) & 0x7fff_ffff;
        }
        tags.push((ty.clone(), tag));
        tag
    }
}

/// `v` is a value of integer type `p` (other types: any).
fn const_fits(v: i128, p: PrimTy) -> bool {
    let (lo, hi): (i128, i128) = match p {
        PrimTy::I8 => (i8::MIN.into(), i8::MAX.into()),
        PrimTy::I16 => (i16::MIN.into(), i16::MAX.into()),
        PrimTy::I32 => (i32::MIN.into(), i32::MAX.into()),
        PrimTy::I64 | PrimTy::Isize => (i64::MIN.into(), i64::MAX.into()),
        PrimTy::U8 => (0, u8::MAX.into()),
        PrimTy::U16 => (0, u16::MAX.into()),
        PrimTy::U32 => (0, u32::MAX.into()),
        PrimTy::U64 | PrimTy::Usize => (0, u64::MAX.into()),
        _ => return true,
    };
    (lo..=hi).contains(&v)
}
