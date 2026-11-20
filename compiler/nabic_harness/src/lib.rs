//! Running the `#[test]` and `#[bench]` harness that `nabic` builds. The
//! compiler produces a self-dispatching executable and the list of cases
//! in it; this crate selects cases, runs each in its own process and
//! reports the result. It is the `nabi test` / `nabi bench` runner, kept out
//! of the compiler because it needs none of it: only a path and some
//! names. Output goes to an injected writer so a caller can capture it.

pub mod bench;
pub mod test;

use nabic_core::term::{self, Role, Stream};
use nabic_diagnostic::Json;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// The manifest format `nabic` writes and this runner reads.
pub const VERSION: u32 = 2;

/// Check a harness manifest's `version` line: up to `VERSION` is read;
/// a newer one, or none, is refused.
pub fn check_version(text: &str) -> Result<(), String> {
    let first = text.lines().next().unwrap_or("");
    let Some(v) = first.strip_prefix("version ") else {
        return Err(
            "not a test harness manifest: it starts with no `version` line; \
                    write it with `nabic test --emit-harness`"
                .to_owned(),
        );
    };
    match v.trim().parse::<u32>() {
        Ok(n) if n <= VERSION => Ok(()),
        _ => Err(format!(
            "the test harness is from a newer nabic (format version {}); run the nabi of the \
             same toolchain, e.g. `nabi +<version> test`",
            v.trim()
        )),
    }
}

/// A test's time limit when neither `--timeout` nor `NABI_TEST_TIMEOUT`
/// sets one.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(600);

/// `nabi test`'s runner options, for its help: `(head, description)`.
pub const TEST_OPTIONS: &[(&str, &str)] = &[
    (
        "--threads=N",
        "tests run at once (default: NABI_TEST_THREADS, else the CPU count)",
    ),
    ("--show-output", "the output of passing tests too"),
    (
        "--format=human|json",
        "json: one JSON line per test and a summary on stdout, the human text on stderr",
    ),
    (
        "--list",
        "print the tests' names (those the filter keeps) instead of running them",
    ),
    (
        "--exact",
        "the filter and `--skip` match whole test names, not parts",
    ),
    ("--skip=PAT", "leave out the tests PAT matches (repeatable)"),
    ("--ignored", "run only the `#[ignore]` tests"),
    ("--include-ignored", "run the `#[ignore]` tests too"),
    (
        "--timeout=SECS",
        "kill and fail a test running longer (default: NABI_TEST_TIMEOUT, else 600; 0: no limit)",
    ),
    (
        "--doc",
        "run only the doc tests (```nabi blocks in the library's docs and README.md)",
    ),
    ("--no-doc", "run no doc tests"),
];

/// `nabi bench`'s runner options, for its help.
pub const BENCH_OPTIONS: &[(&str, &str)] = &[
    (
        "--iters=N",
        "calls per sample (default: calibrated to ~100ms)",
    ),
    ("--samples=N", "timed samples (default 10)"),
    (
        "--format=human|json|nabibench",
        "json: as for tests; nabibench: one `#nabibench` line per benchmark",
    ),
    (
        "--list",
        "print the benchmarks' names (those the filter keeps) instead of running them",
    ),
];

/// The runner's own options, which `nabi test` / `nabi bench` take and
/// `nabic` never sees.
// One flag per runner switch, as `nabic`'s `Args`.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunOptions {
    /// `test --threads=N`; `None`: `NABI_TEST_THREADS`, else the CPU count.
    pub threads: Option<usize>,
    /// `test --show-output`.
    pub show_output: bool,
    /// `bench --iters=N`; `None`: the library calibrates.
    pub iters: Option<u64>,
    /// `bench --samples=N`; `None`: 10.
    pub samples: Option<u64>,
    /// `--format=json`.
    pub json: bool,
    /// `bench --format=nabibench`.
    pub nabibench: bool,
    /// `--list`: the cases' names, not their runs.
    pub list: bool,
    /// `test --exact`.
    pub exact: bool,
    /// `test --skip <pat>`, each.
    pub skip: Vec<String>,
    /// `test --ignored` / `--include-ignored`.
    pub ignored: test::Ignored,
    /// `test --timeout=SECS`; `Some(0)`: no limit; `None`:
    /// `NABI_TEST_TIMEOUT`, else `DEFAULT_TIMEOUT`.
    pub timeout: Option<u64>,
}

