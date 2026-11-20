//! Migration: a full collection whose last phase rewrites every object
//! of a changed type into its new layout (hot reload).
//!
//! The caller's [`Plan`] says which type ids change and how a value is
//! rewritten. An object is rewritten in place when the new layout fits
//! in it, else copied into a new object (forwarded like an evacuated
//! one) and every heap reference to it fixed. Root words are never
//! rewritten: a rooted, pinned or address-taken object that must move
//! blocks the migration, as does an ambiguous word into the middle of
//! one. A dry pass finds every blocker first, so a blocked migration is
//! an ordinary full collection that changes nothing.

use crate::evacuate::{copy_alloc, forwarded};
#[allow(clippy::wildcard_imports)]
use crate::*;
use nabir_common::wtf8::MBuf;

/// How the objects of one type id migrate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rule {
    /// The new type id.
    pub to: u32,
    /// Bytes of one value before and after; a buffer (`elems`) repeats
    /// them over its payload.
    pub old: usize,
    pub new: usize,
    pub elems: bool,
}

impl Rule {
    /// Values in an object of `size` bytes (header included).
    const fn count(&self, size: usize) -> usize {
        if !self.elems {
            1
        } else if self.old == 0 {
            0
        } else {
            (size - HEADER_SIZE) / self.old
        }
    }

    /// Bytes the rewritten object needs, header included.
    const fn need(&self, size: usize) -> usize {
        HEADER_SIZE + self.count(size) * self.new
    }
}

/// What stops a migration now; the type id of the object concerned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Block {
    /// Pinned or address-taken, and its new layout does not fit.
    Pinned(u32),
    /// A root holds it, and its new layout does not fit.
    Rooted(u32),
    /// An ambiguous word points into it (not at its start) and it moves.
    Ambiguous(u32),
    /// A reference points into it where no value of the new layout is.
    Interior(u32),
    /// An `any` holds a value of this tag inline.
    InlineAny(u32),
    /// A no-collect region is open.
    Region,
}

/// The caller's side of a migration. Every method but [`Plan::check`]
/// runs with the world stopped inside the collection's pause: it may
/// not allocate GC memory or reach a safepoint.
pub trait Plan {
    /// The rule for objects of `type_id`, if they migrate.
    fn rule(&self, type_id: u32) -> Option<Rule>;
    /// Whether the change can go on (frames, closures): checked after the
    /// mark, before the heap is looked at.
    fn check(&mut self) -> bool;
    /// A pass over the objects starts: `false` the dry one, `true` the
    /// one that rewrites.
    fn begin(&mut self, commit: bool);
    /// After a dry pass that found nothing: go on and rewrite (else
    /// [`run`] reports the objects it would rewrite and stops).
    fn rewrite(&self) -> bool;
    /// Dry pass: an object of `type_id` holding `count` values at
    /// `payload`, the objects of each id in a fixed order; `false` when
    /// it cannot migrate (the plan keeps why).
    fn admit(&mut self, type_id: u32, payload: *const u8, count: usize) -> bool;
    /// Rewrite `count` values from `from` (old layout) into `to` (new
    /// layout, zeroed), in the order [`Plan::admit`] saw them.
    fn copy(&mut self, type_id: u32, from: *const u8, to: *mut u8, count: usize);
    /// Dry pass, every live object's type id: `false` blocks (a closure
    /// or coroutine of old code that holds a changed type).
    fn object(&mut self, type_id: u32) -> bool;
    /// Whether an `any` tag stored inline names a changed type.
    fn inline_tag(&self, tag: u32) -> bool;
    /// `[lo, hi)` holding every code address [`Plan::code`] asks about.
    fn code_range(&self) -> (usize, usize);
    /// A heap word inside [`Plan::code_range`]: `false` blocks (a
    /// function value of old code that depends on a changed type).
    fn code(&mut self, word: usize) -> bool;
    /// Root words the plan rewrites itself after the move (its own
    /// modules' statics), into `f`.
    fn statics(&mut self, f: &mut dyn FnMut(*mut usize));
    /// A blocker the heap passes found.
    fn blocked(&mut self, why: Block);
    /// After every object is rewritten and every reference fixed.
    fn commit(&mut self, objects: usize, bytes: usize);
}

