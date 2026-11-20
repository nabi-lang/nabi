//! `nabi`, the toolchain front: manifests and locks, dependency
//! resolution, registry packages and the git cache, environments, toolchains,
//! scaffolding and the Jupyter kernelspec. It runs the `nabic` beside it
//! for the compiler commands with resolved deps as `--extern name=lib.nabi`,
//! and the tools (`run`, `repl`, `lsp`, `doc`, `fmt`, ..) itself, with the
//! compiler linked in; a pin naming another toolchain hands the whole
//! command to that toolchain's `nabi`.
//!
//! ```text
//! $NABI_HOME/bin/{nabi,nabic,nabils}     proxies to the active toolchain, copies of the newest's nabi
//! $NABI_HOME/bin/<name>               an executable `nabi install` built
//! $NABI_HOME/installs.nabion           those executables and their sources
//! $NABI_HOME/toolchains/<version>-<triple>/
//!                                    a toolchain: bin/{nabi,nabic,nabils}, lib/
//! $NABI_HOME/cache/git/               shared git cache (see `git`)
//! $NABI_HOME/cache/registry/          registry index clones (see `registry`), source archives (`archive`)
//! $NABI_HOME/cache/native/            native packages, by SHA-256 (see `native`)
//! $NABI_HOME/components.nabion         optional parts `nabi component` added (see `component`)
//! <toolchain>/lib/nabi/packages/      the official packages a release carries (see `bundle`)
//! $NABI_HOME/config.nabion             `Config { toolchain: "<default>" }`
//! $NABI_HOME/tokens.nabion             publish tokens, per index
//! $NABI_HOME/sources.nabion            index mirrors (see `registry`)
//! $NABI_HOME/cache/scripts/<hash>/    a script's lock and prebuilt dependencies
//! $NABI_HOME/envs/<name>/             a named environment (nabi.nabion, nabi.lock, target/)
//! $NABI_HOME/envs/.current            `nabi env use`'s choice
//! ```

mod add;
mod cc;
mod component;
mod delegate;
mod deps;
mod env;
mod help;
mod install;
mod members;
mod new;
mod official;
mod publish;
mod system;
mod tools;

pub use crate::add::*;
#[allow(clippy::wildcard_imports)]
pub(crate) use crate::delegate::*;
#[allow(clippy::wildcard_imports)]
pub(crate) use crate::deps::*;
#[allow(clippy::wildcard_imports)]
pub(crate) use crate::install::*;
#[allow(clippy::wildcard_imports)]
pub(crate) use crate::new::*;
#[allow(clippy::wildcard_imports)]
pub(crate) use crate::publish::*;
pub use nabi_manifest::{VERSION, home};
pub use nabi_toolchain::VERSION_LINE;
pub use toolchain::{proxy, take_guard};

use manifest::{Dep, GitRef, MANIFEST, Manifest, NativeSource, Source};
use nabi_manifest::{
    VERBOSE, canonical_edit, lock, manifest, package_dir, semver, skipped, warn_once, workspace,
};
use nabi_resolve::{archive, bundle, git, native, registry, resolve};
use nabi_toolchain::{VALUE_OPTIONS, cache, child, exec, toolchain};
use nabic_core::cli::{self, NABIC_USAGE, looks_like_script};
use nabic_core::completions::{Spec, Values, section_flags};
use nabic_core::term::{self, ColorChoice, Role, Stream};
use nabic_core::tmp::TempEntry;
use nabic_diagnostic::Json;
use nabic_lexer::TokenKind as T;
use nabic_nabion::{Kind, Value};
use resolve::{Context, Options, Resolved};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Clone, Default)]
#[allow(clippy::struct_excessive_bools)] // independent switches
struct Global {
    env: Option<String>,
    offline: bool,
    locked: bool,
    /// `--frozen`: `--locked` and `--offline` together.
    frozen: bool,
    quiet: bool,
    verbose: bool,
    /// `+<version>`.
    toolchain: Option<String>,
    /// `--color=<when>`, passed on to nabic.
    color: Option<String>,
    /// The program's arguments after `--` (or a script's path) as given:
    /// the strings in `post` stand for them only lossily.
    tail: Vec<std::ffi::OsString>,
    /// The arguments and directory nabi started with, for `handoff`.
    argv: Vec<std::ffi::OsString>,
    start: Option<PathBuf>,
}

