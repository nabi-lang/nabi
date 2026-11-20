//! `nabi doc`: static HTML API docs, in the layout of the Nabi sites'
//! API pages and with their stylesheets.
//!
//! Layout under the output directory (`target/doc/`):
//!
//! ```text
//! <pkg>/index.html           a package's root module
//! <pkg>/<mod>/....html       one page per public module (`a::b`: a/b.html)
//! search-index.js            `SEARCH_INDEX = [{p, k, u, d}, ..]`
//! search.js, theme.js, keys.js, nabi.css, style.css, doc.css, fonts/,
//! favicon.svg
//! ```
//!
//! Documented: the user package, its `--extern` deps and, with `--std`,
//! the standard library; `pub` items of public modules, and what a
//! `pub use` brings in from a private module (shown inline), but not
//! what is `#[doc(hidden)]`. Each item has the anchor `#<kind>.<name>`,
//! a member `#<Type>.<member>`, a re-export `#reexport.<names>`. Doc
//! text comes from `#[doc]` (`///`, `//!`) in a markdown subset:
//! headings, fenced code, inline code, links, `**bold**`, lists,
//! paragraphs. Signatures are shown as written; the types in them, and
//! names in inline code, link to the page that shows them. The navigation
//! (nav.rs) is the docs site's: the package's name and version in the
//! header, a breadcrumb, the package's module tree, the page's items and
//! the package's dependencies in the sidebar. There is no page above the
//! packages': the dependency links and search reach the whole doc set.

mod highlight;
mod kit;
mod model;
mod nav;
mod page;
mod sig;

use crate::doc::model::File;
use crate::doc::page::{Package, Site};
use crate::doc::sig::Texts;
use nabic_ast::{AttrBody, Attribute, FnDef, Item, Literal, Ty};
use nabic_core::Visibility;
use nabic_diagnostic::Json;
use nabic_interface::CompileResult;
use nabic_lexer::escape::{EscapeMode, unescape};
use std::path::{Path, PathBuf};

/// Which packages `generate` documents.
#[derive(Clone)]
pub struct Scope {
    /// The user package and its dependencies.
    pub user: bool,
    /// The standard library (`std::..`, `core::..`, `bare::..`).
    pub std: bool,
    /// The user package's version, shown beside its name.
    pub version: String,
}

/// Write the docs of `result` (built with `Session::set_keep_ast`) into
/// `out`; `name` is the user package's. Returns the root page of that
/// package, else of `std`, else of the first one documented.
pub fn generate(
    result: &CompileResult,
    name: &str,
    out: &Path,
    scope: &Scope,
) -> std::io::Result<PathBuf> {
    let files: Vec<File<'_>> = result
        .sources
        .iter()
        .filter(|f| {
            if nabic_core::in_toolchain(&f.package) {
                scope.std
            } else {
                scope.user
            }
        })
        .map(|f| {
            let (root, rel) = if f.package.is_empty() {
                (name.to_owned(), f.module.clone())
            } else {
                // The module path past the package's mount prefix.
                let skip = f.package.split("::").count().min(f.module.len());
                (f.package.clone(), f.module[skip..].to_vec())
            };
            File {
                dir: root.replace("::", "-"),
                root,
                rel,
                file: f,
            }
        })
        .collect();
    let texts = Texts(result.source_map.files());
    let all = model::collect(&files, &texts);
    let pages = model::pages(&all);
    // The user package first.
    let roots: Vec<&str> = all
        .iter()
        .filter(|m| m.path == m.root)
        .map(|m| m.root.as_str())
        .collect();
    let mut packages: Vec<Package> = Vec::new();
    for &root in &roots {
        let version = if root == name {
            scope.version.clone()
        } else if nabic_core::in_toolchain(root) {
            env!("CARGO_PKG_VERSION").to_owned()
        } else {
            dep_version(&files, root)
        };
        packages.push(Package {
            root: root.to_owned(),
            version,
            deps: deps_of(&all, &roots, root, root == name),
        });
    }
    packages.sort_by_key(|p| (p.root != name, p.root.clone()));
    let site = Site::new(&pages, &all, &packages);
    let main = [name, "std"]
        .iter()
        .find_map(|r| pages.iter().find(|m| m.path == *r))
        .or_else(|| {
            packages
                .iter()
                .find_map(|p| pages.iter().find(|m| m.path == p.root))
        })
        .map(|m| out.join(&m.out))
        .ok_or_else(|| std::io::Error::other("no package to document"))?;
    // A doc-set root an older `nabi doc` wrote: nothing links to it now.
    let stale = out.join("index.html");
    if stale.is_file() {
        std::fs::remove_file(&stale)?;
    }
    for (path, text) in site.render() {
        let file = out.join(&path);
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(file, text)?;
    }
    for (path, data) in kit::FILES {
        let file = out.join(path);
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(file, data)?;
    }
    Ok(main)
}

