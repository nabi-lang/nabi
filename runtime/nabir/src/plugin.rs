//! The module loader: the host's descriptor, plugins loaded by path.
//!
//! One runtime per process, in the host. A plugin's imports are bound by
//! name from the exports of the host, then of plugins loaded before it,
//! then its own; its GC types get a fresh id range, its tags, type
//! descriptors, `any` dispatch entries and static roots are registered,
//! all inside one stop-the-world. Unload is deferred: the plugin leaves
//! lookup at once and is freed after a full collection finds nothing
//! but its own statics reaching it.

mod calls;
#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
use crate::plugin::unix as os;
#[cfg(windows)]
use crate::plugin::windows as os;
use core::cell::UnsafeCell;
use core::ptr;
use core::sync::atomic::{AtomicUsize, Ordering};
use nabir_abi::module::{Dispatch, Export, MAGIC, ModuleDesc, PkgDesc, VERSION, flags};
use nabir_abi::typedesc::{Slice, Str};
use nabir_common::sys::Lock;
use nabir_common::wtf8::MBuf;
use nabir_gc::world;

/// Module slots (the host is slot 0).
const MAX: usize = 256;
/// A handle is `slot << ERA_BITS | era`: fits 32-bit `usize`.
const ERA_BITS: u32 = 24;

const FREE: u8 = 0;
const LOADED: u8 = 1;
const RETIRED: u8 = 2;
const UNLOADED: u8 = 3;

#[derive(Clone, Copy)]
struct Module {
    desc: *const ModuleDesc,
    /// The OS handle; null for the host.
    os: *mut core::ffi::c_void,
    path: Str,
    era: u32,
    state: u8,
    /// Load order.
    seq: u64,
    /// The collector's slot, `usize::MAX` for none.
    gc: usize,
    text: (usize, usize),
    image: (usize, usize),
    /// The modules it binds imports from.
    deps: [u64; MAX / 64],
}

const EMPTY: Module = Module {
    desc: ptr::null(),
    os: ptr::null_mut(),
    path: Slice::EMPTY,
    era: 0,
    state: FREE,
    seq: 0,
    gc: usize::MAX,
    text: (0, 0),
    image: (0, 0),
    deps: [0; MAX / 64],
};

struct Table {
    mods: [Module; MAX],
    seq: u64,
}

struct Cell(UnsafeCell<Table>);
// SAFETY: only touched under `LOCK`.
unsafe impl Sync for Cell {}

static LOCK: Lock = Lock::new();
static TABLE: Cell = Cell(UnsafeCell::new(Table {
    mods: [EMPTY; MAX],
    seq: 0,
}));

/// Take `LOCK` as a blocking call: a stop does not wait for a thread
/// blocked on it.
fn lock() {
    world::enter_native();
    LOCK.lock();
    world::leave_native();
}

/// Under `LOCK`.
fn table() -> &'static mut Table {
    // SAFETY: callers hold `LOCK`.
    unsafe { &mut *TABLE.0.get() }
}

/// The `any Tr` dispatch entries of every loaded module: `(method, tag,
/// code)`, open addressed by `(method, tag)`; `cap` a power of two.
struct Dispatches {
    cap: usize,
    at: *mut (u64, u32, usize),
}

/// Replaced whole under `LOCK`, never freed (readers take no lock). An
/// address, not an `AtomicPtr`: LLVM folds a pointer global that only
/// ever holds fresh allocations into a flag.
static DISPATCH: AtomicUsize = AtomicUsize::new(0);

/// Each plugin's image, `[lo, hi)`, readable without the lock.
static IMAGES: [(AtomicUsize, AtomicUsize); MAX] =
    [const { (AtomicUsize::new(0), AtomicUsize::new(0)) }; MAX];

/// `word` lies in a loaded plugin's image: its statics, which its
/// descriptor's roots already cover.
pub(crate) fn in_plugin(word: usize) -> bool {
    IMAGES[1..]
        .iter()
        .any(|(lo, hi)| word >= lo.load(Ordering::Acquire) && word < hi.load(Ordering::Acquire))
}

/// What `nabi_plugin_*` report a failure in.
#[repr(C)]
pub struct PluginErr {
    pub kind: u32,
    pub reserved: u32,
    pub a: Str,
    pub b: Str,
    /// '\n'-joined names.
    pub list: Str,
}

/// `PluginErr::kind`.
mod kind {
    pub const NOT_FOUND: u32 = 1;
    pub const NOT_A_PLUGIN: u32 = 2;
    pub const TOOLCHAIN: u32 = 3;
    pub const TARGET: u32 = 4;
    pub const MISSING_PACKAGES: u32 = 5;
    pub const MISSING_SYMBOLS: u32 = 6;
    pub const LAYOUT: u32 = 7;
    pub const NO_SUCH_ITEM: u32 = 8;
    pub const WRONG_TYPE: u32 = 9;
    pub const OS: u32 = 10;
    pub const NOT_LOADED: u32 = 11;
}

/// A failure while loading: its kind and texts.
struct Fail {
    kind: u32,
    a: MBuf<u8>,
    b: MBuf<u8>,
    list: MBuf<u8>,
}

impl Fail {
    fn new(kind: u32, a: &[u8]) -> Self {
        let mut f = Self {
            kind,
            a: MBuf::new(),
            b: MBuf::new(),
            list: MBuf::new(),
        };
        f.a.extend(a);
        f
    }

    fn with_b(mut self, b: &[u8]) -> Self {
        self.b.extend(b);
        self
    }

    fn name(&mut self, n: &[u8]) {
        if !self.list.is_empty() {
            self.list.push(b'\n');
        }
        self.list.extend(n);
    }

