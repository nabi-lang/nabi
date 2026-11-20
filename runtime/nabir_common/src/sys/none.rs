//! OS primitives without an OS (`riscv32imac-unknown-none-elf`).
//!
//! One thread; memory, output, time and the end of the program come
//! from four board functions:
//!
//! - `nabi_bare_morecore(len) -> *mut u8`: `len` more bytes of heap, or
//!   null. Consecutive calls may hand out adjacent memory.
//! - `nabi_bare_write(fd, ptr, len) -> isize`: write to stdout (1) or
//!   stderr (2); the count or a negative error.
//! - `nabi_bare_clock_ns(realtime) -> i64`: nanoseconds of a monotonic
//!   clock (`realtime` 0) or since the Unix epoch (1).
//! - `nabi_bare_exit(code) -> !`.
//!
//! The runtime defines them weakly as Linux system calls, which is what
//! qemu-riscv32's user mode runs a static image with; a board defines
//! its own. The weak `_start` sets `gp` and calls `nabir_bare_start` with
//! the initial stack pointer: it takes `argc` and `argv` from there as a
//! Linux process starts, makes the main thread's TLS block from the
//! image's `PT_TLS`, then runs `main` and exits with its result. A
//! board's own startup sets up `sp` and `gp` and calls
//! `nabir_bare_start(null)`: no arguments.
//!
//! The heap is a first-fit, address-ordered free list with coalescing
//! over the board's memory; `malloc` and friends of [`libc`] are it.

// `static` primitives: `const fn new` is the constructor.
#![allow(clippy::new_without_default)]

use core::cell::UnsafeCell;
use core::ptr;

/// The pieces of the C library the runtime uses, over this module.
pub mod libc {
    pub use core::ffi::{c_char, c_int, c_void};

    pub const EINTR: c_int = 4;
    pub const EBADF: c_int = 9;
    pub const ESRCH: c_int = 3;

    /// # Safety
    /// As C's `malloc`.
    pub unsafe fn malloc(size: usize) -> *mut c_void {
        super::alloc_aligned(size, 16).cast()
    }

    /// # Safety
    /// As C's `calloc`.
    pub unsafe fn calloc(n: usize, size: usize) -> *mut c_void {
        let Some(len) = n.checked_mul(size) else {
            return core::ptr::null_mut();
        };
        let p = super::alloc_aligned(len, 16);
        if !p.is_null() {
            // SAFETY: a fresh block of `len` bytes.
            unsafe { core::ptr::write_bytes(p, 0, len) };
        }
        p.cast()
    }

    /// # Safety
    /// As C's `realloc`.
    pub unsafe fn realloc(p: *mut c_void, size: usize) -> *mut c_void {
        // SAFETY: caller contract.
        unsafe { super::HEAP.realloc(p.cast(), size) }.cast()
    }

    /// # Safety
    /// As C's `free`.
    pub unsafe fn free(p: *mut c_void) {
        // SAFETY: caller contract.
        unsafe { super::free_aligned(p.cast()) };
    }

    /// # Safety
    /// None; `unsafe` as C's.
    pub unsafe fn exit(code: c_int) -> ! {
        super::exit(code)
    }

    /// # Safety
    /// None; `unsafe` as C's.
    pub unsafe fn abort() -> ! {
        super::exit(134)
    }

    /// No environment.
    ///
    /// # Safety
    /// None; `unsafe` as C's.
    pub const unsafe fn getenv(_name: *const c_char) -> *const c_char {
        core::ptr::null()
    }

    /// # Safety
    /// `s` must be NUL-terminated.
    // Not `const`, like C's.
    #[allow(clippy::missing_const_for_fn)]
    pub unsafe fn strlen(s: *const c_char) -> usize {
        // SAFETY: caller contract.
        unsafe { core::ffi::CStr::from_ptr(s) }.count_bytes()
    }
}

unsafe extern "C" {
    fn nabi_bare_morecore(len: usize) -> *mut u8;
    fn nabi_bare_write(fd: i32, p: *const u8, len: usize) -> isize;
    fn nabi_bare_clock_ns(realtime: i32) -> i64;
    fn nabi_bare_exit(code: i32) -> !;
}

// The weak board functions and `_start`.
#[cfg(target_arch = "riscv32")]
core::arch::global_asm!(include_str!("none/riscv.s"));

