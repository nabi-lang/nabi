//! `nabic`, the nabi compiler: its command line, building, checking and
//! the test / bench harness builds. The `nabic` binary runs [`main`];
//! `nabi` parses the same options and builds with these functions when it
//! runs a tool (`run`, `repl`, `doc`, ..) in-process. Arguments are parsed
//! by hand: the workspace takes no external crates.
//!
//! Exit codes: 0 ok, 1 user error (compile error / failed test),
//! 2 usage error, 70 internal error.

mod args;
mod build;
mod check;
mod doctest;
mod harness;
mod runner;
mod xcframework;

#[allow(clippy::wildcard_imports)]
pub use crate::args::*;
#[allow(clippy::wildcard_imports)]
pub use crate::build::*;
#[allow(clippy::wildcard_imports)]
pub use crate::check::*;
#[allow(clippy::wildcard_imports)]
pub use crate::harness::*;
#[allow(clippy::wildcard_imports)]
pub use crate::runner::*;

use nabic_core::cli::{self, NABIC_COMMANDS, NABIC_USAGE as USAGE};
use nabic_core::term::{self, ColorChoice, Role, Stream};
use nabic_interface::target_cpu as cpu;
use nabic_interface::{
    BuildOptions, CompileResult, DepBuild, EmitKind, Fingerprint, Package, Session, SourceMap,
    TargetSpec, print_rendered,
};
use nabic_tools::lint::Level;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process;
use std::time::Instant;

// One flag per CLI switch; an enum per group would only add plumbing.
#[allow(clippy::struct_excessive_bools)]
#[derive(Default)]
pub struct Args {
    pub cmd: String,
    pub positional: Vec<String>,
    pub output: Option<PathBuf>,
    pub emit: Vec<EmitKind>,
    pub bootstrap: Vec<PathBuf>,
    pub externs: Vec<(String, PathBuf)>,
    /// `--extern-feature <name>=<f>`: `(code name, feature)`.
    pub extern_features: Vec<(String, String)>,
    /// `--extern-dep <package>=<name>`: `(dependent, dependency)`, `pkg`
    /// the package being built.
    pub extern_deps: Vec<(String, String)>,
    /// `--name`: a library build's package name.
    pub name: Option<String>,
    pub target_dir: Option<PathBuf>,
    pub cfg: Vec<String>,
    /// `--print <what>` (`cfg`, `target-list`, `target-spec`), then exit.
    pub print: Option<String>,
    /// `--explain <code>`: print the code's explanation, then exit.
    pub explain: Option<String>,
    /// `--target`: a built-in target or a `<name>.nabion` file; `None` is
    /// the host.
    pub target: Option<String>,
    /// The resolved `--target`, for `target_cpu` / `target_features`.
    pub spec: Option<TargetSpec>,
    /// `-C target-cpu`.
    pub target_cpu: Option<String>,
    /// `-C target-feature`, each `+f` / `-f`.
    pub target_features: Vec<String>,
    pub opts: BuildOptions,
    /// After `--`: the program's arguments as given (any bytes), or
    /// bindgen's C compiler arguments.
    pub run_args: Vec<OsString>,
    pub verbose: u8,
    pub quiet: bool,
    pub json: bool,
    pub color: ColorChoice,
    pub check: bool,
    pub check_library: bool,
    /// `fix --allow-dirty`: fix files with uncommitted changes too.
    pub allow_dirty: bool,
    /// `doc --std`: document the standard library tiers too.
    pub std: bool,
    /// `doc --std` without a path: the standard library alone.
    pub std_only: bool,
    pub diff: bool,
    pub lint_levels: nabic_tools::lint::Levels,
    pub list: bool,
    /// `inspect --dump`: the whole NABIHIR as text.
    pub dump: bool,
    /// `jupyter --connection-file`.
    pub connection_file: Option<PathBuf>,
    /// `doc --open`.
    pub open: bool,
    /// `--emit-harness <file>`: write the test / bench harness manifest
    /// (built executables and their cases) to `<file>` for the `nabi`
    /// runner.
    pub emit_harness: Option<PathBuf>,
    /// `bindgen --c-shim <file>`: C wrappers of the header's `static
    /// inline` functions go to <file>, the bindings call them.
    pub c_shim: Option<PathBuf>,
    /// `bindgen -x objective-c`: read the header as Objective-C.
    pub objc: bool,
    /// `test --doc` (`Some(true)`: only the doc tests) / `--no-doc`
    /// (`Some(false)`: none); `None`: both kinds.
    pub doc_tests: Option<bool>,
    /// An `-O` level or `--release` was given.
    pub opt_set: bool,
    /// `-C overflow-checks[=yes|no]`, which beats the manifest's profiles.
    pub overflow_checks: Option<bool>,
    /// From `nabi`: the manifest's profiles that turn overflow checks off.
    pub no_overflow_checks: Vec<String>,
    /// `--crate-type=cdylib`: a library package also as a C-ABI shared
    /// library.
    pub cdylib: bool,
    /// `--crate-type=staticlib`: a library package also as one static
    /// library C code links alone (`static/lib<name>.a`), with its header
    /// and module map.
    pub staticlib: bool,
    /// `--xcframework` (with `staticlib`): also put the library into
    /// `<profile>/<name>.xcframework`, one slice per Apple target built.
    pub xcframework: bool,
    /// `--crate-type=plugin`: a library package also as a plugin a Nabi
    /// program loads (`std::plugin`).
    pub plugin: bool,
    /// `--bundle <dep>`: dependencies a plugin links in.
    pub bundle: Vec<String>,
    /// `--hot`: a hot-reload plugin.
    pub hot: bool,
    /// `--hot-base <file>`: the live version's `.layout`.
    pub hot_base: Option<PathBuf>,
    /// `--hot-dep <dep>`: dependencies that are hot packages too.
    pub hot_deps: Vec<String>,
}

