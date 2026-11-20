//! Evacuation: candidates, copies and reference fixing.

#[allow(clippy::wildcard_imports)]
use crate::*;

/// After a full collection's sweep: choose the evacuation candidates
/// (opportunistic defragmentation) from the line states the sweep left:
/// when free lines are at least a quarter of the used blocks' lines,
/// every block at most half occupied - if together they hold an eighth
/// of the used lines free. `NABIR_GC_EVAC=always`: every used block.
/// Whether there is any.
// Occupancy counts lines, not live bytes (no Immix hole histogram). A
// `TAKEN` line counts as occupied: after a concurrent mark it holds
// objects allocated meanwhile, whose liveness nobody knows yet.
pub(crate) fn select_candidates() -> bool {
    let h = heap();
    if h.nblocks == 0 {
        return false;
    }
    // SAFETY: `nblocks` initialized metas.
    let metas = unsafe { core::slice::from_raw_parts_mut(h.meta, h.nblocks) };
    if h.evac_always {
        let mut any = false;
        for m in metas {
            m.evac = m.used;
            any |= m.used;
        }
        h.fresh_blocks = usize::MAX;
        return any;
    }
    let occupied = |m: &BlockMeta| LINES - free_lines(&m.lines);
    let (mut used_lines, mut free_lines, mut gain, mut moved) = (0, 0, 0, 0);
    for m in metas.iter().filter(|m| m.used) {
        let occ = occupied(m);
        used_lines += LINES;
        free_lines += LINES - occ;
        if occ <= LINES / 2 {
            gain += LINES - occ;
            moved += occ;
        }
    }
    if used_lines == 0 || free_lines * 4 < used_lines || gain * 8 < used_lines {
        return false;
    }
    for m in metas {
        m.evac = m.used && occupied(m) <= LINES / 2;
    }
    // At most half full each, the candidates fit in half as many blocks:
    // fresh ones may be taken when the holes and free blocks run out.
    h.fresh_blocks = moved.div_ceil(LINES) + 1;
    true
}

/// Copy every movable survivor out of the candidate blocks (leaving its
/// new address in the old header's `gc_word`, `FORWARDED`), rewrite
/// every precise reference to a moved object, then recompute the
/// candidates' lines. A survivor stays put if it is pinned, address-
/// taken (`NOMOVE`), reached through a root or an ambiguous word, or no
/// free line is left for it.
pub(crate) fn evacuate() {
    let h = heap();
    h.evacuations += 1;
    h.copy_cursor = 0;
    h.copy_limit = 0;
    h.copy_line = 0;
    h.copy_phase = 0;
    let (pinned, npinned) = (mark().pinned, mark().pinned_len);
    for i in 0..npinned {
        // SAFETY: `pinned_len` recorded live block objects.
        let o = unsafe { *pinned.add(i) };
        if in_candidate(o) {
            // SAFETY: as above.
            unsafe {
                (*header(o as *mut u8))
                    .gc_flags
                    .fetch_or(FLAG_NOEVAC, Ordering::Relaxed)
            };
        }
    }
    let (base, nblocks) = (h.base, h.nblocks);
    let mut moved = 0;
    for b in 0..nblocks {
        // SAFETY: `b < nblocks`; copied out, as `copy_alloc` may grow
        // (move) the meta table.
        let (evac, lines, starts) = unsafe {
            let m = &*heap().meta.add(b);
            (m.evac, m.lines, m.starts)
        };
        if !evac {
            continue;
        }
        let block = base + b * BLOCK_SIZE;
        for_each_start(&lines, &starts, true, |off| {
            let obj = (block + off) as *mut u8;
            // SAFETY: a survivor's header.
            let flags = unsafe { &(*header(obj)).gc_flags };
            let f = flags.fetch_and(!FLAG_NOEVAC, Ordering::Relaxed);
            if f & (FLAG_NOEVAC | FLAG_NOMOVE | FLAG_PINNED) != 0 {
                return;
            }
            let size = obj_size(obj);
            let Some(to) = copy_alloc(size) else {
                return;
            };
            // SAFETY: `to` is `size` free bytes outside every candidate.
            unsafe {
                ptr::copy_nonoverlapping(obj, to, size);
                (*header(obj)).gc_word = to as u64;
            }
            flags.fetch_or(FLAG_FORWARDED, Ordering::Relaxed);
            moved += size;
        });
    }
    if moved > 0 {
        fix_references();
    }
    for b in 0..nblocks {
        // SAFETY: `b < nblocks`; nothing else touches the table here.
        let m = unsafe { &mut *heap().meta.add(b) };
        if !m.evac {
            continue;
        }
        m.evac = false;
        let block = base + b * BLOCK_SIZE;
        let (lines, starts) = (m.lines, m.starts);
        m.lines = [FREE; LINES];
        for_each_start(&lines, &starts, true, |off| {
            let obj = (block + off) as *mut u8;
            // SAFETY: an object header (moved or not).
            let f = unsafe { (*header(obj)).gc_flags.load(Ordering::Relaxed) };
            if f & FLAG_FORWARDED != 0 {
                m.starts[off / 8 / 64] &= !(1 << (off / 8 % 64));
            } else {
                let end = (off + obj_size(obj)).div_ceil(LINE_SIZE);
                m.lines[off / LINE_SIZE..end].fill(LIVE);
            }
        });
        m.used = m.lines.contains(&LIVE);
    }
    heap().moved_bytes += moved as u64;
}