/// Linux `brk` (214): the break moved up by `len`.
#[cfg(target_arch = "riscv32")]
#[unsafe(no_mangle)]
extern "C" fn nabir_linux_morecore(len: usize) -> *mut u8 {
    static BRK: Global<usize> = Global::new(0);
    let brk = |to: usize| -> usize {
        let r: usize;
        // SAFETY: `brk` only moves the break.
        unsafe {
            core::arch::asm!("ecall", inlateout("a0") to => r, in("a7") 214);
        }
        r
    };
    // SAFETY: one thread.
    let cur = unsafe { &mut *BRK.0.get() };
    if *cur == 0 {
        *cur = brk(0);
    }
    let Some(want) = cur.checked_add(len) else {
        return ptr::null_mut();
    };
    if brk(want) < want {
        return ptr::null_mut();
    }
    let p = *cur;
    *cur = want;
    p as *mut u8
}

/// Linux `clock_gettime64` (403) of `CLOCK_MONOTONIC` / `CLOCK_REALTIME`.
#[cfg(target_arch = "riscv32")]
#[unsafe(no_mangle)]
extern "C" fn nabir_linux_clock_ns(realtime: i32) -> i64 {
    let mut ts = [0i64; 2];
    let clock = usize::from(realtime == 0);
    // SAFETY: `ts` is a valid `__kernel_timespec`.
    unsafe {
        core::arch::asm!("ecall", inlateout("a0") clock => _, in("a1") ts.as_mut_ptr(),
            in("a7") 403);
    }
    ts[0] * 1_000_000_000 + ts[1]
}

// The inverse hyperbolic functions compiler_builtins' libm lacks, as
// musl computes them (weak above: a board's libm wins).
unsafe extern "C" {
    fn log(x: f64) -> f64;
    fn log1p(x: f64) -> f64;
    fn sqrt(x: f64) -> f64;
}

/// The biased exponent of `x`.
const fn exponent(x: f64) -> u64 {
    (x.to_bits() >> 52) & 0x7ff
}

const LN2: f64 = core::f64::consts::LN_2;

#[unsafe(no_mangle)]
extern "C" fn nabir_acosh(x: f64) -> f64 {
    let e = exponent(x);
    // SAFETY: libm functions of compiler_builtins.
    unsafe {
        if e < 0x3ff + 1 {
            log1p(x - 1.0 + sqrt((x - 1.0) * (x - 1.0) + 2.0 * (x - 1.0)))
        } else if e < 0x3ff + 26 {
            log(2.0 * x - 1.0 / (x + sqrt(x * x - 1.0)))
        } else {
            log(x) + LN2
        }
    }
}

#[unsafe(no_mangle)]
extern "C" fn nabir_asinh(x: f64) -> f64 {
    let e = exponent(x);
    let a = x.abs();
    // SAFETY: as above.
    let y = unsafe {
        if e >= 0x3ff + 26 {
            log(a) + LN2
        } else if e > 0x3ff {
            log(2.0 * a + 1.0 / (sqrt(a * a + 1.0) + a))
        } else if e >= 0x3ff - 26 {
            log1p(a + a * a / (sqrt(a * a + 1.0) + 1.0))
        } else {
            a
        }
    };
    if x.is_sign_negative() { -y } else { y }
}

#[unsafe(no_mangle)]
extern "C" fn nabir_atanh(x: f64) -> f64 {
    let e = exponent(x);
    let a = x.abs();
    // SAFETY: as above.
    let y = unsafe {
        if e < 0x3ff - 32 {
            a
        } else if e < 0x3ff - 1 {
            0.5 * log1p(2.0 * a + 2.0 * a * a / (1.0 - a))
        } else {
            0.5 * log1p(2.0 * (a / (1.0 - a)))
        }
    };
    if x.is_sign_negative() { -y } else { y }
}

#[unsafe(no_mangle)]
extern "C" fn nabir_acoshf(x: f32) -> f32 {
    nabir_acosh(f64::from(x)) as f32
}

#[unsafe(no_mangle)]
extern "C" fn nabir_asinhf(x: f32) -> f32 {
    nabir_asinh(f64::from(x)) as f32
}

#[unsafe(no_mangle)]
extern "C" fn nabir_atanhf(x: f32) -> f32 {
    nabir_atanh(f64::from(x)) as f32
}

/// `argc` and `argv` of the program (0 and null without them).
static ARGS: Global<(usize, *const *const u8)> = Global::new((0, ptr::null()));

/// The program's arguments as [`nabir_bare_start`] found them.
pub fn args() -> (usize, *const *const u8) {
    // SAFETY: written once before `main`.
    unsafe { *ARGS.0.get() }
}

