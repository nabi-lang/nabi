//! The versions of the hot packages, the queue of loads, and applying
//! one: slots, statics, and the migration of changed types.
//!
//! Loads apply in the order they came. One whose types keep their
//! layouts is loaded and bound at once. One that changes a layout waits
//! until no frame or value of old code depends on a changed type; then a
//! dry collection checks the heap, the plugin is loaded, the defaults of
//! added fields are made, and a second collection rewrites the objects
//! (`nabir_gc::migrate`) and binds the slots in its pause.

use crate::hot::channel::{self, push_num};
use crate::hot::frames::Frames;
use crate::plugin::PluginErr;
use crate::str::{NabiStr, build};
use core::cell::UnsafeCell;
use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use nabir_abi::gc::ANY_BOXED;
use nabir_abi::module::{
    Export, GcKey, HOT_MAGIC, HotDesc, MAGIC, ModuleDesc, SymSig, flags, gc_kind, hot_flags, sym,
};
use nabir_abi::typedesc::{
    FieldDesc, NO_OFFSET, NO_TAG, Slice, Str, TypeDesc, TypeRef, field, fnv64, kind,
};
use nabir_common::header::HEADER_SIZE;
use nabir_common::sys::Lock;
use nabir_common::wtf8::MBuf;
use nabir_gc::migrate::{self, Block, Outcome, Plan, Rule};
use nabir_gc::world;

/// One loaded version of a hot package.
#[derive(Clone, Copy)]
struct Version {
    desc: *const ModuleDesc,
    applied: bool,
}

/// A `load` not answered yet.
#[derive(Clone, Copy)]
struct Load {
    path: Str,
    desc: *const ModuleDesc,
    /// The number `nabi dev` gave it (its file name's), for replies.
    n: u64,
    /// The loader has it (registered, not applied).
    loaded: bool,
    /// Hash of the last `waiting` reason sent.
    why: u64,
}

struct Hot {
    versions: MBuf<Version>,
    /// Plugin handles `discover` has looked at.
    seen: MBuf<usize>,
    queue: MBuf<Load>,
    /// Applied changes for `point()`'s hooks, as `nabi_hot_events` gives
    /// them.
    events: MBuf<u8>,
}

struct Cell(UnsafeCell<Hot>);
// SAFETY: only touched under `LOCK`.
unsafe impl Sync for Cell {}

static LOCK: Lock = Lock::new();
static HOT: Cell = Cell(UnsafeCell::new(Hot {
    versions: MBuf::new(),
    seen: MBuf::new(),
    queue: MBuf::new(),
    events: MBuf::new(),
}));
static HOLD: AtomicBool = AtomicBool::new(false);
/// The buffer of prepared defaults: a root while a migration runs.
static DEFAULTS: AtomicUsize = AtomicUsize::new(0);
static DEFAULTS_ROOTED: AtomicBool = AtomicBool::new(false);

/// Take `LOCK` as a blocking call: a stop does not wait for a thread
/// blocked on it.
fn lock() -> &'static mut Hot {
    world::enter_native();
    LOCK.lock();
    world::leave_native();
    // SAFETY: under `LOCK` until `unlock`.
    unsafe { &mut *HOT.0.get() }
}

fn unlock() {
    LOCK.unlock();
}

const fn items<T>(s: &Slice<T>) -> &'static [T] {
    if s.len == 0 || s.ptr.is_null() {
        return &[];
    }
    // SAFETY: descriptor tables live as long as their module, and hot
    // modules are never unmapped.
    unsafe { core::slice::from_raw_parts(s.ptr, s.len) }
}

#[allow(clippy::trivially_copy_pass_by_ref)] // 8 bytes on 32-bit targets only
const fn text(s: &Str) -> &'static [u8] {
    items(s)
}

/// A copy of `b` that lives on.
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

const fn md(d: *const ModuleDesc) -> &'static ModuleDesc {
    // SAFETY: a mapped descriptor of a hot module (never unmapped).
    unsafe { &*d }
}

fn hot_of(d: &ModuleDesc) -> Option<&'static HotDesc> {
    // SAFETY: `hot` is null or the module's HotDesc.
    let h = unsafe { d.hot.as_ref()? };
    (h.magic == HOT_MAGIC).then_some(h)
}

/// The export `name` of `d`.
fn export(d: &ModuleDesc, name: &[u8]) -> Option<&'static Export> {
    let e = items(&d.exports);
    e.binary_search_by(|x| text(&x.sym).cmp(name))
        .ok()
        .map(|k| &e[k])
}

/// The sig of hot function `name` as `d`'s code calls it (0: unknown).
fn sig_of(d: &ModuleDesc, name: &[u8]) -> u64 {
    let Some(h) = hot_of(d) else { return 0 };
    let s: &[SymSig] = items(&h.sigs);
    s.binary_search_by(|x| text(&x.sym).cmp(name))
        .map_or(0, |k| s[k].sig)
}

/// The descriptor of type `key` in `d`'s `types`.
fn desc_of(d: &ModuleDesc, key: u64) -> Option<&'static TypeDesc> {
    items(&d.types)
        .iter()
        // SAFETY: the module's descriptors.
        .map(|&t| unsafe { &*t })
        .find(|t| t.key_hash == key)
}

// Opening a file to read its descriptor before the loader takes it.

/// Map the plugin at `path` and find its descriptor (null: none), as a
/// blocking call.
fn open(path: &[u8]) -> Result<*const ModuleDesc, MBuf<u8>> {
    world::enter_native();
    let r = os_open(path);
    world::leave_native();
    r
}

#[cfg(unix)]
fn os_open(path: &[u8]) -> Result<*const ModuleDesc, MBuf<u8>> {
    let mut p: MBuf<u8> = MBuf::new();
    p.extend(path);
    p.push(0);
    // SAFETY: a NUL-terminated path; the image stays mapped (hot modules
    // are never unloaded).
    unsafe {
        let h = libc::dlopen(
            p.as_slice().as_ptr().cast(),
            libc::RTLD_NOW | libc::RTLD_LOCAL,
        );
        if h.is_null() {
            let e = libc::dlerror();
            let mut m = MBuf::new();
            m.extend(b"cannot open it: ");
            if !e.is_null() {
                m.extend(core::ffi::CStr::from_ptr(e).to_bytes());
            }
            return Err(m);
        }
        let d = libc::dlsym(h, c"nabi_module".as_ptr());
        Ok(d.cast())
    }
}

#[cfg(windows)]
fn os_open(path: &[u8]) -> Result<*const ModuleDesc, MBuf<u8>> {
    unsafe extern "system" {
        fn LoadLibraryExW(
            name: *const u16,
            file: *mut core::ffi::c_void,
            flags: u32,
        ) -> *mut core::ffi::c_void;
    }
    let mut w = nabir_common::wtf8::wide(path);
    // SAFETY: a NUL-terminated wide path; altered search path, as the
    // loader opens it.
    unsafe {
        let h = LoadLibraryExW(w.ptr(), ptr::null_mut(), 8);
        if h.is_null() {
            let mut m = MBuf::new();
            m.extend(b"cannot open it: system error ");
            push_num(&mut m, u64::from(nabir_common::win::GetLastError()));
            return Err(m);
        }
        let d = nabir_common::win::GetProcAddress(h as _, c"nabi_module".as_ptr().cast());
        Ok(d as *const ModuleDesc)
    }
}

/// The `n` of a file named `<pkg>.<n>.<ext>`.
fn file_number(path: &[u8]) -> Option<(Str, u64)> {
    let name = path
        .iter()
        .rposition(|&c| c == b'/' || c == b'\\')
        .map_or(path, |k| &path[k + 1..]);
    let mut parts = name.rsplitn(3, |&c| c == b'.');
    let _ext = parts.next()?;
    let n = parts.next()?;
    let pkg = parts.next()?;
    let mut v = 0u64;
    for &d in n {
        if !d.is_ascii_digit() {
            return None;
        }
        v = v.checked_mul(10)?.checked_add(u64::from(d - b'0'))?;
    }
    Some((keep(pkg), v))
}

/// Add the hot plugins loaded by `std::plugin` (the first versions).
fn discover(h: &mut Hot) {
    for i in 0..crate::plugin::nabi_plugin_count() {
        let p = crate::plugin::nabi_plugin_at(i);
        if h.seen.as_slice().contains(&p) {
            continue;
        }
        h.seen.push(p);
        let mut path = Str::EMPTY;
        // SAFETY: a writable `Str`.
        if unsafe { crate::plugin::raw_info(p, 2, &raw mut path) } != 0 {
            continue;
        }
        let Ok(d) = open(text(&path)) else { continue };
        if d.is_null() || h.versions.as_slice().iter().any(|v| v.desc == d) {
            continue;
        }
        if h.queue.as_slice().iter().any(|l| l.desc == d) {
            continue;
        }
        let m = md(d);
        if m.magic == MAGIC && m.flags & flags::HOT != 0 {
            h.versions.push(Version {
                desc: d,
                applied: true,
            });
        }
    }
}

/// The applied version of package `name` with the highest
/// `hot_version`.
fn live(h: &Hot, name: &[u8]) -> Option<*const ModuleDesc> {
    h.versions
        .as_slice()
        .iter()
        .filter(|v| v.applied && text(&md(v.desc).name) == name)
        .max_by_key(|v| md(v.desc).hot_version)
        .map(|v| v.desc)
}

