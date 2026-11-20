//! Windows stand-in for fork: before a cell, with runtime threads quiesced
//! and the rest suspended, copy the session's writable memory (GC and
//! malloc heaps, statics, TLS blocks, runtime and loader state, sealed image
//! sections: `sealed_part`) and the other threads' registers and stacks. On
//! failure: end threads started since, put the copy back, decommit memory
//! committed since, and resume the main thread at its recovery point. A
//! thread waiting where it was (an idle worker) gets its state back; a
//! runtime thread that moved on is ended and the runtime told
//! (`nabi_session_restored`); others go on. Not saved: thread stacks and
//! TEBs, cell code, this bookkeeping.
//!
//! Copy, not copy-on-write: the kernel writes into buffers (`ReadFile`,
//! `recv`) without faulting, so a protected page would fail the call. The
//! runtime's own memory is write-watched (`nabi_watch_writes`) against a
//! shadow copy, so a checkpoint copies only pages written since the last
//! one and a rollback only those written since the checkpoint (everything
//! when `nabi_page_epoch` moved). Other writable memory is copied whole.

use crate::repl::{
    Buf, CTL, Global, QUIESCE_CANCEL, QUIESCING, STDIN, WHY, clear_shown, cstr, ctl_print,
    ctl_write, link, next_cell, quiesce, read_command, report, set_why, shown, why,
};
use core::ffi::c_void;
use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use nabir_common::sys::{nabi_page_epoch, nabi_watch_writes};
use nabir_common::win::{
    self, BOOL, CloseHandle, DWORD, EXCEPTION_POINTERS, GetCurrentProcessId, GetCurrentThreadId,
    GetProcAddress, HANDLE, MEM_COMMIT, MEM_DECOMMIT, MEM_RELEASE, MEM_RESERVE, MEM_WRITE_WATCH,
    MEMORY_BASIC_INFORMATION, PAGE_READWRITE, VirtualAlloc, VirtualFree, VirtualQuery,
    WaitForSingleObject,
};

unsafe extern "system" {
    pub(crate) fn CreateFileA(
        name: *const u8,
        access: DWORD,
        share: DWORD,
        sa: *mut c_void,
        disposition: DWORD,
        flags: DWORD,
        template: HANDLE,
    ) -> HANDLE;
    pub(crate) fn SetStdHandle(which: DWORD, h: HANDLE) -> BOOL;
    pub(crate) fn FlushInstructionCache(process: HANDLE, at: *const c_void, n: usize) -> BOOL;
    pub(crate) fn RtlAddFunctionTable(table: *mut c_void, count: DWORD, base: u64) -> u8;
    fn GetModuleHandleA(name: *const u8) -> HANDLE;
    fn LoadLibraryA(name: *const u8) -> HANDLE;
    fn SetUnhandledExceptionFilter(
        f: Option<unsafe extern "system" fn(*mut EXCEPTION_POINTERS) -> i32>,
    ) -> *mut c_void;
    fn RtlCaptureContext(ctx: *mut Context);
    fn RtlRestoreContext(ctx: *mut Context, record: *mut c_void) -> !;
    fn VirtualProtect(at: *mut c_void, n: usize, protect: DWORD, old: *mut DWORD) -> BOOL;
    fn NtGetNextThread(
        process: HANDLE,
        thread: HANDLE,
        access: DWORD,
        attributes: DWORD,
        flags: DWORD,
        next: *mut HANDLE,
    ) -> i32;
    fn NtQueryInformationThread(
        h: HANDLE,
        class: i32,
        info: *mut c_void,
        n: DWORD,
        ret: *mut DWORD,
    ) -> i32;
    fn GetThreadId(h: HANDLE) -> DWORD;
    fn SuspendThread(h: HANDLE) -> DWORD;
    fn ResumeThread(h: HANDLE) -> DWORD;
    fn TerminateThread(h: HANDLE, code: DWORD) -> BOOL;
    fn GetThreadContext(h: HANDLE, ctx: *mut Context) -> BOOL;
    fn SetThreadContext(h: HANDLE, ctx: *const Context) -> BOOL;
    fn ExitProcess(code: u32) -> !;
    fn GetWriteWatch(
        flags: DWORD,
        base: *mut c_void,
        size: usize,
        addrs: *mut *mut c_void,
        count: *mut usize,
        granularity: *mut DWORD,
    ) -> u32;
    fn ResetWriteWatch(base: *mut c_void, size: usize) -> u32;
}

unsafe extern "C" {
    #[link_name = "_putenv_s"]
    fn putenv_s(name: *const u8, value: *const u8) -> i32;
    #[link_name = "_resetstkoflw"]
    fn resetstkoflw() -> i32;
    /// The TLS directory nabic's startup object defines.
    #[link_name = "_tls_used"]
    static TLS_USED: TlsDirectory;
}

#[repr(C)]
struct TlsDirectory {
    start: *mut u8,
    end: *mut u8,
    index: *mut c_void,
    callbacks: *mut c_void,
    zero_fill: u32,
    flags: u32,
}

/// The modules a Windows program links (nabic's import libraries), the
/// UCRT first as there: `ntdll` repeats some C functions.
const MODULES: [&core::ffi::CStr; 11] = [
    c"ucrtbase.dll",
    c"kernel32.dll",
    c"api-ms-win-core-synch-l1-2-0.dll",
    c"ws2_32.dll",
    c"advapi32.dll",
    c"bcrypt.dll",
    c"userenv.dll",
    c"shell32.dll",
    c"ole32.dll",
    c"user32.dll",
    c"ntdll.dll",
];

/// The DLLs `lib` loaded (native packages' runtime files).
static SHARED: Global<([HANDLE; 64], usize)> = Global::new(([0; 64], 0));

/// The host's exports (it exports everything), else loaded DLLs', else the
/// modules'. Cells reach the safepoint slow path and the return from
/// blocking calls through the host, which ends interrupted cells there
/// (`host_yield`).
///
/// # Safety
/// `name` is a C string.
pub(crate) unsafe fn host_symbol(name: *const u8) -> *mut c_void {
    // SAFETY: caller contract; plain OS calls.
    unsafe {
        match cstr(name) {
            b"nabi_runtime_yield" => return host_yield as *const () as *mut c_void,
            b"nabi_leave_native" => return host_leave_native as *const () as *mut c_void,
            _ => {}
        }
        let mut a = GetProcAddress(GetModuleHandleA(ptr::null()), name);
        let shared = SHARED.get();
        for &m in &(*shared).0[..(*shared).1] {
            if !a.is_null() {
                break;
            }
            a = GetProcAddress(m, name);
        }
        for m in MODULES {
            if !a.is_null() {
                break;
            }
            let m = LoadLibraryA(m.as_ptr().cast());
            if m != 0 {
                a = GetProcAddress(m, name);
            }
        }
        a
    }
}

