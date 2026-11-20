//! Allocation: TLAB refills, block runs and large objects.

#[allow(clippy::wildcard_imports)]
use crate::*;

/// Allocate a zeroed GC object of `size` bytes (header included) and
/// initialize its header (`nabi_gc_alloc_slow`). May collect first.
/// Never null.
pub fn alloc(size: usize, align: usize, type_id: u32) -> *mut u8 {
    if size < HEADER_SIZE {
        fatal(b"nabi_gc_alloc_slow: size smaller than the object header");
    }
    if !align.is_power_of_two() {
        fatal(b"nabi runtime: allocation align is not a power of two");
    }
    // Compiled code asks for `usize::MAX` when `len * size` overflows.
    if size > isize::MAX as usize {
        fatal(b"nabi runtime: allocation too large");
    }
    let size = size.max(nabir_common::gc_abi::MIN_OBJECT_SIZE as usize);
    let size8 = size.next_multiple_of(8);
    // Type id 0 (invalid) would read as free memory in a
    // block.
    if size8 >= LOS_THRESHOLD || align > HEADER_ALIGN || type_id == 0 {
        return alloc_large(size, align, type_id, 0);
    }
    let tlab = nabir_common::tls::tlab();
    if let Some(obj) = bump(tlab, size8) {
        init_header(obj, type_id, size8, 0);
        return obj;
    }
    if !CLAIMS.configured.load(Ordering::Acquire) {
        world::lock();
        configure(heap());
        world::unlock();
    }
    if CLAIMS.stress.load(Ordering::Relaxed) {
        world::lock();
        before_alloc(size8.max(LINE_SIZE));
        world::unlock();
    }
    // An object over a line that misses the TLAB gets the medium range,
    // so the TLAB's tail stays usable.
    let medium = size8 > LINE_SIZE;
    let slots = if medium {
        let slots = nabir_common::tls::medium();
        if let Some(obj) = bump(slots, size8) {
            init_header(obj, type_id, size8, 0);
            return obj;
        }
        slots
    } else {
        tlab
    };
    let obj = refill(slots, size8, medium);
    init_header(obj, type_id, size8, 0);
    obj
}

/// Bump `size8` bytes off the range in `slots` (`[cursor, limit]`, this
/// thread's): the object, or `None` when it does not fit.
const fn bump(slots: [*mut usize; 2], size8: usize) -> Option<*mut u8> {
    let [cursor, limit] = slots;
    // SAFETY: this thread's own slots; `cursor..limit` is zeroed block
    // memory nobody else allocates from.
    unsafe {
        let c = cursor.read();
        if c != 0 && size8 <= limit.read() - c {
            cursor.write(c + size8);
            return Some(c as *mut u8);
        }
    }
    None
}

/// Blocks handed out whole to one allocating thread each: from the
/// claim lists (free blocks, then partly used ones), else a new block.
/// A thread then refills its TLAB and medium range from its blocks'
/// free lines without a lock. A pause rebuilds the lists from the
/// blocks that changed since the last one; after a concurrent mark the
/// lazy sweep appends the blocks it swept. A claim takes an entry with
/// a CAS on the list's cursor after an `Acquire` load of its length,
/// so it sees what the producer wrote before publishing it (line
/// states, `MARKING`).
pub(crate) struct Claims {
    free: ClaimList,
    recycled: ClaimList,
    /// Entries each list has room for: every block, as a block is
    /// listed at most once between two pauses.
    cap: AtomicUsize,
    /// Bytes claimed since the last pause (whole blocks' free lines),
    /// and how many may be before a collection is due.
    young: AtomicUsize,
    pub(crate) room: AtomicUsize,
    /// Pauses so far: a claim that waited for the world lock retries
    /// when one ran meanwhile.
    epoch: AtomicUsize,
    /// `Heap::base` / `Heap::meta`, for lock-free readers.
    pub(crate) base: AtomicUsize,
    pub(crate) meta: AtomicPtr<BlockMeta>,
    /// `NABIR_GC_*` read; `NABIR_GC_STRESS` set.
    configured: AtomicBool,
    stress: AtomicBool,
}

/// `(block, free lines)` entries in block order, appended by one
/// producer at a time (a pause, or the lazy sweep under its lock) and
/// taken from `next` on. A pause drops the taken ones.
pub(crate) struct ClaimList {
    at: AtomicPtr<[u32; 2]>,
    len: AtomicUsize,
    next: AtomicUsize,
}

