//! Loaded modules (plugins) as the collector sees them.
//!
//! Each gets a contiguous range of type ids and its own static roots. A
//! retired module (unloaded, not yet freed) is checked by every full
//! stop-the-world collection: it is live while an object of one of its
//! types, or a heap word pointing into its code, is reachable from
//! anything but its own statics. Its statics are marked after that, so
//! what they hold stays valid until the module is freed.

#[allow(clippy::wildcard_imports)]
use crate::*;

/// Module slots; ample for the plugins one process loads.
pub const MAX: usize = 256;

#[allow(clippy::struct_excessive_bools)]
struct Module {
    used: bool,
    retired: bool,
    /// Type ids `[lo, hi)`, code `[text_lo, text_hi)`.
    ids: (u32, u32),
    text: (usize, usize),
    /// `(word, any)`: a root word, or the payload of an `any` whose tag
    /// is the word before.
    roots: *mut (usize, usize),
    nroots: usize,
    /// Seen by the current full mark.
    live: AtomicBool,
    /// What last kept it: an object's type id, or `CODE` (a word into
    /// its code).
    why: core::sync::atomic::AtomicU32,
    /// A full collection since `retire`, and what it found.
    checked: bool,
    verdict: bool,
}

struct Table([Module; MAX]);

static MODULES: Global<Table> = Global(UnsafeCell::new(Table(
    [const {
        Module {
            used: false,
            retired: false,
            ids: (0, 0),
            text: (0, 0),
            roots: ptr::null_mut(),
            nroots: 0,
            live: AtomicBool::new(false),
            why: core::sync::atomic::AtomicU32::new(0),
            checked: false,
            verdict: false,
        }
    }; MAX],
)));

/// Some module is retired: marking tells it apart.
static RETIRED: AtomicBool = AtomicBool::new(false);
/// The next type id no module or program has.
static NEXT_ID: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

fn table() -> &'static mut [Module; MAX] {
    // SAFETY: written with the world lock held; the marker reads `live`
    // atomically and the rest only while the world is stopped.
    unsafe { &mut (*MODULES.0.get()).0 }
}

/// With the world lock held: a slot for a module of `n` GC types.
///
/// `tables` are registered at the fresh contiguous ids returned (`ids[i]
/// == base + i`), `roots` become its static roots. `None` when every
/// slot is taken.
///
/// # Safety
/// `tables` holds `n` scan tables; each root word stays valid until
/// [`remove`].
pub unsafe fn add(
    tables: *const *const u8,
    n: u32,
    roots: &[(usize, usize)],
    text: (usize, usize),
) -> Option<(usize, u32)> {
    let t = table();
    let slot = t.iter().position(|m| !m.used)?;
    let base = next_ids(n);
    for i in 0..n {
        // SAFETY: caller contract.
        unsafe { set_table(base + i, *tables.add(i as usize)) };
    }
    let r = if roots.is_empty() {
        ptr::null_mut()
    } else {
        // SAFETY: plain allocation, copied below.
        let p = unsafe { libc::malloc(size_of_val(roots)) }.cast::<(usize, usize)>();
        if p.is_null() {
            nabir_common::oom();
        }
        // SAFETY: `p` holds `roots.len()` entries.
        unsafe { ptr::copy_nonoverlapping(roots.as_ptr(), p, roots.len()) };
        p
    };
    let m = &mut t[slot];
    m.used = true;
    m.retired = false;
    m.ids = (base, base + n);
    m.text = text;
    m.roots = r;
    m.nroots = roots.len();
    m.checked = false;
    Some((slot, base))
}

/// The start of `n` ids nothing registered uses yet.
fn next_ids(n: u32) -> u32 {
    let t = index::TABLES.load(Ordering::Acquire);
    let mut top = USER_TYPE_BASE;
    if !t.is_null() {
        // SAFETY: `[len, tables..]`, as `register_type` keeps it.
        unsafe {
            let len = *t;
            for i in (0..len).rev() {
                if *t.add(1 + i) != 0 {
                    top = USER_TYPE_BASE + i as u32 + 1;
                    break;
                }
            }
        }
    }
    let base = NEXT_ID.load(Ordering::Relaxed).max(top);
    NEXT_ID.store(base + n, Ordering::Relaxed);
    base
}