    /// Into `out` (if any); the texts stay allocated: they are small and
    /// failures are rare.
    fn put(self, out: *mut PluginErr) {
        if out.is_null() {
            return;
        }
        let keep = |m: MBuf<u8>| -> Str {
            let s = Str {
                ptr: m.as_slice().as_ptr(),
                len: m.len(),
            };
            core::mem::forget(m);
            s
        };
        // SAFETY: the caller's `PluginErr`.
        unsafe {
            out.write(PluginErr {
                kind: self.kind,
                reserved: 0,
                a: keep(self.a),
                b: keep(self.b),
                list: keep(self.list),
            });
        }
    }
}

/// `s`'s bytes.
const fn bytes(s: &Str) -> &'static [u8] {
    if s.len == 0 {
        return &[];
    }
    // SAFETY: descriptor and loader strings point at `len` bytes that
    // live as long as their module.
    unsafe { core::slice::from_raw_parts(s.ptr, s.len) }
}

/// `s`'s items.
const fn items<T>(s: &Slice<T>) -> &'static [T] {
    if s.len == 0 || s.ptr.is_null() {
        return &[];
    }
    // SAFETY: as `bytes`.
    unsafe { core::slice::from_raw_parts(s.ptr, s.len) }
}

/// A copy of `b` that lives on (module paths).
fn keep(b: &[u8]) -> Str {
    let mut m = MBuf::new();
    m.extend(b);
    let s = Str {
        ptr: m.as_slice().as_ptr(),
        len: m.len(),
    };
    core::mem::forget(m);
    s
}

const fn handle(i: usize, m: &Module) -> usize {
    (i << ERA_BITS) | m.era as usize
}

/// The module of handle `h`, if it is still that one.
fn by_handle(h: usize) -> Option<usize> {
    let i = h >> ERA_BITS;
    let m = table().mods.get(i)?;
    (m.state != FREE && m.era as usize == h & ((1 << ERA_BITS) - 1)).then_some(i)
}

/// The 8 hex digits of `_Y<pkg8>...`.
fn pkg8(sym: &[u8]) -> Option<&[u8]> {
    sym.strip_prefix(b"_Y").and_then(|r| r.get(..8))
}

/// The export of `name` in `m`'s sorted table.
fn find_export(m: &ModuleDesc, name: &[u8]) -> Option<&'static Export> {
    let e = items(&m.exports);
    e.binary_search_by(|x| bytes(&x.sym).cmp(name))
        .ok()
        .map(|k| &e[k])
}

/// The host's descriptor, once registered.
fn host() -> Option<&'static ModuleDesc> {
    let m = &table().mods[0];
    // SAFETY: a registered descriptor lives for the process.
    (m.state == LOADED).then(|| unsafe { &*m.desc })
}

/// Live modules other than `skip` that may provide symbols, host first,
/// then in load order.
fn providers(skip: usize) -> MBuf<usize> {
    let t = table();
    let mut v: MBuf<usize> = MBuf::new();
    for (i, m) in t.mods.iter().enumerate() {
        if i != skip && m.state == LOADED {
            v.push(i);
        }
    }
    v.as_mut_slice()
        .sort_unstable_by_key(|&i| (i != 0, t.mods[i].seq));
    v
}

/// Bind `idx`'s imports: the address of each into its slot, the modules
/// it uses into `deps`.
fn bind(idx: usize) -> Result<(), Fail> {
    let t = table();
    // SAFETY: a loaded descriptor.
    let me = unsafe { &*t.mods[idx].desc };
    let order = providers(idx);
    let mut missing = Fail::new(kind::MISSING_SYMBOLS, b"");
    let mut deps = [0u64; MAX / 64];
    for imp in items(&me.imports) {
        let name = bytes(&imp.sym);
        let p8 = pkg8(name);
        let mut found = None;
        for &p in order.as_slice() {
            // The runtime's entries come from the host only.
            if p != 0 && p8.is_none() {
                continue;
            }
            // SAFETY: a loaded descriptor.
            if let Some(e) = find_export(unsafe { &*t.mods[p].desc }, name) {
                if imp.sig != 0 && e.sig != 0 && imp.sig != e.sig {
                    return Err(
                        Fail::new(kind::OS, b"internal error: the signature of ").with_b(name)
                    );
                }
                found = Some((p, e.addr));
                break;
            }
        }
        if found.is_none() {
            found = find_export(me, name).map(|e| (idx, e.addr));
        }
        match found {
            Some((p, addr)) => {
                // SAFETY: the slot the linker made for this import.
                unsafe { imp.slot.write(addr) };
                if p != idx && p != 0 {
                    deps[p / 64] |= 1 << (p % 64);
                }
            }
            None => missing.name(name),
        }
    }
    if !missing.list.is_empty() {
        return Err(missing);
    }
    t.mods[idx].deps = deps;
    Ok(())
}

/// Every package `idx` needs is provided by a loaded module, or the host
/// has its code.
fn check_needs(idx: usize) -> Result<(), Fail> {
    let t = table();
    // SAFETY: a loaded descriptor.
    let me = unsafe { &*t.mods[idx].desc };
    let mut missing = Fail::new(kind::MISSING_PACKAGES, b"");
    for need in items(&me.needs) {
        // A module that provides it, or has its code (a host's library,
        // another plugin's or this one's own copies of its statics).
        let ok = has_code(me, need.pkg8)
            || providers(idx).as_slice().iter().any(|&p| {
                // SAFETY: a loaded descriptor.
                let d = unsafe { &*t.mods[p].desc };
                items(&d.provides)
                    .iter()
                    .any(|q: &PkgDesc| q.pkg8 == need.pkg8 && q.sha256 == need.sha256)
                    || has_code(d, need.pkg8)
            });
        if !ok {
            let mut n: MBuf<u8> = MBuf::new();
            n.extend(bytes(&need.name));
            if need.version.len != 0 {
                n.extend(b" ");
                n.extend(bytes(&need.version));
            }
            missing.name(n.as_slice());
        }
    }
    if missing.list.is_empty() {
        Ok(())
    } else {
        Err(missing)
    }
}

