//! The type registry: descriptors by `any` tag and by path, and the tags
//! of type keys.
//!
//! Each module registers its descriptors once (a program at startup, a
//! plugin when loaded). Writers serialize on a lock and publish a fresh
//! index; readers load the index and probe it, without locks or
//! allocation. A replaced index is never freed: a reader may still be in
//! it, and registrations are few.

use core::ptr;
use core::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};
use nabir_abi::gc::ANY_BOXED;
use nabir_abi::module::TagKey;
use nabir_abi::typedesc::{NO_TAG, Str, TypeDesc, fnv64};
use nabir_common::sys::{Lock, alloc_aligned};

/// One registration (a module's tables), newest first.
struct Reg {
    descs: *const *const TypeDesc,
    n: usize,
    keys: *const TagKey,
    tags: *const u32,
    nt: usize,
    next: *mut Reg,
}

/// Open-addressed tables over every registration.
struct Index {
    /// Power of two.
    cap: usize,
    by_tag: *mut (u32, *const TypeDesc),
    by_path: *mut (u64, *const TypeDesc),
    /// The tag of each key: `(fnv64 of the key, tag, key)`.
    tag_of: *mut (u64, u32, Str),
}

static LOCK: Lock = Lock::new();
static REGS: AtomicPtr<Reg> = AtomicPtr::new(ptr::null_mut());
// An address, not an `AtomicPtr`: LLVM would fold a pointer global that
// only ever holds fresh allocations into a flag (no wasm lowering).
static INDEX: AtomicUsize = AtomicUsize::new(0);

/// `s`'s bytes.
const fn bytes(s: &Str) -> &[u8] {
    // SAFETY: descriptor strings point at `len` bytes of read-only data.
    unsafe { s.bytes() }
}

fn alloc<T>(n: usize) -> *mut T {
    let size = core::mem::size_of::<T>() * n.max(1);
    let p = alloc_aligned(size, core::mem::align_of::<T>().max(8)).cast::<T>();
    if p.is_null() {
        nabir_common::fatal(b"out of memory in the type registry");
    }
    p
}

/// The registrations, oldest first, into `f`.
///
/// # Safety
///
/// Under `LOCK`.
unsafe fn each_reg(mut f: impl FnMut(&Reg)) {
    let mut n = 0usize;
    let mut r = REGS.load(Ordering::Relaxed);
    while !r.is_null() {
        n += 1;
        // SAFETY: registrations stay allocated.
        r = unsafe { (*r).next };
    }
    let order = alloc::<*const Reg>(n);
    let mut r = REGS.load(Ordering::Relaxed);
    let mut i = n;
    while !r.is_null() {
        i -= 1;
        // SAFETY: `order` has `n` slots.
        unsafe { order.add(i).write(r) };
        // SAFETY: as above.
        r = unsafe { (*r).next };
    }
    for i in 0..n {
        // SAFETY: filled above.
        f(unsafe { &**order.add(i) });
    }
    // SAFETY: allocated above.
    unsafe { nabir_common::sys::free_aligned(order.cast()) };
}

/// The descriptors of `r`.
const fn reg_descs(r: &Reg) -> &[*const TypeDesc] {
    if r.n == 0 {
        return &[];
    }
    // SAFETY: a registration's `n` descriptor pointers.
    unsafe { core::slice::from_raw_parts(r.descs, r.n) }
}

/// The tag of `d` in registration `r`.
const fn tag(r: &Reg, d: &TypeDesc) -> u32 {
    if d.tag_index == NO_TAG || d.tag_index as usize >= r.nt {
        return NO_TAG;
    }
    // SAFETY: `tags` has `nt` entries.
    unsafe { *r.tags.add(d.tag_index as usize) }
}

