//! The `nabi repl` session host: one process, runtime up once. The REPL
//! (`repl/nabirepl`) links the runtime whole with a `main` that
//! calls [`crate::repl::nabi_repl_host_main`], compiles each cell to an
//! object of the host's format (Mach-O, COFF, ELF; arm64 / `x86_64`),
//! and the host maps, links and runs it (`link::jit_cell`). Commands
//! arrive on stdin (a socket; Windows: a named pipe):
//!
//!   `cell <n> <stdin>\n` + n bytes of object: link it and run its
//!     `nabi_repl_cell` once; later cells link against it. `<stdin>` is
//!     what it reads (`cell_stdin`).
//!   `lib <n>\n` + an n-byte path: link every member of a static library,
//!     or load a shared one; reply `ok 0\n` or `err\n`.
//!
//! A cell replies `pid <p>\n`, then `ok <n>\n<shown value>` or `err <n>\n`
//! and why it failed (`why`); in between, `in\n` for each stdin read of
//! an `ask` cell.
//! `SIGINT` (Windows: Ctrl-Break to the host's group) ends a running cell
//! and is ignored between cells.
//!
//! A failed cell leaves the session as it was. POSIX: the cell runs in the
//! host while a forked copy waits and takes over if the cell's process
//! dies. Windows has no fork: the host saves memory (`checkpoint`), runs
//! the cell from a recovery point, and on failure ends its threads, puts
//! memory back (`rollback`) and returns there (`run_cell`).
//!
//! A symbol redefined by a later cell is renamed `<name>.r<cell>`; later
//! references resolve to the newest, earlier code keeps its own.
//!
//! The loader is one linker over generic sections, symbols and relocations
//! (`link::Obj`, `link::fix`), fed by per-format readers (`map_object`,
//! `link_object`). Thread-locals are the host's: via TLV descriptors
//! (Mach-O), thread-pointer offsets (ELF initial exec) or TLS block offsets
//! (COFF `_tls_index`, SECREL).
//!
//! Most of it is raw memory: objects, mappings and (Windows) the whole
//! address space. State is global, as signal and console handlers reach it.

// Relocation arithmetic reads best in the formats' own short names.
#![allow(clippy::many_single_char_names)]

#[cfg(windows)]
mod coff;
#[cfg(target_os = "linux")]
mod elf;
mod link;
#[cfg(target_os = "macos")]
mod macho;
#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

use crate::NabiStr;
use core::cell::UnsafeCell;
use core::ffi::{CStr, c_void};
use core::fmt::{self, Write as _};
use core::ptr;
use core::sync::atomic::{AtomicBool, Ordering};
#[cfg(target_os = "linux")]
use elf::{SYM_PREFIX, host_symbol, host_tls};
#[cfg(target_os = "macos")]
use macho::{SYM_PREFIX, host_symbol, host_tls};
#[cfg(windows)]
use nabir_common::win;
#[cfg(windows)]
use windows::{host_symbol, host_tls};

/// C names carry no prefix on Windows.
#[cfg(windows)]
const SYM_PREFIX: usize = 0;

/// The host's entry: what nabic's `main` object calls.
///
/// # Safety
/// `argv` holds `argc` C strings, as `main` gets them.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nabi_repl_host_main(argc: i32, argv: *const *const u8) -> i32 {
    // SAFETY: caller contract.
    #[cfg(unix)]
    return unsafe { unix::main(argc, argv) };
    #[cfg(windows)]
    {
        let _ = (argc, argv);
        // SAFETY: once, on the main thread.
        unsafe { windows::main() }
    }
}

/// A global the host and its handlers share, as C would.
struct Global<T>(UnsafeCell<T>);

// SAFETY: the host runs on one thread; handlers touch only what the
// protocol allows them to (flags, `why` just before the process ends).
unsafe impl<T> Sync for Global<T> {}

impl<T> Global<T> {
    const fn new(v: T) -> Self {
        Self(UnsafeCell::new(v))
    }

