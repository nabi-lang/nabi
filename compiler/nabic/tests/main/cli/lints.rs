//! Lints in builds: levels, notes and fixes.

#[allow(clippy::wildcard_imports)]
use crate::cli::*;

/// `build` / `test` lint as `check` does: warnings do not fail the
/// build, `-D` and `#[deny]` do, `-A` and `#[allow]` silence them, and a
/// fresh build prints them again.
#[test]
fn builds_print_lints() {
    let env = setup();
    let dir = scratch("build-lints");
    let app = new_package(&dir, "app", false);
    let src = "fn main() {\n    let x = 1;\n    println(\"hi\");\n}\n";
    put(app.join("src/main.nabi"), src).unwrap();
    let warn = "warning[E0131]: unused variable `x`";
    let out = build_run(Some(&env), &app, &[]);
    assert_exit(&out, 0);
    assert_eq!(text(&out.stdout), "hi\n");
    assert!(text(&out.stderr).contains(warn), "{}", text(&out.stderr));
    let out = build_run(Some(&env), &app, &[]);
    let err = text(&out.stderr);
    assert!(err.contains("Fresh app") && err.contains(warn), "{err}");
    let out = nabic(Some(&env), &app, &["-D", "unused", "build"]);
    assert_exit(&out, 1);
    let err = text(&out.stderr);
    assert!(
        err.contains("error[E0131]: unused variable `x`")
            && err.contains("`-D unused` on the command line"),
        "{err}"
    );
    let out = nabic(Some(&env), &app, &["-A", "unused_variables", "build"]);
    assert_exit(&out, 0);
    let err = text(&out.stderr);
    assert!(
        err.contains("Compiling app") && !err.contains("E0131"),
        "{err}"
    );
    put(
        app.join("src/main.nabi"),
        format!("#![allow(unused_variables)]\n{src}"),
    )
    .unwrap();
    let out = nabic(Some(&env), &app, &["build"]);
    assert!(
        !text(&out.stderr).contains("E0131"),
        "{}",
        text(&out.stderr)
    );
    put(
        app.join("src/main.nabi"),
        format!("#![deny(unused_variables)]\n{src}"),
    )
    .unwrap();
    assert_exit(&nabic(Some(&env), &app, &["build"]), 1);
    // A test build.
    put(
        app.join("src/main.nabi"),
        "fn main() {}\n\n#[test]\nfn t() {\n    let y = 2;\n}\n",
    )
    .unwrap();
    let (code, _, err) = harness(&env, &app, "test", &[]);
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("warning[E0131]: unused variable `y`"), "{err}");
    // A lone file.
    put(dir.join("s.nabi"), src).unwrap();
    let run = || nabic(Some(&env), &dir, &["--target-dir", "t", "build", "s.nabi"]);
    let first = text(&run().stderr);
    let second = text(&run().stderr);
    assert!(
        first.contains("Compiling s") && first.contains(warn),
        "{first}"
    );
    assert!(
        second.contains("Fresh s") && second.contains(warn),
        "{second}"
    );
}

/// `warnings` holds every warning-level lint, later ones too, as
/// a flag and as an attribute; the lint list shows each lint's release.
#[test]
fn deny_warnings_takes_new_lints() {
    let dir = scratch("lint-warnings");
    let app = new_package(&dir, "app", false);
    let src = "fn main() {\n    let x = 1;\n}\n";
    put(app.join("src/main.nabi"), src).unwrap();
    let out = nabic(None, &app, &["check", "-D", "warnings"]);
    assert_exit(&out, 1);
    let err = text(&out.stderr);
    assert!(
        err.contains("error[E0131]: unused variable `x`")
            && err.contains("`-D warnings` on the command line"),
        "{err}"
    );
    let out = nabic(None, &app, &["check", "-D", "warnings", "-A", "unused"]);
    assert_exit(&out, 0);
    put(
        app.join("src/main.nabi"),
        format!("#![deny(warnings)]\n{src}"),
    )
    .unwrap();
    assert_exit(&nabic(None, &app, &["check"]), 1);
    let list = nabic_tools::lint::list();
    assert!(
        list.contains("warnings (every warning-level lint)")
            && list.contains("unused_variables         warn  0.1.0"),
        "{list}"
    );
    let out = nabic(None, &app, &["check", "-W", "unused_varibles"]);
    let err = text(&out.stderr);
    assert!(
        err.contains(
            "unknown lint `unused_varibles` on the command line; did you mean \
             `unused_variables`? (`nabi lint --list` shows every lint and group)"
        ),
        "{err}"
    );
    // A deprecation of any release is denied under `warnings`.
    put(
        app.join("src/main.nabi"),
        "#[deprecated(since = \"1.2.0\")]\nfn new_old() {}\n\
         #[deprecated(since = \"1.1.0\")]\nfn old() {}\n\
         fn main() {\n    new_old();\n    old();\n}\n",
    )
    .unwrap();
    for deny in ["warnings", "all"] {
        let out = nabic(None, &app, &["check", "-D", deny]);
        assert_exit(&out, 1);
        let err = text(&out.stderr);
        assert!(
            err.contains("error[E0131]: use of deprecated function `new_old`")
                && err.contains("error[E0131]: use of deprecated function `old`"),
            "{err}"
        );
    }
    let list = nabic_tools::lint::list();
    assert!(
        list.contains("deprecated_safe          warn  0.1.0"),
        "{list}"
    );
}

