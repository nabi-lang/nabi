//! The HTML of the pages, in the markup and classes of the Nabi sites'
//! API pages (nabi.css and the docs' style.css): the header with the
//! package name and version, the navigation (nav.rs), the module page.

use crate::doc::esc;
use crate::doc::highlight;
use crate::doc::kit;
use crate::doc::model::{Item, Mods, Module, anchor, candidates};
use crate::doc::nav;
use crate::doc::nav::Head;
use nabic_core::HashMap;
use std::fmt::Write as _;

/// A documented package: its root module path, the version shown and
/// the roots of the documented packages it depends on.
pub(crate) struct Package {
    pub(crate) root: String,
    pub(crate) version: String,
    pub(crate) deps: Vec<String>,
}

/// The pages and how names in them resolve.
pub(crate) struct Site<'a> {
    pub(crate) pages: &'a [Module],
    pub(crate) all: Mods<'a>,
    pub(crate) packages: &'a [Package],
    /// (module path, item name) -> its URL from the doc root, for every
    /// item a page shows: where it is declared and where it is shown.
    homes: HashMap<(String, String), String>,
    /// A type name -> its URL, when one type has the name.
    unique: HashMap<String, Option<String>>,
}

const KINDS: [(&str, &str); 8] = [
    ("use", "Re-exports"),
    ("struct", "Structs"),
    ("enum", "Enums"),
    ("trait", "Traits"),
    ("type", "Type aliases"),
    ("fn", "Functions"),
    ("const", "Constants"),
    ("static", "Statics"),
];

/// The path from page `from` to `to`, both under the doc root.
pub(crate) fn rel(from: &str, to: &str) -> String {
    let mut a: Vec<&str> = from.split('/').collect();
    a.pop();
    let b: Vec<&str> = to.split('/').collect();
    let same = a
        .iter()
        .zip(&b[..b.len() - 1])
        .take_while(|(x, y)| x == y)
        .count();
    "../".repeat(a.len() - same) + &b[same..].join("/")
}

/// The first sentence of `doc`, as the sites cut it.
pub(crate) fn summary(doc: &str) -> String {
    let t = doc.trim().replace('\n', " ");
    match t.find(". ") {
        Some(k) => t[..=k].to_owned(),
        None => t,
    }
}

impl<'a> Site<'a> {
    pub(crate) fn new(pages: &'a [Module], all: &'a [Module], packages: &'a [Package]) -> Self {
        let mut homes = HashMap::default();
        let mut unique: HashMap<String, Option<String>> = HashMap::default();
        for v in pages {
            for it in v.items.iter().filter(|it| it.kind != "use") {
                let url = format!("{}#{}", v.out, anchor(it));
                homes
                    .entry((v.path.clone(), it.name.clone()))
                    .or_insert_with(|| url.clone());
                if let Some(o) = &it.origin {
                    homes.entry(o.clone()).or_insert_with(|| url.clone());
                }
                if matches!(it.kind, "struct" | "enum" | "trait" | "type") {
                    let home = it
                        .origin
                        .clone()
                        .unwrap_or_else(|| (v.path.clone(), it.name.clone()));
                    let first = homes.get(&home).cloned().unwrap_or_else(|| url.clone());
                    let e = unique
                        .entry(it.name.clone())
                        .or_insert_with(|| Some(first.clone()));
                    if e.as_ref().is_some_and(|u| *u != first) {
                        *e = None;
                    }
                }
            }
        }
        Self {
            pages,
            all: Mods(all),
            packages,
            homes,
            unique,
        }
    }

    /// The URL of what the absolute `path` names, if a page shows it.
    fn url_of(&self, path: &str) -> Option<String> {
        if let Some(k) = path.rfind("::") {
            let key = (path[..k].to_owned(), path[k + 2..].to_owned());
            if let Some(u) = self.homes.get(&key) {
                return Some(u.clone());
            }
        }
        match self.all.lookup(path, 8)? {
            (q, Some(it)) => self.homes.get(&(q.path.clone(), it.name.clone())).cloned(),
            (q, None) => self
                .pages
                .iter()
                .find(|p| p.path == q.path)
                .map(|p| p.out.clone()),
        }
    }

    /// The URL of `name` as written in module `scope`.
    fn resolve(&self, scope: &Module, name: &str) -> Option<String> {
        if let Some(u) = self.homes.get(&(scope.path.clone(), name.to_owned())) {
            return Some(u.clone());
        }
        for (p, n) in &scope.imports {
            if n == name {
                for c in candidates(scope, p) {
                    if let Some(u) = self.url_of(&c) {
                        return Some(u);
                    }
                }
            }
        }
        self.unique.get(name).cloned().flatten()
    }