impl ClaimList {
    #[allow(clippy::declare_interior_mutable_const)] // the statics' initializer
    const NEW: Self = Self {
        at: AtomicPtr::new(ptr::null_mut()),
        len: AtomicUsize::new(0),
        next: AtomicUsize::new(0),
    };

    /// The next entry, now the caller's.
    fn take(&self) -> Option<[u32; 2]> {
        let mut n = self.next.load(Ordering::Relaxed);
        loop {
            if n >= self.len.load(Ordering::Acquire) {
                return None;
            }
            match self
                .next
                .compare_exchange_weak(n, n + 1, Ordering::AcqRel, Ordering::Relaxed)
            {
                Ok(_) => break,
                Err(m) => n = m,
            }
        }
        // SAFETY: entry `n` is below the length its producer published
        // after writing it; entries never change until the next pause.
        Some(unsafe { self.at.load(Ordering::Relaxed).add(n).read() })
    }

    /// Append block `b` with `free` free lines (the only producer now;
    /// the list has room for every block).
    fn push(&self, b: usize, free: usize) {
        let n = self.len.load(Ordering::Relaxed);
        // SAFETY: `n` is below `cap`; `b` is a block of the heap no
        // claimer holds.
        unsafe {
            (*meta_of(b)).listed = true;
            self.at
                .load(Ordering::Relaxed)
                .add(n)
                .write([b as u32, free as u32]);
        }
        self.len.store(n + 1, Ordering::Release);
    }

    /// The entries taken since the last pause (world stopped).
    fn taken(&self) -> &[[u32; 2]] {
        let at = self.at.load(Ordering::Relaxed);
        if at.is_null() {
            return &[];
        }
        // SAFETY: entries below `next` (at most `len`) were written.
        unsafe { core::slice::from_raw_parts(at, self.next.load(Ordering::Relaxed)) }
    }

    /// Drop the taken entries (world stopped).
    fn compact(&self) {
        let (n, len) = (
            self.next.load(Ordering::Relaxed),
            self.len.load(Ordering::Relaxed),
        );
        if n > 0 {
            // SAFETY: entries `n..len` were written; the list is ours.
            unsafe {
                let at = self.at.load(Ordering::Relaxed);
                ptr::copy(at.add(n), at, len - n);
            }
        }
        self.len.store(len - n, Ordering::Relaxed);
        self.next.store(0, Ordering::Relaxed);
    }

    /// Drop every entry; the untaken ones are no longer listed (world
    /// stopped).
    fn clear(&self) {
        let (n, len) = (
            self.next.load(Ordering::Relaxed),
            self.len.load(Ordering::Relaxed),
        );
        let at = self.at.load(Ordering::Relaxed);
        for i in n..len {
            // SAFETY: a written entry, a block of the heap.
            unsafe { (*meta_of((*at.add(i))[0] as usize)).listed = false };
        }
        self.len.store(0, Ordering::Relaxed);
        self.next.store(0, Ordering::Relaxed);
    }

    /// Room for `cap` entries (world stopped).
    fn grow(&self, cap: usize) {
        let at = self.at.load(Ordering::Relaxed);
        // SAFETY: realloc of our own list (or null); nobody reads it in a
        // pause.
        let at = unsafe { libc::realloc(at.cast(), cap * size_of::<[u32; 2]>()) };
        if at.is_null() {
            nabir_common::oom();
        }
        self.at.store(at.cast(), Ordering::Relaxed);
    }

    /// Entries not taken yet.
    pub(crate) fn untaken(&self) -> &[[u32; 2]] {
        let at = self.at.load(Ordering::Relaxed);
        if at.is_null() {
            return &[];
        }
        let (n, len) = (
            self.next.load(Ordering::Relaxed),
            self.len.load(Ordering::Acquire),
        );
        // SAFETY: entries `n..len` were written.
        unsafe { core::slice::from_raw_parts(at.add(n), len.saturating_sub(n)) }
    }
}

pub(crate) static CLAIMS: Claims = Claims {
    free: ClaimList::NEW,
    recycled: ClaimList::NEW,
    cap: AtomicUsize::new(0),
    young: AtomicUsize::new(0),
    room: AtomicUsize::new(0),
    epoch: AtomicUsize::new(0),
    base: AtomicUsize::new(0),
    meta: AtomicPtr::new(ptr::null_mut()),
    configured: AtomicBool::new(false),
    stress: AtomicBool::new(false),
};

