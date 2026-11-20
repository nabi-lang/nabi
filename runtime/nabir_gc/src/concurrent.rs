//! Concurrent marking of full collections: snapshot at the
//! beginning (SATB, Yuasa), one marker thread.
//!
//! - Start ([`start`], a pause): a minor collection first, so every
//!   survivor is old (`UNLOGGED`, LOS also `CARDED`). Roots and pins are
//!   queued and the marker wakes.
//! - Marking: mutators allocate black (`BLACK` lines, LOS objects in
//!   front of `los_mark_head`), which the mark never reads or sweeps. The
//!   first store into an old object takes `nabi_gc_wb_slow`, which logs
//!   every reference it holds into the thread's SATB chunk ([`snapshot`])
//!   before clearing `UNLOGGED`, so nothing reachable at the start is
//!   lost; it also remembers the object (or dirties its card) for the
//!   next minor collection. A `SCANNED` object logs nothing.
//! - Finish ([`finish`], a pause): when the marker runs dry or the
//!   allocation budget does, drain every SATB chunk, requeue the roots,
//!   complete the mark and sweep the snapshot (`BLACK` lines become
//!   young `TAKEN`). No evacuation: a fragmented heap waits for the next
//!   stop-the-world full collection.
//!
//! The mark state ([`crate::Mark`]) belongs to whoever holds the mark
//! lock: the marker while marking, a pause otherwise (via [`FINISH`]).
//! The marker never holds it while waiting for the world lock.

use crate::{
    BLOCK_SIZE, FLAG_LARGE, FLAG_SATB, FLAG_SCANNED, FLAG_UNLOGGED, Kind, LOG_FULL, LogChunk,
    MARKER_CPU_NS, MIN_HEAP, Pause, Phase, SWEEPING, WEAK_REF, begin_lazy, build_index,
    clear_dead_weak, drain, for_each_logged, for_each_ref, free_listed, free_logs, header, heap,
    lap, load_word, log_word, mark, mark_roots, prefix, push, push_chunks, retire_log, retire_to,
    sweep_los, sweep_some, world,
};
use core::ptr::null_mut;
use core::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use nabir_common::AtomicU64;
use nabir_common::header::{FLAG_CARDED, FLAG_MARK};
use nabir_common::sys;

/// A concurrent mark runs: the barrier slow path snapshots.
pub static MARKING: AtomicBool = AtomicBool::new(false);
/// Retired SATB chunks (reference words to mark).
static SATB_FULL: AtomicPtr<LogChunk> = AtomicPtr::new(null_mut());
/// A pause waits for the mark lock: the marker lets go.
static FINISH: AtomicBool = AtomicBool::new(false);
/// The marker has a mark to work on (under the mark lock).
static ACTIVE: AtomicBool = AtomicBool::new(false);
/// The running mark (`Heap::cycles`), so the marker finishes only its own.
static CYCLE: AtomicU64 = AtomicU64::new(0);
static SPAWNED: AtomicBool = AtomicBool::new(false);
/// Tests: the marker keeps the mark open (never finishes it) while set,
/// so a test may store mid-mark and finish it itself.
#[cfg(test)]
pub static HOLD: AtomicBool = AtomicBool::new(false);

/// The mark lock (guards [`crate::Mark`]) and its condvar (the marker
/// waits on it for a mark).
static MARK_LOCK: sys::Lock = sys::Lock::new();
static MARK_COND: sys::Cond = sys::Cond::new();

fn raw_lock() {
    MARK_LOCK.lock();
}

/// Release the mark lock.
pub fn unlock_mark() {
    MARK_LOCK.unlock();
}

/// Take the mark lock from the marker (a pause).
pub fn lock_mark() {
    FINISH.store(true, Ordering::Relaxed);
    raw_lock();
    FINISH.store(false, Ordering::Relaxed);
}

fn wait() {
    MARK_COND.wait(&MARK_LOCK);
}

fn broadcast() {
    MARK_COND.broadcast();
}

/// Retire a thread's SATB chunk.
pub fn retire_satb(slots: [*mut usize; 2]) {
    retire_to(slots, &SATB_FULL);
}

/// Queue every word of the retired SATB chunks; whether there was any.
fn take_satb() -> bool {
    let log = SATB_FULL.swap(null_mut(), Ordering::Acquire);
    for_each_logged(log, |w| push(w as usize, false));
    free_logs(log);
    !log.is_null()
}