    /// The module whose names `it` (on page `v`) is written with.
    fn scope_of(&self, v: &'a Module, it: &Item) -> &'a Module {
        it.origin
            .as_ref()
            .and_then(|(q, _)| self.all.at(q))
            .unwrap_or(v)
    }

    /// Highlighted `html` with each type that resolves linked, from page
    /// `from`; not `here` (`page#id`) itself.
    fn link_types(&self, html: &str, scope: &Module, from: &str, here: &str) -> String {
        let open = "<span class=\"t\">";
        let mut out = String::new();
        let mut rest = html;
        while let Some(k) = rest.find(open) {
            out.push_str(&rest[..k]);
            rest = &rest[k + open.len()..];
            let e = rest.find("</span>").unwrap_or(rest.len());
            let name = &rest[..e];
            match self.resolve(scope, name).filter(|u| u != here) {
                Some(to) => {
                    let _ = write!(
                        out,
                        "<a class=\"t\" href=\"{}\">{name}</a>",
                        href(from, &to)
                    );
                    rest = &rest[(e + "</span>".len()).min(rest.len())..];
                }
                None => out.push_str(open),
            }
        }
        out.push_str(rest);
        out
    }

    /// Doc text as HTML on page `from`, names in inline code resolved
    /// in `scope`.
    fn md(&self, text: &str, scope: &Module, from: &str) -> String {
        if text.trim().is_empty() {
            return String::new();
        }
        let link = |code: &str| {
            let name = code.trim_end_matches("()");
            if !name
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == ':')
            {
                return None;
            }
            let u = if name.contains("::") {
                candidates(scope, name).iter().find_map(|c| self.url_of(c))
            } else {
                self.resolve(scope, name)
            };
            u.map(|u| href(from, &u))
        };
        markdown(text, &link)
    }

    fn sig_html(&self, sig: &str, name: &str, scope: &Module, from: &str, here: &str) -> String {
        self.link_types(&sig_named(sig, name), scope, from, here)
    }

    /// An item of page `v`.
    fn item_html(&self, v: &Module, it: &Item, heads: &mut Vec<Head>) -> String {
        let id = anchor(it);
        let here = format!("{}#{id}", v.out);
        let scope = self.scope_of(v, it);
        let title = if it.kind == "impl" {
            format!("impl {}", it.name)
        } else {
            it.name.clone()
        };
        let mut out = format!(
            "<section class=\"api-item\" id=\"{}\"><h3><a href=\"#{0}\">{}</a></h3>",
            esc(&id),
            esc(&title)
        );
        if !it.sig.is_empty() {
            let _ = write!(
                out,
                "<pre class=\"sig\"><code>{}</code></pre>",
                self.sig_html(&it.sig, "", scope, &v.out, &here)
            );
        }
        out.push_str(&self.md(&it.doc, scope, &v.out));
        if !it.members.is_empty() {
            out.push_str("<ul class=\"members\">");
            for mb in &it.members {
                let mid = format!("{}.{}", it.name, mb.name);
                let is_fn = mb.sig.contains("fn ");
                let code = self.sig_html(
                    &mb.sig,
                    if is_fn { &mb.name } else { "" },
                    scope,
                    &v.out,
                    &here,
                );
                let _ = write!(
                    out,
                    "<li><div class=\"m-h\"><h4 id=\"{}\"><code>{code}</code><a class=\"self\" href=\"#{0}\" aria-label=\"Link to {}\">\u{a7}</a></h4></div>{}</li>",
                    esc(&mid),
                    esc(&mb.name),
                    self.md(&mb.doc, scope, &v.out)
                );
                if is_fn {
                    heads.push(Head {
                        level: 4,
                        id: mid,
                        text: format!("{}::{}::{}", v.path, it.name, mb.name),
                    });
                }
            }
            out.push_str("</ul>");
        }
        if !it.traits.is_empty() {
            let _ = write!(
                out,
                "<details class=\"impls grow\"><summary>Trait implementations {}</summary><ul>",
                it.traits.len()
            );
            for t in &it.traits {
                let _ = write!(
                    out,
                    "<li><code>{}</code></li>",
                    self.link_types(&highlight::nabi(t), scope, &v.out, &here)
                );
            }
            out.push_str("</ul></details>");
        }
        out.push_str("</section>");
        out
    }