/// Start the program: `sp` (null: none) is the initial stack, `argc`
/// then `argv`; the main thread's TLS block, then `main` and `exit` with
/// its result.
///
/// # Safety
/// Once, from the startup code, `sp` null or a Linux initial stack.
#[cfg(target_arch = "riscv32")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nabir_bare_start(sp: *const usize) -> ! {
    unsafe extern "C" {
        fn main(argc: i32, argv: *const *const u8, envp: *const *const u8) -> i32;
    }
    // SAFETY: caller contract; one thread, before `main`.
    unsafe {
        if !sp.is_null() {
            *ARGS.0.get() = (sp.read(), sp.add(1).cast());
        }
        init_tls();
    }
    let (argc, argv) = args();
    // SAFETY: the program's C entry.
    exit(unsafe { main(argc as i32, argv, ptr::null()) })
}

/// Point `tp` at a fresh copy of the image's TLS template (its
/// `PT_TLS`, found through the ELF header the image starts with).
///
/// # Safety
/// Once, before anything uses TLS.
#[cfg(target_arch = "riscv32")]
#[allow(clippy::cast_ptr_alignment)]
pub unsafe fn init_tls() {
    unsafe extern "C" {
        static __ehdr_start: u8;
    }
    let ehdr = &raw const __ehdr_start;
    // SAFETY: the ELF header and program headers are mapped with the
    // image (`nabild` puts them in the first segment).
    unsafe {
        let phoff = ehdr.add(28).cast::<u32>().read_unaligned() as usize;
        let phnum = ehdr.add(44).cast::<u16>().read_unaligned() as usize;
        for i in 0..phnum {
            let ph = ehdr.add(phoff + 32 * i).cast::<u32>();
            if ph.read_unaligned() != 7 {
                continue;
            }
            let vaddr = ph.add(2).read_unaligned() as usize;
            let filesz = ph.add(4).read_unaligned() as usize;
            let memsz = ph.add(5).read_unaligned() as usize;
            let align = (ph.add(7).read_unaligned() as usize).max(16);
            let block = alloc_aligned(memsz.max(1), align);
            if block.is_null() {
                crate::oom();
            }
            ptr::copy_nonoverlapping(vaddr as *const u8, block, filesz);
            ptr::write_bytes(block.add(filesz), 0, memsz - filesz);
            core::arch::asm!("mv tp, {0}", in(reg) block);
            return;
        }
    }
}

/// A `static` that only the one thread touches.
struct Global<T>(UnsafeCell<T>);

// SAFETY: one thread.
unsafe impl<T> Sync for Global<T> {}

impl<T> Global<T> {
    const fn new(v: T) -> Self {
        Self(UnsafeCell::new(v))
    }
}

/// `AtomicU64` where the target has no 64-bit atomics: one thread, so
/// plain loads and stores.
#[repr(transparent)]
pub struct AtomicU64(UnsafeCell<u64>);

// SAFETY: one thread.
unsafe impl Sync for AtomicU64 {}

#[allow(clippy::missing_const_for_fn, clippy::needless_pass_by_value)]
impl AtomicU64 {
    pub const fn new(v: u64) -> Self {
        Self(UnsafeCell::new(v))
    }

    /// The word at `p` as an atomic, as `core`'s `from_ptr`.
    ///
    /// # Safety
    /// `p` is aligned and valid for `'a`, and only reached through
    /// atomics meanwhile.
    pub const unsafe fn from_ptr<'a>(p: *mut u64) -> &'a Self {
        // SAFETY: `repr(transparent)` over the word; caller contract.
        unsafe { &*p.cast::<Self>() }
    }

    fn update(&self, f: impl FnOnce(u64) -> u64) -> u64 {
        // SAFETY: one thread.
        unsafe {
            let old = *self.0.get();
            *self.0.get() = f(old);
            old
        }
    }

    pub fn load(&self, _o: core::sync::atomic::Ordering) -> u64 {
        // SAFETY: one thread.
        unsafe { *self.0.get() }
    }

    pub fn store(&self, v: u64, _o: core::sync::atomic::Ordering) {
        self.update(|_| v);
    }

    pub fn swap(&self, v: u64, _o: core::sync::atomic::Ordering) -> u64 {
        self.update(|_| v)
    }

    pub fn fetch_add(&self, v: u64, _o: core::sync::atomic::Ordering) -> u64 {
        self.update(|x| x.wrapping_add(v))
    }