    const fn get(&self) -> *mut T {
        self.0.get()
    }
}

/// `N - 1` bytes at most, NUL-terminated; more is dropped.
struct Buf<const N: usize> {
    b: [u8; N],
    len: usize,
}

impl<const N: usize> Buf<N> {
    const fn new() -> Self {
        Self { b: [0; N], len: 0 }
    }

    fn push(&mut self, s: &[u8]) {
        let n = s.len().min(N - 1 - self.len);
        self.b[self.len..self.len + n].copy_from_slice(&s[..n]);
        self.len += n;
        self.b[self.len] = 0;
    }

    fn truncate(&mut self, len: usize) {
        self.len = len;
        self.b[len] = 0;
    }

    fn bytes(&self) -> &[u8] {
        &self.b[..self.len]
    }

    #[cfg(unix)]
    const fn as_ptr(&self) -> *const u8 {
        self.b.as_ptr()
    }
}

impl<const N: usize> fmt::Write for Buf<N> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.push(s.as_bytes());
        Ok(())
    }
}

/// Straight to stderr.
struct Stderr;

impl fmt::Write for Stderr {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        // SAFETY: `s` is valid for its own length.
        unsafe { nabir_common::write_stderr(s.as_ptr(), s.len()) };
        Ok(())
    }
}

fn report_args(args: fmt::Arguments<'_>) {
    let _ = Stderr.write_fmt(args);
}

macro_rules! report {
    ($($t:tt)*) => {
        $crate::repl::report_args(format_args!($($t)*))
    };
}
use report;

/// Bytes shown as text (names, paths).
struct Text<'a>(&'a [u8]);

impl fmt::Display for Text<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for chunk in self.0.utf8_chunks() {
            f.write_str(chunk.valid())?;
            if !chunk.invalid().is_empty() {
                f.write_char(char::REPLACEMENT_CHARACTER)?;
            }
        }
        Ok(())
    }
}

/// The bytes of C string `p`.
///
/// # Safety
/// `p` is NUL-terminated and outlives the use.
const unsafe fn cstr<'a>(p: *const u8) -> &'a [u8] {
    // SAFETY: caller contract.
    unsafe { CStr::from_ptr(p.cast()).to_bytes() }
}

/// `s` NUL-terminated in fresh `malloc` memory.
fn dup(s: &[u8]) -> *mut u8 {
    // SAFETY: plain libc call; the copy fits.
    unsafe {
        let p = libc::malloc(s.len() + 1).cast::<u8>();
        if p.is_null() {
            nabir_common::oom();
        }
        ptr::copy_nonoverlapping(s.as_ptr(), p, s.len());
        *p.add(s.len()) = 0;
        p
    }
}

// Set by Ctrl-C (SIGINT, Ctrl-Break) while `quiesce` waits.
static QUIESCE_CANCEL: AtomicBool = AtomicBool::new(false);
static QUIESCING: AtomicBool = AtomicBool::new(false);

/// Stop the world at a quiet moment before the cell (`nabi_session_hold`:
/// no task mid-poll, no job mid-run, no thread in a critical section) so a
/// restored session loses nothing its threads did. false: Ctrl-C
/// cancelled the wait (and the cell).
fn quiesce() -> bool {
    QUIESCE_CANCEL.store(false, Ordering::SeqCst);
    QUIESCING.store(true, Ordering::SeqCst);
    let mut ok = true;
    let mut tries = 0;
    while crate::session::nabi_session_hold(100_000_000) == 0 {
        if QUIESCE_CANCEL.load(Ordering::SeqCst) {
            crate::session::nabi_session_unhold();
            ok = false;
            break;
        }
        if tries == 10 {
            report!(
                "note: waiting for running tasks, blocking jobs and lock holders to reach a safe \
                 point before the cell (Ctrl-C cancels it)\n"
            );
        }
        tries += 1;
    }
    QUIESCING.store(false, Ordering::SeqCst);
    ok
}

