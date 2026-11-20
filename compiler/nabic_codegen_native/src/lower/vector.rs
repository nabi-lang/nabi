//! Loop vectorization, above `-O0` on `x86_64` (SSE2, or AVX2 where the
//! CPU options name it), i686 (SSE2), arm64 (NEON), 32-bit ARM (NEON,
//! integers only), powerpc64 (VMX / VSX), wasm32 with SIMD128, s390x
//! with the vector facility (z13; floats of 4 bytes from z14) and
//! riscv64 with V (128 bits of each register, at any `VLEN`): a loop
//! polled on no back edge (a strip of `nabic_mir::strip`, or another
//! short loop) that steps a `usize` counter by one up to a bound it never
//! changes, its blocks below the header acyclic, doing element-wise work
//! on buffers indexed by the counter:
//!
//! ```text
//! H:  x = buf_load a[i]; c = x == k; if c then E1 else L
//! L:  y = .. x ..; buf_store b[i] = y; s = s + y
//!     i = i + 1; c = i < n; if c then H else E
//! ```
//!
//! On entry, before the scalar body, a `VLoop` runs whole vectors of the
//! iterations; the scalar loop after it does the rest. A vector holding
//! an element that would leave the loop (`E1`) or trap (overflow) is not
//! run: the scalar loop takes it from its first element and leaves or
//! traps at that element, every element before it done. A side exit on
//! the counter (a bounds check) only lowers the bound the vector loop
//! keeps to. A branch inside the loop runs both ways in every lane: a
//! value assigned on one way keeps, in the other lanes, what it held; a
//! load there reads only elements every lane may read (accessed on every
//! way, or under a bounds check the loop keeps to); stores on the ways
//! meet, merged, where the ways do, else go out under a mask where the
//! target has masked stores (else no vector loop).
//!
//! Reductions keep partial results per lane, combined once the vector
//! loop is done: integer `+` that wraps, `& | ^`, counts, minima and
//! maxima (`m = if x < m { x } else { m }`, `m = m.min(x)`, the lanes
//! starting from `m`; of floats, a vector holding a NaN or a zero, which
//! lanes would order as the scalar loop does not, is left to it, and a
//! NaN `m` leaves the whole loop to it), sums of products of bytes (four
//! to a lane with arm64 `dotprod`);
//! an accumulator may be updated on several ways (each lane adds what its
//! way adds). A checked `+` / `-` keeps the non-negative and negative
//! parts apart, which bound every partial sum the scalar loop would make
//! (one update on any way through), and when either bound overflows the
//! whole range runs scalar again (such a loop stores nothing). Float sums
//! are left alone: their order is the program's. A checked `*` tests the
//! high half of each product (where the target multiplies so), a vector
//! with one that overflows left to the scalar loop.
//!
//! Every lane is `vb / w` elements of `w` bytes, one `w` per loop;
//! elements may be narrower, loaded and widened as a cast says, or
//! narrowed and stored. An access may index `i + c` (`c` small), or `i +
//! inv + c` (`inv` a word the loop never changes, through `isize` casts
//! and checked adds: a slice's `offset + i`, exact below `isize::MAX`,
//! which a test in front checks). Two accesses that may name one element
//! in different lanes keep their order, else the loop needs the buffers
//! to differ, a test before it. Nothing in the vector loop reaches a
//! safepoint: the buffer addresses it computes from its bases never meet
//! a collection. Past a strip it runs `MAX_TRIPS` iterations at most
//! before the strip loop's poll.

mod body;
mod emit;
mod lanes;
mod ops;
mod plan;
mod reduce;
mod regs;
mod simd;
mod slp;
mod stmt;

pub(crate) use crate::lower::vector::body::*;
pub(crate) use crate::lower::vector::plan::*;
pub(crate) use crate::lower::vector::regs::*;
pub(crate) use crate::lower::vector::slp::Plan as SlpPlan;

use crate::lir::{
    Bin, Cc, Conv, Inst, K, Label, Mem, V, VBin, VIO_BASES, VIO_BOUND, VIO_IV, VLoop, VOp, VReg,
    VShift, VUn,
};
use crate::lower::{FnLower, Val};
use nabic_core::{HashMap, HashSet};
use nabic_ir_common::{BlockId, ConstVal, LocalId, PrimOp, PrimTy};
use nabic_mir::gc_abi::HEADER_SIZE;
use nabic_mir::{BasicBlock, MirFunction, MirTy, Operand, RValue, Statement, Target, Terminator};

