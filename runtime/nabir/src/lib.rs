//! `libnabir.a`: the nabi runtime as one static archive, so cross-crate
//! inlining (TLAB bump, safepoint polls, write barriers) survives the
//! codegen boundary.
//!
//! ABI symbols follow the runtime ABI; those at the bottom are
//! conveniences with no ABI stability. The executor is nabi code
//! (`core::task`) over OS threads. The GC (`nabir_gc`) is an
//! Immix-style mark-region with opportunistic evacuation, sticky
//! mark-bit generations and optional concurrent marking.
//! Objects a root reaches never move, so a GC pointer a runtime function
//! got as an argument stays valid across its allocations.

#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]

mod backtrace;
mod bignum;
mod catch;
mod cleanup;
mod coro;
mod deque;
mod errno;
mod fmt;
#[cfg(test)]
mod fuzz;
mod hot;
mod lane;
mod math;
/// No sockets without an OS; its `os` is the few pieces that apply.
#[cfg(not(target_os = "none"))]
mod net;
#[cfg(target_vendor = "apple")]
mod objc;
mod os;
mod park;
#[cfg(not(any(target_family = "wasm", target_os = "none")))]
mod plugin;
/// The `nabi repl` session host, on the hosts that run one: arm64 macOS,
/// Linux glibc and Windows on arm64 and x86_64.
#[cfg(all(
    not(test),
    any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(
            any(target_os = "linux", target_os = "android"),
            target_env = "gnu",
            any(target_arch = "x86_64", target_arch = "aarch64")
        ),
        all(windows, any(target_arch = "x86_64", target_arch = "aarch64")),
    )
))]
mod repl;
#[cfg(not(any(target_family = "wasm", target_os = "none")))]
mod session;
mod stack;
mod stdio;
mod str;
mod thread;
mod types;
#[cfg(any(windows, test))]
mod winargs;
mod xfer;

pub use str::NabiStr;

#[cfg(not(test))]
use core::fmt::Write as _;
use core::ptr;
use core::sync::atomic::{AtomicUsize, Ordering};
use nabir_common::AtomicU64;
use nabir_common::header::{HEADER_ALIGN, HEADER_SIZE};
#[cfg(target_os = "none")]
use nabir_common::sys::libc;
use nabir_common::{fatal, tls};

/// No REPL session on wasm32 or without an OS: no thread is ever found
/// gone.
#[cfg(any(target_family = "wasm", target_os = "none"))]
#[unsafe(no_mangle)]
pub const extern "C" fn nabi_thread_lost(_id: u64) -> i32 {
    0
}

/// No REPL session on wasm32 or without an OS: no gates, no lock notes.
#[cfg(any(target_family = "wasm", target_os = "none"))]
#[unsafe(no_mangle)]
pub const extern "C" fn nabi_session_enter() {}

#[cfg(any(target_family = "wasm", target_os = "none"))]
#[unsafe(no_mangle)]
pub const extern "C" fn nabi_session_leave() {}

#[cfg(any(target_family = "wasm", target_os = "none"))]
#[unsafe(no_mangle)]
pub const extern "C" fn nabi_session_pass() {}

#[cfg(any(target_family = "wasm", target_os = "none"))]
#[unsafe(no_mangle)]
pub const extern "C" fn nabi_lock_note(_buf: *mut u8, _kind: i32) -> i32 {
    0
}

#[cfg(any(target_family = "wasm", target_os = "none"))]
#[unsafe(no_mangle)]
pub const extern "C" fn nabi_lock_drop(_buf: *mut u8) {}

#[cfg(any(target_family = "wasm", target_os = "none"))]
#[unsafe(no_mangle)]
pub const extern "C" fn nabi_lock_notes() -> i32 {
    0
}

// nabic links the runtime's import libraries; the unit-test executable
// gets std's, which lack bcrypt.
#[cfg(all(windows, test))]
#[link(name = "bcrypt")]
unsafe extern "system" {}

/// Rust panic inside the runtime = inconsistent runtime state. Report
/// to stderr and abort. Nabi exceptions are not Rust panics.
///
/// `cargo test` pulls in `std`, which already supplies a panic handler.
#[cfg(not(test))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    struct Stderr;
    impl core::fmt::Write for Stderr {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            // SAFETY: `s` is valid for its own length.
            unsafe { nabir_common::write_stderr(s.as_ptr(), s.len()) };
            Ok(())
        }
    }
    let _ = writeln!(Stderr, "nabi runtime internal error: {info}");
    // SAFETY: abort never returns and has no preconditions.
    unsafe { libc::abort() }
}

// The precompiled `core` / `compiler_builtins` reference
// `rust_eh_personality` even under `panic = "abort"`. It is weak so
// libnabir.a can link next to another Rust staticlib or std.
// `localize.sh` keeps only `nabi_*` (and on ELF the weakened `__*`
// intrinsics) global in the shipped archive.
#[cfg(all(not(test), target_vendor = "apple"))]
core::arch::global_asm!(include_str!("personality/macho.s"));
// MSVC targets use `__CxxFrameHandler3`, not this symbol; powerpc64's
// is in `nabir_common`'s build.rs object (no inline assembly there).
#[cfg(all(
    not(test),
    not(target_vendor = "apple"),
    not(windows),
    not(any(target_arch = "arm", target_arch = "s390x", target_arch = "powerpc64")),
    not(target_family = "wasm")
))]
core::arch::global_asm!(include_str!("personality/elf.s"));
#[cfg(all(not(test), target_arch = "s390x"))]
core::arch::global_asm!(include_str!("personality/s390x/elf.s"));
// ARM EHABI unwind tables also name the personality routines libgcc
// would bring; nothing here unwinds.
#[cfg(all(not(test), target_arch = "arm"))]
core::arch::global_asm!(include_str!("personality/arm/elf.s"));