/// What [`run`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Rewritten: objects, bytes copied.
    Done(usize, usize),
    /// Nothing changed; the plan was told why.
    Blocked,
}

/// Whether some thread is inside a no-collect region (a hot swap waits
/// for it to end).
pub fn in_region() -> bool {
    world::lock();
    let r = heap().no_collect != 0;
    world::unlock();
    r
}

/// Run a full stop-the-world collection, then migrate per `plan`.
pub fn run(plan: &mut dyn Plan) -> Outcome {
    world::lock();
    if heap().no_collect != 0 {
        world::unlock();
        plan.blocked(Block::Region);
        return Outcome::Blocked;
    }
    configure(heap());
    if heap().marking {
        concurrent::finish(false);
    }
    adopt_sweep(heap());
    let p = Pause::begin();
    mark_sweep(true);
    build_index();
    let r = migrate(plan);
    p.end(Kind::Full);
    world::unlock();
    r
}

fn migrate(plan: &mut dyn Plan) -> Outcome {
    if !plan.check() {
        return Outcome::Blocked;
    }
    let rooted = rooted(plan);
    plan.begin(false);
    let (mut ok, mut count) = (true, 0);
    each_object(heap().nblocks, heap().los, |o| {
        ok &= dry(plan, o, &rooted);
        count += usize::from(rule_of(plan, o).is_some());
    });
    if !ok {
        return Outcome::Blocked;
    }
    if !plan.rewrite() {
        return Outcome::Done(count, 0);
    }
    plan.begin(true);
    let h = heap();
    (h.copy_cursor, h.copy_limit, h.copy_line, h.copy_phase) = (0, 0, 0, 0);
    h.fresh_blocks = usize::MAX;
    // Buffers rewritten in place whose values changed size, with their
    // rule (their type id is the new one now).
    let mut inplace: MBuf<(usize, Rule)> = MBuf::new();
    let mut moved: MBuf<usize> = MBuf::new();
    let (mut objects, mut bytes) = (0, 0);
    let mut scratch = Scratch::new();
    each_object(heap().nblocks, heap().los, |o| {
        let Some(rule) = rule_of(plan, o) else { return };
        let size = obj_size(o);
        let need = rule.need(size);
        let count = rule.count(size);
        objects += 1;
        if need <= size {
            let from = scratch.take(size - HEADER_SIZE);
            // SAFETY: `o` is a live object of `size` bytes; `from` holds
            // as many.
            unsafe {
                ptr::copy_nonoverlapping(o.add(HEADER_SIZE), from, size - HEADER_SIZE);
                ptr::write_bytes(o.add(HEADER_SIZE), 0, size - HEADER_SIZE);
                plan.copy((*header(o)).type_id, from, o.add(HEADER_SIZE), count);
                (*header(o)).type_id = rule.to;
            }
            if rule.elems && rule.old != rule.new {
                inplace.push((o as usize, rule));
            }
            bytes += size;
        } else {
            let to = new_object(need, rule.to);
            // SAFETY: `to` is a fresh zeroed object of at least `need`
            // bytes; `o` stays intact until the next collection.
            unsafe {
                plan.copy(
                    (*header(o)).type_id,
                    o.add(HEADER_SIZE),
                    to.add(HEADER_SIZE),
                    count,
                );
                (*header(o)).gc_word = to as u64;
                (*header(o))
                    .gc_flags
                    .fetch_or(FLAG_FORWARDED, Ordering::Relaxed);
            }
            moved.push(o as usize);
            bytes += need;
        }
    });
    inplace.as_mut_slice().sort_unstable_by_key(|e| e.0);
    fix(plan, inplace.as_slice());
    release(&moved);
    heap().moved_bytes += bytes as u64;
    plan.commit(objects, bytes);
    Outcome::Done(objects, bytes)
}

/// The rule of live object `o`, by its type id.
fn rule_of(plan: &dyn Plan, o: *mut u8) -> Option<Rule> {
    // SAFETY: a live object's header.
    plan.rule(unsafe { (*header(o)).type_id })
}

