//! Diagnostics: JSON, colors, explanations and summaries.

#[allow(clippy::wildcard_imports)]
use crate::cli::*;

#[test]
fn json_and_colored_diagnostics() {
    let env = setup();
    let dir = scratch("diag");
    put(dir.join("bad.nabi"), "let x: i32 = nope;\n").unwrap();
    let out = nabic(
        Some(&env),
        &dir,
        &["build", "--message-format=json", "bad.nabi"],
    );
    assert_exit(&out, 1);
    let stdout = text(&out.stdout);
    let line = stdout.lines().next().unwrap_or_default();
    assert!(
        line.starts_with("{\"type\":\"error\",\"code\":\"E"),
        "{stdout}"
    );
    assert!(line.contains("\"file_name\":\"bad.nabi\""), "{line}");
    assert!(line.contains("\"line_start\":1"), "{line}");

    // Forced colour at each depth; none when piped, with NO_COLOR, with
    // TERM=dumb or with --color=never.
    let stderr = |vars: &[(&str, &str)], args: &[&str]| {
        let mut c = Command::new(NABIC);
        c.current_dir(&dir)
            .env("NABI_HOME", workspace())
            .env_remove("NO_COLOR")
            .env_remove("CLICOLOR_FORCE")
            .env_remove("COLORTERM")
            .env("TERM", "xterm")
            .envs(vars.iter().copied())
            .args(args)
            .args(["build", "bad.nabi", "--nabir"])
            .arg(&env.nabir);
        text(&c.output().unwrap().stderr)
    };
    let always = ["--color=always"];
    let err = stderr(&[], &always);
    assert!(err.contains("\x1b[1;91merror[E0027]\x1b[0m"), "{err:?}");
    assert!(err.contains("\x1b[1;96m-->\x1b[0m"), "{err:?}");
    let err = stderr(&[("TERM", "xterm-256color")], &always);
    assert!(
        err.contains("\x1b[1;38;5;203merror[E0027]\x1b[0m"),
        "{err:?}"
    );
    let err = stderr(&[("COLORTERM", "truecolor")], &always);
    assert!(
        err.contains("\x1b[1;38;2;255;95;95merror[E0027]\x1b[0m"),
        "{err:?}"
    );
    let err = stderr(&[("CLICOLOR_FORCE", "1")], &[]);
    assert!(err.contains("\x1b[1;91merror[E0027]\x1b[0m"), "{err:?}");
    for (vars, args) in [
        (&[][..], &[][..]),
        (&[("NO_COLOR", "1"), ("CLICOLOR_FORCE", "1")][..], &[][..]),
        (&[("TERM", "dumb")][..], &[][..]),
        (&[("CLICOLOR_FORCE", "1")][..], &["--color=never"][..]),
        (&[("CLICOLOR_FORCE", "1")][..], &["--no-color"][..]),
    ] {
        let err = stderr(vars, args);
        assert!(err.contains("error[E0027]"), "{err:?}");
        assert!(!err.contains('\x1b'), "{vars:?} {args:?}: {err:?}");
    }
}