/// `d` exports some symbol of package `p8`.
fn has_code(d: &ModuleDesc, p8: [u8; 8]) -> bool {
    let e = items(&d.exports);
    let mut want = [0u8; 10];
    want[..2].copy_from_slice(b"_Y");
    want[2..].copy_from_slice(&p8);
    let k = e.partition_point(|x| bytes(&x.sym) < &want[..]);
    e.get(k).is_some_and(|x| bytes(&x.sym).starts_with(&want))
}

/// Rebuild the dispatch index from every loaded module.
#[allow(clippy::cast_ptr_alignment)] // `alloc_aligned(.., 8)`
fn rebuild_dispatch() {
    let tab = table();
    let mut count = 0;
    for m in tab.mods.iter().filter(|m| m.state == LOADED) {
        // SAFETY: a loaded descriptor.
        count += unsafe { (*m.desc).dispatch.len };
    }
    let cap = (count * 2).next_power_of_two().max(16);
    let size = cap * size_of::<(u64, u32, usize)>();
    let at = nabir_common::sys::alloc_aligned(size, 8).cast::<(u64, u32, usize)>();
    let ix = nabir_common::sys::alloc_aligned(size_of::<Dispatches>(), 8).cast::<Dispatches>();
    if at.is_null() || ix.is_null() {
        nabir_common::oom();
    }
    // SAFETY: fresh allocations of `cap` entries and one index.
    unsafe {
        for k in 0..cap {
            at.add(k).write((0, 0, 0));
        }
        for m in tab.mods.iter().filter(|m| m.state == LOADED) {
            let desc = &*m.desc;
            let tags = desc.tags.tags;
            for ent in items(&desc.dispatch) {
                let ent: &Dispatch = ent;
                if ent.tag_index >= desc.tags.n as usize {
                    continue;
                }
                let tag = *tags.add(ent.tag_index);
                let mut k = slot_of(ent.method, tag, cap);
                while (*at.add(k)).2 != 0
                    && !((*at.add(k)).0 == ent.method && (*at.add(k)).1 == tag)
                {
                    k = (k + 1) & (cap - 1);
                }
                at.add(k).write((ent.method, tag, ent.code));
            }
        }
        ix.write(Dispatches { cap, at });
    }
    DISPATCH.store(ix as usize, Ordering::Release);
}

const fn slot_of(method: u64, tag: u32, cap: usize) -> usize {
    ((method ^ (tag as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15)) as usize).wrapping_mul(0x9e37_79b9)
        & (cap - 1)
}

/// The code of `method` for a value of `tag` from a loaded module; 0
/// when none (the caller panics).
#[unsafe(no_mangle)]
pub extern "C" fn nabi_any_dispatch(method: u64, tag: u32) -> usize {
    let tag = tag & !nabir_abi::gc::ANY_BOXED;
    let ix = DISPATCH.load(Ordering::Acquire) as *const Dispatches;
    if ix.is_null() {
        return 0;
    }
    // SAFETY: published indexes are never freed.
    unsafe {
        let ix = &*ix;
        let mut k = slot_of(method, tag, ix.cap);
        for _ in 0..ix.cap {
            let e = *ix.at.add(k);
            if e.2 == 0 {
                return 0;
            }
            if e.0 == method && e.1 == tag {
                return e.2;
            }
            k = (k + 1) & (ix.cap - 1);
        }
    }
    0
}

/// This thread's TLS block: plugin code has no thread-locals of its own.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_tls_get() -> *mut u8 {
    unsafe extern "C" {
        fn nabi_tls_addr() -> *mut u8;
    }
    // SAFETY: takes nothing, returns this thread's block.
    unsafe { nabi_tls_addr() }
}

/// With the world stopped: register `idx`'s types, tags, GC tables,
/// dispatch entries and roots. A host keeps its own GC registration.
fn register(idx: usize) -> Result<(), Fail> {
    let t = table();
    // SAFETY: a loaded descriptor.
    let d = unsafe { &*t.mods[idx].desc };
    let host = d.flags & flags::HOST != 0;
    // SAFETY: the descriptor's tables, as the contract lays them out.
    let r = unsafe {
        crate::types::nabi_types_register(
            d.types.ptr,
            d.types.len,
            d.tags.keys,
            d.tags.tags,
            d.tags.n as usize,
        )
    };
    if r != 0 {
        let bad = items(&d.types)[r as usize - 1];
        // SAFETY: a descriptor of the module.
        let path = bytes(unsafe { &(*bad).path });
        return Err(Fail::new(kind::LAYOUT, path));
    }
    if !host {
        let roots: MBuf<(usize, usize)> = {
            let mut v = MBuf::new();
            for r in items(&d.roots) {
                v.push((r.word as usize, r.any));
            }
            v
        };
        // SAFETY: the module's scan tables and static words, mapped until
        // it is freed.
        let Some((slot, base)) = (unsafe {
            nabir_gc::modules::add(
                d.gc_types.tables,
                d.gc_types.n,
                roots.as_slice(),
                t.mods[idx].text,
            )
        }) else {
            // SAFETY: as above.
            unsafe { crate::types::nabi_types_unregister(d.types.ptr, d.types.len) };
            return Err(Fail::new(kind::OS, b"too many plugins loaded at once"));
        };
        for i in 0..d.gc_types.n as usize {
            // SAFETY: `n` writable ids.
            unsafe { d.gc_types.ids.add(i).write(base + i as u32) };
        }
        t.mods[idx].gc = slot;
    }
    Ok(())
}