    /// A `pub use` of page `v`: its line, and each name it brings in,
    /// anchored here by the defining item's id and linked to its page.
    fn use_html(&self, v: &Module, it: &Item) -> String {
        let id = anchor(it);
        let own: Vec<String> = v
            .items
            .iter()
            .filter(|x| x.kind != "use")
            .map(anchor)
            .collect();
        let mut out = format!(
            "<section class=\"api-item\" id=\"{}\"><h3><a href=\"#{0}\">{}</a></h3><pre class=\"sig\"><code>{}</code></pre>{}<ul class=\"api-list reexports\">",
            esc(&id),
            esc(&it.name),
            self.link_types(&highlight::nabi(&it.sig), v, &v.out, ""),
            self.md(&it.doc, v, &v.out)
        );
        let mut ids = own;
        for (p, n) in it.paths.iter().filter(|(_, n)| !it.skip.contains(n)) {
            let hit = candidates(v, p.trim_end_matches("::*"))
                .into_iter()
                .find_map(|c| self.all.lookup(&c, 8).map(|r| (c, r)));
            let label = if n == "*" { p.as_str() } else { n.as_str() };
            let (to, rid, what) = match hit {
                Some((c, (q, Some(x)))) => (
                    self.url_of(&c)
                        .or_else(|| self.homes.get(&(q.path.clone(), x.name.clone())).cloned()),
                    format!("{}.{n}", x.kind),
                    format!(
                        "{}{}",
                        if x.name == *n {
                            String::new()
                        } else {
                            format!("<code>{}</code>: ", esc(&x.name))
                        },
                        inline(&self.md(&summary(&x.doc), q, &v.out))
                    ),
                ),
                Some((_, (q, None))) => (
                    self.pages
                        .iter()
                        .find(|pg| pg.path == q.path)
                        .map(|pg| pg.out.clone()),
                    if n == "*" {
                        String::new()
                    } else {
                        format!("mod.{n}")
                    },
                    inline(&self.md(&summary(&q.doc), q, &v.out)),
                ),
                None => (None, String::new(), String::new()),
            };
            let attr = if rid.is_empty() || ids.contains(&rid) || to.is_none() {
                String::new()
            } else {
                format!(" id=\"{}\"", esc(&rid))
            };
            let name = match &to {
                Some(u) => format!(
                    "<a href=\"{}\"><code>{}</code></a>",
                    href(&v.out, u),
                    esc(label)
                ),
                None => format!("<code>{}</code>", esc(label)),
            };
            let _ = write!(out, "<li{attr}>{name} <span>{what}</span></li>");
            ids.push(rid);
        }
        out.push_str("</ul></section>");
        out
    }

    /// Page `v`'s body and its headings.
    fn body(&self, v: &Module) -> (String, Vec<Head>) {
        let mut heads = Vec::new();
        // The path in full, to copy; the breadcrumb above links up.
        let mut out = format!(
            "<h1><code>{}</code></h1>",
            esc(&v.path).replace("::", "::<wbr>")
        );
        out.push_str(&self.md(intro(v), v, &v.out));
        let children: Vec<&Module> = v
            .children
            .iter()
            .filter_map(|c| {
                self.pages
                    .iter()
                    .find(|p| p.path == format!("{}::{c}", v.path))
            })
            .collect();
        if !children.is_empty() {
            out.push_str("<h2 id=\"modules\">Modules</h2><ul class=\"api-list\">");
            heads.push(Head {
                level: 2,
                id: "modules".to_owned(),
                text: "Modules".to_owned(),
            });
            for cm in children {
                let _ = write!(
                    out,
                    "<li><a href=\"{}\"><code>{}</code></a> <span>{}</span></li>",
                    rel(&v.out, &cm.out),
                    esc(cm.path.rsplit("::").next().unwrap_or_default()),
                    inline(&self.md(&summary(intro(cm)), cm, &v.out))
                );
            }
            out.push_str("</ul>");
        }
        for (kind, label) in KINDS {
            let its: Vec<&Item> = v.items.iter().filter(|it| it.kind == kind).collect();
            if its.is_empty() {
                continue;
            }
            let _ = write!(out, "<h2 id=\"{kind}\">{label}</h2>");
            heads.push(Head {
                level: 2,
                id: kind.to_owned(),
                text: label.to_owned(),
            });
            for it in its {
                if kind == "use" {
                    out.push_str(&self.use_html(v, it));
                } else {
                    heads.push(Head {
                        level: 3,
                        id: anchor(it),
                        text: format!("{}::{}", v.path, it.name),
                    });
                    out.push_str(&self.item_html(v, it, &mut heads));
                }
            }
        }
        if !v.impls.is_empty() {
            out.push_str("<h2 id=\"impl\">Implementations</h2>");
            heads.push(Head {
                level: 2,
                id: "impl".to_owned(),
                text: "Implementations".to_owned(),
            });
            for it in &v.impls {
                heads.push(Head {
                    level: 3,
                    id: anchor(it),
                    text: format!("impl {}", it.name),
                });
                out.push_str(&self.item_html(v, it, &mut heads));
            }
        }
        (out, heads)
    }

