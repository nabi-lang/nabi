//! Garbage collector: mark-region (Immix-style blocks and
//! lines), generational, with opportunistic evacuation and concurrent
//! marking ([`concurrent`]). Precise in the heap and roots; ambiguous
//! words pin what they reach.
//!
//! Objects below [`LOS_THRESHOLD`] live in 32 KiB blocks of 128-byte
//! lines. Each thread bump-allocates from its TLAB; compiled
//! code inlines the bump and calls [`alloc`] only on refill, which
//! zeroes the next free lines of a block the thread claimed whole,
//! lock-free ([`Claims`]). Large objects are one allocation each,
//! linked through a hidden [`Prefix`].
//!
//! Every `gc_word` holds the object's size (its new address
//! while evacuating) and block objects have a nonzero `type_id`, so a
//! block is parsable: non-object memory is zero. Per-block object-start
//! bitmaps are filled lazily, so an unreachable young block is freed
//! without being read.
//!
//! Generations use sticky mark bits. A minor collection
//! marks from the roots and the remembered set and sweeps only the
//! blocks handed out since the last collection (the claim lists carry
//! over, [`rebuild_claims`]). The write barrier tests
//! `UNLOGGED` on an old object; the slow path ([`write_barrier_slow`])
//! records it. An old LOS object is `CARDED` instead: the barrier dirties
//! a 512-byte card and a minor collection scans only dirty cards. A
//! minor collection runs once the young budget ([`young_budget`]) is
//! handed out, a full one when old bytes exceed `max(4 MiB, 2 x live)`; past
//! [`CONCURRENT_MIN`] it marks concurrently (no evacuation then), its
//! blocks swept after its last pause while mutators run. Stop-the-world
//! marks run on worker threads too ([`parallel`]).
//!
//! Evacuation (full stop-the-world collections only): when a quarter of
//! used lines are free, blocks at most half occupied are candidates
//! ([`select_candidates`]); survivors move into holes elsewhere and one
//! pass rewrites every precise reference ([`evacuate::fix_references`]).
//! An object reached by a root or an ambiguous word, pinned, or `NOMOVE`
//! stays put: compiled code and runtime calls derive raw pointers from
//! rooted values. Free pages beyond the next cycle's budget go back to
//! the OS. Block objects are remembered whole; evacuation runs in
//! pauses.
//!
//! A program steers it through [`control`]: a pause goal, minor
//! collections on demand, no-collect regions, a heap limit.
//!
//! Roots: compiled code links a frame `{ prev, count, slots[count] }` into `nabi_tls_shadow_stack_top` before
//! the first safepoint of each path and unlinks it on every return of a
//! path that linked it; each slot is the address of a word holding a GC
//! reference (0 = none), or (odd) `(n << 1) | 1` followed by the address
//! of the first of `n` consecutive such words.
//!
//! Children: [`register_type`] maps `type_id` to a `ScanTable`
//! ([`for_each_ref`]): `STRONG_REF` / `INTERIOR_REF` words, `ANY_SLOT`
//! (a reference iff the tag has [`ANY_BOXED`]), `SUM_PAYLOAD` (the tag
//! selects the variant, [`visit_sum`]), `AMBIGUOUS` and `WEAK_REF`
//! ([`clear_dead_weak`]). `TABLE_ARRAY` describes one element repeated
//! over a buffer. Type id 2 is all ambiguous words, 3 a weak cell
//! ([`weak_new`]). A word outside every object is ignored; an ambiguous
//! one only retains and pins the object it hits.
//!
//! Debug knobs (read once): `NABIR_GC_STRESS=N` (a collection every `N`
//! runtime allocations, every 8th full), `NABIR_GC_EVAC=always`,
//! `NABIR_GC_WORKERS=N` (parallel markers),
//! `NABIR_GC_CONCURRENT=always|off`, `NABIR_GC_STATS=1`, `NABIR_GC_LOG=1`
//! ([`Pause`]), `NABIR_HEAP_DUMP=on_oom` ([`heap_dump`]). Other values warn
//! ([`nabir_common::env_knob`]).

#![cfg_attr(not(test), no_std)]

pub mod migrate;
pub mod modules;
pub mod world;

mod barrier;
mod blocks;
mod collect;
mod concurrent;
mod control;
mod evacuate;
mod handles;
mod index;
mod mark;
mod parallel;
mod pins;
mod stats;
mod sweep;

pub use crate::barrier::*;
pub use crate::blocks::*;
pub use crate::collect::*;
pub use crate::control::*;
pub(crate) use crate::evacuate::{evacuate, select_candidates};
pub use crate::handles::*;
pub use crate::index::*;
pub use crate::mark::*;
pub use crate::pins::*;
pub(crate) use crate::stats::{
    Hist, Kind, Phase, Phases, Stderr, heap_dump, lap, now_ns, print_stats, record,
};
pub use crate::stats::{STATS_WORDS, stats};
pub(crate) use crate::sweep::{
    SWEEPING, adopt_sweep, begin_lazy, finish_sweep, release_free_blocks, set_full_lines_at,
    sweep_blocks, sweep_los, sweep_some,
};
pub use nabir_common::gc_abi::{ANY_BOXED, SCAN_WORDS_TYPE_ID};

use core::cell::UnsafeCell;
use core::fmt::Write as _;
use core::mem::size_of;
use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};
use nabir_common::gc_abi::{
    AMBIGUOUS, ANY_SLOT, INTERIOR_REF, KIND_MASK, MIN_OBJECT_SIZE, PLAIN_TYPE_ID, STRONG_REF,
    SUM_DESC_SHIFT, SUM_PAYLOAD, USER_TYPE_BASE, WEAK_REF, WEAK_TYPE_ID,
};
use nabir_common::header::{
    FLAG_CARDED, FLAG_FORWARDED, FLAG_LARGE, FLAG_MARK, FLAG_NOEVAC, FLAG_NOMOVE, FLAG_PINNED,
    FLAG_SATB, FLAG_SCANNED, FLAG_STICKY, FLAG_UNLOGGED, FLAG_WIPE, HEADER_ALIGN, HEADER_SIZE,
    ObjectHeader,
};
#[cfg(target_os = "none")]
use nabir_common::sys::libc;
use nabir_common::{env_choice, env_flag, env_num, fatal, sys};

/// The `ScanTable` of a weak cell ([`WEAK_TYPE_ID`]): one `WEAK_REF`
/// word after the header.
static WEAK_TABLE: [u32; 4] = [24, 1 | (1 << 16), 16, WEAK_REF];

/// Old bytes (and old lines' bytes) below which no full collection
/// runs.
const MIN_HEAP: usize = 4 << 20;
/// Young budget (bytes handed out between collections): this much per
/// thread that allocated in the last cycle, within
/// [`YOUNG_MIN`]..=[`YOUNG_MAX`]; a heap of more old bytes gets as many
/// young ones, up to 512 MiB ([`young_room`]). `NABIR_GC_YOUNG` sets it.
#[cfg(not(target_os = "none"))]
const YOUNG_PER_THREAD: usize = 8 << 20;
#[cfg(not(target_os = "none"))]
const YOUNG_MIN: usize = 32 << 20;
/// A board's heap is small (and it runs one thread).
#[cfg(target_os = "none")]
const YOUNG_PER_THREAD: usize = 4 << 20;
#[cfg(target_os = "none")]
const YOUNG_MIN: usize = 4 << 20;
const YOUNG_MAX: usize = 128 << 20;
const TABLE_ARRAY: u16 = nabir_common::gc_abi::TABLE_ARRAY as u16;

