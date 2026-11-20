//! Whole-program checks: unresolved types, cycles, sizes.

#[allow(clippy::wildcard_imports)]
use crate::lower::collect::*;

/// Safety net: nothing leaves lowering with an unresolved type unless an
/// error was reported. A leftover `HirTy::Path` is an unknown name; a
/// leftover `Unknown` / `UnresolvedPath` is a silent fall-through that MIR
/// would lower as unit / `any`.
pub(crate) fn check_unresolved_types(
    module: &mut HirModule,
    spans: &HashMap<String, nabic_diagnostic::Span>,
    generics_of: &dyn Fn(&str) -> Option<(nabic_ast::Generics, nabic_diagnostic::Span)>,
    sources: &nabic_diagnostic::SourceMap,
    names: &TypeNames,
) {
    // Report each unknown name at the first item naming it, not at the
    // thread-local current span (whatever pass ran last).
    // (name, where, mentioning item's module)
    let mut unknown_types: Vec<(String, nabic_diagnostic::Span, String)> = Vec::new();
    let mut note = |ty: &HirTy, span: nabic_diagnostic::Span, item: &str| {
        let mut names = Vec::new();
        collect_unknown_paths(ty, &mut names);
        for n in names {
            if !unknown_types.iter().any(|(t, ..)| *t == n) {
                unknown_types.push((n, span, owner_module(item)));
            }
        }
    };
    for s in &module.structs {
        for f in &s.fields {
            note(&f.ty, s.span, &s.name);
        }
    }
    for e in &module.enums {
        for v in &e.variants {
            if let Some(t) = &v.payload_ty {
                note(t, e.span, &e.name);
            }
        }
    }
    for st in &module.statics {
        note(&st.ty, st.span, &st.name);
    }
    for f in &module.functions {
        note(&f.ret_ty, f.span, &f.name);
        for p in &f.params {
            note(&p.ty, f.span, &f.name);
        }
        for l in &f.locals {
            note(&l.ty, f.span, &f.name);
        }
    }
    // `List<Nope>` names `Nope`, where it is written.
    let open_args = names.open_args.borrow();
    unknown_types.retain(|(t, ..)| open_args.get(t.as_str()).is_none_or(|a| !a.is_empty()));
    for (t, span, _) in &mut unknown_types {
        if let Some(a) = open_args.get(t.as_str()).filter(|a| spans.contains_key(*a)) {
            span.clone_from(&spans[a]);
            t.clone_from(a);
        }
    }
    drop(open_args);
    unknown_types.sort_by(|a, b| a.0.cmp(&b.0));
    unknown_types.dedup_by(|a, b| a.0 == b.0);
    // Type keys a user may name: the package's own, and the library's
    // `pub` ones.
    let named: Vec<&str> = module
        .structs
        .iter()
        .map(|s| s.name.as_str())
        .chain(module.enums.iter().map(|e| e.name.as_str()))
        .map(|n| n.split("__").next().unwrap_or(n))
        .filter(|n| {
            !nabic_core::in_toolchain(n) || module.type_visibility.get(*n) == Some(&Visibility::Pub)
        })
        .collect();
    let known: Vec<&str> = named
        .iter()
        .map(|n| leaf_name(n))
        .chain(crate::lower::PRIM_NAMES.iter().copied())
        .collect();
    let mut diags = Vec::new();
    let mut reported = Vec::new();
    for (t, span, at) in unknown_types {
        let span = spans.get(&t).copied().unwrap_or(span);
        // A generic type written bare: it exists, it lacks its arguments.
        if let Some((g, at)) = generics_of(&t) {
            let leaf = leaf_name(&t);
            let (msg, help) = crate::lower::mono::bare_generic(leaf, &g);
            diags.push(
                nabic_diagnostic::NabiDiagnostic::error(Code::GENERIC_ARGUMENTS, at, msg)
                    .with_help(help),
            );
            reported.push(t);
            continue;
        }
        let elsewhere = named
            .iter()
            .filter(|k| !nabic_core::in_toolchain(k) && leaf_name(k) == t && module_of(k) != at)
            .min();
        let d = if let (Some(k), false) = (elsewhere, at.is_empty()) {
            let def = module_of(k);
            let depth = |m: &str| m.split("::").filter(|s| !s.is_empty()).count();
            let up = depth(&at).saturating_sub(depth(&def));
            let where_ = if def.is_empty() || at.starts_with(&format!("{def}::")) {
                if up == 1 {
                    "the parent module".to_owned()
                } else {
                    format!("the module {up} levels up")
                }
            } else {
                format!("module `{def}`")
            };
            let path = crate::lower::resolve::use_path(&at, k);
            nabic_diagnostic::NabiDiagnostic::error(
                Code::UNRESOLVED_TYPE,
                span,
                format!(
                    "cannot find type `{t}` in module `{at}`: it is defined in {where_}, \
                     and a module sees only its own items and what it imports"
                ),
            )
            .with_help(format!("add `use {path};` inside `mod {}`", leaf_name(&at)))
        } else {
            let d = nabic_diagnostic::NabiDiagnostic::error(
                Code::UNRESOLVED_TYPE,
                span,
                format!("cannot find type `{t}` in this scope"),
            );
            let public = named
                .iter()
                .copied()
                .filter(|n| module.type_visibility.get(*n) == Some(&Visibility::Pub));
            if let Some(h) = habit_type_help(&at, &t, span, sources) {
                d.with_help(h)
            } else if let Some(h) = tier_type_help(&t, &named) {
                d.with_help(h)
            } else if let Some(h) = resolve::own_library_import(&t, public) {
                d.with_help(h)
            } else if let Some(h) = crate::lower::join_help(
                (!t.contains("::"))
                    .then(|| names.import_help(&at, names.type_leaf(&t), |k| names.type_vis(k)))
                    .flatten(),
                nabic_diagnostic::suggest(leaf_name(&t), known.iter().copied())
                    .map(|s| format!("did you mean `{s}`?")),
            ) {
                d.with_help(h)
            } else {
                d
            }
        };
        diags.push(d);
        reported.push(t);
    }
    // A type or pattern error naming an unknown type follows from it:
    // report the name first and alone.
    module.diagnostics.retain(|d| {
        !matches!(
            d.kind(),
            nabic_diagnostic::ErrorKind::Type | nabic_diagnostic::ErrorKind::Pattern
        ) || !reported
            .iter()
            .filter(|t| t.starts_with(char::is_alphabetic))
            .any(|t| names_type(&d.message, t))
    });
    module.diagnostics.splice(0..0, diags);
    if module
        .diagnostics
        .iter()
        .any(nabic_diagnostic::NabiDiagnostic::is_error)
    {
        return;
    }
    // At the expression with no type where one is known, else the fn.
    let bad = module.functions.iter().find_map(|f| {
        let at = f.body.as_ref().and_then(block_unknown);
        (at.is_some()
            || f.params.iter().any(|p| p.ty == HirTy::Unknown)
            || f.locals.iter().any(|l| l.ty == HirTy::Unknown))
        .then(|| (f, at.filter(|s| !s.is_dummy()).unwrap_or(f.span)))
    });
    if let Some((f, at)) = bad {
        module.diagnostics.push(
            nabic_diagnostic::NabiDiagnostic::error(
                Code::INTERNAL,
                at,
                format!(
                    "internal compiler error: an expression in `{}` has no resolved type \
                 (lowering fell through without a diagnostic) - please report",
                    f.name
                ),
            )
            .with_help(format!(
                "a type annotation on the binding may get past it; please report the code at {}",
                nabic_core::ISSUES
            )),
        );
    }
}