/// The vector unit code is made for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Isa {
    /// SSE2, or AVX2 (256-bit vectors, VEX forms).
    X64 {
        win: bool,
        avx2: bool,
    },
    /// SSE2 with eight registers.
    I686,
    /// 32-bit ARM NEON, integers only.
    Arm,
    /// POWER8 VMX and VSX, one element width per loop.
    Ppc,
    A64,
    Wasm,
    /// The z13 vector facility: floats of 8 bytes, of 4 too with the
    /// z14 enhancements (`ve1`).
    S390x {
        ve1: bool,
    },
    /// RISC-V V: 128 bits of every register.
    Rv,
}

/// Iterations a vector loop runs at most per entry when it runs past a
/// strip: what keeps a lane's count small and the polls near.
const MAX_TRIPS: i64 = 1024;

/// Index offsets an access may add to the counter.
const MAX_OFF: u64 = 16;

/// An index the loop computes from its counter: `iv + inv + c`, `inv` a
/// local the loop never changes. Through `isize` or a checked `+` only
/// below `isize::MAX` (`Plan::guard`).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct Idx {
    inv: Option<LocalId>,
    c: u64,
}

impl Idx {
    const IV: Self = Self { inv: None, c: 0 };
}

impl Isa {
    /// The target's, if its loops are vectorized.
    pub fn of(t: &Target) -> Option<Self> {
        match t.arch.as_str() {
            "x86_64" => Some(Self::X64 {
                win: t.os == "windows",
                avx2: t.cpu_features().has("avx2"),
            }),
            "aarch64" => Some(Self::A64),
            "x86" => Some(Self::I686),
            "arm" if t.cpu_features().has("neon") => Some(Self::Arm),
            "powerpc64" if t.cpu_features().has("vsx") => Some(Self::Ppc),
            "wasm32" if simd128(&t.features) => Some(Self::Wasm),
            "riscv64" if t.cpu_features().has("v") => Some(Self::Rv),
            "s390x" if t.cpu_features().has("vector") => Some(Self::S390x {
                ve1: t.cpu_features().has("vector-enhancements-1"),
            }),
            _ => None,
        }
    }

    /// Vector registers a loop may use.
    const fn regs(self) -> usize {
        match self {
            // xmm15 is the emitter's.
            Self::X64 { .. } => 15,
            // xmm7 is the emitter's.
            Self::I686 => 7,
            // q0..q3, q8..q14; q15 is the emitter's.
            Self::Arm => 11,
            // v0..v17; v18, v19 are the emitter's.
            Self::Ppc => 18,
            // v0..v7, v16..v30; v31 is the emitter's.
            Self::A64 => 23,
            Self::Wasm => 32,
            // v0..v7, v16..v29; v30, v31 are the emitter's.
            Self::S390x { .. } => 22,
            // v1..v30; v0 holds masks, v31 is the emitter's.
            Self::Rv => 30,
        }
    }

    /// Bases a loop may use (integer registers left for them).
    const fn bases(self) -> usize {
        match self {
            Self::X64 { win: true, .. } => 4,
            Self::I686 => 3,
            Self::X64 { win: false, .. } | Self::Arm | Self::Ppc | Self::S390x { .. } => 6,
            Self::A64 | Self::Wasm => 12,
            Self::Rv => 8,
        }
    }