/// Run `nabi` with `args` (without the program name); the exit code.
pub fn main_with(args: Vec<String>) -> i32 {
    main_with_os(args.into_iter().map(Into::into).collect())
}

/// [`main_with`] of the arguments as the system gives them: a program's
/// arguments (after `--`, or after a script's path) may be any bytes,
/// everything before them must be UTF-8.
pub fn main_with_os(args: Vec<std::ffi::OsString>) -> i32 {
    nabic_core::tmp::sweep_once(&["nabi-install-", "nabi-prebuild-"]);
    let mut g = Global {
        argv: args.clone(),
        start: std::env::current_dir().ok(),
        ..Global::default()
    };
    let mut rest = Vec::new();
    let total = args.len();
    let mut it = args.into_iter();
    // The next argument as a string; the error names its position.
    let next = |it: &mut std::vec::IntoIter<std::ffi::OsString>| {
        let a = it.next()?;
        Some(cli::utf8_arg(total - it.len(), a).map_err(|e| usage(&e)))
    };
    // The rest as given, and as strings for `post`.
    let take_tail = |g: &mut Global, it: &mut std::vec::IntoIter<std::ffi::OsString>| {
        g.tail = it.collect();
        g.tail
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect::<Vec<String>>()
    };
    // nabi's own options, anywhere before `--`; the rest goes to nabic.
    while let Some(a) = next(&mut it) {
        let a = match a {
            Ok(a) => a,
            Err(code) => return code,
        };
        let (name, inline) = match a.split_once('=') {
            Some((n, v)) if n.starts_with("--") => (n.to_owned(), Some(v.to_owned())),
            _ => (a.clone(), None),
        };
        let value = |it: &mut std::vec::IntoIter<std::ffi::OsString>| match &inline {
            Some(v) => Ok(Some(v.clone())),
            None => next(it).transpose(),
        };
        match name.as_str() {
            // `nabi cc ..`: everything after it is the C compiler's (gcc's
            // options, which nabi's would take for its own).
            "cc" if rest.iter().all(|r: &String| r.starts_with('-')) => {
                let post = take_tail(&mut g, &mut it);
                return run(&g, "cc", &rest, &post);
            }
            "--" => {
                rest.push(a);
                rest.extend(take_tail(&mut g, &mut it));
            }
            "--env" => match value(&mut it) {
                Ok(Some(v)) => g.env = Some(v),
                Ok(None) => return usage("`--env` needs a name"),
                Err(code) => return code,
            },
            "--no-color" | "--color" => {
                let when = if name == "--color" {
                    match value(&mut it) {
                        Ok(v) => v,
                        Err(code) => return code,
                    }
                } else {
                    Some("never".to_owned())
                };
                let Some(when) = when else {
                    return usage("`--color` needs a value");
                };
                let Some(c) = ColorChoice::parse(&when) else {
                    return usage(&format!("`--color` takes auto|always|never, not `{when}`"));
                };
                term::set_choice(c);
                g.color = Some(format!("--color={when}"));
            }
            "--offline" => g.offline = true,
            "--locked" => g.locked = true,
            "--frozen" => {
                g.frozen = true;
                g.locked = true;
                g.offline = true;
            }
            "--cwd" => {
                let dir = match value(&mut it) {
                    Ok(Some(dir)) => dir,
                    Ok(None) => return usage("`--cwd` needs a directory"),
                    Err(code) => return code,
                };
                if let Err(e) = std::env::set_current_dir(&dir) {
                    return usage(&format!("--cwd {dir}: {e}"));
                }
            }
            "-q" | "--quiet" => {
                g.quiet = true;
                rest.push(a);
            }
            "-v" | "--verbose" => {
                g.verbose = true;
                rest.push(a);
            }
            // `nabi +<version> ..`, before the command.
            t if t.len() > 1 && t.starts_with('+') && rest.iter().all(|r| r.starts_with('-')) => {
                g.toolchain = Some(t[1..].to_owned());
            }
            // `nabi <file> [args..]` is `nabi run -q <file> -- args..`; a path
            // has a `/` or ends in `.nabi`, which no command does.
            p if looks_like_script(p)
                && rest.iter().all(|r| r.starts_with('-'))
                && rest
                    .last()
                    .is_none_or(|l| !VALUE_OPTIONS.contains(&l.as_str())) =>
            {
                let mut post = vec![a, "-q".to_owned(), "--".to_owned()];
                post.extend(take_tail(&mut g, &mut it));
                return run(&g, "run", &rest, &post);
            }
            _ => rest.push(a),
        }
    }
    // `--explain <code>` takes no command.
    let explain = rest
        .iter()
        .take_while(|a| *a != "--")
        .enumerate()
        .find_map(|(j, a)| match a.strip_prefix("--explain") {
            Some("") => Some(rest.get(j + 1).cloned().unwrap_or_default()),
            Some(v) => v.strip_prefix('=').map(str::to_owned),
            None => None,
        });
    if let Some(code) = explain {
        return match nabic_diagnostic::explain(&code) {
            Ok(text) => {
                print!("{text}");
                0
            }
            Err(e) => usage(&e),
        };
    }
    // The command is the first non-option word; nabic options with a
    // separate value go after it. `--version --format json` has no
    // command, so `json` there is the value.
    let is_version = |a: &String| a == "-V" || a == "--version";
    let Some(i) = (0..rest.len()).find(|&j| {
        let a = &rest[j];
        (!a.starts_with('-') || a == "--")
            && !(j > 0 && rest[j - 1] == "--format" && rest[..j].iter().any(is_version))
    }) else {
        if rest.iter().any(is_version) {
            return match json_format(&rest, "version") {
                Ok(json) => {
                    print!("{}", version(&g, json));
                    0
                }
                Err(e) => usage(&e),
            };
        }
        if rest.iter().any(|a| a == "-h" || a == "--help") {
            print!("{}", help::short_usage());
            return 0;
        }
        // Bare `nabi` (options only) is `nabi repl`, like `python` / `julia`.
        return run(&g, "repl", &rest, &[]);
    };
    if rest[i] == "--" {
        return run(&g, "repl", &rest[..i], &rest[i..]);
    }
    let (cmd, post) = (&rest[i], &rest[i + 1..]);
    let before_dashes = post.iter().take_while(|a| *a != "--");
    if before_dashes
        .into_iter()
        .any(|a| a == "-h" || a == "--help")
    {
        return help(cmd);
    }
    // `-q` / `-v` were taken above; nabic and the tools take them again,
    // nabi's own commands do not.
    let to_nabic =
        on_package(cmd) && !(cmd == "jupyter" && post.first().is_some_and(|p| p == "install"));
    let post: Vec<String> = if to_nabic {
        post.to_vec()
    } else {
        let end = post.iter().position(|a| a == "--").unwrap_or(post.len());
        let (opts, after) = post.split_at(end);
        opts.iter()
            .filter(|a| !matches!(a.as_str(), "-q" | "--quiet" | "-v" | "--verbose"))
            .chain(after)
            .cloned()
            .collect()
    };
    if let Some(code) = members::dispatch(cmd, &post, &g.argv, g.quiet) {
        return code;
    }
    run(&g, cmd, &rest[..i], &post)
}

