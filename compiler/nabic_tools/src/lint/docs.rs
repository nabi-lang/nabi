//! `missing_docs` and `plain_module_doc`.

#[allow(clippy::wildcard_imports)]
use crate::lint::*;

/// `missing_docs`: the `pub` items of `items` without a doc comment, and
/// the `pub` methods of their inherent impls and of `pub` traits. A
/// `mod m;` (or the empty stub a library stage keeps) is left out: its
/// file's `//!` documents it.
pub(crate) fn missing_docs(items: &[Item], out: &mut Vec<Finding>) {
    // `#[doc(hidden)]` counts as documented; an item the parser lifted
    // is no item the user wrote. `const fn`, `async fn` and the like
    // carry markers too, and need docs.
    let written = [
        nabic_ast::CONST_FN_ATTR,
        nabic_ast::CORO_FN_ATTR,
        nabic_ast::EXTERN_FN_ATTR,
        nabic_ast::C_VARIADIC_ATTR,
        nabic_ast::UNIT_RET_ATTR,
    ];
    let documented = |attrs: &[Attribute]| {
        attrs.iter().any(|a| {
            a.name() == "doc"
                || a.path
                    .first()
                    .is_some_and(|p| p.starts_with('#') && !written.contains(&p.as_str()))
        })
    };
    let hit = |what: &str, name: &str, span: Span| Finding {
        lint: "missing_docs",
        span,
        msg: format!("`pub {what} {}` has no doc comment", leaf(name)),
        notes: vec![format!(
            "help: add a `///` line above it that says what it {}",
            if what == "fn" { "does" } else { "is" }
        )],
        secondary: None,
        base: None,
        fix: None,
    };
    for item in items {
        match item {
            Item::Impl(im) if im.trait_path.is_none() => {
                for ii in &im.items {
                    if let ImplItem::Method(m) = ii {
                        if m.vis == Visibility::Pub && !documented(&m.attrs) {
                            out.push(hit("fn", &m.name, m.span));
                        }
                    }
                }
                continue;
            }
            Item::Mod(m) => {
                if let (Some(inner), Visibility::Pub) = (&m.items, m.vis) {
                    if !inner.is_empty() && !documented(&m.attrs) {
                        out.push(hit("mod", &m.name, m.span));
                    }
                    missing_docs(inner, out);
                }
                continue;
            }
            _ => {}
        }
        let Some((what, name, vis, attrs)) = crate::doc::item_info(item) else {
            continue;
        };
        if vis != Visibility::Pub {
            continue;
        }
        if !documented(attrs) {
            out.push(hit(what, name, item.span()));
        }
        if let Item::Trait(t) = item {
            for ti in &t.items {
                if let TraitItem::Method(m) = ti {
                    if !documented(&m.attrs) {
                        out.push(hit("fn", &m.name, m.span));
                    }
                }
            }
        }
    }
}

/// Every function in `items`, impl / trait methods and inline modules
/// included.
pub(crate) fn each_fn<'i>(items: &'i [Item], visit: &mut dyn FnMut(&'i FnDef)) {
    for i in items {
        match i {
            Item::Fn(f) => visit(f),
            Item::Impl(im) => im.items.iter().for_each(|ii| {
                if let ImplItem::Method(m) = ii {
                    visit(m);
                }
            }),
            Item::Trait(t) => t.items.iter().for_each(|ti| {
                if let TraitItem::Method(m) = ti {
                    visit(m);
                }
            }),
            Item::Mod(m) => each_fn(m.items.as_deref().unwrap_or(&[]), visit),
            _ => {}
        }
    }
}

/// The last segment of a (library stage's qualified) item name.
pub(crate) fn leaf(name: &str) -> &str {
    name.rsplit("::").next().unwrap_or(name)
}

/// Trait names declared in `items`.
pub(crate) fn collect_traits(items: &[Item], out: &mut HashSet<String>) {
    for i in items {
        match i {
            Item::Trait(t) => {
                out.insert(leaf(&t.name).to_owned());
            }
            Item::Mod(m) => collect_traits(m.items.as_deref().unwrap_or(&[]), out),
            _ => {}
        }
    }
}

