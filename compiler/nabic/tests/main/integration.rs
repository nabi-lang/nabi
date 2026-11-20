//! End to end: `nabic build` each `tests/cases/*.nabi` (or the package
//! `tests/cases/<dir>/`, `src/main.nabi`), then run it. A case has
//! `// expect-exit: N` (optionally `// expect-stdout:`), or
//! `// expect-error: text` when it must fail to build. Each build and run
//! has a time limit (`case_timeout`); past it the case fails. Directives:
//! `common.rs`.
//!
//! `NABI_TEST_STAGE=<dir>`: a case that would run is built and checked as
//! usual but not run: its program goes to `<dir>/bin/<name>` and what its
//! runs expect to `<dir>/cases/<name>.json`, for a device to run
//! (`targets/android`, `targets/ios`).

use crate::common::*;
use std::fs;
#[cfg(unix)]
use std::os::unix::process::ExitStatusExt as _;
use std::path::PathBuf;

#[test]
fn end_to_end_cases() {
    let env = setup();
    // Without `..`: nabic shows a path under its directory relative to it.
    let cases_dir = nabic_core::real_path(workspace().join("tests/cases")).unwrap();
    let scratch = scratch_root(&env.workspace).join(format!(
        "integration_tmp{}{}",
        opt_suffix(),
        test_target().unwrap_or_default()
    ));

    let mut cases: Vec<(String, PathBuf)> = fs::read_dir(&cases_dir)
        .unwrap()
        .filter_map(|e| {
            let p = e.ok()?.path();
            if p.is_dir() {
                let main = p.join("src/main.nabi");
                let name = p.file_name()?.to_string_lossy().into_owned();
                main.exists().then_some((name, main))
            } else if p.extension().is_some_and(|x| x == "nabi") {
                Some((p.file_stem()?.to_string_lossy().into_owned(), p))
            } else {
                None
            }
        })
        .collect();
    cases.sort();
    // `a,b`: a case runs when its name contains any of them. CI sets the
    // variable empty for the whole corpus.
    let filter = std::env::var("NABI_TEST_FILTER").unwrap_or_default();
    let parts: Vec<&str> = filter
        .split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    if !parts.is_empty() {
        cases.retain(|(name, _)| parts.iter().any(|p| name.contains(p)));
    }
    assert!(
        !cases.is_empty(),
        "NABI_TEST_FILTER={filter:?} selects no case"
    );
    let cases = shard(cases);

    let results = par_map(&cases, |(name, path)| {
        let r = run_case(&env, name, path, &scratch);
        if r.is_ok() {
            println!("ok    {name}");
            // Only failures keep their outputs (CI disks are small).
            let _ = fs::remove_dir_all(scratch.join(name));
        }
        r.map_err(|e| format!("{name}: {e}"))
    });
    let ran = results.iter().filter(|r| matches!(r, Ok(true))).count();
    let failures: Vec<String> = results.into_iter().filter_map(Result::err).collect();
    assert!(
        failures.is_empty(),
        "{} of {} integration cases failed:\n  {}",
        failures.len(),
        cases.len(),
        failures.join("\n  ")
    );
    println!("{ran} of {} integration cases ran", cases.len());
    if parts.is_empty() {
        let floor = MIN_CASES / shard_count();
        assert!(
            ran >= floor,
            "only {ran} integration cases ran, want at least {floor}; \
             check tests/cases and the skip directives, or lower MIN_CASES if the corpus shrank"
        );
    }
}

/// Fewest cases the whole corpus must run on any target, so a broken
/// checkout, case discovery or skip directive cannot pass with a handful.
/// The corpus has about 2300 cases and at most about 350 skip on one
/// target; shard `i/n` must run `MIN_CASES / n`.
const MIN_CASES: usize = 1600;

/// The n of `NABI_TEST_SHARD=i/n`, 1 when unset. `shard` already rejected a
/// malformed spec.
fn shard_count() -> usize {
    std::env::var("NABI_TEST_SHARD")
        .ok()
        .and_then(|s| s.split_once('/')?.1.parse().ok())
        .unwrap_or(1)
}