/// Immix block / line geometry.
pub const BLOCK_SIZE: usize = 32 << 10;
pub const LINE_SIZE: usize = 128;
const LINES: usize = BLOCK_SIZE / LINE_SIZE;
const GRANULES: usize = BLOCK_SIZE / 8;
/// Objects this large (header included) go to the LOS.
pub const LOS_THRESHOLD: usize = nabir_common::gc_abi::LOS_THRESHOLD as usize;
/// Payload alignment of a LOS object of at least [`LOS_THRESHOLD`] bytes.
pub const LOS_ALIGN: usize = nabir_common::gc_abi::LOS_ALIGN as usize;
/// Card size of an old LOS object (`FLAG_CARDED`): a reference store
/// dirties the card(s) of its slot and a minor collection scans only
/// dirty cards, not the whole object.
pub const CARD_SIZE: usize = 1 << nabir_common::gc_abi::CARD_SHIFT;
/// Address space reserved for blocks (committed one block at a time):
/// one reservation, the first of these sizes the OS grants. 64-bit: the
/// block heap is capped at 64 GiB. 32-bit (a heap of about
/// 3.5 GB): at most 2 GiB, the rest of the address space left to LOS
/// objects, `malloc` and the program. wasm32 and targets without an OS
/// (`nabi_flat`) reserve nothing: see `blocks::spare_block`.
#[cfg(target_pointer_width = "64")]
const RESERVE: usize = 64 << 30;
#[cfg(all(target_pointer_width = "32", not(nabi_flat)))]
const RESERVE: usize = 2 << 30;
#[cfg(not(nabi_flat))]
const RESERVE_MIN: usize = 64 << 20;

/// Line state of `nabi_flat` memory that is not a block of the heap:
/// `malloc`'s, or a block of a chunk not handed out yet
/// ([`spare_block`]). Never swept or moved.
#[cfg(nabi_flat)]
const FOREIGN: u8 = 4;

/// Line states.
const FREE: u8 = 0;
/// Holds (part of) an object that survived the last collection.
const LIVE: u8 = 1;
/// Handed out (TLAB or medium object) since the last collection.
const TAKEN: u8 = 2;
/// Handed out while a concurrent mark runs: its objects are younger than
/// the mark, which neither reads nor sweeps them (allocate-black, line
/// granular); `TAKEN` once the mark is done.
const BLACK: u8 = 3;

/// The state of line `l` of the block whose metadata is at `m`. Atomic:
/// a concurrent mark reads lines that refills write.
fn line(m: *const BlockMeta, l: usize) -> u8 {
    // SAFETY: `m` is a block's metadata, `l < LINES`.
    unsafe { core::sync::atomic::AtomicU8::from_ptr((&raw const (*m).lines[l]).cast_mut()) }
        .load(Ordering::Relaxed)
}

fn set_line(m: *mut BlockMeta, l: usize, state: u8) {
    // SAFETY: as in `line`.
    unsafe { core::sync::atomic::AtomicU8::from_ptr(&raw mut (*m).lines[l]) }
        .store(state, Ordering::Relaxed);
}

/// Granules (start bits) per line.
const LINE_GRANULES: usize = LINE_SIZE / 8;

/// [`BlockMeta::parse`] states.
const PARSING: u8 = 1;
const PARSED: u8 = 2;

/// Side metadata of one block.
#[allow(clippy::struct_excessive_bools)] // independent per-block states
struct BlockMeta {
    lines: [u8; LINES],
    /// Object starts (one bit per 8-byte granule): `TAKEN` lines are
    /// parsed in by every collection, dead objects cleared by its sweep.
    starts: [u64; GRANULES / 64],
    /// Holds objects or was handed out since the last sweep (else it
    /// is all zero and all `FREE`).
    used: bool,
    /// This collection parsed the `TAKEN` lines (on the first lookup
    /// that landed in one): a block nobody looked into this time holds
    /// no survivor there, so its `TAKEN` lines just become free.
    /// [`PARSED`]; parallel markers see [`PARSING`] meanwhile.
    parse: u8,
    /// Unparsed, but a minor collection set the start bits of the young
    /// objects references reached by their start ([`push_start`]): the
    /// only start bits in its `TAKEN` lines, all of survivors.
    seeded: bool,
    /// An evacuation candidate of the running full collection.
    evac: bool,
    /// Free, and its pages were given back to the OS (they fault in
    /// zeroed when the block is used again).
    released: bool,
    /// Claimed zeroed (fresh or released): its free lines need no
    /// zeroing when handed out. Valid while claimed.
    clean: bool,
    /// In a claim list, not claimed yet.
    listed: bool,
    /// The lazy sweep of this concurrent mark (`Heap::cycles`) passes
    /// it by: it was free and listed when the mark finished.
    swept: u64,
}

/// Hidden bookkeeping right before each LOS object header.
#[repr(C)]
struct Prefix {
    /// Header of the next LOS object.
    next: *mut u8,
    /// Total object size, header included.
    size: usize,
    /// Bytes from the allocation base to the header.
    offset: usize,
    /// Card table (one byte per [`CARD_SIZE`] of the object, after its
    /// end in the same allocation). Last field: compiled code's
    /// barrier loads it from the word before the header.
    cards: *mut u8,
}

#[allow(clippy::struct_excessive_bools)] // independent states
struct Heap {
    /// Bytes of objects alive after the last collection plus the LOS
    /// objects since; the blocks claimed since are
    /// [`blocks::young_bytes`].
    live_bytes: usize,
    next_gc: usize,
    /// `NABIR_GC_YOUNG`: the young budget in bytes (0: by threads).
    young: usize,
    /// Threads that allocated from a block in the cycle the last pause
    /// ended.
    allocators: usize,
    collections: u64,
    /// Bytes alive after the last collection (all old).
    old_bytes: usize,
    /// The next automatic collection is full once
    /// `old_bytes` reaches this.
    full_at: usize,
    /// `LIVE` lines after the last sweep: old objects pin whole lines,
    /// so a few scattered survivors per line hold far more than their
    /// bytes.
    old_lines: usize,
    /// The next automatic collection is also full once `old_lines`
    /// cover this many bytes.
    full_lines_at: usize,
    minors: u64,
    /// Total / longest pause (`NABIR_GC_STATS`).
    gc_ns: u64,
    max_ns: u64,
    /// Collector-thread CPU time in pauses (robust on a loaded machine).
    gc_cpu_ns: u64,
    /// Pauses of full collections (both pauses of a concurrent one):
    /// count, total, longest.
    full_pauses: u64,
    full_ns: u64,
    full_max_ns: u64,
    /// Their collector-thread CPU time: total, longest.
    full_cpu_ns: u64,
    full_cpu_max_ns: u64,
    /// `NABIR_GC_*` read (first slow-path allocation or collection).
    configured: bool,
    /// `NABIR_GC_LOG`: a line per collection to stderr.
    log: bool,
    /// `NABIR_GC_STRESS`: collect every `stress` runtime allocations (0 =
    /// off); `allocs` counts them.
    stress: u64,
    allocs: u64,
    /// Bytes handed out since the last full collection `before_alloc`
    /// started (`NABIR_GC_STRESS` paces its full ones by it).
    since_full: usize,
    /// Block `i` is at `base + i * BLOCK_SIZE`; 0 until the first one.
    /// `meta` is reserved for every block of the reservation, so it never
    /// moves.
    base: usize,
    nblocks: usize,
    meta: *mut BlockMeta,
    /// Bytes reserved at `base`.
    reserved: usize,
    /// `nabi_flat`: the blocks of the last chunk not handed out yet.
    #[cfg(nabi_flat)]
    spare: [usize; 2],
    /// LOS objects, linked through their prefixes (newest first).
    los: *mut u8,
    los_count: usize,
    /// `NABIR_GC_EVAC=always`: every full collection evacuates every block
    /// it may (tests); otherwise only when fragmented.
    evac_always: bool,
    /// `NABIR_GC_CONCURRENT`: whether full collections mark concurrently.
    concurrent: Concurrent,
    /// A concurrent mark runs: refills hand out `BLACK` lines.
    marking: bool,
    /// The LOS list head when the running concurrent mark started (the
    /// objects in front of it are younger than the mark).
    los_mark_head: *mut u8,
    /// `live_bytes` when the running concurrent mark started.
    mark_start_live: usize,
    /// A concurrent mark found the heap fragmented: the next full
    /// collection stops the world and evacuates.
    stw_next: bool,
    /// `NABIR_GC_STATS`: concurrent marks, and those the allocation budget
    /// finished before the marker did.
    cycles: u64,
    forced: u64,
    /// Bytes mark assists granted the running mark beyond its budget,
    /// and the assists so far (`NABIR_GC_STATS`).
    assisted: usize,
    assists: u64,
    /// Copy allocator of an evacuation: bump `copy_cursor..copy_limit`
    /// (free lines of a non-candidate block); `copy_line` is the next
    /// global line index it looks at.
    copy_cursor: usize,
    copy_limit: usize,
    copy_line: usize,
    /// 0: holes of used blocks, 1: free blocks, 2: fresh blocks.
    copy_phase: u8,
    /// Fresh blocks the copy allocator may still commit (unbounded for
    /// `NABIR_GC_EVAC=always`).
    fresh_blocks: usize,
    /// `NABIR_GC_STATS`: evacuating collections, bytes moved.
    evacuations: u64,
    moved_bytes: u64,
    /// Pauses by [`Kind`].
    pauses: [Hist; 4],
    /// Where the running pause spends its time (`NABIR_GC_LOG`).
    phases: Phases,
    /// Blocks that may hold `TAKEN` lines: those claimed since the
    /// last pause (new ones as they are claimed, listed ones at the next
    /// pause) and, after a concurrent mark,
    /// those it handed out `BLACK` lines in. A minor collection sweeps
    /// only these (repeats allowed).
    young_blocks: *mut u32,
    young_len: usize,
    young_cap: usize,
    /// [`set_pause_goal`] (0: none), and what a minor collection costs
    /// per young byte, in picoseconds (a moving average; 0: unknown).
    pause_goal_ns: u64,
    minor_ps_per_byte: u64,
    /// Open no-collect regions ([`no_collect`]); bytes they grew the
    /// heap by since the last pause, and in all.
    no_collect: usize,
    region_room: usize,
    grown_bytes: u64,
    /// [`set_heap_limit`] (0: none), and the collections it caused.
    heap_limit: usize,
    limit_collections: u64,
    /// Objects the last concurrent mark marked (the next one's mark
    /// assists pace themselves by it).
    last_marked: usize,
}

