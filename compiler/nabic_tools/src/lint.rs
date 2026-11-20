//! `nabi lint`: style and common-mistake checks over a
//! type-checked package (no codegen).
//!
//! Most lints walk the parsed AST; `redundant_cast` / `redundant_clone`
//! read HIR types. Lowering reports uses of marked functions, fields,
//! consts and statics (`deprecated`, `deprecated_safe`) as warnings that
//! carry the lint; deprecated types are matched by path here. Levels:
//! each lint's default, then `-A/-W/-D name` (later wins; `nabi` prepends
//! `nabi.nabion` `lints`), then
//! `#[allow/warn/deny]` on an enclosing item, `mod` or `#![..]`
//! (innermost wins).
//!
//! Items only have start spans, so an attribute's scope runs to the next
//! sibling's start. Name-based lints match the last path segment, so a
//! same-named item elsewhere can hide a finding (never invent one).
//! Capitalized names aren't tracked as locals (a pattern `Foo` may be a
//! constant).

mod docs;
mod hir;
mod naming;
mod scope;
mod table;
mod unused;

pub(crate) use crate::lint::docs::*;
pub(crate) use crate::lint::hir::*;
pub(crate) use crate::lint::scope::*;
pub use crate::lint::table::*;
pub use nabic_core::lint::Level;

use crate::analysis::each_block;
use nabic_ast::{
    AccessTail, AttrArg, AttrBody, Attribute, Block, CallArg, CaptureSpec, Expr, ExprKind,
    ExternItem, FnDef, ImplItem, Item, Literal, Pattern, PatternKind, Stmt, StmtKind, TraitItem,
    TryKind, Ty, TyKind, UseTree, UseTreeKind,
};
use nabic_core::{HashMap, HashSet, Visibility};
use nabic_diagnostic::{Code, Fix, NabiDiagnostic, Severity, Span};
use nabic_hir::walk::{AstVisitor, walk_fn, walk_item, walk_stmt};
use nabic_hir::{HirExprKind, HirTy};
use nabic_interface::{CompileResult, SourceFile};
use nabic_lexer::TokenKind;

/// A lint hit before its level is known.
pub(crate) struct Finding {
    lint: &'static str,
    span: Span,
    msg: String,
    notes: Vec<String>,
    secondary: Option<(Span, String)>,
    /// A compiler warning that carries the lint: it keeps its labels,
    /// notes and fixes.
    base: Option<Box<NabiDiagnostic>>,
    /// The edit that resolves it, safe to apply unseen (`nabi fix`).
    fix: Option<Box<Fix>>,
}

/// A macro or meta template renames its own locals with a hygiene suffix
/// (`x__mac` / `x__meta`); the user never wrote it, so strip it before a name
/// reaches a diagnostic.
fn demangle(name: &str) -> &str {
    for suf in ["__mac", "__meta"] {
        if let Some(base) = name.strip_suffix(suf) {
            if !base.is_empty() {
                return base;
            }
        }
    }
    name
}

/// A name a meta template generated (`@variant.pattern` binds payload
/// field `n` of `V` as `V__meta<n>`): nobody wrote it, so no lint names it.
fn generated(name: &str) -> bool {
    name.rsplit_once("__meta").is_some_and(|(base, n)| {
        !base.is_empty() && !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())
    })
}

/// Lint a front-end result (`Session::set_keep_ast`).
///
/// `levels` are the manifest's and the command line's `-A/-W/-D`.
/// Returns compile warnings (those that carry a lint are leveled as that
/// lint), then findings as warnings or (deny) errors.
#[must_use]
pub fn run(result: &CompileResult, levels: &Levels) -> Vec<NabiDiagnostic> {
    let mut out = Vec::new();
    let mut base = defaults(levels.library);
    for (i, (lvl, name)) in levels.flags.iter().enumerate() {
        let (from, at) = match &levels.manifest {
            Some((path, n)) if i < *n => (
                format!("`\"{name}\": \"{}\"` in `lints` of {path}", lvl.name()),
                format!(" in `lints` of {path}"),
            ),
            _ => (
                format!("`{} {name}` on the command line", lvl.flag()),
                " on the command line".to_owned(),
            ),
        };
        match expand(name) {
            Some(ls) => {
                for l in ls {
                    if let Some(sets) = base.get_mut(l) {
                        sets.push((*lvl, Some(from.clone())));
                    }
                }
            }
            None => out.push(NabiDiagnostic::warning(
                Code::LINT,
                Span::DUMMY,
                unknown(name, &at),
            )),
        }
    }

    let texts = result.source_map.files();
    // A prebuilt library's build: its files are the dependency it is
    // mounted as.
    let own = result
        .modules
        .first()
        .and_then(|m| m.interface.as_ref())
        .map_or("", |i| i.name.as_str());
    let user: Vec<&SourceFile> = result.sources.iter().filter(|f| f.package == own).collect();
    let mut cx = Cx::new(&result.sources, result.library, texts);
    let mut regions = Vec::new();
    nabic_diagnostic::time("lints", || {
        for f in &user {
            regions.extend(file_regions(f, &user, &mut out));
            cx.file(f);
            cx.sup_paths(f);
        }
        cx.dead_code(&user);
        plain_module_doc(&user, own, texts, &mut cx.findings);
        // The library's own files with `--check-library`.
        let library = result
            .sources
            .iter()
            .filter(|f| levels.library && nabic_core::in_toolchain(&f.package));
        for items in user.iter().copied().chain(library).map(|f| &f.items) {
            missing_docs(items, &mut cx.findings);
        }
        if let Some(p) = result.library.filter(|_| levels.library) {
            missing_docs(&p.items, &mut cx.findings);
        }
    });
    if let Some(m) = result.modules.first() {
        nabic_diagnostic::time("lints hir", || cx.hir(&m.hir, &user));
    }
    for w in &result.warnings {
        let lint = w
            .lint
            .as_deref()
            .and_then(|n| LINTS.iter().find(|l| l.name == n));
        match lint {
            Some(l) => cx.findings.push(Finding {
                lint: l.name,
                span: w.primary.as_ref().map_or(Span::DUMMY, |p| p.span),
                msg: w.message.clone(),
                notes: Vec::new(),
                secondary: None,
                base: Some(Box::new(w.clone())),
                fix: None,
            }),
            None => out.push(w.clone()),
        }
    }

    let mut findings = cx.findings;
    findings.sort_by_key(|f| (f.span.file, f.span.start, f.lint));
    findings.dedup_by(|a, b| a.lint == b.lint && a.span == b.span);
    // A dropped `#[must_use]` value or `try?` error has its own lint.
    let stronger: HashSet<(u32, u32)> = findings
        .iter()
        .filter(|f| matches!(f.lint, "unused_must_use" | "ignored_error"))
        .map(|f| key(f.span))
        .collect();
    findings.retain(|f| f.lint != "unused_result" || !stronger.contains(&key(f.span)));
    let files: HashSet<u32> = user.iter().map(|f| f.file_id).collect();
    for f in findings {
        // Code another package's template wrote is linted in that
        // package, not where it expands. `--check-library` lints the
        // library too.
        if !levels.library && !f.span.is_dummy() && !files.contains(&f.span.file) {
            continue;
        }
        let generated = text_at(texts, f.span).is_some_and(|t| t.starts_with('#'));
        if generated {
            continue;
        }
        let (mut level, mut from) = base[f.lint]
            .last()
            .map(|s| (s.0, s.1.clone()))
            .expect("the default level is always set");
        let mut at = None;
        for r in &regions {
            if r.file == f.span.file && r.start <= f.span.start && f.span.start < r.end {
                for &(lvl, name, span) in &r.levels {
                    if name == f.lint {
                        level = lvl;
                        at = Some((span, lvl));
                        from = None;
                    }
                }
            }
        }
        if level == Level::Allow {
            continue;
        }
        let severity = if level == Level::Deny {
            Severity::Error
        } else {
            Severity::Warning
        };
        let mut d = match f.base {
            Some(b) => NabiDiagnostic {
                severity,
                code: Code::LINT,
                ..*b
            },
            None => NabiDiagnostic {
                severity,
                tag: if f.lint == "deprecated" {
                    nabic_diagnostic::Tag::Deprecated
                } else {
                    nabic_diagnostic::Tag::None
                },
                ..NabiDiagnostic::error(Code::LINT, f.span, f.msg).with_lint(f.lint)
            },
        };
        if let Some((span, msg)) = f.secondary {
            d = d.with_secondary(span, msg);
        }
        if let Some(mut fix) = f.fix {
            fix.span = located(texts, fix.span).unwrap_or(fix.span);
            d.fixes.push(*fix);
        }
        for n in f.notes {
            d = d.with_note(n);
        }
        d = match (at, from) {
            (Some((span, lvl)), _) => {
                d.with_secondary(span, format!("`#[{}({})]` set here", lvl.name(), f.lint))
            }
            (None, Some(from)) => d.with_note(from),
            (None, None) => d.with_note(format!("`#[{}({})]` on by default", level.name(), f.lint)),
        };
        out.push(d);
    }
    out
}

