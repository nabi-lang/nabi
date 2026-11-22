//! Diagnostics: errors, warnings, help and type text.

#[allow(clippy::wildcard_imports)]
use crate::lower::*;

/// An unknown name's help: the `use` that brings it in, else a close
/// name.
pub(crate) fn join_help(import: Option<String>, typo: Option<String>) -> Option<String> {
    // An item of exactly that name beats a close one.
    import.or(typo)
}

/// `text` with each whole occurrence of type text `ty` (not a tail of a
/// longer name) replaced by `with`.
fn replace_type_text(text: &str, ty: &str, with: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(ty) {
        let joined = rest[..at]
            .chars()
            .next_back()
            .or_else(|| out.chars().next_back())
            .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == ':');
        out.push_str(&rest[..at]);
        out.push_str(if joined { ty } else { with });
        rest = &rest[at + ty.len()..];
    }
    out.push_str(rest);
    out
}

/// A warning at `at` that belongs to `lint`, with a `help:` note.
pub(crate) fn lint_diag(
    lint: &str,
    at: Span,
    msg: String,
    help: String,
) -> nabic_diagnostic::NabiDiagnostic {
    nabic_diagnostic::NabiDiagnostic::warning(Code::LINT, at, msg)
        .with_help(help)
        .with_lint(lint)
}

impl<'a> LowerCtx<'a> {
    /// Add `help` to the last diagnostic if it is an error pushed after
    /// the sink held `since` of them.
    pub(crate) fn help_since(&self, since: usize, help: String) {
        let mut diags = self.env.sink.diagnostics.borrow_mut();
        if diags.len() > since {
            if let Some(d) = diags.last_mut().filter(|d| d.is_error()) {
                d.notes.push(format!("help: {help}"));
            }
        }
    }

    /// Push a lowering-time diagnostic onto the shared sink.
    pub fn emit_error(&self, code: Code, msg: String) {
        self.emit_error_at(code, self.here(), msg);
    }

    /// An error at `at`.
    pub(crate) fn emit_error_at(&self, code: Code, at: Span, msg: String) {
        self.emit_diag(nabic_diagnostic::NabiDiagnostic::error(code, at, msg));
    }

    /// An error built by the caller.
    pub(crate) fn emit_diag(&self, mut d: nabic_diagnostic::NabiDiagnostic) {
        if bounds::in_failed(self.env) {
            let len = self.env.sink.diagnostics.borrow().len();
            self.env.sink.dropped_at.set(Some(len));
            return;
        }
        let sites = self.env.sink.call_sites.borrow();
        if let Some((callee, at, _)) = sites.iter().find(|s| s.2) {
            d = d.with_secondary(*at, format!("in this call of `{}`", leaf_name(callee)));
        }
        drop(sites);
        self.name_aliases(&mut d);
        self.env.sink.dropped_at.set(None);
        self.env.sink.diagnostics.borrow_mut().push(d);
    }

    /// Remember that alias `written` named `ty` (see `name_aliases`).
    pub(crate) fn note_alias(&self, written: &nabic_ast::Ty, ty: &HirTy, module: &str) {
        let generic = match ty {
            HirTy::Struct(id) => !self.struct_def(*id).generic_args.is_empty(),
            HirTy::Enum(id) => !self.enum_def(*id).generic_args.is_empty(),
            _ => false,
        };
        if !generic || crate::lower::resolve::is_toolchain_module(module) {
            return;
        }
        let shown = self.ty_name(ty);
        let pkg = crate::lower::resolve::package_of(module).to_owned();
        let alias = crate::lower::collect::impl_def_display(written);
        let alias = match alias.strip_prefix("pkg::") {
            Some(rest) => format!("{pkg}::{rest}"),
            None => alias,
        };
        if shown == alias {
            return;
        }
        // The package being built (`""`) wins over its dependencies;
        // otherwise the first alias does.
        let mut names = self.env.sink.alias_names.borrow_mut();
        match names.iter_mut().find(|(t, _, _)| *t == shown) {
            Some(e) if pkg.is_empty() && !e.2.is_empty() => *e = (shown, alias, pkg),
            Some(_) => {}
            None => names.push((shown, alias, pkg)),
        }
    }