    pub fn fetch_sub(&self, v: u64, _o: core::sync::atomic::Ordering) -> u64 {
        self.update(|x| x.wrapping_sub(v))
    }

    pub fn fetch_or(&self, v: u64, _o: core::sync::atomic::Ordering) -> u64 {
        self.update(|x| x | v)
    }

    pub fn fetch_and(&self, v: u64, _o: core::sync::atomic::Ordering) -> u64 {
        self.update(|x| x & v)
    }

    pub fn compare_exchange(
        &self,
        current: u64,
        new: u64,
        _s: core::sync::atomic::Ordering,
        _f: core::sync::atomic::Ordering,
    ) -> Result<u64, u64> {
        let old = self.load(core::sync::atomic::Ordering::Relaxed);
        if old == current {
            self.store(new, core::sync::atomic::Ordering::Relaxed);
            Ok(old)
        } else {
            Err(old)
        }
    }

    pub fn compare_exchange_weak(
        &self,
        current: u64,
        new: u64,
        s: core::sync::atomic::Ordering,
        f: core::sync::atomic::Ordering,
    ) -> Result<u64, u64> {
        self.compare_exchange(current, new, s, f)
    }
}

// Compiled code's 8-byte atomics (riscv32 has none): one thread, so
// plain accesses. `order` arguments are ignored.

#[unsafe(no_mangle)]
const unsafe extern "C" fn __atomic_load_8(p: *const u64, _order: i32) -> u64 {
    // SAFETY: compiled code passes a valid 8-byte location.
    unsafe { p.read() }
}

#[unsafe(no_mangle)]
const unsafe extern "C" fn __atomic_store_8(p: *mut u64, v: u64, _order: i32) {
    // SAFETY: as above.
    unsafe { p.write(v) };
}

#[unsafe(no_mangle)]
unsafe extern "C" fn __atomic_exchange_8(p: *mut u64, v: u64, _order: i32) -> u64 {
    // SAFETY: as above.
    unsafe { p.replace(v) }
}

#[unsafe(no_mangle)]
const unsafe extern "C" fn __atomic_compare_exchange_8(
    p: *mut u64,
    expected: *mut u64,
    desired: u64,
    _weak: bool,
    _success: i32,
    _failure: i32,
) -> bool {
    // SAFETY: as above; `expected` is the caller's slot.
    unsafe {
        let cur = p.read();
        if cur == expected.read() {
            p.write(desired);
            true
        } else {
            expected.write(cur);
            false
        }
    }
}

macro_rules! fetch_op {
    ($($name:ident => |$a:ident, $b:ident| $e:expr;)+) => {$(
        #[unsafe(no_mangle)]
        const unsafe extern "C" fn $name(p: *mut u64, v: u64, _order: i32) -> u64 {
            // SAFETY: compiled code passes a valid 8-byte location.
            unsafe {
                let $a = p.read();
                let $b = v;
                p.write($e);
                $a
            }
        }
    )+};
}

fetch_op! {
    __atomic_fetch_add_8 => |a, b| a.wrapping_add(b);
    __atomic_fetch_sub_8 => |a, b| a.wrapping_sub(b);
    __atomic_fetch_and_8 => |a, b| a & b;
    __atomic_fetch_or_8 => |a, b| a | b;
    __atomic_fetch_xor_8 => |a, b| a ^ b;
}

/// The heap: free chunks, address-ordered, each `[size | next]` at its
/// start; a used chunk keeps its size in the word before the payload.
struct Heap {
    free: Global<*mut Chunk>,
}

#[repr(C)]
struct Chunk {
    size: usize,
    next: *mut Chunk,
}

/// The size word before a payload; chunks are multiples of this.
const HDR: usize = 16;
/// The smallest chunk: header and a free-list link.
const MIN: usize = 32;
/// What the heap asks the board for at least.
const GROW: usize = 1 << 20;

static HEAP: Heap = Heap {
    free: Global::new(ptr::null_mut()),
};

// Chunks are `HDR`-aligned.
#[allow(clippy::cast_ptr_alignment)]
impl Heap {
    /// Put the chunk `[at, at + size)` on the free list, merged with its
    /// free neighbours.
    ///
    /// # Safety
    /// The range is heap memory nobody uses.
    unsafe fn release(&self, at: *mut u8, size: usize) {
        // SAFETY: one thread; the list links free chunks only.
        unsafe {
            let head = &mut *self.free.0.get();
            let c = at.cast::<Chunk>();
            let mut prev: *mut Chunk = ptr::null_mut();
            let mut next = *head;
            while !next.is_null() && next < c {
                prev = next;
                next = (*next).next;
            }
            (*c).size = size;
            (*c).next = next;
            if !next.is_null() && at.add(size) == next.cast() {
                (*c).size += (*next).size;
                (*c).next = (*next).next;
            }
            if prev.is_null() {
                *head = c;
            } else if prev.cast::<u8>().add((*prev).size) == at {
                (*prev).size += (*c).size;
                (*prev).next = (*c).next;
            } else {
                (*prev).next = c;
            }
        }
    }