/// Register the host's descriptor: once, before `main`.
///
/// # Safety
/// `m` is the program's `nabi_host_module`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nabi_module_register(m: *const ModuleDesc) -> i32 {
    lock();
    let t = table();
    // SAFETY: caller contract.
    let ok = unsafe { (*m).magic == MAGIC && (*m).flags & flags::HOST != 0 };
    let r = if !ok || t.mods[0].state != FREE {
        1
    } else {
        t.mods[0] = Module {
            desc: m,
            era: 1,
            state: LOADED,
            ..EMPTY
        };
        world::lock();
        world::stop();
        let r = register(0);
        world::resume();
        world::unlock();
        if r.is_err() {
            t.mods[0].state = FREE;
            2
        } else {
            rebuild_dispatch();
            0
        }
    };
    LOCK.unlock();
    r
}

/// Load the plugin at `path`: its handle, or 0 with `err` filled.
///
/// # Safety
/// `path` holds `len` bytes; `err` is null or writable.
pub(crate) unsafe fn raw_load(path: *const u8, len: usize, err: *mut PluginErr) -> usize {
    // SAFETY: caller contract.
    let path = unsafe { core::slice::from_raw_parts(path, len) };
    lock();
    let r = load(path);
    LOCK.unlock();
    match r {
        Ok(h) => h,
        Err(f) => {
            f.put(err);
            0
        }
    }
}

fn load(path: &[u8]) -> Result<usize, Fail> {
    free_dead();
    let Some(host) = host() else {
        return Err(Fail::new(
            kind::OS,
            b"this program does not load plugins: build it with `use std::plugin` reachable",
        ));
    };
    let h = os::open(path)?;
    let desc = os::symbol(h, b"nabi_module\0").cast::<ModuleDesc>();
    if desc.is_null() {
        os::close(h);
        return Err(Fail::new(kind::NOT_A_PLUGIN, path));
    }
    admit(host, h, desc, path)
}

/// Take in plugin `desc` (of image `h`, null in tests).
#[allow(clippy::many_single_char_names)]
fn admit(
    host: &ModuleDesc,
    h: *mut core::ffi::c_void,
    desc: *mut ModuleDesc,
    path: &[u8],
) -> Result<usize, Fail> {
    let t = table();
    if let Some(i) = t
        .mods
        .iter()
        .position(|m| m.state == LOADED && m.desc == desc.cast_const())
    {
        // The same file again: the OS gave the same image.
        os::close(h);
        return Ok(handle(i, &t.mods[i]));
    }
    // SAFETY: the plugin's exported descriptor.
    let d = unsafe { &*desc };
    if d.magic != MAGIC || d.version != VERSION || d.flags & flags::PLUGIN == 0 {
        os::close(h);
        return Err(Fail::new(kind::NOT_A_PLUGIN, path));
    }
    // The texts point into the image: copy them before closing it.
    if bytes(&d.toolchain) != bytes(&host.toolchain) {
        let f = Fail::new(kind::TOOLCHAIN, bytes(&d.toolchain)).with_b(bytes(&host.toolchain));
        os::close(h);
        return Err(f);
    }
    if bytes(&d.target) != bytes(&host.target) {
        let f = Fail::new(kind::TARGET, bytes(&d.target)).with_b(bytes(&host.target));
        os::close(h);
        return Err(f);
    }
    let Some(idx) = (1..MAX).find(|&i| matches!(t.mods[i].state, FREE | UNLOADED)) else {
        os::close(h);
        return Err(Fail::new(kind::OS, b"too many plugins loaded at once"));
    };
    t.seq += 1;
    let era = (t.mods[idx].era.wrapping_add(1) & ((1 << ERA_BITS) - 1)).max(1);
    let ranges = os::ranges(h, desc.cast());
    t.mods[idx] = Module {
        desc,
        os: h,
        path: keep(path),
        era,
        state: LOADED,
        seq: t.seq,
        text: ranges[0],
        image: ranges[1],
        ..EMPTY
    };
    let fail = |idx: usize, f: Fail| {
        let m = &mut table().mods[idx];
        m.state = FREE;
        os::close(m.os);
        Err(f)
    };
    if let Err(f) = check_needs(idx) {
        return fail(idx, f);
    }
    world::lock();
    world::stop();
    let r = bind(idx).and_then(|()| register(idx));
    world::resume();
    world::unlock();
    if let Err(f) = r {
        return fail(idx, f);
    }
    rebuild_dispatch();
    let m = &table().mods[idx];
    IMAGES[idx].0.store(m.image.0, Ordering::Release);
    IMAGES[idx].1.store(m.image.1, Ordering::Release);
    Ok(handle(idx, m))
}

/// The export of `path` in plugin `h` (`kind`: `FN_VALUE` for a
/// function value's code, `DATA` for a static's address); 0 with `err`.
///
/// # Safety
/// `path` holds `len` bytes; `err` is null or writable.
pub(crate) unsafe fn raw_find(
    h: usize,
    path: *const u8,
    len: usize,
    sig: u64,
    want: usize,
    err: *mut PluginErr,
) -> usize {
    // SAFETY: caller contract.
    let path = unsafe { core::slice::from_raw_parts(path, len) };
    lock();
    let r = find(h, path, sig, want);
    LOCK.unlock();
    match r {
        Ok(a) => a,
        Err(f) => {
            f.put(err);
            0
        }
    }
}

fn find(h: usize, path: &[u8], sig: u64, want: usize) -> Result<usize, Fail> {
    let gone = || Fail::new(kind::NOT_LOADED, b"");
    let i = by_handle(h).ok_or_else(gone)?;
    let md = &table().mods[i];
    if md.state != LOADED {
        return Err(gone());
    }
    // SAFETY: a loaded descriptor.
    let desc = unsafe { &*md.desc };
    let mut other = None;
    for e in items(&desc.exports) {
        if bytes(&e.path) != path {
            continue;
        }
        if e.kind != want {
            other = Some(e);
            continue;
        }
        if sig != 0 && e.sig != 0 && sig != e.sig {
            return Err(Fail::new(kind::WRONG_TYPE, path).with_b(bytes(&e.sig_text)));
        }
        return Ok(e.addr);
    }
    if other.is_some() {
        return Err(Fail::new(kind::WRONG_TYPE, path));
    }
    let mut f = Fail::new(kind::NO_SUCH_ITEM, path);
    near(desc, path, &mut |n| f.name(n));
    Err(f)
}

