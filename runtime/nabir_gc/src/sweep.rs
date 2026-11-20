//! Sweeping blocks and the LOS, returning free blocks.

#[allow(clippy::wildcard_imports)]
use crate::*;

/// Sweep the blocks (minor: only the young blocks' `TAKEN` lines,
/// where all young objects are): survivors keep their lines, dead
/// objects lose their start bit. A concurrent mark's `BLACK` lines
/// (full) stay unswept and become `TAKEN`: their objects are young.
/// Bytes of the swept survivors.
pub(crate) fn sweep_blocks(full: bool) -> usize {
    let (mut live, mut young_lines) = (0, 0);
    let mut one = |b: usize| {
        let h = heap();
        // SAFETY: `b < nblocks`; the world is stopped.
        let (l, y) = sweep_block(
            h.base + b * BLOCK_SIZE,
            unsafe { &mut *h.meta.add(b) },
            full,
        );
        live += l;
        young_lines += y;
    };
    if full {
        for b in 0..heap().nblocks {
            one(b);
        }
    } else {
        for &b in young_blocks(heap()) {
            one(b as usize);
        }
    }
    // A full collection counts them all afterwards (`set_full_lines_at`).
    heap().old_lines += young_lines;
    live
}

/// Sweep the block at `block` (metadata `m`): its survivors' bytes and
/// (minor) the lines they turned from `TAKEN` to `LIVE`.
fn sweep_block(block: usize, m: &mut BlockMeta, full: bool) -> (usize, usize) {
    if !m.used {
        return (0, 0);
    }
    let mut lines = m.lines;
    // Eight lines a word: full, `BLACK` -> `TAKEN` and the rest `FREE`;
    // minor, `TAKEN` -> `FREE` (and none left: nothing to do).
    let mut any = 0;
    for w in lines.chunks_exact_mut(8) {
        let x = u64::from_ne_bytes([w[0], w[1], w[2], w[3], w[4], w[5], w[6], w[7]]);
        let black = spread(bytes_eq(x, BLACK));
        let taken = spread(bytes_eq(x, TAKEN));
        any |= taken;
        let y = if full {
            black & (ONES * u64::from(TAKEN))
        } else {
            (x & !taken & !black) | (black & (ONES * u64::from(TAKEN)))
        };
        w.copy_from_slice(&y.to_ne_bytes());
    }
    if !full && any == 0 {
        return (0, 0);
    }
    if !full && m.parse == 0 && !m.seeded {
        // No young survivor here, and no start bits to clear.
        m.used = free_lines(&lines) != LINES;
        m.lines = lines;
        return (0, 0);
    }
    m.parse = 0;
    m.seeded = false;
    let (old_lines, starts) = (m.lines, m.starts);
    let mut live = 0;
    for_each_start(&old_lines, &starts, full, |off| {
        let obj = (block + off) as *mut u8;
        let size = obj_size(obj);
        if survives(obj) {
            live += size;
            lines[off / LINE_SIZE..(off + size).div_ceil(LINE_SIZE)].fill(LIVE);
        } else {
            // Its bytes stay until the line is handed out again.
            m.starts[off / 8 / 64] &= !(1 << (off / 8 % 64));
        }
    });
    m.used = free_lines(&lines) != LINES;
    let young = if full {
        0
    } else {
        // Lines that went from `TAKEN` to `LIVE`, eight a word.
        lines
            .chunks_exact(8)
            .zip(old_lines.chunks_exact(8))
            .map(|(n, o)| {
                let w =
                    |c: &[u8]| u64::from_ne_bytes([c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]]);
                (bytes_eq(w(n), LIVE) & bytes_eq(w(o), TAKEN)).count_ones() as usize
            })
            .sum()
    };
    m.lines = lines;
    (live, young)
}

const ONES: u64 = 0x0101_0101_0101_0101;
const HIGH: u64 = 0x8080_8080_8080_8080;