    /// A payload of `size` bytes aligned to `align` (a power of two, at
    /// least 16), or null.
    fn alloc(&self, size: usize, align: usize) -> *mut u8 {
        let Some(need) = size.max(MIN - HDR).checked_add(HDR) else {
            return ptr::null_mut();
        };
        let need = need.next_multiple_of(HDR);
        for _ in 0..2 {
            // SAFETY: one thread; the list links free chunks only.
            unsafe {
                let head = &mut *self.free.0.get();
                let mut prev: *mut Chunk = ptr::null_mut();
                let mut c = *head;
                while !c.is_null() {
                    let start = c as usize;
                    let end = start + (*c).size;
                    // The payload, its chunk starting a header before it,
                    // with room for a free chunk in any gap in front.
                    let mut p = (start + HDR).next_multiple_of(align);
                    if p - HDR != start && p - HDR - start < MIN {
                        p = (start + MIN + HDR).next_multiple_of(align);
                    }
                    if p - HDR + need <= end {
                        let next = (*c).next;
                        if prev.is_null() {
                            *head = next;
                        } else {
                            (*prev).next = next;
                        }
                        let at = p - HDR;
                        let used = if end - (at + need) < MIN {
                            end - at
                        } else {
                            need
                        };
                        if at != start {
                            self.release(start as *mut u8, at - start);
                        }
                        if at + used != end {
                            self.release((at + used) as *mut u8, end - at - used);
                        }
                        (at as *mut usize).write(used);
                        return p as *mut u8;
                    }
                    prev = c;
                    c = (*c).next;
                }
            }
            // Room for the worst alignment gap too.
            let Some(want) = need.checked_add(align + MIN) else {
                return ptr::null_mut();
            };
            let want = want.max(GROW).next_multiple_of(HDR);
            // SAFETY: the board's allocator.
            let m = unsafe { nabi_bare_morecore(want + HDR) };
            if m.is_null() {
                return ptr::null_mut();
            }
            let at = (m as usize).next_multiple_of(HDR) as *mut u8;
            // SAFETY: fresh memory from the board.
            unsafe { self.release(at, want) };
        }
        ptr::null_mut()
    }

    /// # Safety
    /// `p` is null or a live payload of this heap.
    unsafe fn free(&self, p: *mut u8) {
        if p.is_null() {
            return;
        }
        // SAFETY: caller contract: the size word precedes the payload.
        unsafe {
            let at = p.sub(HDR);
            let size = at.cast::<usize>().read();
            self.release(at, size);
        }
    }

    /// # Safety
    /// As C's `realloc`.
    unsafe fn realloc(&self, p: *mut u8, size: usize) -> *mut u8 {
        if p.is_null() {
            return self.alloc(size, 16);
        }
        // SAFETY: caller contract.
        let have = unsafe { p.sub(HDR).cast::<usize>().read() } - HDR;
        if size <= have {
            return p;
        }
        let q = self.alloc(size, 16);
        if !q.is_null() {
            // SAFETY: both live, `have` bytes fit both.
            unsafe {
                ptr::copy_nonoverlapping(p, q, have);
                self.free(p);
            }
        }
        q
    }
}

/// The calling thread's `errno`: nothing here sets one.
pub const fn errno() -> i32 {
    0
}

/// Write `parts` to `fd`, finishing short writes: 0 or `-errno`.
pub fn write_all(fd: i32, parts: &[&[u8]]) -> i32 {
    for p in parts {
        let mut rest = *p;
        while !rest.is_empty() {
            // SAFETY: `rest` is a live slice.
            let n = unsafe { nabi_bare_write(fd, rest.as_ptr(), rest.len()) };
            if n <= 0 {
                return if n < 0 { n as i32 } else { -libc::EBADF };
            }
            rest = &rest[n as usize..];
        }
    }
    0
}

/// No stdin: always at its end.
///
/// # Safety
/// None; the signature is the other targets'.
pub const unsafe fn read_stdin(_dst: *mut u8, _len: usize) -> isize {
    0
}

