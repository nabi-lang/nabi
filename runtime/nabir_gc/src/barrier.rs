//! The remembered set and the write barrier.

#[allow(clippy::wildcard_imports)]
use crate::*;

/// The remembered set: old objects that had a reference
/// stored into them since the last collection, each logged once (its
/// `UNLOGGED` bit cleared by the barrier). Each thread appends to its
/// own chunk through `nabi_tls_wb_log_cursor` / `_limit` without
/// a lock; a full chunk (and every thread's chunk at a collection or
/// thread exit) is retired onto the lock-free [`LOG_FULL`] stack.
#[repr(C)]
pub(crate) struct LogChunk {
    pub(crate) next: *mut LogChunk,
    /// End of the logged objects (`*mut u8` each, from `self + 1`).
    pub(crate) end: usize,
}

/// Words per log chunk (header included).
pub(crate) const LOG_CHUNK_WORDS: usize = 512;

/// Retired chunks: pushed by any thread, taken with the world stopped.
pub(crate) static LOG_FULL: AtomicPtr<LogChunk> = AtomicPtr::new(ptr::null_mut());

/// Retire the chunk behind the log slots `[cursor, limit]` (none if
/// null) onto `stack` and clear them.
pub(crate) fn retire_to([cursor, limit]: [*mut usize; 2], stack: &AtomicPtr<LogChunk>) {
    // SAFETY: the slots are the owner's (or a stopped / exiting
    // thread's); a non-null limit ends a chunk from `log_word`.
    unsafe {
        let end = limit.read();
        if end == 0 {
            return;
        }
        let c = (end - LOG_CHUNK_WORDS * size_of::<usize>()) as *mut LogChunk;
        (*c).end = cursor.read();
        push_chunks(stack, c, c);
        cursor.write(0);
        limit.write(0);
    }
}

/// Push the chunk list `first..=last` onto `stack`.
pub(crate) fn push_chunks(stack: &AtomicPtr<LogChunk>, first: *mut LogChunk, last: *mut LogChunk) {
    let mut head = stack.load(Ordering::Relaxed);
    loop {
        // SAFETY: `last` is a chunk nobody else holds.
        unsafe { (*last).next = head };
        match stack.compare_exchange_weak(head, first, Ordering::Release, Ordering::Relaxed) {
            Ok(_) => return,
            Err(h) => head = h,
        }
    }
}

/// Retire a thread's wb log chunk (the remembered set).
pub(crate) fn retire_log(slots: [*mut usize; 2]) {
    retire_to(slots, &LOG_FULL);
}

/// Append `word` to the calling thread's log behind `slots`, retiring a
/// full chunk onto `stack`.
pub(crate) fn log_word(slots: [*mut usize; 2], stack: &AtomicPtr<LogChunk>, word: usize) {
    // SAFETY: this thread's own slots: `cursor < limit` inside its chunk
    // (or both 0).
    unsafe {
        if slots[0].read() == slots[1].read() {
            retire_to(slots, stack);
            let c = libc::malloc(LOG_CHUNK_WORDS * size_of::<usize>()).cast::<LogChunk>();
            if c.is_null() {
                nabir_common::oom();
            }
            slots[0].write(c.add(1) as usize);
            slots[1].write(c as usize + LOG_CHUNK_WORDS * size_of::<usize>());
        }
        let c = slots[0].read();
        (c as *mut usize).write(word);
        slots[0].write(c + size_of::<usize>());
    }
}

/// `f` on every word logged in the chunk list `log`.
pub(crate) fn for_each_logged(mut log: *mut LogChunk, mut f: impl FnMut(*mut u8)) {
    // SAFETY: retired chunks hold words up to their `end`.
    unsafe {
        while !log.is_null() {
            let mut e = log.add(1).cast::<*mut u8>();
            while (e as usize) < (*log).end {
                f(*e);
                e = e.add(1);
            }
            log = (*log).next;
        }
    }
}

pub(crate) fn free_logs(mut log: *mut LogChunk) {
    while !log.is_null() {
        // SAFETY: chunks come from `log_word`'s malloc.
        unsafe {
            let next = (*log).next;
            libc::free(log.cast());
            log = next;
        }
    }
}

/// Before the calling thread leaves the registry: retire its logs so
/// the next collection (or the running mark) still sees them.
pub fn retire_own_logs() {
    retire_log(nabir_common::tls::wb_log());
    concurrent::retire_satb(nabir_common::tls::satb_log());
}

/// Write barrier for runtime code: call before storing a GC reference
/// into the word at `slot` of the GC object `obj`.
///
/// What compiled code inlines: the slow path for `UNLOGGED`, then the
/// card for `CARDED` (a concurrent mark sets both on old LOS objects).
///
/// # Safety
/// `obj` must be a live GC object and `slot` a word inside it.
pub unsafe fn write_barrier(obj: *mut u8, slot: *const u8) {
    // SAFETY: caller contract.
    let flags = unsafe { (*header(obj)).gc_flags.load(Ordering::Relaxed) };
    if flags & FLAG_UNLOGGED != 0 {
        // SAFETY: caller contract.
        unsafe { write_barrier_slow(obj, slot, size_of::<usize>()) };
    }
    if flags & FLAG_CARDED != 0 {
        // SAFETY: a carded object is a LOS object with a card table.
        unsafe {
            let cards = (*prefix(obj)).cards;
            *cards.add((slot as usize - obj as usize) / CARD_SIZE) = 1;
        }
    }
}

/// `nabi_gc_wb_slow`: remember `obj` for the next minor collection.
///
/// `obj` (old, `UNLOGGED`) is about to hold a reference that may be
/// young, stored as `len` bytes at `slot`. Not a safepoint: no
/// collection runs between the barrier and the store that follows it.
/// While a concurrent mark runs, every old object is `UNLOGGED` and this
/// is also its snapshot barrier ([`concurrent::snapshot`], by card for a
/// large object: [`concurrent::snapshot_cards`]).
///
/// # Safety
/// `obj` must be a live GC object and `slot..slot + len` inside it.
pub unsafe fn write_barrier_slow(obj: *mut u8, slot: *const u8, len: usize) {
    // SAFETY: caller contract.
    let flags = unsafe { &(*header(obj)).gc_flags };
    if concurrent::MARKING.load(Ordering::Relaxed) {
        // SAFETY: caller contract.
        unsafe {
            if flags.load(Ordering::Relaxed) & FLAG_LARGE != 0 {
                concurrent::snapshot_cards(obj, slot, len);
            } else {
                concurrent::snapshot(obj);
            }
        }
        return;
    }
    // Racing stores: exactly one thread logs it.
    if flags.fetch_and(!FLAG_UNLOGGED, Ordering::Relaxed) & FLAG_UNLOGGED == 0 {
        return;
    }
    log_word(nabir_common::tls::wb_log(), &LOG_FULL, obj as usize);
}
