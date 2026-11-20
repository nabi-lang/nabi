//! `nabi dev`: run the program and apply each saved edit while it runs.
//!
//! The host (runtime, standard library state and every registry or git
//! package) is built once. Each local package (the root, its path
//! dependencies) is a hot plugin; a save rebuilds that package and the
//! hot packages depending on it, and the program loads the new versions
//! at its next safepoint, keeping its state. A compile error leaves the
//! old code running. A manifest, lock or toolchain change restarts.
//!
//! ```text
//! <target>/dev/host/main.nabi     the host's source (build::HOST_MAIN)
//! <target>/dev/<root>-host        the host
//! <target>/dev/<pkg>.<n>.<ext>    build n (from 1) of hot package pkg, and its .layout
//! <target>/dev/dev.sock           the dev channel (Windows: a named pipe)
//! ```
//!
//! The channel carries lines: `load <path>` to the program, and per load
//! `waiting <pkg> v<n>: ..` (not final) and then `applied <pkg> v<n>: ..`,
//! `refused <pkg> v<n>: ..` or `superseded <pkg> v<n>` back; the replies
//! of one package come in the order of its loads.

mod build;
mod ui;
mod watch;

use crate::Global;
use crate::tools::dev::build::{LOAD_FAILED, Plan};
use crate::tools::dev::ui::{Key, Keys, Reply};
use crate::tools::dev::watch::{Changes, Kind, TICK, Watcher};
use std::collections::VecDeque;
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

const DEBUG_ONLY: &str = "hot reload runs debug builds; drop `--release`";

/// `nabi dev [path] [-p member] [--bin name] [options] [-- args]`.
pub(crate) fn cmd_dev(g: &Global, pre: &[String], post: &[String]) -> Result<i32, String> {
    let opts = options(post)?;
    if let Some(m) = &opts.member {
        let dir = member(m)?;
        std::env::set_current_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    } else if let Some(p) = &opts.path {
        let dir = crate::package_dir(Path::new(p))
            .ok_or_else(|| format!("`{p}` is not inside a package"))?;
        std::env::set_current_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    if let Some(bin) = &opts.bin {
        let here = std::env::current_dir().map_err(|e| e.to_string())?;
        let name = crate::manifest::find(&here)
            .and_then(|m| crate::manifest::load(&m).ok()?.name)
            .unwrap_or_else(|| {
                nabic_interface::discover(&here)
                    .map(|p| p.name)
                    .unwrap_or_default()
            });
        if !crate::manifest::same(bin, &name) {
            return Err(format!(
                "`{name}` has one executable, `{name}`, not `{bin}`: write `nabi dev` (or \
                 `--bin {name}`), or `-p {bin}` for a workspace member"
            ));
        }
    }
    ui::catch_interrupt();
    let keys = Keys::start();
    let mut first = true;
    loop {
        match session(g, pre, &opts.rest, &keys, first) {
            End::Fail(e) => return Err(e),
            End::Quit => return Ok(0),
            End::Interrupted => return Ok(130),
            End::Restart(why) => ui::status("Restarting", &why),
        }
        first = false;
    }
}

/// `nabi dev`'s own options taken out of `post`.
#[derive(Debug, Default, PartialEq, Eq)]
struct Options {
    member: Option<String>,
    bin: Option<String>,
    path: Option<String>,
    /// nabic's options, without the program's arguments.
    rest: Vec<String>,
}

fn options(post: &[String]) -> Result<Options, String> {
    let mut o = Options::default();
    let mut it = post.iter();
    let value = |it: &mut std::slice::Iter<'_, String>, flag: &str| {
        it.next()
            .cloned()
            .ok_or_else(|| format!("`{flag}` needs a value: `{flag} <name>`"))
    };
    while let Some(a) = it.next() {
        let (name, inline) = match a.split_once('=') {
            Some((n, v)) if n.starts_with("--") => (n, Some(v.to_owned())),
            _ => (a.as_str(), None),
        };
        match name {
            "--" => break,
            "-p" | "--package" => {
                o.member = Some(inline.map_or_else(|| value(&mut it, name), Ok)?);
            }
            "--bin" => o.bin = Some(inline.map_or_else(|| value(&mut it, name), Ok)?),
            "--workspace" => {
                return Err(
                    "`nabi dev` runs one program: name it with `-p <member>`, not `--workspace`"
                        .to_owned(),
                );
            }
            "--release" => return Err(DEBUG_ONLY.to_owned()),
            // The dev profile's own level; nabic takes none with `--hot`.
            "-O0" => {}
            o2 if o2.starts_with("-O") => {
                return Err(format!("hot reload runs debug builds; drop `{o2}`"));
            }
            "--target" | "--crate-type" | "-o" | "--output" | "--emit" => {
                return Err(format!(
                    "`nabi dev` builds for this machine only; drop `{name}` (use `nabi build \
                     {name} ..` for other outputs)"
                ));
            }
            p if !p.starts_with('-')
                && o.path.is_none()
                && o.rest.last().is_none_or(|l| {
                    l != "--features" && !crate::VALUE_OPTIONS.contains(&l.as_str())
                }) =>
            {
                o.path = Some(p.to_owned());
            }
            _ => o.rest.push(a.clone()),
        }
    }
    Ok(o)
}

