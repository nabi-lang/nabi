//! C ABI lowering of `extern "C"` aggregate parameters and results.
//!
//! As clang does, the front end rewrites a by-value `struct` into what the
//! platform C ABI uses and converts through memory at the call site; the
//! shapes are [`CType`]s (`[2 x i64]`, `{i64, f64}`), which the native
//! backend's `cc` places in registers and stack slots.
//!
//! - AAPCS64 (Linux / Windows / Apple arm64): a homogeneous float
//!   aggregate (HFA, 1-4 members of one float type) goes in `v` registers
//!   as `[n x fT]`; others up to 16 bytes as `i64` / `[2 x i64]` (results
//!   <= 8 bytes as `iN`); bigger ones by pointer to a caller copy, returned
//!   through `x8` (`sret`). Apple differs only for variadics (all on the
//!   stack, placed by `cc`) and small-integer extension; Windows passes
//!   a variadic callee's fixed HFAs as integers and a 16-aligned 16-byte
//!   aggregate as an `i128` (an even register pair; on Linux when its
//!   fields are 16-aligned, Apple never).
//! - x86-64 System V: each eightbyte of an aggregate up to 16 bytes is SSE
//!   (`f32` / `f64` / two `f32` in one register) or INTEGER (`iN`) in
//!   `xmm` / GPRs (short of registers, or with an unaligned field, the whole
//!   aggregate is MEMORY), > 16 bytes are MEMORY (`byval` / `sret`).
//! - Windows x64: 1 / 2 / 4 / 8-byte aggregates as `iN`, others by
//!   reference / `sret`.
//! - i386 System V and Windows x86 (cdecl): aggregates are copied onto
//!   the stack (`byval`, 4-aligned; on Windows one with a `repr(align)`
//!   past 4 by reference unless stdcall, as MSVC); results come back
//!   through `sret`
//!   (the System V callee pops its pointer), except Windows's
//!   1 / 2 / 4 / 8-byte ones, in `eax` / `edx:eax`.
//! - 32-bit ARM AAPCS-VFP: an HFA goes in `s` / `d` registers as
//!   `[n x fT]`; other aggregates up to 64 bytes as `[n x i32]` (`i64`
//!   when 8-aligned without a `repr(align)` of their own: an even
//!   register pair) through `r0`-`r3` and the
//!   stack, bigger ones `byval`; results <= 4 bytes in `r0`, HFAs in
//!   VFP registers (not for a C variadic callee), others through `sret`.
//! - WebAssembly: an aggregate of one scalar (no padding) as that scalar,
//!   others as a pointer to a caller copy (not `byval`, as clang) / `sret`.
//! - RISC-V LP64D: a struct of one float, two floats, or a float and an
//!   integer of at most 8 bytes (flattened: nested structs and arrays,
//!   any offsets, no union) goes in `fa` / `a` registers as `f32` /
//!   `f64` / `{t1, t2}` (fields at their own offsets when padded or
//!   packed) while enough of both are left; other aggregates up to 16
//!   bytes as `i64` / `[2 x i64]` (`i128` when 16-aligned), split between
//!   the last `a` register and the stack as clang splits them; bigger
//!   ones by reference / `sret`.
//! - RISC-V ILP32 (soft float, riscv32): the same integer rules at XLEN
//!   32: up to 4 bytes as `i32`, up to 8 as `[2 x i32]` (`i64` when
//!   8-aligned), bigger ones by reference / `sret`; floats are integers.
//! - powerpc64 `ELFv2`: a homogeneous float aggregate (1-8 members) goes in
//!   `f` registers as `[n x fT]`; others up to 8 bytes as `i64`, larger
//!   ones as `[n x i64]` (`[n x i128]` from an even doubleword when
//!   16-aligned) through `r3`-`r10` and the parameter save area
//!   (clang passes those over 64 bytes `byval`, the same words); results
//!   up to 16 bytes in `r3 r4`, HFAs in `f1`-`f8`, others through `sret`.
//! - s390x: an aggregate of 1, 2, 4 or 8 bytes as that integer (a lone
//!   float as itself, through nested structs but not arrays), others by
//!   reference; aggregate results always through `sret`.
//!
//! A union is classified by its members (overlaid at its offset), as
//! clang does; s390x passes one as an integer. `f16` / `bf16` are
//! `_Float16` / `__bf16`, in the low bits of a float register where C
//! has them (`c_refusal` names the rest), and float members of
//! aggregates. A `Simd` of 8 or 16 bytes is a C vector on AAPCS64 and
//! x86-64 (`float32x4_t`, `__m128`): a `v` / `xmm` register, by
//! reference as a Windows x64 argument.
//!
//! Other targets reject aggregates across the C boundary.

use nabic_ir_common::PrimTy;
use nabic_mir::{CAbi, CKind, MirTy, Target};

/// A scalar of a [`CType`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CScalar {
    /// An integer of this many bits (`i24`: the first three bytes).
    Int(u32),
    F32,
    F64,
    /// Two `f32` in one SSE register (System V `<2 x float>`).
    F32x2,
    Ptr,
    /// `_Float16` / `__bf16`: two bytes in the low bits of a float
    /// register (NaN-boxed on RISC-V).
    F16,
    /// A 16-byte vector in one `v` / `xmm` register.
    V128,
    /// An 8-byte vector in the low half of one.
    V64,
}

/// The C type a value's bits are reinterpreted as to cross the C ABI.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CType {
    Scalar(CScalar),
    /// `n` elements in consecutive registers (`[n x T]`).
    Array(u32, CScalar),
    /// A struct of naturally aligned fields (`{A, B}`).
    Struct(Vec<CScalar>),
    /// Fields at these byte offsets: a struct with padding before or
    /// between them (`{[n x i8], A, [m x i8], B}`), or packed.
    At(Vec<(u32, CScalar)>),
}

impl CType {
    pub const PTR: Self = Self::Scalar(CScalar::Ptr);

    /// An integer of `bits` bits.
    #[must_use]
    pub const fn int(bits: u32) -> Self {
        Self::Scalar(CScalar::Int(bits))
    }
}

/// How one parameter crosses the C boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ArgAbi {
    /// As its own type (scalars).
    Direct,
    /// Bits reinterpreted as this C type (through memory).
    Coerce(CType),
    /// A pointer to a caller-owned copy; `byval` makes it an on-stack
    /// copy (System V MEMORY class) rather than a plain pointer argument.
    Indirect { byval: bool },
}

/// How the result comes back.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RetAbi {
    Direct,
    Coerce(CType),
    /// Caller passes a hidden `sret` pointer first; nothing is returned.
    Sret,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(clippy::struct_excessive_bools)] // independent facts, not a state
pub struct FnAbi {
    pub ret: RetAbi,
    pub args: Vec<ArgAbi>,
    /// C variadic: arguments past `args` are its `...`.
    pub variadic: bool,
    /// A runtime (`extern "nabi"`) call: `str` crosses as its `(ptr, len)`
    /// pair, not as a `char*`.
    pub nabi: bool,
    /// `extern "system"` where it is `stdcall` (32-bit x86 Windows): the
    /// callee pops its arguments; the symbol is decorated `_f@N`.
    pub stdcall: bool,
    /// `#[errno]`: errno is saved right after the call.
    pub errno: bool,
    /// `#[leaf]`: the thread does not park for the call.
    pub leaf: bool,
}