    /// The sidebar of page `v` (nav.rs): its package, the package's
    /// modules, the page's items, the package's dependencies.
    fn sidebar(&self, v: &Module) -> String {
        let href = |q: &Module| rel(&v.out, &q.out);
        let pkg = self.packages.iter().find(|p| p.root == v.root);
        let page = |root: &str| self.pages.iter().find(|m| m.path == root);
        let head = nav::Package {
            name: &v.root,
            version: pkg.map_or("", |p| p.version.as_str()),
            href: page(&v.root).map(href).unwrap_or_default(),
            here: v.path == v.root,
        };
        let deps: Vec<nav::Package<'_>> = pkg
            .map(|p| p.deps.as_slice())
            .unwrap_or_default()
            .iter()
            .filter_map(|d| self.packages.iter().find(|p| p.root == *d))
            .map(|d| nav::Package {
                name: &d.root,
                version: &d.version,
                href: page(&d.root).map(href).unwrap_or_default(),
                here: false,
            })
            .collect();
        nav::side(
            &head,
            &nav::tree(self.pages, v, &href),
            &nav::items(v),
            "Dependencies",
            &deps,
        )
    }

    /// The header's title: `root`'s name, linked to its root page
    /// (`home`), and its version.
    fn title(&self, root: &str, home: &str) -> String {
        let version = self
            .packages
            .iter()
            .find(|p| p.root == root)
            .map_or("", |p| p.version.as_str());
        let mut out = String::from("<div class=\"pkg-id\">");
        if home.is_empty() {
            let _ = write!(out, "<span class=\"pkg-n\">{}</span>", esc(root));
        } else {
            let _ = write!(out, "<a class=\"pkg-n\" href=\"{home}\">{}</a>", esc(root));
        }
        if !version.is_empty() {
            let _ = write!(out, "<span class=\"pkg-v\">{}</span>", esc(version));
        }
        out.push_str("</div>");
        out
    }

    /// Every page and the search index: (path under the doc root, text).
    pub(crate) fn render(&self) -> Vec<(String, String)> {
        let mut files = Vec::new();
        let mut index = String::from("var SEARCH_INDEX = [\n");
        for v in self.pages {
            let (body, heads) = self.body(v);
            let push = |index: &mut String, p: &str, k: &str, u: &str, d: &str| {
                let _ = writeln!(index, "{},", crate::doc::search_entry(p, k, u, d));
            };
            push(&mut index, &v.path, "mod", &v.out, &summary(intro(v)));
            for it in v.items.iter().filter(|it| it.kind != "use") {
                let u = format!("{}#{}", v.out, anchor(it));
                push(
                    &mut index,
                    &format!("{}::{}", v.path, it.name),
                    it.kind,
                    &u,
                    &summary(&it.doc),
                );
                for mb in it.members.iter().filter(|m| m.sig.contains("fn ")) {
                    let u = format!("{}#{}.{}", v.out, it.name, mb.name);
                    let p = format!("{}::{}::{}", v.path, it.name, mb.name);
                    push(&mut index, &p, "method", &u, &summary(&mb.doc));
                }
            }
            for it in &v.impls {
                for mb in &it.members {
                    let u = format!("{}#{}.{}", v.out, it.name, mb.name);
                    let p = format!("{}::{}::{}", v.path, it.name, mb.name);
                    push(&mut index, &p, "method", &u, &summary(&mb.doc));
                }
            }
            let crumb = nav::crumb(
                nav::kind_of(v),
                &nav::trail(self.pages, v, &|q: &Module| rel(&v.out, &q.out)),
            );
            let home = self
                .pages
                .iter()
                .find(|m| m.path == v.root)
                .map_or_else(String::new, |r| rel(&v.out, &r.out));
            let html = frame(&Frame {
                out: &v.out,
                title: &v.path,
                desc: &summary(intro(v)),
                side: &self.sidebar(v),
                toc: &nav::rail(&heads),
                crumb: &crumb,
                body: &body,
                title_bar: &self.title(&v.root, &home),
            });
            files.push((v.out.clone(), html));
        }
        index.push_str("];\n");
        files.push(("search-index.js".to_owned(), index));
        files
    }
}

