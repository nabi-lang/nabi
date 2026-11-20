//! The loader's generic half: map an object's sections near the host, its
//! symbols defined for the session, relocations applied by what they
//! compute (`fix`). The format readers (`map_object`, `link_object`) feed it.

#[cfg(windows)]
use crate::repl::coff::{link_object, map_object};
#[cfg(target_os = "linux")]
use crate::repl::elf::{link_object, map_object};
#[cfg(target_os = "macos")]
use crate::repl::macho::{link_object, map_object};
use crate::repl::{CELLS, Global, Text, bare, cstr, define, report, resolve};
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
use core::arch::asm;
use core::ffi::c_void;
use core::ptr;
#[cfg(windows)]
use nabir_common::win;
#[cfg(windows)]
use nabir_common::win::{MEM_COMMIT, MEM_RESERVE, PAGE_READWRITE, VirtualAlloc};

/// Relocation kinds, by what they compute (S symbol, A addend, P the
/// place, G a slot holding S, T a slot holding the thread-local's value).
// Each format computes some of them only.
#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// S + A, 64 bits.
    Abs64,
    /// S + A, 32 bits.
    Abs32,
    /// S + A - P, 32 bits.
    Pc32,
    /// S + A - P, `x86_64` `call` / `jmp`: far through a stub.
    Call32,
    /// G + A - P.
    GotPc32,
    /// T + A - P.
    TlsPc32,
    /// arm64 `b` / `bl` to S + A: far through a stub.
    B26,
    /// arm64 `adrp` of S + A.
    Page21,
    /// arm64 low 12 bits of S + A (scaled for a load / store).
    Lo12,
    /// ... of G + A.
    GotPage21,
    GotLo12,
    /// ... of T + A.
    TlsPage21,
    TlsLo12,
    /// S + A - the object's data mapping, 32 bits: far through a stub.
    Rva32,
    /// arm64 `add` of bits 12-23 of S + A.
    Hi12,
}

/// An object mapped, its symbols defined, not yet linked.
pub(crate) struct Obj {
    pub buf: *const u8,
    /// What it is, for messages (a C string).
    pub what: *const u8,
    pub nsects: u32,
    pub nsyms: u32,
    /// Each section's mapped address (null: not mapped) and its address
    /// in the object.
    pub base: *mut *mut u8,
    pub at: *mut u64,
    /// Each symbol's address (null: undefined here), and its GOT slot and
    /// stub once made.
    pub addr: *mut *mut u8,
    pub got: *mut *mut u8,
    pub stub: *mut *mut u8,
    /// Code (executable) and data (writable) mappings; GOT slots go after
    /// the data, branch stubs after the code.
    pub code: *mut u8,
    pub data: *mut u8,
    pub code_cap: usize,
    pub data_cap: usize,
    pub stubs: usize,
    pub slots: usize,
    pub entry: *mut u8,
}

impl Obj {
    pub const fn new(buf: *const u8, what: *const u8) -> Self {
        Self {
            buf,
            what,
            nsects: 0,
            nsyms: 0,
            base: ptr::null_mut(),
            at: ptr::null_mut(),
            addr: ptr::null_mut(),
            got: ptr::null_mut(),
            stub: ptr::null_mut(),
            code: ptr::null_mut(),
            data: ptr::null_mut(),
            code_cap: 0,
            data_cap: 0,
            stubs: 0,
            slots: 0,
            entry: ptr::null_mut(),
        }
    }

    /// Section `k`'s mapped address (null: not mapped).
    pub fn base(&self, k: u32) -> *mut u8 {
        // SAFETY: `base` holds `nsects` entries once mapped.
        unsafe { *self.base.add(k as usize) }
    }

    /// Symbol `i`'s address here (null: undefined here).
    pub fn addr(&self, i: u32) -> *mut u8 {
        // SAFETY: `addr` holds `nsyms` entries.
        unsafe { *self.addr.add(i as usize) }
    }

    pub const fn what(&self) -> Text<'_> {
        // SAFETY: a C string for the object's life.
        Text(unsafe { cstr(self.what) })
    }
}

