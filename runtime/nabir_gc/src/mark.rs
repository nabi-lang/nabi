//! Marking: the mark stack and reference scanning.

#[allow(clippy::wildcard_imports)]
use crate::*;

/// The word at `p` (8-aligned): an atomic load, as a concurrent mark
/// reads what mutators store.
pub(crate) fn load_word(p: *const u8) -> usize {
    // SAFETY: callers pass an aligned word inside a live object.
    #[allow(clippy::cast_ptr_alignment)]
    unsafe { core::sync::atomic::AtomicUsize::from_ptr(p.cast::<usize>().cast_mut()) }
        .load(Ordering::Relaxed)
}

/// The `u32` at `p` (4-aligned), like [`load_word`].
pub(crate) fn load_u32(p: *const u8) -> u32 {
    // SAFETY: as in `load_word`.
    #[allow(clippy::cast_ptr_alignment)]
    unsafe { core::sync::atomic::AtomicU32::from_ptr(p.cast::<u32>().cast_mut()) }
        .load(Ordering::Relaxed)
}

/// Where marking puts what it finds: the mark stack and lists of the
/// thread that marks ([`Main`], or a parallel worker's own).
pub(crate) trait Out {
    /// Queue `o` for marking.
    fn obj(&mut self, o: *mut u8);
    /// `o`, a block object a root or an ambiguous word reaches, stays put.
    fn pinned(&mut self, o: usize);
    /// The `WEAK_REF` word at `at` was scanned.
    fn weak(&mut self, at: usize);
    /// The next queued object.
    fn pop(&mut self) -> Option<*mut u8>;
    /// `o` (of `size` bytes) was marked.
    fn marked(&mut self, size: usize);
}

/// The collector's own stack and lists, in [`Mark`].
pub(crate) struct Main;

impl Out for Main {
    fn obj(&mut self, o: *mut u8) {
        push_obj(o);
    }

    fn pinned(&mut self, o: usize) {
        let m = mark();
        append(&mut m.pinned, &mut m.pinned_len, &mut m.pinned_cap, o);
    }

    fn weak(&mut self, at: usize) {
        let m = mark();
        append(&mut m.weak, &mut m.weak_len, &mut m.weak_cap, at);
    }

    fn pop(&mut self) -> Option<*mut u8> {
        let m = mark();
        if m.len == 0 {
            return None;
        }
        m.len -= 1;
        // SAFETY: `push_obj` only queues addresses of live GC objects.
        Some(unsafe { *m.stack.add(m.len) })
    }

    fn marked(&mut self, size: usize) {
        let m = mark();
        m.marked += 1;
        m.marked_bytes += size;
    }
}

/// Queue the object `word` points into, if any (and young, unless the
/// mark is full). `pin`: `word` is a root or ambiguous, so the object
/// must not move (and the lookup is exact); else it is a precise
/// reference.
pub(crate) fn push(word: usize, pin: bool) {
    push_to(&mut Main, word, pin);
}

pub(crate) fn push_to(out: &mut impl Out, word: usize, pin: bool) {
    let Some(obj) = object_in::<false>(word, !view().full, pin) else {
        return;
    };
    let v = view();
    if pin && v.evacuating && (obj as usize) < v.base + v.nblocks * BLOCK_SIZE {
        out.pinned(obj as usize);
    }
    // Marked or not is decided by `drain`, once the header is in cache.
    out.obj(obj);
}