/// One line from `nabi dev`.
pub(crate) fn request(line: &[u8]) {
    let Some(path) = line.strip_prefix(b"load ") else {
        reply(
            b"refused ",
            line,
            None,
            b": unknown request; `nabi dev` sends `load <path>`",
        );
        return;
    };
    let (stem, n) = file_number(path).unwrap_or_else(|| (keep(path), 0));
    let d = match open(path) {
        Ok(d) if !d.is_null() => d,
        Ok(_) => {
            reply(
                b"refused ",
                text(&stem),
                Some(n),
                b": not a Nabi plugin; build it with `nabic build --hot`",
            );
            return;
        }
        Err(e) => {
            let mut m: MBuf<u8> = MBuf::new();
            m.extend(b": ");
            m.extend(e.as_slice());
            reply(b"refused ", text(&stem), Some(n), m.as_slice());
            return;
        }
    };
    let m = md(d);
    if m.magic != MAGIC || m.flags & flags::HOT == 0 || hot_of(m).is_none() {
        reply(
            b"refused ",
            text(&stem),
            Some(n),
            b": not a hot plugin; build it with `nabic build --hot`",
        );
        return;
    }
    let h = lock();
    discover(h);
    let load = Load {
        path: keep(path),
        desc: d,
        n,
        loaded: false,
        why: 0,
    };
    let name = text(&m.name);
    let mut gone = None;
    if let Some(k) = h
        .queue
        .as_slice()
        .iter()
        .position(|l| text(&md(l.desc).name) == name)
    {
        gone = Some(h.queue.as_slice()[k]);
        h.queue.as_mut_slice()[k] = load;
    } else {
        h.queue.push(load);
    }
    unlock();
    if let Some(g) = gone {
        reply(b"superseded ", name, Some(g.n), b"");
    }
}

/// Send `<what><pkg> v<n><rest>`.
fn reply(what: &[u8], pkg: &[u8], n: Option<u64>, rest: &[u8]) {
    let mut m: MBuf<u8> = MBuf::new();
    m.extend(what);
    m.extend(pkg);
    if let Some(n) = n {
        m.extend(b" v");
        push_num(&mut m, n);
    }
    m.extend(rest);
    channel::send(m.as_slice());
}

pub(crate) fn hold(on: bool) {
    HOLD.store(on, Ordering::Release);
}

/// From the channel thread: work on the queue (unless `point()` must).
pub(crate) fn tick() {
    if !HOLD.load(Ordering::Acquire) {
        run_queue();
    }
}

/// From `reload::point()`.
pub(crate) fn point() {
    run_queue();
}

/// Lines of what `point()` would apply.
pub(crate) fn ready() -> NabiStr {
    let h = lock();
    let mut m: MBuf<u8> = MBuf::new();
    for l in h.queue.as_slice() {
        let d = md(l.desc);
        let ch = live(h, text(&d.name)).map(|old| Changes::of(md(old), d));
        event(&mut m, d, l.n, ch.as_ref());
    }
    unlock();
    build(&[m.as_slice()])
}

/// The applied changes since the last call.
pub(crate) fn events() -> NabiStr {
    let h = lock();
    let s = build(&[h.events.as_slice()]);
    h.events.truncate(0);
    unlock();
    s
}

/// `package \t n \t functions \t types` and a newline.
fn event(m: &mut MBuf<u8>, d: &ModuleDesc, n: u64, ch: Option<&Changes>) {
    m.extend(text(&d.name));
    m.push(b'\t');
    push_num(m, n);
    m.extend(b"\t\t");
    if let Some(ch) = ch {
        for (k, &(_, t)) in ch.types.as_slice().iter().enumerate() {
            if k > 0 {
                m.push(b',');
            }
            // SAFETY: a descriptor of the module.
            m.extend(text(unsafe { &(*t).path }));
        }
    }
    m.push(b'\n');
}

enum Step {
    /// Answered (the line to send).
    Done(MBuf<u8>),
    /// Still waiting, and why.
    Wait(MBuf<u8>),
}

fn line(parts: &[&[u8]]) -> MBuf<u8> {
    let mut m = MBuf::new();
    for p in parts {
        m.extend(p);
    }
    m
}

/// Apply what can be, in order; send the replies.
fn run_queue() {
    loop {
        let h = lock();
        discover(h);
        let Some(&first) = h.queue.as_slice().first() else {
            unlock();
            return;
        };
        let mut head = first;
        let step = process(h, &mut head);
        let pkg = text(&md(head.desc).name);
        let mut num: MBuf<u8> = MBuf::new();
        num.extend(b" v");
        push_num(&mut num, head.n);
        match step {
            Step::Done(answer) => {
                h.queue.as_mut_slice().copy_within(1.., 0);
                let left = h.queue.len() - 1;
                h.queue.truncate(left);
                unlock();
                channel::send(answer.as_slice());
            }
            Step::Wait(why) => {
                let hash = fnv64(why.as_slice());
                let send = hash != head.why;
                head.why = hash;
                if let Some(at) = h.queue.as_mut_slice().first_mut() {
                    *at = head;
                }
                unlock();
                if send {
                    channel::send(
                        line(&[b"waiting ", pkg, num.as_slice(), b": ", why.as_slice()]).as_slice(),
                    );
                }
                return;
            }
        }
    }
}

/// Try to apply load `l` (the queue's head).
#[allow(clippy::many_single_char_names)]
fn process(h: &mut Hot, l: &mut Load) -> Step {
    let d = md(l.desc);
    let name = text(&d.name);
    let mut num: MBuf<u8> = MBuf::new();
    num.extend(b" v");
    push_num(&mut num, l.n);
    let refused = |why: &[u8]| Step::Done(line(&[b"refused ", name, num.as_slice(), b": ", why]));
    let Some(old) = live(h, name) else {
        return refused(b"not a hot package of this program; restart with `r`");
    };
    let old = md(old);
    if !l.loaded && d.hot_version <= old.hot_version {
        let mut m: MBuf<u8> = MBuf::new();
        m.extend(b"its version ");
        push_num(&mut m, d.hot_version);
        m.extend(b" is not newer than the running one's (");
        push_num(&mut m, old.hot_version);
        m.extend(b"); restart with `r`");
        return refused(m.as_slice());
    }
    let ch = Changes::of(old, d);
    if let Some(r) = &ch.refuse {
        return refused(r.as_slice());
    }
    if migrate::in_region() {
        return Step::Wait(line(&[
            b"a no-collect region is open; the change applies when it ends",
        ]));
    }
    if ch.affected.is_empty() {
        if !l.loaded {
            if let Err(e) = load(h, l) {
                return refused(e.as_slice());
            }
        }
        let vs = with_new(h, l.desc);
        world::lock();
        world::stop();
        rebind(vs.as_slice());
        world::resume();
        world::unlock();
        return applied(h, l, &ch, 0);
    }
    let new = l.desc;
    // Running frames are found by the FrameInfo address they root: a
    // build that does not root it would hide them.
    let marked = h
        .versions
        .as_slice()
        .iter()
        .filter(|v| !ptr::eq(v.desc, new))
        .all(|v| hot_of(md(v.desc)).is_some_and(|hd| hd.flags & hot_flags::FRAMES_ROOTED != 0));
    if !marked {
        return refused(
            b"the running code was built without frame marks, so a layout change cannot be checked against it; restart with `r`",
        );
    }
    let frames = Frames::of(
        h.versions
            .as_slice()
            .iter()
            .map(|v| v.desc)
            .filter(|&v| !ptr::eq(v, new)),
    );
    world::lock();
    world::stop();
    let w = frames.running(&|k| ch.has(k));
    world::resume();
    world::unlock();
    if let Some(w) = w {
        return Step::Wait(w);
    }
    let mut defaults = Defaults::new();
    if !l.loaded {
        let mut plan = Mig::new(h, d, &ch, &frames, &mut defaults, false);
        if let Some(r) = plan.refuse.take() {
            return refused(r.as_slice());
        }
        if migrate::run(&mut plan) == Outcome::Blocked {
            return Step::Wait(plan.why());
        }
        unregister_types(h);
        if let Err(e) = load(h, l) {
            register_types(h);
            return refused(e.as_slice());
        }
        own_slots(d);
    }
    for _ in 0..4 {
        defaults.prepare();
        let mut plan = Mig::new(h, d, &ch, &frames, &mut defaults, true);
        if let Some(r) = plan.refuse.take() {
            drop_defaults();
            return refused(r.as_slice());
        }
        let r = migrate::run(&mut plan);
        let short = plan.short;
        let why = plan.why();
        drop(plan);
        drop_defaults();
        match r {
            Outcome::Done(objects, _) => return applied(h, l, &ch, objects),
            Outcome::Blocked if short => continue,
            Outcome::Blocked => return Step::Wait(why),
        }
    }
    Step::Wait(line(&[
        b"objects of the changed types are made faster than they can be migrated; call `reload::point()` where less runs",
    ]))
}

/// The versions with `new` added as applied.
fn with_new(h: &Hot, new: *const ModuleDesc) -> MBuf<Version> {
    let mut v: MBuf<Version> = MBuf::new();
    for x in h.versions.as_slice() {
        v.push(Version {
            desc: x.desc,
            applied: x.applied || x.desc == new,
        });
    }
    v
}

