//! The `nabi test` runner.
//!
//! Each `#[test]` runs in its own process so a panic or crash fails only
//! that test; a thread pool runs them in parallel. The compiler-built
//! executable runs the named test when it is its only argument and exits
//! non-zero if it fails. A test that outlives its timeout is killed.

use crate::{command, count, ms, re_match, record};
use nabic_diagnostic::Json;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, ExitStatus, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// A case in a test harness and what passing means for it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Case {
    pub name: String,
    /// What the executable is given to run it; `None`: the name.
    pub call: Option<String>,
    /// `#[ignore]`: run only with `--ignored` / `--include-ignored`; the
    /// reason, maybe empty.
    pub ignore: Option<String>,
    /// `#[should_panic]`: passes only on a panic whose output holds the
    /// text (empty: any panic).
    pub panics: Option<String>,
    /// Settled when the harness was built (a compile-only doc test).
    pub outcome: Option<Settled>,
}

/// A case's end known before any run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Settled {
    Passed,
    /// Why.
    Failed(String),
    /// Why; even when ignored tests run.
    Ignored(String),
}

impl Case {
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Self::default()
        }
    }
}

/// Which `#[ignore]` tests run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Ignored {
    /// Reported as ignored, not run.
    #[default]
    Skip,
    /// `--ignored`: only those.
    Only,
    /// `--include-ignored`: those too.
    Include,
}

#[derive(Clone, Copy, Default)]
pub struct Options<'a> {
    pub filter: Option<&'a str>,
    /// `--exact`: the filter (and `--skip`) must equal the whole name.
    pub exact: bool,
    /// `--skip <pat>`: leave out the tests it matches.
    pub skip: &'a [String],
    pub ignored: Ignored,
    pub threads: usize,
    pub show_output: bool,
    /// A test running longer is killed and fails; `None`: no limit.
    pub timeout: Option<Duration>,
    /// What runs the executable (a non-host target's); empty: itself.
    pub runner: &'a [String],
}

impl Options<'_> {
    /// Whether the filter and `--skip` keep `name`.
    #[must_use]
    pub fn keeps(&self, name: &str) -> bool {
        let hit = |p: &str| {
            if self.exact {
                p == name
            } else {
                re_match(p, name)
            }
        };
        self.filter.is_none_or(hit) && !self.skip.iter().any(|s| hit(s))
    }

    /// Whether `case` is listed or run, past the filters.
    #[must_use]
    pub fn selects(&self, case: &Case) -> bool {
        self.keeps(&case.name) && (self.ignored != Ignored::Only || case.ignore.is_some())
    }
}

/// How a test ended.
#[derive(Clone, Debug, PartialEq, Eq)]
enum End {
    Passed,
    /// Why, for the failures list (empty: its output says it).
    Failed(String),
    TimedOut(Duration),
    Ignored(String),
}

/// A test, how it ended, its output and how long it took.
struct Ran<'a> {
    case: &'a Case,
    end: End,
    output: Vec<u8>,
    took: Duration,
}

/// Run `tests` from `exe`, each in its own process; whether all passed.
/// Progress and the summary are written to `out`.
pub fn run(exe: &Path, tests: &[Case], opts: &Options<'_>, out: &mut dyn Write) -> bool {
    run_to(exe, tests, opts, out, None)
}

