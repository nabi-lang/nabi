//! `nabic test` / `nabic bench` harness: the entry point is
//! replaced by a generated `main` running the `#[test]` named by
//! `argv[1]`; the runner spawns one process per test, so a panic (101) or
//! escaping error (1) fails only that test. `bench_main` times rounds of
//! calls instead.

use nabic_ast::{AttrArg, AttrBody, Attribute, Item, Literal, ThrowsKind};
use nabic_diagnostic::{Code, NabiDiagnostic};
use nabic_lexer::Lexer;
use nabic_parser::{ExtOps, parse_program_ext};

/// Whether `attrs` carries `#[test]`.
pub fn is_test(attrs: &[nabic_ast::Attribute]) -> bool {
    has_attr(attrs, "test")
}

/// Whether `attrs` carries `#[name]` (one path segment).
pub fn has_attr(attrs: &[nabic_ast::Attribute], name: &str) -> bool {
    find(attrs, name).is_some()
}

fn find<'a>(attrs: &'a [Attribute], name: &str) -> Option<&'a Attribute> {
    attrs
        .iter()
        .find(|a| a.path.len() == 1 && a.path[0] == name)
}

/// A `#[test]` and what the runner expects of it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TestCase {
    /// `::`-qualified function name.
    pub name: String,
    /// `#[ignore]`: run only on request; the reason, maybe empty.
    pub ignore: Option<String>,
    /// `#[should_panic]`: passes only on a panic whose message holds the
    /// text (empty: any panic).
    pub panics: Option<String>,
}

fn text(l: &Literal) -> Option<String> {
    match l {
        Literal::Str(s) | Literal::RawStr(s) => Some(s.clone()),
        _ => None,
    }
}

/// `#[should_panic]` -> `""`, `#[should_panic(expected = "t")]` -> `"t"`;
/// `None` for any other form.
pub fn panic_expectation(a: &Attribute) -> Option<String> {
    match &a.body {
        AttrBody::Empty => Some(String::new()),
        AttrBody::Args(xs) => match xs.as_slice() {
            [AttrArg::KeyValue(k, v)] if k == "expected" => text(v),
            _ => None,
        },
        AttrBody::Value(_) => None,
    }
}

/// `#[ignore]` -> `""`, `#[ignore = "why"]` / `#[ignore("why")]` -> `"why"`.
fn ignore_reason(a: &Attribute) -> String {
    match &a.body {
        AttrBody::Value(l) => text(l),
        AttrBody::Args(xs) => match xs.as_slice() {
            [AttrArg::Lit(l)] => text(l),
            _ => None,
        },
        AttrBody::Empty => None,
    }
    .unwrap_or_default()
}

