//! Stack overflow report. Every nabi thread records the addresses just
//! below its stack (the guard page, and a little more for frames
//! without stack probes, e.g. C's) and gets an
//! alternate signal stack. A SIGSEGV / SIGBUS there flushes stdout,
//! prints `thread '<name>' has overflowed its stack` and aborts; any
//! other fault falls back to the default action.
//!
//! Covers the main thread (`nabi_runtime_init`), `nabi_thread_spawn`
//! threads (including executor workers, so deep `await` chains too) and
//! C threads that call `nabi_thread_init`.

use core::ptr;
use core::sync::atomic::{AtomicBool, Ordering};
use nabir_common::{tls, write_stderr};

static INSTALLED: AtomicBool = AtomicBool::new(false);
/// Alternate signal stack size (plus a guard page below it).
const ALT_SIZE: usize = 64 << 10;
/// How far below the stack a fault still counts as an overflow. nabi
/// and Rust frames probe each page, so theirs land in the guard page;
/// this covers unprobed frames up to this size.
const SLOP: usize = 1 << 20;

/// `[lowest usable stack address, guard bytes at / above it)` of the
/// calling thread, or `None` when the OS will not say.
#[cfg(target_vendor = "apple")]
fn stack_bounds() -> Option<(usize, usize)> {
    // SAFETY: queries about the calling thread.
    unsafe {
        let me = libc::pthread_self();
        let top = libc::pthread_get_stackaddr_np(me) as usize;
        let size = libc::pthread_get_stacksize_np(me);
        // The guard page sits below `top - size`.
        Some((top.checked_sub(size)?, 0))
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn stack_bounds() -> Option<(usize, usize)> {
    // SAFETY: `attr` is initialized by `pthread_getattr_np` before use
    // and destroyed after; out-pointers are valid.
    unsafe {
        let mut attr: libc::pthread_attr_t = core::mem::zeroed();
        if libc::pthread_getattr_np(libc::pthread_self(), &mut attr) != 0 {
            return None;
        }
        let mut addr: *mut libc::c_void = ptr::null_mut();
        let mut size = 0usize;
        let mut guard = 0usize;
        let ok = libc::pthread_attr_getstack(&attr, &mut addr, &mut size) == 0
            && libc::pthread_attr_getguardsize(&attr, &mut guard) == 0;
        libc::pthread_attr_destroy(&mut attr);
        if !ok {
            return None;
        }
        // musl reports the main thread's stack as far as it is mapped so
        // far; it grows down to RLIMIT_STACK below the end of its mapping,
        // where it overflows. The end lies above musl's top (the auxiliary
        // vector) by the argument and environment strings: the exec'd
        // file's name (AT_EXECFN) comes last, a null pointer after it.
        let mut lo = addr as usize;
        let mut rl: libc::rlimit = core::mem::zeroed();
        if libc::syscall(libc::SYS_gettid) == libc::c_long::from(libc::getpid())
            && libc::getrlimit(libc::RLIMIT_STACK, &mut rl) == 0
            && rl.rlim_cur != libc::RLIM_INFINITY
        {
            let mut top = lo + size;
            let name = libc::getauxval(libc::AT_EXECFN) as usize;
            if name != 0 {
                let end = name + libc::strlen(name as *const libc::c_char) + 1;
                let page = nabir_common::sys::page_size();
                top = top.max((end + size_of::<usize>()).next_multiple_of(page));
            }
            let max = usize::try_from(rl.rlim_cur).unwrap_or(usize::MAX);
            lo = lo.min(top.saturating_sub(max));
        }
        // glibc before 2.27 counts the guard inside the reported stack,
        // later ones put it below: take both sides of `addr`.
        Some((lo, guard))
    }
}

#[cfg(target_os = "freebsd")]
fn stack_bounds() -> Option<(usize, usize)> {
    // SAFETY: `attr` is initialized before `pthread_attr_get_np` fills it
    // and destroyed after; out-pointers are valid.
    unsafe {
        let mut attr: libc::pthread_attr_t = core::mem::zeroed();
        if libc::pthread_attr_init(&mut attr) != 0 {
            return None;
        }
        let mut addr: *mut libc::c_void = ptr::null_mut();
        let mut size = 0usize;
        let mut guard = 0usize;
        let ok = libc::pthread_attr_get_np(libc::pthread_self(), &mut attr) == 0
            && libc::pthread_attr_getstack(&attr, &mut addr, &mut size) == 0
            && libc::pthread_attr_getguardsize(&attr, &mut guard) == 0;
        libc::pthread_attr_destroy(&mut attr);
        // The guard sits below the reported stack.
        ok.then_some((addr as usize, guard))
    }
}

/// Set up the calling thread: its guard range and alternate signal
/// stack, and (once per process) the signal handlers.
pub fn thread_start() {
    // SAFETY: plain libc calls on this thread's own state.
    unsafe {
        let range =
            stack_bounds().map_or([0, 0], |(lo, guard)| [lo.saturating_sub(SLOP), lo + guard]);
        tls::stack_guard().write(range);
        let page = nabir_common::sys::page_size();
        let mem = libc::mmap(
            ptr::null_mut(),
            ALT_SIZE + page,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANON,
            -1,
            0,
        );
        if mem == libc::MAP_FAILED {
            return; // no report on this thread, nothing else changes
        }
        libc::mprotect(mem, page, libc::PROT_NONE);
        let ss = libc::stack_t {
            ss_sp: mem.cast::<u8>().add(page).cast(),
            ss_flags: 0,
            ss_size: ALT_SIZE,
        };
        libc::sigaltstack(&ss, ptr::null_mut());
        if !INSTALLED.swap(true, Ordering::AcqRel) {
            install(handler as *const () as libc::sighandler_t);
        }
    }
}

/// Undo `thread_start`'s alternate stack before the thread ends.
pub fn thread_exit() {
    // SAFETY: disables, then unmaps, the stack `thread_start` mapped.
    unsafe {
        let mut old: libc::stack_t = core::mem::zeroed();
        let off = libc::stack_t {
            ss_sp: ptr::null_mut(),
            ss_flags: libc::SS_DISABLE,
            ss_size: 0,
        };
        if libc::sigaltstack(&off, &mut old) != 0 || old.ss_sp.is_null() {
            return;
        }
        if old.ss_flags & libc::SS_DISABLE == 0 && old.ss_size == ALT_SIZE {
            let page = nabir_common::sys::page_size();
            libc::munmap(old.ss_sp.cast::<u8>().sub(page).cast(), ALT_SIZE + page);
        }
    }
}

/// The SIGSEGV and SIGBUS actions before ours (a host process's, when
/// the runtime is in a shared library): a fault that is not a stack
/// overflow goes back to them.
struct Prev(core::cell::UnsafeCell<[core::mem::MaybeUninit<libc::sigaction>; 2]>);

// SAFETY: written once by the `INSTALLED` winner before the handler can
// run, then only read.
unsafe impl Sync for Prev {}

static PREV: Prev = Prev(core::cell::UnsafeCell::new(
    [core::mem::MaybeUninit::uninit(); 2],
));

const SIGNALS: [libc::c_int; 2] = [libc::SIGSEGV, libc::SIGBUS];

/// Our `action` for both signals, keeping the actions it replaces.
unsafe fn install(action: libc::sighandler_t) {
    for (k, sig) in SIGNALS.into_iter().enumerate() {
        // SAFETY: a zeroed `sigaction` with the fields set below is a
        // valid argument; `PREV` is written only here, once.
        unsafe {
            let mut sa: libc::sigaction = core::mem::zeroed();
            sa.sa_sigaction = action;
            sa.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK;
            libc::sigemptyset(&mut sa.sa_mask);
            libc::sigaction(sig, &sa, (*PREV.0.get())[k].as_mut_ptr());
        }
    }
}

/// Put back the actions [`install`] replaced.
unsafe fn restore() {
    for (k, sig) in SIGNALS.into_iter().enumerate() {
        // SAFETY: saved by `install` before our handler was set.
        unsafe { libc::sigaction(sig, (*PREV.0.get())[k].as_ptr(), ptr::null_mut()) };
    }
}

extern "C" fn handler(_sig: libc::c_int, info: *mut libc::siginfo_t, _ctx: *mut libc::c_void) {
    // SAFETY: the kernel passes a valid `siginfo_t`; the TLS slot was
    // written by `thread_start` (or is zero).
    let (addr, [lo, hi]) = unsafe { ((*info).si_addr() as usize, *tls::stack_guard()) };
    if !(lo <= addr && addr < hi) {
        // Not ours: the earlier action (by default, the default one)
        // happens when the faulting instruction runs again.
        // SAFETY: restoring what `install` saved.
        unsafe { restore() };
        return;
    }
    crate::stdio::flush_in_signal();
    let name = crate::thread::name();
    // SAFETY: only async-signal-tolerant calls.
    unsafe {
        for part in [
            b"\nthread '".as_slice(),
            name,
            b"' has overflowed its stack\nfatal runtime error: stack overflow, aborting\n",
        ] {
            write_stderr(part.as_ptr(), part.len());
        }
        libc::abort();
    }
}
