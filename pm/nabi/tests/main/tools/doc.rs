//! `nabi doc`, `fmt` of a package, `inspect`, `demangle` and the tools'
//! usage errors.

#[allow(clippy::wildcard_imports)]
use crate::tools::*;

/// A library package `dir/name`.
fn lib(dir: &Path, name: &str) -> PathBuf {
    let pkg = dir.join(name);
    put(
        pkg.join("src/lib.nabi"),
        "/// Adds.\npub fn add(a: i32, b: i32) -> i32 {\n    return a + b;\n}\n",
    )
    .unwrap();
    pkg
}

/// `doc` of a package: its library, in target/doc/.
#[test]
fn doc_writes_the_library() {
    let env = setup();
    let dir = scratch("doc-lib");
    let app = lib(&dir, "app");
    put(
        app.join("src/main.nabi"),
        "println(f\"{app::add(1, 2)}\");\n",
    )
    .unwrap();
    let out = nabi_in(Some(&env), &app, &["doc"]);
    assert_exit(&out, 0);
    let index = fs::read_to_string(app.join("target/doc/app/index.html")).unwrap();
    assert!(index.contains("add"), "{index}");
}

/// `doc --open` opens the package's root page in `$BROWSER`; one that
/// cannot start is a warning naming the file.
#[test]
fn doc_open_accepted() {
    let dir = scratch("doc-open");
    let app = lib(&dir, "app");
    let doc = |browser: &Path| {
        build_nabic();
        Command::new(NABI)
            .current_dir(&app)
            .env("NABI_HOME", tool_home())
            .env("BROWSER", browser)
            .args(["doc", "--open"])
            .output()
            .unwrap()
    };
    // Any program that starts will do: nabi itself.
    let out = doc(Path::new(NABI));
    assert_exit(&out, 0);
    assert!(
        !text(&out.stderr).contains("cannot open"),
        "{}",
        text(&out.stderr)
    );
    let out = doc(&dir.join("no-such-browser"));
    assert_exit(&out, 0);
    let err = text(&out.stderr);
    assert!(
        err.contains("warning: cannot open a browser")
            && err
                .replace('\\', "/")
                .contains("target/doc/app/index.html in one"),
        "{err}"
    );
}

/// `doc --std` without a path, in a package: the standard library alone,
/// and the status line says so instead of a temporary package's path.
#[test]
fn doc_std_alone_names_the_library() {
    let dir = scratch("doc-std-alone");
    let app = lib(&dir, "app");
    let out = nabi_in(None, &app, &["doc", "--std"]);
    assert_exit(&out, 0);
    let err = text(&out.stderr);
    assert!(
        err.contains(" the standard library\n") && !err.contains("nabic-"),
        "{err}"
    );
    assert!(app.join("target/doc/std").is_dir());
    assert!(!app.join("target/doc/app").exists());
}

/// `fmt` of a package: `src/` and `tests/`, not the files beside them.
#[test]
fn fmt_takes_the_package_files() {
    let dir = scratch("fmt-package");
    let app = lib(&dir, "app");
    put(app.join("old.nabi"), "fn (\n").unwrap();
    put(app.join("tests/t.nabi"), "fn t() {\n  1;\n}\n").unwrap();
    let out = nabi_in(None, &app, &["fmt", "--check"]);
    assert_exit(&out, 1);
    assert!(
        text(&out.stdout).contains("t.nabi"),
        "{}",
        text(&out.stdout)
    );
    assert!(
        !text(&out.stderr).contains("old.nabi"),
        "{}",
        text(&out.stderr)
    );
}

