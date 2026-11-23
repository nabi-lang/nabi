//! Lints over HIR types: `#[must_use]`, casts and clones, deprecated types.

#[allow(clippy::wildcard_imports)]
use crate::lint::*;

/// Every function in `items` by short name: per attribute in `attrs`, its
/// message, for names no unmarked function shares.
pub(crate) fn marked_fns<const N: usize>(
    items: &[&[Item]],
    attrs: [&str; N],
) -> [HashMap<String, String>; N] {
    let mut marked: [HashMap<String, String>; N] = std::array::from_fn(|_| HashMap::default());
    for i in items {
        each_fn(i, &mut |f| {
            for (k, a) in attrs.iter().enumerate() {
                if let Some(m) = attr_msg(&f.attrs, a) {
                    marked[k].insert(leaf(&f.name).to_owned(), m);
                }
            }
        });
    }
    if marked.iter().all(HashMap::is_empty) {
        return marked;
    }
    for i in items {
        each_fn(i, &mut |f| {
            for (k, a) in attrs.iter().enumerate() {
                if !marked[k].is_empty() && !f.attrs.iter().any(|x| x.name() == *a) {
                    marked[k].remove(leaf(&f.name));
                }
            }
        });
    }
    marked
}

/// The structs and enums with an `attr` attribute, by full path: its
/// message. `files` are walked in their package and module, `library`
/// items have full names already.
pub(crate) fn marked_types(
    files: &[SourceFile],
    library: Option<&nabic_ast::Program>,
    attr: &str,
) -> HashMap<String, String> {
    fn walk<'i>(
        items: &'i [Item],
        at: &mut Vec<&'i str>,
        attr: &str,
        out: &mut HashMap<String, String>,
    ) {
        for i in items {
            let (name, attrs) = match i {
                Item::Struct(s) => (&s.name, &s.attrs),
                Item::Enum(e) => (&e.name, &e.attrs),
                Item::Mod(m) => {
                    at.push(&m.name);
                    walk(m.items.as_deref().unwrap_or(&[]), at, attr, out);
                    at.pop();
                    continue;
                }
                _ => continue,
            };
            if let Some(m) = attr_msg(attrs, attr) {
                let full: Vec<&str> = at.iter().copied().chain([name.as_str()]).collect();
                out.insert(full.join("::"), m);
            }
        }
    }
    let mut out = HashMap::default();
    for f in files {
        let mut at: Vec<&str> = std::iter::once(f.package.as_str())
            .chain(f.module.iter().map(String::as_str))
            .filter(|s| !s.is_empty())
            .collect();
        walk(&f.items, &mut at, attr, &mut out);
    }
    if let Some(p) = library {
        walk(&p.items, &mut Vec::new(), attr, &mut out);
    }
    out
}

/// The message of the `#[must_use]` type HIR names `name`: a library
/// type by its full path only, so a user type of the same leaf name is
/// not taken for it.
pub(crate) fn must_use_msg<'m>(
    marks: &'m HashMap<String, String>,
    name: &str,
) -> Option<&'m String> {
    let lib = nabic_core::in_toolchain;
    marks.get(name).or_else(|| {
        marks
            .iter()
            .find(|(k, _)| {
                if lib(k) {
                    lib(name) && k.ends_with(&format!("::{name}"))
                } else {
                    !lib(name) && leaf(k) == leaf(name)
                }
            })
            .map(|(_, m)| m)
    })
}