/// `NABIR_GC_CONCURRENT`: `auto` marks concurrently once the old heap
/// reaches [`CONCURRENT_MIN`]; `always` / `off` are for tests.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Concurrent {
    Auto,
    Always,
    Off,
}

/// Old bytes from which `NABIR_GC_CONCURRENT=auto` marks concurrently.
const CONCURRENT_MIN: usize = 32 << 20;

struct Global<T>(UnsafeCell<T>);
// SAFETY: `HEAP` is only touched with the world lock held, `MARK` with
// the mark lock held (see their accessors).
unsafe impl<T> Sync for Global<T> {}

static HEAP: Global<Heap> = Global(UnsafeCell::new(Heap {
    live_bytes: 0,
    next_gc: MIN_HEAP,
    young: 0,
    allocators: 0,
    collections: 0,
    old_bytes: 0,
    full_at: MIN_HEAP,
    old_lines: 0,
    full_lines_at: MIN_HEAP,
    minors: 0,
    gc_ns: 0,
    max_ns: 0,
    gc_cpu_ns: 0,
    full_pauses: 0,
    full_ns: 0,
    full_max_ns: 0,
    full_cpu_ns: 0,
    full_cpu_max_ns: 0,
    configured: false,
    log: false,
    stress: 0,
    allocs: 0,
    since_full: 0,
    base: 0,
    nblocks: 0,
    reserved: 0,
    #[cfg(nabi_flat)]
    spare: [0; 2],
    meta: ptr::null_mut(),
    los: ptr::null_mut(),
    los_count: 0,
    evac_always: false,
    concurrent: Concurrent::Auto,
    marking: false,
    los_mark_head: ptr::null_mut(),
    mark_start_live: 0,
    stw_next: false,
    cycles: 0,
    forced: 0,
    assisted: 0,
    assists: 0,
    copy_cursor: 0,
    copy_limit: 0,
    copy_line: 0,
    copy_phase: 0,
    fresh_blocks: 0,
    evacuations: 0,
    moved_bytes: 0,
    pauses: [Hist::NEW; 4],
    phases: Phases::NEW,
    young_blocks: ptr::null_mut(),
    young_len: 0,
    young_cap: 0,
    pause_goal_ns: 0,
    minor_ps_per_byte: 0,
    no_collect: 0,
    region_room: 0,
    grown_bytes: 0,
    heap_limit: 0,
    limit_collections: 0,
    last_marked: 0,
}));

fn heap() -> &'static mut Heap {
    // SAFETY: callers hold the world lock; no caller keeps the reference
    // across another `heap()` call.
    unsafe { &mut *HEAP.0.get() }
}

/// Marking state: the collector's while the world is stopped, the
/// marker thread's while a concurrent mark runs. Guarded by
/// `concurrent::MARK_LOCK`. Marking never calls [`heap`]: mutators
/// may hold it meanwhile.
struct Mark {
    /// Mark stack (grown with `realloc`), `len` entries.
    stack: *mut *mut u8,
    stack_cap: usize,
    len: usize,
    /// The blocks lookups land in: `nblocks` from `base` as of this
    /// collection's start (a concurrent mark ignores later ones).
    base: usize,
    nblocks: usize,
    meta: *mut BlockMeta,
    reserved: usize,
    /// Sorted LOS addresses as of this collection's start.
    index: *mut usize,
    index_cap: usize,
    los_count: usize,
    /// A full collection (a minor one marks young objects only).
    full: bool,
    /// A concurrent mark: lines may turn `BLACK` meanwhile, scanned
    /// objects get `SCANNED`.
    concurrent: bool,
    /// This (full) collection may evacuate: the mark records `pinned`.
    evacuating: bool,
    /// Block objects a root or an ambiguous word reaches (repeats
    /// allowed): an evacuation leaves them in place.
    pinned: *mut usize,
    pinned_len: usize,
    pinned_cap: usize,
    /// Addresses of the `WEAK_REF` words of the scanned objects.
    weak: *mut usize,
    weak_len: usize,
    weak_cap: usize,
    /// Objects marked by this collection (`NABIR_GC_LOG`), and their
    /// bytes (a concurrent mark's survivors, before the lazy sweep).
    marked: usize,
    marked_bytes: usize,
}

/// Marker thread CPU time (`NABIR_GC_STATS`).
static MARKER_CPU_NS: nabir_common::AtomicU64 = nabir_common::AtomicU64::new(0);

static MARK: Global<Mark> = Global(UnsafeCell::new(Mark {
    stack: ptr::null_mut(),
    stack_cap: 0,
    len: 0,
    base: 0,
    nblocks: 0,
    reserved: 0,
    meta: ptr::null_mut(),
    index: ptr::null_mut(),
    index_cap: 0,
    los_count: 0,
    full: false,
    concurrent: false,
    evacuating: false,
    pinned: ptr::null_mut(),
    pinned_len: 0,
    pinned_cap: 0,
    weak: ptr::null_mut(),
    weak_len: 0,
    weak_cap: 0,
    marked: 0,
    marked_bytes: 0,
}));

fn mark() -> &'static mut Mark {
    // SAFETY: callers hold the mark lock; no caller keeps the reference
    // across another `mark()` call.
    unsafe { &mut *MARK.0.get() }
}

/// The mark state's heap view (blocks, LOS index, kind of mark), read by
/// every marking thread; nobody changes it while they mark.
fn view() -> &'static Mark {
    // SAFETY: as in `mark`; parallel markers only read, and the
    // collector takes no `mark()` reference while they run.
    unsafe { &*MARK.0.get() }
}

const fn prefix(obj: *mut u8) -> *mut Prefix {
    obj.wrapping_sub(size_of::<Prefix>()).cast()
}

const fn header(obj: *mut u8) -> *mut ObjectHeader {
    obj.cast()
}

