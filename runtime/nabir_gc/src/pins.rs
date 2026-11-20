//! Pins, weak cells and objects that never move.

#[allow(clippy::wildcard_imports)]
use crate::*;

/// Pinned objects (`nabi_gc_pin`, `FLAG_PINNED`): roots until unpinned.
/// A table, so a collection does not visit every header looking for the
/// flag: open addressing on the object's address, each slot `(object,
/// pin count)`, an empty slot's object 0. Pin and unpin are O(1).
pub(crate) struct Pins {
    lock: sys::Lock,
    pub(crate) table: UnsafeCell<PinTable>,
}

pub(crate) struct PinTable {
    slots: *mut (usize, usize),
    /// Objects pinned (used slots).
    len: usize,
    /// Slots, a power of two (or 0).
    cap: usize,
}

impl PinTable {
    /// Every pinned object.
    pub(crate) fn objects(&self) -> impl Iterator<Item = *mut u8> + '_ {
        (0..self.cap)
            // SAFETY: `cap` initialized slots.
            .map(|i| unsafe { (*self.slots.add(i)).0 })
            .filter(|&o| o != 0)
            .map(|o| o as *mut u8)
    }

    /// The slot of `obj`, or the empty one it would take. `cap > 0`.
    fn find(&self, obj: usize) -> usize {
        // The address over 8 (alignment), scrambled.
        let mut i = (obj >> 3).wrapping_mul(0x9E37_79B9_7F4A_7C15_u64 as usize) & (self.cap - 1);
        loop {
            // SAFETY: `i < cap`.
            let o = unsafe { (*self.slots.add(i)).0 };
            if o == obj || o == 0 {
                return i;
            }
            i = (i + 1) & (self.cap - 1);
        }
    }

    /// Room for one more object (load at most 3/4).
    fn reserve(&mut self) {
        if (self.len + 1) * 4 <= self.cap * 3 {
            return;
        }
        let old = (self.slots, self.cap);
        self.cap = (self.cap * 2).max(16);
        // SAFETY: plain allocation; zeroed slots are empty.
        self.slots = unsafe { libc::calloc(self.cap, size_of::<(usize, usize)>()) }.cast();
        if self.slots.is_null() {
            nabir_common::oom();
        }
        for k in 0..old.1 {
            // SAFETY: `old.1` initialized slots.
            let e = unsafe { *old.0.add(k) };
            if e.0 != 0 {
                let at = self.find(e.0);
                // SAFETY: `at < cap`.
                unsafe { *self.slots.add(at) = e };
            }
        }
        // SAFETY: our own table (or null).
        unsafe { libc::free(old.0.cast()) };
    }

    /// Empty slot `i`, moving later entries of its run back so every
    /// probe still finds them.
    fn remove(&mut self, mut i: usize) {
        let mask = self.cap - 1;
        let mut j = i;
        loop {
            j = (j + 1) & mask;
            // SAFETY: `j < cap`.
            let e = unsafe { *self.slots.add(j) };
            if e.0 == 0 {
                break;
            }
            let home = (e.0 >> 3).wrapping_mul(0x9E37_79B9_7F4A_7C15_u64 as usize) & mask;
            // `e` may fill `i` unless its home lies cyclically in (i, j].
            if (j.wrapping_sub(home) & mask) >= (j.wrapping_sub(i) & mask) {
                // SAFETY: `i < cap`.
                unsafe { *self.slots.add(i) = e };
                i = j;
            }
        }
        // SAFETY: `i < cap`.
        unsafe { *self.slots.add(i) = (0, 0) };
        self.len -= 1;
    }
}

// SAFETY: `table` is only touched under `lock` (or with the world
// stopped, when no thread is inside `pin` / `unpin`).
unsafe impl Sync for Pins {}

pub(crate) static PINS: Pins = Pins {
    lock: sys::Lock::new(),
    table: UnsafeCell::new(PinTable {
        slots: ptr::null_mut(),
        len: 0,
        cap: 0,
    }),
};