/// `nabi --version`; with `-v`, which binary and toolchain that is, as
/// `key: value` lines like `nabic --version -v`. `json`: one object.
fn version(g: &Global, json: bool) -> String {
    let rows = if g.verbose {
        toolchain::describe(g.toolchain.as_deref()).to_vec()
    } else {
        Vec::new()
    };
    if json {
        let mut fields = vec![
            (
                "schema".to_owned(),
                Json::num(nabic_diagnostic::RECORD_SCHEMA),
            ),
            ("version".to_owned(), Json::str(env!("CARGO_PKG_VERSION"))),
            ("line".to_owned(), Json::str(VERSION_LINE)),
        ];
        fields.extend(rows.into_iter().map(|(k, v)| (k.to_owned(), Json::Str(v))));
        return format!("{}\n", Json::Obj(fields));
    }
    let rows: Vec<(String, String)> = rows
        .into_iter()
        .map(|(k, v)| (format!("{k}:"), v))
        .collect();
    format!("nabi {VERSION_LINE}\n{}", term::list(&rows, None, ""))
}

/// `--format json` / `--format=json` in `args` (before `--`); an error
/// for any other format.
fn json_format(args: &[String], cmd: &str) -> Result<bool, String> {
    let mut it = args.iter().take_while(|a| *a != "--");
    let mut json = false;
    while let Some(a) = it.next() {
        let v = match a.strip_prefix("--format") {
            Some("") => it.next().map(String::as_str),
            Some(v) if v.starts_with('=') => Some(&v[1..]),
            _ => continue,
        };
        json = match v {
            Some("json") => true,
            Some("human") => false,
            Some(v) => {
                return Err(format!(
                    "`--format` takes human or json, not `{v}`: write `nabi {cmd} --format json`"
                ));
            }
            None => return Err("`--format` needs a value".to_owned()),
        };
    }
    Ok(json)
}