/// GC allocation.
///
/// `size` includes the 16-byte
/// header. Returns a pointer to the initialized header with a zeroed
/// payload; never null (OOM is fatal). May run a collection first, so
/// every live GC reference of the caller must be rooted.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_gc_alloc_slow(size: usize, align: usize, type_id: u32) -> *mut u8 {
    nabir_gc::world::may_stop(b"a #[no_safepoint] runtime call allocated");
    // Before allocating: nothing of the caller's is unrooted yet.
    if cleanup::ANY.load(core::sync::atomic::Ordering::Relaxed) {
        cleanup::run();
    }
    nabir_gc::alloc(size, align, type_id)
}

/// Write barrier slow path: compiled code
/// calls it before storing a GC reference (`len` bytes at `slot`) into
/// the old object `obj` (its `UNLOGGED` header bit set); `obj` joins the
/// remembered set.
///
/// During a concurrent mark it also logs the references `obj` holds
/// (snapshot at the beginning; a large object's card at `slot` only).
///
/// # Safety
/// `obj` must be a live GC object and `slot..slot + len` inside it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nabi_gc_wb_slow(obj: *mut u8, slot: *const u8, len: usize) {
    // SAFETY: caller contract.
    unsafe { nabir_gc::write_barrier_slow(obj, slot, len) };
}

/// # Safety
/// `scan_table` must be a valid `ScanTable` that lives for the rest
/// of the process.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nabi_gc_register_type(type_id: u32, scan_table: *const u8) {
    nabir_gc::register_type(type_id, scan_table);
}

/// `nabi_gc_register_type` of each of the `n` `ScanTable`s at `tables`,
/// type ids `base`, `base + 1`, ...
///
/// # Safety
/// `tables` must hold `n` pointers as `nabi_gc_register_type` takes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nabi_gc_register_types(base: u32, tables: *const *const u8, n: usize) {
    for i in 0..n {
        // SAFETY: caller contract.
        unsafe { nabir_gc::register_type(base + i as u32, *tables.add(i)) };
    }
}

/// Keep `obj` (and what it references) alive and in place until
/// `nabi_gc_unpin`.
///
/// # Safety
/// `obj` must be a live GC object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nabi_gc_pin(obj: *mut u8) -> *mut u8 {
    // SAFETY: caller contract.
    unsafe { nabir_gc::pin(obj) };
    obj
}

/// # Safety
/// `obj` must be a live GC object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nabi_gc_unpin(obj: *mut u8) {
    // SAFETY: caller contract.
    unsafe { nabir_gc::unpin(obj) };
}

/// Whether `obj` is pinned (`List::reserve` refuses to move a pinned
/// buffer).
///
/// # Safety
/// `obj` must be a live GC object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nabi_gc_pinned(obj: *mut u8) -> i32 {
    // SAFETY: caller contract.
    i32::from(unsafe { nabir_gc::is_pinned(obj) })
}

/// A handle for C holding `obj` (`bare::ffi::Handle::new`), a value of
/// the type named `name` (a static string).
///
/// # Safety
/// `obj` must be a live GC object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nabi_handle_new(obj: *mut u8, name: NabiStr) -> usize {
    // SAFETY: caller contract; type names are static strings.
    unsafe { nabir_gc::handle_new(obj, name.ptr, name.len) }
}

/// The object of handle `h` (`Handle::get`); panics, saying what to
/// fix, when it was released or holds another type than `name`.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_handle_get(h: usize, name: NabiStr) -> *mut u8 {
    match nabir_gc::handle_get(h, name.bytes()) {
        Ok(obj) => obj,
        Err(f) => handle_fault(&f, name.bytes()),
    }
}

/// `Handle::retain`.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_handle_retain(h: usize) {
    if let Err(f) = nabir_gc::handle_retain(h) {
        handle_fault(&f, b"");
    }
}

/// `Handle::release`.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_handle_release(h: usize) {
    if let Err(f) = nabir_gc::handle_release(h) {
        handle_fault(&f, b"");
    }
}

fn handle_fault(f: &nabir_gc::HandleFault, want: &[u8]) -> ! {
    match *f {
        nabir_gc::HandleFault::Released => panic_parts(
            &[b"handle already released (or not a handle): call `release` once per `Handle::new` and `retain`, and not `get` after the last one"],
            None,
        ),
        nabir_gc::HandleFault::Holds(p, n) => {
            // SAFETY: a static string (`nabi_handle_new`).
            let held = unsafe { core::slice::from_raw_parts(p, n) };
            // A closure's own type cannot be written: store it as a `fn`.
            if let Some(sig) = held.strip_prefix(b"closure") {
                panic_parts(
                    &[
                        b"this handle holds a closure, not `",
                        want,
                        b"`: make it with `Handle::<fn",
                        sig,
                        b">::new(..)` and read it with `Handle::<fn",
                        sig,
                        b">::from_ptr(p)`",
                    ],
                    None,
                )
            }
            panic_parts(
                &[
                    b"this handle holds `",
                    held,
                    b"`, not `",
                    want,
                    b"`: write `Handle::<",
                    held,
                    b">::from_ptr(p)`",
                ],
                None,
            )
        }
    }
}

/// A secret buffer of `len` zeroed bytes (`core::crypto::Secret`):
/// never moved or copied by the collector, zeroed before it is freed.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_gc_secret_new(len: usize) -> *mut u8 {
    nabir_gc::alloc_secret(len)
}

/// Zero a secret buffer now, with stores the optimizer keeps.
///
/// # Safety
/// `obj` must come from `nabi_gc_secret_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nabi_gc_secret_wipe(obj: *mut u8) {
    // SAFETY: caller contract.
    unsafe { nabir_gc::wipe(obj) };
}