/// `href` of `to` (`page#id` under the doc root) on page `from`.
fn href(from: &str, to: &str) -> String {
    match to.split_once('#') {
        Some((page, id)) if page == from => format!("#{id}"),
        _ => rel(from, to),
    }
}

/// A module's doc without the repeated module path in front.
fn intro(m: &Module) -> &str {
    for sep in [" - ", " -- ", ": "] {
        if let Some(r) = m.doc.strip_prefix(&format!("{}{sep}", m.path)) {
            return r;
        }
    }
    &m.doc
}

/// Inline HTML of one paragraph, without the paragraph.
fn inline(h: &str) -> String {
    let h = h.trim();
    h.strip_prefix("<p>")
        .and_then(|x| x.strip_suffix("</p>"))
        .unwrap_or(h)
        .to_owned()
}

/// A one-line fn signature as highlighted pieces that each stay on one
/// line: the head up to `(`, each parameter, `-> T`, `where ...`, the
/// declared `name` set apart. Others are highlighted whole.
fn sig_named(sig: &str, name: &str) -> String {
    if sig.contains('\n') || !sig.contains("fn ") {
        return highlight::nabi(sig);
    }
    let fn_at = sig.find("fn ").unwrap_or(0);
    let mut pieces: Vec<(String, &str)> = Vec::new();
    let mut cur = String::new();
    let mut sep = "";
    let mut depth = 0i32;
    let (mut params, mut done, mut skip) = (false, false, false);
    let mut prev = ' ';
    for (at, c) in sig.char_indices() {
        if skip {
            skip = false;
            if c == ' ' {
                continue;
            }
        }
        if depth == 0
            && done
            && c == ' '
            && (sig[at..].starts_with(" -> ") || sig[at..].starts_with(" where "))
        {
            pieces.push((std::mem::take(&mut cur), sep));
            sep = " ";
            prev = c;
            continue;
        }
        cur.push(c);
        if matches!(c, '(' | '[' | '{' | '<') {
            depth += 1;
        } else if matches!(c, ')' | ']' | '}') || (c == '>' && prev != '-') {
            depth -= 1;
        }
        if c == '(' && depth == 1 && !params && !done && at > fn_at {
            params = true;
            pieces.push((std::mem::take(&mut cur), sep));
            sep = "<wbr>";
        } else if c == ')' && depth == 0 && params {
            params = false;
            done = true;
        } else if c == ',' && depth == 1 && params {
            pieces.push((std::mem::take(&mut cur), sep));
            sep = " ";
            skip = true;
        }
        prev = c;
    }
    if !cur.is_empty() {
        pieces.push((cur, sep));
    }
    let mut out = String::new();
    for (k, (p, sep)) in pieces.iter().enumerate() {
        out.push_str(sep);
        let at = if k == 0 && !name.is_empty() {
            p.find(&format!("fn {name}"))
        } else {
            None
        };
        if let Some(i) = at {
            let n = i + 3;
            let _ = write!(
                out,
                "<span class=\"nw\"><span class=\"mod\">{}</span><span class=\"nm\">{}</span>{}</span>",
                esc(&p[..n]),
                esc(name),
                highlight::nabi(&p[n + name.len()..])
            );
        } else {
            let _ = write!(out, "<span class=\"nw\">{}</span>", highlight::nabi(p));
        }
    }
    out
}

struct Frame<'a> {
    out: &'a str,
    title: &'a str,
    desc: &'a str,
    side: &'a str,
    toc: &'a str,
    /// The breadcrumb (nav.rs).
    crumb: &'a str,
    body: &'a str,
    /// The header's package name and version.
    title_bar: &'a str,
}

const ICON_SEARCH: &str = "<svg width=\"14\" height=\"14\" viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"2\" aria-hidden=\"true\"><circle cx=\"11\" cy=\"11\" r=\"7\"/><path d=\"M20 20l-4-4\"/></svg>";
const ICON_SIDE: &str = "<svg width=\"16\" height=\"16\" viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"2\" aria-hidden=\"true\"><rect x=\"3\" y=\"4\" width=\"18\" height=\"16\" rx=\"2\"/><path d=\"M9 4v16\"/></svg>";
const ICON_CLOSE: &str = "<svg width=\"16\" height=\"16\" viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"2\" aria-hidden=\"true\"><path d=\"M6 6l12 12M18 6L6 18\"/></svg>";
const ICON_THEME: &str = "<svg class=\"moon\" width=\"16\" height=\"16\" viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"2\" aria-hidden=\"true\"><path d=\"M21 12.8A9 9 0 1 1 11.2 3a7 7 0 0 0 9.8 9.8z\"/></svg><svg class=\"sun\" width=\"16\" height=\"16\" viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"2\" aria-hidden=\"true\"><circle cx=\"12\" cy=\"12\" r=\"4\"/><path d=\"M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4\"/></svg>";