/// The commands `nabi` passes to `nabic`.
const DELEGATED: [&str; 5] = ["build", "check", "test", "bench", "emit"];

/// Commands that take nabic's options: the compiler's and the tools.
fn on_package(cmd: &str) -> bool {
    DELEGATED.contains(&cmd) || tools::TOOLS.contains(&cmd)
}

/// `nabi jupyter` with no connection file: the user wants the kernel
/// added, not run.
pub(crate) const JUPYTER_BARE: &str =
    "Jupyter starts the kernel itself; to add it, run `nabi jupyter install`";

/// `nabi <cmd> --help` / `nabi help <cmd>`.
fn help(cmd: &str) -> i32 {
    match help::command(cmd) {
        Some(text) => {
            print!("{text}");
            0
        }
        None => usage(&unknown_command(cmd)),
    }
}

/// "unknown command" for `cmd`, with what to write instead.
fn unknown_command(cmd: &str) -> String {
    let names = help::all().map(|h| h.name).chain(["toolchain list"]);
    let hint = cli::did_you_mean(cmd, names, cli::CMD_SYNONYMS);
    format!("unknown command `{cmd}`{hint}")
}

/// Errors that are about the command line (exit code 2).
fn is_usage(e: &str) -> bool {
    [
        "usage:",
        "unknown option",
        "unexpected argument",
        "give at most one of",
    ]
    .iter()
    .any(|p| e.starts_with(p))
        || e.starts_with('`') && e.contains("` needs a value")
        || e.starts_with('`') && e.contains("` takes ")
}