impl Args {
    #[must_use]
    pub const fn spec(&self) -> &TargetSpec {
        self.spec
            .as_ref()
            .expect("the target is resolved with the arguments")
    }
}

/// The version, and the commit and its date when built from git.
pub const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), env!("NABI_COMMIT_INFO"));

/// The command being run, once known, for usage errors.
static CMD: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// When the command started: `Finished` counts the standard library build.
static STARTED: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

pub fn started() -> Instant {
    *STARTED.get_or_init(Instant::now)
}

/// Every pass recurses over the tree (the parser caps depth), so run on a
/// thread sized for that cap in debug builds; it's only address space.
const COMPILER_STACK: usize = 1 << 30;

/// `nabic`'s command line.
pub fn main() {
    in_compiler(real_main);
}

/// Run `f` as the compiler runs: on a thread with the compiler's stack,
/// an internal error (a panic) exiting 70, apart from a program panic's
/// 101.
pub fn in_compiler(f: impl FnOnce() + Send + 'static) {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        nabic_interface::end_if_output_closed(info);
        default_hook(info);
        let tool = cli::invoked_as();
        let what = "internal compiler error";
        eprintln!(
            "{}",
            nabic_core::ice_report(tool, what, VERSION, env!("NABI_HOST"))
        );
        process::exit(nabic_core::ICE_EXIT);
    }));
    std::thread::Builder::new()
        .name("main".into())
        .stack_size(COMPILER_STACK)
        .spawn(f)
        .expect("spawn compiler thread")
        .join()
        .unwrap_or_else(|e| std::panic::resume_unwind(e));
}

