//! Hover text, and the member a `.name` at the cursor names.

use crate::analysis::{Analysis, short_name, signature, ty_name};
use crate::cursor::{balanced_close, ident_at, name_range, receiver_start};
use nabic_diagnostic::Span;
use nabic_hir::{HirExprKind, HirTy};
use nabic_lexer::{Lexer, TokenKind};
use nabic_parser::MetaKind;

/// Hover markdown: the code in a `nabi` fence, then a function's doc
/// text from its `#[doc]` attributes.
pub fn hover(a: &Analysis, key: &str, text: &str, off: usize) -> Option<String> {
    let hir = a.hir.as_ref()?;
    // A check with errors: nothing at an error, whose code has no type.
    let at_error = |d: &nabic_diagnostic::NabiDiagnostic| {
        d.is_error()
            && d.primary.as_ref().is_some_and(|l| {
                a.in_file(l.span, key)
                    && (l.span.start as usize) <= off
                    && off <= (l.span.end as usize).max(l.span.start as usize + 1)
            })
    };
    if a.partial && a.diags.iter().any(at_error) {
        return None;
    }
    let fenced = |code: String| format!("```nabi\n{code}\n```");
    let with_doc = |f: &nabic_hir::HirFunction| {
        let doc = a.fn_doc(f.span);
        let mut sig = signature(hir, f);
        // `x: some Tr`: the parameter as written, not the instance's type.
        for (name, some) in a.some_params(f.span) {
            sig = with_param_ty(&sig, &name, &some);
        }
        // `-> some Tr`: the body's type stays hidden.
        if let Some(some) = a.opaque_ret(f.span) {
            if let Some(i) = sig.rfind(" -> ") {
                sig.truncate(i);
                sig = format!("{sig} -> {some}");
            }
        }
        let code = fenced(sig);
        if doc.is_empty() {
            code
        } else {
            format!("{code}\n\n{doc}")
        }
    };
    let ident = ident_at(text, off).map(|(s, _)| s);
    // `.field` / `.method`: the member, with its doc comment.
    if let Some((name, start)) = ident_at(text, off) {
        match member_at(a, key, text, name, start) {
            Some(Member::Method(f)) => return Some(with_doc(f)),
            Some(Member::Field(s, name)) => {
                let ty = s.fields.iter().find(|f| f.name == name)?;
                let owner = ty_name(hir, &HirTy::Struct(s.id));
                let code = fenced(format!("{owner}.{name}: {}", ty_name(hir, &ty.ty)));
                let doc = a.field_doc(s.span, name);
                return Some(if doc.is_empty() {
                    code
                } else {
                    format!("{code}\n\n{doc}")
                });
            }
            None => {
                if let Some(m) = indexed_member(a, key, text, name, start) {
                    let code = fenced(m.sig);
                    return Some(if m.doc.is_empty() {
                        code
                    } else {
                        format!("{code}\n\n{}", m.doc)
                    });
                }
            }
        }
    }
    // Offsets into an older compile of an edited buffer point elsewhere.
    let fresh = a
        .source_map
        .files()
        .iter()
        .any(|(p, t)| p == key && t == text);
    let at_off = |s: Span| (s.start as usize..s.end as usize).contains(&off);
    if let Some(e) = fresh.then(|| a.find_expr(key, at_off, false)).flatten() {
        if let HirExprKind::Call { callee, .. } | HirExprKind::FnRef(callee) = &e.kind {
            let f = hir.functions.iter().find(|f| f.id == *callee)?;
            if ident == Some(short_name(&f.name)) {
                return Some(with_doc(f));
            }
        }
        let ty = a.shown_ty(hir, &e.ty);
        let src = text.get(e.span.start as usize..e.span.end as usize);
        return Some(fenced(match ident {
            Some(name) if src == Some(name) => format!("{name}: {ty}"),
            _ => ty,
        }));
    }
    // A binding site (`let x`, a parameter) has no expression: the
    // `let` it starts, else the buffer's locals and items of that name.
    let name = ident?;
    if let Some(ty) = fresh
        .then(|| let_site_ty(a, key, text, name, off))
        .flatten()
    {
        return Some(fenced(format!("{name}: {}", ty_name(hir, &ty))));
    }
    if let Some(decl) = a.some_alias(name) {
        return Some(fenced(decl));
    }
    let doc_fns = hir
        .functions
        .iter()
        .filter(|f| a.file(f.span).is_some_and(|(p, _)| p == key));
    for f in doc_fns {
        if short_name(&f.name) == name {
            return Some(with_doc(f));
        }
        if let Some(l) = f.locals.iter().find(|l| l.name.as_deref() == Some(name)) {
            return Some(fenced(format!("{name}: {}", a.shown_ty(hir, &l.ty))));
        }
    }
    None
}