/// A weak reference cell to `obj` (`bare::gc::Weak`).
#[unsafe(no_mangle)]
pub extern "C" fn nabi_gc_weak_new(obj: *mut u8) -> *mut u8 {
    nabir_gc::weak_new(obj)
}

/// The referent of a weak cell, null once collected.
///
/// # Safety
/// `cell` must come from `nabi_gc_weak_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nabi_gc_weak_get(cell: *mut u8) -> *mut u8 {
    // SAFETY: caller contract.
    unsafe { nabir_gc::weak_get(cell) }
}

/// Register `word` (a static's reference word) as a GC root for the
/// rest of the process. Compiled code calls it at startup.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_gc_add_root(word: *const usize) {
    // A plugin's statics are its descriptor's roots, which unload drops.
    #[cfg(not(any(target_family = "wasm", target_os = "none")))]
    if plugin::in_plugin(word as usize) {
        return;
    }
    nabir_gc::world::add_root(word);
}

#[unsafe(no_mangle)]
pub extern "C" fn nabi_gc_collect() {
    nabir_gc::collect();
    cleanup::run();
}

/// A minor collection (`bare::gc::collect_young`); none while a
/// concurrent mark runs.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_gc_collect_young() {
    nabir_gc::collect_young();
}

/// A minor collection if the last ones say it fits in `ns`
/// (`bare::gc::collect_young_within`); 1 if it ran.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_gc_collect_young_within(ns: u64) -> u32 {
    u32::from(nabir_gc::collect_young_within(ns))
}

/// The pause goal in nanoseconds, 0 for none (`bare::gc::set_pause_goal`).
#[unsafe(no_mangle)]
pub extern "C" fn nabi_gc_set_pause_goal(ns: u64) {
    nabir_gc::set_pause_goal(ns);
}

/// The heap limit in bytes, 0 for none (`bare::gc::set_heap_limit`).
#[unsafe(no_mangle)]
pub extern "C" fn nabi_gc_set_heap_limit(bytes: u64) {
    nabir_gc::set_heap_limit(usize::try_from(bytes).unwrap_or(usize::MAX));
}

/// A generation no `bare::gc::Arena` used before: its `reset` makes
/// every older `ArenaRef`, of any arena, invalid.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_gc_arena_epoch() -> u64 {
    static EPOCH: AtomicU64 = AtomicU64::new(1);
    EPOCH.fetch_add(1, Ordering::Relaxed)
}

/// Into (1) or out of (0) a no-collect region (`bare::gc::no_collect`).
#[unsafe(no_mangle)]
pub extern "C" fn nabi_gc_no_collect(enter: u32) {
    nabir_gc::no_collect(enter != 0);
}

/// Bytes held by GC objects (headers included) as of the last
/// collection, plus everything handed out since in whole TLABs (tests,
/// diagnostics; no ABI stability).
#[unsafe(no_mangle)]
pub extern "C" fn nabi_gc_stats_live_bytes() -> usize {
    nabir_gc::live_bytes()
}

/// Number of completed collections (tests, diagnostics).
#[unsafe(no_mangle)]
pub extern "C" fn nabi_gc_stats_collections() -> u64 {
    nabir_gc::collections()
}

/// Bytes evacuation has copied so far (tests, diagnostics).
#[unsafe(no_mangle)]
pub extern "C" fn nabi_gc_stats_moved_bytes() -> u64 {
    nabir_gc::moved_bytes()
}

/// The collector's counters into the `GcBuf<u64>` `out`
/// (`bare::gc::stats`, [`nabir_gc::stats`] has the order).
///
/// # Safety
/// `out` must be a live `GcBuf<u64>` of at least `STATS_WORDS` elements.
#[unsafe(no_mangle)]
#[allow(clippy::cast_ptr_alignment)] // `GcBuf` elements start 8-aligned
pub unsafe extern "C" fn nabi_gc_stats(out: *mut u8) {
    let mut words = [0; nabir_gc::STATS_WORDS];
    nabir_gc::stats(&mut words);
    // SAFETY: caller contract; elements start after the header.
    unsafe {
        ptr::copy_nonoverlapping(
            words.as_ptr(),
            out.add(HEADER_SIZE).cast::<u64>(),
            words.len(),
        );
    }
}

/// Whether the CPU has the arm64 LSE atomics.
///
/// Code built without them in its baseline tests this byte before each
/// read-modify-write. Zero until known, which takes the exclusive-access
/// loops, right on any CPU.
#[cfg(all(
    target_arch = "aarch64",
    any(target_os = "linux", target_os = "android")
))]
#[allow(non_upper_case_globals)]
#[unsafe(no_mangle)]
pub static nabi_cpu_lse: core::sync::atomic::AtomicU8 = core::sync::atomic::AtomicU8::new(0);

#[unsafe(no_mangle)]
pub extern "C" fn nabi_runtime_init() {
    #[cfg(all(
        target_arch = "aarch64",
        any(target_os = "linux", target_os = "android")
    ))]
    {
        // SAFETY: plain libc call.
        let hw = unsafe { libc::getauxval(libc::AT_HWCAP) };
        // Threads started later see it through their start.
        nabi_cpu_lse.store(u8::from(hw & libc::HWCAP_ATOMICS != 0), Ordering::Relaxed);
    }
    #[cfg(target_os = "wasi")]
    os::start_dir();
    init_thread(true);
}

/// A shared library's `nabi.register_types` less this word's address,
/// written by the linker (`-nabi_anchor`); 1: none.
#[unsafe(no_mangle)]
pub static nabi_library_register: core::sync::atomic::AtomicIsize =
    core::sync::atomic::AtomicIsize::new(1);