/// `nabi` names the manifest's `lints` as where a level came from;
/// the command line's own flags after them still say so.
#[test]
fn manifest_lint_note_names_nabi_nabion() {
    let dir = scratch("lint-manifest");
    let app = new_package(&dir, "app", false);
    put(
        app.join("src/main.nabi"),
        "fn BadName() {}\nfn main() {\n    let x = 1;\n    BadName();\n}\n",
    )
    .unwrap();
    let manifest = app.join("nabi.nabion");
    put(&manifest, "Package { name: \"app\" }\n").unwrap();
    let from = format!("--lints-from-manifest={}", manifest.display());
    let args = [
        "check",
        "-D",
        "unused_variables",
        "-W",
        "nope",
        &from,
        "-D",
        "non_snake_case",
    ];
    let out = nabic(None, &app, &args);
    assert_exit(&out, 1);
    let err = text(&out.stderr);
    for want in [
        "`\"unused_variables\": \"deny\"` in `lints` of nabi.nabion",
        "unknown lint `nope` in `lints` of nabi.nabion",
        "`-D non_snake_case` on the command line",
    ] {
        assert!(err.contains(want), "missing {want:?}:\n{err}");
    }
}

/// An unknown lint name in `#[allow]`, `-A` and `nabi.nabion` `lints`
/// is reported in each place.
#[test]
fn unknown_lint_names_are_reported() {
    let dir = scratch("lint-unknown");
    let app = new_package(&dir, "app", false);
    let manifest = app.join("nabi.nabion");
    put(&manifest, "Package { name: \"app\" }\n").unwrap();
    let from = format!("--lints-from-manifest={}", manifest.display());
    put(app.join("src/main.nabi"), "#[allow(nope)]\nfn main() {}\n").unwrap();
    let args = ["check", "-A", "nope", &from, "-A", "nope", "-D", "warnings"];
    let err = text(&nabic(None, &app, &args).stderr);
    for want in [
        "unknown lint `nope` in `lints` of nabi.nabion",
        "unknown lint `nope` on the command line",
        "unknown lint `nope` (",
    ] {
        assert!(err.contains(want), "missing {want:?}:\n{err}");
    }
}

/// A compiler warning that belongs to a lint takes the lint's
/// level: `-A`, `#[allow]` and `-D` reach it.
#[test]
fn compiler_warning_with_lint_can_be_allowed() {
    let dir = scratch("lint-compiler-warning");
    let app = new_package(&dir, "app", false);
    let src = "fn main() {\n    /// not an item\n    let x = 1;\n    println(f\"{x}\");\n}\n";
    put(app.join("src/main.nabi"), src).unwrap();
    let warn = "warning[E0131]: unused doc comment";
    let out = nabic(None, &app, &["check"]);
    assert_exit(&out, 0);
    let err = text(&out.stderr);
    assert!(
        err.contains(warn) && err.contains("`#[warn(unused_doc_comments)]` on by default"),
        "{err}"
    );
    let out = nabic(None, &app, &["check", "-A", "unused_doc_comments"]);
    assert!(
        !text(&out.stderr).contains("doc comment"),
        "{}",
        text(&out.stderr)
    );
    let out = nabic(None, &app, &["check", "-D", "unused"]);
    assert_exit(&out, 1);
    assert!(
        text(&out.stderr).contains("error[E0131]: unused doc comment"),
        "{}",
        text(&out.stderr)
    );
    put(
        app.join("src/main.nabi"),
        format!("#![allow(unused_doc_comments)]\n{src}"),
    )
    .unwrap();
    let out = nabic(None, &app, &["check"]);
    assert!(
        !text(&out.stderr).contains("doc comment"),
        "{}",
        text(&out.stderr)
    );
    // A confusable pair, from the lexer.
    put(
        app.join("src/main.nabi"),
        "fn main() {\n    let σ² = 1;\n    let σ2 = 2;\n    println(f\"{σ²} {σ2}\");\n}\n",
    )
    .unwrap();
    let out = nabic(None, &app, &["check"]);
    assert!(
        text(&out.stderr).contains("(confusable spelling)"),
        "{}",
        text(&out.stderr)
    );
    let out = nabic(None, &app, &["check", "-A", "confusable_idents"]);
    assert!(
        !text(&out.stderr).contains("confusable"),
        "{}",
        text(&out.stderr)
    );
}