/// The runner options of `kind` (`test` or `bench`) taken out of `args`
/// (up to `--`), and the arguments left for `nabic`.
///
/// # Errors
/// A runner option without a value or with a bad one.
pub fn take_options(kind: &str, args: &[String]) -> Result<(RunOptions, Vec<String>), String> {
    let mut o = RunOptions::default();
    let mut rest = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--" {
            rest.push(a.clone());
            rest.extend(it.cloned());
            break;
        }
        let (name, inline) = match a.split_once('=') {
            Some((n, v)) if n.starts_with("--") => (n, Some(v.to_owned())),
            _ => (a.as_str(), None),
        };
        let mut value = || {
            inline
                .clone()
                .or_else(|| it.next().cloned())
                .ok_or_else(|| format!("`{name}` needs a value"))
        };
        match (kind, name) {
            ("test", "--threads") => {
                let v = value()?;
                o.threads =
                    Some(v.parse().ok().filter(|n| *n > 0).ok_or_else(|| {
                        format!("`--threads` takes a positive number, not `{v}`")
                    })?);
            }
            ("test", "--show-output") if inline.is_none() => o.show_output = true,
            ("test", "--exact") if inline.is_none() => o.exact = true,
            ("test", "--skip") => o.skip.push(value()?),
            ("test", "--ignored") if inline.is_none() => o.ignored = test::Ignored::Only,
            ("test", "--include-ignored") if inline.is_none() => {
                o.ignored = test::Ignored::Include;
            }
            ("test", "--timeout") => {
                let v = value()?;
                o.timeout = Some(v.parse().map_err(|_| {
                    format!(
                        "`--timeout` takes whole seconds (0: no limit), not `{v}`: `--timeout=120`"
                    )
                })?);
            }
            (_, "--list") if inline.is_none() => o.list = true,
            ("bench", "--iters" | "--samples") => {
                let v = value()?;
                let n = v
                    .parse()
                    .ok()
                    .filter(|n| *n > 0)
                    .ok_or_else(|| format!("`{name}` takes a positive number, not `{v}`"))?;
                if name == "--iters" {
                    o.iters = Some(n);
                } else {
                    o.samples = Some(n);
                }
            }
            (_, "--format") => match (kind, value()?.as_str()) {
                (_, "human") => (o.json, o.nabibench) = (false, false),
                (_, "json") => (o.json, o.nabibench) = (true, false),
                ("bench", "nabibench") => (o.json, o.nabibench) = (false, true),
                ("bench", v) => {
                    return Err(format!(
                        "`--format` takes human, json or nabibench, not `{v}`"
                    ));
                }
                (_, "nabibench") => {
                    return Err(
                        "`--format` takes human or json for test; `nabibench` is for bench"
                            .to_owned(),
                    );
                }
                (_, v) => return Err(format!("`--format` takes human or json, not `{v}`")),
            },
            _ => rest.push(a.clone()),
        }
    }
    Ok((o, rest))
}

/// The number of tests run at once: `--threads`, else `NABI_TEST_THREADS`,
/// else the available parallelism.
#[must_use]
pub fn test_threads(o: &RunOptions) -> usize {
    o.threads
        .or_else(|| {
            let v = std::env::var("NABI_TEST_THREADS")
                .ok()
                .filter(|v| !v.is_empty())?;
            let n = v.parse().ok().filter(|&n: &usize| n > 0);
            if n.is_none() {
                eprintln!(
                    "nabi: warning: ignoring NABI_TEST_THREADS={v} (expected a positive number)"
                );
            }
            n
        })
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, usize::from))
}