/// Size of the object at `obj` (its `gc_word`; not of an evacuated
/// one, see [`moved_size`]).
fn obj_size(obj: *mut u8) -> usize {
    // SAFETY: callers pass object headers (or zeroed block memory, which
    // reads as 0).
    unsafe { (*header(obj)).gc_word as usize }
}

/// Bytes after the header of the live object `obj`.
///
/// # Safety
/// `obj` must be the start of a live GC object.
pub unsafe fn payload_bytes(obj: *mut u8) -> usize {
    obj_size(obj) - HEADER_SIZE
}

/// [`obj_size`] that also reads an evacuated object's (its copy's).
fn moved_size(obj: *mut u8) -> usize {
    // SAFETY: an object header; a forwarded one's `gc_word` is its copy.
    unsafe {
        let h = &*header(obj);
        if h.gc_flags.load(Ordering::Relaxed) & FLAG_FORWARDED != 0 {
            return obj_size(h.gc_word as *mut u8);
        }
        h.gc_word as usize
    }
}

/// Write a fresh header with `size` in `gc_word` into zeroed memory.
fn init_header(obj: *mut u8, type_id: u32, size: usize, flags: u32) {
    // SAFETY: `obj` is 8-aligned memory of at least `size` bytes we own.
    unsafe {
        ObjectHeader::init(obj, type_id);
        (*header(obj)).gc_word = size as u64;
        (*header(obj)).gc_flags.store(flags, Ordering::Relaxed);
    }
}

/// Bytes held by GC objects (headers included) as of the last
/// collection, plus everything handed out since (claimed blocks count
/// in full).
pub fn live_bytes() -> usize {
    world::lock();
    let n = heap().live_bytes + young_bytes();
    world::unlock();
    n
}

pub fn collections() -> u64 {
    world::lock();
    let n = heap().collections;
    world::unlock();
    n
}

/// Bytes evacuation has copied so far.
pub fn moved_bytes() -> u64 {
    world::lock();
    let n = heap().moved_bytes;
    world::unlock();
    n
}

#[cfg(test)]
#[allow(
    clippy::missing_const_for_fn,
    clippy::cast_ptr_alignment,
    clippy::items_after_statements
)]
mod tests {
    use super::*;

    #[repr(C)]
    struct Frame {
        prev: *mut Frame,
        count: usize,
    }

    /// A clock that steps back inside a pause gives 0, not an overflow.
    #[test]
    fn pause_spans_survive_a_clock_step_back() {
        assert_eq!(collect::spans(100, 130, 200), (100, 30));
        assert_eq!(collect::spans(100, 130, 90), (0, 0));
        assert_eq!(collect::spans(100, 90, 200), (100, 0));
        assert_eq!(collect::spans(100, 150, 120), (20, 20));
    }

    #[test]
    #[allow(clippy::cast_ptr_alignment)] // GC objects are 8-aligned
    fn collect_frees_unreachable_and_keeps_traced_children() {
        // ScanTable: total 32, version 1, one STRONG_REF at offset 16.
        static TABLE: [u32; 4] = [32, 1 | (1 << 16), 16, STRONG_REF];
        nabir_common::tls::thread_init();
        world::attach_current();
        register_type(USER_TYPE_BASE, TABLE.as_ptr().cast());
        collect();
        let base = live_bytes();
        let a = alloc(32, 8, USER_TYPE_BASE);
        let b = alloc(24, 8, USER_TYPE_BASE + 1); // unregistered: leaf
        let garbage = alloc(24, 8, USER_TYPE_BASE + 1);
        // SAFETY: `a` has a 16-byte payload; frame layout matches
        // `Frame` + one slot; the frame is unlinked before it dies.
        unsafe {
            *garbage.add(16) = 7;
            *a.add(16).cast::<*mut u8>() = b;
            let mut junk = 0x1234usize; // inline `any` bits: ignored
            let mut root = a as usize;
            let mut frame: (Frame, *mut usize, *mut usize) = (
                Frame {
                    prev: ptr::null_mut(),
                    count: 2,
                },
                &mut root,
                &mut junk,
            );
            let top = nabir_common::tls::shadow_stack_top();
            *top = (&raw mut frame).cast();
            collect();
            assert_eq!(live_bytes(), base + 32 + 24);
            assert_eq!((*header(b)).type_id, USER_TYPE_BASE + 1);
            assert_eq!(object_in::<false>(garbage as usize + 16, false, true), None);
            *frame.1 = 0;
            collect();
            assert_eq!(live_bytes(), base);
            *top = ptr::null_mut();
        }
        frame_runs_root_each_word();
        frame_descriptor_roots_by_offset();
        scan_words_buffers_and_interior_pointers();
        medium_and_large_objects();
        free_lines_are_reused();
        minor_collection_keeps_logged_children();
        minor_collection_scans_dirty_cards();
        evacuation_moves_objects_and_fixes_references();
        sum_payloads_scan_the_active_variant();
        concurrent_mark_keeps_the_snapshot();
        concurrent_allocation_is_not_fragmentation();
        evacuation_defragments_sparse_blocks();
        weak_cells_clear_and_follow_moves();
        secrets_stay_in_place_and_are_wiped();
        crate::modules::tests::a_retired_module_lives_while_reached();
        free_lines_count_free_bytes();
        pins_count_per_object();
        pause_histogram_percentiles();
        parallel_mark_keeps_every_object();
        // A thread that ends attached would stall every later stop.
        world::detach();
    }

    /// A graph wide enough to mark in parallel (the stress threshold):
    /// a full and a minor collection keep every reachable object, young
    /// ones in unparsed blocks included, and free the rest.
    fn parallel_mark_keeps_every_object() {
        static R: [u32; 4] = [8, 1 | TABLE_ARRAY as u32 | (1 << 16), 0, STRONG_REF];
        let (tr, leaf) = (USER_TYPE_BASE + 14, USER_TYPE_BASE + 1);
        register_type(tr, R.as_ptr().cast());
        heap().stress = 1 << 40;
        collect();
        let base = live_bytes();
        const ROWS: usize = 64;
        const COLS: usize = 512;
        let top = alloc(16 + 8 * ROWS, 8, tr);
        // SAFETY: the frame is unlinked before it dies.
        unsafe {
            let mut root = top as usize;
            let mut frame: (Frame, *mut usize) = (
                Frame {
                    prev: ptr::null_mut(),
                    count: 1,
                },
                &mut root,
            );
            let stack = nabir_common::tls::shadow_stack_top();
            *stack = (&raw mut frame).cast();
            for r in 0..ROWS {
                let row = alloc(16 + 8 * COLS, 8, tr);
                set(top, 16 + 8 * r, row as usize);
                for c in 0..COLS {
                    let o = alloc(24, 8, leaf);
                    set(o, 16, r * COLS + c);
                    set(row, 16 + 8 * c, o as usize);
                }
            }
            let rows = 16 + 8 * ROWS + ROWS * (16 + 8 * COLS);
            collect_young();
            assert_eq!(live_bytes(), base + rows + ROWS * COLS * 24);
            collect();
            for r in 0..ROWS {
                let row = word(top, 16 + 8 * r) as *mut u8;
                for c in (0..COLS).step_by(7) {
                    assert_eq!(word(word(row, 16 + 8 * c) as *mut u8, 16), r * COLS + c);
                }
                // Half of each row dies.
                for c in (0..COLS).filter(|c| c % 2 == 1) {
                    set(row, 16 + 8 * c, 0);
                }
            }
            collect();
            assert_eq!(live_bytes(), base + rows + ROWS * COLS / 2 * 24);
            *frame.1 = 0;
            collect();
            assert_eq!(live_bytes(), base);
            *stack = ptr::null_mut();
        }
        heap().stress = 0;
    }