/// Build and run one case; `Ok(false)` when it does not apply to this target.
fn run_case(
    env: &Env,
    name: &str,
    path: &PathBuf,
    scratch: &std::path::Path,
) -> Result<bool, String> {
    let source = fs::read_to_string(path).unwrap();
    if skipped_on_target(&source) {
        return Ok(false);
    }
    let case_dir = scratch.join(name);
    let _ = fs::create_dir_all(&case_dir);
    let plug = directive(&source, "wasm-plug:").map(str::trim);
    let wac = std::env::var_os("NABI_TEST_WAC").filter(|w| !w.is_empty());
    if plug.is_some() && wac.is_none() {
        return Ok(false);
    }
    let mut exe = case_dir.join(exe_name("out"));

    let build = nabic_build(env, path, &exe).map_err(|e| format!("nabic: {e}"))?;
    let stderr = String::from_utf8_lossy(&build.stderr);
    if parse_expect_error(&source).is_some() {
        if build.status.success() {
            return Err("build succeeded but `// expect-error:` expects failure".into());
        }
        return check_expect_error(&source, &stderr).map(|()| true);
    }
    check_lints(env, &source, path, &case_dir, &stderr)?;
    let Some(expected) = parse_expect_exit(&source) else {
        return Err("missing `// expect-exit: N` (or `// expect-error:`) directive".into());
    };
    if !build.status.success() {
        return Err(format!(
            "nabic build failed: {}",
            build_failure(build.status, &stderr)
        ));
    }
    if let (Some(plug), Some(wac)) = (plug, wac) {
        // Relative to the case's package directory, above `src/`.
        let dir = path
            .parent()
            .and_then(std::path::Path::parent)
            .unwrap_or_else(|| std::path::Path::new("."));
        let plugged = case_dir.join(exe_name("plug"));
        let b = nabic_build(env, &dir.join(plug), &plugged).map_err(|e| format!("nabic: {e}"))?;
        if !b.status.success() {
            return Err(format!(
                "plug build failed: {}",
                String::from_utf8_lossy(&b.stderr)
            ));
        }
        let composed = case_dir.join(exe_name("composed"));
        let mut c = std::process::Command::new(wac);
        c.arg("plug")
            .arg(&exe)
            .arg("--plug")
            .arg(&plugged)
            .arg("-o")
            .arg(&composed);
        let c = output_within(&mut c, case_timeout(&source, Step::Build))
            .map_err(|e| format!("wac: {e}"))?;
        if !c.status.success() {
            return Err(format!(
                "wac plug failed: {}",
                String::from_utf8_lossy(&c.stderr)
            ));
        }
        exe = composed;
    }
    for want in directives(&source, "expect-warning:") {
        if !stderr.contains(want) {
            return Err(format!(
                "build stderr lacks warning {want:?}\n    stderr: {stderr:?}"
            ));
        }
    }

    for unwanted in directives(&source, "forbid-warning:") {
        if stderr.contains(unwanted) {
            return Err(format!(
                "build stderr has warning {unwanted:?}\n    stderr: {stderr:?}"
            ));
        }
    }

    if let Some(stage) = stage_dir() {
        stage_case(&source, name, &exe, expected, &stage)?;
        return Ok(true);
    }
    // A reactor has no `run`: only its exports are called.
    let reactor = directives(&source, "wasm-reactor").next().is_some();
    if !reactor {
        check_run(&source, &exe, expected, &[])?;
    }
    for inv in directives(&source, "wasm-invoke:") {
        let (call, want) = inv
            .split_once(" => ")
            .ok_or("`// wasm-invoke: call => value`")?;
        let run = output_within(
            &mut invoke_command(&exe, call.trim()),
            case_timeout(&source, Step::Run),
        )
        .map_err(|e| format!("--invoke {call}: {e}"))?;
        let got = String::from_utf8_lossy(&run.stdout);
        if !run.status.success() || got.trim_end() != want.trim() {
            return Err(format!(
                "--invoke {call}: want {want:?}, got {got:?} ({})\n    stderr: {}",
                run.status,
                String::from_utf8_lossy(&run.stderr)
            ));
        }
    }
    // Again with a collection every N allocations, each full one
    // evacuating, then with concurrent marking too.
    if let Some(n) = directive(&source, "gc-stress:") {
        let env = [("NABIR_GC_STRESS", n.trim()), ("NABIR_GC_EVAC", "always")];
        check_run(&source, &exe, expected, &env).map_err(|e| format!("gc-stress: {e}"))?;
        let env = [
            ("NABIR_GC_STRESS", n.trim()),
            ("NABIR_GC_EVAC", "always"),
            ("NABIR_GC_CONCURRENT", "always"),
        ];
        check_run(&source, &exe, expected, &env)
            .map_err(|e| format!("gc-stress concurrent: {e}"))?;
    }
    Ok(true)
}