/// A shared library's initializer, run by the loader.
///
/// The runtime on the loading thread, the library's GC types
/// registered, then the thread back in C (`PARKED`) until it calls an
/// export. Exports called on other threads attach them
/// (`nabi_native_reenter`).
#[unsafe(no_mangle)]
pub extern "C" fn nabi_library_init() {
    nabi_runtime_init();
    let d = nabi_library_register.load(Ordering::Relaxed);
    if d != 1 {
        let at = (&raw const nabi_library_register as usize).wrapping_add_signed(d);
        // SAFETY: the linker pointed it at the library's
        // `nabi.register_types`, a plain C function.
        let register: extern "C" fn() = unsafe { core::mem::transmute(at) };
        register();
    }
    nabi_enter_native();
}

/// A DLL's entry point: `nabi_library_init` when the process loads it.
#[cfg(windows)]
#[unsafe(no_mangle)]
pub extern "system" fn nabi_dll_main(
    _module: *mut core::ffi::c_void,
    reason: u32,
    _reserved: *mut core::ffi::c_void,
) -> i32 {
    const DLL_PROCESS_ATTACH: u32 = 1;
    if reason == DLL_PROCESS_ATTACH {
        nabi_library_init();
    }
    1
}

/// The main thread is done with nabi code: every other nabi thread (the
/// executor's worker pool, threads still running) stops for good before
/// the process exits.
#[cfg(not(all(nabi_solo, target_env = "p1")))]
#[unsafe(no_mangle)]
pub extern "C" fn nabi_runtime_shutdown() {
    nabir_gc::world::halt();
}

/// The browser's module (a reactor, one thread) stays up after `main`
/// for its exports.
#[cfg(all(nabi_solo, target_env = "p1"))]
#[unsafe(no_mangle)]
pub const extern "C" fn nabi_runtime_shutdown() {}

/// Into an `extern "C"` call: the thread counts as `PARKED`.
///
/// A C call that blocks does not hold up a collection; the
/// caller's GC references are in its shadow-stack frame. Compiled code
/// inlines the fast path (store the state, read the yield flag) and
/// calls this, or [`nabi_leave_native`], only when the flag is up.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_enter_native() {
    nabir_gc::world::enter_native();
}

/// Back from C: waits out a running collection.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_leave_native() {
    nabir_gc::world::leave_native();
}

/// The C entry of a nabi `extern "C" fn`: a call from C code that this
/// thread entered from nabi (`PARKED`) runs nabi code again until
/// [`nabi_native_resume`]. Returns whether it was parked.
///
/// A thread nabi does not know is attached first and counts as parked:
/// back in C it is `PARKED`, so a collection does not wait for it.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_native_reenter() -> u32 {
    // SAFETY: this thread's own slot.
    if unsafe { tls::attached().read() } == 0 {
        thread::adopt();
        catch::foreign(true);
        return 1;
    }
    let parked = nabir_common::tls::mutator_state().load(core::sync::atomic::Ordering::Relaxed)
        == nabir_common::tls::MUTATOR_PARKED;
    if parked {
        nabir_gc::world::leave_native();
        catch::foreign(true);
    }
    u32::from(parked)
}

/// End of [`nabi_native_reenter`]'s bracket: back to C.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_native_resume(parked: u32) {
    if parked != 0 {
        catch::foreign(false);
        nabir_gc::world::enter_native();
    }
}

/// Around a `#[no_safepoint]` call, from a debug nabic: such a call must
/// never stop this thread (`world::may_stop`). No-ops in a release
/// runtime.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_nosp_enter() {
    #[cfg(debug_assertions)]
    nabir_gc::world::no_safepoint(1);
}

#[unsafe(no_mangle)]
pub extern "C" fn nabi_nosp_leave() {
    #[cfg(debug_assertions)]
    nabir_gc::world::no_safepoint(-1);
}

/// Safepoint slow path: park while another thread stops
/// the world.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_runtime_yield() {
    nabir_gc::world::safepoint();
}

/// Initialize this thread's TLS slots and register it as a
/// mutator (its shadow stack becomes a GC root set).
#[unsafe(no_mangle)]
pub extern "C" fn nabi_thread_init() {
    init_thread(false);
}

fn init_thread(main: bool) {
    tls::thread_init();
    nabir_gc::world::attach_current();
    stack::thread_start();
    if main {
        thread::name_main();
    }
}

/// Unregister this thread; it must not touch GC objects afterwards.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_thread_shutdown() {
    thread::thread_exit();
}

/// A coroutine frame: [`nabi_gc_alloc_slow`] at header alignment.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_coro_frame_alloc(size: usize, type_id: u32) -> *mut u8 {
    nabi_gc_alloc_slow(size, HEADER_ALIGN, type_id)
}

/// Park `waker` on coroutine `frame` (a GC object; its address is the
/// parking key) until `nabi_coro_unpark(frame)`.
///
/// # Safety
/// `frame` and `waker` must be live GC objects.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nabi_coro_park(frame: *mut u8, waker: *mut u8) {
    // SAFETY: an installed hook; caller contract.
    unsafe { (hooks().park)(frame, waker) };
}

/// Wake every waker parked on `frame`.
///
/// # Safety
/// `frame` must be a live GC object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nabi_coro_unpark(frame: *mut u8) {
    // SAFETY: as above.
    unsafe { (hooks().unpark)(frame) };
}

/// A GC object's address as a parking key: from now on the collector
/// never moves it, so the key stays its own. Only for keys that outlive
/// the call (identity tests use `buf_same`, which pins nothing).
///
/// # Safety
/// `obj` must be a live GC object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nabi_task_key(obj: *mut u8) -> usize {
    // SAFETY: caller contract.
    unsafe { nabir_gc::no_move(obj) };
    obj as usize
}

