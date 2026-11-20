//! Definitions, references and renames of the name at the cursor.

#[allow(clippy::wildcard_imports)]
use crate::*;

/// Where the item under the cursor is defined (functions, types).
pub(crate) fn definition(a: &Analysis, key: &str, ident: &str, off: usize) -> Option<Span> {
    let hir = a.hir.as_ref()?;
    let fn_span = |id| hir.functions.iter().find(|f| f.id == id).map(|f| f.span);
    let struct_span = |id| hir.structs.iter().find(|s| s.id == id).map(|s| s.span);
    let at = a.find_expr(
        key,
        |s| (s.start as usize..s.end as usize).contains(&off),
        false,
    );
    let hit =
        at.and_then(|e| match &e.kind {
            HirExprKind::Call { callee, .. } | HirExprKind::FnRef(callee) => fn_span(*callee)
                .filter(|_| {
                    hir.functions
                        .iter()
                        .any(|f| f.id == *callee && short_name(&f.name) == ident)
                }),
            HirExprKind::StructInit { struct_id, .. }
            | HirExprKind::FieldRead { struct_id, .. } => struct_span(*struct_id),
            HirExprKind::EnumVariantInit { enum_id, .. } => {
                hir.enums.iter().find(|e| e.id == *enum_id).map(|e| e.span)
            }
            _ => None,
        });
    let real = |s: &Span| !s.is_dummy() && a.file(*s).is_some();
    hit.filter(real).or_else(|| {
        // By name: functions, then types.
        let fns = hir
            .functions
            .iter()
            .filter(|f| short_name(&f.name) == ident)
            .map(|f| f.span);
        let types = hir
            .structs
            .iter()
            .filter(|s| short_name(&s.name) == ident)
            .map(|s| s.span)
            .chain(
                hir.enums
                    .iter()
                    .filter(|e| short_name(&e.name) == ident)
                    .map(|e| e.span),
            );
        fns.chain(types).filter(real).min_by_key(|s| {
            // Prefer the buffer itself.
            a.file(*s).is_none_or(|(p, _)| p != key)
        })
    })
}

/// The `name:` of every struct literal field `name` in the buffer.
pub(crate) struct FieldInits<'t> {
    /// The struct's name as written in a literal.
    owner: &'t str,
    name: &'t str,
    text: &'t str,
    out: Vec<(usize, usize)>,
}

impl AstVisitor for FieldInits<'_> {
    fn expr(&mut self, e: &Expr) {
        let ExprKind::StructInit { path, fields, .. } = &e.kind else {
            return;
        };
        if path.segments.last().is_none_or(|s| s.name != self.owner) {
            return;
        }
        for (n, value) in fields.iter().filter(|(n, _)| n == self.name) {
            // The name is the last one before the value (`x: v`), or the
            // value itself (`P { x }`).
            let end = (value.span.end as usize).min(self.text.len());
            let head = &self.text[..end];
            if let Some(s) = head.rfind(n.as_str()) {
                self.out.push((s, s + n.len()));
            }
        }
    }
}