/// A partly used block joins the recycled list with this many free
/// lines: fewer cost a claim for little room.
pub(crate) const RECYCLE_MIN: usize = 8;

/// Bytes handed out since the last pause (claimed blocks count in full).
pub(crate) fn young_bytes() -> usize {
    CLAIMS.young.load(Ordering::Relaxed)
}

/// In a pause: [`young_bytes`], reset.
pub(crate) fn take_young() -> usize {
    CLAIMS.young.swap(0, Ordering::Relaxed)
}

/// Refill the range in `slots` (the TLAB, or the medium range if
/// `medium`) with a run of free lines for `size8` bytes and bump the
/// object off it: the next run of its block, else the first of a newly
/// claimed block. The run's lines become `TAKEN` (`BLACK` while a
/// concurrent mark runs) and are zeroed.
fn refill(slots: [*mut usize; 2], size8: usize, medium: bool) -> *mut u8 {
    let lines = size8.div_ceil(LINE_SIZE);
    // SAFETY: this thread's own slot.
    let limit = unsafe { slots[1].read() };
    let run = if limit == 0 {
        None
    } else {
        // The range lies in a block this thread claimed since the last
        // pause (which emptied every range): its free lines are ours.
        let base = CLAIMS.base.load(Ordering::Relaxed);
        let b = (limit - 1 - base) / BLOCK_SIZE;
        let from = (limit - base - b * BLOCK_SIZE).div_ceil(LINE_SIZE);
        free_run(b, from, lines).map(|r| (b, r))
    };
    let (b, (l, end)) = match run {
        Some(r) => r,
        None => loop {
            let b = claim(medium);
            if let Some(r) = free_run(b, 0, lines) {
                break (b, r);
            }
        },
    };
    let stress = CLAIMS.stress.load(Ordering::Relaxed);
    // Stress: a range of exactly this object, so the next allocation
    // comes back here too.
    let end = if stress { l + lines } else { end };
    let m = meta_of(b);
    let state = if concurrent::MARKING.load(Ordering::Relaxed) {
        BLACK
    } else {
        TAKEN
    };
    for i in l..end {
        set_line(m, i, state);
    }
    let block = CLAIMS.base.load(Ordering::Relaxed) + b * BLOCK_SIZE;
    let (start, run_end) = (block + l * LINE_SIZE, block + end * LINE_SIZE);
    // SAFETY: fields the mark does not read, of a block this thread
    // claimed; its lines `l..end` are ours, and dirty unless the block
    // came zeroed.
    unsafe {
        (*m).used = true;
        (*m).released = false;
        if !(*m).clean {
            ptr::write_bytes(start as *mut u8, 0, run_end - start);
        }
        slots[0].write(start + size8);
        slots[1].write(if stress { start + size8 } else { run_end });
    }
    start as *mut u8
}

pub(crate) fn meta_of(b: usize) -> *mut BlockMeta {
    // SAFETY: callers pass the index of a committed block.
    unsafe { CLAIMS.meta.load(Ordering::Relaxed).add(b) }
}

/// The first run of at least `lines` free lines of block `b` from line
/// `from`: `(first, end)`, the run's end being where its free lines
/// end.
fn free_run(b: usize, from: usize, lines: usize) -> Option<(usize, usize)> {
    let m = meta_of(b);
    let mut l = from;
    while l + lines <= LINES {
        if line(m, l) != FREE {
            l += 1;
            continue;
        }
        let mut end = l + 1;
        while end < LINES && line(m, end) == FREE {
            end += 1;
        }
        if end - l >= lines {
            return Some((l, end));
        }
        l = end;
    }
    None
}