/// The directory of workspace member `name`.
fn member(name: &str) -> Result<PathBuf, String> {
    let here = crate::package_dir(Path::new("."));
    let ws = match here.as_deref() {
        Some(d) if crate::workspace::is_root(d) => crate::workspace::load(d)?,
        Some(d) => crate::workspace::of(d)?,
        None => None,
    };
    let Some(ws) = ws else {
        return Err(
            "`-p` works inside a workspace (a nabi.nabion with `Workspace { members: [..] }`); \
             here, run `nabi dev` alone"
                .to_owned(),
        );
    };
    if let Some(m) = (ws.members.iter()).find(|m| crate::manifest::same(&m.name, name)) {
        return Ok(m.dir.clone());
    }
    let names: Vec<&str> = ws.members.iter().map(|m| m.name.as_str()).collect();
    Err(format!(
        "the workspace has no member `{name}` (members: {})",
        names.join(", ")
    ))
}

/// How a session ended.
enum End {
    /// The first session could not start: nothing to wait for.
    Fail(String),
    Quit,
    Interrupted,
    Restart(String),
}

/// One run of the program, from building the host to a restart or quit.
fn session(g: &Global, pre: &[String], rest: &[String], keys: &Keys, first: bool) -> End {
    let mut w = Watcher::new();
    toolchain_files(&mut w);
    let mut plan = match Plan::new(g, pre, rest, g.tail.clone()) {
        Ok(p) => p,
        Err(e) if first => return End::Fail(e),
        Err(e) => {
            error(&e);
            // Watch what may fix it: the manifest, lock and sources here.
            if let Ok(here) = std::env::current_dir() {
                w.tree(&here.join("src"), Kind::Source(0));
                w.files_of(&here);
            }
            w.arm();
            return idle(&mut w, keys, "fix it and save");
        }
    };
    if g.verbose {
        build::describe(&plan);
    }
    for (i, h) in plan.hot.iter().enumerate() {
        w.tree(&h.dir.join("src"), Kind::Source(i));
    }
    for m in &plan.manifests {
        w.file(m, Kind::Manifest);
    }
    w.arm();
    if let Err(e) = plan.clean() {
        error(&e);
        return idle(&mut w, keys, "fix it");
    }
    let started = Instant::now();
    ui::status("Building", &format!("the host of `{}`", plan.root().name));
    if plan.build_host().is_err() {
        return idle(&mut w, keys, "fix the error and save");
    }
    let mut files = Vec::new();
    for i in 0..plan.hot.len() {
        match plan.build_hot(i) {
            Ok(f) => files.push(f),
            Err(_) => return idle(&mut w, keys, "fix the error and save"),
        }
    }
    for h in &mut plan.hot {
        h.live = Some(1);
    }
    let mut ch = match Channel::open(&plan.dev) {
        Ok(c) => c,
        Err(e) => {
            error(&format!("cannot open the dev channel: {e}"));
            return idle(&mut w, keys, "press `r` to try again");
        }
    };
    let mut prog = match Program::start(&plan, &files, &ch.endpoint) {
        Ok(p) => p,
        Err(e) => {
            error(&e);
            return idle(&mut w, keys, "press `r` to try again");
        }
    };
    ui::status(
        "Running",
        &format!(
            "`{}` ({:.1}s)",
            plan.root().name,
            started.elapsed().as_secs_f64()
        ),
    );
    if first {
        ui::keys_help();
    }
    let mut pending: VecDeque<Load> = VecDeque::new();
    let mut exited = false;
    loop {
        if ui::interrupted() {
            return End::Interrupted;
        }
        match keys.poll() {
            Some(Key::Quit) => return End::Quit,
            Some(Key::Restart) => return End::Restart("as asked (`r`)".to_owned()),
            None => {}
        }
        if !exited {
            if let Some(st) = prog.exited() {
                let code = st.code();
                exited = true;
                drain(&mut ch, &mut pending, &mut plan);
                if code == Some(LOAD_FAILED) && pending.is_empty() {
                    ui::status(
                        "Stopped",
                        "the program did not load; fix it and save, or press `r`",
                    );
                } else {
                    let how =
                        code.map_or_else(|| "by a signal".to_owned(), |c| format!("with {c}"));
                    ui::status(
                        "Exited",
                        &format!("{how}; save a file or press `r` to run it again, `q` to quit"),
                    );
                }
            }
        }
        drain(&mut ch, &mut pending, &mut plan);
        let c = w.poll();
        if let Some(why) = restart_reason(&c) {
            return End::Restart(why);
        }
        if !c.sources.is_empty() {
            if exited {
                return End::Restart("a source changed".to_owned());
            }
            reload(&mut plan, &mut ch, &mut pending, &c.sources);
        }
        std::thread::sleep(TICK);
    }
}