/// No terminal.
pub const fn is_terminal(_fd: i32) -> bool {
    false
}

static AT_EXIT: Global<[Option<extern "C" fn()>; 8]> = Global::new([None; 8]);

/// Run `f` at [`exit`], last registered first.
pub fn atexit(f: extern "C" fn()) {
    // SAFETY: one thread.
    let fs = unsafe { &mut *AT_EXIT.0.get() };
    if let Some(slot) = fs.iter_mut().find(|s| s.is_none()) {
        *slot = Some(f);
    }
}

/// Run the [`atexit`] functions, then end with `code`.
pub fn exit(code: i32) -> ! {
    // SAFETY: one thread.
    let fs = unsafe { &mut *AT_EXIT.0.get() };
    for f in fs.iter_mut().rev() {
        if let Some(f) = f.take() {
            f();
        }
    }
    // SAFETY: the board's exit.
    unsafe { nabi_bare_exit(code) }
}

/// One thread: a lock is never contended.
pub struct Lock;

#[allow(clippy::missing_const_for_fn)]
impl Lock {
    pub const fn new() -> Self {
        Self
    }

    pub fn lock(&self) {}

    pub fn try_lock(&self) -> bool {
        true
    }

    pub fn unlock(&self) {}
}

/// One thread: nobody else can signal, so an untimed wait never ends.
pub struct Cond;

impl Cond {
    pub const fn new() -> Self {
        Self
    }

    pub fn wait(&self, _lock: &Lock) {
        crate::fatal(b"fatal error: deadlock: the only thread waits for another");
    }

    pub fn wait_timeout(&self, _lock: &Lock, ns: u64) {
        sleep_ns(ns);
    }

    #[allow(clippy::missing_const_for_fn)]
    pub fn broadcast(&self) {}
}

fn clock(realtime: i32) -> i64 {
    // SAFETY: the board's clock.
    unsafe { nabi_bare_clock_ns(realtime) }
}

/// Monotonic nanoseconds.
pub fn monotonic_ns() -> i64 {
    clock(0)
}

/// Wall clock, nanoseconds since the Unix epoch.
pub fn realtime_ns() -> i64 {
    clock(1)
}

/// Nanoseconds since boot: the monotonic clock.
pub fn boot_ns() -> i64 {
    clock(0)
}

/// CPU time of the only thread: the monotonic clock.
pub fn thread_cpu_ns() -> i64 {
    clock(0)
}

/// One thread: nobody else to run.
pub fn yield_now() {
    core::hint::spin_loop();
}

/// Spin `ns` nanoseconds on the monotonic clock.
pub fn sleep_ns(ns: u64) {
    let end = monotonic_ns().saturating_add(ns.min(i64::MAX as u64) as i64);
    while monotonic_ns() < end {
        core::hint::spin_loop();
    }
}

/// No pages to map: a zeroed allocation.
pub fn map(len: usize, align: usize) -> *mut u8 {
    let p = alloc_aligned(len, align);
    if !p.is_null() {
        // SAFETY: a fresh block of `len` bytes.
        unsafe { ptr::write_bytes(p, 0, len) };
    }
    p
}

/// # Safety
/// `p` must be a [`map`] result nobody uses any more.
pub unsafe fn unmap(p: *mut u8, _len: usize) {
    // SAFETY: caller contract.
    unsafe { free_aligned(p) };
}

/// Pages cannot be returned: zero the range.
///
/// # Safety
/// The range must be allocated and hold nothing anyone still uses.
pub const unsafe fn reset(p: *mut u8, len: usize) -> bool {
    // SAFETY: caller contract.
    unsafe { ptr::write_bytes(p, 0, len) };
    true
}

/// `size` bytes aligned to `align` (a power of two), freed with
/// [`free_aligned`]: null on failure.
pub fn alloc_aligned(size: usize, align: usize) -> *mut u8 {
    HEAP.alloc(size, align.max(16))
}

/// # Safety
/// `p` must be null or from [`alloc_aligned`].
pub unsafe fn free_aligned(p: *mut u8) {
    // SAFETY: caller contract.
    unsafe { HEAP.free(p) };
}

/// No threads.
pub const fn spawn_detached(_f: extern "C" fn()) -> bool {
    false
}

/// Only reached by a thread the runtime has halted: the only one, which
/// ends the program.
pub fn park_forever() -> ! {
    crate::fatal(b"fatal error: deadlock: the only thread waits for another")
}
