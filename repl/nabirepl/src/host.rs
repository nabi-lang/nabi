//! The session host: its process, channel and Ctrl-C handling.

#[allow(clippy::wildcard_imports)]
use crate::*;

/// The host and its channel. Field order matters: the channel closes
/// (host sees EOF) before the host is waited for.
pub(crate) struct Host {
    ctl: Ctl,
    _child: Reaped,
    _job: Option<Job>,
}

pub(crate) struct Reaped(Child);

impl Drop for Reaped {
    fn drop(&mut self) {
        let _ = self.0.wait();
    }
}

/// Windows: a job that ends the host when its last handle, ours, closes:
/// when the REPL process exits, however it ends. POSIX hosts watch their
/// parent themselves (`nabir/src/repl/unix.rs`).
#[cfg(windows)]
pub(crate) struct Job(*mut std::ffi::c_void);

// SAFETY: a job handle may be used and closed from any thread.
#[cfg(windows)]
unsafe impl Send for Job {}

#[cfg(windows)]
impl Job {
    pub(crate) fn ending(child: &Child) -> Option<Self> {
        type Handle = *mut std::ffi::c_void;
        unsafe extern "system" {
            fn CreateJobObjectW(security: *mut std::ffi::c_void, name: *const u16) -> Handle;
            fn SetInformationJobObject(job: Handle, class: i32, info: *const u8, len: u32) -> i32;
            fn AssignProcessToJobObject(job: Handle, process: Handle) -> i32;
        }
        #[repr(C)]
        #[derive(Default)]
        struct Limits {
            per_process_time: i64,
            per_job_time: i64,
            flags: u32,
            min_working_set: usize,
            max_working_set: usize,
            active_processes: u32,
            affinity: usize,
            priority_class: u32,
            scheduling_class: u32,
            io: [u64; 6],
            process_memory: usize,
            job_memory: usize,
            peak_process_memory: usize,
            peak_job_memory: usize,
        }
        const EXTENDED_LIMIT_INFORMATION: i32 = 9;
        const KILL_ON_JOB_CLOSE: u32 = 0x2000;
        // SAFETY: an unnamed job with default security, not inherited.
        let job = Self(unsafe { CreateJobObjectW(std::ptr::null_mut(), std::ptr::null()) });
        if job.0.is_null() {
            return None;
        }
        let limits = Limits {
            flags: KILL_ON_JOB_CLOSE,
            ..Limits::default()
        };
        // SAFETY: `limits` has the layout of
        // JOBOBJECT_EXTENDED_LIMIT_INFORMATION; both handles are live.
        let ok = unsafe {
            SetInformationJobObject(
                job.0,
                EXTENDED_LIMIT_INFORMATION,
                (&raw const limits).cast(),
                std::mem::size_of::<Limits>() as u32,
            ) != 0
                && AssignProcessToJobObject(job.0, child.as_raw_handle()) != 0
        };
        if !ok {
            return None;
        }
        JOBS.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(job.0 as usize);
        nabic_core::tmp::on_release(end_jobs);
        Some(job)
    }
}

/// Live jobs, for an exit that skips drops: the session directory can
/// only go once its host.exe has ended.
#[cfg(windows)]
pub(crate) static JOBS: std::sync::Mutex<Vec<usize>> = std::sync::Mutex::new(Vec::new());