pub(crate) fn collect_variants(items: &[Item], out: &mut HashSet<String>) {
    for i in items {
        match i {
            Item::Enum(e) => out.extend(e.variants.iter().map(|v| v.name.clone())),
            Item::Mod(m) => collect_variants(m.items.as_deref().unwrap_or(&[]), out),
            _ => {}
        }
    }
}

/// `plain_module_doc`: a library's root file, or the file of a module
/// public from outside, that has no `//!` doc but starts with `//`
/// lines: they read as the module's doc and are not one.
pub(crate) fn plain_module_doc(
    user: &[&SourceFile],
    own: &str,
    texts: &[(String, String)],
    out: &mut Vec<Finding>,
) {
    let skip = if own.is_empty() {
        0
    } else {
        own.split("::").count()
    };
    let rel = |f: &SourceFile| f.module.get(skip..).unwrap_or_default().to_vec();
    let library = user
        .iter()
        .any(|f| rel(f).is_empty() && f.path.file_name().is_some_and(|n| n == "lib.nabi"));
    if !library {
        return;
    }
    // `mod` items: (path, public, documented by `///` on the item).
    let mut decls: Vec<(Vec<String>, bool, bool)> = Vec::new();
    for f in user {
        mods(&rel(f), &f.items, &mut decls);
    }
    for f in user {
        let path = rel(f);
        let public = (1..=path.len()).all(|n| decls.iter().any(|d| d.0 == path[..n] && d.1));
        let outer = decls.iter().any(|d| d.0 == path && d.2);
        if !public || outer || !crate::doc::doc_text(&f.attrs).is_empty() {
            continue;
        }
        let Some((_, text)) = usize::try_from(f.file_id)
            .ok()
            .and_then(|i| i.checked_sub(1))
            .and_then(|i| texts.get(i))
        else {
            continue;
        };
        let body = text.strip_prefix('\u{feff}').unwrap_or(text);
        let bom = text.len() - body.len();
        let (at, line) = if body.starts_with("#!") && !body.starts_with("#![") {
            (bom + body.find('\n').map_or(body.len(), |k| k + 1), 2)
        } else {
            (bom, 1)
        };
        let first = text[at..].lines().next().unwrap_or_default();
        if !first.starts_with("//") || first.starts_with("///") || first.starts_with("//!") {
            continue;
        }
        let what = if path.is_empty() {
            "the package's".to_owned()
        } else {
            format!("module `{}`'s", path.join("::"))
        };
        out.push(Finding {
            lint: "plain_module_doc",
            span: Span {
                start: u32::try_from(at).unwrap_or(0),
                end: u32::try_from(at + first.len()).unwrap_or(0),
                line,
                col: 1,
                file: f.file_id,
            },
            msg: format!("this `//` comment reads as {what} doc, but only `//!` lines are one"),
            notes: vec![format!(
                "help: start each of its lines with `//!`: `//! {}`",
                lead(first)
            )],
            secondary: None,
            base: None,
            fix: None,
        });
    }
}

/// The first words of comment line `line`, cut with `..`.
fn lead(line: &str) -> String {
    let text = line.trim_start_matches('/').trim();
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.len() > 4 {
        format!("{} ..", words[..4].join(" "))
    } else {
        words.join(" ")
    }
}

fn mods(at: &[String], items: &[Item], out: &mut Vec<(Vec<String>, bool, bool)>) {
    for i in items {
        if let Item::Mod(m) = i {
            let mut p = at.to_vec();
            p.push(m.name.clone());
            let public = m.vis == Visibility::Pub && !nabic_ast::doc_hidden(&m.attrs);
            out.push((p.clone(), public, m.attrs.iter().any(|a| a.name() == "doc")));
            if let Some(inner) = &m.items {
                mods(&p, inner, out);
            }
        }
    }
}