/// In the pause after a minor collection (world stopped, mark lock
/// held): start a concurrent mark of everything that survived it.
pub fn start() {
    let h = heap();
    h.marking = true;
    h.los_mark_head = h.los;
    h.mark_start_live = h.live_bytes;
    h.assisted = 0;
    h.cycles += 1;
    CYCLE.store(h.cycles, Ordering::Relaxed);
    // Old LOS objects: the first store into each card snapshots that
    // card ([`snapshot_cards`]; then marks the card as always).
    // SAFETY: walks the LOS list.
    unsafe {
        let mut o = h.los;
        while !o.is_null() {
            let flags = &(*header(o)).gc_flags;
            if flags.load(Ordering::Relaxed) & FLAG_CARDED != 0 {
                let (bits, words) = crate::satb_bits(o);
                core::ptr::write_bytes(bits, 0, words);
                flags.fetch_or(FLAG_UNLOGGED, Ordering::Relaxed);
            }
            o = (*prefix(o)).next;
        }
    }
    build_index();
    let m = mark();
    (m.full, m.concurrent, m.evacuating, m.len) = (true, true, false, 0);
    (m.marked, m.marked_bytes) = (0, 0);
    mark_roots(false);
    MARKING.store(true, Ordering::Relaxed);
    ACTIVE.store(true, Ordering::Relaxed);
    if !SPAWNED.swap(true, Ordering::Relaxed) {
        spawn_marker();
    }
    broadcast();
}

fn spawn_marker() {
    // The thread runs for the rest of the process.
    if !sys::spawn_detached(marker) {
        nabir_common::fatal(b"nabi runtime: cannot start the GC marker thread");
    }
}

/// The marker thread: drains the mark stack and the retired SATB chunks
/// while mutators run, then stops the world to finish.
extern "C" fn marker() {
    raw_lock();
    loop {
        while !ACTIVE.load(Ordering::Relaxed) {
            // Between marks: the lazy sweep, a step at a time, letting a
            // pause in.
            if SWEEPING.load(Ordering::Acquire) && !FINISH.load(Ordering::Relaxed) {
                sweep_some();
                continue;
            }
            wait();
        }
        let cycle = CYCLE.load(Ordering::Relaxed);
        let c0 = sys::thread_cpu_ns() as u64;
        let done = loop {
            drain(256);
            if FINISH.load(Ordering::Relaxed) {
                break false;
            }
            if mark().len == 0 && !take_satb() {
                #[cfg(test)]
                if HOLD.load(Ordering::Relaxed) {
                    unlock_mark();
                    std::thread::yield_now();
                    raw_lock();
                    continue;
                }
                break true;
            }
        };
        MARKER_CPU_NS.fetch_add(
            (sys::thread_cpu_ns() as u64).saturating_sub(c0),
            Ordering::Relaxed,
        );
        if done {
            unlock_mark();
            world::lock();
            let h = heap();
            let held = h.no_collect > 0;
            if h.marking && h.cycles == cycle && !held {
                finish(false);
            }
            world::unlock();
            raw_lock();
            // A no-collect region is open: finish once the last closes
            // (`wake_marker`; checked again under the lock, as the region
            // may have closed meanwhile).
            if held && crate::NO_COLLECT.load(Ordering::Acquire) > 0 {
                wait();
            }
        } else {
            // A pause takes the lock; wait it out.
            wait();
        }
    }
}

/// Objects one mark assist marks.
const ASSIST_OBJECTS: usize = 4096;

/// Wake the marker (with the world lock held, so no pause holds the
/// mark lock meanwhile).
pub fn wake_marker() {
    lock_mark();
    unlock_mark();
    broadcast();
}