/// A block for this thread alone until the next pause: the next one of
/// the free list, then of the recycled list (not for `medium` ranges,
/// which want long runs), then one the lazy sweep frees up, else a new
/// one. Collects first once the young budget is spent.
fn claim(medium: bool) -> usize {
    loop {
        let epoch = CLAIMS.epoch.load(Ordering::Acquire);
        if young_bytes() >= CLAIMS.room.load(Ordering::Relaxed) {
            // A pause would finish the lazy sweep: sweep here instead,
            // while the other threads run.
            if sweep_some() {
                continue;
            }
            world::lock();
            collect_if(false, BLOCK_SIZE);
            world::unlock();
            if CLAIMS.epoch.load(Ordering::Acquire) != epoch {
                continue;
            }
        }
        if let Some(b) = from_lists(medium) {
            return b;
        }
        if sweep_some() {
            continue;
        }
        world::lock();
        if CLAIMS.epoch.load(Ordering::Acquire) == epoch {
            collect_if(false, BLOCK_SIZE);
        }
        if CLAIMS.epoch.load(Ordering::Acquire) != epoch {
            world::unlock();
            continue;
        }
        let b = new_block();
        // SAFETY: a fresh block, ours: zero, not yet in a list.
        unsafe { (*heap().meta.add(b)).clean = true };
        // Young until the next pause. Not by index: `nabi_flat` takes
        // a whole chunk's blocks at once and hands them out later.
        push_young(heap(), b as u32);
        CLAIMS.young.fetch_add(BLOCK_SIZE, Ordering::Relaxed);
        world::unlock();
        return b;
    }
}

/// The next block of the lists, claimed: its free bytes count as young.
fn from_lists(medium: bool) -> Option<usize> {
    let c = &CLAIMS;
    // Free blocks first: the survivors of a cycle land in the blocks
    // it filled last, so recycled ones stay the same few.
    let [block, free] = match c.free.take() {
        Some(e) => e,
        None if medium => return None,
        None => c.recycled.take()?,
    };
    let m = meta_of(block as usize);
    // SAFETY: the block is ours now; `released` pages fault in zeroed.
    unsafe {
        (*m).listed = false;
        (*m).clean = (*m).released;
    }
    c.young
        .fetch_add(free as usize * LINE_SIZE, Ordering::Relaxed);
    Some(block as usize)
}

/// List block `b` by its free lines, unless it is listed already.
pub(crate) fn list_block(b: usize) {
    let m = meta_of(b);
    // SAFETY: a block of the heap nobody claims now.
    let (listed, n) = unsafe { ((*m).listed, free_lines(&(*m).lines)) };
    if listed {
        return;
    }
    if n == LINES {
        CLAIMS.free.push(b, n);
    } else if n >= RECYCLE_MIN {
        CLAIMS.recycled.push(b, n);
    }
}

/// The free list's untaken entries (the lazy sweep passes them by).
pub(crate) fn free_listed() -> &'static [[u32; 2]] {
    CLAIMS.free.untaken()
}

/// At the start of a pause (the world stopped, a lazy sweep done): note
/// the blocks claimed from the lists since the last pause as young (new
/// ones are noted as they are claimed).
pub(crate) fn note_claimed(h: &mut Heap) {
    let c = &CLAIMS;
    for list in [&c.free, &c.recycled] {
        for &[b, _] in list.taken() {
            push_young(h, b);
        }
    }
}

fn push_young(h: &mut Heap, b: u32) {
    if h.young_len == h.young_cap {
        let cap = (h.young_cap * 2).max(1024);
        // SAFETY: realloc of our own list (or null).
        let p = unsafe { libc::realloc(h.young_blocks.cast(), cap * size_of::<u32>()) };
        if p.is_null() {
            nabir_common::oom();
        }
        h.young_blocks = p.cast();
        h.young_cap = cap;
    }
    // SAFETY: `young_len < young_cap`.
    unsafe { h.young_blocks.add(h.young_len).write(b) };
    h.young_len += 1;
}

/// The young blocks of [`Heap::young_blocks`].
pub(crate) const fn young_blocks(h: &Heap) -> &'static [u32] {
    if h.young_len == 0 {
        return &[];
    }
    // SAFETY: `young_len` written entries; the list changes only under
    // the world lock, never while a pause reads it.
    unsafe { core::slice::from_raw_parts(h.young_blocks, h.young_len) }
}

/// How a pause's end rebuilds the claim lists.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Rebuild {
    /// After a minor collection: the young blocks change, nothing else.
    Young,
    /// After a stop-the-world full collection: every block.
    Full,
    /// After a concurrent mark: the free blocks stay listed, the rest
    /// waits for the lazy sweep, which lists them as it goes (the young
    /// blocks stay young: their `BLACK` lines are `TAKEN` now).
    Lazy,
}

