//! Signatures as plain text: as written in the source when the item's
//! span leads to its header there, else printed from the AST.

use nabic_ast::{
    Attribute, ConstExpr, FnDef, GenericParam, Generics, ImplDef, Literal, Path, ThrowsKind, Ty,
    TyKind, UseTree, UseTreeKind, WherePred,
};
use nabic_core::Visibility;
use nabic_diagnostic::Span;

/// What ends a header as written.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum End {
    /// `{` or `;`: an item, a method, a field.
    Body,
    /// `;` alone: a `const` / `static` with its value.
    Semi,
    /// `,` too: an enum variant.
    Comma,
}

/// The source text of every file, by span file id.
pub(crate) struct Texts<'a>(pub(crate) &'a [(String, String)]);

impl Texts<'_> {
    /// The header at `span` as written: from its start past attributes
    /// and doc comments up to the `{` / `;` / `,` (or closing bracket)
    /// that ends it, lines after the first dedented. None when the text
    /// there does not hold `words` in order (a generated item).
    /// `end` says what ends it.
    pub(crate) fn header(&self, span: Span, words: &[&str], end: End) -> Option<String> {
        let id = usize::try_from(span.file).ok()?.checked_sub(1)?;
        let text = &self.0.get(id)?.1;
        let mut start = usize::try_from(span.start).ok()?;
        // A span may start at the keyword: take the modifiers before it
        // on its line (`pub`, `pub(pkg)`, `const`, ..) too.
        let line = text.get(..start)?.rfind('\n').map_or(0, |k| k + 1);
        let pre = text.get(line..start)?;
        let mods = pre.split_whitespace();
        if mods.clone().count() > 0 && mods.clone().all(is_modifier) {
            start = line + (pre.len() - pre.trim_start().len());
        }
        let src = text.get(start..)?;
        let b = src.as_bytes();
        let mut i = skip_attrs(b, 0);
        let col = text[..start + i].rsplit('\n').next().map_or(0, str::len);
        let head_start = i;
        let mut depth = 0i32;
        while i < b.len() {
            match b[i] {
                b'"' => i = quote_end(b, i),
                b'/' if b.get(i + 1) == Some(&b'/') => {
                    while i < b.len() && b[i] != b'\n' {
                        i += 1;
                    }
                }
                c if depth == 0 && stops(c, end) => break,
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => depth -= 1,
                _ => {}
            }
            i += 1;
        }
        let raw = src.get(head_start..i)?.trim_end();
        let mut out = String::new();
        for (n, line) in raw.lines().enumerate() {
            if n > 0 {
                out.push('\n');
                let cut = line.len() - line.trim_start_matches(' ').len();
                out.push_str(&line[cut.min(col)..]);
            } else {
                out.push_str(line);
            }
        }
        // Comments inside a header are not part of the signature.
        let toks: Vec<&str> = out.split(|c: char| !is_word(c)).collect();
        let mut at = 0;
        for w in words {
            at += toks[at..].iter().position(|t| t == w)? + 1;
        }
        Some(out)
    }
}

fn is_modifier(w: &str) -> bool {
    w == "pub"
        || (w.starts_with("pub(") && w.ends_with(')'))
        || matches!(w, "const" | "unsafe" | "async" | "extern" | "\"C\"")
}