/// `inspect` describes a prebuilt library; `demangle` decodes a symbol.
#[test]
fn inspect_and_demangle() {
    let env = setup();
    let dir = scratch("inspect");
    let geo = lib(&dir, "geo");
    assert_exit(&nabi_in(Some(&env), &geo, &["build"]), 0);
    let out = nabi_in(None, &dir, &["inspect", "geo/target/debug/libgeo.a"]);
    assert_exit(&out, 0);
    let info = text(&out.stdout);
    assert!(
        info.contains("Package: geo\n") && info.contains("pub fn add(a: i32, b: i32) -> i32"),
        "{info}"
    );
    let out = nabi_in(
        None,
        &dir,
        &["inspect", "--dump", "geo/target/debug/geo.nabihir"],
    );
    assert_exit(&out, 0);
    assert!(text(&out.stdout).contains("package: geo (hash: "));
    assert_exit(&nabi_in(None, &dir, &["inspect"]), 2);

    let out = nabi_in(
        None,
        &dir,
        &["demangle", "_Y3f8a1c2b_std_vec_push_a7c91d8e44b2f190"],
    );
    assert_exit(&out, 0);
    assert_eq!(text(&out.stdout), "std::vec::push\n");
    let out = nabi_in(
        None,
        &dir,
        &["demangle", "-v", "_Y3f8a1c2b_std_vec_push_a7c91d8e44b2f190"],
    );
    assert_exit(&out, 0);
    assert_eq!(
        text(&out.stdout),
        "package:  3f8a1c2b\npath:     std::vec\nname:     push\n\
         generics: a7c91d8e44b2f190 (type tuple hash)\n"
    );
    // No symbol: a filter over stdin.
    let out = piped(
        NABI,
        &["demangle"],
        "[.] __Y0b3cd907_core_collections_dict_Dict_0_0grow_1707c745048bf996 _main\n",
    );
    assert_exit(&out, 0);
    assert_eq!(
        text(&out.stdout),
        "[.] core::collections::dict::Dict::grow _main\n"
    );
    assert_exit(&nabi_in(None, &dir, &["demangle", "_ZN3foo"]), 1);
    // The REPL reads its cells from stdin, not a path `-`.
    let out = nabi_in(None, &dir, &["repl", "-"]);
    assert_exit(&out, 2);
    assert!(
        text(&out.stderr).contains("nabi repl -"),
        "{}",
        text(&out.stderr)
    );
}

/// A fresh scratch package directory holding `files` under `src/`.
fn package(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = scratch(name);
    for (path, text) in files {
        put(dir.join("src").join(path), text).unwrap();
    }
    dir
}

/// `nabi <args>` in `cwd` that exits `code`: its stdout and stderr.
#[track_caller]
fn exit(cwd: &Path, args: &[&str], code: i32) -> (String, String) {
    let out = nabi_in(None, cwd, args);
    assert_exit(&out, code);
    (text(&out.stdout), text(&out.stderr))
}

/// The internal links of the doc tree `dir` that point at no page or
/// at no anchor on it, and the pages' empty summaries in module lists.
fn broken_links(dir: &Path) -> (Vec<String>, Vec<String>) {
    let mut pages = Vec::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        for e in fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                todo.push(p);
            } else if p.extension().is_some_and(|x| x == "html") {
                pages.push(p);
            }
        }
    }
    let attr = |html: &str, name: &str| -> Vec<String> {
        let key = format!(" {name}=\"");
        html.match_indices(&key)
            .map(|(k, _)| {
                let v = &html[k + key.len()..];
                v[..v.find('"').unwrap()].replace("&amp;", "&")
            })
            .collect()
    };
    let ids = |p: &Path| attr(&fs::read_to_string(p).unwrap(), "id");
    let (mut broken, mut empty) = (Vec::new(), Vec::new());
    for p in &pages {
        let html = fs::read_to_string(p).unwrap();
        for u in attr(&html, "href").into_iter().chain(attr(&html, "src")) {
            if u.contains("://") || u.starts_with("mailto:") {
                continue;
            }
            let (path, frag) = u.split_once('#').unwrap_or((&u, ""));
            let target = if path.is_empty() {
                p.clone()
            } else {
                p.parent().unwrap().join(path)
            };
            let ok = target.exists() && (frag.is_empty() || ids(&target).iter().any(|i| i == frag));
            if !ok {
                broken.push(format!("{} -> {u}", p.display()));
            }
        }
        if html.contains("</code></a> <span></span></li>") {
            empty.push(p.display().to_string());
        }
    }
    (broken, empty)
}