/// `span` with the line and column of its start, which a fix built from
/// another span's does not have.
fn located(texts: &[(String, String)], span: Span) -> Option<Span> {
    let (_, text) = texts.get(usize::try_from(span.file).ok()?.checked_sub(1)?)?;
    let before = text.get(..usize::try_from(span.start).ok()?)?;
    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    Some(Span {
        line: u32::try_from(before.matches('\n').count() + 1).ok()?,
        col: u32::try_from(before[line_start..].chars().count() + 1).ok()?,
        ..span
    })
}

/// The source text from `span` to the end of its line.
fn text_at(texts: &[(String, String)], span: Span) -> Option<&str> {
    let (_, text) = texts.get(usize::try_from(span.file).ok()?.checked_sub(1)?)?;
    let rest = text.get(usize::try_from(span.start).ok()?..)?;
    Some(rest.split('\n').next().unwrap_or(rest))
}

/// The name `name` a `let` at `at` binds (`at` covers `let` only).
fn name_span(texts: &[(String, String)], at: Span, name: &str) -> Option<Span> {
    text_at(texts, at)?.starts_with("let").then_some(())?;
    word_after(texts, at, "let", name)
}

/// The name `name` after keyword `kw` on the line from `at` on (`fn f`,
/// `const fn f`).
fn word_after(texts: &[(String, String)], at: Span, kw: &str, name: &str) -> Option<Span> {
    let line = text_at(texts, at)?;
    let k = if line.starts_with(kw) {
        0
    } else {
        line.find(&format!(" {kw} "))? + 1
    };
    let rest = &line[k + kw.len()..];
    let off = k + kw.len() + rest.find(|c: char| !c.is_whitespace())?;
    let word = line.get(off..)?;
    let end = word
        .find(|c: char| !c.is_alphanumeric() && c != '_')
        .unwrap_or(word.len());
    if &word[..end] != name {
        return None;
    }
    let skip = u32::try_from(line[..off].chars().count()).ok()?;
    let off = u32::try_from(off).ok()?;
    Some(Span {
        start: at.start + off,
        end: at.start + off + u32::try_from(name.len()).ok()?,
        col: at.col + skip,
        ..at
    })
}

/// `e` as source for a fix: a long call shows its arguments as `..`.
fn short_expr(e: &Expr) -> String {
    let s = nabic_ast::print::print_expr(e);
    if s.len() <= 40 {
        return s;
    }
    match &e.kind {
        ExprKind::Call { callee, .. } => format!("{}(..)", nabic_ast::print::print_expr(callee)),
        _ => "..".to_owned(),
    }
}

struct Binding {
    name: String,
    span: Span,
    used: bool,
    /// A `ref` parameter: a write reaches the caller, so it is a use.
    by_ref: bool,
}

struct Scope {
    bindings: Vec<Binding>,
    /// Parameters fixed by a trait or with no body: never "unused".
    exempt: bool,
}

/// The AST pass: one visitor over every user file.
struct Cx<'a> {
    texts: &'a [(String, String)],
    findings: Vec<Finding>,
    scopes: Vec<Scope>,
    exempt_next: bool,
    /// The bindings of one name across an or-pattern's alternatives, by
    /// span: one variable, used if any is.
    or_peers: HashMap<(u32, u32), u32>,
    /// Plain-assignment targets (`x = ..`): not reads.
    assign_targets: HashSet<(u32, u32)>,
    /// Spans of `ref` parameters.
    ref_params: HashSet<(u32, u32)>,
    /// Names mentioned in the current file / any user file.
    file_names: HashSet<String>,
    all_names: HashSet<String>,
    /// `(file, start)` of user-written casts (`x as T`; f-strings
    /// desugar to casts too).
    casts: HashSet<(u32, u32)>,
    /// `(file, start, end)` of the receivers of user-written `x.clone()`
    /// calls: a primitive's clone lowers to exactly that span.
    clones: HashSet<(u32, u32, u32)>,
    must_use: HashMap<String, String>,
    /// `#[must_use]` structs and enums by short name: the message.
    must_use_types: HashMap<String, String>,
    /// `(file, start)` of statement expressions: the expression as source.
    stmt_exprs: HashMap<(u32, u32), String>,
    /// `(file, start)` of a `let` initializer: the binding's span and name.
    let_inits: HashMap<(u32, u32), (Span, String)>,
    types: TypeMarks,
    /// In an impl's head: its type is not a use.
    impl_head: bool,
    /// The last segment of the type whose impl is walked: a deprecated
    /// type's own impl does not warn about it.
    impl_of: Option<String>,
    traits: HashSet<String>,
    /// Every enum variant name: a bare one in a pattern may be a `use`d
    /// variant, not a binding.
    variants: HashSet<String>,
    /// Function bodies the walk is in.
    in_fn: u32,
}