/// Whether byte `c` at depth 0 ends a header.
const fn stops(c: u8, end: End) -> bool {
    match c {
        b')' | b']' | b';' => true,
        b'{' | b'}' => !matches!(end, End::Semi),
        b',' => matches!(end, End::Comma),
        _ => false,
    }
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

const fn quote_end(b: &[u8], mut i: usize) -> usize {
    i += 1;
    while i < b.len() && b[i] != b'"' {
        if b[i] == b'\\' {
            i += 1;
        }
        i += 1;
    }
    i
}

/// Past whitespace, `#[..]` attributes and `//` comments at `i`.
fn skip_attrs(b: &[u8], mut i: usize) -> usize {
    loop {
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if b.get(i) == Some(&b'#') && b.get(i + 1) == Some(&b'[') {
            let mut depth = 0;
            while i < b.len() {
                match b[i] {
                    b'"' => i = quote_end(b, i),
                    b'[' => depth += 1,
                    b']' => {
                        depth -= 1;
                        if depth == 0 {
                            i += 1;
                            break;
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
        } else if b.get(i) == Some(&b'/') && b.get(i + 1) == Some(&b'/') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if b.get(i) == Some(&b'/') && b.get(i + 1) == Some(&b'*') {
            let mut depth = 0;
            while i < b.len() {
                if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
                    depth += 1;
                    i += 1;
                } else if b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
                    depth -= 1;
                    i += 1;
                    if depth == 0 {
                        i += 1;
                        break;
                    }
                }
                i += 1;
            }
        } else {
            return i;
        }
    }
}

/// Whether `attrs` mark a `const fn`.
fn is_const(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|a| a.name() == nabic_ast::CONST_FN_ATTR)
}

/// `f`'s signature as written, `pub` included when it is public; with
/// `full`, `const` too.
pub(crate) fn fn_sig(f: &FnDef, full: bool) -> String {
    let mut s = String::new();
    if f.vis == Visibility::Pub {
        s.push_str("pub ");
    }
    for (on, kw) in [
        (full && is_const(&f.attrs), "const "),
        (f.is_unsafe, "unsafe "),
        (f.is_async, "async "),
        (f.is_generator, "yield "),
    ] {
        if on {
            s.push_str(kw);
        }
    }
    let params: Vec<String> = f
        .params
        .iter()
        .filter(|p| !p.name.starts_with('#'))
        .map(|p| {
            let mut x = String::new();
            if p.by_ref {
                x.push_str("ref ");
            }
            x.push_str(&p.name);
            if let Some(t) = &p.ty {
                x.push_str(": ");
                // `xs: ...T` is `xs: Slice<T>`.
                match &t.kind {
                    TyKind::Path(path) if p.variadic => {
                        x.push_str("...");
                        if let Some(e) = path.segments.last().and_then(|s| s.generics.first()) {
                            x.push_str(&ty(e));
                        }
                    }
                    _ => x.push_str(&ty(t)),
                }
            }
            if p.default.is_some() {
                x.push_str(" = ..");
            }
            x
        })
        .collect();
    s.push_str("fn ");
    s.push_str(&f.name);
    s.push_str(&generics(&f.generics));
    s.push('(');
    s.push_str(&params.join(", "));
    s.push(')');
    if let ThrowsKind::Typed(tys) = &f.throws {
        let untyped = matches!(tys.as_slice(), [t] if nabic_ast::is_untyped_throws(t));
        if untyped || tys.is_empty() {
            s.push_str(" throws");
        } else {
            let e: Vec<String> = tys.iter().map(ty).collect();
            s.push_str(&format!(" throws ({})", e.join(" | ")));
        }
    } else if matches!(f.throws, ThrowsKind::Untyped) {
        s.push_str(" throws");
    }
    if let Some(r) = &f.ret_ty {
        s.push_str(" -> ");
        s.push_str(&ty(r));
    }
    if full {
        s.push_str(&where_clause(&f.where_clause));
    }
    s
}

fn where_clause(w: &[WherePred]) -> String {
    if w.is_empty() {
        return String::new();
    }
    let ps: Vec<String> = w
        .iter()
        .map(|p| format!("{}: {}", ty(&p.ty), list(&p.bounds, " + ")))
        .collect();
    format!(" where {}", ps.join(", "))
}

pub(crate) fn generics(g: &Generics) -> String {
    if g.params.is_empty() {
        return String::new();
    }
    let ps: Vec<String> = g
        .params
        .iter()
        .map(|p| match p {
            GenericParam::Type { name, bounds, .. } if bounds.is_empty() => name.clone(),
            GenericParam::Type { name, bounds, .. } => {
                format!("{name}: {}", list(bounds, " + "))
            }
            GenericParam::Const { name, ty: t } => format!("const {name}: {}", ty(t)),
        })
        .collect();
    format!("<{}>", ps.join(", "))
}

fn list(ts: &[Ty], sep: &str) -> String {
    ts.iter().map(ty).collect::<Vec<_>>().join(sep)
}

pub(crate) fn path(p: &Path) -> String {
    p.segments
        .iter()
        .map(|seg| {
            if seg.generics.is_empty() {
                seg.name.clone()
            } else {
                format!("{}<{}>", seg.name, list(&seg.generics, ", "))
            }
        })
        .collect::<Vec<_>>()
        .join("::")
}

/// A type as written.
pub(crate) fn ty(t: &Ty) -> String {
    match &t.kind {
        TyKind::Nest(inner) => ty(inner),
        TyKind::Binding(n, b) => format!("{n} = {}", ty(b)),
        TyKind::QPath(q) => format!(
            "<{} as {}>::{}",
            ty(&q.self_ty),
            path(&q.trait_path),
            q.item
        ),
        TyKind::Path(p) => path(p),
        TyKind::Sum(ts) => list(ts, " | "),
        TyKind::Tuple(ts) => format!("({})", list(ts, ", ")),
        TyKind::Fn(args, ret, _) => format!("fn({}) -> {}", list(args, ", "), ty(ret)),
        TyKind::Throws { ok, err } => format!("throws ({}) -> {}", ty(err), ty(ok)),
        TyKind::Array(e, n) => format!("[{}; {}]", ty(e), const_text(n)),
        TyKind::Any => "any".to_owned(),
        TyKind::AnyTrait(ps) => {
            let b: Vec<&str> = ps
                .iter()
                .filter_map(|p| p.segments.last().map(|s| s.name.as_str()))
                .collect();
            format!("any {}", b.join(" + "))
        }
        TyKind::Some(ts) => match ts.as_slice() {
            [t] => format!("some {}", ty(t)),
            _ => format!("some ({})", list(ts, " + ")),
        },
        TyKind::None => "none".to_owned(),
        TyKind::SelfUpper => "Self".to_owned(),
        TyKind::Never => "!".to_owned(),
        TyKind::Const(c) => const_text(c),
    }
}

fn const_text(c: &ConstExpr) -> String {
    match c {
        ConstExpr::Literal(Literal::Int(s) | Literal::Float(s)) => s.clone(),
        ConstExpr::Path(p) => p
            .segments
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
            .join("::"),
        _ => "_".to_owned(),
    }
}

/// An impl's header: `impl<T> Debug for List<T>`.
pub(crate) fn impl_header(im: &ImplDef) -> String {
    let mut s = format!("impl{}", generics(&im.generics));
    if let Some(tr) = &im.trait_path {
        s.push(' ');
        if im.negative {
            s.push('!');
        }
        // A derive writes the trait's full path; show the name.
        match tr.segments.last().filter(|_| tr.origin.is_some()) {
            Some(seg) => s.push_str(&path(&Path {
                segments: vec![seg.clone()],
                span: tr.span,
                origin: None,
            })),
            None => s.push_str(&path(tr)),
        }
        s.push_str(" for");
    }
    s.push(' ');
    s.push_str(&ty(&im.self_ty));
    s.push_str(&where_clause(&im.where_clause));
    s
}

/// A `use` tree as written, and the paths it names with the name each
/// binds: `a::{B, c::D as E, self}` gives (`a::B`, `B`), (`a::c::D`,
/// `E`), (`a`, `a`); a glob binds `*`.
pub(crate) fn use_tree(t: &UseTree, out: &mut Vec<(String, String)>) -> String {
    use_at(&[], t, out)
}

fn use_at(prefix: &[String], t: &UseTree, out: &mut Vec<(String, String)>) -> String {
    let segs: Vec<String> = t.prefix.segments.iter().map(|s| s.name.clone()).collect();
    let mut full = prefix.to_vec();
    full.extend(segs.iter().cloned());
    let mut text = segs.join("::");
    let join = |text: &mut String, tail: &str| {
        if !text.is_empty() {
            text.push_str("::");
        }
        text.push_str(tail);
    };
    match &t.kind {
        UseTreeKind::Simple(alias) => {
            let last = full.last().cloned().unwrap_or_default();
            // `a::{self}` binds `a`.
            let (path, bound) = if last == "self" {
                let p = full[..full.len() - 1].join("::");
                let n = full
                    .len()
                    .checked_sub(2)
                    .map_or_else(String::new, |i| full[i].clone());
                (p, n)
            } else {
                (full.join("::"), last)
            };
            let bound = alias.clone().unwrap_or(bound);
            if let Some(a) = alias {
                text.push_str(" as ");
                text.push_str(a);
            }
            out.push((path, bound));
        }
        UseTreeKind::Glob | UseTreeKind::GlobRecursive => {
            join(
                &mut text,
                if matches!(t.kind, UseTreeKind::Glob) {
                    "*"
                } else {
                    "**"
                },
            );
            let mut p = full.join("::");
            p.push_str(if p.is_empty() { "*" } else { "::*" });
            out.push((p, "*".to_owned()));
        }
        UseTreeKind::Group(ts) => {
            let parts: Vec<String> = ts.iter().map(|x| use_at(&full, x, out)).collect();
            join(&mut text, &format!("{{{}}}", parts.join(", ")));
        }
    }
    text
}