    /// Pins nest per object; the table survives growth and removals in
    /// any order, and an object is unpinned after its last unpin.
    fn pins_count_per_object() {
        let leaf = USER_TYPE_BASE + 1;
        let n0 = unsafe { &*PINS.table.get() }.objects().count();
        // Pinned as allocated, so no collection frees one meanwhile.
        let objs: Vec<*mut u8> = (0..200)
            .map(|i| {
                let o = alloc(24, 8, leaf);
                for _ in 0..=i % 3 {
                    // SAFETY: a live object.
                    unsafe { pin(o) };
                }
                o
            })
            .collect();
        let pinned =
            // SAFETY: live objects.
            |o: *mut u8| unsafe { (*header(o)).gc_flags.load(Ordering::Relaxed) } & FLAG_PINNED != 0;
        let count = || unsafe { &*PINS.table.get() }.objects().count();
        // SAFETY: live objects, unpinned as often as pinned.
        unsafe {
            assert_eq!(count(), n0 + objs.len());
            for (i, &o) in objs.iter().enumerate().rev().step_by(2) {
                for _ in 0..=i % 3 {
                    assert!(pinned(o));
                    unpin(o);
                }
                assert!(!pinned(o));
            }
            for (i, &o) in objs.iter().enumerate() {
                assert_eq!(pinned(o), (objs.len() - 1 - i) % 2 == 1, "{i}");
                if pinned(o) {
                    for _ in 0..=i % 3 {
                        unpin(o);
                    }
                }
                assert!(!pinned(o));
            }
        }
        assert_eq!(count(), n0);
    }

    /// Pause percentiles come from the log-linear buckets: within an
    /// eighth above the true value, never past the longest pause.
    fn pause_histogram_percentiles() {
        let mut h = Hist::NEW;
        for us in 1..=1000u64 {
            h.add(us * 1000, 0);
        }
        for (p, want) in [(50, 500_000u64), (90, 900_000), (99, 990_000)] {
            let got = h.percentile(p);
            assert!(got >= want && got <= want + want / 8, "p{p}: {got}");
        }
        assert_eq!(h.percentile(100), 1_000_000);
    }

    /// A line table's `FREE` lines, counted a word at a time.
    fn free_lines_count_free_bytes() {
        let mut lines = [FREE; LINES];
        assert_eq!(free_lines(&lines), LINES);
        for (i, s) in [(0, LIVE), (7, TAKEN), (8, BLACK), (200, 0x80), (255, LIVE)] {
            lines[i] = s;
        }
        assert_eq!(free_lines(&lines), LINES - 5);
    }

    /// A frame entry `(n << 1) | 1` and an address roots `n` words.
    fn frame_runs_root_each_word() {
        let base = live_bytes();
        let x = alloc(24, 8, USER_TYPE_BASE + 1);
        let y = alloc(24, 8, USER_TYPE_BASE + 1);
        // SAFETY: the frame (a run of three words, then one word) is
        // unlinked before it dies.
        unsafe {
            let mut words = [x as usize, 0, y as usize];
            let mut lone = 0usize;
            let mut frame: (Frame, usize, *mut usize, *mut usize) = (
                Frame {
                    prev: ptr::null_mut(),
                    count: 3,
                },
                (3 << 1) | 1,
                words.as_mut_ptr(),
                &mut lone,
            );
            let top = nabir_common::tls::shadow_stack_top();
            *top = (&raw mut frame).cast();
            collect();
            assert_eq!(live_bytes(), base + 2 * 24);
            words[2] = 0;
            *frame.3 = x as usize;
            words[0] = 0;
            collect();
            assert_eq!(live_bytes(), base + 24);
            *frame.3 = 0;
            collect();
            assert_eq!(live_bytes(), base);
            *top = ptr::null_mut();
        }
    }

    /// A count of 0: the frame's one slot is a static descriptor, its
    /// entries the words' offsets from the frame in words, doubled (a
    /// run, then one).
    fn frame_descriptor_roots_by_offset() {
        #[repr(C)]
        struct Fr {
            head: Frame,
            desc: *const u16,
            words: [usize; 3],
            lone: usize,
        }
        let base = live_bytes();
        let x = alloc(24, 8, USER_TYPE_BASE + 1);
        let y = alloc(24, 8, USER_TYPE_BASE + 1);
        let w = size_of::<usize>();
        static DESC: [u16; 4] = [3, (3 << 1) | 1, 3 << 1, 6 << 1];
        // SAFETY: the frame is unlinked before it dies; the descriptor's
        // offsets are those of `Fr`'s fields.
        unsafe {
            let mut fr = Fr {
                head: Frame {
                    prev: ptr::null_mut(),
                    count: 0,
                },
                desc: DESC.as_ptr(),
                words: [x as usize, 0, y as usize],
                lone: 0,
            };
            assert_eq!(core::mem::offset_of!(Fr, words), 3 * w);
            assert_eq!(core::mem::offset_of!(Fr, lone), 6 * w);
            let top = nabir_common::tls::shadow_stack_top();
            *top = (&raw mut fr).cast();
            collect();
            assert_eq!(live_bytes(), base + 2 * 24);
            fr.words[2] = 0;
            fr.lone = x as usize;
            fr.words[0] = 0;
            collect();
            assert_eq!(live_bytes(), base + 24);
            fr.lone = 0;
            collect();
            assert_eq!(live_bytes(), base);
            *top = ptr::null_mut();
        }
    }

    /// Secret buffers freed by a sweep, which wipes them first.
    pub static WIPED: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

    /// A secret buffer: zeroed, a LOS object (never moved), kept by a
    /// collection while reachable, zeroed by `wipe`, and wiped by the
    /// sweep that frees it.
    fn secrets_stay_in_place_and_are_wiped() {
        let base = live_bytes();
        let s = alloc_secret(37);
        // SAFETY: `s` has a payload of 40 bytes; the frame is unlinked
        // before it dies.
        unsafe {
            let f = (*header(s)).gc_flags.load(Ordering::Relaxed);
            assert_eq!(f & (FLAG_LARGE | FLAG_WIPE), FLAG_LARGE | FLAG_WIPE);
            assert_eq!((*prefix(s)).size, HEADER_SIZE + 40);
            let pay = core::slice::from_raw_parts_mut(s.add(HEADER_SIZE), 40);
            assert!(pay.iter().all(|&b| b == 0));
            pay.fill(0xa5);
            let mut root = s as usize;
            let mut frame: (Frame, *mut usize) = (
                Frame {
                    prev: ptr::null_mut(),
                    count: 1,
                },
                &mut root,
            );
            let top = nabir_common::tls::shadow_stack_top();
            *top = (&raw mut frame).cast();
            collect();
            assert!(pay.iter().all(|&b| b == 0xa5));
            wipe(s);
            assert!(pay.iter().all(|&b| b == 0));
            pay.fill(0x5a);
            let wiped = WIPED.load(Ordering::Relaxed);
            *frame.1 = 0;
            collect();
            assert_eq!(WIPED.load(Ordering::Relaxed), wiped + 1);
            assert_eq!(live_bytes(), base);
            *top = ptr::null_mut();
        }
    }