/// Load `l` through the plugin loader and list it (not applied).
fn load(h: &mut Hot, l: &mut Load) -> Result<(), MBuf<u8>> {
    let mut e = PluginErr {
        kind: 0,
        reserved: 0,
        a: Str::EMPTY,
        b: Str::EMPTY,
        list: Str::EMPTY,
    };
    // SAFETY: a path and a writable record.
    let r = unsafe { crate::plugin::raw_load(l.path.ptr, l.path.len, &raw mut e) };
    if r == 0 {
        return Err(load_error(&e));
    }
    l.loaded = true;
    h.versions.push(Version {
        desc: l.desc,
        applied: false,
    });
    Ok(())
}

/// What a loader failure means here.
fn load_error(e: &PluginErr) -> MBuf<u8> {
    let (a, b, list) = (text(&e.a), text(&e.b), text(&e.list));
    let mut m: MBuf<u8> = MBuf::new();
    let restart: &[u8] = b"; restart with `r`";
    let names = |m: &mut MBuf<u8>| {
        for (k, n) in list.split(|&c| c == b'\n').enumerate() {
            if k > 0 {
                m.extend(b", ");
            }
            m.extend(n);
        }
    };
    match e.kind {
        3 => {
            m.extend(b"it was built by ");
            m.extend(a);
            m.extend(b", the program by ");
            m.extend(b);
        }
        4 => {
            m.extend(b"it was built for ");
            m.extend(a);
            m.extend(b", the program for ");
            m.extend(b);
        }
        5 => {
            m.extend(b"it needs packages the program lacks: ");
            names(&mut m);
        }
        6 => {
            m.extend(b"it calls what the program lacks: ");
            names(&mut m);
        }
        7 => {
            m.extend(b"`");
            m.extend(a);
            m.extend(b"` is laid out otherwise in the program");
        }
        _ => {
            m.extend(b"the system could not load it: ");
            m.extend(a);
        }
    }
    m.extend(restart);
    m
}

/// Before the loader takes a version with changed layouts: drop every
/// hot version's descriptors from the type registry (their tags stay),
/// so the new ones register without a layout clash.
fn unregister_types(h: &Hot) {
    for v in h.versions.as_slice() {
        let d = md(v.desc);
        // SAFETY: the module's own tables.
        unsafe {
            crate::types::nabi_types_register(
                ptr::null(),
                0,
                d.tags.keys,
                d.tags.tags,
                d.tags.n as usize,
            );
            if !d.types.ptr.is_null() {
                crate::types::nabi_types_unregister(d.types.ptr, d.types.len);
            }
        }
    }
}

/// Undo [`unregister_types`] after a failed load.
fn register_types(h: &Hot) {
    for v in h.versions.as_slice() {
        let d = md(v.desc);
        if d.types.ptr.is_null() {
            continue;
        }
        // SAFETY: as above; the tags are set, so they are kept.
        unsafe {
            crate::types::nabi_types_register(
                d.types.ptr,
                d.types.len,
                d.tags.keys,
                d.tags.tags,
                d.tags.n as usize,
            );
        }
    }
}

/// Bind the new version's own hot slots to itself, so its default
/// initializers run its own code before it applies.
fn own_slots(d: &ModuleDesc) {
    for imp in items(&d.imports) {
        if imp.kind != sym::HOT_FN {
            continue;
        }
        if let Some(e) = export(d, text(&imp.sym)) {
            // SAFETY: the module's slot.
            unsafe { imp.slot.write(e.addr) };
        }
    }
}

/// With the world stopped: bind every `HOT_FN` slot of every version to
/// the newest applied export with its name and signature.
fn rebind(vs: &[Version]) {
    let mut order: MBuf<(u64, usize)> = MBuf::new();
    for (i, v) in vs.iter().enumerate() {
        if v.applied {
            order.push((md(v.desc).hot_version, i));
        }
    }
    order.as_mut_slice().sort_unstable_by(|a, b| b.cmp(a));
    for v in vs {
        let d = md(v.desc);
        for imp in items(&d.imports) {
            if imp.kind != sym::HOT_FN {
                continue;
            }
            let name = text(&imp.sym);
            let want = sig_of(d, name);
            for &(_, i) in order.as_slice() {
                let Some(e) = export(md(vs[i].desc), name) else {
                    continue;
                };
                if e.kind != sym::FN || (want != 0 && e.sig != 0 && want != e.sig) {
                    continue;
                }
                // SAFETY: the module's slot.
                unsafe { imp.slot.write(e.addr) };
                break;
            }
        }
    }
}

/// Mark `l` applied; the reply and the event.
fn applied(h: &mut Hot, l: &Load, ch: &Changes, objects: usize) -> Step {
    for v in h.versions.as_mut_slice() {
        if v.desc == l.desc {
            v.applied = true;
        }
    }
    let d = md(l.desc);
    let mut ev: MBuf<u8> = MBuf::new();
    event(&mut ev, d, l.n, Some(ch));
    h.events.extend(ev.as_slice());
    let mut m: MBuf<u8> = MBuf::new();
    m.extend(b"applied ");
    m.extend(text(&d.name));
    m.extend(b" v");
    push_num(&mut m, l.n);
    m.extend(b": ");
    if ch.types.is_empty() {
        m.extend(b"code");
    } else {
        push_num(&mut m, ch.types.len() as u64);
        m.extend(if ch.types.len() == 1 {
            b" type ("
        } else {
            b" types ("
        });
        for (k, &(o, n)) in ch.types.as_slice().iter().enumerate() {
            if k > 0 {
                m.extend(b"; ");
            }
            // SAFETY: descriptors of the modules.
            diff_text(&mut m, unsafe { &*o }, unsafe { &*n });
        }
        m.extend(b"), ");
        push_num(&mut m, objects as u64);
        m.extend(if objects == 1 {
            b" object migrated"
        } else {
            b" objects migrated"
        });
    }
    Step::Done(m)
}

/// `Enemy: +mana -hp hp->health +Paused`.
fn diff_text(m: &mut MBuf<u8>, o: &TypeDesc, n: &TypeDesc) {
    m.extend(text(&n.name));
    m.push(b':');
    let start = m.len();
    for f in items(&n.fields) {
        let old = old_field(o, f);
        match old {
            None => {
                m.extend(b" +");
                m.extend(text(&f.name));
            }
            Some(g) if text(&g.name) != text(&f.name) => {
                m.push(b' ');
                m.extend(text(&g.name));
                m.extend(b"->");
                m.extend(text(&f.name));
            }
            Some(_) => {}
        }
    }
    for g in items(&o.fields) {
        if !items(&n.fields)
            .iter()
            .any(|f| old_field(o, f).is_some_and(|x| ptr::eq(x, g)))
        {
            m.extend(b" -");
            m.extend(text(&g.name));
        }
    }
    for v in items(&n.variants) {
        if !items(&o.variants)
            .iter()
            .any(|w| text(&w.name) == text(&v.name))
        {
            m.extend(b" +");
            m.extend(text(&v.name));
        }
    }
    for w in items(&o.variants) {
        if !items(&n.variants)
            .iter()
            .any(|v| text(&v.name) == text(&w.name))
        {
            m.extend(b" -");
            m.extend(text(&w.name));
        }
    }
    if m.len() == start {
        m.extend(b" laid out anew");
    }
}

/// The field of `o` that new field `f` keeps: by name, else the one it
/// is `#[renamed_from]`.
fn old_field<'a>(o: &'a TypeDesc, f: &FieldDesc) -> Option<&'a FieldDesc> {
    let fs = items(&o.fields);
    fs.iter()
        .find(|g| text(&g.name) == text(&f.name))
        .or_else(|| {
            let r = text(&f.renamed_from);
            (!r.is_empty())
                .then(|| fs.iter().find(|g| text(&g.name) == r))
                .flatten()
        })
}

/// What a new version changes against the live one.
struct Changes {
    /// Type keys whose values must be rewritten (laid out anew, or
    /// holding such a type by value), sorted.
    affected: MBuf<u64>,
    /// `(live, new)` descriptors of the types whose own layout changed.
    types: MBuf<(*const TypeDesc, *const TypeDesc)>,
    refuse: Option<MBuf<u8>>,
}

impl Changes {
    fn of(old: &ModuleDesc, new: &ModuleDesc) -> Self {
        let mut ch = Self {
            affected: MBuf::new(),
            types: MBuf::new(),
            refuse: None,
        };
        for &t in items(&new.types) {
            // SAFETY: the module's descriptors.
            let n = unsafe { &*t };
            let Some(o) = desc_of(old, n.key_hash) else {
                continue;
            };
            if o.layout_hash == n.layout_hash {
                continue;
            }
            ch.affected.push(n.key_hash);
            ch.types.push((o, n));
            if ch.refuse.is_none() {
                ch.refuse = refusal(o, n);
            }
        }
        // Types holding a changed type by value change too.
        loop {
            let mut more = false;
            for &t in items(&new.types) {
                // SAFETY: as above.
                let n = unsafe { &*t };
                if ch.has(n.key_hash) || desc_of(old, n.key_hash).is_none() {
                    continue;
                }
                if by_value(n).any(|r| ch.has(r.key_hash)) {
                    ch.affected.push(n.key_hash);
                    ch.affected.as_mut_slice().sort_unstable();
                    more = true;
                }
            }
            if !more {
                break;
            }
        }
        ch.affected.as_mut_slice().sort_unstable();
        ch
    }