/// The export paths of `d` close to `path`: one edit or less per three
/// bytes, or sharing the last segment.
fn near(d: &ModuleDesc, path: &[u8], f: &mut impl FnMut(&[u8])) {
    let last = |p: &'static [u8]| -> &'static [u8] {
        p.iter()
            .rposition(|&c| c == b':')
            .map_or(p, |k| &p[k + 1..])
    };
    let want_last = path
        .iter()
        .rposition(|&c| c == b':')
        .map_or(path, |k| &path[k + 1..]);
    let mut shown = 0;
    let mut prev: &[u8] = &[];
    for e in items(&d.exports) {
        let p = bytes(&e.path);
        if p.is_empty() || p == prev || shown == 5 {
            continue;
        }
        if last(p) == want_last || distance(p, path) <= path.len().max(3) / 3 {
            f(p);
            shown += 1;
            prev = p;
        }
    }
}

/// Edit distance of short paths (longer ones compare as far).
fn distance(a: &[u8], b: &[u8]) -> usize {
    const N: usize = 64;
    if a.len() >= N || b.len() >= N {
        return usize::MAX;
    }
    let mut row = [0usize; N];
    for (j, r) in row.iter_mut().enumerate().take(b.len() + 1) {
        *r = j;
    }
    for i in 1..=a.len() {
        let mut diag = row[0];
        row[0] = i;
        for j in 1..=b.len() {
            let up = row[j];
            row[j] = (diag + usize::from(a[i - 1] != b[j - 1]))
                .min(up + 1)
                .min(row[j - 1] + 1);
            diag = up;
        }
    }
    row[b.len()]
}

/// Copy the export paths of plugin `h` close to `path` into `out`
/// ('\n'-joined, at most `cap` bytes); the bytes written.
///
/// # Safety
/// `path` holds `len` bytes, `out` `cap`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nabi_plugin_near(
    h: usize,
    path: *const u8,
    len: usize,
    out: *mut u8,
    cap: usize,
) -> usize {
    // SAFETY: caller contract.
    let (path, out) = unsafe {
        (
            core::slice::from_raw_parts(path, len),
            core::slice::from_raw_parts_mut(out, cap),
        )
    };
    lock();
    let mut n = 0;
    if let Some(i) = by_handle(h) {
        // SAFETY: a registered descriptor.
        let d = unsafe { &*table().mods[i].desc };
        near(d, path, &mut |p| {
            let sep = usize::from(n != 0);
            if n + sep + p.len() <= out.len() {
                if sep == 1 {
                    out[n] = b'\n';
                }
                out[n + sep..n + sep + p.len()].copy_from_slice(p);
                n += sep + p.len();
            }
        });
    }
    LOCK.unlock();
    n
}

/// Plugin `h`'s name (0), version (1) or path (2) into `out`; 0 on
/// success.
///
/// # Safety
/// `out` is writable.
pub(crate) unsafe fn raw_info(h: usize, what: usize, out: *mut Str) -> i32 {
    lock();
    let r = by_handle(h).map(|i| {
        let m = &table().mods[i];
        // SAFETY: a registered descriptor.
        let d = unsafe { &*m.desc };
        match what {
            0 => d.name,
            1 => items(&d.provides)
                .first()
                .map_or(Slice::EMPTY, |p| p.version),
            _ => m.path,
        }
    });
    LOCK.unlock();
    match r {
        Some(s) => {
            // SAFETY: caller contract.
            unsafe { out.write(s) };
            0
        }
        None => 1,
    }
}

/// How many plugins are loaded (not unloading).
#[unsafe(no_mangle)]
pub extern "C" fn nabi_plugin_count() -> usize {
    lock();
    let n = table().mods[1..]
        .iter()
        .filter(|m| m.state == LOADED)
        .count();
    LOCK.unlock();
    n
}

/// The handle of the `i`th loaded plugin, in load order; 0 past the end.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_plugin_at(i: usize) -> usize {
    lock();
    let t = table();
    let mut v: MBuf<(u64, usize)> = MBuf::new();
    for (k, m) in t.mods.iter().enumerate().skip(1) {
        if m.state == LOADED {
            v.push((m.seq, k));
        }
    }
    v.as_mut_slice().sort_unstable();
    let h = v
        .as_slice()
        .get(i)
        .map_or(0, |&(_, k)| handle(k, &t.mods[k]));
    LOCK.unlock();
    h
}

/// The address of plugin `h`'s `i`th type descriptor; 0 past the end or
/// once `h` is unmapped.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_plugin_types(h: usize, i: usize) -> usize {
    lock();
    let mapped = by_handle(h).filter(|&k| matches!(table().mods[k].state, LOADED | RETIRED));
    let r = mapped.map_or(0, |k| {
        // SAFETY: a registered descriptor.
        let types = unsafe { (*table().mods[k].desc).types };
        items(&types).get(i).map_or(0, |&d| d as usize)
    });
    LOCK.unlock();
    r
}

/// 0 loaded, 1 unloading (retired), 2 unloaded.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_plugin_state(h: usize) -> i32 {
    lock();
    if by_handle(h).is_some_and(|i| table().mods[i].state == RETIRED) {
        LOCK.unlock();
        nabir_gc::collect();
        lock();
    }
    free_dead();
    let s = match by_handle(h).map(|i| table().mods[i].state) {
        Some(LOADED) => 0,
        Some(RETIRED) => 1,
        _ => 2,
    };
    LOCK.unlock();
    s
}