/// `NABI_TEST_STAGE`: where the programs go instead of running.
fn stage_dir() -> Option<PathBuf> {
    std::env::var_os("NABI_TEST_STAGE")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
}

/// Copy `exe` to `<stage>/bin/<name>` and write `<stage>/cases/<name>.json`:
/// the exit code, stdout and stderr patterns `check_run` would check, the
/// run's limit in seconds, the prefix to cut from library panic locations,
/// the case's `needs:` and one environment per run (the gc-stress runs
/// after the first).
fn stage_case(
    source: &str,
    name: &str,
    exe: &std::path::Path,
    expected: i32,
    stage: &std::path::Path,
) -> Result<(), String> {
    let bin = stage.join("bin");
    let cases = stage.join("cases");
    fs::create_dir_all(&bin)
        .and_then(|()| fs::create_dir_all(&cases))
        .map_err(|e| format!("stage: {e}"))?;
    fs::copy(exe, bin.join(name)).map_err(|e| format!("stage {}: {e}", exe.display()))?;
    let own: Vec<(&str, &str)> = directives(source, "run-env:")
        .filter_map(|d| d.split_once('='))
        .collect();
    let mut runs = vec![own.clone()];
    if let Some(n) = directive(source, "gc-stress:") {
        let gc = [("NABIR_GC_STRESS", n.trim()), ("NABIR_GC_EVAC", "always")];
        runs.push(own.iter().copied().chain(gc).collect());
        let cc = [("NABIR_GC_CONCURRENT", "always")];
        runs.push(own.iter().copied().chain(gc).chain(cc).collect());
    }
    let stdout = parse_expect_stdout(source).ok_or("missing `// expect-stdout:` directive")?;
    let stderr: Vec<String> = directives(source, "expect-stderr:").map(json).collect();
    let runs: Vec<String> = runs
        .iter()
        .map(|env| {
            let kv: Vec<String> = env
                .iter()
                .map(|(k, v)| format!("{}:{}", json(k), json(v)))
                .collect();
            format!("{{{}}}", kv.join(","))
        })
        .collect();
    let needs: Vec<String> = directive(source, "needs:")
        .unwrap_or_default()
        .split_whitespace()
        .map(json)
        .collect();
    let root = nabic_core::real_path(workspace()).unwrap();
    let root = format!("{}/", root.display()).replace('\\', "/");
    let doc = format!(
        "{{\"name\":{},\"exit\":{expected},\"stdout\":{},\"stderr\":[{}],\
         \"timeout\":{},\"root\":{},\"needs\":[{}],\"runs\":[{}]}}\n",
        json(name),
        json(&stdout),
        stderr.join(","),
        case_timeout(source, Step::Run).as_secs(),
        json(&root),
        needs.join(","),
        runs.join(",")
    );
    fs::write(cases.join(format!("{name}.json")), doc).map_err(|e| format!("stage: {e}"))
}