/// After the copies: rewrite every precise (or weak) reference word of
/// every survivor (copies included, old copies not) that points into a
/// moved object. Roots and ambiguous words only reach objects that
/// stayed; a weak word's referent is live (or the word was cleared).
// A second pass over the live heap: recording the slots that
// point into candidates during the mark needs the candidates before
// the mark, from stale line states (young objects refill the holes and
// look live, so sparse blocks are rarely seen).
#[allow(clippy::cast_ptr_alignment)] // reference words are 8-aligned
pub(crate) fn fix_references() {
    let fix = |o: *mut u8| {
        for_each_ref(o, 0, usize::MAX, |off, kind| {
            if kind == AMBIGUOUS {
                return;
            }
            // SAFETY: `off` is a reference word inside the live `o`.
            unsafe {
                let slot = o.add(off).cast::<usize>();
                let w = slot.read_unaligned();
                if !in_candidate(w) {
                    return;
                }
                if let Some(old) = object_in::<true>(w, false, true) {
                    slot.write_unaligned(forwarded(old as usize) + (w - old as usize));
                }
            }
        });
    };
    let h = heap();
    let (base, nblocks) = (h.base, h.nblocks);
    for b in 0..nblocks {
        // SAFETY: `b < nblocks`; `fix` only reads the table.
        let (used, lines, starts) = unsafe {
            let m = &*heap().meta.add(b);
            (m.used, m.lines, m.starts)
        };
        if !used {
            continue;
        }
        let block = base + b * BLOCK_SIZE;
        for_each_start(&lines, &starts, true, |off| {
            let obj = (block + off) as *mut u8;
            // SAFETY: a survivor's (or an old copy's) header.
            if unsafe { (*header(obj)).gc_flags.load(Ordering::Relaxed) } & FLAG_FORWARDED == 0 {
                fix(obj);
            }
        });
    }
    // SAFETY: walks the LOS list.
    unsafe {
        let mut o = heap().los;
        while !o.is_null() {
            fix(o);
            o = (*prefix(o)).next;
        }
    }
}

/// The new address of the object at `o` if it was evacuated, else `o`.
pub(crate) fn forwarded(o: usize) -> usize {
    // SAFETY: `o` is an object header.
    unsafe {
        let h = &*header(o as *mut u8);
        if h.gc_flags.load(Ordering::Relaxed) & FLAG_FORWARDED != 0 {
            h.gc_word as usize
        } else {
            o
        }
    }
}

/// Evacuation's bump allocator: `size` bytes in the free lines of a
/// block that is not a candidate (holes of used blocks first, then
/// free blocks, then fresh blocks within the budget). Its lines
/// become `LIVE` and its start bit is set. `None`: no room.
pub(crate) fn copy_alloc(size: usize) -> Option<*mut u8> {
    let h = heap();
    if size > h.copy_limit - h.copy_cursor && !next_copy_run(size.div_ceil(LINE_SIZE)) {
        return None;
    }
    let h = heap();
    let to = h.copy_cursor;
    h.copy_cursor += size;
    let b = (to - h.base) / BLOCK_SIZE;
    let off = to - h.base - b * BLOCK_SIZE;
    // SAFETY: `b < nblocks`.
    let m = unsafe { &mut *h.meta.add(b) };
    m.lines[off / LINE_SIZE..(off + size).div_ceil(LINE_SIZE)].fill(LIVE);
    m.starts[off / 8 / 64] |= 1 << (off / 8 % 64);
    m.used = true;
    m.released = false;
    Some(to as *mut u8)
}

/// Point the copy allocator at the next run of at least `lines` free
/// lines; false when there is none (and no fresh block may be taken).
pub(crate) fn next_copy_run(lines: usize) -> bool {
    let h = heap();
    while h.copy_phase < 2 {
        let total = h.nblocks * LINES;
        while h.copy_line < total {
            let (b, l) = (h.copy_line / LINES, h.copy_line % LINES);
            // SAFETY: `b < nblocks`.
            let m = unsafe { &*h.meta.add(b) };
            if m.evac || m.used != (h.copy_phase == 0) {
                h.copy_line = (b + 1) * LINES;
                continue;
            }
            let mut end = l;
            while end < LINES && m.lines[end] == FREE {
                end += 1;
            }
            h.copy_line = b * LINES + end.max(l + 1);
            if end - l >= lines {
                h.copy_cursor = h.base + b * BLOCK_SIZE + l * LINE_SIZE;
                h.copy_limit = h.base + b * BLOCK_SIZE + end * LINE_SIZE;
                return true;
            }
        }
        h.copy_phase += 1;
        h.copy_line = 0;
    }
    if h.fresh_blocks == 0 {
        return false;
    }
    h.fresh_blocks -= 1;
    let b = new_block();
    let h = heap();
    h.copy_cursor = h.base + b * BLOCK_SIZE;
    h.copy_limit = h.copy_cursor + BLOCK_SIZE;
    true
}