/// At the end of a pause (the world still stopped): the claim lists,
/// the young bytes reset and the room left before the next collection.
/// Then a new epoch.
pub(crate) fn rebuild_claims(h: &mut Heap, how: Rebuild) {
    let c = &CLAIMS;
    if c.cap.load(Ordering::Relaxed) < h.nblocks {
        let cap = h.nblocks.next_power_of_two().max(64);
        c.free.grow(cap);
        c.recycled.grow(cap);
        c.cap.store(cap, Ordering::Relaxed);
    }
    match how {
        Rebuild::Full => {
            c.free.clear();
            c.recycled.clear();
            for b in 0..h.nblocks {
                list_block(b);
            }
            h.young_len = 0;
        }
        Rebuild::Young => {
            c.free.compact();
            c.recycled.compact();
            for &b in young_blocks(h) {
                list_block(b as usize);
            }
            h.young_len = 0;
        }
        Rebuild::Lazy => {
            c.free.compact();
            c.recycled.clear();
        }
    }
    c.young.store(0, Ordering::Relaxed);
    c.room.store(room(h), Ordering::Relaxed);
    c.epoch.fetch_add(1, Ordering::Release);
}

/// The `FREE` (zero) lines of a line table, a word at a time.
pub(crate) fn free_lines(lines: &[u8; LINES]) -> usize {
    const LOW: u64 = 0x7f7f_7f7f_7f7f_7f7f;
    let mut used = 0;
    for w in lines.chunks_exact(8) {
        let w = u64::from_ne_bytes([w[0], w[1], w[2], w[3], w[4], w[5], w[6], w[7]]);
        // The top bit of each nonzero byte.
        used += ((((w & LOW) + LOW) | w) & !LOW).count_ones() as usize;
    }
    LINES - used
}

/// With the lock held: the bytes that may still be claimed before a
/// collection is due.
pub(crate) fn room(h: &Heap) -> usize {
    budget(h).saturating_sub(h.live_bytes)
}

/// With the lock held: the heap size at which a collection is due. A
/// concurrent mark gets twice the young budget, then what its mark
/// assists earn ([`concurrent::assist`]).
fn budget(h: &Heap) -> usize {
    budget_of(h).0
}

/// [`budget`], and whether the heap limit lowered it. Open no-collect
/// regions add what they grew the heap by.
fn budget_of(h: &Heap) -> (usize, bool) {
    let b = if h.marking {
        (h.next_gc.saturating_mul(2) - h.mark_start_live.min(h.next_gc)).saturating_add(h.assisted)
    } else {
        h.next_gc
    };
    let b = b.saturating_add(h.region_room);
    if h.heap_limit != 0 && b > limit_cap(h) {
        return (limit_cap(h), true);
    }
    (b, false)
}

/// With the lock held, during a concurrent mark: the heap size past
/// which allocation no longer assists but finishes the mark in a pause.
pub(crate) fn assist_cap(h: &Heap) -> usize {
    let cap = h.next_gc.saturating_mul(4) - h.mark_start_live.min(h.next_gc);
    if h.heap_limit != 0 {
        return cap.min(limit_cap(h));
    }
    cap
}

/// With the lock held, before handing out `want` more bytes: collect
/// if the heap outgrew its budget or `NABIR_GC_STRESS` says so.
pub(crate) fn before_alloc(want: usize) {
    let h = heap();
    configure(h);
    h.allocs += 1;
    h.since_full = h.since_full.saturating_add(want);
    let stressed = h.stress != 0 && h.allocs % h.stress == 0;
    collect_if(stressed, want);
}

/// With the lock held: collect if `stressed` or the heap outgrew its
/// budget with `want` more bytes. Stress: every 8th collection is full,
/// but only once the heap handed out a sixteenth of the old generation
/// since the last one, so a large heap costs linear time, not
/// quadratic.
fn collect_if(stressed: bool, want: usize) {
    let h = heap();
    adopt_sweep(h);
    let live = h.live_bytes + young_bytes();
    let (budget, limited) = budget_of(h);
    let over = live.saturating_add(want) > budget;
    if stressed || over {
        // A no-collect region: grow instead (not past the heap limit).
        if (over && region_grows(h, live, want)) || (!over && h.no_collect > 0) {
            return;
        }
        if over && limited {
            h.limit_collections += 1;
        }
        let full = h.old_bytes >= h.full_at
            || h.old_lines * LINE_SIZE >= h.full_lines_at
            || (over && limited)
            || (stressed && (h.allocs / h.stress) % 8 == 0 && h.since_full >= h.old_bytes / 16);
        if full {
            h.since_full = 0;
        }
        collect_locked(full, true);
    }
}

