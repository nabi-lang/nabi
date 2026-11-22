//! The command line: help, options, colours and output.

#[allow(clippy::wildcard_imports)]
use crate::*;

/// The commands `nabi --help` lists.
pub(crate) fn listed_commands(home: &Path, cwd: &Path) -> Vec<String> {
    let list = ok(&nabi(home, cwd, &["--help"]));
    let mut out = Vec::new();
    for l in list.lines().skip_while(|l| !l.starts_with("packages:")) {
        if l.starts_with("options") {
            break;
        }
        if let Some(name) = l.strip_prefix("  ").and_then(|r| r.split(' ').next()) {
            out.push(name.to_owned());
        }
    }
    out
}

/// `nabi help <cmd>` is that command's text alone: its usage, examples
/// and options, written as `nabi`.
#[test]
fn every_command_help_is_its_own() {
    let root = scratch("help-own");
    let home = root.join("home");
    let cmds = listed_commands(&home, &root);
    assert!(
        cmds.len() > 30 && cmds.contains(&"toolchain".to_owned()),
        "{cmds:?}"
    );
    for c in &cmds {
        let help = ok(&nabi(&home, &root, &["help", c]));
        assert!(
            help.starts_with(&format!("usage: nabi {c}")),
            "{c}:\n{help}"
        );
        assert!(help.contains("\nexamples:\n"), "{c}:\n{help}");
        assert!(
            !help.contains("nabic ") && !help.contains("nabic`"),
            "{c}:\n{help}"
        );
        for other in cmds.iter().filter(|o| *o != c) {
            for l in help.lines() {
                let form = l.strip_prefix("usage: ").unwrap_or(l).trim_start();
                assert!(
                    !form.starts_with(&format!("nabi {other} ")) || l.starts_with("  nabi"),
                    "{c} holds {other}'s usage:\n{help}"
                );
            }
        }
        assert_eq!(ok(&nabi(&home, &root, &[c, "--help"])), help);
    }
}

/// The rows of a help table start their text in one column, and the
/// lines that go on a row's text start there too.
#[test]
fn help_tables_align() {
    let root = scratch("help-align");
    let home = root.join("home");
    let nabic = Path::new(NABI).with_file_name(exe("nabic"));
    let mut pages = vec![
        (
            "nabi --help".to_owned(),
            ok(&nabi(&home, &root, &["--help"])),
        ),
        (
            "nabic --help".to_owned(),
            text(&Command::new(&nabic).arg("--help").output().unwrap().stdout),
        ),
    ];
    for c in listed_commands(&home, &root) {
        pages.push((
            format!("nabi help {c}"),
            ok(&nabi(&home, &root, &["help", &c])),
        ));
    }
    for (name, page) in &pages {
        for block in page.split("\n\n") {
            let rows: Vec<&str> = block.lines().filter(|l| l.starts_with(' ')).collect();
            // A row: indent, its head, two or more spaces, its text.
            let mut cols = std::collections::BTreeSet::new();
            for r in &rows {
                let head = r.trim_start();
                let indent = r.len() - head.len();
                if let Some(gap) = head.find("  ") {
                    let rest = &head[gap..];
                    let text = rest.trim_start();
                    if !text.is_empty() && indent <= 7 {
                        cols.insert(indent + gap + rest.len() - text.len());
                    }
                } else if indent > 7 {
                    cols.insert(indent);
                }
            }
            assert!(cols.len() <= 1, "{name}: columns {cols:?} in\n{block}");
        }
    }
}

#[test]
fn nabi_help_is_short() {
    let root = scratch("help-short");
    let home = root.join("home");
    let help = ok(&nabi(&home, &root, &["--help"]));
    assert!(help.lines().count() < 66, "{help}");
    assert_eq!(ok(&nabi(&home, &root, &["help"])), help);
    assert!(help.contains("\n  build ") && help.contains("nabi help <command>"));
}