/// Section `k` of what is mapped: `size` bytes aligned to `align`, code
/// or data, copied from `src` (null: zero).
#[derive(Clone, Copy)]
pub(crate) struct Place {
    pub size: u64,
    pub align: u64,
    pub code: bool,
    pub src: *const u8,
}

impl Place {
    pub const NONE: Self = Self {
        size: 0,
        align: 0,
        code: false,
        src: ptr::null(),
    };
}

/// `n` zeroed `T`s, `malloc`ed (one to spare).
pub(crate) fn zeroed<T>(n: usize) -> *mut T {
    // SAFETY: plain libc call.
    let p = unsafe { libc::calloc(n + 1, size_of::<T>()) }.cast::<T>();
    if p.is_null() {
        nabir_common::oom();
    }
    p
}

// Executable memory: Apple arm64 flips a `MAP_JIT` mapping per thread,
// elsewhere RWX. `map_near`: `size` bytes at `at`, else the first free
// place above it within 1 GiB, else anywhere; null: none.

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn sys_icache_invalidate(start: *mut c_void, len: usize);
}

// Empty off macOS, so `const` there only.
#[allow(clippy::missing_const_for_fn)]
pub(crate) fn jit_writable(on: bool) {
    #[cfg(target_os = "macos")]
    // SAFETY: flips this thread's view of `MAP_JIT` memory.
    unsafe {
        libc::pthread_jit_write_protect_np(i32::from(!on));
    }
    #[cfg(not(target_os = "macos"))]
    let _ = on;
}

