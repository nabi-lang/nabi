//! Collections and pauses: roots, minor and full cycles.

#[allow(clippy::wildcard_imports)]
use crate::*;

/// Full stop-the-world collection (`nabi_gc_collect`); a running
/// concurrent mark is finished first.
pub fn collect() {
    world::lock();
    configure(heap());
    if heap().marking {
        concurrent::finish(false);
    }
    collect_locked(true, false);
    world::unlock();
}

/// A minor collection (`nabi_gc_collect_young`); none while a
/// concurrent mark runs (it would finish the mark in a pause).
pub fn collect_young() {
    world::lock();
    configure(heap());
    if !heap().marking {
        collect_locked(false, false);
    }
    world::unlock();
}

/// With the world lock held: stop every other mutator, collect, resume.
/// While a concurrent mark runs, finish it instead (no minor collection
/// runs meanwhile). A full collection that `may_concur` marks
/// concurrently if `NABIR_GC_CONCURRENT` says so: this pause is then a
/// minor collection that starts the mark.
pub(crate) fn collect_locked(full: bool, may_concur: bool) {
    adopt_sweep(heap());
    if heap().marking {
        if !concurrent::assist() {
            concurrent::finish(true);
        }
        return;
    }
    let concur = full && may_concur && wants_concurrent();
    let mut p = Pause::begin();
    if concur {
        mark_sweep(false);
        // The minor's count: the mark starts from zero.
        p.marked = Some(mark().marked);
        concurrent::start();
        lap(Phase::Mark);
        p.end(Kind::Start);
    } else {
        mark_sweep(full);
        p.end(if full { Kind::Full } else { Kind::Minor });
    }
}

/// A full collection due now should mark concurrently.
pub(crate) fn wants_concurrent() -> bool {
    let h = heap();
    match h.concurrent {
        Concurrent::Always => true,
        Concurrent::Off => false,
        // A pause goal: always, and never stop the world to compact.
        Concurrent::Auto => {
            !h.evac_always
                && (h.pause_goal_ns != 0 || (!h.stw_next && h.old_bytes >= CONCURRENT_MIN))
        }
    }
}

/// A stop-the-world pause: every other mutator stopped, every TLAB and
/// medium range empty, the mark lock held (the marker thread waits).
/// Its end hands out the blocks anew ([`rebuild_claims`]).
pub(crate) struct Pause {
    t0: u64,
    c0: u64,
    /// When every other mutator stood still.
    stopped: u64,
    /// Other mutators registered.
    threads: usize,
    /// `live_bytes` before, and the young bytes of it.
    live: usize,
    young: usize,
    /// Objects marked, when not the mark state's count.
    pub(crate) marked: Option<usize>,
}

impl Pause {
    /// With the world lock held.
    pub(crate) fn begin() -> Self {
        let (t0, c0) = (now_ns(), sys::thread_cpu_ns() as u64);
        let threads = world::stop();
        let stopped = now_ns();
        let h = heap();
        h.phases.at = stopped;
        h.phases.ns = [0; 3];
        h.allocators = world::reset_tlabs();
        h.live_bytes += take_young();
        let live = h.live_bytes;
        let young = live.saturating_sub(h.old_bytes);
        concurrent::lock_mark();
        let h = heap();
        finish_sweep(h);
        note_claimed(h);
        Self {
            t0,
            c0,
            stopped,
            threads,
            live,
            young,
            marked: None,
        }
    }

    /// The pause of a `kind` collection ends.
    pub(crate) fn end(self, kind: Kind) {
        let marked = self.marked.unwrap_or_else(|| mark().marked);
        let h = heap();
        let (dt, ttsp) = spans(self.t0, self.stopped, now_ns());
        if h.log {
            let us = |ns: u64| ns / 1000;
            let [mk, sw, ev] = h.phases.ns;
            let at = self.t0.saturating_sub(h.phases.epoch);
            let _ = writeln!(
                Stderr,
                "nabi gc: #{} {}: {}.{:03} ms pause, {marked} objects marked, {} -> {} KiB; ttsp {} us, mark {} us, sweep {} us, evac {} us, threads {}, at {}.{:03} ms",
                h.collections,
                kind.label(),
                dt / 1_000_000,
                dt / 1000 % 1000,
                self.live >> 10,
                h.live_bytes >> 10,
                us(ttsp),
                us(mk),
                us(sw),
                us(ev),
                self.threads + 1,
                at / 1_000_000,
                at / 1000 % 1000
            );
        }
        // A thread's CPU clock may step back a little (FreeBSD).
        let cpu = (sys::thread_cpu_ns() as u64).saturating_sub(self.c0);
        h.gc_cpu_ns += cpu;
        h.gc_ns += dt;
        h.max_ns = h.max_ns.max(dt);
        if kind.full() {
            h.full_pauses += 1;
            h.full_ns += dt;
            h.full_max_ns = h.full_max_ns.max(dt);
            h.full_cpu_ns += cpu;
            h.full_cpu_max_ns = h.full_cpu_max_ns.max(cpu);
        }
        record(kind, dt, ttsp);
        if matches!(kind, Kind::Minor) {
            note_minor(h, self.young, dt - ttsp);
        }
        h.region_room = 0;
        let how = match kind {
            Kind::Minor | Kind::Start => Rebuild::Young,
            Kind::Full => Rebuild::Full,
            Kind::Finish => Rebuild::Lazy,
        };
        rebuild_claims(h, how);
        // Only now: the marker's lazy sweep appends to the claim lists.
        concurrent::unlock_mark();
        world::resume();
    }
}

