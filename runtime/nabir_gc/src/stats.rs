//! Statistics and heap dumps.

#[allow(clippy::wildcard_imports)]
use crate::*;

/// `core::fmt` to stderr.
pub(crate) struct Stderr;

impl core::fmt::Write for Stderr {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        // SAFETY: `s` is valid for its own length.
        unsafe { nabir_common::write_stderr(s.as_ptr(), s.len()) };
        Ok(())
    }
}

/// The kinds of pause, as `NABIR_GC_LOG` and `NABIR_GC_STATS` name them.
#[derive(Clone, Copy)]
pub(crate) enum Kind {
    Minor,
    /// A full collection that stops the world.
    Full,
    /// A concurrent mark's first pause (a minor collection).
    Start,
    /// A concurrent mark's final pause.
    Finish,
}

impl Kind {
    const ALL: [Self; 4] = [Self::Minor, Self::Full, Self::Start, Self::Finish];

    /// The `NABIR_GC_LOG` line's name.
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Minor => "minor",
            Self::Full => "full",
            Self::Start => "minor, concurrent full mark started",
            Self::Finish => "full, concurrent",
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Minor => "minor",
            Self::Full => "full",
            Self::Start => "concurrent start",
            Self::Finish => "concurrent finish",
        }
    }

    /// Part of a full collection (`NABIR_GC_STATS` full pause totals).
    pub(crate) const fn full(self) -> bool {
        !matches!(self, Self::Minor)
    }
}

/// Buckets of [`Hist`]: 8 per power of two of microseconds.
const BUCKETS: usize = 512;

/// The pauses of one kind: count, totals, longest, and a log-linear
/// histogram of microseconds, so a percentile is within an eighth.
pub(crate) struct Hist {
    n: u64,
    total_ns: u64,
    max_ns: u64,
    /// Time to safepoint: from the stop request until every other
    /// thread stood still.
    ttsp_ns: u64,
    ttsp_max_ns: u64,
    buckets: [u32; BUCKETS],
}

impl Hist {
    pub(crate) const NEW: Self = Self {
        n: 0,
        total_ns: 0,
        max_ns: 0,
        ttsp_ns: 0,
        ttsp_max_ns: 0,
        buckets: [0; BUCKETS],
    };

    const fn bucket(us: u64) -> usize {
        if us < 8 {
            return us as usize;
        }
        let e = 63 - us.leading_zeros() as usize;
        (e - 2) * 8 + ((us >> (e - 3)) & 7) as usize
    }

    /// The upper end of bucket `b`, in microseconds.
    const fn bucket_end(b: usize) -> u64 {
        if b < 8 {
            return b as u64 + 1;
        }
        let (e, sub) = (b / 8 + 2, b % 8);
        (9 + sub as u64) << (e - 3)
    }

    pub(crate) fn add(&mut self, ns: u64, ttsp: u64) {
        self.n += 1;
        self.total_ns += ns;
        self.max_ns = self.max_ns.max(ns);
        self.ttsp_ns += ttsp;
        self.ttsp_max_ns = self.ttsp_max_ns.max(ttsp);
        let b = Self::bucket(ns / 1000).min(BUCKETS - 1);
        self.buckets[b] = self.buckets[b].saturating_add(1);
    }

    /// The `p`th percentile in nanoseconds: the end of its bucket, at
    /// most the longest pause.
    pub(crate) fn percentile(&self, p: u64) -> u64 {
        let rank = (self.n * p).div_ceil(100).max(1);
        let mut seen = 0;
        for (b, &c) in self.buckets.iter().enumerate() {
            seen += u64::from(c);
            if seen >= rank {
                return (Self::bucket_end(b) * 1000).min(self.max_ns);
            }
        }
        self.max_ns
    }
}

/// The parts of a pause `NABIR_GC_LOG` times.
#[derive(Clone, Copy)]
pub(crate) enum Phase {
    /// Roots, remembered objects, dirty cards, the mark itself.
    Mark,
    /// Blocks and large objects.
    Sweep,
    /// Copying and fixing references.
    Evac,
}

/// The running pause's phase times; `epoch`: when the collector was
/// configured (the log's timestamps count from it).
pub(crate) struct Phases {
    pub(crate) epoch: u64,
    pub(crate) at: u64,
    pub(crate) ns: [u64; 3],
}

impl Phases {
    pub(crate) const NEW: Self = Self {
        epoch: 0,
        at: 0,
        ns: [0; 3],
    };
}

/// The running pause did `phase` since the previous phase ended.
pub(crate) fn lap(phase: Phase) {
    let p = &mut heap().phases;
    let t = now_ns();
    p.ns[phase as usize] += t - p.at;
    p.at = t;
}

/// End of a pause of `kind`: `ns` long, `ttsp` of it until the world
/// stood still.
pub(crate) fn record(kind: Kind, ns: u64, ttsp: u64) {
    heap().pauses[kind as usize].add(ns, ttsp);
}

