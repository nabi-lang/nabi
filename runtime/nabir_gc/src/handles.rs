//! Handles: GC objects C code holds as a `void*` (`bare::ffi::Handle`).
//!
//! A handle is a slot of one table, `(index + 1) << GEN_BITS | gen`, never
//! null. The slot pins its object, so the object is a root and keeps its
//! address while the handle lives; a release unpins it. The generation
//! catches a handle used after its last release.

#[allow(clippy::wildcard_imports)]
use crate::*;

#[cfg(target_pointer_width = "64")]
const GEN_BITS: u32 = 16;
#[cfg(not(target_pointer_width = "64"))]
const GEN_BITS: u32 = 8;
const GEN_MASK: usize = (1 << GEN_BITS) - 1;

#[derive(Clone, Copy)]
struct Slot {
    /// The pinned object; 0: free.
    obj: usize,
    /// The next free slot's index + 1 while free (0: none).
    next: usize,
    /// Releases left until the slot frees.
    refs: usize,
    /// Never 0, so no handle value is 0.
    generation: usize,
    /// What it holds, a static string (`Handle::<T>` type name).
    name: (*const u8, usize),
}

struct Table {
    slots: *mut Slot,
    len: usize,
    cap: usize,
    /// The first free slot's index + 1 (0: none).
    free: usize,
}

struct Handles {
    lock: sys::Lock,
    table: UnsafeCell<Table>,
}

// SAFETY: `table` is only touched under `lock`.
unsafe impl Sync for Handles {}

static HANDLES: Handles = Handles {
    lock: sys::Lock::new(),
    table: UnsafeCell::new(Table {
        slots: ptr::null_mut(),
        len: 0,
        cap: 0,
        free: 0,
    }),
};

/// Why a handle does not give its object.
pub enum HandleFault {
    /// Released as often as it was made and retained, or never a handle.
    Released,
    /// It holds a value of another type, this one (a static string).
    Holds(*const u8, usize),
}

fn with<R>(f: impl FnOnce(&mut Table) -> R) -> R {
    HANDLES.lock.lock();
    // SAFETY: we hold the lock.
    let r = f(unsafe { &mut *HANDLES.table.get() });
    HANDLES.lock.unlock();
    r
}

/// The live slot of handle `h`.
fn slot(t: &mut Table, h: usize) -> Option<&mut Slot> {
    let i = (h >> GEN_BITS).checked_sub(1)?;
    if i >= t.len {
        return None;
    }
    // SAFETY: `i < len`.
    let s = unsafe { &mut *t.slots.add(i) };
    (s.obj != 0 && s.generation == h & GEN_MASK).then_some(s)
}

/// A new handle holding `obj`, of the type named `name` (a static
/// string).
///
/// # Safety
/// `obj` must be a live GC object; `name` readable for `len` bytes for
/// the rest of the process.
pub unsafe fn handle_new(obj: *mut u8, name: *const u8, len: usize) -> usize {
    // SAFETY: caller contract.
    unsafe { pin(obj) };
    with(|t| {
        let i = if t.free != 0 {
            let i = t.free - 1;
            // SAFETY: free slots are below `len`.
            t.free = unsafe { (*t.slots.add(i)).next };
            i
        } else {
            if t.len == t.cap {
                let cap = (t.cap * 2).max(64);
                if cap > (usize::MAX >> GEN_BITS) - 1 {
                    nabir_common::oom();
                }
                // SAFETY: realloc of our own buffer.
                let p = unsafe { libc::realloc(t.slots.cast(), cap * size_of::<Slot>()) };
                if p.is_null() {
                    nabir_common::oom();
                }
                t.slots = p.cast();
                t.cap = cap;
            }
            let i = t.len;
            // SAFETY: `i < cap`.
            unsafe {
                t.slots.add(i).write(Slot {
                    obj: 0,
                    next: 0,
                    refs: 0,
                    generation: 0,
                    name: (ptr::null(), 0),
                });
            }
            t.len += 1;
            i
        };
        // SAFETY: `i < len`.
        let s = unsafe { &mut *t.slots.add(i) };
        s.generation = (s.generation % GEN_MASK) + 1;
        s.obj = obj as usize;
        s.refs = 1;
        s.next = 0;
        s.name = (name, len);
        ((i + 1) << GEN_BITS) | s.generation
    })
}

/// The object of handle `h`, which holds a value of type `name`.
pub fn handle_get(h: usize, name: &[u8]) -> Result<*mut u8, HandleFault> {
    with(|t| {
        let s = slot(t, h).ok_or(HandleFault::Released)?;
        // SAFETY: a static string (`handle_new`).
        let held = unsafe { core::slice::from_raw_parts(s.name.0, s.name.1) };
        if held != name {
            return Err(HandleFault::Holds(s.name.0, s.name.1));
        }
        Ok(s.obj as *mut u8)
    })
}

/// One more release before `h` frees.
pub fn handle_retain(h: usize) -> Result<(), HandleFault> {
    with(|t| {
        let s = slot(t, h).ok_or(HandleFault::Released)?;
        s.refs += 1;
        Ok(())
    })
}

/// Undo `handle_new` or one `handle_retain`; the last one unpins the
/// object and frees the slot.
pub fn handle_release(h: usize) -> Result<(), HandleFault> {
    let obj = with(|t| {
        let free = t.free;
        let s = slot(t, h).ok_or(HandleFault::Released)?;
        s.refs -= 1;
        if s.refs > 0 {
            return Ok(0);
        }
        let obj = s.obj;
        s.obj = 0;
        s.next = free;
        t.free = h >> GEN_BITS;
        Ok(obj)
    })?;
    if obj != 0 {
        // SAFETY: pinned by `handle_new`, so still live.
        unsafe { unpin(obj as *mut u8) };
    }
    Ok(())
}