/// `sig` with parameter `name`'s type replaced by `ty`.
pub fn with_param_ty(sig: &str, name: &str, ty: &str) -> String {
    let Some(open) = sig.find('(') else {
        return sig.to_owned();
    };
    let mut depth = 0;
    let mut start = open + 1;
    let mut out = sig[..=open].to_owned();
    for (i, c) in sig[open + 1..]
        .char_indices()
        .map(|(i, c)| (i + open + 1, c))
    {
        let end = match c {
            '(' | '<' | '[' => {
                depth += 1;
                None
            }
            ')' | '>' | ']' if depth > 0 => {
                depth -= 1;
                None
            }
            ',' if depth == 0 => Some(i),
            ')' => Some(i),
            _ => None,
        };
        if let Some(end) = end {
            let part = &sig[start..end];
            let label = format!("{name}: ");
            match part.trim_start().strip_prefix(&label) {
                Some(_) => {
                    let lead = &part[..part.len() - part.trim_start().len()];
                    out.push_str(&format!("{lead}{label}{ty}"));
                }
                None => out.push_str(part),
            }
            out.push(c);
            start = end + 1;
            if c == ')' {
                out.push_str(&sig[start..]);
                return out;
            }
        }
    }
    sig.to_owned()
}

/// The type of the local a `let` at `off` declares: the nearest `let
/// name` whose initializer follows in the same statement (a redeclared
/// name is another local).
pub fn let_site_ty(a: &Analysis, key: &str, text: &str, name: &str, off: usize) -> Option<HirTy> {
    let mut best: Option<(u32, HirTy)> = None;
    for f in &a.hir.as_ref()?.functions {
        let Some(body) = &f.body else { continue };
        let mut scan = |b: &nabic_hir::HirBlock| {
            for s in &b.stmts {
                let nabic_hir::HirStmt::Let { local, init } = s else {
                    continue;
                };
                let start = init.span.start;
                let Some(l) = f.locals.iter().find(|l| l.id == *local) else {
                    continue;
                };
                let between = text.get(off..start as usize).unwrap_or(";");
                if l.name.as_deref() == Some(name)
                    && start as usize > off
                    && !between.contains(';')
                    && a.in_file(init.span, key)
                    && best.as_ref().is_none_or(|(s, _)| start < *s)
                {
                    best = Some((start, l.ty.clone()));
                }
            }
        };
        scan(body);
        nabic_hir::each_block(body, &mut |e| match &e.kind {
            HirExprKind::Block(b)
            | HirExprKind::While { body: b, .. }
            | HirExprKind::Loop { body: b, .. }
            | HirExprKind::If { then_block: b, .. } => scan(b),
            _ => {}
        });
    }
    best.map(|(_, t)| t)
}

/// If `off` is inside a `name!(..)` call of a macro defined in this
/// file, its expansion as markdown (expand in place, through hover).
pub fn macro_hover(text: &str, off: usize) -> Option<String> {
    let (toks, _) = Lexer::new(text, "").lex_all();
    for (i, t) in toks.iter().enumerate() {
        let TokenKind::Ident(name) = &t.kind else {
            continue;
        };
        if toks.get(i + 1).map(|x| &x.kind) != Some(&TokenKind::Bang) {
            continue;
        }
        let open = i + 2;
        if !matches!(
            toks.get(open).map(|x| &x.kind),
            Some(TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace)
        ) {
            continue;
        }
        let Some(close) = balanced_close(&toks, open) else {
            continue;
        };
        let (lo, hi) = (t.span.start as usize, toks[close].span.end as usize);
        if off < lo || off > hi {
            continue;
        }
        let def = nabic_parser::collect_meta_blocks(&toks, "")
            .into_iter()
            .find(|d| d.kind == MetaKind::Macro && &d.name == name)?;
        let call = toks[open + 1..close].to_vec();
        let expanded = nabic_parser::expand_to_text(&def, &call, t.span)?;
        return Some(format!("```nabi\n{expanded}\n```"));
    }
    None
}