/// A test's time limit: `--timeout`, else `NABI_TEST_TIMEOUT`, else
/// `DEFAULT_TIMEOUT`; `None` for 0.
#[must_use]
pub fn test_timeout(o: &RunOptions) -> Option<Duration> {
    let secs = o.timeout.or_else(|| {
        let v = std::env::var("NABI_TEST_TIMEOUT")
            .ok()
            .filter(|v| !v.is_empty())?;
        let n = v.parse().ok();
        if n.is_none() {
            eprintln!(
                "nabi: warning: ignoring NABI_TEST_TIMEOUT={v} (expected whole seconds, 0: no limit)"
            );
        }
        n
    });
    match secs {
        Some(0) => None,
        Some(n) => Some(Duration::from_secs(n)),
        None => Some(DEFAULT_TIMEOUT),
    }
}

/// Run the cases in a harness manifest with options `o`; the exit code
/// (0: all passed).
///
/// The manifest is what `nabic test` / `nabic bench` write with
/// `--emit-harness`: a `version N` line, then simple lines of
/// `kind test|bench`, an optional `filter` (raw to end of line), the
/// `runner <word>` lines a non-host target's executables run under (or
/// `norunner <why>`), then an `exe <path>` per built executable (empty:
/// none, all its cases settled) each followed by its `case <name>` lines.
/// After a case come what it needs: `call <arg>` (what the executable is
/// given instead of the name), `ignore <reason>`, `panic <text>` and
/// `outcome ok|failed|ignored <why>` (settled at build time), with a
/// backslash written `\\` and a newline `\n`. A package with
/// both roots lists the library first; a failing run ends the command. With `--format=json`, `out` gets the
/// JSON records and the human text goes to stderr.
pub fn run_manifest(text: &str, o: &RunOptions, out: &mut dyn Write) -> i32 {
    run_manifest_to(text, o, out, &mut std::io::stderr())
}

/// [`run_manifest`], with `--format=json`'s human text going to `human`.
pub fn run_manifest_to(
    text: &str,
    o: &RunOptions,
    out: &mut dyn Write,
    human: &mut dyn Write,
) -> i32 {
    let mut kind = "test";
    let mut filter: Option<String> = None;
    let mut runner: Vec<String> = Vec::new();
    let mut no_runner: Option<String> = None;
    let mut harnesses: Vec<(PathBuf, Vec<test::Case>)> = Vec::new();
    for line in text.lines() {
        let (key, rest) = line.split_once(' ').unwrap_or((line, ""));
        match key {
            "kind" => kind = if rest == "bench" { "bench" } else { "test" },
            "filter" => filter = Some(rest.to_owned()),
            "runner" => runner.push(rest.to_owned()),
            "norunner" => no_runner = Some(rest.to_owned()),
            "exe" => harnesses.push((PathBuf::from(rest), Vec::new())),
            "case" => {
                if let Some((_, cases)) = harnesses.last_mut() {
                    cases.push(test::Case::new(rest));
                }
            }
            "call" | "ignore" | "panic" | "outcome" => {
                let Some(c) = harnesses.last_mut().and_then(|(_, cs)| cs.last_mut()) else {
                    continue;
                };
                let rest = unescape(rest);
                match key {
                    "call" => c.call = Some(rest),
                    "ignore" => c.ignore = Some(rest),
                    "panic" => c.panics = Some(rest),
                    _ => {
                        let (r, why) = rest.split_once(' ').unwrap_or((&rest, ""));
                        let why = why.to_owned();
                        c.outcome = Some(match r {
                            "ok" => test::Settled::Passed,
                            "ignored" => test::Settled::Ignored(why),
                            _ => test::Settled::Failed(why),
                        });
                    }
                }
            }
            _ => {}
        }
    }
    let skip = o.skip.clone();
    let select = test::Options {
        filter: filter.as_deref(),
        exact: o.exact,
        skip: &skip,
        ignored: o.ignored,
        ..test::Options::default()
    };
    if o.list {
        for c in harnesses
            .iter()
            .flat_map(|(_, cases)| cases.iter().filter(|c| select.selects(c)))
        {
            let ignored = c.ignore.is_some();
            if o.json {
                let mut fields = vec![("name", Json::str(c.name.as_str()))];
                if ignored {
                    fields.push(("ignored", Json::Bool(true)));
                }
                record(out, "listed", kind, fields);
            } else if ignored {
                let _ = writeln!(out, "{} (ignored)", c.name);
            } else {
                let _ = writeln!(out, "{}", c.name);
            }
        }
        let _ = out.flush();
        return 0;
    }
    if let Some(why) = no_runner {
        let _ = writeln!(
            human,
            "nabi: {} {why}",
            term::label(Stream::Stderr, Role::Error)
        );
        return 1;
    }
    let threads = if kind == "test" { test_threads(o) } else { 1 };
    let (text_out, mut records): (&mut dyn Write, Option<&mut dyn Write>) = if o.json {
        (human, Some(out))
    } else {
        (out, None)
    };
    let mut ok = true;
    for (exe, cases) in &harnesses {
        let passed = if kind == "bench" {
            let names: Vec<String> = cases.iter().map(|c| c.name.clone()).collect();
            let opts = bench::Options {
                filter: filter.as_deref(),
                iters: o.iters,
                samples: o.samples.unwrap_or(10),
                nabibench: o.nabibench,
                runner: &runner,
            };
            bench::run_to(exe, &names, &opts, text_out, records.as_deref_mut())
        } else {
            let opts = test::Options {
                threads,
                show_output: o.show_output,
                timeout: test_timeout(o),
                runner: &runner,
                ..select
            };
            test::run_to(exe, cases, &opts, text_out, records.as_deref_mut())
        };
        let _ = text_out.flush();
        if let Some(r) = records.as_deref_mut() {
            let _ = r.flush();
        }
        if !passed {
            // A failing run (the library's, when it is first) ends it.
            ok = false;
            break;
        }
    }
    i32::from(!ok)
}