/// A fresh index over every registration.
///
/// # Safety
///
/// Under `LOCK`.
unsafe fn rebuild() {
    let mut count = 0;
    // SAFETY: under the lock.
    unsafe { each_reg(|r| count += r.n + r.nt) };
    let cap = (count * 2).next_power_of_two().max(16);
    let ix = alloc::<Index>(1);
    let by_tag = alloc::<(u32, *const TypeDesc)>(cap);
    let by_path = alloc::<(u64, *const TypeDesc)>(cap);
    let tag_of = alloc::<(u64, u32, Str)>(cap);
    for i in 0..cap {
        // SAFETY: every table has `cap` slots.
        unsafe {
            by_tag.add(i).write((NO_TAG, ptr::null()));
            by_path.add(i).write((0, ptr::null()));
            tag_of.add(i).write((0, NO_TAG, Str::EMPTY));
        }
    }
    let mask = cap - 1;
    // SAFETY: under the lock; the tables have `cap` slots.
    unsafe {
        each_reg(|r| {
            for &d in reg_descs(r) {
                let d = &*d;
                let tg = tag(r, d);
                if tg != NO_TAG {
                    let mut i = (tg as usize).wrapping_mul(0x9e37_79b9) & mask;
                    while (*by_tag.add(i)).0 != NO_TAG && (*by_tag.add(i)).0 != tg {
                        i = (i + 1) & mask;
                    }
                    by_tag.add(i).write((tg, d));
                }
                let path = bytes(&d.path);
                if !path.is_empty() {
                    let hash = fnv64(path) | 1;
                    let mut i = hash as usize & mask;
                    while !((*by_path.add(i)).1.is_null()
                        || (*by_path.add(i)).0 == hash
                            && bytes(&(*(*by_path.add(i)).1).path) == path)
                    {
                        i = (i + 1) & mask;
                    }
                    // Newest wins: later registrations overwrite.
                    by_path.add(i).write((hash, d));
                }
            }
            for k in 0..r.nt {
                let key = &*r.keys.add(k);
                let t = *r.tags.add(k);
                if t == NO_TAG {
                    continue;
                }
                let h = key.hash | 1;
                let mut i = h as usize & mask;
                while (*tag_of.add(i)).1 != NO_TAG {
                    if (*tag_of.add(i)).0 == h && bytes(&(*tag_of.add(i)).2) == bytes(&key.key) {
                        break;
                    }
                    i = (i + 1) & mask;
                }
                tag_of.add(i).write((h, t, key.key));
            }
        });
        ix.write(Index {
            cap,
            by_tag,
            by_path,
            tag_of,
        });
    }
    INDEX.store(ix as usize, Ordering::Release);
}

/// The current index, if any.
fn index() -> Option<&'static Index> {
    let ix = INDEX.load(Ordering::Acquire) as *const Index;
    // SAFETY: published indexes are never freed.
    (!ix.is_null()).then(|| unsafe { &*ix })
}

/// The registered tag of `key`, if any.
fn tag_of_key(ix: &Index, key: &Str, hash: u64) -> Option<u32> {
    let h = hash | 1;
    let mask = ix.cap - 1;
    let mut i = h as usize & mask;
    for _ in 0..ix.cap {
        // SAFETY: `i < cap`.
        let e = unsafe { &*ix.tag_of.add(i) };
        if e.1 == NO_TAG {
            return None;
        }
        if e.0 == h && bytes(&e.2) == bytes(key) {
            return Some(e.1);
        }
        i = (i + 1) & mask;
    }
    None
}

/// Whether `t` is the tag of a registered descriptor or key.
fn tag_used(ix: Option<&Index>, t: u32, fresh: &[u32]) -> bool {
    if fresh.contains(&t) {
        return true;
    }
    let Some(ix) = ix else { return false };
    if !by_tag(ix, t).is_null() {
        return true;
    }
    (0..ix.cap).any(|i| {
        // SAFETY: `i < cap`.
        unsafe { (*ix.tag_of.add(i)).1 == t }
    })
}

fn by_tag(ix: &Index, t: u32) -> *const TypeDesc {
    let mask = ix.cap - 1;
    let mut i = (t as usize).wrapping_mul(0x9e37_79b9) & mask;
    for _ in 0..ix.cap {
        // SAFETY: `i < cap`.
        let e = unsafe { &*ix.by_tag.add(i) };
        if e.0 == t {
            return e.1;
        }
        if e.0 == NO_TAG {
            return ptr::null();
        }
        i = (i + 1) & mask;
    }
    ptr::null()
}