/// The migrating objects some root reaches, sorted; the plan's own
/// static words are not counted (it rewrites them).
fn rooted(plan: &mut dyn Plan) -> MBuf<usize> {
    let mut saved: MBuf<(usize, usize)> = MBuf::new();
    plan.statics(&mut |w| {
        // SAFETY: a word the plan owns; the world is stopped.
        unsafe {
            saved.push((w as usize, *w));
            *w = 0;
        }
    });
    let mut out: MBuf<usize> = MBuf::new();
    world::for_each_root(|r| {
        if let Some(o) = object_in::<false>(r as usize, false, true) {
            if rule_of(plan, o).is_some() {
                out.push(o as usize);
            }
        }
    });
    for &(w, v) in saved.as_slice() {
        // SAFETY: as above.
        unsafe { *(w as *mut usize) = v };
    }
    out.as_mut_slice().sort_unstable();
    out
}

/// `f` on every live object: the first `nblocks` blocks, then the LOS
/// list from `los`. Objects made meanwhile are not visited.
fn each_object(nblocks: usize, los: *mut u8, mut f: impl FnMut(*mut u8)) {
    let base = heap().base;
    for b in 0..nblocks {
        // SAFETY: `b < nblocks`; copied out, as `f` may take fresh
        // blocks (and set start bits in this one).
        let (used, lines, starts) = unsafe {
            let m = &*heap().meta.add(b);
            (m.used, m.lines, m.starts)
        };
        if !used {
            continue;
        }
        let block = base + b * BLOCK_SIZE;
        for_each_start(&lines, &starts, true, |off| {
            let o = (block + off) as *mut u8;
            // SAFETY: an object header.
            if unsafe { (*header(o)).gc_flags.load(Ordering::Relaxed) } & FLAG_FORWARDED == 0 {
                f(o);
            }
        });
    }
    let mut o = los;
    while !o.is_null() {
        // SAFETY: a LOS object's prefix; `f` may only prepend to the list.
        let next = unsafe { (*prefix(o)).next };
        f(o);
        o = next;
    }
}

/// The dry pass over live object `o`: whether nothing about it blocks.
fn dry(plan: &mut dyn Plan, o: *mut u8, rooted: &MBuf<usize>) -> bool {
    let mut ok = true;
    // SAFETY: a live object's header.
    let (tid, flags) = unsafe {
        let h = &*header(o);
        (h.type_id, h.gc_flags.load(Ordering::Relaxed))
    };
    let size = obj_size(o);
    ok &= plan.object(tid);
    if let Some(rule) = plan.rule(tid) {
        if rule.need(size) > size {
            if flags & (FLAG_PINNED | FLAG_NOMOVE) != 0 {
                plan.blocked(Block::Pinned(tid));
                ok = false;
            } else if rooted.as_slice().binary_search(&(o as usize)).is_ok() {
                plan.blocked(Block::Rooted(tid));
                ok = false;
            }
        }
        // SAFETY: the object's payload.
        ok &= plan.admit(tid, unsafe { o.add(HEADER_SIZE) }, rule.count(size));
    }
    for_each_ref(o, 0, usize::MAX, |off, kind| {
        if !matches!(kind, AMBIGUOUS | INTERIOR_REF) {
            return;
        }
        // SAFETY: a reference word inside the live `o`.
        let w = load_word(unsafe { o.add(off) });
        let Some(t) = object_in::<false>(w, false, true) else {
            return;
        };
        let Some(rule) = rule_of(plan, t) else { return };
        let t = t as usize;
        // SAFETY: a live object's header.
        let ttid = unsafe { (*header(t as *mut u8)).type_id };
        let moves = rule.need(obj_size(t as *mut u8)) > obj_size(t as *mut u8);
        if kind == AMBIGUOUS && moves && w != t {
            plan.blocked(Block::Ambiguous(ttid));
            ok = false;
        } else if kind == INTERIOR_REF && remap(&rule, t, w).is_none() {
            plan.blocked(Block::Interior(ttid));
            ok = false;
        }
    });
    each_any_tag(o, |tag| {
        if tag & ANY_BOXED == 0 && plan.inline_tag(tag) {
            plan.blocked(Block::InlineAny(tag));
            ok = false;
        }
    });
    let (lo, hi) = plan.code_range();
    if lo < hi {
        let mut at = HEADER_SIZE;
        while at + size_of::<usize>() <= size {
            // SAFETY: a word inside the live `o`.
            let w = unsafe { o.add(at).cast::<usize>().read_unaligned() };
            if w >= lo && w < hi && !plan.code(w) {
                ok = false;
            }
            at += size_of::<usize>();
        }
    }
    ok
}