/// A thread-local's value for a cell: its offset in the thread's TLS
/// block (what SECREL gives), the same in every thread.
///
/// # Safety
/// `name` is a C string.
pub(crate) unsafe fn host_tls(name: *const u8) -> *mut c_void {
    // SAFETY: caller contract.
    let a = unsafe { host_symbol(name) };
    if a.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: the loader's TLS directory.
    let start = unsafe { TLS_USED.start };
    (a as usize).wrapping_sub(start as usize) as *mut c_void
}

/// A shared library, loaded by the system. true: it failed (reported).
///
/// # Safety
/// `path` is a C string.
pub(crate) unsafe fn load_shared(path: *const u8) -> bool {
    // SAFETY: the host's thread only; plain OS call.
    unsafe {
        let s = SHARED.get();
        let m = if (*s).1 < (*s).0.len() {
            LoadLibraryA(path)
        } else {
            0
        };
        if m == 0 {
            report!("error: cannot load {}\n", crate::repl::Text(cstr(path)));
            return true;
        }
        (*s).0[(*s).1] = m;
        (*s).1 += 1;
        false
    }
}

const MEM_FREE: DWORD = 0x1_0000;
const MEM_PRIVATE: DWORD = 0x2_0000;
const MEM_IMAGE: DWORD = 0x100_0000;
const PAGE_NOACCESS: DWORD = 1;
const PAGE_READONLY: DWORD = 2;
const PAGE_WRITECOPY: DWORD = 8;
const PAGE_GUARD: DWORD = 0x100;
const THREADS: usize = 256;
const PAGE: usize = 4096;
const SHADOWS: usize = 1024;
const DIRTY: usize = 65536;
const BOOK_SIZE: usize = 16 << 20;
/// The bit of the main thread's yield flag that interrupts the cell (the
/// runtime raises and clears only bit 0).
const INTERRUPT: u8 = 2;

/// A region of the address space at the checkpoint; `copy`: where its
/// bytes are saved (`usize::MAX`: not saved).
#[derive(Clone, Copy)]
struct Region {
    base: *mut u8,
    alloc: *mut u8,
    size: usize,
    copy: usize,
    state: DWORD,
    kind: DWORD,
    protect: DWORD,
    /// Write-watched: saved in its allocation's shadow, not the copy.
    watched: bool,
}

/// A write-watched allocation's shadow: its saved bytes at the same
/// offsets (committed as its regions are).
#[derive(Clone, Copy)]
struct Shadow {
    alloc: *mut u8,
    copy: *mut u8,
    size: usize,
    seen: bool,
}

/// A CONTEXT (x64: 1232 bytes, arm64: 912) and what is read of it.
#[repr(C, align(16))]
struct Context([u8; 1232]);

#[cfg(target_arch = "x86_64")]
mod ctx {
    pub const FLAGS: usize = 0x30;
    pub const SP: usize = 0x98;
    pub const PC: usize = 0xf8;
    pub const FULL: u32 = 0x10_000b;
    pub const CONTROL: u32 = 0x10_0001;
    /// Rbx, Rbp, Rsi, Rdi, R12-R15.
    pub const KEPT: &[usize] = &[0x90, 0xa0, 0xa8, 0xb0, 0xd8, 0xe0, 0xe8, 0xf0];
}

#[cfg(target_arch = "aarch64")]
mod ctx {
    pub const FLAGS: usize = 0;
    pub const SP: usize = 0x100;
    pub const PC: usize = 0x108;
    pub const FULL: u32 = 0x40_0007;
    pub const CONTROL: u32 = 0x40_0001;
    /// X19-X28, Fp.
    pub const KEPT: &[usize] = &[
        0xa0, 0xa8, 0xb0, 0xb8, 0xc0, 0xc8, 0xd0, 0xd8, 0xe0, 0xe8, 0xf0,
    ];
}

impl Context {
    fn word(&self, off: usize) -> u64 {
        u64::from_le_bytes(self.0[off..off + 8].try_into().unwrap_or_default())
    }

    fn want(&mut self, flags: u32) {
        self.0[ctx::FLAGS..ctx::FLAGS + 4].copy_from_slice(&flags.to_le_bytes());
    }
}

/// A thread from before the cell: its registers, and its stack from the
/// stack pointer up to the base (saved at `copy` in the copy).
struct Before {
    id: DWORD,
    nabi: bool,
    sp: *mut u8,
    base: *mut u8,
    copy: usize,
    ctx: Context,
}

/// Bookkeeping that outlives a rollback: its own allocation, not saved.
/// The regions follow it.
struct Book {
    /// The recovery point, and a CONTEXT to read suspended threads into.
    recover: Context,
    probe: Context,
    running: AtomicBool,
    ended: AtomicBool,
    overflowed: AtomicBool,
    /// `why` the cell failed, which a rollback would undo.
    why: [u8; WHY],
    nwhy: usize,
    /// The threads currently suspended.
    handle: [HANDLE; THREADS],
    id: [DWORD; THREADS],
    nthreads: usize,
    /// The other threads at the checkpoint, and the runtime's (OS ids).
    before: [Before; THREADS],
    nbefore: usize,
    nabi: [u64; THREADS],
    nnabi: usize,
    /// The runtime's threads the last rollback ended or found ended.
    lost: [u64; THREADS],
    nlost: usize,
    /// Idle pool workers kept suspended while the cell runs (`park`).
    parked: [HANDLE; THREADS],
    nparked: usize,
    /// Allocations not saved or freed: stacks, TEBs, the bookkeeping.
    excl: [*mut u8; 2 * THREADS + 4],
    nexcl: usize,
    copy: *mut u8,
    copy_cap: usize,
    shadows: [Shadow; SHADOWS],
    nshadows: usize,
    /// `nabi_page_epoch` at the checkpoint; written pages found.
    epoch: u64,
    dirty: [*mut c_void; DIRTY],
    nregions: usize,
    cap: usize,
}