    /// Weak cells: a minor collection clears the one whose young referent
    /// died and keeps an old referent; an evacuation updates a moved
    /// referent's; a full collection clears it once nothing else holds the
    /// referent. A referent read during a concurrent mark survives it.
    #[allow(clippy::many_single_char_names)]
    fn weak_cells_clear_and_follow_moves() {
        let leaf = USER_TYPE_BASE + 1;
        heap().evac_always = true;
        let base = live_bytes();
        let a = alloc(32, 8, USER_TYPE_BASE); // one STRONG_REF @16
        let (x, y) = (alloc(24, 8, leaf), alloc(24, 8, leaf));
        set(y, 16, 0xA);
        set(a, 16, y as usize);
        let (cx, cy) = (weak_new(x), weak_new(y));
        let cell = |c: *mut u8| word(c, 16);
        // SAFETY: frame layout matches `Frame` + three slots, unlinked
        // before it dies.
        unsafe {
            let (mut ra, mut rx, mut ry) = (a as usize, cx as usize, cy as usize);
            let mut frame: (Frame, *mut usize, *mut usize, *mut usize) = (
                Frame {
                    prev: ptr::null_mut(),
                    count: 3,
                },
                &mut ra,
                &mut rx,
                &mut ry,
            );
            let top = nabir_common::tls::shadow_stack_top();
            *top = (&raw mut frame).cast();
            collect_young();
            assert_eq!(cell(cx), 0);
            assert_eq!(cell(cy), y as usize);
            collect();
            let y2 = word(a, 16);
            assert_ne!(y2, y as usize);
            assert_eq!(cell(cy), y2);
            assert_eq!(word(weak_get(cy), 16), 0xA);
            collect_young();
            assert_eq!(cell(cy), y2);
            // A concurrent mark (held off): `y` is weakly reachable only
            // when it starts; read through the cell and stored into an
            // object younger than the mark (never scanned), it survives.
            set(a, 16, 0);
            heap().concurrent = Concurrent::Always;
            concurrent::HOLD.store(true, Ordering::Relaxed);
            world::lock();
            collect_locked(true, true);
            world::unlock();
            assert!(heap().marking);
            concurrent::lock_mark();
            let c = alloc(32, 8, USER_TYPE_BASE);
            set(c, 16, weak_get(cy) as usize);
            *frame.2 = c as usize;
            concurrent::unlock_mark();
            concurrent::HOLD.store(false, Ordering::Relaxed);
            world::lock();
            if heap().marking {
                concurrent::finish(false);
            }
            world::unlock();
            heap().concurrent = Concurrent::Auto;
            assert_eq!(cell(cy), word(c, 16));
            assert_eq!(word(weak_get(cy), 16), 0xA);
            *frame.2 = 0;
            collect();
            assert_eq!(cell(cy), 0);
            assert!(weak_get(cy).is_null());
            *frame.1 = 0;
            *frame.2 = 0;
            *frame.3 = 0;
            collect();
            assert_eq!(live_bytes(), base);
            *top = ptr::null_mut();
        }
        heap().evac_always = false;
    }

    fn word(o: *mut u8, off: usize) -> usize {
        // SAFETY: tests read words inside their own objects.
        unsafe { o.add(off).cast::<usize>().read_unaligned() }
    }

    fn set(o: *mut u8, off: usize, v: usize) {
        // SAFETY: tests write words inside their own objects.
        unsafe { o.add(off).cast::<usize>().write_unaligned(v) };
    }

    /// `NABIR_GC_EVAC=always`: a full collection moves what it may and
    /// fixes every precise reference (header, interior `str`, boxed
    /// `any` payload, array-table buffer element); roots, ambiguous
    /// words, `NOMOVE` objects and inline `any` payloads behave.
    #[allow(clippy::many_single_char_names)]
    fn evacuation_moves_objects_and_fixes_references() {
        // The `any` payload follows the tag by a pointer-size word.
        const ANY: usize = 32 + size_of::<usize>();
        // A: {ref @16, str ptr @24, any tag @32 + payload @32 + W}.
        static A: [u32; 8] = [
            48,
            1 | (3 << 16),
            16,
            STRONG_REF,
            24,
            INTERIOR_REF,
            32,
            ANY_SLOT,
        ];
        // R: a buffer of references (array table, 8-byte elements).
        static R: [u32; 4] = [8, 1 | TABLE_ARRAY as u32 | (1 << 16), 0, STRONG_REF];
        let (ta, tr, leaf) = (USER_TYPE_BASE + 10, USER_TYPE_BASE + 11, USER_TYPE_BASE + 1);
        register_type(ta, A.as_ptr().cast());
        register_type(tr, R.as_ptr().cast());
        heap().evac_always = true;
        let base = live_bytes();
        let a = alloc(48, 8, ta);
        let a2 = alloc(48, 8, ta);
        let (b, c, d, e, f, g) = (
            alloc(24, 8, leaf),
            alloc(32, 8, leaf),
            alloc(24, 8, leaf),
            alloc(24, 8, leaf),
            alloc(24, 8, leaf),
            alloc(24, 8, leaf),
        );
        let r = alloc(16 + 16, 8, tr);
        let w = alloc(16 + 8, 8, SCAN_WORDS_TYPE_ID);
        for (o, v) in [(b, 0xB), (c, 0xC), (d, 0xD), (e, 0xE), (f, 0xF)] {
            set(o, 16, v);
        }
        set(a, 16, b as usize);
        set(a, 24, c as usize + 20);
        // SAFETY: tests write their own objects' headers / words.
        unsafe {
            a.add(32).cast::<u32>().write(0x1234 | ANY_BOXED);
            a2.add(32).cast::<u32>().write(0x1234); // inline: payload is bits
            no_move(f);
        }
        set(a, ANY, r as usize);
        set(a2, ANY, g as usize);
        set(r, 16, e as usize);
        set(r, 24, f as usize);
        set(w, 16, d as usize);
        // SAFETY: frame layout matches `Frame` + three slots, unlinked
        // before it dies.
        unsafe {
            let (mut ra, mut ra2, mut rw) = (a as usize, a2 as usize, w as usize);
            let mut frame: (Frame, *mut usize, *mut usize, *mut usize) = (
                Frame {
                    prev: ptr::null_mut(),
                    count: 3,
                },
                &mut ra,
                &mut ra2,
                &mut rw,
            );
            let top = nabir_common::tls::shadow_stack_top();
            *top = (&raw mut frame).cast();
            let moved = heap().moved_bytes;
            collect();
            assert!(heap().moved_bytes > moved);
            // Rooted objects stay; what they reach precisely moves.
            assert_eq!((ra, ra2, rw), (a as usize, a2 as usize, w as usize));
            let b2 = word(a, 16) as *mut u8;
            assert_ne!(b2, b);
            assert_eq!(word(b2, 16), 0xB);
            let c2 = (word(a, 24) - 20) as *mut u8;
            assert_ne!(c2, c);
            assert_eq!(word(c2, 16), 0xC);
            let r2 = word(a, ANY) as *mut u8;
            assert_ne!(r2, r);
            assert_eq!((*header(r2)).type_id, tr);
            let e2 = word(r2, 16) as *mut u8;
            assert_ne!(e2, e);
            assert_eq!(word(e2, 16), 0xE);
            // Address-taken, ambiguous: in place.
            assert_eq!(word(r2, 24), f as usize);
            assert_eq!(word(f, 16), 0xF);
            assert_eq!(word(w, 16), d as usize);
            assert_eq!(word(d, 16), 0xD);
            // `g` was only behind an inline `any` payload: dead.
            assert_eq!(object_in::<false>(g as usize, false, true), None);
            assert_eq!(object_in::<false>(b as usize, false, true), None);
            assert_eq!(live_bytes(), base + 48 * 2 + 24 * 4 + 32 + 32 + 24);
            // A second round moves the copies again.
            collect();
            let b3 = word(a, 16) as *mut u8;
            assert_eq!(word(b3, 16), 0xB);
            assert_eq!(word(word(word(a, ANY) as *mut u8, 16) as *mut u8, 16), 0xE);
            *frame.1 = 0;
            *frame.2 = 0;
            *frame.3 = 0;
            collect();
            assert_eq!(live_bytes(), base);
            *top = ptr::null_mut();
        }
        heap().evac_always = false;
    }

