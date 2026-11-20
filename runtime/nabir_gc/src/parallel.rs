//! Parallel marking in stop-the-world pauses: worker threads drain the
//! mark stack beside the collector. Each marks from a stack of its own
//! and shares with the others through one locked pool (the collector's
//! stack, handed over for the phase); the mark ends when the pool is
//! empty and every marker idles. Marking itself is already safe to
//! run on several threads: mark bits are set with `fetch_or`, start
//! bits atomically, and a block's lines are parsed by one marker while
//! the others wait ([`crate::object_in`]).

#[allow(clippy::wildcard_imports)]
use crate::*;

/// Marking threads besides the collector, at most.
const WORKERS_MAX: usize = 7;
/// Queued objects from which a pause marks in parallel (fewer:
/// waking the workers costs more than it saves).
const PARALLEL_MIN: usize = 8192;
/// The same under `NABIR_GC_STRESS`, so tests mark in parallel often.
const PARALLEL_MIN_STRESS: usize = 64;
/// Objects a marker takes from the pool at a time, and the length of
/// its own stack past which it gives half to an idle one.
const GRAB: usize = 1024;
const SHARE: usize = 4096;

/// A marker's own stack, counts and lists.
pub(crate) struct Worker {
    stack: *mut *mut u8,
    len: usize,
    cap: usize,
    marked: usize,
    marked_bytes: usize,
    pinned: *mut usize,
    pinned_len: usize,
    pinned_cap: usize,
    weak: *mut usize,
    weak_len: usize,
    weak_cap: usize,
}

impl Worker {
    const NEW: Self = Self {
        stack: ptr::null_mut(),
        len: 0,
        cap: 0,
        marked: 0,
        marked_bytes: 0,
        pinned: ptr::null_mut(),
        pinned_len: 0,
        pinned_cap: 0,
        weak: ptr::null_mut(),
        weak_len: 0,
        weak_cap: 0,
    };

    fn push(&mut self, o: *mut u8) {
        if self.len == self.cap {
            let cap = (self.cap * 2).max(1024);
            // SAFETY: realloc of our own stack (or null).
            let p = unsafe { libc::realloc(self.stack.cast(), cap * size_of::<*mut u8>()) };
            if p.is_null() {
                nabir_common::oom();
            }
            self.stack = p.cast();
            self.cap = cap;
        }
        // SAFETY: `len < cap`.
        unsafe { *self.stack.add(self.len) = o };
        self.len += 1;
    }

    /// Give the older half of the stack to the pool.
    fn share(&mut self) {
        let half = self.len / 2;
        POOL_LOCK.lock();
        let p = pool();
        for i in 0..half {
            // SAFETY: entries below `len`.
            p.push(unsafe { *self.stack.add(i) });
        }
        POOL_COND.broadcast();
        POOL_LOCK.unlock();
        // SAFETY: entries `half..len` move down.
        unsafe { ptr::copy(self.stack.add(half), self.stack, self.len - half) };
        self.len -= half;
    }
}

impl Out for Worker {
    fn obj(&mut self, o: *mut u8) {
        self.push(o);
        if self.len > SHARE && IDLE.load(Ordering::Relaxed) > 0 {
            self.share();
        }
    }

    fn pinned(&mut self, o: usize) {
        append(
            &mut self.pinned,
            &mut self.pinned_len,
            &mut self.pinned_cap,
            o,
        );
    }

    fn weak(&mut self, at: usize) {
        append(&mut self.weak, &mut self.weak_len, &mut self.weak_cap, at);
    }

    fn pop(&mut self) -> Option<*mut u8> {
        if self.len == 0 {
            return None;
        }
        self.len -= 1;
        // SAFETY: entries below `len` are queued objects.
        Some(unsafe { *self.stack.add(self.len) })
    }

    fn marked(&mut self, size: usize) {
        self.marked += 1;
        self.marked_bytes += size;
    }
}

/// The shared pool, guarded by [`POOL_LOCK`]: the collector's mark stack
/// for the phase; how many markers joined it, and how many of those
/// idle; the workers still inside it; whether it ended (a worker that
/// wakes late stays out). The phase ends once every marker that joined
/// idles with the pool empty, so a worker the OS runs late delays
/// nothing.
struct Pool {
    stack: *mut *mut u8,
    len: usize,
    cap: usize,
    active: usize,
    idle: usize,
    inside: usize,
    closed: bool,
}

