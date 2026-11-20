//! Long runtime calls run parked, so a collection on another thread
//! does not wait for them (the `int` kernels, long `str` searches and
//! counts, large byte copies).

use core::sync::atomic::{AtomicUsize, Ordering};

/// `f()`, parked when `long`: like a blocking runtime call, so a
/// collection on another thread does not wait for it. `f` touches no
/// GC object but the ones it was passed and stores no reference: the
/// caller's frame keeps them alive (these calls are safepoints) and a
/// collection never moves what a root reaches.
pub(crate) fn parked<R>(long: bool, f: impl FnOnce() -> R) -> R {
    if !long {
        return f();
    }
    nabir_gc::world::enter_native();
    MARKS.fetch_add(1, Ordering::SeqCst);
    let r = f();
    // Before any safepoint: a collection that waited for the call sees
    // it ended.
    MARKS.fetch_add(1, Ordering::SeqCst);
    nabir_gc::world::leave_native();
    r
}

/// Parked calls started plus those ended ([`nabi_parked_marks`]).
static MARKS: AtomicUsize = AtomicUsize::new(0);

/// How many times a parked call started or ended: odd while one runs
/// (tests: a collection that ends with the count unchanged did not wait
/// for that call).
#[unsafe(no_mangle)]
pub extern "C" fn nabi_parked_marks() -> usize {
    MARKS.load(Ordering::SeqCst)
}