/// The documented packages `root` depends on: for the user package
/// (`user`) every other one but the standard library's tiers, and for
/// each package those its `use`s name.
fn deps_of(all: &[model::Module], roots: &[&str], root: &str, user: bool) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for &q in roots.iter().filter(|q| **q != root) {
        let named = all.iter().filter(|m| m.root == root).any(|m| {
            m.imports
                .iter()
                .any(|(p, _)| p == q || p.starts_with(&format!("{q}::")))
        });
        if named || (user && !nabic_core::in_toolchain(q)) {
            out.push(q.to_owned());
        }
    }
    out
}

/// The version in the manifest of the dependency mounted at `root`:
/// the `nabi.nabion` above its root file; empty when there is none.
fn dep_version(files: &[File<'_>], root: &str) -> String {
    let Some(f) = files.iter().find(|f| f.root == root && f.rel.is_empty()) else {
        return String::new();
    };
    f.file
        .path
        .ancestors()
        .skip(1)
        .take(4)
        .find_map(|d| std::fs::read_to_string(d.join("nabi.nabion")).ok())
        .map(|t| manifest_version(&t))
        .unwrap_or_default()
}

/// The top-level `version: ".."` field of a manifest's text.
fn manifest_version(text: &str) -> String {
    let b = text.as_bytes();
    let (mut k, mut depth) = (0, 0usize);
    while k < b.len() {
        match b[k] {
            b'"' => {
                k += 1;
                while k < b.len() && b[k] != b'"' {
                    k += if b[k] == b'\\' { 2 } else { 1 };
                }
            }
            b'/' if b.get(k + 1) == Some(&b'/') => {
                while k < b.len() && b[k] != b'\n' {
                    k += 1;
                }
            }
            b'{' | b'[' | b'(' => depth += 1,
            b'}' | b']' | b')' => depth = depth.saturating_sub(1),
            b'v' if depth == 1
                && text[k..].starts_with("version")
                && (k == 0 || !(b[k - 1].is_ascii_alphanumeric() || b[k - 1] == b'_')) =>
            {
                let rest = text[k + "version".len()..].trim_start();
                if let Some(v) = rest.strip_prefix(':').map(str::trim_start) {
                    if let Some(v) = v.strip_prefix('"') {
                        return v[..v.find('"').unwrap_or(0)].to_owned();
                    }
                }
            }
            _ => {}
        }
        k += 1;
    }
    String::new()
}

pub(crate) fn item_info(item: &Item) -> Option<(&'static str, &str, Visibility, &[Attribute])> {
    // An item in a function body is no API.
    if item.is_local() {
        return None;
    }
    Some(match item {
        Item::Fn(x) => ("fn", &x.name, x.vis, &x.attrs),
        Item::Struct(x) => ("struct", &x.name, x.vis, &x.attrs),
        Item::Enum(x) => ("enum", &x.name, x.vis, &x.attrs),
        Item::Union(x) => ("union", &x.name, x.vis, &x.attrs),
        Item::Trait(x) => ("trait", &x.name, x.vis, &x.attrs),
        Item::TypeAlias(x) => ("type", &x.name, x.vis, &x.attrs),
        Item::Const(x) => ("const", &x.name, x.vis, &x.attrs),
        Item::Static(x) => ("static", &x.name, x.vis, &x.attrs),
        Item::Mod(x) => ("mod", &x.name, x.vis, &x.attrs),
        Item::Impl(_) | Item::Use(_) | Item::Extern(_) => return None,
    })
}

/// The text of the `#[doc = ".."]` attributes (`///` lines), one line each.
pub(crate) fn doc_text(attrs: &[Attribute]) -> String {
    let lines: Vec<String> = attrs
        .iter()
        .filter(|a| a.name() == "doc")
        .filter_map(|a| match &a.body {
            AttrBody::Value(Literal::Str(s)) => Some(unescape(s, EscapeMode::Str).ok()?),
            AttrBody::Value(Literal::RawStr(s)) => Some(s.clone()),
            _ => None,
        })
        .collect();
    lines.join("\n")
}

/// `f`'s signature as plain text, under the name `name` (hover,
/// completion).
pub(crate) fn fn_sig_text(f: &FnDef, name: &str) -> String {
    let mut f = f.signature();
    name.clone_into(&mut f.name);
    sig::fn_sig(&f, false)
}

/// A type as written, plain text.
pub(crate) fn ty_text(t: &Ty) -> String {
    sig::ty(t)
}

fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            c => o.push(c),
        }
    }
    o
}