/// `.name` (at byte `start` of `text`) by the receiver's type: its
/// member in the index, called or not.
pub fn indexed_member(
    a: &Analysis,
    key: &str,
    text: &str,
    name: &str,
    start: usize,
) -> Option<crate::members::Member> {
    let dot = start.checked_sub(1)?;
    if text.as_bytes().get(dot) != Some(&b'.') {
        return None;
    }
    let recv = receiver_start(text, dot);
    let e = a.find_expr(
        key,
        |s| s.start as usize == recv && s.end as usize <= dot,
        true,
    )?;
    let ty = ty_name(a.hir.as_ref()?, &e.ty);
    let found = crate::members::Members::of(a)
        .get(&ty)
        .find(|m| m.name == name)
        .cloned();
    found
}

/// What `.name` names: a struct field or a method.
pub enum Member<'a> {
    Field(&'a nabic_hir::HirStructDef, &'a str),
    Method(&'a nabic_hir::HirFunction),
}

impl Member<'_> {
    pub const fn def_span(&self) -> Span {
        match self {
            Self::Field(s, _) => s.span,
            Self::Method(f) => f.span,
        }
    }

    /// The name at the definition in its file's `text`.
    pub fn name_range(&self, text: &str) -> Option<(usize, usize)> {
        match self {
            // The field's `name:` inside the struct's braces (not a
            // mention in a doc comment).
            Self::Field(s, name) => {
                let start = s.span.start as usize;
                let mut at = start + text.get(start..)?.find('{')?;
                loop {
                    let (s, e) = name_range(text, at, name)?;
                    if text[e..].trim_start().starts_with(':') {
                        return Some((s, e));
                    }
                    at = e;
                }
            }
            Self::Method(f) => name_range(text, f.span.start as usize, short_name(&f.name)),
        }
    }
}

/// The member `name` (at byte `start` of `text`, after a `.`) of the
/// receiver before it, from the typed expressions of `a`.
pub fn member_at<'a>(
    a: &'a Analysis,
    key: &str,
    text: &str,
    name: &str,
    start: usize,
) -> Option<Member<'a>> {
    let dot = start.checked_sub(1)?;
    if text.as_bytes().get(dot) != Some(&b'.') {
        return None;
    }
    let receiver = receiver_start(text, dot);
    let hir = a.hir.as_ref()?;
    let mut found = None;
    for f in &hir.functions {
        let Some(body) = &f.body else { continue };
        crate::analysis::each_block(body, &mut |e| {
            if found.is_some() || e.span.start as usize != receiver || !a.in_file(e.span, key) {
                return;
            }
            match &e.kind {
                HirExprKind::FieldRead {
                    struct_id,
                    field_index,
                    ..
                } => {
                    let s = hir.structs.iter().find(|s| s.id == *struct_id);
                    let field = s.and_then(|s| s.fields.get(*field_index as usize));
                    if let (Some(s), Some(fd)) = (s, field) {
                        if fd.name == name {
                            found = Some(Member::Field(s, fd.name.as_str()));
                        }
                    }
                }
                HirExprKind::Call { callee, .. } => {
                    found = hir
                        .functions
                        .iter()
                        .find(|g| {
                            g.id == *callee
                                && short_name(&g.name) == name
                                && g.params.first().is_some_and(|p| p.name == "self")
                        })
                        .map(Member::Method);
                }
                _ => {}
            }
        });
        if found.is_some() {
            break;
        }
    }
    found
}
