//! Panic backtraces (`NABI_BACKTRACE=1` or `full`): the frame-pointer
//! chain of compiled code from the panicking call, named by the
//! function table the linker writes (`nabi_backtrace_symbols` says
//! where it is).
//!
//! The panic entry points compiled code calls are small assembly stubs
//! that save the caller's frame pointer and return address before the
//! Rust code runs (the runtime itself keeps no frame pointers). Every
//! function nabic emits keeps one; the walk stops at the first return
//! address outside the table, before reading a frame it cannot trust.

use core::sync::atomic::{AtomicIsize, AtomicUsize, Ordering};

/// The function table's address less this word's own, written by the
/// linker; 1: no table (the link was not nabi's).
#[unsafe(no_mangle)]
pub static nabi_backtrace_symbols: AtomicIsize = AtomicIsize::new(1);

/// Frame pointer and return address of the last panic entry's caller.
#[repr(C)]
pub(crate) struct Frame([AtomicUsize; 2]);

pub(crate) static FRAME: Frame = Frame([AtomicUsize::new(0), AtomicUsize::new(0)]);

/// One table entry: start (from the table), size, name offset, length.
#[derive(Clone, Copy)]
struct Entry(i32, u32, u32, u32);

/// The linker's table: entries and names.
struct Table {
    base: usize,
    n: usize,
}

impl Table {
    fn get() -> Option<Self> {
        let d = nabi_backtrace_symbols.load(Ordering::Relaxed);
        if d == 1 {
            return None;
        }
        let base = (&raw const nabi_backtrace_symbols as usize).wrapping_add_signed(d);
        // SAFETY: the linker put the table's header at `base`.
        let (n, version) = unsafe { (word(base), word(base + 4)) };
        (version == 1).then_some(Self {
            base,
            n: n as usize,
        })
    }

    const fn entry(&self, i: usize) -> Entry {
        let at = self.base + 8 + 16 * i;
        // SAFETY: `i < n`, inside the table.
        unsafe { Entry(word(at) as i32, word(at + 4), word(at + 8), word(at + 12)) }
    }

    /// The name of the function holding `pc`.
    fn name(&self, pc: usize) -> Option<&'static [u8]> {
        let rel = pc.wrapping_sub(self.base) as isize;
        let (mut lo, mut hi) = (0, self.n);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.entry(mid).0 as isize <= rel {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        let e = self.entry(lo.checked_sub(1)?);
        let off = rel - e.0 as isize;
        // SAFETY: names lie in the table, as the linker wrote them.
        (off >= 0 && (off as usize) < e.1 as usize).then(|| unsafe {
            core::slice::from_raw_parts((self.base + e.2 as usize) as *const u8, e.3 as usize)
        })
    }
}

/// A 32-bit word of the table.
///
/// # Safety
/// `at` must be readable for 4 bytes, 4-aligned.
const unsafe fn word(at: usize) -> u32 {
    // SAFETY: as the caller promises.
    unsafe { (at as *const u32).read() }
}

/// Where a frame record keeps the caller's frame pointer and the return
/// address, from the frame pointer: as each prologue nabic emits saves
/// them.
const RECORD: (isize, isize) = if cfg!(target_arch = "arm") {
    // push {r4-r11, lr}; mov r11, sp
    (28, 32)
} else if cfg!(target_arch = "s390x") {
    // stmg r6, r15, 48(r15); lgr r11, r15
    (88, 112)
} else if cfg!(target_arch = "riscv64") {
    // s0 is the caller's sp, ra and s0 just below it.
    (-16, -8)
} else if cfg!(target_arch = "riscv32") {
    (-8, -4)
} else {
    // x86, aarch64: the old frame pointer, then the return address.
    (0, size_of::<usize>() as isize)
};

/// The most one frame of the walk may span: past it the chain is not a
/// stack's.
const MAX_FRAME: usize = 1 << 28;

/// # Safety
/// `at` must be a readable word.
unsafe fn read(at: usize) -> usize {
    // SAFETY: as the caller promises.
    unsafe { (at as *const usize).read_volatile() }
}

/// What `NABI_BACKTRACE` asks for: 0 none, 1 short, 2 full.
fn mode() -> u8 {
    match nabir_common::env_choice(c"NABI_BACKTRACE", "0|1|full") {
        Some("1") => 1,
        Some("full") => 2,
        _ => 0,
    }
}