/// [`run`]; with `records`, also a `format json` record per test and a
/// summary there.
pub fn run_to(
    exe: &Path,
    tests: &[Case],
    opts: &Options<'_>,
    out: &mut dyn Write,
    mut records: Option<&mut (dyn Write + '_)>,
) -> bool {
    let started = Instant::now();
    let selected: Vec<&Case> = tests.iter().filter(|t| opts.selects(t)).collect();
    let filtered_out = tests.len() - selected.len();
    let _ = writeln!(
        out,
        "\nrunning {} test{}",
        selected.len(),
        if selected.len() == 1 { "" } else { "s" }
    );

    // Each test runs in its own process across a thread pool; the threads
    // only collect results, and reporting stays single-threaded (`out` is
    // not shared) so a test prints once it and the ones before it finish.
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<Ran<'_>>> = Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for _ in 0..opts.threads.clamp(1, selected.len().max(1)) {
            s.spawn(|| {
                while let Some(&case) = selected.get(next.fetch_add(1, Ordering::Relaxed)) {
                    let r = run_one(exe, case, opts);
                    results
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(r);
                }
            });
        }
    });
    let mut results = results.into_inner().unwrap_or_else(PoisonError::into_inner);
    results.sort_by(|a, b| natural(&a.case.name, &b.case.name));

    for r in &results {
        let shown = match &r.end {
            End::Passed => "ok".to_owned(),
            End::Ignored(why) if why.is_empty() => "ignored".to_owned(),
            End::Ignored(why) => format!("ignored, {why}"),
            End::Failed(_) | End::TimedOut(_) => "FAILED".to_owned(),
        };
        let _ = writeln!(out, "test {} ... {shown}", r.case.name);
        if let Some(rec) = records.as_deref_mut() {
            let (result, why) = match &r.end {
                End::Passed => ("ok", String::new()),
                End::Failed(why) => ("failed", why.clone()),
                End::TimedOut(t) => ("timeout", timed_out(*t)),
                End::Ignored(why) => ("ignored", why.clone()),
            };
            let mut fields = vec![
                ("name", Json::str(r.case.name.as_str())),
                ("result", Json::str(result)),
                ("ms", ms(r.took)),
            ];
            if !why.is_empty() {
                fields.push(("reason", Json::str(why.as_str())));
            }
            record(rec, "case", "test", fields);
        }
    }
    let failed: Vec<_> = results
        .iter()
        .filter(|r| matches!(r.end, End::Failed(_) | End::TimedOut(_)))
        .collect();
    let ignored = results
        .iter()
        .filter(|r| matches!(r.end, End::Ignored(_)))
        .count();
    let passed = results.len() - failed.len() - ignored;
    if opts.show_output {
        let ok: Vec<_> = results.iter().filter(|r| r.end == End::Passed).collect();
        section(out, "successes", &ok);
    }
    section(out, "failures", &failed);
    let _ = writeln!(
        out,
        "\ntest result: {}. {passed} passed; {} failed; {ignored} ignored; {filtered_out} filtered out; finished in {:.2}s\n",
        if failed.is_empty() { "ok" } else { "FAILED" },
        failed.len(),
        started.elapsed().as_secs_f64()
    );
    if let Some(rec) = records {
        let result = if failed.is_empty() { "ok" } else { "failed" };
        record(
            rec,
            "summary",
            "test",
            vec![
                ("result", Json::str(result)),
                ("passed", count(passed)),
                ("failed", count(failed.len())),
                ("ignored", count(ignored)),
                ("filtered_out", count(filtered_out)),
                ("ms", ms(started.elapsed())),
            ],
        );
    }
    failed.is_empty()
}

/// Names in order, digit runs by value: `doc x:9` before `doc x:10`.
fn natural(left: &str, right: &str) -> std::cmp::Ordering {
    /// The name as text pieces and numbers.
    fn pieces(s: &str) -> Vec<(bool, &str)> {
        let mut out = Vec::new();
        let mut rest = s;
        while let Some(first) = rest.chars().next() {
            let digit = first.is_ascii_digit();
            let len = rest
                .find(|c: char| c.is_ascii_digit() != digit)
                .unwrap_or(rest.len());
            out.push((digit, &rest[..len]));
            rest = &rest[len..];
        }
        out
    }
    let key = |(digit, text): &(bool, &str)| {
        let value = if *digit {
            text.trim_start_matches('0')
        } else {
            text
        };
        (!*digit, value.len() * usize::from(*digit), value.to_owned())
    };
    let (l, r) = (pieces(left), pieces(right));
    l.iter().map(key).cmp(r.iter().map(key))
}

fn timed_out(t: Duration) -> String {
    format!(
        "timed out after {}s and was killed; raise the limit with `--timeout=SECS` (0: none)",
        t.as_secs()
    )
}

fn run_one<'a>(exe: &Path, case: &'a Case, opts: &Options<'_>) -> Ran<'a> {
    let t = Instant::now();
    let done = |end, output| Ran {
        case,
        end,
        output,
        took: t.elapsed(),
    };
    if let Some(why) = case
        .ignore
        .as_ref()
        .filter(|_| opts.ignored == Ignored::Skip)
    {
        return done(End::Ignored(why.clone()), Vec::new());
    }
    if let Some(s) = &case.outcome {
        let end = match s {
            Settled::Passed => End::Passed,
            Settled::Failed(why) => End::Failed(why.clone()),
            Settled::Ignored(why) => End::Ignored(why.clone()),
        };
        return done(end, Vec::new());
    }
    let mut c = command(opts.runner, exe);
    c.arg(case.call.as_deref().unwrap_or(&case.name));
    nabic_core::strip_handoffs(&mut c);
    c.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let cannot = |e: std::io::Error| End::Failed(format!("cannot run {}: {e}", exe.display()));
    let child = match c.spawn() {
        Ok(ch) => ch,
        Err(e) => return done(cannot(e), Vec::new()),
    };
    match wait(child, opts.timeout) {
        Err(e) => done(cannot(e), Vec::new()),
        Ok((None, output)) => done(End::TimedOut(opts.timeout.unwrap_or_default()), output),
        Ok((Some(status), output)) => done(judge(case, status, &output), output),
    }
}