// Empty on x86_64 Linux, so `const` there only.
#[allow(clippy::missing_const_for_fn)]
pub(crate) fn flush_code(p: *mut u8, n: usize) {
    #[cfg(target_os = "macos")]
    // SAFETY: `p..p + n` is mapped code.
    unsafe {
        sys_icache_invalidate(p.cast(), n);
    }
    #[cfg(windows)]
    // SAFETY: as above.
    unsafe {
        crate::repl::windows::FlushInstructionCache(win::GetCurrentProcess(), p.cast(), n);
    }
    // arm64 cleans and invalidates caches by line (no libgcc
    // `__clear_cache` here); x86_64 keeps fetch coherent.
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    // SAFETY: cache maintenance of mapped code.
    unsafe {
        let ctr: u64;
        asm!("mrs {}, ctr_el0", out(reg) ctr, options(nomem, nostack, preserves_flags));
        let d = 4usize << ((ctr >> 16) & 15);
        let i = 4usize << (ctr & 15);
        let end = p as usize + n;
        let mut a = p as usize & !(d - 1);
        while a < end {
            asm!("dc cvau, {}", in(reg) a, options(nostack, preserves_flags));
            a += d;
        }
        asm!("dsb ish", options(nostack, preserves_flags));
        a = p as usize & !(i - 1);
        while a < end {
            asm!("ic ivau, {}", in(reg) a, options(nostack, preserves_flags));
            a += i;
        }
        asm!("dsb ish", "isb", options(nostack, preserves_flags));
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    let _ = (p, n);
}

#[cfg(windows)]
fn map_near(at: *mut u8, size: usize, code: bool) -> *mut u8 {
    const PAGE_EXECUTE_READWRITE: u32 = 0x40;
    let prot = if code {
        PAGE_EXECUTE_READWRITE
    } else {
        PAGE_READWRITE
    };
    // `VirtualAlloc` takes an address, not a hint: the first free 64 KiB
    // boundary from `at` on, within 1 GiB.
    let mut a = (at as usize + 0xffff) & !0xffff;
    let end = a + (1 << 30);
    // SAFETY: plain OS calls; a taken range fails.
    unsafe {
        while a < end {
            let p = VirtualAlloc(a as *mut c_void, size, MEM_COMMIT | MEM_RESERVE, prot);
            if !p.is_null() {
                return p.cast();
            }
            a += 0x10000;
        }
        VirtualAlloc(ptr::null_mut(), size, MEM_COMMIT | MEM_RESERVE, prot).cast()
    }
}

#[cfg(unix)]
fn map_near(at: *mut u8, size: usize, code: bool) -> *mut u8 {
    #[cfg(target_os = "macos")]
    const MAP_CODE: i32 = libc::MAP_JIT;
    #[cfg(not(target_os = "macos"))]
    const MAP_CODE: i32 = 0;
    let prot = libc::PROT_READ | libc::PROT_WRITE | if code { libc::PROT_EXEC } else { 0 };
    let flags = libc::MAP_PRIVATE | libc::MAP_ANON | if code { MAP_CODE } else { 0 };
    let page = nabir_common::sys::page_size();
    let at = (at as usize).next_multiple_of(page);
    // SAFETY: fresh anonymous mappings; a fixed one never replaces.
    unsafe {
        // Linux drops a hint whose range is taken (the heap, say) and maps
        // at the top of the address space, out of `adrp` reach of the host:
        // the first free 64 KiB boundary from `at` on, within 1 GiB.
        #[cfg(target_os = "linux")]
        {
            const MAP_FIXED_NOREPLACE: i32 = 0x10_0000;
            let mut a = at;
            while a < at + (1 << 30) {
                let p = libc::mmap(
                    a as *mut c_void,
                    size,
                    prot,
                    flags | MAP_FIXED_NOREPLACE,
                    -1,
                    0,
                );
                if p as usize == a {
                    return p.cast();
                }
                // Before 4.17 the flag is ignored: a hint not taken.
                if p != libc::MAP_FAILED {
                    libc::munmap(p, size);
                } else if nabir_common::errno() != libc::EEXIST {
                    break;
                }
                a = (a + 0x10000) & !0xffff;
            }
        }
        // Darwin takes a hint as where to start looking.
        let p = libc::mmap(at as *mut c_void, size, prot, flags, -1, 0);
        if p == libc::MAP_FAILED {
            ptr::null_mut()
        } else {
            p.cast()
        }
    }
}

/// The next mapping's hint: after the last.
static NEAR: Global<usize> = Global::new(0);

/// Map the sections (`p[k].align` 0: not mapped), with room for a stub
/// and a slot per relocation (`nrel`).
pub(crate) fn map_sections(o: &mut Obj, p: &[Place], nrel: usize) -> bool {
    let off = zeroed::<usize>(p.len());
    let (mut code, mut data) = (0usize, 0usize);
    // SAFETY: `off` holds an entry per section.
    unsafe {
        for (k, s) in p.iter().enumerate() {
            if s.align == 0 {
                continue;
            }
            let end = if s.code { &mut code } else { &mut data };
            let align = s.align as usize;
            *end = (*end + align - 1) & !(align - 1);
            *off.add(k) = *end + 1;
            *end += s.size as usize;
        }
    }
    code = (code + 15) & !15;
    data = (data + 15) & !15;
    o.stubs = code;
    o.slots = data;
    o.code_cap = code + 16 * nrel + 16;
    o.data_cap = data + 8 * nrel + 16;
    // Data and code near each other and the host (cells may address runtime
    // data directly): `adrp` reaches 4 GiB, rip-relative 2 GiB. Only a hint;
    // a reference out of range is reported. Code after data, since COFF's
    // image-relative references count from the data.
    // SAFETY: the host's thread only.
    let near = unsafe { &mut *NEAR.get() };
    if *near == 0 {
        *near = (CELLS.get() as usize + (1 << 29)) & !0xffff;
    }
    o.data = map_near(*near as *mut u8, o.data_cap, false);
    o.code = if o.data.is_null() {
        ptr::null_mut()
    } else {
        map_near(o.data.wrapping_add(o.data_cap), o.code_cap, true)
    };
    if o.code.is_null() {
        report!("error: {}: cannot map executable memory\n", o.what());
        // SAFETY: `zeroed`'s.
        unsafe { libc::free(off.cast()) };
        return false;
    }
    *near = o.code as usize + o.code_cap;
    o.base = zeroed(p.len());
    jit_writable(true);
    // SAFETY: each section's place is inside its mapping.
    unsafe {
        for (k, s) in p.iter().enumerate() {
            let at = *off.add(k);
            if at == 0 {
                continue;
            }
            let b = if s.code { o.code } else { o.data }.add(at - 1);
            *o.base.add(k) = b;
            if !s.src.is_null() {
                ptr::copy_nonoverlapping(s.src, b, s.size as usize);
            }
        }
        libc::free(off.cast());
    }
    jit_writable(false);
    true
}

pub(crate) fn alloc_syms(o: &mut Obj, n: u32) {
    o.nsyms = n;
    o.addr = zeroed(n as usize);
    o.got = zeroed(n as usize);
    o.stub = zeroed(n as usize);
}

/// Symbol `i` defined at `addr`; `global`: the session's too.
///
/// # Safety
/// `name` is a C string; `i` is a symbol of `o`.
pub(crate) unsafe fn def_sym(o: &mut Obj, i: u32, name: *const u8, addr: *mut u8, global: bool) {
    // SAFETY: caller contract.
    unsafe {
        *o.addr.add(i as usize) = addr;
        if global {
            define(cstr(name), addr);
        }
        if bare(name) == b"nabi_repl_cell" {
            o.entry = addr;
        }
    }
}

/// The address of symbol `i` (`name`), defined here or resolved; 0:
/// undefined (reported).
///
/// # Safety
/// As `def_sym`.
pub(crate) unsafe fn sym_value(o: &Obj, i: u32, name: *const u8, tls: bool) -> u64 {
    if !o.addr(i).is_null() && !tls {
        return o.addr(i) as u64;
    }
    // SAFETY: caller contract.
    let a = unsafe { resolve(name, tls) };
    if a.is_null() {
        // SAFETY: caller contract.
        let n = unsafe { bare(name) };
        report!("error: {}: undefined symbol `{}`\n", o.what(), Text(n));
    }
    a as u64
}

/// A GOT slot holding `value`, one per symbol (`sym` -1: none).
pub(crate) fn slot(o: &mut Obj, sym: i64, value: u64) -> *mut u8 {
    // SAFETY: slots stay within `data_cap` (one per relocation at most).
    unsafe {
        let mut g = if sym >= 0 {
            *o.got.add(sym as usize)
        } else {
            ptr::null_mut()
        };
        if g.is_null() {
            g = o.data.add(o.slots);
            o.slots += 8;
            ptr::write_unaligned(g.cast::<u64>(), value);
            if sym >= 0 {
                *o.got.add(sym as usize) = g;
            }
        }
        g
    }
}

/// A branch stub to `target`, one per symbol (`sym` -1: none).
fn stub(o: &mut Obj, sym: i64, target: u64) -> *mut u8 {
    // SAFETY: stubs stay within `code_cap` (one per relocation at most).
    unsafe {
        let mut t = if sym >= 0 {
            *o.stub.add(sym as usize)
        } else {
            ptr::null_mut()
        };
        if t.is_null() {
            t = o.code.add(o.stubs);
            o.stubs += 16;
            // jmp *2(%rip): the address at +8, after the 6-byte jmp and 2 pad.
            #[cfg(target_arch = "x86_64")]
            let jump: [u8; 8] = [0xff, 0x25, 2, 0, 0, 0, 0x90, 0x90];
            // ldr x16, #8; br x16
            #[cfg(target_arch = "aarch64")]
            let jump: [u8; 8] = {
                let (a, b) = (0x5800_0050u32.to_le_bytes(), 0xd61f_0200u32.to_le_bytes());
                [a[0], a[1], a[2], a[3], b[0], b[1], b[2], b[3]]
            };
            ptr::copy_nonoverlapping(jump.as_ptr(), t, 8);
            ptr::write_unaligned(t.add(8).cast::<u64>(), target);
            if sym >= 0 {
                *o.stub.add(sym as usize) = t;
            }
        }
        t
    }
}

const fn put32(p: *mut u8, v: u32) {
    // SAFETY: the caller's place holds 4 bytes.
    unsafe { ptr::write_unaligned(p.cast::<u32>(), v) };
}

/// Apply one relocation at `p`: symbol index `sym` (-1: none) with value
/// `s`, addend `a` (false: failed, reported).
pub(crate) fn fix(o: &mut Obj, p: *mut u8, k: Kind, sym: i64, mut s: u64, a: i64) -> bool {
    let pc = p as u64;
    if matches!(
        k,
        Kind::GotPc32
            | Kind::GotPage21
            | Kind::GotLo12
            | Kind::TlsPc32
            | Kind::TlsPage21
            | Kind::TlsLo12
    ) {
        s = slot(o, sym, s) as u64;
    }
    let mut target = s.wrapping_add(a as u64);
    match k {
        Kind::Abs64 => {
            // SAFETY: the place holds 8 bytes.
            unsafe { ptr::write_unaligned(p.cast::<u64>(), target) };
            return true;
        }
        Kind::Abs32 => {
            put32(p, target as u32);
            return true;
        }
        Kind::Rva32 => {
            // An unwind handler in the host's code: through a stub.
            let data = o.data as u64;
            if target.wrapping_sub(data) > u64::from(u32::MAX) && sym >= 0 {
                target = stub(o, sym, target) as u64;
            }
            let v = target.wrapping_sub(data);
            if v > u64::from(u32::MAX) {
                report!(
                    "error: {}: an image-relative reference out of range\n",
                    o.what()
                );
                return false;
            }
            put32(p, v as u32);
            return true;
        }
        Kind::Call32 | Kind::Pc32 | Kind::GotPc32 | Kind::TlsPc32 => {
            let mut d = target.wrapping_sub(pc) as i64;
            if i32::try_from(d).is_err() && k == Kind::Call32 {
                // Far (the host's code): through a stub. The field ends the
                // instruction, so the destination is `target + 4`.
                let t = stub(o, sym, target.wrapping_add(4)) as i64;
                d = t.wrapping_sub(pc as i64 + 4);
            }
            let Ok(v) = i32::try_from(d) else {
                report!(
                    "error: {}: a pc-relative reference out of range\n",
                    o.what()
                );
                return false;
            };
            put32(p, v as u32);
            return true;
        }
        _ => {}
    }
    // SAFETY: an arm64 instruction's place.
    let mut ins = unsafe { ptr::read_unaligned(p.cast::<u32>()) };
    match k {
        Kind::B26 => {
            let mut d = target.wrapping_sub(pc) as i64;
            if !(-(1 << 27)..1 << 27).contains(&d) {
                d = (stub(o, sym, target) as i64).wrapping_sub(pc as i64);
            }
            ins = (ins & 0xfc00_0000) | ((d >> 2) as u32 & 0x03ff_ffff);
        }
        Kind::Page21 | Kind::GotPage21 | Kind::TlsPage21 => {
            let pages = (target >> 12) as i64 - (pc >> 12) as i64;
            if !(-(1 << 20)..1 << 20).contains(&pages) {
                report!("error: {}: a page reference out of range\n", o.what());
                return false;
            }
            let pages = pages as u32;
            ins = (ins & 0x9f00_001f) | ((pages & 3) << 29) | (((pages >> 2) & 0x7ffff) << 5);
        }
        Kind::Hi12 => ins = (ins & 0xffc0_03ff) | ((((target >> 12) & 0xfff) as u32) << 10),
        Kind::Lo12 | Kind::GotLo12 | Kind::TlsLo12 => {
            let mut lo = (target & 0xfff) as u32;
            // A load / store scales its offset by the access size.
            if (ins & 0x3b00_0000) == 0x3900_0000 {
                // A 128-bit vector's is 16 bytes.
                let scale = if (ins & 0x0480_0000) == 0x0480_0000 {
                    4
                } else {
                    ins >> 30
                };
                lo >>= scale;
            }
            ins = (ins & 0xffc0_03ff) | ((lo & 0xfff) << 10);
        }
        _ => return false,
    }
    put32(p, ins);
    true
}

pub(crate) fn free_obj(o: &mut Obj) {
    // SAFETY: `zeroed`'s or null.
    unsafe {
        libc::free(o.base.cast());
        libc::free(o.at.cast());
        libc::free(o.addr.cast());
        libc::free(o.got.cast());
        libc::free(o.stub.cast());
    }
}

/// A cell's entry point.
pub(crate) type Cell = unsafe extern "C" fn();

/// Map and link the cell; its entry point (None: failed, reported).
pub(crate) fn jit_cell(obj: *const u8, n: usize) -> Option<Cell> {
    // SAFETY: the host's thread only.
    unsafe { *CELLS.get() += 1 };
    let mut o = Obj::new(obj, c"cell".as_ptr().cast());
    // SAFETY: `obj` holds `n` bytes.
    let ok = unsafe { map_object(&mut o, obj, n) } && link_object(&mut o) && !o.entry.is_null();
    let entry = o.entry;
    free_obj(&mut o);
    // SAFETY: the object's `nabi_repl_cell`, a C function.
    ok.then(|| unsafe { core::mem::transmute::<*mut u8, Cell>(entry) })
}

/// A shared library, loaded by the system: later symbols resolve against
/// it too. true: it failed (reported).
///
/// # Safety
/// `path` is a C string.
unsafe fn load_shared(path: *const u8) -> bool {
    #[cfg(windows)]
    // SAFETY: caller contract.
    unsafe {
        crate::repl::windows::load_shared(path)
    }
    #[cfg(unix)]
    // SAFETY: caller contract.
    unsafe {
        if !libc::dlopen(path.cast(), libc::RTLD_NOW | libc::RTLD_GLOBAL).is_null() {
            return false;
        }
        report!("error: {}\n", Text(cstr(libc::dlerror().cast())));
        true
    }
}

/// A decimal number at `p`, after blanks (an `ar` header field).
///
/// # Safety
/// `p` is readable up to the first byte that is not a blank or digit.
unsafe fn field(mut p: *const u8) -> usize {
    let mut n = 0usize;
    // SAFETY: caller contract.
    unsafe {
        while *p == b' ' {
            p = p.add(1);
        }
        while (*p).is_ascii_digit() {
            n = n.wrapping_mul(10).wrapping_add(usize::from(*p - b'0'));
            p = p.add(1);
        }
    }
    n
}

/// Link every member of an archive (BSD, GNU or COFF `ar`) now, all mapped
/// first so they can refer to each other; other files are shared libraries
/// (`load_shared`). true: it failed.
///
/// # Safety
/// `path` is a C string.
pub(crate) unsafe fn link_lib(path: *const u8) -> bool {
    const MAX: usize = 256;
    // SAFETY: plain libc calls; the archive's members are bounds-checked
    // against its size.
    unsafe {
        let f = libc::fopen(path.cast(), c"rb".as_ptr());
        if f.is_null() {
            return true;
        }
        libc::fseek(f, 0, libc::SEEK_END);
        let size = libc::ftell(f) as usize;
        let a = libc::malloc(size + 1).cast::<u8>();
        libc::fseek(f, 0, libc::SEEK_SET);
        let whole = !a.is_null() && libc::fread(a.cast(), 1, size, f) == size;
        libc::fclose(f);
        if !whole || size < 8 || core::slice::from_raw_parts(a, 8) != b"!<arch>\n" {
            libc::free(a.cast());
            return if whole { load_shared(path) } else { true };
        }
        *a.add(size) = 0;
        let objs = zeroed::<Obj>(MAX);
        let mut n = 0;
        let mut bad = false;
        *CELLS.get() += 1;
        let mut off = 8;
        while off + 60 <= size && n < MAX && !bad {
            let h = a.add(off);
            let len = field(h.add(48));
            let skip = if core::slice::from_raw_parts(h, 3) == b"#1/" {
                field(h.add(3))
            } else {
                0
            };
            let name_at = if skip != 0 { 60 } else { 0 };
            let name = core::slice::from_raw_parts(h.add(name_at), 16.min(size - off - name_at));
            let body = h.add(60 + skip);
            // Not the symbol table (BSD `__.SYMDEF`, GNU and COFF `/`,
            // `/SYM64/`) nor the long-name table `//`.
            let index = name.starts_with(b"__.SYMDEF")
                || matches!(name, [b'/', b' ' | b'/', ..])
                || name.starts_with(b"/SYM64/");
            if !index && len > skip && 60 + len <= size - off {
                let o = &mut *objs.add(n);
                *o = Obj::new(body, path);
                if map_object(o, body, len - skip) {
                    n += 1;
                } else {
                    bad = true;
                }
            }
            off += 60 + len;
            off += off & 1;
        }
        for i in 0..n {
            let o = &mut *objs.add(i);
            if !bad && !link_object(o) {
                bad = true;
            }
            free_obj(o);
        }
        libc::free(objs.cast());
        libc::free(a.cast());
        bad
    }
}