/// The process's 128-bit hash key into `out[0..2]`, a `GcBuf<u64>`.
///
/// `RandomState`'s key (hash values change between runs):
/// OS randomness read once, else the clock and an address. Neither word
/// is 0, so a key is never all zero, the reserved key of a state built
/// at compile time.
///
/// # Safety
/// `out` is a live `GcBuf<u64>` of at least two elements.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nabi_hash_keys(out: *mut u8) {
    static K0: AtomicU64 = AtomicU64::new(0);
    static K1: AtomicU64 = AtomicU64::new(0);
    let mut k0 = K0.load(Ordering::SeqCst);
    if k0 == 0 {
        // `nabi_os_random` writes past an object header.
        let mut b = [0u8; HEADER_SIZE + 16];
        let (a, c) = if os::nabi_os_random(b.as_mut_ptr(), 0, 16) == 0 {
            let word = |at: usize| {
                b[HEADER_SIZE + at..HEADER_SIZE + at + 8]
                    .iter()
                    .rev()
                    .fold(0u64, |a, &x| (a << 8) | u64::from(x))
            };
            (word(0), word(8))
        } else {
            let t = nabir_common::sys::monotonic_ns() as u64;
            let p = ptr::addr_of!(K0) as u64;
            (
                (t ^ p).wrapping_mul(0x9E37_79B9_7F4A_7C15),
                (t.rotate_left(32) ^ p).wrapping_mul(0xC2B2_AE3D_27D4_EB4F),
            )
        };
        // Both words are made nonzero (0 is "not set yet"). K1 is set
        // before any K0 is published, so a reader that sees K0 sees K1;
        // racing callers agree on the first of each.
        let (a, c) = (a | 1, c | 1);
        let _ = K1.compare_exchange(0, c, Ordering::SeqCst, Ordering::SeqCst);
        k0 = match K0.compare_exchange(0, a, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => a,
            Err(first) => first,
        };
    }
    let k1 = K1.load(Ordering::SeqCst);
    // SAFETY: caller contract; elements start after the header.
    unsafe {
        let p = out.add(HEADER_SIZE);
        ptr::copy_nonoverlapping(k0.to_ne_bytes().as_ptr(), p, 8);
        ptr::copy_nonoverlapping(k1.to_ne_bytes().as_ptr(), p.add(8), 8);
    }
}

/// Monotonic clock in nanoseconds, stopped during suspend (`Instant`,
/// timers; `CLOCK_UPTIME_RAW` on macOS, whose
/// `CLOCK_MONOTONIC` counts sleep; `QueryPerformanceCounter` on Windows).
#[unsafe(no_mangle)]
pub extern "C" fn nabi_time_now_ns() -> i64 {
    nabir_common::sys::monotonic_ns()
}

/// Total GC collections so far. A benchmark classifies a sample during
/// which one ran as an outlier.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_gc_collections() -> i64 {
    nabir_gc::collections() as i64
}

/// Wall clock, nanoseconds since the Unix epoch (`SystemTime`).
#[unsafe(no_mangle)]
pub extern "C" fn nabi_time_realtime_ns() -> i64 {
    nabir_common::sys::realtime_ns()
}

/// Time since boot, counting suspend (`BootTime`).
///
/// `CLOCK_BOOTTIME` on Linux, `CLOCK_MONOTONIC` (which counts sleep) on
/// macOS, the `Instant` clock on Windows.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_time_boot_ns() -> i64 {
    nabir_common::sys::boot_ns()
}

// `nabi_io_register` / `nabi_timer_register` and the coroutine `nabi_coro_park` /
// `nabi_coro_unpark` act on `core::task`'s parking table and timers,
// so they call into it through hooks the executor installs when it
// starts. Defining them in nabi (`#[export_name]`) would compile the
// executor into every program. A `waker` is a one-element `GcBuf<Waker>`.

/// The executor's hooks (C function pointers to nabi `extern "C" fn`s).
#[derive(Clone, Copy)]
struct Hooks {
    io: unsafe extern "C" fn(i32, u32, *mut u8),
    timer: unsafe extern "C" fn(u64, *mut u8),
    park: unsafe extern "C" fn(*mut u8, *mut u8),
    unpark: unsafe extern "C" fn(*mut u8),
    /// After a failed REPL cell (`session`): start again what ran on
    /// threads that are gone.
    #[cfg_attr(any(target_family = "wasm", target_os = "none"), allow(dead_code))]
    restart: unsafe extern "C" fn(),
}

static HOOKS: core::sync::atomic::AtomicPtr<Hooks> =
    core::sync::atomic::AtomicPtr::new(ptr::null_mut());

/// Install `core::task`'s hooks (the first call wins).
#[unsafe(no_mangle)]
pub extern "C" fn nabi_task_hooks_install(
    io: unsafe extern "C" fn(i32, u32, *mut u8),
    timer: unsafe extern "C" fn(u64, *mut u8),
    park: unsafe extern "C" fn(*mut u8, *mut u8),
    unpark: unsafe extern "C" fn(*mut u8),
    restart: unsafe extern "C" fn(),
) {
    // SAFETY: plain allocation, never freed: the hooks live as long as
    // the process.
    let h = unsafe { libc::malloc(core::mem::size_of::<Hooks>()) }.cast::<Hooks>();
    if h.is_null() {
        nabir_common::oom();
    }
    // SAFETY: fresh allocation.
    unsafe {
        h.write(Hooks {
            io,
            timer,
            park,
            unpark,
            restart,
        });
    }
    if HOOKS
        .compare_exchange(ptr::null_mut(), h, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        // SAFETY: ours, never published.
        unsafe { libc::free(h.cast()) };
    }
}

/// The executor's restart hook, if it started (`session`).
#[cfg(not(any(target_family = "wasm", target_os = "none")))]
fn restart_executor() {
    let h = HOOKS.load(Ordering::Acquire);
    if !h.is_null() {
        // SAFETY: published once, never freed; a nabi `extern "C" fn`.
        unsafe { ((*h).restart)() };
    }
}

