//! Mutator TLS.
//!
//! The ABI slots are one exported `thread_local` block,
//! `nabi_tls`, at the offsets of `tls_abi`. A nabi function gets the
//! block's address from its caller as a hidden last argument;
//! code entered from outside nabi reads it once. Defined in assembly
//! because stable `no_std` Rust cannot export TLS symbols. The runtime
//! reaches its own block through the hidden `nabi_tls_addr`, another
//! thread's through the thread registry (`nabir_gc::world`).
//!
//! Mach-O: `__thread_vars`. ELF: initial-exec on `x86_64`, local-exec
//! elsewhere, as the runtime is always linked into the executable (arm
//! reads `TPIDRURO`; on powerpc64 build.rs writes it). Android: TLS
//! descriptors (aarch64) and `__tls_get_addr` (`x86_64`), which the
//! linker relaxes in an executable, as bionic lets no `dlopen`ed library
//! use initial-exec. COFF: `.tls$` through `_tls_index`, which nabic's startup
//! object defines with the TLS directory. wasm32: a plain global, or wasm
//! TLS on `wasm32-wasip1-threads` (defined by nabic's startup object).
//!
//! The assembly is in `tls/<arch>/<format>.s`; its `{size}` is the
//! block's size.

use crate::AtomicU64;
use crate::tls_abi::{self, Slot};
use core::sync::atomic::{AtomicU8, Ordering};

pub const MUTATOR_RUNNING: u8 = 0;
pub const MUTATOR_GC_COOPERATING: u8 = 1;
pub const MUTATOR_PARKED: u8 = 2;

static NEXT_WORKER_ID: AtomicU64 = AtomicU64::new(0);

// The block `nabi_tls` and its accessor `nabi_tls_addr`. wasm32 has
// none: `wasm32-wasip1-threads` gets them from nabic
// (`wasm_tls_object`, nabic_driver backend.rs).
#[cfg(all(target_vendor = "apple", target_arch = "aarch64"))]
core::arch::global_asm!(include_str!("tls/aarch64/macho.s"), size = const SIZE);
#[cfg(all(
    any(target_os = "linux", target_os = "freebsd"),
    target_arch = "x86_64"
))]
core::arch::global_asm!(include_str!("tls/x86_64/elf.s"), size = const SIZE);
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
core::arch::global_asm!(include_str!("tls/aarch64/elf.s"), size = const SIZE);
#[cfg(all(target_os = "android", target_arch = "aarch64"))]
core::arch::global_asm!(include_str!("tls/aarch64/android.s"), size = const SIZE);
#[cfg(all(target_os = "android", target_arch = "x86_64"))]
core::arch::global_asm!(include_str!("tls/x86_64/android.s"), size = const SIZE);
#[cfg(all(target_os = "linux", target_arch = "x86"))]
core::arch::global_asm!(include_str!("tls/x86/elf.s"), size = const SIZE);
#[cfg(all(target_os = "linux", target_arch = "arm"))]
core::arch::global_asm!(include_str!("tls/arm/elf.s"), size = const SIZE);
#[cfg(any(
    all(target_os = "linux", target_arch = "riscv64"),
    all(target_os = "none", target_arch = "riscv32")
))]
core::arch::global_asm!(include_str!("tls/riscv/elf.s"), size = const SIZE);
#[cfg(all(target_os = "linux", target_arch = "s390x"))]
core::arch::global_asm!(include_str!("tls/s390x/elf.s"), size = const SIZE);
#[cfg(all(windows, target_arch = "x86_64"))]
core::arch::global_asm!(include_str!("tls/x86_64/coff.s"), size = const SIZE);
#[cfg(all(windows, target_arch = "aarch64"))]
core::arch::global_asm!(include_str!("tls/aarch64/coff.s"), size = const SIZE);
#[cfg(all(windows, target_arch = "x86"))]
core::arch::global_asm!(include_str!("tls/x86/coff.s"), size = const SIZE);

// powerpc64: stable Rust has no inline assembly there; the block and
// the accessor are build.rs's object, its block this large.
#[cfg(target_arch = "powerpc64")]
const _: () = assert!(SIZE <= 256);

// The assembly's `{size}` (wasm32 has no assembly).
#[cfg(not(target_family = "wasm"))]
#[cfg_attr(target_arch = "powerpc64", allow(dead_code))]
const SIZE: usize = tls_abi::SIZE.offset(size_of::<usize>() as u32) as usize;

#[cfg(not(any(
    target_vendor = "apple",
    target_family = "wasm",
    all(
        any(target_os = "linux", windows),
        any(target_arch = "x86_64", target_arch = "aarch64", target_arch = "x86")
    ),
    all(
        target_os = "linux",
        any(
            target_arch = "arm",
            target_arch = "riscv64",
            target_arch = "powerpc64",
            target_arch = "s390x"
        )
    ),
    all(target_os = "none", target_arch = "riscv32"),
    all(target_os = "freebsd", target_arch = "x86_64"),
    all(
        target_os = "android",
        any(target_arch = "x86_64", target_arch = "aarch64")
    )
)))]
compile_error!("nabir: no `nabi_tls` definition for this target");