/// The cell's shown value (a nabi `str`), which the cell sets.
#[repr(C)]
pub struct ReplOut {
    ptr: *const u8,
    len: usize,
}

#[unsafe(no_mangle)]
#[allow(non_upper_case_globals)]
pub static mut __nabi_repl_out: ReplOut = ReplOut {
    ptr: ptr::null(),
    len: 0,
};

/// The shown value's bytes.
fn shown() -> &'static [u8] {
    // SAFETY: the cell sets a valid `str`, or none (0 bytes).
    unsafe {
        let out = &raw const __nabi_repl_out;
        if (*out).len == 0 {
            return &[];
        }
        core::slice::from_raw_parts((*out).ptr, (*out).len)
    }
}

fn clear_shown() {
    // SAFETY: the host's own static, between cells.
    unsafe { __nabi_repl_out.len = 0 };
}

/// Cells and libraries linked so far; its address is a place in the image.
static CELLS: Global<u32> = Global::new(0);

fn cells() -> u32 {
    // SAFETY: the host's thread only.
    unsafe { *CELLS.get() }
}

/// Why the running cell is ending early, for the REPL: `exit <code>`,
/// `panic`, `interrupted` or `throw\n<where>\n<error>`; empty: it failed
/// otherwise.
const WHY: usize = 480;
static WHY_BUF: Global<([u8; WHY], usize)> = Global::new(([0; WHY], 0));

fn add_why(s: &[u8]) {
    // SAFETY: the cell's thread, or a handler as the process ends.
    unsafe {
        let w = WHY_BUF.get();
        let at = (*w).1;
        let n = s.len().min(WHY - at);
        (*w).0[at..at + n].copy_from_slice(&s[..n]);
        (*w).1 = at + n;
    }
}

fn set_why(s: &[u8]) {
    // SAFETY: as `add_why`.
    unsafe { (*WHY_BUF.get()).1 = 0 };
    add_why(s);
}

fn why() -> &'static [u8] {
    // SAFETY: as `add_why`.
    unsafe {
        let w = WHY_BUF.get();
        &(*w).0[..(*w).1]
    }
}

// Cells reach `exit` and an error escaping the cell through these, which
// note why the cell ends (`resolve`).
extern "C" fn host_exit(code: i32) -> ! {
    let mut s = Buf::<32>::new();
    let _ = write!(s, "exit {code}");
    set_why(s.bytes());
    crate::nabi_exit(code)
}

/// `throw\n<where>\n<raised at>\n<error>`; `<raised at>` is empty when it
/// is not known or is `<where>`.
extern "C" fn host_uncaught_at(what: NabiStr, at: NabiStr) -> ! {
    let site = crate::nabi_throw_site_take();
    set_why(b"throw\n");
    add_why(at.bytes());
    add_why(b"\n");
    if site.len != 0 && site.bytes() != at.bytes() {
        add_why(site.bytes());
    }
    add_why(b"\n");
    add_why(what.bytes());
    crate::nabi_exit(1)
}

extern "C" fn host_uncaught(what: NabiStr) -> ! {
    host_uncaught_at(
        what,
        NabiStr {
            ptr: b"".as_ptr(),
            len: 0,
        },
    )
}

/// Symbols the session defines: name -> its newest definition's name (and
/// a definition's address under its own name). Strings are `malloc`ed,
/// NUL-terminated.
struct Sym {
    name: *mut u8,
    now: *mut u8,
    addr: *mut u8,
    next: *mut Sym,
}

const NSYMS: usize = 4096;
static SYMS: Global<[*mut Sym; NSYMS]> = Global::new([ptr::null_mut(); NSYMS]);

/// The chain link holding `name`, or the null one ending its chain.
fn sym_slot(name: &[u8]) -> *mut *mut Sym {
    let mut h: u32 = 2_166_136_261;
    for &c in name {
        h = (h ^ u32::from(c)).wrapping_mul(16_777_619);
    }
    // SAFETY: the table and its chains are the host's own.
    unsafe {
        let mut p = SYMS.get().cast::<*mut Sym>().add(h as usize % NSYMS);
        while !(*p).is_null() && cstr((**p).name) != name {
            p = &raw mut (**p).next;
        }
        p
    }
}