/// `register_type` with the world lock already held.
///
/// # Safety
/// `table` is a scan table, or null.
unsafe fn set_table(type_id: u32, table: *const u8) {
    let i = (type_id - USER_TYPE_BASE) as usize;
    let mut t = index::TABLES.load(Ordering::Acquire);
    // SAFETY: as in `register_type`.
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
        AtomicUsize::from_ptr(t.add(1 + i)).store(table as usize, Ordering::Release);
    }
    index::TABLES.store(t, Ordering::Release);
}

/// With the world lock held: module `slot` is unloaded; full collections
/// check it from now on.
pub fn retire(slot: usize) {
    let m = &mut table()[slot];
    m.retired = true;
    m.checked = false;
    RETIRED.store(true, Ordering::Release);
}

/// [`why`]: a heap word points into the module's code.
pub const CODE: u32 = u32::MAX;

/// What kept module `slot` alive at the last check: the type id of an
/// object of its types, or [`CODE`].
#[must_use]
pub fn why(slot: usize) -> u32 {
    table()[slot].why.load(Ordering::Relaxed)
}

/// Whether a full collection since [`retire`] found module `slot` dead.
#[must_use]
pub fn dead(slot: usize) -> bool {
    world::lock();
    let m = &table()[slot];
    let d = m.used && m.retired && m.checked && !m.verdict;
    world::unlock();
    d
}

/// With the world stopped: forget module `slot`: its roots go, its type
/// ids scan nothing (an object of them is garbage by now).
pub fn remove(slot: usize) {
    let t = table();
    let m = &mut t[slot];
    for id in m.ids.0..m.ids.1 {
        // SAFETY: null clears the entry.
        unsafe { set_table(id, ptr::null()) };
    }
    if !m.roots.is_null() {
        // SAFETY: allocated by `add`.
        unsafe { libc::free(m.roots.cast()) };
    }
    m.roots = ptr::null_mut();
    m.nroots = 0;
    m.used = false;
    m.retired = false;
    RETIRED.store(t.iter().any(|m| m.used && m.retired), Ordering::Release);
}

/// The referent of root `(word, any)`, 0 for none.
const fn referent((word, any): (usize, usize)) -> usize {
    // SAFETY: `add`'s contract: the word is valid; an `any` payload has
    // its tag word just before.
    unsafe {
        if any != 0 && *((word - size_of::<usize>()) as *const u32) & ANY_BOXED == 0 {
            return 0;
        }
        *(word as *const usize)
    }
}

/// With the world stopped (`world::for_each_root`): `f` on the referent
/// of every module root, except those of retired modules in a full
/// stop-the-world mark, which [`late`] marks.
pub(crate) fn for_each_root(f: &mut impl FnMut(*mut u8)) {
    let m = mark();
    let check = m.full && !m.concurrent && RETIRED.load(Ordering::Acquire);
    for md in table().iter().filter(|md| md.used) {
        if check && md.retired {
            md.live.store(false, Ordering::Relaxed);
            continue;
        }
        for k in 0..md.nroots {
            // SAFETY: `nroots` entries.
            let r = referent(unsafe { *md.roots.add(k) });
            if r != 0 {
                f(r as *mut u8);
            }
        }
    }
}

/// Marking `o`: note a retired module whose type `o` has, or into whose
/// code one of `o`'s words points.
#[inline]
pub(crate) fn seen(o: *mut u8) {
    if !RETIRED.load(Ordering::Relaxed) {
        return;
    }
    seen_slow(o);
}

#[cold]
fn seen_slow(o: *mut u8) {
    // SAFETY: `o` is a live object being marked.
    let (id, size) = unsafe { ((*header(o)).type_id, obj_size(o)) };
    for m in table().iter().filter(|m| m.used && m.retired) {
        if m.live.load(Ordering::Relaxed) {
            continue;
        }
        let mut why = (m.ids.0..m.ids.1).contains(&id).then_some(id);
        let mut at = HEADER_SIZE;
        while why.is_none() && at + size_of::<usize>() <= size {
            // SAFETY: inside the object.
            let w = unsafe { (o.add(at) as *const usize).read_unaligned() };
            if w >= m.text.0 && w < m.text.1 {
                why = Some(CODE);
            }
            at += size_of::<usize>();
        }
        if let Some(w) = why {
            m.why.store(w, Ordering::Relaxed);
            m.live.store(true, Ordering::Relaxed);
        }
    }
}

