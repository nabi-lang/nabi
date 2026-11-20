//! `nabi repl` and the Jupyter kernel: sessions whose cells are compiled
//! and run.

use nabic::{Args, VERSION, discover_at, fail, lib_name, prebuild, usage_error, with_defaults};
use nabic_interface::Package;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process;

pub(crate) fn cmd_repl(mut a: Args) {
    // Standard input is the cell input.
    if a.positional.first().is_some_and(|p| p == "-") {
        usage_error("`nabi repl -`: standard input is the cell input; give a path or none");
    }
    let pkg = a.positional.first().map(|p| discover_at(&a, p));
    let (prelude, program) = repl_load(&mut a, pkg.as_ref());
    let here = nabic_interface::discover(Path::new(".")).ok();
    let deps = prebuild(&mut a, pkg.as_ref().or(here.as_ref())).unwrap_or_else(|e| fail(&e));
    // In a package it does not load, the session hints at its names.
    let package = here
        .filter(|_| pkg.is_none())
        .and_then(|h| h.root.parent().map(Path::to_path_buf));
    let interactive = std::io::stdin().is_terminal();
    let start = nabirepl::Start {
        prelude: prelude.as_deref(),
        program: program.as_ref().map(|(p, anon)| (p.as_path(), *anon)),
        package,
        banner: (interactive && std::io::stdout().is_terminal() && !a.quiet).then(banner),
    };
    process::exit(nabirepl::main(
        a.bootstrap,
        a.externs,
        deps,
        a.opts,
        interactive,
        start,
    ));
}

/// The REPL's and kernel's first line: the `--version` text and the host.
pub(crate) fn banner() -> String {
    format!("Nabi {VERSION} on {}", env!("NABI_HOST"))
}

/// What a session on `pkg` loads: a library becomes a session dependency
/// with `use <name>::*;` as the first cell; a program is loaded,
/// with its package's library as a dependency.
pub(crate) fn repl_load(
    a: &mut Args,
    pkg: Option<&Package>,
) -> (Option<String>, Option<(PathBuf, bool)>) {
    let Some(p) = pkg else {
        return (None, None);
    };
    if let Some(lib) = p.library.clone() {
        a.externs.push((lib_name(p), lib));
    }
    if p.lib {
        let name = lib_name(p);
        a.externs.push((name.clone(), p.root.clone()));
        return (Some(format!("use {name}::*;")), None);
    }
    (None, Some((p.root.clone(), p.anonymous)))
}

/// `nabi jupyter --connection-file <file>`: the kernel Jupyter starts.
pub(crate) fn cmd_jupyter(mut a: Args) {
    let Some(file) = a.connection_file.take() else {
        usage_error(crate::JUPYTER_BARE);
    };
    let mut a = with_defaults(a);
    // The kernel loads the package it starts in (`nabi repl .`).
    let pkg = nabic_interface::discover(Path::new("."))
        .ok()
        .filter(|p| !p.anonymous);
    let (prelude, program) = repl_load(&mut a, pkg.as_ref());
    let deps = prebuild(&mut a, pkg.as_ref()).unwrap_or_else(|e| fail(&e));
    let start = nabirepl::Start {
        prelude: prelude.as_deref(),
        program: program.as_ref().map(|(p, anon)| (p.as_path(), *anon)),
        package: None,
        banner: Some(banner()),
    };
    process::exit(nabirepl_jupyter::serve(
        &file,
        a.bootstrap,
        a.externs,
        deps,
        a.opts,
        start,
    ));
}