    /// A concurrent mark (held off by the test, so the stores below
    /// happen mid-mark): a reference moved out of a scanned-later old
    /// object into a new one survives through the snapshot barrier; a new
    /// object stored into an old one is young afterwards and survives the
    /// next minor collection through the carried remembered set.
    fn concurrent_mark_keeps_the_snapshot() {
        let leaf = USER_TYPE_BASE + 1;
        let base = live_bytes();
        let a = alloc(32, 8, USER_TYPE_BASE);
        let b = alloc(24, 8, leaf);
        set(b, 16, 0xB);
        set(a, 16, b as usize);
        // SAFETY: frame layout matches `Frame` + two slots, unlinked
        // before it dies.
        unsafe {
            let (mut ra, mut rc) = (a as usize, 0usize);
            let mut frame: (Frame, *mut usize, *mut usize) = (
                Frame {
                    prev: ptr::null_mut(),
                    count: 2,
                },
                &mut ra,
                &mut rc,
            );
            let top = nabir_common::tls::shadow_stack_top();
            *top = (&raw mut frame).cast();
            collect();
            // `b` may have moved (evacuation); `a` is rooted.
            let b = word(a, 16) as *mut u8;
            heap().concurrent = Concurrent::Always;
            concurrent::HOLD.store(true, Ordering::Relaxed);
            world::lock();
            collect_locked(true, true);
            world::unlock();
            assert!(heap().marking);
            concurrent::lock_mark();
            let c = alloc(32, 8, USER_TYPE_BASE);
            set(c, 16, b as usize);
            *frame.2 = c as usize;
            write_barrier(a, a.add(16));
            set(a, 16, 0);
            let d = alloc(24, 8, leaf);
            set(d, 16, 0xD);
            write_barrier(a, a.add(16));
            set(a, 16, d as usize);
            concurrent::unlock_mark();
            concurrent::HOLD.store(false, Ordering::Relaxed);
            world::lock();
            if heap().marking {
                concurrent::finish(false);
            }
            world::unlock();
            heap().concurrent = Concurrent::Auto;
            assert_eq!(object_in::<false>(b as usize, false, true), Some(b));
            assert_eq!(word(b, 16), 0xB);
            collect_young();
            assert_eq!(word(a, 16), d as usize);
            assert_eq!(object_in::<false>(d as usize, false, true), Some(d));
            assert_eq!(word(d, 16), 0xD);
            assert_eq!(live_bytes(), base + 32 * 2 + 24 * 2);
            *frame.1 = 0;
            *frame.2 = 0;
            collect();
            assert_eq!(live_bytes(), base);
            *top = ptr::null_mut();
        }
    }

    /// Blocks filled during a concurrent mark hold young objects, not
    /// holes: the finish must not take them for fragmentation and make
    /// the next full collection stop the world.
    fn concurrent_allocation_is_not_fragmentation() {
        let leaf = USER_TYPE_BASE + 1;
        collect();
        heap().concurrent = Concurrent::Always;
        concurrent::HOLD.store(true, Ordering::Relaxed);
        world::lock();
        collect_locked(true, true);
        world::unlock();
        assert!(heap().marking);
        concurrent::lock_mark();
        // Whole blocks of `BLACK` lines, more than the old ones.
        for i in 0..(64 * BLOCK_SIZE / 64) {
            set(alloc(64, 8, leaf), 16, i);
        }
        concurrent::unlock_mark();
        concurrent::HOLD.store(false, Ordering::Relaxed);
        world::lock();
        if heap().marking {
            concurrent::finish(false);
        }
        // The lazy sweep's verdict, as the next collection adopts it.
        finish_sweep(heap());
        world::unlock();
        heap().concurrent = Concurrent::Auto;
        assert!(!heap().stw_next);
        collect();
    }

    /// `SUM_PAYLOAD`: only the active variant's words are references -
    /// they move and are fixed; the same word under a scalar variant
    /// keeps nothing alive.
    #[allow(clippy::many_single_char_names)]
    fn sum_payloads_scan_the_active_variant() {
        // {u32 tag @16, payload @24..40}: variant 0 a reference @+8,
        // variant 2 a `str` @+16, variant 1 scalars. Descriptor at word
        // 4, variant entries at words 11 and 13.
        let (ts, leaf) = (USER_TYPE_BASE + 13, USER_TYPE_BASE + 1);
        static U: [u32; 15] = [
            40,
            1 | (1 << 16),
            16,
            SUM_PAYLOAD | (4 << SUM_DESC_SHIFT),
            2,
            0,
            1,
            11,
            2,
            1,
            13,
            8,
            STRONG_REF,
            16,
            INTERIOR_REF,
        ];
        register_type(ts, U.as_ptr().cast());
        heap().evac_always = true;
        let base = live_bytes();
        let (a, b, c) = (alloc(40, 8, ts), alloc(40, 8, ts), alloc(40, 8, ts));
        let (x, y, z) = (alloc(24, 8, leaf), alloc(24, 8, leaf), alloc(32, 8, leaf));
        set(x, 16, 0xA);
        set(z, 16, 0xC);
        // SAFETY: tests write their own objects' words.
        unsafe {
            a.add(16).cast::<u32>().write(0);
            b.add(16).cast::<u32>().write(1);
            c.add(16).cast::<u32>().write(2);
        }
        set(a, 24, x as usize);
        set(b, 24, y as usize); // scalar bits under variant 1
        set(c, 32, z as usize + 20); // `str` into `z`
        // SAFETY: frame layout matches `Frame` + three slots.
        unsafe {
            let (mut ra, mut rb, mut rc) = (a as usize, b as usize, c as usize);
            let mut frame: (Frame, *mut usize, *mut usize, *mut usize) = (
                Frame {
                    prev: ptr::null_mut(),
                    count: 3,
                },
                &mut ra,
                &mut rb,
                &mut rc,
            );
            let top = nabir_common::tls::shadow_stack_top();
            *top = (&raw mut frame).cast();
            collect();
            let x2 = word(a, 24) as *mut u8;
            assert_ne!(x2, x);
            assert_eq!(word(x2, 16), 0xA);
            let z2 = (word(c, 32) - 20) as *mut u8;
            assert_ne!(z2, z);
            assert_eq!(word(z2, 16), 0xC);
            assert_eq!(word(b, 24), y as usize);
            assert_eq!(object_in::<false>(y as usize, false, true), None);
            assert_eq!(live_bytes(), base + 40 * 3 + 24 + 32);
            *frame.1 = 0;
            *frame.2 = 0;
            *frame.3 = 0;
            collect();
            assert_eq!(live_bytes(), base);
            *top = ptr::null_mut();
        }
        heap().evac_always = false;
    }

    /// Default mode: after most of a block-filling population dies, the
    /// sparse blocks are emptied into one and the rest freed.
    fn evacuation_defragments_sparse_blocks() {
        static R: [u32; 4] = [8, 1 | TABLE_ARRAY as u32 | (1 << 16), 0, STRONG_REF];
        let (tr, leaf) = (USER_TYPE_BASE + 12, USER_TYPE_BASE + 1);
        register_type(tr, R.as_ptr().cast());
        assert!(!heap().evac_always);
        collect();
        let base = live_bytes();
        const N: usize = 4096;
        let r = alloc(16 + 8 * N, 8, tr);
        let used = || {
            // SAFETY: `nblocks` initialized metas.
            unsafe { core::slice::from_raw_parts(heap().meta, heap().nblocks) }
                .iter()
                .filter(|m| m.used)
                .count()
        };
        // SAFETY: as above.
        unsafe {
            let mut root = r as usize;
            let mut frame: (Frame, *mut usize) = (
                Frame {
                    prev: ptr::null_mut(),
                    count: 1,
                },
                &mut root,
            );
            let top = nabir_common::tls::shadow_stack_top();
            *top = (&raw mut frame).cast();
            let before = used();
            for i in 0..N {
                let o = alloc(64, 8, leaf);
                set(o, 16, i);
                set(r, 16 + 8 * i, o as usize);
            }
            collect();
            let full = used();
            assert!(full >= before + N * 64 / BLOCK_SIZE, "{before} -> {full}");
            for i in (0..N).filter(|i| i % 16 != 0) {
                set(r, 16 + 8 * i, 0);
            }
            let moved = heap().moved_bytes;
            collect(); // sweeps the dead: blocks 1/16 live
            collect(); // evacuates them
            assert!(heap().moved_bytes - moved >= (N / 16 * 64) as u64);
            assert!(used() <= before + 2, "{before} -> {full} -> {}", used());
            for i in (0..N).step_by(16) {
                assert_eq!(word(word(r, 16 + 8 * i) as *mut u8, 16), i);
            }
            assert_eq!(live_bytes(), base + 16 + 8 * N + N / 16 * 64);
            *frame.1 = 0;
            collect();
            *top = ptr::null_mut();
        }
    }