    fn has(&self, key: u64) -> bool {
        self.affected.as_slice().binary_search(&key).is_ok()
    }
}

/// The types `d` holds by value: fields, variant fields, a tuple's or
/// array's parts.
fn by_value(d: &TypeDesc) -> impl Iterator<Item = &'static TypeRef> {
    let fields = items(&d.fields).iter().map(|f| &f.ty);
    let variants = items(&d.variants)
        .iter()
        .flat_map(|v| items(&v.fields).iter().map(|f| &f.ty));
    let parts = matches!(d.kind, kind::TUPLE | kind::ARRAY | kind::SUM)
        .then(|| items(&d.generics).iter())
        .into_iter()
        .flatten();
    fields.chain(variants).chain(parts)
}

/// Why a change from `o` to `n` cannot apply while the program runs.
fn refusal(o: &TypeDesc, n: &TypeDesc) -> Option<MBuf<u8>> {
    let shape = |k: u16| match k {
        kind::STRUCT | kind::TUPLE_STRUCT | kind::UNIT => 1,
        kind::ENUM => 2,
        k => u32::from(k) + 10,
    };
    let mut m: MBuf<u8> = MBuf::new();
    if shape(o.kind) != shape(n.kind) {
        m.extend(b"`");
        m.extend(text(&n.path));
        m.extend(
            b"` changed between a struct and an enum while the program runs; restart with `r`",
        );
        return Some(m);
    }
    for f in items(&n.fields) {
        if let Some(g) = old_field(o, f) {
            if g.ty.key_hash != f.ty.key_hash {
                m.extend(b"field `");
                m.extend(text(&g.name));
                m.extend(b"` of `");
                m.extend(text(&n.name));
                m.extend(b"` changed from `");
                m.extend(text(&g.ty.name));
                m.extend(b"` to `");
                m.extend(text(&f.ty.name));
                m.extend(b"` while the program runs; give the new field its own name with a default to start it fresh, or restart with `r`");
                return Some(m);
            }
        }
    }
    if n.kind == kind::ENUM {
        for v in items(&n.variants) {
            for f in items(&v.fields) {
                if f.offset == NO_OFFSET {
                    m.extend(b"`");
                    m.extend(text(&n.path));
                    m.extend(b"` is laid out by a niche, so it cannot change while the program runs; restart with `r`");
                    return Some(m);
                }
            }
        }
    }
    None
}

/// The defaults of added fields, made before the pause that uses them:
/// per field, `cap` values in a GC buffer of ambiguous words (rooted
/// while it lives), handed out in order.
struct Defaults {
    /// `(field, value bytes, slot bytes, need, cap, at, used)`.
    slots: MBuf<(*const FieldDesc, usize, usize, usize, usize, usize, usize)>,
    buf: usize,
}

impl Defaults {
    const fn new() -> Self {
        Self {
            slots: MBuf::new(),
            buf: 0,
        }
    }

    /// Start counting again.
    fn reset(&mut self) {
        for s in self.slots.as_mut_slice() {
            s.3 = 0;
            s.6 = 0;
        }
    }

    /// One more value of field `f` (of `size` bytes) is needed.
    fn need(&mut self, f: &FieldDesc, size: usize) {
        let fp = ptr::from_ref(f);
        if let Some(s) = self.slots.as_mut_slice().iter_mut().find(|s| s.0 == fp) {
            s.3 += 1;
            return;
        }
        self.slots
            .push((fp, size, size.next_multiple_of(8).max(8), 1, 0, 0, 0));
    }

    /// Every count fits what was made.
    fn enough(&self) -> bool {
        self.slots.as_slice().iter().all(|s| s.3 <= s.4)
    }

    /// Make the values counted (and some more), calling each field's
    /// initializer.
    fn prepare(&mut self) {
        let mut total = 0;
        for s in self.slots.as_mut_slice() {
            s.4 = s.3 + s.3 / 4 + 4;
            s.5 = total;
            s.6 = 0;
            total += s.4 * s.2;
        }
        if total == 0 {
            return;
        }
        if !DEFAULTS_ROOTED.swap(true, Ordering::AcqRel) {
            world::add_root(DEFAULTS.as_ptr().cast_const());
        }
        let obj = nabir_gc::alloc(HEADER_SIZE + total, 8, nabir_abi::gc::SCAN_WORDS_TYPE_ID);
        DEFAULTS.store(obj as usize, Ordering::Release);
        self.buf = obj as usize + HEADER_SIZE;
        for s in self.slots.as_slice() {
            // SAFETY: a field of a hot descriptor.
            let f = unsafe { &*s.0 };
            let init = f.init_default;
            if init.code == 0 {
                continue;
            }
            // SAFETY: `fn(env, owner)`, the compiler's initializer.
            let call: extern "C" fn(usize, usize) = unsafe { core::mem::transmute(init.code) };
            for k in 0..s.4 {
                let at = self.buf + s.5 + k * s.2;
                call(init.env, at.wrapping_sub(f.offset as usize));
            }
        }
    }

    /// Copy the next value of field `f` to `dst`; false when none is
    /// left.
    fn take(&mut self, f: &FieldDesc, dst: *mut u8) -> bool {
        let fp = ptr::from_ref(f);
        let buf = self.buf;
        let Some(s) = self.slots.as_mut_slice().iter_mut().find(|s| s.0 == fp) else {
            return false;
        };
        if s.6 >= s.4 || buf == 0 {
            return false;
        }
        // SAFETY: a made value of `s.1` bytes; `dst` is the field.
        unsafe { ptr::copy_nonoverlapping((buf + s.5 + s.6 * s.2) as *const u8, dst, s.1) };
        s.6 += 1;
        true
    }
}

fn drop_defaults() {
    DEFAULTS.store(0, Ordering::Release);
}

/// How one type id migrates.
#[derive(Clone, Copy)]
struct Entry {
    rule: Rule,
    old: *const TypeDesc,
    new: *const TypeDesc,
}

/// A static that moves to the new version's storage.
#[derive(Clone, Copy)]
struct Moved {
    sym: Str,
    from: usize,
    to: usize,
    old: *const TypeDesc,
    new: *const TypeDesc,
}

/// The migration of one version, as `nabir_gc::migrate` asks it.
struct Mig<'a> {
    ch: &'a Changes,
    frames: &'a Frames,
    defaults: &'a mut Defaults,
    /// Rewrite (else only the dry pass, before the plugin loads).
    rewrite: bool,
    /// Type ids from `base`: index + 1 into `entries`, or `u32::MAX`
    /// (a holder of a changed type: blocks), or 0.
    base: u32,
    map: MBuf<u32>,
    entries: MBuf<Entry>,
    /// The tags of changed types.
    tags: MBuf<u32>,
    /// Every hot version, the new one applied.
    versions: MBuf<Version>,
    new: &'static ModuleDesc,
    statics: MBuf<Moved>,
    why: Option<MBuf<u8>>,
    refuse: Option<MBuf<u8>>,
    /// A dry pass needed more defaults than were made.
    short: bool,
}

impl<'a> Mig<'a> {
    fn new(
        h: &Hot,
        new: &'static ModuleDesc,
        ch: &'a Changes,
        frames: &'a Frames,
        defaults: &'a mut Defaults,
        rewrite: bool,
    ) -> Self {
        let mut m = Self {
            ch,
            frames,
            defaults,
            rewrite,
            base: 0,
            map: MBuf::new(),
            entries: MBuf::new(),
            tags: MBuf::new(),
            versions: with_new(h, new),
            new,
            statics: MBuf::new(),
            why: None,
            refuse: None,
            short: false,
        };
        let olds: MBuf<*const ModuleDesc> = {
            let mut v = MBuf::new();
            for x in h.versions.as_slice() {
                if !ptr::eq(x.desc, new) {
                    v.push(x.desc);
                }
            }
            v
        };
        let (mut lo, mut hi) = (u32::MAX, 0);
        for &o in olds.as_slice() {
            let d = md(o);
            for i in 0..d.gc_types.n as usize {
                // SAFETY: `n` ids, filled by the loader.
                let id = unsafe { *d.gc_types.ids.add(i) };
                lo = lo.min(id);
                hi = hi.max(id + 1);
            }
        }
        if lo < hi {
            m.base = lo;
            for _ in lo..hi {
                m.map.push(0);
            }
        }
        let new_hot = hot_of(new);
        for &o in olds.as_slice() {
            let d = md(o);
            let Some(hd) = hot_of(d) else { continue };
            let keys: &[GcKey] = items(&hd.gc);
            for (i, k) in keys.iter().enumerate().take(d.gc_types.n as usize) {
                // SAFETY: as above.
                let id = unsafe { *d.gc_types.ids.add(i) };
                let slot = (id - m.base) as usize;
                match k.kind {
                    gc_kind::VALUE | gc_kind::ELEMS if ch.has(k.key_hash) => {
                        let (Some(od), Some(nd)) =
                            (desc_of(d, k.key_hash), desc_of(new, k.key_hash))
                        else {
                            m.refuse_missing(d, k.key_hash);
                            continue;
                        };
                        let to = new_hot.and_then(|nh| {
                            items(&nh.gc)
                                .iter()
                                .position(|j| j.kind == k.kind && j.key_hash == k.key_hash)
                        });
                        let Some(j) = to else {
                            m.refuse_missing(d, k.key_hash);
                            continue;
                        };
                        // SAFETY: the new module's ids (0 before it loads).
                        let to = unsafe { *new.gc_types.ids.add(j) };
                        m.entries.push(Entry {
                            rule: Rule {
                                to,
                                old: od.size as usize,
                                new: nd.size as usize,
                                elems: k.kind == gc_kind::ELEMS,
                            },
                            old: od,
                            new: nd,
                        });
                        m.map.as_mut_slice()[slot] = m.entries.len() as u32;
                    }
                    gc_kind::HOLDS | gc_kind::AMBIGUOUS
                        if items(&k.held).iter().any(|&x| ch.has(x)) =>
                    {
                        m.map.as_mut_slice()[slot] = u32::MAX;
                    }
                    _ => {}
                }
            }
            for &t in items(&d.types) {
                // SAFETY: the module's descriptors.
                let td = unsafe { &*t };
                if ch.has(td.key_hash) && td.tag_index != NO_TAG && td.tag_index < d.tags.n {
                    // SAFETY: `n` tags.
                    m.tags
                        .push(unsafe { *d.tags.tags.add(td.tag_index as usize) });
                }
            }
        }
        m.tags.as_mut_slice().sort_unstable();
        m.find_statics(h, new);
        m
    }