fn with_pins<R>(f: impl FnOnce(&mut PinTable) -> R) -> R {
    PINS.lock.lock();
    // SAFETY: we hold the lock.
    let r = f(unsafe { &mut *PINS.table.get() });
    PINS.lock.unlock();
    r
}

/// Keep `obj` (and what it references) alive and in place until as
/// many [`unpin`]s. Not a safepoint.
///
/// # Safety
/// `obj` must be a live GC object.
pub unsafe fn pin(obj: *mut u8) {
    with_pins(|t| {
        t.reserve();
        let i = t.find(obj as usize);
        // SAFETY: `i < cap`.
        let e = unsafe { &mut *t.slots.add(i) };
        if e.0 == 0 {
            *e = (obj as usize, 0);
            t.len += 1;
            // SAFETY: caller contract.
            unsafe {
                (*header(obj))
                    .gc_flags
                    .fetch_or(FLAG_PINNED, Ordering::Relaxed)
            };
        }
        e.1 += 1;
    });
}

/// Undo one [`pin`]: the object may move again after the last.
///
/// # Safety
/// `obj` must be a live GC object.
pub unsafe fn unpin(obj: *mut u8) {
    with_pins(|t| {
        if t.cap == 0 {
            return;
        }
        let i = t.find(obj as usize);
        // SAFETY: `i < cap`.
        let e = unsafe { &mut *t.slots.add(i) };
        if e.0 == 0 {
            return;
        }
        e.1 -= 1;
        if e.1 == 0 {
            t.remove(i);
            // SAFETY: caller contract.
            unsafe {
                (*header(obj))
                    .gc_flags
                    .fetch_and(!FLAG_PINNED, Ordering::Relaxed)
            };
        }
    });
}

/// Whether `obj` is pinned now (a pinned buffer cannot be replaced by a
/// grown one: C may hold its address).
///
/// # Safety
/// `obj` must be a live GC object.
pub unsafe fn is_pinned(obj: *mut u8) -> bool {
    // SAFETY: caller contract.
    unsafe { (*header(obj)).gc_flags.load(Ordering::Relaxed) & FLAG_PINNED != 0 }
}

/// A new weak cell referring to `obj`.
///
/// The collection that frees `obj` sets the cell's word to 0, one that
/// moves it updates it. The cell is never written again, so (young when
/// written) it needs no barrier. May collect: the caller roots `obj`.
#[allow(clippy::cast_ptr_alignment)] // GC objects are 8-aligned
pub fn weak_new(obj: *mut u8) -> *mut u8 {
    let cell = alloc(24, 8, WEAK_TYPE_ID);
    // SAFETY: a fresh 24-byte object.
    unsafe { cell.add(HEADER_SIZE).cast::<usize>().write(obj as usize) };
    cell
}

/// The referent of the weak cell `cell`, null once it was freed.
///
/// # Safety
/// `cell` must be a live cell from [`weak_new`].
pub unsafe fn weak_get(cell: *mut u8) -> *mut u8 {
    // SAFETY: caller contract.
    let w = load_word(unsafe { cell.add(HEADER_SIZE) });
    // No safepoint between the load and the log: a running mark sees it.
    if w != 0 && concurrent::MARKING.load(Ordering::Relaxed) {
        concurrent::shade(w);
    }
    w as *mut u8
}

/// Whether a collection freed the referent of weak cell `cell` (no
/// shading: the referent is not read).
///
/// # Safety
/// `cell` must be a live cell from [`weak_new`].
pub unsafe fn weak_cleared(cell: *mut u8) -> bool {
    // SAFETY: caller contract.
    load_word(unsafe { cell.add(HEADER_SIZE) }) == 0
}

/// Never move `obj` again (its address became a number: a parking key).
/// It still dies when unreachable.
///
/// # Safety
/// `obj` must be a live GC object.
pub unsafe fn no_move(obj: *mut u8) {
    // SAFETY: caller contract.
    unsafe {
        (*header(obj))
            .gc_flags
            .fetch_or(FLAG_NOMOVE, Ordering::Relaxed)
    };
}