/// Where a reference `w` into migrating object `t` (rule `rule`) points
/// after the rewrite, relative to the object's start: the header, the
/// payload start, or the start of a buffer's value (scaled); `None`
/// for anywhere else.
const fn remap(rule: &Rule, t: usize, w: usize) -> Option<usize> {
    let off = w - t;
    if off == 0 || off == HEADER_SIZE {
        return Some(off);
    }
    if !rule.elems || off < HEADER_SIZE || rule.old == 0 {
        return None;
    }
    let k = off - HEADER_SIZE;
    if k % rule.old != 0 {
        return None;
    }
    Some(HEADER_SIZE + k / rule.old * rule.new)
}

/// `f` on the tag of every `any` slot of live object `o`.
#[allow(clippy::cast_ptr_alignment)] // tables are 4-aligned
fn each_any_tag(o: *mut u8, mut f: impl FnMut(u32)) {
    // SAFETY: `o` is a live object; its ScanTable describes it.
    unsafe {
        let tid = (*header(o)).type_id;
        if tid == SCAN_WORDS_TYPE_ID {
            return;
        }
        let t = scan_table(tid);
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
            tags_at(o, t, entries, 0, &mut f);
            return;
        }
        let stride = t.cast::<u32>().read_unaligned() as usize;
        for i in 0..(obj_size(o) - HEADER_SIZE) / stride.max(1) {
            tags_at(o, t, entries, HEADER_SIZE + i * stride, &mut f);
        }
    }
}