/// `s` as a JSON string.
fn json(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if u32::from(c) < 0x20 => out.push_str(&format!("\\u{:04x}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// `// expect-lint:`: the `E0131` diagnostics of `nabic check`, as
/// `line:col headline`, are exactly the directives, and so are those
/// the build printed (`build_err`).
fn check_lints(
    env: &Env,
    source: &str,
    path: &std::path::Path,
    dir: &std::path::Path,
    build_err: &str,
) -> Result<(), String> {
    let want: Vec<&str> = directives(source, "expect-lint:").collect();
    if want.is_empty() {
        return Ok(());
    }
    let out =
        nabic_check(env, path, &dir.join("check")).map_err(|e| format!("nabic check: {e}"))?;
    let got = lint_lines(&String::from_utf8_lossy(&out.stderr));
    if got != want {
        return Err(format!("lints mismatch\n  want: {want:?}\n  got:  {got:?}"));
    }
    let built = lint_lines(build_err);
    if built != want {
        return Err(format!(
            "build lints mismatch\n  want: {want:?}\n  got:  {built:?}"
        ));
    }
    Ok(())
}

/// The `E0131` diagnostics of `err` as `line:col headline`.
fn lint_lines(err: &str) -> Vec<String> {
    let mut got = Vec::new();
    let mut lines = err.lines();
    while let Some(l) = lines.next() {
        let Some(msg) = l
            .strip_prefix("warning[E0131]: ")
            .or_else(|| l.strip_prefix("error[E0131]: "))
        else {
            continue;
        };
        let at = lines
            .next()
            .and_then(|w| w.rsplit(':').nth(1).zip(w.rsplit(':').next()))
            .map(|(line, col)| format!("{line}:{col}"))
            .unwrap_or_default();
        got.push(format!("{at} {msg}"));
    }
    got
}

/// What a failed build said: its first error (`error[..]`, a driver's
/// `nabic: error`, a panic with its message), else its last line, else
/// how it ended.
fn build_failure(st: std::process::ExitStatus, stderr: &str) -> String {
    let lines: Vec<&str> = stderr.lines().collect();
    for (i, l) in lines.iter().enumerate() {
        if l.starts_with("thread '") && l.contains("panicked at") {
            let msg = lines.get(i + 1).map_or("", |m| m.trim());
            return format!("{l} {msg}").trim_end().to_owned();
        }
        if l.starts_with("error") || l.contains(": error") {
            return (*l).to_owned();
        }
    }
    if let Some(l) = lines.iter().rev().find(|l| !l.trim().is_empty()) {
        return l.trim().to_owned();
    }
    match exit_code(st) {
        Some(n) if st.code().is_none() => format!("killed by signal {}", n - 128),
        Some(n) => format!("exit {n} with no output"),
        None => format!("{st} with no output"),
    }
}

/// The exit code, or as a shell reports it a death by signal `n`: 128 + `n`
/// (an `abort` is 134).
fn exit_code(st: std::process::ExitStatus) -> Option<i32> {
    #[cfg(unix)]
    {
        st.code().or_else(|| st.signal().map(|n| 128 + n))
    }
    #[cfg(not(unix))]
    st.code()
}

/// Run `exe` (with the case's `run-env` and `env`) and check its exit
/// code, stderr and stdout against the case's directives.
fn check_run(
    source: &str,
    exe: &std::path::Path,
    expected: i32,
    env: &[(&str, &str)],
) -> Result<(), String> {
    // `// run-env: KEY=VALUE`: the case's own environment.
    let own = directives(source, "run-env:").filter_map(|d| d.split_once('='));
    let mut c = run_command(exe);
    c.envs(own).envs(env.iter().copied());
    let run = output_within(&mut c, case_timeout(source, Step::Run))?;
    let exit = exit_code(run.status).ok_or("no exit code")?;
    if !exit_matches(exit, expected) {
        // The tail of stderr tells a crash from a runner's failure.
        let err = String::from_utf8_lossy(&run.stderr);
        let mut tail: Vec<&str> = err.lines().rev().take(20).collect();
        tail.reverse();
        return Err(format!(
            "exit={exit} (expected {expected})\n    stderr: {}",
            tail.join("\n      ")
        ));
    }
    // Library panic locations are absolute: relative to the workspace.
    let root = nabic_core::real_path(workspace()).unwrap();
    let root = format!("{}/", root.display()).replace('\\', "/");
    let got = String::from_utf8_lossy(&run.stderr)
        .replace('\\', "/")
        .replace(&root, "");
    // wasmtime's report of a preview 1 exit code of 126 or more (run as
    // 1, `exit_matches`) is not the program's.
    let got = match got.split_once("Error: failed to run main module") {
        Some((program, _)) if exit != expected => program.to_owned(),
        _ => got,
    };
    // wasmtime's notice on `-S threads` (wasm32-wasip1-threads) and the
    // blank padding around it are not the program's.
    let got = match got.split_once('\n') {
        Some((first, rest)) if first.starts_with("WARNING: the `-Sthreads` flag") => {
            rest.trim().to_owned()
        }
        _ => got,
    };
    let mut got: Vec<&str> = got.lines().map(str::trim).collect();
    // qemu-user's report of the signal that ended the guest is not the
    // program's: one last line, only under a runner.
    if std::env::var_os("NABI_TEST_RUNNER").is_some()
        && got
            .last()
            .is_some_and(|l| l.starts_with("qemu: uncaught target signal "))
    {
        got.pop();
    }
    let want: Vec<&str> = directives(source, "expect-stderr:").collect();
    if got.len() != want.len() || !got.iter().zip(&want).all(|(g, w)| glob(w, g)) {
        return Err(format!(
            "stderr mismatch\n  want: {want:?}\n  got:  {got:?}"
        ));
    }
    let want = parse_expect_stdout(source).ok_or("missing `// expect-stdout:` directive")?;
    let got = String::from_utf8_lossy(&run.stdout);
    if got.trim_end() != want.trim_end() {
        return Err(format!(
            "stdout mismatch\n  want: {want:?}\n  got:  {got:?}"
        ));
    }
    Ok(())
}

/// `line` matches `pat`, where each `*` stands for any text (a library
/// panic's `line:column`, which moves as the library changes).
fn glob(pat: &str, line: &str) -> bool {
    let mut parts = pat.split('*');
    let first = parts.next().unwrap_or_default();
    let Some(mut rest) = line.strip_prefix(first) else {
        return false;
    };
    let mut parts: Vec<&str> = parts.collect();
    let Some(last) = parts.pop() else {
        return rest.is_empty();
    };
    for p in parts {
        match rest.find(p) {
            Some(i) => rest = &rest[i + p.len()..],
            None => return false,
        }
    }
    rest.ends_with(last)
}

/// A case that never ends fails at its limit instead of holding the job.
#[test]
fn hung_case_times_out() {
    let env = setup();
    let dir = scratch("hung");
    let src = dir.join("hung.nabi");
    put(&src, "fn main() {\n    loop {}\n}\n").unwrap();
    let exe = dir.join(exe_name("hung"));
    let build = nabic_build(&env, &src, &exe).unwrap();
    assert_exit(&build, 0);
    let limit = case_timeout_in(Some("1"), "", Step::Run);
    assert_eq!(limit, std::time::Duration::from_secs(1));
    let start = std::time::Instant::now();
    let got = output_within(&mut run_command(&exe), limit);
    let msg = got.err().unwrap_or_default();
    assert!(msg.starts_with("timed out after 1s"), "{msg}");
    // Linux names the threads it found waiting.
    if cfg!(target_os = "linux") {
        assert!(msg.contains("\n    thread "), "{msg}");
    }
    assert!(start.elapsed() < std::time::Duration::from_secs(30));
}

#[cfg(unix)]
#[test]
fn build_failure_says_why() {
    let st = |code: i32| {
        std::process::Command::new("sh")
            .args(["-c", &format!("exit {code}")])
            .status()
            .unwrap()
    };
    let out = "   Compiling app (tests/cases/x)\nnabic: error cannot create /x: denied\n";
    assert_eq!(
        build_failure(st(1), out),
        "nabic: error cannot create /x: denied"
    );
    let out = "error[E0001]: bad\n --> a.nabi:1:1\n";
    assert_eq!(build_failure(st(1), out), "error[E0001]: bad");
    let out = "thread 'main' panicked at x.rs:1:1:\nindex out of bounds\n\
               nabic: internal compiler error\n";
    assert_eq!(
        build_failure(st(70), out),
        "thread 'main' panicked at x.rs:1:1: index out of bounds"
    );
    assert_eq!(build_failure(st(1), "note: a\n\n"), "note: a");
    assert_eq!(build_failure(st(1), ""), "exit 1 with no output");
    let killed = std::process::Command::new("sh")
        .args(["-c", "kill -9 $$"])
        .status()
        .unwrap();
    assert_eq!(build_failure(killed, ""), "killed by signal 9");
}

#[test]
fn case_timeout_sources() {
    let d = std::time::Duration::from_secs;
    assert_eq!(case_timeout_in(None, "", Step::Build), d(600));
    assert_eq!(case_timeout_in(None, "", Step::Run), d(300));
    assert_eq!(case_timeout_in(Some("900"), "", Step::Run), d(900));
    assert_eq!(case_timeout_in(Some(""), "", Step::Run), d(300));
    assert_eq!(
        case_timeout_in(Some("900"), "// timeout: 5\n", Step::Build),
        d(5)
    );
}
