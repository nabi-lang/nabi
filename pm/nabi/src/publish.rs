//! The registry: `nabi publish`, `yank`, `login` and `logout`.

#[allow(clippy::wildcard_imports)]
use crate::*;

/// The index `--registry` names, else the default one.
pub(crate) fn index_url(registry: Option<&str>) -> String {
    registry.unwrap_or(registry::DEFAULT).to_owned()
}

/// The registry API of the index `--registry` names (else the default), and
/// the token for it.
pub(crate) fn api_token(g: &Global, registry: Option<&str>) -> Result<(String, String), String> {
    let home = home();
    let url = index_url(registry);
    let login = match registry {
        Some(u) if !registry::same_index(u, registry::DEFAULT) => {
            format!("nabi login --registry {u}")
        }
        _ => "nabi login".to_owned(),
    };
    let token = registry::token(&home, &url, &login)?;
    let api = registry::api(&home, &url, g.quiet)?;
    Ok((api, token))
}

/// `nabi publish [--native <file>] [--index-dir <dir> | --registry <url>]
/// [--dry-run]`: print the index line and commit it to `--index-dir` or
/// publish it through the API; `--dry-run` only checks.
pub(crate) fn cmd_publish(g: &Global, args: &[String]) -> Result<i32, String> {
    let (vals, sw, pos) = flags(
        args,
        &["--index-dir", "--registry", "--native"],
        &["--dry-run"],
    )?;
    if let Some(p) = pos.first() {
        return Err(format!("unexpected argument `{p}`"));
    }
    let write = !sw.iter().any(|s| s == "--dry-run");
    let index = val(&vals, "--index-dir").map(Path::new);
    let reg = val(&vals, "--registry");
    if index.is_some() && reg.is_some() {
        return Err(
            "`--index-dir` and `--registry` go apart: `--index-dir <dir>` commits to a checkout \
             of the index, `--registry <url>` publishes through that index's API"
                .into(),
        );
    }
    let remote = match index {
        None if write => Some(api_token(g, reg)?),
        _ => None,
    };
    let (line, meta, mut name, mut vers, rel, what) = if let Some(file) = val(&vals, "--native") {
        let e = native::publish(&home(), Path::new(file), index, write)?.0;
        let rel = native::path(&e.name);
        let meta = registry::native_meta(&e);
        (
            e.to_line(),
            meta,
            e.name,
            e.version.to_string(),
            rel,
            "native ",
        )
    } else {
        let (dir, m) = this_package()?;
        let e = registry::entry_for(&dir, &m)?;
        // Checked on a dry run too.
        let meta = registry::meta(&dir, &m)?;
        if let Some(i) = index {
            registry::publish(i, &e, write)?;
        }
        let rel = registry::path(&e.name);
        (e.to_line(), meta, e.name, e.version.to_string(), rel, "")
    };
    let to = match (index, remote) {
        (Some(i), _) => i.display().to_string(),
        (None, Some((api, token))) => {
            let p = registry::publish_api(&api, &token, &line, meta)?;
            (name, vers) = (p.name, p.version);
            for w in &p.warnings {
                warn_once(&format!("{api}: {w}"));
            }
            api
        }
        (None, None) => index_url(reg),
    };
    println!("{line}");
    if !g.quiet {
        if write {
            term::status("Published", &format!("{what}{name} {vers} to {to}"));
        } else if index.is_some() {
            eprintln!(
                "{} dry run; `nabi publish` commits it to {to} as {rel} (not pushed)",
                term::label(Stream::Stderr, Role::Note)
            );
        } else {
            eprintln!(
                "{} dry run; `nabi publish` sends it to the registry API of {to} (needs `nabi \
                 login`), which adds it as {rel}",
                term::label(Stream::Stderr, Role::Note)
            );
        }
    }
    Ok(0)
}

/// The most bytes of a yank's `--message`.
pub(crate) const MAX_YANK_MESSAGE: usize = 512;