    /// A type an alias named, in a diagnostic's text, by that alias.
    fn name_aliases(&self, d: &mut nabic_diagnostic::NabiDiagnostic) {
        let names = self.env.sink.alias_names.borrow();
        if names.is_empty() {
            return;
        }
        let swap = |text: &mut String| {
            for (shown, alias, _) in names.iter() {
                *text = replace_type_text(text, shown, alias);
            }
        };
        swap(&mut d.message);
        d.notes.iter_mut().for_each(swap);
    }

    /// An error at `at` with a `help:` note.
    pub(crate) fn emit_error_help(&self, code: Code, at: Span, msg: String, help: String) {
        self.emit_error_at(code, at, msg);
        self.help_last(Some(help));
    }

    /// A warning. In a generic instance it is held and reported only if
    /// every instance raised it: a `rethrows` instance with a non-throwing
    /// argument would otherwise flag every `try`.
    pub(crate) fn emit_warning(&self, code: Code, msg: String) {
        self.push_warning(nabic_diagnostic::NabiDiagnostic::warning(
            code,
            self.here(),
            msg,
        ));
    }

    /// A warning at `at` that belongs to `lint`, so `-A lint` and
    /// `#[allow(lint)]` silence it.
    pub(crate) fn emit_lint(&self, lint: &str, at: Span, msg: String, help: String) {
        self.push_warning(lint_diag(lint, at, msg, help));
    }

    pub(crate) fn push_warning(&self, d: nabic_diagnostic::NabiDiagnostic) {
        if self.ty_subst.is_empty() {
            self.env.sink.diagnostics.borrow_mut().push(d);
            return;
        }
        let Some(Some((template, id))) = self.env.sink.instances.borrow().last().copied() else {
            return;
        };
        self.env
            .sink
            .instance_warns
            .borrow_mut()
            .warn(template, d, id);
    }

    /// Report a type error and return an `Unknown` placeholder; checks skip
    /// `Unknown`, so one cause gives one diagnostic.
    pub(crate) fn error_expr(
        &self,
        code: Code,
        span: nabic_diagnostic::Span,
        msg: String,
    ) -> HirExpr {
        self.emit_error(code, msg);
        poison(span)
    }

    /// An unknown name, with an optional hint.
    pub(crate) fn unknown_name(
        &self,
        code: Code,
        span: nabic_diagnostic::Span,
        msg: String,
        help: Option<String>,
    ) -> HirExpr {
        self.emit_error(code, msg);
        // A name a meta template wrote is looked up where the template is.
        let help = match self.access.borrow().as_deref() {
            Some(m) => Some(format!(
                "the meta template that wrote this name looks it up in module `{}`; to look \
                 it up where the meta block is used, write `@use_site(..)` around it",
                module_display(m)
            )),
            None => help,
        };
        self.help_last(help);
        poison(span)
    }

    /// The diagnostic just emitted, unless `emit_diag` dropped it.
    pub(crate) fn with_last(&self, f: impl FnOnce(&mut nabic_diagnostic::NabiDiagnostic)) {
        let mut diags = self.env.sink.diagnostics.borrow_mut();
        if self.env.sink.dropped_at.get() == Some(diags.len()) {
            return;
        }
        if let Some(d) = diags.last_mut() {
            f(d);
        }
    }

    /// Attach `help` to the diagnostic just emitted.
    pub fn help_last(&self, help: Option<String>) {
        if let Some(h) = help {
            self.with_last(|d| d.notes.push(format!("help: {h}")));
        }
    }

    /// Attach a fix (replace `span` with `text`) to the diagnostic just
    /// emitted.
    pub(crate) fn fix_last(&self, span: Span, text: impl Into<String>, title: impl Into<String>) {
        let fix = nabic_diagnostic::Fix {
            span,
            text: text.into(),
            title: title.into(),
        };
        self.with_last(|d| d.fixes.push(fix));
    }

    /// Tag the diagnostic just emitted.
    pub fn tag_last(&self, tag: nabic_diagnostic::Tag) {
        self.with_last(|d| d.tag = tag);
    }