/// The navigation of `doc_links_resolve_and_modules_have_summaries`'s
/// root page and its `geo::inner`: package name, breadcrumb, module
/// tree, the page's items and the dependencies.
fn nav_shown(root: &str, inner: &str) {
    for want in [
        "<div class=\"pkg-id\"><a class=\"pkg-n\" href=\"index.html\">doc</a></div>",
        "<span class=\"api-kind\">Package</span><a class=\"here\" href=\"index.html\" aria-current=\"page\">doc</a>",
        "<p class=\"api-pkg\"><a href=\"index.html\" aria-current=\"page\"><span>doc</span></a></p><div class=\"api-sec\"><h2 class=\"t-label\">Modules</h2><ul class=\"api-tree\"><li><a href=\"geo.html\">geo<span class=\"ct\" aria-hidden=\"true\">1</span></a>",
        "<div class=\"api-sec\"><h2 class=\"t-label\">In <code>doc</code></h2><h3 class=\"api-k\">",
        "<li><a href=\"#struct.Point\">Point</a></li>",
        "<div class=\"api-sec\"><h2 class=\"t-label\">Dependencies</h2><ul class=\"api-items api-deps\"><li><a href=\"../util/index.html\">util",
    ] {
        assert!(root.contains(want), "missing {want:?}:\n{root}");
    }
    assert!(!root.contains("class=\"up\""), "{root}");
    assert!(!root.contains("<details class=\"vers"), "{root}");
    let tree = &root[root.find("api-tree").unwrap()..];
    assert!(
        !tree[..tree.find("</ul></div>").unwrap()].contains("aria-current"),
        "{root}"
    );
    assert!(
        inner.contains("<a class=\"pkg-n\" href=\"../index.html\">doc</a>")
            && !inner.contains(">docs<"),
        "{inner}"
    );
    assert!(
        inner.contains("<a href=\"../index.html\">doc</a><span class=\"sep\" aria-hidden=\"true\">\u{203a}</span><a href=\"../geo.html\">geo</a>")
            && inner.contains("<li class=\"open\"><a href=\"../geo.html\">geo<span class=\"ct\" aria-hidden=\"true\">1</span></a><details class=\"tw\" open><summary><span class=\"sr-only\">geo: 1 submodule</span>"),
        "{inner}"
    );
}