    fn refuse_missing(&mut self, d: &ModuleDesc, key: u64) {
        if self.refuse.is_some() {
            return;
        }
        let mut r: MBuf<u8> = MBuf::new();
        r.extend(b"values of `");
        r.extend(desc_of(d, key).map_or(&b"a changed type"[..], |t| text(&t.name)));
        r.extend(b"` are alive but the new version has no place for them; restart with `r`");
        self.refuse = Some(r);
    }

    /// The statics of changed types: from the live storage to the new
    /// version's own.
    fn find_statics(&mut self, h: &Hot, new: &ModuleDesc) {
        let Some(live) = live(h, text(&new.name)) else {
            return;
        };
        let live = md(live);
        for imp in items(&new.imports) {
            if imp.kind != sym::DATA {
                continue;
            }
            let name = text(&imp.sym);
            let Some(e) = export(new, name) else { continue };
            if e.sig == 0 {
                continue;
            }
            if !self.ch.has(e.sig) {
                continue;
            }
            let (Some(od), Some(nd)) = (desc_of(live, e.sig), desc_of(new, e.sig)) else {
                continue;
            };
            let Some(li) = items(&live.imports).iter().find(|i| text(&i.sym) == name) else {
                continue;
            };
            // SAFETY: the live version's bound slot.
            let from = unsafe { *li.slot };
            self.statics.push(Moved {
                sym: e.sym,
                from,
                to: e.addr,
                old: od,
                new: nd,
            });
        }
    }

    fn entry(&self, type_id: u32) -> Option<&Entry> {
        let i = *self
            .map
            .as_slice()
            .get(type_id.checked_sub(self.base)? as usize)?;
        if i == 0 || i == u32::MAX {
            return None;
        }
        self.entries.as_slice().get(i as usize - 1)
    }

    fn set_why(&mut self, m: MBuf<u8>) {
        if self.why.is_none() {
            self.why = Some(m);
        }
    }

    fn why(&mut self) -> MBuf<u8> {
        self.why
            .take()
            .unwrap_or_else(|| line(&[b"a value of a changed type cannot be moved now"]))
    }

    /// The name of type id `t`'s values.
    fn name_of(&self, t: u32) -> &'static [u8] {
        self.entry(t)
            // SAFETY: a descriptor of a module.
            .map_or(&b"value"[..], |e| text(unsafe { &(*e.new).name }))
    }

    /// The statics, checked or rewritten like objects.
    fn each_static(&mut self, commit: bool) {
        for k in 0..self.statics.len() {
            let s = self.statics.as_slice()[k];
            // SAFETY: descriptors of the modules; the static's storage.
            let r = unsafe {
                let mut cx = Cx {
                    commit,
                    defaults: &mut *self.defaults,
                    strict: self.rewrite,
                };
                xform(
                    &*s.old,
                    &*s.new,
                    s.from as *const u8,
                    s.to as *mut u8,
                    &mut cx,
                )
            };
            if let Err(w) = r {
                self.fail(w);
            }
        }
    }

    fn fail(&mut self, w: Why) {
        match w {
            Why::Short => self.short = true,
            Why::Text(m) => self.set_why(m),
        }
    }
}

impl Plan for Mig<'_> {
    fn rule(&self, type_id: u32) -> Option<Rule> {
        self.entry(type_id).map(|e| e.rule)
    }

    fn check(&mut self) -> bool {
        if let Some(w) = self.frames.running(&|k| self.ch.has(k)) {
            self.set_why(w);
            return false;
        }
        true
    }

    fn begin(&mut self, commit: bool) {
        if commit {
            for s in self.defaults.slots.as_mut_slice() {
                s.6 = 0;
            }
        } else {
            self.defaults.reset();
            self.each_static(false);
        }
    }

    fn rewrite(&self) -> bool {
        self.rewrite && self.why.is_none() && !self.short
    }

    fn object(&mut self, type_id: u32) -> bool {
        let Some(i) = type_id.checked_sub(self.base) else {
            return true;
        };
        if self.map.as_slice().get(i as usize) != Some(&u32::MAX) {
            return true;
        }
        self.set_why(line(&[
            b"a closure, coroutine or other value of old code holds a changed type; let it finish or drop it, or restart with `r`",
        ]));
        false
    }

    fn admit(&mut self, type_id: u32, payload: *const u8, count: usize) -> bool {
        let Some(&e) = self.entry(type_id) else {
            return true;
        };
        // SAFETY: descriptors of the modules.
        let (od, nd) = unsafe { (&*e.old, &*e.new) };
        let mut cx = Cx {
            commit: false,
            defaults: &mut *self.defaults,
            strict: self.rewrite,
        };
        for k in 0..count {
            // SAFETY: value `k` of the object's payload.
            let p = unsafe { payload.add(k * od.size as usize) };
            if let Err(w) = xform(od, nd, p, ptr::null_mut(), &mut cx) {
                self.fail(w);
                return false;
            }
        }
        true
    }

    fn copy(&mut self, type_id: u32, from: *const u8, to: *mut u8, count: usize) {
        let Some(&e) = self.entry(type_id) else {
            return;
        };
        // SAFETY: descriptors of the modules.
        let (od, nd) = unsafe { (&*e.old, &*e.new) };
        let mut cx = Cx {
            commit: true,
            defaults: &mut *self.defaults,
            strict: true,
        };
        for k in 0..count {
            // SAFETY: values `k` of the old and new payloads.
            let (s, d) = unsafe { (from.add(k * od.size as usize), to.add(k * nd.size as usize)) };
            let _ = xform(od, nd, s, d, &mut cx);
        }
    }

    fn inline_tag(&self, tag: u32) -> bool {
        self.tags.as_slice().binary_search(&tag).is_ok()
    }

    fn code_range(&self) -> (usize, usize) {
        self.frames.range()
    }

    fn code(&mut self, word: usize) -> bool {
        match self.frames.value(word, &|k| self.ch.has(k)) {
            Some(w) => {
                self.set_why(w);
                false
            }
            None => true,
        }
    }

    fn statics(&mut self, f: &mut dyn FnMut(*mut usize)) {
        for v in self.versions.as_slice() {
            for r in items(&md(v.desc).roots) {
                let w = r.word.cast_mut();
                // SAFETY: an `any` root's tag is the word before.
                if r.any != 0 && unsafe { *w.sub(1).cast::<u32>() } & ANY_BOXED == 0 {
                    continue;
                }
                f(w);
            }
        }
    }

    fn blocked(&mut self, why: Block) {
        let mut m: MBuf<u8> = MBuf::new();
        let tick = |m: &mut MBuf<u8>, n: &[u8]| {
            m.push(b'`');
            m.extend(n);
            m.push(b'`');
        };
        match why {
            Block::Pinned(t) => {
                m.extend(b"a pinned ");
                tick(&mut m, self.name_of(t));
                m.extend(b" (or one whose address was taken) grows and cannot move; release the pin, or restart with `r`");
            }
            Block::Rooted(t) => {
                m.extend(b"a running frame holds a ");
                tick(&mut m, self.name_of(t));
                m.extend(b", which grows; keep it in a `reload::State`, or let the frame return");
            }
            Block::Ambiguous(t) => {
                m.extend(b"a runtime buffer points into a ");
                tick(&mut m, self.name_of(t));
                m.extend(b", which moves; let the work holding it finish, or restart with `r`");
            }
            Block::Interior(t) => {
                m.extend(b"a reference points into the middle of a ");
                tick(&mut m, self.name_of(t));
                m.extend(b"; let the work holding it finish, or restart with `r`");
            }
            Block::InlineAny(_) => {
                m.extend(b"an `any` holds a value of a changed type; drop it, or restart with `r`");
            }
            Block::Region => {
                m.extend(b"a no-collect region is open; the change applies when it ends");
            }
        }
        self.set_why(m);
    }

    fn commit(&mut self, _objects: usize, _bytes: usize) {
        self.each_static(true);
        let moved: MBuf<Moved> = {
            let mut v = MBuf::new();
            v.extend(self.statics.as_slice());
            v
        };
        rebind(self.versions.as_slice());
        // Every version's code reads the static from its new storage.
        for s in moved.as_slice() {
            let name = text(&s.sym);
            for v in self.versions.as_slice() {
                let d = md(v.desc);
                for imp in items(&d.imports) {
                    let n = text(&imp.sym);
                    if imp.kind != sym::DATA || !n.starts_with(name) {
                        continue;
                    }
                    let rest = &n[name.len()..];
                    let to = if rest.is_empty() {
                        s.to
                    } else if rest == b".state" || rest == b".init" {
                        // The lazy-init words come along.
                        let Some(e) = export(self.new, n) else {
                            continue;
                        };
                        e.addr
                    } else {
                        continue;
                    };
                    // SAFETY: the module's slot; the lazy word is one word.
                    unsafe {
                        if !rest.is_empty() && *imp.slot != to {
                            *(to as *mut usize) = *(*imp.slot as *const usize);
                        }
                        imp.slot.write(to);
                    }
                }
            }
        }
    }
}