    const fn bin(self, op: VBin, w: u32) -> bool {
        let float = matches!(
            op,
            VBin::FAdd
                | VBin::FSub
                | VBin::FMul
                | VBin::FDiv
                | VBin::FEq
                | VBin::FNe
                | VBin::FLt
                | VBin::FLe
                | VBin::FMin
                | VBin::FMax
        );
        if float {
            // NEON flushes subnormals: ARM keeps floats scalar.
            return match self {
                Self::Arm => false,
                Self::S390x { ve1 } => w == 8 || (w == 4 && ve1),
                _ => matches!(w, 4 | 8),
            };
        }
        if matches!(
            op,
            VBin::Add | VBin::Sub | VBin::And | VBin::Or | VBin::Xor | VBin::AndNot
        ) {
            return true;
        }
        let minmax = matches!(op, VBin::MinS | VBin::MinU | VBin::MaxS | VBin::MaxU);
        if matches!(op, VBin::MulHiS | VBin::MulHiU) {
            // `pmulhw`; `smull` and `uzp2`; `extmul` and a shuffle.
            return match self {
                Self::X64 { .. } | Self::I686 => w == 2,
                Self::A64 | Self::S390x { .. } => w != 8,
                Self::Wasm => w == 2 || w == 4,
                Self::Rv => true,
                Self::Arm | Self::Ppc => false,
            };
        }
        match self {
            // AVX2: 32-bit `Mul`, 64-bit `Eq` / `GtS`, the SSE4.1 minima
            // and maxima; unsigned compares through `GtS`.
            Self::X64 { avx2: true, .. } => match op {
                VBin::Mul => w == 2 || w == 4,
                VBin::Eq | VBin::GtS => true,
                VBin::GtU => false,
                _ => minmax && w != 8,
            },
            // SSE2: `pmullw`, `pminub` / `pmaxub`, `pminsw` / `pmaxsw`.
            Self::X64 { .. } | Self::I686 => match op {
                VBin::Mul | VBin::MinS | VBin::MaxS => w == 2,
                VBin::Eq | VBin::GtS => w != 8,
                VBin::MinU | VBin::MaxU => w == 1,
                _ => false,
            },
            // No 64-bit `Mul`, nor (ARMv7) 64-bit compares or extremes.
            Self::A64 => match op {
                VBin::Eq | VBin::GtS | VBin::GtU => true,
                _ => w != 8,
            },
            Self::Arm => w != 8,
            // No 64-bit `GtU` nor 8-bit `Mul`.
            Self::Wasm => match op {
                VBin::Eq | VBin::GtS => true,
                VBin::Mul => w != 1,
                _ => w != 8,
            },
            Self::Ppc => !matches!(op, VBin::Mul) || w == 2 || w == 4,
            // `vml` has no 64-bit form.
            Self::S390x { .. } => !matches!(op, VBin::Mul) || w != 8,
            Self::Rv => true,
        }
    }

    /// Whether the vector unit fuses a multiply and an add of float
    /// lanes of `w` bytes (given the scalar unit does, `FnLower::fuses`).
    const fn fma(self, w: u32) -> bool {
        match self {
            Self::X64 { .. } | Self::A64 | Self::Ppc | Self::Rv => matches!(w, 4 | 8),
            Self::S390x { ve1 } => w == 8 || (w == 4 && ve1),
            Self::I686 | Self::Arm | Self::Wasm => false,
        }
    }

    /// Whether lanes of `w` bytes store under a mask (`vpmaskmov`, a
    /// V store under `v0`).
    const fn masked(self, w: u32) -> bool {
        matches!(self, Self::Rv)
            || (matches!(self, Self::X64 { avx2: true, .. }) && (w == 4 || w == 8))
    }

    /// Whether elements of `e` bytes load widened to lanes of `w`.
    const fn widen(self, w: u32, e: u32) -> bool {
        !matches!(self, Self::Ppc) && (w == 2 * e || w == 4 * e)
    }

    /// Whether lanes of `w` bytes store narrowed to elements of `e`.
    const fn narrow(self, w: u32, e: u32) -> bool {
        match self {
            // The packs of 256 bits work by halves: one step only.
            Self::X64 { avx2: true, .. } | Self::Wasm => w == 2 * e,
            Self::Ppc => false,
            _ => w == 2 * e || w == 4 * e,
        }
    }

    /// Bytes of a vector.
    pub const fn bytes(self) -> u32 {
        match self {
            Self::X64 { avx2: true, .. } => 32,
            _ => 16,
        }
    }

    const fn shift(self, op: VShift, w: u32) -> bool {
        match self {
            Self::X64 { .. } | Self::I686 => match op {
                VShift::Shl | VShift::ShrU => w != 1,
                VShift::ShrS => w == 2 || w == 4,
            },
            Self::A64 | Self::Wasm | Self::Arm | Self::Ppc | Self::S390x { .. } | Self::Rv => true,
        }
    }
}

/// Whether `+simd128` is on in a feature list (the last word wins).
pub fn simd128(features: &str) -> bool {
    let mut on = false;
    for f in features.split(',') {
        match f.trim() {
            "+simd128" => on = true,
            "-simd128" => on = false,
            _ => {}
        }
    }
    on
}

/// An edge into a block: the lanes that take it and its arguments.
type Edge = (Option<VReg>, Vec<(VReg, Form)>);

/// How a vector holds `bool`s.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Form {
    /// Values of the lane type.
    Num,
    /// All ones for `true`, zero for `false`.
    Mask,
    /// 1 for `true`, 0 for `false`.
    Bit,
}