#[cfg(windows)]
pub(crate) fn end_jobs() {
    type Handle = *mut std::ffi::c_void;
    unsafe extern "system" {
        fn TerminateJobObject(job: Handle, code: u32) -> i32;
        fn QueryInformationJobObject(
            job: Handle,
            class: i32,
            info: *mut u8,
            len: u32,
            ret: *mut u32,
        ) -> i32;
    }
    #[repr(C)]
    #[derive(Default)]
    struct Accounting {
        times: [i64; 4],
        page_faults: u32,
        total_processes: u32,
        active_processes: u32,
        terminated_processes: u32,
    }
    const BASIC_ACCOUNTING_INFORMATION: i32 = 1;
    let Ok(jobs) = JOBS.try_lock() else { return };
    for &j in jobs.iter() {
        let job = j as Handle;
        // SAFETY: a job handle that is ours until `Job::drop`, which also
        // takes it out of `JOBS`.
        unsafe { TerminateJobObject(job, 1) };
        for _ in 0..200 {
            let mut a = Accounting::default();
            // SAFETY: `a` has the layout of
            // JOBOBJECT_BASIC_ACCOUNTING_INFORMATION.
            let ok = unsafe {
                QueryInformationJobObject(
                    job,
                    BASIC_ACCOUNTING_INFORMATION,
                    (&raw mut a).cast(),
                    std::mem::size_of::<Accounting>() as u32,
                    std::ptr::null_mut(),
                ) != 0
            };
            if !ok || a.active_processes == 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}

#[cfg(windows)]
impl Drop for Job {
    fn drop(&mut self) {
        unsafe extern "system" {
            fn CloseHandle(h: *mut std::ffi::c_void) -> i32;
        }
        JOBS.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|&j| j != self.0 as usize);
        // SAFETY: the job handle is ours.
        unsafe { CloseHandle(self.0) };
    }
}

#[cfg(not(windows))]
pub(crate) struct Job;

#[cfg(not(windows))]
impl Job {
    #[allow(clippy::unnecessary_wraps)]
    pub(crate) const fn ending(_: &Child) -> Option<Self> {
        None
    }
}

/// One stream both ways, the host's stdin. Unix: a socket pair. Windows:
/// a named pipe whose both ends this process opens.
#[cfg(unix)]
pub(crate) type Ctl = std::os::unix::net::UnixStream;

#[cfg(windows)]
pub(crate) type Ctl = std::fs::File;

/// Our end of a new channel, and the host's as its stdin.
#[cfg(unix)]
pub(crate) fn channel() -> std::io::Result<(Ctl, Stdio)> {
    let (ours, theirs) = Ctl::pair()?;
    Ok((ours, Stdio::from(std::os::fd::OwnedFd::from(theirs))))
}

#[cfg(windows)]
pub(crate) fn channel() -> std::io::Result<(Ctl, Stdio)> {
    unsafe extern "system" {
        fn CreateNamedPipeW(
            name: *const u16,
            open_mode: u32,
            pipe_mode: u32,
            instances: u32,
            out_size: u32,
            in_size: u32,
            timeout: u32,
            security: *mut std::ffi::c_void,
        ) -> *mut std::ffi::c_void;
    }
    const PIPE_ACCESS_DUPLEX: u32 = 0x3;
    const FILE_FLAG_FIRST_PIPE_INSTANCE: u32 = 0x0008_0000;
    const PIPE_REJECT_REMOTE_CLIENTS: u32 = 0x8;
    static N: AtomicU32 = AtomicU32::new(0);
    let name = format!(
        r"\\.\pipe\nabi-repl-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    );
    let wide: Vec<u16> = name.encode_utf16().chain([0]).collect();
    // SAFETY: a NUL-terminated name; one byte-mode instance, ours.
    let h = unsafe {
        CreateNamedPipeW(
            wide.as_ptr(),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_REJECT_REMOTE_CLIENTS,
            1,
            1 << 16,
            1 << 16,
            0,
            std::ptr::null_mut(),
        )
    };
    if h as isize == -1 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: a handle just created and owned by nothing else.
    let ours = unsafe { std::fs::File::from_raw_handle(h) };
    let theirs = std::fs::File::options()
        .read(true)
        .write(true)
        .open(&name)?;
    Ok((ours, Stdio::from(theirs)))
}

/// End the cell running in host process `pid`: `SIGINT`, or on Windows
/// Ctrl-Break to the host's own process group (`CREATE_NEW_PROCESS_GROUP`).
pub fn interrupt(pid: u32) {
    #[cfg(unix)]
    let _ = Command::new("kill")
        .args(["-INT", &pid.to_string()])
        .status();
    #[cfg(windows)]
    {
        unsafe extern "system" {
            fn GenerateConsoleCtrlEvent(event: u32, group: u32) -> i32;
        }
        const CTRL_BREAK_EVENT: u32 = 1;
        // SAFETY: plain call; a gone group is an error return.
        unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, pid) };
    }
}

/// Windows: the console sends Ctrl-C to the REPL process only (the
/// host's group ignores it); forward it to a running cell.
#[cfg(windows)]
pub(crate) fn forward_ctrl_c(running: Arc<AtomicU32>) {
    static RUNNING: std::sync::OnceLock<Arc<AtomicU32>> = std::sync::OnceLock::new();
    unsafe extern "system" fn handler(event: u32) -> i32 {
        const CTRL_C_EVENT: u32 = 0;
        if event != CTRL_C_EVENT {
            return 0;
        }
        let pid = RUNNING.get().map_or(0, |r| r.load(Ordering::Relaxed));
        if pid != 0 {
            interrupt(pid);
        }
        1
    }
    unsafe extern "system" {
        fn SetConsoleCtrlHandler(
            handler: Option<unsafe extern "system" fn(u32) -> i32>,
            add: i32,
        ) -> i32;
    }
    if RUNNING.set(running).is_ok() {
        // SAFETY: a handler for the process's lifetime.
        unsafe { SetConsoleCtrlHandler(Some(handler), 1) };
    }
}