const fn key(s: Span) -> (u32, u32) {
    (s.file, s.start)
}

/// `#[must_use]` HIR types by (is an enum, id): the message.
type MustUse = HashMap<(bool, u32), String>;

impl<'a> Cx<'a> {
    /// `all`: the parsed files; `library`: the items of the library
    /// files `all` leaves out.
    fn new(
        all_files: &[SourceFile],
        library: Option<&nabic_ast::Program>,
        texts: &'a [(String, String)],
    ) -> Self {
        let all: Vec<&[Item]> = all_files
            .iter()
            .map(|f| f.items.as_slice())
            .chain(library.map(|p| p.items.as_slice()))
            .collect();
        let mut traits = HashSet::default();
        let mut variants = HashSet::default();
        for items in &all {
            collect_traits(items, &mut traits);
            collect_variants(items, &mut variants);
        }
        let [must_use] = marked_fns(&all, ["must_use"]);
        let mut types = TypeMarks::default();
        for f in all_files {
            let mut at: Vec<&str> = std::iter::once(f.package.as_str())
                .chain(f.module.iter().map(String::as_str))
                .filter(|s| !s.is_empty())
                .collect();
            types.add(&f.items, &mut at);
        }
        if let Some(p) = library {
            types.add(&p.items, &mut Vec::new());
        }
        Self {
            texts,
            findings: Vec::new(),
            scopes: Vec::new(),
            exempt_next: false,
            or_peers: HashMap::default(),
            assign_targets: HashSet::default(),
            ref_params: HashSet::default(),
            file_names: HashSet::default(),
            all_names: HashSet::default(),
            casts: HashSet::default(),
            clones: HashSet::default(),
            must_use,
            must_use_types: marked_types(all_files, library, "must_use"),
            stmt_exprs: HashMap::default(),
            let_inits: HashMap::default(),
            types,
            impl_head: false,
            impl_of: None,
            traits,
            variants,
            in_fn: 0,
        }
    }

    fn hit(&mut self, lint: &'static str, span: Span, msg: String) {
        self.findings.push(Finding {
            lint,
            span,
            msg,
            notes: Vec::new(),
            secondary: None,
            base: None,
            fix: None,
        });
    }

    /// `hit`, resolved by replacing `at` with `text`.
    fn hit_fix(
        &mut self,
        lint: &'static str,
        span: Span,
        msg: String,
        fix: Option<(Span, String, String)>,
    ) {
        self.hit(lint, span, msg);
        if let (Some((at, text, title)), Some(f)) = (fix, self.findings.last_mut()) {
            f.fix = Some(Box::new(Fix {
                span: at,
                text,
                title,
            }));
        }
    }

    /// `if T is A { .. } else if T is B { .. } else { panic(..) }`: a
    /// closed set of types. Reported at the fallback, so an inner
    /// `else if` of the chain adds nothing.
    fn type_switch(&mut self, e: &Expr) {
        fn type_test(e: &Expr) -> Option<&str> {
            let ExprKind::Is(operand, pat) = &e.kind else {
                return None;
            };
            let PatternKind::TypeCheck { binder: None, .. } = &pat.kind else {
                return None;
            };
            match &operand.kind {
                ExprKind::SelfUpper => Some("Self"),
                ExprKind::Path(p) => match p.segments.as_slice() {
                    [s] if s.generics.is_empty()
                        && s.name.starts_with(|c: char| c.is_ascii_uppercase()) =>
                    {
                        Some(s.name.as_str())
                    }
                    _ => None,
                },
                _ => None,
            }
        }
        fn fails(e: &Expr) -> bool {
            match &e.kind {
                ExprKind::Call { callee, .. } => matches!(
                    &callee.kind,
                    ExprKind::Path(p) if p.segments.len() == 1
                        && matches!(p.segments[0].name.as_str(), "panic" | "unreachable" | "todo")
                ),
                ExprKind::Block(b) => block_fails(b),
                _ => false,
            }
        }
        fn block_fails(b: &Block) -> bool {
            match (b.stmts.as_slice(), &b.trailing) {
                ([], Some(t)) => fails(t),
                ([s], None) => {
                    matches!(&s.kind, StmtKind::Expr(x) | StmtKind::Trailing(x) if fails(x))
                }
                _ => false,
            }
        }
        let mut at = e;
        let mut name = None;
        loop {
            let ExprKind::If {
                cond, else_branch, ..
            } = &at.kind
            else {
                return;
            };
            let Some(n) = type_test(cond) else {
                return;
            };
            if name.is_some_and(|m| m != n) {
                return;
            }
            name = Some(n);
            match else_branch.as_deref() {
                Some(
                    next @ Expr {
                        kind: ExprKind::If { .. },
                        ..
                    },
                ) => at = next,
                Some(fallback) if fails(fallback) => {
                    let n = name.unwrap_or("T");
                    self.hit(
                        "closed_type_switch",
                        fallback.span,
                        format!(
                            "this `if {n} is ..` chain fails for every other type: a new type \
                         compiles and panics at run time"
                        ),
                    );
                    if let Some(f) = self.findings.last_mut() {
                        f.notes.push(
                            "help: the types are a closed set: use a trait with an impl per type"
                                .to_owned(),
                        );
                    }
                    return;
                }
                _ => return,
            }
        }
    }

    /// The source text of `span`.
    fn src(&self, span: Span) -> Option<&str> {
        let (_, text) = self
            .texts
            .get(usize::try_from(span.file).ok()?.checked_sub(1)?)?;
        text.get(usize::try_from(span.start).ok()?..usize::try_from(span.end).ok()?)
    }

    fn file(&mut self, f: &SourceFile) {
        self.file_names.clear();
        for a in &f.attrs {
            self.attr_names(a);
        }
        for item in &f.items {
            self.item(item);
        }
        if !f.stmts.is_empty() {
            self.unreachable(&f.stmts, None);
            self.push_scope();
            for s in &f.stmts {
                walk_stmt(self, s);
            }
            self.pop_scope();
        }
        self.unused_imports(&f.items);
        let names = std::mem::take(&mut self.file_names);
        self.all_names.extend(names);
        // A template's paths are uses even where it never expands.
        self.all_names.extend(f.template_names.iter().cloned());
    }

    fn attr_names(&mut self, a: &Attribute) {
        self.file_names.extend(a.path.iter().cloned());
        if let AttrBody::Args(args) = &a.body {
            for x in args {
                if let AttrArg::Word(w) = x {
                    self.file_names.insert(w.clone());
                }
            }
        }
    }