/// Wait for a fix (`hint`): a change restarts, `q` quits.
fn idle(w: &mut Watcher, keys: &Keys, hint: &str) -> End {
    ui::status("Waiting", &format!("{hint} (`r` restarts, `q` quits)"));
    loop {
        if ui::interrupted() {
            return End::Interrupted;
        }
        match keys.poll() {
            Some(Key::Quit) => return End::Quit,
            Some(Key::Restart) => return End::Restart("as asked (`r`)".to_owned()),
            None => {}
        }
        let c = w.poll();
        if let Some(why) = restart_reason(&c) {
            return End::Restart(why);
        }
        if !c.is_empty() {
            return End::Restart("a source changed".to_owned());
        }
        std::thread::sleep(TICK);
    }
}

/// Why changes `c` restart the program, if they do.
fn restart_reason(c: &Changes) -> Option<String> {
    if c.toolchain {
        return Some("the toolchain changed".to_owned());
    }
    let m = c.manifests.first()?;
    Some(format!("{} changed", crate::shown(m)))
}

/// Rebuild the hot packages `changed` sources reach and send the
/// versions to the program.
fn reload(plan: &mut Plan, ch: &mut Channel, pending: &mut VecDeque<Load>, changed: &[usize]) {
    let todo = plan.affected(changed);
    // A load still unanswered may move the base the next build diffs
    // against: give it a moment.
    let start = Instant::now();
    while pending.iter().any(|l| todo.contains(&l.pkg) && !l.waiting)
        && start.elapsed() < Duration::from_secs(1)
    {
        std::thread::sleep(Duration::from_millis(10));
        drain(ch, pending, plan);
    }
    let mut built = Vec::new();
    for &i in &todo {
        let n = plan.hot[i].next;
        ui::status("Rebuilding", &format!("`{}`", plan.hot[i].name));
        let Ok(f) = plan.build_hot(i) else {
            ui::kept(&format!("`{}`", plan.hot[i].name));
            return;
        };
        built.push((i, n, f));
    }
    for (i, n, f) in built {
        if let Err(e) = ch.send(&format!("load {}", f.display())) {
            error(&format!("cannot reach the program: {e}"));
            return;
        }
        pending.push_back(Load {
            pkg: i,
            n,
            waiting: false,
        });
    }
}

/// A load sent and not finally answered.
struct Load {
    pkg: usize,
    n: u32,
    waiting: bool,
}