/// `n` in decimal, right-aligned in 4 columns.
fn decimal(mut n: usize, b: &mut [u8; 20]) -> &[u8] {
    let mut i = b.len();
    loop {
        i -= 1;
        b[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    while i > b.len() - 4 {
        i -= 1;
        b[i] = b' ';
    }
    &b[i..]
}

fn hex(n: usize, b: &mut [u8; 18]) -> &[u8] {
    b[0] = b'0';
    b[1] = b'x';
    let digits = 2 * size_of::<usize>();
    for i in 0..digits {
        b[2 + i] = b"0123456789abcdef"[(n >> (4 * (digits - 1 - i))) & 15];
    }
    &b[..2 + digits]
}

/// The backtrace of the panicking thread, to stderr, as `NABI_BACKTRACE`
/// asks.
pub(crate) fn report() {
    let mode = mode();
    if mode == 0 {
        return;
    }
    let out = |parts: &[&[u8]]| {
        nabir_common::write_all(2, parts);
    };
    let (mut fp, mut pc) = (
        FRAME.0[0].swap(0, Ordering::Relaxed),
        FRAME.0[1].swap(0, Ordering::Relaxed),
    );
    let Some(table) = Table::get().filter(|_| cfg!(nabi_frames) && pc != 0) else {
        out(&[
            b"note: no backtrace: this program was not linked with nabi's function table, or \
                 the panic came from inside the runtime\n",
        ]);
        return;
    };
    out(&[b"stack backtrace:\n"]);
    let mut shown = 0usize;
    let mut top = true;
    let mut last: Option<&[u8]> = None;
    for _ in 0..1024 {
        // A return address: the call is the instruction before it.
        let name = table.name(pc.wrapping_sub(1));
        // The panic machinery's own frames lead the short form.
        let hidden = mode == 1 && top && name.is_some_and(|n| n.starts_with(b"core::panic::"));
        // The short form shows a name once in a row (a thunk and its
        // function, the C entry and the `main` it calls).
        let repeat = mode == 1 && name.is_some() && name == last;
        if !hidden && !repeat {
            top = false;
            let (mut dec, mut hexbuf) = ([0u8; 20], [0u8; 18]);
            let index = decimal(shown, &mut dec);
            match (mode, name) {
                (2, n) => out(&[
                    index,
                    b": ",
                    hex(pc, &mut hexbuf),
                    b" - ",
                    n.unwrap_or(b"<unknown>"),
                    b"\n",
                ]),
                (_, Some(n)) => out(&[index, b": ", n, b"\n"]),
                (_, None) => {}
            }
            shown += 1;
        }
        last = name;
        // Outside compiled code the frame pointer means nothing.
        if name.is_none() {
            break;
        }
        let next = (fp as isize + RECORD.0) as usize;
        // SAFETY: `fp` is the frame pointer of a function nabic
        // emitted (its `pc` is in the table), whose prologue saved both
        // words at these offsets.
        let (prev, ret) = unsafe { (read(next), read((fp as isize + RECORD.1) as usize)) };
        // Stacks grow down: a caller's frame is above, and not far.
        if prev <= fp || prev - fp > MAX_FRAME || prev % size_of::<usize>() != 0 || ret == 0 {
            break;
        }
        fp = prev;
        pc = ret;
    }
    if mode == 1 {
        out(&[
            b"note: some details are omitted, run with `NABI_BACKTRACE=full` for a verbose \
                 backtrace.\n",
        ]);
    }
}

// The panic entry points compiled code calls: each saves its caller's
// frame pointer and return address in `FRAME`, then jumps to the Rust
// function of the same name.

#[cfg(all(not(test), nabi_frames, target_vendor = "apple"))]
macro_rules! head {
    ($n:ident) => {
        concat!(
            ".globl _",
            stringify!($n),
            "\n.p2align 2\n_",
            stringify!($n),
            ":\n"
        )
    };
}

#[cfg(all(not(test), nabi_frames, windows, target_arch = "x86"))]
macro_rules! head {
    ($n:ident) => {
        concat!(
            ".globl _",
            stringify!($n),
            "\n.p2align 4\n_",
            stringify!($n),
            ":\n"
        )
    };
}

#[cfg(all(not(test), nabi_frames, windows, not(target_arch = "x86")))]
macro_rules! head {
    ($n:ident) => {
        concat!(
            ".globl ",
            stringify!($n),
            "\n.p2align 4\n",
            stringify!($n),
            ":\n"
        )
    };
}

#[cfg(all(not(test), nabi_frames, not(windows), not(target_vendor = "apple")))]
macro_rules! head {
    ($n:ident) => {
        concat!(
            ".globl ",
            stringify!($n),
            "\n.type ",
            stringify!($n),
            ", %function\n.p2align 4\n",
            stringify!($n),
            ":\n"
        )
    };
}

#[cfg(all(not(test), nabi_frames))]
macro_rules! stub {
    ($n:ident, $save:expr $(, $opt:ident)?) => {
        core::arch::global_asm!(
            concat!(".text\n", head!($n), $save),
            imp = sym crate::$n,
            frame = sym FRAME,
            $(options($opt))?
        );
    };
}

#[cfg(all(not(test), nabi_frames, target_arch = "x86_64"))]
macro_rules! save {
    () => {
        "mov qword ptr [rip + {frame}], rbp
         mov rax, qword ptr [rsp]
         mov qword ptr [rip + {frame} + 8], rax
         jmp {imp}\n"
    };
}

// i386 Linux code is position independent: the address from a call.
#[cfg(all(not(test), nabi_frames, target_arch = "x86", not(windows)))]
macro_rules! save {
    () => {
        "call 2f
         2: popl %ecx
         movl %ebp, {frame}-2b(%ecx)
         movl (%esp), %eax
         movl %eax, {frame}+4-2b(%ecx)
         jmp {imp}\n"
    };
}

#[cfg(all(not(test), nabi_frames, target_arch = "x86", windows))]
macro_rules! save {
    () => {
        "movl %ebp, {frame}
         movl (%esp), %eax
         movl %eax, {frame}+4
         jmp {imp}\n"
    };
}

#[cfg(all(
    not(test),
    nabi_frames,
    target_arch = "aarch64",
    target_vendor = "apple"
))]
macro_rules! save {
    () => {
        "adrp x16, {frame}@PAGE
         add x16, x16, {frame}@PAGEOFF
         stp x29, x30, [x16]
         b {imp}\n"
    };
}