/// The top bit of each byte of `w` equal to `v`.
const fn bytes_eq(w: u64, v: u8) -> u64 {
    let x = w ^ (ONES * v as u64);
    !(((x & !HIGH) + !HIGH) | x) & HIGH
}

/// The lines of `lines` in `state`, eight a word.
fn count_lines(lines: &[u8; LINES], state: u8) -> usize {
    lines
        .chunks_exact(8)
        .map(|c| {
            let w = u64::from_ne_bytes([c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]]);
            bytes_eq(w, state).count_ones() as usize
        })
        .sum()
}

/// Each byte of `mask` with its top bit set as `0xff`.
const fn spread(mask: u64) -> u64 {
    (mask >> 7) * 0xff
}

/// The sweep after a concurrent mark, done while mutators run: by the
/// marker thread, by a claim that finds the lists empty, and by the
/// next pause for whatever is left. Each swept block joins the claim
/// lists; free blocks beyond the next cycle's budget go back to the OS
/// first. Guarded by [`SWEEP_LOCK`].
struct Lazy {
    /// The concurrent mark (`Heap::cycles`) whose blocks `next..end`
    /// wait.
    cycle: u64,
    next: usize,
    end: usize,
    /// Free blocks still kept committed.
    keep: usize,
    /// `LIVE` lines; used blocks' lines, their free lines, and those of
    /// the blocks an evacuation would empty (the fragmentation test).
    live_lines: usize,
    used_lines: usize,
    free_lines: usize,
    gain: usize,
    /// `NABIR_GC_EVAC=always`: any used block is fragmentation.
    evac_always: bool,
    /// Free blocks `run[0]..run[1]` whose pages go back to the OS
    /// before they are listed.
    run: [usize; 2],
}

static LAZY: Global<Lazy> = Global(UnsafeCell::new(Lazy {
    cycle: 0,
    next: 0,
    end: 0,
    keep: 0,
    live_lines: 0,
    used_lines: 0,
    free_lines: 0,
    gain: 0,
    evac_always: false,
    run: [0; 2],
}));

static SWEEP_LOCK: sys::Lock = sys::Lock::new();
/// Blocks wait for the lazy sweep.
pub(crate) static SWEEPING: AtomicBool = AtomicBool::new(false);
/// A finished lazy sweep's numbers, for the next collection to adopt
/// ([`adopt_sweep`]): `LIVE` lines, and whether the heap is fragmented.
static SWEPT: AtomicBool = AtomicBool::new(false);
static SWEPT_LINES: AtomicUsize = AtomicUsize::new(0);
static SWEPT_FRAG: AtomicBool = AtomicBool::new(false);

/// Blocks a lazy sweep step takes on: about 20-50 us.
const SWEEP_STEP: usize = 32;

fn lazy() -> &'static mut Lazy {
    // SAFETY: callers hold `SWEEP_LOCK` (or the world is stopped with
    // no sweep running); no caller keeps the reference across another
    // `lazy()` call.
    unsafe { &mut *LAZY.0.get() }
}

/// In the final pause of a concurrent mark: the blocks wait for the
/// lazy sweep instead of being swept here. `keep` free blocks stay
/// committed.
pub(crate) fn begin_lazy(h: &mut Heap, keep: usize) {
    let z = lazy();
    *z = Lazy {
        cycle: h.cycles,
        next: 0,
        end: h.nblocks,
        keep,
        live_lines: 0,
        used_lines: 0,
        free_lines: 0,
        gain: 0,
        evac_always: h.evac_always,
        run: [0; 2],
    };
    // The listed free blocks stay listed and are not swept: claims may
    // take them meanwhile.
    for &[b, _] in free_listed() {
        // SAFETY: a block of the heap; the world is stopped.
        unsafe { (*h.meta.add(b as usize)).swept = h.cycles };
    }
    SWEPT.store(false, Ordering::Relaxed);
    SWEEPING.store(true, Ordering::Release);
}