/// Print the program's replies, ending the loads they answer.
fn drain(ch: &mut Channel, pending: &mut VecDeque<Load>, plan: &mut Plan) {
    for line in ch.lines() {
        let r = Reply::parse(&line);
        if let Reply::Hello(_) = r {
            continue;
        }
        ui::reply(&r);
        // Replies of one package come in order: they answer its oldest load.
        let Some(pkg) = r.package() else {
            continue;
        };
        let Some(at) = pending
            .iter()
            .position(|l| nabic_interface::code_name(&plan.hot[l.pkg].name) == pkg)
        else {
            continue;
        };
        if let Reply::Waiting(_) = r {
            pending[at].waiting = true;
        }
        if r.last() {
            if let Some(l) = pending.remove(at) {
                if let Reply::Applied(_) = r {
                    plan.hot[l.pkg].live = Some(l.n);
                }
            }
        }
    }
}

fn error(e: &str) {
    if e.starts_with("error[") {
        eprintln!("{e}");
    } else {
        eprintln!(
            "nabi: {} {e}",
            nabic_core::term::label(
                nabic_core::term::Stream::Stderr,
                nabic_core::term::Role::Error
            )
        );
    }
}

/// Watch the toolchain binaries: a new build of them restarts.
fn toolchain_files(w: &mut Watcher) {
    if let Ok(me) = std::env::current_exe() {
        w.file(&me, Kind::Toolchain);
    }
    if let Ok(c) = crate::toolchain::nabic_for() {
        w.file(&c, Kind::Toolchain);
    }
}

/// The running host; killed and reaped when dropped.
struct Program {
    child: Child,
    done: Option<ExitStatus>,
}

impl Program {
    fn start(plan: &Plan, files: &[PathBuf], endpoint: &str) -> Result<Self, String> {
        let list: Vec<String> = files.iter().map(|f| f.display().to_string()).collect();
        let exe = plan.host_path();
        let mut c = Command::new(&exe);
        c.args(&plan.args)
            .env("NABI_DEV", endpoint)
            .env("NABI_DEV_PLUGINS", list.join("\n"))
            .env("NABI_DEV_MAIN", plan.entry())
            .stdin(Stdio::null());
        nabic_core::strip_handoffs(&mut c);
        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        {
            // SAFETY: async-signal-safe calls between fork and exec.
            unsafe {
                c.pre_exec(|| {
                    #[cfg(target_os = "linux")]
                    libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                    #[cfg(target_os = "freebsd")]
                    {
                        let mut sig = libc::SIGKILL;
                        libc::procctl(
                            libc::P_PID,
                            0,
                            libc::PROC_PDEATHSIG_CTL,
                            (&raw mut sig).cast(),
                        );
                    }
                    Ok(())
                });
            }
        }
        let child = c
            .spawn()
            .map_err(|e| format!("cannot run {}: {e}", exe.display()))?;
        Ok(Self { child, done: None })
    }

    /// How it ended, once it has.
    fn exited(&mut self) -> Option<ExitStatus> {
        if self.done.is_none() {
            self.done = self.child.try_wait().ok().flatten();
        }
        self.done
    }
}

impl Drop for Program {
    fn drop(&mut self) {
        if self.done.is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// The dev channel's server end: lines both ways, never blocking.
struct Channel {
    /// What `NABI_DEV` holds.
    endpoint: String,
    sys: sys::Server,
    /// Lines to send once the program connects.
    queue: Vec<u8>,
    buf: Vec<u8>,
}

impl Channel {
    fn open(dev: &Path) -> std::io::Result<Self> {
        let (sys, endpoint) = sys::Server::open(dev)?;
        Ok(Self {
            endpoint,
            sys,
            queue: Vec::new(),
            buf: Vec::new(),
        })
    }

    fn send(&mut self, line: &str) -> std::io::Result<()> {
        self.queue.extend_from_slice(line.as_bytes());
        self.queue.push(b'\n');
        self.flush()
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if self.queue.is_empty() || !self.sys.connect() {
            return Ok(());
        }
        let n = self.sys.write(&self.queue)?;
        self.queue.drain(..n);
        Ok(())
    }

    /// The complete lines read so far.
    fn lines(&mut self) -> Vec<String> {
        let _ = self.flush();
        if self.sys.connect() {
            self.sys.read(&mut self.buf);
        }
        let mut out = Vec::new();
        while let Some(i) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=i).collect();
            let line = String::from_utf8_lossy(&line[..i]);
            out.push(line.trim_end_matches('\r').to_owned());
        }
        out
    }
}