static BOOK: Global<*mut Book> = Global::new(ptr::null_mut());

fn book() -> *mut Book {
    // SAFETY: set once at start.
    unsafe { *BOOK.get() }
}

/// The checkpoint's region `i`.
///
/// # Safety
/// `i` is below the book's capacity.
unsafe fn region(i: usize) -> *mut Region {
    // SAFETY: the regions follow the book in its allocation.
    unsafe { book().add(1).cast::<Region>().add(i) }
}

/// The main thread, ntdll's image (where the system thread pool's workers
/// start), the syscall stub idle pool workers wait in, and the main
/// thread's yield flag.
struct Host {
    main_thread: DWORD,
    ntdll: *const u8,
    pool_wait: *const u8,
    main_yield: *const AtomicU8,
}

static HOST: Global<Host> = Global::new(Host {
    main_thread: 0,
    ntdll: ptr::null(),
    pool_wait: ptr::null(),
    main_yield: ptr::null(),
});

fn host() -> &'static Host {
    // SAFETY: set once at start, read after.
    unsafe { &*HOST.get() }
}

fn main_yield() -> &'static AtomicU8 {
    // SAFETY: the main thread's TLS slot, which lives as long as the host.
    unsafe { &*host().main_yield }
}

/// Suspend every other thread, including new ones, into the book's
/// `handle` / `id`. Takes no lock a stopped thread may hold (no heap, no
/// stdio).
///
/// # Safety
/// The main thread, with the book set up.
unsafe fn stop_others() {
    let b = book();
    // SAFETY: caller contract; plain OS calls.
    unsafe {
        (*b).nthreads = 0;
        let mut more = true;
        while more {
            more = false;
            let mut cur: HANDLE = 0;
            let mut next: HANDLE = 0;
            let mut kept = false;
            while NtGetNextThread(-1, cur, 0x1f_ffff, 0, 0, &raw mut next) == 0 {
                if cur != 0 && !kept {
                    CloseHandle(cur);
                }
                cur = next;
                kept = false;
                let id = GetThreadId(cur);
                let n = (*b).nthreads;
                if id == host().main_thread || (&(*b).id)[..n].contains(&id) || n == THREADS {
                    continue;
                }
                // It has ended.
                if WaitForSingleObject(cur, 0) == 0 {
                    continue;
                }
                SuspendThread(cur);
                // Suspension completes by the time a context can be read.
                (*b).probe.want(ctx::CONTROL);
                GetThreadContext(cur, &raw mut (*b).probe);
                (*b).handle[n] = cur;
                (*b).id[n] = id;
                (*b).nthreads = n + 1;
                kept = true;
                more = true;
            }
            if cur != 0 && !kept {
                CloseHandle(cur);
            }
        }
    }
}

/// # Safety
/// As `stop_others`.
unsafe fn resume_others() {
    let b = book();
    // SAFETY: caller contract.
    unsafe {
        for i in 0..(*b).nthreads {
            let h = (*b).handle[i];
            if h != 0 {
                ResumeThread(h);
                CloseHandle(h);
            }
        }
        (*b).nthreads = 0;
    }
}

fn query(at: *const u8, m: &mut MEMORY_BASIC_INFORMATION) -> bool {
    let n = size_of::<MEMORY_BASIC_INFORMATION>();
    // SAFETY: `m` is writable.
    unsafe { VirtualQuery(at.cast(), m, n) == n }
}

const fn empty_info() -> MEMORY_BASIC_INFORMATION {
    // SAFETY: plain data; all zeroes is valid.
    unsafe { core::mem::zeroed() }
}

/// # Safety
/// As `stop_others`.
unsafe fn exclude(at: *const u8) {
    let mut m = empty_info();
    if !at.is_null() && query(at, &mut m) {
        let b = book();
        // SAFETY: caller contract.
        unsafe {
            if (*b).nexcl < (*b).excl.len() {
                (*b).excl[(*b).nexcl] = m.AllocationBase.cast();
                (*b).nexcl += 1;
            }
        }
    }
}

fn excluded(alloc: *const u8) -> bool {
    let b = book();
    // SAFETY: the book's own list.
    unsafe { (&(*b).excl)[..(*b).nexcl].contains(&alloc.cast_mut()) }
}

/// A thread's TEB (null: gone).
fn teb_of(h: HANDLE) -> *mut u8 {
    #[repr(C)]
    struct Basic {
        status: i32,
        teb: *mut u8,
        rest: [*mut c_void; 4],
    }
    let mut basic = Basic {
        status: 0,
        teb: ptr::null_mut(),
        rest: [ptr::null_mut(); 4],
    };
    let n = size_of::<Basic>() as DWORD;
    // SAFETY: `basic` is writable; class 0 is ThreadBasicInformation.
    let r = unsafe { NtQueryInformationThread(h, 0, (&raw mut basic).cast(), n, ptr::null_mut()) };
    if r == 0 { basic.teb } else { ptr::null_mut() }
}

/// A thread's TEB and stack (its `NT_TIB`'s `StackLimit`).
///
/// # Safety
/// As `stop_others`.
unsafe fn exclude_thread(h: HANDLE) {
    let teb = teb_of(h);
    if teb.is_null() {
        return;
    }
    // SAFETY: caller contract; a live TEB.
    unsafe {
        exclude(teb);
        let limit: *mut u8 = ptr::read_unaligned(teb.add(0x10).cast::<*mut u8>());
        exclude(limit);
    }
}

/// # Safety
/// As `stop_others`.
unsafe fn exclusions() {
    let b = book();
    // SAFETY: caller contract.
    unsafe {
        (*b).nexcl = 0;
        exclude(b.cast());
        exclude((*b).copy);
        for i in 0..(*b).nshadows {
            exclude((*b).shadows[i].copy);
        }
        exclude_thread(-2);
        for i in 0..(*b).nthreads {
            if (*b).handle[i] != 0 {
                exclude_thread((*b).handle[i]);
            }
        }
    }
}

fn saved(m: &MEMORY_BASIC_INFORMATION) -> bool {
    let p = m.Protect & 0xff;
    m.State == MEM_COMMIT
        && (m.Type == MEM_PRIVATE || m.Type == MEM_IMAGE)
        && (p == PAGE_READWRITE || p == PAGE_WRITECOPY)
        && m.Protect & PAGE_GUARD == 0
        && !excluded(m.AllocationBase.cast())
}

