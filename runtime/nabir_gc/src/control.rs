//! What a program asks of the collector: a pause goal, minor
//! collections on demand, regions that never collect, a heap limit.

#[allow(clippy::wildcard_imports)]
use crate::*;

/// A no-collect region is open on some thread (`Heap::no_collect`,
/// for the marker, which does not take the world lock to look).
pub(crate) static NO_COLLECT: AtomicUsize = AtomicUsize::new(0);

/// The young budget of a pause goal never drops below this.
const GOAL_YOUNG_MIN: usize = 1 << 20;
/// A region that runs out of budget grows the heap this much at a time.
const REGION_GROWTH: usize = 1 << 20;
/// A minor collection costs at least about this, whatever its size.
const MINOR_FLOOR_NS: u64 = 50_000;

/// The pause goal (`bare::gc::set_pause_goal`), in nanoseconds; 0: none.
///
/// The collector keeps minor collections near it by sizing the young
/// generation from the cost of the last ones, marks every full
/// collection concurrently and never compacts on its own.
pub fn set_pause_goal(ns: u64) {
    world::lock();
    let h = heap();
    configure(h);
    h.pause_goal_ns = ns;
    h.next_gc = h.old_bytes.saturating_add(young_room(h, h.old_bytes));
    CLAIMS.room.store(room(h), Ordering::Relaxed);
    world::unlock();
}

/// The heap limit (`bare::gc::set_heap_limit`) in bytes; 0: none.
///
/// Collections run sooner as the heap nears it, and full ones once it is
/// reached. Live data beyond it does not fail: the heap grows past it a
/// little at a time, collecting fully each step.
pub fn set_heap_limit(bytes: usize) {
    world::lock();
    let h = heap();
    configure(h);
    h.heap_limit = bytes;
    CLAIMS.room.store(room(h), Ordering::Relaxed);
    world::unlock();
}

/// A minor collection when the last ones say it takes at most `ns`
/// (`nabi_gc_collect_young_within`); whether it ran. None runs while a
/// concurrent mark does (it would finish the mark in a pause).
pub fn collect_young_within(ns: u64) -> bool {
    world::lock();
    let h = heap();
    configure(h);
    let young = young_bytes() + h.live_bytes.saturating_sub(h.old_bytes);
    let ok = !h.marking && predict_minor(h, young) <= ns;
    if ok {
        collect_locked(false, false);
    }
    world::unlock();
    ok
}

/// The pause a minor collection of `young` bytes is expected to take.
const fn predict_minor(h: &Heap, young: usize) -> u64 {
    MINOR_FLOOR_NS + (h.minor_ps_per_byte.saturating_mul(young as u64) / 1000)
}

/// After a minor collection of `young` bytes that took `ns`: learn its
/// cost per byte (a moving average).
pub(crate) fn note_minor(h: &mut Heap, young: usize, ns: u64) {
    if young < GOAL_YOUNG_MIN {
        return;
    }
    let rate = ns.saturating_sub(MINOR_FLOOR_NS).saturating_mul(1000) / young as u64;
    h.minor_ps_per_byte = if h.minor_ps_per_byte == 0 {
        rate
    } else {
        (h.minor_ps_per_byte * 3 + rate) / 4
    };
}

/// Into (`true`) or out of a no-collect region (`nabi_gc_no_collect`).
///
/// While one is open on any thread, allocation never collects; once the
/// young budget is spent the heap grows instead ([`region_grows`]). A
/// concurrent mark keeps marking but waits to finish until the last
/// region closes.
pub fn no_collect(enter: bool) {
    world::lock();
    let h = heap();
    configure(h);
    if enter {
        h.no_collect += 1;
    } else {
        h.no_collect = h.no_collect.saturating_sub(1);
    }
    NO_COLLECT.store(h.no_collect, Ordering::Release);
    if h.no_collect == 0 && h.marking {
        // The marker may be waiting for the regions to close.
        concurrent::wake_marker();
    }
    world::unlock();
}

/// With the world lock held, in a no-collect region, when `want` more
/// bytes would pass the budget: grow the heap instead of collecting
/// (counted in `bare::gc::stats`). False past the heap limit, which
/// regions do not lift.
pub(crate) fn region_grows(h: &mut Heap, live: usize, want: usize) -> bool {
    if h.no_collect == 0 || (h.heap_limit != 0 && live.saturating_add(want) > limit_cap(h)) {
        return false;
    }
    let grant = want.max(REGION_GROWTH);
    h.region_room += grant;
    h.grown_bytes += grant as u64;
    CLAIMS.room.store(room(h), Ordering::Relaxed);
    true
}

/// The heap size the limit allows: the limit, or a little more than the
/// live data when that alone exceeds it.
pub(crate) fn limit_cap(h: &Heap) -> usize {
    h.heap_limit
        .max(h.old_bytes.saturating_add((h.old_bytes / 8).max(4 << 20)))
}

/// Young bytes as many as old ones stop here: a minor collection sweeps
/// every block handed out since the last one.
const YOUNG_OLD_MAX: usize = 512 << 20;

/// The young part of the next budget over `live` old bytes: as many
/// young bytes as old ones (within the young budget and
/// [`YOUNG_OLD_MAX`]), or with a pause goal what the last minor
/// collections say fits in it.
pub(crate) fn young_room(h: &Heap, live: usize) -> usize {
    let yb = young_budget(h);
    if h.pause_goal_ns != 0 && h.minor_ps_per_byte != 0 {
        let fit =
            (h.pause_goal_ns.saturating_sub(MINOR_FLOOR_NS) * 1000 / h.minor_ps_per_byte) as usize;
        return (fit / 4 * 3).clamp(GOAL_YOUNG_MIN, yb.max(GOAL_YOUNG_MIN));
    }
    if h.young != 0 {
        return yb;
    }
    live.max(yb).min(YOUNG_OLD_MAX.max(yb))
}