/// [`each_any_tag`] over `entries` of table `t` at byte `at` of `o`,
/// through the variant each sum's tag selects.
#[allow(clippy::cast_ptr_alignment)] // tables and tags are 4-aligned
fn tags_at(o: *mut u8, t: *const u8, entries: &[[u32; 2]], at: usize, f: &mut impl FnMut(u32)) {
    for &[eo, kind] in entries {
        let off = at + eo as usize;
        // SAFETY: as in `visit_entries` / `visit_sum`.
        unsafe {
            match kind & KIND_MASK {
                ANY_SLOT => f(load_u32(o.add(off))),
                SUM_PAYLOAD => {
                    let tag = load_u32(o.add(off));
                    let t32 = t.cast::<u32>();
                    let desc = t32.add((kind >> SUM_DESC_SHIFT) as usize);
                    for v in 0..desc.read() as usize {
                        let head = desc.add(1 + 3 * v);
                        if head.read() == tag {
                            let inner = core::slice::from_raw_parts(
                                t32.add(head.add(2).read() as usize).cast::<[u32; 2]>(),
                                head.add(1).read() as usize,
                            );
                            tags_at(o, t, inner, off, f);
                            break;
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

/// A zeroed object of `need` bytes (header included) and type `to`,
/// old already (it survives a full collection), with the world stopped.
fn new_object(need: usize, to: u32) -> *mut u8 {
    let size8 = need
        .max(nabir_common::gc_abi::MIN_OBJECT_SIZE as usize)
        .next_multiple_of(8);
    if size8 < LOS_THRESHOLD {
        let Some(o) = copy_alloc(size8) else {
            nabir_common::oom();
        };
        // SAFETY: `size8` bytes of free lines (not zeroed by the sweep).
        unsafe { ptr::write_bytes(o, 0, size8) };
        init_header(o, to, size8, FLAG_STICKY | FLAG_UNLOGGED);
        return o;
    }
    new_large(need, to)
}

/// [`blocks::alloc_large`] without the world lock (held by the caller)
/// or a collection, old and carded.
fn new_large(size: usize, to: u32) -> *mut u8 {
    let align = LOS_ALIGN;
    let offset = (size_of::<Prefix>() + HEADER_SIZE).next_multiple_of(align) - HEADER_SIZE;
    let total = los_total(&Prefix {
        next: ptr::null_mut(),
        size,
        offset,
        cards: ptr::null_mut(),
    });
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
    let h = heap();
    // SAFETY: `base..base+total` is ours, laid out as `alloc_large` does.
    unsafe {
        let obj = base.cast::<u8>().add(offset);
        prefix(obj).write(Prefix {
            next: h.los,
            size,
            offset,
            cards: obj.add(size),
        });
        init_header(obj, to, size, FLAG_LARGE | FLAG_STICKY | FLAG_CARDED);
        h.los = obj;
        h.los_count += 1;
        h.live_bytes += size;
        obj
    }
}

/// A scratch buffer for in-place rewrites, grown as needed.
struct Scratch {
    p: *mut u8,
    cap: usize,
}

impl Scratch {
    const fn new() -> Self {
        Self {
            p: ptr::null_mut(),
            cap: 0,
        }
    }

    fn take(&mut self, n: usize) -> *mut u8 {
        if n > self.cap {
            // SAFETY: our own buffer (or null).
            let p = unsafe { libc::realloc(self.p.cast(), n.max(256)) };
            if p.is_null() {
                nabir_common::oom();
            }
            (self.p, self.cap) = (p.cast(), n.max(256));
        }
        self.p
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // SAFETY: our own buffer (or null).
        unsafe { libc::free(self.p.cast()) };
    }
}

/// After the rewrite: fix every reference into a moved object, and
/// every interior one into a buffer rewritten in place, in every live
/// object (copies included) and in the plan's static words.
#[allow(clippy::cast_ptr_alignment)] // reference words are 8-aligned
fn fix(plan: &mut dyn Plan, inplace: &[(usize, Rule)]) {
    each_object(heap().nblocks, heap().los, |o| {
        for_each_ref(o, 0, usize::MAX, |off, _| {
            // SAFETY: a reference word inside the live `o`.
            unsafe {
                let slot = o.add(off).cast::<usize>();
                let w = slot.read_unaligned();
                let n = fixed(plan, inplace, w);
                if n != w {
                    slot.write_unaligned(n);
                }
            }
        });
    });
    let mut words: MBuf<usize> = MBuf::new();
    plan.statics(&mut |w| words.push(w as usize));
    for &w in words.as_slice() {
        // SAFETY: a word the plan owns; the world is stopped.
        unsafe { *(w as *mut usize) = fixed(plan, inplace, *(w as *const usize)) };
    }
}

/// Reference `w` after the rewrite.
fn fixed(plan: &dyn Plan, inplace: &[(usize, Rule)], w: usize) -> usize {
    let Some(t) = object_in::<true>(w, false, true) else {
        return w;
    };
    let t = t as usize;
    // SAFETY: an object header (forwarded or not).
    let (flags, tid) = unsafe {
        let h = &*header(t as *mut u8);
        (h.gc_flags.load(Ordering::Relaxed), h.type_id)
    };
    if flags & FLAG_FORWARDED != 0 {
        return plan
            .rule(tid)
            .and_then(|r| remap(&r, t, w))
            .map_or(w, |off| forwarded(t) + off);
    }
    match inplace.binary_search_by_key(&t, |e| e.0) {
        Ok(i) => remap(&inplace[i].1, t, w).map_or(w, |off| t + off),
        Err(_) => w,
    }
}

/// The old copies of moved objects become garbage: a block object's
/// start bit goes (its lines are free at the next sweep), a LOS object
/// is freed.
fn release(moved: &MBuf<usize>) {
    let h = heap();
    for &o in moved.as_slice() {
        if o >= h.base && o < h.base + h.nblocks * BLOCK_SIZE {
            let b = (o - h.base) / BLOCK_SIZE;
            let off = o - h.base - b * BLOCK_SIZE;
            // SAFETY: `b < nblocks`.
            unsafe { (*h.meta.add(b)).starts[off / 8 / 64] &= !(1 << (off / 8 % 64)) };
        }
    }
    // SAFETY: walks the LOS list, unlinking moved objects before their
    // memory goes.
    unsafe {
        let mut link: *mut *mut u8 = &mut h.los;
        while !(*link).is_null() {
            let o = *link;
            let p = prefix(o);
            if (*header(o)).gc_flags.load(Ordering::Relaxed) & FLAG_FORWARDED == 0 {
                link = &mut (*p).next;
                continue;
            }
            *link = (*p).next;
            h.los_count -= 1;
            h.live_bytes = h.live_bytes.saturating_sub((*p).size);
            let (base, total) = (o.sub((*p).offset), los_total(&*p));
            if total >= LOS_MAP_MIN {
                sys::unmap(base, total);
            } else {
                sys::free_aligned(base);
            }
        }
    }
}