/// Registers `n` descriptors and resolves `nt` tags (see
/// `nabir_abi::module::entry::TypesRegister`).
///
/// # Safety
///
/// `descs` points at `n` descriptors and `tags` / `out_tags` at `nt`
/// entries, all alive until unregistered.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nabi_types_register(
    descs: *const *const TypeDesc,
    n: usize,
    tags: *const TagKey,
    out_tags: *mut u32,
    nt: usize,
) -> i32 {
    LOCK.lock();
    let ix = index();
    let new: &[*const TypeDesc] = if n == 0 {
        &[]
    } else {
        // SAFETY: the caller's contract.
        unsafe { core::slice::from_raw_parts(descs, n) }
    };
    // A key described before must keep its layout.
    let mut clash = 0;
    // SAFETY: under the lock.
    unsafe {
        each_reg(|r| {
            for &old in reg_descs(r) {
                let old = &*old;
                for (i, &d) in new.iter().enumerate() {
                    let d = &*d;
                    if clash == 0
                        && d.key_hash == old.key_hash
                        && bytes(&d.key) == bytes(&old.key)
                        && d.layout_hash != old.layout_hash
                    {
                        clash = i32::try_from(i + 1).unwrap_or(i32::MAX);
                    }
                }
            }
        });
    }
    if clash != 0 {
        LOCK.unlock();
        return clash;
    }
    for k in 0..nt {
        // SAFETY: the caller's contract.
        let (key, slot) = unsafe { (&*tags.add(k), out_tags.add(k)) };
        // SAFETY: as above.
        if unsafe { *slot } != NO_TAG {
            continue;
        }
        let t = ix
            .and_then(|ix| tag_of_key(ix, &key.key, key.hash))
            .unwrap_or_else(|| {
                let mut t = (fnv64(bytes(&key.key)) as u32) & !ANY_BOXED;
                // Tags this call already gave count as used too.
                // SAFETY: `out_tags` has `nt` entries.
                let given = unsafe { core::slice::from_raw_parts(out_tags, nt) };
                while t == NO_TAG & !ANY_BOXED || tag_used(ix, t, given) {
                    t = (t + 1) & !ANY_BOXED;
                }
                t
            });
        // SAFETY: as above.
        unsafe { slot.write(t) };
    }
    let reg = alloc::<Reg>(1);
    // SAFETY: fresh allocation.
    unsafe {
        reg.write(Reg {
            descs,
            n,
            keys: tags,
            tags: out_tags,
            nt,
            next: REGS.load(Ordering::Relaxed),
        });
    }
    REGS.store(reg, Ordering::Relaxed);
    // SAFETY: under the lock.
    unsafe { rebuild() };
    LOCK.unlock();
    0
}

/// The descriptor of `any` tag `tag`, or null.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_types_by_tag(tag: u32) -> *const TypeDesc {
    index().map_or(ptr::null(), |ix| by_tag(ix, tag & !ANY_BOXED))
}

/// The newest descriptor of path `p` (`len` bytes), or null.
///
/// # Safety
///
/// `p` points at `len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nabi_types_by_path(p: *const u8, len: usize) -> *const TypeDesc {
    let Some(ix) = index() else {
        return ptr::null();
    };
    let want: &[u8] = if len == 0 {
        &[]
    } else {
        // SAFETY: the caller's contract.
        unsafe { core::slice::from_raw_parts(p, len) }
    };
    let h = fnv64(want) | 1;
    let mask = ix.cap - 1;
    let mut i = h as usize & mask;
    for _ in 0..ix.cap {
        // SAFETY: `i < cap`.
        let e = unsafe { &*ix.by_path.add(i) };
        if e.1.is_null() {
            return ptr::null();
        }
        // SAFETY: registered descriptors stay alive.
        if e.0 == h && bytes(unsafe { &(*e.1).path }) == want {
            return e.1;
        }
        i = (i + 1) & mask;
    }
    ptr::null()
}

/// Drops the registration whose descriptor array is `descs`.
///
/// # Safety
///
/// No reader may use those descriptors after this (the world is
/// stopped).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nabi_types_unregister(descs: *const *const TypeDesc, _n: usize) {
    LOCK.lock();
    let mut at = &REGS;
    loop {
        let r = at.load(Ordering::Relaxed);
        if r.is_null() {
            break;
        }
        // SAFETY: registrations stay allocated while listed.
        let reg = unsafe { &*r };
        if reg.descs == descs {
            at.store(reg.next, Ordering::Relaxed);
            break;
        }
        // SAFETY: `AtomicPtr<Reg>` and `*mut Reg` have one layout.
        at = unsafe { &*ptr::addr_of!(reg.next).cast::<AtomicPtr<Reg>>() };
    }
    // SAFETY: under the lock.
    unsafe { rebuild() };
    LOCK.unlock();
}