/// Why a value cannot be rewritten.
enum Why {
    /// No default value left (made too few).
    Short,
    Text(MBuf<u8>),
}

/// A rewrite's state: whether it writes, and the defaults it takes.
struct Cx<'a> {
    commit: bool,
    defaults: &'a mut Defaults,
    /// Running out of defaults blocks (else it is only counted).
    strict: bool,
}

/// The fields of `d` with their offsets: its own, or (a tuple without
/// them) laid out from its parts in order.
fn fields_of(
    d: &TypeDesc,
) -> MBuf<(
    &'static [u8],
    &'static TypeRef,
    usize,
    Option<&'static FieldDesc>,
)> {
    const NAMES: [&[u8]; 8] = [b"0", b"1", b"2", b"3", b"4", b"5", b"6", b"7"];
    let mut v: MBuf<(
        &'static [u8],
        &'static TypeRef,
        usize,
        Option<&'static FieldDesc>,
    )> = MBuf::new();
    let fs = items(&d.fields);
    if !fs.is_empty() || d.kind != kind::TUPLE {
        for f in fs {
            v.push((text(&f.name), &f.ty, f.offset as usize, Some(f)));
        }
        return v;
    }
    let mut at = 0usize;
    for (i, g) in items(&d.generics).iter().enumerate() {
        // SAFETY: a descriptor or null.
        let Some(gd) = (unsafe { g.desc.as_ref() }) else {
            break;
        };
        at = at.next_multiple_of((gd.align as usize).max(1));
        v.push((NAMES.get(i).copied().unwrap_or(b"?"), g, at, None));
        at += gd.size as usize;
    }
    v
}

/// Rewrite the value at `src` (descriptor `o`) into `dst` (descriptor
/// `n`, zeroed); a dry run (`!cx.commit`) only checks and counts.
fn xform(
    o: &TypeDesc,
    n: &TypeDesc,
    src: *const u8,
    dst: *mut u8,
    cx: &mut Cx<'_>,
) -> Result<(), Why> {
    let same = o.layout_hash == n.layout_hash && o.size == n.size;
    if !composite(n) || same && !holds_composite(n) {
        if !same {
            let mut m: MBuf<u8> = MBuf::new();
            m.extend(b"`");
            m.extend(text(&n.name));
            m.extend(b"` changed in a way the running program cannot follow; restart with `r`");
            return Err(Why::Text(m));
        }
        if cx.commit {
            // SAFETY: both hold `size` bytes of this type.
            unsafe { ptr::copy_nonoverlapping(src, dst, n.size as usize) };
        }
        return Ok(());
    }
    match n.kind {
        kind::ENUM | kind::SUM => xform_enum(o, n, src, dst, cx),
        kind::ARRAY => {
            let (Some(oe), Some(ne)) = (elem(o), elem(n)) else {
                return Err(no_desc(n));
            };
            let count = if oe.size == 0 { 0 } else { o.size / oe.size };
            for k in 0..count as usize {
                // SAFETY: element `k` of both arrays.
                let (s, d) = unsafe {
                    (
                        src.add(k * oe.size as usize),
                        dst.wrapping_add(k * ne.size as usize),
                    )
                };
                xform(oe, ne, s, d, cx)?;
            }
            Ok(())
        }
        _ => {
            let of = fields_of(o);
            for (name, ty, at, fd) in fields_of(n).as_slice().iter().copied() {
                let renamed = fd.map_or(&b""[..], |f| text(&f.renamed_from));
                let old = of.as_slice().iter().find(|x| x.0 == name).or_else(|| {
                    (!renamed.is_empty())
                        .then(|| of.as_slice().iter().find(|x| x.0 == renamed))
                        .flatten()
                });
                let d = dst.wrapping_add(at);
                if let Some(&(_, oty, oat, _)) = old {
                    // SAFETY: descriptors or null.
                    let (Some(od), Some(nd)) =
                        (unsafe { oty.desc.as_ref() }, unsafe { ty.desc.as_ref() })
                    else {
                        return Err(no_desc(n));
                    };
                    // SAFETY: the field of the old value.
                    xform(od, nd, unsafe { src.add(oat) }, d, cx)?;
                } else if let Some(f) = fd {
                    added(f, d, cx)?;
                }
            }
            Ok(())
        }
    }
}

/// A type rewritten part by part.
const fn composite(d: &TypeDesc) -> bool {
    matches!(
        d.kind,
        kind::STRUCT | kind::TUPLE_STRUCT | kind::TUPLE | kind::ENUM | kind::SUM | kind::ARRAY
    )
}

/// Whether `d` holds a composite type by value: one whose bytes may
/// move though `d`'s own layout hash (sizes, offsets, keys) is the same.
fn holds_composite(d: &TypeDesc) -> bool {
    // SAFETY: a descriptor or null.
    by_value(d).any(|r| unsafe { r.desc.as_ref() }.is_some_and(composite))
}

fn elem(d: &TypeDesc) -> Option<&'static TypeDesc> {
    // SAFETY: a descriptor or null.
    items(&d.generics)
        .first()
        .and_then(|g| unsafe { g.desc.as_ref() })
}

fn no_desc(n: &TypeDesc) -> Why {
    let mut m: MBuf<u8> = MBuf::new();
    m.extend(b"the compiler gave no layout of a part of `");
    m.extend(text(&n.name));
    m.extend(b"`; restart with `r`");
    Why::Text(m)
}

/// An enum or sum: the variant by name, its fields like a struct's.
fn xform_enum(
    o: &TypeDesc,
    n: &TypeDesc,
    src: *const u8,
    dst: *mut u8,
    cx: &mut Cx<'_>,
) -> Result<(), Why> {
    let payload = |d: &TypeDesc| items(&d.variants).iter().any(|v| v.fields.len != 0);
    let read = |d: &TypeDesc, p: *const u8| -> u32 {
        // SAFETY: the value's tag (`u32` at 0, else the value itself).
        unsafe {
            if payload(d) || d.size >= 4 {
                p.cast::<u32>().read_unaligned()
            } else if d.size == 2 {
                u32::from(p.cast::<u16>().read_unaligned())
            } else {
                u32::from(*p)
            }
        }
    };
    let tag = read(o, src);
    let Some(ov) = items(&o.variants).iter().find(|v| v.tag == tag) else {
        return Err(Why::Text(line(&[
            b"a value of `",
            text(&o.name),
            b"` holds a variant its layout does not list; restart with `r`",
        ])));
    };
    let Some(nv) = items(&n.variants)
        .iter()
        .find(|v| text(&v.name) == text(&ov.name))
    else {
        return Err(Why::Text(line(&[
            b"a live `",
            text(&o.name),
            b"::",
            text(&ov.name),
            b"` exists, and the new version removed that variant; drop it, or restart with `r`",
        ])));
    };
    if cx.commit {
        // SAFETY: the new value's tag, as `read` reads it.
        unsafe {
            if payload(n) || n.size >= 4 {
                dst.cast::<u32>().write_unaligned(nv.tag);
            } else if n.size == 2 {
                dst.cast::<u16>().write_unaligned(nv.tag as u16);
            } else {
                *dst = nv.tag as u8;
            }
        }
    }
    let ofs = items(&ov.fields);
    for f in items(&nv.fields) {
        let old = ofs.iter().find(|g| text(&g.name) == text(&f.name));
        let d = dst.wrapping_add(f.offset as usize);
        match old {
            Some(g) => {
                // SAFETY: descriptors or null.
                let (Some(od), Some(nd)) =
                    (unsafe { g.ty.desc.as_ref() }, unsafe { f.ty.desc.as_ref() })
                else {
                    return Err(no_desc(n));
                };
                // SAFETY: the field of the old value.
                xform(od, nd, unsafe { src.add(g.offset as usize) }, d, cx)?;
            }
            None => added(f, d, cx)?,
        }
    }
    Ok(())
}