    /// "struct `S` has no field `x`" plus the closest field name.
    pub(crate) fn no_field(
        &self,
        sid: StructId,
        name: &str,
        span: nabic_diagnostic::Span,
    ) -> HirExpr {
        let def = self.struct_def(sid);
        let fields: Vec<&str> = def
            .fields
            .iter()
            .map(|f| f.name.as_str())
            .chain(
                def.union_kind
                    .iter()
                    .flat_map(|u| u.aliases.iter().map(|(n, _)| n.as_str())),
            )
            .filter(|n| !n.starts_with("__") && self.field_hidden(sid, n).is_none())
            .collect();
        let kind = if def.union_kind.is_some() {
            "union"
        } else {
            "struct"
        };
        let e = self.error_expr(
            Code::NO_FIELD,
            span,
            format!(
                "{kind} `{}` has no field `{name}`",
                self.ty_name(&HirTy::Struct(sid))
            ),
        );
        // `xs.length` / `.size` / `.count` (JS, Java, C#): the `len` method.
        let recv = self
            .src(span)
            .and_then(|t| t.strip_suffix(name)?.strip_suffix('.'));
        let len = suggest::is_synonym(name, "len")
            && matches!(
                expr::pick_method(self, &HirTy::Struct(sid), "len", None),
                Ok(Some(_))
            );
        let help = recv
            .filter(|_| len)
            .map(|r| {
                self.fix_last(span, format!("{r}.len()"), "call `len`");
                format!("the length is a method: `{r}.len()`")
            })
            .or_else(|| {
                nabic_diagnostic::suggest(name, fields.iter().copied())
                    .map(|s| format!("did you mean `{s}`?"))
            })
            .or_else(|| {
                (!fields.is_empty() && fields.len() <= 8).then(|| {
                    let list: Vec<String> = fields.iter().map(|f| format!("`{f}`")).collect();
                    format!("available fields: {}", list.join(", "))
                })
            });
        self.help_last(help);
        e
    }

    /// "did you mean" for an unknown type path: types of the current
    /// module and the primitives.
    pub(crate) fn type_help(&self, segs: &[&str]) -> Option<String> {
        if let Some(h) = habits::path_help(&self.current_module, segs) {
            return Some(h);
        }
        if let Some(h) = self.tier_help(segs, false) {
            return Some(h);
        }
        let name = segs.last()?;
        if let (1, Some(h)) = (segs.len(), wide_int_help(name)) {
            return Some(h);
        }
        let module = self.current_module.as_str();
        let defs = &self.env.defs;
        let (structs, enums) = (defs.struct_table.borrow(), defs.enum_table.borrow());
        let types = structs
            .keys()
            .chain(enums.keys())
            .chain(defs.generic_structs.keys())
            .chain(defs.generic_unions.keys())
            .chain(defs.generic_enums.keys())
            .filter_map(|k| {
                let (m, n) = k.rsplit_once("::").unwrap_or(("", k.as_str()));
                (m == module).then_some(n)
            })
            .chain(PRIM_NAMES.iter().copied());
        let typo = nabic_diagnostic::suggest(name, types).map(|s| format!("did you mean `{s}`?"));
        let import = (segs.len() == 1).then(|| {
            let names = &self.env.names;
            names.import_help(module, names.type_leaf(name), |k| names.type_vis(k))
        });
        join_help(import.flatten(), typo)
    }