/// After the first drain of a full stop-the-world mark: record each
/// retired module's verdict, then queue its own roots. Whether any were
/// queued (the caller drains again).
pub(crate) fn late() -> bool {
    if !RETIRED.load(Ordering::Acquire) {
        return false;
    }
    let mut queued = false;
    for md in table().iter_mut().filter(|md| md.used && md.retired) {
        md.checked = true;
        md.verdict = md.live.load(Ordering::Relaxed);
        for k in 0..md.nroots {
            // SAFETY: `nroots` entries.
            let r = referent(unsafe { *md.roots.add(k) });
            if r != 0 {
                push(r, true);
                queued = true;
            }
        }
    }
    queued
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn ids_are_contiguous_past_every_registered_one() {
        static T: [u32; 4] = [24, 1 << 16, 0, 0];
        register_type(USER_TYPE_BASE + 40, T.as_ptr().cast());
        let tables = [T.as_ptr().cast::<u8>(); 3];
        world::lock();
        // SAFETY: three tables, no roots.
        let (a, base) = unsafe { add(tables.as_ptr(), 3, &[], (0, 0)) }.unwrap();
        // SAFETY: as above.
        let (b, next) = unsafe { add(tables.as_ptr(), 1, &[], (0, 0)) }.unwrap();
        world::unlock();
        assert!(base > USER_TYPE_BASE + 40);
        assert_eq!(next, base + 3);
        assert_eq!(scan_table(base + 2), T.as_ptr().cast());
        world::lock();
        remove(a);
        remove(b);
        world::unlock();
        assert!(scan_table(base).is_null());
    }

    /// Run from the collector's sequential test (one heap per process).
    #[allow(clippy::cast_ptr_alignment)] // GC objects are 8-aligned
    pub(crate) fn a_retired_module_lives_while_reached() {
        #[repr(C)]
        struct Frame {
            prev: *mut Frame,
            count: usize,
        }
        static LEAF: [u32; 4] = [24, 1, 0, 0];
        let tables = [LEAF.as_ptr().cast::<u8>()];
        let mut own_static = 0usize;
        world::lock();
        // SAFETY: one table; the root word outlives the module.
        let (slot, id) = unsafe {
            add(
                tables.as_ptr(),
                1,
                &[(&raw const own_static as usize, 0)],
                (0x1000, 0x2000),
            )
        }
        .unwrap();
        world::unlock();
        let own = alloc(24, 8, id);
        own_static = own as usize;
        let held = alloc(24, 8, id);
        // SAFETY: as in the collector's test: a one-slot frame linked
        // and unlinked here.
        unsafe {
            let mut root = held as usize;
            let mut frame: (Frame, *mut usize) = (
                Frame {
                    prev: ptr::null_mut(),
                    count: 1,
                },
                &mut root,
            );
            let top = nabir_common::tls::shadow_stack_top();
            *top = (&raw mut frame).cast();
            world::lock();
            retire(slot);
            world::unlock();
            assert!(!dead(slot), "no collection checked it yet");
            collect();
            assert!(!dead(slot), "the frame holds an object of its type");
            let code = alloc(24, 8, USER_TYPE_BASE + 1);
            *code.add(16).cast::<usize>() = 0x1800;
            ptr::write_volatile(&raw mut root, code as usize);
            collect();
            assert!(!dead(slot), "a heap word points into its code");
            ptr::write_volatile(&raw mut root, 0);
            collect();
            assert!(dead(slot));
            // What only its statics hold stays until it is freed.
            assert_eq!((*header(own)).type_id, id);
            assert_eq!(own_static, own as usize);
            *top = ptr::null_mut();
        }
        world::lock();
        remove(slot);
        world::unlock();
        collect();
    }
}