/// Address of this thread's block (on `wasm32-wasip1-threads` nabic's
/// startup object defines the block and the accessor: stable Rust cannot
/// define wasm TLS).
#[cfg(not(all(nabi_solo, target_family = "wasm")))]
fn block() -> *mut u8 {
    unsafe extern "C" {
        fn nabi_tls_addr() -> *mut u8;
    }
    // SAFETY: the accessor takes no arguments and only returns the
    // address of this thread's block.
    unsafe { nabi_tls_addr() }
}

/// One thread: the block is a plain global (what a wasm toolchain makes
/// of `thread_local` globals without the atomics feature).
#[cfg(all(nabi_solo, target_family = "wasm"))]
fn block() -> *mut u8 {
    const N: usize = tls_abi::SIZE.offset(4) as usize;
    #[repr(C, align(8))]
    struct Block(core::cell::UnsafeCell<[u8; N]>);
    // SAFETY: one thread.
    unsafe impl Sync for Block {}
    #[unsafe(export_name = "nabi_tls")]
    static BLOCK: Block = Block(core::cell::UnsafeCell::new([0; N]));
    BLOCK.0.get().cast()
}

/// Address of slot `s` of this thread's block.
fn at(s: Slot) -> *mut u8 {
    // SAFETY: the slot lies inside the block.
    unsafe { block().add(s.offset(size_of::<usize>() as u32) as usize) }
}

/// Address of this thread's `nabi_tls_stack_guard` (`[lo, hi)`).
#[allow(clippy::cast_ptr_alignment)] // pointer-aligned slot
pub fn stack_guard() -> *mut [usize; 2] {
    at(tls_abi::STACK_GUARD).cast()
}

/// Address of this thread's `nabi_tls_shadow_stack_top` (the head of its
/// shadow-stack frame chain, pushed / popped by compiled code).
#[allow(clippy::cast_ptr_alignment)] // pointer-aligned slot
pub fn shadow_stack_top() -> *mut *mut u8 {
    at(tls_abi::SHADOW_STACK_TOP).cast()
}

/// The `nabi_tls_yield_flag` bit of a stop (the runtime raises and clears
/// it).
///
/// Other bits are the embedder's: the Windows REPL host raises one to
/// end a running cell. Compiled code's polls take the slow path for any
/// nonzero flag.
pub const YIELD_STOP: u8 = 1;

/// This thread's `nabi_tls_yield_flag` (set by a collecting thread).
pub fn yield_flag() -> &'static AtomicU8 {
    // SAFETY: the slot is a live, 1-byte TLS slot of this thread;
    // `AtomicU8` has the layout of `u8`.
    unsafe { &*at(tls_abi::YIELD_FLAG).cast::<AtomicU8>() }
}

/// This thread's `nabi_tls_mutator_state` (read by a collecting thread).
pub fn mutator_state() -> &'static AtomicU8 {
    // SAFETY: as above.
    unsafe { &*at(tls_abi::MUTATOR_STATE).cast::<AtomicU8>() }
}

/// This thread's depth of bracketed `#[no_safepoint]` calls.
pub fn no_safepoint() -> *mut u8 {
    at(tls_abi::NO_SAFEPOINT)
}

/// `attached()`: started by nabi, or `nabi_thread_init`.
pub const ATTACHED_INIT: u8 = 1;
/// `attached()`: a thread nabi did not start, attached by a callback.
pub const ATTACHED_CALLBACK: u8 = 2;

/// How this thread joined the runtime: 0 (not, or left), or one of
/// `ATTACHED_*`. A new thread's block is zeroed.
pub fn attached() -> *mut u8 {
    at(tls_abi::ATTACHED)
}

/// `nabi_thread_init` slot values. The TLAB starts empty
/// (the first allocation refills it); the write barrier logs into its
/// own remembered-set chunk, allocated by the first logged store.
#[allow(clippy::cast_ptr_alignment)] // pointer-aligned slots
pub fn thread_init() {
    yield_flag().store(0, Ordering::Relaxed);
    mutator_state().store(MUTATOR_RUNNING, Ordering::Relaxed);
    // SAFETY: each slot is this thread's own and aligned.
    unsafe {
        for s in [
            tls_abi::TLAB_CURSOR,
            tls_abi::TLAB_LIMIT,
            tls_abi::MEDIUM_CURSOR,
            tls_abi::MEDIUM_LIMIT,
            tls_abi::SHADOW_STACK_TOP,
            tls_abi::WB_LOG_CURSOR,
            tls_abi::WB_LOG_LIMIT,
            tls_abi::SATB_CURSOR,
            tls_abi::SATB_LIMIT,
            tls_abi::TASK_WORKER,
            tls_abi::TASK_LANE,
            tls_abi::HELD_LOCKS,
        ] {
            at(s).cast::<usize>().write(0);
        }
        throw_site().write(core::ptr::null());
        errno_slots().0.write(0);
        errno_slots().1.write(0);
        foreign().write(0);
        catch().write(core::ptr::null_mut());
        worker_id().write(new_worker_id());
        no_safepoint().write(0);
        attached().write(ATTACHED_INIT);
    }
}