#[cfg(unix)]
mod sys {
    use std::io::{ErrorKind, Read, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};

    pub(super) struct Server {
        listener: UnixListener,
        stream: Option<UnixStream>,
        path: PathBuf,
    }

    impl Server {
        pub(super) fn open(dev: &Path) -> std::io::Result<(Self, String)> {
            let mut path = dev.join("dev.sock");
            // `sun_path` holds about 100 bytes.
            if path.as_os_str().len() > 100 {
                path = std::env::temp_dir().join(format!("nabi-dev-{}.sock", std::process::id()));
            }
            let _ = std::fs::remove_file(&path);
            let listener = UnixListener::bind(&path)?;
            listener.set_nonblocking(true)?;
            let endpoint = path.display().to_string();
            Ok((
                Self {
                    listener,
                    stream: None,
                    path,
                },
                endpoint,
            ))
        }

        /// Whether the program is connected (accepting it if it waits).
        pub(super) fn connect(&mut self) -> bool {
            if self.stream.is_none() {
                if let Ok((s, _)) = self.listener.accept() {
                    if s.set_nonblocking(true).is_ok() {
                        self.stream = Some(s);
                    }
                }
            }
            self.stream.is_some()
        }

        pub(super) fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            let Some(s) = &mut self.stream else {
                return Ok(0);
            };
            match s.write(b) {
                Ok(n) => Ok(n),
                Err(e) if e.kind() == ErrorKind::WouldBlock => Ok(0),
                Err(e) => Err(e),
            }
        }