fn sym_now(name: &[u8]) -> Option<&'static [u8]> {
    let s = sym_slot(name);
    // SAFETY: a live entry.
    unsafe { (!(*s).is_null()).then(|| cstr((**s).now)) }
}

fn sym_set(name: &[u8], now: &[u8]) {
    let p = sym_slot(name);
    // SAFETY: the slot is the table's; entries are `calloc`ed.
    unsafe {
        if (*p).is_null() {
            let s = libc::calloc(1, size_of::<Sym>()).cast::<Sym>();
            if s.is_null() {
                nabir_common::oom();
            }
            (*s).name = dup(name);
            *p = s;
        }
        libc::free((**p).now.cast());
        (**p).now = dup(now);
    }
}

fn sym_addr(now: &[u8]) -> *mut u8 {
    let s = sym_slot(now);
    // SAFETY: a live entry.
    unsafe {
        if (*s).is_null() {
            ptr::null_mut()
        } else {
            (**s).addr
        }
    }
}

/// A definition of `name` at `addr`: renamed `<name>.r<cell>` when an
/// earlier cell has one (later cells use the newest).
fn define(name: &[u8], addr: *mut u8) {
    let mut fresh = Buf::<1024>::new();
    let renamed = sym_now(name).is_some();
    if renamed {
        fresh.push(name);
        let _ = write!(fresh, ".r{}", cells());
    }
    let as_ = if renamed { fresh.bytes() } else { name };
    sym_set(name, as_);
    if renamed {
        sym_set(as_, as_);
    }
    // SAFETY: just set.
    unsafe { (**sym_slot(as_)).addr = addr };
}

/// `name` without the C prefix.
///
/// # Safety
/// `name` is a C string.
unsafe fn bare(name: *const u8) -> &'static [u8] {
    // SAFETY: caller contract.
    let s = unsafe { cstr(name) };
    s.get(SYM_PREFIX..).unwrap_or(s)
}

/// An undefined symbol: the session's newest definition, else the host's
/// (a thread-local: what `host_tls` gives).
///
/// # Safety
/// `name` is a C string.
unsafe fn resolve(name: *const u8, tls: bool) -> *mut c_void {
    // SAFETY: caller contract.
    match unsafe { bare(name) } {
        b"nabi_exit" => return host_exit as *const () as *mut c_void,
        b"nabi_uncaught_error" => return host_uncaught as *const () as *mut c_void,
        b"nabi_uncaught_error_at" => return host_uncaught_at as *const () as *mut c_void,
        _ => {}
    }
    // SAFETY: caller contract.
    let full = unsafe { cstr(name) };
    if let Some(now) = sym_now(full) {
        let a = sym_addr(now);
        if !a.is_null() {
            return a.cast();
        }
    }
    // SAFETY: caller contract.
    unsafe {
        if tls {
            return host_tls(name);
        }
        #[cfg(unix)]
        {
            let a = libc::dlsym(unix::RTLD_DEFAULT, name.add(SYM_PREFIX).cast());
            if !a.is_null() {
                return a;
            }
        }
        host_symbol(name)
    }
}

// The command channel: our stdin at start.
#[cfg(unix)]
type Handle = i32;
#[cfg(windows)]
type Handle = nabir_common::win::HANDLE;

static CTL: Global<Handle> = Global::new(0);

fn ctl() -> Handle {
    // SAFETY: set once at start.
    unsafe { *CTL.get() }
}