    fn item(&mut self, item: &Item) {
        for a in item.attrs() {
            self.attr_names(a);
        }
        match item {
            Item::Fn(f) => {
                if !has_attr(&f.attrs, &["no_mangle", "export_name"]) {
                    self.name_lint("function", &f.name, f.span, 0);
                }
                self.func(f, false);
            }
            Item::Struct(s) => {
                self.name_lint("type", &s.name, s.span, 1);
                walk_item(self, item);
            }
            Item::Union(u) => {
                self.name_lint("type", &u.name, u.span, 1);
                walk_item(self, item);
            }
            Item::TypeAlias(t) => {
                self.name_lint("type", &t.name, t.span, 1);
                walk_item(self, item);
            }
            Item::Enum(e) => {
                self.name_lint("type", &e.name, e.span, 1);
                for v in &e.variants {
                    self.name_lint("variant", &v.name, v.span, 1);
                }
                walk_item(self, item);
            }
            Item::Trait(t) => {
                self.name_lint("trait", &t.name, t.span, 1);
                for ti in &t.items {
                    if let TraitItem::Method(m) = ti {
                        self.name_lint("method", &m.name, m.span, 0);
                        self.func(m, true);
                    }
                }
            }
            Item::Impl(im) => {
                self.impl_head = true;
                self.ty_names(&im.self_ty);
                self.impl_head = false;
                self.impl_of = match &im.self_ty.kind {
                    TyKind::Path(p) => p.segments.last().map(|s| s.name.clone()),
                    _ => None,
                };
                if let Some(p) = &im.trait_path {
                    self.file_names
                        .extend(p.segments.iter().map(|s| s.name.clone()));
                    // `Mul<Array<T, 2>>`: the trait's arguments use names too.
                    for g in p.segments.iter().flat_map(|s| &s.generics) {
                        self.ty_names(g);
                    }
                }
                for ii in &im.items {
                    match ii {
                        ImplItem::Method(m) => {
                            if im.trait_path.is_none() {
                                self.name_lint("method", &m.name, m.span, 0);
                            }
                            self.func(m, im.trait_path.is_some());
                        }
                        ImplItem::Const(c) => {
                            self.name_lint("constant", &c.name, c.span, 2);
                            self.scoped(|cx| nabic_hir::walk::walk_expr(cx, &c.value));
                        }
                        ImplItem::AssocType { ty, .. } => self.ty_names(ty),
                    }
                }
                self.impl_of = None;
            }
            Item::Const(c) => {
                self.name_lint("constant", &c.name, c.span, 2);
                self.scoped(|cx| walk_item(cx, item));
            }
            Item::Static(s) => {
                self.name_lint("static", &s.name, s.span, 2);
                self.scoped(|cx| walk_item(cx, item));
            }
            Item::Mod(m) => {
                self.name_lint("module", &m.name, m.span, 0);
                for i in m.items.as_deref().unwrap_or(&[]) {
                    self.item(i);
                }
            }
            // C names are not ours to style; the types are uses.
            Item::Extern(b) => {
                for ei in &b.items {
                    match ei {
                        ExternItem::Fn(f) => {
                            for t in f.params.iter().filter_map(|p| p.ty.as_ref()) {
                                self.ty_names(t);
                            }
                            if let Some(t) = &f.ret_ty {
                                self.ty_names(t);
                            }
                        }
                        ExternItem::Static(s) => self.ty_names(&s.ty),
                    }
                }
            }
            Item::Use(_) => {}
        }
    }

    fn ty_names(&mut self, t: &Ty) {
        nabic_hir::walk::walk_ty(self, t);
    }

    fn scoped(&mut self, f: impl FnOnce(&mut Self)) {
        self.push_scope();
        f(self);
        self.pop_scope();
    }

    /// A function body; `fixed`: its signature comes from a trait.
    fn func(&mut self, f: &FnDef, fixed: bool) {
        self.exempt_next = fixed || f.body.is_none();
        self.ref_params
            .extend(f.params.iter().filter(|p| p.by_ref).map(|p| key(p.span)));
        self.in_fn += 1;
        walk_fn(self, f);
        self.in_fn -= 1;
        if let Some(b) = &f.body {
            self.unreachable(&b.stmts, b.trailing.as_deref());
        }
    }

    /// `unreachable_code` in one statement list.
    fn unreachable(&mut self, stmts: &[Stmt], trailing: Option<&Expr>) {
        let Some(i) = stmts.iter().position(diverges) else {
            return;
        };
        let next = stmts
            .get(i + 1)
            .map(|s| s.span)
            .or_else(|| trailing.map(|e| e.span));
        if let Some(span) = next {
            self.findings.push(Finding {
                lint: "unreachable_code",
                span,
                msg: "unreachable statement".to_owned(),
                notes: Vec::new(),
                secondary: Some((
                    stmts[i].span,
                    "any code following this statement is unreachable".to_owned(),
                )),
                base: None,
                fix: None,
            });
        }
    }

    fn block(&mut self, b: &Block) {
        self.unreachable(&b.stmts, b.trailing.as_deref());
    }

    /// `sup_path`: `sup::` paths and `pub(sub)` outside an inline
    /// `mod x { }`, read from the tokens as written.
    fn sup_paths(&mut self, f: &SourceFile) {
        let Some((_, text)) = usize::try_from(f.file_id)
            .ok()
            .and_then(|i| i.checked_sub(1))
            .and_then(|i| self.texts.get(i))
        else {
            return;
        };
        let (tokens, _) = nabic_lexer::Lexer::new(text, "").lex_all();
        let ident = |i: usize, name: &str| matches!(tokens.get(i).map(|t| &t.kind), Some(TokenKind::Ident(n)) if n == name);
        let kind = |i: usize| tokens.get(i).map(|t| &t.kind);
        // Per open brace: whether it opens an inline module.
        let mut braces: Vec<bool> = Vec::new();
        let mut inline = 0usize;
        let mut i = 0;
        while i < tokens.len() {
            match tokens[i].kind {
                TokenKind::LBrace => {
                    let m = i >= 2 && kind(i - 2) == Some(&TokenKind::Mod);
                    inline += usize::from(m);
                    braces.push(m);
                }
                TokenKind::RBrace => inline -= usize::from(braces.pop().unwrap_or(false)),
                _ => {}
            }
            let span = Span {
                file: f.file_id,
                ..tokens[i].span
            };
            let path_start = i == 0 || kind(i - 1) != Some(&TokenKind::ColonColon);
            if inline > 0 {
                i += 1;
            } else if ident(i, "sup") && kind(i + 1) == Some(&TokenKind::ColonColon) && path_start {
                let mut n = 0;
                while ident(i + 2 * n, "sup") && kind(i + 2 * n + 1) == Some(&TokenKind::ColonColon)
                {
                    n += 1;
                }
                let written = "sup::".repeat(n);
                let note = match f.module.len().checked_sub(n) {
                    Some(k) => {
                        let to: String = f.module[..k]
                            .iter()
                            .flat_map(|m| [m.as_str(), "::"])
                            .collect();
                        format!(
                            "write the path from the package root: `pkg::{to}` instead of `{written}`"
                        )
                    }
                    None => "write the path from the package root, `pkg::..`".to_owned(),
                };
                self.findings.push(Finding {
                    lint: "sup_path",
                    span: Span {
                        end: tokens[i + 2 * n - 1].span.end,
                        ..span
                    },
                    msg: format!("`{written}` path outside an inline `mod {{ }}`"),
                    notes: vec![note],
                    secondary: None,
                    base: None,
                    fix: None,
                });
                i += 2 * n;
            } else if tokens[i].kind == TokenKind::Pub
                && kind(i + 1) == Some(&TokenKind::LParen)
                && ident(i + 2, "sub")
                && kind(i + 3) == Some(&TokenKind::RParen)
            {
                self.findings.push(Finding {
                    lint: "sup_path",
                    span: Span {
                        end: tokens[i + 3].span.end,
                        ..span
                    },
                    msg: "`pub(sub)` outside an inline `mod { }`".to_owned(),
                    notes: vec![
                        "write `pub(pkg)`: the item is then visible in the whole package"
                            .to_owned(),
                    ],
                    secondary: None,
                    base: None,
                    fix: None,
                });
                i += 4;
            } else {
                i += 1;
            }
        }
    }