/// Whether a quoted type in `msg` names the path `t`: `ConstPtr` in
/// "no method `is_null` found for type `ConstPtr`" or "`[ConstPtr; 2]`".
pub(crate) fn names_type(msg: &str, t: &str) -> bool {
    let word = |c: char| c.is_alphanumeric() || c == '_' || c == ':';
    msg.split('`').skip(1).step_by(2).any(|quoted| {
        quoted.match_indices(t).any(|(i, _)| {
            let before = quoted[..i].chars().next_back();
            let after = quoted[i + t.len()..].chars().next();
            !before.is_some_and(word) && !after.is_some_and(word)
        })
    })
}

/// Most scalar slots one value type may flatten to: nesting a generic
/// that doubles its payload (`W<W<..>>`) grows exponentially.
pub(crate) const MAX_VALUE_LEAVES: u64 = 1 << 16;

/// Largest value type, in (estimated) bytes.
pub(crate) const MAX_VALUE_BYTES: u64 = 1 << 32;

/// Statics whose initializers name each other directly form a cycle no
/// order can initialize. A cycle through a function call stays the
/// run-time panic.
pub(crate) fn check_static_cycles(module: &mut HirModule) {
    let bodies: HashMap<FunctionId, &crate::HirFunction> =
        module.functions.iter().map(|f| (f.id, f)).collect();
    let index: HashMap<&str, usize> = (module.statics.iter().enumerate())
        .map(|(i, s)| (s.name.as_str(), i))
        .collect();
    let reads: Vec<Vec<usize>> = (module.statics.iter())
        .map(|s| {
            let mut out = Vec::new();
            let body = s.init_fn.and_then(|id| bodies.get(&id)?.body.as_ref());
            if let Some(b) = body {
                crate::each_block(b, &mut |e| {
                    if let HirExprKind::StaticRef(n) = &e.kind {
                        if let Some(&j) = index.get(n.as_str()) {
                            if !out.contains(&j) {
                                out.push(j);
                            }
                        }
                    }
                });
            }
            out
        })
        .collect();
    // 0 unvisited, 1 on the path, 2 done.
    let mut state = vec![0u8; reads.len()];
    let mut diags = Vec::new();
    for start in 0..reads.len() {
        if state[start] != 0 {
            continue;
        }
        let mut path = vec![start];
        let mut next = vec![0usize];
        state[start] = 1;
        while let Some(&at) = path.last() {
            let k = next.last_mut().expect("parallel to path");
            let Some(&to) = reads[at].get(*k) else {
                state[at] = 2;
                path.pop();
                next.pop();
                continue;
            };
            *k += 1;
            if state[to] == 0 {
                state[to] = 1;
                path.push(to);
                next.push(0);
            } else if state[to] == 1 {
                let from = path.iter().position(|&p| p == to).unwrap_or(0);
                let cycle: Vec<&str> = path[from..]
                    .iter()
                    .chain(std::iter::once(&to))
                    .map(|&i| leaf_name(&module.statics[i].name))
                    .collect();
                diags.push(
                    nabic_diagnostic::NabiDiagnostic::error(
                        Code::CYCLIC_DEFINITION,
                        module.statics[to].span,
                        format!(
                            "static `{}` is initialized from itself: `{}`",
                            cycle[0],
                            cycle.join(" -> ")
                        ),
                    )
                    .with_help(format!(
                        "give `{}` a value that does not read `{}`, such as a literal or a \
                         `const`",
                        cycle[0], cycle[1]
                    )),
                );
            }
        }
    }
    module.diagnostics.extend(diags);
}