/// The young budget: `NABIR_GC_YOUNG`, else [`YOUNG_PER_THREAD`] for
/// each thread that allocated in the last cycle, within
/// [`YOUNG_MIN`]..=[`YOUNG_MAX`].
pub(crate) fn young_budget(h: &Heap) -> usize {
    if h.young != 0 {
        return h.young;
    }
    (YOUNG_PER_THREAD * h.allocators).clamp(YOUNG_MIN, YOUNG_MAX)
}

/// Read the `NABIR_GC_*` knobs once, before the first collection.
pub(crate) fn configure(h: &mut Heap) {
    if h.configured {
        return;
    }
    h.configured = true;
    h.phases.epoch = now_ns();
    h.stress = env_num(c"NABIR_GC_STRESS", false).unwrap_or(0);
    h.young = env_num(c"NABIR_GC_YOUNG", true).map_or(0, |n| n as usize);
    h.evac_always = env_choice(c"NABIR_GC_EVAC", "always").is_some();
    h.concurrent = match env_choice(c"NABIR_GC_CONCURRENT", "auto|always|off") {
        Some("always") => Concurrent::Always,
        Some("off") => Concurrent::Off,
        _ => Concurrent::Auto,
    };
    // One thread: no marker thread to run beside the mutator.
    if cfg!(nabi_solo) {
        h.concurrent = Concurrent::Off;
    }
    if env_flag(c"NABIR_GC_STATS") {
        sys::atexit(print_stats);
    }
    h.log = env_flag(c"NABIR_GC_LOG");
    if env_choice(c"NABIR_HEAP_DUMP", "on_oom").is_some() {
        nabir_common::set_oom_hook(heap_dump);
    }
    if h.collections == 0 {
        h.next_gc = h.live_bytes + young_room(h, 0);
    }
    CLAIMS.room.store(room(h), Ordering::Relaxed);
    CLAIMS.stress.store(h.stress != 0, Ordering::Relaxed);
    CLAIMS.configured.store(true, Ordering::Release);
}

/// With the lock held: commit one more (zeroed) block; its index.
pub(crate) fn new_block() -> usize {
    let h = heap();
    if h.meta.is_null() {
        reserve_blocks(h);
        CLAIMS.base.store(h.base, Ordering::Relaxed);
        CLAIMS.meta.store(h.meta, Ordering::Relaxed);
    }
    #[cfg(nabi_flat)]
    let b = spare_block(h);
    #[cfg(not(nabi_flat))]
    let b = {
        if h.nblocks == h.reserved / BLOCK_SIZE {
            nabir_common::oom();
        }
        let b = h.nblocks;
        // Commit in 64 KiB steps (a multiple of every page size): block
        // `b`'s metadata pages, unless block `b - 1`'s commit covered them.
        let step = 64 << 10;
        let (start, end) = (b * size_of::<BlockMeta>(), (b + 1) * size_of::<BlockMeta>());
        let from = if b == 0 {
            0
        } else {
            start.next_multiple_of(step)
        };
        let to = end.next_multiple_of(step);
        // SAFETY: block `b` and its metadata lie inside our reservations.
        let ok = unsafe {
            sys::commit((h.base + b * BLOCK_SIZE) as *mut u8, BLOCK_SIZE)
                && (from >= to || sys::commit(h.meta.cast::<u8>().add(from), to - from))
        };
        if !ok {
            nabir_common::oom();
        }
        h.nblocks += 1;
        b
    };
    // SAFETY: `b` is below `nblocks`.
    init_meta(unsafe { h.meta.add(b) }, FREE);
    b
}