/// Added field `f` at `d`: its default (counted on a dry run); a field
/// without one stays zero.
fn added(f: &FieldDesc, d: *mut u8, cx: &mut Cx<'_>) -> Result<(), Why> {
    if f.flags & field::HAS_DEFAULT == 0 {
        return Ok(());
    }
    if cx.commit {
        return if cx.defaults.take(f, d) {
            Ok(())
        } else {
            Err(Why::Short)
        };
    }
    // SAFETY: a descriptor or null.
    let size = unsafe { f.ty.desc.as_ref() }.map_or(0, |t| t.size as usize);
    cx.defaults.need(f, size);
    if cx.strict && !cx.defaults.enough() {
        return Err(Why::Short);
    }
    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::missing_const_for_fn,
    clippy::cast_ptr_alignment,
    clippy::too_many_lines,
    clippy::items_after_statements
)]
mod tests {
    use super::*;
    use nabir_abi::gc::{INTERIOR_REF, PLAIN_TYPE_ID, STRONG_REF, TABLE_ARRAY};
    use nabir_abi::module::{
        FRAME_MAGIC, FnRange, FrameInfo, GcTypes, Held, Import, Root, TagKey, Tags,
    };
    use nabir_abi::typedesc::{FnValue, MAGIC as TMAGIC};
    use std::sync::atomic::AtomicUsize as Word;

    const fn s(t: &'static str) -> Str {
        Slice {
            ptr: t.as_ptr(),
            len: t.len(),
        }
    }

    fn slice<T>(v: Vec<T>) -> Slice<T> {
        let v = v.leak();
        Slice {
            ptr: v.as_ptr(),
            len: v.len(),
        }
    }