fn run(g: &Global, cmd: &str, pre: &[String], post: &[String]) -> i32 {
    VERBOSE.store(g.verbose && !g.quiet, std::sync::atomic::Ordering::Relaxed);
    // A pin naming another toolchain: its own nabi runs the command, so nabi
    // and nabic always come from one toolchain. Commands that
    // run on a package decide in `invocation`, by the package they work on.
    if !matches!(
        cmd,
        "toolchain" | "help" | "version" | "completions" | "__complete" | "__official"
    ) && !on_package(cmd)
    {
        match toolchain::handoff(g.toolchain.as_deref(), toolchain::project_pin(), false) {
            Ok(Some(h)) => return toolchain::run_handoff(&h, &g.argv, g.start.as_deref()),
            Ok(None) => {}
            Err(e) => {
                eprintln!("nabi: {} {e}", term::label(Stream::Stderr, Role::Error));
                return 1;
            }
        }
    }
    let r = match cmd {
        "version" => json_format(post, cmd).map(|json| {
            print!("{}", version(g, json));
            0
        }),
        "help" if !post.is_empty() => return help(&post[0]),
        "help" => {
            print!("{}", help::short_usage());
            Ok(0)
        }
        "new" | "init" => cmd_new(g, cmd, post),
        "add" => cmd_add(g, post),
        "remove" => cmd_remove(g, post),
        "install" => cmd_install(g, post),
        "uninstall" => cmd_uninstall(g, post),
        "update" => cmd_update(g, post),
        "deps" => cmd_deps(g, post),
        "search" => cmd_search(g, post),
        "publish" => cmd_publish(g, post),
        "yank" => cmd_yank(g, post),
        "login" => cmd_login(g, post),
        "logout" => cmd_logout(g, post),
        "env" => env::cmd(g, post),
        "component" => component::cmd(g, post),
        "toolchain" => toolchain::cmd(post, g.quiet, g.verbose),
        "cache" => cache::cmd(post, g.quiet),
        "completions" => cmd_completions(post),
        "__complete" => complete(post),
        // The REPL asks before it adds an official package a cell uses.
        "__official" => {
            if let Some((name, req)) = post
                .first()
                .and_then(|n| official::requirement(n, g.offline))
            {
                println!("{name} {req}");
            }
            Ok(0)
        }
        "jupyter" if post.first().is_some_and(|p| p == "install") => install_kernel(g, &post[1..]),
        "jupyter" if post.is_empty() => return usage_of(cmd, JUPYTER_BARE),
        // Editors pass `--stdio`; stdio is the only transport.
        "lsp" => {
            let post: Vec<String> = post.iter().filter(|a| *a != "--stdio").cloned().collect();
            tool(g, cmd, pre, &post)
        }
        "test" | "bench" => harness(g, cmd, pre, post),
        "dev" => tools::cmd_dev(g, pre, post),
        "cc" => return cc(g),
        c if DELEGATED.contains(&c) => delegate(g, cmd, pre, post),
        c if tools::TOOLS.contains(&c) => tool(g, cmd, pre, post),
        other => return usage(&unknown_command(other)),
    };
    r.unwrap_or_else(|e| {
        if is_usage(&e) {
            return usage_of(cmd, &e);
        }
        // A rendered diagnostic (`error[E..]: ..`) as it is.
        if e.starts_with("error[") {
            eprintln!("{e}");
        } else {
            eprintln!("nabi: {} {e}", term::label(Stream::Stderr, Role::Error));
        }
        1
    })
}

/// `nabi cc`: this toolchain's `nabicc` with the arguments as given.
fn cc(g: &Global) -> i32 {
    // Help needs no nabicc.
    if matches!(g.tail.as_slice(), [h] if h == "-h" || h == "--help") {
        return help("cc");
    }
    let nabicc = match toolchain::tool_for("nabicc") {
        Ok(p) => p,
        Err(e) => {
            eprintln!("nabi: {} {e}", term::label(Stream::Stderr, Role::Error));
            return 1;
        }
    };
    let mut c = std::process::Command::new(&nabicc);
    // Messages then name `nabi cc`.
    c.args(&g.tail).env("__NABI_CLI", "nabi");
    exec(c, &nabicc)
}

fn usage(msg: &str) -> i32 {
    eprintln!("nabi: {msg}\nrun `nabi help` for usage");
    2
}

/// A usage error of command `cmd`: an unknown option gets the closest of
/// its options, and the pointer is to the command's help.
fn usage_of(cmd: &str, msg: &str) -> i32 {
    let mut msg = msg.to_owned();
    let unknown = msg
        .strip_prefix("unknown option `")
        .and_then(|r| r.strip_suffix('`'))
        .map(str::to_owned);
    if let Some(flag) = unknown {
        let spec = completion_spec();
        let sub = format!("{cmd} ");
        let cands: Vec<&str> = spec
            .flags
            .iter()
            .filter(|(k, _)| *k == cmd || k.starts_with(&sub))
            .flat_map(|(_, v)| v)
            .chain(&spec.global)
            .map(String::as_str)
            .collect();
        msg.push_str(&cli::did_you_mean(&flag, cands, &[]));
    }
    if help::find(cmd).is_none() {
        return usage(&msg);
    }
    eprintln!("nabi: {msg}\nrun `nabi help {cmd}` for usage");
    2
}