/// A struct / enum containing itself by value has no finite size; one past
/// the layout caps is refused before MIR expands it.
pub(crate) fn check_value_types(module: &mut HirModule) {
    #[derive(Clone, Copy, PartialEq, Eq, Hash)]
    enum Def {
        S(usize),
        E(usize),
    }
    struct Defs<'m> {
        m: &'m HirModule,
        structs: HashMap<StructId, usize>,
        enums: HashMap<EnumId, usize>,
    }
    impl Defs<'_> {
        fn of(&self, t: &HirTy) -> Option<Def> {
            match t {
                HirTy::Struct(id) => self.structs.get(id).copied().map(Def::S),
                HirTy::Enum(id) => self.enums.get(id).copied().map(Def::E),
                _ => None,
            }
        }
        fn children(&self, d: Def) -> Vec<&HirTy> {
            match d {
                Def::S(i) => self.m.structs[i].fields.iter().map(|f| &f.ty).collect(),
                Def::E(i) => self.m.enums[i]
                    .variants
                    .iter()
                    .filter_map(|v| v.payload_ty.as_ref())
                    .collect(),
            }
        }
        /// Defs held by value, through tuples / sums / arrays.
        fn inline(&self, t: &HirTy, out: &mut Vec<Def>) {
            match t {
                HirTy::Tuple(ps) | HirTy::Sum(ps) => ps.iter().for_each(|p| self.inline(p, out)),
                HirTy::Array(e, _) => self.inline(e, out),
                t => out.extend(self.of(t)),
            }
        }
        fn edges(&self, d: Def) -> Vec<Def> {
            let mut out = Vec::new();
            for t in self.children(d) {
                self.inline(t, &mut out);
            }
            out
        }
        /// Source name (`W<..>` for an instance), span, "a struct" / "an enum".
        fn name_span(&self, d: Def) -> (String, nabic_diagnostic::Span, &'static str) {
            let (name, generic, span, kind) = match d {
                Def::S(i) => {
                    let s = &self.m.structs[i];
                    (&s.name, !s.generic_args.is_empty(), s.span, "a struct")
                }
                Def::E(i) => {
                    let e = &self.m.enums[i];
                    (&e.name, !e.generic_args.is_empty(), e.span, "an enum")
                }
            };
            let base = name.split("__").next().unwrap_or(name);
            let name = if generic {
                format!("{base}<..>")
            } else {
                base.to_owned()
            };
            (name, span, kind)
        }
        /// (scalar slots, estimated bytes); saturating.
        fn size(&self, t: &HirTy, memo: &mut HashMap<Def, (u64, u64)>) -> (u64, u64) {
            let add =
                |a: (u64, u64), b: (u64, u64)| (a.0.saturating_add(b.0), a.1.saturating_add(b.1));
            match t {
                HirTy::Tuple(ps) | HirTy::Sum(ps) => ps
                    .iter()
                    .fold((1, 8), |acc, p| add(acc, self.size(p, memo))),
                HirTy::Array(elem, len) => {
                    let (slots, bytes) = self.size(elem, memo);
                    (slots, bytes.saturating_mul(u64::from(*len)))
                }
                t => match self.of(t) {
                    Some(d) => {
                        if let Some(v) = memo.get(&d) {
                            return *v;
                        }
                        // Provisional entry: cycles were reported already.
                        memo.insert(d, (1, 8));
                        let v = self
                            .children(d)
                            .iter()
                            .fold((1, 8), |acc, c| add(acc, self.size(c, memo)));
                        memo.insert(d, v);
                        v
                    }
                    None => (1, 8),
                },
            }
        }
    }
    let defs = Defs {
        m: module,
        structs: module
            .structs
            .iter()
            .enumerate()
            .map(|(i, s)| (s.id, i))
            .collect(),
        enums: module
            .enums
            .iter()
            .enumerate()
            .map(|(i, e)| (e.id, i))
            .collect(),
    };
    let all: Vec<Def> = (0..module.structs.len())
        .map(Def::S)
        .chain((0..module.enums.len()).map(Def::E))
        .collect();
    let mut errs = Vec::new();
    for &d in &all {
        let mut stack = defs.edges(d);
        let mut seen = HashSet::default();
        while let Some(x) = stack.pop() {
            if x == d {
                let (name, span, kind) = defs.name_span(d);
                errs.push((
                    Code::CYCLIC_DEFINITION,
                    span,
                    format!(
                        "recursive type `{name}` has infinite size ({kind} can't contain \
                         itself by value; hold it through a reference type such as `List`)"
                    ),
                ));
                break;
            }
            if seen.insert(x) {
                stack.extend(defs.edges(x));
            }
        }
    }
    if errs.is_empty() {
        let mut memo = HashMap::default();
        for &d in &all {
            let t = match d {
                Def::S(i) => HirTy::Struct(module.structs[i].id),
                Def::E(i) => HirTy::Enum(module.enums[i].id),
            };
            let (leaves, bytes) = defs.size(&t, &mut memo);
            if leaves > MAX_VALUE_LEAVES || bytes > MAX_VALUE_BYTES {
                let (name, span, _) = defs.name_span(d);
                errs.push((
                    Code::TYPE_SIZE,
                    span,
                    format!(
                        "value type `{name}` is too large ({leaves} scalar slots, ~{bytes} \
                         bytes; the limit is {MAX_VALUE_LEAVES} slots / {MAX_VALUE_BYTES} bytes)"
                    ),
                ));
                break;
            }
        }
    }
    for (code, span, msg) in errs {
        module
            .diagnostics
            .push(nabic_diagnostic::NabiDiagnostic::error(code, span, msg));
    }
}

