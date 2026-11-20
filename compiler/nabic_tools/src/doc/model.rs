//! What the pages show, by the rules the Nabi sites' API pages follow
//! (the web kit's apidoc `pages()`): one page per public module; an item
//! a `pub use` brings in from a private module (or through private
//! modules only) is shown inline under the re-exported name, its doc
//! after the `pub use`'s; each item of a `pub use private::*` likewise;
//! a re-export of what a public module shows stays a `use` entry that
//! links there; the methods of an `impl` written in a private module
//! join the type in its nearest public parent, else that module's
//! implementations.

use crate::doc::sig::{self, End, Texts};
use crate::doc::{doc_text, item_info};
use nabic_ast::{
    EnumVariantKind, ImplItem, Item as Ast, StructKind, TraitItem, TyKind, doc_hidden,
};
use nabic_core::{HashMap, Visibility};
use nabic_interface::SourceFile;

#[derive(Clone)]
pub(crate) struct Member {
    pub(crate) name: String,
    pub(crate) sig: String,
    pub(crate) doc: String,
}

#[derive(Clone)]
pub(crate) struct Item {
    /// `fn`, `struct` (unions too), `enum`, `trait`, `type`, `const`,
    /// `static`, `use`; `impl` for the inherent impl of a type declared
    /// elsewhere.
    pub(crate) kind: &'static str,
    /// Of a `use`: its tree (`a::{B, C}`).
    pub(crate) name: String,
    pub(crate) sig: String,
    pub(crate) doc: String,
    /// Fields, variants, trait items, inherent methods.
    pub(crate) members: Vec<Member>,
    /// `impl Trait for Self` headers.
    pub(crate) traits: Vec<String>,
    /// Of a `use`: each path it names and the name it binds.
    pub(crate) paths: Vec<(String, String)>,
    /// Of a `use`: the names it brings in that the page shows inline.
    pub(crate) skip: Vec<String>,
    /// Shown inline: the module that declares it and its name there.
    pub(crate) origin: Option<(String, String)>,
}

#[derive(Clone)]
pub(crate) struct Module {
    /// `linalg::factor`.
    pub(crate) path: String,
    /// The package's root module path (`linalg`, `core`).
    pub(crate) root: String,
    /// The page under the doc root: `linalg/index.html` for a package
    /// root, `linalg/factor.html`, `linalg/factor/lu.html` below it.
    pub(crate) out: String,
    pub(crate) doc: String,
    pub(crate) items: Vec<Item>,
    pub(crate) impls: Vec<Item>,
    /// Public child modules, by name.
    pub(crate) children: Vec<String>,
    /// Every module on its path is `pub` (and not `#[doc(hidden)]`).
    pub(crate) public: bool,
    /// Every `use` of the module, public or not: (path, name). Names in
    /// its signatures resolve through them.
    pub(crate) imports: Vec<(String, String)>,
}

impl Item {
    fn new(kind: &'static str, name: &str, sig: String, doc: String) -> Self {
        Self {
            kind,
            name: name.to_owned(),
            sig,
            doc,
            members: Vec::new(),
            traits: Vec::new(),
            paths: Vec::new(),
            skip: Vec::new(),
            origin: None,
        }
    }
}

/// The page of module `rel` (path segments below the package root) of
/// the package in directory `dir`: `index.html` for the root, else the
/// path as directories and `.html`.
pub(crate) fn page_of(dir: &str, rel: &[String]) -> String {
    match rel {
        [] => format!("{dir}/index.html"),
        // Not the root's own page.
        [one] if one == "index" => format!("{dir}/index/index.html"),
        _ => format!("{dir}/{}.html", rel.join("/")),
    }
}

fn join(a: &str, b: &str) -> String {
    match (a.is_empty(), b.is_empty()) {
        (true, _) => b.to_owned(),
        (_, true) => a.to_owned(),
        _ => format!("{a}::{b}"),
    }
}

fn up(p: &str) -> &str {
    p.rfind("::").map_or("", |k| &p[..k])
}