/// A reduction: `acc = acc op x` once per iteration.
#[derive(Clone, Copy, Debug)]
enum Red {
    /// `+ & | ^`, wrapping: one register.
    Wrap(Bin),
    /// `-` wrapping: the sum of what is taken off.
    WrapSub,
    /// Checked `+` / `-` (`sub`): the positive parts (and, signed, the
    /// negative ones) and the lanes whose sums overflowed (sign bits).
    Checked { sub: bool, signed: bool },
    /// `+ 1` where a test holds (`checked` or not), counted in lanes
    /// narrower or wider than the accumulator.
    Count { checked: bool, signed: bool },
    /// The least or greatest element (`max`), each lane its own; of
    /// floats (a float `Acc::k`) none a NaN or a zero.
    MinMax { max: bool, signed: bool },
    /// `+` of products of byte elements widened (`signed` ones) into
    /// a 4-byte accumulator (`acc_signed`), four per lane; `checked`:
    /// with the sum of their magnitudes, which bounds every partial sum
    /// of the scalar loop.
    Dot {
        checked: bool,
        signed: bool,
        acc_signed: bool,
    },
}

/// A reduction of the plan: the local, its kind, the registers.
#[derive(Clone, Debug)]
pub(crate) struct Acc {
    local: LocalId,
    /// The accumulator's kind, and its lanes'.
    k: K,
    lane: K,
    red: Red,
    /// The value added each iteration.
    regs: Vec<VReg>,
}

/// A vector loop for one loop.
pub(crate) struct Plan {
    w: u32,
    /// Bytes of a vector.
    vb: u32,
    iv: LocalId,
    bound: Operand,
    /// The loop's test: its local and the value it has on exit.
    cond: (LocalId, bool),
    exit: (BlockId, Vec<Operand>),
    /// Done in vectors, the scalar body may be skipped: no value it
    /// computes is read after the loop.
    skip: bool,
    /// `(buffer, index, element bytes)` per base.
    bases: Vec<(LocalId, Idx, u32)>,
    /// Scalars splatted: `(operand, type, io offset)`.
    splats: Vec<(Operand, MirTy, u32)>,
    accs: Vec<Acc>,
    /// Local pairs that must hold different objects.
    distinct: Vec<(LocalId, LocalId)>,
    /// Index pairs into one buffer that must be equal or a vector's
    /// lanes apart.
    apart: Vec<(Idx, Idx)>,
    /// Loads of objects the loop reads but never writes, done once in
    /// front of it.
    hoist: Vec<(LocalId, RValue)>,
    /// `(l, idx)`: a side exit leaves where `idx >= l`.
    limits: Vec<(Operand, Idx)>,
    /// Indices are exact below `isize::MAX` only: the loop's bound and
    /// these offsets must be well below it.
    guard: Option<Vec<LocalId>>,
    /// Strides that must be 1 (`Forms::units`).
    units: Vec<LocalId>,
    /// Float accumulators that must hold no NaN on entry.
    nan_free: Vec<(LocalId, K)>,
    /// A strip's loop (`nabic_mir::strip`): the bound its test after the
    /// strip keeps to, which the vector loop may run up to.
    outer: Option<Operand>,
    pre: Vec<VOp>,
    body: Vec<VOp>,
    post: Vec<(VReg, u32)>,
    regs: u8,
    io_size: u32,
    save: u32,
    /// The first position of the loop, for the vector loop's line.
    line: Option<(u32, u32, u32)>,
}

/// One access of a buffer, in order.
pub(crate) struct Access {
    base: LocalId,
    inv: Option<LocalId>,
    off: u64,
    /// Bytes of the element.
    e: u32,
    store: bool,
}

/// Why a block is no vector loop; nothing reads it.
type No = ();