/// Re-exports from private modules shown inline, with their impls; a
/// re-export of a public item a link entry; nested and inline modules;
/// a dependency's docs beside, linked into: every internal link lands
/// on a page and an anchor that exist, and documented modules have
/// summaries. No doc-set root: a stale one goes, other files stay.
#[test]
fn doc_links_resolve_and_modules_have_summaries() {
    let dep = package(
        "doc-dep",
        &[
            (
                "lib.nabi",
                "//! Utilities.\n\nmod core;\n\npub use core::Tool;\n\n/// Says hi.\npub fn hello() -> i32 { 1 }\n",
            ),
            (
                "core.nabi",
                "//! Inside.\n\n/// A tool.\npub struct Tool {}\n",
            ),
        ],
    );
    let dir = package(
        "doc",
        &[
            (
                "lib.nabi",
                "//! Shapes and **things**.\n//!\n//! - one\n//! - two\n\n\
                 pub mod geo;\npub mod outer;\n\nmod hidden;\nmod imp;\n\n\
                 /// From imp.\npub use imp::{Hidden, helper as aid};\npub use geo::Area;\n\n\
                 /// A point. See `Shape`, `Hidden` and [nabi](https://example.com).\n\
                 #[derive(Clone)]\npub struct Point {\n    /// Across.\n    pub x: f64;\n    \
                 secret: f64;\n}\n\n\
                 impl Point {\n    /// The origin.\n    pub const fn origin() -> Point { Point { x: 0.0, secret: 0.0 } }\n}\n\n\
                 /// Kinds.\npub enum Shape {\n    /// Round.\n    Circle(f64),\n    Square,\n}\n\n\
                 /// Adds.\n///\n/// ```\n/// add(1, 2) < 4\n/// ```\npub fn add(a: i32, b: i32) -> i32 { a + b }\n\n\
                 /// A tool of the dependency.\npub fn tool(t: util::Tool) -> Hidden { Hidden { n: 0 } }\n\n\
                 fn private_fn() {}\n",
            ),
            (
                "imp.nabi",
                "//! Private.\n\n/// Hidden away.\npub struct Hidden {\n    /// Count.\n    pub n: i32;\n}\n\n\
                 impl Hidden {\n    /// Makes one.\n    pub fn new() -> Hidden { Hidden { n: 1 } }\n}\n\n\
                 /// Helps.\npub fn helper(h: Hidden) -> Hidden { h }\n",
            ),
            (
                "geo.nabi",
                "//! Geometry.\n\npub mod inner;\n\n/// A region.\npub struct Area {}\n\n\
                 /// Area.\npub fn area(p: pkg::Point) -> f64 { p.x }\n",
            ),
            (
                "geo/inner.nabi",
                "//! Deeper.\n\n/// Deep.\npub fn deep(a: pkg::geo::Area, h: pkg::Hidden) {}\n",
            ),
            (
                "outer.nabi",
                "//! Outer.\n\n/// Nested inline.\npub mod nested {\n    //! More inline.\n\n    \
                 /// N.\n    pub fn n() {}\n}\n",
            ),
            ("hidden.nabi", "pub fn invisible() {}\n"),
        ],
    );
    let doc = dir.join("target/doc");
    put(doc.join("index.html"), "stale").unwrap();
    put(doc.join("notes.txt"), "mine").unwrap();
    let ext = format!("util={}", dep.display());
    let (_, err) = exit(&dir, &["doc", "--extern", &ext], 0);
    assert!(
        err.replace('\\', "/").contains("target/doc/doc/index.html"),
        "{err}"
    );
    assert!(!doc.join("index.html").exists(), "stale root kept");
    assert_eq!(fs::read_to_string(doc.join("notes.txt")).unwrap(), "mine");
    let read = |p: &str| fs::read_to_string(doc.join(p)).unwrap();
    let (broken, empty) = broken_links(&doc);
    assert!(broken.is_empty(), "broken links:\n{}", broken.join("\n"));
    assert!(empty.is_empty(), "empty summaries in {empty:?}");

    let root = read("doc/index.html");
    for want in [
        "Shapes and <strong>things</strong>.",
        "<li>one</li>",
        "id=\"struct.Point\"",
        "<pre class=\"sig\"><code><span class=\"nw\"><span class=\"k\">pub</span> <span class=\"k\">fn</span> add(</span>",
        "<span class=\"n\">1</span>, <span class=\"n\">2</span>) &lt; <span class=\"n\">4</span>",
        "<a href=\"https://example.com\">nabi</a>",
        "<a href=\"#enum.Shape\"><code>Shape</code></a>",
        "<h4 id=\"Point.x\"><code><span class=\"k\">pub</span> x: <span class=\"t\">f64</span></code>",
        "<p>Across.</p>",
        "<span class=\"k\">impl</span> <span class=\"t\">Clone</span> <span class=\"k\">for</span> <span class=\"t\">Point</span>",
        "<span class=\"mod\">pub const fn </span><span class=\"nm\">origin</span>",
        "<span class=\"t\">Circle</span>(<span class=\"t\">f64</span>)",
        "href=\"../style.css\"",
        "href=\"../nabi.css\"",
        "<a href=\"geo.html\"><code>geo</code></a> <span>Geometry.</span>",
        // Inline from the private module, its impl and docs along.
        "<section class=\"api-item\" id=\"struct.Hidden\">",
        "<p>From imp.</p>\n<p>Hidden away.</p>",
        "id=\"Hidden.new\"",
        "id=\"fn.aid\"",
        "<span class=\"k\">pub</span> <span class=\"k\">fn</span> aid(</span>",
        // A public item's re-export stays a link to its page.
        "<li id=\"struct.Area\"><a href=\"geo.html#struct.Area\"><code>Area</code></a> <span>A region.</span></li>",
        // Into the dependency, whose `Tool` is inline at its root.
        "<a class=\"t\" href=\"../util/index.html#struct.Tool\">Tool</a>",
        "<a href=\"#struct.Hidden\"><code>Hidden</code></a>",
    ] {
        assert!(root.contains(want), "missing {want:?}:\n{root}");
    }
    for absent in [
        "secret",
        "private_fn",
        "invisible",
        "imp.html",
        "hidden.html",
    ] {
        assert!(!root.contains(absent), "{absent} leaked:\n{root}");
    }
    assert!(!doc.join("doc/imp.html").exists() && !doc.join("doc/hidden.html").exists());
    nav_shown(&root, &read("doc/geo/inner.html"));
    let geo = read("doc/geo.html");
    assert!(
        geo.contains("<p>Geometry.</p>") && geo.contains("href=\"../style.css\""),
        "{geo}"
    );
    assert!(
        geo.contains("<a class=\"t\" href=\"index.html#struct.Point\">Point</a>"),
        "{geo}"
    );
    let inner = read("doc/geo/inner.html");
    assert!(
        inner.contains("href=\"../../style.css\"")
            && inner.contains("<a class=\"t\" href=\"../index.html#struct.Hidden\">Hidden</a>"),
        "{inner}"
    );
    let nested = read("doc/outer/nested.html");
    assert!(
        nested.contains("<p>Nested inline.\nMore inline.</p>"),
        "{nested}"
    );
    let util = read("util/index.html");
    assert!(
        util.contains("Says hi.") && util.contains("id=\"struct.Tool\""),
        "{util}"
    );

    let search = read("search-index.js");
    for want in [
        "\"p\":\"util::hello\",\"k\":\"fn\"",
        "\"p\":\"doc::Hidden\",\"k\":\"struct\",\"u\":\"doc/index.html#struct.Hidden\"",
        "\"p\":\"doc::Hidden::new\",\"k\":\"method\"",
        "doc::geo::area",
    ] {
        assert!(search.contains(want), "missing {want:?}:\n{search}");
    }
    assert!(!search.contains("invisible"), "{search}");
    for asset in [
        "search.js",
        "theme.js",
        "keys.js",
        "nabi.css",
        "style.css",
        "doc.css",
        "favicon.svg",
    ] {
        assert!(doc.join(asset).is_file(), "{asset}");
    }
    assert!(read("search.js").contains("SEARCH_INDEX"));
    assert!(doc.join("fonts/Geist-Variable.woff2").is_file());
}

/// `nabi doc --std` (no package): the standard library tiers alone, into
/// `./target/doc/`, one directory per tier.
#[test]
fn doc_std_documents_the_library() {
    let dir = scratch("doc-std-only");
    let (_, err) = exit(&dir, &["doc", "--std"], 0);
    let err = err.replace('\\', "/");
    assert!(err.contains("target/doc/std/index.html"), "{err}");
    let read = |p: &str| fs::read_to_string(dir.join("target/doc").join(p)).unwrap();
    assert!(!dir.join("target/doc/index.html").exists());
    // Each tier links the tiers below it as dependencies.
    let index = read("std/index.html");
    for want in [
        "<a href=\"../core/index.html\">core",
        "<a href=\"../bare/index.html\">bare",
    ] {
        assert!(index.contains(want), "missing {want:?}:\n{index}");
    }
    let list = read("core/collections/list.html");
    assert!(list.contains("id=\"struct.List\""), "{list}");
    assert!(list.contains("href=\"../../style.css\""), "{list}");
    let (broken, _) = broken_links(&dir.join("target/doc"));
    assert!(broken.is_empty(), "broken links:\n{}", broken.join("\n"));
    assert!(read("search-index.js").contains("core::collections::list::List"));
}