fn real_main() {
    started();
    let (raw, tail) = cli::os_args().unwrap_or_else(|e| usage_error(&e));
    let a = parse_args(raw, tail);
    term::set_choice(a.color);
    nabic_interface::set_report_depth(a.json, term::depth(Stream::Stderr));
    if let Some(code) = &a.explain {
        match nabic_interface::explain(code) {
            Ok(text) => print!("{text}"),
            Err(e) => option_error(
                e,
                "a code is written `E0040`, `e0040` or `40`: `--explain E0040`",
            ),
        }
        return;
    }
    match a.print.as_deref() {
        None | Some("cfg") => {}
        Some("target-list") => {
            for t in nabic_interface::available_targets() {
                println!("{t}");
            }
            return;
        }
        Some("target-spec") => {
            println!("{}", a.spec().to_nabion());
            return;
        }
        Some(what @ ("lldbinit" | "gdbinit" | "natvis")) => {
            let sys = nabic_interface::sysroot();
            match what {
                "lldbinit" => sys
                    .lldb
                    .map(|p| format!("command script import '{}'", p.display())),
                "gdbinit" => sys.gdb.map(|p| format!("source {}", p.display())),
                _ => sys.natvis.map(|p| p.display().to_string()),
            }
            .map_or_else(|e| fail(&e), |line| println!("{line}"));
            return;
        }
        Some(what) => option_error(
            format!(
                "`--print` takes `cfg`, `target-list`, `target-spec`, `lldbinit`, `gdbinit` or \
                 `natvis`, not `{what}`"
            ),
            "write `--print cfg`, `--print target-list`, `--print target-spec`, \
             `--print lldbinit`, `--print gdbinit` or `--print natvis`",
        ),
    }
    if a.print.is_some() {
        let mut session = Session::new();
        for c in &a.cfg {
            session.add_cfg(c.clone());
        }
        if a.opts.opt_level == "0" {
            session.add_cfg("debug_assertions".to_owned());
        }
        session.set_target(a.spec());
        for line in session.cfg_lines() {
            println!("{line}");
        }
        return;
    }
    // Commands of one `[path]` (`test` / `bench`: and a filter).
    let most = match a.cmd.as_str() {
        "build" | "emit" | "check" => 1,
        "test" | "bench" => 2,
        _ => usize::MAX,
    };
    if let Some(extra) = a.positional.get(most) {
        let takes = if most == 1 {
            "one path"
        } else {
            "a path and a filter"
        };
        usage_error(&format!(
            "unexpected argument `{extra}`: `{}` takes {takes}",
            a.cmd
        ));
    }
    match a.cmd.as_str() {
        "build" => {
            // Before looking for the target's runtime: a plugin target
            // error says what to do instead.
            if a.plugin {
                build::plugin_target(&a);
            }
            cmd_build(with_defaults(a));
        }
        "emit" => {
            let mut a = with_defaults(a);
            if a.emit.is_empty() {
                a.emit.push(EmitKind::Mir);
            }
            cmd_build(a);
        }
        "check" => cmd_check(with_defaults(a), true),
        "test" => cmd_harness(with_defaults(a), Mode::Test),
        "bench" => cmd_harness(with_defaults(a), Mode::Bench),
        "version" => {
            println!("nabic {}", VERSION);
            if a.verbose > 0 {
                let rows =
                    [("host:", env!("NABI_HOST"))].map(|(k, v)| (k.to_owned(), v.to_owned()));
                print!("{}", term::list(&rows, None, ""));
            }
        }
        "help" => match a.positional.first() {
            Some(c) => print!("{}", command_help(c)),
            None => print!("{USAGE}"),
        },
        other => usage_error(&format!(
            "unknown command `{other}`{}",
            suggest_command(other)
        )),
    }
}