/// [`push`] of a precise reference to an object's start (a `GcRef`
/// word, a boxed `any` payload). A minor collection takes a young
/// object in an unparsed block as it is: it sets the object's start bit
/// instead of parsing the block, so the sweep visits survivors only.
pub(crate) fn push_start(out: &mut impl Out, word: usize) {
    let m = view();
    if m.full
        || word % 8 != 0
        || word < m.base
        || word >= m.base + m.nblocks * BLOCK_SIZE
        || foreign(m, word)
    {
        return push_to(out, word, false);
    }
    let blk = (word - m.base) / BLOCK_SIZE;
    // SAFETY: `blk < nblocks`.
    let meta = unsafe { m.meta.add(blk) };
    let off = word - (m.base + blk * BLOCK_SIZE);
    if line(meta, off / LINE_SIZE) != TAKEN {
        return;
    }
    if parse_state(meta).load(Ordering::Acquire) != 0 {
        return push_to(out, word, false);
    }
    set_start(meta, off);
    // SAFETY: a field of the block's metadata; parallel markers only
    // ever set it.
    unsafe { core::sync::atomic::AtomicBool::from_ptr(&raw mut (*meta).seeded) }
        .store(true, Ordering::Relaxed);
    out.obj(word as *mut u8);
}

/// Append `v` to the `realloc`-grown list `list[..len]`.
pub(crate) fn append(list: &mut *mut usize, len: &mut usize, cap: &mut usize, v: usize) {
    if *len == *cap {
        *cap = (*cap * 2).max(1024);
        // SAFETY: realloc of our own buffer (or null).
        let p = unsafe { libc::realloc(list.cast(), *cap * size_of::<usize>()) };
        if p.is_null() {
            nabir_common::oom();
        }
        *list = p.cast();
    }
    // SAFETY: `len < cap`.
    unsafe { *list.add(*len) = v };
    *len += 1;
}

/// The word at offset `off` of the scanned object `o`, of kind `kind`
/// ([`for_each_ref`]): queue its referent, or record a weak word.
pub(crate) fn visit_word(out: &mut impl Out, o: *mut u8, off: usize, kind: u32) {
    // SAFETY: `off` is a word inside the live object `o`.
    let at = unsafe { o.add(off) };
    if kind == WEAK_REF {
        out.weak(at as usize);
    } else if kind == STRONG_REF {
        push_start(out, load_word(at));
    } else {
        push_to(out, load_word(at), kind == AMBIGUOUS);
    }
}

/// After marking, before the sweep: clear every recorded weak word whose
/// referent this collection frees (unmarked; a minor collection frees
/// young objects only). One it does not look at (allocated during a
/// concurrent mark) stays.
pub(crate) fn clear_dead_weak(full: bool) {
    let m = mark();
    if m.weak_len == 0 {
        return;
    }
    // SAFETY: `weak_len` recorded words inside marked (live) objects.
    let words = unsafe { core::slice::from_raw_parts(m.weak, m.weak_len) };
    for &at in words {
        let w = load_word(at as *const u8);
        if w == 0 {
            continue;
        }
        let Some(obj) = object_in::<false>(w, false, true) else {
            continue;
        };
        // SAFETY: an object header.
        let f = unsafe { (*header(obj)).gc_flags.load(Ordering::Relaxed) };
        if f & FLAG_MARK == 0 && (full || f & FLAG_STICKY == 0) {
            // SAFETY: as above; the world is stopped.
            unsafe { (at as *mut usize).write(0) };
        }
    }
    mark().weak_len = 0;
}

/// Start loading the cache line at `p` (a hint: never faults).
#[inline]
#[allow(clippy::missing_const_for_fn)] // const only where it is a no-op
pub(crate) fn prefetch(p: *const u8) {
    // SAFETY (each): a prefetch does not access memory architecturally.
    #[cfg(target_arch = "aarch64")]
    unsafe {
        core::arch::asm!("prfm pldl1keep, [{0}]", in(reg) p, options(nostack, readonly, preserves_flags));
    }
    #[cfg(target_arch = "x86_64")]
    unsafe {
        core::arch::x86_64::_mm_prefetch(p.cast(), core::arch::x86_64::_MM_HINT_T0);
    }
    #[cfg(target_arch = "x86")]
    unsafe {
        core::arch::x86::_mm_prefetch(p.cast(), core::arch::x86::_MM_HINT_T0);
    }
    #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64", target_arch = "x86")))]
    let _ = p;
}

