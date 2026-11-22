//! Type-namespace tables: names, aliases and their visibility.

#[allow(clippy::wildcard_imports)]
use crate::lower::*;

/// Glob resolution of one bare name: a single import beats any glob; two
/// globs landing on one suffix are `Ambiguous`.
pub(crate) enum LookupOutcome {
    None,
    Resolved(String),
    Ambiguous(Vec<String>),
}

/// A name table to resolve against: membership test plus the keys
/// ending in a given segment, needed only by `use a::**` deep globs.
pub(crate) struct NameSet<'b> {
    pub(crate) exists: &'b dyn Fn(&str) -> bool,
    pub(crate) keys: &'b dyn Fn(&str) -> Vec<String>,
    /// Is the key visible from the resolving module? A glob brings in only
    /// what it can see, so core's private helpers never compete.
    pub(crate) visible: &'b dyn Fn(&str) -> bool,
    /// Does a deep glob's matched path end at a type before the name? Then
    /// the key is an associated item, which `**` doesn't reach.
    pub(crate) in_type: &'b dyn Fn(&str) -> bool,
}

/// Type namespace: every struct / union / enum / alias / trait name
/// (qualified, generic bases unmangled) plus `pub use` aliases. Every type
/// path resolves through `canon`.
#[derive(Clone)]
pub struct TypeNames {
    pub(crate) names: nabic_core::HashSet<String>,
    pub(crate) traits: nabic_core::HashSet<String>,
    /// Structural auto traits (`#[lang("send_trait" | "sync_trait")]`):
    /// a type has them unless a component lacks them.
    pub auto_traits: nabic_core::HashSet<String>,
    /// `unsafe trait`s: implemented only by `unsafe impl`.
    pub(crate) unsafe_traits: nabic_core::HashSet<String>,
    /// The `#[lang]` slots.
    pub(crate) lang: nabic_parser::LangTable,
    /// Every enum variant as `a::E::V` (generic bases unmangled). A
    /// variant is as visible as its enum.
    pub(crate) variants: nabic_core::HashSet<String>,
    /// `pub use` alias key -> the type its re-export chain ends at.
    pub(crate) reexports: HashMap<String, String>,
    pub(crate) visibility: HashMap<String, Visibility>,
    /// Module path -> its `mod` visibility (the driver's flattened stubs).
    pub(crate) mod_vis: HashMap<String, Visibility>,
    /// Item path -> the public path diagnostics name it by, where the two
    /// differ (`resolve::visible_paths`).
    pub(crate) visible: HashMap<String, String>,
    pub(crate) uses: crate::uses::UseTable,
    pub(crate) diags: std::cell::RefCell<Vec<nabic_diagnostic::NabiDiagnostic>>,
    /// Generic aliases by full name, expanded at each use; also non-generic
    /// ones with a `where` clause (checked once, not expanded here).
    pub(crate) generic_aliases: HashMap<String, nabic_ast::TypeAliasDef>,
    /// Non-generic aliases, so generic instance arguments can name one
    /// before the alias table exists.
    pub(crate) plain_aliases: HashMap<String, Ty>,
    /// Plain aliases being expanded (caps a cycle).
    pub(crate) alias_depth: std::cell::Cell<u32>,
    /// Plain aliases that are cyclic or name a projection that fails.
    pub(crate) broken_aliases: std::cell::RefCell<nabic_core::HashSet<String>>,
    /// Every generic alias use: (alias, arguments, module, span), for
    /// its bounds check (`check_type_bounds`).
    pub(crate) alias_uses: std::cell::RefCell<Vec<AliasUse>>,
    /// First use of each unresolved type path, for its "cannot find
    /// type" location.
    pub(crate) unknown_spans: std::cell::RefCell<HashMap<String, nabic_diagnostic::Span>>,
    /// First use of each generic type written without arguments (a
    /// leftover one is "needs generic arguments", not "cannot find").
    pub(crate) bare_generics: std::cell::RefCell<HashMap<String, nabic_diagnostic::Span>>,
    /// A generic type left unresolved by an unknown argument (`List<Nope>`)
    /// -> that argument, which the report names instead; `""` when a bad
    /// argument was reported already.
    pub(crate) open_args: std::cell::RefCell<HashMap<String, String>>,
    /// `canon` results without local `use`s or diagnostics, by (path,
    /// module): its glob lookups scan every type name.
    pub(crate) canon_memo: std::cell::RefCell<HashMap<(String, String), Option<String>>>,
    /// Type and variant keys by last segment, built on the first deep
    /// glob lookup (the tables are complete by then).
    pub(crate) leaves: std::cell::OnceCell<Leaves>,
    pub(crate) variant_leaves: std::cell::OnceCell<Leaves>,
    /// Module-level `const`s by full name, for array lengths in
    /// signatures and fields (`[i32; N]`).
    pub(crate) consts: HashMap<String, (Ty, nabic_ast::Expr)>,
    pub(crate) const_leaves: std::cell::OnceCell<Leaves>,
    /// `pub use` alias key -> the `const` / `static` its re-export chain
    /// ends at (both keys are in the const / static tables).
    pub(crate) value_reexports: HashMap<String, String>,
    /// `type X = some Tr;` by full name.
    pub(crate) some_types: HashMap<String, SomeType>,
    /// Each named `some` type's concrete type, once its defining
    /// functions are lowered.
    pub(crate) some_concrete: std::cell::RefCell<HashMap<String, HirTy>>,
    /// Non-generic structs with a field that writes a named `some` type:
    /// every field's type as written.
    pub(crate) some_fields: Vec<(StructId, Vec<Ty>)>,
}