/// A whole page: the docs site's frame without its site-only chrome
/// (section nav, language switch, version list, site footer).
fn frame(f: &Frame<'_>) -> String {
    let root = "../".repeat(f.out.matches('/').count());
    let full = format!("{} - Nabi docs", f.title);
    let (on_page, body) = if f.toc.is_empty() {
        (String::new(), f.body.to_owned())
    } else {
        let aside = format!(
            "<aside class=\"toc\" aria-label=\"On this page\"><p class=\"t-label\">On this page</p><ul>{}</ul></aside>\n",
            f.toc
        );
        let body = match f.body.find("</h1>") {
            Some(k) => format!(
                "{}\n<details class=\"toc-m grow\"><summary>On this page</summary><ul>{}</ul></details>{}",
                &f.body[..k + 5],
                f.toc,
                &f.body[k + 5..]
            ),
            None => f.body.to_owned(),
        };
        (aside, body)
    };
    let desc = if f.desc.is_empty() {
        String::new()
    } else {
        format!("<meta name=\"description\" content=\"{}\">\n", esc(f.desc))
    };
    format!(
        "<!doctype html>
<html class=\"nb lt\" lang=\"en\">
<head>
<meta charset=\"utf-8\">
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">
<title>{title}</title>
<meta name=\"color-scheme\" content=\"light dark\">
<meta name=\"theme-color\" content=\"#fcfbfb\">
<script src=\"{root}theme.js\"></script>
{desc}<link rel=\"preload\" href=\"{root}fonts/Geist-Variable.woff2\" as=\"font\" type=\"font/woff2\" crossorigin>
<link rel=\"preload\" href=\"{root}fonts/GeistMono-Variable.woff2\" as=\"font\" type=\"font/woff2\" crossorigin>
<link rel=\"stylesheet\" href=\"{root}nabi.css\">
<link rel=\"stylesheet\" href=\"{root}style.css\">
<link rel=\"stylesheet\" href=\"{root}doc.css\">
<link rel=\"icon\" href=\"{root}favicon.svg\" type=\"image/svg+xml\">
</head>
<body class=\"doc\" data-root=\"{root}\">
<a class=\"skip\" href=\"#content\">Skip to content</a>
<header class=\"hdr top\" lang=\"en\">
<div class=\"in\">
<div class=\"pkg-t\"><span class=\"brand\" lang=\"en\"><span class=\"mk\">{mark}</span><span class=\"sl\" aria-hidden=\"true\">/</span></span>{title_bar}</div>
<div class=\"tools\">
<div class=\"search\" role=\"search\"><button type=\"button\" class=\"s-open\" aria-label=\"Search docs\" tabindex=\"-1\" aria-hidden=\"true\">{ICON_SEARCH}</button><input id=\"q\" type=\"search\" placeholder=\"Search docs\" aria-label=\"Search docs\" aria-keyshortcuts=\"/ Control+K Meta+K\" autocomplete=\"off\" role=\"combobox\" aria-autocomplete=\"list\" aria-expanded=\"false\" aria-controls=\"results\"><kbd class=\"kbd\" aria-hidden=\"true\">/</kbd><ul id=\"results\" class=\"results appear\" role=\"listbox\" aria-label=\"Search results\" hidden></ul><p class=\"sr-only\" role=\"status\"></p></div>
<button class=\"ib theme\" type=\"button\" aria-label=\"Dark theme\" aria-pressed=\"false\" title=\"Switch to dark theme\">{ICON_THEME}</button>
</div>
</div>
<div class=\"cbar\"><button class=\"side-toggle\" type=\"button\" aria-controls=\"side\" aria-expanded=\"false\">{ICON_SIDE}Contents</button></div>
</header>
<div class=\"shell\">
<nav id=\"side\" class=\"side\" aria-label=\"Contents\"><div class=\"side-h\"><p class=\"t-label\">Contents</p><button class=\"ib side-close\" type=\"button\" aria-label=\"Close contents\">{ICON_CLOSE}</button></div>{side}</nav>
<main id=\"content\" class=\"main\">
<article class=\"prose\">
<div class=\"pagehead\">{crumb}</div>
{body}
</article>
{on_page}</main>
</div>
<footer class=\"foot\" lang=\"en\">
<div class=\"frame bar\">
<span class=\"mono\">nabi doc {version}</span>
<span class=\"seg\" role=\"group\" aria-label=\"Theme\" lang=\"en\"><button type=\"button\" data-theme-set=\"auto\" aria-pressed=\"false\">System</button><button type=\"button\" data-theme-set=\"light\" aria-pressed=\"false\">Light</button><button type=\"button\" data-theme-set=\"dark\" aria-pressed=\"false\">Dark</button></span>
</div>
</footer>
<script src=\"{root}search-index.js\"></script>
<script src=\"{root}keys.js\" defer></script>
<script src=\"{root}search.js\" defer></script>
</body>
</html>
",
        title = esc(&full),
        mark = kit::wordmark(),
        side = f.side,
        crumb = f.crumb,
        title_bar = f.title_bar,
        version = env!("CARGO_PKG_VERSION"),
    )
}