struct Builder<'b, 'c, 'a> {
    lw: &'b mut FnLower<'c, 'a>,
    isa: Isa,
    w: u32,
    checks: bool,
    next: VReg,
    pre: Vec<VOp>,
    body: Vec<VOp>,
    tail: Vec<VOp>,
    /// Vector values of locals the loop computes.
    vals: HashMap<LocalId, (VReg, Form)>,
    /// Splats of locals the loop reads but never writes.
    inv: HashMap<LocalId, (VReg, Form)>,
    consts: HashMap<u64, VReg>,
    splats: Vec<(Operand, MirTy)>,
    /// `(buffer, index, element bytes)` per base.
    bases: Vec<(LocalId, Idx, u32)>,
    accesses: Vec<Access>,
    /// Locals holding an index of the counter.
    ivoff: HashMap<LocalId, Idx>,
    iv: LocalId,
    /// Locals the loop assigns.
    written: HashSet<LocalId>,
    stored: bool,
    /// Registers holding for the whole loop.
    pinned: Vec<VReg>,
    accs: Vec<Acc>,
    /// The lanes the block at hand runs for (`None`: all).
    pred: Option<VReg>,
    cur: usize,
    /// Edges into blocks not started yet: their lanes and arguments.
    inbox: HashMap<usize, Vec<Edge>>,
    /// Locals holding a value for some lanes only: the blocks whose
    /// lanes they are (a block reads them where every way to it passes
    /// one of these).
    defs: HashMap<LocalId, Vec<usize>>,
    /// Edges between the loop's blocks, the back edge aside, and the
    /// header.
    succ: HashMap<usize, Vec<usize>>,
    top: usize,
    /// The blocks started, with their lanes.
    started: Vec<(usize, Option<VReg>)>,
    /// Locals holding a `bool` cast to an integer of another width for a
    /// count: masks.
    counts: HashSet<LocalId>,
    /// Updates of reductions, in order: `(op, value, constant, lanes)`.
    ups: Vec<(LocalId, PrimTy, Vec<Up>)>,
    /// Min / max reductions: `(local, max, signed, register)`.
    minmax: Vec<(LocalId, bool, bool, VReg)>,
    /// The float ones among them.
    fmm: HashSet<LocalId>,
    /// Lanes of bytes summed four at a time into 4-byte lanes (`Dot`):
    /// the byte elements widened (`signed`), their products.
    dot: bool,
    dotcast: HashMap<LocalId, (VReg, bool)>,
    dotmul: HashMap<LocalId, (VReg, VReg, bool)>,
    /// Masks that compare: `a > b`, signed, negated.
    gts: HashMap<VReg, (VReg, VReg, bool, bool)>,
    /// Masks of a float minimum's test: `a < b` (no NaN, no zero),
    /// negated.
    flts: HashMap<VReg, (VReg, VReg, bool)>,
    /// Stores for some lanes only, not done yet.
    pending: Vec<Pend>,
    /// `(buffer, inv, c)`: indices up to `iv + inv + c` that every lane
    /// may read, whatever way it takes.
    safe: Vec<(LocalId, Option<LocalId>, u64)>,
    /// Locals loaded in front holding what another one does.
    same: HashMap<LocalId, LocalId>,
    /// Elements narrower than the lanes, widened (`signed`): read by a
    /// cast only.
    widened: HashMap<LocalId, (VReg, bool)>,
    /// Values narrowed to `e` bytes: stored only.
    narrow: HashMap<LocalId, (VReg, u32)>,
}

/// An update of a reduction: `(op, value, constant value, lanes)`.
type Up = (PrimOp, VReg, Option<i128>, Option<VReg>);

/// A store some lanes do: merged across the ways that store at one
/// element, done where all of them meet.
struct Pend {
    buf: Operand,
    index: Operand,
    src: VReg,
    mask: VReg,
    blocks: Vec<usize>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simd128_follows_the_last_word() {
        assert!(simd128("+simd128"));
        assert!(simd128("+atomics,-simd128,+simd128"));
        assert!(!simd128("+simd128,+atomics,-simd128"));
        assert!(!simd128(""));
    }