/// The part of region `m` in image section `k` that is read-only now but
/// marked writable: ntdll's `.mrdata`, unprotected only while written,
/// holds list heads (function tables, loader) whose nodes are heap memory,
/// so it is saved with the heap.
fn sealed_part(m: &MEMORY_BASIC_INFORMATION, k: u32) -> Option<(*mut u8, *mut u8)> {
    let img = m.AllocationBase.cast::<u8>();
    // SAFETY: a mapped image's headers.
    unsafe {
        let rd32 = |p: *const u8| ptr::read_unaligned(p.cast::<u32>());
        let rd16 = |p: *const u8| ptr::read_unaligned(p.cast::<u16>());
        let pe = rd32(img.add(0x3c)) as usize;
        let nsect = rd16(img.add(pe + 6));
        let optsize = rd16(img.add(pe + 20)) as usize;
        if k >= u32::from(nsect) {
            return None;
        }
        let s = img.add(pe + 24 + optsize + 40 * k as usize);
        let vsize = rd32(s.add(8)) as usize;
        let va = rd32(s.add(12)) as usize;
        let flags = rd32(s.add(36));
        // IMAGE_SCN_MEM_WRITE
        if flags & 0x8000_0000 == 0 {
            return None;
        }
        let a = img.add(va);
        let b = a.add((vsize + PAGE - 1) & !(PAGE - 1));
        let base = m.BaseAddress.cast::<u8>();
        let end = base.add(m.RegionSize);
        let lo = if a > base { a } else { base };
        let hi = if b < end { b } else { end };
        (lo < hi).then_some((lo, hi))
    }
}

fn shadow_of(alloc: *const u8) -> *mut Shadow {
    let b = book();
    // SAFETY: the book's own list.
    unsafe {
        for i in 0..(*b).nshadows {
            if (*b).shadows[i].alloc.cast_const() == alloc {
                return &raw mut (*b).shadows[i];
            }
        }
    }
    ptr::null_mut()
}

/// The pages of `base..base + size` (at most `DIRTY`) written since the
/// last reset, into the book's `dirty`; None: not write-watched.
fn dirty_pages(base: *mut u8, size: usize, reset: bool) -> Option<usize> {
    let b = book();
    let mut n = DIRTY;
    let mut granularity = 0;
    // SAFETY: the book's buffer holds `DIRTY` addresses.
    let r = unsafe {
        GetWriteWatch(
            DWORD::from(reset),
            base.cast(),
            size,
            (&raw mut (*b).dirty).cast(),
            &raw mut n,
            &raw mut granularity,
        )
    };
    (r == 0).then_some(n)
}

/// The checkpoint's extent of allocation `alloc`.
fn extent_of(alloc: *const u8) -> usize {
    let mut extent = 0;
    // SAFETY: the book's regions.
    unsafe {
        for i in 0..(*book()).nregions {
            let q = &*region(i);
            if q.alloc.cast_const() == alloc {
                extent = extent.max(q.base as usize + q.size - alloc as usize);
            }
        }
    }
    extent
}

/// The chunks of `size` bytes a write watch query covers.
fn chunks(size: usize) -> impl Iterator<Item = (usize, usize)> {
    (0..size)
        .step_by(DIRTY * PAGE)
        .map(move |off| (off, (size - off).min(DIRTY * PAGE)))
}

/// Bring watched regions' shadows up to date (pages written since the last
/// checkpoint; all of a new region, or all when `nabi_page_epoch` moved),
/// resetting the watch. false: no room.
///
/// # Safety
/// As `stop_others`, the world stopped.
unsafe fn save_watched() -> bool {
    let b = book();
    let epoch = nabi_page_epoch.load(Ordering::SeqCst);
    // SAFETY: caller contract; regions and shadows are the book's.
    unsafe {
        for i in 0..(*b).nshadows {
            (*b).shadows[i].seen = false;
        }
        for i in 0..(*b).nregions {
            let r = *region(i);
            if !r.watched {
                continue;
            }
            let extent = extent_of(r.alloc);
            let mut sh = shadow_of(r.alloc);
            let mut whole = epoch != (*b).epoch;
            if !sh.is_null() && (*sh).size < extent {
                VirtualFree((*sh).copy.cast(), 0, MEM_RELEASE);
                (*b).nshadows -= 1;
                *sh = (*b).shadows[(*b).nshadows];
                sh = ptr::null_mut();
            }
            if sh.is_null() {
                let c = if (*b).nshadows < SHADOWS {
                    VirtualAlloc(ptr::null_mut(), extent, MEM_RESERVE, PAGE_NOACCESS).cast::<u8>()
                } else {
                    ptr::null_mut()
                };
                if c.is_null() {
                    return false;
                }
                sh = &raw mut (*b).shadows[(*b).nshadows];
                (*b).nshadows += 1;
                *sh = Shadow {
                    alloc: r.alloc,
                    copy: c,
                    size: extent,
                    seen: false,
                };
                whole = true;
            }
            (*sh).seen = true;
            let copy = (*sh).copy.add(r.base as usize - r.alloc as usize);
            if VirtualAlloc(copy.cast(), r.size, MEM_COMMIT, PAGE_READWRITE).is_null() {
                return false;
            }
            if whole {
                ptr::copy_nonoverlapping(r.base, copy, r.size);
                ResetWriteWatch(r.base.cast(), r.size);
                continue;
            }
            for (off, len) in chunks(r.size) {
                // The walk found it watched.
                let Some(n) = dirty_pages(r.base.add(off), len, true) else {
                    return false;
                };
                for k in 0..n {
                    let page = (*b).dirty[k].cast::<u8>();
                    ptr::copy_nonoverlapping(page, copy.add(page as usize - r.base as usize), PAGE);
                }
            }
        }
        // Allocations that are gone.
        let mut i = 0;
        while i < (*b).nshadows {
            if (*b).shadows[i].seen {
                i += 1;
                continue;
            }
            VirtualFree((*b).shadows[i].copy.cast(), 0, MEM_RELEASE);
            (*b).nshadows -= 1;
            (*b).shadows[i] = (*b).shadows[(*b).nshadows];
        }
        (*b).epoch = epoch;
    }
    true
}