/// With the world lock held, during a concurrent mark whose budget ran
/// out: mark some of what is left in the allocating thread instead of
/// stopping the world for all of it, then let it allocate a little
/// more. The more threads outrun the marker, the more they mark
/// themselves. Once the mark has nothing left, finish it here (a short
/// pause). False: the heap grew past [`crate::assist_cap`]; finish it
/// stop-the-world now.
pub fn assist() -> bool {
    let h = heap();
    if h.live_bytes + crate::young_bytes() >= crate::assist_cap(h) {
        return false;
    }
    lock_mark();
    drain(ASSIST_OBJECTS);
    let left = mark().len > 0 || take_satb() || mark().len > 0;
    unlock_mark();
    broadcast();
    #[cfg(test)]
    let left = left || HOLD.load(Ordering::Relaxed);
    if !left {
        finish(false);
        return true;
    }
    // Each assist earns its share of the young budget: what is left to
    // mark (as the last mark's count says) over the objects it marked,
    // so the mark ends about when the budget does.
    let h = heap();
    let left = h
        .last_marked
        .saturating_sub(mark().marked)
        .max(ASSIST_OBJECTS);
    let young = h.next_gc.saturating_sub(h.old_bytes);
    let earned = (young / left).saturating_mul(ASSIST_OBJECTS);
    h.assisted += earned.clamp(64 << 10, 16 << 20);
    h.assists += 1;
    crate::CLAIMS.room.store(crate::room(h), Ordering::Relaxed);
    true
}

/// With the world lock held, during a concurrent mark: stop the world
/// and finish it. `forced`: the allocation budget ran out first.
pub fn finish(forced: bool) {
    let p = Pause::begin();
    world::for_each_satb(retire_satb);
    // Roots stored since the start need no marking (they were reachable
    // then or are new); queueing them again is cheap and robust.
    mark_roots(false);
    loop {
        drain(usize::MAX);
        if !take_satb() {
            break;
        }
    }
    clear_dead_weak(true);
    // The objects stored into during the mark stay remembered (they may
    // reference new, young objects): the survivors among them.
    world::for_each_wb_log(retire_log);
    let log = LOG_FULL.swap(null_mut(), Ordering::Acquire);
    let last = keep_marked(log);
    lap(Phase::Mark);
    MARKING.store(false, Ordering::Relaxed);
    ACTIVE.store(false, Ordering::Relaxed);
    mark().concurrent = false;
    let h = heap();
    h.marking = false;
    sweep_los(true, true);
    for_each_logged(log, |o| {
        // SAFETY: a survivor (its lazy sweep sets `UNLOGGED` again,
        // which at worst logs it twice).
        unsafe {
            (*header(o))
                .gc_flags
                .fetch_and(!FLAG_UNLOGGED, Ordering::Relaxed)
        };
    });
    if !log.is_null() {
        push_chunks(&LOG_FULL, log, last);
    }
    // The marked bytes are the survivors: the blocks are swept later,
    // while mutators run.
    let survived = mark().marked_bytes;
    let h = heap();
    let allocated = h.live_bytes - h.mark_start_live;
    h.old_bytes = survived;
    h.live_bytes = survived + allocated;
    h.next_gc = survived.saturating_add(crate::young_room(h, survived));
    h.last_marked = mark().marked;
    h.full_at = survived.saturating_mul(2).max(MIN_HEAP);
    // The `LIVE` lines are counted as the lazy sweep goes (`adopt_sweep`);
    // until then the marked bytes stand in for them.
    h.old_lines = survived / crate::LINE_SIZE;
    h.full_lines_at = usize::MAX;
    let keep = h
        .next_gc
        .saturating_sub(h.live_bytes)
        .div_ceil(BLOCK_SIZE)
        .saturating_sub(free_listed().len());
    begin_lazy(h, keep);
    lap(Phase::Sweep);
    h.collections += 1;
    h.forced += u64::from(forced);
    p.end(Kind::Finish);
    // The marker sweeps: wake it if a mutator finished the mark.
    broadcast();
}

/// Drop the unmarked (dead) objects from the chunk list `log`, in place;
/// its last chunk (null if empty).
fn keep_marked(mut log: *mut LogChunk) -> *mut LogChunk {
    let mut last = null_mut();
    // SAFETY: retired chunks hold objects up to their `end`.
    unsafe {
        while !log.is_null() {
            let first = log.add(1).cast::<*mut u8>();
            let (mut r, mut w) = (first, first);
            while (r as usize) < (*log).end {
                if (*header(*r)).gc_flags.load(Ordering::Relaxed) & FLAG_MARK != 0 {
                    *w = *r;
                    w = w.add(1);
                }
                r = r.add(1);
            }
            (*log).end = w as usize;
            last = log;
            log = (*log).next;
        }
    }
    last
}