/// `-D warnings`, `-D all` and its own name deny a newer lint.
#[test]
fn literal_spread_mismatch_is_in_warnings() {
    let dir = scratch("lint-spread");
    let app = new_package(&dir, "app", false);
    let src = "fn add(a: i32, b: i32) -> i32 { a + b }\n\
               fn main() {\n    println(f\"{add(..[1, 2, 3])}\");\n}\n";
    put(app.join("src/main.nabi"), src).unwrap();
    let warn = "warning[E0131]: spread of 3 elements into 2 parameters; this call panics";
    let out = nabic(None, &app, &["check"]);
    assert_exit(&out, 0);
    assert!(text(&out.stderr).contains(warn), "{}", text(&out.stderr));
    for deny in ["warnings", "all", "literal_spread_mismatch"] {
        let out = nabic(None, &app, &["check", "-D", deny]);
        assert_exit(&out, 1);
    }
    let out = nabic(None, &app, &["check", "-A", "literal_spread_mismatch"]);
    assert!(
        !text(&out.stderr).contains("spread"),
        "{}",
        text(&out.stderr)
    );
    let list = nabic_tools::lint::list();
    assert!(
        list.contains("literal_spread_mismatch  warn  0.1.0"),
        "{list}"
    );
}

/// The errors for a cast to `bool` and a pattern that misspells a
/// variant carry the edit their help asks for.
#[test]
fn flipped_lint_errors_carry_fixes() {
    let env = setup();
    let dir = scratch("diag-flip-fix");
    put(
        dir.join("bad.nabi"),
        "enum Color { Red, Green }\n\
         let n = 3;\nlet a = n as bool;\nlet b = (n + 1) as bool;\nlet c = 0.5 as bool;\n\
         match Color::Green { Color::Red => println(\"r\"), Gren => println(\"g\") };\n",
    )
    .unwrap();
    let out = nabic(
        Some(&env),
        &dir,
        &["check", "--message-format=json", "bad.nabi"],
    );
    assert_exit(&out, 1);
    let stdout = text(&out.stdout);
    for fix in [
        r#""text":"n != 0","title":"write `n != 0`""#,
        r#""text":"(n + 1) != 0","title":"write `(n + 1) != 0`""#,
        r#""text":"0.5 != 0.0","title":"write `0.5 != 0.0`""#,
        r#""text":"Color::Green","title":"write `Color::Green`""#,
    ] {
        assert!(stdout.contains(fix), "{fix}\n{stdout}");
    }
}

/// A `pub` fn without `->` whose body gives a value: the fix writes the
/// inferred type before the body, after a `throws` clause.
#[test]
fn pub_fn_nonunit_body_fix() {
    let env = setup();
    let dir = scratch("pub-fn-ret-fix");
    put(
        dir.join("bad.nabi"),
        "pub fn double(x: i32) { x * 2 }\n\
         pub fn half(x: i32) throws (str) { if x < 0 { throw \"neg\"; } x / 2 }\n",
    )
    .unwrap();
    let out = nabic(
        Some(&env),
        &dir,
        &["check", "--message-format=json", "bad.nabi"],
    );
    assert_exit(&out, 1);
    let stdout = text(&out.stdout);
    for fix in [
        r#""line_start":1,"col_start":23,"byte_start":22,"byte_end":22,"text":"-> i32 ","title":"add `-> i32`""#,
        r#""line_start":2,"col_start":34,"byte_start":65,"byte_end":65,"text":"-> i32 ","title":"add `-> i32`""#,
    ] {
        assert!(stdout.contains(fix), "{fix}\n{stdout}");
    }
}