/// One step of the lazy sweep, if one runs; whether it did anything (a
/// claim then looks at the lists again).
pub(crate) fn sweep_some() -> bool {
    if !SWEEPING.load(Ordering::Acquire) {
        return false;
    }
    SWEEP_LOCK.lock();
    let did = SWEEPING.load(Ordering::Relaxed);
    if did {
        sweep_step(SWEEP_STEP);
    }
    SWEEP_LOCK.unlock();
    did
}

/// In a pause (the world stopped, the mark lock held): finish the lazy
/// sweep, then adopt its numbers.
pub(crate) fn finish_sweep(h: &mut Heap) {
    if SWEEPING.load(Ordering::Acquire) {
        SWEEP_LOCK.lock();
        while SWEEPING.load(Ordering::Relaxed) {
            sweep_step(usize::MAX);
        }
        SWEEP_LOCK.unlock();
    }
    adopt_sweep(h);
}

/// With the world lock held: take over a finished lazy sweep's line
/// count and fragmentation verdict.
pub(crate) fn adopt_sweep(h: &mut Heap) {
    if !SWEPT.swap(false, Ordering::Acquire) {
        return;
    }
    h.old_lines = SWEPT_LINES.load(Ordering::Relaxed);
    h.full_lines_at = (h.old_lines * LINE_SIZE * 2).max(MIN_HEAP);
    if SWEPT_FRAG.load(Ordering::Relaxed) {
        // Fragmented: the next full collection stops the world and
        // evacuates (fixing references needs every slot stable).
        h.stw_next = true;
    }
}

/// Sweep up to `n` more blocks (with `SWEEP_LOCK`); the last one
/// publishes the numbers.
fn sweep_step(n: usize) {
    let z = lazy();
    let base = CLAIMS.base.load(Ordering::Relaxed);
    let stop = z.end.min(z.next.saturating_add(n));
    while z.next < stop {
        let b = z.next;
        z.next += 1;
        let m = meta_of(b);
        // SAFETY: a block of the heap below the mark's `nblocks`. One
        // still listed from the mark's end may be claimed meanwhile: only
        // its `swept` is read.
        if unsafe { (*m).swept } == z.cycle {
            continue;
        }
        // SAFETY: nobody claims it before it is listed below.
        let m = unsafe { &mut *m };
        m.swept = z.cycle;
        sweep_block(base + b * BLOCK_SIZE, m, true);
        let free = free_lines(&m.lines);
        if m.used {
            let occ = LINES - free;
            z.used_lines += LINES;
            z.free_lines += free;
            z.live_lines += count_lines(&m.lines, LIVE);
            if occ <= LINES / 2 {
                z.gain += free;
            }
        }
        if free == LINES && !m.released && z.keep == 0 {
            if z.run[1] != b {
                flush_run(z);
                z.run[0] = b;
            }
            z.run[1] = b + 1;
            continue;
        }
        if free == LINES && !m.released {
            z.keep -= 1;
        }
        list_block(b);
    }
    if z.next < z.end {
        return;
    }
    flush_run(z);
    let frag = if z.evac_always {
        z.used_lines > 0
    } else {
        !(z.used_lines == 0 || z.free_lines * 4 < z.used_lines || z.gain * 8 < z.used_lines)
    };
    SWEPT_LINES.store(z.live_lines, Ordering::Relaxed);
    SWEPT_FRAG.store(frag, Ordering::Relaxed);
    SWEPT.store(true, Ordering::Release);
    SWEEPING.store(false, Ordering::Release);
}

/// Give the free blocks of the pending run back to the OS, then list
/// them.
fn flush_run(z: &mut Lazy) {
    let [from, to] = z.run;
    z.run = [0; 2];
    if from == to {
        return;
    }
    let at = (CLAIMS.base.load(Ordering::Relaxed) + from * BLOCK_SIZE) as *mut u8;
    // SAFETY: whole committed blocks of our reservation that hold no
    // object and are in no list.
    if !unsafe { sys::reset(at, (to - from) * BLOCK_SIZE) } {
        nabir_common::oom();
    }
    for b in from..to {
        // SAFETY: as above.
        unsafe { (*meta_of(b)).released = true };
        list_block(b);
    }
}