    /// `x86_64` (SSE2, AVX2 where the CPU options say), `i686`, `aarch64`,
    /// `armv7` (NEON), `powerpc64le` (VSX) and `wasm32` (SIMD128) vectorize
    /// unless their feature is off; riscv64 (`v`) and s390x (`vector`) only
    /// with theirs on, the rest stay scalar.
    #[test]
    fn targets_that_vectorize() {
        let names = [
            "x86_64-unknown-linux-gnu",
            "x86_64-unknown-linux-musl",
            "x86_64-unknown-freebsd",
            "x86_64-pc-windows-msvc",
            "aarch64-apple-darwin",
            "aarch64-unknown-linux-gnu",
            "aarch64-unknown-linux-musl",
            "aarch64-pc-windows-msvc",
            "wasm32-unknown-unknown",
            "wasm32-wasip2",
            "wasm32-wasip1-threads",
            "i686-unknown-linux-gnu",
            "i686-pc-windows-msvc",
            "armv7-unknown-linux-gnueabihf",
            "riscv64gc-unknown-linux-gnu",
            "riscv32imac-unknown-none-elf",
            "powerpc64le-unknown-linux-gnu",
            "s390x-unknown-linux-gnu",
        ];
        for name in names {
            let t = Target::builtin(name);
            let want = match t.arch.as_str() {
                "x86_64" => Some(Isa::X64 {
                    win: t.os == "windows",
                    avx2: false,
                }),
                "aarch64" => Some(Isa::A64),
                "x86" => Some(Isa::I686),
                "arm" => Some(Isa::Arm),
                "powerpc64" => Some(Isa::Ppc),
                "wasm32" => Some(Isa::Wasm),
                _ => None,
            };
            assert_eq!(Isa::of(&t), want, "{name}");
        }
        let with = |name: &str, cpu: Option<&str>, f: &str| {
            let mut spec = nabic_mir::TargetSpec::builtin(name).expect("a target");
            let fs: Vec<String> = f
                .split(',')
                .filter(|x| !x.is_empty())
                .map(str::to_owned)
                .collect();
            spec.set_cpu(cpu, &fs).expect("a feature");
            Isa::of(&Target::new(spec))
        };
        assert_eq!(with("wasm32-wasip2", None, "-simd128"), None);
        assert_eq!(with("armv7-unknown-linux-gnueabihf", None, "-neon"), None);
        assert_eq!(with("powerpc64le-unknown-linux-gnu", None, "-vsx"), None);
        assert_eq!(with("s390x-unknown-linux-gnu", None, ""), None);
        assert_eq!(
            with("s390x-unknown-linux-gnu", Some("z13"), ""),
            Some(Isa::S390x { ve1: false })
        );
        assert_eq!(
            with("s390x-unknown-linux-gnu", None, "+vector-enhancements-1"),
            Some(Isa::S390x { ve1: true })
        );
        let avx2 = Some(Isa::X64 {
            win: false,
            avx2: true,
        });
        assert_eq!(with("x86_64-unknown-linux-gnu", None, "+avx2"), avx2);
        assert_eq!(
            with("x86_64-unknown-linux-gnu", Some("x86-64-v3"), ""),
            avx2
        );
        let sse2 = with("x86_64-unknown-linux-gnu", Some("haswell"), "-avx");
        assert_eq!(sse2.map(Isa::bytes), Some(16));
    }

    /// Accesses of one element in different lanes keep their order, or
    /// need two lists.
    #[test]
    fn order_of_accesses() {
        let (a, b) = (LocalId(1), LocalId(2));
        let acc = |base, off, store| Access {
            base,
            inv: None,
            off,
            e: 4,
            store,
        };
        // Load ahead, then store behind: as the scalar loop.
        assert_eq!(
            order(&[acc(a, 1, false), acc(a, 0, true)], 4),
            Some((Vec::new(), Vec::new()))
        );
        // Store ahead, then load behind: what the last element stored.
        assert_eq!(
            order(&[acc(a, 1, true), acc(a, 0, false)], 4),
            Some((Vec::new(), Vec::new()))
        );
        // Load behind, then store ahead (`a[i + 1] = a[i]`): one list
        // cannot.
        assert_eq!(order(&[acc(a, 0, false), acc(a, 1, true)], 4), None);
        // Two lists may be one: the test in front.
        assert_eq!(
            order(&[acc(a, 0, false), acc(b, 1, true)], 4),
            Some((vec![(a, b)], Vec::new()))
        );
        // A whole vector apart: no lane meets another.
        assert_eq!(
            order(&[acc(a, 0, false), acc(a, 4, true)], 4),
            Some((Vec::new(), Vec::new()))
        );
        // Rows of one buffer at offsets the loop never changes: a test
        // that they are a vector apart.
        let row = |inv, store| Access {
            base: a,
            inv: Some(LocalId(inv)),
            off: 0,
            e: 4,
            store,
        };
        let (r0, r1) = (
            Idx {
                inv: Some(LocalId(3)),
                c: 0,
            },
            Idx {
                inv: Some(LocalId(4)),
                c: 0,
            },
        );
        assert_eq!(
            order(
                &[row(3, false), row(4, false), row(3, true), row(4, true)],
                4
            ),
            Some((Vec::new(), vec![(r0, r1)]))
        );
    }
}