fn ctl_read(buf: *mut u8, n: usize) -> isize {
    #[cfg(unix)]
    loop {
        // SAFETY: caller's buffer of `n` bytes.
        let r = unsafe { libc::read(ctl(), buf.cast(), n) };
        if r >= 0 || nabir_common::errno() != libc::EINTR {
            return r;
        }
    }
    #[cfg(windows)]
    {
        let mut got = 0;
        // SAFETY: caller's buffer of `n` bytes.
        let ok = unsafe {
            nabir_common::win::ReadFile(ctl(), buf.cast(), n as u32, &raw mut got, ptr::null_mut())
        };
        if ok != 0 { got as isize } else { -1 }
    }
}

fn ctl_write(b: &[u8]) {
    #[cfg(unix)]
    // SAFETY: `b` is valid for its length.
    unsafe {
        libc::write(ctl(), b.as_ptr().cast(), b.len());
    }
    #[cfg(windows)]
    {
        let mut put = 0;
        // SAFETY: `b` is valid for its length.
        unsafe {
            nabir_common::win::WriteFile(
                ctl(),
                b.as_ptr().cast(),
                b.len() as u32,
                &raw mut put,
                ptr::null_mut(),
            );
        }
    }
}

/// `ctl_write` of formatted text.
fn ctl_print(args: fmt::Arguments<'_>) {
    let mut s = Buf::<64>::new();
    let _ = s.write_fmt(args);
    ctl_write(s.bytes());
}

/// A command line: `<word> <n>`.
fn read_command<const N: usize>(line: &mut Buf<N>) -> bool {
    line.truncate(0);
    let mut c = 0u8;
    while ctl_read(&raw mut c, 1) == 1 && c != b'\n' {
        line.push(&[c]);
    }
    line.len > 0
}

/// `n` bytes off the channel, `malloc`ed with a byte to spare; null: it
/// ended first.
fn read_ir(n: usize) -> *mut u8 {
    // SAFETY: plain libc calls into the fresh buffer.
    unsafe {
        let ir = libc::malloc(n + 1).cast::<u8>();
        let mut got = 0;
        while !ir.is_null() && got < n {
            let r = ctl_read(ir.add(got), n - got);
            if r <= 0 {
                libc::free(ir.cast());
                return ptr::null_mut();
            }
            got += r as usize;
        }
        ir
    }
}

/// What a command line asks for.
enum Command<'a> {
    Lib(usize),
    In(usize),
    Cell(usize, &'a [u8]),
    Other,
}

/// Skip blanks, then a decimal number; the rest after it.
fn number(s: &[u8]) -> Option<(usize, &[u8])> {
    let s = s.trim_ascii_start();
    let digits = s.iter().take_while(|c| c.is_ascii_digit()).count();
    if digits == 0 {
        return None;
    }
    let mut n = 0usize;
    for &c in &s[..digits] {
        n = n.wrapping_mul(10).wrapping_add(usize::from(c - b'0'));
    }
    Some((n, &s[digits..]))
}

fn parse(line: &[u8]) -> Command<'_> {
    let arg = |word: &[u8]| line.strip_prefix(word).and_then(number);
    if let Some((n, _)) = arg(b"lib") {
        return Command::Lib(n);
    }
    if let Some((n, _)) = arg(b"in") {
        return Command::In(n);
    }
    if let Some((n, rest)) = arg(b"cell") {
        // The stdin mode, 7 bytes at most; `-` when missing.
        let rest = rest.trim_ascii_start();
        let len = rest
            .iter()
            .take_while(|c| !c.is_ascii_whitespace())
            .count()
            .min(7);
        let mode = if len == 0 { &b"-"[..] } else { &rest[..len] };
        return Command::Cell(n, mode);
    }
    Command::Other
}

/// A cell's stdin, the third word of `cell`: `-` (empty), `tty` (the
/// terminal) or `ask`: each read asks the REPL with `in\n`, answered by
/// `in <n>\n` and n bytes (0: end of input).
struct Asked {
    buf: *mut u8,
    len: usize,
    at: usize,
}

static ASKED: Global<Asked> = Global::new(Asked {
    buf: ptr::null_mut(),
    len: 0,
    at: 0,
});