/// A `#[repr(packed)]` struct is for C data; its fields may
/// sit unaligned, where the collector cannot read a reference.
pub(crate) fn check_packed(module: &mut HirModule) {
    fn gc(m: &HirModule, t: &HirTy, seen: &mut Vec<HirTy>) -> bool {
        if seen.contains(t) {
            return false;
        }
        seen.push(t.clone());
        let mut rec = |t: &HirTy| gc(m, t, seen);
        match t {
            HirTy::Prim(PrimTy::Str)
            | HirTy::GcBuf(_)
            | HirTy::Any
            | HirTy::AnyTrait(_)
            | HirTy::Closure { .. } => true,
            HirTy::Fn(..) => !t.is_c_fn(),
            HirTy::Tuple(ts) | HirTy::Sum(ts) => ts.iter().any(rec),
            HirTy::Array(e, n) => *n > 0 && rec(e),
            HirTy::Throws { ok, err } => rec(ok) || rec(err),
            HirTy::Struct(id) => {
                let s = &m.structs[id.0 as usize];
                match &s.union_kind {
                    Some(u) if matches!(u.storage, crate::UnionStorage::ByteArray) => false,
                    _ => s.fields.iter().any(|f| rec(&f.ty)),
                }
            }
            HirTy::Enum(id) => m.enums[id.0 as usize]
                .variants
                .iter()
                .filter_map(|v| v.payload_ty.as_ref())
                .any(rec),
            _ => false,
        }
    }
    let bad: Vec<(nabic_diagnostic::Span, String)> = module
        .structs
        .iter()
        .filter(|s| s.repr.packed && s.fields.iter().any(|f| gc(module, &f.ty, &mut Vec::new())))
        .map(|s| {
            (
                s.span,
                s.name.split("__").next().unwrap_or(&s.name).to_owned(),
            )
        })
        .collect();
    for (span, name) in bad {
        module
            .diagnostics
            .push(nabic_diagnostic::NabiDiagnostic::error(
                Code::INVALID_REPR,
                span,
                format!(
                    "`#[repr(packed)]` struct `{}` holds a GC reference (`str`, a collection, \
                     a closure ..); a packed struct is for C data",
                    leaf_name(&name)
                ),
            ));
    }
}

