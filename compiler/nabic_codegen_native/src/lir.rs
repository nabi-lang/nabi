//! Low-level IR between MIR and machine code. Every value lives in a frame
//! slot; instructions load operands into scratch registers, compute and
//! store. ISA modules only pick instructions; everything else is decided
//! here once for all targets.

use nabic_core::HashMap;

/// A frame slot (a local, or a temporary).
pub type Slot = u32;

/// A jump target within a function.
pub type Label = u32;

/// A symbol of the object being built (`obj::Obj::syms`).
pub type SymId = u32;

/// Scalar kind: the size of a value in memory and its register class.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum K {
    I8,
    I16,
    I32,
    I64,
    F32,
    F64,
}

impl K {
    #[must_use]
    pub const fn size(self) -> u32 {
        match self {
            Self::I8 => 1,
            Self::I16 => 2,
            Self::I32 | Self::F32 => 4,
            Self::I64 | Self::F64 => 8,
        }
    }

    #[must_use]
    pub const fn is_float(self) -> bool {
        matches!(self, Self::F32 | Self::F64)
    }

    #[must_use]
    pub const fn bits(self) -> u32 {
        self.size() * 8
    }
}

/// A memory location.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mem {
    /// Byte `off` of a frame slot.
    Frame(Slot, u32),
    /// Byte `off` past the pointer stored in a frame slot.
    Ind(Slot, i32),
}

impl Mem {
    #[must_use]
    pub const fn at(self, d: u32) -> Self {
        match self {
            Self::Frame(s, o) => Self::Frame(s, o + d),
            Self::Ind(s, o) => Self::Ind(s, o + d as i32),
        }
    }
}

/// A scalar operand: memory, or an immediate (a float's bits).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum V {
    M(Mem),
    I(i64),
}

/// Where a call argument / result / incoming parameter is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Loc {
    /// The `n`th integer argument / result register.
    Int(u8),
    /// The `n`th float argument / result register.
    Fp(u8),
    /// Byte offset in the outgoing (caller) / incoming (callee) argument
    /// area.
    Stack(u32),
    /// The hidden result pointer (`x8` on arm64).
    Sret,
    /// All 16 bytes of the `n`th float argument / result register: a C
    /// vector, moved through memory.
    Vec(u8),
    /// 16 bytes at this offset of the argument area: a C vector.
    VecStack(u32),
    /// A `Param` only: the address of byte offset `n` of the incoming
    /// argument area (a C variadic function's `va_start`).
    Area(u32),
}

/// A scalar crossing a call boundary: its location, kind, and whether a
/// narrow integer sign-extends (else zero-extends) into its register.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Part {
    pub loc: Loc,
    pub k: K,
    pub signed: bool,
}

impl V {
    /// The memory of a C vector part, which is never an immediate.
    #[must_use]
    pub fn vector_mem(self) -> Mem {
        match self {
            Self::M(m) => m,
            Self::I(_) => unreachable!("a C vector moves through memory"),
        }
    }
}

impl Part {
    /// The bytes it moves: a vector's 16, else its kind's.
    #[must_use]
    pub const fn size(self) -> u32 {
        if self.vector() { 16 } else { self.k.size() }
    }

    /// A C vector (`Loc::Vec`, `Loc::VecStack`).
    #[must_use]
    pub const fn vector(self) -> bool {
        matches!(self.loc, Loc::Vec(_) | Loc::VecStack(_))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Bin {
    Add,
    Sub,
    Mul,
    /// Division / remainder; the divisor is known non-zero (and not
    /// `MIN / -1`).
    Div,
    Rem,
    And,
    Or,
    Xor,
    /// Shift amount already checked below the width.
    Shl,
    Shr,
    /// Rotate left by the amount modulo the width; `I32` / `I64` only,
    /// where the target has one instruction for it (`FnLower::rotates`).
    Rotl,
    /// The high half of the full product (by `signed`), where the
    /// target has it (`FnLower::mul_hi`).
    MulHi,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Un {
    Neg,
    Not,
    Sqrt,
    Popcnt,
    Clz,
    Ctz,
    Bswap,
}

/// Comparison predicate (integer signedness in `Cmp::signed`; floats are
/// the ordered predicates, `Ne` unordered).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Cc {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl Cc {
    /// The predicate with its operands swapped.
    #[must_use]
    pub const fn swap(self) -> Self {
        match self {
            Self::Lt => Self::Gt,
            Self::Le => Self::Ge,
            Self::Gt => Self::Lt,
            Self::Ge => Self::Le,
            c => c,
        }
    }
}

/// A conversion between scalar kinds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Conv {
    /// Integer to integer: extend by `signed` from the source width, or
    /// truncate.
    Int { signed: bool },
    /// Integer to bool: `!= 0`.
    ToBool,
    /// Integer to float.
    IToF { signed: bool },
    /// Float to integer, saturating, NaN to 0; `bits` bits of the
    /// destination are the integer's (1 for `bool`).
    FToI { signed: bool, bits: u32 },
    /// Float to float.
    FToF,
    /// Bits reinterpreted (same size), int <-> float.
    Bits,
    /// IEEE binary16 bits (`I16`) to `F32` or back, to nearest even;
    /// only where the CPU converts (`FnLower::half_conv`).
    Half,
}

/// Memory ordering of an atomic access.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Order {
    Relaxed,
    Acquire,
    Release,
    AcqRel,
    SeqCst,
}

