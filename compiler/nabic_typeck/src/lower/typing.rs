//! Expected types, type resolution, consts and projections.

#[allow(clippy::wildcard_imports)]
use crate::lower::*;

impl LowerCtx<'_> {
    pub(crate) fn push_expected(&mut self, ty: HirTy) {
        self.expected_ty_stack.push(ty);
    }
    pub(crate) fn pop_expected(&mut self) {
        self.expected_ty_stack.pop();
    }
    pub fn current_expected(&self) -> Option<&HirTy> {
        self.expected_ty_stack.last()
    }

    /// AST type to `HirTy` under the substitution, `Self` and aliases;
    /// generic structs with concrete arguments are instantiated on demand.
    pub fn resolve_ast_ty(&self, ty: &nabic_ast::Ty) -> HirTy {
        let mut t = self.resolve_ty_inner(ty);
        if let Some(st) = &self.self_ty {
            substitute_params(
                &mut t,
                &HashMap::from_iter([("Self".to_owned(), st.clone())]),
            );
        }
        t
    }

    /// A compile-time integer (literal, const parameter, or const
    /// expression via `consteval`).
    pub(crate) fn eval_const(&self, c: &nabic_ast::ConstExpr) -> Option<i128> {
        self.eval_const_msg(c).ok()
    }

    pub(crate) fn eval_const_msg(&self, c: &nabic_ast::ConstExpr) -> Result<i128, String> {
        self.eval_const_arg(c).map(|(v, _)| v)
    }

    /// A const argument's value, tagged `char` when it is one (a `char`
    /// const parameter takes only a `char`).
    pub(crate) fn eval_const_arg(
        &self,
        c: &nabic_ast::ConstExpr,
    ) -> Result<(i128, Option<PrimTy>), String> {
        let not_int = || "not an integer".to_owned();
        let e = match c {
            nabic_ast::ConstExpr::Literal(nabic_ast::Literal::Char(s)) => {
                return expr::parse_char_literal(s)
                    .map(|v| (i128::from(v), Some(PrimTy::Char)))
                    .ok_or_else(not_int);
            }
            nabic_ast::ConstExpr::Literal(nabic_ast::Literal::Int(s)) => {
                return expr::parse_int_literal(s)
                    .map(|(v, _)| (v, None))
                    .ok_or_else(not_int);
            }
            nabic_ast::ConstExpr::Literal(nabic_ast::Literal::Bool(b)) => {
                return Ok((i128::from(*b), Some(PrimTy::Bool)));
            }
            nabic_ast::ConstExpr::Literal(_) => return Err(not_int()),
            nabic_ast::ConstExpr::Path(p) => {
                if let [seg] = p.segments.as_slice() {
                    if let Some((v, p)) = self.ty_subst.get(&seg.name).and_then(HirTy::as_const) {
                        return Ok((v, p.filter(|p| matches!(p, PrimTy::Char | PrimTy::Bool))));
                    }
                }
                Expr {
                    kind: ExprKind::Path(p.clone()),
                    span: p.span,
                }
            }
            nabic_ast::ConstExpr::Expr(e) => {
                const_arg_shape(e)?;
                (**e).clone()
            }
        };
        consteval::eval_scalar(self, &e)
    }

    /// An array length; `None` if unknown (reported unless it waits for the
    /// second signature round or an unbound const parameter).
    pub(crate) fn array_len(&self, len: &nabic_ast::ConstExpr) -> Option<u32> {
        // No body lowers before signatures are known; a hard length waits
        // for the second signature round.
        if let Some(pending) = self.env.sink.sig_pass.get() {
            let param = |name: &str| {
                self.ty_subst
                    .get(name)
                    .and_then(HirTy::as_const)
                    .map(|c| c.0)
            };
            let names = &self.env.names;
            if let Some(n) = names.const_int(len, &self.current_module, &param) {
                if let Ok(n) = u32::try_from(n) {
                    return Some(n);
                }
            }
            if !pending {
                self.env.sink.sig_pass.set(Some(true));
            }
            return None;
        }
        // `[T; N]`, `[T; M * N]`, `[T; f(N)]`.
        let n = self
            .eval_const_msg(len)
            .and_then(|v| u32::try_from(v).map_err(|_| format!("length {v} is out of range")));
        match n {
            Ok(n) => Some(n),
            // A const parameter not bound yet (argument hints
            // before inference): no type yet, not an error.
            Err(_)
                if self
                    .ty_subst
                    .values()
                    .any(|t| matches!(t, HirTy::OpenConst(_))) =>
            {
                None
            }
            Err(why) => {
                self.emit_error(
                    Code::CONST_EVAL,
                    format!(
                        "array length must be a compile-time integer: \
                             {why}"
                    ),
                );
                if let nabic_ast::ConstExpr::Expr(e) = len {
                    self.help_last(consteval::layout_help(self, e));
                }
                None
            }
        }
    }

    pub(crate) fn resolve_ty_inner(&self, ty: &nabic_ast::Ty) -> HirTy {
        // A type path names what the module it comes from sees;
        // its arguments carry their own origins.
        let origin = match &ty.kind {
            AstTyKind::Path(p) => p.origin.as_deref(),
            _ => None,
        };
        let outer = self.set_access(origin);
        let t = self.resolve_ty_here(ty);
        self.access.replace(outer);
        t
    }

    /// E0081 for a const generic argument that did not evaluate.
    fn bad_const_arg(&self, why: &str) {
        let panicked = why.strip_prefix("panicked: ");
        let msg = match panicked {
            Some(m) => format!("evaluating the const generic argument panicked: {m}"),
            None => format!(
                "a const generic argument must be a compile-time integer, `bool` or `char`: {why}"
            ),
        };
        self.emit_error(Code::INVALID_CONST_ARGUMENT, msg);
        if panicked.is_some() {
            self.help_last(Some(
                "the argument runs at compile time: pass a value its `const fn` accepts, or a \
                 literal"
                    .to_owned(),
            ));
        }
    }

    pub(crate) fn resolve_ty_here(&self, ty: &nabic_ast::Ty) -> HirTy {
        let rec = |t: &nabic_ast::Ty| self.resolve_ty_inner(t);
        let module = self.access_module();
        let module = &*module;
        let scope = Scope {
            names: &self.env.names,
            module,
            at: self.here(),
        };
        match &ty.kind {
            // `Self` inside generic arguments (`Map<Self, ..>`) must be
            // concrete before the instantiation check.
            AstTyKind::SelfUpper if self.self_ty.is_some() => {
                self.self_ty.clone().unwrap_or(HirTy::Unknown)
            }
            AstTyKind::Path(_)
                if self
                    .env
                    .names
                    .expand_alias(ty, module, self.here())
                    .is_some() =>
            {
                let t = self.env.names.expand_alias(ty, module, self.here());
                let h = t.map_or(HirTy::Unknown, |t| rec(&t));
                self.note_alias(ty, &h, module);
                h
            }
            AstTyKind::Path(p) => {
                // Depth-capped: a cyclic alias is reported by the alias pass.
                let param =
                    matches!(p.segments.as_slice(), [s] if self.ty_subst.contains_key(&s.name));
                let names = &self.env.names;
                if !param && !names.some_types.is_empty() {
                    if let Some(x) = self.some_type_name(p, module) {
                        return mono::some_type_ty(self, &x);
                    }
                }
                let alias = (!param && names.alias_depth.get() < 32)
                    .then(|| names.plain_alias(ty, module, self.here()))
                    .flatten();
                if let Some(full) = alias {
                    // Reported at the alias.
                    if names.broken_aliases.borrow().contains(&full) {
                        return HirTy::Unknown;
                    }
                    if let Some(t) = names.projection_alias(&full, module, self.here()) {
                        let depth = &names.alias_depth;
                        depth.set(depth.get() + 1);
                        let r = rec(&t);
                        depth.set(depth.get() - 1);
                        return r;
                    }
                }
                if let [seg] = p.segments.as_slice() {
                    if seg.generics.is_empty() {
                        if let Some(t) = self.ty_subst.get(&seg.name) {
                            return t.clone();
                        }
                        // `X` in a trait / impl body is `Self::X`.
                        if self.self_ty.is_some() && self.assoc_names.contains(&seg.name) {
                            let mut sp = p.clone();
                            sp.segments.insert(
                                0,
                                nabic_ast::PathSegment {
                                    name: "Self".to_owned(),
                                    generics: Vec::new(),
                                },
                            );
                            if let Some(t) = self.projection(&sp) {
                                return t;
                            }
                        }
                        // A `const` item as a const generic argument (`Grid<W, 3>`).
                        if self.canon_type(&[seg.name.as_str()]).is_none()
                            && self.resolve_in(&[seg.name.as_str()], false).is_some()
                        {
                            if let Ok((v, t)) =
                                self.eval_const_arg(&nabic_ast::ConstExpr::Path(p.clone()))
                            {
                                return HirTy::Const(v, t);
                            }
                        }
                    }
                }
                if let Some(t) = self.projection(p) {
                    return t;
                }
                let generics = p.segments.last().map_or(&[][..], |s| s.generics.as_slice());
                let bare = generics.is_empty()
                    && p.segments
                        .last()
                        .is_some_and(|s| self.env.defs.defaulted(&s.name));
                if !generics.is_empty() || bare {
                    let segs: Vec<&str> = p.segments.iter().map(|s| s.name.as_str()).collect();
                    if let Some(full) = self
                        .canon_type(&segs)
                        .filter(|f| !bare || self.env.defs.bare_instance(f))
                    {
                        if let (GC_BUF, [elem]) = (full.as_str(), generics) {
                            return HirTy::GcBuf(Box::new(rec(elem)));
                        }
                        let args: Vec<HirTy> = generics.iter().map(rec).collect();
                        // A new instance's bound errors point at this type.
                        let at = Some(ty.span)
                            .filter(|s| !s.is_dummy())
                            .unwrap_or_else(|| self.here());
                        let mut leftover = Vec::new();
                        args.iter()
                            .for_each(|a| collect_unknown_paths(a, &mut leftover));
                        // An inference variable is an argument like any type.
                        leftover.retain(|p| p != "_");
                        let mark = self.env.sink.diagnostics.borrow().len();
                        if leftover.is_empty() && self.env.defs.is_generic_struct(&full) {
                            if let Some(id) = self.instantiate_struct(&full, args, at) {
                                return HirTy::Struct(id);
                            }
                        } else if leftover.is_empty() {
                            if let Some(id) = self.instantiate_enum(&full, args, at) {
                                return HirTy::Enum(id);
                            }
                        } else {
                            // Names an unbound type parameter: resolving it
                            // by name could hit a same-named type.
                            self.env
                                .names
                                .open_args
                                .borrow_mut()
                                .entry(full.clone())
                                .or_insert_with(|| leftover[0].clone());
                            return HirTy::Path(vec![full]);
                        }
                        // Reported (an out-of-range const argument): not also an unknown type.
                        if self.env.sink.diagnostics.borrow().len() > mark {
                            self.env
                                .names
                                .open_args
                                .borrow_mut()
                                .insert(full, String::new());
                        }
                    }
                }
                let mut t = resolve_ty_with_structs(ty, &self.env.defs, None, scope);
                substitute_params(&mut t, &self.ty_subst);
                t
            }
            AstTyKind::Sum(parts) => crate::ty::written_sum(
                parts,
                |n| self.ty_subst.contains_key(n) || self.assoc_names.iter().any(|a| a == n),
                rec,
            ),
            AstTyKind::Nest(x) => rec(x),
            AstTyKind::Tuple(parts) if parts.is_empty() => HirTy::Unit,
            AstTyKind::Tuple(parts) => HirTy::Tuple(parts.iter().map(rec).collect()),
            AstTyKind::Array(elem, len) => {
                if let nabic_ast::ConstExpr::Literal(_) = len {
                    return match resolve_ty_with_structs(ty, &self.env.defs, None, scope) {
                        HirTy::Array(_, n) => HirTy::Array(Box::new(rec(elem)), n),
                        other => other,
                    };
                }
                self.array_len(len)
                    .map_or(HirTy::Unknown, |n| HirTy::Array(Box::new(rec(elem)), n))
            }
            AstTyKind::Const(c) => match self.eval_const_arg(c) {
                Ok((v, p)) => HirTy::Const(v, p),
                Err(why) => {
                    self.bad_const_arg(&why);
                    HirTy::Unknown
                }
            },
            AstTyKind::Fn(args, ret, b) => HirTy::Fn(
                args.iter().map(rec).collect(),
                Box::new(rec(ret)),
                self.env.names.fn_bounds(b, module, self.here()),
            ),
            AstTyKind::Throws { ok, err } => crate::ty::throws_ty(rec(ok), rec(err)),
            AstTyKind::QPath(q) => self.qpath_ty(q),
            _ => resolve_ty_with_structs(ty, &self.env.defs, None, scope),
        }
    }

    /// `<T as Tr>::X`: only `Tr`'s `X`.
    pub(crate) fn qpath_ty(&self, q: &nabic_ast::QPath) -> HirTy {
        let base = self.resolve_ty_inner(&q.self_ty);
        let tp = &q.trait_path;
        let segs: Vec<&str> = tp.segments.iter().map(|s| s.name.as_str()).collect();
        let key = self
            .env
            .names
            .trait_key(&segs, &self.current_module, self.here());
        if !self.env.names.is_trait(&key) {
            self.emit_error(
                Code::NOT_A_TRAIT,
                format!(
                    "`{}` in `<.. as {}>` is not a trait",
                    segs.join("::"),
                    segs.join("::")
                ),
            );
            return HirTy::Unknown;
        }
        let want = resolve::trait_ref(
            &self.env.names,
            tp,
            &self.current_module,
            self.here(),
            |t| self.resolve_ty_inner(t),
        );
        self.project_or_report(&base, &q.item, Some(&[key]), &[want], true)
    }

    /// `T::Item` / `List<i32>::IntoIter`, resolved once the
    /// prefix is concrete; an unbound parameter prefix stays opaque
    /// (`J::Item`). `None`: not a projection.
    pub(crate) fn projection(&self, p: &AstPath) -> Option<HirTy> {
        let (last, prefix) = p.segments.split_last()?;
        if prefix.is_empty() || !last.generics.is_empty() {
            return None;
        }
        let segs: Vec<&str> = p.segments.iter().map(|s| s.name.as_str()).collect();
        if self.canon_type(&segs).is_some() {
            return None;
        }
        let pre: Vec<&str> = prefix.iter().map(|s| s.name.as_str()).collect();
        let base = match pre.as_slice() {
            ["Self"] => self.self_ty.clone()?,
            [n] if self.ty_subst.contains_key(*n) && prefix[0].generics.is_empty() => {
                self.ty_subst[*n].clone()
            }
            _ => {
                let ty = Ty {
                    kind: AstTyKind::Path(AstPath {
                        segments: prefix.to_vec(),
                        span: p.span,
                        origin: None,
                    }),
                    span: p.span,
                };
                let is_prim = matches!(crate::ty::resolve_ty(&ty), HirTy::Prim(_));
                if !is_prim && self.canon_type(&pre).is_none() {
                    return None;
                }
                self.resolve_ty_inner(&ty)
            }
        };
        let mut open = Vec::new();
        collect_unknown_paths(&base, &mut open);
        if !open.is_empty() {
            let mut path: Vec<String> = pre.iter().map(|s| (*s).to_owned()).collect();
            path.push(last.name.clone());
            return Some(HirTy::Path(path));
        }
        // A type parameter's projection goes through its bounds (and
        // their supertraits) first.
        let via = match pre.as_slice() {
            [n] => self.ty_bounds.get(*n).cloned(),
            _ => None,
        };
        let with_args = match pre.as_slice() {
            [n] => self.bound_wants(n),
            _ => Vec::new(),
        };
        Some(self.project_or_report(&base, &last.name, via.as_deref(), &with_args, false))
    }

    /// The bounds written on type parameter `name`, as trait refs: what
    /// its `T::X` / `T::N` must come from.
    pub(crate) fn bound_wants(&self, name: &str) -> Vec<TraitRef> {
        self.ty_bound_args
            .get(name)
            .into_iter()
            .flatten()
            .map(|p| {
                resolve::trait_ref(&self.env.names, p, &self.current_module, self.here(), |t| {
                    self.resolve_ty_inner(t)
                })
            })
            .collect()
    }

    /// `project`, reporting a miss. `strict`: only `traits`; else they are
    /// preferred over any impl. `args`: required leading trait arguments.
    pub(crate) fn project_or_report(
        &self,
        base: &HirTy,
        name: &str,
        traits: Option<&[String]>,
        args: &[TraitRef],
        strict: bool,
    ) -> HirTy {
        let hit = match traits {
            Some(t) if strict => self.project(base, name, Some(t), args),
            Some(t) => self
                .project(base, name, Some(t), args)
                .or_else(|| self.project(base, name, None, &[])),
            None => self.project(base, name, None, &[]),
        };
        hit.unwrap_or_else(|| {
            let via = match traits {
                Some([t]) if strict => format!(" of `{t}`"),
                _ => String::new(),
            };
            self.emit_error(
                Code::NO_METHOD,
                format!(
                    "`{}` has no associated type `{name}`{via} (no trait impl of it defines one)",
                    self.ty_name(base)
                ),
            );
            HirTy::Unknown
        })
    }

    /// Associated type `name` of concrete type `base`, from its trait
    /// impls (concrete or generic), of `traits` only when given.
    pub(crate) fn project(
        &self,
        base: &HirTy,
        name: &str,
        traits: Option<&[String]>,
        args: &[TraitRef],
    ) -> Option<HirTy> {
        if let HirTy::Param(p) = base {
            // No bound gives it: a blanket impl over the bounds may
            // (`I: Iterator` has `IntoIterator::IntoIter`).
            if let Some(t) = bounds::project(self, *p, name, traits, args) {
                return Some(t);
            }
        }
        // An open literal: as the type it holds, `Self` there staying
        // the literal (`<#var as Add>::Output`).
        if let Some(d) = infer::literal_default(self, base) {
            let r = self.project(&d, name, traits, args)?;
            return Some(if r == d { base.clone() } else { r });
        }
        let key = (
            base.clone(),
            name.to_owned(),
            traits.map(<[String]>::to_vec),
        );
        if self.env.sink.projecting.borrow().contains(&key) {
            // A cycle: this candidate adds nothing (filtered as unknown).
            return Some(HirTy::Unknown);
        }
        let hit = self.project_impls(base, name, traits, args, key);
        // Open literals inside (`Range<{float}>` of `lo..2.0`) that no impl
        // takes as they are: as the types they hold.
        if hit.is_none() && infer::has_var(self, base) {
            let z = infer::zonk(self, base);
            if z != *base && !infer::has_var(self, &z) {
                return self.project(&z, name, traits, args);
            }
        }
        hit
    }

    /// `project` over the impls of `base` (`key` guards the cycle).
    fn project_impls(
        &self,
        base: &HirTy,
        name: &str,
        traits: Option<&[String]>,
        args: &[TraitRef],
        key: (HirTy, String, Option<Vec<String>>),
    ) -> Option<HirTy> {
        let wanted = |t: &str| traits.is_none_or(|ts| ts.iter().any(|x| x == t));
        // An impl's trait arguments against a bound's (`TryFrom<i64>`);
        // an argument not yet concrete matches anything.
        let args_fit = |t: &str, have: &[HirTy]| {
            args.iter().filter(|w| w.path == t).all(|want| {
                want.args.iter().zip(have).all(|(w, h)| {
                    // An open literal fits what it can become.
                    if infer::literal_default(self, w).is_some() {
                        return infer::trial(self, &|| infer::unify(self, w, h));
                    }
                    let mut open = Vec::new();
                    collect_unknown_paths(w, &mut open);
                    collect_unknown_paths(h, &mut open);
                    !open.is_empty() || w == h || *w == HirTy::Unknown || *h == HirTy::Unknown
                })
            })
        };
        self.env.sink.projecting.borrow_mut().push(key);
        let mut hits: Vec<HirTy> = self
            .concrete_impls(base)
            .filter(|r| wanted(&r.path) && args_fit(&r.path, &r.args))
            .filter_map(|r| r.binds.iter().find(|(n, _)| n == name))
            .map(|(_, t)| t.clone())
            .collect();
        let keep = |gi: &Impl| wanted(&gi.trait_key) && gi.assoc.iter().any(|(n, _)| n == name);
        let generic = self.generic_impls(base, Pick::Where(&keep), 0);
        for (gi, subst) in generic {
            let Some((_, ty)) = gi.assoc.iter().find(|(n, _)| n == name) else {
                continue;
            };
            // A parameter only the trait arguments name (`N` of
            // `impl<T, const N: usize> DotAdd<Array<T, N>> for T`) leaves
            // them open: not this projection's impl.
            let open = gi.generics.params.iter().any(|gp| match gp {
                nabic_ast::GenericParam::Type { name, .. }
                | nabic_ast::GenericParam::Const { name, .. } => !subst.contains_key(name),
            });
            if open {
                continue;
            }
            let mut rcx = LowerCtx::new(self.env, &subst, None, self.here());
            rcx.current_module.clone_from(&gi.module);
            rcx.set_bounds(&gi.generics, &gi.where_clause);
            let have = resolve::trait_args(&gi.trait_args, |t| rcx.resolve_ast_ty(t)).0;
            if args_fit(&gi.trait_key, &have) {
                hits.push(rcx.resolve_ast_ty(ty));
            }
        }
        self.env.sink.projecting.borrow_mut().pop();
        hits.retain(|t| *t != HirTy::Unknown);
        let mut uniq: Vec<HirTy> = Vec::new();
        for h in hits {
            if !uniq.contains(&h) {
                uniq.push(h);
            }
        }
        if uniq.len() > 1 {
            let names: Vec<String> = uniq
                .iter()
                .map(|t| format!("`{}`", self.ty_name(t)))
                .collect();
            self.emit_error(
                Code::AMBIGUOUS_ITEM,
                format!(
                    "associated type `{}::{name}` is ambiguous: impls define it as {} - name \
                     the trait (`<T as Tr>::{name}`)",
                    self.ty_name(base),
                    names.join(", ")
                ),
            );
        }
        uniq.into_iter().next()
    }

    /// Record the trait bounds (with their supertraits) of the type
    /// parameters `generics` / `where_clause` declare, for projections.
    pub fn set_bounds(
        &mut self,
        generics: &nabic_ast::Generics,
        where_clause: &[nabic_ast::WherePred],
    ) {
        let mut add = |name: &str, bounds: &[Ty]| {
            for b in bounds {
                let AstTyKind::Path(p) = &b.kind else {
                    continue;
                };
                let segs: Vec<&str> = p.segments.iter().map(|s| s.name.as_str()).collect();
                let key = self
                    .env
                    .names
                    .trait_key(&segs, &self.current_module, self.here());
                self.ty_bound_args
                    .entry(name.to_owned())
                    .or_default()
                    .push(p.clone());
                let mut stack = vec![key];
                while let Some(k) = stack.pop() {
                    let entry = self.ty_bounds.entry(name.to_owned()).or_default();
                    if entry.contains(&k) {
                        continue;
                    }
                    entry.push(k.clone());
                    if let Some(info) = self.env.trait_of(&k) {
                        stack.extend(info.supers.iter().cloned());
                    }
                }
            }
        };
        for gp in &generics.params {
            if let nabic_ast::GenericParam::Type { name, bounds, .. } = gp {
                add(name, bounds);
            }
        }
        for w in where_clause {
            if let AstTyKind::Path(p) = &w.ty.kind {
                if let [seg] = p.segments.as_slice() {
                    add(&seg.name, &w.bounds);
                }
            }
        }
    }
}
