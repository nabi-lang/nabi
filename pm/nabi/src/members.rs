//! `-p <name>` and `--workspace`: a command run in workspace members
//! (`workspace`).

use crate::{manifest, workspace};
use nabic_core::term;
use std::path::Path;
use std::process::Command;

/// `-p <name>` / `--package <name>` and `--workspace` taken out of `args`
/// (up to `--`): the members named, whether all were asked for, and the
/// rest.
fn take(args: &[String]) -> Result<(Vec<String>, bool, Vec<String>), String> {
    let (mut names, mut all, mut rest) = (Vec::new(), false, Vec::new());
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--" {
            rest.push(a.clone());
            rest.extend(it.cloned());
            break;
        }
        match a.as_str() {
            "--workspace" => all = true,
            "-p" | "--package" => {
                names.push(
                    it.next()
                        .cloned()
                        .ok_or_else(|| format!("`{a}` needs a value"))?,
                );
            }
            _ => match a
                .strip_prefix("--package=")
                .or_else(|| a.strip_prefix("-p="))
            {
                Some(n) => names.push(n.to_owned()),
                None => rest.push(a.clone()),
            },
        }
    }
    Ok((names, all, rest))
}

/// Run `cmd` on the members `post` selects (`-p`, `--workspace`, or all
/// of them at a workspace root), each with `argv` minus those options, in
/// its directory; `None` when the command is for this directory alone.
pub fn dispatch(
    cmd: &str,
    post: &[String],
    argv: &[std::ffi::OsString],
    quiet: bool,
) -> Option<i32> {
    if !workspace::COMMANDS.contains(&cmd) {
        return None;
    }
    let fail = |e: String| {
        eprintln!(
            "nabi: {} {e}",
            term::label(term::Stream::Stderr, term::Role::Error)
        );
        Some(1)
    };
    let (names, all, _) = match take(post) {
        Ok(t) => t,
        Err(e) => return fail(e),
    };
    let here = crate::package_dir(Path::new("."));
    let at_root = here.as_deref().is_some_and(workspace::is_root);
    if names.is_empty() && !all && !at_root {
        return None;
    }
    let ws = match here.as_deref() {
        Some(d) if at_root => workspace::load(d),
        Some(d) => workspace::of(d),
        None => Ok(None),
    };
    let ws = match ws {
        Ok(Some(w)) => w,
        Ok(None) => {
            return fail(format!(
                "`-p` and `--workspace` work inside a workspace (a nabi.nabion with \
                 `Workspace {{ members: [..] }}` above this package); here, run `nabi {cmd}` alone"
            ));
        }
        Err(e) => return fail(e),
    };
    let chosen: Vec<&workspace::Member> = if names.is_empty() {
        ws.members.iter().collect()
    } else {
        let mut out = Vec::new();
        for n in &names {
            let Some(m) = ws.members.iter().find(|m| manifest::same(&m.name, n)) else {
                let list: Vec<&str> = ws.members.iter().map(|m| m.name.as_str()).collect();
                return fail(format!(
                    "the workspace has no member `{n}` (members: {})",
                    list.join(", ")
                ));
            };
            out.push(m);
        }
        out
    };
    if cmd == "run" && chosen.len() != 1 {
        return fail(format!(
            "`nabi run` runs one package; name it: `nabi run -p <member>` (members: {})",
            ws.members
                .iter()
                .map(|m| m.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let me = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => return fail(format!("cannot find this nabi: {e}")),
    };
    let args = strip(argv);
    let mut failed = Vec::new();
    let mut code = 0;
    for m in &chosen {
        if chosen.len() > 1 && !quiet {
            term::status("Member", &format!("{} ({})", m.name, crate::shown(&m.dir)));
        }
        let mut c = Command::new(&me);
        c.args(&args).current_dir(&m.dir);
        let status = match crate::child::status(&mut c) {
            Ok(s) => s.code().unwrap_or(1),
            Err(e) => return fail(format!("cannot run {}: {e}", me.display())),
        };
        if status != 0 {
            failed.push(m.name.clone());
            if code == 0 {
                code = status;
            }
        }
    }
    if chosen.len() > 1 && !failed.is_empty() {
        eprintln!(
            "nabi: {} `nabi {cmd}` failed in {}",
            term::label(term::Stream::Stderr, term::Role::Error),
            failed.join(", ")
        );
    }
    Some(code)
}

/// `argv` without the member selection and `--cwd` (the member's
/// directory replaces it), up to `--`.
fn strip(argv: &[std::ffi::OsString]) -> Vec<std::ffi::OsString> {
    let mut out = Vec::new();
    let mut it = argv.iter();
    while let Some(a) = it.next() {
        let s = a.to_string_lossy();
        if s == "--" {
            out.push(a.clone());
            out.extend(it.cloned());
            break;
        }
        match s.as_ref() {
            "--workspace" => {}
            "-p" | "--package" | "--cwd" => {
                it.next();
            }
            _ if s.starts_with("--package=") || s.starts_with("-p=") || s.starts_with("--cwd=") => {
            }
            _ => out.push(a.clone()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection() {
        let args: Vec<String> = ["-p", "a", "--workspace", "x", "--package=b", "--", "-p"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        let (names, all, rest) = take(&args).unwrap();
        assert_eq!(names, ["a", "b"]);
        assert!(all);
        assert_eq!(rest, ["x", "--", "-p"]);
        let argv: Vec<std::ffi::OsString> = ["--cwd", "w", "test", "-p", "a", "f", "--", "-p"]
            .iter()
            .map(Into::into)
            .collect();
        assert_eq!(strip(&argv), ["test", "f", "--", "-p"]);
    }
}