/// A manifest value with its `\\` and `\n` escapes read back.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some(x) => out.push(x),
            None => out.push('\\'),
        }
    }
    out
}

/// The command running `exe` under `runner` (program and arguments
/// first); `exe` itself when it is empty.
fn command(runner: &[String], exe: &Path) -> Command {
    match runner.split_first() {
        Some((program, args)) => {
            let mut c = Command::new(program);
            c.args(args).arg(exe);
            c
        }
        None => Command::new(exe),
    }
}

/// Write one `format json` record: `{"schema":1,"type":ty,"kind":kind,..fields}`.
fn record(to: &mut dyn Write, ty: &str, kind: &str, fields: Vec<(&str, Json)>) {
    let mut all = vec![
        (
            "schema".to_owned(),
            Json::num(nabic_diagnostic::RECORD_SCHEMA),
        ),
        ("type".to_owned(), Json::str(ty)),
        ("kind".to_owned(), Json::str(kind)),
    ];
    all.extend(fields.into_iter().map(|(k, v)| (k.to_owned(), v)));
    let _ = writeln!(to, "{}", Json::Obj(all));
}

fn count(n: usize) -> Json {
    Json::num(u32::try_from(n).unwrap_or(u32::MAX))
}

/// `d` in milliseconds, to the microsecond.
fn ms(d: std::time::Duration) -> Json {
    Json::num((d.as_secs_f64() * 1e6).round() / 1e3)
}

/// Test / bench filter ("regex or plain substring"): unanchored,
/// with `^ $ . * + ?` and `\`; no classes, groups or alternation.
#[must_use]
pub fn re_match(re: &str, text: &str) -> bool {
    let re: Vec<char> = re.chars().collect();
    let text: Vec<char> = text.chars().collect();
    if re.first() == Some(&'^') {
        return here(&re[1..], &text);
    }
    (0..=text.len()).any(|i| here(&re, &text[i..]))
}

