//! Meta blocks and macros: expansion, hygiene, name scoping.

#[allow(clippy::wildcard_imports)]
use crate::tests::*;

/// Parse with the meta blocks of `src` registered (the driver's pre-pass).
fn parse_meta(src: &str) -> (nabic_ast::Program, Vec<nabic_diagnostic::NabiDiagnostic>) {
    let (tokens, _) = Lexer::new(src, "test").lex_all();
    let mut table = crate::MetaTable::default();
    for d in crate::collect_meta_blocks(&tokens, "test") {
        assert!(table.add(d).is_none());
    }
    crate::parse_program_meta(
        &tokens,
        "test",
        "",
        &crate::ExtOps::default(),
        &std::sync::Arc::new(table),
        &std::sync::Arc::default(),
        None,
    )
}

#[test]
fn meta_derive_expands_fields() {
    let (p, diags) = parse_meta(
        "#[impl_derive(Tr)] pub @derive { impl Tr for @Self { fn n(self) -> i32 { \
         let n = 0; @for_fields { n = n + self.@field; } return n; } } }\n\
         #[derive(Tr)] struct S { a: i32; b: i32; }",
    );
    assert!(diags.is_empty(), "{diags:?}");
    // The struct (its user derive dropped from `#[derive]`) and the impl.
    let Item::Struct(s) = &p.items[0] else {
        panic!()
    };
    assert!(s.attrs.is_empty());
    let Item::Impl(i) = &p.items[1] else { panic!() };
    assert_eq!(i.items.len(), 1);
}

#[test]
fn meta_macro_block_registers() {
    // `#[impl_macro(name)] @macro { (matcher) => { template } }`
    // is collected as a Macro meta definition and registered as `name!`.
    let src = "#[impl_macro(dup)] pub @macro { (@e: expr) => { (@e, @e) } }\n";
    let (tokens, _) = Lexer::new(src, "test").lex_all();
    let defs = crate::collect_meta_blocks(&tokens, "test");
    assert_eq!(defs.len(), 1, "{defs:?}");
    assert!(matches!(defs[0].kind, crate::MetaKind::Macro));
    assert_eq!(defs[0].name, "dup");
    let mut table = crate::MetaTable::default();
    assert!(table.add(defs[0].clone()).is_none());
    assert!(table.find_macro("dup").is_some());
    // A macro is not an item-attached derive / attribute trigger.
    assert!(!table.defines("dup"));
}

#[test]
fn macro_expr_simple() {
    let (p, diags) = parse_meta(
        "#[impl_macro(twice)] pub @macro { (@e: expr) => { (@e, @e) } }\n\
         fn f() -> (i32, i32) { return twice!(1 + 2); }",
    );
    assert!(diags.is_empty(), "{diags:?}");
    assert!(matches!(p.items.last(), Some(Item::Fn(_))));
}

#[test]
fn macro_expr_multi_arg_and_hygiene() {
    // Three `expr` fragments, and a template-local `v` that must be made
    // hygienic (not clash with a caller `v`).
    let (_, diags) = parse_meta(
        "#[impl_macro(clamp)] pub @macro { (@x: expr, @lo: expr, @hi: expr) => { \
         { let v = @x; if v < @lo { @lo } else if v > @hi { @hi } else { v } } } }\n\
         fn f(v: i32) -> i32 { return clamp!(v, 0, 9); }",
    );
    assert!(diags.is_empty(), "{diags:?}");
}

#[test]
fn macro_expr_repetition() {
    let (_, diags) = parse_meta(
        "#[impl_macro(list)] pub @macro { (@rep[sep = \",\"] { @x: expr }) => { \
         { let acc = List::new(); @for @x { acc.push(@x); } acc } } }\n\
         fn f() -> List<i32> { return list![1, 2, 3]; }",
    );
    assert!(diags.is_empty(), "{diags:?}");
}

