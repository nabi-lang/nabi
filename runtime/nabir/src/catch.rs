//! Panics under foreign frames: abort, or return `#[on_panic]`'s value.
//!
//! A panic must never unwind or `exit` through C code that called Nabi:
//! the host's state is unknown. An `extern "C" fn` C entry that foreign
//! code called counts in this thread's `FOREIGN` slot; a panic below one
//! aborts. An entry marked `#[on_panic(return = v)]` pushes a record and
//! calls `_setjmp` on it; a panic directly below it (no other foreign
//! entry in between) restores the thread's shadow stack and `longjmp`s
//! back, and the entry returns `v`. Drops in the skipped frames do not run.

#![allow(clippy::cast_ptr_alignment)] // nabic allocates each record 16-aligned

use core::sync::atomic::{AtomicPtr, AtomicU32, Ordering};
use nabir_common::tls;

/// A recovery record, in the entry's frame. The entry's `_setjmp`
/// buffer follows at `JMP_AT`.
#[repr(C)]
struct Record {
    prev: *mut Record,
    shadow: *mut u8,
    statics: usize,
    foreign: u32,
}

/// Where the `jmp_buf` starts in the record nabic allocates (its size and
/// alignment there: `JMP_AT + 768`, 16).
#[cfg(not(any(target_family = "wasm", target_os = "none")))]
const JMP_AT: usize = 64;

#[cfg(not(any(windows, target_family = "wasm", target_os = "none")))]
unsafe extern "C" {
    /// No signal mask restore, unlike `longjmp`: none was saved.
    #[link_name = "_longjmp"]
    fn longjmp_raw(env: *mut u8, val: i32) -> !;
}

#[cfg(all(windows, not(target_arch = "aarch64")))]
unsafe extern "C" {
    fn longjmp(env: *mut u8, val: i32) -> !;
}

// ucrtbase's arm64 longjmp unwinds to the setjmp frame, through frames
// with no unwind info: compiled code calls `nabi_setjmp` there instead.
#[cfg(all(windows, target_arch = "aarch64"))]
core::arch::global_asm!(include_str!("catch/aarch64/windows.s"));

#[cfg(all(windows, target_arch = "aarch64"))]
unsafe extern "C" {
    #[link_name = "nabi_longjmp"]
    fn longjmp(env: *mut u8, val: i32) -> !;
}

/// Push `rec` (`JMP_AT + 768` bytes, 16-aligned) for an `#[on_panic]` C
/// entry; compiled code then calls `_setjmp(rec + JMP_AT)`.
///
/// # Safety
/// `rec` must stay valid until [`nabi_catch_pop`] or a recovery.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nabi_catch_push(rec: *mut u8) {
    let r = rec.cast::<Record>();
    // SAFETY: this thread's own slots; `rec` per the contract.
    unsafe {
        r.write(Record {
            prev: tls::catch().read().cast(),
            shadow: tls::shadow_stack_top().read(),
            statics: tls::static_inits().read(),
            foreign: tls::foreign().read(),
        });
        tls::catch().write(rec);
    }
}

/// The `#[on_panic]` entry returned normally: drop its record.
///
/// # Safety
/// `rec` must be the innermost record pushed on this thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nabi_catch_pop(rec: *mut u8) {
    // SAFETY: caller contract.
    unsafe { tls::catch().write((*rec.cast::<Record>()).prev.cast()) };
}

/// A C entry foreign code called runs (`entered`) or returns.
pub(crate) fn foreign(entered: bool) {
    // SAFETY: this thread's own slot.
    unsafe {
        let n = tls::foreign().read();
        tls::foreign().write(if entered { n + 1 } else { n.saturating_sub(1) });
    }
}

/// The panic hook `std::panic::set_hook` installed: `f(ud, msg, at)`.
type Hook = extern "C" fn(*mut u8, *const u8, usize, *const u8, usize);