/// Unload plugin `h`: 0 done, 1 pending, 2 kept (`why` says why).
///
/// # Safety
/// `why` is null or writable.
pub(crate) unsafe fn raw_unload(h: usize, why: *mut PluginErr) -> i32 {
    lock();
    let r = retire(h);
    LOCK.unlock();
    match r {
        Ok(true) => {}
        Ok(false) => return 0,
        Err((code, f)) => {
            f.put(why);
            return code;
        }
    }
    nabir_gc::collect();
    lock();
    free_dead();
    let done = by_handle(h).is_none_or(|i| table().mods[i].state == UNLOADED);
    LOCK.unlock();
    if done {
        return 0;
    }
    lock();
    let gc = by_handle(h).map_or(usize::MAX, |i| table().mods[i].gc);
    LOCK.unlock();
    let what: &[u8] = if gc != usize::MAX && nabir_gc::modules::why(gc) == nabir_gc::modules::CODE {
        b"a value still points into its code (a closure or function value it made, a `str` of its text)"
    } else {
        b"an object of one of its types is still alive"
    };
    let mut f = Fail::new(kind::OS, what);
    f.a.extend(
        b"; drop it, then the plugin unloads at a later collection (`plugin::wait_unloaded`)",
    );
    f.put(why);
    1
}

/// Take plugin `h` out of lookup: `Ok(true)` when it was loaded,
/// `Ok(false)` when it is already gone.
fn retire(h: usize) -> Result<bool, (i32, Fail)> {
    let t = table();
    let Some(i) = by_handle(h).filter(|&i| i != 0) else {
        return Ok(false);
    };
    if t.mods[i].state != LOADED {
        return Ok(false);
    }
    for (k, m) in t.mods.iter().enumerate() {
        if k != i && matches!(m.state, LOADED | RETIRED) && m.deps[i / 64] & (1 << (i % 64)) != 0 {
            // SAFETY: a registered descriptor.
            let mut why = Fail::new(kind::OS, bytes(unsafe { &(*m.desc).name }));
            why.a.extend(b" uses it; unload ");
            why.a.extend(bytes(unsafe { &(*m.desc).name }));
            why.a.extend(b" first");
            return Err((1, why));
        }
    }
    // SAFETY: a registered descriptor.
    if unsafe { (*t.mods[i].desc).flags } & flags::NO_UNLOAD != 0 {
        let why = Fail::new(
            kind::OS,
            b"it gave a C callback out; plugins that do stay loaded",
        );
        return Err((2, why));
    }
    t.mods[i].state = RETIRED;
    world::lock();
    nabir_gc::modules::retire(t.mods[i].gc);
    world::unlock();
    rebuild_dispatch();
    Ok(true)
}