fn hooks() -> Hooks {
    let h = HOOKS.load(Ordering::Acquire);
    if h.is_null() {
        fatal(
            b"nabi runtime: nabi_io_register / nabi_timer_register / nabi_coro_park outside a task",
        );
    }
    // SAFETY: published once, never freed.
    unsafe { *h }
}

/// Wake `waker` once `fd` may be readable (`interest & 1`) or writable
/// (`interest & 2`); one-shot, edge-triggered (spurious wakes possible).
///
/// # Safety
/// `waker` must be a live `GcBuf<Waker>`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nabi_io_register(fd: i32, interest: u32, waker: *mut u8) {
    // SAFETY: an installed hook; caller contract.
    unsafe { (hooks().io)(fd, interest, waker) };
}

/// Wake `waker` once `nabi_time_now_ns()` reaches `deadline` (ns).
///
/// # Safety
/// `waker` must be a live `GcBuf<Waker>`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nabi_timer_register(deadline: u64, waker: *mut u8) {
    // SAFETY: as above.
    unsafe { (hooks().timer)(deadline, waker) };
}

// Errors: `throw` / `try` / `catch` are value-based (a throwing function
// returns an `is_err` flag with its payload). Each `throw` also stores
// its `path:line:col` in the thread's throw site slot (inline in compiled
// code; `nabi_throw_site` otherwise), so a report names where the error
// started; a handled error clears it.

/// Note where the error in flight was thrown (empty: not known). `at` is
/// a static, NUL-terminated `str`: only its address is kept.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_throw_site(at: NabiStr) {
    // SAFETY: this thread's own slot.
    unsafe { tls::throw_site().write(at.ptr) };
}

/// Where the error in flight was thrown, cleared (a `catch` that may pass
/// the error on keeps it).
#[unsafe(no_mangle)]
pub extern "C" fn nabi_throw_site_take() -> NabiStr {
    // SAFETY: this thread's own slot.
    let p = unsafe { tls::throw_site().replace(ptr::null()) };
    // SAFETY: compiled code stores only NUL-terminated constants.
    let len = unsafe { str::nabi_cstr_len(p) };
    NabiStr { ptr: p, len }
}

/// Take where the error in flight was thrown, unless not known or `at`
/// itself.
fn raise_site(at: &[u8]) -> Option<&'static [u8]> {
    let s = nabi_throw_site_take();
    if s.len == 0 {
        return None;
    }
    // SAFETY: a static string of `len` bytes.
    let site = unsafe { core::slice::from_raw_parts(s.ptr, s.len) };
    (site != at).then_some(site)
}

/// Uncaught error out of `fn main() throws (..)`: a fixed
/// prefix, the error's description, `\n` to stderr, then exit status 1.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_uncaught_error(what: NabiStr) -> ! {
    uncaught(&[UNCAUGHT, what.bytes(), b"\n"])
}

const UNCAUGHT: &[u8] = b"error: `main` returned an uncaught error: ";

/// `nabi_uncaught_error` plus the `path:line:col` the error left `main`
/// at, on a second line, and the `throw` that raised it when that was
/// elsewhere.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_uncaught_error_at(what: NabiStr, at: NabiStr) -> ! {
    let (sep, site) = raise_site(at.bytes()).map_or((&b""[..], &b""[..]), |s| (RAISED, s));
    uncaught(&[
        UNCAUGHT,
        what.bytes(),
        b"\n  --> ",
        at.bytes(),
        sep,
        site,
        b"\n",
    ])
}

const RAISED: &[u8] = b"\n  raised at ";

fn uncaught(parts: &[&[u8]]) -> ! {
    nabir_common::write_all(2, parts);
    exit(1)
}

/// Unrecoverable panic: `thread '<name>' panicked:`, `msg` and `\n` to
/// stderr, then exit 101.
///
/// # Safety
/// `msg` must be null or valid for reads of `msg_len` bytes.
#[cfg_attr(any(test, not(nabi_frames)), unsafe(no_mangle))]
pub unsafe extern "C" fn nabi_panic(msg: *const u8, msg_len: usize) -> ! {
    let msg = NabiStr {
        ptr: msg,
        len: if msg.is_null() { 0 } else { msg_len },
    };
    panic_at(msg.bytes(), None)
}

/// The thread that panicked, by the address of its own name slot (a
/// per-thread address); 0: none.
static PANICKING: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

/// `thread::panicking()`: 1 when the calling thread panicked and the
/// process is ending (code the exit hook runs).
#[unsafe(no_mangle)]
pub extern "C" fn nabi_thread_panicking() -> i32 {
    i32::from(PANICKING.load(Ordering::Relaxed) == tls::thread_name() as usize)
}

/// A panic's report, `thread '<name>' panicked at <at>:\n<msg>\n` (no
/// ` at <at>` without a location), then exit 101.
pub(crate) fn panic_at(msg: &[u8], at: Option<&[u8]>) -> ! {
    panic_parts(&[msg], at)
}

/// `panic_at` of a message in up to 8 parts.
fn panic_parts(msg: &[&[u8]], at: Option<&[u8]>) -> ! {
    PANICKING.store(tls::thread_name() as usize, Ordering::Relaxed);
    let name = thread::name();
    let (sep, at) = at.map_or((&b""[..], &b""[..]), |a| (&b" at "[..], a));
    let mut parts: [&[u8]; 16] = [&[]; 16];
    let head: [&[u8]; 6] = [b"thread '", name, b"' panicked", sep, at, b":\n"];
    parts[..6].copy_from_slice(&head);
    let n = msg.len().min(8);
    parts[6..6 + n].copy_from_slice(&msg[..n]);
    parts[6 + n] = b"\n";
    nabir_common::write_all(2, &parts[..=6 + n]);
    backtrace::report();
    catch::unwind(&msg[..n], at_of(sep, at));
    exit(101)
}