/// `ns` as `ms.us` for the stats lines.
struct Ms(u64);

impl core::fmt::Display for Ms {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}.{:03}", self.0 / 1_000_000, self.0 / 1000 % 1000)
    }
}

/// The `nabi_gc_stats` words, in order.
pub const STATS_WORDS: usize = 13;

/// The counters of `bare::gc::stats`.
///
/// In order: collections (all, minor, full stop-the-world, concurrent
/// marks), total and longest pause (ns), live bytes, heap bytes (blocks
/// held from the OS plus large objects), bytes moved by evacuation, mark
/// assists, bytes no-collect regions grew the heap by, collections the
/// heap limit caused, the young budget now.
pub fn stats(out: &mut [u64; STATS_WORDS]) {
    world::lock();
    let h = heap();
    let metas = if h.nblocks == 0 {
        &[][..]
    } else {
        // SAFETY: `nblocks` initialized metas.
        unsafe { core::slice::from_raw_parts(h.meta, h.nblocks) }
    };
    let resident = metas.iter().filter(|m| !m.released).count() * BLOCK_SIZE;
    let mut los = 0;
    let mut o = h.los;
    while !o.is_null() {
        // SAFETY: walks the LOS list.
        unsafe {
            los += (*prefix(o)).size;
            o = (*prefix(o)).next;
        }
    }
    *out = [
        h.collections,
        h.minors,
        h.pauses[Kind::Full as usize].n,
        h.cycles,
        h.gc_ns,
        h.max_ns,
        (h.live_bytes + young_bytes()) as u64,
        (resident + los) as u64,
        h.moved_bytes,
        h.assists,
        h.grown_bytes,
        h.limit_collections,
        h.next_gc.saturating_sub(h.old_bytes) as u64,
    ];
    world::unlock();
}

pub(crate) extern "C" fn print_stats() {
    world::lock_at_exit();
    let h = heap();
    let _ = writeln!(
        Stderr,
        "nabi gc: {} collections ({} minor), {}.{:03} ms total pause, max {}.{:03} ms, {}.{:03} ms cpu, {} KiB live",
        h.collections,
        h.minors,
        h.gc_ns / 1_000_000,
        h.gc_ns / 1000 % 1000,
        h.max_ns / 1_000_000,
        h.max_ns / 1000 % 1000,
        h.gc_cpu_ns / 1_000_000,
        h.gc_cpu_ns / 1000 % 1000,
        h.old_bytes >> 10
    );
    // SAFETY: `nblocks` initialized metas.
    let metas = unsafe { core::slice::from_raw_parts(h.meta, h.nblocks) };
    let released = metas.iter().filter(|m| m.released).count();
    let _ = writeln!(
        Stderr,
        "nabi gc: {} blocks ({} KiB resident), {} evacuations moved {} KiB",
        h.nblocks,
        ((h.nblocks - released) * BLOCK_SIZE) >> 10,
        h.evacuations,
        h.moved_bytes >> 10
    );
    let ms = |ns: u64| (ns / 1_000_000, ns / 1000 % 1000);
    let ((ft, ftu), (fm, fmu)) = (ms(h.full_ns), ms(h.full_max_ns));
    let ((ct, ctu), (cm, cmu)) = (ms(h.full_cpu_ns), ms(h.full_cpu_max_ns));
    let (mc, mcu) = ms(MARKER_CPU_NS.load(Ordering::Relaxed));
    let _ = writeln!(
        Stderr,
        "nabi gc: {} full pauses, {ft}.{ftu:03} ms total, max {fm}.{fmu:03} ms ({ct}.{ctu:03} / {cm}.{cmu:03} ms cpu); {} concurrent marks ({} finished by allocation), {mc}.{mcu:03} ms marker cpu, {} mark assists",
        h.full_pauses, h.cycles, h.forced, h.assists
    );
    for k in Kind::ALL {
        let p = &h.pauses[k as usize];
        if p.n == 0 {
            continue;
        }
        let _ = writeln!(
            Stderr,
            "nabi gc: {} pauses: {}, p50 {} ms, p90 {} ms, p99 {} ms, max {} ms, total {} ms; time to safepoint max {} ms, total {} ms",
            k.name(),
            p.n,
            Ms(p.percentile(50)),
            Ms(p.percentile(90)),
            Ms(p.percentile(99)),
            Ms(p.max_ns),
            Ms(p.total_ns),
            Ms(p.ttsp_max_ns),
            Ms(p.ttsp_ns)
        );
    }
    world::unlock();
}

/// Types [`heap_dump`] tells apart; the rest count as one.
pub(crate) const DUMP_TYPES: usize = 512;

/// Types and large objects [`heap_dump`] lists.
pub(crate) const DUMP_LINES: usize = 32;