/// `nabi yank [--native] <name>@<version> [--undo] [--message <why>]
/// [--index-dir <dir> | --registry <url>]`: into the `--index-dir`
/// checkout, else via the API.
pub(crate) fn cmd_yank(g: &Global, args: &[String]) -> Result<i32, String> {
    let (vals, sw, pos) = flags(
        args,
        &["--index-dir", "--registry", "--message"],
        &["--undo", "--native"],
    )?;
    let usage = "usage: nabi yank [--native] <name>@<version> [--undo] [--message <why>] \
                 [--index-dir <dir> | --registry <url>]";
    let native = sw.iter().any(|s| s == "--native");
    let (name, vers) = match pos.as_slice() {
        [at] => match at.split_once('@') {
            Some((n, v)) if !n.is_empty() && !v.is_empty() => (n.to_owned(), v.to_owned()),
            _ => {
                // A bare version is likely this package's.
                let n = at.trim_end_matches('@');
                let fix = if n.starts_with(|c: char| c.is_ascii_digit()) {
                    format!("<name>@{n}")
                } else {
                    format!("{n}@1.2.0")
                };
                return Err(format!(
                    "`{at}`: give the version as `<name>@<version>`, e.g. `nabi yank {fix}`\n{usage}"
                ));
            }
        },
        _ => return Err(usage.into()),
    };
    let undo = sw.iter().any(|s| s == "--undo");
    let message = val(&vals, "--message").filter(|m| !m.is_empty());
    if let Some(m) = message {
        if undo {
            return Err(
                "`--message` says why a version is yanked; `--undo` takes none: drop one of them"
                    .into(),
            );
        }
        if m.len() > MAX_YANK_MESSAGE {
            return Err(format!(
                "`--message` is {} bytes; keep it to {MAX_YANK_MESSAGE}",
                m.len()
            ));
        }
    }
    let index = val(&vals, "--index-dir");
    let reg = val(&vals, "--registry");
    if index.is_some() && reg.is_some() {
        return Err(
            "`--index-dir` and `--registry` go apart: `--index-dir <dir>` commits to a checkout \
             of the index, `--registry <url>` goes through that index's API"
                .into(),
        );
    }
    let to = if let Some(i) = index {
        if message.is_some() {
            return Err(
                "`--message` goes to the registry API, which keeps it; an `--index-dir` \
                 checkout has no place for it: drop `--message`"
                    .into(),
            );
        }
        registry::yank(Path::new(i), &name, &vers, !undo, native)?;
        i.to_owned()
    } else {
        let (api, token) = api_token(g, reg)?;
        registry::yank_api(&api, &token, &name, &vers, undo, native, message)?;
        api
    };
    if !g.quiet {
        let verb = if undo { "Unyanked" } else { "Yanked" };
        eprintln!("   {verb} {name} {vers} in {to}");
    }
    Ok(0)
}

/// `nabi login [--registry <url>] [<token>]`: store a token whose account
/// may publish, for that index (default: packages.nabi-lang.org).
pub(crate) fn cmd_login(g: &Global, args: &[String]) -> Result<i32, String> {
    let (vals, _, pos) = flags(args, &["--registry"], &[])?;
    if pos.len() > 1 {
        return Err("usage: nabi login [--registry <url>] [<token>]".into());
    }
    let home = home();
    let default = registry::DEFAULT.to_owned();
    let url = val(&vals, "--registry").unwrap_or(&default);
    let api = registry::api(&home, url, g.quiet)?;
    let token = match pos.first() {
        Some(t) => t.clone(),
        None => read_secret(&format!(
            "a token from {api}/me (not shown; Ctrl-C to cancel): "
        ))?,
    };
    registry::check_token(&token)?;
    let (login, publish) = registry::me(&api, &token)?;
    if !publish {
        return Err(
            "this account cannot publish yet: publishing is open to the registry's maintainers only".into(),
        );
    }
    let file = registry::save_token(&home, url, &token)?;
    if !g.quiet {
        term::status(
            "Logged in",
            &format!("as {login} at {api} (token in {})", file.display()),
        );
    }
    Ok(0)
}

/// `nabi logout [--registry <url> | --all]`: remove `nabi login`'s token
/// for that index (default: packages.nabi-lang.org), or every one.
pub(crate) fn cmd_logout(g: &Global, args: &[String]) -> Result<i32, String> {
    let (vals, sw, pos) = flags(args, &["--registry"], &["--all"])?;
    if let Some(a) = pos.first() {
        return Err(format!("unexpected argument `{a}`"));
    }
    let all = sw.iter().any(|s| s == "--all");
    let reg = val(&vals, "--registry");
    if all && reg.is_some() {
        return Err(
            "`--all` logs out of every index; give `--all` or `--registry <url>`, not both".into(),
        );
    }
    let home = home();
    let had = if all {
        registry::logout(&home, None)?
    } else {
        let default = registry::DEFAULT.to_owned();
        registry::logout(&home, Some(reg.unwrap_or(&default)))?
    };
    if !g.quiet {
        if had {
            term::status("Logged out", "");
        } else {
            eprintln!("{} not logged in", term::label(Stream::Stderr, Role::Note));
        }
    }
    Ok(0)
}