pub fn collect_unknown_paths(ty: &HirTy, out: &mut Vec<String>) {
    match ty {
        HirTy::Path(p) => {
            out.push(p.join("::"));
        }
        HirTy::OpenConst(n) => out.push(n.clone()),
        HirTy::NoThrow => out.push("#nothrow".to_owned()),
        HirTy::Sum(parts) | HirTy::Tuple(parts) => {
            for p in parts {
                collect_unknown_paths(p, out);
            }
        }
        HirTy::Array(e, _) | HirTy::GcBuf(e) => collect_unknown_paths(e, out),
        HirTy::Fn(a, r, _)
        | HirTy::Closure {
            args: a, ret: r, ..
        } => {
            for p in a {
                collect_unknown_paths(p, out);
            }
            collect_unknown_paths(r, out);
        }
        HirTy::Throws { ok, err } => {
            collect_unknown_paths(ok, out);
            collect_unknown_paths(err, out);
        }
        _ => {}
    }
}

pub(crate) fn hir_block_has_unknown(b: &HirBlock) -> bool {
    block_unknown(b).is_some()
}

pub(crate) fn hir_expr_has_unknown(e: &HirExpr) -> bool {
    expr_unknown(e).is_some()
}

/// Where an expression of `b` has no type: the innermost one.
pub(crate) fn block_unknown(b: &HirBlock) -> Option<nabic_diagnostic::Span> {
    b.stmts
        .iter()
        .find_map(|s| match s {
            HirStmt::Let { init: e, .. }
            | HirStmt::Assign { value: e, .. }
            | HirStmt::StaticAssign { value: e, .. }
            | HirStmt::Expr(e)
            | HirStmt::Panic(e)
            | HirStmt::Return(Some(e))
            | HirStmt::Break { value: Some(e), .. } => expr_unknown(e),
            HirStmt::ArrayAssign { index, value, .. } => {
                expr_unknown(index).or_else(|| expr_unknown(value))
            }
            HirStmt::Return(None)
            | HirStmt::Break { value: None, .. }
            | HirStmt::Continue { .. } => None,
        })
        .or_else(|| b.trailing.as_ref().and_then(expr_unknown))
}