    /// `"hello {name}"` with a local `name` in scope: the `f` is missing.
    /// The hole counts as a use, so `name` is not also unused.
    fn missing_f(&mut self, text: &str, span: Span) {
        let bound = |cx: &Self, n: &str| {
            cx.scopes
                .iter()
                .any(|s| s.bindings.iter().any(|b| b.name == n))
        };
        let names: Vec<String> = text
            .split('{')
            .skip(1)
            .filter_map(|h| h.split_once('}').map(|(n, _)| n.trim()))
            .filter(|n| {
                n.chars()
                    .next()
                    .is_some_and(|c| c.is_alphabetic() || c == '_')
                    && n.chars().all(|c| c.is_alphanumeric() || c == '_')
                    && bound(self, n)
            })
            .map(str::to_owned)
            .collect();
        let Some(name) = names.first().cloned() else {
            return;
        };
        // Each is read once the `f` is there: none is "unused".
        for n in &names {
            self.mark_used(n);
        }
        let raw = text.replace('\n', "\\n").replace('"', "\\\"");
        self.findings.push(Finding {
            lint: "missing_f_prefix",
            span,
            msg: format!("`{{{name}}}` is plain text here: the string has no `f` prefix"),
            notes: vec![format!(
                "help: to put the value of `{name}` in the text, write `f\"{raw}\"`"
            )],
            secondary: None,
            base: None,
            fix: self.src(span).filter(|t| t.starts_with('"')).map(|_| {
                Box::new(Fix {
                    span: Span {
                        end: span.start,
                        ..span
                    },
                    text: "f".to_owned(),
                    title: "add the `f` prefix".to_owned(),
                })
            }),
        });
    }

    /// `==` / `!=` / ordering comparisons with a NaN, `none`, or a `bool`
    /// literal.
    fn comparison(&mut self, e: &Expr, op: &str, lhs: &Expr, rhs: &Expr) {
        if !matches!(op, "==" | "!=" | "<" | "<=" | ">" | ">=") {
            return;
        }
        let nan = |x: &Expr| match &x.kind {
            ExprKind::Path(p) => p.segments.last().is_some_and(|s| s.name == "NAN"),
            ExprKind::Call { callee, args, .. } if args.is_empty() => matches!(
                &callee.kind,
                ExprKind::Path(p) if p.segments.len() == 2
                    && matches!(p.segments[0].name.as_str(), "f32" | "f64")
                    && p.segments[1].name == "nan"
            ),
            _ => false,
        };
        if nan(lhs) || nan(rhs) {
            self.hit(
                "nan_comparison",
                e.span,
                format!(
                    "comparison with NaN is always {}; use `.is_nan()`",
                    if op == "!=" { "true" } else { "false" }
                ),
            );
            return;
        }
        if !matches!(op, "==" | "!=") {
            return;
        }
        let lit = |x: &Expr| match &x.kind {
            ExprKind::Literal(l) => Some(l.clone()),
            _ => None,
        };
        match (lit(lhs), lit(rhs)) {
            (Some(Literal::Bool(b)), _) | (_, Some(Literal::Bool(b))) => {
                let same = (op == "==") == b;
                let x = if matches!(lit(lhs), Some(Literal::Bool(_))) {
                    rhs
                } else {
                    lhs
                };
                let fix = self
                    .src(x.span)
                    .filter(|_| self.src(e.span).is_some())
                    .map(|t| {
                        let plain = t
                            .chars()
                            .all(|c| c.is_alphanumeric() || "_.:()".contains(c));
                        let text = match (same, plain) {
                            (true, _) => t.to_owned(),
                            (false, true) => format!("!{t}"),
                            (false, false) => format!("!({t})"),
                        };
                        let title = format!("write `{text}`");
                        (e.span, text, title)
                    });
                self.hit_fix(
                    "bool_comparison",
                    e.span,
                    format!(
                        "comparison with `{b}`; use the operand {}",
                        if same { "directly" } else { "negated (`!x`)" }
                    ),
                    fix,
                );
            }
            _ => {}
        }
    }

    /// Statement-position calls: `unused_must_use`, `ignored_error`.
    fn discarded(&mut self, e: &Expr) {
        let mut inner = e;
        if let ExprKind::Try { kind, operand } = &e.kind {
            if *kind == TryKind::Optional {
                self.hit(
                    "ignored_error",
                    e.span,
                    "`try?` result discarded: the error is silently ignored".to_owned(),
                );
            }
            inner = operand;
        }
        let name = match &inner.kind {
            ExprKind::Call { callee, .. } => match &callee.kind {
                ExprKind::Path(p) => p.segments.last().map(|s| s.name.as_str()),
                _ => None,
            },
            ExprKind::MethodCall { method, .. } => Some(method.as_str()),
            _ => None,
        };
        if let Some(msg) = name.and_then(|n| self.must_use.get(n).cloned()) {
            let mut f = Finding {
                lint: "unused_must_use",
                span: e.span,
                msg: format!(
                    "unused result of `{}`, which is `#[must_use]`",
                    name.unwrap_or("")
                ),
                notes: Vec::new(),
                secondary: None,
                base: None,
                fix: None,
            };
            f.notes.extend(Some(msg).filter(|m| !m.is_empty()));
            self.findings.push(f);
        }
    }