impl Pool {
    fn push(&mut self, o: *mut u8) {
        if self.len == self.cap {
            let cap = (self.cap * 2).max(1024);
            // SAFETY: realloc of the pool's stack (or null).
            let p = unsafe { libc::realloc(self.stack.cast(), cap * size_of::<*mut u8>()) };
            if p.is_null() {
                nabir_common::oom();
            }
            self.stack = p.cast();
            self.cap = cap;
        }
        // SAFETY: `len < cap`.
        unsafe { *self.stack.add(self.len) = o };
        self.len += 1;
    }
}

static POOL: Global<Pool> = Global(UnsafeCell::new(Pool {
    stack: ptr::null_mut(),
    len: 0,
    cap: 0,
    active: 0,
    idle: 0,
    inside: 0,
    closed: true,
}));
static POOL_LOCK: sys::Lock = sys::Lock::new();
/// Idle markers sleep on it until work is shared or the phase ends.
static POOL_COND: sys::Cond = sys::Cond::new();
/// `Pool::idle`, for a busy marker to see without the lock.
static IDLE: AtomicUsize = AtomicUsize::new(0);

fn pool() -> &'static mut Pool {
    // SAFETY: callers hold `POOL_LOCK` (or are the collector between
    // phases); no caller keeps the reference across another call.
    unsafe { &mut *POOL.0.get() }
}

/// The markers' slots: 0 is the collector's, `1..` the workers'. Each
/// thread touches only its own during a phase; the collector merges
/// them after it.
static WORKERS: Global<[Worker; WORKERS_MAX + 1]> =
    Global(UnsafeCell::new([const { Worker::NEW }; WORKERS_MAX + 1]));

fn slot(i: usize) -> &'static mut Worker {
    // SAFETY: slot `i` belongs to one thread at a time (see `WORKERS`).
    unsafe { &mut (*WORKERS.0.get())[i] }
}

/// Phases so far (a worker joins the newest when it wakes), workers
/// started.
static PHASE: AtomicUsize = AtomicUsize::new(0);
static READY: AtomicUsize = AtomicUsize::new(0);
static NEXT: AtomicUsize = AtomicUsize::new(0);
/// Workers there are (0: not started yet, or none on this machine).
static COUNT: AtomicUsize = AtomicUsize::new(usize::MAX);
static WAKE_LOCK: sys::Lock = sys::Lock::new();
static WAKE: sys::Cond = sys::Cond::new();

/// Online CPUs, at least 1.
fn cpus() -> usize {
    #[cfg(unix)]
    // SAFETY: plain sysconf query.
    let n = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) };
    #[cfg(windows)]
    // SAFETY: plain query; 0xFFFF = every processor group.
    let n = unsafe { nabir_common::win::GetActiveProcessorCount(0xFFFF) };
    #[cfg(not(any(unix, windows)))]
    let n = 1;
    usize::try_from(n).unwrap_or(1).max(1)
}

/// The workers, started on first use: `NABIR_GC_WORKERS`, else one per
/// CPU beyond the collector's; at most [`WORKERS_MAX`], none where
/// threads cannot run.
fn workers() -> usize {
    let n = COUNT.load(Ordering::Acquire);
    if n != usize::MAX {
        return n;
    }
    let want = if cfg!(any(nabi_solo, nabi_flat)) {
        0
    } else {
        env_num(c"NABIR_GC_WORKERS", false)
            .map_or_else(|| cpus() - 1, |n| n as usize)
            .min(WORKERS_MAX)
    };
    let mut started = 0;
    while started < want && sys::spawn_detached(worker) {
        started += 1;
    }
    while READY.load(Ordering::Acquire) < started {
        sys::yield_now();
    }
    COUNT.store(started, Ordering::Release);
    started
}