impl Cx<'_> {
    /// `redundant_cast` / `redundant_clone` from typed HIR, skipping generic
    /// instances (a cast to `T` isn't redundant because `T = i32` in one).
    pub(crate) fn hir(&mut self, hir: &nabic_hir::HirModule, user: &[&SourceFile]) {
        let files: HashSet<u32> = user.iter().map(|f| f.file_id).collect();
        let mut hits: Vec<(&'static str, Span, String, Option<Span>)> = Vec::new();
        let mut cloned: HashSet<(u32, u32)> = HashSet::default();
        // The package's own bodies.
        let own: Vec<&nabic_hir::HirFunction> = hir
            .functions
            .iter()
            .filter(|f| files.contains(&f.span.file) && !f.name.contains("__mono__"))
            .collect();
        // Every statement list of the bodies, for `must_use_values`.
        let mut blocks: Vec<&nabic_hir::HirBlock> = Vec::new();
        for f in &own {
            let Some(body) = &f.body else { continue };
            blocks.push(body);
            each_block(body, &mut |e| {
                if let HirExprKind::Block(b)
                | HirExprKind::Loop { body: b, .. }
                | HirExprKind::While { body: b, .. }
                | HirExprKind::If { then_block: b, .. } = &e.kind
                {
                    blocks.push(b);
                }
                if !files.contains(&e.span.file) {
                    return;
                }
                match &e.kind {
                    // An enum's cast reads its tag: `from_ty` is the tag's type.
                    HirExprKind::Cast {
                        from_ty,
                        to_ty,
                        operand,
                    } if from_ty == to_ty
                        && operand.ty == *to_ty
                        && self.casts.contains(&key(e.span)) =>
                    {
                        hits.push((
                            "redundant_cast",
                            e.span,
                            format!("casting to the same type `{}`", hir_ty(hir, to_ty)),
                            Some(operand.span),
                        ));
                    }
                    // A primitive's `clone` is the `nabi.clone.copy` intrinsic,
                    // lowered to its receiver. Match the receiver's whole
                    // span: a call on the clone (`x.clone().len()`) or an
                    // f-string hole's temps start at the same place.
                    _ if matches!(e.ty, HirTy::Prim(_))
                        && self
                            .clones
                            .contains(&(e.span.file, e.span.start, e.span.end))
                        && cloned.insert(key(e.span)) =>
                    {
                        hits.push((
                            "redundant_clone",
                            e.span,
                            format!(
                                "redundant `.clone()` of {}: the value is copied anyway",
                                nabic_diagnostic::article(&hir_ty(hir, &e.ty))
                            ),
                            None,
                        ));
                    }
                    _ => {}
                }
            });
        }
        let types: MustUse = hir
            .structs
            .iter()
            .map(|s| ((false, s.id.0), s.name.as_str()))
            .chain(hir.enums.iter().map(|e| ((true, e.id.0), e.name.as_str())))
            .filter_map(|(k, n)| {
                let base = n.split("__").next().unwrap_or(n);
                let m = must_use_msg(&self.must_use_types, base)?;
                // Without a message the warning names the type.
                if m.is_empty() {
                    let leaf = base.rsplit("::").next().unwrap_or(base);
                    return Some((k, format!("value of type `{leaf}` is never used")));
                }
                Some((k, m.clone()))
            })
            .collect();
        if !types.is_empty() {
            self.must_use_values(&types, &blocks);
        }
        for (lint, span, msg, operand) in hits {
            let fix = match operand {
                Some(o) => self.uncast(span, o),
                None => self.unclone(span),
            };
            self.hit_fix(lint, span, msg, fix);
        }
    }

    /// `x as T` at `cast` written as `x`.
    fn uncast(&self, cast: Span, operand: Span) -> Option<(Span, String, String)> {
        let (all, x) = (self.src(cast)?, self.src(operand)?);
        let rest = all.strip_prefix(x)?;
        (rest.starts_with(char::is_whitespace) && rest.trim_start().starts_with("as "))
            .then(|| (cast, x.to_owned(), format!("write `{x}`")))
    }

    /// `.clone()` after the receiver at `r`, deleted.
    fn unclone(&self, r: Span) -> Option<(Span, String, String)> {
        let call = Span {
            start: r.end,
            end: r.end + 8,
            ..r
        };
        (self.src(call)? == ".clone()")
            .then(|| (call, String::new(), "delete `.clone()`".to_owned()))
    }

    /// Unused `#[must_use]` values: a statement of the type
    /// (`unused_must_use`) and an unused `let` of it. Messages are
    /// `noun phrase: fix`, `{}` standing for the value.
    pub(crate) fn must_use_values(&mut self, types: &MustUse, blocks: &[&nabic_hir::HirBlock]) {
        let msg_of = |t: &HirTy| {
            match t {
                HirTy::Struct(id) => types.get(&(false, id.0)),
                HirTy::Enum(id) => types.get(&(true, id.0)),
                _ => None,
            }
            .cloned()
        };
        let split = |msg: &str, what: &str| match msg.split_once(": ") {
            Some((head, fix)) => (
                head.to_owned(),
                Some(format!("help: {}", fix.replace("{}", what))),
            ),
            None => (msg.to_owned(), None),
        };
        for s in blocks.iter().flat_map(|b| &b.stmts) {
            match s {
                nabic_hir::HirStmt::Expr(e) => {
                    let (Some(msg), Some(src)) = (msg_of(&e.ty), self.stmt_exprs.get(&key(e.span)))
                    else {
                        continue;
                    };
                    let (head, fix) = split(&msg, src);
                    let msg = format!("this {head}");
                    self.findings.push(Finding {
                        lint: "unused_must_use",
                        span: e.span,
                        msg,
                        notes: fix.into_iter().collect(),
                        secondary: None,
                        base: None,
                        fix: None,
                    });
                }
                nabic_hir::HirStmt::Let { init, .. } => {
                    let (Some(msg), Some((at, name))) =
                        (msg_of(&init.ty), self.let_inits.get(&key(init.span)))
                    else {
                        continue;
                    };
                    // A type without a message: the plain unused warning.
                    if msg.starts_with("value of type `") {
                        continue;
                    }
                    let name = demangle(name);
                    let (head, fix) = split(&msg, name);
                    let at = name_span(self.texts, *at, name).unwrap_or(*at);
                    let unused = self
                        .findings
                        .iter_mut()
                        .find(|f| f.lint == "unused_variables" && f.span == at);
                    if let Some(f) = unused {
                        f.msg = format!("`{name}` is never used: the {head}");
                        f.notes.splice(0..0, fix);
                    }
                }
                _ => {}
            }
        }
    }
}

