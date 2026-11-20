//! What the developer sees and presses: status lines, the program's
//! replies to a load, and the keys. On a terminal a key acts at once
//! (no echo, no Enter); otherwise each line's first letter is a key.

use nabic_core::term::{self, Role, Stream};
use std::io::{IsTerminal, Read};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError, channel};

/// A key the session acts on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Key {
    Restart,
    Quit,
}

/// One reply of the program to a load.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Reply<'a> {
    /// `hello <protocol> <pid>`, the program's first line.
    Hello(&'a str),
    Applied(&'a str),
    Waiting(&'a str),
    Refused(&'a str),
    Superseded(&'a str),
    Other(&'a str),
}

impl<'a> Reply<'a> {
    pub(crate) fn parse(line: &'a str) -> Self {
        let word = |w: &str| {
            let rest = line.strip_prefix(w)?;
            if rest.is_empty() {
                return Some("");
            }
            rest.strip_prefix(':')
                .or_else(|| rest.strip_prefix(' '))
                .map(str::trim_start)
        };
        if let Some(r) = word("hello") {
            Self::Hello(r)
        } else if let Some(r) = word("applied") {
            Self::Applied(r)
        } else if let Some(r) = word("waiting") {
            Self::Waiting(r)
        } else if let Some(r) = word("refused") {
            Self::Refused(r)
        } else if let Some(r) = word("superseded") {
            Self::Superseded(r)
        } else {
            Self::Other(line)
        }
    }

    /// The text after the keyword: `<pkg> v<n>[: <why>]`.
    const fn rest(&self) -> &'a str {
        match self {
            Self::Hello(t)
            | Self::Applied(t)
            | Self::Waiting(t)
            | Self::Refused(t)
            | Self::Superseded(t)
            | Self::Other(t) => t,
        }
    }

    /// The package a reply is about (its code name).
    pub(crate) fn package(&self) -> Option<&'a str> {
        if matches!(self, Self::Hello(_) | Self::Other(_)) {
            return None;
        }
        self.rest()
            .split([' ', ':'])
            .next()
            .filter(|p| !p.is_empty())
    }

    /// Whether this ends its load.
    pub(crate) const fn last(&self) -> bool {
        matches!(
            self,
            Self::Applied(_) | Self::Refused(_) | Self::Superseded(_)
        )
    }
}

/// Print a reply.
pub(crate) fn reply(r: &Reply<'_>) {
    let t = r.rest();
    match r {
        Reply::Applied(_) => term::status("Applied", t),
        Reply::Waiting(_) => term::status("Waiting", t),
        Reply::Superseded(_) => term::status("Superseded", t),
        Reply::Refused(_) => {
            let l = term::paint(Stream::Stderr, Role::Warning, &format!("{:>12}", "Refused"));
            eprintln!(
                "{l} {t}\n{:>12} the old code keeps running; fix it and save, or press `r` to restart",
                ""
            );
        }
        Reply::Hello(_) => {}
        Reply::Other(_) => term::note(t),
    }
}

pub(crate) fn status(verb: &str, msg: &str) {
    term::status(verb, msg);
}

/// A compile error: the program keeps the old code.
pub(crate) fn kept(what: &str) {
    let l = term::paint(Stream::Stderr, Role::Warning, &format!("{:>12}", "Kept"));
    eprintln!("{l} the running code of {what}: fix the error and save");
}

pub(crate) fn keys_help() {
    term::status("Watching", "press `r` to restart, `q` to quit");
}

/// The keys typed, read by a thread of their own.
pub(crate) struct Keys {
    rx: Receiver<Key>,
    _raw: Raw,
}

impl Keys {
    pub(crate) fn start() -> Self {
        let raw = Raw::enter();
        let (tx, rx) = channel();
        let line = !raw.on;
        std::thread::spawn(move || {
            let mut stdin = std::io::stdin().lock();
            let mut b = [0u8; 1];
            let mut first = true;
            while matches!(stdin.read(&mut b), Ok(1)) {
                let c = b[0];
                // Line mode: only a line's first letter counts.
                let take = !line || first;
                first = c == b'\n';
                let key = match c.to_ascii_lowercase() {
                    b'r' if take => Key::Restart,
                    b'q' if take => Key::Quit,
                    // Ctrl-D on a terminal in raw mode.
                    4 if !line => Key::Quit,
                    _ => continue,
                };
                if tx.send(key).is_err() {
                    break;
                }
            }
        });
        Self { rx, _raw: raw }
    }

    /// The next key typed, if any.
    pub(crate) fn poll(&self) -> Option<Key> {
        match self.rx.try_recv() {
            Ok(k) => Some(k),
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => None,
        }
    }
}

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

/// Ctrl-C ends the session cleanly: the terminal back as it was, the
/// program reaped.
pub(crate) fn catch_interrupt() {
    #[cfg(unix)]
    {
        extern "C" fn on(_: i32) {
            INTERRUPTED.store(true, Ordering::SeqCst);
        }
        // SAFETY: a handler that only stores to an atomic.
        unsafe {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = on as *const () as usize;
            libc::sigemptyset(&raw mut sa.sa_mask);
            libc::sigaction(libc::SIGINT, &raw const sa, std::ptr::null_mut());
            libc::sigaction(libc::SIGTERM, &raw const sa, std::ptr::null_mut());
            libc::sigaction(libc::SIGHUP, &raw const sa, std::ptr::null_mut());
        }
    }
    #[cfg(windows)]
    {
        unsafe extern "system" {
            fn SetConsoleCtrlHandler(h: Option<extern "system" fn(u32) -> i32>, add: i32) -> i32;
        }
        extern "system" fn on(_: u32) -> i32 {
            INTERRUPTED.store(true, Ordering::SeqCst);
            1
        }
        // SAFETY: a handler that only stores to an atomic.
        unsafe {
            SetConsoleCtrlHandler(Some(on), 1);
        }
    }
}

pub(crate) fn interrupted() -> bool {
    INTERRUPTED.load(Ordering::SeqCst)
}

/// The terminal without line editing and echo while the session runs.
struct Raw {
    on: bool,
    #[cfg(unix)]
    old: Option<libc::termios>,
}

impl Raw {
    fn enter() -> Self {
        #[cfg(unix)]
        {
            if !std::io::stdin().is_terminal() {
                return Self {
                    on: false,
                    old: None,
                };
            }
            // SAFETY: termios of fd 0, a terminal.
            unsafe {
                let mut t: libc::termios = std::mem::zeroed();
                if libc::tcgetattr(0, &raw mut t) != 0 {
                    return Self {
                        on: false,
                        old: None,
                    };
                }
                let old = t;
                t.c_lflag &= !(libc::ICANON | libc::ECHO);
                t.c_cc[libc::VMIN] = 1;
                t.c_cc[libc::VTIME] = 0;
                if libc::tcsetattr(0, libc::TCSANOW, &raw const t) != 0 {
                    return Self {
                        on: false,
                        old: None,
                    };
                }
                Self {
                    on: true,
                    old: Some(old),
                }
            }
        }
        #[cfg(not(unix))]
        {
            let _ = std::io::stdin().is_terminal();
            Self { on: false }
        }
    }
}

impl Drop for Raw {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(old) = self.old {
            // SAFETY: the settings read at `enter`.
            unsafe {
                libc::tcsetattr(0, libc::TCSANOW, &raw const old);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replies() {
        let r = Reply::parse("applied game v3: 4 functions, 1 type (Enemy: +mana)");
        assert_eq!(
            r,
            Reply::Applied("game v3: 4 functions, 1 type (Enemy: +mana)")
        );
        assert_eq!(r.package(), Some("game"));
        assert!(r.last());
        let r = Reply::parse("waiting game v2: a live `State::Paused` exists");
        assert_eq!(r.package(), Some("game"));
        assert!(!r.last());
        assert_eq!(Reply::parse("refused a v2: no"), Reply::Refused("a v2: no"));
        let r = Reply::parse("superseded my_lib v4");
        assert_eq!(r.package(), Some("my_lib"));
        assert!(r.last());
        assert_eq!(Reply::parse("hello 1 42"), Reply::Hello("1 42"));
        assert_eq!(Reply::parse("hello 1 42").package(), None);
        assert_eq!(Reply::parse("appliedx"), Reply::Other("appliedx"));
    }
}