/// A worker thread: marks in every phase from its start on.
extern "C" fn worker() {
    let i = NEXT.fetch_add(1, Ordering::Relaxed) + 1;
    WAKE_LOCK.lock();
    let mut seen = PHASE.load(Ordering::Relaxed);
    READY.fetch_add(1, Ordering::Release);
    loop {
        while PHASE.load(Ordering::Relaxed) == seen {
            WAKE.wait(&WAKE_LOCK);
        }
        seen = PHASE.load(Ordering::Relaxed);
        WAKE_LOCK.unlock();
        POOL_LOCK.lock();
        let p = pool();
        let join = !p.closed;
        if join {
            p.active += 1;
            p.inside += 1;
        }
        POOL_LOCK.unlock();
        if join {
            work(slot(i));
            POOL_LOCK.lock();
            pool().inside -= 1;
            POOL_LOCK.unlock();
        }
        WAKE_LOCK.lock();
    }
}

/// Mark until the pool is empty and every marker idles.
fn work(w: &mut Worker) {
    loop {
        drain_into(w, usize::MAX);
        POOL_LOCK.lock();
        let p = pool();
        if p.len == 0 {
            p.idle += 1;
            IDLE.store(p.idle, Ordering::Relaxed);
            while p.len == 0 {
                if p.closed || p.idle == p.active {
                    p.closed = true;
                    POOL_COND.broadcast();
                    POOL_LOCK.unlock();
                    return;
                }
                POOL_COND.wait(&POOL_LOCK);
            }
            p.idle -= 1;
            IDLE.store(p.idle, Ordering::Relaxed);
        }
        let take = p.len.min(GRAB);
        for _ in 0..take {
            p.len -= 1;
            // SAFETY: entries below `len` are queued objects.
            w.push(unsafe { *p.stack.add(p.len) });
        }
        POOL_LOCK.unlock();
    }
}

/// In a stop-the-world pause with the mark lock held: [`drain`] the
/// mark stack, with the workers once it holds enough to share.
#[allow(clippy::many_single_char_names)]
pub(crate) fn drain_parallel() {
    let min = if heap().stress != 0 {
        PARALLEL_MIN_STRESS
    } else {
        PARALLEL_MIN
    };
    loop {
        if mark().len == 0 {
            return;
        }
        if mark().len >= min {
            break;
        }
        drain(256);
    }
    let n = workers();
    if n == 0 {
        return drain(usize::MAX);
    }
    // A worker of the last phase may still be on its way out.
    loop {
        POOL_LOCK.lock();
        let left = pool().inside;
        POOL_LOCK.unlock();
        if left == 0 {
            break;
        }
        sys::yield_now();
    }
    // The collector's stack becomes the pool for the phase.
    let m = mark();
    POOL_LOCK.lock();
    let p = pool();
    (p.stack, p.len, p.cap) = (m.stack, m.len, m.stack_cap);
    (p.active, p.idle, p.inside, p.closed) = (1, 0, 0, false);
    POOL_LOCK.unlock();
    (m.stack, m.len, m.stack_cap) = (ptr::null_mut(), 0, 0);
    IDLE.store(0, Ordering::Relaxed);
    WAKE_LOCK.lock();
    PHASE.fetch_add(1, Ordering::Release);
    WAKE.broadcast();
    WAKE_LOCK.unlock();
    work(slot(0));
    // Every marker that joined idles now: its slot is final, though it
    // may not have left `work` yet (the next phase waits for that).
    let m = mark();
    let p = pool();
    (m.stack, m.len, m.stack_cap) = (p.stack, 0, p.cap);
    (p.stack, p.len, p.cap) = (ptr::null_mut(), 0, 0);
    for i in 0..=n {
        let w = slot(i);
        m.marked += w.marked;
        m.marked_bytes += w.marked_bytes;
        for k in 0..w.pinned_len {
            // SAFETY: entries below `pinned_len`.
            let o = unsafe { *w.pinned.add(k) };
            append(&mut m.pinned, &mut m.pinned_len, &mut m.pinned_cap, o);
        }
        for k in 0..w.weak_len {
            // SAFETY: entries below `weak_len`.
            let at = unsafe { *w.weak.add(k) };
            append(&mut m.weak, &mut m.weak_len, &mut m.weak_cap, at);
        }
        (w.marked, w.marked_bytes, w.pinned_len, w.weak_len) = (0, 0, 0, 0);
    }
}
