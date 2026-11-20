//! The navigation of a page, as the web kit's `web::apidoc::nav` writes
//! it for the docs site and the package registry: the breadcrumb, the sidebar (the
//! package, then one section each for its module tree, the page's items
//! by kind and its dependencies) and "On this page". The `nav_markup`
//! test is the twin of the kit's and checks the same fixture.
//!
//! The sidebar lists the page's items; "On this page" lists only its
//! sections and the methods of its types, so the two never repeat each
//! other.

use crate::doc::esc;
use crate::doc::model::{Item, Module, anchor};
use std::fmt::Write as _;

/// A package in the sidebar's head or its dependencies.
pub(crate) struct Package<'a> {
    pub(crate) name: &'a str,
    pub(crate) version: &'a str,
    /// From the page; empty when it has no docs here.
    pub(crate) href: String,
    /// The page is this package's root (the head only).
    pub(crate) here: bool,
}

/// One heading of a page: level 2 a section (`Structs`), 3 an item
/// (`struct.Qr`), 4 a member (`Qr.solve`, text ending in its name).
pub(crate) struct Head {
    pub(crate) level: u8,
    pub(crate) id: String,
    pub(crate) text: String,
}

/// Items of a page by kind, in the sidebar's order (re-exports are
/// listed on the page only).
const KINDS: [(&str, &str); 8] = [
    ("struct", "Structs"),
    ("enum", "Enums"),
    ("trait", "Traits"),
    ("type", "Type aliases"),
    ("fn", "Functions"),
    ("const", "Constants"),
    ("static", "Statics"),
    ("impl", "Implementations"),
];

/// With several method groups in "On this page", one with more members
/// than this starts folded.
const FOLD: usize = 15;

const SEP: &str = "<span class=\"sep\" aria-hidden=\"true\">\u{203a}</span>";

/// `Package` for a package's root module, else `Module`.
pub(crate) fn kind_of(m: &Module) -> &'static str {
    if m.path == m.root {
        "Package"
    } else {
        "Module"
    }
}

/// The way from `m`'s package root down to it: (label, href) for the
/// root (labelled by its full path) and each module below it; href
/// empty where there is no page.
pub(crate) fn trail(
    mods: &[Module],
    m: &Module,
    href: &dyn Fn(&Module) -> String,
) -> Vec<(String, String)> {
    let page = |path: &str| {
        mods.iter()
            .find(|q| q.path == path)
            .map(href)
            .unwrap_or_default()
    };
    let mut out = vec![(m.root.clone(), page(&m.root))];
    let mut at = m.root.clone();
    if m.path.len() > m.root.len() {
        for seg in m.path[m.root.len() + 2..].split("::") {
            at = format!("{at}::{seg}");
            out.push((seg.to_owned(), page(&at)));
        }
    }
    out
}

/// The breadcrumb: the page's `kind` and `segs` (trail), the last one
/// the page itself.
pub(crate) fn crumb(kind: &str, segs: &[(String, String)]) -> String {
    let mut out = format!(
        "<nav class=\"crumb api-crumb\" aria-label=\"Breadcrumb\"><span class=\"api-kind\">{}</span>",
        esc(kind)
    );
    for (k, (label, href)) in segs.iter().enumerate() {
        if k > 0 {
            out.push_str(SEP);
        }
        let last = k + 1 == segs.len();
        let (label, href) = (esc(label), esc(href));
        let _ = match (href.is_empty(), last) {
            (true, true) => write!(
                out,
                "<span class=\"here\" aria-current=\"page\">{label}</span>"
            ),
            (true, false) => write!(out, "<span>{label}</span>"),
            (false, true) => write!(
                out,
                "<a class=\"here\" href=\"{href}\" aria-current=\"page\">{label}</a>"
            ),
            (false, false) => write!(out, "<a href=\"{href}\">{label}</a>"),
        };
    }
    out.push_str("</nav>");
    out
}