/// Find the library roots (`nabi/core`) and `libnabir.a`
/// without flags.
#[must_use]
pub fn with_defaults(mut a: Args) -> Args {
    let sys = nabic_interface::sysroot();
    if a.bootstrap.is_empty() {
        match sys.libs {
            Ok(roots) => a.bootstrap = roots,
            // The language server still starts.
            Err(e) if a.cmd == "lsp" => {
                eprintln!(
                    "{}: {} {e}",
                    cli::invoked_as(),
                    term::label(Stream::Stderr, Role::Warning)
                );
            }
            Err(e) => fail(&e),
        }
    }
    a.bootstrap = nabic_interface::bootstrap_tiers(&a.bootstrap);
    let links = matches!(
        a.cmd.as_str(),
        "run" | "test" | "bench" | "repl" | "jupyter"
    ) || (a.cmd == "build" && a.emit.is_empty());
    let t = a.spec().clone();
    let name = &t.name;
    // The host runtime is the sysroot's; other targets' (and their libc,
    // when shipped) are in their directory.
    let dir = sys.targets.join(name);
    let missing = || {
        format!(
            "no runtime for {name}: {} is missing (this toolchain does not ship the \
             target; in the repository, run targets/build.sh {name})",
            dir.display()
        )
    };
    if a.opts.nabir == "nabir" {
        let nabir = if t.is_host() {
            sys.nabir
        } else {
            Some(dir.join(t.runtime_lib()))
                .filter(|p| p.is_file())
                .ok_or_else(missing)
        };
        match nabir {
            Ok(lib) => a.opts.nabir = lib.display().to_string(),
            Err(e) if links => fail(&e),
            Err(_) => {}
        }
    }
    if t.libc.shipped() {
        if dir.is_dir() {
            a.opts.libc = Some(dir.clone());
        } else if links {
            fail(&missing());
        }
    }
    // Android: the NDK's crt objects and stub libraries.
    if t.libc == nabic_target::Libc::Bionic {
        match nabic_target::android::lib_dir(&t) {
            Ok(d) => a.opts.libc = Some(d),
            Err(e) if links => fail(&e),
            Err(_) => {}
        }
    }
    let codegen = links || a.cmd == "build";
    if codegen {
        for w in inert_cpu_options(&a, &t) {
            eprintln!(
                "{}: {} {w}",
                cli::invoked_as(),
                term::label(Stream::Stderr, Role::Warning)
            );
        }
    }
    a
}

/// What of `-C target-cpu` / `-C target-feature` changes no code on
/// target `t` (the options applied): one line each.
fn inert_cpu_options(a: &Args, t: &TargetSpec) -> Vec<String> {
    let used = cpu::used(&t.arch);
    let list = if used.is_empty() {
        "none".to_owned()
    } else {
        used.join(", ")
    };
    let now = t.cpu_features();
    let flags: Vec<&str> = t.features.split(',').filter(|f| !f.is_empty()).collect();
    let mine = a.target_features.len().min(flags.len());
    let first = flags.len() - mine;
    let mut out = Vec::new();
    for (j, f) in a.target_features.iter().enumerate() {
        // The code without this one option.
        let rest: Vec<&str> = flags
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != first + j)
            .map(|(_, f)| *f)
            .collect();
        let without = cpu::features(&t.arch, &t.cpu, &rest.join(","));
        if cpu::same_code(&t.arch, &now, &without) {
            out.push(format!(
                "-C target-feature={f} changes no code on {} (the features the native backend \
                 uses there: {list})",
                t.arch
            ));
        }
    }
    if let Some(c) = &a.target_cpu {
        let base = TargetSpec::resolve(a.target.as_deref()).map_or_else(
            |_| now.clone(),
            |b| {
                // The baseline with the feature options alone.
                let fs: Vec<&str> = std::iter::once(b.features.as_str())
                    .chain(flags[first..].iter().copied())
                    .collect();
                cpu::features(&b.arch, &b.cpu, &fs.join(","))
            },
        );
        if cpu::same_code(&t.arch, &now, &base) {
            out.push(format!(
                "-C target-cpu={c} changes no code on {}: it adds none of the features the \
                 native backend uses there ({list})",
                t.arch
            ));
        }
    }
    out
}