/// A weak reference read during a concurrent mark: its referent `obj`
/// may be unmarked and about to be stored where the mark has looked
/// already, so it is marked like an overwritten value.
pub fn shade(obj: usize) {
    log_word(nabir_common::tls::satb_log(), &SATB_FULL, obj);
}

/// The barrier slow path of a large (LOS) object while a concurrent mark
/// runs: the first store into each card of `len` bytes at `slot` logs
/// the references that card holds, so a store waits for at most one
/// card (64 words) to be logged, not the whole object. `obj` stays
/// `UNLOGGED` for the rest of the mark.
///
/// # Safety
/// `obj` must be a live LOS object about to be stored into at
/// `slot..slot + len`.
pub unsafe fn snapshot_cards(obj: *mut u8, slot: *const u8, len: usize) {
    // SAFETY: caller contract.
    let f = unsafe { (*header(obj)).gc_flags.load(Ordering::Acquire) };
    if f & (FLAG_UNLOGGED | FLAG_SCANNED) != FLAG_UNLOGGED {
        return;
    }
    let (bits, _) = crate::satb_bits(obj);
    let at = slot as usize - obj as usize;
    let (c0, c1) = (
        at / crate::CARD_SIZE,
        (at + len.max(1) - 1) / crate::CARD_SIZE,
    );
    for c in c0..=c1 {
        // SAFETY: card `c` of the object; its bits word is ours to update
        // atomically.
        let w = unsafe { nabir_common::AtomicU64::from_ptr(bits.add(c / 32)) };
        let (claim, done) = (1u64 << (c % 32 * 2), 2u64 << (c % 32 * 2));
        if w.load(Ordering::Acquire) & done != 0 {
            continue;
        }
        if w.fetch_or(claim, Ordering::AcqRel) & claim != 0 {
            let mut spins = 0u32;
            while w.load(Ordering::Acquire) & done == 0 {
                if spins < 64 {
                    spins += 1;
                    core::hint::spin_loop();
                } else {
                    sys::yield_now();
                }
            }
            continue;
        }
        let slots = nabir_common::tls::satb_log();
        let (lo, hi) = (c * crate::CARD_SIZE, (c + 1) * crate::CARD_SIZE);
        for_each_ref(obj, lo, hi, |off, kind| {
            if kind == WEAK_REF {
                return;
            }
            // SAFETY: a reference word inside `obj`.
            let v = load_word(unsafe { obj.add(off) });
            if v != 0 {
                log_word(slots, &SATB_FULL, v);
            }
        });
        w.fetch_or(done, Ordering::Release);
    }
}

/// The barrier slow path while a concurrent mark runs: the first store
/// into the old object `obj` logs every reference it holds (unless the
/// marker scanned it already) and remembers `obj`; a racing store waits
/// until that is done, so no value is overwritten before it is logged.
///
/// # Safety
/// `obj` must be a live GC object about to be stored into.
pub unsafe fn snapshot(obj: *mut u8) {
    // SAFETY: caller contract.
    let flags = unsafe { &(*header(obj)).gc_flags };
    let f = flags.fetch_or(FLAG_SATB, Ordering::Acquire);
    if f & FLAG_SATB != 0 {
        // The logger may take a while (a large object) or be descheduled:
        // spin a little, then let it run.
        let mut spins = 0u32;
        while flags.load(Ordering::Acquire) & FLAG_UNLOGGED != 0 {
            if spins < 64 {
                spins += 1;
                core::hint::spin_loop();
            } else {
                sys::yield_now();
            }
        }
        return;
    }
    if f & FLAG_UNLOGGED == 0 {
        return;
    }
    if f & FLAG_SCANNED == 0 {
        let slots = nabir_common::tls::satb_log();
        // A weak word keeps nothing alive: its old value need not.
        for_each_ref(obj, 0, usize::MAX, |off, kind| {
            if kind == WEAK_REF {
                return;
            }
            // SAFETY: a reference word inside `obj`.
            let w = load_word(unsafe { obj.add(off) });
            if w != 0 {
                log_word(slots, &SATB_FULL, w);
            }
        });
    }
    if f & FLAG_LARGE == 0 {
        log_word(nabir_common::tls::wb_log(), &LOG_FULL, obj as usize);
    }
    flags.fetch_and(!FLAG_UNLOGGED, Ordering::Release);
}
