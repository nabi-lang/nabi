//! Objective-C support on Apple targets: `Id` handles released by a GC
//! cleanup (on the main thread for UI objects), and the handler that
//! makes an uncaught Objective-C exception fatal with its name, reason
//! and stack. libobjc is only looked up (a program that has an `Id`
//! has loaded it), so programs without Objective-C do not link it.

use crate::cleanup;
use core::ffi::{c_char, c_int, c_void};
use core::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

unsafe extern "C" {
    static _dispatch_main_q: u8;
    fn dispatch_async_f(q: *const u8, ctx: *mut c_void, work: unsafe extern "C" fn(*mut c_void));
    fn pthread_main_np() -> c_int;
}

/// The thread is the process's main thread.
pub fn on_main() -> bool {
    // SAFETY: plain libSystem call.
    unsafe { pthread_main_np() != 0 }
}

/// `f(arg)` later on the main queue.
///
/// # Safety
/// `f(arg)` must be sound on the main thread.
pub unsafe fn on_main_queue(f: unsafe extern "C" fn(usize), arg: usize) {
    // SAFETY: the same C ABI; `arg` is a word either way.
    unsafe {
        let work: unsafe extern "C" fn(*mut c_void) = core::mem::transmute(f);
        dispatch_async_f(&raw const _dispatch_main_q, arg as *mut c_void, work);
    }
}

static INSTALLED: AtomicBool = AtomicBool::new(false);

/// Release the object `p` (`release`, libobjc's `objc_release`) when
/// `cell`, its `Id`'s handle, is collected; on the main thread if
/// `main`. The cleanup's token, for `release_now()`.
///
/// # Safety
/// `cell` must be a live GC object, `release(p)` sound once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nabi_objc_track(
    cell: *mut u8,
    release: unsafe extern "C" fn(usize),
    p: usize,
    main: bool,
) -> usize {
    if !INSTALLED.swap(true, Ordering::Relaxed) {
        install();
    }
    let flags = if main { cleanup::MAIN } else { 0 };
    // SAFETY: caller contract.
    unsafe { cleanup::nabi_gc_add_cleanup(cell, release, p, flags) }
}

/// Destroy the weak slot at `slot` (`objc_destroyWeak`) and free it when
/// `cell`, its `WeakId`'s handle, is collected.
///
/// # Safety
/// `cell` must be a live GC object, `slot` a weak slot `objc_initWeak`
/// set up in memory from `malloc`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nabi_objc_weak_track(cell: *mut u8, slot: usize) -> usize {
    // SAFETY: caller contract.
    unsafe { cleanup::nabi_gc_add_cleanup(cell, destroy_weak, slot, 0) }
}

unsafe extern "C" fn destroy_weak(slot: usize) {
    let f = lookup(b"objc_destroyWeak\0");
    if !f.is_null() {
        // SAFETY: libobjc's `void objc_destroyWeak(id *)`.
        unsafe {
            let f: unsafe extern "C" fn(usize) = core::mem::transmute(f);
            f(slot);
        }
    }
    // SAFETY: the slot came from `malloc` (`bare::alloc`).
    unsafe { libc::free(slot as *mut c_void) };
}

type Send0 = unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void;
type SendIndex = unsafe extern "C" fn(*mut c_void, *mut c_void, usize) -> *mut c_void;
type SendCount = unsafe extern "C" fn(*mut c_void, *mut c_void) -> usize;
type Register = unsafe extern "C" fn(*const c_char) -> *mut c_void;

/// libobjc's `objc_msgSend` and `sel_registerName`, looked up once for
/// the handler.
static SEND: AtomicPtr<c_void> = AtomicPtr::new(core::ptr::null_mut());
static SEL: AtomicPtr<c_void> = AtomicPtr::new(core::ptr::null_mut());

fn lookup(name: &[u8]) -> *mut c_void {
    // SAFETY: a NUL-terminated name.
    unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr().cast()) }
}

/// `objc_setUncaughtExceptionHandler(uncaught)`.
fn install() {
    let set = lookup(b"objc_setUncaughtExceptionHandler\0");
    let send = lookup(b"objc_msgSend\0");
    let sel = lookup(b"sel_registerName\0");
    if set.is_null() || send.is_null() || sel.is_null() {
        return;
    }
    SEND.store(send, Ordering::Relaxed);
    SEL.store(sel, Ordering::Relaxed);
    // SAFETY: libobjc's function, by its C type.
    unsafe {
        let set: unsafe extern "C" fn(unsafe extern "C" fn(*mut c_void)) =
            core::mem::transmute(set);
        set(uncaught);
    }
}

fn err(b: &[u8]) {
    // SAFETY: plain write to stderr.
    unsafe { libc::write(2, b.as_ptr().cast(), b.len()) };
}

/// The selector named `name`.
unsafe fn sel(name: &core::ffi::CStr) -> *mut c_void {
    // SAFETY: set by `install` before the handler can run.
    unsafe {
        let f: Register = core::mem::transmute(SEL.load(Ordering::Relaxed));
        f(name.as_ptr())
    }
}

/// `[o name]` for an object result.
unsafe fn send(o: *mut c_void, name: &core::ffi::CStr) -> *mut c_void {
    // SAFETY: caller contract; set by `install` before the handler.
    unsafe {
        let f: Send0 = core::mem::transmute(SEND.load(Ordering::Relaxed));
        f(o, sel(name))
    }
}

/// An `NSString`'s UTF-8 text to stderr.
unsafe fn put(s: *mut c_void) {
    if s.is_null() {
        return;
    }
    // SAFETY: `s` is an `NSString`.
    let c = unsafe { send(s, c"UTF8String") }.cast::<c_char>();
    if !c.is_null() {
        // SAFETY: a C string.
        err(unsafe { core::ffi::CStr::from_ptr(c) }.to_bytes());
    }
}

/// The uncaught exception handler: name, reason and stack, then abort.
unsafe extern "C" fn uncaught(e: *mut c_void) {
    err(b"Objective-C exception ");
    // SAFETY: `e` is an `NSException`; the sends are its accessors.
    unsafe {
        put(send(e, c"name"));
        err(b": ");
        put(send(e, c"reason"));
        err(b"\nits stack:\n");
        let stack = send(e, c"callStackSymbols");
        if !stack.is_null() {
            let raw = SEND.load(Ordering::Relaxed);
            let count: SendCount = core::mem::transmute(raw);
            let at: SendIndex = core::mem::transmute(raw);
            let n = count(stack, sel(c"count"));
            for i in 0..n {
                err(b"  ");
                put(at(stack, sel(c"objectAtIndex:"), i));
                err(b"\n");
            }
        }
    }
    err(
        b"an Objective-C exception is a programmer error and Nabi does not catch it, as Swift \
          does not: fix the call the reason names\n",
    );
    // SAFETY: plain libc call.
    unsafe { libc::abort() };
}