/// A named opaque type `type X = some Tr;`: one concrete type, fixed by
/// the functions of X's module that return `X`. Outside that module only
/// the bounds are visible.
#[derive(Clone, Debug)]
pub(crate) struct SomeType {
    pub(crate) bounds: Vec<Ty>,
    /// The defining functions' full names, in source order.
    pub(crate) definers: Vec<String>,
    pub(crate) span: Span,
}

/// Keys by their last `::` segment.
pub(crate) type Leaves = HashMap<String, Vec<String>>;

pub(crate) fn leaves<'k>(keys: impl IntoIterator<Item = &'k String>) -> Leaves {
    let mut out: Leaves = HashMap::default();
    for k in keys {
        let leaf = k.rsplit("::").next().unwrap_or(k);
        out.entry(leaf.to_owned()).or_default().push(k.clone());
    }
    out
}

/// (alias full name, arguments, module of the use, span of the use).
pub(crate) type AliasUse = (String, Vec<Ty>, String, nabic_diagnostic::Span);

/// Can code in `module` name the type-like item `full`?
/// Unknown names (primitives, ..) are visible.
#[must_use]
pub fn type_visible(m: &crate::HirModule, full: &str, module: &str) -> bool {
    m.type_visibility
        .get(full)
        .is_none_or(|&vis| visible_from(module, &module_of(full), vis))
}

impl TypeNames {
    /// The item bound to lang slot `slot`, `""` if unbound; a
    /// method slot (`drop_method`) gives its trait.
    pub(crate) fn lang_item(&self, slot: &str) -> &str {
        self.lang.lookup(slot).map_or("", |e| {
            if slot.ends_with("_method") {
                e.item_name.rsplit_once("::").map_or("", |(t, _)| t)
            } else {
                &e.item_name
            }
        })
    }

    /// `Row<3>` -> `[i32; 3]` for a generic alias; the body is resolved in
    /// the alias's module and written absolute.
    pub(crate) fn expand_alias(&self, ty: &Ty, module: &str, at: Span) -> Option<Ty> {
        self.expand_alias_with(ty, module, at, true)
    }

    /// `expand_alias` that reports nothing and records no use: for
    /// comparing types (impl headers) already checked elsewhere.
    pub(crate) fn expand_alias_quiet(&self, ty: &Ty, module: &str) -> Option<Ty> {
        self.expand_alias_with(ty, module, ty.span, false)
    }

    fn expand_alias_with(&self, ty: &Ty, module: &str, at: Span, report: bool) -> Option<Ty> {
        let AstTyKind::Path(p) = &ty.kind else {
            return None;
        };
        let last = p.segments.last()?;
        let args = &last.generics;
        if !self.any_generic_alias() {
            return None;
        }
        // A bare name is an alias use only for an alias with every
        // parameter defaulted (`type Vel<V = f64> = ..`).
        if args.is_empty() && !self.defaulted_alias_leaf(&last.name) {
            return None;
        }
        let segs: Vec<&str> = p.segments.iter().map(|s| s.name.as_str()).collect();
        let full = self.canon(&segs, module, &[], at)?;
        let alias = self
            .generic_alias(&full)
            .filter(|a| !a.generics.params.is_empty())?;
        let params = &alias.generics.params;
        let min = params
            .iter()
            .take_while(|p| {
                !matches!(
                    p,
                    nabic_ast::GenericParam::Type {
                        default: Some(_),
                        ..
                    }
                )
            })
            .count();
        // `Vel` for `type Vel<V = f64> = ..`: every argument defaulted.
        if args.is_empty() && min > 0 {
            return None;
        }
        if report && (args.len() < min || args.len() > params.len()) {
            let (msg, help) = mono::misfit(leaf_name(&full), &alias.generics, args.len());
            self.error_help(
                Code::GENERIC_ARGUMENTS,
                if ty.span.is_dummy() { at } else { ty.span },
                msg,
                help,
            );
        }
        // A missing argument (reported above) reads as `any`.
        let any = Ty {
            kind: AstTyKind::Any,
            span: ty.span,
        };
        let map: HashMap<String, Ty> = params
            .iter()
            .enumerate()
            .map(|(i, gp)| match gp {
                nabic_ast::GenericParam::Type { name, default, .. } => (
                    name.clone(),
                    args.get(i).or(default.as_ref()).unwrap_or(&any).clone(),
                ),
                nabic_ast::GenericParam::Const { name, .. } => {
                    (name.clone(), args.get(i).unwrap_or(&any).clone())
                }
            })
            .collect();
        let mut out = alias.ty.clone();
        self.absolutize(&mut out, &module_of(&full), module, &map, at);
        collect::subst_assoc_ty(&mut out, &map);
        if report {
            self.alias_uses
                .borrow_mut()
                .push((full, args.clone(), module.to_owned(), ty.span));
        }
        Some(out)
    }