fn last(p: &str) -> &str {
    p.rsplit("::").next().unwrap_or(p)
}

/// A source file to document: its package root module path, the package
/// directory, the module path below the root, and the file.
pub(crate) struct File<'a> {
    pub(crate) root: String,
    pub(crate) dir: String,
    pub(crate) rel: Vec<String>,
    pub(crate) file: &'a SourceFile,
}

/// Every module of `files` (inline ones too), DFS from each package
/// root, children by name, public ones first.
pub(crate) fn collect(files: &[File<'_>], texts: &Texts<'_>) -> Vec<Module> {
    let mut found: HashMap<String, Module> = HashMap::default();
    // `mod` declarations: visibility and outer docs, by module path.
    let mut decls: HashMap<String, (bool, String)> = HashMap::default();
    let mut roots: Vec<(String, String)> = Vec::new();
    for f in files {
        if !roots.iter().any(|(r, _)| *r == f.root) {
            roots.push((f.root.clone(), f.dir.clone()));
        }
        let path = f.rel.iter().fold(f.root.clone(), |a, b| join(&a, b));
        scan(
            &mut found,
            &mut decls,
            texts,
            (&f.root, &f.dir),
            &f.rel,
            &path,
            doc_text(&f.file.attrs),
            &f.file.items,
        );
    }
    let mut out = Vec::new();
    for (root, _) in roots {
        walk(&mut found, &decls, &root, true, &mut out);
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn scan(
    found: &mut HashMap<String, Module>,
    decls: &mut HashMap<String, (bool, String)>,
    texts: &Texts<'_>,
    (root, dir): (&str, &str),
    rel: &[String],
    path: &str,
    inner_doc: String,
    items: &[Ast],
) {
    let m = found.entry(path.to_owned()).or_insert_with(|| Module {
        path: path.to_owned(),
        root: root.to_owned(),
        out: page_of(dir, rel),
        doc: String::new(),
        items: Vec::new(),
        impls: Vec::new(),
        children: Vec::new(),
        public: false,
        imports: Vec::new(),
    });
    if !inner_doc.is_empty() {
        m.doc = if m.doc.is_empty() {
            inner_doc
        } else {
            format!("{}\n\n{inner_doc}", m.doc)
        };
    }
    let mut inline = Vec::new();
    for item in items.iter().filter(|i| !i.is_local()) {
        match item {
            Ast::Mod(md) => {
                let child = join(path, &md.name);
                let public = md.vis == Visibility::Pub && !doc_hidden(&md.attrs);
                let e = decls
                    .entry(child.clone())
                    .or_insert((public, String::new()));
                e.0 = public;
                let outer = doc_text(&md.attrs);
                if !outer.is_empty() {
                    e.1 = outer;
                }
                let m = found.get_mut(path).unwrap();
                if !m.children.contains(&md.name) {
                    m.children.push(md.name.clone());
                }
                if let Some(inner) = md.items.as_ref().filter(|i| !i.is_empty()) {
                    inline.push((md.name.clone(), inner));
                }
            }
            Ast::Use(u) => {
                let mut paths = Vec::new();
                let tree = sig::use_tree(&u.tree, &mut paths);
                let m = found.get_mut(path).unwrap();
                m.imports.extend(paths.iter().cloned());
                if u.vis == Visibility::Pub && !doc_hidden(&u.attrs) {
                    let mut it =
                        Item::new("use", &tree, format!("pub use {tree}"), doc_text(&u.attrs));
                    it.paths = paths;
                    m.items.push(it);
                }
            }
            _ => {
                if let Some(it) = entry(item, texts) {
                    found.get_mut(path).unwrap().items.push(it);
                }
            }
        }
    }
    let m = found.get_mut(path).unwrap();
    impls(m, items, texts);
    for (name, inner) in inline {
        let mut child = rel.to_vec();
        child.push(name);
        let cpath = join(path, child.last().unwrap());
        scan(
            found,
            decls,
            texts,
            (root, dir),
            &child,
            &cpath,
            String::new(),
            inner,
        );
    }
}

fn walk(
    found: &mut HashMap<String, Module>,
    decls: &HashMap<String, (bool, String)>,
    path: &str,
    public: bool,
    out: &mut Vec<Module>,
) {
    let Some(mut m) = found.remove(path) else {
        return;
    };
    m.public = public;
    let mut kids: Vec<(bool, String)> = m
        .children
        .iter()
        .map(|c| (decls.get(&join(path, c)).is_some_and(|d| d.0), c.clone()))
        .collect();
    kids.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    m.children = kids.iter().filter(|k| k.0).map(|k| k.1.clone()).collect();
    if let Some((_, outer)) = decls.get(path).filter(|d| !d.1.is_empty()) {
        m.doc = if m.doc.is_empty() {
            outer.clone()
        } else {
            format!("{outer}\n\n{}", m.doc)
        };
    }
    out.push(m);
    for (p, c) in kids {
        walk(found, decls, &join(path, &c), public && p, out);
    }
}

fn hidden(attrs: &[nabic_ast::Attribute]) -> bool {
    doc_hidden(attrs)
}

/// The documented form of a `pub` item; none for one that is not.
fn entry(item: &Ast, texts: &Texts<'_>) -> Option<Item> {
    let (kind, name, vis, attrs) = item_info(item)?;
    if vis != Visibility::Pub || hidden(attrs) || kind == "mod" {
        return None;
    }
    let doc = doc_text(attrs);
    let span = item.span();
    let head = |kw: &str| texts.header(span, &[kw, name], End::Body);
    let it = match item {
        Ast::Fn(f) => Item::new(
            "fn",
            name,
            head("fn").unwrap_or_else(|| sig::fn_sig(f, true)),
            doc,
        ),
        Ast::Struct(s) => {
            let g = sig::generics(&s.generics);
            let fallback = match &s.kind {
                StructKind::Tuple(ts) => {
                    let tys: Vec<String> = ts.iter().map(|t| sig::ty(&t.ty)).collect();
                    format!("pub struct {name}{g}({})", tys.join(", "))
                }
                _ => format!("pub struct {name}{g}"),
            };
            let mut it = Item::new("struct", name, head("struct").unwrap_or(fallback), doc);
            if let StructKind::Named(fs) = &s.kind {
                it.members = fields(fs, texts);
            }
            it
        }
        Ast::Union(u) => {
            let fallback = format!("pub union {name}{}", sig::generics(&u.generics));
            let mut it = Item::new("struct", name, head("union").unwrap_or(fallback), doc);
            it.members = fields(&u.fields, texts);
            it
        }
        Ast::Enum(e) => {
            let fallback = format!("pub enum {name}{}", sig::generics(&e.generics));
            let mut it = Item::new("enum", name, head("enum").unwrap_or(fallback), doc);
            for v in e.variants.iter().filter(|v| !hidden(&v.attrs)) {
                let shape = match &v.kind {
                    EnumVariantKind::Unit => String::new(),
                    EnumVariantKind::Tuple(ts) => {
                        format!(
                            "({})",
                            ts.iter().map(sig::ty).collect::<Vec<_>>().join(", ")
                        )
                    }
                    EnumVariantKind::Named(fs) => format!(
                        " {{ {} }}",
                        fs.iter()
                            .map(|f| format!("{}: {}", f.name, sig::ty(&f.ty)))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    EnumVariantKind::Value(_) => " = ..".to_owned(),
                };
                let written = match v.kind {
                    EnumVariantKind::Named(_) => None,
                    _ => texts.header(v.span, &[&v.name], End::Comma),
                };
                it.members.push(Member {
                    name: v.name.clone(),
                    sig: written.unwrap_or_else(|| format!("{}{shape}", v.name)),
                    doc: doc_text(&v.attrs),
                });
            }
            it
        }
        Ast::Trait(t) => {
            let sup = if t.supertraits.is_empty() {
                String::new()
            } else {
                let s: Vec<String> = t.supertraits.iter().map(sig::ty).collect();
                format!(": {}", s.join(" + "))
            };
            let fallback = format!("pub trait {name}{}{sup}", sig::generics(&t.generics));
            let mut it = Item::new("trait", name, head("trait").unwrap_or(fallback), doc);
            for ti in &t.items {
                let m = match ti {
                    TraitItem::Method(m) if hidden(&m.attrs) => continue,
                    TraitItem::Method(m) => Member {
                        name: m.name.clone(),
                        sig: texts
                            .header(m.span, &["fn", &m.name], End::Body)
                            .unwrap_or_else(|| sig::fn_sig(m, true)),
                        doc: doc_text(&m.attrs),
                    },
                    TraitItem::AssocType {
                        name: n,
                        bounds,
                        default,
                    } => {
                        let mut s = format!("type {n}");
                        if !bounds.is_empty() {
                            let b: Vec<String> = bounds.iter().map(sig::ty).collect();
                            s = format!("{s}: {}", b.join(" + "));
                        }
                        if let Some(d) = default {
                            s = format!("{s} = {}", sig::ty(d));
                        }
                        Member {
                            name: n.clone(),
                            sig: s,
                            doc: String::new(),
                        }
                    }
                    TraitItem::Const { name: n, ty, .. } => Member {
                        name: n.clone(),
                        sig: format!("const {n}: {}", sig::ty(ty)),
                        doc: String::new(),
                    },
                };
                it.members.push(m);
            }
            it
        }
        Ast::TypeAlias(a) => {
            let fallback = format!(
                "pub type {name}{} = {}",
                sig::generics(&a.generics),
                sig::ty(&a.ty)
            );
            Item::new("type", name, head("type").unwrap_or(fallback), doc)
        }
        Ast::Const(c) => Item::new(
            "const",
            name,
            texts
                .header(span, &["const", name], End::Semi)
                .unwrap_or_else(|| format!("pub const {name}: {}", sig::ty(&c.ty))),
            doc,
        ),
        Ast::Static(s) => Item::new(
            "static",
            name,
            texts
                .header(span, &["static", name], End::Semi)
                .unwrap_or_else(|| {
                    format!(
                        "pub static {}{name}: {}",
                        if s.is_mut { "mut " } else { "" },
                        sig::ty(&s.ty)
                    )
                }),
            doc,
        ),
        _ => return None,
    };
    Some(it)
}

fn fields(fs: &[nabic_ast::NamedField], texts: &Texts<'_>) -> Vec<Member> {
    fs.iter()
        .filter(|f| f.vis == Visibility::Pub && !hidden(&f.attrs))
        .map(|f| Member {
            name: f.name.clone(),
            sig: texts
                .header(f.span, &[&f.name], End::Semi)
                .unwrap_or_else(|| format!("pub {}: {}", f.name, sig::ty(&f.ty))),
            doc: doc_text(&f.attrs),
        })
        .collect()
}

/// The impls of `items` onto their types in `m`, or `m.impls` for a type
/// declared elsewhere (kept when it has methods).
fn impls(m: &mut Module, items: &[Ast], texts: &Texts<'_>) {
    for item in items.iter().filter(|i| !i.is_local()) {
        let Ast::Impl(im) = item else { continue };
        let TyKind::Path(p) = &im.self_ty.kind else {
            continue;
        };
        let Some(ty_name) = p.segments.last().map(|s| s.name.clone()) else {
            continue;
        };
        let at = m.items.iter().position(|e| {
            e.name == ty_name && !matches!(e.kind, "fn" | "const" | "static" | "use")
        });
        let target = match at {
            Some(i) => &mut m.items[i],
            None => {
                if let Some(i) = m.impls.iter().position(|e| e.name == ty_name) {
                    &mut m.impls[i]
                } else {
                    m.impls
                        .push(Item::new("impl", &ty_name, String::new(), String::new()));
                    m.impls.last_mut().unwrap()
                }
            }
        };
        if im.trait_path.is_some() {
            target.traits.push(sig::impl_header(im));
            continue;
        }
        for ii in &im.items {
            match ii {
                ImplItem::Method(f) if f.vis == Visibility::Pub && !hidden(&f.attrs) => {
                    target.members.push(Member {
                        name: f.name.clone(),
                        sig: texts
                            .header(f.span, &["fn", &f.name], End::Body)
                            .unwrap_or_else(|| sig::fn_sig(f, true)),
                        doc: doc_text(&f.attrs),
                    });
                }
                ImplItem::Const(c) if c.vis == Visibility::Pub && !hidden(&c.attrs) => {
                    target.members.push(Member {
                        name: c.name.clone(),
                        sig: texts
                            .header(c.span, &["const", &c.name], End::Semi)
                            .unwrap_or_else(|| format!("pub const {}: {}", c.name, sig::ty(&c.ty))),
                        doc: doc_text(&c.attrs),
                    });
                }
                _ => {}
            }
        }
    }
    m.impls.retain(|it| !it.members.is_empty());
}

/// The modules, and lookups among them.
pub(crate) struct Mods<'a>(pub(crate) &'a [Module]);

impl<'a> Mods<'a> {
    pub(crate) fn at(&self, path: &str) -> Option<&'a Module> {
        self.0.iter().find(|m| m.path == path)
    }

    /// The module and item the absolute `path` names, through re-exports
    /// (at most `depth` more); the item is none for a module.
    pub(crate) fn lookup(
        &self,
        path: &str,
        depth: usize,
    ) -> Option<(&'a Module, Option<&'a Item>)> {
        if let Some(k) = path.rfind("::") {
            let name = &path[k + 2..];
            if let Some(q) = self.at(&path[..k]) {
                if let Some(it) = q
                    .items
                    .iter()
                    .find(|it| it.kind != "use" && it.name == name)
                {
                    return Some((q, Some(it)));
                }
                if depth > 0 {
                    for it in q.items.iter().filter(|it| it.kind == "use") {
                        for (p, n) in &it.paths {
                            if n != name {
                                continue;
                            }
                            for c in candidates(q, p) {
                                if let Some(found) = self.lookup(&c, depth - 1) {
                                    return Some(found);
                                }
                            }
                        }
                    }
                }
            }
        }
        self.at(path).map(|q| (q, None))
    }

    /// The item `path` (re-exported by `m`) names when the way to it
    /// stays in private modules; none when it reaches a public module, a
    /// module, or nothing documented.
    fn hidden_target(
        &self,
        m: &Module,
        path: &str,
        depth: usize,
    ) -> Option<(&'a Module, &'a Item)> {
        for c in candidates(m, path) {
            let Some(k) = c.rfind("::") else { continue };
            let name = &c[k + 2..];
            if let Some(q) = self.at(&c[..k]) {
                if let Some(found) = self.chase(q, name, depth) {
                    return Some(found);
                }
                // `json::X` in package `json` with a `mod json;`.
                if q.path != m.path && names(q, name) {
                    return None;
                }
                continue;
            }
            if self.at(&c).is_some() {
                return None;
            }
        }
        None
    }

    fn chase(&self, q: &'a Module, name: &str, depth: usize) -> Option<(&'a Module, &'a Item)> {
        if q.public {
            return None;
        }
        if let Some(it) = q
            .items
            .iter()
            .find(|it| it.kind != "use" && it.name == name)
        {
            return Some((q, it));
        }
        if depth == 0 {
            return None;
        }
        for it in q.items.iter().filter(|it| it.kind == "use") {
            for (p, n) in &it.paths {
                let target = if n == name {
                    p.clone()
                } else if n == "*" {
                    format!("{}{name}", p.trim_end_matches('*'))
                } else {
                    continue;
                };
                if let Some(found) = self.hidden_target(q, &target, depth - 1) {
                    return Some(found);
                }
            }
        }
        None
    }

    fn glob_module(&self, m: &Module, path: &str) -> Option<&'a Module> {
        let base = path.trim_end_matches("::*");
        candidates(m, base).iter().find_map(|c| self.at(c))
    }

    /// What a glob of the private `q` brings into `v`.
    fn glob_into(&self, q: &'a Module, v: &mut Module, names: &mut Vec<String>, depth: usize) {
        for x in q.items.iter().filter(|x| x.kind != "use") {
            if names.contains(&x.name) {
                continue;
            }
            names.push(x.name.clone());
            v.items.push(shown_as(x, &x.name, q, ""));
        }
        if depth == 0 {
            return;
        }
        for it in q.items.iter().filter(|it| it.kind == "use") {
            for (p, n) in &it.paths {
                if n == "*" {
                    if let Some(g) = self.glob_module(q, p).filter(|g| !g.public) {
                        self.glob_into(g, v, names, depth - 1);
                        continue;
                    }
                }
                if names.contains(n) {
                    continue;
                }
                names.push(n.clone());
                if let Some((d, x)) = self.hidden_target(q, p, depth - 1) {
                    v.items.push(shown_as(x, n, d, &it.doc));
                    continue;
                }
                let base = if n == "*" {
                    p.trim_end_matches("::*")
                } else {
                    p
                };
                let abs = candidates(q, base)
                    .into_iter()
                    .find(|c| {
                        if n == "*" {
                            self.at(c).is_some()
                        } else {
                            self.lookup(c, 8).is_some()
                        }
                    })
                    .map_or_else(|| p.clone(), |c| if n == "*" { c + "::*" } else { c });
                v.items.push(use_of(&abs, n));
            }
        }
    }

    fn public_parent(&self, p: &Module) -> Option<String> {
        let mut path = p.path.as_str();
        while path.contains("::") {
            path = up(path);
            if self.at(path).is_some_and(|q| q.public) {
                return Some(path.to_owned());
            }
        }
        None
    }
}

/// Absolute paths `path`, written in `m`, may mean, likeliest first.
pub(crate) fn candidates(m: &Module, path: &str) -> Vec<String> {
    {
        if let Some(r) = path.strip_prefix("self::") {
            return vec![join(&m.path, r)];
        }
        if let Some(r) = path
            .strip_prefix("sup::")
            .or_else(|| path.strip_prefix("super::"))
        {
            return vec![join(up(&m.path), r)];
        }
        if let Some(r) = path
            .strip_prefix("pkg::")
            .or_else(|| path.strip_prefix("crate::"))
        {
            return vec![join(&m.root, r)];
        }
        let mut out = vec![path.to_owned()];
        let mut p = m.path.as_str();
        while !p.is_empty() {
            out.push(join(p, path));
            p = up(p);
        }
        out
    }
}

/// Whether `q` declares or re-exports `name`.
fn names(q: &Module, name: &str) -> bool {
    q.items.iter().any(|it| {
        if it.kind == "use" {
            it.paths.iter().any(|(_, n)| n == name || n == "*")
        } else {
            it.name == name
        }
    })
}

/// `sig` with the declared name `from` (after `kind`) changed to `to`.
fn renamed(sig: &str, kind: &str, from: &str, to: &str) -> String {
    let key = format!("{kind} {from}");
    let Some(k) = sig.find(&key) else {
        return sig.to_owned();
    };
    let rest = &sig[k + key.len()..];
    if rest
        .chars()
        .next()
        .is_some_and(|c| c.is_alphanumeric() || c == '_')
    {
        return sig.to_owned();
    }
    format!("{}{kind} {to}{rest}", &sig[..k])
}

/// A copy of `x` (declared in `q`) shown as `name`, the doc on its `pub
/// use` (`doc`) first.
fn shown_as(x: &Item, name: &str, q: &Module, doc: &str) -> Item {
    let mut it = x.clone();
    it.doc = match (doc.trim().is_empty(), x.doc.trim().is_empty()) {
        (true, _) => x.doc.clone(),
        (_, true) => doc.to_owned(),
        _ => format!("{doc}\n\n{}", x.doc),
    };
    if name != x.name {
        let kw = if x.sig.contains("union ") {
            "union"
        } else {
            x.kind
        };
        it.sig = renamed(&x.sig, kw, &x.name, name);
        name.clone_into(&mut it.name);
    }
    if it.origin.is_none() {
        it.origin = Some((q.path.clone(), x.name.clone()));
    }
    it
}

/// A `pub use` of the absolute `path` as `name`.
fn use_of(path: &str, name: &str) -> Item {
    let tree = if last(path) == name {
        path.to_owned()
    } else {
        format!("{path} as {name}")
    };
    let mut it = Item::new("use", &tree, format!("pub use {tree}"), String::new());
    it.paths = vec![(path.to_owned(), name.to_owned())];
    it
}

/// The public modules as the pages show them, in the same order.
pub(crate) fn pages(all: &[Module]) -> Vec<Module> {
    let mods = Mods(all);
    let mut out: Vec<Module> = Vec::new();
    for m in all.iter().filter(|m| m.public) {
        let mut v = Module {
            items: Vec::new(),
            impls: Vec::new(),
            ..m.clone()
        };
        let mut names: Vec<String> = m
            .items
            .iter()
            .filter(|it| it.kind != "use")
            .map(|it| it.name.clone())
            .collect();
        for it in &m.items {
            if it.kind != "use" {
                v.items.push(it.clone());
                continue;
            }
            let mut skip = Vec::new();
            for (p, n) in &it.paths {
                if n == "*" {
                    if let Some(q) = mods.glob_module(m, p).filter(|q| !q.public) {
                        mods.glob_into(q, &mut v, &mut names, 8);
                        skip.push(n.clone());
                    }
                    continue;
                }
                let Some((q, x)) = mods.hidden_target(m, p, 8) else {
                    continue;
                };
                if names.contains(n) {
                    continue;
                }
                names.push(n.clone());
                v.items.push(shown_as(x, n, q, &it.doc));
                skip.push(n.clone());
            }
            if skip.len() == it.paths.len() {
                continue;
            }
            let mut u = it.clone();
            u.skip = skip;
            v.items.push(u);
        }
        v.impls.clone_from(&m.impls);
        out.push(v);
    }
    for p in all.iter().filter(|m| !m.public) {
        let Some(a) = mods.public_parent(p) else {
            continue;
        };
        let Some(v) = out.iter_mut().find(|v| v.path == a) else {
            continue;
        };
        for ii in &p.impls {
            let t = v.items.iter().position(|x| {
                !matches!(x.kind, "use" | "fn" | "const" | "static") && x.name == ii.name
            });
            let target = match t {
                Some(i) => Some(&mut v.items[i]),
                None => v.impls.iter_mut().find(|x| x.name == ii.name),
            };
            if let Some(t) = target {
                t.members.extend(ii.members.iter().cloned());
                t.traits.extend(ii.traits.iter().cloned());
            } else {
                v.impls.push(ii.clone());
            }
        }
    }
    out
}

/// An item's id: `struct.List`; a re-export by the names it brings in,
/// `reexport.List.ListIter` for `use list::{List, ListIter}`.
pub(crate) fn anchor(it: &Item) -> String {
    if it.kind == "impl" {
        return format!("impl.{}", it.name);
    }
    if it.kind != "use" {
        return format!("{}.{}", it.kind, it.name);
    }
    // As the sites spell it: the names after the last `::` or inside
    // the braces, both sides of an `as`.
    let tree = it.name.as_str();
    let tail = match (tree.find('{'), tree.rfind("::")) {
        (Some(k), _) => &tree[k + 1..],
        (None, Some(k)) => &tree[k + 2..],
        _ => tree,
    };
    let tail = tail.replace('}', "").replace(" as ", ",");
    let names: Vec<&str> = tail
        .split(',')
        .map(|w| {
            let n = w.trim();
            let n = n.rfind("::").map_or(n, |k| &n[k + 2..]);
            if n == "*" { "all" } else { n }
        })
        .filter(|n| !n.is_empty())
        .collect();
    format!("reexport.{}", names.join("."))
}