/// The location `panic_parts` printed, if any.
fn at_of<'a>(sep: &[u8], at: &'a [u8]) -> Option<&'a [u8]> {
    (!sep.is_empty()).then_some(at)
}

/// `nabi_panic_str` with the `path:line:col` of the code that panicked
/// (a failed runtime check, or the first caller outside a
/// `#[track_caller]` chain).
#[cfg_attr(any(test, not(nabi_frames)), unsafe(no_mangle))]
pub extern "C" fn nabi_panic_at(msg: NabiStr, at: NabiStr) -> ! {
    panic_at(msg.bytes(), Some(at.bytes()))
}

/// A failed `try!` at `loc`: the error's description, the `throw` that
/// raised it and the `path:line:col` it reached the `try!` through (`at`;
/// alone when it was thrown there or the `throw` is not known).
#[cfg_attr(any(test, not(nabi_frames)), unsafe(no_mangle))]
pub extern "C" fn nabi_try_failed(what: NabiStr, at: NabiStr, loc: NabiStr) -> ! {
    let (site, through) = raise_site(at.bytes()).map_or((&b""[..], &b""[..]), |s| (s, THROUGH));
    let msg: [&[u8]; 6] = [
        b"try! failed: ",
        what.bytes(),
        RAISED,
        site,
        through,
        at.bytes(),
    ];
    panic_parts(&msg, Some(loc.bytes()))
}

const THROUGH: &[u8] = b"\n  through ";

/// `par` ran a call's items one at a time because two reach one collection:
/// a warning, once per call site (`at` is a static string,
/// told apart by address; past 64 sites every one warns).
#[unsafe(no_mangle)]
pub extern "C" fn nabi_par_sequential(what: NabiStr, at: NabiStr) {
    static SEEN: [AtomicUsize; 64] = [const { AtomicUsize::new(0) }; 64];
    let key = at.bytes().as_ptr() as usize;
    for s in &SEEN {
        match s.compare_exchange(0, key, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => break,
            Err(k) if k == key => return,
            Err(_) => {}
        }
    }
    nabir_common::write_all(
        2,
        &[
            b"warning: ",
            what.bytes(),
            b" at ",
            at.bytes(),
            b": two items reach one collection or future, so they run one at a time on this thread; \
              give each item its own copy, freeze what they only read (`xs.freeze()`), or split a list with `chunks_mut`\n",
        ],
    );
}

/// `core::panic::oom()`: out of memory,
/// exit 101. Runtime allocation failures take the same path.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_oom() -> ! {
    nabir_common::oom()
}

/// `nabi_panic` for a nabi `str` value.
#[cfg_attr(any(test, not(nabi_frames)), unsafe(no_mangle))]
pub extern "C" fn nabi_panic_str(msg: NabiStr) -> ! {
    panic_at(msg.bytes(), None)
}

// From here on: conveniences, no ABI stability.

/// `std::process::exit(code)`: exit now with `code`.
/// `finally` and `Drop` do not run; `exit` flushes buffered stdout.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_exit(code: i32) -> ! {
    exit(code)
}

/// The hook `nabi_set_exit_hook` sets (null: none).
static EXIT_HOOK: core::sync::atomic::AtomicPtr<()> =
    core::sync::atomic::AtomicPtr::new(ptr::null_mut());

/// The hook a panic, an uncaught error, `exit` or (Windows) a stack
/// overflow calls before ending the process; null: none.
///
/// It gets the exit status, on the thread ending the process, which
/// ends when it returns. The Windows REPL host ends just the running
/// cell instead (it does not return).
#[unsafe(no_mangle)]
pub extern "C" fn nabi_set_exit_hook(hook: Option<extern "C" fn(i32)>) {
    EXIT_HOOK.store(
        hook.map_or(ptr::null_mut(), |h| h as *mut ()),
        Ordering::Release,
    );
}

/// Run the exit hook, if any, for status `code`.
pub(crate) fn ending(code: i32) {
    let h = EXIT_HOOK.load(Ordering::Acquire);
    if !h.is_null() {
        // SAFETY: only `nabi_set_exit_hook` stores, a function of this type.
        let hook = unsafe { core::mem::transmute::<*mut (), extern "C" fn(i32)>(h) };
        hook(code);
    }
}

/// The exit hook, then end the process with `code`; `exit` flushes
/// buffered stdout.
fn exit(code: i32) -> ! {
    ending(code);
    // SAFETY: plain libc call.
    unsafe { libc::exit(code) }
}

/// `bare::alloc::alloc`: null on failure (`size == 0`, bad
/// `align`, overflow, OOM) instead of aborting. `zeroed != 0`
/// zero-fills. Freed with `nabi_free`.
#[unsafe(no_mangle)]
pub extern "C" fn nabi_alloc_raw(size: usize, align: usize, zeroed: i32) -> *mut u8 {
    if size == 0 || !align.is_power_of_two() {
        return ptr::null_mut();
    }
    let Some(rounded) = size.checked_add(align - 1).map(|s| s & !(align - 1)) else {
        return ptr::null_mut();
    };
    let p = nabir_common::sys::alloc_aligned(rounded, align);
    if !p.is_null() && zeroed != 0 {
        // SAFETY: `p` is valid for `rounded` bytes.
        unsafe { ptr::write_bytes(p, 0, rounded) };
    }
    p
}

/// `bare::alloc::realloc`: a block of `new_size` bytes holding the
/// first `min(old_size, new_size)` bytes of `ptr`; null on failure (then
/// `ptr` stays valid). `ptr` is freed on success.
///
/// # Safety
/// `ptr` must be a live block from `nabi_alloc_raw` with `old_size` and
/// `align`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nabi_realloc_raw(
    ptr: *mut u8,
    old_size: usize,
    align: usize,
    new_size: usize,
) -> *mut u8 {
    let q = nabi_alloc_raw(new_size, align, 0);
    if !q.is_null() {
        // SAFETY: both blocks are valid for the copied length; caller
        // contract on `ptr`.
        unsafe {
            ptr::copy_nonoverlapping(ptr, q, old_size.min(new_size));
            nabi_free(ptr);
        }
    }
    q
}