/// Reset the metadata at `m` to a block of `state` lines holding no
/// object. Lines are stored atomically: a concurrent mark may look up a
/// word that lands in the block.
pub(crate) fn init_meta(m: *mut BlockMeta, state: u8) {
    for l in 0..LINES {
        set_line(m, l, state);
    }
    // SAFETY: fields a concurrent mark does not read in a block without
    // `TAKEN` lines.
    unsafe {
        (&raw mut (*m).starts).write([0; GRANULES / 64]);
        (&raw mut (*m).used).write(false);
        (&raw mut (*m).parse).write(0);
        (&raw mut (*m).seeded).write(false);
        (&raw mut (*m).evac).write(false);
        (&raw mut (*m).released).write(state != FREE);
        (&raw mut (*m).clean).write(false);
        (&raw mut (*m).listed).write(false);
        (&raw mut (*m).swept).write(0);
    }
}

/// The block reservation: the first of the sizes from [`RESERVE`] down
/// the OS grants. Metadata for every block of it is reserved once and
/// committed as blocks are: a concurrent mark reads it while refills
/// commit blocks, so it never moves.
#[cfg(not(nabi_flat))]
pub(crate) fn reserve_blocks(h: &mut Heap) {
    let mut len = RESERVE;
    let p = loop {
        let p = sys::reserve(len + BLOCK_SIZE);
        if !p.is_null() {
            break p;
        }
        len /= 2;
        if len < RESERVE_MIN {
            nabir_common::oom();
        }
    };
    h.base = (p as usize).next_multiple_of(BLOCK_SIZE);
    h.reserved = len;
    h.meta = sys::reserve(len / BLOCK_SIZE * size_of::<BlockMeta>()).cast();
    if h.meta.is_null() {
        nabir_common::oom();
    }
}

/// wasm32 has one linear memory, which `malloc` grows too, in steps it
/// assumes nobody else interleaves: the heap takes its blocks from
/// `malloc` ([`spare_block`]) and never grows memory itself. Block `i`
/// is at `i * BLOCK_SIZE` (`base` 0). The metadata covers the 4 GiB
/// address space (about 2.4% of it, written only up to the highest
/// block) or, under a lower engine limit, the first of its halves
/// `malloc` grants. A target without an OS does the same over its heap.
#[cfg(nabi_flat)]
pub(crate) fn reserve_blocks(h: &mut Heap) {
    let mut n = usize::MAX / BLOCK_SIZE;
    loop {
        // SAFETY: plain allocation, never freed.
        h.meta = unsafe { libc::malloc(n * size_of::<BlockMeta>()) }.cast();
        if !h.meta.is_null() {
            break;
        }
        n /= 2;
        if n * BLOCK_SIZE < 64 << 20 {
            nabir_common::oom();
        }
    }
    h.reserved = n * BLOCK_SIZE;
}

/// `nabi_flat`: the next block of the current chunk, taking a new chunk
/// (1 MiB) from `malloc` when it is used up. The blocks between chunks
/// are `malloc`'s and a chunk's stay its own until handed out, both
/// [`FOREIGN`]. Chunks are never freed.
#[cfg(nabi_flat)]
pub(crate) fn spare_block(h: &mut Heap) -> usize {
    const CHUNK: usize = 1 << 20;
    if h.spare[0] == h.spare[1] {
        let p = sys::alloc_aligned(CHUNK, BLOCK_SIZE) as usize;
        if p == 0 || p > h.reserved - CHUNK {
            nabir_common::oom();
        }
        // SAFETY: a fresh allocation of `CHUNK` bytes.
        unsafe { ptr::write_bytes(p as *mut u8, 0, CHUNK) };
        let end = (p + CHUNK) / BLOCK_SIZE;
        while h.nblocks < end {
            // SAFETY: `nblocks` is below the metadata's block count.
            init_meta(unsafe { h.meta.add(h.nblocks) }, FOREIGN);
            h.nblocks += 1;
        }
        h.spare = [p / BLOCK_SIZE, end];
    }
    let b = h.spare[0];
    h.spare[0] += 1;
    b
}

/// LOS allocations from this size on (prefix and card table included)
/// get pages of their own, zero-filled by the OS and unmapped when the
/// object dies; smaller ones (over-aligned small objects, objects just
/// over [`LOS_THRESHOLD`]) come from the C heap.
pub(crate) const LOS_MAP_MIN: usize = 64 << 10;

/// Bytes of a LOS object's allocation.
pub(crate) const fn los_total(p: &Prefix) -> usize {
    p.offset + los_extra(p.size)
}

