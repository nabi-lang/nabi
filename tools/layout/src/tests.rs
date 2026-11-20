use crate::check::{Lang, check};
use crate::{repo_root, scan, summary};

fn kinds(src: &str, lang: Lang) -> Vec<String> {
    check(src, lang)
        .into_iter()
        .map(|f| format!("{}:{} {}", f.rule, f.line, f.kind))
        .collect()
}

#[test]
fn clean_header() {
    let src = "//! Doc.\n\npub mod a;\npub(crate) mod b;\n\nmod c;\n\npub use a::X;\n\n\
               use std::fmt;\nuse crate::c::Y;\n\nfn f() {}\n\n#[cfg(test)]\nmod tests {\n    \
               use super::*;\n\n    fn g() {\n        super::f();\n    }\n}\n";
    assert_eq!(kinds(src, Lang::Rust), Vec::<String>::new());
}

#[test]
fn header_order() {
    let src = "use a::b;\nmod x;\n\nuse c::d;\n\nfn f() {}\nmod tests;\n";
    assert_eq!(
        kinds(src, Lang::Rust),
        [
            "D:0 group-split",
            "D:0 group-split",
            "D:2 order",
            "D:2 no-blank-between-groups",
            "D:7 decl-after-items",
            "D:7 order",
        ]
    );
}

#[test]
fn blank_in_group_and_local_use() {
    let src = "use std::fmt;\n\nuse crate::x;\n\nfn f() {\n    use std::io;\n}\n";
    assert_eq!(
        kinds(src, Lang::Rust),
        ["D:2 blank-in-group", "D:6 local-use"]
    );
}

#[test]
fn macro_bodies_may_use() {
    let src = "macro_rules! m {\n    () => {\n        use std::io;\n    };\n}\n";
    assert_eq!(kinds(src, Lang::Rust), Vec::<String>::new());
}

#[test]
fn macro_reexport_follows_its_macro() {
    let src = "use std::fmt;\n\nmacro_rules! m {\n    () => {};\n}\npub(crate) use m;\n";
    assert_eq!(kinds(src, Lang::Rust), Vec::<String>::new());
}

#[test]
fn super_paths() {
    let src = "use super::x;\n\npub(super) fn f() {\n    super::super::g();\n}\n";
    assert_eq!(
        kinds(src, Lang::Rust),
        ["E:1 use-super", "E:3 vis-pub(super)", "E:4 path"]
    );
}

#[test]
fn nabi_sup_paths() {
    let src = "// A file.\nuse sup::x;\n\npub(sub) fn f() {\n    sup::g()\n}\n";
    assert_eq!(
        kinds(src, Lang::Nabi),
        ["E:2 use-sup", "E:4 vis-pub(sub)", "E:5 path"]
    );
}

#[test]
fn strings_and_comments_hide_tokens() {
    let src = "fn f() {\n    let _ = \"super::x use y;\";\n    // super::z\n    \
               let _ = r#\"mod a;\"#;\n    let _ = f\"{g(\"super::\")}\";\n}\n";
    assert_eq!(kinds(src, Lang::Nabi), Vec::<String>::new());
}

#[test]
fn path_attr() {
    let src = "#[path = \"x.rs\"]\nmod x;\n#[cfg_attr(windows, path = \"y.rs\")]\nmod y;\n";
    assert_eq!(kinds(src, Lang::Rust), ["A:1 path-attr", "A:3 path-attr"]);
}

/// Report only: prints what the tree still violates and never fails.
#[test]
fn report() {
    let v = scan(&repo_root());
    println!("{}", summary(&v));
}