/// Unix: Ctrl-C reaches the REPL process and the host alike; the host ends
/// the cell.
/// A handler, unlike `SIG_IGN`, is reset by `exec`, so the host doesn't
/// inherit it.
#[cfg(unix)]
pub(crate) fn survive_ctrl_c() {
    const extern "C" fn ignore(_: i32) {}
    unsafe extern "C" {
        fn signal(sig: i32, handler: usize) -> usize;
    }
    const SIGINT: i32 = 2;
    // SAFETY: installs a handler that does nothing.
    unsafe { signal(SIGINT, ignore as extern "C" fn(i32) as usize) };
}

impl Repl {
    /// Build and start the host in the background (the first cell
    /// compiles meanwhile).
    pub(crate) fn start_host(&mut self) {
        if self.host.is_none() && self.starting.is_none() {
            let (dir, opts, rich) = (self.tmp.path().to_owned(), self.opts.clone(), self.rich);
            self.starting = Some(std::thread::spawn(move || start_host(&dir, &opts, rich)));
        }
    }

    /// Record `fns`. `fresh` replaced earlier definitions, so everything
    /// compiled against those (transitively) is stale.
    pub(crate) fn learn(&mut self, fresh: Vec<String>, fns: Vec<ReplFn>) {
        let mut gone: HashSet<String> = fresh.into_iter().collect();
        loop {
            let stale: Vec<String> = self
                .known
                .iter()
                .filter(|(_, (_, refs))| refs.iter().any(|r| gone.contains(r)))
                .map(|(k, _)| k.clone())
                .collect();
            if stale.is_empty() {
                break;
            }
            for k in stale {
                self.known.remove(&k);
                gone.insert(k);
            }
        }
        for f in fns {
            self.known.insert(f.name, (f.hash, f.refs));
        }
    }

    /// The host, started on first use.
    pub(crate) fn host(&mut self) -> Option<&mut Host> {
        if self.host.is_none() {
            self.host = match self.starting.take() {
                Some(h) => h.join().ok().flatten(),
                None => start_host(self.tmp.path(), &self.opts, self.rich),
            };
        }
        self.host.as_mut()
    }

    /// Link the native packages' libraries, then the prebuilt
    /// dependencies `r` uses, into the host, once each.
    pub(crate) fn link_archives(&mut self, r: &CompileResult) {
        let mut files = native_files(&self.opts);
        files.extend(
            r.modules
                .first()
                .into_iter()
                .flat_map(|m| m.archives.clone()),
        );
        for a in &files {
            if self.linked.contains(a) {
                continue;
            }
            let Some(host) = self.host() else { return };
            let path = a.display().to_string();
            let reply = (|| {
                writeln!(host.ctl, "lib {}", path.len())?;
                host.ctl.write_all(path.as_bytes())?;
                let mut line = String::new();
                BufReader::new(&host.ctl).read_line(&mut line)?;
                Ok::<_, std::io::Error>(line)
            })();
            if reply.is_ok_and(|l| l.starts_with("ok")) {
                self.linked.insert(a.clone());
            } else {
                eprintln!("error: cannot link {path} into the session");
            }
        }
    }

