//! Attribute scopes: the levels each item and file sets.

#[allow(clippy::wildcard_imports)]
use crate::lint::*;

/// An attribute scope: `[start, end)` of file `file`, with the levels its
/// `#[allow/warn/deny]` attributes set.
pub(crate) struct Region {
    pub(crate) file: u32,
    pub(crate) start: u32,
    pub(crate) end: u32,
    pub(crate) levels: Vec<(Level, &'static str, Span)>,
}

/// A level set for a lint: the level and the note naming where (`None`:
/// the default).
pub(crate) type Set = (Level, Option<String>);

/// `(level, lint, attribute span)` for each lint an `allow` / `warn` /
/// `deny` attribute names; unknown names are reported to `out`.
pub(crate) fn levels_of(
    attrs: &[Attribute],
    out: &mut Vec<NabiDiagnostic>,
) -> Vec<(Level, &'static str, Span)> {
    let mut levels = Vec::new();
    for a in attrs {
        let Some(lvl) = Level::parse(a.name()) else {
            continue;
        };
        let AttrBody::Args(args) = &a.body else {
            continue;
        };
        for arg in args {
            let name = match arg {
                AttrArg::Word(w) => w.as_str(),
                AttrArg::Nested { path, .. } => path.last().map_or("", String::as_str),
                _ => "",
            };
            match expand(name) {
                Some(ls) => {
                    levels.extend(ls.into_iter().map(|l| (lvl, l, a.span)));
                }
                None => out.push(NabiDiagnostic::warning(
                    Code::LINT,
                    a.span,
                    unknown(name, ""),
                )),
            }
        }
    }
    levels
}

/// Where an item's text starts: its first attribute or its first token.
pub(crate) fn item_start(attrs: &[Attribute], span: Span) -> u32 {
    attrs
        .iter()
        .map(|a| a.span.start)
        .chain([span.start])
        .min()
        .unwrap_or(span.start)
}

/// One file's attribute scopes, outermost first: the file (its `mod`
/// declarations up the path, then `#![..]`), then each item down the tree.
pub(crate) fn file_regions(
    f: &SourceFile,
    user: &[&SourceFile],
    out: &mut Vec<NabiDiagnostic>,
) -> Vec<Region> {
    let mut attrs: Vec<Attribute> = Vec::new();
    // `#[allow(x)] mod m;` in the parent file covers all of `m.nabi`.
    for depth in 1..=f.module.len() {
        let (parent, name) = (&f.module[..depth - 1], &f.module[depth - 1]);
        for p in user.iter().filter(|p| p.module == parent) {
            for item in &p.items {
                if let Item::Mod(m) = item {
                    if m.name == *name && m.items.is_none() {
                        attrs.extend(m.attrs.iter().cloned());
                    }
                }
            }
        }
    }
    attrs.extend(f.attrs.iter().cloned());
    let mut regions = vec![Region {
        file: f.file_id,
        start: 0,
        end: u32::MAX,
        levels: levels_of(&attrs, out),
    }];
    let spans: Vec<(u32, &[Attribute])> = f
        .items
        .iter()
        .map(|i| (item_start(i.attrs(), i.span()), i.attrs().as_slice()))
        .collect();
    item_regions(f.file_id, &f.items, &spans, u32::MAX, &mut regions, out);
    regions
}

pub(crate) fn item_regions(
    file: u32,
    items: &[Item],
    starts: &[(u32, &[Attribute])],
    end: u32,
    regions: &mut Vec<Region>,
    out: &mut Vec<NabiDiagnostic>,
) {
    for (i, item) in items.iter().enumerate() {
        let (start, attrs) = starts[i];
        // Past the item's own keyword: what `#[derive]` generates carries
        // the attribute's span, before it.
        let next = starts
            .iter()
            .map(|(s, _)| *s)
            .filter(|s| *s > item.span().start)
            .min()
            .unwrap_or(end)
            .min(end);
        let levels = levels_of(attrs, out);
        if !levels.is_empty() {
            regions.push(Region {
                file,
                start,
                end: next,
                levels,
            });
        }
        let fns: Vec<&FnDef> = match item {
            Item::Impl(im) => im
                .items
                .iter()
                .filter_map(|ii| match ii {
                    ImplItem::Method(m) => Some(m),
                    _ => None,
                })
                .collect(),
            Item::Trait(t) => t
                .items
                .iter()
                .filter_map(|ti| match ti {
                    TraitItem::Method(m) => Some(m),
                    _ => None,
                })
                .collect(),
            Item::Mod(m) => {
                if let Some(inner) = &m.items {
                    let s: Vec<(u32, &[Attribute])> = inner
                        .iter()
                        .map(|i| (item_start(i.attrs(), i.span()), i.attrs().as_slice()))
                        .collect();
                    item_regions(file, inner, &s, next, regions, out);
                }
                Vec::new()
            }
            _ => Vec::new(),
        };
        let starts: Vec<u32> = fns.iter().map(|m| item_start(&m.attrs, m.span)).collect();
        for (k, m) in fns.iter().enumerate() {
            let levels = levels_of(&m.attrs, out);
            if levels.is_empty() {
                continue;
            }
            let mend = starts
                .iter()
                .copied()
                .filter(|s| *s > starts[k])
                .min()
                .unwrap_or(next)
                .min(next);
            regions.push(Region {
                file,
                start: starts[k],
                end: mend,
                levels,
            });
        }
    }
}

/// `#[name]` / `#[name = "msg"]` / `#[name("msg")]`: its message (empty
/// when it has none), or `None` without the attribute.
pub(crate) fn attr_msg(attrs: &[Attribute], name: &str) -> Option<String> {
    let a = attrs.iter().find(|a| a.name() == name)?;
    let lit = match &a.body {
        AttrBody::Value(l) => Some(l),
        AttrBody::Args(args) => args.iter().find_map(|x| match x {
            AttrArg::Lit(l) => Some(l),
            _ => None,
        }),
        AttrBody::Empty => None,
    };
    Some(match lit {
        Some(Literal::Str(s) | Literal::RawStr(s)) => s.trim_matches('"').to_owned(),
        _ => String::new(),
    })
}

pub(crate) fn has_attr(attrs: &[Attribute], names: &[&str]) -> bool {
    attrs
        .iter()
        .any(|a| names.contains(&a.name()) || a.path.first().is_some_and(|p| p.starts_with('#')))
}