fn here(re: &[char], t: &[char]) -> bool {
    let Some(&c) = re.first() else { return true };
    if c == '$' && re.len() == 1 {
        return t.is_empty();
    }
    let (atom, len) = match (c, re.get(1)) {
        ('\\', Some(&l)) => (Some(l), 2),
        ('.', _) => (None, 1),
        _ => (Some(c), 1),
    };
    let hit = |t: &[char]| t.first().is_some_and(|&x| atom.is_none_or(|a| a == x));
    let rest = &re[len..];
    match rest.first() {
        Some('*') => star(hit, &rest[1..], t),
        Some('+') => hit(t) && star(hit, &rest[1..], &t[1..]),
        Some('?') => here(&rest[1..], t) || (hit(t) && here(&rest[1..], &t[1..])),
        _ => hit(t) && here(rest, &t[1..]),
    }
}

fn star(hit: impl Fn(&[char]) -> bool, re: &[char], mut t: &[char]) -> bool {
    loop {
        if here(re, t) {
            return true;
        }
        if !hit(t) {
            return false;
        }
        t = &t[1..];
    }
}

#[cfg(test)]
mod tests {
    use super::{RunOptions, check_version, re_match, run_manifest_to};

    /// No runner for the target: listing works, running fails and says why.
    #[test]
    fn missing_runner() {
        let m = "version 1\nkind test\nnorunner no `wasmtime` on PATH\nexe /nowhere/t.wasm\ncase adds\n";
        let (mut out, mut human) = (Vec::new(), Vec::new());
        let list = RunOptions {
            list: true,
            ..RunOptions::default()
        };
        assert_eq!(run_manifest_to(m, &list, &mut out, &mut human), 0);
        assert_eq!(String::from_utf8_lossy(&out), "adds\n");
        let code = run_manifest_to(m, &RunOptions::default(), &mut out, &mut human);
        let why = String::from_utf8_lossy(&human);
        assert_eq!(code, 1);
        assert!(why.contains("no `wasmtime` on PATH"), "{why}");
    }

    /// `runner` words go before the executable, one argument each.
    #[test]
    fn runner_words() {
        let c = super::command(
            &["sh".to_owned(), "-c".to_owned(), "exit 3".to_owned()],
            std::path::Path::new("x"),
        );
        assert_eq!(c.get_program(), "sh");
        let args: Vec<_> = c.get_args().collect();
        assert_eq!(args, ["-c", "exit 3", "x"]);
    }

    #[test]
    fn version_line_accepted() {
        assert!(check_version("version 1\nkind test\n").is_ok());
        assert!(check_version("version 1\nkind test\nformat json\n").is_ok());
        let e = check_version("kind test\nthreads 1\n").unwrap_err();
        assert!(e.contains("no `version` line"), "{e}");
    }

    #[test]
    fn newer_version_refused() {
        assert!(check_version("version 2\nkind test\n").is_ok());
        let e = check_version("version 3\nkind test\n").unwrap_err();
        assert!(e.contains("newer nabic (format version 3)"), "{e}");
        assert!(e.contains("run the nabi of the same toolchain"), "{e}");
        assert!(check_version("version x\n").is_err());
    }

    #[test]
    fn test_options() {
        let args: Vec<String> = [
            "--exact",
            "--skip",
            "a",
            "--skip=b",
            "--include-ignored",
            "--timeout=5",
            "x",
        ]
        .map(str::to_owned)
        .to_vec();
        let (o, rest) = super::take_options("test", &args).unwrap();
        assert!(o.exact);
        assert_eq!(o.skip, ["a", "b"]);
        assert_eq!(o.ignored, crate::test::Ignored::Include);
        assert_eq!(o.timeout, Some(5));
        assert_eq!(rest, ["x"]);
        assert_eq!(
            super::test_timeout(&o),
            Some(std::time::Duration::from_secs(5))
        );
        let none = RunOptions {
            timeout: Some(0),
            ..RunOptions::default()
        };
        assert_eq!(super::test_timeout(&none), None);
        let e = super::take_options("test", &["--timeout=1.5".to_owned()]).unwrap_err();
        assert!(e.contains("whole seconds"), "{e}");
    }

