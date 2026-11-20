//! `nabi doc`: HTML API docs.

use nabic::{Args, Mode, compile, discover_arg, fail, fail_on, finished, prebuild};
use nabic_core::cli;
use nabic_core::term::{self, Role, Stream};
use nabic_interface::{SourceMap, TempDir};
use std::path::{Path, PathBuf};
use std::process::{self, Command};

/// `nabi doc [path]`: HTML into `target/doc/` (see `nabic_tools::doc`).
pub(crate) fn cmd_doc(mut a: Args) {
    // `--std` alone: an empty package that only mounts the library.
    let std_only = a.std && a.positional.is_empty();
    a.std_only = std_only;
    let tmp = std_only.then(|| fail_on(TempDir::new(a.opts.keep_temps), &SourceMap::new()));
    if let Some(t) = &tmp {
        let main = t.path().join("nabi/src/main.nabi");
        std::fs::create_dir_all(t.path().join("nabi/src"))
            .and_then(|()| std::fs::write(&main, ""))
            .unwrap_or_else(|e| fail(&format!("cannot write {}: {e}", main.display())));
        a.positional.push(main.display().to_string());
        a.name.get_or_insert_with(|| "nabi".to_owned());
    }
    // A package with both root files: its library.
    let pkg = discover_arg(&a);
    let pkg = pkg.library_target().unwrap_or(pkg);
    prebuild(&mut a, Some(&pkg)).unwrap_or_else(|e| fail(&e));
    let (result, _, started) = compile(&a, &pkg, Mode::Doc);
    let out = a
        .target_dir
        .clone()
        .unwrap_or_else(|| {
            if std_only {
                PathBuf::from("target")
            } else {
                pkg.target_dir()
            }
        })
        .join("doc");
    // The version shown beside the package's name; none without a manifest.
    let version = crate::manifest::load(&pkg.dir.join(crate::manifest::MANIFEST))
        .ok()
        .and_then(|m| m.version)
        .unwrap_or_default();
    let docs = nabic_tools::doc::Scope {
        user: !std_only,
        std: a.std,
        version,
    };
    // The package's root page: `target/doc/<pkg>/index.html`.
    let page = nabic_tools::doc::generate(&result, &pkg.name, &out, &docs)
        .unwrap_or_else(|e| fail(&format!("cannot write {}: {e}", out.display())));
    drop(tmp);
    finished(&a, started);
    if !a.quiet {
        term::status("Generated", &page.display().to_string());
    }
    if a.open {
        open_in_browser(&page);
    }
}

/// `doc --open`: `$BROWSER`, else the system's opener. A failure warns
/// and leaves the path to open by hand.
pub(crate) fn open_in_browser(page: &Path) {
    let page = std::path::absolute(page).unwrap_or_else(|_| page.to_path_buf());
    let browser = std::env::var_os("BROWSER").filter(|b| !b.is_empty());
    let r = if let Some(b) = browser {
        // A browser may run until closed: do not wait for it.
        Command::new(&b).arg(&page).spawn().map(|_| ())
    } else {
        let mut c = if cfg!(windows) {
            // `start`'s first quoted argument is a window title.
            let mut c = Command::new("cmd");
            c.args(["/C", "start", ""]);
            c
        } else if cfg!(target_os = "macos") {
            Command::new("open")
        } else {
            Command::new("xdg-open")
        };
        c.arg(&page)
            .stdout(process::Stdio::null())
            .stderr(process::Stdio::null())
            .status()
            .and_then(|s| {
                if s.success() {
                    Ok(())
                } else {
                    Err(std::io::Error::other(format!("{s}")))
                }
            })
    };
    if let Err(e) = r {
        eprintln!(
            "{}: {} cannot open a browser ({e}); open {} in one, or set BROWSER",
            cli::invoked_as(),
            term::label(Stream::Stderr, Role::Warning),
            page.display()
        );
    }
}
