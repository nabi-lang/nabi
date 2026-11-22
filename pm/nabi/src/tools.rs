//! The tools `nabi` runs itself, with the compiler linked in: `run`,
//! `dev`, `lint`, `fix`, `repl`, `jupyter`, `lsp`, `fmt`, `doc`, `clean`,
//! `bindgen`, `inspect` and `demangle`. They take nabic's options (`nabic::parse_args`)
//! and build with nabic's functions, in this process.

mod bindgen;
mod dev;
mod doc;
mod fix;
mod fmt;
mod inspect;
mod repl;
mod run;

pub(crate) use crate::tools::dev::cmd_dev;

use crate::tools::bindgen::cmd_bindgen;
use crate::tools::doc::cmd_doc;
use crate::tools::fix::cmd_fix;
use crate::tools::fmt::cmd_fmt;
use crate::tools::inspect::{cmd_demangle, cmd_inspect};
use crate::tools::repl::{cmd_jupyter, cmd_repl};
use crate::tools::run::cmd_run;
use nabic::{Args, cmd_check, discover_arg, fail, prebuild, usage_error, with_defaults};
use nabic_core::cli;
use nabic_core::term::{self, Role, Stream};
use nabils::Deps;
use std::ffi::OsString;
use std::path::Path;
use std::process;

/// The tools, for `nabi`'s dispatch.
pub(crate) const TOOLS: [&str; 12] = [
    "run", "lint", "fix", "repl", "jupyter", "lsp", "fmt", "doc", "clean", "bindgen", "inspect",
    "demangle",
];

/// Run tool `raw[..]`'s command (nabic's options, up to `--`), the
/// program's arguments `tail` after it; its exit code.
/// `lsp`: the language server's per-package dependencies.
pub(crate) fn run(raw: Vec<String>, tail: Vec<OsString>, lsp: Option<Deps>) -> i32 {
    nabic::in_compiler(move || {
        nabic::started();
        let a = nabic::parse_args(raw, tail);
        term::set_choice(a.color);
        nabic_interface::set_report_depth(a.json, term::depth(Stream::Stderr));
        if let Some(extra) = a.positional.get(1).filter(|_| takes_one(&a.cmd)) {
            usage_error(&format!(
                "unexpected argument `{extra}`: `{}` takes one path",
                a.cmd
            ));
        }
        dispatch(a, lsp);
    });
    0
}

/// Tools of one `[path]`.
fn takes_one(cmd: &str) -> bool {
    matches!(
        cmd,
        "run" | "lint" | "fix" | "doc" | "clean" | "repl" | "bindgen"
    )
}

fn dispatch(a: Args, lsp: Option<Deps>) {
    match a.cmd.as_str() {
        "run" => cmd_run(with_defaults(a)),
        "lint" if a.list => print!("{}", nabic_tools::lint::list()),
        "lint" => cmd_check(with_defaults(a), false),
        "fix" => cmd_fix(with_defaults(a)),
        "repl" => cmd_repl(with_defaults(a)),
        "jupyter" => cmd_jupyter(a),
        "lsp" => cmd_lsp(with_defaults(a), lsp),
        "fmt" => cmd_fmt(&a),
        "doc" => cmd_doc(with_defaults(a)),
        "clean" => cmd_clean(&a),
        "bindgen" => cmd_bindgen(&a),
        "inspect" => cmd_inspect(&a),
        "demangle" => cmd_demangle(&a),
        other => unreachable!("`{other}` is not a tool"),
    }
}

/// `nabi lsp`: the language server on stdio. The dependencies resolved
/// here serve the start directory's package and buffers outside every
/// package; `deps` resolves any other package's.
fn cmd_lsp(mut a: Args, deps: Option<Deps>) {
    let pkg = nabic_interface::discover(Path::new(".")).ok();
    // The server starts without the dependencies it cannot build.
    let built = match prebuild(&mut a, pkg.as_ref()) {
        Ok(_) => true,
        Err(e) => {
            eprintln!(
                "{}: {} {e}",
                cli::invoked_as(),
                term::label(Stream::Stderr, Role::Warning)
            );
            a.externs.clear();
            false
        }
    };
    let real = |p: &Path| std::fs::canonicalize(p).ok();
    let here = pkg
        .filter(|p| built && !p.anonymous)
        .and_then(|p| real(&p.dir));
    let mut seed = here.map(|d| {
        let own = nabils::Externs {
            externs: a.externs.clone(),
            deps: a.extern_deps.clone(),
        };
        (d, own)
    });
    let deps = deps.map(|mut deps| -> Deps {
        Box::new(
            move |dir: &Path| match seed.take_if(|(d, _)| real(dir).as_ref() == Some(d)) {
                Some((_, externs)) => Ok(externs),
                None => deps(dir),
            },
        )
    });
    let io = (
        std::io::BufReader::new(std::io::stdin()),
        std::io::stdout().lock(),
    );
    process::exit(nabils::serve_with(
        io.0,
        io.1,
        a.bootstrap,
        a.externs,
        a.lint_levels,
        deps,
    ));
}

/// `nabi clean [path]`: remove the package's target/.
fn cmd_clean(a: &Args) {
    let pkg = discover_arg(a);
    let target = a.target_dir.clone().unwrap_or_else(|| pkg.target_dir());
    match std::fs::remove_dir_all(&target) {
        Ok(()) if !a.quiet => term::status("Removed", &target.display().to_string()),
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => fail(&format!("cannot remove {}: {e}", target.display())),
    }
}