/// Turn the user items into a test (`bench`: benchmark) program; the
/// `::`-qualified names in source order.
pub fn install(
    items: &mut Vec<(String, Item)>,
    ext: &ExtOps,
    diags: &mut Vec<NabiDiagnostic>,
    bench: bool,
) -> Vec<TestCase> {
    let attr = if bench { "bench" } else { "test" };
    let mut tests = Vec::new();
    for (module, item) in items.iter() {
        let Item::Fn(f) = item else { continue };
        if !has_attr(&f.attrs, attr) {
            let stray = find(&f.attrs, "should_panic").or_else(|| find(&f.attrs, "ignore"));
            if let Some(a) = stray.filter(|_| !bench && !has_attr(&f.attrs, "bench")) {
                diags.push(NabiDiagnostic::error(
                    Code::MISPLACED_ATTRIBUTE,
                    a.span,
                    format!(
                        "`#[{}]` marks a test: write `#[test]` above `fn {}` too",
                        a.path[0], f.name
                    ),
                ));
            }
            continue;
        }
        // A bench takes one `Bench` handle; a test takes nothing.
        let params_ok = if bench {
            f.params.len() == 1
        } else {
            f.params.is_empty()
        };
        if bench {
            if let Some(a) = find(&f.attrs, "should_panic").or_else(|| find(&f.attrs, "ignore")) {
                diags.push(NabiDiagnostic::error(
                    Code::MISPLACED_ATTRIBUTE,
                    a.span,
                    format!(
                        "`#[{}]` goes with `#[test]`, not `#[bench]`; remove it from `{}`",
                        a.path[0], f.name
                    ),
                ));
            }
        }
        if !params_ok || !f.generics.params.is_empty() {
            let want = if bench {
                "one `Bench` parameter and no generics"
            } else {
                "no parameters or generics"
            };
            diags.push(NabiDiagnostic::error(
                Code::INVALID_SIGNATURE,
                f.span,
                format!("`#[{attr}] fn {}` must take {want}", f.name),
            ));
            continue;
        }
        let name = if module.is_empty() {
            f.name.clone()
        } else {
            format!("{module}::{}", f.name)
        };
        let case = TestCase {
            name,
            ignore: find(&f.attrs, "ignore").map(ignore_reason),
            panics: find(&f.attrs, "should_panic").and_then(panic_expectation),
        };
        tests.push((case, !matches!(f.throws, ThrowsKind::None)));
    }
    // The package's own entry point is replaced (top-level statements
    // were already dropped by the caller).
    items.retain(|(m, i)| !(m.is_empty() && matches!(i, Item::Fn(f) if f.name == "main")));

    let names: Vec<(String, bool)> = tests.iter().map(|(c, t)| (c.name.clone(), *t)).collect();
    let src = if bench {
        bench_main(&names)
    } else {
        test_main(&names)
    };
    let (tokens, mut d) = Lexer::new(&src, "<test harness>").lex_all();
    diags.append(&mut d);
    let (program, mut d) = parse_program_ext(&tokens, "<test harness>", ext);
    diags.append(&mut d);
    items.extend(program.items.into_iter().map(|i| (String::new(), i)));
    tests.into_iter().map(|(c, _)| c).collect()
}

/// `main` running the test named by `argv[1]` once.
fn test_main(tests: &[(String, bool)]) -> String {
    let throws = tests.iter().any(|(_, t)| *t);
    let mut src = String::from("fn main() ");
    if throws {
        src.push_str("throws ");
    }
    src.push_str("{\n    let name = ::std::env::args()[1];\n");
    for (name, throws) in tests {
        let call = if *throws { "try " } else { "" };
        src.push_str(&format!(
            "    if name == \"{name}\" {{ {call}{name}(); return; }}\n"
        ));
    }
    src.push_str("    panic(\"nabic test: unknown test name\");\n}\n");
    src
}

/// `nabic bench`'s `main`, `argv` = `name iters samples tty width` (`tty`
/// 1: the human line, 0: the `#nabibench` one): builds the `Bench` handle
/// and hands it to the named benchmark, which measures and reports itself
/// (`core::bench`). Exit 3: it never called `b.iter` (the runner
/// says so).
fn bench_main(benches: &[(String, bool)]) -> String {
    let mut src = String::from(if benches.is_empty() {
        "fn main() {\n"
    } else {
        "fn main() -> i32 {\n"
    });
    src.push_str(
        "\x20   let argv = std::env::args();\n\
         \x20   let name = argv[1];\n\
         \x20   let iters: i64 = try! argv[2].parse();\n\
         \x20   let samples: i64 = try! argv[3].parse();\n\
         \x20   let tty = argv[4] == \"1\";\n\
         \x20   let width: usize = try! argv[5].parse();\n\
         \x20   let b = core::bench::Bench::new(name, iters, samples, tty, width);\n",
    );
    for (k, (name, throws)) in benches.iter().enumerate() {
        let call = if *throws { "try! " } else { "" };
        let kw = if k == 0 { "if" } else { "else if" };
        src.push_str(&format!(
            "    {kw} name == \"{name}\" {{ {call}{name}(b); }}\n"
        ));
    }
    if !benches.is_empty() {
        src.push_str("    else ");
    }
    src.push_str("{ panic(\"nabic bench: unknown bench name\"); }\n");
    if !benches.is_empty() {
        src.push_str("    if ::core::bench::measured() { 0 } else { 3 }\n");
    }
    src.push_str("}\n");
    src
}