/// A trimmed stdin line, unechoed on a terminal (echo returns on Ctrl-C).
pub(crate) fn read_secret(prompt: &str) -> Result<String, String> {
    let tty = std::io::stdin().is_terminal();
    let hidden = tty && {
        eprint!("{prompt}");
        echo::off()
    };
    let mut line = String::new();
    let r = std::io::stdin().read_line(&mut line);
    if hidden {
        echo::on();
    }
    if tty {
        eprintln!();
    }
    r.map_err(|e| format!("cannot read stdin: {e}"))?;
    Ok(line.trim().to_owned())
}

/// Turn terminal echo off; restore it on `on` or Ctrl-C (which then ends
/// the process as usual).
#[cfg(unix)]
mod echo {
    use std::sync::atomic::{AtomicUsize, Ordering};

    // `struct termios` starts with `c_iflag`, `c_oflag`, `c_cflag`,
    // `c_lflag` (`tcflag_t`); the rest is copied as is.
    #[cfg(target_vendor = "apple")]
    type Flag = u64;
    #[cfg(not(target_vendor = "apple"))]
    type Flag = u32;
    const LFLAG: usize = 3 * size_of::<Flag>();
    const ECHO: Flag = 8;
    const SIGINT: i32 = 2;
    const TCSANOW: i32 = 0;

    /// The saved `termios`, larger than any.
    static mut SAVED: [u64; 32] = [0; 32];
    static HANDLER: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" {
        fn tcgetattr(fd: i32, t: *mut u8) -> i32;
        fn tcsetattr(fd: i32, when: i32, t: *const u8) -> i32;
        fn signal(sig: i32, handler: usize) -> usize;
        fn raise(sig: i32) -> i32;
    }

    // Async-signal-safe calls only.
    extern "C" fn interrupted(_: i32) {
        // SAFETY: `SAVED` is written before this handler is installed.
        unsafe {
            tcsetattr(0, TCSANOW, (&raw const SAVED).cast());
            signal(SIGINT, 0);
            raise(SIGINT);
        }
    }

    pub fn off() -> bool {
        // SAFETY: `SAVED` is only touched here, before the handler is
        // installed, and by `on` / the handler afterwards.
        unsafe {
            if tcgetattr(0, (&raw mut SAVED).cast()) != 0 {
                return false;
            }
            let mut t = SAVED;
            let lflag = (&raw mut t).cast::<u8>().add(LFLAG).cast::<Flag>();
            lflag.write_unaligned(lflag.read_unaligned() & !ECHO);
            let h = interrupted as extern "C" fn(i32) as usize;
            HANDLER.store(signal(SIGINT, h), Ordering::Relaxed);
            tcsetattr(0, TCSANOW, (&raw const t).cast());
        }
        true
    }

    pub fn on() {
        // SAFETY: as in `off`.
        unsafe {
            tcsetattr(0, TCSANOW, (&raw const SAVED).cast());
            signal(SIGINT, HANDLER.load(Ordering::Relaxed));
        }
    }
}

#[cfg(windows)]
mod echo {
    use std::sync::atomic::{AtomicU32, Ordering};

    unsafe extern "system" {
        fn GetStdHandle(which: u32) -> isize;
        fn GetConsoleMode(h: isize, mode: *mut u32) -> i32;
        fn SetConsoleMode(h: isize, mode: u32) -> i32;
        fn SetConsoleCtrlHandler(handler: Option<extern "system" fn(u32) -> i32>, add: i32) -> i32;
    }
    const STD_INPUT_HANDLE: u32 = 0xFFFF_FFF6;
    const ENABLE_ECHO_INPUT: u32 = 4;
    static SAVED: AtomicU32 = AtomicU32::new(0);

    fn restore() {
        // SAFETY: the console input handle.
        unsafe {
            SetConsoleMode(
                GetStdHandle(STD_INPUT_HANDLE),
                SAVED.load(Ordering::Relaxed),
            )
        };
    }

    /// Restore, then let the next handler (the default one) end the
    /// process.
    extern "system" fn interrupted(_: u32) -> i32 {
        restore();
        0
    }

    pub fn off() -> bool {
        // SAFETY: the console input handle and a local out-parameter.
        unsafe {
            let h = GetStdHandle(STD_INPUT_HANDLE);
            let mut m = 0;
            if GetConsoleMode(h, &mut m) == 0 {
                return false;
            }
            SAVED.store(m, Ordering::Relaxed);
            SetConsoleCtrlHandler(Some(interrupted), 1);
            SetConsoleMode(h, m & !ENABLE_ECHO_INPUT);
        }
        true
    }

    pub fn on() {
        restore();
        // SAFETY: removes the handler `off` added.
        unsafe { SetConsoleCtrlHandler(Some(interrupted), 0) };
    }
}