/// Save the session before the cell. 1: saved; 0: no room; -1: Ctrl-C
/// while waiting for quiet, so the cell doesn't run.
///
/// # Safety
/// The main thread, between cells.
unsafe fn checkpoint() -> i32 {
    if !quiesce() {
        return -1;
    }
    let b = book();
    // SAFETY: caller contract; the world is stopped from here to the end.
    unsafe {
        (*b).nnabi = crate::session::nabi_world_threads((&raw mut (*b).nabi).cast(), THREADS);
        stop_others();
        (*b).nbefore = (*b).nthreads;
        for i in 0..(*b).nthreads {
            let t = &raw mut (*b).before[i];
            let teb = teb_of((*b).handle[i]);
            (*t).ctx.want(ctx::FULL);
            GetThreadContext((*b).handle[i], &raw mut (*t).ctx);
            (*t).id = (*b).id[i];
            (*t).nabi = (&(*b).nabi)[..(*b).nnabi].contains(&u64::from((*t).id));
            (*t).sp = (*t).ctx.word(ctx::SP) as *mut u8;
            (*t).base = if teb.is_null() {
                (*t).sp
            } else {
                ptr::read_unaligned(teb.add(0x8).cast::<*mut u8>())
            };
            if (*t).base < (*t).sp {
                (*t).base = (*t).sp;
            }
        }
        exclusions();
        let mut m = empty_info();
        let mut n = 0;
        let mut total = 0;
        let mut ok = true;
        let mut a: *const u8 = ptr::null();
        while ok && query(a, &mut m) {
            a = m.BaseAddress.cast::<u8>().wrapping_add(m.RegionSize);
            if m.State == MEM_FREE {
                continue;
            }
            if n == (*b).cap {
                ok = false;
            } else if m.State == MEM_COMMIT
                && m.Type == MEM_IMAGE
                && m.Protect & 0xff == PAGE_READONLY
            {
                for k in 0..96 {
                    if !ok {
                        break;
                    }
                    let Some((lo, hi)) = sealed_part(&m, k) else {
                        continue;
                    };
                    if n == (*b).cap {
                        ok = false;
                    } else {
                        let size = hi as usize - lo as usize;
                        *region(n) = Region {
                            base: lo,
                            alloc: m.AllocationBase.cast(),
                            size,
                            copy: total,
                            state: m.State,
                            kind: m.Type,
                            protect: m.Protect,
                            watched: false,
                        };
                        n += 1;
                        total += size;
                    }
                }
            } else {
                let keep = saved(&m);
                let watched = keep
                    && m.Type == MEM_PRIVATE
                    && dirty_pages(m.BaseAddress.cast(), PAGE, false).is_some();
                *region(n) = Region {
                    base: m.BaseAddress.cast(),
                    alloc: m.AllocationBase.cast(),
                    size: m.RegionSize,
                    copy: if keep && !watched { total } else { usize::MAX },
                    state: m.State,
                    kind: m.Type,
                    protect: m.Protect,
                    watched,
                };
                n += 1;
                if keep && !watched {
                    total += m.RegionSize;
                }
            }
        }
        (*b).nregions = n;
        for i in 0..(*b).nbefore {
            let t = &raw mut (*b).before[i];
            (*t).copy = total;
            total += (*t).base as usize - (*t).sp as usize;
        }
        if ok && total > (*b).copy_cap {
            if !(*b).copy.is_null() {
                VirtualFree((*b).copy.cast(), 0, MEM_RELEASE);
            }
            (*b).copy_cap = total + total / 4;
            (*b).copy = VirtualAlloc(
                ptr::null_mut(),
                (*b).copy_cap,
                MEM_COMMIT | MEM_RESERVE,
                PAGE_READWRITE,
            )
            .cast();
            if (*b).copy.is_null() {
                (*b).copy_cap = 0;
                ok = false;
            }
        }
        if ok {
            for i in 0..n {
                let r = &*region(i);
                if r.copy != usize::MAX {
                    ptr::copy_nonoverlapping(r.base, (*b).copy.add(r.copy), r.size);
                }
            }
            for i in 0..(*b).nbefore {
                let t = &(*b).before[i];
                ptr::copy_nonoverlapping(
                    t.sp,
                    (*b).copy.add(t.copy),
                    t.base as usize - t.sp as usize,
                );
            }
        }
        ok = ok && save_watched();
        park();
        resume_others();
        crate::session::nabi_session_resume();
        i32::from(ok)
    }
}

/// The checkpoint's region holding `p` (null: none, it was free).
fn region_at(p: *const u8) -> *mut Region {
    // SAFETY: the book's sorted regions.
    unsafe {
        let (mut lo, mut hi) = (0, (*book()).nregions);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let r = region(mid);
            if p < (*r).base.cast_const() {
                hi = mid;
            } else if p >= (*r).base.add((*r).size).cast_const() {
                lo = mid + 1;
            } else {
                return r;
            }
        }
    }
    ptr::null_mut()
}

fn lost() -> ! {
    let msg = b"error: the session cannot be restored\n";
    let mut put = 0;
    // SAFETY: plain OS calls; the process ends.
    unsafe {
        win::WriteFile(
            win::GetStdHandle(win::STD_ERROR_HANDLE),
            msg.as_ptr().cast(),
            msg.len() as DWORD,
            &raw mut put,
            ptr::null_mut(),
        );
        ExitProcess(1)
    }
}

