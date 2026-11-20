//! `nabils`: the nabi language server on stdio, the same as `nabi lsp`
//! (each file with its package's dependencies). It runs the `nabi` beside
//! it, which resolves them and serves.

#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::process::Command;

const VERSION_LINE: &str = concat!(env!("CARGO_PKG_VERSION"), env!("NABI_COMMIT_INFO"));

fn main() {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    match args.first().and_then(|a| a.to_str()) {
        Some("-V" | "--version") => println!("nabils {VERSION_LINE}"),
        Some("-h" | "--help") => print!(
            "usage: nabils [nabi options]\n\nThe nabi language server on stdin / stdout (LSP), \
             checking each file with the\ndependencies of its package; the same as `nabi lsp`. \
             Editors start it.\n"
        ),
        _ => {
            let nabi = std::env::current_exe()
                .unwrap_or_default()
                .with_file_name(format!("nabi{}", std::env::consts::EXE_SUFFIX));
            let mut c = Command::new(&nabi);
            c.arg("lsp").args(args);
            #[cfg(unix)]
            let e = c.exec();
            #[cfg(not(unix))]
            let e = match c.status() {
                Ok(s) => std::process::exit(s.code().unwrap_or(1)),
                Err(e) => e,
            };
            eprintln!("nabils: cannot run {}: {e}", nabi.display());
            std::process::exit(1);
        }
    }
}