impl Order {
    /// The ordering an intrinsic names (`relaxed` .. `seq_cst`).
    #[must_use]
    pub fn parse(s: &str) -> Self {
        match s {
            "relaxed" => Self::Relaxed,
            "acquire" => Self::Acquire,
            "release" => Self::Release,
            "acq_rel" => Self::AcqRel,
            _ => Self::SeqCst,
        }
    }

    /// Later accesses stay after it.
    #[must_use]
    pub const fn acq(self) -> bool {
        matches!(self, Self::Acquire | Self::AcqRel | Self::SeqCst)
    }

    /// Earlier accesses stay before it.
    #[must_use]
    pub const fn rel(self) -> bool {
        matches!(self, Self::Release | Self::AcqRel | Self::SeqCst)
    }

    /// The weakest ordering at least as strong as both.
    #[must_use]
    pub const fn join(self, o: Self) -> Self {
        match (self.acq() || o.acq(), self.rel() || o.rel()) {
            _ if matches!(self, Self::SeqCst) || matches!(o, Self::SeqCst) => Self::SeqCst,
            (true, true) => Self::AcqRel,
            (true, false) => Self::Acquire,
            (false, true) => Self::Release,
            (false, false) => Self::Relaxed,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Rmw {
    Xchg,
    Add,
    Sub,
    And,
    Or,
    Xor,
}

#[derive(Clone, Debug)]
pub enum Callee {
    Sym(SymId),
    /// A code pointer in memory.
    Ptr(Mem),
}

#[derive(Clone, Debug)]
pub enum Inst {
    Label(Label),
    Jmp(Label),
    /// To `t` when the `I8` value is non-zero, else to `f`.
    Br {
        c: V,
        t: Label,
        f: Label,
    },
    Mov {
        k: K,
        dst: Mem,
        src: V,
    },
    Copy {
        dst: Mem,
        src: Mem,
        size: u32,
    },
    Zero {
        dst: Mem,
        size: u32,
    },
    /// The address of `src` (an `I64`).
    Addr {
        dst: Mem,
        src: Mem,
    },
    /// The address of a symbol; `got`: through the GOT (may be in
    /// another image).
    Sym {
        dst: Mem,
        sym: SymId,
        got: bool,
    },
    /// The address of this thread's instance of a thread-local symbol.
    Tls {
        dst: Mem,
        sym: SymId,
    },
    Bin {
        op: Bin,
        k: K,
        signed: bool,
        dst: Mem,
        a: V,
        b: V,
    },
    /// `+ - *` that jumps to `fail` on overflow (of `k`, by `signed`).
    Ovf {
        op: Bin,
        k: K,
        signed: bool,
        dst: Mem,
        a: V,
        b: V,
        fail: Label,
    },
    Un {
        op: Un,
        k: K,
        dst: Mem,
        a: V,
    },
    /// `dst: I8 = a <cc> b`.
    Cmp {
        cc: Cc,
        k: K,
        signed: bool,
        dst: Mem,
        a: V,
        b: V,
    },
    Conv {
        conv: Conv,
        from: K,
        to: K,
        dst: Mem,
        a: V,
    },
    /// `dst = a * b + c` (a float `k`) rounded once; only where the
    /// target fuses it (`FnLower::fma`).
    Fma {
        k: K,
        dst: Mem,
        a: V,
        b: V,
        c: V,
    },
    /// `ptr` holds the address. Each ISA fences as little as `ord` lets
    /// it (the usual C++ mappings).
    AtomicLoad {
        k: K,
        dst: Mem,
        ptr: Mem,
        ord: Order,
    },
    AtomicStore {
        k: K,
        ptr: Mem,
        src: V,
        ord: Order,
    },
    AtomicRmw {
        op: Rmw,
        k: K,
        dst: Mem,
        ptr: Mem,
        val: V,
        ord: Order,
    },
    /// `dst` = the old value; stores `new` when it was `expected`. `ord`
    /// orders it whether it stores or not.
    Cas {
        k: K,
        dst: Mem,
        ptr: Mem,
        expected: V,
        new: V,
        ord: Order,
    },
    /// Load the arguments into place, call, store the results.
    /// `sret`: the hidden result pointer's memory (passed by address).
    Call {
        callee: Callee,
        args: Vec<(Part, V)>,
        rets: Vec<(Part, Mem)>,
        sret: Option<Mem>,
        /// A C variadic call (`x86_64`: `al` = the vector registers used).
        variadic: bool,
        /// Bytes of stack arguments the callee pops (i386).
        pop: u32,
    },
    /// An incoming parameter (first, before anything else).
    Param {
        part: Part,
        dst: Mem,
    },
    /// Return, the results loaded into place.
    Ret {
        vals: Vec<(Part, V)>,
    },
    Trap,
    /// Source line, column and lexical scope of what follows (debug
    /// line info).
    Line(u32, u32, u32),
    /// A vectorized loop (`lower::vector`). To the rest of LIR it reads
    /// and writes its frame block (`VLoop::io`) whole and clobbers what a
    /// call clobbers.
    VLoop(Box<VLoop>),
    /// One operation on whole vectors (`core::simd`); clobbers nothing.
    V(Box<Vx>),
}

/// One function of LIR.
#[derive(Clone, Debug, Default)]
pub struct Func {
    pub sym: SymId,
    /// `(size, align)` of each slot.
    pub slots: Vec<(u32, u32)>,
    pub insts: Vec<Inst>,
    pub labels: u32,
    /// Bytes of stack arguments it pops on return (i386).
    pub pop: u32,
    /// `-g`: what the debug info says about it.
    pub debug: Option<FnDebug>,
    /// Every parameter part and the result's (`None`: the hidden result
    /// pointer), when lowering knows them: wasm32 types each function.
    pub sig: Option<(Vec<Part>, Option<Vec<Part>>)>,
    /// Above `-O0`: values may live in registers (`regalloc`).
    pub opt: bool,
    /// Above `-O0` with `-g` on a register machine: variables are where
    /// the code put them (`locs`), not kept in their slots for the
    /// debugger, and positions are not in the stream (`rows`).
    pub track: bool,
    /// With `track`, after `regalloc::prepare`: `(instruction, line,
    /// column, scope)` positions, each before its instruction.
    pub rows: Vec<(usize, u32, u32, u32)>,
    /// The shadow-stack frame's slot and the slots it roots.
    pub gc_frame: Option<(Slot, Vec<Slot>)>,
    /// Labels of rarely run blocks (slow paths of polls, allocation,
    /// barriers).
    pub cold: Vec<Label>,
    /// Pointer arguments its direct calls pass that the callee keeps
    /// no longer than the call, known from the convention (by-address
    /// arguments and results).
    pub byref: Vec<(SymId, Loc, Cap)>,
    /// Above `-O0`: those of every callee, by symbol, the convention's
    /// and what the bodies of the object show (`frame::caps`, which
    /// takes over the object's `byref`).
    pub caps: Option<std::sync::Arc<Caps>>,
    /// Functions it calls that are `#[cold]`.
    pub cold_fns: Vec<SymId>,
    /// A C variadic definition where the argument registers must sit
    /// right below the stack arguments (32-bit ARM, RISC-V): bytes the
    /// prologue stores them in first (`Loc::Area` counts from there).
    pub va_save: u32,
}

/// What a callee does with a pointer argument it does not keep.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Cap {
    /// Only reads and writes through it.
    No,
    /// May also return it, or a pointer derived from it.
    Ret,
}

/// Per callee symbol, its pointer parameters it keeps no longer than
/// the call.
pub type Caps = HashMap<SymId, Vec<(Loc, Cap)>>;

/// `-g` facts of a function.
#[derive(Clone, Debug)]
pub struct FnDebug {
    /// The nabi path.
    pub name: String,
    /// `Span::file` (1-based; 0: the unit's file) and line of its
    /// declaration.
    pub file: u32,
    pub line: u32,
    /// Lexical scopes other than the body (0): `(id, parent)`.
    pub scopes: Vec<(u32, u32)>,
    pub vars: Vec<DebugVar>,
    /// `MirFunction::debug_ret`.
    pub ret: Option<nabic_ir_common::PrimTy>,
    /// Optimized (`Func::track`): variables by code range.
    pub opt: bool,
    /// Bodies inlined, each a scope of `scopes`.
    pub inlined: Vec<InlineSite>,
}

impl FnDebug {
    /// The innermost inlined body scope `s` is in.
    #[must_use]
    pub fn site(&self, mut s: u32) -> Option<&InlineSite> {
        for _ in 0..=self.scopes.len() {
            if s == 0 {
                return None;
            }
            if let Some(i) = self.inlined.iter().find(|i| i.scope == s) {
                return Some(i);
            }
            s = self.scopes.iter().find(|x| x.0 == s).map_or(0, |x| x.1);
        }
        None
    }

    /// The source file of positions in scope `s` (`Span::file`).
    #[must_use]
    pub fn file_of(&self, s: u32) -> u32 {
        self.site(s).map_or(self.file, |i| i.file)
    }
}

/// A body inlined into a function (`nabic_mir::Inlined`).
#[derive(Clone, Debug)]
pub struct InlineSite {
    pub scope: u32,
    pub parent: u32,
    /// The callee's nabi path, file and line.
    pub name: String,
    pub file: u32,
    pub line: u32,
    /// The call's line and column (in the parent scope's file).
    pub call_line: u32,
    pub call_col: u32,
}

/// No slot: a variable whose value is a constant or gone.
pub const NO_SLOT: Slot = u32::MAX;

/// A named local in its stack slot.
#[derive(Clone, Debug)]
pub struct DebugVar {
    pub name: String,
    /// Its slot, or [`NO_SLOT`] (only with `Func::track`).
    pub slot: Slot,
    pub ty: nabic_mir::DebugTy,
    pub line: u32,
    pub scope: u32,
    /// 1-based parameter position.
    pub arg: Option<usize>,
    /// A parameter: of the function (`arg`) or of a body inlined.
    pub param: bool,
    /// Its value, a constant's bits (`NO_SLOT`).
    pub konst: Option<u64>,
    /// Its value, an aggregate built of these `(offset, size, part)`
    /// (`NO_SLOT`).
    pub parts: Vec<(u32, u32, PartSrc)>,
}

/// Where a part of a variable built in place is (`DebugVar::parts`).
#[derive(Clone, Copy, Debug)]
pub enum PartSrc {
    Slot(Slot),
    Const(u64),
    Gone,
}

impl Func {
    /// A copy of all but the instructions, which are `insts`.
    #[must_use]
    pub fn with_insts(&self, insts: Vec<Inst>) -> Self {
        Self {
            sym: self.sym,
            slots: self.slots.clone(),
            insts,
            labels: self.labels,
            pop: self.pop,
            debug: self.debug.clone(),
            sig: self.sig.clone(),
            opt: self.opt,
            track: self.track,
            rows: self.rows.clone(),
            gc_frame: self.gc_frame.clone(),
            cold: self.cold.clone(),
            byref: self.byref.clone(),
            caps: self.caps.clone(),
            cold_fns: self.cold_fns.clone(),
            va_save: self.va_save,
        }
    }

    pub fn slot(&mut self, size: u32, align: u32) -> Slot {
        self.slots.push((size.max(1), align.max(1)));
        self.slots.len() as Slot - 1
    }

    pub const fn label(&mut self) -> Label {
        self.labels += 1;
        self.labels - 1
    }

    /// Append `i`, folding arithmetic on two immediates or with an identity
    /// (`x + 0`, `x * 1`) into a move.
    pub fn push(&mut self, i: Inst) {
        if let Inst::Bin {
            op, k, dst, a, b, ..
        } = i
        {
            if !k.is_float() {
                let src = match (op, a, b) {
                    (Bin::Add, V::I(x), V::I(y)) => Some(V::I(x.wrapping_add(y))),
                    (Bin::Sub, V::I(x), V::I(y)) => Some(V::I(x.wrapping_sub(y))),
                    (Bin::Mul, V::I(x), V::I(y)) => Some(V::I(x.wrapping_mul(y))),
                    (Bin::And, V::I(x), V::I(y)) => Some(V::I(x & y)),
                    (Bin::Or, V::I(x), V::I(y)) => Some(V::I(x | y)),
                    (Bin::Xor, V::I(x), V::I(y)) => Some(V::I(x ^ y)),
                    (Bin::Add | Bin::Or | Bin::Xor, V::I(0), v)
                    | (
                        Bin::Add | Bin::Sub | Bin::Or | Bin::Xor | Bin::Shl | Bin::Shr,
                        v,
                        V::I(0),
                    )
                    | (Bin::Mul, V::I(1), v)
                    | (Bin::Mul, v, V::I(1)) => Some(v),
                    _ => None,
                };
                if let Some(src) = src {
                    self.insts.push(Inst::Mov { k, dst, src });
                    return;
                }
            }
        }
        self.insts.push(i);
    }
}

/// One operation on vectors of `vb` bytes, lanes of `w` bytes. A vector
/// operand is a frame part of `vb` bytes, which a vector register may
/// hold, or `vb` bytes through a pointer (`Mem::Ind`, at most one per
/// instruction).
#[derive(Clone, Debug)]
pub struct Vx {
    pub op: VxOp,
    pub w: u32,
    pub vb: u32,
    pub dst: Mem,
    pub a: Mem,
    pub b: Mem,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VxOp {
    /// `dst = a`: a load or a store when either is `Ind`.
    Mov,
    Bin(VBin),
    Un(VUn),
    /// Every lane the low `w` bytes of the bits.
    Const(u64),
    /// Every lane the scalar `a` of this kind (`w` bytes).
    Splat(K),
    /// `a` with lane `n` the scalar `b` of this kind.
    Insert(K, u8),
    /// The scalar `dst` of this kind lane `n` of `a`.
    Lane(K, u8),
    /// The scalar `dst` of this kind the wrapping sum of `a`'s lanes.
    Sum(K),
}

impl Vx {
    /// The vector operands it reads.
    pub fn srcs(&self) -> impl Iterator<Item = Mem> {
        let n = match self.op {
            VxOp::Bin(_) => 2,
            VxOp::Mov | VxOp::Un(_) | VxOp::Insert(..) | VxOp::Lane(..) | VxOp::Sum(_) => 1,
            VxOp::Const(_) | VxOp::Splat(_) => 0,
        };
        [self.a, self.b].into_iter().take(n)
    }

    /// Its scalar operand: `(the part, its kind)`.
    #[must_use]
    pub const fn scalar(&self) -> Option<(Mem, K)> {
        match self.op {
            VxOp::Splat(k) => Some((self.a, k)),
            VxOp::Insert(k, _) => Some((self.b, k)),
            _ => None,
        }
    }

    /// Its scalar result's kind (`Lane`), else `None`: a vector.
    #[must_use]
    pub const fn scalar_dst(&self) -> Option<K> {
        match self.op {
            VxOp::Lane(k, _) | VxOp::Sum(k) => Some(k),
            _ => None,
        }
    }

    /// Bytes of `dst` it writes.
    #[must_use]
    pub const fn dst_size(&self) -> u32 {
        match self.scalar_dst() {
            Some(k) => k.size(),
            None => self.vb,
        }
    }

    /// Whether it reads `a`, and `b`.
    #[must_use]
    pub fn reads(&self) -> (bool, bool) {
        let n = self.srcs().count();
        (
            n > 0 || matches!(self.op, VxOp::Splat(_)),
            n > 1 || matches!(self.op, VxOp::Insert(..)),
        )
    }
}

/// A vector register of a [`VLoop`], numbered from 0 (the emitter maps
/// the numbers to its registers).
pub type VReg = u8;

/// Byte offsets in `VLoop::io`: the counter, the bound, then a word per
/// base (`VLoop::bases`), the splatted scalars and the results.
pub const VIO_IV: u32 = 0;
pub const VIO_BOUND: u32 = 8;
pub const VIO_BASES: u32 = 16;

/// A vectorized counted loop over registers of `vb / w` lanes of `w`
/// bytes. With the counter `iv` and the bound in `io`, each iteration
/// runs `body` on the lanes of elements `iv .. iv + vb / w` while `iv +
/// min_left <= bound` (unsigned) and no `VOp::Bail` leaves, then adds
/// `vb / w` to `iv`. Before the first iteration `pre` sets up
/// the registers that hold for the whole loop; after the last, `post`
/// stores registers to `io` and the counter goes back to `io`.
/// Nothing in it reaches a safepoint.
#[derive(Clone, Debug)]
pub struct VLoop {
    /// A frame slot (16-byte aligned), read and written through memory.
    pub io: Slot,
    /// Lane width in bytes: 1, 2, 4 or 8.
    pub w: u32,
    /// Bytes of a vector: 16, or 32 (`x86_64` with AVX2).
    pub vb: u32,
    /// A vector iteration runs while `iv + min_left <= bound`.
    pub min_left: u32,
    /// Per base: the byte added to the word at `VIO_BASES + 8 * n`; lane
    /// `j` of an access is at `base + disp + (iv + j) * e`, `e` its
    /// element's bytes (`elem`).
    pub bases: Vec<i32>,
    /// Per base: the bytes of its elements, `w` or less (`LoadExt`,
    /// `StoreTrunc`).
    pub elem: Vec<u32>,
    pub pre: Vec<VOp>,
    pub body: Vec<VOp>,
    /// `(register, io offset)`: stored once the loop is done.
    pub post: Vec<(VReg, u32)>,
    /// Registers used: `0 .. regs`.
    pub regs: u8,
    /// Bytes of `io` from this offset are the emitter's (callee-saved
    /// vector registers it uses, Windows x64).
    pub save: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VOp {
    /// 16 bytes from base `base` at the counter.
    Load {
        dst: VReg,
        base: u8,
    },
    Store {
        src: VReg,
        base: u8,
    },
    /// `Load` of elements narrower than `w`, each extended to its lane
    /// (`signed`: with its sign).
    LoadExt {
        dst: VReg,
        base: u8,
        signed: bool,
    },
    /// `Store` of the low bytes of each lane to elements narrower than
    /// `w`.
    StoreTrunc {
        src: VReg,
        base: u8,
    },
    /// `Store` of the lanes whose `mask` lane is all ones; the others
    /// are neither read nor written.
    MaskStore {
        src: VReg,
        mask: VReg,
        base: u8,
    },
    /// Each lane = the low `w` bytes at `io` offset `off`.
    Splat {
        dst: VReg,
        off: u32,
    },
    /// Each lane = the low `w` bytes of `bits`.
    Const {
        dst: VReg,
        bits: u64,
    },
    Bin {
        op: VBin,
        dst: VReg,
        a: VReg,
        b: VReg,
    },
    Un {
        op: VUn,
        dst: VReg,
        a: VReg,
    },
    /// Each lane shifted by `n` (below its width).
    Shift {
        op: VShift,
        dst: VReg,
        a: VReg,
        n: u8,
    },
    /// Leave before this iteration does anything more when the sign bit
    /// of some lane of `a` is set. Never after a `Store`.
    Bail {
        a: VReg,
    },
    /// Each 4-byte lane of `acc` plus the products of the four byte
    /// lanes of `a` and `b` in it, `signed` or not (arm64 `sdot` /
    /// `udot`, the loop's lanes bytes).
    Dot {
        acc: VReg,
        a: VReg,
        b: VReg,
        signed: bool,
    },
    /// Each float lane `a * b + c`, rounded once (`Isa::fma`).
    Fma {
        dst: VReg,
        a: VReg,
        b: VReg,
        c: VReg,
    },
}

/// Lane-wise binary operations; integer ones wrap. `Eq` .. `GtU` and the
/// float comparisons give all ones (true) or zero per lane; float ones
/// are ordered but `FNe`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VBin {
    Add,
    Sub,
    Mul,
    And,
    Or,
    Xor,
    /// `a & !b`.
    AndNot,
    Eq,
    GtS,
    GtU,
    FAdd,
    FSub,
    FMul,
    FDiv,
    FEq,
    FNe,
    FLt,
    FLe,
    MinS,
    MinU,
    MaxS,
    MaxU,
    /// The lesser / greater of two lanes neither of which is a NaN; of
    /// equal ones (`0.0` and `-0.0`) either.
    FMin,
    FMax,
    /// The high half of each lane's full product, signed / unsigned.
    MulHiS,
    MulHiU,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VUn {
    FSqrt,
    /// All ones where the lane is negative (as a signed integer), else
    /// zero.
    SignMask,
    /// The lane's magnitude (of the least integer, as unsigned).
    Abs,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VShift {
    Shl,
    ShrU,
    ShrS,
}

/// The integer kind `size` bytes make, if one does.
pub const fn scalar(size: u32) -> Option<K> {
    match size {
        1 => Some(K::I8),
        2 => Some(K::I16),
        4 => Some(K::I32),
        8 => Some(K::I64),
        _ => None,
    }
}

/// `i` scales an `I64` index by `2^s` (`s <= max`) into a slot part that
/// only `next`, an `I64` add of it to another slot part, reads: `(the part,
/// the index, s)`. The add can scale the index itself.
#[must_use]
pub fn scaled(i: &Inst, next: Option<&Inst>, waits: &Waits, max: u32) -> Option<(Mem, V, u32)> {
    let Inst::Bin {
        op,
        k: K::I64,
        dst: Mem::Frame(ds, doff),
        a: idx @ V::M(_),
        b: V::I(c),
        ..
    } = i
    else {
        return None;
    };
    let s = match op {
        Bin::Mul if *c > 0 && (*c as u64).is_power_of_two() => c.trailing_zeros(),
        Bin::Shl if (0..64).contains(c) => *c as u32,
        _ => return None,
    };
    let t = Mem::Frame(*ds, *doff);
    if s > max || waits.get(&(*ds, *doff)) != Some(&(8, 1)) || *idx == V::M(t) {
        return None;
    }
    match next {
        Some(Inst::Bin {
            op: Bin::Add,
            k: K::I64,
            a,
            b,
            ..
        }) if (*a == V::M(t) && matches!(b, V::M(m) if *m != t))
            || (*b == V::M(t) && matches!(a, V::M(m) if *m != t)) =>
        {
            Some((t, *idx, s))
        }
        _ => None,
    }
}

/// `(slot, offset)` -> `(size, reads)` (`waitable`).
pub type Waits = HashMap<(u32, u32), (u32, u32)>;

/// How a slot is accessed.
#[derive(Default)]
struct Use {
    /// Reached by address, or accessed whole (a copy).
    opaque: bool,
    /// `(offset, size, reads, writes)` of its scalar parts, by offset.
    parts: std::collections::BTreeMap<u32, (u32, u32, u32, u32)>,
}

impl Use {
    /// The part at `off`, accessed as `size` bytes: one size per part,
    /// else the slot is opaque.
    fn part(&mut self, off: u32, size: u32) -> &mut (u32, u32, u32, u32) {
        let p = self.parts.entry(off).or_insert((off, size, 0, 0));
        self.opaque |= p.1 != size;
        p
    }
}

/// Scalar slot parts whose store may wait (`Pend`), `(slot, offset)` ->
/// `(size, reads)`: written once, read at most once as the same bytes, in a
/// slot with non-overlapping parts, never addressed, copied whole or
/// described to the debugger. Also: which slots are addressed.
pub fn waitable(f: &Func) -> (Waits, Vec<bool>) {
    fn rd(u: &mut [Use], m: Mem, size: u32) {
        match m {
            Mem::Frame(s, o) => u[s as usize].part(o, size).2 += 1,
            // The pointer, at offset 0.
            Mem::Ind(s, _) => u[s as usize].part(0, 8).2 += 1,
        }
    }
    fn rv(u: &mut [Use], v: V, size: u32) {
        if let V::M(m) = v {
            rd(u, m, size);
        }
    }
    fn wr(u: &mut [Use], m: Mem, size: u32) {
        match m {
            Mem::Frame(s, o) => u[s as usize].part(o, size).3 += 1,
            Mem::Ind(..) => rd(u, m, 8),
        }
    }
    fn whole(u: &mut [Use], e: &mut [bool], m: Mem, escapes: bool) {
        match m {
            Mem::Frame(s, _) => {
                u[s as usize].opaque = true;
                e[s as usize] |= escapes;
            }
            Mem::Ind(..) => rd(u, m, 8),
        }
    }
    let n = f.slots.len();
    let mut u: Vec<Use> = (0..n).map(|_| Use::default()).collect();
    let mut e = vec![false; n];
    for i in &f.insts {
        match i {
            Inst::Label(_) | Inst::Jmp(_) | Inst::Trap | Inst::Line(..) => {}
            Inst::VLoop(v) => whole(&mut u, &mut e, Mem::Frame(v.io, 0), false),
            Inst::V(x) => {
                for m in x.srcs() {
                    rd(&mut u, m, x.vb);
                }
                if let Some((m, k)) = x.scalar() {
                    rd(&mut u, m, k.size());
                }
                wr(&mut u, x.dst, x.dst_size());
            }
            Inst::Br { c, .. } => rv(&mut u, *c, 1),
            Inst::Mov { k, dst, src } => {
                rv(&mut u, *src, k.size());
                wr(&mut u, *dst, k.size());
            }
            Inst::Copy { dst, src, size } => {
                if scalar(*size).is_some() {
                    rd(&mut u, *src, *size);
                    wr(&mut u, *dst, *size);
                } else {
                    whole(&mut u, &mut e, *src, false);
                    whole(&mut u, &mut e, *dst, false);
                }
            }
            Inst::Zero { dst, .. } => whole(&mut u, &mut e, *dst, false),
            Inst::Sym { dst, .. } | Inst::Tls { dst, .. } => wr(&mut u, *dst, 8),
            Inst::Param { part, dst } => wr(&mut u, *dst, part.size()),
            Inst::Addr { dst, src } => {
                whole(&mut u, &mut e, *src, true);
                wr(&mut u, *dst, 8);
            }
            Inst::Bin { k, dst, a, b, .. } | Inst::Ovf { k, dst, a, b, .. } => {
                rv(&mut u, *a, k.size());
                rv(&mut u, *b, k.size());
                wr(&mut u, *dst, k.size());
            }
            Inst::Cmp { k, dst, a, b, .. } => {
                rv(&mut u, *a, k.size());
                rv(&mut u, *b, k.size());
                wr(&mut u, *dst, 1);
            }
            Inst::Un { k, dst, a, .. } => {
                rv(&mut u, *a, k.size());
                wr(&mut u, *dst, k.size());
            }
            Inst::Fma { k, dst, a, b, c } => {
                rv(&mut u, *a, k.size());
                rv(&mut u, *b, k.size());
                rv(&mut u, *c, k.size());
                wr(&mut u, *dst, k.size());
            }
            Inst::Conv {
                from, to, dst, a, ..
            } => {
                rv(&mut u, *a, from.size());
                wr(&mut u, *dst, to.size());
            }
            Inst::AtomicLoad { k, dst, ptr, .. } => {
                whole(&mut u, &mut e, *ptr, true);
                wr(&mut u, *dst, k.size());
            }
            Inst::AtomicStore { k, ptr, src, .. } => {
                whole(&mut u, &mut e, *ptr, true);
                rv(&mut u, *src, k.size());
            }
            Inst::AtomicRmw {
                k, dst, ptr, val, ..
            } => {
                whole(&mut u, &mut e, *ptr, true);
                rv(&mut u, *val, k.size());
                wr(&mut u, *dst, k.size());
            }
            Inst::Cas {
                k,
                dst,
                ptr,
                expected,
                new,
                ..
            } => {
                whole(&mut u, &mut e, *ptr, true);
                rv(&mut u, *expected, k.size());
                rv(&mut u, *new, k.size());
                wr(&mut u, *dst, k.size());
            }
            Inst::Call {
                callee,
                args,
                rets,
                sret,
                ..
            } => {
                if let Callee::Ptr(m) = callee {
                    rd(&mut u, *m, 8);
                }
                for (p, v) in args {
                    rv(&mut u, *v, p.size());
                }
                for (p, m) in rets {
                    wr(&mut u, *m, p.size());
                }
                if let Some(m) = sret {
                    whole(&mut u, &mut e, *m, true);
                }
            }
            Inst::Ret { vals } => {
                for (p, v) in vals {
                    rv(&mut u, *v, p.size());
                }
            }
        }
    }
    for v in f.debug.iter().filter(|_| !f.track).flat_map(|d| &d.vars) {
        u[v.slot as usize].opaque = true;
    }
    let mut out = HashMap::default();
    for (s, use_) in u.iter().enumerate() {
        let ps: Vec<_> = use_.parts.values().collect();
        // Every part written; no two overlapping (by offset: no two
        // neighbours).
        let clean = !use_.opaque
            && ps.iter().all(|p| p.3 > 0)
            && ps.windows(2).all(|w| w[0].0 + w[0].1 <= w[1].0);
        if clean {
            for p in ps.iter().filter(|p| p.3 == 1 && p.2 <= 1) {
                out.insert((s as u32, p.0), (p.1, p.2));
            }
        }
    }
    (out, e)
}
