//! GC cleanups: `f(arg)` once an object is collected. Foreign resources
//! a shared GC object holds (an Objective-C object's retain) go back
//! this way when the last Nabi reference to it is gone.
//!
//! Each cleanup is a malloc'd entry holding a pinned weak cell to its
//! object: the pin keeps the cell, not the object. After a collection
//! the next allocation refill (or `nabi_gc_collect`) takes the entries
//! whose cells read null and runs them, on that thread, outside every
//! lock. Order is not kept; a program that exits runs none.

use core::ptr;
use core::sync::atomic::{AtomicBool, Ordering};
use nabir_common::AtomicU64;
use nabir_common::sys;
#[cfg(target_os = "none")]
use nabir_common::sys::libc;

/// On the main thread (`dispatch_async_f` to the main queue from any
/// other): Objective-C objects whose `dealloc` must run there.
#[cfg(target_vendor = "apple")]
pub const MAIN: usize = 1;

struct Entry {
    next: *mut Entry,
    /// The pinned weak cell; its word is the object until it dies.
    cell: *mut u8,
    /// `None` once cancelled.
    f: Option<unsafe extern "C" fn(usize)>,
    arg: usize,
    flags: usize,
}

struct List {
    lock: sys::Lock,
    head: core::cell::UnsafeCell<*mut Entry>,
}

// SAFETY: `head` and the entries are only touched under `lock`.
unsafe impl Sync for List {}

static LIST: List = List {
    lock: sys::Lock::new(),
    head: core::cell::UnsafeCell::new(ptr::null_mut()),
};

/// Some cleanup is registered: allocation refills look.
pub static ANY: AtomicBool = AtomicBool::new(false);
/// The collection count the list was last checked at.
static SEEN: AtomicU64 = AtomicU64::new(0);

/// Run `f(arg)` once `obj` is collected; the entry's address, for
/// `nabi_gc_cancel_cleanup`. May collect: the caller roots `obj`.
///
/// # Safety
/// `obj` must be a live GC object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nabi_gc_add_cleanup(
    obj: *mut u8,
    f: unsafe extern "C" fn(usize),
    arg: usize,
    flags: usize,
) -> usize {
    let cell = nabir_gc::weak_new(obj);
    // SAFETY: a fresh cell, rooted from here on by the pin.
    unsafe { nabir_gc::pin(cell) };
    // SAFETY: plain allocation, written before use.
    let e = unsafe { libc::malloc(size_of::<Entry>()) }.cast::<Entry>();
    if e.is_null() {
        nabir_common::oom();
    }
    LIST.lock.lock();
    // SAFETY: under the lock.
    unsafe {
        e.write(Entry {
            next: *LIST.head.get(),
            cell,
            f: Some(f),
            arg,
            flags,
        });
        *LIST.head.get() = e;
    }
    LIST.lock.unlock();
    ANY.store(true, Ordering::Relaxed);
    e as usize
}

/// Drop the cleanup `token` (`nabi_gc_add_cleanup`'s) before it runs:
/// its resource went back another way (`release_now()`).
///
/// # Safety
/// `token` must come from `nabi_gc_add_cleanup` on an object still
/// alive (so the entry is not yet freed).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nabi_gc_cancel_cleanup(token: usize) {
    LIST.lock.lock();
    // SAFETY: caller contract; under the lock.
    unsafe { (*(token as *mut Entry)).f = None };
    LIST.lock.unlock();
}

/// Run the cleanups of objects collected since the last look.
pub fn run() {
    let n = nabir_gc::collections();
    if SEEN.swap(n, Ordering::Relaxed) == n {
        return;
    }
    let mut dead: *mut Entry = ptr::null_mut();
    LIST.lock.lock();
    // SAFETY: under the lock; every entry is ours.
    unsafe {
        let mut at: *mut *mut Entry = LIST.head.get();
        while !(*at).is_null() {
            let e = *at;
            if nabir_gc::weak_cleared((*e).cell) {
                *at = (*e).next;
                (*e).next = dead;
                dead = e;
            } else {
                at = &raw mut (*e).next;
            }
        }
        if (*LIST.head.get()).is_null() {
            ANY.store(false, Ordering::Relaxed);
        }
    }
    LIST.lock.unlock();
    while !dead.is_null() {
        let p = dead;
        // SAFETY: taken off the list, ours alone.
        unsafe {
            let e = p.read();
            dead = e.next;
            nabir_gc::unpin(e.cell);
            libc::free(p.cast());
            if let Some(f) = e.f {
                call(f, e.arg, e.flags);
            }
        }
    }
}

/// `f(arg)` here, or on the main thread when `flags` say so.
unsafe fn call(f: unsafe extern "C" fn(usize), arg: usize, flags: usize) {
    #[cfg(target_vendor = "apple")]
    if flags & MAIN != 0 && !crate::objc::on_main() {
        // SAFETY: as the caller's.
        unsafe { crate::objc::on_main_queue(f, arg) };
        return;
    }
    let _ = flags;
    // SAFETY: the registrant's contract.
    unsafe { f(arg) };
}