#[test]
fn macro_item_repetition_impls() {
    let (p, diags) = parse_meta(
        "#[impl_macro(imp)] pub @macro { (@rep[sep = \",\"] { @t: ty }) => { \
         @for @t { impl Foo for @t {} } } }\n\
         imp!(i8, i16, i32);",
    );
    assert!(diags.is_empty(), "{diags:?}");
    assert_eq!(p.items.len(), 3, "{:?}", p.items);
    assert!(p.items.iter().all(|i| matches!(i, Item::Impl(_))));
}

#[test]
fn macro_item_name_synthesis() {
    let (p, diags) = parse_meta(
        "#[impl_macro(mk)] pub @macro { (@rep[sep = \",\"] { @n: ident }) => { \
         @for @n { fn @concat(do_, @n)() {} } } }\n\
         mk!(a, b);",
    );
    assert!(diags.is_empty(), "{diags:?}");
    let names: Vec<&str> = p
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Fn(f) => Some(f.name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(names, ["do_a", "do_b"], "{names:?}");
}

#[test]
fn macro_stmt_position() {
    // `name!(..);` at statement position splices in a `let` whose binding
    // comes from a metavariable (so it is visible to the caller's code).
    let (_, diags) = parse_meta(
        "#[impl_macro(decl)] pub @macro { (@n: ident, @v: expr) => { let @n = @v; } }\n\
         fn f() -> i64 { decl!(x, 41i64); return x + 1i64; }",
    );
    assert!(diags.is_empty(), "{diags:?}");
}

#[test]
fn macro_type_position() {
    let (_, diags) = parse_meta(
        "#[impl_macro(pair_of)] pub @macro { (@t: ty) => { (@t, @t) } }\n\
         fn f(x: pair_of!(i32)) -> i32 { return x.0; }",
    );
    assert!(diags.is_empty(), "{diags:?}");
}

#[test]
fn macro_pattern_position() {
    let (_, diags) = parse_meta(
        "#[impl_macro(both)] pub @macro { (@a: pat, @b: pat) => { (@a, @b) } }\n\
         fn f(p: (i32, i32)) -> i32 { match p { both!(a, b) => { return a + b; } } }",
    );
    assert!(diags.is_empty(), "{diags:?}");
}

#[test]
fn macro_expansion_is_recorded() {
    // The expansion is recorded (surfaced by `nabic --emit=expansions`).
    let (p, diags) = parse_meta(
        "#[impl_macro(twice)] pub @macro { (@e: expr) => { (@e, @e) } }\n\
         fn f() -> (i32, i32) { return twice!(1i32); }",
    );
    assert!(diags.is_empty(), "{diags:?}");
    assert!(
        p.expanded
            .iter()
            .any(|e| e.contains("expanded from twice!")),
        "{:?}",
        p.expanded
    );
}

#[test]
fn macro_string_interpolation() {
    // `@name` inside a string interpolates the binding (per-element in @for).
    let (p, diags) = parse_meta(
        "#[impl_macro(tag)] pub @macro { (@rep[sep = \",\"] { @t: ident }) => { \
         @for @t { fn @concat(g_, @t)() -> str { return \"@t\"; } } } }\n\
         tag!(a, b);",
    );
    assert!(diags.is_empty(), "{diags:?}");
    let joined = p.expanded.join("\n");
    assert!(
        joined.contains("\"a\"") && joined.contains("\"b\""),
        "{joined}"
    );
}

#[test]
fn macro_expr_no_rule_matches() {
    let (_, diags) = parse_meta(
        "#[impl_macro(one)] pub @macro { (@a: expr, @b: expr) => { (@a, @b) } }\n\
         fn f() -> i32 { return one!(42); }",
    );
    assert!(
        diags.iter().any(|d| format!("{d:?}").contains("no rule")),
        "{diags:?}"
    );
}

#[test]
fn meta_macro_rejects_builtin_bang_name() {
    // `#[impl_macro]` cannot take a reserved built-in `!`-form name.
    let src = "#[impl_macro(cfg)] pub @macro { (@e: expr) => { @e } }\n";
    let (tokens, _) = Lexer::new(src, "test").lex_all();
    let defs = crate::collect_meta_blocks(&tokens, "test");
    assert_eq!(defs.len(), 1, "{defs:?}");
    let mut table = crate::MetaTable::default();
    // `cfg!` is a reserved built-in form, so registration is refused.
    assert!(table.add(defs[0].clone()).is_some());
}

#[test]
fn meta_attribute_builtin_name_refused() {
    // Every built-in attribute name is refused.
    let src = "#[impl_attribute(bench)] pub @attribute { @item }\n\
               #[impl_attribute(test)] pub @attribute { @item }\n";
    let (tokens, _) = Lexer::new(src, "test").lex_all();
    let defs = crate::collect_meta_blocks(&tokens, "test");
    assert_eq!(defs.len(), 2, "{defs:?}");
    let mut table = crate::MetaTable::default();
    for (def, name) in defs.iter().zip(["bench", "test"]) {
        let d = table.add(def.clone()).expect("an error");
        assert_eq!(d.severity, nabic_diagnostic::Severity::Error, "{d:?}");
        assert!(
            d.notes.iter().any(|n| n.contains(&format!("my_{name}"))),
            "{d:?}"
        );
        assert!(!table.knows_attribute(name));
    }
}

#[test]
fn meta_error_points_at_template_and_use() {
    let (_, diags) = parse_meta(
        "#[impl_derive(Tr)] pub @derive {\n  impl Tr for @Self { fn f() { @nope; } }\n}\n\
         #[derive(Tr)]\nstruct S { a: i32; }",
    );
    let d = diags.first().expect("an error");
    assert!(
        d.message.contains("unknown template token `@nope`"),
        "{d:?}"
    );
    assert_eq!(d.primary.as_ref().map(|l| l.span.line), Some(2));
    assert_eq!(d.secondary.first().map(|l| l.span.line), Some(4));
}

#[test]
fn meta_hygiene_renames_locals_only() {
    // `x` / `i` are template locals; `Other` (after `impl .. for`) and
    // `len` (a member) are not.
    let (p, diags) = parse_meta(
        "#[impl_derive(Tr)] pub @derive { impl Tr for Other { fn f() { \
         let x = 1; for i in 0..x { x = i.len(); } } } }\n\
         #[derive(Tr)] struct S { a: i32; }",
    );
    assert!(diags.is_empty(), "{diags:?}");
    let text = format!("{:?}", p.items[1]);
    assert!(text.contains("\"x__meta\"") && text.contains("\"i__meta\""));
    assert!(text.contains("\"Other\"") && text.contains("\"len\""));
    assert!(!text.contains("Other__meta") && !text.contains("len__meta"));
}

#[test]
fn macro_probe_multirule() {
    // Several rules; the first matching one is chosen by arity.
    let (_, d) = parse_meta(
        "#[impl_macro(pick)] pub @macro { (@a: expr) => { (@a,) } (@a: expr, @b: expr) => { (@a, @b) } }\n\
         fn f() -> (i32, i32) { return pick!(1i32, 2i32); }",
    );
    assert!(d.is_empty(), "multirule: {d:?}");
}

#[test]
fn macro_probe_trailing_sep() {
    let (_, d) = parse_meta(
        "#[impl_macro(li)] pub @macro { (@rep[sep = \",\"] { @x: expr }) => { \
         { let acc = List::new(); @for @x { acc.push(@x); } acc } } }\n\
         fn f() -> List<i32> { return li![1, 2, 3,]; }",
    );
    assert!(d.is_empty(), "trailing sep: {d:?}");
}

#[test]
fn macro_probe_nested_rep() {
    // Nested @rep: rows of columns.
    let (_, d) = parse_meta(
        "#[impl_macro(grid)] pub @macro { (@rep[sep = \";\"] { @rep[sep = \",\"] { @x: expr } }) => { \
         { let n = 0i32; @for @x { @for @x { n = n + @x; } } n } } }\n\
         fn f() -> i32 { return grid![1, 2; 3, 4]; }",
    );
    assert!(d.is_empty(), "nested rep: {d:?}");
    // One `@for` short of the depth: the use is a sequence.
    let (_, d) = parse_meta(
        "#[impl_macro(grid)] pub @macro { (@rep[sep = \";\"] { @rep[sep = \",\"] { @x: expr } }) => { \
         { let n = 0i32; @for @x { n = n + @x; } n } } }\n\
         fn f() -> i32 { return grid![1, 2; 3, 4]; }",
    );
    assert!(
        d.iter()
            .any(|x| x.message.contains("`@x` is a sequence here")),
        "{d:?}"
    );
}

#[test]
fn macro_rejects_shape_tokens() {
    // Type-driven template tokens (@apply_if, @Self, @field, ..) are not
    // valid in a macro; it has no shape to test.
    let (_, d) = parse_meta(
        "#[impl_macro(ai)] pub @macro { (@e: expr) => { @apply_if(true) { @e } } }\n\
         fn f() -> i32 { return ai!(5i32); }",
    );
    assert!(
        d.iter()
            .any(|x| format!("{x:?}").contains("not valid in a macro template")),
        "{d:?}"
    );
}

#[test]
fn macro_unknown_fragment_errors() {
    let (_, d) = parse_meta(
        "#[impl_macro(bad)] pub @macro { (@x: wat) => { @x } }\n\
         fn f() -> i32 { return bad!(1i32); }",
    );
    assert!(
        d.iter()
            .any(|x| format!("{x:?}").contains("unknown fragment")),
        "{d:?}"
    );
}

#[test]
fn macro_tuple_rep() {
    // A @rep of `( @t, @v )` tuples (bounds / defaults carried per type).
    let (p, d) = parse_meta(
        "#[impl_macro(defs)] pub @macro { (@rep[sep = \",\"] { (@t: ty, @v: expr) }) => { \
         @for @t { fn @concat(z_, @t)() -> @t { @v } } } }\n\
         defs!((i8, 0i8), (bool, false));",
    );
    assert!(d.is_empty(), "{d:?}");
    let names: Vec<&str> = p
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Fn(f) => Some(f.name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(names, ["z_i8", "z_bool"], "{names:?}");
}

#[test]
fn macro_json_int_shape_parses() {
    // The shape used by json/toml `convert.nabi` (ToJson/FromJson per int):
    // `let n` is hygienic; `@t::try_from`, `int_of::<@t>` resolve on @t.
    let (p, d) = parse_meta(
        "#[impl_macro(ji)] pub @macro { (@rep[sep = \",\"] { @t: ty }) => { \
         @for @t { \
         impl ToJson for @t { fn to_json(self) -> Value { Value::Int(Int::from(self)) } } \
         impl FromJson for @t { fn from_json(v: Value) throws (JsonError) -> @t { \
         let n = try int_of::<@t>(v); try @t::try_from(n) catch _ { throw oor::<@t>(n); } } } } } }\n\
         ji!(i8, u32);",
    );
    assert!(d.is_empty(), "{d:?}");
    // 2 types x 2 impls = 4 impl items.
    assert_eq!(
        p.items
            .iter()
            .filter(|i| matches!(i, Item::Impl(_)))
            .count(),
        4,
        "{:?}",
        p.items
    );
}

/// Parse `src` as package `package`, with the meta blocks of each
/// `(package, source)` registered under that package.
fn parse_in(
    package: &str,
    deps: &[(&str, &str)],
    src: &str,
) -> (nabic_ast::Program, Vec<nabic_diagnostic::NabiDiagnostic>) {
    let mut table = crate::MetaTable::default();
    for (pkg, dep) in deps {
        let (tokens, _) = Lexer::new(dep, *pkg).lex_all();
        for mut d in crate::collect_meta_blocks(&tokens, pkg) {
            d.set_package(pkg);
            assert!(table.add(d).is_none());
        }
    }
    let (tokens, _) = Lexer::new(src, "test").lex_all();
    crate::parse_program_meta(
        &tokens,
        "test",
        package,
        &crate::ExtOps::default(),
        &std::sync::Arc::new(table),
        &std::sync::Arc::default(),
        None,
    )
}

const LOG_A: &str = "#[impl_macro(log)] pub @macro { (@e: expr) => { a(@e) } }\n\
                     #[impl_derive(Tr)] pub @derive { impl Tr for @Self {} }\n\
                     #[impl_attribute(traced)] pub @attribute { @item fn traced() {} }";

const LOG_B: &str = "#[impl_macro(log)] pub @macro { (@e: expr) => { b(@e) } }";

/// The body of `main`, as debug text.
fn main_body(p: &nabic_ast::Program) -> String {
    let f = p
        .items
        .iter()
        .find_map(|i| match i {
            Item::Fn(f) if f.name == "main" => Some(f),
            _ => None,
        })
        .unwrap();
    format!("{:?}", f.body)
}

#[test]
fn qualified_macro_picks_its_package() {
    let deps = [("a", LOG_A), ("b", LOG_B)];
    for (call, want) in [("a::log!(1);", "\"a\""), ("b::log!(1);", "\"b\"")] {
        let (p, diags) = parse_in("", &deps, &format!("fn main() {{ {call} }}"));
        assert!(diags.is_empty(), "{diags:?}");
        assert!(main_body(&p).contains(want), "{call}");
    }
    // A bare name two packages define: this package's, else the `use`d one.
    let (p, diags) = parse_in("b", &deps, "fn main() { log!(1); }");
    assert!(diags.is_empty(), "{diags:?}");
    assert!(main_body(&p).contains("\"b\""));
    let (p, diags) = parse_in("", &deps, "use a::log!;\nfn main() { log!(1); }");
    assert!(diags.is_empty(), "{diags:?}");
    assert!(main_body(&p).contains("\"a\""));
    assert!(p.items.iter().all(|i| !matches!(i, Item::Use(_))));
    let (_, diags) = parse_in("", &deps, "fn main() { log!(1); }");
    assert!(diags[0].message.contains("does not reach"), "{diags:?}");
    let (_, diags) = parse_in(
        "",
        &deps,
        "use a::log!;\nuse b::log!;\nfn main() { log!(1); }",
    );
    assert!(diags[0].message.contains("ambiguous"), "{diags:?}");
    let (_, diags) = parse_in("", &deps, "fn main() { b::nope!(1); }");
    assert!(
        diags[0]
            .message
            .contains("package `b` defines no macro `nope`"),
        "{diags:?}"
    );
}

#[test]
fn bare_meta_name_reaches_its_scope() {
    let deps = [("a", LOG_A)];
    let src = "#[derive(Tr)] struct S {}\n#[traced] fn f() {}\n\
               fn main() { log!(1); log!(2); }";
    let (_, diags) = parse_in("", &deps, src);
    let got: Vec<(bool, String)> = diags
        .iter()
        .map(|d| (d.is_error(), d.notes.join(";")))
        .collect();
    assert_eq!(
        got,
        [
            (
                true,
                "help: write `#[derive(a::Tr)]`, or add `use a::Tr;`".to_owned()
            ),
            (
                true,
                "help: write `#[a::traced]`, or add `use a::traced;`".to_owned()
            ),
            (
                true,
                "help: write `a::log!`, or add `use a::log!;`".to_owned()
            ),
            (
                true,
                "help: write `a::log!`, or add `use a::log!;`".to_owned()
            ),
        ]
    );
    // Qualified, imported, its own package's, or the standard library's.
    for (pkg, src) in [
        (
            "",
            "#[derive(a::Tr)] struct S {}\n#[a::traced] fn f() {}\nfn main() { a::log!(1); }",
        ),
        (
            "",
            "use a::{log!, Tr};\n#[derive(Tr)] struct S {}\nfn main() { log!(1); }",
        ),
        ("", "use a::*;\n#[traced] fn f() {}\nfn main() { log!(1); }"),
        ("a", "fn main() { log!(1); }"),
    ] {
        let (_, diags) = parse_in(pkg, &deps, src);
        assert!(diags.is_empty(), "{src}: {diags:?}");
    }
    let (_, diags) = parse_in("", &[("core", LOG_A)], "fn main() { log!(1); }");
    assert!(diags.is_empty(), "{diags:?}");
}

#[test]
fn expansion_meta_names_resolve_at_definition() {
    // `a::outer!`'s bare `log!` is `a`'s, though the user package has one.
    let outer =
        format!("{LOG_A}\n#[impl_macro(outer)] pub @macro {{ (@e: expr) => {{ log!(@e) }} }}");
    let mine = "#[impl_macro(log)] pub @macro { (@e: expr) => { mine(@e) } }";
    let deps = [("a", outer.as_str()), ("", mine)];
    let (p, diags) = parse_in("", &deps, "fn main() { a::outer!(1); log!(2); }");
    assert!(diags.is_empty(), "{diags:?}");
    let body = main_body(&p);
    assert!(
        body.contains("\"a\"") && body.contains("\"mine\""),
        "{body}"
    );
    // Tokens a call passes in keep the use site's lookup.
    let (_, diags) = parse_in(
        "",
        &[("a", outer.as_str())],
        "fn main() { a::outer!(log!(1)); }",
    );
    assert!(diags[0].message.contains("does not reach"), "{diags:?}");
}

/// One path per library item: a `pub use` may only flatten a child
/// module into its parent (`collections::List` for
/// `collections::list::List`) or sit in the prelude.
#[test]
fn library_one_path_per_item() {
    // Not user paths: `collections::raw` is `pub(pkg)`.
    const KNOWN: [(&str, &str); 1] = [("core::collections::raw", "bare::mem")];
    fn walk(module: &str, items: &[Item], bad: &mut Vec<String>) {
        let children: Vec<&str> = items
            .iter()
            .filter_map(|i| match i {
                Item::Mod(m) => Some(m.name.as_str()),
                _ => None,
            })
            .collect();
        for item in items {
            match item {
                Item::Mod(m) => {
                    if let Some(inner) = &m.items {
                        walk(&format!("{module}::{}", m.name), inner, bad);
                    }
                }
                Item::Use(u) if u.vis == Visibility::Pub => {
                    let segs: Vec<&str> = u
                        .tree
                        .prefix
                        .segments
                        .iter()
                        .map(|s| s.name.as_str())
                        .collect();
                    let path = segs.join("::");
                    let rel = path.strip_prefix(&format!("{module}::")).unwrap_or(&path);
                    let first = rel.split("::").next().unwrap_or("");
                    let named = rel.contains("::") || matches!(u.tree.kind, UseTreeKind::Group(_));
                    let child = named && children.contains(&first);
                    let known = KNOWN
                        .iter()
                        .any(|(m, p)| *m == module && path.starts_with(p));
                    if !(child || known || module.ends_with("::prelude")) {
                        bad.push(format!("{module}: pub use {path}"));
                    }
                }
                _ => {}
            }
        }
    }
    fn files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                files(&p, out);
            } else if p.extension().is_some_and(|x| x == "nabi") {
                out.push(p);
            }
        }
    }
    let lib = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../library");
    let mut bad = Vec::new();
    let mut seen = 0;
    for tier in ["bare", "core", "std"] {
        let src = lib.join(tier).join("src");
        let mut all = Vec::new();
        files(&src, &mut all);
        for f in all {
            let rel = f.strip_prefix(&src).unwrap().with_extension("");
            let mut module = tier.to_owned();
            if rel != std::path::Path::new("lib") {
                for c in rel.components() {
                    module.push_str("::");
                    module.push_str(&c.as_os_str().to_string_lossy());
                }
            }
            let text = std::fs::read_to_string(&f).unwrap();
            let (tokens, _) = Lexer::new(&text, "t").lex_all();
            let (program, _) = parse_program(&tokens, "t");
            walk(&module, &program.items, &mut bad);
            seen += 1;
        }
    }
    assert!(seen > 50, "{seen}");
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}