/// `addr` lies in an evacuation candidate block.
pub(crate) fn in_candidate(addr: usize) -> bool {
    let h = heap();
    // SAFETY: bounds-checked block index.
    addr >= h.base
        && addr < h.base + h.nblocks * BLOCK_SIZE
        && unsafe { (*h.meta.add((addr - h.base) / BLOCK_SIZE)).evac }
}

pub(crate) fn push_obj(obj: *mut u8) {
    let m = mark();
    if m.len == m.stack_cap {
        let cap = (m.stack_cap * 2).max(1024);
        // SAFETY: realloc of our own stack (or null).
        let p = unsafe { libc::realloc(m.stack.cast(), cap * size_of::<*mut u8>()) };
        if p.is_null() {
            nabir_common::oom();
        }
        m.stack = p.cast();
        m.stack_cap = cap;
    }
    // SAFETY: `len < stack_cap`.
    unsafe { *m.stack.add(m.len) = obj };
    m.len += 1;
}

/// Objects popped from the mark stack and prefetched before they are
/// scanned: the header of each is loading while the ones ahead of it
/// are scanned.
pub(crate) const PREFETCH: usize = 8;

/// Mark the queued objects until none is left (`batch`: at most that
/// many). A minor collection never enters an old object: whatever young
/// object it references is in the remembered set's reach.
pub(crate) fn drain(batch: usize) {
    drain_into(&mut Main, batch);
}

/// [`drain`] of the stack behind `out`; with a stop-the-world mark of
/// many objects, parallel workers help ([`crate::parallel`]).
pub(crate) fn drain_into(out: &mut impl Out, mut batch: usize) {
    let mut ring = [ptr::null_mut::<u8>(); PREFETCH];
    let (mut head, mut count) = (0, 0);
    let (full, concurrent) = (view().full, view().concurrent);
    while batch > 0 {
        while count < PREFETCH {
            let Some(o) = out.pop() else {
                break;
            };
            prefetch(o);
            ring[(head + count) % PREFETCH] = o;
            count += 1;
        }
        if count == 0 {
            return;
        }
        let o = ring[head];
        head = (head + 1) % PREFETCH;
        count -= 1;
        batch -= 1;
        // SAFETY: only live GC objects are queued.
        let flags = unsafe { &(*header(o)).gc_flags };
        if (!full && flags.load(Ordering::Relaxed) & FLAG_STICKY != 0)
            || flags.load(Ordering::Relaxed) & FLAG_MARK != 0
            || flags.fetch_or(FLAG_MARK, Ordering::Relaxed) & FLAG_MARK != 0
        {
            continue;
        }
        out.marked(obj_size(o));
        scan_children(out, o);
        if concurrent {
            // The first store into `o` from now on need not log what it
            // held (`write_barrier_slow`).
            flags.fetch_or(FLAG_SCANNED, Ordering::Release);
        }
    }
    // Out of budget: what was popped goes back.
    for k in 0..count {
        out.obj(ring[(head + k) % PREFETCH]);
    }
}

