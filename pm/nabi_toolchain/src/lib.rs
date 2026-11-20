//! Toolchains: the installed `$NABI_HOME/toolchains/`, the proxies in
//! `$NABI_HOME/bin`, the hand-off to a pinned toolchain, and the build
//! cache each toolchain's nabic keeps.

pub mod cache;
pub mod child;
pub mod sign;
pub mod toolchain;

use nabi_manifest::{VERSION, canonical_edit, home, manifest, package_dir, semver, skipped};
use nabic_core::term::{self, Role, Stream};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

/// `VERSION`, and the commit and its date when built from git.
pub const VERSION_LINE: &str = concat!(env!("CARGO_PKG_VERSION"), env!("NABI_COMMIT_INFO"));

/// Fields of `$NABI_HOME/config.nabion` (`Config { .. }`).
const CONFIG_FIELDS: [&str; 1] = ["toolchain"];

/// String field `field` of `$NABI_HOME/config.nabion`.
pub fn config(home: &Path, field: &str) -> Result<Option<String>, String> {
    let path = home.join("config.nabion");
    let text = config_text(&path)?;
    config_field(&text, &path.display().to_string(), field)
}

/// `path`'s text; an empty config when there is none. Any other read
/// error is fatal, so the file is never replaced by defaults.
fn config_text(path: &Path) -> Result<String, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok("Config {}\n".to_owned()),
        Err(e) => Err(format!(
            "cannot read {}: {e}\nhelp: fix its permissions or save it as UTF-8, \
             or move it aside to start from the defaults",
            path.display()
        )),
    }
}

/// String field `field` of the config text `text`, read from `file`.
/// Fields a newer nabi wrote are skipped.
fn config_field(text: &str, file: &str, field: &str) -> Result<Option<String>, String> {
    let doc = nabic_nabion::Doc::parse(text, file)?;
    let c = doc.root().record("Config")?;
    skipped(file, &c.known(&CONFIG_FIELDS));
    c.opt(field).map(|r| r.string()).transpose()
}

/// Set string field `field` of `$NABI_HOME/config.nabion`, keeping the rest.
pub fn set_config(home: &Path, field: &str, value: &str) -> Result<(), String> {
    let path = home.join("config.nabion");
    let file = path.display().to_string();
    std::fs::create_dir_all(home).map_err(|e| format!("{}: {e}", home.display()))?;
    let old = config_text(&path)?;
    config_field(&old, &file, field)?;
    let step = [nabic_nabion::edit::Step::Field(field)];
    let text = nabic_nabion::edit::set(&old, &step, &nabic_nabion::quote(value))
        .map_err(|e| e.render(&old, &file))?;
    std::fs::write(&path, canonical_edit(&old, &text))
        .map_err(|e| format!("cannot write {file}: {e}"))
}

/// nabic's options that take a separate value (`--target x86_64-..`).
pub const VALUE_OPTIONS: [&str; 37] = [
    "--bundle",
    "--color",
    "--cwd",
    "--message-format",
    "-o",
    "--output",
    "--bootstrap",
    "--cfg",
    "--print",
    "--explain",
    "--extern",
    "--extern-feature",
    "--name",
    "--target-dir",
    "--target",
    "--nabir",
    "-L",
    "-l",
    "--native-runtime",
    "--emit",
    "-C",
    "--codegen",
    "--topn",
    "--connection-file",
    "-A",
    "-W",
    "-D",
    "--allow",
    "--warn",
    "--deny",
    "--iters",
    "--samples",
    "--format",
    "--threads",
    "--crate-type",
    "-x",
    "--language",
];

/// Replace this process with `c` (unix), else run it to its end.
pub fn exec(mut c: Command, what: &Path) -> i32 {
    #[cfg(unix)]
    let e = c.exec();
    #[cfg(not(unix))]
    let e = match crate::child::status(&mut c) {
        Ok(s) => return s.code().unwrap_or(1),
        Err(e) => e,
    };
    eprintln!(
        "nabi: {} cannot run {}: {e}",
        term::label(Stream::Stderr, Role::Error),
        what.display()
    );
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_updates_keep_other_fields() {
        let home = nabic_core::tmp::test_dir("config-fields");
        assert_eq!(config(&home, "toolchain").unwrap(), None);
        set_config(&home, "registry", "https://example.test/index").unwrap();
        set_config(&home, "toolchain", "0.1.1").unwrap();
        assert_eq!(
            config(&home, "toolchain").unwrap().as_deref(),
            Some("0.1.1")
        );
        assert_eq!(
            config(&home, "registry").unwrap().as_deref(),
            Some("https://example.test/index")
        );
    }

    #[test]
    fn config_read_errors_preserve_existing_bytes() {
        let home = nabic_core::tmp::test_dir("config-read-error");
        let path = home.join("config.nabion");
        let bytes = b"Config { registry: \"index\" }\n\xff";
        std::fs::write(&path, bytes).unwrap();
        let error = config(&home, "toolchain").unwrap_err();
        assert!(error.contains("cannot read"));
        assert!(error.contains(&path.display().to_string()));
        assert!(error.contains("help: fix its permissions or save it as UTF-8"));
        assert_eq!(set_config(&home, "toolchain", "0.1.1").unwrap_err(), error);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(config(&home, "toolchain").is_err());
        assert!(set_config(&home, "toolchain", "0.1.1").is_err());
        assert!(path.is_dir());
    }
}