/// Every reference to what the identifier at `off` names, its declaration
/// included, as byte ranges: a local through the buffer's scopes, a
/// `.member` every same-named member, anything else every same-named
/// non-local identifier.
pub(crate) fn references(doc: &Doc, key: &str, off: usize) -> Option<Vec<(usize, usize)>> {
    let text = doc.text.as_str();
    let (name, start) = ident_at(text, off)?;
    let (tokens, _) = nabic_lexer::Lexer::new(text, key.to_owned()).lex_all();
    let words: Vec<(usize, usize)> = tokens
        .iter()
        .filter(|t| matches!(&t.kind, nabic_lexer::TokenKind::Ident(n) if n == name))
        .map(|t| (t.span.start as usize, t.span.end as usize))
        .collect();
    let is_member = |s: usize| s > 0 && text.as_bytes()[s - 1] == b'.';
    let program = parse_doc(text, key);
    let walk = locals(&program, usize::MAX);
    // A binding's name, mapped to its declaring span.
    let binding = |s: usize| {
        walk.binds.iter().find_map(|(n, at)| {
            (n == name && name_range(text, at.start as usize, name).is_some_and(|r| r.0 == s))
                .then_some(*at)
        })
    };
    let resolve = |s: usize| binding(s).or_else(|| resolve_local(&program, s));
    let mut out: Vec<(usize, usize)> = if is_member(start) {
        let a = doc.good.as_ref()?;
        let m = member_at(a, key, text, name, start)?;
        // `.name` of the same field / method.
        let same = |w: &(usize, usize)| {
            is_member(w.0)
                && member_at(a, key, text, name, w.0).is_some_and(|o| o.def_span() == m.def_span())
        };
        let mut v: Vec<_> = words.iter().copied().filter(same).collect();
        let here = a.file(m.def_span()).is_some_and(|(p, _)| p == key);
        if let Some(r) = m.name_range(text).filter(|_| here) {
            v.push(r);
        }
        if let Member::Field(s, _) = m {
            let mut inits = FieldInits {
                owner: short_name(s.name.split("__").next().unwrap_or(&s.name)),
                name,
                text,
                out: Vec::new(),
            };
            for item in &program.items {
                walk_item(&mut inits, item);
            }
            program.stmts.iter().for_each(|s| walk_stmt(&mut inits, s));
            v.extend(inits.out);
        }
        v
    } else if let Some(target) = resolve(start) {
        words
            .iter()
            .copied()
            .filter(|w| resolve(w.0) == Some(target))
            .collect()
    } else {
        words
            .iter()
            .copied()
            .filter(|w| !is_member(w.0) && resolve(w.0).is_none())
            .collect()
    };
    out.sort_unstable();
    out.dedup();
    (!out.is_empty()).then_some(out)
}

/// Is what the identifier at `off` names declared in this buffer?
pub(crate) fn defined_here(doc: &Doc, key: &str, off: usize) -> bool {
    let text = doc.text.as_str();
    let Some((name, start)) = ident_at(text, off) else {
        return false;
    };
    let a = doc.good.as_ref();
    if let Some(m) = a.and_then(|a| member_at(a, key, text, name, start)) {
        return a
            .and_then(|a| a.file(m.def_span()))
            .is_some_and(|(p, _)| p == key);
    }
    let program = parse_doc(text, key);
    let walk = locals(&program, off);
    if walk.found.is_some()
        || walk.binds.iter().any(|(n, at)| {
            n == name && name_range(text, at.start as usize, name).is_some_and(|r| r.0 == start)
        })
    {
        return true;
    }
    a.and_then(|a| {
        let span = definition(a, key, name, off)?;
        Some(a.file(span)?.0 == key)
    })
    .unwrap_or(false)
}

/// The references `rename` edits for the identifier at `off`: `None`
/// with nothing there, an error naming where a name declared outside
/// the buffer comes from.
pub(crate) fn renamable(
    doc: &Doc,
    key: &str,
    off: usize,
) -> Result<Option<Vec<(usize, usize)>>, String> {
    let text = doc.text.as_str();
    let Some((name, start)) = ident_at(text, off) else {
        return Ok(None);
    };
    if nabic_lexer::token::keyword_kind(name).is_some() {
        return Err(format!("`{name}` is a keyword"));
    }
    let Some(refs) = references(doc, key, off) else {
        return Ok(None);
    };
    if defined_here(doc, key, off) {
        return Ok(Some(refs));
    }
    let a = doc.good.as_ref();
    let span = a.and_then(|a| {
        member_at(a, key, text, name, start)
            .map(|m| m.def_span())
            .or_else(|| indexed_member(a, key, text, name, start).map(|m| m.span))
            .or_else(|| definition(a, key, name, off))
    });
    let source = span.and_then(|s| a?.sources.iter().find(|f| f.file_id == s.file));
    let why = match (span, source) {
        (None, _) => "nothing in this file declares it".to_owned(),
        (Some(_), Some(f)) if f.package.is_empty() => {
            "it is declared in another file; rename it there".to_owned()
        }
        (Some(_), Some(f)) if !nabic_core::in_toolchain(&f.package) => {
            format!("it is defined in dependency `{}`", f.package)
        }
        _ => "it is defined in the standard library".to_owned(),
    };
    Err(format!("cannot rename `{name}`: {why}"))
}