/// nabi's global options go anywhere before `--`, after the command too.
#[test]
fn global_options_after_command() {
    let root = scratch("global-after");
    let home = root.join("home");
    let out = nabi(&home, &root, &["new", "x", "-q"]);
    ok(&out);
    assert_eq!(text(&out.stderr), "");
    assert!(root.join("x/src/main.nabi").is_file());
    fs::create_dir_all(root.join("y")).unwrap();
    let out = nabi(&home, &root.join("y"), &["init", "-v"]);
    ok(&out);
    assert!(
        text(&out.stderr).contains("Created"),
        "{}",
        text(&out.stderr)
    );
    ok(&nabi(&home, &root, &["toolchain", "list", "--quiet"]));
}

/// An unknown command or option names the closest one and the help to
/// read.
#[test]
fn unknown_command_suggests() {
    let root = scratch("unknown-cmd");
    let home = root.join("home");
    for (typo, want) in [
        ("biuld", "build"),
        ("instal", "install"),
        ("compile", "build"),
        ("ls", "toolchain list"),
    ] {
        let out = nabi(&home, &root, &[typo, "x"]);
        assert_eq!(out.status.code(), Some(2));
        let err = text(&out.stderr);
        assert_eq!(
            err,
            format!(
                "nabi: unknown command `{typo}`; did you mean `{want}`?\nrun `nabi help` for usage\n"
            )
        );
    }
    let out = nabi(&home, &root, &["help", "biuld"]);
    assert_eq!(out.status.code(), Some(2));
    fails(&out, "did you mean `build`?");
    let out = nabi(&home, &root, &["new", "z", "--lbi"]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(
        text(&out.stderr),
        "nabi: unknown option `--lbi`; did you mean `--lib`?\nrun `nabi help new` for usage\n"
    );
    let out = nabi(&home, &root, &["add", "x", "--regstry", "u"]);
    fails(&out, "did you mean `--registry`?");
}

/// nabic's usage errors under nabi speak of nabi.
#[test]
fn nabic_option_reported_as_nabi() {
    let root = scratch("nabic-as-nabi");
    let home = root.join("home");
    ok(&nabi(&home, &root, &["new", "app"]));
    let app = root.join("app");
    let out = nabi(&home, &app, &["build", "--relase"]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(
        text(&out.stderr),
        "nabi: unknown option `--relase`; did you mean `--release`?\nrun `nabi help build` for usage\n"
    );
    let out = nabi(&home, &app, &["check", ".", "extra"]);
    assert_eq!(out.status.code(), Some(2));
    fails(
        &out,
        "nabi: unexpected argument `extra`: `check` takes one path",
    );
}

/// `nabi args` in `cwd` with `TERM=xterm` (16 colours) and `vars`; its
/// stderr.
pub(crate) fn stderr_with(home: &Path, cwd: &Path, vars: &[(&str, &str)], args: &[&str]) -> String {
    let out = nabi_cmd(home, cwd, args)
        .env_remove("NO_COLOR")
        .env_remove("COLORTERM")
        .env("TERM", "xterm")
        .envs(vars.iter().copied())
        .output()
        .unwrap();
    text(&out.stderr)
}

#[test]
fn status_lines_coloured_when_forced() {
    let root = scratch("colour-forced");
    let home = root.join("home");
    let err = stderr_with(&home, &root, &[("CLICOLOR_FORCE", "1")], &["new", "a"]);
    assert!(err.starts_with("\x1b[1;96m     Created\x1b[0m "), "{err:?}");
    let err = stderr_with(&home, &root, &[], &["--color=always", "new", "b"]);
    assert!(err.starts_with("\x1b[1;96m     Created\x1b[0m "), "{err:?}");
    let vars = [("CLICOLOR_FORCE", "1"), ("TERM", "xterm-256color")];
    let err = stderr_with(&home, &root, &vars, &["new", "c"]);
    assert!(
        err.starts_with("\x1b[1;38;5;81m     Created\x1b[0m "),
        "{err:?}"
    );
    let vars = [("CLICOLOR_FORCE", "1"), ("COLORTERM", "24bit")];
    let err = stderr_with(&home, &root, &vars, &["new", "d"]);
    assert!(
        err.starts_with("\x1b[1;38;2;56;189;248m     Created\x1b[0m "),
        "{err:?}"
    );
    // Labels are coloured, and there without colour.
    let err = stderr_with(&home, &root, &[("CLICOLOR_FORCE", "1")], &["check"]);
    assert!(
        err.starts_with("nabi: \x1b[1;91merror:\x1b[0m not inside a package"),
        "{err:?}"
    );
}

/// `doc --std` without a path documents the standard library alone:
/// outside a package too, into ./target/doc/, and the status line names
/// the library, not a temporary package.
#[test]
fn doc_std_without_a_package() {
    let root = scratch("doc-std");
    let home = root.join("home");
    let dir = root.join("plain");
    fs::create_dir_all(&dir).unwrap();
    let out = nabi(&home, &dir, &["doc", "--std"]);
    ok(&out);
    let err = text(&out.stderr);
    assert!(
        err.contains(" the standard library\n") && !err.contains("nabic-"),
        "{err}"
    );
    assert!(dir.join("target/doc/std").is_dir());
    assert!(!dir.join("target/doc/plain").exists());
    // A plain `doc` there still wants a package.
    fails(&nabi(&home, &dir, &["doc"]), "not inside a package");
}

#[test]
fn no_colour_when_piped_or_no_color() {
    let root = scratch("colour-off");
    let home = root.join("home");
    let force = ("CLICOLOR_FORCE", "1");
    for (i, (vars, args)) in [
        (&[][..], &[][..]),
        (&[("NO_COLOR", "1"), force][..], &[][..]),
        (&[("TERM", "dumb")][..], &[][..]),
        (&[force][..], &["--color=never"][..]),
        (&[force][..], &["--no-color"][..]),
        (&[force][..], &["--color", "never"][..]),
    ]
    .into_iter()
    .enumerate()
    {
        let name = format!("p{i}");
        let args = [args, &["new", &name]].concat();
        let err = stderr_with(&home, &root, vars, &args);
        assert!(err.starts_with("     Created "), "{args:?}: {err:?}");
        assert!(!err.contains('\x1b'), "{vars:?} {args:?}: {err:?}");
    }
    ok(&nabi(&home, &root, &["env", "create", "e1"]));
    ok(&nabi(&home, &root, &["env", "use", "e1"]));
    let list = nabi_cmd(&home, &root, &["env", "list"])
        .env("CLICOLOR_FORCE", "1")
        .env("TERM", "xterm")
        .env_remove("NO_COLOR")
        .env_remove("COLORTERM")
        .output()
        .unwrap();
    assert_eq!(text(&list.stdout), "\x1b[1;96m*\x1b[0m e1 (active)\n");
    let out = nabi(&home, &root, &["--color=sometimes", "new", "x"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        text(&out.stderr).contains("`--color` takes auto|always|never, not `sometimes`"),
        "{}",
        text(&out.stderr)
    );
}

#[test]
fn nabi_passes_color_to_nabic() {
    let root = scratch("colour-nabic");
    let home = root.join("home");
    ok(&nabi(&home, &root, &["new", "app"]));
    let app = root.join("app");
    write(&app.join("src/main.nabi"), "let x: i32 = nope;\n");
    for args in [
        &["--color=always", "check"][..],
        &["check", "--color", "always"],
    ] {
        let err = stderr_with(&home, &app, &[], args);
        assert!(
            err.contains("\x1b[1;91merror[E0027]\x1b[0m"),
            "{args:?}: {err:?}"
        );
    }
    let err = stderr_with(
        &home,
        &app,
        &[("CLICOLOR_FORCE", "1")],
        &["check", "--no-color"],
    );
    assert!(
        err.contains("error[E0027]") && !err.contains('\x1b'),
        "{err:?}"
    );
}

/// A script that is not there is named, not taken for a missing
/// package.
#[test]
fn run_missing_script_says_so() {
    let root = scratch("missing-script");
    let home = root.join("home");
    for args in [
        &["run", "nope.nabi"][..],
        &["nope.nabi"],
        &["build", "nope.nabi"],
        &["check", "dir/nope"],
        &["test", "nope.nabi"],
        &["fmt", "nope.nabi"],
    ] {
        let out = nabi_within(&home, &root, args);
        let want = format!("`{}` does not exist", args.last().unwrap());
        fails(&out, &want);
        assert!(
            !text(&out.stderr).contains("not inside a package"),
            "{args:?}"
        );
    }
    // For `test`, a word may be a filter.
    fails(
        &nabi_within(&home, &root, &["test", "add"]),
        "not inside a package",
    );
}

/// Bare `nabi jupyter` points to `nabi jupyter install`.
#[test]
fn bare_jupyter_points_to_install() {
    let root = scratch("bare-jupyter");
    let home = root.join("home");
    let out = nabi_within(&home, &root, &["jupyter"]);
    assert_eq!(out.status.code(), Some(2));
    let want = "nabi: Jupyter starts the kernel itself; to add it, run `nabi jupyter install`\n\
                run `nabi help jupyter` for usage\n";
    assert_eq!(text(&out.stderr), want);
    // With options but no connection file, the same.
    let out = nabi_within(&home, &root, &["jupyter", "-q"]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(text(&out.stderr), want);
}

/// `nabi --version -v` names the binary, the toolchain it resolves and
/// why, its `nabic` and the host, as `key: value` lines like `nabic
/// --version -v`; `--format json` gives one object.
#[test]
fn version_verbose_key_value() {
    let root = scratch("version-verbose");
    let home = root.join("home");
    let out = ok(&nabi(&home, &root, &["--version", "-v"]));
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines[0].starts_with("nabi "), "{out}");
    let real = |p: &str| nabic_core::real_path(p).unwrap().display().to_string();
    let nabic = Path::new(NABI).with_file_name(exe("nabic"));
    assert_eq!(lines[1], format!("binary:    {}", real(NABI)));
    assert!(
        lines[2].starts_with("toolchain: none (this nabi is "),
        "{out}"
    );
    assert_eq!(lines[3], format!("nabic:     {}", nabic.display()));
    assert_eq!(lines[4], format!("host:      {}", env!("NABI_HOST")));
    assert_eq!(ok(&nabi(&home, &root, &["version", "-v"])), out);
    assert_eq!(lines.len(), 5, "{out}");
    // Read the way `nabi toolchain` reads `nabic --version -v`.
    let host = lines.iter().find_map(|l| l.strip_prefix("host: "));
    assert_eq!(host.map(str::trim), Some(env!("NABI_HOST")));

    let json = ok(&nabi(&home, &root, &["version", "-v", "--format", "json"]));
    let j = nabic_diagnostic::Json::parse(json.trim()).unwrap();
    assert_eq!(j.get("schema").as_u32(), Some(1), "{json}");
    assert_eq!(j.get("version").as_str(), Some(env!("CARGO_PKG_VERSION")));
    assert_eq!(j.get("binary").as_str(), Some(real(NABI).as_str()));
    assert_eq!(j.get("host").as_str(), Some(env!("NABI_HOST")));
    assert_eq!(json.lines().count(), 1, "{json}");
    for args in [
        &["--version", "-v", "--format", "json"][..],
        &["-V", "-v", "--format", "json"],
    ] {
        assert_eq!(ok(&nabi(&home, &root, args)), json, "{args:?}");
    }
    let json = ok(&nabi(&home, &root, &["--version", "--format=json"]));
    let j = nabic_diagnostic::Json::parse(json.trim()).unwrap();
    assert_eq!(j.get("host"), &nabic_diagnostic::Json::Null, "{json}");
    let out = nabi(&home, &root, &["version", "--format", "csv"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        text(&out.stderr).contains(
            "`--format` takes human or json, not `csv`: write `nabi version --format json`"
        ),
        "{}",
        text(&out.stderr)
    );
}

#[test]
fn nabils_is_nabi_lsp() {
    let root = scratch("lsp");
    let frame = |b: &str| format!("Content-Length: {}\r\n\r\n{b}", b.len());
    let input = [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"capabilities":{}}}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"shutdown"}"#,
        r#"{"jsonrpc":"2.0","method":"exit"}"#,
    ]
    .map(frame)
    .concat();
    build_nabic();
    // lsp/nabils, which runs this `nabi`.
    let nabils = Path::new(NABI).with_file_name(exe("nabils"));
    let nabils = nabils.to_str().unwrap();
    // Editors pass `--stdio`: accepted, stdio being the only transport.
    for (exe, args) in [
        (nabils, &[][..]),
        (NABI, &["lsp"][..]),
        (nabils, &["--stdio"][..]),
        (NABI, &["lsp", "--stdio"][..]),
    ] {
        let mut child = Command::new(exe)
            .args(args)
            .current_dir(&root)
            .env("NABI_HOME", root.join("home"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        let stdout = ok(&out);
        assert!(stdout.contains(r#""hoverProvider":true"#), "{stdout}");
    }
}

/// `cmd` in `dir`, which must succeed.
#[cfg(unix)]
pub(crate) fn sh(dir: &Path, cmd: &str, args: &[&str]) {
    let out = Command::new(cmd)
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{cmd} {args:?}: {}",
        text(&out.stderr)
    );
}

/// Whether `shell` runs here (zsh, fish and pwsh may be missing).
pub(crate) fn have(shell: &str) -> bool {
    Command::new(shell)
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// `nabi completions <shell>`: a script with nabi's commands, its own, its
/// tools and those it passes to nabic, that the shell parses; toolchain and
/// environment names come from `nabi __complete`.
#[test]
fn completions_for_each_shell() {
    let root = scratch("completions");
    let home = root.join("home");
    for shell in ["bash", "zsh", "fish", "powershell"] {
        let script = ok(&nabi(&home, &root, &["completions", shell]));
        for want in [
            "toolchain",
            "completions",
            "build",
            "env",
            "--offline",
            "--release",
            "bindgen",
            "demangle",
            "--check-library",
            "--target-dir",
            "nabi __complete toolchains",
        ] {
            // fish spells `--x` as `-l x`.
            let want = match want.strip_prefix("--") {
                Some(f) if shell == "fish" => format!("-l {f}"),
                _ => want.to_owned(),
            };
            assert!(script.contains(&want), "{shell} lacks {want}");
        }
        let ext = if shell == "powershell" { "ps1" } else { shell };
        let file = root.join(format!("nabi-{shell}.{ext}"));
        write(&file, &script);
        let check: &[&str] = match shell {
            "bash" => &["bash", "-n"],
            "zsh" => &["zsh", "-n"],
            "fish" => &["fish", "--no-config", "-n"],
            _ => &[],
        };
        if let Some((sh, args)) = check.split_first().filter(|(sh, _)| have(sh)) {
            let out = Command::new(sh).args(args).arg(&file).output().unwrap();
            assert!(out.status.success(), "{shell}: {}", text(&out.stderr));
        }
        if shell == "powershell" && have("pwsh") {
            let parse = format!(
                "$e = $null; [void][Management.Automation.Language.Parser]::ParseFile('{}', \
                 [ref]$null, [ref]$e); if ($e) {{ $e; exit 1 }}",
                file.display()
            );
            let out = Command::new("pwsh")
                .args(["-NoProfile", "-c", &parse])
                .output()
                .unwrap();
            assert!(out.status.success(), "{}", text(&out.stdout));
        }
    }
    let out = nabi(&home, &root, &["completions", "tcsh"]);
    assert_eq!(out.status.code(), Some(2));
    fails(&out, "nabi completions bash|zsh|fish|powershell");

    let host = env!("NABI_HOST");
    fs::create_dir_all(home.join(format!("toolchains/9.9.1-{host}"))).unwrap();
    fs::create_dir_all(home.join("envs/dev")).unwrap();
    assert_eq!(
        ok(&nabi(&home, &root, &["__complete", "toolchains"])),
        "9.9.1\n"
    );
    assert_eq!(ok(&nabi(&home, &root, &["__complete", "envs"])), "dev\n");

    // What bash offers, with this nabi first on PATH (where there is a
    // bash: FreeBSD's base has none).
    #[cfg(unix)]
    if Command::new("bash").arg("-c").arg("true").output().is_ok() {
        let bin = Path::new(NABI).parent().unwrap();
        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let offer = |words: &[&str]| {
            let out = Command::new("bash")
                .arg("-c")
                .arg(
                    "source \"$1\"; shift; COMP_WORDS=(\"$@\"); \
                     COMP_CWORD=$((${#COMP_WORDS[@]} - 1)); _nabi; echo \"${COMPREPLY[*]}\"",
                )
                .arg("bash")
                .arg(root.join("nabi-bash.bash"))
                .args(words)
                .env("PATH", &path)
                .env("NABI_HOME", &home)
                .output()
                .unwrap();
            ok(&out).trim().to_owned()
        };
        for (words, want) in [
            (&["nabi", "toolc"][..], "toolchain"),
            (&["nabi", "toolchain", "def"], "default"),
            (&["nabi", "toolchain", "default", ""], "9.9.1"),
            (&["nabi", "+"], "+9.9.1"),
            (&["nabi", "--env", ""], "dev"),
            (&["nabi", "--env", "dev", "env", "use", ""], "dev"),
            (&["nabi", "build", "--rel"], "--release"),
            (&["nabi", "add", "x", "--regi"], "--registry"),
            (&["nabi", "help", "compl"], "completions"),
            (&["nabi", "new", "x", "--li"], "--lib"),
            (&["nabi", "build", "--all-f"], "--all-features"),
            (&["nabi", "deps", "--no-def"], "--no-default-features"),
            (&["nabi", "env", "u"], "use"),
            (&["nabi", "toolchain", "install", "1", "--fr"], "--from"),
        ] {
            assert_eq!(offer(words), want, "{words:?}");
        }
    }
}

/// A closed stdout ends `nabi` quietly (`nabi help | head`): killed by
/// SIGPIPE (Windows: its own status, std dropping the writes), not an
/// internal error (101).
#[test]
fn closed_output_ends_nabi_quietly() {
    // A pipe whose reader has exited: `nabi version` never reads stdin.
    let mut reader = Command::new(NABI)
        .arg("version")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let pipe = reader.stdin.take().unwrap();
    assert!(reader.wait().unwrap().success());
    let out = Command::new(NABI)
        .arg("help")
        .stdout(pipe)
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    #[cfg(unix)]
    assert_eq!(
        std::os::unix::process::ExitStatusExt::signal(&out.status),
        Some(13),
        "{stderr}"
    );
    // Windows' std drops writes to a pipe whose reader left: `help` ends
    // as it would.
    #[cfg(not(unix))]
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert_eq!(stderr, "");
}

/// `nabi -q <args>`'s stdout.
#[track_caller]
pub(crate) fn script_out(home: &Path, cwd: &Path, args: &[&str]) -> String {
    let mut all = vec!["-q"];
    all.extend_from_slice(args);
    ok(&nabi(home, cwd, &all))
}

/// nabi leaves nothing in the temp directory: a script run, a failing
/// build, an environment library's prebuild, an executable install.
#[test]
fn nabi_leaves_no_temporaries() {
    let root = scratch("no-temporaries");
    let home = root.join("home");
    let tmp = root.join("tmp");
    fs::create_dir_all(&tmp).unwrap();
    let run = |args: &[&str]| {
        let mut c = nabi_cmd(&home, &root, args);
        for v in ["TMPDIR", "TMP", "TEMP"] {
            c.env(v, &tmp);
        }
        c.output().unwrap()
    };
    write(
        &root.join("greet/nabi.nabion"),
        "Package { name: \"greet\" }\n",
    );
    write(&root.join("greet/src/lib.nabi"), LIB_V1);
    write(
        &root.join("tool/nabi.nabion"),
        "Package { name: \"tool\" }\n",
    );
    write(&root.join("tool/src/main.nabi"), "println(\"tool\");\n");
    write(&root.join("hi.nabi"), "println(\"hi\");\n");
    write(&root.join("bad.nabi"), "let x: i32 = \"no\";\n");
    assert_eq!(ok(&run(&["-q", "hi.nabi"])), "hi\n");
    assert_ne!(run(&["-q", "bad.nabi"]).status.code(), Some(0));
    ok(&run(&["install", "--path", "greet"]));
    ok(&run(&["install", "--path", "tool"]));
    let left: Vec<String> = fs::read_dir(&tmp)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !n.starts_with("nabic-target"))
        .collect();
    assert!(left.is_empty(), "left behind: {left:?}");
}

/// `nabi test`'s runner options are its own: a bad value is a usage
/// error, and a bad `NABI_TEST_THREADS` warns and keeps the default.
#[test]
fn test_runner_options() {
    let root = scratch("test-runner-opts");
    let home = root.join("home");
    ok(&nabi(&home, &root, &["new", "t"]));
    let dir = root.join("t");
    write(
        &dir.join("src/main.nabi"),
        "fn main() {}\n#[test]\nfn one() { println(\"from one\"); }\n",
    );
    let out = nabi_cmd(&home, &dir, &["-q", "test", "--show-output"])
        .env("NABI_TEST_THREADS", "0")
        .output()
        .unwrap();
    let s = ok(&out);
    assert!(s.contains("from one") && s.contains("1 passed"), "{s}");
    assert!(
        text(&out.stderr)
            .contains("nabi: warning: ignoring NABI_TEST_THREADS=0 (expected a positive number)"),
        "{}",
        text(&out.stderr)
    );
    let out = nabi(&home, &dir, &["test", "--threads=0"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        text(&out.stderr).contains("`--threads` takes a positive number, not `0`"),
        "{}",
        text(&out.stderr)
    );
}

/// `nabi bench` prints the human line whatever stdout is; `--format
/// nabibench` the machine one. A bench that never calls `b.iter` is named,
/// not counted as run.
#[test]
fn bench_output_is_human_when_piped() {
    let root = scratch("bench-piped");
    let home = root.join("home");
    ok(&nabi(&home, &root, &["new", "b"]));
    let dir = root.join("b");
    write(
        &dir.join("src/main.nabi"),
        "use core::bench::Bench;\nuse core::hint::black_box;\n\
         #[bench]\nfn sum(b: Bench) { b.iter(|| black_box(1i64 + 2i64)); }\n\
         #[bench]\nfn idle(b: Bench) { let _ = b; }\n",
    );
    let args = ["-q", "bench", "--iters=3", "--samples=2"];
    let out = nabi(&home, &dir, &args);
    let s = ok(&out);
    assert!(s.contains("  sum   ") && s.contains(" +/- "), "{s}");
    assert!(!s.contains("#nabibench"), "{s}");
    assert!(
        s.contains("  idle  warning: measured nothing: time the code with `b.iter(|| ..)`"),
        "{s}"
    );
    assert!(
        s.contains("bench result: ok. 1 run; 0 failed; 1 measured nothing; 0 filtered out"),
        "{s}"
    );
    let out = nabi(
        &home,
        &dir,
        &[&args[..], &["--format", "nabibench"]].concat(),
    );
    let s = ok(&out);
    assert!(s.contains("#nabibench name=sum median_ns="), "{s}");
    let out = nabi(&home, &dir, &["bench", "--format=csv"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        text(&out.stderr).contains("`--format` takes human, json or nabibench, not `csv`"),
        "{}",
        text(&out.stderr)
    );
}

/// `nabi cache` names the toolchain's cache and prunes it; a bad value
/// says what to write.
#[test]
fn cache_shows_and_prunes() {
    let home = scratch("cache-cmd");
    let out = nabi(&home, &home, &["cache", "dir"]);
    let dir = ok(&out);
    assert!(dir.trim_end().ends_with("nabi-cache"), "{dir}");
    // Nothing is that old: the prune keeps every entry.
    let out = nabi(&home, &home, &["cache", "prune", "--unused", "36500"]);
    ok(&out);
    assert!(
        text(&out.stderr).contains("Pruned "),
        "{}",
        text(&out.stderr)
    );
    fails(
        &nabi(&home, &home, &["cache", "prune", "--max-size", "lots"]),
        "write bytes with K, M or G, as in `--max-size 2G`",
    );
}

/// `nabi build --target` with an in-progress target says what to build
/// now, without the maintainers' opt-in.
#[test]
fn build_refuses_an_in_progress_target() {
    let root = scratch("in-progress-target");
    let home = root.join("home");
    let app = root.join("app");
    write(
        &app.join("nabi.nabion"),
        "Package { name: \"app\", version: \"0.1.0\" }\n",
    );
    write(&app.join("src/main.nabi"), "fn main() {}\n");
    let out = nabi_cmd(&home, &app, &["build", "--target", "aarch64-linux-android"])
        .env_remove(nabic_target::IN_PROGRESS_VAR)
        .output()
        .unwrap();
    fails(
        &out,
        "target `aarch64-linux-android` (64-bit ARM Android devices) is in progress: it is not \
         tested on real hardware yet and comes in a later release; for now build for the \
         emulator with `--target x86_64-linux-android`",
    );
}