    /// Send the host (started on first use) `cell <len>` and the
    /// cell's object; its shown value.
    pub(crate) fn load(&mut self, ir: &[u8]) -> Loaded {
        self.host()?;
        let host = self.host.as_mut()?;
        let _ = std::io::stdout().flush();
        let stdin = match self.stdin {
            CellStdin::Null => "-",
            CellStdin::Tty => "tty",
            CellStdin::Ask(_) => "ask",
        };
        let _term = matches!(self.stdin, CellStdin::Tty)
            .then(TermMode::save)
            .flatten();
        if let Some(live) = self.live {
            live(true);
        }
        let reply = (|| {
            writeln!(host.ctl, "cell {} {stdin}", ir.len())?;
            host.ctl.write_all(ir)?;
            let mut rd = BufReader::new(&host.ctl);
            let mut line = String::new();
            // `pid <n>`: the process the cell runs in.
            rd.read_line(&mut line)?;
            let pid = line
                .strip_prefix("pid ")
                .and_then(|p| p.trim().parse().ok());
            self.running.store(pid.unwrap_or(0), Ordering::Relaxed);
            loop {
                line.clear();
                rd.read_line(&mut line)?;
                if line != "in\n" {
                    break;
                }
                let got = match &mut self.stdin {
                    CellStdin::Ask(ask) => ask(),
                    _ => None,
                };
                let got = got.map(|l| l + "\n").unwrap_or_default();
                let mut ctl = *rd.get_ref();
                write!(ctl, "in {}\n{got}", got.len())?;
            }
            self.running.store(0, Ordering::Relaxed);
            // `ok <n>` and the shown value, or `err <n>` and why it failed.
            let (ok, n) = match line.trim_end().split_once(' ') {
                Some(("ok", n)) => (true, n),
                Some(("err", n)) => (false, n),
                _ => return Err(std::io::ErrorKind::InvalidData.into()),
            };
            let mut text = vec![0; n.parse().unwrap_or(0)];
            rd.read_exact(&mut text)?;
            let text = String::from_utf8_lossy(&text).into_owned();
            Ok::<_, std::io::Error>(if ok { Ok(text) } else { Err(text) })
        })();
        if let Some(live) = self.live {
            live(false);
        }
        match reply {
            Ok(Ok(value)) => Some(value),
            Ok(Err(why)) => {
                eprintln!("error: {}", failure(&why));
                None
            }
            Err(_) => {
                eprintln!("error: the session host is gone; the bindings are lost");
                self.host = None;
                self.linked.clear();
                self.bindings.clear();
                self.statics.clear();
                self.any_types.clear();
                self.gc_tables.clear();
                self.known.clear();
                None
            }
        }
    }
}

/// Native libraries for the host, in load order: shared ones (runtime
/// files; off Windows also `dylib`s in `-L` dirs), then static ones after
/// those they use.
pub(crate) fn native_files(opts: &BuildOptions) -> Vec<PathBuf> {
    let find = |f: String| {
        opts.lib_dirs
            .iter()
            .map(|d| d.join(&f))
            .find(|p| p.is_file())
    };
    let dylib = |n: &str| {
        let ext = if cfg!(target_vendor = "apple") {
            "dylib"
        } else {
            "so"
        };
        (!cfg!(windows)).then(|| format!("lib{n}.{ext}"))
    };
    let archive = |n: &str| {
        Some(if cfg!(windows) {
            format!("{n}.lib")
        } else {
            format!("lib{n}.a")
        })
    };
    let mut out: Vec<PathBuf> = opts.runtime_files.iter().rev().cloned().collect();
    for want_static in [false, true] {
        for (n, _) in opts.libs.iter().rev().filter(|l| l.1 == want_static) {
            let file = if want_static { archive(n) } else { dylib(n) };
            if let Some(p) = file.and_then(find) {
                push_new(&mut out, p);
            }
        }
    }
    out
}

pub(crate) fn push_new<T: PartialEq>(to: &mut Vec<T>, x: T) {
    if !to.contains(&x) {
        to.push(x);
    }
}

/// Build and start the session host.
pub(crate) fn start_host(dir: &std::path::Path, opts: &BuildOptions, rich: bool) -> Option<Host> {
    if !nabic_interface::repl_host() {
        eprintln!(
            "error: the REPL runs on arm64 macOS and on Linux glibc and Windows (arm64, x86_64) only \
             (a musl nabic is static: its session host cannot bind cells)"
        );
        return None;
    }
    let obj = dir.join("host.o");
    let exe = dir.join(format!("host{}", std::env::consts::EXE_SUFFIX));
    if let Err(e) = std::fs::write(&obj, HOST_OBJ) {
        eprintln!("error: cannot write {}: {e}", obj.display());
        return None;
    }
    if let Err(d) = link_host(&obj, opts, &exe) {
        SourceMap::new().report(&d);
        return None;
    }
    let (ctl, theirs) = match channel() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: cannot make the host channel: {e}");
            return None;
        }
    };
    let mut cmd = Command::new(&exe);
    cmd.stdin(theirs);
    // `display` writes its frames for the kernel only.
    if rich {
        cmd.env("NABI_JUPYTER", "1");
    }
    #[cfg(windows)]
    {
        // Its own group: `interrupt`'s Ctrl-Break reaches it alone.
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x200;
        std::os::windows::process::CommandExt::creation_flags(&mut cmd, CREATE_NEW_PROCESS_GROUP);
    }
    match cmd.spawn() {
        Ok(child) => Some(Host {
            ctl,
            _job: Job::ending(&child),
            _child: Reaped(child),
        }),
        Err(e) => {
            eprintln!("error: cannot run {}: {e}", exe.display());
            None
        }
    }
}