fn drop_asked() {
    // SAFETY: the host's thread only.
    unsafe {
        let a = ASKED.get();
        libc::free((*a).buf.cast());
        (*a).buf = ptr::null_mut();
        (*a).len = 0;
        (*a).at = 0;
    }
}

extern "C" fn ask_stdin(dst: *mut u8, len: usize) -> isize {
    let a = ASKED.get();
    // SAFETY: the reading thread is the cell's; `dst` holds `len` bytes.
    unsafe {
        if (*a).at == (*a).len {
            drop_asked();
            ctl_write(b"in\n");
            let mut line = Buf::<64>::new();
            if !read_command(&mut line) {
                return 0;
            }
            let Some((n, _)) = line.bytes().strip_prefix(b"in").and_then(number) else {
                return 0;
            };
            if n == 0 {
                return 0;
            }
            (*a).buf = read_ir(n);
            if (*a).buf.is_null() {
                return 0;
            }
            (*a).len = n;
        }
        let k = ((*a).len - (*a).at).min(len);
        ptr::copy_nonoverlapping((*a).buf.add((*a).at), dst, k);
        (*a).at += k;
        k as isize
    }
}

/// The cells' stdin when not the terminal, and the terminal once opened.
#[cfg(unix)]
static STDIN: Global<(Handle, Handle)> = Global::new((-1, -1));
#[cfg(windows)]
static STDIN: Global<(Handle, Handle)> = Global::new((0, 0));

fn cell_stdin(mode: &[u8]) {
    let tty = mode == b"tty";
    // SAFETY: the host's thread only; plain OS calls.
    unsafe {
        let s = STDIN.get();
        #[cfg(unix)]
        {
            if tty && (*s).1 < 0 {
                (*s).1 = libc::open(c"/dev/tty".as_ptr(), libc::O_RDWR | libc::O_CLOEXEC);
            }
            libc::dup2(if tty && (*s).1 >= 0 { (*s).1 } else { (*s).0 }, 0);
        }
        #[cfg(windows)]
        {
            if tty && (*s).1 == 0 {
                (*s).1 = windows::CreateFileA(
                    c"CONIN$".as_ptr().cast(),
                    win::GENERIC_READ | win::GENERIC_WRITE,
                    win::FILE_SHARE_READ | win::FILE_SHARE_WRITE,
                    ptr::null_mut(),
                    win::OPEN_EXISTING,
                    0,
                    0,
                );
            }
            let h = if tty && (*s).1 != win::INVALID_HANDLE_VALUE {
                (*s).1
            } else {
                (*s).0
            };
            windows::SetStdHandle(win::STD_INPUT_HANDLE, h);
        }
    }
    drop_asked();
    crate::stdio::nabi_set_stdin_hook(if mode == b"ask" {
        Some(ask_stdin)
    } else {
        None
    });
}

/// The next command, read into `line`, with what it brings: a `lib` is
/// handled here, input for a cell that has ended dropped; `Some` a cell's
/// object (`malloc`ed, `n` bytes; null: the channel ended), `None` to read
/// on.
fn next_cell<const N: usize>(line: &Buf<N>) -> Option<(*mut u8, usize)> {
    match parse(line.bytes()) {
        Command::Lib(n) => {
            let path = read_ir(n);
            if path.is_null() {
                return Some((ptr::null_mut(), 0));
            }
            // SAFETY: `read_ir` leaves a byte to spare.
            let bad = unsafe {
                *path.add(n) = 0;
                let bad = link::link_lib(path);
                libc::free(path.cast());
                bad
            };
            ctl_write(if bad { b"err\n" } else { b"ok 0\n" });
            None
        }
        Command::In(n) => {
            // SAFETY: `malloc`ed or null.
            unsafe { libc::free(read_ir(n).cast()) };
            None
        }
        Command::Cell(n, mode) => {
            cell_stdin(mode);
            Some((read_ir(n), n))
        }
        Command::Other => Some((ptr::null_mut(), 0)),
    }
}