static HOOK: AtomicPtr<()> = AtomicPtr::new(core::ptr::null_mut());
static HOOK_UD: AtomicPtr<u8> = AtomicPtr::new(core::ptr::null_mut());
/// Nonzero while a thread runs the hook: a panic in it skips it.
static IN_HOOK: AtomicU32 = AtomicU32::new(0);

/// Install `hook` with its context `ud` (null: none); returns the
/// previous context, for its owner to release.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_panic_set_hook(hook: Option<Hook>, ud: *mut u8) -> *mut u8 {
    let old = HOOK_UD.swap(ud, Ordering::AcqRel);
    HOOK.store(
        hook.map_or(core::ptr::null_mut(), |h| h as *mut ()),
        Ordering::Release,
    );
    old
}

/// Run the hook, if any, on the panic message `msg` (parts) and location.
fn run_hook(msg: &[&[u8]], at: Option<&[u8]>) {
    let h = HOOK.load(Ordering::Acquire);
    if h.is_null() || IN_HOOK.fetch_add(1, Ordering::AcqRel) != 0 {
        return;
    }
    let mut buf = [0u8; 1024];
    let mut n = 0;
    for p in msg {
        let k = p.len().min(buf.len() - n);
        buf[n..n + k].copy_from_slice(&p[..k]);
        n += k;
    }
    let at = at.unwrap_or(&[]);
    // SAFETY: only `nabi_panic_set_hook` stores, a function of this type.
    let hook = unsafe { core::mem::transmute::<*mut (), Hook>(h) };
    hook(
        HOOK_UD.load(Ordering::Acquire),
        buf.as_ptr(),
        n,
        at.as_ptr(),
        at.len(),
    );
    IN_HOOK.fetch_sub(1, Ordering::AcqRel);
}

/// After a panic's report: back to the innermost `#[on_panic]` entry when
/// it is the nearest foreign entry, else abort when foreign code is below,
/// else return (the caller exits 101).
pub(crate) fn unwind(msg: &[&[u8]], at: Option<&[u8]>) {
    run_hook(msg, at);
    // SAFETY: this thread's own slots.
    let (rec, depth) = unsafe { (tls::catch().read().cast::<Record>(), tls::foreign().read()) };
    // SAFETY: a record stays valid while it is pushed.
    if !rec.is_null() && unsafe { (*rec).foreign } == depth {
        recover(rec);
    }
    if depth > 0 {
        nabir_common::write_all(
            2,
            &[
                b"panic in a function C called: aborting (`#[on_panic(return = v)]` on the \
                `extern \"C\" fn` returns `v` to C instead)\n",
            ],
        );
        crate::ending(134);
        // SAFETY: plain libc call.
        unsafe { libc_abort() }
    }
}

#[cfg(not(any(target_family = "wasm", target_os = "none")))]
fn recover(rec: *mut Record) {
    // SAFETY: `rec` is pushed, so live; this thread's own slots. The
    // entry's frame is still on the stack, below the panicking code.
    unsafe {
        let r = &*rec;
        tls::catch().write(r.prev.cast());
        tls::shadow_stack_top().write(r.shadow);
        tls::static_inits().write(r.statics);
        crate::PANICKING.store(0, Ordering::Relaxed);
        let env = rec.cast::<u8>().add(JMP_AT);
        #[cfg(windows)]
        longjmp(env, 1);
        #[cfg(not(windows))]
        longjmp_raw(env, 1);
    }
}

/// No `setjmp` here: nabic pushes no records, so this never runs.
#[cfg(any(target_family = "wasm", target_os = "none"))]
const fn recover(_rec: *mut Record) {}

#[cfg(not(target_os = "none"))]
unsafe fn libc_abort() -> ! {
    // SAFETY: plain libc call.
    unsafe { libc::abort() }
}

#[cfg(target_os = "none")]
unsafe fn libc_abort() -> ! {
    // SAFETY: plain call.
    unsafe { nabir_common::sys::libc::abort() }
}