/// Restore a saved region: recommit (re-reserving a released allocation),
/// reprotect, rewrite; a watched one from its shadow, pages written since
/// the checkpoint (`all`: every differing page).
///
/// # Safety
/// The world stopped, during a rollback.
unsafe fn put_back(r: &Region, mut all: bool) {
    let b = book();
    // SAFETY: caller contract; the region was the session's.
    unsafe {
        if r.kind == MEM_PRIVATE
            && VirtualAlloc(r.base.cast(), r.size, MEM_COMMIT, PAGE_READWRITE).is_null()
        {
            let watch = if r.watched { MEM_WRITE_WATCH } else { 0 };
            VirtualAlloc(
                r.alloc.cast(),
                extent_of(r.alloc),
                MEM_RESERVE | watch,
                PAGE_NOACCESS,
            );
            if VirtualAlloc(r.base.cast(), r.size, MEM_COMMIT, PAGE_READWRITE).is_null() {
                lost();
            }
            all = true;
        }
        let mut old = 0;
        let p = r.protect & 0xff;
        // A sealed section is written unprotected, then sealed again.
        let sealed = p != PAGE_READWRITE && p != PAGE_WRITECOPY;
        let now = if sealed { PAGE_WRITECOPY } else { r.protect };
        if VirtualProtect(r.base.cast(), r.size, now, &raw mut old) == 0 {
            lost();
        }
        let from = if r.watched {
            (*shadow_of(r.alloc))
                .copy
                .add(r.base as usize - r.alloc as usize)
        } else {
            (*b).copy.add(r.copy)
        };
        if r.watched && !all {
            for (off, len) in chunks(r.size) {
                let Some(n) = dirty_pages(r.base.add(off), len, false) else {
                    all = true;
                    continue;
                };
                for k in 0..n {
                    let page = (*b).dirty[k].cast::<u8>();
                    ptr::copy_nonoverlapping(from.add(page as usize - r.base as usize), page, PAGE);
                }
            }
        }
        // Pages the cell did not change are left alone (an image's stay
        // shared).
        if !r.watched || all {
            for off in (0..r.size).step_by(PAGE) {
                let (at, was) = (r.base.add(off), from.add(off));
                if core::slice::from_raw_parts(at, PAGE) != core::slice::from_raw_parts(was, PAGE) {
                    ptr::copy_nonoverlapping(was, at, PAGE);
                }
            }
        }
        if r.watched {
            ResetWriteWatch(r.base.cast(), r.size);
        }
        if sealed && VirtualProtect(r.base.cast(), r.size, r.protect, &raw mut old) == 0 {
            lost();
        }
    }
}

/// Private memory committed since the checkpoint: decommitted, or released
/// with its allocation when that is new.
///
/// # Safety
/// The world stopped, during a rollback.
unsafe fn free_new() {
    let mut m = empty_info();
    let mut a: *const u8 = ptr::null();
    // SAFETY: caller contract; memory the session made since.
    unsafe {
        while query(a, &mut m) {
            let base = m.BaseAddress.cast::<u8>();
            let alloc = m.AllocationBase.cast::<u8>();
            a = base.wrapping_add(m.RegionSize);
            if m.State != MEM_COMMIT || m.Type != MEM_PRIVATE || excluded(alloc) {
                continue;
            }
            let first = region_at(alloc);
            if first.is_null() || (*first).alloc != alloc {
                VirtualFree(alloc.cast(), 0, MEM_RELEASE);
                continue;
            }
            let end = base.add(m.RegionSize);
            let mut p = base;
            while p < end {
                let r = region_at(p);
                let to = if !r.is_null() && (*r).base.add((*r).size) < end {
                    (*r).base.add((*r).size)
                } else {
                    end
                };
                if r.is_null() || (*r).state != MEM_COMMIT {
                    VirtualFree(p.cast(), to as usize - p as usize, MEM_DECOMMIT);
                }
                p = to;
            }
        }
    }
}

/// End stopped thread `i`, started since or moved on.
///
/// # Safety
/// A rollback's stopped thread.
unsafe fn end_thread(i: usize) {
    let b = book();
    // SAFETY: caller contract.
    unsafe {
        TerminateThread((*b).handle[i], 1);
        WaitForSingleObject((*b).handle[i], 5000);
        CloseHandle((*b).handle[i]);
        (*b).handle[i] = 0;
    }
}

/// The thread from before the cell with id `id` (null: none).
fn before_of(id: DWORD) -> *mut Before {
    let b = book();
    // SAFETY: the book's own list.
    unsafe {
        for i in 0..(*b).nbefore {
            if (*b).before[i].id == id {
                return &raw mut (*b).before[i];
            }
        }
    }
    ptr::null_mut()
}

/// Is stopped thread `i` where it was at the checkpoint? Then its registers
/// and stack go back.
///
/// # Safety
/// A rollback's stopped thread from before the cell.
unsafe fn put_thread_back(i: usize, t: &Before) -> bool {
    let b = book();
    // SAFETY: caller contract; the thread is suspended.
    unsafe {
        let now = &raw mut (*b).probe;
        (*now).want(ctx::FULL);
        if GetThreadContext((*b).handle[i], now) == 0
            || (*now).word(ctx::PC) != t.ctx.word(ctx::PC)
            || (*now).word(ctx::SP) != t.ctx.word(ctx::SP)
        {
            return false;
        }
        let n = t.base as usize - t.sp as usize;
        let was = (*b).copy.add(t.copy);
        let same = core::slice::from_raw_parts(t.sp, n) == core::slice::from_raw_parts(was, n)
            && ctx::KEPT.iter().all(|&k| (*now).word(k) == t.ctx.word(k));
        if !same {
            ptr::copy_nonoverlapping(was, t.sp, n);
            SetThreadContext((*b).handle[i], &raw const t.ctx);
        }
        true
    }
}

/// Restore a gone runtime thread's stack (re-mapping freed memory) so what
/// restored memory links to there (a waiter entry) stays readable.
///
/// # Safety
/// During a rollback.
unsafe fn restore_stack(t: &Before) {
    let lo = (t.sp as usize & !(PAGE - 1)) as *mut c_void;
    let n = t.base as usize - lo as usize;
    if n == 0 {
        return;
    }
    // SAFETY: caller contract; the stack's own range.
    unsafe {
        if VirtualAlloc(lo, n, MEM_COMMIT, PAGE_READWRITE).is_null()
            && VirtualAlloc(lo, n, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE).is_null()
        {
            lost();
        }
        let len = t.base as usize - t.sp as usize;
        ptr::copy_nonoverlapping((*book()).copy.add(t.copy), t.sp, len);
    }
}

/// Is thread `i` a system thread-pool worker (starts in ntdll), which a
/// busy machine may start after the checkpoint? Ending one kills the
/// process (`STATUS_INVALID_THREAD`) and letting it run faults on the
/// restored pool state, so it stays suspended (one extra suspension;
/// `stop_others` finds it each time, so its stack is never put back).
fn pool_worker(i: usize) -> bool {
    let mut start: *mut c_void = ptr::null_mut();
    let mut m = empty_info();
    // SAFETY: the book's stopped thread; 9: ThreadQuerySetWin32StartAddress.
    unsafe {
        NtQueryInformationThread(
            (*book()).handle[i],
            9,
            (&raw mut start).cast(),
            size_of::<*mut c_void>() as DWORD,
            ptr::null_mut(),
        ) == 0
            && query(start.cast(), &mut m)
            && m.AllocationBase.cast_const().cast::<u8>() == host().ntdll
    }
}