/// Addresses of this thread's `nabi_tls_wb_log_cursor` and
/// `nabi_tls_wb_log_limit` (its remembered-set chunk, `nabir_gc`).
#[allow(clippy::cast_ptr_alignment)] // pointer-aligned slots
pub fn wb_log() -> [*mut usize; 2] {
    [
        at(tls_abi::WB_LOG_CURSOR).cast(),
        at(tls_abi::WB_LOG_LIMIT).cast(),
    ]
}

/// Addresses of this thread's `nabi_tls_satb_cursor` and
/// `nabi_tls_satb_limit` (its SATB log chunk, `nabir_gc`).
#[allow(clippy::cast_ptr_alignment)] // pointer-aligned slots
pub fn satb_log() -> [*mut usize; 2] {
    [
        at(tls_abi::SATB_CURSOR).cast(),
        at(tls_abi::SATB_LIMIT).cast(),
    ]
}

/// Addresses of this thread's `nabi_tls_tlab_cursor` and
/// `nabi_tls_tlab_limit` (the bump range compiled code allocates from).
#[allow(clippy::cast_ptr_alignment)] // pointer-aligned slots
pub fn tlab() -> [*mut usize; 2] {
    [
        at(tls_abi::TLAB_CURSOR).cast(),
        at(tls_abi::TLAB_LIMIT).cast(),
    ]
}

/// Addresses of this thread's medium cursor and limit (implementation-
/// defined; the bump range of `nabir_gc`'s objects over a line that miss
/// the TLAB).
#[allow(clippy::cast_ptr_alignment)] // pointer-aligned slots
pub fn medium() -> [*mut usize; 2] {
    [
        at(tls_abi::MEDIUM_CURSOR).cast(),
        at(tls_abi::MEDIUM_LIMIT).cast(),
    ]
}

/// Address of this thread's `nabi_tls_task_worker`.
#[allow(clippy::cast_ptr_alignment)] // pointer-aligned slot
pub fn task_worker() -> *mut usize {
    at(tls_abi::TASK_WORKER).cast()
}

/// Address of this thread's head of running static initializers
/// (implementation-defined).
#[allow(clippy::cast_ptr_alignment)] // pointer-aligned slot
pub fn static_inits() -> *mut usize {
    at(tls_abi::STATIC_INITS).cast()
}

/// Address of this thread's record of the locks it holds outside the
/// task executor (`nabir_gc::world::note_lock`, implementation-defined),
/// null = none yet.
#[allow(clippy::cast_ptr_alignment)] // pointer-aligned slot
pub fn held_locks() -> *mut usize {
    at(tls_abi::HELD_LOCKS).cast()
}

/// Address of this thread's name, `{ptr, len}` (implementation-defined;
/// owned by `nabir::thread`).
#[allow(clippy::cast_ptr_alignment)] // pointer-aligned slot
pub fn thread_name() -> *mut [usize; 2] {
    at(tls_abi::THREAD_NAME).cast()
}

/// Address of this thread's throw site, a NUL-terminated string or null
/// (implementation-defined; written by compiled code at a `throw`).
#[allow(clippy::cast_ptr_alignment)] // pointer-aligned slot
pub fn throw_site() -> *mut *const u8 {
    at(tls_abi::THROW_SITE).cast()
}

/// Addresses of this thread's saved errno and `GetLastError`
/// (implementation-defined; `nabi_errno_save`).
#[allow(clippy::cast_ptr_alignment)] // 4-aligned slots
pub fn errno_slots() -> (*mut i32, *mut u32) {
    (at(tls_abi::ERRNO).cast(), at(tls_abi::LAST_ERROR).cast())
}

/// Address of this thread's count of C entries foreign code called
/// (implementation-defined).
#[allow(clippy::cast_ptr_alignment)] // 4-aligned slot
pub fn foreign() -> *mut u32 {
    at(tls_abi::FOREIGN).cast()
}

/// Address of this thread's innermost `#[on_panic]` recovery record
/// (implementation-defined), null: none.
#[allow(clippy::cast_ptr_alignment)] // pointer-aligned slot
pub fn catch() -> *mut *mut u8 {
    at(tls_abi::CATCH).cast()
}

/// Address of this thread's executor lane (implementation-defined).
#[allow(clippy::cast_ptr_alignment)] // pointer-aligned slot
pub fn task_lane() -> *mut usize {
    at(tls_abi::TASK_LANE).cast()
}

/// Address of this thread's `nabi_tls_worker_id`.
#[allow(clippy::cast_ptr_alignment)] // 8-aligned slot
pub fn worker_id() -> *mut u64 {
    at(tls_abi::WORKER_ID).cast()
}

/// A fresh, process-unique worker id.
pub fn new_worker_id() -> u64 {
    NEXT_WORKER_ID.fetch_add(1, Ordering::Relaxed)
}