        pub(super) fn read(&mut self, buf: &mut Vec<u8>) {
            let Some(s) = &mut self.stream else {
                return;
            };
            let mut chunk = [0u8; 4096];
            loop {
                match s.read(&mut chunk) {
                    Ok(0) => {
                        self.stream = None;
                        return;
                    }
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    Err(e) if e.kind() == ErrorKind::Interrupted => {}
                    Err(_) => return,
                }
            }
        }
    }

    impl Drop for Server {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[cfg(windows)]
mod sys {
    //! A named pipe in non-blocking mode: connect, read and write all
    //! return at once.

    use std::fs::File;
    use std::io::{Read, Write};
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    use std::path::Path;
    use std::sync::atomic::{AtomicU32, Ordering};

    type Handle = *mut std::ffi::c_void;

    unsafe extern "system" {
        fn CreateNamedPipeW(
            name: *const u16,
            open: u32,
            mode: u32,
            instances: u32,
            out: u32,
            inb: u32,
            timeout: u32,
            sa: *mut std::ffi::c_void,
        ) -> Handle;
        fn ConnectNamedPipe(h: Handle, ov: *mut std::ffi::c_void) -> i32;
        fn GetLastError() -> u32;
    }

    const PIPE_ACCESS_DUPLEX: u32 = 3;
    const FILE_FLAG_FIRST_PIPE_INSTANCE: u32 = 0x0008_0000;
    const PIPE_NOWAIT: u32 = 1;
    const PIPE_REJECT_REMOTE_CLIENTS: u32 = 8;
    const ERROR_PIPE_CONNECTED: u32 = 535;
    const ERROR_NO_DATA: i32 = 232;

    pub(super) struct Server {
        pipe: File,
        connected: bool,
    }

    impl Server {
        pub(super) fn open(_dev: &Path) -> std::io::Result<(Self, String)> {
            static N: AtomicU32 = AtomicU32::new(0);
            let name = format!(
                r"\\.\pipe\nabi-dev-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            );
            let wide: Vec<u16> = name.encode_utf16().chain([0]).collect();
            // SAFETY: a NUL-terminated name; no security attributes.
            let h = unsafe {
                CreateNamedPipeW(
                    wide.as_ptr(),
                    PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
                    PIPE_NOWAIT | PIPE_REJECT_REMOTE_CLIENTS,
                    1,
                    4096,
                    4096,
                    0,
                    std::ptr::null_mut(),
                )
            };
            if h as isize == -1 {
                return Err(std::io::Error::last_os_error());
            }
            // SAFETY: a handle this process owns from here on.
            let pipe = unsafe { File::from_raw_handle(h) };
            Ok((
                Self {
                    pipe,
                    connected: false,
                },
                name,
            ))
        }

        pub(super) fn connect(&mut self) -> bool {
            if !self.connected {
                // SAFETY: the pipe's handle; in no-wait mode this returns at once.
                let ok =
                    unsafe { ConnectNamedPipe(self.pipe.as_raw_handle(), std::ptr::null_mut()) };
                // SAFETY: no other call between.
                self.connected = ok != 0 || unsafe { GetLastError() } == ERROR_PIPE_CONNECTED;
            }
            self.connected
        }

        pub(super) fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            match self.pipe.write(b) {
                Ok(n) => Ok(n),
                Err(e) if e.raw_os_error() == Some(ERROR_NO_DATA) => Ok(0),
                Err(e) => Err(e),
            }
        }

        pub(super) fn read(&mut self, buf: &mut Vec<u8>) {
            let mut chunk = [0u8; 4096];
            loop {
                match self.pipe.read(&mut chunk) {
                    Ok(0) | Err(_) => return,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::io::{BufRead, BufReader, Write};

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|a| (*a).to_owned()).collect()
    }

    #[test]
    fn takes_its_options() {
        let o = options(&s(&["-p", "game", "--features", "x", "--", "-p"])).unwrap();
        assert_eq!(o.member.as_deref(), Some("game"));
        assert_eq!(o.rest, ["--features", "x"]);
        let o = options(&s(&["app", "--bin=app", "-O0", "-g"])).unwrap();
        assert_eq!(o.path.as_deref(), Some("app"));
        assert_eq!(o.bin.as_deref(), Some("app"));
        assert_eq!(o.rest, ["-g"]);
        assert_eq!(options(&s(&["--release"])).unwrap_err(), DEBUG_ONLY);
        assert!(options(&s(&["-O2"])).unwrap_err().contains("drop `-O2`"));
        assert!(
            options(&s(&["--target", "x"]))
                .unwrap_err()
                .contains("drop `--target`")
        );
        assert!(options(&s(&["-p"])).unwrap_err().contains("`-p <name>`"));
    }

    #[test]
    fn restarts_on_manifests_and_toolchain() {
        let mut c = Changes::default();
        assert_eq!(restart_reason(&c), None);
        c.sources.push(0);
        assert_eq!(restart_reason(&c), None);
        c.manifests.push(PathBuf::from("/x/nabi.nabion"));
        assert!(restart_reason(&c).unwrap().ends_with("nabi.nabion changed"));
        c.toolchain = true;
        assert_eq!(restart_reason(&c).unwrap(), "the toolchain changed");
    }

    #[cfg(unix)]
    #[test]
    fn channel_lines() {
        let dir = nabic_core::tmp::test_dir("dev-channel");
        std::fs::create_dir_all(&dir).unwrap();
        let mut ch = Channel::open(&dir).unwrap();
        // Queued until the program connects.
        ch.send("load /a.dylib").unwrap();
        assert!(ch.lines().is_empty());
        let mut peer = std::os::unix::net::UnixStream::connect(&ch.endpoint).unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        assert!(ch.lines().is_empty());
        let mut line = String::new();
        BufReader::new(peer.try_clone().unwrap())
            .read_line(&mut line)
            .unwrap();
        assert_eq!(line, "load /a.dylib\n");
        peer.write_all(b"waiting: x\napplied v1: 1 function\npart")
            .unwrap();
        let mut got = Vec::new();
        let start = Instant::now();
        while got.len() < 2 && start.elapsed() < Duration::from_secs(5) {
            got.extend(ch.lines());
        }
        assert_eq!(got, ["waiting: x", "applied v1: 1 function"]);
        drop(ch);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