    fn leak<T>(v: T) -> &'static T {
        Box::leak(Box::new(v))
    }

    fn prim(name: &'static str, size: u64, k: u16) -> &'static TypeDesc {
        ty(name, k, size, 0, vec![])
    }

    fn ty(
        name: &'static str,
        k: u16,
        size: u64,
        layout: u64,
        fields: Vec<FieldDesc>,
    ) -> &'static TypeDesc {
        leak(TypeDesc {
            magic: TMAGIC,
            version: 1,
            kind: k,
            flags: 0,
            tag_index: NO_TAG,
            key_hash: fnv64(name.as_bytes()),
            layout_hash: layout,
            size,
            align: 8,
            reserved0: 0,
            key: s(name),
            name: s(name),
            path: s(name),
            fields: slice(fields),
            variants: Slice::EMPTY,
            generics: Slice::EMPTY,
            attrs: Slice::EMPTY,
            ops: ptr::null(),
            reserved1: 0,
        })
    }

    fn fld(
        name: &'static str,
        t: &'static TypeDesc,
        offset: u64,
        init: Option<extern "C" fn(usize, usize)>,
    ) -> FieldDesc {
        FieldDesc {
            name: s(name),
            renamed_from: Str::EMPTY,
            ty: TypeRef {
                name: t.name,
                key_hash: t.key_hash,
                desc: t,
                kind: t.kind as usize,
            },
            attrs: Slice::EMPTY,
            offset,
            flags: if init.is_some() {
                field::HAS_DEFAULT
            } else {
                0
            },
            index: 0,
            init_default: FnValue {
                code: init.map_or(0, |f| f as usize),
                env: 0,
            },
        }
    }

    /// A version of package "game": its GC tables (registered), keys,
    /// types, hot slots and exports.
    #[allow(clippy::too_many_arguments)]
    fn version(
        n: u64,
        tables: Vec<&'static [u32]>,
        keys: Vec<GcKey>,
        types: Vec<&'static TypeDesc>,
        imports: Vec<Import>,
        mut exports: Vec<Export>,
        sigs: Vec<SymSig>,
        functions: Vec<FnRange>,
    ) -> &'static ModuleDesc {
        let nt = tables.len();
        let tabs: Vec<*const u8> = tables.iter().map(|t| t.as_ptr().cast()).collect();
        let tabs = tabs.leak();
        world::lock();
        // SAFETY: `nt` scan tables, no roots.
        let (_, base) =
            unsafe { nabir_gc::modules::add(tabs.as_ptr(), nt as u32, &[], (0, 0)) }.unwrap();
        world::unlock();
        let ids: Vec<u32> = (0..nt as u32).map(|i| base + i).collect();
        exports.sort_by(|a, b| text(&a.sym).cmp(text(&b.sym)));
        let hot = leak(HotDesc {
            magic: HOT_MAGIC,
            version: 1,
            flags: 0,
            base_version: n - 1,
            gc: slice(keys),
            sigs: slice(sigs),
        });
        let types: Vec<*const TypeDesc> = types.into_iter().map(ptr::from_ref).collect();
        leak(ModuleDesc {
            magic: MAGIC,
            version: 1,
            flags: flags::PLUGIN | flags::HOT,
            hot_version: n,
            text_lo: 0,
            text_hi: 0,
            toolchain: s("nabi test"),
            target: s("test"),
            name: s("game"),
            provides: Slice::EMPTY,
            needs: Slice::EMPTY,
            imports: slice(imports),
            exports: slice(exports),
            gc_types: GcTypes {
                tables: tabs.as_ptr(),
                ids: ids.leak().as_mut_ptr(),
                n: nt as u32,
                reserved: 0,
            },
            tags: Tags {
                keys: Vec::<TagKey>::new().leak().as_ptr(),
                tags: Vec::<u32>::new().leak().as_mut_ptr(),
                n: 0,
                reserved: 0,
            },
            roots: Slice::<Root>::EMPTY,
            types: slice(types),
            dispatch: Slice::EMPTY,
            functions: slice(functions),
            hot,
            reserved: 0,
        })
    }

    fn hot_fn(name: &'static str) -> (Import, &'static Word) {
        let slot: &'static Word = leak(Word::new(0));
        (
            Import {
                sym: s(name),
                sig: 0,
                slot: slot.as_ptr(),
                kind: sym::HOT_FN,
            },
            slot,
        )
    }

    fn func(name: &'static str, addr: usize, sig: u64) -> Export {
        Export {
            sym: s(name),
            path: Str::EMPTY,
            sig,
            addr,
            kind: sym::FN,
            sig_text: Str::EMPTY,
        }
    }

    fn table(words: &[u32]) -> &'static [u32] {
        words.to_vec().leak()
    }

    fn word(o: *mut u8, off: usize) -> usize {
        // SAFETY: a word inside a live test object.
        unsafe { o.add(off).cast::<usize>().read_unaligned() }
    }

    fn set(o: *mut u8, off: usize, v: usize) {
        // SAFETY: as above.
        unsafe { o.add(off).cast::<usize>().write_unaligned(v) };
    }

    extern "C" fn mana_default(_env: usize, owner: usize) {
        // SAFETY: `owner` is a value of the new `Enemy`; `mana` at 16.
        unsafe { ((owner + 16) as *mut usize).write_unaligned(7) };
    }

    /// Run one migration from `old` to `new` (both in `h`), dry then
    /// for real, as `process` does after the plugin loaded.
    fn migrate_to(h: &Hot, old: &ModuleDesc, new: &'static ModuleDesc) -> Result<usize, Vec<u8>> {
        let ch = Changes::of(old, new);
        assert!(ch.refuse.is_none());
        let frames = Frames::of(h.versions.as_slice().iter().map(|v| v.desc));
        let mut defaults = Defaults::new();
        let mut dry = Mig::new(h, new, &ch, &frames, &mut defaults, false);
        if migrate::run(&mut dry) == Outcome::Blocked {
            return Err(dry.why().as_slice().to_vec());
        }
        drop(dry);
        defaults.prepare();
        let mut plan = Mig::new(h, new, &ch, &frames, &mut defaults, true);
        let r = migrate::run(&mut plan);
        let why = plan.why().as_slice().to_vec();
        drop(plan);
        drop_defaults();
        match r {
            Outcome::Done(n, _) => Ok(n),
            Outcome::Blocked => Err(why),
        }
    }

    fn hot_of_versions(vs: &[(&'static ModuleDesc, bool)]) -> Hot {
        let mut h = Hot {
            versions: MBuf::new(),
            seen: MBuf::new(),
            queue: MBuf::new(),
            events: MBuf::new(),
        };
        for &(d, applied) in vs {
            h.versions.push(Version { desc: d, applied });
        }
        h
    }

    #[test]
    fn swaps_slots_and_migrates() {
        let _gc = crate::tests::GC_TEST
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _m = crate::tests::Mutator::attach();

        // Slots: the newest export with the slot's signature.
        let (f1, s1) = hot_fn("_Ya_f");
        let (g1, s2) = hot_fn("_Ya_g");
        let v1 = version(
            1,
            vec![],
            vec![],
            vec![],
            vec![f1, g1],
            vec![func("_Ya_f", 0x100, 5), func("_Ya_g", 0x200, 6)],
            vec![
                SymSig {
                    sym: s("_Ya_f"),
                    sig: 5,
                },
                SymSig {
                    sym: s("_Ya_g"),
                    sig: 6,
                },
            ],
            vec![],
        );
        let (f2, s3) = hot_fn("_Ya_f");
        let (g2, s4) = hot_fn("_Ya_g");
        let v2 = version(
            2,
            vec![],
            vec![],
            vec![],
            vec![f2, g2],
            vec![func("_Ya_f", 0x300, 5), func("_Ya_g", 0x400, 7)],
            vec![
                SymSig {
                    sym: s("_Ya_f"),
                    sig: 5,
                },
                SymSig {
                    sym: s("_Ya_g"),
                    sig: 7,
                },
            ],
            vec![],
        );
        let vs = [
            Version {
                desc: v1,
                applied: true,
            },
            Version {
                desc: v2,
                applied: true,
            },
        ];
        rebind(&vs);
        let get = |w: &Word| w.load(Ordering::Relaxed);
        assert_eq!(
            (get(s1), get(s2)),
            (0x300, 0x200),
            "a changed signature keeps the old body"
        );
        assert_eq!((get(s3), get(s4)), (0x300, 0x400));
        let mut m: MBuf<u8> = MBuf::new();
        // SAFETY: test descriptors.
        diff_text(
            &mut m,
            prim("A", 8, kind::STRUCT),
            prim("A", 8, kind::STRUCT),
        );
        assert_eq!(m.as_slice(), b"A: laid out anew");

        // Types: `Enemy` v1 { hp: i64 @0, tag: ref @8 } (16 bytes);
        // v2 reorders it in place; v3 adds `mana: i64 = 7` (24 bytes).
        let int = prim("i64", 8, kind::PRIM);
        let rf = prim("Tag", 8, kind::OPAQUE);
        let e1 = ty(
            "game::Enemy",
            kind::STRUCT,
            16,
            1,
            vec![fld("hp", int, 0, None), fld("tag", rf, 8, None)],
        );
        let e2 = ty(
            "game::Enemy",
            kind::STRUCT,
            16,
            2,
            vec![fld("tag", rf, 0, None), fld("hp", int, 8, None)],
        );
        let e3 = ty(
            "game::Enemy",
            kind::STRUCT,
            24,
            3,
            vec![
                fld("tag", rf, 0, None),
                fld("hp", int, 8, None),
                fld("mana", int, 16, Some(mana_default)),
            ],
        );
        let key = e1.key_hash;
        let keys = || {
            vec![
                GcKey {
                    key_hash: key,
                    kind: gc_kind::VALUE,
                    reserved: 0,
                    held: Slice::EMPTY,
                },
                GcKey {
                    key_hash: key,
                    kind: gc_kind::ELEMS,
                    reserved: 0,
                    held: Slice::EMPTY,
                },
                GcKey {
                    key_hash: 0,
                    kind: gc_kind::NONE,
                    reserved: 0,
                    held: Slice::EMPTY,
                },
            ]
        };
        let arr = u32::from(TABLE_ARRAY as u16);
        let holder = table(&[
            40,
            1 | (3 << 16),
            16,
            STRONG_REF,
            24,
            STRONG_REF,
            32,
            INTERIOR_REF,
        ]);
        let t1 = vec![
            table(&[32, 1 | (1 << 16), 24, STRONG_REF]),
            table(&[16, arr | 1 | (1 << 16), 8, STRONG_REF]),
            holder,
        ];
        let t2 = vec![
            table(&[32, 1 | (1 << 16), 16, STRONG_REF]),
            table(&[16, arr | 1 | (1 << 16), 0, STRONG_REF]),
            holder,
        ];
        let t3 = vec![
            table(&[40, 1 | (1 << 16), 16, STRONG_REF]),
            table(&[24, arr | 1 | (1 << 16), 0, STRONG_REF]),
            holder,
        ];
        // Held by a frame of `main` while it runs.
        let held = vec![Held {
            key_hash: key,
            line: 12,
            reserved: 0,
            name: s("Enemy"),
        }];
        let fi: &'static FrameInfo = leak(FrameInfo {
            magic: FRAME_MAGIC,
            len: 0x40,
            code: 0x1_0000,
            line: 3,
            name: s("main"),
            file: s("src/main.nabi"),
            held: slice(held),
        });
        let main_fn = FnRange {
            start: 0,
            len: 0,
            frame: ptr::from_ref(fi).cast(),
            reserved: 0,
        };
        let m1 = version(
            1,
            t1,
            keys(),
            vec![e1],
            vec![],
            vec![],
            vec![],
            vec![main_fn],
        );
        let m2 = version(2, t2, keys(), vec![e2], vec![], vec![], vec![], vec![]);
        let m3 = version(3, t3, keys(), vec![e3], vec![], vec![], vec![], vec![]);
        // SAFETY: the ids the test registered.
        let id = |m: &ModuleDesc, i: usize| unsafe { *m.gc_types.ids.add(i) };

        // A holder rooted for the test; a plain object each `Enemy` names.
        static ROOT: Word = Word::new(0);
        static FRAME: Word = Word::new(0);
        static TAG: Word = Word::new(0);
        world::add_root(ROOT.as_ptr());
        world::add_root(FRAME.as_ptr());
        world::add_root(TAG.as_ptr());
        let hold = nabir_gc::alloc(40, 8, id(m1, 2));
        ROOT.store(hold as usize, Ordering::Relaxed);
        // Rooted, so it never moves.
        let tag = nabir_gc::alloc(24, 8, PLAIN_TYPE_ID);
        TAG.store(tag as usize, Ordering::Relaxed);
        let one = nabir_gc::alloc(32, 8, id(m1, 0));
        set(one, 16, 11);
        set(one, 24, tag as usize);
        let two = nabir_gc::alloc(48, 8, id(m1, 1));
        for k in 0..2 {
            set(two, 16 + 16 * k, 20 + k);
            set(two, 24 + 16 * k, tag as usize);
        }
        set(hold, 16, one as usize);
        set(hold, 24, two as usize);
        set(hold, 32, two as usize + 16 + 16);

        // A running frame of old code that uses `Enemy` holds it back.
        FRAME.store(ptr::from_ref(fi) as usize, Ordering::Relaxed);
        let h = hot_of_versions(&[(m1, true), (m2, false)]);
        let why = migrate_to(&h, m1, m2).expect_err("main runs");
        assert_eq!(
            core::str::from_utf8(&why).unwrap(),
            "`main` (src/main.nabi:12) is running and uses `Enemy`, which changed; keep the `Enemy` in a `reload::State` made in another function, or move this work into a function it calls"
        );
        FRAME.store(0, Ordering::Relaxed);

        // v1 -> v2: same sizes, rewritten in place.
        assert_eq!(migrate_to(&h, m1, m2), Ok(2));
        let hold = ROOT.load(Ordering::Relaxed) as *mut u8;
        // (A collection may evacuate them: no address is checked.)
        let (b, a) = (word(hold, 16) as *mut u8, word(hold, 24) as *mut u8);
        // SAFETY: live objects.
        assert_eq!(
            unsafe { (*b.cast::<nabir_common::header::ObjectHeader>()).type_id },
            id(m2, 0)
        );
        assert_eq!((word(b, 16), word(b, 24)), (tag as usize, 11));
        assert_eq!((word(a, 32), word(a, 40)), (tag as usize, 21));
        assert_eq!(word(hold, 32), a as usize + 32);

        // v2 -> v3: grows, so the objects move and `mana` gets its default;
        // the interior reference follows its element.
        // A pinned weak cell (a GC cleanup's) stays put and follows the box.
        let cell = nabir_gc::weak_new(b);
        // SAFETY: a live cell.
        unsafe { nabir_gc::pin(cell) };
        let h = hot_of_versions(&[(m1, true), (m2, true), (m3, false)]);
        assert_eq!(migrate_to(&h, m2, m3), Ok(2));
        let hold = ROOT.load(Ordering::Relaxed) as *mut u8;
        let (b, a) = (word(hold, 16) as *mut u8, word(hold, 24) as *mut u8);
        // SAFETY: a live object.
        assert_eq!(
            unsafe { (*b.cast::<nabir_common::header::ObjectHeader>()).type_id },
            id(m3, 0)
        );
        assert_eq!(
            (word(b, 16), word(b, 24), word(b, 32)),
            (tag as usize, 11, 7)
        );
        for k in 0..2 {
            let at = 16 + 24 * k;
            assert_eq!(
                (word(a, at), word(a, at + 8), word(a, at + 16)),
                (tag as usize, 20 + k, 7)
            );
        }
        assert_eq!(word(hold, 32), a as usize + 16 + 24);
        assert_eq!(word(cell, 16), b as usize, "the weak cell follows");
        // SAFETY: pinned above.
        unsafe { nabir_gc::unpin(cell) };
        nabir_gc::collect();
        assert_eq!(
            word(word(hold, 16) as *mut u8, 24),
            11,
            "survives a collection"
        );

        // A rooted object that must grow cannot move.
        let e4 = ty(
            "game::Enemy",
            kind::STRUCT,
            32,
            4,
            vec![
                fld("tag", rf, 0, None),
                fld("hp", int, 8, None),
                fld("mana", int, 16, None),
                fld("speed", int, 24, None),
            ],
        );
        let t4 = vec![
            table(&[48, 1 | (1 << 16), 16, STRONG_REF]),
            table(&[32, arr | 1 | (1 << 16), 0, STRONG_REF]),
            holder,
        ];
        let m4 = version(4, t4, keys(), vec![e4], vec![], vec![], vec![], vec![]);
        ROOT.store(word(hold, 16), Ordering::Relaxed);
        let h = hot_of_versions(&[(m1, true), (m2, true), (m3, true), (m4, false)]);
        let why = migrate_to(&h, m3, m4).expect_err("rooted");
        assert_eq!(
            core::str::from_utf8(&why).unwrap(),
            "a running frame holds a `game::Enemy`, which grows; keep it in a `reload::State`, or let the frame return"
        );
        ROOT.store(0, Ordering::Relaxed);
        TAG.store(0, Ordering::Relaxed);
        nabir_gc::collect();
    }
}