fn expr_unknown(e: &HirExpr) -> Option<nabic_diagnostic::Span> {
    let inner = inner_unknown(e);
    if inner.is_none() && (e.ty == HirTy::Unknown || matches!(e.kind, K::UnresolvedPath(_))) {
        return Some(e.span);
    }
    inner
}

fn inner_unknown(e: &HirExpr) -> Option<nabic_diagnostic::Span> {
    let any = |es: &[HirExpr]| es.iter().find_map(expr_unknown);
    let two = |a: &HirExpr, b: &HirExpr| expr_unknown(a).or_else(|| expr_unknown(b));
    match &e.kind {
        K::UnresolvedPath(_)
        | K::Const(_)
        | K::Local(_)
        | K::StaticRef(_)
        | K::FnRef(_)
        | K::SizeOf { .. }
        | K::TypeDesc(_) => None,
        K::Block(b) => block_unknown(b),
        K::PrimBinOp { lhs, rhs, .. } => two(lhs, rhs),
        K::PrimUnaryOp { operand, .. } | K::Cast { operand, .. } => expr_unknown(operand),
        K::Call { args, .. } | K::EnumVariantInit { args, .. } | K::CoroNew { args, .. } => {
            any(args)
        }
        K::CallIndirect { callee, args, .. } => expr_unknown(callee).or_else(|| any(args)),
        K::ClosureValue { env, .. } => expr_unknown(env),
        K::CallClosure { closure, args, .. } => expr_unknown(closure).or_else(|| any(args)),
        K::If {
            cond,
            then_block,
            else_branch,
        } => expr_unknown(cond)
            .or_else(|| block_unknown(then_block))
            .or_else(|| else_branch.as_deref().and_then(expr_unknown)),
        K::While { cond, body, .. } => expr_unknown(cond).or_else(|| block_unknown(body)),
        K::Loop { body, .. } => block_unknown(body),
        K::Match { scrutinee, arms } => expr_unknown(scrutinee).or_else(|| {
            arms.iter().find_map(|a| {
                a.guard
                    .as_ref()
                    .and_then(expr_unknown)
                    .or_else(|| expr_unknown(&a.body))
            })
        }),
        K::StructInit { fields, .. } | K::Tuple { fields } => any(fields),
        K::FieldRead { target, .. }
        | K::TupleField { target, .. }
        | K::UnionRead { target, .. }
        | K::ArrayRepeat { value: target, .. } => expr_unknown(target),
        K::WithField { target, value, .. } => two(target, value),
        K::SumWrap { value, .. } => value.as_deref().and_then(expr_unknown),
        K::ArrayInit { elements, .. } => any(elements),
        K::ArrayIndex { array, index, .. } => two(array, index),
        K::UnionWrite { value, .. } | K::Suspend { value, .. } => expr_unknown(value),
        K::BufNew { len, .. } | K::RawRead { addr: len, .. } | K::AddrOf { place: len } => {
            expr_unknown(len)
        }
        K::RawWrite { addr, value, .. } => two(addr, value),
        K::BufGet { buf, index, .. } => two(buf, index),
        K::BufSet {
            buf, index, value, ..
        } => two(buf, index).or_else(|| expr_unknown(value)),
        K::BufAtomic {
            buf, index, args, ..
        } => two(buf, index).or_else(|| any(args)),
    }
}

/// `std::time::Instant` that another toolchain root has, among the type
/// keys a user may name: write `core::time::Instant`.
fn tier_type_help(t: &str, named: &[&str]) -> Option<String> {
    let (root, rest) = t.split_once("::")?;
    if !nabic_core::TIERS.contains(&root) {
        return None;
    }
    let found = nabic_core::TIERS
        .iter()
        .filter(|r| **r != root)
        .map(|r| format!("{r}::{rest}"))
        .find(|c| named.contains(&c.as_str()))?;
    Some(format!("`{t}` is not in `{root}`: write `{found}`"))
}