/// A pause's length and its time to safepoint, from its start `t0`,
/// the stop of every other mutator and its end. The monotonic clock
/// may step back a little (FreeBSD's kvmclock): a negative span is 0,
/// and the time to safepoint is at most the pause.
pub(crate) fn spans(t0: u64, stopped: u64, end: u64) -> (u64, u64) {
    let dt = end.saturating_sub(t0);
    (dt, stopped.saturating_sub(t0).min(dt))
}

/// Queue the pinned objects and every root. `pin`: their referents must
/// not move (an evacuating collection).
pub(crate) fn mark_roots(pin: bool) {
    // SAFETY: the world is stopped: nobody is inside `pin` / `unpin`.
    // Through the lookup, so a young one's block gets parsed.
    for o in unsafe { &*PINS.table.get() }.objects() {
        push(o as usize, pin);
    }
    // Roots pin their referents: a root word may be an inline `any`
    // payload, and compiled code / runtime calls derive pointers from
    // rooted values (`str` bytes handed to C, element addresses), so an
    // object a frame reaches directly never moves.
    world::for_each_root(|word| push(word as usize, pin));
}

/// In a pause: mark from all registered threads' shadow stacks, pinned
/// objects and (minor) the remembered set, sweep, maybe evacuate.
pub(crate) fn mark_sweep(full: bool) {
    build_index();
    let m = mark();
    (m.full, m.concurrent, m.evacuating, m.len) = (full, false, full, 0);
    (m.marked, m.marked_bytes) = (0, 0);
    mark_roots(true);
    // Nobody logs while the world is stopped (the barrier has no
    // safepoint): every thread's chunk joins the retired ones.
    world::for_each_wb_log(retire_log);
    let log = LOG_FULL.swap(ptr::null_mut(), Ordering::Acquire);
    if !full {
        for_each_logged(log, |o| scan_children(&mut Main, o));
        scan_dirty_cards();
    }
    parallel::drain_parallel();
    if full && crate::modules::late() {
        parallel::drain_parallel();
    }
    clear_dead_weak(full);
    lap(Phase::Mark);
    if !full {
        // The logged (old) objects all survive: unlogged again.
        for_each_logged(log, |o| {
            // SAFETY: logged objects are live.
            unsafe {
                (*header(o))
                    .gc_flags
                    .fetch_or(FLAG_UNLOGGED, Ordering::Relaxed)
            };
        });
    }
    free_logs(log);
    let survived = sweep_blocks(full) + sweep_los(full, false);
    lap(Phase::Sweep);
    if mark().evacuating {
        if select_candidates() {
            evacuate();
        }
        lap(Phase::Evac);
        let m = mark();
        m.evacuating = false;
        m.pinned_len = 0;
    }
    let h = heap();
    let live = if full {
        survived
    } else {
        h.old_bytes + survived
    };
    h.live_bytes = live;
    h.old_bytes = live;
    h.next_gc = live.saturating_add(young_room(h, live));
    if full {
        h.full_at = live.saturating_mul(2).max(MIN_HEAP);
        set_full_lines_at(h);
        h.stw_next = false;
    } else {
        h.minors += 1;
    }
    h.collections += 1;
    if full {
        release_free_blocks();
    }
}

/// Minor collection: queue the referents in the dirty cards of every
/// carded (old LOS) object, cleaning the cards (every survivor of this
/// collection is old, so no old -> young reference outlives it).
pub(crate) fn scan_dirty_cards() {
    // SAFETY: walks the LOS list; each prefix's card table covers its
    // object.
    unsafe {
        let mut o = heap().los;
        while !o.is_null() {
            let p = prefix(o);
            if (*header(o)).gc_flags.load(Ordering::Relaxed) & FLAG_CARDED != 0 {
                let n = (*p).size.div_ceil(CARD_SIZE);
                let cards = (*p).cards;
                let mut c = 0;
                while c < n {
                    // Clean runs a word at a time.
                    if c % 8 == 0 && c + 8 <= n && cards.add(c).cast::<u64>().read_unaligned() == 0
                    {
                        c += 8;
                        continue;
                    }
                    if *cards.add(c) != 0 {
                        *cards.add(c) = 0;
                        scan_range(&mut Main, o, c * CARD_SIZE, (c + 1) * CARD_SIZE);
                    }
                    c += 1;
                }
            }
            o = (*p).next;
        }
    }
}

/// After marking: whether `obj` lives on (it was marked); a survivor's
/// mark is cleared and it becomes old (`STICKY | UNLOGGED`, or
/// `STICKY | CARDED` for a LOS object).
pub(crate) fn survives(obj: *mut u8) -> bool {
    // SAFETY: `obj` is an object header; the world is stopped, or the
    // lazy sweep owns its block (mutators may pin it or log it meanwhile:
    // one atomic update keeps their bits).
    let flags = unsafe { &(*header(obj)).gc_flags };
    let large = flags.load(Ordering::Relaxed) & FLAG_LARGE != 0;
    flags
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |f| {
            if f & FLAG_MARK == 0 {
                return None;
            }
            let f = f & !(FLAG_MARK
                | FLAG_STICKY
                | FLAG_UNLOGGED
                | FLAG_CARDED
                | FLAG_SATB
                | FLAG_SCANNED);
            Some(if large {
                f | FLAG_STICKY | FLAG_CARDED
            } else {
                f | FLAG_STICKY | FLAG_UNLOGGED
            })
        })
        .is_ok()
}