/// Bytes of a LOS object of `size` with its card table and its
/// snapshot bits (2 per card: claimed, logged; whole words after the
/// cards).
const fn los_extra(size: usize) -> usize {
    let cards = size.div_ceil(CARD_SIZE);
    (size + cards).next_multiple_of(8) + cards.div_ceil(32) * 8
}

/// The snapshot bits of the LOS object `obj` ([`los_extra`]).
#[allow(clippy::cast_ptr_alignment)] // 8-aligned after the cards
pub(crate) fn satb_bits(obj: *mut u8) -> (*mut u64, usize) {
    // SAFETY: a LOS object's prefix; the bits follow its card table.
    unsafe {
        let p = &*prefix(obj);
        let cards = p.size.div_ceil(CARD_SIZE);
        let at = obj.add((p.size + cards).next_multiple_of(8)).cast::<u64>();
        (at, cards.div_ceil(32))
    }
}

/// A large (or over-aligned) object: its own zeroed allocation, header
/// flags `LARGE | flags`. The payload (after the header) is
/// `align`-aligned, and at least [`LOS_ALIGN`]-aligned from
/// [`LOS_THRESHOLD`] on.
pub(crate) fn alloc_large(size: usize, align: usize, type_id: u32, flags: u32) -> *mut u8 {
    let floor = if size >= LOS_THRESHOLD {
        LOS_ALIGN
    } else {
        HEADER_ALIGN
    };
    let align = align.max(floor);
    let offset = (size_of::<Prefix>() + HEADER_SIZE).next_multiple_of(align) - HEADER_SIZE;
    let Some(total) = size
        .checked_add(offset + CARD_SIZE + 16)
        .map(|_| offset + los_extra(size))
    else {
        fatal(b"nabi runtime: allocation size overflow");
    };
    let base = if total >= LOS_MAP_MIN {
        sys::map(total, align)
    } else {
        let base = sys::alloc_aligned(total, align);
        if !base.is_null() {
            // SAFETY: a fresh block of `total` bytes.
            unsafe { ptr::write_bytes(base, 0, total) };
        }
        base
    };
    if base.is_null() {
        nabir_common::oom();
    }
    world::lock();
    // The new block is not on the list yet, so a collection here cannot
    // free it.
    before_alloc(size);
    let h = heap();
    // SAFETY: `base..base+total` is ours; `obj`'s payload is
    // `align`-aligned and the prefix fits in the `offset` bytes before it.
    let obj = unsafe {
        let obj = base.cast::<u8>().add(offset);
        prefix(obj).write(Prefix {
            next: h.los,
            size,
            offset,
            cards: obj.add(size),
        });
        init_header(obj, type_id, size, FLAG_LARGE | flags);
        h.los = obj;
        h.los_count += 1;
        h.live_bytes += size;
        CLAIMS.room.store(room(h), Ordering::Relaxed);
        obj
    };
    world::unlock();
    obj
}

/// A secret buffer of `len` zeroed bytes (`nabi_gc_secret_new`).
///
/// A LOS object with no references, so it never moves and is never
/// copied, marked `WIPE`, so the sweep that frees it zeroes it first
/// ([`wipe`]).
pub fn alloc_secret(len: usize) -> *mut u8 {
    let Some(size) = len
        .checked_add(HEADER_SIZE + 7)
        .filter(|&s| isize::try_from(s).is_ok())
    else {
        fatal(b"nabi runtime: allocation too large");
    };
    let size = (size & !7).max(MIN_OBJECT_SIZE as usize);
    alloc_large(size, HEADER_ALIGN, PLAIN_TYPE_ID, FLAG_WIPE)
}

/// Zero the payload of the LOS object `obj` with volatile stores, which
/// the optimizer keeps even when nothing reads the memory again (the
/// release right after the sweep's wipe).
///
/// # Safety
/// `obj` must be a live LOS object (or one the sweep is freeing).
#[allow(clippy::cast_ptr_alignment)] // the payload is 8-aligned
pub unsafe fn wipe(obj: *mut u8) {
    // SAFETY: a LOS object's size (header included) is a multiple of 8
    // and its payload is 8-aligned.
    unsafe {
        let words = ((*prefix(obj)).size - HEADER_SIZE) / 8;
        let p = obj.add(HEADER_SIZE).cast::<u64>();
        for i in 0..words {
            p.add(i).write_volatile(0);
        }
    }
    core::sync::atomic::compiler_fence(Ordering::SeqCst);
}