    /// "did you mean" for an unknown value path (one segment): locals in
    /// scope, then functions / constants / statics of the current module.
    pub(crate) fn value_help(&self, segs: &[&str]) -> Option<String> {
        let [name] = segs else {
            if let Some(h) = habits::path_help(&self.current_module, segs) {
                return Some(h);
            }
            if let Some(h) = self.tier_help(segs, true) {
                return Some(h);
            }
            if let Some(h) = segs
                .first()
                .filter(|_| segs.len() == 2)
                .and_then(|t| wide_int_help(t))
            {
                return Some(h);
            }
            // `T::f` with `T` a type not imported.
            let names = &self.env.names;
            let cur = self.current_module.as_str();
            return self.import_help(segs).or_else(|| match segs {
                [ty, member] => {
                    let keys = names.type_leaf(ty);
                    let open = keys.iter().filter(|k| names.hidden(cur, k).is_none());
                    match names.prelude_write(cur, open) {
                        Some(w) => Some(format!("write `{w}::{member}`")),
                        None => names.import_help(cur, keys, |k| names.type_vis(k)),
                    }
                }
                _ => None,
            });
        };
        let name = *name;
        if let Some(k) = match name {
            "null" | "nil" | "None" | "NULL" => Some("none"),
            "True" => Some("true"),
            "False" => Some("false"),
            _ => None,
        } {
            return Some(format!("use `{k}`"));
        }
        if name == "pass" {
            return Some(
                "Nabi needs no `pass`: an empty block `{}` is complete; delete it".to_owned(),
            );
        }
        let module = self.current_module.as_str();
        let in_module = |k: &'a String| {
            let (m, n) = k.rsplit_once("::").unwrap_or(("", k.as_str()));
            (m == module).then_some(n)
        };
        let locals = self
            .scopes
            .iter()
            .flat_map(|s| s.keys().map(String::as_str));
        let all = || {
            self.env
                .fn_table
                .keys()
                .chain(self.env.generic_fns.keys())
                .chain(self.env.consts.keys())
                .chain(self.env.statics.keys())
        };
        let public = all()
            .filter(|k| self.env.item_vis(k).unwrap_or(Visibility::Pub) == Visibility::Pub)
            .map(String::as_str);
        if let Some(help) = resolve::own_library_import(name, public) {
            return Some(help);
        }
        let mut keys = [
            self.env.fn_leaf(name),
            self.env.value_leaf(true, name),
            self.env.value_leaf(false, name),
        ]
        .concat();
        keys.retain(|k| !self.env.names.type_member(k));
        let import = self
            .env
            .names
            .import_help(module, keys, |k| self.env.item_vis(k));
        let items = all().filter_map(in_module);
        let typo = nabic_diagnostic::suggest(name, locals.chain(items))
            .map(|s| format!("did you mean `{s}`?"));
        join_help(import, typo)
    }

    /// A bare `name` that is only some enum's variant, with no expected
    /// type to pick it: the error and its help, naming the
    /// variant by its shortest path here.
    pub(crate) fn variant_not_in_scope(&self, name: &str) -> Option<(String, String)> {
        // Another language's spelling has its own help (`none`).
        if matches!(
            name,
            "null" | "nil" | "None" | "NULL" | "True" | "False" | "Some"
        ) {
            return None;
        }
        let names = &self.env.names;
        let cur = self.current_module.as_str();
        let write = |e: &String| {
            if self.canon_type(&[leaf_name(e)]).as_ref() == Some(e) {
                return leaf_name(e).to_owned();
            }
            names
                .prelude_write(cur, std::iter::once(e))
                .unwrap_or_else(|| resolve::use_path(cur, e))
        };
        let mut paths: Vec<String> = names
            .variant_leaf(name)
            .iter()
            .map(|k| module_of(k))
            .filter(|e| {
                names.hidden(cur, e).is_none()
                    && names
                        .type_vis(e)
                        .is_none_or(|v| visible_from(cur, &module_of(e), v))
            })
            .map(|e| format!("{}::{name}", write(&e)))
            .collect();
        paths.sort_by_key(|p| (p.matches("::").count(), p.len(), p.clone()));
        paths.dedup();
        let first = paths.first()?;
        let msg = format!("`{name}` is not in scope: it is the variant `{first}`");
        let picks: Vec<String> = paths.iter().take(3).map(|p| format!("`{p}`")).collect();
        let help = format!("write {}", picks.join(" or "));
        Some((msg, help))
    }

    /// `m::f` naming an item of a module `m` that is not imported: the
    /// `use` that brings `m` in.
    /// `std::time::Instant::now` for an item another toolchain root has:
    /// write `core::time::Instant::now`. A `value` path may end in a
    /// member of a type there.
    pub(crate) fn tier_help(&self, segs: &[&str], value: bool) -> Option<String> {
        let (root, rest) = segs.split_first()?;
        if rest.is_empty() || !nabic_core::TIERS.contains(root) {
            return None;
        }
        let names = &self.env.names;
        let cur = self.current_module.as_str();
        let open = |full: &str| names.hidden(cur, full).is_none();
        let found = nabic_core::TIERS
            .iter()
            .filter(|t| *t != root)
            .map(|t| [&[*t][..], rest].concat())
            .find(|cand| {
                let ty = |c: &[&str]| self.canon_type(c).is_some_and(|k| open(&k));
                match cand.split_last() {
                    _ if !value => ty(cand),
                    Some((_, owner)) if owner.len() > 1 && ty(owner) => true,
                    _ => self.resolve_any_fn(cand).is_some_and(|k| open(&k)),
                }
            })?;
        Some(format!(
            "`{}` is not in `{root}`: write `{}`",
            segs.join("::"),
            found.join("::")
        ))
    }

    pub(crate) fn import_help(&self, segs: &[&str]) -> Option<String> {
        let module = self.access_module();
        if let Some(h) = (segs.len() > 1)
            .then(|| crate::lower::foreign_help(&module, segs[0]))
            .flatten()
        {
            return Some(h);
        }
        let tail = format!("::{}", segs.join("::"));
        let env = self.env;
        let found = env
            .fn_table
            .keys()
            .chain(env.generic_fns.keys())
            .chain(env.consts.keys())
            .chain(env.statics.keys())
            .filter_map(|k| k.strip_suffix(&tail))
            .min()?;
        Some(format!(
            "add `use {found}::{};` at the top of the file",
            segs[0]
        ))
    }

    /// How many of `args` (an instance of template `base`) a name shows:
    /// trailing arguments equal to their parameter's default are left
    /// out (`Dict<str, i32>`, not `Dict<str, i32, RandomState>`).
    pub(crate) fn shown_args(&self, base: &str, args: &[HirTy]) -> usize {
        let Some(g) = self.env.defs.generics_of(base) else {
            return args.len();
        };
        let mut n = args.len().min(g.params.len());
        while n > 0 {
            let nabic_ast::GenericParam::Type {
                default: Some(d), ..
            } = &g.params[n - 1]
            else {
                break;
            };
            let subst = mono::generic_subst(g, &args[..n - 1]);
            let mut rcx = LowerCtx::new(self.env, &subst, None, self.here());
            rcx.current_module = module_of(base);
            // An open literal argument shows as the type it holds.
            if rcx.resolve_ast_ty(d) != infer::zonk(self, &args[n - 1]) {
                break;
            }
            n -= 1;
        }
        n
    }

    /// User-facing type name (no internal `Struct0` / mangled names).
    pub fn ty_name(&self, ty: &HirTy) -> String {
        self.ty_text(ty, false)
    }

    /// `ty` as source that names it from any module: library types by
    /// their full path (`core::io::IoError`, not `IoError`), for
    /// generated items compiled in a scope that may not import them.
    pub(crate) fn ty_path(&self, ty: &HirTy) -> String {
        self.ty_text(ty, true)
    }

    pub(crate) fn ty_text(&self, ty: &HirTy, full: bool) -> String {
        // A const parameter's stand-in by its name (never in a path:
        // generated items are concrete).
        if let Some(n) = ty
            .as_const()
            .filter(|_| !full)
            .and_then(|(v, _)| bounds::const_name(self.env, v))
        {
            return n;
        }
        if let Some((v, p)) = ty.as_const() {
            return match u32::try_from(v).ok().and_then(char::from_u32) {
                Some(c) if p == Some(PrimTy::Char) => format!("{c:?}"),
                _ => v.to_string(),
            };
        }
        match ty {
            HirTy::GcBuf(t) => format!("GcBuf<{}>", self.ty_text(t, full)),
            HirTy::Never => "!".to_owned(),
            HirTy::Struct(_) | HirTy::Enum(_) => {
                let (name, generic_args) = match ty {
                    HirTy::Struct(id) => {
                        let d = self.struct_def(*id);
                        (d.name, d.generic_args)
                    }
                    HirTy::Enum(id) => {
                        let d = self.enum_def(*id);
                        (d.name, d.generic_args)
                    }
                    _ => unreachable!("matched struct / enum"),
                };
                if name == self.lang("int_type") {
                    return "int".to_owned();
                }
                if name == self.lang("float_type") {
                    return "float".to_owned();
                }
                if name == self.lang("f16_type") {
                    return "f16".to_owned();
                }
                if name == self.lang("bf16_type") {
                    return "bf16".to_owned();
                }
                let full_base = name.split("__").next().unwrap_or(&name).to_owned();
                // A library type by its name (`List<i32>`, not
                // `core::collections::list::List<i32>`).
                let base = name.split("__").next().unwrap_or(&name);
                let name = if full {
                    name.clone()
                } else if nabic_core::in_toolchain(&name) {
                    leaf_name(base).to_owned() + &name[base.len()..]
                } else {
                    // A type of a private module by its public re-export
                    // (`r::Iter`, not `r::iter::Iter`).
                    self.env.names.visible_path(base).to_owned() + &name[base.len()..]
                };
                if generic_args.is_empty() {
                    name
                } else {
                    let base = name.split("__").next().unwrap_or(&name).to_owned();
                    let shown = self.shown_args(&full_base, &generic_args);
                    if shown == 0 {
                        return base;
                    }
                    let args: Vec<String> = generic_args[..shown]
                        .iter()
                        .map(|a| self.ty_text(a, full))
                        .collect();
                    format!("{base}<{}>", args.join(", "))
                }
            }
            HirTy::Prim(p) => p.name().to_owned(),
            HirTy::Unit => "()".to_owned(),
            HirTy::Any => "any".to_owned(),
            HirTy::AnyTrait(b) if b.is_empty() => "any".to_owned(),
            HirTy::AnyTrait(b) if b.len() == 1 => format!("any {}", self.trait_text(&b[0], full)),
            HirTy::AnyTrait(b) => {
                let b: Vec<String> = b.iter().map(|r| self.trait_text(r, full)).collect();
                format!("any ({})", b.join(" + "))
            }
            HirTy::Unknown => "{unknown}".to_owned(),
            HirTy::Var(_) => match infer::zonk(self, ty) {
                HirTy::Var(_) => "_".to_owned(),
                t => self.ty_text(&t, full),
            },
            HirTy::Param(p) => self.env.sink.params.borrow()[*p as usize].name.clone(),
            HirTy::Path(p) if full => p.join("::"),
            HirTy::Path(p) => self.env.names.visible_path(&p.join("::")).to_owned(),
            HirTy::None => "none".to_owned(),
            HirTy::OpenConst(n) => format!("#{n}"),
            HirTy::NoThrow => "#nothrow".to_owned(),
            HirTy::Const(v, _) => v.to_string(),
            HirTy::Tuple(parts) => {
                let inner: Vec<String> = parts.iter().map(|t| self.ty_text(t, full)).collect();
                format!("({})", inner.join(", "))
            }
            HirTy::Sum(_) => ty.sum_name(|t| self.ty_text(t, full)),
            HirTy::Array(e, n) => format!(
                "[{}; {}]",
                self.ty_text(e, full),
                bounds::const_name(self.env, i128::from(*n))
                    .filter(|_| !full)
                    .unwrap_or_else(|| n.to_string())
            ),
            HirTy::Fn(args, ret, _) | HirTy::Closure { args, ret, .. } => {
                let inner: Vec<String> = args.iter().map(|t| self.ty_text(t, full)).collect();
                let kw = if matches!(ty, HirTy::Fn(..)) {
                    "fn"
                } else {
                    "closure"
                };
                let ret = match &**ret {
                    HirTy::Throws { .. } => self.ty_text(ret, full),
                    r => format!("-> {}", self.ty_text(r, full)),
                };
                let (bounds, kw): (String, &str) = match ty {
                    HirTy::Fn(_, _, b) => (
                        b.auto
                            .iter()
                            .flat_map(|k| [" + ", if full { k } else { leaf_name(k) }])
                            .collect(),
                        match (b.unsafe_fn, b.c_abi, b.stdcall) {
                            (true, true, true) => "unsafe extern \"system\" fn",
                            (false, true, true) => "extern \"system\" fn",
                            (true, true, false) => "unsafe extern \"C\" fn",
                            (true, false, _) => "unsafe fn",
                            (false, true, false) => "extern \"C\" fn",
                            (false, false, _) => kw,
                        },
                    ),
                    _ => (String::new(), kw),
                };
                format!("{kw}({}) {ret}{bounds}", inner.join(", "))
            }
            HirTy::Throws { ok, err } => {
                format!(
                    "throws ({}) -> {}",
                    self.ty_text(err, full),
                    self.ty_text(ok, full)
                )
            }
        }
    }

    /// The sum of `a` and `b` as a slot annotation would write it: members
    /// of both, each once (`Resp | none`, not `none | Resp | none`).
    pub(crate) fn sum_hint(&self, a: &HirTy, b: &HirTy) -> String {
        let members = [a, b].into_iter().flat_map(|t| match t {
            HirTy::Sum(ps) => ps.clone(),
            // An open literal by the type it shows (`i64`).
            t => vec![crate::lower::infer::literal_default(self, t).unwrap_or_else(|| t.clone())],
        });
        self.ty_name(&HirTy::layered(members.collect(), Vec::new()))
    }

    /// `Path<A, Name = B>` for diagnostics.
    pub fn trait_name(&self, r: &TraitRef) -> String {
        self.trait_text(r, false)
    }

    /// `trait_name`, or by its full path (`full`, see `ty_path`).
    pub(crate) fn trait_text(&self, r: &TraitRef, full: bool) -> String {
        if full {
            return r.render(&r.path, |t| self.ty_text(t, true));
        }
        r.render(&self.display_path(&r.path), |t| self.ty_name(t))
    }

    /// A library item as written: `List::sort` for
    /// `core::collections::list::List__sort`, the leaf when no other
    /// function shares it, else the shortest suffix of its public path
    /// naming it alone (`thread::spawn`). Other paths are kept.
    pub fn display_path(&self, full: &str) -> String {
        // A generic impl's method `__impl1f2e3d4c_show` (or `_2_show`).
        if let Some(rest) = leaf_name(full).strip_prefix("__impl") {
            let rest = rest.get(8..).unwrap_or_default();
            let rest = rest.trim_start_matches(|c: char| c == '_' || c.is_ascii_digit());
            if !rest.is_empty() {
                return rest.to_owned();
            }
        }
        if !nabic_core::in_toolchain(full) {
            return full.to_owned();
        }
        let leaf = leaf_name(full);
        if let Some((ty, method)) = leaf.split_once("__") {
            return format!("{ty}::{method}");
        }
        let others = self.env.fn_leaf(leaf);
        let names = &self.env.names;
        let shown = names.visible_path(full);
        let segs: Vec<&str> = shown.split("::").collect();
        (1..segs.len())
            .map(|n| segs[segs.len() - n..].join("::"))
            .find(|s| {
                let tail = format!("::{s}");
                // The prelude's re-exports and the other paths of the item
                // name the same item.
                others.iter().all(|o| {
                    o == full
                        || !o.ends_with(&tail)
                        || o.starts_with(&format!("{}::", resolve::PRELUDE))
                        || names.visible_path(o) == shown
                })
            })
            .unwrap_or_else(|| shown.to_owned())
    }
}

/// Primitive type names, for "did you mean" hints.
pub(crate) const PRIM_NAMES: &[&str] = &[
    "i8", "i16", "i32", "i64", "isize", "u8", "u16", "u32", "u64", "usize", "f32", "f64", "bool",
    "char", "str", "int", "float", "f16", "bf16",
];

/// The 64-bit type to suggest for a 128-bit integer name, which Nabi
/// does not have.
pub(crate) fn wide_int_narrow(name: &str) -> Option<&'static str> {
    match name {
        "i128" => Some("i64"),
        "u128" => Some("u64"),
        _ => None,
    }
}

/// Help for a 128-bit integer type name.
pub(crate) fn wide_int_help(name: &str) -> Option<String> {
    let narrow = wide_int_narrow(name)?;
    Some(format!(
        "Nabi has no `{name}`; use `int` for integers of any size, or `{narrow}`"
    ))
}