/// A by-value aggregate (as opposed to a scalar, `str` or `()`).
#[must_use]
pub const fn is_aggregate(ty: &MirTy) -> bool {
    matches!(
        ty,
        MirTy::Struct(..) | MirTy::Tuple(_) | MirTy::Array(..) | MirTy::Union(..)
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
enum Leaf {
    Int,
    F32,
    F64,
    /// `f16` / `bf16`.
    Half,
    /// A `Simd` of 8 or 16 bytes.
    Vec,
}

impl Leaf {
    /// The C scalar of a `size`-byte leaf.
    const fn scalar(self, size: u32) -> CScalar {
        match self {
            Self::Int => CScalar::Int(size * 8),
            Self::F32 => CScalar::F32,
            Self::F64 => CScalar::F64,
            Self::Half => CScalar::F16,
            Self::Vec if size == 16 => CScalar::V128,
            Self::Vec => CScalar::V64,
        }
    }
}

/// Scalar leaves of `ty` at their byte offsets (`str` is two words), a
/// union's members overlaid.
fn leaves(ty: &MirTy, at: u32, target: &Target, out: &mut Vec<(u32, u32, Leaf)>) {
    let word = target.pointer_width / 8;
    match ty {
        MirTy::Unit => {}
        MirTy::Prim(PrimTy::F32) => out.push((at, 4, Leaf::F32)),
        MirTy::Prim(PrimTy::F64) => out.push((at, 8, Leaf::F64)),
        MirTy::Prim(PrimTy::Str) => {
            out.push((at, word, Leaf::Int));
            out.push((at + word, word, Leaf::Int));
        }
        MirTy::Prim(_) | MirTy::Any | MirTy::GcRef | MirTy::Fn(..) => {
            out.push((at, target.size_align(ty).0, Leaf::Int));
        }
        MirTy::Struct(..) if half(ty, target) => out.push((at, 2, Leaf::Half)),
        MirTy::Struct(id, _) if target.repr(*id).c_kind == CKind::Simd => {
            out.push((at, target.size_align(ty).0, Leaf::Vec));
        }
        MirTy::Struct(id, _) if target.union_members(*id).is_some() => {
            for m in target.union_members(*id).unwrap_or_default() {
                leaves(m, at, target, out);
            }
        }
        MirTy::Tuple(ps) | MirTy::Struct(_, ps) => {
            for (p, off) in ps.iter().zip(target.field_offsets(ty)) {
                leaves(p, at + off, target, out);
            }
        }
        MirTy::Array(e, n) => {
            let s = target.size_align(e).0;
            for i in 0..*n {
                leaves(e, at + i * s, target, out);
            }
        }
        MirTy::Union(_, n, _) => {
            for i in 0..*n {
                out.push((at + i * word, word, Leaf::Int));
            }
        }
    }
}

/// `f16` / `bf16`.
fn half(ty: &MirTy, target: &Target) -> bool {
    matches!(ty, MirTy::Struct(id, _) if matches!(target.repr(*id).c_kind, CKind::F16 | CKind::BF16))
}

/// A `Simd`, which C aligns to 16 bytes as its vectors.
#[must_use]
pub fn is_vector(ty: &MirTy, target: &Target) -> bool {
    vector(ty, target).is_some()
}

/// A `Simd`: the C vector it is (`V128`, or `V64` for 8 bytes).
fn vector(ty: &MirTy, target: &Target) -> Option<CType> {
    let MirTy::Struct(id, _) = ty else {
        return None;
    };
    (target.repr(*id).c_kind == CKind::Simd).then(|| match target.size_align(ty).0 {
        16 => CType::Scalar(CScalar::V128),
        _ => CType::Scalar(CScalar::V64),
    })
}

/// What the C ABI of `target` cannot pass among `params` and `ret`.
///
/// The error and what to write instead: `f16` / `bf16` where C has no
/// such type (on i686, as a result), a `Simd` that is no C vector there.
#[must_use]
pub fn c_refusal(target: &Target, params: &[MirTy], ret: &MirTy) -> Option<(String, String)> {
    let triple = target.name.as_str();
    for (i, t) in params.iter().chain([ret]).enumerate() {
        let MirTy::Struct(id, fields) = t else {
            continue;
        };
        let k = target.repr(*id).c_kind;
        let (name, c) = match k {
            CKind::F16 => ("f16", "_Float16"),
            CKind::BF16 => ("bf16", "__bf16"),
            CKind::Simd => ("Simd", ""),
            CKind::Plain | CKind::Mask => continue,
        };
        let arg = i < params.len();
        if k == CKind::Simd {
            let size = target.size_align(t).0;
            let lanes = match fields.first() {
                Some(MirTy::Array(_, n)) => *n,
                _ => 0,
            };
            let vectors = matches!(
                target.c_abi,
                CAbi::Aarch64 | CAbi::X86_64SysV | CAbi::X86_64Win
            );
            if !(vectors && matches!(size, 8 | 16) && lanes >= 2) {
                return Some((
                    format!("a `Simd` of {size} bytes cannot cross the C ABI on {triple}"),
                    "help: pass a pointer to its lanes: `ConstPtr<T>` to an array of them"
                        .to_owned(),
                ));
            }
            continue;
        }
        let has = match target.c_abi {
            CAbi::Aarch64
            | CAbi::X86_64SysV
            | CAbi::X86_64Win
            | CAbi::ArmHf
            | CAbi::Riscv64
            | CAbi::Riscv32 => true,
            // A half result comes back in `xmm0`, which nabi does not read.
            CAbi::X86SysV | CAbi::X86Win => arg,
            CAbi::S390x => k == CKind::F16,
            CAbi::Ppc64 | CAbi::Wasm32 | CAbi::Other => false,
        };
        if !has {
            let help = if arg {
                "help: pass its bits: declare the parameter `u16` and pass `x.to_bits()`".to_owned()
            } else {
                format!(
                    "help: take its bits: declare the result `u16` and use `{name}::from_bits(r)`"
                )
            };
            let what = if arg { "argument" } else { "result" };
            return Some((
                format!("C on {triple} takes no `{c}` {what} (`{name}`)"),
                help,
            ));
        }
    }
    None
}

/// The scalar an aggregate made of one scalar leaf (and no padding)
/// crosses the WebAssembly C ABI as.
fn single_scalar(ty: &MirTy, target: &Target) -> Option<CType> {
    let mut ls = Vec::new();
    leaves(ty, 0, target, &mut ls);
    let [(0, size, leaf)] = ls[..] else {
        return None;
    };
    (size == target.size_align(ty).0).then(|| CType::Scalar(leaf.scalar(size)))
}

/// Homogeneous float aggregate: `[n x f32|f64]` for 1..=4 members.
fn hfa(ty: &MirTy, target: &Target) -> Option<CType> {
    hfa_of(ty, target, 4)
}

/// Homogeneous float aggregate of 1..=`max` members of one float type,
/// a union's overlaid members counted once, without padding.
fn hfa_of(ty: &MirTy, target: &Target, max: usize) -> Option<CType> {
    let mut ls = Vec::new();
    leaves(ty, 0, target, &mut ls);
    let &(_, size, first) = ls.first()?;
    if first == Leaf::Int || ls.iter().any(|l| l.2 != first || l.1 != size) {
        return None;
    }
    ls.sort_unstable();
    ls.dedup();
    if ls.len() > max
        || ls.iter().enumerate().any(|(i, l)| l.0 != i as u32 * size)
        || ls.len() as u32 * size != target.size_align(ty).0
    {
        return None;
    }
    Some(CType::Array(
        u32::try_from(ls.len()).ok()?,
        first.scalar(size),
    ))
}

/// [`hfa_of`], halves excluded (32-bit ARM passes those as integers).
fn hfa_no_half(ty: &MirTy, target: &Target) -> Option<CType> {
    hfa(ty, target).filter(|h| !matches!(h, CType::Array(_, CScalar::F16)))
}

/// Classify a C signature on `target`; `params` are the fixed ones
/// (variadic extras are scalars). `None`: aggregates the target can't pass.
#[must_use]
pub fn classify(target: &Target, params: &[MirTy], ret: &MirTy, variadic: bool) -> Option<FnAbi> {
    // A `#[repr(transparent)]` struct passes as its field.
    let params: Vec<MirTy> = params.iter().map(|p| transparent(p, target)).collect();
    let params = params.as_slice();
    let ret = &transparent(ret, target);
    let (ret, args) = match target.c_abi {
        CAbi::Aarch64 => {
            // Windows: a C variadic callee takes its fixed HFAs in
            // integer registers too (vectors stay in `v` registers). A
            // 16-aligned aggregate is an `i128` (an even register pair).
            let windows = target.os == "windows";
            let ints = variadic && windows;
            // The alignment that makes a 16-byte aggregate an `i128`: its
            // own on Windows, its fields' on Linux (an `aligned` of its
            // own does not count), none on Apple.
            let pair_align = |p: &MirTy| match target.os.as_str() {
                "windows" => target.size_align(p).1,
                "macos" | "ios" => 0,
                _ => unadjusted_align(p, target),
            };
            let args = params
                .iter()
                .map(|p| {
                    let size = target.size_align(p).0;
                    match aapcs_arg(p, target) {
                        ArgAbi::Coerce(CType::Array(2, CScalar::Int(64)))
                            if size == 16 && pair_align(p) == 16 =>
                        {
                            ArgAbi::Coerce(CType::int(128))
                        }
                        ArgAbi::Coerce(CType::Array(..)) if ints && is_aggregate(p) => match size {
                            1..=8 => ArgAbi::Coerce(CType::int(64)),
                            9..=16 => ArgAbi::Coerce(CType::Array(2, CScalar::Int(64))),
                            _ => ArgAbi::Indirect { byval: false },
                        },
                        a => a,
                    }
                })
                .collect();
            (aapcs_ret(ret, target), args)
        }
        CAbi::X86_64Win => {
            // A half in an `xmm` register; a vector by reference, its
            // result in `xmm0`.
            let small = |t: &MirTy| match target.size_align(t).0 {
                _ if half(t, target) => Some(CType::Scalar(CScalar::F16)),
                s @ (1 | 2 | 4 | 8) => Some(CType::int(s * 8)),
                _ => None,
            };
            let ret = if let Some(v) = vector(ret, target) {
                RetAbi::Coerce(v)
            } else if is_aggregate(ret) {
                small(ret).map_or(RetAbi::Sret, RetAbi::Coerce)
            } else {
                RetAbi::Direct
            };
            let args = params
                .iter()
                .map(|p| {
                    if !is_aggregate(p) {
                        return ArgAbi::Direct;
                    }
                    if vector(p, target).is_some() {
                        return ArgAbi::Indirect { byval: false };
                    }
                    small(p).map_or(ArgAbi::Indirect { byval: false }, ArgAbi::Coerce)
                })
                .collect();
            (ret, args)
        }
        CAbi::X86_64SysV => sysv(params, ret, target),
        CAbi::X86SysV | CAbi::X86Win => x86(params, ret, target, variadic),
        CAbi::ArmHf => (
            aapcs32_ret(ret, target, !variadic),
            params.iter().map(|p| aapcs32_arg(p, target)).collect(),
        ),
        CAbi::Wasm32 => {
            let ret = if is_aggregate(ret) {
                single_scalar(ret, target).map_or(RetAbi::Sret, RetAbi::Coerce)
            } else {
                RetAbi::Direct
            };
            let args = params
                .iter()
                .map(|p| {
                    if !is_aggregate(p) {
                        return ArgAbi::Direct;
                    }
                    single_scalar(p, target)
                        .map_or(ArgAbi::Indirect { byval: false }, ArgAbi::Coerce)
                })
                .collect();
            (ret, args)
        }
        CAbi::Riscv64 | CAbi::Riscv32 => riscv(params, ret, target),
        CAbi::Ppc64 => (
            ppc64_ret(ret, target),
            params.iter().map(|p| ppc64_arg(p, target)).collect(),
        ),
        CAbi::S390x => (
            if half(ret, target) {
                RetAbi::Coerce(CType::Scalar(CScalar::F16))
            } else if is_aggregate(ret) && target.size_align(ret).0 > 0 {
                RetAbi::Sret
            } else {
                RetAbi::Direct
            },
            params.iter().map(|p| s390x_arg(p, target)).collect(),
        ),
        CAbi::Other => {
            if params.iter().chain([ret]).any(is_aggregate) {
                return None;
            }
            (RetAbi::Direct, vec![ArgAbi::Direct; params.len()])
        }
    };
    Some(FnAbi {
        ret,
        args,
        variadic,
        nabi: false,
        stdcall: false,
        errno: false,
        leaf: false,
    })
}

/// A runtime (`extern "nabi"`) declaration: the C ABI, with `str` a by-value
/// `(ptr, len)` C struct like `NabiFn`.
///
/// `None` means a plain call already matches C: `cc` splits a two-word pair
/// (`str`, a `fn` value) into words, which agrees with C while all integer
/// words fit in registers. Otherwise the signature is classified: a pair
/// that could straddle the last register and the stack, any bigger or float
/// aggregate, any pair on Windows x64 / WebAssembly (by reference), and a
/// by-value pair result on 32-bit targets (memory, or `edx:eax` on Windows
/// x86). RISC-V passes a pair as C's 16-byte integer struct both ways,
/// which `cc` matches. No runtime runs on other targets.
#[must_use]
pub fn classify_nabi(target: &Target, params: &[MirTy], ret: &MirTy) -> Option<FnAbi> {
    let by_value = |t: &MirTy| is_aggregate(t) || *t == MirTy::Prim(PrimTy::Str);
    let word = target.pointer_width / 8;
    let pair = |t: &MirTy| {
        let mut ls = Vec::new();
        leaves(t, 0, target, &mut ls);
        ls == [(0, word, Leaf::Int), (word, word, Leaf::Int)] && target.size_align(t).0 == 2 * word
    };
    let regs = match target.c_abi {
        CAbi::Aarch64 => 8,
        CAbi::X86_64SysV => 6,
        CAbi::X86_64Win | CAbi::Wasm32 | CAbi::S390x => 0,
        CAbi::X86SysV | CAbi::X86Win | CAbi::ArmHf if by_value(ret) => 0,
        CAbi::X86SysV
        | CAbi::X86Win
        | CAbi::ArmHf
        | CAbi::Riscv64
        | CAbi::Riscv32
        | CAbi::Ppc64 => u32::MAX,
        CAbi::Other => return None,
    };
    let words: u32 = params
        .iter()
        .filter(|t| !matches!(t, MirTy::Prim(PrimTy::F32 | PrimTy::F64)))
        .map(|t| target.size_align(t).0.div_ceil(word))
        .sum();
    let other = params
        .iter()
        .chain([ret])
        .any(|t| is_aggregate(t) && !pair(t));
    if !other {
        if regs == 0 {
            if !params.iter().chain([ret]).any(by_value) {
                return None;
            }
        } else if words <= regs {
            return None;
        }
    }
    let pair = |t: &MirTy| {
        if *t == MirTy::Prim(PrimTy::Str) {
            MirTy::Tuple(vec![MirTy::Prim(PrimTy::Usize); 2])
        } else {
            t.clone()
        }
    };
    let params: Vec<MirTy> = params.iter().map(pair).collect();
    let mut abi = classify(target, &params, &pair(ret), false)?;
    abi.nabi = true;
    Some(abi)
}

/// Aggregates larger than this go between nabi functions by address.
pub const BY_REF_BYTES: u32 = 64;

/// A nabi-to-nabi argument or result passed by address.
///
/// An aggregate over [`BY_REF_BYTES`]. The argument is a pointer the callee
/// copies from; the result goes through a pointer passed first and the
/// function returns nothing.
#[must_use]
pub fn nabi_by_ref(ty: &MirTy, target: &Target) -> bool {
    is_aggregate(ty) && target.size_align(ty).0 > BY_REF_BYTES
}

/// The type a value of `ty` crosses the C boundary as: the field of a
/// `#[repr(transparent)]` struct (recursively), else `ty`.
#[must_use]
pub fn transparent(ty: &MirTy, target: &Target) -> MirTy {
    match ty {
        MirTy::Struct(id, ps) if target.repr(*id).transparent && ps.len() == 1 => {
            transparent(&ps[0], target)
        }
        _ => ty.clone(),
    }
}

fn aapcs_arg(ty: &MirTy, target: &Target) -> ArgAbi {
    if !is_aggregate(ty) {
        return ArgAbi::Direct;
    }
    let size = target.size_align(ty).0;
    if size == 0 {
        return ArgAbi::Direct;
    }
    if let Some(h) = vector(ty, target).or_else(|| hfa(ty, target)) {
        return ArgAbi::Coerce(h);
    }
    match size {
        1..=8 => ArgAbi::Coerce(CType::int(64)),
        9..=16 => ArgAbi::Coerce(CType::Array(2, CScalar::Int(64))),
        _ => ArgAbi::Indirect { byval: false },
    }
}

fn aapcs_ret(ty: &MirTy, target: &Target) -> RetAbi {
    if !is_aggregate(ty) {
        return RetAbi::Direct;
    }
    let size = target.size_align(ty).0;
    if size == 0 {
        return RetAbi::Direct;
    }
    if let Some(h) = vector(ty, target).or_else(|| hfa(ty, target)) {
        return RetAbi::Coerce(h);
    }
    match size {
        1..=8 => RetAbi::Coerce(CType::int(size * 8)),
        9..=16 => RetAbi::Coerce(CType::Array(2, CScalar::Int(64))),
        _ => RetAbi::Sret,
    }
}

/// The alignment of a `byval` stack copy of `ty`.
#[must_use]
pub fn byval_align(ty: &MirTy, target: &Target) -> u32 {
    let a = target.size_align(ty).1;
    match target.c_abi {
        CAbi::X86SysV | CAbi::X86Win => 4,
        CAbi::ArmHf => unadjusted_align(ty, target).clamp(4, 8),
        _ => a.max(8),
    }
}

fn x86(params: &[MirTy], ret: &MirTy, target: &Target, variadic: bool) -> (RetAbi, Vec<ArgAbi>) {
    let size = target.size_align(ret).0;
    let ret = if !is_aggregate(ret) || size == 0 {
        RetAbi::Direct
    } else if target.c_abi == CAbi::X86Win && matches!(size, 1 | 2 | 4 | 8) {
        RetAbi::Coerce(CType::int(size * 8))
    } else {
        RetAbi::Sret
    };
    let args = params
        .iter()
        .map(|p| {
            // Windows: one with a `repr(align(N))` past 4 by reference.
            let win = target.c_abi == CAbi::X86Win && !variadic;
            if is_aggregate(p) && win && target.required_align(p) > 4 {
                ArgAbi::Indirect { byval: false }
            } else if is_aggregate(p) && target.size_align(p).0 > 0 {
                ArgAbi::Indirect { byval: true }
            } else {
                ArgAbi::Direct
            }
        })
        .collect();
    (ret, args)
}

/// The alignment of `ty` without a `#[repr(align(N))]` of its own (the
/// one 32-bit ARM C passes an aggregate by): its fields'.
fn unadjusted_align(ty: &MirTy, target: &Target) -> u32 {
    match ty {
        MirTy::Struct(id, ps)
            if target.repr(*id).align > 0 && target.union_members(*id).is_none() =>
        {
            if target.repr(*id).packed {
                1
            } else {
                ps.iter().map(|p| target.size_align(p).1).max().unwrap_or(1)
            }
        }
        _ => target.size_align(ty).1,
    }
}

fn aapcs32_arg(ty: &MirTy, target: &Target) -> ArgAbi {
    let size = target.size_align(ty).0;
    let align = unadjusted_align(ty, target);
    if !is_aggregate(ty) || size == 0 {
        return ArgAbi::Direct;
    }
    if half(ty, target) {
        return ArgAbi::Coerce(CType::Scalar(CScalar::F16));
    }
    if let Some(h) = hfa_no_half(ty, target) {
        return ArgAbi::Coerce(h);
    }
    if size > 64 {
        return ArgAbi::Indirect { byval: true };
    }
    let (n, elem) = if align <= 4 {
        (size.div_ceil(4), 32)
    } else {
        (size.div_ceil(8), 64)
    };
    ArgAbi::Coerce(CType::Array(n, CScalar::Int(elem)))
}

/// A 32-bit ARM result: HFAs in VFP registers only with `vfp` (a C
/// variadic call follows the base standard: memory past 4 bytes).
fn aapcs32_ret(ty: &MirTy, target: &Target, vfp: bool) -> RetAbi {
    let size = target.size_align(ty).0;
    if !is_aggregate(ty) || size == 0 {
        return RetAbi::Direct;
    }
    if half(ty, target) {
        return RetAbi::Coerce(CType::Scalar(CScalar::F16));
    }
    if let Some(h) = hfa_no_half(ty, target).filter(|_| vfp) {
        return RetAbi::Coerce(h);
    }
    match size {
        1 => RetAbi::Coerce(CType::int(8)),
        2 => RetAbi::Coerce(CType::int(16)),
        3 | 4 => RetAbi::Coerce(CType::int(32)),
        _ => RetAbi::Sret,
    }
}

fn ppc64_arg(ty: &MirTy, target: &Target) -> ArgAbi {
    let (size, align) = target.size_align(ty);
    if !is_aggregate(ty) || size == 0 {
        return ArgAbi::Direct;
    }
    if let Some(h) = hfa_of(ty, target, 8) {
        return ArgAbi::Coerce(h);
    }
    if size <= 8 {
        return ArgAbi::Coerce(CType::int(64));
    }
    // A 16-aligned one starts at an even doubleword: quadwords, as clang.
    if align >= 16 {
        return ArgAbi::Coerce(CType::Array(size.div_ceil(16), CScalar::Int(128)));
    }
    ArgAbi::Coerce(CType::Array(size.div_ceil(8), CScalar::Int(64)))
}

fn ppc64_ret(ty: &MirTy, target: &Target) -> RetAbi {
    let size = target.size_align(ty).0;
    if !is_aggregate(ty) || size == 0 {
        return RetAbi::Direct;
    }
    if let Some(h) = hfa_of(ty, target, 8) {
        return RetAbi::Coerce(h);
    }
    match size {
        1..=8 => RetAbi::Coerce(CType::int(64)),
        9..=16 => RetAbi::Coerce(CType::Array(2, CScalar::Int(64))),
        _ => RetAbi::Sret,
    }
}

/// s390x: an aggregate of 1, 2, 4 or 8 bytes as that integer (a lone
/// float as itself), others by reference.
fn s390x_arg(ty: &MirTy, target: &Target) -> ArgAbi {
    let size = target.size_align(ty).0;
    if !is_aggregate(ty) || size == 0 {
        return ArgAbi::Direct;
    }
    match s390x_float(ty, target) {
        Some(f) if target.size_align(&f).0 == size => {
            let s = match f {
                MirTy::Prim(PrimTy::F32) => CScalar::F32,
                MirTy::Prim(_) => CScalar::F64,
                _ => CScalar::F16,
            };
            ArgAbi::Coerce(CType::Scalar(s))
        }
        _ if matches!(size, 1 | 2 | 4 | 8) => ArgAbi::Coerce(CType::int(size * 8)),
        _ => ArgAbi::Indirect { byval: false },
    }
}

/// The float of a struct holding one, as s390x C finds it: through
/// nested structs of one member, never an array or a union.
fn s390x_float(ty: &MirTy, target: &Target) -> Option<MirTy> {
    match ty {
        MirTy::Prim(PrimTy::F32 | PrimTy::F64) => Some(ty.clone()),
        _ if half(ty, target) => Some(ty.clone()),
        MirTy::Struct(id, _) if target.union_members(*id).is_some() => None,
        MirTy::Struct(_, ps) | MirTy::Tuple(ps) if ps.len() == 1 => s390x_float(&ps[0], target),
        _ => None,
    }
}

fn riscv(params: &[MirTy], ret: &MirTy, target: &Target) -> (RetAbi, Vec<ArgAbi>) {
    // ILP32 is soft float: no `fa` registers.
    let fprs = if target.c_abi == CAbi::Riscv64 { 8 } else { 0 };
    let mut gprs = 8;
    let mut fprs_left = fprs;
    let ret = match riscv_arg(ret, target, &mut 2, &mut fprs.min(2)) {
        ArgAbi::Direct => RetAbi::Direct,
        ArgAbi::Coerce(c) => RetAbi::Coerce(c),
        ArgAbi::Indirect { .. } => {
            gprs -= 1;
            RetAbi::Sret
        }
    };
    let args = params
        .iter()
        .map(|p| riscv_arg(p, target, &mut gprs, &mut fprs_left))
        .collect();
    (ret, args)
}

/// One RISC-V argument (or, with two of each register, a result),
/// taking the registers it uses from `gprs` / `fprs` as clang counts them.
fn riscv_arg(ty: &MirTy, target: &Target, gprs: &mut u32, fprs: &mut u32) -> ArgAbi {
    let (size, align) = target.size_align(ty);
    let xlen = target.pointer_width / 8;
    if !is_aggregate(ty) {
        match ty {
            MirTy::Unit => {}
            MirTy::Prim(PrimTy::F32 | PrimTy::F64) if *fprs > 0 => *fprs -= 1,
            // C sees `str` as a `char*`.
            MirTy::Prim(PrimTy::Str) => *gprs = gprs.saturating_sub(1),
            _ => *gprs = gprs.saturating_sub(size.div_ceil(xlen)),
        }
        return ArgAbi::Direct;
    }
    if size == 0 {
        return ArgAbi::Direct;
    }
    if *fprs > 0 {
        if let Some((c, ints, floats)) = riscv_fp(ty, target) {
            if ints <= *gprs && floats <= *fprs {
                *gprs -= ints;
                *fprs -= floats;
                return ArgAbi::Coerce(c);
            }
        }
    }
    if size <= xlen {
        *gprs = gprs.saturating_sub(1);
        ArgAbi::Coerce(CType::int(xlen * 8))
    } else if size <= 2 * xlen {
        *gprs = gprs.saturating_sub(2);
        ArgAbi::Coerce(if align == 2 * xlen {
            CType::int(16 * xlen)
        } else {
            CType::Array(2, CScalar::Int(xlen * 8))
        })
    } else {
        *gprs = gprs.saturating_sub(1);
        ArgAbi::Indirect { byval: false }
    }
}

/// A struct the LP64D float convention passes as fields (one float, two
/// floats, or a float and an integer <= 8 bytes, flattened; never a
/// union): its C type and the integer and float registers it takes. The
/// fields keep their offsets, as clang's coerced type pads (or packs)
/// them.
fn riscv_fp(ty: &MirTy, target: &Target) -> Option<(CType, u32, u32)> {
    fn has_union(ty: &MirTy, target: &Target) -> bool {
        match ty {
            MirTy::Union(..) => true,
            MirTy::Struct(id, _) if target.union_members(*id).is_some() => true,
            MirTy::Tuple(ps) | MirTy::Struct(_, ps) => ps.iter().any(|p| has_union(p, target)),
            MirTy::Array(e, _) => has_union(e, target),
            _ => false,
        }
    }
    if has_union(ty, target) {
        return None;
    }
    let mut ls = Vec::new();
    leaves(ty, 0, target, &mut ls);
    let name = |&(_, size, leaf): &(u32, u32, Leaf)| leaf.scalar(size);
    let floats = u32::try_from(ls.iter().filter(|l| l.2 != Leaf::Int).count()).ok()?;
    match ls[..] {
        [one] if one.2 != Leaf::Int => {
            let c = if one.0 == 0 {
                CType::Scalar(name(&one))
            } else {
                CType::At(vec![(one.0, name(&one))])
            };
            Some((c, 0, 1))
        }
        [a, b] if floats > 0 && a.1 <= 8 && b.1 <= 8 => {
            let c = if a.0 == 0 && b.0 == (a.0 + a.1).next_multiple_of(b.1) {
                CType::Struct(vec![name(&a), name(&b)])
            } else {
                CType::At(vec![(a.0, name(&a)), (b.0, name(&b))])
            };
            Some((c, 2 - floats, floats))
        }
        _ => None,
    }
}

/// System V `x86_64` eightbyte classes of a <= 16-byte aggregate: the C
/// scalar of each eightbyte and whether it is SSE.
fn sysv_eightbytes(ty: &MirTy, target: &Target) -> Vec<(CScalar, bool)> {
    let size = target.size_align(ty).0;
    let mut ls = Vec::new();
    leaves(ty, 0, target, &mut ls);
    let mut out = Vec::new();
    for start in [0u32, 8] {
        if start >= size {
            break;
        }
        let here: Vec<_> = ls
            .iter()
            .filter(|(o, _, _)| *o >= start && *o < start + 8)
            .collect();
        if here.is_empty() {
            // A padding-only eightbyte (NO_CLASS): none at the end, as
            // clang passes only the low one (an over-aligned struct);
            // a leading one keeps the offsets.
            if start > 0 {
                break;
            }
            out.push((CScalar::Int(64), false));
            continue;
        }
        let sse = here.iter().all(|l| l.2 != Leaf::Int);
        let end = here.iter().map(|l| l.0 + l.1).max().unwrap_or(start);
        let t = if sse {
            // Data in the low four bytes only: one `f32` or halves.
            if end <= start + 4 {
                CScalar::F32
            } else if here.iter().all(|l| l.2 == Leaf::F32) {
                CScalar::F32x2
            } else {
                CScalar::F64
            }
        } else {
            // Up to the last data byte (tail padding excluded), like clang.
            CScalar::Int((end - start).min(8) * 8)
        };
        out.push((t, sse));
    }
    out
}

/// An unaligned field (`#[repr(packed)]`): System V MEMORY class.
fn unaligned(ty: &MirTy, target: &Target) -> bool {
    let mut ls = Vec::new();
    leaves(ty, 0, target, &mut ls);
    ls.iter().any(|(off, s, _)| off % s != 0)
}

fn sysv(params: &[MirTy], ret: &MirTy, target: &Target) -> (RetAbi, Vec<ArgAbi>) {
    let mut free_int = 6u32;
    let mut free_sse = 8u32;
    let ret_size = target.size_align(ret).0;
    let ret_abi = if !is_aggregate(ret) || ret_size == 0 {
        RetAbi::Direct
    } else if let Some(v) = vector(ret, target) {
        RetAbi::Coerce(v)
    } else if ret_size > 16 || unaligned(ret, target) {
        free_int -= 1;
        RetAbi::Sret
    } else {
        RetAbi::Coerce(join(&sysv_eightbytes(ret, target)))
    };
    let mut args = Vec::new();
    for t in params {
        if !is_aggregate(t) {
            match t {
                MirTy::Prim(PrimTy::F32 | PrimTy::F64) => free_sse = free_sse.saturating_sub(1),
                MirTy::Unit => {}
                _ => free_int = free_int.saturating_sub(1),
            }
            args.push(ArgAbi::Direct);
            continue;
        }
        let size = target.size_align(t).0;
        if size == 0 {
            args.push(ArgAbi::Direct);
            continue;
        }
        if let Some(v) = vector(t, target) {
            // One `xmm` register, else the stack.
            free_sse = free_sse.saturating_sub(1);
            args.push(ArgAbi::Coerce(v));
            continue;
        }
        if size > 16 {
            args.push(ArgAbi::Indirect { byval: true });
            continue;
        }
        if unaligned(t, target) {
            args.push(ArgAbi::Indirect { byval: true });
            continue;
        }
        let ebs = sysv_eightbytes(t, target);
        let need_sse = u32::try_from(ebs.iter().filter(|e| e.1).count()).unwrap_or(2);
        let need_int = u32::try_from(ebs.len()).unwrap_or(2) - need_sse;
        if need_int > free_int || need_sse > free_sse {
            // Not enough registers for all of it: the whole aggregate
            // goes on the stack.
            args.push(ArgAbi::Indirect { byval: true });
            continue;
        }
        free_int -= need_int;
        free_sse -= need_sse;
        args.push(ArgAbi::Coerce(join(&ebs)));
    }
    (ret_abi, args)
}

fn join(ebs: &[(CScalar, bool)]) -> CType {
    match ebs {
        [(one, _)] => CType::Scalar(*one),
        _ => CType::Struct(ebs.iter().map(|e| e.0).collect()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use CScalar::Int;
    use PrimTy::{F32, F64, I8, I32, I64};
    use nabic_hir::{Repr, StructId};

    fn st(fields: &[PrimTy]) -> MirTy {
        MirTy::Tuple(fields.iter().map(|p| MirTy::Prim(*p)).collect())
    }

    fn on(triple: &str) -> Target {
        Target::builtin(triple)
    }

    /// `t` with struct `id` `#[repr(align(n))]`.
    fn aligned(mut t: Target, id: u32, n: u32) -> Target {
        let r = Repr {
            align: n,
            ..Repr::default()
        };
        t.reprs = std::sync::Arc::new(std::iter::once((id, r)).collect());
        t
    }

    const fn arr(n: u32, elem: CScalar) -> CType {
        CType::Array(n, elem)
    }

    #[test]
    fn aapcs64() {
        let t = on("aarch64-unknown-linux-gnu");
        let a = |ty: &MirTy| aapcs_arg(ty, &t);
        let r = |ty: &MirTy| aapcs_ret(ty, &t);
        // div_t: one x register, both ways.
        assert_eq!(r(&st(&[I32, I32])), RetAbi::Coerce(CType::int(64)));
        assert_eq!(a(&st(&[I32, I32])), ArgAbi::Coerce(CType::int(64)));
        assert_eq!(r(&st(&[I8, I8, I8])), RetAbi::Coerce(CType::int(24)));
        assert_eq!(r(&st(&[I64, I32])), RetAbi::Coerce(arr(2, Int(64))));
        // HFA / HFA-like.
        assert_eq!(
            a(&st(&[F32, F32, F32])),
            ArgAbi::Coerce(arr(3, CScalar::F32))
        );
        assert_eq!(
            r(&st(&[F64, F64, F64, F64])),
            RetAbi::Coerce(arr(4, CScalar::F64))
        );
        // Mixed float kinds are not an HFA.
        assert_eq!(a(&st(&[F32, F64])), ArgAbi::Coerce(arr(2, Int(64))));
        // 5 floats: too many for an HFA, 20 bytes -> memory.
        assert_eq!(a(&st(&[F32; 5])), ArgAbi::Indirect { byval: false });
        assert_eq!(r(&st(&[I64, I64, I64])), RetAbi::Sret);
        assert_eq!(a(&MirTy::Prim(I32)), ArgAbi::Direct);
    }

    #[test]
    fn sysv_x86_64() {
        let t = on("x86_64-unknown-linux-gnu");
        let f = |ps: &[MirTy], r: &MirTy| sysv(ps, r, &t);
        assert_eq!(f(&[], &st(&[I32, I32])).0, RetAbi::Coerce(CType::int(64)));
        assert_eq!(
            f(&[], &st(&[I64, F64])).0,
            RetAbi::Coerce(CType::Struct(vec![Int(64), CScalar::F64]))
        );
        assert_eq!(
            f(&[], &st(&[F32, F32, F32])).0,
            RetAbi::Coerce(CType::Struct(vec![CScalar::F32x2, CScalar::F32]))
        );
        assert_eq!(
            f(&[], &st(&[I64, I32])).0,
            RetAbi::Coerce(CType::Struct(vec![Int(64), Int(32)]))
        );
        assert_eq!(f(&[], &st(&[F32, I32])).0, RetAbi::Coerce(CType::int(64)));
        assert_eq!(f(&[], &st(&[I64, I64, I64])).0, RetAbi::Sret);
        assert_eq!(
            f(&[st(&[I64, I64, I64])], &MirTy::Unit).1,
            vec![ArgAbi::Indirect { byval: true }]
        );
        // Five ints use five GPRs; a two-INTEGER struct then does not fit
        // and goes to memory whole, while a one-eightbyte one still fits.
        let ints = vec![MirTy::Prim(I64); 5];
        let mut ps = ints.clone();
        ps.push(st(&[I64, I64]));
        assert_eq!(f(&ps, &MirTy::Unit).1[5], ArgAbi::Indirect { byval: true });
        let mut ps = ints;
        ps.push(st(&[I32, I32]));
        assert_eq!(f(&ps, &MirTy::Unit).1[5], ArgAbi::Coerce(CType::int(64)));
        // sret takes rdi.
        let mut ps = vec![MirTy::Prim(I64); 4];
        ps.push(st(&[I64, I64]));
        assert_eq!(
            f(&ps, &st(&[I64, I64, I64])).1[4],
            ArgAbi::Indirect { byval: true }
        );
    }

    /// Windows arm64: a 16-aligned aggregate of 16 bytes is an `i128`.
    #[test]
    fn win_arm64_aligned_pair() {
        let t = aligned(on("aarch64-pc-windows-msvc"), 6, 16);
        let a = MirTy::Struct(StructId(6), vec![MirTy::Prim(I64), MirTy::Prim(I64)]);
        let c = classify(&t, &[MirTy::Prim(I32), a.clone()], &MirTy::Unit, false).unwrap();
        assert_eq!(c.args[1], ArgAbi::Coerce(CType::int(128)));
        let lin = aligned(on("aarch64-unknown-linux-gnu"), 6, 16);
        let c = classify(&lin, &[a], &MirTy::Unit, false).unwrap();
        assert_eq!(c.args[0], ArgAbi::Coerce(arr(2, Int(64))));
    }

    /// A Windows arm64 variadic callee takes fixed HFAs in integer
    /// registers, by reference past 16 bytes.
    #[test]
    fn win_arm64_variadic_hfa() {
        let t = on("aarch64-pc-windows-msvc");
        let a = classify(&t, &[st(&[F32; 3]), st(&[F64; 4])], &st(&[F32; 3]), true).unwrap();
        assert_eq!(
            a.args,
            [
                ArgAbi::Coerce(arr(2, Int(64))),
                ArgAbi::Indirect { byval: false }
            ]
        );
        assert_eq!(a.ret, RetAbi::Coerce(arr(3, CScalar::F32)));
        let fixed = classify(&t, &[st(&[F32; 3])], &MirTy::Unit, false).unwrap();
        assert_eq!(fixed.args[0], ArgAbi::Coerce(arr(3, CScalar::F32)));
    }

    #[test]
    fn win64_and_targets() {
        let w = classify(
            &on("x86_64-pc-windows-msvc"),
            &[st(&[I32, I32]), st(&[I32, I32, I32])],
            &st(&[I8; 3]),
            false,
        )
        .unwrap();
        assert_eq!(w.args[0], ArgAbi::Coerce(CType::int(64)));
        assert_eq!(w.args[1], ArgAbi::Indirect { byval: false });
        assert_eq!(w.ret, RetAbi::Sret);
        // A target without a C ABI for aggregates refuses them.
        let mut spec = nabic_mir::TargetSpec::builtin("riscv32imac-unknown-none-elf").unwrap();
        spec.c_abi = CAbi::Other;
        let other = Target::new(spec);
        assert!(classify(&other, &[st(&[I32, I32])], &MirTy::Unit, false).is_none());
        let scalars = classify(&other, &[MirTy::Prim(I32)], &MirTy::Unit, true).unwrap();
        assert!(scalars.variadic);
    }

    /// RISC-V ILP32 (soft float): aggregates of up to two words in `a`
    /// registers, float ones too; bigger ones by reference / `sret`.
    #[test]
    fn riscv_ilp32() {
        let t = on("riscv32imac-unknown-none-elf");
        let c = |ps: &[MirTy], r: &MirTy| classify(&t, ps, r, false).unwrap();
        let co = ArgAbi::Coerce;
        let a = c(
            &[
                st(&[I8, I8]),
                st(&[I32, I32]),
                st(&[F32, F32]),
                st(&[I64]),
                st(&[F64]),
                st(&[I32, I32, I32]),
            ],
            &st(&[F32, I32]),
        );
        assert_eq!(a.ret, RetAbi::Coerce(arr(2, Int(32))));
        assert_eq!(
            a.args,
            [
                co(CType::int(32)),
                co(arr(2, Int(32))),
                co(arr(2, Int(32))),
                co(CType::int(64)),
                co(CType::int(64)),
                ArgAbi::Indirect { byval: false },
            ]
        );
        assert_eq!(c(&[], &st(&[I32; 3])).ret, RetAbi::Sret);
        assert_eq!(c(&[], &st(&[F32])).ret, RetAbi::Coerce(CType::int(32)));
    }

    /// i386: aggregates on the stack, results in memory but for Windows's
    /// register-sized ones (`div_t` in `edx:eax`, a 3-byte one not).
    #[test]
    fn x86_cdecl() {
        let lin = on("i686-unknown-linux-gnu");
        let win = on("i686-pc-windows-msvc");
        let ps = [st(&[I32, I32]), MirTy::Prim(I32)];
        for t in [&lin, &win] {
            let a = classify(t, &ps, &MirTy::Unit, false).unwrap();
            assert_eq!(a.args, [ArgAbi::Indirect { byval: true }, ArgAbi::Direct]);
            assert_eq!(byval_align(&st(&[I64, F64]), t), 4);
        }
        let r = |t: &Target, ty: &MirTy| classify(t, &[], ty, false).unwrap().ret;
        assert_eq!(r(&lin, &st(&[I32, I32])), RetAbi::Sret);
        assert_eq!(r(&win, &st(&[I32, I32])), RetAbi::Coerce(CType::int(64)));
        assert_eq!(r(&win, &st(&[F32])), RetAbi::Coerce(CType::int(32)));
        assert_eq!(r(&win, &st(&[I8; 3])), RetAbi::Sret);
        assert_eq!(r(&lin, &MirTy::Prim(I64)), RetAbi::Direct);
    }

    #[test]
    fn aapcs_vfp() {
        let arm = on("armv7-unknown-linux-gnueabihf");
        let a = |ty: &MirTy| aapcs32_arg(ty, &arm);
        let r = |ty: &MirTy| aapcs32_ret(ty, &arm, true);
        assert_eq!(
            a(&st(&[F32, F32, F32])),
            ArgAbi::Coerce(arr(3, CScalar::F32))
        );
        assert_eq!(r(&st(&[F64, F64])), RetAbi::Coerce(arr(2, CScalar::F64)));
        assert_eq!(a(&st(&[I32, I8])), ArgAbi::Coerce(arr(2, Int(32))));
        // 8-aligned: whole even register pairs.
        assert_eq!(a(&st(&[I64, I32])), ArgAbi::Coerce(arr(2, Int(64))));
        assert_eq!(a(&st(&[I32; 17])), ArgAbi::Indirect { byval: true });
        // `repr(align(16))` of its own does not count, its fields do.
        let t16 = aligned(on("armv7-unknown-linux-gnueabihf"), 4, 16);
        let f4 = MirTy::Struct(StructId(4), vec![MirTy::Prim(F32), MirTy::Prim(I32)]);
        assert_eq!(aapcs32_arg(&f4, &t16), ArgAbi::Coerce(arr(4, Int(32))));
        assert_eq!(byval_align(&st(&[I64; 9]), &arm), 8);
        assert_eq!(r(&st(&[I8, I8, I8])), RetAbi::Coerce(CType::int(32)));
        assert_eq!(r(&st(&[I32, I32])), RetAbi::Sret);
        // A variadic callee returns an HFA in memory (the base standard).
        let v = classify(&arm, &[MirTy::Prim(I32)], &st(&[F64, F64]), true).unwrap();
        assert_eq!(v.ret, RetAbi::Sret);
        let s = MirTy::Prim(PrimTy::Str);
        let nabi = classify_nabi(&arm, &[s.clone()], &s).unwrap();
        assert_eq!(nabi.args[0], ArgAbi::Coerce(arr(2, Int(32))));
        assert_eq!(nabi.ret, RetAbi::Sret);
        // Arguments alone split as C lays them out.
        assert!(classify_nabi(&arm, &[s.clone(), s], &MirTy::Unit).is_none());
    }

    /// RISC-V LP64D: float structs in `fa` / `a` registers while both
    /// are left, integer ones as `i64` / `[2 x i64]`, bigger ones by
    /// reference; `sret` takes `a0`.
    #[test]
    fn riscv_lp64d() {
        let t = on("riscv64gc-unknown-linux-gnu");
        let c = |ps: &[MirTy], r: &MirTy| classify(&t, ps, r, false).unwrap();
        let co = ArgAbi::Coerce;
        let a = c(
            &[
                st(&[F32, F64]),
                st(&[I32, F32]),
                st(&[F64]),
                st(&[F32, F32, F32]),
                st(&[I64, I64, I64]),
            ],
            &st(&[F64, I64]),
        );
        assert_eq!(
            a.ret,
            RetAbi::Coerce(CType::Struct(vec![CScalar::F64, Int(64)]))
        );
        assert_eq!(
            a.args,
            [
                co(CType::Struct(vec![CScalar::F32, CScalar::F64])),
                co(CType::Struct(vec![Int(32), CScalar::F32])),
                co(CType::Scalar(CScalar::F64)),
                co(arr(2, Int(64))),
                ArgAbi::Indirect { byval: false },
            ]
        );
        // Two integers, or a union, are not a float struct.
        assert_eq!(
            c(&[st(&[I32, I32])], &MirTy::Unit).args[0],
            co(CType::int(64))
        );
        assert_eq!(
            c(
                &[MirTy::Union(Vec::new(), 1, nabic_mir::UnionKind::Plain)],
                &MirTy::Unit
            )
            .args[0],
            co(CType::int(64))
        );
        // Eight floats take every `fa`: the struct goes in `a` registers.
        let mut ps = vec![MirTy::Prim(F64); 8];
        ps.push(st(&[F32, F32]));
        assert_eq!(c(&ps, &MirTy::Unit).args[8], co(CType::int(64)));
        // An `sret` result takes `a0`: seven integers then leave one
        // register, too few for the float struct's integer half.
        let mut ps = vec![MirTy::Prim(I64); 7];
        ps.push(st(&[F32, I32]));
        assert_eq!(
            c(&ps, &MirTy::Unit).args[7],
            co(CType::Struct(vec![CScalar::F32, Int(32)]))
        );
        assert_eq!(c(&ps, &st(&[I64; 3])).args[7], co(CType::int(64)));
        let s = MirTy::Prim(PrimTy::Str);
        assert!(classify_nabi(&t, &[s.clone(), s.clone()], &s).is_none());
    }

    /// LP64D float structs whose fields are padded or packed keep their
    /// offsets (clang's `{[n x i8], A, [m x i8], B}`); nested structs and
    /// float arrays flatten; a wide integer or a third field does not
    /// qualify.
    #[test]
    fn riscv_lp64d_offsets() {
        let mut t = on("riscv64gc-unknown-linux-gnu");
        let (packed, aligned) = (StructId(0), StructId(1));
        let reprs = [
            (
                0,
                Repr {
                    packed: true,
                    ..Repr::default()
                },
            ),
            (
                1,
                Repr {
                    align: 16,
                    ..Repr::default()
                },
            ),
        ];
        t.reprs = std::sync::Arc::new(reprs.into_iter().collect());
        let c = |ps: &[MirTy], r: &MirTy| classify(&t, ps, r, false).unwrap();
        let co = ArgAbi::Coerce;
        let pf = MirTy::Struct(packed, vec![MirTy::Prim(I8), MirTy::Prim(F32)]);
        let pd = MirTy::Struct(packed, vec![MirTy::Prim(F32), MirTy::Prim(F64)]);
        // { f32, { f64 aligned to 16 } }: the f64 at 16.
        let inner = MirTy::Struct(aligned, vec![MirTy::Prim(F64)]);
        let padded = MirTy::Tuple(vec![MirTy::Prim(F32), inner]);
        let nested = MirTy::Tuple(vec![
            st(&[F32]),
            MirTy::Array(Box::new(MirTy::Prim(F32)), 1),
        ]);
        let a = c(
            &[
                pf,
                pd,
                padded,
                nested,
                MirTy::Array(Box::new(MirTy::Prim(F64)), 2),
                st(&[F32, F32, F32]),
            ],
            &MirTy::Unit,
        );
        assert_eq!(
            a.args,
            [
                co(CType::At(vec![(0, Int(8)), (1, CScalar::F32)])),
                co(CType::At(vec![(0, CScalar::F32), (4, CScalar::F64)])),
                co(CType::At(vec![(0, CScalar::F32), (16, CScalar::F64)])),
                co(CType::Struct(vec![CScalar::F32, CScalar::F32])),
                co(arr(2, Int(64))),
                co(arr(2, Int(64))),
            ]
        );
        // 16-aligned and 16 bytes: `i128`.
        let wide = MirTy::Struct(aligned, vec![MirTy::Prim(I64)]);
        assert_eq!(c(&[wide], &MirTy::Unit).args[0], co(CType::int(128)));
    }

    /// powerpc64 `ELFv2`: HFAs of up to eight floats, other aggregates as
    /// doublewords; results of 16 bytes in registers. s390x: aggregates
    /// of an integer's size as one, a lone float as itself, others (and
    /// a runtime `str`) by reference; every aggregate result through
    /// `sret`.
    #[test]
    fn ppc64_and_s390x() {
        let co = ArgAbi::Coerce;
        let t = on("powerpc64le-unknown-linux-gnu");
        let a = classify(
            &t,
            &[st(&[F32; 8]), st(&[I32, F32]), st(&[I8; 20])],
            &st(&[I64, I64]),
            false,
        )
        .unwrap();
        assert_eq!(
            a.args,
            [
                co(arr(8, CScalar::F32)),
                co(CType::int(64)),
                co(arr(3, Int(64)))
            ]
        );
        assert_eq!(a.ret, RetAbi::Coerce(arr(2, Int(64))));
        assert_eq!(
            classify(&t, &[], &st(&[I64; 3]), false).unwrap().ret,
            RetAbi::Sret
        );
        let t16 = aligned(on("powerpc64le-unknown-linux-gnu"), 3, 16);
        let a16 = MirTy::Struct(StructId(3), vec![MirTy::Prim(I64), MirTy::Prim(I64)]);
        assert_eq!(ppc64_arg(&a16, &t16), co(arr(1, Int(128))));
        let s = MirTy::Prim(PrimTy::Str);
        assert!(classify_nabi(&t, &[s.clone(), s.clone()], &s).is_none());

        let t = on("s390x-unknown-linux-gnu");
        let a = classify(
            &t,
            &[st(&[F32]), st(&[I32, F32]), st(&[I8; 3]), st(&[F64])],
            &st(&[I32]),
            false,
        )
        .unwrap();
        assert_eq!(
            a.args,
            [
                co(CType::Scalar(CScalar::F32)),
                co(CType::int(64)),
                ArgAbi::Indirect { byval: false },
                co(CType::Scalar(CScalar::F64))
            ]
        );
        assert_eq!(a.ret, RetAbi::Sret);
        let r = classify_nabi(&t, &[MirTy::Prim(I32), s.clone()], &s).unwrap();
        assert_eq!(r.args[1], ArgAbi::Indirect { byval: false });
        assert_eq!(r.ret, RetAbi::Sret);
    }

    /// A runtime `str` on Windows x64 is a 16-byte C struct: by
    /// reference, returned through `sret`; scalars alone stay plain.
    #[test]
    fn win64_runtime_strs_go_by_reference() {
        let t = on("x86_64-pc-windows-msvc");
        let s = MirTy::Prim(PrimTy::Str);
        let r = classify_nabi(&t, &[MirTy::Prim(I32), s.clone()], &s).unwrap();
        assert_eq!(r.args[1], ArgAbi::Indirect { byval: false });
        assert_eq!(r.ret, RetAbi::Sret);
        assert!(r.nabi);
        assert!(classify_nabi(&t, &[MirTy::Prim(I64)], &MirTy::Prim(I32)).is_none());
        let arm = on("aarch64-pc-windows-msvc");
        assert!(classify_nabi(&arm, &[MirTy::Prim(I32), s.clone()], &s).is_none());
    }

    /// A runtime aggregate other than a two-word pair follows C wherever
    /// the registers stand: 24 bytes go by reference on AAPCS64,
    /// `byval` on System V; a `fn` value pair splits like a `str`.
    #[test]
    fn runtime_aggregates_follow_c() {
        let big = MirTy::Tuple(vec![MirTy::Prim(I64); 3]);
        let a = classify_nabi(&on("aarch64-apple-darwin"), &[big.clone()], &big).unwrap();
        assert_eq!(a.args[0], ArgAbi::Indirect { byval: false });
        assert_eq!(a.ret, RetAbi::Sret);
        let x = on("x86_64-unknown-linux-gnu");
        let r = classify_nabi(&x, &[big], &MirTy::Unit).unwrap();
        assert_eq!(r.args[0], ArgAbi::Indirect { byval: true });
        let mixed = MirTy::Tuple(vec![MirTy::Prim(I32), MirTy::Prim(PrimTy::F32)]);
        let arm = on("armv7-unknown-linux-gnueabihf");
        let r = classify_nabi(&arm, &[mixed], &MirTy::Unit).unwrap();
        assert_eq!(r.args[0], ArgAbi::Coerce(arr(2, Int(32))));
        let f = MirTy::Tuple(vec![MirTy::GcRef, MirTy::GcRef]);
        assert!(classify_nabi(&x, &[f], &MirTy::Unit).is_none());
    }

    /// A target with unions 10 `{f32, f32}`, 11 `{f64, f32}`, 12
    /// `{i32, f32}`, 13 `{f32}`, `f16` 20, `bf16` 21 and `Simd`s 30
    /// (`f32 x 4`), 31 (`f32 x 2`), 32 (`f32 x 3`).
    fn kinds(triple: &str) -> Target {
        let mut t = on(triple);
        let p = MirTy::Prim;
        t.unions = std::sync::Arc::new(
            [
                (10, vec![p(F32), p(F32)]),
                (11, vec![p(F64), p(F32)]),
                (12, vec![p(I32), p(F32)]),
                (13, vec![p(F32)]),
            ]
            .into_iter()
            .collect(),
        );
        let k = |c_kind| Repr {
            c_kind,
            ..Repr::default()
        };
        t.reprs = std::sync::Arc::new(
            [
                (20, k(CKind::F16)),
                (21, k(CKind::BF16)),
                (30, k(CKind::Simd)),
                (31, k(CKind::Simd)),
                (32, k(CKind::Simd)),
            ]
            .into_iter()
            .collect(),
        );
        t
    }

    fn union(id: u32, storage: PrimTy) -> MirTy {
        MirTy::Struct(StructId(id), vec![MirTy::Prim(storage)])
    }

    fn half() -> MirTy {
        MirTy::Struct(StructId(20), vec![MirTy::Prim(PrimTy::U16)])
    }

    fn simd(id: u32, n: u32) -> MirTy {
        MirTy::Struct(
            StructId(id),
            vec![MirTy::Array(Box::new(MirTy::Prim(F32)), n)],
        )
    }

    /// An over-aligned struct whose high eightbyte is padding takes one
    /// register, as clang passes it.
    #[test]
    fn sysv_padding_eightbyte() {
        let t = aligned(on("x86_64-unknown-linux-gnu"), 5, 16);
        let a = MirTy::Struct(StructId(5), vec![MirTy::Prim(I32)]);
        let f = MirTy::Struct(StructId(5), vec![MirTy::Prim(F32)]);
        let (r, args) = sysv(&[a.clone(), f], &a, &t);
        assert_eq!(r, RetAbi::Coerce(CType::int(32)));
        assert_eq!(
            args,
            [
                ArgAbi::Coerce(CType::int(32)),
                ArgAbi::Coerce(CType::Scalar(CScalar::F32))
            ]
        );
    }

    /// Unions by their members, not their storage: an HFA of floats, SSE
    /// for a double and a float, an integer for an int and a float; an
    /// integer on s390x and RISC-V; a scalar on wasm32 only alone.
    #[test]
    fn unions_by_members() {
        let co = ArgAbi::Coerce;
        let (ff, df, iff, one) = (
            union(10, F32),
            union(11, F64),
            union(12, PrimTy::U32),
            union(13, F32),
        );
        let t = kinds("aarch64-apple-darwin");
        assert_eq!(aapcs_arg(&ff, &t), co(arr(1, CScalar::F32)));
        assert_eq!(aapcs_arg(&df, &t), co(CType::int(64)));
        assert_eq!(aapcs_ret(&iff, &t), RetAbi::Coerce(CType::int(32)));
        let t = kinds("x86_64-unknown-linux-gnu");
        let r = |u: &MirTy| sysv(&[], u, &t).0;
        assert_eq!(r(&df), RetAbi::Coerce(CType::Scalar(CScalar::F64)));
        assert_eq!(r(&ff), RetAbi::Coerce(CType::Scalar(CScalar::F32)));
        assert_eq!(r(&iff), RetAbi::Coerce(CType::int(32)));
        let t = kinds("armv7-unknown-linux-gnueabihf");
        assert_eq!(aapcs32_arg(&ff, &t), co(arr(1, CScalar::F32)));
        let t = kinds("s390x-unknown-linux-gnu");
        assert_eq!(s390x_arg(&ff, &t), co(CType::int(32)));
        let t = kinds("riscv64gc-unknown-linux-gnu");
        assert_eq!(
            classify(&t, &[ff.clone()], &MirTy::Unit, false)
                .unwrap()
                .args[0],
            co(CType::int(64))
        );
        let t = kinds("wasm32-unknown-unknown");
        let a = classify(&t, &[ff, one], &MirTy::Unit, false).unwrap();
        assert_eq!(
            a.args,
            [
                ArgAbi::Indirect { byval: false },
                co(CType::Scalar(CScalar::F32))
            ]
        );
    }

    /// `f16` in a float register's low half (as `f32` bits on x86-64),
    /// halves as float members (an HFA on AAPCS64, a float struct on
    /// riscv64, integers on armv7); refused where C has no `_Float16`.
    #[test]
    fn halves() {
        let co = ArgAbi::Coerce;
        let h = half();
        let h2 = MirTy::Tuple(vec![half(), half()]);
        let t = kinds("aarch64-unknown-linux-gnu");
        assert_eq!(aapcs_arg(&h, &t), co(arr(1, CScalar::F16)));
        assert_eq!(aapcs_ret(&h2, &t), RetAbi::Coerce(arr(2, CScalar::F16)));
        let t = kinds("x86_64-unknown-linux-gnu");
        assert_eq!(
            sysv(&[h.clone()], &h2, &t),
            (
                RetAbi::Coerce(CType::Scalar(CScalar::F32)),
                vec![co(CType::Scalar(CScalar::F32))]
            )
        );
        let t = kinds("x86_64-pc-windows-msvc");
        let a = classify(&t, &[h.clone(), h2.clone()], &h, false).unwrap();
        assert_eq!(
            a.args,
            [co(CType::Scalar(CScalar::F16)), co(CType::int(32))]
        );
        assert_eq!(a.ret, RetAbi::Coerce(CType::Scalar(CScalar::F16)));
        let t = kinds("armv7-unknown-linux-gnueabihf");
        assert_eq!(aapcs32_arg(&h, &t), co(CType::Scalar(CScalar::F16)));
        assert_eq!(aapcs32_arg(&h2, &t), co(arr(1, Int(32))));
        let t = kinds("riscv64gc-unknown-linux-gnu");
        let a = classify(&t, &[h.clone(), h2], &MirTy::Unit, false).unwrap();
        assert_eq!(
            a.args,
            [
                co(CType::Scalar(CScalar::F16)),
                co(CType::Struct(vec![CScalar::F16, CScalar::F16]))
            ]
        );
        let t = kinds("s390x-unknown-linux-gnu");
        assert_eq!(
            classify(&t, &[], &h, false).unwrap().ret,
            RetAbi::Coerce(CType::Scalar(CScalar::F16))
        );
        let bf = MirTy::Struct(StructId(21), vec![MirTy::Prim(PrimTy::U16)]);
        assert!(c_refusal(&t, &[bf], &MirTy::Unit).is_some());
        for triple in ["powerpc64le-unknown-linux-gnu", "wasm32-unknown-unknown"] {
            assert!(c_refusal(&kinds(triple), &[h.clone()], &MirTy::Unit).is_some());
        }
        let t = kinds("i686-unknown-linux-gnu");
        assert!(c_refusal(&t, &[h.clone()], &MirTy::Unit).is_none());
        let (why, help) = c_refusal(&t, &[], &h).unwrap();
        assert!(
            why.contains("`_Float16` result") && help.contains("f16::from_bits"),
            "{why} {help}"
        );
    }

    /// `Simd` of 16 / 8 bytes: one `v` / `xmm` register, by reference as
    /// a Windows x64 argument; refused elsewhere and at other sizes.
    #[test]
    fn vectors() {
        let co = ArgAbi::Coerce;
        let (v4, v2) = (simd(30, 4), simd(31, 2));
        let v128 = CType::Scalar(CScalar::V128);
        let t = kinds("aarch64-pc-windows-msvc");
        assert_eq!(aapcs_arg(&v4, &t), co(v128.clone()));
        assert_eq!(
            aapcs_ret(&v2, &t),
            RetAbi::Coerce(CType::Scalar(CScalar::V64))
        );
        let t = kinds("x86_64-unknown-linux-gnu");
        assert_eq!(
            sysv(&[v4.clone()], &v4, &t),
            (RetAbi::Coerce(v128.clone()), vec![co(v128.clone())])
        );
        let t = kinds("x86_64-pc-windows-msvc");
        let a = classify(&t, &[v4.clone()], &v4, false).unwrap();
        assert_eq!(a.args, [ArgAbi::Indirect { byval: false }]);
        assert_eq!(a.ret, RetAbi::Coerce(v128));
        assert!(c_refusal(&t, &[simd(32, 3)], &MirTy::Unit).is_some());
        assert!(c_refusal(&kinds("armv7-unknown-linux-gnueabihf"), &[v4], &MirTy::Unit).is_some());
    }
}