/// Whether the process `status` with `output` passes `case`.
fn judge(case: &Case, status: ExitStatus, output: &[u8]) -> End {
    let Some(want) = &case.panics else {
        return if status.success() {
            End::Passed
        } else {
            End::Failed(describe(status))
        };
    };
    if status.code() != Some(101) {
        return End::Failed(if status.success() {
            "`#[should_panic]`: the test returned without panicking".to_owned()
        } else {
            format!(
                "`#[should_panic]`: it ended with {} instead of a panic",
                describe(status)
            )
        });
    }
    if String::from_utf8_lossy(output).contains(want.as_str()) {
        End::Passed
    } else {
        End::Failed(format!(
            "`#[should_panic]`: it panicked, but its message does not contain `{want}`"
        ))
    }
}

type Waited = (Option<ExitStatus>, Vec<u8>);

/// Wait for `child`, collecting its stdout and stderr in the order they
/// came; no status when it ran past `timeout` and was killed.
fn wait(mut child: Child, timeout: Option<Duration>) -> std::io::Result<Waited> {
    let buf = Arc::new(Mutex::new(Vec::new()));
    let pipes: [Option<Box<dyn Read + Send>>; 2] = [
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    ];
    let readers: Vec<_> = pipes
        .into_iter()
        .flatten()
        .map(|mut p| {
            let buf = Arc::clone(&buf);
            std::thread::spawn(move || {
                let mut chunk = [0u8; 8192];
                while let Ok(n @ 1..) = p.read(&mut chunk) {
                    buf.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .extend_from_slice(&chunk[..n]);
                }
            })
        })
        .collect();
    let take = |buf: &Mutex<Vec<u8>>| {
        std::mem::take(&mut *buf.lock().unwrap_or_else(PoisonError::into_inner))
    };
    let Some(limit) = timeout else {
        let status = child.wait()?;
        readers.into_iter().for_each(|r| drop(r.join()));
        return Ok((Some(status), take(&buf)));
    };
    let start = Instant::now();
    let mut nap = Duration::from_micros(100);
    loop {
        if let Some(status) = child.try_wait()? {
            readers.into_iter().for_each(|r| drop(r.join()));
            return Ok((Some(status), take(&buf)));
        }
        if start.elapsed() >= limit {
            let _ = child.kill();
            let _ = child.wait();
            // A process it started may still hold the pipes: keep what
            // came so far and leave the readers.
            return Ok((None, take(&buf)));
        }
        std::thread::sleep(nap);
        nap = (nap * 2).min(Duration::from_millis(10));
    }
}

fn section(out: &mut dyn Write, title: &str, rows: &[&Ran<'_>]) {
    if rows.is_empty() {
        return;
    }
    let _ = writeln!(out, "\n{title}:");
    for r in rows {
        let _ = writeln!(out, "\n---- {} ----", r.case.name);
        let _ = out.write_all(&r.output);
        match &r.end {
            End::Failed(why) if !why.is_empty() => {
                let _ = writeln!(out, "({why})");
            }
            End::TimedOut(t) => {
                let _ = writeln!(out, "({})", timed_out(*t));
            }
            _ => {}
        }
    }
    let _ = writeln!(out, "\n{title}:");
    for r in rows {
        let _ = writeln!(out, "    {}", r.case.name);
    }
}

fn describe(status: ExitStatus) -> String {
    #[cfg(unix)]
    if let Some(sig) = std::os::unix::process::ExitStatusExt::signal(&status) {
        return format!("killed by signal {sig}");
    }
    match status.code() {
        Some(101) => "panicked (exit 101)".to_owned(),
        Some(c) => format!("exit {c}"),
        None => "no exit code".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::natural;
    use std::cmp::Ordering::{Greater, Less};

    #[test]
    fn natural_order() {
        assert_eq!(natural("doc a:9", "doc a:10"), Less);
        assert_eq!(natural("doc a:10", "doc a:9 (x)"), Greater);
        assert_eq!(natural("adds", "booms"), Less);
        assert_eq!(natural("t2", "t02"), std::cmp::Ordering::Equal);
    }
}