    /// `p` names a deprecated type, or one's item (`Old::new`).
    fn type_use(&mut self, p: &nabic_ast::Path) {
        if self.impl_head || p.span.is_dummy() || self.types.marked.is_empty() {
            return;
        }
        let segs: Vec<&str> = p.segments.iter().map(|s| s.name.as_str()).collect();
        let hit = self
            .types
            .find(&segs)
            .or_else(|| self.types.find(&segs[..segs.len().saturating_sub(1)]));
        let Some((name, what, d)) = hit.map(|(k, w, d)| (leaf(k).to_owned(), *w, d.clone())) else {
            return;
        };
        if self.impl_of.as_deref() == Some(name.as_str()) {
            return;
        }
        self.findings.push(Finding {
            lint: "deprecated",
            span: p.span,
            msg: format!("use of deprecated {what} `{name}`"),
            notes: d.notes(&name),
            secondary: None,
            base: None,
            fix: None,
        });
    }
}

/// Whether control never continues past `s`.
fn diverges(s: &Stmt) -> bool {
    match &s.kind {
        StmtKind::Return(_)
        | StmtKind::Break { .. }
        | StmtKind::Continue { .. }
        | StmtKind::Throw(_) => true,
        StmtKind::Expr(e) => matches!(
            &e.kind,
            ExprKind::Call { callee, .. } if matches!(
                &callee.kind,
                ExprKind::Path(p) if p.segments.last().is_some_and(|s| matches!(
                    s.name.as_str(), "panic" | "exit" | "unreachable" | "todo"
                ))
            )
        ),
        _ => false,
    }
}

/// Path names a pattern mentions (enum / struct / constant paths).
fn pat_names(p: &Pattern, out: &mut HashSet<String>) {
    match &p.kind {
        PatternKind::Struct { path, fields, .. } => {
            out.extend(path.segments.iter().map(|s| s.name.clone()));
            for f in fields {
                if let Some(x) = &f.pat {
                    pat_names(x, out);
                }
            }
        }
        PatternKind::EnumVariant { path, args } => {
            out.extend(path.segments.iter().map(|s| s.name.clone()));
            args.iter().for_each(|x| pat_names(x, out));
        }
        PatternKind::Binding { name, sub } => {
            out.insert(name.clone());
            if let Some(s) = sub {
                pat_names(s, out);
            }
        }
        PatternKind::TypeCheck { ty, .. } => {
            if let TyKind::Path(p) = &ty.kind {
                out.extend(p.segments.iter().map(|s| s.name.clone()));
            }
        }
        PatternKind::Tuple(ps) | PatternKind::Slice(ps) | PatternKind::Or(ps) => {
            ps.iter().for_each(|x| pat_names(x, out));
        }
        _ => {}
    }
}

/// The names `p` binds and the spans `walk_pat` binds them at.
fn pat_binds(p: &Pattern, out: &mut Vec<(String, Span)>) {
    match &p.kind {
        PatternKind::Binding { name, sub } => {
            if let Some(s) = sub {
                pat_binds(s, out);
            }
            out.push((name.clone(), p.span));
        }
        PatternKind::TypeCheck {
            binder: Some(b), ..
        }
        | PatternKind::Rest(Some(b)) => out.push((b.clone(), p.span)),
        PatternKind::Tuple(ps) | PatternKind::Slice(ps) | PatternKind::Or(ps) => {
            ps.iter().for_each(|x| pat_binds(x, out));
        }
        PatternKind::EnumVariant { args, .. } => args.iter().for_each(|x| pat_binds(x, out)),
        PatternKind::Struct { fields, .. } => {
            for f in fields {
                match &f.pat {
                    Some(x) => pat_binds(x, out),
                    None => out.push((f.name.clone(), p.span)),
                }
            }
        }
        _ => {}
    }
}