/// [`heap_dump`]'s tables (it must not allocate).
pub(crate) struct Dump {
    /// `(type_id, objects, bytes)` per type; the last entry (type id 0)
    /// counts the types that did not fit.
    types: [(u32, u64, u64); DUMP_TYPES + 1],
    /// The largest LOS objects, `(bytes, type_id)`.
    large: [(usize, u32); DUMP_LINES],
}

pub(crate) static DUMP: Global<Dump> = Global(UnsafeCell::new(Dump {
    types: [(0, 0, 0); DUMP_TYPES + 1],
    large: [(0, 0); DUMP_LINES],
}));

/// `NABIR_HEAP_DUMP=on_oom`: the object count and bytes of each type (most
/// bytes first) and the largest LOS objects, to
/// stderr. Runs on the failing thread without taking the world lock (it
/// may hold it): other threads' newest objects may be missed.
pub(crate) fn heap_dump() {
    // SAFETY: called once (`oom` takes the hook); best effort over the
    // heap as it stands.
    let Dump { types, large } = unsafe { &mut *DUMP.0.get() };
    let mut count = |type_id: u32, size: usize| {
        let mut i = type_id as usize % DUMP_TYPES;
        for _ in 0..DUMP_TYPES {
            if types[i].0 == type_id || types[i].1 == 0 {
                break;
            }
            i = (i + 1) % DUMP_TYPES;
        }
        if types[i].1 != 0 && types[i].0 != type_id {
            i = DUMP_TYPES; // type id 0: the rest
        } else {
            types[i].0 = type_id;
        }
        types[i].1 += 1;
        types[i].2 += size as u64;
    };
    let h = heap();
    let (mut objects, mut used) = (0usize, 0usize);
    for b in 0..h.nblocks {
        // SAFETY: `b < nblocks`.
        let m = unsafe { &*h.meta.add(b) };
        if !m.used {
            continue;
        }
        used += 1;
        let block = h.base + b * BLOCK_SIZE;
        // Survivors by their start bits; what was handed out since, by
        // parsing (a header being written may read as 0: skipped).
        for_each_start(&m.lines, &m.starts, true, |off| {
            if line(m, off / LINE_SIZE) == LIVE {
                let o = (block + off) as *mut u8;
                // SAFETY: a survivor's header.
                count(unsafe { (*header(o)).type_id }, obj_size(o));
                objects += 1;
            }
        });
        let mut off = 0;
        while off + HEADER_SIZE <= BLOCK_SIZE {
            let state = line(m, off / LINE_SIZE);
            if state != TAKEN && state != BLACK {
                off = (off / LINE_SIZE + 1) * LINE_SIZE;
                continue;
            }
            let o = (block + off) as *mut u8;
            // SAFETY: inside a handed-out line: zeroes or headers.
            let (type_id, size) = unsafe { ((*header(o)).type_id, obj_size(o)) };
            if type_id == 0 || size < HEADER_SIZE || size % 8 != 0 || off + size > BLOCK_SIZE {
                off += 8;
                continue;
            }
            count(type_id, size);
            objects += 1;
            off += size;
        }
    }
    let (mut los, mut los_bytes) = (0usize, 0usize);
    let mut o = h.los;
    while !o.is_null() {
        // SAFETY: walks the LOS list.
        let (size, type_id, next) =
            unsafe { ((*prefix(o)).size, (*header(o)).type_id, (*prefix(o)).next) };
        count(type_id, size);
        los += 1;
        los_bytes += size;
        if let Some(i) = large.iter().position(|&(s, _)| s < size) {
            large.copy_within(i..DUMP_LINES - 1, i + 1);
            large[i] = (size, type_id);
        }
        o = next;
    }
    types.sort_unstable_by(|a, b| b.2.cmp(&a.2));
    let _ = writeln!(
        Stderr,
        "nabi heap dump: {used} blocks ({} KiB), {objects} objects in them; {los} large objects ({} KiB)",
        (used * BLOCK_SIZE) >> 10,
        los_bytes >> 10
    );
    for &(type_id, n, bytes) in types.iter().take(DUMP_LINES).filter(|t| t.1 != 0) {
        if type_id == 0 {
            let _ = writeln!(
                Stderr,
                "nabi heap dump: other types: {n} objects, {bytes} bytes"
            );
        } else {
            let _ = writeln!(
                Stderr,
                "nabi heap dump: type {type_id:#x}: {n} objects, {bytes} bytes"
            );
        }
    }
    for &(size, type_id) in large.iter().filter(|l| l.0 != 0) {
        let _ = writeln!(
            Stderr,
            "nabi heap dump: large object of type {type_id:#x}: {size} bytes"
        );
    }
}

pub(crate) fn now_ns() -> u64 {
    sys::monotonic_ns() as u64
}