/// The deprecated types and traits of every file, for the paths that name
/// them: by full path when written so, else by a last segment no other
/// type shares (never invent one).
#[derive(Default)]
pub(crate) struct TypeMarks {
    /// Full path, `type` or `trait`, the deprecation.
    pub(crate) marked: Vec<(String, &'static str, nabic_hir::marks::Deprecation)>,
    /// How many types and traits have each last segment.
    leaves: HashMap<String, usize>,
}

impl TypeMarks {
    /// `items`, in module `at` (a library's names are full already).
    pub(crate) fn add<'i>(&mut self, items: &'i [Item], at: &mut Vec<&'i str>) {
        for i in items {
            let (name, attrs, what) = match i {
                Item::Struct(s) => (&s.name, &s.attrs, "type"),
                Item::Enum(e) => (&e.name, &e.attrs, "type"),
                Item::Union(u) => (&u.name, &u.attrs, "type"),
                Item::TypeAlias(t) => (&t.name, &t.attrs, "type"),
                Item::Trait(t) => (&t.name, &t.attrs, "trait"),
                Item::Mod(m) => {
                    at.push(&m.name);
                    self.add(m.items.as_deref().unwrap_or(&[]), at);
                    at.pop();
                    continue;
                }
                _ => continue,
            };
            *self.leaves.entry(leaf(name).to_owned()).or_default() += 1;
            let d = attrs
                .iter()
                .filter(|a| a.name() == "deprecated")
                .find_map(nabic_hir::marks::deprecation);
            if let Some(d) = d {
                let full = at
                    .iter()
                    .copied()
                    .chain([name.as_str()])
                    .collect::<Vec<_>>();
                self.marked.push((full.join("::"), what, d));
            }
        }
    }

    /// A type or trait has the last segment `name`.
    pub(crate) fn has(&self, name: &str) -> bool {
        self.leaves.contains_key(name)
    }

    /// The deprecated type `segs` names.
    pub(crate) fn find(
        &self,
        segs: &[&str],
    ) -> Option<&(String, &'static str, nabic_hir::marks::Deprecation)> {
        let segs = match segs {
            ["pkg" | "sup" | "self", rest @ ..] => rest,
            s => s,
        };
        let last = *segs.last()?;
        if last == "Self" {
            return None;
        }
        let unique = self.leaves.get(last) == Some(&1);
        let path = segs.join("::");
        self.marked.iter().find(|(k, ..)| {
            leaf(k) == last
                && (unique || (segs.len() > 1 && (*k == path || k.ends_with(&format!("::{path}")))))
        })
    }
}

/// `t` as the user writes it: structs and enums by their own name (as
/// `P<f64>`), not their ids.
pub(crate) fn hir_ty(hir: &nabic_hir::HirModule, t: &HirTy) -> String {
    let list = |ts: &[HirTy]| {
        ts.iter()
            .map(|t| hir_ty(hir, t))
            .collect::<Vec<_>>()
            .join(", ")
    };
    // An instance's name is its mangled `base__args`.
    let named = |name: &str, args: &[HirTy]| {
        let leaf = |n: &str| n.rsplit("::").next().unwrap_or(n).to_owned();
        if args.is_empty() {
            return leaf(name);
        }
        let base = name.split("__").next().unwrap_or(name);
        format!("{}<{}>", leaf(base), list(args))
    };
    match t {
        HirTy::Prim(p) => p.name().to_owned(),
        HirTy::Unit => "()".to_owned(),
        HirTy::Struct(id) => hir.structs.iter().find(|s| s.id == *id).map_or_else(
            || "a struct".to_owned(),
            |s| named(&s.name, &s.generic_args),
        ),
        HirTy::Enum(id) => hir
            .enums
            .iter()
            .find(|e| e.id == *id)
            .map_or_else(|| "an enum".to_owned(), |e| named(&e.name, &e.generic_args)),
        HirTy::Tuple(parts) => format!("({})", list(parts)),
        HirTy::Array(e, n) => format!("[{}; {n}]", hir_ty(hir, e)),
        HirTy::Path(parts) => parts.join("::"),
        other => format!("{other:?}"),
    }
}
