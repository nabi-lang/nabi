//! Scan tables and the heap view: object starts, the LOS index, interior lookups.

#[allow(clippy::wildcard_imports)]
use crate::*;

/// Register the `ScanTable` for `type_id`. Ids below the user range
/// (the runtime's own) are ignored.
pub fn register_type(type_id: u32, table: *const u8) {
    let Some(i) = type_id.checked_sub(USER_TYPE_BASE) else {
        return;
    };
    let i = i as usize;
    world::lock();
    let mut t = TABLES.load(Ordering::Acquire);
    // SAFETY: `t` is null or a `[len, tables..]` array from below; a
    // grown array replaces it whole and the old one is never freed (the
    // marker and barriers read it without the lock).
    unsafe {
        let len = if t.is_null() { 0 } else { *t };
        if i >= len {
            let cap = (i + 1).next_power_of_two().max(64);
            let p = libc::calloc(cap + 1, size_of::<usize>()).cast::<usize>();
            if p.is_null() {
                nabir_common::oom();
            }
            *p = cap;
            if len != 0 {
                ptr::copy_nonoverlapping(t.add(1), p.add(1), len);
            }
            t = p;
        }
        core::sync::atomic::AtomicUsize::from_ptr(t.add(1 + i))
            .store(table as usize, Ordering::Release);
        TABLES.store(t, Ordering::Release);
    }
    world::unlock();
}

/// Registered `ScanTable`s: `[len, table of USER_TYPE_BASE + i..]`.
pub(crate) static TABLES: core::sync::atomic::AtomicPtr<usize> =
    core::sync::atomic::AtomicPtr::new(ptr::null_mut());

pub(crate) fn scan_table(type_id: u32) -> *const u8 {
    let t = TABLES.load(Ordering::Acquire);
    match type_id.checked_sub(USER_TYPE_BASE) {
        // SAFETY: a bounds-checked entry of the live array.
        Some(i) if !t.is_null() && (i as usize) < unsafe { *t } => unsafe {
            core::sync::atomic::AtomicUsize::from_ptr(t.add(1 + i as usize)).load(Ordering::Acquire)
                as *const u8
        },
        _ if type_id == WEAK_TYPE_ID => WEAK_TABLE.as_ptr().cast(),
        _ => ptr::null(),
    }
}

/// Set the start bits of every object in the `TAKEN` lines of the block
/// at `block` (metadata `m`) by parsing them: an object has a nonzero
/// `type_id` (0 is reserved) and its size in `gc_word`; everything else
/// is zero. A run of `TAKEN` lines was all free when handed out, so no
/// object crosses its ends. Bits are set atomically: parallel markers
/// may seed others meanwhile ([`push_start`]).
pub(crate) fn parse_taken(block: usize, m: *mut BlockMeta) {
    let mut l = 0;
    while l < LINES {
        if line(m, l) != TAKEN {
            l += 1;
            continue;
        }
        let mut end = l + 1;
        while end < LINES && line(m, end) == TAKEN {
            end += 1;
        }
        let mut off = l * LINE_SIZE;
        while off < end * LINE_SIZE {
            let obj = (block + off) as *mut u8;
            // SAFETY: `off` is inside the run: an object header or zeroes.
            if unsafe { (*header(obj)).type_id } == 0 {
                off += 8;
            } else {
                set_start(m, off);
                off += obj_size(obj);
            }
        }
        l = end;
    }
}

/// Set the start bit of the object at byte `off` of the block whose
/// metadata is `m` (atomic: parallel markers).
pub(crate) fn set_start(m: *mut BlockMeta, off: usize) {
    // SAFETY: a word of the block's start bitmap.
    unsafe {
        nabir_common::AtomicU64::from_ptr((&raw mut (*m).starts).cast::<u64>().add(off / 8 / 64))
            .fetch_or(1 << (off / 8 % 64), Ordering::Relaxed);
    }
}

/// Word `i` of the start bitmap of the block whose metadata is `m`.
fn start_word(m: *const BlockMeta, i: usize) -> u64 {
    // SAFETY: as in `set_start`.
    unsafe {
        nabir_common::AtomicU64::from_ptr((&raw const (*m).starts).cast::<u64>().cast_mut().add(i))
            .load(Ordering::Relaxed)
    }
}

/// The block's `parse` state, as an atomic.
pub(crate) fn parse_state(m: *mut BlockMeta) -> &'static core::sync::atomic::AtomicU8 {
    // SAFETY: a field of the block's metadata, which never moves.
    unsafe { core::sync::atomic::AtomicU8::from_ptr(&raw mut (*m).parse) }
}

/// Make sure the `TAKEN` lines of the block at `block` are parsed: one
/// marker parses, the others wait for it.
fn ensure_parsed(block: usize, m: *mut BlockMeta) {
    let s = parse_state(m);
    if s.load(Ordering::Acquire) == PARSED {
        return;
    }
    match s.compare_exchange(0, PARSING, Ordering::Acquire, Ordering::Acquire) {
        Ok(_) => {
            parse_taken(block, m);
            s.store(PARSED, Ordering::Release);
        }
        Err(_) => {
            while s.load(Ordering::Acquire) != PARSED {
                core::hint::spin_loop();
            }
        }
    }
}