/// A doc test's setup line (`# let x = 1;`, or a lone `#`), which runs
/// but is not shown.
fn hidden(line: &str) -> bool {
    let t = line.trim_start();
    t == "#" || t.starts_with("# ")
}

/// The markdown subset (see the module doc) as HTML; `link` gives the
/// href of a name in inline code, when it documents one.
pub(crate) fn markdown(src: &str, link: &dyn Fn(&str) -> Option<String>) -> String {
    let mut out = String::new();
    let mut para: Vec<&str> = Vec::new();
    let mut list: Option<&str> = None;
    let mut code: Option<(String, String)> = None;
    let flush = |out: &mut String, para: &mut Vec<&str>, list: &mut Option<&str>| {
        if !para.is_empty() {
            let _ = writeln!(out, "<p>{}</p>", inline_md(&para.join("\n"), link));
            para.clear();
        }
        if let Some(tag) = list.take() {
            let _ = writeln!(out, "</{tag}>");
        }
    };
    for line in src.lines() {
        let t = line.trim();
        if let Some((lang, buf)) = &mut code {
            if t.starts_with("```") {
                let body = if lang == "nabi" {
                    highlight::nabi(buf.trim_end_matches('\n'))
                } else {
                    esc(buf.trim_end_matches('\n'))
                };
                let _ = writeln!(
                    out,
                    "<div class=\"code cb\"><div class=\"cb-h\"><span class=\"t-label\">{}</span></div><div class=\"cb-body\"><pre><code class=\"language-{0}\">{body}</code></pre></div></div>",
                    esc(lang)
                );
                code = None;
            } else if lang != "nabi" || !hidden(line) {
                buf.push_str(line);
                buf.push('\n');
            }
            continue;
        }
        if let Some(lang) = t.strip_prefix("```") {
            flush(&mut out, &mut para, &mut list);
            // `nabi,ignore`-style info strings name the language first.
            let lang = lang.split([',', ' ']).next().unwrap_or_default();
            let lang = if lang.is_empty() { "nabi" } else { lang };
            code = Some((lang.to_owned(), String::new()));
            continue;
        }
        if t.is_empty() {
            flush(&mut out, &mut para, &mut list);
            continue;
        }
        let hashes = t.chars().take_while(|c| *c == '#').count();
        if (1..=4).contains(&hashes) && t[hashes..].starts_with(' ') {
            flush(&mut out, &mut para, &mut list);
            let level = hashes + 2;
            let _ = writeln!(
                out,
                "<h{level}>{}</h{level}>",
                inline_md(t[hashes..].trim(), link)
            );
            continue;
        }
        let ordered = t
            .split_once(". ")
            .filter(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
            .map(|(_, rest)| rest);
        let bullet = t.strip_prefix("- ").or_else(|| t.strip_prefix("* "));
        if let Some((tag, rest)) = bullet
            .map(|r| ("ul", r))
            .or_else(|| ordered.map(|r| ("ol", r)))
        {
            if !para.is_empty() || list.is_some_and(|l| l != tag) {
                flush(&mut out, &mut para, &mut list);
            }
            if list.is_none() {
                let _ = writeln!(out, "<{tag}>");
                list = Some(tag);
            }
            let _ = writeln!(out, "<li>{}</li>", inline_md(rest, link));
            continue;
        }
        if list.is_some() && out.ends_with("</li>\n") {
            // A continuation line of the last item.
            out.truncate(out.len() - "</li>\n".len());
            let _ = writeln!(out, " {}</li>", inline_md(t, link));
            continue;
        }
        para.push(t);
    }
    if let Some((lang, buf)) = code {
        let _ = writeln!(
            out,
            "<pre><code class=\"language-{}\">{}</code></pre>",
            esc(&lang),
            esc(&buf)
        );
    }
    flush(&mut out, &mut para, &mut list);
    out
}

/// Inline markdown: `code`, `[text](url)`, `**bold**`.
fn inline_md(s: &str, link: &dyn Fn(&str) -> Option<String>) -> String {
    let mut out = String::new();
    let mut rest = s;
    let mut bold = false;
    while let Some(i) = rest.find(['`', '[', '*']) {
        out.push_str(&esc(&rest[..i]));
        let r = &rest[i..];
        if let Some(body) = r.strip_prefix('`') {
            if let Some(end) = body.find('`') {
                let code = &body[..end];
                match link(code) {
                    Some(u) => {
                        let _ = write!(
                            out,
                            "<a href=\"{}\"><code>{}</code></a>",
                            esc(&u),
                            esc(code)
                        );
                    }
                    None => {
                        let _ = write!(out, "<code>{}</code>", esc(code));
                    }
                }
                rest = &body[end + 1..];
                continue;
            }
        } else if let Some(body) = r.strip_prefix('[') {
            let found = body.find("](").and_then(|m| {
                let close = body[m + 2..].find(')')?;
                Some((&body[..m], &body[m + 2..m + 2 + close], m + 3 + close))
            });
            if let Some((text, url, used)) = found {
                let _ = write!(
                    out,
                    "<a href=\"{}\">{}</a>",
                    esc(url),
                    inline_md(text, link)
                );
                rest = &body[used..];
                continue;
            }
        } else if let Some(body) = r.strip_prefix("**") {
            out.push_str(if bold { "</strong>" } else { "<strong>" });
            bold = !bold;
            rest = body;
            continue;
        }
        out.push_str(&esc(&r[..1]));
        rest = &r[1..];
    }
    out.push_str(&esc(rest));
    if bold {
        out.push_str("</strong>");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_subset() {
        let link = |c: &str| (c == "Point").then(|| "app/index.html#struct.Point".to_owned());
        let html = markdown(
            "# Title\nSome `x` and [a link](https://e.x) **bold**.\n\n- one\n- two\n\n1. first\n\n\
             ```\nlet a = 1 < 2;\n```\nSee `Point`.",
            &link,
        );
        assert!(html.contains("<h3>Title</h3>"), "{html}");
        assert!(html.contains("<code>x</code>"));
        assert!(html.contains("<a href=\"https://e.x\">a link</a>"));
        assert!(html.contains("<strong>bold</strong>"));
        assert!(
            html.contains("<ul>\n<li>one</li>\n<li>two</li>\n</ul>"),
            "{html}"
        );
        assert!(html.contains("<ol>\n<li>first</li>\n</ol>"), "{html}");
        assert!(
            html.contains("<code class=\"language-nabi\"><span class=\"k\">let</span> a = <span class=\"n\">1</span> &lt; <span class=\"n\">2</span>;</code>"),
            "{html}"
        );
        assert!(html.contains("<a href=\"app/index.html#struct.Point\"><code>Point</code></a>"));
    }

    /// A doc test's `# ` setup lines are not shown; other code keeps them.
    #[test]
    fn hidden_lines() {
        let none = |_: &str| None;
        let html = markdown("```nabi\n# let a = 1;\n#\nlet b = a;\n```", &none);
        assert!(!html.contains("let</span> a"), "{html}");
        assert!(html.contains("let</span> b"), "{html}");
        let html = markdown("```text\n# heading\n```", &none);
        assert!(html.contains("# heading"), "{html}");
    }

    /// Fn signatures break only between their pieces, as on the sites.
    #[test]
    fn signature_pieces() {
        let h = sig_named("pub fn retain(self, keep: fn(T) -> bool) -> List<T>", "");
        assert_eq!(h.split("class=\"nw\"").count(), 5, "{h}");
        assert!(h.contains("<wbr><span") && !h.contains("( "), "{h}");
        let n = sig_named("pub const fn new(inner: R) -> BufReader<R>", "new");
        assert!(
            n.starts_with("<span class=\"nw\"><span class=\"mod\">pub const fn </span><span class=\"nm\">new</span>(</span><wbr>"),
            "{n}"
        );
        assert_eq!(
            sig_named("pub struct List<T>", "")
                .split("class=\"nw\"")
                .count(),
            1
        );
    }
}