/// # Safety
/// `ptr` must be null or a live pointer returned by `nabi_alloc_raw`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nabi_free(ptr: *mut u8) {
    // SAFETY: caller contract; freeing null is a no-op.
    unsafe { nabir_common::sys::free_aligned(ptr) };
}

#[cfg(test)]
mod tests {
    use super::*;
    use nabir_abi::fmt::spec::{FmtSpec, pack, parse, unpack};
    use nabir_common::header::ObjectHeader;

    /// Held by the tests that collect or hold unrooted GC objects: the
    /// heap is one, and a collection on one test thread frees what
    /// another holds unrooted.
    pub(crate) static GC_TEST: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// A test thread attached as a mutator until this drops, panic or
    /// not. A thread that ends attached leaves its record behind: the
    /// dead thread's state slot reads `RUNNING` (0), and every later
    /// stop waits for it forever.
    pub(crate) struct Mutator;

    impl Mutator {
        pub(crate) fn attach() -> Self {
            nabir_common::tls::thread_init();
            nabir_gc::world::attach_current();
            Self
        }
    }

    impl Drop for Mutator {
        fn drop(&mut self) {
            nabir_gc::world::detach();
        }
    }

    #[test]
    #[allow(clippy::cast_ptr_alignment)] // the header is 8-aligned
    fn gc_alloc_initializes_header_and_alignment() {
        let _gc = GC_TEST
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let p = nabi_gc_alloc_slow(24, 64, 12345);
        // The payload, after the 16-byte header, is 64-aligned.
        assert_eq!((p as usize + 16) % 64, 0);
        // SAFETY: fresh 24-byte allocation with an initialized header.
        let h = unsafe { &*p.cast::<ObjectHeader>() };
        assert_eq!(h.type_id, 12345);
        // Over-aligned: a LOS object; `gc_word` is the size.
        assert_eq!(
            h.gc_flags.load(Ordering::Relaxed),
            nabir_common::header::FLAG_LARGE
        );
        assert_eq!(h.gc_word, 24);
        // Block objects keep their 8-rounded size in `gc_word` too.
        // SAFETY: as above.
        let h = unsafe { &*nabi_gc_alloc_slow(20, 8, 7).cast::<ObjectHeader>() };
        assert_eq!((h.type_id, h.gc_word), (7, 24));
        str::tests::string_ops();
        str::tests::unicode_ops();
        fmt::tests::fmt_ops();
        fmt::tests::exp_rounding();
        fmt::tests::general_and_debug();
        fmt::tests::formatter_ops();
        fuzz::allocating();
    }

    /// `[[fill]align][sign]['#']['0'][width]['.' precision][type]`,
    /// and `pack` / `unpack` round trips.
    #[test]
    fn fmt_spec_grammar() {
        let s = |t: &str| parse(t).unwrap_or_else(|e| panic!("{t:?}: {e}"));
        let plain = s("");
        assert_eq!((plain.fill, plain.align, plain.ty), (' ', 0, 0));
        assert_eq!((plain.width, plain.precision), (None, None));
        let all = s("*^+#010.3e");
        assert_eq!(
            all,
            FmtSpec {
                fill: '*',
                align: b'^',
                sign: b'+',
                alt: true,
                zero: true,
                width: Some(10),
                grouping: 0,
                precision: Some(3),
                ty: b'e',
            }
        );
        // An align char alone; a fill takes any char, `<` included.
        assert_eq!((s(">").fill, s(">").align), (' ', b'>'));
        assert_eq!(
            (s("<<5").fill, s("<<5").align, s("<<5").width),
            ('<', b'<', Some(5))
        );
        assert_eq!((s("é=8").fill, s("é=8").align), ('é', b'='));
        assert_eq!((s("08").zero, s("08").width), (true, Some(8)));
        assert_eq!((s(".0f").precision, s(".0f").ty), (Some(0), b'f'));
        assert_eq!(s(" ").sign, b' ');
        for ty in ["d", "b", "o", "x", "X", "f", "e", "E", "g", "s", "?"] {
            assert_eq!(s(ty).ty, ty.as_bytes()[0]);
        }
        for bad in [
            "z",
            ".",
            "5.",
            "10.x2",
            "1.2.3",
            "+-",
            "#x5",
            "99999999999999999999999",
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
        // A wide precision is kept; `s d g` pack as none; the rest round-trips.
        let (f, c) = pack(&s(".9999"));
        assert_eq!(unpack(f, c).precision, Some(9999));
        assert_eq!(unpack(pack(&s("d")).0, pack(&s("d")).1).ty, 0);
        for t in ["*^+#010.3e", "", ">7x", "é<3?", "-.2E"] {
            let (f, c) = pack(&s(t));
            assert_eq!(unpack(f, c), s(t), "{t:?}");
        }
    }

    /// `NABIR_WORKERS` sets the pool size. Not on Windows: `set_var`
    /// there does not reach the C runtime's copy that `getenv` reads.
    #[cfg(not(windows))]
    #[test]
    fn workers_env() {
        let n = thread::nabi_cpu_count();
        if n == 0 {
            return;
        }
        // SAFETY: no other test reads or sets it.
        unsafe { std::env::remove_var("NABIR_WORKERS") };
        assert_eq!(thread::nabi_sched_workers(), n);
        // SAFETY: as above.
        unsafe { std::env::set_var("NABIR_WORKERS", "5") };
        assert_eq!(thread::nabi_sched_workers(), 5);
        // SAFETY: as above.
        unsafe { std::env::remove_var("NABIR_WORKERS") };
    }
}