/// One `SEARCH_INDEX` record: path, kind, URL, summary.
fn search_entry(path: &str, kind: &str, url: &str, doc: &str) -> Json {
    Json::obj([
        ("p", Json::str(path)),
        ("k", Json::str(kind)),
        ("u", Json::str(url)),
        ("d", Json::str(doc.replace('`', ""))),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::model::{Module, anchor};
    use crate::doc::nav;
    use nabic_interface::SourceFile;

    fn parse(src: &str) -> nabic_ast::Program {
        let (tokens, _) = nabic_lexer::Lexer::new(src, "a.nabi").lex_all();
        let (p, diags) = nabic_parser::parse_program(&tokens, "a.nabi");
        assert!(diags.is_empty(), "{diags:?}");
        p
    }

    /// The pages of package `k` made of `files` (module path, source).
    fn pages_of(files: &[(&str, &str)]) -> (Vec<Module>, Vec<Module>) {
        let parsed: Vec<SourceFile> = files
            .iter()
            .map(|(m, src)| {
                let p = parse(src);
                SourceFile {
                    path: PathBuf::from(format!("{m}.nabi")),
                    file_id: 0,
                    package: String::new(),
                    module: m
                        .split('/')
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                        .collect(),
                    attrs: p.attrs,
                    items: p.items,
                    stmts: Vec::new(),
                    expanded: Vec::new(),
                    template_names: Vec::new(),
                }
            })
            .collect();
        let files: Vec<File<'_>> = parsed
            .iter()
            .map(|f| File {
                root: "k".to_owned(),
                dir: "k".to_owned(),
                rel: f.module.clone(),
                file: f,
            })
            .collect();
        let all = model::collect(&files, &Texts(&[]));
        let pages = model::pages(&all);
        (all, pages)
    }

    fn names(m: &Module) -> String {
        m.items
            .iter()
            .map(|it| format!("{}.{}", it.kind, it.name))
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn doc_comments_are_attributes() {
        let p = parse(
            "//! Module doc.\n//!More.\n\n/// Adds.\n#[inline]\n/// Twice.\n//// not doc\n\
             pub fn add() {}\n",
        );
        assert_eq!(doc_text(&p.attrs), "Module doc.\nMore.");
        let Item::Fn(f) = &p.items[0] else { panic!() };
        assert_eq!(doc_text(&f.attrs), "Adds.\nTwice.");
        let p = parse(
            "/*! Module\n    doc. */\n/**\n * Adds \"1\".\n *\n * \\n\n */\npub fn add() {}\n",
        );
        assert_eq!(doc_text(&p.attrs), "Module\n   doc.");
        let Item::Fn(f) = &p.items[0] else { panic!() };
        assert_eq!(doc_text(&f.attrs), "Adds \"1\".\n\n\\n");
    }

    /// The fixture of the web kit's apidoc `private_reexports_inline`:
    /// both generators show the same items, in the same order.
    #[test]
    fn private_reexports_inline() {
        let (all, ps) = pages_of(&[
            (
                "",
                "pub mod open;\nmod buffer;\nmod glob;\nmod ints;\n/// From buffer.\n\
                 pub use buffer::{BufReader, LineWriter as Lines};\npub use pkg::open::Shown;\n\
                 pub use glob::*;\npub fn own() {}\n",
            ),
            (
                "buffer",
                "/// Reads in chunks.\npub struct BufReader { pub cap: usize; }\nimpl BufReader {\n    \
                 /// New.\n    pub fn new() -> BufReader { BufReader { cap: 0 } }\n}\n\
                 impl Read for BufReader {}\npub struct LineWriter<T> {}\npub struct Unused {}\n",
            ),
            ("open", "pub struct Shown {}\n"),
            (
                "glob",
                "mod deep;\npub fn globbed() {}\npub fn own() {}\npub use deep::Deep;\n\
                 pub use pkg::open::Shown as Again;\n",
            ),
            ("glob/deep", "/// Deep.\npub enum Deep { A, B }\n"),
            (
                "ints",
                "impl i8 {\n    pub fn bits() -> u32 { 8 }\n}\nimpl BufReader {\n    pub fn more(self) {}\n}\n",
            ),
        ]);
        let paths: Vec<&str> = all.iter().map(|m| m.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "k",
                "k::open",
                "k::buffer",
                "k::glob",
                "k::glob::deep",
                "k::ints"
            ]
        );
        assert_eq!(ps.len(), 2);
        assert_eq!(ps[1].out, "k/open.html");
        assert_eq!(
            names(&ps[0]),
            "struct.BufReader struct.Lines use.pkg::open::Shown fn.globbed enum.Deep \
             use.k::open::Shown as Again fn.own"
        );
        let br = &ps[0].items[0];
        assert_eq!(br.doc, "From buffer.\n\nReads in chunks.");
        let ms: Vec<&str> = br.members.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(ms, ["cap", "new", "more"]);
        assert_eq!(br.traits, ["impl Read for BufReader"]);
        assert_eq!(ps[0].items[1].sig, "pub struct Lines<T>");
        assert_eq!(ps[0].impls.len(), 1);
        assert_eq!(ps[0].impls[0].name, "i8");
        // Anchors as the sites spell them.
        assert_eq!(anchor(&ps[0].items[0]), "struct.BufReader");
        assert_eq!(anchor(&ps[0].items[2]), "reexport.Shown");
        assert_eq!(anchor(&ps[0].items[5]), "reexport.Shown.Again");
        assert_eq!(anchor(&ps[0].impls[0]), "impl.i8");
    }

    /// Items in a function body are not the package's API; nor is what is
    /// `#[doc(hidden)]`.
    #[test]
    fn local_and_hidden_items_are_not_documented() {
        let (_, ps) = pages_of(&[(
            "",
            "pub trait Show { fn show(self) -> str; }\npub struct P { pub x: i32; #[doc(hidden)] pub raw: i32; }\n\
             pub fn f() {\n    struct Q {}\n    impl Show for P { fn show(self) -> str { \"\" } }\n    \
             impl P { fn m(self) {} }\n}\n\
             impl P { pub fn get(self) {} #[doc(hidden)] pub fn peek(self) {} }\n\
             /// A hook.\n#[doc(hidden)]\npub fn hook() {}\n#[doc(hidden)]\npub mod inner { pub fn deep() {} }\n",
        )]);
        assert_eq!(ps.len(), 1, "a hidden module has no page");
        assert_eq!(names(&ps[0]), "trait.Show struct.P fn.f");
        let p = &ps[0].items[1];
        let ms: Vec<&str> = p.members.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(ms, ["x", "get"]);
        assert!(p.traits.is_empty(), "{:?}", p.traits);
        assert!(ps[0].children.is_empty());
    }

    /// The twin of the web kit's `nav_markup`: the same fixture, the
    /// same markup.
    #[test]
    fn nav_markup() {
        let (_, ps) = pages_of(&[
            (
                "",
                "pub mod factor;\npub mod ops;\n/// Q.\npub struct Qr {}\npub fn lu() {}\npub fn qr() {}\n",
            ),
            ("factor", "pub mod qr;\npub mod lu;\n"),
            ("factor/qr", "pub fn householder() {}\n"),
            ("factor/lu", "pub mod blocked;\n"),
            ("factor/lu/blocked", "pub fn panel() {}\n"),
            ("ops", "pub mod scalar;\n"),
            ("ops/scalar", "pub const ONE: i32 = 1;\n"),
        ]);
        let at = |p: &str| ps.iter().find(|m| m.path == p).unwrap();
        let rel = |from: &str| {
            let from = from.to_owned();
            move |q: &Module| crate::doc::page::rel(&from, &q.out)
        };
        let qr = at("k::factor::qr");
        assert_eq!(qr.out, "k/factor/qr.html");
        let href = rel(&qr.out);
        assert_eq!(
            nav::crumb(nav::kind_of(qr), &nav::trail(&ps, qr, &href)),
            "<nav class=\"crumb api-crumb\" aria-label=\"Breadcrumb\"><span class=\"api-kind\">Module</span><a href=\"../index.html\">k</a><span class=\"sep\" aria-hidden=\"true\">\u{203a}</span><a href=\"../factor.html\">factor</a><span class=\"sep\" aria-hidden=\"true\">\u{203a}</span><a class=\"here\" href=\"qr.html\" aria-current=\"page\">qr</a></nav>"
        );
        assert_eq!(
            nav::tree(&ps, qr, &href),
            "<ul class=\"api-tree\"><li class=\"open\"><a href=\"../factor.html\">factor<span class=\"ct\" aria-hidden=\"true\">2</span></a><details class=\"tw\" open><summary><span class=\"sr-only\">factor: 2 submodules</span></summary><ul><li><a href=\"lu.html\">lu<span class=\"ct\" aria-hidden=\"true\">1</span></a><details class=\"tw\"><summary><span class=\"sr-only\">lu: 1 submodule</span></summary><ul><li><a href=\"lu/blocked.html\">blocked</a></li></ul></details></li><li class=\"open\"><a href=\"qr.html\" aria-current=\"page\">qr</a></li></ul></details></li><li><a href=\"../ops.html\">ops<span class=\"ct\" aria-hidden=\"true\">1</span></a><details class=\"tw\"><summary><span class=\"sr-only\">ops: 1 submodule</span></summary><ul><li><a href=\"../ops/scalar.html\">scalar</a></li></ul></details></li></ul>"
        );
        assert_eq!(
            nav::items(&ps[0]),
            "<div class=\"api-sec\"><h2 class=\"t-label\">In <code>k</code></h2><h3 class=\"api-k\">Structs <span class=\"ct\">1</span></h3><ul class=\"api-items\"><li><a href=\"#struct.Qr\">Qr</a></li></ul><h3 class=\"api-k\">Functions <span class=\"ct\">2</span></h3><ul class=\"api-items\"><li><a href=\"#fn.lu\">lu</a></li><li><a href=\"#fn.qr\">qr</a></li></ul></div>"
        );
        assert_eq!(
            nav::items(qr),
            "<div class=\"api-sec\"><h2 class=\"t-label\">In <code>factor::qr</code></h2><h3 class=\"api-k\">Functions <span class=\"ct\">1</span></h3><ul class=\"api-items\"><li><a href=\"#fn.householder\">householder</a></li></ul></div>"
        );
        let pkg = |name, version, href: &str| nav::Package {
            name,
            version,
            href: href.to_owned(),
            here: false,
        };
        let head = nav::Package {
            here: true,
            ..pkg("k", "0.1.0", "index.html")
        };
        assert_eq!(
            nav::side(
                &head,
                "<ul></ul>",
                "",
                "Dependencies",
                &[
                    pkg("dep", "1.2.0", "../dep/index.html"),
                    pkg("gone", "", "")
                ]
            ),
            "<div class=\"api-nav\"><p class=\"api-pkg\"><a href=\"index.html\" aria-current=\"page\"><span>k</span><small>0.1.0</small></a></p><div class=\"api-sec\"><h2 class=\"t-label\">Modules</h2><ul></ul></div><div class=\"api-sec\"><h2 class=\"t-label\">Dependencies</h2><ul class=\"api-items api-deps\"><li><a href=\"../dep/index.html\">dep<small>1.2.0</small></a></li><li><span>gone</span></li></ul></div></div>"
        );
        assert_eq!(
            nav::crumb("Package", &nav::trail(&ps, &ps[0], &rel(&ps[0].out))),
            "<nav class=\"crumb api-crumb\" aria-label=\"Breadcrumb\"><span class=\"api-kind\">Package</span><a class=\"here\" href=\"index.html\" aria-current=\"page\">k</a></nav>"
        );
        let top = nav::tree(&ps, &ps[0], &rel(&ps[0].out));
        assert!(
            top.starts_with("<ul class=\"api-tree\"><li><a href=\"factor.html\">factor<span class=\"ct\" aria-hidden=\"true\">2</span></a>")
                && !top.contains("aria-current"),
            "{top}"
        );
        let scalar = at("k::ops::scalar");
        assert!(nav::tree(&ps, scalar, &rel(&scalar.out)).contains(
            "<li class=\"open\"><a href=\"../ops.html\">ops<span class=\"ct\" aria-hidden=\"true\">1</span></a><details class=\"tw\" open>"
        ));
        assert_eq!(nav::tree(&ps[..1], &ps[0], &href), "");
        let h = |level, id: &str, text: &str| nav::Head {
            level,
            id: id.to_owned(),
            text: text.to_owned(),
        };
        assert_eq!(
            nav::rail(&[
                h(2, "struct", "Structs"),
                h(3, "struct.P", "k::P"),
                h(2, "fn", "Functions"),
                h(3, "fn.a", "k::a")
            ]),
            ""
        );
        assert_eq!(
            nav::rail(&[
                h(2, "struct", "Structs"),
                h(3, "struct.Qr", "k::Qr"),
                h(4, "Qr.solve", "k::Qr::solve"),
                h(4, "Qr.det", "k::Qr::det"),
                h(3, "struct.P", "k::P"),
                h(2, "fn", "Functions"),
                h(3, "fn.a", "k::a")
            ]),
            "<li><a href=\"#struct\">Structs</a></li><li class=\"grp\"><details open><summary><code>Qr</code><span class=\"ct\">2</span></summary><ul><li class=\"m\"><a href=\"#Qr.det\"><code>det</code></a></li><li class=\"m\"><a href=\"#Qr.solve\"><code>solve</code></a></li></ul></details></li><li><a href=\"#fn\">Functions</a></li>"
        );
    }

    /// A dependency's version comes from the top level of its manifest.
    #[test]
    fn manifest_versions() {
        let text = "// version: \"9\"\nPackage {\n    name: \"a\",\n    deps: { \"b\": Dep::version(\"1\"), version: \"8\" },\n    \
                    description: \"no version: here\",\n    version: \"0.3.1\",\n}\n";
        assert_eq!(manifest_version(text), "0.3.1");
        assert_eq!(manifest_version("Package { name: \"a\" }"), "");
    }

    /// A child module named `index` does not take the root's page.
    #[test]
    fn index_module_page() {
        assert_eq!(model::page_of("k", &[]), "k/index.html");
        assert_eq!(
            model::page_of("k", &["index".to_owned()]),
            "k/index/index.html"
        );
        assert_eq!(
            model::page_of("k", &["a".to_owned(), "index".to_owned()]),
            "k/a/index.html"
        );
    }
}