    /// Generational: a young object reachable only through an old one
    /// (stored behind the write barrier) survives minor collections;
    /// unreachable young objects do not.
    #[allow(clippy::cast_ptr_alignment)] // GC objects are 8-aligned
    fn minor_collection_keeps_logged_children() {
        let base = live_bytes();
        let old = alloc(32, 8, USER_TYPE_BASE);
        // SAFETY: as above.
        unsafe {
            let mut root = old as usize;
            let mut frame: (Frame, *mut usize) = (
                Frame {
                    prev: ptr::null_mut(),
                    count: 1,
                },
                &mut root,
            );
            let top = nabir_common::tls::shadow_stack_top();
            *top = (&raw mut frame).cast();
            collect();
            let flags = (*header(old)).gc_flags.load(Ordering::Relaxed);
            assert_eq!(
                flags & (FLAG_STICKY | FLAG_UNLOGGED),
                FLAG_STICKY | FLAG_UNLOGGED
            );
            let young = alloc(24, 8, USER_TYPE_BASE + 1);
            let garbage = alloc(24, 8, USER_TYPE_BASE + 1);
            write_barrier(old, old.add(16));
            *old.add(16).cast::<*mut u8>() = young;
            let minors = heap().minors;
            collect_young();
            collect_young();
            assert_eq!(heap().minors, minors + 2);
            assert_eq!((*header(young)).type_id, USER_TYPE_BASE + 1);
            assert_eq!(object_in::<false>(garbage as usize, false, true), None);
            assert_eq!(live_bytes(), base + 32 + 24);
            *old.add(16).cast::<*mut u8>() = ptr::null_mut();
            *frame.1 = 0;
            collect();
            assert_eq!(live_bytes(), base);
            *top = ptr::null_mut();
        }
    }

    /// Generational: an old LOS buffer is carded; a young object stored
    /// into it (behind the barrier) survives minors through its card
    /// alone; once overwritten it dies at the next minor.
    #[allow(clippy::cast_ptr_alignment)] // GC objects are 8-aligned
    fn minor_collection_scans_dirty_cards() {
        let base = live_bytes();
        let size = 64 << 10;
        let buf = alloc(size, 8, SCAN_WORDS_TYPE_ID);
        // SAFETY: as above.
        unsafe {
            let mut root = buf as usize;
            let mut frame: (Frame, *mut usize) = (
                Frame {
                    prev: ptr::null_mut(),
                    count: 1,
                },
                &mut root,
            );
            let top = nabir_common::tls::shadow_stack_top();
            *top = (&raw mut frame).cast();
            collect();
            let flags = (*header(buf)).gc_flags.load(Ordering::Relaxed);
            assert_eq!(
                flags & (FLAG_STICKY | FLAG_CARDED),
                FLAG_STICKY | FLAG_CARDED
            );
            let young = alloc(24, 8, USER_TYPE_BASE + 1);
            let slot = buf.add(40_000);
            write_barrier(buf, slot);
            *slot.cast::<*mut u8>() = young;
            assert_eq!(*(*prefix(buf)).cards.add(40_000 / CARD_SIZE), 1);
            collect_young();
            assert_eq!((*header(young)).type_id, USER_TYPE_BASE + 1);
            assert_eq!(live_bytes(), base + size + 24);
            let young2 = alloc(24, 8, USER_TYPE_BASE + 1);
            write_barrier(buf, slot);
            *slot.cast::<*mut u8>() = young2;
            collect_young();
            // `young` is old now (it survived a minor): only a full
            // collection frees it.
            assert_eq!(live_bytes(), base + size + 48);
            collect();
            assert_eq!(live_bytes(), base + size + 24);
            *frame.1 = 0;
            collect();
            assert_eq!(live_bytes(), base);
            *top = ptr::null_mut();
        }
    }

    // Called from the test above: the heap is a single-mutator global,
    // so GC tests must not run on parallel test threads.
    #[allow(clippy::cast_ptr_alignment)] // GC objects are 8-aligned
    fn scan_words_buffers_and_interior_pointers() {
        let base = live_bytes();
        let buf = alloc(16 + 16, 8, SCAN_WORDS_TYPE_ID);
        let s = alloc(24, 8, USER_TYPE_BASE + 7);
        // SAFETY: as above; the buffer holds an interior pointer into `s`
        // and a non-pointer word; only the buffer's payload is rooted.
        unsafe {
            *buf.add(16).cast::<usize>() = 0x99;
            *buf.add(24).cast::<*mut u8>() = s.add(18);
            let mut root = buf as usize + 20; // interior root
            let mut frame: (Frame, *mut usize) = (
                Frame {
                    prev: ptr::null_mut(),
                    count: 1,
                },
                &mut root,
            );
            let top = nabir_common::tls::shadow_stack_top();
            *top = (&raw mut frame).cast();
            collect();
            assert_eq!(live_bytes(), base + 32 + 24);
            *frame.1 = 0;
            collect();
            assert_eq!(live_bytes(), base);
            *top = ptr::null_mut();
        }
    }

    /// Medium objects (several lines) and LOS objects are found through
    /// interior pointers and freed when unreachable.
    fn medium_and_large_objects() {
        let base = live_bytes();
        let medium = alloc(1000, 8, USER_TYPE_BASE + 1);
        let large = alloc(LOS_THRESHOLD + 8, 8, USER_TYPE_BASE + 1);
        // SAFETY: as above.
        unsafe {
            assert_ne!(
                (*header(large)).gc_flags.load(Ordering::Relaxed) & FLAG_LARGE,
                0
            );
            let mut r1 = medium as usize + 900;
            let mut r2 = large as usize + 5000;
            let mut frame: (Frame, *mut usize, *mut usize) = (
                Frame {
                    prev: ptr::null_mut(),
                    count: 2,
                },
                &mut r1,
                &mut r2,
            );
            let top = nabir_common::tls::shadow_stack_top();
            *top = (&raw mut frame).cast();
            collect();
            assert_eq!(live_bytes(), base + 1000 + LOS_THRESHOLD + 8);
            *frame.1 = 0;
            *frame.2 = 0;
            collect();
            assert_eq!(live_bytes(), base);
            *top = ptr::null_mut();
        }
        // A large payload starts on a cache line, for vector code.
        let l = alloc(LOS_THRESHOLD + 24, 8, PLAIN_TYPE_ID);
        assert_eq!((l as usize + HEADER_SIZE) % LOS_ALIGN, 0);
        // An over-aligned payload starts on its alignment.
        for align in [16, 32, 64, 4096] {
            let o = alloc(40, align, USER_TYPE_BASE + 1);
            assert_eq!((o as usize + HEADER_SIZE) % align, 0);
        }
        collect();
        assert_eq!(live_bytes(), base);
        // A big one has pages of its own, gone once it dies.
        let big = alloc(1 << 20, 8, USER_TYPE_BASE + 1);
        // SAFETY: the object and its prefix are live until the collection.
        let at = unsafe { big.sub((*prefix(big)).offset) };
        assert_eq!(at as usize % 4096, 0);
        collect();
        assert_eq!(live_bytes(), base);
        #[cfg(unix)]
        {
            // SAFETY: msync only queries the range; ENOMEM: not mapped.
            let rc = unsafe { libc::msync(at.cast(), 4096, libc::MS_ASYNC) };
            assert_eq!(rc, -1);
        }
    }

    /// After a collection the dead objects' lines are handed out again
    /// (zeroed) instead of growing the heap.
    fn free_lines_are_reused() {
        collect();
        let blocks = heap().nblocks;
        for _ in 0..3 {
            for _ in 0..(4 * BLOCK_SIZE / 64) {
                let p = alloc(64, 8, USER_TYPE_BASE + 1);
                // SAFETY: a fresh 64-byte object.
                unsafe {
                    assert_eq!(*p.add(40), 0);
                    *p.add(40) = 1;
                }
            }
            collect();
        }
        assert!(heap().nblocks <= blocks + 5, "{} blocks", heap().nblocks);
    }
}