    /// A shell script stands in for the harness: `$1` is the case.
    #[cfg(unix)]
    const SCRIPT: &str = "runner sh\nrunner -c\nrunner case \"$1\" in ok) exit 0;; boom) echo 'panicked at x: boom' >&2; exit 101;; hang) sleep 5;; *) echo \"no $1\"; exit 1;; esac\n";

    #[cfg(unix)]
    fn run_script(cases: &str, o: &RunOptions) -> (i32, String) {
        let m = format!("version 2\nkind test\n{SCRIPT}exe /harness\n{cases}");
        let (mut out, mut human) = (Vec::new(), Vec::new());
        let code = run_manifest_to(&m, o, &mut out, &mut human);
        (code, String::from_utf8_lossy(&out).into_owned())
    }

    #[cfg(unix)]
    #[test]
    fn should_panic_cases() {
        let o = RunOptions::default();
        let (code, out) = run_script(
            "case p1\ncall boom\npanic\ncase p2\ncall boom\npanic boom\n",
            &o,
        );
        assert_eq!(code, 0, "{out}");
        let (code, out) = run_script("case p3\ncall boom\npanic other\n", &o);
        assert_eq!(code, 1);
        assert!(out.contains("does not contain `other`"), "{out}");
        let (code, out) = run_script("case p4\ncall ok\npanic\n", &o);
        assert_eq!(code, 1);
        assert!(out.contains("returned without panicking"), "{out}");
    }

    #[cfg(unix)]
    #[test]
    fn ignored_settled_and_selection() {
        let cases = "case ok\ncase slow\ncall nope\nignore takes long\ncase built\noutcome ok\n\
                     case broken\noutcome failed it built\n";
        let (code, out) = run_script(cases, &RunOptions::default());
        assert_eq!(code, 1);
        assert!(out.contains("test slow ... ignored, takes long"), "{out}");
        assert!(out.contains("test built ... ok"), "{out}");
        assert!(out.contains("(it built)"), "{out}");
        assert!(out.contains("2 passed; 1 failed; 1 ignored"), "{out}");
        let only = RunOptions {
            ignored: crate::test::Ignored::Only,
            ..RunOptions::default()
        };
        let (code, out) = run_script(cases, &only);
        assert_eq!(code, 1);
        assert!(
            out.contains("test slow ... FAILED") && out.contains("no nope"),
            "{out}"
        );
        let skip = RunOptions {
            skip: vec!["broken".to_owned()],
            exact: true,
            ..RunOptions::default()
        };
        let (code, out) = run_script(cases, &skip);
        assert_eq!(code, 0, "{out}");
        let list = RunOptions {
            list: true,
            json: true,
            ..RunOptions::default()
        };
        let (_, out) = run_script(cases, &list);
        assert!(out.contains("\"name\":\"slow\",\"ignored\":true"), "{out}");
    }

    #[cfg(unix)]
    #[test]
    fn timeout_kills() {
        let o = RunOptions {
            timeout: Some(1),
            json: true,
            ..RunOptions::default()
        };
        let t = std::time::Instant::now();
        let (code, out) = run_script("case hang\n", &o);
        assert_eq!(code, 1);
        assert!(t.elapsed() < std::time::Duration::from_secs(4));
        assert!(out.contains("\"result\":\"timeout\""), "{out}");
    }

    #[test]
    fn filter_regex() {
        assert!(re_match("add", "math::adds"));
        assert!(!re_match("sub", "math::adds"));
        assert!(re_match("^math::", "math::adds"));
        assert!(!re_match("^adds", "math::adds"));
        assert!(re_match("s$", "math::adds"));
        assert!(re_match("a.*d", "math::adds"));
        assert!(re_match("ad+s", "math::adds"));
        assert!(re_match("^x?math", "math::adds"));
        assert!(re_match("a\\.b", "a.b"));
        assert!(!re_match("a\\.b", "axb"));
    }
}