    /// The non-generic alias `ty` names in `module`.
    pub(crate) fn plain_alias(&self, ty: &Ty, module: &str, at: Span) -> Option<String> {
        let AstTyKind::Path(p) = &ty.kind else {
            return None;
        };
        if !p.segments.last()?.generics.is_empty() {
            return None;
        }
        let segs: Vec<&str> = p.segments.iter().map(|s| s.name.as_str()).collect();
        self.canon(&segs, module, &[], at)
            .filter(|full| self.plain_alias_ty(full).is_some())
    }

    /// `B` -> `<A as P>::Out` for a plain alias of a projection, expanded in
    /// body context (after impl registration).
    pub(crate) fn projection_alias(&self, full: &str, module: &str, at: Span) -> Option<Ty> {
        let body = self.plain_alias_ty(full)?;
        let from = module_of(full);
        if !self.names_projection(body, &from, at) {
            return None;
        }
        let mut out = body.clone();
        self.absolutize(&mut out, &from, module, &HashMap::default(), at);
        Some(out)
    }

    /// Does `ty` (written in `module`) name a projection: `<T as Tr>::X`,
    /// or `A::X` with `A` a type?
    pub(crate) fn names_projection(&self, ty: &Ty, module: &str, at: Span) -> bool {
        struct Find<'x> {
            names: &'x TypeNames,
            module: &'x str,
            at: Span,
            found: bool,
        }
        impl crate::walk::AstVisitor for Find<'_> {
            fn ty(&mut self, t: &Ty) {
                match &t.kind {
                    AstTyKind::QPath(_) => self.found = true,
                    AstTyKind::Path(p) => {
                        let segs: Vec<&str> = p.segments.iter().map(|s| s.name.as_str()).collect();
                        let n = self.names;
                        match n.canon(&segs, self.module, &[], self.at) {
                            // Through another alias (`type D = List<B>;`).
                            Some(full) if n.alias_depth.get() < 32 => {
                                if let Some(body) = n.plain_alias_ty(&full) {
                                    n.alias_depth.set(n.alias_depth.get() + 1);
                                    self.found |=
                                        n.names_projection(body, &module_of(&full), self.at);
                                    n.alias_depth.set(n.alias_depth.get() - 1);
                                }
                            }
                            Some(_) => {}
                            None => {
                                self.found |= segs.len() > 1
                                    && n.canon(&segs[..segs.len() - 1], self.module, &[], self.at)
                                        .is_some();
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        let mut f = Find {
            names: self,
            module,
            at,
            found: false,
        };
        crate::walk::walk_ty(&mut f, ty);
        f.found
    }

    /// Rewrite the type names in `ty` (outside `params`) that resolve in
    /// module `from` to absolute paths valid in module `to`.
    pub(crate) fn absolutize(
        &self,
        ty: &mut Ty,
        from: &str,
        to: &str,
        params: &HashMap<String, Ty>,
        at: Span,
    ) {
        let mut rec = |t: &mut Ty| self.absolutize(t, from, to, params, at);
        match &mut ty.kind {
            AstTyKind::Path(p) => {
                for s in &mut p.segments {
                    s.generics.iter_mut().for_each(&mut rec);
                }
                let mut segs: Vec<&str> = p.segments.iter().map(|s| s.name.as_str()).collect();
                if params.contains_key(segs[0]) || matches!(segs[0], "pkg" | "" | "Self") {
                    return;
                }
                // `A::Out`: a projection keeps its last segment.
                let mut tail = None;
                let full = match self.canon(&segs, from, &[], at) {
                    Some(full) => full,
                    None if segs.len() > 1 => {
                        segs.pop();
                        let Some(full) = self.canon(&segs, from, &[], at) else {
                            return;
                        };
                        tail = p.segments.pop();
                        full
                    }
                    None => return,
                };
                let pkg = resolve::package_of(&full);
                let abs: Vec<String> = if nabic_core::in_toolchain(&full) {
                    std::iter::once("")
                        .chain(full.split("::"))
                        .map(str::to_owned)
                        .collect()
                } else if pkg == resolve::package_of(to) {
                    let rest = full.strip_prefix(pkg).unwrap_or(&full);
                    std::iter::once("pkg")
                        .chain(rest.split("::").filter(|x| !x.is_empty()))
                        .map(str::to_owned)
                        .collect()
                } else {
                    // Another package names it by its public path, not one
                    // through a private module (`a::Q`, not `a::quantity::Q`).
                    let full = if self.hidden(to, &full).is_some() {
                        self.visible_path(&full).to_owned()
                    } else {
                        full
                    };
                    // From the package's root: `to`'s package may not
                    // depend on it.
                    let mut abs: Vec<String> = full.split("::").map(str::to_owned).collect();
                    abs[0].insert_str(0, nabic_parser::PKG_ROOT);
                    abs
                };
                let last = p.segments.pop().map(|s| s.generics).unwrap_or_default();
                let n = abs.len();
                p.segments = abs
                    .into_iter()
                    .enumerate()
                    .map(|(i, name)| nabic_ast::PathSegment {
                        name,
                        generics: if i + 1 == n { last.clone() } else { Vec::new() },
                    })
                    .collect();
                p.segments.extend(tail);
            }
            AstTyKind::Sum(ts) | AstTyKind::Tuple(ts) => ts.iter_mut().for_each(rec),
            AstTyKind::Array(e, _) | AstTyKind::Nest(e) | AstTyKind::Binding(_, e) => rec(e),
            AstTyKind::QPath(q) => {
                rec(&mut q.self_ty);
                let mut tp = Ty {
                    kind: AstTyKind::Path(q.trait_path.clone()),
                    span: ty.span,
                };
                rec(&mut tp);
                if let AstTyKind::Path(p) = tp.kind {
                    q.trait_path = p;
                }
            }
            AstTyKind::Fn(args, ret, _) => {
                args.iter_mut().for_each(&mut rec);
                rec(ret);
            }
            AstTyKind::Throws { ok, err } => {
                rec(ok);
                rec(err);
            }
            _ => {}
        }
    }

    /// The type key `segments` names from `module`; its errors
    /// (ambiguity, visibility) are reported at `at`.
    pub fn canon(
        &self,
        segments: &[&str],
        module: &str,
        local_uses: &[Vec<crate::uses::UseEntry>],
        at: Span,
    ) -> Option<String> {
        if !local_uses.iter().all(Vec::is_empty) {
            return self.canon_uncached(segments, module, local_uses, at);
        }
        incr::read(|| {
            incr::Read::Canon(
                segments.iter().map(|s| (*s).to_owned()).collect(),
                module.to_owned(),
            )
        });
        let key = (segments.join("::"), module.to_owned());
        if let Some(hit) = self.canon_memo.borrow().get(&key) {
            return hit.clone();
        }
        let errors = self.diags.borrow().len();
        let out = self.canon_uncached(segments, module, local_uses, at);
        if self.diags.borrow().len() == errors {
            self.canon_memo.borrow_mut().insert(key, out.clone());
        }
        out
    }

    /// Can `module` see type-namespace key `k`?
    pub(crate) fn visible_type(&self, k: &str, module: &str) -> bool {
        self.type_vis(k)
            .is_none_or(|vis| visible_from(module, &module_of(k), vis))
    }

    pub(crate) fn canon_uncached(
        &self,
        segments: &[&str],
        module: &str,
        local_uses: &[Vec<crate::uses::UseEntry>],
        at: Span,
    ) -> Option<String> {
        let exists = |k: &str| self.has_type(k) || self.reexport(k).is_some();
        let keys = |leaf: &str| self.type_leaf(leaf);
        // A glob reaches an item in a private module through its `pub use`.
        let visible = |k: &str| self.visible_type(k, module) && self.hidden(module, k).is_none();
        let set = NameSet {
            exists: &exists,
            keys: &keys,
            visible: &visible,
            in_type: &|_| false,
        };
        let item = |k: &String| self.reexport(k).unwrap_or(k).clone();
        let mut full = match resolve_name(segments, module, local_uses, &self.uses, &set) {
            Ok(r) => r?,
            // An item and its re-exports are one item, named by the
            // first path `module` can see (followed below).
            Err(hits) if hits.iter().all(|h| item(h) == item(&hits[0])) => (hits.iter())
                .find(|h| self.hidden(module, h).is_none())
                .unwrap_or(&hits[0])
                .clone(),
            Err(hits) => {
                self.error(
                    Code::AMBIGUOUS_NAME,
                    at,
                    format!(
                        "type name `{}` is ambiguous in module `{}` - multiple glob imports \
                     resolve it: {}",
                        segments.join("::"),
                        module_display(module),
                        hits.join(", ")
                    ),
                );
                hits.into_iter().next()?
            }
        };
        // Every module on the path as written (a re-export
        // key, before following it) must be visible too.
        if let Some(m) = self.hidden(module, &full) {
            self.error(
                Code::PRIVATE_ITEM,
                at,
                resolve::hidden_module_msg(&full, &m, module, Some(self.visible_path(&full))),
            );
        }
        if let Some(t) = self.reexport(&full) {
            full.clone_from(t);
        }
        if let Some(vis) = self.type_vis(&full) {
            let def_module = module_of(&full);
            if !visible_from(module, &def_module, vis) {
                let (vis, _) = resolve::needed_vis(module, &def_module);
                let msg = format!(
                    "type `{full}` is private to module `{}` and cannot be used from `{}` \
                     (add `{vis}` to expose it)",
                    module_display(&def_module),
                    module_display(module)
                );
                match resolve::own_library_privacy(&def_module) {
                    Some(help) => self.error_help(Code::PRIVATE_ITEM, at, msg, help),
                    None => self.error(Code::PRIVATE_ITEM, at, msg),
                }
            }
        }
        Some(full)
    }

    /// A bare (imported) variant name -> its `a::E::V` key. Use entries
    /// may name the enum relative to the module (`use Result::{Ok}`).
    pub(crate) fn canon_variant(
        &self,
        segments: &[&str],
        module: &str,
        local_uses: &[Vec<crate::uses::UseEntry>],
        at: Span,
    ) -> Option<String> {
        let full_key = |k: &str| -> Option<String> {
            if self.is_variant(k) {
                return Some(k.to_owned());
            }
            let (ty, v) = k.rsplit_once("::")?;
            let segs: Vec<&str> = ty.split("::").collect();
            let e = self.canon(&segs, module, local_uses, at)?;
            let key = format!("{e}::{v}");
            self.is_variant(&key).then_some(key)
        };
        let exists = |k: &str| full_key(k).is_some();
        let keys = |leaf: &str| self.variant_leaf(leaf);
        // A variant is as visible as its enum.
        let visible = |k: &str| self.visible_type(&module_of(k), module);
        let set = NameSet {
            exists: &exists,
            keys: &keys,
            visible: &visible,
            in_type: &|_| false,
        };
        match resolve_name(segments, module, local_uses, &self.uses, &set) {
            Ok(r) => full_key(&r?),
            Err(hits) => {
                self.error(
                    Code::AMBIGUOUS_NAME,
                    at,
                    format!(
                        "variant name `{}` is ambiguous in module `{}` - multiple glob imports \
                     resolve it: {}",
                        segments.join("::"),
                        module_display(module),
                        hits.join(", ")
                    ),
                );
                full_key(hits.first()?)
            }
        }
    }

    /// A `fn` type's bounds: `+ Send + Sync` (sorted auto-trait keys),
    /// `unsafe`, `extern "C"`.
    pub(crate) fn fn_bounds(
        &self,
        bounds: &nabic_ast::FnTyBounds,
        module: &str,
        at: Span,
    ) -> crate::FnBounds {
        let mut keys: Vec<String> = bounds
            .auto
            .iter()
            .filter_map(|b| {
                let segs: Vec<&str> = b.segments.iter().map(|s| s.name.as_str()).collect();
                let key = self.trait_key(&segs, module, at);
                if self.auto_traits.contains(&key) {
                    Some(key)
                } else {
                    self.error(
                        Code::NOT_A_TRAIT,
                        at,
                        format!(
                            "`{}` in a `fn` type bound is not an auto trait - only `Send` / \
                         `Sync` can bound a `fn` type",
                            segs.join("::")
                        ),
                    );
                    None
                }
            })
            .collect();
        keys.sort();
        keys.dedup();
        crate::FnBounds {
            auto: keys,
            unsafe_fn: bounds.unsafe_fn,
            c_abi: bounds.c_abi,
            stdcall: bounds.system && consteval::stdcall_system(),
        }
    }

    /// Every auto trait, sorted: what a named function (no captures)
    /// carries as a `fn` value.
    pub(crate) fn all_auto(&self) -> crate::FnBounds {
        let mut auto: Vec<String> = self.auto_traits.iter().cloned().collect();
        auto.sort();
        crate::FnBounds {
            auto,
            ..crate::FnBounds::default()
        }
    }

    /// A pre-body integer constant (array lengths, trait-path const
    /// arguments): literals, bound const parameters and module `const`s via
    /// `consteval::syntactic_int`. `None` (e.g. a `const fn` call) waits
    /// for the full evaluator.
    pub(crate) fn const_int(
        &self,
        c: &nabic_ast::ConstExpr,
        module: &str,
        param: &dyn Fn(&str) -> Option<i128>,
    ) -> Option<i128> {
        self.const_int_in(c, module, param, &std::cell::RefCell::new(Vec::new()))
    }

    pub(crate) fn const_int_in(
        &self,
        c: &nabic_ast::ConstExpr,
        module: &str,
        param: &dyn Fn(&str) -> Option<i128>,
        seen: &std::cell::RefCell<Vec<String>>,
    ) -> Option<i128> {
        consteval::syntactic_int(c, &|path| {
            if let Some(v) = param(path) {
                return Some(v);
            }
            let segs: Vec<&str> = path.split("::").collect();
            let key = self.const_key(&segs, module)?;
            if seen.borrow().contains(&key) {
                return None;
            }
            seen.borrow_mut().push(key.clone());
            let value = nabic_ast::ConstExpr::Expr(Box::new(self.name_const(&key)?.1.clone()));
            let v = self.const_int_in(&value, &module_of(&key), &|_| None, seen);
            seen.borrow_mut().pop();
            v
        })
    }

    /// The module `const` `segments` names from `module`.
    pub(crate) fn const_key(&self, segments: &[&str], module: &str) -> Option<String> {
        let exists = |k: &str| self.name_const(k).is_some();
        let keys = |leaf: &str| self.name_const_leaf(leaf);
        let visible = |_: &str| true;
        let set = NameSet {
            exists: &exists,
            keys: &keys,
            visible: &visible,
            in_type: &|k| self.has_type(k),
        };
        resolve_name(segments, module, &[], &self.uses, &set)
            .ok()
            .flatten()
            .map(|k| self.value_reexport(&k).cloned().unwrap_or(k))
    }

    pub(crate) fn trait_key(&self, segments: &[&str], module: &str, at: Span) -> String {
        self.canon(segments, module, &[], at)
            .filter(|full| self.is_trait(full))
            .unwrap_or_else(|| segments.last().copied().unwrap_or_default().to_owned())
    }

    pub(crate) fn error(&self, code: Code, at: Span, msg: String) {
        self.diags
            .borrow_mut()
            .push(nabic_diagnostic::NabiDiagnostic::error(code, at, msg));
    }

    pub(crate) fn error_help(&self, code: Code, at: Span, msg: String, help: String) {
        self.diags
            .borrow_mut()
            .push(nabic_diagnostic::NabiDiagnostic::error(code, at, msg).with_help(help));
    }
}

/// Lowering reads the name tables through these: each read is logged
/// for the incremental store (`incr::Read`).
impl TypeNames {
    /// Type-namespace key `k` names an item.
    pub(crate) fn has_type(&self, k: &str) -> bool {
        incr::read(|| incr::Read::Type(k.to_owned()));
        self.names.contains(k)
    }

    /// The type `pub use` alias `k` ends at.
    pub(crate) fn reexport(&self, k: &str) -> Option<&String> {
        incr::read(|| incr::Read::Reexport(k.to_owned()));
        self.reexports.get(k)
    }

    /// Type keys and re-exports whose last segment is `leaf`.
    pub(crate) fn type_leaf(&self, leaf: &str) -> Vec<String> {
        incr::read(|| incr::Read::TypeLeaf(leaf.to_owned()));
        self.leaves
            .get_or_init(|| leaves(self.names.iter().chain(self.reexports.keys())))
            .get(leaf)
            .cloned()
            .unwrap_or_default()
    }

    pub(crate) fn type_vis(&self, k: &str) -> Option<Visibility> {
        incr::read(|| incr::Read::TypeVis(k.to_owned()));
        self.visibility.get(k).copied()
    }

    /// `k` as users name it: its public path (`core::io::BufReader`),
    /// never one through a private module.
    pub(crate) fn visible_path<'k>(&'k self, k: &'k str) -> &'k str {
        incr::read(|| incr::Read::Visible(k.to_owned()));
        self.visible.get(k).map_or(k, String::as_str)
    }

    /// `resolve::hidden_module` over the `mod` visibilities.
    pub(crate) fn hidden(&self, cur: &str, full: &str) -> Option<String> {
        let def = module_of(full);
        for (i, _) in def.match_indices("::") {
            incr::read(|| incr::Read::ModVis(def[..i].to_owned()));
        }
        incr::read(|| incr::Read::ModVis(def.clone()));
        resolve::hidden_module(&self.mod_vis, cur, full)
    }

    /// Help for a name `cur` can't find: up to three `use` lines for items
    /// of that name (own package, library, dependencies; shortest first),
    /// or which `pub` is missing when only hidden own items match.
    pub(crate) fn import_help(
        &self,
        cur: &str,
        mut keys: Vec<String>,
        vis: impl Fn(&str) -> Option<Visibility>,
    ) -> Option<String> {
        // The shortest path the prelude gives (`task::sleep`), before the
        // spellings through it are dropped.
        let open = |k: &str| {
            self.hidden(cur, k).is_none()
                && visible_from(cur, &module_of(k), vis(k).unwrap_or(Visibility::Pub))
        };
        let write = self.prelude_write(cur, keys.iter().filter(|k| open(k)));
        let prelude = format!("{}::", resolve::PRELUDE);
        keys.retain(|k| {
            module_of(k) != cur
                && !k.contains('/')
                && !k.starts_with(&prelude)
                && !k.split("::").any(|s| s.starts_with("__"))
        });
        keys.sort();
        keys.dedup();
        let tier = |k: &str| match package_of(&module_of(k)) {
            "" => 0,
            p if nabic_core::in_toolchain(p) => 1,
            _ => 2,
        };
        keys.sort_by_key(|k| (tier(k), k.matches("::").count()));
        if let Some(w) = write {
            // The `use` of the same item the path names.
            let same = format!("::{w}");
            let import = (keys.iter().filter(|k| open(k)))
                .find(|k| k.ends_with(&same))
                .or_else(|| keys.iter().find(|k| open(k)));
            return Some(match import {
                Some(k) => format!("write `{w}`, or add `use {k};`"),
                None => format!("write `{w}`"),
            });
        }
        let lines: Vec<String> = keys
            .iter()
            .filter(|k| open(k))
            .take(3)
            .map(|k| format!("`use {};`", resolve::use_path(cur, k)))
            .collect();
        // The package's own item, when only a private one has the name.
        let private = (!keys.iter().any(|k| tier(k) == 0 && open(k)))
            .then(|| keys.iter().find(|k| tier(k) == 0))
            .flatten()
            .map(|k| {
                if let Some(m) = self.hidden(cur, k) {
                    return format!(
                        "`{k}` has that name, but module `{m}` is private: add `{}` to its `mod`",
                        resolve::needed_vis(cur, &module_of(&m)).0
                    );
                }
                let (vis, reach) = resolve::needed_vis(cur, &module_of(k));
                format!(
                    "`{k}` has that name, but it is private to module `{}`: write `{vis}` before \
                     it, {reach}, and add `use {};`",
                    module_display(&module_of(k)),
                    resolve::use_path(cur, k)
                )
            });
        let import = match lines.as_slice() {
            [] => None,
            [one] => Some(format!("add {one}")),
            _ => Some(format!("add one of {}", lines.join(", "))),
        };
        match (private, import) {
            (Some(p), Some(i)) => Some(format!("{p}; or {i}")),
            (p, i) => p.or(i),
        }
    }

    /// The shortest of `keys` as code in `cur` writes it through the
    /// prelude (`core::task::sleep`: `task::sleep`), if it applies
    /// there.
    pub(crate) fn prelude_write<'k>(
        &self,
        cur: &str,
        keys: impl Iterator<Item = &'k String>,
    ) -> Option<String> {
        let prelude = resolve::PRELUDE;
        if !matches!(implicit_use(cur), Some(crate::uses::UseEntry::Glob { prefix }) if prefix == prelude)
        {
            return None;
        }
        incr::read(|| incr::Read::Uses(prelude.to_owned()));
        let entries = self.uses.get(prelude)?;
        let entries: Vec<(&str, &str)> = (entries.iter())
            .filter_map(|e| match e {
                crate::uses::UseEntry::Single { local, full } => {
                    Some((local.as_str(), full.as_str()))
                }
                _ => None,
            })
            .collect();
        // A key spelled through the prelude (`core::prelude::task::x`)
        // stands for its own path (`core::task::x`).
        let real = |k: &str| {
            let Some(rest) = k.strip_prefix(prelude).and_then(|r| r.strip_prefix("::")) else {
                return k.to_owned();
            };
            let (head, tail) = rest.split_once("::").unwrap_or((rest, ""));
            match entries.iter().find(|(local, _)| *local == head) {
                Some((_, full)) if tail.is_empty() => (*full).to_owned(),
                Some((_, full)) => format!("{full}::{tail}"),
                None => k.to_owned(),
            }
        };
        let entries = &entries;
        keys.map(|k| real(k))
            .filter(|k| self.hidden(cur, k).is_none())
            .flat_map(|k| {
                entries.iter().filter_map(move |(local, full)| {
                    if k == *full {
                        return Some((*local).to_owned());
                    }
                    let rest = k.strip_prefix(full)?.strip_prefix("::")?;
                    Some(format!("{local}::{rest}"))
                })
            })
            .min_by_key(|p| (p.matches("::").count(), p.len(), p.clone()))
    }

    /// Value key `k` is a type's associated item (`Duration::MAX`,
    /// `f32::MAX`): no `use` names it.
    pub(crate) fn type_member(&self, k: &str) -> bool {
        let owner = module_of(k);
        self.has_type(&owner) || crate::ty::prim_named(&owner).is_some()
    }

    pub fn is_trait(&self, k: &str) -> bool {
        incr::read(|| incr::Read::Trait(k.to_owned()));
        self.traits.contains(k)
    }

    pub(crate) fn all_traits(&self) -> &nabic_core::HashSet<String> {
        incr::read(|| incr::Read::Traits);
        &self.traits
    }

    pub(crate) fn is_variant(&self, k: &str) -> bool {
        incr::read(|| incr::Read::Variant(k.to_owned()));
        self.variants.contains(k)
    }

    pub(crate) fn variant_leaf(&self, leaf: &str) -> Vec<String> {
        incr::read(|| incr::Read::VariantLeaf(leaf.to_owned()));
        self.variant_leaves
            .get_or_init(|| leaves(&self.variants))
            .get(leaf)
            .cloned()
            .unwrap_or_default()
    }

    pub(crate) fn name_const(&self, k: &str) -> Option<&(Ty, nabic_ast::Expr)> {
        incr::read(|| incr::Read::NameConst(k.to_owned()));
        self.consts.get(k)
    }

    pub(crate) fn name_const_leaf(&self, leaf: &str) -> Vec<String> {
        incr::read(|| incr::Read::NameConstLeaf(leaf.to_owned()));
        self.const_leaves
            .get_or_init(|| leaves(self.consts.keys()))
            .get(leaf)
            .cloned()
            .unwrap_or_default()
    }

    /// The `const` / `static` `pub use` alias `k` ends at.
    pub(crate) fn value_reexport(&self, k: &str) -> Option<&String> {
        incr::read(|| incr::Read::ValueReexport(k.to_owned()));
        self.value_reexports.get(k)
    }

    pub(crate) fn generic_alias(&self, k: &str) -> Option<&nabic_ast::TypeAliasDef> {
        incr::read(|| incr::Read::GenericAlias(k.to_owned()));
        self.generic_aliases.get(k)
    }

    /// Whether a generic alias named `leaf` has a default for every
    /// parameter.
    pub(crate) fn defaulted_alias_leaf(&self, leaf: &str) -> bool {
        incr::read(|| incr::Read::AnyGenericAlias);
        self.generic_aliases.iter().any(|(k, a)| {
            leaf_name(k) == leaf
                && a.generics.params.iter().all(|p| {
                    matches!(
                        p,
                        nabic_ast::GenericParam::Type {
                            default: Some(_),
                            ..
                        }
                    )
                })
        })
    }

    /// Whether any generic alias exists.
    pub(crate) fn any_generic_alias(&self) -> bool {
        incr::read(|| incr::Read::AnyGenericAlias);
        !self.generic_aliases.is_empty()
    }

    pub(crate) fn plain_alias_ty(&self, k: &str) -> Option<&Ty> {
        incr::read(|| incr::Read::PlainAlias(k.to_owned()));
        self.plain_aliases.get(k)
    }
}

/// Where a type is being resolved: the name index, the module the
/// referencing item lives in, and the location its errors go to.
#[derive(Clone, Copy)]
pub(crate) struct Scope<'b> {
    pub(crate) names: &'b TypeNames,
    pub(crate) module: &'b str,
    pub(crate) at: Span,
}

impl TypeNames {
    /// No name declared yet.
    pub(crate) fn empty() -> Self {
        Self {
            names: nabic_core::HashSet::default(),
            traits: nabic_core::HashSet::default(),
            auto_traits: nabic_core::HashSet::default(),
            unsafe_traits: nabic_core::HashSet::default(),
            lang: nabic_parser::LangTable::default(),
            variants: nabic_core::HashSet::default(),
            reexports: HashMap::default(),
            visibility: HashMap::default(),
            mod_vis: HashMap::default(),
            visible: HashMap::default(),
            uses: HashMap::default(),
            diags: std::cell::RefCell::new(Vec::new()),
            generic_aliases: HashMap::default(),
            plain_aliases: HashMap::default(),
            alias_depth: std::cell::Cell::new(0),
            broken_aliases: std::cell::RefCell::default(),
            alias_uses: std::cell::RefCell::new(Vec::new()),
            unknown_spans: std::cell::RefCell::default(),
            bare_generics: std::cell::RefCell::default(),
            open_args: std::cell::RefCell::default(),
            canon_memo: std::cell::RefCell::default(),
            leaves: std::cell::OnceCell::new(),
            variant_leaves: std::cell::OnceCell::new(),
            consts: HashMap::default(),
            const_leaves: std::cell::OnceCell::new(),
            value_reexports: HashMap::default(),
            some_types: HashMap::default(),
            some_concrete: std::cell::RefCell::default(),
            some_fields: Vec::new(),
        }
    }
}