#[test]
fn fixes_in_json() {
    let env = setup();
    let dir = scratch("diag-fix");
    put(
        dir.join("bad.nabi"),
        "let xs: List<i32> = [1];\nlet n = xs.length();\n",
    )
    .unwrap();
    let out = nabic(
        Some(&env),
        &dir,
        &["check", "--message-format=json", "bad.nabi"],
    );
    assert_exit(&out, 1);
    let stdout = text(&out.stdout);
    let line = stdout
        .lines()
        .find(|l| l.contains("no method `length`"))
        .unwrap_or_default();
    // `length` at line 2, col 12, bytes 36..42.
    assert!(
        line.contains(
            r#""fixes":[{"file_name":"bad.nabi","line_start":2,"col_start":12,"byte_start":36,"byte_end":42,"text":"len","title":"use `len`","line_end":2,"col_end":18}]"#
        ),
        "{stdout}"
    );
    assert!(line.ends_with(r#""lint":null,"schema":1}"#), "{line}");
}

/// `check` instantiates what the program reaches, so a failing
/// `const { .. }` check of an instance is its error too, not only
/// `build`'s.
#[test]
fn check_reports_const_block() {
    let env = setup();
    let dir = scratch("diag-const-block");
    put(
        dir.join("c.nabi"),
        "fn chunk<const N: usize>(xs: [i32; N]) -> usize {\n    \
         const { assert(N > 0, \"chunk needs one\") };\n    N\n}\n\
         let e: [i32; 0] = [];\nprintln(f\"{chunk(e)}\");\n",
    )
    .unwrap();
    let out = nabic(Some(&env), &dir, &["check", "c.nabi"]);
    assert_exit(&out, 1);
    let stderr = text(&out.stderr);
    assert!(
        stderr.contains("const block failed in `chunk::<0>`: chunk needs one"),
        "{stderr}"
    );
}

/// JSON diagnostics name the lint a warning belongs to, where its span
/// ends, and its help on its own.
#[test]
fn json_names_lint_and_end() {
    let env = setup();
    let dir = scratch("diag-lint");
    put(dir.join("w.nabi"), "fn main() {\n    let unused = 1;\n}\n").unwrap();
    let out = nabic(
        Some(&env),
        &dir,
        &["check", "--message-format=json", "w.nabi"],
    );
    assert_exit(&out, 0);
    let stdout = text(&out.stdout);
    let line = stdout
        .lines()
        .find(|l| l.contains("unused_variables"))
        .unwrap_or_else(|| panic!("{stdout}"));
    let d = nabic_diagnostic::Json::parse(line).unwrap();
    assert_eq!(d.get("schema").as_u32(), Some(1), "{line}");
    assert_eq!(d.get("type").as_str(), Some("warning"), "{line}");
    assert_eq!(d.get("lint").as_str(), Some("unused_variables"), "{line}");
    let nabic_diagnostic::Json::Arr(spans) = d.get("spans") else {
        panic!("{line}")
    };
    let primary = &spans[0];
    assert_eq!(primary.get("line_start").as_u32(), Some(2), "{line}");
    assert_eq!(primary.get("line_end").as_u32(), Some(2), "{line}");
    let (s, e) = (primary.get("col_start"), primary.get("col_end"));
    assert!(e.as_u32() > s.as_u32(), "{line}");
    assert!(
        matches!(d.get("help"), nabic_diagnostic::Json::Arr(_)),
        "{line}"
    );
}

/// `--explain` prints a code's page; an unknown code is a usage error
/// that says which codes exist.
#[test]
fn explain_prints_the_code() {
    let dir = scratch("explain");
    let out = nabic(None, &dir, &["--explain", "E0040"]);
    assert_exit(&out, 0);
    let page = text(&out.stdout);
    assert!(page.starts_with("E0040: mismatched types\n\n"), "{page}");
    let out = nabic(None, &dir, &["--explain", "E200"]);
    assert_exit(&out, 2);
    assert!(
        text(&out.stderr).contains("codes run from E0001"),
        "{}",
        text(&out.stderr)
    );
}

/// An internal compiler error exits 70, apart from a program panic's 101,
/// and says where to report it with what a report needs.
#[test]
fn ice_exit_70() {
    if !cfg!(debug_assertions) {
        return;
    }
    let dir = scratch("ice-exit");
    put(dir.join("ice.nabi"), "let x = 1; // ice-here\n").unwrap();
    let out = Command::new(NABIC)
        .current_dir(&dir)
        .env("NABI_HOME", workspace())
        .env("NABI_TEST_ICE", "ice-here")
        .args(["check", "ice.nabi"])
        .output()
        .unwrap();
    assert_exit(&out, 70);
    let err = text(&out.stderr);
    let Some((_, banner)) = err.split_once("nabic: internal compiler error\n") else {
        panic!("{err}");
    };
    let lines: Vec<&str> = banner.lines().collect();
    // The pass, when the panic came from inside one.
    assert!(
        lines.len() == 3 || (lines.len() == 4 && lines[3].starts_with("note: in pass: ")),
        "{err}"
    );
    assert!(
        lines[0].starts_with("note: this is a bug in nabi")
            && lines[0].ends_with(nabic_core::ISSUES),
        "{err}"
    );
    let version = format!("note: nabic {} ", env!("CARGO_PKG_VERSION"));
    assert!(
        lines[1].starts_with(&version) && lines[1].contains(" on "),
        "{err}"
    );
    assert_eq!(lines[2], "note: command: nabic check ice.nabi", "{err}");
}

/// A failed build ends with what failed and the counts, as `check` does;
/// `-q` and JSON output leave the line out.
#[test]
fn build_error_ends_with_summary() {
    let dir = scratch("build-summary");
    let app = new_package(&dir, "app", false);
    put(
        app.join("src/main.nabi"),
        "fn main() {\n    println(y);\n}\n",
    )
    .unwrap();
    let out = nabic(None, &app, &["build"]);
    assert_exit(&out, 1);
    let err = text(&out.stderr);
    assert!(
        err.ends_with("\nerror: could not build app (1 error, 0 warnings)\n"),
        "{err}"
    );
    for quiet in [&["-q"][..], &["--message-format=json"][..]] {
        let out = nabic(None, &app, &[&["build"][..], quiet].concat());
        assert_exit(&out, 1);
        assert!(!text(&out.stderr).contains("could not build"), "{quiet:?}");
    }
    // Denied lints are the errors; the others the warnings.
    put(
        app.join("src/main.nabi"),
        "fn main() {\n    let a = 1;\n    let b = 2;\n}\n",
    )
    .unwrap();
    let out = nabic(None, &app, &["build", "-D", "unused_variables"]);
    assert_exit(&out, 1);
    let err = text(&out.stderr);
    assert!(
        err.ends_with("\nerror: could not build app (2 errors, 0 warnings)\n"),
        "{err}"
    );
}

/// An up-to-date build says `Fresh`, then repeats its warnings.
#[test]
fn fresh_build_prints_warnings_after_status() {
    let env = setup();
    let dir = scratch("fresh-warnings");
    let app = new_package(&dir, "app", false);
    put(
        app.join("src/main.nabi"),
        "fn main() {\n    let a = 1;\n}\n",
    )
    .unwrap();
    assert_exit(&nabic(Some(&env), &app, &["build"]), 0);
    let out = nabic(Some(&env), &app, &["build"]);
    assert_exit(&out, 0);
    let err = text(&out.stderr);
    let fresh = err.find("       Fresh app").expect(&err);
    let warning = err.find("warning[").expect(&err);
    assert!(fresh < warning, "{err}");
}

/// A message has no space before its end or before a `;`.
#[test]
fn messages_have_no_stray_spaces() {
    let env = setup();
    let dir = scratch("diag-spaces");
    let cases = [
        "let m = Mutex::new(0);\nlet g = m.lock();\n",
        "let m = Mutex::new([1, 2, 3]);\nlet xs = using g = m.lock() { g.get() };\nxs.push(4);\n",
        "unsafe fn danger() -> i32 { 1 }\nprintln(f\"{danger()}\");\n",
        "pub fn double(x: i32) { x * 2 }\n\nprintln(f\"{double(4)}\");\n",
        "static N: i32 = 0;\nN = 3;\n",
    ];
    for (i, src) in cases.iter().enumerate() {
        let file = format!("s{i}.nabi");
        put(dir.join(&file), src).unwrap();
        let out = nabic(Some(&env), &dir, &["check", &file]);
        assert_exit(&out, 1);
        let err = text(&out.stderr);
        for l in err
            .lines()
            .filter(|l| l.starts_with("error") || l.contains("= help"))
        {
            assert!(!l.ends_with(' ') && !l.contains(" ;"), "{src}: `{l}`");
        }
    }
}

/// An option value it does not take is E0132, with what to write and
/// exit code 2; as JSON wherever `--message-format` stands.
#[test]
fn invalid_option_is_e0132() {
    let env = setup();
    let dir = scratch("diag-option");
    put(dir.join("ok.nabi"), "println(\"x\");\n").unwrap();
    let out = nabic(Some(&env), &dir, &["build", "-C", "icff", "ok.nabi"]);
    assert_exit(&out, 2);
    let err = text(&out.stderr);
    assert!(
        err.starts_with("error[E0132]: unknown codegen option `icff`; did you mean `icf`?\n"),
        "{err}"
    );
    assert!(err.contains("= help: `-C` takes `overflow-checks"), "{err}");
    assert!(err.ends_with("run `nabic help build` for usage\n"), "{err}");
    let out = nabic(
        Some(&env),
        &dir,
        &["build", "--emit=mirr", "ok.nabi", "--message-format=json"],
    );
    assert_exit(&out, 2);
    let line = text(&out.stdout);
    assert!(
        line.starts_with("{\"type\":\"error\",\"code\":\"E0132\""),
        "{line}"
    );
    assert!(line.contains("did you mean `mir`?"), "{line}");
}