/// At the checkpoint: idle pool workers (the loader's from process start,
/// the system pool's) stay suspended until the cell ends. One that woke
/// or timed out and exited during the cell would leave restored pool
/// memory pointing at a gone thread and stack; suspended, the pool makes
/// new workers for work the cell posts. Busy ones may hold a lock the
/// cell needs, so they run on.
///
/// # Safety
/// During a checkpoint.
unsafe fn park() {
    let b = book();
    let wait = host().pool_wait;
    // SAFETY: caller contract.
    unsafe {
        for i in 0..(*b).nthreads {
            let t = &(*b).before[i];
            let pc = t.ctx.word(ctx::PC) as *const u8;
            if t.nabi
                || wait.is_null()
                || pc < wait
                || pc >= wait.wrapping_add(32)
                || !pool_worker(i)
            {
                continue;
            }
            (*b).parked[(*b).nparked] = (*b).handle[i];
            (*b).nparked += 1;
            (*b).handle[i] = 0;
        }
    }
}

fn unpark() {
    let b = book();
    // SAFETY: the book's parked threads.
    unsafe {
        for i in 0..(*b).nparked {
            ResumeThread((*b).parked[i]);
            CloseHandle((*b).parked[i]);
        }
        (*b).nparked = 0;
    }
}

/// Back to the checkpoint: the threads started since end, and memory is
/// as it was then.
///
/// # Safety
/// The main thread, after a checkpoint.
unsafe fn rollback() {
    let b = book();
    // SAFETY: caller contract.
    unsafe {
        stop_others();
        // The I/O poller's requests in flight write into memory about to be
        // put back.
        crate::session::nabi_io_quiesce();
        (*b).nlost = 0;
        for i in 0..(*b).nthreads {
            let t = before_of((*b).id[i]);
            if t.is_null() {
                // Started since.
                if pool_worker(i) {
                    SuspendThread((*b).handle[i]);
                } else {
                    end_thread(i);
                }
            } else if !put_thread_back(i, &*t) && (*t).nabi {
                (*b).lost[(*b).nlost] = u64::from((*t).id);
                (*b).nlost += 1;
                end_thread(i);
            }
        }
        // A runtime thread from before that has ended since.
        for i in 0..(*b).nbefore {
            let t = &(*b).before[i];
            if t.nabi && !(&(*b).id)[..(*b).nthreads].contains(&t.id) {
                (*b).lost[(*b).nlost] = u64::from(t.id);
                (*b).nlost += 1;
            }
        }
        exclusions();
        // New memory first: what was released since may have been reused.
        free_new();
        // Read before the runtime's statics go back.
        let all = nabi_page_epoch.load(Ordering::SeqCst) != (*b).epoch;
        for i in 0..(*b).nregions {
            let r = *region(i);
            if r.copy != usize::MAX || r.watched {
                put_back(&r, all);
            }
        }
        for i in 0..(*b).nbefore {
            let t = &(*b).before[i];
            if (&(*b).lost)[..(*b).nlost].contains(&u64::from(t.id)) {
                restore_stack(t);
            }
        }
        resume_others();
    }
}

/// End the running cell: the session as before it, then back to the
/// recovery point (`run_cell`).
///
/// # Safety
/// The main thread, during a cell.
unsafe fn fail_cell() -> ! {
    let b = book();
    // SAFETY: caller contract; `recover` was captured by `run_cell`, whose
    // frame is still live below the cell.
    unsafe {
        (*b).running.store(false, Ordering::SeqCst);
        (*b).ended.store(true, Ordering::SeqCst);
        let w = why();
        (*b).why[..w.len()].copy_from_slice(w);
        (*b).nwhy = w.len();
        rollback();
        RtlRestoreContext(&raw mut (*b).recover, ptr::null_mut())
    }
}

fn on_cell_thread() -> bool {
    // SAFETY: the book is set before any handler is.
    unsafe {
        (*book()).running.load(Ordering::SeqCst) && GetCurrentThreadId() == host().main_thread
    }
}

/// The runtime is about to end the process (panic, uncaught error, `exit`,
/// stack overflow): on the cell's thread end only the cell, after the
/// stdout flush `exit` would do.
extern "C" fn on_exit_hook(status: i32) {
    if !on_cell_thread() {
        return;
    }
    if why().is_empty() && status == 101 {
        set_why(b"panic");
    }
    // SAFETY: the book is set.
    if !unsafe { (*book()).overflowed.load(Ordering::SeqCst) } {
        crate::stdio::nabi_stdio_flush();
    }
    // SAFETY: the cell's thread, during the cell.
    unsafe { fail_cell() }
}

/// A stack overflow on the cell's thread, noted before the runtime's
/// handler reports it (and ends the cell through `on_exit_hook`).
unsafe extern "system" fn on_overflow(p: *mut EXCEPTION_POINTERS) -> i32 {
    // SAFETY: the system's exception record.
    if on_cell_thread()
        && unsafe { (*(*p).ExceptionRecord).ExceptionCode } == win::EXCEPTION_STACK_OVERFLOW
    {
        // SAFETY: the book is set.
        unsafe { (*book()).overflowed.store(true, Ordering::SeqCst) };
    }
    win::EXCEPTION_CONTINUE_SEARCH
}

/// A fault nothing handled (unwinding through the cell's frames by its
/// unwind tables): on the cell's thread it ends the cell.
unsafe extern "system" fn on_fault(p: *mut EXCEPTION_POINTERS) -> i32 {
    if !on_cell_thread() {
        return win::EXCEPTION_CONTINUE_SEARCH;
    }
    // SAFETY: the system's exception record.
    let code = unsafe { (*(*p).ExceptionRecord).ExceptionCode };
    report!("error: the cell raised exception 0x{code:08x}\n");
    // SAFETY: the cell's thread, during the cell.
    unsafe { fail_cell() }
}

/// End the cell if it was interrupted and this is its thread.
fn interrupted() {
    if main_yield().load(Ordering::SeqCst) & INTERRUPT == 0
        // SAFETY: plain OS call.
        || unsafe { GetCurrentThreadId() } != host().main_thread
    {
        return;
    }
    set_why(b"interrupted");
    // SAFETY: the cell's thread, during the cell.
    unsafe { fail_cell() }
}

