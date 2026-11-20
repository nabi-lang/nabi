//! Struct / enum instances, name resolution and visibility.

#[allow(clippy::wildcard_imports)]
use crate::lower::*;

impl LowerCtx<'_> {
    pub fn struct_def(&self, id: StructId) -> HirStructDef {
        self.env.defs.struct_at(id)
    }

    /// Struct id by table key (a declared struct or an instance).
    pub(crate) fn lookup_struct(&self, key: &str) -> Option<StructId> {
        self.env.defs.struct_id(key)
    }

    /// Instance `base<args>`, fields resolved here; a new one points bound
    /// errors at `span`.
    pub(crate) fn instantiate_struct(
        &self,
        base: &str,
        args: Vec<HirTy>,
        span: nabic_diagnostic::Span,
    ) -> Option<StructId> {
        let mut args = args;
        let in_base = |subst: &TySubst, t: &Ty| {
            let mut rcx = LowerCtx::new(self.env, subst, None, self.here());
            rcx.current_module = module_of(base);
            rcx.resolve_ast_ty(t)
        };
        if let Some(g) = self
            .env
            .defs
            .generics_of(base)
            .filter(|_| self.env.defs.is_generic_struct(base))
        {
            if let Some(e) = mono::fit_args(leaf_name(base), g, &mut args, in_base) {
                self.emit_arity(span, e);
            }
            self.check_const_args(g, &args)?;
        }
        infer::unify_def_bounds(self, base, &args);
        self.env.defs.instantiate_struct(base, args, span, in_base)
    }

    pub fn enum_def(&self, id: EnumId) -> HirEnumDef {
        self.env.defs.enum_at(id)
    }

    /// Enum id by table key (a declared enum or an instance).
    pub(crate) fn lookup_enum(&self, key: &str) -> Option<EnumId> {
        self.env.defs.enum_id(key)
    }

    /// The instances of generic enum `base` made so far.
    pub(crate) fn enum_instantiations(&self, base: &str) -> Vec<EnumId> {
        let ids = Self::instantiations(&self.env.defs.enum_table.borrow(), base);
        incr::log(
            || format!("ie|{base}"),
            || incr::Lookup::EnumInsts(base.to_owned(), ids.clone()),
        );
        ids
    }

    /// `instantiate_struct` for a generic enum.
    pub(crate) fn instantiate_enum(
        &self,
        base: &str,
        args: Vec<HirTy>,
        span: nabic_diagnostic::Span,
    ) -> Option<EnumId> {
        let mut args = args;
        if let Some(def) = self.env.defs.generic_enum(base) {
            let misfit = mono::fit_args(leaf_name(base), &def.generics, &mut args, |subst, t| {
                let mut rcx = LowerCtx::new(self.env, subst, None, self.here());
                rcx.current_module = module_of(base);
                rcx.resolve_ast_ty(t)
            });
            if let Some(e) = misfit {
                self.emit_arity(span, e);
            }
            self.check_const_args(&def.generics, &args)?;
        }
        infer::unify_def_bounds(self, base, &args);
        self.env
            .defs
            .instantiate_enum(base, args, span, |def, subst| {
                let mut rcx = LowerCtx::new(self.env, subst, None, self.here());
                rcx.current_module = module_of(base);
                generic_enum_variants(def, &|t| rcx.resolve_ast_ty(t))
            })
    }

    /// A generic type's argument list that does not fit (`mono::fit_args`).
    pub(crate) fn emit_arity(&self, span: Span, (msg, help): (String, String)) {
        let at = if span.is_dummy() { self.here() } else { span };
        self.emit_diag(
            nabic_diagnostic::NabiDiagnostic::error(Code::GENERIC_ARGUMENTS, at, msg)
                .with_help(help),
        );
    }

    /// A `char` const parameter takes a `char` argument, any other an
    /// integer or `bool`.
    pub(crate) fn check_const_args(
        &self,
        generics: &nabic_ast::Generics,
        args: &[HirTy],
    ) -> Option<()> {
        for (gp, arg) in generics.params.iter().zip(args) {
            if let (nabic_ast::GenericParam::Const { name, ty }, Some(v)) = (gp, arg.as_const()) {
                mono::typed_const(self, name, ty, v)?;
            }
        }
        Some(())
    }

    /// Canonical type-namespace name of a path, from this module and its
    /// `use`s.
    pub(crate) fn canon_type(&self, segments: &[&str]) -> Option<String> {
        if let Some(key) = self.self_key(segments) {
            return Some(key);
        }
        self.env.names.canon(
            segments,
            &self.access_module(),
            self.reach_uses(),
            self.here(),
        )
    }

    /// The key of `Self` when `segments` is the absolute path a `Self::..`
    /// rewrites to. `Self` is the impl's target, visible from the impl's
    /// module: a library trait's default may name a user's private type.
    pub(crate) fn self_key(&self, segments: &[&str]) -> Option<String> {
        let key = match segments {
            ["pkg" | "", rest @ ..] => rest.join("::"),
            _ => return None,
        };
        let is_self = match &self.self_ty {
            Some(HirTy::Struct(id)) => self.lookup_struct(&key) == Some(*id),
            Some(HirTy::Enum(id)) => self.lookup_enum(&key) == Some(*id),
            _ => false,
        };
        is_self.then_some(key)
    }

    /// Split `a::E::V` into (`a::E`, `V`) when the prefix is an enum; a
    /// single segment is an imported variant (`use E::*`, `Ok` via core).
    pub(crate) fn enum_path(&self, segments: &[&str]) -> Option<(String, String)> {
        let (variant, ty) = segments.split_last()?;
        if ty.is_empty() {
            let key = self.env.names.canon_variant(
                segments,
                &self.current_module,
                &self.local_uses,
                self.here(),
            )?;
            let (e, v) = key.rsplit_once("::")?;
            return Some((e.to_owned(), v.to_owned()));
        }
        Some((self.canon_type(ty)?, (*variant).to_owned()))
    }

    /// Instantiations of generic base `base` in `table`, sorted by key
    /// for deterministic choice.
    pub(crate) fn instantiations<V: Copy>(table: &HashMap<String, V>, base: &str) -> Vec<V> {
        let prefix = format!("{base}__");
        let mut hits: Vec<(&String, V)> = table
            .iter()
            .filter(|(k, _)| k.starts_with(&prefix))
            .map(|(k, v)| (k, *v))
            .collect();
        hits.sort_by(|a, b| a.0.cmp(b.0));
        hits.into_iter().map(|(_, v)| v).collect()
    }

    /// The module `const` (`statics`: `static`) that `segments` names.
    pub(crate) fn resolve_in(&self, segments: &[&str], statics: bool) -> Option<String> {
        let env = self.env;
        let exists = |k: &str| {
            if statics {
                env.static_ty(k).is_some()
            } else {
                env.konst(k).is_some()
            }
        };
        let keys = |leaf: &str| {
            incr::read(|| {
                if statics {
                    incr::Read::StaticLeaf(leaf.to_owned())
                } else {
                    incr::Read::ConstLeaf(leaf.to_owned())
                }
            });
            env.value_leaf(statics, leaf)
        };
        // A glob reaches an item in a private module through its `pub use`.
        let visible = |k: &str| {
            let vis = self.env.item_vis(k);
            vis.is_none_or(|v| self.is_visible(&module_of(k), v))
                && self.env.names.hidden(&self.access_module(), k).is_none()
        };
        let set = NameSet {
            exists: &exists,
            keys: &keys,
            visible: &visible,
            in_type: &|k| env.names.has_type(k),
        };
        match resolve_name(
            segments,
            &self.access_module(),
            self.reach_uses(),
            &self.env.names.uses,
            &set,
        ) {
            Ok(r) => r,
            Err(hits) => {
                self.report_ambiguous(&segments.join("::"), &hits);
                hits.into_iter().next()
            }
        }
    }

    /// Resolve a function path across concrete and generic functions, so
    /// the nearest declaration wins (own items, `use`s, core).
    pub(crate) fn resolve_any_fn(&self, segments: &[&str]) -> Option<String> {
        let env = self.env;
        // A hook lowering calls (`expr::lang_fn`): its slot's item, else
        // the path it had before it had a slot.
        if let Some(slot) = segments.first().and_then(|s| expr::lang_slot(s)) {
            let item = self.lang(slot);
            if !item.is_empty() {
                return Some(item.to_owned());
            }
            return self.resolve_any_fn(&segments[1..]);
        }
        if let [name] = segments {
            // An empty key: a `use` of the name hides outer nested fns.
            let nested = self.nested_fns.iter().rev().find_map(|m| m.get(*name));
            if let Some(k) = nested.filter(|k| !k.is_empty()) {
                return Some(k.clone());
            }
        }
        let exists = |k: &str| env.sig(k).is_some() || env.generic_fn(k).is_some();
        let keys = |leaf: &str| env.fn_leaf(leaf);
        // A glob reaches an item in a private module through its `pub use`.
        let visible = |k: &str| {
            let vis = self.env.item_vis(k);
            vis.is_none_or(|v| self.is_visible(&module_of(k), v))
                && self.env.names.hidden(&self.access_module(), k).is_none()
        };
        let set = NameSet {
            exists: &exists,
            keys: &keys,
            visible: &visible,
            in_type: &|k| env.names.has_type(k),
        };
        match resolve_name(
            segments,
            &self.access_module(),
            self.reach_uses(),
            &self.env.names.uses,
            &set,
        ) {
            Ok(r) => r,
            // An item and its re-exports are one item.
            Err(mut hits) => {
                let item = |k: &String| {
                    (
                        env.sig(k).map(|s| s.id),
                        env.generic_fn(k).map(std::ptr::from_ref::<FnDef>),
                    )
                };
                // Each item once, by its shortest path.
                hits.sort_by_key(|h| (h.len(), h.clone()));
                let mut seen = Vec::new();
                hits.retain(|h| {
                    let it = item(h);
                    let new = !seen.contains(&it);
                    seen.push(it);
                    new
                });
                if hits.len() > 1 {
                    self.report_ambiguous(&segments.join("::"), &hits);
                }
                hits.into_iter().next()
            }
        }
    }

    /// Resolve a callee name (single- or multi-segment path) to a fully
    /// qualified fn-table key (a concrete fn), or an `extern "C"` one.
    pub(crate) fn resolve_fn_name(&self, segments: &[&str]) -> Option<String> {
        match self.resolve_any_fn(segments) {
            Some(f) => self.env.sig(&f).is_some().then_some(f),
            None => match segments {
                [name] if self.env.extern_fn(name).is_some() => Some((*name).to_owned()),
                _ => None,
            },
        }
    }

    /// `resolve_fn_name` for a generic fn (a `generic_fns` key).
    pub(crate) fn resolve_generic_fn_name(&self, segments: &[&str]) -> Option<String> {
        self.resolve_any_fn(segments)
            .filter(|f| self.env.generic_fn(f).is_some())
    }

    pub(crate) fn report_ambiguous(&self, name: &str, hits: &[String]) {
        self.emit_error(
            Code::AMBIGUOUS_NAME,
            format!(
                "name `{name}` is ambiguous in module `{}` - multiple glob imports \
                 resolve it: {}",
                module_display(&self.access_module()),
                hits.join(", ")
            ),
        );
    }

    /// Visibility check: can a caller in `self.current_module` see an
    /// item declared in `def_module` with the given visibility?
    pub(crate) fn is_visible(&self, def_module: &str, vis: Visibility) -> bool {
        visible_from(&self.access_module(), def_module, vis)
    }

    /// Report use of a private value item (fn / const / static).
    pub(crate) fn check_item_visible(&self, full: &str, what: &str) -> bool {
        self.check_reached_visible(full, full, what)
    }

    /// `check_item_visible` for item `full` named as `written`, a
    /// `pub use` of it: the modules on the written path must be visible,
    /// not those on the item's own.
    pub(crate) fn check_reached_visible(&self, written: &str, full: &str, what: &str) -> bool {
        if self.harness_main {
            return true;
        }
        let vis = self.env.item_vis(full).unwrap_or(Visibility::Pub);
        let def_module = module_of(full);
        let from = self.access_module();
        if let Some(m) = self.env.names.hidden(&from, written) {
            self.emit_error(
                Code::PRIVATE_ITEM,
                resolve::hidden_module_msg(
                    written,
                    &m,
                    &from,
                    Some(self.env.names.visible_path(full)),
                ),
            );
            return false;
        }
        if self.is_visible(&def_module, vis) {
            return true;
        }
        if vis == Visibility::Pkg {
            self.emit_error(
                Code::PRIVATE_ITEM,
                format!(
                    "{what} `{full}` is `pub(pkg)`: visible only inside its package \
                     `{}`",
                    module_display(package_of(&def_module))
                ),
            );
            let pkg = module_display(package_of(&def_module));
            let help = resolve::own_library_privacy(&def_module).unwrap_or_else(|| {
                if is_toolchain_module(&def_module) {
                    format!(
                        "it is a helper of `{pkg}`, not part of its API: use a `pub` item instead"
                    )
                } else {
                    format!(
                        "use a `pub` item of `{pkg}`, or make `{}` `pub` there",
                        leaf_name(full)
                    )
                }
            });
            self.help_last(Some(help));
            return false;
        }
        if vis == Visibility::Sub {
            self.emit_error(
                Code::PRIVATE_ITEM,
                format!(
                    "{what} `{full}` is `pub(sub)`: visible only in module `{}` and its \
                     descendants, not from `{}`",
                    module_display(&def_module),
                    module_display(&from)
                ),
            );
            self.help_last(Some(format!(
                "make `{}` `pub(pkg)` or `pub` to use it here",
                leaf_name(full)
            )));
            return false;
        }
        let (vis, _) = resolve::needed_vis(&from, &def_module);
        self.emit_error(
            Code::PRIVATE_ITEM,
            format!(
                "{what} `{full}` is private to module `{}` and cannot be used from `{}` \
                 (add `{vis}` to expose it)",
                module_display(&def_module),
                module_display(&from)
            ),
        );
        self.help_last(resolve::own_library_privacy(&def_module));
        false
    }

    /// The module defining `sid`, when the current module may not see
    /// its field `field`.
    pub(crate) fn field_hidden(&self, sid: StructId, field: &str) -> Option<String> {
        let def = self.struct_def(sid);
        let base = if def.generic_args.is_empty() {
            def.name
        } else {
            def.name.split("__").next().unwrap_or(&def.name).to_owned()
        };
        let vis = self.env.fields_vis(&base)?.get(field).copied()?;
        let def_module = module_of(&base);
        (!self.is_visible(&def_module, vis)).then_some(def_module)
    }

    /// Report access to a private field from outside its struct's module,
    /// and warn about a marked one.
    pub(crate) fn check_field_visible(&self, sid: StructId, field: &str) {
        self.check_field_visible_at(sid, field, self.here());
    }

    /// `check_field_visible`, a marked field's warning at `at`.
    pub(crate) fn check_field_visible_at(&self, sid: StructId, field: &str, at: Span) {
        // The struct's own methods still set and read its marked fields.
        if self.marks_checked() && self.self_ty != Some(HirTy::Struct(sid)) {
            let name = self.struct_def(sid).name;
            let base = name.split("__").next().unwrap_or(&name);
            if let Some(m) = self.env.mark(&format!("{base}.{field}")) {
                let shown = format!("{}.{field}", leaf_name(base));
                self.use_marked("field", &shown, m, at);
            }
        }
        if let Some(def_module) = self.field_hidden(sid, field) {
            // A getter of the same name, or a `take_` one for an owned
            // part, is what to write instead.
            let has = |m: &str| {
                self.env
                    .methods_on(&HirTy::Struct(sid), m)
                    .is_some_and(|m| !m.is_empty())
            };
            let take = format!("take_{field}");
            let fix = if has(field) {
                format!("read it with `.{field}()`")
            } else if has(&take) {
                format!("take it with `.{take}()`")
            } else {
                "add `pub` to the field to expose it".to_owned()
            };
            self.emit_error(
                Code::PRIVATE_ITEM,
                format!(
                    "field `{field}` of `{}` is private to module `{}` ({fix})",
                    self.ty_name(&HirTy::Struct(sid)),
                    module_display(&def_module)
                ),
            );
        }
    }

    /// Uses here warn about marked items: the code is the built
    /// package's.
    pub(crate) fn marks_apply(&self) -> bool {
        self.env
            .mark_pkg
            .as_deref()
            .is_some_and(|p| package_of(&self.access_module()) == p)
    }

    /// Uses here are checked against marks at all: the built package's,
    /// or any code outside the toolchain's packages (`#[internal]` holds
    /// in a dependency built from source too).
    pub(crate) fn marks_checked(&self) -> bool {
        self.marks_apply() || !is_toolchain_module(&self.current_module)
    }

    /// Statements here warn `unused_result`: the built package's, and
    /// with `--check-library` the library's.
    pub(crate) fn results_apply(&self) -> bool {
        self.marks_apply()
            || (self.env.lint_library && {
                let p = package_of(&self.current_module);
                nabic_core::in_toolchain(p)
            })
    }

    /// A call at `at` of a function with marks `mark`: a discardable one
    /// is remembered for `unused_result`.
    pub(crate) fn call_marked(&self, mark: &crate::marks::Mark, at: Span) {
        if mark.discardable && self.results_apply() {
            self.discardable_calls.borrow_mut().push(at);
        }
    }

    /// A use at `at` of `name`, a `what` with marks `mark`: an error for an
    /// `internal` one, else a `deprecated` warning, and outside `unsafe` a
    /// `deprecated_safe` one. Only the built package's own uses are checked.
    pub(crate) fn use_marked(&self, what: &str, name: &str, mark: &crate::marks::Mark, at: Span) {
        if at.is_dummy() {
            return;
        }
        // Code a toolchain template or the compiler wrote is checked
        // where it is defined.
        let name = &shown_mark_name(name);
        if let Some(note) = mark
            .internal
            .as_ref()
            .filter(|_| !is_toolchain_module(&self.access_module()))
        {
            let help = if note.is_empty() {
                "help: it is compiler-only: no public item replaces it, so remove the use"
                    .to_owned()
            } else {
                format!("help: {note}")
            };
            let e = nabic_diagnostic::NabiDiagnostic::error(
                Code::PRIVATE_ITEM,
                at,
                format!(
                    "{what} `{name}` is internal to the Nabi toolchain: only `bare`, \
                     `core` and `std` may use it"
                ),
            )
            .with_note(help);
            let seen = self
                .env
                .sink
                .diagnostics
                .borrow()
                .iter()
                .any(|d| d.primary == e.primary && d.message == e.message);
            if !seen {
                self.emit_diag(e);
            }
            return;
        }
        if !self.marks_apply() {
            return;
        }
        if let Some(why) = crate::lower::objc::refused(name, mark, &self.available) {
            self.emit_diag(nabic_diagnostic::NabiDiagnostic::error(Code::FFI, at, why));
        }
        let mut out = Vec::new();
        if let Some(d) = &mark.deprecated {
            let mut w = nabic_diagnostic::NabiDiagnostic::warning(
                Code::LINT,
                at,
                format!("use of deprecated {what} `{name}`"),
            )
            .with_lint("deprecated")
            .with_tag(nabic_diagnostic::Tag::Deprecated);
            for n in d.notes(name) {
                w = w.with_note(n);
            }
            out.push(w);
        }
        if let (Some(note), 0) = (&mark.deprecated_safe, self.unsafe_depth) {
            let mut w = nabic_diagnostic::NabiDiagnostic::warning(
                Code::LINT,
                at,
                format!(
                    "`{name}` will need `unsafe` in a later release: wrap the {} in \
                     `unsafe {{ .. }}`",
                    if what == "static" { "use" } else { "call" }
                ),
            )
            .with_lint("deprecated_safe");
            if !note.is_empty() {
                w = w.with_note(note.clone());
            }
            out.push(w);
        }
        // Every instance of a generic body lowers the same use.
        let mut sink = self.env.sink.diagnostics.borrow_mut();
        for w in out {
            if !sink
                .iter()
                .any(|d| d.primary == w.primary && d.message == w.message)
            {
                sink.push(w);
            }
        }
    }
}