/// Queue the referents of `o`'s reference words.
// The mark loop's hot path: a plain (non-array) table read inline, the
// general `for_each_ref` otherwise.
#[allow(clippy::cast_ptr_alignment)] // tables are 4-aligned, payloads 8
pub(crate) fn scan_children(out: &mut impl Out, o: *mut u8) {
    crate::modules::seen(o);
    // SAFETY: `o` is a live GC object; its ScanTable describes it.
    unsafe {
        let type_id = (*header(o)).type_id;
        if type_id == SCAN_WORDS_TYPE_ID {
            scan_range(out, o, 0, usize::MAX);
            return;
        }
        let t = scan_table(type_id);
        if t.is_null() {
            return;
        }
        // `version | n << 16`, one word in the target's byte order.
        let head = t.add(4).cast::<u32>().read_unaligned();
        let n = (head >> 16) as usize;
        if head as u16 & TABLE_ARRAY != 0 {
            // A buffer of plain references (`List<str>`, a list of
            // handles): one word per entry and element.
            let entries = core::slice::from_raw_parts(t.add(8).cast::<[u32; 2]>(), n);
            if !entries
                .iter()
                .all(|&[_, kind]| matches!(kind & KIND_MASK, STRONG_REF | INTERIOR_REF))
            {
                scan_range(out, o, 0, usize::MAX);
                return;
            }
            let stride = t.cast::<u32>().read_unaligned() as usize;
            if n == 0 || stride == 0 {
                return;
            }
            let end = o.add(obj_size(o));
            let mut e = o.add(HEADER_SIZE);
            while e.add(stride) <= end {
                for &[eo, kind] in entries {
                    let word = load_word(e.add(eo as usize));
                    if kind & KIND_MASK == STRONG_REF {
                        push_start(out, word);
                    } else {
                        push_to(out, word, false);
                    }
                }
                e = e.add(stride);
            }
            return;
        }
        for e in 0..n {
            let entry = t.add(8 + 8 * e).cast::<u32>();
            let off = entry.read_unaligned() as usize;
            let kind = entry.add(1).read_unaligned();
            let precise = match kind & KIND_MASK {
                STRONG_REF => {
                    push_start(out, load_word(o.add(off)));
                    continue;
                }
                INTERIOR_REF => true,
                ANY_SLOT => {
                    if load_u32(o.add(off)) & ANY_BOXED == 0 {
                        continue;
                    }
                    push_start(out, load_word(o.add(off + size_of::<usize>())));
                    continue;
                }
                SUM_PAYLOAD => {
                    visit_sum(o, t, kind, off, &mut |off, k| visit_word(out, o, off, k));
                    continue;
                }
                AMBIGUOUS => false,
                WEAK_REF => {
                    visit_word(out, o, off, WEAK_REF);
                    continue;
                }
                _ => continue,
            };
            push_to(out, load_word(o.add(off)), !precise);
        }
    }
}

/// Queue the referents of `o`'s reference words at byte offsets
/// `lo..hi` (from the header).
pub(crate) fn scan_range(out: &mut impl Out, o: *mut u8, lo: usize, hi: usize) {
    for_each_ref(o, lo, hi, |off, kind| visit_word(out, o, off, kind));
}

/// `f(offset, kind)` for every word of the live object `o` at byte
/// offsets `lo..hi` (from the header) that may hold a reference, per
/// its `ScanTable`. `kind`: `STRONG_REF` (an object's start or an
/// address outside the heap), `INTERIOR_REF` (a pointer into or one past
/// an object), `AMBIGUOUS` (anything) or `WEAK_REF` (a reference or 0,
/// not to be followed). An `ANY_SLOT` yields its payload word, as a
/// `STRONG_REF`, only when the tag says it is a box. An object whose type was
/// never registered has none.
#[allow(clippy::cast_ptr_alignment)] // tables are 4-aligned, payloads 8
pub(crate) fn for_each_ref(o: *mut u8, lo: usize, hi: usize, mut f: impl FnMut(usize, u32)) {
    // SAFETY: `o` is a live GC object; its ScanTable describes it.
    unsafe {
        let hd = &*header(o);
        if hd.type_id == SCAN_WORDS_TYPE_ID {
            // Pointer-size words: 4 bytes on 32-bit targets.
            const W: usize = size_of::<usize>();
            let hi = hi.min(obj_size(o)) & !(W - 1);
            let mut off = lo.max(HEADER_SIZE).next_multiple_of(W);
            while off < hi {
                f(off, AMBIGUOUS);
                off += W;
            }
            return;
        }
        let t = scan_table(hd.type_id);
        if t.is_null() {
            return;
        }
        let head = t.add(4).cast::<u32>().read_unaligned();
        let (version, n) = (head as u16, (head >> 16) as usize);
        if n == 0 {
            return;
        }
        let entries = core::slice::from_raw_parts(t.add(8).cast::<[u32; 2]>(), n);
        if version & TABLE_ARRAY == 0 {
            if lo == 0 && hi == usize::MAX {
                visit_entries(o, t, entries, 0, &mut f);
            } else {
                visit_entries(o, t, entries, 0, &mut |off, p| {
                    if (lo..hi).contains(&off) {
                        f(off, p);
                    }
                });
            }
            return;
        }
        // An array table repeats its entries every `stride` bytes after
        // the header: visit the elements that overlap `lo..hi` (an `any`
        // counts by its payload word).
        let stride = t.cast::<u32>().read_unaligned() as usize;
        let count = (obj_size(o) - HEADER_SIZE) / stride;
        if lo == 0 && hi == usize::MAX {
            for i in 0..count {
                visit_entries(o, t, entries, HEADER_SIZE + i * stride, &mut f);
            }
            return;
        }
        let first = lo.saturating_sub(HEADER_SIZE + size_of::<usize>()) / stride;
        let last = hi.saturating_sub(HEADER_SIZE).div_ceil(stride).min(count);
        for i in first..last {
            visit_entries(o, t, entries, HEADER_SIZE + i * stride, &mut |off, p| {
                if (lo..hi).contains(&off) {
                    f(off, p);
                }
            });
        }
    }
}