// The safepoint slow path and the way back from a blocking call that
// cells call: the runtime's, then the end of an interrupted cell.
extern "C" fn host_yield() {
    crate::nabi_runtime_yield();
    interrupted();
}

extern "C" fn host_leave_native() {
    crate::nabi_leave_native();
    interrupted();
}

/// Ctrl-Break / Ctrl-C on its own thread: the cell ends at its next poll or
/// return from a blocking call, both of which see the raised yield flag.
unsafe extern "system" fn on_break(event: DWORD) -> BOOL {
    // Not CTRL_C_EVENT / CTRL_BREAK_EVENT.
    if event > 1 {
        return 0;
    }
    if QUIESCING.load(Ordering::SeqCst) {
        QUIESCE_CANCEL.store(true, Ordering::SeqCst);
    }
    // SAFETY: the book is set before the handler.
    if unsafe { (*book()).running.load(Ordering::SeqCst) } {
        main_yield().fetch_or(INTERRUPT, Ordering::SeqCst);
    }
    1
}

/// Link and run the cell from a recovery point; false: it failed
/// (`fail_cell` came back here, with every register as it was at the
/// capture, so nothing after it but the `ended` check is shared by both
/// returns).
///
/// # Safety
/// The main thread, after a checkpoint.
#[inline(never)]
unsafe fn run_cell(obj: *const u8, n: usize) -> bool {
    // SAFETY: caller contract.
    unsafe {
        (*book()).ended.store(false, Ordering::SeqCst);
        RtlCaptureContext(&raw mut (*book()).recover);
        if (*book()).ended.load(Ordering::SeqCst) {
            return false;
        }
        let Some(cell) = link::jit_cell(obj, n) else {
            fail_cell();
        };
        (*book()).running.store(true, Ordering::SeqCst);
        cell();
        (*book()).running.store(false, Ordering::SeqCst);
        true
    }
}

/// The REPL waits for the host before removing the session directory, and a
/// running host.exe could not delete itself anyway.
const fn remove_session() {}

/// # Safety
/// Once, on the main thread.
pub(crate) unsafe fn main() -> i32 {
    // SAFETY: start-up, before any other thread.
    unsafe {
        *CTL.get() = win::GetStdHandle(win::STD_INPUT_HANDLE);
        // The cells' stdin: NUL.
        let nul = CreateFileA(
            c"NUL".as_ptr().cast(),
            win::GENERIC_READ,
            win::FILE_SHARE_READ | win::FILE_SHARE_WRITE,
            ptr::null_mut(),
            win::OPEN_EXISTING,
            0,
            0,
        );
        (*STDIN.get()).0 = nul;
        SetStdHandle(win::STD_INPUT_HANDLE, nul);
        // No concurrent marking: its marker thread would live on across a
        // rollback with its own idea of the heap.
        putenv_s(
            c"NABIR_GC_CONCURRENT".as_ptr().cast(),
            c"off".as_ptr().cast(),
        );
        nabi_watch_writes();
        crate::nabi_runtime_init();
        crate::stdio::nabi_stdio_line_buffered();
        let b = VirtualAlloc(
            ptr::null_mut(),
            BOOK_SIZE,
            MEM_COMMIT | MEM_RESERVE,
            PAGE_READWRITE,
        )
        .cast::<Book>();
        if b.is_null() {
            return 1;
        }
        *BOOK.get() = b;
        (*b).cap = (BOOK_SIZE - size_of::<Book>()) / size_of::<Region>();
        let ntdll = GetModuleHandleA(c"ntdll.dll".as_ptr().cast());
        *HOST.get() = Host {
            main_thread: GetCurrentThreadId(),
            ntdll: ntdll as *const u8,
            pool_wait: GetProcAddress(ntdll, c"NtWaitForWorkViaWorkerFactory".as_ptr().cast())
                .cast(),
            main_yield: nabir_common::tls::yield_flag(),
        };
        crate::nabi_set_exit_hook(Some(on_exit_hook));
        SetUnhandledExceptionFilter(Some(on_fault));
        win::AddVectoredExceptionHandler(1, on_overflow);
        win::SetConsoleCtrlHandler(Some(on_break), 1);
    }
    let b = book();
    let mut line = Buf::<8192>::new();
    loop {
        if !read_command(&mut line) {
            // The REPL has ended the session, or died.
            remove_session();
            return 0;
        }
        let Some((ir, n)) = next_cell(&line) else {
            continue;
        };
        if ir.is_null() {
            return 1;
        }
        crate::stdio::nabi_stdio_flush();
        // SAFETY: plain OS call.
        ctl_print(format_args!("pid {}\n", unsafe { GetCurrentProcessId() }));
        main_yield().fetch_and(!INTERRUPT, Ordering::SeqCst);
        clear_shown();
        set_why(b"");
        // SAFETY: the main thread, between cells; `ir` is `malloc`ed and,
        // after a rollback, still (the heap is as it was before the cell).
        unsafe {
            (*b).nwhy = 0;
            let saved = checkpoint();
            if saved == 0 {
                report!("error: no room to save the session before the cell\n");
            }
            if saved < 0 {
                (*b).why[..11].copy_from_slice(b"interrupted");
                (*b).nwhy = 11;
            }
            let ok = saved > 0 && run_cell(ir, n);
            unpark();
            libc::free(ir.cast());
            if !ok {
                // The stack's guard page again, after an overflow.
                if (*b).overflowed.load(Ordering::SeqCst) {
                    resetstkoflw();
                }
                (*b).overflowed.store(false, Ordering::SeqCst);
                // Memory is as it was at the checkpoint: the world stopped.
                if saved > 0 {
                    crate::session::nabi_session_resume();
                    crate::session::nabi_session_restored(
                        (&raw const (*b).lost).cast(),
                        (*b).nlost,
                    );
                }
                ctl_print(format_args!("err {}\n", (*b).nwhy));
                ctl_write(&(&(*b).why)[..(*b).nwhy]);
                continue;
            }
        }
        crate::stdio::nabi_stdio_flush();
        let out = shown();
        ctl_print(format_args!("ok {}\n", out.len()));
        ctl_write(out);
    }
}