#[cfg(all(
    not(test),
    nabi_frames,
    target_arch = "aarch64",
    not(target_vendor = "apple")
))]
macro_rules! save {
    () => {
        "adrp x16, {frame}
         add x16, x16, :lo12:{frame}
         stp x29, x30, [x16]
         b {imp}\n"
    };
}

#[cfg(all(not(test), nabi_frames, target_arch = "arm"))]
macro_rules! save {
    () => {
        "ldr ip, 3f
         2: add ip, pc, ip
         str r11, [ip]
         str lr, [ip, #4]
         b {imp}
         3: .word {frame} - (2b + 8)\n"
    };
}

#[cfg(all(not(test), nabi_frames, target_arch = "riscv64"))]
macro_rules! save {
    () => {
        "lla t0, {frame}
         sd s0, 0(t0)
         sd ra, 8(t0)
         tail {imp}\n"
    };
}

#[cfg(all(not(test), nabi_frames, target_arch = "riscv32"))]
macro_rules! save {
    () => {
        "lla t0, {frame}
         sw s0, 0(t0)
         sw ra, 4(t0)
         tail {imp}\n"
    };
}

#[cfg(all(not(test), nabi_frames, target_arch = "s390x"))]
macro_rules! save {
    () => {
        "larl %r1, {frame}
         stg %r11, 0(%r1)
         stg %r14, 8(%r1)
         jg {imp}\n"
    };
}

#[cfg(all(not(test), nabi_frames, target_arch = "x86"))]
macro_rules! stubs {
    ($($n:ident),*) => {$(stub!($n, save!(), att_syntax);)*};
}

#[cfg(all(not(test), nabi_frames, not(target_arch = "x86")))]
macro_rules! stubs {
    ($($n:ident),*) => {$(stub!($n, save!());)*};
}

#[cfg(all(not(test), nabi_frames))]
stubs!(nabi_panic, nabi_panic_at, nabi_panic_str, nabi_try_failed);