/// Free every retired module a collection found dead.
#[allow(clippy::needless_range_loop)] // `t.mods` changes inside
fn free_dead() {
    let t = table();
    for i in 1..MAX {
        let m = t.mods[i];
        if m.state != RETIRED || !nabir_gc::modules::dead(m.gc) {
            continue;
        }
        if t.mods
            .iter()
            .any(|o| matches!(o.state, LOADED | RETIRED) && o.deps[i / 64] & (1 << (i % 64)) != 0)
        {
            continue;
        }
        world::lock();
        world::stop();
        nabir_gc::modules::remove(m.gc);
        // SAFETY: the module's own descriptors.
        unsafe { crate::types::nabi_types_unregister((*m.desc).types.ptr, (*m.desc).types.len) };
        world::resume();
        world::unlock();
        IMAGES[i].1.store(0, Ordering::Release);
        IMAGES[i].0.store(0, Ordering::Release);
        os::close(m.os);
        t.mods[i] = Module {
            era: m.era,
            state: UNLOADED,
            ..EMPTY
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nabir_abi::module::{GcTypes, Import, Root, TagKey, Tags, sym};
    use nabir_abi::typedesc::{TypeDesc, fnv64, kind as tkind};
    use std::sync::atomic::{AtomicBool, AtomicU32};

    const fn s(text: &'static str) -> Str {
        Slice {
            ptr: text.as_ptr(),
            len: text.len(),
        }
    }

    fn slice<T>(v: Vec<T>) -> Slice<T> {
        let v = v.leak();
        Slice {
            ptr: v.as_ptr(),
            len: v.len(),
        }
    }

    const fn export(name: &'static str, path: &'static str, addr: usize, kind: usize) -> Export {
        Export {
            sym: s(name),
            path: s(path),
            sig: 0,
            addr,
            kind,
            sig_text: s(""),
        }
    }

    fn import(name: &'static str) -> (Import, *mut usize) {
        let slot = Box::leak(Box::new(0usize));
        (
            Import {
                sym: s(name),
                sig: 0,
                slot,
                kind: sym::FN,
            },
            slot,
        )
    }

    const fn pkg(name: &'static str, p8: [u8; 8], seed: u8) -> PkgDesc {
        PkgDesc {
            name: s(name),
            version: s("1.0.0"),
            pkg8: p8,
            sha256: [seed; 32],
        }
    }

    fn tdesc(path: &'static str, layout_hash: u64) -> *const TypeDesc {
        Box::leak(Box::new(TypeDesc {
            magic: nabir_abi::typedesc::MAGIC,
            version: 1,
            kind: tkind::STRUCT,
            flags: 0,
            tag_index: 0,
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

    #[derive(Default)]
    struct Spec {
        flags: u16,
        toolchain: &'static str,
        name: &'static str,
        provides: Vec<PkgDesc>,
        needs: Vec<PkgDesc>,
        imports: Vec<Import>,
        exports: Vec<Export>,
        types: Vec<*const TypeDesc>,
        tags: Vec<(&'static str, u32)>,
        dispatch: Vec<Dispatch>,
        gc: usize,
        roots: Vec<Root>,
    }

    fn module(sp: Spec) -> *mut ModuleDesc {
        static TABLE: [u32; 4] = [24, 1, 0, 0];
        let keys: Vec<TagKey> = sp
            .tags
            .iter()
            .map(|&(k, _)| TagKey {
                key: s(k),
                hash: fnv64(k.as_bytes()),
            })
            .collect();
        let tags: Vec<u32> = sp.tags.iter().map(|&(_, t)| t).collect();
        let nt = tags.len() as u32;
        let tables: Vec<*const u8> = (0..sp.gc).map(|_| TABLE.as_ptr().cast()).collect();
        let ids: Vec<u32> = vec![0; sp.gc];
        let mut exports = sp.exports;
        exports.sort_by(|a, b| bytes(&a.sym).cmp(bytes(&b.sym)));
        Box::leak(Box::new(ModuleDesc {
            magic: MAGIC,
            version: VERSION,
            flags: sp.flags,
            hot_version: 0,
            text_lo: 0,
            text_hi: 0,
            toolchain: s(if sp.toolchain.is_empty() {
                "nabi test"
            } else {
                sp.toolchain
            }),
            target: s("test-target"),
            name: s(sp.name),
            provides: slice(sp.provides),
            needs: slice(sp.needs),
            imports: slice(sp.imports),
            exports: slice(exports),
            gc_types: GcTypes {
                tables: tables.leak().as_ptr(),
                ids: ids.leak().as_mut_ptr(),
                n: sp.gc as u32,
                reserved: 0,
            },
            tags: Tags {
                keys: keys.leak().as_ptr(),
                tags: tags.leak().as_mut_ptr(),
                n: nt,
                reserved: 0,
            },
            roots: slice(sp.roots),
            types: slice(sp.types),
            dispatch: slice(sp.dispatch),
            functions: Slice::EMPTY,
            hot: core::ptr::null(),
            reserved: 0,
        }))
    }

    fn admit_locked(d: *mut ModuleDesc) -> Result<usize, Fail> {
        lock();
        let host = host().expect("host registered");
        let r = admit(host, ptr::null_mut(), d, b"test.plugin");
        LOCK.unlock();
        r
    }

    fn unload(h: usize) -> (i32, Vec<u8>) {
        let mut e = PluginErr {
            kind: 0,
            reserved: 0,
            a: Slice::EMPTY,
            b: Slice::EMPTY,
            list: Slice::EMPTY,
        };
        // SAFETY: a writable `PluginErr`.
        let r = unsafe { raw_unload(h, &raw mut e) };
        (r, bytes(&e.a).to_vec())
    }

    const fn list(f: &Fail) -> &[u8] {
        f.list.as_slice()
    }

    /// One process-wide loader: every case in one test, in order.
    #[test]
    #[allow(clippy::cognitive_complexity)]
    fn loads_binds_registers_and_unloads() {
        let _gc = crate::tests::GC_TEST
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _m = crate::tests::Mutator::attach();
        let host = module(Spec {
            flags: flags::HOST,
            name: "host",
            exports: vec![
                export("_Yaaaaaaaa_engine_tick", "", 0x1111, sym::FN),
                export("_Yaaaaaaaa_engine_WORLD", "", 0x2222, sym::DATA),
                export("nabi_gc_alloc_slow", "", 0x3333, sym::FN),
            ],
            types: vec![tdesc("plug::Body", 7)],
            tags: vec![("plug::Body", 900)],
            ..Spec::default()
        });
        // SAFETY: a host descriptor.
        assert_eq!(unsafe { nabi_module_register(host) }, 0);

        // Imports bind to the host; a package's own symbols to the module.
        let (i1, s1) = import("_Yaaaaaaaa_engine_tick");
        let (i2, s2) = import("nabi_gc_alloc_slow");
        let (i3, s3) = import("_Ybbbbbbbb_enemies_STATE");
        let (i4, _) = import("_Yaaaaaaaa_engine_WORLD");
        let a = module(Spec {
            flags: flags::PLUGIN,
            name: "enemies",
            provides: vec![pkg("enemies", *b"bbbbbbbb", 2)],
            imports: vec![i1, i4, i2, i3],
            exports: vec![
                export("_Ybbbbbbbb_enemies_STATE", "", 0x4444, sym::DATA),
                export(
                    "_Ybbbbbbbb_enemies_spawn.v",
                    "enemies::spawn",
                    0x5555,
                    sym::FN_VALUE,
                ),
            ],
            types: vec![tdesc("plug::Body", 7), tdesc("plug::Orc", 1)],
            tags: vec![("plug::Body", u32::MAX), ("plug::Orc", u32::MAX)],
            dispatch: vec![Dispatch {
                method: 77,
                code: 0x6666,
                tag_index: 1,
            }],
            gc: 2,
            ..Spec::default()
        });
        let ha = admit_locked(a).unwrap_or_else(|f| panic!("{:?}", core::str::from_utf8(list(&f))));
        // SAFETY: slots the module made.
        unsafe {
            assert_eq!((*s1, *s2, *s3), (0x1111, 0x3333, 0x4444));
        }
        // SAFETY: the module's tables.
        let (tags, ids) = unsafe { ((*a).tags.tags, (*a).gc_types.ids) };
        // SAFETY: two of each.
        unsafe {
            assert_eq!(*tags, 900, "a key the host has keeps its tag");
            assert_ne!(*tags.add(1), u32::MAX);
            assert_eq!(*ids.add(1), *ids + 1);
            assert_eq!(nabi_any_dispatch(77, *tags.add(1)), 0x6666);
        }
        assert_eq!(nabi_plugin_count(), 1);
        assert_eq!(nabi_plugin_at(0), ha);
        // SAFETY: plain arguments.
        unsafe {
            let p = b"enemies::spawn";
            let mut e = core::mem::zeroed::<PluginErr>();
            assert_eq!(
                raw_find(ha, p.as_ptr(), p.len(), 0, sym::FN_VALUE, &raw mut e),
                0x5555
            );
            let q = b"enemies::spwan";
            assert_eq!(
                raw_find(ha, q.as_ptr(), q.len(), 0, sym::FN_VALUE, &raw mut e),
                0
            );
            assert_eq!(e.kind, kind::NO_SUCH_ITEM);
            assert_eq!(bytes(&e.list), p);
            assert_eq!(
                raw_find(ha, p.as_ptr(), p.len(), 0, sym::DATA, &raw mut e),
                0
            );
            assert_eq!(e.kind, kind::WRONG_TYPE);
        }

        // A missing import is named; nothing stays registered.
        let (m1, _) = import("_Yaaaaaaaa_engine_gone");
        let (m2, _) = import("nabi_not_there");
        let bad = module(Spec {
            flags: flags::PLUGIN,
            name: "broken",
            imports: vec![m1, m2],
            ..Spec::default()
        });
        let f = admit_locked(bad).expect_err("refused");
        assert_eq!(f.kind, kind::MISSING_SYMBOLS);
        assert_eq!(list(&f), b"_Yaaaaaaaa_engine_gone\nnabi_not_there");
        assert_eq!(nabi_plugin_count(), 1);

        // A layout clash names the type.
        let clash = module(Spec {
            flags: flags::PLUGIN,
            name: "clash",
            types: vec![tdesc("plug::Body", 8)],
            tags: vec![("plug::Body", u32::MAX)],
            ..Spec::default()
        });
        let f = admit_locked(clash).expect_err("refused");
        assert_eq!((f.kind, f.a.as_slice()), (kind::LAYOUT, &b"plug::Body"[..]));

        // Toolchain and packages.
        let other = module(Spec {
            flags: flags::PLUGIN,
            toolchain: "nabi other",
            ..Spec::default()
        });
        let f = admit_locked(other).expect_err("refused");
        assert_eq!(
            (f.kind, f.b.as_slice()),
            (kind::TOOLCHAIN, &b"nabi test"[..])
        );
        let needy = module(Spec {
            flags: flags::PLUGIN,
            needs: vec![
                pkg("noise", *b"cccccccc", 9),
                pkg("enemies", *b"bbbbbbbb", 2),
            ],
            ..Spec::default()
        });
        let f = admit_locked(needy).expect_err("refused");
        assert_eq!(
            (f.kind, list(&f)),
            (kind::MISSING_PACKAGES, &b"noise 1.0.0"[..])
        );

        // A plugin that uses `enemies` holds it loaded.
        let (u1, us) = import("_Ybbbbbbbb_enemies_STATE");
        let b = module(Spec {
            flags: flags::PLUGIN,
            name: "waves",
            needs: vec![pkg("enemies", *b"bbbbbbbb", 2)],
            imports: vec![u1],
            ..Spec::default()
        });
        let hb = admit_locked(b).unwrap_or_else(|_| panic!("waves"));
        // SAFETY: its slot.
        assert_eq!(unsafe { *us }, 0x4444);
        let (r, why) = unload(ha);
        assert_eq!(r, 1);
        assert_eq!(why, b"waves uses it; unload waves first");
        assert_eq!(unload(hb).0, 0);
        assert_eq!(nabi_plugin_state(hb), 2);

        // An object of its types keeps it until it is dropped; what its
        // statics hold does not.
        // SAFETY: the module's ids.
        let id = unsafe { *ids };
        let held = nabir_gc::alloc(24, 8, id);
        // A root stays one for the rest of the process: not a word on
        // this thread's stack, which later collections would read.
        let root = Box::leak(Box::new(AtomicUsize::new(held as usize)));
        nabir_gc::world::add_root(root.as_ptr());
        assert_eq!(unload(ha).0, 1);
        assert_eq!(nabi_plugin_state(ha), 1);
        assert_eq!(nabi_plugin_count(), 0, "out of lookup at once");
        assert_eq!(nabi_any_dispatch(77, 901), 0);
        root.store(0, Ordering::Relaxed);
        assert_eq!(nabi_plugin_state(ha), 2);
        assert_eq!(nabi_plugin_count(), 0);
        loads_while_others_collect();
    }

    /// Plugins come and go while other threads allocate objects of
    /// their types and collect.
    fn loads_while_others_collect() {
        static STOP: AtomicBool = AtomicBool::new(false);
        static ID: AtomicU32 = AtomicU32::new(0);
        let workers: Vec<_> = (0..4)
            .map(|k| {
                std::thread::spawn(move || {
                    let _m = crate::tests::Mutator::attach();
                    let mut n = 0u32;
                    while !STOP.load(Ordering::Relaxed) {
                        let id = ID.load(Ordering::Relaxed);
                        if id != 0 {
                            let o = nabir_gc::alloc(24, 8, id);
                            assert!(!o.is_null());
                        }
                        n += 1;
                        if n % 64 == k {
                            nabir_gc::collect();
                        }
                        nabir_gc::world::safepoint();
                    }
                })
            })
            .collect();
        for round in 0..40 {
            let word = Box::leak(Box::new(0usize));
            let m = module(Spec {
                flags: flags::PLUGIN,
                name: "churn",
                gc: 3,
                roots: vec![Root {
                    word: &raw const *word,
                    any: 0,
                }],
                ..Spec::default()
            });
            let h = admit_locked(m).unwrap_or_else(|_| panic!("round {round}"));
            // SAFETY: its ids.
            let id = unsafe { *(*m).gc_types.ids };
            *word = nabir_gc::alloc(24, 8, id) as usize;
            ID.store(id, Ordering::Relaxed);
            for _ in 0..50 {
                nabir_gc::world::safepoint();
            }
            ID.store(0, Ordering::Relaxed);
            let _ = unload(h);
            for _ in 0..4 {
                if nabi_plugin_state(h) == 2 {
                    break;
                }
            }
            assert_eq!(nabi_plugin_state(h), 2, "round {round}");
        }
        STOP.store(true, Ordering::Relaxed);
        // Blocked in `join`: out of the way of their collections.
        nabir_gc::world::enter_native();
        for w in workers {
            w.join().unwrap();
        }
        nabir_gc::world::leave_native();
    }
}