#[cfg(test)]
mod tests {
    use super::*;
    use nabir_abi::typedesc::{Slice, kind};

    const fn s(text: &'static str) -> Str {
        Slice {
            ptr: text.as_ptr(),
            len: text.len(),
        }
    }

    fn desc(path: &'static str, layout_hash: u64, tag_index: u32) -> &'static TypeDesc {
        Box::leak(Box::new(TypeDesc {
            magic: nabir_abi::typedesc::MAGIC,
            version: 1,
            kind: kind::STRUCT,
            flags: 0,
            tag_index,
            key_hash: fnv64(path.as_bytes()),
            layout_hash,
            size: 8,
            align: 8,
            reserved0: 0,
            key: s(path),
            name: s(path),
            path: s(path),
            fields: Slice::EMPTY,
            variants: Slice::EMPTY,
            generics: Slice::EMPTY,
            attrs: Slice::EMPTY,
            ops: ptr::null(),
            reserved1: 0,
        }))
    }

    #[test]
    fn registers_finds_and_resolves() {
        let a = desc("t::A", 1, 0);
        let b = desc("t::B", 2, 1);
        let descs: &'static [*const TypeDesc] =
            Box::leak(Box::new([ptr::from_ref(a), ptr::from_ref(b)]));
        let keys: &'static [TagKey] = Box::leak(Box::new([
            TagKey {
                key: s("t::A"),
                hash: fnv64(b"t::A"),
            },
            TagKey {
                key: s("t::B"),
                hash: fnv64(b"t::B"),
            },
        ]));
        let tags: &'static mut [u32] = Box::leak(Box::new([77, NO_TAG]));
        // SAFETY: leaked, so alive for the test.
        let r =
            unsafe { nabi_types_register(descs.as_ptr(), 2, keys.as_ptr(), tags.as_mut_ptr(), 2) };
        assert_eq!(r, 0);
        assert_eq!(tags[0], 77);
        assert_ne!(tags[1], NO_TAG);
        assert_eq!(nabi_types_by_tag(77), ptr::from_ref(a));
        assert_eq!(nabi_types_by_tag(77 | ANY_BOXED), ptr::from_ref(a));
        assert_eq!(nabi_types_by_tag(tags[1]), ptr::from_ref(b));
        // SAFETY: a live string.
        assert_eq!(
            unsafe { nabi_types_by_path(b"t::B".as_ptr(), 4) },
            ptr::from_ref(b)
        );
        // A second module: the known key keeps its tag; another layout clashes.
        let a2 = desc("t::A", 1, 0);
        let more: &'static [*const TypeDesc] = Box::leak(Box::new([ptr::from_ref(a2)]));
        let tags2: &'static mut [u32] = Box::leak(Box::new([NO_TAG]));
        // SAFETY: as above.
        let r =
            unsafe { nabi_types_register(more.as_ptr(), 1, keys.as_ptr(), tags2.as_mut_ptr(), 1) };
        assert_eq!(r, 0);
        assert_eq!(tags2[0], 77);
        // SAFETY: as above.
        assert_eq!(
            unsafe { nabi_types_by_path(b"t::A".as_ptr(), 4) },
            ptr::from_ref(a2)
        );
        let bad = desc("t::B", 9, 1);
        let clash: &'static [*const TypeDesc] = Box::leak(Box::new([ptr::from_ref(bad)]));
        // SAFETY: as above.
        assert_eq!(
            unsafe { nabi_types_register(clash.as_ptr(), 1, keys.as_ptr(), tags2.as_mut_ptr(), 0) },
            1
        );
        // SAFETY: as above.
        unsafe { nabi_types_unregister(more.as_ptr(), 1) };
        // SAFETY: as above.
        assert_eq!(
            unsafe { nabi_types_by_path(b"t::A".as_ptr(), 4) },
            ptr::from_ref(a)
        );
    }
}