/// `f(offset)` for every object start in the lines of `lines` that are
/// `TAKEN` (all lines if `all`), from the start bitmap `starts`.
pub(crate) fn for_each_start(
    lines: &[u8; LINES],
    starts: &[u64; GRANULES / 64],
    all: bool,
    mut f: impl FnMut(usize),
) {
    const PER_WORD: usize = 64 / LINE_GRANULES;
    for (w, &word) in starts.iter().enumerate() {
        let mut bits = word;
        if !all {
            for k in 0..PER_WORD {
                if lines[w * PER_WORD + k] != TAKEN {
                    bits &= !(((1u64 << LINE_GRANULES) - 1) << (k * LINE_GRANULES));
                }
            }
        }
        while bits != 0 {
            f((w * 64 + bits.trailing_zeros() as usize) * 8);
            bits &= bits - 1;
        }
    }
}

/// Before marking: the sorted LOS index and the block heap view lookups
/// use ([`Mark`]); new block lines are parsed lazily by [`object_in`].
/// With the world stopped and the mark lock held.
pub(crate) fn build_index() {
    let (h, m) = (heap(), mark());
    (m.base, m.nblocks, m.meta, m.reserved) = (h.base, h.nblocks, h.meta, h.reserved);
    m.los_count = h.los_count;
    if h.los_count == 0 {
        return;
    }
    if m.index_cap < h.los_count {
        let cap = h.los_count.next_power_of_two();
        // SAFETY: realloc of our own buffer (or null).
        let p = unsafe { libc::realloc(m.index.cast(), cap * size_of::<usize>()) };
        if p.is_null() {
            nabir_common::oom();
        }
        m.index = p.cast();
        m.index_cap = cap;
    }
    // SAFETY: the list holds exactly `los_count` objects; `index` has
    // room.
    unsafe {
        let mut o = h.los;
        for i in 0..h.los_count {
            *m.index.add(i) = o as usize;
            o = (*prefix(o)).next;
        }
        core::slice::from_raw_parts_mut(m.index, h.los_count).sort_unstable();
    }
}

/// The object whose header address is `word` or whose memory contains
/// it (a `str` points at its payload bytes, or into them after slicing).
/// `young`: block objects outside `TAKEN` lines (old ones) are `None`,
/// decided from the line table without touching the object. Free and
/// `BLACK` lines hold none (a live object covers only its own lines;
/// a concurrent mark never looks at what was allocated meanwhile).
/// `MOVED`: objects may be evacuated already. `!exact` (a precise
/// reference: inside its object, or one past its end): the nearest block
/// object start at or before `word`, its header left unread.
pub(crate) fn object_in<const MOVED: bool>(
    word: usize,
    young: bool,
    exact: bool,
) -> Option<*mut u8> {
    let size_of = |o: *mut u8| if MOVED { moved_size(o) } else { obj_size(o) };
    let m = view();
    if word >= m.base && word < m.base + m.nblocks * BLOCK_SIZE && !foreign(m, word) {
        let blk = (word - m.base) / BLOCK_SIZE;
        // SAFETY: `blk < nblocks`.
        let meta = unsafe { m.meta.add(blk) };
        let block = m.base + blk * BLOCK_SIZE;
        let state = line(meta, (word - block) / LINE_SIZE);
        if state == FREE || state == BLACK || (young && state != TAKEN) {
            return None;
        }
        // Only lookups write start bits, and only while the world is
        // stopped: `TAKEN` lines exist only then (a concurrent mark starts
        // right after a sweep and makes `BLACK` ones).
        if state == TAKEN {
            ensure_parsed(block, meta);
        }
        let granule = (word - block) / 8;
        // The nearest object start at or before `granule`: objects are
        // smaller than `LOS_THRESHOLD`, so only that many granules back.
        let lowest = granule.saturating_sub(LOS_THRESHOLD / 8);
        let mut i = granule / 64;
        let mut bits = start_word(meta, i) & (u64::MAX >> (63 - granule % 64));
        loop {
            if bits != 0 {
                let start = block + (i * 64 + 63 - bits.leading_zeros() as usize) * 8;
                if !exact {
                    return Some(start as *mut u8);
                }
                return (word < start + size_of(start as *mut u8)).then_some(start as *mut u8);
            }
            if i == 0 || (i - 1) * 64 + 63 < lowest {
                return None;
            }
            i -= 1;
            bits = start_word(meta, i);
        }
    }
    // No LOS object, or a block committed after the view (`nabi_flat`:
    // `malloc`'s memory lies among the blocks).
    if m.los_count == 0
        || (!cfg!(nabi_flat) && m.base != 0 && word >= m.base && word < m.base + m.reserved)
    {
        return None;
    }
    // SAFETY: `build_index` filled `los_count` entries.
    let index = unsafe { core::slice::from_raw_parts(m.index, m.los_count) };
    let i = match index.binary_search(&word) {
        Ok(i) => i,
        Err(0) => return None,
        Err(i) => i - 1,
    };
    let start = index[i];
    (word < start + size_of(start as *mut u8)).then_some(start as *mut u8)
}

/// Whether `word`, in one of the view's blocks, is in a [`FOREIGN`] one
/// (`malloc`'s, holding LOS objects).
#[cfg(nabi_flat)]
pub(crate) fn foreign(m: &Mark, word: usize) -> bool {
    // SAFETY: the block is below the view's `nblocks`.
    line(unsafe { m.meta.add((word - m.base) / BLOCK_SIZE) }, 0) == FOREIGN
}

#[cfg(not(nabi_flat))]
pub(crate) const fn foreign(_m: &Mark, _word: usize) -> bool {
    false
}