/// What nabi's command line takes, read from its help; the commands nabi
/// passes to nabic, and the tools, take nabic's flags too.
fn completion_spec() -> Spec {
    let cmds: Vec<&cli::CmdHelp> = help::all().collect();
    let mut s = Spec::from_commands("nabi", &cmds, help::GLOBAL);
    let nabic = cli::nabic_spec();
    for c in DELEGATED.iter().chain(&tools::TOOLS) {
        let mut flags = nabic.flags.get(*c).cloned().unwrap_or_default();
        for sec in help::find(c).map_or(&[][..], |h| h.sections) {
            flags.extend(section_flags(NABIC_USAGE, sec));
        }
        for (head, _) in help::runner_options(c).map_or(&[][..], |(_, rows)| rows) {
            flags.extend(head.split('=').next().map(str::to_owned));
        }
        flags.extend(nabic.global.iter().cloned());
        s.add_flags(c, &flags);
    }
    let features: Vec<String> = help::FEATURES
        .iter()
        .filter_map(|(head, _)| head.split(' ').next())
        .map(str::to_owned)
        .collect();
    for c in help::WITH_FEATURES {
        s.add_flags(c, &features);
    }
    let members: Vec<String> = ["-p", "--package", "--workspace"]
        .iter()
        .map(|f| (*f).to_owned())
        .collect();
    for c in workspace::COMMANDS {
        s.add_flags(c, &members);
    }
    let run = |c: &str| Values::Run(c.to_owned());
    let toolchains = run("nabi __complete toolchains");
    let envs = run("nabi __complete envs");
    s.values = vec![("+".to_owned(), toolchains.clone())];
    for k in ["toolchain default", "toolchain remove"] {
        s.values.push((k.to_owned(), toolchains.clone()));
    }
    for k in ["env use", "env remove", "env shell", "env export", "--env"] {
        s.values.push((k.to_owned(), envs.clone()));
    }
    s.values.extend([
        ("--target".to_owned(), run("nabic --print target-list")),
        ("help".to_owned(), Values::Commands),
    ]);
    s
}

/// `nabi completions <shell>`.
fn cmd_completions(args: &[String]) -> Result<i32, String> {
    let [shell] = args else {
        return Err("usage: nabi completions bash|zsh|fish|powershell".to_owned());
    };
    let script = completion_spec().render(shell).ok_or_else(|| {
        format!(
            "usage: nabi completions bash|zsh|fish|powershell (no completion script for `{shell}`)"
        )
    })?;
    print!("{script}");
    Ok(0)
}

/// `nabi __complete toolchains | envs`: the names the completion scripts
/// offer, one per line.
fn complete(args: &[String]) -> Result<i32, String> {
    let names = match args {
        [w] if w == "toolchains" => toolchain::versions(&home()),
        [w] if w == "envs" => env::names(),
        _ => return Err("usage: nabi __complete toolchains | envs".to_owned()),
    };
    for n in names {
        println!("{n}");
    }
    Ok(0)
}

/// The current package's directory and manifest.
fn this_package() -> Result<(PathBuf, Manifest), String> {
    let dir = package_dir(Path::new("."))
        .filter(|d| manifest::find(d).is_some())
        .ok_or("not inside a package with a nabi.nabion")?;
    let m = manifest::load(&dir.join(MANIFEST))?;
    Ok((dir, m))
}

/// Options with a value (`--f v` / `--f=v`), switches given, positionals.
type Flags = (Vec<(String, String)>, Vec<String>, Vec<String>);

/// `args` split by `valued` options and `switches`; others are errors.
fn flags(args: &[String], valued: &[&str], switches: &[&str]) -> Result<Flags, String> {
    let (mut vals, mut sw, mut pos) = (Vec::new(), Vec::new(), Vec::new());
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let (f, inline) = match a.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_owned())),
            _ => (a.as_str(), None),
        };
        if valued.contains(&f) {
            let v = inline
                .or_else(|| it.next().cloned())
                .ok_or_else(|| format!("`{f}` needs a value"))?;
            vals.push((f.to_owned(), v));
        } else if switches.contains(&f) {
            sw.push(f.to_owned());
        } else if f.starts_with('-') {
            return Err(format!("unknown option `{f}`"));
        } else {
            pos.push(a.clone());
        }
    }
    Ok((vals, sw, pos))
}

fn val<'a>(vals: &'a [(String, String)], f: &str) -> Option<&'a str> {
    vals.iter()
        .rev()
        .find(|(k, _)| k == f)
        .map(|(_, v)| v.as_str())
}