impl AstVisitor for Cx<'_> {
    fn or_pattern(&mut self, alts: &[Pattern]) {
        let mut all = Vec::new();
        alts.iter().for_each(|a| pat_binds(a, &mut all));
        let mut names: Vec<&String> = all.iter().map(|(n, _)| n).collect();
        names.sort();
        names.dedup();
        for n in names {
            let id = u32::try_from(self.or_peers.len()).unwrap_or(u32::MAX);
            for (_, at) in all.iter().filter(|(m, _)| m == n) {
                self.or_peers.entry(key(*at)).or_insert(id);
            }
        }
    }

    fn push_scope(&mut self) {
        self.scopes.push(Scope {
            bindings: Vec::new(),
            exempt: std::mem::take(&mut self.exempt_next),
        });
    }

    fn pop_scope(&mut self) {
        let Some(s) = self.scopes.pop() else { return };
        if s.exempt {
            return;
        }
        // One finding per or-pattern variable, at its first binding.
        let mut said = HashSet::default();
        for b in s.bindings {
            if self
                .or_peers
                .get(&key(b.span))
                .is_some_and(|g| !said.insert((b.name.clone(), *g)))
            {
                continue;
            }
            if !b.used {
                let name = demangle(&b.name);
                let named = name_span(self.texts, b.span, name);
                let span = named.unwrap_or(b.span);
                // Only a `let` name found as written: a hygiene-renamed or
                // shorthand binding has no place to put the `_`.
                let fix = named.filter(|_| name == b.name).map(|at| {
                    Box::new(Fix {
                        span: Span {
                            end: at.start,
                            ..at
                        },
                        text: "_".to_owned(),
                        title: format!("write `_{name}`"),
                    })
                });
                self.findings.push(Finding {
                    lint: "unused_variables",
                    span,
                    msg: format!("unused variable `{name}`"),
                    notes: vec![format!(
                        "if this is intentional, prefix it with an underscore: `_{name}`"
                    )],
                    secondary: None,
                    base: None,
                    fix,
                });
            }
        }
    }

    fn bind_at(&mut self, name: &str, at: Span) {
        if name == "self"
            || name.starts_with('_')
            || generated(name)
            || self.variants.contains(name)
        {
            return;
        }
        self.name_lint("variable", name, at, 0);
        if let Some(s) = self.scopes.last_mut() {
            s.bindings.push(Binding {
                name: name.to_owned(),
                span: at,
                used: false,
                by_ref: self.ref_params.contains(&key(at)),
            });
        }
    }

    fn path(&mut self, p: &nabic_ast::Path) {
        self.type_use(p);
    }

    fn ty(&mut self, t: &Ty) {
        if let TyKind::Path(p) = &t.kind {
            self.file_names
                .extend(p.segments.iter().map(|s| s.name.clone()));
        }
    }

    fn stmt(&mut self, s: &Stmt) {
        match &s.kind {
            StmtKind::Assign(target, value) => {
                if let ExprKind::Path(p) = &target.kind {
                    if let [seg] = p.segments.as_slice() {
                        self.assign_targets.insert(key(target.span));
                        self.mark_written(&seg.name);
                        if matches!(&value.kind, ExprKind::Path(q)
                            if q.segments.len() == 1 && q.segments[0].name == seg.name)
                        {
                            self.hit(
                                "self_assignment",
                                s.span,
                                format!("`{0}` is assigned to itself", seg.name),
                            );
                        }
                    }
                }
            }
            StmtKind::Expr(e) => {
                self.stmt_exprs.insert(key(e.span), short_expr(e));
                self.discarded(e);
            }
            StmtKind::Let { name, init, .. } => {
                self.let_inits
                    .insert(key(init.span), (s.span, name.clone()));
            }
            StmtKind::LetPattern { pat, .. } => pat_names(pat, &mut self.file_names),
            StmtKind::LetStruct { path, .. } => {
                self.file_names
                    .extend(path.segments.iter().map(|s| s.name.clone()));
            }
            StmtKind::Item(i) => {
                if let Item::Use(u) = i {
                    if self.in_fn > 0 {
                        self.findings.push(Finding {
                            lint: "local_use",
                            span: u.span,
                            msg: "`use` inside a function body".to_owned(),
                            notes: vec![
                                "move it to the top of the file, with the other `use` items \
                                 (the order is `pub mod`, `mod`, `pub use`, `use`)"
                                    .to_owned(),
                            ],
                            secondary: None,
                            base: None,
                            fix: None,
                        });
                    }
                }
                // A nested item can't see the enclosing locals.
                let saved = std::mem::take(&mut self.scopes);
                self.item(i);
                self.scopes = saved;
            }
            _ => {}
        }
    }

    fn expr(&mut self, e: &Expr) {
        match &e.kind {
            ExprKind::Path(p) => {
                self.file_names
                    .extend(p.segments.iter().map(|s| s.name.clone()));
                if let [seg] = p.segments.as_slice() {
                    if !self.assign_targets.contains(&key(e.span)) {
                        let n = seg.name.clone();
                        self.mark_used(&n);
                    }
                }
            }
            ExprKind::MethodCall {
                receiver,
                method,
                args,
                ..
            } => {
                if method == "clone" && args.is_empty() {
                    let r = receiver.span;
                    self.clones.insert((r.file, r.start, r.end));
                }
                self.file_names.insert(method.clone());
                for a in args {
                    if let CallArg::Named { name, .. } = a {
                        self.file_names.insert(name.clone());
                    }
                }
            }
            ExprKind::QuestionDot(_, tails) => {
                for t in tails {
                    if let AccessTail::Method { method, .. } = t {
                        self.file_names.insert(method.clone());
                    }
                }
            }
            ExprKind::StructInit { path, .. } => {
                self.file_names
                    .extend(path.segments.iter().map(|s| s.name.clone()));
            }
            ExprKind::Closure {
                captures, params, ..
            } => {
                for c in captures {
                    if let CaptureSpec::Ref(n) = c {
                        self.mark_written(n);
                    }
                }
                self.ref_params
                    .extend(params.iter().filter(|p| p.by_ref).map(|p| key(p.span)));
            }
            ExprKind::Cast(_, ty) => {
                let genuine = self
                    .texts
                    .get(usize::try_from(ty.span.file).unwrap_or(0).wrapping_sub(1))
                    .and_then(|(_, t)| t.get(..usize::try_from(ty.span.start).ok()?))
                    .is_some_and(|before| before.trim_end().ends_with("as"));
                if genuine {
                    self.casts.insert(key(e.span));
                }
            }
            ExprKind::Literal(Literal::Str(text)) => self.missing_f(text, e.span),
            ExprKind::OpInfix { op, lhs, rhs } => self.comparison(e, op, lhs, rhs),
            ExprKind::While { cond, body, .. } => {
                if matches!(cond.kind, ExprKind::Literal(Literal::Bool(true))) {
                    let fix = self.src(cond.span).filter(|c| *c == "true").map(|_| {
                        let at = Span {
                            end: cond.span.end,
                            ..e.span
                        };
                        (at, "loop".to_owned(), "write `loop`".to_owned())
                    });
                    self.hit_fix(
                        "while_true",
                        e.span,
                        "`while true` is `loop`".to_owned(),
                        fix,
                    );
                }
                self.block(body);
            }
            ExprKind::For {
                pat, body, by_ref, ..
            } => {
                // A `for ref` binding writes through: a write is a use.
                if *by_ref {
                    self.ref_params.insert(key(pat.span));
                }
                pat_names(pat, &mut self.file_names);
                self.block(body);
            }
            ExprKind::Match { arms, .. } => {
                for a in arms {
                    pat_names(&a.pat, &mut self.file_names);
                }
            }
            ExprKind::Is(_, pat) => pat_names(pat, &mut self.file_names),
            ExprKind::Catch {
                catches, finally, ..
            } => {
                for c in catches {
                    pat_names(&c.pat, &mut self.file_names);
                    self.block(&c.body);
                }
                if let Some(f) = finally {
                    self.block(f);
                }
            }
            ExprKind::Block(b)
            | ExprKind::Unsafe(b)
            | ExprKind::ConstBlock(b)
            | ExprKind::Loop { body: b, .. }
            | ExprKind::AsyncBlock(b)
            | ExprKind::YieldBlock(b)
            | ExprKind::AsyncYieldBlock(b) => self.block(b),
            ExprKind::If { then_branch, .. } => {
                self.type_switch(e);
                self.block(then_branch);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lint::naming::{is_camel, is_snake, is_upper, to_camel, to_snake};

    #[test]
    fn demangle_hides_hygiene_suffix() {
        assert_eq!(demangle("count__mac"), "count");
        assert_eq!(demangle("Field__meta"), "Field");
        assert_eq!(demangle("plain"), "plain");
        // A lone suffix is not a hygiene name: left as it is.
        assert_eq!(demangle("__mac"), "__mac");
    }

    #[test]
    fn names_and_levels() {
        assert!(is_snake("σ²") && is_snake("a_b1") && !is_snake("aB"));
        assert!(is_camel("Foo") && is_camel("HTTP") && !is_camel("foo") && !is_camel("Foo_Bar"));
        assert!(is_upper("MAX_N") && !is_upper("Max"));
        assert_eq!(to_snake("myVarName"), "my_var_name");
        assert_eq!(to_camel("my_type"), "MyType");
        assert_eq!(expand("unused").unwrap().len(), 7);
        assert_eq!(expand("all").unwrap().len(), LINTS.len());
        assert!(expand("nope").is_none());
        assert_eq!(Level::parse("deny"), Some(Level::Deny));
    }

    #[test]
    fn layout_lints() {
        let lib = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../library");
        let dir = nabic_core::tmp::test_dir("lint-layout");
        std::fs::create_dir_all(dir.join("src/a")).unwrap();
        let main = "mod a;\n\nfn main() {\n    use a::f;\n    println(f());\n}\n\n\
                    mod t {\n    use sup::a;\n    pub(sub) fn h() { a::f(); }\n}\n";
        std::fs::write(
            dir.join("src/a.nabi"),
            "pub mod b;\n\npub fn f() -> i32 { b::g() }\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("src/a/b.nabi"),
            "pub(sub) fn q() {}\npub fn g() -> i32 { sup::sup::k() }\n",
        )
        .unwrap();
        let root = dir.join("src/main.nabi");
        let main = format!("{main}pub fn k() -> i32 {{ 1 }}\n");
        std::fs::write(&root, &main).unwrap();
        let mut s = nabic_interface::Session::new();
        s.add_bootstrap_file(lib.join("core/src/lib.nabi"));
        let a = crate::analysis::analyze(s, &main, &root, &Levels::default());
        let got: Vec<String> = a
            .diags
            .iter()
            .filter(|d| {
                matches!(
                    d.lint.as_deref().map(String::as_str),
                    Some("sup_path" | "local_use")
                )
            })
            .map(|d| format!("{} {}", d.message, d.notes.join("; ")))
            .collect();
        assert_eq!(
            got,
            [
                "`use` inside a function body move it to the top of the file, with the other \
                 `use` items (the order is `pub mod`, `mod`, `pub use`, `use`); \
                 `#[warn(local_use)]` on by default",
                "`pub(sub)` outside an inline `mod { }` write `pub(pkg)`: the item is then \
                 visible in the whole package; `#[warn(sup_path)]` on by default",
                "`sup::sup::` path outside an inline `mod { }` write the path from the package \
                 root: `pkg::` instead of `sup::sup::`; `#[warn(sup_path)]` on by default",
            ],
            "{:?}",
            a.diags
        );
    }

    /// The import and visibility helps, applied as written, fix the
    /// errors without tripping `sup_path` or `local_use`.
    #[test]
    fn suggestions_pass_layout_lints() {
        let lib = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../library");
        let dir = nabic_core::tmp::test_dir("lint-suggest");
        std::fs::create_dir_all(dir.join("src/geo")).unwrap();
        // A parent's private item, a parent's item and type from below,
        // and Rust `super::` in a path and in a `use`.
        let mut files = vec![
            (
                dir.join("src/main.nabi"),
                "mod geo;\n\nfn helper() -> i32 { 1 }\n\nfn main() {\n    println(geo::f());\n}\n"
                    .to_owned(),
            ),
            (
                dir.join("src/geo.nabi"),
                "pub mod edge;\npub mod inner;\n\npub struct Point { pub x: i32 }\n\n\
                 pub fn sides() -> i32 { 3 }\n\n\
                 pub fn f() -> i32 { helper() + inner::g() + edge::e() }\n"
                    .to_owned(),
            ),
            (
                dir.join("src/geo/inner.nabi"),
                "pub fn g() -> i32 { sides() + super::sides() + h(Point { x: 1 }) }\n\n\
                 fn h(p: Point) -> i32 { p.x }\n"
                    .to_owned(),
            ),
            (
                dir.join("src/geo/edge.nabi"),
                "use super::sides;\n\npub fn e() -> i32 { sides() }\n".to_owned(),
            ),
        ];
        let run = |files: &[(std::path::PathBuf, String)]| {
            for (p, t) in files {
                std::fs::write(p, t).unwrap();
            }
            let mut s = nabic_interface::Session::new();
            s.add_bootstrap_file(lib.join("core/src/lib.nabi"));
            crate::analysis::analyze(s, &files[0].1, &files[0].0, &Levels::default())
        };
        let a = run(&files);
        let mut helps = Vec::new();
        // Each help's edits, by the file of its diagnostic.
        for d in a.diags.iter().filter(|d| d.severity == Severity::Error) {
            let span = d.primary.as_ref().unwrap().span;
            let path = std::path::PathBuf::from(a.source_map.path(span.file).unwrap());
            let i = files.iter().position(|(p, _)| *p == path).unwrap();
            for help in d.notes.iter().filter_map(|n| n.strip_prefix("help: ")) {
                helps.push(help.to_owned());
                let quoted: Vec<&str> = help.split('`').skip(1).step_by(2).collect();
                for q in &quoted {
                    if q.starts_with("use ") && q.ends_with(';') && !files[i].1.contains(q) {
                        let t = &mut files[i].1;
                        let at = t.find("\n\n").map_or(0, |n| n + 2);
                        t.insert_str(at, &format!("{q}\n\n"));
                    }
                }
                if help.contains("write `pub(pkg)` before it") {
                    let name = quoted[0];
                    for (_, t) in &mut files {
                        *t = t.replace(&format!("\nfn {name}("), &format!("\npub(pkg) fn {name}("));
                    }
                }
                if let Some(p) = help.strip_prefix("write the path from the package root: ") {
                    let p = p.trim_matches('`');
                    let (_, text) = a.source_map.file(span.file).unwrap();
                    if p.starts_with("use ") {
                        let line = text[..span.start as usize].rfind('\n').map_or(0, |n| n + 1);
                        let line = text[line..].lines().next().unwrap();
                        files[i].1 = files[i].1.replacen(line, p, 1);
                        continue;
                    }
                    // The span is the call; the path is up to its `(`.
                    let written = &text[span.start as usize..span.end as usize];
                    let path = written.split('(').next().unwrap_or(written);
                    files[i].1 = files[i].1.replacen(path, p, 1);
                }
            }
        }
        assert_eq!(
            helps,
            [
                "write the path from the package root: `use pkg::geo::sides;`",
                "add `use pkg::geo::Point;` inside `mod inner`",
                "add `use pkg::geo::sides;`",
                "add `use pkg::geo::sides;`",
                "write the path from the package root: `pkg::geo::sides`",
                "add `use pkg::geo::Point;`",
                "`helper` has that name, but it is private to module `<root>`: write `pub(pkg)` \
                 before it, so its whole package sees it, and add `use pkg::helper;`",
            ],
            "{:?}",
            a.diags
        );
        let b = run(&files);
        let left: Vec<&NabiDiagnostic> = b
            .diags
            .iter()
            .filter(|d| {
                d.severity == Severity::Error
                    || matches!(
                        d.lint.as_deref().map(String::as_str),
                        Some("sup_path" | "local_use")
                    )
            })
            .collect();
        assert!(left.is_empty(), "{left:?}\n{files:?}");
    }

    #[test]
    fn warnings_group_takes_later_lints() {
        static FAKE: &[Lint] = &[
            warn("old", "a lint of the first release"),
            warn_since("new", "a lint of a later release"),
            Lint {
                name: "quiet",
                default: Level::Allow,
                desc: "a lint off by default",
                since: "0.2.0",
            },
        ];
        assert_eq!(expand_in(FAKE, "warnings").unwrap(), ["old", "new"]);
        assert_eq!(expand_in(FAKE, "all").unwrap(), ["old", "new", "quiet"]);
    }
}