/// A package's name and its version, if any, as a sidebar link's text.
fn named(p: &Package<'_>) -> String {
    if p.version.is_empty() {
        esc(p.name)
    } else {
        format!("{}<small>{}</small>", esc(p.name), esc(p.version))
    }
}

/// The sidebar: the package `head` (current on its root page), then a
/// section each for the module `tree`, the page's `items` and, under
/// `label`, the packages `deps`.
pub(crate) fn side(
    head: &Package<'_>,
    tree: &str,
    items: &str,
    label: &str,
    deps: &[Package<'_>],
) -> String {
    let ver = if head.version.is_empty() {
        String::new()
    } else {
        format!("<small>{}</small>", esc(head.version))
    };
    let cur = if head.here {
        " aria-current=\"page\""
    } else {
        ""
    };
    let mut out = format!(
        "<div class=\"api-nav\"><p class=\"api-pkg\"><a href=\"{}\"{cur}><span>{}</span>{ver}</a></p>",
        esc(&head.href),
        esc(head.name)
    );
    if !tree.is_empty() {
        let _ = write!(
            out,
            "<div class=\"api-sec\"><h2 class=\"t-label\">Modules</h2>{tree}</div>"
        );
    }
    out.push_str(items);
    if !deps.is_empty() {
        let _ = write!(
            out,
            "<div class=\"api-sec\"><h2 class=\"t-label\">{}</h2><ul class=\"api-items api-deps\">",
            esc(label)
        );
        for d in deps {
            let _ = if d.href.is_empty() {
                write!(out, "<li><span>{}</span></li>", named(d))
            } else {
                write!(
                    out,
                    "<li><a href=\"{}\">{}</a></li>",
                    esc(&d.href),
                    named(d)
                )
            };
        }
        out.push_str("</ul></div>");
    }
    out.push_str("</div>");
    out
}

/// The modules of `m`'s package as a tree, its top-level modules first
/// (the package itself is the sidebar's head): `m`'s ancestors and
/// itself open, every other branch folded behind a toggle that names
/// its number of submodules. Empty when the package has no module
/// below its root.
pub(crate) fn tree(mods: &[Module], m: &Module, href: &dyn Fn(&Module) -> String) -> String {
    let Some(r) = mods.iter().find(|q| q.path == m.root) else {
        return String::new();
    };
    let (kids, _) = node_kids(mods, r, m, href);
    if kids.is_empty() {
        return String::new();
    }
    format!("<ul class=\"api-tree\">{kids}</ul>")
}

/// The children of `q` in the tree and how many there are.
fn node_kids(
    mods: &[Module],
    q: &Module,
    m: &Module,
    href: &dyn Fn(&Module) -> String,
) -> (String, usize) {
    let mut out = String::new();
    let mut count = 0;
    for c in &q.children {
        let p = format!("{}::{c}", q.path);
        let Some(k) = mods.iter().find(|k| k.path == p) else {
            continue;
        };
        count += 1;
        let on = m.path == p || m.path.starts_with(&format!("{p}::"));
        let cur = if m.path == p {
            " aria-current=\"page\""
        } else {
            ""
        };
        let (sub, subs) = node_kids(mods, k, m, href);
        // The toggle names the count; the link's name stays the module's.
        let ct = if subs == 0 {
            String::new()
        } else {
            format!("<span class=\"ct\" aria-hidden=\"true\">{subs}</span>")
        };
        let _ = write!(
            out,
            "<li{}><a href=\"{}\"{cur}>{}{ct}</a>",
            if on { " class=\"open\"" } else { "" },
            esc(&href(k)),
            esc(c)
        );
        if subs > 0 {
            let _ = write!(
                out,
                "<details class=\"tw\"{}><summary><span class=\"sr-only\">{}: {subs} submodule{}</span></summary><ul>{sub}</ul></details>",
                if on { " open" } else { "" },
                esc(c),
                if subs == 1 { "" } else { "s" }
            );
        }
        out.push_str("</li>");
    }
    (out, count)
}

/// The items of `m` by kind and name, each linked to its anchor on the
/// page, in a section headed by the module's path below its package
/// (the package's name on its root). Empty when it has none.
pub(crate) fn items(m: &Module) -> String {
    let mut out = String::new();
    for (kind, label) in KINDS {
        let mut its: Vec<&Item> = if kind == "impl" {
            m.impls.iter().collect()
        } else {
            m.items.iter().filter(|it| it.kind == kind).collect()
        };
        if its.is_empty() {
            continue;
        }
        its.sort_by(|a, b| a.name.cmp(&b.name));
        let _ = write!(
            out,
            "<h3 class=\"api-k\">{label} <span class=\"ct\">{}</span></h3><ul class=\"api-items\">",
            its.len()
        );
        for it in its {
            let name = if kind == "impl" {
                format!("impl {}", it.name)
            } else {
                it.name.clone()
            };
            let _ = write!(
                out,
                "<li><a href=\"#{}\">{}</a></li>",
                esc(&anchor(it)),
                esc(&name)
            );
        }
        out.push_str("</ul>");
    }
    if out.is_empty() {
        return out;
    }
    let name = m
        .path
        .strip_prefix(&m.root)
        .and_then(|p| p.strip_prefix("::"))
        .unwrap_or(&m.path);
    format!(
        "<div class=\"api-sec\"><h2 class=\"t-label\">In <code>{}</code></h2>{out}</div>",
        esc(name)
    )
}

/// An item's methods in "On this page": (name, id).
type Methods = Vec<(String, String)>;

fn last_name(path: &str) -> &str {
    path.rsplit("::").next().unwrap_or(path)
}

/// "On this page" of `heads`: the sections, and under each the methods
/// of its items grouped by item; when there are several groups, one of
/// more than FOLD starts folded. Items without members are the
/// sidebar's. Empty for a short page: no methods and fewer than three
/// sections, which the sidebar's kind headings already show.
pub(crate) fn rail(heads: &[Head]) -> String {
    // A section (text, no methods, id) or a group (name, methods, "").
    let mut rows: Vec<(String, Methods, String)> = Vec::new();
    let (mut sections, mut groups) = (0, 0);
    let mut group = String::new();
    let mut ms: Methods = Vec::new();
    for h in heads {
        if h.level > 3 {
            ms.push((last_name(&h.text).to_owned(), h.id.clone()));
            continue;
        }
        if !ms.is_empty() {
            rows.push((group.clone(), std::mem::take(&mut ms), String::new()));
            groups += 1;
        }
        if h.level == 2 {
            sections += 1;
            rows.push((h.text.clone(), Vec::new(), h.id.clone()));
        } else if h.text.starts_with("impl ") {
            group.clone_from(&h.text);
        } else {
            last_name(&h.text).clone_into(&mut group);
        }
    }
    if !ms.is_empty() {
        rows.push((group, ms, String::new()));
        groups += 1;
    }
    if sections < 3 && groups == 0 {
        return String::new();
    }
    let mut out = String::new();
    for (text, mut gm, id) in rows {
        if gm.is_empty() {
            let _ = write!(out, "<li><a href=\"#{}\">{}</a></li>", esc(&id), esc(&text));
            continue;
        }
        gm.sort();
        let open = if groups > 1 && gm.len() > FOLD {
            ""
        } else {
            " open"
        };
        let _ = write!(
            out,
            "<li class=\"grp\"><details{open}><summary><code>{}</code><span class=\"ct\">{}</span></summary><ul>",
            esc(&text),
            gm.len()
        );
        for (name, id) in gm {
            let _ = write!(
                out,
                "<li class=\"m\"><a href=\"#{}\"><code>{}</code></a></li>",
                esc(&id),
                esc(&name)
            );
        }
        out.push_str("</ul></details></li>");
    }
    out
}