/// Whether every reference slot of GC object `obj` is null (a
/// coroutine frame that holds nothing to copy).
///
/// # Safety
/// `obj` must be a live GC object.
pub unsafe fn refs_all_null(obj: *mut u8) -> bool {
    let mut none = true;
    for_each_ref(obj, 0, usize::MAX, |off, _| {
        // SAFETY: a reference slot inside the live `obj`.
        if none && load_word(unsafe { obj.add(off) }) != 0 {
            none = false;
        }
    });
    none
}

/// [`for_each_ref`] over the `entries` of table `t` at byte offset `at`.
#[inline]
#[allow(clippy::cast_ptr_alignment)] // the tag is 4-aligned
pub(crate) fn visit_entries(
    o: *mut u8,
    t: *const u8,
    entries: &[[u32; 2]],
    at: usize,
    f: &mut impl FnMut(usize, u32),
) {
    for &[eo, kind] in entries {
        let off = at + eo as usize;
        match kind & KIND_MASK {
            // SAFETY: the tag word of an `any` inside the live `o`.
            ANY_SLOT if load_u32(unsafe { o.add(off) }) & ANY_BOXED != 0 => {
                f(off + size_of::<usize>(), STRONG_REF);
            }
            SUM_PAYLOAD => visit_sum(o, t, kind, off, f),
            k @ (STRONG_REF | INTERIOR_REF | AMBIGUOUS | WEAK_REF) => f(off, k),
            _ => {}
        }
    }
}

/// The `SUM_PAYLOAD` entry `kind` of table `t` whose tag is at byte
/// offset `at` of `o`: visit the entries of the variant the tag selects.
#[allow(clippy::cast_ptr_alignment)] // tables and tags are 4-aligned
pub(crate) fn visit_sum(
    o: *mut u8,
    t: *const u8,
    kind: u32,
    at: usize,
    f: &mut impl FnMut(usize, u32),
) {
    // SAFETY: the tag of a sum inside the live `o`; the descriptor and
    // its variants' entries lie inside the table (`gc_abi`).
    unsafe {
        let tag = load_u32(o.add(at));
        let t32 = t.cast::<u32>();
        let desc = t32.add((kind >> SUM_DESC_SHIFT) as usize);
        for v in 0..desc.read() as usize {
            let head = desc.add(1 + 3 * v);
            if head.read() == tag {
                let entries = core::slice::from_raw_parts(
                    t32.add(head.add(2).read() as usize).cast::<[u32; 2]>(),
                    head.add(1).read() as usize,
                );
                visit_entries(o, t, entries, at, f);
                return;
            }
        }
    }
}