/// After a full collection: count the `LIVE` lines and let them double
/// before the next full one.
#[allow(clippy::naive_bytecount)] // 256 bytes per block
pub(crate) fn set_full_lines_at(h: &mut Heap) {
    let metas = if h.nblocks == 0 {
        &[][..]
    } else {
        // SAFETY: `nblocks` initialized metas.
        unsafe { core::slice::from_raw_parts(h.meta, h.nblocks) }
    };
    h.old_lines = metas
        .iter()
        .filter(|m| m.used)
        .map(|m| m.lines.iter().filter(|&&l| l == LIVE).count())
        .sum();
    h.full_lines_at = (h.old_lines * LINE_SIZE * 2).max(MIN_HEAP);
}

/// Sweep the LOS list: free dead objects (minor: old ones stay
/// unexamined). Bytes of the swept survivors. `concurrent`: after a
/// concurrent mark, whose younger objects (in front of `los_mark_head`)
/// stay unexamined and whose survivors keep their dirty cards (they may
/// reference objects allocated during the mark, which are young).
pub(crate) fn sweep_los(full: bool, concurrent: bool) -> usize {
    let h = heap();
    let mut live = 0;
    let mut link: *mut *mut u8 = &mut h.los;
    let mut young = concurrent;
    // SAFETY: walks the LOS list; freed objects are unlinked before their
    // memory is released.
    unsafe {
        while !(*link).is_null() {
            let o = *link;
            let p = prefix(o);
            young &= o != h.los_mark_head;
            let old = (*header(o)).gc_flags.load(Ordering::Relaxed) & FLAG_STICKY != 0;
            if young || (!full && old) {
                link = &mut (*p).next;
            } else if survives(o) {
                live += (*p).size;
                if !concurrent {
                    // Clean: everything it references is old now.
                    ptr::write_bytes((*p).cards, 0, (*p).size.div_ceil(CARD_SIZE));
                }
                link = &mut (*p).next;
            } else {
                *link = (*p).next;
                h.los_count -= 1;
                if (*header(o)).gc_flags.load(Ordering::Relaxed) & FLAG_WIPE != 0 {
                    wipe(o);
                    #[cfg(test)]
                    tests::WIPED.fetch_add(1, Ordering::Relaxed);
                }
                let (base, total) = (o.sub((*p).offset), los_total(&*p));
                if total >= LOS_MAP_MIN {
                    sys::unmap(base, total);
                } else {
                    sys::free_aligned(base);
                }
            }
        }
    }
    live
}

/// After a full collection: give the pages of the free blocks beyond
/// the next cycle's allocation budget back to the OS. They stay
/// reserved and writable, and fault back in zeroed when reused.
pub(crate) fn release_free_blocks() {
    let h = heap();
    let mut keep = h.next_gc.saturating_sub(h.live_bytes).div_ceil(BLOCK_SIZE);
    let mut b = 0;
    while b < h.nblocks {
        // SAFETY: `b < nblocks`.
        let m = unsafe { &*h.meta.add(b) };
        if m.used || m.released || keep > 0 {
            keep -= usize::from(!m.used && !m.released);
            b += 1;
            continue;
        }
        let first = b;
        while b < h.nblocks {
            // SAFETY: `b < nblocks`.
            let m = unsafe { &mut *h.meta.add(b) };
            if m.used || m.released {
                break;
            }
            m.released = true;
            b += 1;
        }
        // SAFETY: whole committed blocks of our reservation that hold no
        // object.
        let at = (h.base + first * BLOCK_SIZE) as *mut u8;
        if !unsafe { sys::reset(at, (b - first) * BLOCK_SIZE) } {
            nabir_common::oom();
        }
    }
}
